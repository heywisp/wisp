mod start;

use serde_json::{Map, Value, json};
use wisp_plugin_api::host::{
    CallContext, HoldSpec, Host, JobEnd, JobError, JobOutput, Mode, SpawnSpec, StoppedBy, Waited,
};
use wisp_plugin_api::tools::{
    ClientTool, Effect, Purpose, Tags, Timeouts, ToolDef, ToolKind, ToolMeta,
};
use wisp_plugin_api::types::{
    Action, Completed, Display, ExitStatus, Fact, Failure, Outcome, OutputKind, ToolRun,
};
use wisp_tool_text::output::{Captured, ResultStyle, group, problem_of, result_text_with};
use wisp_tool_text::question::SECRET_TOOL;
use wisp_tool_text::terminal::DEFAULT_SIZE;

use crate::{BASH, Timing};

pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 600_000;
pub const MAX_COMMAND_BYTES: usize = 256 * 1024;
pub const SYSTEM_LINE: &str =
    "You can run shell commands on the user's computer with the bash tool.";

pub const NON_INTERACTIVE: [(&str, &str); 9] = [
    ("GH_PAGER", "cat"),
    ("GIT_EDITOR", "false"),
    ("GIT_PAGER", "cat"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("MANPAGER", "cat"),
    ("NO_COLOR", "1"),
    ("PAGER", "cat"),
    ("SSH_ASKPASS_REQUIRE", "never"),
    ("TERM", "dumb"),
];

pub const TERMINAL_ENV: [(&str, &str); 9] = [
    ("GH_PAGER", "cat"),
    ("GIT_EDITOR", "false"),
    ("GIT_PAGER", "cat"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("MANPAGER", "cat"),
    ("NO_COLOR", "1"),
    ("PAGER", "cat"),
    ("SSH_ASKPASS_REQUIRE", "never"),
    ("TERM", "xterm-256color"),
];

const DESCRIPTION: &str = "Run a bash command on the user's computer and get its exit code and combined stdout and stderr. The working directory carries over between calls (a cd persists); variables, aliases and functions do not. Commands run as the user with no sandbox and no approval step. By default a command runs in the foreground with empty stdin and no terminal, and the call waits for it to finish, so don't run editors, pagers or anything that prompts that way: pass flags such as --yes or --no-pager, or set tty. A command is stopped when it runs longer than timeout_ms, 120000 by default; give a larger value for slow builds or tests, or 0 for no time limit. A foreground command still running after 2 minutes is handed back to you as a numbered process with its output so far; it keeps running, and its time limit still applies. Set tty to run the command in a terminal, for REPLs, prompts, full-screen programs or other agents' terminal interfaces; in the foreground, a terminal command also returns once its screen has been still for 10 seconds, since it may be waiting for input. Set run_in_background, with or without tty, to start a server, a watcher or any long job and get its process number within a few seconds; it then has no time limit unless you give timeout_ms. For REPLs and other programs you will keep talking to, use run_in_background, or timeout_ms 0 in the foreground. Never ask the user to paste a password or other secret into the chat, where it would reach you and the session log; ";
const DESCRIPTION_END: &str = "Check one variable by name and list processes by number and name; don't print the whole environment or processes' arguments (env, ps aux), where any secret could reach you and the session log. When a background process ends while no turn is running, you are told in a new turn, so end your turn rather than wait for a long job. A result that gives a process number means the process tool can wait for it, read or search its output, show its screen, type into it or stop it. Programs left running with & or nohup are stopped when the command that started them exits; run them with run_in_background instead. Background processes are stopped when Wisp exits; for a server or other service that must stay up after that, also set keep_running. The full output of every call is saved to a file whose path is in the result; long output is shown only as a preview, so read the parts you need with the process tool, or from that file with sed -n, grep or tail.";

const WAIT_FAILED: &str = "could not wait for it: ";

pub fn parse_version(text: &str) -> Option<String> {
    let first = text.lines().next()?.trim();
    let rest = first.strip_prefix("GNU bash, version ")?;
    let numbers: Vec<&str> = rest
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()?
        .split('.')
        .take(2)
        .collect();
    (numbers.len() == 2 && numbers.iter().all(|n| !n.is_empty()))
        .then(|| format!("GNU bash {}", numbers.join(".")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashInput {
    pub command: String,
    pub timeout_ms: u64,
    pub limit_ms: Option<u64>,
    pub background: bool,
    pub tty: bool,
    pub keep_running: bool,
}

fn meta() -> ToolMeta {
    ToolMeta {
        effect: Effect::NonIdempotent,
        tags: Tags {
            irreversible: true,
            external_message: true,
            reads_untrusted: true,
            ..Tags::default()
        },
        parallel_safe: false,
        timeouts: Timeouts {
            default_ms: DEFAULT_TIMEOUT_MS,
            max_ms: Some(MAX_TIMEOUT_MS),
        },
        saves_output: true,
        needs_workspace: true,
        ends_reply: false,
        asks_user: false,
        reports_jobs: true,
        writing: Some("a command".to_owned()),
        purpose: Purpose::Run,
    }
}

pub(crate) fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "The bash command or script to run."
            },
            "description": {
                "type": "string",
                "description": "Five to ten words on what the command does, shown to the user."
            },
            "keep_running": {
                "type": "boolean",
                "description": "With run_in_background: keep the process running after Wisp exits, for a server or service that must stay up. Its output then goes to a log file named in the result."
            },
            "run_in_background": {
                "type": "boolean",
                "description": "Start the command and return within a few seconds with its process number and first output, instead of waiting for it to finish. Without tty its stdin stays open for the process tool. It is stopped when Wisp exits unless keep_running is set."
            },
            "timeout_ms": {
                "type": "integer",
                "description": "The command's time limit in milliseconds: it is stopped once it has run this long. 0 means no limit. Defaults to 120000 in the foreground and to no limit with run_in_background. A foreground call returns after 2 minutes at most; a longer limit keeps applying to the process it hands back."
            },
            "tty": {
                "type": "boolean",
                "description": "Run the command in a terminal, so it can prompt, draw a full screen and take keys through the process tool. Combine with run_in_background to start it without waiting."
            }
        },
        "required": ["command"],
        "additionalProperties": false
    })
}

