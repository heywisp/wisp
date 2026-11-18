use wisp_render::styled::StyledLine;
use wisp_render::text::{WidthMode, sanitize, spans_text, width as text_width};
use wisp_render::theme::Depth;

use super::caps::Caps;
use super::scrollback::row_bytes;

pub const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub const fn name(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

pub fn styled(lines: &[StyledLine], caps: &Caps, stream: Stream, width: usize) -> Vec<u8> {
    let depth = caps.depth_on(stream);
    if depth == Depth::None {
        return plain(lines);
    }
    let theme = caps.theme_on(stream);
    let widest = lines
        .iter()
        .map(|line| text_width(&text(line), WidthMode::AmbiguousWide));
    let width = u16::try_from(widest.fold(width, usize::max)).unwrap_or(u16::MAX);
    let mut out = vec![];
    for line in lines {
        let Ok(row) = row_bytes(&theme.line(line), width) else {
            return plain(lines);
        };
        out.extend(row);
        out.extend_from_slice(RESET.as_bytes());
        out.push(b'\n');
    }
    out
}

pub fn inline(line: StyledLine, caps: &Caps, stream: Stream, width: usize) -> Vec<u8> {
    let mut out = styled(&[line], caps, stream, width);
    out.pop();
    out
}

pub(super) fn plain(lines: &[StyledLine]) -> Vec<u8> {
    lines
        .iter()
        .flat_map(|line| {
            let text = wisp_render::chrome::account::unmarked(line);
            format!("{}\n", sanitize(text.trim_end())).into_bytes()
        })
        .collect()
}

pub(crate) fn text(line: &StyledLine) -> String {
    spans_text(&line.spans)
}

#[cfg(test)]
mod tests;
