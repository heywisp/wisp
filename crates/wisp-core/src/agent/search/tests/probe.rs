use wisp_protocol::log::FeatureState;
use wisp_protocol::{CacheEffect, CompactionTrigger, Feature, RebuildReason};

use super::kept_tools::compact_now;
use super::native::{
    digest, index_of, listed, logged_tools, native_rig, origin, refusal, refused_rebuilds,
    resumed_with, searches, tools_cuts, tools_events, without_search,
};
use super::*;
use crate::agent::compact::auto::TOO_LARGE;
use crate::clock::StillClock;
use crate::provider::Refused;
use crate::search::{NoSearch, SearchForm};
use crate::test_guard::{RED_PIXEL, models};

const UNKNOWN_TOOL: &str = "tools.1: Input tag 'web_search_20250305' does not match";

fn suspect() -> Step {
    refused(refusal(Refused::SearchSuspect, UNKNOWN_TOOL))
}

fn plain_400(message: &str) -> Step {
    refused(classified(
        ErrorKind::InvalidRequest,
        message,
        FailurePhase::Connect,
        RetryClass::Never,
    ))
}

fn still(mut rig: Rig) -> Rig {
    let placeholder = Agent::new(rig.provider.clone(), default_session());
    rig.agent = std::mem::replace(&mut rig.agent, placeholder).with_clock(Arc::new(StillClock));
    rig
}

#[tokio::test]
async fn a_suspected_refusal_resends_once_and_then_takes_the_refusal_path() {
    let mut rig = native_rig(
        vec![suspect(), done(&[B::Text("ok")]), done(&[B::Text("again")])],
        &[DEFAULT_MODEL],
    );
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let (_, later) = rig.turn("next").await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 3);
    assert!(searches(&bodies[0]));
    assert!(!searches(&bodies[1]));
    assert!(!searches(&bodies[2]));
    assert_eq!(bodies[1]["messages"], bodies[0]["messages"]);
    assert_eq!(bodies[2]["tools"], bodies[1]["tools"]);

    let state = index_of(&events, |e| {
        matches!(
            e,
            AgentEvent::FeatureStateChanged {
                feature: Feature::NativeWebSearch,
                state: FeatureState::FallenBack,
                ..
            }
        )
    });
    let rebuilt = index_of(&events, |e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                cache: CacheEffect::Rebuilt {
                    reason: RebuildReason::NativeSearchRefused
                },
                answers: None,
                ..
            }
        )
    });
    let tools = index_of(&events, |e| matches!(e, AgentEvent::ToolsChanged { .. }));
    assert_eq!((state.len(), rebuilt.len(), tools.len()), (1, 1, 1));
    assert!(state[0] < rebuilt[0] && rebuilt[0] < tools[0]);
    assert_eq!(
        tools_events(&events),
        vec![(Vec::new(), vec!["web_search".to_owned()])]
    );

    let kinds = rig.kinds();
    let record = position(&kinds, "native_web_search");
    let cut = position(&kinds, "thinking_cut");
    let changed = position(&kinds, "settings_changed");
    assert_eq!((record.len(), cut.len(), changed.len()), (1, 1, 1));
    assert!(record[0] < changed[0] && changed[0] < cut[0]);
    assert_eq!(
        records_of(&rig, "native_web_search"),
        vec![LogEvent::NativeWebSearch {
            endpoint_name: IMPLICIT_ENDPOINT.into(),
            state: FeatureState::FallenBack,
        }]
    );
    let none = digest(&rig, SearchForm::None(NoSearch::Refused));
    assert_eq!(tools_cuts(&rig), vec![(origin(), 0, none.clone())]);
    assert_eq!(
        logged_tools(&rig).last(),
        Some(&(without_search(&rig), none))
    );
    assert!(tools_events(&later).is_empty());
    assert!(refused_rebuilds(&later).is_empty());
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::None(NoSearch::Refused)
    );
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn the_resend_is_the_same_request_the_organisation_refusal_sends() {
    let mut suspected = native_rig(
        vec![suspect(), done(&[B::Text("ok")]), done(&[B::Text("b")])],
        &[DEFAULT_MODEL],
    );
    let mut refused = native_rig(
        vec![search_off(), done(&[B::Text("ok")]), done(&[B::Text("b")])],
        &[DEFAULT_MODEL],
    );
    for rig in [&mut suspected, &mut refused] {
        rig.turn("go").await;
        rig.turn("next").await;
    }
    assert_eq!(suspected.provider.bodies(), refused.provider.bodies());
    let renderings = |rig: &Rig| -> Vec<Rendering> {
        rig.provider
            .sent()
            .into_iter()
            .map(|s| s.rendering)
            .collect()
    };
    assert_eq!(renderings(&suspected), renderings(&refused));
}