pub(crate) fn def(context_line: Option<String>, secret: bool) -> ToolDef {
    let ask = if secret {
        format!(
            "when a command needs one, call {SECRET_TOOL}, which has the user enter it in a box and types it into the process without showing it to you; when a terminal command needs something else only a person can do, such as a browser login or a program you can't drive, call process with ask_user_to_attach so they type into it directly."
        )
    } else {
        "when a terminal command asks for one, or needs something only a person can do, such as a browser login or a program you can't drive, call process with ask_user_to_attach so they type into it directly.".to_owned()
    };
    ToolDef {
        name: BASH.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: format!("{DESCRIPTION}{ask} {DESCRIPTION_END}"),
            input_schema: schema().to_string(),
            meta: meta(),
        }),
        system_line: Some(SYSTEM_LINE.to_owned()),
        context_line,
    }
}

pub(crate) fn parse(input: &Value) -> Result<BashInput, String> {
    let map = input
        .as_object()
        .ok_or_else(|| "input must be an object".to_owned())?;
    if let Some(key) = map.keys().find(|k| {
        !matches!(
            k.as_str(),
            "command" | "description" | "keep_running" | "timeout_ms" | "run_in_background" | "tty"
        )
    }) {
        return Err(format!("unknown field `{key}`"));
    }
    let command = map
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "`command` must be a string".to_owned())?;
    if command.trim().is_empty() {
        return Err("`command` is empty".to_owned());
    }
    if command.len() > MAX_COMMAND_BYTES {
        return Err(format!(
            "`command` is {} bytes; the limit is {} bytes",
            group(command.len() as u64),
            group(MAX_COMMAND_BYTES as u64)
        ));
    }
    if let Some(description) = map.get("description")
        && !description.is_string()
    {
        return Err("`description` must be a string".to_owned());
    }
    let background = flag(map, "run_in_background")?;
    let tty = flag(map, "tty")?;
    let keep_running = flag(map, "keep_running")?;
    let limit_ms = match map.get("timeout_ms") {
        None | Some(Value::Null) => (!background).then_some(DEFAULT_TIMEOUT_MS),
        Some(value) => limit(value)?,
    };
    Ok(BashInput {
        command: command.to_owned(),
        timeout_ms: limit_ms.unwrap_or(0),
        limit_ms,
        background,
        tty,
        keep_running,
    })
}

fn title(input: &Value) -> Option<String> {
    input
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(str::to_owned)
}

pub(crate) fn display(parsed: &BashInput, input: &Value) -> Display {
    Display {
        action: Action::Run,
        verbs: None,
        target: first_line(&parsed.command),
        output: OutputKind::Text,
        view: None,
        title: title(input),
    }
}

