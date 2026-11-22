use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use wisp_protocol::log::{LOG_SCHEMA_VERSION, LogEvent, LogRecord};

use wisp_protocol::settings::{Layer, Saved, SettingName, When};

use super::*;
use crate::session::{JsonlStore, Line, parse_jsonl, rebuild_checked, reread};
use crate::test_guard::setting_text;
use crate::test_guard::shared::{OPUS, at, lines, started_in, usage};

const ID: &str = "019a0f3c-5e2a-7b31-9c4d-2f6e8a1b3c5d";

fn log(events: &[Value]) -> Vec<LogRecord> {
    parse_jsonl(lines(events)).unwrap()
}

fn started() -> Value {
    started_in(ID, "~/wisp", &[], None)
}

fn turn(n: u64) -> Value {
    json!({"type": "turn_started", "turn": n, "text": format!("message {n}")})
}

fn turn_on(n: u64, model: &str, effort: &str, endpoint: &str) -> Value {
    json!({
        "type": "turn_started",
        "turn": n,
        "text": "hello",
        "model": model,
        "effort": effort,
        "endpoint_name": endpoint,
    })
}

fn said(n: u64, role: &str) -> Value {
    json!({
        "type": "message_committed",
        "turn": n,
        "message": {"role": role, "content": [{"type": "text", "text": format!("{role} {n}")}]},
        "model": (role == "assistant").then_some(OPUS),
    })
}

fn step(n: u64, step: u32, prompt: u64, accepted: bool) -> Value {
    json!({
        "type": "step_ended",
        "turn": n,
        "step": step,
        "usage": usage(prompt, 40),
        "cost_usd": 0.01,
        "lower_bound": false,
        "prompt_tokens": prompt,
        "response_id": format!("msg_{n}_{step}"),
        "used": prompt,
        "accepted": accepted,
    })
}

fn ended(n: u64, session_usd: f64) -> Value {
    json!({
        "type": "turn_ended",
        "turn": n,
        "outcome": {
            "status": "completed",
            "stop_reason": "end_turn",
            "model": OPUS,
            "usage": usage(1, 1),
            "cost": {"turn_usd": 0.1, "session_usd": session_usd},
            "committed": true,
        },
        "stats": {"started_at": at(0), "duration_ms": 1, "retries": 0},
    })
}

fn full_turn(n: u64, session_usd: f64) -> Vec<Value> {
    vec![
        turn(n),
        said(n, "user"),
        said(n, "assistant"),
        step(n, 1, 1_000 * n, true),
        ended(n, session_usd),
    ]
}

fn compacted(cost_usd: Option<f64>) -> Value {
    json!({
        "type": "context_compacted",
        "epoch": 1,
        "trigger": "manual",
        "engine": "wisp",
        "before": 5_000,
        "after": 900,
        "from": 2,
        "head": {"role": "user", "content": [{"type": "text", "text": "summary"}]},
        "summary": "summary",
        "usage": usage(5_000, 300),
        "cost_usd": cost_usd,
        "duration_ms": 10,
    })
}

#[test]
fn the_fold_reads_the_current_pair_the_cost_and_the_last_reply() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([
        turn_on(2, "claude-sonnet-5", "high", "env"),
        said(2, "user"),
        said(2, "assistant"),
        step(2, 1, 1_500, true),
        ended(2, 0.5),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.current.model, "claude-sonnet-5");
    assert_eq!(logged.current.effort, Effort::High);
    assert_eq!(logged.started.model, OPUS);
    assert_eq!(logged.turns, 2);
    assert!((logged.session_usd - 0.5).abs() < 1e-9);
    assert!(!logged.lower_bound);
    let last = logged.last_step.clone().unwrap();
    assert_eq!((last.turn, last.step, last.reply), (2, 1, Some(8)));
    assert_eq!(last.prompt_tokens, 1_500);
    assert_eq!(last.served.as_deref(), Some(OPUS));
    assert_eq!(last.model, "claude-sonnet-5");
    assert_eq!(last.endpoint_name.as_deref(), Some("env"));
    assert_eq!(last.started_at, at(6));
    assert_eq!(logged.replies, vec![(3, 40), (8, 40)]);
    assert_eq!(logged.completed_on.get("env"), Some(&2));
    assert_eq!(logged.repaired_turn(), None);
    assert!(logged.open_inputs.is_empty());
    assert!(!logged.ended);
    assert_eq!(logged.short_id(), "019a0f3c-5e2a");
}

#[test]
fn the_last_reply_takes_its_send_time_and_ttl_from_its_step() {
    let mut events = vec![started()];
    events.extend([turn(1), said(1, "user"), said(1, "assistant")]);
    let mut logged_step = step(1, 1, 1_000, true);
    logged_step["view_len"] = json!(1);
    logged_step["started_at"] = json!("2026-10-25T09:59:58.125Z");
    logged_step["ttl"] = json!("five_minutes");
    events.extend([logged_step, ended(1, 0.25)]);
    let last = LoggedSession::read(&log(&events))
        .unwrap()
        .last_step
        .unwrap();
    assert_eq!(last.started_at, "2026-10-25T09:59:58.125Z");
    assert_eq!(last.ttl, CacheTtl::FiveMinutes);

    let mut older = vec![started()];
    older.extend([
        turn(1),
        said(1, "user"),
        said(1, "assistant"),
        step(1, 1, 1_000, true),
        ended(1, 0.25),
    ]);
    let last = LoggedSession::read(&log(&older))
        .unwrap()
        .last_step
        .unwrap();
    assert_eq!(last.started_at, at(1));
    assert_eq!(last.ttl, CacheTtl::OneHour);
}

#[test]
fn a_step_that_committed_no_reply_keeps_the_last_reply() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([turn(2), step(2, 1, 9_000, true), ended(2, 0.3)]);
    let last = LoggedSession::read(&log(&events))
        .unwrap()
        .last_step
        .unwrap();
    assert_eq!((last.turn, last.prompt_tokens), (1, 1_000));
    let mut unpriced = step(3, 1, 0, true);
    unpriced["prompt_tokens"] = Value::Null;
    events.extend([turn(3), said(3, "user"), said(3, "assistant"), unpriced]);
    assert_eq!(LoggedSession::read(&log(&events)).unwrap().last_step, None);
}

#[test]
fn compaction_costs_add_to_the_session_and_drop_the_last_reply() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(compacted(Some(0.125)));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!((logged.session_usd - 0.375).abs() < 1e-9);
    assert_eq!(logged.compaction_usd, Some(0.125));
    assert_eq!(logged.compactions, 1);
    assert_eq!(logged.last_compaction, Some(6));
    assert!(logged.cold_since_step);
    assert!(!logged.lower_bound);
    let mut bounded = events.clone();
    bounded.last_mut().unwrap()["lower_bound"] = json!(true);
    let bounded = LoggedSession::read(&log(&bounded)).unwrap();
    assert!(bounded.lower_bound && bounded.accounts["env"].lower_bound);

    events.push(compacted(None));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!(logged.lower_bound, "usage without a price is a lower bound");
    assert_eq!(logged.compactions, 2);

    events.extend(full_turn(2, 1.0));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!(!logged.cold_since_step);
    assert!((logged.session_usd - 1.0).abs() < 1e-9);
}

fn compaction_retry(cost_usd: f64, lower_bound: bool) -> Value {
    json!({
        "type": "retrying",
        "attempt": 1,
        "delay_ms": 10,
        "reason": "overloaded",
        "usage": usage(5_000, 0),
        "cost_usd": cost_usd,
        "lower_bound": lower_bound,
    })
}

#[test]
fn a_manual_compactions_retries_and_billed_failures_count_in_the_session() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(compaction_retry(0.5, false));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!(
        (logged.session_usd - 0.75).abs() < 1e-9,
        "a compaction cut short by a crash still cost its retries"
    );
    assert!(!logged.lower_bound);
    let mut resumed = events.clone();
    resumed.push(json!({
        "type": "session_resumed",
        "wisp_version": "0.1.0",
        "endpoint": "https://api.anthropic.com",
        "endpoint_name": "env",
    }));
    resumed.push(compacted(Some(0.125)));
    let logged = LoggedSession::read(&log(&resumed)).unwrap();
    assert!(
        (logged.session_usd - 0.875).abs() < 1e-9,
        "a later run's compaction doesn't absorb the one the crash cut short"
    );

    events.push(compacted(Some(0.625)));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!(
        (logged.session_usd - 0.875).abs() < 1e-9,
        "the compaction's cost already holds its retries"
    );

    events.push(compaction_retry(0.0, true));
    events.push(json!({
        "type": "compaction_failed",
        "trigger": "manual",
        "engine": "native",
        "kind": {"error": "server"},
        "message": "the stream broke",
    }));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!((logged.session_usd - 0.875).abs() < 1e-9);
    assert!(
        logged.lower_bound,
        "a possibly billed retry is a lower bound"
    );

    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(json!({
        "type": "compaction_failed",
        "trigger": "manual",
        "engine": "native",
        "kind": {"error": "server"},
        "message": "the stream broke",
        "lower_bound": true,
    }));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!((logged.session_usd - 0.25).abs() < 1e-9);
    assert!(
        logged.lower_bound,
        "a failure that may have been billed without usage is a lower bound"
    );

    events.extend(full_turn(2, 1.0));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!((logged.session_usd - 1.0).abs() < 1e-9);
    assert!(
        !logged.lower_bound,
        "the next turn's total replaces the fold's"
    );
}

fn entry(name: &str, value: SettingValue, saved: bool) -> Value {
    let change = LoggedChange {
        target: SettingTarget::Name(SettingName::of(
            wisp_protocol::settings::find(name).unwrap(),
        ))
        .into(),
        value: Some(value),
        layer: Layer::Session,
        when: When::NextTurn,
        saved: saved.then(|| Saved {
            path_display: "~/.config/wisp/config.toml".into(),
            line: Some(3),
            notes: Vec::new(),
        }),
        unset: false,
        by: None,
    };
    serde_json::to_value(change).unwrap()
}

