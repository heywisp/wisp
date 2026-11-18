use super::native::{digest, native_rig, origin, searches, tools_cuts, tools_events};
use super::retry_choice::{OTHER, retry};
use super::*;
use crate::search::{NoSearch, SearchForm};

pub(super) fn server_down() -> Step {
    refused(classified(
        ErrorKind::Server,
        "server error",
        FailurePhase::Connect,
        RetryClass::Never,
    ))
}

async fn refused_after_a_failure(retried: Vec<Step>) -> Rig {
    let mut steps = vec![uses(&[echo("e0")]), server_down()];
    steps.extend(retried);
    let mut rig = native_rig(steps, &[DEFAULT_MODEL]);
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    refuse_between_turns(&mut rig);
    rig
}

pub(super) async fn compact_now(rig: &mut Rig) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::channel(4096);
    rig.agent
        .manual_compaction(None, wisp_protocol::RequestId(1), &mut rig.rx, &tx)
        .await;
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    events
}

async fn retried_after_a_refused_compaction(first: Step, answered: Step) -> (Rig, Vec<AgentEvent>) {
    let steps = vec![
        first,
        server_down(),
        search_off(),
        server_down(),
        answered,
        done(&[B::Text("again")]),
    ];
    let mut rig = native_rig(steps, &[DEFAULT_MODEL]);
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    assert!(rig.agent.tail_live());
    let compaction = compact_now(&mut rig).await;
    assert!(
        compaction
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { .. })),
        "{compaction:?}"
    );
    assert!(ended(&compaction).is_empty(), "{compaction:?}");
    assert!(rig.agent.tail_live());
    assert!(!rig.kinds().contains(&"tail_closed".to_owned()));
    let events = rig.serve_for(retry(None)).await;
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
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 5);
    assert!(searches(&bodies[1]));
    assert!(!searches(&bodies[3]));
    assert_eq!(bodies[4], bodies[1], "the Retry repeats the failed request");
    let retried = rig.messages(4);
    let call = retried[1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "server_tool_use");
    assert_eq!(call.unwrap()["id"], "s1", "{retried:?}");
    assert_eq!(
        kept_on(&rig),
        vec![None, Some(digest(&rig, SearchForm::Native))]
    );
    assert!(!rig.kinds().contains(&"tail_closed".to_owned()));
    (rig, events)
}

#[tokio::test]
async fn a_compaction_refused_for_search_leaves_the_failed_turns_open_search_to_its_retry() {
    let (mut rig, _) = retried_after_a_refused_compaction(
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        done(&[B::Found("s1"), B::Text("found")]),
    )
    .await;
    let (_, later) = rig.turn("again").await;
    assert!(tools_events(&later).is_empty(), "{later:?}");
    assert!(!searches(&rig.provider.bodies()[5]));
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_compaction_refused_for_search_leaves_a_failed_pause_to_its_retry() {
    let (mut rig, _) = retried_after_a_refused_compaction(
        paused(&[B::Think("t1"), B::Search("s1", "rust")]),
        done(&[B::Found("s1"), B::Text("found")]),
    )
    .await;
    let retried = rig.messages(4);
    let last = retried.last().unwrap()["content"].as_array().unwrap();
    assert_eq!(last.last().unwrap()["type"], "server_tool_use");
    let (_, later) = rig.turn("again").await;
    assert!(tools_events(&later).is_empty(), "{later:?}");
    assert!(!searches(&rig.provider.bodies()[5]));
    assert_resumes_byte_for_byte(&rig);
}

async fn completed_refused_compaction(first: Step) {
    let steps = vec![
        first,
        server_down(),
        search_off(),
        compacted_step(),
        done(&[B::Text("again")]),
    ];
    let mut rig = native_rig(steps, &[DEFAULT_MODEL]);
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    assert!(rig.agent.tail_live());
    let compaction = compact_now(&mut rig).await;
    assert!(
        compaction
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{compaction:?}"
    );
    assert_eq!(
        ended(&compaction),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, false))]
    );
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed");
    assert_eq!(closed.len(), 1, "{kinds:?}");
    assert!(closed[0] < position(&kinds, "context_compacted")[0]);
    assert!(!rig.agent.tail_live());
    let bodies = rig.provider.bodies();
    assert!(searches(&bodies[2]));
    assert!(!searches(&bodies[3]));
    let (outcome, later) = rig.turn("again").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(tools_events(&later).is_empty(), "{later:?}");
    assert!(ended(&later).is_empty(), "{later:?}");
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 5);
    assert!(!searches(&bodies[4]));
    assert!(
        !bodies[4].to_string().contains("server_tool_use"),
        "{}",
        bodies[4]
    );
    assert_eq!(position(&rig.kinds(), "tail_closed").len(), 1);
    assert_eq!(kept_on(&rig), vec![None, None]);
    let events = log_events(&rig.store);
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, &*rig.agent.history());
    assert_eq!(rebuilt.view(), rig.agent.view_messages());
    assert_eq!(rebuilt.closed.calls, rig.agent.search.closed.calls);
    assert!(rebuilt.closed.calls.contains("s1"));
    assert_eq!(rebuilt.paused, rig.agent.search.paused);
    assert_eq!(rebuilt.cuts, rig.agent.search.cuts);
    assert_eq!(
        rebuilt.search_refused,
        BTreeSet::from([IMPLICIT_ENDPOINT.to_owned()])
    );
}

