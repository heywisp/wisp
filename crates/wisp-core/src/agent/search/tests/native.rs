use wisp_protocol::log::{BlockPosition, Cut, FeatureState};
use wisp_protocol::{CacheEffect, Feature, RebuildReason, RenderingForm, RequestId, ToolChangeVia};

use super::*;
use crate::agent::tests::kinds;
use crate::clock::StillClock;
use crate::models::ModelTable;
use crate::provider::Refused;
use crate::search::{NoSearch, SearchForm};
use crate::test_guard::models;
use crate::tool::tools_json;

pub(super) const HAIKU: &str = "claude-haiku-5-5";

pub(super) fn listed(endpoint: &str, ids: &[&str]) -> String {
    let shipped = ModelTable::shipped().unwrap();
    let others = shipped
        .rows()
        .iter()
        .map(|row| row.id.as_str())
        .filter(|id| !ids.contains(id))
        .map(|id| format!("[endpoints.{endpoint}.models.\"{id}\"]\nweb_search = []\n"));
    ids.iter()
        .map(|id| {
            format!(
                "[endpoints.{endpoint}.models.\"{id}\"]\nweb_search = [\"web_search_20250305\"]\n"
            )
        })
        .chain(others)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn native_rig(steps: Vec<Step>, ids: &[&str]) -> Rig {
    let mut rig = rig(steps);
    let placeholder = Agent::new(rig.provider.clone(), default_session());
    let agent = std::mem::replace(&mut rig.agent, placeholder);
    rig.agent = agent
        .with_model_settings(models(&listed("env", ids)))
        .with_web_search(true);
    rig
}

pub(super) fn refusal(refused: Refused, message: &str) -> ProviderError {
    classified(
        ErrorKind::InvalidRequest,
        message,
        FailurePhase::Connect,
        RetryClass::Never,
    )
    .refusing(refused)
}

fn cross_account() -> Step {
    refused(refusal(
        Refused::CrossAccount,
        "messages.1.content.1.content.0.encrypted_content: invalid",
    ))
}

fn native_entry() -> Value {
    json!({"name": "web_search", "type": "web_search_20250305"})
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn searches(body: &Value) -> bool {
    tool_names(body).iter().any(|n| n == "web_search")
}

pub(super) fn digest(rig: &Rig, form: SearchForm) -> String {
    crate::session::sha256_hex(&tools_json(
        rig.agent.toolbox().specs(),
        rig.agent
            .search_tools(rig.agent.endpoint_name(), &rig.agent.session.model, form),
    ))
}

pub(super) fn without_search(rig: &Rig) -> Vec<String> {
    rig.agent.toolbox().names()
}

pub(super) fn tools_events(events: &[AgentEvent]) -> Vec<(Vec<String>, Vec<String>)> {
    pick!(
        events,
        AgentEvent::ToolsChanged {
            added,
            removed,
            via: ToolChangeVia::Rebuild,
            ..
        } => (added.clone(), removed.clone())
    )
}

pub(super) fn index_of(events: &[AgentEvent], wanted: impl Fn(&AgentEvent) -> bool) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| wanted(e))
        .map(|(i, _)| i)
        .collect()
}

pub(super) fn refused_rebuilds(events: &[AgentEvent]) -> Vec<(String, Option<RequestId>)> {
    pick!(
        events,
        AgentEvent::SettingsChanged {
            model,
            answers,
            cache:
                CacheEffect::Rebuilt {
                    reason: RebuildReason::NativeSearchRefused,
                },
            ..
        } => (model.clone(), *answers)
    )
}

pub(super) fn tools_cuts(rig: &Rig) -> Vec<(BlockPosition, usize, String)> {
    pick!(
        records_of(rig, "thinking_cut"),
        LogEvent::ThinkingCut {
            from,
            through,
            tools_sha256: Some(sha),
        } => (from, through, sha)
    )
}

pub(super) fn logged_tools(rig: &Rig) -> Vec<(Vec<String>, String)> {
    log_events(&rig.store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::SessionStarted {
                tools,
                tools_sha256,
                ..
            } => Some((tools, tools_sha256.unwrap_or_default())),
            LogEvent::SettingsChanged {
                tools,
                tools_sha256: Some(sha),
                ..
            } => Some((tools, sha)),
            _ => None,
        })
        .collect()
}

pub(super) fn origin() -> BlockPosition {
    BlockPosition {
        message: 0,
        block: 0,
    }
}

#[tokio::test]
async fn a_native_pair_sends_the_same_tools_on_every_step_and_turn() {
    let mut rig = native_rig(
        vec![
            uses(&[echo("e1")]),
            done(&[B::Text("ok")]),
            paused(&[B::Search("s1", "rust")]),
            done(&[B::Found("s1"), B::Text("again")]),
        ],
        &[DEFAULT_MODEL],
    );
    rig.turn("go").await;
    rig.turn("next").await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 4);
    let names = tool_names(&bodies[0]);
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert!(
        bodies[0]["tools"]
            .as_array()
            .unwrap()
            .contains(&native_entry())
    );
    for body in &bodies {
        assert_eq!(body["tools"], bodies[0]["tools"]);
        assert_eq!(body["system"], bodies[0]["system"]);
    }
    let logged = logged_tools(&rig);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].0, names);
    assert_eq!(logged[0].1, digest(&rig, SearchForm::Native));
    assert!(tools_cuts(&rig).is_empty());
}

