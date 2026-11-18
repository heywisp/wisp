use std::path::PathBuf;

use wisp_plugin_api::host::{
    CallContext, Host, Job, JobEnd, JobError, Mode, Typing, WaitFor, Waited, Written,
};
use wisp_plugin_api::types::{
    AskReply, Fact, Failure, Outcome, ReadKind, SignalKind, ToolRun, Typed,
};
use wisp_tool_text::jobs::{
    KEPT_RESTORED_LIST, KEPT_WORDS, LIST_TARGET_CHARS, kept_no_limit, no_process,
};
use wisp_tool_text::output::{count, cut_chars, group, invalid_input_text, seconds};
use wisp_tool_text::terminal::size_text;

use super::input::{self, Call};
use super::{STEERED_NOTE, view};
use crate::{START_MAX_MS, START_QUIET_MS};

pub(crate) const WRITE_AT_LEAST_MS: u64 = 1_000;
const STOPPED: &str = "[stopped by the user]";
const OUT_OF_TIME: &str = "[stopped: the call reached its time limit]";
pub const LINE_LIMIT: usize = 4095;
pub const NO_ONE_TO_ASK: &str = "Not asked: no one can type into processes in this session. Don't ask for the secret in the chat or from another agent; say what the process needs.";

pub(crate) struct Env<'a> {
    pub(crate) host: &'a dyn Host,
    pub(crate) ctx: &'a dyn CallContext,
}

impl Env<'_> {
    pub(crate) fn home(&self) -> Option<PathBuf> {
        self.host.home().map(PathBuf::from)
    }

    fn stop_text(&self) -> &'static str {
        if self
            .ctx
            .deadline_ms()
            .is_some_and(|at| self.host.elapsed_ms() >= at)
        {
            OUT_OF_TIME
        } else {
            STOPPED
        }
    }

    fn report(&self, id: u32, end: Option<&JobEnd>) {
        if end.is_some() {
            self.host.mark_reported(id, self.ctx);
        }
    }
}

pub fn line_limit_text(id: u32, longest: u64) -> String {
    format!(
        "Process {id}'s terminal is in line mode, which keeps only the first {} bytes of a line and drops the rest; this input has a line of {}. Write long text to a file and pass its path, or start the program with run_in_background and no tty, so its stdin is a pipe.",
        group(LINE_LIMIT as u64),
        count(longest, "byte")
    )
}

pub(crate) fn refused(message: String) -> ToolRun {
    let text = invalid_input_text(&message);
    ToolRun::new(Outcome::InvalidInput(message)).with_result(text)
}

pub(crate) fn refused_as(text: String) -> ToolRun {
    ToolRun::new(Outcome::InvalidInput(text.clone())).with_result(text)
}

fn failed(message: String, text: String) -> ToolRun {
    ToolRun::new(Outcome::Failed(Failure {
        message,
        before_start: false,
    }))
    .with_result(text)
}

pub(crate) fn answered(id: Option<u32>, text: String) -> ToolRun {
    let mut run = ToolRun::completed().with_result(text);
    if let Some(id) = id {
        run.facts.push(Fact::Process(id));
    }
    run
}

pub(crate) fn has_ended(id: u32) -> ToolRun {
    refused(format!(
        "process {id} has ended, so it takes no input, signals, size or time limit"
    ))
}

pub(crate) fn screen_of_pipe(id: u32) -> ToolRun {
    refused(format!(
        "read screen only applies to a terminal process, and process {id} reads a pipe"
    ))
}

pub(crate) async fn run(host: &dyn Host, call: Call, ctx: &dyn CallContext) -> ToolRun {
    answer(call, &Env { host, ctx }).await
}

