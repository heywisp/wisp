use wisp_protocol::log::{LogEvent, no_images, rebuild_history};

use super::*;
use crate::agent::tests::turn_started;
use crate::clock::TickingClock;
use crate::session::{MemoryStore, SessionLog, rfc3339};
use crate::test_guard::{kind_of, log_context, log_events, pick, records};

fn logged(agent: Agent, store: &MemoryStore) -> Agent {
    agent.with_clock(TickingClock::new()).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    )
}

fn logged_rig(provider: Arc<Fake>, store: &MemoryStore) -> Rig {
    Rig::of(logged(Agent::new(provider, default_session()), store))
}

fn two_endpoint_rig(provider: Arc<Fake>, store: &MemoryStore) -> Rig {
    let agent = logged(Agent::new(provider, default_session()), store);
    let first = agent.connections[0].clone();
    let mut second = first.clone();
    second.name = "other".into();
    Rig::of(agent.with_connections(vec![first, second], vec![], "env"))
}

fn marks(records: &[LogEvent]) -> Vec<(u64, String)> {
    pick!(records, LogEvent::RebuildMarked { turn, reason } => (*turn, reason.clone()))
}

fn first(records: &[LogEvent], wanted: impl Fn(&LogEvent) -> bool) -> usize {
    records.iter().position(wanted).unwrap()
}

fn is(name: &'static str) -> impl Fn(&LogEvent) -> bool {
    move |e| kind_of(e) == name
}

fn resumes_alike(rig: &Rig, store: &MemoryStore) {
    let read = records(store);
    let logged_records = log_events(store);
    let rebuilt = rebuild_history(logged_records.iter(), no_images).unwrap();
    let fresh = MemoryStore::default();
    let mut agent = logged(Agent::new(scripted(vec![]), default_session()), &fresh);
    let names: Vec<_> = rig
        .agent
        .connections
        .iter()
        .map(|c| c.name.clone())
        .collect();
    if names.len() > 1 {
        let first = agent.connections[0].clone();
        let connections = names
            .iter()
            .map(|name| {
                let mut c = first.clone();
                c.name.clone_from(name);
                c
            })
            .collect();
        agent = agent.with_connections(connections, vec![], "env");
    }
    let resumed = agent.resumed(rebuilt, &read);
    assert!(rig.entries().iter().all(|e| e.seq.is_some()));
    assert_eq!(
        resumed.epoch.entries(),
        rig.entries(),
        "{logged_records:#?}"
    );
    assert_eq!(resumed.epoch.marked_at(), rig.agent.epoch.marked_at());
}

#[derive(Debug)]
struct Step {
    turn: u64,
    view_len: Option<usize>,
    at: Option<String>,
    ttl: Option<CacheTtl>,
    prefix: Option<String>,
}

#[tokio::test]
async fn every_committed_step_logs_the_entry_it_adds_and_no_other_step_does() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        calls_tool(1_000),
        answer("done", 1_200),
        refused(ErrorKind::InvalidRequest, vec![]),
        empty_reply(1_300),
        answer("again", 1_400),
    ]);
    let mut rig = logged_rig(provider, &store);
    assert!(completed(&rig.turn("first").await));
    assert!(!completed(&rig.turn("fails").await));
    rig.turn("empty").await;
    assert!(completed(&rig.turn("works").await));
    let steps: Vec<Step> = pick!(
        log_events(&store),
        LogEvent::StepEnded {
            turn,
            view_len,
            started_at,
            ttl,
            prefix,
            ..
        } => Step {
            turn,
            view_len,
            at: started_at,
            ttl,
            prefix,
        }
    );
    type Logged = (usize, jiff::Timestamp, CacheTtl, Option<String>);
    let logged: Vec<Logged> = steps
        .iter()
        .filter_map(|s| {
            let at = s.at.as_deref()?.parse().unwrap();
            Some((s.view_len?, at, s.ttl?, s.prefix.clone()))
        })
        .collect();
    let live: Vec<Logged> = rig
        .entries()
        .iter()
        .map(|e| (e.view_len, e.started_at, e.ttl, e.prefix.clone()))
        .collect();
    assert_eq!(logged, live);
    assert_eq!(live.len(), 3);
    for (_, _, _, prefix) in &live {
        let prefix = prefix.as_deref().unwrap();
        assert_eq!(prefix.len(), 64, "{prefix}");
        assert!(prefix.bytes().all(|b| b.is_ascii_hexdigit()), "{prefix}");
    }
    for step in &steps {
        let commits = step.turn == 1 || step.turn == 4;
        assert_eq!(step.view_len.is_some(), commits, "{step:?}");
        assert_eq!(step.at.is_some(), commits, "{step:?}");
        assert_eq!(step.ttl.is_some(), commits, "{step:?}");
        assert_eq!(step.prefix.is_some(), commits, "{step:?}");
    }
    let at = steps[0].at.as_deref().unwrap();
    assert_eq!(at, rfc3339(rig.entries()[0].started_at));
    assert_eq!(at.len(), "2026-10-22T03:58:00.000Z".len());
    assert!(at.ends_with(".000Z"), "{at}");
    assert!(marks(&log_events(&store)).is_empty());
    resumes_alike(&rig, &store);
}

