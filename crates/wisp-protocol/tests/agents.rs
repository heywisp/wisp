#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use serde_json::json;
use support::{fixture_text, golden, record_late};
use wisp_protocol::agents::Preset;
use wisp_protocol::log::{LOG_SCHEMA_VERSION, LogEvent, LogRecord};
use wisp_protocol::{
    Activity, AgentEntry, AgentEvent, AgentPath, AgentState, ClientCommand, ContextFill, Cost,
    Effort, HoldReason, IdleOutcome, ParentLink, PresetChange, QueueReason, ReportOutcome,
    RequestId, SpawnChoices, SpawnRequest, TodoItem, TodoProgress, TodoStatus, TurnOutcome,
    TurnStats,
};

fn spawn() -> SpawnRequest {
    SpawnRequest {
        name: "planner".into(),
        task: "Plan the change in docs/plans/x.md.".into(),
        todos: vec!["Read the report".into(), "Write the plan".into()],
        cwd: Some("~/Projects/Wisp-worktrees/review".into()),
        preset: Some("reviewer".into()),
        model: Some("claude-opus-4-1".into()),
        effort: Some(Effort::High),
        tools_off: vec!["show_diff".into()],
        spawn: Some(false),
        keep_warm: false,
        endpoint: None,
    }
}

fn choices() -> SpawnChoices {
    SpawnChoices {
        preset: Some("reviewer".into()),
        tools_off: vec!["show_diff".into()],
        spawn: false,
        instructions: Some("~/roles/reviewer/AGENTS.md".into()),
        preset_values: None,
        keep_warm: false,
        choices: Vec::new(),
    }
}

fn entry() -> AgentEntry {
    AgentEntry {
        plan_usd: None,
        billing: Default::default(),
        path: AgentPath::new("/review/planner"),
        name: "planner".into(),
        session_id: String::new(),
        parent: Some(AgentPath::new("/review")),
        preset: Some("reviewer".into()),
        model: "claude-opus-4-1".into(),
        effort: Effort::High,
        depth: 2,
        state: AgentState::Idle {
            outcome: Some(IdleOutcome::Reported),
        },
        activity: Activity::Idle,
        spent_usd: 0.84,
        lower_bound: false,
        accounts: Default::default(),
        checklist: None,
        last_report: Some("Plan written to docs/plans/x.md".into()),
        cwd: Some("~/Projects/Wisp-worktrees/review".into()),
        running_ms: None,
        context: None,
        keep_warm: None,
        endpoint: None,
        provider: None,
        priced: true,
        preset_change: None,
        since_ms: None,
    }
}

#[test]
fn the_agent_commands_match_their_fixtures() {
    golden(
        "command_spawn_agent",
        &ClientCommand::SpawnAgent {
            request: RequestId(7),
            spawn: spawn(),
        },
    );
    golden(
        "command_spawn_agent_minimal",
        &ClientCommand::SpawnAgent {
            request: RequestId(8),
            spawn: SpawnRequest {
                name: "docs".into(),
                task: "Update the docs.".into(),
                ..SpawnRequest::default()
            },
        },
    );
    golden(
        "command_child",
        &ClientCommand::Child {
            agent: AgentPath::new("/review"),
            command: Box::new(ClientCommand::Cancel),
        },
    );
    golden(
        "command_list_agents",
        &ClientCommand::ListAgents {
            request: RequestId(9),
        },
    );
    golden(
        "command_end_agent",
        &ClientCommand::EndAgent {
            agent: AgentPath::new("/review/planner"),
        },
    );
    golden(
        "command_rename_agent",
        &ClientCommand::RenameAgent {
            request: RequestId(11),
            agent: AgentPath::new("/review/planner"),
            name: "fixer".into(),
        },
    );
    golden(
        "command_watch_agent",
        &ClientCommand::WatchAgent {
            agent: AgentPath::new("/review"),
            live: true,
        },
    );
}

