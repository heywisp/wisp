use super::*;

fn without_both(first: &str, second: &str) -> Vec<String> {
    without(first)
        .into_iter()
        .filter(|name| name != second)
        .collect()
}

fn on_a_gateway_without_show_diff(server: &Server, env: &Env, store: &MemoryStore) -> Kit {
    Kit {
        connect: on_a_gateway(server, env, store).connect,
        ..without_show_diff(server, env, store)
    }
}

fn holds(store: &MemoryStore) -> usize {
    logged_kinds(store)
        .iter()
        .filter(|k| *k == "deferred_tools")
        .count()
}

#[tokio::test]
async fn a_resume_after_a_tool_was_surfaced_sends_the_logged_tools() {
    let env = Env::new("resumed-deferred-surfaced");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.end().await;
    let kit = configured(&server, &env, &store, every_default());
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    kept_prefix(&server, 1, 2);
    assert_eq!(body(&server, 2)["tools"], body(&server, 0)["tools"]);
    assert!(deferred_in(&body(&server, 2)).contains(&"show_diff".to_owned()));
    assert!(tool_message(&body(&server, 2)).is_none());
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resume_after_a_tool_was_turned_off_sends_the_logged_tools() {
    let env = Env::new("resumed-deferred-off");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session
        .tools(1, without_both("show_diff", "view_file"))
        .await;
    session.turn("u2", "two").await;
    session.end().await;
    let kit = configured(
        &server,
        &env,
        &store,
        without_both("show_diff", "view_file"),
    );
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    kept_prefix(&server, 1, 2);
    let resumed = body(&server, 2);
    assert_eq!(resumed["tools"], body(&server, 0)["tools"]);
    assert!(deferred_in(&resumed).contains(&"show_diff".to_owned()));
    assert!(client_names(&resumed).contains(&"view_file".to_owned()));
    assert!(!deferred_in(&resumed).contains(&"view_file".to_owned()));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_deferred_tool_turned_on_while_closed_is_surfaced_by_reference() {
    let env = Env::new("resumed-deferred-on");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&without_show_diff(&server, &env, &store), CAPABLE).await;
    let kit = configured(&server, &env, &store, every_default());
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(changed(&second).len(), 1, "{:?}", changed(&second));
    kept_prefix(&server, 0, 1);
    assert!(deferred_in(&body(&server, 1)).contains(&"show_diff".to_owned()));
    let message = tool_message(&body(&server, 1)).unwrap();
    assert_eq!(additions_in(&message), ["tool_addition show_diff"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_gateway_resumed_after_its_trial_passed_keeps_its_tools_without_deferring() {
    for older in [false, true] {
        let env = Env::new("resumed-deferred-gateway");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        let kit = on_a_gateway_without_show_diff(&server, &env, &store);
        let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent);
        session.turn("u1", "one").await;
        session.tools(1, every_default()).await;
        session.turn("u2", "two").await;
        session.end().await;
        assert_eq!(holds(&store), 1);
        let store = if older {
            edited(&store, |value| {
                if value["type"] == "deferred_tools" {
                    value["model"] = json!(PLAIN);
                }
            })
        } else {
            store
        };
        let kit = Kit {
            connect: on_a_gateway(&server, &env, &store).connect,
            ..configured(&server, &env, &store, every_default())
        };
        let agent = resumed(&kit, &store, &server, CAPABLE).await;
        assert!(!rebuilt(&agent), "{older}");
        let mut session = Session::start(agent);
        let third = session.turn("u3", "three").await;
        let agent = session.end().await;
        assert!(changed(&third).is_empty(), "{older}: {:?}", changed(&third));
        let resumed = body(&server, 2);
        assert!(no_deferral(&resumed), "{older}");
        assert_eq!(resumed["tools"], body(&server, 0)["tools"], "{older}");
        kept_prefix(&server, 1, 2);
        rebuilds(&store, &agent).await;
        assert_eq!(holds(&store), if older { 2 } else { 1 }, "{older}");
    }
}

#[tokio::test]
async fn a_gateway_held_before_a_switch_away_stays_held_when_switched_back_after_a_resume() {
    let env = Env::new("resumed-deferred-switch-back");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = on_a_gateway_without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session
        .tools(1, without_both("show_diff", "view_file"))
        .await;
    session.turn("u2", "two").await;
    change(&mut session, 1, model(PLAIN)).await;
    session.turn("u3", "three").await;
    session.end().await;
    let kit = Kit {
        connect: on_a_gateway(&server, &env, &store).connect,
        ..configured(
            &server,
            &env,
            &store,
            without_both("show_diff", "view_file"),
        )
    };
    let agent = resumed(&kit, &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    change(&mut session, 1, model(CAPABLE)).await;
    session.turn("u4", "four").await;
    session.end().await;
    let back = body(&server, 3);
    assert_eq!(back["model"], CAPABLE);
    assert!(no_deferral(&back), "{back:#}");
    assert_eq!(back["tools"], body(&server, 1)["tools"]);
}

#[tokio::test]
async fn a_kept_rebuild_defers_only_the_logged_tools_still_held() {
    let env = Env::new("resumed-deferred-rebuild");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = configured(
        &server,
        &env,
        &store,
        without_both("show_diff", "view_file"),
    );
    one_session(&kit, CAPABLE).await;
    let logged = deferred_in(&body(&server, 0));
    assert!(logged.contains(&"show_diff".to_owned()));
    assert!(logged.contains(&"view_file".to_owned()));
    let store = edited(&store, |value| {
        if value["type"] == "session_started" {
            value["system_prompt"] = json!("An older prompt.");
        }
    });
    let kit = configured(&server, &env, &store, without("show_diff"));
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    session.end().await;
    let rebuilt_request = body(&server, 1);
    let deferred = deferred_in(&rebuilt_request);
    assert!(deferred.contains(&"show_diff".to_owned()), "{deferred:?}");
    assert!(!deferred.contains(&"view_file".to_owned()), "{deferred:?}");
    assert!(deferred.iter().all(|name| logged.contains(name)));
    assert_eq!(
        client_names(&rebuilt_request)
            .iter()
            .filter(|n| *n == "view_file")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_copy_logged_without_deferred_definitions_defers_nothing() {
    let env = Env::new("resumed-deferred-older");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&without_show_diff(&server, &env, &store), CAPABLE).await;
    let store = edited(&store, |value| {
        if value["type"] == "session_started" {
            value
                .as_object_mut()
                .unwrap()
                .remove("deferred_definitions");
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
    session.turn("u2", "two").await;
    session.end().await;
    assert!(no_deferral(&body(&server, 1)));
}

#[tokio::test]
async fn a_call_to_a_deferred_tool_whose_plugin_is_gone_is_refused_as_turned_off() {
    let env = Env::new("resumed-deferred-call");
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
    one_session(&without_show_diff(&server, &env, &store), CAPABLE).await;
    let kit = plugin_gone(&server, &env, &store, &["show-diff"]);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(body(&server, 1)["tools"], body(&server, 0)["tools"]);
    let shown = result_text(&body(&server, 2), "toolu_01");
    assert!(shown.contains(REFUSED), "{shown}");
    assert!(!shown.contains("no tool named"), "{shown}");
    rebuilds(&store, &agent).await;
}

type Fields = serde_json::Map<String, Value>;

fn older_description(fields: &mut Fields) {
    let older = format!("{} Older.", fields["description"].as_str().unwrap());
    fields.insert("description".into(), json!(older));
}

fn older_schema(fields: &mut Fields) {
    fields["input_schema"]["properties"]
        .as_object_mut()
        .unwrap()
        .insert("older".into(), json!({"type": "string"}));
}

fn deferred_upgraded(
    store: &MemoryStore,
    server: &Server,
    name: &str,
    older: fn(&mut Fields),
) -> MemoryStore {
    let sent = body(server, 0);
    let digest = |upgrade: bool| {
        let tools: Vec<Value> = sent["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| {
                let mut tool = tool.clone();
                let fields = tool.as_object_mut().unwrap();
                fields.remove("cache_control");
                if upgrade && fields["name"] == name {
                    older(fields);
                }
                tool
            })
            .collect();
        json!(crate::session::sha256_hex(
            &serde_json::to_string(&tools).unwrap()
        ))
    };
    let (old, new) = (digest(false), digest(true));
    edited(store, |value| {
        if value["tools_sha256"] == old {
            value["tools_sha256"] = new.clone();
        }
        if value["type"] == "session_started" {
            for definition in value["deferred_definitions"].as_array_mut().unwrap() {
                if definition["name"] == name {
                    older(definition.as_object_mut().unwrap());
                }
            }
        }
    })
}

fn today_by_value(message: &Value, name: &str) -> bool {
    message["content"].as_array().unwrap().iter().any(|block| {
        block["tool"]["type"] == "tool_definition"
            && block["tool"]["definition"]["name"] == name
            && !block["tool"]["definition"]["description"]
                .as_str()
                .unwrap()
                .ends_with("Older.")
            && block["tool"]["definition"]["input_schema"]["properties"]
                .get("older")
                .is_none()
    })
}

#[tokio::test]
async fn an_upgraded_deferred_tool_surfaced_before_the_close_goes_once_by_value() {
    let env = Env::new("resumed-deferred-upgraded");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.end().await;
    let store = deferred_upgraded(&store, &server, "show_diff", older_description);
    let kit = configured(&server, &env, &store, every_default());
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let message = tool_message(&body(&server, 2)).unwrap();
    assert_eq!(additions_in(&message), ["tool_addition show_diff by value"]);
    assert!(today_by_value(&message, "show_diff"), "{message:#}");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn an_upgraded_schema_alone_of_a_deferred_tool_goes_once_by_value() {
    let env = Env::new("resumed-deferred-upgraded-schema");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.end().await;
    let store = deferred_upgraded(&store, &server, "show_diff", older_schema);
    let kit = configured(&server, &env, &store, every_default());
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let message = tool_message(&body(&server, 2)).unwrap();
    assert_eq!(additions_in(&message), ["tool_addition show_diff by value"]);
    assert!(today_by_value(&message, "show_diff"), "{message:#}");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn an_upgraded_deferred_tool_turned_on_after_the_resume_goes_by_value() {
    let env = Env::new("resumed-deferred-upgraded-on");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    one_session(&without_show_diff(&server, &env, &store), CAPABLE).await;
    let store = deferred_upgraded(&store, &server, "show_diff", older_description);
    let kit = configured(&server, &env, &store, without("show_diff"));
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    let agent = session.end().await;
    let message = tool_message(&body(&server, 1)).unwrap();
    assert_eq!(additions_in(&message), ["tool_addition show_diff by value"]);
    assert!(today_by_value(&message, "show_diff"), "{message:#}");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resume_after_a_refused_deferral_keeps_the_tools_it_sent() {
    let env = Env::new("resumed-deferred-refused");
    let server = Server::start(vec![
        refused_deferral(),
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.turn("u2", "two").await;
    session.end().await;
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let resumed_request = body(&server, 3);
    assert!(no_deferral(&resumed_request));
    assert_eq!(resumed_request["tools"], body(&server, 2)["tools"]);
    let explained: Vec<Value> = log_events(&store)
        .iter()
        .filter(|e| matches!(e, LogEvent::CacheMiss { .. }))
        .map(|e| serde_json::to_value(e).unwrap())
        .filter(|miss| miss["cause"]["type"] != "unexplained")
        .collect();
    assert!(explained.is_empty(), "{explained:?}");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_tool_turned_on_while_closed_goes_by_value_on_a_held_gateway() {
    let env = Env::new("resumed-deferred-held-on");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = on_a_gateway_without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session
        .tools(1, without_both("show_diff", "view_file"))
        .await;
    session.turn("u2", "two").await;
    session.end().await;
    let kit = Kit {
        connect: on_a_gateway(&server, &env, &store).connect,
        ..configured(&server, &env, &store, without("view_file"))
    };
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let resumed_request = body(&server, 2);
    assert!(no_deferral(&resumed_request));
    assert_eq!(resumed_request["tools"], body(&server, 0)["tools"]);
    let message = tool_message(&resumed_request).unwrap();
    assert_eq!(additions_in(&message), ["tool_addition show_diff by value"]);
    rebuilds(&store, &agent).await;
}

fn rebuilt_deferred(store: &MemoryStore) -> Vec<Vec<String>> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::ToolsChanged { copy, .. } if copy.system_prompt.is_some() => Some(
                copy.whole
                    .deferred_definitions
                    .iter()
                    .filter_map(|d| d["name"].as_str().map(str::to_owned))
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

async fn rebuilt_live(server: &Server, env: &Env, store: &MemoryStore) -> Vec<String> {
    let kit = configured(server, env, store, without_both("show_diff", "view_file"));
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(&mut session, 1, model(PLAIN)).await;
    session
        .tools(2, without_both("view_file", "write_todos"))
        .await;
    let plain = session.turn("u2", "two").await;
    assert_eq!(
        changed(&plain),
        [(
            vec!["show_diff".to_owned()],
            vec!["write_todos".to_owned()],
            ToolChangeVia::Rebuild,
            Some(CacheEffect::Rebuilt {
                reason: wisp_protocol::RebuildReason::ToolsChange,
            })
        )]
    );
    change(&mut session, 3, model(CAPABLE)).await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    rebuilds(store, &agent).await;
    deferred_in(&body(server, 0))
}

#[tokio::test]
async fn a_live_rebuild_keeps_the_deferred_set_without_the_tool_it_turns_on() {
    let env = Env::new("rebuild-deferred-live");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let logged = rebuilt_live(&server, &env, &store).await;
    assert!(logged.contains(&"show_diff".to_owned()), "{logged:?}");
    assert!(logged.contains(&"view_file".to_owned()), "{logged:?}");
    let kept: Vec<String> = logged
        .iter()
        .filter(|n| *n != "show_diff")
        .cloned()
        .collect();
    let (plain, back) = (body(&server, 1), body(&server, 2));
    assert!(no_deferral(&plain));
    assert!(client_names(&plain).contains(&"show_diff".to_owned()));
    assert!(!client_names(&plain).contains(&"view_file".to_owned()));
    assert_eq!(rebuilt_deferred(&store), std::slice::from_ref(&kept));
    assert_eq!(deferred_in(&back), kept);
    assert_eq!(
        client_names(&back)
            .iter()
            .filter(|n| *n == "show_diff")
            .count(),
        1
    );
    let message = tool_message(&back).unwrap();
    assert_eq!(kinds(&message), ["tool_removal write_todos"]);
}

#[tokio::test]
async fn a_rebuild_never_defers_a_tool_the_old_array_declared_normally() {
    let env = Env::new("rebuild-deferred-normal");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let logged = rebuilt_live(&server, &env, &store).await;
    let first = body(&server, 0);
    let normal: Vec<String> = client_names(&first)
        .into_iter()
        .filter(|n| !logged.contains(n))
        .collect();
    assert!(normal.contains(&"write_todos".to_owned()), "{normal:?}");
    let back = body(&server, 2);
    let deferred = deferred_in(&back);
    for name in &normal {
        assert!(client_names(&back).contains(name), "{name}");
        assert!(!deferred.contains(name), "{name}");
    }
    assert!(deferred.iter().all(|name| logged.contains(name)));
}

#[tokio::test]
async fn a_resume_after_a_live_rebuild_adopts_its_deferred_set_and_sends_the_same_bytes() {
    let env = Env::new("rebuild-deferred-resumed");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    rebuilt_live(&server, &env, &store).await;
    let kit = configured(
        &server,
        &env,
        &store,
        without_both("view_file", "write_todos"),
    );
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    kept_prefix(&server, 2, 3);
    assert_eq!(body(&server, 3)["tools"], body(&server, 2)["tools"]);
    assert!(tool_message(&body(&server, 3)).is_none());
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_rebuild_after_an_adopted_copy_logs_the_kept_set_and_the_next_resume_declares_once() {
    let env = Env::new("rebuild-deferred-adopted");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let off = || without_both("show_diff", "view_file");
    one_session(&configured(&server, &env, &store, off()), CAPABLE).await;
    let logged = deferred_in(&body(&server, 0));
    assert!(logged.contains(&"show_diff".to_owned()), "{logged:?}");
    assert!(logged.contains(&"view_file".to_owned()), "{logged:?}");
    let agent = resumed(
        &configured(&server, &env, &store, off()),
        &store,
        &server,
        CAPABLE,
    )
    .await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    change(&mut session, 1, model(PLAIN)).await;
    session.tools(2, without("view_file")).await;
    let second = session.turn("u2", "two").await;
    session.end().await;
    assert_eq!(changed(&second)[0].2, ToolChangeVia::Rebuild);
    let kept: Vec<String> = logged
        .iter()
        .filter(|n| *n != "show_diff")
        .cloned()
        .collect();
    assert_eq!(rebuilt_deferred(&store), std::slice::from_ref(&kept));
    let kit = configured(&server, &env, &store, without("view_file"));
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    change(&mut session, 1, model(CAPABLE)).await;
    session.turn("u3", "three").await;
    session.end().await;
    let back = body(&server, 2);
    assert_eq!(back["model"], CAPABLE);
    assert_eq!(deferred_in(&back), kept);
    assert_eq!(
        client_names(&back)
            .iter()
            .filter(|n| *n == "show_diff")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_held_gateway_that_rebuilds_at_its_first_opening_keeps_one_hold_and_its_bytes_at_the_next_resume()
 {
    let env = Env::new("rebuild-deferred-held");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let kit = on_a_gateway_without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session
        .tools(1, without_both("show_diff", "view_file"))
        .await;
    session.turn("u2", "two").await;
    session.end().await;
    assert_eq!(holds(&store), 1);
    let gateway = || Kit {
        connect: on_a_gateway(&server, &env, &store).connect,
        ..configured(&server, &env, &store, every_default())
    };
    let agent = resumed(&gateway(), &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    change(&mut session, 1, model(PLAIN)).await;
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(
        changed(&third)
            .iter()
            .any(|(added, _, via, _)| *via == ToolChangeVia::Rebuild
                && added.contains(&"show_diff".to_owned())),
        "{:?}",
        changed(&third)
    );
    rebuilds(&store, &agent).await;
    assert_eq!(holds(&store), 1);
    let agent = resumed(&gateway(), &store, &server, PLAIN).await;
    assert!(!rebuilt(&agent));
    let mut session = Session::start(agent);
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    kept_prefix(&server, 2, 3);
    assert_eq!(body(&server, 3)["tools"], body(&server, 2)["tools"]);
    assert!(no_deferral(&body(&server, 3)));
    rebuilds(&store, &agent).await;
    assert_eq!(holds(&store), 1);
}

fn without_three(first: &str, second: &str, third: &str) -> Vec<String> {
    without_both(first, second)
        .into_iter()
        .filter(|name| name != third)
        .collect()
}

fn system_kinds(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .flat_map(kinds)
        .collect()
}

fn names_once(request: &Value) -> bool {
    let names = client_names(request);
    let unique: std::collections::BTreeSet<&String> = names.iter().collect();
    unique.len() == names.len()
}

async fn flipped_with_one_surfaced(kit: &Kit, server: &Server) -> Agent {
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("view_file")).await;
    session.turn("u2", "two").await;
    session
        .tools(2, without_both("view_file", crate::search::WEB_SEARCH))
        .await;
    let flipped = session.turn("u3", "three").await;
    assert_eq!(
        changed(&flipped),
        [(
            Vec::new(),
            vec![crate::search::WEB_SEARCH.to_owned()],
            ToolChangeVia::Rebuild,
            Some(CacheEffect::Rebuilt {
                reason: wisp_protocol::RebuildReason::ToolsChange,
            })
        )]
    );
    let rebuilt = body(server, 2);
    assert!(!client_names(&rebuilt).contains(&crate::search::WEB_SEARCH.to_owned()));
    assert!(names_once(&rebuilt), "{rebuilt:#}");
    session.end().await
}

#[tokio::test]
async fn a_web_search_flip_on_a_deferring_pair_keeps_the_set_and_the_surfaced_reference() {
    let env = Env::new("deferred-search-flip");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let wanted = without_three("show_diff", "view_file", crate::search::WEB_SEARCH);
    let kit = configured(
        &server,
        &env,
        &store,
        without_both("show_diff", "view_file"),
    );
    let agent = flipped_with_one_surfaced(&kit, &server).await;
    rebuilds(&store, &agent).await;
    let (surfaced, flipped) = (body(&server, 1), body(&server, 2));
    assert_eq!(
        deferred_in(&surfaced),
        ["show_diff", "show_page", "view_file"]
    );
    assert_eq!(deferred_in(&flipped), deferred_in(&surfaced));
    assert!(system_kinds(&flipped).contains(&"tool_addition show_diff".to_owned()));
    assert_eq!(
        rebuilt_deferred(&store).last().unwrap(),
        &["show_diff", "show_page", "view_file"]
    );
    let mut on_now = wanted;
    on_now.push("show_diff".to_owned());
    let kit = configured(&server, &env, &store, on_now);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!rebuilt(&agent), "{:?}", agent.cache_rebuild());
    let mut session = Session::start(agent);
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    assert!(changed(&fourth).is_empty(), "{:?}", changed(&fourth));
    kept_prefix(&server, 2, 3);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_web_search_flip_where_the_deferral_is_held_keeps_the_set_and_the_by_value_addition() {
    let env = Env::new("deferred-search-flip-held");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = Kit {
        connect: on_a_gateway(&server, &env, &store).connect,
        ..configured(
            &server,
            &env,
            &store,
            without_both("show_diff", "view_file"),
        )
    };
    let agent = flipped_with_one_surfaced(&kit, &server).await;
    assert_eq!(holds(&store), 1);
    let rebuilt = body(&server, 2);
    assert!(no_deferral(&rebuilt), "{rebuilt:#}");
    assert!(!client_names(&rebuilt).contains(&"show_diff".to_owned()));
    assert!(system_kinds(&rebuilt).contains(&"tool_addition show_diff by value".to_owned()));
    assert_eq!(
        rebuilt_deferred(&store).last().unwrap(),
        &["show_diff", "show_page", "view_file"]
    );
    rebuilds(&store, &agent).await;
}
