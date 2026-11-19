use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Position, Rect};
use wisp_protocol::settings::{SettingChange, SettingValue, names};
use wisp_protocol::{AgentEvent, ClientCommand, TermSize};
use wisp_render::chrome::sidebar::{RowOf, Step, rail};
use wisp_render::chrome::{KeyHint, WIDE_FROM, up_down};
use wisp_render::markdown::Layout;
use wisp_render::motion::{FRAME_MIN, PULSE_STEP};
use wisp_render::picture::{self, Scene};
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::theme::{Rgb, Token};
use wisp_render::transcript::copy;

use super::Presenter;
use crate::app::selection::Click;
use crate::app::sidebar::{
    AGENTS_SECTION, Frame, NO_SUCH_SECTION, NOTHING_NOW, NOTHING_YET, OPEN_ARG, RULE,
    SECTIONS_UNKNOWN, band, no_section_named, none_on, overlay_band, overlay_whole, section_off,
    sidebar_usage, transcript_width,
};
use crate::term::picture::Tile;
use crate::term::{Key, KeyCode, Mouse, MouseKind, SidebarSetting};

const SIDEBAR_ENV: &str = "WISP_SIDEBAR=";
const FLAG_LEAD: &str = "--set ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PanelArea {
    pub(super) rule: u16,
    pub(super) rows: u16,
    pub(super) covered_from: u16,
    pub(super) ground: Rgb,
}

impl PanelArea {
    fn shown(&self) -> u16 {
        self.covered_from.min(self.rows)
    }
}

pub(super) type Shown = (Tile, Rc<Scene>, picture::Rect);

pub(super) fn shown_tiles(
    scene: &Rc<Scene>,
    cell: (u16, u16),
    (left, top): (usize, usize),
    first_row: usize,
) -> impl Iterator<Item = Shown> {
    picture::tiles(scene, cell)
        .into_iter()
        .filter_map(move |(rect, hash)| {
            let x = u16::try_from(left + rect.x).ok()?;
            let y = u16::try_from(top + rect.y - first_row).ok()?;
            let cells = u16::try_from(rect.cells).ok()?;
            let tile = Tile {
                at: Position { x, y },
                cells,
                hash,
            };
            Some((tile, Rc::clone(scene), rect))
        })
}

impl Presenter {
    pub(super) fn sections_on(&self) -> Vec<String> {
        let saved = self.kept.as_ref().map(|k| &k.saved);
        self.sidebar
            .on(&|path| saved.and_then(|s| s.section(path)).map(|c| c.value))
    }

    pub(super) fn panel_docked(&self) -> bool {
        self.sidebar.docked()
    }

    pub(super) fn transcript_width(&self) -> usize {
        transcript_width(self.sidebar.width(), self.sidebar.docked())
    }

    pub(super) fn sidebar_event(&mut self, event: &AgentEvent) {
        if matches!(event, AgentEvent::QuestionAsked { .. }) && self.sidebar.close_overlay() {
            self.frames.mark();
        }
        if self.sidebar.event(event) {
            self.frames.mark();
        }
        self.fold_refused(event);
        self.sidebar_changed();
    }

    pub(super) fn sidebar_changed(&mut self) {
        let on = self.sections_on();
        if self.open_page().is_some_and(|name| !on.contains(&name)) {
            self.close_pager();
        }
        if self.resize_at.is_none() && self.sidebar.wants_dock(&on) != self.sidebar.docked() {
            self.settle_resize();
            return;
        }
        self.sync_slots(&on);
    }

    pub(super) fn open_page(&self) -> Option<String> {
        self.pager
            .as_ref()
            .and_then(|p| p.section_name())
            .map(str::to_owned)
    }

    pub(super) fn sync_terminal(&mut self) {
        if self.resize_at.is_some() {
            return;
        }
        let size = TermSize {
            cols: u16::try_from(self.layout().wide()).unwrap_or(u16::MAX),
            rows: self.size.height,
        };
        if self.terminal_sent == Some(size) {
            return;
        }
        let sent = self
            .commands
            .as_ref()
            .is_some_and(|c| c.try_send(ClientCommand::Terminal { size }).is_ok());
        if sent {
            self.terminal_sent = Some(size);
        }
    }

