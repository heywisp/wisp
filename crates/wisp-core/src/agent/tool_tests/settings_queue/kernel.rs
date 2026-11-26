use wisp_protocol::context::{ContextReport, PointSource, WindowSource};
use wisp_protocol::settings::{AppliedChange, Place, SitePath, When, WindowRoom, sorted_changes};
use wisp_protocol::{CacheEffect, CacheTtl, RebuildReason};

use super::*;
use crate::provider::RequestOptions;
use crate::test_guard::log_context;
use crate::test_guard::setting_text;

fn change_of(request: u64, list: &[(&str, Option<SettingValue>)], save: bool) -> ClientCommand {
    let changes = list
        .iter()
        .map(|(name, value)| SettingChange {
            target: SettingTarget::Name(SettingName::of(
                wisp_protocol::settings::find(name).unwrap(),
            )),
            value: value.clone(),
        })
        .collect();
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: sorted_changes(changes),
        save,
    }
}

fn set(request: u64, name: &str, value: SettingValue) -> ClientCommand {
    change_of(request, &[(name, Some(value))], false)
}

fn unset(request: u64, name: &str) -> ClientCommand {
    change_of(request, &[(name, None)], false)
}

fn answered(events: &[AgentEvent], request: u64) -> (CacheEffect, Vec<AppliedChange>) {
    events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SettingsChanged {
                answers: Some(r),
                cache,
                changes,
                ..
            } if r.0 == request => Some((cache.clone(), changes.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no answer to {request}: {events:?}"))
}

fn answering(request: u64) -> impl Fn(&AgentEvent) -> bool {
    move |e| {
        answers(std::slice::from_ref(e))
            .iter()
            .any(|(r, _)| *r == request)
    }
}

async fn turn(live: &mut Live, id: &str, text: &str) -> Vec<AgentEvent> {
    live.send(send(id, text)).await;
    live.until(is_turn_end).await
}

async fn ask(live: &mut Live, command: ClientCommand) -> Vec<AgentEvent> {
    let request = match &command {
        ClientCommand::ChangeSettings { request, .. } => request.0,
        _ => 0,
    };
    live.send(command).await;
    live.until(answering(request)).await
}

fn last_user_lines(request: &Sent) -> Vec<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|m| {
            m.role == Role::User
                && !m
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        })
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn json_of(request: &Sent) -> Value {
    request.body()
}

fn assert_prefix_kept(requests: &[Sent]) {
    for pair in requests.windows(2) {
        assert_eq!(pair[0].session.system, pair[1].session.system);
        assert_eq!(pair[0].session.instructions, pair[1].session.instructions);
        assert_eq!(
            serde_json::to_string(&pair[0].tools).unwrap(),
            serde_json::to_string(&pair[1].tools).unwrap()
        );
        let first = serde_json::to_value(&pair[0].messages).unwrap();
        let second = serde_json::to_value(&pair[1].messages).unwrap();
        let first = first.as_array().unwrap();
        assert_eq!(first[..], second.as_array().unwrap()[..first.len()]);
    }
}

#[tokio::test]
async fn request_options_and_the_cache_lifetime_apply_from_the_next_turn() {
    let provider = scripted(vec![done("one"), done("two"), done("three")]);
    let mut live = Live::start(provider.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    let events = ask(
        &mut live,
        change_of(
            1,
            &[
                ("cache_ttl", Some(setting_text("5m"))),
                ("debug_cache", Some(SettingValue::Bool(true))),
                ("refusal_fallback", Some(SettingValue::Bool(false))),
            ],
            false,
        ),
    )
    .await;
    let (cache, changes) = answered(&events, 1);
    assert_eq!(cache, rebuilt(RebuildReason::CacheTtlChange));
    let effects: Vec<(When, Option<CacheEffect>)> = changes
        .iter()
        .map(|c| (c.when.clone(), c.cache.clone()))
        .collect();
    assert_eq!(
        effects,
        [
            (When::NextTurn, Some(rebuilt(RebuildReason::CacheTtlChange))),
            (
                When::NextTurn,
                Some(rebuilt(RebuildReason::RequestOptionChange))
            ),
            (When::NextTurn, Some(CacheEffect::Kept)),
        ]
    );
    turn(&mut live, "tui-2", "second").await;
    ask(&mut live, set(2, "cache_ttl", setting_text("1h"))).await;
    turn(&mut live, "tui-3", "third").await;
    let requests = provider.sent();
    assert_eq!(requests.len(), 3);
    let ttls: Vec<CacheTtl> = requests.iter().map(|r| r.session.cache_ttl).collect();
    assert_eq!(
        ttls,
        [CacheTtl::OneHour, CacheTtl::FiveMinutes, CacheTtl::OneHour]
    );
    let changed = RequestOptions {
        debug_cache: true,
        refusal_fallback: false,
        message_threads: true,
    };
    let options: Vec<RequestOptions> = requests.iter().map(|r| r.options).collect();
    assert_eq!(options, [RequestOptions::default(), changed, changed]);
    let second = json_of(&requests[1]);
    assert_eq!(second["system"][0]["cache_control"]["ttl"], "1h");
    assert_eq!(second["cache_control"], json!({"type": "ephemeral"}));
    assert!(second.get("fallbacks").is_none());
    assert!(second.get("diagnostics").is_some());
    assert_eq!(requests[1].beta(), "cache-diagnosis-2026-04-07");
    let first = json_of(&requests[0]);
    assert_eq!(first["fallbacks"], "default");
    assert_eq!(requests[0].beta(), "server-side-fallback-2026-07-01");
    assert_prefix_kept(&requests);
    live.end().await;
}

fn missed(events: &[AgentEvent]) -> Vec<wisp_protocol::MissCause> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::CacheMiss { cause, .. } => cause.clone(),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn lengthening_the_cache_lifetime_expects_the_next_miss_and_shortening_does_not() {
    let large = |text: &'static str| {
        Step::from(reply_on(
            DEFAULT_MODEL,
            50_000,
            &[B::Text(text)],
            StopReason::EndTurn,
        ))
    };
    let provider = scripted(vec![large("one"), large("two"), large("three")]);
    let mut live = Live::start(provider.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    ask(&mut live, set(1, "cache_ttl", setting_text("5m"))).await;
    let shortened = turn(&mut live, "tui-2", "second").await;
    ask(&mut live, set(2, "cache_ttl", setting_text("1h"))).await;
    let lengthened = turn(&mut live, "tui-3", "third").await;
    live.end().await;
    let shortened = missed(&shortened);
    assert_eq!(shortened.len(), 1);
    assert!(
        !matches!(shortened[0], wisp_protocol::MissCause::Expected { .. }),
        "{shortened:?}"
    );
    assert_eq!(
        missed(&lengthened),
        [wisp_protocol::MissCause::Expected {
            reason: "cache_ttl_change".into()
        }]
    );
}

#[tokio::test]
async fn a_value_set_back_before_the_turn_leaves_nothing_pending() {
    let provider = scripted(vec![done("one"), done("two")]);
    let mut live = Live::start(provider.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    ask(&mut live, set(1, "debug_cache", SettingValue::Bool(true))).await;
    let events = ask(&mut live, set(2, "debug_cache", SettingValue::Bool(false))).await;
    let (cache, changes) = answered(&events, 2);
    assert_eq!(cache, CacheEffect::Kept);
    assert_eq!(changes[0].cache, Some(CacheEffect::Kept));
    ask(&mut live, set(3, "debug_cache", SettingValue::Bool(true))).await;
    let events = ask(&mut live, unset(4, "debug_cache")).await;
    let (cache, changes) = answered(&events, 4);
    assert_eq!(cache, CacheEffect::Kept);
    assert_eq!(changes[0].cache, Some(CacheEffect::Kept));
    assert_eq!(changes[0].value, None);
    turn(&mut live, "tui-2", "second").await;
    let requests = provider.sent();
    assert_eq!(requests[1].options, RequestOptions::default());
    live.end().await;
}

#[tokio::test]
async fn the_time_and_context_lines_change_only_the_next_messages() {
    let provider = scripted(vec![done("one"), done("two"), done("three")]);
    let mut live = Live::start(provider.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    let events = ask(
        &mut live,
        change_of(
            1,
            &[
                ("context.context_note", Some(SettingValue::Bool(false))),
                ("time", Some(setting_text("off"))),
            ],
            false,
        ),
    )
    .await;
    let (cache, changes) = answered(&events, 1);
    assert_eq!(cache, CacheEffect::Kept);
    assert!(
        changes
            .iter()
            .all(|c| c.when == When::NextTurn && c.cache == Some(CacheEffect::Kept))
    );
    turn(&mut live, "tui-2", "second").await;
    ask(&mut live, unset(2, "time")).await;
    turn(&mut live, "tui-3", "third").await;
    let requests = provider.sent();
    let lines = |n: usize| -> Vec<bool> {
        let text = last_user_lines(&requests[n]);
        vec![
            text.iter().any(|t| t.starts_with("Sent: ")),
            text.iter().any(|t| t.starts_with("Context: ")),
        ]
    };
    assert_eq!(lines(0), [true, true]);
    assert_eq!(lines(1), [false, false]);
    assert_eq!(lines(2), [true, false]);
    assert_prefix_kept(&requests);
    live.end().await;
}

#[tokio::test]
async fn now_settings_leave_the_next_request_byte_identical() {
    let plain = scripted(vec![done("one"), done("two")]);
    let mut live = Live::start(plain.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    turn(&mut live, "tui-2", "second").await;
    live.end().await;

    let changed = scripted(vec![done("one"), done("two")]);
    let mut live = Live::start(changed.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    let events = ask(
        &mut live,
        change_of(
            1,
            &[
                ("context.count_tokens", Some(SettingValue::Bool(false))),
                (
                    "context.verbatim_user_tokens",
                    Some(SettingValue::Tokens(4_000)),
                ),
                ("display.theme", Some(setting_text("light"))),
                ("instructions.warn_tokens", Some(SettingValue::Tokens(100))),
                ("pricing", Some(setting_text("none"))),
                (
                    "retry.budget_usd",
                    Some(SettingValue::Usd { micros: 500_000 }),
                ),
            ],
            false,
        ),
    )
    .await;
    let (cache, changes) = answered(&events, 1);
    assert_eq!(cache, CacheEffect::Kept);
    assert!(
        changes
            .iter()
            .all(|c| c.when == When::Now && c.cache.is_none() && c.layer == Layer::Session)
    );
    let pricing = changes
        .iter()
        .find(|c| c.target.spec().name == "pricing")
        .unwrap();
    assert_eq!(
        pricing.target,
        SettingTarget::Path(SitePath::new(
            Place::Endpoint(crate::agent::IMPLICIT_ENDPOINT.into()),
            wisp_protocol::settings::find("pricing").unwrap()
        ))
    );
    turn(&mut live, "tui-2", "second").await;
    live.end().await;
    assert_eq!(plain.texts(), changed.texts());
}

#[tokio::test]
async fn a_saved_change_sends_the_same_request_as_the_same_session_change() {
    let session = scripted(vec![done("one"), done("two")]);
    let mut live = Live::start(session.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    ask(&mut live, set(1, "time", setting_text("utc"))).await;
    turn(&mut live, "tui-2", "second").await;
    live.end().await;

    let host = Host::new(false);
    host.open();
    let saved_run = scripted(vec![done("one"), done("two")]);
    let mut live = Live::start(saved_run.clone(), Some(host.clone()));
    turn(&mut live, "tui-1", "first").await;
    let events = ask(
        &mut live,
        change_of(1, &[("time", Some(setting_text("utc")))], true),
    )
    .await;
    let (_, changes) = answered(&events, 1);
    assert_eq!(changes[0].layer, Layer::Session);
    assert!(changes[0].saved.is_some());
    turn(&mut live, "tui-2", "second").await;
    live.end().await;
    assert_eq!(session.texts(), saved_run.texts());
    assert_eq!(
        host.saves(),
        [vec![SiteEdit::Set(
            site_path("time").unwrap(),
            setting_text("utc")
        )]]
    );
}

#[tokio::test]
async fn a_next_session_setting_is_only_saved_and_changes_nothing_now() {
    let plain = scripted(vec![done("one"), done("two")]);
    let mut live = Live::start(plain.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    turn(&mut live, "tui-2", "second").await;
    live.end().await;

    let host = Host::new(false);
    host.open();
    let changed = scripted(vec![done("one"), done("two")]);
    let mut live = Live::with(changed.clone(), Some(host), |agent| {
        agent.with_catalog(crate::tool::testkit::catalog())
    });
    turn(&mut live, "tui-1", "first").await;
    let events = ask(
        &mut live,
        change_of(
            1,
            &[("tools", Some(SettingValue::List(vec!["bash".into()])))],
            true,
        ),
    )
    .await;
    let (cache, changes) = answered(&events, 1);
    assert_eq!(cache, CacheEffect::Kept);
    assert_eq!(changes[0].when, When::NextSession);
    assert_eq!(changes[0].layer, Layer::Session);
    let voice = [("voice.voice", Some(SettingValue::Text("cove".into())))];
    let events = ask(&mut live, change_of(2, &voice, true)).await;
    assert_eq!(answered(&events, 2).1[0].when, When::NextCall);
    turn(&mut live, "tui-2", "second").await;
    live.end().await;
    assert_eq!(plain.texts(), changed.texts());
}

#[tokio::test]
async fn a_tools_change_is_checked_against_the_catalog_and_offers_its_leaders() {
    let host = Host::new(false);
    host.open();
    let mut live = Live::with(scripted(Vec::new()), Some(host), |agent| {
        agent.with_catalog(crate::tool::testkit::catalog())
    });
    let mut announced = Vec::new();
    for (request, name, reason) in [
        (
            1,
            "nope",
            "tools: nope is no tool; tools: ask_secret, ask_user, bash, list_accounts, list_settings, manage, remember, rename_chat, search, show_code, show_diff, show_page, spawn_agent, view_file, web_search, write_todos, or none",
        ),
        (
            2,
            "process",
            "tools: process comes with bash; name bash instead",
        ),
        (
            3,
            "list_agents",
            "tools: list_agents comes with spawn_agent; name spawn_agent instead",
        ),
        (
            4,
            "report",
            "tools: report is for child agents only; spawn_agent gives it to every child",
        ),
    ] {
        let events = ask(
            &mut live,
            change_of(
                request,
                &[("tools", Some(SettingValue::List(vec![name.into()])))],
                true,
            ),
        )
        .await;
        announced.extend(pick!(&events, AgentEvent::CatalogChanged { catalog } => catalog.clone()));
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::Rejected { reason: r, request: Some(RequestId(n)), .. }
                    if r == reason && *n == request
            )),
            "{events:?}"
        );
    }
    for (request, names) in [
        (5, vec!["list_agents", "spawn_agent", "write_todos"]),
        (6, vec!["spawn_agent"]),
        (7, vec!["list_agents", "spawn_agent"]),
    ] {
        let events = ask(
            &mut live,
            change_of(
                request,
                &[(
                    "tools",
                    Some(SettingValue::List(
                        names.into_iter().map(str::to_owned).collect(),
                    )),
                )],
                true,
            ),
        )
        .await;
        let (_, changes) = answered(&events, request);
        assert_eq!(changes[0].when, When::NextSession, "{request}");
    }
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(8),
    })
    .await;
    let events = live.until(answering(8)).await;
    let Some(AgentEvent::SettingsSnapshot(snapshot)) = events.last() else {
        panic!("{events:?}");
    };
    let tools = snapshot
        .settings
        .iter()
        .find(|s| s.name.spec().name == "tools")
        .unwrap();
    assert_eq!(
        tools.choices,
        [
            "ask_secret",
            "ask_user",
            "bash",
            "list_accounts",
            "list_settings",
            "manage",
            "remember",
            "rename_chat",
            "search",
            "show_code",
            "show_diff",
            "show_page",
            "spawn_agent",
            "view_file",
            "web_search",
            "write_todos"
        ]
    );
    assert_eq!(announced.len(), 1);
    assert_eq!(snapshot.plugins, announced[0].plugins);
    live.end().await;
}

#[tokio::test]
async fn a_bad_value_refuses_the_whole_change() {
    let provider = scripted(vec![done("one"), done("two")]);
    let mut live = Live::start(provider.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    let events = ask(
        &mut live,
        change_of(
            1,
            &[
                ("cache_ttl", Some(setting_text("5m"))),
                ("time", Some(setting_text("sometimes"))),
            ],
            false,
        ),
    )
    .await;
    assert_eq!(answers(&events), [(1, false)]);
    assert!(reasons(&events)[0].starts_with("time "), "{events:?}");
    turn(&mut live, "tui-2", "second").await;
    let requests = provider.sent();
    assert_eq!(requests[1].session.cache_ttl, CacheTtl::OneHour);
    live.end().await;
}

#[tokio::test]
async fn connection_fields_point_to_wisp_endpoint_edit() {
    let host = Host::new(false);
    host.open();
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![SettingChange {
            target: SettingTarget::Path(SitePath::new(
                Place::Endpoint("env".into()),
                wisp_protocol::settings::find("auth").unwrap(),
            )),
            value: Some(setting_text("bearer")),
        }],
        save: true,
    })
    .await;
    let events = live.quiet().await;
    assert_eq!(
        reasons(&events),
        ["auth is part of how Wisp connects to an account; change it with wisp account edit"]
    );
    assert!(host.saves().is_empty());
    live.end().await;
}

#[tokio::test]
async fn a_display_change_stores_nothing_and_keeps_a_pending_switch_in_the_answer() {
    let host = Host::new(false);
    host.open();
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(ClientCommand::shortcut(
        RequestId(1),
        None,
        Some(SONNET.into()),
        None,
    ))
    .await;
    let events = ask(
        &mut live,
        change_of(2, &[("display.theme", Some(setting_text("light")))], true),
    )
    .await;
    let (cache, changes) = answered(&events, 2);
    assert_eq!(cache, rebuilt(RebuildReason::ModelSwitch));
    assert_eq!(changes[0].when, When::Now);
    assert_eq!(changes[0].layer, Layer::Session);
    assert_eq!(changes[0].cache, None);
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(3),
    })
    .await;
    let events = live.until(answering(3)).await;
    let Some(AgentEvent::SettingsSnapshot(snapshot)) = events.last() else {
        panic!("{events:?}");
    };
    let theme = snapshot
        .settings
        .iter()
        .find(|s| s.name.spec().name == "display.theme")
        .unwrap();
    assert_ne!(theme.layer, Layer::Session);
    live.end().await;
}

