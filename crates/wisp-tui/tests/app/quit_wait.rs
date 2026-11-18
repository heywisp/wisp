use std::future::Future;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep};
use wisp_client::QuitBound;
use wisp_protocol::{AgentEvent, AgentState, ClientCommand, RequestId};
use wisp_render::theme::glyphs::{ASCII, GlyphSet};
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::{Exit, Finished, Session, run_to_end};

use super::agents::{entry, spawned};
use super::steering::Agent;
use super::{Screen, facts, options};
use crate::events::{Changed, End, cost, settings_changed, turn_ended};
use crate::{Drive, shows};

const MARGIN: Duration = Duration::from_secs(5);

async fn quitting<D, F>(drive: D) -> (Exit, Instant)
where
    D: FnOnce(Agent, watch::Sender<Duration>) -> F,
    F: Future<Output = ()>,
{
    quitting_in(GlyphSet::Unicode, drive).await
}

async fn quitting_in<D, F>(glyphs: GlyphSet, drive: D) -> (Exit, Instant)
where
    D: FnOnce(Agent, watch::Sender<Duration>) -> F,
    F: Future<Output = ()>,
{
    let (finished, ended) = finishing(glyphs, drive).await;
    (finished.exit.unwrap(), ended)
}

async fn finishing<D, F>(glyphs: GlyphSet, drive: D) -> (Finished, Instant)
where
    D: FnOnce(Agent, watch::Sender<Duration>) -> F,
    F: Future<Output = ()>,
{
    let (commands, asked) = super::without_terminal(64);
    let (events_in, events) = mpsc::channel(256);
    let (inputs, received) = mpsc::unbounded_channel();
    let (worst_in, worst) = watch::channel(Duration::ZERO);
    let mut options = options(Start::Stalled);
    options.glyphs = glyphs;
    options.quit = Some(QuitBound {
        worst,
        margin: MARGIN,
    });
    let screen = Screen::new(30, 80);
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts: facts(),
        events,
        commands,
        resumed: None,
    };
    let mut console = screen.clone();
    let app = async {
        let finished = run_to_end(&mut console, received, session, options).await;
        (finished, Instant::now())
    };
    let agent = Agent {
        screen: screen.clone(),
        inputs,
        events: events_in,
        commands: asked,
    };
    let driver = async move {
        sleep(Duration::from_millis(300)).await;
        drive(agent, worst_in).await;
    };
    let ((finished, ended), ()) = tokio::join!(app, driver);
    (finished, ended)
}

async fn effort_asked(agent: &mut Agent) -> RequestId {
    agent.say("/effort low");
    match agent.next().await {
        ClientCommand::ChangeSettings { request, .. } => request,
        other => panic!("not a settings change: {other:?}"),
    }
}

fn answer(request: RequestId) -> AgentEvent {
    settings_changed(Changed {
        answers: Some(request),
        endpoint: "claude-api".into(),
        model: facts().model,
        ..Changed::default()
    })
}

#[tokio::test(start_paused = true)]
async fn a_quit_with_a_shortcut_waiting_says_applying_settings_until_it_is_answered() {
    let (exit, _) = quitting(|mut agent, _worst| async move {
        let request = effort_asked(&mut agent).await;
        agent.say("/quit");
        let lines = agent.settle().await;
        assert!(shows(&lines, copy::APPLYING_SETTINGS), "{lines:#?}");
        assert!(!shows(&lines, copy::SAVING_SETTINGS), "{lines:#?}");
        sleep(Duration::from_secs(3)).await;
        assert!(shows(&agent.screen.visible(), copy::APPLYING_SETTINGS));
        agent.emit(answer(request)).await;
        let lines = agent.settle().await;
        assert!(!shows(&lines, copy::APPLYING_SETTINGS), "{lines:#?}");
    })
    .await;
    assert_eq!(exit, Exit::Quit);
}

