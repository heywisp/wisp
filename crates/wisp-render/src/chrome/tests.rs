use std::time::Duration;

use wisp_protocol::{
    AgentEvent, BlockKind, CacheTtl, ErrorKind, NoSearch, SearchForm, StopReason, TurnOutcome,
    TurnStats, Usage,
};

use super::*;
use crate::events::{
    Changed, End, Ending, Finished, Requested, cancelled, cost, ended, outcome, settings_changed,
    tool_finished, turn_ended, usage,
};
use crate::markdown::Layout;
use crate::motion::{MotionCtx, MotionPolicy, Pacing, next_frame};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::testing::{layout, line_text as line, millis as d, width_of};
use crate::text::spans_text;
use crate::theme::glyphs::{GlyphSet, Glyphs, UNICODE};
use crate::theme::{Ink, Token};
use crate::transcript::{UserPart, copy};

const FULL: MotionCtx = MotionCtx {
    policy: MotionPolicy::Full,
    ascii: false,
    blend: true,
};
const CALM: MotionCtx = MotionCtx {
    policy: MotionPolicy::Calm,
    ..FULL
};
const REDUCED: MotionCtx = MotionCtx {
    policy: MotionPolicy::Reduced,
    ..FULL
};
const SIXTEEN: MotionCtx = MotionCtx {
    blend: false,
    ..FULL
};
const ASCII: MotionCtx = MotionCtx {
    ascii: true,
    ..FULL
};

fn completed(stop_reason: StopReason, committed: bool) -> TurnOutcome {
    outcome(End {
        stop_reason,
        model: "claude-sonnet-5-5".to_owned(),
        usage: Some(usage()),
        cost: Some(cost(0.014, 0.21, false)),
        committed,
        ..End::default()
    })
}

fn failed() -> TurnOutcome {
    outcome(End {
        ending: Ending::Error(ErrorKind::Network),
        message: "reset".to_owned(),
        ..End::default()
    })
}

fn text_block() -> AgentEvent {
    AgentEvent::BlockStarted {
        index: 1,
        kind: BlockKind::Text,
    }
}

fn delta() -> AgentEvent {
    AgentEvent::TextDelta {
        index: 1,
        text: "Hi".to_owned(),
    }
}

fn facts() -> Facts {
    Facts {
        version: "0.1.0".to_owned(),
        provider: "Anthropic".to_owned(),
        model: "claude-sonnet-5-5".to_owned(),
        effort: "low".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        credential: Some(Credential::ApiKey),
        host: None,
        log: Some("~/.local/share/wisp/sessions/0193.jsonl".to_owned()),
        home: None,
        cwd: "~/Projects/Wisp".to_owned(),
        endpoint: Endpoint::Official,
        models: vec![known("claude-sonnet-5-5"), known("claude-opus-5-5")],
        on_accounts: Vec::new(),
        write_multipliers: WriteMultipliers {
            prefix: 2.0,
            tail: 2.0,
            five_minutes: 1.25,
            one_hour: 2.0,
        },
        next: None,
        max_tokens: 64_000,
        endpoint_name: None,
        endpoints: Vec::new(),
        endpoint_note: None,
        resume_refused: false,
        instructions: Default::default(),
        search: Default::default(),
        providers_off: Default::default(),
        resumed: None,
        claude_usage: false,
        start_pick: None,
    }
}

fn known(id: &str) -> ModelFacts {
    ModelFacts {
        provider: None,
        shipped: false,
        id: id.to_owned(),
        listed: None,
        rates: None,
        caps: None,
        variant: None,
        image_tier: None,
    }
}

fn say(s: &Status, ms: u64, cx: MotionCtx) -> String {
    spans_text(&s.sentence(d(ms), cx, &UNICODE))
}

fn answered(cx: MotionCtx) -> Status {
    let mut s = Status::new();
    s.submitted(d(0), cx);
    s.apply(&AgentEvent::turn_started(1), d(10), cx);
    s.apply(&text_block(), d(800), cx);
    s.apply(&delta(), d(820), cx);
    s
}

#[test]
fn events_and_inputs_drive_the_m3_states() {
    let cx = FULL;
    let mut s = Status::new();
    assert_eq!(s.state(d(0), cx), State::Ready);
    s.composer(false, d(100), cx);
    assert_eq!(s.state(d(100), cx), State::Listening);
    s.composer(true, d(200), cx);
    assert_eq!(s.state(d(200), cx), State::Ready);
    s.submitted(d(1000), cx);
    assert_eq!(s.state(d(1000), cx), State::Thinking);
    s.apply(&AgentEvent::turn_started(1), d(1010), cx);
    s.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        d(1100),
        cx,
    );
    s.apply(
        &AgentEvent::ThinkingDelta {
            index: 0,
            text: "hm".to_owned(),
        },
        d(1200),
        cx,
    );
    assert_eq!(s.state(d(1200), cx), State::Thinking);
    s.apply(&text_block(), d(1300), cx);
    assert_eq!(s.state(d(1300), cx), State::Answering);
    s.apply(
        &AgentEvent::BlockStarted {
            index: 2,
            kind: BlockKind::ToolUse {
                writing: Some("a command".to_owned()),
                name: "shell".to_owned(),
            },
        },
        d(1400),
        cx,
    );
    assert_eq!(s.state(d(1400), cx), State::Working);
    assert_eq!(say(&s, 1400, cx), "Writing a command \u{b7} 0s");
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 400),
        d(1500),
        cx,
    );
    assert_eq!(s.state(d(1500), cx), State::Done);
    assert_eq!(s.state(d(3699), cx), State::Done);
    assert_eq!(s.state(d(3700), cx), State::Ready);
    assert!(!s.is_running());
}

#[test]
fn composer_text_while_running_never_moves_the_orb() {
    let mut s = answered(FULL);
    s.composer(false, d(900), FULL);
    assert_eq!(s.state(d(900), FULL), State::Answering);
    s.composer(true, d(950), FULL);
    assert_eq!(s.state(d(950), FULL), State::Answering);
}

#[test]
fn thinking_shows_nothing_for_300_ms_then_the_verb_then_its_facts_from_1_s() {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    assert_eq!(say(&s, 0, FULL), "");
    assert_eq!(say(&s, 299, FULL), "");
    assert_eq!(say(&s, 300, FULL), "Kindling\u{2026}");
    assert_eq!(say(&s, 999, FULL), "Kindling\u{2026}");
    assert_eq!(say(&s, 1000, FULL), "Kindling\u{2026} 1s \u{00B7} thinking");
    assert_eq!(say(&s, 4500, FULL), "Kindling\u{2026} 4s \u{00B7} thinking");
    assert_eq!(
        say(&s, 75_000, FULL),
        "Kindling\u{2026} 1m 15s \u{00B7} thinking"
    );
    s.turn_effort(Some("xhigh"));
    s.apply(&AgentEvent::turn_started(1), d(4600), FULL);
    assert_eq!(
        say(&s, 5000, FULL),
        "Kindling\u{2026} 5s \u{00B7} thinking with xhigh effort"
    );
    s.apply(
        &AgentEvent::ThinkingDelta {
            index: 0,
            text: "x".repeat(7_600),
        },
        d(25_000),
        FULL,
    );
    assert_eq!(
        say(&s, 25_400, FULL),
        "Kindling\u{2026} 25s \u{00B7} thinking with xhigh effort"
    );
    let spans = s.sentence(d(5000), SIXTEEN, &UNICODE);
    assert_eq!(spans[0].look.fg, Some(Ink::Token(Token::Thinking)));
    assert_eq!(spans[1].look.fg, Some(Ink::Token(Token::TextMuted)));
}

#[test]
fn the_working_line_rounds_down_to_whole_seconds() {
    let mut s = Status::new();
    s.submitted(d(0), SIXTEEN);
    for (ms, time) in [
        (1_000, "1s"),
        (1_999, "1s"),
        (9_950, "9s"),
        (59_999, "59s"),
        (60_000, "1m 00s"),
        (65_500, "1m 05s"),
        (3_600_000, "1h 00m"),
    ] {
        assert_eq!(
            say(&s, ms, SIXTEEN),
            format!("Kindling\u{2026} {time} \u{00B7} thinking"),
            "{ms}"
        );
    }
}

#[test]
fn the_verb_and_its_facts_appear_on_the_thresholds() {
    for cx in [FULL, CALM, REDUCED, SIXTEEN, ASCII] {
        let mut s = Status::new();
        s.submitted(d(0), cx);
        let glyphs = Glyphs::of(if cx.ascii {
            GlyphSet::Ascii
        } else {
            GlyphSet::Unicode
        });
        let at = |ms: u64| spans_text(&s.sentence(d(ms), cx, &glyphs));
        assert_eq!(at(299), "");
        assert!(at(300).starts_with("Kindling"), "{cx:?}");
        assert!(!at(999).contains('('), "{cx:?}");
        assert!(at(1000).contains(" 1s"), "{cx:?}");
        let mut times = vec![];
        let mut t = d(0);
        while let Some(n) = s.next_change(t, cx) {
            if n > d(1000) {
                break;
            }
            if at(n.as_millis() as u64) != at(t.as_millis() as u64) {
                times.push(n.as_millis() as u64);
            }
            t = n;
        }
        assert_eq!(times, [300, 1000], "{cx:?}");
    }
}

#[test]
fn the_verb_is_fixed_by_the_seed_and_changes_per_turn() {
    let verbs = |seed: u64| {
        let mut s = Status::new().with_seed(seed);
        let mut out = vec![];
        for turn in 0..3u64 {
            let at = turn * 10_000;
            s.submitted(d(at), FULL);
            out.push(s.verb());
            s.apply(&AgentEvent::turn_started(1), d(at + 10), FULL);
            s.apply(
                &ended(completed(StopReason::EndTurn, true), 100),
                d(at + 100),
                FULL,
            );
        }
        out
    };
    assert_eq!(verbs(0), ["Kindling", "Drifting", "Glimmering"]);
    assert_eq!(verbs(7), verbs(7));
    assert_eq!(verbs(7), ["Weaving", "Gathering", "Musing"]);
    assert_eq!(verbs(19)[1], "Kindling");
    assert_eq!(verbs(u64::MAX).len(), 3);
    for seed in 0..40 {
        let v = verbs(seed);
        assert_ne!(v[0], v[1], "{seed}");
        assert_ne!(v[1], v[2], "{seed}");
    }
}

#[test]
fn every_verb_is_calm_and_never_a_phase_word() {
    let brand_banned = [
        "agentic",
        "llm",
        "seamless",
        "revolutionary",
        "magic",
        "supercharge",
        "unleash",
        "effortless",
        "100%",
        "never",
        "successfully",
        "oops",
        "sorry",
    ];
    let phase_words = [
        "ready",
        "thinking",
        "answering",
        "working",
        "done",
        "stopped",
        "running",
        "retrying",
        "preparing",
        "listening",
        "failed",
        "error",
    ];
    assert_eq!(copy::VERBS.len(), 20);
    let unique: std::collections::BTreeSet<&str> = copy::VERBS.iter().copied().collect();
    assert_eq!(unique.len(), copy::VERBS.len());
    for verb in copy::VERBS {
        let lower = verb.to_lowercase();
        for banned in brand_banned {
            assert!(!lower.contains(banned), "{verb} contains {banned}");
        }
        assert!(!phase_words.contains(&lower.as_str()), "{verb}");
        assert!(verb.is_ascii() && !verb.contains('!'), "{verb}");
        assert!(
            verb.chars().next().is_some_and(char::is_uppercase),
            "{verb}"
        );
        assert!(verb.ends_with("ing"), "{verb}");
    }
}

#[test]
fn the_estimate_counts_streamed_characters_and_done_shows_the_exact_count() {
    let mut s = Status::new();
    s.submitted(d(0), SIXTEEN);
    s.apply(&AgentEvent::turn_started(1), d(10), SIXTEEN);
    assert_eq!(s.estimated_tokens(), 0);
    s.apply(
        &AgentEvent::ThinkingDelta {
            index: 0,
            text: "abcde".to_owned(),
        },
        d(20),
        SIXTEEN,
    );
    assert_eq!(s.estimated_tokens(), 0);
    s.apply(&text_block(), d(30), SIXTEEN);
    s.apply(
        &AgentEvent::TextDelta {
            index: 1,
            text: "\u{00E9}t\u{00E9}".to_owned(),
        },
        d(40),
        SIXTEEN,
    );
    assert_eq!(s.estimated_tokens(), 1);
    assert_eq!(
        say(&s, 3000, SIXTEEN),
        "Kindling\u{2026} 3s \u{00B7} \u{2193} \u{2248}1 token \u{00B7} \u{2248}0 tok/s \u{00B7} esc to stop"
    );
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 18_000),
        d(3000),
        SIXTEEN,
    );
    assert_eq!(
        say(&s, 3000, SIXTEEN),
        "Done \u{00B7} 18s \u{00B7} \u{2193} 475 tokens \u{00B7} cached 92%"
    );
    assert_eq!(s.estimated_tokens(), 0);
    s.submitted(d(10_000), SIXTEEN);
    assert_eq!(s.estimated_tokens(), 0);
    let ascii = Glyphs::of(GlyphSet::Ascii);
    s.apply(
        &AgentEvent::TextDelta {
            index: 0,
            text: "x".repeat(4_001),
        },
        d(10_010),
        ASCII,
    );
    assert_eq!(
        spans_text(&s.sentence(d(11_000), ASCII, &ascii)),
        "Drifting... 1s - v ~1k tokens - ~1k tok/s - esc to stop"
    );
}

#[test]
fn no_failure_shows_a_verb_or_done() {
    let verbs_or_done =
        |text: &str| text.contains("Done") || copy::VERBS.iter().any(|v| text.contains(v));
    for cx in [FULL, SIXTEEN, ASCII] {
        for outcome in [
            cancelled(),
            failed(),
            TurnOutcome::Cancelled {
                usage: Some(usage()),
                cost: Some(cost(0.01, 0.02, false)),
                committed_steps: 2,
            },
            outcome(End {
                ending: Ending::Error(ErrorKind::Auth),
                message: "no".to_owned(),
                usage: Some(usage()),
                committed_steps: 1,
                ..End::default()
            }),
            completed(StopReason::MaxTokens, true),
            completed(StopReason::Refusal, true),
            completed(StopReason::EndTurn, false),
            completed(StopReason::PauseTurn, true),
        ] {
            for typed in [false, true] {
                let mut s = answered(cx);
                if typed {
                    s.composer(false, d(900), cx);
                }
                s.apply(&ended(outcome.clone(), 5_000), d(5_000), cx);
                for t in [5_000, 5_299, 6_000, 8_000, 60_000] {
                    let said = say(&s, t, cx);
                    assert!(!verbs_or_done(&said), "{outcome:?} at {t}: {said}");
                    for w in [40, 80, 120] {
                        let row = line(&s.row(d(t), cx, &facts(), layout(w)));
                        assert!(!verbs_or_done(&row), "{outcome:?} at {t}: {row}");
                    }
                }
            }
        }
        let mut rejected = Status::new();
        rejected.submitted(d(0), cx);
        rejected.stop_awaiting(d(1500), cx);
        assert!(!verbs_or_done(&say(&rejected, 2000, cx)));
    }
}

#[test]
fn a_narrow_row_drops_effort_then_tokens_then_time() {
    let mut s = Status::new();
    s.turn_effort(Some("xhigh"));
    s.submitted(d(0), SIXTEEN);
    s.apply(&AgentEvent::turn_started(1), d(10), SIXTEEN);
    s.apply(&long_reply(), d(20), SIXTEEN);
    s.apply(&thinking_block(), d(25), SIXTEEN);
    let at = |w: usize| line(&s.row(d(25_000), SIXTEEN, &facts(), layout(w)));
    let wide = at(120);
    assert!(
        wide.contains("Kindling\u{2026} 25s \u{00B7} \u{2193} \u{2248}1.9k tokens \u{00B7} thinking with xhigh effort"),
        "{wide}"
    );
    let mut seen = vec![];
    let mut narrow = vec![];
    for w in (12..=120).rev() {
        let row = at(w);
        let seen = if w < STOP_ONLY_BELOW {
            &mut narrow
        } else {
            &mut seen
        };
        let kept = if row.contains("thinking with") {
            "all"
        } else if row.contains("tokens") {
            "tokens"
        } else if row.contains("25s") {
            "time"
        } else if row.contains("Kindling") {
            "verb"
        } else {
            "clipped"
        };
        if seen.last() != Some(&kept) {
            seen.push(kept);
        }
        assert!(!row.contains("(25s \u{00B7} thinking"), "{row}");
        assert!(row.chars().count() <= w, "{w}: {row}");
    }
    assert_eq!(seen, ["all", "tokens", "time"]);
    assert_eq!(narrow, ["tokens", "time", "verb", "clipped"]);
    let mut a = answered(SIXTEEN);
    a.apply(&delta(), d(1500), SIXTEEN);
    let row = line(&a.row(d(2000), SIXTEEN, &facts(), layout(79)));
    assert!(row.contains("\u{b7} esc to stop"), "{row}");
    assert_eq!(row.matches("esc to stop").count(), 1, "{row}");
    let row = line(&a.row(d(2000), SIXTEEN, &facts(), layout(30)));
    assert!(!row.contains("\u{b7} esc to stop"), "{row}");
    let mut reply = Status::new();
    reply.submitted(d(0), SIXTEEN);
    reply.apply(&AgentEvent::turn_started(1), d(10), SIXTEEN);
    reply.apply(&long_reply(), d(24_000), SIXTEEN);
    let (mut kept, mut narrow) = (vec![], vec![]);
    for w in (12..=120).rev() {
        let row = line(&reply.row(d(25_000), SIXTEEN, &facts(), layout(w)));
        let kept = if w < STOP_ONLY_BELOW {
            &mut narrow
        } else {
            &mut kept
        };
        let (counter, clock, speed) = (
            row.contains("tokens"),
            row.contains("25s"),
            row.contains("\u{2248}1.9k tok/s"),
        );
        assert!(!counter || clock, "{w}: {row}");
        let now = if row.contains("\u{b7} esc to stop") {
            "all"
        } else if counter {
            "tokens"
        } else if clock {
            "time"
        } else if speed {
            "speed"
        } else {
            "verb"
        };
        if kept.last() != Some(&now) {
            kept.push(now);
        }
    }
    assert_eq!(kept, ["all", "time", "speed"]);
    assert_eq!(narrow, ["time", "speed", "time", "verb"]);
}

fn long_turn(seed: u64, answering: bool) -> Status {
    let mut s = Status::new().with_seed(seed);
    s.turn_effort(Some("xhigh"));
    s.submitted(d(0), SIXTEEN);
    s.apply(&AgentEvent::turn_started(1), d(10), SIXTEEN);
    s.apply(&long_reply(), d(20), SIXTEEN);
    if !answering {
        s.apply(&thinking_block(), d(30), SIXTEEN);
    }
    s
}

fn long_reply() -> AgentEvent {
    AgentEvent::TextDelta {
        index: 0,
        text: "x".repeat(7_600),
    }
}

fn thinking_block() -> AgentEvent {
    AgentEvent::BlockStarted {
        index: 1,
        kind: BlockKind::Thinking,
    }
}

#[test]
fn esc_to_stop_shows_at_every_width_under_80_while_running() {
    let esc = &copy::esc_to_stop().text();
    for seed in [0, 16] {
        for answering in [false, true] {
            let s = long_turn(seed, answering);
            let verb = s.verb();
            assert_eq!(
                say(&s, 65_000, SIXTEEN).contains(esc),
                answering,
                "{seed} {answering}"
            );
            for w in 18..=79 {
                for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
                    let cx = MotionCtx {
                        ascii: set == GlyphSet::Ascii,
                        ..SIXTEEN
                    };
                    let row = line(&s.row(d(65_000), cx, &facts(), Layout::new(w, set)));
                    assert_eq!(row.matches(esc).count(), 1, "{seed} {answering} {w}: {row}");
                    assert!(row.chars().count() <= w, "{w}: {row}");
                    let whole = verb.len() + Glyphs::of(set).more.chars().count() + 21;
                    assert_eq!(
                        row.contains(verb),
                        w >= whole,
                        "{seed} {answering} {w}: {row}"
                    );
                    if !row.contains(&format!("{} {esc}", Glyphs::of(set).separator))
                        && w < STOP_ONLY_BELOW
                    {
                        assert!(row.trim_end().ends_with(esc), "{w}: {row}");
                        assert!(!row.contains("sonnet"), "{w}: {row}");
                    }
                    if !row.contains(&format!("{} {esc}", Glyphs::of(set).separator))
                        && w >= STOP_ONLY_BELOW
                    {
                        let tail = row.trim_end();
                        let after = tail.split_once(esc).map_or("", |(_, after)| after);
                        assert!(
                            after.is_empty()
                                || (after.starts_with("   ")
                                    && after.trim_start().starts_with("Claude API")),
                            "{w}: {row}"
                        );
                    }
                    if w <= 24 {
                        let after_orb: String = row.chars().skip(4).collect();
                        assert!(after_orb.starts_with("  "), "{w}: {row}");
                        assert!(!after_orb.starts_with("   "), "{w}: {row}");
                        assert!(!row.contains(&format!("    {esc}")), "{w}: {row}");
                        assert!(
                            !after_orb.starts_with(&format!("  {}", Glyphs::of(set).more)),
                            "{w}: {row}"
                        );
                    }
                }
            }
            for w in 1..18 {
                let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
                assert!(!row.contains("esc"), "{w}: {row}");
                assert!(row.chars().count() <= w.max(1), "{w}: {row}");
            }
        }
    }
    let s = long_turn(16, true);
    for w in [18, 19, 20, 21] {
        let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
        assert_eq!(
            row.trim_end(),
            " \u{288E}\u{2836}\u{2870}  esc to stop",
            "{w}"
        );
    }
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(30)));
    assert_eq!(
        row.trim_end(),
        " \u{288E}\u{2836}\u{2870}  Lantern-\u{2026}   esc to stop"
    );
    let s = long_turn(16, false);
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(79)));
    assert!(
        row.contains(&format!(
            "Lantern-lighting\u{2026} 1m 05s \u{00B7} \u{2193} \u{2248}1.9k tokens   {esc}"
        )),
        "{row}"
    );
}

fn sentence_first(s: &Status, t: u64, w: usize) -> String {
    let l = layout(w);
    let glyphs = Glyphs::of(l.glyphs);
    let stop = if s.is_running() {
        vec![copy::esc_to_stop()]
    } else {
        vec![]
    };
    let facts = if s.is_running() && w < STOP_ONLY_BELOW {
        vec![]
    } else {
        s.tally().facts(&facts(), &glyphs, &[], None)
    };
    line(&status_row(
        &s.orb(d(t), SIXTEEN),
        s.sentence(d(t), SIXTEEN, &glyphs),
        &stop,
        &facts,
        l,
    ))
}

#[test]
fn only_the_working_line_gives_way_to_esc_to_stop() {
    let mut retry = long_turn(0, false);
    retry.apply(
        &AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 8000,
            reason: "overloaded".to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        d(60_000),
        SIXTEEN,
    );
    let mut quit = long_turn(0, true);
    quit.hint("Press ctrl-c again to quit", d(60_000), Some(d(10_000)));
    let mut busy = long_turn(0, true);
    busy.hint(
        "Wisp is still answering. Send this after, or press esc to stop it.",
        d(60_000),
        None,
    );
    let mut preparing = long_turn(0, true);
    preparing.apply(&tool(3), d(60_000), SIXTEEN);
    let mut working = long_turn(0, true);
    working.apply(&tool(3), d(1_000), SIXTEEN);
    working.apply(
        &tool_requested("c", "cargo test -p wisp-core"),
        d(1_100),
        SIXTEEN,
    );
    name_call_from_transcript(
        &mut working,
        &tool_requested("c", "cargo test -p wisp-core"),
    );
    working.apply(&AgentEvent::call_started("c", "~"), d(13_000), SIXTEEN);
    let cases = [
        ("retry", retry, "Retrying in 3 s"),
        ("quit", quit, "to quit"),
        ("busy", busy, "Wisp is still answering"),
        ("preparing", preparing, "Writing a x call"),
        (
            "working",
            working,
            "Running cargo test -p wisp-core \u{b7} 52s",
        ),
    ];
    for (name, s, said) in &cases {
        assert!(say(s, 65_000, SIXTEEN).contains(said), "{name}");
        assert!(s.is_running(), "{name}");
        for w in 30..=79 {
            let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
            if row == sentence_first(s, 65_000, w) {
                continue;
            }
            let sentence = say(s, 65_000, SIXTEEN);
            let kept = row
                .split_once("   esc to stop")
                .map(|(before, _)| before.chars().skip(6).collect::<String>())
                .unwrap_or_else(|| panic!("{name} at {w}: {row}"));
            let mut words = sentence.split(' ');
            for word in kept.replace('\u{2026}', " ").split_whitespace() {
                let lone = kept.starts_with(&format!("{word}\u{2026}"));
                assert!(words.any(|w| w == word || lone), "{name} at {w}: {row}");
            }
        }
    }
    let at = |s: &Status, w: usize| line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
    assert!(
        at(&cases[0].1, 59).contains("Retrying in 3 s"),
        "{}",
        at(&cases[0].1, 59)
    );
    assert!(
        at(&cases[1].1, 50).contains("to quit"),
        "{}",
        at(&cases[1].1, 50)
    );
    assert!(
        at(&cases[1].1, 40).contains("to quit"),
        "{}",
        at(&cases[1].1, 40)
    );
    assert!(
        at(&cases[4].1, 50).contains("52s"),
        "{}",
        at(&cases[4].1, 50)
    );
    let thinking = long_turn(0, false);
    for w in 30..=79 {
        assert!(at(&thinking, w).contains("esc to stop"), "{w}");
    }
    assert_ne!(at(&thinking, 50), sentence_first(&thinking, 65_000, 50));
}

#[test]
fn the_shimmer_sweeps_on_a_100_ms_grid_in_full_motion_only() {
    let word = "Kindling\u{2026}";
    let looks = |local: u64, cx: MotionCtx| -> Vec<Option<Ink>> {
        shimmer(word, Token::Thinking, d(local), cx)
            .iter()
            .flat_map(|s| s.text.chars().map(|_| s.look.fg).collect::<Vec<_>>())
            .collect()
    };
    assert_eq!(looks(0, FULL).len(), 9);
    assert_eq!(looks(0, FULL), looks(99, FULL));
    assert_ne!(looks(100, FULL), looks(99, FULL));
    assert_eq!(looks(150, FULL), looks(2150, FULL));
    let mut distinct = std::collections::BTreeSet::new();
    for step in 0..20u64 {
        distinct.insert(format!("{:?}", looks(step * 100, FULL)));
        for ink in looks(step * 100, FULL).into_iter().flatten() {
            match ink {
                Ink::Token(t) => assert_eq!(t, Token::Thinking),
                Ink::Mix { u, still, .. } => {
                    assert_eq!(still, Token::Thinking);
                    assert!(u.get() <= 0.4 + 1e-9, "{u:?}");
                }
            }
        }
    }
    assert_eq!(distinct.len(), 14);
    let rest = looks(0, FULL);
    assert!(rest.iter().all(|i| *i == Some(Ink::Token(Token::Thinking))));
    for local in [1500, 1700, 1999, 2000] {
        assert_eq!(looks(local, FULL), rest, "{local}");
    }
    for cx in [CALM, REDUCED, SIXTEEN, ASCII] {
        assert!(!shimmers(cx));
        assert_eq!(
            shimmer(word, Token::Text, d(700), cx),
            vec![StyledSpan::new(word, Token::Text)]
        );
    }
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    let mut times = vec![];
    let mut t = d(300);
    while let Some(n) = s.next_change(t, FULL) {
        if n > d(2000) {
            break;
        }
        times.push(n.as_millis() as u64);
        t = n;
    }
    assert!(times.iter().all(|t| t % 100 == 0), "{times:?}");
    assert_eq!(
        times,
        [
            400, 500, 600, 700, 800, 900, 1000, 1100, 1200, 1300, 1400, 1500, 1700, 1800, 1900,
            2000
        ]
    );
    let mut still = Status::new();
    still.submitted(d(0), REDUCED);
    assert_eq!(still.next_change(d(300), REDUCED), Some(d(1000)));
    assert_eq!(still.next_change(d(1000), REDUCED), Some(d(2000)));
}

#[test]
fn a_busy_retry_says_how_long_it_has_waited() {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    s.apply(
        &AgentEvent::Retrying {
            attempt: 14,
            delay_ms: 30_000,
            reason: "HTTP 529: Overloaded".to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 0,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: Some(wisp_protocol::BusyWait {
                waited_ms: 241_000,
                window_ms: 600_000,
            }),
        },
        d(1000),
        FULL,
    );
    assert_eq!(
        say(&s, 1000, FULL),
        "Retrying in 30 s (attempt 14): overloaded \u{00B7} waited 4 min"
    );
}

#[test]
fn a_queued_request_keeps_the_row_it_would_show_without_the_queue() {
    let mut plain = Status::new();
    plain.submitted(d(0), FULL);
    plain.apply(&AgentEvent::turn_started(1), d(5), FULL);
    plain.apply(&AgentEvent::StepStarted { step: 1 }, d(6), FULL);
    let mut queued = plain.clone();
    for (ms, ahead) in [(10, 2), (2000, 0)] {
        queued.apply(
            &AgentEvent::RequestQueued {
                step: 1,
                endpoint: "env".to_owned(),
                ahead,
            },
            d(ms),
            FULL,
        );
    }
    let same = |queued: &Status, at: u64| {
        for w in [40, 80] {
            let row = |s: &Status| line(&s.row(d(at), FULL, &facts(), layout(w)));
            assert_eq!(row(queued), row(&plain), "{at} {w}");
        }
    };
    same(&queued, 1000);
    same(&queued, 2500);
    queued.apply(
        &AgentEvent::RequestAdmitted {
            step: 1,
            waited_ms: 3000,
        },
        d(3000),
        FULL,
    );
    same(&queued, 3000);
}

