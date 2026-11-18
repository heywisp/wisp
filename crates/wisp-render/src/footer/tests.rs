use wisp_protocol::{Cost, ErrorKind, StopReason};

use super::*;
use crate::events::{End, Ending, cost, outcome};
use crate::theme::glyphs::{AMBIGUOUS_WIDE, ASCII, UNICODE};

fn usage() -> Usage {
    Usage {
        input_tokens: 4,
        output_tokens: 475,
        cache_read_input_tokens: 21_900,
        cache_creation_input_tokens: 45,
        cache_creation_5m_input_tokens: Some(0),
        cache_creation_1h_input_tokens: Some(45),
        web_search_requests: 0,
        ..Usage::default()
    }
}

fn stats() -> TurnStats {
    TurnStats {
        started_at: "2026-10-21T19:15:02.481Z".into(),
        first_byte_ms: Some(800),
        duration_ms: 2_100,
        retries: 0,
        previous_prompt_tokens: Some(21_900),
    }
}

fn completed() -> TurnOutcome {
    outcome(End {
        model: "claude-opus-5-5".into(),
        usage: Some(usage()),
        cost: Some(cost(0.014, 0.628, false)),
        ..End::default()
    })
}

#[test]
fn footer_reads_as_one_line() {
    assert_eq!(
        footer(&completed(), &stats(), CacheTtl::OneHour, &UNICODE),
        "✓ end_turn · 2.1s (first token 0.8s) · in 4 · out 475 · reused 21.9k (100%) · +45 cached 1h · $0.014 · opus-5-5"
    );
}

#[test]
fn reported_openai_cache_write_buckets_keep_their_actual_lifetimes() {
    let mut outcome = completed();
    if let TurnOutcome::Completed { usage, .. } = &mut outcome {
        *usage = Usage {
            cache_creation_input_tokens: 3_500,
            cache_creation_30m_input_tokens: Some(1_500),
            cache_creation_12h_input_tokens: Some(2_000),
            ..Usage::default()
        };
    }
    for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
        let line = footer(&outcome, &stats(), ttl, &UNICODE);
        assert!(line.contains("+1.5k cached 30m · +2k cached 12h"), "{line}");
        assert!(!line.contains("+3.5k cached"), "{line}");
    }
}

#[test]
fn a_key_reported_as_the_model_is_never_shown() {
    let keyed = outcome(End {
        model: "sk-ant-api03-CANARYMODEL0000".into(),
        usage: Some(usage()),
        cost: Some(cost(0.014, 0.628, false)),
        ..End::default()
    });
    let line = footer(&keyed, &stats(), CacheTtl::OneHour, &UNICODE);
    assert!(line.ends_with(" · (looks like a key, not shown)"), "{line}");
    assert!(!line.contains("CANARY"), "{line}");
}

#[test]
fn a_model_with_a_bidi_control_shows_it_as_a_marker() {
    let outcome = outcome(End {
        model: "claude-opus\u{202E}-5-5".into(),
        usage: Some(usage()),
        cost: Some(cost(0.014, 0.628, false)),
        ..End::default()
    });
    let line = footer(&outcome, &stats(), CacheTtl::OneHour, &UNICODE);
    assert!(line.ends_with(" · opus<U+202E>-5-5"), "{line}");
    assert!(!line.contains('\u{202E}'), "{line}");
}

#[test]
fn reused_is_relative_to_the_previous_prompt() {
    let s = TurnStats {
        previous_prompt_tokens: Some(40_000),
        ..stats()
    };
    assert!(footer(&completed(), &s, CacheTtl::OneHour, &UNICODE).contains("reused 21.9k (55%)"));
    let first = TurnStats {
        previous_prompt_tokens: None,
        ..stats()
    };
    let line = footer(&completed(), &first, CacheTtl::OneHour, &UNICODE);
    assert!(line.contains("reused 21.9k ·"), "{line}");
    let cold = outcome(End {
        model: "m".into(),
        usage: Some(Usage {
            input_tokens: 10,
            cache_creation_input_tokens: 5_000,
            ..Usage::default()
        }),
        committed: false,
        ..End::default()
    });
    let line = footer(&cold, &first, CacheTtl::FiveMinutes, &UNICODE);
    assert!(!line.contains("reused"), "{line}");
    assert!(line.contains("+5k cached 5m"), "{line}");
    assert!(line.contains("cost unknown"), "{line}");
    assert!(line.ends_with("· m · not kept in history"), "{line}");
    let miss = TurnStats {
        previous_prompt_tokens: Some(9_000),
        ..first
    };
    assert!(footer(&cold, &miss, CacheTtl::OneHour, &UNICODE).contains("reused 0 (0%)"));
}

#[test]
fn footer_for_cancelled_and_failed_turns() {
    let s = TurnStats {
        first_byte_ms: None,
        duration_ms: 75_000,
        retries: 2,
        ..stats()
    };
    let cancelled = outcome(End {
        ending: Ending::Cancelled,
        ..End::default()
    });
    assert_eq!(
        footer(&cancelled, &s, CacheTtl::OneHour, &UNICODE),
        "\u{2A2F} cancelled · 1m 15s · 2 retries · cost unknown · dropped from history"
    );
    let failed = outcome(End {
        ending: Ending::Error(ErrorKind::Network),
        message: "reset".into(),
        usage: Some(usage()),
        cost: Some(cost(0.0042, 1.5, true)),
        ..End::default()
    });
    let line = footer(&failed, &s, CacheTtl::OneHour, &UNICODE);
    assert!(line.starts_with("\u{2A2F} failed · 1m 15s"), "{line}");
    assert!(line.contains("$0.0042+ · dropped from history"), "{line}");
}