#[tokio::test]
async fn a_model_switch_logs_its_mark_before_the_turn_it_applies_to() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("hi", 1_000),
        refused(ErrorKind::InvalidRequest, vec![]),
        answer("there", 1_100),
    ]);
    let mut rig = logged_rig(provider, &store);
    rig.turn("first").await;
    rig.change(None, Some(OTHER_MODEL), None).await;
    assert!(marks(&log_events(&store)).is_empty());
    rig.turn("fails on the new model").await;
    let logged = log_events(&store);
    assert_eq!(marks(&logged), [(2, "model_switch".to_owned())]);
    let mark = first(&logged, is("rebuild_marked"));
    assert!(first(&logged, is("settings_changed")) < mark);
    assert!(mark < first(&logged, turn_started(2)));
    assert_eq!((rig.entries().len(), rig.mark()), (1, 1));
    resumes_alike(&rig, &store);
    rig.turn("works").await;
    assert_eq!(marks(&log_events(&store)).len(), 1);
    assert_eq!((rig.entries().len(), rig.mark()), (2, 1));
    resumes_alike(&rig, &store);
}

#[tokio::test]
async fn an_endpoint_switch_and_an_effort_change_log_their_marks() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("hi", 1_000),
        answer("there", 1_100),
        answer("again", 1_200),
    ]);
    let mut rig = two_endpoint_rig(provider, &store);
    rig.turn("first").await;
    rig.change(Some("other"), None, None).await;
    rig.turn("second").await;
    rig.change(None, None, Some(Effort::Low)).await;
    rig.turn("third").await;
    let logged = log_events(&store);
    assert_eq!(
        marks(&logged),
        [
            (2, "endpoint_switch".to_owned()),
            (3, "effort_change".to_owned())
        ]
    );
    let second = first(&logged, |e| {
        matches!(e, LogEvent::RebuildMarked { turn: 3, .. })
    });
    assert!(first(&logged, turn_started(2)) < second);
    assert!(second < first(&logged, turn_started(3)));
    assert_eq!((rig.entries().len(), rig.mark()), (3, 2));
    resumes_alike(&rig, &store);
}

#[tokio::test]
async fn a_per_message_effort_fallback_logs_its_mark_before_the_resent_request() {
    let store = MemoryStore::default();
    let provider = with_row(
        vec![
            answer("hi", 1_000),
            refused(ErrorKind::InvalidRequest, vec![Feature::PerMessageEffort]),
            answer("there", 1_100),
        ],
        Some(&PER_MESSAGE),
        false,
    );
    let mut rig = logged_rig(provider, &store);
    rig.turn("first").await;
    rig.change(None, None, Some(Effort::Low)).await;
    assert!(completed(&rig.turn("second").await));
    let logged = log_events(&store);
    assert_eq!(
        marks(&logged),
        [(2, "per_message_effort_rejected".to_owned())]
    );
    let mark = first(&logged, is("rebuild_marked"));
    assert!(first(&logged, turn_started(2)) < mark);
    assert!(first(&logged, is("per_message_effort")) < mark);
    let committed = logged
        .iter()
        .rposition(|e| {
            matches!(
                e,
                LogEvent::StepEnded {
                    view_len: Some(_),
                    ..
                }
            )
        })
        .unwrap();
    assert!(mark < committed);
    assert_eq!((rig.entries().len(), rig.mark()), (2, 1));
    resumes_alike(&rig, &store);
}

