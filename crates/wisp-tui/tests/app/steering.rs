use std::future::Future;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::sleep;
use wisp_client::link::LinkEnd;
use wisp_protocol::context::ReportDetail;
use wisp_protocol::{
    AgentEvent, BlockKind, ClientCommand, DeliveryMode, Effort, ErrorKind, InputId, RejectKind,
    RequestId, WithdrawnBy,
};
use wisp_render::theme::glyphs::{ASCII, GlyphSet};
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::pager::stop_key;
use wisp_tui::app::resumed::Resumed;
use wisp_tui::app::{Finished, Options, Session, run_to_end};
use wisp_tui::term::{Input, KeyCode};

use super::{Screen, completed, facts, options};
use crate::Drive;
use crate::events::{End, Ending, turn_ended};
pub(super) use wisp_tui::app::replay::cancelled;

pub(super) struct Agent {
    pub(super) screen: Screen,
    pub(super) inputs: mpsc::UnboundedSender<Input>,
    pub(super) events: mpsc::Sender<AgentEvent>,
    pub(super) commands: mpsc::Receiver<ClientCommand>,
}

impl Agent {
    pub(super) async fn cut(self, ended: watch::Sender<Option<LinkEnd>>) {
        ended.send(Some(LinkEnd::Cut)).unwrap();
        drop(self.events);
        sleep(Duration::from_millis(20)).await;
    }

    pub(super) async fn emit(&self, event: AgentEvent) {
        self.events.send(event).await.unwrap();
        sleep(Duration::from_millis(5)).await;
    }

    pub(super) async fn emit_spent(&self, event: AgentEvent) {
        for event in super::agents::with_spend(event) {
            self.emit(event).await;
        }
    }

    pub(super) async fn next(&mut self) -> ClientCommand {
        sleep(Duration::from_millis(20)).await;
        self.commands.try_recv().unwrap()
    }

    pub(super) async fn describe(&mut self) {
        let ClientCommand::DescribeSettings { request } = self.next().await else {
            panic!("expected DescribeSettings");
        };
        self.emit(super::described(request.0)).await;
    }

    pub(super) async fn nothing_sent(&mut self) {
        sleep(Duration::from_millis(20)).await;
        let command = self.commands.try_recv();
        assert!(command.is_err(), "unexpected command: {command:?}");
    }

    pub(super) async fn settle(&self) -> Vec<String> {
        sleep(Duration::from_millis(100)).await;
        self.screen.steady()
    }

    pub(super) async fn start(&mut self, text: &str, turn: u64) -> InputId {
        self.paste(text).key(KeyCode::Enter);
        let id = sent_id(&self.next().await);
        self.emit(AgentEvent::turn_started(turn)).await;
        self.emit(AgentEvent::StepStarted { step: 1 }).await;
        self.emit(AgentEvent::InputDelivered {
            ids: vec![id.clone()],
            step: 1,
        })
        .await;
        self.emit(AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        })
        .await;
        self.emit(AgentEvent::TextDelta {
            index: 0,
            text: "working on it\n".to_owned(),
        })
        .await;
        id
    }

    pub(super) async fn accept(&self, command: &ClientCommand) -> InputId {
        let ClientCommand::Send { id, text, mode, .. } = command else {
            panic!("not a send: {command:?}");
        };
        self.emit(AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: id.clone(),
            mode: *mode,
            text: text.clone(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
        })
        .await;
        id.clone()
    }
}

impl Drive for Agent {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input> {
        &self.inputs
    }
}

pub(super) fn sent_id(command: &ClientCommand) -> InputId {
    match command {
        ClientCommand::Send { id, .. } => id.clone(),
        other => panic!("not a send: {other:?}"),
    }
}

fn send_of(command: &ClientCommand) -> (String, DeliveryMode, Option<u64>) {
    match command {
        ClientCommand::Send {
            text,
            mode,
            turn,
            replaces: None,
            ..
        } => (text.clone(), *mode, *turn),
        other => panic!("not a new send: {other:?}"),
    }
}

pub(super) async fn steering<D, F>(drive: D)
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    steering_on(Screen::new(30, 80), options(Start::Stalled), drive).await;
}

pub(super) async fn steering_on<D, F>(screen: Screen, options: Options, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    steering_facts(screen, options, facts(), drive).await;
}