    pub(super) fn sync_slots(&mut self, on: &[String]) {
        self.sync_terminal();
        let agents = (self.agents_open() && self.sidebar.offers(AGENTS_SECTION))
            .then(|| AGENTS_SECTION.to_owned());
        let page = self.open_page().or(agents);
        let Some(shown) = self.sidebar.unsent(on, page.as_deref()) else {
            return;
        };
        let sent = self.commands.as_ref().is_some_and(|c| {
            c.try_send(ClientCommand::SlotsShown {
                sections: shown.sections.clone(),
                pages: shown.pages.clone(),
            })
            .is_ok()
        });
        if sent {
            self.sidebar.sent(shown);
        }
    }

    pub(super) fn sidebar_command(&mut self, words: &str, at: Duration) {
        if words.contains(char::is_whitespace) {
            self.hint(&sidebar_usage(), at);
            return;
        }
        if self.sections_on().is_empty() && !self.sidebar.known() {
            self.hint(SECTIONS_UNKNOWN, at);
            return;
        }
        let open = match words {
            "" => false,
            OPEN_ARG => true,
            name => {
                self.sidebar_page(name, at);
                return;
            }
        };
        if self.sections_on().is_empty() {
            let text = none_on(Glyphs::of(self.glyphs).separator);
            self.hint(&text, at);
            return;
        }
        let width = usize::from(self.size.width);
        if open || width < WIDE_FROM {
            self.sidebar.open_overlay(width);
        } else if let Some(source) = self.sidebar_forced() {
            self.sidebar.open_overlay(width);
            self.hint(&format!("{source} sets it"), at);
        } else if self.sidebar.docked() {
            self.save_sidebar(SidebarSetting::Hidden, at);
        } else {
            self.sidebar.ask();
            self.save_sidebar(SidebarSetting::Auto, at);
        }
        self.sidebar_changed();
        self.frames.mark();
    }

    fn sidebar_page(&mut self, name: &str, at: Duration) {
        if self.sections_on().iter().any(|on| on == name) {
            self.open_section(name.to_owned());
            return;
        }
        let text = if self.sidebar.is_section(name) {
            section_off(name, Glyphs::of(self.glyphs).separator)
        } else if wisp_protocol::settings::section_ref(name) {
            no_section_named(name)
        } else {
            NO_SUCH_SECTION.to_owned()
        };
        self.hint(&text, at);
    }

    fn sidebar_forced(&self) -> Option<String> {
        let source = self.kept.as_ref()?.resolve().sidebar.source;
        [SIDEBAR_ENV, FLAG_LEAD]
            .iter()
            .any(|lead| source.starts_with(lead))
            .then_some(source)
    }

    fn save_sidebar(&mut self, setting: SidebarSetting, at: Duration) {
        self.sidebar.set_setting(setting);
        let Some(change) = SettingChange::named(
            names::DISPLAY_SIDEBAR,
            SettingValue::Text(setting.word().to_owned()),
        ) else {
            return;
        };
        let request = self.next_request();
        self.command(
            ClientCommand::ChangeSettings {
                request,
                changes: vec![change],
                save: true,
            },
            at,
        );
    }

    pub(super) fn panel_press(&mut self, mouse: Mouse, at: Duration, held: bool) {
        let showing = self.selection_painted();
        self.clear_selection();
        self.select.swallow();
        self.sidebar.cancel_press();
        let on_rule = self.panel_area.is_none_or(|area| mouse.column <= area.rule);
        let click = Click::next(self.sidebar_click, at, mouse.column, mouse.row);
        self.sidebar_click = Some(click);
        if held || showing || on_rule || click.count > 1 || self.select.refocused(at) {
            return;
        }
        self.sidebar
            .press_row(usize::from(mouse.row), (mouse.column, mouse.row));
    }