#[test]
fn a_retry_counts_down_then_returns_to_the_timer() {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    s.apply(&AgentEvent::StepStarted { step: 1 }, d(10), FULL);
    s.apply(
        &AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 8000,
            reason: "overloaded".to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        d(1000),
        FULL,
    );
    assert_eq!(s.state(d(1000), FULL), State::Thinking);
    let retry = |n: u64| format!("Retrying in {n} s (1 of 5): overloaded");
    assert_eq!(say(&s, 1000, FULL), retry(8));
    assert_eq!(say(&s, 1999, FULL), retry(8));
    assert_eq!(say(&s, 2000, FULL), retry(7));
    assert_eq!(say(&s, 8999, FULL), retry(1));
    assert_eq!(say(&s, 9000, FULL), "Kindling\u{2026} 9s \u{00B7} thinking");
    let mut t = d(1000);
    let mut ticks = vec![];
    while t < d(9000) {
        let Some(n) = s.next_change(t, FULL) else {
            break;
        };
        if say(&s, n.as_millis() as u64, FULL) != say(&s, t.as_millis() as u64, FULL) {
            ticks.push(n.as_millis() as u64);
        }
        t = n;
    }
    assert_eq!(
        ticks,
        [2000, 3000, 4000, 5000, 6000, 7000, 8000, 9000],
        "the countdown ticks once a second"
    );
    s.apply(&text_block(), d(9100), FULL);
    s.apply(&delta(), d(13_100), FULL);
    s.apply(
        &AgentEvent::StepEnded {
            step: 1,
            usage: Some(usage()),
            cost_usd: None,
            lower_bound: false,
        },
        d(14_000),
        FULL,
    );
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 14_010),
        d(14_010),
        FULL,
    );
    let row = line(&s.row(d(30_000), FULL, &facts(), layout(149)));
    assert!(!row.contains("tok/s"), "{row}");
    for (seconds, attempt, max, expected) in [
        (300, 12, 0, "Retrying in 5 min (attempt 12): cause"),
        (30, 2, 0, "Retrying in 30 s (attempt 2): cause"),
        (0, 0, 0, "Retrying in 0 s: cause"),
        (30, 0, 5, "Retrying in 30 s: cause"),
    ] {
        assert_eq!(copy::retrying_in(seconds, attempt, max, "cause"), expected);
    }
}

#[test]
fn a_resend_never_shows_a_retry_count() {
    let resend = |delay_ms: u64, attempt: u32| AgentEvent::Retrying {
        attempt,
        delay_ms,
        reason: "too many cache breakpoints".to_owned(),
        step: 1,
        kind: wisp_protocol::ErrorKind::InvalidRequest,
        detail: None,
        max_attempts: 5,
        discarded_blocks: 1,
        discarded_chars: 12,
        usage: None,
        model: None,
        cost_usd: Some(0.01),
        lower_bound: true,
        resend: Some(wisp_protocol::Resend::BreakpointLimit),
        busy: None,
    };
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    s.apply(&resend(0, 0), d(1000), FULL);
    let hint = "Too many cache breakpoints; sent again without one";
    for ms in [1000, 2500, 3999] {
        assert_eq!(say(&s, ms, FULL), hint);
    }
    assert_eq!(
        s.next_change(d(3500), FULL).map(|t| t <= d(4000)),
        Some(true)
    );
    for ms in [1000, 4000, 9000] {
        let said = say(&s, ms, FULL);
        assert!(!said.contains("Retrying"), "{said}");
        assert!(!said.contains(" of 5"), "{said}");
    }
    assert_ne!(say(&s, 4000, FULL), hint);
}

#[test]
fn answering_offers_esc_and_relights_the_core_per_chunk() {
    let mut s = answered(FULL);
    assert_eq!(say(&s, 900, FULL), "Kindling\u{2026}");
    assert_eq!(
        say(&s, 1000, FULL),
        "Kindling\u{2026} 1s \u{00B7} \u{2193} \u{2248}1 token \u{00B7} \u{2248}1 tok/s \u{00B7} esc to stop"
    );
    let spans = s.sentence(d(1000), SIXTEEN, &UNICODE);
    assert_eq!(spans[0].look.fg, Some(Ink::Token(Token::Text)));
    s.apply(&delta(), d(1300), FULL);
    let lit = s.core(d(1300), FULL);
    assert_eq!(
        lit,
        Ink::mix(Token::Accent, Token::OrbCoreDim, 1.0, Token::Accent)
    );
    assert_ne!(s.core(d(1800), FULL), lit);
    s.apply(&delta(), d(1900), FULL);
    assert_eq!(s.core(d(1900), FULL), lit);
}

#[test]
fn done_names_time_and_cache_then_reads_ready() {
    for (cx, hold) in [(FULL, 2200), (CALM, 4400), (REDUCED, 1200)] {
        for (ids, ready) in [
            (&[][..], "Ready"),
            (&[3][..], "Ready · agent continues when process 3 ends"),
            (
                &[3, 5, 7][..],
                "Ready · agent continues when 1 of 3 processes ends",
            ),
        ] {
            let mut s = answered(cx);
            s.background_processes(ids.to_vec());
            s.apply(
                &ended(completed(StopReason::EndTurn, true), 18_000),
                d(2000),
                cx,
            );
            assert_eq!(
                say(&s, 2000, cx),
                "Done \u{00B7} 18s \u{00B7} \u{2193} 475 tokens \u{00B7} cached 92%"
            );
            let spans = s.sentence(d(2000), cx, &UNICODE);
            assert_eq!(spans[0].look.fg, Some(Ink::Token(Token::Ok)));
            assert_eq!(say(&s, 2000 + hold - 1, cx), say(&s, 2000, cx));
            assert_eq!(say(&s, 2000 + hold, cx), ready);
            assert!(s.next_change(d(2000 + hold - 1), cx).is_some());
            s.background_processes(Vec::new());
            assert_eq!(say(&s, 2000 + hold, cx), "Ready");
        }
    }
}

#[test]
fn process_wake_names_match_the_shell_manifest() {
    use wisp_plugin_api::manifest::{Manifest, Origin};

    let manifest = Manifest::parse(
        include_str!("../../../../plugins/shell/wisp-plugin.toml"),
        Origin::Bundled,
    )
    .unwrap();
    assert_eq!(manifest.id, super::status::PROCESS_PLUGIN);
    assert!(
        manifest
            .hooks
            .iter()
            .any(|hook| { hook.name == super::status::PROCESS_NEWS && hook.job_news })
    );
}

#[test]
fn a_cache_miss_reaches_the_done_sentence() {
    let mut s = answered(FULL);
    s.apply(
        &AgentEvent::CacheMiss {
            previous_prompt_tokens: 10,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 10,
            diagnostics: None,
            cause: None,
        },
        d(900),
        FULL,
    );
    let outcome = outcome(End {
        model: "m".to_owned(),
        usage: Some(usage()),
        cost: Some(cost(0.5, 1.0, true)),
        ..End::default()
    });
    s.apply(&ended(outcome, 4000), d(1000), FULL);
    assert_eq!(
        say(&s, 1000, FULL),
        "Done \u{00B7} 4s \u{00B7} \u{2193} 475 tokens \u{00B7} cache miss"
    );
}

#[test]
fn only_a_kept_normal_end_is_done() {
    for (outcome, note) in [
        (
            completed(StopReason::MaxTokens, true),
            "Cut off at the output limit",
        ),
        (completed(StopReason::Refusal, true), "The model declined"),
        (completed(StopReason::EndTurn, false), "Not kept in history"),
        (
            completed(StopReason::MaxTokens, false),
            "Cut off at the output limit \u{00B7} not kept in history",
        ),
    ] {
        let mut s = answered(FULL);
        s.apply(&ended(outcome, 1000), d(1000), FULL);
        assert_eq!(s.state(d(1000), FULL), State::Ready);
        assert_eq!(say(&s, 1000, FULL), note);
        assert_eq!(say(&s, 60_000, FULL), note);
        assert!(!say(&s, 1000, FULL).contains("Done"));
    }
    let mut s = answered(FULL);
    s.apply(&ended(failed(), 1000), d(1000), FULL);
    assert_eq!(s.state(d(1000), FULL), State::Ready);
    assert_eq!(say(&s, 1000, FULL), "Ready");
    let mut s = answered(FULL);
    s.apply(&AgentEvent::StepStarted { step: 2 }, d(900), FULL);
    s.apply(&text_block(), d(950), FULL);
    s.apply(
        &ended(completed(StopReason::EndTurn, false), 1000),
        d(1000),
        FULL,
    );
    assert!(
        say(&s, 1000, FULL).starts_with("Done"),
        "{}",
        say(&s, 1000, FULL)
    );
}

#[test]
fn stopped_yields_the_core_for_300_ms_and_says_what_was_kept() {
    let mut s = answered(FULL);
    s.apply(&ended(cancelled(), 1000), d(1000), FULL);
    assert_eq!(s.state(d(1000), FULL), State::Stopped);
    assert_eq!(s.orb(d(1250), FULL).core(), Ink::Token(Token::OrbCoreDim));
    assert_eq!(s.state(d(1300), FULL), State::Ready);
    assert_eq!(
        say(&s, 1000, FULL),
        "Stopped. Nothing from this turn was kept."
    );
    assert_eq!(
        say(&s, 3199, FULL),
        "Stopped. Nothing from this turn was kept."
    );
    assert_eq!(say(&s, 3200, FULL), "Ready");
    let mut r = answered(REDUCED);
    r.apply(&ended(cancelled(), 1000), d(1000), REDUCED);
    assert_eq!(r.state(d(1000), REDUCED), State::Ready);
}

#[test]
fn typing_during_done_listens_at_once_and_text_left_over_listens_after() {
    let mut s = answered(FULL);
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 900),
        d(1000),
        FULL,
    );
    s.composer(false, d(1500), FULL);
    assert_eq!(s.state(d(1500), FULL), State::Listening);
    assert_eq!(say(&s, 1500, FULL), "");

    let mut s = answered(FULL);
    s.composer(false, d(900), FULL);
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 900),
        d(1000),
        FULL,
    );
    assert_eq!(s.state(d(1000), FULL), State::Done);
    assert_eq!(s.state(d(3200), FULL), State::Listening);
    assert_eq!(s.orb(d(3200), FULL).text(), "\u{288E}\u{2836}\u{2871}");
    s.composer(true, d(4000), FULL);
    assert_eq!(s.state(d(4000), FULL), State::Ready);
}

#[test]
fn a_rejected_send_returns_to_rest_only_when_told_nothing_waits() {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::rejected("busy".to_owned()), d(50), FULL);
    assert!(s.is_awaiting());
    s.stop_awaiting(d(50), FULL);
    assert_eq!(s.state(d(50), FULL), State::Ready);
    assert!(!s.is_running());
    assert!(!s.is_awaiting());
}

#[test]
fn still_states_schedule_nothing() {
    for cx in [FULL, CALM, REDUCED, SIXTEEN, ASCII] {
        let s = Status::new();
        assert_eq!(s.next_change(d(0), cx), None);
        let mut l = Status::new();
        l.composer(false, d(0), cx);
        assert_eq!(l.next_change(d(1000), cx), None);
        let mut done = answered(cx);
        done.apply(&ended(completed(StopReason::EndTurn, true), 1), d(1000), cx);
        assert_eq!(done.next_change(d(20_000), cx), None);
        let mut note = answered(cx);
        note.apply(
            &ended(completed(StopReason::MaxTokens, true), 1),
            d(1000),
            cx,
        );
        assert_eq!(note.next_change(d(2000), cx), None);
    }
}

fn frames(s: &Status, from: u64, window: u64, cx: MotionCtx) -> Vec<u64> {
    let mut t = d(from);
    let mut out = vec![];
    while let Some(n) = s.next_change(t, cx) {
        if n > d(from + window) {
            break;
        }
        assert!(n > t);
        out.push(n.as_millis() as u64);
        t = n;
    }
    out
}

fn tool(index: usize) -> AgentEvent {
    AgentEvent::BlockStarted {
        index,
        kind: BlockKind::ToolUse {
            writing: None,
            name: "x".to_owned(),
        },
    }
}

fn cases(cx: MotionCtx) -> Vec<(&'static str, Status, u64)> {
    let mut out = vec![("ready", Status::new(), 0)];
    let mut listening = Status::new();
    listening.composer(false, d(0), cx);
    out.push(("listening", listening, 0));
    let mut thinking = Status::new();
    thinking.submitted(d(0), cx);
    out.push(("thinking", thinking.clone(), 0));
    let mut retry = thinking.clone();
    retry.apply(&AgentEvent::turn_started(1), d(5), cx);
    retry.apply(
        &AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 8000,
            reason: "x".to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        d(1210),
        cx,
    );
    out.push(("retry off the grid", retry, 1210));
    let mut short = thinking;
    short.apply(
        &AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 3500,
            reason: "x".to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        d(700),
        cx,
    );
    out.push(("short retry", short, 700));
    out.push(("answering", answered(cx), 820));
    let mut working = answered(cx);
    working.apply(&tool(3), d(1000), cx);
    out.push(("working", working, 1000));
    for typed in [false, true] {
        let mut done = answered(cx);
        let mut stopped = answered(cx);
        if typed {
            done.composer(false, d(900), cx);
            stopped.composer(false, d(900), cx);
        }
        done.apply(&ended(completed(StopReason::EndTurn, true), 1), d(1000), cx);
        stopped.apply(&ended(cancelled(), 1), d(1000), cx);
        out.push((if typed { "done, text left" } else { "done" }, done, 1000));
        out.push((
            if typed {
                "stopped, text left"
            } else {
                "stopped"
            },
            stopped,
            1000,
        ));
    }
    let mut hinted = answered(cx);
    hinted.hint("Press ctrl-c again to quit", d(1000), Some(d(1500)));
    out.push(("hint", hinted, 1000));
    let mut held = answered(cx);
    held.hint("Wisp is still answering", d(1000), None);
    out.push(("hint until cleared", held, 1000));
    out
}

#[test]
fn the_frame_budget_holds_over_a_fake_clock() {
    for cx in [FULL, CALM, REDUCED, SIXTEEN, ASCII] {
        assert!(frames(&Status::new(), 0, 10_000, cx).is_empty());
        let mut thinking = Status::new();
        thinking.submitted(d(0), cx);
        let t = frames(&thinking, 0, 10_000, cx);
        assert!(t.len() <= 100, "{cx:?}: {} thinking frames", t.len());
        for (name, s, from) in cases(cx) {
            let times = frames(&s, from, 10_000, cx);
            let mut last = from;
            for t in &times {
                assert!(t - last >= 33, "{name} {cx:?}: {t} after {last}");
                last = *t;
            }
            for second in 0..12 {
                let start = from + second * 1000;
                let n = times
                    .iter()
                    .filter(|t| **t >= start && **t < start + 1000)
                    .count();
                assert!(n <= 30, "{name} {cx:?}: {n} frames in second {second}");
            }
        }
    }
}

#[test]
fn every_scheduled_change_is_real() {
    for cx in [FULL, CALM, REDUCED, SIXTEEN, ASCII] {
        for (name, s, from) in cases(cx) {
            let mut t = d(from);
            for _ in 0..400 {
                let Some(n) = s.next_change(t, cx) else {
                    break;
                };
                let now = (s.orb(t, cx), s.sentence(t, cx, &UNICODE));
                let then = (s.orb(n, cx), s.sentence(n, cx, &UNICODE));
                assert_ne!(now, then, "{name} {cx:?}: nothing changed at {n:?}");
                t = n;
            }
        }
    }
}

#[test]
fn text_left_at_the_end_morphs_into_listening() {
    let mut s = answered(FULL);
    s.composer(false, d(900), FULL);
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 1),
        d(1000),
        FULL,
    );
    let at = s.orb(d(3200 + 80), FULL);
    assert_eq!(at.text(), "\u{288E}\u{2836}\u{2871}");
    assert!(matches!(at.cells[0].ink, Ink::Mix { .. }), "{at:?}");
    assert_eq!(
        s.orb(d(3200 + 240), FULL).cells[0].ink,
        Ink::Token(Token::OrbRingListen)
    );
}

#[test]
fn a_finished_done_stays_finished_when_motion_returns() {
    let mut s = answered(FULL);
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 1),
        d(1000),
        REDUCED,
    );
    assert_eq!(say(&s, 2300, REDUCED), "Ready");
    assert_eq!(say(&s, 2500, FULL), "Ready");
    assert_eq!(s.state(d(2500), FULL), State::Ready);
    assert_eq!(s.orb(d(2500), FULL), Status::new().orb(d(0), FULL));
}

#[test]
fn a_hint_takes_the_sentence_until_it_expires() {
    let mut s = answered(FULL);
    s.hint(
        "Press ctrl-c again to quit\u{1b}[2J",
        d(1000),
        Some(d(1500)),
    );
    assert_eq!(say(&s, 1000, FULL), "Press ctrl-c again to quit^[[2J");
    assert_eq!(say(&s, 2499, FULL), "Press ctrl-c again to quit^[[2J");
    assert_eq!(
        say(&s, 2500, FULL),
        "Kindling\u{2026} 2s \u{00B7} \u{2193} \u{2248}1 token \u{00B7} \u{2248}0 tok/s \u{00B7} esc to stop"
    );
    assert!(s.next_change(d(2400), FULL).is_some_and(|n| n <= d(2500)));
    let row = line(&s.row(d(1000), FULL, &facts(), layout(60)));
    assert!(row.ends_with("esc to stop"), "{row}");
    s.clear_hint();
    assert_eq!(
        say(&s, 1000, FULL),
        "Kindling\u{2026} 1s \u{00B7} \u{2193} \u{2248}1 token \u{00B7} \u{2248}1 tok/s \u{00B7} esc to stop"
    );
    let mut idle = Status::new();
    idle.hint("No pager yet", d(0), Some(d(2000)));
    assert_eq!(idle.next_change(d(0), FULL), Some(d(2000)));
    assert_eq!(idle.next_change(d(2000), FULL), None);
}

#[test]
fn a_hint_until_cleared_is_never_scheduled() {
    let mut s = Status::new();
    s.hint("Press ctrl-c again to quit", d(1000), None);
    assert_eq!(s.next_change(d(1000), FULL), None);
    assert_eq!(say(&s, 9_000_000, FULL), "Press ctrl-c again to quit");
    s.clear_hint();
    assert_eq!(say(&s, 9_000_000, FULL), "Ready");
    let mut busy = answered(FULL);
    busy.hint("Wisp is still answering", d(900), None);
    let row = line(&busy.row(d(900), FULL, &facts(), layout(60)));
    assert!(
        row.contains("Wisp is still answering   esc to stop   "),
        "{row}"
    );
    let row = line(&busy.row(d(900), FULL, &facts(), layout(59)));
    assert!(
        row.ends_with("Wisp is still answering   esc to stop"),
        "{row}"
    );
}

#[test]
fn next_change_stays_within_the_horizon() {
    let mut cases = vec![];
    let mut hint = Status::new();
    hint.hint("x", d(0), Some(Duration::MAX));
    cases.push(hint);
    let mut long = Status::new();
    long.hint("x", d(0), Some(Duration::from_secs(86_400)));
    cases.push(long);
    let mut retry = Status::new();
    retry.submitted(d(0), FULL);
    retry.apply(
        &AgentEvent::Retrying {
            attempt: 1,
            delay_ms: u64::MAX,
            reason: "x".to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        d(10),
        FULL,
    );
    cases.push(retry);
    for s in &cases {
        for t in [0, 10, 5_000, 4_000_000] {
            if let Some(n) = s.next_change(d(t), FULL) {
                assert!(n <= d(t) + SCHEDULE_HORIZON, "{n:?} after {t}");
                assert!(n.checked_add(Duration::from_secs(1 << 40)).is_some());
            }
        }
    }
    assert_eq!(cases[0].next_change(d(0), FULL), Some(SCHEDULE_HORIZON));
}

#[test]
fn a_hint_longer_than_the_horizon_still_wakes_to_clear() {
    let mut s = Status::new();
    s.hint("x", d(0), Some(Duration::from_secs(61 * 60)));
    assert_eq!(s.next_change(d(0), FULL), Some(SCHEDULE_HORIZON));
    assert_eq!(say(&s, 0, FULL), "x");
    assert_eq!(say(&s, SCHEDULE_HORIZON.as_millis() as u64, FULL), "Ready");
    assert_eq!(s.next_change(SCHEDULE_HORIZON, FULL), None);
}

#[test]
fn typing_during_the_stopped_hold_morphs_into_listening() {
    let mut s = answered(FULL);
    s.apply(&ended(cancelled(), 1), d(1000), FULL);
    let before = s.orb(d(1999), FULL);
    assert_eq!(before.text(), "\u{288E}\u{2836}\u{2870}");
    s.composer(false, d(2000), FULL);
    let at = s.orb(d(2000), FULL);
    assert_eq!(at.text(), "\u{288E}\u{2836}\u{2871}");
    assert!(matches!(at.cells[0].ink, Ink::Mix { .. }), "{at:?}");
    assert_eq!(
        s.orb(d(2240), FULL).cells[0].ink,
        Ink::Token(Token::OrbRingListen)
    );
    assert_eq!(s.state(d(2000), FULL), State::Listening);
    assert_eq!(say(&s, 2000, FULL), "");
    let mut quiet = answered(FULL);
    quiet.apply(&ended(cancelled(), 1), d(1000), FULL);
    quiet.composer(true, d(2000), FULL);
    assert_eq!(quiet.state(d(2000), FULL), State::Ready);
    assert_eq!(
        say(&quiet, 2000, FULL),
        "Stopped. Nothing from this turn was kept."
    );
    assert_eq!(say(&quiet, 3200, FULL), "Ready");
}

#[test]
fn the_caret_waits_for_the_first_drawn_row() {
    use crate::markdown::Renderer;
    use crate::transcript::{Config, Transcript};
    let r = Renderer::new(layout(80));
    let mut tr = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: true,
    });
    tr.apply(&AgentEvent::turn_started(1), d(5));
    tr.deliver(vec![UserPart::Text("Hi".to_owned())], None);
    tr.apply(&text_block(), d(100));
    let f = tr.frame(&r);
    assert_eq!(f.prose, None);
    let mut live = f.live.clone();
    place_caret(
        &mut live,
        f.prose,
        Ink::Token(Token::Accent),
        FULL,
        layout(80),
    );
    assert!(live.iter().all(|l| !line(l).contains('\u{2836}')));
    tr.apply(&delta(), d(200));
    let f = tr.frame(&r);
    let mut live = f.live.clone();
    place_caret(
        &mut live,
        f.prose,
        Ink::Token(Token::Accent),
        FULL,
        layout(80),
    );
    let rows: Vec<String> = live.iter().map(line).collect();
    assert_eq!(rows, ["", " \u{2836} Hi \u{2836}"]);
}

#[test]
fn an_unfocused_window_keeps_every_animation_moving() {
    let mut p = Pacing::new(FULL);
    p.focus(false);
    let cx = p.ctx();
    assert_eq!(cx, FULL);
    let mut thinking = Status::new();
    thinking.submitted(d(0), cx);
    assert_ne!(thinking.orb(d(1000), cx), thinking.orb(d(1600), cx));
    let idle = Status::new();
    let moving = |t| crate::motion::idle(idle.resting(d(t), cx).unwrap(), cx);
    assert_ne!(moving(3000), moving(4500));
}

#[test]
fn frames_never_come_faster_than_thirty_a_second() {
    assert_eq!(MAX_FPS_CHECK, 30);
    assert_eq!(next_frame(d(10), Some(d(0))), d(33));
    assert_eq!(next_frame(d(100), Some(d(0))), d(100));
    assert_eq!(next_frame(d(5), None), d(5));
}

const MAX_FPS_CHECK: u32 = crate::motion::MAX_FPS;

#[test]
fn the_status_row_drops_facts_from_the_right() {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    s.apply(&ended(completed(StopReason::EndTurn, true), 1), d(10), FULL);
    let f = facts();
    let at = d(5000);
    let wide = line(&s.row(at, FULL, &f, layout(120)));
    assert!(
        wide.ends_with("Claude API   sonnet-5-5 low \u{00B7} cached 92%"),
        "{wide}"
    );
    assert_eq!(crate::text::width(&wide, Default::default()), 119);
    let row80 = line(&s.row(at, FULL, &f, layout(80)));
    assert_eq!(crate::text::width(&row80, Default::default()), 79);
    let mut last = usize::MAX;
    for w in (10..=120).rev() {
        let row = line(&s.row(at, FULL, &f, layout(w)));
        let width = crate::text::width(&row, Default::default());
        assert!(width < w.max(2), "{w}: {row}");
        let n = row.matches('\u{00B7}').count();
        assert!(n <= last, "facts reappeared at {w}");
        last = n;
    }
    let row40 = line(&s.row(at, FULL, &f, layout(40)));
    assert!(row40.ends_with("Claude API   sonnet-5-5"), "{row40}");
    let row20 = line(&s.row(at, FULL, &f, layout(20)));
    assert_eq!(row20, " \u{288E}\u{2836}\u{2870}  Ready");
}

#[test]
fn a_narrow_status_row_drops_the_detail_before_cutting_a_word() {
    let cx = FULL;
    let started = || {
        let mut s = Status::new();
        s.submitted(d(0), cx);
        s.apply(&AgentEvent::turn_started(1), d(5), cx);
        s.apply(&AgentEvent::StepStarted { step: 1 }, d(6), cx);
        s
    };
    let mut writing = started();
    writing.apply(&tool(1), d(10), cx);
    let running = |doing: &str| {
        let mut s = started();
        s.name_call("c", doing);
        s.apply(&AgentEvent::call_started("c", "~"), d(10), cx);
        s
    };
    let mut retrying = started();
    retrying.apply(&compacting::retrying(), d(10), cx);
    let mut stopped = answered(cx);
    stopped.apply(&ended(cancelled(), 1), d(1000), cx);
    let f = facts();
    let after_orb = |s: &Status, at: u64, w: usize| -> String {
        line(&s.row(d(at), cx, &f, layout(w)))
            .chars()
            .skip(6)
            .collect()
    };
    let stop = "   esc to stop";
    for (s, at, narrow, sixty, whole) in [
        (
            writing,
            1010,
            "Writing · 1s",
            "Writing a x call · 1s",
            "Writing a x call",
        ),
        (
            running("Run the tests in wisp-core"),
            1010,
            "Run the tests… · 1s",
            "Run the tests in… · 1s",
            "Run the tests in wisp-core",
        ),
        (
            running("Running the tests in wisp-core"),
            1010,
            "Running the… · 1s",
            "Running the tests in… · 1s",
            "Running the tests in wisp-core",
        ),
        (
            retrying,
            10,
            "Retrying in 8 s",
            "Retrying in 8 s",
            "Retrying in 8 s (1 of 5): overloaded",
        ),
        (
            asking(CacheTtl::OneHour, 1000, 1000),
            1000,
            "Waiting · 0s",
            "Waiting · 0s",
            "Waiting for your answer",
        ),
    ] {
        assert_eq!(after_orb(&s, at, 40), format!("{narrow}{stop}"));
        assert!(after_orb(&s, at, 60).starts_with(&format!("{sixty}{stop}")));
        assert!(after_orb(&s, at, 80).starts_with(whole));
    }
    assert_eq!(
        after_orb(&stopped, 1000, 40),
        "Stopped.               Claude API"
    );
    assert_eq!(
        after_orb(&stopped, 1000, 60),
        "Stopped. Nothing from this turn was kept."
    );
}

#[test]
fn snapshot_wide_status_rows() {
    let mut done = Status::new();
    done.submitted(d(0), FULL);
    done.apply(&AgentEvent::turn_started(1), d(5), FULL);
    done.apply(&ended(completed(StopReason::EndTurn, true), 1), d(10), FULL);
    let thinking = long_turn(16, false);
    let mut out = String::new();
    for w in [120, 160, 200] {
        out.push_str(&format!("--- {w} ---\n"));
        for row in [
            done.row(d(5000), FULL, &facts(), layout(w)),
            thinking.row(d(65_000), SIXTEEN, &facts(), layout(w)),
        ] {
            let row = line(&row);
            assert_eq!(crate::text::width(&row, Default::default()), w - 1, "{row}");
            out.push_str(&row);
            out.push('\n');
        }
    }
    let facts = Facts {
        endpoint_name: Some("claude-main".to_owned()),
        endpoints: vec![subscribed("claude-main")],
        ..claude_max()
    };
    done.background_processes(vec![3]);
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let layout = Layout::new(106, set);
        let cx = MotionCtx {
            ascii: set == GlyphSet::Ascii,
            ..FULL
        };
        let glyphs = Glyphs::of(set);
        done.keys(&[
            KeyHint::note(format!("{} {}", glyphs.running, copy::running_count(1))),
            KeyHint::note(copy::PS_HINT),
        ]);
        let row = frame_row(done.row(d(5000), cx, &facts, frame_inner(layout)), layout);
        let row = line(&row);
        assert!(row.contains("agent continues when process 3 ends"), "{row}");
        assert!(row.contains(copy::PS_HINT), "{row}");
        assert!(row.contains("claude-main"), "{row}");
        out.push_str(&format!("--- 106 {set:?} background ---\n{row}\n"));
    }
    insta::assert_snapshot!("wide_status_rows", out);
}

#[test]
fn a_hint_that_names_the_stop_key_is_not_repeated_on_the_right() {
    let hint = "ctrl-c copy \u{b7} esc to stop";
    for answering in [false, true] {
        let mut s = long_turn(16, answering);
        s.hint_naming_stop(hint, d(60_000), None);
        for w in 40..=200 {
            let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
            assert!(row.contains(hint), "{w}: {row}");
            assert_eq!(
                row.matches(&*copy::esc_to_stop().text()).count(),
                1,
                "{w}: {row}"
            );
            assert!(row.chars().count() < w, "{w}: {row}");
        }
        let mut plain = long_turn(16, answering);
        plain.hint(hint, d(60_000), None);
        let row = line(&plain.row(d(65_000), SIXTEEN, &facts(), layout(60)));
        assert!(
            row.contains(&format!("{hint}   {}", copy::esc_to_stop().text())),
            "{row}"
        );
        let row = line(&plain.row(d(65_000), SIXTEEN, &facts(), layout(59)));
        assert!(row.ends_with(&copy::esc_to_stop().text()), "{row}");
        for w in 1..40 {
            let named = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
            let shown = line(&plain.row(d(65_000), SIXTEEN, &facts(), layout(w)));
            assert!(
                named.matches(&*copy::esc_to_stop().text()).count() <= 1,
                "{w}: {named}"
            );
            assert!(shown.chars().count() <= w.max(1), "{w}: {shown}");
        }
    }
}

#[test]
fn a_narrow_row_offers_esc_while_running() {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    let f = facts();
    let row = line(&s.row(d(3000), FULL, &f, layout(60)));
    assert!(row.contains("   esc to stop   "), "{row}");
    assert!(row.ends_with("Claude API"), "{row}");
    let row = line(&s.row(d(3000), FULL, &f, layout(59)));
    assert!(row.ends_with("   esc to stop"), "{row}");
    let row = line(&s.row(d(3000), FULL, &f, layout(100)));
    assert!(row.ends_with("Claude API   sonnet-5-5 low"), "{row}");
    let a = answered(FULL);
    let row = line(&a.row(d(900), FULL, &f, layout(60)));
    assert_eq!(row.matches("esc to stop").count(), 1, "{row}");
}

#[test]
fn the_row_starts_with_the_orb_and_two_spaces() {
    let s = Status::new();
    let row = line(&s.row(d(0), FULL, &facts(), layout(80)));
    assert!(row.starts_with(" \u{288E}\u{2836}\u{2870}  Ready"), "{row}");
    let gateway = Facts {
        credential: Some(Credential::AuthToken),
        ..facts()
    };
    assert!(
        line(&s.row(d(0), FULL, &gateway, layout(80))).ends_with("Claude API   sonnet-5-5 low")
    );
    let replay = Facts {
        credential: None,
        ..facts()
    };
    let row = line(&s.row(d(0), FULL, &replay, layout(80)));
    assert!(row.ends_with("sonnet-5-5 low"), "{row}");
    assert!(!row.contains("key") && !row.contains("token"), "{row}");
}

