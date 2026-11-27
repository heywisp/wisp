use jiff::Timestamp;
use jiff::tz::TimeZone;
use wisp_protocol::log::{LogRecord, ToolContext, ToolsCopy, TurnLead};

use super::*;
use crate::clock::ShiftedClock;
use crate::session::{rebuild_checked, sha256_hex};
use crate::test_guard::RED_PIXEL as RED;

pub(super) fn shifted() -> Arc<ShiftedClock> {
    Arc::new(ShiftedClock::at(
        "2026-10-25T09:00:00Z".parse().unwrap(),
        Some(TimeZone::UTC),
    ))
}

pub(super) fn agent_on(
    provider: Arc<Fake>,
    store: &MemoryStore,
    clock: Arc<ShiftedClock>,
) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(clock)
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        )
}

fn reopened(
    provider: Arc<Fake>,
    session: SessionConfig,
    records: &[LogRecord],
    clock: Arc<ShiftedClock>,
    context: LogContext,
) -> Result<Agent, String> {
    let (_, log) = continued(records);
    Agent::new(provider, session)
        .with_clock(clock)
        .with_log(log, context)
        .resume(records, 0)
}

fn past_ttl(clock: &ShiftedClock, records: &[LogRecord]) {
    let last: Timestamp = records.last().unwrap().at.parse().unwrap();
    let base: Timestamp = "2026-10-25T09:00:00Z".parse().unwrap();
    let behind = last.duration_since(base).as_mins().max(0);
    clock.advance(jiff::SignedDuration::from_mins(behind + 120));
}

fn resumed(provider: Arc<Fake>, records: &[LogRecord], clock: Arc<ShiftedClock>) -> Agent {
    reopened(provider, default_session(), records, clock, log_context()).unwrap()
}

