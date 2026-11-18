use super::*;

#[test]
fn later_states_activities_and_outcomes_read_as_unknown() {
    let state: AgentState = serde_json::from_str(r#"{"state":"later"}"#).unwrap();
    assert_eq!(state, AgentState::Unknown);
    for later in [r#"{"type":"later"}"#, r#"{"type":"later","processes":[3]}"#] {
        let activity: Activity = serde_json::from_str(later).unwrap();
        assert_eq!(activity, Activity::Unknown);
    }
    let outcome: IdleOutcome = serde_json::from_str("\"later\"").unwrap();
    assert_eq!(outcome, IdleOutcome::Unknown);
    let reported: ReportOutcome = serde_json::from_str("\"later\"").unwrap();
    assert_eq!(reported, ReportOutcome::Unknown);
    let queued: QueueReason = serde_json::from_str("\"later\"").unwrap();
    assert_eq!(queued, QueueReason::Unknown);
}

#[test]
fn a_waiting_activity_names_its_processes_and_reads_back() {
    let waiting = |processes: &[u32]| Activity::Waiting {
        processes: processes.into(),
    };
    let one = waiting(&[3]).waiting_text();
    assert_eq!(one.as_deref(), Some("waiting on its process 3"));
    let several = waiting(&[4, 7]).waiting_text();
    assert_eq!(several.as_deref(), Some("waiting on its processes 4, 7"));
    assert_eq!(waiting(&[]).waiting_text(), None);
    assert_eq!(Activity::Idle.waiting_text(), None);
    let json = serde_json::to_string(&waiting(&[4, 7])).unwrap();
    assert_eq!(json, r#"{"type":"waiting","processes":[4,7]}"#);
    assert_eq!(
        serde_json::from_str::<Activity>(&json).unwrap(),
        waiting(&[4, 7])
    );
}

#[test]
fn each_state_and_reason_word_is_its_serde_name() {
    let states = [
        AgentState::Queued {
            reason: QueueReason::Stagger,
        },
        AgentState::Running,
        AgentState::Stopping,
        AgentState::Idle { outcome: None },
        AgentState::Held {
            reason: HoldReason::Concurrency,
        },
        AgentState::Ended,
        AgentState::Unknown,
    ];
    for state in states {
        assert_eq!(
            serde_json::to_value(state).unwrap()["state"],
            state.as_str()
        );
    }
    for name in "concurrency stagger endpoint retry unknown".split_whitespace() {
        let reason: QueueReason = serde_json::from_value(name.into()).unwrap();
        assert_eq!(reason.as_str(), name);
    }
    for name in "stopped held_messages retry_offer concurrency unknown".split_whitespace() {
        let reason: HoldReason = serde_json::from_value(name.into()).unwrap();
        assert_eq!(reason.as_str(), name);
    }
    let outcomes = "reported asked waiting no_report invalid_report failed stopped unknown";
    for name in outcomes.split_whitespace() {
        let outcome: IdleOutcome = serde_json::from_value(name.into()).unwrap();
        assert_eq!(outcome.as_str(), name);
    }
}

#[test]
fn default_choices_are_left_out_and_read_back() {
    let choices = SpawnChoices::default();
    assert_eq!(serde_json::to_string(&choices).unwrap(), "{}");
    let back: SpawnChoices = serde_json::from_str("{}").unwrap();
    assert_eq!(back, choices);
}

#[test]
fn the_root_is_shown_as_main() {
    assert_eq!(shown_agent(&AgentPath::root()), "main");
    assert_eq!(shown_agent(&AgentPath::new("/review")), "/review");
}

#[test]
fn a_logged_report_kind_from_before_free_text_reports_is_ignored() {
    let choices: SpawnChoices =
        serde_json::from_str(r#"{"preset":"planner","report":"plan"}"#).unwrap();
    assert_eq!(choices.preset.as_deref(), Some("planner"));
    assert_eq!(choices.instructions, None);
    let request: SpawnRequest =
        serde_json::from_str(r#"{"name":"p","task":"t","report":"plan"}"#).unwrap();
    assert_eq!(request.name, "p");
}
