#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use serde_json::json;
use support::{golden, record};
use wisp_protocol::log::{
    ImageRef, LEAD_PROCESSES, LOG_SCHEMA_VERSION, LogEvent, LogRecord, LoggedInput, TurnLead,
    rebuild_history,
};
use wisp_protocol::{
    AgentEvent, AttachInput, AttachItems, AttachKey, AttachKeyCode, AttachRequestEnd,
    BackgroundReason, ClientCommand, ContentBlock, DetachReason, Ends, ExitStatus, Fact, ImageType,
    InputId, LostReason, Message, NeedsInputKind, OutputKind, ProcessEnd, RejectKind, ReportedEnd,
    Role, Secret, SecretRequestEnd, SignalKind, StoppedBy, TermSize, ToolAction, ToolDisplay,
    ToolOutcome, ToolOutputView, ToolPurpose, TurnOrigin,
};

const SIZE: TermSize = TermSize {
    cols: 100,
    rows: 30,
};

fn ends() -> Vec<(&'static str, ProcessEnd)> {
    vec![
        (
            "exited",
            ProcessEnd::Exited {
                exit: ExitStatus::Code { code: 1 },
            },
        ),
        (
            "exited_by_signal",
            ProcessEnd::Exited {
                exit: ExitStatus::Signal { signal: 15 },
            },
        ),
        (
            "stopped_by_model",
            ProcessEnd::Stopped {
                by: StoppedBy::Model,
            },
        ),
        (
            "stopped_by_user",
            ProcessEnd::Stopped {
                by: StoppedBy::User,
            },
        ),
        (
            "stopped_session_ended",
            ProcessEnd::Stopped {
                by: StoppedBy::SessionEnded,
            },
        ),
        (
            "stopped_time_limit",
            ProcessEnd::Stopped {
                by: StoppedBy::TimeLimit { after_ms: 600_000 },
            },
        ),
        (
            "lost_stopped_at_resume",
            ProcessEnd::Lost {
                reason: LostReason::StoppedAtResume,
            },
        ),
        (
            "lost_gone",
            ProcessEnd::Lost {
                reason: LostReason::Gone,
            },
        ),
        (
            "failed",
            ProcessEnd::Failed {
                message: "setsid: no --ctty".into(),
            },
        ),
    ]
}

fn key(code: AttachKeyCode, shift: bool, alt: bool, ctrl: bool) -> AttachKey {
    AttachKey::new(code, shift, alt, ctrl).unwrap()
}

fn items() -> AttachItems {
    AttachItems(vec![
        AttachInput::Text { text: "y".into() },
        AttachInput::Key {
            key: key(AttachKeyCode::Enter, false, false, false),
        },
        AttachInput::Key {
            key: key(AttachKeyCode::Char { char: 'c' }, false, false, true),
        },
        AttachInput::Key {
            key: key(AttachKeyCode::Up, true, true, false),
        },
        AttachInput::Key {
            key: key(AttachKeyCode::F { n: 5 }, false, false, false),
        },
        AttachInput::Paste {
            text: "line one\nline two".into(),
        },
    ])
}

#[test]
fn every_process_end_matches_its_fixture() {
    for (name, end) in ends() {
        golden(&format!("process_end_{name}"), &end);
    }
}