async fn two_turns(clock: &Arc<ShiftedClock>, extra: Vec<Step>) -> (Live, Arc<Fake>, MemoryStore) {
    let store = MemoryStore::default();
    let mut steps = vec![answer("Hello.", 1_000), answer("Again.", 1_200)];
    steps.extend(extra);
    let provider = scripted(steps);
    let mut live = Live::start(agent_on(provider.clone(), &store, clock.clone()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    (live, provider, store)
}

#[tokio::test]
async fn after_the_cache_expired_a_resumed_session_sends_what_the_live_one_would_have() {
    let clock = shifted();
    let (mut live, sent_live, store) = two_turns(&clock, vec![answer("Third.", 1_400)]).await;
    let records = store.read("s1").unwrap();
    past_ttl(&clock, &records);
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;

    let sent_resumed = scripted(vec![answer("Third.", 1_400)]);
    let agent = resumed(sent_resumed.clone(), &records, clock.clone());
    let notice = agent.resume_notice_text().unwrap_or_default().to_owned();
    assert!(
        notice.starts_with("Resumed session s1 (2 turns, $"),
        "{notice}"
    );
    assert!(
        notice.contains(" h ago). The cache has likely expired; your next message rewrites about "),
        "{notice}"
    );
    assert!(notice.ends_with(" tokens (~$0.00992)."), "{notice}");
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u3", "third").await));
    again.close().await;
    assert_eq!(sent_resumed.body(0), sent_live.body(2));
    assert_eq!(sent_resumed.beta(0), sent_live.beta(2));
}

#[tokio::test]
async fn within_the_ttl_a_resumed_session_sends_the_same_bytes() {
    let clock = shifted();
    let (mut live, sent_live, store) = two_turns(&clock, vec![answer("Third.", 1_400)]).await;
    let records = store.read("s1").unwrap();
    clock.advance(jiff::SignedDuration::from_mins(5));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;

    let sent_resumed = scripted(vec![answer("Third.", 1_400)]);
    let agent = resumed(sent_resumed.clone(), &records, clock.clone());
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u3", "third").await));
    again.close().await;
    assert_eq!(
        sent_resumed.body(0),
        sent_live.body(2),
        "ordinary requests carry no tail marker, so even a warm cache sees the same bytes"
    );
}

#[tokio::test]
async fn the_resumed_log_continues_with_session_resumed_and_rebuilds_the_same_history() {
    let clock = shifted();
    let (live, _, store) = two_turns(&clock, Vec::new()).await;
    live.close().await;
    let records = store.read("s1").unwrap();
    let provider = scripted(vec![answer("Third.", 1_400)]);
    let (copy, log) = continued(&records);
    let agent = Agent::new(provider, default_session())
        .with_clock(clock.clone())
        .with_log(log, log_context())
        .resume(&records, 7)
        .unwrap();
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;

    let all = copy.read("s1").unwrap();
    let first = &all[records.len()];
    assert_eq!(first.seq, u64::try_from(records.len()).unwrap());
    let LogEvent::SessionResumed {
        endpoint,
        endpoint_name,
        system_prompt_sha256,
        tools,
        tools_sha256,
        truncated_bytes,
        ..
    } = &first.event
    else {
        panic!("{first:?}")
    };
    assert_eq!(endpoint, "https://api.anthropic.com");
    assert_eq!(endpoint_name, "env");
    assert_eq!(
        system_prompt_sha256.as_deref(),
        agent.session.system.as_deref().map(sha256_hex).as_deref()
    );
    assert!(tools.is_empty());
    assert_eq!(*tools_sha256, None);
    assert_eq!(*truncated_bytes, 7);
    assert!(
        all.iter()
            .filter(|r| matches!(r.event, LogEvent::SessionStarted { .. }))
            .count()
            == 1
    );
    let rebuilt = rebuild_checked(&copy, &all, None).unwrap().unwrap();
    assert_eq!(rebuilt.messages, &*agent.history());
    let logged = crate::resume::LoggedSession::read(&all).unwrap();
    assert_eq!(logged.turns, 3);
}

#[tokio::test]
async fn a_compaction_after_resume_matches_the_live_one() {
    let clock = shifted();
    let after = vec![compaction(SUMMARY), answer("Third.", 900)];
    let (mut live, sent_live, store) = two_turns(&clock, after).await;
    let records = store.read("s1").unwrap();
    past_ttl(&clock, &records);
    let done = live.compact(1, Some("the greeting")).await;
    assert!(matches!(
        done,
        AgentEvent::Compacted { .. } | AgentEvent::CompactionFailed { .. }
    ));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;

    let sent_resumed = scripted(vec![compaction(SUMMARY), answer("Third.", 900)]);
    let mut again = Live::start(resumed(sent_resumed.clone(), &records, clock.clone()));
    let done = again.compact(1, Some("the greeting")).await;
    assert!(matches!(
        done,
        AgentEvent::Compacted { .. } | AgentEvent::CompactionFailed { .. }
    ));
    assert!(completed(&again.turn("u3", "third").await));
    again.close().await;
    assert_eq!(sent_resumed.sent().len(), 2);
    assert_eq!(sent_resumed.body(0), sent_live.body(2));
    assert_eq!(sent_resumed.beta(0), sent_live.beta(2));
    assert_eq!(sent_resumed.body(1), sent_live.body(3));
}

#[tokio::test]
async fn a_session_resumed_after_a_compaction_continues_from_its_head() {
    for threads in [false, true] {
        let clock = shifted();
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            compaction(SUMMARY),
            answer("Again.", 300),
            answer("Third.", 500),
        ])
        .view(|k| {
            k.threads = threads;
            k.wire_print = threads;
        });
        let mut live = Live::start(agent_on(provider.clone(), &store, clock.clone()));
        assert!(completed(&live.turn("u1", "first").await));
        assert!(matches!(
            live.compact(2, None).await,
            AgentEvent::Compacted { .. }
        ));
        let compacted = store.read("s1").unwrap();
        assert!(completed(&live.turn("u2", "second").await));
        let records = store.read("s1").unwrap();
        past_ttl(&clock, &records);
        assert!(completed(&live.turn("u3", "third").await));
        let (live_agent, _) = live.close().await;

        let early = scripted(vec![answer("Again.", 300)]).view(|k| {
            k.threads = threads;
            k.wire_print = threads;
        });
        let mut again = Live::start(resumed(early.clone(), &compacted, shifted()));
        assert!(completed(&again.turn("u2", "second").await));
        again.close().await;
        assert_eq!(early.body(0), provider.body(2));
        assert_eq!(early.beta(0), provider.beta(2));
        assert_eq!(
            early.sent()[0].features.thread,
            threads.then_some(Thread::Create)
        );
        assert_eq!(early.beta(0).contains(COMPACTION_BETA), !threads);

        let late = scripted(vec![answer("Third.", 500)]).view(|k| {
            k.threads = threads;
            k.wire_print = threads;
        });
        let agent = resumed(late.clone(), &records, clock.clone());
        assert_eq!(agent.compaction.head, live_agent.compaction.head);
        assert_eq!(agent.compaction.compactions, 1);
        assert_eq!(agent.compaction.since_seq, live_agent.compaction.since_seq);
        let mut again = Live::start(agent);
        assert!(completed(&again.turn("u3", "third").await));
        again.close().await;
        assert_eq!(late.body(0), provider.body(3));
        assert_eq!(late.beta(0), provider.beta(3));
    }
}

