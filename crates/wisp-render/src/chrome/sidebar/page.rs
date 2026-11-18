use std::time::Duration;

use wisp_protocol::slot::{SlotBlock, SlotContent, SlotSpan};

use super::super::band::band_cols;
use super::blocks::{self, Ctx};
use super::{Failure, Sidebar};
use crate::markdown::Layout;
use crate::motion::{MotionCtx, MotionPolicy};
use crate::styled::{StyledLine, StyledSpan};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

pub(super) const MIN_COLUMN: usize = 40;
pub(super) const PAGE_METER: usize = 40;
const COLUMN_GAP: usize = 2;
const STILL: MotionCtx = MotionCtx {
    policy: MotionPolicy::Reduced,
    ascii: false,
    blend: false,
};

#[derive(Clone, Copy, Debug)]
pub struct PageView<'a> {
    pub section: &'a str,
    pub now_ms: u64,
    pub layout: Layout,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SectionPage {
    pub title: StyledLine,
    pub rows: Vec<StyledLine>,
}

impl Sidebar {
    pub fn page_content(&self, section: &str) -> Option<&SlotContent> {
        self.page(section)
            .and_then(|k| k.content.as_ref())
            .or_else(|| self.panel(section).and_then(|k| k.content.as_ref()))
    }

    fn page_failure(&self, section: &str) -> Option<&Failure> {
        let page = self.page(section);
        if let Some(failure) = page.and_then(|k| k.failure.as_ref()) {
            return Some(failure);
        }
        if page.is_some_and(|k| k.content.is_some()) {
            return None;
        }
        self.panel(section).and_then(|k| k.failure.as_ref())
    }

    pub fn draw_page(&self, view: &PageView<'_>) -> Option<SectionPage> {
        let entry = self.entries.iter().find(|e| e.name == view.section)?;
        let ctx = |width: usize| Ctx {
            layout: Layout {
                width,
                ..view.layout
            },
            glyphs: Glyphs::of(view.layout.glyphs),
            at: Duration::ZERO,
            now_ms: view.now_ms,
            cx: STILL,
            meter_max: PAGE_METER,
            pictures: false,
        };
        let page_width = view.layout.width;
        let content = self.page_content(&entry.name);
        let note = content.map_or(&[][..], |c| &c.note[..]);
        let title_ctx = ctx(page_width);
        let title = blocks::title(&entry.title, &[title_ctx.spans(note)], false, &title_ctx);
        let mut rows = self
            .page_failure(&entry.name)
            .map(|f| failure(f, &ctx(band_cols(page_width))))
            .unwrap_or_default();
        if let Some(content) = content {
            rows.extend(columns(&content.blocks, page_width, &ctx));
        }
        Some(SectionPage { title, rows })
    }
}

fn failure(failure: &Failure, ctx: &Ctx) -> Vec<StyledLine> {
    let sep = format!(" {} ", ctx.glyphs.separator);
    let mut text = copy::couldnt_update(&copy::slot_failure(&failure.failure));
    if failure.parked {
        text.push_str(&sep);
        text.push_str(&copy::stopped_updating(&sep));
    }
    if failure.restarted {
        text.push_str(&copy::plugin_restarted(&sep));
    }
    blocks::wrapped(vec![StyledSpan::new(text, Token::TextMuted)], "", ctx)
}

fn draws(block: &SlotBlock) -> bool {
    match block {
        SlotBlock::Line { spans } => spans.iter().any(|span| match span {
            SlotSpan::Text { text, .. } => !text.is_empty(),
            SlotSpan::Elapsed { .. } | SlotSpan::Usd { .. } | SlotSpan::Fill { .. } => true,
            SlotSpan::Unknown => false,
        }),
        SlotBlock::Rows { rows } => !rows.is_empty(),
        SlotBlock::List { items } => !items.is_empty(),
        SlotBlock::Tree { nodes } => !nodes.is_empty(),
        SlotBlock::Topology { nodes, .. } => !nodes.is_empty(),
        SlotBlock::Gauge { .. }
        | SlotBlock::Spark { .. }
        | SlotBlock::Fill { .. }
        | SlotBlock::Unknown => true,
        SlotBlock::NextColumn => false,
    }
}

pub(super) fn column_width(width: usize, count: usize) -> Option<usize> {
    if count < 2 {
        return None;
    }
    let each = width.saturating_sub(COLUMN_GAP * (count - 1)) / count;
    (each >= MIN_COLUMN).then(|| band_cols(each))
}

fn columns(blocks: &[SlotBlock], width: usize, ctx: &dyn Fn(usize) -> Ctx) -> Vec<StyledLine> {
    let groups: Vec<&[SlotBlock]> = blocks
        .split(|b| matches!(b, SlotBlock::NextColumn))
        .filter(|group| group.iter().any(draws))
        .collect();
    let each = column_width(width, groups.len());
    let one = ctx(each.unwrap_or_else(|| band_cols(width)));
    let drawn: Vec<Vec<StyledLine>> = groups.iter().map(|g| blocks::body(g, &one)).collect();
    let Some(each) = each else {
        return drawn.join(&[one.blank()][..]);
    };
    let height = drawn.iter().map(Vec::len).max().unwrap_or(0);
    (0..height)
        .map(|i| {
            let mut spans = Vec::new();
            for (j, column) in drawn.iter().enumerate() {
                if j > 0 {
                    spans.push(StyledSpan::plain(" ".repeat(COLUMN_GAP)));
                }
                match column.get(i) {
                    Some(line) => spans.extend(line.spans.iter().cloned()),
                    None => spans.push(StyledSpan::plain(" ".repeat(each))),
                }
            }
            StyledLine::new(spans)
        })
        .collect()
}