#[tokio::test]
async fn turning_search_on_drops_the_logged_mark_and_leaves_the_form_the_facts_give() {
    let off = "[endpoints.env]\nnative_web_search = false\n";
    let logged = [
        ("", false, SearchForm::None(NoSearch::NotStarted)),
        (off, true, SearchForm::Native),
    ];
    for (config, had, before) in logged {
        let mut rig = native_rig(Vec::new(), &[DEFAULT_MODEL]);
        rig.agent.models = models(&format!("{}\n{config}", listed("env", &[DEFAULT_MODEL])));
        let pair = (IMPLICIT_ENDPOINT.to_owned(), DEFAULT_MODEL.to_owned());
        rig.agent.search.kept.insert(pair, had);
        let form = |rig: &Rig| rig.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL);
        assert_eq!(form(&rig), before, "{config}");
        rig.agent.flip_search(false);
        assert_eq!(form(&rig), SearchForm::None(NoSearch::NotRequested));
        rig.agent.flip_search(true);
        let facts = rig
            .agent
            .unkept_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL, true);
        assert_eq!(form(&rig), facts, "{config}");
        assert_ne!(form(&rig), before, "{config}");
    }
}

#[tokio::test]
async fn a_gateway_sends_search_unless_left_out_or_turned_off() {
    let mut off = rig(vec![done(&[B::Text("ok")])]);
    let placeholder = Agent::new(off.provider.clone(), default_session());
    let agent = std::mem::replace(&mut off.agent, placeholder);
    off.agent = agent
        .with_model_settings(models(&listed("env", &[DEFAULT_MODEL])))
        .with_web_search(false);
    off.turn("go").await;
    assert!(!searches(&off.provider.bodies()[0]));

    let mut turned_off = rig(vec![done(&[B::Text("ok")])]);
    let placeholder = Agent::new(turned_off.provider.clone(), default_session());
    let agent = std::mem::replace(&mut turned_off.agent, placeholder);
    turned_off.agent = agent
        .with_model_settings(models("[endpoints.env]\nnative_web_search = false\n"))
        .with_web_search(true);
    turned_off.turn("go").await;
    assert!(!searches(&turned_off.provider.bodies()[0]));
    assert_eq!(
        turned_off
            .agent
            .search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::None(NoSearch::TurnedOff)
    );

    let mut gateway = rig(vec![done(&[B::Text("ok")])]);
    let placeholder = Agent::new(gateway.provider.clone(), default_session());
    let agent = std::mem::replace(&mut gateway.agent, placeholder);
    gateway.agent = agent.with_web_search(true);
    gateway.turn("go").await;
    assert!(searches(&gateway.provider.bodies()[0]));
    assert_eq!(
        gateway.agent.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
        SearchForm::Native
    );
}

