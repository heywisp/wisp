#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail on an unexpected parse"
)]

use wisp_plugin_api::manifest::{Manifest, Origin, Problem, PromptSection, Retry, SectionPlace};

mod support;

use support::{place, position, problem, refused_at};

const WORKSPACE: &str = r#"id = "workspace"
version = "0.1.0"
world = "0.2.0"
summary = "Tells the model where it works"

[[prompt_sections]]
name = "environment"
place = "lead"
in = ["core/state"]
after = []
before = []
continues = "core/environment"
"#;

const SHELL: &str = r#"id = "shell"
version = "0.1.0"
world = "0.2.0"
summary = "Runs commands"

[[hooks]]
name = "processes"
point = "before-turn"
in = ["core/news"]
retry = "replay"
job_news = true
continues = "core/processes"
"#;

const HEAD: &str = "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"0.2.0\"\nsummary = \"Acme\"\n";

fn bundled(text: &str) -> Result<Manifest, Problem> {
    Manifest::parse(text, Origin::Bundled)
}

fn section(keys: &str) -> String {
    format!("{HEAD}\n[[prompt_sections]]\nname = \"notes\"\nplace = \"lead\"\n{keys}")
}

fn hook(point: &str, keys: &str) -> String {
    format!(
        "{HEAD}\n[[points]]\nname = \"ready\"\nkind = \"observe\"\nversion = \"1\"\n\n[[hooks]]\nname = \"h\"\npoint = \"{point}\"\n{keys}"
    )
}

#[test]
fn the_plans_section_and_replay_hook_parse_as_bundled_plugins() {
    let manifest = bundled(WORKSPACE).unwrap();
    assert_eq!(
        manifest.prompt_sections,
        [PromptSection {
            name: "environment".to_owned(),
            place: SectionPlace::Lead,
            sets: vec!["core/state".to_owned()],
            after: Vec::new(),
            before: Vec::new(),
            continues: Some("core/environment".to_owned()),
        }]
    );
    let manifest = bundled(SHELL).unwrap();
    let hook = &manifest.hooks[0];
    assert_eq!(hook.sets, ["core/news"]);
    assert_eq!(hook.retry, Retry::Replay);
    assert!(hook.job_news);
    assert_eq!(hook.continues.as_deref(), Some("core/processes"));
    assert!(manifest.prompt_sections.is_empty());
}

#[test]
fn a_section_needs_only_a_name_and_a_place_and_a_hook_keeps_its_defaults() {
    let manifest = bundled(&section("")).unwrap();
    let only = &manifest.prompt_sections[0];
    assert_eq!(only.place, SectionPlace::Lead);
    assert!(only.sets.is_empty() && only.after.is_empty() && only.before.is_empty());
    assert_eq!(only.continues, None);
    let system = bundled(&section("").replace("\"lead\"", "\"system\"")).unwrap();
    assert_eq!(system.prompt_sections[0].place, SectionPlace::System);
    let missing = problem(
        &section("").replace("place = \"lead\"\n", ""),
        Origin::Bundled,
    );
    assert!(
        missing.message.contains("missing field `place`"),
        "{missing}"
    );
    assert!(missing.line.is_some());
    let manifest = bundled(&hook("before-turn", "")).unwrap();
    let hook = &manifest.hooks[0];
    assert!(hook.sets.is_empty() && !hook.job_news && hook.continues.is_none());
    assert_eq!(hook.retry, Retry::Rerun);
}

#[test]
fn each_new_key_is_refused_under_world_0_1_0_where_it_is_written() {
    let old = |text: &str| text.replace("world = \"0.2.0\"", "world = \"0.1.0\"");
    let section = old(WORKSPACE);
    for origin in [Origin::Bundled, Origin::External] {
        let problem = problem(&section, origin);
        assert!(
            problem
                .message
                .starts_with("unknown field `prompt_sections`"),
            "{problem}"
        );
        assert_eq!(
            (problem.line, problem.column),
            place(&section, "prompt_sections")
        );
    }
    for key in ["in", "retry", "job_news", "continues"] {
        let kept: String = SHELL
            .lines()
            .filter(|line| {
                !["in", "retry", "job_news", "continues"]
                    .iter()
                    .any(|other| *other != key && line.starts_with(&format!("{other} = ")))
            })
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(bundled(&kept).is_ok(), "{key}: {kept}");
        let text = old(&kept);
        let problem = problem(&text, Origin::Bundled);
        assert!(
            problem
                .message
                .starts_with(&format!("unknown field `{key}`")),
            "{key}: {problem}"
        );
        let offset = text.find(&format!("\n{key} = ")).unwrap() + 1;
        assert_eq!(
            (problem.line, problem.column),
            position(&text, offset),
            "{key}"
        );
    }
}