pub(super) async fn steering_facts<D, F>(
    screen: Screen,
    options: Options,
    facts: super::SessionFacts,
    drive: D,
) -> wisp_tui::app::Exit
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    steering_finished(screen, options, facts, drive)
        .await
        .exit
        .unwrap()
}

pub(super) async fn steering_through<D, F>(
    screen: Screen,
    options: Options,
    facts: super::SessionFacts,
    channel: (mpsc::Sender<ClientCommand>, mpsc::Receiver<ClientCommand>),
    drive: D,
) -> wisp_tui::app::Exit
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    agent_session(screen, options, facts, None, channel, drive)
        .await
        .exit
        .unwrap()
}

pub(super) fn attached(options: &mut Options, first: u64) -> watch::Sender<Option<LinkEnd>> {
    let (end, _) = mpsc::channel(2);
    let (done, ended) = watch::channel(None);
    options.attached = Some(wisp_tui::app::Attached {
        first_request: RequestId(first),
        now: None,
        end,
        ended,
        error_file: Default::default(),
    });
    done
}

pub(super) async fn steering_finished<D, F>(
    screen: Screen,
    options: Options,
    facts: super::SessionFacts,
    drive: D,
) -> Finished
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    let channel = super::without_terminal(64);
    agent_session(screen, options, facts, None, channel, drive).await
}

pub(super) async fn agent_session<D, F>(
    screen: Screen,
    options: Options,
    facts: super::SessionFacts,
    resumed: Option<Resumed>,
    (commands, asked): (mpsc::Sender<ClientCommand>, mpsc::Receiver<ClientCommand>),
    drive: D,
) -> Finished
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    let (events_in, events) = mpsc::channel(256);
    let (inputs, received) = mpsc::unbounded_channel();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts,
        events,
        commands,
        resumed,
    };
    let mut console = screen.clone();
    let app = run_to_end(&mut console, received, session, options);
    let agent = Agent {
        screen: screen.clone(),
        inputs,
        events: events_in,
        commands: asked,
    };
    let driver = async move {
        sleep(Duration::from_millis(300)).await;
        drive(agent).await;
    };
    let (finished, ()) = tokio::join!(app, driver);
    finished
}

pub(super) fn failed() -> AgentEvent {
    turn_ended(End {
        ending: Ending::Error(ErrorKind::Network),
        message: "connection reset".to_owned(),
        ..End::default()
    })
}

pub(super) fn row_with<'a>(lines: &'a [String], needle: &str) -> Option<&'a String> {
    lines.iter().find(|l| l.contains(needle))
}

pub(super) fn position(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle} is not on screen: {lines:#?}"))
}

