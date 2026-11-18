#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use serde_json::json;
use support::golden;
use wisp_protocol::slot::{SlotSpan, SlotTone};
use wisp_protocol::{
    AgentEvent, ClientCommand, CommandLine, CommandOutcome, CommandRefusal, CommandWait, InputId,
    RequestId,
};

fn refused(kind: CommandRefusal, hint: &str) -> CommandOutcome {
    CommandOutcome::Refused {
        kind,
        hint: hint.into(),
    }
}

#[test]
fn plugin_commands_and_their_answers_match_their_fixtures() {
    golden(
        "command_run_plugin_command",
        &ClientCommand::RunPluginCommand {
            name: "acme:deploy".into(),
            args: wisp_protocol::Secret::new("staging --dry"),
            request: RequestId(7),
        },
    );
    let outcomes = [
        ("none", CommandOutcome::None),
        (
            "note",
            CommandOutcome::Note {
                text: "3 files tidied".into(),
            },
        ),
        (
            "lines",
            CommandOutcome::Lines {
                lines: vec![
                    CommandLine::Row {
                        left: vec![SlotSpan::Text {
                            text: "Skills".into(),
                            tone: SlotTone::Text,
                            bold: true,
                        }],
                        right: vec![SlotSpan::Text {
                            text: "1".into(),
                            tone: SlotTone::Text,
                            bold: false,
                        }],
                    },
                    CommandLine::Text {
                        spans: vec![SlotSpan::Text {
                            text: "  A project guide".into(),
                            tone: SlotTone::Muted,
                            bold: false,
                        }],
                    },
                ],
            },
        ),
        (
            "sent",
            CommandOutcome::Sent {
                input: InputId("wisp-command-2".into()),
            },
        ),
        (
            "failed",
            CommandOutcome::Failed {
                message: "the acme plugin panicked".into(),
            },
        ),
        ("timed_out", CommandOutcome::TimedOut),
        ("cancelled", CommandOutcome::Cancelled),
        (
            "refused_unknown",
            refused(CommandRefusal::Unknown, "there is no command /acme:nope"),
        ),
        (
            "refused_busy",
            refused(CommandRefusal::Busy, "Deploy runs between turns."),
        ),
        (
            "refused_plugin_off",
            refused(CommandRefusal::PluginOff, "the acme plugin is turned off"),
        ),
        (
            "refused_too_many",
            refused(
                CommandRefusal::TooMany,
                "32 messages are already waiting. Send this after the turn.",
            ),
        ),
        (
            "refused_empty",
            refused(CommandRefusal::Empty, "empty message: nothing to send"),
        ),
    ];
    for (name, outcome) in outcomes {
        golden(
            &format!("event_command_done_{name}"),
            &AgentEvent::CommandDone {
                answers: RequestId(7),
                outcome,
            },
        );
    }
    for (name, until) in [
        ("call", CommandWait::Call),
        ("hook", CommandWait::Hook),
        ("command", CommandWait::Command),
    ] {
        golden(
            &format!("event_command_deferred_{name}"),
            &AgentEvent::CommandDeferred {
                request: RequestId(8),
                until,
            },
        );
    }
}

#[test]
fn a_command_with_no_args_reads_as_empty() {
    let command: ClientCommand = serde_json::from_value(json!({
        "type": "run_plugin_command",
        "name": "acme:deploy",
        "request": 1,
    }))
    .unwrap();
    assert_eq!(
        command,
        ClientCommand::RunPluginCommand {
            name: "acme:deploy".into(),
            args: wisp_protocol::Secret::new(String::new()),
            request: RequestId(1),
        }
    );
}

#[test]
fn newer_outcomes_waits_and_refusals_read_as_their_catch_alls() {
    let outcome: CommandOutcome = serde_json::from_value(json!({"type": "sulked"})).unwrap();
    assert_eq!(outcome, CommandOutcome::Unknown);
    let line: CommandLine = serde_json::from_value(json!({"type": "new_line"})).unwrap();
    assert_eq!(line, CommandLine::Unknown);
    let until: CommandWait = serde_json::from_value(json!("the_moon")).unwrap();
    assert_eq!(until, CommandWait::Unknown);
    let refusal: CommandOutcome =
        serde_json::from_value(json!({"type": "refused", "kind": "too_tired", "hint": "later"}))
            .unwrap();
    assert_eq!(refusal, refused(CommandRefusal::Other, "later"));
    let unknown: CommandRefusal = serde_json::from_value(json!("unknown")).unwrap();
    assert_eq!(unknown, CommandRefusal::Unknown);
}

