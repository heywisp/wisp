use clap::{CommandFactory, Parser};
use wisp_protocol::{ErrorKind, StopReason, TurnOutcome, TurnStats, WithdrawnBy};

use super::*;
use crate::Args;
use hostile::{corpus, hostile_detail, hostile_events};

mod hostile;

fn pending_line() -> JsonlState {
    JsonlState {
        line: Some(InputId("jsonl-1".into())),
        ..JsonlState::default()
    }
}

fn delivered() -> AgentEvent {
    AgentEvent::InputDelivered {
        ids: vec![InputId("jsonl-1".into())],
        step: 1,
    }
}

fn ended(outcome: TurnOutcome) -> AgentEvent {
    AgentEvent::TurnEnded {
        outcome,
        stats: TurnStats::default(),
    }
}

fn completed() -> AgentEvent {
    ended(TurnOutcome::Completed {
        stop_reason: StopReason::EndTurn,
        model: "test".into(),
        usage: Default::default(),
        cost: None,
        committed: true,
        steps: 1,
        tool_calls: 0,
    })
}

fn failed(kind: ErrorKind) -> AgentEvent {
    ended(TurnOutcome::Error {
        kind,
        message: "did not finish".into(),
        usage: None,
        cost: None,
        committed_steps: 0,
        detail: None,
        attempts: 0,
        gave_up: None,
        offer: None,
    })
}

fn see(state: &mut JsonlState, event: AgentEvent) {
    assert_eq!(settle(state, JsonlEvent::Agent(&event)), None, "{event:?}");
}

#[test]
fn a_line_settles_on_its_turn_ending_even_if_the_first_step_failed_or_never_started() {
    for case in ["completed", "failed step", "failed before step"] {
        let mut state = pending_line();
        see(&mut state, AgentEvent::turn_started(1));
        if case != "failed before step" {
            see(&mut state, AgentEvent::StepStarted { step: 1 });
        }
        if case == "completed" {
            see(&mut state, delivered());
        } else {
            see(
                &mut state,
                AgentEvent::InputWithdrawn {
                    ids: vec![InputId("jsonl-1".into())],
                    by: WithdrawnBy::TurnFailed,
                },
            );
        }
        assert!(state.line.is_some(), "{case}");
        see(
            &mut state,
            if case == "completed" {
                completed()
            } else {
                failed(ErrorKind::Auth)
            },
        );
        assert!(!state.active(), "{case}");
    }
}

#[test]
fn a_blocked_or_cancelled_hook_settles_the_line_on_rejection() {
    for id in [Some(InputId("jsonl-1".into())), None] {
        let mut state = pending_line();
        see(
            &mut state,
            AgentEvent::InputWithdrawn {
                ids: vec![InputId("jsonl-1".into())],
                by: WithdrawnBy::Hook {
                    hook: "test/input".into(),
                    reason: "blocked".into(),
                },
            },
        );
        assert!(state.line.is_some());
        see(
            &mut state,
            AgentEvent::Rejected {
                id,
                request: None,
                kind: None,
                reason: "blocked".into(),
            },
        );
        assert!(!state.active());
    }
}

#[test]
fn rejecting_a_line_does_not_rearm_cancellation_for_an_independent_turn() {
    let mut state = pending_line();
    see(&mut state, AgentEvent::turn_started(1));
    assert_eq!(
        settle(&mut state, JsonlEvent::Interrupt),
        Some(ClientCommand::Cancel)
    );
    see(
        &mut state,
        AgentEvent::Rejected {
            id: Some(InputId("jsonl-1".into())),
            request: None,
            kind: None,
            reason: "rejected waiting input".into(),
        },
    );
    assert!(state.line.is_none());
    assert!(state.in_turn);
    assert_eq!(settle(&mut state, JsonlEvent::Interrupt), None);
}

