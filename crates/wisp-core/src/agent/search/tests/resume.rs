use super::kept_tools::{compact_now, kept_on, server_down};
use super::*;
use crate::clock::StillClock;
use crate::context::ModelSettings;
use crate::models::list::tests::{claude_entry, claude_wire};
use crate::models::list::{ModelList, anthropic};
use crate::models::{ConfigModels, ModelTable};
use crate::provider::anthropic::capabilities;
use crate::test_guard;

fn web_models() -> ModelSettings {
    ModelSettings::new(
        ModelTable::shipped().unwrap(),
        ConfigModels::parse(
            "[endpoints.env.models.\"claude-opus-5-5\"]\nweb_search = [\"web_search_20250305\"]\n\n[endpoints.env.models.\"claude-haiku-5-5\"]\nweb_search = []\n",
        )
        .unwrap(),
    )
}

fn base(provider: Arc<Fake>, env: &ScratchDir, web: bool) -> Agent {
    let agent = Agent::new(provider, default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(
            Toolbox::new(samples().into_iter().map(|s| s.tool)),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300));
    if web {
        agent
            .with_model_settings(web_models())
            .with_web_search(true)
    } else {
        agent
    }
}

fn live_rig(steps: Vec<Step>, web: bool) -> Rig {
    let env = env("resume-live");
    let (tx, rx) = mpsc::channel(64);
    let provider = searching(steps, tx);
    let store = MemoryStore::default();
    let agent = base(provider.clone(), &env, web).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    Rig {
        agent,
        provider,
        rx,
        store,
        _env: env,
    }
}

fn resumed(live: &Rig, records: &[LogRecord], steps: Vec<Step>, web: bool) -> Result<Rig, String> {
    let (tx, rx) = mpsc::channel(64);
    let provider = searching(steps, tx);
    let (store, log) = continued(records);
    let agent = base(provider.clone(), &live._env, web)
        .with_log(log, log_context())
        .resume(records, 0)?;
    Ok(Rig {
        agent,
        provider,
        rx,
        store,
        _env: env("resume-again"),
    })
}

fn plain_done() -> Step {
    done(&[B::Think("t"), B::Text("fine")])
}

async fn live_until_resume(live_steps: Vec<Step>, turns: &[&str], web: bool, end: bool) -> Rig {
    let mut steps = live_steps;
    steps.push(plain_done());
    let mut live = live_rig(steps, web);
    for text in turns {
        live.turn(text).await;
    }
    if end {
        live.end().await;
    }
    live
}

fn listed(entries: &[serde_json::Value]) -> Option<Arc<ModelList>> {
    Some(Arc::new(anthropic::parse(&claude_wire(entries)).unwrap()))
}

fn claude(provider: &Fake, list: Option<Arc<ModelList>>) {
    provider.knobs(|k| {
        k.caps = Some(Caps::Fn(capabilities::row));
        k.list = list;
    });
}

async fn same_next_request(live_steps: Vec<Step>, turns: &[&str], web: bool) {
    let mut live = live_until_resume(live_steps, turns, web, true).await;
    let records = records(&live.store);
    let before = live.sent();
    let model = live.agent.session.model.clone();
    let mut changed = claude_entry(&model, "opus", 1_000_000);
    changed["max_tokens"] = json!(200_000);
    changed["capabilities"]["effort"]["max"] = json!({ "supported": false });
    let added = claude_entry("claude-opus-6", "opus", 1_000_000);
    claude(
        &live.provider,
        listed(&[claude_entry(&model, "opus", 1_000_000)]),
    );
    live.turn("next").await;
    let mut again = resumed(&live, &records, vec![plain_done()], web).unwrap();
    claude(&again.provider, listed(&[added, changed]));
    let facts = |rig: &Rig| rig.provider.capabilities(&model).unwrap();
    let (was, now) = (facts(&live), facts(&again));
    assert_ne!((was.efforts, was.max_output), (now.efforts, now.max_output));
    assert!(now.takes_effort(again.agent.session.effort));
    let (outcome, _) = again.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        live.provider.bodies()[before],
        again.provider.bodies()[0],
        "the resumed session's first request is the one the live session sent"
    );
}