#[test]
fn a_0_1_0_plugin_reads_the_same_with_no_sections_and_default_hook_keys() {
    let old = "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"0.1.0\"\nsummary = \"Acme\"\n\n[[hooks]]\nname = \"fill\"\npoint = \"before-turn\"\nafter = [\"time/sent\"]\nbefore = [\"core/processes\"]\nreserve = \"Context: 99%\"\n";
    let before = Manifest::parse(old, Origin::External).unwrap();
    assert!(before.prompt_sections.is_empty());
    let hook = &before.hooks[0];
    assert!(hook.sets.is_empty() && !hook.job_news && hook.continues.is_none());
    assert_eq!(hook.retry, Retry::Rerun);
    assert_eq!(hook.before, ["core/processes"]);
    let after = Manifest::parse(
        &old.replace("world = \"0.1.0\"", "world = \"0.2.0\""),
        Origin::External,
    )
    .unwrap();
    assert_eq!(
        Manifest {
            world: "0.1.0".to_owned(),
            ..after
        },
        before
    );
}

#[test]
fn a_place_this_wisp_lacks_is_refused_where_it_is_written() {
    for other in ["system-prompt", "Lead", "head", ""] {
        let text = section("").replace("place = \"lead\"", &format!("place = \"{other}\""));
        refused_at(
            &text,
            Origin::Bundled,
            &format!("\"{other}\"\n"),
            &format!("a prompt section's place is `system` or `lead`, not `{other}`"),
        );
    }
}

#[test]
fn section_names_follow_the_hook_grammar_are_unique_and_never_a_hooks() {
    for bad in ["Notes", "1notes", "-notes", "no_tes", "no tes", ""] {
        let text = section("").replace("name = \"notes\"", &format!("name = \"{bad}\""));
        refused_at(
            &text,
            Origin::Bundled,
            &format!("\"{bad}\"\nplace"),
            &format!(
                "prompt section name `{bad}` must be 1 to 32 characters of a-z, 0-9 and -, starting with a letter"
            ),
        );
    }
    let twice = format!(
        "{}\n[[prompt_sections]]\nname = \"notes\" \nplace = \"system\"\n",
        section("")
    );
    refused_at(
        &twice,
        Origin::Bundled,
        "\"notes\" \n",
        "prompt section `notes` is declared twice",
    );
    let clash = format!(
        "{}\n[[hooks]]\nname = \"notes\"\npoint = \"before-turn\"\n",
        section("")
    );
    refused_at(
        &clash,
        Origin::Bundled,
        "\"notes\"\nplace",
        "prompt section `notes` has the name of a hook of this plugin; sections and hooks share one set of names",
    );
    let apart = format!(
        "{}\n[[hooks]]\nname = \"fill\"\npoint = \"before-turn\"\n",
        section("")
    );
    assert!(bundled(&apart).is_ok());
}

#[test]
fn in_names_one_of_the_three_core_sets() {
    for bad in [
        "core/tools",
        "core/typed",
        "core/stat",
        "core/environment",
        "acme/mine",
        "state",
    ] {
        let list = format!("in = [\"core/state\", \"{bad}\"]\n");
        refused_at(
            &section(&list),
            Origin::Bundled,
            &format!("\"{bad}\""),
            &format!(
                "prompt section `notes` joins `{bad}`, which isn't a set; the sets are `core/state`, `core/notes` and `core/news`"
            ),
        );
        refused_at(
            &hook("before-turn", &list),
            Origin::Bundled,
            &format!("\"{bad}\""),
            &format!(
                "hook `h` joins `{bad}`, which isn't a set; the sets are `core/state`, `core/notes` and `core/news`"
            ),
        );
    }
    for set in ["core/state", "core/notes", "core/news"] {
        let one = format!("in = [\"{set}\", \"{set}\"]\n");
        assert!(bundled(&section(&one)).is_ok(), "{set}");
        assert!(bundled(&hook("before-turn", &one)).is_ok(), "{set}");
    }
}

#[test]
fn in_names_at_most_one_set_since_the_sets_run_in_a_chain() {
    for (first, second) in [
        ("core/state", "core/notes"),
        ("core/notes", "core/news"),
        ("core/news", "core/state"),
    ] {
        let list = format!("in = [\"{first}\", \"{first}\", \"{second}\"]\n");
        for (text, what) in [
            (section(&list), "prompt section `notes`"),
            (hook("before-turn", &list), "hook `h`"),
        ] {
            refused_at(
                &text,
                Origin::Bundled,
                &format!("\"{second}\""),
                &format!(
                    "{what} joins `{second}` as well as `{first}`; a node joins one set, since the core's sets run in a chain"
                ),
            );
        }
    }
}