#[test]
fn a_turn_closed_at_resume_before_the_first_message_does_not_end_that_message() {
    let mut state = pending_line();
    assert_eq!(
        settle(&mut state, JsonlEvent::Interrupt),
        Some(ClientCommand::Cancel)
    );
    see(&mut state, failed(ErrorKind::Interrupted));
    assert!(state.line.is_some());
    assert_eq!(settle(&mut state, JsonlEvent::Interrupt), None);
    see(&mut state, AgentEvent::turn_started(2));
    see(&mut state, delivered());
    see(&mut state, completed());
    assert!(!state.active());
}

#[test]
fn a_turn_started_by_itself_does_not_settle_a_waiting_line() {
    let mut state = pending_line();
    see(&mut state, AgentEvent::turn_started(1));
    see(&mut state, AgentEvent::rejected("unrelated"));
    see(&mut state, completed());
    assert!(state.line.is_some());
    see(&mut state, AgentEvent::turn_started(2));
    see(&mut state, delivered());
    see(&mut state, completed());
    assert!(!state.active());
}

#[test]
fn sigint_cancels_once_before_a_line_starts_and_once_per_turn() {
    let mut state = pending_line();
    let interrupt_once = |state: &mut JsonlState| {
        assert_eq!(
            settle(state, JsonlEvent::Interrupt),
            Some(ClientCommand::Cancel)
        );
        assert_eq!(settle(state, JsonlEvent::Interrupt), None);
    };
    interrupt_once(&mut state);
    let held = AgentEvent::InputHeld {
        ids: vec![InputId("jsonl-1".into())],
        by: WithdrawnBy::TurnStopped,
    };
    assert_eq!(
        settle(&mut state, JsonlEvent::Agent(&held)),
        Some(ClientCommand::Release)
    );
    assert!(state.line.is_some());
    see(&mut state, AgentEvent::turn_started(1));
    see(&mut state, delivered());
    interrupt_once(&mut state);
    see(&mut state, completed());
    assert!(!state.cancelling);
    see(&mut state, AgentEvent::turn_started(2));
    assert!(state.line.is_none());
    interrupt_once(&mut state);
}

#[tokio::test]
async fn an_agent_that_exits_while_idle_is_an_error() {
    let (commands, _sent) = mpsc::channel(1);
    let (emit, events) = mpsc::channel(1);
    let (_stdin, lines) = mpsc::channel(1);
    drop(emit);
    let channels = Channels {
        commands,
        events,
        quit: None,
    };
    let result = jsonl_loop(channels, lines, interrupts().unwrap(), 0, |_| Ok(())).await;
    assert_eq!(result, Err(AGENT_STOPPED.to_owned()));
}

#[cfg(unix)]
#[tokio::test]
async fn a_sigint_delivered_while_rendering_still_cancels() {
    use rustix::process::{Signal, getpid, kill_process};
    use std::time::Duration;

    let interrupt = interrupts().unwrap();
    let (commands, mut sent) = mpsc::channel(2);
    let (emit, events) = mpsc::channel(32);
    let (stdin, lines) = mpsc::channel(1);
    stdin.send(Ok("hello".into())).await.unwrap();
    drop(stdin);
    let agent = tokio::spawn(async move {
        assert_eq!(
            sent.recv().await,
            Some(ClientCommand::queue("jsonl-1", "hello"))
        );
        for event in [AgentEvent::turn_started(1), delivered()]
            .into_iter()
            .chain((0..20).map(|i| AgentEvent::TextDelta {
                index: 0,
                text: format!("line {i}"),
            }))
        {
            emit.try_send(event).unwrap();
        }
        assert_eq!(sent.recv().await, Some(ClientCommand::Cancel));
        emit.send(ended(TurnOutcome::Cancelled {
            usage: None,
            cost: None,
            committed_steps: 0,
        }))
        .await
        .unwrap();
        assert_eq!(sent.recv().await, None);
    });
    let channels = Channels {
        commands,
        events,
        quit: None,
    };
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        jsonl_loop(channels, lines, interrupt, 0, |event| {
            if matches!(event, AgentEvent::TextDelta { text, .. } if text == "line 0") {
                kill_process(getpid(), Signal::INT).unwrap();
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        }),
    )
    .await;
    assert_eq!(result.unwrap(), Ok(Drained::Closed));
    agent.await.unwrap();
}

