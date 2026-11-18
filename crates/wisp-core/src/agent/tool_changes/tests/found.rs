use super::*;
use crate::agent::testing::{Caps, Fake, Knobs};
use crate::provider::ModelCapabilities;

fn native(on: &[&str], held: &[&str]) -> Agent {
    let row = ModelCapabilities {
        inline_tools: true,
        deferred_tools: true,
        tools_after_results: true,
        ..ModelCapabilities::default()
    };
    let provider = Fake::with(
        Knobs {
            caps: Some(Caps::Fixed(Some(Box::leak(Box::new(row))))),
            documented: Some(true),
            ..Knobs::default()
        },
        [],
    );
    switchable_on(provider, crate::agent::default_session(), on, held).with_fixed_tools(
        on.iter()
            .chain(held)
            .map(|name| (*name).to_owned())
            .collect(),
    )
}

async fn discover(agent: &mut Agent, name: &str) -> ToolPart {
    agent.tool_switches.discovered.insert(name.into());
    let (sink, _) = mpsc::channel(128);
    agent.found_moment(&sink).await
}

#[tokio::test]
async fn found_tools_use_a_deferred_name_once_without_rebuilding_the_prefix() {
    let mut agent = native(&["echo"], &["sleep"]);
    agent.history.push(Message::user_text("search results"));
    let sent = agent.sent.clone();
    let deferred = agent.deferred.clone();
    let part = discover(&mut agent, "sleep").await;
    assert_eq!(part.blocks.first(), Some(&by_name("sleep")));
    assert!(agent.toolbox.get("sleep").is_some());
    agent.history.push(system(part.blocks));
    assert!(discover(&mut agent, "sleep").await.blocks.is_empty());
    assert_eq!(agent.sent, sent);
    assert_eq!(agent.deferred, deferred);
    assert!(!agent.rebuild_due());
}

#[tokio::test]
async fn an_unlisted_pair_keeps_found_tools_held_and_callable_only_through_use() {
    let mut agent = switchable_with(&["echo"], &["sleep"]);
    agent.toolbox.set_enabled("sleep", true);
    agent.note_deferred();
    let sent = agent.sent.clone();
    assert!(discover(&mut agent, "sleep").await.blocks.is_empty());
    assert!(agent.toolbox.get("sleep").is_none());
    assert_eq!(agent.sent, sent);
    let error = agent
        .prepare_call("sleep", &json!({}), &[])
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("run it with use"));
    assert!(
        agent
            .prepare_runnable("sleep", &json!({"ms": 0}))
            .await
            .is_ok()
    );
}

#[test]
fn definitions_cut_only_descriptions_at_utf8_boundaries_and_refuse_oversized_schemas() {
    let mut tool = spec("long");
    tool.description = "🦀\"\\".repeat(8_000);
    let limited = super::super::found::definition(&tool).unwrap();
    assert!(serde_json::to_vec(&limited).unwrap().len() <= 16 * 1024);
    assert!(limited.description.ends_with("definition limit]"));
    assert_eq!(limited.input_schema, tool.input_schema);
    tool.input_schema = json!({"enum": ["x".repeat(20_000)]});
    assert!(super::super::found::definition(&tool).is_none());
}

#[tokio::test]
async fn a_found_tool_is_restated_after_compaction_once_and_rewind_forgets_it() {
    let mut agent = native(&["echo"], &["sleep"]);
    agent.history.push(Message::user_text("search results"));
    let part = discover(&mut agent, "sleep").await;
    agent.history.push(system(part.blocks));
    agent.history.clear();
    let restated = agent.restated_tools();
    assert_eq!(restated.first(), Some(&by_name("sleep")));
    agent.history.push(Message::user_text("continue"));
    agent.history.push(system(restated));
    assert!(agent.restated_tools().is_empty());
    agent.history.truncate(1);
    agent.rewound_tools(1);
    assert!(agent.tool_switches.found.is_empty());
    assert!(!agent.desired_tools().unwrap().contains("sleep"));
    assert!(agent.toolbox.held_tool("sleep").is_some());
    assert!(agent.restated_tools().is_empty());
}