fn change(model: &str, effort: &str, endpoint: &str, entries: Vec<Value>) -> Value {
    json!({
        "type": "settings_changed",
        "model": model,
        "effort": effort,
        "cache": {"type": "kept"},
        "endpoint_name": endpoint,
        "changes": entries,
    })
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn pending(events: &[Value]) -> Vec<String> {
    let logged = LoggedSession::read(&log(events)).unwrap();
    let mut out = Vec::new();
    if let Some(pair) = &logged.pending {
        out.extend(pair.endpoint.iter().map(|e| format!("endpoint {e}")));
        if pair.model != logged.current.model {
            out.push(format!("model {}", pair.model));
        }
        if pair.effort != logged.current.effort {
            out.push(format!("effort {}", pair.effort.as_str()));
        }
    }
    let later = &logged.later;
    out.extend(later.cache_ttl.map(|t| format!("cache_ttl {}", t.as_str())));
    out.extend(
        later
            .debug_cache
            .map(|on| format!("debug_cache {}", on_off(on))),
    );
    out.extend(
        later
            .refusal_fallback
            .map(|on| format!("refusal_fallback {}", on_off(on))),
    );
    out.extend(
        later
            .auto_compact
            .map(|on| format!("auto_compact {}", on_off(on))),
    );
    out
}

#[test]
fn a_change_after_the_last_ordinary_turn_is_pending_until_undone() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(change(
        OPUS,
        "medium",
        "env",
        vec![entry("effort", setting_text("medium"), false)],
    ));
    assert!(pending(&events).is_empty(), "a change to the current value");

    events.push(change(
        OPUS,
        "low",
        "env",
        vec![entry("effort", setting_text("low"), false)],
    ));
    assert_eq!(pending(&events), ["effort low"]);
    events.push(change(
        OPUS,
        "medium",
        "env",
        vec![entry("effort", setting_text("medium"), false)],
    ));
    assert!(pending(&events).is_empty(), "undone");
    events.push(change(
        OPUS,
        "medium",
        "other",
        vec![entry("endpoint", setting_text("other"), false)],
    ));
    assert_eq!(pending(&events), ["endpoint other"]);
    events.push(change(
        "claude-sonnet-5",
        "medium",
        "other",
        vec![
            entry("model", setting_text("claude-sonnet-5"), false),
            entry("context.auto_compact", SettingValue::Bool(false), false),
        ],
    ));
    assert_eq!(
        pending(&events),
        [
            "endpoint other",
            "model claude-sonnet-5",
            "auto_compact off"
        ]
    );

    events.push(json!({
        "type": "turn_started",
        "turn": 2,
        "model": OPUS,
        "effort": "medium",
        "endpoint_name": "env",
        "retry_of": 1,
    }));
    assert_eq!(
        pending(&events),
        [
            "endpoint other",
            "model claude-sonnet-5",
            "auto_compact off"
        ],
        "a Retry leaves the change pending"
    );
    events.push(turn_on(3, "claude-sonnet-5", "medium", "env"));
    assert!(pending(&events).is_empty());
    let applied = LoggedSession::read(&log(&events)).unwrap().current;
    assert_eq!(applied.auto_compact, Some(false));

    let options = |cache_ttl: &str, debug_cache: bool| {
        change(
            "claude-sonnet-5",
            "medium",
            "env",
            vec![
                entry("cache_ttl", setting_text(cache_ttl), false),
                entry("debug_cache", SettingValue::Bool(debug_cache), false),
                entry("display.theme", setting_text("light"), false),
            ],
        )
    };
    events.push(options("5m", true));
    assert_eq!(pending(&events), ["cache_ttl 5m", "debug_cache on"]);
    events.push(options("1h", false));
    assert!(pending(&events).is_empty(), "request options undone");
    events.push(change(
        "claude-sonnet-5",
        "medium",
        "env",
        vec![entry("refusal_fallback", SettingValue::Bool(false), false)],
    ));
    assert_eq!(pending(&events), ["refusal_fallback off"]);

    events.push(turn_on(4, "claude-sonnet-5", "medium", "env"));
    events.push(change(
        "claude-sonnet-5",
        "medium",
        "env",
        vec![entry("effort", setting_text("medium"), true)],
    ));
    assert!(
        pending(&events).is_empty(),
        "a saved change to the value the last turn used"
    );
    events.push(change(
        "claude-sonnet-5",
        "low",
        "env",
        vec![entry("effort", setting_text("low"), true)],
    ));
    assert_eq!(
        pending(&events),
        ["effort low"],
        "a saved change after the last turn is pending too"
    );
}

#[test]
fn the_binding_controls_flag_is_the_last_turns() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.current.binding_controls, None, "an older log");

    let mut on = turn_on(2, OPUS, "medium", "env");
    on["binding_controls"] = json!(true);
    events.push(on);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.current.binding_controls, Some(true));

    let mut off = turn_on(3, OPUS, "medium", "env");
    off["binding_controls"] = json!(false);
    events.push(off);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.current.binding_controls, Some(false));

    let resumed_on = |name: &str| {
        json!({
            "type": "session_resumed",
            "wisp_version": "0.1.0",
            "endpoint": "https://api.anthropic.com",
            "endpoint_name": name,
        })
    };
    events.push(resumed_on("env"));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(
        logged.current.binding_controls,
        Some(false),
        "a resume on the same endpoint"
    );
    events.push(resumed_on("other"));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(
        logged.current.binding_controls,
        Some(true),
        "a resume on another endpoint binds them, so a second resume keeps them"
    );
}

fn tail_closed(through: usize) -> Value {
    json!({
        "type": "tail_closed",
        "through": through,
        "calls": ["toolu_1"],
        "reason": "refused",
    })
}

fn thinking_cut(message: usize, through: usize) -> Value {
    json!({
        "type": "thinking_cut",
        "from": {"message": message, "block": 0},
        "through": through,
    })
}

#[test]
fn a_closed_tail_is_kept_unless_its_own_cut_follows_it() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(tail_closed(1));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(
        logged.closed_tails,
        vec![LoggedTail {
            endpoint: Some("env".to_owned()),
            model: OPUS.to_owned(),
            through: 1,
            calls: vec!["toolu_1".to_owned()],
            last_turn: true,
            at: events.len() - 1,
        }]
    );

    events.push(turn(2));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.closed_tails.len(), 1);
    assert!(!logged.closed_tails[0].last_turn, "a later turn started");

    events.push(tail_closed(3));
    events.push(thinking_cut(1, 3));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.closed_tails.len(), 1, "the cut undoes its own tail");
}

#[test]
fn a_rendering_form_followed_by_its_cut_keeps_no_break() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(json!({"type": "rendering_form", "form": "without_thinking"}));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.current.binding_controls, Some(true));
    assert_eq!(
        logged.kept_breaks,
        vec![LoggedBreak {
            endpoint: Some("env".to_owned()),
            model: OPUS.to_owned(),
            last_turn: true,
            at: events.len() - 1,
        }]
    );

    let mut later = events.clone();
    later.push(turn(2));
    let logged = LoggedSession::read(&log(&later)).unwrap();
    assert_eq!(logged.kept_breaks.len(), 1);
    assert!(!logged.kept_breaks[0].last_turn, "a later turn started");

    events.push(thinking_cut(1, 2));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.current.binding_controls, None);
    assert!(logged.kept_breaks.is_empty());
}

#[test]
fn a_cut_of_the_whole_history_drops_every_break_and_returns_to_the_turns_binding() {
    let mut events = vec![started()];
    let mut off = turn_on(1, OPUS, "medium", "env");
    off["binding_controls"] = json!(false);
    events.push(off);
    events.push(json!({"type": "rendering_form", "form": "without_thinking"}));
    events.push(tail_closed(1));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.current.binding_controls, Some(true));
    assert_eq!(logged.closed_tails.len(), 1);

    events.push(said(1, "user"));
    events.push(thinking_cut(0, 1));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.current.binding_controls, Some(false));
    assert!(logged.kept_breaks.is_empty());
    assert!(logged.closed_tails.is_empty());
}

fn retrying(n: u64, step: u32, cost_usd: f64) -> Value {
    json!({
        "type": "retrying",
        "turn": n,
        "attempt": 1,
        "delay_ms": 500,
        "reason": "overloaded",
        "step": step,
        "usage": usage(500, 7),
        "cost_usd": cost_usd,
        "lower_bound": true,
    })
}

fn failed(n: u64, session_usd: f64) -> Value {
    json!({
        "type": "turn_ended",
        "turn": n,
        "outcome": {
            "status": "error",
            "kind": "server",
            "message": "boom",
            "usage": usage(1, 1),
            "cost": {"turn_usd": 0.1, "session_usd": session_usd},
            "committed_steps": 0,
        },
        "stats": {"started_at": at(0), "duration_ms": 1, "retries": 0},
    })
}

fn accepted(id: &str, mode: &str, text: &str) -> Value {
    json!({"type": "input_accepted", "turn": 2, "id": id, "mode": mode, "text": text})
}

