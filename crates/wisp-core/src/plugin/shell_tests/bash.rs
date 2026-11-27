use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use futures::StreamExt;
use futures::stream::{self, BoxStream};
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wisp_plugin_shell::{MAX_COMMAND_BYTES, MAX_TIMEOUT_MS, NON_INTERACTIVE, SYSTEM_LINE, Timing};
use wisp_protocol::{
    AgentEvent, ClientCommand, ContentBlock, Effort, ExitStatus, Fact, Message, OutputKind, Role,
    StopReason, ToolAction, ToolOutcome, TurnOutcome, Usage,
};
use wisp_tool_text::output::Captured;

use super::{Ctl, Shell};
use crate::agent::{Agent, default_session};
use crate::call::{FileTarget, OutputDir, OutputSink};
use crate::jobs::exec::{self, HostSpawner, Isolation, Launch, Spawner};
use crate::plugin::KernelHost;
use crate::provider::anthropic::echoed_blocks;
use crate::provider::{Delta, Provider, ProviderError, ProviderEvent, Request};
use crate::test_guard::{ScratchDir, alive, gone, hold, pick, until};
use crate::tool::{EffectClass, Tag, ToolRun};
use crate::workspace::Workspace;

const NAME: &str = "bash";

const PATIENCE: Duration = Duration::from_secs(20);

const SCRATCH_PREFIX: &str = "wisp-bash-test-";

struct Reap(Vec<i32>);

impl Drop for Reap {
    fn drop(&mut self) {
        for pid in &self.0 {
            if let Some(pid) = Pid::from_raw(*pid) {
                let _ = kill_process(pid, Signal::KILL);
            }
        }
    }
}

struct Ran {
    run: ToolRun,
    captured: Captured,
    text: String,
    elapsed: Duration,
    finished: Instant,
    ended: SystemTime,
    kept_lines: bool,
}

impl Ran {
    fn output(&self) -> &str {
        assert!(self.captured.inlined(), "{}", self.text);
        &self.captured.head
    }

    fn exit(&self) -> Option<ExitStatus> {
        match &self.run.outcome {
            ToolOutcome::Completed { exit, .. } => *exit,
            _ => None,
        }
    }

    fn since(&self, at: Instant) -> Duration {
        self.finished.duration_since(at)
    }

    fn since_written(&self, path: &Path) -> Duration {
        let written = fs::metadata(path).unwrap().modified().unwrap();
        self.ended
            .duration_since(written)
            .unwrap_or_else(|_| panic!("the clock stepped back past {}", path.display()))
    }
}

fn cancel_once_written(
    file: PathBuf,
) -> (
    CancellationToken,
    Arc<OnceLock<Instant>>,
    impl Future<Output = ()>,
) {
    let token = CancellationToken::new();
    let trigger = token.clone();
    let at = Arc::new(OnceLock::new());
    let stamp = Arc::clone(&at);
    let fire = async move {
        marker(&file).await;
        let _ = stamp.set(Instant::now());
        trigger.cancel();
    };
    (token, at, fire)
}

fn cancelled_at(at: &OnceLock<Instant>) -> Instant {
    *at.get().unwrap()
}

struct Rig {
    scratch: ScratchDir,
    work: PathBuf,
    out: OutputDir,
    shell: Shell,
    cwd: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Self {
        Self::with_host(name, KernelHost::new())
    }

    fn with_spawner(name: &str, spawner: Arc<dyn Spawner>) -> Self {
        Self::with_host(name, KernelHost::new().with_spawner(spawner))
    }

    fn with_host(name: &str, host: KernelHost) -> Self {
        let scratch = ScratchDir::new(SCRATCH_PREFIX, name);
        let work = scratch.dir("work");
        let out = OutputDir::new(&scratch.dir("tmp"), "s1");
        let shell = Shell::new(host, Timing::default(), out.clone(), &work);
        Self {
            cwd: work.clone(),
            work,
            out,
            shell,
            scratch,
        }
    }

    async fn run(&mut self, input: Value) -> Ran {
        self.run_while(input, CancellationToken::new(), async {})
            .await
    }

    async fn run_while(
        &mut self,
        input: Value,
        cancel: CancellationToken,
        beside: impl Future<Output = ()>,
    ) -> Ran {
        let real = fs::canonicalize(&self.cwd).unwrap_or_else(|_| self.cwd.clone());
        assert!(
            real.starts_with(&self.scratch.root),
            "{} is outside the test's temp root {}",
            real.display(),
            self.scratch.root.display()
        );
        self.shell.out = self.out.clone();
        self.shell.rebind(crate::jobs::Processes::default());
        let cwd = self.cwd.clone();
        let started = Instant::now();
        let ctl = Ctl {
            cancel,
            ..Ctl::default()
        };
        let deadline = Some(tokio::time::Instant::now() + Duration::from_millis(MAX_TIMEOUT_MS));
        let called = self
            .shell
            .call(NAME, &input, &cwd, ctl, deadline, beside)
            .await
            .unwrap();
        let elapsed = started.elapsed();
        let finished = Instant::now();
        let ended = SystemTime::now();
        self.shell.processes.shutdown().await;
        if let Some(dir) = Shell::cwd_of(&called.run) {
            self.cwd = dir;
        }
        let notes: Vec<String> = called.run.trailer.clone().into_iter().collect();
        let text = crate::call::result_text(&called.run.outcome, &called.captured, None, &notes);
        Ran {
            run: called.run,
            captured: called.captured,
            text,
            elapsed,
            finished,
            ended,
            kept_lines: called.kept_lines,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.work.join(name)
    }
}

fn bash(command: &str) -> Value {
    json!({ "command": command })
}

fn have_setsid() -> bool {
    let found = exec::find_program("setsid").is_some();
    if !found {
        eprintln!("skipped: the setsid program is not on PATH");
    }
    found
}

async fn marker(path: &Path) -> String {
    let read = || {
        fs::read_to_string(path)
            .ok()
            .filter(|text| text.ends_with('\n'))
    };
    assert!(
        until(|| read().is_some()).await,
        "{} never appeared",
        path.display()
    );
    read().unwrap().trim().to_owned()
}

fn stat_ids(stat: &str) -> (i32, i32) {
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().collect())
        .unwrap_or_default();
    (fields[2].parse().unwrap(), fields[3].parse().unwrap())
}