async fn answer(call: Call, env: &Env<'_>) -> ToolRun {
    let Some(id) = call.id else {
        return list(env).await;
    };
    let Some(job) = env.host.adopt(id, env.ctx) else {
        return refused(no_process(id));
    };
    let job = job.as_ref();
    let info = job.info().await;
    if info.restored {
        return super::restored::answer(id, job, &info, &call, env).await;
    }
    if let Some(reason) = &call.attach {
        return ask(id, job, info.mode, &call, reason.clone(), env).await;
    }
    if call.changes() && info.end.is_some() {
        return has_ended(id);
    }
    if info.keep && call.timeout_ms.is_some_and(|ms| ms > 0) {
        return refused(kept_no_limit(id));
    }
    let terminal = info.mode == Mode::Terminal;
    if !terminal && call.size.is_some() {
        return refused(format!(
            "`size` only applies to a terminal process, and process {id} reads a pipe"
        ));
    }
    if !terminal && call.read == Some(ReadKind::Screen) {
        return screen_of_pipe(id);
    }
    let started = env.host.elapsed_ms();
    let mut actions = Vec::new();
    let (mut end, typed) = match change(id, job, terminal, &call, env).await {
        Ok(done) => {
            actions.extend(done.actions);
            (done.end, done.typed)
        }
        Err(run) => return *run,
    };
    let spent = env.host.elapsed_ms().saturating_sub(started);
    if let Some(ms) = call.timeout_ms {
        let limit = (ms > 0).then_some(ms);
        job.set_limit(limit);
        actions.push(match limit {
            Some(ms) => format!(
                "Process {id} now has {} left before it is stopped.",
                seconds(ms)
            ),
            None => format!("Process {id} now has no time limit."),
        });
    }
    if end.is_none() && call.wait_ms > 0 {
        let how = WaitFor {
            timeout_ms: call.wait_ms.saturating_sub(spent),
            until: call.until.as_ref().map(|(_, source)| source.clone()),
            quiet_ms: call.quiet_ms,
        };
        end = match job.wait(how).await {
            Ok(waited) => Some(waited),
            Err(message) => return refused(message),
        };
    }
    let mut notes = Vec::new();
    if end == Some(Waited::Steered) {
        notes.push(STEERED_NOTE.to_owned());
    }
    notes.extend(call.notes.iter().cloned());
    let mut run = if end == Some(Waited::Cancelled) {
        stopped(id, job, env, notes).await
    } else {
        reply(id, job, &call, env, (Vec::new(), actions, notes)).await
    };
    run.typed = typed;
    run
}

fn with_typed(mut run: ToolRun, typed: Typed) -> Box<ToolRun> {
    run.typed = Some(typed);
    Box::new(run)
}

fn driven(id: u32) -> ToolRun {
    let message = format!(
        "Process {id} has the user typing into it, so input, keys, size and signals are refused until they detach; reads, waits and timeout_ms still work."
    );
    failed(message.clone(), message)
}

async fn reply(
    id: u32,
    job: &dyn Job,
    call: &Call,
    env: &Env<'_>,
    (before, actions, notes): (Vec<String>, Vec<String>, Vec<String>),
) -> ToolRun {
    let Some(read) = read(id, job, call, env).await else {
        return stopped(id, job, env, notes).await;
    };
    let info = job.info().await;
    let ended = info.end;
    env.report(id, ended.as_ref());
    let first = match (read.whole, &read.inline) {
        (Some(whole), _) => whole,
        (None, Some(inline)) => format!("{} {inline}", view::state_sentence(id, ended.as_ref())),
        (None, None) => view::state_sentence(id, ended.as_ref()),
    };
    let hidden = super::echo_off_line(info.echo_off, env.host);
    let text = before
        .into_iter()
        .chain(std::iter::once(first))
        .chain(actions)
        .chain(read.body)
        .chain(notes)
        .chain(hidden)
        .collect::<Vec<_>>()
        .join("\n");
    answered(Some(id), text)
}

pub fn not_a_terminal(id: u32) -> String {
    format!(
        "Process {id} has no terminal, so the user can't type into it; ask_user_to_attach needs a process started with tty."
    )
}