#[tokio::test]
async fn a_refusal_on_the_first_request_resends_without_search_for_good() {
    for started in [false, true] {
        let first = if started {
            failing(
                &[B::Search("s1", "q")],
                classified(
                    ErrorKind::InvalidRequest,
                    "search refused",
                    FailurePhase::Response,
                    RetryClass::Never,
                )
                .refusing(crate::provider::Refused::NativeSearch),
            )
        } else {
            search_off()
        };
        let mut rig = native_rig(
            vec![first, done(&[B::Text("ok")]), done(&[B::Text("again")])],
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
        assert_eq!(state.len(), 1);
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(tools.len(), 1);
        assert!(state[0] < rebuilt[0] && rebuilt[0] < tools[0]);
        assert_eq!(
            tools_events(&events),
            vec![(Vec::new(), vec!["web_search".to_owned()])]
        );
        assert!(matches!(
            &events[state[0]],
            AgentEvent::FeatureStateChanged { endpoint, model, .. }
                if endpoint == IMPLICIT_ENDPOINT && model == DEFAULT_MODEL
        ));

        let kinds = rig.kinds();
        let record = position(&kinds, "native_web_search");
        let cut = position(&kinds, "thinking_cut");
        let changed = position(&kinds, "settings_changed");
        assert_eq!(record.len(), 1);
        assert_eq!(changed.len(), 1);
        assert_eq!(cut.len(), 1);
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
        let records = records(&rig.store);
        let rebuilt = rebuild_history(records.iter().map(|r| &r.event), no_images).unwrap();
        assert_eq!(
            rebuilt.search_refused,
            BTreeSet::from([IMPLICIT_ENDPOINT.to_owned()])
        );
        let resumed = Agent::new(rig.provider.clone(), default_session())
            .with_model_settings(models(&listed("env", &[DEFAULT_MODEL])))
            .with_web_search(true)
            .resumed(rebuilt, &records);
        assert_eq!(
            resumed.search_form(IMPLICIT_ENDPOINT, DEFAULT_MODEL),
            SearchForm::None(NoSearch::Refused)
        );
        assert_resumes_byte_for_byte(&rig);
        if started {
            assert_eq!(ended(&events).len(), 1);
            assert!(matches!(ended(&events)[0].1, SearchEnd::Unfinished { .. }));
        }
    }
}

#[tokio::test]
async fn a_refusal_after_thinking_cuts_all_of_it_and_keeps_the_replies_after() {
    let mut rig = native_rig(
        vec![
            done(&[B::Think("t1"), B::Text("a")]),
            search_off(),
            done(&[B::Think("t2"), B::Text("b")]),
            done(&[B::Text("c")]),
        ],
        &[DEFAULT_MODEL],
    );
    rig.turn("one").await;
    rig.turn("two").await;
    rig.turn("three").await;
    let refused = rig.messages(1);
    let resent = rig.messages(2);
    let later = rig.messages(3);
    assert_eq!(refused[1]["content"][0]["type"], "thinking");
    assert_eq!(resent[1]["content"], json!([{"type": "text", "text": "a"}]));
    assert_eq!(later[1]["content"], json!([{"type": "text", "text": "a"}]));
    assert_eq!(later[3]["content"][0]["type"], "thinking");
    assert_eq!(resent[0], refused[0]);
    let none = digest(&rig, SearchForm::None(NoSearch::Refused));
    assert_eq!(tools_cuts(&rig), vec![(origin(), 1, none)]);
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_refusal_of_the_results_request_closes_the_search_and_goes_on() {
    let mut rig = native_rig(
        vec![
            uses(&[B::Search("s1", "rust"), echo("e1")]),
            search_off(),
            done(&[B::Text("ok")]),
        ],
        &[DEFAULT_MODEL],
    );
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 3);
    assert_eq!(
        ended(&events),
        vec![(
            "s1".to_owned(),
            unfinished(UnfinishedReason::Refused, false)
        )]
    );
    let refused = rig.messages(1);
    let resent = rig.messages(2);
    assert_eq!(refused[1]["content"][0]["type"], "server_tool_use");
    assert_eq!(resent[1]["content"][0], closed_text("rust"));
    assert_eq!(resent[2], refused[2]);
    assert!(!searches(&rig.provider.bodies()[2]));
    let kinds = rig.kinds();
    let record = position(&kinds, "native_web_search");
    let closed = position(&kinds, "tail_closed");
    assert_eq!(closed.len(), 1);
    assert!(record[0] < closed[0]);
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_refused_pause_continuation_ends_the_turn_as_at_the_cap() {
    let mut rig = native_rig(
        vec![
            paused(&[B::Search("s1", "rust")]),
            search_off(),
            done(&[B::Text("next")]),
        ],
        &[DEFAULT_MODEL],
    );
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::PauseTurn,
                committed: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 2);
    assert_eq!(
        ended(&events),
        vec![(
            "s1".to_owned(),
            unfinished(UnfinishedReason::Refused, false)
        )]
    );
    assert_eq!(tools_events(&events).len(), 1);
    rig.turn("again").await;
    let body = &rig.provider.bodies()[2];
    assert!(!searches(body));
    assert_eq!(body["messages"][1]["content"][0], closed_text("rust"));
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_switch_pending_at_the_refusal_gets_one_unasked_rebuild_for_its_target() {
    let switch = ClientCommand::shortcut(RequestId(7), None, Some(HAIKU.into()), None);
    let mut rig = native_rig(
        vec![
            uses(&[echo("e1")]).then(switch),
            search_off(),
            done(&[B::Text("ok")]),
            done(&[B::Text("on haiku")]),
        ],
        &[DEFAULT_MODEL, HAIKU],
    );
    let (_, events) = rig.turn("go").await;
    assert_eq!(refused_rebuilds(&events), vec![(HAIKU.to_owned(), None)]);
    let answered = index_of(&events, |e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(7)),
                ..
            }
        )
    });
    assert_eq!(answered.len(), 1);
    let changed = records_of(&rig, "settings_changed");
    assert_eq!(changed.len(), 2);
    assert!(matches!(
        &changed[0],
        LogEvent::SettingsChanged { model, tools, tools_sha256: None, .. }
            if model == HAIKU && tools.is_empty()
    ));
    let none = digest(&rig, SearchForm::None(NoSearch::Refused));
    assert!(matches!(
        &changed[1],
        LogEvent::SettingsChanged { model, tools_sha256: Some(sha), .. }
            if model == HAIKU && *sha == none
    ));
    assert!(
        answered[0]
            < index_of(&events, |e| matches!(
                e,
                AgentEvent::FeatureStateChanged { .. }
            ))[0]
    );
    let (_, next) = rig.turn("next").await;
    let body = &rig.provider.bodies()[3];
    assert_eq!(body["model"], HAIKU);
    assert!(!searches(body));
    assert!(tools_events(&next).is_empty());
}