#[tokio::test]
async fn a_failed_resend_ends_the_turn_with_the_first_error_and_keeps_search() {
    let mut control = still(native_rig(
        vec![plain_400(UNKNOWN_TOOL), done(&[B::Text("again")])],
        &[DEFAULT_MODEL],
    ));
    control.turn("go").await;
    control.turn("next").await;
    for second in [plain_400("bad request"), suspect()] {
        let mut rig = still(native_rig(
            vec![suspect(), second, done(&[B::Text("again")])],
            &[DEFAULT_MODEL],
        ));
        let (outcome, events) = rig.turn("go").await;
        let TurnOutcome::Error { message, .. } = &outcome else {
            panic!("{outcome:?}");
        };
        assert!(message.contains(UNKNOWN_TOOL), "{message}");
        assert_eq!(rig.sent(), 2);
        assert!(searches(&rig.provider.bodies()[0]));
        assert!(!searches(&rig.provider.bodies()[1]));
        assert!(records_of(&rig, "native_web_search").is_empty());
        assert!(tools_cuts(&rig).is_empty());
        assert!(tools_events(&events).is_empty(), "{events:?}");
        assert!(refused_rebuilds(&events).is_empty());
        assert_eq!(
            rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
            SearchForm::Native
        );
        let (_, next) = rig.turn("next").await;
        assert!(tools_events(&next).is_empty(), "{next:?}");
        assert!(refused_rebuilds(&next).is_empty());
        let bodies = rig.provider.bodies();
        assert!(searches(&bodies[2]));
        assert_eq!(bodies[2], control.provider.bodies()[1]);
        assert_eq!(
            rig.provider.sent()[2].rendering,
            control.provider.sent()[1].rendering
        );
        assert!(tools_cuts(&rig).is_empty());
    }
}

#[tokio::test]
async fn an_ordinary_error_never_resends_without_the_tool() {
    let mut rig = native_rig(
        vec![plain_400("bad request"), done(&[B::Text("again")])],
        &[DEFAULT_MODEL],
    );
    let (outcome, _) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(rig.sent(), 1);
}

#[tokio::test]
async fn an_open_search_in_the_tail_makes_a_suspect_an_ordinary_error() {
    let mut rig = native_rig(
        vec![
            uses(&[B::Search("s1", "rust"), echo("e1")]),
            suspect(),
            done(&[B::Text("ok")]),
        ],
        &[DEFAULT_MODEL],
    );
    let (outcome, _) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(rig.sent(), 2);
    assert!(records_of(&rig, "native_web_search").is_empty());
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
}

#[tokio::test]
async fn a_compaction_request_treats_a_suspect_as_an_ordinary_error() {
    let mut rig = native_rig(
        vec![done(&[B::Text("a")]), suspect(), done(&[B::Text("b")])],
        &[DEFAULT_MODEL],
    );
    rig.turn("one").await;
    compact_now(&mut rig).await;
    assert_eq!(rig.sent(), 2);
    assert!(records_of(&rig, "native_web_search").is_empty());
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
}