async fn report(live: &mut Live, request: u64) -> Option<ContextReport> {
    live.send(ClientCommand::ContextReport {
        detail: wisp_protocol::context::ReportDetail::Summary,
        request: RequestId(request),
    })
    .await;
    let events = live
        .until(|e| matches!(e, AgentEvent::ContextReport(_)))
        .await;
    events.into_iter().find_map(|e| match e {
        AgentEvent::ContextReport(report) => Some(report),
        _ => None,
    })
}

async fn window(live: &mut Live, request: u64) -> Option<(u64, WindowSource)> {
    let window = report(live, request).await?.window?;
    Some((window.tokens, window.source))
}

async fn point(live: &mut Live, request: u64) -> Option<(u64, PointSource)> {
    let point = report(live, request).await?.compact_at?;
    Some((point.tokens, point.set_by))
}

#[tokio::test]
async fn a_token_point_grows_a_subscription_window_from_the_next_turn_and_keeps_the_prefix() {
    use crate::endpoints::ProviderKind;
    use crate::models::list::{ModelList, tests};

    let astra = "gpt-6.2-astra";
    let list = ModelList::parse(&tests::wire(&[tests::entry(astra, 1, 272_000)])).unwrap();
    let provider = Fake::with(
        Knobs {
            kind: Some(ProviderKind::OpenAi),
            billing: Some(wisp_protocol::Billing::ChatGptPlan),
            list: Some(Arc::new(list)),
            ..Knobs::default()
        },
        [300_000, 310_000].map(|input| {
            Step::from(reply_on(
                astra,
                input,
                &[B::Text("reply")],
                StopReason::EndTurn,
            ))
        }),
    );
    let mut live = Live::with(provider.clone(), None, std::convert::identity);
    ask(
        &mut live,
        set(
            1,
            "context.background_compaction",
            SettingValue::Bool(false),
        ),
    )
    .await;
    live.send(ClientCommand::shortcut(
        RequestId(2),
        None,
        Some(astra.into()),
        None,
    ))
    .await;
    turn(&mut live, "tui-1", "first").await;
    let events = ask(
        &mut live,
        set(3, "compact_at", SettingValue::Tokens(400_000)),
    )
    .await;
    assert_eq!(answered(&events, 3).1[0].when, When::NextTurn);
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(4),
    })
    .await;
    let events = live.until(answering(4)).await;
    let Some(AgentEvent::SettingsSnapshot(snapshot)) = events.last() else {
        panic!("{events:?}");
    };
    assert_eq!(snapshot.context.window, Some(470_589));
    assert_eq!(
        snapshot.context.room,
        Some(WindowRoom {
            base: 272_000,
            max: 872_000
        })
    );
    turn(&mut live, "tui-2", "second").await;
    assert_eq!(
        window(&mut live, 5).await,
        Some((470_589, WindowSource::Subscription))
    );
    assert_eq!(
        point(&mut live, 6).await,
        Some((400_000, PointSource::Session))
    );
    live.end().await;
    let requests = provider.sent();
    assert_eq!(requests.len(), 2);
    for (request, marker) in requests.iter().zip(["of 272k tokens", "of 471k tokens"]) {
        assert!(
            last_user_lines(request)
                .iter()
                .any(|line| line.contains("Context:") && line.contains(marker))
        );
    }
    assert_prefix_kept(&requests);
}

