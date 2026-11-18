use std::time::Duration;

use wisp_protocol::{AccountSpend, short_id};

use super::agents::{AgentRow, TOTAL, tree_amount};
use super::banner::{WORDMARK, logo};
use super::processes::ProcessRow;
use super::row::clip;
use super::welcome::{SPLASH_FROM, SPLASH_GAP, splash};
use crate::markdown::Layout;
use crate::motion::{MotionCtx, MotionPolicy};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::wrap;
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::theme::{Depth, Token};
use crate::transcript::{Keeps, copy};

const SPENT_USD: f64 = 0.0000005;

pub fn up_arrow(glyphs: &Glyphs) -> &'static str {
    match glyphs.set {
        GlyphSet::Unicode => "\u{2191}",
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => "up",
    }
}

pub fn down_arrow(glyphs: &Glyphs) -> &'static str {
    match glyphs.set {
        GlyphSet::Unicode => "\u{2193}",
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => "v",
    }
}

pub fn up_down(glyphs: &Glyphs) -> String {
    let [up, down] = copy::arrow_keys(glyphs.set);
    match glyphs.set {
        GlyphSet::Unicode => format!("{up}{down}"),
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => format!("{up}/{down}"),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub turns: u32,
    pub stopped: usize,
    pub duration_ms: u64,
    pub total: AccountSpend,
    pub session: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Away {
    Taken,
    Lost,
}

pub fn home_short(path: &str, home: Option<&str>) -> String {
    wisp_protocol::shorten_path(std::path::Path::new(path), home.map(std::path::Path::new))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatTurn {
    Idle,
    Running,
}

pub fn session_total(main: AccountSpend, rows: &[AgentRow]) -> AccountSpend {
    let mut total = main;
    for row in rows {
        total.add(row.entry.spend());
    }
    total
}

pub fn shown_total(main: AccountSpend, rows: &[AgentRow], turn: ChatTurn) -> AccountSpend {
    let mut total = session_total(main, rows);
    total.lower_bound |= turn == ChatTurn::Running || rows.iter().any(AgentRow::is_busy);
    total
}

pub(super) fn spent(total: &AccountSpend) -> bool {
    total.usd >= SPENT_USD
}

pub(super) fn estimated(total: &AccountSpend) -> bool {
    total.plan_usd.is_none_or(|plan| plan >= SPENT_USD)
}

impl Summary {
    pub fn goodbye(&self, layout: Layout) -> Vec<StyledLine> {
        if self.turns == 0 && self.stopped == 0 {
            return vec![];
        }
        let agents = copy::goodbye_agents(self.stopped, self.session.is_some())
            .map(|text| vec![StyledSpan::new(text, Token::TextMuted)]);
        self.block(copy::BYE, [Some(self.time()), agents], layout)
    }

    pub fn leave(&self, layout: Layout) -> Vec<StyledLine> {
        let detail =
            copy::goodbye_agents(0, true).map(|text| vec![StyledSpan::new(text, Token::TextMuted)]);
        self.block(copy::CHAT_STAYS_OPEN, [detail, None], layout)
    }

    pub fn away(&self, away: Away, layout: Layout) -> Vec<StyledLine> {
        let (title, detail) = match away {
            Away::Taken => (copy::CHAT_TAKEN, copy::BRING_CHAT_BACK.to_owned()),
            Away::Lost => (
                copy::CHAT_LOST,
                (self.stopped > 0)
                    .then(|| copy::goodbye_agents(self.stopped, true))
                    .flatten()
                    .unwrap_or_else(|| copy::OPEN_CHAT_AGAIN.to_owned()),
            ),
        };
        self.block(
            title,
            [Some(vec![StyledSpan::new(detail, Token::TextMuted)]), None],
            layout,
        )
    }

    fn block(
        &self,
        title: &str,
        body: [Option<Vec<StyledSpan>>; 2],
        layout: Layout,
    ) -> Vec<StyledLine> {
        let glyphs = Glyphs::of(layout.glyphs);
        let bye = vec![
            StyledSpan::new(WORDMARK, Look::fg(Token::Text).bold()),
            StyledSpan::new(format!(" {} ", glyphs.separator), Token::TextFaint),
            StyledSpan::new(title, Token::Text),
        ];
        let cx = MotionCtx::new(MotionPolicy::Reduced, layout.glyphs, Depth::None);
        let mut lines = if layout.width >= SPLASH_FROM {
            let gap = StyledSpan::plain(" ".repeat(SPLASH_GAP));
            let text = [Some(bye), body[0].clone(), body[1].clone()];
            splash(Duration::ZERO, cx)
                .into_iter()
                .zip(text)
                .map(|(mut art, text)| {
                    if let Some(text) = text {
                        art.push(gap.clone());
                        art.extend(text);
                    }
                    StyledLine::new(art)
                })
                .collect()
        } else {
            let mut mark = logo(cx).spans();
            mark.push(StyledSpan::plain(" "));
            mark.extend(bye);
            let mut lines = vec![StyledLine::new(mark)];
            for text in body.into_iter().flatten() {
                lines.extend(wrap(&text, layout.width, layout.width_mode));
            }
            lines
        };
        if let Some(session) = &self.session {
            lines.push(StyledLine::new(vec![StyledSpan::new(
                format!("{} {}", copy::RESUME_COMMAND, short_id(session)),
                Token::Accent,
            )]));
        }
        lines
    }

    fn time(&self) -> Vec<StyledSpan> {
        let mut spans = vec![
            StyledSpan::new(copy::THAT_WAS, Token::TextMuted),
            StyledSpan::new(copy::finished_duration(self.duration_ms), Token::Text),
        ];
        if spent(&self.total) {
            let marked = tree_amount(&self.total);
            spans.push(StyledSpan::new(
                format!(", {TOTAL} {}", marked.lead),
                Token::TextMuted,
            ));
            spans.push(StyledSpan::new(
                format!("{}{}", marked.amount, marked.tail),
                Token::Text,
            ));
        }
        spans.push(StyledSpan::new(".", Token::TextMuted));
        spans
    }
}

pub fn add_kept(goodbye: &mut Vec<StyledLine>, rows: &[ProcessRow], layout: Layout) {
    let kept = kept_lines(rows, layout);
    if !goodbye.is_empty() && !kept.is_empty() {
        goodbye.push(StyledLine::default());
    }
    goodbye.extend(kept);
}

fn kept_lines(rows: &[ProcessRow], layout: Layout) -> Vec<StyledLine> {
    let more = Glyphs::of(layout.glyphs).more;
    let labels = [
        copy::STOP_IT_LABEL,
        copy::CHECK_IT_LABEL,
        copy::OUTPUT_LABEL,
    ];
    let pad = labels.iter().map(|l| l.len()).max().unwrap_or(0) + 2;
    let labelled = |label: &str, value: String| {
        StyledLine::new(vec![
            StyledSpan::new(format!("  {label:<pad$}"), Token::TextMuted),
            StyledSpan::new(value, Token::Accent),
        ])
    };
    let mut lines = vec![];
    for row in rows.iter().filter(|r| r.running() && r.kept != Keeps::No) {
        let heading = vec![
            StyledSpan::new(copy::kept_heading(row.id, row.kept), Token::TextMuted),
            StyledSpan::new(row.command.clone(), Token::Text),
        ];
        lines.push(StyledLine::new(clip(
            heading,
            layout.width,
            more,
            layout.width_mode,
        )));
        if row.kept == Keeps::Unchecked {
            lines.extend(
                row.pid
                    .map(|pid| labelled(copy::CHECK_IT_LABEL, copy::check_pid(pid))),
            );
            continue;
        }
        lines.extend(
            row.pid
                .map(|pid| labelled(copy::STOP_IT_LABEL, copy::stop_group(pid))),
        );
        lines.extend(row.log.clone().map(|log| labelled(copy::OUTPUT_LABEL, log)));
    }
    lines
}

#[cfg(test)]
mod tests;