#[test]
fn the_banner_reveals_the_logo_then_commits_its_final_frame() {
    let f = facts();
    let first = banner(&f, d(0), FULL, layout(80));
    assert_eq!(
        line(&first[0]),
        "  \u{2836}  wisp 0.1.0 \u{00B7} claude-sonnet-5-5 \u{00B7} ~/Projects/Wisp"
    );
    let last = banner(&f, d(280), FULL, layout(80));
    assert_eq!(
        line(&last[0]),
        " \u{288E}\u{2836}\u{2870} wisp 0.1.0 \u{00B7} claude-sonnet-5-5 \u{00B7} ~/Projects/Wisp"
    );
    assert_eq!(banner(&f, d(10_000), FULL, layout(80)), last);
    assert_eq!(
        banner(&f, d(0), REDUCED, layout(80)),
        banner(&f, d(0), REDUCED, layout(80))
    );
    assert_eq!(
        line(&banner(&f, d(0), REDUCED, layout(80))[0]),
        line(&last[0])
    );
    assert_eq!(line(&last[1]), "   low effort \u{00B7} Claude API");
    assert_eq!(last.len(), 2);
    let detailed = banner_with(&f, d(280), FULL, layout(80), true);
    assert_eq!(
        line(&detailed[2]),
        "   cache 1h \u{00B7} logging to ~/.local/share/wisp/sessions/0193.jsonl"
    );
    assert_eq!(last[1].spans[1].look.fg, Some(Ink::Token(Token::TextMuted)));
    let mut changes = vec![];
    let mut t = d(0);
    while let Some(n) = banner_next_change(t, FULL) {
        changes.push(n.as_millis() as u64);
        t = n;
    }
    assert_eq!(changes, [40, 80, 120, 160, 200, 240, 280]);
    assert_eq!(banner_next_change(d(0), REDUCED), None);
}

#[test]
fn the_banner_falls_back_to_ascii_and_tells_the_truth_about_logging() {
    let f = Facts {
        log: None,
        host: Some("gw.example.com".to_owned()),
        endpoint: Endpoint::Custom,
        credential: Some(Credential::AuthToken),
        ..facts()
    };
    let rows = banner(&f, d(0), ASCII, Layout::new(80, GlyphSet::Ascii));
    assert_eq!(
        line(&rows[0]),
        " (*) wisp 0.1.0 - claude-sonnet-5-5 - ~/Projects/Wisp"
    );
    assert_eq!(line(&rows[1]), "   low effort - Gateway - gw.example.com");
    let rows = banner_with(&f, d(0), ASCII, Layout::new(80, GlyphSet::Ascii), true);
    assert!(
        rows.iter()
            .any(|r| line(r) == "   cache 1h - not logging this session")
    );
    for r in &rows {
        assert!(line(r).is_ascii());
    }
}

#[test]
fn a_long_path_keeps_its_end() {
    let f = Facts {
        cwd: "~/a/very/long/path/that/goes/on/and/on/and/on/to/the/project".to_owned(),
        ..facts()
    };
    let row = line(&banner(&f, d(280), FULL, layout(60))[0]);
    assert!(row.ends_with("\u{2026}/project"), "{row}");
    assert!(row.contains("~/a/very/"), "{row}");
    assert!(crate::text::width(&row, Default::default()) <= 59);
    let tiny = line(&banner(&f, d(280), FULL, layout(30))[0]);
    assert!(
        crate::text::width(&tiny, Default::default()) <= 29,
        "{tiny}"
    );
}

#[test]
fn hostile_facts_are_sanitised() {
    let f = Facts {
        model: "m\u{1b}]0;x\u{7}\nnext".to_owned(),
        cwd: "~/p\u{202E}x".to_owned(),
        ..facts()
    };
    let mut rows = banner_with(&f, d(280), FULL, layout(80), true);
    let mut s = answered(FULL);
    let hostile = StopReason::Other("x\u{1b}]0;pwned\u{7}\nFAKE PROMPT> \u{202E}y".to_owned());
    s.apply(&ended(completed(hostile, true), 1000), d(1000), FULL);
    rows.push(s.row(d(1000), FULL, &f, layout(120)));
    let mut w = answered(FULL);
    w.apply(
        &AgentEvent::BlockStarted {
            index: 2,
            kind: BlockKind::ToolUse {
                writing: None,
                name: "a\u{1b}[2Jb\n".to_owned(),
            },
        },
        d(1000),
        FULL,
    );
    rows.push(w.row(d(1000), FULL, &f, layout(120)));
    for r in &rows {
        let t = line(r);
        assert!(
            !t.contains('\u{1b}')
                && !t.contains('\n')
                && !t.contains('\u{7}')
                && !t.contains('\u{202E}'),
            "{t:?}"
        );
    }
    assert!(
        line(&rows[3]).contains("Stopped: x^[]0;pwned^G FAKE PROMPT> <U+202E>y"),
        "{:?}",
        line(&rows[3])
    );
}

#[test]
fn the_caret_follows_the_last_character_and_only_in_the_live_tail() {
    let core = Ink::Token(Token::Accent);
    let row = |t: &str| StyledLine::new(vec![StyledSpan::plain(t)]);
    let mut live = vec![row("   Hello world"), row(""), row(" › next")];
    place_caret(&mut live, Some(1), core, FULL, layout(80));
    assert_eq!(line(&live[0]), "   Hello world \u{2836}");
    assert_eq!(live[0].spans[1].look.fg, Some(core));
    assert_eq!(live.len(), 3);
    let mut spaced = vec![row("   Hello ")];
    place_caret(&mut spaced, Some(1), core, FULL, layout(80));
    assert_eq!(line(&spaced[0]), "   Hello \u{2836}");
    let mut none = vec![row("   x")];
    place_caret(&mut none, None, core, FULL, layout(80));
    assert_eq!(line(&none[0]), "   x");
    let mut empty: Vec<StyledLine> = vec![];
    place_caret(&mut empty, Some(0), core, FULL, layout(80));
    assert_eq!(line(&empty[0]), "   \u{2836}");
    let mut full = vec![row(&format!("   {}", "x".repeat(76)))];
    place_caret(&mut full, Some(1), core, FULL, layout(80));
    assert_eq!(full.len(), 2);
    assert_eq!(line(&full[1]), "   \u{2836}");
    let mut ascii = vec![row("   Hi")];
    place_caret(&mut ascii, Some(1), core, ASCII, layout(80));
    assert_eq!(line(&ascii[0]), "   Hi _");
}

#[test]
fn the_composer_has_no_band_and_a_faint_placeholder() {
    let open = |l: Layout| ComposerShape::open(l);
    let row = composer_placeholder(open(layout(60)), None);
    let t = line(&row);
    assert!(
        t.starts_with(&format!(" \u{203A} {}", copy::PLACEHOLDER)),
        "{t}"
    );
    assert_eq!(crate::text::width(&t, Default::default()), 60);
    let look = ComposerLook::new();
    assert_eq!(row.spans[1].look, look.prompt);
    assert_eq!(row.spans[3].look, look.placeholder);
    assert!(row.spans.iter().all(|s| s.look.bg.is_none()));
    assert_eq!(composer_lead_width(open(layout(60))), 3);
    let first = composer_spans(
        true,
        vec![StyledSpan::new("one", look.text)],
        open(layout(60)),
    );
    let next = composer_spans(
        false,
        vec![StyledSpan::new("two", look.text)],
        open(layout(60)),
    );
    assert_eq!(line(&first).trim_end(), " \u{203A} one");
    assert_eq!(line(&next).trim_end(), "   two");
    assert_eq!(first.spans[3].look, look.text);
    let tiny = open(Layout::new(6, GlyphSet::Ascii));
    assert_eq!(composer_lead_width(tiny), 2);
    assert_eq!(
        line(&composer_spans(
            true,
            vec![StyledSpan::new("x", look.text)],
            tiny
        )),
        "> x   "
    );
    let chip = composer_spans(
        true,
        vec![
            StyledSpan::new("a ", look.text),
            StyledSpan::new("[Pasted text #1 +9 lines]", look.chip),
        ],
        open(layout(60)),
    );
    assert_eq!(chip.spans[4].look, look.chip);
    assert_eq!(crate::text::width(&line(&chip), Default::default()), 60);
}

#[test]
fn the_boxed_composer_puts_the_prompt_at_column_2_and_the_input_at_column_4() {
    let shape = ComposerShape::on_screen(layout(60), 24);
    assert!(shape.boxed);
    assert_eq!(composer_lead_width(shape), 4);
    assert_eq!(shape.room(), 54);
    let look = ComposerLook::new();
    let first = composer_spans(true, vec![StyledSpan::new("one", look.text)], shape);
    let next = composer_spans(false, vec![StyledSpan::new("two", look.text)], shape);
    let pad = |used: usize| " ".repeat(60 - 1 - used);
    assert_eq!(
        line(&first),
        format!("\u{2502} \u{203A} one{}\u{2502}", pad(7))
    );
    assert_eq!(line(&next), format!("\u{2502}   two{}\u{2502}", pad(7)));
    assert_eq!(first.spans[0].look, look.frame);
    assert_eq!(first.spans[2].look, look.prompt);
    assert_eq!(first.spans.last().map(|s| s.look), Some(look.frame));
    assert_eq!(
        line(&composer_edge(shape, Edge::Top)),
        format!("\u{256D}{}\u{256E}", "\u{2500}".repeat(58))
    );
    assert_eq!(
        line(&composer_edge(shape, Edge::Bottom)),
        format!("\u{2570}{}\u{256F}", "\u{2500}".repeat(58))
    );
    assert_eq!(
        composer_edge(shape, Edge::Top).spans[0].look,
        Look::fg(Token::Line)
    );
}

#[test]
fn each_glyph_set_draws_the_composer_frame() {
    for (set, top, side, bottom, prompt) in [
        (
            GlyphSet::Unicode,
            "\u{256D}\u{2500}\u{256E}",
            "\u{2502}",
            "\u{2570}\u{2500}\u{256F}",
            "\u{203A}",
        ),
        (GlyphSet::AmbiguousWide, "+-+", "|", "+-+", "\u{203A}"),
        (GlyphSet::Ascii, "+-+", "|", "+-+", ">"),
    ] {
        let shape = ComposerShape::on_screen(Layout::new(40, set), 24);
        let mode = shape.layout.width_mode;
        let [l, a, r]: [&str; 3] = {
            let g: Vec<&str> = top.split_inclusive(|_| true).collect();
            [g[0], g[1], g[2]]
        };
        let edge = line(&composer_edge(shape, Edge::Top));
        assert_eq!(edge, format!("{l}{}{r}", a.repeat(38)), "{set:?}");
        assert_eq!(crate::text::width(&edge, mode), 40, "{set:?}");
        let [bl, _, br]: [&str; 3] = {
            let g: Vec<&str> = bottom.split_inclusive(|_| true).collect();
            [g[0], g[1], g[2]]
        };
        let low = line(&composer_edge(shape, Edge::Bottom));
        assert!(low.starts_with(bl) && low.ends_with(br), "{set:?} {low}");
        let row = line(&composer_placeholder(shape, None));
        assert!(
            row.starts_with(&format!("{side} {prompt} ")),
            "{set:?} {row}"
        );
        assert!(row.ends_with(side), "{set:?} {row}");
        assert_eq!(crate::text::width(&row, mode), 40, "{set:?} {row}");
    }
}

#[test]
fn the_box_needs_21_columns_and_5_rows() {
    assert!(ComposerShape::on_screen(layout(21), 5).boxed);
    assert!(!ComposerShape::on_screen(layout(20), 24).boxed);
    assert!(!ComposerShape::on_screen(layout(80), 4).boxed);
    let bare = line(&composer_placeholder(
        ComposerShape::on_screen(layout(20), 24),
        None,
    ));
    assert!(
        bare.starts_with(&format!(" \u{203A} {}", copy::PLACEHOLDER_SHORT)),
        "{bare}"
    );
    assert!(!bare.contains('\u{2502}'), "{bare}");
}

#[test]
fn a_narrow_box_shortens_the_placeholder() {
    let at = |w: usize| placeholder_for(ComposerShape::on_screen(layout(w), 24), None);
    assert_eq!(at(42), copy::PLACEHOLDER);
    assert_eq!(at(41), copy::PLACEHOLDER_SHORT);
    assert_eq!(at(40), copy::PLACEHOLDER_SHORT);
    assert_eq!(at(18), copy::PLACEHOLDER_SHORT);
    let tight = placeholder_for(ComposerShape::open(layout(12)), None);
    assert_eq!(tight, "Ask any\u{2026}");
    assert!(!ComposerShape::on_screen(layout(20), 24).boxed);
    let shape = ComposerShape::on_screen(layout(21), 24);
    assert!(shape.boxed);
    let divider = format!("\u{251C}{}\u{2524}", "\u{2500}".repeat(19));
    assert_eq!(line(&composer_edge(shape, Edge::Divider)), divider);
    let row = frame_row(StyledLine::new(vec![StyledSpan::plain(" x")]), layout(21));
    assert_eq!(line(&row), format!("\u{2502} x{}\u{2502}", " ".repeat(17)));
}

#[test]
fn without_the_keys_the_composer_steps_down_by_tone_and_keeps_its_frame() {
    let shape = ComposerShape::on_screen(layout(60), 24).with_keys(false);
    let look = ComposerLook::keyless();
    assert_eq!(look.prompt, Look::fg(Token::TextFaint));
    assert_eq!(look.text, Look::fg(Token::TextMuted));
    assert_eq!(look.placeholder, Look::fg(Token::TextFaint));
    assert_eq!(look.frame, ComposerLook::new().frame);
    let row = composer_spans(true, vec![StyledSpan::new("draft", look.text)], shape);
    assert_eq!(row.spans[2].look, look.prompt);
    assert_eq!(
        composer_edge(shape, Edge::Top),
        composer_edge(shape.with_keys(true), Edge::Top)
    );
}

fn every_sentence() -> Vec<String> {
    let mut out = vec![];
    let glyphs = Glyphs::of(GlyphSet::Unicode);
    let mut s = Status::new();
    out.push(spans_text(&s.sentence(d(0), FULL, &glyphs)));
    s.submitted(d(0), FULL);
    for t in [0, 300, 2000] {
        out.push(spans_text(&s.sentence(d(t), FULL, &glyphs)));
    }
    s.apply(
        &AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 3000,
            reason: "r".to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        d(10),
        FULL,
    );
    out.push(spans_text(&s.sentence(d(10), FULL, &glyphs)));
    let mut a = answered(FULL);
    out.push(spans_text(&a.sentence(d(900), FULL, &glyphs)));
    a.apply(
        &ended(completed(StopReason::EndTurn, true), 5),
        d(1000),
        FULL,
    );
    out.push(spans_text(&a.sentence(d(1000), FULL, &glyphs)));
    for outcome in [
        cancelled(),
        failed(),
        completed(StopReason::MaxTokens, false),
    ] {
        let mut b = answered(FULL);
        b.apply(&ended(outcome, 5), d(1000), FULL);
        out.push(spans_text(&b.sentence(d(1000), FULL, &glyphs)));
    }
    out
}

#[test]
fn every_status_string_is_true_for_m3() {
    let f = Facts {
        log: None,
        ..facts()
    };
    let mut all = every_sentence();
    all.extend(banner(&f, d(280), FULL, layout(120)).iter().map(line));
    all.push(ComposerLook::new().placeholder_text().to_owned());
    let s = Status::new();
    all.push(line(&s.row(d(0), FULL, &f, layout(120))));
    for text in &all {
        for banned in [
            "/keys",
            "ctrl-y",
            "context left",
            "kept in the log",
            "Jarvis",
            "Oops",
            "!",
            "steps that finished",
            "successfully",
        ] {
            assert!(!text.contains(banned), "{text:?} contains {banned:?}");
        }
    }
}

fn outcomes() -> Vec<TurnOutcome> {
    vec![
        completed(StopReason::EndTurn, true),
        completed(StopReason::MaxTokens, false),
        outcome(End {
            model: "m".to_owned(),
            usage: Some(usage()),
            cost: Some(cost(0.014, 0.21, true)),
            ..End::default()
        }),
        cancelled(),
        failed(),
    ]
}

fn chrome_strings(set: GlyphSet) -> Vec<String> {
    use crate::markdown::Renderer;
    use crate::transcript::{Config, Detail, Transcript};
    let cx = MotionCtx {
        ascii: set == GlyphSet::Ascii,
        ..FULL
    };
    let glyphs = Glyphs::of(set);
    let l = Layout::new(120, set);
    let narrow = Layout::new(60, set);
    let f = Facts {
        host: Some("gw.example.com".to_owned()),
        endpoint: Endpoint::Custom,
        credential: Some(Credential::AuthToken),
        cwd: "~/a/very/long/path/that/goes/on/and/on/and/on/to/the/project".to_owned(),
        ..facts()
    };
    let mut out: Vec<String> = vec![];
    let tips = tips();
    for t in [0, 120, 280] {
        out.extend(banner(&f, d(t), cx, l).iter().map(line));
        out.extend(banner(&f, d(t), cx, narrow).iter().map(line));
        for w in [120, 80, 60, 50, 40] {
            let at = Layout::new(w, set);
            out.extend(welcome(&f, &tips, d(t), cx, at).iter().map(line));
            out.extend(welcome_in_detail(&f, cx, at).iter().map(line));
        }
    }
    for shape in [ComposerShape::open(l), ComposerShape::on_screen(l, 24)] {
        out.push(line(&composer_placeholder(shape, None)));
        out.push(line(&composer_edge(shape, Edge::Top)));
        out.push(line(&composer_edge(shape, Edge::Bottom)));
    }
    for (_, s, from) in cases(cx) {
        for t in [from, from + 300, from + 2500, from + 5000] {
            out.push(line(&s.row(d(t), cx, &f, l)));
            out.push(line(&s.row(d(t), cx, &f, narrow)));
            out.push(spans_text(&s.sentence(d(t), cx, &glyphs)));
        }
    }
    for outcome in outcomes() {
        let mut s = answered(cx);
        s.apply(&ended(outcome.clone(), 18_000), d(1000), cx);
        out.push(line(&s.row(d(1000), cx, &f, l)));
        let mut tr = Transcript::new(Config {
            provider: "Anthropic".to_owned(),
            cache_ttl: CacheTtl::OneHour,
            logging: true,
        });
        tr.apply(&AgentEvent::turn_started(1), d(0));
        tr.deliver(vec![UserPart::Text("Hi".to_owned())], None);
        tr.apply(
            &AgentEvent::ThinkingDelta {
                index: 0,
                text: "a\nb\nc\nd\ne\nf\ng\nh\n".to_owned(),
            },
            d(10),
        );
        tr.apply(&AgentEvent::BlockStopped { index: 0 }, d(20));
        tr.apply(&ended(outcome, 18_000), d(30));
        let rows = tr.rows(&Renderer::new(l), Detail::Scrollback);
        out.extend(rows.iter().map(line));
    }
    out
}

#[test]
fn every_chrome_and_footer_string_stays_inside_its_glyph_set() {
    use unicode_width::UnicodeWidthChar;
    for s in chrome_strings(GlyphSet::Ascii) {
        assert!(s.is_ascii(), "ASCII set: {s:?}");
    }
    for s in chrome_strings(GlyphSet::AmbiguousWide) {
        for c in s.chars() {
            assert_eq!(
                c.width(),
                c.width_cjk(),
                "ambiguous-wide set: {c:?} in {s:?}"
            );
        }
    }
    let mut unicode: String = UNICODE.all().iter().map(|(_, g)| *g).collect();
    unicode.push_str(down_arrow(&UNICODE));
    unicode.push_str(approx(GlyphSet::Unicode));
    for g in [
        ROUNDED.top_left,
        ROUNDED.top_right,
        ROUNDED.bottom_left,
        ROUNDED.bottom_right,
        ROUNDED.across,
        ROUNDED.down,
    ] {
        unicode.push_str(g);
    }
    for s in chrome_strings(GlyphSet::Unicode) {
        for c in s.chars() {
            assert!(
                c.is_ascii() || unicode.contains(c) || ('\u{2800}'..='\u{28FF}').contains(&c),
                "Unicode set: {c:?} in {s:?}"
            );
        }
    }
}

fn name_call_from_transcript(s: &mut Status, requested: &AgentEvent) {
    let mut t = crate::transcript::Transcript::new(crate::transcript::Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.apply(requested, Duration::ZERO);
    if let AgentEvent::ToolCallRequested { call_id, .. } = requested {
        let doing = t
            .call_doing(call_id, Layout::new(80, GlyphSet::Unicode))
            .unwrap_or_default();
        s.name_call(call_id, &doing);
    }
}

fn tool_requested(id: &str, target: &str) -> AgentEvent {
    crate::events::tool_requested(Requested {
        call_id: id.to_owned(),
        input: serde_json::json!({"command": target}),
        target: target.to_owned(),
        ..Requested::default()
    })
}

#[test]
fn a_running_call_says_its_verb_and_target_with_a_clock() {
    let cx = FULL;
    let mut s = Status::new();
    s.submitted(d(0), cx);
    s.apply(&AgentEvent::turn_started(1), d(10), cx);
    s.apply(&AgentEvent::StepStarted { step: 1 }, d(10), cx);
    s.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::ToolUse {
                writing: None,
                name: "shell".to_owned(),
            },
        },
        d(100),
        cx,
    );
    assert_eq!(s.state(d(100), cx), State::Working);
    assert_eq!(say(&s, 100, cx), "Writing a shell call \u{b7} 0s");
    assert_eq!(say(&s, 2100, cx), "Writing a shell call \u{b7} 2s");
    s.apply(&tool_requested("c", "cargo test"), d(200), cx);
    name_call_from_transcript(&mut s, &tool_requested("c", "cargo test"));
    s.apply(&AgentEvent::call_started("c", "~"), d(1000), cx);
    assert_eq!(say(&s, 1000, cx), "Running cargo test \u{b7} 0s");
    assert_eq!(say(&s, 8500, cx), "Running cargo test \u{b7} 7s");
    let mut named = s.clone();
    named.name_call("d", "Ready · agent continues when process 3 ends");
    named.apply(&AgentEvent::call_started("d", "~"), d(1000), cx);
    let row = line(&named.row(d(8500), cx, &facts(), layout(80)));
    assert!(row.contains(&copy::esc_to_stop().text()), "{row}");
    let next = s.next_change(d(8500), cx).unwrap();
    assert!(next <= d(9000), "{next:?}");
    s.apply(
        &tool_finished(Finished {
            call_id: "c".to_owned(),
            outcome: wisp_protocol::ToolOutcome::Cancelled,
            duration_ms: 8000,
            ..Finished::default()
        }),
        d(9000),
        cx,
    );
    assert_eq!(s.state(d(9000), cx), State::Thinking);
    s.apply(
        &ended(
            TurnOutcome::Cancelled {
                usage: None,
                cost: None,
                committed_steps: 2,
            },
            9000,
        ),
        d(9100),
        cx,
    );
    assert_eq!(
        say(&s, 9100, cx),
        "Stopped. 2 steps were kept: the commands that ran and their output."
    );
}

#[test]
fn the_status_verb_follows_the_calls_action() {
    let cx = FULL;
    let cases = [
        (
            wisp_protocol::ToolAction::Read,
            "docs/spec.pdf",
            "Reading docs/spec.pdf",
        ),
        (
            wisp_protocol::ToolAction::Other {
                name: "echo".to_owned(),
            },
            "hi\nthere",
            "Using echo hi there",
        ),
        (wisp_protocol::ToolAction::Run, "", "Working"),
    ];
    for (action, target, want) in cases {
        let mut s = Status::new();
        s.submitted(d(0), cx);
        s.apply(&AgentEvent::turn_started(1), d(10), cx);
        let requested = crate::events::tool_requested(Requested {
            call_id: "c".to_owned(),
            tool: "look".to_owned(),
            action,
            target: target.to_owned(),
            output: wisp_protocol::OutputKind::None,
            ..Requested::default()
        });
        s.apply(&requested, d(20), cx);
        name_call_from_transcript(&mut s, &requested);
        s.apply(&AgentEvent::call_started("c", "~"), d(1000), cx);
        assert_eq!(say(&s, 3000, cx), format!("{want} \u{b7} 2s"));
    }
}

fn streamed_step(
    status: &mut Status,
    usage: Usage,
    times: [u64; 4],
    output: [AgentEvent; 2],
) -> [(u64, AgentEvent); 5] {
    let [from, first, last, ended] = times;
    let [first_output, last_output] = output;
    let step = status.tally().requests() + 1;
    let events = [
        (from, AgentEvent::StepStarted { step }),
        (first, first_output),
        (last, last_output),
        (
            ended,
            AgentEvent::StepEnded {
                step,
                usage: Some(usage),
                cost_usd: None,
                lower_bound: false,
            },
        ),
        (ended, context_changed(159_000, true)),
    ];
    for (at, event) in &events {
        status.apply(event, d(*at), FULL);
    }
    events
}

#[test]
fn the_rate_averages_request_spans_only_after_the_turn_and_clears_on_send() {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    let row = |s: &Status, at| line(&s.row(d(at), FULL, &facts(), layout(149)));
    let finish = |output_tokens| {
        turn_ended(End {
            usage: Some(Usage {
                output_tokens,
                ..usage()
            }),
            ..End::default()
        })
    };
    streamed_step(
        &mut s,
        Usage {
            output_tokens: 96,
            ..usage()
        },
        [100, 1900, 2100, 9000],
        [text_block(), delta()],
    );
    assert!(!row(&s, 9000).ends_with("tok/s"), "{}", row(&s, 9000));
    s.apply(&AgentEvent::call_started("call", "~"), d(9100), FULL);
    s.apply(
        &tool_finished(Finished {
            call_id: "call".into(),
            ..Finished::default()
        }),
        d(29900),
        FULL,
    );
    streamed_step(
        &mut s,
        Usage {
            output_tokens: 304,
            ..usage()
        },
        [30_000, 37_000, 38_000, 45_000],
        [text_block(), delta()],
    );
    assert!(!row(&s, 45_000).ends_with("tok/s"), "{}", row(&s, 45_000));
    s.apply(&finish(400), d(45_010), FULL);
    for at in [60_000, 90_000] {
        assert!(
            row(&s, at).ends_with("cached 92% \u{b7} 40 tok/s"),
            "{}",
            row(&s, at)
        );
    }
    s.submitted(d(100_000), FULL);
    assert!(!row(&s, 100_000).contains("tok/s"));
    s.apply(&AgentEvent::turn_started(2), d(100_005), FULL);
    streamed_step(
        &mut s,
        Usage {
            output_tokens: 80,
            ..usage()
        },
        [100_010, 103_900, 104_010, 109_000],
        [text_block(), delta()],
    );
    s.apply(&finish(80), d(109_010), FULL);
    assert!(row(&s, 120_000).ends_with("20 tok/s"));
    s.apply(&settings_changed(Changed::default()), d(120_010), FULL);
    assert!(!row(&s, 120_010).contains("tok/s"));
}

#[test]
fn missing_measurements_and_replay_hide_the_rate_but_custom_endpoints_do_not() {
    let mut measured = Status::new();
    measured.submitted(d(0), FULL);
    measured.apply(&AgentEvent::turn_started(1), d(5), FULL);
    let u = Usage {
        output_tokens: 80,
        ..usage()
    };
    let events = streamed_step(
        &mut measured,
        u,
        [100, 2000, 4100, 9000],
        [text_block(), delta()],
    );
    let end = turn_ended(End {
        usage: Some(u),
        ..End::default()
    });
    measured.apply(&end, d(10_000), FULL);
    for endpoint in [Endpoint::Official, Endpoint::Custom] {
        let f = Facts {
            endpoint,
            ..facts()
        };
        let row = line(&measured.row(d(30_000), FULL, &f, layout(149)));
        assert!(row.ends_with("20 tok/s"), "{endpoint:?}: {row}");
    }
    let mut replayed = Status::new();
    for (_, event) in &events {
        replayed.replayed(event);
    }
    replayed.replayed(&end);
    assert!(!line(&replayed.row(d(30_000), FULL, &facts(), layout(149))).contains("tok/s"));
    for ending in [Ending::Cancelled, Ending::Error(ErrorKind::Network)] {
        let mut s = Status::new();
        s.submitted(d(0), FULL);
        s.apply(&AgentEvent::turn_started(1), d(5), FULL);
        let partial = Usage {
            output_tokens: 1,
            ..usage()
        };
        streamed_step(
            &mut s,
            partial,
            [100, 1000, 12_000, 12_100],
            [text_block(), delta()],
        );
        s.apply(
            &turn_ended(End {
                ending,
                usage: Some(partial),
                ..End::default()
            }),
            d(12_200),
            FULL,
        );
        let row = line(&s.row(d(30_000), FULL, &facts(), layout(149)));
        assert!(!row.contains("tok/s"), "{row}");
    }
    for fault in [
        "start", "text", "usage", "end", "span", "zero", "open", "edit", "replay",
    ] {
        let mut s = Status::new();
        s.submitted(d(0), FULL);
        s.apply(&AgentEvent::turn_started(1), d(5), FULL);
        for (mut at, mut event) in events.clone() {
            match (fault, &mut event) {
                ("start", AgentEvent::StepStarted { .. })
                | ("end", AgentEvent::StepEnded { .. }) => continue,
                ("text", AgentEvent::TextDelta { text, .. }) => text.clear(),
                ("usage", AgentEvent::StepEnded { usage, .. }) => *usage = None,
                ("span", _) => at = 100,
                ("zero", AgentEvent::StepEnded { usage: Some(u), .. }) => u.output_tokens = 0,
                _ => {}
            }
            s.apply(&event, d(at), FULL);
        }
        if fault == "open" {
            s.apply(&AgentEvent::StepStarted { step: 2 }, d(9500), FULL);
        }
        if fault == "edit" {
            s.apply(&settings_changed(Changed::default()), d(9200), FULL);
        }
        if matches!(fault, "span" | "edit") {
            streamed_step(&mut s, u, [9300, 9400, 9500, 9600], [text_block(), delta()]);
        }
        s.apply(&end, d(10_000), FULL);
        let mut f = facts();
        if fault == "replay" {
            f.credential = None;
        }
        let row = line(&s.row(d(30_000), FULL, &f, layout(149)));
        assert!(!row.contains("tok/s"), "{fault}: {row}");
    }
}