#[tokio::test]
async fn the_context_window_is_kept_per_model_and_applies_from_the_next_turn() {
    let provider = scripted(vec![
        done("one"),
        done("two"),
        done("three"),
        done("four"),
        done("five"),
        done("six"),
    ]);
    let mut live = Live::start(provider.clone(), None);
    turn(&mut live, "tui-1", "first").await;
    let events = ask(
        &mut live,
        set(1, "context_window", SettingValue::Tokens(103_999)),
    )
    .await;
    assert_eq!(answers(&events), [(1, false)]);
    assert!(
        reasons(&events)[0].contains("Set at least 104000"),
        "{events:?}"
    );
    let events = ask(
        &mut live,
        set(2, "context_window", SettingValue::Tokens(200_000)),
    )
    .await;
    let (_, changes) = answered(&events, 2);
    let path = |model: &str| {
        SettingTarget::Path(SitePath::new(
            Place::Model(model.into()),
            wisp_protocol::settings::find("context_window").unwrap(),
        ))
    };
    assert_eq!(changes[0].target, path(DEFAULT_MODEL));
    assert_eq!(changes[0].when, When::NextTurn);
    assert_ne!(window(&mut live, 3).await.map(|w| w.0), Some(200_000));
    turn(&mut live, "tui-2", "second").await;
    assert_eq!(
        window(&mut live, 4).await,
        Some((200_000, WindowSource::Session))
    );
    live.send(ClientCommand::shortcut(
        RequestId(5),
        None,
        Some(SONNET.into()),
        None,
    ))
    .await;
    let events = ask(
        &mut live,
        set(6, "context_window", SettingValue::Tokens(300_000)),
    )
    .await;
    let (_, changes) = answered(&events, 6);
    assert_eq!(changes[0].target, path(SONNET));
    turn(&mut live, "tui-3", "third").await;
    assert_eq!(
        window(&mut live, 7).await,
        Some((300_000, WindowSource::Session))
    );
    live.send(ClientCommand::shortcut(
        RequestId(8),
        None,
        Some(DEFAULT_MODEL.into()),
        None,
    ))
    .await;
    turn(&mut live, "tui-4", "fourth").await;
    assert_eq!(
        window(&mut live, 9).await,
        Some((200_000, WindowSource::Session))
    );
    assert_eq!(
        layer_of(&mut live, 10, "context_window").await,
        Layer::Session
    );
    let replaced = SettingValue::Tokens(250_000);
    ask(&mut live, set(11, "context_window", replaced)).await;
    turn(&mut live, "tui-5", "fifth").await;
    assert_eq!(
        window(&mut live, 12).await,
        Some((250_000, WindowSource::Session))
    );
    ask(&mut live, unset(13, "context_window")).await;
    turn(&mut live, "tui-6", "sixth").await;
    let cleared = window(&mut live, 14).await.map(|w| w.1);
    assert_eq!(cleared, Some(WindowSource::Shipped));
    let requests = provider.sent();
    let notes: Vec<Option<String>> = requests
        .iter()
        .map(|r| {
            last_user_lines(r)
                .into_iter()
                .find(|t| t.starts_with("Context: "))
        })
        .collect();
    assert!(
        notes[1].as_deref().unwrap().contains("of 200k tokens"),
        "{notes:?}"
    );
    assert!(
        notes[2].as_deref().unwrap().contains("of 300k tokens"),
        "{notes:?}"
    );
    assert_prefix_kept(&requests[..2]);
    live.end().await;
}