#[tokio::test]
async fn a_completed_refused_compaction_closes_the_open_search() {
    completed_refused_compaction(uses(&[B::Search("s1", "rust"), echo("e1")])).await;
}

#[tokio::test]
async fn a_completed_refused_compaction_closes_a_failed_pause() {
    completed_refused_compaction(paused(&[B::Think("t1"), B::Search("s1", "rust")])).await;
}

fn refuse_between_turns(rig: &mut Rig) {
    rig.agent.search.refused.insert(IMPLICIT_ENDPOINT.into());
    rig.agent.note_sent_tools();
}

pub(super) fn kept_on(rig: &Rig) -> Vec<Option<String>> {
    records_of(rig, "turn_started")
        .into_iter()
        .map(|r| match r {
            LogEvent::TurnStarted {
                kept_tools_sha256, ..
            } => kept_tools_sha256,
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_kept_tools_retry_closes_its_new_search_before_the_results_request() {
    let mut rig = refused_after_a_failure(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        done(&[B::Text("ok")]),
    ])
    .await;
    let events = rig.serve_for(retry(None)).await;
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
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 4);
    assert!(searches(&bodies[1]));
    assert_eq!(bodies[2], bodies[1], "the Retry repeats the failed request");
    assert!(!searches(&bodies[3]));
    let results = rig.messages(3);
    assert_eq!(results[3]["content"][0], closed_text("rust"));
    assert_eq!(
        ended(&events),
        vec![(
            "s1".to_owned(),
            unfinished(UnfinishedReason::Refused, false)
        )]
    );
    assert!(tools_events(&events).is_empty());
    assert_eq!(
        rig.agent.epoch.marked_at(),
        (2, 3),
        "the prefix changes at the results request, not at the Retry"
    );
    let none = digest(&rig, SearchForm::None(NoSearch::Refused));
    let cuts = tools_cuts(&rig);
    assert_eq!(cuts.len(), 1, "{cuts:?}");
    assert_eq!((cuts[0].0, cuts[0].2.clone()), (origin(), none));
    let kinds = rig.kinds();
    let started = position(&kinds, "turn_started");
    let closed = position(&kinds, "tail_closed");
    let cut = position(&kinds, "thinking_cut");
    assert_eq!(closed.len(), 1);
    assert!(started[1] < closed[0] && closed[0] < cut[0], "{kinds:?}");
    assert!(!kinds[started[1]..].contains(&"settings_changed".to_owned()));
    assert_eq!(
        kept_on(&rig),
        vec![None, Some(digest(&rig, SearchForm::Native))]
    );
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_kept_tools_retry_that_pauses_ends_the_turn_as_at_the_cap() {
    let mut rig = refused_after_a_failure(vec![
        paused(&[B::Search("s1", "rust")]),
        done(&[B::Text("next")]),
    ])
    .await;
    let events = rig.serve_for(retry(None)).await;
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed {
                    stop_reason: StopReason::PauseTurn,
                    committed: true,
                    ..
                },
                ..
            })
        ),
        "{events:?}"
    );
    assert_eq!(rig.sent(), 3);
    assert_eq!(
        ended(&events),
        vec![(
            "s1".to_owned(),
            unfinished(UnfinishedReason::Refused, false)
        )]
    );
    assert!(tools_cuts(&rig).is_empty());
    let (_, later) = rig.turn("again").await;
    assert!(tools_events(&later).is_empty(), "{later:?}");
    let body = &rig.provider.bodies()[3];
    assert!(!searches(body));
    assert_eq!(body["messages"][3]["content"][0], closed_text("rust"));
    assert_eq!(tools_cuts(&rig).len(), 1);
    assert_eq!(
        kept_on(&rig),
        vec![None, Some(digest(&rig, SearchForm::Native)), None]
    );
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_retry_on_another_endpoint_keeps_no_tools() {
    let mut rig = native_rig(vec![server_down()], &[DEFAULT_MODEL]);
    let other = rig.with_other(vec![done(&[B::Text("over here")])]);
    rig.turn("go").await;
    refuse_between_turns(&mut rig);
    rig.serve_for(retry(Some(OTHER))).await;
    assert_eq!(other.bodies().len(), 1);
    assert!(searches(&other.bodies()[0]));
    assert_eq!(kept_on(&rig), vec![None, None]);
}