#[test]
fn the_process_events_match_their_fixtures() {
    let events = [
        (
            "event_process_started",
            AgentEvent::ProcessStarted {
                id: 5,
                call_id: "toolu_05".into(),
                target: "python3 -i".into(),
                tty: true,
                stdin: true,
                size: Some(SIZE),
                file: Some("/tmp/wisp-s1/0012-bash.out".into()),
                reason: BackgroundReason::Quiet,
                keeps_running: false,
                pid: None,
                log: None,
            },
        ),
        (
            "event_process_started_pipe",
            AgentEvent::ProcessStarted {
                id: 3,
                call_id: "toolu_03".into(),
                target: "npm run dev".into(),
                tty: false,
                stdin: true,
                size: None,
                file: None,
                reason: BackgroundReason::Started,
                keeps_running: false,
                pid: None,
                log: None,
            },
        ),
        (
            "event_process_output",
            AgentEvent::ProcessOutput {
                id: 3,
                text: "\u{1b}[32m  ready\u{1b}[0m in 412 ms\r\n".into(),
                skipped: 8192,
            },
        ),
        (
            "event_process_resized",
            AgentEvent::ProcessResized { id: 5, size: SIZE },
        ),
        (
            "event_process_ended",
            AgentEvent::ProcessEnded {
                id: 4,
                end: ProcessEnd::Exited {
                    exit: ExitStatus::Code { code: 1 },
                },
                bytes: 98_765,
                lines: 1248,
                duration_ms: 72_000,
            },
        ),
        (
            "event_process_ended_time_limit",
            AgentEvent::ProcessEnded {
                id: 4,
                end: ProcessEnd::Stopped {
                    by: StoppedBy::TimeLimit { after_ms: 600_000 },
                },
                bytes: 0,
                lines: 0,
                duration_ms: 600_004,
            },
        ),
        (
            "event_process_attached",
            AgentEvent::ProcessAttached { id: 5, size: SIZE },
        ),
        (
            "event_process_detached",
            AgentEvent::ProcessDetached {
                id: 5,
                size: TermSize { cols: 80, rows: 24 },
                reason: DetachReason::User,
                typed: true,
            },
        ),
        (
            "event_process_needs_input",
            AgentEvent::ProcessNeedsInput {
                id: 5,
                kind: NeedsInputKind::HiddenInput,
            },
        ),
        (
            "event_process_needs_input_cleared",
            AgentEvent::ProcessNeedsInputCleared { id: 5 },
        ),
        (
            "event_attach_requested",
            AgentEvent::AttachRequested {
                call_id: "toolu_07".into(),
                id: 5,
                reason: "sudo password for apt install".into(),
            },
        ),
        (
            "event_attach_request_closed",
            AgentEvent::AttachRequestClosed {
                call_id: "toolu_07".into(),
                how: AttachRequestEnd::AttachedOnly,
            },
        ),
        (
            "event_secret_requested",
            AgentEvent::SecretRequested {
                call_id: "toolu_07".into(),
                id: Some(5),
                destination: None,
                asked_by: None,
                name: "OpenAI API key".into(),
                reason: "To add it to Wisp as the account work.".into(),
                show: Ends { start: 8, end: 4 },
            },
        ),
        (
            "event_secret_requested_server",
            AgentEvent::SecretRequested {
                call_id: "toolu_08".into(),
                id: None,
                destination: Some(wisp_protocol::SecretDestination { server: "sentry".into(), command: Some("npx -y @sentry/mcp-server".into()) }),
                asked_by: None,
                name: "SENTRY_TOKEN".into(),
                reason: "Your agent asks for the key of the server sentry. Wisp keeps it for that server and never shows it to your agent.".into(),
                show: Ends { start: 4, end: 4 },
            },
        ),
        (
            "event_secret_request_closed",
            AgentEvent::SecretRequestClosed {
                call_id: "toolu_07".into(),
                how: SecretRequestEnd::Entered,
            },
        ),
        (
            "event_turn_started_wake",
            AgentEvent::TurnStarted {
                turn: 4,
                origin: Some(TurnOrigin::ProcessExit),
                answers: None,
                retry_of: None,
                ends: Vec::new(),
            },
        ),
        (
            "event_turn_started_wake_ends",
            AgentEvent::TurnStarted {
                turn: 5,
                origin: Some(TurnOrigin::ProcessExit),
                answers: None,
                retry_of: None,
                ends: vec![
                    ReportedEnd {
                        id: 3,
                        end: ProcessEnd::Exited {
                            exit: ExitStatus::Code { code: 1 },
                        },
                    },
                    ReportedEnd {
                        id: 5,
                        end: ProcessEnd::Stopped {
                            by: StoppedBy::TimeLimit { after_ms: 600_000 },
                        },
                    },
                ],
            },
        ),
        (
            "event_tool_call_started_movable",
            AgentEvent::ToolCallStarted {
                call_id: "toolu_08".into(),
                cwd: "~/Projects/Wisp".into(),
                movable: true,
                file: None,
            },
        ),
        (
            "event_tool_call_finished_backgrounded",
            AgentEvent::ToolCallFinished {
                call_id: "toolu_08".into(),
                outcome: ToolOutcome::Backgrounded {
                    reason: BackgroundReason::MovedByUser,
                },
                output: ToolOutputView {
                    tail: "  VITE v6.3.0  ready in 412 ms\n".into(),
                    bytes: 64,
                    lines: 3,
                    file: Some("/tmp/wisp-s1/0012-bash.out".into()),
                    file_problem: None,
                    inlined: true,
                },
                facts: vec![Fact::Process { id: 3 }],
                state: None,
                duration_ms: 4_200,
                result: None,
                process: Some(3),
                attachments: Vec::new(),
                notes: Vec::new(),
                typed: None,
            },
        ),
        (
            "process_restored",
            AgentEvent::ProcessRestored {
                id: 4,
                call_id: Some("toolu_04".into()),
                target: "cargo test".into(),
                tty: false,
                end: Some(ProcessEnd::Lost {
                    reason: LostReason::StoppedAtResume,
                }),
                duration_ms: 0,
                keeps_running: false,
                unchecked: false,
                pid: None,
                log: None,
            },
        ),
        (
            "event_process_started_kept",
            AgentEvent::ProcessStarted {
                id: 3,
                call_id: "toolu_03".into(),
                target: "python3 app.py".into(),
                tty: false,
                stdin: false,
                size: None,
                file: Some("/tmp/wisp-s1/0012-bash.out".into()),
                reason: BackgroundReason::Started,
                keeps_running: true,
                pid: Some(4_321),
                log: Some("/tmp/wisp-s1/0012-bash.log".into()),
            },
        ),
        (
            "process_restored_kept",
            AgentEvent::ProcessRestored {
                id: 3,
                call_id: Some("toolu_03".into()),
                target: "python3 app.py".into(),
                tty: false,
                end: None,
                duration_ms: 0,
                keeps_running: true,
                unchecked: false,
                pid: Some(4_321),
                log: Some("/tmp/wisp-s1/0012-bash.log".into()),
            },
        ),
        (
            "process_restored_unchecked",
            AgentEvent::ProcessRestored {
                id: 3,
                call_id: Some("toolu_03".into()),
                target: "python3 app.py".into(),
                tty: false,
                end: None,
                duration_ms: 0,
                keeps_running: true,
                unchecked: true,
                pid: Some(4_321),
                log: Some("/tmp/wisp-s1/0012-bash.log".into()),
            },
        ),
    ];
    for (name, event) in events {
        golden(name, &event);
    }
    golden(
        "event_secret_requested_asked",
        &AgentEvent::SecretRequested {
            call_id: "toolu_09".into(),
            id: None,
            destination: Some(wisp_protocol::SecretDestination {
                server: "linear".into(),
                command: None,
            }),
            asked_by: Some(wisp_protocol::AskedBy::Server {
                name: "linear".into(),
            }),
            name: "Admin token".into(),
            reason: "An admin token is needed to archive a project.".into(),
            show: Ends::default(),
        },
    );
    let unknown: wisp_protocol::AskedBy =
        serde_json::from_value(serde_json::json!({"kind":"plugin","id":"x"})).unwrap();
    assert_eq!(unknown, wisp_protocol::AskedBy::Unknown);
    for detach in [DetachReason::Ended, DetachReason::Disconnected] {
        let event = AgentEvent::ProcessDetached {
            id: 1,
            size: SIZE,
            reason: detach,
            typed: false,
        };
        let back: AgentEvent =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(back, event);
    }
    let hows = [
        (AttachRequestEnd::Typed, "typed"),
        (AttachRequestEnd::AttachedOnly, "attached_only"),
        (AttachRequestEnd::Declined, "declined"),
        (AttachRequestEnd::ProcessEnded, "process_ended"),
        (AttachRequestEnd::Stopped, "stopped"),
    ];
    for (how, text) in hows {
        assert_eq!(serde_json::to_value(how).unwrap(), json!(text));
        assert_eq!(
            serde_json::from_value::<AttachRequestEnd>(json!(text)).unwrap(),
            how
        );
    }
    for (how, text) in [
        (SecretRequestEnd::Entered, "entered"),
        (SecretRequestEnd::ProcessEnded, "process_ended"),
        (SecretRequestEnd::Stopped, "stopped"),
    ] {
        assert_eq!(serde_json::to_value(how).unwrap(), json!(text));
        assert_eq!(
            serde_json::from_value::<SecretRequestEnd>(json!(text)).unwrap(),
            how
        );
    }
}