    pub(super) fn panel_release(&mut self, mouse: Mouse, at: Duration) {
        let section = match self.sidebar.released((mouse.column, mouse.row)) {
            Some(RowOf::Title(section)) => section,
            Some(RowOf::Body(section)) => {
                self.open_section(section);
                return;
            }
            None => return,
        };
        let list = self.sidebar.fold(&section);
        self.frames.mark();
        let Some(change) =
            SettingChange::named(names::DISPLAY_SIDEBAR_FOLDED, SettingValue::List(list))
        else {
            return;
        };
        let request = self.next_request();
        let sent = self.command(
            ClientCommand::ChangeSettings {
                request,
                changes: vec![change],
                save: true,
            },
            at,
        );
        if sent {
            self.sidebar.fold_sent(request);
            self.tracker.asked_quietly(request);
        }
    }

    pub(super) fn fold_refused(&mut self, event: &AgentEvent) {
        let AgentEvent::Rejected {
            request: Some(request),
            ..
        } = event
        else {
            return;
        };
        let effective = self
            .kept
            .as_ref()
            .map(|kept| kept.resolve().sidebar_folded.value)
            .unwrap_or_default();
        if self.sidebar.fold_refused(*request, effective) == Some(true) {
            self.frames.mark();
        }
    }

    pub(super) fn overlay_holds(&self) -> bool {
        self.sidebar.overlay().is_some() && self.pager.is_none() && self.settings_screen.is_none()
    }

    pub(super) fn overlay_keys(&self) -> Vec<KeyHint> {
        let glyphs = Glyphs::of(self.glyphs);
        vec![
            KeyHint::new(up_down(&glyphs), "scroll"),
            KeyHint::new(copy::ESC_KEY, copy::CLOSE_WORD),
        ]
    }

    pub(super) fn overlay_key(&mut self, key: Key) {
        let step = match key.code {
            KeyCode::Char('t' | 'T') if key.ctrl => {
                self.close_overlay();
                self.open_agents();
                return;
            }
            KeyCode::Char('c' | 'C') if key.ctrl => {
                self.close_overlay();
                return;
            }
            KeyCode::Esc | KeyCode::Char('q') if !key.ctrl && !key.alt => {
                self.close_overlay();
                return;
            }
            KeyCode::Up => Step::Up(1),
            KeyCode::Down => Step::Down(1),
            KeyCode::PageUp => Step::PageUp,
            KeyCode::PageDown => Step::PageDown,
            KeyCode::Home => Step::Top,
            KeyCode::End => Step::End,
            _ => return,
        };
        if self.sidebar.scroll(step) {
            self.frames.mark();
        }
    }

    pub(super) fn cards_take_the_keys(&mut self) {
        let cards = [self.step_in_shown(), self.retry.is_open()];
        if self.sidebar.card_appeared(cards) {
            self.close_overlay();
        }
    }

    pub(super) fn close_overlay(&mut self) {
        if self.sidebar.close_overlay() {
            self.sidebar_changed();
            self.frames.mark();
        }
    }

    pub(super) fn main_edge(&self) -> Option<u16> {
        if self.pager.is_some() {
            return None;
        }
        self.panel_area.map(|area| area.rule)
    }

    pub(super) fn panel_wheel(&mut self, mouse: Mouse, rows: usize) {
        let step = match mouse.kind {
            MouseKind::WheelUp => Step::Up(rows),
            _ => Step::Down(rows),
        };
        if self.sidebar.scroll(step) {
            self.frames.mark();
        }
    }

