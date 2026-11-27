use tokio::sync::watch;
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::{
    AgentAction, AgentPath, HoldReason, MailFrom, MailId, MailKind, RequestId, RewindPoint,
    TurnOrigin, WakeSource,
};

use super::*;
use crate::agent::{Mail, MailSender, NOT_YOURS, WAKE_SETTLE, render_mail};
use wisp_tool_text::jobs::WAKE_LINE;

mod paths;
mod returned;
mod turn_form;

const HERE: &str = "/review";
const SIBLING: &str = "/docs";
const CHILD: &str = "/review/coder";
const PARENT: &str = "/";

fn letter(id: &str, from: &str, wake: bool) -> Mail {
    Mail {
        id: MailId(id.into()),
        from: AgentPath::new(from),
        to: AgentPath::new(HERE),
        kind: MailKind::Message,
        wake,
        text: format!("note {id}"),
        checklist_spans: Vec::new(),
    }
}

fn task(id: &str) -> Mail {
    Mail {
        kind: MailKind::Task,
        ..letter(id, PARENT, true)
    }
}

fn shown(mail: &Mail) -> String {
    render_mail(&mail.from, mail.kind, &mail.text)
}

fn mid(id: &str) -> InputId {
    InputId(format!("mail-{id}"))
}

struct Built {
    agent: Agent,
    provider: Arc<Fake>,
    store: MemoryStore,
    env: Env,
    mail: MailSender,
}

fn build(
    name: &str,
    plan: Vec<Step>,
    tools: Toolbox,
    commands: Option<mpsc::Sender<ClientCommand>>,
    gate: Option<watch::Receiver<bool>>,
) -> Built {
    build_on(default_session(), name, plan, tools, commands, gate)
}

fn build_on(
    session: SessionConfig,
    name: &str,
    plan: Vec<Step>,
    tools: Toolbox,
    commands: Option<mpsc::Sender<ClientCommand>>,
    gate: Option<watch::Receiver<bool>>,
) -> Built {
    let env = Env::new(name);
    let (mailing, inner) = staged(plan);
    if let Some(commands) = commands {
        inner.send_to(commands);
    }
    let store = MemoryStore::default();
    let mut agent = Agent::new(mailing, session)
        .with_clock(TickingClock::new())
        .with_tools(tools, env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        )
        .with_path(AgentPath::new(HERE));
    if let Some(gate) = gate {
        agent = agent.with_run_gate(gate);
    }
    let mail = agent.mail_sender();
    inner.mail_to(mail.clone());
    Built {
        agent,
        provider: inner,
        store,
        env,
        mail,
    }
}

fn mailed_with(
    name: &str,
    plan: Vec<Step>,
    gate: Option<watch::Receiver<bool>>,
) -> (Harness, MailSender) {
    mailed_with_on(default_session(), name, plan, gate)
}

fn mailed_with_on(
    session: SessionConfig,
    name: &str,
    plan: Vec<Step>,
    gate: Option<watch::Receiver<bool>>,
) -> (Harness, MailSender) {
    let (tx, rx) = mpsc::channel(128);
    let tools = ScriptTool::toolbox(tx.clone(), Duration::from_secs(10), Ops::default());
    let built = build_on(session, name, plan, tools, Some(tx.clone()), gate);
    let harness = Harness {
        agent: built.agent,
        provider: built.provider,
        tx,
        rx,
        store: built.store,
        _env: built.env,
    };
    (harness, built.mail)
}

fn mailed(name: &str, plan: Vec<Step>) -> (Harness, MailSender) {
    mailed_with(name, plan, None)
}

fn resumed_at(
    name: &str,
    path: &str,
    plan: Vec<Step>,
    records: &[LogRecord],
) -> (Harness, MailSender) {
    let env = Env::new(name);
    let (tx, rx) = mpsc::channel(128);
    let (mailing, inner) = staged(plan);
    inner.send_to(tx.clone());
    let (store, log) = continued(records);
    let agent = Agent::new(mailing, default_session())
        .with_clock(TickingClock::new())
        .with_tools(
            ScriptTool::toolbox(tx.clone(), Duration::from_secs(10), Ops::default()),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_log(log, log_context())
        .with_path(AgentPath::new(path))
        .resume(records, 0)
        .unwrap();
    let mail = agent.mail_sender();
    inner.mail_to(mail.clone());
    let harness = Harness {
        agent,
        provider: inner,
        tx,
        rx,
        store,
        _env: env,
    };
    (harness, mail)
}

fn wake_helds(events: &[AgentEvent]) -> Vec<(Vec<WakeSource>, HoldReason)> {
    pick!(events, AgentEvent::WakeHeld { sources, reason } => (sources.clone(), *reason))
}

fn logged_helds(records: &[LogRecord]) -> Vec<(u64, HoldReason)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::WakeHeld { turn, reason, .. } => Some((*turn, *reason)),
            _ => None,
        })
        .collect()
}

fn from_mail(id: &str, from: &str) -> WakeSource {
    WakeSource::Mail {
        mail: MailId(id.into()),
        from: AgentPath::new(from),
    }
}

fn origins(events: &[AgentEvent]) -> Vec<Option<TurnOrigin>> {
    pick!(events, AgentEvent::TurnStarted { origin, .. } => *origin)
}

fn accepted_mail(events: &[AgentEvent]) -> Vec<InputId> {
    pick!(
        events,
        AgentEvent::InputAccepted {
            id, from: Some(_), ..
        } => id.clone()
    )
}

