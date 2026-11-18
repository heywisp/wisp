use std::time::Duration;

use super::facts::Facts;
use super::row::{clip, edge};
use super::welcome::detail_lines;
use super::{model_line, one_line};
use crate::markdown::Layout;
use crate::motion::{MotionCtx, OrbFrame, REVEAL, reveal, reveal_next_change};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{
    Long, WidthMode, fit_end, fit_start, spans_width, trim_end, width, wrap_keeping,
};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

pub const WORDMARK: &str = "wisp";
pub(super) const MIN_PATH: usize = 8;

pub fn logo(cx: MotionCtx) -> OrbFrame {
    reveal(REVEAL, cx)
}

pub fn banner_next_change(t: Duration, cx: MotionCtx) -> Option<Duration> {
    reveal_next_change(t, cx)
}

pub(super) fn shorten_path(path: &str, room: usize, more: &str, mode: WidthMode) -> String {
    if width(path, mode) <= room {
        return path.to_owned();
    }
    let more_w = width(more, mode);
    let name = path
        .trim_end_matches('/')
        .rfind('/')
        .and_then(|i| path.get(i..))
        .unwrap_or(path);
    let name_w = width(name, mode);
    if name_w + more_w < room && name.len() < path.len() {
        let front = fit_end(path, room - more_w - name_w, "", mode);
        return format!("{front}{more}{name}");
    }
    fit_start(path, room, more, mode)
}

fn mark_line(facts: &Facts, t: Duration, cx: MotionCtx, layout: Layout) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let sep = format!(" {} ", glyphs.separator);
    let mut spans = vec![StyledSpan::plain(" ")];
    spans.extend(reveal(t, cx).spans());
    spans.push(StyledSpan::plain(" "));
    let words: usize = spans_width(&spans, mode);
    spans.push(StyledSpan::new(WORDMARK, Look::fg(Token::Text).bold()));
    let chip = facts.chip().is_some();
    let mut lead = format!(" {}", one_line(&facts.version));
    if !chip {
        lead.push_str(&sep);
        lead.push_str(&model_line(&facts.model));
    }
    let used: usize = spans_width(&spans, mode) + width(&lead, mode);
    let edge = edge(layout);
    let cwd = one_line(&facts.cwd);
    let room = edge.saturating_sub(used + width(&sep, mode));
    let mut facts_text = lead;
    if !chip && !cwd.is_empty() && room >= MIN_PATH {
        facts_text.push_str(&sep);
        facts_text.push_str(&shorten_path(&cwd, room, glyphs.more, mode));
    }
    spans.push(StyledSpan::new(facts_text, Token::TextMuted));
    trim_end(StyledLine::new(clip(spans, edge, glyphs.more, mode))).holding(words, usize::MAX)
}

fn detail(text: String, layout: Layout) -> Vec<StyledLine> {
    let gutter = layout.gutter();
    wrap_keeping(
        &[StyledSpan::new(text, Token::TextMuted)],
        layout.wide(),
        layout.width_mode,
        Long::Split,
        Glyphs::of(layout.glyphs).separator,
    )
    .into_iter()
    .map(|row| {
        let mut lead = vec![];
        if gutter > 0 {
            lead.push(StyledSpan::plain(" ".repeat(gutter)));
        }
        let mut spans = lead.clone();
        spans.extend(row.spans);
        trim_end(StyledLine::new(spans))
            .continuing(row.joined)
            .led_by(&lead)
    })
    .collect()
}

pub(super) fn model_and_effort(facts: &Facts) -> String {
    [facts.short_model(), one_line(&facts.effort)]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn banner(facts: &Facts, t: Duration, cx: MotionCtx, layout: Layout) -> Vec<StyledLine> {
    banner_with(facts, t, cx, layout, false)
}

pub fn banner_with(
    facts: &Facts,
    t: Duration,
    cx: MotionCtx,
    layout: Layout,
    detail_on: bool,
) -> Vec<StyledLine> {
    let glyphs = Glyphs::of(layout.glyphs);
    let sep = format!(" {} ", glyphs.separator);
    let mut out = vec![mark_line(facts, t, cx, layout)];
    let effort = one_line(&facts.effort);
    let label = facts.credential_label(usize::MAX, &glyphs, layout.width_mode);
    let settings = if let Some(chip) = facts.chip() {
        let gutter = vec![StyledSpan::plain(" ".repeat(layout.gutter()))];
        let mut line = gutter.clone();
        line.extend(chip.spans(
            edge(layout).saturating_sub(layout.gutter()),
            &glyphs,
            layout.width_mode,
        ));
        out.push(trim_end(StyledLine::new(line)).led_by(&gutter));
        let said = model_and_effort(facts);
        let cwd = one_line(&facts.cwd);
        let room = edge(layout).saturating_sub(
            layout.gutter() + width(&said, layout.width_mode) + width(&sep, layout.width_mode),
        );
        if cwd.trim().is_empty() || room < MIN_PATH {
            said
        } else {
            format!(
                "{said}{sep}{}",
                shorten_path(&cwd, room, glyphs.more, layout.width_mode)
            )
        }
    } else if effort.trim().is_empty() {
        label
    } else {
        format!("{}{sep}{label}", copy::effort_named(&effort))
    };
    out.extend(detail(settings, layout));
    if let Some(resumed) = &facts.resumed {
        out.extend(detail(one_line(resumed), layout));
    }
    for line in detail_lines(facts, &glyphs, detail_on) {
        out.extend(detail(line, layout));
    }
    out
}