#[test]
fn the_spec_is_canonical_and_holds_no_paths_or_ids() {
    let tool = crate::tool::testkit::bash_entry();
    let spec = tool.spec().clone();
    assert_eq!(spec.name, "bash");
    let bytes = serde_json::to_string(&spec).unwrap();
    assert!(!bytes.contains('/'), "{bytes}");
    assert!(
        bytes.starts_with(r#"{"description":"Run a bash command"#),
        "{bytes}"
    );
    assert!(
        bytes.contains(r#""input_schema":{"additionalProperties":false,"properties":{"command""#),
        "{bytes}"
    );
    assert!(
        !SYSTEM_LINE.chars().any(|c| c.is_ascii_digit()) && !SYSTEM_LINE.contains('/'),
        "{SYSTEM_LINE}"
    );
    assert_eq!(tool.meta().effect, EffectClass::NonIdempotent);
    assert_eq!(
        tool.meta().tags,
        BTreeSet::from([Tag::Irreversible, Tag::ExternalMessage, Tag::ReadsUntrusted])
    );
    assert!(!tool.meta().parallel_safe);
    assert_eq!(tool.meta().timeout.default, Duration::from_secs(120));
    assert_eq!(tool.meta().timeout.max, Some(Duration::from_secs(600)));
    assert!(tool.meta().saves_output);
    assert!(tool.meta().needs_workspace);
    assert!(!tool.meta().ends_reply);
}

#[test]
fn validation_rejects_bad_input_and_clamps_the_timeout() {
    let tool = crate::tool::testkit::bash_entry();
    let message = |input: Value| {
        futures::executor::block_on(tool.prepare(&input))
            .err()
            .map(|e| e.message)
    };
    assert_eq!(message(bash("ls")), None);
    assert_eq!(
        message(json!({})),
        Some("`command` must be a string".into())
    );
    assert_eq!(message(bash("  \n")), Some("`command` is empty".into()));
    assert_eq!(
        message(json!({"command": "ls", "cwd": "/"})),
        Some("unknown field `cwd`".into())
    );
    assert_eq!(
        message(json!({"command": "ls", "description": 3})),
        Some("`description` must be a string".into())
    );
    assert_eq!(
        message(json!({"command": "ls", "timeout_ms": 1.5})),
        Some("`timeout_ms` must be 0 or more milliseconds".into())
    );
    assert_eq!(
        message(json!({"command": "ls", "timeout_ms": "5"})),
        Some("`timeout_ms` must be 0 or more milliseconds".into())
    );
    assert_eq!(
        message(json!({"command": "ls", "timeout_ms": -5})),
        Some("`timeout_ms` must be 0 or more milliseconds".into())
    );
    assert_eq!(
        message(bash(&"x".repeat(MAX_COMMAND_BYTES + 1))),
        Some("`command` is 262,145 bytes; the limit is 262,144 bytes".into())
    );
    assert_eq!(message(bash(&"x".repeat(MAX_COMMAND_BYTES))), None);
}

#[test]
fn the_display_is_the_first_line_of_the_command() {
    let tool = crate::tool::testkit::bash_entry();
    let prepared = futures::executor::block_on(
        tool.prepare(&bash("\n  cargo test -p wisp-core  \necho done")),
    )
    .unwrap();
    assert_eq!(prepared.display().action, ToolAction::Run);
    assert_eq!(prepared.display().target, "  cargo test -p wisp-core");
    assert_eq!(prepared.display().output, OutputKind::Text);
}

#[tokio::test]
async fn a_quoted_trap_path_with_spaces_and_quotes_still_tracks() {
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "quote");
    let work = scratch.dir("work");
    fs::create_dir(work.join("sub")).unwrap();
    let mut rig = Rig::new("quote-rig");
    rig.out = OutputDir::new(&scratch.dir("it's a \"dir\" $HOME"), "s1");
    rig.work = work.clone();
    rig.cwd = work.clone();
    rig.scratch = scratch;
    let ran = rig.run(bash("cd sub")).await;
    assert_eq!(rig.cwd, work.join("sub"), "{}", ran.text);
    assert_eq!(ran.run.trailer, None);
}

#[tokio::test]
async fn cd_persists_and_survives_failures() {
    let mut rig = Rig::new("cd");
    fs::create_dir_all(rig.path("a/b")).unwrap();
    let ran = rig.run(bash("cd a")).await;
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
    assert_eq!(rig.cwd, rig.path("a"));
    let ran = rig.run(bash("pwd")).await;
    assert_eq!(ran.output(), format!("{}\n", rig.path("a").display()));
    let ran = rig.run(bash("cd b; exit 3")).await;
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 3 }));
    assert_eq!(ran.run.facts, vec![Fact::Exit { code: 3 }]);
    assert!(!ran.run.outcome.is_error());
    assert_eq!(rig.cwd, rig.path("a/b"));
    let ran = rig.run(bash("cd ..; if then")).await;
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 2 }));
    assert!(ran.output().contains("syntax error"), "{}", ran.text);
    assert_eq!(rig.cwd, rig.path("a/b"));
    let ran = rig.run(bash("pwd")).await;
    assert_eq!(ran.output(), format!("{}\n", rig.path("a/b").display()));
    let leftovers: Vec<_> = fs::read_dir(rig.out.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".cwd"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn a_replaced_trap_or_an_exec_keeps_the_old_directory() {
    let mut rig = Rig::new("trap");
    fs::create_dir(rig.path("sub")).unwrap();
    rig.run(bash("trap - EXIT; cd sub")).await;
    assert_eq!(rig.cwd, rig.work);
    rig.run(bash("trap 'echo bye' EXIT; cd sub")).await;
    assert_eq!(rig.cwd, rig.work);
    let ran = rig.run(bash("cd sub; exec true")).await;
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
    assert_eq!(rig.cwd, rig.work);
}

#[tokio::test]
async fn without_an_output_file_the_command_runs_untracked() {
    let mut rig = Rig::new("readonly");
    fs::create_dir(rig.path("sub")).unwrap();
    let plain_file = rig.scratch.root.join("plain-file");
    fs::write(&plain_file, "not a directory").unwrap();
    rig.out = OutputDir::new(&plain_file.join("tmp"), "s1");
    let ran = rig.run(bash("cd sub && echo moved")).await;
    assert_eq!(ran.output(), "moved\n");
    assert_eq!(rig.cwd, rig.work);
    assert_eq!(ran.run.state, None);
    let trailer = ran.run.trailer.clone().unwrap_or_default();
    assert!(
        trailer.starts_with("Working directory not tracked: "),
        "{trailer}"
    );
    assert!(
        ran.text.contains("Working directory not tracked"),
        "{}",
        ran.text
    );
    assert!(ran.captured.problem.is_some());
}

#[tokio::test]
async fn variables_do_not_persist() {
    let mut rig = Rig::new("vars");
    rig.run(bash("export WISP_TEST_VAR=1; alias ll=ls; f() { :; }"))
        .await;
    let ran = rig
        .run(bash(
            "echo \"${WISP_TEST_VAR-unset}\"; type ll >/dev/null 2>&1 || echo no-alias; type f >/dev/null 2>&1 || echo no-function",
        ))
        .await;
    assert_eq!(ran.output(), "unset\nno-alias\nno-function\n");
}

#[tokio::test]
async fn the_environment_arrives_unchanged_plus_the_defaults() {
    let mut rig = Rig::new("env");
    let ran = rig.run(bash("env -0 > env.out")).await;
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
    let mut seen = parse_env_nul(&fs::read(rig.path("env.out")).unwrap());
    let mut expected: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = std::env::vars_os()
        .map(|(k, v)| {
            use std::os::unix::ffi::OsStrExt;
            (k.as_bytes().to_vec(), v.as_bytes().to_vec())
        })
        .collect();
    for name in crate::jobs::STRIPPED_ENV {
        expected.remove(name.as_bytes());
    }
    for (k, v) in crate::jobs::PROMPT_DEFAULTS {
        expected.insert(k.as_bytes().to_vec(), v.as_bytes().to_vec());
    }
    for (k, v) in NON_INTERACTIVE {
        expected.insert(k.as_bytes().to_vec(), v.as_bytes().to_vec());
    }
    expected.insert(wisp_protocol::WISP_JOB.as_bytes().to_vec(), b"1".to_vec());
    for name in ["PWD", "OLDPWD", "SHLVL", "_"] {
        seen.remove(name.as_bytes());
        expected.remove(name.as_bytes());
    }
    assert!(
        seen == expected,
        "the job environment differs from its expected defaults"
    );
}

#[tokio::test]
async fn stdin_is_empty() {
    let mut rig = Rig::new("stdin");
    let ran = rig
        .run(json!({"command": "cat; echo after", "timeout_ms": 10_000}))
        .await;
    assert_eq!(ran.output(), "after\n");
    assert!(ran.elapsed < Duration::from_secs(5));
}

#[tokio::test]
async fn setsid_does_not_fork_so_the_spawned_pid_leads_its_session_and_group() {
    if !have_setsid() {
        return;
    }
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "setsid");
    let work = scratch.dir("work");
    for (spawner, isolation) in [
        (HostSpawner::detect(), Isolation::Session),
        (HostSpawner::with_setsid(None), Isolation::ProcessGroup),
    ] {
        assert_eq!(spawner.isolation(), isolation);
        let bash = exec::find_program("bash").unwrap();
        let args: Vec<OsString> = ["-c", "cat /proc/$$/stat"]
            .into_iter()
            .map(OsString::from)
            .collect();
        let process = spawner
            .spawn(&Launch {
                program: &bash,
                args: &args,
                cwd: &work,
                env: &NON_INTERACTIVE,
                stdin: false,
                log: None,
            })
            .unwrap();
        let pid = i32::try_from(process.id().unwrap()).unwrap();
        let mut sink = OutputSink::detached("c");
        let deadline = tokio::time::Instant::now() + PATIENCE;
        let done = exec::supervise(process, &mut sink, &CancellationToken::new(), deadline).await;
        assert!(done.status.unwrap().success());
        let stat = sink.finish().await.head;
        assert!(stat.starts_with(&format!("{pid} (")), "{stat}");
        let (group, session) = stat_ids(&stat);
        assert_eq!(group, pid, "{isolation:?}");
        match isolation {
            Isolation::Session => assert_eq!(session, pid),
            Isolation::ProcessGroup => assert_ne!(session, pid),
        }
    }
}

#[tokio::test]
async fn a_grandchild_is_killed_on_timeout() {
    let mut rig = Rig::new("timeout");
    let ran = rig
        .run(json!({
            "command": "sleep 30 & echo $! > pid.txt; wait",
            "timeout_ms": 3000
        }))
        .await;
    assert_eq!(ran.run.outcome, ToolOutcome::TimedOut { after_ms: 3000 });
    assert!(ran.run.outcome.is_error());
    assert!(ran.elapsed < Duration::from_secs(20), "{:?}", ran.elapsed);
    let pid: i32 = marker(&rig.path("pid.txt")).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    assert!(gone(pid).await, "the grandchild {pid} survived the timeout");
    assert!(ran.text.contains("Timed out after 3 s"), "{}", ran.text);
}

