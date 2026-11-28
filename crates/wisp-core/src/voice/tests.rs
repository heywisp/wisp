use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::time::Instant;

use futures::FutureExt;
use serde_json::{Value, json};
use wisp_protocol::settings::names;
use wisp_protocol::voice::{AudioFormat, CallEnd, CallOutcome, VoiceCommand, VoiceEvent};
use wisp_protocol::{
    AgentEvent, BlockKind, ClientCommand, ErrorKind, InputId, OutputKind, RequestId, StopReason,
    ToolAction, ToolDisplay, ToolOutcome, ToolPurpose, TurnOutcome, TurnStats, Usage, WithdrawnBy,
};
use wisp_tool_text::voice as words;

use super::service::{HOLD, RENEW_EARLY, RENEW_LAST, REPLY_WAIT, STEADY, sentence_end};
use super::*;
use crate::agent::Connection;
use crate::agent::testing::{Fake, Knobs};
use crate::agents::PresetPlace;
use crate::kit::Connected;
use crate::provider::voice::OpenCall;
use crate::test_guard::ScratchDir;
use crate::voice::testing::{Route, spent};

const CALL: &str = "call-1";

type Wire = (mpsc::UnboundedReceiver<String>, mpsc::Sender<String>);

struct Legs {
    wires: Mutex<Vec<Wire>>,
    offering: Semaphore,
    gate: Semaphore,
    closed: AtomicUsize,
}

struct FakeLeg(Arc<Legs>);

impl Leg for FakeLeg {
    fn offer(&mut self) -> BoxFuture<'_, Result<String, ProviderError>> {
        async move {
            drop(self.0.offering.acquire().await);
            Ok("offer".to_owned())
        }
        .boxed()
    }

    fn answer(&mut self, _: String, _: LegAudio) -> BoxFuture<'_, Result<Control, ProviderError>> {
        async move {
            drop(self.0.gate.acquire().await);
            let (out, said) = mpsc::unbounded_channel();
            let (heard, frames) = mpsc::channel(64);
            self.0.wires.lock().unwrap().push((said, heard));
            Ok(Control { out, frames })
        }
        .boxed()
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        self.0.closed.fetch_add(1, Ordering::SeqCst);
        futures::future::ready(()).boxed()
    }
}

#[derive(Default)]
struct Saved(Mutex<BTreeMap<&'static str, String>>, Mutex<Vec<Value>>);

impl Saved {
    fn set(&self, name: &'static str, value: &str) {
        self.0.lock().unwrap().insert(name, value.into());
    }

    fn calling(&self, accounts: &[(&str, bool)]) {
        *self.1.lock().unwrap() = accounts
            .iter()
            .map(|(name, expired)| {
                json!({"name": name, "provider": "openai", "priced": true, "default": false,
                "expired": expired, "voice_model": "gpt-live-1-codex"})
            })
            .collect();
    }

    fn snapshot(&self, answers: RequestId) -> AgentEvent {
        let settings: Vec<_> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(name, value)| {
                json!({"name": name, "value": {"kind": "text", "text": value},
                "layer": "config", "origin": "config.toml", "applies": true})
            })
            .collect();
        serde_json::from_value(json!({"type": "settings_snapshot", "answers": answers,
            "context": {"endpoint": "talk", "model": "m", "max_tokens": 1},
            "settings": settings, "endpoints": self.1.lock().unwrap().clone(), "file": {"type": "missing"}}))
        .unwrap()
    }
}

fn kernel(
    saved: Arc<Saved>,
    brain: &mpsc::Sender<AgentEvent>,
) -> (mpsc::Sender<ClientCommand>, mpsc::Receiver<ClientCommand>) {
    let (agent_tx, mut asked) = mpsc::channel(64);
    let (passed, agent) = mpsc::channel(64);
    let answers = brain.downgrade();
    tokio::spawn(async move {
        while let Some(command) = asked.recv().await {
            if let ClientCommand::DescribeSettings { request } = command {
                if let Some(answers) = answers.upgrade() {
                    let _ = answers.send(saved.snapshot(request)).await;
                }
            } else if passed.send(command).await.is_err() {
                break;
            }
        }
    });
    (agent_tx, agent)
}

struct Harness {
    client: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    brain: mpsc::Sender<AgentEvent>,
    agent: mpsc::Receiver<ClientCommand>,
    records: mpsc::UnboundedReceiver<CallRecord>,
    legs: Arc<Legs>,
    route: Arc<Route>,
    saved: Arc<Saved>,
}

fn harness(route: Route, place: PresetPlace) -> Harness {
    resumed(route, place, Earlier::default())
}

fn resumed(route: Route, place: PresetPlace, earlier: Earlier) -> Harness {
    let route = Arc::new(route);
    let knobs = Knobs {
        voice: Some(route.clone()),
        ..Knobs::default()
    };
    let provider = Fake::with(knobs, []);
    let ready = vec![Connection {
        name: "talk".into(),
        provider: provider.clone(),
        context: crate::test_guard::log_context(),
        priced: !route.unpriced,
    }];
    let accounts = Connected {
        provider,
        ready,
        unavailable: Vec::new(),
        active: "talk".into(),
        env_account: None,
    };
    let saved = Arc::new(Saved::default());
    saved.set(names::VOICE_ENDPOINT, "talk");
    saved.calling(&[("talk", false)]);
    let legs = Arc::new(Legs {
        wires: Mutex::default(),
        offering: Semaphore::new(Semaphore::MAX_PERMITS),
        gate: Semaphore::new(Semaphore::MAX_PERMITS),
        closed: AtomicUsize::new(0),
    });
    let made = legs.clone();
    let (records_tx, records) = mpsc::unbounded_channel();
    let voice = Voice {
        accounts: Arc::new(accounts),
        place,
        legs: Some(Arc::new(move || Box::new(FakeLeg(made.clone())))),
        records: records_tx,
        earlier,
    };
    let (brain, brain_rx) = mpsc::channel(64);
    let (agent_tx, agent) = kernel(saved.clone(), &brain);
    let (client, events, _) = attach(agent_tx, brain_rx, voice);
    Harness {
        client,
        events,
        brain,
        agent,
        records,
        legs,
        route,
        saved,
    }
}

fn call() -> CallId {
    CallId(CALL.into())
}

impl Harness {
    async fn voice(&self, command: VoiceCommand) {
        self.client
            .send(ClientCommand::Voice { command })
            .await
            .unwrap();
    }

    async fn start(&mut self) -> Wire {
        let format = AudioFormat::Pcm16Mono48k;
        self.voice(VoiceCommand::Start {
            call: call(),
            format,
        })
        .await;
        assert_eq!(
            self.voice_event().await,
            VoiceEvent::Started { call: call() }
        );
        self.legs.wires.lock().unwrap().pop().unwrap()
    }