#[tokio::test]
async fn a_cache_ttl_change_and_a_context_count_log_no_mark() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("hi", 1_000), answer("there", 1_100)]);
    let mut rig = logged_rig(provider, &store);
    rig.turn("first").await;
    rig.agent.session.cache_ttl = CacheTtl::FiveMinutes;
    let (tx, mut rx) = mpsc::channel(64);
    rig.agent
        .context_command(ReportDetail::Summary, RequestId(9), &tx)
        .await;
    while tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .is_ok_and(|e| e.is_some())
    {}
    rig.turn("second").await;
    assert!(marks(&log_events(&store)).is_empty());
    assert_eq!((rig.entries().len(), rig.mark()), (2, 0));
    resumes_alike(&rig, &store);
}

#[tokio::test]
async fn a_kept_rendering_form_logs_its_mark_after_its_records() {
    let store = MemoryStore::default();
    let mut rig = logged_rig(scripted(vec![answer("hi", 1_000)]), &store);
    rig.turn("first").await;
    rig.agent.keep_form(&rig.events).await;
    assert!(marks(&log_events(&store)).is_empty());
    rig.agent.search.trial = Some(Trial {
        form: RenderingForm::WithoutThinking,
        cut: Some(Cut {
            from: wisp_protocol::log::BlockPosition {
                message: 1,
                block: 0,
            },
            through: 1,
            tools: false,
        }),
    });
    rig.agent.keep_form(&rig.events).await;
    let logged = log_events(&store);
    let kinds: Vec<String> = logged.iter().map(kind_of).collect();
    let tail = &kinds[kinds.len() - 3..];
    assert_eq!(tail, ["rendering_form", "thinking_cut", "rebuild_marked"]);
    assert_eq!(marks(&logged), [(2, "rendering_form".to_owned())]);
    assert_eq!(rig.mark(), 1);
    resumes_alike(&rig, &store);
}

fn open_search(rig: &mut Rig, thinking_after: bool) {
    let mut content = vec![server_call("s1")];
    if thinking_after {
        content.push(ContentBlock::Thinking {
            thinking: "t".into(),
            signature: "sig".into(),
        });
    }
    rig.agent.history = vec![
        Message::user_text("search"),
        Message {
            role: wisp_protocol::Role::Assistant,
            content,
        },
    ];
    rig.agent.sync_history();
}

#[tokio::test]
async fn closing_a_tail_logs_a_mark_only_when_it_moves_the_mark() {
    for (view_len, thinking, expected) in [
        (1, false, None),
        (2, false, Some("tail_closed")),
        (2, true, Some("thinking_cut")),
    ] {
        let store = MemoryStore::default();
        let mut rig = logged_rig(scripted(vec![]), &store);
        open_search(&mut rig, thinking);
        rig.agent
            .epoch
            .push(entry(view_len, CacheTtl::OneHour, at(0)));
        rig.agent
            .close_tail(UnfinishedReason::Failed, &BTreeSet::new(), &rig.events)
            .await;
        let logged = log_events(&store);
        let reasons: Vec<String> = marks(&logged).into_iter().map(|(_, r)| r).collect();
        assert_eq!(
            reasons,
            expected.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            "view_len {view_len}"
        );
        if expected.is_some() {
            assert_eq!(kind_of(logged.last().unwrap()), "rebuild_marked");
            assert!(first(&logged, is("tail_closed")) < logged.len() - 1);
        }
    }
}

#[tokio::test]
async fn a_tail_a_compaction_closes_logs_no_mark_and_the_compaction_drops_it() {
    let store = MemoryStore::default();
    let mut rig = logged_rig(scripted(vec![]), &store);
    open_search(&mut rig, false);
    rig.agent.epoch.push(entry(2, CacheTtl::OneHour, at(0)));
    rig.agent.close_compacted_tail(&rig.events).await;
    assert_eq!(rig.mark(), 1);
    assert_eq!(kind_of(log_events(&store).last().unwrap()), "tail_closed");
    assert!(marks(&log_events(&store)).is_empty());
    rig.agent.epoch.clear();
    rig.agent.log_mark(&rig.events).await;
    assert!(marks(&log_events(&store)).is_empty());
}
