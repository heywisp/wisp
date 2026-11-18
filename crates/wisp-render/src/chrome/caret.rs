use unicode_segmentation::UnicodeSegmentation;

use crate::markdown::Layout;
use crate::motion::{MotionCtx, caret};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{spans_text, width};
use crate::theme::Ink;

pub fn place_caret(
    live: &mut Vec<StyledLine>,
    prose: Option<usize>,
    core: Ink,
    cx: MotionCtx,
    layout: Layout,
) {
    let Some(rows) = prose.map(|n| n.min(live.len())) else {
        return;
    };
    let mode = layout.width_mode;
    let room = layout.width.saturating_sub(1);
    if let Some(last) = rows.checked_sub(1).and_then(|i| live.get_mut(i)) {
        let before = spans_text(&last.spans);
        if !before.trim().is_empty() {
            let span = caret(&before, core, cx);
            if width(&before, mode) + width(&span.text, mode) <= room {
                last.spans.push(span);
                return;
            }
        }
    }
    let mut spans = vec![];
    if layout.gutter() > 0 {
        spans.push(StyledSpan::plain(" ".repeat(layout.gutter())));
    }
    spans.push(caret("", core, cx));
    live.insert(rows, StyledLine::new(spans));
}

pub(super) fn field(text: &str, at: Option<usize>, look: Look, caret: Look) -> Vec<StyledSpan> {
    let mut spans = vec![StyledSpan::new(text, look)];
    if let Some(at) = at {
        let (before, rest) = text.split_at(at.min(text.len()));
        let mut graphemes = rest.graphemes(true);
        let under = graphemes.next().unwrap_or(" ");
        spans = vec![
            StyledSpan::new(before, look),
            StyledSpan::new(under, caret),
            StyledSpan::new(graphemes.as_str(), look),
        ];
    }
    spans.retain(|span| !span.text.is_empty());
    spans
}
