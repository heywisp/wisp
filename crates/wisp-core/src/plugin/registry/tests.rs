use std::sync::atomic::{AtomicUsize, Ordering};

use futures::FutureExt;
use wisp_plugin_api::host::CallContext;
use wisp_plugin_api::tools::{ClientTool, Prepared, ToolDef};
use wisp_plugin_api::types::{Display, ToolRun};

use super::*;
use crate::plugin::testing::{self, TestTool, manifest};

struct Fixed {
    defs: Vec<ToolDef>,
}

impl Plugin for Fixed {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        futures::future::ready(self.defs.clone()).boxed()
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(std::future::ready(Err("fixed".to_owned())))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        futures::future::ready(ToolRun::completed()).boxed()
    }
}

fn def(name: &str, schema: &str) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: format!("The {name} tool."),
            input_schema: schema.to_owned(),
            meta: testing::meta(1_000, Some(1_000)),
        }),
        system_line: None,
        context_line: None,
    }
}

fn fixed(id: &str, declared: &[&str], described: Vec<ToolDef>) -> (String, Make) {
    let names: Vec<String> = declared.iter().map(|s| (*s).to_owned()).collect();
    let make: Make = Arc::new(move |_host| {
        Arc::new(Fixed {
            defs: described.clone(),
        }) as Arc<dyn Plugin>
    });
    (manifest(id, &names, &[]), make)
}