    async fn tap(&self, said: &mut mpsc::UnboundedReceiver<String>, text: &str) {
        let note = VoiceCommand::Note {
            call: call(),
            text: text.into(),
        };
        self.voice(note).await;
        let cut = wisp_tool_text::output::cut_chars(text, words::TAP_CHARS);
        assert_eq!(said.recv().await.unwrap(), format!("note - Tapped: {cut}"));
    }

    async fn voice_event(&mut self) -> VoiceEvent {
        loop {
            let event = self.events.recv().await.unwrap();
            assert!(
                !matches!(event, AgentEvent::SettingsSnapshot(_)),
                "{event:?}"
            );
            if let AgentEvent::Voice { event } = event {
                return event;
            }
        }
    }

    async fn think(&self, events: Vec<AgentEvent>) {
        for event in events {
            self.brain.send(event).await.unwrap();
        }
    }

    async fn queued(&mut self) -> (InputId, String) {
        let next = tokio::time::timeout(REPLY_WAIT * 10, self.agent.recv()).await;
        match next.unwrap().unwrap() {
            ClientCommand::Send { id, text, .. } => (id, text),
            other => panic!("{other:?}"),
        }
    }
}

async fn heard(wire: &mpsc::Sender<String>, frames: &[&str]) {
    for frame in frames {
        wire.send((*frame).to_owned()).await.unwrap();
    }
}

async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

fn answering(id: &InputId) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        AgentEvent::InputDelivered {
            ids: vec![id.clone()],
            step: 1,
        },
    ]
}

fn reply(index: usize, text: &str) -> Vec<AgentEvent> {
    [
        streamed(index, &[text]),
        vec![AgentEvent::BlockStopped { index }],
    ]
    .concat()
}

fn streamed(index: usize, deltas: &[&str]) -> Vec<AgentEvent> {
    let kind = BlockKind::Text;
    let text = |text: &&str| AgentEvent::TextDelta {
        index,
        text: (*text).into(),
    };
    [AgentEvent::BlockStarted { index, kind }]
        .into_iter()
        .chain(deltas.iter().map(text))
        .collect()
}

fn requested(call_id: &str) -> AgentEvent {
    AgentEvent::ToolCallRequested {
        step: 1,
        block: 1,
        call_id: call_id.into(),
        tool: "show_page".into(),
        input: json!({}),
        display: ToolDisplay {
            action: ToolAction::Run,
            verbs: None,
            target: String::new(),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        },
        refused: false,
    }
}

fn finished(call_id: &str, is_error: bool) -> AgentEvent {
    let outcome = ToolOutcome::Completed {
        is_error,
        exit: None,
    };
    AgentEvent::call_finished(call_id, outcome)
}

fn ended(outcome: TurnOutcome) -> AgentEvent {
    AgentEvent::TurnEnded {
        outcome,
        stats: TurnStats::default(),
    }
}

fn completed() -> AgentEvent {
    completed_by(StopReason::EndTurn)
}

fn completed_by(stop_reason: StopReason) -> AgentEvent {
    ended(TurnOutcome::Completed {
        stop_reason,
        model: "brain".into(),
        usage: Usage::default(),
        cost: None,
        committed: true,
        steps: 1,
        tool_calls: 0,
    })
}

fn rejected(id: InputId) -> AgentEvent {
    AgentEvent::Rejected {
        reason: "refused".into(),
        id: Some(id),
        kind: None,
        request: None,
    }
}

fn stopped() -> AgentEvent {
    ended(TurnOutcome::Cancelled {
        usage: None,
        cost: None,
        committed_steps: 0,
    })
}

async fn hang_up(mut h: Harness) -> (VoiceEvent, CallRecord) {
    let ended = h.voice_event().await;
    drop(h.brain);
    drop(h.client);
    while let Some(event) = h.events.recv().await {
        assert!(!matches!(event, AgentEvent::Voice { .. }), "{event:?}");
    }
    let record = h.records.recv().await.unwrap();
    assert!(h.records.try_recv().is_err());
    (ended, record)
}

#[tokio::test]
async fn a_delegation_queues_a_hand_off_whose_reply_is_said_as_its_text_block_closes() {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let (mut said, wire) = h.start().await;
    let question = [
        "delegate d1 seven times eight",
        "heard What is seven times eight?",
        "said Let me check.",
    ];
    heard(&wire, &question).await;
    let (first, text) = h.queued().await;
    assert_eq!(
        text,
        "Heard: What is seven times eight?\nAsked: seven times eight"
    );
    heard(
        &wire,
        &["heard And nine?", "delegate d2 nine", "delegate d3 ten"],
    )
    .await;
    settle().await;
    let mut turn = answering(&first);
    turn.extend(reply(0, "Fifty-six."));
    turn.push(requested("t1"));
    h.think(turn).await;
    assert_eq!(said.recv().await.unwrap(), "reply d1 Fifty-six.");
    h.think(vec![finished("t1", false), completed()]).await;
    assert_eq!(said.recv().await.unwrap(), "note d1 show_page finished");
    let (second, text) = h.queued().await;
    assert_eq!(
        text,
        "Said: Let me check.\nHeard: And nine?\nAsked: nine\nAsked: ten"
    );
    let mut turn = answering(&second);
    turn.extend(reply(0, "Ninety. And ten?"));
    h.think(turn).await;
    assert_eq!(
        said.recv().await.unwrap(),
        format!("note d2 {}", words::COVERED)
    );
    assert_eq!(said.recv().await.unwrap(), "reply d3 Ninety.");
    assert_eq!(said.recv().await.unwrap(), "reply d3 And ten?");
    let opened = h.route.opened.lock().unwrap().clone();
    let call = OpenCall {
        offer: "offer".into(),
        instructions: String::new(),
        voice: "cove".into(),
        language: None,
    };
    assert_eq!(opened, [call]);
}

#[tokio::test(start_paused = true)]
async fn a_learner_line_with_no_delegation_reaches_the_brain_with_the_voices_own_line() {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let (mut said, wire) = h.start().await;
    let rows: [(&[&str], Duration, &str, &str); 3] = [
        (
            &["heard Is a whale a fish?"],
            REPLY_WAIT,
            "Heard: Is a whale a fish?",
            "-",
        ),
        (
            &["heard Why not?", "said Good question."],
            HOLD,
            "Heard: Why not?\nSaid: Good question.",
            "-",
        ),
        (
            &[
                "heard And a shark?",
                "said Hmm.",
                "delegate d1 is a shark a fish",
            ],
            Duration::ZERO,
            "Heard: And a shark?\nSaid: Hmm.\nAsked: is a shark a fish",
            "d1",
        ),
    ];
    for (frames, wait, expected, delegation) in rows {
        let at = Instant::now();
        heard(&wire, frames).await;
        let (id, text) = h.queued().await;
        assert_eq!((text.as_str(), at.elapsed()), (expected, wait));
        let mut turn = answering(&id);
        turn.extend(reply(0, "Answer."));
        turn.push(completed());
        h.think(turn).await;
        assert_eq!(
            said.recv().await.unwrap(),
            format!("reply {delegation} Answer.")
        );
        settle().await;
    }
    let at = Instant::now();
    heard(&wire, &["heard I think B"]).await;
    settle().await;
    h.tap(&mut said, "B").await;
    let (_, text) = h.queued().await;
    let expected = ("Heard: I think B\nTapped: B", HOLD);
    assert_eq!((text.as_str(), at.elapsed()), expected);
    let reader = Route {
        reader: true,
        ..Route::default()
    };
    let mut h = harness(reader, PresetPlace::of_config(None, None, 0));
    let (mut said, wire) = h.start().await;
    let at = Instant::now();
    heard(&wire, &["heard Is a whale a fish?; send next"]).await;
    let (_, text) = h.queued().await;
    let expected = ("Heard: Is a whale a fish?", Duration::ZERO);
    assert_eq!((text.as_str(), at.elapsed()), expected);
    assert_eq!(said.recv().await.unwrap(), "next");
}

