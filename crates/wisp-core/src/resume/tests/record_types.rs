use wisp_protocol::Billing;
use wisp_protocol::endpoint::{CheckResult, ConnectionField, NotSavedReason, StoreKind};

use super::*;
use crate::session::read_lines;

fn after_a_turn(event: &LogEvent) -> Vec<Line> {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(serde_json::to_value(event).unwrap());
    read_lines(lines(&events).as_bytes()).lines
}

fn misplaced(event: &LogEvent) -> Line {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.push(serde_json::to_value(event).unwrap());
    let text = lines(&events).replace(r#""seq":6,"#, r#""seq":99,"#);
    read_lines(text.as_bytes()).lines.pop().unwrap()
}

fn assert_kept(event: &LogEvent) {
    let read = after_a_turn(event);
    assert!(
        matches!(read.last(), Some(Line::Record(r)) if r.event == *event),
        "{:?}",
        read.last()
    );
    assert!(
        lookup_records(read)
            .unwrap()
            .iter()
            .any(|r| r.event == *event)
    );
}

fn assert_turn_kept_when_misplaced(event: &LogEvent) {
    match misplaced(event) {
        Line::Misplaced(record) => assert_eq!(turn_of(&record.event), Some(1), "{event:?}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_prepared_summary_is_charged_on_resume() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    let mut prepared = usage(5_000, 300);
    prepared["billing"] = json!("anthropic_plan");
    events.push(json!({
        "type": "compaction_prepared",
        "epoch": 1,
        "block": {"type": "compaction", "content": "So far."},
        "usage": prepared,
        "cost_usd": 0.5,
        "duration_ms": 900,
    }));
    let records = log(&events);
    assert!(matches!(
        records.last().map(|r| &r.event),
        Some(LogEvent::CompactionPrepared { .. })
    ));
    let logged = LoggedSession::read(&records).unwrap();
    assert!(
        (logged.session_usd - 0.75).abs() < 1e-9,
        "{}",
        logged.session_usd
    );
    assert_eq!(billing(&records), Some(Billing::Mixed));
}

#[test]
fn a_saved_endpoint_is_read_back_and_kept_on_resume() {
    let saved = |turn| LogEvent::EndpointSaved {
        turn,
        name: "manifest".into(),
        store: StoreKind::Keyring,
        check: CheckResult::skipped(),
    };
    assert_kept(&saved(None));
    assert_turn_kept_when_misplaced(&saved(Some(1)));
}

#[test]
fn an_endpoint_not_saved_is_read_back_and_kept_on_resume() {
    let not_saved = |turn| LogEvent::EndpointNotSaved {
        turn,
        name: "manifest".into(),
        why: NotSavedReason::KeychainFailed("didn't answer".into()),
    };
    assert_kept(&not_saved(None));
    assert_turn_kept_when_misplaced(&not_saved(Some(1)));
}

#[test]
fn a_changed_endpoint_list_is_read_back_and_kept_on_resume() {
    let changed = |turn| LogEvent::EndpointsChanged {
        turn,
        endpoints: Vec::new(),
    };
    assert_kept(&changed(None));
    assert_turn_kept_when_misplaced(&changed(Some(1)));
}

#[test]
fn an_edited_endpoint_is_read_back_and_kept_on_resume() {
    let edited = |turn| LogEvent::EndpointEdited {
        turn,
        name: "manifest".into(),
        changed: vec![ConnectionField::Key, ConnectionField::Store],
        store: Some(StoreKind::File),
        check: CheckResult::skipped(),
    };
    assert_kept(&edited(None));
    assert_turn_kept_when_misplaced(&edited(Some(1)));
}

#[test]
fn a_removed_endpoint_is_read_back_and_kept_on_resume() {
    let removed = |turn| LogEvent::EndpointRemoved {
        turn,
        name: "direct".into(),
        kept_item: false,
        default_cleared: true,
    };
    assert_kept(&removed(None));
    assert_turn_kept_when_misplaced(&removed(Some(1)));
}

#[test]
fn a_cut_drops_the_hook_runs_of_the_unfinished_turn() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    let ran = LogEvent::HookRan {
        hook: "acme/check".into(),
        point: "before-turn".into(),
        turn: Some(2),
        step: None,
        call_id: None,
        outcome: wisp_protocol::HookOutcome::Applied,
        ms: 4,
    };
    let applied = LogEvent::HookApplied {
        hook: "acme/check".into(),
        point: "before-turn".into(),
        turn: Some(2),
        step: None,
        call_id: None,
        input_id: None,
        change: wisp_protocol::HookChange::Text {
            before: "a".into(),
            after: "b".into(),
        },
    };
    for event in [&ran, &applied] {
        events.push(serde_json::to_value(event).unwrap());
    }
    events.push(turn(2));
    let records = log(&events);
    let borrowed: Vec<&LogRecord> = records.iter().collect();
    let kept = cut(&borrowed, 100);
    assert_eq!(kept.len(), 6, "{kept:?}");
    assert!(matches!(
        kept.last().map(|r| &r.event),
        Some(LogEvent::TurnEnded { turn: 1, .. })
    ));
}

#[test]
fn a_misplaced_todo_list_keeps_its_turn() {
    assert_turn_kept_when_misplaced(&LogEvent::TodosUpdated {
        turn: 1,
        call_id: "toolu_1".into(),
        items: Vec::new(),
    });
}

#[test]
fn a_summary_prepared_mid_turn_is_charged_to_the_open_turn() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([turn(2), said(2, "user"), step(2, 1, 2_000, true)]);
    let prepared = |turn: Option<u64>, cost_usd: Option<f64>| {
        json!({
            "type": "compaction_prepared",
            "turn": turn,
            "epoch": 1,
            "block": {"type": "compaction", "content": "So far."},
            "usage": usage(500, 30),
            "cost_usd": cost_usd,
            "duration_ms": 900,
        })
    };
    events.push(prepared(Some(2), Some(0.5)));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    let last = logged.repaired_turn().unwrap();
    assert_eq!(last.turn, 2);
    assert!((last.usd.unwrap() - 0.51).abs() < 1e-9, "{:?}", last.usd);
    assert_eq!(last.usage.unwrap().input_tokens, 2_500);
    assert!(!last.lower_bound);
    assert!((logged.session_usd - 0.76).abs() < 1e-9);

    events.push(prepared(None, Some(0.5)));
    events.push(prepared(Some(1), Some(0.5)));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    let last = logged.repaired_turn().unwrap();
    assert!((last.usd.unwrap() - 0.51).abs() < 1e-9, "{:?}", last.usd);
    assert!((logged.session_usd - 1.76).abs() < 1e-9);

    events.push(prepared(Some(2), None));
    let logged = LoggedSession::read(&log(&events)).unwrap();
    let last = logged.repaired_turn().unwrap();
    assert_eq!(last.usage.unwrap().input_tokens, 3_000);
    assert!(last.lower_bound);
}

#[test]
fn a_summary_prepared_before_its_turn_started_counts_once_in_it() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    events.extend([
        json!({
            "type": "compaction_prepared",
            "turn": 2,
            "epoch": 1,
            "block": {"type": "compaction", "content": "So far."},
            "usage": usage(500, 30),
            "cost_usd": 0.5,
            "duration_ms": 900,
        }),
        turn(2),
        said(2, "user"),
    ]);
    let logged = LoggedSession::read(&log(&events)).unwrap();
    assert!((logged.session_usd - 0.75).abs() < 1e-9);
    let last = logged.repaired_turn().unwrap();
    assert_eq!(last.turn, 2);
    assert!((last.usd.unwrap() - 0.5).abs() < 1e-9, "{:?}", last.usd);
    assert_eq!(last.usage.unwrap().input_tokens, 500);
    assert_eq!(last.retries, 0);
    assert!(!last.lower_bound);
}