async fn ask(
    id: u32,
    job: &dyn Job,
    mode: Mode,
    call: &Call,
    reason: String,
    env: &Env<'_>,
) -> ToolRun {
    if mode != Mode::Terminal {
        return refused_as(not_a_terminal(id));
    }
    if job.info().await.end.is_some() {
        return reply(id, job, call, env, (Vec::new(), Vec::new(), Vec::new())).await;
    }
    let lead = |text: String| (vec![text], Vec::new(), Vec::new());
    match job.ask_attach(reason).await {
        AskReply::Stopped => {
            let text = format!(
                "Not answered: the user stopped the turn while asked to type into process {id}."
            );
            let mut run = ToolRun::new(Outcome::Cancelled).with_result(text);
            run.facts.push(Fact::Process(id));
            run
        }
        AskReply::Attached(true) => {
            job.settle(START_QUIET_MS, START_MAX_MS).await;
            let text = format!("The user typed into process {id} and detached.");
            reply(id, job, call, env, lead(text)).await
        }
        AskReply::Attached(false) => {
            let text = format!("The user attached to process {id} and detached without typing.");
            reply(id, job, call, env, lead(text)).await
        }
        AskReply::ProcessEnded(typed) => {
            let before = if typed {
                vec![format!("The user typed into process {id} before it ended.")]
            } else {
                Vec::new()
            };
            reply(id, job, call, env, (before, Vec::new(), Vec::new())).await
        }
        AskReply::Declined => answered(
            Some(id),
            format!("The user declined to type into process {id}."),
        ),
        AskReply::MessageInstead => answered(
            Some(id),
            format!("Not answered: the user wrote a message instead of typing into process {id}."),
        ),
        AskReply::Unavailable | AskReply::Answered(_) => {
            failed(NO_ONE_TO_ASK.to_owned(), NO_ONE_TO_ASK.to_owned())
        }
    }
}

async fn stopped(id: u32, job: &dyn Job, env: &Env<'_>, notes: Vec<String>) -> ToolRun {
    let ended = job.info().await.end;
    env.report(id, ended.as_ref());
    let text = std::iter::once(view::state_sentence(id, ended.as_ref()))
        .chain(notes)
        .chain(std::iter::once(env.stop_text().to_owned()))
        .collect::<Vec<_>>()
        .join("\n");
    let mut run = ToolRun::new(Outcome::Cancelled).with_result(text);
    run.facts.push(Fact::Process(id));
    run
}

async fn cancelled_write(id: u32, job: &dyn Job, env: &Env<'_>) -> ToolRun {
    let ended = job.info().await.end;
    env.report(id, ended.as_ref());
    let text = format!(
        "{}\n{}",
        view::state_sentence(id, ended.as_ref()),
        env.stop_text()
    );
    ToolRun::new(Outcome::Cancelled).with_result(text)
}

fn write_failed(id: u32, error: &str) -> ToolRun {
    let message = format!("could not write to process {id}: {error}");
    failed(message.clone(), format!("Failed: {message}"))
}

fn resize_failed(id: u32, error: &str) -> ToolRun {
    let message = format!("could not resize process {id}'s terminal: {error}");
    failed(message.clone(), format!("Failed: {message}"))
}

fn unwritten(id: u32, left: u64, total: u64) -> String {
    format!(
        "{} of {} weren't written: process {id} isn't reading its input.",
        group(left),
        count(total, "byte")
    )
}

pub fn unwritten_before_interrupt(id: u32, left: u64, total: u64) -> String {
    let (left, total) = (left.saturating_sub(1), total.saturating_sub(1));
    if left == 0 {
        return format!("The interrupt wasn't sent: process {id} isn't reading its input.");
    }
    format!(
        "{} of {} weren't written and the interrupt wasn't sent: process {id} isn't reading its input.",
        group(left),
        count(total, "byte")
    )
}

struct Changed {
    actions: Vec<String>,
    end: Option<Waited>,
    typed: Option<Typed>,
}