#[tokio::test(start_paused = true)]
async fn enter_steers_and_tab_queues_while_wisp_works_and_idle_enter_sends_without_a_turn() {
    steering(|mut a| async move {
        a.paste("first").key(KeyCode::Enter);
        assert_eq!(
            send_of(&a.next().await),
            ("first".to_owned(), DeliveryMode::Steer, None)
        );
        a.emit(AgentEvent::turn_started(4)).await;
        a.paste("a steer").key(KeyCode::Enter);
        assert_eq!(
            send_of(&a.next().await),
            ("a steer".to_owned(), DeliveryMode::Steer, Some(4))
        );
        a.paste("for later").key(KeyCode::Tab);
        assert_eq!(
            send_of(&a.next().await),
            ("for later".to_owned(), DeliveryMode::Queue, Some(4))
        );
        a.paste("mid").key(KeyCode::Left).key(KeyCode::Tab);
        a.nothing_sent().await;
        a.key(KeyCode::End).key(KeyCode::Tab);
        assert_eq!(
            send_of(&a.next().await),
            ("mi  d".to_owned(), DeliveryMode::Queue, Some(4))
        );
        a.key(KeyCode::Enter);
        a.nothing_sent().await;
        a.ctrl('d');
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn waiting_rows_come_from_the_kernel_and_a_delivered_steer_becomes_a_cell_at_its_step() {
    steering(|mut a| async move {
        a.start("run the checks", 1).await;
        a.paste("also print the date").key(KeyCode::Enter);
        let steer = a.next().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, copy::NEXT_STEP).is_none(), "{lines:#?}");
        let steer = a.accept(&steer).await;
        let lines = a.settle().await;
        let row = row_with(&lines, copy::NEXT_STEP).unwrap();
        assert!(row.contains("also print the date"), "{lines:#?}");
        for cols in [20, 80] {
            let size = a.screen.resize(30, cols);
            a.inputs.send(Input::Resize(size)).unwrap();
            let lines = a.settle().await;
            assert!(row_with(&lines, copy::NEXT_STEP).is_some(), "{lines:#?}");
        }
        a.emit(AgentEvent::BlockStopped { index: 0 }).await;
        a.emit(AgentEvent::StepStarted { step: 2 }).await;
        a.emit(AgentEvent::InputDelivered {
            ids: vec![steer],
            step: 2,
        })
        .await;
        a.emit(AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        })
        .await;
        a.emit(AgentEvent::TextDelta {
            index: 0,
            text: "it is friday\n".to_owned(),
        })
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, copy::NEXT_STEP).is_none(), "{lines:#?}");
        let cell = position(&lines, "also print the date");
        assert!(lines[cell].contains(copy::SENT_WHILE_WORKING), "{lines:#?}");
        assert!(position(&lines, "working on it") < cell);
        assert!(cell < position(&lines, "it is friday"));
        a.emit(completed()).await;
        a.ctrl('d');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn queued_messages_keep_wisp_busy_until_their_turn_and_cells_follow_the_kernels_order() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.paste("then commit").key(KeyCode::Tab);
        let queued = a.next().await;
        let queued = a.accept(&queued).await;
        a.emit(completed()).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, copy::NEXT_TURN).is_some(), "{lines:#?}");
        a.paste("and push").key(KeyCode::Tab);
        assert_eq!(
            send_of(&a.next().await),
            ("and push".to_owned(), DeliveryMode::Queue, None)
        );
        a.paste("late").key(KeyCode::Enter);
        let late = a.next().await;
        assert_eq!(
            send_of(&late),
            ("late".to_owned(), DeliveryMode::Steer, None)
        );
        a.emit(AgentEvent::turn_started(2)).await;
        a.emit(AgentEvent::StepStarted { step: 1 }).await;
        a.emit(AgentEvent::InputDelivered {
            ids: vec![queued],
            step: 1,
        })
        .await;
        let late = a.accept(&late).await;
        a.emit(AgentEvent::StepStarted { step: 2 }).await;
        a.emit(AgentEvent::InputDelivered {
            ids: vec![late],
            step: 2,
        })
        .await;
        let lines = a.settle().await;
        let commit = position(&lines, "then commit");
        assert!(lines[commit].contains(copy::QUEUED_TAG), "{lines:#?}");
        let late = position(&lines, "late");
        assert!(commit < late, "{lines:#?}");
        assert!(lines[late].contains(copy::SENT_WHILE_WORKING), "{lines:#?}");
        a.emit(completed()).await;
        a.ctrl('d');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_returns_waiting_steers_ahead_of_the_draft_and_holds_queued_messages() {
    steering(|mut a| async move {
        let first = a.start("first", 1).await;
        a.emit(super::processes::started(3)).await;
        a.paste("unsent steer").key(KeyCode::Enter);
        let steer = a.next().await;
        let steer = a.accept(&steer).await;
        a.paste("for later").key(KeyCode::Tab);
        let queued = a.next().await;
        let queued = a.accept(&queued).await;
        a.paste("my draft");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![first, steer],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(AgentEvent::InputHeld {
            ids: vec![queued],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(cancelled()).await;
        let lines = a.settle().await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains("first") && l.contains(copy::NOT_KEPT_TAG)),
            "{lines:#?}"
        );
        let held = row_with(&lines, "for later").unwrap();
        assert!(held.contains(copy::HELD), "{lines:#?}");
        let note = position(&lines, "Stopped. 1 queued message is held; enter sends it.");
        assert!(lines[note..].iter().any(|l| l.contains("composer")));
        let steer = position(&lines, "\u{203A} unsent steer");
        let draft = position(&lines, "my draft");
        assert_eq!(draft, steer + 2, "{lines:#?}");
        assert!(super::unframed(&lines[steer + 1]).trim().is_empty());
        assert!(
            row_with(&lines, &copy::send_held().text()).is_some(),
            "{lines:#?}"
        );
        a.ctrl('c');
        sleep(Duration::from_secs(3)).await;
        for cols in [80, 100] {
            a.input(Input::Resize(a.screen.resize(30, cols)));
            let status = super::status_line(&a.settle().await);
            assert!(status.contains(&copy::send_held().text()), "{status}");
        }
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, ClientCommand::Release);
        a.ctrl('d');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_failure_returns_steers_and_says_the_held_messages_go_when_you_are_ready() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.paste("a steer").key(KeyCode::Enter);
        let steer = a.next().await;
        let steer = a.accept(&steer).await;
        a.paste("next one").key(KeyCode::Tab);
        let queued = a.next().await;
        let queued = a.accept(&queued).await;
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![steer],
            by: WithdrawnBy::TurnFailed,
        })
        .await;
        a.emit(AgentEvent::InputHeld {
            ids: vec![queued],
            by: WithdrawnBy::TurnFailed,
        })
        .await;
        a.emit(failed()).await;
        let lines = a.settle().await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains("The turn failed. 1 queued")),
            "{lines:#?}"
        );
        assert!(row_with(&lines, copy::HELD).is_some(), "{lines:#?}");
        assert!(a.screen.caret_line().contains("a steer"), "{lines:#?}");
        a.key(KeyCode::Enter);
        assert_eq!(
            send_of(&a.next().await),
            ("a steer".to_owned(), DeliveryMode::Steer, None)
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refusal_is_matched_by_its_id_and_the_text_comes_back() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.paste("/effort high").key(KeyCode::Enter);
        let ClientCommand::ChangeSettings { request, .. } = a.next().await else {
            panic!("expected a settings change");
        };
        a.paste("one too many").key(KeyCode::Enter);
        let id = sent_id(&a.next().await);
        a.emit(AgentEvent::Rejected {
            request: None,
            reason: "32 messages are already waiting. Send this after the turn.".to_owned(),
            id: Some(id),
            kind: Some(RejectKind::TooMany),
        })
        .await;
        let lines = a.settle().await;
        assert!(
            row_with(
                &lines,
                "This message wasn't sent: 32 messages are already waiting."
            )
            .is_some(),
            "{lines:#?}"
        );
        assert!(!lines.iter().any(|l| l.contains("Settings not changed")));
        assert!(a.screen.caret_line().contains("one too many"));
        a.emit(AgentEvent::refused(
            request,
            "this model doesn't take effort high",
        ))
        .await;
        a.emit(completed()).await;
        let lines = a.settle().await;
        assert!(
            lines.iter().any(|l| l.contains("Settings not changed")),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn help_runs_and_compact_is_refused_until_the_turn_ends() {
    let screen = Screen::new(38, 80);
    steering_on(screen, options(Start::Stalled), |mut a| async move {
        a.start("first", 1).await;
        a.paste("/help").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        let turn = row_with(&lines, "Add to this turn").unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(turn.trim_start().starts_with("enter"), "{lines:#?}");
        assert!(row_with(&lines, &stop_key().text()).is_some(), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Add to this turn").is_none(), "{lines:#?}");
        a.paste("/compact").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::COMPACT_BETWEEN_TURNS).is_some(),
            "{lines:#?}"
        );
        assert!(a.screen.caret_line().contains("/compact"));
        a.ctrl('c');
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(cancelled()).await;
        a.key(KeyCode::Enter);
        assert!(
            matches!(a.next().await, ClientCommand::Compact { focus: None, .. }),
            "an idle /compact is sent"
        );
        assert!(!a.screen.caret_line().contains("/compact"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn built_commands_send_what_they_sent_before() {
    steering(|mut a| async move {
        a.paste("/model claude-sonnet-5-5").key(KeyCode::Enter);
        assert_eq!(
            a.next().await,
            ClientCommand::shortcut(
                RequestId(1),
                None,
                Some("claude-sonnet-5-5".to_owned()),
                None
            )
        );
        a.paste("/EFFORT   high  ").key(KeyCode::Enter);
        assert_eq!(
            a.next().await,
            ClientCommand::shortcut(RequestId(2), None, None, Some(Effort::High))
        );
        a.paste("/context all").key(KeyCode::Enter);
        assert_eq!(
            a.next().await,
            ClientCommand::ContextReport {
                detail: ReportDetail::All,
                request: RequestId(3),
            }
        );
        a.paste("/context all count").key(KeyCode::Enter);
        a.nothing_sent().await;
        a.paste("/effort zz").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, &copy::unknown_effort("zz")).is_some(),
            "{lines:#?}"
        );
        a.paste("/endpoint").key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Account").is_some(), "{lines:#?}");
        assert!(!a.screen.caret_line().contains("/endpoint"), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn lines_that_only_look_like_commands_are_sent_as_messages() {
    steering(|mut a| async move {
        for text in [
            "  /help",
            "/--",
            "/2x",
            "/etc/hosts is wrong",
            "/model x\nand more",
            "see /model",
        ] {
            a.paste(text).key(KeyCode::Enter);
            assert_eq!(
                send_of(&a.next().await),
                (text.to_owned(), DeliveryMode::Steer, None),
                "{text:?}"
            );
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_unknown_command_stays_in_the_composer_out_of_history() {
    steering(|mut a| async move {
        a.paste("/effort high").key(KeyCode::Enter);
        assert!(matches!(
            a.next().await,
            ClientCommand::ChangeSettings { .. }
        ));
        for text in ["/hepl me", "/tmp is full", "/abcdefghijkl now"] {
            a.paste(text).key(KeyCode::Enter);
            a.nothing_sent().await;
            let lines = a.settle().await;
            let name = text.split_whitespace().next().unwrap_or_default();
            assert!(
                row_with(&lines, &copy::unknown_command(&name[1..])).is_some(),
                "{lines:#?}"
            );
            assert!(a.screen.caret_line().contains(text), "{lines:#?}");
            a.ctrl('u');
            a.key(KeyCode::Up);
            let lines = a.settle().await;
            assert!(a.screen.caret_line().contains("/effort high"), "{lines:#?}");
            a.key(KeyCode::Down);
            let lines = a.settle().await;
            assert!(!a.screen.caret_line().contains("/effort"), "{lines:#?}");
        }
        a.paste("/hepl").key(KeyCode::Home).key(KeyCode::Char(' '));
        a.key(KeyCode::Enter);
        assert_eq!(
            send_of(&a.next().await),
            (" /hepl".to_owned(), DeliveryMode::Steer, None)
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_chip_that_starts_with_a_command_is_sent_as_a_message() {
    steering(|mut a| async move {
        for name in ["/hepl", "/model"] {
            let text = format!("{name} {}", "x".repeat(900));
            a.paste(&text);
            let lines = a.settle().await;
            assert!(!a.screen.caret_line().contains(name), "{lines:#?}");
            a.key(KeyCode::Enter);
            let (sent, _, _) = send_of(&a.next().await);
            assert_eq!(sent, text);
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_tab_hint_shows_while_busy_only_where_tab_would_queue() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, &copy::tab_queues().text()).is_none());
        a.paste("draft");
        let lines = a.settle().await;
        assert!(
            row_with(&lines, &copy::tab_queues().text()).is_some(),
            "{lines:#?}"
        );
        a.key(KeyCode::Left);
        let lines = a.settle().await;
        assert!(
            row_with(&lines, &copy::tab_queues().text()).is_none(),
            "{lines:#?}"
        );
        a.emit(AgentEvent::BlockStopped { index: 0 }).await;
        a.emit(completed()).await;
        a.key(KeyCode::End);
        let lines = a.settle().await;
        assert!(
            row_with(&lines, &copy::tab_queues().text()).is_none(),
            "{lines:#?}"
        );
        a.key(KeyCode::Tab);
        a.nothing_sent().await;
        a.ctrl('c').ctrl('d');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_picker_takes_tab_first_and_hides_the_rows_behind_a_count() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.paste("for later").key(KeyCode::Tab);
        let queued = a.next().await;
        a.accept(&queued).await;
        a.paste("/model").key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "for later").is_none(), "{lines:#?}");
        assert!(
            row_with(&lines, &copy::waiting_count(1)).is_some(),
            "{lines:#?}"
        );
        a.key(KeyCode::Tab);
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(row_with(&lines, "for later").is_some(), "{lines:#?}");
        assert!(
            row_with(&lines, &copy::waiting_count(1)).is_none(),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_return_while_browsing_history_keeps_the_real_draft_and_drops_the_shown_entry() {
    steering(|mut a| async move {
        let first = a.start("an older question", 1).await;
        a.paste("unsent steer").key(KeyCode::Enter);
        let steer = a.next().await;
        let steer = a.accept(&steer).await;
        a.paste("my draft");
        a.key(KeyCode::Up).key(KeyCode::Up).key(KeyCode::Up);
        let lines = a.settle().await;
        assert!(
            a.screen.caret_line().contains("an older question"),
            "{lines:#?}"
        );
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![first, steer],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(cancelled()).await;
        let lines = a.settle().await;
        let steer = position(&lines, "\u{203A} unsent steer");
        assert!(
            super::unframed(&lines[steer + 1]).trim().is_empty(),
            "{lines:#?}"
        );
        assert!(lines[steer + 2].contains("my draft"), "{lines:#?}");
        assert!(a.screen.caret_line().contains("my draft"));
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("an older question"))
                .count(),
            1,
            "{lines:#?}"
        );
        a.paste(" more").key(KeyCode::Enter);
        assert_eq!(
            send_of(&a.next().await),
            (
                "unsent steer\n\nmy draft more".to_owned(),
                DeliveryMode::Steer,
                None
            )
        );
    })
    .await;
}

async fn awaiting(a: &Agent) -> bool {
    sleep(Duration::from_millis(1500)).await;
    let lines = a.screen.visible();
    super::status_line(&lines).contains('\u{2026}')
}

#[tokio::test(start_paused = true)]
async fn an_idle_wait_outlasts_refusals_while_a_message_waits_and_ends_when_none_does() {
    steering(|mut a| async move {
        a.paste("hello").key(KeyCode::Enter);
        let send = a.next().await;
        let (_, _, turn) = send_of(&send);
        assert_eq!(turn, None);
        let id = sent_id(&send);
        assert!(awaiting(&a).await);
        a.emit(AgentEvent::InputAccepted {
            opening: false,
            front: true,
            id: id.clone(),
            mode: DeliveryMode::Steer,
            text: "hello".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
        })
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, copy::NEXT_TURN).is_some(), "{lines:#?}");
        a.paste("/effort high").key(KeyCode::Enter);
        let ClientCommand::ChangeSettings { request, .. } = a.next().await else {
            panic!("expected a settings change");
        };
        a.emit(AgentEvent::refused(request, "not now")).await;
        assert!(awaiting(&a).await);
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![id],
            by: WithdrawnBy::User,
        })
        .await;
        assert!(!awaiting(&a).await);
        a.paste("again").key(KeyCode::Enter);
        let again = sent_id(&a.next().await);
        assert!(awaiting(&a).await);
        a.emit(AgentEvent::Rejected {
            request: None,
            reason: "no".to_owned(),
            id: Some(again),
            kind: Some(RejectKind::TooMany),
        })
        .await;
        assert!(!awaiting(&a).await);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_model_picker_checks_in_ascii_dots_with_ascii_glyphs() {
    let mut ascii = options(Start::Stalled);
    ascii.glyphs = GlyphSet::Ascii;
    steering_on(Screen::new(30, 120), ascii, |mut a| async move {
        a.paste("/model").key(KeyCode::Enter);
        let ClientCommand::DescribeSettings { request } = a.next().await else {
            panic!("expected DescribeSettings");
        };
        let lines = a.settle().await;
        let checking = copy::ongoing(copy::CHECKING_COMPACTION, &ASCII);
        assert_eq!(checking, "checking for a compaction...");
        assert!(row_with(&lines, &checking).is_some(), "{lines:#?}");
        assert!(lines.iter().all(|l| l.is_ascii()), "{lines:#?}");
        a.emit(super::described(request.0)).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, &checking).is_none(), "{lines:#?}");
        assert!(lines.iter().all(|l| l.is_ascii()), "{lines:#?}");
        a.key(KeyCode::Esc);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_model_picker_asks_for_quotes_and_checks_until_answered() {
    steering(|mut a| async move {
        a.paste("/model").key(KeyCode::Enter);
        let ClientCommand::DescribeSettings { request } = a.next().await else {
            panic!("expected DescribeSettings");
        };
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CHECKING_COMPACTION).is_some(),
            "{lines:#?}"
        );
        a.emit(super::described(request.0 + 7)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CHECKING_COMPACTION).is_some(),
            "{lines:#?}"
        );
        a.emit(super::described(request.0)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CHECKING_COMPACTION).is_none(),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "sonnet-5-5").is_some(), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.paste("/effort").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CHECKING_COMPACTION).is_none(),
            "{lines:#?}"
        );
        a.key(KeyCode::Esc);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn steers_coming_back_drop_a_selection_in_the_draft() {
    use wisp_render::theme::{Depth, Mode, Theme, Token};
    use wisp_tui::term::{Input, Mouse, MouseKind};
    steering(|mut a| async move {
        let first = a.start("first", 1).await;
        a.paste("unsent steer").key(KeyCode::Enter);
        let steer = a.next().await;
        let steer = a.accept(&steer).await;
        a.paste("my draft");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
        let lines = a.settle().await;
        let top = super::composer_top(&lines);
        let input = u16::try_from(top + 1).unwrap();
        for (kind, column) in [(MouseKind::Press, 4), (MouseKind::Drag, 9)] {
            a.inputs
                .send(Input::Mouse(Mouse {
                    kind,
                    column,
                    row: input,
                    modified: false,
                }))
                .unwrap();
        }
        let rgb = Theme::new(Mode::Dark, Depth::TrueColor).rgb(Token::AccentTint);
        let tint = vt100::Color::Rgb(rgb.r, rgb.g, rgb.b);
        let tinted = |a: &Agent| {
            let state = a.screen.0.borrow();
            let shown = state.parser.screen();
            (0..30u16)
                .flat_map(|y| (0..80u16).map(move |x| (y, x)))
                .filter(|&(y, x)| shown.cell(y, x).is_some_and(|c| c.bgcolor() == tint))
                .count()
        };
        a.settle().await;
        assert_eq!(tinted(&a), 6);
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![first, steer],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(cancelled()).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "unsent steer").is_some(), "{lines:#?}");
        assert_eq!(tinted(&a), 0, "{lines:#?}");
        a.ctrl('c').ctrl('c').ctrl('d');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn up_walks_a_wrapped_draft_before_history_or_the_waiting_rows() {
    steering(|mut a| async move {
        a.start("an older question", 1).await;
        a.paste("an unsent steer").key(KeyCode::Enter);
        let steer = a.next().await;
        a.accept(&steer).await;
        let draft: Vec<String> = (1..=30).map(|n| format!("w{n:02}")).collect();
        a.paste(&draft.join(" "));
        let lines = a.settle().await;
        assert!(a.screen.caret_line().contains("w30"), "{lines:#?}");
        assert!(!a.screen.caret_line().contains("w01"), "{lines:#?}");
        a.key(KeyCode::Up);
        let lines = a.settle().await;
        assert!(a.screen.caret_line().contains("w01"), "{lines:#?}");
        a.key(KeyCode::Down);
        let lines = a.settle().await;
        assert!(a.screen.caret_line().contains("w30"), "{lines:#?}");
        a.key(KeyCode::Up).key(KeyCode::Up);
        let lines = a.settle().await;
        assert!(
            !a.screen.caret_line().contains("w01"),
            "the first row's Up leaves the draft: {lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn up_on_a_wrapped_drafts_first_row_recalls_history() {
    steering(|mut a| async move {
        a.start("an older question", 1).await;
        a.emit(completed()).await;
        let draft: Vec<String> = (1..=30).map(|n| format!("w{n:02}")).collect();
        a.paste(&draft.join(" "));
        a.key(KeyCode::Up);
        let lines = a.settle().await;
        assert!(a.screen.caret_line().contains("w01"), "{lines:#?}");
        a.key(KeyCode::Up);
        let lines = a.settle().await;
        assert!(
            a.screen.caret_line().contains("an older question"),
            "{lines:#?}"
        );
        a.key(KeyCode::Down);
        let lines = a.settle().await;
        assert!(a.screen.caret_line().contains("w30"), "{lines:#?}");
    })
    .await;
}
