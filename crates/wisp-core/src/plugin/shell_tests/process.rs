use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wisp_plugin_shell::{STEERED_NOTE, Timing, hidden_line};
use wisp_protocol::ToolOutcome;
use wisp_tool_text::output::{Captured, invalid_input_text};

use super::{Ctl, Shell};
use crate::call::user::UserChannel;
use crate::call::{OutputDir, OutputSink};
use crate::jobs::{Carrier, Handle, Processes, SHUTDOWN_LIMIT};
use crate::plugin::KernelHost;
use crate::test_guard::{
    ScratchDir, alive, gone, has_ctty, have_bash, hold, pid_in, runner_alive, until, wait_for,
};
use crate::tool::{ToolEntry, ToolRun};

mod attach;
mod kept;
mod live;
mod notes;
mod restored;
mod rows;
mod terminal;
mod typing;
mod views;
mod yielding;

pub(super) const NAME: &str = "process";
pub(super) const PATIENCE: Duration = Duration::from_secs(20);
pub(super) const QUIET: Duration = Duration::from_millis(wisp_plugin_shell::START_QUIET_MS);
pub(super) const WINDOW: Duration = Duration::from_secs(3);
pub(super) const START_MAX: Duration = Duration::from_millis(wisp_plugin_shell::START_MAX_MS);

pub(super) fn entry(name: &str) -> ToolEntry {
    crate::tool::testkit::shell(Timing::default())
        .get(name)
        .cloned()
        .unwrap()
}

pub(super) fn scratch(name: &str) -> ScratchDir {
    let scratch = ScratchDir::new("wisp-process-test-", name);
    scratch.dir("work");
    scratch.dir("tmp");
    scratch
}

pub(super) struct Done {
    pub(super) run: ToolRun,
    pub(super) text: String,
    pub(super) number: Option<u32>,
    pub(super) captured: Captured,
    pub(super) elapsed: Duration,
    pub(super) ends: Vec<String>,
}

impl Done {
    pub(super) fn invalid(&self) -> Option<&str> {
        match &self.run.outcome {
            ToolOutcome::InvalidInput { message } => Some(message),
            _ => None,
        }
    }
}

pub(super) struct Rig {
    pub(super) scratch: ScratchDir,
    pub(super) work: PathBuf,
    out: OutputDir,
    pub(super) shell: Shell,
    pub(super) processes: Processes,
    pub(super) user: UserChannel,
}

pub(super) fn window(quiet: Duration, max: Duration) -> Timing {
    let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap();
    Timing {
        start_quiet_ms: ms(quiet),
        start_max_ms: ms(max),
        ..Timing::default()
    }
}

impl Rig {
    pub(super) fn new(name: &str) -> Self {
        Self::with_window(name, QUIET, WINDOW)
    }

    pub(super) fn with_window(name: &str, quiet: Duration, max: Duration) -> Self {
        Self::with_timing(name, window(quiet, max))
    }

    pub(super) fn with_timing(name: &str, timing: Timing) -> Self {
        Self::with_host(name, KernelHost::new(), timing)
    }

    pub(super) fn with_host(name: &str, host: KernelHost, timing: Timing) -> Self {
        let scratch = scratch(name);
        let work = scratch.root.join("work");
        let out = OutputDir::new(&scratch.root.join("tmp"), "s1");
        let shell = Shell::new(host, timing, out.clone(), &work);
        Self {
            work,
            out,
            processes: shell.processes.clone(),
            shell,
            user: UserChannel::none(),
            scratch,
        }
    }

    pub(super) fn file(&self, n: u32) -> PathBuf {
        self.out.path().join(OutputDir::file_name(n, "bash"))
    }