#[test]
fn an_open_turn_sums_its_steps_and_retries_and_keeps_its_open_inputs() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([
        turn(2),
        said(2, "user"),
        said(2, "assistant"),
        step(2, 1, 2_000, true),
        accepted("tui-5", "steer", "also this"),
        accepted("tui-6", "queue", "and later"),
        json!({"type": "input_delivered", "turn": 2, "step": 2, "ids": ["tui-5"]}),
        retrying(2, 2, 0.02),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.open_turn, Some(2));
    let last = logged.repaired_turn().unwrap();
    assert_eq!(last.turn, 2);
    assert!(last.replied);
    assert_eq!((last.committed_steps, last.step, last.retries), (1, 2, 1));
    assert!((last.usd.unwrap() - 0.03).abs() < 1e-9);
    assert!(last.lower_bound);
    assert_eq!(last.usage.unwrap().input_tokens, 2_500);
    assert_eq!(last.started_at, at(6));
    assert_eq!(last.last_at, at(13));
    assert_eq!(last.opening[0].text, "message 2");
    assert_eq!(last.resend(), vec![InputId("tui-5".into())]);
    assert!((logged.session_usd - 0.28).abs() < 1e-9);
    assert!(logged.lower_bound);
    assert_eq!(
        logged.open_inputs,
        vec![InputId("tui-5".into()), InputId("tui-6".into())]
    );
    assert_eq!(
        logged.accepted[&InputId("tui-6".into())].mode,
        DeliveryMode::Queue
    );

    events.extend([
        accepted("tui-7", "queue", "removed"),
        json!({"type": "input_withdrawn", "ids": ["tui-7"], "by": "user"}),
    ]);
    let withdrawal =
        json!({"type": "input_withdrawn", "ids": ["tui-5", "tui-6"], "by": "session_ended"});
    let resumed = json!({
        "type": "session_resumed",
        "wisp_version": "0.1.0",
        "endpoint": "https://gateway.example",
        "endpoint_name": "work",
    });
    let mut crashed = events.clone();
    crashed.extend([resumed.clone(), withdrawal.clone()]);
    assert!(
        LoggedSession::read(&log(&crashed))
            .unwrap()
            .ended_inputs
            .is_empty()
    );
    events.push(withdrawal);
    events.push(failed(2, 0.4));
    events.push(json!({"type": "session_ended", "turns": 2}));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.repaired_turn(), None);
    assert!(logged.open_inputs.is_empty());
    assert!((logged.session_usd - 0.4).abs() < 1e-9);
    assert!(logged.ended);
    assert_eq!(
        logged.ended_inputs,
        [InputId("tui-5".into()), InputId("tui-6".into())]
    );
    let mut carried = resumed.clone();
    carried["returned"] = json!(["tui-5", "tui-6"]);
    events.push(carried.clone());
    assert_eq!(
        LoggedSession::read(&log(&events)).unwrap().ended_inputs,
        [InputId("tui-5".into()), InputId("tui-6".into())]
    );
    let mut taken = events.clone();
    taken.push(json!({"type": "returned_taken", "ids": ["tui-5", "tui-6"]}));
    assert!(
        LoggedSession::read(&log(&taken))
            .unwrap()
            .ended_inputs
            .is_empty()
    );
    crashed.truncate(crashed.len() - 2);
    crashed.extend([
        carried,
        json!({"type": "input_withdrawn", "ids": ["tui-5", "tui-6"], "by": "session_ended"}),
    ]);
    let logged = LoggedSession::read(&log(&crashed)).unwrap();
    assert_eq!(
        logged.ended_inputs,
        [InputId("tui-5".into()), InputId("tui-6".into())]
    );
    assert!(logged.open_inputs.is_empty());
    events.push(resumed.clone());
    assert!(
        LoggedSession::read(&log(&events))
            .unwrap()
            .ended_inputs
            .is_empty()
    );
    let mut opening = turn(3);
    let image =
        json!({"sha256": "image", "media_type": "image/png", "bytes": 1, "width": 1, "height": 1});
    opening["inputs"] = json!([{"id": "tui-8", "text": "message 3", "images": [image.clone()]}]);
    let withdrawal = json!({"type": "input_withdrawn", "ids": ["tui-8"], "by": "session_ended"});
    let mut direct = events.clone();
    direct.extend([opening.clone(), withdrawal.clone()]);
    events.extend([
        accepted("tui-8", "queue", "message 3"),
        opening,
        withdrawal.clone(),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.ended_inputs, [InputId("tui-8".into())]);
    events.extend([resumed.clone(), withdrawal]);
    assert!(
        LoggedSession::read(&log(&events))
            .unwrap()
            .ended_inputs
            .is_empty()
    );
    let mut carried = resumed;
    carried["returned"] = json!(["tui-8"]);
    direct.extend([carried, turn(4)]);
    let logged = LoggedSession::read(&log(&direct)).unwrap();
    assert_eq!(logged.ended_inputs, [InputId("tui-8".into())]);
    assert_eq!(
        logged
            .accepted
            .get(&InputId("tui-8".into()))
            .map(|accepted| accepted.input.text.as_str()),
        Some("message 3")
    );
    assert_eq!(
        serde_json::to_value(&logged.accepted[&InputId("tui-8".into())].input.images).unwrap(),
        json!([image])
    );
}