#[test]
fn late_output_of_every_kind_keeps_reasoning_and_request_wait_in_the_span() {
    let mut output: Vec<_> = [
        BlockKind::Text,
        BlockKind::Thinking,
        BlockKind::RedactedThinking,
        BlockKind::ToolUse {
            name: "tool".into(),
            writing: None,
        },
        BlockKind::Other {
            name: "placeholder".into(),
        },
    ]
    .into_iter()
    .map(|kind| {
        [
            AgentEvent::BlockStarted { index: 1, kind },
            AgentEvent::BlockStopped { index: 1 },
        ]
    })
    .collect();
    output.push([text_block(), delta()]);
    output.push([
        AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 1,
            text: "placeholder".into(),
        },
    ]);
    for output in output {
        let mut s = Status::new();
        s.submitted(d(0), FULL);
        s.apply(&AgentEvent::turn_started(1), d(5), FULL);
        let u = Usage {
            output_tokens: 5200,
            reasoning_tokens: 5000,
            ..usage()
        };
        streamed_step(&mut s, u, [100, 97_000, 100_100, 110_000], output);
        s.apply(
            &turn_ended(End {
                usage: Some(u),
                ..End::default()
            }),
            d(110_010),
            FULL,
        );
        let row = line(&s.row(d(130_000), FULL, &facts(), layout(149)));
        assert!(row.ends_with("52 tok/s"), "{row}");
    }
}

#[test]
fn the_live_speed_counts_the_last_three_seconds_of_reply_text_on_each_tick() {
    let text = |n: usize| AgentEvent::TextDelta {
        index: 1,
        text: "x".repeat(n),
    };
    let speed = |s: &Status, ms: u64| {
        say(s, ms, SIXTEEN)
            .split(" \u{b7} ")
            .find(|fact| fact.ends_with("tok/s"))
            .map(str::to_owned)
    };
    let mut s = Status::new();
    s.submitted(d(0), SIXTEEN);
    s.apply(&AgentEvent::turn_started(1), d(5), SIXTEEN);
    s.apply(&thinking_block(), d(100), SIXTEEN);
    s.apply(
        &AgentEvent::ThinkingDelta {
            index: 1,
            text: "x".repeat(400),
        },
        d(200),
        SIXTEEN,
    );
    assert_eq!(
        say(&s, 1_500, SIXTEEN),
        "Kindling\u{2026} 1s \u{b7} thinking"
    );
    s.apply(&text_block(), d(2_000), SIXTEEN);
    s.apply(&text(80), d(2_200), SIXTEEN);
    assert_eq!(speed(&s, 2_999), None);
    assert_eq!(speed(&s, 3_000).as_deref(), Some("\u{2248}20 tok/s"));
    s.apply(&text(400), d(3_500), SIXTEEN);
    assert_eq!(speed(&s, 3_999).as_deref(), Some("\u{2248}20 tok/s"));
    assert_eq!(
        say(&s, 4_000, SIXTEEN),
        "Kindling\u{2026} 4s \u{b7} \u{2193} \u{2248}120 tokens \u{b7} \u{2248}67 tok/s \u{b7} esc to stop"
    );
    for (ms, shown) in [(5_200, "43"), (6_000, "33"), (7_000, "0")] {
        assert_eq!(
            speed(&s, ms),
            Some(format!("\u{2248}{shown} tok/s")),
            "{ms}"
        );
    }
    s.apply(&tool(2), d(7_100), SIXTEEN);
    assert_eq!(speed(&s, 7_200), None);
    s.name_call("c", "cargo test");
    s.apply(&AgentEvent::call_started("c", "~"), d(7_300), SIXTEEN);
    assert_eq!(say(&s, 8_000, SIXTEEN), "cargo test \u{b7} 0s");
    s.apply(
        &tool_finished(Finished {
            call_id: "c".into(),
            ..Finished::default()
        }),
        d(8_100),
        SIXTEEN,
    );
    assert_eq!(
        say(&s, 8_200, SIXTEEN),
        "Kindling\u{2026} 8s \u{b7} \u{2193} \u{2248}120 tokens \u{b7} thinking"
    );
    s.apply(&text_block(), d(8_300), SIXTEEN);
    s.apply(&text(40), d(8_400), SIXTEEN);
    assert_eq!(speed(&s, 8_999), None);
    assert_eq!(speed(&s, 9_000).as_deref(), Some("\u{2248}10 tok/s"));
    s.apply(&compacting::retrying(), d(9_500), SIXTEEN);
    assert!(say(&s, 10_000, SIXTEEN).starts_with("Retrying in 8 s"));
    assert_eq!(speed(&s, 18_000), None);
    s.apply(&text_block(), d(18_100), SIXTEEN);
    s.apply(&text(40), d(18_200), SIXTEEN);
    assert_eq!(speed(&s, 19_000).as_deref(), Some("\u{2248}10 tok/s"));
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 19_100),
        d(19_100),
        SIXTEEN,
    );
    assert_eq!(speed(&s, 20_000), None);
}

#[test]
fn the_tally_and_request_count_update_from_each_step_and_the_row_shows_no_cost() {
    let cx = FULL;
    let layout = Layout::new(160, GlyphSet::Unicode);
    let mut s = Status::new();
    s.submitted(d(0), cx);
    s.apply(&AgentEvent::turn_started(1), d(10), cx);
    let row = |s: &Status, ms: u64| line(&s.row(d(ms), cx, &facts(), layout));
    assert!(!row(&s, 20).contains('$'), "{}", row(&s, 20));
    let step = |n: u32, usd: f64| AgentEvent::StepEnded {
        step: n,
        usage: Some(usage()),
        cost_usd: Some(usd),
        lower_bound: false,
    };
    s.apply(&step(1, 0.004), d(100), cx);
    let first = row(&s, 100);
    assert!(!first.contains('$'), "{first}");
    assert!(!first.contains("tok/s"), "{first}");
    assert!(!first.contains("request"), "{first}");
    s.apply(&step(2, 0.006), d(200), cx);
    let second = row(&s, 200);
    assert!(second.contains("2 requests"), "{second}");
    assert!(!second.contains('$'), "{second}");
    assert!(!second.contains("tok/s"), "{second}");
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 300),
        d(300),
        cx,
    );
    let after = row(&s, 300);
    assert!(!after.contains('$'), "{after}");
    assert!(!after.contains("request"), "{after}");
    s.submitted(d(400), cx);
    s.apply(&AgentEvent::turn_started(1), d(410), cx);
    s.apply(
        &AgentEvent::StepEnded {
            step: 1,
            usage: None,
            cost_usd: None,
            lower_bound: true,
        },
        d(500),
        cx,
    );
    let lower = row(&s, 500);
    assert!(!lower.contains('$'), "{lower}");
}

#[test]
fn the_status_row_shows_the_effort_and_a_pending_change() {
    let s = Status::new();
    let pending = Facts {
        next: Some(NextTurn {
            endpoint: None,
            model: Some("claude-opus-5-5".to_owned()),
            effort: None,
            cache_ttl: None,
        }),
        ..facts()
    };
    let row = line(&s.row(d(0), FULL, &pending, layout(100)));
    assert!(
        row.contains("Claude API   \u{2192} opus-5-5 next turn \u{b7} sonnet-5-5 low"),
        "{row}"
    );
    let both = Facts {
        next: Some(NextTurn {
            endpoint: None,
            model: Some("claude-opus-5-5".to_owned()),
            effort: Some(wisp_protocol::Effort::Max),
            cache_ttl: None,
        }),
        ..facts()
    };
    let ascii = Layout::new(100, GlyphSet::Ascii);
    let row = line(&s.row(d(0), ASCII, &both, ascii));
    assert!(row.contains("-> opus-5-5 - max next turn"), "{row}");
    let none = Facts {
        next: Some(NextTurn::default()),
        ..facts()
    };
    assert_eq!(none.pending(&UNICODE), None);
}

#[test]
fn done_names_an_expected_rebuild() {
    let mut s = Status::new();
    let switch = settings_changed(Changed {
        endpoint: "env".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        cache: wisp_protocol::CacheEffect::Rebuilt {
            reason: wisp_protocol::RebuildReason::ModelSwitch,
        },
        ..Changed::default()
    });
    s.apply(&switch, d(0), FULL);
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    s.apply(
        &AgentEvent::CacheMiss {
            previous_prompt_tokens: 9_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 9_000,
            diagnostics: None,
            cause: None,
        },
        d(6),
        FULL,
    );
    s.apply(&ended(completed(StopReason::EndTurn, true), 1), d(10), FULL);
    let said = say(&s, 20, FULL);
    assert!(said.ends_with("cache rebuilt: model switched"), "{said}");
}

fn menu() -> Menu {
    Menu {
        titles: [
            "Model".to_owned(),
            "$/MTok in \u{b7} out".to_owned(),
            "next turn".to_owned(),
        ],
        choices: vec![
            Choice {
                mark: None,
                current: true,
                label: "opus-5-5".to_owned(),
                detail: "$4 \u{b7} $20".to_owned(),
                effect: "current".to_owned(),
                enabled: true,
                line: None,
            },
            Choice {
                mark: None,
                current: false,
                label: "opus-4-5\u{1b}[2J".to_owned(),
                detail: "$5 \u{b7} $25".to_owned(),
                effect: "not supported yet (no adaptive thinking)".to_owned(),
                enabled: false,
                line: None,
            },
        ],
        selected: Some(0),
        notes: vec![("a warning".to_owned(), Tone::Note)],
        shape: Shape::default(),
    }
}

#[test]
fn current_is_a_check_after_its_name_with_or_without_the_band() {
    let mut m = menu();
    m.selected = Some(1);
    for w in [40, 80, 120, 200] {
        let rows = menu_rows(&m, 12, layout(w));
        let current = &rows[1];
        assert!(
            line(current).starts_with("   opus-5-5 \u{2713}"),
            "{current:?}"
        );
        let check = current.spans.iter().find(|s| s.text == "\u{2713}");
        assert_eq!(check.and_then(|s| s.look.fg), Some(Token::Accent.into()));
        crate::chrome::band::check::assert_unselected(current);
        crate::chrome::band::check::assert_selected(&rows[2], w, "\u{203A}");
    }
    m.selected = Some(0);
    for w in [40, 80, 120, 200] {
        let rows = menu_rows(&m, 12, layout(w));
        let both = &rows[1];
        crate::chrome::band::check::assert_selected(both, w, "\u{203A}");
        assert!(line(both).starts_with(" \u{203A} opus-5-5 \u{2713}"));
        let dot = both.spans.iter().find(|s| s.text == "\u{2713}").unwrap();
        assert_eq!(dot.look.fg, Some(Token::Accent.into()));
        assert_eq!(dot.look.bg, Some(Token::AccentTint.into()));
        crate::chrome::band::check::assert_unselected(&rows[2]);
    }
}

#[test]
fn a_menu_draws_aligned_full_width_rows_and_dims_disabled_choices() {
    let rows = menu_rows(&menu(), 12, layout(80));
    let texts: Vec<String> = rows.iter().map(line).collect();
    assert_eq!(texts.len(), 4);
    assert!(texts.iter().all(|t| t.chars().count() == 80), "{texts:#?}");
    assert_eq!(
        texts[0].trim_end(),
        "   Model          $/MTok in \u{b7} out  next turn"
    );
    assert_eq!(
        texts[1].trim_end(),
        " \u{203A} opus-5-5 \u{2713}     $4 \u{b7} $20         current"
    );
    assert!(texts[2].starts_with("   opus-4-5^[[2J  $5"), "{}", texts[2]);
    assert!(!texts.concat().contains('\u{1b}'));
    assert_eq!(texts[3].trim_end(), "   a warning");
    let selected = &rows[1].spans[3];
    assert_eq!(selected.text, "opus-5-5");
    assert_eq!(selected.look.fg, Some(Token::Text.into()));
    assert_eq!(selected.look.bg, Some(Token::AccentTint.into()));
    let dim = rows[2]
        .spans
        .iter()
        .find(|s| s.text.starts_with("opus-4-5"))
        .unwrap();
    assert_eq!(dim.look.fg, Some(Token::TextFaint.into()));
    assert_eq!(rows[3].spans[1].look.fg, Some(Token::Text.into()));
    let ascii = menu_rows(&menu(), 12, Layout::new(80, GlyphSet::Ascii));
    assert!(line(&ascii[1]).starts_with(" > opus-5-5 +"));
}

#[test]
fn a_choice_with_only_a_label_spans_the_row_without_widening_the_label_column() {
    let mut m = menu();
    m.choices.push(Choice {
        label: "+ Add an API key with a long label\u{2026}".to_owned(),
        enabled: true,
        ..Choice::default()
    });
    let texts: Vec<String> = menu_rows(&m, 12, layout(80)).iter().map(line).collect();
    assert_eq!(
        texts[0].trim_end(),
        "   Model          $/MTok in \u{b7} out  next turn"
    );
    assert_eq!(
        texts[3].trim_end(),
        "   + Add an API key with a long label\u{2026}"
    );
}

#[test]
fn a_menu_fits_the_room_it_is_given_and_keeps_the_selection_in_view() {
    let mut m = menu();
    m.choices = (0..20)
        .map(|i| Choice {
            label: format!("model-{i}"),
            enabled: true,
            ..Choice::default()
        })
        .collect();
    m.selected = Some(19);
    let rows = menu_rows(&m, 40, layout(80));
    assert_eq!(rows.len(), MENU_MAX_ROWS);
    let texts: Vec<String> = rows.iter().map(line).collect();
    assert!(texts[MENU_MAX_ROWS - 2].contains("model-19"), "{texts:#?}");
    assert!(texts[MENU_MAX_ROWS - 1].contains("a warning"));
    let small = menu_rows(&m, 3, layout(30));
    assert_eq!(small.len(), 3);
    assert!(small.iter().all(|r| line(r).chars().count() == 30));
    assert!(menu_rows(&m, 0, layout(80)).is_empty());
}

#[test]
fn menu_lines_name_the_choice_on_each_line_and_the_selected_line() {
    let mut m = menu();
    m.choices = (0..20)
        .map(|i| Choice {
            label: format!("model-{i}"),
            enabled: true,
            ..Choice::default()
        })
        .collect();
    m.selected = Some(15);
    let rows = menu_lines(&m, 40, layout(80));
    assert_eq!(rows.lines, menu_rows(&m, 40, layout(80)));
    assert_eq!(rows.hits.len(), rows.lines.len());
    assert_eq!(rows.hits[0], None);
    assert_eq!(rows.hits[MENU_MAX_ROWS - 1], None);
    let shown: Vec<usize> = rows.hits.iter().flatten().copied().collect();
    assert_eq!(shown, (6..16).collect::<Vec<_>>());
    let at = rows.selected.unwrap();
    assert_eq!(rows.hits[at], Some(15));
    assert!(line(&rows.lines[at]).contains("model-15"));
    m.selected = None;
    let rows = menu_lines(&m, 40, layout(80));
    assert_eq!(rows.selected, None);
    assert_eq!(rows.hits[1], Some(0));
    assert_eq!(menu_lines(&m, 0, layout(80)), SuggestRows::default());
}

#[test]
fn a_menu_with_no_choices_keeps_its_notes() {
    let mut m = menu();
    m.choices.clear();
    m.selected = None;
    let texts: Vec<String> = menu_rows(&m, 12, layout(80)).iter().map(line).collect();
    assert_eq!(texts.len(), 2, "{texts:#?}");
    assert_eq!(texts[1].trim_end(), "   a warning");
    let rows = menu_lines(&m, 12, layout(80));
    assert_eq!(rows.hits, [None, None]);
    assert_eq!(menu_rows(&m, 1, layout(80)).len(), 1);
}

#[test]
fn a_narrow_menu_drops_the_price_column_before_clipping_the_effect() {
    let mut m = menu();
    m.choices[1].effect = "cache rebuild ~$0.20".to_owned();
    let wide: Vec<String> = menu_rows(&m, 12, layout(60)).iter().map(line).collect();
    assert!(wide[1].contains("$4 \u{b7} $20"), "{wide:#?}");
    let narrow: Vec<String> = menu_rows(&m, 12, layout(40)).iter().map(line).collect();
    assert_eq!(narrow[0].trim_end(), "   Model          next turn");
    assert_eq!(
        narrow[2].trim_end(),
        "   opus-4-5^[[2J  cache rebuild ~$0.20"
    );
    assert!(!narrow.concat().contains("$/MTok"), "{narrow:#?}");
}

#[test]
fn a_tight_menu_cuts_the_middle_column_short_before_it_drops_it() {
    let long = "cache rebuild ~$0.04 \u{b7} web search: native \u{2192} none";
    let m = Menu {
        titles: [
            "Account".to_owned(),
            "credential".to_owned(),
            "next turn".to_owned(),
        ],
        choices: vec![
            Choice {
                mark: None,
                current: true,
                label: "direct".to_owned(),
                detail: "Anthropic API key".to_owned(),
                effect: "current".to_owned(),
                enabled: true,
                line: None,
            },
            Choice {
                mark: None,
                current: false,
                label: "manifest".to_owned(),
                detail: "Custom Gateway \u{b7} gw.example.com".to_owned(),
                effect: long.to_owned(),
                enabled: true,
                line: None,
            },
        ],
        selected: Some(0),
        notes: Vec::new(),
        shape: Shape::default(),
    };
    let rows: Vec<String> = menu_rows(&m, 12, layout(80)).iter().map(line).collect();
    assert_eq!(
        rows[0].trim_end(),
        "   Account   credential         next turn"
    );
    assert_eq!(
        rows[1].trim_end(),
        " \u{203A} direct \u{2713}  Anthropic API key  current"
    );
    assert_eq!(
        rows[2].trim_end(),
        format!("   manifest  Custom Gateway\u{2026}    {long}")
    );
    let ascii: Vec<String> = menu_rows(&m, 12, Layout::new(80, GlyphSet::Ascii))
        .iter()
        .map(line)
        .collect();
    assert!(
        ascii[2].starts_with("   manifest  Custom Gateway...  cache"),
        "{ascii:#?}"
    );
    let full: Vec<String> = menu_rows(&m, 12, layout(120)).iter().map(line).collect();
    assert!(
        full[2].contains("Custom Gateway \u{b7} gw.example.com  cache"),
        "{full:#?}"
    );
    let dropped: Vec<String> = menu_rows(&m, 12, layout(70)).iter().map(line).collect();
    assert_eq!(dropped[0].trim_end(), "   Account   next turn");
    assert_eq!(dropped[2].trim_end(), format!("   manifest  {long}"));

    let mut prices = menu();
    prices.choices[1].effect = "x".repeat(40);
    let tight: Vec<String> = menu_rows(&prices, 12, layout(74))
        .iter()
        .map(line)
        .collect();
    assert_eq!(
        tight[0].trim_end(),
        "   Model          $/MTok in \u{b7} out next turn"
    );
    assert!(
        tight[2].starts_with("   opus-4-5^[[2J  $5 \u{b7} $25        xxx"),
        "{tight:#?}"
    );
}

fn gateway(host: &str) -> Facts {
    Facts {
        credential: Some(Credential::AuthToken),
        endpoint: Endpoint::Custom,
        host: Some(host.to_owned()),
        ..facts()
    }
}

fn tips() -> Vec<KeyHint> {
    vec![
        KeyHint::new("enter", "send"),
        KeyHint::new("ctrl-j", "new line"),
        KeyHint::note("/model"),
        KeyHint::note("/help"),
    ]
}

fn panel(f: &Facts, w: usize, set: GlyphSet, t: u64, cx: MotionCtx) -> Vec<String> {
    welcome(f, &tips(), d(t), cx, Layout::new(w, set))
        .iter()
        .map(line)
        .collect()
}

fn detailed(f: &Facts, w: usize, set: GlyphSet) -> Vec<String> {
    welcome_in_detail(f, FULL, Layout::new(w, set))
        .iter()
        .map(line)
        .collect()
}

#[test]
fn the_credential_label_follows_the_table_row_by_row() {
    let full = |f: &Facts| f.credential_label(usize::MAX, &UNICODE, Default::default());
    let key = facts();
    assert_eq!(full(&key), "Claude API");
    let token = Facts {
        credential: Some(Credential::AuthToken),
        ..facts()
    };
    assert_eq!(full(&token), "Claude API");
    for credential in [Credential::ApiKey, Credential::AuthToken] {
        let f = Facts {
            credential: Some(credential),
            ..gateway("gw.example.com")
        };
        assert_eq!(full(&f), "Gateway \u{00B7} gw.example.com");
    }
    let no_host = Facts {
        host: None,
        ..gateway("x")
    };
    assert_eq!(full(&no_host), "Gateway");
    let blank_host = gateway("  ");
    assert_eq!(full(&blank_host), "Gateway");
    let replay = Facts {
        credential: None,
        ..gateway("gw.example.com")
    };
    assert_eq!(full(&replay), "replay, no API calls");
    assert_eq!(replay.credential(), None);
    let row = line(&Status::new().row(d(0), FULL, &replay, layout(120)));
    assert!(row.ends_with("sonnet-5-5 low"), "{row}");
    assert!(!row.contains("replay") && !row.contains("Gateway"), "{row}");
}

#[test]
fn the_host_narrows_then_drops_before_any_other_fact() {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    s.apply(&ended(completed(StopReason::EndTurn, true), 1), d(10), FULL);
    let host = "gateway.internal.example.com:8443";
    let f = gateway(host);
    let at = d(5000);
    let mut shortened = vec![];
    let mut dropped_at = None;
    for w in (20..=140).rev() {
        let row = line(&s.row(at, FULL, &f, layout(w)));
        assert!(width_of(&row) < w, "{w}: {row}");
        let parts: Vec<&str> = row.split(" \u{00B7} ").collect();
        let Some(i) = parts.iter().position(|p| p.contains("Gateway")) else {
            assert!(dropped_at.is_some(), "{w}: {row}");
            continue;
        };
        let next = parts
            .get(i + 1)
            .filter(|_| parts[i].ends_with("Gateway"))
            .map(|part| part.split("   ").next().unwrap_or(part));
        match next {
            Some(h) if h == host => {
                assert!(row.contains("cached 92%"), "{row}");
            }
            Some(h) if h.contains('\u{2026}') => {
                assert!(width_of(h) >= HOST_MIN, "{w}: {h}");
                assert!(h.starts_with("gatew") && h.ends_with(":8443"), "{h}");
                assert!(row.contains("cached 92%"), "{row}");
                assert!(dropped_at.is_none(), "{w}: {row}");
                shortened.push(h.to_owned());
            }
            _ => {
                if dropped_at.is_none() {
                    dropped_at = Some(w);
                    assert!(row.contains("cached 92%"), "{row}");
                }
            }
        }
    }
    assert!(shortened.len() > 5, "{shortened:?}");
    assert!(dropped_at.is_some());
    let short = gateway("gw.example.com");
    let row = line(&s.row(at, FULL, &short, layout(120)));
    assert!(
        row.ends_with("Gateway \u{00B7} gw.example.com   sonnet-5-5 low \u{00B7} cached 92%"),
        "{row}"
    );
}

#[test]
fn a_label_fits_its_room_with_a_middle_ellipsis() {
    let label = CredentialLabel {
        endpoint: None,
        name: "Custom Gateway",
        host: Some("gw.example.com".to_owned()),
    };
    let fit = |room| label.fit(room, &UNICODE, Default::default());
    assert_eq!(fit(100), "Custom Gateway \u{00B7} gw.example.com");
    assert_eq!(fit(31), "Custom Gateway \u{00B7} gw.example.com");
    assert_eq!(fit(30), "Custom Gateway \u{00B7} gw.exa\u{2026}le.com");
    assert_eq!(fit(29), "Custom Gateway \u{00B7} gw.exa\u{2026}e.com");
    assert_eq!(fit(28), "Custom Gateway");
    assert_eq!(fit(3), "Custom Gateway");
    let ascii = label.fit(30, &Glyphs::of(GlyphSet::Ascii), Default::default());
    assert_eq!(ascii, "Custom Gateway - gw.ex...e.com");
}

fn endpoint(name: &str, unavailable: bool) -> EndpointFacts {
    EndpointFacts {
        models: Vec::new(),
        mark: crate::chrome::account::mockup_mark(name),
        name: name.to_owned(),
        provider: "anthropic".to_owned(),
        credential: Some(Credential::ApiKey),
        reach: Some(Endpoint::Official),
        host: None,
        unavailable: unavailable.then(|| "K is not set".to_owned()),
        priced: true,
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }
}

fn named(name: &str, endpoints: &[(&str, bool)], base: Facts) -> Facts {
    Facts {
        endpoint_name: Some(name.to_owned()),
        endpoints: endpoints.iter().map(|(n, u)| endpoint(n, *u)).collect(),
        ..base
    }
}

#[test]
fn the_label_names_the_endpoint_per_the_table() {
    let full = |f: &Facts| f.credential_label(usize::MAX, &UNICODE, Default::default());
    assert_eq!(
        full(&named("claude-api", &[("claude-api", false)], facts())),
        "Claude API"
    );
    assert_eq!(
        full(&named(
            "claude-api",
            &[("claude-api", false), ("work", true)],
            facts()
        )),
        "Claude API"
    );
    assert_eq!(
        full(&named(
            "gateway",
            &[("gateway", false)],
            gateway("gw2.example.com")
        )),
        "Gateway \u{00B7} gw2.example.com"
    );
    assert_eq!(
        full(&named(
            "gateway",
            &[("gateway", false), ("direct", false)],
            gateway("gw2.example.com")
        )),
        "gateway \u{00B7} Gateway \u{00B7} gw2.example.com"
    );
    assert_eq!(
        full(&named(
            "manifest",
            &[("manifest", false)],
            gateway("gw.example.com")
        )),
        "manifest \u{00B7} Gateway \u{00B7} gw.example.com"
    );
    assert_eq!(
        full(&named(
            "direct",
            &[("direct", false), ("manifest", false)],
            facts()
        )),
        "direct \u{00B7} Claude API"
    );
    let token = Facts {
        credential: Some(Credential::AuthToken),
        ..facts()
    };
    assert_eq!(
        full(&named("direct", &[("direct", false)], token)),
        "direct \u{00B7} Claude API"
    );
    let hostile = named(
        "a\u{1b}[31mb\nc",
        &[("x", false), ("y", false)],
        gateway("h\u{1b}]0;x\u{7}.com"),
    );
    let shown = full(&hostile);
    assert!(
        !shown.contains('\u{1b}') && !shown.contains('\n') && !shown.contains('\u{7}'),
        "{shown:?}"
    );
    assert_eq!(hostile.account_chip("a\u{1b}[31mb\nc").name, "a^[[31mb c");
}

#[test]
fn a_named_label_narrows_host_then_kind_never_the_name() {
    let label = CredentialLabel {
        endpoint: Some("manifest".to_owned()),
        name: "Custom Gateway",
        host: Some("gw.example.com".to_owned()),
    };
    let fit = |room| label.fit(room, &UNICODE, Default::default());
    assert_eq!(
        fit(100),
        "manifest \u{00B7} Custom Gateway \u{00B7} gw.example.com"
    );
    assert_eq!(
        fit(42),
        "manifest \u{00B7} Custom Gateway \u{00B7} gw.example.com"
    );
    assert_eq!(
        fit(41),
        "manifest \u{00B7} Custom Gateway \u{00B7} gw.exa\u{2026}le.com"
    );
    assert_eq!(
        fit(40),
        "manifest \u{00B7} Custom Gateway \u{00B7} gw.exa\u{2026}e.com"
    );
    assert_eq!(fit(39), "manifest \u{00B7} Custom Gateway");
    assert_eq!(fit(25), "manifest \u{00B7} Custom Gateway");
    assert_eq!(fit(24), "manifest");
    assert_eq!(fit(3), "manifest");
    let official = CredentialLabel {
        endpoint: Some("direct".to_owned()),
        name: "Anthropic API key",
        host: None,
    };
    assert_eq!(
        official.fit(100, &UNICODE, Default::default()),
        "direct \u{00B7} Anthropic API key"
    );
    assert_eq!(
        official.fit(26, &UNICODE, Default::default()),
        "direct \u{00B7} Anthropic API key"
    );
    assert_eq!(official.fit(25, &UNICODE, Default::default()), "direct");
    let ascii = label.fit(41, &Glyphs::of(GlyphSet::Ascii), Default::default());
    assert_eq!(ascii, "manifest - Custom Gateway - gw.ex...e.com");
}

#[test]
fn the_welcome_panel_names_the_endpoint_and_shows_the_override_note() {
    let f = Facts {
        model: "claude-opus-5-5".to_owned(),
        effort: "medium".to_owned(),
        endpoint_note: Some("Using ANTHROPIC_API_KEY from the environment, not the default account \"manifest\" (--account manifest to use it)".to_owned()),
        resume_refused: false,
        ..named("manifest", &[("manifest", false), ("direct", false)], gateway("gw.example.com"))
    };
    let plain = panel(&f, 120, GlyphSet::Unicode, SETTLED, FULL).join("\n");
    assert!(
        plain.contains("manifest · API · opus-5-5 medium"),
        "{plain}"
    );
    assert!(!plain.contains("Using ANTHROPIC_API_KEY"), "{plain}");
    let rows = detailed(&f, 120, GlyphSet::Unicode);
    let text = rows.join("\n");
    let place = rows
        .iter()
        .position(|r| r.contains("~/Projects/Wisp"))
        .unwrap();
    let log = rows.iter().position(|r| r.contains("logging to")).unwrap();
    assert_eq!(log, place + 1, "{text}");
    assert!(
        rows[log + 1].contains("Using ANTHROPIC_API_KEY from the environment"),
        "{text}"
    );
    let banner_rows: Vec<String> = banner_with(&f, d(0), FULL, layout(200), true)
        .iter()
        .map(line)
        .collect();
    assert!(
        banner_rows
            .iter()
            .any(|r| r.contains("not the default account")),
        "{banner_rows:#?}"
    );
}

fn searching(form: SearchForm, chat_only: bool) -> SearchForms {
    SearchForms {
        requested: true,
        chat_only,
        pairs: vec![SearchPair {
            endpoint: "manifest".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            form,
        }],
        refused: Vec::new(),
        kept: None,
    }
}