fn flag(map: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("`{key}` must be true or false")),
    }
}

fn limit(value: &Value) -> Result<Option<u64>, String> {
    let invalid = || "`timeout_ms` must be 0 or more milliseconds".to_owned();
    if value.as_i64().is_some_and(|n| n < 0) {
        return Err(invalid());
    }
    let ms = value.as_u64().ok_or_else(invalid)?;
    Ok((ms > 0).then_some(ms))
}

pub(crate) fn time_limit_ms(input: &Value, timing: &Timing) -> Option<u64> {
    match parse(input) {
        Ok(parsed) if parsed.background => Some(timing.start_max_ms),
        Ok(parsed) => parsed.limit_ms,
        Err(_) => Some(MAX_TIMEOUT_MS),
    }
}

pub(crate) fn movable(input: &Value) -> bool {
    parse(input).is_ok_and(|parsed| !parsed.background)
}

pub fn first_line(command: &str) -> String {
    command
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim_end()
        .to_owned()
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub fn script(command: &str, cwd_file: Option<&str>) -> String {
    match cwd_file {
        Some(file) => {
            let file = quote(file);
            let action =
                format!("{{ builtin test -f {file} && builtin pwd -P >| {file}; }} 2>/dev/null");
            format!("trap {} EXIT\n{command}", quote(&action))
        }
        None => command.to_owned(),
    }
}

fn environment(tty: bool) -> Vec<(String, String)> {
    let pairs: &[(&str, &str)] = if tty { &TERMINAL_ENV } else { &NON_INTERACTIVE };
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn tracked_dir(file: &str, ran_in: &str) -> Result<Option<String>, String> {
    let bytes = std::fs::read(file);
    let _ = std::fs::remove_file(file);
    let Ok(bytes) = bytes else {
        return Ok(None);
    };
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(format!(
            "{} is not valid UTF-8",
            String::from_utf8_lossy(bytes)
        ));
    };
    let dir = std::path::PathBuf::from(text);
    if !dir.is_absolute() {
        return Ok(None);
    }
    let same = std::fs::canonicalize(ran_in).is_ok_and(|c| c == dir);
    Ok(Some(if same {
        ran_in.to_owned()
    } else {
        dir.display().to_string()
    }))
}

fn with_notes(mut run: ToolRun, notes: Vec<String>) -> ToolRun {
    if !notes.is_empty() {
        run.trailer = Some(notes.join("\n"));
    }
    run
}

fn spawn_message(error: JobError, input: &BashInput) -> String {
    match error {
        JobError::Invalid(message) if input.tty || input.keep_running => message,
        JobError::Invalid(message) | JobError::Io(message) => {
            format!("could not start bash: {message}")
        }
        other => format!("could not start bash: {other:?}"),
    }
}

pub(crate) fn captured(output: JobOutput) -> Captured {
    Captured {
        head: output.head,
        tail: output.tail,
        bytes: output.bytes,
        lines: output.lines,
        path: output.file.path.map(std::path::PathBuf::from),
        problem: output.file.problem.as_ref().map(problem_of),
    }
}

fn exit_of(end: &JobEnd) -> Option<ExitStatus> {
    match end {
        JobEnd::Exited(exit) => Some(*exit),
        _ => None,
    }
}

fn foreground_outcome(end: &JobEnd, timeout_ms: u64) -> Outcome {
    match end {
        JobEnd::Exited(exit) => Outcome::Completed(Completed {
            is_error: false,
            exit: Some(*exit),
        }),
        JobEnd::Stopped(StoppedBy::TimeLimit(_)) => Outcome::TimedOut(timeout_ms),
        JobEnd::Stopped(_) | JobEnd::Lost(_) => Outcome::Cancelled,
        JobEnd::Failed(message) => Outcome::Failed(Failure {
            message: match message.strip_prefix(WAIT_FAILED) {
                Some(why) => format!("could not wait for bash: {why}"),
                None => message.clone(),
            },
            before_start: false,
        }),
    }
}

pub(crate) struct Bash<'a> {
    pub(crate) host: &'a dyn Host,
    pub(crate) path: Option<String>,
    pub(crate) timing: Timing,
}

