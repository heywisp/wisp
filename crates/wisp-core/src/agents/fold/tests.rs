use serde_json::{Value, json};
use wisp_protocol::TodoStatus;

use super::*;
use crate::test_guard::path;

#[test]
fn legacy_checklists_scrub_detected_fields_in_live_and_ended_folds() {
    use crate::session::REDACTED;

    let email = ["CANARY-fold", "example.invalid"].join("@");
    let log = Log::new().push(json!({
        "type": "todos_updated", "turn": 1, "call_id": "todos",
        "items": [
            {"text": "ops_CANARY12345678901234567890", "status": "pending", "section": email},
            {"text": "Keep the ordinary step", "status": "completed"},
        ]
    }));
    let folded = fold(&path("/child"), &log.records, &[]);
    assert_eq!(folded.checklist, ended_row(&log.records).checklist);
    assert_eq!(folded.checklist[0].text, REDACTED);
    assert_eq!(folded.checklist[0].section.as_deref(), Some(REDACTED));
    assert_eq!(folded.checklist[1].text, "Keep the ordinary step");
    assert_eq!(folded.checklist[1].status, TodoStatus::Completed);
}

struct Log {
    records: Vec<LogRecord>,
}

impl Log {
    fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    fn push(mut self, mut event: Value) -> Self {
        let seq = self.records.len() + 1;
        if let Some(fields) = event.as_object_mut() {
            fields.insert("schema_version".into(), json!(28));
            fields.insert("seq".into(), json!(seq));
            fields.insert("at".into(), json!("2026-10-28T12:00:00Z"));
        }
        let record = serde_json::from_value(event).unwrap();
        self.records.push(record);
        self
    }

    fn mail(self, n: u64, from: &str, kind: &str, wake: bool) -> Self {
        self.push(json!({
            "type": "input_accepted", "turn": null, "id": format!("mail-m{n}"), "mode": "steer",
            "text": "mail",
            "from": {"agent": from, "kind": kind, "mail": format!("m{n}"), "wake": wake},
        }))
    }

    fn typed(self, id: &str) -> Self {
        self.push(json!({
            "type": "input_accepted", "turn": null, "id": id, "mode": "steer", "text": "typed",
        }))
    }

    fn started(self, turn: u64, inputs: &[&str]) -> Self {
        let inputs: Vec<Value> = inputs
            .iter()
            .map(|i| json!({"id": i, "text": "in"}))
            .collect();
        self.push(json!({"type": "turn_started", "turn": turn, "inputs": inputs}))
    }

    fn committed(self, turn: u64, inputs: &[&str]) -> Self {
        let inputs: Vec<Value> = inputs
            .iter()
            .map(|i| json!({"id": i, "text": "in"}))
            .collect();
        self.push(json!({"type": "input_committed", "turn": turn, "step": 2, "inputs": inputs}))
    }

    fn says(self, turn: u64, text: &str) -> Self {
        self.push(json!({
            "type": "message_committed", "turn": turn, "model": "m",
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
        }))
    }

    fn sends(self, turn: u64, step: u32, call: &str, to: &str, wake: bool, ok: bool) -> Self {
        self.push(json!({
            "type": "tool_call_requested", "turn": turn, "step": step, "block": 0,
            "call_id": call, "tool": SEND_MESSAGE,
            "input": {"to": to, "message": "hi", "wake": wake},
            "display": {"action": {"type": "run"}, "target": to, "output": {"type": "text"}},
        }))
        .push(json!({
            "type": "tool_call_finished", "turn": turn, "call_id": call,
            "outcome": {"status": "completed", "is_error": !ok, "exit": null},
            "output": {"tail": "", "bytes": 0, "lines": 0, "file": null, "file_problem": null, "inlined": true},
            "facts": [], "state": null, "duration_ms": 1,
        }))
    }

