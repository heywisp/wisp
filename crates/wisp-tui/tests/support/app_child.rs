use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wisp_protocol::{
    AgentEvent, BlockKind, CacheTtl, Effort, ExitStatus, Fact, ToolOutcome, ToolOutputView, Usage,
};
use wisp_tui::app::endpoint_card::CardSetup;
use wisp_tui::app::replay::{self, Script};
use wisp_tui::app::setup::Opening;
use wisp_tui::app::start::{Launched, launch};
use wisp_tui::app::{Credential, Session, SessionFacts};
use wisp_tui::term::Flags;

use crate::events::{End, Finished, Requested, tool_finished, tool_requested, turn_ended};
use crate::models::{models, multipliers};
use wisp_render::chrome::Endpoint;
use wisp_render::transcript::copy;

pub(crate) const BEFORE: &str = "before the app";
pub(crate) const SHORT_END: &str = "short-answer-end";
pub(crate) const PAGE_END: &str = "page-answer-end";
pub(crate) const STEPS_END: &str = "steps-answer-end";
pub(crate) const STEP_WORDS: usize = 100;
pub(crate) const STEPS_PACE: Duration = Duration::from_millis(50);
pub(crate) const AFTER_STEER: &str = "after the steer";
pub(crate) const PAGE_PARAGRAPHS: usize = 30;
pub(crate) const LONG_WORDS: usize = 3000;
pub(crate) const LONG_PARAGRAPH: usize = 20;
pub(crate) const LONG_SEEN: usize = 100;
pub(crate) const PANIC_AFTER: usize = 300;
pub(crate) const OUTPUT_TOKENS: u64 = 4100;
pub(crate) const MODEL: &str = "pty-model";
pub(crate) const PICKER_MODEL: &str = "claude-opus-5-5";
pub(crate) const LOG_TAIL: &str = ".local/share/wisp/sessions/pty-session.jsonl";
pub(crate) const PANIC_MESSAGE: &str = "app-boom";
pub(crate) const EXITED: &str = "app-exit";
pub(crate) const APP_ENDED: &str = "WISP_TUI_PTY_APP_ENDED";
pub(crate) const PANIC_FILE: &str = "WISP_TUI_PTY_PANIC_FILE";
pub(crate) const HOLD_MS: &str = "WISP_TUI_PTY_HOLD_MS";
pub(crate) const FOLD_END: &str = "fold-answer-end";
pub(crate) const FOLD_THOUGHT: &str = "Thought for";
pub(crate) const LIVE_FILE: &str = "WISP_TUI_PTY_LIVE_FILE";
pub(crate) const LIVE_ROW: &str = "npm run live";
pub(crate) const LIVE_END: &str = "live-answer-end";

pub(crate) fn placeholder() -> &'static str {
    &copy::PLACEHOLDER[..copy::PLACEHOLDER_SHORT.len()]
}

fn completed() -> AgentEvent {
    turn_ended(End {
        model: MODEL.to_owned(),
        usage: Some(Usage {
            output_tokens: OUTPUT_TOKENS,
            ..Usage::default()
        }),
        ..End::default()
    })
}

fn turn(deltas: Vec<String>) -> Vec<AgentEvent> {
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
    ];
    events.extend(
        deltas
            .into_iter()
            .map(|text| AgentEvent::TextDelta { index: 0, text }),
    );
    events.push(AgentEvent::BlockStopped { index: 0 });
    events.push(completed());
    events
}

fn stepped() -> Vec<AgentEvent> {
    let text = |index: usize, deltas: Vec<String>| {
        let mut events = vec![AgentEvent::BlockStarted {
            index,
            kind: BlockKind::Text,
        }];
        events.extend(
            deltas
                .into_iter()
                .map(|text| AgentEvent::TextDelta { index, text }),
        );
        events.push(AgentEvent::BlockStopped { index });
        events
    };
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
    ];
    events.extend(text(0, (0..STEP_WORDS).map(|i| format!("s{i} ")).collect()));
    events.push(AgentEvent::StepStarted { step: 2 });
    events.extend(text(0, vec![format!("{AFTER_STEER}.\n\n{STEPS_END}")]));
    events.push(completed());
    events
}

fn folded() -> Vec<AgentEvent> {
    let thought: String = (1..=10)
        .map(|i| format!("Thought line {i} about the fold.\n"))
        .collect();
    let output: String = (1..=41).map(|i| format!("test {i} ... ok\n")).collect();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: thought,
        },
        AgentEvent::BlockStopped { index: 0 },
        tool_requested(Requested {
            block: 1,
            call_id: "fold".to_owned(),
            input: serde_json::json!({"command": "cargo test"}),
            target: "cargo test".to_owned(),
            ..Requested::default()
        }),
        AgentEvent::call_started("fold", "~/pty"),
        tool_finished(Finished {
            call_id: "fold".to_owned(),
            outcome: ToolOutcome::Completed {
                is_error: false,
                exit: Some(ExitStatus::Code { code: 0 }),
            },
            output: ToolOutputView {
                bytes: output.len() as u64,
                tail: output,
                lines: 41,
                file: None,
                file_problem: None,
                inlined: true,
            },
            facts: vec![Fact::Exit { code: 0 }],
            duration_ms: 1_000,
            ..Finished::default()
        }),
        AgentEvent::BlockStarted {
            index: 2,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 2,
            text: format!("Both folded.\n\n{FOLD_END}"),
        },
        AgentEvent::BlockStopped { index: 2 },
    ];
    events.push(completed());
    events
}