#[tokio::test(start_paused = true)]
async fn a_late_heard_line_of_a_delegated_question_starts_no_second_hand_off() {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let (mut said, wire) = h.start().await;
    heard(&wire, &["delegate d1 what is a mammal"]).await;
    let (first, text) = h.queued().await;
    assert_eq!(text, "Asked: what is a mammal");
    heard(&wire, &["heard What is a mammal?"]).await;
    settle().await;
    let mut turn = answering(&first);
    turn.extend(reply(0, "It feeds its young milk."));
    turn.push(completed());
    h.think(turn).await;
    assert_eq!(
        said.recv().await.unwrap(),
        "reply d1 It feeds its young milk."
    );
    tokio::time::sleep(REPLY_WAIT * 2).await;
    assert!(h.agent.try_recv().is_err());
    heard(&wire, &["delegate d2 is a bat a mammal"]).await;
    let (second, text) = h.queued().await;
    assert_eq!(text, "Heard: What is a mammal?\nAsked: is a bat a mammal");
    let mut turn = answering(&second);
    turn.extend(reply(0, "Yes. Can a bat fly?"));
    turn.push(requested("t1"));
    h.think(turn).await;
    assert_eq!(said.recv().await.unwrap(), "reply d2 Yes.");
    assert_eq!(said.recv().await.unwrap(), "reply d2 Can a bat fly?");
    heard(&wire, &["heard Yes it can"]).await;
    tokio::time::sleep(REPLY_WAIT * 2).await;
    assert!(h.agent.try_recv().is_err());
    h.think(vec![finished("t1", false), completed()]).await;
    assert_eq!(said.recv().await.unwrap(), "note d2 show_page finished");
    let (third, text) = h.queued().await;
    assert_eq!(text, "Heard: Yes it can");
    h.think(answering(&third)).await;
    heard(&wire, &["heard Is that right?"]).await;
    settle().await;
    let mut turn = reply(0, "Well done.");
    turn.push(completed());
    h.think(turn).await;
    assert_eq!(said.recv().await.unwrap(), "reply - Well done.");
    let (again, text) = h.queued().await;
    assert_eq!(text, "Heard: Is that right?");
    let mut turn = answering(&again);
    turn.extend(reply(0, "Yes."));
    turn.push(completed());
    h.think(turn).await;
    assert_eq!(said.recv().await.unwrap(), "reply - Yes.");
    heard(&wire, &["heard Do birds have fur?"]).await;
    tokio::time::sleep(HOLD).await;
    heard(&wire, &["delegate d3 do birds have fur"]).await;
    let (fourth, text) = h.queued().await;
    assert_eq!(text, "Heard: Do birds have fur?\nAsked: do birds have fur");
    let mut turn = answering(&fourth);
    turn.extend(reply(0, "No, feathers."));
    turn.push(requested("t2"));
    h.think(turn).await;
    assert_eq!(said.recv().await.unwrap(), "reply d3 No, feathers.");
    heard(&wire, &["delegate d4 do fish have fur"]).await;
    tokio::time::sleep(HOLD * 2).await;
    heard(&wire, &["heard And fish?"]).await;
    settle().await;
    let at = Instant::now();
    h.think(vec![finished("t2", false), completed()]).await;
    let (_, text) = h.queued().await;
    assert_eq!(text, "Heard: And fish?\nAsked: do fish have fur");
    assert_eq!(at.elapsed(), Duration::ZERO);
}

#[tokio::test]
async fn only_a_successful_tool_call_becomes_a_note_and_the_next_text_is_said() {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let (mut said, wire) = h.start().await;
    heard(&wire, &["delegate d1 draw a cat"]).await;
    let (id, _) = h.queued().await;
    let mut turn = answering(&id);
    turn.extend([requested("t0"), finished("t0", false)]);
    turn.extend(reply(0, "Here is a cat."));
    turn.extend(reply(1, "Not after a tool."));
    turn.extend([requested("t1"), finished("t1", true)]);
    turn.extend(reply(2, "After a failed call."));
    turn.extend([requested("t2"), finished("t2", false)]);
    turn.extend(reply(3, "It has stripes."));
    turn.extend(reply(4, "Not said either."));
    turn.extend([requested("t3"), finished("t3", false)]);
    turn.extend([requested("t4"), finished("t4", true)]);
    turn.extend(reply(5, "After a later failure."));
    turn.push(completed());
    h.think(turn).await;
    for note in [
        "note d1 show_page finished",
        "reply d1 Here is a cat.",
        "note d1 show_page finished",
        "reply d1 It has stripes.",
        "note d1 show_page finished",
    ] {
        assert_eq!(said.recv().await.unwrap(), note);
    }
    h.voice(VoiceCommand::Stop { call: call() }).await;
    let (_, record) = hang_up(h).await;
    assert_eq!(said.recv().await.as_deref(), Some("hang up"));
    assert_eq!(said.recv().await, None);
    assert!(record.lines.is_empty(), "{:?}", record.lines);
}

