use wisp_protocol::{Fact, ToolAction, ToolOutcome, ToolOutputView};

use super::row::entry_facts;
use super::{Finish, ToolCell, ToolFile, ToolState, clean_lines, first_text_line, refused};
use crate::markdown::Renderer;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::spans_text;
use crate::theme::Token;
use crate::transcript::{Pen, Show, copy, one_line};

impl ToolCell {
    pub(super) fn page_body(
        &self,
        r: &Renderer<'_>,
        pen: &Pen,
        show: Show,
    ) -> (Vec<StyledLine>, usize) {
        let muted = Look::fg(Token::TextMuted);
        let mut out = vec![];
        if let Some(line) = self.name_line(pen) {
            out.extend(Self::long_lines(pen, r, &line, muted));
        }
        out.extend(self.input_lines(r, pen, show));
        if let Some(facts) = self.facts_line(pen) {
            out.extend(Self::long_lines(pen, r, &facts, muted));
        }
        if let Some(d) = self
            .description
            .as_ref()
            .filter(|d| self.title.as_ref() != Some(d))
        {
            out.extend(Self::long_lines(pen, r, d, muted));
        }
        out.extend(self.todo_rows(pen, r));
        if let Some(diff) = &self.diff {
            out.extend(diff.rows(r, pen, false, None));
        }
        if let Some(finish) = &self.finish {
            out.extend(self.notes(finish, r, pen));
        }
        if let Some(path) = self.shown_path() {
            out.extend(Self::long_lines(pen, r, &copy::saved_to(path), muted));
        }
        let output = out.len();
        out.extend(self.output_lines(r, pen));
        if let Some(problem) = self
            .finish
            .as_ref()
            .and_then(|f| f.output.file_problem.as_ref())
        {
            out.extend(Self::long_lines(pen, r, problem, muted));
        }
        if let Some(line) = self.job_line(pen) {
            out.extend(Self::long_lines(pen, r, &line, muted));
        }
        (out, output)
    }

    fn input_lines(&self, r: &Renderer<'_>, pen: &Pen, show: Show) -> Vec<StyledLine> {
        let muted = Look::fg(Token::TextMuted);
        let faint = Look::fg(Token::TextFaint);
        if self.state == ToolState::Preparing {
            let local = show.now.saturating_sub(self.since);
            let line = format!(
                "{}{}{}",
                copy::writing(&self.writing),
                pen.sep(),
                copy::clock(local.as_secs())
            );
            return Self::long_lines(pen, r, &line, muted);
        }
        if let Some(call) = &self.process {
            return call
                .input(self.typed.as_ref(), (&pen.glyphs, pen.mode))
                .iter()
                .flat_map(|line| Self::long_lines(pen, r, line, muted))
                .collect();
        }
        let run = matches!(
            self.display.as_ref().map(|d| &d.action),
            Some(ToolAction::Run)
        );
        let command = self
            .full
            .clone()
            .or_else(|| self.target().map(str::to_owned))
            .unwrap_or_default();
        let mut out = vec![];
        for (i, line) in command.lines().enumerate() {
            let lead = (run && i == 0).then(|| StyledSpan::new(copy::PROMPT, faint));
            out.extend(Self::led_lines(pen, r, lead, line, muted));
        }
        out
    }

    fn name_line(&self, pen: &Pen) -> Option<String> {
        let id = self.process_id()?;
        let mut parts = vec![copy::process_name(id)];
        parts.extend(self.about.clone());
        parts.extend(self.program.clone().filter(|p| !p.trim().is_empty()));
        Some(parts.join(&pen.sep()))
    }

    fn facts_line(&self, pen: &Pen) -> Option<String> {
        let mut words = vec![];
        if let Some(cwd) = &self.cwd {
            words.push(copy::ran_in(cwd));
        }
        match (&self.finish, self.state) {
            (Some(finish), _) => {
                let (outcome, clocked) = self.outcome(finish);
                words.push(outcome);
                if clocked {
                    words.push(Self::clock(finish.duration_ms));
                }
            }
            (None, ToolState::Queued) => words.push(copy::QUEUED_FACT.to_owned()),
            (None, ToolState::NotSent) => words.push(copy::NOT_SENT_FACT.to_owned()),
            (None, ToolState::Abandoned) => {
                words.push(copy::NOT_RUN_FACT.to_owned());
                words.push(copy::TURN_STOPPED.to_owned());
            }
            (None, _) => {}
        }
        (!words.is_empty()).then(|| words.join(&pen.sep()))
    }

