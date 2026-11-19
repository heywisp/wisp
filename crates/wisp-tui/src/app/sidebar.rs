use std::collections::BTreeMap;
use std::time::Duration;

use wisp_protocol::slot::{
    NodeState, SlotBlock, SlotContent, SlotEntry, SlotPlace, SlotSpan, SlotView,
};
use wisp_protocol::{AgentEvent, PluginEntry, RequestId};
use wisp_render::chrome::WIDE_FROM;
use wisp_render::chrome::sidebar::{
    PageView, Panel, PanelView, Position, RowOf, SectionPage, Sidebar, Step,
};
use wisp_render::markdown::{GUTTER, Layout};
use wisp_render::motion::MotionCtx;

use crate::app::selection::within_drift;
use crate::term::SidebarSetting;

pub const AGENTS_SECTION: &str = "agent-viz/topology";
pub const BAND_MIN: usize = 38;
pub const BAND_MAX: usize = 48;
pub const RULE: usize = 1;
pub const NOTHING_YET: &str = "Nothing to show yet";
pub const NOTHING_NOW: &str = "Nothing to show now";
pub const OPEN_ARG: &str = "open";
pub const NO_SUCH_SECTION: &str = "No sidebar section by that name";
pub const SECTIONS_UNKNOWN: &str = "The sidebar's sections aren't known yet";
const DRAWN_ROWS: usize = 10_000;

pub fn sidebar_usage() -> String {
    format!("/sidebar takes {OPEN_ARG}, a section, or nothing")
}

pub fn none_on(separator: &str) -> String {
    format!("No sidebar sections are on {separator} /config")
}

pub fn section_off(name: &str, separator: &str) -> String {
    format!("{name} is off {separator} /config")
}

pub fn no_section_named(name: &str) -> String {
    format!("No sidebar section named {name}")
}

pub fn band(width: usize) -> usize {
    (width / 4).clamp(BAND_MIN, BAND_MAX)
}

pub fn transcript_width(width: usize, docked: bool) -> usize {
    if docked {
        width.saturating_sub(band(width) + RULE)
    } else {
        width
    }
}

pub fn overlay_whole(width: usize) -> bool {
    width.saturating_sub(BAND_MIN + RULE) <= GUTTER
}

pub fn overlay_band(width: usize) -> usize {
    if overlay_whole(width) {
        width.saturating_sub(RULE)
    } else {
        band(width)
    }
}

