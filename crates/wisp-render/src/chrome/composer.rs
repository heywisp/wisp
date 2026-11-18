use super::row::clip;
use super::welcome::{Frame, frame_for};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{spans_width, width};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

pub const COMPOSER_MAX_ROWS: usize = 8;

pub const BOX_FROM_COLUMNS: usize = 21;

pub const BOX_FROM_ROWS: usize = 5;

pub const TOP_ROW_FROM_ROWS: usize = 9;

pub const FRAMED_STATUS_FROM_ROWS: usize = 7;

pub const STATUS_FROM_ROWS: usize = 2;

const ROOMY_GUTTER: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComposerLook {
    pub band: Look,
    pub frame: Look,
    pub prompt: Look,
    pub text: Look,
    pub placeholder: Look,
    pub chip: Look,
}

impl ComposerLook {
    pub fn new() -> ComposerLook {
        let band = Look::default();
        ComposerLook {
            band,
            frame: Look::fg(Token::Line),
            prompt: Look::fg(Token::Accent),
            text: band,
            placeholder: Look::fg(Token::TextFaint),
            chip: Look::fg(Token::Text).on(Token::AccentTint),
        }
    }

    pub fn keyless() -> ComposerLook {
        ComposerLook {
            prompt: Look::fg(Token::TextFaint),
            text: Look::fg(Token::TextMuted),
            chip: Look::fg(Token::TextMuted).on(Token::AccentTint),
            ..ComposerLook::new()
        }
    }

    pub fn placeholder_text(&self) -> &'static str {
        copy::PLACEHOLDER
    }
}