#[tokio::test(start_paused = true)]
async fn the_quit_status_ends_in_ascii_dots_with_ascii_glyphs() {
    let (exit, _) = quitting_in(GlyphSet::Ascii, |mut agent, _worst| async move {
        let request = effort_asked(&mut agent).await;
        agent.say("/quit");
        let lines = agent.settle().await;
        let status = copy::ongoing(copy::APPLYING_SETTINGS, &ASCII);
        assert_eq!(status, "applying settings...");
        assert!(shows(&lines, &status), "{lines:#?}");
        assert!(lines.iter().all(|l| l.is_ascii()), "{lines:#?}");
        agent.emit(answer(request)).await;
        let _ = agent.settle().await;
    })
    .await;
    assert_eq!(exit, Exit::Quit);
}

#[tokio::test(start_paused = true)]
async fn a_quit_with_nothing_waiting_shows_no_settings_status() {
    let (exit, _) = quitting(|agent, _worst| async move {
        agent.say("/quit");
        let lines = agent.settle().await;
        assert!(!shows(&lines, copy::APPLYING_SETTINGS), "{lines:#?}");
    })
    .await;
    assert_eq!(exit, Exit::Quit);
}

#[tokio::test(start_paused = true)]
async fn the_watched_limit_ends_the_quit_wait_as_forced_and_restarts_when_it_changes() {
    let quit_at = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = quit_at.clone();
    let (exit, ended) = quitting(|mut agent, worst| async move {
        effort_asked(&mut agent).await;
        agent.say("/quit");
        *seen.lock().unwrap() = Some(Instant::now());
        sleep(Duration::from_secs(3)).await;
        worst.send(Duration::from_secs(10)).unwrap();
        sleep(Duration::from_secs(60)).await;
        drop(agent);
    })
    .await;
    assert_eq!(
        exit,
        Exit::Forced {
            save_waiting: false
        }
    );
    let quit_at = quit_at.lock().unwrap().unwrap();
    let waited = ended - quit_at;
    assert!(waited >= Duration::from_secs(18), "{waited:?}");
    assert!(waited < Duration::from_secs(19), "{waited:?}");
}

#[tokio::test(start_paused = true)]
async fn a_second_quit_exits_at_once_as_forced_even_with_nothing_waiting() {
    let (exit, _) = quitting(|mut agent, _worst| async move {
        effort_asked(&mut agent).await;
        agent.say("/quit");
        sleep(Duration::from_millis(100)).await;
        agent.ctrl('c');
        sleep(Duration::from_secs(60)).await;
    })
    .await;
    assert_eq!(
        exit,
        Exit::Forced {
            save_waiting: false
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_quit_with_processes_running_says_it_is_stopping_them() {
    let (exit, _) = quitting(|agent, _worst| async move {
        for id in [3, 4] {
            agent
                .emit(AgentEvent::ProcessStarted {
                    id,
                    call_id: format!("toolu_{id}"),
                    target: "npm run dev".to_owned(),
                    tty: false,
                    stdin: false,
                    size: None,
                    file: None,
                    reason: wisp_protocol::BackgroundReason::Started,
                    log: None,
                    keeps_running: false,
                    pid: None,
                })
                .await;
        }
        agent.say("/quit");
        let lines = agent.settle().await;
        let status = copy::ongoing(
            &copy::stopping_processes(2),
            &wisp_render::theme::glyphs::UNICODE,
        );
        assert!(shows(&lines, &status), "{lines:#?}");
    })
    .await;
    assert_eq!(exit, Exit::Quit);
}

fn priced_end(usd: f64) -> AgentEvent {
    turn_ended(End {
        model: "test-model".to_owned(),
        cost: Some(cost(usd, usd, false)),
        ..End::default()
    })
}

#[tokio::test(start_paused = true)]
async fn the_goodbye_counts_every_agents_spend_not_only_mains() {
    let (finished, _) = finishing(GlyphSet::Unicode, |agent, _worst| async move {
        agent
            .emit(spawned(entry("/review", AgentState::Ended, 0.96)))
            .await;
        agent
            .emit(spawned(entry(
                "/coder",
                AgentState::Idle { outcome: None },
                0.46,
            )))
            .await;
        agent.emit(AgentEvent::turn_started(1)).await;
        agent.emit_spent(priced_end(0.42)).await;
        agent.settle().await;
        agent.say("/quit");
        agent.settle().await;
    })
    .await;
    let rows = crate::testing::texts(&finished.goodbye);
    assert!(rows[1].ends_with(", total $1.84."), "{rows:#?}");
}
