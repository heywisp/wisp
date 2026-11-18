use std::path::PathBuf;

use futures::FutureExt;
use serde_json::json;

use super::*;
use crate::plugin::testing::{self, TestTool};

struct Dummy {
    name: String,
    schema: Value,
}

impl Dummy {
    fn tool(name: &str, schema: Value) -> ToolEntry {
        testing::entry(Arc::new(Self {
            name: name.into(),
            schema,
        }))
    }
}

impl TestTool for Dummy {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn meta(&self) -> wisp_plugin_api::tools::ToolMeta {
        wisp_plugin_api::tools::ToolMeta {
            parallel_safe: true,
            ..testing::meta(1_000, Some(2_000))
        }
    }

    fn prepare(&self, _input: &str) -> Result<Box<dyn PluginPrepared>, String> {
        Err("dummy".to_owned())
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn PluginPrepared>,
        _ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        async { wisp_plugin_api::types::ToolRun::completed() }.boxed()
    }
}

#[test]
fn tools_registered_out_of_order_serialise_sorted_and_canonical() {
    let zeta = Dummy::tool(
        "zeta",
        json!({"type": "object", "required": ["b"], "properties": {"b": {"type": "string"}, "a": {"type": "integer"}}}),
    );
    let alpha = Dummy::tool("alpha", json!({"type": "object", "properties": {}}));
    let toolbox = Toolbox::new([zeta.clone(), alpha.clone(), zeta]);
    assert_eq!(toolbox.names(), ["alpha", "zeta"]);
    assert_eq!(
        toolbox.canonical_json(),
        r#"[{"description":"The alpha tool.","input_schema":{"properties":{},"type":"object"},"name":"alpha"},{"description":"The zeta tool.","input_schema":{"properties":{"a":{"type":"integer"},"b":{"type":"string"}},"required":["b"],"type":"object"},"name":"zeta"}]"#
    );
    let again = Toolbox::new([
        alpha,
        Dummy::tool(
            "zeta",
            json!({"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "string"}}, "required": ["b"]}),
        ),
    ]);
    assert_eq!(again.canonical_json(), toolbox.canonical_json());
    assert_eq!(again.sha256(), toolbox.sha256());
    assert_eq!(Toolbox::default().sha256(), None);
}

#[test]
fn prepare_answers_unknown_and_turned_off_tools() {
    let mut toolbox = Toolbox::new([Dummy::tool("alpha", json!({}))]);
    assert_eq!(
        futures::executor::block_on(toolbox.prepare("nope", &json!({})))
            .unwrap_err()
            .message,
        "there is no tool named `nope`"
    );
    assert_eq!(
        futures::executor::block_on(toolbox.prepare("alpha", &json!({})))
            .unwrap_err()
            .message,
        "dummy"
    );
    let search = crate::agent::testing::search_tool("web_search_20250305");
    assert_eq!(
        futures::executor::block_on(toolbox.prepare_after("web_search", &json!({}), search))
            .unwrap_err()
            .message,
        "web search didn't run for this request; answer without it"
    );
    assert_eq!(
        futures::executor::block_on(toolbox.prepare_after("web_search", &json!({}), search))
            .unwrap_err()
            .cause,
        InvalidCause::SearchNotRun
    );
    let named = Toolbox::new([Dummy::tool("web_search", json!({}))]);
    assert_eq!(
        futures::executor::block_on(named.prepare_after("web_search", &json!({}), search))
            .unwrap_err(),
        InvalidInput::new("dummy"),
        "a client tool of that name is prepared as itself"
    );
    assert_eq!(
        futures::executor::block_on(toolbox.prepare_after("nope", &json!({}), search))
            .unwrap_err()
            .message,
        "there is no tool named `nope`"
    );
    assert_eq!(
        futures::executor::block_on(toolbox.prepare("web_search", &json!({})))
            .unwrap_err()
            .message,
        "there is no tool named `web_search`"
    );
    toolbox.set_enabled("alpha", false);
    assert_eq!(
        futures::executor::block_on(toolbox.prepare("alpha", &json!({})))
            .unwrap_err()
            .message,
        "this tool is turned off"
    );
    assert_eq!(
        toolbox.names(),
        ["alpha"],
        "a turned-off tool stays declared"
    );
    toolbox.set_enabled("alpha", true);
    assert!(toolbox.is_enabled("alpha"));
}

#[test]
fn the_fallback_display_lists_the_inputs_keys_and_never_a_value() {
    let display = fallback_display("x", &json!({"b": "two\nlines", "a": 1}));
    assert_eq!(display.target, "{a, b}");
    assert_eq!(display.action, ToolAction::Other { name: "x".into() });
    assert_eq!(display.view, None);
    assert_eq!(display.title, None);
    let marker = "marker-5f1c2e";
    let secret = fallback_display("x", &json!({"id": 3, "input": marker, "keys": [marker]}));
    assert_eq!(secret.target, "{id, input, keys}");
    assert!(!serde_json::to_string(&secret).unwrap().contains(marker));
    assert_eq!(fallback_display("x", &json!({})).target, "{}");
    assert_eq!(fallback_display("x", &json!(marker)).target, "{}");
    assert_eq!(fallback_display("x", &json!([marker])).target, "{}");
    let long: serde_json::Map<String, Value> = (0..40)
        .map(|n| (format!("key{n:02}"), json!(marker)))
        .collect();
    let long = fallback_display("x", &Value::Object(long));
    assert_eq!(long.target.chars().count(), FALLBACK_TARGET_CHARS);
    assert!(long.target.ends_with('…'));
    assert!(!long.target.contains(marker));
}

#[test]
fn an_unregistered_tools_refused_call_shows_only_its_keys() {
    let toolbox = Toolbox::new([Dummy::tool("known", json!({"type": "object"}))]);
    let marker = "marker-77ab09";
    let display = futures::executor::block_on(
        toolbox.rejected_display("nope", &json!({"input": marker, "id": 1})),
    );
    assert_eq!(display.target, "{id, input}");
    assert_eq!(
        display.action,
        ToolAction::Other {
            name: "nope".into()
        }
    );
    assert!(!serde_json::to_string(&display).unwrap().contains(marker));
}

#[test]
fn canonical_bytes_of_a_value_input_are_pinned() {
    let input: Value =
        serde_json::from_str(r#"{"z": 1, "a": {"y": [3, {"c": true, "b": null}], "x": "é"}}"#)
            .unwrap();
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        r#"{"a":{"x":"é","y":[3,{"b":null,"c":true}]},"z":1}"#
    );
}

#[test]
fn the_shipped_toolbox_is_empty_in_every_mode() {
    assert!(testkit::bundled(&[]).is_empty());
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools")
}

#[test]
fn view_file_alone_sends_its_golden_no_bash_spec() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    let toolbox = testkit::bundled(&["view_file"]);
    assert_eq!(toolbox.names(), ["view_file"]);
    let path = fixture_dir().join("view_file_without_bash.json");
    let bytes = format!("{}\n", serde_json::to_string(&toolbox.specs()[0]).unwrap());
    if bless {
        std::fs::write(&path, &bytes).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    assert!(!bytes.contains("bash"));
    let again = testkit::bundled(&["view_file"]);
    assert_eq!(
        format!("{}\n", serde_json::to_string(&again.specs()[0]).unwrap()),
        bytes
    );
}

#[test]
fn bash_and_process_without_secret_match_todays_specs() {
    for toolbox in [
        testkit::bundled(&["bash"]),
        testkit::start(
            vec![testkit::shell_source(wisp_plugin_shell::Timing::default())],
            &["bash"],
        ),
    ] {
        assert_eq!(toolbox.names(), ["bash", "process"]);
        for spec in toolbox.specs() {
            let path = fixture_dir().join(format!("{}_without_secret.json", spec.name));
            let bytes = format!("{}\n", serde_json::to_string(spec).unwrap());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                bytes,
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn every_builtin_tool_matches_its_golden_spec() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    let toolbox = testkit::bundled(
        &[
            testkit::ALL,
            &["show_page", "show_code", "manage", "rename_chat"],
        ]
        .concat(),
    );
    assert!(toolbox.names().iter().any(|n| n == "view_file"));
    let held = toolbox.held_specs();
    let held = held.iter().filter(|spec| toolbox.is_enabled(&spec.name));
    assert_eq!(
        held.clone()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>(),
        ["manage", "rename_chat"]
    );
    for spec in toolbox.specs().iter().chain(held) {
        let path = fixture_dir().join(format!("{}.json", spec.name));
        let bytes = format!("{}\n", serde_json::to_string(spec).unwrap());
        if bless {
            std::fs::create_dir_all(fixture_dir()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bytes,
            "{}",
            path.display()
        );
    }
}

#[test]
fn write_todos_stands_alone_adds_no_sentence_and_matches_its_golden_spec() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    let toolbox = testkit::bundled(&["write_todos"]);
    assert_eq!(toolbox.names(), ["write_todos"]);
    assert!(toolbox.system_lines().is_empty());
    let path = fixture_dir().join("write_todos.json");
    let bytes = format!("{}\n", serde_json::to_string(&toolbox.specs()[0]).unwrap());
    if bless {
        std::fs::write(&path, &bytes).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    let again = testkit::bundled(&["write_todos", "show_diff"]);
    let spec = again
        .specs()
        .iter()
        .find(|s| s.name == "write_todos")
        .map(|s| format!("{}\n", serde_json::to_string(s).unwrap()));
    assert_eq!(spec.as_deref(), Some(bytes.as_str()));
}

#[test]
fn spawn_agent_brings_its_followers_but_never_report_and_each_matches_its_golden_spec() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    let names = [
        "list_agents",
        "rename_agent",
        "report",
        "send_message",
        "spawn_agent",
        "stop_agent",
    ];
    let (registry, _) = crate::plugin::Registry::new(
        wisp_plugins::bundled(),
        &crate::plugin::PluginConfig::default(),
    );
    let defaults = registry.on_tools(&registry.default_on());
    for name in [
        "list_agents",
        "rename_agent",
        "send_message",
        "spawn_agent",
        "stop_agent",
        "write_todos",
    ] {
        assert!(defaults.contains(name), "{name}");
    }
    assert!(!defaults.contains("report"));
    assert_eq!(
        testkit::bundled(&["spawn_agent"]).names(),
        [
            "list_agents",
            "rename_agent",
            "send_message",
            "spawn_agent",
            "stop_agent"
        ]
    );
    assert!(testkit::bundled(&["report", "list_agents"]).is_empty());
    let toolbox = testkit::bundled_exact(&names);
    assert_eq!(toolbox.names(), names);
    assert_eq!(
        toolbox.system_lines(),
        [
            wisp_tool_text::agents::MESSAGES_SENTENCE,
            wisp_tool_text::agents::REPORT_SENTENCE
        ]
    );
    for spec in toolbox.specs() {
        let path = fixture_dir().join(format!("{}.json", spec.name));
        let bytes = format!("{}\n", serde_json::to_string(spec).unwrap());
        if bless {
            std::fs::write(&path, &bytes).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bytes,
            "{}",
            spec.name
        );
        let alone = testkit::bundled_exact(&[spec.name.as_str(), "show_diff", "write_todos"]);
        let again = alone.specs().iter().find(|s| s.name == spec.name).unwrap();
        assert_eq!(serde_json::to_string(again).unwrap(), bytes.trim_end());
    }
    assert!(
        testkit::bundled(&["spawn_agent"])
            .get("spawn_agent")
            .is_some_and(|tool| !tool.meta().needs_workspace && !tool.meta().asks_user)
    );
}

#[test]
fn every_test_tool_has_a_sample_its_prepare_accepts() {
    let toolbox = test_toolbox();
    assert!(!toolbox.is_empty());
    for sample in samples() {
        let name = &sample.tool.spec().name;
        assert!(toolbox.get(name).is_some(), "{name}");
        assert!(
            futures::executor::block_on(toolbox.prepare(name, &sample.input)).is_ok(),
            "{name}"
        );
        let spec = serde_json::to_string(sample.tool.spec()).unwrap();
        assert!(!spec.chars().any(|c| c.is_ascii_digit()), "{spec}");
        assert!(!spec.contains('/'), "{spec}");
    }
}

#[test]
fn show_diff_is_the_only_built_in_that_shows_content() {
    let toolbox = testkit::bundled(testkit::ALL);
    let declared: Vec<(String, ToolPurpose)> = toolbox
        .names()
        .into_iter()
        .map(|name| {
            let purpose = toolbox.get(&name).unwrap().meta().purpose;
            (name, purpose)
        })
        .collect();
    let mut expected = vec![
        ("ask_user".to_owned(), ToolPurpose::Run),
        ("list_accounts".to_owned(), ToolPurpose::Run),
        ("list_settings".to_owned(), ToolPurpose::Run),
        ("show_diff".to_owned(), ToolPurpose::Show),
    ];
    if cfg!(unix) {
        expected.extend([
            ("ask_secret".to_owned(), ToolPurpose::Run),
            ("bash".to_owned(), ToolPurpose::Run),
            ("process".to_owned(), ToolPurpose::Run),
            ("view_file".to_owned(), ToolPurpose::Run),
        ]);
    }
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(declared, expected);
}

#[test]
fn a_tool_that_is_not_registered_runs() {
    let toolbox = testkit::bundled(&["show_diff"]);
    assert_eq!(toolbox.purpose("show_diff"), ToolPurpose::Show);
    assert_eq!(toolbox.purpose("nowhere"), ToolPurpose::Run);
    assert_eq!(toolbox.purpose("web_search"), ToolPurpose::Run);
    assert_eq!(Toolbox::default().purpose("show_diff"), ToolPurpose::Run);
}

#[test]
fn a_refused_show_diff_call_keeps_its_own_display_and_an_unknown_tool_falls_back() {
    let toolbox = testkit::bundled(&["show_diff"]);
    let input = json!({"files": [{"path": "a.rs"}]});
    assert!(futures::executor::block_on(toolbox.prepare("show_diff", &input)).is_err());
    let own = futures::executor::block_on(toolbox.rejected_display("show_diff", &input));
    assert_ne!(own, fallback_display("show_diff", &input));
    assert_eq!(
        futures::executor::block_on(toolbox.rejected_display("nowhere", &input)),
        fallback_display("nowhere", &input)
    );
}

#[cfg(unix)]
#[test]
fn view_file_names_bash_only_while_the_shell_is_on() {
    let alone = testkit::bundled(&["view_file"]);
    let with_shell = testkit::bundled(&["bash", "view_file"]);
    assert_eq!(alone.provider_of(SHELL), None);
    assert_eq!(with_shell.provider_of(SHELL), Some("bash"));
    assert_eq!(alone.capabilities(), Vec::<String>::new());
    assert_eq!(with_shell.capabilities(), ["shell"]);
    let described =
        |toolbox: &Toolbox| toolbox.get("view_file").unwrap().spec().description.clone();
    assert!(!described(&alone).contains("bash"));
    assert!(described(&with_shell).contains("bash"));
}

#[test]
fn json_keys_keep_sorted_order_in_the_kernels_feature_graph() {
    let value = json!({"b": 1, "a": 2});
    assert_eq!(value.to_string(), r#"{"a":2,"b":1}"#);
}

fn source_files(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

fn test_only(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or_default();
    relative.split('/').any(|part| {
        part == "tests" || part == "tool_tests" || part == "shell_tests" || part.ends_with("_tests")
    }) || matches!(
        name,
        "tests.rs"
            | "testing.rs"
            | "testkit.rs"
            | "snap.rs"
            | "shell_tests.rs"
            | "tool_tests.rs"
            | "test_guard.rs"
    )
}

#[test]
fn the_kernel_names_no_bundled_tool_outside_its_allow_list() {
    let mut names: Vec<String> = wisp_plugins::bundled()
        .into_iter()
        .chain([wisp_plugin_testkit::bundled()])
        .flat_map(|bundled| {
            wisp_plugin_api::manifest::Manifest::parse(
                bundled.manifest,
                wisp_plugin_api::manifest::Origin::Bundled,
            )
            .unwrap()
            .tools
            .into_iter()
            .map(|tool| tool.name)
        })
        .collect();
    names.push("web_search".to_owned());
    let allowed = [
        ("web_search", "search.rs"),
        ("web_search", "settings.rs"),
        ("web_search", "config.rs"),
        ("web_search", "models.rs"),
        ("confirm", "tool.rs"),
        ("echo", "provider/openai/realtime.rs"),
        ("search", "provider/foreign.rs"),
        ("search", "provider/gemini/request.rs"),
    ];
    let allowed_lines = [
        (
            "agents.rs",
            "\"report\" => return Err(PresetProblem::ReportRemoved)".to_owned(),
        ),
        (
            "plugin/host/manage.rs",
            "pub(crate) const SEARCH_PLUGIN: &str = \"search\";".to_owned(),
        ),
    ];
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    source_files(&root, &mut files);
    let mut found = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if test_only(&relative) {
            continue;
        }
        let text = allowed_lines
            .iter()
            .filter(|(path, _)| *path == relative)
            .fold(std::fs::read_to_string(&file).unwrap(), |text, (_, key)| {
                assert_eq!(text.matches(key.as_str()).count(), 1, "{relative}: {key}");
                text.replace(key.as_str(), "")
            });
        for name in &names {
            let literal = format!("\"{name}\"");
            if text.contains(&literal) && !allowed.contains(&(name.as_str(), relative.as_str())) {
                found.push(format!("{relative}: {literal}"));
            }
        }
    }
    assert!(found.is_empty(), "{found:#?}");
    assert!(
        !cfg!(unix) || names.iter().any(|n| n == "bash") && names.iter().any(|n| n == "process")
    );
}