#[tokio::test]
async fn a_grandchild_is_killed_on_cancel_even_when_it_ignores_sigterm() {
    let mut rig = Rig::new("cancel");
    let pid_file = rig.path("pid.txt");
    for (command, slow) in [
        ("sleep 30 & echo $! > pid.txt; wait", false),
        ("trap '' TERM; sleep 30 & echo $! > pid.txt; wait", true),
    ] {
        let _ = fs::remove_file(&pid_file);
        let (token, at, fire) = cancel_once_written(pid_file.clone());
        let ran = rig.run_while(bash(command), token, fire).await;
        let stopping = ran.since(cancelled_at(&at));
        assert_eq!(ran.run.outcome, ToolOutcome::Cancelled, "{command}");
        let pid: i32 = marker(&pid_file).await.parse().unwrap();
        let _reap = Reap(vec![pid]);
        assert!(gone(pid).await, "{command}: {pid} survived the cancel");
        let limit = exec::TERM_GRACE + exec::DRAIN + Duration::from_secs(2);
        assert!(stopping < limit, "{command}: {stopping:?}");
        if slow {
            assert!(stopping >= exec::TERM_GRACE, "{stopping:?}");
        }
    }
}

#[tokio::test]
async fn a_background_job_does_not_outlive_its_call() {
    let mut rig = Rig::new("background");
    let ran = rig
        .run(bash("sleep 30 & echo $! > pid.txt; echo started"))
        .await;
    assert_eq!(ran.output(), "started\n");
    assert!(ran.elapsed < Duration::from_secs(5), "{:?}", ran.elapsed);
    let pid: i32 = marker(&rig.path("pid.txt")).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    assert!(
        gone(pid).await,
        "the background job {pid} outlived its call"
    );
}

#[tokio::test]
async fn an_escaped_child_holding_the_pipe_ends_the_call_within_the_drain() {
    if !have_setsid() {
        return;
    }
    let mut rig = Rig::new("escape");
    let ran = rig
        .run(bash(
            "setsid sh -c 'echo $$ > pid.txt; exec sleep 30' & while [ ! -s pid.txt ]; do :; done; echo out; : > exited.txt",
        ))
        .await;
    let pid: i32 = marker(&rig.path("pid.txt")).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    assert!(alive(pid), "the escaped child should still be running");
    assert_eq!(ran.output(), "out\n");
    let drained = ran.since_written(&rig.path("exited.txt"));
    assert!(drained >= exec::DRAIN, "{drained:?}");
    assert!(
        drained < exec::DRAIN + Duration::from_secs(2),
        "{drained:?}"
    );
}

#[tokio::test]
async fn stdout_and_stderr_keep_their_order() {
    let mut rig = Rig::new("order");
    let ran = rig
        .run(bash(
            "for i in 1 2 3; do echo out$i; echo err$i >&2; done; ls /nonexistent-wisp-path",
        ))
        .await;
    let output = ran.output();
    assert!(
        output.starts_with("out1\nerr1\nout2\nerr2\nout3\nerr3\nls: "),
        "{output}"
    );
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 2 }));
}

#[tokio::test]
async fn fifty_mebibytes_stream_to_the_file_with_an_exact_preview() {
    let mut rig = Rig::new("large");
    let lines: u64 = 3_744_915;
    let width = 14u64;
    let ran = rig
        .run(json!({
            "command": format!("seq -f 'line %08.0f' 1 {lines}"),
            "timeout_ms": 300_000
        }))
        .await;
    let total = lines * width;
    assert!(total >= 50 * 1024 * 1024);
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
    assert_eq!(ran.captured.bytes, total);
    assert_eq!(ran.captured.lines, lines);
    assert!(!ran.captured.inlined());
    let file = ran.captured.path.clone().unwrap();
    assert_eq!(fs::metadata(&file).unwrap().len(), total);
    let line = |n: u64| format!("line {n:08}");
    let head: Vec<String> = (1..=73).map(line).collect();
    let tail: Vec<String> = (lines - 218..=lines).map(line).collect();
    let expected = format!(
        "Exit code: 0\nOutput: 52,428,810 bytes, 3,744,915 lines, saved in full to {}\nLines 1–73:\n{}\n[… lines 74–3,744,696 (52,424,722 bytes) not shown; read them from the file …]\nLines 3,744,697–3,744,915:\n{}",
        file.display(),
        head.join("\n"),
        tail.join("\n")
    );
    assert_eq!(ran.text, expected);
}

#[tokio::test]
async fn the_reader_queue_and_the_sink_stay_bounded() {
    let rig = Rig::new("bounded");
    let target: FileTarget = rig
        .out
        .create(1, NAME)
        .map(|(p, f)| (p, Box::new(f) as Box<dyn std::io::Write + Send>))
        .map_err(|e| e.to_string());
    let mut sink = OutputSink::new("c", None, target);
    let bash_path = exec::find_program("bash").unwrap();
    let args: Vec<OsString> = ["-c", "head -c 20000000 /dev/zero | tr '\\0' 'x'"]
        .into_iter()
        .map(OsString::from)
        .collect();
    let process = HostSpawner::detect()
        .spawn(&Launch {
            program: &bash_path,
            args: &args,
            cwd: &rig.work,
            env: &NON_INTERACTIVE,
            stdin: false,
            log: None,
        })
        .unwrap();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let done = exec::supervise(process, &mut sink, &CancellationToken::new(), deadline).await;
    assert!(done.status.unwrap().success());
    assert!(sink.held_bytes() <= 2 * wisp_tool_text::output::INLINE_LIMIT + 64 * 1024 + 16 * 1024);
    let captured = sink.finish().await;
    assert_eq!(captured.bytes, 20_000_000);
    assert_eq!(exec::QUEUED_CHUNKS * exec::CHUNK_BYTES, 1024 * 1024);
}

#[tokio::test]
async fn the_result_is_cleaned_while_the_file_keeps_the_raw_bytes() {
    let mut rig = Rig::new("clean");
    let ran = rig
        .run(bash(
            r"printf '10%%\r50%%\r100%%\n\033[31mred\033[0m\n\377ok\r\n'",
        ))
        .await;
    assert_eq!(ran.output(), "100%\nred\n\u{fffd}ok\n");
    let raw = fs::read(ran.captured.path.clone().unwrap()).unwrap();
    assert_eq!(raw, b"10%\r50%\r100%\n\x1b[31mred\x1b[0m\n\xffok\r\n");
}

#[tokio::test]
async fn a_long_timeout_is_taken_as_given() {
    let mut rig = Rig::new("long-limit");
    let ran = rig
        .run(json!({"command": "echo hi", "timeout_ms": 900_000}))
        .await;
    assert_eq!(ran.text, "Exit code: 0\nOutput:\nhi");
}

#[test]
fn turning_bash_on_brings_the_process_tool() {
    assert_eq!(
        crate::tool::testkit::bundled(&["bash"]).names(),
        ["bash", "process"]
    );
}

fn one_cell(rows: Vec<wisp_protocol::DoctorRow>) -> Vec<(String, String)> {
    rows.into_iter()
        .map(|row| match row.cells.as_slice() {
            [wisp_protocol::DoctorCell::Text { text }] => (row.key, text.clone()),
            other => panic!("{} has the cells {other:?}", row.key),
        })
        .collect()
}

#[test]
fn jobs_doctor_rows_name_setsid_and_the_output_directory() {
    let rows = one_cell(crate::jobs::doctor_rows());
    let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["setsid", "setsid --ctty", "tool output"]);
    assert!(
        rows[1].1.starts_with("yes: ") || rows[1].1.starts_with("no: "),
        "{rows:?}"
    );
    assert!(rows[2].1.ends_with("wisp-<session id>"), "{rows:?}");
}

#[tokio::test]
async fn the_tools_doctor_includes_unavailable_memory_before_bash_and_jobs() {
    let (registry, problems) = crate::plugin::Registry::new(
        wisp_plugins::bundled(),
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let on: BTreeSet<String> = crate::tool::testkit::ALL
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let redactions = crate::session::Redactions::default();
    let rows = one_cell(
        crate::plugin::doctor_rows(
            &registry,
            &on,
            crate::mcp::Service::default(),
            &redactions,
            crate::plugin::DoctorWorkspace::default(),
        )
        .await,
    );
    let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        ["memory", "bash", "setsid", "setsid --ctty", "tool output"]
    );
    assert_eq!(rows[0].1, "unavailable");
    let bash = &rows[1].1;
    let found = exec::find_program("bash");
    match found {
        Some(path) => {
            assert!(
                bash.starts_with(&crate::workspace::home_shown(&path)),
                "{bash}"
            );
            assert!(bash.ends_with(')'), "{bash}");
        }
        None => assert_eq!(bash, "not found on PATH"),
    }
    let off = one_cell(
        crate::plugin::doctor_rows(
            &registry,
            &BTreeSet::new(),
            crate::mcp::Service::default(),
            &redactions,
            crate::plugin::DoctorWorkspace::default(),
        )
        .await,
    );
    let keys: Vec<&str> = off.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["memory", "setsid", "setsid --ctty", "tool output"]);
}

type Reply = Box<dyn Fn(&[Message]) -> Vec<ProviderEvent> + Send + Sync>;

