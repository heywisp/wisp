mod blocks;
mod ingest;
mod page;

use std::collections::BTreeMap;
use std::time::Duration;

use wisp_protocol::AgentEvent;
use wisp_protocol::plugin::PluginEntry;
use wisp_protocol::slot::{
    NodeState, SlotBlock, SlotContent, SlotFailure, SlotNoun, SlotPlace, SlotView,
};

pub(super) use blocks::rails;
pub(crate) use blocks::token;
pub use page::{PageView, SectionPage};

use super::agents::count_ladder;
use super::composer::{edge_line, frame_inner, frame_row};
use super::menu::more_row;
use super::row::Rows;
use super::summary::{down_arrow, up_arrow};
use super::welcome::frame_for;
use crate::markdown::Layout;
use crate::motion::MotionCtx;
use crate::picture::Strip;
use crate::styled::{StyledLine, StyledSpan};
use crate::text::width;
use crate::theme::Token;
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::transcript::copy;

const PINNED_FLOOR: usize = 3;
const MORE_ROWS_FROM: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionEntry {
    pub name: String,
    pub title: String,
    pub order: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub failure: SlotFailure,
    pub parked: bool,
    pub restarted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    pub content: Option<SlotContent>,
    pub failure: Option<Failure>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Views {
    panel: Kept,
    page: Kept,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sidebar {
    entries: Vec<SectionEntry>,
    views: BTreeMap<String, Views>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Position {
    #[default]
    Top,
    At {
        section: String,
        order: u32,
        row: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Up(usize),
    Down(usize),
    PageUp,
    PageDown,
    Top,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowOf {
    Title(String),
    Body(String),
}

#[derive(Clone, Copy, Debug)]
pub struct PanelView<'a> {
    pub on: &'a [String],
    pub folded: &'a [String],
    pub pinned: &'a [String],
    pub position: &'a Position,
    pub height: usize,
    pub at: Duration,
    pub now_ms: u64,
    pub cx: MotionCtx,
    pub layout: Layout,
    pub whole: bool,
    pub pictures: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panel {
    pub lines: Vec<StyledLine>,
    pub rows: Vec<Option<RowOf>>,
    pub strips: Vec<Option<Strip>>,
    pub position: Position,
    pub pinned_rows: usize,
    pub moving: bool,
    scrolling: Scrolling,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Part {
    name: String,
    order: u32,
    start: usize,
    lines: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Scrolling {
    parts: Vec<Part>,
    height: usize,
    shared: bool,
}

#[derive(Clone)]
enum Row {
    Edge,
    Inside(StyledLine, Option<RowOf>, Option<Strip>),
    Outside(StyledLine),
}

struct Drawn {
    name: String,
    order: u32,
    rows: Vec<Row>,
    moving: bool,
}

impl Drawn {
    fn asked(&self) -> usize {
        self.rows.len()
    }
}

impl Sidebar {
    pub fn event(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::CatalogChanged { catalog } => self.catalog(&catalog.plugins),
            AgentEvent::SettingsSnapshot(snapshot) => self.catalog(&snapshot.plugins),
            AgentEvent::SlotContent {
                section,
                view,
                content,
            } => {
                let Some(kept) = self.kept_mut(section, *view) else {
                    return false;
                };
                kept.content = Some(ingest::content(content));
                kept.failure = None;
                true
            }
            AgentEvent::SlotFailed {
                section,
                view,
                failure,
                parked,
                restarted,
            } => {
                let Some(kept) = self.kept_mut(section, *view) else {
                    return false;
                };
                kept.failure = Some(Failure {
                    failure: ingest::failure(failure),
                    parked: *parked,
                    restarted: *restarted,
                });
                true
            }
            _ => false,
        }
    }

    pub fn catalog(&mut self, plugins: &[PluginEntry]) -> bool {
        let mut entries: Vec<SectionEntry> = plugins
            .iter()
            .flat_map(|p| &p.slots)
            .filter(|e| e.slot == SlotPlace::Sidebar)
            .map(|e| SectionEntry {
                name: ingest::text(&e.name),
                title: ingest::text(&e.title),
                order: e.order,
            })
            .collect();
        entries.sort_by(|a, b| (a.order, &a.name).cmp(&(b.order, &b.name)));
        let changed = entries != self.entries;
        self.entries = entries;
        changed
    }

    fn kept_mut(&mut self, section: &str, view: SlotView) -> Option<&mut Kept> {
        if view == SlotView::Unknown {
            return None;
        }
        let views = self.views.entry(ingest::text(section)).or_default();
        Some(match view {
            SlotView::Page => &mut views.page,
            SlotView::Panel | SlotView::Unknown => &mut views.panel,
        })
    }

    pub fn forget_page(&mut self, section: &str) -> bool {
        self.views
            .get_mut(section)
            .is_some_and(|v| std::mem::take(&mut v.page) != Kept::default())
    }

    pub fn sections(&self) -> &[SectionEntry] {
        &self.entries
    }

    pub fn panel(&self, section: &str) -> Option<&Kept> {
        self.views.get(section).map(|v| &v.panel)
    }

    pub fn page(&self, section: &str) -> Option<&Kept> {
        self.views.get(section).map(|v| &v.page)
    }

    fn drawn(&self, entry: &SectionEntry, folded: bool, ctx: &blocks::Ctx) -> Option<Drawn> {
        let kept = self.panel(&entry.name);
        let content = kept.and_then(|k| k.content.as_ref());
        let failure = kept.and_then(|k| k.failure.as_ref());
        let (groups, moving) = content
            .map(|c| blocks::groups(&c.blocks, ctx))
            .unwrap_or_default();
        if groups.is_empty() && failure.is_none() {
            return None;
        }
        let note = content.map_or(&[][..], |c| &c.note[..]);
        let note = [ctx.spans(note)];
        let counts = content.and_then(|c| c.blocks.iter().find_map(counted));
        let ladder = counts.map(|states| count_ladder(&states, &ctx.glyphs));
        let rungs = match &ladder {
            Some(rungs) if note[0].is_empty() => rungs,
            _ => &note[..],
        };
        let title = blocks::title(&entry.title, rungs, folded, ctx);
        let mut rows = vec![
            Row::Edge,
            Row::Inside(title, Some(RowOf::Title(entry.name.clone())), None),
        ];
        let failed = failure.map(|f| vec![(blocks::failure(f, ctx), None)]);
        let shown = if folded { vec![] } else { groups };
        for group in failed.into_iter().chain(shown) {
            rows.push(Row::Edge);
            rows.extend(group.into_iter().map(|(line, strip)| {
                Row::Inside(line, Some(RowOf::Body(entry.name.clone())), strip)
            }));
        }
        Some(Drawn {
            name: entry.name.clone(),
            order: entry.order,
            rows,
            moving: moving && !folded,
        })
    }

    pub fn draw(&self, view: &PanelView<'_>) -> Panel {
        let ctx = blocks::Ctx {
            layout: Layout {
                width: frame_inner(view.layout).width.saturating_sub(1),
                ..view.layout
            },
            glyphs: Glyphs::of(view.layout.glyphs),
            at: view.at,
            now_ms: view.now_ms,
            cx: view.cx,
            meter_max: usize::MAX,
            pictures: view.pictures,
        };
        let drawn: Vec<Drawn> = self
            .entries
            .iter()
            .filter(|e| view.on.contains(&e.name))
            .filter_map(|e| self.drawn(e, view.folded.contains(&e.name), &ctx))
            .collect();
        let moving: Vec<String> = (drawn.iter())
            .filter(|d| d.moving)
            .map(|d| d.name.clone())
            .collect();
        let (pinned, rest): (Vec<Drawn>, Vec<Drawn>) = drawn
            .into_iter()
            .partition(|d| view.pinned.contains(&d.name));
        let shared = !pinned.is_empty() && !rest.is_empty();
        let height = view.height.saturating_sub(1);
        let mut scrolling = Scrolling::new(&rest, shared);
        let pinned_rows = pinned_height(stacked(&pinned), scrolling.total(), height);
        scrolling.height = height - pinned_rows;
        let mut rows = Vec::new();
        pinned_part(&pinned, pinned_rows, shared, &ctx, &mut rows);
        let offset = scrolling.offset(view.position, true).min(scrolling.most());
        scrolling_part(&rest, &scrolling, offset, &ctx, &mut rows);
        let (out, strips) = boxed(rows, view.whole, view.layout);
        let moving = (out.hits.iter().flatten())
            .any(|row| matches!(row, RowOf::Body(name) if moving.contains(name)));
        Panel {
            lines: out.lines,
            rows: out.hits,
            strips,
            position: scrolling.position(offset),
            pinned_rows,
            moving,
            scrolling,
        }
    }
}

fn counted(block: &SlotBlock) -> Option<Vec<NodeState>> {
    match block {
        SlotBlock::Topology {
            nodes,
            counted: Some(SlotNoun::Agents),
            ..
        } => Some(nodes.iter().map(|n| n.state).collect()),
        _ => None,
    }
}

fn pinned_height(asked: usize, below: usize, height: usize) -> usize {
    asked.min((height / 2).max(height.saturating_sub(below)))
}

fn stacked(sections: &[Drawn]) -> usize {
    sections.iter().map(Drawn::asked).sum::<usize>() + usize::from(!sections.is_empty())
}

fn worded(rows: &[Row]) -> usize {
    rows.iter().filter(|r| !matches!(r, Row::Edge)).count()
}

pub fn rail(set: GlyphSet, whole: bool) -> StyledSpan {
    if whole {
        StyledSpan::plain(" ")
    } else {
        StyledSpan::new(frame_for(set).down, Token::Line)
    }
}

fn boxed(rows: Vec<Row>, whole: bool, layout: Layout) -> (Rows<RowOf>, Vec<Option<Strip>>) {
    let frame = frame_for(layout.glyphs);
    let side = width(frame.down, layout.width_mode);
    let inside: Vec<bool> = rows.iter().map(|r| matches!(r, Row::Inside(..))).collect();
    let mut out = Rows::default();
    let mut strips = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        strips.push(match &row {
            Row::Inside(_, _, Some(strip)) => Some(Strip {
                x: strip.x + side,
                ..strip.clone()
            }),
            _ => None,
        });
        match row {
            Row::Inside(line, hit, _) => out.push(frame_row(line, layout), hit),
            Row::Outside(line) => {
                let mut spans = vec![rail(layout.glyphs, whole)];
                spans.extend(line.spans);
                out.push(StyledLine::new(spans), None);
            }
            Row::Edge => {
                let above = i > 0 && inside[i - 1];
                let below = inside.get(i + 1).copied().unwrap_or(false);
                let (left, right) = match (above, below) {
                    (true, true) => (frame.left_tee, frame.right_tee),
                    (false, true) if whole || i == 0 => (frame.top_left, frame.top_right),
                    (false, true) => (frame.left_tee, frame.top_right),
                    _ if whole => (frame.bottom_left, frame.bottom_right),
                    _ => (frame.left_tee, frame.bottom_right),
                };
                out.push(edge_line(left, right, Token::Line, layout), None);
            }
        }
    }
    (out, strips)
}

fn water_fill(asks: &[usize], room: usize) -> Vec<usize> {
    if asks.iter().sum::<usize>() <= room {
        return asks.to_vec();
    }
    let given = |level: usize| asks.iter().map(|a| (*a).min(level)).sum::<usize>();
    let top = asks.iter().copied().max().unwrap_or(0);
    let level = (0..=top)
        .take_while(|l| given(*l) <= room)
        .last()
        .unwrap_or(0);
    let mut left = room.saturating_sub(given(level));
    asks.iter()
        .map(|a| {
            let mut grant = (*a).min(level);
            if *a > level && left > 0 {
                grant += 1;
                left -= 1;
            }
            grant
        })
        .collect()
}

fn cut_into(section: &Drawn, grant: usize, ctx: &blocks::Ctx, out: &mut Vec<Row>) {
    if grant >= section.asked() || grant < PINNED_FLOOR {
        out.extend(section.rows.iter().take(grant).cloned());
        return;
    }
    let shown = grant - 1;
    out.extend(section.rows.iter().take(shown).cloned());
    let hidden = worded(&section.rows[shown..]);
    out.push(Row::Inside(
        ctx.faint_row(copy::more_rows(ctx.glyphs.more, hidden)),
        Some(RowOf::Body(section.name.clone())),
        None,
    ));
}

fn pinned_part(
    sections: &[Drawn],
    room: usize,
    shared: bool,
    ctx: &blocks::Ctx,
    out: &mut Vec<Row>,
) {
    if sections.is_empty() || room == 0 {
        return;
    }
    let start = out.len();
    let asks: Vec<usize> = sections.iter().map(Drawn::asked).collect();
    let all = sections.len();
    let overhead =
        |kept: usize| usize::from(kept > 0) + usize::from(kept < all) * (1 + usize::from(shared));
    let floor =
        |kept: usize| -> usize { asks.iter().take(kept).map(|a| (*a).min(PINNED_FLOOR)).sum() };
    let kept = (1..=all)
        .rev()
        .find(|k| overhead(*k) + floor(*k) <= room)
        .unwrap_or(0);
    let grants = water_fill(&asks[..kept], room.saturating_sub(overhead(kept)));
    for (section, grant) in sections.iter().zip(grants) {
        cut_into(section, grant, ctx, out);
    }
    if kept > 0 {
        out.push(Row::Edge);
    }
    if kept < all {
        out.push(Row::Outside(
            ctx.faint_row(copy::more_sections(ctx.glyphs.more, all - kept)),
        ));
    }
    while out.len() - start + usize::from(shared) < room {
        out.push(Row::Outside(ctx.blank()));
    }
    if shared && !matches!(out.last(), Some(Row::Edge)) {
        out.push(Row::Edge);
    }
    out.truncate(start + room);
}

fn scrolling_part(
    sections: &[Drawn],
    scrolling: &Scrolling,
    offset: usize,
    ctx: &blocks::Ctx,
    out: &mut Vec<Row>,
) {
    let total = scrolling.total();
    if total == 0 {
        return;
    }
    let skip = usize::from(scrolling.shared);
    let mut all: Vec<Row> = sections
        .iter()
        .flat_map(|s| s.rows.iter().cloned())
        .skip(skip)
        .collect();
    all.push(Row::Edge);
    let height = scrolling.height;
    let end = total.min(offset + height);
    let mut window = all.get(offset..end).unwrap_or_default().to_vec();
    if total > height && height >= MORE_ROWS_FROM {
        let more = |arrow: &str, hidden: &[Row]| {
            Row::Inside(more_row(arrow, worded(hidden), " ", ctx.layout), None, None)
        };
        if offset > 0 {
            window[0] = more(up_arrow(&ctx.glyphs), &all[..=offset]);
        }
        if offset + height < total {
            let last = window.len() - 1;
            window[last] = more(down_arrow(&ctx.glyphs), &all[offset + last..]);
        }
    }
    out.extend(window);
}

impl Scrolling {
    fn new(sections: &[Drawn], shared: bool) -> Scrolling {
        let mut start = 0;
        let mut parts: Vec<Part> = sections
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let lines = s.asked().saturating_sub(usize::from(shared && i == 0));
                let part = Part {
                    name: s.name.clone(),
                    order: s.order,
                    start,
                    lines,
                };
                start += lines;
                part
            })
            .collect();
        if let Some(last) = parts.last_mut() {
            last.lines += 1;
        }
        Scrolling {
            parts,
            height: 0,
            shared,
        }
    }

    fn total(&self) -> usize {
        self.parts.last().map_or(0, |p| p.start + p.lines)
    }

    fn most(&self) -> usize {
        self.total().saturating_sub(self.height)
    }

    fn offset(&self, position: &Position, settle: bool) -> usize {
        let Position::At {
            section,
            order,
            row,
        } = position
        else {
            return 0;
        };
        match self.parts.iter().find(|p| &p.name == section) {
            Some(part) if settle && *row >= part.lines => part.start + part.lines - 1,
            Some(part) => part.start.saturating_add(*row),
            None => self
                .parts
                .iter()
                .find(|p| (p.order, &p.name) > (*order, section))
                .map_or(self.total(), |p| p.start),
        }
    }

    fn position(&self, offset: usize) -> Position {
        if offset == 0 {
            return Position::Top;
        }
        let Some(part) = self.parts.iter().rev().find(|p| p.start <= offset) else {
            return Position::Top;
        };
        Position::At {
            section: part.name.clone(),
            order: part.order,
            row: offset - part.start,
        }
    }
}

impl Panel {
    pub fn scroll(&self, position: &Position, step: Step) -> Position {
        let s = &self.scrolling;
        let from = s.offset(position, false);
        let page = s.height.saturating_sub(1).max(1);
        let to = match step {
            Step::Up(n) => from.saturating_sub(n),
            Step::Down(n) => from.saturating_add(n).min(s.most()),
            Step::PageUp => from.saturating_sub(page),
            Step::PageDown => from.saturating_add(page).min(s.most()),
            Step::Top => 0,
            Step::End => s.most(),
        };
        s.position(to)
    }
}

#[cfg(any(test, feature = "testing"))]
pub fn fill_scenes(
    fill: &wisp_protocol::slot::SlotFill,
    width: usize,
) -> Vec<crate::picture::Scene> {
    use crate::motion::MotionPolicy;
    use crate::theme::Depth;
    let ctx = blocks::Ctx {
        layout: Layout::new(width, GlyphSet::Unicode),
        glyphs: Glyphs::of(GlyphSet::Unicode),
        at: Duration::ZERO,
        now_ms: 0,
        cx: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
        meter_max: usize::MAX,
        pictures: Some(9),
    };
    let groups = blocks::fill_groups(fill, &ctx);
    let mut scenes: Vec<_> = groups
        .into_iter()
        .flatten()
        .filter_map(|(_, s)| s)
        .collect();
    scenes.dedup_by(|a, b| std::sync::Arc::ptr_eq(&a.scene, &b.scene));
    scenes.into_iter().map(|s| (*s.scene).clone()).collect()
}

#[cfg(test)]
mod tests;