    pub(super) fn draw_sidebar(
        &mut self,
        buffer: &mut Buffer,
        rows: u16,
        at: Duration,
    ) -> Option<Rect> {
        self.panel_area = None;
        let overlay = self.sidebar.overlay().is_some();
        if !self.sidebar.showing() || rows == 0 {
            self.sidebar.forget_drawn();
            return None;
        }
        let (rule, band, whole) = if self.sidebar.docked() {
            let settled = self.sidebar.width();
            (transcript_width(settled, true), band(settled), false)
        } else {
            let width = usize::from(self.size.width);
            let band = overlay_band(width);
            (
                width.saturating_sub(band + RULE),
                band,
                overlay_whole(width),
            )
        };
        let on = self.sections_on();
        let frame = Frame {
            height: usize::from(rows),
            at,
            now_ms: self.now_ms(),
            cx: self.cx(),
            layout: Layout::new(band + RULE, self.glyphs),
            whole,
            pictures: self.pictures.map(|(_, cell)| cell.0),
        };
        let mut lines = self.sidebar.draw(&on, frame).lines.clone();
        let below = StyledLine::new(vec![rail(self.glyphs, whole && !lines.is_empty())]);
        if lines.is_empty() {
            let text = if self.sidebar.content_seen() {
                NOTHING_NOW
            } else {
                NOTHING_YET
            };
            lines.push(StyledLine::new(vec![
                rail(self.glyphs, false),
                StyledSpan::new(format!(" {text}"), Token::TextMuted),
            ]));
        }
        let (Ok(x), Ok(span)) = (u16::try_from(rule), u16::try_from(band + RULE)) else {
            return None;
        };
        let area = Rect::new(x, 0, span, rows);
        let theme = self.theme;
        for y in 0..rows {
            for column in x..x.saturating_add(span) {
                if let Some(cell) = buffer.cell_mut((column, y)) {
                    cell.reset();
                }
            }
            let line = lines.get(usize::from(y)).unwrap_or(&below);
            buffer.set_line(x, y, &theme.line(line), span);
        }
        let ground = if overlay {
            buffer.set_style(area, theme.bg(Token::Surface));
            theme.painted(Token::Surface.into())
        } else {
            theme.rgb(Token::Bg)
        };
        self.panel_area = Some(PanelArea {
            rule: x,
            rows,
            covered_from: rows,
            ground,
        });
        Some(area)
    }

    pub(super) fn panel_pictures(&self) -> Vec<Shown> {
        let (Some((_, cell)), Some(area)) = (self.pictures, self.panel_area) else {
            return Vec::new();
        };
        let strips = self.sidebar.drawn().map_or(&[][..], |p| &p.strips[..]);
        let shown = strips.get(..usize::from(area.shown())).unwrap_or(strips);
        let mut out = Vec::new();
        let mut y = 0;
        while let Some(rest) = shown.get(y..).filter(|r| !r.is_empty()) {
            let Some(strip) = &rest[0] else {
                y += 1;
                continue;
            };
            let follows = |(i, s): (usize, &Option<picture::Strip>)| {
                s.as_ref()
                    .is_some_and(|s| Arc::ptr_eq(&s.scene, &strip.scene) && s.row == strip.row + i)
            };
            let run = rest.iter().enumerate().take_while(|&r| follows(r)).count();
            let scene = Rc::new(Scene {
                rows: strip.row..strip.row + run,
                ..Scene::clone(&strip.scene)
            });
            out.extend(shown_tiles(
                &scene,
                cell,
                (usize::from(area.rule) + strip.x, y),
                strip.row,
            ));
            y += run;
        }
        out
    }

    pub(super) fn panel_covered(&mut self, from: u16) {
        if let Some(area) = &mut self.panel_area {
            area.covered_from = area.covered_from.min(from);
        }
    }

    pub(super) fn now_ms(&self) -> u64 {
        u64::try_from((self.now)().as_millisecond()).unwrap_or(0)
    }

    pub(super) fn sidebar_due(&self, at: Duration) -> Option<Duration> {
        self.panel_area?;
        let ticks = self.sidebar.ticks();
        let pulse = (ticks.pulse && self.cx().pulses()).then(|| at.saturating_add(PULSE_STEP));
        let clock = ticks.clock.then(|| {
            let into = self.now_ms() % 1000;
            at.saturating_add(Duration::from_millis(1000 - into))
        });
        let flight = ticks.flight.then(|| at.saturating_add(FRAME_MIN));
        [pulse, clock, flight].into_iter().flatten().min()
    }
}
