use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::plugin::HookDisabled;
use wisp_protocol::{AgentEvent, BlockKind, ClientCommand, ExitStatus, ProcessEnd, TurnOrigin};
use wisp_render::chrome::endpoint_card;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Theme, Token};
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::pager::back_keys;
use wisp_tui::app::{Options, replay};
use wisp_tui::term::{Input, KeyCode, Mouse, MouseKind};

use super::images::{NO_LEAKS, PASTED, leaks, paste_every_way, pasting, seeing};
use super::sidebar::snapshot;
use super::steering::{Agent, position, steering_facts, steering_on};
use super::{Screen, facts, key, options, session_with};
use crate::Drive;
use crate::events::{Requested, tool_requested};
use crate::shared::{catalog, hook_entry, plugin};

const FIXTURE: &str = include_str!("../fixtures/processes.jsonl");

fn listing() -> Options {
    let mut options = options(Start::Stalled);
    options.open_processes = true;
    options
}

#[tokio::test(start_paused = true)]
async fn ps_at_40_80_and_120_in_unicode_and_ascii() {
    for (glyphs, set) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
        for width in [40u16, 80, 120] {
            let events = replay::parse(FIXTURE).unwrap();
            let screen = Screen::new(24, width);
            let mut options = listing();
            options.glyphs = glyphs;
            let name = format!("ps-dark-truecolor-{set}-{width}");
            session_with(screen.clone(), vec![events], options, |run| async move {
                run.say("start the dev server");
                sleep(Duration::from_secs(3)).await;
                let mut shots = vec![("listed".to_owned(), run.screen.styled())];
                run.send(key(KeyCode::Esc));
                sleep(Duration::from_millis(100)).await;
                shots.push(("idle-several".to_owned(), run.screen.styled()));
                let status = super::status_line(&run.screen.visible());
                assert!(status.contains("continues when"), "{width}: {status}");
                if width >= 80 {
                    assert!(status.contains("2 running"), "{width}: {status}");
                    assert!(status.contains(copy::PS_HINT), "{width}: {status}");
                }
                run.say("/ps");
                sleep(Duration::from_millis(100)).await;
                run.send(key(KeyCode::Down));
                sleep(Duration::from_millis(100)).await;
                shots.push(("moved".to_owned(), run.screen.styled()));
                run.send(key(KeyCode::Up));
                run.send(key(KeyCode::Char('x')));
                run.send(key(KeyCode::Char('x')));
                sleep(Duration::from_millis(200)).await;
                shots.push(("stopped".to_owned(), run.screen.styled()));
                run.send(key(KeyCode::Esc));
                sleep(Duration::from_millis(100)).await;
                shots.push(("idle-one".to_owned(), run.screen.styled()));
                let status = super::status_line(&run.screen.visible());
                assert!(status.contains("continues when"), "{width}: {status}");
                if width >= 80 {
                    assert!(status.contains("1 running"), "{width}: {status}");
                    assert!(status.contains(copy::PS_HINT), "{width}: {status}");
                }
                run.say("/ps");
                run.send(key(KeyCode::Char('x')));
                run.send(key(KeyCode::Char('x')));
                sleep(Duration::from_millis(200)).await;
                run.send(key(KeyCode::Esc));
                sleep(Duration::from_secs(3)).await;
                shots.push(("idle-ready".to_owned(), run.screen.styled()));
                let status = super::status_line(&run.screen.visible());
                assert!(status.contains("Ready"), "{width}: {status}");
                assert!(!status.contains("continues when"), "{width}: {status}");
                snapshot(&name, &shots);
                run.quit();
            })
            .await
            .unwrap();
        }
    }
}

pub(crate) fn line_with<'a>(lines: &'a [String], needle: &str) -> &'a str {
    lines
        .iter()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in {lines:#?}"))
}

#[tokio::test(start_paused = true)]
async fn the_processes_fixture_lists_opens_and_stops_its_processes() {
    let events = replay::parse(FIXTURE).unwrap();
    let screen = Screen::new(30, 90);
    session_with(screen.clone(), vec![events], listing(), |run| async move {
        sleep(Duration::from_millis(100)).await;
        let lines = run.screen.visible();
        assert!(!lines.iter().any(|l| l.contains("Processes")), "{lines:#?}");
        run.say("start the dev server");
        sleep(Duration::from_secs(3)).await;
        let lines = run.screen.visible();
        let dev = line_with(&lines, "3  Preview site  npm run dev");
        assert!(dev.contains("running"), "{dev}");
        let py = line_with(&lines, "5  Python shell  python3 -i");
        assert!(py.contains("terminal 40x6"), "{py}");
        line_with(
            &lines,
            "enter open \u{b7} x stop \u{b7} \u{2191}\u{2193} move",
        );
        run.send(key(KeyCode::Down));
        run.send(key(KeyCode::Enter));
        sleep(Duration::from_millis(100)).await;
        let lines = run.screen.visible();
        line_with(&lines, "Process 5  python3 -i");
        line_with(&lines, "Python 3.12.3");
        line_with(&lines, " >>>");
        run.send(key(KeyCode::Esc));
        run.send(key(KeyCode::Up));
        run.send(key(KeyCode::Char('x')));
        sleep(Duration::from_millis(100)).await;
        let lines = run.screen.visible();
        line_with(&lines, &copy::press_x_again(3));
        assert!(line_with(&lines, "3  Preview site  npm run dev").contains("running"));
        run.send(key(KeyCode::Char('x')));
        sleep(Duration::from_millis(200)).await;
        let lines = run.screen.visible();
        assert!(
            line_with(&lines, "3  Preview site  npm run dev").contains("stopped by you"),
            "{lines:#?}"
        );
        run.send(key(KeyCode::Up));
        sleep(Duration::from_millis(100)).await;
        assert!(
            line_with(&run.screen.visible(), "5  Python shell  python3 -i")
                .trim_start()
                .starts_with('›')
        );
        run.send(key(KeyCode::Down));
        run.send(key(KeyCode::Enter));
        sleep(Duration::from_millis(200)).await;
        let lines = run.screen.visible();
        line_with(&lines, &back_keys().text());
        assert!(lines[0].contains("Preview site"), "{lines:#?}");
        line_with(&lines, "process 3 \u{b7} stopped by you");
        run.send(key(KeyCode::Esc));
        sleep(Duration::from_millis(200)).await;
        let lines = run.screen.visible();
        assert!(
            !lines.iter().any(|l| l.contains(&back_keys().text())),
            "{lines:#?}"
        );
        assert!(!run.screen.in_pager());
        line_with(&run.screen.visible(), "3  Preview site  npm run dev");
        run.send(key(KeyCode::Esc));
        sleep(Duration::from_millis(100)).await;
        let lines = run.screen.visible();
        let head = line_with(&lines, "Preview site  npm run dev");
        assert!(head.trim_start().starts_with('\u{25E6}'), "{head}");
        assert!(
            head.trim_end().ends_with("process 3 \u{b7} stopped by you"),
            "{head}"
        );
        let live = line_with(&lines, "Python shell  python3 -i");
        assert!(live.contains("\u{21D7}"), "{live}");
        assert!(
            live.trim_end().ends_with("process 5 \u{b7} running"),
            "{live}"
        );
        let status = super::status_line(&lines);
        assert!(status.contains("1 running"), "{status}");
        assert!(status.contains(copy::PS_HINT), "{status}");
        assert!(!lines.iter().any(|l| l.contains("Processes")), "{lines:#?}");
        run.quit();
    })
    .await
    .unwrap();
}