pub fn switch_path(section: &str) -> Option<String> {
    let (plugin, name) = section.split_once('/')?;
    Some(format!(
        "{}.{plugin}.{}.{name}",
        wisp_protocol::settings::PLUGINS_GROUP,
        wisp_protocol::settings::SLOTS_TABLE
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overlay {
    pub narrow: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ticks {
    pub pulse: bool,
    pub clock: bool,
    pub flight: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub height: usize,
    pub at: Duration,
    pub now_ms: u64,
    pub cx: MotionCtx,
    pub layout: Layout,
    pub whole: bool,
    pub pictures: bool,
}

#[derive(Debug, Default)]
pub struct Side {
    model: Sidebar,
    plugins: Vec<PluginEntry>,
    setting: SidebarSetting,
    content_seen: bool,
    asked: bool,
    docked: bool,
    width: usize,
    overlay: Option<Overlay>,
    position: Position,
    sent: Shown,
    drawn: Option<Panel>,
    cards: [bool; 2],
    known: bool,
    folded: Vec<String>,
    pinned: Vec<String>,
    folds_waiting: Vec<RequestId>,
    press: Option<RowPress>,
    revisions: BTreeMap<String, u64>,
    catalogs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    pub sections: Vec<String>,
    pub pages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowPress {
    row: RowOf,
    at: (u16, u16),
}

impl Side {
    pub fn new(setting: SidebarSetting, width: usize) -> Side {
        Side {
            setting,
            width,
            ..Side::default()
        }
    }

    pub fn event(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::CatalogChanged { catalog } => {
                self.plugins.clone_from(&catalog.plugins);
                self.known = true;
            }
            AgentEvent::SettingsSnapshot(snapshot) => {
                self.plugins.clone_from(&snapshot.plugins);
                self.known = true;
            }
            _ => {}
        }
        let changed = self.model.event(event);
        if changed {
            match event {
                AgentEvent::SlotContent { section, .. }
                | AgentEvent::SlotFailed { section, .. } => {
                    *self.revisions.entry(section.clone()).or_default() += 1;
                }
                _ => self.catalogs += 1,
            }
        }
        if let AgentEvent::SlotContent {
            section,
            view: SlotView::Panel,
            ..
        } = event
            && changed
            && !self.content_seen
        {
            self.content_seen = self.draws(section);
        }
        changed
    }

    fn draws(&self, section: &str) -> bool {
        let on = [section.to_owned()];
        let position = Position::Top;
        let view = PanelView {
            on: &on,
            folded: &[],
            pinned: &[],
            position: &position,
            height: DRAWN_ROWS,
            at: Duration::ZERO,
            now_ms: 0,
            cx: MotionCtx::new(
                wisp_render::motion::MotionPolicy::Reduced,
                wisp_render::theme::glyphs::GlyphSet::Unicode,
                wisp_render::theme::Depth::None,
            ),
            layout: Layout::new(BAND_MAX, wisp_render::theme::glyphs::GlyphSet::Unicode),
            whole: false,
            pictures: false,
        };
        let panel = self.model.draw(&view);
        panel.rows.iter().any(|r| matches!(r, Some(RowOf::Body(_))))
    }

    pub fn on(&self, switch: &dyn Fn(&str) -> Option<bool>) -> Vec<String> {
        self.model
            .sections()
            .iter()
            .filter(|section| self.is_on(&section.name, switch))
            .map(|section| section.name.clone())
            .collect()
    }

    fn offered(&self, name: &str) -> Option<&SlotEntry> {
        let (plugin, _) = name.split_once('/')?;
        let entry = self
            .plugins
            .iter()
            .find(|p| p.id == plugin && p.installed && p.enabled)?;
        entry
            .slots
            .iter()
            .find(|s| s.name == name && s.slot == SlotPlace::Sidebar && s.disabled.is_none())
    }

    pub fn offers(&self, name: &str) -> bool {
        self.offered(name).is_some()
    }

    fn is_on(&self, name: &str, switch: &dyn Fn(&str) -> Option<bool>) -> bool {
        let Some(slot) = self.offered(name) else {
            return false;
        };
        switch_path(name)
            .and_then(|path| switch(&path))
            .unwrap_or(slot.default_on)
    }

    pub fn known(&self) -> bool {
        self.known
    }

    pub fn setting(&self) -> SidebarSetting {
        self.setting
    }

    pub fn set_setting(&mut self, setting: SidebarSetting) {
        self.setting = setting;
    }

    pub fn ask(&mut self) {
        self.asked = true;
    }

    pub fn content_seen(&self) -> bool {
        self.content_seen
    }

    pub fn docked(&self) -> bool {
        self.docked
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn holds(&self, on: &[String]) -> bool {
        self.setting == SidebarSetting::Auto && self.width >= WIDE_FROM && !on.is_empty()
    }

    pub fn wants_dock(&self, on: &[String]) -> bool {
        self.holds(on) && (self.content_seen || self.asked)
    }

    pub fn settle(&mut self, width: usize, on: &[String]) {
        self.width = width;
        if width >= WIDE_FROM && self.overlay.is_some_and(|o| o.narrow) {
            self.overlay = None;
        }
        self.docked = self.wants_dock(on);
    }

    pub fn overlay(&self) -> Option<Overlay> {
        self.overlay
    }

    pub fn open_overlay(&mut self, width: usize) {
        self.overlay = Some(Overlay {
            narrow: width < WIDE_FROM,
        });
    }

    pub fn close_overlay(&mut self) -> bool {
        self.overlay.take().is_some()
    }

    pub fn card_appeared(&mut self, cards: [bool; 2]) -> bool {
        let appeared = cards.iter().zip(self.cards).any(|(now, was)| *now && !was);
        self.cards = cards;
        appeared
    }

    pub fn showing(&self) -> bool {
        self.docked || self.overlay.is_some()
    }

    pub fn shown(&self, on: &[String]) -> Vec<String> {
        if self.holds(on) || self.overlay.is_some() {
            on.to_vec()
        } else {
            Vec::new()
        }
    }

    pub fn unsent(&self, on: &[String], page: Option<&str>) -> Option<Shown> {
        let shown = Shown {
            sections: self.shown(on),
            pages: page.map(str::to_owned).into_iter().collect(),
        };
        (shown != self.sent).then_some(shown)
    }

    pub fn sent(&mut self, shown: Shown) {
        self.sent = shown;
    }

    pub fn is_section(&self, name: &str) -> bool {
        self.model.sections().iter().any(|s| s.name == name)
    }

    pub fn titled(&self, on: &[String]) -> Vec<(String, String)> {
        self.model
            .sections()
            .iter()
            .filter(|s| on.contains(&s.name))
            .map(|s| (s.name.clone(), s.title.clone()))
            .collect()
    }

    pub fn revision(&self, section: &str) -> u64 {
        self.catalogs + self.revisions.get(section).copied().unwrap_or(0)
    }

    pub fn page(&self, section: &str, now_ms: u64, layout: Layout) -> Option<SectionPage> {
        self.model.draw_page(&PageView {
            section,
            now_ms,
            layout,
        })
    }

    pub fn forget_page(&mut self, section: &str) {
        if self.model.forget_page(section) {
            *self.revisions.entry(section.to_owned()).or_default() += 1;
        }
    }

    pub fn page_content(&self, section: &str) -> Option<&SlotContent> {
        self.model.page_content(section)
    }

    pub fn page_clock(&self, section: &str) -> bool {
        self.model
            .page_content(section)
            .is_some_and(|c| content_ticks(c).clock)
    }

    pub fn position(&self) -> &Position {
        &self.position
    }

    pub fn set_lists(&mut self, folded: Vec<String>, pinned: Vec<String>) {
        self.folded = folded;
        self.pinned = pinned;
    }

    pub fn set_pinned(&mut self, pinned: Vec<String>) -> bool {
        let changed = pinned != self.pinned;
        self.pinned = pinned;
        changed
    }

    pub fn fold(&mut self, section: &str) -> Vec<String> {
        match self.folded.iter().position(|name| name == section) {
            Some(at) => {
                self.folded.remove(at);
            }
            None => self.folded.push(section.to_owned()),
        }
        self.folded.sort();
        self.folded.dedup();
        self.folded.clone()
    }

    pub fn fold_sent(&mut self, request: RequestId) {
        self.folds_waiting.push(request);
    }

    pub fn press_row(&mut self, row: usize, at: (u16, u16)) {
        self.press = self.row_at(row).map(|row| RowPress { row, at });
    }

    pub fn moved_to(&mut self, at: (u16, u16)) {
        if self.press.as_ref().is_some_and(|p| !within_drift(p.at, at)) {
            self.press = None;
        }
    }

    pub fn cancel_press(&mut self) {
        self.press = None;
    }

    pub fn released(&mut self, at: (u16, u16)) -> Option<RowOf> {
        self.press
            .take()
            .filter(|p| within_drift(p.at, at))
            .map(|p| p.row)
    }

    pub fn folds_changed(&mut self, answers: Option<RequestId>, effective: Vec<String>) -> bool {
        if let Some(at) = answers.and_then(|r| self.folds_waiting.iter().position(|w| *w == r)) {
            self.folds_waiting.remove(at);
            return false;
        }
        let changed = effective != self.folded;
        self.folded = effective;
        changed
    }

    pub fn fold_refused(&mut self, request: RequestId, effective: Vec<String>) -> Option<bool> {
        let at = self.folds_waiting.iter().position(|w| *w == request)?;
        self.folds_waiting.remove(at);
        if !self.folds_waiting.is_empty() {
            return Some(false);
        }
        let changed = effective != self.folded;
        self.folded = effective;
        Some(changed)
    }

    pub fn row_at(&self, row: usize) -> Option<RowOf> {
        self.drawn.as_ref()?.rows.get(row)?.clone()
    }

    pub fn draw(&mut self, on: &[String], frame: Frame) -> &Panel {
        let view = PanelView {
            on,
            folded: &self.folded,
            pinned: &self.pinned,
            position: &self.position,
            height: frame.height,
            at: frame.at,
            now_ms: frame.now_ms,
            cx: frame.cx,
            layout: frame.layout,
            whole: frame.whole,
            pictures: frame.pictures,
        };
        let panel = self.model.draw(&view);
        self.position = panel.position.clone();
        self.drawn.insert(panel)
    }

    pub fn forget_drawn(&mut self) {
        self.drawn = None;
    }

    pub fn drawn(&self) -> Option<&Panel> {
        self.drawn.as_ref()
    }

    pub fn scroll(&mut self, step: Step) -> bool {
        let Some(panel) = &self.drawn else {
            return false;
        };
        let next = panel.scroll(&self.position, step);
        if next == self.position {
            return false;
        }
        self.position = next;
        true
    }

    pub fn ticks(&self) -> Ticks {
        let Some(panel) = &self.drawn else {
            return Ticks::default();
        };
        let mut shown: Vec<&str> = panel
            .rows
            .iter()
            .flatten()
            .map(|row| match row {
                RowOf::Title(name) | RowOf::Body(name) => name.as_str(),
            })
            .collect();
        shown.dedup();
        let ticks = Ticks {
            flight: panel.moving,
            ..Ticks::default()
        };
        shown
            .into_iter()
            .filter_map(|name| self.model.panel(name)?.content.as_ref())
            .fold(ticks, |ticks, content| {
                let own = content_ticks(content);
                Ticks {
                    pulse: ticks.pulse || own.pulse,
                    clock: ticks.clock || own.clock,
                    ..ticks
                }
            })
    }
}

fn clocked(spans: &[SlotSpan]) -> bool {
    spans
        .iter()
        .any(|span| matches!(span, SlotSpan::Elapsed { .. }))
}

fn content_ticks(content: &SlotContent) -> Ticks {
    let mut ticks = Ticks {
        clock: clocked(&content.note),
        ..Ticks::default()
    };
    for block in &content.blocks {
        match block {
            SlotBlock::Line { spans } => ticks.clock |= clocked(spans),
            SlotBlock::Rows { rows } => ticks.clock |= rows.iter().any(|r| clocked(&r.value)),
            SlotBlock::Gauge { note, .. } | SlotBlock::Spark { note, .. } => {
                ticks.clock |= clocked(note);
            }
            SlotBlock::List { items } => ticks.clock |= items.iter().any(|i| clocked(i)),
            SlotBlock::Tree { nodes } => {
                ticks.pulse |= nodes.iter().any(|n| n.state == NodeState::Working);
                ticks.clock |= nodes.iter().any(|n| clocked(&n.facts));
            }
            SlotBlock::Topology { nodes, .. } => {
                ticks.pulse |= nodes.iter().any(|n| n.state == NodeState::Working);
            }
            SlotBlock::NextColumn | SlotBlock::Fill { .. } | SlotBlock::Unknown => {}
        }
    }
    ticks
}

#[cfg(test)]
mod tests;
