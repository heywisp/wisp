use wisp_protocol::Feature;

use super::retry::{failed_second_turn, retry, still};
use super::*;
use crate::agent::tests::completed_last;
use crate::search::{NoSearch, SearchForm};
use crate::test_guard::kind_of;

fn searching(provider: Arc<Fake>, store: &MemoryStore) -> Agent {
    still(provider, store)
        .with_model_settings(models(&format!(
            "[endpoints.env.models.\"{DEFAULT_MODEL}\"]\nweb_search = [\"web_search_20250305\"]\n"
        )))
        .with_web_search(true)
        .with_binding_controls(true)
}

fn searches(body: &Value) -> bool {
    body["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|t| t["name"] == "web_search"))
}

fn digest(agent: &Agent, form: SearchForm) -> String {
    crate::session::sha256_hex(&crate::tool::tools_json(
        agent.toolbox().specs(),
        agent.search_tools(agent.endpoint_name(), &agent.session.model, form),
    ))
}

fn retried(event: &AgentEvent, of: u64, request: u64) -> bool {
    matches!(
        event,
        AgentEvent::TurnStarted {
            retry_of: Some(o),
            answers: Some(r),
            ..
        } if *o == of && r.0 == request
    )
}

async fn compaction_refused_for_search(live: &mut Live) {
    failed_second_turn(live).await;
    let failed = live.compact(2, None).await;
    assert!(
        matches!(failed, AgentEvent::CompactionFailed { .. }),
        "{failed:?}"
    );
}

async fn retry_turn(live: &mut Live, of: u64, request: u64) -> Vec<AgentEvent> {
    let from = live.seen.len();
    live.send(retry(of, request)).await;
    live.until(|e| retried(e, of, request)).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.seen[from..].to_vec()
}

fn turn_starts(store: &MemoryStore) -> Vec<(usize, Option<String>)> {
    log_events(store)
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            LogEvent::TurnStarted {
                kept_tools_sha256, ..
            } => Some((i, kept_tools_sha256)),
            _ => None,
        })
        .collect()
}

fn cuts(store: &MemoryStore) -> Vec<(usize, Option<String>)> {
    log_events(store)
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            LogEvent::ThinkingCut { tools_sha256, .. } => Some((i, tools_sha256)),
            _ => None,
        })
        .collect()
}

fn quiet_about_tools(events: &[AgentEvent]) -> bool {
    !events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::ToolsChanged { .. }
                | AgentEvent::SettingsChanged { .. }
                | AgentEvent::FeatureStateChanged { .. }
        )
    })
}

#[tokio::test]
async fn a_retry_after_a_compaction_refused_for_search_sends_the_failed_tools_once() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        search_off(),
        failing(server_error()),
        answer("Retried.", 1_100),
        answer("Later.", 1_200),
    ]);
    let mut live = Live::start(searching(provider.clone(), &store));
    compaction_refused_for_search(&mut live).await;
    let events = retry_turn(&mut live, 2, 3).await;
    assert!(completed_last(&events), "{events:?}");
    assert!(quiet_about_tools(&events), "{events:?}");
    let from = live.seen.len();
    assert!(completed(&live.turn("u4", "later").await));
    assert!(
        quiet_about_tools(&live.seen[from..]),
        "{:?}",
        &live.seen[from..]
    );
    let (agent, _) = live.close().await;

    assert_eq!(provider.sent().len(), 6);
    assert!(searches(&provider.body(1)));
    assert!(searches(&provider.body(2)));
    assert!(!searches(&provider.body(3)));
    assert_eq!(provider.body(4), provider.body(1));
    assert!(!searches(&provider.body(5)));

    let starts = turn_starts(&store);
    assert_eq!(
        starts.iter().map(|(_, k)| k.clone()).collect::<Vec<_>>(),
        vec![None, None, Some(digest(&agent, SearchForm::Native)), None]
    );
    let logged = log_events(&store);
    let (retry_at, _) = starts[2];
    let (later_at, _) = starts[3];
    assert!(
        !logged[retry_at..]
            .iter()
            .any(|r| matches!(r, LogEvent::SettingsChanged { .. })),
        "{:?}",
        logged.iter().map(kind_of).collect::<Vec<_>>()
    );
    assert!(cuts(&store).is_empty(), "{:?}", cuts(&store));
    assert!(later_at > retry_at);
}