impl Default for ComposerLook {
    fn default() -> ComposerLook {
        ComposerLook::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComposerShape {
    pub layout: Layout,
    pub boxed: bool,
    pub keys: bool,
}

impl ComposerShape {
    pub fn open(layout: Layout) -> ComposerShape {
        ComposerShape {
            layout,
            boxed: false,
            keys: true,
        }
    }

    pub fn on_screen(layout: Layout, height: usize) -> ComposerShape {
        ComposerShape {
            boxed: fits_box(layout.width, height),
            ..ComposerShape::open(layout)
        }
    }

    pub fn with_keys(self, keys: bool) -> ComposerShape {
        ComposerShape { keys, ..self }
    }

    pub fn look(self) -> ComposerLook {
        if self.keys {
            ComposerLook::new()
        } else {
            ComposerLook::keyless()
        }
    }

    fn side(self) -> usize {
        if self.boxed {
            width(frame_for(self.layout.glyphs).down, self.layout.width_mode)
        } else {
            0
        }
    }

    pub fn lead(self) -> usize {
        let layout = self.layout;
        let prompt = width(Glyphs::of(layout.glyphs).prompt, layout.width_mode);
        if self.boxed {
            return self.side() + 1 + prompt + 1;
        }
        let before = usize::from(layout.gutter() >= ROOMY_GUTTER);
        layout.gutter().max(before + prompt + 1)
    }

    fn tail(self) -> usize {
        if self.boxed { 1 + self.side() } else { 1 }
    }

    pub fn room(self) -> usize {
        self.layout
            .width
            .saturating_sub(self.lead() + self.tail())
            .max(1)
    }
}

pub fn fits_box(columns: usize, rows: usize) -> bool {
    columns >= BOX_FROM_COLUMNS && rows >= BOX_FROM_ROWS
}

fn lead(first: bool, shape: ComposerShape) -> Vec<StyledSpan> {
    let look = shape.look();
    let layout = shape.layout;
    let glyphs = Glyphs::of(layout.glyphs);
    let prompt = width(glyphs.prompt, layout.width_mode);
    let total = shape.lead();
    let mut spans = vec![];
    let mut used = 0;
    if shape.boxed {
        spans.push(StyledSpan::new(frame_for(layout.glyphs).down, look.frame));
        used = shape.side();
    }
    let before = if shape.boxed {
        1
    } else {
        usize::from(layout.gutter() >= ROOMY_GUTTER)
    };
    if first {
        spans.push(StyledSpan::new(" ".repeat(before), look.band));
        spans.push(StyledSpan::new(glyphs.prompt, look.prompt));
        used += before + prompt;
    }
    spans.push(StyledSpan::new(" ".repeat(total - used), look.band));
    spans
}

pub fn composer_lead_width(shape: ComposerShape) -> usize {
    shape.lead()
}

pub fn composer_spans(first: bool, body: Vec<StyledSpan>, shape: ComposerShape) -> StyledLine {
    let look = shape.look();
    let layout = shape.layout;
    let used = shape.lead() + spans_width(&body, layout.width_mode);
    let mut spans = lead(first, shape);
    spans.extend(body);
    let side = shape.side();
    spans.push(StyledSpan::new(
        " ".repeat(layout.width.saturating_sub(used + side)),
        look.band,
    ));
    if shape.boxed {
        spans.push(StyledSpan::new(frame_for(layout.glyphs).down, look.frame));
    }
    spans.retain(|s| !s.text.is_empty());
    StyledLine::new(spans)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Divider,
    Bottom,
}

impl Edge {
    fn corners(self, frame: Frame) -> (&'static str, &'static str) {
        match self {
            Edge::Top => (frame.top_left, frame.top_right),
            Edge::Divider => (frame.left_tee, frame.right_tee),
            Edge::Bottom => (frame.bottom_left, frame.bottom_right),
        }
    }
}

pub fn composer_edge(shape: ComposerShape, at: Edge) -> StyledLine {
    let layout = shape.layout;
    let (left, right) = at.corners(frame_for(layout.glyphs));
    edge_line(left, right, shape.look().frame, layout)
}

pub fn border(
    at: Edge,
    title: Vec<StyledSpan>,
    label: Option<StyledSpan>,
    layout: Layout,
) -> (StyledLine, bool) {
    let frame = frame_for(layout.glyphs);
    let mode = layout.width_mode;
    let (left, right) = at.corners(frame);
    let rule = width(frame.across, mode);
    let closing = |label: &StyledSpan| 2 + width(&label.text, mode) + rule + width(right, mode);
    let label = label.filter(|l| layout.width >= width(left, mode) + rule + closing(l));
    let end = label.as_ref().map_or(width(right, mode), closing);
    let lead = format!("{left}{} ", frame.across);
    let room = layout
        .width
        .saturating_sub(width(&lead, mode) + 1 + rule + end);
    let title = clip(title, room, Glyphs::of(layout.glyphs).more, mode);
    let (mut spans, open) = match title.is_empty() {
        true => (vec![], left.to_owned()),
        false => {
            let mut spans = vec![StyledSpan::new(lead, Token::Line)];
            spans.extend(title);
            (spans, " ".to_owned())
        }
    };
    let used = spans_width(&spans, mode) + width(&open, mode) + end;
    let fill = frame
        .across
        .repeat(layout.width.saturating_sub(used) / rule.max(1));
    let drawn = label.is_some();
    match label {
        Some(label) => spans.extend([
            StyledSpan::new(format!("{open}{fill} "), Token::Line),
            label,
            StyledSpan::new(format!(" {}{right}", frame.across), Token::Line),
        ]),
        None => spans.push(StyledSpan::new(format!("{open}{fill}{right}"), Token::Line)),
    }
    (StyledLine::new(spans), drawn)
}

pub fn frame_inner(layout: Layout) -> Layout {
    let side = width(frame_for(layout.glyphs).down, layout.width_mode);
    Layout {
        width: layout.width.saturating_sub(2 * side),
        ..layout
    }
}

pub fn frame_row(line: StyledLine, layout: Layout) -> StyledLine {
    let inner = frame_inner(layout);
    let side = StyledSpan::new(frame_for(layout.glyphs).down, Token::Line);
    let more = Glyphs::of(layout.glyphs).more;
    let body = clip(line.spans, inner.width, more, layout.width_mode);
    let pad = inner
        .width
        .saturating_sub(spans_width(&body, layout.width_mode));
    let mut spans = vec![side.clone()];
    spans.extend(body);
    spans.push(StyledSpan::plain(" ".repeat(pad)));
    spans.push(side);
    spans.retain(|s| !s.text.is_empty());
    StyledLine::new(spans)
}

pub(super) fn edge_line(
    left: &str,
    right: &str,
    look: impl Into<Look>,
    layout: Layout,
) -> StyledLine {
    let across = frame_for(layout.glyphs).across;
    StyledLine::new(vec![StyledSpan::new(
        edge(left, across, right, 0, layout),
        look,
    )])
}

pub(super) fn edge(left: &str, fill: &str, right: &str, used: usize, layout: Layout) -> String {
    let mode = layout.width_mode;
    let n = layout
        .width
        .saturating_sub(used + width(left, mode) + width(right, mode))
        / width(fill, mode).max(1);
    format!("{left}{}{right}", fill.repeat(n))
}

pub fn placeholder_for(shape: ComposerShape, text: Option<&str>) -> String {
    let room = shape.room();
    let mode = shape.layout.width_mode;
    let full = text.unwrap_or(copy::PLACEHOLDER);
    if width(full, mode) <= room {
        return full.to_owned();
    }
    let short = text.unwrap_or(copy::PLACEHOLDER_SHORT);
    let more = Glyphs::of(shape.layout.glyphs).more;
    clip(vec![StyledSpan::plain(short)], room, more, mode)
        .into_iter()
        .map(|s| s.text)
        .collect()
}

pub fn composer_placeholder(shape: ComposerShape, text: Option<&str>) -> StyledLine {
    let look = shape.look();
    composer_spans(
        true,
        vec![StyledSpan::new(
            placeholder_for(shape, text),
            look.placeholder,
        )],
        shape,
    )
}