fn untouched(events: &[AgentEvent], records: &[LogRecord], mail: &[InputId]) {
    for (ids, by) in withdrawn(events).into_iter().chain(held(events)) {
        assert!(
            !ids.iter().any(|id| mail.contains(id)),
            "{by:?} named mail: {ids:?}"
        );
    }
    for record in records {
        if let LogEvent::InputWithdrawn { ids, .. } | LogEvent::InputHeld { ids, .. } =
            &record.event
        {
            assert!(!ids.iter().any(|id| mail.contains(id)), "{record:?}");
        }
    }
}

fn text_count(messages: &[Message], wanted: &str) -> usize {
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .flat_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => content.clone(),
            other => vec![other.clone()],
        })
        .filter(|b| matches!(b, ContentBlock::Text { text } if text == wanted))
        .count()
}

fn last_texts(h: &Harness) -> Vec<String> {
    let requests = h.requests();
    texts(requests.last().unwrap().last().unwrap())
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn ends_with(all: &[String], tail: &[String]) {
    assert!(all.ends_with(tail), "{all:#?}\n{tail:#?}");
}

fn retry(turn: u64) -> ClientCommand {
    ClientCommand::Retry {
        turn,
        endpoint: None,
        request: RequestId(9),
    }
}

async fn settle_quietly(h: &mut Harness) -> Vec<AgentEvent> {
    h.drive(Vec::new(), nothing, never, Duration::from_secs(5))
        .await
}

#[tokio::test(start_paused = true)]
async fn a_mail_at_a_step_boundary_lands_after_the_results_and_your_steer() {
    let note = letter("m1", SIBLING, true);
    let (mut h, _mail) = mailed(
        "mail-boundary",
        vec![
            two_calls().then(steer("also this")).mail(note.clone()),
            done("ok"),
        ],
    );
    let events = h.run(vec![steer("go")], 1).await;
    check_events(&events);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed { steps: 2, .. }]
    ));
    assert_eq!(
        delivered(&events),
        [(ids(&["go"]), 1), (vec![id("also this"), mid("m1")], 2)]
    );
    let requests = h.requests();
    let tail = requests[1].last().unwrap();
    assert_eq!(
        results(tail).iter().map(|r| r.0).collect::<Vec<_>>(),
        ["a", "b"]
    );
    let n = tail.content.len();
    assert_eq!(
        tail.content[n - 3..],
        [text("also this"), text(STEER_NOTE), text(&shown(&note))]
    );
    assert_each_request_extends_the_last(&requests);
    assert_bodies_extend(&h.provider, &[(0, 1)]);
    h.assert_rebuilds();
    let logged = log_events(&h.store);
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::InputCommitted { step: 2, inputs, .. }
            if inputs.iter().map(|i| (i.id.clone(), i.note.as_deref())).collect::<Vec<_>>()
                == [(id("also this"), Some(STEER_NOTE)), (mid("m1"), None)]
    )));
    assert!(h.agent.inbox.mail_ids().is_empty());
}