#[test]
fn the_agent_events_match_their_fixtures() {
    golden(
        "event_agent_spawned",
        &AgentEvent::AgentSpawned {
            request: Some(RequestId(7)),
            agent: AgentEntry {
                session_id: "s-child".into(),
                state: AgentState::Running,
                activity: Activity::Streaming,
                spent_usd: 0.0,
                last_report: None,
                ..entry()
            },
            session_id: "s-child".into(),
            config: choices(),
        },
    );
    golden(
        "event_agent_state_changed",
        &AgentEvent::AgentStateChanged {
            plan_usd: None,
            billing: Default::default(),
            agent: AgentPath::new("/review"),
            state: AgentState::Held {
                reason: HoldReason::Stopped,
            },
            activity: Activity::Held {
                reason: HoldReason::Stopped,
            },
            spent_usd: 1.25,
            lower_bound: true,
            accounts: Default::default(),
            since_ms: None,
        },
    );
    golden(
        "event_agent_state_queued",
        &AgentEvent::AgentStateChanged {
            plan_usd: None,
            billing: Default::default(),
            agent: AgentPath::new("/docs"),
            state: AgentState::Queued {
                reason: QueueReason::Concurrency,
            },
            activity: Activity::Tool {
                name: "bash".into(),
            },
            spent_usd: 0.0,
            lower_bound: false,
            accounts: Default::default(),
            since_ms: None,
        },
    );
    golden(
        "event_agent_renamed",
        &AgentEvent::AgentRenamed {
            request: Some(RequestId(11)),
            agent: AgentPath::new("/review/planner"),
            to: AgentPath::new("/review/fixer"),
        },
    );
    golden(
        "event_agent_reported",
        &AgentEvent::AgentReported {
            agent: AgentPath::new("/review/planner"),
            turn: 1,
            outcome: ReportOutcome::Reported,
            report: Some(json!("Planned: docs/plans/x.md\n\nTwo decisions are open.")),
            cost: Some(0.84),
            duration_ms: 372_000,
        },
    );
    golden(
        "event_agent_wrapped",
        &AgentEvent::Agent {
            agent: AgentPath::new("/review"),
            event: Box::new(AgentEvent::TextDelta {
                index: 0,
                text: "Reading".into(),
            }),
        },
    );
    golden(
        "event_agents_snapshot",
        &AgentEvent::AgentsSnapshot {
            request: Some(RequestId(9)),
            agents: vec![entry()],
        },
    );
}

#[test]
fn session_ended_names_the_turn_the_exit_stopped() {
    golden(
        "log_session_ended_interrupted",
        &record_late(
            57,
            LogEvent::SessionEnded {
                turns: 3,
                interrupted: Some(3),
            },
        ),
    );
    let older = r#"{"schema_version":28,"seq":4,"at":"2026-10-21T19:16:40.007Z","type":"session_ended","turns":1}"#;
    let read: LogRecord = serde_json::from_str(older).unwrap();
    assert_eq!(
        read.event,
        LogEvent::SessionEnded {
            turns: 1,
            interrupted: None
        }
    );
    assert_eq!(serde_json::to_string(&read).unwrap(), older);
}

#[test]
fn the_agent_records_match_their_fixtures() {
    golden(
        "log_agent_spawned",
        &record_late(
            12,
            LogEvent::AgentSpawned {
                turn: Some(3),
                agent: AgentPath::new("/review/planner"),
                session_id: "s-child".into(),
                depth: 2,
                model: "claude-opus-4-1".into(),
                effort: Effort::High,
                cwd: Some("~/Projects/Wisp-worktrees/review".into()),
                config: choices(),
                endpoint: None,
                provider: None,
            },
        ),
    );
    golden(
        "log_agent_ended",
        &record_late(
            41,
            LogEvent::AgentEnded {
                billing: wisp_protocol::Billing::Metered,
                plan_usd: None,
                turn: Some(4),
                agent: AgentPath::new("/review/planner"),
                spent_usd: 0.84,
            },
        ),
    );
    golden(
        "log_agent_renamed",
        &record_late(
            42,
            LogEvent::AgentRenamed {
                turn: Some(4),
                agent: AgentPath::new("/review/planner"),
                to: AgentPath::new("/review/fixer"),
            },
        ),
    );
    golden(
        "log_agent_reported",
        &record_late(
            30,
            LogEvent::AgentReported {
                turn: 1,
                agent: AgentPath::new("/review/planner"),
                outcome: ReportOutcome::Reported,
                report: Some(json!("Planned: docs/plans/x.md")),
                duration_ms: 1_500,
            },
        ),
    );
}

