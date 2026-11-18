use wisp_protocol::{CacheTtl, StopReason, TurnOutcome, TurnStats, Usage};

use crate::theme::glyphs::Glyphs;
use crate::transcript::copy::{self, Fact};

pub fn footer(outcome: &TurnOutcome, stats: &TurnStats, ttl: CacheTtl, glyphs: &Glyphs) -> String {
    let mut parts: Vec<String> = Vec::new();
    match outcome {
        TurnOutcome::Completed {
            stop_reason,
            committed,
            ..
        } => {
            let success =
                *committed && matches!(stop_reason, StopReason::EndTurn | StopReason::StopSequence);
            let mark = if success { glyphs.done } else { glyphs.stopped };
            parts.push(format!("{mark} {}", stop_reason.as_str()));
        }
        TurnOutcome::Cancelled { .. } => parts.push(format!("{} cancelled", glyphs.failed)),
        TurnOutcome::Error { .. } => parts.push(format!("{} failed", glyphs.failed)),
    }
    let mut timing = secs(stats.duration_ms);
    if let Some(first) = stats.first_byte_ms {
        timing.push_str(&format!(" (first token {})", secs(first)));
    }
    parts.push(timing);
    if stats.retries > 0 {
        let noun = if stats.retries == 1 {
            "retry"
        } else {
            "retries"
        };
        parts.push(format!("{} {noun}", stats.retries));
    }
    if let Some(u) = outcome.usage() {
        parts.extend(usage_parts(u, stats.previous_prompt_tokens, ttl));
    }
    match outcome.cost() {
        Some(c) => parts.extend(copy::cost_facts(&c).iter().map(Fact::text)),
        None => parts.push(copy::COST_UNKNOWN.to_owned()),
    }
    match outcome {
        TurnOutcome::Completed {
            model, committed, ..
        } => {
            parts.push(crate::chrome::short_model(model));
            if !committed {
                parts.push(copy::NOT_KEPT.to_owned());
            }
        }
        TurnOutcome::Cancelled { .. } | TurnOutcome::Error { .. } => {
            parts.push("dropped from history".to_owned());
        }
    }
    parts.join(&format!(" {} ", glyphs.separator))
}

fn usage_parts(u: &Usage, previous_prompt: Option<u64>, ttl: CacheTtl) -> Vec<String> {
    let mut parts = vec![
        format!("in {}", num(u.input_tokens)),
        format!("out {}", num(u.output_tokens)),
    ];
    let read = u.cache_read_input_tokens;
    match previous_prompt.filter(|p| *p > 0) {
        Some(previous) => {
            let pct = (read as f64 / previous as f64 * 100.0).min(100.0);
            parts.push(format!("reused {} ({pct:.0}%)", num(read)));
        }
        None if read > 0 => parts.push(format!("reused {}", num(read))),
        None => {}
    }
    let split = [
        (u.cache_creation_5m_input_tokens, "5m"),
        (u.cache_creation_30m_input_tokens, "30m"),
        (u.cache_creation_1h_input_tokens, "1h"),
        (u.cache_creation_12h_input_tokens, "12h"),
    ];
    let known: Vec<String> = split
        .iter()
        .filter_map(|(n, label)| {
            n.filter(|n| *n > 0)
                .map(|n| format!("+{} cached {label}", num(n)))
        })
        .collect();
    if !known.is_empty() {
        parts.extend(known);
    } else if u.cache_creation_input_tokens > 0 {
        parts.push(format!(
            "+{} cached {}",
            num(u.cache_creation_input_tokens),
            ttl.as_str()
        ));
    }
    parts
}

fn secs(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

pub fn num(n: u64) -> String {
    let scaled = |v: f64, unit: &str| {
        if v >= 100.0 {
            format!("{v:.0}{unit}")
        } else {
            let s = format!("{v:.1}");
            format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
        }
    };
    match n {
        0..1_000 => n.to_string(),
        1_000..999_500 => scaled(n as f64 / 1e3, "k"),
        _ => scaled(n as f64 / 1e6, "M"),
    }
}

#[cfg(test)]
mod tests;