fn models_from(text: &str) -> (crate::context::ModelSettings, crate::config::SavedSettings) {
    let rows = crate::models::ConfigModels::parse(text).unwrap();
    let saved = crate::config::SavedSettings::from_models(&rows);
    let shipped = crate::models::ModelTable::shipped().unwrap();
    (crate::context::ModelSettings::new(shipped, rows), saved)
}

fn all_models_point(request: u64, value: Option<SettingValue>) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![SettingChange {
            target: SettingTarget::Path(SitePath::new(
                Place::Global,
                wisp_protocol::settings::find("compact_at").unwrap(),
            )),
            value,
        }],
        save: true,
    }
}

async fn layer_of(live: &mut Live, request: u64, name: &str) -> Layer {
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(request),
    })
    .await;
    let events = live.until(answering(request)).await;
    let Some(AgentEvent::SettingsSnapshot(snapshot)) = events.last() else {
        panic!("{events:?}");
    };
    let row = snapshot
        .settings
        .iter()
        .find(|s| s.name.spec().name == name)
        .unwrap();
    row.layer.clone()
}

async fn session_point_in_turns(live: &mut Live, tokens: Option<u64>, request: u64) {
    let value = tokens.map(SettingValue::Tokens);
    let events = ask(live, change_of(request, &[("compact_at", value)], false)).await;
    let (_, changes) = answered(&events, request);
    let spec = wisp_protocol::settings::find("compact_at").unwrap();
    assert_eq!(
        changes[0].target,
        SettingTarget::Name(SettingName::of(spec))
    );
    assert_eq!(changes[0].when, When::NextTurn);
    turn(live, &format!("tui-{request}"), "next").await;
}