#[test]
fn the_welcome_panel_says_why_the_pair_cannot_search() {
    let untrusted = SearchForm::None(NoSearch::TurnedOff);
    let on = |search: SearchForms| Facts {
        model: "claude-opus-5-5".to_owned(),
        effort: "medium".to_owned(),
        search,
        ..named(
            "manifest",
            &[("manifest", false), ("direct", false)],
            gateway("gw.example.com"),
        )
    };
    let why = "web search: off (off for this account)";
    let f = on(searching(untrusted, false));
    let rows = panel(&f, 120, GlyphSet::Unicode, SETTLED, FULL);
    let place = rows
        .iter()
        .position(|r| r.contains("~/Projects/Wisp"))
        .unwrap();
    assert!(
        rows[place + 1].contains("web search off · ctrl-o for why"),
        "{rows:#?}"
    );
    assert!(!rows.join("\n").contains(why), "{rows:#?}");
    let rows = detailed(&f, 120, GlyphSet::Unicode);
    let text = rows.join("\n");
    let log = rows.iter().position(|r| r.contains("logging to")).unwrap();
    assert_eq!(log, place + 1, "{text}");
    assert!(rows[log + 1].contains(why), "{text}");
    let banner_rows: Vec<String> = banner_with(&f, d(0), FULL, layout(200), true)
        .iter()
        .map(line)
        .collect();
    assert!(
        banner_rows.iter().any(|r| r.contains(why)),
        "{banner_rows:#?}"
    );
    let chat = on(searching(untrusted, true));
    let card = panel(&chat, 120, GlyphSet::Unicode, SETTLED, FULL).join("\n");
    assert!(
        card.contains("web search off · chat only · ctrl-o for why"),
        "{card}"
    );
    let card = panel(&chat, 120, GlyphSet::Ascii, SETTLED, ASCII).join("\n");
    assert!(
        card.contains("web search off - chat only - ctrl-o for why"),
        "{card}"
    );
    let card = panel(&f, 120, GlyphSet::Unicode, SETTLED, FULL).join("\n");
    assert!(!card.contains("chat only"), "{card}");
    let rows = detailed(&chat, 120, GlyphSet::Ascii);
    let at = rows.iter().position(|r| r.contains(why)).unwrap();
    assert!(rows[at].contains(" - chat only "), "{rows:#?}");
    for quiet in [
        searching(SearchForm::Native, true),
        SearchForms {
            requested: false,
            ..searching(untrusted, true)
        },
    ] {
        let text = panel(&on(quiet.clone()), 120, GlyphSet::Unicode, SETTLED, FULL).join("\n");
        assert!(!text.contains("web search"), "{text}");
        let text = detailed(&on(quiet), 120, GlyphSet::Unicode).join("\n");
        assert!(!text.contains("web search"), "{text}");
    }
}

#[test]
fn a_resumed_session_says_so_in_the_panel_and_the_banner() {
    let resumed = "Resumed session 01a167d6 (18 turns, $5.15, last active 2 h ago).";
    let f = Facts {
        model: "claude-opus-5-5".to_owned(),
        effort: "medium".to_owned(),
        resumed: Some(resumed.to_owned()),
        ..named(
            "manifest",
            &[("manifest", false)],
            gateway("gw.example.com"),
        )
    };
    let rows = panel(&f, 120, GlyphSet::Unicode, 280, FULL);
    let text = rows.join("\n");
    assert!(rows.iter().any(|r| r.contains(resumed)), "{text}");
    let banner_rows: Vec<String> = banner(&f, d(0), FULL, layout(200))
        .iter()
        .map(line)
        .collect();
    assert!(
        banner_rows.iter().any(|r| r.contains(resumed)),
        "{banner_rows:#?}"
    );
    let fresh = Facts { resumed: None, ..f };
    let text = panel(&fresh, 120, GlyphSet::Unicode, 280, FULL).join("\n");
    assert!(!text.contains("Resumed"), "{text}");
}

#[test]
fn the_welcome_panel_matches_the_plan_at_80_columns() {
    let f = Facts {
        model: "claude-opus-5-5".to_owned(),
        effort: "medium".to_owned(),
        ..gateway("gw.example.com")
    };
    let rows = panel(&f, 80, GlyphSet::Unicode, SETTLED, FULL);
    let name = format!(
        "    \u{2850}\u{2808} \u{2801}\u{2801}     wisp{}0.1.0",
        " ".repeat(62 - 4 - 5)
    );
    let inside = [
        "",
        &name,
        "   \u{2830}  \u{28BE}\u{2877}  \u{2806}   Your own AI. With a computer of its own.",
        "    \u{2821}\u{2880} \u{2840}\u{2840}\u{280C}    opus-5-5 medium",
        "              ~/Projects/Wisp · Gateway · gw.example.com",
        "",
    ];
    let mut expect = vec![format!("\u{256D}{}\u{256E}", "\u{2500}".repeat(77))];
    for row in inside {
        let pad = 77 - width_of(row);
        expect.push(format!("\u{2502}{row}{}\u{2502}", " ".repeat(pad)));
    }
    expect.push(format!("\u{2570}{}\u{256F}", "\u{2500}".repeat(77)));
    expect.push("  enter send · ctrl-j new line · /model · /help".to_owned());
    assert_eq!(rows, expect, "{rows:#?}");
    let first = welcome(&f, &tips(), d(SETTLED), FULL, layout(80));
    assert_eq!(first[0].spans[0].look.fg, Some(Ink::Token(Token::Line)));
    let look = |row: usize, text: &str| {
        first[row]
            .spans
            .iter()
            .find(|s| s.text == text)
            .map(|s| (s.look.fg, s.look.bold))
    };
    let text = Some(Ink::Token(Token::Text));
    let muted = Some(Ink::Token(Token::TextMuted));
    assert_eq!(look(2, "wisp"), Some((text, true)));
    assert_eq!(
        look(2, "0.1.0"),
        Some((Some(Ink::Token(Token::TextFaint)), false))
    );
    assert_eq!(
        look(2, "\u{2850}").map(|l| l.0),
        Some(Some(Ink::Token(Token::OrbRing)))
    );
    assert_eq!(
        look(3, "\u{28BE}").map(|l| l.0),
        Some(Some(Ink::Token(Token::Accent)))
    );
    assert_eq!(look(3, "Your own AI. "), Some((muted, false)));
    assert_eq!(look(3, "With a computer of its own."), Some((text, true)));
    assert_eq!(look(4, "opus-5-5 medium"), Some((text, false)));
    assert_eq!(
        look(5, "~/Projects/Wisp · Gateway · gw.example.com"),
        Some((muted, false))
    );
    let keys = &first[first.len() - 1];
    let ink = |text: &str| {
        keys.spans
            .iter()
            .find(|s| s.text == text)
            .map(|s| s.look.fg)
    };
    assert_eq!(ink("enter"), Some(text));
    assert_eq!(ink(" send"), Some(muted));
    assert_eq!(ink("/help"), Some(text));
    assert_eq!(ink(" · "), Some(muted));
    let detail: Vec<String> = welcome_in_detail(&f, FULL, layout(80))
        .iter()
        .map(line)
        .collect();
    assert_eq!(detail.len(), rows.len());
    assert!(
        detail[6].contains("cache 1h · logging to ~/.local/share/wisp/sessions/0193.jsonl"),
        "{detail:#?}"
    );
    assert!(
        !rows
            .iter()
            .any(|r| r.contains("cache") || r.contains("logging")),
        "{rows:#?}"
    );
}

#[test]
fn the_key_line_drops_whole_keys_that_do_not_fit() {
    let at = |w: usize, set: GlyphSet| -> String {
        key_line(&tips(), Layout::new(w, set))
            .map(|l| line(&l))
            .unwrap_or_default()
    };
    assert_eq!(
        at(80, GlyphSet::Unicode),
        "  enter send · ctrl-j new line · /model · /help"
    );
    assert_eq!(
        at(80, GlyphSet::Ascii),
        "  enter send - ctrl-j new line - /model - /help"
    );
    assert_eq!(
        at(40, GlyphSet::Unicode),
        "  enter send · ctrl-j new line · /help"
    );
    assert_eq!(at(30, GlyphSet::Unicode), "  enter send · /help");
    assert_eq!(at(10, GlyphSet::Unicode), "  /help");
    assert_eq!(at(5, GlyphSet::Unicode), "  /\u{2026}");
    assert_eq!(key_line(&[], layout(80)), None);
    let row = key_line(&tips(), layout(80)).unwrap();
    assert_eq!(row.content.map(|c| c.from), Some(2));
}

#[test]
fn the_box_is_one_column_short_of_the_width_up_to_100() {
    let f = gateway("gw.example.com");
    for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
        for w in [44, 50, 59, 60, 61, 80, 101, 120, 200] {
            let rows = panel(&f, w, set, SETTLED, FULL);
            let want = (w - 1).min(WELCOME_MAX);
            let mode = Layout::new(w, set).width_mode;
            let (keys, card) = rows.split_last().unwrap();
            for r in card {
                assert_eq!(crate::text::width(r, mode), want, "{w} {set:?}: {r:?}");
            }
            assert!(
                crate::text::width(keys, mode) <= want,
                "{w} {set:?}: {keys:?}"
            );
        }
    }
}

#[test]
fn the_panel_degrades_by_width_and_glyph_set() {
    let f = gateway("gw.example.com");
    let wide = panel(&f, 80, GlyphSet::Unicode, SETTLED, FULL);
    assert!(wide[2].contains("\u{2850}\u{2808}"), "{wide:#?}");
    let mid = panel(&f, 50, GlyphSet::Unicode, SETTLED, FULL);
    assert_eq!(mid[0].chars().next(), Some('\u{256D}'));
    assert!(
        mid[2].starts_with("│   \u{288E}\u{2836}\u{2870} wisp "),
        "{mid:#?}"
    );
    assert!(
        mid[2].trim_end_matches('│').trim_end().ends_with("0.1.0"),
        "{mid:#?}"
    );
    assert!(mid.iter().all(|r| !r.contains('\u{2850}')), "{mid:#?}");
    assert!(mid.iter().all(|r| !r.contains("logging to")), "{mid:#?}");
    let mid_detail: Vec<String> = welcome_in_detail(&f, FULL, layout(50))
        .iter()
        .map(line)
        .collect();
    assert!(
        mid_detail.iter().any(|r| r.contains("logging to")),
        "{mid_detail:#?}"
    );
    let edge = panel(&f, 44, GlyphSet::Unicode, SETTLED, FULL);
    assert_eq!(edge[0].chars().next(), Some('\u{256D}'), "{edge:#?}");
    assert!(edge.iter().any(|r| r.contains("Your own AI.")), "{edge:#?}");
    let narrow = panel(&f, 43, GlyphSet::Unicode, SETTLED, FULL);
    assert_ne!(narrow[0].chars().next(), Some('\u{256D}'), "{narrow:#?}");
    assert!(
        narrow.iter().all(|r| !r.contains("Your own AI.")),
        "{narrow:#?}"
    );
    let banner_rows: Vec<String> = banner(&f, d(280), FULL, layout(43))
        .iter()
        .map(line)
        .collect();
    assert_eq!(narrow[..narrow.len() - 1], banner_rows);
    assert!(
        narrow[narrow.len() - 1].starts_with("  enter send"),
        "{narrow:#?}"
    );
    assert!(
        narrow.iter().all(|r| !r.contains('\u{2502}')),
        "{narrow:#?}"
    );
    let ascii = panel(&f, 80, GlyphSet::Ascii, 0, ASCII);
    assert!(ascii[0].starts_with("+---") && ascii[0].ends_with("---+"));
    assert_eq!(&ascii[2][..15], "|    .-''  .   ");
    assert_eq!(&ascii[3][..15], "|   :  **  :   ");
    assert_eq!(&ascii[4][..15], "|    '-..-'    ");
    assert!(ascii.iter().all(|r| r.is_ascii()), "{ascii:#?}");
    assert!(ascii[5].contains("Gateway - gw.example.com"), "{ascii:#?}");
    let ambiguous = panel(&f, 80, GlyphSet::AmbiguousWide, SETTLED, FULL);
    assert!(ambiguous[0].starts_with("+---"), "{ambiguous:#?}");
    assert!(ambiguous[1].starts_with('|'), "{ambiguous:#?}");
    assert!(ambiguous[2].contains("\u{2850}\u{2808}"), "{ambiguous:#?}");
}

const SETTLED: u64 = 60_000;

fn shape(cells: &SplashCells) -> Vec<String> {
    cells
        .iter()
        .map(|row| row.iter().map(|c| c.map_or(' ', |c| c.ch)).collect())
        .collect()
}

fn dots(cells: &SplashCells) -> Vec<u32> {
    cells
        .iter()
        .flatten()
        .map(|c| c.map_or(0, |c| u32::from(c.ch) - 0x2800))
        .collect()
}

fn glow_of(ink: Ink) -> f64 {
    match ink {
        Ink::Mix { u, .. } => u.get(),
        Ink::Token(_) => 0.0,
    }
}

fn glows(cells: &SplashCells) -> Vec<f64> {
    cells
        .iter()
        .enumerate()
        .flat_map(|(r, row)| {
            row.iter().enumerate().map(move |(c, cell)| match cell {
                Some(cell) if !(r == 1 && (3..=4).contains(&c)) => glow_of(cell.ink),
                _ => 0.0,
            })
        })
        .collect()
}

fn biggest_jump(a: &SplashCells, b: &SplashCells) -> f64 {
    glows(a)
        .iter()
        .zip(glows(b))
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f64::max)
}

const REST: [&str; 3] = [
    " \u{2850}\u{2808} \u{2801}\u{2801}  ",
    "\u{2830}  \u{28BE}\u{2877}  \u{2806}",
    " \u{2821}\u{2880} \u{2840}\u{2840}\u{280C} ",
];

#[test]
fn the_opening_is_a_pure_function_of_time_on_a_100_ms_grid() {
    for t in (0..1200).step_by(100) {
        assert_eq!(opening(d(t), FULL), opening(d(t), FULL), "{t}");
        assert_eq!(opening(d(t), FULL), opening(d(t + 99), FULL), "{t}");
        assert_eq!(splash(d(t), FULL), splash(d(t + 50), FULL), "{t}");
    }
    assert_ne!(opening(d(0), FULL), opening(d(100), FULL));
    assert_eq!(opening(d(0), CALM), opening(d(199), CALM));
}

#[test]
fn the_comet_draws_each_light_as_it_passes_and_every_light_stays_lit() {
    let mut before = dots(&opening(d(0), FULL));
    let mut drawn = vec![before.iter().map(|b| b.count_ones()).sum::<u32>()];
    for t in (100..=1200).step_by(100) {
        let frame = opening(d(t), FULL);
        let now = dots(&frame);
        for (cell, (old, new)) in before.iter().zip(&now).enumerate() {
            assert_eq!(old & new, *old, "a light went out at {t} ms, cell {cell}");
        }
        let g = glows(&frame);
        let brightest = g.iter().copied().fold(0.0, f64::max);
        let grown: Vec<usize> = (0..now.len()).filter(|c| now[*c] != before[*c]).collect();
        if !grown.is_empty() {
            assert!(
                grown.iter().any(|c| g[*c] == brightest && brightest > 0.5),
                "the head draws the newest light at {t} ms: {g:?}"
            );
        }
        drawn.push(now.iter().map(|b| b.count_ones()).sum());
        before = now;
    }
    assert_eq!(drawn[0], 12, "only the core at first: {drawn:?}");
    assert!(drawn.windows(2).all(|w| w[0] <= w[1]), "{drawn:?}");
    assert!(
        drawn.windows(2).filter(|w| w[0] < w[1]).count() >= 8,
        "{drawn:?}"
    );
    assert_eq!(shape(&opening(d(1100), FULL)), REST);
    assert_eq!(shape(&opening(d(1200), FULL)), REST);
    for t in (0..1200).step_by(100) {
        for row in splash(d(t), FULL) {
            let row = spans_text(&row);
            assert!(!row.contains('\u{2800}'), "{t}: {row:?}");
        }
    }
}