#[test]
fn the_process_commands_match_their_fixtures() {
    let commands = [
        ("command_stop_process", ClientCommand::StopProcess { id: 3 }),
        (
            "command_background",
            ClientCommand::Background {
                call_id: "toolu_08".into(),
            },
        ),
        (
            "command_attach_process",
            ClientCommand::AttachProcess { id: 5, size: None },
        ),
        (
            "command_process_input",
            ClientCommand::ProcessInput {
                id: 5,
                items: items(),
            },
        ),
        (
            "command_detach_process",
            ClientCommand::DetachProcess { id: 5 },
        ),
        (
            "command_decline_attach",
            ClientCommand::DeclineAttach {
                call_id: "toolu_07".into(),
            },
        ),
        (
            "command_answer_secret",
            ClientCommand::AnswerSecret {
                call_id: "toolu_07".into(),
                value: Secret::new("CANARY-secret-0001"),
            },
        ),
    ];
    for (name, command) in commands {
        golden(name, &command);
    }
}

#[test]
fn a_process_input_command_never_prints_its_input() {
    let command = ClientCommand::ProcessInput {
        id: 5,
        items: items(),
    };
    let printed = format!("{command:?}");
    assert_eq!(printed, "ProcessInput { id: 5, items: AttachItems(6) }");
    assert!(!format!("{command:#?}").contains("line one"));
    let secret = ClientCommand::AnswerSecret {
        call_id: "toolu_07".into(),
        value: Secret::new("CANARY-secret-0001"),
    };
    assert_eq!(
        format!("{secret:?}"),
        "AnswerSecret { call_id: \"toolu_07\", value: <redacted> }"
    );
    assert!(!format!("{secret:#?}").contains("CANARY"));
}