#[tokio::test]
async fn a_resumed_session_after_a_suspected_refusal_sends_no_tool_byte_for_byte() {
    let mut live = still(native_rig(
        vec![suspect(), done(&[B::Text("ok")]), done(&[B::Text("again")])],
        &[DEFAULT_MODEL],
    ));
    live.turn("go").await;
    let records = records(&live.store);
    live.turn("next").await;
    let (tx, mut rx) = mpsc::channel(64);
    let provider = searching(vec![done(&[B::Text("again")])], tx);
    let agent = Agent::new(provider.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(
            Toolbox::new(samples().into_iter().map(|s| s.tool)),
            live._env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_model_settings(models(&listed("env", &[DEFAULT_MODEL])))
        .with_web_search(true);
    let mut agent = resumed_with(agent, &records, implicit_context());
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    agent
        .run_turn("next".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    assert!(!searches(&provider.bodies()[0]));
    assert_eq!(live.provider.bodies()[2], provider.bodies()[0]);
}

fn probe_agent(
    provider: Arc<Fake>,
    session: SessionConfig,
    workspace: Workspace,
    config: &str,
    log: SessionLog,
) -> Agent {
    Agent::new(provider, session)
        .with_clock(Arc::new(StillClock))
        .with_tools(
            Toolbox::new(samples().into_iter().map(|s| s.tool)),
            workspace,
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_model_settings(models(config))
        .with_web_search(true)
        .with_log(log, implicit_context())
}

fn resume_with(
    provider: Arc<Fake>,
    session: SessionConfig,
    workspace: Workspace,
    config: &str,
    store: &MemoryStore,
    logged: &[LogRecord],
) -> Result<Agent, String> {
    let seq = u64::try_from(logged.len()).unwrap();
    probe_agent(
        provider,
        session,
        workspace,
        config,
        SessionLog::continuing(Box::new(store.clone()), "s1", seq),
    )
    .resume(logged, 0)
}

async fn resumed_on(live: &Rig, config: &str) -> Result<(Agent, Arc<Fake>), String> {
    let logged = records(&live.store);
    let (tx, _rx) = mpsc::channel(64);
    let provider = searching(vec![done(&[B::Text("later")])], tx);
    let agent = resume_with(
        provider.clone(),
        default_session(),
        live._env.workspace(),
        config,
        &MemoryStore::default(),
        &logged,
    )?;
    Ok((agent, provider))
}

async fn one_turn(agent: &mut Agent) {
    let (_tx, mut rx) = mpsc::channel(64);
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    let outcome = agent.run_turn("later".into(), &mut rx, &ev_tx).await;
    assert!(
        matches!(outcome, Some(TurnOutcome::Completed { .. })),
        "{outcome:?}"
    );
}

const OFF: &str = "[endpoints.env]\nnative_web_search = false\n";

#[tokio::test]
async fn a_pre_upgrade_gateway_log_resumes_with_search_on_now_once() {
    let mut live = rig(vec![done(&[B::Text("a")])]);
    let placeholder = Agent::new(live.provider.clone(), default_session());
    live.agent = std::mem::replace(&mut live.agent, placeholder)
        .with_clock(Arc::new(StillClock))
        .with_model_settings(models(OFF))
        .with_web_search(true);
    live.turn("one").await;
    assert!(!searches(&live.provider.bodies()[0]));

    let logged = records(&live.store);
    let (tx, _rx) = mpsc::channel(64);
    let provider = searching(vec![done(&[B::Text("two")]), done(&[B::Text("three")])], tx);
    let store = MemoryStore::default();
    let mut agent = resume_with(
        provider.clone(),
        default_session(),
        live._env.workspace(),
        "",
        &store,
        &logged,
    )
    .unwrap();
    assert!(agent.cache_rebuild().is_some());
    assert_eq!(
        agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
    one_turn(&mut agent).await;
    one_turn(&mut agent).await;
    assert!(searches(&provider.bodies()[0]));
    assert_eq!(provider.bodies()[0]["tools"], provider.bodies()[1]["tools"]);

    let mut combined = logged;
    combined.extend(records(&store));
    let (tx2, _rx2) = mpsc::channel(64);
    let provider2 = searching(vec![done(&[B::Text("four")])], tx2);
    let mut again = resume_with(
        provider2.clone(),
        default_session(),
        live._env.workspace(),
        "",
        &MemoryStore::default(),
        &combined,
    )
    .unwrap();
    assert!(again.cache_rebuild().is_none());
    one_turn(&mut again).await;
    assert!(searches(&provider2.bodies()[0]));
    assert_eq!(
        provider2.bodies()[0]["tools"],
        provider.bodies()[1]["tools"]
    );
}

#[tokio::test]
async fn gemini_and_openai_resumed_chats_pick_up_native_search_once() {
    use crate::endpoints::ProviderKind;
    for (kind, model, no_search, with_search, expected_kind) in [
        (
            ProviderKind::Gemini,
            "gemini-4-carbon",
            "[models.\"gemini-4-carbon\"]\nweb_search = []\n",
            "[models.\"gemini-4-carbon\"]\nweb_search = [\"google_search\"]\n",
            "google_search",
        ),
        (
            ProviderKind::OpenAi,
            "gpt-6-astra",
            "[models.\"gpt-6-astra\"]\nweb_search = []\n",
            "[models.\"gpt-6-astra\"]\nweb_search = [\"web_search\"]\n",
            "web_search",
        ),
    ] {
        let env = ScratchDir::new("search", &format!("pickup-{}", kind.as_str()));
        let store = MemoryStore::default();
        let mut session = default_session();
        session.model = model.into();
        let fake = |steps| {
            Fake::with(
                Knobs {
                    kind: Some(kind),
                    ..Knobs::default()
                },
                steps,
            )
        };
        let p1 = fake(vec![done(&[B::Text("one")])]);
        let mut live = probe_agent(
            p1.clone(),
            session.clone(),
            env.workspace(),
            no_search,
            SessionLog::new(Box::new(store.clone()), "s1"),
        );
        one_turn(&mut live).await;
        assert!(p1.sent()[0].server_tools.is_empty(), "{kind:?}");

        let logged = records(&store);
        let p2 = fake(vec![done(&[B::Text("two")]), done(&[B::Text("three")])]);
        let store2 = MemoryStore::default();
        let mut resumed = resume_with(
            p2.clone(),
            session.clone(),
            env.workspace(),
            with_search,
            &store2,
            &logged,
        )
        .unwrap();
        assert!(resumed.cache_rebuild().is_some(), "{kind:?}");
        assert_eq!(
            resumed.search_form(IMPLICIT_ENDPOINT, model),
            SearchForm::Native
        );
        one_turn(&mut resumed).await;
        one_turn(&mut resumed).await;
        assert_eq!(p2.sent()[0].server_tools.len(), 1, "{kind:?}");
        assert_eq!(p2.sent()[0].server_tools[0].kind, expected_kind, "{kind:?}");
        assert_eq!(
            p2.sent()[1].server_tools,
            p2.sent()[0].server_tools,
            "{kind:?}"
        );
        assert_eq!(p2.sent()[1].tools, p2.sent()[0].tools, "{kind:?}");

        let mut combined = logged;
        combined.extend(records(&store2));
        let p3 = fake(vec![done(&[B::Text("four")])]);
        let mut again = resume_with(
            p3.clone(),
            session,
            env.workspace(),
            with_search,
            &MemoryStore::default(),
            &combined,
        )
        .unwrap();
        assert!(again.cache_rebuild().is_none(), "{kind:?}");
        one_turn(&mut again).await;
        assert_eq!(
            p3.sent()[0].server_tools,
            p2.sent()[0].server_tools,
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn a_session_that_had_search_still_resumes_with_it_after_it_is_turned_off() {
    let mut live = still(native_rig(
        vec![done(&[B::Text("a")]), done(&[B::Text("b")])],
        &[DEFAULT_MODEL],
    ));
    live.turn("one").await;
    assert!(searches(&live.provider.bodies()[0]));
    let config = format!("{OFF}\n{}", listed("env", &[DEFAULT_MODEL]));
    let (mut agent, provider) = resumed_on(&live, &config).await.unwrap();
    assert_eq!(
        agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
    one_turn(&mut agent).await;
    live.turn("later").await;
    assert_eq!(provider.bodies()[0], live.provider.bodies()[1]);
}

#[tokio::test]
async fn a_new_session_takes_the_default_and_the_off_switch() {
    let mut on = rig(vec![done(&[B::Text("a")]), done(&[B::Text("b")])]);
    let placeholder = Agent::new(on.provider.clone(), default_session());
    on.agent = std::mem::replace(&mut on.agent, placeholder).with_web_search(true);
    on.turn("one").await;
    on.turn("two").await;
    let bodies = on.provider.bodies();
    assert!(searches(&bodies[0]));
    assert_eq!(bodies[1]["tools"], bodies[0]["tools"]);
    assert_eq!(bodies[1]["system"], bodies[0]["system"]);
    assert_eq!(
        bodies[1]["messages"][0], bodies[0]["messages"][0],
        "the first turn's message is a stable prefix"
    );
    let mut off = rig(vec![done(&[B::Text("a")])]);
    let placeholder = Agent::new(off.provider.clone(), default_session());
    off.agent = std::mem::replace(&mut off.agent, placeholder)
        .with_model_settings(models(OFF))
        .with_web_search(true);
    off.turn("one").await;
    assert!(!searches(&off.provider.bodies()[0]));
}

#[tokio::test]
async fn a_failed_mid_session_resend_logs_what_a_plain_failure_logs() {
    let mut control = still(native_rig(
        vec![
            done(&[B::Text("a")]),
            done(&[B::Text("b")]),
            plain_400(UNKNOWN_TOOL),
            done(&[B::Text("again")]),
        ],
        &[DEFAULT_MODEL],
    ));
    let mut rig = still(native_rig(
        vec![
            done(&[B::Text("a")]),
            done(&[B::Text("b")]),
            suspect(),
            plain_400("bad request"),
            done(&[B::Text("again")]),
        ],
        &[DEFAULT_MODEL],
    ));
    for t in ["one", "two", "three", "four"] {
        control.turn(t).await;
        rig.turn(t).await;
    }
    assert_eq!(rig.kinds(), control.kinds());
    assert!(!rig.kinds().contains(&"rebuild_marked".to_owned()));
    assert_eq!(rig.agent.epoch, control.agent.epoch);
    let bodies = rig.provider.bodies();
    let controls = control.provider.bodies();
    assert_eq!(bodies.last(), controls.last());
    assert_eq!(
        rig.provider.sent().pop().map(|s| s.rendering),
        control.provider.sent().pop().map(|s| s.rendering)
    );
}

#[tokio::test]
async fn a_successful_mid_session_resend_logs_what_the_organisation_refusal_logs() {
    let script = |first: Step| {
        vec![
            done(&[B::Think("hmm"), B::Text("a")]),
            first,
            done(&[B::Text("ok")]),
            done(&[B::Text("later")]),
        ]
    };
    let mut suspected = still(native_rig(script(suspect()), &[DEFAULT_MODEL]));
    let mut refused = still(native_rig(script(search_off()), &[DEFAULT_MODEL]));
    for rig in [&mut suspected, &mut refused] {
        for t in ["one", "two", "three"] {
            rig.turn(t).await;
        }
    }
    assert_eq!(suspected.kinds(), refused.kinds());
    assert!(suspected.kinds().contains(&"rebuild_marked".to_owned()));
    assert_eq!(suspected.provider.bodies(), refused.provider.bodies());
    assert_eq!(suspected.agent.epoch, refused.agent.epoch);
}

#[tokio::test]
async fn a_transient_failure_of_the_resend_is_retried_before_anything_is_marked() {
    let mut rig = native_rig(
        vec![
            suspect(),
            overloaded_before_any_event(),
            done(&[B::Text("ok")]),
            done(&[B::Text("again")]),
        ],
        &[DEFAULT_MODEL],
    );
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 3);
    assert!(searches(&bodies[0]));
    assert_eq!(bodies[2], bodies[1]);
    assert!(!searches(&bodies[2]));
    let retrying = index_of(&events, |e| matches!(e, AgentEvent::Retrying { .. }));
    let state = index_of(&events, |e| {
        matches!(e, AgentEvent::FeatureStateChanged { .. })
    });
    assert_eq!((retrying.len(), state.len()), (1, 1));
    assert!(retrying[0] < state[0]);
    assert_eq!(records_of(&rig, "native_web_search").len(), 1);
}

fn cancelled_before_any_event() -> Step {
    Step::from(vec![Ok(ProviderEvent::Sending)])
        .then(ClientCommand::Cancel)
        .hanging()
}

#[tokio::test]
async fn a_resend_cancelled_before_its_first_event_marks_nothing() {
    let mut rig = still(native_rig(
        vec![
            suspect(),
            cancelled_before_any_event(),
            done(&[B::Text("again")]),
        ],
        &[DEFAULT_MODEL],
    ));
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert!(records_of(&rig, "native_web_search").is_empty());
    assert!(tools_events(&events).is_empty());
    assert!(!rig.agent.probing());
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
    let (_, next) = rig.turn("next").await;
    assert!(tools_events(&next).is_empty());
    assert!(searches(&rig.provider.bodies()[2]));
    assert!(rig.provider.sent()[2].rendering.cuts.is_empty());
}

fn has_image(body: &Value) -> bool {
    body["messages"].to_string().contains("\"type\":\"image\"")
}

fn seeing_rig(steps: Vec<Step>) -> Rig {
    let mut rig = native_rig(steps, &[DEFAULT_MODEL]);
    let store = MemoryStore::default();
    rig.store = store.clone();
    let seeing = rig.provider.view(|k| {
        k.caps = Some(Caps::Fn(crate::provider::anthropic::capabilities::row));
        k.media = true;
        k.documented = None;
        k.wire_print = false;
    });
    rig.agent = Agent::new(seeing, default_session())
        .with_clock(TickingClock::new())
        .with_tools(
            Toolbox::new(
                samples()
                    .into_iter()
                    .map(|s| s.tool)
                    .chain([crate::tool::snap::tool()]),
            ),
            rig._env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_retry_policy(retry::quick())
        .with_log(SessionLog::new(Box::new(store), "s1"), implicit_context())
        .with_model_settings(models(&listed("env", &[DEFAULT_MODEL])))
        .with_web_search(true);
    rig
}

#[tokio::test]
async fn under_a_live_tail_a_suspect_still_withdraws_a_new_image() {
    let mut rig = seeing_rig(vec![
        uses(&[
            B::Search("s1", "rust"),
            B::Call("a", "snap", json!({"name": "shots/a.png"})),
        ]),
        suspect(),
        done(&[B::Found("s1"), B::Text("ok")]),
    ]);
    let (outcome, _) = rig.turn("one").await;
    let bodies = rig.provider.bodies();
    assert!(
        has_image(&bodies[1]),
        "{}",
        serde_json::to_string(&bodies[1]["messages"]).unwrap()
    );
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(bodies.len(), 3);
    assert!(
        has_image(&bodies[1]) && searches(&bodies[1]),
        "{}",
        bodies[1]["messages"]
    );
    assert!(!has_image(&bodies[2]), "{}", bodies[2]["messages"]);
    assert!(searches(&bodies[2]));
    assert!(records_of(&rig, "native_web_search").is_empty());
}

fn on_endpoints(provider: (Arc<Fake>, Arc<Fake>), start: &str, config: &str) -> Agent {
    let env = env("probe-moved");
    Agent::new(provider.0.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(
            Toolbox::new(samples().into_iter().map(|s| s.tool)),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_connections(
            vec![connection("a", provider.0), connection("b", provider.1)],
            Vec::new(),
            start,
        )
        .with_model_settings(models(config))
        .with_web_search(true)
}

#[tokio::test]
async fn a_resume_onto_another_endpoint_pins_only_the_logged_one() {
    let (tx, mut rx) = mpsc::channel(64);
    let a = searching(vec![done(&[B::Text("one")])], tx.clone());
    let b = searching(Vec::new(), tx.clone());
    let store = MemoryStore::default();
    let a_off = "[endpoints.a]\nnative_web_search = false\n";
    let mut live = on_endpoints((a.clone(), b.clone()), "a", a_off).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        connection("a", a.clone()).context,
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    live.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    assert!(!searches(&a.bodies()[0]));
    let records = records(&store);

    let a2 = searching(Vec::new(), tx.clone());
    let b2 = searching(vec![done(&[B::Text("two")])], tx);
    let b_off = "[endpoints.b]\nnative_web_search = false\n";
    let seq = u64::try_from(records.len()).unwrap();
    let mut resumed = on_endpoints((a2.clone(), b2.clone()), "b", b_off)
        .with_log(
            SessionLog::continuing(Box::new(MemoryStore::default()), "s1", seq),
            connection("b", b2.clone()).context,
        )
        .resume(&records, 0)
        .unwrap();
    assert_eq!(resumed.search_form("a", DEFAULT_MODEL), SearchForm::Native);
    assert_eq!(
        resumed.search_form("b", DEFAULT_MODEL),
        SearchForm::None(NoSearch::TurnedOff)
    );
    resumed
        .run_turn("two".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    assert!(!searches(&b2.bodies()[0]));
}

#[tokio::test]
async fn the_resume_check_compares_tools_on_the_logged_endpoint() {
    let (tx, mut rx) = mpsc::channel(64);
    let a = searching(vec![done(&[B::Text("one")])], tx.clone());
    let b = searching(Vec::new(), tx.clone());
    let store = MemoryStore::default();
    let mut live = on_endpoints((a.clone(), b.clone()), "a", "").with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        connection("a", a.clone()).context,
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    live.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    assert!(searches(&a.bodies()[0]));
    let records = records(&store);
    let seq = u64::try_from(records.len()).unwrap();
    let a2 = searching(Vec::new(), tx.clone());
    let b2 = searching(Vec::new(), tx);
    let b_off = "[endpoints.b]\nnative_web_search = false\n";
    let resumed = on_endpoints((a2, b2.clone()), "b", b_off)
        .with_log(
            SessionLog::continuing(Box::new(MemoryStore::default()), "s1", seq),
            connection("b", b2).context,
        )
        .resume(&records, 0)
        .unwrap();
    assert_eq!(resumed.search_form("a", DEFAULT_MODEL), SearchForm::Native);
    assert_ne!(resumed.search_form("b", DEFAULT_MODEL), SearchForm::Native);
    let notice = resumed.resume_notice_text().unwrap_or_default();
    assert!(!notice.contains("prompt or tools changed"), "{notice}");
}

fn overflow() -> Step {
    refused(classified(
        ErrorKind::ContextOverflow,
        "prompt is too long: 250000 tokens > 200000 maximum",
        FailurePhase::Connect,
        RetryClass::Never,
    ))
}

fn unmarked(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != "cache_control")
                .map(|(k, v)| (k.clone(), unmarked(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(unmarked).collect()),
        other => other.clone(),
    }
}

fn sized(mut step: Step, input: u64) -> Step {
    for event in &mut step.script {
        if let Ok(
            ProviderEvent::MessageStart { usage, .. } | ProviderEvent::MessageDelta { usage, .. },
        ) = event
        {
            usage.input_tokens = input;
        }
    }
    step
}

fn three_turns_then(rest: Vec<Step>) -> Vec<Step> {
    let mut steps = vec![
        sized(done(&[B::Text("one")]), 50_000),
        sized(done(&[B::Text("two")]), 100_000),
        sized(done(&[B::Text("three")]), 160_000),
    ];
    steps.extend(rest);
    steps
}

async fn after_three_turns(steps: Vec<Step>) -> (Rig, TurnOutcome, Vec<AgentEvent>) {
    let mut rig = still(native_rig(three_turns_then(steps), &[DEFAULT_MODEL]));
    for text in ["t1", "t2", "t3"] {
        rig.turn(text).await;
    }
    let (outcome, events) = rig.turn("t4").await;
    (rig, outcome, events)
}

fn epoch_of(agent: &Agent) -> (Vec<usize>, usize) {
    (
        agent.epoch.entries().iter().map(|e| e.view_len).collect(),
        agent.epoch.mark(),
    )
}

fn resumed_epoch(rig: &Rig) -> (Vec<usize>, usize) {
    let (tx, _rx) = mpsc::channel(64);
    let provider = searching(Vec::new(), tx);
    let agent = Agent::new(provider, default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(
            Toolbox::new(samples().into_iter().map(|s| s.tool)),
            rig._env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_model_settings(models(&listed("env", &[DEFAULT_MODEL])))
        .with_web_search(true);
    epoch_of(&resumed_with(
        agent,
        &records(&rig.store),
        implicit_context(),
    ))
}

fn turn_kinds(rig: &Rig) -> Vec<String> {
    let kinds = rig.kinds();
    let start = kinds
        .iter()
        .rposition(|k| k == "turn_started")
        .unwrap_or_default();
    kinds[start..].to_vec()
}

#[tokio::test]
async fn an_overflowing_resend_with_nothing_earlier_ends_as_too_large_and_keeps_search() {
    let mut plain = rig(vec![overflow()]);
    let (unprobed, _) = plain.turn("go").await;
    let mut rig = native_rig(
        vec![suspect(), overflow(), done(&[B::Text("summary")])],
        &[DEFAULT_MODEL],
    );
    let (outcome, events) = rig.turn("go").await;
    let TurnOutcome::Error {
        kind,
        message,
        gave_up,
        offer,
        ..
    } = &outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::ContextOverflow);
    assert_eq!(message, TOO_LARGE);
    assert_eq!(*gave_up, Some(GaveUp::NotRetryable));
    assert!(offer.is_none());
    let TurnOutcome::Error {
        kind: plain_kind,
        message: plain_message,
        ..
    } = &unprobed
    else {
        panic!("{unprobed:?}");
    };
    assert_eq!((plain_kind, plain_message), (kind, message));
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 2);
    assert!(searches(&bodies[0]));
    assert!(!searches(&bodies[1]));
    assert_eq!(bodies[1]["messages"], bodies[0]["messages"]);
    assert_eq!(
        turn_kinds(&rig),
        turn_kinds(&plain),
        "the probe leaves the log a plain overflow leaves"
    );
    assert!(records_of(&rig, "native_web_search").is_empty());
    assert!(!rig.kinds().contains(&"context_compacted".to_owned()));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { .. })),
        "{events:?}"
    );
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
    assert_eq!(epoch_of(&rig.agent), epoch_of(&plain.agent));
}

#[tokio::test]
async fn an_overflowing_resend_compacts_and_the_resend_then_resolves_the_probe() {
    let (mut rig, outcome, events) = after_three_turns(vec![
        suspect(),
        overflow(),
        compacted_step(),
        done(&[B::Text("four")]),
        done(&[B::Text("five")]),
    ])
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 7);
    let shape: Vec<(bool, usize)> = bodies[3..]
        .iter()
        .map(|b| (searches(b), b["messages"].as_array().map_or(0, Vec::len)))
        .collect();
    assert_eq!(shape, [(true, 7), (false, 7), (false, 3), (false, 5)]);
    assert_eq!(bodies[4]["messages"], bodies[3]["messages"]);
    assert_eq!(
        unmarked(&bodies[5]["messages"]),
        unmarked(&bodies[1]["messages"]),
        "the compaction request is the earlier request that fits"
    );
    assert!(bodies[6].to_string().contains("So far."), "{}", bodies[6]);
    assert!(bodies[6].to_string().contains("t4"), "{}", bodies[6]);
    let kinds = turn_kinds(&rig);
    let at = |kind: &str| -> Vec<usize> { position(&kinds, kind) };
    assert_eq!(at("context_compacted").len(), 1, "{kinds:?}");
    assert_eq!(at("native_web_search").len(), 1, "{kinds:?}");
    assert_eq!(at("message_committed").len(), 1, "{kinds:?}");
    assert!(at("context_compacted")[0] < at("native_web_search")[0]);
    assert!(at("native_web_search")[0] < at("message_committed")[0]);
    assert_eq!(refused_rebuilds(&events).len(), 1);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Overflow,
            ..
        }
    )));
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::None(NoSearch::Refused)
    );
    assert_eq!(epoch_of(&rig.agent), resumed_epoch(&rig));
    rig.turn("t5").await;
    assert!(!searches(rig.provider.bodies().last().unwrap()));
}

#[tokio::test]
async fn a_failed_resend_after_an_overflow_compaction_keeps_the_compacted_epoch() {
    let (control, _, _) =
        after_three_turns(vec![overflow(), compacted_step(), plain_400("bad request")]).await;
    let (mut rig, outcome, _) = after_three_turns(vec![
        suspect(),
        overflow(),
        compacted_step(),
        plain_400("bad request"),
        done(&[B::Text("five")]),
    ])
    .await;
    let TurnOutcome::Error { kind, message, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::InvalidRequest);
    assert!(message.contains(UNKNOWN_TOOL), "{message}");
    assert_eq!(rig.provider.bodies().len(), 7);
    assert_eq!(position(&turn_kinds(&rig), "context_compacted").len(), 1);
    assert!(records_of(&rig, "native_web_search").is_empty());
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
    assert_eq!(epoch_of(&rig.agent), epoch_of(&control.agent));
    assert_eq!(epoch_of(&rig.agent), resumed_epoch(&rig));
    rig.turn("t5").await;
    assert!(searches(rig.provider.bodies().last().unwrap()));
}

#[tokio::test]
async fn a_resend_that_stays_overloaded_ends_with_the_overload_and_a_retry_offer() {
    let mut steps = vec![suspect()];
    steps.extend((0..20).map(|_| overloaded_before_any_event()));
    let mut rig = native_rig(steps, &[DEFAULT_MODEL]);
    let (outcome, _) = rig.turn("go").await;
    let TurnOutcome::Error {
        kind,
        message,
        attempts,
        gave_up,
        offer,
        detail,
        ..
    } = &outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::Overloaded);
    assert!(!message.contains(UNKNOWN_TOOL), "{message}");
    assert!(*attempts > 1, "{outcome:?}");
    assert!(gave_up.is_some(), "{outcome:?}");
    assert!(offer.is_some(), "{outcome:?}");
    assert_eq!(
        detail.as_ref().map(|d| d.retry),
        Some(RetryClass::Transient),
        "{outcome:?}"
    );
    let bodies = rig.provider.bodies();
    assert!(bodies.len() > 2);
    assert!(searches(&bodies[0]));
    assert!(bodies[1..].iter().all(|b| b == &bodies[1]));
    assert!(!searches(&bodies[1]));
    assert!(records_of(&rig, "native_web_search").is_empty());
    assert_eq!(
        rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
}

#[tokio::test]
async fn a_resend_refused_as_invalid_ends_with_the_first_error() {
    let mut rig = native_rig(
        vec![suspect(), plain_400("messages.0: something else")],
        &[DEFAULT_MODEL],
    );
    let (outcome, _) = rig.turn("go").await;
    let TurnOutcome::Error { kind, message, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::InvalidRequest);
    assert!(message.contains(UNKNOWN_TOOL), "{message}");
    assert!(!message.contains("something else"), "{message}");
    assert_eq!(rig.sent(), 2);
}

#[tokio::test]
async fn unreadable_results_converted_inside_a_probe_that_then_ends_name_the_next_miss() {
    let unreadable = "messages.1.content.0: Invalid `encrypted_content` in `search_result` block";
    let mut rig = still(native_rig(
        vec![
            uses(&[
                B::Think("t1"),
                B::Search("s1", "rust"),
                B::Found("s1"),
                echo("e1"),
            ]),
            done(&[B::Text("a")]),
            suspect(),
            refused(refusal(Refused::CrossAccount, unreadable)),
            plain_400("something else"),
        ],
        &[DEFAULT_MODEL],
    ));
    rig.turn("one").await;
    let (outcome, _) = rig.turn("two").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(rig.sent(), 5);
    assert!(!rig.agent.probing());
    assert_eq!(position(&rig.kinds(), "search_history_unreadable").len(), 1);
    assert_eq!(
        rig.agent.epoch.take_expected(),
        Some("search_history_unreadable")
    );
}

const NEW_WORDING: &str = "tools.1: the gateway no longer takes this";

fn ladder_refusal(kind: ErrorKind, message: &str) -> Step {
    refused(
        classified(kind, message, FailurePhase::Connect, RetryClass::Never)
            .refusing(Refused::Rendering),
    )
}

fn ladder_rig(refusal: impl Fn() -> Step, last: Option<Step>) -> Rig {
    let mut steps = vec![stopped_with_thinking(), refusal(), refusal(), refusal()];
    steps.extend(last);
    still(native_rig(steps, &[DEFAULT_MODEL]))
}

#[tokio::test]
async fn a_converted_history_refused_in_a_new_wording_climbs_the_ladder_then_probes_and_falls_back()
{
    let mut rig = ladder_rig(
        || ladder_refusal(ErrorKind::InvalidRequest, NEW_WORDING),
        Some(done(&[B::Text("ok")])),
    );
    rig.turn("go").await;
    let (outcome, _) = rig.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 5);
    assert!(bodies[1..4].iter().all(searches));
    assert!(!searches(&bodies[4]));
    assert_eq!(
        records_of(&rig, "native_web_search"),
        vec![LogEvent::NativeWebSearch {
            endpoint_name: IMPLICIT_ENDPOINT.into(),
            state: FeatureState::FallenBack,
        }]
    );
    assert!(position(&rig.kinds(), "rendering_form").is_empty());
}

#[tokio::test]
async fn a_probe_refused_after_the_ladder_goes_once_in_the_kept_form_and_ends_the_turn() {
    let mut steps = vec![stopped_with_thinking()];
    steps.extend((0..6).map(|_| ladder_refusal(ErrorKind::InvalidRequest, NEW_WORDING)));
    let mut rig = still(native_rig(steps, &[DEFAULT_MODEL]));
    rig.turn("go").await;
    let (outcome, _) = rig.turn("next").await;
    let TurnOutcome::Error { kind, message, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::InvalidRequest);
    assert!(message.contains(NEW_WORDING), "{message}");
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 5);
    assert!(!searches(&bodies[4]));
    let tool_turns = |b: &Value| b["messages"].to_string().contains("\"tool_use\"");
    assert!(!tool_turns(&bodies[3]));
    assert!(tool_turns(&bodies[4]));
    assert_eq!(bodies[4]["messages"][2], bodies[1]["messages"][2]);
}

#[tokio::test]
async fn a_thinking_refusal_the_ladder_hands_back_through_a_gateway_is_cut_and_never_probed() {
    let binding =
        "Invalid `signature` in `thinking` block. The block is bound to a different conversation";
    let mut rig = ladder_rig(
        || ladder_refusal(ErrorKind::ThinkingBinding, binding),
        Some(done(&[B::Text("ok")])),
    );
    rig.turn("go").await;
    let (outcome, _) = rig.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 5);
    assert!(bodies[1..].iter().all(searches));
    assert!(records_of(&rig, "native_web_search").is_empty());
    let whole = records_of(&rig, "thinking_cut")
        .into_iter()
        .filter(|e| {
            matches!(e, LogEvent::ThinkingCut { from, .. } if *from == wisp_protocol::log::BlockPosition { message: 0, block: 0 })
        })
        .count();
    assert_eq!(whole, 1);
}