#[test]
fn a_failed_turn_keeps_what_its_retry_resends_until_something_lapses_it() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([
        json!({"type": "turn_started", "turn": 2, "text": "go", "input_id": "tui-2"}),
        accepted("tui-3", "steer", "steer one"),
        json!({"type": "input_delivered", "turn": 2, "step": 1, "ids": ["tui-3"]}),
        accepted("tui-4", "steer", "steer two"),
        json!({"type": "input_withdrawn", "turn": 2, "ids": ["tui-2", "tui-3", "tui-4"], "by": "turn_failed"}),
        failed(2, 0.3),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    let last = logged.last_turn.clone().unwrap();
    assert!(!last.replied);
    assert_eq!(last.opening[0].id, InputId("tui-2".into()));
    assert_eq!(last.resend(), vec![InputId("tui-3".into())]);
    assert_eq!(last.lapsed, None);
    assert!(matches!(last.outcome, Some(TurnOutcome::Error { .. })));

    events.push(compacted(Some(0.1)));
    events.push(accepted("tui-5", "steer", "after"));
    let last = LoggedSession::read(&log(&events))
        .unwrap()
        .last_turn
        .unwrap();
    assert_eq!(last.lapsed, Some(Lapse::Compacted));

    let mut unheld = events[..events.len() - 2].to_vec();
    unheld.push(json!({"type": "input_unheld", "turn": null, "ids": ["tui-4"]}));
    let last = LoggedSession::read(&log(&unheld))
        .unwrap()
        .last_turn
        .unwrap();
    assert_eq!(
        last.lapsed,
        Some(Lapse::SentAfter),
        "an unheld message lapses it"
    );
}

#[test]
fn a_retry_resends_each_delivered_steer_once_and_none_already_committed() {
    let mut events = vec![started()];
    events.extend([
        json!({"type": "turn_started", "turn": 1, "text": "go", "input_id": "tui-1"}),
        accepted("tui-2", "steer", "steer one"),
        json!({"type": "input_delivered", "turn": 1, "step": 2, "ids": ["tui-2"]}),
        json!({"type": "input_committed", "turn": 1, "step": 2, "inputs": [{"id": "tui-2", "text": "steer one"}]}),
        accepted("tui-3", "steer", "steer two"),
        json!({"type": "input_delivered", "turn": 1, "step": 3, "ids": ["tui-3"]}),
        json!({"type": "input_delivered", "turn": 1, "step": 3, "ids": ["tui-3"]}),
        failed(1, 0.3),
    ]);
    let last = LoggedSession::read(&log(&events))
        .unwrap()
        .last_turn
        .unwrap();
    assert_eq!(last.resend(), vec![InputId("tui-3".into())]);
}

#[test]
fn the_logged_tools_are_the_last_change_or_resume() {
    let events = vec![started_in(ID, "~/wisp", &["bash"], Some("aaa"))];
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!(!logged.tools.from_change);
    assert_eq!(logged.tools.sha256.as_deref(), Some("aaa"));

    let mut events = events;
    events.push(json!({
        "type": "settings_changed",
        "model": OPUS,
        "effort": "medium",
        "cache": {"type": "kept"},
        "tools": ["bash", "web_search"],
        "tools_sha256": "ddd",
    }));
    let changed = log(&events);
    let logged = LoggedSession::read(&changed).unwrap();
    assert!(logged.tools.from_change);
    assert_eq!(logged.tools.names, ["bash", "web_search"]);
    assert_eq!(logged.tools.sha256.as_deref(), Some("ddd"));

    events.push(json!({
        "type": "session_resumed",
        "wisp_version": "0.1.0",
        "endpoint": "https://gateway.example",
        "endpoint_name": "other",
        "tools": ["bash"],
        "tools_sha256": "aaa",
    }));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!(!logged.tools.from_change);
    assert_eq!(logged.tools.sha256.as_deref(), Some("aaa"));
    assert_eq!(logged.endpoint(), Some("other"));
    assert!(logged.switched);
    assert_eq!(
        logged.origins.get("other").map(String::as_str),
        Some("https://gateway.example")
    );
}

#[test]
fn feature_states_rendering_and_tool_facts_are_read_in_order() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([
        json!({"type": "per_message_effort", "model": OPUS, "state": "proven", "endpoint_name": "env"}),
        json!({"type": "binding_controls", "model": OPUS, "state": "fallen_back"}),
        json!({"type": "native_web_search", "endpoint_name": "env", "state": "fallen_back"}),
        json!({"type": "search_history_rendered", "endpoint_name": "env"}),
        json!({"type": "system_marker_dropped", "endpoint_name": "env"}),
        json!({"type": "rendering_form", "form": "flattened"}),
        json!({
            "type": "tool_call_requested",
            "turn": 1, "step": 1, "block": 0, "call_id": "toolu_1", "tool": "bash",
            "input": {"command": "cd /tmp"},
            "display": {"action": {"type": "run"}, "target": "cd /tmp", "output": {"type": "text"}},
        }),
        json!({"type": "tool_call_started", "turn": 1, "call_id": "toolu_1", "cwd": "~/wisp"}),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(
        logged.features,
        vec![
            LoggedFeature {
                endpoint: Some("env".into()),
                model: OPUS.into(),
                feature: Feature::PerMessageEffort,
                state: FeatureState::Proven,
            },
            LoggedFeature {
                endpoint: None,
                model: OPUS.into(),
                feature: Feature::BindingControls,
                state: FeatureState::FallenBack,
            },
        ]
    );
    assert_eq!(logged.rendered.get("env"), Some(&9));
    assert!(logged.marker_dropped.contains("env"));
    assert_eq!(logged.form, Some(RenderingForm::Flattened));
    assert_eq!(logged.started_tools, ["bash"]);
}

#[test]
fn a_log_that_never_ended_may_still_be_open_for_ten_minutes() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    let last: jiff::Timestamp = logged.last_at.parse().unwrap();
    let after = |minutes: i64| {
        last.checked_add(jiff::SignedDuration::from_mins(minutes))
            .unwrap()
    };
    assert_eq!(
        logged.maybe_open(after(3)).as_deref(),
        Some("This session's last record is 3 min old and it never ended; it may still be open.")
    );
    assert_eq!(logged.maybe_open(after(10)), None);

    events.push(json!({"type": "session_ended", "turns": 1}));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(logged.maybe_open(after(1)), None);
}

#[test]
fn a_log_must_start_with_session_started() {
    assert_eq!(
        LoggedSession::read::<LogRecord>(&[]).unwrap_err(),
        "the log is empty".to_owned()
    );
    let records = log(&[turn(1)]);
    assert_eq!(
        LoggedSession::read(&records).unwrap_err(),
        "the log doesn't start with session_started".to_owned()
    );
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wisp-resume-{name}-{}-{}",
        std::process::id(),
        crate::session::new_session_id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_session(store: &JsonlStore, id: &str, events: &[Value], age_minutes: u64) {
    let path = store.path(id);
    fs::write(&path, lines(events)).unwrap();
    set_modified(
        &path,
        SystemTime::now() - Duration::from_secs(age_minutes * 60),
    );
}

pub(crate) fn set_modified(path: &Path, modified: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

pub(crate) fn session_here(
    store: &JsonlStore,
    id: &str,
    cwd: &str,
    tools: Option<&str>,
    turns: u64,
    age: u64,
) {
    let names: &[&str] = if tools.is_some() { &["bash"] } else { &[] };
    let mut events = vec![started_in(id, cwd, names, tools)];
    for n in 1..=turns {
        events.extend(full_turn(n, 0.1));
    }
    write_session(store, id, &events, age);
}

fn now() -> jiff::Timestamp {
    "2026-10-25T12:00:00Z".parse().unwrap()
}

const A: &str = "0001aaaa-0000-7000-8000-000000000000";
const B: &str = "0002bbbb-0000-7000-8000-000000000000";

#[test]
fn an_id_prefix_names_one_session() {
    let dir = temp_dir("by-id");
    let store = JsonlStore::open(&dir).unwrap();
    for id in [A, B, "0002bbbc-0000-7000-8000-000000000000"] {
        session_here(&store, id, "~/wisp", None, 1, 0);
    }
    let find_id = |prefix: &str| find(&store, Wanted::Id(prefix));
    assert_eq!(find_id("0001").unwrap().id, A);
    assert_eq!(find_id("0002BBBB").unwrap().id, B);
    assert_eq!(
        find_id("0002bbb").unwrap_err(),
        "0002bbb matches 2 sessions (0002bbbb-0000, 0002bbbc-0000); give more of the ID"
    );
    assert_eq!(
        find_id("0009").unwrap_err(),
        "no session's ID starts with 0009"
    );
    assert_eq!(find_id("../x").unwrap_err(), "../x is not a session ID");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ages_read_in_minutes_hours_and_days() {
    let at = |minutes: i64| {
        now()
            .checked_sub(jiff::SignedDuration::from_mins(minutes))
            .unwrap()
            .to_string()
    };
    assert_eq!(ago(&at(0), now()), "just now");
    assert_eq!(ago(&at(5), now()), "5 min ago");
    assert_eq!(ago(&at(125), now()), "2 h ago");
    assert_eq!(ago(&at(3 * 1440), now()), "3 days ago");
    assert_eq!(ago("yesterday", now()), "at an unknown time");
}

#[test]
fn a_failed_switch_compaction_counts_once_through_its_turn() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(json!({
        "type": "compaction_failed",
        "turn": 2,
        "trigger": "switch",
        "engine": "native",
        "kind": {"error": "invalid_request"},
        "message": "bad request",
        "usage": usage(2_000, 0),
        "cost_usd": 0.05,
        "lower_bound": true,
        "switch": {"estimate_micros": 50_000, "head_tokens": 300, "cause": "convert"},
    }));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!((logged.session_usd - 0.3).abs() < 1e-9);
    assert!(logged.lower_bound);

    let mut finished = ended(2, 0.4);
    finished["outcome"]["cost"]["session_lower_bound"] = json!(true);
    events.extend([
        turn(2),
        said(2, "user"),
        said(2, "assistant"),
        step(2, 1, 2_000, true),
        finished,
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!((logged.session_usd - 0.4).abs() < 1e-9);
    assert!(logged.lower_bound);
    assert_eq!(logged.compaction_usd, Some(0.05));
}

fn compacted_in(n: u64, trigger: &str, cost_usd: f64) -> Value {
    let mut record = compacted(Some(cost_usd));
    record["turn"] = json!(n);
    record["trigger"] = json!(trigger);
    record
}

#[test]
fn a_compactions_retry_in_an_open_turn_counts_once() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([
        turn(2),
        said(2, "user"),
        said(2, "assistant"),
        step(2, 1, 2_000, true),
        retrying(2, 0, 0.125),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!(
        (logged.session_usd - 0.385).abs() < 1e-9,
        "a compaction a crash cut short still cost its retries"
    );
    let last = logged.repaired_turn().unwrap();
    assert!((last.usd.unwrap() - 0.01).abs() < 1e-9);
    assert_eq!(last.retries, 0, "a compaction's retry isn't the turn's");

    events.push(compacted_in(2, "convert", 0.5));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!(
        (logged.session_usd - 0.76).abs() < 1e-9,
        "the compaction's cost already holds its retry"
    );
    assert!(logged.lower_bound);
    let last = logged.repaired_turn().unwrap();
    assert!((last.usd.unwrap() - 0.51).abs() < 1e-9);
    assert_eq!(last.usage.unwrap().input_tokens, 7_000);
    assert_eq!((last.retries, last.step), (0, 1));
    assert!(last.lower_bound, "the retry's lower bound reaches the turn");
}

#[test]
fn a_compaction_before_an_open_turn_counts_once_in_it() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([
        retrying(2, 0, 0.125),
        compacted_in(2, "switch", 0.5),
        turn(2),
        said(2, "user"),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!((logged.session_usd - 0.75).abs() < 1e-9);
    let last = logged.repaired_turn().unwrap();
    assert_eq!(last.turn, 2);
    assert!((last.usd.unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(last.usage.unwrap().input_tokens, 5_000);
    assert_eq!(last.retries, 0);
    assert!(last.lower_bound);
}

#[test]
fn the_fold_keeps_the_last_session_value_per_target_and_the_logged_instructions() {
    let mut events = vec![started()];
    events[0]["instructions"] = json!("rules");
    events[0]["instructions_sha256"] = json!("abc");
    events[0]["instructions_source"] = json!("default");
    events.push(json!({"type": "instructions_loaded", "turn": null, "chosen": null}));
    events.extend(full_turn(1, 0.25));
    events.push(change(
        OPUS,
        "medium",
        "env",
        vec![
            entry("time", setting_text("utc"), false),
            entry("effort", setting_text("medium"), false),
        ],
    ));
    events.push(change(
        OPUS,
        "medium",
        "env",
        vec![entry("time", setting_text("off"), false)],
    ));
    let mut model_point = entry("compact_at", SettingValue::Tokens(400_000), false);
    model_point["target"] = json!({"path": format!("models.\"{OPUS}\".compact_at")});
    events.push(change(
        OPUS,
        "medium",
        "env",
        vec![
            entry("compact_at", SettingValue::Tokens(500_000), true),
            model_point,
        ],
    ));
    let older = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(
        older
            .session_values
            .iter()
            .filter(|v| v.target.spec().name == "compact_at")
            .count(),
        2
    );
    let mut unset = entry("cache_ttl", setting_text("1h"), false);
    unset["unset"] = json!(true);
    let mut global = entry("compact_at", setting_text("80%"), true);
    global["layer"] = json!("config");
    events.push(change(OPUS, "medium", "env", vec![unset, global]));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    let values: Vec<(String, Option<SettingValue>)> = logged
        .session_values
        .iter()
        .map(|v| (v.target.to_string(), v.value.clone()))
        .collect();
    assert_eq!(
        values,
        [
            ("effort".to_owned(), Some(setting_text("medium"))),
            ("time".to_owned(), Some(setting_text("off"))),
            ("cache_ttl".to_owned(), None),
        ]
    );
    assert_eq!(logged.started.instructions.as_deref(), Some("rules"));
    assert_eq!(
        logged.started.instructions_source,
        Some(InstructionsSource::Default)
    );
    assert_eq!(
        logged.instructions_loaded,
        Some(InstructionsSummary::default())
    );
}

fn thought(n: u64) -> Value {
    json!({
        "type": "message_committed",
        "turn": n,
        "message": {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "hm", "signature": "sig"},
            {"type": "text", "text": "done"},
        ]},
        "model": OPUS,
    })
}

#[test]
fn the_newest_thinking_keeps_the_tools_it_was_built_with() {
    let tools_of = |events: &[Value]| LoggedSession::read(&log(events)).unwrap().thinking_tools;
    let mut events = vec![started_in(ID, "~/wisp", &["bash"], Some("start"))];
    events.extend(full_turn(1, 0.25));
    assert_eq!(tools_of(&events), None, "no thinking yet");
    events.extend([
        turn(2),
        said(2, "user"),
        thought(2),
        step(2, 1, 2_000, true),
        ended(2, 0.5),
    ]);
    let start = Some(ToolsHash::Started(Some("start".into())));
    assert_eq!(tools_of(&events), start);
    events.push(json!({
        "type": "settings_changed",
        "model": OPUS,
        "effort": "medium",
        "cache": {"type": "kept"},
        "endpoint_name": "env",
        "tools": ["bash", "web_search"],
        "tools_sha256": "searching",
    }));
    assert_eq!(tools_of(&events), start, "a switch after the thinking");
    events.extend([turn(3), said(3, "user"), thought(3)]);
    assert_eq!(
        tools_of(&events),
        Some(ToolsHash::Digest("searching".into()))
    );
    events.push(json!({
        "type": "thinking_cut",
        "from": {"message": 0, "block": 0},
        "through": 5,
        "tools_sha256": "plain",
    }));
    assert_eq!(
        tools_of(&events),
        Some(ToolsHash::Digest("plain".into())),
        "a later tools-change cut already left it out"
    );
}

fn lowered(endpoint: &str, model: &str, tokens: u64) -> Value {
    json!({"type": "window_lowered", "endpoint": endpoint, "model": model, "tokens": tokens})
}

#[test]
fn the_fold_keeps_the_last_stated_window_for_each_pair() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([
        lowered("manifest", OPUS, 200_000),
        lowered("env", OPUS, 500_000),
        lowered("manifest", OPUS, 150_000),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert_eq!(
        logged.windows,
        [
            ("env".to_owned(), OPUS.to_owned(), 500_000),
            ("manifest".to_owned(), OPUS.to_owned(), 150_000),
        ]
    );
}

fn rewrite_lines(store: &JsonlStore, id: &str, edit: impl Fn(usize, &str) -> String) {
    let path = store.path(id);
    let text = fs::read_to_string(&path).unwrap();
    let out: String = text
        .lines()
        .enumerate()
        .map(|(i, line)| format!("{}\n", edit(i, line)))
        .collect();
    fs::write(&path, out).unwrap();
}

#[test]
fn a_session_whose_turns_were_all_damaged_is_still_found() {
    let dir = temp_dir("lookup-damaged");
    let store = JsonlStore::open(&dir).unwrap();
    session_here(&store, A, "~/wisp", None, 2, 30);
    rewrite_lines(&store, A, |i, line| {
        if i > 0 && line.contains("\"turn_started\"") {
            "#garbled#".to_owned()
        } else {
            line.to_owned()
        }
    });
    let found = find(&store, Wanted::Latest { here: "~/wisp" }).unwrap();
    assert_eq!(found.id, A);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_damaged_first_line_takes_its_directory_from_a_later_start() {
    let dir = temp_dir("lookup-first-line");
    let store = JsonlStore::open(&dir).unwrap();
    session_here(&store, A, "~/wisp", None, 1, 30);
    rewrite_lines(&store, A, |i, line| {
        if i == 0 {
            "#garbled#".to_owned()
        } else {
            line.to_owned()
        }
    });
    let latest = || find(&store, Wanted::Latest { here: "~/wisp" });
    assert!(latest().is_err(), "no readable directory");
    let by_id = find(&store, Wanted::Id("0001")).unwrap();
    assert_eq!(by_id.id, A);

    let path = store.path(A);
    let mut text = fs::read_to_string(&path).unwrap();
    let seq = text.lines().count();
    text.push_str(&format!(
        "{{\"schema_version\":{},\"seq\":{seq},\"at\":\"2026-10-25T11:00:00Z\",\"type\":\"session_resumed\",\"wisp_version\":\"0.1.0\",\"endpoint\":\"https://api.anthropic.com\",\"endpoint_name\":\"env\",\"cwd\":\"~/wisp\"}}\n",
        LOG_SCHEMA_VERSION
    ));
    fs::write(&path, text).unwrap();
    assert_eq!(latest().unwrap().id, A);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_lost_tool_attachment_is_noted_with_its_type_and_size() {
    let opening = format!(
        r#"{{"schema_version":{LOG_SCHEMA_VERSION},"seq":0,"at":"2026-10-25T10:00:00Z","type":"session_started","session_id":"s1","wisp_version":"0.1.0","model":"claude-opus-5-5","effort":"medium","cache_ttl":"one_hour","endpoint":"https://api.anthropic.com","credential_source":"environment","endpoint_name":"env"}}
{{"schema_version":{LOG_SCHEMA_VERSION},"seq":1,"at":"2026-10-25T10:00:01Z","type":"turn_started","turn":1,"text":"read it","model":"claude-opus-5-5","effort":"medium"}}
"#
    );
    let mut records = parse_jsonl(opening).unwrap();
    let message = wisp_protocol::Message {
        role: wisp_protocol::Role::User,
        content: vec![wisp_protocol::ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: vec![wisp_protocol::ContentBlock::media(
                "application/pdf".parse().unwrap(),
                "JVBERi0=",
            )],
            is_error: false,
        }],
    };
    let sha256 = "07".repeat(32);
    let stored = wisp_protocol::log::LoggedMessage::stored(&message, |source| {
        Some(wisp_protocol::BlobRef {
            media_type: source.media_type().clone(),
            sha256: sha256.clone(),
            bytes: 2_000,
        })
    });
    records.push(LogRecord {
        schema_version: LOG_SCHEMA_VERSION,
        seq: 2,
        at: "2026-10-25T10:00:02Z".into(),
        event: LogEvent::MessageCommitted {
            turn: 1,
            message: stored,
            model: None,
            stop_reason: None,
        },
    });
    let lines = reread(&records).unwrap();
    let store = crate::session::MemoryStore::default();
    let mut recovered = recover(lines, &store, &|_| None).unwrap();
    assert!(recovered.fell_back.is_none());
    assert_eq!(
        recovered.lost,
        [Lost {
            turn: 1,
            sha256,
            note: "[attachment no longer available: application/pdf, 2 KB]".into(),
            transient: None,
        }]
    );
    let notes = lost_notes(&recovered.records, &recovered.lost);
    let fresh = rebuild_checked(
        &store,
        &with_notes(&recovered.records, &notes),
        recovered.start,
    )
    .unwrap();
    let (logged, rebuilt) = recovered.take_checked();
    assert_eq!(rebuilt.map(|rebuilt| *rebuilt), fresh);
    assert_eq!(logged, LoggedSession::read(&recovered.records));
    assert!(recovered.checked.is_none());
}

fn damaged_at(events: &[Value], damaged: &[usize]) -> Vec<Line> {
    log(events)
        .into_iter()
        .enumerate()
        .map(|(index, record)| {
            if damaged.contains(&index) {
                Line::Damaged(wisp_protocol::log::DamageKind::Unreadable)
            } else {
                Line::Record(Box::new(record))
            }
        })
        .collect()
}

fn with_line(mut lines: Vec<Line>, event: LogEvent) -> Vec<Line> {
    let seq = lines.len();
    lines.push(Line::Record(Box::new(LogRecord {
        schema_version: LOG_SCHEMA_VERSION,
        seq: u64::try_from(seq).unwrap(),
        at: at(seq),
        event,
    })));
    lines
}

fn recovered(lines: &[Line]) -> Recovered {
    recover(
        lines.to_vec(),
        &crate::session::MemoryStore::default(),
        &|_| None,
    )
    .unwrap()
}

fn position_of(events: &[Value], wanted: &Value) -> usize {
    events.iter().position(|e| e == wanted).unwrap()
}

fn process_started(n: u64, id: u32) -> Value {
    json!({
        "type": "process_started",
        "turn": n,
        "id": id,
        "call_id": format!("toolu_{id}"),
        "reason": "started",
        "tty": false,
        "stdin": false,
    })
}

fn process_ended(id: u32) -> Value {
    json!({
        "type": "process_ended",
        "id": id,
        "end": {"type": "exited", "exit": {"type": "code", "code": 0}},
        "bytes": 0,
        "lines": 0,
        "duration_ms": 1,
    })
}

fn binding_states(records: &[LogRecord]) -> usize {
    records
        .iter()
        .filter(|r| matches!(r.event, LogEvent::BindingControls { .. }))
        .count()
}

#[test]
fn a_feature_state_from_a_dropped_turn_is_kept_now_and_at_every_later_resume() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.1));
    let lost_reply = said(2, "assistant");
    events.extend([
        turn(2),
        said(2, "user"),
        json!({"type": "binding_controls", "model": OPUS, "state": "fallen_back", "endpoint_name": "env"}),
        lost_reply.clone(),
        step(2, 1, 2_000, true),
        ended(2, 0.2),
    ]);
    let lines = damaged_at(&events, &[position_of(&events, &lost_reply)]);
    let first = recovered(&lines);
    let fell_back = first.fell_back.clone().unwrap();
    assert_eq!(fell_back.last_turn, Some(1));
    assert_eq!(binding_states(&first.records), 1);
    assert!(first.records.iter().all(|r| turn_of(&r.event) != Some(2)));

    let later = with_line(lines, fell_back.event);
    let second = recovered(&later);
    assert!(second.fell_back.is_none());
    assert_eq!(binding_states(&second.records), 1);
    assert!(second.records.iter().all(|r| turn_of(&r.event) != Some(2)));
}

#[test]
fn tree_records_are_carried_through_a_fallback() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.1));
    events.insert(
        events.len() - 1,
        json!({
            "type": "input_accepted", "turn": 1, "id": "tui-4", "mode": "queue", "text": "waiting before fallback",
        }),
    );
    let lost_reply = said(2, "assistant");
    events.extend([
        turn(2),
        said(2, "user"),
        accepted("tui-5", "queue", "queued while stopping"),
        accepted("tui-6", "queue", "already taken"),
        json!({"type": "input_withdrawn", "ids": ["tui-5", "tui-6"], "by": "session_ended"}),
        accepted("tui-7", "queue", "waiting in dropped turn"),
        accepted("tui-8", "queue", "removed by user"),
        json!({"type": "input_withdrawn", "ids": ["tui-8"], "by": "user"}),
        json!({"type": "agent_spawned", "turn": 2, "agent": "/w", "session_id": "s-w", "depth": 1, "model": OPUS, "effort": "high", "config": {}}),
        json!({"type": "agent_renamed", "turn": 2, "agent": "/w", "to": "/v"}),
        json!({"type": "agent_ended", "turn": 2, "agent": "/v", "spent_usd": 0.5}),
        json!({"type": "returned_taken", "ids": ["tui-6"]}),
        lost_reply.clone(),
        step(2, 1, 2_000, true),
        ended(2, 0.2),
    ]);
    let lines = damaged_at(&events, &[position_of(&events, &lost_reply)]);
    let first = recovered(&lines);
    let fell_back = first.fell_back.as_ref().unwrap();
    assert_eq!(fell_back.last_turn, Some(1));
    let returned: Vec<(&str, &str)> = fell_back
        .returned
        .iter()
        .map(|input| (input.id.as_str(), input.text.as_str()))
        .collect();
    assert_eq!(
        returned,
        [
            ("turn-2", "message 2"),
            ("tui-5", "queued while stopping"),
            ("tui-7", "waiting in dropped turn"),
        ]
    );
    let logged = LoggedSession::read(&first.records).unwrap();
    assert_eq!(logged.open_inputs, [InputId("tui-4".into())]);
    assert_eq!(
        logged.accepted[&logged.open_inputs[0]].input.text,
        "waiting before fallback"
    );
    assert!(first.checked.is_none());
    let kept: Vec<String> = first
        .records
        .iter()
        .map(|r| serde_json::to_value(&r.event).unwrap()["type"].to_string())
        .filter(|kind| kind.starts_with("\"agent_"))
        .collect();
    assert_eq!(
        kept,
        ["\"agent_spawned\"", "\"agent_renamed\"", "\"agent_ended\""]
    );
    assert!(first.records.iter().any(|r| matches!(
        &r.event, LogEvent::ReturnedTaken { ids } if ids == &[InputId("tui-6".into())]
    )));
    assert_eq!(ended_records(lines).unwrap(), first.records);
}

#[test]
fn a_record_numbered_differently_from_its_line_fails_the_read() {
    let mut record = log(&[started()]).remove(0);
    record.seq = 1;
    let lines = vec![Line::Record(Box::new(record))];
    assert!(ended_records(lines.clone()).is_err());
    assert!(lookup_records(lines).is_err());
}

#[test]
fn numbering_keeps_the_turns_and_processes_an_earlier_fallback_saw() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.1));
    let first_damage = events.len();
    events.push(turn(2));
    let lines = damaged_at(&events, &[first_damage]);
    let earlier = LogEvent::ResumeFellBack {
        billing: wisp_protocol::Billing::Metered,
        plan_usd: None,
        kept_through: Some(u64::try_from(first_damage - 1).unwrap()),
        dropped_through: Some(u64::try_from(first_damage).unwrap()),
        covered_through: None,
        last_turn: Some(1),
        turns_seen: 9,
        processes_seen: 4,
        spent_usd: 0.0,
        damage: Vec::new(),
    };
    let mut lines = with_line(lines, earlier);
    lines.push(Line::Damaged(wisp_protocol::log::DamageKind::Unreadable));
    let fell_back = recovered(&lines).fell_back.unwrap();
    let LogEvent::ResumeFellBack {
        turns_seen,
        processes_seen,
        ..
    } = fell_back.event
    else {
        panic!("{:?}", fell_back.event);
    };
    assert_eq!((turns_seen, processes_seen), (9, 4));
}