#[test]
fn numbers_use_k_and_m() {
    for (n, s) in [
        (0, "0"),
        (999, "999"),
        (1_000, "1k"),
        (1_500, "1.5k"),
        (21_900, "21.9k"),
        (99_960, "100k"),
        (150_000, "150k"),
        (999_499, "999k"),
        (999_500, "1M"),
        (999_949, "1M"),
        (999_999, "1M"),
        (2_500_000, "2.5M"),
        (123_456_789, "123M"),
    ] {
        assert_eq!(num(n), s, "{n}");
    }
}

#[test]
fn only_a_kept_finished_reply_gets_the_success_mark() {
    for (stop_reason, committed, mark) in [
        (StopReason::EndTurn, true, "✓ "),
        (StopReason::StopSequence, true, "✓ "),
        (StopReason::EndTurn, false, "\u{25E6} "),
        (StopReason::MaxTokens, true, "\u{25E6} "),
        (StopReason::Refusal, true, "\u{25E6} "),
        (StopReason::ToolUse, false, "\u{25E6} "),
        (StopReason::ContextWindowExceeded, true, "\u{25E6} "),
    ] {
        let outcome = outcome(End {
            stop_reason,
            model: "m".into(),
            usage: Some(usage()),
            committed,
            ..End::default()
        });
        let line = footer(&outcome, &stats(), CacheTtl::OneHour, &UNICODE);
        assert!(line.starts_with(mark), "{line}");
    }
}

fn endings() -> Vec<(&'static str, TurnOutcome, TurnStats)> {
    let slow = TurnStats {
        first_byte_ms: None,
        duration_ms: 75_000,
        retries: 2,
        ..stats()
    };
    let max_tokens = outcome(End {
        stop_reason: StopReason::MaxTokens,
        model: "claude-opus-5-5".into(),
        usage: Some(usage()),
        cost: Some(cost(0.014, 0.628, true)),
        committed: false,
        ..End::default()
    });
    vec![
        ("completed", completed(), stats()),
        ("max tokens, not kept", max_tokens, stats()),
        (
            "cancelled",
            TurnOutcome::Cancelled {
                usage: Some(usage()),
                cost: Some(cost(0.0031, 0.2, true)),
                committed_steps: 0,
            },
            slow.clone(),
        ),
        (
            "failed",
            outcome(End {
                ending: Ending::Error(ErrorKind::Network),
                message: "reset".into(),
                usage: Some(usage()),
                cost: Some(cost(0.0042, 1.5, true)),
                ..End::default()
            }),
            slow,
        ),
    ]
}

#[test]
fn snapshot_ascii_footers() {
    let mut out = String::new();
    for (name, outcome, stats) in endings() {
        let line = footer(&outcome, &stats, CacheTtl::OneHour, &ASCII);
        assert!(line.is_ascii(), "{name}: {line}");
        out.push_str(&format!("{name}: {line}\n"));
    }
    insta::assert_snapshot!("ascii_footers", out);
}

#[test]
fn the_footer_takes_every_glyph_from_its_set() {
    for glyphs in [UNICODE, AMBIGUOUS_WIDE, ASCII] {
        let lines: Vec<String> = endings()
            .iter()
            .map(|(_, outcome, stats)| footer(outcome, stats, CacheTtl::OneHour, &glyphs))
            .collect();
        let sep = format!(" {} ", glyphs.separator);
        let starts = [
            format!("{} end_turn{sep}", glyphs.done),
            format!("{} max_tokens{sep}", glyphs.stopped),
            format!("{} cancelled{sep}", glyphs.failed),
            format!("{} failed{sep}", glyphs.failed),
        ];
        for (line, start) in lines.iter().zip(&starts) {
            assert!(line.starts_with(start.as_str()), "{line}");
        }
        assert!(
            lines[1].contains(&format!("$0.014+{sep}opus-5-5")),
            "{}",
            lines[1]
        );
    }
}

#[test]
fn the_footer_shows_the_turns_own_cost_joined_like_its_other_parts() {
    let with = |c: Cost, glyphs| {
        let mut outcome = completed();
        if let TurnOutcome::Completed { cost, .. } = &mut outcome {
            *cost = Some(c);
        }
        footer(&outcome, &stats(), CacheTtl::OneHour, glyphs)
    };
    let tree = Cost {
        agents_usd: Some(12.4),
        ..cost(0.1, 0.2, false)
    };
    let line = with(tree, &UNICODE);
    assert!(line.ends_with("+45 cached 1h · $0.10 · opus-5-5"), "{line}");
    let mixed = Cost {
        turn_billing: wisp_protocol::Billing::Mixed,
        turn_plan_usd: Some(0.04),
        ..tree
    };
    let line = with(mixed, &ASCII);
    assert!(
        line.contains("- $0.06 - ~$0.04 subscriptions - opus-5-5"),
        "{line}"
    );
    let unknown = Cost {
        turn_plan_usd: None,
        ..mixed
    };
    let line = with(unknown, &ASCII);
    assert!(
        line.contains("- mixed billing - split unavailable - opus"),
        "{line}"
    );
}