async fn replaced_then_cleared(
    mut live: Live,
    before: (u64, PointSource, Layer),
    after: (PointSource, Layer),
) {
    let (tokens, set_by, layer) = before;
    assert_eq!(point(&mut live, 1).await, Some((tokens, set_by)));
    assert_eq!(layer_of(&mut live, 2, "compact_at").await, layer);
    session_point_in_turns(&mut live, Some(300_000), 3).await;
    let replaced = Some((300_000, PointSource::Session));
    assert_eq!(point(&mut live, 4).await, replaced);
    assert_eq!(layer_of(&mut live, 5, "compact_at").await, Layer::Session);
    session_point_in_turns(&mut live, None, 6).await;
    let (set_by, layer) = after;
    assert_eq!(point(&mut live, 7).await.map(|p| p.1), Some(set_by));
    assert_eq!(layer_of(&mut live, 8, "compact_at").await, layer);
    live.end().await;
}

#[tokio::test]
async fn a_name_replaces_and_clears_a_model_s_restored_session_point() {
    let spec = wisp_protocol::settings::find("compact_at").unwrap();
    let place = SitePath::new(Place::Model(DEFAULT_MODEL.into()), spec);
    let host = Host::new(false);
    host.open();
    let provider = scripted(vec![done("one"), done("two")]);
    let live = Live::with(provider, Some(host), move |agent| {
        let restored = SettingValue::Tokens(500_000);
        let seeded = crate::settings::Given {
            target: wisp_protocol::settings::Target::Path(place.clone()),
            value: restored.clone(),
            origin: "/config".into(),
        };
        let mut models = crate::context::ModelSettings::shipped_only();
        models.set_session(vec![seeded]);
        let mut agent = agent.with_model_settings(models);
        agent.restore_session(&SettingTarget::Path(place), Some(restored));
        agent
    });
    let before = (500_000, PointSource::Session, Layer::Session);
    replaced_then_cleared(live, before, (PointSource::Default, Layer::Default)).await;
}

#[tokio::test]
async fn a_name_replaces_a_flag_point_for_the_session_and_clears_back_to_it() {
    let spec = wisp_protocol::settings::find("compact_at").unwrap();
    let flags = vec![crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(spec),
        value: SettingValue::Tokens(600_000),
        origin: "--compact-at".into(),
    }];
    let models = crate::context::ModelSettings::shipped_only().with_flags(&flags);
    let provider = scripted(vec![done("one"), done("two")]);
    let host = Host::new(false);
    host.open();
    let live = Live::with(provider, Some(host), move |agent| {
        agent
            .with_model_settings(models)
            .with_start_layers(crate::settings::host::StartLayers {
                flags,
                ..crate::settings::host::StartLayers::default()
            })
    });
    let before = (600_000, PointSource::Flag, Layer::Flag);
    replaced_then_cleared(live, before, (PointSource::Flag, Layer::Flag)).await;
}

#[tokio::test]
async fn a_path_save_for_all_models_sets_no_session_value_and_leaves_a_model_s_own_point() {
    let config = "[models.\"claude-opus-5-5\"]\ncompact_at = 400000\n";
    let host = Host::new(false);
    host.open();
    let provider = scripted(vec![done("one"), done("two"), done("three")]);
    let mut live = Live::with(provider, Some(host.clone()), |agent| {
        let (models, saved) = models_from(config);
        let flags = vec![crate::settings::Given {
            target: wisp_protocol::settings::Target::Name(
                wisp_protocol::settings::find("compact_at").unwrap(),
            ),
            value: SettingValue::Tokens(600_000),
            origin: "--compact-at".into(),
        }];
        agent
            .with_model_settings(models.with_flags(&flags))
            .with_start_layers(crate::settings::host::StartLayers {
                saved,
                flags,
                ..crate::settings::host::StartLayers::default()
            })
    });
    assert_eq!(
        point(&mut live, 7).await,
        Some((600_000, PointSource::Flag))
    );
    let events = ask(
        &mut live,
        set(8, "compact_at", SettingValue::Tokens(500_000)),
    )
    .await;
    assert_eq!(answered(&events, 8).1[0].layer, Layer::Session);
    let value = SettingValue::Tokens(250_000);
    let events = ask(&mut live, all_models_point(1, Some(value))).await;
    let (_, changes) = answered(&events, 1);
    assert_eq!(changes[0].layer, Layer::Config);
    assert_eq!(changes[0].when, When::NextTurn);
    assert_eq!(changes[0].overridden, this_session(Layer::Model));
    turn(&mut live, "tui-1", "first").await;
    let own = Some((400_000, PointSource::Config));
    assert_eq!(point(&mut live, 2).await, own);
    assert_eq!(layer_of(&mut live, 9, "compact_at").await, Layer::Model);
    live.send(ClientCommand::shortcut(
        RequestId(3),
        None,
        Some(SONNET.into()),
        None,
    ))
    .await;
    turn(&mut live, "tui-2", "second").await;
    assert_eq!(
        point(&mut live, 4).await,
        Some((250_000, PointSource::Config))
    );
    let events = ask(&mut live, all_models_point(5, None)).await;
    let (_, changes) = answered(&events, 5);
    assert_eq!(changes[0].overridden, None);
    turn(&mut live, "tui-3", "third").await;
    let set_by = point(&mut live, 6).await.map(|p| p.1);
    assert_eq!(set_by, Some(PointSource::Default));
    let saves = host.saves();
    assert_eq!(saves.len(), 2);
    assert_eq!(saves[1][0].path().to_string(), "compact_at");
    live.end().await;
}

#[tokio::test]
async fn a_window_change_binds_to_the_model_a_queued_switch_makes_current() {
    let host = Host::new(true);
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(saved(1, "model", SONNET)).await;
    live.send(set(2, "context_window", SettingValue::Tokens(300_000)))
        .await;
    assert!(answers(&live.quiet().await).is_empty());
    host.open();
    let events = live.until(answering(2)).await;
    assert_eq!(answers(&events), [(1, false), (2, true)]);
    let (_, changes) = answered(&events, 2);
    assert_eq!(
        changes[0].target,
        SettingTarget::Path(SitePath::new(
            Place::Model(SONNET.into()),
            wisp_protocol::settings::find("context_window").unwrap(),
        ))
    );
    live.end().await;
}