#[test]
fn only_processes_started_in_dropped_turns_are_handed_back() {
    let mut events = vec![started(), turn(1), said(1, "user"), process_started(1, 1)];
    events.extend([said(1, "assistant"), step(1, 1, 1_000, true), ended(1, 0.1)]);
    let lost_reply = said(2, "assistant");
    events.extend([
        turn(2),
        said(2, "user"),
        process_ended(1),
        process_started(2, 2),
        process_ended(2),
        lost_reply.clone(),
        step(2, 1, 2_000, true),
        ended(2, 0.2),
    ]);
    let lines = damaged_at(&events, &[position_of(&events, &lost_reply)]);
    let handed: Vec<(&str, u32)> = recovered(&lines)
        .dropped_processes
        .iter()
        .map(|r| match r.event {
            LogEvent::ProcessStarted { id, .. } => ("started", id),
            LogEvent::ProcessEnded { id, .. } => ("ended", id),
            _ => ("other", 0),
        })
        .collect();
    assert_eq!(handed, [("started", 2), ("ended", 2)]);
}

fn make_child(store: &JsonlStore, id: &str, parent: &str, agent: &str) {
    rewrite_lines(store, id, |i, line| {
        if i > 0 {
            return line.to_owned();
        }
        let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
        record["parent"] = serde_json::json!({
            "session_id": parent, "agent": agent, "depth": 1, "config": {},
        });
        record.to_string()
    });
}

