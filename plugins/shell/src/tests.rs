use std::sync::Arc;

use futures::executor::block_on;
use serde_json::{Value, json};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::host::{Captured, Host};
use wisp_plugin_api::tools::{BuildContext, ToolDef};
use wisp_plugin_api::types::{Failure, Outcome};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};
use wisp_tool_text::jobs::no_process;
use wisp_tool_text::output::invalid_input_text;

use super::*;

mod display;
mod notice;
mod parse;

const VERSION: &str = "GNU bash, version 5.2.21(1)-release (x86_64-pc-linux-gnu)\nCopyright";

fn host(captured: Option<Captured>, bash: Option<&str>) -> Arc<dyn Host> {
    Arc::new(FakeHost {
        captured,
        programs: bash
            .map(|path| vec![(BASH.to_owned(), path.to_owned())])
            .unwrap_or_default(),
        ..FakeHost::default()
    })
}

fn context(capabilities: &[&str]) -> BuildContext {
    BuildContext {
        platform: "unix".to_owned(),
        capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
    }
}

fn described_with(captured: Option<Captured>, capabilities: &[&str]) -> Vec<ToolDef> {
    let shell = Shell::new(host(captured, Some("/usr/bin/bash")));
    block_on(shell.describe(&context(capabilities)))
}

fn line_of(defs: &[ToolDef]) -> Option<String> {
    defs.iter()
        .find(|def| def.name == BASH)
        .and_then(|def| def.context_line.clone())
}

#[test]
fn the_version_line_names_gnu_bash_by_major_and_minor() {
    assert_eq!(parse_version(VERSION), Some("GNU bash 5.2".into()));
    assert_eq!(parse_version("zsh 5.9"), None);
    assert_eq!(parse_version(""), None);
}

#[test]
fn the_context_line_comes_from_capture() {
    let line = |captured: Captured| line_of(&described_with(Some(captured), &[SHELL]));
    assert_eq!(
        line(Captured::Output(VERSION.as_bytes().to_vec())).as_deref(),
        Some("Shell for the bash tool: GNU bash 5.2")
    );
    assert_eq!(
        line(Captured::NotFound).as_deref(),
        Some("Shell for the bash tool: not found on PATH")
    );
    assert_eq!(
        line(Captured::Failed("it exited with code 1".to_owned())).as_deref(),
        Some("Shell for the bash tool: bash (version unknown)")
    );
    assert_eq!(
        line(Captured::Output(b"zsh 5.9\n".to_vec())).as_deref(),
        Some("Shell for the bash tool: bash (version unknown)")
    );
    let off = described_with(Some(Captured::Output(VERSION.as_bytes().to_vec())), &[]);
    assert_eq!(line_of(&off), None);
}

#[test]
fn both_tools_are_described_with_the_system_line_on_bash_only() {
    let defs = described_with(None, &[SHELL]);
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, [BASH, PROCESS]);
    assert_eq!(defs[0].system_line.as_deref(), Some(SYSTEM_LINE));
    assert_eq!(defs[1].system_line, None);
    assert_eq!(defs[1].context_line, None);
}

#[test]
fn the_shell_names_the_secret_tool_only_while_its_capability_is_on() {
    use wisp_plugin_api::tools::ToolKind;
    use wisp_tool_text::question::{SECRET_CAPABILITY, SECRET_TOOL};

    for secret in [false, true] {
        let capabilities = if secret {
            vec![SHELL, SECRET_CAPABILITY]
        } else {
            vec![SHELL]
        };
        let expected = if secret {
            format!(
                "The terminal has echo off, so it is probably asking for a password. Don't type secrets yourself: ask the user for it with {SECRET_TOOL}, which types it into this process."
            )
        } else {
            "The terminal has echo off, so it is probably asking for a password. Don't type secrets yourself: ask the user to type it (process with ask_user_to_attach).".to_owned()
        };
        assert_eq!(hidden_line(secret), expected);
        let defs = described_with(None, &capabilities);
        let texts: Vec<_> = defs
            .iter()
            .map(|def| {
                let ToolKind::Client(tool) = &def.kind else {
                    panic!("not a client tool")
                };
                tool
            })
            .collect();
        let schema: Value = serde_json::from_str(&texts[1].input_schema).unwrap();
        for text in [
            texts[0].description.as_str(),
            texts[1].description.as_str(),
            schema["properties"]["ask_user_to_attach"]["description"]
                .as_str()
                .unwrap(),
        ] {
            assert_eq!(text.contains(SECRET_TOOL), secret, "{text}");
        }
    }
}