#[tokio::test(start_paused = true)]
async fn mail_from_above_at_the_last_step_keeps_the_turn_going() {
    let brief = task("m1");
    let (mut h, _mail) = mailed(
        "mail-above-last",
        vec![done("first").mail(brief.clone()), done("second")],
    );
    let events = h.run(vec![steer("go")], 1).await;
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed { steps: 2, .. }]
        ),
        "{events:#?}"
    );
    assert_eq!(delivered(&events)[1], (vec![mid("m1")], 2));
    assert_eq!(last_texts(&h), [shown(&brief)]);
    assert_each_request_extends_the_last(&h.requests());
    assert_bodies_extend(&h.provider, &[(0, 1)]);
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn peer_and_child_mail_at_the_last_step_wait_and_wake_one_turn_after_the_settle() {
    let peer = letter("m1", SIBLING, true);
    let child = Mail {
        kind: MailKind::Report,
        ..letter("m2", CHILD, true)
    };
    let (mut h, _mail) = mailed(
        "mail-peer-last",
        vec![
            done("first").mail(peer.clone()).mail(child.clone()),
            done("woke"),
        ],
    );
    let mut ended_at = None;
    let mut started_at = None;
    let events = h
        .drive(
            vec![steer("go")],
            |e| {
                match e {
                    AgentEvent::TurnEnded { .. } if ended_at.is_none() => {
                        ended_at = Some(tokio::time::Instant::now());
                    }
                    AgentEvent::TurnStarted { turn: 2, .. } => {
                        started_at = Some(tokio::time::Instant::now());
                    }
                    _ => {}
                }
                Vec::new()
            },
            turns(2),
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&events), 2, "{events:#?}");
    assert!(matches!(
        outcomes(&events)[0],
        TurnOutcome::Completed { steps: 1, .. }
    ));
    assert_eq!(origins(&events), [None, Some(TurnOrigin::Mail)]);
    let waited = started_at.unwrap() - ended_at.unwrap();
    assert!(waited >= WAKE_SETTLE, "{waited:?}");
    assert!(
        waited < WAKE_SETTLE + Duration::from_millis(500),
        "{waited:?}"
    );
    ends_with(&last_texts(&h), &[shown(&peer), shown(&child)]);
    assert!(!last_texts(&h).iter().any(|t| t.contains(WAKE_LINE)));
    assert!(wake_helds(&events).is_empty());
    let logged = log_events(&h.store);
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::TurnStarted { turn: 2, origin: Some(TurnOrigin::Mail), inputs, text, .. }
            if text.is_empty()
                && inputs.iter().map(|i| i.id.clone()).collect::<Vec<_>>() == [mid("m1"), mid("m2")]
    )));
    assert_each_request_extends_the_last(&h.requests());
    assert_bodies_extend(&h.provider, &[(0, 1)]);
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn two_mails_a_second_apart_share_one_wake() {
    let (mut h, mail) = mailed("mail-batch", vec![done("woke")]);
    let first = letter("m1", SIBLING, true);
    let second = letter("m2", CHILD, true);
    let begun = tokio::time::Instant::now();
    mail.send(first.clone()).unwrap();
    let later = mail.clone();
    let sent = second.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        later.send(sent).unwrap();
    });
    let mut started_at = None;
    let events = h
        .drive(
            Vec::new(),
            |e| {
                if matches!(e, AgentEvent::TurnStarted { .. }) {
                    started_at = Some(tokio::time::Instant::now());
                }
                Vec::new()
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(origins(&events), [Some(TurnOrigin::Mail)]);
    let waited = started_at.unwrap() - begun;
    assert!(waited >= WAKE_SETTLE, "{waited:?}");
    assert!(
        waited < WAKE_SETTLE + Duration::from_millis(500),
        "{waited:?}"
    );
    ends_with(&last_texts(&h), &[shown(&first), shown(&second)]);
    assert_eq!(h.requests().len(), 1);
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn a_stopped_turn_requeues_its_mail_and_the_next_turn_carries_it_once() {
    let note = letter("m1", SIBLING, true);
    let (mut h, _mail) = mailed(
        "mail-stopped",
        vec![
            two_calls().mail(note.clone()),
            hanging(&[B::Text("partial")]),
            done("ok"),
        ],
    );
    let stopped = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::InputDelivered { ids, .. } if ids.contains(&mid("m1")) => {
                    vec![ClientCommand::Cancel]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(matches!(
        outcomes(&stopped)[..],
        [TurnOutcome::Cancelled { .. }]
    ));
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1")]);
    let waiting = settle_quietly(&mut h).await;
    assert_eq!(started(&waiting), Vec::<u64>::new());
    let mut all = stopped;
    all.extend(waiting);
    assert_eq!(
        wake_helds(&all),
        [(vec![from_mail("m1", SIBLING)], HoldReason::Stopped)]
    );
    let next = h.run(vec![steer("next")], 1).await;
    all.extend(next);
    untouched(&all, &records(&h.store), &[mid("m1")]);
    ends_with(&last_texts(&h), &["next".to_owned(), shown(&note)]);
    let requests = h.requests();
    assert_eq!(text_count(requests.last().unwrap(), &shown(&note)), 1);
    assert_eq!(accepted_mail(&all), [mid("m1")]);
    assert!(h.agent.inbox.mail_ids().is_empty());
    assert_eq!(logged_helds(&records(&h.store)), [(1, HoldReason::Stopped)]);
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn a_retry_resends_delivered_mail_byte_for_byte_and_delivers_it_once() {
    let note = letter("m1", SIBLING, true);
    let (mut h, _mail) = mailed(
        "mail-retry",
        vec![
            two_calls().mail(note.clone()),
            server_error_step(),
            done("ok"),
        ],
    );
    let failed = h.run(vec![steer("go")], 1).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    let waiting = settle_quietly(&mut h).await;
    let mut held_so_far = failed.clone();
    held_so_far.extend(waiting.iter().cloned());
    assert_eq!(
        wake_helds(&held_so_far),
        [(vec![from_mail("m1", SIBLING)], HoldReason::RetryOffer)]
    );
    let retried = h.run(vec![retry(1)], 1).await;
    let after = settle_quietly(&mut h).await;
    assert_eq!(started(&after), Vec::<u64>::new(), "{after:#?}");
    let bodies = h.provider.texts();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[1], bodies[2], "the retry resends the failed request");
    let requests = h.requests();
    assert_eq!(text_count(&requests[2], &shown(&note)), 1);
    let mut all = failed;
    all.extend(waiting);
    all.extend(retried);
    all.extend(after);
    untouched(&all, &records(&h.store), &[mid("m1")]);
    assert_eq!(accepted_mail(&all), [mid("m1")]);
    let committed = records(&h.store)
        .iter()
        .filter(|r| {
            matches!(
                &r.event,
                LogEvent::InputCommitted { inputs, .. } if inputs.iter().any(|i| i.id == mid("m1"))
            )
        })
        .count();
    assert_eq!(committed, 1);
    assert!(h.agent.inbox.mail_ids().is_empty());
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn after_a_failed_turn_mail_waits_once_and_mail_from_above_carries_it() {
    let peer = letter("m1", SIBLING, true);
    let brief = task("m2");
    let (mut h, mail) = mailed("mail-failed", vec![unoffered(), done("ok")]);
    let failed = h.run(vec![steer("go")], 1).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: None, .. }]
    ));
    mail.send(peer.clone()).unwrap();
    let waiting = settle_quietly(&mut h).await;
    mail.send(letter("m3", CHILD, true)).unwrap();
    let still = settle_quietly(&mut h).await;
    assert_eq!(started(&waiting), Vec::<u64>::new());
    assert_eq!(
        wake_helds(&waiting),
        [(vec![from_mail("m1", SIBLING)], HoldReason::Stopped)]
    );
    assert!(wake_helds(&still).is_empty(), "{still:#?}");
    mail.send(brief.clone()).unwrap();
    let woke = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&woke), [Some(TurnOrigin::Mail)]);
    let all = last_texts(&h);
    ends_with(
        &all,
        &[
            shown(&brief),
            shown(&peer),
            shown(&letter("m3", CHILD, true)),
        ],
    );
    assert_eq!(logged_helds(&records(&h.store)), [(1, HoldReason::Stopped)]);
}