#[tokio::test]
async fn a_refetch_that_names_the_chats_model_and_changes_another_entry_changes_no_byte_it_sends() {
    let mut next = Vec::new();
    for refetch in [false, true] {
        let mut live = live_rig(vec![plain_done(), plain_done()], false);
        let model = live.agent.session.model.clone();
        let haiku = claude_entry(HAIKU, "haiku", 1_000_000);
        claude(&live.provider, listed(std::slice::from_ref(&haiku)));
        let mut named = claude_entry(&model, "opus", 200_000);
        named["max_tokens"] = json!(64_000);
        named["capabilities"]["effort"]["max"] = json!({ "supported": false });
        let mut changed = haiku;
        changed["max_tokens"] = json!(64_000);
        let added = claude_entry("claude-opus-6", "opus", 1_000_000);
        live.provider
            .knobs(|k| k.fresh = listed(&[named, added, changed]));
        live.turn("first").await;
        let before = (
            live.provider.capabilities(&model),
            live.provider.capabilities(HAIKU),
        );
        if refetch {
            live.provider.refresh_list(PathBuf::new(), false).await;
            let list = live.provider.model_list().unwrap();
            assert!(list.find("claude-opus-6").is_some());
            assert!(list.find(&model).is_none());
            let fresh = live.provider.knobs(|k| k.fresh.clone()).unwrap();
            assert_ne!(list.find(HAIKU), fresh.find(HAIKU));
        }
        let after = (
            live.provider.capabilities(&model),
            live.provider.capabilities(HAIKU),
        );
        assert_eq!(before, after);
        live.turn("next").await;
        next.push(live.provider.bodies()[1].clone());
    }
    assert_eq!(next[0], next[1]);
}

#[tokio::test]
async fn plain_turns_with_web_search_on_resume_with_the_same_bytes() {
    same_next_request(
        vec![
            done(&[B::Think("a"), B::Text("one")]),
            done(&[B::Think("b"), B::Text("two")]),
        ],
        &["first", "second"],
        true,
    )
    .await;
}

#[tokio::test]
async fn a_tool_call_resumes_with_the_same_bytes() {
    same_next_request(
        vec![
            uses(&[B::Think("a"), echo("e1")]),
            done(&[B::Think("b"), B::Text("two")]),
        ],
        &["first"],
        false,
    )
    .await;
}

const HAIKU: &str = "claude-haiku-5-5";

async fn switch_to(rig: &mut Rig, model: &str) -> Vec<AgentEvent> {
    switch_on(rig, None, model).await
}

async fn switch_on(rig: &mut Rig, endpoint: Option<&str>, model: &str) -> Vec<AgentEvent> {
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    rig.agent
        .change_settings(endpoint.map(Into::into), Some(model.into()), None, &ev_tx)
        .await;
    drop(ev_tx);
    let mut events = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        events.push(e);
    }
    events
}

fn warnings(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SettingsChanged { changes, .. } => Some(changes),
            _ => None,
        })
        .flatten()
        .flat_map(|change| change.warnings.clone())
        .collect()
}

#[tokio::test]
async fn a_switch_to_a_model_the_account_does_not_list_warns_once_and_still_switches() {
    let env = env("resume-warns");
    let (tx, rx) = mpsc::channel(64);
    let provider = searching(vec![], tx);
    let agent = base(provider.clone(), &env, false).with_connections(
        vec![
            connection("a", provider.clone()),
            connection("b", provider.clone()),
        ],
        Vec::new(),
        "a",
    );
    let mut rig = Rig {
        agent,
        provider,
        rx,
        store: MemoryStore::default(),
        _env: env,
    };
    let own = |id: &str| claude_entry(id, "any", 1_000_000);
    claude(&rig.provider, listed(&[own(DEFAULT_MODEL), own(HAIKU)]));
    assert!(warnings(&switch_to(&mut rig, HAIKU).await).is_empty());
    let events = switch_to(&mut rig, "claude-zzz-9-9").await;
    assert_eq!(
        warnings(&events),
        ["a doesn't list claude-zzz-9-9, so the request may be refused."]
    );
    assert_eq!(
        rig.agent.settings.pending.as_ref().unwrap().model,
        "claude-zzz-9-9"
    );
    let key = "sk-ant-api03-CANARYMODEL0000";
    let hidden = warnings(&switch_to(&mut rig, key).await);
    assert_eq!(hidden.len(), 1, "{hidden:?}");
    assert!(!hidden[0].contains("CANARY"), "{hidden:?}");
    let both = switch_on(&mut rig, Some("b"), "claude-xxx-9-9").await;
    assert_eq!(
        warnings(&both),
        ["b doesn't list claude-xxx-9-9, so the request may be refused."]
    );
    claude(&rig.provider, None);
    assert!(warnings(&switch_to(&mut rig, "claude-yyy-9-9").await).is_empty());
}

