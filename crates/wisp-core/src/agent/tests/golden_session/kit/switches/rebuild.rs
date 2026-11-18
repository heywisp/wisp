use wisp_protocol::RebuildReason;
use wisp_protocol::log::{PrefixCopy, ToolsCopy};
use wisp_protocol::settings::ENABLED_KEY;

use super::*;

fn tools_rebuilt() -> CacheEffect {
    CacheEffect::Rebuilt {
        reason: RebuildReason::ToolsChange,
    }
}

fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

fn rebuild_records(store: &MemoryStore) -> Vec<PrefixCopy> {
    pick!(
        log_events(store),
        LogEvent::ToolsChanged {
            via: ToolChangeVia::Rebuild,
            copy,
            ..
        } => *copy
    )
}

fn copy_digest(definitions: &[Value], sent: &Value) -> String {
    let mut all: Vec<Value> = definitions.to_vec();
    all.extend(
        sent["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| !definitions.iter().any(|d| d["name"] == t["name"]))
            .map(|t| {
                let mut t = t.clone();
                if let Some(fields) = t.as_object_mut() {
                    fields.remove("cache_control");
                }
                t
            }),
    );
    all.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    crate::session::sha256_hex(&serde_json::to_string(&all).unwrap())
}

fn system_text(request: &Value) -> String {
    request["system"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_of(name: &str) -> String {
    started(Tools::Default)
        .toolbox
        .system_line_of(name)
        .unwrap()
}

fn on_and_off() -> Vec<String> {
    every_default()
        .into_iter()
        .filter(|name| name != "view_file")
        .collect()
}

fn viewing(env: &Env) -> Part {
    std::fs::create_dir_all(env.work()).unwrap();
    let path = env.work().join("small.txt");
    std::fs::write(&path, "first line\n").unwrap();
    call(
        "toolu_01",
        "view_file",
        json!({"path": path.display().to_string()}),
    )
}

async fn rebuilt_on_plain(
    env: &Env,
    server: &Server,
    store: &MemoryStore,
) -> (Vec<AgentEvent>, Vec<AgentEvent>, Agent) {
    let kit = without_show_diff(server, env, store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let answer = session.tools(1, on_and_off()).await;
    let second = session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    (answer, second, session.end().await)
}

fn plain_replies(env: &Env) -> Vec<Reply> {
    vec![
        says("msg_01", 400, "One."),
        reply("msg_02", 520, vec![viewing(env)], "tool_use"),
        says("msg_03", 640, "Two."),
        says("msg_04", 760, "Three."),
    ]
}

#[tokio::test]
async fn a_tool_never_sent_turned_on_where_blocks_cant_go_rebuilds_once_sorted_with_off_tools_kept()
{
    let env = Env::new("rebuild-plain");
    let server = Server::start(plain_replies(&env)).await;
    let store = MemoryStore::default();
    let (answer, second, agent) = rebuilt_on_plain(&env, &server, &store).await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(tools_rebuilt())));
    assert!(waits(&answer).is_empty(), "{:?}", waits(&answer));
    assert_eq!(
        changed(&second),
        [(
            vec!["show_diff".to_owned()],
            vec!["view_file".to_owned()],
            ToolChangeVia::Rebuild,
            Some(tools_rebuilt())
        )]
    );
    let (first, rebuilt) = (body(&server, 0), body(&server, 1));
    assert!(!client_names(&first).contains(&"show_diff".to_owned()));
    let names = client_names(&rebuilt);
    assert_eq!(names, sorted(names.clone()));
    assert!(names.contains(&"show_diff".to_owned()));
    assert!(names.contains(&"view_file".to_owned()));
    let system = system_text(&rebuilt);
    assert!(system.contains(&line_of("show_diff")), "{system}");
    assert!(system.contains(&line_of("view_file")), "{system}");
    let (_, user) = last_two(&rebuilt);
    assert_eq!(texts(&user)[0], "Tools turned off: view_file.\n");
    let refused = body(&server, 2);
    assert!(
        refused
            .to_string()
            .contains(wisp_tool_text::switches::REFUSED)
    );
    for later in [2, 3] {
        assert_eq!(body(&server, later)["tools"], rebuilt["tools"]);
        assert_eq!(body(&server, later)["system"], rebuilt["system"]);
    }
    assert_eq!(marked(&store), ["\"tools_change\""]);
    let records = rebuild_records(&store);
    assert_eq!(records.len(), 1);
    let PrefixCopy {
        system_prompt,
        system_prompt_sha256,
        system_base_sha256,
        tools,
        tools_sha256,
        whole:
            ToolsCopy {
                tool_definitions,
                deferred_definitions,
                tool_context,
                history_len,
            },
        tools_on,
    } = records[0].clone();
    assert_eq!(system_prompt.as_deref(), Some(system.as_str()));
    assert_eq!(
        system_prompt_sha256,
        Some(crate::session::sha256_hex(&system))
    );
    assert!(system_base_sha256.is_some());
    assert_eq!(tools, names);
    assert_eq!(tools_sha256, Some(copy_digest(&tool_definitions, &rebuilt)));
    let defined: Vec<String> = tool_definitions
        .iter()
        .filter_map(|d| d["name"].as_str().map(str::to_owned))
        .collect();
    let functions: Vec<String> = names
        .iter()
        .filter(|n| *n != "web_search")
        .cloned()
        .collect();
    assert_eq!(defined, functions);
    assert!(tool_context.is_some());
    assert!(
        deferred_definitions
            .iter()
            .all(|d| d["name"] != "show_diff")
    );
    assert_eq!(history_len, 2);
    let on = tools_on.unwrap();
    assert!(on.contains(&"show_diff".to_owned()));
    assert!(!on.contains(&"view_file".to_owned()));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_rebuild_emits_one_event_one_record_and_one_hook_fire() {
    use wisp_plugin_api::hooks::{Event, ToolsChange};
    let (watched, calls) = watching();
    let env = Env::new("rebuild-hook");
    let server = Server::start(plain_replies(&env)).await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.tools = Arc::new(crate::plugin::Tools::new(
        watched.clone(),
        watched.on_tools_with(
            &without("show_diff").into_iter().collect(),
            &std::collections::BTreeSet::new(),
        ),
    ));
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, on_and_off()).await;
    let second = session.turn("u2", "two").await;
    let third = session.turn("u3", "three").await;
    session.end().await;
    assert_eq!(changed(&second).len(), 1, "{:?}", changed(&second));
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    let records: Vec<String> = logged_kinds(&store)
        .into_iter()
        .filter(|k| k == "tools_changed")
        .collect();
    assert_eq!(records.len(), 1);
    let seen: Vec<Event> = calls.of("seen").into_iter().map(|c| c.event).collect();
    assert_eq!(
        seen,
        [Event::ToolsChanged(ToolsChange {
            added: vec!["show_diff".into()],
            removed: vec!["view_file".into()],
            via: "rebuild".into(),
        })]
    );
}

#[tokio::test]
async fn a_switch_to_a_model_without_inline_tools_after_inline_additions_rebuilds_with_the_switch()
{
    let env = Env::new("rebuild-switch");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    change(&mut session, 2, model(PLAIN)).await;
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(
        changed(&third),
        [(
            Vec::new(),
            Vec::new(),
            ToolChangeVia::Rebuild,
            Some(tools_rebuilt())
        )]
    );
    assert!(deferred_in(&body(&server, 1)).contains(&"show_diff".to_owned()));
    assert!(client_names(&body(&server, 2)).contains(&"show_diff".to_owned()));
    assert!(no_deferral(&body(&server, 2)));
    assert_eq!(body(&server, 2)["model"], PLAIN);
    assert_eq!(marked(&store), ["\"model_switch\""]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_tool_change_where_blocks_cant_go_answers_next_turn_rebuilt_and_nothing_waits() {
    let env = Env::new("rebuild-answer");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let answer = session.tools(1, every_default()).await;
    let agent = session.end().await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(tools_rebuilt())));
    assert_eq!(
        agent.catalog().tool("show_diff").map(|t| t.waits),
        Some(false)
    );
}

async fn plugin_turned_on_rebuilds(kit: Kit, server: &Server, store: &MemoryStore, key: &str) {
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent.with_keep_warm(8));
    session.turn("u1", "one").await;
    let answer = session.switched(1, "show-diff", key, true).await;
    skips_as_changed(store).await;
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert!(!client_names(&body(server, 0)).contains(&"show_diff".to_owned()));
    assert_eq!(answered(&answer), (When::NextTurn, Some(tools_rebuilt())));
    assert_eq!(
        changed(&second),
        [(
            vec!["show_code".to_owned(), "show_diff".to_owned()],
            Vec::new(),
            ToolChangeVia::Rebuild,
            Some(tools_rebuilt())
        )]
    );
    assert_eq!(rebuild_records(store).len(), 1);
    rebuilds(store, &agent).await;
}

#[tokio::test]
async fn a_plugin_turned_on_where_blocks_cant_go_answers_next_turn_rebuilt_and_rebuilds() {
    use crate::plugin::testing::plugin_flag;
    let env = Env::new("rebuild-plugin-answer");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.start_layers.flags = vec![plugin_flag("show-diff", ENABLED_KEY, false)];
    kit.tools = Arc::new(crate::plugin::Tools::new(
        registry(Tools::Default),
        registry(Tools::Default).on_tools_with(
            &every_default().into_iter().collect(),
            &["show-diff".to_owned()].into_iter().collect(),
        ),
    ));
    plugin_turned_on_rebuilds(kit, &server, &store, ENABLED_KEY).await;
}

#[tokio::test]
async fn a_plugin_added_back_where_blocks_cant_go_answers_next_turn_rebuilt_and_rebuilds() {
    let env = Env::new("rebuild-plugin-added-back");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = plugin_gone(&server, &env, &store, &["show-diff"]);
    plugin_turned_on_rebuilds(kit, &server, &store, wisp_protocol::settings::INSTALLED_KEY).await;
}

#[tokio::test]
async fn a_rebuild_beside_a_pair_switch_that_changes_the_server_tools_leaves_announce_its_change() {
    let env = Env::new("rebuild-beside-switch");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-4-7\"]\nweb_search = []\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(&mut session, 1, model(PLAIN)).await;
    session.tools(2, every_default()).await;
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(
        changed(&second),
        [
            (
                vec!["show_diff".to_owned()],
                Vec::new(),
                ToolChangeVia::Rebuild,
                Some(tools_rebuilt())
            ),
            (
                Vec::new(),
                vec!["web_search".to_owned()],
                ToolChangeVia::Rebuild,
                None
            ),
        ]
    );
    assert_eq!(marked(&store), ["\"model_switch\""]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_by_value_tool_a_fallback_took_off_the_wire_comes_back_by_a_rebuild() {
    let env = Env::new("rebuild-after-fallback");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        reply_error(
            "400 Bad Request",
            "invalid_request_error",
            "messages.3.content.0.tool_addition: Extra inputs are not permitted",
        ),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = on_a_gateway(&server, &env, &store);
    kit.start_layers = without_show_diff(&server, &env, &store).start_layers;
    kit.tools = without_show_diff(&server, &env, &store).tools;
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    let third = session.turn("u3", "three").await;
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert_eq!(
        kinds(&last_two(&body(&server, 1)).1),
        ["tool_addition show_diff by value", "text"]
    );
    assert!(!client_names(&body(&server, 2)).contains(&"show_diff".to_owned()));
    assert_eq!(
        changed(&third),
        [(
            Vec::new(),
            Vec::new(),
            ToolChangeVia::Rebuild,
            Some(tools_rebuilt())
        )]
    );
    assert!(changed(&fourth).is_empty());
    assert!(client_names(&body(&server, 3)).contains(&"show_diff".to_owned()));
    assert_eq!(body(&server, 4)["tools"], body(&server, 3)["tools"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn the_prefix_key_tools_change_only_at_a_rebuild() {
    let env = Env::new("rebuild-key");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let agent = session.end().await;
    let first = agent.prefix_key();
    let mut session = Session::start(agent);
    session
        .tools(
            1,
            without("view_file")
                .into_iter()
                .filter(|n| n != "show_diff")
                .collect(),
        )
        .await;
    session.turn("u2", "two").await;
    let agent = session.end().await;
    let noted = agent.prefix_key();
    assert!(first.same_bytes(&noted));
    let mut session = Session::start(agent);
    session.tools(2, without("view_file")).await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(!noted.same_bytes(&agent.prefix_key()));
    assert_eq!(body(&server, 1)["tools"], body(&server, 0)["tools"]);
    assert_ne!(body(&server, 2)["tools"], body(&server, 1)["tools"]);
}

#[tokio::test]
async fn a_pre_turn_compaction_while_a_rebuild_is_due_restates_blocks_only_and_the_moment_rebuilds()
{
    let env = Env::new("rebuild-compacted");
    let server = Server::start(vec![
        says("msg_01", 40_000, "One."),
        reply("msg_02", 40_100, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_03", 640, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.models =
        models("[models.\"claude-opus-4-7\"]\nnative_compaction = false\ncompact_at = 20000\n");
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert!(
        second
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{second:#?}"
    );
    assert_eq!(changed(&second).len(), 1, "{:?}", changed(&second));
    assert_eq!(body(&server, 1)["tools"], body(&server, 0)["tools"]);
    assert!(client_names(&body(&server, 2)).contains(&"show_diff".to_owned()));
    assert_eq!(rebuild_records(&store).len(), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn keep_warm_skips_as_changed_while_a_rebuild_is_due() {
    let env = Env::new("rebuild-keep-warm");
    let server = Server::start(vec![says("msg_01", 400, "One.")]).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent.with_keep_warm(8));
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    skips_as_changed(&store).await;
    session.end().await;
}

async fn skips_as_changed(store: &MemoryStore) {
    an_hour_later().await;
    let refresh = || {
        log_events(store).into_iter().find_map(|e| match e {
            LogEvent::CacheRefresh {
                outcome, detail, ..
            } => Some((outcome, detail)),
            _ => None,
        })
    };
    for _ in 0..200 {
        if refresh().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        refresh(),
        Some((
            wisp_protocol::RefreshOutcome::Skipped,
            Some("changed".to_owned())
        ))
    );
}

#[tokio::test]
async fn the_summary_fork_after_a_rebuild_sends_the_new_copy() {
    let env = Env::new("rebuild-summary-fork");
    let server = Server::start(vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 700, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-4-7\"]\nnative_compaction = false\n");
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (turn, fork, after) = (body(&server, 1), body(&server, 2), body(&server, 3));
    assert!(client_names(&turn).contains(&"show_diff".to_owned()));
    for request in [&fork, &after] {
        assert_eq!(request["tools"], turn["tools"]);
        assert_eq!(request["system"], turn["system"]);
    }
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_retry_that_opens_its_message_again_rebuilds_there() {
    let env = Env::new("rebuild-retry-reopens");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        server_error(),
        says("msg_02", 520, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.send(ClientCommand::send("u2", "two")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session.tools(1, every_default()).await;
    session
        .send(ClientCommand::Retry {
            turn: 2,
            endpoint: None,
            request: RequestId(3),
        })
        .await;
    let retried = session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let agent = session.end().await;
    assert_eq!(changed(&retried).len(), 1, "{:?}", changed(&retried));
    assert!(!client_names(&body(&server, 1)).contains(&"show_diff".to_owned()));
    assert!(client_names(&body(&server, 2)).contains(&"show_diff".to_owned()));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_rebuild_where_binding_controls_were_still_off_is_marked_as_the_tools_change() {
    let env = Env::new("rebuild-binding-mark");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit
        .build(on("claude-sonnet-4-5", kit::Start::Fresh))
        .await
        .unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(&mut session, 1, model("claude-sonnet-5")).await;
    session.turn("u2", "two").await;
    session.tools(2, every_default()).await;
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(changed(&third).len(), 1, "{:?}", changed(&third));
    assert_eq!(marked(&store), ["\"model_switch\"", "\"tools_change\""]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_rebuild_moves_the_logged_tools_so_the_next_settings_change_logs_none() {
    let env = Env::new("rebuild-logged-tools");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session
        .send(ClientCommand::ChangeSettings {
            request: RequestId(2),
            changes: vec![
                SettingChange::named("effort", SettingValue::Text("high".into())).unwrap(),
            ],
            save: false,
        })
        .await;
    session
        .until(|e| matches!(e, AgentEvent::SettingsChanged { answers, .. } if *answers == Some(RequestId(2))))
        .await;
    session.turn("u3", "three").await;
    session.end().await;
    let digests: Vec<Option<String>> = pick!(
        log_events(&store),
        LogEvent::SettingsChanged { tools_sha256, .. } => tools_sha256
    );
    assert!(!digests.is_empty());
    assert!(digests.iter().all(Option::is_none), "{digests:?}");
}

const SEARCH: &str = crate::search::WEB_SEARCH;

fn searches(request: &Value) -> bool {
    client_names(request).contains(&SEARCH.to_owned())
}

fn tools_records(store: &MemoryStore) -> usize {
    logged_kinds(store)
        .iter()
        .filter(|k| *k == "tools_changed")
        .count()
}

fn without_search() -> Vec<String> {
    without(SEARCH)
}

fn whole_cuts(store: &MemoryStore) -> usize {
    records(store)
        .iter()
        .filter(|r| {
            matches!(
                &r.event,
                LogEvent::ThinkingCut {
                    tools_sha256: Some(_),
                    ..
                }
            )
        })
        .count()
}

#[tokio::test]
async fn web_search_turned_off_rebuilds_once_with_its_note_and_binds_or_cuts_the_thinking() {
    for (refused, by_switch) in [(false, false), (true, false), (false, true), (true, true)] {
        let env = Env::new("rebuild-search-off");
        let mut replies = Vec::new();
        if refused {
            replies.push(reply_error(
                "400 Bad Request",
                "invalid_request_error",
                "thinking.block_binding: Extra inputs are not permitted",
            ));
        }
        replies.extend(three());
        let first = usize::from(refused);
        let server = Server::start(replies).await;
        let store = MemoryStore::default();
        let kit = switching(&server, &env, &store);
        let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent);
        session.turn("u1", "one").await;
        let answer = if by_switch {
            session.switched(1, "web-search", ENABLED_KEY, false).await
        } else {
            session.tools(1, without_search()).await
        };
        let second = session.turn("u2", "two").await;
        let third = session.turn("u3", "three").await;
        let agent = session.end().await;
        assert_eq!(
            answered(&answer),
            (When::NextTurn, Some(tools_rebuilt())),
            "{refused}"
        );
        assert!(waits(&answer).is_empty(), "{refused}");
        assert_eq!(
            changed(&second),
            [(
                Vec::new(),
                vec![SEARCH.to_owned()],
                ToolChangeVia::Rebuild,
                Some(tools_rebuilt())
            )],
            "{refused}"
        );
        assert!(changed(&third).is_empty(), "{refused}");
        assert!(searches(&body(&server, first)), "{refused}");
        let rebuilt = body(&server, first + 1);
        assert!(!searches(&rebuilt), "{refused}");
        assert_eq!(
            body(&server, first + 2)["tools"],
            rebuilt["tools"],
            "{refused}"
        );
        let (_, user) = last_two(&rebuilt);
        assert_eq!(
            texts(&user)[0],
            "Tools turned off: web_search.\n",
            "{refused}"
        );
        assert_eq!(marked(&store), ["\"tools_change\""], "{refused}");
        assert_eq!(tools_records(&store), 1, "{refused}");
        let records = rebuild_records(&store);
        assert!(!records[0].tools.contains(&SEARCH.to_owned()), "{refused}");
        assert!(!records[0].tools.is_empty(), "{refused}");
        assert_eq!(whole_cuts(&store), usize::from(refused), "{refused}");
        assert_eq!(
            agent.catalog().tool(SEARCH).map(|t| (t.on, t.waits)),
            Some((false, false)),
            "{refused}"
        );
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn web_search_turned_on_where_the_pair_cant_send_it_changes_nothing_until_a_pair_can() {
    let env = Env::new("rebuild-search-unsupported");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = configured(&server, &env, &store, without_search());
    kit.models = models("[models.\"claude-opus-4-7\"]\nweb_search = []\n");
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let answer = session.tools(1, every_default()).await;
    let second = session.turn("u2", "two").await;
    change(&mut session, 2, model(CAPABLE)).await;
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(CacheEffect::Kept)));
    assert!(waits(&answer).is_empty());
    assert!(changed(&second).is_empty(), "{:?}", changed(&second));
    let quiet = body(&server, 1);
    assert!(!searches(&quiet));
    assert_eq!(quiet["tools"], body(&server, 0)["tools"]);
    assert!(
        !texts(&last_two(&quiet).1)
            .iter()
            .any(|t| t.contains(SEARCH))
    );
    assert!(searches(&body(&server, 2)));
    let named: Vec<Changed> = changed(&third)
        .into_iter()
        .filter(|(added, _, _, _)| added.contains(&SEARCH.to_owned()))
        .collect();
    assert_eq!(named.len(), 1, "{:?}", changed(&third));
    assert!(!marked(&store).contains(&"\"tools_change\"".to_owned()));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn one_tools_edit_that_turns_web_search_and_a_never_sent_tool_on_rebuilds_once() {
    for with_switch in [false, true] {
        let env = Env::new("rebuild-search-and-tool");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        let names: Vec<String> = without_search()
            .into_iter()
            .filter(|n| n != "show_diff")
            .collect();
        let mut kit = configured(&server, &env, &store, names.clone());
        if with_switch {
            kit.start_layers
                .flags
                .push(crate::plugin::testing::plugin_flag(
                    "web-search",
                    ENABLED_KEY,
                    false,
                ));
            kit.tools = Arc::new(crate::plugin::Tools::new(
                registry(Tools::Default),
                registry(Tools::Default).on_tools_with(
                    &names.into_iter().collect(),
                    &std::collections::BTreeSet::from(["web-search".to_owned()]),
                ),
            ));
        }
        let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent);
        session.turn("u1", "one").await;
        let answer = if with_switch {
            let list = SettingChange::named("tools", SettingValue::List(every_default())).unwrap();
            let switch = crate::plugin::testing::plugin_switch(1, "web-search", ENABLED_KEY, true);
            let ClientCommand::ChangeSettings { mut changes, .. } = switch else {
                panic!("{switch:?}");
            };
            changes.push(list);
            session
                .send(ClientCommand::ChangeSettings {
                    request: RequestId(1),
                    changes,
                    save: false,
                })
                .await;
            session
                .until(|e| matches!(e, AgentEvent::SettingsChanged { answers, .. } if *answers == Some(RequestId(1))))
                .await
        } else {
            session.tools(1, every_default()).await
        };
        let second = session.turn("u2", "two").await;
        let agent = session.end().await;
        assert_eq!(
            answered(&answer),
            (When::NextTurn, Some(tools_rebuilt())),
            "{with_switch}"
        );
        assert_eq!(
            changed(&second),
            [(
                vec!["show_diff".to_owned(), SEARCH.to_owned()],
                Vec::new(),
                ToolChangeVia::Rebuild,
                Some(tools_rebuilt())
            )],
            "{with_switch}"
        );
        assert!(!searches(&body(&server, 0)), "{with_switch}");
        let names = client_names(&body(&server, 1));
        assert!(
            names.contains(&"show_diff".to_owned()) && searches(&body(&server, 1)),
            "{with_switch}"
        );
        assert_eq!(marked(&store), ["\"tools_change\""], "{with_switch}");
        assert_eq!(tools_records(&store), 1, "{with_switch}");
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_pair_switch_and_a_web_search_flip_in_one_opening_rebuild_once_on_the_new_pair() {
    let env = Env::new("rebuild-search-switch");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = configured(&server, &env, &store, without_search());
    kit.models = models("[models.\"claude-opus-4-7\"]\nweb_search = []\n");
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(&mut session, 1, model(CAPABLE)).await;
    let answer = session.tools(2, every_default()).await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(tools_rebuilt())));
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(
        changed(&second),
        [(
            vec![SEARCH.to_owned()],
            Vec::new(),
            ToolChangeVia::Rebuild,
            Some(tools_rebuilt())
        )]
    );
    assert!(searches(&body(&server, 1)));
    assert_eq!(tools_records(&store), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn keep_warm_skips_as_changed_while_a_web_search_rebuild_is_due() {
    let env = Env::new("rebuild-search-keep-warm");
    let server = Server::start(vec![says("msg_01", 400, "One.")]).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent.with_keep_warm(8));
    session.turn("u1", "one").await;
    let answer = session.tools(1, without_search()).await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(tools_rebuilt())));
    skips_as_changed(&store).await;
    session.end().await;
}

fn by_value_additions(request: &Value, name: &str) -> usize {
    let wanted = format!("tool_addition {name} by value");
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .flat_map(kinds)
        .filter(|kind| *kind == wanted)
        .count()
}

fn declared(request: &Value, name: &str) -> usize {
    client_names(request).iter().filter(|n| *n == name).count() + by_value_additions(request, name)
}

#[tokio::test]
async fn a_switch_back_to_an_inline_pair_after_a_rebuild_declares_each_tool_once() {
    let env = Env::new("rebuild-void");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 40_000, "Four."),
        reply("msg_05", 40_100, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_06", 1_000, "Five."),
        says("msg_07", 1_100, "Six."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = on_a_gateway(&server, &env, &store);
    kit.start_layers = without_show_diff(&server, &env, &store).start_layers;
    kit.tools = without_show_diff(&server, &env, &store).tools;
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    change(&mut session, 2, model(PLAIN)).await;
    session.turn("u3", "three").await;
    change(&mut session, 3, model(CAPABLE)).await;
    let back = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert_eq!(
        counted_names(&agent)
            .iter()
            .filter(|n| *n == "show_diff")
            .count(),
        1
    );
    let mut session = Session::start(agent);
    session.compact(4).await;
    session.turn("u5", "five").await;
    session.end().await;
    let mut kit = on_a_gateway(&server, &env, &store);
    kit.tools = switching(&server, &env, &store).tools;
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let resumed_agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(resumed_agent.cache_rebuild().is_none());
    let mut session = Session::start(resumed_agent);
    session.turn("u6", "six").await;
    let agent = session.end().await;
    assert_eq!(by_value_additions(&body(&server, 1), "show_diff"), 1);
    assert!(client_names(&body(&server, 2)).contains(&"show_diff".to_owned()));
    assert!(changed(&back).is_empty(), "{:?}", changed(&back));
    for n in [3, 4, 5, 6] {
        let request = body(&server, n);
        assert_eq!(declared(&request, "show_diff"), 1, "request {n}");
        assert!(
            [4, 5].contains(&n) || tool_message(&request).is_none(),
            "request {n}"
        );
    }
    let switched_back = body(&server, 3);
    let sentence = line_of("show_diff");
    assert!(
        switched_back["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "system")
            .any(|m| texts(m).iter().any(|t| t.contains(&sentence))),
        "a voided addition's message keeps its other blocks"
    );
    kept_prefix(&server, 3, 4);
    kept_prefix(&server, 5, 6);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_session_started_with_every_tool_held_turns_bash_on_with_one_rebuild_and_runs_it() {
    for names in [Vec::new(), vec!["none".to_owned()]] {
        let env = Env::new("rebuild-first-tool");
        let server = Server::start(vec![
            says("msg_01", 400, "One."),
            reply(
                "msg_02",
                520,
                vec![call(
                    "toolu_01",
                    "bash",
                    json!({"command": "echo first-tool"}),
                )],
                "tool_use",
            ),
            says("msg_03", 640, "Two."),
        ])
        .await;
        let store = MemoryStore::default();
        let kit = configured(&server, &env, &store, names.clone());
        let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent);
        session.turn("u1", "one").await;
        let answer = session.tools(1, vec!["bash".to_owned()]).await;
        let second = session.turn("u2", "two").await;
        let agent = session.end().await;
        assert_eq!(
            answered(&answer),
            (When::NextTurn, Some(tools_rebuilt())),
            "{names:?}"
        );
        let rebuilds_seen: Vec<Changed> = changed(&second)
            .into_iter()
            .filter(|(_, _, via, _)| *via == ToolChangeVia::Rebuild)
            .collect();
        assert_eq!(rebuilds_seen.len(), 1, "{names:?}: {:?}", changed(&second));
        assert!(rebuilds_seen[0].0.contains(&"bash".to_owned()), "{names:?}");
        assert!(
            body(&server, 0)["tools"]
                .as_array()
                .is_none_or(Vec::is_empty)
        );
        assert!(client_names(&body(&server, 1)).contains(&"bash".to_owned()));
        assert!(
            result_text(&body(&server, 2), "toolu_01").contains("first-tool"),
            "{names:?}"
        );
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_held_only_session_with_no_workspace_starts_without_tools() {
    let env = Env::new("rebuild-held-no-workspace");
    let server = Server::start(vec![says("msg_01", 400, "One.")]).await;
    let store = MemoryStore::default();
    let mut kit = configured(&server, &env, &store, vec!["none".to_owned()]);
    kit.workspace = Err("no such directory".to_owned());
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    assert!(matches!(
        built.workspace,
        Some(kit::WorkspaceNote::Missing(_))
    ));
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.end().await;
    assert!(
        body(&server, 0)["tools"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
}

#[tokio::test]
async fn a_trial_pair_whose_only_tool_blocks_are_voided_sends_no_beta_and_is_not_proven() {
    const TRIED: &str = "claude-opus-5";
    let env = Env::new("rebuild-void-trial");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let mut kit = on_a_gateway(&server, &env, &store);
    kit.start_layers = without_show_diff(&server, &env, &store).start_layers;
    kit.tools = without_show_diff(&server, &env, &store).tools;
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    change(&mut session, 2, model(PLAIN)).await;
    session.turn("u3", "three").await;
    change(&mut session, 3, model(TRIED)).await;
    session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(betas(&server, 1).contains(&INLINE_TOOLS_BETA.to_owned()));
    let tried = body(&server, 3);
    assert_eq!(tried["model"], TRIED);
    assert_eq!(declared(&tried, "show_diff"), 1);
    assert!(!betas(&server, 3).contains(&INLINE_TOOLS_BETA.to_owned()));
    let proven: Vec<String> = pick!(
        log_events(&store),
        LogEvent::InlineTools {
            model,
            state: wisp_protocol::log::FeatureState::Proven,
            ..
        } => model
    );
    assert_eq!(proven, [CAPABLE]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn web_search_turned_off_beside_a_noted_tool_is_named_in_order() {
    let env = Env::new("rebuild-search-note-order");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let names = without_search()
        .into_iter()
        .filter(|n| n != "write_todos")
        .collect();
    session.tools(1, names).await;
    session.turn("u2", "two").await;
    session.end().await;
    let (_, user) = last_two(&body(&server, 1));
    assert_eq!(
        texts(&user)[0],
        "Tools turned off: web_search, write_todos.\n"
    );
}

fn system_messages(request: &Value) -> Vec<Vec<String>> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .map(kinds)
        .collect()
}

#[tokio::test]
async fn a_rebuild_after_by_value_adds_and_a_removal_restores_the_tool_by_name() {
    let env = Env::new("rebuild-void-diff");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
        says("msg_05", 880, "Five."),
        reply(
            "msg_06",
            1_000,
            vec![call(
                "toolu_01",
                "show_diff",
                json!({"files": [{"path": "a.txt", "before": "a\n", "after": "b\n"}]}),
            )],
            "tool_use",
        ),
        says("msg_07", 1_100, "Six."),
        says("msg_08", 1_200, "Seven."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = configured(&server, &env, &store, vec!["none".to_owned()]);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let bash = || vec!["bash".to_owned()];
    let with = |extra: &[&str]| {
        let mut names = bash();
        names.extend(extra.iter().map(|n| (*n).to_owned()));
        names
    };
    session.tools(1, bash()).await;
    session.turn("u2", "two").await;
    session.tools(2, with(&["show_diff"])).await;
    session.turn("u3", "three").await;
    session.tools(3, bash()).await;
    session.turn("u4", "four").await;
    session.tools(4, with(&["show_diff"])).await;
    session.turn("u5", "five").await;
    session.tools(5, with(&["show_diff", SEARCH])).await;
    let flipped = session.turn("u6", "six").await;
    session.turn("u7", "seven").await;
    let agent = session.end().await;
    assert_eq!(by_value_additions(&body(&server, 2), "show_diff"), 1);
    assert_eq!(by_value_additions(&body(&server, 4), "show_diff"), 2);
    assert_eq!(changed(&flipped).len(), 1, "{:?}", changed(&flipped));
    let rebuilt = body(&server, 5);
    assert!(searches(&rebuilt));
    assert!(client_names(&rebuilt).contains(&"show_diff".to_owned()));
    assert_eq!(
        system_messages(&rebuilt),
        [
            vec!["text".to_owned()],
            vec!["tool_removal show_diff".to_owned()],
            vec!["tool_addition show_diff".to_owned()],
        ]
    );
    assert!(
        !result_text(&body(&server, 6), "toolu_01").contains(wisp_tool_text::switches::REFUSED)
    );
    kept_prefix(&server, 5, 6);
    kept_prefix(&server, 6, 7);
    rebuilds(&store, &agent).await;
}