fn with_open_turn(records: &[LogRecord]) -> Vec<LogRecord> {
    let mut records = records.to_vec();
    let seq = u64::try_from(records.len()).unwrap();
    records.push(LogRecord {
        schema_version: wisp_protocol::log::LOG_SCHEMA_VERSION,
        seq,
        at: "2026-10-25T09:30:00Z".into(),
        event: LogEvent::TurnStarted {
            ends: Vec::new(),
            turn: 3,
            origin: None,
            lead: TurnLead::default(),
            text: "lost".into(),
            model: None,
            effort: None,
            effort_marker: None,
            images: Vec::new(),
            endpoint_name: None,
            input_id: None,
            inputs: Vec::new(),
            cache_ttl: None,
            debug_cache: None,
            refusal_fallback: None,
            binding_controls: None,
            retry_of: None,
            kept_tools_sha256: None,
            tool_message: Vec::new(),
        },
    });
    records
}

#[tokio::test]
async fn resume_recovers_from_a_changed_prompt_origin_or_tool_set() {
    let clock = shifted();
    let (live, _, store) = two_turns(&clock, Vec::new()).await;
    live.close().await;
    let records = store.read("s1").unwrap();
    let attempt = |session: SessionConfig, records: &[LogRecord], context: LogContext| {
        reopened(
            scripted(Vec::new()),
            session,
            records,
            clock.clone(),
            context,
        )
        .unwrap()
    };
    let changed = " Wisp's prompt or tools changed since this session: the cache is rebuilt";
    let dropped = " and the model's earlier reasoning is dropped; the conversation itself is kept.";

    let repaired = attempt(default_session(), &with_open_turn(&records), log_context());
    assert!(!repaired.settings.binding_controls);
    let mut other_prompt = default_session();
    other_prompt.system = Some("A different prompt.".into());
    let prompt = attempt(other_prompt, &records, log_context());
    let notice = prompt.resume_notice_text().unwrap();
    assert!(
        notice.starts_with("Resumed session s1 (2 turns, $"),
        "{notice}"
    );
    assert!(
        notice.contains(changed) && notice.ends_with(dropped),
        "{notice}"
    );
    assert!(
        prompt.settings.binding_controls,
        "drop_block on every request"
    );

    let moved = LogContext {
        endpoint: "https://gateway.example".into(),
        ..log_context()
    };
    let origin = attempt(default_session(), &records, moved);
    let notice = origin.resume_notice_text().unwrap();
    assert!(
        notice.ends_with(
            " The keys-file account now points at https://gateway.example, not https://api.anthropic.com as when this session ran; Wisp continues there."
        ),
        "{notice}"
    );
    assert!(notice.contains("The cache has likely expired"), "{notice}");
    assert!(origin.settings.binding_controls);

    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    let with_tools = Agent::new(scripted(Vec::new()), default_session())
        .with_clock(clock.clone())
        .with_tools(crate::tool::test_toolbox(), workspace)
        .with_log(
            SessionLog::continuing(Box::new(stored(&records)), "s1", 9),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let notice = with_tools.resume_notice_text().unwrap();
    assert!(notice.contains(changed), "{notice}");
    assert!(with_tools.settings.binding_controls);
}

fn release_tools(ask_user: bool) -> crate::tool::Toolbox {
    let on: &[&str] = if ask_user {
        crate::tool::testkit::ALL
    } else {
        &["bash", "show_diff", "view_file"]
    };
    crate::tool::testkit::bundled(on).with_web_search(true)
}

#[tokio::test]
async fn a_log_from_before_ask_user_was_on_rebuilds_once_unless_it_is_turned_off() {
    let clock = shifted();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), answer("Again.", 1_200)]);
    let before = Agent::new(provider, default_session())
        .with_clock(clock.clone())
        .with_tools(release_tools(false), temp_workspace())
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    let mut live = Live::start(before);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let records = store.read("s1").unwrap();
    let changed = "Wisp's prompt or tools changed since this session";
    let resume_with = |ask_user: bool| {
        Agent::new(scripted(Vec::new()), default_session())
            .with_clock(clock.clone())
            .with_tools(release_tools(ask_user), temp_workspace())
            .with_log(continued(&records).1, log_context())
            .resume(&records, 0)
            .unwrap()
    };

    let on = resume_with(true);
    let notice = on.resume_notice_text().unwrap();
    assert_eq!(notice.matches(changed).count(), 1, "{notice}");
    assert!(on.settings.binding_controls);

    let off = resume_with(false);
    let notice = off.resume_notice_text().unwrap_or_default();
    assert!(!notice.contains(changed), "{notice}");
    assert!(!off.settings.binding_controls);
}