#[test]
fn a_key_with_no_terminal_code_is_refused_on_the_wire() {
    let line = json!({
        "type": "process_input",
        "id": 5,
        "items": [{"type": "key", "key": {"code": {"type": "char", "char": "]"}, "ctrl": true}}]
    });
    let refused = serde_json::from_value::<ClientCommand>(line).unwrap_err();
    assert!(
        refused.to_string().contains("the key has no terminal code"),
        "{refused}"
    );
    assert!(!refused.to_string().contains(']'), "{refused}");
}

fn refusals() -> Vec<(&'static str, RejectKind, &'static str)> {
    vec![
        ("no_process", RejectKind::NoProcess, "7"),
        ("process_ended", RejectKind::ProcessEnded, "3"),
        ("not_movable", RejectKind::NotMovable, "toolu_01"),
        ("not_a_terminal", RejectKind::NotATerminal, "3"),
        ("already_attached", RejectKind::AlreadyAttached, "3"),
        ("not_attached", RejectKind::NotAttached, "3"),
        ("input_too_large", RejectKind::InputTooLarge, "3"),
        ("no_attach_request", RejectKind::NoAttachRequest, "toolu_01"),
    ]
}

#[test]
fn every_new_reject_kind_matches_its_fixture() {
    for (name, kind, target) in refusals() {
        let reason = wisp_protocol::process_refusal(&kind, target);
        assert_eq!(serde_json::to_value(&kind).unwrap(), json!(name));
        golden(
            &format!("event_rejected_{name}"),
            &AgentEvent::Rejected {
                reason: reason.clone(),
                id: None,
                kind: Some(kind.clone()),
                request: None,
            },
        );
        golden(
            &format!("log_rejected_{name}"),
            &record(
                1,
                LogEvent::Rejected {
                    turn: None,
                    reason,
                    id: None,
                    kind: Some(kind),
                },
            ),
        );
    }
}