#[test]
fn the_doctor_row_names_bash_with_its_path_and_version() {
    let rows = |captured: Captured, path: Option<&str>| {
        let shell = Shell::new(host(Some(captured), path));
        block_on(shell.doctor(&context(&[])))
    };
    let found = rows(
        Captured::Output(VERSION.as_bytes().to_vec()),
        Some("/usr/bin/bash"),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].key, BASH);
    assert_eq!(
        found[0].cells,
        [DoctorCell::Text("/usr/bin/bash (GNU bash 5.2)".to_owned())]
    );
    let missing = rows(Captured::NotFound, None);
    assert_eq!(
        missing[0].cells,
        [DoctorCell::Text("not found on PATH".to_owned())]
    );
}

#[test]
fn the_script_sets_a_quoted_exit_trap_before_the_command() {
    assert_eq!(
        script("cd sub", Some("/tmp/it's here/0001-bash.cwd")),
        "trap '{ builtin test -f '\\''/tmp/it'\\''\\'\\'''\\''s here/0001-bash.cwd'\\'' && builtin pwd -P >| '\\''/tmp/it'\\''\\'\\'''\\''s here/0001-bash.cwd'\\''; } 2>/dev/null' EXIT\ncd sub"
    );
    assert_eq!(script("cd sub", None), "cd sub");
}

#[test]
fn parsing_clamps_and_defaults_the_time_limit() {
    let input = |v: Value| bash::parse(&v).unwrap();
    assert_eq!(
        input(json!({"command": "ls"})).timeout_ms,
        DEFAULT_TIMEOUT_MS
    );
    assert_eq!(
        input(json!({"command": "ls", "timeout_ms": null})).timeout_ms,
        DEFAULT_TIMEOUT_MS
    );
    let high = input(json!({"command": "ls", "timeout_ms": 900_000}));
    assert_eq!(high.limit_ms, Some(900_000));
    let none = input(json!({"command": "ls", "timeout_ms": 0}));
    assert_eq!(none.limit_ms, None);
    let huge = input(json!({"command": "ls", "timeout_ms": u64::MAX}));
    assert_eq!(huge.limit_ms, Some(u64::MAX));
    let fine = input(json!({"command": "ls", "timeout_ms": 5000, "description": "list"}));
    assert_eq!(fine.limit_ms, Some(5000));
    let background = input(json!({"command": "ls", "run_in_background": true}));
    assert_eq!(background.limit_ms, None);
}

#[test]
fn keep_running_parses_as_a_flag_and_other_names_stay_unknown() {
    let input = |v: Value| bash::parse(&v);
    assert!(!input(json!({"command": "ls"})).unwrap().keep_running);
    let kept = input(json!({"command": "ls", "run_in_background": true, "keep_running": true}));
    assert!(kept.unwrap().keep_running);
    assert!(
        !input(json!({"command": "ls", "keep_running": null}))
            .unwrap()
            .keep_running
    );
    assert_eq!(
        input(json!({"command": "ls", "keep_running": "yes"})),
        Err("`keep_running` must be true or false".to_owned())
    );
    for name in ["detach", "keep", "keepRunning"] {
        assert_eq!(
            input(json!({"command": "ls", name: true})),
            Err(format!("unknown field `{name}`"))
        );
    }
    let schema = bash::schema();
    let fields: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
    assert_eq!(
        fields,
        [
            "command",
            "description",
            "keep_running",
            "run_in_background",
            "timeout_ms",
            "tty"
        ]
    );
}

fn run(shell: &Shell, tool: &str, input: Value) -> wisp_plugin_api::types::ToolRun {
    let prepared = block_on(shell.prepare(tool, &input.to_string())).unwrap();
    let call = FakeCall::default();
    block_on(shell.run(prepared, &call))
}

#[test]
fn a_failed_spawn_and_a_missing_bash_are_failures() {
    let shell = Shell::new(host(None, Some("/no/such/bash")));
    block_on(shell.describe(&context(&[SHELL])));
    let ran = run(&shell, BASH, json!({"command": "true"}));
    assert!(
        matches!(&ran.outcome, Outcome::Failed(Failure { message, before_start: true }) if message.starts_with("could not start bash: ")),
        "{:?}",
        ran.outcome
    );
    let missing = Shell::new(host(None, None));
    block_on(missing.describe(&context(&[SHELL])));
    let ran = run(&missing, BASH, json!({"command": "true", "timeout_ms": 0}));
    assert_eq!(
        ran.outcome,
        Outcome::Failed(Failure {
            message: "bash was not found on PATH".into(),
            before_start: true,
        })
    );
    assert_eq!(ran.trailer, None);
}

#[test]
fn process_without_jobs_lists_none_and_refuses_unknown_ids() {
    let shell = Shell::new(host(None, None));
    let listed = run(&shell, PROCESS, json!({}));
    assert_eq!(
        listed.result.as_deref(),
        Some("No processes have been started in this session.")
    );
    let missing = run(&shell, PROCESS, json!({"id": 3}));
    assert_eq!(missing.outcome, Outcome::InvalidInput(no_process(3)));
    assert_eq!(missing.result, Some(invalid_input_text(&no_process(3))));
}