#[test]
fn verbose_is_the_only_jsonl_warning() {
    let mut p = Presentation {
        presenter: Presenter::Jsonl,
        verbose: false,
        tui_verbose: true,
        display: wisp_tui::term::SavedDisplay::default(),
    };
    assert!(jsonl_warnings(&p).is_empty());
    p.verbose = true;
    assert_eq!(
        jsonl_warnings(&p),
        vec!["wisp: --verbose has no effect with --output jsonl".to_owned()]
    );
}

#[tokio::test]
async fn a_line_that_is_not_utf8_is_an_error() {
    let input = std::io::Cursor::new(b"hello\n\xff\xfe\nlater\n".to_vec());
    let mut lines = read_lines(input);
    assert_eq!(lines.recv().await, Some(Ok("hello".to_owned())));
    assert!(matches!(lines.recv().await, Some(Err(e)) if e.contains("stdin")));
    assert_eq!(lines.recv().await, None);
}

#[test]
fn output_flags_parse_and_jsonl_is_marked_unstable() {
    let args = Args::try_parse_from(["wisp"]).unwrap();
    assert_eq!(args.output, OutputMode::Auto);
    assert!(!args.verbose);
    let args = Args::try_parse_from(["wisp", "--output", "jsonl", "--verbose"]).unwrap();
    assert_eq!(args.output, OutputMode::Jsonl);
    assert!(args.verbose);
    assert!(Args::try_parse_from(["wisp", "--output", "fancy"]).is_err());
    let help = Args::command().term_width(0).render_long_help().to_string();
    assert!(help.contains("Unstable until M6"), "{help}");
    assert!(help.contains("[default: auto]"), "{help}");
    for mode in ["auto", "tui", "jsonl"] {
        assert_eq!(
            Args::try_parse_from(["wisp", "--output", mode])
                .unwrap()
                .output
                .requested(),
            match mode {
                "auto" => Requested::Auto,
                "tui" => Requested::Tui,
                _ => Requested::Jsonl,
            }
        );
    }
    assert!(help.contains("--verbose"), "{help}");
    assert!(!help.contains("plain"), "{help}");
    assert!(!help.contains("screen"), "{help}");
}

#[test]
fn plain_output_and_screen_reader_mode_are_gone() {
    for args in [
        &["wisp", "--output", "plain"][..],
        &["wisp", "--screen-reader"][..],
    ] {
        let err = Args::try_parse_from(args).err().unwrap();
        assert_eq!(err.exit_code(), 2, "{args:?}");
    }
}

#[test]
fn a_hostile_error_and_its_detail_show_inert_in_the_transcript() {
    use wisp_render::markdown::{Layout, Renderer};
    use wisp_render::theme::glyphs::GlyphSet;
    use wisp_render::transcript::{Config, Detail, Transcript};
    let renderer = Renderer::new(Layout::new(200, GlyphSet::Unicode));
    for h in corpus() {
        let mut transcript = Transcript::new(Config {
            provider: "Anthropic".into(),
            cache_ttl: wisp_protocol::CacheTtl::OneHour,
            logging: true,
        });
        let event = AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                message: h.clone(),
                usage: None,
                cost: None,
                committed_steps: 2,
                detail: Some(hostile_detail(&h)),
                attempts: 0,
                gave_up: None,
                offer: None,
            },
            stats: TurnStats::default(),
        };
        transcript.apply(&AgentEvent::turn_started(1), std::time::Duration::ZERO);
        transcript.apply(&event, std::time::Duration::ZERO);
        for detail in [Detail::Scrollback, Detail::Full] {
            let shown: String = transcript
                .rows(&renderer, detail)
                .iter()
                .flat_map(|row| row.spans.iter().map(|s| s.text.clone()))
                .collect();
            assert!(
                !shown
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\u{202A}'..='\u{202E}')),
                "{shown:?}"
            );
        }
        let full: String = transcript
            .rows(&renderer, Detail::Full)
            .iter()
            .flat_map(|row| row.spans.iter().map(|s| s.text.clone()))
            .collect();
        assert!(full.contains("502"), "{full:?}");
        let line = jsonl_line(&event).unwrap();
        let body = line.strip_suffix('\n').unwrap();
        assert!(!body.chars().any(char::is_control), "{line:?}");
    }
}

