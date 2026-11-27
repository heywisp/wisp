use std::time::Duration;

use serde_json::json;
use tokio::time::sleep;
use wisp_protocol::plugin::CommandOutcome;
use wisp_protocol::settings::EndpointSummary;
use wisp_protocol::{
    Activity, AgentEntry, AgentEvent, AgentPath, AgentState, ClientCommand, DeliveryMode,
    HoldReason, IdleOutcome, InputId, MissingImage, OutputKind, PointInfo, ProcessEnd, Question,
    QuestionOption, RejectKind, RequestId, RetryFocus, RewindPoint, StoppedBy, ToolAction, Warmth,
    WithdrawnBy,
};
use wisp_render::chrome::Credential;
use wisp_render::chrome::account::ACCOUNT_TITLE;
use wisp_render::theme::{Depth, Mode, Theme, glyphs::GlyphSet};
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::pager::{back_keys, return_keys, stop_key};
use wisp_tui::app::{NEW_KEY, Options, replay};
use wisp_tui::term::{Input, Key, KeyCode, MouseKind, RESIZE_DEBOUNCE};

use super::Screen;
use super::agents::{changed, entry, idle, spawned};
use super::folds::click;
use super::images::{NO_LEAKS, PASTED, clipboard_leaks, paste_from_clipboard, pasting, seeing};
use super::processes::{ask_closed, asked, exited, waiting_terminal};
use super::rewind::{point, returned, rewound};
use super::select::locate;
use super::sidebar::{mouse, snapshot};
use super::steering::{Agent, steering_facts, steering_on};
use super::{facts, options};
use crate::Drive;
use crate::events::{Requested, tool_requested};
use crate::processes::line_with;

pub(crate) fn screen() -> Screen {
    Screen::new(30, 100)
}

pub(crate) fn none_with(lines: &[String], needle: &str) {
    assert!(
        !lines.iter().any(|l| l.contains(needle)),
        "{needle:?} in {lines:#?}"
    );
}

pub(super) fn wrapped(agent: &str, event: AgentEvent) -> AgentEvent {
    AgentEvent::Agent {
        agent: AgentPath::new(agent),
        event: Box::new(event),
    }
}

pub(super) fn bash(id: &str, command: &str) -> AgentEvent {
    tool_requested(Requested {
        block: 1,
        call_id: id.to_owned(),
        tool: "bash".to_owned(),
        input: json!({"command": command}),
        target: command.to_owned(),
        ..Requested::default()
    })
}

fn running_bash() -> Activity {
    Activity::Tool {
        name: "bash".to_owned(),
    }
}

pub(super) fn watch(agent: &str, live: bool) -> ClientCommand {
    ClientCommand::WatchAgent {
        agent: AgentPath::new(agent),
        live,
    }
}

pub(super) fn child(agent: &str, command: ClientCommand) -> ClientCommand {
    ClientCommand::Child {
        agent: AgentPath::new(agent),
        command: Box::new(command),
    }
}

fn press(a: &Agent, c: char) {
    a.key(KeyCode::Char(c));
}

async fn worker_mid_bash(a: &Agent) {
    worker_on(a, entry("/w", AgentState::Running, 0.2)).await;
}

async fn worker_on(a: &Agent, worker: AgentEntry) {
    a.emit(spawned(worker)).await;
    a.emit(wrapped("/w", AgentEvent::turn_started(1))).await;
    a.emit(wrapped("/w", AgentEvent::StepStarted { step: 1 }))
        .await;
    a.emit(wrapped("/w", bash("c1", "cargo test"))).await;
    a.emit(wrapped(
        "/w",
        AgentEvent::call_started("c1", "~/Projects/Wisp"),
    ))
    .await;
    a.emit(changed("/w", AgentState::Running, running_bash(), 0.2))
        .await;
}

async fn drain(a: &mut Agent) -> Vec<ClientCommand> {
    sleep(Duration::from_millis(20)).await;
    let mut out = Vec::new();
    while let Ok(command) = a.commands.try_recv() {
        out.push(command);
    }
    out
}