fn after(rig: &Rig, from: usize) -> Vec<LogEvent> {
    log_events(&rig.store).into_iter().skip(from).collect()
}

fn tools_written(events: &[LogEvent]) -> Vec<String> {
    pick!(
        events,
        LogEvent::SettingsChanged {
            tools_sha256: Some(sha),
            ..
        } => sha.clone()
    )
}

fn cuts_written(events: &[LogEvent]) -> Vec<LogEvent> {
    events
        .iter()
        .filter(|e| matches!(e, LogEvent::ThinkingCut { .. }))
        .cloned()
        .collect()
}

fn assert_kept_prefix(agent: &Agent) {
    let notice = agent.resume_notice_text().unwrap();
    assert!(!notice.contains("prompt or tools changed"), "{notice}");
    assert!(!agent.settings.binding_controls);
}

#[tokio::test]
async fn a_form_changing_switch_logged_before_a_kill_resumes_byte_for_byte() {
    let mut live = live_rig(
        vec![
            done(&[B::Think("kept reasoning"), B::Text("one")]),
            plain_done(),
        ],
        true,
    );
    live.turn("first").await;
    switch_to(&mut live, HAIKU).await;
    let records = records(&live.store);
    live.turn("second").await;
    let mut again = resumed(&live, &records, vec![plain_done()], true).unwrap();
    assert_kept_prefix(&again.agent);
    let resumed_at = test_guard::records(&again.store).len();
    again.turn("second").await;
    assert_eq!(again.provider.bodies()[0], live.provider.bodies()[1]);
    let body = again.provider.bodies()[0].to_string();
    assert!(!body.contains("kept reasoning"), "{body}");
    assert_eq!(
        cuts_written(&after(&again, resumed_at)),
        cuts_written(&after(&live, records.len()))
    );
    let started = log_events(&again.store);
    let tools = started.iter().rev().find_map(|e| match e {
        LogEvent::SessionResumed { tools_sha256, .. } => Some(tools_sha256.clone()),
        _ => None,
    });
    let haiku = again.agent.tool_set("env", HAIKU).sha256;
    assert_eq!(
        tools,
        Some(haiku),
        "session_resumed logs the next request's tools"
    );

    let twice = resumed(&again, &test_guard::records(&again.store), vec![], true).unwrap();
    assert_kept_prefix(&twice.agent);
}

#[tokio::test]
async fn going_back_after_a_resume_writes_the_tools_the_live_session_wrote() {
    let mut live = live_rig(
        vec![
            done(&[B::Think("kept reasoning"), B::Text("one")]),
            plain_done(),
        ],
        true,
    );
    live.turn("first").await;
    switch_to(&mut live, HAIKU).await;
    let records = records(&live.store);
    switch_to(&mut live, DEFAULT_MODEL).await;
    live.turn("second").await;
    let mut again = resumed(&live, &records, vec![plain_done()], true).unwrap();
    assert_kept_prefix(&again.agent);
    let resumed_at = test_guard::records(&again.store).len();
    switch_to(&mut again, DEFAULT_MODEL).await;
    again.turn("second").await;
    assert_eq!(again.provider.bodies()[0], live.provider.bodies()[1]);
    assert_eq!(
        tools_written(&after(&again, resumed_at)),
        tools_written(&after(&live, records.len()))
    );
    assert_eq!(tools_written(&after(&live, records.len())).len(), 1);
}