#[tokio::test]
async fn a_form_changing_switch_cuts_thinking_and_going_back_restores_the_tools() {
    let mut rig = native_rig(
        vec![
            done(&[B::Think("t1"), B::Text("a")]),
            done(&[B::Think("t2"), B::Text("b")]),
            done(&[B::Text("c")]),
            done(&[B::Text("d")]),
        ],
        &[DEFAULT_MODEL],
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    rig.turn("one").await;
    rig.agent
        .change_settings(None, Some(HAIKU.into()), None, &ev_tx)
        .await;
    let (_, second) = rig.turn("two").await;
    rig.agent
        .change_settings(None, Some(DEFAULT_MODEL.into()), None, &ev_tx)
        .await;
    let (_, third) = rig.turn("three").await;
    let (_, fourth) = rig.turn("four").await;
    let bodies = rig.provider.bodies();
    assert!(searches(&bodies[0]));
    assert!(!searches(&bodies[1]));
    assert!(searches(&bodies[2]));
    assert_eq!(bodies[2]["tools"], bodies[0]["tools"]);
    assert_eq!(bodies[3]["tools"], bodies[0]["tools"]);
    assert_eq!(
        tools_events(&second),
        vec![(Vec::new(), vec!["web_search".to_owned()])]
    );
    assert_eq!(
        tools_events(&third),
        vec![(vec!["web_search".to_owned()], Vec::new())]
    );
    assert!(tools_events(&fourth).is_empty());
    assert_eq!(
        rig.messages(1)[1]["content"],
        json!([{"type": "text", "text": "a"}])
    );
    assert_eq!(
        rig.messages(2)[1]["content"],
        json!([{"type": "text", "text": "a"}])
    );
    assert_eq!(
        rig.messages(2)[3]["content"],
        json!([{"type": "text", "text": "b"}])
    );
    let native = digest(&rig, SearchForm::Native);
    let none = digest(&rig, SearchForm::None(NoSearch::NotSupported));
    assert_eq!(
        tools_cuts(&rig),
        vec![(origin(), 1, none.clone()), (origin(), 3, native.clone())]
    );
    let logged: Vec<String> = logged_tools(&rig).into_iter().map(|(_, sha)| sha).collect();
    assert_eq!(logged, vec![native.clone(), none, native]);
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn going_back_before_the_switch_applies_logs_the_original_tools_again() {
    let mut rig = native_rig(
        vec![done(&[B::Text("a")]), done(&[B::Text("b")])],
        &[DEFAULT_MODEL],
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    rig.turn("one").await;
    rig.agent
        .change_settings(None, Some(HAIKU.into()), None, &ev_tx)
        .await;
    rig.agent
        .change_settings(None, Some(DEFAULT_MODEL.into()), None, &ev_tx)
        .await;
    let (_, events) = rig.turn("two").await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies[1]["tools"], bodies[0]["tools"]);
    assert!(tools_events(&events).is_empty());
    assert!(tools_cuts(&rig).is_empty());
    let native = digest(&rig, SearchForm::Native);
    let logged: Vec<String> = logged_tools(&rig).into_iter().map(|(_, sha)| sha).collect();
    assert_eq!(
        logged,
        vec![
            native.clone(),
            digest(&rig, SearchForm::None(NoSearch::NotSupported)),
            native
        ]
    );
}

fn two_endpoints(
    first: Vec<Step>,
    second: Vec<Step>,
) -> (
    Agent,
    Arc<Fake>,
    Arc<Fake>,
    mpsc::Receiver<ClientCommand>,
    ScratchDir,
    MemoryStore,
) {
    let env = env("cross");
    let (tx, rx) = mpsc::channel(64);
    let a = searching(first, tx.clone());
    let b = searching(second, tx);
    let tools: Vec<crate::tool::ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    let config = format!(
        "{}\n{}",
        listed("a", &[DEFAULT_MODEL]),
        listed("b", &[DEFAULT_MODEL])
    );
    let agent = Agent::new(a.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(Toolbox::new(tools), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_connections(
            vec![connection("a", a.clone()), connection("b", b.clone())],
            Vec::new(),
            "a",
        )
        .with_model_settings(models(&config))
        .with_web_search(true)
        .with_retry_policy(retry::quick());
    let store = MemoryStore::default();
    let context = connection("a", a.clone()).context;
    let agent = agent.with_log(SessionLog::new(Box::new(store.clone()), "s1"), context);
    (agent, a, b, rx, env, store)
}

#[tokio::test]
async fn an_encrypted_refusal_after_a_switch_renders_the_other_endpoints_searches() {
    let (mut agent, a, b, mut rx, _env, _store) = two_endpoints(
        vec![done(&[
            B::Think("t1"),
            B::Search("s1", "rust"),
            B::Found("s1"),
            B::Text("found"),
        ])],
        vec![
            cross_account(),
            done(&[B::Search("s2", "more"), B::Found("s2"), B::Text("b")]),
            done(&[B::Text("c")]),
        ],
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    agent.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .change_settings(Some("b".into()), None, None, &ev_tx)
        .await;
    let outcome = agent.run_turn("two".into(), &mut rx, &ev_tx).await.unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    agent
        .run_turn("three".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    let sent = b.bodies();
    assert_eq!(sent.len(), 3);
    assert_eq!(a.bodies()[0]["tools"], sent[0]["tools"]);
    assert!(searches(&sent[1]));
    assert_eq!(
        sent[0]["messages"][1]["content"][1]["type"],
        "server_tool_use"
    );
    let rendered = &sent[1]["messages"][1]["content"];
    assert_eq!(rendered[0], sent[0]["messages"][1]["content"][0]);
    assert_eq!(
        rendered[1],
        json!({"type": "text", "text": "[web search: \"rust\"]"})
    );
    assert_eq!(
        rendered[2],
        json!({"type": "text", "text": "1. Page s1 \u{2014} https://example.com/s1"})
    );
    assert_eq!(sent[2]["messages"][1], sent[1]["messages"][1]);
    assert_eq!(
        sent[2]["messages"][3]["content"][0]["type"],
        "server_tool_use"
    );
    assert!(agent.search.rendered_on.contains("b"));
}

#[tokio::test]
async fn rendering_the_other_endpoints_searches_says_so_once_naming_the_endpoint() {
    let (mut agent, _a, _b, mut rx, _env, _store) = two_endpoints(
        vec![done(&[
            B::Search("s1", "rust"),
            B::Found("s1"),
            B::Think("t1"),
            B::Text("found"),
        ])],
        vec![
            cross_account(),
            done(&[B::Text("b")]),
            done(&[B::Text("c")]),
        ],
    );
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    agent.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .change_settings(Some("b".into()), None, None, &ev_tx)
        .await;
    agent.run_turn("two".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .run_turn("three".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    let mut events = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        events.push(e);
    }
    assert_eq!(agent.search.cuts.len(), 1);
    assert_eq!(
        adaptations(&events),
        vec![(
            Some("b".to_owned()),
            wisp_protocol::HistoryChange::SearchesAsText { thinking_cut: true }
        )]
    );
}

pub(super) fn resumed_with(agent: Agent, records: &[LogRecord], context: LogContext) -> Agent {
    agent
        .with_log(continued(records).1, context)
        .resume(records, 0)
        .unwrap()
}

#[tokio::test]
async fn a_resumed_session_keeps_search_off_where_it_was_refused() {
    let mut live = native_rig(
        vec![
            search_off(),
            done(&[B::Text("ok")]),
            done(&[B::Text("again")]),
        ],
        &[DEFAULT_MODEL],
    );
    let placeholder = Agent::new(live.provider.clone(), default_session());
    live.agent = std::mem::replace(&mut live.agent, placeholder).with_clock(Arc::new(StillClock));
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
    let outcome = agent
        .run_turn("next".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(!searches(&provider.bodies()[0]));
    assert_eq!(live.provider.bodies()[2], provider.bodies()[0]);
}

#[tokio::test]
async fn a_resumed_session_keeps_rendering_the_other_endpoints_searches() {
    let (agent, _, b, mut rx, env, store) = two_endpoints(
        vec![done(&[
            B::Think("t1"),
            B::Search("s1", "rust"),
            B::Found("s1"),
            B::Text("found"),
        ])],
        vec![
            cross_account(),
            done(&[B::Search("s2", "more"), B::Found("s2"), B::Text("b")]),
            done(&[B::Text("c")]),
        ],
    );
    let mut agent = agent.with_clock(Arc::new(StillClock));
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    agent.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .change_settings(Some("b".into()), None, None, &ev_tx)
        .await;
    agent.run_turn("two".into(), &mut rx, &ev_tx).await.unwrap();
    let records = records(&store);
    agent
        .run_turn("three".into(), &mut rx, &ev_tx)
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel(64);
    let a2 = searching(Vec::new(), tx.clone());
    let b2 = searching(vec![done(&[B::Text("c")])], tx);
    let config = format!(
        "{}\n{}",
        listed("a", &[DEFAULT_MODEL]),
        listed("b", &[DEFAULT_MODEL])
    );
    let fresh = Agent::new(b2.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(
            Toolbox::new(samples().into_iter().map(|s| s.tool)),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_connections(
            vec![connection("a", a2), connection("b", b2.clone())],
            Vec::new(),
            "b",
        )
        .with_model_settings(models(&config))
        .with_web_search(true)
        .with_retry_policy(retry::quick());
    let context = connection("b", b2.clone()).context;
    let mut again = resumed_with(fresh, &records, context);
    assert!(again.search.rendered_on.contains("b"));
    let outcome = again
        .run_turn("three".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(b.bodies()[2], b2.bodies()[0]);
}

#[tokio::test]
async fn an_encrypted_refusal_with_nothing_foreign_ends_the_turn_with_its_error() {
    let mut rig = native_rig(vec![cross_account()], &[DEFAULT_MODEL]);
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 1);
    assert!(rig.agent.search.rendered_on.is_empty());
}

#[tokio::test]
async fn a_second_encrypted_refusal_of_a_rendered_request_moves_to_the_next_form() {
    let (mut agent, _a, b, mut rx, _env, _store) = two_endpoints(
        vec![done(&[
            B::Search("s1", "rust"),
            B::Found("s1"),
            B::Text("a"),
        ])],
        vec![cross_account(), cross_account(), done(&[B::Text("b")])],
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    agent.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .change_settings(Some("b".into()), None, None, &ev_tx)
        .await;
    let outcome = agent.run_turn("two".into(), &mut rx, &ev_tx).await.unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(b.bodies().len(), 3);
    assert_eq!(agent.search.form, RenderingForm::WithoutThinking);
}

#[tokio::test]
async fn an_encrypted_refusal_of_a_rendered_request_with_nothing_foreign_moves_to_the_next_form() {
    let mut rig = native_rig(
        vec![
            stopped_with_thinking(),
            cross_account(),
            done(&[B::Text("ok")]),
        ],
        &[DEFAULT_MODEL],
    );
    rig.turn("go").await;
    let (outcome, _) = rig.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 3);
    assert!(rig.agent.search.rendered_on.is_empty());
    assert_eq!(rig.agent.search.form, RenderingForm::WithoutThinking);
    assert_eq!(rig.messages(2)[1]["content"][0], closed_text("rust"));
}

#[tokio::test]
async fn the_first_foreign_search_is_found_past_the_endpoints_own() {
    let (mut agent, _a, b, mut rx, _env, _store) = two_endpoints(
        vec![done(&[
            B::Search("s1", "rust"),
            B::Found("s1"),
            B::Think("t1"),
            B::Text("a"),
        ])],
        vec![
            done(&[B::Search("s0", "own"), B::Found("s0"), B::Text("b0")]),
            cross_account(),
            done(&[B::Text("b1")]),
        ],
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    agent
        .change_settings(Some("b".into()), None, None, &ev_tx)
        .await;
    agent
        .run_turn("zero".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    agent
        .change_settings(Some("a".into()), None, None, &ev_tx)
        .await;
    agent.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .change_settings(Some("b".into()), None, None, &ev_tx)
        .await;
    let outcome = agent.run_turn("two".into(), &mut rx, &ev_tx).await.unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let sent = b.bodies();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[2]["messages"][1], sent[1]["messages"][1]);
    assert_eq!(
        sent[2]["messages"][1]["content"][0]["type"],
        "server_tool_use"
    );
    let rendered = &sent[2]["messages"][3]["content"];
    assert_eq!(
        rendered[0],
        json!({"type": "text", "text": "[web search: \"rust\"]"})
    );
    assert!(
        rendered
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["type"] != "thinking")
    );
    assert_eq!(
        agent.search.cuts,
        vec![Cut {
            from: BlockPosition {
                message: 3,
                block: 0
            },
            through: 3,
            tools: false,
        }]
    );
}

#[tokio::test]
async fn a_later_visit_renders_the_new_foreign_searches_with_their_cut_and_no_refusal() {
    let (mut agent, _a, b, mut rx, _env, _store) = two_endpoints(
        vec![
            done(&[
                B::Search("s1", "rust"),
                B::Found("s1"),
                B::Think("t1"),
                B::Text("a1"),
            ]),
            done(&[
                B::Search("s3", "again"),
                B::Found("s3"),
                B::Think("t3"),
                B::Text("a3"),
            ]),
        ],
        vec![
            cross_account(),
            done(&[B::Search("s2", "own"), B::Found("s2"), B::Text("b2")]),
            done(&[B::Text("b4")]),
        ],
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    for (endpoint, text) in [("a", "one"), ("b", "two"), ("a", "three"), ("b", "four")] {
        agent
            .change_settings(Some(endpoint.into()), None, None, &ev_tx)
            .await;
        let outcome = agent.run_turn(text.into(), &mut rx, &ev_tx).await.unwrap();
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{text}: {outcome:?}"
        );
    }
    let sent = b.bodies();
    assert_eq!(sent.len(), 3);
    let later = sent[2]["messages"].as_array().unwrap();
    let earlier = sent[1]["messages"].as_array().unwrap();
    assert_eq!(later[..3], earlier[..3]);
    assert_eq!(later[3]["content"][0]["type"], "server_tool_use");
    let rendered = later[5]["content"].as_array().unwrap();
    assert_eq!(
        rendered[0],
        json!({"type": "text", "text": "[web search: \"again\"]"})
    );
    assert!(rendered.iter().all(|b| b["type"] != "thinking"));
    let first = Cut {
        from: BlockPosition {
            message: 1,
            block: 0,
        },
        through: 1,
        tools: false,
    };
    let second = Cut {
        from: BlockPosition {
            message: 5,
            block: 0,
        },
        through: 5,
        tools: false,
    };
    assert_eq!(agent.search.cuts, vec![first, second]);
    assert_eq!(agent.search.rendered_on.len(), 1);
    assert_eq!(agent.search.rendered_through.get("b"), Some(&6));
}

#[tokio::test]
async fn a_later_visits_cut_is_kept_once_across_retries_and_stays_byte_stable() {
    let (mut agent, _a, b, mut rx, _env, store) = two_endpoints(
        vec![
            done(&[
                B::Search("s1", "rust"),
                B::Found("s1"),
                B::Think("t1"),
                B::Text("a1"),
            ]),
            done(&[
                B::Search("s3", "again"),
                B::Found("s3"),
                B::Think("t3"),
                B::Text("a3"),
            ]),
        ],
        vec![
            cross_account(),
            done(&[B::Search("s2", "own"), B::Found("s2"), B::Text("b2")]),
            overloaded_before_any_event(),
            failing(
                &[B::Text("partial")],
                mid_stream(ErrorKind::Overloaded, RetryClass::Transient),
            ),
            done(&[B::Think("t4"), B::Text("b4")]),
            done(&[B::Think("t5"), B::Text("b5")]),
        ],
    );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    let turns = [
        ("a", "one"),
        ("b", "two"),
        ("a", "three"),
        ("b", "four"),
        ("b", "five"),
    ];
    for (endpoint, text) in turns {
        agent
            .change_settings(Some(endpoint.into()), None, None, &ev_tx)
            .await;
        let outcome = agent.run_turn(text.into(), &mut rx, &ev_tx).await.unwrap();
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{text}: {outcome:?}"
        );
    }
    let sent = b.bodies();
    assert_eq!(sent.len(), 6);
    assert_eq!(sent[2], sent[3]);
    assert_eq!(sent[3], sent[4]);
    assert!(
        sent[4]["messages"][5]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["type"] != "thinking")
    );
    let fourth = sent[4]["messages"].as_array().unwrap();
    let fifth = sent[5]["messages"].as_array().unwrap();
    assert_eq!(fifth[..fourth.len()], fourth[..]);
    assert_eq!(fifth[7]["content"][0]["type"], "thinking");
    assert_eq!(agent.search.cuts.len(), 2);

    let records = records(&store);
    let kinds = kinds(&store);
    let later: Vec<usize> = position(&kinds, "thinking_cut")
        .into_iter()
        .filter(|&i| {
            matches!(
                &records[i].event,
                LogEvent::ThinkingCut { from, .. } if from.message == 5
            )
        })
        .collect();
    assert_eq!(later.len(), 1);
    let fourth_turn = position(&kinds, "turn_started")[3];
    let retries: Vec<usize> = position(&kinds, "retrying")
        .into_iter()
        .filter(|&i| i > fourth_turn)
        .collect();
    assert_eq!(retries.len(), 2);
    assert!(retries[0] < later[0] && later[0] < retries[1]);
    let committed = position(&kinds, "message_committed")
        .into_iter()
        .find(|&i| i > fourth_turn)
        .unwrap();
    assert!(later[0] < committed);
    let rebuilt = rebuild_history(records.iter().map(|r| &r.event), no_images).unwrap();
    assert_eq!(rebuilt.cuts, agent.search.cuts);
}

#[tokio::test]
async fn a_search_refusal_marks_the_epoch_rebuilt() {
    let mut rig = native_rig(
        vec![
            done(&[B::Text("one")]),
            search_off(),
            done(&[B::Text("two")]),
        ],
        &[DEFAULT_MODEL],
    );
    rig.turn("go").await;
    assert_eq!(rig.agent.epoch.marked_at(), (0, 1));
    rig.turn("next").await;
    assert_eq!(rig.agent.epoch.marked_at(), (1, 2));
}

#[tokio::test]
async fn steps_with_search_on_keep_the_epoch() {
    let mut rig = native_rig(
        vec![
            uses(&[B::Search("s1", "rust"), B::Found("s1"), echo("e1")]),
            done(&[B::Text("one")]),
            done(&[B::Text("two")]),
        ],
        &[DEFAULT_MODEL],
    );
    rig.turn("go").await;
    rig.turn("next").await;
    assert_eq!(rig.agent.epoch.marked_at(), (0, 3));
}

fn web_search_call(id: &'static str) -> B {
    B::Call(id, "web_search", json!({"query": "rust release"}))
}

fn first_result(rig: &Rig, request: usize) -> Value {
    rig.messages(request)
        .last()
        .unwrap()
        .get("content")
        .unwrap()[0]
        .clone()
}

fn not_run_events(events: &[AgentEvent]) -> Vec<(String, String)> {
    pick!(
        events,
        AgentEvent::SearchNotRun {
            endpoint_name,
            model,
        } => (endpoint_name.clone(), model.clone())
    )
}

#[tokio::test]
async fn a_search_that_reached_a_model_without_it_is_answered_and_noted_once() {
    let mut rig = native_rig(
        vec![
            uses(&[web_search_call("w1")]),
            done(&[B::Text("ok")]),
            uses(&[web_search_call("w2")]),
            done(&[B::Text("again")]),
        ],
        &[DEFAULT_MODEL],
    );
    let (_, first) = rig.turn("go").await;
    let (_, second) = rig.turn("next").await;
    assert_eq!(
        not_run_events(&first),
        [(IMPLICIT_ENDPOINT.to_owned(), DEFAULT_MODEL.to_owned())]
    );
    assert!(
        not_run_events(&second).is_empty(),
        "one notice per endpoint"
    );
    for request in [1, 3] {
        let result = first_result(&rig, request);
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["is_error"], true);
        assert_eq!(
            result["content"][0]["text"],
            format!("Invalid input: {}", crate::tool::SEARCH_NOT_RUN),
            "{result}"
        );
    }
    let calls: Vec<String> = first
        .iter()
        .chain(&second)
        .filter_map(|e| match e {
            AgentEvent::ToolCallRequested { tool, .. } => Some(tool.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls, ["web_search", "web_search"]);
    let bodies = rig.provider.bodies();
    assert!(bodies.iter().all(searches));
    assert!(bodies.iter().all(|b| b["tools"] == bodies[0]["tools"]));
    assert!(tools_events(&first).is_empty() && tools_events(&second).is_empty());
    assert_eq!(
        records_of(&rig, "web_search_not_run"),
        [LogEvent::WebSearchNotRun {
            turn: 1,
            endpoint_name: IMPLICIT_ENDPOINT.into(),
            model: DEFAULT_MODEL.into(),
        }]
    );
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn an_unknown_tool_on_a_step_without_search_keeps_its_answer() {
    let mut rig = rig(vec![uses(&[web_search_call("w1")]), done(&[B::Text("ok")])]);
    let (_, events) = rig.turn("go").await;
    assert!(!searches(&rig.provider.bodies()[0]));
    assert!(not_run_events(&events).is_empty());
    assert_eq!(
        first_result(&rig, 1)["content"][0]["text"],
        "Invalid input: there is no tool named `web_search`"
    );
    assert!(records_of(&rig, "web_search_not_run").is_empty());
}

#[tokio::test]
async fn a_mixed_step_gets_one_result_per_call() {
    let mut rig = native_rig(
        vec![
            uses(&[
                B::Search("s1", "rust"),
                B::Found("s1"),
                echo("e1"),
                web_search_call("w1"),
                web_search_call("w2"),
            ]),
            done(&[B::Text("ok")]),
            done(&[B::Text("next")]),
        ],
        &[DEFAULT_MODEL],
    );
    let (_, first) = rig.turn("go").await;
    rig.turn("again").await;
    assert_eq!(not_run_events(&first).len(), 1);
    let results = rig.messages(1).last().unwrap()["content"].clone();
    let ids: Vec<&str> = results
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["tool_use_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e1", "w1", "w2"], "{results}");
    assert_ne!(results[0]["is_error"], true, "{results}");
    for i in [1, 2] {
        assert_eq!(
            results[i]["content"][0]["text"],
            format!("Invalid input: {}", crate::tool::SEARCH_NOT_RUN)
        );
    }
    let bodies = rig.provider.bodies();
    assert_eq!(bodies[2]["messages"][1], bodies[1]["messages"][1]);
    assert_eq!(bodies[2]["messages"][2], bodies[1]["messages"][2]);
    assert!(bodies.iter().all(|b| b["tools"] == bodies[0]["tools"]));
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn the_notice_shows_once_per_endpoint_across_switches() {
    let (mut agent, _a, _b, mut rx, _env, store) = two_endpoints(
        vec![
            uses(&[web_search_call("w1")]),
            done(&[B::Text("a1")]),
            uses(&[web_search_call("w3")]),
            done(&[B::Text("a2")]),
        ],
        vec![uses(&[web_search_call("w2")]), done(&[B::Text("b1")])],
    );
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    agent.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .change_settings(Some("b".into()), None, None, &ev_tx)
        .await;
    agent.run_turn("two".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .change_settings(Some("a".into()), None, None, &ev_tx)
        .await;
    agent
        .run_turn("three".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    let mut events = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        events.push(e);
    }
    let names: Vec<String> = not_run_events(&events)
        .into_iter()
        .map(|(e, _)| e)
        .collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(
        kinds(&store)
            .iter()
            .filter(|k| *k == "web_search_not_run")
            .count(),
        2
    );
}

struct Named {
    inner: Arc<dyn wisp_plugin_api::Plugin>,
    echo: wisp_plugin_api::tools::ClientTool,
}

impl Named {
    fn new() -> Self {
        use futures::FutureExt;
        let host = Arc::new(crate::plugin::PluginHost::new(
            "testkit",
            "0.2.0",
            &[],
            Arc::new(crate::plugin::KernelHost::new()),
        ));
        let inner: Arc<dyn wisp_plugin_api::Plugin> =
            Arc::new(wisp_plugin_testkit::Testkit::new(host));
        let context = wisp_plugin_api::tools::BuildContext {
            platform: crate::plugin::PLATFORM.to_owned(),
            capabilities: Vec::new(),
        };
        let defs = inner.describe(&context).now_or_never().unwrap();
        let echo = defs
            .into_iter()
            .find_map(|d| match d.kind {
                wisp_plugin_api::tools::ToolKind::Client(client) if d.name == "echo" => {
                    Some(client)
                }
                _ => None,
            })
            .unwrap();
        Self { inner, echo }
    }
}

impl crate::plugin::testing::TestTool for Named {
    fn name(&self) -> String {
        "web_search".into()
    }

    fn description(&self) -> String {
        self.echo.description.clone()
    }

    fn schema(&self) -> Value {
        serde_json::from_str(&self.echo.input_schema).unwrap()
    }

    fn meta(&self) -> wisp_plugin_api::tools::ToolMeta {
        self.echo.meta.clone()
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        futures::executor::block_on(self.inner.prepare("echo", input))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn wisp_plugin_api::tools::Prepared>,
        ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        self.inner.run(call, ctx)
    }
}

#[tokio::test]
async fn a_client_tool_named_like_the_search_tool_runs_with_no_notice() {
    let mut rig = native_rig(
        vec![
            uses(&[B::Call(
                "w1",
                "web_search",
                match echo("x") {
                    B::Call(_, _, input) => input,
                    _ => unreachable!(),
                },
            )]),
            done(&[B::Text("ok")]),
        ],
        &[DEFAULT_MODEL],
    );
    let named = crate::plugin::testing::entry(Arc::new(Named::new()));
    let mut tools: Vec<crate::tool::ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    tools.push(named);
    let placeholder = Agent::new(rig.provider.clone(), default_session());
    let agent = std::mem::replace(&mut rig.agent, placeholder);
    rig.agent = agent.with_tools(Toolbox::new(tools), rig._env.workspace());
    let (_, events) = rig.turn("go").await;
    assert!(searches(&rig.provider.bodies()[0]));
    assert!(not_run_events(&events).is_empty());
    let result = first_result(&rig, 1);
    assert_ne!(result["is_error"], true, "{result}");
    assert!(records_of(&rig, "web_search_not_run").is_empty());
}