struct Model {
    replies: Mutex<Vec<Reply>>,
    requests: Mutex<Vec<(Option<String>, Vec<Message>)>>,
    instructions: Mutex<Vec<Option<String>>>,
}

impl Model {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().rev().collect()),
            requests: Mutex::new(Vec::new()),
            instructions: Mutex::new(Vec::new()),
        })
    }
}

impl Provider for Model {
    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        self.requests
            .lock()
            .unwrap()
            .push((request.session.system.clone(), request.messages.to_vec()));
        self.instructions
            .lock()
            .unwrap()
            .push(request.session.instructions.clone());
        let reply = self.replies.lock().unwrap().pop().unwrap();
        stream::iter(reply(request.messages).into_iter().map(Ok)).boxed()
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &crate::provider::ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        echoed_blocks(content, earlier)
    }

    fn cache_prefix(&self, _request: &Request<'_>) -> Option<String> {
        None
    }
}

fn events(
    blocks: Vec<ContentBlock>,
    deltas: Vec<Option<Delta>>,
    stop: StopReason,
) -> Vec<ProviderEvent> {
    let usage = Usage {
        input_tokens: 600,
        output_tokens: 10,
        ..Usage::default()
    };
    let mut out = vec![
        ProviderEvent::Sending,
        crate::agent::testing::started(None, crate::agent::DEFAULT_MODEL, usage),
    ];
    for (index, (block, delta)) in blocks.into_iter().zip(deltas).enumerate() {
        out.push(ProviderEvent::BlockStart { index, block });
        out.extend(delta.map(|delta| ProviderEvent::BlockDelta { index, delta }));
        out.push(ProviderEvent::BlockStop { index });
    }
    out.push(ProviderEvent::MessageDelta {
        stop_reason: Some(stop),
        stop_details: None,
        usage,
        iterations: Vec::new(),
        diagnostics: None,
    });
    out.push(ProviderEvent::MessageStop);
    out
}

fn calls(id: &'static str, command: String) -> Vec<ProviderEvent> {
    let block = ContentBlock::ToolUse {
        id: id.into(),
        name: NAME.into(),
        input: json!({}),
    };
    let input = serde_json::to_string(&bash(&command)).unwrap();
    events(
        vec![block],
        vec![Some(Delta::InputJson(input))],
        StopReason::ToolUse,
    )
}

fn says(text: &'static str) -> Vec<ProviderEvent> {
    let block = ContentBlock::Text {
        text: String::new(),
    };
    events(
        vec![block],
        vec![Some(Delta::Text(text.into()))],
        StopReason::EndTurn,
    )
}

fn last_result(messages: &[Message]) -> String {
    messages
        .last()
        .into_iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => content.iter().find_map(|c| match c {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
}

async fn turn(agent: &mut Agent, text: &str) -> (TurnOutcome, Vec<AgentEvent>) {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let collector = tokio::spawn(async move {
        let _keep = cmd_tx;
        let mut seen = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            seen.push(e);
        }
        seen
    });
    let outcome = agent
        .run_turn(text.into(), &mut cmd_rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    (outcome, collector.await.unwrap())
}

fn bash_agent(model: Arc<Model>, scratch: &ScratchDir, work: &Path) -> Agent {
    let toolbox = crate::tool::testkit::shell(Timing::default());
    Agent::new(model, default_session())
        .with_tools(
            toolbox,
            Workspace {
                root: work.to_path_buf(),
                home: None,
                temp: scratch.dir("tmp"),
            },
        )
        .with_platform("Linux")
        .with_resources(None)
}

#[tokio::test]
async fn the_model_reads_a_previewed_file_back_with_sed() {
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "readback");
    let work = scratch.dir("work");
    let model = Model::new(vec![
        Box::new(|_: &[Message]| calls("a", "seq -f 'row %05.0f' 1 5000".into())),
        Box::new(|messages: &[Message]| {
            let result = last_result(messages);
            let path = result
                .lines()
                .find_map(|l| l.split("saved in full to ").nth(1))
                .unwrap_or("missing")
                .to_owned();
            calls("b", format!("sed -n 1234p {path}"))
        }),
        Box::new(|_: &[Message]| says("row 01234")),
    ]);
    let mut agent = bash_agent(model.clone(), &scratch, &work);
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                steps: 3,
                tool_calls: 2,
                ..
            }
        ),
        "{outcome:?}"
    );
    let requests = model.requests.lock().unwrap().clone();
    let first = last_result(&requests[1].1);
    assert!(
        first.contains("not shown; read or search them with the process tool"),
        "{first}"
    );
    let second = last_result(&requests[2].1);
    assert_eq!(second, "Exit code: 0\nOutput:\nrow 01234");
    let finished = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .count();
    assert_eq!(finished, 2);
}

#[tokio::test]
async fn a_deleted_directory_falls_back_to_the_session_root_then_to_the_root_dir() {
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "fallback");
    let work = scratch.dir("work");
    let model = Model::new(vec![
        Box::new(|_: &[Message]| calls("a", "mkdir sub && cd sub".into())),
        Box::new(|_: &[Message]| calls("b", "rmdir \"$PWD\"".into())),
        Box::new(|_: &[Message]| calls("c", "pwd".into())),
        Box::new(|_: &[Message]| calls("d", "rmdir \"$PWD\"".into())),
        Box::new(|_: &[Message]| calls("e", "pwd".into())),
        Box::new(|_: &[Message]| says("done")),
    ]);
    let mut agent = bash_agent(model.clone(), &scratch, &work);
    let (outcome, _) = turn(&mut agent, "go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(
        last_result(&requests[1].1),
        format!(
            "Exit code: 0\nOutput: (none)\nWorking directory: {}",
            work.join("sub").display()
        )
    );
    assert_eq!(
        last_result(&requests[3].1),
        format!(
            "Exit code: 0\nOutput:\n{w}\n{s} no longer exists; this ran in {w}",
            w = work.display(),
            s = work.join("sub").display()
        )
    );
    assert_eq!(
        last_result(&requests[5].1),
        format!(
            "Exit code: 0\nOutput:\n/\n{} no longer exists; this ran in /",
            work.display()
        )
    );
}

#[tokio::test]
async fn a_bash_session_states_the_tool_in_the_system_prompt_and_the_environment() {
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "prompt");
    let work = scratch.dir("work");
    fs::write(work.join("CLAUDE.md"), "secret-instructions-claude").unwrap();
    fs::write(work.join("AGENTS.md"), "secret-instructions-agents").unwrap();
    let model = Model::new(vec![
        Box::new(|_: &[Message]| says("hi")),
        Box::new(|_: &[Message]| says("again")),
    ]);
    let mut agent = bash_agent(model.clone(), &scratch, &work).with_test_instructions(
        crate::agent::InstructionParts::auto(),
        Some(&scratch.dir("config")),
    );
    turn(&mut agent, "hello").await;
    turn(&mut agent, "more").await;
    let requests = model.requests.lock().unwrap().clone();
    let project = model.instructions.lock().unwrap().clone();
    assert_eq!(project[0], project[1]);
    let project = project[0].clone().unwrap();
    assert!(project.contains("secret-instructions-agents"), "{project}");
    assert!(!project.contains("secret-instructions-claude"), "{project}");
    let system = requests[0].0.clone().unwrap();
    assert!(!system.contains("secret-instructions"), "{system}");
    assert!(system.contains(SYSTEM_LINE), "{system}");
    assert_eq!(system.matches(SYSTEM_LINE).count(), 1);
    assert!(
        !system.chars().any(|c| c.is_ascii_digit()) && !system.contains('/'),
        "{system}"
    );
    assert_eq!(requests[1].0, requests[0].0);
    let first = serde_json::to_string(&requests[1].1[0]).unwrap();
    assert!(first.contains("Shell for the bash tool: "), "{first}");
    let all = serde_json::to_string(&requests[1].1).unwrap();
    assert_eq!(all.matches("<environment>").count(), 1);
    assert!(!all.contains("secret-instructions"), "{all}");
    assert_eq!(requests[1].1[0].role, Role::User);
}

#[test]
fn the_tool_sentence_joins_the_system_prompt_once() {
    use crate::agent::{BASE_SYSTEM_PROMPT, system_prompt};
    let lines = vec![SYSTEM_LINE.to_owned()];
    let sentences = crate::plugin::testing::turn_line_sentences();
    let tail = sentences.join(" ");
    let stamped = system_prompt(Some(BASE_SYSTEM_PROMPT), &[], &sentences).unwrap();
    let with = system_prompt(Some(&stamped), &lines, &sentences).unwrap();
    assert_eq!(with, format!("{BASE_SYSTEM_PROMPT} {SYSTEM_LINE} {tail}"));
    assert_eq!(
        system_prompt(Some(&with), &lines, &sentences).as_deref(),
        Some(with.as_str())
    );
    assert_eq!(
        system_prompt(None, &lines, &sentences),
        Some(format!("{SYSTEM_LINE} {tail}"))
    );
    assert_eq!(
        system_prompt(Some(BASE_SYSTEM_PROMPT), &[], &sentences),
        Some(format!("{BASE_SYSTEM_PROMPT} {tail}"))
    );
    assert_eq!(
        system_prompt(Some(BASE_SYSTEM_PROMPT), &lines, &[]),
        Some(format!("{BASE_SYSTEM_PROMPT} {SYSTEM_LINE}"))
    );
}