#[tokio::test(start_paused = true)]
async fn while_your_messages_are_held_mail_waits_once_and_release_carries_it() {
    let note = letter("m1", SIBLING, true);
    let (mut h, mail) = mailed(
        "mail-held",
        vec![uses(&[steps("a", &["queue:q1", "cancel"])]), done("ok")],
    );
    let stopped = h.run(vec![steer("go")], 1).await;
    assert_eq!(held(&stopped), [(ids(&["q1"]), WithdrawnBy::TurnStopped)]);
    mail.send(note.clone()).unwrap();
    let waiting = settle_quietly(&mut h).await;
    assert_eq!(
        wake_helds(&waiting),
        [(vec![from_mail("m1", SIBLING)], HoldReason::HeldMessages)]
    );
    assert_eq!(started(&waiting), Vec::<u64>::new());
    h.run(vec![ClientCommand::Release], 1).await;
    ends_with(&last_texts(&h), &["q1".to_owned(), shown(&note)]);
    assert_eq!(
        logged_helds(&records(&h.store)),
        [(1, HoldReason::HeldMessages)]
    );
}

#[tokio::test(start_paused = true)]
async fn while_a_retry_offer_stands_mail_waits_once_and_wakes_after_the_retry() {
    let note = letter("m1", SIBLING, true);
    let (mut h, mail) = mailed(
        "mail-offer",
        vec![server_error_step(), done("again"), done("woke")],
    );
    h.run(vec![steer("go")], 1).await;
    mail.send(note.clone()).unwrap();
    let waiting = settle_quietly(&mut h).await;
    assert_eq!(
        wake_helds(&waiting),
        [(vec![from_mail("m1", SIBLING)], HoldReason::RetryOffer)]
    );
    let events = h.run(vec![retry(1)], 2).await;
    assert_eq!(origins(&events), [None, Some(TurnOrigin::Mail)]);
    ends_with(&last_texts(&h), &[shown(&note)]);
    assert_eq!(text_count(&h.requests()[1], &shown(&note)), 0);
    assert_eq!(
        logged_helds(&records(&h.store)),
        [(1, HoldReason::RetryOffer)]
    );
}

#[tokio::test(start_paused = true)]
async fn after_an_esc_in_the_settle_window_mail_waits_until_your_send() {
    let note = letter("m1", SIBLING, true);
    let (mut h, mail) = mailed("mail-esc-settle", vec![done("ok")]);
    mail.send(note.clone()).unwrap();
    let waiting = h
        .drive(
            Vec::new(),
            |e| match e {
                AgentEvent::InputAccepted { from: Some(_), .. } => vec![ClientCommand::Cancel],
                _ => Vec::new(),
            },
            never,
            Duration::from_secs(5),
        )
        .await;
    assert_eq!(started(&waiting), Vec::<u64>::new(), "{waiting:#?}");
    assert_eq!(
        wake_helds(&waiting),
        [(vec![from_mail("m1", SIBLING)], HoldReason::Stopped)]
    );
    h.run(vec![steer("hi")], 1).await;
    ends_with(&last_texts(&h), &["hi".to_owned(), shown(&note)]);
}

