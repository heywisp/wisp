use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use wisp_render::chrome::{ORB_GAP, clip, edge, place_caret};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::motion::{MotionCtx, OrbFrame};
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::theme::{Ink, Token};
use wisp_render::transcript::{RowsVersion, Transcript, copy};

use super::presenter::note_rows;
use crate::term::contain;

const WARNING: Token = Token::Warn;

fn warning_rows(mut text: &str, layout: Layout) -> Vec<StyledLine> {
    let mut rows = Vec::new();
    let measure = layout
        .width
        .saturating_sub(layout.gutter() + super::presenter::NOTE_HANG)
        .max(1);
    if text.starts_with('/') {
        while wisp_render::text::width(text, layout.width_mode) > measure {
            let end = text
                .char_indices()
                .filter(|(index, ch)| {
                    *ch == '/'
                        && *index > 0
                        && wisp_render::text::width(&text[..*index + 1], layout.width_mode)
                            <= measure
                })
                .map(|(index, _)| index + 1)
                .next_back();
            let Some(end) = end else {
                break;
            };
            rows.extend(note_rows(&text[..end], layout));
            text = &text[end..];
        }
    }
    rows.extend(note_rows(text, layout));
    rows
}

pub const KEEP_SCREENS: usize = 2;
pub const SLICE: Duration = Duration::from_millis(4);
pub const SLICE_MARGIN: Duration = Duration::from_millis(1);
const PLACE_ROWS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliceClock {
    started: Instant,
    last: Instant,
    longest: Duration,
}

impl SliceClock {
    pub fn new(now: Instant) -> SliceClock {
        SliceClock {
            started: now,
            last: now,
            longest: Duration::ZERO,
        }
    }

