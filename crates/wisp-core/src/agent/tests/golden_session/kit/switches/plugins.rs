use wisp_protocol::settings::INSTALLED_KEY;

use super::*;
use crate::plugin::testing::{Disk, PluginMoves, installed, plugin_moves, plugins_changed};
use wisp_protocol::settings::SitePath;

#[tokio::test]
async fn a_plugin_removed_mid_session_takes_its_tools_out_inline_and_keeps_the_prefix() {
    let env = Env::new("plugin-removed-inline");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let answer = session.switched(1, "show-diff", INSTALLED_KEY, false).await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(CacheEffect::Kept)));
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec!["show_code".to_owned(), "show_diff".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    assert_eq!(
        plugins_changed(&second),
        [(Vec::new(), vec!["show-diff".to_owned()], Vec::new())]
    );
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (first, second, third) = (body(&server, 0), body(&server, 1), body(&server, 2));
    assert_eq!(first["tools"], second["tools"]);
    assert_eq!(first["system"], second["system"]);
    assert_eq!(second["system"], third["system"]);
    let (_, system) = last_two(&second);
    assert_eq!(
        kinds(&system),
        ["tool_removal show_code", "tool_removal show_diff"]
    );
    let messages = third["messages"].as_array().unwrap();
    assert_eq!(
        messages[..messages.len() - 2],
        second["messages"].as_array().unwrap()[..]
    );
    assert!(agent.catalog().tool("show_diff").is_none_or(|t| !t.on));
    assert!(
        !agent
            .hooks
            .engine
            .plugin_ids()
            .contains(&"show-diff".to_owned())
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn plugins_removed_at_start_and_added_back_send_their_tools_and_sentence_once() {
    let env = Env::new("plugin-added-back");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = plugin_gone(&server, &env, &store, &["show-diff", "time"]);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let joining = session.switched(1, "show-diff", INSTALLED_KEY, true).await;
    for event in joining {
        if let AgentEvent::CatalogChanged { catalog } = event {
            assert!(catalog.tool("show_diff").is_none_or(|tool| !tool.waits));
        }
    }
    session.switched(2, "time", INSTALLED_KEY, true).await;
    let second = session.turn("u2", "two").await;
    assert_eq!(
        plugins_changed(&second),
        [(
            vec!["show-diff".to_owned(), "time".to_owned()],
            Vec::new(),
            Vec::new()
        )]
    );
    let third = session.turn("u3", "three").await;
    assert!(changed(&third).is_empty());
    assert!(plugins_changed(&third).is_empty());
    let agent = session.end().await;
    let (first, second, third) = (body(&server, 0), body(&server, 1), body(&server, 2));
    assert_eq!(first["tools"], second["tools"]);
    assert_eq!(first["system"], second["system"]);
    assert!(!first["system"].to_string().contains("'Sent:' line"));
    let (_, system) = last_two(&second);
    assert_eq!(
        kinds(&system),
        [
            "tool_addition show_code by value",
            "tool_addition show_diff by value",
            "text"
        ]
    );
    let text = &texts(&system)[0];
    assert!(text.contains("show_diff"), "{text}");
    assert!(text.contains("'Sent:' line"), "{text}");
    let messages = third["messages"].as_array().unwrap();
    assert_eq!(
        messages[..messages.len() - 2],
        second["messages"].as_array().unwrap()[..]
    );
    assert_eq!(messages.iter().filter(|m| m["role"] == "system").count(), 1);
    rebuilds(&store, &agent).await;
}

fn failing(calls: &Arc<std::sync::atomic::AtomicUsize>) -> crate::plugin::Make {
    let calls = calls.clone();
    Arc::new(move |_| {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        panic!("left out")
    })
}

fn warm_replies() -> Vec<Reply> {
    vec![
        says("msg_01", 400, "One."),
        super::super::keep_warm::refreshes_read("msg_r1", 400),
        says("msg_02", 520, "Two."),
        super::super::keep_warm::refreshes_read("msg_r2", 520),
        says("msg_03", 640, "Three."),
    ]
}

fn never_rebuilt(turns: &[Vec<AgentEvent>]) {
    for events in turns {
        assert!(
            changed(events)
                .iter()
                .all(|(_, _, via, _)| *via != ToolChangeVia::Rebuild),
            "{:?}",
            changed(events)
        );
    }
}

#[tokio::test]
async fn a_plugin_left_out_at_start_answers_kept_to_its_switch_and_never_rebuilds() {
    use wisp_protocol::settings::ENABLED_KEY;
    let env = Env::new("plugin-left-out");
    let server = Server::start(warm_replies()).await;
    let store = MemoryStore::default();
    let calls = Arc::default();
    let kit = plugins_kit(&server, &env, &store, &[], Some(failing(&calls)));
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent.with_keep_warm(8));
    let first = session.turn("u1", "one").await;
    let answer = session.switched(1, "show-diff", ENABLED_KEY, false).await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(CacheEffect::Kept)));
    session.refreshed().await;
    let second = session.turn("u2", "two").await;
    session.refreshed().await;
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    never_rebuilt(&[first, second, third]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!agent.rebuild_due());
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn an_add_back_whose_start_fails_notes_it_once_never_rebuilds_and_is_not_tried_again() {
    let env = Env::new("plugin-add-back-fails");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        super::super::keep_warm::refreshes_read("msg_r1", 520),
        says("msg_03", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let calls = Arc::default();
    let kit = plugins_kit(&server, &env, &store, &["show-diff"], Some(failing(&calls)));
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent.with_keep_warm(8));
    let first = session.turn("u1", "one").await;
    session.switched(1, "show-diff", INSTALLED_KEY, true).await;
    let second = session.turn("u2", "two").await;
    assert_eq!(
        plugins_changed(&second),
        [(Vec::new(), Vec::new(), vec!["show-diff".to_owned()])]
    );
    session.refreshed().await;
    let third = session.turn("u3", "three").await;
    assert!(plugins_changed(&third).is_empty());
    let agent = session.end().await;
    never_rebuilt(&[first, second, third]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!agent.rebuild_due());
    assert_eq!(body(&server, 1)["tools"], body(&server, 0)["tools"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resume_after_a_plugin_was_removed_sends_the_same_prefix_and_changes_nothing() {
    let env = Env::new("plugin-removed-resumed");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.switched(1, "show-diff", INSTALLED_KEY, false).await;
    session.turn("u2", "two").await;
    session.end().await;
    let kit = plugin_gone(&server, &env, &store, &["show-diff"]);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    assert!(plugins_changed(&third).is_empty());
    let agent = session.end().await;
    kept_prefix(&server, 1, 2);
    assert!(tool_message(&body(&server, 2)).is_none());
    assert!(!client_names(&body(&server, 2)).is_empty());
    assert!(agent.catalog().tool("show_diff").is_none_or(|t| !t.on));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resume_after_a_plugin_was_added_back_sends_the_same_prefix_and_changes_nothing() {
    let env = Env::new("plugin-added-back-resumed");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = plugin_gone(&server, &env, &store, &["show-diff"]);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.switched(1, "show-diff", INSTALLED_KEY, true).await;
    session.turn("u2", "two").await;
    session.end().await;
    let kit = switching(&server, &env, &store);
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    assert!(plugins_changed(&third).is_empty());
    let agent = session.end().await;
    kept_prefix(&server, 1, 2);
    assert!(tool_message(&body(&server, 2)).is_none());
    assert!(agent.catalog().tool("show_diff").is_some_and(|t| t.on));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_root_that_cannot_switch_tools_keeps_a_removed_plugin_whose_tools_it_sent() {
    let env = Env::new("plugin-kept-line-mode");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.start_layers.full_screen = false;
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.switched(1, "show-diff", INSTALLED_KEY, false).await;
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    assert!(plugins_changed(&second).is_empty());
    assert!(changed(&second).is_empty());
    assert_eq!(body(&server, 0)["tools"], body(&server, 1)["tools"]);
    assert!(
        agent
            .hooks
            .engine
            .plugin_ids()
            .contains(&"show-diff".to_owned())
    );
    assert!(agent.toolbox.has_plugin("show-diff"));
    let entry = agent
        .catalog()
        .plugins
        .iter()
        .find(|p| p.id == "show-diff")
        .cloned();
    assert!(
        entry.is_some_and(|p| p.installed),
        "the plugin stays installed"
    );
}

fn board(version: &str, says: &str) -> crate::plugin::Source {
    installed("board", version, "board_read", says)
}

fn memo(version: &str, tool: &str, says: &str) -> crate::plugin::Source {
    installed("memo", version, tool, says)
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_owned()).collect()
}

fn watching(kit: Kit, extra: Vec<crate::plugin::Source>) -> (Kit, Arc<Disk>) {
    let disk = Arc::new(Disk::default());
    let registry = Arc::new(registry_without(&[]).with_installed(extra).0);
    let on = registry.default_on();
    let installed: Arc<dyn crate::plugin::Installed> = disk.clone();
    let tools = crate::plugin::Tools::watching(registry, on, Some(installed));
    (
        Kit {
            tools: Arc::new(tools),
            ..kit
        },
        disk,
    )
}

#[tokio::test]
async fn a_plugin_installed_mid_session_joins_inline_and_a_child_started_later_has_it() {
    let env = Env::new("plugin-installed-inline");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let (kit, disk) = watching(switching(&server, &env, &store), Vec::new());
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    disk.put(vec![board("0.1.0", "Reads a board.")], Vec::new());
    let second = session.turn("u2", "two").await;
    assert_eq!(
        plugin_moves(&second),
        [PluginMoves {
            added: ids(&["board"]),
            ..PluginMoves::default()
        }]
    );
    assert_eq!(
        changed(&second),
        [(
            ids(&["board_read"]),
            Vec::new(),
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    let third = session.turn("u3", "three").await;
    assert!(plugin_moves(&third).is_empty());
    let agent = session.end().await;
    let (first, second, third) = (body(&server, 0), body(&server, 1), body(&server, 2));
    assert_eq!(first["tools"], second["tools"]);
    assert_eq!(first["system"], second["system"]);
    assert_eq!(second["system"], third["system"]);
    let (_, system) = last_two(&second);
    assert_eq!(kinds(&system), ["tool_addition board_read by value"]);
    let messages = third["messages"].as_array().unwrap();
    assert_eq!(
        messages[..messages.len() - 2],
        second["messages"].as_array().unwrap()[..]
    );
    rebuilds(&store, &agent).await;
    let child = kit
        .tools
        .start_only(
            "/root/child",
            &ids(&["board_read"]),
            &crate::plugin::PluginSwitches::default(),
            Arc::new(crate::plugin::KernelHost::new()),
        )
        .await;
    assert_eq!(child.toolbox.names(), ids(&["board_read"]));
}

#[tokio::test]
async fn an_installed_update_goes_by_value_with_no_tools_line() {
    let env = Env::new("plugin-updated-inline");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let (kit, disk) = watching(
        switching(&server, &env, &store),
        vec![board("0.1.0", "Reads a board.")],
    );
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    disk.put(
        vec![board("0.2.0", "Reads a board and its owners.")],
        Vec::new(),
    );
    let second = session.turn("u2", "two").await;
    assert_eq!(
        plugin_moves(&second),
        [PluginMoves {
            updated: ids(&["board"]),
            ..PluginMoves::default()
        }]
    );
    assert!(changed(&second).is_empty(), "{:?}", changed(&second));
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (first, second) = (body(&server, 0), body(&server, 1));
    assert_eq!(first["tools"], second["tools"]);
    assert_eq!(first["system"], second["system"]);
    let (_, system) = last_two(&second);
    assert_eq!(kinds(&system), ["tool_addition board_read by value"]);
    assert!(system.to_string().contains("its owners"), "{system}");
    assert!(agent.catalog().tool("board_read").is_some_and(|t| t.on));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_failed_update_keeps_the_old_version_reports_kept_and_is_not_tried_again() {
    let env = Env::new("plugin-update-fails");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let (kit, disk) = watching(
        switching(&server, &env, &store),
        vec![board("0.1.0", "Reads a board.")],
    );
    let calls = Arc::default();
    let broken = || crate::plugin::Source {
        make: failing(&calls),
        ..board("0.2.0", "Reads a board badly.")
    };
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    disk.put(vec![broken()], Vec::new());
    let second = session.turn("u2", "two").await;
    assert_eq!(
        plugin_moves(&second),
        [PluginMoves {
            kept: ids(&["board"]),
            notes: ids(&["board"]),
            ..PluginMoves::default()
        }]
    );
    disk.put(vec![broken()], Vec::new());
    let third = session.turn("u3", "three").await;
    assert!(plugin_moves(&third).is_empty());
    let agent = session.end().await;
    assert!(changed(&second).is_empty() && changed(&third).is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(agent.hooks.engine.has_plugin("board"));
    let entry = agent
        .catalog()
        .plugins
        .iter()
        .find(|p| p.id == "board")
        .cloned();
    assert!(entry.is_some_and(|p| p.version == "0.1.0"));
    assert_eq!(body(&server, 0)["tools"], body(&server, 2)["tools"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn an_update_of_a_left_out_plugin_is_never_kept_and_its_fix_is_added() {
    let env = Env::new("plugin-left-out-update");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let calls = Arc::default();
    let broken = |version: &str| crate::plugin::Source {
        make: failing(&calls),
        ..board(version, "Reads a board badly.")
    };
    let (kit, disk) = watching(switching(&server, &env, &store), vec![broken("0.1.0")]);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    disk.put(vec![broken("0.1.1")], Vec::new());
    let second = session.turn("u2", "two").await;
    assert_eq!(
        plugin_moves(&second),
        [PluginMoves {
            notes: ids(&["board"]),
            ..PluginMoves::default()
        }]
    );
    disk.put(vec![board("0.1.2", "Reads a board.")], Vec::new());
    let third = session.turn("u3", "three").await;
    assert_eq!(
        plugin_moves(&third),
        [PluginMoves {
            added: ids(&["board"]),
            ..PluginMoves::default()
        }]
    );
    let agent = session.end().await;
    assert!(agent.hooks.engine.has_plugin("board"));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn where_tools_cannot_go_inline_a_changed_reinstall_waits_and_never_rebuilds() {
    let env = Env::new("plugin-reinstall-plain");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let (kit, disk) = watching(
        switching(&server, &env, &store),
        vec![board("0.1.0", "Reads a board.")],
    );
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    let first = session.turn("u1", "one").await;
    disk.put(Vec::new(), Vec::new());
    let second = session.turn("u2", "two").await;
    disk.put(
        vec![board("0.2.0", "Reads a board and its owners.")],
        Vec::new(),
    );
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    never_rebuilt(&[first, second, third]);
    assert!(!agent.rebuild_due());
    assert!(
        agent.catalog().tool("board_read").is_some_and(|t| t.waits),
        "{:?}",
        agent.catalog().tool("board_read")
    );
    for n in 1..3 {
        assert_eq!(body(&server, 0)["tools"], body(&server, n)["tools"]);
    }
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn where_tools_cannot_go_inline_new_tools_wait_and_a_changed_update_is_kept() {
    type KitOf = fn(&Server, &Env, &MemoryStore) -> Kit;
    let pairs: [(&str, KitOf, &str); 2] = [
        ("plugin-held-plain", switching, PLAIN),
        ("plugin-held-gateway", on_a_gateway, CAPABLE),
    ];
    for (name, kit_of, model) in pairs {
        let env = Env::new(name);
        let server = Server::start(vec![
            says("msg_01", 400, "One."),
            says("msg_02", 520, "Two."),
            super::super::keep_warm::refreshes_read("msg_r1", 520),
            says("msg_03", 640, "Three."),
        ])
        .await;
        let store = MemoryStore::default();
        let (kit, disk) = watching(
            kit_of(&server, &env, &store),
            vec![memo("0.1.0", "memo_read", "Reads memos.")],
        );
        let built = kit.build(on(model, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent.with_keep_warm(8));
        let first = session.turn("u1", "one").await;
        disk.put(
            vec![
                memo("0.2.0", "memo_read", "Reads memos and their dates."),
                board("0.1.0", "Reads a board."),
            ],
            Vec::new(),
        );
        let second = session.turn("u2", "two").await;
        assert_eq!(
            plugin_moves(&second),
            [PluginMoves {
                added: ids(&["board"]),
                kept: ids(&["memo"]),
                notes: ids(&["board", "memo"]),
                ..PluginMoves::default()
            }],
            "{name}"
        );
        session.refreshed().await;
        let third = session.turn("u3", "three").await;
        let agent = session.end().await;
        never_rebuilt(&[first, second, third]);
        assert!(!agent.rebuild_due(), "{name}");
        assert!(
            agent.catalog().tool("board_read").is_some_and(|t| t.waits),
            "{name}: {:?}",
            agent.catalog().tool("board_read")
        );
        assert!(!agent.toolbox.has_plugin("board"), "{name}");
        for n in 1..4 {
            assert_eq!(
                body(&server, 0)["tools"],
                body(&server, n)["tools"],
                "{name}"
            );
            assert_eq!(
                body(&server, 0)["system"],
                body(&server, n)["system"],
                "{name}"
            );
        }
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn the_saved_tools_list_gains_a_new_plugins_tools_and_a_config_list_is_kept_as_given() {
    let env = Env::new("plugin-saved-tools");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let (mut kit, disk) = watching(
        switching(&server, &env, &store),
        vec![memo("0.1.0", "memo_read", "Reads memos.")],
    );
    let path = SitePath::global(wisp_protocol::settings::find("tools").unwrap());
    let saved: Vec<String> = registry_without(&[])
        .with_installed(vec![memo("0.1.0", "memo_read", "Reads memos.")])
        .0
        .default_on()
        .into_iter()
        .collect();
    kit.start_layers
        .saved
        .insert(path.clone(), SettingValue::List(saved.clone()), 1);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let toggled: Vec<String> = saved
        .iter()
        .filter(|n| *n != "show_diff")
        .cloned()
        .collect();
    session.tools(1, toggled.clone()).await;
    disk.put(
        vec![
            memo("0.2.0", "memo_view", "Views memos."),
            board("0.1.0", "Reads a board."),
        ],
        Vec::new(),
    );
    session.turn("u2", "two").await;
    let agent = session.end().await;
    let now = agent
        .settings
        .layers
        .saved
        .get(&path)
        .map(|s| s.value.clone());
    let merged = |list: Vec<String>| {
        let mut list: Vec<String> = list.into_iter().filter(|n| n != "memo_read").collect();
        list.extend(ids(&["board_read", "memo_view"]));
        SettingValue::List(list)
    };
    assert_eq!(now, Some(merged(saved)));
    let session_list: Vec<SettingValue> = agent
        .settings
        .session
        .iter()
        .filter(|g| g.target.spec().name == "tools")
        .map(|g| g.value.clone())
        .collect();
    assert_eq!(session_list, [SettingValue::List(toggled)]);
    for tool in ["board_read", "memo_view", "show_diff"] {
        assert!(agent.catalog().tool(tool).is_some_and(|t| !t.on), "{tool}");
    }
    assert!(agent.catalog().tool("memo_read").is_none());
}

#[tokio::test]
async fn a_saved_tools_list_naming_a_deleted_plugins_tool_is_not_merged() {
    let env = Env::new("plugin-saved-tools-ghost");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let memo = || memo("0.1.0", "memo_read", "Reads memos.");
    let (mut kit, disk) = watching(switching(&server, &env, &store), vec![memo()]);
    let path = SitePath::global(wisp_protocol::settings::find("tools").unwrap());
    let saved = SettingValue::List(
        registry_without(&[])
            .with_installed(vec![memo()])
            .0
            .default_on()
            .into_iter()
            .collect(),
    );
    kit.start_layers
        .saved
        .insert(path.clone(), saved.clone(), 1);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    disk.put(vec![board("0.1.0", "Reads a board.")], Vec::new());
    session.turn("u2", "two").await;
    let agent = session.end().await;
    let now = agent
        .settings
        .layers
        .saved
        .get(&path)
        .map(|s| s.value.clone());
    assert_eq!(now, Some(saved));
    assert!(agent.catalog().tool("board_read").is_some_and(|t| !t.on));
}

#[tokio::test]
async fn a_clashing_tool_left_after_its_rival_is_deleted_does_not_wait() {
    let env = Env::new("plugin-clash-left");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let rival = || memo("0.1.0", "board_read", "Reads memos.");
    let (kit, disk) = watching(
        switching(&server, &env, &store),
        vec![board("0.1.0", "Reads a board.")],
    );
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    disk.put(vec![board("0.1.0", "Reads a board."), rival()], Vec::new());
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            ids(&["board_read"]),
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    disk.put(vec![rival()], Vec::new());
    let third = session.turn("u3", "three").await;
    let agent = session.end().await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    assert!(
        agent
            .catalog()
            .tool("board_read")
            .is_some_and(|t| !t.on && !t.waits),
        "{:?}",
        agent.catalog().tool("board_read")
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn another_providers_off_and_on_switches_keep_every_prefix_byte() {
    for key in [wisp_protocol::settings::ENABLED_KEY, INSTALLED_KEY] {
        let env = Env::new("provider-off-and-on-prefix");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        let kit = switching(&server, &env, &store);
        let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent);
        session.turn("u1", "one").await;
        assert_eq!(
            answered(&session.switched(1, "gemini", key, false).await),
            (When::Now, None)
        );
        session.turn("u2", "two").await;
        assert_eq!(
            answered(&session.switched(2, "gemini", key, true).await),
            (When::Now, None)
        );
        session.turn("u3", "three").await;
        let agent = session.end().await;
        prefix::assert_prefix_stable(
            &[body(&server, 0), body(&server, 1), body(&server, 2)],
            None,
        );
        assert_eq!(tool_message(&body(&server, 1)), None);
        assert_eq!(tool_message(&body(&server, 2)), None);
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn an_accepted_provider_switch_resumes_with_the_logged_prefix() {
    let env = Env::new("provider-off-resumed-prefix");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session
        .switched(1, "gemini", wisp_protocol::settings::ENABLED_KEY, false)
        .await;
    session.end().await;
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "two").await;
    let agent = session.end().await;
    kept_prefix(&server, 0, 1);
    assert_eq!(tool_message(&body(&server, 1)), None);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_refused_switch_of_this_chats_provider_keeps_its_native_request_prefix() {
    let env = Env::new("provider-refused-prefix");
    let response = http::sse(&format!(
        "data: {}\n\n",
        json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "Answer."}]}, "finishReason": "STOP"}], "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 2}})
    ));
    let server = Server::start(
        (0..2)
            .map(|_| Reply {
                response: response.clone(),
                before: Vec::new(),
                gate: None,
            })
            .collect(),
    )
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    let endpoint = crate::endpoints::ResolvedEndpoint {
        name: crate::test_guard::name("g1"),
        provider: crate::endpoints::ProviderKind::Gemini,
        credential: Credential::ApiKey("CANARY_GEMINI_GOLDEN".into()),
        base_url: server.base.clone(),
        source: crate::provider::credential::Source::Environment,
        key_env: None,
        url_source: crate::endpoints::UrlSource::Official,
        pricing: crate::endpoints::PricingMode::List,
    };
    let provider: Arc<dyn Provider> =
        Arc::new(crate::provider::gemini::GeminiProvider::from_endpoint(&endpoint).unwrap());
    kit.connect = Arc::new(move |_: &str| {
        Ok(crate::kit::Connected {
            provider: provider.clone(),
            ready: vec![crate::agent::Connection {
                name: "g1".into(),
                provider: provider.clone(),
                context: log_context(),
                priced: true,
            }],
            unavailable: Vec::new(),
            active: "g1".into(),
            env_account: None,
        })
    });
    kit.count_tokens = false;
    let built = kit
        .build(on(
            crate::provider::gemini::default_model(),
            kit::Start::Fresh,
        ))
        .await
        .unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session
        .send(crate::plugin::testing::plugin_switch(
            1,
            "gemini",
            wisp_protocol::settings::ENABLED_KEY,
            false,
        ))
        .await;
    let refused = session
        .until(|event| {
            matches!(
                event,
                AgentEvent::Rejected {
                    request: Some(RequestId(1)),
                    ..
                }
            )
        })
        .await;
    assert!(refused.iter().any(|event| matches!(event, AgentEvent::Rejected { reason, .. } if reason == &crate::endpoints::goes::no_other_account())));
    session.turn("u2", "two").await;
    let agent = session.end().await;
    prefix::assert_gemini_prefix_stable(&[body(&server, 0), body(&server, 1)]);
    assert!(
        agent
            .catalog()
            .plugins
            .iter()
            .find(|plugin| plugin.id == "gemini")
            .unwrap()
            .enabled
    );
}