#[tokio::test(start_paused = true)]
async fn enter_watches_a_child_mid_bash_and_every_way_out_stops_watching() {
    let (main, options) = (super::agents::main_on(true), options(Start::Stalled));
    steering_facts(screen(), options, main, |mut a| async move {
        let mut on_main = entry("/w", AgentState::Running, 0.2);
        on_main.endpoint = Some("direct".to_owned());
        worker_on(&a, on_main).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.emit(wrapped(
            "/w",
            AgentEvent::ToolOutput {
                call_id: "c1".to_owned(),
                text: "running 12 tests\n".to_owned(),
            },
        ))
        .await;
        let lines = a.settle().await;
        line_with(&lines, "/w \u{b7} running");
        line_with(&lines, "cargo test");
        line_with(&lines, "running 12 tests");
        line_with(&lines, &copy::esc(copy::BACK_WORD).text());
        let (column, row) = locate(&a.screen, "direct");
        click(&a, column, row);
        none_with(&a.settle().await, ACCOUNT_TITLE);
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        none_with(&lines, "running 12 tests");
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.ctrl('d');
        a.nothing_sent().await;
        a.ctrl('t');
        assert_eq!(a.next().await, watch("/w", false));
        let lines = a.settle().await;
        line_with(&lines, copy::AGENTS_TITLE);
        none_with(&lines, "running 12 tests");
        a.key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.paste("a draft for w");
        a.ctrl('c');
        let lines = a.settle().await;
        none_with(&lines, "a draft for w");
        line_with(&lines, "running 12 tests");
        a.ctrl('c');
        assert_eq!(a.next().await, watch("/w", false));
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        none_with(&lines, "running 12 tests");
        a.nothing_sent().await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.inputs
            .send(Input::Closed("the terminal went away".to_owned()))
            .unwrap();
        assert_eq!(drain(&mut a).await, [watch("/w", false)]);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_in_a_workers_chat_goes_straight_to_the_main_chat() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(spawned(entry("/c", AgentState::Running, 0.1))).await;
        a.emit(spawned(entry("/c/w", AgentState::Running, 0.2)))
            .await;
        a.emit(wrapped("/c/w", super::processes::started(3))).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Down);
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/c/w", true));
        let lines = a.settle().await;
        line_with(&lines, "/c/w \u{b7} running");
        for (extra, sentence) in [
            (None, "agent continues when process 3 ends"),
            (Some(4), "agent continues when 1 of 2 processes ends"),
        ] {
            if let Some(id) = extra {
                a.emit(wrapped("/c/w", super::processes::started(id))).await;
            }
            for cols in [80, 100] {
                a.input(Input::Resize(a.screen.resize(30, cols)));
                let lines = a.settle().await;
                let status = super::status_line(&lines);
                assert!(status.contains(sentence), "{status}");
                line_with(&lines, &copy::esc(copy::BACK_WORD).text());
            }
        }
        a.key(KeyCode::Esc);
        assert_eq!(drain(&mut a).await, [watch("/c/w", false)]);
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        none_with(&lines, &copy::message_to("/c/w"));
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Down);
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/c/w", true));
        a.emit(changed("/c/w", AgentState::Ended, Activity::Idle, 0.2))
            .await;
        let lines = a.settle().await;
        line_with(&lines, &copy::agent_ended("/c/w", 0, " \u{b7} "));
        none_with(&lines, copy::AGENTS_TITLE);
        none_with(&lines, &copy::esc(copy::BACK_WORD).text());
        a.nothing_sent().await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn letters_in_a_childs_chat_steer_it_and_commands_work_as_in_the_main_chat() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.start("plan it", 1).await;
        watching_w(&mut a).await;
        let lines = a.settle().await;
        line_with(&lines, &copy::message_to("/w"));
        let back = copy::esc(copy::BACK_WORD).text();
        let before = line_with(&lines, &back).to_owned();
        sleep(Duration::from_secs(2)).await;
        let lines = a.settle().await;
        assert_ne!(line_with(&lines, &back), before);
        for c in "fix x e X r m".chars() {
            press(&a, c);
        }
        a.ctrl('d');
        a.nothing_sent().await;
        a.key(KeyCode::Enter);
        let ClientCommand::Child { agent, command } = a.next().await else {
            panic!("expected a message to /w");
        };
        assert_eq!(agent, AgentPath::new("/w"));
        let ClientCommand::Send {
            id,
            text,
            mode,
            turn,
            ..
        } = *command
        else {
            panic!("expected a send");
        };
        assert_eq!(text, "fix x e X r m");
        assert_eq!(mode, DeliveryMode::Steer);
        assert_eq!(turn, Some(1));
        a.nothing_sent().await;
        a.emit(wrapped("/w", accepted(&id, "fix x e X r m"))).await;
        a.paste("/model sk-ant-api03-CANARYMODEL0000")
            .key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        line_with(&lines, "this looks like a key");
        none_with(&lines, "CANARY");
        a.key(KeyCode::Up);
        let lines = a.settle().await;
        none_with(&lines, "CANARY");
        a.ctrl('c');
        a.paste("/help").key(KeyCode::Enter);
        let lines = a.settle().await;
        let hint = lines.iter().rev().find(|l| !l.trim().is_empty()).unwrap();
        assert!(hint.contains(&return_keys().text()), "{lines:#?}");
        assert!(!hint.contains(&stop_key().text()), "{lines:#?}");
        assert!(hint.contains(&copy::waiting_count(1)), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.nothing_sent().await;
        let lines = a.settle().await;
        line_with(&lines, &copy::message_to("/w"));
        a.paste("/agents").key(KeyCode::Enter);
        assert_eq!(drain(&mut a).await, [watch("/w", false)]);
        let lines = a.settle().await;
        line_with(&lines, copy::AGENTS_TITLE);
        none_with(&lines, &copy::message_to("/w"));
        none_with(&lines, "/agents");
        a.key(KeyCode::Esc);
        a.ctrl('c');
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(super::steering::cancelled()).await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stopped_child_reads_stopping_then_idle_stopped() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        worker_mid_bash(&a).await;
        a.ctrl('t').key(KeyCode::Down);
        for c in ['x', 'X', 'X', 'e', 'e'] {
            press(&a, c);
        }
        a.nothing_sent().await;
        a.emit(changed("/w", AgentState::Stopping, running_bash(), 0.2))
            .await;
        let lines = a.settle().await;
        line_with(&lines, "stopping");
        a.emit(changed(
            "/w",
            idle(Some(IdleOutcome::Stopped)),
            Activity::Idle,
            0.2,
        ))
        .await;
        let lines = a.settle().await;
        line_with(&lines, "idle 0s: stopped");
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn r_releases_a_held_childs_messages_and_x_and_e_send_nothing() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(spawned(entry(
            "/h",
            AgentState::Held {
                reason: HoldReason::HeldMessages,
            },
            0.3,
        )))
        .await;
        a.ctrl('t').key(KeyCode::Down);
        let lines = a.settle().await;
        line_with(&lines, "r release");
        line_with(&lines, "held: your messages");
        press(&a, 'r');
        assert_eq!(a.next().await, child("/h", ClientCommand::Release));
        none_with(&lines, "e end");
        for c in ['x', 'X', 'X', 'e', 'e'] {
            press(&a, c);
        }
        a.nothing_sent().await;
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/h", true));
        let lines = a.settle().await;
        line_with(&lines, &copy::send_held().text());
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, child("/h", ClientCommand::Release));
        a.paste("after the release").key(KeyCode::Enter);
        let ClientCommand::Child { command, .. } = a.next().await else {
            panic!("expected a message to /h");
        };
        let ClientCommand::Send { id, .. } = *command else {
            panic!("expected a send");
        };
        let lines = a.settle().await;
        let back = copy::esc(copy::BACK_WORD).text();
        assert!(!line_with(&lines, &back).contains(copy::READY));
        a.emit(wrapped("/h", accepted(&id, "after the release")))
            .await;
        a.emit(wrapped(
            "/h",
            AgentEvent::InputHeld {
                ids: vec![id],
                by: WithdrawnBy::TurnStopped,
            },
        ))
        .await;
        a.ctrl('d');
        let lines = a.settle().await;
        assert!(line_with(&lines, &back).contains(copy::READY));
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/h", false));
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn mains_row_closes_on_enter_and_ignores_x_e_and_r() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(spawned(entry("/w", AgentState::Running, 0.2))).await;
        a.ctrl('t');
        for c in ['x', 'e', 'e', 'r'] {
            press(&a, c);
        }
        a.nothing_sent().await;
        let lines = a.settle().await;
        line_with(&lines, copy::AGENTS_TITLE);
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        a.paste("typed into the composer");
        let lines = a.settle().await;
        line_with(&lines, "typed into the composer");
        a.nothing_sent().await;
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_child_that_ends_while_its_chat_is_open_goes_back_to_the_main_chat_without_unwatching() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        worker_mid_bash(&a).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.emit(changed("/w", AgentState::Ended, Activity::Idle, 0.2))
            .await;
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        none_with(&lines, &copy::esc(copy::BACK_WORD).text());
        line_with(&lines, &copy::agent_ended("/w", 0, " \u{b7} "));
        a.nothing_sent().await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_childs_chat_at_40_80_and_120_in_every_look() {
    let (mode, mode_name) = (Mode::Dark, "dark");
    for (glyphs, set) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
        for width in [40u16, 80, 120] {
            let mut options = options(Start::Stalled);
            options.theme = Theme::new(mode, Depth::TrueColor);
            options.glyphs = glyphs;
            let name = format!("agents-watch-{mode_name}-truecolor-{set}-{width}");
            watch_scene(Screen::new(24, width), options, name).await;
        }
    }
}

async fn watch_scene(screen: Screen, options: Options, name: String) {
    steering_on(screen, options, |mut a| async move {
        let mut shots = Vec::new();
        worker_mid_bash(&a).await;
        a.emit(wrapped(
            "/w",
            AgentEvent::ToolOutput {
                call_id: "c1".to_owned(),
                text: "running 12 tests\ntest agents::watch ... ok\n".to_owned(),
            },
        ))
        .await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        sleep(Duration::from_millis(200)).await;
        shots.push(("running".to_owned(), a.screen.styled()));
        a.paste("use the release build");
        sleep(Duration::from_millis(200)).await;
        shots.push(("typing".to_owned(), a.screen.styled()));
        a.key(KeyCode::Enter);
        let ClientCommand::Child { command, .. } = a.next().await else {
            panic!("expected a message to /w");
        };
        let ClientCommand::Send { id, .. } = *command else {
            panic!("expected a send");
        };
        a.emit(wrapped("/w", accepted(&id, "use the release build")))
            .await;
        a.emit(wrapped(
            "/w",
            AgentEvent::InputHeld {
                ids: vec![id.clone()],
                by: WithdrawnBy::TurnStopped,
            },
        ))
        .await;
        a.emit(wrapped("/w", super::steering::cancelled())).await;
        let held = AgentState::Held {
            reason: HoldReason::HeldMessages,
        };
        a.emit(changed("/w", held, Activity::Idle, 0.2)).await;
        a.ctrl('d');
        sleep(Duration::from_secs(5)).await;
        shots.push(("held".to_owned(), a.screen.styled()));
        a.emit(wrapped("/w", AgentEvent::turn_started(2))).await;
        let delivered = AgentEvent::InputDelivered {
            ids: vec![id],
            step: 1,
        };
        a.emit(wrapped("/w", delivered)).await;
        a.emit(wrapped("/w", bash("c2", "cargo test"))).await;
        let started = AgentEvent::call_started("c2", "~/Projects/Wisp");
        a.emit(wrapped("/w", started)).await;
        a.emit(changed("/w", AgentState::Running, running_bash(), 0.2))
            .await;
        notes(&a, 1..=12).await;
        a.key(KeyCode::PageUp);
        notes(&a, 13..=16).await;
        sleep(Duration::from_millis(200)).await;
        shots.push(("scrolled".to_owned(), a.screen.styled()));
        a.emit(wrapped("/w", super::completed())).await;
        let reported = idle(Some(IdleOutcome::Reported));
        a.emit(changed("/w", reported, Activity::Idle, 0.2)).await;
        a.key(KeyCode::End);
        a.paste("/model").key(KeyCode::Enter);
        let request = describe_w(&mut a).await;
        let pair = [
            ("claude-api", Credential::ApiKey),
            ("claude-work", Credential::AnthropicPlan),
        ];
        a.emit(listed(request, pair, &[])).await;
        a.key(KeyCode::Down);
        sleep(Duration::from_millis(200)).await;
        shots.push(("model picker".to_owned(), a.screen.styled()));
        a.key(KeyCode::Enter);
        let ClientCommand::ChangeSettings { request, .. } = to_w(&mut a).await else {
            panic!("expected a change for /w");
        };
        a.emit(switched(request, "claude-api")).await;
        sleep(Duration::from_millis(200)).await;
        shots.push(("switched".to_owned(), a.screen.styled()));
        let turns = [
            (1, "\u{21bb} a message from main", "14:02"),
            (2, "use the release build", "14:05"),
            (3, "now run the tests", "14:09"),
        ];
        let points = turns.map(|(turn, label, at)| PointInfo {
            at: format!("2026-10-26T{at}:00Z"),
            ..point(turn, label)
        });
        rewind_w(&mut a, points.to_vec()).await;
        sleep(Duration::from_millis(200)).await;
        shots.push(("rewind".to_owned(), a.screen.styled()));
        a.key(KeyCode::Esc);
        programs(&a).await;
        a.paste("/ps").key(KeyCode::Enter);
        a.key(KeyCode::Down).key(KeyCode::Down);
        sleep(Duration::from_millis(200)).await;
        shots.push(("ps".to_owned(), a.screen.styled()));
        snapshot(&name, &shots);
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.ctrl('c').ctrl('c');
    })
    .await;
}