#[test]
fn resuming_without_an_id_skips_child_logs_and_a_childs_id_names_the_root_to_resume() {
    let dir = temp_dir("lookup-tree");
    let store = JsonlStore::open(&dir).unwrap();
    const C: &str = "0003cccc-0000-7000-8000-000000000000";
    session_here(&store, A, "~/wisp", None, 1, 30);
    session_here(&store, B, "~/wisp", None, 1, 10);
    session_here(&store, C, "~/wisp", None, 1, 5);
    make_child(&store, B, A, "/a");
    make_child(&store, C, B, "/a/g");
    let latest = find(&store, Wanted::Latest { here: "~/wisp" }).unwrap();
    assert_eq!(latest.id, A);
    let refusal = |id| find(&store, Wanted::Id(id)).unwrap_err();
    let expected = "is an agent in session 0001aaaa-0000's tree; resume the tree with wisp --resume 0001aaaa-0000";
    assert_eq!(refusal("0002"), format!("session 0002bbbb-0000 {expected}"));
    assert_eq!(refusal("0003"), format!("session 0003cccc-0000 {expected}"));
    assert_eq!(find(&store, Wanted::Id("0001")).unwrap().id, A);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_chat_and_an_agent_started_ten_seconds_later_resume_by_the_short_id() {
    let dir = temp_dir("lookup-short-id");
    let store = JsonlStore::open(&dir).unwrap();
    const ROOT: &str = "01a18e69-13bd-7c2e-9d4f-3a5b6c7d8e9f";
    const AGENT: &str = "01a18e69-3acd-7c2e-9d4f-3a5b6c7d8e9f";
    session_here(&store, ROOT, "~/wisp", None, 1, 30);
    session_here(&store, AGENT, "~/wisp", None, 1, 20);
    make_child(&store, AGENT, ROOT, "/a");
    assert_eq!(crate::session::short_id(ROOT), "01a18e69-13bd");
    let find_id = |prefix: &str| find(&store, Wanted::Id(prefix));
    assert_eq!(find_id(crate::session::short_id(ROOT)).unwrap().id, ROOT);
    assert!(find_id(&ROOT[..8]).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn jsonl_logs_read_a_trees_records_past_damage_and_reopen_through_the_lock() {
    use crate::kit::{JsonlLogs, OpenLog};
    use crate::session::ResumeLock;
    let dir = temp_dir("jsonl-logs");
    let store = JsonlStore::open(&dir).unwrap();
    let mut events = vec![started_in(A, "~/wisp", &[], None)];
    events.extend(full_turn(1, 0.1));
    events.extend(full_turn(2, 0.2));
    events.push(json!({"type": "agent_ended", "turn": 2, "agent": "/w", "spent_usd": 0.5}));
    write_session(&store, A, &events, 5);
    let second = fs::read_to_string(store.path(A))
        .unwrap()
        .lines()
        .position(|l| l.contains("\"turn_started\"") && l.contains("\"turn\":2"))
        .unwrap();
    rewrite_lines(&store, A, |i, line| {
        if i == second + 1 {
            "#garbled#".to_owned()
        } else {
            line.to_owned()
        }
    });
    let logs = JsonlLogs {
        on: true,
        dir: Some(dir.clone()),
    };
    let ended = |records: &[LogRecord]| {
        records
            .iter()
            .any(|r| matches!(r.event, LogEvent::AgentEnded { .. }))
    };
    let bytes = fs::read(store.path(A)).unwrap();
    assert!(!ended(
        &lookup_records(crate::session::read_lines(&bytes).lines).unwrap()
    ));
    let read = logs.read(A).unwrap();
    assert!(matches!(read[0].event, LogEvent::SessionStarted { .. }));
    assert!(ended(&read), "an end past the damage is still read");
    assert!(logs.read("../x").is_err());
    assert_eq!(logs.stamp("../x"), None);
    let stamp = logs.stamp(A).unwrap();
    assert_eq!(stamp.len, bytes.len() as u64);
    {
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(store.path(A))
            .unwrap()
            .write_all(b"\n")
            .unwrap();
    }
    assert_ne!(logs.stamp(A), Some(stamp));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(store.path(A), store.path(B)).unwrap();
        assert!(logs.read(B).is_err(), "a symlinked log isn't followed");
        assert_eq!(logs.stamp(B), None);
    }
    let reopened = logs.reopen(A, ResumeLock::Required).unwrap();
    assert!(!reopened.lines.is_empty());
    assert!(
        logs.reopen(A, ResumeLock::Required).is_err(),
        "the lock is held"
    );
    drop(reopened);
    assert!(logs.reopen(A, ResumeLock::Required).is_ok());
    fs::remove_dir_all(&dir).unwrap();
}

fn step_at(version: u8, betas: Option<&[&str]>) -> Vec<LogRecord> {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.1));
    let mut text = lines(&events);
    let step = text.lines().nth(4).unwrap().to_owned();
    let mut value: Value = serde_json::from_str(&step).unwrap();
    value["schema_version"] = json!(version);
    if let Some(betas) = betas {
        value["betas"] = json!(betas);
    }
    text = text.replace(&step, &value.to_string());
    parse_jsonl(text).unwrap()
}

#[test]
fn a_step_logs_its_betas_from_36_and_an_older_step_reads_as_unknown() {
    let none = step_at(36, None);
    let logged = LoggedSession::read(&none).unwrap();
    assert_eq!(logged.sent_betas, Some(Vec::new()));
    let older = LoggedSession::read(&step_at(35, None)).unwrap();
    assert_eq!(older.sent_betas, None);
    let sent = ["a-beta-2026-01-01", "b-beta-2026-01-01"];
    let listed = LoggedSession::read(&step_at(36, Some(&sent))).unwrap();
    assert_eq!(
        listed.sent_betas,
        Some(sent.iter().map(|b| (*b).to_owned()).collect())
    );
}