#[test]
fn the_process_display_types_match_their_fixtures() {
    let actions = [
        ("input", ToolAction::Input),
        ("wait", ToolAction::Wait),
        (
            "signal_interrupt",
            ToolAction::Signal {
                kind: SignalKind::Interrupt,
            },
        ),
        (
            "signal_kill",
            ToolAction::Signal {
                kind: SignalKind::Kill,
            },
        ),
        ("attach", ToolAction::Attach),
    ];
    for (name, action) in actions {
        golden(
            &format!("tool_display_{name}"),
            &ToolDisplay {
                action,
                verbs: None,
                target: "3 · npm run dev".into(),
                output: OutputKind::Text,
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            },
        );
    }
    golden("fact_process", &Fact::Process { id: 3 });
    golden(
        "tool_outcome_backgrounded",
        &ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started,
        },
    );
    golden("term_size", &SIZE);
    for reason in [
        BackgroundReason::Started,
        BackgroundReason::Quiet,
        BackgroundReason::MovedByUser,
        BackgroundReason::StillRunning,
    ] {
        let outcome = ToolOutcome::Backgrounded { reason };
        assert!(!outcome.is_error());
        let back: ToolOutcome =
            serde_json::from_str(&serde_json::to_string(&outcome).unwrap()).unwrap();
        assert_eq!(back, outcome);
    }
}