#[tokio::test]
async fn the_retry_budget_the_counter_and_the_kept_words_apply_at_once() {
    let env = Env::new("budget");
    let mut agent = super::super::agent(scripted(Vec::new()), &env);
    let (events, mut sink) = mpsc::channel(64);
    tokio::spawn(async move { while sink.recv().await.is_some() {} });
    let queued = |changes| Queued::Change {
        request: Some(RequestId(1)),
        changes,
        save: false,
        projected: crate::agent::settings::Projected::default(),
        by: None,
    };
    agent
        .enqueue(
            queued(vec![
                SettingChange::named("retry.budget_usd", SettingValue::Usd { micros: 500_000 })
                    .unwrap(),
            ]),
            &events,
        )
        .await;
    assert_eq!(agent.retry.budget_usd, Some(0.5));
    agent
        .enqueue(
            queued(vec![
                SettingChange::named("context.count_tokens", SettingValue::Bool(false)).unwrap(),
            ]),
            &events,
        )
        .await;
    assert!(agent.fill.count_off);
    let verbatim = wisp_protocol::settings::find("context.verbatim_user_tokens").unwrap();
    agent
        .enqueue(
            queued(vec![SettingChange::set(
                verbatim,
                SettingValue::Tokens(4_000),
            )]),
            &events,
        )
        .await;
    assert_eq!(
        agent.resolved_at(verbatim, &Place::Global),
        Some(SettingValue::Tokens(4_000))
    );
    let none = SettingChange {
        target: SettingTarget::Name(SettingName::of(
            wisp_protocol::settings::find("retry.budget_usd").unwrap(),
        )),
        value: None,
    };
    agent.enqueue(queued(vec![none]), &events).await;
    assert_eq!(agent.retry.budget_usd, None);
}

async fn kept_words_note(agent: &mut Agent) -> Option<wisp_protocol::settings::Overridden> {
    let (events, mut sink) = mpsc::channel(64);
    let verbatim = wisp_protocol::settings::find("context.verbatim_user_tokens").unwrap();
    agent
        .enqueue(
            Queued::Change {
                request: Some(RequestId(1)),
                changes: vec![SettingChange::set(verbatim, SettingValue::Tokens(4_000))],
                save: false,
                projected: crate::agent::settings::Projected::default(),
                by: None,
            },
            &events,
        )
        .await;
    let mut seen = Vec::new();
    while let Ok(event) = sink.try_recv() {
        seen.push(event);
    }
    let (_, changes) = answered(&seen, 1);
    changes[0].overridden.clone()
}

#[tokio::test]
async fn a_model_table_s_kept_words_still_win_over_the_session_value() {
    let env = Env::new("kept-words-table");
    let mut plain = super::super::agent(scripted(Vec::new()), &env);
    assert_eq!(kept_words_note(&mut plain).await, None);
    let models = |config: &str| {
        crate::context::ModelSettings::new(
            crate::models::ModelTable::shipped().unwrap(),
            crate::models::ConfigModels::parse(config).unwrap(),
        )
    };
    let mut tabled = super::super::agent(scripted(Vec::new()), &env).with_model_settings(models(
        "[models.\"claude-opus-5-5\"]\nverbatim_user_tokens = 10\n",
    ));
    assert_eq!(
        kept_words_note(&mut tabled).await,
        Some(wisp_protocol::settings::Overridden {
            by: Layer::Model,
            scope: wisp_protocol::settings::OverrideScope::ThisSession,
        })
    );
}

#[tokio::test]
async fn pricing_is_kept_per_endpoint_and_binds_to_a_pending_switch() {
    let env = Env::new("pricing");
    let agent = super::super::agent(scripted(Vec::new()), &env);
    let first = agent.connections[0].clone();
    let mut second = first.clone();
    second.name = "other".into();
    let mut agent =
        agent.with_connections(vec![first, second], vec![], crate::agent::IMPLICIT_ENDPOINT);
    let (events, mut sink) = mpsc::channel(64);
    tokio::spawn(async move { while sink.recv().await.is_some() {} });
    let queued = |changes| Queued::Change {
        request: Some(RequestId(1)),
        changes,
        save: false,
        projected: crate::agent::settings::Projected::default(),
        by: None,
    };
    agent
        .enqueue(
            queued(vec![
                SettingChange::named("pricing", setting_text("none")).unwrap(),
            ]),
            &events,
        )
        .await;
    let priced: Vec<bool> = agent.connections.iter().map(|c| c.priced).collect();
    assert_eq!(priced, [false, true]);
    agent
        .enqueue(
            queued(ClientCommand::shortcut_changes(
                Some("other".into()),
                None,
                None,
            )),
            &events,
        )
        .await;
    agent
        .enqueue(
            queued(vec![
                SettingChange::named("pricing", setting_text("none")).unwrap(),
            ]),
            &events,
        )
        .await;
    let priced: Vec<bool> = agent.connections.iter().map(|c| c.priced).collect();
    assert_eq!(priced, [false, false]);
    let back = SettingChange {
        target: SettingTarget::Name(SettingName::of(
            wisp_protocol::settings::find("pricing").unwrap(),
        )),
        value: None,
    };
    agent.enqueue(queued(vec![back]), &events).await;
    let priced: Vec<bool> = agent.connections.iter().map(|c| c.priced).collect();
    assert_eq!(priced, [false, true]);
}

#[tokio::test]
async fn the_log_records_each_change_and_the_request_settings_of_each_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![done("one"), done("two")]);
    let shape_store = store.clone();
    let mut live = Live::with(provider, None, move |agent| {
        let context = log_context();
        agent.with_log(SessionLog::new(Box::new(shape_store), "s1"), context)
    });
    turn(&mut live, "tui-1", "first").await;
    ask(&mut live, set(1, "cache_ttl", setting_text("5m"))).await;
    ask(&mut live, unset(2, "cache_ttl")).await;
    ask(&mut live, set(3, "debug_cache", SettingValue::Bool(true))).await;
    turn(&mut live, "tui-2", "second").await;
    live.end().await;
    let records: Vec<LogEvent> = log_events(&store);
    let LogEvent::SessionStarted {
        debug_cache,
        refusal_fallback,
        ..
    } = &records[0]
    else {
        panic!("{records:?}");
    };
    assert_eq!((*debug_cache, *refusal_fallback), (Some(false), Some(true)));
    let changes: Vec<serde_json::Value> = pick!(
        &records,
        LogEvent::SettingsChanged { changes, cache, .. } => json!({
            "cache": cache,
            "changes": changes,
        })
    );
    assert_eq!(
        changes,
        [
            json!({
                "cache": {"type": "rebuilt", "reason": "cache_ttl_change"},
                "changes": [{"target": {"name": "cache_ttl"}, "value": {"kind": "text", "text": "5m"}, "layer": "session", "when": "next_turn"}],
            }),
            json!({
                "cache": {"type": "kept"},
                "changes": [{"target": {"name": "cache_ttl"}, "value": {"kind": "text", "text": "1h"}, "layer": "session", "when": "next_turn", "unset": true}],
            }),
            json!({
                "cache": {"type": "rebuilt", "reason": "request_option_change"},
                "changes": [{"target": {"name": "debug_cache"}, "value": {"kind": "bool", "text": "on"}, "layer": "session", "when": "next_turn"}],
            }),
        ]
    );
    let turns: Vec<(Option<CacheTtl>, Option<bool>, Option<bool>)> = pick!(
        &records,
        LogEvent::TurnStarted {
            cache_ttl,
            debug_cache,
            refusal_fallback,
            ..
        } => (*cache_ttl, *debug_cache, *refusal_fallback)
    );
    assert_eq!(
        turns,
        [
            (Some(CacheTtl::OneHour), Some(false), Some(true)),
            (Some(CacheTtl::OneHour), Some(true), Some(true)),
        ]
    );
}