    fn ended(self, turn: u64, status: &str, duration_ms: u64) -> Self {
        let outcome = match status {
            "completed" => json!({
                "status": "completed", "stop_reason": "end_turn", "model": "m",
                "usage": {
                    "input_tokens": 1, "output_tokens": 1, "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                },
                "cost": null, "committed": true, "steps": 1, "tool_calls": 0,
            }),
            "cancelled" => json!({"status": "cancelled", "usage": null, "cost": null}),
            "interrupted" => json!({
                "status": "error", "kind": "interrupted", "message": "Wisp stopped", "usage": null,
                "cost": null,
            }),
            _ => json!({
                "status": "error", "kind": "server", "message": "down", "usage": null, "cost": null,
            }),
        };
        self.push(json!({
            "type": "turn_ended", "turn": turn, "outcome": outcome,
            "stats": {"started_at": "2026-10-28T12:00:00Z", "duration_ms": duration_ms, "retries": 0},
        }))
    }

    fn reported(self, turn: u64, outcome: &str, summary: Option<&str>) -> Self {
        let mut event = json!({
            "type": "agent_reported", "turn": turn, "agent": "/w", "outcome": outcome,
            "duration_ms": 5,
        });
        if let (Some(summary), Some(fields)) = (summary, event.as_object_mut()) {
            fields.insert("report".into(), json!({"summary": summary}));
        }
        self.push(event)
    }

    fn session_ended(self, turns: u64, interrupted: Option<u64>) -> Self {
        self.push(json!({"type": "session_ended", "turns": turns, "interrupted": interrupted}))
    }

    fn session_resumed(self) -> Self {
        self.push(json!({
            "type": "session_resumed", "wisp_version": "0.1.0", "endpoint": "e",
            "endpoint_name": "env", "system_prompt_sha256": "a", "tools": [],
            "tools_sha256": "b", "cwd": "~/w", "gap_ms": 1, "truncated_bytes": 0,
        }))
    }

    fn todos(self, turn: u64, statuses: &[&str]) -> Self {
        let items: Vec<Value> = statuses
            .iter()
            .enumerate()
            .map(|(i, s)| json!({"text": format!("item {i}"), "status": s}))
            .collect();
        self.push(json!({"type": "todos_updated", "turn": turn, "call_id": "t", "items": items}))
    }

    fn fold(&self) -> Folded {
        fold(&path("/w"), &self.records, &[])
    }
}

fn tasked() -> Log {
    Log::new()
        .mail(1, "/", "task", true)
        .started(1, &["mail-m1"])
}

#[test]
fn a_task_from_above_is_owed_until_the_child_reports_and_the_report_names_its_first_line() {
    let waiting = tasked().says(1, "working").ended(1, "completed", 10);
    let folded = waiting.fold();
    assert!(folded.owes);
    assert_eq!(folded.outcome, Some(IdleOutcome::Waiting));
    assert_eq!(folded.last_report, None);

    let done = waiting.reported(1, "reported", Some("Done.\nThe rest."));
    let folded = done.fold();
    assert!(!folded.owes);
    assert_eq!(folded.outcome, Some(IdleOutcome::Reported));
    assert_eq!(folded.last_report.as_deref(), Some("Done."));
}

#[test]
fn a_turn_without_a_report_names_its_last_texts_first_line_and_a_new_task_owes_again() {
    let log = tasked()
        .says(1, "\nFirst line\nsecond")
        .ended(1, "completed", 10)
        .reported(1, "no_report", None);
    let folded = log.fold();
    assert_eq!(folded.outcome, Some(IdleOutcome::NoReport));
    assert_eq!(folded.last_report.as_deref(), Some("First line"));
    assert!(!folded.owes);

    let again = log.mail(2, "/", "task", true).fold();
    assert!(again.owes);
}

#[test]
fn a_last_step_that_messaged_a_sibling_with_wake_leaves_the_child_asking_it() {
    let asking = tasked()
        .sends(1, 1, "c1", "/s", true, true)
        .ended(1, "completed", 10)
        .fold();
    assert_eq!(asking.outcome, Some(IdleOutcome::Asked));
    assert_eq!(asking.asking, BTreeSet::from([path("/s")]));

    let quiet = tasked()
        .sends(1, 1, "c1", "/s", false, true)
        .ended(1, "completed", 10)
        .fold();
    assert_eq!(quiet.outcome, Some(IdleOutcome::Waiting));
    assert!(quiet.asking.is_empty());

    let refused = tasked()
        .sends(1, 1, "c1", "/s", true, false)
        .ended(1, "completed", 10)
        .fold();
    assert_eq!(refused.outcome, Some(IdleOutcome::Waiting));

    let earlier = tasked()
        .sends(1, 1, "c1", "/s", true, true)
        .says(1, "more")
        .push(json!({
            "type": "tool_call_requested", "turn": 1, "step": 2, "block": 0, "call_id": "c2",
            "tool": "bash", "input": {"command": "ls"},
            "display": {"action": {"type": "run"}, "target": "ls", "output": {"type": "text"}},
        }))
        .ended(1, "completed", 10)
        .fold();
    assert_eq!(earlier.outcome, Some(IdleOutcome::Waiting));
}

#[test]
fn a_waking_sibling_question_stays_open_until_answered_or_a_turn_ends() {
    let asked = Log::new()
        .mail(1, "/s", "message", true)
        .started(1, &["mail-m1"])
        .says(1, "thinking");
    assert_eq!(asked.fold().questions, BTreeSet::from([path("/s")]));

    let answered = asked.sends(1, 1, "c1", "/s", false, true);
    assert!(answered.fold().questions.is_empty());

    let stopped = Log::new()
        .mail(1, "/s", "message", true)
        .started(1, &["mail-m1"])
        .says(1, "thinking")
        .ended(1, "cancelled", 10)
        .fold();
    assert_eq!(stopped.questions, BTreeSet::from([path("/s")]));
    assert_eq!(stopped.outcome, Some(IdleOutcome::Stopped));

    let completed = Log::new()
        .typed("tui-1")
        .started(1, &["tui-1"])
        .says(1, "thinking")
        .mail(1, "/s", "message", true)
        .committed(1, &["mail-m1"]);
    assert_eq!(completed.fold().questions, BTreeSet::from([path("/s")]));
    assert!(
        completed
            .ended(1, "completed", 10)
            .fold()
            .questions
            .is_empty()
    );

    let not_waking = Log::new()
        .mail(1, "/s", "message", false)
        .started(1, &["mail-m1"])
        .says(1, "ok")
        .fold();
    assert!(not_waking.questions.is_empty());
}

#[test]
fn sibling_messages_count_toward_the_peer_cap_until_mail_from_above_or_you() {
    let log = Log::new()
        .mail(1, "/s", "message", true)
        .mail(2, "/s", "message", false)
        .mail(3, "/t", "message", true)
        .mail(4, "/s", "report", true);
    assert_eq!(
        log.fold().peers,
        BTreeMap::from([(path("/s"), 2), (path("/t"), 1)])
    );

    let from_above = log.mail(5, "/", "message", true).fold();
    assert!(from_above.peers.is_empty());

    let typed = Log::new().mail(1, "/s", "message", true).typed("tui-1");
    assert!(typed.fold().peers.is_empty());

    let notice = Log::new()
        .mail(1, "/s", "message", true)
        .mail(2, "/", "notice", false);
    assert_eq!(notice.fold().peers, BTreeMap::from([(path("/s"), 1)]));
}

#[test]
fn a_turn_that_ended_in_0_ms_still_ran_and_a_turn_that_never_ended_did_not() {
    assert_eq!(tasked().ended(1, "completed", 0).fold().running_ms, Some(0));
    assert_eq!(tasked().fold().running_ms, None);
}

#[test]
fn running_time_sums_every_turn_and_the_checklist_is_the_last_one_written() {
    let log = tasked()
        .todos(1, &["completed", "in_progress"])
        .ended(1, "completed", 1500)
        .mail(2, "/", "message", true)
        .started(2, &["mail-m2"])
        .todos(2, &["completed", "completed", "pending"])
        .ended(2, "cancelled", 250)
        .reported(2, "reported", Some("Done."));
    let folded = log.fold();
    assert_eq!(folded.running_ms, Some(1750));
    assert_eq!(folded.last_report.as_deref(), Some("Done."));
    assert!(folded.owes);
    let row = Folded {
        running_ms: folded.running_ms,
        last_report: folded.last_report.clone(),
        checklist: folded.checklist.clone(),
        ..Folded::default()
    };
    assert_eq!(Folded::from(ended_row(&log.records)), row);
    let statuses: Vec<TodoStatus> = folded.checklist.iter().map(|i| i.status).collect();
    assert_eq!(
        statuses,
        [
            TodoStatus::Completed,
            TodoStatus::Completed,
            TodoStatus::Pending
        ]
    );
}

#[test]
fn a_stopped_or_failed_turn_sets_the_idle_outcome_even_when_nothing_is_owed() {
    let message = || {
        Log::new()
            .mail(1, "/", "message", true)
            .started(1, &["mail-m1"])
    };
    assert_eq!(
        message().ended(1, "cancelled", 1).fold().outcome,
        Some(IdleOutcome::Stopped)
    );
    assert_eq!(
        message().ended(1, "error", 1).fold().outcome,
        Some(IdleOutcome::Failed)
    );
    assert_eq!(message().ended(1, "completed", 1).fold().outcome, None);
    let repaired = message().ended(1, "interrupted", 1).fold();
    assert_eq!(repaired.outcome, Some(IdleOutcome::Stopped));
    assert!(
        !repaired.interrupted,
        "a repaired turn sends no second notice"
    );
    let reported = tasked()
        .says(1, "done")
        .ended(1, "completed", 1)
        .reported(1, "reported", Some("ok"))
        .mail(2, "/", "message", true)
        .started(2, &["mail-m2"])
        .ended(2, "completed", 1)
        .fold();
    assert_eq!(reported.outcome, Some(IdleOutcome::Reported));
}

#[test]
fn an_open_turn_or_one_the_exit_named_is_interrupted_and_stopped() {
    let open = tasked()
        .sends(1, 1, "c1", "/s", true, true)
        .session_ended(0, None)
        .fold();
    assert!(open.interrupted);
    assert_eq!(open.outcome, Some(IdleOutcome::Stopped));
    assert!(open.asking.is_empty());
    assert!(open.owes);

    let named = tasked()
        .ended(1, "cancelled", 1)
        .session_ended(1, Some(1))
        .fold();
    assert!(named.interrupted);

    let resumed = tasked()
        .ended(1, "cancelled", 1)
        .session_ended(1, Some(1))
        .session_resumed()
        .session_ended(1, None)
        .fold();
    assert!(!resumed.interrupted);
    assert_eq!(resumed.outcome, Some(IdleOutcome::Stopped));

    let older = tasked()
        .ended(1, "cancelled", 1)
        .session_ended(1, Some(1))
        .session_resumed()
        .mail(2, "/", "message", true)
        .started(2, &["mail-m2"])
        .ended(2, "completed", 1)
        .session_ended(2, None)
        .fold();
    assert!(!older.interrupted);
    assert_eq!(older.outcome, Some(IdleOutcome::Waiting));

    let clean = tasked()
        .ended(1, "completed", 1)
        .session_ended(1, None)
        .fold();
    assert!(!clean.interrupted);
}

#[test]
fn a_completed_turn_an_older_exit_named_stays_stopped_through_later_resumes() {
    let named = || {
        tasked()
            .ended(1, "completed", 1)
            .session_ended(1, Some(1))
            .session_resumed()
    };
    let first = tasked()
        .ended(1, "completed", 1)
        .session_ended(1, Some(1))
        .fold();
    assert!(first.interrupted);
    assert_eq!(first.outcome, Some(IdleOutcome::Stopped));

    let second = named().session_ended(1, None).fold();
    assert_eq!(second.outcome, Some(IdleOutcome::Stopped));
    assert!(
        !second.interrupted,
        "the first resume already told the parent"
    );
    assert_eq!(
        named()
            .session_ended(1, None)
            .session_resumed()
            .session_ended(1, None)
            .fold()
            .outcome,
        Some(IdleOutcome::Stopped)
    );

    let crashed = named().fold();
    assert_eq!(crashed.outcome, Some(IdleOutcome::Stopped));
    assert!(!crashed.interrupted);

    let records = named().session_ended(1, None).records;
    assert!(exit_named(&records, 1));
    assert!(!exit_named(&records, 2));
}

#[test]
fn links_collect_spawns_ends_and_the_highest_mail_id_even_past_a_rewind() {
    let log = Log::new()
        .push(json!({
            "type": "agent_spawned", "turn": 1, "agent": "/w", "session_id": "s-w", "depth": 1,
            "model": "m", "effort": "high", "cwd": null, "config": {},
        }))
        .mail(7, "/w", "report", true)
        .mail(3, "/w", "message", true)
        .push(json!({"type": "agent_ended", "turn": 2, "agent": "/w", "spent_usd": 0.5}))
        .push(json!({
            "type": "rewound", "to": 1, "point": {"type": "turn", "turn": 2}, "reason": "user",
            "messages": 2, "blocks": 1, "dropped": {"turns": [2], "messages": 2},
            "cache": {"type": "kept"},
        }));
    let links = links(&log.records);
    let spawned: Vec<(&str, &str)> = links
        .spawned
        .iter()
        .map(|s| (s.path.as_str(), s.session_id.as_str()))
        .collect();
    assert_eq!(spawned, [("/w", "s-w")]);
    assert_eq!(
        links.ended,
        BTreeMap::from([(
            path("/w"),
            LoggedEnd {
                spent_usd: 0.5,
                plan_usd: None,
                billing: wisp_protocol::Billing::Metered,
                at: "2026-10-28T12:00:00Z".to_owned(),
            }
        )])
    );
    assert_eq!(links.mail, 7);
}

#[test]
fn an_agent_is_ended_when_it_or_one_above_it_ended_but_not_for_a_name_that_only_starts_the_same() {
    let ended = links(
        &Log::new()
            .push(json!({"type": "agent_ended", "turn": 1, "agent": "/a", "spent_usd": 0.5}))
            .push(json!({"type": "agent_ended", "turn": 1, "agent": "/b/c", "spent_usd": 0.5}))
            .records,
    )
    .ended;
    let paths = ["/", "/a", "/a/x/y", "/ab", "/b", "/b/c", "/b/c/d", "/b/cd"];
    let counted: Vec<&str> = paths
        .into_iter()
        .filter(|p| ended_above(&ended, &path(p)))
        .collect();
    assert_eq!(counted, ["/a", "/a/x/y", "/b/c", "/b/c/d"]);
}

fn stamped(log: Log) -> Log {
    let mut log = log;
    for (i, record) in log.records.iter_mut().enumerate() {
        record.at = format!("2026-10-28T12:00:{i:02}Z");
    }
    log
}

fn at(i: usize) -> Option<String> {
    Some(format!("2026-10-28T12:00:{i:02}Z"))
}

#[test]
fn since_is_the_at_of_the_record_that_settled_the_folded_state() {
    let crashed = stamped(tasked().says(1, "working"));
    assert_eq!(crashed.fold().since, at(crashed.records.len() - 1));
    let no_turn = stamped(Log::new().mail(1, "/", "task", true).typed("u1"));
    assert_eq!(no_turn.fold().since, at(0));
    let idle = stamped(tasked().ended(1, "completed", 10).session_ended(1, None));
    assert_eq!(idle.fold().since, at(2));
    let stopped = stamped(
        tasked()
            .ended(1, "cancelled", 10)
            .session_ended(1, Some(1))
            .typed("u1"),
    );
    assert_eq!(stopped.fold().since, at(3));
}