#[test]
fn a_backgrounded_outcome_is_parsed_strictly() {
    let ok = json!({"status": "backgrounded", "reason": "moved_by_user"});
    assert_eq!(
        serde_json::from_value::<ToolOutcome>(ok).unwrap(),
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::MovedByUser
        }
    );
    assert!(
        serde_json::from_value::<ToolOutcome>(
            json!({"status": "backgrounded", "reason": "timeout"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<ToolOutcome>(json!({"status": "backgrounded"})).is_err());
    assert_eq!(
        serde_json::from_value::<ToolOutcome>(
            json!({"status": "backgrounded", "reason": "still_running"})
        )
        .unwrap(),
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::StillRunning
        }
    );
}

#[test]
fn the_process_log_records_match_their_fixtures() {
    let records = [
        (
            "log_process_started",
            LogEvent::ProcessStarted {
                turn: 2,
                id: 5,
                call_id: "toolu_05".into(),
                reason: BackgroundReason::Quiet,
                tty: true,
                stdin: true,
                size: Some(SIZE),
                file: Some("/tmp/wisp-s1/0012-bash.out".into()),
                pid: Some(48_211),
                sid: Some(48_211),
                start_ticks: Some(9_120_334),
                boot_id: Some("8f1e5c7a-2d4b-4e1f-9a3c-6b7d8e9f0a1b".into()),
                keeps_running: false,
                log: None,
            },
        ),
        (
            "log_process_started_pipe",
            LogEvent::ProcessStarted {
                turn: 2,
                id: 3,
                call_id: "toolu_03".into(),
                reason: BackgroundReason::Started,
                tty: false,
                stdin: true,
                size: None,
                file: None,
                pid: None,
                sid: None,
                start_ticks: None,
                boot_id: None,
                keeps_running: false,
                log: None,
            },
        ),
        (
            "log_process_ended",
            LogEvent::ProcessEnded {
                id: 4,
                end: ProcessEnd::Exited {
                    exit: ExitStatus::Code { code: 1 },
                },
                bytes: 98_765,
                lines: 1248,
                duration_ms: 72_000,
            },
        ),
        (
            "log_process_ended_lost",
            LogEvent::ProcessEnded {
                id: 3,
                end: ProcessEnd::Lost {
                    reason: LostReason::StoppedAtResume,
                },
                bytes: 2_048,
                lines: 40,
                duration_ms: 0,
            },
        ),
        (
            "log_process_resized",
            LogEvent::ProcessResized { id: 5, size: SIZE },
        ),
        (
            "log_process_attached",
            LogEvent::ProcessAttached { id: 5, size: SIZE },
        ),
        (
            "log_process_detached",
            LogEvent::ProcessDetached {
                id: 5,
                size: SIZE,
                reason: DetachReason::Ended,
                typed: false,
                answered: false,
            },
        ),
        (
            "log_process_heard",
            LogEvent::ProcessHeard {
                turn: 3,
                ids: vec![3, 4],
                typed: Vec::new(),
            },
        ),
        (
            "log_process_needs_input",
            LogEvent::ProcessNeedsInput {
                id: 5,
                kind: NeedsInputKind::HiddenInput,
            },
        ),
        (
            "log_process_needs_input_cleared",
            LogEvent::ProcessNeedsInputCleared { id: 5 },
        ),
        (
            "log_turn_started_wake",
            LogEvent::TurnStarted {
                ends: Vec::new(),
                turn: 4,
                origin: Some(TurnOrigin::ProcessExit),
                lead: TurnLead::from_fields(
                    None,
                    Some("Sent: Sunday 2026-10-25 12:00 UTC\n".into()),
                    None,
                    Some(
                        "<processes>\nProcess 3 (npm run dev) exited with code 1.\n</processes>\n"
                            .into(),
                    ),
                ),
                text: String::new(),
                model: Some("claude-opus-5-5".into()),
                effort: None,
                effort_marker: None,
                images: Vec::new(),
                endpoint_name: Some("env".into()),
                input_id: None,
                inputs: Vec::new(),
                cache_ttl: None,
                debug_cache: None,
                refusal_fallback: None,
                binding_controls: None,
                retry_of: None,
                kept_tools_sha256: None,
                tool_message: Vec::new(),
            },
        ),
        (
            "log_tool_call_started_movable",
            LogEvent::ToolCallStarted {
                turn: 2,
                call_id: "toolu_08".into(),
                cwd: "~/Projects/Wisp".into(),
                movable: true,
                file: None,
            },
        ),
        (
            "log_tool_call_finished_backgrounded",
            LogEvent::ToolCallFinished {
                turn: 2,
                call_id: "toolu_08".into(),
                outcome: ToolOutcome::Backgrounded {
                    reason: BackgroundReason::Started,
                },
                output: ToolOutputView::default(),
                facts: vec![Fact::Process { id: 3 }],
                state: None,
                duration_ms: 1_004,
                result: Some(
                    "Process 3 is running in the background: npm run dev\nNo output yet.".into(),
                ),
                process: Some(3),
                attachments: Vec::new(),
                notes: None,
                typed: None,
            },
        ),
        (
            "log_turn_started_wake_ends",
            LogEvent::TurnStarted {
                ends: vec![ReportedEnd {
                    id: 3,
                    end: ProcessEnd::Exited {
                        exit: ExitStatus::Code { code: 1 },
                    },
                }],
                turn: 4,
                origin: Some(TurnOrigin::ProcessExit),
                lead: TurnLead::from_fields(
                    None,
                    Some("Sent: Sunday 2026-10-25 12:00 UTC\n".into()),
                    None,
                    Some(
                        "<processes>\nProcess 3 (npm run dev) exited with code 1.\n</processes>\n"
                            .into(),
                    ),
                ),
                text: String::new(),
                model: Some("claude-opus-5-5".into()),
                effort: None,
                effort_marker: None,
                images: Vec::new(),
                endpoint_name: Some("env".into()),
                input_id: None,
                inputs: Vec::new(),
                cache_ttl: None,
                debug_cache: None,
                refusal_fallback: None,
                binding_controls: None,
                retry_of: None,
                kept_tools_sha256: None,
                tool_message: Vec::new(),
            },
        ),
        (
            "log_process_detached_answered",
            LogEvent::ProcessDetached {
                id: 5,
                size: SIZE,
                reason: DetachReason::User,
                typed: true,
                answered: true,
            },
        ),
        (
            "log_process_heard_typed",
            LogEvent::ProcessHeard {
                turn: 3,
                ids: Vec::new(),
                typed: vec![5],
            },
        ),
        (
            "log_process_started_kept",
            LogEvent::ProcessStarted {
                turn: 2,
                id: 3,
                call_id: "toolu_03".into(),
                reason: BackgroundReason::Started,
                tty: false,
                stdin: false,
                size: None,
                file: Some("/tmp/wisp-s1/0012-bash.out".into()),
                pid: Some(4_321),
                sid: Some(4_321),
                start_ticks: Some(9_120_334),
                boot_id: Some("8f1e5c7a-2d4b-4e1f-9a3c-6b7d8e9f0a1b".into()),
                keeps_running: true,
                log: Some("/tmp/wisp-s1/0012-bash.log".into()),
            },
        ),
        (
            "log_process_started_still_running",
            LogEvent::ProcessStarted {
                turn: 2,
                id: 4,
                call_id: "toolu_04".into(),
                reason: BackgroundReason::StillRunning,
                tty: false,
                stdin: false,
                size: None,
                file: Some("/tmp/wisp-s1/0004-bash.out".into()),
                pid: None,
                sid: None,
                start_ticks: None,
                boot_id: None,
                keeps_running: false,
                log: None,
            },
        ),
    ];
    for (seq, (name, event)) in records.into_iter().enumerate() {
        golden(name, &record(seq as u64, event));
    }
}

#[test]
fn a_tool_call_started_from_the_version_before_reads_as_not_movable() {
    let older = json!({
        "schema_version": LOG_SCHEMA_VERSION - 1,
        "seq": 4,
        "at": "x",
        "type": "tool_call_started",
        "turn": 1,
        "call_id": "toolu_1",
        "cwd": "~"
    });
    let parsed: LogRecord = serde_json::from_value(older.clone()).unwrap();
    assert!(matches!(
        parsed.event,
        LogEvent::ToolCallStarted { movable: false, .. }
    ));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), older);

    let event: AgentEvent = serde_json::from_value(
        json!({"type": "tool_call_started", "call_id": "toolu_1", "cwd": "~"}),
    )
    .unwrap();
    assert_eq!(event, AgentEvent::call_started("toolu_1", "~"));
}