#[test]
fn an_unknown_key_in_a_0_2_0_hook_or_prompt_section_is_refused_where_it_is_written() {
    for text in [
        hook("before-turn", "job_nwes = true\n"),
        section("contiues = \"core/environment\"\n"),
    ] {
        let key = if text.contains("job_nwes") {
            "job_nwes"
        } else {
            "contiues"
        };
        let problem = problem(&text, Origin::Bundled);
        assert!(
            problem
                .message
                .starts_with(&format!("unknown field `{key}`")),
            "{problem}"
        );
        assert_eq!((problem.line, problem.column), place(&text, key));
    }
}

#[test]
fn job_news_is_for_bundled_plugins_only() {
    let shell = SHELL
        .replace("id = \"shell\"", "id = \"acme\"")
        .replace("continues = \"core/processes\"\n", "");
    assert!(bundled(&shell).is_ok());
    refused_at(
        &shell,
        Origin::External,
        "true",
        "hook `processes` declares job_news, which only a bundled plugin may do",
    );
    let off = shell.replace("job_news = true", "job_news = false");
    assert!(Manifest::parse(&off, Origin::External).is_ok());
}

#[test]
fn set_keys_retry_and_job_news_are_for_before_turn_hooks_only() {
    let keys = [
        ("in", "in = [\"core/notes\"]\n", "\"core/notes\""),
        ("retry", "retry = \"rerun\"\n", "\"rerun\""),
        ("job_news", "job_news = false\n", "false"),
        (
            "continues",
            "continues = \"core/processes\"\n",
            "\"core/processes\"",
        ),
    ];
    for point in [
        "after-turn",
        "compaction-head",
        "before-request",
        "acme/ready",
        "other/point",
    ] {
        for (key, line, needle) in keys {
            let text = hook(point, line);
            let offset = text.find(line).unwrap() + line.find(needle).unwrap();
            let problem = problem(&text, Origin::Bundled);
            assert_eq!(
                problem.message,
                format!("hook `h` declares {key} at `{point}`; only before-turn takes it"),
                "{point} {key}"
            );
            assert_eq!(
                (problem.line, problem.column),
                position(&text, offset),
                "{point} {key}"
            );
        }
    }
    for (_, line, _) in keys {
        assert!(bundled(&hook("before-turn", line)).is_ok(), "{line}");
    }
}

#[test]
fn continues_names_only_the_two_core_nodes() {
    for bad in [
        "core/state",
        "core/notes",
        "core/news",
        "core/tools",
        "core/typed",
        "core/instructions",
        "time/sent",
        "acme/notes",
        "environment",
        "",
    ] {
        let line = format!("continues = \"{bad}\"\n");
        for (text, what) in [
            (section(&line), "prompt section `notes`"),
            (hook("before-turn", &line), "hook `h`"),
        ] {
            for origin in [Origin::Bundled, Origin::External] {
                refused_at(
                    &text,
                    origin,
                    &format!("\"{bad}\"\n"),
                    &format!(
                        "{what} continues `{bad}`; only `core/environment` and `core/processes` can be continued"
                    ),
                );
            }
        }
    }
    for good in ["core/environment", "core/processes"] {
        let line = format!("continues = \"{good}\"\n");
        assert!(bundled(&section(&line)).is_ok(), "{good}");
        assert!(bundled(&hook("before-turn", &line)).is_ok(), "{good}");
    }
}

#[test]
fn the_registry_refuses_continues_from_a_plugin_that_isnt_bundled() {
    let external = WORKSPACE.replace("id = \"workspace\"", "id = \"acme\"");
    assert!(bundled(&external).is_ok());
    refused_at(
        &external,
        Origin::External,
        "\"core/environment\"",
        "prompt section `environment` continues `core/environment`, which only a bundled plugin may do",
    );
    let shell = SHELL.replace("id = \"shell\"", "id = \"acme\"");
    refused_at(
        &shell,
        Origin::External,
        "\"core/processes\"",
        "hook `processes` continues `core/processes`, which only a bundled plugin may do",
    );
    let without = external.replace("continues = \"core/environment\"\n", "");
    assert!(Manifest::parse(&without, Origin::External).is_ok());
}

#[test]
fn a_core_node_is_continued_once() {
    let text = format!(
        "{}\n[[hooks]]\nname = \"h\"\npoint = \"before-turn\"\ncontinues = \"core/environment\" \n",
        section("continues = \"core/environment\"\n")
    );
    refused_at(
        &text,
        Origin::Bundled,
        "\"core/environment\" \n",
        "`core/environment` is continued twice",
    );
}
