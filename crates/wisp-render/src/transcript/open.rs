use super::{
    Body, Detail, Pen, Thinking, Transcript, copy, one_line, thinking_body, thinking_label,
};
use crate::markdown::{Layout, Renderer};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{spans_padded, spans_text, spans_width};
use crate::theme::Token;

const COLUMN_GAP: usize = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpenTarget {
    #[default]
    Transcript,
    Cell(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Openable {
    pub target: OpenTarget,
    pub line: StyledLine,
    pub filter: String,
    pub live: bool,
}

enum Listed {
    Laid(Entry),
    Line(Openable),
}

struct Entry {
    target: OpenTarget,
    label: Vec<StyledSpan>,
    text: StyledSpan,
    live: bool,
}

impl Thinking {
    fn last_line(&self) -> Option<String> {
        self.text
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(one_line)
    }

    fn entry(&self, pen: &Pen, open: bool) -> Option<(Vec<StyledSpan>, StyledSpan)> {
        let text = self.last_line()?;
        let mut label = vec![
            StyledSpan::new(pen.glyphs.rail, Look::fg(Token::TextFaint)),
            StyledSpan::plain(" "),
        ];
        if open {
            label.extend(self.live_label(pen));
        } else {
            label.push(StyledSpan::new(self.took(), thinking_label()));
        }
        Some((label, StyledSpan::new(text, thinking_body())))
    }
}

impl Transcript {
    pub fn openables(&self, layout: Layout) -> Vec<Openable> {
        let r = Renderer::new(layout);
        let pen = Pen::of(&r);
        let show = self.show(Detail::Scrollback);
        let mut entries = vec![Listed::Laid(Entry {
            target: OpenTarget::Transcript,
            label: vec![StyledSpan::new(copy::FULL_TRANSCRIPT, Token::Text)],
            text: StyledSpan::new(copy::EVERY_CELL, Token::TextMuted),
            live: false,
        })];
        for (index, cell) in self.cells.iter().enumerate().rev() {
            let target = OpenTarget::Cell(index);
            match &cell.body {
                Body::Thinking(t) => {
                    if let Some((label, text)) = t.entry(&pen, cell.open) {
                        entries.push(Listed::Laid(Entry {
                            target,
                            label,
                            text,
                            live: cell.open,
                        }));
                    }
                }
                Body::Mail(mail) => entries.push(Listed::Laid(Entry {
                    target,
                    label: mail.title(&pen),
                    text: StyledSpan::new(
                        one_line(&mail.first_line(&pen.glyphs).unwrap_or_default()),
                        Token::TextMuted,
                    ),
                    live: false,
                })),
                Body::Tool(t) => {
                    let e = t.entry(&pen, show, layout.width);
                    entries.push(Listed::Line(Openable {
                        target,
                        line: e.line,
                        filter: e.filter,
                        live: e.live,
                    }));
                }
                _ => {}
            }
        }
        laid_out(entries, layout)
    }
}

fn laid_out(listed: Vec<Listed>, layout: Layout) -> Vec<Openable> {
    let entries: Vec<&Entry> = listed
        .iter()
        .filter_map(|l| match l {
            Listed::Laid(e) => Some(e),
            Listed::Line(_) => None,
        })
        .collect();
    let label_w = entries
        .iter()
        .map(|e| spans_width(&e.label, layout.width_mode))
        .max()
        .unwrap_or(0);
    let glyphs = crate::theme::glyphs::Glyphs::of(layout.glyphs);
    listed
        .into_iter()
        .map(|l| {
            let e = match l {
                Listed::Laid(e) => e,
                Listed::Line(o) => return o,
            };
            let filter = format!("{} {}", spans_text(&e.label), e.text.text);
            let mut spans = spans_padded(e.label, label_w + COLUMN_GAP, layout.width_mode);
            spans.push(e.text);
            spans.retain(|s| !s.text.is_empty());
            let line = super::tool::clip(
                StyledLine::new(spans),
                layout.width.max(1),
                layout.width_mode,
                glyphs.more,
            );
            Openable {
                target: e.target,
                line: crate::text::trim_end(line),
                filter,
                live: e.live,
            }
        })
        .collect()
}