#[test]
fn the_new_optional_fields_add_no_bytes_when_absent() {
    assert_eq!(
        serde_json::to_value(AgentEvent::turn_started(2)).unwrap(),
        json!({"type": "turn_started", "turn": 2})
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::call_started("c", "~")).unwrap(),
        json!({"type": "tool_call_started", "call_id": "c", "cwd": "~"})
    );
    let finished =
        serde_json::to_value(AgentEvent::call_finished("c", ToolOutcome::Cancelled)).unwrap();
    assert!(finished.get("process").is_none(), "{finished}");
    let older = json!({
        "schema_version": LOG_SCHEMA_VERSION - 1,
        "seq": 1,
        "at": "x",
        "type": "turn_started",
        "turn": 1,
        "text": "hi"
    });
    let parsed: LogRecord = serde_json::from_value(older.clone()).unwrap();
    assert!(matches!(
        parsed.event,
        LogEvent::TurnStarted {
            origin: None,
            ref lead,
            ..
        } if lead.is_empty()
    ));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), older);
}

const NOTICE: &str = "<processes>\nProcess 1 (make) exited with code 2.\n</processes>";

fn opening(text: &str, images: Vec<ImageRef>, inputs: Vec<LoggedInput>) -> LogEvent {
    LogEvent::TurnStarted {
        ends: Vec::new(),
        turn: 1,
        origin: None,
        lead: TurnLead::from_fields(
            Some("<environment>\n</environment>".into()),
            Some("Sent: Sunday 2026-10-25 12:00 UTC\n".into()),
            Some("Context: 3k of 1M tokens (0%).\n".into()),
            Some(NOTICE.into()),
        ),
        text: text.into(),
        model: None,
        effort: None,
        effort_marker: None,
        images,
        endpoint_name: None,
        input_id: None,
        inputs,
        cache_ttl: None,
        debug_cache: None,
        refusal_fallback: None,
        binding_controls: None,
        retry_of: None,
        kept_tools_sha256: None,
        tool_message: Vec::new(),
    }
}

