use wisp_protocol::plugin::{CommandOutcome, CommandRefusal, CommandWait};
use wisp_protocol::slot::SlotSpan;
use wisp_protocol::{CommandLine, RequestId};

use super::{Body, FACTS_GAP, Pen, Token, Transcript, copy, facts_at_edge, one_line};
use crate::chrome::clip;
use crate::chrome::sidebar::token;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::soft_lead;
use crate::text::{spans_width, wrap};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Asked {
    name: String,
    cell: Option<usize>,
}

impl Transcript {
    pub fn command_asked(&mut self, request: RequestId, name: &str) {
        self.commands.insert(
            request,
            Asked {
                name: name.to_owned(),
                cell: None,
            },
        );
    }

    pub(super) fn asked_command(&self, request: RequestId) -> bool {
        self.commands.contains_key(&request)
    }

    pub(super) fn command_deferred(&mut self, request: RequestId, until: CommandWait) {
        let Some(asked) = self.commands.get(&request) else {
            return;
        };
        let text = one_line(&copy::command_queued(&asked.name, until));
        let cell = asked.cell;
        let body = Body::Notice(text, Token::TextMuted);
        let cell = self.replace_or_push(cell, body);
        if let Some(asked) = self.commands.get_mut(&request) {
            asked.cell = Some(cell);
        }
    }

    pub(super) fn command_done(&mut self, request: RequestId, outcome: &CommandOutcome) {
        let Some(asked) = self.commands.remove(&request) else {
            return;
        };
        let name = asked.name.as_str();
        let line = match outcome {
            CommandOutcome::Note { text } => Some(Body::Note(Box::new(StyledLine::new(vec![
                StyledSpan::new(one_line(text), Token::TextMuted),
            ])))),
            CommandOutcome::Lines { lines } => {
                let mut lines = lines.clone();
                for line in &mut lines {
                    match line {
                        CommandLine::Row { left, right } => {
                            clean_spans(left);
                            clean_spans(right);
                        }
                        CommandLine::Text { spans } => clean_spans(spans),
                        CommandLine::Unknown => {}
                    }
                }
                Some(Body::Lines(lines))
            }
            CommandOutcome::Failed { message } => Some(Body::Notice(
                one_line(&copy::command_failed(name, message)),
                Token::Text,
            )),
            CommandOutcome::TimedOut => Some(Body::Notice(
                one_line(&copy::command_timed_out(name)),
                Token::TextMuted,
            )),
            CommandOutcome::Cancelled => Some(Body::Notice(
                one_line(&copy::command_cancelled(name)),
                Token::TextMuted,
            )),
            CommandOutcome::Refused { hint, .. } => Some(Body::Notice(
                one_line(&copy::command_refused(name, hint)),
                Token::Text,
            )),
            CommandOutcome::None | CommandOutcome::Sent { .. } | CommandOutcome::Unknown => None,
        };
        let waiting = asked.cell.filter(|cell| *cell < self.cells.len());
        if let Some(cell) = waiting
            && cell + 1 == self.cells.len()
        {
            self.replace_or_push(Some(cell), line.unwrap_or(Body::Discarded));
            return;
        }
        if let Some(cell) = waiting {
            self.replace_or_push(Some(cell), Body::Discarded);
        }
        if let Some(body) = line {
            self.push(None, body, false);
        }
    }

    pub(super) fn command_rejected(&mut self, request: RequestId, reason: &str) {
        let refused = CommandOutcome::Refused {
            kind: CommandRefusal::Other,
            hint: reason.to_owned(),
        };
        self.command_done(request, &refused);
    }

    fn replace_or_push(&mut self, cell: Option<usize>, body: Body) -> usize {
        match cell.filter(|cell| *cell < self.cells.len()) {
            Some(cell) => {
                if let Some(found) = self.cells.get_mut(cell) {
                    found.body = body;
                }
                self.touch(cell);
                cell
            }
            None => self.push(None, body, false),
        }
    }

    pub(super) fn cut_commands(&mut self, at: usize) {
        for asked in self.commands.values_mut() {
            asked.cell = asked.cell.filter(|cell| *cell < at);
        }
    }
}

fn clean_spans(spans: &mut [SlotSpan]) {
    for span in spans {
        if let SlotSpan::Text { text, .. } = span {
            *text = one_line(text);
        }
    }
}

fn spans(spans: &[SlotSpan], pen: &Pen) -> Vec<StyledSpan> {
    spans
        .iter()
        .filter_map(|span| match span {
            SlotSpan::Text { text, tone, bold } => {
                let mut look = Look::fg(token(*tone));
                look.bold = *bold;
                Some(StyledSpan::new(pen.glyphs.fold(text), look))
            }
            _ => None,
        })
        .collect()
}

pub(super) fn rows(lines: &[CommandLine], pen: &Pen) -> Vec<StyledLine> {
    lines
        .iter()
        .flat_map(|line| match line {
            CommandLine::Row { left, right } => {
                let right = clip(
                    spans(right, pen),
                    pen.measure.min(pen.edge.saturating_sub(FACTS_GAP)),
                    pen.glyphs.more,
                    pen.mode,
                );
                let right_w = spans_width(&right, pen.mode);
                let room =
                    pen.measure
                        .saturating_sub(if right_w > 0 { right_w + FACTS_GAP } else { 0 });
                let left = clip(spans(left, pen), room, pen.glyphs.more, pen.mode);
                let mut row = if left.is_empty() {
                    Vec::new()
                } else {
                    pen.indent()
                };
                row.extend(left);
                facts_at_edge(&mut row, right, pen.edge, pen.mode);
                vec![StyledLine::new(row)]
            }
            CommandLine::Text { spans: text } => {
                let mut body = spans(text, pen);
                let mut hang = 0;
                for span in &mut body {
                    let trimmed = span.text.trim_start_matches(' ');
                    hang += span.text.len() - trimmed.len();
                    span.text = trimmed.to_owned();
                    if !span.text.is_empty() {
                        break;
                    }
                }
                let hang = hang.min(pen.measure.saturating_sub(1));

                wrap(&body, pen.measure.saturating_sub(hang), pen.mode)
                    .into_iter()
                    .map(|row| pen.row(soft_lead(pen.gutter + hang, row.joined, &pen.glyphs), row))
                    .collect()
            }
            CommandLine::Unknown => Vec::new(),
        })
        .collect()
}
