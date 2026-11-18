use std::path::Path;

use wisp_protocol::agents::Preset;
use wisp_protocol::settings::{SettingValue, SitePath, find};
use wisp_protocol::{Effort, settings};

use super::*;
use crate::config::SavedSettings;
use crate::endpoints::{ConfigFile, PathShown};
use crate::settings::Given;
use crate::test_guard::{catalog_names, parse};

fn presets(text: &str) -> Presets {
    parse(text).unwrap().presets
}

fn off(text: &str) -> Vec<String> {
    presets(text)
        .off()
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn checked_off(text: &str) -> Vec<String> {
    presets(text)
        .checked(None, &catalog_names(), &PresetPlace::default())
        .off()
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn view() -> endpoints::EndpointView {
    use crate::agents::testing::connection;
    endpoints::EndpointView::new(
        &[
            connection("env", None),
            connection("small", Some(&["claude-haiku-4-5"])),
        ],
        &[("gpt".into(), "no key in /opt/keys/gpt.env".into())],
        None,
    )
}

fn names(presets: &Presets) -> Vec<&str> {
    presets.usable().iter().map(|p| p.name.as_str()).collect()
}

const REVIEWER: &str = "[agents.presets.reviewer]\ndescription = \"Reviews a change it did not write\"\neffort = \"high\"\ntools_off = [\"show_diff\"]\n";

fn current() -> Current {
    Current {
        endpoint: None,
        model: "claude-opus-5-5".into(),
    }
}

fn limits(saved: Option<&SavedSettings>, flags: Vec<Given>) -> [Limit; 4] {
    Limits::explained(
        &Layers {
            flags,
            saved,
            ..Layers::default()
        },
        &current(),
    )
}

#[test]
fn each_limit_defaults_to_the_registry_default() {
    let explained = limits(None, Vec::new());
    assert_eq!(
        explained.clone().map(|l| (l.name, l.value, l.source)),
        [
            (AGENTS_MAX_RUNNING, Some(4), "default"),
            (AGENTS_MAX_TREE_RUNNING, None, "default"),
            (AGENTS_MAX_AGENTS, Some(50), "default"),
            (AGENTS_MAX_DEPTH, Some(2), "default"),
        ]
    );
    for limit in explained {
        let spec = find(limit.name).unwrap();
        assert_eq!(
            spec.default_value(),
            Some(limit.value.map_or(SettingValue::Text("none".into()), |n| {
                SettingValue::Int(i64::from(n))
            }))
        );
    }
    let worker = wisp_protocol::agents::PRESET_FIELDS.map(|f| f.help);
    let named: Vec<&str> = [wisp_tool_text::agents::QUEUED_SENTENCE]
        .iter()
        .chain(&worker)
        .flat_map(|text| text.split(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_'))
        .map(|word| word.trim_end_matches('.'))
        .filter(|word| word.starts_with("agents."))
        .collect();
    assert_eq!(named.len(), 4, "{named:?}");
    for name in named {
        assert!(find(name).is_some(), "{name} is no setting");
    }
}

#[test]
fn each_limit_reads_the_file_and_the_flags() {
    let file =
        parse("[agents]\nmax_running = 2\nmax_tree_running = 6\nmax_agents = 10\nmax_depth = 3\n")
            .unwrap();
    let flag = Given {
        target: settings::Target::Name(find(AGENTS_MAX_DEPTH).unwrap()),
        value: SettingValue::Int(1),
        origin: "--set agents.max_depth=1".into(),
    };
    let explained = limits(Some(&file.settings), vec![flag]);
    assert_eq!(
        explained.map(|l| (l.name, l.value, l.source, l.line)),
        [
            (AGENTS_MAX_RUNNING, Some(2), "config", Some(2)),
            (AGENTS_MAX_TREE_RUNNING, Some(6), "config", Some(3)),
            (AGENTS_MAX_AGENTS, Some(10), "config", Some(4)),
            (AGENTS_MAX_DEPTH, Some(1), "flag", None),
        ]
    );
    assert!(file.presets.is_empty());
    let dotted = parse("agents.max_running = 3\n").unwrap();
    assert_eq!(
        dotted.settings.global(AGENTS_MAX_RUNNING),
        Some(&SettingValue::Int(3))
    );
}

#[test]
fn each_limit_takes_none_from_the_file_and_the_flags() {
    let file = parse(
        "[agents]\nmax_tree_running = \"none\"\nmax_agents = \"none\"\nmax_depth = \"none\"\n",
    )
    .unwrap();
    let flag = Given {
        target: settings::Target::Name(find(AGENTS_MAX_RUNNING).unwrap()),
        value: find(AGENTS_MAX_RUNNING).unwrap().parse("none").unwrap(),
        origin: "--set agents.max_running=none".into(),
    };
    let layers = Layers {
        flags: vec![flag],
        saved: Some(&file.settings),
        ..Layers::default()
    };
    assert_eq!(
        Limits::explained(&layers, &current()).map(|l| (l.name, l.value, l.source, l.line)),
        [
            (AGENTS_MAX_RUNNING, None, "flag", None),
            (AGENTS_MAX_TREE_RUNNING, None, "config", Some(2)),
            (AGENTS_MAX_AGENTS, None, "config", Some(3)),
            (AGENTS_MAX_DEPTH, None, "config", Some(4)),
        ]
    );
    assert_eq!(
        Limits::resolve(&layers, &current()),
        Limits {
            max_running: None,
            max_tree_running: None,
            max_agents: None,
            max_depth: None,
            ..Limits::default()
        }
    );
}

#[test]
fn each_limit_refuses_a_value_out_of_range_or_of_the_wrong_type() {
    for (text, error) in [
        (
            "[agents]\nmax_running = 0\n",
            "/h/config.toml line 2: agents.max_running must be a whole number from 1 to 16, or \"none\"",
        ),
        (
            "[agents]\nmax_running = 17\n",
            "/h/config.toml line 2: agents.max_running must be a whole number from 1 to 16, or \"none\"",
        ),
        (
            "[agents]\nmax_agents = 51\n",
            "/h/config.toml line 2: agents.max_agents must be a whole number from 1 to 50, or \"none\"",
        ),
        (
            "[agents]\nmax_depth = 0\n",
            "/h/config.toml line 2: agents.max_depth must be a whole number from 1 to 5, or \"none\"",
        ),
        (
            "[agents]\nmax_depth = \"lots\"\n",
            "/h/config.toml line 2: agents.max_depth must be a whole number from 1 to 5, or \"none\"",
        ),
        (
            "[agents]\nmax_depth = \"2\"\n",
            "/h/config.toml line 2: agents.max_depth must be a whole number from 1 to 5, or \"none\"",
        ),
        (
            "[agents]\nmax_runing = 2\n",
            "/h/config.toml line 2: unknown field \"max_runing\" in [agents]; did you mean max_running?",
        ),
        (
            "agents = 3\n",
            "/h/config.toml line 1: agents must be a table",
        ),
        (
            "[agents]\npresets = 3\n",
            "/h/config.toml line 2: agents.presets must be a table",
        ),
        (
            "[agents.reviewer]\ntime = \"off\"\n",
            "/h/config.toml line 1: [agents.reviewer] is planned with agent types; a preset goes in [agents.presets.reviewer]",
        ),
    ] {
        assert_eq!(parse(text).unwrap_err(), error, "{text}");
    }
    for (name, rule) in [
        (
            AGENTS_MAX_RUNNING,
            "must be a whole number from 1 to 16, or none",
        ),
        (
            AGENTS_MAX_AGENTS,
            "must be a whole number from 1 to 50, or none",
        ),
        (
            AGENTS_MAX_DEPTH,
            "must be a whole number from 1 to 5, or none",
        ),
    ] {
        let spec = find(name).unwrap();
        for bad in ["0", "-1", "many", "2.5", "99"] {
            assert_eq!(spec.parse(bad).unwrap_err().rule, rule, "{name} {bad}");
        }
        assert_eq!(spec.parse("1"), Ok(SettingValue::Int(1)));
    }
}

#[test]
fn a_preset_takes_every_key() {
    let text = "[agents.presets.coder]\ndescription = \"  Writes the code  \"\nmodel = \"claude-sonnet-5-5\"\neffort = \"medium\"\ntools_off = [\"web_search\", \"ask_user\", \"web_search\"]\npreload = [\"none\"]\nspawn = false\ninstructions = \" roles/coder \"\nendpoint = \"work\"\nfallback = []\nworker = false\n";
    let all = presets(text);
    for key in preset_keys() {
        assert!(text.contains(&format!("\n{key} = ")), "{key}");
    }
    assert!(all.off().is_empty(), "{:?}", all.off());
    assert_eq!(
        all.usable().into_iter().find(|p| p.name == "coder"),
        Some(&Preset {
            name: "coder".into(),
            description: "Writes the code".into(),
            model: Some("claude-sonnet-5-5".into()),
            effort: Some(Effort::Medium),
            preload: Some(vec!["none".to_owned()]),
            tools_off: vec!["ask_user".into(), "web_search".into()],
            spawn: Some(false),
            instructions: Some("roles/coder".into()),
            endpoint: Some("work".into()),
            fallback: Vec::new(),
            worker: Some(false),
        })
    );
    let bare = presets("[agents.presets.helper]\ndescription = \"Helps\"\n");
    assert_eq!(
        bare.usable(),
        [&Preset {
            name: "helper".into(),
            description: "Helps".into(),
            model: None,
            effort: None,
            preload: None,
            tools_off: Vec::new(),
            spawn: None,
            instructions: None,
            endpoint: None,
            fallback: Vec::new(),
            worker: None,
        }]
    );
    let later = presets(
        "[agents.presets.todo]\ndescription = \"Plans\"\ntools_off = [\"write_todos\", \"stop_agent\"]\n",
    );
    assert_eq!(
        later
            .usable()
            .into_iter()
            .find(|p| p.name == "todo")
            .unwrap()
            .tools_off,
        ["stop_agent", "write_todos"]
    );
}

#[test]
fn a_bad_preset_turns_off_only_itself() {
    let text = format!(
        "{REVIEWER}\n[agents.presets.broken]\ndescription = \"Broken\"\neffort = \"extreme\"\n\n[agents.presets.planner]\ndescription = \"Plans\"\n\n[agents]\nmax_depth = 3\n"
    );
    let file = parse(&text).unwrap();
    assert_eq!(names(&file.presets), ["planner", "reviewer"]);
    assert_eq!(
        off(&text),
        ["preset broken (line 6) is off: effort must be low, medium, high, xhigh or max"]
    );
    assert_eq!(
        file.settings.global(AGENTS_MAX_DEPTH),
        Some(&SettingValue::Int(3))
    );
}

#[test]
fn the_presets_list_is_the_same_in_any_order() {
    let a = "[agents.presets.zeta]\ndescription = \"Z\"\n\n[agents.presets.alpha]\ndescription = \"A\"\n\n[agents.presets.bad]\nprompt = \"x\"\n\n[agents.presets.mid]\ndescription = \"M\"\n";
    let b = "[agents.presets.mid]\ndescription = \"M\"\n\n[agents.presets.bad]\nprompt = \"x\"\n\n[agents.presets.alpha]\ndescription = \"A\"\n\n[agents.presets.zeta]\ndescription = \"Z\"\n";
    let inline = "[agents]\npresets = { zeta = { description = \"Z\" }, mid = { description = \"M\" }, alpha = { description = \"A\" } }\n";
    let (a, b, inline) = (presets(a), presets(b), presets(inline));
    assert_eq!(names(&a), ["alpha", "mid", "zeta"]);
    assert_eq!(a.usable(), b.usable());
    assert_eq!(a.usable(), inline.usable());
    assert_eq!(a.off().len(), 1);
    assert_eq!(b.off().len(), 1);
    assert_eq!(a.off()[0].problem, b.off()[0].problem);
}

#[test]
fn an_unknown_tool_names_the_preset_and_the_key() {
    assert!(off(&REVIEWER.replace("show_diff", "shwo_diff")).is_empty());
    assert_eq!(
        checked_off(&REVIEWER.replace("show_diff", "shwo_diff")),
        [
            "preset reviewer (line 1) is off: tools_off names shwo_diff, which is no tool; did you mean show_diff?"
        ]
    );
    assert_eq!(
        checked_off("[agents.presets.x]\ndescription = \"X\"\ntools_off = [\"telepathy\"]\n"),
        [
            "preset x (line 1) is off: tools_off names telepathy, which is no tool; tools: ask_secret, ask_user, bash, list_accounts, list_settings, manage, rename_chat, search, show_code, show_diff, show_page, stop_agent, view_file, web_search, write_todos"
        ]
    );
    assert_eq!(
        checked_off("[agents.presets.x]\ndescription = \"X\"\ntools_off = [\"reprot\"]\n"),
        [
            "preset x (line 1) is off: tools_off names reprot, which is no tool; tools: ask_secret, ask_user, bash, list_accounts, list_settings, manage, rename_chat, search, show_code, show_diff, show_page, stop_agent, view_file, web_search, write_todos"
        ]
    );
    for canary in [
        "sk-ant-CANARY0123456789abcdef",
        &"ghp_canary012345CANARYcdef".to_ascii_lowercase(),
    ] {
        let shown = checked_off(&format!(
            "[agents.presets.x]\ndescription = \"X\"\ntools_off = [\"{canary}\"]\n"
        ));
        assert_eq!(
            shown,
            [
                "preset x (line 1) is off: tools_off names something that is no tool; tools: ask_secret, ask_user, bash, list_accounts, list_settings, manage, rename_chat, search, show_code, show_diff, show_page, stop_agent, view_file, web_search, write_todos"
            ]
        );
    }
}

#[test]
fn tools_off_refuses_the_agent_tools_and_points_to_spawn() {
    assert_eq!(
        off("[agents.presets.x]\ndescription = \"X\"\ntools_off = [\"spawn_agent\"]\n"),
        [
            "preset x (line 1) is off: tools_off can't name spawn_agent; use spawn = false, which also turns off stop_agent"
        ]
    );
    for kept in ["send_message", "list_agents", "report"] {
        assert_eq!(
            off(&format!(
                "[agents.presets.x]\ndescription = \"X\"\ntools_off = [\"{kept}\"]\n"
            )),
            [format!(
                "preset x (line 1) is off: tools_off can't name {kept}: every child keeps list_agents, report and send_message; use spawn = false to stop it spawning"
            )]
        );
    }
}

#[test]
fn each_preset_key_names_its_own_error() {
    let unknown = format!("unknown key prompt; allowed: {}", preset_keys().join(", "));
    for (body, problem) in [
        ("effort = \"high\"", "a preset needs a description"),
        ("description = \"   \"", "a preset needs a description"),
        (
            "description = \"a\\nb\"",
            "description must be one line of text",
        ),
        ("description = 3", "description must be a string"),
        (
            "description = \"X\"\npreload = true",
            "preload: tool list must be a list of tool names",
        ),
        (
            "description = \"X\"\npreload = [\"none\", \"bash\"]",
            "preload: must be core, none, or a list of tool names; none must stand alone",
        ),
        (
            "description = \"X\"\nprompt = \"Be nice\"",
            unknown.as_str(),
        ),
        (
            "description = \"X\"\nefort = \"high\"",
            "unknown key efort; did you mean effort?",
        ),
        (
            "description = \"X\"\nmodel = \"a b\"",
            "model must be a model ID (letters, digits and . _ : @ / -)",
        ),
        ("description = \"X\"\nmodel = 5", "model must be a string"),
        (
            "description = \"X\"\nreport = \"review\"",
            "report was removed: reports are free text now; shape them in the preset's instructions",
        ),
        (
            "description = \"X\"\ninstructions = \"  \"",
            "instructions must name a file or a directory holding AGENTS.md",
        ),
        (
            "description = \"X\"\ninstructions = 3",
            "instructions must be a string",
        ),
        (
            "description = \"X\"\nspawn = \"no\"",
            "spawn must be true or false",
        ),
        (
            "description = \"X\"\nworker = \"no\"",
            "worker must be true or false",
        ),
        (
            "description = \"X\"\ntools_off = \"bash\"",
            "tools_off must be a list of tool names",
        ),
        (
            "description = \"X\"\ntools_off = [1]",
            "tools_off must be a list of tool names",
        ),
    ] {
        assert_eq!(
            off(&format!("[agents.presets.x]\n{body}\n")),
            [format!("preset x (line 1) is off: {problem}")],
            "{body}"
        );
    }
    assert_eq!(
        off("[agents.presets]\nx = 3\n"),
        ["preset x (line 2) is off: a preset must be a table"]
    );
    assert_eq!(
        off("[agents.presets.Bad]\ndescription = \"X\"\n"),
        [
            "preset (not a preset name, not shown) (line 1) is off: a preset's name must be a lowercase letter, then up to 31 of a-z, 0-9, _ and -"
        ]
    );
    let canary = "sk-ant-CANARY0123456789abcdef";
    let text = format!("[agents.presets.x]\ndescription = \"X\"\n\"{canary}\" = 1\n");
    let shown = off(&text).join("\n");
    assert!(!shown.contains("CANARY"), "{shown}");
}

#[test]
fn a_preset_naming_a_tool_the_catalog_lacks_turns_off_only_itself() {
    let text = "[agents.presets.good]\ndescription = \"G\"\ntools_off = [\"write_todos\", \"web_search\", \"stop_agent\"]\n\n[agents.presets.gone]\ndescription = \"B\"\ntools_off = [\"telepathy\"]\n\n[agents.presets.follower]\ndescription = \"F\"\ntools_off = [\"process\"]\n";
    for text in [
        text.to_owned(),
        text.replace("tools_off =", "preload =").replace(
            "[\"write_todos\", \"web_search\", \"stop_agent\"]",
            "[\"spawn_agent\"]",
        ),
    ] {
        let checked = presets(&text).checked(None, &catalog_names(), &PresetPlace::default());
        let kept: Vec<&str> = checked.usable().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(kept, ["good"]);
        let off: Vec<&str> = checked
            .off()
            .iter()
            .filter_map(|o| o.name.as_deref())
            .collect();
        assert_eq!(off, ["follower", "gone"]);
        assert!(
            checked
                .off()
                .iter()
                .all(
                    |problem| problem.to_string().contains(if text.contains("preload") {
                        "preload"
                    } else {
                        "tools_off"
                    })
                )
        );
    }
}

#[test]
fn a_preset_model_and_effort_are_checked_against_the_capability_table() {
    let text = "[agents.presets.fast]\ndescription = \"F\"\nmodel = \"claude-haiku-4-5\"\neffort = \"high\"\n\n[agents.presets.ghost]\ndescription = \"G\"\nmodel = \"claude-ghost-1\"\n\n[agents.presets.deep]\ndescription = \"D\"\nmodel = \"claude-opus-5-5\"\neffort = \"max\"\n\n[agents.presets.plain]\ndescription = \"P\"\neffort = \"xhigh\"\n";
    let checked = presets(text).checked(Some(&view()), &catalog_names(), &PresetPlace::default());
    let kept: Vec<&str> = checked.usable().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(kept, ["deep", "plain"]);
    let shown: Vec<String> = checked.off().iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "preset fast (line 1) is off: model claude-haiku-4-5 doesn't take effort high",
            "preset ghost (line 6) is off: model claude-ghost-1 isn't in the capability table",
        ]
    );
}

#[test]
fn a_saved_limit_keeps_its_path() {
    let file = parse("[agents]\nmax_agents = 20\n").unwrap();
    let path = SitePath::global(find(AGENTS_MAX_AGENTS).unwrap());
    assert_eq!(path.to_string(), AGENTS_MAX_AGENTS);
    assert_eq!(path.segments(), ["agents", "max_agents"]);
    assert_eq!(file.settings.get(&path).map(|s| s.line), Some(2));
}

#[test]
fn checked_presets_turn_each_disabled_preset_into_one_startup_warning() {
    let text = "[agents.presets.good]\ndescription = \"G\"\n\n[agents.presets.bad]\ndescription = \"B\"\nhammer = 1\n";
    let checked = presets(text).checked(None, &catalog_names(), &PresetPlace::default());
    assert_eq!(checked.usable().len(), 1);
    assert!(checked.find("good").is_some());
    assert!(checked.find("bad").is_none());
    let warnings = checked.warnings(true, None);
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].starts_with("preset bad (line 4) is off: unknown key hammer"),
        "{}",
        warnings[0]
    );
}