fn program(id: u32, target: &str) -> AgentEvent {
    let mut event = super::processes::started(id);
    if let AgentEvent::ProcessStarted { target: shown, .. } = &mut event {
        *shown = target.to_owned();
    }
    event
}

fn output(id: u32, text: &str) -> AgentEvent {
    let text = text.to_owned();
    AgentEvent::ProcessOutput {
        id,
        text,
        skipped: 0,
    }
}

async fn programs(a: &Agent) {
    a.emit(super::processes::started(3)).await;
    a.emit(output(3, "\u{279c}  Local:   http://localhost:5173/\n"))
        .await;
    sleep(Duration::from_secs(149)).await;
    a.emit(wrapped("/w", program(3, "cargo test --workspace")))
        .await;
    a.emit(wrapped("/w", output(3, "test agents::watch ... ok\n")))
        .await;
    sleep(Duration::from_secs(7)).await;
    a.emit(wrapped("/w", super::processes::terminal(4))).await;
    a.emit(wrapped("/w", output(4, ">>> "))).await;
    sleep(Duration::from_secs(5)).await;
}

async fn send_from_watch(a: &mut Agent, text: &str) -> InputId {
    a.paste(text).key(KeyCode::Enter);
    let ClientCommand::Child { command, .. } = a.next().await else {
        panic!("expected a message");
    };
    let ClientCommand::Send { id, .. } = *command else {
        panic!("expected a send");
    };
    id
}

fn accepted(id: &InputId, text: &str) -> AgentEvent {
    AgentEvent::InputAccepted {
        opening: false,
        id: id.clone(),
        mode: DeliveryMode::Steer,
        text: text.to_owned(),
        images: Vec::new(),
        replaces: None,
        held: false,
        front: false,
        from: None,
        checklist_spans: Vec::new(),
    }
}

async fn watching_w(a: &mut Agent) {
    worker_mid_bash(a).await;
    a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
    assert_eq!(a.next().await, watch("/w", true));
}