#[tokio::test]
async fn inline_rejection_then_a_turn_start_never_rebuilds_and_use_still_runs() {
    let mut agent = native(&["echo"], &["sleep"]);
    agent.history.push(Message::user_text("search results"));
    let part = discover(&mut agent, "sleep").await;
    agent.history.push(system(part.blocks));
    agent.settings.carried(
        (
            agent.endpoint_name().into(),
            agent.session.model.clone(),
            Feature::InlineTools,
        ),
        PairState::FallenBack,
    );
    agent.inline_fell_back();
    assert!(!agent.rebuild_due());
    let sent = agent.sent.clone();
    let (sink, _) = mpsc::channel(128);
    agent.tool_moment(&sink).await;
    assert_eq!(agent.sent, sent);
    assert!(!agent.rebuild_due());
    let error = agent
        .prepare_call("sleep", &json!({}), &[])
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("run it with use"));
    assert!(
        agent
            .prepare_runnable("sleep", &json!({"ms": 0}))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn resume_restores_both_committed_and_not_yet_accepted_found_tools() {
    for (accepted, masked) in [(false, false), (true, false), (false, true)] {
        let mut agent = native(&["echo"], &["sleep"]);
        agent.history.push(Message::user_text("search results"));
        if accepted {
            agent.history.push(system(vec![by_name("sleep")]));
        }
        agent.tool_switches.logged_on = Some(["echo".into(), "sleep".into()].into());
        let (opening, change) = discovery_records();
        let sources = if accepted { vec![0, 2] } else { vec![0] };
        agent.restore_found(
            [(0, &opening, masked), (1, &change, masked)].into_iter(),
            &sources,
        );
        agent.settle_resumed_tools();
        assert_eq!(agent.tool_switches.found.contains_key("sleep"), !masked);
        assert_eq!(agent.toolbox.get("sleep").is_some(), !masked);
        assert!(
            !agent
                .tool_switches
                .started
                .as_ref()
                .unwrap()
                .contains("sleep")
        );
        assert_eq!(agent.restated_tools().is_empty(), accepted || masked);
        agent.rewound_tools(1);
        assert!(agent.tool_switches.found.is_empty());
    }
}

#[test]
fn tool_messages_follow_results_but_never_an_assistant_or_an_unanswered_call() {
    let agent = native(&["echo"], &["sleep"]);
    let tool = system(vec![by_name("sleep")]);
    let assistant = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "search".into(),
            name: "echo".into(),
            input: json!({}),
        }],
    };
    let results = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "search".into(),
            content: vec![],
            is_error: false,
        }],
    };
    let opening = Message::user_text("find sleep");
    assert!(
        crate::agent::check_layout(
            &[opening.clone(), assistant.clone(), results, tool.clone()],
            &[],
            &Default::default(),
            agent.provider().as_ref()
        )
        .is_ok()
    );
    assert!(
        crate::agent::check_layout(
            &[opening, assistant, tool],
            &[],
            &Default::default(),
            agent.provider().as_ref()
        )
        .is_err()
    );
}

fn discovery_records() -> (LogEvent, LogEvent) {
    let mut opening = crate::test_guard::protocol_fixture("log_turn_started").event;
    if let LogEvent::TurnStarted { turn, .. } = &mut opening {
        *turn = 1;
    }
    let change = LogEvent::ToolsChanged {
        turn: 1,
        via: ToolChangeVia::Inline,
        added: vec!["sleep".into()],
        removed: Vec::new(),
        copy: Box::default(),
    };
    (opening, change)
}

#[test]
fn a_turn_start_addition_is_not_a_discovery_and_remains_loaded_on_resume() {
    let mut agent = native(&["echo"], &["sleep"]);
    agent.history.push(Message::user_text("turn opening"));
    agent.history.push(system(vec![by_name("sleep")]));
    agent.tool_switches.logged_on = Some(["echo".into(), "sleep".into()].into());
    let (opening, change) = discovery_records();
    let mut next = opening.clone();
    if let LogEvent::TurnStarted { turn, .. } = &mut next {
        *turn = 2;
    }
    let mut ordinary = change.clone();
    if let LogEvent::ToolsChanged { turn, .. } = &mut ordinary {
        *turn = 2;
    }
    agent.restore_found(
        [
            (0, &opening, true),
            (1, &change, true),
            (2, &ordinary, false),
            (3, &next, false),
        ]
        .into_iter(),
        &[3, 3],
    );
    agent.settle_resumed_tools();
    assert!(agent.tool_switches.found.is_empty());
    assert!(
        agent
            .tool_switches
            .started
            .as_ref()
            .unwrap()
            .contains("sleep")
    );
    assert!(agent.desired_tools().unwrap().contains("sleep"));
    assert!(agent.toolbox.get("sleep").is_some());
}

#[tokio::test]
async fn a_later_discovery_cannot_reenable_a_switched_off_found_tool() {
    let mut agent = native(&["echo"], &["sleep", "confirm"]);
    agent.history.push(Message::user_text("search results"));
    let part = discover(&mut agent, "sleep").await;
    agent.history.push(system(part.blocks));
    agent.tool_switches.fixed.as_mut().unwrap().remove("sleep");
    agent.history.push(Message::user_text("switch sleep off"));
    let (sink, _) = mpsc::channel(128);
    let off = agent.tool_moment(&sink).await;
    agent.history.push(system(off.blocks));
    assert!(!agent.toolbox.is_enabled("sleep"));
    agent.history.push(Message::user_text("find confirm"));
    let _ = discover(&mut agent, "confirm").await;
    assert!(
        !agent.toolbox.is_enabled("sleep"),
        "search silently reenabled sleep"
    );
}

#[tokio::test]
async fn compaction_with_a_pending_discovery_restates_prior_found_tools() {
    let mut agent = native(&["echo"], &["sleep", "confirm"]);
    agent.history.push(Message::user_text("search results"));
    let part = discover(&mut agent, "sleep").await;
    agent.history.push(system(part.blocks));
    agent.history.push(Message::user_text("find confirm"));
    let new = discover(&mut agent, "confirm").await;
    assert_eq!(new.blocks, vec![by_name("confirm")]);
    let mut pending = crate::agent::Pending {
        tools: new.blocks,
        late_tools: true,
        found_tools: true,
        ..Default::default()
    };
    agent.history.clear();
    agent.recompute_tools(&mut pending);
    assert!(
        pending.tools.contains(&by_name("sleep")),
        "compaction lost the earlier found tool"
    );
}

#[test]
fn ordinary_late_tools_wait_for_user_input_but_found_tools_are_a_payload() {
    let mut pending = crate::agent::Pending {
        tools: vec![by_name("sleep")],
        late_tools: true,
        ..Default::default()
    };
    assert!(!pending.has_payload());
    pending.found_tools = true;
    assert!(pending.has_payload());
}