#[tokio::test(start_paused = true)]
async fn a_closed_concurrency_gate_holds_a_wake_once_per_episode() {
    let (gate, open) = watch::channel(false);
    let (mut h, mail) = mailed_with("mail-gate", vec![done("woke")], Some(open));
    mail.send(letter("m1", SIBLING, true)).unwrap();
    let first = settle_quietly(&mut h).await;
    mail.send(letter("m2", CHILD, true)).unwrap();
    let second = settle_quietly(&mut h).await;
    assert_eq!(
        wake_helds(&first),
        [(vec![from_mail("m1", SIBLING)], HoldReason::Concurrency)]
    );
    assert!(wake_helds(&second).is_empty(), "{second:#?}");
    assert_eq!(started(&first), Vec::<u64>::new());
    assert_eq!(started(&second), Vec::<u64>::new());
    gate.send_replace(true);
    let woke = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&woke), [Some(TurnOrigin::Mail)]);
    ends_with(
        &last_texts(&h),
        &[
            shown(&letter("m1", SIBLING, true)),
            shown(&letter("m2", CHILD, true)),
        ],
    );
    gate.send_replace(false);
    mail.send(letter("m3", SIBLING, true)).unwrap();
    let again = settle_quietly(&mut h).await;
    assert_eq!(
        wake_helds(&again),
        [(vec![from_mail("m3", SIBLING)], HoldReason::Concurrency)]
    );
    assert_eq!(
        logged_helds(&records(&h.store)),
        [(0, HoldReason::Concurrency), (1, HoldReason::Concurrency)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_gate_that_opens_after_the_settle_starts_the_wake_at_once() {
    let (gate, open) = watch::channel(false);
    let (mut h, mail) = mailed_with("mail-gate-settled", vec![done("woke")], Some(open));
    mail.send(letter("m1", SIBLING, true)).unwrap();
    let held = settle_quietly(&mut h).await;
    assert_eq!(
        wake_helds(&held),
        [(vec![from_mail("m1", SIBLING)], HoldReason::Concurrency)]
    );
    let opened = tokio::time::Instant::now();
    gate.send_replace(true);
    let woke = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&woke), [Some(TurnOrigin::Mail)]);
    assert!(
        opened.elapsed() < WAKE_SETTLE,
        "no second settle: {:?}",
        opened.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn a_closed_gate_holds_nothing_while_the_agent_is_busy() {
    let (_gate, open) = watch::channel(false);
    let note = letter("m1", SIBLING, true);
    let (mut h, _mail) = mailed_with(
        "mail-gate-busy",
        vec![two_calls().mail(note.clone()), done("ok")],
        Some(open),
    );
    let events = h.run(vec![steer("go")], 1).await;
    let after = settle_quietly(&mut h).await;
    assert!(wake_helds(&events).is_empty(), "{events:#?}");
    assert!(wake_helds(&after).is_empty(), "{after:#?}");
    assert_eq!(delivered(&events)[1], (vec![mid("m1")], 2));
}

#[tokio::test(start_paused = true)]
async fn mail_sent_mid_stream_is_logged_on_arrival() {
    let (mut h, mail) = mailed("mail-mid-stream", vec![hanging(&[B::Text("thinking")])]);
    let note = letter("m1", SIBLING, false);
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::BlockStarted { .. } => {
                    mail.send(note.clone()).unwrap();
                    Vec::new()
                }
                AgentEvent::InputAccepted { from: Some(_), .. } => vec![ClientCommand::Cancel],
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(accepted_mail(&events), [mid("m1")]);
    let logged = log_events(&h.store);
    let arrived = logged
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::InputAccepted { turn: Some(1), from: Some(from), .. }
                    if from.mail == MailId("m1".into()) && !from.wake
            )
        })
        .unwrap();
    let end = logged
        .iter()
        .position(|e| matches!(*e, LogEvent::TurnEnded { .. }))
        .unwrap();
    assert!(arrived < end);
}

#[tokio::test(start_paused = true)]
async fn mail_sent_mid_tool_is_logged_on_arrival() {
    let (mut h, mail) = mailed(
        "mail-mid-tool",
        vec![uses(&[steps("a", &["sleep:500"])]), done("ok")],
    );
    let note = letter("m1", SIBLING, true);
    let events = h
        .drive(
            vec![steer("go")],
            |e| {
                if matches!(e, AgentEvent::ToolCallStarted { .. }) {
                    mail.send(note.clone()).unwrap();
                }
                Vec::new()
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(accepted_mail(&events), [mid("m1")]);
    let logged = log_events(&h.store);
    let arrived = logged
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::InputAccepted {
                    turn: Some(1),
                    from: Some(_),
                    ..
                }
            )
        })
        .unwrap();
    let finished = logged
        .iter()
        .position(|e| matches!(*e, LogEvent::ToolCallFinished { .. }))
        .unwrap();
    assert!(arrived < finished);
    assert_eq!(delivered(&events)[1], (vec![mid("m1")], 2));
}

#[tokio::test(start_paused = true)]
async fn mail_is_not_yours_to_withdraw_or_edit() {
    let (mut h, mail) = mailed("mail-not-yours", Vec::new());
    mail.send(letter("m1", SIBLING, false)).unwrap();
    let events = h
        .idle(vec![
            ClientCommand::Withdraw {
                ids: vec![mid("m1")],
            },
            edit("e1", "changed", DeliveryMode::Queue, "mail-m1"),
        ])
        .await;
    assert_eq!(
        rejected_kinds(&events),
        [
            (Some(mid("m1")), Some(RejectKind::Other(NOT_YOURS.into()))),
            (Some(id("e1")), Some(RejectKind::Other(NOT_YOURS.into())))
        ]
    );
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1")]);
    untouched(&events, &records(&h.store), &[mid("m1")]);
}

#[tokio::test(start_paused = true)]
async fn a_refused_agent_action_is_an_event_and_a_record() {
    let (mut h, _mail) = mailed("mail-refused", Vec::new());
    let (tx, mut rx) = mpsc::channel(16);
    h.agent
        .refused_action(AgentAction::Send, "no agent named x".into(), &tx)
        .await;
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert!(events.contains(&AgentEvent::AgentRefused {
        by: AgentPath::new(HERE),
        action: AgentAction::Send,
        reason: "no agent named x".into(),
    }));
    assert!(log_events(&h.store).iter().any(|e| matches!(
        e,
        LogEvent::AgentRefused { by, action: AgentAction::Send, .. } if by.as_str() == HERE
    )));
}