#[test]
fn a_step_that_sent_no_betas_writes_no_key_and_a_list_round_trips() {
    let empty = LogEvent::StepEnded {
        list_usd: None,
        model: None,
        turn: 1,
        step: 1,
        usage: None,
        cost_usd: None,
        lower_bound: false,
        prompt_tokens: None,
        response_id: None,
        window: None,
        compact_at: None,
        used: None,
        first_pass: None,
        accepted: true,
        view_len: None,
        started_at: None,
        ttl: None,
        prefix: None,
        queued_ms: None,
        thread: None,
        thread_error: None,
        betas: Vec::new(),
    };
    assert!(!serde_json::to_string(&empty).unwrap().contains("betas"));
    let mut listed = serde_json::to_value(&empty).unwrap();
    listed["betas"] = json!(["a-beta-2026-01-01", "b-beta-2026-01-01"]);
    let back: LogEvent = serde_json::from_value(listed.clone()).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), listed);
}

#[test]
fn a_settings_target_this_build_doesnt_know_is_skipped_and_its_line_still_applies() {
    let known = entry("cache_ttl", setting_text("5m"), false);
    let mut unknown = known.clone();
    unknown["target"] = json!({"name": "a_setting_from_later"});
    let mut path = known.clone();
    path["target"] = json!({"path": "later.place.key"});
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.1));
    events.push(change(OPUS, "medium", "env", vec![unknown, path, known]));
    let records = log(&events);
    let LogEvent::SettingsChanged { changes, .. } = &records.last().unwrap().event else {
        panic!("not a settings change")
    };
    assert_eq!(changes[0].target.known(), None);
    assert_eq!(changes[0].target.to_string(), "a_setting_from_later");
    assert_eq!(changes[1].target.known(), None);
    assert!(changes[2].target.known().is_some());
    let line = serde_json::to_value(&records.last().unwrap().event).unwrap();
    assert_eq!(
        line["changes"][0]["target"],
        json!({"name": "a_setting_from_later"})
    );
    assert_eq!(
        line["changes"][1]["target"],
        json!({"path": "later.place.key"})
    );
    assert_eq!(pending(&events), ["cache_ttl 5m"]);
}

#[test]
fn plan_split_replays_steps_and_terminal_snapshots_without_double_counting() {
    let mut metered = step(1, 1, 1000, true);
    metered["cost_usd"] = json!(12.3);
    let mut plan = step(2, 1, 1000, true);
    plan["usage"]["billing"] = json!("anthropic_plan");
    plan["cost_usd"] = json!(4.1);
    let mut end = ended(2, 16.4);
    end["outcome"]["cost"]["session_plan_usd"] = json!(4.1);
    end["outcome"]["cost"]["session_billing"] = json!("mixed");
    let mut later = step(3, 1, 1000, true);
    later["usage"]["billing"] = json!("chat_gpt_plan");
    later["cost_usd"] = json!(0.9);
    let mut unnamed = started();
    unnamed.as_object_mut().unwrap().remove("endpoint_name");
    let records = log(&[
        unnamed,
        turn(1),
        metered,
        ended(1, 12.3),
        turn_on(2, OPUS, "medium", "a"),
        plan,
        end,
        turn_on(3, OPUS, "medium", "b"),
        later,
    ]);
    assert_eq!(plan_usd(&records), 5.0);
    assert_eq!(billing(&records), Some(wisp_protocol::Billing::Mixed));
    let logged = LoggedSession::read(&records).unwrap();
    let names: Vec<&str> = logged.accounts.keys().map(String::as_str).collect();
    assert_eq!(names, ["a", "b"], "turn 1 named no account");
    for (name, want) in [("a", 4.1), ("b", 0.9)] {
        let spent = logged.accounts[name];
        assert!((spent.usd - want).abs() < 1e-9, "{name}: {spent:?}");
        assert!(
            (spent.plan_usd.unwrap() - want).abs() < 1e-9,
            "{name}: {spent:?}"
        );
    }
    let named: f64 = logged.accounts.values().map(|a| a.usd).sum();
    assert!((named + 12.3 - logged.session_usd).abs() < 1e-9);
}

#[test]
fn plan_split_counts_compaction_retries_once_and_preserves_interrupted_work() {
    let mut retry = compaction_retry(1.0, false);
    retry["turn"] = json!(1);
    retry["step"] = json!(0);
    retry["usage"] = usage(1000, 100);
    retry["usage"]["billing"] = json!("anthropic_plan");
    let mut compact = compacted(Some(2.0));
    compact["usage"]["billing"] = json!("anthropic_plan");
    assert_eq!(plan_usd(&log(&[retry.clone()])), 1.0);
    assert_eq!(plan_usd(&log(&[retry.clone(), compact.clone()])), 2.0);
    let logged = LoggedSession::read(&log(&[started(), retry.clone(), compact])).unwrap();
    let spent = logged.accounts["env"];
    assert_eq!((spent.usd, spent.plan_usd), (2.0, Some(2.0)));
    let mut end = ended(1, 1.5);
    for (billing, plan, want) in [
        ("anthropic_plan", Some(1.5), Some(1.5)),
        ("anthropic_plan", None, None),
        ("metered", None, Some(1.0)),
    ] {
        end["outcome"]["cost"]["session_billing"] = json!(billing);
        end["outcome"]["cost"]["session_plan_usd"] = json!(plan);
        let records = log(&[started(), turn(1), retry.clone(), end.clone()]);
        let logged = LoggedSession::read(&records).unwrap();
        assert_eq!(logged.accounts["env"].plan_usd, want, "{billing} {plan:?}");
    }
}

#[test]
fn plan_fold_ignores_steps_and_retries_outside_an_open_turn() {
    let mut orphan = step(1, 1, 1000, true);
    orphan["usage"]["billing"] = json!("anthropic_plan");
    orphan["cost_usd"] = json!(4.1);
    let records = log(&[started(), orphan.clone()]);
    let logged = LoggedSession::read(&records).unwrap();
    assert_eq!((logged.session_usd, logged.plan_usd), (0.0, 0.0));
    let mut retry = compaction_retry(4.1, false);
    retry["turn"] = json!(1);
    retry["step"] = json!(1);
    retry["usage"] = orphan["usage"].clone();
    let records = log(&[started(), turn(1), ended(1, 0.0), orphan, retry]);
    let logged = LoggedSession::read(&records).unwrap();
    assert_eq!(
        (logged.session_usd, logged.plan_usd, plan_usd(&records)),
        (0.0, 0.0, 0.0)
    );
}

#[test]
fn plan_fold_preserves_mixed_compaction_splits_for_context_reports() {
    let mut compact = compacted(Some(16.4));
    compact["usage"]["billing"] = json!("mixed");
    compact["plan_usd"] = json!(4.1);
    let records = log(&[started(), compact]);
    let logged = LoggedSession::read(&records).unwrap();
    assert_eq!(logged.session_usd, 16.4);
    assert_eq!(logged.plan_usd, 4.1);
    assert_eq!(logged.compaction_usd, Some(16.4));
    assert_eq!(logged.compaction_plan_usd, 4.1);
}

#[test]
fn legacy_mixed_compaction_without_a_split_is_not_assumed_plan_covered() {
    let mut compact = compacted(Some(16.4));
    compact["usage"]["billing"] = json!("mixed");
    let records = log(&[started(), compact]);
    let logged = LoggedSession::read(&records).unwrap();
    assert!(logged.plan_unknown);
    assert!(logged.compaction_plan_unknown);
    assert_eq!(plan_split(&records), None);
}

#[test]
fn rebuilds_leaves_out_a_stored_value_equal_to_now() {
    let off = LaterOptions {
        debug_cache: Some(false),
        ..LaterOptions::default()
    };
    assert!(off.rebuilds(&off).is_empty());
}

mod fixture_logs;
mod record_types;

fn attached(n: u64) -> Value {
    let message = wisp_protocol::Message {
        role: wisp_protocol::Role::User,
        content: vec![wisp_protocol::ContentBlock::ToolResult {
            tool_use_id: format!("toolu_{n}"),
            content: vec![wisp_protocol::ContentBlock::media(
                "application/pdf".parse().unwrap(),
                "JVBERi0=",
            )],
            is_error: false,
        }],
    };
    let stored = wisp_protocol::log::LoggedMessage::stored(&message, |source| {
        Some(wisp_protocol::BlobRef {
            media_type: source.media_type().clone(),
            sha256: format!("{n:064x}"),
            bytes: 2_000,
        })
    });
    json!({"type": "message_committed", "turn": n, "message": stored})
}

struct AnyMedia;

impl wisp_protocol::log::BlobSource for AnyMedia {
    fn image(&mut self, image: &wisp_protocol::log::ImageRef) -> Result<String, String> {
        Err(image.sha256.clone())
    }

    fn media(&mut self, _blob: &wisp_protocol::BlobRef) -> Result<String, String> {
        Ok("JVBERi0=".to_owned())
    }
}

fn long_compacted_log() -> Vec<Value> {
    let mut events = vec![started()];
    for n in 1..=160 {
        let opened = events.len();
        events.extend([turn(n), said(n, "user"), said(n, "assistant")]);
        if n % 10 == 5 {
            events.push(said(n, "assistant"));
            events.push(attached(n));
            events.push(said(n, "assistant"));
        }
        if n == 50 {
            events.push(process_started(n, 1));
        }
        if n == 70 {
            events.push(process_ended(1));
        }
        events.extend([step(n, 1, 1_000 * n, true), ended(n, 0.1 * n as f64)]);
        if n % 40 == 0 && n < 160 {
            events.push(compacted(Some(0.01)));
        }
        if n == 60 {
            events.push(change(
                OPUS,
                "medium",
                "env",
                vec![entry("time", setting_text("off"), false)],
            ));
        }
        if n == 150 {
            let kept = wisp_protocol::log::rebuild_history_with(
                log(&events[..opened]).iter().map(|r| &r.event),
                AnyMedia,
            )
            .unwrap()
            .messages;
            events.push(json!({
                "type": "rewound",
                "to": opened - 1,
                "point": {"type": "turn", "turn": n},
                "reason": "user",
                "messages": kept.len(),
                "blocks": kept.last().map_or(0, |m| m.content.len()),
                "dropped": {"turns": [n], "messages": 2},
                "cache": {"type": "kept"},
            }));
        }
    }
    events
}