#[tokio::test]
async fn a_kept_tools_retry_refused_for_search_is_resent_with_the_cut_and_nothing_new() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        search_off(),
        failing(server_error()),
        search_off(),
        answer("Retried.", 1_100),
        answer("Later.", 1_200),
    ]);
    let mut live = Live::start(searching(provider.clone(), &store));
    compaction_refused_for_search(&mut live).await;
    let events = retry_turn(&mut live, 2, 3).await;
    assert!(completed_last(&events), "{events:?}");
    assert!(quiet_about_tools(&events), "{events:?}");
    let from = live.seen.len();
    assert!(completed(&live.turn("u4", "later").await));
    assert!(
        quiet_about_tools(&live.seen[from..]),
        "{:?}",
        &live.seen[from..]
    );
    let (agent, seen) = live.close().await;

    assert_eq!(provider.sent().len(), 7);
    assert_eq!(provider.body(4), provider.body(1));
    assert!(!searches(&provider.body(5)));
    assert_eq!(provider.body(5)["messages"], provider.body(4)["messages"]);
    assert_eq!(provider.body(6)["tools"], provider.body(5)["tools"]);
    let states = seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::FeatureStateChanged {
                    feature: Feature::NativeWebSearch,
                    ..
                }
            )
        })
        .count();
    assert_eq!(states, 1);
    let logged = log_events(&store);
    let searched = logged
        .iter()
        .filter(|r| matches!(r, LogEvent::NativeWebSearch { .. }))
        .count();
    assert_eq!(searched, 1);

    let starts = turn_starts(&store);
    assert_eq!(starts[2].1, Some(digest(&agent, SearchForm::Native)));
    assert!(cuts(&store).is_empty(), "{:?}", cuts(&store));
}

#[tokio::test]
async fn a_kept_tools_retry_that_fails_again_keeps_them_for_the_next_retry() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        search_off(),
        failing(server_error()),
        failing(server_error()),
        answer("Retried.", 1_100),
    ]);
    let mut live = Live::start(searching(provider.clone(), &store));
    compaction_refused_for_search(&mut live).await;
    let first = retry_turn(&mut live, 2, 3).await;
    assert!(
        matches!(
            first.last(),
            Some(AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error { offer: Some(_), .. },
                ..
            })
        ),
        "{first:?}"
    );
    let second = retry_turn(&mut live, 3, 4).await;
    assert!(completed_last(&second), "{second:?}");
    let (agent, _) = live.close().await;
    assert_eq!(provider.body(4), provider.body(1));
    assert_eq!(provider.body(5), provider.body(1));
    let kept = Some(digest(&agent, SearchForm::Native));
    let starts = turn_starts(&store);
    assert_eq!(
        starts.iter().map(|(_, k)| k.clone()).collect::<Vec<_>>(),
        vec![None, None, kept.clone(), kept]
    );
    assert!(cuts(&store).is_empty());
}

#[tokio::test]
async fn a_turn_on_kept_tools_starts_no_precompute_until_a_turn_on_the_current_tools() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        search_off(),
        failing(server_error()),
        answer("Retried.", super::background::NEAR),
        answer("Later.", super::background::NEAR + 100),
        compaction(SUMMARY),
    ]);
    let config = format!(
        "{}\n[endpoints.env.models.\"{DEFAULT_MODEL}\"]\nweb_search = [\"web_search_20250305\"]\n",
        super::background::POINT
    );
    let agent = still(provider.clone(), &store)
        .with_model_settings(models(&config))
        .with_web_search(true)
        .with_start_layers(super::background::saved(super::background::small_band()));
    let mut live = Live::start(agent);
    compaction_refused_for_search(&mut live).await;
    let events = retry_turn(&mut live, 2, 3).await;
    assert!(completed_last(&events), "{events:?}");
    assert_eq!(provider.body(4), provider.body(1));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(super::background::background_starts(&live.seen), 0);
    assert!(completed(&live.turn("u4", "later").await));
    live.until(|e| matches!(e, AgentEvent::CompactionPrepared { .. }))
        .await;
    let (_, seen) = live.close().await;
    assert_eq!(super::background::background_starts(&seen), 1);
    let precompute = provider.body(6);
    assert!(!searches(&precompute), "{precompute}");
    assert_eq!(precompute["tools"], provider.body(5)["tools"]);
}

#[tokio::test]
async fn a_kept_tools_retry_that_overflows_compacts_and_resends_with_the_pairs_tools() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        search_off(),
        failing(server_error()),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long",
        )),
        compaction(SUMMARY),
        answer("Retried.", 1_100),
        answer("Later.", 1_200),
    ]);
    let mut live = Live::start(searching(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", &"word ".repeat(30_000)).await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    let refused = live.compact(2, None).await;
    assert!(
        matches!(refused, AgentEvent::CompactionFailed { .. }),
        "{refused:?}"
    );
    let events = retry_turn(&mut live, 2, 3).await;
    assert!(completed_last(&events), "{events:?}");
    assert!(completed(&live.turn("u4", "later").await));
    let (agent, _) = live.close().await;

    assert_eq!(provider.sent().len(), 8);
    assert!(searches(&provider.body(4)));
    assert!(!searches(&provider.body(5)));
    assert!(!searches(&provider.body(6)));
    assert!(!searches(&provider.body(7)));
    assert_eq!(
        agent.search.built_with,
        Some(digest(&agent, SearchForm::None(NoSearch::Refused)))
    );
    assert!(cuts(&store).is_empty(), "{:?}", cuts(&store));
}