#[test]
fn either_order_of_tools_and_hooks_gives_the_same_prompt() {
    let workspace = || Workspace {
        root: std::env::temp_dir(),
        home: None,
        temp: std::env::temp_dir(),
    };
    let toolbox = || crate::tool::testkit::shell(Timing::default());
    let model = || Model::new(Vec::new());
    let hooks = || crate::plugin::testing::turn_lines(Vec::new());
    let tools_first = {
        let started = hooks();
        Agent::new(model(), default_session())
            .with_tools(toolbox(), workspace())
            .with_hooks(started.hooks, started.host)
    };
    let hooks_first = {
        let started = hooks();
        Agent::new(model(), default_session())
            .with_hooks(started.hooks, started.host)
            .with_tools(toolbox(), workspace())
    };
    let system = tools_first.session().system.clone();
    assert_eq!(system, hooks_first.session().system);
    assert_eq!(
        system,
        Some(format!(
            "{} {SYSTEM_LINE} {}",
            crate::agent::BASE_SYSTEM_PROMPT,
            crate::plugin::testing::turn_line_tail()
        ))
    );
}

fn open_pty() -> (OwnedFd, fs::File) {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    (master, fs::File::from(slave))
}

const PTY_CHILD: &str = "WISP_BASH_PTY_CHILD";