fn live() -> Vec<AgentEvent> {
    let file = std::env::var(LIVE_FILE).ok();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        tool_requested(Requested {
            call_id: "live".to_owned(),
            input: serde_json::json!({"command": LIVE_ROW}),
            target: LIVE_ROW.to_owned(),
            ..Requested::default()
        }),
        AgentEvent::ToolCallStarted {
            call_id: "live".to_owned(),
            cwd: "~/pty".to_owned(),
            movable: false,
            file: file.clone(),
        },
        tool_finished(Finished {
            call_id: "live".to_owned(),
            outcome: ToolOutcome::Backgrounded {
                reason: wisp_protocol::BackgroundReason::Started,
            },
            output: ToolOutputView {
                bytes: 0,
                tail: String::new(),
                lines: 0,
                file,
                file_problem: None,
                inlined: false,
            },
            facts: vec![Fact::Process { id: 4 }],
            duration_ms: 100,
            process: Some(4),
            ..Finished::default()
        }),
        AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 1,
            text: format!("It runs.\n\n{LIVE_END}"),
        },
        AgentEvent::BlockStopped { index: 1 },
    ];
    events.push(completed());
    events
}

pub(crate) fn script(name: &str) -> Vec<AgentEvent> {
    match name {
        "fold" => folded(),
        "live" => live(),
        "steps" => stepped(),
        "short" | "pickers" => turn(vec![format!("A short answer.\n\n{SHORT_END}")]),
        "page" => {
            let mut deltas: Vec<String> = (0..PAGE_PARAGRAPHS)
                .map(|p| format!("Paragraph p{p} of a page that is taller than the screen.\n\n"))
                .collect();
            deltas.push(PAGE_END.to_owned());
            turn(deltas)
        }
        _ => turn(
            (0..LONG_WORDS)
                .map(|i| {
                    if i % LONG_PARAGRAPH == LONG_PARAGRAPH - 1 {
                        format!("w{i}\n\n")
                    } else {
                        format!("w{i} ")
                    }
                })
                .collect(),
        ),
    }
}

pub(crate) fn log_path() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    Some(format!("{}/{LOG_TAIL}", home.trim_end_matches('/')))
}

pub(crate) fn run(role: &str) {
    assert_eq!(
        wisp_tui::term::host::wsl_root().as_path(),
        std::path::Path::new(crate::term_env::ISOLATED_WSL_ROOT),
    );
    if role.starts_with("app:long:") {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            std::thread::sleep(Duration::from_millis(250));
            previous(info);
        }));
    }
    let mut parts = role.split(':').skip(1);
    let name = parts.next().unwrap_or("short").to_owned();
    let panics: usize = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    if let Some(ms) = std::env::var(HOLD_MS).ok().and_then(|ms| ms.parse().ok()) {
        wisp_tui::term::hold_deaths(Duration::from_millis(ms)).ok();
    }
    println!("{BEFORE}");
    std::io::stdout().flush().ok();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok();
    let Some(runtime) = runtime else {
        return;
    };
    let model = if name == "pickers" {
        PICKER_MODEL
    } else {
        MODEL
    };
    let launched = runtime.block_on(async move {
        let settings = replay::Settings {
            model: model.to_owned(),
            effort: Effort::Low,
            ..replay::Settings::default()
        };
        let script = Script::new(script(&name)).with_settings(settings);
        let pace = if name == "steps" {
            STEPS_PACE
        } else {
            replay::PACE
        };
        let (commands, mut agent) = replay::spawn(script, pace);
        let (forward, events) = tokio::sync::mpsc::channel(256);
        let streamed = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&streamed);
        tokio::spawn(async move {
            while let Some(event) = agent.recv().await {
                if matches!(event, AgentEvent::TextDelta { .. }) {
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                if forward.send(event).await.is_err() {
                    return;
                }
            }
        });
        let signal = std::env::var(PANIC_FILE).ok();
        if panics > 0 {
            std::thread::spawn(move || {
                match &signal {
                    Some(path) => {
                        while !std::path::Path::new(path).exists() {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    None => {
                        while streamed.load(Ordering::SeqCst) < PANIC_AFTER {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                }
                for _ in 0..panics {
                    std::thread::spawn(|| std::panic::panic_any(PANIC_MESSAGE))
                        .join()
                        .ok();
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
        }
        let session = Session {
            leave_note: None,
            attached: None,
            alerts: Vec::new(),
            facts: SessionFacts {
                version: "0.0.0".to_owned(),
                provider: "anthropic".to_owned(),
                model: model.to_owned(),
                effort: "low".to_owned(),
                cache_ttl: CacheTtl::OneHour,
                credential: Some(Credential::ApiKey),
                host: None,
                log: log_path(),
                home: std::env::var("HOME").ok(),
                cwd: "~/pty".to_owned(),
                endpoint: Endpoint::Official,
                models: models(),
                on_accounts: Vec::new(),
                write_multipliers: multipliers(2.0),
                next: None,
                max_tokens: 64_000,
                endpoint_name: None,
                endpoints: Vec::new(),
                endpoint_note: None,
                resume_refused: false,
                instructions: Default::default(),
                search: Default::default(),
                providers_off: Default::default(),
                resumed: None,
                claude_usage: false,
                start_pick: None,
            },
            events,
            commands,
            resumed: None,
        };
        launch(
            Opening::Session {
                session: Box::new(session),
                card: CardSetup::default(),
                notes: Vec::new(),
                started: None,
            },
            Flags::default(),
            &wisp_tui::term::SavedDisplay::default(),
            false,
            None,
            None,
        )
        .await
    });
    wisp_tui::term::die_if_dying();
    let Launched { exit, goodbye, .. } = launched;
    if let Ok(path) = std::env::var(APP_ENDED) {
        std::fs::write(path, format!("{exit:?}")).ok();
    }
    let mut out = std::io::stdout();
    drop(out.write_all(&goodbye));
    drop(writeln!(out, "{EXITED} {exit:?}"));
}