#[tokio::test]
async fn a_resume_that_rebuilds_logs_its_whole_prefix_and_one_that_does_not_logs_none() {
    let clock = shifted();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let before = Agent::new(provider, default_session())
        .with_clock(clock.clone())
        .with_tools(release_tools(false), temp_workspace())
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    let mut live = Live::start(before);
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let records = store.read("s1").unwrap();
    let context = ToolContext {
        platform: "unix".into(),
        capabilities: vec!["shell".into()],
    };
    let resume_with = |ask_user: bool| {
        Agent::new(scripted(Vec::new()), default_session())
            .with_clock(clock.clone())
            .with_tools(release_tools(ask_user), temp_workspace())
            .with_tool_context(context.clone())
            .with_log(continued(&records).1, log_context())
            .resume(&records, 0)
            .unwrap()
    };

    let mut rebuilt = resume_with(true);
    let canonical = rebuilt.toolbox().canonical_json();
    let system = rebuilt.session().system.clone();
    let Some(LogEvent::SessionResumed {
        system_prompt,
        whole:
            ToolsCopy {
                tool_definitions,
                tool_context,
                history_len,
                ..
            },
        ..
    }) = rebuilt.resumed_record()
    else {
        panic!("no session_resumed");
    };
    assert!(system.is_some());
    assert_eq!(system_prompt, system);
    assert!(!tool_definitions.is_empty());
    assert_eq!(serde_json::to_string(&tool_definitions).unwrap(), canonical);
    assert_eq!(tool_context, Some(Box::new(context.clone())));
    assert_eq!(history_len, rebuilt.history().len());
    assert_ne!(history_len, 0);

    let mut kept = resume_with(false);
    let Some(LogEvent::SessionResumed {
        system_prompt,
        whole:
            ToolsCopy {
                tool_definitions,
                tool_context,
                history_len,
                ..
            },
        ..
    }) = kept.resumed_record()
    else {
        panic!("no session_resumed");
    };
    assert_eq!(system_prompt, None);
    assert!(tool_definitions.is_empty());
    assert_eq!(tool_context, None);
    assert_eq!(history_len, 0);
}