fn instructions_of(env: &Env, bytes: usize) -> impl FnOnce(Agent) -> Agent + use<> {
    fs::write(env.work.join("AGENTS.md"), "word ".repeat(bytes / 5)).unwrap();
    let workspace = env.workspace();
    let config = env.root.join("config");
    move |agent| {
        agent
            .with_tools(super::super::test_toolbox(), workspace)
            .with_test_instructions(crate::agent::InstructionParts::auto(), Some(&config))
    }
}

fn warnings(events: &[AgentEvent]) -> Vec<String> {
    pick!(events, AgentEvent::InstructionsWarning { diagnostic } => diagnostic.message.clone())
}

#[tokio::test]
async fn a_lower_warning_threshold_rechecks_the_loaded_instructions_at_once() {
    let docs = Env::new("warn-docs");
    let shape = instructions_of(&docs, 2_000);
    let provider = scripted(vec![done("one")]);
    let mut live = Live::with(provider.clone(), None, shape);
    let raised = ask(
        &mut live,
        set(1, "instructions.warn_tokens", SettingValue::Tokens(100_000)),
    )
    .await;
    let (_, changes) = answered(&raised, 1);
    assert_eq!(changes[0].when, When::Now);
    let lowered = ask(
        &mut live,
        set(2, "instructions.warn_tokens", SettingValue::Tokens(100)),
    )
    .await;
    let (cache, changes) = answered(&lowered, 2);
    assert_eq!(
        (cache, changes[0].when.clone()),
        (CacheEffect::Kept, When::Now)
    );
    let after = turn(&mut live, "tui-1", "first").await;
    let mut seen = warnings(&raised);
    seen.extend(warnings(&lowered));
    seen.extend(warnings(&after));
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].starts_with("instructions are large: about 500 tokens"),
        "{seen:?}"
    );
    let first = after
        .iter()
        .position(|e| matches!(e, AgentEvent::InstructionsWarning { .. }))
        .unwrap();
    let started = after
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .unwrap();
    assert!(first < started, "{after:?}");
    live.end().await;
}

fn given(name: &str, value: &str, origin: &str) -> crate::settings::Given {
    crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(wisp_protocol::settings::find(name).unwrap()),
        value: setting_text(value),
        origin: origin.into(),
    }
}

async fn display_note(
    layers: crate::settings::host::StartLayers,
    name: &str,
    value: &str,
) -> Option<wisp_protocol::settings::Overridden> {
    let host = Host::new(false);
    host.open();
    let mut live = Live::with(scripted(Vec::new()), Some(host), |agent| {
        agent.with_start_layers(layers)
    });
    let events = ask(
        &mut live,
        change_of(1, &[(name, Some(setting_text(value)))], true),
    )
    .await;
    let (_, changes) = answered(&events, 1);
    live.end().await;
    changes[0].overridden.clone()
}

fn this_session(by: Layer) -> Option<wisp_protocol::settings::Overridden> {
    Some(wisp_protocol::settings::Overridden {
        by,
        scope: wisp_protocol::settings::OverrideScope::ThisSession,
    })
}

#[tokio::test]
async fn a_forced_display_layer_still_wins_this_session() {
    let forced = |name: &str, value: &str, origin: &str| crate::settings::host::StartLayers {
        forced: vec![given(name, value, origin)],
        ..crate::settings::host::StartLayers::default()
    };
    assert_eq!(
        display_note(
            forced("display.color", "none", "NO_COLOR is set"),
            "display.color",
            "16"
        )
        .await,
        this_session(Layer::Forced)
    );
    assert_eq!(
        display_note(
            forced("display.motion", "reduced", "TERM=dumb"),
            "display.motion",
            "full"
        )
        .await,
        this_session(Layer::Forced)
    );
    assert_eq!(
        display_note(
            forced("display.color", "none", "NO_COLOR is set"),
            "display.theme",
            "light"
        )
        .await,
        None
    );
}

#[tokio::test]
async fn auto_under_a_display_variable_lets_the_variable_win_this_session() {
    let theme = || crate::settings::host::StartLayers {
        env: vec![given("display.theme", "dark", "WISP_THEME=dark")],
        ..crate::settings::host::StartLayers::default()
    };
    assert_eq!(
        display_note(theme(), "display.theme", "auto").await,
        this_session(Layer::Env)
    );
    assert_eq!(
        display_note(theme(), "display.theme", "light").await,
        Some(wisp_protocol::settings::Overridden {
            by: Layer::Env,
            scope: wisp_protocol::settings::OverrideScope::NewSessions,
        })
    );
    let auto_variable = crate::settings::host::StartLayers {
        env: vec![given("display.theme", "auto", "WISP_THEME=auto")],
        ..crate::settings::host::StartLayers::default()
    };
    assert_eq!(
        display_note(auto_variable, "display.theme", "light").await,
        None
    );
}

async fn change_note(
    layers: crate::settings::host::StartLayers,
    name: &str,
    value: Option<&str>,
) -> Option<wisp_protocol::settings::Overridden> {
    let host = Host::new(false);
    host.open();
    let mut live = Live::with(scripted(Vec::new()), Some(host), |agent| {
        agent.with_start_layers(layers)
    });
    let events = ask(
        &mut live,
        change_of(1, &[(name, value.map(setting_text))], true),
    )
    .await;
    let (_, changes) = answered(&events, 1);
    live.end().await;
    changes[0].overridden.clone()
}

fn new_sessions(by: Layer) -> Option<wisp_protocol::settings::Overridden> {
    Some(wisp_protocol::settings::Overridden {
        by,
        scope: wisp_protocol::settings::OverrideScope::NewSessions,
    })
}

fn env_layer(name: &str, value: &str, origin: &str) -> crate::settings::host::StartLayers {
    crate::settings::host::StartLayers {
        env: vec![given(name, value, origin)],
        ..crate::settings::host::StartLayers::default()
    }
}

#[tokio::test]
async fn a_reset_under_a_variable_keeps_its_new_sessions_note() {
    assert_eq!(
        change_note(
            env_layer("display.theme", "light", "WISP_THEME=light"),
            "display.theme",
            None
        )
        .await,
        new_sessions(Layer::Env)
    );
}