#[tokio::test]
async fn a_retry_with_nothing_refused_since_the_failure_keeps_no_tools() {
    let mut rig = native_rig(
        vec![uses(&[echo("e0")]), server_down(), done(&[B::Text("ok")])],
        &[DEFAULT_MODEL],
    );
    rig.turn("go").await;
    rig.serve_for(retry(None)).await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies[2], bodies[1]);
    assert!(searches(&bodies[2]));
    assert_eq!(kept_on(&rig), vec![None, None]);
    assert!(tools_cuts(&rig).is_empty());
}

#[tokio::test]
async fn an_in_step_retry_of_a_kept_tools_request_repeats_its_bytes() {
    let mut rig =
        refused_after_a_failure(vec![overloaded_before_any_event(), done(&[B::Text("ok")])]).await;
    rig.serve_for(retry(None)).await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 4);
    assert_eq!(bodies[2], bodies[1]);
    assert_eq!(bodies[3], bodies[1]);
    assert!(tools_cuts(&rig).is_empty());
}

#[tokio::test]
async fn a_retry_on_an_endpoint_refused_before_the_failure_keeps_no_tools() {
    let mut rig = native_rig(
        vec![uses(&[echo("e0")]), server_down(), done(&[B::Text("ok")])],
        &[DEFAULT_MODEL],
    );
    refuse_between_turns(&mut rig);
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    rig.serve_for(retry(None)).await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 3);
    assert!(!searches(&bodies[1]));
    assert_eq!(bodies[2], bodies[1]);
    assert_eq!(kept_on(&rig), vec![None, None]);
}

const RV3_SUMMARY: &str = "Goal and standing instructions: search.\n\nDecisions: none.\n\nState of the work: searching.\n\nFiles, branches and artefacts: none.\n\nCommands, errors and fixes: none.\n\nOpen questions: none.";

fn rv3_wisp_rig(steps: Vec<Step>) -> Rig {
    let mut rig = rig(steps);
    let placeholder = Agent::new(rig.provider.clone(), default_session());
    let agent = std::mem::replace(&mut rig.agent, placeholder);
    let config = format!(
        "[endpoints.env.models.\"{DEFAULT_MODEL}\"]\nweb_search = [\"web_search_20250305\"]\nnative_compaction = false\n"
    );
    let settings = crate::context::ModelSettings::new(
        crate::models::ModelTable::shipped().unwrap(),
        crate::models::ConfigModels::parse(&config).unwrap(),
    );
    rig.agent = agent.with_model_settings(settings).with_web_search(true);
    rig
}

fn rv3_written() -> Step {
    let input = 50_000;
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start(input))];
    script.extend([
        Ok(ProviderEvent::BlockStart {
            index: 0,
            block: ContentBlock::Text {
                text: RV3_SUMMARY.into(),
            },
        }),
        Ok(ProviderEvent::BlockStop { index: 0 }),
    ]);
    script.extend(testing::ended(
        StopReason::EndTurn,
        usage(input, 0, 10),
        Vec::new(),
    ));
    Step::from(script)
}

fn rv3_native_rejected() -> Step {
    let mut error = ProviderError::new(ErrorKind::InvalidRequest, "compaction: Extra inputs");
    error.rejected = vec![Feature::NativeCompaction];
    refused(error)
}

fn rv3_engines(compaction: &[AgentEvent]) -> Vec<wisp_protocol::CompactionEngine> {
    compaction
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compacted { engine, .. } => Some(*engine),
            AgentEvent::CompactionFailed {
                engine: Some(engine),
                ..
            } => Some(*engine),
            _ => None,
        })
        .collect()
}

