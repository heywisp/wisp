use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;
use wisp_protocol::endpoint::{CheckKind, CheckResult, NotSavedReason, StoreKind};
use wisp_protocol::{AgentEvent, ClientCommand, RequestId};
use wisp_render::theme::Theme;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_tui::app::endpoint_card::CardSetup;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::setup::{self, Opening, SetupChannels};
use wisp_tui::app::{Exit, Options, Session, run};
use wisp_tui::term::{Input, KeyCode};

use super::{Look, MODES, Screen, Shots, Stage, facts, snapshot, truecolor};
use crate::Drive;
use crate::endpoints::{KEY, card_setup, saved};

const CARD_ROWS: u16 = 30;

fn looks() -> Vec<Look> {
    let mut looks = vec![];
    for mode in MODES {
        for width in [40, 80, 120] {
            looks.push(Look {
                rows: CARD_ROWS,
                ..truecolor(mode, GlyphSet::Unicode, width)
            });
        }
    }
    for width in [40, 80, 120] {
        looks.push(Look {
            rows: CARD_ROWS,
            ..truecolor(MODES[0], GlyphSet::Ascii, width)
        });
    }
    looks
}

fn request_of(command: Option<ClientCommand>) -> RequestId {
    match command {
        Some(ClientCommand::SaveEndpoint { request, .. }) => request,
        other => panic!("not a save: {other:?}"),
    }
}

fn options_for(look: Look) -> Options {
    let mut options = Options::new(Theme::new(look.mode, look.depth), look.glyphs);
    options.code = Start::Stalled;
    options.motion = look.motion;
    options
}

async fn staged_card<D, F>(look: Look, drive: D) -> Vec<(String, String)>
where
    D: FnOnce(Stage, mpsc::Receiver<ClientCommand>) -> F,
    F: Future<Output = ()>,
{
    let screen = Screen::new(look.rows, look.width);
    let (commands, sent) = super::super::without_terminal(64);
    let (events_in, events) = mpsc::channel(1024);
    let (inputs, received) = mpsc::unbounded_channel();
    let shots: Shots = Rc::default();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts: facts(),
        events,
        commands,
        resumed: None,
    };
    let mut options = options_for(look);
    options.card_setup = Some(card_setup());
    let mut console = screen.clone();
    let app = run(&mut console, received, session, options);
    let stage = Stage {
        screen: screen.clone(),
        inputs,
        events: events_in,
        shots: Rc::clone(&shots),
        sent: std::cell::Cell::new(0),
        turns: std::cell::Cell::new(0),
    };
    let driver = async move {
        sleep(Duration::from_millis(400)).await;
        drive(stage, sent).await;
    };
    let (exit, ()) = tokio::join!(app, driver);
    exit.unwrap();
    shots.take()
}

async fn add(s: Stage, mut sent: mpsc::Receiver<ClientCommand>) {
    s.say("/endpoint add");
    s.wait(300).await.shot("opened");
    s.key(KeyCode::Down).key(KeyCode::Enter);
    s.paste("https://gw.example.com/anthropic");
    s.key(KeyCode::Enter);
    s.wait(100).await.shot("gateway");
    s.key(KeyCode::Down).key(KeyCode::Enter);
    s.paste(KEY);
    s.wait(100).await.shot("key");
    s.key(KeyCode::Enter).key(KeyCode::Enter);
    s.wait(100).await.shot("saving");
    let first = request_of(sent.recv().await);
    s.event(AgentEvent::EndpointNotSaved {
        answers: first,
        name: "gw".to_owned(),
        why: NotSavedReason::Check(CheckResult::of(CheckKind::KeyRefused)),
    })
    .await;
    s.wait(100).await.shot("refused");
    s.key(KeyCode::Char('2'));
    let second = request_of(sent.recv().await);
    s.event(saved(second, "gw", StoreKind::Keyring)).await;
    s.wait(100).await.shot("saved");
}

struct Setup {
    screen: Screen,
    shots: Rc<RefCell<Vec<(String, String)>>>,
    inputs: mpsc::UnboundedSender<Input>,
    events: Option<mpsc::Sender<AgentEvent>>,
    commands: mpsc::Receiver<ClientCommand>,
}

impl Setup {
    async fn shot(&self, name: &str) {
        sleep(Duration::from_millis(100)).await;
        let screen = self.screen.styled();
        self.shots.borrow_mut().push((name.to_owned(), screen));
    }

    async fn emit(&self, event: AgentEvent) {
        self.events.as_ref().unwrap().send(event).await.unwrap();
    }
}

impl Drive for Setup {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input> {
        &self.inputs
    }
}