#[tokio::test]
async fn auto_under_a_display_flag_lets_the_flag_win_this_session() {
    let flagged = crate::settings::host::StartLayers {
        flags: vec![given("display.theme", "dark", "--set display.theme=dark")],
        ..crate::settings::host::StartLayers::default()
    };
    assert_eq!(
        change_note(flagged, "display.theme", Some("auto")).await,
        this_session(Layer::Flag)
    );
}

fn flag_layer(name: &str, value: &str, origin: &str) -> crate::settings::host::StartLayers {
    crate::settings::host::StartLayers {
        flags: vec![given(name, value, origin)],
        ..crate::settings::host::StartLayers::default()
    }
}

#[tokio::test]
async fn a_saved_value_under_a_matching_flag_reports_no_override() {
    assert_eq!(
        change_note(
            flag_layer("display.theme", "dark", "--set display.theme=dark"),
            "display.theme",
            Some("light")
        )
        .await,
        None
    );
    assert_eq!(
        change_note(
            flag_layer("effort", "low", "--effort low"),
            "effort",
            Some("high")
        )
        .await,
        None
    );
}

#[tokio::test]
async fn a_next_session_setting_under_a_variable_still_wins_in_new_sessions() {
    for value in [None, Some("on")] {
        assert_eq!(
            change_note(
                env_layer("keychain", "off", "WISP_KEYCHAIN=off"),
                "keychain",
                value
            )
            .await,
            new_sessions(Layer::Env),
            "{value:?}"
        );
        assert_eq!(
            change_note(
                flag_layer("keychain", "off", "--keychain off"),
                "keychain",
                value
            )
            .await,
            this_session(Layer::Flag),
            "{value:?}"
        );
    }
}

#[tokio::test]
async fn a_variable_beats_a_flag_in_the_note_for_a_next_session_setting() {
    let both = || crate::settings::host::StartLayers {
        flags: vec![given("keychain", "on", "--keychain on")],
        env: vec![given("keychain", "off", "WISP_KEYCHAIN=off")],
        ..crate::settings::host::StartLayers::default()
    };
    for value in [None, Some("auto"), Some("on")] {
        assert_eq!(
            change_note(both(), "keychain", value).await,
            new_sessions(Layer::Env),
            "{value:?}"
        );
    }
}

#[tokio::test]
async fn a_flag_under_a_models_alias_shows_as_the_flag_on_its_row_and_in_the_window() {
    let target =
        wisp_protocol::settings::parse_target("models.\"claude-opus-4-5-20251101\".context_window")
            .unwrap();
    let flags = vec![crate::settings::Given {
        target,
        value: SettingValue::Tokens(150_000),
        origin: "--set models.\"claude-opus-4-5-20251101\".context_window".into(),
    }];
    let models = crate::context::ModelSettings::shipped_only().with_flags(&flags);
    let mut live = Live::with(scripted(Vec::new()), None, move |mut agent| {
        agent.session.model = "claude-opus-4-5".into();
        agent
            .with_model_settings(models)
            .with_start_layers(crate::settings::host::StartLayers {
                flags,
                ..crate::settings::host::StartLayers::default()
            })
    });
    assert_eq!(
        window(&mut live, 1).await,
        Some((150_000, WindowSource::Flag))
    );
    assert_eq!(layer_of(&mut live, 2, "context_window").await, Layer::Flag);
    live.end().await;
}

#[tokio::test]
async fn a_tools_save_keeps_unknown_names_refuses_new_ones_and_resets_to_the_default() {
    let host = Host::new(false);
    host.open();
    let tools = wisp_protocol::settings::find("tools").unwrap();
    let mut saved = crate::config::SavedSettings::default();
    saved.insert(
        wisp_protocol::settings::SitePath::global(tools),
        SettingValue::List(vec!["bash".into(), "telepathy".into()]),
        1,
    );
    let mut live = Live::with(scripted(Vec::new()), Some(host.clone()), move |agent| {
        agent
            .with_catalog(crate::tool::testkit::catalog())
            .with_start_layers(crate::settings::host::StartLayers {
                saved,
                ..crate::settings::host::StartLayers::default()
            })
    });
    let list = |names: &[&str]| SettingValue::List(names.iter().map(|n| (*n).into()).collect());
    let events = ask(
        &mut live,
        change_of(
            1,
            &[("tools", Some(list(&["bash", "nope", "telepathy"])))],
            true,
        ),
    )
    .await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Rejected { reason, request: Some(RequestId(1)), .. }
                if reason.starts_with("tools: nope is no tool;")
        )),
        "{events:?}"
    );
    let events = ask(
        &mut live,
        change_of(
            2,
            &[("tools", Some(list(&["bash", "show_diff", "telepathy"])))],
            true,
        ),
    )
    .await;
    let (_, changes) = answered(&events, 2);
    assert_eq!(changes[0].when, When::NextSession);
    let defaults = [
        "ask_secret",
        "ask_user",
        "bash",
        "list_accounts",
        "list_settings",
        "manage",
        "remember",
        "rename_chat",
        "search",
        "show_code",
        "show_diff",
        "spawn_agent",
        "view_file",
        "web_search",
        "write_todos",
    ];
    let tools_path = wisp_protocol::settings::SitePath::global(tools);
    for request in [3, 4] {
        let events = ask(
            &mut live,
            change_of(request, &[("tools", Some(list(&defaults)))], true),
        )
        .await;
        let (_, changes) = answered(&events, request);
        assert_eq!(changes[0].value, None, "{changes:?}");
        assert_eq!(changes[0].saved.is_some(), request == 3, "{changes:?}");
    }
    assert_eq!(host.saves().len(), 2);
    assert_eq!(host.saves()[1], vec![SiteEdit::Unset(tools_path.clone())]);
    live.end().await;
    let flagged = Host::new(false);
    flagged.open();
    let flags = vec![crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(tools),
        value: list(&["bash"]),
        origin: "--tools bash".into(),
    }];
    let mut live = Live::with(scripted(Vec::new()), Some(flagged.clone()), move |agent| {
        agent
            .with_catalog(crate::tool::testkit::catalog())
            .with_start_layers(crate::settings::host::StartLayers {
                flags,
                ..crate::settings::host::StartLayers::default()
            })
    });
    let events = ask(
        &mut live,
        change_of(1, &[("tools", Some(list(&defaults)))], true),
    )
    .await;
    let (_, changes) = answered(&events, 1);
    assert_eq!(changes[0].value, Some(list(&defaults)), "{changes:?}");
    assert_eq!(
        flagged.saves(),
        vec![vec![SiteEdit::Set(tools_path, list(&defaults))]]
    );
    live.end().await;
}