#[test]
fn the_lap_hands_off_to_the_idle_comet_without_a_jump() {
    assert_eq!(opening(d(1200), FULL), splash_idle(d(1200), FULL));
    assert_eq!(opening(d(2400), CALM), splash_idle(d(2400), CALM));
    assert_eq!(opening(d(1250), FULL), splash_idle(d(1250), FULL));
    for (cx, end, step) in [(FULL, 1200, 100), (CALM, 2400, 200)] {
        let idle_step = biggest_jump(&splash_idle(d(end), cx), &splash_idle(d(end + step), cx));
        let last = biggest_jump(&opening(d(end - step), cx), &opening(d(end), cx));
        assert!(
            last <= idle_step * 1.5,
            "{:?}: {last} against the idle's {idle_step}",
            cx.policy
        );
    }
    let core = |t: u64| opening(d(t), FULL)[1][3].map(|c| c.ink);
    assert_eq!(
        core(1200),
        Some(splash_idle(d(1200), FULL)[1][3].map(|c| c.ink)).flatten()
    );
    match core(1100) {
        Some(Ink::Mix { u, still, .. }) => {
            assert!(u.get() > 0.9, "{u:?}");
            assert_eq!(still, Token::Accent);
        }
        other => panic!("{other:?}"),
    }
    match core(0) {
        Some(Ink::Mix { u, under, .. }) => {
            assert!(u.get() < 0.1, "{u:?}");
            assert_eq!(under, crate::theme::Tone::Token(Token::Bg));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_words_fade_in_under_the_lap() {
    assert_eq!(words_fade(0, d(0), FULL), Some(0.0));
    assert_eq!(words_fade(0, d(300), FULL), Some(0.0));
    assert_eq!(words_fade(0, d(400), FULL), Some(1.0 / 3.0));
    assert_eq!(words_fade(0, d(600), FULL), Some(1.0));
    assert_eq!(words_fade(1, d(400), FULL), Some(0.0));
    assert_eq!(words_fade(1, d(700), FULL), Some(1.0));
    assert_eq!(words_fade(9, d(1100), FULL), Some(2.0 / 3.0));
    for row in 0..12 {
        let mut last = 0.0;
        for t in (0..1200).step_by(100) {
            let Some(u) = words_fade(row, d(t), FULL) else {
                panic!("row {row} at {t}");
            };
            assert!(u >= last, "row {row} at {t}");
            last = u;
        }
        assert_eq!(words_fade(row, d(1200), FULL), None);
    }
    assert_eq!(words_fade(0, d(800), CALM), Some(1.0 / 3.0));
    for cx in [SIXTEEN, REDUCED, ASCII] {
        assert_eq!(words_fade(0, d(0), cx), None);
    }
    let f = gateway("gw.example.com");
    let look = |t: u64, cx: MotionCtx| {
        welcome(&f, &tips(), d(t), cx, layout(80))[2]
            .spans
            .iter()
            .find(|s| s.text == "wisp")
            .and_then(|s| s.look.fg)
    };
    assert_eq!(
        look(0, FULL),
        Some(Ink::mix(Token::Text, Token::Bg, 0.0, Token::Text))
    );
    assert_eq!(look(1200, FULL), Some(Ink::Token(Token::Text)));
    assert_eq!(look(0, SIXTEEN), Some(Ink::Token(Token::Text)));
    let words = |t: u64| -> Vec<String> {
        panel(&f, 80, GlyphSet::Unicode, t, FULL)
            .iter()
            .map(|row| row.chars().skip(14).collect())
            .collect()
    };
    assert_eq!(
        words(0),
        words(SETTLED),
        "the words are laid out from the first frame"
    );
}

#[test]
fn reduced_motion_ascii_and_plain_colours_open_as_the_brief_says() {
    let f = gateway("gw.example.com");
    assert_eq!(shape(&opening(d(0), REDUCED)), REST);
    assert_eq!(splash(d(0), REDUCED), splash(d(SETTLED), FULL));
    assert_eq!(
        panel(&f, 80, GlyphSet::Unicode, 0, REDUCED),
        panel(&f, 80, GlyphSet::Unicode, SETTLED, FULL)
    );
    assert_eq!(
        welcome(&f, &tips(), d(0), REDUCED, layout(80)),
        welcome(&f, &tips(), d(SETTLED), REDUCED, layout(80))
    );
    let ascii: Vec<String> = splash(d(0), ASCII).iter().map(|r| spans_text(r)).collect();
    assert_eq!(ascii, SPLASH_ASCII);
    assert_eq!(
        panel(&f, 80, GlyphSet::Ascii, 0, ASCII),
        panel(&f, 80, GlyphSet::Ascii, SETTLED, ASCII)
    );
    for t in (0..=1200).step_by(100) {
        assert_eq!(
            shape(&opening(d(t), SIXTEEN)),
            shape(&opening(d(t), FULL)),
            "{t}"
        );
        for cell in opening(d(t), SIXTEEN).iter().flatten().flatten() {
            assert!(matches!(cell.ink, Ink::Token(_)), "{t}: {cell:?}");
        }
        let welcome = welcome(&f, &tips(), d(t), SIXTEEN, layout(80));
        for span in welcome.iter().flat_map(|l| &l.spans) {
            assert!(
                !matches!(span.look.fg, Some(Ink::Mix { .. })),
                "{t}: {span:?}"
            );
        }
    }
    assert_eq!(
        opening(d(0), SIXTEEN)[1][3].map(|c| c.ink),
        Some(Ink::Token(Token::Accent))
    );
    let bright = |t: u64| {
        opening(d(t), SIXTEEN)
            .iter()
            .flatten()
            .flatten()
            .filter(|c| c.ink == Ink::Token(Token::Text))
            .count()
    };
    let shown = (100..1200).step_by(100).filter(|t| bright(*t) > 0).count();
    assert!(shown >= 9, "the head shows outside the gap: {shown} of 11");
}

#[test]
fn the_opening_falls_back_by_width_and_keeps_under_ten_frames_a_second() {
    let changes = |cx: MotionCtx, w: usize| {
        let mut out = vec![];
        let mut t = d(0);
        while let Some(n) = welcome_next_change(t, cx, layout(w)) {
            out.push(n.as_millis() as u64);
            t = n;
        }
        out
    };
    let full: Vec<u64> = (1..=12).map(|i| i * 100).collect();
    for w in [60, 80, 100, 200] {
        assert_eq!(changes(FULL, w), full, "{w} columns");
    }
    let calm: Vec<u64> = (1..=12).map(|i| i * 200).collect();
    assert_eq!(changes(CALM, 80), calm);
    for w in [40, 43, 44, 59] {
        assert_eq!(
            changes(FULL, w),
            [40, 80, 120, 160, 200, 240, 280],
            "{w} columns"
        );
    }
    for cx in [REDUCED, ASCII] {
        for w in [40, 50, 80] {
            assert_eq!(welcome_next_change(d(0), cx, layout(w)), None);
        }
    }
    assert_eq!(welcome_next_change(d(39), FULL, layout(80)), Some(d(100)));
    assert_eq!(welcome_next_change(d(90), FULL, layout(80)), Some(d(123)));
    assert_eq!(welcome_next_change(d(1200), FULL, layout(80)), None);
    let f = gateway("gw.example.com");
    let at = |w: usize, t: u64| panel(&f, w, GlyphSet::Unicode, t, FULL);
    assert!(at(80, 0)[2].contains("wisp"));
    assert_eq!(at(200, SETTLED)[0].chars().count(), WELCOME_MAX);
    assert!(at(50, 0)[2].contains("wisp") && at(50, 0)[2].starts_with('\u{2502}'));
    assert!(at(43, 0)[0].contains("wisp") && !at(43, 0)[0].starts_with('\u{256D}'));
    for w in [40, 50, 80, 200] {
        for t in (0..=1200).step_by(100) {
            for row in at(w, t) {
                assert!(!row.contains('\u{2800}'), "{w} {t}: {row:?}");
            }
        }
    }
}

#[test]
fn the_panel_shows_the_current_model_and_effort_and_wraps_long_facts() {
    let f = Facts {
        model: "claude-haiku-5-5".to_owned(),
        effort: "xhigh".to_owned(),
        cache_ttl: CacheTtl::FiveMinutes,
        log: None,
        ..facts()
    };
    let rows = panel(&f, 120, GlyphSet::Unicode, SETTLED, FULL);
    assert!(rows[4].contains("haiku-5-5 xhigh"), "{rows:#?}");
    assert!(
        rows[5].contains("~/Projects/Wisp \u{00B7} Claude API"),
        "{rows:#?}"
    );
    assert!(rows[6].trim_matches(['│', ' ']).is_empty(), "{rows:#?}");
    let rows = detailed(&f, 120, GlyphSet::Unicode);
    assert!(
        rows[6].contains("cache 5m \u{00B7} not logging this session"),
        "{rows:#?}"
    );
    let path = format!(
        "/home/someone/.local/share/wisp/sessions/{}.jsonl",
        "0".repeat(60)
    );
    let long = Facts {
        log: Some(path.clone()),
        ..gateway("a-very-long-gateway-host-name.internal.example.com:8443")
    };
    for w in [44, 60, 80] {
        let rows = detailed(&long, w, GlyphSet::Unicode);
        let inside: String = rows
            .iter()
            .map(|r| {
                r.trim_start_matches('\u{2502}')
                    .trim_end_matches('\u{2502}')
                    .trim()
                    .to_owned()
            })
            .collect();
        let joined = inside.replace(' ', "");
        assert!(
            joined.contains(&format!("loggingto{path}")),
            "{w}: {rows:#?}"
        );
        assert!(
            joined.contains("a-very-long-gateway-host-name.internal.example.com:8443"),
            "{w}: {rows:#?}"
        );
    }
    let replay = Facts {
        credential: None,
        ..facts()
    };
    let rows = panel(&replay, 80, GlyphSet::Unicode, SETTLED, FULL);
    assert!(
        rows[5].contains("~/Projects/Wisp \u{00B7} replay, no API calls"),
        "{rows:#?}"
    );
}

#[test]
fn hostile_welcome_facts_are_sanitised() {
    let evil = "x\u{1b}]0;pwned\u{7}\n\u{1b}[2JFAKE\u{202E}y\t\r";
    let f = Facts {
        model: format!("claude-{evil}"),
        effort: evil.to_owned(),
        version: evil.to_owned(),
        cwd: format!("~/{evil}"),
        log: Some(format!("/tmp/{evil}.jsonl")),
        ..gateway(&format!("gw{evil}.example.com"))
    };
    let tips = vec![KeyHint::new(format!("tip {evil}"), evil)];
    let mut rows = vec![];
    for (w, set) in [
        (120, GlyphSet::Unicode),
        (50, GlyphSet::Unicode),
        (40, GlyphSet::Unicode),
        (80, GlyphSet::Ascii),
    ] {
        rows.extend(welcome(&f, &tips, d(SETTLED), FULL, Layout::new(w, set)));
        rows.extend(welcome_in_detail(&f, FULL, Layout::new(w, set)));
    }
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    rows.push(s.row(d(0), FULL, &f, layout(140)));
    rows.push(s.row(d(0), FULL, &f, layout(60)));
    for r in &rows {
        let t = line(r);
        assert!(
            !t.chars().any(|c| c.is_control() || c == '\u{202E}'),
            "{t:?}"
        );
    }
}

#[test]
fn the_panel_shows_the_log_path_under_home_as_a_tilde() {
    let f = Facts {
        log: Some("/home/u/.local/share/wisp/sessions/0193.jsonl".to_owned()),
        home: Some("/home/u".to_owned()),
        ..facts()
    };
    let rows = detailed(&f, 100, GlyphSet::Unicode);
    assert!(
        rows.iter()
            .any(|r| r.contains("logging to ~/.local/share/wisp/sessions/0193.jsonl")),
        "{rows:#?}"
    );
    let elsewhere = Facts {
        home: Some("/home/someone-else".to_owned()),
        ..f.clone()
    };
    let rows = detailed(&elsewhere, 100, GlyphSet::Unicode);
    assert!(
        rows.iter()
            .any(|r| r.contains("logging to /home/u/.local/share/wisp/sessions/0193.jsonl")),
        "{rows:#?}"
    );
    let hostile = Facts {
        log: Some("/home/u/a\u{1b}]0;x\u{7}b.jsonl".to_owned()),
        ..f
    };
    let rows = detailed(&hostile, 100, GlyphSet::Unicode);
    let row = rows
        .iter()
        .find(|r| r.contains("logging to"))
        .cloned()
        .unwrap_or_default();
    assert!(row.contains("logging to ~/a"), "{row:?}");
    assert!(!row.chars().any(char::is_control), "{row:?}");
}

#[test]
fn the_orb_rests_only_in_ready_without_a_hint() {
    let mut s = Status::new();
    assert_eq!(s.resting(d(4000), FULL), Some(d(4000)));
    s.composer(false, d(5000), FULL);
    assert_eq!(s.resting(d(6000), FULL), None);
    s.composer(true, d(7000), FULL);
    assert_eq!(s.resting(d(7500), FULL), Some(d(500)));
    s.hint("a hint", d(8000), Some(d(1000)));
    assert_eq!(s.resting(d(8500), FULL), None);
    assert_eq!(s.resting(d(9000), FULL), Some(d(2000)));
    s.submitted(d(10_000), FULL);
    assert_eq!(s.resting(d(10_500), FULL), None);
    s.apply(&AgentEvent::turn_started(1), d(10_600), FULL);
    s.apply(&text_block(), d(10_700), FULL);
    assert_eq!(s.resting(d(11_000), FULL), None);
    s.apply(
        &ended(completed(StopReason::EndTurn, true), 900),
        d(12_000),
        FULL,
    );
    assert_eq!(s.resting(d(12_100), FULL), None);
    let settled = d(12_000) + crate::motion::settles(crate::motion::Presence::Done, FULL).unwrap();
    assert_eq!(s.resting(settled + d(3000), FULL), Some(d(3000)));
}

#[test]
fn a_native_search_refusal_note_names_the_refused_endpoint_and_the_rebuild() {
    let refused = wisp_protocol::CacheEffect::Rebuilt {
        reason: wisp_protocol::RebuildReason::NativeSearchRefused,
    };
    let note = |endpoint: Option<&str>, search_refused_on: Option<&str>| {
        let c = Change {
            endpoint,
            account_bound: &[],
            model: "claude-opus-5-5",
            effort: Some(wisp_protocol::Effort::High),
            cache: &refused,
            model_changed: false,
            effort_changed: false,
            rebuild: Rebuild::Usd(0.04),
            search_refused_on,
            search_refused_gateway: false,
            search: None,
            work: None,
        };
        change_note(&c, &UNICODE)
    };
    assert_eq!(
        note(None, Some("direct")),
        "web search isn't enabled for this account on direct; continuing without web search \u{b7} cache rebuild ~$0.04"
    );
    assert_eq!(
        note(Some("gateway"), Some("direct")),
        "web search isn't enabled for this account on direct; continuing without web search \u{b7} cache rebuild ~$0.04"
    );
    assert!(!note(None, Some("evil\u{1b}[2J")).contains('\u{1b}'));
}

#[test]
fn a_gateway_refusal_note_names_the_off_command() {
    let refused = wisp_protocol::CacheEffect::Rebuilt {
        reason: wisp_protocol::RebuildReason::NativeSearchRefused,
    };
    let c = Change {
        endpoint: None,
        account_bound: &[],
        model: "claude-opus-5-5",
        effort: Some(wisp_protocol::Effort::High),
        cache: &refused,
        model_changed: false,
        effort_changed: false,
        rebuild: Rebuild::Usd(0.04),
        search_refused_on: Some("gw"),
        search_refused_gateway: true,
        search: None,
        work: None,
    };
    assert_eq!(
        change_note(&c, &UNICODE),
        "gw refused the web search tool; continuing without web search. To stop sending it: wisp config set endpoints.gw.native_web_search off \u{b7} cache rebuild ~$0.04"
    );
}

#[test]
fn a_change_of_search_form_follows_the_rebuild_in_the_note() {
    let switch = wisp_protocol::CacheEffect::Rebuilt {
        reason: wisp_protocol::RebuildReason::ModelSwitch,
    };
    let shift = "web search: on \u{2192} off (not on this model) \u{b7} earlier reasoning dropped";
    let bound = vec!["claude-sonnet-5-5".to_owned()];
    let c = Change {
        endpoint: None,
        account_bound: &bound,
        model: "my-model",
        effort: Some(wisp_protocol::Effort::Low),
        cache: &switch,
        model_changed: true,
        effort_changed: false,
        rebuild: Rebuild::Usd(0.04),
        search_refused_on: None,
        search_refused_gateway: false,
        search: Some(shift),
        work: None,
    };
    assert_eq!(
        change_note(&c, &UNICODE),
        format!(
            "Model \u{2192} my-model \u{b7} cache rebuild ~$0.04 \u{b7} {shift} \u{b7} earlier Sonnet 5.5 reasoning may not carry over"
        )
    );
    let c = Change {
        search: Some("web search: none\u{1b}[2J"),
        ..c
    };
    assert!(!change_note(&c, &UNICODE).contains('\u{1b}'));
}

#[test]
fn an_endpoint_change_note_names_the_endpoint_then_the_model_and_effort() {
    let switch = wisp_protocol::CacheEffect::Rebuilt {
        reason: wisp_protocol::RebuildReason::EndpointSwitch,
    };
    let bound = vec![
        "claude-sonnet-5-5".to_owned(),
        "claude-haiku-5-5".to_owned(),
    ];
    let change = |model_changed, effort_changed, account_bound: &[String]| {
        let c = Change {
            endpoint: Some("direct"),
            account_bound,
            model: "claude-sonnet-5-5",
            effort: Some(wisp_protocol::Effort::Low),
            cache: &switch,
            model_changed,
            effort_changed,
            rebuild: Rebuild::Usd(0.04),
            search_refused_on: None,
            search_refused_gateway: false,
            search: None,
            work: None,
        };
        (
            change_note(&c, &UNICODE),
            change_note(&c, &Glyphs::of(GlyphSet::Ascii)),
        )
    };
    assert_eq!(
        change(false, false, &[]).0,
        "Account \u{2192} direct \u{b7} cache rebuild ~$0.04"
    );
    assert_eq!(
        change(true, false, &[]).0,
        "Account \u{2192} direct, model \u{2192} claude-sonnet-5-5 \u{b7} cache rebuild ~$0.04"
    );
    assert_eq!(
        change(true, true, &bound).0,
        "Account \u{2192} direct, model \u{2192} claude-sonnet-5-5, effort \u{2192} low \u{b7} cache rebuild ~$0.04 \u{b7} earlier Sonnet 5.5 and Haiku 5.5 reasoning may not carry over"
    );
    assert_eq!(
        change(false, false, &bound[..1]).1,
        "Account -> direct - cache rebuild ~$0.04 - earlier Sonnet 5.5 reasoning may not carry over"
    );
}

#[test]
fn model_names_read_as_words_and_fall_back_to_the_short_id() {
    assert_eq!(model_name("claude-sonnet-5-5"), "Sonnet 5.5");
    assert_eq!(model_name("claude-haiku-5-5"), "Haiku 5.5");
    assert_eq!(model_name("claude-opus-4-8"), "Opus 4.8");
    assert_eq!(
        model_name("claude-sonnet-4-5-20250929"),
        "sonnet-4-5-20250929"
    );
    assert_eq!(model_name("my-gw/model"), "my-gw/model");
    let hostile = model_name("claude-x\u{1b}[2J-5");
    assert!(!hostile.contains('\u{1b}'), "{hostile}");
    assert!(hostile.starts_with('x'), "{hostile}");
}

#[test]
fn an_unpriced_rebuild_names_no_amount() {
    assert_eq!(
        Rebuild::Usd(0.04).unpriced().line(GlyphSet::Unicode),
        "cache rebuild, no prices"
    );
    assert_eq!(
        Rebuild::Tokens(9).unpriced().amount(GlyphSet::Ascii),
        "no prices"
    );
    assert_eq!(
        Rebuild::UpToUsd(0.04).unpriced().line(GlyphSet::Unicode),
        "cache rebuild up to the whole prompt, no prices"
    );
    assert_eq!(
        Rebuild::UpToTokens(9).unpriced().amount(GlyphSet::Unicode),
        "up to the whole prompt, no prices"
    );
    assert_eq!(
        Effect::Rebuild(Rebuild::UpToTokens(9).unpriced()).text(GlyphSet::Ascii),
        "cache rebuild up to the whole prompt, no prices"
    );
    assert_eq!(Rebuild::Expired.unpriced(), Rebuild::Expired);
    assert_eq!(Rebuild::Empty.unpriced(), Rebuild::Empty);
}

#[test]
fn a_pending_endpoint_leads_the_next_turn_fact() {
    let next = |model: Option<&str>| Facts {
        next: Some(NextTurn {
            endpoint: Some("direct".to_owned()),
            model: model.map(str::to_owned),
            effort: None,
            cache_ttl: None,
        }),
        ..facts()
    };
    assert_eq!(next(None).pending(&UNICODE), None);
    assert_eq!(
        next(None).pending_chip().map(|chip| chip.name),
        Some("direct".to_owned())
    );
    assert_eq!(
        next(Some("claude-sonnet-5-5")).pending(&UNICODE).as_deref(),
        Some("\u{2192} sonnet-5-5 next turn")
    );
}

fn after_switch(reads: u64, written: u64) -> (String, Vec<String>) {
    let mut s = Status::new();
    let switch = settings_changed(Changed {
        endpoint: "direct".to_owned(),
        model: "claude-sonnet-5-5".to_owned(),
        cache: wisp_protocol::CacheEffect::Rebuilt {
            reason: wisp_protocol::RebuildReason::EndpointSwitch,
        },
        ..Changed::default()
    });
    let outcome = outcome(End {
        model: "claude-sonnet-5-5".to_owned(),
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: reads,
            cache_creation_input_tokens: written,
            ..Usage::default()
        }),
        cost: Some(cost(0.014, 0.21, false)),
        ..End::default()
    });
    let footer = copy::footer_parts(
        &outcome,
        &TurnStats::default(),
        false,
        Some(wisp_protocol::RebuildReason::EndpointSwitch),
        copy::Counts {
            requests: 1,
            tools: 0,
        },
    )
    .into_iter()
    .map(|(fact, _)| fact.text())
    .collect();
    s.apply(&switch, d(0), FULL);
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    s.apply(&ended(outcome, 1), d(10), FULL);
    (say(&s, 20, FULL), footer)
}

#[test]
fn an_endpoint_switch_is_named_only_when_the_cache_was_not_read() {
    let (said, footer) = after_switch(0, 9_000);
    assert!(said.ends_with("cache rebuilt: account switched"), "{said}");
    assert_eq!(
        footer.last().map(String::as_str),
        Some("cache rebuilt: account switched")
    );
    let (said, footer) = after_switch(9_000, 0);
    assert!(said.ends_with("cached 98%"), "{said}");
    assert_eq!(footer.last().map(String::as_str), Some("cached 98%"));
    assert_eq!(
        copy::shown_rebuild(
            Some(wisp_protocol::RebuildReason::EndpointSwitch),
            false,
            None
        ),
        None
    );
    assert_eq!(
        copy::shown_rebuild(
            Some(wisp_protocol::RebuildReason::ModelSwitch),
            false,
            Some(&Usage {
                input_tokens: 9_000,
                ..Usage::default()
            })
        ),
        None
    );
}

#[test]
fn a_whole_rebuild_is_named_below_half_the_prompt_read() {
    let read = |cache_read_input_tokens| Usage {
        input_tokens: 100 - cache_read_input_tokens,
        cache_read_input_tokens,
        ..Usage::default()
    };
    let switch = Some(wisp_protocol::RebuildReason::EndpointSwitch);
    assert_eq!(
        copy::shown_rebuild(switch.clone(), false, Some(&read(49))),
        switch
    );
    assert_eq!(copy::shown_rebuild(switch, false, Some(&read(50))), None);
}

fn asking(tail: CacheTtl, step_end_ms: u64, ask_ms: u64) -> Status {
    let cx = FULL;
    let tail = match tail {
        CacheTtl::FiveMinutes => Duration::from_secs(300),
        CacheTtl::OneHour => Duration::from_secs(3600),
    };
    let mut s = Status::new().with_tail(tail);
    s.submitted(d(0), cx);
    s.apply(&AgentEvent::turn_started(1), d(10), cx);
    s.apply(&tool_requested("q", "Database"), d(20), cx);
    name_call_from_transcript(&mut s, &tool_requested("q", "Database"));
    s.apply(
        &AgentEvent::StepEnded {
            step: 1,
            usage: None,
            cost_usd: None,
            lower_bound: false,
        },
        d(step_end_ms),
        cx,
    );
    s.apply(&AgentEvent::call_started("q", ""), d(ask_ms), cx);
    s.apply(
        &AgentEvent::QuestionAsked {
            call_id: "q".to_owned(),
            questions: vec![],
        },
        d(ask_ms),
        cx,
    );
    s
}

#[test]
fn an_open_question_says_waiting_for_your_answer_with_its_clock() {
    let cx = FULL;
    let mut s = asking(CacheTtl::OneHour, 1000, 1000);
    assert_eq!(s.state(d(1000), cx), State::Asking);
    assert_eq!(say(&s, 1000, cx), "Waiting for your answer \u{b7} 0s");
    assert_eq!(say(&s, 13_400, cx), "Waiting for your answer \u{b7} 12s");
    let next = s.next_change(d(13_400), cx).unwrap();
    assert!(next <= d(14_000), "{next:?}");
    s.apply(
        &AgentEvent::QuestionClosed {
            call_id: "q".to_owned(),
            how: wisp_protocol::QuestionEnd::Stopped,
        },
        d(14_000),
        cx,
    );
    assert_eq!(s.state(d(14_000), cx), State::Thinking);
    assert_eq!(say(&s, 14_000, cx), "Kindling\u{2026} 14s \u{b7} thinking");
}

#[test]
fn the_cache_note_shows_in_the_last_sixth_of_a_one_hour_tail() {
    let cx = FULL;
    let s = asking(CacheTtl::OneHour, 1000, 1000);
    let at = |mins: u64, secs: u64| 1000 + (mins * 60 + secs) * 1000;
    assert!(!say(&s, at(50, 0), cx).contains("cache"));
    assert_eq!(
        say(&s, at(50, 1), cx),
        "Waiting for your answer \u{b7} 50m 1s \u{b7} cache expires in 10 min"
            .replace("50m 1s", &copy::clock(50 * 60 + 1))
    );
    assert!(say(&s, at(59, 1), cx).ends_with("cache expires in 1 min"));
    assert!(say(&s, at(60, 0), cx).ends_with("cache expired"));
    assert!(say(&s, at(75, 0), cx).ends_with("cache expired"));
    let next = s.next_change(d(at(49, 59) + 500), cx).unwrap();
    assert!(next <= d(at(50, 0) + 1), "{next:?}");
}

#[test]
fn the_cache_note_on_a_five_minute_tail_counts_from_the_last_step() {
    let cx = FULL;
    let s = asking(CacheTtl::FiveMinutes, 1000, 3000);
    let at = |secs: u64| 1000 + secs * 1000;
    assert!(!say(&s, at(250), cx).contains("cache"));
    assert!(say(&s, at(251), cx).ends_with("cache expires in 1 min"));
    assert!(say(&s, at(300), cx).ends_with("cache expired"));
    assert_eq!(s.cache_note(d(at(10))), None);
}

#[test]
fn without_a_known_tail_there_is_no_cache_note() {
    let cx = FULL;
    let mut s = Status::new();
    s.submitted(d(0), cx);
    s.apply(&AgentEvent::turn_started(1), d(10), cx);
    s.apply(&AgentEvent::call_started("q", ""), d(15), cx);
    s.apply(
        &AgentEvent::QuestionAsked {
            call_id: "q".to_owned(),
            questions: vec![],
        },
        d(20),
        cx,
    );
    assert_eq!(s.cache_note(d(10_000_000)), None);
    assert_eq!(say(&s, 3_620, cx), "Waiting for your answer \u{b7} 3s");
}

#[test]
fn a_turn_ending_with_an_open_question_waits_thinks_on_next_turn_and_waits_again() {
    let cx = FULL;
    let mut s = asking(CacheTtl::OneHour, 1000, 1000);
    s.apply(
        &AgentEvent::call_finished(
            "q",
            wisp_protocol::ToolOutcome::Completed {
                is_error: false,
                exit: None,
            },
        ),
        d(1200),
        cx,
    );
    assert_eq!(s.state(d(1200), cx), State::Thinking);
    s.apply(
        &AgentEvent::TurnEnded {
            outcome: completed(StopReason::EndTurn, true),
            stats: TurnStats::default(),
        },
        d(2000),
        cx,
    );
    assert_eq!(s.state(d(5000), cx), State::Asking);
    assert_eq!(say(&s, 5000, cx), "Waiting for your answer \u{b7} 4s");
    s.apply(&AgentEvent::turn_started(2), d(6000), cx);
    assert_eq!(s.state(d(6500), cx), State::Thinking);
    s.apply(
        &AgentEvent::TurnEnded {
            outcome: completed(StopReason::EndTurn, true),
            stats: TurnStats::default(),
        },
        d(10_000),
        cx,
    );
    assert_eq!(s.state(d(15_000), cx), State::Asking);
    assert_eq!(say(&s, 15_000, cx), "Waiting for your answer \u{b7} 14s");
}

#[test]
fn an_idle_close_goes_idle_and_an_idle_ask_waits_without_a_clock() {
    let cx = FULL;
    let mut s = Status::new().with_tail(Duration::from_secs(3600));
    s.apply(
        &AgentEvent::QuestionAsked {
            call_id: "q".to_owned(),
            questions: vec![],
        },
        d(5000),
        cx,
    );
    assert_eq!(s.state(d(20_000), cx), State::Asking);
    assert_eq!(say(&s, 20_000, cx), "Waiting for your answer");
    assert_eq!(s.next_change(d(20_000), cx), None);
    s.apply(
        &AgentEvent::QuestionClosed {
            call_id: "q".to_owned(),
            how: wisp_protocol::QuestionEnd::Stopped,
        },
        d(25_000),
        cx,
    );
    assert_eq!(s.state(d(25_000), cx), State::Ready);
}

#[test]
fn idle_waiting_row_fits_at_40_80_and_120_columns() {
    let cx = SIXTEEN;
    let mut s = asking(CacheTtl::OneHour, 1000, 1000);
    s.apply(
        &AgentEvent::call_finished(
            "q",
            wisp_protocol::ToolOutcome::Completed {
                is_error: false,
                exit: None,
            },
        ),
        d(1200),
        cx,
    );
    s.apply(
        &AgentEvent::TurnEnded {
            outcome: completed(StopReason::EndTurn, true),
            stats: TurnStats::default(),
        },
        d(2000),
        cx,
    );
    let t = d(1000 + (52 * 60 + 10) * 1000);
    s.keys(&[copy::esc(copy::CLOSE_WORD)]);
    let at = |s: &Status, w: usize| line(&s.row(t, cx, &facts(), layout(w)));
    let r40 = at(&s, 40);
    assert!(r40.contains("Waiting \u{b7} 52m 10s"), "{r40}");
    assert!(r40.contains("esc close"), "{r40}");
    assert!(!r40.contains("cache"), "{r40}");
    assert!(!r40.contains("Claude"), "{r40}");
    let r80 = at(&s, 80);
    assert!(r80.contains("Waiting \u{b7} 52m 10s"), "{r80}");
    assert!(r80.contains("cache expires in 8 min"), "{r80}");
    assert!(r80.contains("esc close"), "{r80}");
    assert!(r80.contains("Claude"), "{r80}");
    let r120 = at(&s, 120);
    assert!(
        r120.contains("Waiting for your answer \u{b7} 52m 10s \u{b7} cache expires in 8 min"),
        "{r120}"
    );
    assert!(r120.contains("esc close"), "{r120}");
    s.keys(&[]);
    let bare40 = at(&s, 40);
    assert!(bare40.contains("Waiting \u{b7} 52m 10s"), "{bare40}");
    assert!(!bare40.contains("cache"), "{bare40}");
    assert!(!bare40.contains("Claude"), "{bare40}");
}

fn context_changed(used: u64, auto_compact: bool) -> AgentEvent {
    AgentEvent::ContextChanged {
        used,
        used_estimated: false,
        next: used,
        window: Some(1_000_000),
        compact_at: Some(850_000),
        meter_limit: Some(850_000),
        auto_compact,
        epoch: 0,
    }
}

fn meter_row(used: u64, auto_compact: bool, replayed: bool) -> StyledLine {
    let mut s = Status::new();
    let changed = context_changed(used, auto_compact);
    if replayed {
        s.replayed(&changed);
    } else {
        s.apply(&changed, d(0), FULL);
    }
    let l = layout(120);
    let glyphs = Glyphs::of(l.glyphs);
    status_row(
        &s.orb(d(0), FULL),
        vec![],
        &[],
        &s.tally().facts(&facts(), &glyphs, &[], None),
        l,
    )
}

#[test]
fn past_the_limit_the_status_meter_is_red_only_while_nothing_compacts() {
    let loud = crate::styled::Look::fg(Token::Text).bold();
    let red = crate::styled::Look::fg(Token::Error).bold();
    for replayed in [false, true] {
        for (used, auto_compact, look, shown) in [
            (700_000, false, loud, "700k/850k"),
            (900_000, true, loud, "900k/850k"),
            (900_000, false, red, "900k/850k"),
        ] {
            let row = meter_row(used, auto_compact, replayed);
            let text = line(&row);
            assert!(text.contains(&format!("\u{b7} {shown}")), "{text}");
            let span = row.spans.iter().find(|s| s.text == shown).unwrap();
            assert_eq!(span.look, look, "{used} {auto_compact} {replayed}");
        }
    }
}

#[test]
fn a_calm_meter_merges_into_the_muted_facts() {
    let row = meter_row(62_400, true, false);
    let muted: Vec<&StyledSpan> = row
        .spans
        .iter()
        .filter(|s| s.look == crate::styled::Look::fg(Token::TextMuted))
        .collect();
    let merged = muted.iter().find(|s| s.text.contains("62k/850k"));
    assert!(merged.is_some_and(|s| s.text.contains("low")), "{row:?}");
}

mod compacting {
    use wisp_protocol::{
        CacheEffect, CompactionEngine, CompactionFailure, CompactionTrigger, RebuildReason,
        RequestId,
    };

    use super::*;

    fn started(trigger: CompactionTrigger, turn: Option<u64>) -> AgentEvent {
        AgentEvent::CompactionStarted {
            trigger,
            turn,
            switch: None,
            requested_by: None,
        }
    }

    fn compacted(trigger: CompactionTrigger, turn: Option<u64>) -> AgentEvent {
        AgentEvent::Compacted {
            trigger,
            engine: CompactionEngine::Native,
            before: 182_000,
            after: 14_000,
            partial: false,
            converted: false,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: Some(usage()),
            cost: Some(cost(0.28, 1.5, false)),
            answers: turn.is_none().then_some(RequestId(1)),
            turn,
            summary: None,
            paused: false,
        }
    }

    pub(super) fn retrying() -> AgentEvent {
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 8000,
            reason: "overloaded".to_owned(),
            step: 0,
            kind: ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: Some(0.01),
            lower_bound: false,
            resend: None,
            busy: None,
        }
    }

    fn compacting(cx: MotionCtx) -> Status {
        let mut s = Status::new();
        s.apply(&started(CompactionTrigger::Manual, None), d(0), cx);
        s
    }

    #[test]
    fn a_manual_compaction_is_busy_and_reads_compacting_with_its_clock() {
        let cx = SIXTEEN;
        let s = compacting(cx);
        assert!(s.is_running() && s.is_compacting());
        assert!(!s.is_awaiting());
        assert_eq!(s.state(d(0), cx), State::Compacting);
        assert_eq!(say(&s, 100, cx), "");
        assert_eq!(say(&s, 500, cx), "Compacting\u{2026}");
        assert_eq!(
            say(&s, 3_200, cx),
            "Compacting\u{2026} 3s \u{b7} esc to stop"
        );
        let next = s.next_change(d(3_200), cx).unwrap();
        assert!(next <= d(4_000), "{next:?}");
    }

    #[test]
    fn a_compaction_end_returns_to_ready() {
        let cx = SIXTEEN;
        let mut s = compacting(cx);
        s.apply(&retrying(), d(1_000), cx);
        assert_eq!(s.state(d(2_000), cx), State::Compacting);
        assert!(
            say(&s, 2_000, cx).starts_with("Retrying in 7 s"),
            "{}",
            say(&s, 2_000, cx)
        );
        assert_eq!(
            say(&s, 9_500, cx),
            "Compacting\u{2026} 9s \u{b7} esc to stop"
        );
        s.apply(&compacted(CompactionTrigger::Manual, None), d(12_000), cx);
        assert!(!s.is_running() && !s.is_compacting());
        assert_eq!(s.state(d(12_000), cx), State::Ready);
    }

    #[test]
    fn a_failed_or_stopped_compaction_ends_the_busy_state_too() {
        let cx = SIXTEEN;
        let mut s = compacting(cx);
        s.apply(
            &AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Manual,
                engine: Some(CompactionEngine::Native),
                kind: CompactionFailure::Cancelled,
                message: "The compaction was cancelled.".to_owned(),
                usage: None,
                cost: Some(cost(0.004, 0.204, false)),
                answers: Some(RequestId(1)),
                turn: None,
                paused: false,
                requested_by: None,
            },
            d(2_000),
            cx,
        );
        assert!(!s.is_running());
        assert_eq!(s.state(d(2_000), cx), State::Ready);
    }

    #[test]
    fn a_background_compaction_never_makes_wisp_busy() {
        let cx = SIXTEEN;
        let mut s = Status::new();
        s.apply(&started(CompactionTrigger::Background, None), d(0), cx);
        assert!(!s.is_running());
        assert_eq!(s.state(d(0), cx), State::Ready);
    }

    #[test]
    fn a_compaction_inside_a_turn_shows_then_hands_back_to_thinking() {
        let cx = SIXTEEN;
        let mut s = Status::new();
        s.submitted(d(0), cx);
        s.apply(&AgentEvent::turn_started(1), d(10), cx);
        s.apply(&started(CompactionTrigger::Convert, Some(1)), d(20), cx);
        assert_eq!(s.state(d(20), cx), State::Compacting);
        assert!(!s.is_compacting());
        s.apply(&compacted(CompactionTrigger::Convert, Some(1)), d(30), cx);
        assert_eq!(s.state(d(30), cx), State::Thinking);
        assert!(s.is_running());
    }

    #[test]
    fn an_end_with_no_start_changes_nothing() {
        let cx = SIXTEEN;
        let mut s = Status::new();
        s.apply(&compacted(CompactionTrigger::Manual, None), d(0), cx);
        assert_eq!(s.state(d(0), cx), State::Ready);
        assert!(!s.is_running());
    }
}

#[test]
fn a_key_given_as_the_model_is_never_drawn() {
    let key = "sk-ant-api03-CANARYMODEL0000";
    for drawn in [model_line(key), short_model(key), model_name(key)] {
        assert_eq!(drawn, wisp_protocol::NOT_SHOWN);
    }
    assert_eq!(
        short_model("claude-sonnet-4-5-20250929"),
        "sonnet-4-5-20250929"
    );
    for (id, shown) in [
        ("gw/abcde12345", wisp_protocol::NOT_SHOWN),
        ("gw/abcd12345", "gw/abcd12345"),
        ("gw/AbcdefghijKlmnopqrst", wisp_protocol::NOT_SHOWN),
        ("gw/AbcdefghijKlmnopqrs", "gw/AbcdefghijKlmnopqrs"),
    ] {
        assert_eq!(short_model(id), shown, "{id}");
    }
    assert_eq!(
        model_line("gpt-4o-mini-2024-07-18"),
        "gpt-4o-mini-2024-07-18"
    );
    let f = Facts {
        model: key.to_owned(),
        next: Some(NextTurn {
            model: Some(key.to_owned()),
            ..NextTurn::default()
        }),
        ..facts()
    };
    let mut drawn = panel(&f, 120, GlyphSet::Unicode, SETTLED, FULL);
    drawn.extend(
        banner(&f, d(280), FULL, Layout::new(120, GlyphSet::Unicode))
            .iter()
            .map(line),
    );
    drawn.extend(f.pending(&UNICODE));
    drawn.push(f.short_model());
    let all = drawn.join("\n");
    assert!(!all.contains("CANARY"), "{all}");
    assert!(all.contains(wisp_protocol::NOT_SHOWN), "{all}");
}

#[test]
fn a_model_no_row_and_no_list_names_is_labelled_as_stored() {
    let mut account = endpoint("direct", false);
    account.models = vec![wisp_protocol::settings::AccountModel {
        id: "claude-nova-7".to_owned(),
        listed: None,
    }];
    let f = Facts {
        model: "claude-zzz-9-9".to_owned(),
        endpoints: vec![account],
        next: Some(NextTurn {
            model: Some("claude-zzz-9-9".to_owned()),
            ..NextTurn::default()
        }),
        ..facts()
    };
    for (id, label) in [
        ("claude-opus-5-5", "opus-5-5"),
        ("claude-nova-7", "nova-7"),
        ("claude-zzz-9-9", "claude-zzz-9-9"),
        ("zzz-9-9", "zzz-9-9"),
        ("sk-ant-api03-CANARYMODEL0000", wisp_protocol::NOT_SHOWN),
    ] {
        assert_eq!(f.model_label(id), label, "{id}");
    }
    assert_eq!(f.short_model(), "claude-zzz-9-9");
    assert_eq!(
        f.pending(&UNICODE).as_deref(),
        Some("\u{2192} claude-zzz-9-9 next turn")
    );
}

#[test]
fn every_sentence_that_names_a_model_hides_a_key() {
    let key = "sk-ant-api03-CANARYMODEL0000";
    let models = vec![key.to_owned(), "claude-opus-5-5".to_owned()];
    let window = wisp_protocol::settings::find("context_window").unwrap();
    let per_model = |place| {
        setting_label(&wisp_protocol::settings::SettingTarget::Path(
            wisp_protocol::settings::SitePath::new(place, window),
        ))
    };
    let sentences = [
        copy::model_also("\u{2192}", key),
        copy::reasoning_may_not_carry(&models),
        copy::gateway_rejected(key),
        copy::model_changed("\u{2192}", key),
        copy::unchanged(key, Some("high"), " \u{b7} "),
        copy::model_on(key, "gw"),
        copy::effort_back("high", key, "max"),
        copy::effort_rebuilds(key, "$0.01"),
        copy::gateway_fell_back(key, "$0.01"),
        copy::no_effort_levels(key),
        copy::no_images_for(key),
        copy::model_at(key, Some("high")),
        copy::model_at(key, None),
        per_model(wisp_protocol::settings::Place::Model(key.to_owned())),
        per_model(wisp_protocol::settings::Place::EndpointModel {
            endpoint: "gw".to_owned(),
            model: key.to_owned(),
        }),
    ];
    for sentence in &sentences {
        assert!(!sentence.contains("CANARY"), "{sentence}");
        assert!(sentence.contains(wisp_protocol::NOT_SHOWN), "{sentence}");
    }
    assert_eq!(
        copy::model_at("claude-opus-5-5", Some("high")),
        "claude-opus-5-5 at high"
    );
    assert_eq!(
        copy::reasoning_may_not_carry(&["Sonnet 5.5".to_owned()]),
        "earlier Sonnet 5.5 reasoning may not carry over"
    );
}

#[test]
fn a_hidden_model_is_taken_out_of_provider_text() {
    let key = "sk-ant-api03-CANARYMODEL0000";
    let models = [key, "claude-opus-5-5"];
    let failed = turn_ended(End {
        ending: Ending::Error(wisp_protocol::ErrorKind::InvalidRequest),
        message: format!("model: {key}"),
        attempts: 1,
        ..End::default()
    });
    let Some(AgentEvent::TurnEnded {
        outcome: TurnOutcome::Error { message, .. },
        ..
    }) = without_models(&failed, &models)
    else {
        panic!("the message kept the key");
    };
    assert_eq!(message, format!("model: {}", wisp_protocol::NOT_SHOWN));
    let rejected = AgentEvent::Rejected {
        reason: format!("no such model {key}"),
        id: None,
        kind: None,
        request: None,
    };
    let shown = without_models(&rejected, &models).unwrap();
    assert!(!format!("{shown:?}").contains("CANARY"), "{shown:?}");
    assert_eq!(without_models(&failed, &["claude-opus-5-5"]), None);
    let delta = AgentEvent::TextDelta {
        index: 0,
        text: key.to_owned(),
    };
    assert_eq!(without_models(&delta, &models), None);
}

#[test]
fn every_scrubbed_event_loses_the_hidden_model() {
    let key = "sk-ant-api03-CANARYMODEL0000";
    let models = [key, "claude-opus-5-5"];
    let retrying = |reason: String| AgentEvent::Retrying {
        attempt: 1,
        delay_ms: 8000,
        reason,
        step: 1,
        kind: wisp_protocol::ErrorKind::Server,
        detail: None,
        max_attempts: 5,
        discarded_blocks: 0,
        discarded_chars: 0,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        busy: None,
    };
    let compaction = |message: String| AgentEvent::CompactionFailed {
        trigger: wisp_protocol::CompactionTrigger::Manual,
        engine: None,
        kind: wisp_protocol::CompactionFailure::Error(wisp_protocol::ErrorKind::InvalidRequest),
        message,
        usage: None,
        cost: None,
        answers: None,
        turn: None,
        paused: false,
        requested_by: None,
    };
    let disabled = |reason: String| AgentEvent::LogDisabled { reason };
    let shown = wisp_protocol::NOT_SHOWN;
    let lower = key.to_ascii_lowercase();
    assert_eq!(
        without_models(&retrying(format!("model {key} is busy")), &models),
        Some(retrying(format!("model {shown} is busy")))
    );
    assert_eq!(
        without_models(&compaction(format!("unknown model {lower}")), &models),
        Some(compaction(format!("unknown model {shown}")))
    );
    assert_eq!(
        without_models(&disabled(format!("log for {key} full")), &models),
        Some(disabled(format!("log for {shown} full")))
    );
    for plain in [
        retrying("overloaded".to_owned()),
        compaction("claude-opus-5-5 overloaded".to_owned()),
        disabled("disk full".to_owned()),
    ] {
        assert_eq!(without_models(&plain, &models), None);
    }
}