#[tokio::test]
async fn a_switch_and_a_switch_back_before_a_kill_resume_byte_for_byte() {
    let mut live = live_rig(
        vec![
            done(&[B::Think("on opus"), B::Text("one")]),
            done(&[B::Text("two")]),
            plain_done(),
        ],
        true,
    );
    live.turn("first").await;
    switch_to(&mut live, HAIKU).await;
    live.turn("second").await;
    switch_to(&mut live, DEFAULT_MODEL).await;
    let records = records(&live.store);
    live.turn("third").await;
    let mut again = resumed(&live, &records, vec![plain_done()], true).unwrap();
    assert_kept_prefix(&again.agent);
    let resumed_at = test_guard::records(&again.store).len();
    again.turn("third").await;
    assert_eq!(again.provider.bodies()[0], live.provider.bodies()[2]);
    assert_eq!(
        cuts_written(&after(&again, resumed_at)),
        cuts_written(&after(&live, records.len()))
    );
    assert_eq!(
        tools_written(&after(&again, resumed_at)),
        tools_written(&after(&live, records.len()))
    );
}

#[tokio::test]
async fn a_completed_paused_search_resumes_with_the_same_bytes() {
    same_next_request(
        vec![
            paused(&[B::Think("plan"), B::Search("s1", "rust")]),
            done(&[B::Found("s1"), B::Text("ok")]),
        ],
        &["search"],
        true,
    )
    .await;
}

#[tokio::test]
async fn a_search_closed_at_a_stop_resumes_with_the_same_bytes() {
    same_next_request(
        vec![
            uses(&[
                B::Think("before"),
                B::Search("s1", "rust"),
                B::Think("after"),
                echo("e1"),
            ])
            .then(ClientCommand::Cancel),
        ],
        &["go"],
        true,
    )
    .await;
}

#[tokio::test]
async fn a_search_closed_at_the_pause_cap_resumes_with_the_same_bytes() {
    let mut steps = pause_chain();
    steps.push(done(&[B::Text("done")]));
    same_next_request(steps, &["search a lot"], true).await;
}

#[tokio::test]
async fn a_search_an_error_left_open_and_a_clean_exit_closed_resumes_with_the_same_bytes() {
    same_next_request(
        vec![
            uses(&[B::Search("s1", "rust"), echo("e1")]),
            failing(
                &[],
                mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
            ),
        ],
        &["go"],
        true,
    )
    .await;
}

#[tokio::test]
async fn a_search_an_error_left_open_before_a_crash_is_closed_at_resume() {
    let mut live = live_until_resume(
        vec![
            uses(&[B::Search("s1", "rust"), echo("e1")]),
            failing(
                &[],
                mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
            ),
        ],
        &["go"],
        true,
        false,
    )
    .await;
    let records = records(&live.store);
    let before = live.sent();
    let (_, live_events) = live.turn("next").await;
    let mut again = resumed(&live, &records, vec![plain_done()], true).unwrap();
    let (outcome, events) = again.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        live.provider.bodies()[before],
        again.provider.bodies()[0],
        "resume closes the search as the next turn would have"
    );
    assert_eq!(ended(&events), ended(&live_events));
    assert_eq!(
        ended(&events),
        vec![("s1".to_owned(), unfinished(UnfinishedReason::Failed, true))]
    );
    let closed = |rig: &Rig| -> Vec<LogEvent> {
        log_events(&rig.store)
            .into_iter()
            .skip(records.len())
            .filter(|e| matches!(e, LogEvent::TailClosed { .. }))
            .collect()
    };
    let live_closed = closed(&live);
    assert!(matches!(
        live_closed.as_slice(),
        [LogEvent::TailClosed {
            reason: UnfinishedReason::Failed,
            ..
        }]
    ));
    assert_eq!(closed(&again), live_closed);
}

