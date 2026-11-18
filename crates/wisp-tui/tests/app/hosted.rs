use tokio::sync::mpsc;
use wisp_client::link::{self as client, Link};
use wisp_protocol::{
    AgentEvent, AgentState, ClientCommand, DeliveryMode, Effort, InputId, Now, Opening,
    SessionView, TodoItem, TodoProgress, TodoStatus,
};
use wisp_tui::app::highlight::Start;
use wisp_tui::app::resumed::Resumed;
use wisp_tui::app::settings::Tracker;

use super::agents::{entry, spawned};
use super::processes::{started, terminal};
use super::resumed::{completed, reply, turn};
use super::steering::agent_session;
use super::{Screen, options, without_terminal};
use crate::Drive;
use crate::scratch::ScratchDir;

fn child(event: AgentEvent) -> AgentEvent {
    AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::new("/worker"),
        event: Box::new(event),
    }
}

fn waiting(text: &str, held: bool) -> AgentEvent {
    AgentEvent::InputAccepted {
        opening: false,
        id: InputId(text.into()),
        text: text.into(),
        mode: DeliveryMode::Queue,
        images: Vec::new(),
        replaces: None,
        held,
        front: false,
        from: None,
    }
}

async fn take(link: &mut Link) -> AgentEvent {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        link.channels.events.recv(),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn picture(
    opening: Opening,
    view: Option<SessionView>,
    now: Option<Now>,
    events: Vec<AgentEvent>,
    columns: u16,
) -> Vec<String> {
    let screen = Screen::new(30, columns);
    let mut facts = wisp_tui::app::SessionFacts::from(opening);
    if let Some(now) = now {
        Tracker::at(&mut facts, now);
    }
    let display = screen.clone();
    let mut options = options(Start::Stalled);
    options.motion = wisp_render::motion::MotionPolicy::Reduced;
    agent_session(
        screen,
        options,
        facts,
        view.map(|view| Resumed {
            view,
            continuing: None,
        }),
        without_terminal(64),
        |agent| async move {
            for event in events {
                agent.emit(event).await;
            }
            agent.settle().await;
        },
    )
    .await;
    display.visible()
}

#[tokio::test]
async fn reopening_mid_turn_agrees_with_the_window_there_from_the_start() {
    let scratch = ScratchDir::new("wisp-tui-", "host-gate");
    let mut socket = wispd::bind(&scratch.root).await.unwrap();
    socket.publish("abcd-1234").unwrap();
    let path = socket.path().to_owned();
    let mut opening: Opening = serde_json::from_str(include_str!(
        "../../../wisp-protocol/tests/fixtures/opening.json"
    ))
    .unwrap();
    opening.start = None;
    opening.start_pick = None;
    opening.resumed = None;
    opening.next = None;
    let host = wispd::Host::new(wispd::Session {
        opening: opening.clone(),
        view: SessionView::default(),
        session: "abcd-1234".into(),
        build: 0,
    });
    let (commands, mut received) = mpsc::channel(64);
    let (events, output) = mpsc::channel(64);
    let server = tokio::spawn(async move {
        let shutdown = host.run(socket, commands, output).await.unwrap();
        shutdown.finish(async { true }).await
    });
    let mut first = client::open(client::connect(&path, 0).await.unwrap())
        .await
        .unwrap();
    let mut history = turn(1, "an earlier turn");
    let mut accepted = history.remove(2);
    if let AgentEvent::InputAccepted { opening, .. } = &mut accepted {
        *opening = true;
    }
    history.insert(0, accepted);
    history.extend(reply("placeholder earlier reply"));
    history.push(completed());
    let mut from_start = history.clone();
    from_start.insert(1, started(1));
    let mut worker = entry("/worker", AgentState::Running, 0.0);
    let spawned = spawned(worker.clone());
    worker.checklist = Some(TodoProgress {
        completed: 1,
        total: 2,
    });
    let mut live = turn(2, "a turn in flight");
    let mut accepted = live.remove(2);
    if let AgentEvent::InputAccepted { opening, .. } = &mut accepted {
        *opening = true;
    }
    live.insert(0, accepted);
    live.extend(reply("placeholder in-flight reply"));
    live.extend([
        waiting("queued input", false),
        waiting("held input", true),
        terminal(2),
        AgentEvent::ProcessOutput {
            id: 2,
            text: "placeholder terminal output\n".into(),
            skipped: 0,
        },
        spawned,
        child(AgentEvent::InputAccepted {
            opening: true,
            id: InputId("child-direct".into()),
            text: "a direct child input".into(),
            mode: DeliveryMode::Steer,
            images: Vec::new(),
            replaces: None,
            held: false,
            front: false,
            from: None,
        }),
        child(AgentEvent::turn_started(1)),
        child(AgentEvent::StepStarted { step: 1 }),
        child(AgentEvent::InputDelivered {
            ids: vec![InputId("child-direct".into())],
            step: 1,
        }),
        child(AgentEvent::TodosUpdated {
            call_id: None,
            items: vec![
                TodoItem {
                    text: "first check".into(),
                    status: TodoStatus::Completed,
                    section: None,
                },
                TodoItem {
                    text: "second check".into(),
                    status: TodoStatus::InProgress,
                    section: None,
                },
            ],
        }),
    ]);
    from_start.extend(live);
    for event in &from_start {
        events.send(event.clone()).await.unwrap();
        assert_eq!(take(&mut first).await, *event);
    }
    drop(first);
    assert_eq!(received.recv().await.unwrap(), ClientCommand::WindowGone);
    let connect_path = path.clone();
    let opening_link = tokio::spawn(async move {
        client::open(client::connect(&connect_path, 0).await.unwrap())
            .await
            .unwrap()
    });
    let ClientCommand::DescribeSession { request } = received.recv().await.unwrap() else {
        panic!("missing description");
    };
    let now = Now {
        endpoint: opening
            .endpoint_name
            .clone()
            .unwrap_or_else(|| "test".into()),
        model: opening.model.clone(),
        effort: Effort::High,
        cache_ttl: opening.cache_ttl,
        next: None,
        endpoints: opening.endpoints.clone(),
    };
    events
        .send(AgentEvent::SessionSnapshot {
            answers: request,
            view: SessionView {
                events: history,
                input_ids: vec!["queued input".into(), "held input".into()],
                ..Default::default()
            },
            now,
            state: vec![
                AgentEvent::AgentsSnapshot {
                    request: None,
                    agents: vec![worker],
                },
                started(1),
                terminal(2),
            ],
        })
        .await
        .unwrap();
    let mut second = opening_link.await.unwrap();
    let marker = AgentEvent::StepStarted { step: 77 };
    events.send(marker.clone()).await.unwrap();
    from_start.push(marker.clone());
    let mut replay = Vec::new();
    loop {
        let event = take(&mut second).await;
        let done = event == marker;
        replay.push(event);
        if done {
            break;
        }
    }
    assert!(
        !replay
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentSpawned { .. }))
    );
    let returned_opening = second.opening.clone();
    let view = second.view.clone();
    let now = second.now.clone();
    second.end(false).await.unwrap();
    assert!(server.await.unwrap());
    tokio::time::pause();
    for columns in [40, 80, 120] {
        let baseline = picture(opening.clone(), None, None, from_start.clone(), columns).await;
        let reopened = picture(
            returned_opening.clone(),
            Some(view.clone()),
            now.clone(),
            replay.clone(),
            columns,
        )
        .await;
        assert_eq!(reopened, baseline, "{columns} columns");
    }
}

#[tokio::test(start_paused = true)]
async fn opening_acceptance_does_not_change_the_window_that_typed_it() {
    let screen = Screen::new(30, 80);
    let mut options = options(Start::Stalled);
    options.motion = wisp_render::motion::MotionPolicy::Reduced;
    agent_session(
        screen,
        options,
        super::facts(),
        None,
        without_terminal(8),
        |mut agent| async move {
            agent
                .paste("a direct opening")
                .key(wisp_tui::term::KeyCode::Enter);
            let ClientCommand::Send { id, text, mode, .. } = agent.next().await else {
                panic!("missing opening input");
            };
            let before = agent.settle().await;
            agent
                .emit(AgentEvent::InputAccepted {
                    opening: true,
                    id,
                    text,
                    mode,
                    images: Vec::new(),
                    replaces: None,
                    held: false,
                    front: false,
                    from: None,
                })
                .await;
            assert_eq!(agent.settle().await, before);
        },
    )
    .await;
}