async fn rv3_retried_after_a_refused_wisp_compaction(rig: &mut Rig, legs: usize) {
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    assert!(rig.agent.tail_live());
    let compaction = compact_now(rig).await;
    assert_eq!(
        rv3_engines(&compaction),
        vec![wisp_protocol::CompactionEngine::Wisp]
    );
    assert!(
        compaction
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { .. })),
        "{compaction:?}"
    );
    assert!(ended(&compaction).is_empty(), "{compaction:?}");
    assert!(rig.agent.tail_live());
    assert!(!rig.kinds().contains(&"tail_closed".to_owned()));
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::None(NoSearch::Refused)
    );
    let events = rig.serve_for(retry(None)).await;
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
    let bodies = rig.provider.bodies();
    let retried = 2 + legs;
    assert_eq!(bodies.len(), retried + 1);
    assert!(searches(&bodies[1]));
    assert!(searches(&bodies[retried - 2]));
    assert!(!searches(&bodies[retried - 1]));
    assert_eq!(
        bodies[retried]["messages"], bodies[1]["messages"],
        "the Retry repeats the failed request's messages"
    );
    assert_eq!(bodies[retried]["tools"], bodies[1]["tools"]);
    let call = rig.messages(retried)[1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "server_tool_use")
        .cloned();
    assert_eq!(call.unwrap()["id"], "s1");
    assert_eq!(
        kept_on(rig),
        vec![None, Some(digest(rig, SearchForm::Native))]
    );
    assert!(!rig.kinds().contains(&"tail_closed".to_owned()));
}

#[tokio::test]
async fn rv3_a_wisp_engine_compaction_refused_for_search_leaves_the_tail_to_its_retry() {
    let mut rig = rv3_wisp_rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        server_down(),
        search_off(),
        server_down(),
        done(&[B::Found("s1"), B::Text("found")]),
    ]);
    rv3_retried_after_a_refused_wisp_compaction(&mut rig, 2).await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies[4], bodies[1], "the Retry repeats the failed request");
}

#[tokio::test]
async fn rv3_a_native_fallback_to_wisps_engine_refused_for_search_leaves_the_tail_to_its_retry() {
    let mut rig = native_rig(
        vec![
            uses(&[B::Search("s1", "rust"), echo("e1")]),
            server_down(),
            rv3_native_rejected(),
            search_off(),
            server_down(),
            done(&[B::Found("s1"), B::Text("found")]),
        ],
        &[DEFAULT_MODEL],
    );
    rv3_retried_after_a_refused_wisp_compaction(&mut rig, 3).await;
}

async fn rv3_completed_wisp_compaction(first: Step) {
    let mut rig = rv3_wisp_rig(vec![
        first,
        server_down(),
        search_off(),
        rv3_written(),
        done(&[B::Text("again")]),
    ]);
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    assert!(rig.agent.tail_live());
    let compaction = compact_now(&mut rig).await;
    assert_eq!(
        rv3_engines(&compaction),
        vec![wisp_protocol::CompactionEngine::Wisp]
    );
    assert!(
        compaction
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{compaction:?}"
    );
    assert_eq!(
        ended(&compaction),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, false))]
    );
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed");
    assert_eq!(closed.len(), 1, "{kinds:?}");
    assert!(closed[0] < position(&kinds, "context_compacted")[0]);
    assert!(!rig.agent.tail_live());
    let bodies = rig.provider.bodies();
    assert!(searches(&bodies[2]));
    assert!(!searches(&bodies[3]));
    let (outcome, later) = rig.turn("again").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(tools_events(&later).is_empty(), "{later:?}");
    assert!(ended(&later).is_empty(), "{later:?}");
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 5);
    assert!(!searches(&bodies[4]));
    assert!(
        !bodies[4].to_string().contains("server_tool_use"),
        "{}",
        bodies[4]
    );
    assert_eq!(position(&rig.kinds(), "tail_closed").len(), 1);
    let events = log_events(&rig.store);
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, &*rig.agent.history());
    assert_eq!(rebuilt.view(), rig.agent.view_messages());
    assert_eq!(rebuilt.closed.calls, rig.agent.search.closed.calls);
    assert!(rebuilt.closed.calls.contains("s1"));
    assert_eq!(
        rebuilt.search_refused,
        BTreeSet::from([IMPLICIT_ENDPOINT.to_owned()])
    );
}

#[tokio::test]
async fn rv3_a_completed_wisp_engine_compaction_refused_for_search_closes_the_search_once() {
    rv3_completed_wisp_compaction(uses(&[B::Search("s1", "rust"), echo("e1")])).await;
}

#[tokio::test]
async fn rv3_a_completed_wisp_engine_compaction_refused_for_search_closes_a_failed_pause_once() {
    rv3_completed_wisp_compaction(paused(&[B::Think("t1"), B::Search("s1", "rust")])).await;
}