#[tokio::test]
async fn no_command_can_reach_the_controlling_terminal() {
    if !have_setsid() {
        return;
    }
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "pty");
    let report = scratch.root.join("report.txt");
    let (master, slave) = open_pty();
    let mut child = std::process::Command::new(exec::find_program("setsid").unwrap())
        .arg("-c")
        .arg(std::env::current_exe().unwrap())
        .args([
            "plugin::shell_tests::bash::pty_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PTY_CHILD, &report)
        .current_dir(&scratch.root)
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .spawn()
        .unwrap();
    let _reap = Reap(vec![i32::try_from(child.id()).unwrap()]);
    let flags = rustix::fs::fcntl_getfl(&master).unwrap();
    rustix::fs::fcntl_setfl(&master, flags | rustix::fs::OFlags::NONBLOCK).unwrap();
    let mut screen = Vec::new();
    let start = Instant::now();
    let mut buf = [0u8; 4096];
    loop {
        match rustix::io::read(&master, &mut buf) {
            Ok(n) if n > 0 => screen.extend_from_slice(&buf[..n]),
            _ => {
                if child.try_wait().unwrap().is_some() {
                    while let Ok(n) = rustix::io::read(&master, &mut buf) {
                        if n == 0 {
                            break;
                        }
                        screen.extend_from_slice(&buf[..n]);
                    }
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(60),
                    "the pty child hung"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    let screen = String::from_utf8_lossy(&screen);
    let report = fs::read_to_string(&report).unwrap_or_default();
    assert!(
        screen.contains("control-reached"),
        "{screen}\n---\n{report}"
    );
    assert!(!screen.contains("leaked"), "{screen}");
    assert!(report.contains("read-refused"), "{report}");
    assert!(report.contains("write-refused"), "{report}");
    assert!(!report.contains("opened"), "{report}");
    assert!(report.contains("cat-returned"), "{report}");
}

#[tokio::test]
#[ignore = "a helper that no_command_can_reach_the_controlling_terminal runs under a pty"]
async fn pty_child() {
    let Some(report) = std::env::var_os(PTY_CHILD) else {
        return;
    };
    let control = std::process::Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "echo control-reached > /dev/tty",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let mut rig = Rig::new("pty-child");
    let ran = rig
        .run(bash(
            "(exec </dev/tty) 2>&1 && echo opened || echo read-refused; (echo leaked > /dev/tty) 2>&1 || echo write-refused",
        ))
        .await;
    let stdin = rig
        .run(json!({"command": "cat; echo cat-returned", "timeout_ms": 3000}))
        .await;
    fs::write(
        report,
        format!("control {control}\n{}\n{}", ran.text, stdin.text),
    )
    .unwrap();
}

#[tokio::test]
async fn an_escaped_process_that_keeps_writing_ends_the_call_at_the_drain() {
    if !have_setsid() {
        return;
    }
    let mut rig = Rig::new("escaped-writer");
    let ran = tokio::time::timeout(
        PATIENCE,
        rig.run(bash(
            "setsid sh -c 'echo $$ > pid.txt; exec yes' & while [ ! -s pid.txt ]; do :; done; : > exited.txt",
        )),
    )
    .await;
    let pid: i32 = marker(&rig.path("pid.txt")).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    let ran = ran.expect("the drain never ended");
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
    let drained = ran.since_written(&rig.path("exited.txt"));
    assert!(drained >= exec::DRAIN, "{drained:?}");
}

#[tokio::test]
async fn a_process_in_its_own_group_inside_the_session_is_stopped() {
    if !have_setsid() {
        return;
    }
    let mut rig = Rig::new("own-group");
    let pid_file = rig.path("pid.txt");
    let detached = "timeout 60 sh -c 'echo $$ > pid.txt; exec sleep 33'";
    let token = CancellationToken::new();
    let trigger = token.clone();
    let file = pid_file.clone();
    let ran = rig
        .run_while(bash(detached), token, async move {
            marker(&file).await;
            trigger.cancel();
        })
        .await;
    assert_eq!(ran.run.outcome, ToolOutcome::Cancelled);
    let pid: i32 = marker(&pid_file).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    assert!(gone(pid).await, "cancel left {pid} running");
    for command in [
        format!("{detached} & while [ ! -s pid.txt ]; do :; done; echo started"),
        "set -m; sleep 32 & echo $! > pid.txt; echo started".to_owned(),
    ] {
        let _ = fs::remove_file(&pid_file);
        let ran = rig.run(bash(&command)).await;
        assert_eq!(ran.output(), "started\n", "{command}");
        let pid: i32 = marker(&pid_file).await.parse().unwrap();
        let _reap = Reap(vec![pid]);
        assert!(gone(pid).await, "{command}: {pid} outlived its call");
    }
    let _ = fs::remove_file(&pid_file);
    let ran = rig
        .run(json!({"command": format!("{detached}; true"), "timeout_ms": 3000}))
        .await;
    assert_eq!(ran.run.outcome, ToolOutcome::TimedOut { after_ms: 3000 });
    let pid: i32 = marker(&pid_file).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    assert!(gone(pid).await, "the timeout left {pid} running");
}

#[tokio::test]
async fn each_leftover_gets_exactly_one_sigterm() {
    if !have_setsid() {
        return;
    }
    let mut rig = Rig::new("one-term");
    let ran = rig
        .run(bash(&format!(
            "for i in 1 2 3 4; do bash -c \"exec 2>/dev/null; trap 'echo t >> term-$i.txt' TERM; echo \\$\\$ > ready-$i.txt; {}\" & done; while [ $(cat ready-*.txt 2>/dev/null | wc -l) -lt 4 ]; do :; done; echo started",
            hold()
        )))
        .await;
    let pids = (1..=4)
        .map(|i| fs::read_to_string(rig.path(&format!("ready-{i}.txt"))))
        .filter_map(|text| text.ok()?.trim().parse().ok())
        .collect();
    let _reap = Reap(pids);
    assert_eq!(ran.output(), "started\n");
    for i in 1..=4 {
        let terms = fs::read_to_string(rig.path(&format!("term-{i}.txt"))).unwrap_or_default();
        assert_eq!(
            terms.lines().count(),
            1,
            "leftover {i} got SIGTERM {terms:?}"
        );
    }
}

#[tokio::test]
async fn children_get_sigterm_and_the_grace_to_clean_up() {
    let mut rig = Rig::new("cleanup");
    let pid_file = rig.path("pid.txt");
    let cleaned = rig.path("cleaned.txt");
    let child = format!(
        "sh -c 'trap \"sleep 0.3; echo cleaned > cleaned.txt; exit 0\" TERM; echo $$ > pid.txt; {}' &",
        hold()
    );
    let (token, at, fire) = cancel_once_written(pid_file.clone());
    let ran = rig
        .run_while(bash(&format!("{child} wait")), token, fire)
        .await;
    let stopping = ran.since(cancelled_at(&at));
    assert_eq!(ran.run.outcome, ToolOutcome::Cancelled);
    let pid: i32 = marker(&pid_file).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    assert_eq!(
        marker(&cleaned).await,
        "cleaned",
        "the cancel skipped the cleanup"
    );
    assert!(stopping < exec::TERM_GRACE, "{stopping:?}");
    fs::remove_file(&pid_file).unwrap();
    fs::remove_file(&cleaned).unwrap();
    let ran = rig
        .run(bash(&format!(
            "{child} while [ ! -s pid.txt ]; do :; done; echo bye"
        )))
        .await;
    assert!(ran.output().starts_with("bye\n"), "{}", ran.text);
    let pid: i32 = marker(&pid_file).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    assert_eq!(
        marker(&cleaned).await,
        "cleaned",
        "the exit skipped the cleanup"
    );
    assert!(gone(pid).await);
}

#[tokio::test]
async fn dropping_a_running_call_kills_its_processes() {
    let mut rig = Rig::new("drop");
    let cwd = rig.work.clone();
    let deadline = Some(tokio::time::Instant::now() + PATIENCE);
    let mut begun = rig
        .shell
        .begin(
            NAME,
            &bash("sleep 31 & echo $! > pid.txt; wait"),
            &cwd,
            Ctl::default(),
            deadline,
        )
        .unwrap();
    let input = begun.input.take().unwrap();
    let pid_file = rig.path("pid.txt");
    let pid: i32 = {
        let run = begun.plugin.run(input, begun.call.as_ref());
        tokio::select! {
            _ = run => panic!("the call ended before it was dropped"),
            pid = marker(&pid_file) => pid.parse().unwrap(),
        }
    };
    let _reap = Reap(vec![pid]);
    let called = rig.shell.finish(&begun, None).await;
    assert_eq!(called.run.outcome, ToolOutcome::Cancelled);
    assert!(gone(pid).await, "dropping the call left {pid} running");
}

#[tokio::test]
async fn the_trap_ignores_a_redefined_pwd_and_stays_out_of_xtrace() {
    let mut rig = Rig::new("pwd");
    fs::create_dir(rig.path("sub")).unwrap();
    rig.run(bash("pwd() { echo /etc; }; cd sub")).await;
    assert_eq!(rig.cwd, rig.path("sub"));
    let ran = rig.run(bash("set -x; cd ..")).await;
    assert_eq!(rig.cwd, rig.work);
    assert_eq!(ran.output(), "+ cd ..\n");
}

#[tokio::test]
async fn a_directory_name_that_is_not_utf8_is_reported_untracked() {
    let mut rig = Rig::new("utf8");
    let ran = rig.run(bash("mkdir $'\\xff' && cd $'\\xff'")).await;
    assert_eq!(rig.cwd, rig.work);
    assert_eq!(ran.run.state, None);
    let expected = format!(
        "Working directory not tracked: {}/\u{fffd} is not valid UTF-8",
        rig.work.display()
    );
    assert_eq!(ran.run.trailer.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn a_symlinked_start_directory_is_not_reported_as_a_change() {
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "symlink");
    let real = scratch.dir("real");
    let link = scratch.root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let model = Model::new(vec![
        Box::new(|_: &[Message]| calls("a", "true".into())),
        Box::new(|_: &[Message]| calls("b", "echo again".into())),
        Box::new(|_: &[Message]| says("done")),
    ]);
    let mut agent = bash_agent(model.clone(), &scratch, &link);
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(last_result(&requests[1].1), "Exit code: 0\nOutput: (none)");
    assert_eq!(last_result(&requests[2].1), "Exit code: 0\nOutput:\nagain");
    let link_text = link.display().to_string();
    let cwds: Vec<&str> = pick!(&events, AgentEvent::ToolCallStarted { cwd, .. } => cwd.as_str());
    assert_eq!(cwds, [link_text.as_str(), link_text.as_str()]);
}

fn reap_session(session: i32) -> Vec<i32> {
    let Some(leader) = Pid::from_raw(session) else {
        return Vec::new();
    };
    let first: Vec<i32> = exec::session_members(leader)
        .iter()
        .map(|p| p.as_raw_nonzero().get())
        .collect();
    let start = Instant::now();
    loop {
        let left = exec::session_members(leader);
        if left.is_empty() || start.elapsed() > Duration::from_secs(5) {
            return first;
        }
        for pid in left {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}

#[tokio::test]
async fn processes_forked_during_the_kill_sweep_are_killed_too() {
    if !have_setsid() {
        return;
    }
    fork_chain_leaves_no_survivors(Rig::new("fork-sweep")).await;
}

struct Stale;

impl exec::SessionProbe for Stale {
    fn scan(&self, session: i32) -> std::io::Result<Vec<Pid>> {
        let found = exec::ProcProbe.scan(session);
        std::thread::sleep(Duration::from_millis(25));
        found
    }

    fn open(&self, pid: Pid) -> std::io::Result<OwnedFd> {
        exec::ProcProbe.open(pid)
    }
}

#[tokio::test]
async fn a_fork_chain_faster_than_the_session_scan_is_killed_too() {
    if !have_setsid() {
        return;
    }
    let rig = Rig::with_spawner(
        "fork-stale-scan",
        Arc::new(HostSpawner::detect().with_probe(Arc::new(Stale))),
    );
    fork_chain_leaves_no_survivors(rig).await;
}

async fn fork_chain_leaves_no_survivors(mut rig: Rig) {
    let sid_file = rig.path("sid.txt");
    let token = CancellationToken::new();
    let trigger = token.clone();
    let file = sid_file.clone();
    let ran = rig
        .run_while(
            bash(
                "echo $$ > sid.txt; trap '' TERM; spawn() { sleep 0.002; spawn & exec sleep 40; }; spawn",
            ),
            token,
            async move {
                let session: i32 = marker(&file).await.parse().unwrap();
                let leader = Pid::from_raw(session).unwrap();
                let start = Instant::now();
                while exec::session_members(leader).len() < 60 && start.elapsed() < PATIENCE {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                trigger.cancel();
            },
        )
        .await;
    let session: i32 = marker(&sid_file).await.parse().unwrap();
    let left = reap_session(session);
    assert_eq!(ran.run.outcome, ToolOutcome::Cancelled);
    assert!(
        left.is_empty(),
        "survivors in session {session}: {:?}",
        left
    );
}

struct Broken {
    scan: bool,
}

impl exec::SessionProbe for Broken {
    fn scan(&self, session: i32) -> std::io::Result<Vec<Pid>> {
        if self.scan {
            Err(std::io::Error::from_raw_os_error(
                rustix::io::Errno::NOSYS.raw_os_error(),
            ))
        } else {
            exec::ProcProbe.scan(session)
        }
    }

    fn open(&self, _pid: Pid) -> std::io::Result<OwnedFd> {
        Err(std::io::Error::from_raw_os_error(
            rustix::io::Errno::NOSYS.raw_os_error(),
        ))
    }
}

#[tokio::test]
async fn without_a_proc_scan_or_pidfds_signals_still_arrive() {
    if !have_setsid() {
        return;
    }
    for scan in [true, false] {
        let mut rig = Rig::with_spawner(
            "broken-probe",
            Arc::new(HostSpawner::detect().with_probe(Arc::new(Broken { scan }))),
        );
        let pid_file = rig.path("pid.txt");
        let (token, at, fire) = cancel_once_written(pid_file.clone());
        let script = "trap 'echo leader >> term.txt' TERM; \
                      (trap 'echo child >> term.txt; exit' TERM; \
                      sleep 41 & echo $! > pid.txt; wait) & wait";
        let ran = rig.run_while(bash(script), token, fire).await;
        let stopping = ran.since(cancelled_at(&at));
        let pid: i32 = marker(&pid_file).await.parse().unwrap();
        let _reap = Reap(vec![pid]);
        assert_eq!(
            ran.run.outcome,
            ToolOutcome::Cancelled,
            "scan broken: {scan}"
        );
        assert!(gone(pid).await, "scan broken {scan}: {pid} survived");
        let heard = || fs::read_to_string(rig.path("term.txt")).unwrap_or_default();
        assert!(
            until(|| {
                let text = heard();
                text.contains("leader") && text.contains("child")
            })
            .await,
            "scan broken {scan}: the TERM never arrived ({stopping:?}, {:?})",
            heard()
        );
    }
}

struct Counting<P>(P, AtomicU32);

impl<P: exec::SessionProbe> exec::SessionProbe for Counting<P> {
    fn scan(&self, session: i32) -> std::io::Result<Vec<Pid>> {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0.scan(session)
    }

    fn open(&self, pid: Pid) -> std::io::Result<OwnedFd> {
        self.0.open(pid)
    }
}

#[tokio::test]
async fn a_normal_exit_with_nothing_left_scans_once() {
    if !have_setsid() {
        return;
    }
    let probe = Arc::new(Counting(exec::ProcProbe, AtomicU32::new(0)));
    let mut rig = Rig::with_spawner(
        "quick",
        Arc::new(HostSpawner::detect().with_probe(probe.clone())),
    );
    let ran = rig.run(bash("echo hi")).await;
    assert_eq!(ran.output(), "hi\n");
    assert_eq!(probe.1.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn without_a_proc_scan_a_reaped_session_is_left_alone() {
    if !have_setsid() {
        return;
    }
    let probe = Arc::new(Counting(Broken { scan: true }, AtomicU32::new(0)));
    let mut rig = Rig::with_spawner(
        "reaped-group",
        Arc::new(HostSpawner::detect().with_probe(probe.clone())),
    );
    let ran = rig.run(bash("sleep 41 & echo $! > pid.txt")).await;
    let pid: i32 = marker(&rig.path("pid.txt")).await.parse().unwrap();
    let _reap = Reap(vec![pid]);
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
    assert!(
        alive(pid),
        "a reaped leader's group id may be reused, so nothing is signalled through it"
    );
    let after_exit = ran.since_written(&rig.path("pid.txt"));
    assert_eq!(probe.1.load(Ordering::SeqCst), 1, "{after_exit:?}");
}

const LIVE_BUDGET_USD: f64 = 0.03;

fn live_line(n: u32) -> String {
    const WORDS: [&str; 12] = [
        "kestrel", "heron", "osprey", "plover", "curlew", "merlin", "avocet", "dunlin", "gannet",
        "linnet", "shrike", "wren",
    ];
    format!(
        "line {n}: {}-{:04} (batch {n} of 2000, status nominal)",
        WORDS[((n * 37) % 12) as usize],
        (n * 7919) % 10000
    )
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_bash_reads_a_line_back_from_the_saved_output() {
    use crate::provider::anthropic::{AnthropicProvider, Settings};
    let settings = Settings::resolve(|name| std::env::var(name).ok(), None).unwrap();
    let provider = AnthropicProvider::from_settings(&settings).unwrap();
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "live");
    let work = scratch.dir("work");
    let gen_script = "#!/bin/sh\nawk 'BEGIN { split(\"kestrel heron osprey plover curlew merlin avocet dunlin gannet linnet shrike wren\", w, \" \"); for (i = 1; i <= 2000; i++) printf \"line %d: %s-%04d (batch %d of 2000, status nominal)\\n\", i, w[(i * 37) % 12 + 1], (i * 7919) % 10000, i }'\n";
    let mut writer = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "/bin/cat > \"$1\" && /bin/chmod 755 \"$1\"",
            "install",
        ])
        .arg(work.join("gen.sh"))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = writer.stdin.take().expect("stdin is piped");
    std::io::Write::write_all(&mut stdin, gen_script.as_bytes()).unwrap();
    drop(stdin);
    assert!(writer.wait().unwrap().success());
    let mut session = default_session();
    session.model = "claude-sonnet-5-5".into();
    session.effort = Effort::Low;
    let agent = Agent::new(Arc::new(provider), session).with_tools(
        crate::tool::testkit::bundled(&["bash"]),
        Workspace {
            root: work.clone(),
            home: None,
            temp: scratch.dir("tmp"),
        },
    );
    let expected = live_line(1234);
    let (commands, mut events, _task) = crate::agent::spawn(agent);
    commands
        .send(ClientCommand::send("gen", "Run ./gen.sh by itself, with no pipes, redirection or arguments. Its output will be long. Then tell me exactly what is on line 1234 of its output."))
        .await
        .unwrap();
    let mut spent = 0.0;
    let mut stopped = false;
    let mut answer = String::new();
    let mut commands_run = Vec::new();
    let mut previewed = false;
    let outcome = loop {
        let event = tokio::time::timeout(Duration::from_secs(300), events.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            AgentEvent::StepEnded { cost_usd, .. } => {
                spent += cost_usd.unwrap_or(0.0);
                if spent > LIVE_BUDGET_USD && !stopped {
                    stopped = true;
                    commands.send(ClientCommand::Cancel).await.unwrap();
                }
            }
            AgentEvent::TextDelta { text, .. } => answer.push_str(&text),
            AgentEvent::ToolCallRequested { input, .. } => {
                commands_run.push(input["command"].as_str().unwrap_or_default().to_owned());
            }
            AgentEvent::ToolCallFinished { output, .. } => previewed |= !output.inlined,
            AgentEvent::TurnEnded { outcome, .. } => break outcome,
            _ => {}
        }
    };
    drop(commands);
    eprintln!("live: spent ${spent:.4}; commands {commands_run:?}; answer {answer:?}");
    let seen = format!("spent ${spent:.4}; commands {commands_run:?}; answer {answer:?}");
    assert!(
        !stopped,
        "the cost guard stopped the turn after ${spent:.4}"
    );
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}; {seen}"
    );
    assert_eq!(
        commands_run.first().map(|c| c.trim()),
        Some("./gen.sh"),
        "the first call must run gen.sh on its own; {seen}"
    );
    assert!(
        previewed,
        "gen.sh's output should have been previewed; {seen}"
    );
    assert!(
        commands_run.iter().skip(1).any(|c| c.contains(".out")),
        "the model never read the saved file; {seen}"
    );
    let token = expected
        .split(": ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_default()
        .to_owned();
    assert!(answer.contains(&token), "expected {expected:?}; {seen}");
}

#[tokio::test]
async fn bash_keeps_line_numbers() {
    let mut rig = Rig::new("lines-on");
    let flagged = rig.run(bash("seq 1 3")).await;
    assert_eq!(flagged.output(), "1\n2\n3\n");
    assert!(flagged.kept_lines);
}

#[test]
fn the_description_titles_the_call_unless_it_is_blank() {
    let tool = crate::tool::testkit::bash_entry();
    let display = |input: Value| {
        futures::executor::block_on(tool.prepare(&input))
            .unwrap()
            .display()
            .clone()
    };
    let titled =
        display(json!({"command": "cargo test\n--quiet", "description": "  Run the tests \n"}));
    assert_eq!(titled.title.as_deref(), Some("Run the tests"));
    assert_eq!(titled.target, "cargo test");
    assert_eq!(titled.action, ToolAction::Run);
    assert_eq!(display(json!({"command": "cargo test"})).title, None);
    assert_eq!(
        display(json!({"command": "cargo test", "description": " \n "})).title,
        None
    );
    assert_eq!(
        display(json!({"command": "cargo test", "description": ""})).title,
        None
    );
    assert_eq!(tool.meta().writing.as_deref(), Some("a command"));
}

#[tokio::test]
async fn the_cwd_file_is_private_never_reused_and_removed_after_the_call() {
    let mut rig = Rig::new("cwd-file");
    let file = rig.out.path().join("0001-bash.cwd");
    let ran = rig
        .run(bash(&format!("stat -c %a '{}'", file.display())))
        .await;
    assert_eq!(ran.output(), "600\n");
    assert!(!file.exists());
    let taken = rig.out.path().join("0002-bash.cwd");
    fs::write(&taken, "keep").unwrap();
    fs::create_dir(rig.path("sub")).unwrap();
    let ran = rig.run(bash("cd sub")).await;
    assert_eq!(rig.cwd, rig.work);
    assert!(
        ran.run
            .trailer
            .as_deref()
            .is_some_and(|t| t.starts_with("Working directory not tracked: ")),
        "{:?}",
        ran.run.trailer
    );
    assert_eq!(fs::read_to_string(&taken).unwrap(), "keep");
}

const JOB_ENV_CHILD: &str = "WISP_BASH_JOB_ENV_CHILD";

fn conflicting_prompt_value(default: &str) -> String {
    format!("inherited-not-{default}")
}

fn write_askpass_script(path: &Path, marker: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let body = format!(
        "#!/bin/sh\necho invoked >> '{}'\nprintf 'test-cred\\n'\n",
        marker.display()
    );
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn parse_env_nul(raw: &[u8]) -> std::collections::BTreeMap<Vec<u8>, Vec<u8>> {
    raw.split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let eq = entry.iter().position(|b| *b == b'=')?;
            Some((entry[..eq].to_vec(), entry[eq + 1..].to_vec()))
        })
        .collect()
}

fn assert_clean_job_env(raw: &[u8], label: &str) {
    let seen = parse_env_nul(raw);
    for name in crate::jobs::STRIPPED_ENV {
        assert!(
            !seen.contains_key(name.as_bytes()),
            "{label}: {name} was not stripped"
        );
    }
    for (name, value) in crate::jobs::PROMPT_DEFAULTS {
        assert_eq!(
            seen.get(name.as_bytes()).map(Vec::as_slice),
            Some(value.as_bytes()),
            "{label}: {name} mismatch"
        );
    }
    assert_eq!(
        seen.get(wisp_protocol::WISP_JOB.as_bytes())
            .map(Vec::as_slice),
        Some(b"1".as_slice()),
        "{label}: WISP_JOB mismatch"
    );
}

#[test]
fn stripped_env_and_prompt_defaults_apply_to_jobs_and_allow_overrides() {
    let scratch = ScratchDir::new(SCRATCH_PREFIX, "job-env");
    let inherited_script = scratch.root.join("inherited-askpass.sh");
    let inherited_marker = scratch.root.join("inherited-ran");
    write_askpass_script(&inherited_script, &inherited_marker);
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "plugin::shell_tests::bash::job_env_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(JOB_ENV_CHILD, &scratch.root)
        .current_dir(&scratch.root);
    for name in crate::jobs::STRIPPED_ENV {
        if name == "GIT_ASKPASS" {
            command.env(name, &inherited_script);
        } else {
            command.env(name, format!("inherited-{name}"));
        }
    }
    for (name, default) in crate::jobs::PROMPT_DEFAULTS {
        command.env(name, conflicting_prompt_value(default));
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "job_env_child failed ({:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        !inherited_marker.exists(),
        "inherited GIT_ASKPASS script was executed"
    );
}

#[tokio::test]
#[ignore = "a helper that stripped_env_and_prompt_defaults_apply_to_jobs_and_allow_overrides runs with seeded env"]
async fn job_env_child() {
    let Some(root) = std::env::var_os(JOB_ENV_CHILD) else {
        return;
    };
    let root = PathBuf::from(root);
    let inherited_marker = root.join("inherited-ran");
    for name in crate::jobs::STRIPPED_ENV {
        assert!(
            std::env::var_os(name).is_some(),
            "parent test env missing {name}"
        );
    }
    for (name, default) in crate::jobs::PROMPT_DEFAULTS {
        let expected = conflicting_prompt_value(default);
        assert_eq!(
            std::env::var_os(name).as_deref(),
            Some(std::ffi::OsStr::new(&expected)),
            "parent test env missing {name}={expected}"
        );
    }

    let mut rig = Rig::new("job-env-pipe");
    let ran = rig.run(bash("env -0 > env.out")).await;
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
    assert_clean_job_env(&fs::read(rig.path("env.out")).unwrap(), "pipe");

    let spawner = HostSpawner::detect();
    if spawner.has_ctty() {
        let mut tty_rig = Rig::new("job-env-tty");
        let ran = tty_rig
            .run(json!({"command": "env -0 > env.out", "tty": true, "timeout_ms": 10_000}))
            .await;
        assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
        assert_clean_job_env(&fs::read(tty_rig.path("env.out")).unwrap(), "tty");
    }

    let jobs = crate::jobs::Jobs::new(crate::jobs::Processes::default(), Arc::new(spawner.clone()));
    let captured = jobs.capture("env", &["-0".to_owned()], 10_000).await;
    let wisp_plugin_api::host::Captured::Output(bytes) = captured else {
        panic!("capture env -0 failed: {captured:?}");
    };
    assert_clean_job_env(&bytes, "capture");

    let sh_bin = exec::find_program("sh").unwrap();
    let args = [OsString::from("-c"), OsString::from("env -0 > caller.out")];
    let process = spawner
        .spawn(&Launch {
            program: &sh_bin,
            args: &args,
            cwd: &rig.work,
            env: &[
                ("GIT_ASKPASS", "/tmp/caller-askpass"),
                ("GIT_TERMINAL_PROMPT", "1"),
            ],
            stdin: false,
            log: None,
        })
        .unwrap();
    let mut sink = OutputSink::detached("caller-override");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let done = exec::supervise(process, &mut sink, &CancellationToken::new(), deadline).await;
    assert!(done.status.unwrap().success());
    let seen = parse_env_nul(&fs::read(rig.path("caller.out")).unwrap());
    assert_eq!(
        seen.get(b"GIT_ASKPASS".as_slice()).map(Vec::as_slice),
        Some(b"/tmp/caller-askpass".as_slice())
    );
    assert_eq!(
        seen.get(b"GIT_TERMINAL_PROMPT".as_slice())
            .map(Vec::as_slice),
        Some(b"1".as_slice())
    );
    for name in crate::jobs::STRIPPED_ENV {
        if name != "GIT_ASKPASS" {
            assert!(!seen.contains_key(name.as_bytes()), "caller: {name}");
        }
    }
    for (name, value) in crate::jobs::PROMPT_DEFAULTS {
        if name != "GIT_TERMINAL_PROMPT" {
            assert_eq!(
                seen.get(name.as_bytes()).map(Vec::as_slice),
                Some(value.as_bytes()),
                "caller: {name}"
            );
        }
    }
    assert_eq!(
        seen.get(wisp_protocol::WISP_JOB.as_bytes())
            .map(Vec::as_slice),
        Some(b"1".as_slice())
    );

    let ran = rig
        .run(bash(
            r#"GIT_ASKPASS=/tmp/inline-askpass GIT_TERMINAL_PROMPT=1 sh -c 'printf "%s|%s" "$GIT_ASKPASS" "$GIT_TERMINAL_PROMPT"'"#,
        ))
        .await;
    assert_eq!(ran.exit(), Some(ExitStatus::Code { code: 0 }));
    assert_eq!(ran.output(), "/tmp/inline-askpass|1");

    if exec::find_program("git").is_some() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
                    let _ = tokio::io::AsyncWriteExt::write_all(
                        &mut stream,
                        b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                });
            }
        });
        let git_cmd = format!(
            "GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost git -c http.proxy= -c credential.helper= ls-remote http://127.0.0.1:{port}/repo.git"
        );
        let ran = rig.run(bash(&git_cmd)).await;
        assert!(
            matches!(ran.exit(), Some(ExitStatus::Code { code }) if code != 0),
            "unexpected bare git exit: {:?}\n{}",
            ran.exit(),
            ran.text
        );
        assert!(
            !inherited_marker.exists(),
            "inherited GIT_ASKPASS ran during bare git ls-remote"
        );

        let explicit_script = rig.path("explicit-askpass.sh");
        let explicit_marker = rig.path("explicit-ran");
        write_askpass_script(&explicit_script, &explicit_marker);
        let override_cmd = format!("GIT_ASKPASS='{}' {git_cmd}", explicit_script.display());
        let ran = rig.run(bash(&override_cmd)).await;
        assert!(
            matches!(ran.exit(), Some(ExitStatus::Code { code }) if code != 0),
            "unexpected override git exit: {:?}\n{}",
            ran.exit(),
            ran.text
        );
        assert!(
            explicit_marker.exists(),
            "explicit inline GIT_ASKPASS did not run during git ls-remote: {}",
            ran.text
        );
        assert!(
            !inherited_marker.exists(),
            "inherited GIT_ASKPASS ran during explicit override git ls-remote"
        );
        server.abort();
    }
}

#[tokio::test]
async fn long_commands_run_in_foreground_with_cwd_exit_dollar0_and_cleanup() {
    let mut rig = Rig::new("long-fg");
    let base = rig.run(bash("printf '%s' \"$0\"")).await;
    let dollar0 = base.output().to_owned();
    assert!(!dollar0.is_empty());

    for (i, size) in super::LONG_SIZES.into_iter().enumerate() {
        let sub = format!("sub-{i}");
        fs::create_dir(rig.path(&sub)).unwrap();
        let body = format!(
            "cd '{}' && cat && printf 'ok:%s:%s' \"$0\" \"$LINENO\"; exit 7",
            rig.path(&sub).display()
        );
        let ran = rig.run(bash(&super::long_command(size, &body))).await;
        assert_eq!(
            ran.exit(),
            Some(ExitStatus::Code { code: 7 }),
            "{}",
            ran.text
        );
        assert_eq!(ran.output(), format!("ok:{dollar0}:5"));
        assert!(!ran.text.contains(".sh"), "{}", ran.text);
        assert_eq!(rig.cwd, rig.path(&sub));
        let n = rig.shell.calls();
        assert!(!rig.out.path().join(format!("{n:04}-bash.sh")).exists());
        assert!(!rig.out.path().join(format!("{n:04}-bash.cwd")).exists());
    }

    let small_nul = rig.run(bash("echo a\0b")).await;
    assert!(matches!(
        small_nul.run.outcome,
        ToolOutcome::Failed {
            before_start: true,
            ..
        }
    ));
    let big_nul = rig
        .run(bash(&super::long_command(
            super::LONG_SIZES[0],
            "echo a\0b",
        )))
        .await;
    assert_eq!(big_nul.run.outcome, small_nul.run.outcome);
    assert!(
        !rig.out
            .path()
            .join(format!("{:04}-bash.sh", rig.shell.calls()))
            .exists()
    );

    let ready = rig.path("cancel-ready");
    let body = format!("echo ready > '{}'; sleep 30", ready.display());
    let (cancel, _, fire) = cancel_once_written(ready);
    let ran = rig
        .run_while(
            bash(&super::long_command(super::LONG_SIZES[0], &body)),
            cancel,
            fire,
        )
        .await;
    assert_eq!(ran.run.outcome, ToolOutcome::Cancelled, "{}", ran.text);
    assert!(
        !rig.out
            .path()
            .join(format!("{:04}-bash.sh", rig.shell.calls()))
            .exists()
    );
}