#[tokio::test]
async fn a_process_end_and_a_mail_share_one_turn() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 3);
    let (tx, rx) = mpsc::channel(128);
    let built = build(
        "mail-process",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": &a.command, "run_in_background": true}),
            )]),
            done("started"),
            done("woke"),
        ],
        job_tools(),
        None,
        None,
    );
    let mut h = Harness {
        agent: built.agent,
        provider: built.provider,
        tx,
        rx,
        store: built.store,
        _env: built.env,
    };
    let note = letter("m1", SIBLING, true);
    h.run(vec![steer("go")], 1).await;
    built.mail.send(note.clone()).unwrap();
    a.open(&h._env.work);
    let woke = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&woke), [Some(TurnOrigin::Mail)]);
    let last = last_texts(&h);
    ends_with(&last, &[shown(&note)]);
    let notice = last
        .iter()
        .find(|t| t.starts_with("<processes>"))
        .unwrap_or_else(|| panic!("{last:#?}"));
    assert!(!notice.contains(WAKE_LINE), "{notice}");
    assert!(notice.contains("Process 1"), "{notice}");
    let after = h
        .drive(Vec::new(), nothing, never, Duration::from_secs(3))
        .await;
    assert_eq!(started(&after), Vec::<u64>::new());
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_mail_from_above_is_known_by_the_paths() {
    let from = MailFrom {
        agent: AgentPath::new(PARENT),
        kind: MailKind::Task,
        mail: MailId("m1".into()),
        wake: true,
        plugin: None,
    };
    let (tx, _rx) = mpsc::channel(1);
    let built = build(
        "mail-letter",
        Vec::new(),
        ScriptTool::toolbox(tx, Duration::from_secs(1), Ops::default()),
        None,
        None,
    );
    assert!(built.agent.letter(from.clone()).above);
    let sibling = MailFrom {
        agent: AgentPath::new(SIBLING),
        ..from.clone()
    };
    assert!(!built.agent.letter(sibling).above);
    let child = MailFrom {
        agent: AgentPath::new(CHILD),
        ..from
    };
    assert!(!built.agent.letter(child).above);
}

#[tokio::test(start_paused = true)]
async fn with_the_notice_off_a_mail_wake_still_runs() {
    let off = hook_switch(1, false);
    let peer = letter("m1", SIBLING, true);
    let (mut h, _mail) = mailed(
        "mail-notice-off",
        vec![done("first").mail(peer.clone()), done("woke")],
    );
    let events = h
        .drive(vec![off, steer("go")], nothing, turns(2), PATIENT)
        .await;
    assert_eq!(origins(&events), [None, Some(TurnOrigin::Mail)]);
    ends_with(&last_texts(&h), &[shown(&peer)]);
    assert!(
        !log_events(&h.store).iter().any(|e| matches!(
            e,
            LogEvent::HookRan { hook, .. } if hook == wisp_protocol::log::LEAD_PROCESSES
        )),
        "{:?}",
        records(&h.store)
    );
}

fn retry_starts(events: &[AgentEvent]) -> Vec<(u64, Option<u64>, Option<TurnOrigin>)> {
    pick!(
        events,
        AgentEvent::TurnStarted { turn, retry_of, origin, .. } => (*turn, *retry_of, *origin)
    )
}

fn transient(kind: ErrorKind) -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        Err(crate::agent::testing::failure(
            kind,
            RetryClass::Transient,
            false,
        )),
    ])
}

fn unreadable_loss() -> Vec<Step> {
    (0..=crate::agent::retry::INVALID_RESPONSE_RETRIES)
        .map(|_| transient(ErrorKind::InvalidResponse))
        .collect()
}

fn root_letter(id: &str, from: &str, wake: bool) -> Mail {
    Mail {
        to: AgentPath::root(),
        ..letter(id, from, wake)
    }
}

fn root_mailed(name: &str, plan: Vec<Step>) -> (Harness, MailSender) {
    let (mut h, mail) = mailed(name, plan);
    h.agent = h.agent.with_path(AgentPath::root());
    (h, mail)
}