#[test]
fn a_childs_session_start_links_its_parent() {
    let started: LogEvent = serde_json::from_value(json!({
        "type": "session_started",
        "session_id": "s-child",
        "wisp_version": "0.1.0",
        "model": "claude-opus-4-1",
        "effort": "high",
        "cache_ttl": "one_hour",
        "system_prompt": null,
        "system_prompt_sha256": null,
        "endpoint": "https://api.anthropic.com",
        "credential_source": "environment",
        "parent": {
            "session_id": "s-root",
            "agent": "/review/planner",
            "depth": 2,
            "config": {"preset": "reviewer", "spawn": false, "instructions": "~/roles/reviewer/AGENTS.md"}
        }
    }))
    .unwrap();
    let LogEvent::SessionStarted { parent, .. } = &started else {
        panic!("{started:?}");
    };
    assert_eq!(
        parent.as_deref(),
        Some(&ParentLink {
            session_id: "s-root".into(),
            agent: AgentPath::new("/review/planner"),
            depth: 2,
            config: SpawnChoices {
                preset: Some("reviewer".into()),
                tools_off: Vec::new(),
                spawn: false,
                instructions: Some("~/roles/reviewer/AGENTS.md".into()),
                preset_values: None,
                keep_warm: false,
                choices: Vec::new(),
            },
            todos: Vec::new(),
        })
    );
    golden("log_session_started_child", &record_late(1, started));
}

#[test]
fn the_main_agents_cost_carries_its_childrens_total() {
    let cost = Cost {
        turn_plan_usd: None,
        session_plan_usd: None,
        agents_plan_usd: None,
        agents_billing: Default::default(),
        turn_billing: Default::default(),
        session_billing: Default::default(),
        turn_usd: 0.12,
        session_usd: 0.5,
        turn_lower_bound: false,
        session_lower_bound: false,
        agents_usd: Some(2.25),
        agents_lower_bound: true,
    };
    golden(
        "event_turn_ended_agents_cost",
        &AgentEvent::TurnEnded {
            outcome: TurnOutcome::Cancelled {
                usage: None,
                cost: Some(cost),
                committed_steps: 1,
            },
            stats: TurnStats::default(),
        },
    );
    let solo = serde_json::to_value(Cost {
        agents_usd: None,
        agents_lower_bound: false,
        ..cost
    })
    .unwrap();
    assert_eq!(
        solo,
        json!({
            "turn_usd": 0.12,
            "session_usd": 0.5,
            "turn_lower_bound": false,
            "session_lower_bound": false,
        })
    );
    let older: Cost = serde_json::from_value(solo).unwrap();
    assert_eq!(older.agents_usd, None);
    assert!(!older.agents_lower_bound);
}

#[test]
fn an_entry_carries_its_checklist_progress_and_a_child_its_starting_checklist() {
    golden(
        "event_agents_snapshot_checklist",
        &AgentEvent::AgentsSnapshot {
            request: Some(RequestId(4)),
            agents: vec![AgentEntry {
                checklist: Some(TodoProgress {
                    completed: 2,
                    total: 5,
                }),
                ..entry()
            }],
        },
    );
    let without = serde_json::to_value(entry()).unwrap();
    assert!(without.get("checklist").is_none(), "{without}");
    assert!(without.get("running_ms").is_none(), "{without}");
    golden(
        "event_agents_snapshot_running",
        &AgentEvent::AgentsSnapshot {
            request: Some(RequestId(5)),
            agents: vec![AgentEntry {
                running_ms: Some(372_000),
                ..entry()
            }],
        },
    );
    let link = ParentLink {
        session_id: "s-root".into(),
        agent: AgentPath::new("/review"),
        depth: 1,
        config: SpawnChoices::default(),
        todos: vec![TodoItem {
            text: "Read the plan".into(),
            status: TodoStatus::Pending,
            section: None,
        }],
    };
    golden("parent_link_todos", &link);
    let older: ParentLink = serde_json::from_value(json!({
        "session_id": "s-root",
        "agent": "/review",
        "depth": 1,
        "config": {"report": "summary"}
    }))
    .unwrap();
    assert!(older.todos.is_empty());
    let shown = serde_json::to_value(&older).unwrap();
    assert!(shown.get("todos").is_none(), "{shown}");
}