fn typing(call: &Call) -> Typing {
    Typing {
        text: call.input.clone(),
        keys: call.keys.clone(),
        signal: call.signal,
        size: call.size,
    }
}

async fn change(
    id: u32,
    job: &dyn Job,
    terminal: bool,
    call: &Call,
    env: &Env<'_>,
) -> Result<Changed, Box<ToolRun>> {
    let mut changed = Changed {
        actions: Vec::new(),
        end: None,
        typed: None,
    };
    let types = call.writes() || call.signal.is_some();
    if !types && call.size.is_none() {
        return Ok(changed);
    }
    let within = call.wait_ms.max(WRITE_AT_LEAST_MS);
    let written = job.write(typing(call), within).await;
    if types {
        changed.typed = Some(written.typed);
    }
    refusal(id, &written, terminal, call)?;
    changed.end = written.after_kill.clone();
    if terminal && let Some(size) = call.size {
        changed.actions.push(format!(
            "Process {id}'s terminal is now {}.",
            size_text(size)
        ));
    }
    if written.stopped {
        return Err(with_typed(
            cancelled_write(id, job, env).await,
            written.typed,
        ));
    }
    if let Some(JobError::Io(error)) = &written.failed {
        return Err(with_typed(write_failed(id, error), written.typed));
    }
    if written.left > 0 {
        let interrupts = terminal && call.signal == Some(SignalKind::Interrupt);
        changed.actions.push(if interrupts {
            unwritten_before_interrupt(id, written.left, written.total)
        } else {
            unwritten(id, written.left, written.total)
        });
    }
    Ok(changed)
}

fn refusal(id: u32, written: &Written, terminal: bool, call: &Call) -> Result<(), Box<ToolRun>> {
    let Some(error) = &written.failed else {
        return Ok(());
    };
    let run = match error {
        JobError::Ended => has_ended(id),
        JobError::Driven => driven(id),
        JobError::LineTooLong(longest) => refused_as(line_limit_text(id, *longest)),
        JobError::NoInput => refused_as(format!(
            "Process {id} has no stdin: only commands started with run_in_background or tty can take input."
        )),
        JobError::Closed => refused(format!(
            "process {id}'s stdin is closed: EOF was already sent"
        )),
        JobError::Invalid(message) if terminal => refused(message.clone()),
        JobError::Invalid(message) => refused(format!("{message}, and process {id} reads a pipe")),
        JobError::Io(error) if terminal && written.total == 0 && call.size.is_some() => {
            resize_failed(id, error)
        }
        JobError::Io(_) => return Ok(()),
    };
    Err(Box::new(run))
}

struct Shown {
    whole: Option<String>,
    inline: Option<String>,
    body: Option<String>,
}

impl Shown {
    fn plain(body: String) -> Self {
        Self {
            whole: None,
            inline: None,
            body: (!body.is_empty()).then_some(body),
        }
    }

    fn inline(inline: String, body: Option<String>) -> Self {
        Self {
            whole: None,
            inline: Some(inline),
            body,
        }
    }
}

pub(crate) fn lines_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

async fn screen_read(id: u32, job: &dyn Job) -> Option<Shown> {
    let screen = job.screen().await?;
    let end = job.info().await.end;
    if !screen.changed {
        return Some(Shown {
            whole: Some(view::screen_unchanged(id, end.as_ref())),
            inline: None,
            body: None,
        });
    }
    Some(Shown {
        whole: Some(view::screen_sentence_of(id, end.as_ref(), &screen)),
        inline: None,
        body: Some(view::screen_rows_text(&screen.rows).join("\n")),
    })
}