#[tokio::test]
async fn a_new_wording_on_a_documented_endpoint_climbs_only_the_ladder() {
    let mut rig = ladder_rig(
        || ladder_refusal(ErrorKind::InvalidRequest, NEW_WORDING),
        None,
    );
    rig.turn("go").await;
    rig.provider.knobs(|k| k.documented = Some(true));
    let (outcome, _) = rig.turn("next").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 4);
    assert!(bodies[1..].iter().all(searches));
    assert!(records_of(&rig, "native_web_search").is_empty());
}

#[tokio::test]
async fn a_gateway_that_refuses_search_and_attachments_recovers_both() {
    const TEXT_MODEL: &str = "text-model";
    let mut rig = native_rig(
        vec![
            done(&[B::Text("seen")]),
            suspect(),
            refused(crate::agent::testing::content_refusal(
                "Could not process image",
            )),
            suspect(),
            done(&[B::Text("ok")]),
        ],
        &[DEFAULT_MODEL, TEXT_MODEL],
    );
    rig.provider
        .knobs(|k| k.caps = Some(Caps::Only(&[DEFAULT_MODEL])));
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    let image = wisp_protocol::ImageData {
        data: RED_PIXEL.into(),
    };
    let first = rig
        .agent
        .run_user_turn("look".into(), vec![image], &mut rig.rx, &ev_tx)
        .await;
    assert!(
        matches!(first, Some(TurnOutcome::Completed { .. })),
        "{first:?}"
    );
    rig.agent
        .change_settings(None, Some(TEXT_MODEL.into()), None, &ev_tx)
        .await;
    let (outcome, _) = rig.turn("what does it say?").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 5);
    let noted = |n: usize| {
        !bodies[n]["messages"]
            .to_string()
            .contains(r#""type":"image""#)
    };
    assert_eq!(
        (1..5)
            .map(|n| (searches(&bodies[n]), noted(n)))
            .collect::<Vec<_>>(),
        [(true, false), (false, false), (true, true), (false, true)]
    );
    assert_eq!(records_of(&rig, "native_web_search").len(), 1);
    assert_eq!(
        records_of(&rig, "attachments_as_notes"),
        vec![LogEvent::AttachmentsAsNotes {
            endpoint_name: IMPLICIT_ENDPOINT.into(),
            model: TEXT_MODEL.into(),
        }]
    );
}
