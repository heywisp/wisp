use std::hash::{Hash, Hasher};

use unicode_segmentation::UnicodeSegmentation;

use crate::text::{WidthMode, spans_width};
use crate::theme::{Ink, Token};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Look {
    pub fg: Option<Ink>,
    pub bg: Option<Ink>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub strike: bool,
    pub bold_without_colour: bool,
    pub diff_code: bool,
    pub band_text: bool,
    pub glued: bool,
}

impl Look {
    pub fn fg(ink: impl Into<Ink>) -> Look {
        Look {
            fg: Some(ink.into()),
            ..Look::default()
        }
    }

    pub fn on(self, ink: impl Into<Ink>) -> Look {
        Look {
            bg: Some(ink.into()),
            ..self
        }
    }

    pub fn bold(self) -> Look {
        Look { bold: true, ..self }
    }

    pub fn italic(self) -> Look {
        Look {
            italic: true,
            ..self
        }
    }

    pub fn underline(self) -> Look {
        Look {
            underline: true,
            ..self
        }
    }

    pub fn reverse(self) -> Look {
        Look {
            reverse: true,
            ..self
        }
    }

    pub fn bold_without_colour(self) -> Look {
        Look {
            bold_without_colour: true,
            ..self
        }
    }

    pub fn strike(self) -> Look {
        Look {
            strike: true,
            ..self
        }
    }

    pub fn diff_code(self) -> Look {
        Look {
            diff_code: true,
            ..self
        }
    }

    pub fn glued(self) -> Look {
        Look {
            glued: true,
            ..self
        }
    }
}

impl From<Token> for Look {
    fn from(t: Token) -> Look {
        Look::fg(t)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct StyledSpan {
    pub text: String,
    pub look: Look,
}

impl StyledSpan {
    pub fn new(text: impl Into<String>, look: impl Into<Look>) -> StyledSpan {
        StyledSpan {
            text: text.into(),
            look: look.into(),
        }
    }

    pub fn plain(text: impl Into<String>) -> StyledSpan {
        StyledSpan::new(text, Look::default())
    }
}

pub(crate) fn push_merged(spans: &mut Vec<StyledSpan>, span: StyledSpan) {
    match spans.last_mut() {
        Some(last) if last.look == span.look => last.text.push_str(&span.text),
        _ => spans.push(span),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Band {
    pub ink: Ink,
    pub cols: usize,
    pub mode: WidthMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Joined {
    pub space: bool,
    pub lead: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Content {
    pub from: usize,
    pub to: usize,
    pub padding: Option<(usize, usize)>,
    pub skip: Option<(usize, usize)>,
    pub verbatim: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Opens {
    Fold,
    Tool,
    Copy,
}

#[derive(Clone, Debug, Default, Eq)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
    pub band: Option<Band>,
    pub joined: Option<Joined>,
    pub content: Option<Content>,
    pub opens: Option<Opens>,
}

impl PartialEq for StyledLine {
    fn eq(&self, other: &StyledLine) -> bool {
        self.spans == other.spans && self.band == other.band
    }
}

impl Hash for StyledLine {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.spans.hash(state);
        self.band.hash(state);
    }
}

impl StyledLine {
    pub fn new(spans: Vec<StyledSpan>) -> StyledLine {
        StyledLine {
            spans,
            band: None,
            joined: None,
            content: None,
            opens: None,
        }
    }

    pub fn with_fold(self) -> StyledLine {
        self.opening(Opens::Fold)
    }

    pub fn opening(self, opens: Opens) -> StyledLine {
        StyledLine {
            opens: Some(opens),
            ..self
        }
    }

    pub fn holding(self, from: usize, to: usize) -> StyledLine {
        StyledLine {
            content: Some(Content {
                from,
                to,
                ..Content::default()
            }),
            ..self
        }
    }

    pub fn padded(self, from: usize, to: usize) -> StyledLine {
        let content = self.content.map(|c| Content {
            padding: (from < to).then_some((from, to)),
            ..c
        });
        StyledLine { content, ..self }
    }

    pub fn verbatim(self, skip: Option<(usize, usize)>) -> StyledLine {
        let content = self.content.map(|c| Content {
            skip: skip.filter(|(a, b)| a < b),
            verbatim: true,
            ..c
        });
        StyledLine { content, ..self }
    }

    pub fn continuing(self, joined: Option<Joined>) -> StyledLine {
        StyledLine { joined, ..self }
    }

    pub fn led_by(self, prefix: &[StyledSpan]) -> StyledLine {
        let extra: usize = prefix.iter().map(|s| s.text.graphemes(true).count()).sum();
        let joined = self.joined.map(|j| Joined {
            lead: j.lead + extra,
            ..j
        });
        StyledLine { joined, ..self }
    }

    pub fn banded(self, ink: impl Into<Ink>, cols: usize, mode: WidthMode) -> StyledLine {
        let ink = ink.into();
        StyledLine {
            spans: self
                .spans
                .into_iter()
                .map(|s| StyledSpan {
                    look: s.look.on(s.look.bg.unwrap_or(ink)),
                    text: s.text,
                })
                .collect(),
            band: Some(Band { ink, cols, mode }),
            joined: self.joined,
            content: self.content,
            opens: self.opens,
        }
    }

    pub fn band_pad(&self) -> Option<(usize, Look)> {
        let band = self.band?;
        let used: usize = spans_width(&self.spans, band.mode);
        Some((band.cols.saturating_sub(used), Look::default().on(band.ink)))
    }
}