impl Bash<'_> {
    fn spec(
        &self,
        path: &str,
        input: &BashInput,
        script: String,
        ctx: &dyn CallContext,
    ) -> SpawnSpec {
        SpawnSpec {
            program: path.to_owned(),
            args: vec![
                "--noprofile".to_owned(),
                "--norc".to_owned(),
                "-c".to_owned(),
                script,
            ],
            cwd: ctx.cwd(),
            env: environment(input.tty),
            mode: if input.tty {
                Mode::Terminal
            } else {
                Mode::Pipe
            },
            size: input.tty.then_some(DEFAULT_SIZE),
            stdin: input.background && !input.tty && !input.keep_running,
            background: input.background,
            limit_ms: input.limit_ms,
            label: first_line(&input.command),
            keep: input.keep_running,
        }
    }

    pub(crate) async fn run(&self, input: BashInput, ctx: &dyn CallContext) -> ToolRun {
        let mut notes = Vec::new();
        let Some(path) = self.path.clone() else {
            return with_notes(
                ToolRun::failed_to_start("bash was not found on PATH"),
                notes,
            );
        };
        if input.background {
            let spec = self.spec(&path, &input, script(&input.command, None), ctx);
            return with_notes(self.background(spec, &input, ctx).await, notes);
        }
        let cwd_file = match ctx.private_file("cwd") {
            Ok(path) => Some(path),
            Err(reason) => {
                notes.push(format!("Working directory not tracked: {reason}"));
                None
            }
        };
        let remove = |file: &Option<String>| {
            if let Some(file) = file {
                let _ = std::fs::remove_file(file);
            }
        };
        let spec = self.spec(
            &path,
            &input,
            script(&input.command, cwd_file.as_deref()),
            ctx,
        );
        let job = match self.host.spawn(spec, ctx).await {
            Ok(job) => job,
            Err(error) => {
                remove(&cwd_file);
                return with_notes(
                    ToolRun::failed_to_start(spawn_message(error, &input)),
                    notes,
                );
            }
        };
        let spec = HoldSpec {
            quiet_ms: input.tty.then_some(self.timing.hand_back_ms),
            yield_ms: input
                .limit_ms
                .is_none_or(|limit| limit > self.timing.yield_ms)
                .then_some(self.timing.yield_ms),
        };
        let end = match job.hold(spec).await {
            Waited::Ended(end) => end,
            Waited::Moved(id) => {
                remove(&cwd_file);
                return with_notes(start::moved(self.host, job.as_ref(), id).await, notes);
            }
            Waited::NeedsInput(back) => {
                remove(&cwd_file);
                let run = start::handed_back(self.host, job.as_ref(), back, ctx).await;
                return with_notes(run, notes);
            }
            Waited::StillRunning(id) => {
                remove(&cwd_file);
                let run =
                    start::still_running(self.host, job.as_ref(), id, self.timing.yield_ms, ctx)
                        .await;
                return with_notes(run, notes);
            }
            Waited::Matched
            | Waited::Quiet
            | Waited::TimedOut
            | Waited::Cancelled
            | Waited::Steered => job
                .info()
                .await
                .end
                .unwrap_or(JobEnd::Stopped(StoppedBy::User)),
        };
        let captured = captured(job.output().await);
        let previewed = (input.tty || !captured.inlined())
            && matches!(
                end,
                JobEnd::Exited(_) | JobEnd::Stopped(StoppedBy::TimeLimit(_))
            );
        let number = if previewed {
            job.background().await
        } else {
            None
        };
        let cwd = ctx.cwd();
        let tracked = match cwd_file.as_deref().map(|file| tracked_dir(file, &cwd)) {
            Some(Ok(dir)) => dir,
            Some(Err(reason)) => {
                notes.push(format!("Working directory not tracked: {reason}"));
                None
            }
            None => None,
        };
        let outcome = foreground_outcome(&end, input.timeout_ms);
        let mut run = ToolRun::new(outcome);
        if let Some(ExitStatus::Code(code)) = exit_of(&end) {
            run.facts.push(Fact::Exit(code));
        }
        run.cwd = tracked;
        let timed_out = matches!(run.outcome, Outcome::TimedOut(_));
        if let Some(id) = number {
            run.facts.push(Fact::Process(id));
        }
        if number.is_some() || timed_out {
            let style = ResultStyle {
                time_limit: true,
                process: number,
                terminal: input.tty,
            };
            let home = self.host.home().map(std::path::PathBuf::from);
            run.result = Some(result_text_with(
                &run.outcome,
                &captured,
                home.as_deref(),
                &[],
                style,
            ));
        }
        with_notes(run, notes)
    }
}