fn home(name: &str) -> crate::test_guard::ScratchDir {
    let home = crate::test_guard::ScratchDir::new("wisp-preset-files-", name);
    home.dir("conf");
    home
}

fn write(path: &Path, text: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn checked_in(
    home: &crate::test_guard::ScratchDir,
    text: &str,
    warn_tokens: u64,
) -> CheckedPresets {
    let config = home.root.join("conf/config.toml");
    let file = ConfigFile::parse(&config, PathShown::Default, text).unwrap();
    let place = PresetPlace::of_config(Some(&config), Some(home.root.clone()), warn_tokens);
    file.presets.checked(None, &catalog_names(), &place)
}

#[test]
fn preset_instructions_resolve_against_the_config_files_directory_home_or_an_absolute_path() {
    let home = home("resolve");
    write(&home.root.join("conf/roles/planner/AGENTS.md"), b"Plan.\n");
    write(&home.root.join("conf/coder.md"), b"Code.\n");
    write(&home.root.join("roles/reviewer.md"), b"Review.\n");
    write(&home.root.join("abs/AGENTS.md"), b"Abs.\n");
    let absolute = home.root.join("abs");
    assert!(!Path::new("roles/planner").exists());
    let text = format!(
        "[agents.presets.planner]\ndescription = \"P\"\ninstructions = \"roles/planner\"\n\n[agents.presets.coder]\ndescription = \"C\"\ninstructions = \"coder.md\"\n\n[agents.presets.reviewer]\ndescription = \"R\"\ninstructions = \"~/roles/reviewer.md\"\n\n[agents.presets.abs]\ndescription = \"A\"\ninstructions = \"{}\"\n",
        absolute.display()
    );
    let checked = checked_in(&home, &text, 8_000);
    assert!(checked.off().is_empty(), "{:?}", checked.off());
    let path = |name: &str| checked.instructions(name).unwrap().path.clone();
    assert_eq!(
        path("planner"),
        home.root.join("conf/roles/planner/AGENTS.md")
    );
    assert_eq!(path("coder"), home.root.join("conf/coder.md"));
    assert_eq!(path("reviewer"), home.root.join("roles/reviewer.md"));
    assert_eq!(path("abs"), home.root.join("abs/AGENTS.md"));
    assert_eq!(checked.instructions("planner").unwrap().files, 1);
    assert!(checked.warnings(true, None).is_empty());
}

#[test]
fn each_instructions_problem_turns_off_only_its_preset_in_the_presets_words() {
    let home = home("problems");
    let conf = home.root.join("conf");
    write(&conf.join("good.md"), b"Good.\n");
    std::fs::create_dir_all(conf.join("bare")).unwrap();
    write(&conf.join("latin.md"), &[0xff, 0xfe, b'x']);
    write(&conf.join("empty.md"), b"");
    write(&conf.join("imports.md"), b"Top.\n@gone.md\n");
    write(&conf.join("deep.md"), b"0\n@d1.md\n");
    for n in 1..=5 {
        write(
            &conf.join(format!("d{n}.md")),
            format!("{n}\n@d{}.md\n", n + 1).as_bytes(),
        );
    }
    let text = "[agents.presets.good]\ndescription = \"G\"\ninstructions = \"good.md\"\n\n[agents.presets.missing]\ndescription = \"M\"\ninstructions = \"nowhere.md\"\n\n[agents.presets.bare]\ndescription = \"B\"\ninstructions = \"bare\"\n\n[agents.presets.latin]\ndescription = \"L\"\ninstructions = \"latin.md\"\n\n[agents.presets.empty]\ndescription = \"E\"\ninstructions = \"empty.md\"\n\n[agents.presets.imports]\ndescription = \"I\"\ninstructions = \"imports.md\"\n\n[agents.presets.deep]\ndescription = \"D\"\ninstructions = \"deep.md\"\n";
    let checked = checked_in(&home, text, 8_000);
    let kept: Vec<&str> = checked.usable().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(kept, ["good"]);
    let shown: Vec<String> = checked.off().iter().map(ToString::to_string).collect();
    let short = "~/conf";
    assert_eq!(
        shown,
        [
            format!("preset bare (line 9) is off: instructions {short}/bare has no AGENTS.md"),
            format!(
                "preset deep (line 25) is off: instructions import {short}/d5.md, imported by d4.md, is more than 4 imports deep"
            ),
            format!("preset empty (line 17) is off: instructions {short}/empty.md hold no text"),
            format!(
                "preset imports (line 21) is off: instructions import {short}/gone.md, imported by imports.md, doesn't exist"
            ),
            format!(
                "preset latin (line 13) is off: instructions {short}/latin.md isn't valid UTF-8"
            ),
            format!(
                "preset missing (line 5) is off: instructions {short}/nowhere.md doesn't exist"
            ),
        ]
    );
}

#[test]
fn large_preset_instructions_warn_naming_the_preset_and_keep_it() {
    let home = home("large");
    write(
        &home.root.join("conf/big.md"),
        "word ".repeat(400).as_bytes(),
    );
    let checked = checked_in(
        &home,
        "[agents.presets.big]\ndescription = \"B\"\ninstructions = \"big.md\"\n",
        100,
    );
    assert!(checked.find("big").is_some());
    assert!(checked.warnings(true, None).is_empty());
    assert_eq!(
        checked.warnings(true, Some("big")),
        ["preset big's instructions are large: about 500 tokens"]
    );
}

#[test]
fn a_key_shaped_instructions_path_is_never_shown() {
    let home = home("key");
    let canary = "sk-ant-api03-CANARY0123456789abcdef";
    let checked = checked_in(
        &home,
        &format!("[agents.presets.k]\ndescription = \"K\"\ninstructions = \"{canary}\"\n"),
        8_000,
    );
    let shown = checked.warnings(true, None).join("\n");
    assert!(!shown.contains("CANARY"), "{shown}");
    assert!(shown.contains("(looks like a key, not shown)"), "{shown}");
}

#[test]
fn a_relative_config_path_gives_an_absolute_directory_to_resolve_preset_files_from() {
    let here = std::env::current_dir().unwrap();
    let bare = PresetPlace::of_config(Some(Path::new("pilot.toml")), None, 8_000);
    assert_eq!(bare.config_dir.as_deref(), Some(here.as_path()));
    let nested = PresetPlace::of_config(Some(Path::new("conf/pilot.toml")), None, 8_000);
    assert_eq!(nested.config_dir, Some(here.join("conf")));
    assert_eq!(PresetPlace::of_config(None, None, 0).config_dir, None);
}

#[test]
fn list_agents_writes_token_counts_as_the_meter_does() {
    for n in [
        0, 999, 1_000, 312_000, 999_499, 999_500, 1_000_000, 1_200_000,
    ] {
        assert_eq!(
            wisp_tool_text::agents::tokens(n),
            wisp_protocol::context::format_tokens(n),
            "{n}"
        );
    }
}

#[test]
fn keep_warm_is_a_switch_like_the_others_true_in_the_file_and_on_on_the_command_line() {
    assert_eq!(
        find(KEEP_WARM).unwrap().kind,
        find("debug_cache").unwrap().kind
    );
    let file = parse("keep_warm = true\n").unwrap();
    let saved = Layers {
        saved: Some(&file.settings),
        ..Layers::default()
    };
    assert!(keep_warm_main(&saved, &current()));
    let flag = Layers {
        flags: vec![Given {
            target: settings::Target::Name(find(KEEP_WARM).unwrap()),
            value: find(KEEP_WARM).unwrap().parse("on").unwrap(),
            origin: "--set keep_warm=on".into(),
        }],
        ..Layers::default()
    };
    assert!(keep_warm_main(&flag, &current()));
    assert!(!keep_warm_main(&Layers::default(), &current()));
    assert_eq!(
        parse("keep_warm = \"on\"\n").err().as_deref(),
        Some("/h/config.toml line 1: keep_warm must be true or false")
    );
}

#[test]
fn missing_preset_endpoints_are_deferred_while_unrelated_validation_stays_checked() {
    let text = "[agents.presets.lost]\ndescription = \"L\"\nendpoint = \"nowhere\"\n\n[agents.presets.keyed]\ndescription = \"K\"\nendpoint = \"abcdefghij0123456789xyz\"\n\n[agents.presets.reviewer]\ndescription = \"R\"\nendpoint = \"gpt\"\nmodel = \"gpt-6-sol\"\n\n[agents.presets.narrow]\ndescription = \"N\"\nendpoint = \"small\"\nmodel = \"claude-opus-5-5\"\n\n[agents.presets.fits]\ndescription = \"F\"\nendpoint = \"small\"\nmodel = \"claude-haiku-4-5\"\n\n[agents.presets.slow]\ndescription = \"S\"\nendpoint = \"small\"\nmodel = \"claude-haiku-4-5\"\neffort = \"high\"\n\n[agents.presets.any]\ndescription = \"A\"\nmodel = \"claude-opus-5-5\"\n\n[agents.presets.ghost]\ndescription = \"G\"\nmodel = \"claude-ghost-1\"\n\n[agents.presets.bad]\ndescription = \"B\"\nendpoint = \"Not A Name\"\n";
    let checked = presets(text).checked(Some(&view()), &catalog_names(), &PresetPlace::default());
    let kept: Vec<&str> = checked.usable().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(kept, ["any", "fits", "keyed", "lost", "narrow", "reviewer"]);
    assert_eq!(
        checked.note("reviewer"),
        Some(&EndpointNote::Unavailable(
            "no key in /opt/keys/gpt.env".into()
        ))
    );
    assert_eq!(checked.note("fits"), None);
    let warnings = checked.warnings(true, None);
    assert_eq!(
        warnings,
        [
            &format!(
                "preset bad (line 38) is off: endpoint must be an account's name ({})",
                wisp_protocol::endpoint_name_rule()
            ),
            "preset ghost (line 34) is off: model claude-ghost-1 isn't in the capability table",
            "preset slow (line 24) is off: model claude-haiku-4-5 doesn't take effort high",
            "preset reviewer's account gpt is unavailable: no key in /opt/keys/gpt.env; its spawns are refused until the account becomes available",
        ]
    );
    assert!(!warnings.join("\n").contains("abcdefghij"), "{warnings:?}");
    assert_eq!(checked.warnings(false, Some("slow")), warnings[2..3]);
    assert_eq!(checked.warnings(false, Some("reviewer")), warnings[3..]);
    assert!(checked.warnings(false, None).is_empty());
    let unchecked = presets(text).checked(None, &catalog_names(), &PresetPlace::default());
    let kept: Vec<&str> = unchecked.usable().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        kept,
        [
            "any", "fits", "ghost", "keyed", "lost", "narrow", "reviewer", "slow"
        ]
    );
    assert_eq!(unchecked.note("lost"), Some(&EndpointNote::NotChecked));
    assert_eq!(unchecked.note("any"), Some(&EndpointNote::NotChecked));
}