async fn read(id: u32, job: &dyn Job, call: &Call, env: &Env<'_>) -> Option<Shown> {
    let kind = match call.read {
        Some(kind) => kind,
        None if job.info().await.full_screen => ReadKind::Screen,
        None => ReadKind::New,
    };
    let home = env.home();
    match kind {
        ReadKind::Search => search(job, call).await,
        ReadKind::Screen => screen_read(id, job).await,
        ReadKind::New => {
            let read = job.new_output().await;
            Some(match view::new_text(&read, "New output", home.as_deref()) {
                Some(text) => Shown::plain(text),
                None => Shown::inline(view::no_new_text(read.held.as_deref()), None),
            })
        }
        ReadKind::Tail => {
            let n = call.lines.unwrap_or(input::TAIL_LINES);
            let read = job.tail(lines_u32(n)).await;
            Some(Shown::plain(view::tail_text(&read, home.as_deref())))
        }
        ReadKind::Lines => {
            let from = call.from.unwrap_or(1);
            let n = call.lines.unwrap_or(input::RANGE_LINES);
            let rows = job.lines(from, lines_u32(n)).await;
            Some(Shown::plain(view::rows_text(&rows, from)))
        }
    }
}

pub(crate) async fn found(job: &dyn Job, call: &Call) -> Option<String> {
    let (_, pattern) = call.pattern.as_ref()?;
    let from = call.from.unwrap_or(1);
    let context = lines_u32(call.context);
    let max = call.lines.unwrap_or(input::SEARCH_LINES);
    let found = job
        .search(pattern.clone(), from, context, lines_u32(max))
        .await
        .ok()?;
    Some(view::found_text(&found, pattern, from, call.context))
}

pub(crate) fn split_header(text: String) -> (String, Option<String>) {
    match text.split_once('\n') {
        Some((header, rest)) => (header.to_owned(), Some(rest.to_owned())),
        None => (text, None),
    }
}

async fn search(job: &dyn Job, call: &Call) -> Option<Shown> {
    let (header, rest) = split_header(found(job, call).await?);
    Some(Shown::inline(header, rest))
}

async fn list(env: &Env<'_>) -> ToolRun {
    let all = env.host.all().await;
    if all.is_empty() {
        return answered(
            None,
            "No processes have been started in this session.".to_owned(),
        );
    }
    let mut rows = Vec::new();
    for info in &all {
        let id = info.id.unwrap_or_default();
        let target = cut_chars(&info.label, LIST_TARGET_CHARS);
        let log = info
            .log
            .as_deref()
            .filter(|_| info.keep && info.end.is_none())
            .map(|log| format!("log {}", env.host.shorten(log)));
        if info.restored {
            let words = match &log {
                Some(_) => KEPT_RESTORED_LIST.to_owned(),
                None => view::state_words(info.end.as_ref()),
            };
            rows.push((id, words, target, log));
            continue;
        }
        env.report(id, info.end.as_ref());
        let mut words = match &log {
            Some(_) => KEPT_WORDS.to_owned(),
            None => view::state_words_for(info.end.as_ref(), info.mode == Mode::Terminal),
        };
        if info.end.is_none() && info.echo_off {
            words.push_str(", echo off");
        }
        let unread = (info.unread > 0).then(|| count(info.unread, "unread line"));
        let extra = match (unread, log) {
            (Some(unread), Some(log)) => Some(format!("{unread}, {log}")),
            (unread, log) => unread.or(log),
        };
        rows.push((id, words, target, extra));
    }
    rows.sort_by_key(|r| r.0);
    let rows: Vec<(String, String, String, Option<String>)> = rows
        .into_iter()
        .map(|(id, state, target, unread)| (id.to_string(), state, target, unread))
        .collect();
    let id_width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let state_width = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0);
    let target_width = rows
        .iter()
        .filter(|r| r.3.is_some())
        .map(|r| r.2.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = vec!["Processes:".to_owned()];
    for (id, state, target, unread) in rows {
        let line = match unread {
            Some(unread) => {
                format!("{id:<id_width$}  {state:<state_width$}  {target:<target_width$}  {unread}")
            }
            None => format!("{id:<id_width$}  {state:<state_width$}  {target}"),
        };
        text.push(line.trim_end().to_owned());
    }
    answered(None, text.join("\n"))
}