async fn run_call(a: &Agent, id: &str, movable: bool) {
    a.emit(AgentEvent::BlockStarted {
        index: 1,
        kind: BlockKind::ToolUse {
            writing: None,
            name: "shell".to_owned(),
        },
    })
    .await;
    a.emit(tool_requested(Requested {
        block: 1,
        call_id: id.to_owned(),
        target: "cargo build".to_owned(),
        ..Requested::default()
    }))
    .await;
    a.emit(AgentEvent::ToolCallStarted {
        call_id: id.to_owned(),
        cwd: "~/demo".to_owned(),
        movable,
        file: None,
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn ctrl_b_backgrounds_only_a_movable_call_and_moves_the_cursor_otherwise() {
    steering_on(
        Screen::new(24, 90),
        options(Start::Stalled),
        |mut a| async move {
            a.start("build it", 1).await;
            run_call(&a, "toolu_fixed", false).await;
            a.paste("ab").ctrl('b');
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(
                !lines
                    .iter()
                    .any(|l| l.contains(&copy::background_key().text())),
                "{lines:#?}"
            );
            a.emit(AgentEvent::call_finished(
                "toolu_fixed",
                wisp_protocol::ToolOutcome::Completed {
                    exit: None,
                    is_error: false,
                },
            ))
            .await;
            run_call(&a, "toolu_move", true).await;
            let lines = a.settle().await;
            assert!(
                super::status_line(&lines).contains(&copy::background_key().text()),
                "{lines:#?}"
            );
            a.ctrl('b');
            assert_eq!(
                a.next().await,
                ClientCommand::Background {
                    call_id: "toolu_move".to_owned()
                }
            );
            a.emit(AgentEvent::call_finished(
                "toolu_move",
                wisp_protocol::ToolOutcome::Backgrounded {
                    reason: wisp_protocol::BackgroundReason::MovedByUser,
                },
            ))
            .await;
            a.ctrl('b');
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(
                !super::status_line(&lines).contains(&copy::background_key().text()),
                "{lines:#?}"
            );
            a.emit(super::steering::cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

pub(super) fn started(id: u32) -> AgentEvent {
    AgentEvent::ProcessStarted {
        id,
        call_id: format!("toolu_{id}"),
        target: "npm run dev".to_owned(),
        tty: false,
        stdin: true,
        size: None,
        file: None,
        reason: wisp_protocol::BackgroundReason::Started,
        log: None,
        keeps_running: false,
        pid: None,
    }
}

pub(super) fn exited(id: u32, code: i32) -> AgentEvent {
    AgentEvent::ProcessEnded {
        id,
        end: ProcessEnd::Exited {
            exit: ExitStatus::Code { code },
        },
        bytes: 0,
        lines: 0,
        duration_ms: 1000,
    }
}

#[tokio::test(start_paused = true)]
async fn an_exit_while_idle_flashes_once_and_a_wake_turn_says_why_it_started() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            a.start("start the servers", 1).await;
            a.emit(started(3)).await;
            let mut server = started(5);
            if let AgentEvent::ProcessStarted { keeps_running, .. } = &mut server {
                *keeps_running = true;
            }
            a.emit(server).await;
            a.emit(super::completed()).await;
            sleep(Duration::from_secs(3)).await;
            let lines = a.settle().await;
            let status = super::status_line(&lines);
            assert!(
                status.contains("Ready · agent continues when 1 of 2 processes ends"),
                "{status}"
            );
            assert!(status.contains("2 running"), "{status}");
            let mut shell = plugin("shell", true, vec![]);
            for (settings, off) in [(false, true), (true, false), (true, true), (false, false)] {
                shell.hooks = vec![hook_entry(
                    "processes",
                    "before-turn",
                    0,
                    None,
                    off.then_some(HookDisabled::SwitchedOff),
                )];
                let event = if settings {
                    let mut event = super::described(0);
                    if let AgentEvent::SettingsSnapshot(snapshot) = &mut event {
                        snapshot.plugins = vec![shell.clone()];
                    }
                    event
                } else {
                    catalog(vec![shell.clone()])
                };
                a.emit(event).await;
                let status = super::status_line(&a.settle().await);
                assert!(status.contains("Ready"), "{status}");
                assert_eq!(status.contains("agent continues"), !off, "{status}");
                assert!(status.contains("2 running"), "{status}");
                assert!(status.contains(copy::PS_HINT), "{status}");
            }
            a.emit(exited(5, 0)).await;
            a.settle().await;
            a.key(KeyCode::Left);
            let status = super::status_line(&a.settle().await);
            assert!(
                status.contains("Ready · agent continues when process 3 ends"),
                "{status}"
            );
            assert!(status.contains("1 running"), "{status}");
            a.emit(exited(3, 1)).await;
            let lines = a.settle().await;
            assert!(
                super::status_line(&lines).contains("process 3 exited (1)"),
                "{lines:#?}"
            );
            assert!(
                !super::status_line(&lines).contains("running"),
                "{lines:#?}"
            );
            for cols in [80, 100] {
                a.input(Input::Resize(a.screen.resize(24, cols)));
                let status = super::status_line(&a.settle().await);
                assert!(status.contains("process 3 exited (1)"), "{status}");
                assert!(!status.contains("agent continues"), "{status}");
            }
            a.key(KeyCode::Left);
            let status = super::status_line(&a.settle().await);
            assert!(status.contains("Ready"), "{status}");
            assert!(!status.contains("agent continues"), "{status}");
            a.emit(AgentEvent::TurnStarted {
                turn: 2,
                origin: Some(TurnOrigin::ProcessExit),
                retry_of: None,
                answers: None,
                ends: vec![wisp_protocol::ReportedEnd {
                    id: 3,
                    end: ProcessEnd::Exited {
                        exit: ExitStatus::Code { code: 1 },
                    },
                }],
            })
            .await;
            let lines = a.settle().await;
            line_with(&lines, "\u{21BB} process 3 exited");
            a.emit(super::steering::cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

pub(super) fn terminal(id: u32) -> AgentEvent {
    AgentEvent::ProcessStarted {
        id,
        call_id: format!("toolu_{id}"),
        target: "python3 -i".to_owned(),
        tty: true,
        stdin: true,
        size: Some(wisp_protocol::TermSize { cols: 40, rows: 6 }),
        file: None,
        reason: wisp_protocol::BackgroundReason::Started,
        log: None,
        keeps_running: false,
        pid: None,
    }
}

#[tokio::test(start_paused = true)]
async fn an_exit_while_wisp_works_shows_no_flash() {
    steering_on(
        Screen::new(24, 90),
        options(Start::Stalled),
        |mut a| async move {
            a.start("build it", 1).await;
            a.emit(started(3)).await;
            a.emit(exited(3, 1)).await;
            let lines = a.settle().await;
            assert!(
                !lines.iter().any(|l| l.contains("process 3 exited (1)")),
                "{lines:#?}"
            );
            a.emit(super::steering::cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_running_count_hides_while_the_list_is_open() {
    steering_on(Screen::new(24, 90), listing(), |a| async move {
        a.emit(started(3)).await;
        let lines = a.settle().await;
        line_with(&lines, "3  npm run dev");
        let status = super::status_line(&lines);
        assert!(!status.contains("1 running"), "{status}");
        assert!(!status.contains(copy::PS_HINT), "{status}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        let status = super::status_line(&lines);
        assert!(status.contains("1 running"), "{status}");
        assert!(status.contains(copy::PS_HINT), "{status}");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn another_key_resets_the_stop_confirmation() {
    steering_on(Screen::new(24, 90), listing(), |mut a| async move {
        a.emit(started(3)).await;
        a.settle().await;
        a.key(KeyCode::Char('x'));
        let lines = a.settle().await;
        line_with(&lines, &copy::press_x_again(3));
        a.key(KeyCode::Down);
        let lines = a.settle().await;
        assert!(
            !lines.iter().any(|l| l.contains(&copy::press_x_again(3))),
            "{lines:#?}"
        );
        a.key(KeyCode::Char('x'));
        a.nothing_sent().await;
        line_with(&a.settle().await, &copy::press_x_again(3));
        a.key(KeyCode::Char('x'));
        assert_eq!(a.next().await, ClientCommand::StopProcess { id: 3 });
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_closes_the_screen_view_back_to_the_list() {
    steering_on(Screen::new(24, 90), listing(), |mut a| async move {
        a.emit(terminal(5)).await;
        a.settle().await;
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        line_with(&lines, "Process 5  python3 -i");
        a.key(KeyCode::Char('a'));
        assert_eq!(
            a.next().await,
            ClientCommand::AttachProcess { id: 5, size: None }
        );
        a.settle().await;
        assert!(matches!(
            a.next().await,
            ClientCommand::AttachProcess { .. }
        ));
        a.ctrl(']');
        assert_eq!(a.next().await, ClientCommand::DetachProcess { id: 5 });
        let lines = a.settle().await;
        line_with(&lines, "Process 5  python3 -i");
        a.ctrl('c');
        let lines = a.settle().await;
        assert!(
            !lines.iter().any(|l| l.contains("Process 5  python3 -i")),
            "{lines:#?}"
        );
        line_with(&lines, "5  python3 -i");
        a.nothing_sent().await;
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_turn_that_ends_clears_the_call_ctrl_b_would_move() {
    steering_on(
        Screen::new(24, 90),
        options(Start::Stalled),
        |mut a| async move {
            a.start("build it", 1).await;
            run_call(&a, "toolu_move", true).await;
            let lines = a.settle().await;
            assert!(
                super::status_line(&lines).contains(&copy::background_key().text()),
                "{lines:#?}"
            );
            a.emit(super::steering::cancelled()).await;
            let lines = a.settle().await;
            assert!(
                !super::status_line(&lines).contains(&copy::background_key().text()),
                "{lines:#?}"
            );
            a.ctrl('b');
            a.nothing_sent().await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

const INSTALL: &str = "Install the package";

pub(super) fn described_start(id: u32) -> AgentEvent {
    tool_requested(Requested {
        call_id: format!("toolu_{id}"),
        input: serde_json::json!({"description": INSTALL}),
        target: "sudo apt install x".to_owned(),
        title: Some(INSTALL.to_owned()),
        ..Requested::default()
    })
}

pub(super) fn waiting_terminal(id: u32) -> AgentEvent {
    AgentEvent::ProcessStarted {
        id,
        call_id: format!("toolu_{id}"),
        target: "sudo apt install x".to_owned(),
        tty: true,
        stdin: false,
        size: Some(wisp_protocol::TermSize {
            cols: 100,
            rows: 30,
        }),
        file: None,
        reason: wisp_protocol::BackgroundReason::Quiet,
        log: None,
        keeps_running: false,
        pid: None,
    }
}

fn items_of(command: ClientCommand) -> (u32, Vec<wisp_protocol::AttachInput>) {
    match command {
        ClientCommand::ProcessInput { id, items } => (id, items.0),
        other => panic!("expected ProcessInput, got {other:?}"),
    }
}

async fn took_over(a: &mut Agent, id: u32) -> wisp_protocol::TermSize {
    assert_eq!(
        a.next().await,
        ClientCommand::AttachProcess { id, size: None }
    );
    a.settle().await;
    let size = match a.next().await {
        ClientCommand::AttachProcess {
            size: Some(size), ..
        } => size,
        other => panic!("expected the view's size, got {other:?}"),
    };
    a.emit(AgentEvent::ProcessAttached { id, size }).await;
    size
}

#[tokio::test(start_paused = true)]
async fn f12_takes_over_a_waiting_terminal_types_into_it_and_leaves_it() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.emit(described_start(5)).await;
            a.emit(waiting_terminal(5)).await;
            a.emit(AgentEvent::ProcessNeedsInput {
                id: 5,
                kind: wisp_protocol::NeedsInputKind::HiddenInput,
            })
            .await;
            for cols in [80, 100] {
                a.input(Input::Resize(a.screen.resize(24, cols)));
                let status = super::status_line(&a.settle().await);
                assert!(
                    status.contains(&format!(
                        "{} \u{b7} f12 to take over",
                        copy::waiting_for_password(5)
                    )),
                    "{status}"
                );
                assert!(!status.contains("agent continues"), "{status}");
            }
            a.key(KeyCode::F(12));
            let size = took_over(&mut a, 5).await;
            let lines = a.settle().await;
            let title = line_with(&lines, "Process 5  ");
            assert!(title.trim_end().ends_with("f12 to go back"), "{title}");
            a.emit(AgentEvent::Rejected {
                reason: "/w has ended".to_owned(),
                id: None,
                kind: Some(wisp_protocol::RejectKind::Other(
                    wisp_protocol::agents::NO_SUCH_AGENT.to_owned(),
                )),
                request: None,
            })
            .await;
            let lines = a.settle().await;
            line_with(&lines, copy::ATTACH_WARNING);
            assert!(
                super::status_line(&lines)
                    .contains(&format!("{} \u{b7} f12 to go back", copy::typing_into(5))),
                "{lines:#?}"
            );
            a.emit(AgentEvent::QuestionAsked {
                call_id: "q1".to_owned(),
                questions: vec![super::questions::database()],
            })
            .await;
            sleep(Duration::from_millis(600)).await;
            let lines = a.settle().await;
            line_with(&lines, "Process 5  ");
            assert!(
                !lines.iter().any(|line| line.contains("Which database")),
                "{lines:?}"
            );
            a.key(KeyCode::Char('p'));
            let (id, items) = items_of(a.next().await);
            assert_eq!(id, 5);
            assert_eq!(
                items,
                [wisp_protocol::AttachInput::Text { text: "p".into() }]
            );
            a.ctrl('c');
            let (_, items) = items_of(a.next().await);
            assert!(
                matches!(&items[..], [wisp_protocol::AttachInput::Key { .. }]),
                "{items:?}"
            );
            a.paste("line one\nline two");
            let (_, items) = items_of(a.next().await);
            assert_eq!(
                items,
                [wisp_protocol::AttachInput::Paste {
                    text: "line one\nline two".into()
                }]
            );
            a.key(KeyCode::F(12));
            assert_eq!(a.next().await, ClientCommand::DetachProcess { id: 5 });
            a.emit(AgentEvent::ProcessDetached {
                id: 5,
                size,
                reason: wisp_protocol::DetachReason::User,
                typed: true,
            })
            .await;
            let lines = a.settle().await;
            line_with(&lines, &copy::typed_into_about(5, INSTALL, " \u{b7} "));
            assert!(
                !lines.iter().any(|l| l.contains("Process 5  ")),
                "{lines:#?}"
            );
            line_with(&lines, "Which database");
            a.key(KeyCode::Char('2'));
            a.nothing_sent().await;
            a.key(KeyCode::Backspace);
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Char('2'));
            let ClientCommand::Answer { answers, .. } = a.next().await else {
                panic!("expected answer");
            };
            assert_eq!(answers, vec![wisp_protocol::Answer::choose(1)]);
            a.emit(AgentEvent::QuestionClosed {
                call_id: "q1".to_owned(),
                how: wisp_protocol::QuestionEnd::Answered { answers },
            })
            .await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_on_a_tool_row_while_typing_into_a_process_opens_nothing_and_keys_reach_the_process()
 {
    steering_on(
        Screen::new(50, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.start("build it", 1).await;
            run_call(&a, "toolu_long", false).await;
            let tail: String = (1..=40).map(|i| format!("line {i}\n")).collect();
            a.emit(AgentEvent::ToolOutput {
                call_id: "toolu_long".to_owned(),
                text: tail.clone(),
            })
            .await;
            let mut finished = AgentEvent::call_finished(
                "toolu_long",
                wisp_protocol::ToolOutcome::Completed {
                    exit: Some(ExitStatus::Code { code: 0 }),
                    is_error: false,
                },
            );
            if let AgentEvent::ToolCallFinished { output, .. } = &mut finished {
                output.bytes = tail.len() as u64;
                output.lines = 40;
                output.tail = tail;
                output.inlined = true;
            }
            a.emit(finished).await;
            a.emit(waiting_terminal(5)).await;
            a.emit(AgentEvent::ProcessNeedsInput {
                id: 5,
                kind: wisp_protocol::NeedsInputKind::HiddenInput,
            })
            .await;
            let lines = a.settle().await;
            let (row, line) = lines
                .iter()
                .enumerate()
                .find(|(_, l)| l.contains("Ran cargo build"))
                .unwrap_or_else(|| panic!("no tool row: {lines:#?}"));
            assert!(
                !lines.iter().any(|l| l.contains("more lines")),
                "{lines:#?}"
            );
            let spot = Mouse {
                kind: MouseKind::Press,
                column: u16::try_from(line[..line.find("Ran").unwrap()].chars().count() + 1)
                    .unwrap(),
                row: u16::try_from(row).unwrap(),
                modified: false,
            };
            a.ctrl(']');
            let size = took_over(&mut a, 5).await;
            a.settle().await;
            a.inputs.send(Input::Mouse(spot)).unwrap();
            a.inputs
                .send(Input::Mouse(Mouse {
                    kind: MouseKind::Release,
                    ..spot
                }))
                .unwrap();
            sleep(Duration::from_millis(600)).await;
            let lines = a.settle().await;
            assert!(
                !lines.iter().any(|l| l.contains(&back_keys().text())),
                "{lines:#?}"
            );
            a.key(KeyCode::Char('q'));
            let (id, items) = items_of(a.next().await);
            assert_eq!(id, 5);
            assert_eq!(
                items,
                [wisp_protocol::AttachInput::Text { text: "q".into() }]
            );
            a.ctrl(']');
            assert_eq!(a.next().await, ClientCommand::DetachProcess { id: 5 });
            a.emit(AgentEvent::ProcessDetached {
                id: 5,
                size,
                reason: wisp_protocol::DetachReason::User,
                typed: true,
            })
            .await;
            a.settle().await;
            a.emit(super::steering::cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

pub(super) fn asked(id: u32) -> AgentEvent {
    AgentEvent::AttachRequested {
        call_id: "toolu_ask".to_owned(),
        id,
        reason: "sudo password".to_owned(),
    }
}

pub(super) fn ask_closed() -> AgentEvent {
    AgentEvent::AttachRequestClosed {
        call_id: "toolu_ask".to_owned(),
        how: wisp_protocol::AttachRequestEnd::Declined,
    }
}

fn decline() -> ClientCommand {
    ClientCommand::DeclineAttach {
        call_id: "toolu_ask".to_owned(),
    }
}

pub(super) async fn step_in_card(a: &mut Agent) {
    a.start("install it", 1).await;
    a.emit(waiting_terminal(5)).await;
    a.emit(asked(5)).await;
    sleep(Duration::from_millis(600)).await;
    let lines = a.settle().await;
    let title = position(&lines, &copy::step_in_title(5));
    assert!(lines[title - 1].contains("working on it"), "{lines:#?}");
    assert!(!super::chat_row_shown(&lines), "{lines:#?}");
}

pub(crate) async fn finish(a: &Agent) {
    a.emit(super::steering::cancelled()).await;
    a.ctrl('c').ctrl('c').ctrl('c');
}

#[tokio::test(start_paused = true)]
async fn the_step_in_card_attaches_with_1_f12_or_down_and_enter() {
    for take in [
        &[KeyCode::Char('1')][..],
        &[KeyCode::F(12)][..],
        &[KeyCode::Down, KeyCode::Enter][..],
    ] {
        steering_on(
            Screen::new(24, 100),
            options(Start::Stalled),
            move |mut a| async move {
                step_in_card(&mut a).await;
                for (rows, cols) in [(9, 80), (24, 80), (24, 100)] {
                    let size = a.screen.resize(rows, cols);
                    a.inputs.send(Input::Resize(size)).unwrap();
                    line_with(&a.settle().await, &copy::step_in_title(5));
                }
                for code in take {
                    a.key(*code);
                }
                assert_eq!(
                    a.next().await,
                    ClientCommand::AttachProcess { id: 5, size: None }
                );
                finish(&a).await;
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn ctrl_bracket_on_the_card_takes_over_and_the_programs_end_closes_the_view() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            step_in_card(&mut a).await;
            a.emit(waiting_terminal(6)).await;
            a.emit(AgentEvent::ProcessNeedsInput {
                id: 6,
                kind: wisp_protocol::NeedsInputKind::HiddenInput,
            })
            .await;
            a.settle().await;
            a.ctrl(']');
            let size = took_over(&mut a, 5).await;
            a.emit(exited(5, 0)).await;
            a.emit(AgentEvent::ProcessDetached {
                id: 5,
                size,
                reason: wisp_protocol::DetachReason::Ended,
                typed: true,
            })
            .await;
            a.emit(AgentEvent::AttachRequestClosed {
                call_id: "toolu_ask".to_owned(),
                how: wisp_protocol::AttachRequestEnd::ProcessEnded,
            })
            .await;
            let lines = a.settle().await;
            assert!(
                !lines.iter().any(|l| l.contains("Process 5  ")),
                "{lines:#?}"
            );
            assert!(
                super::status_line(&lines).contains("process 5 exited (0)"),
                "{lines:#?}"
            );
            assert_eq!(
                prompt_and_caret(&a.screen, &lines),
                (ink(Token::Accent), ink(Token::Text)),
                "{lines:#?}"
            );
            a.key(KeyCode::Char('p'));
            a.nothing_sent().await;
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn f12_in_the_process_screens_takes_over_what_waits_and_the_end_leaves_the_ended_screen() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.emit(waiting_terminal(5)).await;
            a.paste("/ps").key(KeyCode::Enter);
            a.settle().await;
            a.key(KeyCode::Char('x')).key(KeyCode::F(12));
            a.key(KeyCode::Char('x'));
            a.nothing_sent().await;
            a.emit(AgentEvent::ProcessNeedsInput {
                id: 5,
                kind: wisp_protocol::NeedsInputKind::HiddenInput,
            })
            .await;
            a.emit(asked(5)).await;
            a.key(KeyCode::F(12));
            a.nothing_sent().await;
            a.emit(ask_closed()).await;
            a.key(KeyCode::Enter);
            line_with(&a.settle().await, "Process 5  ");
            a.key(KeyCode::F(12));
            let size = took_over(&mut a, 5).await;
            a.emit(exited(5, 0)).await;
            a.emit(AgentEvent::ProcessDetached {
                id: 5,
                size,
                reason: wisp_protocol::DetachReason::Ended,
                typed: false,
            })
            .await;
            let lines = a.settle().await;
            let title = line_with(&lines, "Process 5  ");
            assert!(title.contains(copy::SCREEN_ENDED), "{title}");
            line_with(&lines, "esc close");
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert!(
                !lines.iter().any(|l| l.contains("Process 5  ")),
                "{lines:#?}"
            );
            line_with(&lines, "enter open");
            a.key(KeyCode::Esc);
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn take_over_go_back_take_over_before_the_answers_keeps_the_view_and_the_keys_in_it() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.emit(waiting_terminal(5)).await;
            a.emit(AgentEvent::ProcessNeedsInput {
                id: 5,
                kind: wisp_protocol::NeedsInputKind::HiddenInput,
            })
            .await;
            a.settle().await;
            a.key(KeyCode::F(12))
                .key(KeyCode::F(12))
                .key(KeyCode::F(12));
            a.settle().await;
            let size = wisp_protocol::TermSize {
                cols: 100,
                rows: 30,
            };
            let attached = AgentEvent::ProcessAttached { id: 5, size };
            let left = AgentEvent::ProcessDetached {
                id: 5,
                size,
                reason: wisp_protocol::DetachReason::User,
                typed: false,
            };
            for event in [attached.clone(), left, attached] {
                a.emit(event).await;
            }
            line_with(&a.settle().await, "Process 5  ");
            sent_so_far(&mut a);
            a.key(KeyCode::Char('p'));
            assert_eq!(items_of(a.next().await).0, 5);
            a.key(KeyCode::F(12));
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_on_the_step_in_card_stops_the_turn() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            step_in_card(&mut a).await;
            a.key(KeyCode::Esc);
            assert_eq!(a.next().await, ClientCommand::Cancel);
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_on_the_step_in_card_stops_the_turn() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            step_in_card(&mut a).await;
            a.ctrl('c');
            assert_eq!(a.next().await, ClientCommand::Cancel);
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_step_in_card_lets_the_global_keys_through() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            step_in_card(&mut a).await;
            a.key(KeyCode::PageUp).key(KeyCode::PageDown).ctrl('l');
            a.nothing_sent().await;
            let lines = a.settle().await;
            line_with(&lines, &copy::step_in_title(5));
            assert!(
                !super::status_line(&lines).contains(&copy::step_in_hint()),
                "{lines:#?}"
            );
            a.key(KeyCode::Char('a')).key(KeyCode::Char('d')).ctrl('y');
            a.key(KeyCode::Tab).key(KeyCode::Enter);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(
                super::status_line(&lines).contains(&copy::step_in_hint()),
                "{lines:#?}"
            );
            a.key(KeyCode::Down);
            line_with(&a.settle().await, "\u{203a} 1 Yes");
            a.emit(ask_closed()).await;
            a.emit(asked(5)).await;
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Enter);
            a.nothing_sent().await;
            a.ctrl('o');
            a.settle().await;
            assert!(a.screen.in_pager());
            a.nothing_sent().await;
            a.key(KeyCode::Esc);
            a.settle().await;
            assert!(!a.screen.in_pager());
            a.nothing_sent().await;
            line_with(&a.settle().await, &copy::step_in_title(5));
            a.key(KeyCode::Char('2'));
            assert_eq!(a.next().await, decline());
            a.key(KeyCode::Char('x'));
            let lines = a.settle().await;
            assert!(
                super::status_line(&lines).contains(&copy::step_in_hint()),
                "{lines:#?}"
            );
            a.emit(super::steering::cancelled()).await;
            let lines = a.settle().await;
            assert!(
                !super::status_line(&lines).contains(&copy::step_in_hint()),
                "{lines:#?}"
            );
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_step_in_card_settles_half_a_second_after_the_last_keystroke() {
    let (dir, options) = pasting("settle-draft");
    steering_facts(
        Screen::new(24, 100),
        options,
        seeing(facts()),
        |mut a| async move {
            a.start("install it", 1).await;
            a.emit(waiting_terminal(5)).await;
            a.key(KeyCode::Char('x'));
            a.emit(asked(5)).await;
            sleep(Duration::from_millis(300)).await;
            a.key(KeyCode::Char('n'));
            a.nothing_sent().await;
            sleep(Duration::from_millis(400)).await;
            a.key(KeyCode::Char('n'));
            a.nothing_sent().await;
            sleep(Duration::from_millis(600)).await;
            paste_every_way(&a.inputs).await;
            assert_eq!(leaks(&a.screen, &dir), NO_LEAKS);
            line_with(&a.settle().await, "xnn");
            a.key(KeyCode::Tab)
                .key(KeyCode::Down)
                .key(KeyCode::Char(' '))
                .key(KeyCode::Enter);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(
                super::status_line(&lines).contains(&copy::step_in_hint()),
                "{lines:#?}"
            );
            a.key(KeyCode::Char('2'));
            assert_eq!(a.next().await, decline());
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_key_with_no_terminal_code_is_not_sent_and_says_so() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.emit(waiting_terminal(5)).await;
            a.emit(AgentEvent::ProcessNeedsInput {
                id: 5,
                kind: wisp_protocol::NeedsInputKind::HiddenInput,
            })
            .await;
            a.settle().await;
            a.ctrl(']');
            took_over(&mut a, 5).await;
            a.settle().await;
            a.ctrl('1');
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(
                super::status_line(&lines).contains("not sent"),
                "{lines:#?}"
            );
            a.ctrl(']');
            assert_eq!(a.next().await, ClientCommand::DetachProcess { id: 5 });
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_status_row_puts_waiting_hints_before_process_hints_before_the_facts() {
    steering_on(
        Screen::new(24, 200),
        options(Start::Stalled),
        |mut a| async move {
            a.start("build it", 1).await;
            a.emit(started(3)).await;
            a.paste("more");
            let lines = a.settle().await;
            let status = super::status_line(&lines);
            let at = |needle: &str| {
                status
                    .find(needle)
                    .unwrap_or_else(|| panic!("{needle:?} not in {status}"))
            };
            assert!(at(&copy::tab_queues().text()) < at("1 running"), "{status}");
            assert!(at("1 running") < at(copy::PS_HINT), "{status}");
            assert!(at(copy::PS_HINT) < at("test-model"), "{status}");
            finish(&a).await;
        },
    )
    .await;
}

fn prompt_and_caret(screen: &Screen, lines: &[String]) -> (vt100::Color, vt100::Color) {
    let top = super::composer_top(lines);
    let row = u16::try_from(top + 1).unwrap();
    let state = screen.0.borrow();
    let shown = state.parser.screen();
    let prompt = shown.cell(row, 2).unwrap().fgcolor();
    let caret = shown.cell(row, 4).unwrap().bgcolor();
    (prompt, caret)
}

pub(crate) fn ink(t: Token) -> vt100::Color {
    let rgb = Theme::new(Mode::Dark, Depth::TrueColor).rgb(t);
    vt100::Color::Rgb(rgb.r, rgb.g, rgb.b)
}

#[tokio::test(start_paused = true)]
async fn the_process_list_holds_the_keys_and_esc_gives_them_back_to_the_composer() {
    let screen = Screen::new(24, 90);
    steering_on(screen.clone(), listing(), |a| async move {
        a.emit(started(3)).await;
        let lines = a.settle().await;
        line_with(&lines, "3  npm run dev");
        assert!(!super::chat_row_shown(&lines), "{lines:#?}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert_eq!(
            prompt_and_caret(&a.screen, &lines),
            (ink(Token::Accent), ink(Token::Text)),
            "{lines:#?}"
        );
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_step_in_request_waits_while_the_ctrl_o_picker_is_open_and_typed_keys_only_filter() {
    steering_on(
        Screen::new(30, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.start("install it", 1).await;
            a.emit(AgentEvent::BlockStarted {
                index: 1,
                kind: BlockKind::Thinking,
            })
            .await;
            a.emit(AgentEvent::ThinkingDelta {
                index: 1,
                text: "Placeholder thought.\n".to_owned(),
            })
            .await;
            a.emit(AgentEvent::BlockStopped { index: 1 }).await;
            a.settle().await;
            a.ctrl('o');
            let lines = a.settle().await;
            line_with(&lines, copy::EVERY_CELL);
            a.emit(waiting_terminal(5)).await;
            a.emit(asked(5)).await;
            a.emit(AgentEvent::ProcessNeedsInput {
                id: 5,
                kind: wisp_protocol::NeedsInputKind::HiddenInput,
            })
            .await;
            sleep(Duration::from_millis(600)).await;
            let lines = a.settle().await;
            assert!(
                super::status_line(&lines).contains(&copy::waiting_for_password(5)),
                "{lines:#?}"
            );
            assert!(
                !super::status_line(&lines).contains(copy::F12_KEY),
                "{lines:#?}"
            );
            a.key(KeyCode::Char('1')).key(KeyCode::Char('2'));
            a.nothing_sent().await;
            let lines = a.settle().await;
            line_with(&lines, "matching \"12\"");
            assert!(
                !lines.iter().any(|l| l.contains(&copy::step_in_title(5))),
                "{lines:#?}"
            );
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            line_with(&lines, &copy::step_in_title(5));
            a.key(KeyCode::Char('1'));
            a.nothing_sent().await;
            sleep(Duration::from_millis(600)).await;
            a.settle().await;
            a.key(KeyCode::Char('2'));
            assert_eq!(a.next().await, decline());
            finish(&a).await;
        },
    )
    .await;
}

pub(super) async fn background_call(a: &Agent, id: u32, start: AgentEvent) {
    let call_id = format!("toolu_{id}");
    run_call(a, &call_id, false).await;
    a.emit(start).await;
    let mut finished = AgentEvent::call_finished(
        &call_id,
        wisp_protocol::ToolOutcome::Backgrounded {
            reason: wisp_protocol::BackgroundReason::Started,
        },
    );
    if let AgentEvent::ToolCallFinished { process, facts, .. } = &mut finished {
        *process = Some(id);
        facts.push(wisp_protocol::Fact::Process { id });
    }
    a.emit(finished).await;
}

fn restored(id: u32, call_id: Option<&str>, target: &str) -> AgentEvent {
    AgentEvent::ProcessRestored {
        id,
        call_id: call_id.map(str::to_owned),
        target: target.to_owned(),
        tty: false,
        end: Some(ProcessEnd::Exited {
            exit: ExitStatus::Code { code: 0 },
        }),
        duration_ms: 1000,
        keeps_running: false,
        unchecked: false,
        pid: None,
        log: None,
    }
}

fn in_reader(lines: &[String]) -> bool {
    lines.iter().any(|l| l.contains(&back_keys().text()))
}

#[tokio::test(start_paused = true)]
async fn ps_enter_opens_a_pipe_or_restored_call_page_a_live_screen_or_nothing() {
    steering_on(Screen::new(30, 100), listing(), |mut a| async move {
        a.start("build it", 1).await;
        background_call(&a, 3, started(3)).await;
        background_call(&a, 5, terminal(5)).await;
        run_call(&a, "toolu_7", false).await;
        a.emit(AgentEvent::call_finished(
            "toolu_7",
            wisp_protocol::ToolOutcome::Completed {
                exit: Some(ExitStatus::Code { code: 0 }),
                is_error: false,
            },
        ))
        .await;
        a.emit(restored(7, Some("toolu_7"), "make watch")).await;
        a.emit(restored(9, None, "an old job")).await;
        let lines = a.settle().await;
        line_with(&lines, "3  npm run dev");
        line_with(&lines, "9  an old job");
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(in_reader(&lines), "{lines:#?}");
        assert!(lines[0].contains("cargo build"), "{lines:#?}");
        line_with(&lines, "process 3 \u{b7} running");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(!in_reader(&lines), "{lines:#?}");
        line_with(&lines, "3  npm run dev");
        a.key(KeyCode::Down).key(KeyCode::Enter);
        let lines = a.settle().await;
        line_with(&lines, "Process 5  python3 -i");
        assert!(!in_reader(&lines), "{lines:#?}");
        a.key(KeyCode::Esc).key(KeyCode::Char('x'));
        a.emit(exited(5, 0)).await;
        let lines = a.settle().await;
        let hint = line_with(&lines, "enter open");
        assert!(
            !hint.contains("a take over") && !hint.contains("x stop"),
            "{lines:#?}"
        );
        a.key(KeyCode::Char('a'))
            .key(KeyCode::Char('x'))
            .key(KeyCode::Char('x'));
        a.nothing_sent().await;
        a.key(KeyCode::Down).key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(in_reader(&lines), "{lines:#?}");
        assert!(lines[0].contains("cargo build"), "{lines:#?}");
        line_with(&lines, "in ~/demo \u{b7} exit 0");
        a.key(KeyCode::Esc);
        a.key(KeyCode::Down).key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(!in_reader(&lines), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("Process 9")), "{lines:#?}");
        line_with(&lines, "9  an old job");
        a.key(KeyCode::Esc);
        a.emit(super::steering::cancelled()).await;
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

async fn open_tool_row(a: &Agent, needle: &str) -> Vec<String> {
    let lines = a.settle().await;
    let (row, line) = lines
        .iter()
        .enumerate()
        .rfind(|(_, l)| l.contains(needle))
        .unwrap_or_else(|| panic!("no {needle}: {lines:#?}"));
    let spot = Mouse {
        kind: MouseKind::Press,
        column: u16::try_from(line[..line.find(needle).unwrap()].chars().count() + 1).unwrap(),
        row: u16::try_from(row).unwrap(),
        modified: false,
    };
    a.inputs.send(Input::Mouse(spot)).unwrap();
    a.inputs
        .send(Input::Mouse(Mouse {
            kind: MouseKind::Release,
            ..spot
        }))
        .unwrap();
    sleep(Duration::from_millis(500)).await;
    a.settle().await
}

#[tokio::test(start_paused = true)]
async fn s_on_a_running_terminals_page_watches_its_screen_and_esc_steps_back_out() {
    use wisp_tui::app::pager::{follow_key, screen_key};
    steering_on(
        Screen::new(30, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.start("build it", 1).await;
            background_call(&a, 5, terminal(5)).await;
            let lines = open_tool_row(&a, "cargo build").await;
            assert!(in_reader(&lines), "{lines:#?}");
            let keys = super::status_line(&lines);
            assert!(keys.contains(&screen_key().text()), "{keys}");
            assert!(!keys.contains(&follow_key().text()), "{keys}");
            a.key(KeyCode::Char('s'));
            let lines = a.settle().await;
            assert!(!in_reader(&lines), "{lines:#?}");
            line_with(&lines, "Process 5  python3 -i");
            a.nothing_sent().await;
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert!(
                !lines.iter().any(|l| l.contains("Process 5  python3 -i")),
                "{lines:#?}"
            );
            line_with(&lines, "5  python3 -i");
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert!(
                !lines.iter().any(|l| l.contains("5  python3 -i")),
                "{lines:#?}"
            );
            a.emit(exited(5, 0)).await;
            let lines = open_tool_row(&a, "cargo build").await;
            assert!(in_reader(&lines), "{lines:#?}");
            assert!(
                !super::status_line(&lines).contains(&screen_key().text()),
                "{lines:#?}"
            );
            a.key(KeyCode::Char('s'));
            let lines = a.settle().await;
            assert!(in_reader(&lines), "{lines:#?}");
            a.key(KeyCode::Esc);
            a.emit(super::steering::cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_pipes_page_offers_no_screen() {
    use wisp_tui::app::pager::screen_key;
    steering_on(
        Screen::new(30, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.start("build it", 1).await;
            background_call(&a, 3, started(3)).await;
            let lines = open_tool_row(&a, "cargo build").await;
            assert!(in_reader(&lines), "{lines:#?}");
            assert!(
                !super::status_line(&lines).contains(&screen_key().text()),
                "{lines:#?}"
            );
            a.key(KeyCode::Char('s'));
            let lines = a.settle().await;
            assert!(in_reader(&lines), "{lines:#?}");
            a.key(KeyCode::Esc);
            a.emit(super::steering::cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_under_the_process_list_never_reaches_the_composer() {
    let (dir, mut options) = pasting("under-ps");
    options.open_processes = true;
    steering_facts(
        Screen::new(24, 90),
        options,
        seeing(facts()),
        |a| async move {
            a.emit(started(3)).await;
            let lines = a.settle().await;
            line_with(&lines, "3  npm run dev");
            paste_every_way(&a.inputs).await;
            a.key(KeyCode::Esc);
            a.settle().await;
            assert_eq!(leaks(&a.screen, &dir), NO_LEAKS);
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_at_the_step_in_card_reaches_neither_the_composer_nor_the_process() {
    let (dir, options) = pasting("at-step-in");
    steering_facts(
        Screen::new(24, 100),
        options,
        seeing(facts()),
        |mut a| async move {
            step_in_card(&mut a).await;
            paste_every_way(&a.inputs).await;
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert_eq!(leaks(&a.screen, &dir), NO_LEAKS);
            assert!(
                super::status_line(&lines).contains(&copy::step_in_hint()),
                "{lines:#?}"
            );
            a.key(KeyCode::Char('2'));
            assert_eq!(a.next().await, decline());
            a.emit(ask_closed()).await;
            let lines = a.settle().await;
            assert!(
                !super::status_line(&lines).contains(&copy::step_in_hint()),
                "{lines:#?}"
            );
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_while_typing_into_a_process_goes_to_the_process_and_never_the_composer() {
    let (dir, options) = pasting("attached");
    steering_facts(Screen::new(24, 100), options, seeing(facts()), |mut a| async move {
        a.emit(described_start(5)).await;
        a.emit(waiting_terminal(5)).await;
        a.emit(AgentEvent::ProcessNeedsInput {
            id: 5,
            kind: wisp_protocol::NeedsInputKind::HiddenInput,
        })
        .await;
        a.ctrl(']');
        let size = took_over(&mut a, 5).await;
        a.settle().await;
        paste_every_way(&a.inputs).await;
        let (id, items) = items_of(a.next().await);
        assert_eq!(id, 5);
        assert!(
            matches!(&items[..], [wisp_protocol::AttachInput::Paste { text }] if text == PASTED),
            "the text paste did not reach the process"
        );
        a.nothing_sent().await;
        a.ctrl(']');
        assert_eq!(a.next().await, ClientCommand::DetachProcess { id: 5 });
        a.emit(AgentEvent::ProcessDetached {
            id: 5,
            size,
            reason: wisp_protocol::DetachReason::User,
            typed: true,
        })
        .await;
        a.settle().await;
        assert_eq!(leaks(&a.screen, &dir), NO_LEAKS);
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

fn sent_so_far(a: &mut Agent) -> Vec<ClientCommand> {
    let mut out = vec![];
    while let Ok(command) = a.commands.try_recv() {
        out.push(command);
    }
    out
}

fn the_card_took_no_key(a: &mut Agent) {
    let sent = sent_so_far(a);
    assert!(
        !sent.iter().any(|c| matches!(
            c,
            ClientCommand::DeclineAttach { .. }
                | ClientCommand::Cancel
                | ClientCommand::AttachProcess { .. }
                | ClientCommand::ProcessInput { .. }
        )),
        "{sent:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_paste_at_a_step_in_card_over_the_endpoint_card_never_becomes_its_key() {
    let mut options = options(Start::Stalled);
    options.card_setup = Some(super::endpoints::card_setup());
    steering_on(Screen::new(30, 100), options, |mut a| async move {
        a.paste("/endpoint add").key(KeyCode::Enter);
        let lines = a.settle().await;
        line_with(&lines, endpoint_card::ADD_TITLE);
        a.key(KeyCode::Enter);
        a.emit(waiting_terminal(5)).await;
        a.emit(asked(5)).await;
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        line_with(&lines, &copy::step_in_title(5));
        a.paste(PASTED);
        a.emit(ask_closed()).await;
        let lines = a.settle().await;
        assert!(
            !lines.iter().any(|l| l.contains(&copy::step_in_title(5))),
            "the step-in card stayed open"
        );
        a.key(KeyCode::Enter).key(KeyCode::Enter);
        a.settle().await;
        let saved = sent_so_far(&mut a)
            .into_iter()
            .any(|c| matches!(c, ClientCommand::SaveEndpoint { .. }));
        assert!(!saved, "the text paste became the endpoint's key");
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_step_in_card_waits_while_config_is_open_and_holds_the_keys_once_it_closes() {
    for secret in [true, false] {
        steering_on(
            Screen::new(24, 100),
            options(Start::Stalled),
            |mut a| async move {
                a.start("install it", 1).await;
                a.emit(waiting_terminal(5)).await;
                a.paste("/config").key(KeyCode::Enter);
                let lines = a.settle().await;
                line_with(&lines, "Loading settings");
                a.emit(if secret {
                    super::secrets::asked()
                } else {
                    asked(5)
                })
                .await;
                sleep(Duration::from_millis(600)).await;
                a.key(KeyCode::Char('2'))
                    .key(KeyCode::Esc)
                    .key(KeyCode::Char('1'));
                let lines = a.settle().await;
                assert!(
                    !lines.iter().any(|l| l.contains("Loading settings")),
                    "{lines:#?}"
                );
                the_card_took_no_key(&mut a);
                line_with(
                    &lines,
                    &if secret {
                        "? OpenAI API key".to_owned()
                    } else {
                        copy::step_in_title(5)
                    },
                );
                sleep(Duration::from_millis(600)).await;
                if secret {
                    a.paste("CANARY-config").key(KeyCode::Enter);
                    assert!(matches!(a.next().await, ClientCommand::AnswerSecret { .. }));
                } else {
                    a.key(KeyCode::Char('2'));
                    assert_eq!(a.next().await, decline());
                }
                finish(&a).await;
            },
        )
        .await;
    }
}

async fn a_step_in_card_waits_once_a_full_screen_view_closes(open: fn(&Agent)) {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            a.start("install it", 1).await;
            a.emit(waiting_terminal(5)).await;
            open(&a);
            let lines = a.settle().await;
            assert!(a.screen.in_pager(), "{lines:#?}");
            a.emit(asked(5)).await;
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Char('q')).key(KeyCode::Char('1'));
            let lines = a.settle().await;
            assert!(!a.screen.in_pager(), "{lines:#?}");
            the_card_took_no_key(&mut a);
            line_with(&lines, &copy::step_in_title(5));
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Char('2'));
            assert_eq!(a.next().await, decline());
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_step_in_card_waits_once_transcript_mode_closes() {
    a_step_in_card_waits_once_a_full_screen_view_closes(|a| {
        a.ctrl('o');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_step_in_card_waits_once_the_help_screen_closes() {
    a_step_in_card_waits_once_a_full_screen_view_closes(|a| {
        a.paste("/help").key(KeyCode::Enter);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_new_step_in_request_under_transcript_mode_waits_once_it_closes() {
    steering_on(
        Screen::new(24, 100),
        options(Start::Stalled),
        |mut a| async move {
            step_in_card(&mut a).await;
            a.ctrl('o');
            let lines = a.settle().await;
            assert!(a.screen.in_pager(), "{lines:#?}");
            a.emit(ask_closed()).await;
            a.emit(asked(5)).await;
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Char('q')).key(KeyCode::Char('1'));
            let lines = a.settle().await;
            assert!(!a.screen.in_pager(), "{lines:#?}");
            the_card_took_no_key(&mut a);
            line_with(&lines, &copy::step_in_title(5));
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Char('2'));
            assert_eq!(a.next().await, decline());
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_covering_request_keeps_keys_out_of_underlying_panes() {
    for secret in [false, true] {
        for (pane, marker, keys) in [
            (
                "/ps",
                "Processes",
                vec![KeyCode::Char('x'), KeyCode::Char('x')],
            ),
            ("/ps screen", "Process 5", vec![KeyCode::Char('a')]),
            ("/agents", "agent-viz is off", vec![KeyCode::Enter]),
            ("/usage", "Usage", vec![KeyCode::Enter]),
            (
                "/endpoint add",
                endpoint_card::ADD_TITLE,
                vec![KeyCode::Enter, KeyCode::Char('x')],
            ),
        ] {
            let mut opts = options(Start::Stalled);
            opts.card_setup = Some(super::endpoints::card_setup());
            let mut facts = facts();
            facts.endpoints = vec![super::session_account(
                wisp_render::chrome::Endpoint::Official,
            )];
            steering_facts(Screen::new(30, 100), opts, facts, |mut a| async move {
                a.start("install it", 1).await;
                a.emit(waiting_terminal(5)).await;
                a.paste(if pane == "/ps screen" { "/ps" } else { pane })
                    .key(KeyCode::Enter);
                if pane == "/ps screen" {
                    a.settle().await;
                    a.key(KeyCode::Enter);
                }
                let before = a.settle().await;
                line_with(&before, marker);
                sent_so_far(&mut a);
                a.emit(if secret {
                    super::secrets::asked()
                } else {
                    asked(5)
                })
                .await;
                a.settle().await;
                for key in keys {
                    a.key(key);
                }
                a.paste("x");
                sleep(Duration::from_millis(30)).await;
                let sent = sent_so_far(&mut a);
                assert!(sent.is_empty(), "{pane}, secret={secret}: {sent:?}");
                sleep(Duration::from_millis(300)).await;
                for _ in 0..2 {
                    a.key(KeyCode::Char('2'));
                    sleep(Duration::from_millis(300)).await;
                    let sent = sent_so_far(&mut a);
                    assert!(sent.is_empty(), "early {pane}, secret={secret}: {sent:?}");
                }
                sleep(Duration::from_millis(600)).await;
                if secret {
                    a.paste("CANARY-input").key(KeyCode::Enter);
                    assert!(matches!(a.next().await, ClientCommand::AnswerSecret { .. }));
                    a.emit(AgentEvent::SecretRequestClosed {
                        call_id: "secret".to_owned(),
                        how: wisp_protocol::SecretRequestEnd::Entered,
                    })
                    .await;
                } else {
                    a.key(KeyCode::Char('2'));
                    assert_eq!(a.next().await, decline());
                    a.emit(ask_closed()).await;
                }
                let after = a.settle().await;
                line_with(&after, marker);
                assert!(
                    pane != "/endpoint add" || !after.iter().any(|line| line.contains("character")),
                    "{after:?}"
                );
                assert!(
                    pane != "/usage" || after.iter().any(|line| line.contains("r refresh")),
                    "{after:?}"
                );
                finish(&a).await;
            })
            .await;
        }
    }
}