#[test]
fn a_spawn_from_a_preset_logs_the_presets_values_and_older_records_read_without_them() {
    golden(
        "log_agent_spawned_preset_values",
        &record_late(
            12,
            LogEvent::AgentSpawned {
                turn: Some(3),
                agent: AgentPath::new("/review/planner"),
                session_id: "s-child".into(),
                depth: 2,
                model: "claude-opus-4-1".into(),
                effort: Effort::Medium,
                cwd: None,
                config: SpawnChoices {
                    preset_values: Some(Box::new(Preset {
                        name: "reviewer".into(),
                        description: "Reviews a change".into(),
                        model: None,
                        effort: Some(Effort::Low),
                        preload: None,
                        tools_off: vec!["show_diff".into()],
                        spawn: Some(false),
                        instructions: Some("~/roles/reviewer".into()),
                        endpoint: None,
                        fallback: Vec::new(),
                        worker: None,
                    })),
                    ..choices()
                },
                endpoint: None,
                provider: None,
            },
        ),
    );
    let old_values = fixture_text("log_agent_spawned_preset_values.json");
    let mut read: LogRecord = serde_json::from_str(&old_values).unwrap();
    let LogEvent::AgentSpawned { config, .. } = &mut read.event else {
        panic!("not a spawn")
    };
    let preset = config.preset_values.as_mut().unwrap();
    assert!(preset.fallback.is_empty());
    preset.fallback.push(wisp_protocol::agents::Choice {
        endpoint: "other".into(),
        model: None,
        effort: Some(Effort::High),
    });
    assert_eq!(
        serde_json::from_str::<LogRecord>(&serde_json::to_string(&read).unwrap()).unwrap(),
        read
    );
    let older = fixture_text("log_agent_spawned.json");
    let read: LogRecord = serde_json::from_str(&older).unwrap();
    let LogEvent::AgentSpawned { config, .. } = read.event else {
        panic!("{:?}", read.event)
    };
    assert_eq!(config.preset_values, None);
}