fn on(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

fn start(registry: &Registry, names: &[&str]) -> Started {
    registry
        .start(
            &on(names),
            Arc::new(crate::tool::testkit::fake_capture_host()),
        )
        .now_or_never()
        .unwrap()
}

const OBJECT: &str = r#"{"type":"object"}"#;

#[test]
fn a_clash_turns_the_tool_off_in_both_plugins_and_names_them() {
    let (registry, problems) = Registry::from_sources(
        vec![
            fixed(
                "beta",
                &["dup", "own_b"],
                vec![def("dup", OBJECT), def("own_b", OBJECT)],
            ),
            fixed(
                "alpha",
                &["dup", "own_a"],
                vec![def("dup", OBJECT), def("own_a", OBJECT)],
            ),
        ],
        &PluginConfig::default(),
    );
    assert_eq!(
        problems,
        [
            Problem {
                plugin: "alpha".into(),
                message:
                    "its tool `dup` has the same name as a tool of beta; it is turned off in both"
                        .into(),
            },
            Problem {
                plugin: "beta".into(),
                message:
                    "its tool `dup` has the same name as a tool of alpha; it is turned off in both"
                        .into(),
            },
        ]
    );
    let started = start(&registry, &["dup", "own_a", "own_b"]);
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    assert_eq!(started.toolbox.names(), ["own_a", "own_b"]);
    let catalog = registry.catalog(&on(&["dup", "own_a", "own_b"]), &started.problems);
    let dup: Vec<(String, Option<String>)> = catalog
        .plugins
        .iter()
        .flat_map(|p| p.tools.iter().map(move |t| (p.id.clone(), t)))
        .filter(|(_, t)| t.name == "dup")
        .map(|(id, t)| (id, t.unavailable.clone()))
        .collect();
    assert_eq!(
        dup,
        [
            (
                "alpha".to_owned(),
                Some("turned off: beta has a tool with the same name".to_owned())
            ),
            (
                "beta".to_owned(),
                Some("turned off: alpha has a tool with the same name".to_owned())
            ),
        ]
    );
}

#[test]
fn an_off_tool_whose_schema_is_not_json_leaves_its_plugin_out() {
    let (registry, _) = Registry::from_sources(
        vec![
            fixed(
                "sloppy",
                &["used", "spare"],
                vec![def("used", OBJECT), def("spare", "{")],
            ),
            fixed("honest", &["fine"], vec![def("fine", OBJECT)]),
        ],
        &PluginConfig::default(),
    );
    let started = start(&registry, &["used", "fine"]);
    assert_eq!(started.toolbox.names(), ["fine"]);
    assert_eq!(
        started
            .problems
            .iter()
            .map(|p| (
                p.plugin.as_str(),
                p.message
                    .starts_with("the input schema of `spare` is not JSON")
            ))
            .collect::<Vec<_>>(),
        [("sloppy", true)]
    );
}

#[test]
fn describing_a_tool_the_manifest_lacks_refuses_the_plugin() {
    let (registry, problems) = Registry::from_sources(
        vec![
            fixed(
                "liar",
                &["told"],
                vec![def("told", OBJECT), def("extra", OBJECT)],
            ),
            fixed("honest", &["fine"], vec![def("fine", OBJECT)]),
        ],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty());
    let started = start(&registry, &["told", "fine"]);
    assert_eq!(started.toolbox.names(), ["fine"]);
    assert_eq!(
        started.problems,
        [Problem {
            plugin: "liar".into(),
            message: "it describes a tool `extra` that its manifest doesn't declare; the plugin is left out".into(),
        }]
    );
    let recorded: Vec<String> = started
        .toolbox
        .plugins()
        .iter()
        .map(|r| r.id.clone())
        .collect();
    assert_eq!(recorded, ["honest"]);
    let catalog = registry.catalog(&on(&["told", "fine"]), &started.problems);
    let shown: Vec<(String, bool, Vec<String>)> = catalog
        .plugins
        .iter()
        .map(|p| {
            let left_out = p.tools.iter().all(|t| t.unavailable.is_some());
            (p.id.clone(), left_out, p.problems.clone())
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("honest".to_owned(), false, Vec::new()),
            (
                "liar".to_owned(),
                true,
                vec!["it describes a tool `extra` that its manifest doesn't declare; the plugin is left out".to_owned()]
            ),
        ]
    );
    let told = catalog.tool("told").unwrap();
    assert!(!told.on);
    assert_eq!(
        told.unavailable.as_deref(),
        Some("its plugin was left out when it started")
    );
    assert!(catalog.tool("fine").unwrap().on);
}

#[test]
fn two_plugins_with_one_id_are_both_left_out_whatever_their_order() {
    for reversed in [false, true] {
        let mut sources = vec![
            fixed("twin", &["first"], vec![def("first", OBJECT)]),
            fixed("twin", &["second"], vec![def("second", OBJECT)]),
            fixed("other", &["third"], vec![def("third", OBJECT)]),
        ];
        if reversed {
            sources.reverse();
        }
        let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
        assert_eq!(
            problems,
            [Problem {
                plugin: "twin".into(),
                message: "more than one plugin has this id; all of them are left out".into(),
            }]
        );
        assert_eq!(registry.ids(), ["other"]);
        let started = start(&registry, &["first", "second", "third"]);
        assert_eq!(started.toolbox.names(), ["third"]);
    }
}

#[test]
fn two_plugins_continuing_one_core_node_both_lose_it_whatever_their_order() {
    use wisp_plugin_testkit::hooks as kit;
    let section = "\n[[prompt_sections]]\nname = \"env\"\nplace = \"lead\"\ncontinues = \"core/environment\"\n";
    let hook = |name: &str, extra: &str| {
        format!("\n[[hooks]]\nname = \"{name}\"\npoint = \"before-turn\"\n{extra}")
    };
    let continuing = hook("env", "continues = \"core/environment\"\n");
    let kept = hook("kept", "");
    for reversed in [false, true] {
        let make = || kit::instantiate(kit::none(), kit::Calls::default());
        let mut sources = vec![
            (
                kit::manifest_in("0.2.0", "aa", &(section.to_owned() + &kept)),
                make(),
            ),
            (kit::manifest_in("0.2.0", "bb", &continuing), make()),
            (kit::manifest_in("0.2.0", "cc", &kept), make()),
        ];
        if reversed {
            sources.reverse();
        }
        let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
        let clash = |plugin: &str, other: &str| Problem {
            plugin: plugin.to_owned(),
            message: format!(
                "it continues `core/environment`, as {other} does; that node is left out of both"
            ),
        };
        assert_eq!(problems, [clash("aa", "bb"), clash("bb", "aa")]);
        assert_eq!(registry.hook_names(), ["aa/kept", "cc/kept"]);
        assert!(registry.plugins["aa"].manifest.prompt_sections.is_empty());
        assert_eq!(registry.ids(), ["aa", "bb", "cc"]);
    }
}

#[test]
fn a_plugin_asking_for_a_setting_that_is_not_plain_is_left_out() {
    for key in ["key", "store_id"] {
        let settings = vec![key.to_owned()];
        let make: Make = Arc::new(|_host| {
            Arc::new(Fixed {
                defs: vec![def("peek", OBJECT)],
            }) as Arc<dyn Plugin>
        });
        let sources = vec![
            (manifest("peeker", &["peek".to_owned()], &settings), make),
            (
                manifest("timer", &["clock".to_owned()], &["time".to_owned()]),
                fixed("unused", &[], Vec::new()).1,
            ),
        ];
        let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
        assert_eq!(
            problems,
            [Problem {
                plugin: "peeker".into(),
                message: format!(
                    "it asks to read the setting `{key}`, which is not a plain setting; the plugin is left out"
                ),
            }]
        );
        assert_eq!(registry.ids(), ["timer"]);
    }
}

#[test]
fn a_schema_returned_with_unsorted_keys_is_sent_sorted() {
    let unsorted = r#"{"type":"object","required":["b"],"properties":{"b":{"type":"string"},"a":{"type":"integer"}}}"#;
    let (registry, _) = Registry::from_sources(
        vec![fixed("messy", &["messy"], vec![def("messy", unsorted)])],
        &PluginConfig::default(),
    );
    let toolbox = start(&registry, &["messy"]).toolbox;
    assert_eq!(
        toolbox.canonical_json(),
        r#"[{"description":"The messy tool.","input_schema":{"properties":{"a":{"type":"integer"},"b":{"type":"string"}},"required":["b"],"type":"object"},"name":"messy"}]"#
    );
}

struct Panicky;

impl TestTool for Panicky {
    fn name(&self) -> String {
        "panicky".into()
    }

    fn prepare(&self, _input: &str) -> Result<Box<dyn Prepared>, String> {
        std::panic::panic_any("prepare panicked")
    }

    fn rejected_display(&self, _input: &str) -> Option<Display> {
        std::panic::panic_any("rejected_display panicked")
    }

    fn time_limit_ms(&self, _input: &str) -> Option<u64> {
        std::panic::panic_any("time_limit panicked")
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        std::panic::panic_any("run panicked")
    }
}

#[test]
fn a_panic_in_a_sync_plugin_call_is_contained_and_named() {
    let toolbox = testing::toolbox(vec![Arc::new(Panicky)]);
    let input = serde_json::json!({"a": 1});
    let refused = futures::executor::block_on(toolbox.prepare("panicky", &input)).unwrap_err();
    assert_eq!(refused.message, "the test-panicky plugin panicked");
    assert_eq!(
        futures::executor::block_on(toolbox.rejected_display("panicky", &input)),
        crate::tool::fallback_display("panicky", &input)
    );
    let entry = toolbox.get("panicky").unwrap();
    assert_eq!(
        futures::executor::block_on(entry.time_limit(&input)),
        Some(std::time::Duration::from_secs(2))
    );
    assert!(!wisp_contain::is_contained());
}

#[test]
fn a_shuffled_bundle_gives_the_same_tools_system_lines_and_catalog() {
    let all = || {
        wisp_plugins::bundled()
            .into_iter()
            .chain([wisp_plugin_testkit::bundled()])
            .map(|b| (b.manifest.to_owned(), bundled_make(b)))
            .collect::<Vec<_>>()
    };
    let names = [
        "ask_user",
        "echo",
        "list_agents",
        "report",
        "show_diff",
        "sleep",
        "spawn_agent",
        "view_file",
        "write_todos",
    ];
    let mut seen = Vec::new();
    for turn in 0..3 {
        let mut sources = all();
        sources.rotate_left(turn);
        if turn == 2 {
            sources.reverse();
        }
        let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start_exact(
                &on(&names),
                Arc::new(crate::tool::testkit::fake_capture_host()),
            )
            .now_or_never()
            .unwrap();
        let toolbox = started.toolbox;
        seen.push((
            toolbox.canonical_json(),
            toolbox.system_lines(),
            registry.catalog(&on(&names), &[]),
            registry.ids(),
            started.hooks.table.sentences(),
        ));
    }
    assert_eq!(seen[0], seen[1]);
    assert_eq!(seen[0], seen[2]);
    let sentences: Vec<String> = [wisp_plugin_time::MANIFEST, wisp_plugin_context::MANIFEST]
        .into_iter()
        .map(|text| {
            Manifest::parse(text, Origin::Bundled)
                .unwrap()
                .system
                .unwrap()
                .text
        })
        .collect();
    assert_eq!(seen[0].4, sentences);
    let mut ids = crate::tool::testkit::bundled_ids();
    ids.push("testkit");
    ids.sort_unstable();
    assert_eq!(seen[0].3, ids);
    assert_eq!(
        seen[0].1[..2],
        [
            wisp_tool_text::agents::MESSAGES_SENTENCE,
            wisp_tool_text::agents::REPORT_SENTENCE
        ]
    );
    assert_eq!(
        seen[0]
            .1
            .iter()
            .filter(|l| l.as_str() == wisp_tool_text::agents::MESSAGES_SENTENCE)
            .count(),
        1
    );
}

#[test]
fn every_loaded_plugin_is_recorded_with_the_shell() {
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty());
    let ids: Vec<String> = registry.records().into_iter().map(|r| r.id).collect();
    let expected = crate::tool::testkit::bundled_ids();
    assert_eq!(ids, expected);
    let started = start(&registry, &[]);
    assert!(started.toolbox.is_empty());
    assert_eq!(started.toolbox.plugins(), registry.records());
}