#[test]
fn snapshot_the_key_line_keeps_help_last_at_every_width() {
    let shift_enter = vec![
        KeyHint::new("enter", "send"),
        KeyHint::new("shift-enter", "new line"),
        KeyHint::note("/model"),
        KeyHint::note("/help"),
    ];
    let mut out = String::new();
    for (name, keys) in [("ctrl-j", tips()), ("shift-enter", shift_enter)] {
        out.push_str(&format!("--- {name} ---\n"));
        let mut last = String::new();
        for w in (8..=60).rev() {
            let row = key_line(&keys, layout(w))
                .map(|l| line(&l))
                .unwrap_or_default();
            assert!(row.ends_with("/help"), "{w}: {row:?}");
            assert!(
                crate::text::width(&row, Default::default()) < w,
                "{w}: {row:?}"
            );
            if row != last {
                out.push_str(&format!("{w:>3} |{row}\n"));
                last = row;
            }
        }
    }
    insta::assert_snapshot!("key_line_drop_order", out);
}

#[test]
fn a_switch_compaction_shows_its_estimate_beside_the_rebuild_and_never_a_zero() {
    let work = |usd, cause, alone| SwitchWork { usd, cause, alone };
    let effect = |w| Effect::Switch(Rebuild::Usd(0.04), w);
    assert_eq!(
        effect(work(Some(0.48), SwitchCause::Convert, false)).text(GlyphSet::Unicode),
        "cache rebuild ~$0.04 \u{b7} compacts before your next message, then switches (~$0.48 \u{b7} converts the summary)"
    );
    assert_eq!(
        effect(work(None, SwitchCause::SmallerWindow, false)).text(GlyphSet::Ascii),
        "cache rebuild ~$0.04 - compacts before your next message, then switches (the new window is smaller)"
    );
    assert_eq!(
        effect(work(None, SwitchCause::Convert, true)).text(GlyphSet::Unicode),
        "cache rebuild ~$0.04 \u{b7} converts the summary before switching"
    );
    assert_eq!(
        work(Some(0.2), SwitchCause::KeptTurns, false).text(GlyphSet::Unicode),
        "compacts before your next message, then switches (~$0.20 \u{b7} kept turns can't go to another model)"
    );
    let unpriced = Effect::Switch(
        Rebuild::Unpriced(Scope::Exact),
        work(None, SwitchCause::Convert, false),
    )
    .text(GlyphSet::Unicode);
    assert!(!unpriced.contains("$0.00"), "{unpriced}");
    assert!(!unpriced.contains('\u{2248}'), "{unpriced}");
}

fn ink_of(row: &StyledLine, needle: &str) -> Vec<Option<Ink>> {
    row.spans
        .iter()
        .filter(|s| s.text.contains(needle))
        .map(|s| s.look.fg)
        .collect()
}

fn hinted(keys: &[KeyHint]) -> Status {
    let mut s = Status::new();
    s.submitted(d(0), FULL);
    s.apply(&AgentEvent::turn_started(1), d(5), FULL);
    s.apply(&ended(completed(StopReason::EndTurn, true), 1), d(10), FULL);
    s.keys(keys);
    s
}

#[test]
fn the_status_row_is_three_groups_with_no_dot_between_a_hint_and_a_fact() {
    let s = hinted(&copy::row_keys());
    for w in 60..=200 {
        let row = line(&s.row(d(5000), FULL, &facts(), layout(w)));
        let hints = "enter edit \u{b7} del remove \u{b7} tab switch";
        assert!(row.contains(&format!("Ready   {hints}")), "{w}: {row}");
        if let Some((_, after)) = row.split_once(hints) {
            assert!(!after.trim_start().starts_with('\u{b7}'), "{w}: {row}");
            if !after.is_empty() {
                assert!(after.starts_with("   "), "{w}: {row}");
            }
        }
        assert!(!row.contains("switches \u{b7}"), "{w}: {row}");
        assert!(
            crate::text::width(&row, Default::default()) < w,
            "{w}: {row}"
        );
    }
    let row = line(&s.row(d(5000), FULL, &facts(), layout(200)));
    assert!(
        row.ends_with("Claude API   sonnet-5-5 low \u{b7} cached 92%"),
        "{row}"
    );
    assert_eq!(crate::text::width(&row, Default::default()), 199);
}

#[test]
fn facts_drop_from_the_right_before_any_hint_does() {
    let s = hinted(&copy::row_keys());
    let order = ["cached 92%", "low", "sonnet-5-5", "Claude API"];
    let mut gone = 0;
    for w in (10..=160).rev() {
        let row = line(&s.row(d(5000), FULL, &facts(), layout(w)));
        let hints_whole = row.contains("tab switch");
        let shown: Vec<bool> = order.iter().map(|f| row.contains(f)).collect();
        let now = shown.iter().take_while(|s| !**s).count();
        assert!(now >= gone, "a fact came back at {w}: {row}");
        assert!(
            shown.iter().skip(now).all(|s| *s),
            "facts left out of order at {w}: {row}"
        );
        gone = now;
        if !hints_whole {
            assert_eq!(
                now,
                order.len(),
                "a hint dropped before the facts at {w}: {row}"
            );
        }
    }
    assert_eq!(gone, order.len());
}

#[test]
fn the_model_is_text_the_other_facts_muted_and_hint_words_faint() {
    let s = hinted(&copy::row_keys());
    let row = s.row(d(5000), FULL, &facts(), layout(160));
    let text = Some(Ink::Token(Token::Text));
    let muted = Some(Ink::Token(Token::TextMuted));
    let faint = Some(Ink::Token(Token::TextFaint));
    assert_eq!(ink_of(&row, "sonnet-5-5"), vec![text]);
    assert_eq!(ink_of(&row, "low"), vec![muted]);
    assert_eq!(ink_of(&row, "cached 92%"), vec![muted]);
    assert_eq!(ink_of(&row, "Ready"), vec![muted]);
    assert_eq!(ink_of(&row, "enter"), vec![muted]);
    assert_eq!(ink_of(&row, "del"), vec![muted]);
    assert_eq!(ink_of(&row, " edit"), vec![faint]);
    assert_eq!(ink_of(&row, " remove"), vec![faint]);
}

#[test]
fn ready_is_muted_like_every_status_sentence() {
    let s = Status::new();
    let row = s.row(d(0), FULL, &facts(), layout(80));
    assert_eq!(
        ink_of(&row, "Ready"),
        vec![Some(Ink::Token(Token::TextMuted))]
    );
}

#[test]
fn below_60_columns_a_busy_row_shows_its_sentence_and_esc_to_stop_only() {
    let mut s = long_turn(16, false);
    s.keys(&copy::row_keys());
    for w in 20..STOP_ONLY_BELOW {
        let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
        assert!(
            row.trim_end().ends_with(&copy::esc_to_stop().text()),
            "{w}: {row}"
        );
        assert!(
            !row.contains("sonnet") && !row.contains("edits"),
            "{w}: {row}"
        );
    }
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(STOP_ONLY_BELOW)));
    assert!(row.contains("   esc to stop"), "{row}");
    let mut quiet = long_turn(16, false);
    quiet.keys(&[KeyHint::new("tab", "queue")]);
    let row = line(&quiet.row(d(65_000), SIXTEEN, &facts(), layout(79)));
    assert!(row.contains("   esc to stop \u{b7} tab queue"), "{row}");
    let idle = hinted(&copy::row_keys());
    let row = line(&idle.row(d(5000), FULL, &facts(), layout(50)));
    assert!(row.contains("Ready   enter edit"), "{row}");
}

#[test]
fn a_key_hint_splits_its_key_from_its_words() {
    let hint = copy::tab_queues();
    assert_eq!(hint.key(), "tab");
    assert_eq!(hint.does(), "queue for the next turn");
    assert_eq!(hint.text(), "tab queue for the next turn");
    let note = KeyHint::note("editing a queued message");
    assert_eq!(note.key(), "editing a queued message");
    assert_eq!(note.does(), "");
    assert_eq!(copy::esc_to_stop().text(), "esc to stop");
}

fn busy_kinds() -> Vec<(&'static str, Status)> {
    let mut preparing = long_turn(0, true);
    preparing.apply(&tool(3), d(60_000), SIXTEEN);
    let mut still = long_turn(0, true);
    still.hint(
        "Wisp is still answering. Send this after, or press esc to stop it.",
        d(60_000),
        None,
    );
    let mut asking = long_turn(0, true);
    asking.apply(
        &AgentEvent::QuestionAsked {
            call_id: "q".to_owned(),
            questions: Vec::new(),
        },
        d(60_000),
        SIXTEEN,
    );
    vec![
        ("thinking", long_turn(0, false)),
        ("answering", long_turn(16, true)),
        ("preparing", preparing),
        ("still answering", still),
        ("needs you", asking),
    ]
}

#[test]
fn esc_to_stop_stays_on_a_busy_row_at_every_width_from_20_to_200() {
    let key_sets = [
        ("ctrl-b", vec![copy::background_key()]),
        ("edit waiting", copy::edit_waiting("\u{2191}")),
        ("editing", copy::editing("next-turn")),
    ];
    for (name, s) in busy_kinds() {
        assert!(s.is_running(), "{name}");
        let notice = s.shown_hint(d(65_000)).is_some();
        for (keys_name, keys) in &key_sets {
            let mut s = s.clone();
            s.keys(keys);
            for w in 20..=200 {
                for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
                    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), Layout::new(w, set)));
                    let at = format!("{name} with {keys_name} at {w}: {row}");
                    if !notice {
                        assert!(row.contains("esc "), "{at}");
                        let sentence: String = row.chars().skip(6).collect();
                        assert!(!sentence.starts_with('\u{2026}'), "{at}");
                        assert!(!sentence.starts_with("..."), "{at}");
                    }
                    if row.contains("editing a next-turn message") {
                        let sep = Glyphs::of(set).separator;
                        let want = format!("editing a next-turn message {sep} esc cancel");
                        assert!(row.contains(&want), "{at}");
                    }
                    let sep = Glyphs::of(set).separator;
                    if row.contains("ctrl-b") && !row.contains(&format!("{sep} esc to stop")) {
                        assert!(
                            row.contains(&format!("   esc to stop {sep} ctrl-b")),
                            "{at}"
                        );
                    }
                    assert!(row.chars().count() <= w, "{at}");
                }
            }
        }
    }
}

#[test]
fn esc_to_stop_leads_the_hints_after_a_sentence_that_is_not_the_working_line() {
    let mut s = long_turn(0, true);
    s.apply(&tool(3), d(60_000), SIXTEEN);
    s.keys(&[copy::background_key()]);
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(70)));
    assert!(
        row.contains("Writing a x call \u{b7} 5s   esc to stop \u{b7} ctrl-b background"),
        "{row}"
    );
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(24)));
    assert!(row.trim_end().ends_with("   esc to stop"), "{row}");
    assert!(row.contains('\u{2026}'), "{row}");
}

#[test]
fn the_model_is_bold_without_colour_and_plain_in_ansi_16() {
    use crate::theme::{Depth, Mode, Theme};
    let s = Status::new();
    let row = s.row(d(0), FULL, &facts(), layout(80));
    let model = row
        .spans
        .iter()
        .find(|span| span.text.contains("sonnet-5-5"))
        .map(|span| span.look)
        .unwrap_or_default();
    let low = row
        .spans
        .iter()
        .find(|span| span.text.contains("low"))
        .map(|span| span.look)
        .unwrap_or_default();
    for mode in [Mode::Dark, Mode::Light] {
        let none = Theme::new(mode, Depth::None);
        assert!(
            none.style(&model)
                .add_modifier
                .contains(ratatui_core::style::Modifier::BOLD)
        );
        assert!(
            !none
                .style(&low)
                .add_modifier
                .contains(ratatui_core::style::Modifier::BOLD)
        );
        for depth in [Depth::Ansi16, Depth::Ansi256, Depth::TrueColor] {
            let theme = Theme::new(mode, depth);
            assert_eq!(
                theme.style(&model),
                theme.style(&Look::fg(Token::Text)),
                "{depth:?}"
            );
        }
    }
}

#[test]
fn an_empty_text_fact_takes_no_room_and_no_separator() {
    let facts = vec![
        RowFact::Text("sonnet-5-5".to_owned()),
        RowFact::Text(String::new()),
        RowFact::Text("low".to_owned()),
    ];
    let row = line(&status_row(
        &Status::new().orb(d(0), FULL),
        vec![StyledSpan::plain("Ready")],
        &[],
        &facts,
        layout(40),
    ));
    assert!(row.trim_end().ends_with("sonnet-5-5   low"), "{row}");
    assert!(!row.contains("\u{b7}"), "{row}");
}

#[test]
fn the_credential_goes_after_requests_and_cache_and_before_the_model() {
    let label = CredentialLabel {
        endpoint: Some("direct".to_owned()),
        name: "Anthropic API key",
        host: None,
    };
    let facts = vec![
        RowFact::Text("sonnet-5-5".to_owned()),
        RowFact::Text("low".to_owned()),
        RowFact::Credential(label),
        RowFact::Text("2 requests".to_owned()),
        RowFact::Text("cached 92%".to_owned()),
        RowFact::Speed("48 tok/s".to_owned()),
    ];
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let cx = MotionCtx {
            ascii: set == GlyphSet::Ascii,
            ..FULL
        };
        let orb = Status::new().orb(d(0), cx);
        let at = |w: usize| {
            line(&status_row(
                &orb,
                vec![StyledSpan::plain("Ready")],
                &[],
                &facts,
                Layout::new(w, set),
            ))
        };
        let row = at(80);
        assert!(row.contains("Anthropic API key"), "{row}");
        assert!(!row.contains("cached 92%"), "{row}");
        let mut seen_short = false;
        let mut rate_without_cache = false;
        let mut seen_cache = false;
        for w in 20..=120 {
            let row = at(w);
            if row.contains("cached 92%") {
                seen_cache = true;
                assert!(
                    row.contains("tok/s") && row.contains("2 requests"),
                    "{w}: {row}"
                );
            }
            rate_without_cache |= row.contains("tok/s") && !row.contains("cached 92%");
            if row.contains("cached 92%") || row.contains("2 requests") {
                assert!(row.contains("Anthropic API key"), "{w}: {row}");
            }
            if row.contains("direct") && !row.contains("Anthropic API key") {
                seen_short = true;
                assert!(!row.contains("2 requests"), "{w}: {row}");
            }
        }
        assert!(seen_short && rate_without_cache && seen_cache);
    }
}

#[test]
fn a_waiting_row_drops_the_cache_note_before_it_cuts_the_clock() {
    let s = asking(CacheTtl::OneHour, 1000, 1000);
    let t = 1000 + (50 * 60 + 1) * 1000;
    let clock = copy::clock(50 * 60 + 1);
    let note = "cache expires in 10 min";
    let mut dropped = 0;
    for w in 40..=120 {
        let row = line(&s.row(d(t), FULL, &facts(), layout(w)));
        let whole = row.contains(&format!("\u{b7} {clock}   {}", copy::esc_to_stop().text()))
            || row.contains(&format!(
                "\u{b7} {clock} \u{b7} {note}   {}",
                copy::esc_to_stop().text()
            ));
        assert!(whole, "{w}: {row}");
        if (45..=56).contains(&w) {
            assert!(!row.contains("cache"), "{w}: {row}");
            dropped += 1;
        }
    }
    assert_eq!(dropped, 12);
}

#[test]
fn a_narrowed_host_never_comes_back_once_cost_or_cache_has_dropped() {
    let label = CredentialLabel {
        endpoint: Some("manifest".to_owned()),
        name: "Gateway",
        host: Some("gateway.internal.example.com:8443".to_owned()),
    };
    let facts = vec![
        RowFact::Text("sonnet-5-5".to_owned()),
        RowFact::Text("low".to_owned()),
        RowFact::Credential(label),
        RowFact::Text("$0.21".to_owned()),
        RowFact::Text("cached 92%".to_owned()),
    ];
    let orb = Status::new().orb(d(0), FULL);
    let mut narrowed = false;
    for w in 20..=160 {
        let row = line(&status_row(
            &orb,
            vec![StyledSpan::plain("Ready")],
            &[],
            &facts,
            layout(w),
        ));
        narrowed |= row.contains("gatew") && row.contains('\u{2026}');
        if !row.contains("$0.21") || !row.contains("cached 92%") {
            assert!(
                !row.contains("gatew") && !row.contains("8443"),
                "{w}: {row}"
            );
        }
    }
    assert!(narrowed);
}

#[test]
fn the_host_narrows_down_to_exactly_its_minimum() {
    let label = CredentialLabel {
        endpoint: Some("manifest".to_owned()),
        name: "Gateway",
        host: Some("gateway.internal.example.com:8443".to_owned()),
    };
    let glyphs = Glyphs::of(GlyphSet::Unicode);
    let mode = layout(80).width_mode;
    let lead = width_of("manifest \u{b7} Gateway \u{b7} ");
    let kind = "manifest \u{b7} Gateway";
    for shown in [
        label.narrow_host(lead + HOST_MIN, &glyphs, mode),
        label.fit(lead + HOST_MIN, &glyphs, mode),
    ] {
        let host = shown
            .strip_prefix(&format!("{kind} \u{b7} "))
            .unwrap_or_default();
        assert_eq!(width_of(host), HOST_MIN, "{shown}");
        assert!(host.contains('\u{2026}'), "{shown}");
    }
    assert_eq!(label.narrow_host(lead + HOST_MIN - 1, &glyphs, mode), kind);
    assert_eq!(label.fit(lead + HOST_MIN - 1, &glyphs, mode), kind);
}

#[test]
fn a_hint_that_already_binds_esc_keeps_esc_to_stop_off_the_row() {
    for keys in [copy::editing("next-turn"), copy::edit_waiting("\u{2191}")] {
        let mut s = long_turn(0, true);
        s.apply(&tool(3), d(60_000), SIXTEEN);
        s.keys(&keys);
        for w in 20..=200 {
            let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
            assert!(
                row.matches(&*copy::esc_to_stop().text()).count() <= 1,
                "{w}: {row}"
            );
            if (40..STOP_ONLY_BELOW).contains(&w) || w >= 120 {
                assert!(row.contains("esc "), "{w}: {row}");
            }
        }
    }
}

#[test]
fn a_notice_keeps_its_room_and_esc_to_stop_returns_when_it_expires() {
    let mut s = long_turn(0, true);
    s.apply(&tool(3), d(60_000), SIXTEEN);
    s.hint(copy::NO_CLIPBOARD, d(60_000), Some(Duration::from_secs(3)));
    let row = line(&s.row(d(61_000), SIXTEEN, &facts(), layout(80)));
    assert!(row.contains(copy::NO_CLIPBOARD), "{row}");
    let row = line(&s.row(d(64_000), SIXTEEN, &facts(), layout(80)));
    assert!(!row.contains(copy::NO_CLIPBOARD), "{row}");
    assert!(row.contains("   esc to stop"), "{row}");
    let long = "x".repeat(70);
    s.hint(&long, d(65_000), None);
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(80)));
    assert!(row.contains(&long), "{row}");
    assert!(!row.contains(&copy::esc_to_stop().text()), "{row}");
}

#[test]
fn the_esc_hint_follows_a_state_note_and_outlasts_it() {
    let mut s = long_turn(0, false);
    s.keys(&copy::editing("next-turn"));
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(140)));
    assert!(
        row.contains("   editing a next-turn message \u{b7} esc cancel \u{b7} enter save"),
        "{row}"
    );
    let mut note_gone = false;
    for w in STOP_ONLY_BELOW..=140 {
        let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
        assert!(row.contains("esc cancel"), "{w}: {row}");
        note_gone |= !row.contains("editing a next-turn message");
    }
    assert!(note_gone);
}

fn typing_keys() -> Vec<KeyHint> {
    vec![KeyHint::note(copy::typing_into(3)), copy::go_back_hint()]
}

#[test]
fn the_exit_hint_takes_the_place_of_esc_to_stop_from_22_columns() {
    for (kind, mut s) in busy_kinds() {
        s.keys(&typing_keys());
        for w in 22..=200 {
            let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
            let hints = row.replace("press esc to stop", "");
            assert!(!hints.contains("esc to stop"), "{kind} {w}: {row}");
            let notice_holds_the_row = kind == "still answering" && w < 100;
            assert!(
                row.contains("f12 to go back") || notice_holds_the_row,
                "{kind} {w}: {row}"
            );
        }
    }
}

#[test]
fn an_idle_row_attached_to_a_process_keeps_its_exit_hint_from_22_columns() {
    let mut s = Status::new();
    s.keys(&typing_keys());
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(140)));
    assert!(row.contains("typing into 3 \u{b7} f12 to go back"), "{row}");
    for w in 22..=200 {
        let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
        assert!(row.contains("f12 to go back"), "{w}: {row}");
        assert!(!row.contains("esc to stop"), "{w}: {row}");
        assert!(
            row.contains("typing into 3 \u{b7} f12 to go back") || !row.contains("typing into"),
            "{w}: {row}"
        );
    }
}

#[test]
fn the_exit_hint_comes_right_after_its_note_and_outlasts_it() {
    let mut s = long_turn(0, false);
    s.keys(&typing_keys());
    let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(140)));
    assert!(
        row.contains("   typing into 3 \u{b7} f12 to go back"),
        "{row}"
    );
    let mut note_gone = false;
    for w in 20..=140 {
        let row = line(&s.row(d(65_000), SIXTEEN, &facts(), layout(w)));
        note_gone |= !row.contains("typing into 3");
    }
    assert!(note_gone);
}

#[test]
fn an_exit_hint_survives_mapping_and_a_plain_hint_is_not_one() {
    let exit = copy::go_back_hint();
    assert!(exit.map(str::to_owned).is_exit());
    assert!(!copy::take_over_hint().is_exit());
    assert!(!KeyHint::note(copy::typing_into(3)).is_exit());
}

#[test]
fn a_password_prompt_keeps_esc_to_stop_after_its_note_and_keys_f12() {
    let mut s = long_turn(0, false);
    s.keys(&[
        KeyHint::note(copy::waiting_for_password(5)),
        copy::take_over_hint(),
    ]);
    let styled = s.row(d(65_000), SIXTEEN, &facts(), layout(140));
    let row = line(&styled);
    assert!(
        row.contains("5 is waiting for a password \u{b7} esc to stop \u{b7} f12 to take over"),
        "{row}"
    );
    let key = styled
        .spans
        .iter()
        .find(|span| span.text == copy::F12_KEY)
        .map(|span| span.look.fg);
    assert_eq!(key, Some(Some(Ink::Token(Token::TextMuted))), "{styled:?}");
}

fn dock() -> Dock {
    Dock {
        name: "Account".to_owned(),
        column: "name".to_owned(),
        keys: vec![KeyHint::new("enter", "use"), copy::esc(copy::CLOSE_WORD)],
        ..Dock::default()
    }
}

fn many(n: usize, selected: usize) -> Menu {
    Menu {
        titles: [
            "Account".to_owned(),
            "credential".to_owned(),
            "next turn".to_owned(),
        ],
        choices: (0..n)
            .map(|i| Choice {
                mark: None,
                current: i == 0,
                label: format!("ep{i}"),
                detail: "Claude API".to_owned(),
                effect: "cache rebuild".to_owned(),
                enabled: true,
                line: None,
            })
            .collect(),
        selected: Some(selected),
        notes: vec![],
        shape: Shape::default(),
    }
}

#[test]
fn the_docked_panel_has_a_rule_a_title_headers_rows_and_a_key_row() {
    let rows = docked_rows(&many(3, 1), &dock(), 20, layout(80));
    let texts: Vec<String> = rows.iter().map(line).collect();
    assert_eq!(texts[0], "\u{2500}".repeat(80));
    assert_eq!(rows[0].spans[0].look, Look::fg(Token::Line));
    assert_eq!(texts[1].trim_end(), "   Account", "{texts:#?}");
    let name = rows[1].spans.iter().find(|s| s.text == "Account").unwrap();
    assert_eq!(name.look, Look::fg(Token::Text).bold());
    assert!(texts[2].starts_with("   name"), "{texts:#?}");
    assert!(
        rows[2]
            .spans
            .iter()
            .filter(|s| !s.text.trim().is_empty())
            .all(|s| s.look == Look::fg(Token::TextFaint)),
        "{:?}",
        rows[2]
    );
    assert!(texts[3].starts_with("   ep0 \u{2713}"), "{texts:#?}");
    assert!(texts[4].starts_with(" \u{203A} ep1"), "{texts:#?}");
    assert_eq!(texts.len(), 7, "{texts:#?}");
    assert_eq!(texts[6].trim_end(), "   enter use \u{b7} esc close");
    let keys = &rows[6].spans;
    assert!(
        keys.iter()
            .any(|s| s.text == "enter" && s.look == Look::fg(Token::TextMuted))
    );
    assert!(
        keys.iter()
            .any(|s| s.text == " use" && s.look == Look::fg(Token::TextFaint))
    );
    let keyed = |exit_short: &str| {
        let keys = vec![
            KeyHint::new("enter", "start"),
            KeyHint::new("type", "to filter"),
            copy::esc("stay on a-very-long-account-name"),
        ];
        let dock = Dock {
            keys,
            exit_short: exit_short.to_owned(),
            ..dock()
        };
        let rows = docked_rows(&many(3, 1), &dock, 20, layout(40));
        rows.last().map(line).unwrap().trim_end().to_owned()
    };
    assert_eq!(keyed("stay"), "   enter start \u{b7} esc stay");
    assert_eq!(keyed(""), "   esc stay on a-very-long-account-name");
}

#[test]
fn a_short_panel_scrolls_its_list_with_more_rows_and_keeps_the_selection() {
    let rows = docked_rows(&many(10, 5), &dock(), 9, layout(80));
    let texts: Vec<String> = rows.iter().map(line).collect();
    assert_eq!(texts.len(), 9, "{texts:#?}");
    assert_eq!(texts[3].trim_end(), "   \u{2191} 3 more", "{texts:#?}");
    assert!(texts[6].starts_with(" \u{203A} ep5"), "{texts:#?}");
    assert_eq!(texts[7].trim_end(), "   \u{2193} 4 more", "{texts:#?}");
    let top = docked_rows(&many(10, 0), &dock(), 9, layout(80));
    let texts: Vec<String> = top.iter().map(line).collect();
    assert!(texts[3].starts_with(" \u{203A} ep0"), "{texts:#?}");
    assert_eq!(texts[7].trim_end(), "   \u{2193} 6 more", "{texts:#?}");
    let end = docked_rows(&many(10, 9), &dock(), 9, layout(80));
    let texts: Vec<String> = end.iter().map(line).collect();
    assert_eq!(texts[3].trim_end(), "   \u{2191} 6 more", "{texts:#?}");
    assert!(texts[7].starts_with(" \u{203A} ep9"), "{texts:#?}");
    let tight = docked_rows(&many(10, 7), &dock(), 3, layout(80));
    let texts: Vec<String> = tight.iter().map(line).collect();
    assert_eq!(texts.len(), 3, "{texts:#?}");
    assert!(texts[2].starts_with(" \u{203A} ep7"), "{texts:#?}");
    assert!(docked_rows(&many(10, 0), &dock(), 2, layout(80)).is_empty());
}

#[test]
fn the_dock_leaves_the_transcript_three_rows_when_it_can() {
    assert_eq!(dock_room(20), 17);
    assert_eq!(dock_room(6), 3);
    assert_eq!(dock_room(4), 3);
    assert_eq!(dock_room(2), 2);
}

#[test]
fn the_current_mark_is_spaced_from_the_longest_name() {
    let mut menu = many(3, 2);
    menu.choices[2].current = true;
    menu.choices[0].current = false;
    menu.choices[2].label = "sonnet-5-5".to_owned();
    let mark_at = |menu: &Menu| {
        let texts: Vec<String> = menu_rows(menu, 20, layout(80)).iter().map(line).collect();
        let row = texts.iter().find(|t| t.contains("sonnet-5-5")).cloned();
        let row = row.unwrap_or_else(|| panic!("{texts:#?}"));
        assert!(
            row.contains("sonnet-5-5 \u{2713}  Claude API"),
            "{texts:#?}"
        );
        row.chars().position(|c| c == '\u{2713}')
    };
    for rows in [
        docked_rows(&menu, &dock(), 20, layout(80)),
        menu_rows(&menu, 20, layout(80)),
    ] {
        let texts: Vec<String> = rows.iter().map(line).collect();
        let row = texts
            .iter()
            .find(|t| t.contains("sonnet-5-5"))
            .unwrap_or_else(|| panic!("{texts:#?}"));
        assert!(row.contains("sonnet-5-5 \u{2713}"), "{texts:#?}");
    }
    let focused = mark_at(&menu);
    menu.selected = Some(0);
    assert_eq!(mark_at(&menu), focused);
    for c in &mut menu.choices {
        c.detail.clear();
        c.effect.clear();
    }
    let bare: Vec<String> = menu_rows(&menu, 20, layout(80)).iter().map(line).collect();
    assert!(
        bare.iter().any(|t| t.starts_with("   sonnet-5-5 \u{2713}")),
        "{bare:#?}"
    );
    menu.choices[0].detail = "Claude API".to_owned();
    let mixed: Vec<String> = menu_rows(&menu, 20, layout(80)).iter().map(line).collect();
    assert!(
        mixed.iter().any(|t| t.contains("ep0           Claude API")),
        "{mixed:#?}"
    );
    let mut long = many(2, 0);
    long.choices[1].current = true;
    long.choices[0].current = false;
    long.choices[1].label = "claude-research-team-subscription-eu".to_owned();
    let rows: Vec<String> = menu_rows(&long, 20, layout(40)).iter().map(line).collect();
    assert_eq!(
        rows[2], "   claude-research-team-subscription-\u{2026} \u{2713}",
        "{rows:#?}"
    );
    long.choices[1].label = "sonnet-5-5".to_owned();
    let rows: Vec<String> = menu_rows(&long, 20, layout(40)).iter().map(line).collect();
    assert!(
        rows[2].starts_with("   sonnet-5-5 \u{2713}  cache rebuild"),
        "{rows:#?}"
    );
}

#[test]
fn a_narrow_key_row_drops_whole_hints_and_keeps_esc() {
    let mut keyed = dock();
    keyed.keys = vec![
        KeyHint::new("enter", "use"),
        KeyHint::new("type", "to filter"),
        copy::arrows(GlyphSet::Ascii),
        copy::esc(copy::CLOSE_WORD),
    ];
    let keys = |dock: &Dock, width: usize| {
        let rows = docked_rows(&many(3, 0), dock, 20, layout(width));
        line(rows.last().unwrap()).trim_end().to_owned()
    };
    assert_eq!(
        keys(&keyed, 80),
        "   enter use \u{b7} type to filter \u{b7} up/down move \u{b7} esc close"
    );
    assert_eq!(
        keys(&keyed, 50),
        "   enter use \u{b7} type to filter \u{b7} esc close"
    );
    assert_eq!(keys(&keyed, 40), "   enter use \u{b7} esc close");
    assert_eq!(keys(&keyed, 23), "   esc close");
    keyed.keys.pop();
    assert_eq!(keys(&keyed, 40), "   enter use \u{b7} type to filter");
    assert_eq!(keys(&keyed, 16), "   enter use");
}