fn opening_texts(events: &[LogEvent]) -> Vec<String> {
    let rebuilt = rebuild_history(events.iter(), |_| Ok("AAAA".into()))
        .unwrap()
        .messages;
    assert_eq!(rebuilt.len(), 2, "{rebuilt:?}");
    rebuilt[0]
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { .. } => "<image>".to_owned(),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn answered() -> Vec<LogEvent> {
    vec![
        LogEvent::MessageCommitted {
            turn: 1,
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "ok".into() }],
            }
            .into(),
            model: None,
            stop_reason: None,
        },
        LogEvent::ProcessHeard {
            turn: 1,
            ids: vec![1],
            typed: Vec::new(),
        },
    ]
}

#[test]
fn the_notice_rebuilds_after_the_context_note_and_before_the_input() {
    let head = [
        "<environment>\n</environment>",
        "Sent: Sunday 2026-10-25 12:00 UTC\n",
        "Context: 3k of 1M tokens (0%).\n",
        NOTICE,
    ];
    let image = ImageRef {
        sha256: "ab".repeat(32),
        media_type: ImageType::Png,
        bytes: 70,
        width: 1,
        height: 1,
    };
    let cases = [
        (opening("hi", Vec::new(), Vec::new()), vec!["hi"]),
        (opening("", vec![image], Vec::new()), vec!["<image>", ""]),
        (
            opening(
                "",
                Vec::new(),
                ["a", "b"]
                    .iter()
                    .map(|t| LoggedInput {
                        id: InputId((*t).into()),
                        text: (*t).into(),
                        images: Vec::new(),
                        note: None,
                        checklist_spans: Vec::new(),
                    })
                    .collect(),
            ),
            vec!["a", "b"],
        ),
    ];
    for (started, input) in cases {
        let events: Vec<LogEvent> = std::iter::once(started).chain(answered()).collect();
        let expected: Vec<String> = head.iter().chain(&input).map(|s| (*s).to_owned()).collect();
        assert_eq!(opening_texts(&events), expected);
    }
}

#[test]
fn a_turn_with_only_a_notice_rebuilds_with_no_empty_text_block() {
    let head: Vec<String> = [
        "<environment>\n</environment>",
        "Sent: Sunday 2026-10-25 12:00 UTC\n",
        "Context: 3k of 1M tokens (0%).\n",
        NOTICE,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let mut woken = opening("", Vec::new(), Vec::new());
    if let LogEvent::TurnStarted { origin, .. } = &mut woken {
        *origin = Some(TurnOrigin::ProcessExit);
    }
    let mut retried = opening("", Vec::new(), Vec::new());
    if let LogEvent::TurnStarted { retry_of, .. } = &mut retried {
        *retry_of = Some(1);
    }
    for started in [woken, retried] {
        let events: Vec<LogEvent> = std::iter::once(started).chain(answered()).collect();
        assert_eq!(opening_texts(&events), head);
    }
    let mut bare = opening("", Vec::new(), Vec::new());
    if let LogEvent::TurnStarted { lead, .. } = &mut bare {
        lead.entries.retain(|e| e.by != LEAD_PROCESSES);
    }
    let events: Vec<LogEvent> = std::iter::once(bare).chain(answered()).collect();
    let mut kept = head[..3].to_vec();
    kept.push(String::new());
    assert_eq!(opening_texts(&events), kept);
}