fn old(name: &str) -> serde_json::Value {
    let text = fixture_text(&format!("{name}_v29.json"));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn the_version_29_agent_records_with_typed_reports_still_read() {
    let command: ClientCommand = serde_json::from_value(old("command_spawn_agent")).unwrap();
    let ClientCommand::SpawnAgent { spawn: read, .. } = command else {
        panic!("{command:?}")
    };
    assert_eq!(read, spawn());
    let event: AgentEvent = serde_json::from_value(old("event_agent_reported")).unwrap();
    let AgentEvent::AgentReported { report, .. } = event else {
        panic!("{event:?}")
    };
    assert_eq!(report.unwrap()["summary"], json!("Planned."));
    let event: AgentEvent = serde_json::from_value(old("event_agent_spawned")).unwrap();
    let AgentEvent::AgentSpawned { config, agent, .. } = event else {
        panic!("{event:?}")
    };
    assert_eq!(config.instructions, None);
    assert_eq!(config.preset.as_deref(), Some("reviewer"));
    assert_eq!(agent.session_id, "");
    for name in [
        "log_agent_spawned",
        "log_agent_spawned_preset_values",
        "log_agent_reported",
        "log_session_started_child",
    ] {
        let line = old(name);
        assert_eq!(line["schema_version"], json!(29), "{name}");
        let read: LogRecord = serde_json::from_value(line).unwrap();
        assert_eq!(read.schema_version, 29, "{name}");
        let shown = serde_json::to_value(&read).unwrap();
        assert!(
            !shown.to_string().contains("\"report\":\"plan\""),
            "{name}: {shown}"
        );
    }
    let link: ParentLink = serde_json::from_value(old("parent_link_todos")).unwrap();
    assert_eq!(link.config, SpawnChoices::default());
}

const READER_29: u16 = 29;

const REPORTS_VERSION: u16 = 30;

fn summary_at_29() -> String {
    "summary".to_owned()
}

#[derive(serde::Deserialize)]
struct ChoicesAt29 {
    #[serde(default = "summary_at_29")]
    report: String,
}

#[derive(serde::Deserialize)]
struct SpawnedAt29 {
    schema_version: u16,
    config: ChoicesAt29,
}

#[test]
fn a_version_29_reader_sees_free_text_reports_as_newer() {
    let stored = fixture_text("log_agent_reported.json");
    let line: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(line["schema_version"], json!(LOG_SCHEMA_VERSION));
    const { assert!(LOG_SCHEMA_VERSION >= REPORTS_VERSION) };
    assert_eq!(REPORTS_VERSION, READER_29 + 1);
    assert!(line["report"].is_string());
    let spawned = fixture_text("log_agent_spawned.json");
    let seen: SpawnedAt29 = serde_json::from_str(&spawned).unwrap();
    assert!(seen.schema_version > READER_29);
    assert_eq!(seen.config.report, "summary");
}

#[test]
fn an_entry_carries_its_session_id_and_older_entries_read_without_one() {
    golden(
        "event_agents_snapshot_session",
        &AgentEvent::AgentsSnapshot {
            request: Some(RequestId(6)),
            agents: vec![AgentEntry {
                session_id: "0199a3c4-5e6f-7a8b-9c0d-1e2f3a4b5c6d".into(),
                ..entry()
            }],
        },
    );
    let mut older = serde_json::to_value(AgentEntry {
        session_id: "s-child".into(),
        ..entry()
    })
    .unwrap();
    older.as_object_mut().unwrap().remove("session_id");
    let read: AgentEntry = serde_json::from_value(older).unwrap();
    assert_eq!(read.session_id, "");
    assert!(
        serde_json::to_value(&read)
            .unwrap()
            .get("session_id")
            .is_none()
    );
}

#[test]
fn a_stopping_state_and_a_context_fill_have_fixtures_and_older_entries_read_without_them() {
    golden(
        "event_agent_state_changed_stopping",
        &AgentEvent::AgentStateChanged {
            plan_usd: None,
            billing: Default::default(),
            agent: AgentPath::new("/review/coder"),
            state: AgentState::Stopping,
            activity: Activity::Tool {
                name: "bash".into(),
            },
            spent_usd: 0.42,
            lower_bound: false,
            accounts: Default::default(),
            since_ms: None,
        },
    );
    let fill = ContextFill {
        used: 312_000,
        window: Some(1_000_000),
        limit: Some(400_000),
        estimated: false,
        auto_compact: true,
    };
    golden(
        "event_agents_snapshot_context",
        &AgentEvent::AgentsSnapshot {
            request: Some(RequestId(5)),
            agents: vec![
                AgentEntry {
                    context: Some(fill),
                    ..entry()
                },
                AgentEntry {
                    path: AgentPath::new("/docs"),
                    name: "docs".into(),
                    parent: None,
                    context: Some(ContextFill {
                        used: 48_000,
                        window: None,
                        limit: None,
                        estimated: true,
                        auto_compact: false,
                    }),
                    ..entry()
                },
            ],
        },
    );
    let state: AgentState = serde_json::from_str(r#"{"state":"stopping"}"#).unwrap();
    assert_eq!(state, AgentState::Stopping);
    assert!(state.is_running() && state.is_busy());
    let older = serde_json::to_value(entry()).unwrap();
    assert!(older.get("context").is_none());
    let read: AgentEntry = serde_json::from_value(older).unwrap();
    assert_eq!(read.context, None);
    let filled = serde_json::to_value(AgentEntry {
        context: Some(fill),
        ..entry()
    })
    .unwrap();
    assert_eq!(
        filled["context"],
        json!({"used": 312_000, "window": 1_000_000, "limit": 400_000, "auto_compact": true})
    );
    for old in [
        json!({"used": 312_000, "window": 1_000_000, "limit": 400_000}),
        json!({"used": 312_000, "window": 1_000_000, "limit": 400_000, "estimated": false}),
    ] {
        let old: ContextFill = serde_json::from_value(old).unwrap();
        assert_eq!(
            old,
            ContextFill {
                auto_compact: false,
                ..fill
            }
        );
    }
}

#[test]
fn a_keep_warm_mark_is_written_only_when_set_and_read_back() {
    let marked = SpawnRequest {
        keep_warm: true,
        ..spawn()
    };
    let json = serde_json::to_value(&marked).unwrap();
    assert_eq!(json["keep_warm"], serde_json::json!(true));
    assert_eq!(
        serde_json::from_value::<SpawnRequest>(json).unwrap(),
        marked
    );
    assert!(
        serde_json::to_value(spawn())
            .unwrap()
            .get("keep_warm")
            .is_none()
    );
    let config = SpawnChoices {
        keep_warm: true,
        ..choices()
    };
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["keep_warm"], serde_json::json!(true));
    assert_eq!(
        serde_json::from_value::<SpawnChoices>(json).unwrap(),
        config
    );
    assert!(
        serde_json::to_value(choices())
            .unwrap()
            .get("keep_warm")
            .is_none()
    );
}

#[test]
fn a_spawns_choices_are_written_only_when_it_has_some_and_read_back() {
    let config = SpawnChoices {
        choices: vec![
            wisp_protocol::agents::Choice {
                endpoint: "first".into(),
                model: Some("claude-opus-5-5".into()),
                effort: Some(Effort::High),
            },
            wisp_protocol::agents::Choice {
                endpoint: "second".into(),
                model: None,
                effort: None,
            },
        ],
        ..choices()
    };
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(
        json["choices"],
        serde_json::json!([
            {"endpoint": "first", "model": "claude-opus-5-5", "effort": "high"},
            {"endpoint": "second"}
        ])
    );
    assert_eq!(
        serde_json::from_value::<SpawnChoices>(json).unwrap(),
        config
    );
    let older = serde_json::to_value(choices()).unwrap();
    assert!(older.get("choices").is_none());
    assert_eq!(
        serde_json::from_value::<SpawnChoices>(older).unwrap(),
        choices()
    );
}

#[test]
fn a_keep_warm_fact_has_fixtures_and_older_entries_read_without_it() {
    use wisp_protocol::KeepWarm;
    let paused = KeepWarm {
        refreshes: 1,
        limit: 8,
        paused: true,
    };
    golden(
        "event_keep_warm_changed",
        &AgentEvent::KeepWarmChanged {
            agent: AgentPath::new("/review/planner"),
            keep_warm: KeepWarm {
                refreshes: 2,
                limit: 8,
                paused: false,
            },
        },
    );
    golden(
        "event_agents_snapshot_keep_warm",
        &AgentEvent::AgentsSnapshot {
            request: Some(RequestId(6)),
            agents: vec![AgentEntry {
                keep_warm: Some(paused),
                ..entry()
            }],
        },
    );
    let older = serde_json::to_value(entry()).unwrap();
    assert!(older.get("keep_warm").is_none());
    let read: AgentEntry = serde_json::from_value(older).unwrap();
    assert_eq!(read.keep_warm, None);
    let unpaused: KeepWarm = serde_json::from_value(json!({"refreshes": 0, "limit": 2})).unwrap();
    assert!(!unpaused.paused);
    assert!(
        serde_json::to_value(unpaused)
            .unwrap()
            .get("paused")
            .is_none()
    );
}

#[test]
fn an_endpoint_is_logged_only_when_known_and_older_records_read_without_one() {
    let older = fixture_text("log_agent_spawned.json");
    let read: LogRecord = serde_json::from_str(&older).unwrap();
    let LogEvent::AgentSpawned {
        endpoint,
        provider,
        config,
        ..
    } = read.event
    else {
        panic!("{:?}", read.event)
    };
    assert_eq!((endpoint, provider), (None, None));
    assert_eq!(config.preset_values, None);
    let older = serde_json::to_value(entry()).unwrap();
    for key in ["endpoint", "provider", "priced"] {
        assert!(older.get(key).is_none(), "{key}");
    }
    let read: AgentEntry = serde_json::from_value(older).unwrap();
    assert_eq!(
        (read.endpoint, read.provider, read.priced),
        (None, None, true)
    );
    let preset: Preset =
        serde_json::from_value(json!({"name": "reviewer", "description": "Reviews"})).unwrap();
    assert_eq!(preset.endpoint, None);
    let request: SpawnRequest =
        serde_json::from_value(json!({"name": "r", "task": "Review."})).unwrap();
    assert_eq!(request.endpoint, None);
    let placed = AgentEntry {
        endpoint: Some("gpt".into()),
        provider: Some("anthropic".into()),
        priced: false,
        ..entry()
    };
    let json = serde_json::to_value(&placed).unwrap();
    assert_eq!(json["endpoint"], json!("gpt"));
    assert_eq!(json["provider"], json!("anthropic"));
    assert_eq!(json["priced"], json!(false));
    assert_eq!(serde_json::from_value::<AgentEntry>(json).unwrap(), placed);
    let spawned = LogEvent::AgentSpawned {
        turn: Some(3),
        agent: AgentPath::new("/review/planner"),
        session_id: "s-child".into(),
        depth: 2,
        model: "claude-opus-4-1".into(),
        effort: Effort::High,
        cwd: None,
        config: choices(),
        endpoint: Some("gpt".into()),
        provider: Some("future".into()),
    };
    let json = serde_json::to_value(&spawned).unwrap();
    assert_eq!(json["endpoint"], json!("gpt"));
    assert_eq!(json["provider"], json!("future"));
    assert_eq!(serde_json::from_value::<LogEvent>(json).unwrap(), spawned);
}

#[test]
fn agent_billing_splits_have_goldens() {
    let spent = |usd, plan_usd| wisp_protocol::AccountSpend {
        usd,
        plan_usd,
        lower_bound: false,
    };
    let accounts = std::collections::BTreeMap::from([
        ("claude-main".to_owned(), spent(4.1, Some(4.1))),
        ("old".to_owned(), spent(12.3, None)),
    ]);
    let agent = AgentEntry {
        spent_usd: 16.4,
        plan_usd: Some(4.1),
        billing: wisp_protocol::Billing::Mixed,
        accounts: accounts.clone(),
        ..entry()
    };
    golden("agent_entry_mixed_billing_split", &agent);
    let json = serde_json::to_value(&agent).unwrap();
    assert!(json["accounts"]["old"].get("plan_usd").is_none(), "{json}");
    let mut older = json;
    older.as_object_mut().unwrap().remove("accounts");
    assert!(
        serde_json::from_value::<AgentEntry>(older)
            .unwrap()
            .accounts
            .is_empty()
    );
    golden(
        "event_agent_state_mixed_billing_split",
        &AgentEvent::AgentStateChanged {
            agent: agent.path.clone(),
            state: agent.state,
            activity: agent.activity.clone(),
            spent_usd: agent.spent_usd,
            billing: agent.billing,
            plan_usd: agent.plan_usd,
            lower_bound: true,
            accounts: accounts.clone(),
            since_ms: None,
        },
    );
    let total = wisp_protocol::AccountSpend::of(16.4, Some(4.1), agent.billing, true);
    golden(
        "event_spend_changed",
        &AgentEvent::SpendChanged { total, accounts },
    );
}

#[test]
fn a_preset_change_is_written_only_when_set_and_unknown_kinds_read_as_unknown() {
    let older = serde_json::to_value(entry()).unwrap();
    assert!(older.get("preset_change").is_none(), "{older}");
    assert_eq!(
        serde_json::from_value::<AgentEntry>(older).unwrap(),
        entry()
    );
    let marked = AgentEntry {
        preset_change: Some(PresetChange::Edited),
        ..entry()
    };
    let json = serde_json::to_value(&marked).unwrap();
    assert_eq!(json["preset_change"], json!("edited"));
    assert_eq!(serde_json::from_value::<AgentEntry>(json).unwrap(), marked);
    assert_eq!(
        serde_json::from_value::<PresetChange>(json!("renamed")).unwrap(),
        PresetChange::Unknown
    );
}

#[test]
fn each_state_names_one_node_state_and_only_the_accounts_queue_is_working() {
    use wisp_protocol::slot::NodeState::{Done, Failed, Stopped, Waiting, Working};
    let idle = |outcome| AgentState::Idle { outcome };
    let queued = |reason| AgentState::Queued { reason };
    let cases = [
        (AgentState::Running, Working),
        (AgentState::Stopping, Working),
        (queued(QueueReason::Endpoint), Working),
        (queued(QueueReason::Concurrency), Waiting),
        (queued(QueueReason::Stagger), Waiting),
        (queued(QueueReason::Retry), Waiting),
        (queued(QueueReason::Unknown), Waiting),
        (
            AgentState::Held {
                reason: HoldReason::HeldMessages,
            },
            Waiting,
        ),
        (idle(Some(IdleOutcome::Asked)), Waiting),
        (idle(Some(IdleOutcome::Waiting)), Waiting),
        (idle(Some(IdleOutcome::Reported)), Done),
        (idle(Some(IdleOutcome::Failed)), Failed),
        (idle(Some(IdleOutcome::InvalidReport)), Failed),
        (idle(Some(IdleOutcome::NoReport)), Stopped),
        (idle(Some(IdleOutcome::Stopped)), Stopped),
        (idle(Some(IdleOutcome::Unknown)), Stopped),
        (idle(None), Stopped),
        (AgentState::Ended, Stopped),
        (AgentState::Unknown, Stopped),
    ];
    for (state, node) in cases {
        assert_eq!(state.node_state(), node, "{state:?}");
    }
}
