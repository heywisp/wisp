use super::*;
use crate::test_guard::protocol_fixtures;

fn fixture(path: impl AsRef<Path>) -> Vec<LogRecord> {
    parse_jsonl(fs::read(path).unwrap()).unwrap()
}

#[test]
fn v14_tool_result_tail() {
    let records = fixture(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/logs/v14-tool-result-tail.jsonl"),
    );
    insta::assert_debug_snapshot!(LoggedSession::read(&records));
}

#[test]
fn log_version_4() {
    let records = fixture(protocol_fixtures().join("log_version_4.jsonl"));
    insta::assert_debug_snapshot!(LoggedSession::read(&records));
}

#[test]
fn log_version_6() {
    let records = fixture(protocol_fixtures().join("log_version_6.jsonl"));
    insta::assert_debug_snapshot!(LoggedSession::read(&records));
}

#[test]
fn without_session_started() {
    let records = fixture(protocol_fixtures().join("log_version_6.jsonl"));
    let headless = &records[1..];
    insta::assert_debug_snapshot!((
        LoggedSession::read(headless),
        LoggedSession::costs(headless)
    ));
}

#[test]
fn an_open_turn_with_a_pending_switch_and_a_later_option() {
    let mut events = vec![started()];
    events.extend(full_turn(1, 0.25));
    let unsplit = json!({
        "type": "resume_fell_back", "kept_through": 3, "dropped_through": 5, "last_turn": 1,
        "turns_seen": 1, "processes_seen": 2, "spent_usd": 0.5, "damage": [],
    });
    let mut split = unsplit.clone();
    split["plan_usd"] = json!(0.375);
    split["billing"] = json!("anthropic_plan");
    events.extend([unsplit, split]);
    let mut plan_retry = retrying(2, 0, 0.125);
    plan_retry["usage"]["billing"] = json!("anthropic_plan");
    events.extend([
        turn(2),
        said(2, "user"),
        said(2, "assistant"),
        step(2, 1, 2_000, true),
        plan_retry,
        change(
            "claude-sonnet-5",
            "medium",
            "other",
            vec![
                entry("endpoint", setting_text("other"), false),
                entry("model", setting_text("claude-sonnet-5"), false),
                entry("cache_ttl", setting_text("5m"), false),
            ],
        ),
    ]);
    insta::assert_debug_snapshot!(LoggedSession::read(&log(&events)));
}
