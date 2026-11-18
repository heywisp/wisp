use std::path::PathBuf;

use wisp_plugin_api::host::{
    CallContext, HandedBack, Host, Job, JobEnd, Settled, SpawnSpec, StoppedBy,
};
use wisp_plugin_api::types::{
    BackgroundReason, Completed, ExitStatus, Fact, Failure, Outcome, ToolRun,
};
use wisp_tool_text::jobs::{ended_sentence_named, kept_result_line};
use wisp_tool_text::output::{group, seconds, time_limit_text};
use wisp_tool_text::terminal::size_text;

use super::{Bash, BashInput, spawn_message};
use crate::process::echo_off_line;
use crate::process::view::{screen_rows_text, whole_text};

const WHOLE: u32 = u32::MAX;

fn home(host: &dyn Host) -> Option<PathBuf> {
    host.home().map(PathBuf::from)
}

fn numbered(mut run: ToolRun, id: u32, text: String) -> ToolRun {
    run.facts.push(Fact::Process(id));
    run.result = Some(text);
    run
}

async fn output_lines(host: &dyn Host, job: &dyn Job, label: &str) -> Vec<String> {
    let read = job.tail(WHOLE).await;
    whole_text(&read, label, home(host).as_deref())
}

fn time_left(id: u32, limit_ms: Option<u64>, left_ms: Option<u64>) -> String {
    match (limit_ms, left_ms) {
        (Some(limit), Some(left)) => format!(
            "It has {} s left of its {} time limit; to give it longer, call process with id {id} and timeout_ms (0 for no limit).",
            group(left / 1000),
            seconds(limit)
        ),
        _ => "It has no time limit.".to_owned(),
    }
}

pub(super) async fn moved(host: &dyn Host, job: &dyn Job, id: u32) -> ToolRun {
    let text = std::iter::once(format!(
        "Moved to the background as process {id} by the user; it keeps running with no time limit."
    ))
    .chain(output_lines(host, job, "Output so far").await)
    .chain(std::iter::once(format!(
        "Use the process tool with id {id} to wait for it, read or search its output or stop it."
    )))
    .collect::<Vec<_>>()
    .join("\n");
    numbered(
        ToolRun::new(Outcome::Backgrounded(BackgroundReason::MovedByUser)),
        id,
        text,
    )
}

pub(super) async fn handed_back(
    host: &dyn Host,
    job: &dyn Job,
    back: HandedBack,
    ctx: &dyn CallContext,
) -> ToolRun {
    let id = back.id;
    let (size, screen) = match job.screen().await {
        Some(screen) => (size_text(screen.size), screen_rows_text(&screen.rows)),
        None => (String::new(), Vec::new()),
    };
    let info = job.info().await;
    let text = [
        format!(
            "Process {id}'s screen has not changed for {}, so it may be waiting for input; it keeps running in its {size} terminal.",
            seconds(back.quiet_ms)
        ),
        time_left(id, info.limit_ms, info.left_ms),
    ]
    .into_iter()
    .chain(screen)
    .chain([
        format!(
            "The working directory stays {}.",
            host.shorten(&ctx.cwd())
        ),
        format!("Use the process tool with id {id} to type into it, wait for it or stop it."),
    ])
    .chain(echo_off_line(info.echo_off, host))
    .collect::<Vec<_>>()
    .join("\n");
    numbered(
        ToolRun::new(Outcome::Backgrounded(BackgroundReason::Quiet)),
        id,
        text,
    )
}