#[test]
fn misplaced_records_written_inside_a_turn_keep_their_turn() {
    for event in [
        LogEvent::InputTransformations {
            turn: Some(1),
            provider: "anthropic".into(),
            transformations: Vec::new(),
        },
        LogEvent::AgentRefused {
            turn: Some(1),
            by: wisp_protocol::AgentPath::root(),
            action: wisp_protocol::AgentAction::Send,
            reason: "no such agent".into(),
        },
        LogEvent::WebSearchNotRun {
            turn: 1,
            endpoint_name: "main".into(),
            model: "claude-x".into(),
        },
        LogEvent::AgentReported {
            turn: 1,
            agent: wisp_protocol::AgentPath::new("/review"),
            outcome: wisp_protocol::ReportOutcome::NoReport,
            report: None,
            duration_ms: 5,
        },
    ] {
        assert_turn_kept_when_misplaced(&event);
    }
}

#[test]
fn a_record_of_a_turn_not_yet_started_goes_with_damage_after_it() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.1));
    events.push(json!({
        "type": "input_transformations",
        "turn": 2,
        "provider": "anthropic",
        "transformations": [],
    }));
    events.push(turn(2));
    let lines = damaged_at(&events, &[events.len() - 1]);
    let recovered = recovered(&lines);
    assert!(
        !recovered
            .records
            .iter()
            .any(|r| matches!(r.event, LogEvent::InputTransformations { .. }))
    );
    assert_eq!(recovered.fell_back.unwrap().dropped_turns, [2]);
}