#[test]
fn time_limits_and_moving_follow_the_input() {
    let shell = Shell::with_timing(
        host(None, None),
        Timing {
            start_quiet_ms: 100,
            start_max_ms: 700,
            hand_back_ms: 900,
            yield_ms: 120_000,
        },
    );
    let limit = |tool: &str, input: Value| block_on(shell.time_limit_ms(tool, &input.to_string()));
    assert_eq!(
        limit(BASH, json!({"command": "ls"})),
        Some(DEFAULT_TIMEOUT_MS)
    );
    assert_eq!(
        limit(BASH, json!({"command": "ls", "run_in_background": true})),
        Some(700)
    );
    assert_eq!(limit(BASH, json!({"command": "ls", "timeout_ms": 0})), None);
    assert_eq!(limit(BASH, json!({})), Some(MAX_TIMEOUT_MS));
    assert!(block_on(
        shell.movable(BASH, &json!({"command": "ls"}).to_string())
    ));
    assert!(!block_on(shell.movable(
        BASH,
        &json!({"command": "ls", "run_in_background": true}).to_string()
    )));
    assert!(!block_on(
        shell.movable(PROCESS, &json!({"id": 1}).to_string())
    ));
    assert_eq!(
        block_on(shell.interrupted_result(
            PROCESS,
            &json!({"id": 2, "ask_user_to_attach": "login"}).to_string()
        ))
        .as_deref(),
        Some("Not answered: Wisp stopped while the user was asked to type into process 2.")
    );
    assert_eq!(
        block_on(shell.interrupted_result(BASH, &json!({"command": "ls"}).to_string())),
        None
    );
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("wisp-shell-unit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, suffix: &str) -> std::path::PathBuf {
        self.0.join(format!("call.{suffix}"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn large_scripts_use_a_private_file_and_clean_up_on_spawn_failure() {
    let dir = TempDir::new("script");
    let call = FakeCall {
        private_dir: Some(dir.0.clone()),
        ..FakeCall::default()
    };
    let near = "x".repeat(bash::INLINE_SCRIPT_BYTES - 16);
    let (built, file) = bash::prepare_script(&near, None, &call);
    assert!(file.is_none());
    assert_eq!(built.len(), near.len());
    assert!(!dir.file("sh").exists());

    let (built, file) = bash::prepare_script(&near, Some("/tmp/a.cwd"), &call);
    let file = file.unwrap();
    assert_eq!(std::fs::read_to_string(file.path()).unwrap(), near);
    assert!(built.starts_with("trap "));
    assert!(built.ends_with(&bash::file_bootstrap(file.path())));
    drop(file);
    assert!(!dir.file("sh").exists());

    let small = "x".repeat(bash::INLINE_SCRIPT_BYTES);
    let (built, file) = bash::prepare_script(&small, None, &call);
    assert!(file.is_none());
    assert_eq!(built.len(), bash::INLINE_SCRIPT_BYTES);
    assert!(!dir.file("sh").exists());

    let nul = format!("{}\0tail", "x".repeat(bash::INLINE_SCRIPT_BYTES + 1));
    let (built, file) = bash::prepare_script(&nul, None, &call);
    assert!(file.is_none());
    assert_eq!(built, nul);
    assert!(!dir.file("sh").exists());

    let large = "y".repeat(bash::INLINE_SCRIPT_BYTES + 1);
    let (built, file) = bash::prepare_script(&large, None, &FakeCall::default());
    assert!(file.is_none());
    assert_eq!(built, large);

    let (built, file) = bash::prepare_script(&large, Some("/tmp/it's.cwd"), &call);
    let file = file.unwrap();
    assert_eq!(std::fs::read_to_string(file.path()).unwrap(), large);
    assert!(built.starts_with("trap "));
    assert!(built.ends_with(&bash::file_bootstrap(file.path())));
    drop(file);
    assert!(!dir.file("sh").exists());

    let shell = Shell::new(host(None, Some("/no/such/bash")));
    block_on(shell.describe(&context(&[SHELL])));
    for bg in [false, true] {
        let input = json!({"command": large, "run_in_background": bg});
        let prepared = block_on(shell.prepare(BASH, &input.to_string())).unwrap();
        let ran = block_on(shell.run(prepared, &call));
        assert!(matches!(
            ran.outcome,
            Outcome::Failed(Failure {
                before_start: true,
                ..
            })
        ));
        assert!(!dir.file("sh").exists());
        assert!(!dir.file("cwd").exists());
    }
}
