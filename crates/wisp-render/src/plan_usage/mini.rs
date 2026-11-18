use wisp_protocol::plan_usage::{UsageScope, UsageState, UsageWindow};
use wisp_protocol::settings::PROVIDER_GEMINI;

use super::{ResetTime, gauge, is_stale, percent, reset, severity, window_names};
use crate::chrome::context::MeterLevel;
use crate::chrome::one_line;
use crate::markdown::Layout;
use crate::styled::{StyledLine, StyledSpan};
use crate::text::{spans_width, wrap};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageHint {
    Show,
    PageOpen,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageCandidate {
    pub line: StyledLine,
    pub measured: bool,
    pub warns: bool,
}

pub fn mini_candidates(
    state: &UsageState,
    model: &str,
    provider: Option<&str>,
    now: i64,
    hint: UsageHint,
    layout: Layout,
) -> Vec<UsageCandidate> {
    let glyphs = Glyphs::of(layout.glyphs);
    let separator = format!(" {} ", glyphs.separator);
    let family_prefix = provider == Some(PROVIDER_GEMINI);
    let mut windows: Vec<_> = state
        .snapshot
        .iter()
        .flat_map(|s| &s.windows)
        .filter(|w| {
            (family_prefix || !matches!(w.scope, UsageScope::ModelFamily(_)))
                && w.scope.applies_to(model)
        })
        .collect();
    windows.sort_by(|a, b| {
        b.used_percent
            .map(|p| p.value())
            .partial_cmp(&a.used_percent.map(|p| p.value()))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.key.cmp(&b.key))
    });
    let suffix = if hint == UsageHint::Show {
        format!("{separator}/usage")
    } else {
        String::new()
    };
    let drawn: Vec<_> = windows
        .iter()
        .copied()
        .filter(|w| w.resets_at.is_none_or(|at| at > now))
        .collect();
    let mut candidates: Vec<UsageCandidate> = vec![];
    for (bars, count, hinted) in [
        (true, drawn.len(), true),
        (false, drawn.len(), true),
        (false, 1, true),
        (false, 1, false),
    ] {
        let mut spans = vec![];
        let mut warns = false;
        for (index, window) in drawn.iter().take(count).enumerate() {
            if !spans.is_empty() {
                spans.push(StyledSpan::plain(&separator));
            }
            let name = window_names(window).map_or(window.label.as_str(), |(short, _)| short);
            let stale = is_stale(state, window, now);
            let warning = match severity(window) {
                _ if stale => "stale ",
                MeterLevel::Over if index == 0 => "!! ",
                MeterLevel::Warning if index == 0 => "! ",
                _ => "",
            };
            warns |= warning.starts_with('!');
            spans.push(StyledSpan::new(
                format!("{warning}{name} "),
                severity(window).look(),
            ));
            if bars
                && index == 0
                && !stale
                && let Some(bar) = gauge(window, 4, severity(window).look(), layout)
            {
                spans.extend(bar);
                spans.push(StyledSpan::plain(" "));
            }
            spans.push(StyledSpan::new(used(window), severity(window).look()));
        }
        if !spans.is_empty() {
            let suffix = if hinted { suffix.as_str() } else { "" };
            spans.push(StyledSpan::new(suffix, Token::TextMuted));
            let line = StyledLine::new(spans);
            if candidates.last().map(|candidate| &candidate.line) != Some(&line) {
                candidates.push(UsageCandidate {
                    line,
                    measured: drawn
                        .iter()
                        .take(count)
                        .any(|window| window.used_percent.is_some()),
                    warns,
                });
            }
        }
    }
    let state_word = if state.loading {
        "refreshing"
    } else if windows.iter().any(|w| is_stale(state, w, now)) {
        "stale"
    } else if windows.iter().any(|w| severity(w) == MeterLevel::Over) {
        "!!"
    } else if windows.iter().any(|w| severity(w) == MeterLevel::Warning) {
        "!"
    } else if state.error.is_some() || windows.is_empty() {
        "unavailable"
    } else {
        ""
    };
    let minimal = if hint == UsageHint::Show {
        format!("{state_word} /usage").trim().to_owned()
    } else {
        state_word.into()
    };
    if !minimal.is_empty() {
        candidates.push(UsageCandidate {
            line: StyledLine::new(vec![StyledSpan::new(minimal, Token::TextMuted)]),
            measured: false,
            warns: state_word.starts_with('!'),
        });
    }
    candidates
}

pub(crate) fn used(window: &UsageWindow) -> String {
    window.used_percent.map_or_else(
        || "usage unknown".into(),
        |p| format!("{} used", percent(p)),
    )
}

pub fn fit_mini(
    candidates: &[UsageCandidate],
    remaining: usize,
    layout: Layout,
) -> Option<StyledLine> {
    candidates
        .iter()
        .find(|line| spans_width(&line.line.spans, layout.width_mode) <= remaining)
        .map(|candidate| candidate.line.clone())
}

pub fn warning_rows(
    endpoint: &str,
    window: &UsageWindow,
    layout: Layout,
    time: impl Fn(i64) -> ResetTime,
) -> Vec<StyledLine> {
    let separator = Glyphs::of(layout.glyphs).separator;
    let measured = used(window);
    let reset = reset(window, |at| time(at).short);
    notice_rows(
        &format!(
            "{} {separator} {} {measured} {separator} {reset}",
            one_line(endpoint),
            window.label
        ),
        &format!("/usage {}", one_line(endpoint)),
        severity(window).look(),
        layout,
    )
}

fn notice_rows(
    title: &str,
    action: &str,
    look: crate::styled::Look,
    layout: Layout,
) -> Vec<StyledLine> {
    let mut rows = wrap(
        &[StyledSpan::new(
            Glyphs::of(layout.glyphs).fold(&one_line(title)),
            look,
        )],
        layout.width.saturating_sub(4).max(1),
        layout.width_mode,
    );
    for (index, row) in rows.iter_mut().enumerate() {
        row.spans.insert(
            0,
            StyledSpan::new(if index == 0 { " ! " } else { "   " }, look),
        );
    }
    rows.extend(
        wrap(
            &[StyledSpan::new(
                Glyphs::of(layout.glyphs).fold(&one_line(action)),
                Token::TextMuted,
            )],
            layout.width.saturating_sub(4).max(1),
            layout.width_mode,
        )
        .into_iter()
        .map(|mut row| {
            row.spans.insert(0, StyledSpan::plain("   "));
            row
        }),
    );
    rows
}