#[tokio::test]
async fn a_search_open_in_a_turn_wisp_stopped_in_closes_as_stopped() {
    let live = live_until_resume(
        vec![
            uses(&[B::Search("s1", "rust"), echo("e1")]),
            done(&[B::Found("s1"), B::Text("found")]),
        ],
        &["go"],
        true,
        true,
    )
    .await;
    let records = records(&live.store);
    let cut = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::ToolCallFinished { .. }))
        .unwrap()
        + 1;
    let records = records[..cut].to_vec();
    let mut again = resumed(&live, &records, vec![plain_done()], true).unwrap();
    let (outcome, events) = again.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        ended(&events),
        vec![("s1".to_owned(), unfinished(UnfinishedReason::Stopped, true))]
    );
    let added: Vec<LogEvent> = log_events(&again.store)
        .into_iter()
        .skip(records.len())
        .collect();
    let closed: Vec<&LogEvent> = added
        .iter()
        .filter(|e| matches!(e, LogEvent::TailClosed { .. }))
        .collect();
    assert!(
        matches!(
            closed.as_slice(),
            [LogEvent::TailClosed {
                reason: UnfinishedReason::Stopped,
                ..
            }]
        ),
        "{added:#?}"
    );
    let repaired = added
        .iter()
        .position(|e| matches!(e, LogEvent::TurnEnded { turn: 1, .. }))
        .unwrap();
    let tail = added
        .iter()
        .position(|e| matches!(e, LogEvent::TailClosed { .. }))
        .unwrap();
    assert!(tail < repaired, "{added:#?}");
}

#[tokio::test]
async fn a_cancelled_turn_with_web_search_on_resumes_with_the_same_bytes() {
    same_next_request(
        vec![
            done(&[B::Think("a"), B::Text("one")]),
            hanging(&[B::Think("half")]).then(ClientCommand::Cancel),
        ],
        &["first", "second"],
        true,
    )
    .await;
}

fn kept_tools_steps() -> Vec<Step> {
    vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        server_down(),
        search_off(),
        server_down(),
        done(&[B::Found("s1"), B::Text("found")]),
    ]
}

async fn fail_compact_and_retry(rig: &mut Rig) {
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    let compaction = compact_now(rig).await;
    assert!(
        compaction
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { .. })),
        "{compaction:?}"
    );
    let retry = ClientCommand::Retry {
        turn: 2,
        endpoint: None,
        request: wisp_protocol::RequestId(1),
    };
    let events = rig.serve_for(retry).await;
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            })
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_retry_after_resume_keeps_the_failed_request_s_tools_as_live() {
    let mut steps = vec![done(&[B::Think("a"), B::Text("one")])];
    steps.extend(kept_tools_steps());
    let mut live = live_rig(steps, true);
    live.turn("first").await;
    live.end().await;
    let records = records(&live.store);
    let before = live.sent();
    fail_compact_and_retry(&mut live).await;

    let mut again = resumed(&live, &records, kept_tools_steps(), true).unwrap();
    fail_compact_and_retry(&mut again).await;
    let kept = kept_on(&again);
    assert!(kept.last().unwrap().is_some(), "{kept:?}");
    assert_eq!(kept, kept_on(&live));
    let live_bodies = live.provider.bodies();
    let bodies = again.provider.bodies();
    assert_eq!(bodies.len(), 5);
    assert_eq!(bodies[4], bodies[1], "the Retry repeats the failed request");
    assert_eq!(&live_bodies[before..], &bodies[..]);
}