pub(super) async fn still_running(
    host: &dyn Host,
    job: &dyn Job,
    id: u32,
    yield_ms: u64,
    ctx: &dyn CallContext,
) -> ToolRun {
    let info = job.info().await;
    let text = [
        format!(
            "Process {id} is still running after {}, so the call returned; it keeps running in the background, and is stopped if Wisp exits before it ends.",
            seconds(yield_ms)
        ),
        time_left(id, info.limit_ms, info.left_ms),
    ]
    .into_iter()
    .chain(output_lines(host, job, "Output so far").await)
    .chain([
        format!(
            "The working directory stays {}.",
            host.shorten(&ctx.cwd())
        ),
        format!(
            "Use the process tool with id {id} to wait for it, read or search its output or stop it."
        ),
    ])
    .collect::<Vec<_>>()
    .join("\n");
    numbered(
        ToolRun::new(Outcome::Backgrounded(BackgroundReason::StillRunning)),
        id,
        text,
    )
}

fn ended_outcome(end: &JobEnd) -> (Outcome, Option<String>) {
    match end {
        JobEnd::Exited(exit) => (
            Outcome::Completed(Completed {
                is_error: false,
                exit: Some(*exit),
            }),
            None,
        ),
        JobEnd::Stopped(StoppedBy::TimeLimit(after_ms)) => (
            Outcome::TimedOut(*after_ms),
            Some(time_limit_text(*after_ms)),
        ),
        JobEnd::Failed(message) => (
            Outcome::Failed(Failure {
                message: message.clone(),
                before_start: false,
            }),
            None,
        ),
        JobEnd::Stopped(_) | JobEnd::Lost(_) => (Outcome::Cancelled, None),
    }
}

impl Bash<'_> {
    pub(super) async fn background(
        &self,
        spec: SpawnSpec,
        input: &BashInput,
        ctx: &dyn CallContext,
    ) -> ToolRun {
        let target = spec.label.clone();
        let job = match self.host.spawn(spec, ctx).await {
            Ok(job) => job,
            Err(error) => return ToolRun::failed_to_start(spawn_message(error, input)),
        };
        let waited = job
            .settle(self.timing.start_quiet_ms, self.timing.start_max_ms)
            .await;
        let info = job.info().await;
        let Some(id) = info.id else {
            return ToolRun::failed_to_start("the background job has no number");
        };
        let output = if info.full_screen {
            match job.screen().await {
                Some(screen) => screen_rows_text(&screen.rows),
                None => Vec::new(),
            }
        } else {
            output_lines(self.host, job.as_ref(), "Output").await
        };
        let ended = info.end.clone();
        let (outcome, first, last) = match &ended {
            None => (
                Outcome::Backgrounded(BackgroundReason::Started),
                match info.size {
                    Some(size) => format!(
                        "Process {id} is running in the background in a {} terminal: {target}",
                        size_text(size)
                    ),
                    None => format!("Process {id} is running in the background: {target}"),
                },
                match &info.log {
                    Some(log) if info.keep => vec![
                        format!(
                            "Use the process tool with id {id} to wait for it, read or search its output or stop it."
                        ),
                        kept_result_line(&self.host.shorten(log)),
                    ],
                    _ => vec![format!(
                        "Use the process tool with id {id} to wait for it, read or search its output, send it input or stop it."
                    )],
                },
            ),
            Some(end) => {
                let (outcome, extra) = ended_outcome(end);
                (
                    outcome,
                    ended_sentence_named(&format!("Process {id} ({target})"), end),
                    extra
                        .into_iter()
                        .chain(std::iter::once(format!(
                            "Use the process tool with id {id} to read or search its output."
                        )))
                        .collect(),
                )
            }
        };
        let stopped = (waited == Settled::Cancelled && ended.is_none())
            .then(|| format!("[stopped by the user]; process {id} keeps running."));
        let text = std::iter::once(first)
            .chain(output)
            .chain(last)
            .chain(stopped)
            .chain(echo_off_line(info.echo_off, self.host))
            .collect::<Vec<_>>()
            .join("\n");
        if ended.is_some() {
            self.host.mark_reported(id, ctx);
        }
        let mut run = ToolRun::new(outcome);
        if let Some(JobEnd::Exited(ExitStatus::Code(code))) = ended {
            run.facts.push(Fact::Exit(code));
        }
        numbered(run, id, text)
    }
}