#[test]
fn session_descriptions_and_window_loss_match_their_fixtures() {
    golden(
        "command_describe_session",
        &ClientCommand::DescribeSession {
            request: RequestId(9),
        },
    );
    golden("command_window_gone", &ClientCommand::WindowGone);
    golden(
        "command_returned_taken",
        &ClientCommand::ReturnedTaken {
            ids: vec![InputId("tui-7".into())],
        },
    );
    golden(
        "event_session_snapshot",
        &AgentEvent::SessionSnapshot {
            answers: RequestId(9),
            view: wisp_protocol::SessionView {
                returned: vec![wisp_protocol::Returned {
                    id: Some(InputId("tui-7".into())),
                    text: "later".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            now: wisp_protocol::Now {
                endpoint: "work".into(),
                model: "model".into(),
                effort: wisp_protocol::Effort::Low,
                cache_ttl: wisp_protocol::CacheTtl::OneHour,
                next: Some(wisp_protocol::NextTurn {
                    model: Some("next".into()),
                    ..Default::default()
                }),
                endpoints: Vec::new(),
            },
            state: vec![AgentEvent::AgentsSnapshot {
                request: None,
                agents: Vec::new(),
            }],
        },
    );
    let old = r#"{"text":"later","images":[],"missing":0}"#;
    let returned: wisp_protocol::Returned = serde_json::from_str(old).unwrap();
    assert_eq!(returned.id, None);
    assert_eq!(serde_json::to_string(&returned).unwrap(), old);
}

#[test]
fn the_permanent_link_and_daemon_frames_match_their_fixtures() {
    use wisp_protocol::link::{FromDaemon, ToClient, ToDaemon, ToHost, Welcome};

    golden(
        "link_hello",
        &ToHost::Hello {
            contract: 1,
            build: 7,
        },
    );
    golden("link_end", &ToHost::End { forced: false });
    golden("link_restart", &ToHost::Restart { wait: true });
    golden("link_restarting", &ToClient::Restarting);
    golden(
        "link_welcome",
        &ToClient::Welcome(Welcome {
            contract: 1,
            build: 7,
            wisp: "0.1.0".into(),
            session: "abcd-1234".into(),
            first_request: RequestId(1),
            folder: "/project".into(),
            window: false,
            restart: true,
        }),
    );
    golden(
        "link_refused",
        &ToClient::Refused {
            contract: 1,
            wisp: "0.1.0".into(),
        },
    );
    golden("daemon_hello", &ToDaemon::Hello { daemon: 1 });
    golden("daemon_welcome", &FromDaemon::Welcome { daemon: 1 });
    golden("daemon_refused", &FromDaemon::Refused { daemon: 1 });
    golden("daemon_stop", &ToDaemon::Stop);
}

#[test]
fn a_running_chat_row_keeps_its_hosted_fact() {
    let row = wisp_protocol::ChatRow {
        id: "abcd-1234".into(),
        name: Some("A kept chat".into()),
        topic: String::new(),
        typed: String::new(),
        active_at: "1h ago".into(),
        account: None,
        mark: None,
        model: None,
        agents: 0,
        open_elsewhere: true,
        hosted: true,
    };
    golden(
        "event_hosted_chat",
        &AgentEvent::ChatsListed {
            request: RequestId(1),
            chats: vec![row],
        },
    );
}

#[test]
fn spend_listing_and_its_answer_match_their_fixtures() {
    use wisp_protocol::{AccountSpend, SpendDay, SpendWindow};

    golden(
        "command_list_spend",
        &ClientCommand::ListSpend {
            request: RequestId(2),
        },
    );
    let total = AccountSpend {
        usd: 3.0,
        plan_usd: Some(1.0),
        lower_bound: true,
    };
    let accounts: std::collections::BTreeMap<_, _> = [
        ("api".into(), AccountSpend::charged(None, Some(2.0), true)),
        (
            "subscription".into(),
            AccountSpend {
                usd: 1.0,
                plan_usd: Some(1.0),
                lower_bound: true,
            },
        ),
    ]
    .into();
    golden(
        "event_spend_listed",
        &AgentEvent::SpendListed {
            request: RequestId(2),
            days: vec![SpendDay {
                day: "2026-11-14".into(),
                accounts: accounts.clone(),
                total,
            }],
            window: SpendWindow {
                since: Some("2026-11-14".into()),
                accounts,
                total,
            },
        },
    );
}

#[test]
fn private_plugin_arguments_keep_the_wire_string_but_never_appear_in_debug() {
    let args = "sign-in linear http://127.0.0.1:8123/oauth-callback?code=CANARY-code-0011&state=CANARY-state";
    let json = json!({"type":"run_plugin_command", "name":"mcp:mcp", "args":args, "request":7});
    let command: ClientCommand = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&command).unwrap(), json);
    assert!(!format!("{command:?}").contains("CANARY"));
    let ClientCommand::RunPluginCommand { args: received, .. } = command else {
        panic!("not a plugin command")
    };
    assert_eq!(received.expose(), args);
}

#[test]
fn restart_readiness_matches_its_fixtures() {
    use wisp_protocol::{AgentPath, RestartState, Unfinished, Unready};

    golden("command_get_ready", &ClientCommand::GetReady { on: true });
    for (name, state, open) in [
        (
            "getting_ready",
            RestartState::GettingReady,
            vec![Unready {
                agent: AgentPath::root(),
                what: Unfinished::Reply,
                title: None,
                target: String::new(),
                running_ms: 1250,
                opening: true,
            }],
        ),
        ("ready", RestartState::Ready, Vec::new()),
        ("cancelled", RestartState::Cancelled, Vec::new()),
    ] {
        golden(
            &format!("event_restart_{name}"),
            &AgentEvent::Restart { state, open },
        );
    }
    golden("event_ready_to_restart", &AgentEvent::ReadyToRestart);
    let row: Unready = serde_json::from_value(json!({
        "agent": "/", "what": "future_kind", "target": "", "running_ms": 0
    }))
    .unwrap();
    assert_eq!(row.what, Unfinished::Other);
    assert_eq!(
        serde_json::to_value(&row).unwrap(),
        json!({
            "agent": "/", "what": "other", "target": "", "running_ms": 0
        })
    );
    let mut rows = vec![row.clone(), row];
    rows[0].running_ms = 1250;
    rows[1].running_ms = u64::MAX - 1;
    wisp_protocol::age_unready(&mut rows, std::time::Duration::from_micros(2501));
    assert_eq!(rows[0].running_ms, 1252);
    assert_eq!(rows[1].running_ms, u64::MAX);
    wisp_protocol::age_unready(&mut rows, std::time::Duration::MAX);
    assert!(rows.iter().all(|row| row.running_ms == u64::MAX));
}