#[test]
fn jsonl_lines_carry_no_raw_controls_and_round_trip() {
    for h in corpus() {
        for event in hostile_events(&h) {
            let line = jsonl_line(&event).unwrap();
            let body = line.strip_suffix('\n').unwrap();
            assert!(!body.contains('\n'), "{line:?}");
            let raw: Vec<char> = body
                .chars()
                .filter(|c| c.is_control() || wisp_protocol::jsonl::hidden_format(*c))
                .collect();
            assert_eq!(raw, Vec::<char>::new(), "{line:?}");
            let back: AgentEvent = serde_json::from_str(body).unwrap();
            assert_eq!(back, event);
        }
    }
}

#[test]
fn jsonl_matches_the_session_log_escaping() {
    let event = AgentEvent::TextDelta {
        index: 0,
        text: "a\u{7f}b\u{9b}c\u{1b}d".into(),
    };
    let line = jsonl_line(&event).unwrap();
    assert_eq!(
        line,
        "{\"type\":\"text_delta\",\"index\":0,\"text\":\"a\\u007fb\\u009bc\\u001bd\"}\n"
    );
}

#[test]
fn help_fits_80_columns() {
    let built = crate::tools::command::<Args>;
    for help in [
        built().term_width(80).render_help().to_string(),
        built().term_width(80).render_long_help().to_string(),
    ] {
        for line in help.lines() {
            assert!(line.chars().count() <= 80, "{line:?}");
        }
    }
}

#[test]
fn account_telemetry_is_excluded_from_root_and_child_jsonl_output() {
    let event = AgentEvent::PlanUsageChanged {
        request: Some(wisp_protocol::RequestId(2)),
        endpoint: "alias".into(),
        state: wisp_protocol::plan_usage::UsageState {
            snapshot: Some({
                let mut snapshot = wisp_protocol::plan_usage::PlanUsage::new(None, vec![]).unwrap();
                snapshot.plan_tier = Some(wisp_protocol::plan_usage::PlanTier::Pro);
                snapshot
            }),
            loading: false,
            stale: true,
            error: Some(wisp_protocol::plan_usage::PlanUsageError::Unsupported),
        },
    };
    assert_eq!(jsonl_line(&event).unwrap(), "");
    let child = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::root(),
        event: Box::new(event),
    };
    assert_eq!(jsonl_line(&child).unwrap(), "");
}

#[test]
fn private_auth_challenges_are_excluded_from_jsonl_even_inside_child_events() {
    use wisp_protocol::subscription_auth::{
        AuthEvent, AuthOutcome, ChallengeKind, PrivateChallenge, SubscriptionProvider,
    };
    let event = AgentEvent::SubscriptionAuth {
        event: AuthEvent {
            request: wisp_protocol::RequestId(1),
            outcome: AuthOutcome::Challenge {
                challenge: PrivateChallenge {
                    login: wisp_protocol::RequestId(1),
                    provider: SubscriptionProvider::ChatGpt,
                    alias: "plan".to_owned(),
                    kind: ChallengeKind::BrowserLoopback,
                    authorization_url: wisp_protocol::Secret::new(
                        "https://example.invalid/?state=STATE_CANARY&code=CODE_CANARY&token=TOKEN_CANARY&account=ACCOUNT_CANARY",
                    ),
                    expires_in_secs: 600,
                    opened: false,
                },
            },
        },
    };
    assert!(super::jsonl_line(&event).unwrap().is_empty());
    let nested = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::root(),
        event: Box::new(event),
    };
    assert!(super::jsonl_line(&nested).unwrap().is_empty());
}