    fn outcome(&self, finish: &Finish) -> (String, bool) {
        if self.interrupted {
            return (copy::STOPPED_WHILE_IT_RAN.to_owned(), true);
        }
        if self.withdrawn.is_some() {
            return (copy::COULD_NOT_ATTACH.to_owned(), false);
        }
        if let Some(refusal) = finish.refusal() {
            return (refusal, false);
        }
        let process = self.job_id().or_else(|| {
            finish.facts.iter().find_map(|f| match f {
                Fact::Process { id } => Some(*id),
                _ => None,
            })
        });
        let words = copy::outcome_words(&finish.outcome, finish.duration_ms, process);
        let reason = self
            .message()
            .as_deref()
            .and_then(first_text_line)
            .map(one_line);
        let with = |words: String| match &reason {
            Some(reason) => copy::with_reason(&words, reason),
            None => words,
        };
        match &finish.outcome {
            ToolOutcome::InvalidInput { .. }
            | ToolOutcome::Failed {
                before_start: true, ..
            } => (with(words), false),
            ToolOutcome::Failed { .. } => (with(words), true),
            ToolOutcome::TimedOut { .. }
            | ToolOutcome::Backgrounded { .. }
            | ToolOutcome::NotRun { .. } => (words, false),
            ToolOutcome::Completed { .. } | ToolOutcome::Cancelled => (words, true),
        }
    }

    fn message(&self) -> Option<String> {
        match self.finish.as_ref().map(|f| &f.outcome) {
            Some(ToolOutcome::InvalidInput { message } | ToolOutcome::Failed { message, .. }) => {
                Some(message.clone())
            }
            _ => None,
        }
    }

    fn notes(&self, finish: &Finish, r: &Renderer<'_>, pen: &Pen) -> Vec<StyledLine> {
        let muted = Look::fg(Token::TextMuted);
        let mut out = vec![];
        if let Some(message) = self.message() {
            for line in message.lines().skip_while(|l| l.trim().is_empty()).skip(1) {
                out.extend(Self::long_lines(pen, r, line, muted));
            }
        }
        out.extend(self.result_lines(finish, r, pen));
        if let Some(withdrawn) = &self.withdrawn {
            let line = copy::attachment_not_kept(&withdrawn.reason, withdrawn.others);
            out.extend(Self::long_lines(pen, r, &line, Look::fg(Token::Error)));
        }
        for saved in &finish.saved {
            out.extend(Self::long_lines(pen, r, &copy::saved_copy(saved), muted));
        }
        out
    }

    fn result_lines(&self, finish: &Finish, r: &Renderer<'_>, pen: &Pen) -> Vec<StyledLine> {
        let Some(result) = finish.result.as_deref() else {
            return vec![];
        };
        let refusal = refused(&finish.outcome);
        let invalid = matches!(finish.outcome, ToolOutcome::InvalidInput { .. });
        let look = if refusal {
            Look::fg(Token::Error)
        } else {
            Look::fg(Token::TextMuted)
        };
        result
            .lines()
            .filter(|l| !l.trim().is_empty())
            .skip(usize::from(refusal || invalid))
            .flat_map(|l| Self::long_lines(pen, r, &one_line(l), look))
            .collect()
    }

    fn tail_lines(output: &ToolOutputView) -> Vec<String> {
        let mut lines = clean_lines(&output.tail);
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines
    }

    fn output_lines(&self, r: &Renderer<'_>, pen: &Pen) -> Vec<StyledLine> {
        let muted = Look::fg(Token::TextMuted);
        let faint = Look::fg(Token::TextFaint);
        let text_look = Look::fg(Token::Text);
        let tail = match &self.finish {
            Some(finish) => Self::tail_lines(&finish.output),
            None if self.state == ToolState::Running => self.live.tail(),
            None => vec![],
        };
        let mut out = vec![];
        match self.file.as_deref() {
            Some(ToolFile::Text { text, skipped }) => {
                if *skipped > 0 {
                    let path = self.shown_path().unwrap_or_default();
                    let note = copy::earlier_output_in(pen.glyphs.more, path);
                    out.extend(Self::long_lines(pen, r, &note, muted));
                }
                for line in clean_lines(text) {
                    out.extend(Self::long_lines(pen, r, &line, text_look));
                }
                return out;
            }
            Some(ToolFile::Loading) => {
                let loading = copy::ongoing(copy::LOADING_FILE, &pen.glyphs);
                out.extend(Self::long_lines(pen, r, &loading, faint));
            }
            Some(ToolFile::Gone(reason)) => {
                let gone = copy::file_gone(&one_line(reason));
                out.extend(Self::long_lines(pen, r, &gone, muted));
            }
            None => {
                let total = self
                    .finish
                    .as_ref()
                    .map_or(0, |f| usize::try_from(f.output.lines).unwrap_or(usize::MAX));
                if total > tail.len() && !tail.is_empty() {
                    out.extend(Self::long_lines(pen, r, copy::TAIL_ONLY, muted));
                }
            }
        }
        for line in tail {
            out.extend(Self::long_lines(pen, r, &line, text_look));
        }
        out
    }

    fn job_line(&self, pen: &Pen) -> Option<String> {
        let finish = self.finish.as_ref()?;
        let ToolOutcome::Backgrounded { reason } = finish.outcome else {
            return None;
        };
        let head = self.background_head(reason, finish, pen);
        let (facts, _) = entry_facts(&head.facts, &pen.sep());
        let text = spans_text(&facts);
        (!text.trim().is_empty()).then_some(text)
    }
}
