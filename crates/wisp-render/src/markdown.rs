mod cite;
mod ir;
mod parse;
mod render;

pub(crate) use cite::{block_takes_marks, list_items, parse_contained};
pub use ir::{Align, Block, Child, Inline, Inlines, Item, Marks, Parsed, Table};
#[cfg(test)]
use parse::guarded;
pub use parse::{MAX_ABSOLUTE, MAX_DEPTH, MAX_NESTING, MAX_PREFIX, PLAIN_INDENT, blocks, parse};
pub(crate) use parse::{capped_line, clean, html_ends, html_start};
pub use render::{
    CELL_MIN, CELL_WORD_CAP, COLUMN_GAP, CodeKey, CodeLines, CodeStyle, CutAt, CutError, DiffKind,
    FenceAt, FenceBase, FenceText, GUTTER, LABEL_GAP, Layout, MIN_CONTENT, PlainCode,
    RECORD_INDENT, RenderKey, Renderer, Revision, TINY, WRAP_CELLS_ABOVE, code_lines, diff_hint,
    diff_kind, diff_row, fence_texts, fences, first_difference, is_diff,
};

#[cfg(test)]
mod tests;