#[test]
fn highest_jsonl_ignores_other_clients_ids() {
    let highest_jsonl = |ids: &[&str]| InputId::highest_numbered(super::JSONL_PREFIX, ids.to_vec());
    let ids = ["tui-9", "jsonl-2", "a", "jsonl-x", "jsonl-007", "jsonl-"];
    assert_eq!(highest_jsonl(&ids), 7);
    assert_eq!(highest_jsonl(&[]), 0);
    assert_eq!(highest_jsonl(&["tui-4"]), 0);
}

#[test]
fn jsonl_hides_server_sign_in_links_but_keeps_its_notes() {
    let event: AgentEvent = serde_json::from_value(serde_json::json!({"type":"plugins_changed","notes":[{"plugin":"mcp","message":"child added server local"}],"links":[{"plugin":"mcp","message":"sign in","link":"https://example.invalid/authorize?CANARY-placeholder"}]})).unwrap();
    let nested = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::new("/child"),
        event: Box::new(event.clone()),
    };
    for event in [event, nested] {
        let line = super::jsonl_line(&event).unwrap();
        assert!(!line.contains("CANARY-placeholder"));
        assert!(line.contains("child added server local"));
        assert!(!line.contains("\"links\""));
    }
    let only: AgentEvent = serde_json::from_value(serde_json::json!({"type":"plugins_changed","links":[{"plugin":"mcp","message":"sign in","link":"https://example.invalid/authorize?CANARY-placeholder"}]})).unwrap();
    for event in [
        only.clone(),
        AgentEvent::Agent {
            agent: wisp_protocol::AgentPath::new("/child"),
            event: Box::new(only),
        },
    ] {
        assert!(super::jsonl_line(&event).unwrap().is_empty());
    }
}

#[tokio::test]
async fn jsonl_scrubs_queued_events_against_a_secret_registered_after_they_were_queued() {
    use crate::scrub::testing::{CANARY, queued, scrub};
    use wisp_core::session::{REDACTED, register_secrets};

    let scrub = scrub();
    let (commands, _sent) = mpsc::channel(1);
    let (_stdin, lines) = mpsc::channel(1);
    let (emit, events) = mpsc::channel(8);
    let (drain_emit, mut drain_events) = mpsc::channel(8);
    for event in queued() {
        emit.send(event.clone()).await.unwrap();
        drain_emit.send(event).await.unwrap();
    }
    drop((emit, drain_emit));
    register_secrets([CANARY.to_owned()]);
    let channels = Channels {
        commands,
        events,
        quit: None,
    };
    let mut looped = Vec::new();
    let result = jsonl_loop(channels, lines, interrupts().unwrap(), 0, |event| {
        write_event(&scrub, event, &mut looped)
    })
    .await;
    assert_eq!(result, Err(AGENT_STOPPED.to_owned()));
    let mut drain = Vec::new();
    let ended = drained(&mut drain_events, None, |event| {
        write_event(&scrub, event, &mut drain)
    })
    .await;
    assert_eq!(ended, Ok(Drained::Closed));
    for (path, out) in [("loop", looped), ("drain", drain)] {
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains(CANARY), "{path}: {text}");
        assert_eq!(text.lines().count(), 4, "{path}: {text}");
        for line in text.lines() {
            assert_eq!(line.matches(REDACTED).count(), 1, "{path}: {line}");
        }
    }
}