fn native(name: &str, family: &str) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        kind: ToolKind::Native(family.to_owned()),
        system_line: None,
        context_line: None,
    }
}

fn with_native(id: &str, tool: &str, family: Option<&str>, described: ToolDef) -> (String, Make) {
    let (mut text, make) = fixed(id, &[tool], vec![described]);
    if let Some(family) = family {
        text.push_str(&format!("native = \"{family}\"\n"));
    }
    (text, make)
}

#[test]
fn a_native_tool_is_requested_only_while_it_is_on() {
    let (registry, problems) = Registry::from_sources(
        vec![
            with_native(
                "searcher",
                "finder",
                Some("web_search"),
                native("finder", "web_search"),
            ),
            fixed("plain", &["fine"], vec![def("fine", OBJECT)]),
        ],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let on_too = start(&registry, &["finder", "fine"]);
    assert!(on_too.problems.is_empty(), "{:?}", on_too.problems);
    assert_eq!(on_too.toolbox.names(), ["fine"]);
    assert_eq!(
        on_too.toolbox.natives(),
        [crate::tool::Native::new("finder", "web_search")]
    );
    assert!(on_too.toolbox.web_search());
    let off = start(&registry, &["fine"]);
    assert!(off.toolbox.natives().is_empty());
    assert!(!off.toolbox.web_search());
    assert_eq!(registry.native_families(&on(&["finder"])), ["web_search"]);
    assert!(registry.native_families(&on(&["fine"])).is_empty());
    assert!(
        registry
            .catalog(&on(&["finder"]), &[])
            .tool("finder")
            .unwrap()
            .native
    );
    assert!(
        !registry
            .catalog(&on(&["finder"]), &[])
            .tool("fine")
            .unwrap()
            .native
    );
}

#[test]
fn a_native_tool_the_description_contradicts_leaves_the_plugin_out() {
    let cases = [
        (
            with_native(
                "aa",
                "finder",
                Some("web_search"),
                native("finder", "web_fetch"),
            ),
            "its manifest says `finder` asks for the native tool `web_search`, but it describes `web_fetch`; the plugin is left out",
        ),
        (
            with_native("aa", "finder", None, native("finder", "web_search")),
            "it describes `finder` as the native tool `web_search`, which its manifest doesn't say; the plugin is left out",
        ),
        (
            with_native("aa", "finder", Some("web_search"), def("finder", OBJECT)),
            "its manifest says `finder` asks for the native tool `web_search`, but it describes a tool Wisp runs; the plugin is left out",
        ),
    ];
    for (source, message) in cases {
        let (registry, problems) = Registry::from_sources(vec![source], &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = start(&registry, &[]);
        assert_eq!(
            started.problems,
            [Problem {
                plugin: "aa".into(),
                message: message.into(),
            }]
        );
        assert!(!started.toolbox.web_search());
    }
}

#[test]
fn a_bundled_draft_native_runner_exists_on_or_off_but_is_never_a_loaded_or_held_tool() {
    for world in ["0.3.0", "0.4.0"] {
        let (text, make) = with_native(
            "searcher",
            "finder",
            Some("web_search"),
            def("finder", OBJECT),
        );
        let text = text.replace("world = \"0.2.0\"", &format!("world = {world:?}"));
        let (registry, problems) =
            Registry::from_sources(vec![(text, make)], &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        for on in [false, true] {
            let started = start(&registry, if on { &["finder"] } else { &[] });
            if world == "0.3.0" {
                assert_eq!(started.problems.len(), 1);
                continue;
            }
            assert!(started.problems.is_empty(), "{:?}", started.problems);
            let mut toolbox = started.toolbox;
            assert!(toolbox.runner("web_search").is_some());
            assert_eq!(toolbox.web_search(), on);
            assert!(
                toolbox.specs().is_empty()
                    && toolbox.names().is_empty()
                    && toolbox.held_names().is_empty()
            );
            assert!(toolbox.definition("finder").is_none());
            assert!(toolbox.has_plugin("searcher"));
            assert_eq!(toolbox.hosts().len(), 1);
            toolbox.drop_plugin("searcher");
            assert!(toolbox.runner("web_search").is_none());
        }
    }
}

#[test]
fn the_native_families_come_from_the_manifests_without_starting_a_plugin() {
    let make: Make = Arc::new(|_host| -> Arc<dyn Plugin> { panic!("started") });
    let (text, _) = with_native(
        "searcher",
        "finder",
        Some("web_search"),
        native("finder", "web_search"),
    );
    let (registry, problems) = Registry::from_sources(vec![(text, make)], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(registry.native_families(&on(&["finder"])), ["web_search"]);
    assert_eq!(registry.default_on(), on(&["finder"]));
}

#[test]
fn the_catalog_lists_plugins_and_their_tools_in_name_order() {
    let (registry, problems) = Registry::from_sources(
        vec![
            fixed(
                "zeta",
                &["zz", "aa"],
                vec![def("zz", OBJECT), def("aa", OBJECT)],
            ),
            fixed("alpha", &["mm"], vec![def("mm", OBJECT)]),
        ],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let catalog = registry.catalog(&on(&["aa"]), &[]);
    let listed: Vec<(String, Vec<String>)> = catalog
        .plugins
        .iter()
        .map(|p| {
            (
                p.id.clone(),
                p.tools.iter().map(|t| t.name.clone()).collect(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("alpha".to_owned(), vec!["mm".to_owned()]),
            ("zeta".to_owned(), vec!["aa".to_owned(), "zz".to_owned()]),
        ]
    );
    assert_eq!(catalog.leader_names(), ["aa", "mm", "zz"]);
    let text = serde_json::to_string(&catalog).unwrap();
    assert!(
        text.find("\"aa\"").unwrap() < text.find("\"zz\"").unwrap(),
        "{text}"
    );
}

#[test]
fn the_tool_context_is_the_one_describe_got_even_when_its_provider_was_left_out() {
    let (left_out, make) = fixed("ghostly", &["ghost"], Vec::new());
    let left_out = left_out.replace(
        "name = \"ghost\"\n",
        "name = \"ghost\"\nprovides = [\"probe\"]\n",
    );
    let (registry, problems) = Registry::from_sources(
        vec![
            (left_out, make),
            fixed("plain", &["plain"], vec![def("plain", OBJECT)]),
        ],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let started = start(&registry, &["ghost", "plain"]);
    assert_eq!(started.problems.len(), 1, "{:?}", started.problems);
    assert_eq!(started.toolbox.names(), ["plain"]);
    assert!(started.toolbox.capabilities().is_empty());
    assert_eq!(
        started.tool_context,
        ToolContext {
            platform: PLATFORM.to_owned(),
            capabilities: vec!["probe".to_owned()],
        }
    );
    let quiet = start(&registry, &["plain"]);
    assert!(quiet.tool_context.capabilities.is_empty());
}

#[test]
fn a_selection_brings_followers_that_come_with_their_leader_and_an_exact_list_starts_only_its_names()
 {
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let agents = [
        "list_agents",
        "rename_agent",
        "send_message",
        "spawn_agent",
        "stop_agent",
    ];
    assert_eq!(registry.on_tools(&on(&["spawn_agent"])), on(&agents));
    assert!(
        registry
            .on_tools(&on(&["report", "list_agents"]))
            .is_empty()
    );
    let defaults = registry.on_tools(&registry.default_on());
    assert!(defaults.is_superset(&on(&agents)), "{defaults:?}");
    assert!(defaults.contains("write_todos"));
    assert!(!defaults.contains("report") && !defaults.contains("show_page"));
    let host = || Arc::new(crate::tool::testkit::fake_capture_host());
    let mut child: Vec<&str> = vec!["report", "show_page", "spawn_agent", "web_search"];
    if cfg!(unix) {
        child.push("bash");
    }
    let started = registry
        .start_exact(&on(&child), host())
        .now_or_never()
        .unwrap();
    let mut client: Vec<&str> = vec!["report", "show_page", "spawn_agent"];
    if cfg!(unix) {
        client.insert(0, "bash");
    }
    assert_eq!(started.toolbox.names(), client);
    assert_eq!(started.toolbox.native_names(), ["web_search"]);
    let on_in_catalog: Vec<&str> = started
        .catalog
        .tools()
        .filter(|t| t.on)
        .map(|t| t.name.as_str())
        .collect();
    let mut expected = child.clone();
    expected.sort_unstable();
    let mut on_sorted = on_in_catalog.clone();
    on_sorted.sort_unstable();
    assert_eq!(on_sorted, expected);
    let shell: Vec<String> = if cfg!(unix) {
        vec!["shell".to_owned()]
    } else {
        vec![]
    };
    assert_eq!(started.toolbox.capabilities(), shell.as_slice());
    assert_eq!(started.tool_context.capabilities, shell);
    let selected = registry
        .start(&on(&["spawn_agent", "report"]), host())
        .now_or_never()
        .unwrap();
    assert_eq!(selected.toolbox.names(), agents);
    assert!(!selected.catalog.tool("report").unwrap().on);
}

#[test]
fn the_testkits_commands_leave_the_tools_and_the_system_prompt_unchanged() {
    let full = wisp_plugin_testkit::MANIFEST;
    let cut = full.find("\n[[commands]]").unwrap();
    let bare = &full[..cut];
    assert!(full.len() > bare.len() && !bare.contains("[[commands]]"));
    let names = ["echo", "sleep"];
    let parts = |manifest: &str| {
        let make: Make = bundled_make(wisp_plugin_testkit::bundled());
        let (registry, problems) =
            Registry::from_sources(vec![(manifest.to_owned(), make)], &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start_exact(
                &on(&names),
                Arc::new(crate::tool::testkit::fake_capture_host()),
            )
            .now_or_never()
            .unwrap();
        let commands = registry.catalog(&on(&names), &[]).plugins[0].commands.len();
        let system = crate::agent::system_prompt(
            Some("Base."),
            &started.toolbox.system_lines(),
            &started.hooks.table.sentences(),
        );
        (started.toolbox.canonical_json(), system, commands)
    };
    let with = parts(full);
    let without = parts(bare);
    assert_eq!((with.2, without.2), (3, 0));
    assert_eq!((&with.0, &with.1), (&without.0, &without.1));
}

#[test]
fn the_catalog_lists_a_plugins_prompt_sections_with_their_places() {
    use wisp_plugin_testkit::hooks as kit;
    use wisp_protocol::plugin::{PromptSectionEntry, SectionPlace};
    let body =
        kit::prompt_section("rules", "system", "") + &kit::prompt_section("notes", "lead", "");
    let make = kit::instantiate(kit::none(), kit::Calls::default());
    let (registry, problems) = Registry::from_sources(
        vec![(kit::manifest_in("0.2.0", "aa", &body), make)],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let catalog = registry.catalog(&BTreeSet::new(), &[]);
    let entry = catalog.plugins.iter().find(|p| p.id == "aa").unwrap();
    assert_eq!(
        entry.prompt_sections,
        [
            PromptSectionEntry {
                name: "rules".into(),
                place: SectionPlace::System
            },
            PromptSectionEntry {
                name: "notes".into(),
                place: SectionPlace::Lead
            },
        ]
    );
}

struct Toolless(Arc<AtomicUsize>);

impl Plugin for Toolless {
    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(std::future::ready({
            self.0.fetch_add(1, Ordering::SeqCst);
            Err("asked".to_owned())
        }))
    }
}

#[test]
fn a_plugin_with_no_tools_is_never_asked_to_prepare() {
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = asked.clone();
    let make: Make = Arc::new(move |_host| Arc::new(Toolless(counter.clone())) as Arc<dyn Plugin>);
    let (registry, problems) = Registry::from_sources(
        vec![
            (manifest("quiet", &[], &[]), make),
            fixed("loud", &["own"], vec![def("own", OBJECT)]),
        ],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let started = start(&registry, &["own"]);
    let input = serde_json::json!({});
    for name in ["quiet", "own", "anything"] {
        let _ = futures::executor::block_on(started.toolbox.prepare(name, &input));
    }
    assert_eq!(
        futures::executor::block_on(started.toolbox.prepare("quiet", &input))
            .unwrap_err()
            .message,
        "there is no tool named `quiet`"
    );
    assert_eq!(asked.load(Ordering::SeqCst), 0);
}

mod switches;

#[test]
fn every_bundled_plugin_reads_registry_settings() {
    let manifests: Vec<_> = wisp_plugins::bundled()
        .into_iter()
        .map(|bundled| Manifest::parse(bundled.manifest, Origin::Bundled).unwrap())
        .collect();
    let mut seen = 0;
    for manifest in &manifests {
        for name in manifest.names() {
            assert!(
                wisp_protocol::settings::find(name).is_some(),
                "{}: {name}",
                manifest.id
            );
            seen += 1;
        }
    }
    for spec in wisp_protocol::settings::SETTINGS {
        if let Some(plugin) = spec.plugin {
            assert!(
                manifests.iter().any(|manifest| manifest.id == plugin
                    && manifest.names().any(|name| name == spec.name)),
                "{plugin}: {}",
                spec.name
            );
        }
    }
    assert!(seen > 0);
}

fn outside(described: ToolDef) -> Vec<Problem> {
    let (manifest, make) = fixed("outside", &[&described.name.clone()], vec![described]);
    let source = Source::wasm(manifest, make, "0".repeat(64));
    let (registry, problems) = Registry::from_origins(vec![source], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    start(&registry, &["told"]).problems
}

fn left_out(message: &str) -> Vec<Problem> {
    vec![Problem {
        plugin: "outside".into(),
        message: message.into(),
    }]
}

#[test]
fn an_outside_system_line_holding_a_path_leaves_the_plugin_out() {
    let mut told = def("told", OBJECT);
    told.system_line = Some("Notes live in the plugin's folder.".to_owned());
    assert!(outside(told.clone()).is_empty());
    told.system_line = Some("Notes live in /srv/notes.".to_owned());
    assert_eq!(
        outside(told),
        left_out(
            "the system line of its tool `told` holds a digit, `/` or `\\`, but it may hold no dates, paths or ids; the plugin is left out"
        )
    );
}

#[test]
fn an_outside_schema_or_description_holding_a_date_path_or_id_leaves_the_plugin_out() {
    let cases = [
        (
            r#"{"type":"object","properties":{"when":{"type":"string","description":"Since 2026-11-03."}}}"#,
            "The told tool.",
            "the `schema.properties.when.description` of its tool `told` holds a date or time",
        ),
        (
            OBJECT,
            "Reads ~/notes.txt at 9:30.",
            "the description of its tool `told` holds a path",
        ),
        (
            OBJECT,
            "Writes to (/srv/out) or [/srv/err].",
            "the description of its tool `told` holds a path",
        ),
        (
            r#"{"type":"object","properties":{"f81d4fae-7dec-11d0-a765-00a0c91e6bf6":{}}}"#,
            "The told tool.",
            "the `schema.properties.f81d4fae-7dec-11d0-a765-00a0c91e6bf6` key of its tool `told` holds an id",
        ),
        (
            r#"{"type":"string","enum":["C:\\Users"]}"#,
            "The told tool.",
            "the `schema.enum` of its tool `told` holds a path",
        ),
    ];
    for (schema, description, said) in cases {
        let mut told = def("told", schema);
        if let ToolKind::Client(client) = &mut told.kind {
            client.description = description.to_owned();
        }
        assert_eq!(
            outside(told),
            left_out(&format!(
                "{said}, which differs across days, machines or agents; the plugin is left out"
            ))
        );
    }
}

#[test]
fn a_schema_url_prose_and_every_bundled_definition_pass_the_outside_checks() {
    let mut told = def(
        "told",
        r#"{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","description":"Read and/or write as /config shows, version 0.2.0, chunk 64 of 1024."}"#,
    );
    told.system_line = Some("Use told for notes.".to_owned());
    assert!(outside(told).is_empty());
    let host = Arc::new(crate::tool::testkit::fake_capture_host());
    let context = BuildContext {
        platform: PLATFORM.to_owned(),
        capabilities: Vec::new(),
    };
    let mut described = 0;
    for bundled in wisp_plugins::bundled() {
        let manifest = Manifest::parse(bundled.manifest, Origin::Bundled).unwrap();
        let plugin = (bundled.instantiate)(Arc::new(PluginHost::new(
            &manifest.id,
            &manifest.world,
            &manifest.names().map(str::to_owned).collect::<Vec<_>>(),
            host.clone(),
        )));
        for def in plugin.describe(&context).now_or_never().unwrap() {
            assert_eq!(outside_says(&def), None, "{}", def.name);
            described += 1;
        }
    }
    assert!(described >= 14, "{described}");
}

#[test]
fn a_plugins_rows_join_the_registry_and_the_catalog_when_it_loads() {
    let bundled = wisp_plugin_testkit::settings();
    let (registry, problems) =
        Registry::from_origins(vec![Source::bundled(bundled)], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let spec = wisp_protocol::settings::find("plugins.testkit.minutes").unwrap();
    assert_eq!(
        spec.default_value(),
        Some(wisp_protocol::settings::SettingValue::Int(10))
    );
    let catalog = registry.catalog(&Default::default(), &[]);
    assert!(
        matches!(&catalog.plugins[0].settings[0], wisp_protocol::plugin::Setting::Row(row) if row.name == "minutes")
    );
    assert_eq!(
        catalog.plugins[0].setting_names().collect::<Vec<_>>(),
        [spec.name]
    );
    let started = registry
        .start(&Default::default(), Arc::new(KernelHost::new()))
        .now_or_never()
        .unwrap();
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    assert!(
        started
            .host
            .owned_settings()
            .contains(&spec.name.to_owned())
    );
    let mut config = PluginConfig::default();
    config.switches.removed.insert("testkit".into());
    let (removed, problems) = Registry::from_origins(vec![Source::bundled(bundled)], &config);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(removed.ids().is_empty());
    assert!(wisp_protocol::settings::find(spec.name).is_some());
}

#[test]
fn a_row_whose_name_is_refused_leaves_its_plugin_out_and_declares_nothing() {
    for bad in ["enabled", "A", "a-b", "max_tokens"] {
        let id = format!("refused-{}", bad.to_ascii_lowercase().replace('_', "-"));
        let body = format!(
            "[[settings]]\nname = \"fine\"\nlabel = \"Fine\"\nhelp = \"A row.\"\nkind = \"bool\"\ndefault = \"on\"\n\n[[settings]]\nname = \"{bad}\"\nlabel = \"Bad\"\nhelp = \"A row.\"\nkind = \"bool\"\ndefault = \"off\"\n"
        );
        let text = wisp_plugin_testkit::hooks::manifest_in("0.3.0", &id, &body);
        let (registry, problems) = Registry::from_sources(
            vec![(text, fixed("unused", &[], Vec::new()).1)],
            &PluginConfig::default(),
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.contains(bad), "{problems:?}");
        assert!(registry.ids().is_empty());
        assert!(wisp_protocol::settings::find(&format!("plugins.{id}.fine")).is_none());
    }
}

#[test]
fn doctor_includes_enabled_index_sources_without_tools() {
    struct Doctor;
    impl Plugin for Doctor {
        fn doctor<'a>(
            &'a self,
            _ctx: &'a BuildContext,
        ) -> BoxFuture<'a, Vec<wisp_plugin_api::tools::DoctorRow>> {
            Box::pin(async {
                vec![wisp_plugin_api::tools::DoctorRow {
                    key: "source".to_owned(),
                    cells: Vec::new(),
                }]
            })
        }
    }
    for enabled in [true, false] {
        let config = PluginConfig {
            switches: PluginSwitches {
                off: if enabled {
                    BTreeSet::new()
                } else {
                    on(&["source"])
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let text = "id = \"source\"\nversion = \"0.1.0\"\nworld = \"0.3.0\"\nsummary = \"A source\"\nindex = true\n";
        let make: Make = Arc::new(|_| Arc::new(Doctor));
        let (registry, problems) = Registry::from_sources(vec![(text.to_owned(), make)], &config);
        assert!(problems.is_empty(), "{problems:?}");
        let rows = registry
            .doctor(&BTreeSet::new(), Arc::new(crate::plugin::KernelHost::new()))
            .now_or_never()
            .unwrap();
        assert_eq!(rows.len(), usize::from(enabled));
    }
}

struct CodeBox;

impl TestTool for CodeBox {
    fn name(&self) -> String {
        "box".to_owned()
    }
    fn prepare(&self, input: &str) -> Result<Box<dyn Prepared>, String> {
        Ok(testing::Call::boxed(
            self.rejected_display(input).unwrap(),
            (),
        ))
    }
    fn rejected_display(&self, input: &str) -> Option<Display> {
        let code = serde_json::from_str::<serde_json::Value>(input).ok()?;
        Some(Display {
            view: Some(wisp_plugin_api::types::View::Code(
                wisp_plugin_api::types::CodeView {
                    language: Some("text".to_owned()),
                    code: code["code"].as_str()?.to_owned(),
                },
            )),
            ..testing::fallback("box", input)
        })
    }
    fn run<'a>(
        &'a self,
        _call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(std::future::ready(ToolRun::completed()))
    }
}

#[test]
fn a_code_view_that_breaks_its_rule_refuses_the_call() {
    let toolbox = testing::toolbox(vec![Arc::new(CodeBox)]);
    for (code, message) in [
        (
            "a\nb\n\u{1b}".to_owned(),
            "code holds a control character (U+001B) on line 3",
        ),
        (
            "x\n".repeat(10_001),
            "code has 10,001 lines; the limit is 10,000 lines",
        ),
        (
            "x".repeat(512 * 1024 + 1),
            "code is 513 KiB; the limit is 512 KiB",
        ),
    ] {
        let input = serde_json::json!({"code": code});
        let refused = futures::executor::block_on(toolbox.prepare("box", &input)).unwrap_err();
        assert_eq!(
            refused.message,
            format!("the test-box plugin's code view: {message}")
        );
        assert_eq!(
            futures::executor::block_on(toolbox.rejected_display("box", &input)),
            crate::tool::fallback_display("box", &input)
        );
    }
    let input = serde_json::json!({"code": "kept\n"});
    let prepared = futures::executor::block_on(toolbox.prepare("box", &input)).unwrap();
    assert!(
        matches!(&prepared.display().view, Some(wisp_protocol::ToolView::Code {code, ..}) if code == "kept\n")
    );
}

#[test]
fn a_tool_held_by_its_manifest_starts_held_and_enabled_when_allowed_and_off_when_not() {
    let (_, make) = fixed(
        "keeper",
        &["kept", "plain"],
        vec![def("kept", OBJECT), def("plain", OBJECT)],
    );
    let text = "id = \"keeper\"\nversion = \"0.1.0\"\nworld = \"0.4.0\"\nsummary = \"A test plugin\"\n\n[[tools]]\nname = \"kept\"\nheld = true\n\n[[tools]]\nname = \"plain\"\n";
    let (registry, problems) =
        Registry::from_sources(vec![(text.to_owned(), make)], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    assert!(registry.starts_held("kept") && !registry.starts_held("plain"));
    let allowed = start(&registry, &["kept", "plain"]);
    assert_eq!(allowed.toolbox.names(), ["plain"]);
    assert_eq!(allowed.toolbox.held_names(), ["kept"]);
    assert!(allowed.toolbox.is_enabled("kept"));
    let left_out = start(&registry, &["plain"]);
    assert_eq!(left_out.toolbox.held_names(), ["kept"]);
    assert!(!left_out.toolbox.is_enabled("kept"));
}

#[test]
fn the_settings_tool_name_rule_matches_the_manifests() {
    for name in [
        "bash",
        "show_diff",
        "rust-code",
        "a",
        "",
        "two words",
        "dotted.name",
        "ünïcode",
        &"x".repeat(64),
        &"x".repeat(65),
    ] {
        assert_eq!(
            wisp_protocol::settings::is_tool_name(name),
            wisp_plugin_api::manifest::is_tool_name(name),
            "{name}"
        );
    }
}

#[test]
fn config_row_names_in_tool_wording_follow_the_registry_labels() {
    use wisp_protocol::settings::{find, names};
    use wisp_tool_text::manage as text;
    for (name, sentences) in [
        (
            names::TURNED_OFF,
            vec![
                text::TURN_ON_WHERE.to_owned(),
                text::stays("item"),
                text::off_in_list("item"),
                text::turned_off("item"),
            ],
        ),
        (
            names::TOOLS,
            vec![
                text::native("item"),
                text::off_in_tools("item"),
                wisp_tool_text::index::SEARCH_OFF.to_owned(),
            ],
        ),
    ] {
        let label = find(name).unwrap().label;
        for sentence in sentences {
            assert!(
                sentence.contains(&format!("/config, {label}"))
                    || sentence.contains(&format!(" in {label};")),
                "{name}: {sentence}"
            );
        }
    }
}