async fn first_run(look: Look, lead: Option<&str>) -> Vec<(String, String)> {
    let screen = Screen::new(look.rows, look.width);
    let (commands, server_commands) = mpsc::channel(16);
    let (server_events, events) = mpsc::channel(64);
    let (started, start) = oneshot::channel();
    let (inputs, received) = mpsc::unbounded_channel();
    let opening = Opening::FirstRun {
        setup: SetupChannels { events, commands },
        card: CardSetup {
            lead: lead.map(str::to_owned),
            ..card_setup()
        },
        start,
    };
    let mut console = screen.clone();
    let app = setup::open(&mut console, received, opening, options_for(look));
    let shots: Shots = Rc::default();
    let open_inputs = inputs.clone();
    let mut s = Setup {
        screen: screen.clone(),
        shots: Rc::clone(&shots),
        inputs,
        events: Some(server_events),
        commands: server_commands,
    };
    let driver = async move {
        sleep(Duration::from_millis(400)).await;
        s.shot("opened").await;
        s.key(KeyCode::Char('4'));
        s.shot("key card").await;
        s.key(KeyCode::Enter).paste(KEY).key(KeyCode::Enter);
        s.shot("name").await;
        s.key(KeyCode::Enter);
        s.shot("saving").await;
        let first = request_of(s.commands.recv().await);
        s.emit(AgentEvent::EndpointNotSaved {
            answers: first,
            name: "anthropic".to_owned(),
            why: NotSavedReason::KeychainFailed("didn't answer".to_owned()),
        })
        .await;
        s.shot("keychain").await;
        s.key(KeyCode::Char('1'));
        let second = request_of(s.commands.recv().await);
        s.key(KeyCode::Esc);
        s.shot("quitting").await;
        s.emit(saved(second, "anthropic", StoreKind::File)).await;
        s.shot("saved").await;
        drop(s);
        drop(started);
    };
    let (opened, ()) = tokio::join!(app, driver);
    drop(open_inputs);
    assert_eq!(
        opened.exit.unwrap(),
        Exit::SetupSaved {
            name: "anthropic".to_owned(),
            forced: false,
        }
    );
    shots.take()
}

#[tokio::test(start_paused = true)]
async fn the_endpoint_cards_at_40_80_and_120_in_light_dark_and_ascii() {
    for look in looks() {
        let add = staged_card(look, add).await;
        assert!(!add.iter().any(|(_, shot)| shot.contains("CANARY")));
        snapshot("endpoint-add", look, &add);
        let setup = first_run(look, None).await;
        assert!(!setup.iter().any(|(_, shot)| shot.contains("CANARY")));
        snapshot("endpoint-setup", look, &setup);
        let lead = "claude-api's key is missing from ~/.config/wisp/.env.";
        let left = first_run(look, Some(lead)).await;
        snapshot("endpoint-setup-left", look, &left[..1]);
    }
}

fn short_looks() -> Vec<Look> {
    let mut looks = vec![];
    for (width, rows) in [(40, 10), (80, 8), (120, 8)] {
        for mode in MODES {
            looks.push(Look {
                rows,
                ..truecolor(mode, GlyphSet::Unicode, width)
            });
        }
        looks.push(Look {
            rows,
            ..truecolor(MODES[0], GlyphSet::Ascii, width)
        });
    }
    looks
}

async fn to_gateway(s: Stage, _sent: mpsc::Receiver<ClientCommand>) {
    s.say("/endpoint add");
    s.wait(300).await;
    s.key(KeyCode::Down).key(KeyCode::Enter);
    s.paste("https://gw.example.com/anthropic");
    s.key(KeyCode::Enter);
    s.wait(100).await.shot("gateway");
    s.key(KeyCode::Down);
    s.wait(100).await.shot("down");
    s.key(KeyCode::Tab);
    s.wait(100).await.shot("tab");
}

#[tokio::test(start_paused = true)]
async fn a_short_pane_keeps_the_focused_field_in_view() {
    for width in [40, 80] {
        for rows in 6..=20 {
            let look = Look {
                rows,
                ..truecolor(MODES[0], GlyphSet::Unicode, width)
            };
            let shots = staged_card(look, to_gateway).await;
            let gateway = &shots[0].1;
            assert!(
                gateway.contains("Sends as   \u{25cf} x-api-key"),
                "{width}x{rows}:\n{gateway}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_short_first_run_keeps_the_focused_field_in_view() {
    for width in [40, 80] {
        for rows in 4..=16 {
            let look = Look {
                rows,
                ..truecolor(MODES[0], GlyphSet::Unicode, width)
            };
            let shots = first_run(look, None).await;
            let opened = &shots[1].1;
            assert!(
                opened.contains("Connect    \u{25cf} Anthropic's API"),
                "{width}x{rows}:\n{opened}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_endpoint_card_in_a_short_pane_at_40_80_and_120() {
    for look in short_looks() {
        let shots = staged_card(look, to_gateway).await;
        snapshot("endpoint-short", look, &shots);
    }
}

async fn to_plain_http(s: Stage, _sent: mpsc::Receiver<ClientCommand>) {
    s.say("/endpoint add");
    s.wait(300).await;
    s.key(KeyCode::Down).key(KeyCode::Enter);
    s.paste("http://gw.example.com/anthropic");
    s.key(KeyCode::Enter);
    s.wait(100).await.shot("plain http");
}

#[tokio::test(start_paused = true)]
async fn a_short_pane_keeps_the_selected_menu_option_in_view() {
    for width in [40, 80] {
        for rows in 6..=20 {
            let look = Look {
                rows,
                ..truecolor(MODES[0], GlyphSet::Unicode, width)
            };
            let shots = staged_card(look, to_plain_http).await;
            let menu = &shots[0].1;
            assert!(menu.contains("\u{203a} 2 No"), "{width}x{rows}:\n{menu}");
            assert!(menu.contains("enter choose"), "{width}x{rows}:\n{menu}");
        }
    }
}