    pub(super) fn next_call(&self) -> String {
        format!("call-{}", self.shell.calls() + 1)
    }

    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.work.join(name)
    }

    pub(super) async fn bash(&mut self, input: Value) -> Done {
        self.bash_with(input, Ctl::default()).await
    }

    pub(super) async fn bash_with(&mut self, input: Value, ctl: Ctl) -> Done {
        self.call("bash", input, ctl).await
    }

    pub(super) async fn process(&mut self, input: Value) -> Done {
        self.process_with(input, Ctl::default()).await
    }

    pub(super) async fn process_with(&mut self, input: Value, ctl: Ctl) -> Done {
        self.call(NAME, input, ctl).await
    }

    pub(super) async fn call(&mut self, name: &str, input: Value, ctl: Ctl) -> Done {
        self.shell.user = self.user.clone();
        let started = Instant::now();
        let deadline = self.shell.limit_of(name, &input);
        let work = self.work.clone();
        match self
            .shell
            .call(name, &input, &work, ctl, deadline, async {})
            .await
        {
            Ok(called) => Done {
                run: called.run,
                text: called.text,
                number: called.number,
                captured: called.captured,
                elapsed: started.elapsed(),
                ends: called.ends,
            },
            Err(message) => Done {
                text: invalid_input_text(&message),
                run: ToolRun::new(ToolOutcome::InvalidInput { message }),
                number: None,
                captured: Captured::default(),
                elapsed: started.elapsed(),
                ends: Vec::new(),
            },
        }
    }

    pub(super) async fn finish(self) {
        self.processes.shutdown().await;
    }
}

pub(super) fn bash(command: &str) -> Value {
    json!({ "command": command })
}

pub(super) fn background(command: &str) -> Value {
    json!({ "command": command, "run_in_background": true })
}

pub(super) fn terminal(command: &str) -> Value {
    json!({ "command": command, "tty": true, "run_in_background": true })
}

pub(super) async fn until_hidden(rig: &Rig, id: u32, want: bool) {
    let handle = rig.processes.handle(id).unwrap();
    assert!(
        until(|| handle.state.hidden() == want).await,
        "hidden input never became {want}"
    );
}

pub(super) async fn until_file(path: &Path, want: &str) {
    assert!(
        until(|| fs::read_to_string(path).unwrap_or_default().contains(want)).await,
        "{want:?} never reached {}",
        path.display()
    );
}

#[test]
fn the_process_tool_comes_with_bash() {
    let names = |on: &[&str]| crate::tool::testkit::bundled(on).names();
    assert_eq!(names(&["bash"]), ["bash", NAME]);
    assert!(!names(&["show_diff"]).contains(&NAME.to_owned()));
}

#[test]
fn the_process_tool_has_bash_metadata_and_asks_the_user() {
    let shell = crate::tool::testkit::shell(Timing::default());
    let tool = shell.get(NAME).unwrap();
    let bash = shell.get("bash").unwrap();
    assert_eq!(tool.meta().tags, bash.meta().tags);
    assert_eq!(tool.meta().effect, crate::tool::EffectClass::NonIdempotent);
    assert!(!tool.meta().parallel_safe);
    assert!(!tool.meta().saves_output);
    assert!(!tool.meta().needs_workspace);
    assert!(tool.meta().asks_user);
    let properties = tool.spec().input_schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        properties,
        [
            "ask_user_to_attach",
            "context",
            "description",
            "from",
            "id",
            "input",
            "keys",
            "lines",
            "pattern",
            "quiet_ms",
            "read",
            "signal",
            "size",
            "timeout_ms",
            "until",
            "wait_ms"
        ]
    );
    assert_eq!(
        tool.spec().input_schema["properties"]["read"]["enum"],
        json!(["new", "tail", "lines", "search", "screen"])
    );
}

#[test]
fn the_process_tools_time_limit_covers_its_wait_and_its_work() {
    let tool = crate::tool::testkit::shell(Timing::default())
        .get(NAME)
        .cloned()
        .unwrap();
    let limit = |input: Value| futures::executor::block_on(tool.time_limit(&input));
    assert_eq!(limit(json!({"id": 1})), Some(Duration::ZERO));
    assert_eq!(
        limit(json!({"id": 1, "wait_ms": 5000})),
        Some(Duration::from_secs(5))
    );
    assert_eq!(
        limit(json!({"id": 1, "input": "x"})),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        limit(json!({"id": 1, "read": "search", "pattern": "x"})),
        Some(Duration::from_millis(
            wisp_plugin_shell::SEARCH_ALLOWANCE_MS
        ))
    );
    assert_eq!(
        limit(json!({"id": 1, "signal": "kill"})),
        Some(Duration::from_millis(wisp_plugin_shell::KILL_ALLOWANCE_MS))
    );
    assert_eq!(
        limit(json!({"id": 1, "signal": "interrupt"})),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        limit(json!({"id": 1, "wait_ms": 900_000})),
        Some(Duration::from_secs(600))
    );
    assert!(!futures::executor::block_on(
        tool.movable(&json!({"id": 1}))
    ));
}