#[tokio::test]
async fn a_paused_reply_closed_at_resume_is_never_sent_last_and_a_new_message_follows_it() {
    let live = live_until_resume(
        vec![
            paused(&[B::Think("plan"), B::Search("s1", "rust")]),
            done(&[B::Found("s1"), B::Text("found")]),
        ],
        &["go"],
        true,
        true,
    )
    .await;
    let records = records(&live.store);
    let cut = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::StepEnded { .. }))
        .unwrap()
        + 1;
    let records = records[..cut].to_vec();
    let mut again = resumed(&live, &records, vec![plain_done()], true).unwrap();
    let (tx, mut rx) = mpsc::channel(4096);
    again.agent.start_log(&tx).await;
    drop(tx);
    let mut seen = Vec::new();
    while let Some(event) = rx.recv().await {
        seen.push(event);
    }
    assert!(
        seen.iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error { offer: Some(_), .. },
                ..
            }
        )),
        "{seen:#?}"
    );
    let len = again.agent.history().len();
    assert_eq!(again.agent.paused_flags(len).last(), Some(&true));
    assert_eq!(again.agent.closed_tail_flags(len).last(), Some(&true));
    assert!(
        crate::agent::check_request_layout(
            &again.agent.history(),
            (
                &again.agent.paused_flags(len),
                &again.agent.closed_tail_flags(len),
            ),
            &again.agent.search.closed.calls,
            again.agent.provider().as_ref(),
        )
        .is_err()
    );
    let (outcome, _) = again.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_retry_after_a_crash_closed_a_paused_reply_sends_one_note_after_it() {
    let live = live_until_resume(
        vec![
            paused(&[B::Think("plan"), B::Search("s1", "rust")]),
            done(&[B::Found("s1"), B::Text("found")]),
        ],
        &["go"],
        true,
        true,
    )
    .await;
    let records = records(&live.store);
    let cut = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::StepEnded { .. }))
        .unwrap()
        + 1;
    let records = records[..cut].to_vec();
    let mut again = resumed(&live, &records, vec![plain_done()], true).unwrap();
    let (tx, mut rx) = mpsc::channel(4096);
    again.agent.start_log(&tx).await;
    drop(tx);
    let mut seen = Vec::new();
    while let Some(event) = rx.recv().await {
        seen.push(event);
    }
    assert!(
        seen.iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error {
                    kind: ErrorKind::Interrupted,
                    offer: Some(_),
                    ..
                },
                ..
            }
        )),
        "{seen:#?}"
    );
    let (turn, _) = again.agent.retry_offer().unwrap();
    assert_eq!(turn, 1);
    let restored = resumed(&live, &test_guard::records(&again.store), Vec::new(), true).unwrap();
    assert_eq!(
        restored.agent.retry_offer().map(|(turn, _)| turn),
        Some(1),
        "a later resume restores the offer"
    );
    again
        .provider
        .commands()
        .try_send(ClientCommand::Retry {
            turn,
            endpoint: None,
            request: wisp_protocol::RequestId(5),
        })
        .unwrap();
    let outcome = cut::retried(&mut again.agent, &mut again.rx).await.unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );

    let continued = &live.provider.bodies()[1];
    let sent = again.provider.bodies();
    assert_eq!(sent.len(), 1);
    let body = &sent[0];
    assert_eq!(body["system"], continued["system"]);
    assert_eq!(body["tools"], continued["tools"]);
    let messages = body["messages"].as_array().unwrap();
    let live_messages = continued["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{messages:#?}");
    assert_eq!(messages[0], live_messages[0]);
    assert_eq!(messages[1]["role"], "assistant");
    let paused_blocks = live_messages[1]["content"].as_array().unwrap();
    let closed_blocks = messages[1]["content"].as_array().unwrap();
    assert_eq!(closed_blocks.len(), paused_blocks.len());
    assert_eq!(closed_blocks[0], paused_blocks[0], "the thinking is kept");
    assert_eq!(closed_blocks[1], closed_text("rust"));
    assert_eq!(
        messages[2],
        json!({"role": "user", "content": [{"type": "text", "text": CLOSED_PAUSE_NOTE}]})
    );
    assert_eq!(body.to_string().matches(CLOSED_PAUSE_NOTE).count(), 1);

    let events = log_events(&again.store);
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, &*again.agent.history());
}

mod cut;

mod fallback;

fn compacted() -> Step {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start(1_200))];
    script.push(Ok(ProviderEvent::BlockStart {
        index: 0,
        block: opaque(json!({"type": "compaction", "content": "So far: a search and two tools."})),
    }));
    script.push(Ok(ProviderEvent::BlockStop { index: 0 }));
    script.extend(testing::ended(
        StopReason::Compaction,
        usage(1_200, 0, 300),
        Vec::new(),
    ));
    Step::from(script)
}