#[test]
fn the_window_keeps_the_selected_row_at_two_three_and_four_rows() {
    for rows in 2..=4 {
        for selected in 0..10 {
            let w = menu::window(10, rows, selected, false);
            assert!(
                (w.first..w.first + w.shown).contains(&selected),
                "{rows} rows, row {selected}: {w:?}"
            );
            assert_eq!(
                w.shown + usize::from(w.above > 0) + usize::from(w.below > 0),
                rows,
                "{rows} rows, row {selected}: {w:?}"
            );
            assert_eq!(rows >= 3, w.above + w.below > 0, "{rows} rows: {w:?}");
        }
    }
}

#[test]
fn a_tight_dock_drops_the_key_row_before_the_headers() {
    let rows = docked_rows(&many(3, 0), &dock(), 6, layout(80));
    let texts: Vec<String> = rows.iter().map(line).collect();
    assert_eq!(texts.len(), 6, "{texts:#?}");
    assert!(texts[2].starts_with("   name"), "{texts:#?}");
    assert!(texts.iter().all(|t| !t.contains("esc close")), "{texts:#?}");
}

#[test]
fn a_short_dock_gives_the_list_three_rows_before_the_header_key_row_and_notes() {
    let mut menu = many(8, 4);
    menu.notes = vec![("type any model ID".to_owned(), Tone::Faint)];
    let rows = |room: usize| -> Vec<String> {
        docked_rows(&menu, &dock(), room, layout(80))
            .iter()
            .map(line)
            .collect()
    };
    let five = rows(5);
    assert_eq!(five.len(), 5, "{five:#?}");
    assert!(five[2].contains("\u{2191} "), "{five:#?}");
    assert!(five[3].starts_with(" \u{203A} ep4"), "{five:#?}");
    assert!(five[4].contains("\u{2193} "), "{five:#?}");
    let six = rows(6);
    assert!(six[2].starts_with("   name"), "{six:#?}");
    let seven = rows(7);
    assert!(seven[6].contains("esc close"), "{seven:#?}");
    assert!(seven.iter().all(|r| !r.contains("type any")), "{seven:#?}");
    let eight = rows(8);
    assert!(eight.iter().any(|r| r.contains("type any")), "{eight:#?}");
    let four = rows(4);
    assert!(four[1].contains("Account  5 of 8"), "{four:#?}");
    assert!(
        four.iter().any(|r| r.starts_with(" \u{203A} ep4")),
        "{four:#?}"
    );
}

mod todos {
    use wisp_protocol::{TodoItem, TodoProgress, TodoStatus};

    use super::*;

    fn updated(statuses: &[TodoStatus]) -> AgentEvent {
        AgentEvent::TodosUpdated {
            call_id: Some("t1".to_owned()),
            items: statuses
                .iter()
                .enumerate()
                .map(|(i, status)| TodoItem {
                    text: format!("step {i}"),
                    status: *status,
                    section: None,
                })
                .collect(),
        }
    }

    fn five() -> AgentEvent {
        use TodoStatus::{Cancelled, Completed, InProgress, Pending};
        updated(&[Completed, InProgress, Pending, Cancelled, Completed])
    }

    #[test]
    fn the_tally_counts_completed_items_while_any_is_open_and_the_row_never_shows_them() {
        let mut s = Status::new();
        let row = |s: &Status, w: usize| line(&s.row(d(0), FULL, &facts(), layout(w)));
        s.apply(&five(), d(0), FULL);
        let two_of_five = Some(TodoProgress {
            completed: 2,
            total: 5,
        });
        assert_eq!(s.tally().todo_progress(), two_of_five);
        for w in [120, 80, 40] {
            assert!(!row(&s, w).contains("todo"), "{}", row(&s, w));
        }
        s.apply(
            &updated(&[TodoStatus::Completed, TodoStatus::Cancelled]),
            d(0),
            FULL,
        );
        assert_eq!(s.tally().todo_progress(), None);
        let mut resumed = Status::new();
        resumed.replayed(&five());
        assert_eq!(resumed.tally().todo_progress(), two_of_five);
        resumed.replayed(&AgentEvent::TodosUpdated {
            call_id: None,
            items: Vec::new(),
        });
        assert_eq!(resumed.tally().todo_progress(), None);
    }
}

#[test]
fn each_queue_reason_has_its_agents_word() {
    use wisp_protocol::{AgentState, QueueReason};
    let word = |reason| copy::agent_state(AgentState::Queued { reason });
    assert_eq!(word(QueueReason::Concurrency), "queued: running limit");
    assert_eq!(word(QueueReason::Stagger), "queued: warming");
    assert_eq!(word(QueueReason::Retry), "queued: retrying");
    assert_eq!(word(QueueReason::Unknown), "queued");
}

#[test]
fn subscription_and_api_credentials_are_labelled() {
    for (credential, provider, label) in [
        (Credential::ChatGptPlan, "OpenAI", "ChatGPT subscription"),
        (
            Credential::AnthropicPlan,
            "Anthropic",
            "Claude subscription",
        ),
    ] {
        let facts = Facts {
            credential: Some(credential),
            provider: provider.into(),
            ..facts()
        };
        assert_eq!(facts.credential().unwrap().name, label);
    }
    let facts = Facts {
        credential: Some(Credential::ApiKey),
        provider: "OpenAI".into(),
        ..facts()
    };
    assert_eq!(facts.credential().unwrap().name, "OpenAI API");
}

#[test]
fn the_status_line_shows_no_cost_for_any_billing_at_40_80_and_120() {
    for billing in [
        wisp_protocol::Billing::Metered,
        wisp_protocol::Billing::ChatGptPlan,
        wisp_protocol::Billing::AnthropicPlan,
        wisp_protocol::Billing::Mixed,
    ] {
        let mut status = answered(FULL);
        let mut outcome = completed(StopReason::EndTurn, true);
        if let TurnOutcome::Completed {
            cost: Some(cost), ..
        } = &mut outcome
        {
            cost.turn_billing = billing;
            cost.session_billing = billing;
            cost.turn_lower_bound = true;
        }
        status.apply(&ended(outcome, 1500), d(2000), FULL);
        for w in [40, 80, 120] {
            for at in [2000, 9000] {
                let row = line(&status.row(d(at), FULL, &facts(), layout(w)));
                assert!(!row.contains('$'), "{billing:?} {w}: {row}");
                assert!(!row.contains("plan"), "{billing:?} {w}: {row}");
                assert!(!row.contains("mixed"), "{billing:?} {w}: {row}");
            }
        }
    }
}

#[test]
fn rebuild_and_switch_estimates_mark_the_amount_but_not_unpriced_values() {
    for billing in [
        wisp_protocol::Billing::ChatGptPlan,
        wisp_protocol::Billing::AnthropicPlan,
    ] {
        for (rebuild, line) in [
            (Rebuild::Usd(0.3), "cache rebuild ~$0.30, subscription"),
            (
                Rebuild::UpToUsd(0.3),
                "cache rebuild up to ~$0.30, subscription",
            ),
        ] {
            let billed = rebuild.billed(billing);
            assert_eq!(billed.line(GlyphSet::Unicode), line);
            assert!(!billed.unpriced().line(GlyphSet::Unicode).contains('$'));
        }
    }
    let work = SwitchWork {
        usd: Some(0.3),
        cause: SwitchCause::Convert,
        alone: false,
    };
    assert!(work.text(GlyphSet::Unicode).contains("~$0.30 "));
}

#[test]
fn usage_never_replaces_a_narrow_retry_sentence_or_priority_facts() {
    let mut status = Status::new();
    status.submitted(d(0), FULL);
    status.apply(
        &AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 20_000,
            reason: "overloaded".into(),
            step: 1,
            kind: ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        d(0),
        FULL,
    );
    for width in [40, 79, 80, 120] {
        status.usage_candidates(vec![crate::plan_usage::UsageCandidate {
            line: StyledLine::new(vec![StyledSpan::new("! /usage", Token::TextMuted)]),
            measured: false,
            warns: true,
        }]);
        let shown = line(&status.row(d(0), FULL, &facts(), layout(width)));
        assert!(shown.contains("Retrying in 20 s"), "{width}: {shown}");
        assert!(shown.contains("esc"), "{width}: {shown}");
        status.usage_candidates(vec![]);
        let without = line(&status.row(d(0), FULL, &facts(), layout(width)));
        if without.contains(&facts().short_model()) {
            assert!(shown.contains(&facts().short_model()), "{width}: {shown}");
        }
    }
}

fn weekly_usage(glyphs: &Glyphs) -> Vec<crate::plan_usage::UsageCandidate> {
    let hinted = format!("week 43% used {} /usage", glyphs.separator);
    [hinted.as_str(), "week 43% used"]
        .map(|text| crate::plan_usage::UsageCandidate {
            line: StyledLine::new(vec![StyledSpan::new(text, Token::TextMuted)]),
            measured: true,
            warns: false,
        })
        .to_vec()
}

#[test]
fn plan_usage_survives_context_and_spend_after_a_turn_at_common_widths() {
    let mut status = answered(FULL);
    let mut outcome = completed(StopReason::EndTurn, true);
    if let TurnOutcome::Completed {
        cost: Some(cost),
        usage,
        ..
    } = &mut outcome
    {
        cost.turn_usd = 0.0251;
        cost.session_usd = 0.0251;
        cost.turn_billing = wisp_protocol::Billing::ChatGptPlan;
        cost.session_billing = wisp_protocol::Billing::ChatGptPlan;
        cost.turn_plan_usd = Some(0.0251);
        cost.session_plan_usd = Some(0.0251);
        usage.billing = wisp_protocol::Billing::ChatGptPlan;
    }
    status.apply(&context_changed(5000, true), d(1500), FULL);
    status.apply(&ended(outcome, 1500), d(2000), FULL);
    let mut facts = facts();
    facts.model = "gpt-6-astra".into();
    facts.credential = Some(Credential::ChatGptPlan);
    status.usage_candidates(weekly_usage(&UNICODE));
    let mut rows = Vec::new();
    for width in [80, 100, 106, 120] {
        let row = line(&status.row(d(20_000), FULL, &facts, layout(width)));
        assert!(row.contains("week 43% used"), "{width}: {row}");
        if row.contains("/usage") {
            assert!(row.contains("cached"), "{width}: {row}");
        }
        assert!(row.contains("astra low"), "{width}: {row}");
        assert!(!row.contains("at API prices"), "{width}: {row}");
        rows.push(format!("{width}: {row}"));
    }
    insta::assert_snapshot!(rows.join("\n"));
}

const CLAUDE_MAX_UNICODE: &str = "\
80 1 | ⢎⠶⡰  Lantern-lighting… 9s   esc to stop            ● claude-max · subscription |
80 2 | ⢎⠶⡰  Lantern-lighting… ≈42 tok/s   esc to stop     ● claude-max · subscription |
80 3 | ⢎⠶⡰  Writing a command · 14s   esc to stop         ● claude-max · subscription |
80 4 | ⢎⠶⡰  Running cargo test · 7s   esc to stop         ● claude-max · subscription |
80 5 | ⢎⠶⡰  Lantern-lighting… 31s   esc to stop           ● claude-max · subscription |
80 6 | ⢎⠶⡰  Ready             ● claude-max · subscription   opus-5-5 high · 159k/850k |
80 s | ⢎⠶⡰  Ready             ● claude-max · subscription   opus-5-5 high · 159k/850k |
106 1 | ⢎⠶⡰  Lantern-lighting… 9s · thinking with high effort   esc to stop          ● claude-max · subscription |
106 2 | ⢎⠶⡰  Lantern-lighting… 16s · ↓ ≈180 tokens · ≈42 tok/s · esc to stop         ● claude-max · subscription |
106 3 | ⢎⠶⡰  Writing a command · 14s   esc to stop       ● claude-max · subscription   opus-5-5 high · 159k/850k |
106 4 | ⢎⠶⡰  Running cargo test · 7s   esc to stop       ● claude-max · subscription   opus-5-5 high · 159k/850k |
106 5 | ⢎⠶⡰  Lantern-lighting… 31s · ↓ ≈480 tokens   esc to stop          ● claude-max · subscription   opus-5-5 |
106 6 | ⢎⠶⡰  Ready   ● claude-max · subscription   opus-5-5 high · week 43% used · /usage · 159k/850k · 48 tok/s |
106 s | ⢎⠶⡰  Ready          ● claude-max · subscription   opus-5-5 high · week 43% used · 159k/850k · cached 93% |
120 1 | ⢎⠶⡰  Lantern-lighting… 9s · thinking with high effort   esc to stop             ● claude-max · subscription   opus-5-5 |
120 2 | ⢎⠶⡰  Lantern-lighting… 16s · ↓ ≈180 tokens · ≈42 tok/s · esc to stop            ● claude-max · subscription   opus-5-5 |
120 3 | ⢎⠶⡰  Writing a command · 14s   esc to stop     ● claude-max · subscription   opus-5-5 high · week 43% used · 159k/850k |
120 4 | ⢎⠶⡰  Running cargo test · 7s   esc to stop     ● claude-max · subscription   opus-5-5 high · week 43% used · 159k/850k |
120 5 | ⢎⠶⡰  Lantern-lighting… 31s · ↓ ≈480 tokens · thinking with high effort   esc to stop       ● claude-max · subscription |
120 6 | ⢎⠶⡰  Ready    ● claude-max · subscription   opus-5-5 high · week 43% used · /usage · 159k/850k · cached 93% · 48 tok/s |
120 s | ⢎⠶⡰  Ready               ● claude-max · subscription   opus-5-5 high · week 43% used · /usage · 159k/850k · cached 93% |";

const CLAUDE_MAX_ASCII: &str = "\
80 1 | (*)  Lantern-lighting... 9s   esc to stop          * claude-max - subscription |
80 2 | (*)  Lantern-lighting... ~42 tok/s   esc to stop   * claude-max - subscription |
80 3 | (*)  Writing a command - 14s   esc to stop         * claude-max - subscription |
80 4 | (*)  Running cargo test - 7s   esc to stop         * claude-max - subscription |
80 5 | (*)  Lantern-lighting... 31s   esc to stop         * claude-max - subscription |
80 6 | (*)  Ready             * claude-max - subscription   opus-5-5 high - 159k/850k |
80 s | (*)  Ready             * claude-max - subscription   opus-5-5 high - 159k/850k |
106 1 | (*)  Lantern-lighting... 9s - thinking with high effort   esc to stop        * claude-max - subscription |
106 2 | (*)  Lantern-lighting... 16s - v ~180 tokens - ~42 tok/s - esc to stop       * claude-max - subscription |
106 3 | (*)  Writing a command - 14s   esc to stop       * claude-max - subscription   opus-5-5 high - 159k/850k |
106 4 | (*)  Running cargo test - 7s   esc to stop       * claude-max - subscription   opus-5-5 high - 159k/850k |
106 5 | (*)  Lantern-lighting... 31s - v ~480 tokens   esc to stop        * claude-max - subscription   opus-5-5 |
106 6 | (*)  Ready   * claude-max - subscription   opus-5-5 high - week 43% used - /usage - 159k/850k - 48 tok/s |
106 s | (*)  Ready          * claude-max - subscription   opus-5-5 high - week 43% used - 159k/850k - cached 93% |
120 1 | (*)  Lantern-lighting... 9s - thinking with high effort   esc to stop           * claude-max - subscription   opus-5-5 |
120 2 | (*)  Lantern-lighting... 16s - v ~180 tokens - ~42 tok/s - esc to stop          * claude-max - subscription   opus-5-5 |
120 3 | (*)  Writing a command - 14s   esc to stop     * claude-max - subscription   opus-5-5 high - week 43% used - 159k/850k |
120 4 | (*)  Running cargo test - 7s   esc to stop     * claude-max - subscription   opus-5-5 high - week 43% used - 159k/850k |
120 5 | (*)  Lantern-lighting... 31s - v ~480 tokens - thinking with high effort   esc to stop     * claude-max - subscription |
120 6 | (*)  Ready    * claude-max - subscription   opus-5-5 high - week 43% used - /usage - 159k/850k - cached 93% - 48 tok/s |
120 s | (*)  Ready               * claude-max - subscription   opus-5-5 high - week 43% used - /usage - 159k/850k - cached 93% |";

fn claude_max() -> Facts {
    Facts {
        credential: Some(Credential::AnthropicPlan),
        model: "claude-opus-5-5".to_owned(),
        effort: "high".to_owned(),
        endpoint_name: Some("claude-max".to_owned()),
        endpoints: vec![subscribed("claude-max")],
        ..facts()
    }
}

fn claude_max_frames() -> Vec<(&'static str, Status, u64)> {
    let mut done = Status::new().with_seed(15);
    done.turn_effort(Some("high"));
    done.submitted(d(0), FULL);
    done.apply(&AgentEvent::turn_started(1), d(5), FULL);
    let u = Usage {
        output_tokens: 480,
        cache_read_input_tokens: 9_300,
        cache_creation_input_tokens: 660,
        cache_creation_1h_input_tokens: Some(660),
        ..usage()
    };
    streamed_step(
        &mut done,
        u,
        [100, 2_000, 10_100, 10_200],
        [text_block(), delta()],
    );
    let end = turn_ended(End {
        usage: Some(u),
        ..End::default()
    });
    done.apply(&end, d(10_300), FULL);
    let next = |events: Vec<(u64, AgentEvent)>| {
        let mut s = done.clone();
        s.submitted(d(100_000), FULL);
        s.apply(&AgentEvent::turn_started(2), d(100_005), FULL);
        for (at, event) in events {
            s.apply(&event, d(100_000 + at), FULL);
        }
        s
    };
    let text = |n: usize| AgentEvent::TextDelta {
        index: 1,
        text: "x".repeat(n),
    };
    let writing = AgentEvent::BlockStarted {
        index: 1,
        kind: BlockKind::ToolUse {
            name: "x".to_owned(),
            writing: Some("a command".to_owned()),
        },
    };
    let mut running = next(vec![]);
    running.name_call("c", "Running cargo test");
    running.apply(&AgentEvent::call_started("c", "~"), d(101_000), FULL);
    let mut unmeasured = done.clone();
    unmeasured.apply(&settings_changed(Changed::default()), d(10_400), FULL);
    vec![
        ("1", next(vec![(100, thinking_block())]), 109_500),
        (
            "2",
            next(vec![
                (10_000, text_block()),
                (10_100, text(216)),
                (13_500, text(504)),
            ]),
            116_500,
        ),
        ("3", next(vec![(1_000, writing)]), 115_500),
        ("4", running, 108_500),
        (
            "5",
            next(vec![
                (2_000, text_block()),
                (2_100, text(1_920)),
                (3_000, AgentEvent::StepStarted { step: 2 }),
            ]),
            131_500,
        ),
        ("6", done, 60_000),
        ("s", unmeasured, 60_000),
    ]
}

#[test]
fn the_mockup_frames_match_at_80_106_and_120() {
    let facts = claude_max();
    for (set, expected) in [
        (GlyphSet::Unicode, CLAUDE_MAX_UNICODE),
        (GlyphSet::Ascii, CLAUDE_MAX_ASCII),
    ] {
        let glyphs = Glyphs::of(set);
        let cx = MotionCtx {
            ascii: set == GlyphSet::Ascii,
            ..FULL
        };
        let orb = if cx.ascii {
            "(*)"
        } else {
            "\u{288E}\u{2836}\u{2870}"
        };
        let mut shown = vec![];
        for w in [80, 106, 120] {
            for (name, s, at) in claude_max_frames() {
                let mut s = s;
                s.usage_candidates(weekly_usage(&glyphs));
                let row = line(&s.row(d(at), cx, &facts, Layout::new(w, set)));
                let row = format!(" {orb}{}", row.chars().skip(4).collect::<String>());
                let pad = w.saturating_sub(crate::text::width(&row, Default::default()));
                shown.push(format!("{w} {name} |{row}{}|", " ".repeat(pad)));
            }
        }
        assert_eq!(shown.join("\n"), expected, "{set:?}");
    }
}

#[test]
fn a_long_model_outranks_the_usage_numbers_at_eighty_columns() {
    use wisp_protocol::plan_usage::{
        Percent, PlanUsage, UsageScope, UsageSource, UsageState, UsageWindow,
    };
    let now = 1800000000;
    let state = UsageState {
        snapshot: Some(
            PlanUsage::new(
                None,
                vec![UsageWindow {
                    key: "weekly".into(),
                    label: "Weekly".into(),
                    duration_seconds: Some(604800),
                    scope: UsageScope::All,
                    used_percent: Some(Percent::try_from(46.0).unwrap()),
                    resets_at: Some(now + 3600),
                    observed_at: now,
                    source: UsageSource::UsageEndpoint,
                }],
            )
            .unwrap(),
        ),
        loading: false,
        stale: false,
        error: None,
    };
    for credential in [
        None,
        Some(Credential::ChatGptPlan),
        Some(Credential::AnthropicPlan),
    ] {
        let mut facts = facts();
        facts.credential = credential;
        facts.model = "a-long-gateway-model-name-for-the-row".into();
        let mut status = Status::new();
        status.usage_candidates(crate::plan_usage::mini_candidates(
            &state,
            &facts.model,
            None,
            now,
            crate::plan_usage::UsageHint::Show,
            layout(80),
        ));
        let shown = line(&status.row(d(0), FULL, &facts, layout(80)));
        assert!(
            shown.contains("a-long-gateway-model-name-for-the-row"),
            "{shown}"
        );
        assert!(!shown.contains("/usage"), "{shown}");
        assert!(!shown.contains("ChatGPT plan"), "{shown}");
    }
}

fn chat_row(id: &str, topic: &str) -> wisp_protocol::ChatRow {
    let chat = crate::chrome::mockup_chat(
        id,
        topic,
        "claude-subscription",
        "claude-opus-5-5",
        "12m ago",
    );
    wisp_protocol::ChatRow { name: None, ..chat }
}

#[test]
fn every_chat_row_drops_the_same_details_so_names_keep_thirty_cells() {
    let all = crate::chrome::mockup_chats();
    let named: Vec<_> = all.iter().filter(|c| c.name.is_some()).cloned().collect();
    let mut busy_elsewhere = named.clone();
    busy_elsewhere[0].agents = 30;
    for (listed, cols, accounts, models) in [
        (&all, 120, 1, 3),
        (&all, 80, 0, 3),
        (&all, 40, 0, 0),
        (&named, 80, 1, 2),
        (&named, 40, 0, 0),
        (&busy_elsewhere, 80, 1, 2),
    ] {
        let view = ChatsView {
            chats: listed.iter().collect(),
            listed,
            focus: None,
            query: "",
            loading: false,
            below: Vec::new(),
            place: ChatsPlace::Start,
        };
        let lines: Vec<String> = chats_lines(&view, 30, layout(cols))
            .iter()
            .map(line)
            .collect();
        let shown = |needle: &str| lines.iter().filter(|l| l.contains(needle)).count();
        let found = (shown("claude-subscription"), shown("opus-5-5"));
        assert_eq!(found, (accounts, models), "{lines:#?}");
        let marks: Vec<usize> = lines
            .iter()
            .filter_map(|l| l.chars().position(|c| c == '\u{25cf}'))
            .collect();
        assert!(marks.len() > 2, "{lines:#?}");
        assert!(marks.iter().all(|&at| at == marks[0]), "{lines:#?}");
    }
}

#[test]
fn the_chats_list_never_draws_more_rows_than_its_room() {
    let chats: Vec<wisp_protocol::ChatRow> = (0..30)
        .map(|n| chat_row(&format!("c{n}"), &format!("Chat number {n}")))
        .collect();
    let below = vec![copy::RESUME_NEEDS_LOG.to_owned()];
    for room in 0..30 {
        let view = ChatsView {
            chats: chats.iter().collect(),
            listed: &chats,
            focus: Some(29),
            query: "",
            loading: false,
            below: below.clone(),
            place: ChatsPlace::InChat(Joins::default()),
        };
        let lines = chats_lines(&view, room, layout(80));
        assert!(lines.len() <= room, "{room}: {}", lines.len());
    }
    let mockups = crate::chrome::mockup_chats();
    let notes = [below[0].clone(), copy::NOT_SAVED.to_owned()];
    for room in [6, 7] {
        let view = ChatsView {
            chats: mockups.iter().collect(),
            listed: &mockups,
            focus: None,
            query: "",
            loading: false,
            below: notes.to_vec(),
            place: ChatsPlace::InChat(Joins::default()),
        };
        let lines = chats_lines(&view, room, layout(40));
        let text: Vec<String> = lines.iter().map(|l| line(l).trim().to_owned()).collect();
        let text = text.join(" ");
        for note in &notes {
            assert!(
                text.contains(note.as_str()) || !text.contains(&note[..12]),
                "{room}: {text}"
            );
        }
    }
}

fn subscribed(name: &str) -> EndpointFacts {
    EndpointFacts {
        credential: Some(Credential::AnthropicPlan),
        ..endpoint(name, false)
    }
}

fn two_subscriptions(pending: Option<&str>) -> Facts {
    Facts {
        credential: Some(Credential::AnthropicPlan),
        model: "claude-opus-5-5".to_owned(),
        effort: "high".to_owned(),
        endpoint_name: Some("claude-subscription".to_owned()),
        endpoints: vec![
            subscribed("claude-subscription"),
            subscribed("claude-work"),
            endpoint("claude-api", false),
        ],
        next: pending.map(|name| NextTurn {
            endpoint: Some(name.to_owned()),
            model: None,
            effort: None,
            cache_ttl: None,
        }),
        ..facts()
    }
}

#[test]
fn the_chip_leads_the_facts_and_drops_last_at_40_80_and_120() {
    let mut done = Status::new();
    done.submitted(d(0), FULL);
    done.apply(&AgentEvent::turn_started(1), d(5), FULL);
    done.apply(&context_changed(159_000, true), d(6), FULL);
    done.apply(&ended(completed(StopReason::EndTurn, true), 1), d(10), FULL);
    let working = long_turn(16, false);
    let mut out = Vec::new();
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let cx = MotionCtx {
            ascii: set == GlyphSet::Ascii,
            ..FULL
        };
        for pending in [None, Some("claude-work"), Some("claude-api")] {
            let f = two_subscriptions(pending);
            for w in [120, 100, 80, 40] {
                for (s, at) in [(&done, 5000), (&working, 65_000)] {
                    let row = line(&s.row(d(at), cx, &f, Layout::new(w, set)));
                    assert!(row.chars().count() < w, "{w}: {row}");
                    assert!(!row.contains('$'), "{w}: {row}");
                    if pending.is_none() && !s.is_running() {
                        assert!(row.contains(" subscription"), "{w}: {row}");
                    }
                    out.push(format!("{w}: {row}"));
                }
            }
        }
    }
    done.usage_candidates(vec![crate::plan_usage::UsageCandidate {
        line: StyledLine::new(vec![StyledSpan::new("!! week 96% used", Token::Error)]),
        measured: true,
        warns: true,
    }]);
    let row = line(&done.row(d(5000), FULL, &two_subscriptions(None), layout(80)));
    assert!(row.contains("!! week 96% used"), "{row}");
    assert!(row.contains("claude-subscription"), "{row}");
    out.push(format!("80 warning: {row}"));
    let row = line(&done.row(d(5000), FULL, &two_subscriptions(None), layout(70)));
    assert!(row.contains("!! week 96% used"), "{row}");
    assert!(!row.contains("opus-5-5"), "{row}");
    out.push(format!("70 warning: {row}"));
    done.usage_candidates(vec![]);
    let row = line(&done.row(d(5000), FULL, &two_subscriptions(None), layout(70)));
    assert!(row.contains("opus-5-5"), "{row}");
    out.push(format!("70 no warning: {row}"));
    insta::assert_snapshot!(out.join("\n"));
}

#[test]
fn a_pending_switch_shows_where_the_next_message_goes_with_its_billing() {
    let f = two_subscriptions(Some("claude-api"));
    let row = line(&Status::new().row(d(0), FULL, &f, layout(80)));
    assert!(
        row.ends_with(
            "\u{25cf} claude-subscription \u{b7} subscription   \u{2192} \u{25cf} claude-api \u{b7} API"
        ),
        "{row}"
    );
    assert!(!row.contains("opus-5-5"), "{row}");
}

#[test]
fn an_accounts_own_facts_beat_the_models_and_hide_its_price() {
    let rates = |input: f64| ModelRates {
        writes: None,
        input_per_mtok: input,
        output_per_mtok: 4.0,
        cache_read_per_mtok: 0.1,
        long_prompt: None,
    };
    let caps = ModelCaps {
        supported: true,
        efforts: Vec::new(),
        default_effort: None,
        per_message_effort: false,
        adaptive_thinking: false,
        max_output: Some(64_000),
    };
    let entry = |account: &str, model: &str, rates, max_output| AccountFacts {
        account: account.to_owned(),
        model: model.to_owned(),
        rates,
        max_output,
    };
    let mut facts = facts();
    facts.models = vec![ModelFacts {
        provider: Some("anthropic".to_owned()),
        shipped: true,
        id: "shipped".to_owned(),
        listed: None,
        rates: Some(rates(1.0)),
        caps: Some(caps),
        variant: None,
        image_tier: None,
    }];
    facts.on_accounts = vec![
        entry("gw", "shipped", Some(rates(2.0)), Some(32_000)),
        entry("own", "shipped", None, None),
        entry("gw", "my-model", Some(rates(1.2)), None),
    ];
    assert_eq!(facts.rates_on("gw", "shipped"), Some(&rates(2.0)));
    assert_eq!(facts.rates_on("other", "shipped"), Some(&rates(1.0)));
    assert_eq!(facts.rates_on("own", "shipped"), None);
    assert_eq!(facts.rates_on("gw", "my-model"), Some(&rates(1.2)));
    assert_eq!(facts.rates_on("other", "my-model"), None);
    assert_eq!(facts.max_output_on("gw", "shipped"), Some(32_000));
    assert_eq!(facts.max_output_on("own", "shipped"), Some(64_000));
    assert_eq!(facts.max_output_on("gw", "my-model"), None);
    assert!(facts.model_selectable("my-model", Some("gw")));
}

#[test]
fn kept_chats_keep_their_details_and_right_align_the_accent_state() {
    let mut kept = chat_row("a", "A running chat");
    kept.open_elsewhere = true;
    kept.hosted = true;
    let chats = [kept, chat_row("b", "A closed chat")];
    for columns in [40, 80, 120] {
        let view = ChatsView {
            chats: chats.iter().collect(),
            listed: &chats,
            focus: Some(0),
            query: "",
            loading: false,
            below: Vec::new(),
            place: ChatsPlace::Start,
        };
        let lines = chats_lines(&view, 30, layout(columns));
        let row = lines
            .iter()
            .find(|row| line(row).contains("kept open"))
            .unwrap();
        assert!(!line(row).contains(copy::OPEN_ELSEWHERE));
        assert!(row.spans.iter().any(|span| span.text.contains("kept open")
            && span.look.fg == Some(Ink::Token(Token::Accent))));
        assert!(
            row.spans
                .iter()
                .any(|span| span.text.contains("running chat")
                    && span.look.fg == Some(Ink::Token(Token::Text)))
        );
    }
}