#[tokio::test]
async fn a_resume_whose_logged_endpoint_is_gone_still_logs_its_whole_prefix_when_tools_changed() {
    let clock = shifted();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let before = Agent::new(provider, default_session())
        .with_clock(clock.clone())
        .with_tools(release_tools(false), temp_workspace())
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    let mut live = Live::start(before);
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let records: Vec<LogRecord> = store
        .read("s1")
        .unwrap()
        .iter()
        .map(|r| {
            let line = serde_json::to_string(r).unwrap();
            let gone = line.replace(r#""endpoint_name":"env""#, r#""endpoint_name":"gone""#);
            serde_json::from_str(&gone).unwrap()
        })
        .collect();
    let mut resumed = Agent::new(scripted(Vec::new()), default_session())
        .with_clock(clock.clone())
        .with_tools(release_tools(true), temp_workspace())
        .with_log(continued(&records).1, log_context())
        .resume(&records, 0)
        .unwrap();
    let canonical = resumed.toolbox().canonical_json();
    let Some(LogEvent::SessionResumed {
        whole: ToolsCopy {
            tool_definitions, ..
        },
        ..
    }) = resumed.resumed_record()
    else {
        panic!("no session_resumed");
    };
    assert_eq!(serde_json::to_string(&tool_definitions).unwrap(), canonical);
}

#[tokio::test]
async fn a_log_from_before_step_ended_resumes_without_an_estimate() {
    let clock = shifted();
    let (live, _, store) = two_turns(&clock, Vec::new()).await;
    live.close().await;
    let records: Vec<LogRecord> = super::processes::renumbered(
        store
            .read("s1")
            .unwrap()
            .into_iter()
            .filter(|r| !matches!(r.event, LogEvent::StepEnded { .. }))
            .collect(),
    );
    let agent = resumed(scripted(Vec::new()), &records, clock.clone());
    assert!(agent.last.is_none());
    assert_eq!(
        agent.resume_notice_text(),
        Some(
            "Resumed session s1 (2 turns, $0.0104, last active just now). The cache has likely expired; your next message rewrites it."
        )
    );
}

#[tokio::test]
async fn a_log_without_endpoint_names_keeps_its_last_request_on_the_agents_endpoint() {
    let clock = shifted();
    let (live, _, store) = two_turns(&clock, Vec::new()).await;
    live.close().await;
    let records: Vec<LogRecord> = store
        .read("s1")
        .unwrap()
        .iter()
        .map(|record| {
            let mut line = serde_json::to_value(record).unwrap();
            line.as_object_mut().unwrap().remove("endpoint_name");
            serde_json::from_value(line).unwrap()
        })
        .collect();
    let logged = crate::resume::LoggedSession::read(&records).unwrap();
    let step = logged.last_step.unwrap();
    assert_eq!(step.endpoint_name, None);
    let provider = scripted(Vec::new());
    let agent = two_endpoints_on(
        Agent::new(provider.clone(), default_session()),
        provider,
        "other",
    )
    .with_clock(clock.clone())
    .with_log(continued(&records).1, log_context())
    .resume(&records, 0)
    .unwrap();
    let last = agent.last.as_ref().unwrap();
    assert_eq!((last.endpoint, agent.endpoint), (1, 1));
    assert_eq!((last.turn, last.prompt_tokens), (2, step.prompt_tokens));
}

#[tokio::test]
async fn within_the_ttl_the_notice_only_names_the_session() {
    let clock = shifted();
    let (live, _, store) = two_turns(&clock, Vec::new()).await;
    live.close().await;
    let records = store.read("s1").unwrap();
    let sent: Timestamp = records
        .iter()
        .rev()
        .find_map(|r| match &r.event {
            LogEvent::StepEnded { started_at, .. } => started_at.clone(),
            _ => None,
        })
        .unwrap()
        .parse()
        .unwrap();
    let base: Timestamp = "2026-10-25T09:00:00Z".parse().unwrap();
    clock.advance(jiff::SignedDuration::from_mins(
        sent.duration_since(base).as_mins() + 59,
    ));
    let agent = resumed(scripted(Vec::new()), &records, clock.clone());
    assert_eq!(
        agent.resume_notice_text(),
        Some("Resumed session s1 (2 turns, $0.0104, last active just now).")
    );
    clock.advance(jiff::SignedDuration::from_mins(2));
    let cold = resumed(scripted(Vec::new()), &records, clock.clone());
    assert_ne!(
        cold.resume_notice_text(),
        Some("Resumed session s1 (2 turns, $0.0104).")
    );
    assert!(agent.last.is_some());
    assert_eq!(agent.turns, 2);
    assert!(agent.instructions_announced);
}

#[tokio::test]
async fn the_cached_share_after_resume_matches_the_live_one() {
    let clock = shifted();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer_read("Hello.", 100, 900),
        answer_read("Again.", 50, 950),
    ]);
    let mut live = Live::start(agent_on(provider, &store, clock.clone()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let records = store.read("s1").unwrap();
    let back = resumed(scripted(Vec::new()), &records, clock.clone());
    assert_eq!(agent.cached(), Some(95));
    assert_eq!(back.cached(), agent.cached());
}

#[tokio::test]
async fn the_fill_report_after_resume_matches_the_live_one() {
    let clock = shifted();
    let (live, _, store) = two_turns(&clock, Vec::new()).await;
    let (live_agent, _) = live.close().await;
    let records = store.read("s1").unwrap();
    let mut agent = resumed(scripted(Vec::new()), &records, clock.clone());
    let (_commands, mut receiver) = mpsc::channel(1);
    let (events, _seen) = mpsc::channel(64);
    agent.refresh_sections_idle(&mut receiver, &events).await;
    assert_eq!(
        agent.report(ReportDetail::All, RequestId(1)),
        live_agent.report(ReportDetail::All, RequestId(1))
    );
    assert_eq!(agent.fill_estimate(), live_agent.fill_estimate());
    assert_eq!(agent.session_usd, live_agent.session_usd);
    assert_eq!(agent.completed_on, live_agent.completed_on);
    assert_eq!(agent.history(), live_agent.history());
}

#[tokio::test]
async fn images_of_kept_turns_come_back_from_the_store_or_become_notes() {
    let clock = shifted();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("A red pixel.", 1_000)]);
    let mut live = Live::start(agent_on(provider, &store, clock.clone()));
    live.send(ClientCommand::Send {
        id: InputId("u1".into()),
        text: "what is this?".into(),
        images: vec![wisp_protocol::ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (live_agent, _) = live.close().await;
    let records = store.read("s1").unwrap();
    let image = records
        .iter()
        .find_map(|r| match &r.event {
            LogEvent::TurnStarted { images, .. } => images.first().cloned(),
            _ => None,
        })
        .unwrap();

    let missing = reopened(
        scripted(Vec::new()),
        default_session(),
        &records,
        clock.clone(),
        log_context(),
    )
    .unwrap();
    let messages = missing.history();
    let notes: Vec<&str> = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text { text } if text.starts_with('[') => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(notes, ["[image no longer available: image/png, 1 KB]"]);
    assert_ne!(missing.history(), live_agent.history());

    let (mut copy, log) = continued(&records);
    let blob = store
        .get_blob(&image.sha256, &image.media_type.into())
        .unwrap();
    copy.put_blob(&image.sha256, &blob.media_type, &blob.bytes)
        .unwrap();
    let agent = Agent::new(scripted(Vec::new()), default_session())
        .with_clock(clock.clone())
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap();
    assert_eq!(agent.history(), live_agent.history());
}

fn appended(records: &[LogRecord], events: Vec<Value>) -> Vec<LogRecord> {
    let mut records = records.to_vec();
    for event in events {
        let mut line = json!({
            "schema_version": wisp_protocol::log::LOG_SCHEMA_VERSION,
            "seq": records.len(),
            "at": records.last().unwrap().at.clone(),
        });
        for (key, value) in event.as_object().unwrap() {
            line[key] = value.clone();
        }
        records.push(serde_json::from_value(line).unwrap());
    }
    records
}

fn with_echo(clock: &Arc<ShiftedClock>) -> Agent {
    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    Agent::new(scripted(Vec::new()), default_session())
        .with_clock(clock.clone())
        .with_tools(crate::tool::test_toolbox(), workspace)
}

#[tokio::test]
async fn tool_state_and_feature_states_carry_over() {
    let clock = shifted();
    let (live, _, store) = two_turns(&clock, Vec::new()).await;
    live.close().await;
    let mut records = store.read("s1").unwrap();
    let probe = with_echo(&clock);
    let tools = probe.current_tools();
    let LogEvent::SessionStarted {
        system_prompt_sha256,
        tools: names,
        tools_sha256,
        ..
    } = &mut records[0].event
    else {
        panic!("{:?}", records[0])
    };
    *system_prompt_sha256 = probe.session.system.as_deref().map(sha256_hex);
    names.clone_from(&tools.names);
    tools_sha256.clone_from(&tools.sha256);
    let records = appended(
        &records,
        vec![
            json!({
                "type": "tool_call_requested",
                "turn": 2, "step": 1, "block": 0, "call_id": "toolu_1", "tool": "echo",
                "input": {}, "display": {"action": {"type": "run"}, "target": "x", "output": {"type": "text"}},
            }),
            json!({"type": "tool_call_started", "turn": 2, "call_id": "toolu_1", "cwd": "/tmp"}),
            json!({
                "type": "tool_call_finished",
                "turn": 2,
                "call_id": "toolu_1",
                "outcome": {"status": "completed", "is_error": false, "exit": {"type": "code", "code": 0}},
                "output": {"tail": "", "bytes": 0, "lines": 0, "file": null, "file_problem": null, "inlined": true},
                "facts": [],
                "state": {"type": "cwd", "path": "/tmp/elsewhere"},
                "duration_ms": 3,
            }),
            json!({"type": "per_message_effort", "model": DEFAULT_MODEL, "state": "fallen_back", "endpoint_name": "env"}),
            json!({"type": "native_compaction", "model": DEFAULT_MODEL, "state": "proven", "endpoint_name": "gone"}),
        ],
    );
    let agent = with_echo(&clock)
        .with_log(continued(&records).1, log_context())
        .resume(&records, 0)
        .unwrap();
    assert_eq!(agent.outputs, 1);
    assert_eq!(
        agent.tools.as_ref().and_then(|t| t.cwd.clone()),
        Some(std::path::PathBuf::from("/tmp/elsewhere"))
    );
    assert_eq!(
        agent.pair_state("env", DEFAULT_MODEL, Feature::PerMessageEffort),
        crate::agent::PairState::FallenBack
    );
    assert_eq!(
        agent.pair_state("gone", DEFAULT_MODEL, Feature::NativeCompaction),
        crate::agent::PairState::Unknown,
        "a state is kept only for an endpoint that still points where it did"
    );
    assert!(!agent.settings.binding_controls);
}

#[tokio::test]
async fn a_feature_state_is_kept_only_while_its_endpoint_points_where_it_did() {
    let clock = shifted();
    let (live, provider, store) = two_turns(&clock, Vec::new()).await;
    live.close().await;
    let records = appended(
        &store.read("s1").unwrap(),
        vec![json!({
            "type": "per_message_effort",
            "model": DEFAULT_MODEL,
            "state": "fallen_back",
            "endpoint_name": "env",
        })],
    );
    let resumed_on_other = |env_origin: &str| {
        let connection = |name: &str, origin: &str| Connection {
            name: name.into(),
            provider: provider.clone(),
            context: LogContext {
                endpoint: origin.into(),
                endpoint_name: name.into(),
                ..log_context()
            },
            priced: true,
        };
        Agent::new(provider.clone(), default_session())
            .with_clock(clock.clone())
            .with_connections(
                vec![
                    connection("env", env_origin),
                    connection("other", "https://api.anthropic.com"),
                ],
                vec![],
                "other",
            )
            .with_log(continued(&records).1, log_context())
            .resume(&records, 0)
            .unwrap()
    };
    assert_eq!(
        resumed_on_other("https://api.anthropic.com").pair_state(
            "env",
            DEFAULT_MODEL,
            Feature::PerMessageEffort
        ),
        crate::agent::PairState::FallenBack
    );
    assert_eq!(
        resumed_on_other("https://gateway.example").pair_state(
            "env",
            DEFAULT_MODEL,
            Feature::PerMessageEffort
        ),
        crate::agent::PairState::Unknown,
        "the endpoint's name now points somewhere else"
    );
}

#[test]
fn rendering_resumes_after_the_last_run_of_messages_on_its_endpoint() {
    let tags = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let run_start = crate::agent::resume::run_start;
    assert_eq!(run_start(&tags(&["a", "a", "b", "b"]), "b"), 2);
    assert_eq!(run_start(&tags(&["b", "a", "b", "b", "a"]), "b"), 2);
    assert_eq!(run_start(&tags(&["b", "b"]), "b"), 0);
    assert_eq!(run_start(&tags(&["a", "a"]), "b"), 0);
}

#[tokio::test]
async fn a_changed_prompt_is_announced_on_the_first_resume_only() {
    let clock = shifted();
    let (live, _, store) = two_turns(&clock, Vec::new()).await;
    live.close().await;
    let records = store.read("s1").unwrap();
    let changed = "Wisp's prompt or tools changed since this session";
    let other = || {
        let mut session = default_session();
        session.system = Some("A different prompt.".into());
        session
    };
    let mut first = reopened(
        scripted(Vec::new()),
        other(),
        &records,
        clock.clone(),
        log_context(),
    )
    .unwrap();
    assert!(first.resume_notice_text().unwrap().contains(changed));
    let mut again = records.clone();
    again.push(LogRecord {
        schema_version: wisp_protocol::log::LOG_SCHEMA_VERSION,
        seq: u64::try_from(records.len()).unwrap(),
        at: "2026-10-25T09:30:00Z".into(),
        event: first.resumed_record().unwrap(),
    });
    let second = reopened(
        scripted(Vec::new()),
        other(),
        &again,
        clock.clone(),
        log_context(),
    )
    .unwrap();
    let notice = second.resume_notice_text().unwrap();
    assert!(!notice.contains(changed), "{notice}");
    assert!(
        second.settings.binding_controls,
        "the first resume's drop_block holds"
    );
    let back = reopened(
        scripted(Vec::new()),
        default_session(),
        &again,
        clock.clone(),
        log_context(),
    )
    .unwrap();
    assert!(back.resume_notice_text().unwrap().contains(changed));
}