enum Step {
    Think(Vec<AgentEvent>),
    Hear(&'static str),
    Say(&'static str),
}

#[tokio::test(start_paused = true)]
async fn the_answer_is_said_sentence_by_sentence_until_the_learner_cuts_in() {
    use Step::{Hear, Say, Think};
    let more = |text: &str| AgentEvent::TextDelta {
        index: 0,
        text: text.into(),
    };
    let close = |text: &str| {
        Think(vec![
            more(text),
            AgentEvent::BlockStopped { index: 0 },
            completed(),
        ])
    };
    let retry = serde_json::from_value(
        json!({"type": "retrying", "attempt": 1, "delay_ms": 0, "reason": "cut", "discarded_blocks": 1}),
    )
    .unwrap();
    let thinking = AgentEvent::BlockStarted {
        index: 0,
        kind: BlockKind::Thinking,
    };
    let rows = [
        (
            vec![
                Think(streamed(
                    0,
                    &["Seven times eight is fifty", "-six. Well done! Now"],
                )),
                Say("Seven times eight is fifty-six."),
                Say("Well done!"),
                close(" try nine."),
                Say("Now try nine."),
            ],
            "",
        ),
        (
            vec![
                Think(streamed(0, &["One. Two. Three"])),
                Say("One."),
                Say("Two."),
                Hear("interrupt"),
                close(" more. Four."),
            ],
            "Cut off: Three\n",
        ),
        (
            vec![
                Think(streamed(0, &["One. Two. Three"])),
                Say("One."),
                Say("Two."),
                Hear("interrupt Two."),
                close(" Four."),
            ],
            "Cut off: Two.\n",
        ),
        (
            vec![
                Hear("interrupt"),
                Think(streamed(0, &["One"])),
                Hear("interrupt"),
                Hear("interrupt Old tail."),
                close(" two."),
                Say("One two."),
            ],
            "Cut off: Old tail.\n",
        ),
        (
            vec![
                Think([reply(0, "Hello. Bye."), vec![completed()]].concat()),
                Say("Hello."),
                Say("Bye."),
                Hear("interrupt"),
                Hear("interrupt Bye."),
            ],
            "Cut off: Bye.\n",
        ),
        (
            vec![
                Think(streamed(0, &["First. Sec"])),
                Say("First."),
                Think(vec![retry, thinking, AgentEvent::BlockStopped { index: 0 }]),
                Think([reply(1, "First again. Second."), vec![completed()]].concat()),
                Say("First again."),
                Say("Second."),
            ],
            "",
        ),
        (
            vec![
                Think(streamed(0, &["One. Two. "])),
                Say("One."),
                Say("Two."),
                Hear("interrupt"),
                close("Three."),
            ],
            "",
        ),
        (
            vec![
                Think(streamed(0, &["One. Tw"])),
                Say("One."),
                Think(vec![stopped()]),
                Say(words::NO_ANSWER),
            ],
            "",
        ),
    ];
    for (steps, cut) in rows {
        let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
        let (mut said, wire) = h.start().await;
        heard(&wire, &["delegate d1 seven times eight"]).await;
        let (id, _) = h.queued().await;
        h.think(answering(&id)).await;
        for step in steps {
            match step {
                Think(events) => h.think(events).await,
                Hear(frame) => heard(&wire, &[frame]).await,
                Say(text) => assert_eq!(said.recv().await.unwrap(), format!("reply d1 {text}")),
            }
            settle().await;
        }
        assert!(said.try_recv().is_err(), "{cut}");
        heard(&wire, &["heard Next", "delegate d2 next"]).await;
        let (_, text) = h.queued().await;
        assert_eq!(text, format!("{cut}Heard: Next\nAsked: next"));
    }
}

#[test]
fn a_sentence_ends_at_its_mark_and_white_space_or_at_a_line_break() {
    let rows = [
        ("It is 3.5 now", None),
        ("Use e.g. this", Some("Use e.g.")),
        ("He said \"Stop!\" and left", Some("He said \"Stop!\"")),
        ("Wait\u{2026} what", Some("Wait\u{2026}")),
        ("First line\nsecond", Some("First line")),
        ("The end.", None),
    ];
    for (text, sentence) in rows {
        assert_eq!(
            sentence_end(text).map(|end| &text[..end]),
            sentence,
            "{text}"
        );
    }
}

#[derive(Debug, Clone, Copy)]
enum End {
    Tap,
    TapUnpriced,
    Quiet,
    ScreenGone,
    RemoteClose,
    RouteFails,
    ErrorThenClose,
    ErrorThenTap,
    NoAccount,
    Unready,
    Reader,
}

#[tokio::test(start_paused = true)]
async fn every_end_gives_one_ended_with_the_lines_no_hand_off_carried() {
    let done = |end| CallOutcome::Completed { end };
    let failed = |kind: ErrorKind, message: &str| CallOutcome::Failed {
        kind,
        message: message.into(),
    };
    let rows = [
        (End::Tap, done(CallEnd::Hangup)),
        (End::TapUnpriced, done(CallEnd::Hangup)),
        (End::Quiet, done(CallEnd::Idle)),
        (End::ScreenGone, done(CallEnd::ClientGone)),
        (End::RemoteClose, done(CallEnd::Remote)),
        (End::RouteFails, failed(ErrorKind::Overloaded, "refused")),
        (End::ErrorThenClose, failed(ErrorKind::RateLimit, "limited")),
        (End::ErrorThenTap, done(CallEnd::Hangup)),
        (End::NoAccount, failed(ErrorKind::Other, words::NO_ACCOUNT)),
        (
            End::Unready,
            failed(ErrorKind::Other, words::ACCOUNT_UNREADY),
        ),
        (End::Reader, done(CallEnd::Hangup)),
    ];
    for (end, outcome) in rows {
        let refuse = matches!(end, End::RouteFails).then_some(ErrorKind::Overloaded);
        let unpriced = matches!(end, End::TapUnpriced);
        let route = Route {
            refuse,
            unpriced,
            reader: matches!(end, End::Reader),
            ..Route::default()
        };
        let mut h = harness(route, PresetPlace::of_config(None, None, 0));
        if matches!(end, End::NoAccount | End::Unready) {
            h.saved.0.lock().unwrap().clear();
            h.saved.calling(&[("talk", false), ("work", false)]);
        }
        if matches!(end, End::Unready) {
            h.saved.set(names::VOICE_ENDPOINT, "signed-out");
        }
        let up = !matches!(end, End::RouteFails | End::NoAccount | End::Unready);
        let mut wire = None;
        if up {
            let (mut said, frames) = h.start().await;
            h.tap(&mut said, "a button").await;
            wire = Some((said, frames));
        } else {
            let format = AudioFormat::Pcm16Mono48k;
            h.voice(VoiceCommand::Start {
                call: call(),
                format,
            })
            .await;
        }
        match end {
            End::Tap | End::TapUnpriced => h.voice(VoiceCommand::Stop { call: call() }).await,
            End::ScreenGone => drop(std::mem::replace(&mut h.client, mpsc::channel(1).0)),
            End::RemoteClose => drop(wire.take()),
            End::ErrorThenClose => {
                let (_said, frames) = wire.take().unwrap();
                heard(&frames, &["error"]).await;
            }
            End::ErrorThenTap | End::Reader => {
                let frame = if matches!(end, End::Reader) {
                    "spent; spent"
                } else {
                    "error"
                };
                heard(&wire.as_ref().unwrap().1, &[frame]).await;
                settle().await;
                h.voice(VoiceCommand::Stop { call: call() }).await;
            }
            End::Quiet => {
                h.think(vec![AgentEvent::turn_started(1)]).await;
                tokio::time::sleep(QUIET * 2).await;
                h.think(vec![completed()]).await;
                tokio::time::sleep(QUIET / 2).await;
                if let Some((_, frames)) = &wire {
                    heard(frames, &["said Still here."]).await;
                    tokio::time::sleep(QUIET / 2).await;
                    heard(frames, &["interrupt"]).await;
                }
            }
            End::RouteFails | End::NoAccount | End::Unready => {}
        }
        let (ended, record) = hang_up(h).await;
        if let Some((mut said, _frames)) = wire {
            assert_eq!(said.recv().await.as_deref(), Some("hang up"), "{end:?}");
            assert_eq!(said.recv().await, None, "{end:?}");
        }
        let seconds = if matches!(end, End::Quiet) {
            4 * QUIET.as_secs()
        } else {
            0
        };
        let estimate = match end {
            End::Reader => crate::pricing::voice_usd("gpt-realtime-2.1-mini", 0, &[spent(); 2]),
            _ => up.then(|| crate::pricing::voice_usd("gpt-live-1-codex", seconds, &[]).unwrap()),
        };
        let (usd, list_usd) = if unpriced {
            (None, estimate)
        } else {
            (estimate, None)
        };
        let billing = wisp_protocol::Billing::ChatGptPlan;
        let cost = wisp_protocol::AccountSpend::of(usd.unwrap_or(0.0), None, billing, unpriced);
        let expected = VoiceEvent::Ended {
            call: call(),
            outcome: outcome.clone(),
            seconds,
            cost,
        };
        assert_eq!(ended, expected, "{end:?}");
        let still = matches!(end, End::Quiet).then(|| "Said: Still here.".to_owned());
        let lines: Vec<String> = up
            .then(|| "Tapped: a button".to_owned())
            .into_iter()
            .chain(still)
            .collect();
        assert_eq!(record.lines, lines, "{end:?}");
        assert_eq!(record.outcome, outcome, "{end:?}");
        assert_eq!(
            (record.cost_usd, record.list_usd),
            (usd, list_usd),
            "{end:?}"
        );
        assert_eq!(record.unpriced(), unpriced, "{end:?}");
        assert_eq!(record.endpoint_name.is_some(), up, "{end:?}");
    }
}

#[tokio::test]
async fn unknown_frames_are_kept_once_per_type_with_a_count() {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let (_said, wire) = h.start().await;
    let frames = [
        "mystery a",
        "usage",
        "mystery b",
        "oddity",
        "delegate d1 hi",
    ];
    heard(&wire, &frames).await;
    let (id, _) = h.queued().await;
    h.voice(VoiceCommand::Stop { call: call() }).await;
    assert_eq!(
        h.agent.recv().await.unwrap(),
        ClientCommand::Withdraw { ids: vec![id] }
    );
    let (_, record) = hang_up(h).await;
    let unknown = BTreeMap::from([("mystery".to_owned(), 2), ("oddity".to_owned(), 1)]);
    assert_eq!(record.unknown, unknown);
    assert_eq!(record.lines, ["Asked: hi"]);
}

#[tokio::test]
async fn a_held_or_refused_hand_off_is_withdrawn_and_the_next_gets_a_fresh_answer() {
    for refused in [false, true] {
        hand_off_again(refused).await;
    }
    let reader = Route {
        reader: true,
        ..Route::default()
    };
    let mut h = harness(reader, PresetPlace::of_config(None, None, 0));
    let (mut said, wire) = h.start().await;
    for end in [None, Some(stopped()), Some(completed())] {
        heard(&wire, &["heard A question"]).await;
        let (id, _) = h.queued().await;
        h.think(match end {
            Some(end) => [answering(&id), vec![end]].concat(),
            None => vec![rejected(id)],
        })
        .await;
    }
    for _ in 0..2 {
        let no_answer = format!("reply - {}", words::NO_ANSWER);
        assert_eq!(said.recv().await.unwrap(), no_answer);
    }
    settle().await;
    assert!(said.try_recv().is_err());
}

async fn hand_off_again(refused: bool) {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let (mut said, wire) = h.start().await;
    heard(&wire, &["heard First question", "delegate d1 first"]).await;
    let (held, _) = h.queued().await;
    if refused {
        h.think(vec![rejected(held)]).await;
    } else {
        let failed = AgentEvent::InputHeld {
            ids: vec![held.clone()],
            by: WithdrawnBy::TurnFailed,
        };
        h.think(vec![AgentEvent::turn_started(1), failed, stopped()])
            .await;
        assert_eq!(
            h.agent.recv().await.unwrap(),
            ClientCommand::Withdraw { ids: vec![held] }
        );
    }
    assert_eq!(
        said.recv().await.unwrap(),
        format!("reply d1 {}", words::NO_ANSWER)
    );
    heard(&wire, &["heard Second question", "delegate d2 second"]).await;
    let (next, text) = h.queued().await;
    assert_eq!(
        text,
        "Heard: First question\nHeard: Second question\nAsked: second"
    );
    let mut turn = answering(&next);
    turn.extend(reply(0, "A fresh answer."));
    h.think(turn).await;
    assert_eq!(said.recv().await.unwrap(), "reply d2 A fresh answer.");
    h.think(vec![completed()]).await;
    let quiet = |n| {
        [
            format!("note c{n} {}", words::NOTHING_TO_SAY),
            format!("note d{n} {}", words::NOTHING_TO_SAY),
        ]
    };
    let failed = |n| {
        [
            format!("note c{n} {}", words::COVERED),
            format!("reply d{n} {}", words::NO_ANSWER),
        ]
    };
    let led = |n, last: &[String]| {
        [
            vec![
                format!("note c{n} {}", words::COVERED),
                format!("reply d{n} A lead."),
            ],
            last.to_vec(),
        ]
        .concat()
    };
    let no_answer = |n| [format!("reply d{n} {}", words::NO_ANSWER)];
    let cut = || completed_by(StopReason::MaxTokens);
    let error = json!({"status": "error", "kind": "invalid_response", "message": "cut off"});
    let failed_turn = ended(serde_json::from_value(error).unwrap());
    for (n, lead, end, spoken) in [
        (3, false, completed(), quiet(3).to_vec()),
        (4, false, stopped(), failed(4).to_vec()),
        (5, false, cut(), failed(5).to_vec()),
        (6, true, completed(), led(6, &[])),
        (7, true, stopped(), led(7, &no_answer(7))),
        (8, true, cut(), led(8, &no_answer(8))),
        (9, true, failed_turn, led(9, &no_answer(9))),
    ] {
        let asked = [
            format!("delegate c{n} first"),
            format!("delegate d{n} more"),
        ];
        heard(&wire, &[&asked[0], &asked[1], "heard More"]).await;
        let (id, _) = h.queued().await;
        let lead = if lead {
            reply(0, "A lead.")
        } else {
            Vec::new()
        };
        h.think([answering(&id), lead, vec![end]].concat()).await;
        for line in spoken {
            assert_eq!(said.recv().await.unwrap(), line);
        }
    }
    settle().await;
    assert!(said.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn taps_and_lines_past_the_hand_offs_limits_are_cut_and_kept_for_the_record() {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let early = VoiceCommand::Note {
        call: call(),
        text: "before the call".into(),
    };
    h.voice(early).await;
    let (id, text) = h.queued().await;
    assert_eq!(
        (id.0.as_str(), text.as_str()),
        ("tap-1", "Tapped: before the call")
    );
    tokio::time::advance(words::TAP_GAP).await;
    let (mut said, wire) = h.start().await;
    h.tap(&mut said, "first").await;
    let (first, text) = h.queued().await;
    assert_eq!(text, "Tapped: first");
    h.think(vec![rejected(first)]).await;
    settle().await;
    tokio::time::advance(words::TAP_GAP).await;
    h.tap(&mut said, "again").await;
    let (again, text) = h.queued().await;
    assert_eq!(text, "Tapped: first\nTapped: again");
    h.think([answering(&again), vec![completed()]].concat())
        .await;
    settle().await;
    tokio::time::advance(words::TAP_GAP).await;
    let long = "x".repeat(300);
    let taps: Vec<String> = (0..9).map(|n| format!("tap {n}")).chain([long]).collect();
    h.tap(&mut said, &taps[0]).await;
    let (second, text) = h.queued().await;
    assert_eq!(text, "Tapped: tap 0");
    let mut turn = answering(&second);
    turn.extend(reply(0, "Well done."));
    h.think(turn).await;
    assert_eq!(said.recv().await.unwrap(), "reply - Well done.");
    for tap in &taps[1..] {
        tokio::time::advance(words::TAP_GAP).await;
        h.tap(&mut said, tap).await;
    }
    tokio::time::advance(words::TAP_GAP).await;
    let bounce = VoiceCommand::Note {
        call: call(),
        text: "bounce".into(),
    };
    h.voice(bounce.clone()).await;
    h.voice(bounce).await;
    assert_eq!(said.recv().await.unwrap(), "note - Tapped: bounce");
    let talk: Vec<String> = (0..22).map(|n| format!("heard line {n}")).collect();
    let talk: Vec<&str> = talk.iter().map(String::as_str).collect();
    heard(&wire, &talk).await;
    heard(&wire, &["delegate d1 enough"]).await;
    settle().await;
    h.think(vec![completed()]).await;
    let (id, text) = h.queued().await;
    let tapped = |t: &str| format!("Tapped: {}", wisp_tool_text::output::cut_chars(t, 200));
    let mut expected: Vec<String> = taps[3..].iter().map(|t| tapped(t)).collect();
    expected.push(tapped("bounce"));
    expected.extend((2..22).map(|n| format!("Heard: line {n}")));
    expected.push("Asked: enough".to_owned());
    assert_eq!(text, expected.join("\n"));
    assert!(text.contains(&format!("{}…", "x".repeat(199))));
    let turn = answering(&id);
    h.think(turn.clone()).await;
    while h.events.recv().await.as_ref() != turn.last() {}
    h.voice(VoiceCommand::Stop { call: call() }).await;
    assert_eq!(h.agent.recv().await.unwrap(), ClientCommand::Cancel);
    let (_, record) = hang_up(h).await;
    let mut dropped: Vec<String> = taps[1..3].iter().map(|t| tapped(t)).collect();
    dropped.extend(["Heard: line 0".to_owned(), "Heard: line 1".to_owned()]);
    assert_eq!(record.lines, dropped);
}

#[tokio::test]
async fn every_other_command_and_event_passes_unchanged_and_closing_either_side_reaches_the_other()
{
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let commands = [
        ClientCommand::queue("u1", "hello"),
        ClientCommand::Cancel,
        ClientCommand::Release,
    ];
    for command in commands.clone() {
        h.client.send(command).await.unwrap();
    }
    for command in commands {
        assert_eq!(h.agent.recv().await.unwrap(), command);
    }
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(reply(0, "Hi."));
    events.push(completed());
    h.think(events.clone()).await;
    for event in &events {
        assert_eq!(&h.events.recv().await.unwrap(), event);
    }
    drop(std::mem::replace(&mut h.client, mpsc::channel(1).0));
    assert!(h.agent.recv().await.is_none());
    h.think(vec![completed()]).await;
    assert_eq!(h.events.recv().await.unwrap(), completed());
    drop(h.brain);
    assert!(h.events.recv().await.is_none());
}

#[tokio::test]
async fn the_voice_takes_the_kernels_settings_at_each_call() {
    let dir = ScratchDir::new("wisp-voice-test-", "instructions");
    std::fs::write(dir.root.join("VOICE.md"), "Hand every question over.").unwrap();
    let place = PresetPlace::of_config(Some(&dir.root.join("config.toml")), None, 0);
    let mut h = harness(Route::default(), place);
    h.saved.0.lock().unwrap().clear();
    h.saved.calling(&[("talk", false), ("work", false)]);
    let format = AudioFormat::Pcm16Mono48k;
    h.voice(VoiceCommand::Start {
        call: call(),
        format,
    })
    .await;
    let outcome = CallOutcome::Failed {
        kind: ErrorKind::Other,
        message: words::NO_ACCOUNT.into(),
    };
    assert!(matches!(h.voice_event().await, VoiceEvent::Ended { outcome: o, .. } if o == outcome));
    h.saved.set(names::VOICE_ENDPOINT, "talk");
    h.saved.set(names::VOICE_VOICE, "marin");
    h.saved.set(names::VOICE_INSTRUCTIONS, "VOICE.md");
    h.saved.set(names::VOICE_LANGUAGE, "de");
    let _wire = h.start().await;
    let opened = h.route.opened.lock().unwrap().clone();
    let expected = OpenCall {
        offer: "offer".into(),
        instructions: "Hand every question over.".into(),
        voice: "marin".into(),
        language: Some("de".into()),
    };
    assert_eq!(opened, [expected]);
    h.saved.set(names::VOICE_VOICE, "nova");
    h.voice(VoiceCommand::Start {
        call: call(),
        format,
    })
    .await;
    assert!(matches!(h.voice_event().await, VoiceEvent::Ended { .. }));
    assert_eq!(h.voice_event().await, VoiceEvent::Started { call: call() });
    assert_eq!(h.route.opened.lock().unwrap()[1].voice, "cove");
}

#[tokio::test]
async fn with_none_chosen_the_only_signed_in_voice_account_is_used() {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    h.saved.0.lock().unwrap().clear();
    h.saved.calling(&[]);
    let format = AudioFormat::Pcm16Mono48k;
    h.voice(VoiceCommand::Start {
        call: call(),
        format,
    })
    .await;
    let outcome = CallOutcome::Failed {
        kind: ErrorKind::Other,
        message: words::NO_VOICE_ACCOUNT.into(),
    };
    assert!(matches!(h.voice_event().await, VoiceEvent::Ended { outcome: o, .. } if o == outcome));
    h.records.recv().await.unwrap();
    h.saved.calling(&[("old", true), ("talk", false)]);
    let _wire = h.start().await;
    h.voice(VoiceCommand::Stop { call: call() }).await;
    let (_, record) = hang_up(h).await;
    assert_eq!(record.endpoint_name.as_deref(), Some("talk"));
}

#[tokio::test]
async fn only_a_route_that_answers_reads_the_instructions_file() {
    for reader in [true, false] {
        let route = Route {
            reader,
            ..Route::default()
        };
        let mut h = harness(route, PresetPlace::of_config(None, None, 0));
        h.saved.set(names::VOICE_INSTRUCTIONS, "missing/VOICE.md");
        h.voice(VoiceCommand::Start {
            call: call(),
            format: AudioFormat::Pcm16Mono48k,
        })
        .await;
        let event = h.voice_event().await;
        if reader {
            assert_eq!(event, VoiceEvent::Started { call: call() });
            assert_eq!(h.route.opened.lock().unwrap()[0].instructions, "");
        } else {
            let outcome = CallOutcome::Failed {
                kind: ErrorKind::Other,
                message: words::NO_INSTRUCTIONS.into(),
            };
            assert!(matches!(event, VoiceEvent::Ended { outcome: o, .. } if o == outcome));
            assert!(h.route.opened.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn a_call_stopped_while_it_opens_still_hangs_up_what_it_opened() {
    for step in ["offer", "open", "answer"] {
        let opening = Arc::new(Semaphore::new(0));
        let route = Route {
            gate: (step == "open").then(|| opening.clone()),
            ..Route::default()
        };
        let mut h = harness(route, PresetPlace::of_config(None, None, 0));
        let legs = h.legs.clone();
        let gate = match step {
            "offer" => &legs.offering,
            "open" => &*opening,
            _ => &legs.gate,
        };
        gate.forget_permits(Semaphore::MAX_PERMITS);
        let format = AudioFormat::Pcm16Mono48k;
        h.voice(VoiceCommand::Start {
            call: call(),
            format,
        })
        .await;
        let opened = usize::from(step != "offer");
        while h.route.opened.lock().unwrap().len() < opened {
            tokio::task::yield_now().await;
        }
        h.voice(VoiceCommand::Stop { call: call() }).await;
        let hangup = CallOutcome::Completed {
            end: CallEnd::Hangup,
        };
        assert!(matches!(
            h.voice_event().await,
            VoiceEvent::Ended { outcome, seconds: 0, .. } if outcome == hangup
        ));
        gate.add_permits(1);
        while legs.closed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(h.route.opened.lock().unwrap().len(), opened);
        assert_eq!(h.route.closed.load(Ordering::SeqCst), opened);
        let answered = usize::from(step == "answer");
        assert_eq!(legs.wires.lock().unwrap().len(), answered);
    }
}

fn accepted(id: &str, text: &str) -> AgentEvent {
    AgentEvent::InputAccepted {
        opening: false,
        id: InputId(id.into()),
        mode: wisp_protocol::DeliveryMode::Queue,
        text: text.into(),
        images: Vec::new(),
        replaces: None,
        held: false,
        front: false,
        from: None,
        checklist_spans: Vec::new(),
    }
}

#[tokio::test]
async fn a_call_opens_with_the_lessons_last_lines_and_answer_and_taps_number_past_the_log() {
    let said: Vec<String> = (0..words::EARLIER_LINES)
        .map(|n| format!("Heard: line {n}"))
        .collect();
    let mut log = vec![accepted("voice-old-1", &said.join("\n"))];
    log.extend(answering(&InputId("voice-old-1".into())));
    log.extend(reply(0, "A first block."));
    log.extend(reply(1, "A second block."));
    log.extend([requested("t0"), finished("t0", false)]);
    log.push(accepted("wisp-command-1", "not a hand-off"));
    log.extend(
        crate::agent::Agent::new(
            Fake::with(Knobs::default(), []),
            crate::agent::default_session(),
        )
        .view(&[])
        .events,
    );
    let ids = ["tap-7", "tap-x", "voice-old-1"].map(String::from);
    let view = wisp_protocol::SessionView {
        events: log,
        input_ids: ids.into(),
        ..Default::default()
    };
    let mut h = resumed(
        Route::default(),
        PresetPlace::of_config(None, None, 0),
        Earlier::of(&view.events, &view.input_ids),
    );
    let tap = VoiceCommand::Note {
        call: call(),
        text: "B".into(),
    };
    h.voice(tap).await;
    let (id, text) = h.queued().await;
    assert_eq!((id.0.as_str(), text.as_str()), ("tap-8", "Tapped: B"));
    let mut turn = vec![accepted(&id.0, &text)];
    turn.extend(answering(&id));
    turn.extend(reply(0, "Well tapped."));
    turn.extend(reply(1, "Not for the voice."));
    turn.extend([requested("t1"), finished("t1", false)]);
    turn.extend(reply(2, "After the board."));
    turn.push(completed());
    h.think(turn).await;
    let mut opening = accepted(
        "voice-hosted-direct",
        "a hosted opening stays out of voice memory",
    );
    if let AgentEvent::InputAccepted { opening, .. } = &mut opening {
        *opening = true;
    }
    h.think(vec![
        opening,
        AgentEvent::InputDelivered {
            ids: vec![InputId("voice-hosted-direct".into())],
            step: 1,
        },
    ])
    .await;
    settle().await;
    let (mut said, _wire) = h.start().await;
    let mut note = vec![words::EARLIER.to_owned()];
    note.extend((1..words::EARLIER_LINES).map(|n| format!("Heard: line {n}")));
    note.extend([
        "Tapped: B".to_owned(),
        "Said: Well tapped. After the board.".to_owned(),
    ]);
    assert_eq!(
        said.recv().await.unwrap(),
        format!("note - {}", note.join("\n"))
    );
    settle().await;
    assert!(said.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn a_calls_last_lines_go_first_in_the_next_hand_off_and_start_none() {
    let mut h = harness(Route::default(), PresetPlace::of_config(None, None, 0));
    let (_said, wire) = h.start().await;
    heard(
        &wire,
        &["delegate d0 fractions", "heard What is a fraction?"],
    )
    .await;
    let (withdrawn, text) = h.queued().await;
    h.think(vec![accepted(&withdrawn.0, &text)]).await;
    settle().await;
    h.voice(VoiceCommand::Stop { call: call() }).await;
    let withdraw = ClientCommand::Withdraw {
        ids: vec![withdrawn],
    };
    assert_eq!(h.agent.recv().await.unwrap(), withdraw);
    h.voice_event().await;
    let first = ["Heard: What is a fraction?", "Asked: fractions"];
    assert_eq!(h.records.recv().await.unwrap().lines, first);
    let (mut said, wire) = h.start().await;
    heard(&wire, &["said Hello again."]).await;
    tokio::time::sleep(REPLY_WAIT * 2).await;
    assert!(h.agent.try_recv().is_err());
    heard(&wire, &["delegate d1 what next"]).await;
    let (id, text) = h.queued().await;
    let handed = [first[0], first[1], "Said: Hello again.", "Asked: what next"];
    assert_eq!(text, handed.join("\n"));
    let mut turn = vec![accepted(&id.0, &text)];
    turn.extend(answering(&id));
    turn.extend(reply(0, "Fractions."));
    turn.push(completed());
    h.think(turn).await;
    assert_eq!(said.recv().await.unwrap(), "reply d1 Fractions.");
    heard(&wire, &["said See you."]).await;
    settle().await;
    h.voice(VoiceCommand::Stop { call: call() }).await;
    h.voice_event().await;
    assert_eq!(h.records.recv().await.unwrap().lines, ["Said: See you."]);
    h.voice(VoiceCommand::Note {
        call: call(),
        text: "C".into(),
    })
    .await;
    assert_eq!(h.queued().await.1, "Said: See you.\nTapped: C");
    let (mut said, _wire) = h.start().await;
    let note = [words::EARLIER, &handed.join("\n"), "Said: Fractions."].join("\n");
    assert_eq!(said.recv().await.unwrap(), format!("note - {note}"));
}

async fn waited(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if done() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{what} never came");
}

async fn rewired(legs: &Legs) -> Wire {
    let mut wire = None;
    waited("a new leg", || {
        wire = legs.wires.lock().unwrap().pop();
        wire.is_some()
    })
    .await;
    wire.unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_call_reopens_before_its_routes_limit_at_a_quiet_moment_or_at_its_deadline() {
    let limit = Duration::from_secs(600);
    let second = Duration::from_secs(1);
    let route = Route {
        limit: Some(limit),
        ..Route::default()
    };
    let mut h = harness(route, PresetPlace::of_config(None, None, 0));
    let (mut said, wire) = h.start().await;
    let up = Instant::now();
    heard(&wire, &["delegate d1 fractions"]).await;
    let (id, _) = h.queued().await;
    h.think(answering(&id)).await;
    h.route.busy.store(true, Ordering::SeqCst);
    tokio::time::sleep_until(up + limit - RENEW_EARLY + second).await;
    settle().await;
    assert_eq!(h.route.opened.lock().unwrap().len(), 1);
    h.route.busy.store(false, Ordering::SeqCst);
    heard(&wire, &["error", "done"]).await;
    let (_said, _rewired) = rewired(&h.legs).await;
    let renewed = Instant::now();
    assert_eq!(said.recv().await.as_deref(), Some("hang up"));
    assert_eq!(said.recv().await, None);
    let hangup = CallOutcome::Completed {
        end: CallEnd::Hangup,
    };
    let first = h.records.recv().await.unwrap();
    assert_eq!((&first.outcome, first.seconds), (&hangup, 541));
    h.route.busy.store(true, Ordering::SeqCst);
    tokio::time::sleep_until(renewed + limit - RENEW_LAST - second).await;
    settle().await;
    assert_eq!(h.route.opened.lock().unwrap().len(), 2);
    tokio::time::sleep(second * 2).await;
    let (mut said, _last) = rewired(&h.legs).await;
    let opened = h.route.opened.lock().unwrap().clone();
    assert_eq!(
        opened,
        [opened[0].clone(), opened[0].clone(), opened[0].clone()]
    );
    let next = h.records.recv().await.unwrap();
    assert_eq!((&next.outcome, next.seconds), (&hangup, 590));
    h.think([reply(0, "Halves."), vec![completed()]].concat())
        .await;
    assert_eq!(said.recv().await.unwrap(), "reply d1 Halves.");
    h.voice(VoiceCommand::Stop { call: call() }).await;
    let (ended, last) = hang_up(h).await;
    let VoiceEvent::Ended {
        call: ended_call,
        outcome,
        seconds,
        cost,
    } = ended
    else {
        panic!("{ended:?}");
    };
    assert_eq!((ended_call, outcome), (call(), hangup));
    let records = [first, next, last];
    assert_eq!(seconds, records.iter().map(|r| r.seconds).sum::<u64>());
    let usd: f64 = records.iter().filter_map(|r| r.cost_usd).sum();
    assert_eq!(cost.usd, usd);
    assert!(records.iter().all(|r| r.call == call()));
}

#[tokio::test(start_paused = true)]
async fn a_dropped_call_reopens_with_its_says_held_and_ends_when_a_reopen_fails() {
    let route = Route {
        refuse: Some(ErrorKind::Overloaded),
        refuse_after: 2,
        ..Route::default()
    };
    let mut h = harness(route, PresetPlace::of_config(None, None, 0));
    let (_said, wire) = h.start().await;
    heard(&wire, &["delegate d1 fractions"]).await;
    let (id, _) = h.queued().await;
    h.think(answering(&id)).await;
    tokio::time::sleep(STEADY).await;
    h.legs.gate.forget_permits(Semaphore::MAX_PERMITS);
    heard(&wire, &["error"]).await;
    drop(wire);
    waited("a second dial", || {
        h.route.opened.lock().unwrap().len() == 2
    })
    .await;
    h.think(streamed(0, &["Halves. "])).await;
    settle().await;
    h.legs.gate.add_permits(1);
    let (mut said, wire) = rewired(&h.legs).await;
    let note = format!("note - {}\nSaid: Halves.", words::EARLIER);
    assert_eq!(said.recv().await.unwrap(), note);
    assert_eq!(said.recv().await.unwrap(), "reply d1 Halves.");
    let first = h.records.recv().await.unwrap();
    let limited = CallOutcome::Failed {
        kind: ErrorKind::RateLimit,
        message: "limited".into(),
    };
    assert_eq!((first.outcome, first.seconds), (limited, 60));
    tokio::time::sleep(STEADY).await;
    drop(wire);
    let refused = CallOutcome::Failed {
        kind: ErrorKind::Overloaded,
        message: "refused".into(),
    };
    let ended = h.voice_event().await;
    assert!(
        matches!(&ended, VoiceEvent::Ended { outcome, seconds: 120, .. } if *outcome == refused),
        "{ended:?}"
    );
    let next = h.records.recv().await.unwrap();
    let remote = CallOutcome::Completed {
        end: CallEnd::Remote,
    };
    assert_eq!((next.outcome, next.seconds), (remote, 60));
    let last = h.records.recv().await.unwrap();
    assert_eq!(
        (last.outcome, last.seconds, last.cost_usd),
        (refused, 0, None)
    );
    assert_eq!(h.route.opened.lock().unwrap().len(), 3);
}