#[tokio::test(start_paused = true)]
async fn a_steer_withdrawn_when_the_childs_turn_stops_comes_back_to_its_box() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        watching_w(&mut a).await;
        let id = send_from_watch(&mut a, "use the release build").await;
        let lines = a.settle().await;
        none_with(&lines, "Sent to /w");
        a.emit(wrapped("/w", accepted(&id, "use the release build")))
            .await;
        a.emit(wrapped(
            "/w",
            AgentEvent::InputWithdrawn {
                ids: vec![id],
                by: WithdrawnBy::TurnStopped,
            },
        ))
        .await;
        a.emit(wrapped("/w", super::steering::cancelled())).await;
        let lines = a.settle().await;
        line_with(&lines, "\u{203a} use the release build");
        line_with(&lines, &copy::message_back("/w"));
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_message_refused_on_its_way_or_by_the_child_shows_its_reason_in_the_childs_view() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        watching_w(&mut a).await;
        let id = send_from_watch(&mut a, "first try").await;
        a.emit(AgentEvent::Rejected {
            reason: "/w has too many commands waiting; try again".to_owned(),
            id: Some(id),
            kind: Some(RejectKind::TooMany),
            request: None,
        })
        .await;
        let lines = a.settle().await;
        line_with(&lines, "/w has too many commands waiting");
        line_with(&lines, "\u{203a} first try");
        a.key(KeyCode::Enter);
        let ClientCommand::Child { command, .. } = a.next().await else {
            panic!("expected the message again");
        };
        let ClientCommand::Send { id, .. } = *command else {
            panic!("expected a send");
        };
        a.emit(wrapped(
            "/w",
            AgentEvent::Rejected {
                reason: "message tui-9 was refused".to_owned(),
                id: Some(id),
                kind: Some(RejectKind::Other("refused".to_owned())),
                request: None,
            },
        ))
        .await;
        let lines = a.settle().await;
        line_with(&lines, "message tui-9 was refused");
        line_with(&lines, "\u{203a} first try");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_reaches_the_childs_box_and_mains_question_card_waits_unseen() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(spawned(entry("/w", AgentState::Running, 0.2))).await;
        a.emit(wrapped("/w", super::processes::started(3))).await;
        a.start("plan it", 1).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.emit(tool_requested(Requested {
            block: 1,
            call_id: "q1".to_owned(),
            tool: "ask_user".to_owned(),
            action: ToolAction::Other {
                name: "ask_user".to_owned(),
            },
            target: "Database".to_owned(),
            output: OutputKind::None,
            ..Requested::default()
        }))
        .await;
        a.emit(AgentEvent::call_started("q1", "")).await;
        a.emit(AgentEvent::QuestionAsked {
            call_id: "q1".to_owned(),
            questions: vec![Question {
                header: "Database".to_owned(),
                question: "Which database?".to_owned(),
                options: vec![
                    QuestionOption {
                        label: "SQLite".to_owned(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "PostgreSQL".to_owned(),
                        description: String::new(),
                    },
                ],
                multi_select: false,
                source: None,
            }],
        })
        .await;
        sleep(Duration::from_secs(3)).await;
        let lines = a.settle().await;
        line_with(&lines, &copy::main_needs_you());
        none_with(&lines, "Which database?");
        for cols in [80, 100] {
            a.input(Input::Resize(a.screen.resize(30, cols)));
            let lines = a.settle().await;
            line_with(&lines, &copy::main_needs_you());
            assert!(!super::status_line(&lines).contains("agent continues"));
        }
        for row in 2..24 {
            click(&a, 4, row);
        }
        a.nothing_sent().await;
        a.paste("pasted for /w");
        a.key(KeyCode::Enter);
        let ClientCommand::Child { agent, command } = a.next().await else {
            panic!("expected a message to /w");
        };
        assert_eq!(agent, AgentPath::new("/w"));
        let ClientCommand::Send { text, .. } = *command else {
            panic!("expected a send");
        };
        assert_eq!(text, "pasted for /w");
        a.paste("/effort").key(KeyCode::Enter);
        let lines = a.settle().await;
        line_with(&lines, "Effort");
        none_with(&lines, copy::CARD_NO_PICKER);
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        let lines = a.settle().await;
        line_with(&lines, "Which database?");
        a.ctrl('c');
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(super::steering::cancelled()).await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn every_scroll_key_and_the_wheel_scroll_an_agents_chat_as_they_scroll_the_main_chat() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        watching_w(&mut a).await;
        notes(&a, 1..=40).await;
        let lines = a.settle().await;
        line_with(&lines, "note 40 of forty");
        none_with(&lines, "note 1 of forty");
        for _ in 0..20 {
            a.key(KeyCode::PageUp);
        }
        let lines = a.settle().await;
        line_with(&lines, "note 1 of forty");
        none_with(&lines, "note 40 of forty");
        let top = lines.clone();
        notes(&a, 41..=45).await;
        let lines = a.settle().await;
        assert_eq!(
            lines.iter().position(|l| l.contains("note 1 of forty")),
            top.iter().position(|l| l.contains("note 1 of forty")),
            "{lines:#?}"
        );
        a.key(KeyCode::PageDown);
        let lines = a.settle().await;
        none_with(&lines, "note 1 of forty");
        for _ in 0..20 {
            a.key(KeyCode::PageDown);
        }
        let lines = a.settle().await;
        line_with(&lines, "note 45 of forty");
        for (up, down) in [
            (
                super::ctrl_key(KeyCode::Home),
                super::ctrl_key(KeyCode::End),
            ),
            (super::key(KeyCode::Home), super::key(KeyCode::End)),
        ] {
            a.input(up);
            line_with(&a.settle().await, "note 1 of forty");
            a.input(down);
            line_with(&a.settle().await, "note 45 of forty");
        }
        for _ in 0..20 {
            mouse(&a, MouseKind::WheelUp, 10, 5);
        }
        let lines = a.settle().await;
        none_with(&lines, "note 45 of forty");
        let place = |lines: &[String]| {
            let line = lines.iter().find(|l| l.contains(" of forty"))?;
            line.split(" of forty").next().map(str::to_owned)
        };
        let before = place(&lines);
        a.input(Input::Resize(a.screen.resize(30, 90)));
        sleep(RESIZE_DEBOUNCE).await;
        let lines = a.settle().await;
        assert_eq!(place(&lines), before, "{lines:#?}");
        notes(&a, 46..=47).await;
        a.settle().await;
        let (column, row) = locate(&a.screen, &format!("new \u{b7} {NEW_KEY}"));
        click(&a, column, row);
        let lines = a.settle().await;
        line_with(&lines, "note 47 of forty");
        none_with(&lines, NEW_KEY);
        a.key(KeyCode::PageUp).key(KeyCode::PageUp);
        none_with(&a.settle().await, "note 47 of forty");
        send_from_watch(&mut a, "back to the end").await;
        line_with(&a.settle().await, "note 47 of forty");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

async fn notes(a: &Agent, range: std::ops::RangeInclusive<u32>) {
    for n in range {
        let id = InputId(format!("n{n}"));
        a.emit(wrapped("/w", accepted(&id, &format!("note {n} of forty"))))
            .await;
        a.emit(wrapped(
            "/w",
            AgentEvent::InputDelivered {
                ids: vec![id],
                step: 2,
            },
        ))
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn watching_a_replayed_agent_prints_no_refusal() {
    let events = replay::parse(include_str!("../fixtures/agents.jsonl")).unwrap();
    super::session_with(
        screen(),
        vec![events],
        options(Start::Stalled),
        |run| async move {
            run.say("review the plan");
            sleep(Duration::from_secs(5)).await;
            run.send(Input::Key(Key::ctrl('t')));
            run.send(super::key(KeyCode::Down));
            run.send(super::key(KeyCode::Enter));
            sleep(Duration::from_millis(200)).await;
            line_with(&run.screen.visible(), &copy::esc(copy::BACK_WORD).text());
            run.send(super::key(KeyCode::Esc));
            run.send(super::key(KeyCode::Esc));
            sleep(Duration::from_millis(200)).await;
            none_with(&run.screen.visible(), "a replayed session has no agents");
            run.quit();
        },
    )
    .await
    .unwrap();
}

fn withdrawn(id: InputId) -> AgentEvent {
    wrapped(
        "/w",
        AgentEvent::InputWithdrawn {
            ids: vec![id],
            by: WithdrawnBy::TurnStopped,
        },
    )
}

#[tokio::test(start_paused = true)]
async fn a_returned_message_leaves_focus_and_mains_draft_alone_and_waits_in_its_box() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        watching_w(&mut a).await;
        let id = send_from_watch(&mut a, "try the release build").await;
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.paste("a draft for main");
        a.emit(wrapped("/w", accepted(&id, "try the release build")))
            .await;
        a.emit(withdrawn(id)).await;
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        none_with(&lines, &copy::message_to("/w"));
        line_with(&lines, "Your message to /w came back \u{b7} ctrl-t");
        a.paste(" and more");
        let lines = a.settle().await;
        line_with(&lines, "a draft for main and more");
        none_with(&lines, "try the release build");
        a.ctrl('c');
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        let lines = a.settle().await;
        line_with(&lines, "\u{203a} try the release build");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.nothing_sent().await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_returned_message_never_replaces_a_draft_open_for_another_agent() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(spawned(entry("/c", idle(None), 0.1))).await;
        worker_mid_bash(&a).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Down);
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        let id = send_from_watch(&mut a, "for w").await;
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/c", true));
        a.paste("draft for c");
        a.emit(wrapped("/w", accepted(&id, "for w"))).await;
        a.emit(withdrawn(id)).await;
        let lines = a.settle().await;
        line_with(&lines, "\u{203a} draft for c");
        none_with(&lines, "\u{203a} for w");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/c", false));
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Down);
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        let lines = a.settle().await;
        line_with(&lines, "\u{203a} for w");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.nothing_sent().await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn held_and_undelivered_messages_to_an_agent_are_named_in_its_own_words() {
    ended_holding_text(false).await;
}

#[tokio::test(start_paused = true)]
async fn an_agent_list_that_shows_the_end_first_keeps_the_agents_text() {
    ended_holding_text(true).await;
}

#[tokio::test(start_paused = true)]
async fn a_childs_text_survives_a_restart_or_a_second_cut() {
    use super::steering::{attached, steering_finished};
    use wisp_client::link::LinkEnd;
    use wisp_protocol::RestartState;
    use wisp_tui::app::Exit;
    for (restart, cancelled) in [(true, false), (true, true), (false, false)] {
        let mut opts = options(Start::Stalled);
        let ended = attached(&mut opts, 1);
        let finished = steering_finished(screen(), opts, facts(), |mut a| async move {
            chat_on_work(&mut a).await;
            let text = if restart { "queued for w" } else { "sent to w" };
            a.paste(text).key(KeyCode::Enter);
            let ClientCommand::Send { id, .. } = to_w(&mut a).await else {
                panic!("expected a message to /w")
            };
            if restart {
                a.emit(wrapped("/w", accepted(&id, text))).await;
            }
            a.paste("draft for w");
            if restart {
                a.emit(AgentEvent::Restart {
                    state: RestartState::GettingReady,
                    open: Vec::new(),
                })
                .await;
                a.emit(changed("/w", AgentState::Ended, Activity::Idle, 0.0))
                    .await;
                none_with(&a.settle().await, "/w ended");
                if cancelled {
                    a.emit(AgentEvent::Restart {
                        state: RestartState::Cancelled,
                        open: Vec::new(),
                    })
                    .await;
                    assert!(
                        drain(&mut a)
                            .await
                            .iter()
                            .any(|command| matches!(command, ClientCommand::ListAgents { .. }))
                    );
                    a.emit(AgentEvent::AgentsSnapshot {
                        request: None,
                        agents: vec![entry("/w", AgentState::Ended, 0.0)],
                    })
                    .await;
                    line_with(&a.settle().await, &copy::agent_ended("/w", 2, " \u{b7} "));
                    a.key(KeyCode::Up);
                    line_with(&a.settle().await, "\u{203a} draft for w");
                    a.cut(ended).await;
                } else {
                    ended.send(Some(LinkEnd::Restarting)).unwrap();
                    drop(a.events);
                    sleep(Duration::from_millis(20)).await;
                }
            } else {
                a.settle().await;
                a.cut(ended).await;
            }
        })
        .await;
        if cancelled {
            assert_eq!(finished.exit.unwrap(), Exit::Cut);
            continue;
        }
        assert_eq!(
            finished.exit.unwrap(),
            if restart { Exit::Restarting } else { Exit::Cut }
        );
        let mut unsent = finished.unsent.unwrap();
        assert!(!unsent.is_empty());
        let expected = if restart {
            "To /w:\nqueued for w\n\ndraft for w"
        } else {
            "To /w:\nsent to w\n\ndraft for w"
        };
        assert_eq!(unsent.texts(), [expected]);
        if restart {
            continue;
        }
        unsent.reopened(&[]);
        let mut opts = options(Start::Stalled);
        opts.unsent = Some(unsent);
        let ended = attached(&mut opts, 1);
        let finished = steering_finished(screen(), opts, facts(), |a| async move {
            a.cut(ended).await;
        })
        .await;
        assert_eq!(finished.exit.unwrap(), Exit::Cut);
        let unsent = finished.unsent.unwrap();
        assert_eq!(unsent.texts(), [expected]);
        let mut opts = options(Start::Stalled);
        opts.unsent = Some(unsent);
        steering_on(screen(), opts, |mut a| async move {
            a.emit(AgentEvent::AgentsSnapshot { request: None, agents: vec![entry("/w", AgentState::Running, 0.0)] }).await;
            a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
            assert_eq!(a.next().await, watch("/w", true));
            line_with(&a.settle().await, "sent to w");
            a.key(KeyCode::Enter);
            assert!(matches!(to_w(&mut a).await, ClientCommand::Send { text, .. } if text == "sent to w\n\ndraft for w"));
        }).await;
    }
}

async fn ended_holding_text(late_list: bool) {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        watching_w(&mut a).await;
        let id = send_from_watch(&mut a, "queued for w").await;
        a.emit(wrapped("/w", accepted(&id, "queued for w"))).await;
        a.emit(wrapped(
            "/w",
            AgentEvent::InputHeld {
                ids: vec![id],
                by: WithdrawnBy::TurnStopped,
            },
        ))
        .await;
        a.emit(wrapped("/w", super::steering::cancelled())).await;
        let lines = a.settle().await;
        let held = copy::agent_held("/w", false, 1, " \u{b7} ");
        assert_eq!(held, "/w stopped \u{b7} 1 queued message held");
        line_with(&lines, &held);
        none_with(&lines, "enter sends");
        let id = send_from_watch(&mut a, "never read").await;
        a.emit(wrapped("/w", accepted(&id, "never read"))).await;
        a.paste("half typed");
        a.key(KeyCode::Up);
        line_with(&a.settle().await, "\u{203a} never read");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        a.paste("main draft");
        if late_list {
            a.emit(AgentEvent::AgentsSnapshot {
                request: None,
                agents: vec![entry("/w", AgentState::Ended, 0.2)],
            })
            .await;
        }
        a.emit(changed("/w", AgentState::Ended, Activity::Idle, 0.2))
            .await;
        let lines = a.settle().await;
        line_with(&lines, &copy::agent_ended("/w", 3, " \u{b7} "));
        line_with(&lines, "\u{203a} main draft");
        for recalled in ["half typed", "never read", "queued for w"] {
            a.key(KeyCode::Up);
            let lines = a.settle().await;
            line_with(&lines, &format!("\u{203a} {recalled}"));
        }
        a.nothing_sent().await;
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_in_an_agents_chat_goes_to_its_box_and_never_mains() {
    let (dir, options) = pasting("agent-message");
    steering_facts(screen(), options, seeing(facts()), |mut a| async move {
        worker_mid_bash(&a).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        let lines = a.settle().await;
        line_with(&lines, &copy::message_to("/w"));
        a.paste(PASTED);
        paste_from_clipboard(&a.inputs).await;
        assert_eq!(clipboard_leaks(&a.screen, &dir), NO_LEAKS);
        a.key(KeyCode::Enter);
        let ClientCommand::Child { agent, command } = a.next().await else {
            panic!("expected a message to /w");
        };
        assert_eq!(agent, AgentPath::new("/w"));
        let ClientCommand::Send { text, .. } = *command else {
            panic!("expected a send");
        };
        assert!(text == PASTED, "the message is not the text paste alone");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        assert!(
            !a.screen.caret_line().contains(PASTED),
            "the text paste reached the composer"
        );
        assert_eq!(clipboard_leaks(&a.screen, &dir), NO_LEAKS);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_step_in_from_main_waits_behind_an_agents_chat_and_takes_none_of_its_keys() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        worker_mid_bash(&a).await;
        a.emit(wrapped("/w", super::processes::started(3))).await;
        a.emit(wrapped("/w", super::completed())).await;
        sleep(Duration::from_secs(3)).await;
        a.start("install it", 1).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.paste("typed so far");
        a.emit(waiting_terminal(5)).await;
        a.emit(asked(5)).await;
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        line_with(&lines, &copy::main_needs_you());
        none_with(&lines, &copy::step_in_title(5));
        for cols in [80, 100] {
            a.input(Input::Resize(a.screen.resize(30, cols)));
            let lines = a.settle().await;
            line_with(&lines, &copy::main_needs_you());
            none_with(&lines, &copy::step_in_title(5));
        }
        press(&a, 'y');
        a.paste(PASTED);
        a.nothing_sent().await;
        let lines = a.settle().await;
        line_with(&lines, "\u{203a} typed so fary");
        line_with(&lines, &copy::main_needs_you());
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        let lines = a.settle().await;
        line_with(&lines, &copy::step_in_title(5));
        a.emit(ask_closed()).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.key(KeyCode::Enter);
        let ClientCommand::Child { agent, command } = a.next().await else {
            panic!("expected a message to /w");
        };
        assert_eq!(agent, AgentPath::new("/w"));
        let ClientCommand::Send { text, .. } = *command else {
            panic!("expected a send");
        };
        assert!(text == "typed so fary", "the text paste joined the message");
    })
    .await;
}

fn work_accounts() -> super::SessionFacts {
    let accounts = [
        ("claude-work", Credential::AnthropicPlan),
        ("direct", Credential::ApiKey),
        ("chatgpt", Credential::ChatGptPlan),
    ];
    crate::scenes::on_accounts(super::facts(), &accounts)
}

fn listed(request: RequestId, pair: [(&str, Credential); 2], expired: &[&str]) -> AgentEvent {
    let AgentEvent::SettingsSnapshot(mut snapshot) = super::described(request.0) else {
        unreachable!();
    };
    let account = |(name, credential): (&str, Credential)| EndpointSummary {
        credential: Some(credential.wire()),
        expired: expired.contains(&name),
        ..super::endpoints::summary(name, false)
    };
    snapshot.endpoints = pair.map(account).to_vec();
    wrapped("/w", AgentEvent::SettingsSnapshot(snapshot))
}

pub(super) async fn chat_on_work(a: &mut Agent) {
    let worker = super::agents::on_account("/w", "claude-work", 40_000);
    a.emit(spawned(worker)).await;
    a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
    assert_eq!(a.next().await, watch("/w", true));
}

async fn to_w(a: &mut Agent) -> ClientCommand {
    let sent = drain(a).await;
    let child = sent.into_iter().rev().find_map(|c| match c {
        ClientCommand::Child { agent, command } if agent == AgentPath::new("/w") => Some(*command),
        _ => None,
    });
    child.unwrap_or_else(|| panic!("no command for /w"))
}

async fn describe_w(a: &mut Agent) -> RequestId {
    let ClientCommand::DescribeSettings { request } = to_w(a).await else {
        panic!("expected DescribeSettings for /w");
    };
    request
}

fn switched(request: RequestId, endpoint: &str) -> AgentEvent {
    let changed = crate::events::settings_changed(crate::events::Changed {
        answers: Some(request),
        endpoint: endpoint.to_owned(),
        model: "claude-sonnet-5-5".to_owned(),
        effort: wisp_protocol::Effort::High,
        cache: wisp_protocol::CacheEffect::Rebuilt {
            reason: wisp_protocol::RebuildReason::EndpointSwitch,
        },
        ..crate::events::Changed::default()
    });
    wrapped("/w", changed)
}

#[tokio::test(start_paused = true)]
async fn the_agents_model_and_effort_change_from_its_chat() {
    steering_facts(
        screen(),
        options(Start::Stalled),
        work_accounts(),
        |mut a| async move {
            chat_on_work(&mut a).await;
            a.paste("/model").key(KeyCode::Enter);
            let request = describe_w(&mut a).await;
            let lines = a.settle().await;
            line_with(&lines, "claude-work");
            none_with(&lines, "direct");
            none_with(&lines, "chatgpt");
            let pair = [
                ("claude-work", Credential::AnthropicPlan),
                ("direct", Credential::ApiKey),
            ];
            a.emit(listed(request, pair, &[])).await;
            let lines = a.settle().await;
            line_with(&lines, "opus-5-5 \u{2713}");
            line_with(&lines, "direct");
            none_with(&lines, "chatgpt");
            a.paste("haiku");
            line_with(&a.settle().await, &copy::matching("haiku"));
            for _ in 0..5 {
                a.key(KeyCode::Backspace);
            }
            a.key(KeyCode::Up).key(KeyCode::Up).key(KeyCode::Right);
            a.key(KeyCode::Down).key(KeyCode::Enter);
            let ClientCommand::ChangeSettings {
                request,
                changes,
                save: false,
            } = to_w(&mut a).await
            else {
                panic!("expected a session-only change for /w");
            };
            let to = |model: &str| Some(model.to_owned());
            let direct =
                ClientCommand::shortcut_changes(to("direct"), to("claude-sonnet-5-5"), None);
            assert_eq!(changes, direct);
            a.emit(switched(request, "direct")).await;
            let lines = a.settle().await;
            let note = line_with(&lines, "Switched to").to_owned();
            a.emit(wrapped("/w", AgentEvent::turn_started(2))).await;
            let ClientCommand::ListAgents { request } = a.next().await else {
                panic!("expected ListAgents");
            };
            let agents = vec![AgentEntry {
                model: "claude-sonnet-5-5".to_owned(),
                ..super::agents::on_account("/w", "direct", 40_000)
            }];
            let request = Some(request);
            a.emit(AgentEvent::AgentsSnapshot { request, agents }).await;
            a.key(KeyCode::BackTab);
            let ClientCommand::ChangeSettings { save: false, .. } = to_w(&mut a).await else {
                panic!("expected shift-tab to change /w's effort");
            };
            a.paste("/effort").key(KeyCode::Enter);
            let lines = a.settle().await;
            line_with(&lines, "Effort");
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            line_with(&lines, &copy::message_to("/w"));
            a.key(KeyCode::Esc);
            assert_eq!(a.next().await, watch("/w", false));
            none_with(&a.settle().await, &note);
            a.ctrl('t').key(KeyCode::Down);
            line_with(&a.settle().await, "direct");
            a.key(KeyCode::Enter);
            assert_eq!(a.next().await, watch("/w", true));
            a.paste("/model").key(KeyCode::Enter);
            let request = describe_w(&mut a).await;
            a.emit(listed(request, pair, &[])).await;
            line_with(&a.settle().await, "sonnet-5-5 \u{2713}");
            a.key(KeyCode::Esc);
            a.paste("/effort").key(KeyCode::Enter);
            line_with(&a.settle().await, "Effort");
            a.emit(changed("/w", AgentState::Ended, Activity::Idle, 0.2))
                .await;
            let lines = a.settle().await;
            none_with(&lines, "Effort");
            line_with(&lines, &copy::agent_ended("/w", 0, " \u{b7} "));
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

fn done(answers: RequestId, outcome: CommandOutcome) -> AgentEvent {
    AgentEvent::CommandDone {
        answers,
        outcome,
        args_safe_to_echo: false,
    }
}

fn report(request: RequestId) -> AgentEvent {
    let report = serde_json::from_value(json!({
        "answers": request.0, "model": "claude-opus-5-5", "endpoint": "claude-work",
        "fill": {"measured": {"used": 40_000, "turn": 1, "step": 1}},
        "next": 40_000, "auto_compact": false, "epoch": 0,
    }));
    wrapped("/w", AgentEvent::ContextReport(report.unwrap()))
}

#[tokio::test(start_paused = true)]
async fn the_agents_reports_and_plugin_commands_answer_in_its_chat() {
    steering_facts(
        screen(),
        options(Start::Stalled),
        work_accounts(),
        |mut a| async move {
            let hint = Some("/go runs between turns");
            let go = super::plugin_commands::command("go", &[], &[], hint);
            let note = super::plugin_commands::command("note", &[], &[], None);
            let testkit = super::plugin_commands::testkit(vec![go, note]);
            a.emit(crate::shared::catalog(vec![testkit])).await;
            a.start("plan it", 1).await;
            chat_on_work(&mut a).await;
            a.paste("/context").key(KeyCode::Enter);
            let ClientCommand::ContextReport { request, .. } = to_w(&mut a).await else {
                panic!("expected a context report for /w");
            };
            a.emit(report(request)).await;
            let lines = a.settle().await;
            let shown = line_with(&lines, "claude-opus-5-5 \u{b7} claude-work").to_owned();
            a.paste("/go").key(KeyCode::Enter);
            let ClientCommand::RunPluginCommand { name, request, .. } = to_w(&mut a).await else {
                panic!("expected a plugin command for /w");
            };
            assert_eq!(name, "testkit:go");
            let went = CommandOutcome::Note {
                text: "went".to_owned(),
            };
            a.emit(wrapped("/w", done(request, went))).await;
            line_with(&a.settle().await, "went");
            a.paste("/compact").key(KeyCode::Enter);
            let ClientCommand::Compact { .. } = to_w(&mut a).await else {
                panic!("expected a compaction of /w");
            };
            a.emit(wrapped("/w", crate::shared::started())).await;
            sleep(Duration::from_secs(2)).await;
            let lines = a.settle().await;
            let row = line_with(&lines, "Compacting\u{2026}");
            assert!(row.contains(&copy::esc(copy::BACK_WORD).text()), "{row}");
            a.paste("/compact").key(KeyCode::Enter);
            line_with(&a.settle().await, wisp_protocol::ALREADY_COMPACTING);
            a.nothing_sent().await;
            a.ctrl('c').key(KeyCode::Esc);
            assert_eq!(drain(&mut a).await, [watch("/w", false)]);
            a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
            assert_eq!(a.next().await, watch("/w", true));
            a.emit(wrapped("/w", AgentEvent::turn_started(2))).await;
            let alpha = send_from_watch(&mut a, "alpha steer").await;
            a.emit(wrapped("/w", accepted(&alpha, "alpha steer"))).await;
            a.paste("/note").key(KeyCode::Enter);
            let ClientCommand::RunPluginCommand { request, .. } = to_w(&mut a).await else {
                panic!("expected /note for /w");
            };
            let bravo = send_from_watch(&mut a, "bravo steer").await;
            a.emit(wrapped("/w", accepted(&bravo, "bravo steer"))).await;
            let input = InputId("wisp-command-1".to_owned());
            a.emit(wrapped("/w", accepted(&input, "charlie reply")))
                .await;
            a.emit(wrapped("/w", done(request, CommandOutcome::Sent { input })))
                .await;
            let lines = a.settle().await;
            let at = |text: &str| super::steering::position(&lines, text);
            assert!(at("alpha steer") < at("charlie reply"), "{lines:#?}");
            assert!(at("charlie reply") < at("bravo steer"), "{lines:#?}");
            a.paste("/account add").key(KeyCode::Enter);
            assert_eq!(drain(&mut a).await, [watch("/w", false)]);
            let lines = a.settle().await;
            line_with(&lines, wisp_render::chrome::endpoint_card::NO_CARD);
            none_with(&lines, "went");
            none_with(&lines, &shown);
            a.ctrl('c');
            assert_eq!(a.next().await, ClientCommand::Cancel);
            a.emit(super::steering::cancelled()).await;
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_typed_account_answer_does_not_follow_focus_to_another_chat() {
    for destination in ["main", "other", "closed"] {
        steering_facts(
            screen(),
            options(Start::Stalled),
            work_accounts(),
            |mut a| async move {
                chat_on_work(&mut a).await;
                a.paste("/account chatgpt").key(KeyCode::Enter);
                let request = describe_w(&mut a).await;
                if destination == "closed" {
                    a.emit(changed("/w", AgentState::Ended, Activity::Idle, 0.2))
                        .await;
                    a.settle().await;
                } else {
                    a.key(KeyCode::Esc);
                    assert_eq!(a.next().await, watch("/w", false));
                    if destination == "other" {
                        a.emit(spawned(super::agents::on_account("/c", "direct", 40_000)))
                            .await;
                        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
                        assert_eq!(a.next().await, watch("/c", true));
                    }
                }
                let snapshot = super::settings_screen::snapshot_of(request);
                a.emit(wrapped("/w", AgentEvent::SettingsSnapshot(snapshot)))
                    .await;
                a.nothing_sent().await;
                a.ctrl('c').ctrl('c');
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn an_agents_account_picker_shows_another_providers_row_and_reasoning_note() {
    let mut facts = work_accounts();
    facts.on_accounts.push(wisp_render::chrome::AccountFacts {
        account: "chatgpt".into(),
        model: "gpt-6.1-sol".into(),
        rates: Some(wisp_render::chrome::ModelRates {
            writes: None,
            input_per_mtok: 2.0,
            output_per_mtok: 10.0,
            cache_read_per_mtok: 0.2,
            long_prompt: None,
        }),
        max_output: None,
    });
    steering_facts(
        screen(),
        options(Start::Stalled),
        facts,
        |mut a| async move {
            chat_on_work(&mut a).await;
            a.paste("/account").key(KeyCode::Enter);
            let request = describe_w(&mut a).await;
            let pair = [
                ("claude-work", Credential::AnthropicPlan),
                ("chatgpt", Credential::ChatGptPlan),
            ];
            let mut listed = listed(request, pair, &[]);
            let AgentEvent::Agent { event, .. } = &mut listed else {
                unreachable!()
            };
            let AgentEvent::SettingsSnapshot(snapshot) = event.as_mut() else {
                unreachable!()
            };
            snapshot.endpoints[1].provider = "openai".into();
            snapshot.endpoints[1].default_model = Some("gpt-6.1-sol".into());
            a.emit(listed).await;
            a.key(KeyCode::Down);
            let lines = a.settle().await;
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("chatgpt") && l.contains("ChatGPT subscription")),
                "{lines:#?}"
            );
            let preview = line_with(&lines, "Moves this chat to");
            assert!(
                preview.contains("chatgpt") && preview.contains("cache rebuild ~$"),
                "{preview}"
            );
            line_with(&lines, "On chatgpt, earlier reasoning won't carry over.");
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_switch_in_an_agents_chat_shows_one_note_there() {
    steering_facts(screen(), options(Start::Stalled), work_accounts(), |mut a| async move {
        chat_on_work(&mut a).await;
        a.paste("/account chatgpt").key(KeyCode::Enter);
        let request = describe_w(&mut a).await;
        let snapshot = super::settings_screen::snapshot_of(request);
        a.emit(wrapped("/w", AgentEvent::SettingsSnapshot(snapshot))).await;
        let ClientCommand::ChangeSettings { request, save: false, .. } = to_w(&mut a).await else {
            panic!("expected /account chatgpt to go to /w");
        };
        let reason = "account chatgpt was removed in this chat, or edited so it no longer loads; agents can use it again from the next session";
        let refused = AgentEvent::Rejected {
            reason: reason.to_owned(),
            id: None,
            kind: None,
            request: Some(request),
        };
        a.emit(wrapped("/w", refused)).await;
        let lines = a.settle().await;
        let lead = &copy::settings_refused(reason)[..40];
        assert_eq!(lines.iter().filter(|l| l.contains(lead)).count(), 1, "{lines:#?}");
        none_with(&lines, &copy::rejected(reason)[..20]);
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        none_with(&a.settle().await, "was removed in this chat");
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.paste("/model").key(KeyCode::Enter);
        let request = describe_w(&mut a).await;
        let pair = [
            ("claude-work", Credential::AnthropicPlan),
            ("direct", Credential::ApiKey),
        ];
        a.emit(listed(request, pair, &["direct"])).await;
        a.key(KeyCode::Up).key(KeyCode::Enter);
        let sent = drain(&mut a).await;
        assert!(sent.contains(&watch("/w", false)), "{sent:#?}");
        none_with(&a.settle().await, &copy::message_to("/w"));
        a.ctrl('c').ctrl('c');
    })
    .await;
}

async fn rewind_w(a: &mut Agent, points: Vec<PointInfo>) {
    a.paste("/rewind").key(KeyCode::Enter);
    let ClientCommand::RewindPoints { request } = to_w(a).await else {
        panic!("expected RewindPoints for /w");
    };
    let listed = AgentEvent::RewindPoints {
        answers: request,
        points,
    };
    a.emit(wrapped("/w", listed)).await;
}

async fn rewind_w_to(a: &mut Agent, turn: u64) -> RequestId {
    a.key(KeyCode::Enter).key(KeyCode::Enter);
    let ClientCommand::Rewind { to, request } = to_w(a).await else {
        panic!("expected a rewind of /w");
    };
    assert_eq!(to, RewindPoint::Turn { turn });
    request
}

#[tokio::test(start_paused = true)]
async fn the_agents_rewind_takes_it_back_from_its_chat() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.paste("main draft");
        watching_w(&mut a).await;
        a.paste("/rewind").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        line_with(&lines, copy::REWIND_AFTER_TURN);
        line_with(&lines, "\u{203a} /rewind");
        a.ctrl('c');
        a.emit(wrapped("/w", super::completed())).await;
        let reported = idle(Some(IdleOutcome::Reported));
        a.emit(changed("/w", reported, Activity::Idle, 0.2)).await;
        let brief = point(1, "\u{21bb} a message from /lead");
        let mut build = point(2, "use the release build");
        build.quote.warm = Warmth::OtherModel {
            model: "claude-sonnet-5-5".to_owned(),
        };
        rewind_w(&mut a, vec![brief.clone(), build]).await;
        let lines = a.settle().await;
        line_with(&lines, "turn 1  14:02  \u{21bb} a message from /lead");
        line_with(&lines, "on claude-opus-5-5");
        let request = rewind_w_to(&mut a, 2).await;
        line_with(&a.settle().await, copy::REWIND_SENT);
        let id = InputId("w-2".to_owned());
        let back = vec![returned(&id, "use the release build")];
        a.emit(wrapped("/w", rewound(request, 2, back))).await;
        line_with(&a.settle().await, "rewound to before turn 2");
        assert!(a.screen.caret_line().contains("use the release build"));
        a.key(KeyCode::Esc);
        assert_eq!(drain(&mut a).await, [watch("/w", false)]);
        a.settle().await;
        assert!(a.screen.caret_line().contains("main draft"));
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        a.ctrl('c');
        rewind_w(&mut a, vec![brief.clone()]).await;
        let request = rewind_w_to(&mut a, 1).await;
        a.emit(wrapped("/w", rewound(request, 1, Vec::new()))).await;
        let lines = a.settle().await;
        line_with(&lines, &copy::message_to("/w"));
        line_with(&lines, "rewound to before turn 1");
        none_with(&lines, "cargo test");
        rewind_w(&mut a, vec![brief.clone()]).await;
        let request = rewind_w_to(&mut a, 1).await;
        let running = AgentEvent::Rejected {
            id: None,
            request: Some(request),
            reason: "a turn is running".to_owned(),
            kind: Some(RejectKind::TurnRunning),
        };
        a.emit(wrapped("/w", running)).await;
        line_with(
            &a.settle().await,
            &copy::not_rewound(copy::REWIND_AFTER_TURN),
        );
        rewind_w(&mut a, vec![brief]).await;
        a.emit(changed("/w", AgentState::Ended, Activity::Idle, 0.2))
            .await;
        none_with(&a.settle().await, copy::REWIND_NAME);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_agents_rewind_that_cuts_nothing_still_scrolls_its_chat_to_the_bottom() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        watching_w(&mut a).await;
        notes(&a, 1..=40).await;
        a.emit(wrapped("/w", super::completed())).await;
        a.emit(changed("/w", idle(None), Activity::Idle, 0.2)).await;
        for _ in 0..20 {
            a.key(KeyCode::PageUp);
        }
        line_with(&a.settle().await, "note 1 of forty");
        rewind_w(&mut a, vec![point(2, "second")]).await;
        let request = rewind_w_to(&mut a, 2).await;
        a.emit(wrapped("/w", rewound(request, 2, Vec::new()))).await;
        let lines = a.settle().await;
        line_with(&lines, "rewound to before turn 2");
        none_with(&lines, "note 1 of forty");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_agents_rewind_names_a_lost_image_in_its_chat_and_leaves_mains_retry() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        super::retry::failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        worker_on(&a, entry("/w", idle(None), 0.2)).await;
        a.emit(wrapped("/w", super::completed())).await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        rewind_w(&mut a, vec![point(1, "look at this")]).await;
        let request = rewind_w_to(&mut a, 1).await;
        let mut back = returned(&InputId("w-1".to_owned()), "look at this");
        back.missing = vec![MissingImage {
            ordinal: 1,
            sha256: "b".repeat(64),
        }];
        a.emit(wrapped("/w", rewound(request, 1, vec![back]))).await;
        let lost = copy::image_not_restored(&[1]);
        line_with(&a.settle().await, &lost);
        a.key(KeyCode::Esc);
        assert_eq!(drain(&mut a).await, [watch("/w", false)]);
        let lines = a.settle().await;
        line_with(&lines, "\u{203a} Retry");
        none_with(&lines, &lost);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn ps_lists_every_agents_programs_in_any_chat_and_stops_through_their_agent() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.start("serve it", 1).await;
        a.paste("/ps").key(KeyCode::Enter);
        let described = |id, command, title: &str| {
            let mut call = bash(id, command);
            if let AgentEvent::ToolCallRequested { display, .. } = &mut call {
                display.title = Some(title.to_owned());
            }
            call
        };
        worker_mid_bash(&a).await;
        a.emit(wrapped(
            "/w",
            described("toolu_3", "cargo test --workspace", "Check child code"),
        ))
        .await;
        a.emit(wrapped(
            "/w",
            described("toolu_4", "python3 -i", "Child shell"),
        ))
        .await;
        a.emit(wrapped("/w", program(3, "cargo test --workspace")))
            .await;
        a.emit(wrapped("/w", output(3, "test agents::watch ... ok\n")))
            .await;
        a.emit(wrapped("/w", super::processes::terminal(4))).await;
        a.emit(described("toolu_3", "npm run dev", "Preview site"))
            .await;
        a.emit(super::processes::started(3)).await;
        let listed = [
            "   main",
            "   /w",
            "3  Preview site  npm run dev",
            "3  Check child code  cargo test",
        ];
        let lines = a.settle().await;
        for row in listed {
            line_with(&lines, row);
        }
        press(&a, 'x');
        line_with(&a.settle().await, &copy::press_x_again("3 of /w"));
        press(&a, 'x');
        let stop = ClientCommand::StopProcess { id: 3 };
        assert_eq!(a.next().await, child("/w", stop));
        a.key(KeyCode::Esc);
        watching_w(&mut a).await;
        a.ctrl(']');
        let waiting = copy::nothing_waiting("\u{b7}");
        assert!(super::status_line(&a.settle().await).contains(&waiting));
        a.paste("/ps").key(KeyCode::Enter);
        let lines = a.settle().await;
        for row in listed {
            line_with(&lines, row);
        }
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", false));
        let lines = a.settle().await;
        assert!(lines[0].contains("Preview site"), "{lines:#?}");
        line_with(&lines, &back_keys().text());
        a.key(KeyCode::Esc);
        a.emit(changed("/w", AgentState::Ended, Activity::Idle, 0.2))
            .await;
        let lines = a.settle().await;
        line_with(&lines, "   /w \u{b7} ended");
        line_with(&lines, "3  Check child code  cargo test");
        line_with(&lines, "4  Child shell  python3 -i");
        a.key(KeyCode::Esc);
        let running = |n| copy::running_count(n);
        assert!(super::status_line(&a.settle().await).contains(&running(3)));
        let ended = ProcessEnd::Stopped {
            by: StoppedBy::SessionEnded,
        };
        let (bytes, lines, duration_ms) = (0, 0, 1000);
        let end = AgentEvent::ProcessEnded {
            id: 4,
            end: ended,
            bytes,
            lines,
            duration_ms,
        };
        a.emit(wrapped("/w", end)).await;
        assert!(super::status_line(&a.settle().await).contains(&running(2)));
        a.paste("/ps").key(KeyCode::Enter);
        let lines = a.settle().await;
        let shell = line_with(&lines, "4  Child shell  python3 -i");
        assert!(shell.contains("stopped"), "{lines:#?}");
        a.key(KeyCode::Down).key(KeyCode::Down);
        press(&a, 'x');
        press(&a, 'x');
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        a.emit(super::steering::cancelled()).await;
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

pub(super) async fn take_over(a: &mut Agent, agent: &str, id: u32) {
    assert_eq!(
        a.next().await,
        child(agent, ClientCommand::AttachProcess { id, size: None })
    );
    a.settle().await;
    let ClientCommand::Child { command, .. } = a.next().await else {
        panic!("expected the view's size for {agent}");
    };
    let ClientCommand::AttachProcess {
        size: Some(size), ..
    } = *command
    else {
        panic!("expected the view's size, got {command:?}");
    };
    a.emit(wrapped(agent, AgentEvent::ProcessAttached { id, size }))
        .await;
}

fn typed_w(id: u32, input: wisp_protocol::AttachInput) -> ClientCommand {
    let items = wisp_protocol::AttachItems(vec![input]);
    child("/w", ClientCommand::ProcessInput { id, items })
}

#[tokio::test(start_paused = true)]
async fn taking_over_an_agents_program_goes_through_it_from_any_chat() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        worker_mid_bash(&a).await;
        a.emit(wrapped("/w", waiting_terminal(4))).await;
        a.paste("/ps").key(KeyCode::Enter);
        a.key(KeyCode::Char('a'));
        take_over(&mut a, "/w", 4).await;
        let lines = a.settle().await;
        line_with(&lines, "Process 4 of /w  sudo apt install x");
        line_with(&lines, copy::ATTACH_WARNING);
        let size = wisp_protocol::TermSize {
            cols: 100,
            rows: 30,
        };
        let reason = wisp_protocol::DetachReason::Ended;
        let mains = AgentEvent::ProcessDetached {
            id: 4,
            size,
            reason,
            typed: true,
        };
        a.emit(mains.clone()).await;
        line_with(&a.settle().await, copy::ATTACH_WARNING);
        let status = super::status_line(&a.settle().await);
        assert!(status.contains(&copy::typing_into("4 of /w")), "{status}");
        a.key(KeyCode::Char('p'));
        let text = wisp_protocol::AttachInput::Text { text: "p".into() };
        assert_eq!(a.next().await, typed_w(4, text));
        a.emit(wrapped("/w", mains)).await;
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let hidden = wisp_protocol::NeedsInputKind::HiddenInput;
        let asks = AgentEvent::ProcessNeedsInput {
            id: 4,
            kind: hidden,
        };
        a.emit(wrapped("/w", asks)).await;
        let status = super::status_line(&a.settle().await);
        let waits = copy::waiting_for_password("4 of /w");
        assert!(status.contains(&waits), "{status}");
        a.ctrl(']');
        take_over(&mut a, "/w", 4).await;
        none_with(&a.settle().await, copy::ATTACH_WARNING);
        a.ctrl(']');
        let detach = child("/w", ClientCommand::DetachProcess { id: 4 });
        assert_eq!(a.next().await, detach);
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/w", true));
        let status = super::status_line(&a.settle().await);
        assert!(status.contains(&copy::waiting_for_password(4)), "{status}");
        a.ctrl(']');
        take_over(&mut a, "/w", 4).await;
        let lines = a.settle().await;
        line_with(&lines, "Process 4  sudo apt install x");
        let status = super::status_line(&lines);
        assert!(status.contains(&copy::typing_into(4)), "{status}");
        assert!(
            !status.contains(&copy::esc(copy::BACK_WORD).text()),
            "{status}"
        );
        a.key(KeyCode::Esc);
        let esc =
            wisp_protocol::AttachKey::new(wisp_protocol::AttachKeyCode::Esc, false, false, false);
        let esc = wisp_protocol::AttachInput::Key { key: esc.unwrap() };
        assert_eq!(a.next().await, typed_w(4, esc));
        let refused = |kind, reason: &str| AgentEvent::Rejected {
            reason: reason.to_owned(),
            id: None,
            kind: Some(kind),
            request: None,
        };
        let busy = "/w has too many commands waiting; try again";
        a.emit(refused(RejectKind::TooMany, busy)).await;
        a.emit(refused(RejectKind::NoProcess, "no process 4")).await;
        let status = super::status_line(&a.settle().await);
        assert!(status.contains(&copy::typing_into(4)), "{status}");
        let gone = RejectKind::Other(wisp_protocol::agents::NO_SUCH_AGENT.to_owned());
        a.emit(wrapped("/v", refused(gone.clone(), "/v has ended")))
            .await;
        let status = super::status_line(&a.settle().await);
        assert!(status.contains(&copy::typing_into(4)), "{status}");
        a.emit(refused(gone, "/w has ended")).await;
        let status = super::status_line(&a.settle().await);
        assert!(!status.contains(&copy::typing_into(4)), "{status}");
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/w", false));
        line_with(&a.settle().await, busy);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_agents_program_that_ends_while_taken_over_closes_the_view_and_says_so() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        worker_mid_bash(&a).await;
        a.emit(wrapped("/w", waiting_terminal(4))).await;
        let kind = wisp_protocol::NeedsInputKind::HiddenInput;
        a.emit(wrapped("/w", AgentEvent::ProcessNeedsInput { id: 4, kind }))
            .await;
        a.settle().await;
        a.ctrl(']');
        take_over(&mut a, "/w", 4).await;
        line_with(&a.settle().await, "Process 4 of /w");
        let size = wisp_protocol::TermSize {
            cols: 100,
            rows: 30,
        };
        let reason = wisp_protocol::DetachReason::Ended;
        let left = AgentEvent::ProcessDetached {
            id: 4,
            size,
            reason,
            typed: true,
        };
        a.emit(wrapped("/w", exited(4, 0))).await;
        a.emit(wrapped("/w", left)).await;
        let lines = a.settle().await;
        none_with(&lines, "Process 4 of /w");
        let status = super::status_line(&lines);
        assert!(status.contains("process 4 of /w exited (0)"), "{status}");
        a.ctrl('c').ctrl('c');
    })
    .await;
}
