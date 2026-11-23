use wisp_tool_text::switches::{REFUSED, base_block};

use super::*;
use crate::resume::LoggedSession;
use wisp_protocol::log::ToolsCopy;
use wisp_protocol::settings::ENABLED_KEY;

fn rebuilt(agent: &Agent) -> bool {
    agent.cache_rebuild().is_some()
}

fn edited(store: &MemoryStore, edit: impl Fn(&mut Value)) -> MemoryStore {
    let fresh = MemoryStore::default();
    let mut log = fresh.clone();
    for record in records(store) {
        let mut value = serde_json::to_value(&record).unwrap();
        edit(&mut value);
        let record: LogRecord = serde_json::from_value(value).unwrap();
        log.append("s1", &record).unwrap();
    }
    fresh
}

async fn one_session(kit: &Kit, model: &str) -> Agent {
    let built = kit.build(on(model, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.end().await
}

fn removal_of(message: &Value) -> Vec<String> {
    kinds(message)
        .into_iter()
        .filter(|k| k.starts_with("tool_removal"))
        .collect()
}

#[tokio::test]
async fn a_resume_with_the_config_unchanged_sends_the_logged_prefix_and_nothing_else() {
    for model in [CAPABLE, PLAIN] {
        for off in [None, Some(SEARCH), Some("show_diff")] {
            let env = Env::new("resumed-unchanged");
            let server = Server::start(three()).await;
            let store = MemoryStore::default();
            let mut kit = switching(&server, &env, &store);
            if let Some(tool) = off {
                kit.tools = Arc::new(crate::plugin::Tools::new(
                    registry(Tools::Default),
                    without(tool).into_iter().collect(),
                ));
            }
            one_session(&kit, model).await;
            let agent = resumed(&kit, &store, &server, model).await;
            assert!(!rebuilt(&agent), "{model}, off={off:?}");
            let mut session = Session::start(agent);
            let second = session.turn("u2", "two").await;
            let agent = session.end().await;
            prefix::assert_prefix_stable(&[body(&server, 0), body(&server, 1)], None);
            assert!(
                changed(&second).is_empty(),
                "{model}, off={off:?}: {:?}",
                changed(&second)
            );
            assert_eq!(tool_message(&body(&server, 1)), None);
            assert_eq!(notes_in(&body(&server, 1)), 0);
            rebuilds(&store, &agent).await;
        }
    }
}

#[tokio::test]
async fn a_session_logged_before_provider_plugins_resumes_with_its_exact_prefix() {
    let env = Env::new("resumed-before-provider-plugins");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut old = switching(&server, &env, &store);
    let registry = registry_without(wisp_protocol::settings::PROVIDERS);
    old.tools = Arc::new(crate::plugin::Tools::new(
        registry.clone(),
        registry.default_on(),
    ));
    one_session(&old, CAPABLE).await;
    let current = switching(&server, &env, &store);
    let agent = resumed(&current, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    let agent = session.end().await;
    kept_prefix(&server, 0, 1);
    assert_eq!(tool_message(&body(&server, 1)), None);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resumed_preload_keeps_its_start_and_surfaces_tools_it_no_longer_holds() {
    for model in [CAPABLE, PLAIN] {
        for (before, now) in [("none", "none"), ("core", "none"), ("none", "core")] {
            let env = Env::new("resumed-preload");
            let server = Server::start(vec![
                says("msg_01", 400, "One."),
                reply(
                    "msg_02",
                    520,
                    vec![call(
                        "toolu_01",
                        "search",
                        json!({"query":"diff", "kind":"tool"}),
                    )],
                    "tool_use",
                ),
                says("msg_03", 640, "Two."),
            ])
            .await;
            let store = MemoryStore::default();
            let registry = registry(Tools::Default);
            let with_preload = |value: &str| {
                let mut kit = switching(&server, &env, &store);
                kit.tools = Arc::new(
                    crate::plugin::Tools::new(registry.clone(), registry.default_on())
                        .with_preload(vec![value.to_owned()]),
                );
                kit
            };
            one_session(&with_preload(before), model).await;
            let first = body(&server, 0);
            let loaded = |request: &Value| {
                client_names(request)
                    .into_iter()
                    .filter(|name| !deferred_in(request).contains(name))
                    .collect::<Vec<_>>()
            };
            if before == "none" {
                assert_eq!(loaded(&first), ["search", "use"]);
            }
            let agent = resumed(&with_preload(now), &store, &server, model).await;
            assert!(!rebuilt(&agent));
            let mut session = Session::start(agent);
            let events = session.turn("u2", "two").await;
            session.end().await;
            if before == "none" && now == "core" {
                let changes = changed(&events);
                assert_eq!(changes.len(), 1, "{events:?}");
                assert!(changes[0].0.contains(&SEARCH.to_owned()), "{changes:?}");
                assert!(changes[0].1.is_empty());
                assert_eq!(
                    changes[0].2,
                    if model == CAPABLE {
                        ToolChangeVia::Inline
                    } else {
                        ToolChangeVia::Rebuild
                    }
                );
                if model == CAPABLE {
                    kept_prefix(&server, 0, 1);
                    let message = tool_message(&body(&server, 1)).unwrap();
                    assert!(
                        by_value_in(&message)
                            .contains(&"tool_addition web_search by value".to_owned()),
                        "{message:#}"
                    );
                } else {
                    assert!(searches(&body(&server, 1)));
                    assert!(loaded(&body(&server, 1)).len() > loaded(&first).len());
                    assert_ne!(body(&server, 1)["system"], first["system"]);
                }
            } else {
                assert!(changed(&events).is_empty(), "{events:?}");
                kept_prefix(&server, 0, 1);
                assert_eq!(tool_message(&body(&server, 1)), None);
                assert_eq!(loaded(&body(&server, 2)), loaded(&first));
            }
            let result = result_text(&body(&server, 2), "toolu_01");
            if before == "none" && now == "none" {
                assert!(result.contains("run it with use"), "{result}");
            } else {
                assert_eq!(
                    result,
                    json!([{"type":"text", "text":"nothing found"}]).to_string()
                );
            }
        }
    }
}

#[tokio::test]
async fn a_tool_turned_off_in_the_config_while_closed_goes_as_a_removal_and_its_calls_are_refused()
{
    let env = Env::new("resumed-config-off");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        reply(
            "msg_02",
            520,
            vec![call("toolu_01", "show_diff", json!({}))],
            "tool_use",
        ),
        says("msg_03", 640, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), CAPABLE).await;
    let kit = configured(&server, &env, &store, without("show_diff"));
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    assert!(
        !agent
            .resume_notice_text()
            .unwrap_or_default()
            .contains("differ"),
        "{:?}",
        agent.resume_notice_text()
    );
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec!["show_diff".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    let agent = session.end().await;
    kept_prefix(&server, 0, 1);
    let message = tool_message(&body(&server, 1)).unwrap();
    assert_eq!(removal_of(&message), ["tool_removal show_diff"]);
    assert!(result_text(&body(&server, 2), "toolu_01").contains(REFUSED));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_tool_turned_off_in_the_config_while_closed_is_noted_where_blocks_cant_go() {
    let env = Env::new("resumed-config-off-noted");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), PLAIN).await;
    let kit = configured(&server, &env, &store, without("show_diff"));
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    assert_eq!(changed(&second).len(), 1, "{:?}", changed(&second));
    let agent = session.end().await;
    kept_prefix(&server, 0, 1);
    assert_eq!(notes_in(&body(&server, 1)), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_by_value_tool_whose_plugin_was_removed_leaves_the_wire_and_its_calls_are_refused() {
    let env = Env::new("resumed-gone-by-value");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        reply(
            "msg_03",
            640,
            vec![call("toolu_01", "show_diff", json!({}))],
            "tool_use",
        ),
        says("msg_04", 760, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.end().await;
    let kit = plugin_gone(&server, &env, &store, &["show-diff"]);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    assert_eq!(
        changed(&third),
        [(
            Vec::new(),
            vec!["show_code".to_owned(), "show_diff".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    let agent = session.end().await;
    kept_prefix(&server, 1, 2);
    let message = tool_message(&body(&server, 2)).unwrap();
    assert_eq!(
        removal_of(&message),
        ["tool_removal show_code", "tool_removal show_diff"]
    );
    assert!(result_text(&body(&server, 3), "toolu_01").contains(REFUSED));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn an_array_tool_whose_plugin_was_removed_is_noted_off_where_blocks_cant_go() {
    let env = Env::new("resumed-gone-noted");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), PLAIN).await;
    let kit = plugin_gone(&server, &env, &store, &["show-diff"]);
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    let agent = session.end().await;
    kept_prefix(&server, 0, 1);
    assert_eq!(notes_naming(&body(&server, 1), "show_code, show_diff"), 1);
    rebuilds(&store, &agent).await;
}

fn catalog_marks(agent: &Agent, name: &str) -> Option<(bool, bool)> {
    agent
        .catalog()
        .plugins
        .iter()
        .flat_map(|p| p.tools.iter())
        .find(|t| t.name == name)
        .map(|t| (t.on, t.waits))
}

#[tokio::test]
async fn the_catalog_after_a_resume_reads_the_session_before_the_first_message() {
    let env = Env::new("resumed-catalog");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let mut wanted = every_default();
    wanted.retain(|n| n != "view_file");
    session.tools(1, wanted).await;
    session.turn("u2", "two").await;
    session.end().await;
    let kit = plugin_gone(&server, &env, &store, &["todos"]);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert_eq!(catalog_marks(&agent, "view_file"), Some((false, false)));
    assert_eq!(catalog_marks(&agent, "show_diff"), Some((true, false)));
    assert_eq!(
        catalog_marks(&agent, "write_todos").map(|m| m.1),
        Some(false)
    );
}

fn started_copy(store: &MemoryStore) -> (Option<String>, ToolsCopy) {
    log_events(store)
        .into_iter()
        .find_map(|e| match e {
            LogEvent::SessionStarted {
                system_prompt,
                whole,
                ..
            } => Some((system_prompt, whole)),
            _ => None,
        })
        .unwrap()
}

fn folded_copy(store: &MemoryStore) -> (Option<String>, ToolsCopy) {
    let copy = LoggedSession::read(&records(store)).unwrap().copy;
    (copy.system_prompt, copy.tools)
}

fn digest_of(started: &Value, sent: &Value) -> String {
    let definitions = started["tool_definitions"].as_array().unwrap();
    let mut all: Vec<Value> = definitions.clone();
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

fn upgraded(store: &MemoryStore, server: &Server, name: &str) -> MemoryStore {
    let sent = body(server, 0);
    let digests = std::cell::RefCell::new(None);
    edited(store, |value| {
        if value["type"] != "session_started" {
            if let Some((old, new)) = digests.borrow().as_ref()
                && value["tools_sha256"] == *old
            {
                value["tools_sha256"] = json!(new);
            }
            return;
        }
        let old = digest_of(value, &sent);
        assert_eq!(value["tools_sha256"], json!(old));
        for definition in value["tool_definitions"].as_array_mut().unwrap() {
            if definition["name"] == name {
                let older = format!("{} Older.", definition["description"].as_str().unwrap());
                definition["description"] = json!(older);
            }
        }
        let new = digest_of(value, &sent);
        value["tools_sha256"] = json!(new);
        *digests.borrow_mut() = Some((json!(old), new));
    })
}

fn rebased(store: &MemoryStore, base: &str, older: &str, digest: bool) -> MemoryStore {
    edited(store, |value| {
        if value["type"] != "session_started" {
            return;
        }
        let prompt = value["system_prompt"]
            .as_str()
            .unwrap()
            .replacen(base, older, 1);
        value["system_prompt_sha256"] = json!(crate::session::sha256_hex(&prompt));
        value["system_prompt"] = json!(prompt);
        match (digest, value.as_object_mut()) {
            (true, _) => value["system_base_sha256"] = json!(crate::session::sha256_hex(older)),
            (false, Some(fields)) => {
                fields.remove("system_base_sha256");
            }
            (false, None) => {}
        }
    })
}

fn older(tools: &Value, name: &str) -> Value {
    let mut tools = tools.clone();
    for tool in tools.as_array_mut().unwrap() {
        if tool["name"] == name {
            let older = format!("{} Older.", tool["description"].as_str().unwrap());
            tool["description"] = json!(older);
        }
    }
    tools
}

fn search_refused() -> Reply {
    reply_error(
        "400 Bad Request",
        "invalid_request_error",
        "web search is not enabled for this organization",
    )
}

fn additions_in(message: &Value) -> Vec<String> {
    kinds(message)
        .into_iter()
        .filter(|k| k.starts_with("tool_addition"))
        .collect()
}

fn by_value_in(message: &Value) -> Vec<String> {
    kinds(message)
        .into_iter()
        .filter(|k| k.ends_with("by value"))
        .collect()
}

#[tokio::test]
async fn the_fold_reads_the_started_copy_past_both_copy_less_writers() {
    let env = Env::new("resumed-copy-less");
    let server = Server::start(vec![search_refused(), says("msg_01", 400, "One.")]).await;
    let store = MemoryStore::default();
    let mut agent = switching(&server, &env, &store)
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    agent.toolbox.drop_plugin("web-search");
    one_turn(agent, "u1").await;
    assert!(logged_kinds(&store).contains(&"tools_changed".to_owned()));
    assert_eq!(folded_copy(&store), started_copy(&store));

    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-4-7\"]\nweb_search = []\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(&mut session, 1, model(PLAIN)).await;
    session.turn("u2", "two").await;
    session.end().await;
    assert!(logged_kinds(&store).contains(&"tools_changed".to_owned()));
    assert_eq!(folded_copy(&store), started_copy(&store));
    let mut kit = configured(&server, &env, &store, without("show_diff"));
    kit.models = models("[models.\"claude-opus-4-7\"]\nweb_search = []\n");
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    assert_eq!(changed(&third).len(), 1, "{:?}", changed(&third));
    let agent = session.end().await;
    kept_prefix(&server, 1, 2);
    assert_eq!(notes_in(&body(&server, 2)), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resume_past_the_search_refusal_takes_a_config_change_as_a_removal() {
    let env = Env::new("resumed-after-refusal");
    let server = Server::start(vec![
        search_refused(),
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut agent = switching(&server, &env, &store)
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    agent.toolbox.drop_plugin("web-search");
    one_turn(agent, "u1").await;
    let kit = configured(&server, &env, &store, without("show_diff"));
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec!["show_diff".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    let agent = session.end().await;
    kept_prefix(&server, 1, 2);
    let message = tool_message(&body(&server, 2)).unwrap();
    assert_eq!(removal_of(&message), ["tool_removal show_diff"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_tool_turned_on_since_is_surfaced_by_reference_or_rebuilds_where_blocks_cant_go() {
    for model in [CAPABLE, PLAIN] {
        let env = Env::new("resumed-config-on");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        one_session(&without_show_diff(&server, &env, &store), model).await;
        let kit = configured(&server, &env, &store, every_default());
        let agent = resumed(&kit, &store, &server, model).await;
        assert!(!rebuilt(&agent), "{model}");
        let mut session = Session::start(agent);
        let second = session.turn("u2", "two").await;
        let agent = session.end().await;
        assert!(resumed_copies(&store).is_empty(), "{model}");
        if model == PLAIN {
            assert_eq!(
                changed(&second),
                [(
                    vec!["show_diff".to_owned()],
                    Vec::new(),
                    ToolChangeVia::Rebuild,
                    Some(tools_rebuilt())
                )]
            );
            assert!(client_names(&body(&server, 1)).contains(&"show_diff".to_owned()));
            assert_eq!(rebuild_copies(&store).len(), 1);
        }
        if model == CAPABLE {
            assert_eq!(
                changed(&second),
                [(
                    vec!["show_diff".to_owned()],
                    Vec::new(),
                    ToolChangeVia::Inline,
                    Some(CacheEffect::Kept)
                )]
            );
            kept_prefix(&server, 0, 1);
            let message = tool_message(&body(&server, 1)).unwrap();
            assert_eq!(additions_in(&message), ["tool_addition show_diff"]);
            let line = started(Tools::Default)
                .toolbox
                .system_line_of("show_diff")
                .unwrap();
            assert!(
                texts(&message).iter().any(|t| t.contains(&line)),
                "{message:#?}"
            );
        }
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn an_upgraded_definition_goes_once_as_a_same_name_addition_or_rebuilds() {
    for model in [CAPABLE, PLAIN] {
        let env = Env::new("resumed-upgraded");
        let mut replies = three();
        replies.push(says("msg_04", 760, "Four."));
        let server = Server::start(replies).await;
        let store = MemoryStore::default();
        let kit = switching(&server, &env, &store);
        one_session(&kit, model).await;
        let store = upgraded(&store, &server, "show_diff");
        let kit = switching(&server, &env, &store);
        let agent = resumed(&kit, &store, &server, model).await;
        assert!(!rebuilt(&agent), "{model}");
        let mut session = Session::start(agent);
        let second = session.turn("u2", "two").await;
        session.turn("u3", "three").await;
        let agent = session.end().await;
        if model == PLAIN {
            assert_eq!(
                changed(&second),
                [(
                    Vec::new(),
                    Vec::new(),
                    ToolChangeVia::Rebuild,
                    Some(tools_rebuilt())
                )]
            );
            assert_eq!(body(&server, 1)["tools"], body(&server, 0)["tools"]);
            assert_eq!(body(&server, 2)["tools"], body(&server, 1)["tools"]);
            assert_eq!(rebuild_copies(&store).len(), 1);
        }
        if model == CAPABLE {
            assert!(changed(&second).is_empty(), "{:?}", changed(&second));
            let first = body(&server, 1);
            assert_eq!(
                first["tools"],
                older(&body(&server, 0)["tools"], "show_diff")
            );
            let message = tool_message(&first).unwrap();
            assert_eq!(by_value_in(&message), ["tool_addition show_diff by value"]);
            assert_eq!(tool_message(&body(&server, 2)), None);
        }
        rebuilds(&store, &agent).await;
        let agent = resumed(&kit, &store, &server, model).await;
        assert!(!rebuilt(&agent), "{model}");
        let mut session = Session::start(agent);
        let fourth = session.turn("u4", "four").await;
        session.end().await;
        assert!(
            changed(&fourth).is_empty(),
            "{model}: {:?}",
            changed(&fourth)
        );
        assert_eq!(
            body(&server, 3)["tools"],
            body(&server, 2)["tools"],
            "{model}"
        );
        assert_eq!(
            body(&server, 3)["system"],
            body(&server, 2)["system"],
            "{model}"
        );
        assert_eq!(
            rebuild_copies(&store).len(),
            usize::from(model == PLAIN),
            "{model}"
        );
    }
}

fn long_file(env: &Env) -> String {
    std::fs::create_dir_all(env.work()).unwrap();
    let path = env.work().join("long.txt");
    let text: String = (1..=5000).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

#[tokio::test]
async fn bash_turned_off_since_keeps_view_file_as_logged_and_its_output_follows_this_run() {
    let env = Env::new("resumed-bash-off");
    let path = long_file(&env);
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        reply(
            "msg_02",
            520,
            vec![call("toolu_01", "view_file", json!({"path": path}))],
            "tool_use",
        ),
        says("msg_03", 640, "Two."),
        says("msg_04", 760, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), CAPABLE).await;
    let kit = configured(&server, &env, &store, without("bash"));
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec!["bash".to_owned(), "process".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    kept_prefix(&server, 0, 1);
    let message = tool_message(&body(&server, 1)).unwrap();
    assert_eq!(
        removal_of(&message),
        ["tool_removal bash", "tool_removal process"]
    );
    assert!(by_value_in(&message).is_empty(), "{message:#?}");
    let viewed = result_text(&body(&server, 2), "toolu_01");
    assert!(
        viewed.contains("only this part can be viewed in this session"),
        "{viewed}"
    );
    assert_eq!(tool_message(&body(&server, 3)), None);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn bash_turned_off_since_with_an_upgraded_view_file_adds_it_once() {
    let env = Env::new("resumed-bash-off-upgraded");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), CAPABLE).await;
    let store = upgraded(&store, &server, "view_file");
    let kit = configured(&server, &env, &store, without("bash"));
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let message = tool_message(&body(&server, 1)).unwrap();
    assert_eq!(
        removal_of(&message),
        ["tool_removal bash", "tool_removal process"]
    );
    assert_eq!(by_value_in(&message), ["tool_addition view_file by value"]);
    assert_eq!(tool_message(&body(&server, 2)), None);
    rebuilds(&store, &agent).await;
}

fn base_of(agent: &Agent) -> String {
    agent.prompt.base.clone().unwrap()
}

fn system_text(request: &Value) -> String {
    request["system"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect()
}

fn base_blocks(request: &Value) -> usize {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(texts)
        .filter(|t| wisp_tool_text::switches::is_base_block(t))
        .count()
}

async fn rebased_session(
    model: &str,
    replies: Vec<Reply>,
    older: impl Fn(&str) -> String,
    digest: bool,
) -> (Env, Server, MemoryStore, String) {
    let env = Env::new("resumed-base");
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let agent = one_session(&switching(&server, &env, &store), model).await;
    let base = base_of(&agent);
    let store = rebased(&store, &base, &older(&base), digest);
    (env, server, store, base)
}

#[tokio::test]
async fn a_changed_base_goes_once_as_a_block_where_blocks_can_go() {
    for model in [CAPABLE, PLAIN] {
        let (env, server, store, base) =
            rebased_session(model, three(), |_| "Older base.".to_owned(), true).await;
        let kit = switching(&server, &env, &store);
        let agent = resumed(&kit, &store, &server, model).await;
        assert!(!rebuilt(&agent), "{model}");
        let mut session = Session::start(agent);
        let second = session.turn("u2", "two").await;
        session.turn("u3", "three").await;
        let agent = session.end().await;
        assert!(changed(&second).is_empty(), "{:?}", changed(&second));
        let first = body(&server, 1);
        assert!(system_text(&first).starts_with("Older base."), "{model}");
        assert_eq!(body(&server, 2)["system"], first["system"]);
        if model == CAPABLE {
            let message = tool_message(&first).unwrap();
            assert_eq!(texts(&message), [base_block(&base)]);
            assert_eq!(base_blocks(&body(&server, 2)), 1, "kept in view");
            assert_eq!(tool_message(&body(&server, 2)), None);
        } else {
            assert_eq!(tool_message(&first), None);
            assert_eq!(base_blocks(&first), 0);
        }
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_base_that_lost_its_last_sentence_is_sent() {
    let (env, server, store, base) = rebased_session(
        CAPABLE,
        three(),
        |b| format!("{b} A sentence since cut."),
        true,
    )
    .await;
    let kit = switching(&server, &env, &store);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.end().await;
    let message = tool_message(&body(&server, 1)).unwrap();
    assert_eq!(texts(&message), [base_block(&base)]);
}

#[tokio::test]
async fn a_log_without_the_base_digest_reads_the_logged_prompt() {
    for (older, sent) in [
        (None, 0),
        (Some("Older base."), 1),
        (Some("Older base, then this base."), 1),
        (Some("{base}X, a longer base."), 1),
    ] {
        let older = move |b: &str| older.map_or_else(|| b.to_owned(), |o| o.replace("{base}", b));
        let (env, server, store, base) = rebased_session(CAPABLE, three(), older, false).await;
        let kit = switching(&server, &env, &store);
        let agent = resumed(&kit, &store, &server, CAPABLE).await;
        let mut session = Session::start(agent);
        session.turn("u2", "two").await;
        session.end().await;
        assert_eq!(base_blocks(&body(&server, 1)), sent, "{base}");
    }
}

#[tokio::test]
async fn a_base_block_dropped_by_a_compaction_is_restated() {
    let replies = vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
        says("msg_05", 760, "Four."),
    ];
    let (env, server, store, base) =
        rebased_session(CAPABLE, replies, |_| "Older base.".to_owned(), true).await;
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.compact(1).await;
    session.turn("u3", "three").await;
    session.turn("u4", "four").await;
    let agent = session.end().await;
    assert_eq!(base_blocks(&body(&server, 1)), 1);
    assert_eq!(
        texts(&tool_message(&body(&server, 3)).unwrap()),
        [base_block(&base)]
    );
    assert_eq!(tool_message(&body(&server, 4)), None);
    assert_eq!(base_blocks(&body(&server, 4)), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_retry_of_the_first_resumed_turn_resends_the_base() {
    let replies = vec![
        says("msg_01", 400, "One."),
        server_error(),
        says("msg_02", 520, "Two."),
    ];
    let (env, server, store, base) =
        rebased_session(CAPABLE, replies, |_| "Older base.".to_owned(), true).await;
    let mut kit = switching(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.send(ClientCommand::send("u2", "two")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session
        .send(ClientCommand::Retry {
            turn: 2,
            endpoint: None,
            request: RequestId(3),
        })
        .await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let agent = session.end().await;
    assert_eq!(
        texts(&tool_message(&body(&server, 1)).unwrap()),
        [base_block(&base)]
    );
    assert_eq!(
        texts(&tool_message(&body(&server, 2)).unwrap()),
        [base_block(&base)]
    );
    rebuilds(&store, &agent).await;
}

fn retold(store: &MemoryStore, from: &str, to: &str) -> MemoryStore {
    let quoted = |text: &str| {
        let json = serde_json::to_string(text).unwrap();
        json[1..json.len() - 1].to_owned()
    };
    let (from, to) = (quoted(from), quoted(to));
    edited(store, |value| {
        let text = serde_json::to_string(value).unwrap().replace(&from, &to);
        *value = serde_json::from_str(&text).unwrap();
    })
}

fn rehashed(store: &MemoryStore) -> MemoryStore {
    let digest = std::cell::RefCell::new(Value::Null);
    edited(store, |value| {
        if let Some(prompt) = value["system_prompt"].as_str() {
            *digest.borrow_mut() = json!(crate::session::sha256_hex(prompt));
        }
        if value.get("system_prompt_sha256").is_some() {
            value["system_prompt_sha256"] = digest.borrow().clone();
        }
    })
}

#[tokio::test]
async fn a_base_changed_and_back_goes_again_past_the_other_block() {
    let mut replies = three();
    replies.truncate(2);
    replies.push(says("msg_03", 640, "Three."));
    let (env, server, store, base) =
        rebased_session(CAPABLE, replies, |_| "Older base.".to_owned(), true).await;
    let kit = switching(&server, &env, &store);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.end().await;
    assert_eq!(
        texts(&tool_message(&body(&server, 1)).unwrap()),
        [base_block(&base)]
    );
    let store = edited(&store, |value| {
        if value["type"] == "turn_started"
            && let Some(blocks) = value.get_mut("tool_message").and_then(Value::as_array_mut)
        {
            blocks.push(json!({"type": "text", "text": base_block("Other base.")}));
        }
    });
    let store = rehashed(&retold(&store, "Older base.", &base));
    let kit = switching(&server, &env, &store);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    session.end().await;
    assert_eq!(
        texts(&tool_message(&body(&server, 2)).unwrap()),
        [base_block(&base)]
    );
}

#[tokio::test]
async fn a_line_the_logged_prompt_states_is_not_sent_again() {
    let env = Env::new("resumed-stated-line");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let agent = one_session(&without_show_diff(&server, &env, &store), CAPABLE).await;
    let line = started(Tools::Default)
        .toolbox
        .system_line_of("show_diff")
        .unwrap();
    let base = base_of(&agent);
    let store = rebased(&store, &base, &format!("{base} {line}"), false);
    let store = edited(&store, |value| {
        if value["type"] == "session_started" {
            value["system_base_sha256"] = json!(crate::session::sha256_hex(&base));
        }
    });
    let agent = resumed(&switching(&server, &env, &store), &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.end().await;
    let message = tool_message(&body(&server, 1)).unwrap();
    assert_eq!(kinds(&message), ["tool_addition show_diff"]);
}

#[tokio::test]
async fn a_tool_sentence_changed_since_the_log_stays_as_logged() {
    let line = started(Tools::Default)
        .toolbox
        .system_line_of("spawn_agent")
        .unwrap();
    let earlier = "An earlier sentence.";
    for model in [CAPABLE, PLAIN] {
        let env = Env::new("resumed-changed-sentence");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        one_session(&switching(&server, &env, &store), model).await;
        let store = rehashed(&retold(&store, &line, earlier));
        let kit = switching(&server, &env, &store);
        let agent = resumed(&kit, &store, &server, model).await;
        assert!(!rebuilt(&agent), "{model}");
        let mut session = Session::start(agent);
        let second = session.turn("u2", "two").await;
        let third = session.turn("u3", "three").await;
        let agent = session.end().await;
        let system = system_text(&body(&server, 1));
        assert!(system.contains(earlier), "{model}: {system}");
        assert!(!system.contains(&line), "{model}: {system}");
        assert_eq!(tool_message(&body(&server, 1)), None, "{model}");
        assert!(
            changed(&second).is_empty(),
            "{model}: {:?}",
            changed(&second)
        );
        assert!(changed(&third).is_empty(), "{model}: {:?}", changed(&third));
        prefix::assert_prefix_stable(&[body(&server, 1), body(&server, 2)], None);
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_line_mode_resume_with_a_difference_takes_the_rebuild() {
    let (env, server, store, _) =
        rebased_session(CAPABLE, three(), |_| "Older base.".to_owned(), true).await;
    let line_mode = Kit {
        start_layers: StartLayers::default(),
        ..switching(&server, &env, &store)
    };
    let agent = resumed(&line_mode, &store, &server, CAPABLE).await;
    assert!(rebuilt(&agent));
}

fn small_file(env: &Env) -> String {
    std::fs::create_dir_all(env.work()).unwrap();
    let path = env.work().join("small.txt");
    std::fs::write(&path, "first line\n").unwrap();
    path.to_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_continuing_retry_before_the_first_opening_runs_a_tool_turned_off_since() {
    let env = Env::new("resumed-retry-held");
    let path = small_file(&env);
    let server = Server::start(vec![
        reply(
            "msg_01",
            400,
            vec![call("toolu_01", "view_file", json!({"path": path}))],
            "tool_use",
        ),
        server_error(),
        reply(
            "msg_02",
            520,
            vec![call("toolu_02", "view_file", json!({"path": path}))],
            "tool_use",
        ),
        says("msg_03", 640, "Done."),
        says("msg_04", 760, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.send(ClientCommand::send("u1", "one")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session.end().await;
    let mut kit = configured(&server, &env, &store, without("view_file"));
    kit.retry_budget = Some(0.0);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session
        .send(ClientCommand::Retry {
            turn: 1,
            endpoint: None,
            request: RequestId(1),
        })
        .await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(body(&server, 2)["tools"], body(&server, 0)["tools"]);
    assert_eq!(tool_message(&body(&server, 2)), None);
    let viewed = result_text(&body(&server, 3), "toolu_02");
    assert!(!viewed.contains(REFUSED), "{viewed}");
    assert!(viewed.contains("first line"), "{viewed}");
    assert_eq!(changed(&second).len(), 1, "{:?}", changed(&second));
    let message = tool_message(&body(&server, 4)).unwrap();
    assert_eq!(removal_of(&message), ["tool_removal view_file"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_tool_turned_back_on_after_an_upgrade_goes_by_value() {
    let env = Env::new("resumed-back-on");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.end().await;
    let store = upgraded(&store, &server, "show_diff");
    let agent = resumed(&switching(&server, &env, &store), &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    session.tools(2, every_default()).await;
    session.turn("u4", "four").await;
    let agent = session.end().await;
    assert_eq!(tool_message(&body(&server, 2)), None);
    let message = tool_message(&body(&server, 3)).unwrap();
    assert_eq!(kinds(&message), ["tool_addition show_diff by value"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_removed_plugin_installed_again_with_a_changed_definition_goes_by_value() {
    let env = Env::new("resumed-reinstalled");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), CAPABLE).await;
    let gone = plugin_gone(&server, &env, &store, &["show-diff"]);
    let agent = resumed(&gone, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.end().await;
    let store = upgraded(&store, &server, "show_diff");
    let agent = resumed(&switching(&server, &env, &store), &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let message = tool_message(&body(&server, 2)).unwrap();
    assert_eq!(
        kinds(&message),
        [
            "tool_addition show_code",
            "tool_addition show_diff by value"
        ]
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_gone_endpoint_adopts_where_the_server_tools_match_and_rebuilds_where_not() {
    for (models_toml, rebuilds_now) in [
        ("", false),
        ("[models.\"claude-opus-5-5\"]\nweb_search = []\n", true),
    ] {
        let env = Env::new("resumed-endpoint-gone");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        one_session(&switching(&server, &env, &store), CAPABLE).await;
        let store = edited(&store, |value| {
            if value["type"] == "session_started" {
                value["endpoint_name"] = json!("gone");
            }
        });
        let mut kit = configured(&server, &env, &store, without("show_diff"));
        kit.models = models(models_toml);
        let agent = resumed(&kit, &store, &server, CAPABLE).await;
        assert_eq!(rebuilt(&agent), rebuilds_now, "{models_toml}");
    }
}

fn registry_failing(id: &str) -> Arc<crate::plugin::Registry> {
    use crate::plugin::{PluginConfig, Registry};
    let sources = wisp_plugins::bundled()
        .into_iter()
        .map(|b| {
            let make: crate::plugin::Make = if b.manifest == wisp_plugin_shell::MANIFEST {
                crate::tool::testkit::shell_make(wisp_plugin_shell::Timing::default())
            } else if b.manifest.contains(&format!("id = \"{id}\"")) {
                Arc::new(|_| panic!("this plugin fails to start"))
            } else {
                Arc::new(move |host| (b.instantiate)(host))
            };
            (b.manifest.to_owned(), make)
        })
        .collect();
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    Arc::new(registry)
}

#[tokio::test]
async fn a_plugin_that_failed_to_start_is_adopted_without_a_rebuild() {
    let env = Env::new("resumed-plugin-failed");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), CAPABLE).await;
    let registry = registry_failing("show-diff");
    let on = registry.default_on();
    let kit = Kit {
        tools: Arc::new(crate::plugin::Tools::new(registry, on)),
        ..switching(&server, &env, &store)
    };
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    let agent = session.end().await;
    kept_prefix(&server, 0, 1);
    let message = tool_message(&body(&server, 1)).unwrap();
    assert_eq!(
        removal_of(&message),
        ["tool_removal show_code", "tool_removal show_diff"]
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_redacted_copy_or_a_log_without_definitions_takes_todays_path() {
    for case in ["redacted", "no definitions"] {
        let env = Env::new("resumed-no-copy");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        let agent = one_session(&switching(&server, &env, &store), CAPABLE).await;
        let base = base_of(&agent);
        let store = edited(&store, |value| {
            if value["type"] != "session_started" {
                return;
            }
            if case == "redacted" {
                let prompt =
                    value["system_prompt"]
                        .as_str()
                        .unwrap()
                        .replacen(&base, "[REDACTED]", 1);
                value["system_prompt"] = json!(prompt);
            } else if let Some(fields) = value.as_object_mut() {
                fields.remove("tool_definitions");
            }
        });
        let kit = configured(&server, &env, &store, without("show_diff"));
        let agent = resumed(&kit, &store, &server, CAPABLE).await;
        assert!(rebuilt(&agent), "{case}");
        let mut session = Session::start(agent);
        session.turn("u2", "two").await;
        session.end().await;
        assert!(
            !system_text(&body(&server, 1)).contains("[REDACTED]"),
            "{case}"
        );
        assert_eq!(tool_message(&body(&server, 1)), None, "{case}");
    }
}

#[tokio::test]
async fn a_resume_reads_the_copy_a_rebuilding_resume_wrote() {
    let env = Env::new("resumed-from-resumed");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), CAPABLE).await;
    let line_mode = Kit {
        start_layers: StartLayers {
            flags: vec![removed_flag("show-diff")],
            ..StartLayers::default()
        },
        ..plugin_gone(&server, &env, &store, &["show-diff"])
    };
    let agent = resumed(&line_mode, &store, &server, CAPABLE).await;
    assert!(rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.end().await;
    assert_ne!(folded_copy(&store), started_copy(&store));
    let agent = resumed(&switching(&server, &env, &store), &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    let agent = session.end().await;
    kept_prefix(&server, 1, 2);
    let message = tool_message(&body(&server, 2)).unwrap();
    assert_eq!(
        by_value_in(&message),
        [
            "tool_addition show_code by value",
            "tool_addition show_diff by value"
        ]
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_13a_era_resume_over_a_by_value_addition_keeps_the_bytes_as_logged() {
    let env = Env::new("resumed-13a-era");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.end().await;
    let line_mode = Kit {
        start_layers: StartLayers::default(),
        ..switching(&server, &env, &store)
    };
    let agent = resumed(&line_mode, &store, &server, CAPABLE).await;
    assert!(rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    session.end().await;
    let store = edited(&store, |value| {
        if let Some(fields) = value.as_object_mut() {
            fields.remove("system_base_sha256");
        }
    });
    let agent = resumed(&switching(&server, &env, &store), &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    kept_prefix(&server, 2, 3);
    assert_eq!(tool_message(&body(&server, 3)), None);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn the_summary_fork_and_keep_warm_send_the_adopted_bytes() {
    let replies = vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        super::super::super::keep_warm::refreshes_read("msg_r1", 40_100),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 760, "Three."),
    ];
    let (env, server, store, _) =
        rebased_session(CAPABLE, replies, |_| "Older base.".to_owned(), true).await;
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent.with_keep_warm(8));
    session.turn("u2", "two").await;
    session.refreshed().await;
    session.compact(1).await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (turn, refresh, summary) = (body(&server, 1), body(&server, 2), body(&server, 3));
    let strip = |mut value: Value| {
        if let Some(map) = value.as_object_mut() {
            map.remove("max_tokens");
            map.remove("fallbacks");
        }
        value
    };
    assert_eq!(strip(refresh), strip(turn.clone()));
    assert_eq!(summary["system"], turn["system"]);
    assert_eq!(summary["tools"], turn["tools"]);
    assert!(system_text(&summary).starts_with("Older base."));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resume_over_a_wisp_compaction_sends_nothing_new() {
    let env = Env::new("resumed-over-compaction");
    let server = Server::start(vec![
        says("msg_01", 40_000, "One."),
        reply("msg_02", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_03", 640, "Two."),
        says("msg_04", 760, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.compact(1).await;
    session.turn("u2", "two").await;
    session.end().await;
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(changed(&third).is_empty());
    kept_prefix(&server, 2, 3);
    assert_eq!(tool_message(&body(&server, 3)), None);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resumed_difference_emits_tools_changed_and_its_restatement_after_a_compaction_none() {
    let env = Env::new("resumed-events");
    let server = Server::start(vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let native_off = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
    let mut kit = switching(&server, &env, &store);
    kit.models = models(native_off);
    one_session(&kit, CAPABLE).await;
    let mut kit = configured(&server, &env, &store, without("show_diff"));
    kit.models = models(native_off);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    session.compact(1).await;
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec!["show_diff".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    assert_eq!(
        removal_of(&tool_message(&body(&server, 1)).unwrap()),
        ["tool_removal show_diff"]
    );
    assert_eq!(
        removal_of(&tool_message(&body(&server, 3)).unwrap()),
        ["tool_removal show_diff"]
    );
    let tools_changed = logged_kinds(&store)
        .iter()
        .filter(|k| *k == "tools_changed")
        .count();
    assert_eq!(tools_changed, 1, "{:?}", logged_kinds(&store));
    rebuilds(&store, &agent).await;
}

fn removals_anywhere(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(kinds)
        .filter(|k| k.starts_with("tool_removal"))
        .collect()
}

fn saved_tools(names: Vec<String>) -> StartLayers {
    use wisp_protocol::settings::{Place, SitePath, find};
    let mut layers = StartLayers {
        full_screen: true,
        ..StartLayers::default()
    };
    layers.saved.insert(
        SitePath::new(Place::Global, find("tools").unwrap()),
        SettingValue::List(names),
        1,
    );
    layers
}

fn saved_without(
    server: &Server,
    env: &Env,
    store: &MemoryStore,
    tool: &str,
    models_toml: &str,
) -> Kit {
    let names = without(tool);
    Kit {
        start_layers: saved_tools(names.clone()),
        tools: Arc::new(crate::plugin::Tools::new(
            registry(Tools::Default),
            registry(Tools::Default).on_tools_with(
                &names.into_iter().collect(),
                &std::collections::BTreeSet::new(),
            ),
        )),
        models: models(models_toml),
        ..switching(server, env, store)
    }
}

#[tokio::test]
async fn a_pre_turn_compaction_before_the_first_resumed_opening_keeps_a_tool_on_for_the_session() {
    let env = Env::new("resumed-held-compacted");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 40_000, "Two."),
        reply("msg_03", 40_100, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let native_off = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
    let kit = saved_without(&server, &env, &store, "view_file", native_off);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    let second = session.turn("u2", "two").await;
    session.end().await;
    assert_eq!(changed(&second).len(), 1, "{:?}", changed(&second));
    let banded = format!("{native_off}compact_at = 20000\n");
    let kit = saved_without(&server, &env, &store, "view_file", &banded);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(
        third
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{third:#?}"
    );
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    let opened = body(&server, 3);
    assert_eq!(opened["tools"], body(&server, 1)["tools"]);
    assert!(removals_anywhere(&opened).is_empty(), "{opened:#?}");
    let message = tool_message(&opened).unwrap();
    assert_eq!(additions_in(&message), ["tool_addition view_file"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_background_compaction_before_the_close_is_restated_at_the_first_resumed_opening() {
    let env = Env::new("resumed-background");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 75_000, "Three."),
        reply(
            "msg_04",
            1_200,
            vec![Part::Compaction("So far: three turns.")],
            "compaction",
        ),
        server_error(),
        says("msg_05", 900, "Five."),
    ])
    .await;
    let store = MemoryStore::default();
    let banded = |kit: Kit| {
        let mut start_layers = crate::agent::tests::golden_session::background_layers(30_000);
        start_layers.full_screen = true;
        Kit {
            start_layers,
            retry_budget: Some(0.0),
            models: models("[models.\"claude-opus-5-5\"]\ncompact_at = 100000\n"),
            ..kit
        }
    };
    let kit = banded(switching(&server, &env, &store));
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    session
        .until(|e| matches!(e, AgentEvent::CompactionPrepared { .. }))
        .await;
    session.send(ClientCommand::send("u4", "four")).await;
    let failed = session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        failed
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{failed:#?}"
    );
    session.end().await;
    assert_eq!(
        removal_of(&tool_message(&body(&server, 1)).unwrap()),
        ["tool_removal show_diff"]
    );
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let fifth = session.turn("u5", "five").await;
    let agent = session.end().await;
    assert!(changed(&fifth).is_empty(), "{:?}", changed(&fifth));
    let first = body(&server, 5);
    assert_eq!(first["system"], body(&server, 1)["system"]);
    assert_eq!(first["tools"], body(&server, 1)["tools"]);
    assert_eq!(
        removal_of(&tool_message(&first).unwrap()),
        ["tool_removal show_diff"]
    );
    assert_eq!(removals_anywhere(&first).len(), 1, "{first:#?}");
    rebuilds(&store, &agent).await;
}

fn diff_call(id: &'static str) -> Part {
    call(
        id,
        "show_diff",
        json!({"files": [{"path": "a.txt", "before": "a\n", "after": "b\n"}]}),
    )
}

fn waits(agent: &Agent, name: &str) -> bool {
    catalog_marks(agent, name).is_some_and(|(_, waits)| waits)
}

#[tokio::test]
async fn an_upgraded_tool_stays_usable_after_a_switch_to_a_plain_pair() {
    for switch_first in [false, true] {
        let env = Env::new("resumed-upgraded-plain");
        let server = Server::start(vec![
            says("msg_01", 400, "One."),
            says("msg_02", 520, "Two."),
            reply("msg_03", 640, vec![diff_call("toolu_01")], "tool_use"),
            says("msg_04", 760, "Shown."),
            says("msg_05", 880, "Four."),
            says("msg_06", 1000, "Five."),
        ])
        .await;
        let store = MemoryStore::default();
        let nothing_deferred = |store: &MemoryStore| {
            let mut kit = Kit {
                tools: Arc::new(crate::plugin::Tools::new(
                    registry_without(&["page"]),
                    registry_without(&["page"]).default_on(),
                )),
                ..switching(&server, &env, store)
            };
            kit.start_layers.flags.push(removed_flag("page"));
            kit
        };
        one_session(&nothing_deferred(&store), CAPABLE).await;
        let store = upgraded(&store, &server, "show_diff");
        let agent = resumed(&nothing_deferred(&store), &store, &server, CAPABLE).await;
        assert!(!rebuilt(&agent), "{switch_first}");
        let mut session = Session::start(agent);
        let (second, third) = if switch_first {
            change(&mut session, 1, model(PLAIN)).await;
            let second = session.turn("u2", "two").await;
            let third = session.turn("u3", "three").await;
            (second, third)
        } else {
            let second = session.turn("u2", "two").await;
            change(&mut session, 1, model(PLAIN)).await;
            let third = session.turn("u3", "three").await;
            (second, third)
        };
        change(&mut session, 2, model(CAPABLE)).await;
        session.turn("u4", "four").await;
        session.turn("u5", "five").await;
        let agent = session.end().await;
        let additions: Vec<usize> = (1..=5)
            .filter(|&index| {
                tool_message(&body(&server, index))
                    .is_some_and(|m| by_value_in(&m) == ["tool_addition show_diff by value"])
            })
            .collect();
        let expected: &[usize] = if switch_first { &[] } else { &[1] };
        assert_eq!(additions, expected, "{switch_first}");
        let after = tool_message(&body(&server, 5));
        assert!(after.is_none(), "{switch_first} {after:?}");
        let at = if switch_first { 1 } else { 2 };
        assert_eq!(body(&server, at)["tools"], body(&server, 0)["tools"]);
        assert_eq!(body(&server, 4)["tools"], body(&server, at)["tools"]);
        assert_eq!(body(&server, 4)["system"], body(&server, at)["system"]);
        if switch_first {
            kept_prefix(&server, 1, 4);
        }
        kept_prefix(&server, 4, 5);
        let (rebuilt_turn, quiet) = if switch_first {
            (&second, &third)
        } else {
            (&third, &second)
        };
        assert_eq!(
            changed(rebuilt_turn),
            [(
                Vec::new(),
                Vec::new(),
                ToolChangeVia::Rebuild,
                Some(tools_rebuilt())
            )],
            "{switch_first}"
        );
        assert!(changed(quiet).is_empty(), "{:?}", changed(quiet));
        assert_eq!(rebuild_copies(&store).len(), 1, "{switch_first}");
        assert!(!waits(&agent, "show_diff"), "{switch_first}");
        let shown = result_text(&body(&server, 3), "toolu_01");
        assert!(!shown.contains(REFUSED), "{shown}");
        assert!(!shown.contains("no tool named"), "{shown}");
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_model_switch_pending_at_the_close_decides_the_resume_on_its_pair() {
    let env = Env::new("resumed-pending-plain");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(&mut session, 1, model(PLAIN)).await;
    session.end().await;
    let kit = configured(&server, &env, &store, every_default());
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(body(&server, 1)["model"], PLAIN);
    assert!(!waits(&agent, "show_diff"));
    assert_eq!(
        changed(&second),
        [(
            vec!["show_diff".to_owned()],
            Vec::new(),
            ToolChangeVia::Rebuild,
            Some(tools_rebuilt())
        )]
    );
    assert!(client_names(&body(&server, 1)).contains(&"show_diff".to_owned()));
    assert_eq!(marked(&store), ["\"tools_change\""]);
}

#[tokio::test]
async fn the_copy_is_checked_against_the_pending_pairs_tools() {
    let env = Env::new("resumed-pending-digest");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let plain_without_search = "[models.\"claude-opus-4-7\"]\nweb_search = []\n";
    let mut kit = switching(&server, &env, &store);
    kit.models = models(plain_without_search);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(&mut session, 1, model(PLAIN)).await;
    session.end().await;
    let mut kit = configured(&server, &env, &store, without("show_diff"));
    kit.models = models(plain_without_search);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.end().await;
    let first = body(&server, 1);
    assert_eq!(first["model"], PLAIN);
    assert_eq!(notes_in(&first), 1);
}

fn failing_retry_session(first: Vec<Part>) -> Vec<Reply> {
    vec![
        reply("msg_01", 400, first, "tool_use"),
        server_error(),
        reply("msg_02", 520, vec![diff_call("toolu_02")], "tool_use"),
        says("msg_03", 640, "Done."),
    ]
}

async fn retried(session: &mut Session, turn: u64) {
    session
        .send(ClientCommand::Retry {
            turn,
            endpoint: None,
            request: RequestId(9),
        })
        .await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
}

#[tokio::test]
async fn a_call_to_an_array_tool_whose_plugin_was_removed_is_refused_in_a_continuing_retry() {
    let env = Env::new("resumed-guard-gone");
    let path = small_file(&env);
    let server = Server::start(failing_retry_session(vec![call(
        "toolu_01",
        "view_file",
        json!({"path": path}),
    )]))
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.send(ClientCommand::send("u1", "one")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session.end().await;
    let mut kit = plugin_gone(&server, &env, &store, &["show-diff"]);
    kit.retry_budget = Some(0.0);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    retried(&mut session, 1).await;
    session.end().await;
    let refused = result_text(&body(&server, 3), "toolu_02");
    assert!(refused.contains(REFUSED), "{refused}");
}

#[tokio::test]
async fn a_call_to_an_array_tool_the_offer_holds_off_is_refused_in_a_continuing_retry() {
    let env = Env::new("resumed-guard-held");
    let path = small_file(&env);
    let mut replies = vec![says("msg_00", 300, "Zero.")];
    replies.extend(failing_retry_session(vec![call(
        "toolu_01",
        "view_file",
        json!({"path": path}),
    )]));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u0", "zero").await;
    session.tools(1, without("show_diff")).await;
    session.send(ClientCommand::send("u1", "one")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session.end().await;
    let mut kit = configured(&server, &env, &store, without("show_diff"));
    kit.retry_budget = Some(0.0);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    retried(&mut session, 2).await;
    session.end().await;
    let refused = result_text(&body(&server, 4), "toolu_02");
    assert!(refused.contains(REFUSED), "{refused}");
}

#[tokio::test]
async fn a_continuing_retry_runs_a_tool_on_for_the_session_only() {
    let env = Env::new("resumed-retry-session-only");
    let path = small_file(&env);
    let server = Server::start(vec![
        says("msg_00", 300, "Zero."),
        reply(
            "msg_01",
            400,
            vec![call("toolu_01", "view_file", json!({"path": path}))],
            "tool_use",
        ),
        server_error(),
        reply(
            "msg_02",
            520,
            vec![call("toolu_02", "view_file", json!({"path": path}))],
            "tool_use",
        ),
        says("msg_03", 640, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let native_off = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
    let mut kit = saved_without(&server, &env, &store, "view_file", native_off);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u0", "zero").await;
    session.tools(1, every_default()).await;
    session.send(ClientCommand::send("u1", "one")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session.end().await;
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    retried(&mut session, 2).await;
    let agent = session.end().await;
    let viewed = result_text(&body(&server, 4), "toolu_02");
    assert!(!viewed.contains(REFUSED), "{viewed}");
    assert!(viewed.contains("first line"), "{viewed}");
    rebuilds(&store, &agent).await;
}

async fn compacted_before_close(kit: &Kit, wanted: Vec<String>) {
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, wanted).await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    session.end().await;
}

fn compaction_replies() -> Vec<Reply> {
    vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
    ]
}

const NATIVE_OFF: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";

#[tokio::test]
async fn a_change_a_compaction_dropped_just_before_the_close_is_restated_with_no_event() {
    for added in [true, false] {
        let env = Env::new("resumed-compacted-close");
        let server = Server::start(compaction_replies()).await;
        let store = MemoryStore::default();
        let kit = if added {
            saved_without(&server, &env, &store, "show_diff", NATIVE_OFF)
        } else {
            Kit {
                models: models(NATIVE_OFF),
                ..switching(&server, &env, &store)
            }
        };
        let wanted = if added {
            every_default()
        } else {
            without("show_diff")
        };
        compacted_before_close(&kit, wanted).await;
        let agent = resumed(&kit, &store, &server, CAPABLE).await;
        assert!(!rebuilt(&agent), "{added}");
        let mut session = Session::start(agent);
        let third = session.turn("u3", "three").await;
        let agent = session.end().await;
        assert!(changed(&third).is_empty(), "{added}: {:?}", changed(&third));
        let restated = kinds(&tool_message(&body(&server, 3)).unwrap());
        let expected = if added {
            "tool_addition show_diff"
        } else {
            "tool_removal show_diff"
        };
        assert!(restated.iter().any(|k| k == expected), "{restated:?}");
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_by_value_tool_a_compaction_dropped_leaves_with_its_event_when_its_plugin_is_gone() {
    let env = Env::new("resumed-compacted-gone");
    let server = Server::start(compaction_replies()).await;
    let store = MemoryStore::default();
    let kit = saved_without(&server, &env, &store, "show_diff", NATIVE_OFF);
    compacted_before_close(&kit, every_default()).await;
    let registry = registry_without(&["show-diff"]);
    let defaults = registry.default_on();
    let mut gone = Kit {
        tools: Arc::new(crate::plugin::Tools::new(registry, defaults)),
        ..saved_without(&server, &env, &store, "show_diff", NATIVE_OFF)
    };
    gone.start_layers.flags.push(removed_flag("show-diff"));
    let agent = resumed(&gone, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(
        changed(&third),
        [(
            Vec::new(),
            vec!["show_code".to_owned(), "show_diff".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    assert!(
        by_value_in(
            &body(&server, 3)["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .clone()
        )
        .is_empty()
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_rewound_change_stays_applied_across_a_resume() {
    let env = Env::new("resumed-rewound");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = saved_without(&server, &env, &store, "show_diff", NATIVE_OFF);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(2),
        })
        .await;
    session
        .until(|e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    session.end().await;
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    assert_eq!(
        additions_in(&tool_message(&body(&server, 2)).unwrap()),
        ["tool_addition show_diff"]
    );
    rebuilds(&store, &agent).await;
}

fn strategy_registry() -> Arc<crate::plugin::Registry> {
    use wisp_plugin_testkit::hooks::{self as hk, Answer, StrategyCalled};
    let mut sources: Vec<(String, crate::plugin::Make)> = wisp_plugins::bundled()
        .into_iter()
        .map(|b| {
            let make: crate::plugin::Make = if b.manifest == wisp_plugin_shell::MANIFEST {
                crate::tool::testkit::shell_make(wisp_plugin_shell::Timing::default())
            } else {
                Arc::new(move |host| (b.instantiate)(host))
            };
            (b.manifest.to_owned(), make)
        })
        .collect();
    let make: crate::plugin::Make = hk::strategising(
        hk::none(),
        None,
        Arc::new(|_, called| {
            let answer = match called {
                StrategyCalled::Instruction(_) => Answer::Instruction(None),
                StrategyCalled::Accept(..) => Answer::Accept(Ok(())),
                StrategyCalled::Head(..) => Answer::Head(vec!["Strategy head.".to_owned()]),
            };
            Box::pin(async move { answer })
        }),
        hk::Calls::default(),
    );
    sources.push((hk::manifest_in("0.2.0", "aa", &hk::strategy_body()), make));
    let (registry, problems) =
        crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    Arc::new(registry)
}

#[tokio::test]
async fn a_resume_over_a_strategy_compaction_sends_nothing_new() {
    use wisp_protocol::settings::{Place, SitePath, find};
    let env = Env::new("resumed-strategy");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 40_000, "Two."),
        reply("msg_03", 40_100, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
        says("msg_05", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let registry = strategy_registry();
    let defaults = registry.default_on();
    let mut start_layers = StartLayers {
        full_screen: true,
        ..StartLayers::default()
    };
    start_layers.saved.insert(
        SitePath::new(Place::Global, find("context.strategy").unwrap()),
        SettingValue::Text("aa".to_owned()),
        1,
    );
    let kit = Kit {
        start_layers,
        tools: Arc::new(crate::plugin::Tools::new(registry, defaults)),
        models: models(NATIVE_OFF),
        ..switching(&server, &env, &store)
    };
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    session.turn("u3", "three").await;
    session.end().await;
    let compacted = log_events(&store).into_iter().any(|e| {
        matches!(
            e,
            LogEvent::ContextCompacted {
                strategy: Some(_),
                ..
            }
        )
    });
    assert!(compacted, "{:?}", logged_kinds(&store));
    assert_eq!(
        removal_of(&tool_message(&body(&server, 3)).unwrap()),
        ["tool_removal show_diff"]
    );
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    kept_prefix(&server, 3, 4);
    assert_eq!(tool_message(&body(&server, 4)), None);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_failed_step_logs_the_betas_it_sent_and_an_older_one_without_them_is_skipped() {
    let env = Env::new("resumed-failed-betas");
    let server = Server::start(vec![says("msg_01", 400, "One."), server_error()]).await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.send(ClientCommand::send("u2", "two")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session.end().await;
    let steps: Vec<Value> = records(&store)
        .iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .filter(|v| v["type"] == "step_ended")
        .collect();
    assert_eq!(steps.len(), 2, "{steps:#?}");
    assert_eq!(steps[1]["accepted"], json!(false), "{steps:#?}");
    assert_eq!(steps[1]["betas"], steps[0]["betas"], "{steps:#?}");
    let older = edited(&store, |value| {
        if value["type"] == "step_ended"
            && value["accepted"] == json!(false)
            && let Some(fields) = value.as_object_mut()
        {
            fields.remove("betas");
        }
    });
    let agent = resumed(&kit, &older, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
}

#[tokio::test]
async fn an_older_refused_step_without_betas_is_skipped() {
    let env = Env::new("resumed-refused-betas");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        reply("msg_02", 400, vec![Part::Text("No.")], "refusal"),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.send(ClientCommand::send("u2", "two")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session.end().await;
    let records: Vec<Value> = records(&store)
        .iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .collect();
    let steps: Vec<&Value> = records
        .iter()
        .filter(|v| v["type"] == "step_ended")
        .collect();
    assert_eq!(steps.len(), 2, "{steps:#?}");
    assert_eq!(steps[1]["accepted"], Value::Null, "{steps:#?}");
    assert!(steps[1]["betas"].is_array(), "{steps:#?}");
    assert!(
        !records.iter().any(|v| v["type"] == "message_committed"
            && v["turn"] == json!(2)
            && v["message"]["role"] == "assistant"),
        "{records:#?}"
    );
    let last = steps[1]["seq"].clone();
    let older = edited(&store, |value| {
        if value["type"] == "step_ended"
            && value["seq"] == last
            && let Some(fields) = value.as_object_mut()
        {
            fields.remove("betas");
        }
    });
    let agent = resumed(&kit, &older, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
}

fn tool_changes_in(store: &MemoryStore) -> usize {
    records(store)
        .iter()
        .filter(|r| matches!(r.event, LogEvent::ToolsChanged { .. }))
        .count()
}

#[tokio::test]
async fn a_tool_noted_off_before_a_rebuilding_resume_stays_off_at_the_next_one() {
    for stripped in [false, true] {
        let env = Env::new("resumed-applied-after-rebuild");
        let path = small_file(&env);
        let viewing = |id: &'static str| call(id, "view_file", json!({"path": path}));
        let server = Server::start(vec![
            says("msg_00", 300, "Zero."),
            says("msg_01", 400, "One."),
            reply("msg_02", 520, vec![viewing("toolu_02")], "tool_use"),
            server_error(),
            reply("msg_03", 640, vec![diff_call("toolu_03")], "tool_use"),
            says("msg_04", 760, "Done."),
        ])
        .await;
        let store = MemoryStore::default();
        let mut kit = switching(&server, &env, &store);
        kit.retry_budget = Some(0.0);
        let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent);
        session.turn("u0", "zero").await;
        session.tools(1, without("show_diff")).await;
        session.turn("u1", "one").await;
        session.end().await;
        let store = upgraded(&store, &server, "view_file");
        let mut kit = switching(&server, &env, &store);
        kit.retry_budget = Some(0.0);
        let agent = resumed(&kit, &store, &server, PLAIN).await;
        assert!(!rebuilt(&agent), "{stripped}");
        let mut session = Session::start(agent);
        session.send(ClientCommand::send("u2", "two")).await;
        session
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        session.end().await;
        assert!(resumed_copies(&store).is_empty(), "{stripped}");
        let copies = rebuild_copies(&store);
        assert_eq!(copies.len(), 1, "{copies:?}");
        assert!(copies[0].0.contains(&"show_diff".to_owned()), "{copies:?}");
        let on = copies[0].1.clone().unwrap();
        assert!(!on.contains(&"show_diff".to_owned()), "{on:?}");
        assert!(on.contains(&"view_file".to_owned()), "{on:?}");
        assert!(!on.contains(&"web_search".to_owned()), "{on:?}");
        let store = if stripped {
            edited(&store, |value| {
                if value["type"] == "tools_changed"
                    && let Some(fields) = value.as_object_mut()
                {
                    fields.remove("tools_on");
                }
            })
        } else {
            store
        };
        let before = tool_changes_in(&store);
        let mut kit = switching(&server, &env, &store);
        kit.retry_budget = Some(0.0);
        let agent = resumed(&kit, &store, &server, PLAIN).await;
        assert!(!rebuilt(&agent), "{stripped}");
        let mut session = Session::start(agent);
        retried(&mut session, 3).await;
        session.end().await;
        assert_eq!(tool_changes_in(&store), before, "{stripped}");
        let refused = result_text(&body(&server, 5), "toolu_03");
        assert!(refused.contains(REFUSED), "{stripped} {refused}");
    }
}

fn tools_rebuilt() -> CacheEffect {
    CacheEffect::Rebuilt {
        reason: wisp_protocol::RebuildReason::ToolsChange,
    }
}

fn copy_names(definitions: &[Value]) -> Vec<String> {
    definitions
        .iter()
        .filter_map(|d| d["name"].as_str().map(str::to_owned))
        .collect()
}

fn resumed_copies(store: &MemoryStore) -> Vec<(Vec<String>, Option<Vec<String>>)> {
    pick!(
        log_events(store),
        LogEvent::SessionResumed {
            whole,
            tools_on,
            system_prompt: Some(_),
            ..
        } => (copy_names(&whole.tool_definitions), tools_on)
    )
}

fn rebuild_copies(store: &MemoryStore) -> Vec<(Vec<String>, Option<Vec<String>>)> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::ToolsChanged {
                via: ToolChangeVia::Rebuild,
                copy,
                ..
            } if copy.system_prompt.is_some() => {
                Some((copy_names(&copy.whole.tool_definitions), copy.tools_on))
            }
            _ => None,
        })
        .collect()
}

async fn rebuilt_session(kit: &Kit, model: &str) -> Agent {
    let built = kit.build(on(model, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let answer = session.tools(1, every_default()).await;
    let second = session.turn("u2", "two").await;
    assert_eq!(changed(&second).len(), 1, "{answer:#?}");
    assert_eq!(changed(&second)[0].2, ToolChangeVia::Rebuild);
    session.end().await
}

#[tokio::test]
async fn a_resume_after_a_rebuild_adopts_its_copy_and_sends_the_same_bytes() {
    let env = Env::new("rebuilt-resume");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    rebuilt_session(&without_show_diff(&server, &env, &store), PLAIN).await;
    assert_eq!(rebuild_copies(&store).len(), 1);
    let kit = configured(&server, &env, &store, every_default());
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    assert_eq!(agent.whole_tools().history_len, 2);
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    kept_prefix(&server, 1, 2);
    assert!(resumed_copies(&store).is_empty());
    assert_eq!(rebuild_copies(&store).len(), 1);
    rebuilds(&store, &agent).await;
}

fn thinking_cuts(store: &MemoryStore) -> usize {
    records(store)
        .iter()
        .filter(|r| matches!(r.event, LogEvent::ThinkingCut { .. }))
        .count()
}

#[tokio::test]
async fn two_resumes_after_a_rebuild_take_no_thinking_cut_where_binding_controls_cant_go() {
    let env = Env::new("rebuilt-two-resumes");
    let mut replies = vec![reply_error(
        "400 Bad Request",
        "invalid_request_error",
        "thinking.block_binding: Extra inputs are not permitted",
    )];
    replies.extend(three());
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    rebuilt_session(&without_show_diff(&server, &env, &store), PLAIN).await;
    let cuts = thinking_cuts(&store);
    assert_eq!(cuts, 1);
    for (n, text) in [(3, "three"), (4, "four")] {
        let kit = configured(&server, &env, &store, every_default());
        let agent = resumed(&kit, &store, &server, PLAIN).await;
        assert!(
            !rebuilt(&agent),
            "resume before u{n}: {:?}",
            agent.cache_rebuild()
        );
        assert!(!agent.binding_usable_now(), "resume before u{n}");
        let mut session = Session::start(agent);
        session.turn(&format!("u{n}"), text).await;
        session.end().await;
    }
    assert_eq!(thinking_cuts(&store), cuts);
    kept_prefix(&server, 3, 4);
}

#[tokio::test]
async fn a_gone_tool_through_a_rebuild_stays_declared_with_its_definition_and_refused() {
    let env = Env::new("rebuilt-gone");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        reply("msg_02", 520, vec![diff_call("toolu_01")], "tool_use"),
        says("msg_03", 640, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    one_session(
        &configured(&server, &env, &store, without("view_file")),
        PLAIN,
    )
    .await;
    let logged = body(&server, 0);
    let mut kit = plugin_gone(&server, &env, &store, &["show-diff"]);
    kit.start_layers = configured(&server, &env, &store, every_default()).start_layers;
    kit.start_layers.flags.push(removed_flag("show-diff"));
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(
        changed(&second),
        [(
            vec!["view_file".to_owned()],
            vec!["show_code".to_owned(), "show_diff".to_owned()],
            ToolChangeVia::Rebuild,
            Some(CacheEffect::Rebuilt {
                reason: wisp_protocol::RebuildReason::ToolsChange,
            })
        )]
    );
    let rebuilt_body = body(&server, 1);
    let declared = |request: &Value| {
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "show_diff")
            .cloned()
    };
    assert_eq!(declared(&rebuilt_body), declared(&logged));
    assert!(declared(&rebuilt_body).is_some());
    let refused = result_text(&body(&server, 2), "toolu_01");
    assert!(refused.contains(REFUSED), "{refused}");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn the_base_is_not_due_after_a_rebuild() {
    let env = Env::new("rebuilt-base");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let agent = one_session(&without_show_diff(&server, &env, &store), PLAIN).await;
    let base = base_of(&agent);
    let store = rebased(&store, &base, "Older base.", true);
    let kit = without_show_diff(&server, &env, &store);
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.tools(1, every_default()).await;
    let second = session.turn("u2", "two").await;
    change(&mut session, 2, model(CAPABLE)).await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(changed(&second)[0].2, ToolChangeVia::Rebuild);
    assert!(system_text(&body(&server, 1)).starts_with(&base));
    assert_eq!(base_blocks(&body(&server, 2)), 0);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_rewind_before_a_rebuild_lowers_its_point_live_and_in_the_fold() {
    let env = Env::new("rebuilt-rewound");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    let folded = || {
        LoggedSession::read(&records(&store))
            .unwrap()
            .copy
            .tools
            .history_len
    };
    assert_eq!(folded(), 2);
    session
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 1 },
            request: RequestId(2),
        })
        .await;
    let events = session
        .until(|e| matches!(e, AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. }))
        .await;
    assert!(
        matches!(events.last(), Some(AgentEvent::Rewound { .. })),
        "{events:#?}"
    );
    let agent = session.end().await;
    assert_eq!(agent.whole_tools().history_len, 0);
    assert_eq!(folded(), 0);
}

#[tokio::test]
async fn a_rebuild_after_a_resume_whose_context_differed_drops_the_copied_descriptions() {
    let env = Env::new("rebuilt-context");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&without_show_diff(&server, &env, &store), PLAIN).await;
    let kit = configured(
        &server,
        &env,
        &store,
        every_default()
            .into_iter()
            .filter(|n| n != "bash")
            .collect(),
    );
    let shell = |agent: &Agent| {
        agent
            .tool_context_now()
            .is_some_and(|c| c.capabilities.iter().any(|c| c == "shell"))
    };
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    assert!(agent.copy_described() > 0);
    assert!(shell(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    change(&mut session, 1, model(CAPABLE)).await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(changed(&second)[0].2, ToolChangeVia::Rebuild);
    assert_eq!(agent.copy_described(), 0);
    assert!(agent.tool_context_now().is_some());
    assert!(!shell(&agent));
    let message = tool_message(&body(&server, 2)).unwrap();
    assert!(by_value_in(&message).is_empty(), "{message:#?}");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_continuing_retry_before_the_first_opening_keeps_the_logged_prefix_where_a_rebuild_is_due()
 {
    let env = Env::new("rebuilt-retry-continues");
    let path = small_file(&env);
    let server = Server::start(failing_retry_session(vec![call(
        "toolu_01",
        "view_file",
        json!({"path": path}),
    )]))
    .await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.send(ClientCommand::send("u1", "one")).await;
    session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    session.end().await;
    let mut kit = configured(&server, &env, &store, every_default());
    kit.retry_budget = Some(0.0);
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    retried(&mut session, 1).await;
    session.end().await;
    assert_eq!(body(&server, 2)["tools"], body(&server, 0)["tools"]);
    assert_eq!(body(&server, 2)["system"], body(&server, 0)["system"]);
    assert!(rebuild_copies(&store).is_empty());
}

#[tokio::test]
async fn an_on_tool_over_an_empty_array_rebuilds_once_at_the_first_resumed_opening() {
    let env = Env::new("rebuilt-empty-array");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(
        &configured(&server, &env, &store, vec!["none".to_owned()]),
        CAPABLE,
    )
    .await;
    assert!(tool_names_of(&body(&server, 0)).is_empty());
    let kit = configured(&server, &env, &store, every_default());
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent), "{:?}", agent.cache_rebuild());
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(changed(&second).len(), 1, "{:?}", changed(&second));
    assert_eq!(changed(&second)[0].2, ToolChangeVia::Rebuild);
    assert!(tool_names_of(&body(&server, 1)).contains(&"show_diff".to_owned()));
    assert!(betas(&server, 1).contains(&INLINE_TOOLS_BETA.to_owned()));
    assert!(betas(&server, 2).contains(&INLINE_TOOLS_BETA.to_owned()));
    assert_eq!(tool_message(&body(&server, 1)), None);
    rebuilds(&store, &agent).await;
}

fn tool_names_of(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter(|t| {
                    t["type"]
                        .as_str()
                        .is_none_or(|k| !k.starts_with("web_search"))
                })
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_base_block_from_before_a_rebuild_at_a_resumes_first_opening_does_not_decide_the_base_then_or_at_the_next_resume()
 {
    let env = Env::new("rebuilt-old-block");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    replies.push(says("msg_05", 760, "Five."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let agent = one_session(&without_show_diff(&server, &env, &store), CAPABLE).await;
    let base = base_of(&agent);
    let store = rebased(&store, &base, "Older base.", true);
    let agent = resumed(
        &without_show_diff(&server, &env, &store),
        &store,
        &server,
        CAPABLE,
    )
    .await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.end().await;
    assert_eq!(base_blocks(&body(&server, 1)), 1);
    let store = edited(&store, |value| {
        if value["type"] == "turn_started"
            && let Some(blocks) = value.get_mut("tool_message").and_then(|m| m.as_array_mut())
        {
            for block in blocks {
                if block["text"]
                    .as_str()
                    .is_some_and(wisp_tool_text::switches::is_base_block)
                {
                    block["text"] = json!(base_block("An even older base."));
                }
            }
        }
    });
    let agent = resumed(
        &without_show_diff(&server, &env, &store),
        &store,
        &server,
        CAPABLE,
    )
    .await;
    let mut session = Session::start(agent);
    change(&mut session, 1, model(PLAIN)).await;
    session.tools(2, every_default()).await;
    let third = session.turn("u3", "three").await;
    change(&mut session, 3, model(CAPABLE)).await;
    session.turn("u4", "four").await;
    let agent = session.end().await;
    assert_eq!(changed(&third)[0].2, ToolChangeVia::Rebuild);
    assert_eq!(
        base_blocks(&body(&server, 3)),
        1,
        "only the older block, never a new one"
    );
    rebuilds(&store, &agent).await;
    let agent = resumed(
        &without_show_diff(&server, &env, &store),
        &store,
        &server,
        CAPABLE,
    )
    .await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u5", "five").await;
    session.end().await;
    let fifth = body(&server, 4);
    assert_eq!(fifth["system"], body(&server, 3)["system"]);
    assert_eq!(
        base_blocks(&fifth),
        1,
        "the old block decides nothing after the point"
    );
}

mod deferred;

const SEARCH: &str = crate::search::WEB_SEARCH;

fn searches(request: &Value) -> bool {
    request["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|t| t["name"] == SEARCH))
}

fn search_rebuilt() -> Option<CacheEffect> {
    Some(CacheEffect::Rebuilt {
        reason: wisp_protocol::RebuildReason::ToolsChange,
    })
}

fn form(agent: &Agent) -> crate::search::SearchForm {
    agent.search_form(agent.endpoint_name(), CAPABLE)
}

fn tools_records(store: &MemoryStore) -> (usize, usize) {
    records(store)
        .iter()
        .fold((0, 0), |(with, without), r| match &r.event {
            LogEvent::ToolsChanged { copy, .. } if **copy == Default::default() => {
                (with, without + 1)
            }
            LogEvent::ToolsChanged { .. } => (with + 1, without),
            _ => (with, without),
        })
}

fn search_plugin_off(server: &Server, env: &Env, store: &MemoryStore) -> Kit {
    let mut kit = switching(server, env, store);
    kit.start_layers.flags = vec![crate::plugin::testing::plugin_flag(
        "web-search",
        ENABLED_KEY,
        false,
    )];
    kit.tools = Arc::new(crate::plugin::Tools::new(
        registry(Tools::Default),
        registry(Tools::Default).on_tools_with(
            &every_default().into_iter().collect(),
            &std::collections::BTreeSet::from(["web-search".to_owned()]),
        ),
    ));
    kit
}

#[tokio::test]
async fn web_search_turned_on_appears_with_one_mark_and_one_record_fresh_or_resumed_without_it() {
    for (resume, by_switch) in [(false, false), (true, false), (false, true), (true, true)] {
        let env = Env::new("resumed-search-on");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        let mut kit = if by_switch {
            search_plugin_off(&server, &env, &store)
        } else {
            configured(&server, &env, &store, without(SEARCH))
        };
        let agent = if resume {
            one_session(&kit, CAPABLE).await;
            let agent = resumed(&kit, &store, &server, CAPABLE).await;
            assert!(!rebuilt(&agent));
            agent
        } else {
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent
        };
        let mut session = Session::start(agent);
        if !resume {
            session.turn("u1", "one").await;
        }
        let answer = if by_switch {
            session.switched(1, "web-search", ENABLED_KEY, true).await
        } else {
            session.tools(1, every_default()).await
        };
        let opened = session.turn("u2", "two").await;
        let agent = session.end().await;
        assert_eq!(answered(&answer).1, Some(CacheEffect::Kept), "{resume}");
        assert_eq!(
            changed(&opened),
            [(
                vec![SEARCH.to_owned()],
                Vec::new(),
                ToolChangeVia::Inline,
                Some(CacheEffect::Kept)
            )],
            "{resume}"
        );
        assert!(!searches(&body(&server, 0)), "{resume}");
        assert!(!searches(&body(&server, 1)), "{resume}");
        kept_prefix(&server, 0, 1);
        assert_eq!(
            by_value_in(&tool_message(&body(&server, 1)).unwrap()),
            ["tool_addition web_search by value"],
            "{resume}"
        );
        assert!(form(&agent).is_native(), "{resume}");
        assert_eq!(tools_records(&store), (0, 1), "{resume}");
        rebuilds(&store, &agent).await;
        kit.start_layers.flags.clear();
        let agent = resumed(&kit, &store, &server, CAPABLE).await;
        assert!(!rebuilt(&agent), "{resume}, by_switch={by_switch}");
        let mut session = Session::start(agent);
        let reopened = session.turn("u3", "three").await;
        let agent = session.end().await;
        prefix::assert_prefix_stable(
            &[body(&server, 0), body(&server, 1), body(&server, 2)],
            None,
        );
        assert!(
            changed(&reopened).is_empty(),
            "{resume}, by_switch={by_switch}: {:?}",
            changed(&reopened)
        );
        assert_eq!(
            tools_records(&store),
            (0, 1),
            "{resume}, by_switch={by_switch}"
        );
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_refusal_after_web_search_was_turned_on_takes_the_refusal_path() {
    let env = Env::new("resumed-search-on-refused");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        search_refused(),
        says("msg_02", 520, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = configured(&server, &env, &store, without(SEARCH));
    let mut built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    built.agent.toolbox.drop_plugin("web-search");
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    let agent = session.end().await;
    assert!(searches(&body(&server, 1)));
    assert!(!searches(&body(&server, 2)));
    assert!(log_events(&store).iter().any(|e| matches!(
        *e,
        LogEvent::NativeWebSearch {
            state: wisp_protocol::log::FeatureState::FallenBack,
            ..
        }
    )));
    assert_eq!(
        form(&agent),
        crate::search::SearchForm::None(crate::search::NoSearch::Refused)
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_log_with_web_search_resumed_by_a_run_without_it_adopts_then_removes_once() {
    for model in [CAPABLE, PLAIN] {
        let env = Env::new("resumed-search-seeded");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        one_session(&switching(&server, &env, &store), model).await;
        let kit = configured(&server, &env, &store, without(SEARCH));
        let agent = resumed(&kit, &store, &server, model).await;
        assert!(!rebuilt(&agent), "{model}: {:?}", agent.cache_rebuild());
        let mut session = Session::start(agent);
        let opened = session.turn("u2", "two").await;
        let next = session.turn("u3", "three").await;
        let agent = session.end().await;
        let (via, cache) = if model == CAPABLE {
            (ToolChangeVia::Inline, Some(CacheEffect::Kept))
        } else {
            (ToolChangeVia::Rebuild, search_rebuilt())
        };
        assert_eq!(
            changed(&opened),
            [(Vec::new(), vec![SEARCH.to_owned()], via, cache)],
            "{model}"
        );
        assert!(changed(&next).is_empty(), "{model}");
        assert!(searches(&body(&server, 0)), "{model}");
        if model == CAPABLE {
            kept_prefix(&server, 0, 1);
            kept_prefix(&server, 1, 2);
            assert_eq!(
                removal_of(&tool_message(&body(&server, 1)).unwrap()),
                ["tool_removal web_search"]
            );
        } else {
            assert!(!searches(&body(&server, 1)));
            assert_eq!(body(&server, 2)["tools"], body(&server, 1)["tools"]);
        }
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_log_without_web_search_resumed_with_search_on_now_switches_it_on_once() {
    for model_gained in [false, true] {
        let env = Env::new("resumed-search-pickup");
        let replies = if model_gained {
            vec![
                says("msg_01", 40_000, "One."),
                says("msg_02", 40_100, "Two."),
                reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
                says("msg_04", 760, "Four."),
            ]
        } else {
            vec![
                says("msg_01", 400, "One."),
                says("msg_02", 520, "Two."),
                says("msg_03", 640, "Three."),
                says("msg_04", 760, "Four."),
            ]
        };
        let server = Server::start(replies).await;
        let store = MemoryStore::default();
        let mut old_kit = if model_gained {
            switching(&server, &env, &store)
        } else {
            configured(&server, &env, &store, without(SEARCH))
        };
        if model_gained {
            old_kit.models = models(
                "[models.\"claude-opus-5-5\"]\nweb_search = []\nnative_compaction = false\n",
            );
        }
        one_session(&old_kit, CAPABLE).await;
        assert!(!searches(&body(&server, 0)));

        let mut kit = switching(&server, &env, &store);
        if model_gained {
            kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
        }
        let agent = resumed(&kit, &store, &server, CAPABLE).await;
        assert!(!rebuilt(&agent));
        assert!(form(&agent).is_native());
        assert_eq!(catalog_marks(&agent, SEARCH), Some((true, false)));
        assert!(
            agent
                .snapshot(RequestId(0), None)
                .plugins
                .iter()
                .flat_map(|p| &p.tools)
                .find(|t| t.is_search())
                .and_then(|t| t.unavailable.as_deref())
                .is_none()
        );

        let mut session = Session::start(agent);
        let second = session.turn("u2", "two").await;
        assert_eq!(
            changed(&second),
            [(
                vec![SEARCH.to_owned()],
                Vec::new(),
                ToolChangeVia::Inline,
                Some(CacheEffect::Kept)
            )],
            "{model_gained}"
        );
        kept_prefix(&server, 0, 1);
        assert_eq!(
            by_value_in(&tool_message(&body(&server, 1)).unwrap()),
            ["tool_addition web_search by value"]
        );
        if model_gained {
            session.compact(7).await;
            let after_compact = session.turn("u3", "three").await;
            let agent = session.end().await;
            assert!(changed(&after_compact).is_empty());
            assert_eq!(body(&server, 2)["tools"], body(&server, 0)["tools"]);
            assert_eq!(body(&server, 3)["tools"], body(&server, 0)["tools"]);
            assert_eq!(
                by_value_in(&tool_message(&body(&server, 3)).unwrap()),
                ["tool_addition web_search by value"]
            );
            rebuilds(&store, &agent).await;
        } else {
            let third = session.turn("u3", "three").await;
            let agent = session.end().await;
            assert!(changed(&third).is_empty());
            assert_eq!(tools_records(&store), (0, 1));
            prefix::assert_prefix_stable(
                &[body(&server, 0), body(&server, 1), body(&server, 2)],
                None,
            );
            rebuilds(&store, &agent).await;

            let again = resumed(&kit, &store, &server, CAPABLE).await;
            assert!(!rebuilt(&again));
            let mut session = Session::start(again);
            let fourth = session.turn("u4", "four").await;
            let agent = session.end().await;
            assert!(changed(&fourth).is_empty());
            assert_eq!(tools_records(&store), (0, 1));
            prefix::assert_prefix_stable(
                &[
                    body(&server, 0),
                    body(&server, 1),
                    body(&server, 2),
                    body(&server, 3),
                ],
                None,
            );
            rebuilds(&store, &agent).await;
        }
    }

    let env = Env::new("resumed-search-pickup-cancelled");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&configured(&server, &env, &store, without(SEARCH)), CAPABLE).await;
    let kit = switching(&server, &env, &store);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(
        agent
            .snapshot(RequestId(0), None)
            .plugins
            .iter()
            .flat_map(|p| &p.tools)
            .find(|t| t.is_search())
            .and_then(|t| t.unavailable.as_deref())
            .is_none()
    );
    let mut session = Session::start(agent);
    session.switched(1, "web-search", ENABLED_KEY, false).await;
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert!(changed(&second).is_empty(), "{:?}", changed(&second));
    assert_eq!(tool_message(&body(&server, 1)), None);
    kept_prefix(&server, 0, 1);
    assert!(!form(&agent).is_native());
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_chat_with_search_switched_off_in_chat_stays_off_on_resume_and_can_turn_back_on() {
    for (had_before, by_switch) in [(true, false), (true, true), (false, false), (false, true)] {
        let env = Env::new("resumed-search-switched-off");
        let mut replies = three();
        replies.push(says("msg_04", 760, "Four."));
        let server = Server::start(replies).await;
        let store = MemoryStore::default();
        let mut first_kit = switching(&server, &env, &store);
        if !had_before {
            first_kit.models = models("[models.\"claude-opus-5-5\"]\nweb_search = []\n");
        }
        let agent = first_kit
            .build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent;
        let mut session = Session::start(agent);
        session.turn("u1", "one").await;
        if by_switch {
            session.switched(1, "web-search", ENABLED_KEY, false).await;
        } else {
            session.tools(1, without(SEARCH)).await;
        }
        session.turn("u2", "two").await;
        session.end().await;

        let kit = switching(&server, &env, &store);
        let agent = resumed(&kit, &store, &server, CAPABLE).await;
        assert!(!rebuilt(&agent), "{had_before} {by_switch}");
        assert!(!form(&agent).is_native(), "{had_before} {by_switch}");
        let mut session = Session::start(agent);
        let third = session.turn("u3", "three").await;
        assert!(
            changed(&third).is_empty(),
            "{had_before} {by_switch}: {:?}",
            changed(&third)
        );
        kept_prefix(&server, 1, 2);

        if by_switch {
            session.switched(2, "web-search", ENABLED_KEY, true).await;
        } else {
            session.tools(2, every_default()).await;
        }
        let fourth = session.turn("u4", "four").await;
        let agent = session.end().await;
        assert_eq!(
            changed(&fourth),
            [(
                vec![SEARCH.to_owned()],
                Vec::new(),
                ToolChangeVia::Inline,
                Some(CacheEffect::Kept)
            )],
            "{had_before} {by_switch}"
        );
        assert!(form(&agent).is_native(), "{had_before} {by_switch}");
        kept_prefix(&server, 2, 3);
        let expected = if had_before {
            "tool_addition web_search"
        } else {
            "tool_addition web_search by value"
        };
        assert_eq!(
            additions_in(&tool_message(&body(&server, 3)).unwrap()),
            [expected],
            "{had_before} {by_switch}"
        );
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_failed_adoption_takes_web_search_from_this_run_and_rebuilds_only_at_resume() {
    let env = Env::new("resumed-search-failed");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let agent = one_session(&switching(&server, &env, &store), CAPABLE).await;
    let base = base_of(&agent);
    let store = edited(&store, |value| {
        if value["type"] == "session_started" {
            let prompt = value["system_prompt"]
                .as_str()
                .unwrap()
                .replacen(&base, "[REDACTED]", 1);
            value["system_prompt"] = json!(prompt);
        }
    });
    let kit = configured(&server, &env, &store, without(SEARCH));
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(rebuilt(&agent));
    let mut session = Session::start(agent);
    let opened = session.turn("u2", "two").await;
    session.end().await;
    assert!(
        changed(&opened)
            .iter()
            .all(|(_, _, _, cache)| *cache != search_rebuilt()),
        "{:?}",
        changed(&opened)
    );
    assert!(!searches(&body(&server, 1)));
    assert_eq!(
        marked(&store)
            .iter()
            .filter(|m| *m == "\"tools_change\"")
            .count(),
        0
    );
}

#[tokio::test]
async fn a_log_with_web_search_on_an_endpoint_now_turned_off_rebuilds_off_and_stays_off() {
    let env = Env::new("resumed-search-endpoint-off");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), CAPABLE).await;
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[endpoints.env]\nnative_web_search = false\n");
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.tools(1, without(SEARCH)).await;
    let third = session.turn("u3", "three").await;
    let answer = session.tools(2, every_default()).await;
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(searches(&body(&server, 1)));
    assert_eq!(changed(&third).len(), 1, "{:?}", changed(&third));
    assert!(!searches(&body(&server, 2)));
    assert_eq!(answered(&answer), (When::NextTurn, Some(CacheEffect::Kept)));
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    assert!(!searches(&body(&server, 3)));
    assert_eq!(body(&server, 3)["tools"], body(&server, 2)["tools"]);
    assert_eq!(marked(&store), ["\"tools_change\""]);
    assert_eq!(tools_records(&store), (1, 0));
    assert_eq!(
        form(&agent),
        crate::search::SearchForm::None(crate::search::NoSearch::TurnedOff)
    );
    rebuilds(&store, &agent).await;
}

pub(super) fn child_with(cwd: std::path::PathBuf, tools: &[&str]) -> kit::ChildSpec {
    kit::ChildSpec {
        path: AgentPath::new("/review"),
        model: CAPABLE.into(),
        effort: Effort::High,
        cache_ttl: CacheTtl::OneHour,
        message_time: MessageTime::Off,
        context_note: false,
        instructions: kit::InstructionsChoice {
            source: InstructionsSource::Auto,
            warn_tokens: 8_000,
        },
        cwd,
        tools: tools.iter().map(|t| (*t).to_owned()).collect(),
        parent: wisp_protocol::ParentLink {
            session_id: "s-root".into(),
            agent: AgentPath::new("/review"),
            depth: 1,
            config: wisp_protocol::SpawnChoices::default(),
            todos: Vec::new(),
        },
        endpoint: "env".into(),
        preset_instructions: None,
        plugins: crate::plugin::PluginSwitches::default(),
    }
}

#[tokio::test]
async fn a_resumed_child_removes_or_picks_up_web_search_inline_once() {
    for had_before in [true, false] {
        let env = Env::new("resumed-search-child");
        let mut replies = three();
        replies.push(says("msg_04", 760, "Four."));
        let server = Server::start(replies).await;
        let store = MemoryStore::default();
        let make_kit = |no_search: bool| {
            let mut kit = switching(&server, &env, &store);
            if no_search {
                kit.models = models("[models.\"claude-opus-5-5\"]\nweb_search = []\n");
            }
            let provider = server.provider();
            let ready = ready(&server);
            kit.connect = Arc::new(move |_: &str| {
                Ok(kit::Connected {
                    provider: provider.clone(),
                    ready: ready.clone(),
                    unavailable: unavailable(),
                    active: "env".into(),
                    env_account: None,
                })
            });
            kit
        };
        let child = make_kit(!had_before)
            .build_child(child_with(env.work(), &["bash", SEARCH]))
            .await
            .unwrap();
        assert_pinned_environment(&child);
        let mut session = Session::start(child);
        session.turn("u1", "one").await;
        session.end().await;
        assert_eq!(searches(&body(&server, 0)), had_before, "{had_before}");

        let (log, recovered, logged, handed) = reopen(&store, &server);
        let resume = kit::Resume {
            log,
            recovered: &recovered,
            truncated_bytes: 0,
            logged: logged.as_ref(),
            rebuilt: handed,
        };
        let next_tools: &[&str] = if had_before {
            &["bash"]
        } else {
            &["bash", SEARCH]
        };
        let (child, _) = make_kit(false)
            .resume_child(child_with(env.work(), next_tools), resume)
            .await
            .unwrap();
        assert_pinned_environment(&child);
        assert!(
            !rebuilt(&child),
            "{had_before}: {:?}",
            child.cache_rebuild()
        );
        let mut session = Session::start(child);
        let opened = session.turn("u2", "two").await;
        let third = session.turn("u3", "three").await;
        session.end().await;
        let (added, removed) = if had_before {
            (Vec::new(), vec![SEARCH.to_owned()])
        } else {
            (vec![SEARCH.to_owned()], Vec::new())
        };
        assert_eq!(
            changed(&opened),
            [(
                added,
                removed,
                ToolChangeVia::Inline,
                Some(CacheEffect::Kept)
            )],
            "{had_before}"
        );
        assert!(changed(&third).is_empty(), "{had_before}");
        prefix::assert_prefix_stable(
            &[body(&server, 0), body(&server, 1), body(&server, 2)],
            None,
        );
        if had_before {
            let (log, recovered, logged, handed) = reopen(&store, &server);
            let resume = kit::Resume {
                log,
                recovered: &recovered,
                truncated_bytes: 0,
                logged: logged.as_ref(),
                rebuilt: handed,
            };
            let (child, _) = make_kit(false)
                .resume_child(child_with(env.work(), &["bash", SEARCH]), resume)
                .await
                .unwrap();
            assert!(!rebuilt(&child));
            let mut session = Session::start(child);
            let fourth = session.turn("u4", "four").await;
            session.end().await;
            assert_eq!(
                changed(&fourth),
                [(
                    vec![SEARCH.to_owned()],
                    Vec::new(),
                    ToolChangeVia::Inline,
                    Some(CacheEffect::Kept)
                )]
            );
            assert_eq!(
                additions_in(&tool_message(&body(&server, 3)).unwrap()),
                ["tool_addition web_search"]
            );
            prefix::assert_prefix_stable(
                &[
                    body(&server, 0),
                    body(&server, 1),
                    body(&server, 2),
                    body(&server, 3),
                ],
                None,
            );
        }
    }
}

#[tokio::test]
async fn a_chat_resumed_without_web_search_picks_it_back_up_when_resumed_with_search_on() {
    let env = Env::new("resumed-search-re-pickup");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    one_session(&switching(&server, &env, &store), CAPABLE).await;
    let agent = resumed(
        &configured(&server, &env, &store, without(SEARCH)),
        &store,
        &server,
        CAPABLE,
    )
    .await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    session.end().await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec![SEARCH.to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );

    let kit = switching(&server, &env, &store);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    assert!(form(&agent).is_native());
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert_eq!(
        changed(&third),
        [(
            vec![SEARCH.to_owned()],
            Vec::new(),
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    assert_eq!(
        additions_in(&tool_message(&body(&server, 2)).unwrap()),
        ["tool_addition web_search"]
    );
    prefix::assert_prefix_stable(
        &[
            body(&server, 0),
            body(&server, 1),
            body(&server, 2),
            body(&server, 3),
        ],
        None,
    );
    rebuilds(&store, &agent).await;
}

fn with_extra_endpoint(
    mut kit: Kit,
    primary: Arc<dyn Provider>,
    name: &str,
    extra: Arc<dyn Provider>,
    active: &str,
) -> Kit {
    let use_extra = active == name;
    let active_name = active.to_owned();
    let active_provider = if use_extra {
        extra.clone()
    } else {
        primary.clone()
    };
    let both = vec![
        Connection {
            name: "env".into(),
            provider: primary,
            context: log_context(),
            priced: true,
        },
        Connection {
            name: name.into(),
            provider: extra,
            context: log_context(),
            priced: true,
        },
    ];
    kit.connect = Arc::new(move |_: &str| {
        Ok(kit::Connected {
            provider: active_provider.clone(),
            ready: both.clone(),
            unavailable: unavailable(),
            active: active_name.clone(),
            env_account: None,
        })
    });
    kit
}

#[tokio::test]
async fn an_inline_search_pickup_stays_single_across_model_openai_and_gateway_switches() {
    let env = Env::new("resumed-search-switches");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    one_session(&configured(&server, &env, &store, without(SEARCH)), CAPABLE).await;
    let kit = switching(&server, &env, &store);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    change(&mut session, 1, model("claude-opus-5")).await;
    let third = session.turn("u3", "three").await;
    session.end().await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    assert_eq!(super::rebuild::declared(&body(&server, 2), SEARCH), 1);
    assert_eq!(
        super::rebuild::by_value_additions(&body(&server, 2), SEARCH),
        0
    );
    let again = resumed(&kit, &store, &server, "claude-opus-5").await;
    assert!(!rebuilt(&again));
    let mut session = Session::start(again);
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    assert_eq!(super::rebuild::declared(&body(&server, 3), SEARCH), 1);
    assert_eq!(
        super::rebuild::by_value_additions(&body(&server, 3), SEARCH),
        0
    );
    rebuilds(&store, &agent).await;

    let env = Env::new("resumed-search-to-openai");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        super::openai::gpt(&[crate::provider::openai::tests::message()]),
    ])
    .await;
    let store = MemoryStore::default();
    one_session(&configured(&server, &env, &store, without(SEARCH)), CAPABLE).await;
    let kit = with_extra_endpoint(
        switching(&server, &env, &store),
        server.provider(),
        "oa",
        super::openai::subscribed(&server),
        "env",
    );
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    change(
        &mut session,
        1,
        vec![
            SettingChange::named("endpoint", SettingValue::Text("oa".into())).unwrap(),
            SettingChange::named("model", SettingValue::Text("gpt-6.1-sol".into())).unwrap(),
        ],
    )
    .await;
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    let oa_req = body(&server, 2);
    assert!(
        oa_req["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["type"] == "web_search")
    );
    assert!(
        !oa_req["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "additional_tools")
            .flat_map(|item| item["tools"].as_array().into_iter().flatten())
            .any(|tool| tool["name"] == SEARCH),
        "{oa_req:#}"
    );
    rebuilds(&store, &agent).await;

    let env = Env::new("resumed-search-to-gw-off");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    one_session(&configured(&server, &env, &store, without(SEARCH)), CAPABLE).await;
    let make_gw_kit = |active: &str| {
        let mut kit = with_extra_endpoint(
            switching(&server, &env, &store),
            server.provider(),
            "gw",
            server.provider(),
            active,
        );
        kit.models = models("[endpoints.gw]\nnative_web_search = false\n");
        kit
    };
    let kit = make_gw_kit("env");
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    change(
        &mut session,
        1,
        vec![SettingChange::named("endpoint", SettingValue::Text("gw".into())).unwrap()],
    )
    .await;
    let third = session.turn("u3", "three").await;
    session.end().await;
    assert_eq!(
        changed(&third),
        [(
            Vec::new(),
            vec![SEARCH.to_owned()],
            ToolChangeVia::Rebuild,
            None
        )]
    );
    assert_eq!(super::rebuild::declared(&body(&server, 2), SEARCH), 0);

    let again = resumed(&make_gw_kit("gw"), &store, &server, CAPABLE).await;
    assert_eq!(
        again.search_form("gw", CAPABLE),
        crate::search::SearchForm::None(crate::search::NoSearch::TurnedOff)
    );
    let mut session = Session::start(again);
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    assert_eq!(super::rebuild::declared(&body(&server, 3), SEARCH), 0);
    rebuilds(&store, &agent).await;
}

fn show_diff_declared(request: &Value) -> usize {
    let in_array = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["name"] == "show_diff")
        .count();
    let by_value = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .flat_map(by_value_in)
        .filter(|k| k == "tool_addition show_diff by value")
        .count();
    in_array + by_value
}

fn gateway_without_show_diff(server: &Server, env: &Env, store: &MemoryStore) -> Kit {
    let mut kit = on_a_gateway(server, env, store);
    kit.start_layers = without_show_diff(server, env, store).start_layers;
    kit.tools = without_show_diff(server, env, store).tools;
    kit
}

#[tokio::test]
async fn a_resume_whose_adoption_failed_voids_the_additions_before_its_whole_rebuild() {
    let env = Env::new("resumed-void-whole");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = gateway_without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(show_diff_declared(&body(&server, 1)), 1);
    let base = base_of(&agent);
    let store = edited(&store, |value| {
        if value["type"] == "session_started" {
            let prompt = value["system_prompt"]
                .as_str()
                .unwrap()
                .replacen(&base, "[REDACTED]", 1);
            value["system_prompt"] = json!(prompt);
        }
    });
    let mut kit = on_a_gateway(&server, &env, &store);
    kit.tools = switching(&server, &env, &store).tools;
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    session.end().await;
    let resumed_request = body(&server, 2);
    assert_eq!(
        show_diff_declared(&resumed_request),
        1,
        "{resumed_request:#}"
    );
}

#[tokio::test]
async fn a_by_value_add_an_off_a_re_add_and_a_web_search_rebuild_end_with_the_tool_offered() {
    let env = Env::new("resumed-void-diff");
    let path = env.work().join("small.txt");
    std::fs::create_dir_all(env.work()).unwrap();
    std::fs::write(&path, "x\n").unwrap();
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
        reply("msg_05", 880, vec![diff_call("toolu_01")], "tool_use"),
        says("msg_06", 1_000, "Five."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = gateway_without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.tools(2, without("show_diff")).await;
    session.turn("u3", "three").await;
    session.tools(3, every_default()).await;
    session.turn("u4", "four").await;
    session.tools(4, without(SEARCH)).await;
    let flipped = session.turn("u5", "five").await;
    let agent = session.end().await;
    assert!(
        changed(&flipped)
            .iter()
            .any(|(_, removed, via, _)| removed.contains(&SEARCH.to_owned())
                && *via == ToolChangeVia::Rebuild),
        "{:?}",
        changed(&flipped)
    );
    let after = body(&server, 5);
    assert!(
        !result_text(&after, "toolu_01").contains(REFUSED),
        "{after:#}"
    );
    assert!(show_diff_declared(&after) >= 1, "{after:#}");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_logged_server_search_moves_to_the_function_with_one_opening_rebuild() {
    let env = Env::new("resumed-search-runner");
    let mut served_elsewhere = says("msg_04", 40_300, "Four.");
    served_elsewhere.response = served_elsewhere.response.replacen(
        &format!("\"model\":\"{DEFAULT_MODEL}\""),
        &format!("\"model\":\"{PLAIN}\""),
        1,
    );
    let server = Server::start(vec![
        reply(
            "msg_old",
            40_000,
            vec![
                Part::Think,
                Part::Search("s1", json!({"query":"wisp"}), json!([])),
                Part::Think,
                Part::Text("Found."),
            ],
            "end_turn",
        ),
        says("msg_02", 40_100, "Two."),
        super::super::super::keep_warm::refreshes_read("msg_r1", 40_100),
        says("msg_03", 40_200, "Three."),
        served_elsewhere,
        reply("msg_summary", 40_400, vec![Part::Text(SUMMARY)], "end_turn"),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let mut agent = kit
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    agent.toolbox.drop_plugin("web-search");
    agent.toolbox.set_web_search(true);
    agent.settings.carried(
        (
            "env".into(),
            CAPABLE.into(),
            crate::provider::Feature::BindingControls,
        ),
        crate::agent::settings::PairState::FallenBack,
    );
    let mut session = Session::start(agent);
    session.turn("u1", "one").await;
    let old = session.end().await;
    let committed = old.history().to_vec();
    let records = records(&store);
    assert!(
        body(&server, 0)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "web_search" && tool["type"] == "web_search_20250305")
    );
    let mut agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(rebuilt(&agent));
    agent.settings.carried(
        (
            "env".into(),
            CAPABLE.into(),
            crate::provider::Feature::BindingControls,
        ),
        crate::agent::settings::PairState::FallenBack,
    );
    assert!(!agent.binding_usable_now());
    assert_eq!(thinking_cuts(&store), 0);
    let mut session = Session::start(agent.with_keep_warm(1));
    session.turn("u2", "two").await;
    let marks = |store: &MemoryStore| {
        log_events(store)
            .iter()
            .filter(|event| matches!(event, LogEvent::RebuildMarked { .. }))
            .count()
    };
    let first_cuts = thinking_cuts(&store);
    let first_marks = marks(&store);
    assert_eq!((first_cuts, first_marks), (1, 0));
    session.refreshed().await;
    let third = session.turn("u3", "three").await;
    let fourth = session.turn("u4", "four").await;
    assert!(changed(&third).is_empty() && changed(&fourth).is_empty());
    assert_eq!(
        (thinking_cuts(&store), marks(&store)),
        (first_cuts, first_marks)
    );
    kept_prefix(&server, 1, 3);
    kept_prefix(&server, 3, 4);
    session
        .send(ClientCommand::ContextReport {
            detail: wisp_protocol::context::ReportDetail::All,
            request: RequestId(7),
        })
        .await;
    session.until(|event| matches!(event, AgentEvent::ContextReport(report) if report.answers == RequestId(7) && report.basis.settled())).await;
    session.compact(8).await;
    let agent = session.end().await;
    assert!(agent.history().starts_with(&committed));
    assert_eq!(test_guard::records(&store)[..records.len()], records);
    let rendered =
        |body: &Value| serde_json::to_string(&unmarked(&body["messages"])[1]["content"]).unwrap();
    let expected = rendered(&body(&server, 1));
    assert!(!expected.contains("server_tool_use") && !expected.contains("web_search_tool_result"));
    for index in 2..6 {
        assert_eq!(rendered(&body(&server, index)), expected, "request {index}");
    }
    let counts: Vec<Value> = server
        .seen()
        .counts
        .iter()
        .map(|body| serde_json::from_str::<Value>(body).unwrap())
        .filter(|body| {
            body["messages"]
                .as_array()
                .is_some_and(|messages| messages.len() > 3)
        })
        .collect();
    assert!(!counts.is_empty());
    for count in counts {
        assert_eq!(rendered(&count), expected);
    }
    assert_eq!(body(&server, 2)["max_tokens"], 1);
    assert_eq!(server.seen().bodies.len(), 6);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn manage_resumed_without_keys_keeps_its_prefix_and_appends_the_new_schema_once() {
    fn old_manage(value: &mut Value) -> usize {
        let mut changed = 0;
        if value["name"] == "manage"
            && let Some(properties) = value
                .get_mut("input_schema")
                .and_then(|schema| schema.get_mut("properties"))
                .and_then(Value::as_object_mut)
        {
            changed += usize::from(properties.remove("keys").is_some());
        }
        match value {
            Value::Object(fields) => changed + fields.values_mut().map(old_manage).sum::<usize>(),
            Value::Array(items) => changed + items.iter_mut().map(old_manage).sum::<usize>(),
            _ => changed,
        }
    }
    let env = Env::new("resumed-manage-keys");
    let server = Server::start(vec![
        calls(
            "msg_find",
            400,
            vec![call(
                "find_manage",
                "search",
                json!({"query":"turn a tool off","kind":"tool","limit":1}),
            )],
        ),
        says("msg_one", 500, "One."),
        says("msg_two", 600, "Two."),
        says("msg_three", 700, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = super::found::found_kit(&server, &env, &store, true);
    one_session(&kit, CAPABLE).await;
    let mut previous = body(&server, 1);
    assert!(
        old_manage(&mut previous) > 0,
        "manage must be loaded before the close"
    );
    let store = edited(&store, |value| {
        old_manage(value);
    });
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    prefix::assert_prefix_stable(&[previous, body(&server, 2), body(&server, 3)], None);
    let message = tool_message(&body(&server, 2)).unwrap();
    assert_eq!(by_value_in(&message), ["tool_addition manage by value"]);
    assert!(message["content"].as_array().unwrap().iter().any(|block| {
        block["tool"]["definition"]["input_schema"]["properties"]
            .get("keys")
            .is_some()
    }));
    assert_eq!(tool_message(&body(&server, 3)), None);
    rebuilds(&store, &agent).await;
}