#[tokio::test(start_paused = true)]
async fn a_waking_message_retries_a_turn_lost_to_unreadable_replies_once_per_message() {
    let first = root_letter("m1", "/coder", true);
    let later = root_letter("m2", "/docs", true);
    let mut plan = vec![two_calls()];
    let mut loss = unreadable_loss();
    loss[0] = transient(ErrorKind::InvalidResponse).mail(first.clone());
    plan.extend(loss);
    plan.extend(unreadable_loss());
    plan.extend([done("back"), done("delivered")]);
    let (mut h, mail) = root_mailed("mail-restart-rounds", plan);

    let rounds = h.run(vec![steer("go")], 2).await;
    assert_eq!(retry_starts(&rounds), [(1, None, None), (2, Some(1), None)]);
    for outcome in outcomes(&rounds) {
        assert!(
            matches!(
                outcome,
                TurnOutcome::Error {
                    kind: ErrorKind::InvalidResponse,
                    gave_up: Some(GaveUp::StepLimit),
                    offer: Some(offer),
                    ..
                } if offer.retry && offer.retries_at.is_none()
            ),
            "{outcome:#?}"
        );
    }
    let bodies = h.provider.texts();
    assert_eq!(bodies.len(), 9);
    assert_eq!(
        bodies[5], bodies[4],
        "the restart resends the failed request"
    );

    let resting = settle_quietly(&mut h).await;
    assert_eq!(started(&resting), Vec::<u64>::new(), "{resting:#?}");
    let held_events: Vec<_> = wake_helds(&rounds)
        .into_iter()
        .chain(wake_helds(&resting))
        .collect();
    assert_eq!(
        held_events,
        [(vec![from_mail("m1", "/coder")], HoldReason::RetryOffer)]
    );
    assert_eq!(
        logged_helds(&records(&h.store)),
        [(2, HoldReason::RetryOffer)]
    );

    mail.send(later.clone()).unwrap();
    let woke = h.run(Vec::new(), 2).await;
    assert_eq!(
        retry_starts(&woke),
        [(3, Some(2), None), (4, None, Some(TurnOrigin::Mail))]
    );
    let bodies = h.provider.texts();
    assert_eq!(bodies.len(), 11);
    assert_eq!(bodies[9], bodies[8]);
    assert_bodies_extend(&h.provider, &[(9, 10)]);
    let requests = h.requests();
    assert_eq!(text_count(&requests[9], &shown(&first)), 0);
    assert_eq!(text_count(&requests[9], &shown(&later)), 0);
    ends_with(&last_texts(&h), &[shown(&first), shown(&later)]);
    assert!(h.agent.inbox.mail_ids().is_empty());
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn unrelated_stops_senders_own_letters_and_refused_retries_never_restart_on_mail() {
    let wake_coder = root_letter("m1", "/coder", true);
    let loss_then = |tail: Step| unreadable_loss().into_iter().chain([tail]).collect();

    let (mut h, mail) = root_mailed(
        "mail-restart-esc",
        vec![
            transient(ErrorKind::InvalidResponse),
            transient(ErrorKind::InvalidResponse),
            done("no"),
        ],
    );
    let stopped = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::Retrying { .. } => vec![ClientCommand::Cancel],
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(matches!(
        outcomes(&stopped)[..],
        [TurnOutcome::Error {
            kind: ErrorKind::InvalidResponse,
            gave_up: None,
            offer: Some(offer),
            ..
        }] if offer.retry
    ));
    mail.send(wake_coder.clone()).unwrap();
    let waiting = settle_quietly(&mut h).await;
    assert!(started(&waiting).is_empty(), "esc: {waiting:#?}");
    assert_eq!(
        wake_helds(&waiting),
        [(vec![from_mail("m1", "/coder")], HoldReason::RetryOffer)]
    );

    let (mut h, mail) = mailed("mail-restart-child", loss_then(done("no")));
    h.run(vec![steer("go")], 1).await;
    mail.send(letter("m1", CHILD, true)).unwrap();
    assert!(started(&settle_quietly(&mut h).await).is_empty());

    let (mut h, mail) = root_mailed(
        "mail-restart-server",
        vec![
            transient(ErrorKind::Server),
            transient(ErrorKind::Server),
            done("no"),
        ],
    );
    h.agent = h.agent.with_retry_policy(crate::agent::retry::RetryPolicy {
        step_retries: Some(1),
        ..crate::agent::retry::quick()
    });
    let failed = h.run(vec![steer("go")], 1).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error {
            kind: ErrorKind::Server,
            gave_up: Some(GaveUp::StepLimit),
            offer: Some(offer),
            ..
        }] if offer.retry
    ));
    mail.send(wake_coder.clone()).unwrap();
    assert!(started(&settle_quietly(&mut h).await).is_empty());

    let (gate, open) = watch::channel(true);
    let (mut h, mail) = mailed_with(
        "mail-restart-above",
        loss_then(done("answered")),
        Some(open),
    );
    h.agent = h.agent.with_path(AgentPath::root());
    h.run(vec![steer("go")], 1).await;
    gate.send_replace(false);
    mail.send(Mail {
        kind: MailKind::Question,
        ..root_letter("q1", "/coder", true)
    })
    .unwrap();
    assert!(started(&settle_quietly(&mut h).await).is_empty());
    assert!(h.agent.reset_retry_at().is_none());
    assert!(h.agent.retry_offer().is_some());
    gate.send_replace(true);
    let answered = h.run(Vec::new(), 1).await;
    assert_eq!(retry_starts(&answered), [(2, None, Some(TurnOrigin::Mail))]);

    let (mut h, mail) = root_mailed("mail-restart-notices", loss_then(done("no")));
    h.run(vec![steer("go")], 1).await;
    mail.send(Mail {
        kind: MailKind::Notice,
        ..root_letter("own", wisp_protocol::agents::ROOT_PATH, true)
    })
    .unwrap();
    mail.send(root_letter("quiet", "/coder", false)).unwrap();
    let (ev_tx, _ev_rx) = mpsc::channel(16);
    h.agent
        .push_letter(
            mid("plug"),
            MailFrom {
                agent: AgentPath::new("/coder"),
                kind: MailKind::Notice,
                mail: MailId("plug".into()),
                wake: true,
                plugin: Some("mcp".into()),
            },
            "notice".into(),
            Vec::new(),
            &ev_tx,
        )
        .await;
    assert!(started(&settle_quietly(&mut h).await).is_empty());
    assert!(h.agent.reset_retry_at().is_none());

    let (mut h, mail) = root_mailed("mail-restart-own-open", loss_then(done("no")));
    mail.send(wake_coder.clone()).unwrap();
    let failed = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&failed), [Some(TurnOrigin::Mail)]);
    assert!(started(&settle_quietly(&mut h).await).is_empty());
    h.agent.inbox.clear_returned();
    assert!(h.agent.reset_retry_at().is_none());

    let mut plan = vec![two_calls().mail(wake_coder.clone())];
    plan.extend(loss_then(done("no")));
    let (mut h, _mail) = root_mailed("mail-restart-own-steer", plan);
    let failed = h.run(vec![steer("go")], 1).await;
    assert_eq!(delivered(&failed)[1], (vec![mid("m1")], 2));
    assert!(started(&settle_quietly(&mut h).await).is_empty());
    h.agent.inbox.clear_returned();
    assert!(h.agent.reset_retry_at().is_none());

    let (mut h, mail) = root_mailed("mail-restart-refused", loss_then(done("no")));
    h.run(vec![steer("go")], 1).await;
    let original = h.agent.connections[0].provider.clone();
    let guarded = crate::endpoints::generation::Guarded::wrap(original.clone());
    guarded.generation().unwrap().change().unwrap().retire();
    h.agent.connections[0].provider = guarded;
    mail.send(wake_coder).unwrap();
    assert!(started(&settle_quietly(&mut h).await).is_empty());
    assert!(h.agent.retry_offer().is_some());
    assert!(h.agent.reset_retry_at().is_none());
    assert!(h.agent.mail_retried > 0);
    h.agent.connections[0].provider = original;
    assert!(started(&settle_quietly(&mut h).await).is_empty());
    assert_eq!(h.requests().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_resumed_unreadable_offer_waits_for_a_message_that_arrives_after_the_resume() {
    let before = root_letter("m1", "/coder", true);
    let after = root_letter("m2", "/docs", true);
    let mut plan = unreadable_loss();
    plan[0] = transient(ErrorKind::InvalidResponse).mail(before.clone());
    plan.extend(unreadable_loss());
    let (mut h, _mail) = root_mailed("mail-restart-resume-live", plan);
    h.run(vec![steer("go")], 2).await;
    let saved = records(&h.store);

    let (mut again, mail) = resumed_at(
        "mail-restart-resume-again",
        wisp_protocol::agents::ROOT_PATH,
        vec![done("back"), done("delivered")],
        &saved,
    );
    assert_eq!(again.agent.inbox.mail_ids(), [mid("m1")]);
    let (_, offer) = again.agent.retry_offer().expect("offer restored");
    assert!(offer.retry && offer.retries_at.is_none());

    let waiting = settle_quietly(&mut again).await;
    assert!(started(&waiting).is_empty(), "{waiting:#?}");
    assert_eq!(
        wake_helds(&waiting),
        [(vec![from_mail("m1", "/coder")], HoldReason::RetryOffer)]
    );

    mail.send(after.clone()).unwrap();
    let woke = again.run(Vec::new(), 2).await;
    assert_eq!(
        retry_starts(&woke),
        [(3, Some(2), None), (4, None, Some(TurnOrigin::Mail))]
    );
    assert_bodies_extend(&again.provider, &[(0, 1)]);
    assert_eq!(text_count(&again.requests()[0], &shown(&before)), 0);
    assert_eq!(text_count(&again.requests()[0], &shown(&after)), 0);
    ends_with(&last_texts(&again), &[shown(&before), shown(&after)]);
    assert!(again.agent.inbox.mail_ids().is_empty());
    again.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn a_malformed_call_step_limit_never_restarts_automatically_on_mail() {
    let malformed = || {
        let mut error =
            crate::agent::testing::failure(ErrorKind::InvalidResponse, RetryClass::Transient, true);
        error.detail.malformed_call = Some(Box::default());
        Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
    };
    let during = root_letter("m1", "/coder", true);
    let after = root_letter("m2", "/docs", true);
    let resumed_mail = root_letter("m3", "/review", true);

    let mut plan = vec![two_calls()];
    plan.push(malformed().mail(during.clone()));
    plan.extend((0..crate::agent::retry::INVALID_RESPONSE_RETRIES).map(|_| malformed()));
    plan.push(done("no"));
    let (mut h, mail) = root_mailed("mail-restart-malformed", plan);

    let failed = h.run(vec![steer("go")], 1).await;
    assert_eq!(retry_starts(&failed), [(1, None, None)]);
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error {
            kind: ErrorKind::InvalidResponse,
            gave_up: Some(GaveUp::StepLimit),
            offer: Some(offer),
            ..
        }] if offer.retry && offer.retries_at.is_none()
    ));
    let resting = settle_quietly(&mut h).await;
    assert!(started(&resting).is_empty(), "{resting:#?}");
    assert!(h.agent.reset_retry_at().is_none());
    assert_eq!(h.agent.mail_retried, 0);
    assert_eq!(h.requests().len(), 5);

    mail.send(after.clone()).unwrap();
    let still_waiting = settle_quietly(&mut h).await;
    assert!(started(&still_waiting).is_empty(), "{still_waiting:#?}");
    assert!(h.agent.reset_retry_at().is_none());
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1"), mid("m2")]);
    assert_eq!(h.requests().len(), 5);

    let saved = records(&h.store);
    let (mut again, mail) = resumed_at(
        "mail-restart-malformed-resumed",
        wisp_protocol::agents::ROOT_PATH,
        vec![done("no")],
        &saved,
    );
    assert!(again.agent.retry_offer().is_some());
    assert!(again.agent.reset_retry_at().is_none());
    mail.send(resumed_mail).unwrap();
    let after_resume = settle_quietly(&mut again).await;
    assert!(started(&after_resume).is_empty(), "{after_resume:#?}");
    assert!(again.agent.reset_retry_at().is_none());
    assert_eq!(
        again.agent.inbox.mail_ids(),
        [mid("m1"), mid("m2"), mid("m3")]
    );
    assert!(again.requests().is_empty());
}
