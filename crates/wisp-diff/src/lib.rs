mod line_diff;
mod lines;
mod model;
mod parse;
mod words;

pub use line_diff::{CONTEXT_LINES, diff_texts};
pub use lines::{Counts, Header, Kind, Lines, Step};
pub use model::{DiffLine, FileDiff, Hunk, NO_NEWLINE, Repairs};
pub use parse::{Parsed, header_path, parse_diff, parse_hunks};
pub use words::{
    Cancelled, MAX_DISTANCE_DEN, MAX_DISTANCE_NUM, MAX_LINE_BYTES, MAX_LINE_TOKENS, MAX_RUN_LINES,
    Pair, RUN_BUDGET, Run, Token, TokenKind, WALK_CANCEL_STEPS, distance_within, tokens,
};