    pub fn out_of_time(&mut self, now: Instant) -> bool {
        self.longest = self.longest.max(now.saturating_duration_since(self.last));
        self.last = now;
        now.saturating_duration_since(self.started) + self.longest + SLICE_MARGIN > SLICE
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Banner,
    Cell(usize),
    Note(Vec<String>, Token),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stamp {
    Fixed,
    Cell(RowsVersion),
    Volatile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Measured {
    width: usize,
    stamp: Stamp,
    height: usize,
}

#[derive(Clone, Debug)]
struct Item {
    source: Source,
    measured: Option<Measured>,
    rows: Option<Vec<StyledLine>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Follow,
    At { item: usize, offset: usize },
}

#[derive(Clone, Copy, Debug)]
pub struct Caret {
    pub cell: usize,
    pub core: Ink,
    pub cx: MotionCtx,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Waiting(OrbFrame),
    Reports(OrbFrame),
    Taken,
    Off,
}

pub struct Paint<'a> {
    pub transcript: &'a mut Transcript,
    pub layout: Layout,
    pub banner: &'a [StyledLine],
    pub banner_live: bool,
    pub caret: Option<Caret>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sweep {
    width: usize,
    center: usize,
    distance: usize,
}

pub type Origin = Option<(usize, usize)>;

#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub rows: Vec<StyledLine>,
    pub origins: Vec<Origin>,
    pub top: Option<(usize, usize)>,
    pub following: bool,
    pub short: bool,
}

#[derive(Clone, Debug)]
pub struct View {
    items: Vec<Item>,
    cells: usize,
    width: usize,
    anchor: Anchor,
    paused_at: usize,
    rescale: Option<(usize, usize)>,
    sweep: Option<Sweep>,
    cached: BTreeSet<usize>,
    renders: u64,
    rendered: Vec<usize>,
    height: usize,
    revealing: Option<(usize, usize)>,
    wait: Wait,
    place_open: bool,
    last: Option<(usize, usize)>,
}

pub(super) fn first_blank(rows: &[StyledLine]) -> bool {
    rows.first()
        .is_some_and(|r| r.band.is_none() && r.spans.iter().all(|s| s.text.trim().is_empty()))
}

impl View {
    pub fn new(width: usize) -> View {
        View {
            items: vec![Item {
                source: Source::Banner,
                measured: None,
                rows: None,
            }],
            cells: 0,
            width,
            anchor: Anchor::Follow,
            paused_at: 0,
            rescale: None,
            sweep: None,
            cached: BTreeSet::new(),
            renders: 0,
            rendered: vec![],
            height: 0,
            revealing: None,
            wait: Wait::Off,
            place_open: false,
            last: None,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn set_wait(&mut self, wait: Wait) {
        self.wait = wait;
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn anchor(&self) -> Anchor {
        self.anchor
    }

    pub fn is_following(&self) -> bool {
        self.anchor == Anchor::Follow
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn source(&self, item: usize) -> Option<&Source> {
        self.items.get(item).map(|i| &i.source)
    }

    pub fn renders(&self) -> u64 {
        self.renders
    }

    pub fn take_rendered(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.rendered)
    }

    pub fn cached_rows(&self) -> Vec<usize> {
        self.cached.iter().copied().collect()
    }

    pub fn stale(&self) -> usize {
        (0..self.items.len()).filter(|&i| self.stale_at(i)).count()
    }

    fn stale_at(&self, index: usize) -> bool {
        self.items
            .get(index)
            .and_then(|i| i.measured)
            .is_some_and(|m| m.width != self.width)
    }

    pub fn sweeping(&self) -> bool {
        self.sweep.is_some()
    }

    pub fn unseen(&self) -> usize {
        match self.anchor {
            Anchor::Follow => 0,
            Anchor::At { .. } => self.items.len().saturating_sub(self.paused_at),
        }
    }

    pub fn sync(&mut self, cells: usize) {
        while self.cells < cells {
            self.items.push(Item {
                source: Source::Cell(self.cells),
                measured: None,
                rows: None,
            });
            self.cells += 1;
        }
    }

    pub fn cut(&mut self, cells: usize) {
        let keep = self
            .items
            .iter()
            .position(|i| matches!(i.source, Source::Cell(cell) if cell >= cells))
            .unwrap_or(self.items.len());
        self.items.truncate(keep);
        self.cells = self.cells.min(cells);
        self.cached.retain(|&index| index < keep);
        self.anchor = Anchor::Follow;
        self.paused_at = self.paused_at.min(keep);
        self.rescale = None;
        self.sweep = None;
        self.revealing = None;
    }

    pub fn refresh_banner(&mut self) {
        if let Some(item) = self.items.get_mut(0) {
            item.measured = None;
            item.rows = None;
        }
        self.cached.remove(&0);
    }

    pub fn note(&mut self, lines: Vec<String>) {
        self.items.push(Item {
            source: Source::Note(lines, Token::TextMuted),
            measured: None,
            rows: None,
        });
    }

    pub fn warning(&mut self, lines: Vec<String>) {
        self.note(lines);
        if let Some(item) = self.items.last_mut()
            && let Source::Note(_, token) = &mut item.source
        {
            *token = WARNING;
        }
    }

    pub fn set_width(&mut self, width: usize) {
        if width == self.width {
            return;
        }
        self.relayout(width);
    }

    pub fn rewrap(&mut self) {
        self.relayout(self.width);
        for item in &mut self.items {
            item.measured = None;
        }
    }

    fn relayout(&mut self, width: usize) {
        if let Anchor::At { item, .. } = self.anchor
            && let Some(m) = self.items.get(item).and_then(|i| i.measured)
            && m.width == self.width
        {
            self.rescale = Some((item, m.height));
        }
        self.width = width;
        for index in std::mem::take(&mut self.cached) {
            if let Some(item) = self.items.get_mut(index) {
                item.rows = None;
            }
        }
        let center = match self.anchor {
            Anchor::Follow => self.items.len().saturating_sub(1),
            Anchor::At { item, .. } => item,
        };
        self.sweep = Some(Sweep {
            width,
            center,
            distance: 0,
        });
    }

    fn stamp(&self, index: usize, paint: &mut Paint<'_>, r: &Renderer<'_>) -> Stamp {
        match self.items.get(index).map(|i| &i.source) {
            Some(Source::Banner) if paint.banner_live => Stamp::Volatile,
            Some(Source::Banner | Source::Note(..)) | None => Stamp::Fixed,
            Some(Source::Cell(cell)) => {
                let cell = *cell;
                let open = paint
                    .transcript
                    .cells()
                    .get(cell)
                    .is_none_or(|c| c.is_open());
                if open || paint.caret.is_some_and(|c| c.cell == cell) {
                    return Stamp::Volatile;
                }
                let detail = paint.transcript.view().detail;
                match paint.transcript.cell_version(cell, r, detail) {
                    Some(v) => Stamp::Cell(v),
                    None => Stamp::Volatile,
                }
            }
        }
    }

    fn render(&mut self, index: usize, paint: &mut Paint<'_>, r: &Renderer<'_>) -> Vec<StyledLine> {
        self.renders += 1;
        self.rendered.push(index);
        let Some(source) = self.items.get(index).map(|i| i.source.clone()) else {
            return vec![];
        };
        let cells_since = |earlier: usize| {
            self.items[..index]
                .iter()
                .rev()
                .find(|i| !matches!(i.source, Source::Cell(c) if c > earlier))
                .is_some_and(|i| matches!(i.source, Source::Cell(c) if c == earlier))
        };
        let mut glued = false;
        let mut own = match source {
            Source::Banner => paint.banner.to_vec(),
            Source::Note(lines, token) => lines
                .iter()
                .flat_map(|line| {
                    let mut rows = if token == WARNING {
                        warning_rows(line, paint.layout)
                    } else {
                        note_rows(line, paint.layout)
                    };
                    for row in &mut rows {
                        for span in &mut row.spans {
                            if span.look.fg.is_some() {
                                span.look.fg = Some(token.into());
                            }
                        }
                    }
                    rows
                })
                .collect(),
            Source::Cell(cell) => {
                let transcript = &mut *paint.transcript;
                let detail = transcript.view().detail;
                let cell_rows = contain(|| transcript.cell_rows(cell, r, detail))
                    .ok()
                    .flatten();
                glued = cell_rows
                    .as_ref()
                    .and_then(|c| c.version.grouped_after)
                    .is_some_and(cells_since);
                let mut rows = cell_rows.map(|c| c.rows).unwrap_or_default();
                if first_blank(&rows) && rows.len() > 1 {
                    rows.remove(0);
                }
                if let Some(caret) = paint.caret.filter(|c| c.cell == cell)
                    && !rows.is_empty()
                {
                    let n = rows.len();
                    place_caret(&mut rows, Some(n), caret.core, caret.cx, paint.layout);
                }
                rows
            }
        };
        if index > 0 && !own.is_empty() && !glued {
            own.insert(0, StyledLine::default());
        }
        own
    }

    fn measure(&mut self, index: usize, paint: &mut Paint<'_>, keep: bool) -> usize {
        let r = Renderer::new(paint.layout);
        let stamp = self.stamp(index, paint, &r);
        let width = self.width;
        if let Some(item) = self.items.get(index)
            && let Some(m) = item.measured
            && m.width == width
            && m.stamp == stamp
            && stamp != Stamp::Volatile
            && (!keep || item.rows.is_some())
        {
            return m.height;
        }
        let rows = self.render(index, paint, &r);
        let height = rows.len();
        if let Some(item) = self.items.get_mut(index) {
            item.measured = Some(Measured {
                width,
                stamp,
                height,
            });
            if keep {
                item.rows = Some(rows);
                self.cached.insert(index);
            } else {
                item.rows = None;
                self.cached.remove(&index);
            }
        }
        height
    }

    fn known_height(&self, index: usize) -> Option<usize> {
        self.items
            .get(index)
            .and_then(|i| i.measured)
            .filter(|m| m.width == self.width)
            .map(|m| m.height)
    }

    pub fn item_rows(&mut self, index: usize, paint: &mut Paint<'_>) -> Vec<StyledLine> {
        self.rows_of(index, paint)
    }

    fn rows_of(&mut self, index: usize, paint: &mut Paint<'_>) -> Vec<StyledLine> {
        self.measure(index, paint, true);
        self.items
            .get(index)
            .and_then(|i| i.rows.clone())
            .unwrap_or_default()
    }

    fn apply_rescale(&mut self, paint: &mut Paint<'_>) {
        let Some((item, old)) = self.rescale.take() else {
            return;
        };
        if let Anchor::At {
            item: anchored,
            offset,
        } = self.anchor
            && anchored == item
        {
            let new = self.measure(item, paint, true);
            let scaled = offset.saturating_mul(new).checked_div(old).unwrap_or(0);
            self.anchor = Anchor::At {
                item,
                offset: scaled.min(new.saturating_sub(1)),
            };
        }
    }

    fn normalize(&mut self, paint: &mut Paint<'_>) {
        let Anchor::At {
            mut item,
            mut offset,
        } = self.anchor
        else {
            return;
        };
        item = item.min(self.items.len().saturating_sub(1));
        loop {
            let h = self.measure(item, paint, true);
            if offset < h || item + 1 >= self.items.len() {
                break;
            }
            offset -= h;
            item += 1;
        }
        self.anchor = Anchor::At { item, offset };
    }

    fn last_row(&mut self, paint: &mut Paint<'_>) -> Option<(usize, usize)> {
        (0..self.items.len()).rev().find_map(|index| {
            let rows =
                (self.known_height(index)).unwrap_or_else(|| self.measure(index, paint, false));
            (rows > 0).then_some((index, rows))
        })
    }

    fn settle_place(&mut self, paint: &mut Paint<'_>) -> bool {
        let last = self.last_row(paint);
        let landed = last != self.last;
        let new_item = last.map(|(index, _)| index) != self.last.map(|(index, _)| index);
        self.last = last;
        let open = match self.wait {
            Wait::Taken => false,
            Wait::Waiting(_) | Wait::Reports(_) if landed => true,
            Wait::Off if new_item && self.place_open => {
                let rows = last.map(|(index, _)| self.rows_of(index, paint));
                !rows.is_some_and(|rows| first_blank(&rows))
            }
            _ => self.place_open,
        };
        let changed = open != self.place_open;
        self.place_open = open;
        changed
    }

    fn place_shown(&self, height: usize, paint: &Paint<'_>) -> bool {
        self.place_open && height > PLACE_ROWS && paint.layout.gutter() > 0
    }

    fn follow_layout(&mut self, height: usize, paint: &mut Paint<'_>) -> Shown {
        if !self.place_shown(height, paint) {
            return self.follow_items(height, paint);
        }
        let mut shown = self.follow_items(height - PLACE_ROWS, paint);
        let mark = match self.wait {
            Wait::Waiting(frame) => StyledLine::new(frame.spans()),
            Wait::Reports(frame) => {
                let layout = paint.layout;
                let mut spans = frame.spans();
                spans.push(StyledSpan::plain(" ".repeat(ORB_GAP)));
                spans.push(StyledSpan::new(
                    copy::waiting_for_reports(),
                    Token::TextMuted,
                ));
                let more = Glyphs::of(layout.glyphs).more;
                StyledLine::new(clip(spans, edge(layout), more, layout.width_mode))
            }
            Wait::Taken | Wait::Off => StyledLine::default(),
        };
        shown.rows.extend([StyledLine::default(), mark]);
        shown.origins.extend([None, None]);
        shown
    }

    fn follow_items(&mut self, height: usize, paint: &mut Paint<'_>) -> Shown {
        let mut need = height;
        let mut parts: Vec<(usize, Vec<StyledLine>)> = vec![];
        let mut index = self.items.len();
        while index > 0 && need > 0 {
            index -= 1;
            let rows = self.rows_of(index, paint);
            need = need.saturating_sub(rows.len());
            parts.push((index, rows));
        }
        parts.reverse();
        let total: usize = parts.iter().map(|(_, r)| r.len()).sum::<usize>();
        let numbered = |i: usize, rows: Vec<StyledLine>| {
            rows.into_iter()
                .enumerate()
                .map(move |(r, row)| (Some((i, r)), row))
        };
        if total <= height && index == 0 {
            let mut out: Vec<(Origin, StyledLine)> = vec![];
            let mut rest: Vec<(Origin, StyledLine)> = vec![];
            for (i, rows) in parts {
                if i == 0 {
                    out.extend(numbered(i, rows));
                } else {
                    rest.extend(numbered(i, rows));
                }
            }
            let gap = height.saturating_sub(out.len() + rest.len());
            out.extend(std::iter::repeat_n((None, StyledLine::default()), gap));
            out.extend(rest);
            let (origins, rows) = out.into_iter().unzip();
            return Shown {
                rows,
                origins,
                top: Some((0, 0)),
                following: true,
                short: true,
            };
        }
        let mut skip = total.saturating_sub(height);
        let mut top = None;
        let mut rows: Vec<(Origin, StyledLine)> = vec![];
        for (i, own) in parts {
            if top.is_none() {
                if skip < own.len() {
                    top = Some((i, skip));
                    rows.extend(numbered(i, own).skip(skip));
                    skip = 0;
                } else {
                    skip -= own.len();
                }
            } else {
                rows.extend(numbered(i, own));
            }
        }
        if top.is_none() {
            top = Some((self.items.len().saturating_sub(1), 0));
        }
        rows.truncate(height);
        let gap = height.saturating_sub(rows.len());
        let mut out: Vec<(Origin, StyledLine)> =
            std::iter::repeat_n((None, StyledLine::default()), gap).collect();
        out.extend(rows);
        let (origins, rows) = out.into_iter().unzip();
        Shown {
            rows,
            origins,
            top,
            following: true,
            short: false,
        }
    }

    fn anchored_layout(&mut self, height: usize, paint: &mut Paint<'_>) -> Option<Shown> {
        let Anchor::At { item, offset } = self.anchor else {
            return None;
        };
        let mut rows: Vec<StyledLine> = vec![];
        let mut origins: Vec<Origin> = vec![];
        let mut index = item;
        let mut skip = offset;
        while rows.len() < height + 1 && index < self.items.len() {
            let own = self.rows_of(index, paint);
            for (r, row) in own.into_iter().enumerate().skip(skip) {
                origins.push(Some((index, r)));
                rows.push(row);
            }
            skip = 0;
            index += 1;
        }
        if rows.len() <= height {
            return None;
        }
        rows.truncate(height);
        origins.truncate(height);
        Some(Shown {
            rows,
            origins,
            top: Some((item, offset)),
            following: false,
            short: false,
        })
    }

    pub fn layout(&mut self, height: usize, paint: &mut Paint<'_>) -> Shown {
        self.height = height;
        self.apply_rescale(paint);
        self.normalize(paint);
        let mut shown = self.place(height, paint);
        if let Some((item, row)) = self.revealing.take()
            && let Some(row) = self.first_row(item, row, paint)
            && !shown.origins.contains(&Some((item, row)))
        {
            let above = shown.top.is_some_and(|top| (item, row) < top);
            self.pause();
            self.anchor = Anchor::At { item, offset: row };
            if !above {
                self.scroll_up(height.saturating_sub(1), paint);
            }
            self.normalize(paint);
            shown = self.place(height, paint);
        }
        if self.settle_place(paint) && shown.following {
            shown = self.place(height, paint);
        }
        self.trim(&shown, height);
        shown
    }

    fn place(&mut self, height: usize, paint: &mut Paint<'_>) -> Shown {
        match self.anchored_layout(height, paint) {
            Some(shown) => shown,
            None => {
                self.anchor = Anchor::Follow;
                self.follow_layout(height, paint)
            }
        }
    }

    fn first_row(&mut self, item: usize, row: usize, paint: &mut Paint<'_>) -> Option<usize> {
        if item >= self.items.len() {
            return None;
        }
        let rows = self.rows_of(item, paint);
        if rows.is_empty() {
            return None;
        }
        let mut row = row.min(rows.len() - 1);
        while row + 1 < rows.len() && first_blank(&rows[row..]) {
            row += 1;
        }
        Some(row)
    }

    pub fn item_of(&self, cell: usize) -> Option<usize> {
        self.items
            .iter()
            .position(|i| i.source == Source::Cell(cell))
    }

    pub fn reveal(&mut self, item: usize, row: usize) {
        self.revealing = Some((item, row));
    }

    pub fn restore(&mut self, anchor: Anchor) {
        self.revealing = None;
        match anchor {
            Anchor::Follow => self.anchor = Anchor::Follow,
            Anchor::At { .. } => {
                self.pause();
                self.anchor = anchor;
            }
        }
    }

    fn trim(&mut self, shown: &Shown, height: usize) {
        let Some((top, _)) = shown.top else {
            return;
        };
        let reach = height.saturating_mul(KEEP_SCREENS);
        let mut low = top;
        let mut above = 0;
        while low > 0 && above < reach {
            match self.known_height(low - 1) {
                Some(h) => {
                    above += h;
                    low -= 1;
                }
                None => break,
            }
        }
        let mut high = top;
        let mut below = 0;
        while high + 1 < self.items.len() && below < reach + height {
            match self.known_height(high + 1) {
                Some(h) => {
                    below += h;
                    high += 1;
                }
                None => break,
            }
        }
        let drop: Vec<usize> = self
            .cached
            .iter()
            .copied()
            .filter(|&i| i < low || i > high)
            .collect();
        for index in drop {
            self.cached.remove(&index);
            if let Some(item) = self.items.get_mut(index) {
                item.rows = None;
            }
        }
    }

    fn top_of(&mut self, paint: &mut Paint<'_>) -> Option<(usize, usize)> {
        match self.anchor {
            Anchor::At { item, offset } => Some((item, offset)),
            Anchor::Follow => {
                let height = self.height;
                let shown = self.follow_layout(height, paint);
                if shown.short { None } else { shown.top }
            }
        }
    }

    fn pause(&mut self) {
        if self.anchor == Anchor::Follow {
            self.paused_at = self.items.len();
        }
    }

    pub fn scroll_up(&mut self, rows: usize, paint: &mut Paint<'_>) {
        let past_place = self.is_following() && self.place_shown(self.height, paint);
        let Some((mut item, mut offset)) = self.top_of(paint) else {
            return;
        };
        let mut left = if past_place {
            rows.max(PLACE_ROWS + 1)
        } else {
            rows
        };
        while left > 0 {
            if offset >= left {
                offset -= left;
                left = 0;
            } else {
                left -= offset;
                if item == 0 {
                    offset = 0;
                    break;
                }
                item -= 1;
                offset = self.measure(item, paint, true);
            }
        }
        self.pause();
        self.anchor = Anchor::At { item, offset };
        self.normalize(paint);
    }

    pub fn scroll_down(&mut self, rows: usize, paint: &mut Paint<'_>) {
        let Anchor::At { item, offset } = self.anchor else {
            return;
        };
        self.anchor = Anchor::At {
            item,
            offset: offset.saturating_add(rows),
        };
        self.normalize(paint);
        let Anchor::At { item, offset } = self.anchor else {
            return;
        };
        let mut below = 0usize;
        let mut index = item;
        while index < self.items.len() && below <= self.height + offset {
            below += self.measure(index, paint, true);
            index += 1;
        }
        if below.saturating_sub(offset) <= self.height {
            self.anchor = Anchor::Follow;
        }
    }

    pub fn to_top(&mut self) {
        self.pause();
        self.anchor = Anchor::At { item: 0, offset: 0 };
    }

    pub fn to_bottom(&mut self) {
        self.anchor = Anchor::Follow;
    }

    pub fn slice(&mut self, paint: &mut Paint<'_>, mut out_of_time: impl FnMut() -> bool) -> bool {
        let Some(mut sweep) = self.sweep else {
            return false;
        };
        if sweep.width != self.width {
            sweep = Sweep {
                width: self.width,
                center: sweep.center,
                distance: 0,
            };
        }
        let len = self.items.len();
        sweep.center = sweep.center.min(len.saturating_sub(1));
        loop {
            let d = sweep.distance;
            if d > sweep.center && sweep.center + d >= len {
                self.sweep = None;
                return false;
            }
            let mut sides = vec![];
            if let Some(below) = sweep.center.checked_sub(d) {
                sides.push(below);
            }
            if d > 0 && sweep.center + d < len {
                sides.push(sweep.center + d);
            }
            for index in sides {
                if self.stale_at(index) {
                    if out_of_time() {
                        self.sweep = Some(sweep);
                        return true;
                    }
                    self.measure(index, paint, false);
                }
            }
            sweep.distance += 1;
        }
    }
}

#[cfg(test)]
mod tests;