#[test]
fn recovering_by_reference_matches_the_copying_oracle_on_a_long_compacted_log() {
    let events = long_compacted_log();
    let intact = reread(&log(&events)).unwrap();
    let middle = position_of(&events, &turn(135));
    let damaged = damaged_at(&events, &[middle + 1]);
    let mut misplaced = reread(&log(&events)).unwrap();
    if let Line::Record(record) = misplaced.remove(middle) {
        misplaced.insert(middle, Line::Misplaced(record));
    }
    let mut open = reread(&log(&events)).unwrap();
    open.truncate(open.len() - 3);
    let store = crate::session::MemoryStore::default();
    let mut shapes = Vec::new();
    for (shape, lines) in [&intact, &damaged, &misplaced, &open]
        .into_iter()
        .enumerate()
    {
        let recovered = recover(lines.clone(), &store, &|_| None);
        assert_eq!(
            recovered,
            recover::oracle::recover(lines, &store, &|_| None)
        );
        let recovered = recovered.unwrap();
        assert!(!recovered.lost.is_empty());
        shapes.push(recovered.fell_back.is_some());
        if shape == 1 {
            let fell_back = recovered.fell_back.unwrap();
            assert!(!fell_back.settings_dropped);
            let returned: Vec<&str> = fell_back.returned.iter().map(|r| r.text.as_str()).collect();
            assert!(returned.contains(&"message 136"), "{returned:?}");
            assert!(!returned.contains(&"message 150"), "{returned:?}");
        }
    }
    assert_eq!(shapes, [false, true, true, false]);
}

#[test]
fn spend_records_without_a_split_read_as_unknown() {
    let old = json!({
        "type": "resume_fell_back", "kept_through": 3, "dropped_through": 5, "last_turn": 1,
        "turns_seen": 2, "processes_seen": 0, "spent_usd": 0.5, "damage": [],
    });
    let mut split = old.clone();
    split["plan_usd"] = json!(0.5);
    split["billing"] = json!("anthropic_plan");
    let mut metered = old.clone();
    metered["plan_usd"] = json!(0.0);
    for (record, want) in [
        (old, (0.0, true)),
        (split, (0.5, false)),
        (metered, (0.0, false)),
    ] {
        let clean = [started(), turn(1), ended(1, 0.0)];
        let mut events = clean.to_vec();
        events.push(record.clone());
        let read = LoggedSession::read(&log(&events)).unwrap();
        assert_eq!(read.session_usd, 0.5, "{record}");
        assert_eq!((read.plan_usd, read.plan_unknown), want, "{record}");
        assert!(read.accounts.is_empty(), "{record}");
        let mut fresh = LoggedSession::read(&log(&clean)).unwrap();
        fresh.fell_back(&serde_json::from_value(record.clone()).unwrap());
        assert_eq!(fresh.session_usd, 0.5, "{record}");
        assert_eq!((fresh.plan_usd, fresh.plan_unknown), want, "{record}");
    }
    let old = json!({"type": "agent_ended", "turn": 1, "agent": "/w", "spent_usd": 0.5});
    let mut split = old.clone();
    split["plan_usd"] = json!(0.5);
    split["billing"] = json!("anthropic_plan");
    let mut metered = old.clone();
    metered["plan_usd"] = json!(0.0);
    for (record, want) in [
        (old, (None, wisp_protocol::Billing::Mixed)),
        (split, (Some(0.5), wisp_protocol::Billing::AnthropicPlan)),
        (metered, (None, wisp_protocol::Billing::Metered)),
    ] {
        let links = crate::agents::fold::links(&log(&[started(), record.clone()]));
        let end = links.ended.values().next().unwrap();
        assert_eq!(end.spent_usd, 0.5, "{record}");
        assert_eq!(end.split(), want, "{record}");
    }
}

#[test]
fn a_fallback_writes_the_dropped_plan_part_and_reads_it_back() {
    for (billing, plan) in [
        (wisp_protocol::Billing::AnthropicPlan, 0.2),
        (wisp_protocol::Billing::Metered, 0.0),
    ] {
        let mut second = full_turn(2, 0.3);
        second[3]["usage"]["billing"] = json!(billing);
        second[3]["cost_usd"] = json!(0.2);
        second[4]["outcome"]["cost"]["session_plan_usd"] = json!(plan);
        let lost = second[2].clone();
        let mut events = vec![started()];
        events.extend(full_turn(1, 0.1));
        events.extend(second);
        let lines = damaged_at(&events, &[position_of(&events, &lost)]);
        let first = recovered(&lines);
        let fell_back = first.fell_back.clone().unwrap();
        let LogEvent::ResumeFellBack {
            spent_usd,
            plan_usd,
            billing: written,
            ..
        } = &fell_back.event
        else {
            panic!("{:?}", fell_back.event)
        };
        assert!((spent_usd - 0.2).abs() < 1e-9, "{billing:?} {spent_usd}");
        assert!(
            plan_usd.is_some_and(|p| (p - plan).abs() < 1e-9),
            "{billing:?} {plan_usd:?}"
        );
        assert_eq!(*written, billing);
        let later = with_line(lines, fell_back.event.clone());
        let read = LoggedSession::read(&recovered(&later).records).unwrap();
        let mut fresh = LoggedSession::read(&first.records).unwrap();
        fresh.fell_back(&fell_back.event);
        for logged in [read, fresh] {
            assert!(!logged.plan_unknown, "{billing:?}");
            assert!((logged.plan_usd - plan).abs() < 1e-9, "{billing:?}");
        }
    }
}

#[test]
fn search_switched_off_and_search_on_follow_in_chat_settings() {
    let plugin_entry = |key: &str, value: Option<bool>, unset: bool| {
        serde_json::to_value(LoggedChange {
            target: SettingTarget::Path(
                wisp_protocol::settings::SitePath::plugin_switch("web-search", key).unwrap(),
            )
            .into(),
            value: value.map(SettingValue::Bool),
            layer: Layer::Session,
            when: When::NextTurn,
            saved: None,
            unset,
            by: None,
        })
        .unwrap()
    };
    let settings = |entries: Vec<Value>| change(OPUS, "medium", "env", entries);
    let read = |events: &[Value]| LoggedSession::read(&log(events)).unwrap();
    let plugin = Some("web-search");

    let plain = read(&[started()]);
    assert!(!plain.search_switched_off(plugin));
    assert!(!plain.search_on());

    let mut with_search = started();
    with_search["tools"] = json!(["bash", "web_search"]);
    assert!(read(&[with_search.clone()]).search_on());

    let enabled_off = settings(vec![plugin_entry("enabled", Some(false), false)]);
    let enabled_on = settings(vec![plugin_entry("enabled", Some(true), false)]);
    let enabled_unset = settings(vec![plugin_entry("enabled", None, true)]);
    let installed_off = settings(vec![plugin_entry("installed", Some(false), false)]);
    let tools_off = settings(vec![entry(
        "tools",
        SettingValue::List(vec!["bash".into()]),
        false,
    )]);
    let tools_on = settings(vec![entry(
        "tools",
        SettingValue::List(vec!["bash".into(), "web_search".into()]),
        false,
    )]);
    let turned_off_search = settings(vec![entry(
        "turned_off",
        SettingValue::List(vec!["web_search".into()]),
        false,
    )]);
    let turned_off_other = settings(vec![entry(
        "turned_off",
        SettingValue::List(vec!["bash".into()]),
        false,
    )]);
    for (events, off) in [
        (vec![started(), enabled_off.clone()], true),
        (vec![started(), enabled_off.clone(), enabled_on], false),
        (vec![started(), enabled_off, enabled_unset], false),
        (vec![started(), installed_off], true),
        (vec![started(), tools_off], true),
        (vec![started(), tools_on], false),
        (vec![started(), turned_off_search], true),
        (vec![started(), turned_off_other], false),
    ] {
        assert_eq!(read(&events).search_switched_off(plugin), off, "{events:?}");
    }

    let inline_add = json!({
        "type": "tools_changed",
        "turn": 1,
        "added": ["web_search"],
        "removed": [],
        "via": "inline",
        "cache": {"type": "kept"},
    });
    let inline_remove = json!({
        "type": "tools_changed",
        "turn": 1,
        "added": [],
        "removed": ["web_search"],
        "via": "inline",
        "cache": {"type": "kept"},
    });
    assert!(read(&[started(), inline_add.clone()]).search_on());
    assert!(!read(&[with_search.clone(), inline_remove.clone()]).search_on());

    let mut replaced_tools = change(OPUS, "medium", "env", vec![]);
    replaced_tools["tools"] = json!(["bash"]);
    replaced_tools["tools_sha256"] = json!("deadbeef");
    let switched_ep = change(OPUS, "medium", "gw", vec![]);
    let effort_only = change(OPUS, "low", "env", vec![]);
    let rewound_to_start = json!({
        "type": "rewound",
        "to": 0,
        "point": {"type": "turn", "turn": 1},
        "reason": "user",
        "messages": 0,
        "blocks": 0,
        "dropped": {"turns": [1], "messages": 2},
        "cache": {"type": "kept"},
    });
    assert!(!read(&[started(), inline_add.clone(), replaced_tools]).search_on());
    assert!(!read(&[started(), inline_add.clone(), switched_ep]).search_on());
    assert!(read(&[started(), inline_add.clone(), effort_only]).search_on());
    assert!(!read(&[started(), inline_add, rewound_to_start.clone()]).search_on());
    assert!(read(&[with_search, inline_remove, rewound_to_start]).search_on());
}

mod listing;