#[test]
fn a_fallback_list_is_read_and_checked_like_the_presets_own_choice() {
    let text = "[agents.presets.review]\ndescription = 'Reviews'\nendpoint = 'small'\nmodel = 'claude-haiku-4-5'\nfallback = [{endpoint = 'env', model = 'claude-opus-5-5', effort = 'high'}, {endpoint = 'later'}, {endpoint = 'gpt'}, {endpoint = 'small', model = 'claude-opus-5-5'}]\n";
    let checked = presets(text).checked(Some(&view()), &catalog_names(), &PresetPlace::default());
    assert!(checked.off().is_empty(), "{:?}", checked.off());
    let preset = checked.find("review").unwrap();
    assert_eq!(preset.fallback.len(), 4);
    let unavailable = presets("[agents.presets.review]\ndescription = 'Reviews'\nendpoint = 'gpt'\nfallback = [{endpoint = 'env'}]\n").checked(Some(&view()), &catalog_names(), &PresetPlace::default());
    assert!(unavailable.warnings(true, None).is_empty());
    assert_eq!(
        preset.fallback[0],
        wisp_protocol::agents::Choice {
            endpoint: "env".into(),
            model: Some("claude-opus-5-5".into()),
            effort: Some(Effort::High)
        }
    );
    assert_eq!(
        checked.fallback_notes("review"),
        [
            None,
            Some(EndpointNote::NotSetUp),
            Some(EndpointNote::Unavailable(
                "no key in /opt/keys/gpt.env".into()
            )),
            Some(EndpointNote::NotChecked)
        ]
    );
    for (entry, message) in [
        (
            "[{model='claude-opus-5-5'}]",
            "fallback 1: endpoint is required",
        ),
        (
            "[{endpoint='env', effort='huge'}]",
            "fallback 1: effort must be",
        ),
        (
            "[{endpoint='env', model='not a model'}]",
            "fallback 1: model must be a model ID",
        ),
        ("[{endpoint='bad name'}]", "fallback 1: endpoint must be"),
        (
            "[{endpoint='env', mystery=1}]",
            "fallback 1: unknown key mystery; allowed: endpoint, model, effort",
        ),
        (
            "[{endpoint='env'}, {endpoint='small', model='gpt-9'}]",
            "fallback 2: model gpt-9 isn't one account small serves",
        ),
        (
            "[{endpoint='small', model='claude-haiku-4-5', effort='high'}]",
            "fallback 1: model claude-haiku-4-5 doesn't take effort high",
        ),
        ("[3]", "fallback 1: fallback entry must be a table"),
        ("'env'", "fallback must be a list of tables"),
    ] {
        let text =
            format!("[agents.presets.review]\ndescription = 'Reviews'\nfallback = {entry}\n");
        let checked =
            presets(&text).checked(Some(&view()), &catalog_names(), &PresetPlace::default());
        assert_eq!(checked.off().len(), 1, "{entry}");
        assert!(
            checked.off()[0].to_string().contains(message),
            "{}",
            checked.off()[0]
        );
    }
}
