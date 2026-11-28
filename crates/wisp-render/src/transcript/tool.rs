use std::collections::VecDeque;
use std::time::Duration;

use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use wisp_protocol::{
    Answer, AttachRequestEnd, AttachmentRef, BackgroundReason, ExitStatus, Fact, FileSlice,
    NotRunReason, ProcessEnd, Question, QuestionEnd, StopReason, TodoItem, TodoStatus, ToolAction,
    ToolDisplay, ToolOutcome, ToolOutputView, ToolPurpose, ToolView, Typed, Verbs,
};

use super::{Detail, Pen, Show, copy, one_line};
use crate::chrome::{answer_words, cut_header, home_short};
use crate::markdown::Renderer;
use crate::motion::{MotionCtx, comet, comet_next_change, ms};
use crate::styled::{Look, Opens, StyledLine, StyledSpan};
use crate::text::{
    TAB_STOP, WidthMode, expand_tabs, grapheme_width, plain_ascii, sanitize, spans_text,
    spans_width, strip_ansi, trim_end, width, wrap,
};
use crate::theme::Token;
use crate::theme::glyphs::{GlyphSet, Glyphs};

pub const LIVE_LINES: usize = 3;
pub const OUTPUT_INDENT: usize = 2;
pub const PARTIAL_CAP: usize = 4096;
pub const FILE_TAIL: u64 = 2 * 1024 * 1024;

mod diff;
mod page;
pub(crate) mod row;

use diff::DiffBody;
pub use diff::{
    CONTEXT_KEEP, CONTEXT_RUN, CopyLabel, DIFF_HUNKS, FOLD_ALL_LINES, FOLD_SHOWN_ROWS,
    MIN_NUMBER_WIDTH,
};
pub use row::{
    COMMAND_GAP, FACTS_GAP, MIN_COMMAND, MIN_TITLE, PATTERN_COLUMNS, TYPED_COLUMNS, echoed,
};
use row::{Head, Pens, ProcessCall, Said, Weight, entry_facts, lay_out, quoted, title_spans};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landed {
    Changed,
    Same,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolFile {
    Text { text: String, skipped: u64 },
    Gone(String),
    Loading,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolState {
    Preparing,
    Queued,
    Running,
    Finished,
    NotSent,
    Abandoned,
}

#[derive(Default)]
struct Live {
    lines: VecDeque<String>,
    partial: String,
}

impl Live {
    fn push(&mut self, text: &str) {
        for piece in text.split_inclusive('\n') {
            match piece.strip_suffix('\n') {
                Some(end) => {
                    self.partial.push_str(end);
                    let line = std::mem::take(&mut self.partial);
                    self.lines.push_back(clean_line(&line));
                    while self.lines.len() > LIVE_LINES {
                        self.lines.pop_front();
                    }
                }
                None => {
                    self.partial.push_str(piece);
                    if self.partial.len() > PARTIAL_CAP {
                        let mut cut = self.partial.len() - PARTIAL_CAP;
                        while !self.partial.is_char_boundary(cut) {
                            cut += 1;
                        }
                        self.partial.drain(..cut);
                    }
                }
            }
        }
    }

    fn tail(&self) -> Vec<String> {
        let mut out: Vec<String> = self.lines.iter().cloned().collect();
        let partial = clean_line(&self.partial);
        if !partial.trim().is_empty() {
            out.push(partial);
        }
        let skip = out.len().saturating_sub(LIVE_LINES);
        out.into_iter().skip(skip).collect()
    }
}

pub fn clean_line(raw: &str) -> String {
    if plain_ascii(raw) {
        return raw.to_owned();
    }
    let stripped = strip_ansi(raw);
    let line = stripped.strip_suffix('\r').unwrap_or(&stripped);
    let line = line.rsplit('\r').next().unwrap_or(line);
    if plain_ascii(line) {
        return line.to_owned();
    }
    sanitize(line).replace('\n', " ")
}

pub fn clean_lines(text: &str) -> Vec<String> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() {
        return vec![];
    }
    body.split('\n').map(clean_line).collect()
}

struct Asked {
    questions: Vec<Question>,
    end: Option<QuestionEnd>,
    late: bool,
}

impl Asked {
    fn answers(&self) -> &[Answer] {
        match &self.end {
            Some(QuestionEnd::Answered { answers }) => answers,
            _ => &[],
        }
    }

    fn answered(&self) -> Vec<(String, String)> {
        self.questions
            .iter()
            .zip(self.answers())
            .filter(|(_, a)| !a.is_skipped())
            .map(|(q, a)| {
                (
                    cut_header(&q.header, GlyphSet::Unicode),
                    answer_words(q, a, GlyphSet::Unicode),
                )
            })
            .collect()
    }
}

struct SteppedIn {
    id: u32,
    reason: String,
    typed: bool,
    end: Option<AttachRequestEnd>,
}

struct Finish {
    outcome: ToolOutcome,
    output: ToolOutputView,
    path: Option<String>,
    facts: Vec<Fact>,
    duration_ms: u64,
    result: Option<String>,
    saved: Vec<String>,
    slice: Option<FileSlice>,
}

impl Finish {
    fn refusal(&self) -> Option<String> {
        if !refused(&self.outcome) {
            return None;
        }
        self.result
            .as_deref()
            .and_then(first_text_line)
            .map(one_line)
    }

    fn describes_a_file(&self) -> bool {
        self.facts.iter().any(describes_a_file)
    }
}

struct RunFile {
    path: String,
    shown: String,
}

struct Withdrawn {
    reason: String,
    others: usize,
}

pub(super) struct ToolCell {
    name: String,
    writing: String,
    since: Duration,
    diff: Option<Box<DiffBody>>,
    pub(super) call_id: Option<String>,
    display: Option<CleanDisplay>,
    title: Option<String>,
    purpose: ToolPurpose,
    process: Option<Box<ProcessCall>>,
    typed: Option<Typed>,
    program: Option<String>,
    about: Option<String>,
    job: Option<Box<Job>>,
    full: Option<String>,
    description: Option<String>,
    pub(super) state: ToolState,
    started: Option<Duration>,
    cwd: Option<String>,
    live: Live,
    finish: Option<Finish>,
    pub(super) file: Option<Box<ToolFile>>,
    run_file: Option<Box<RunFile>>,
    file_epoch: u64,
    file_stale: bool,
    asked: Option<Box<Asked>>,
    stepped: Option<Box<SteppedIn>>,
    withdrawn: Option<Box<Withdrawn>>,
    interrupted: bool,
    todos: Vec<TodoItem>,
    agent: Option<Box<AgentLink>>,
    hooked: Option<Box<(String, Hooked)>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Hooked {
    Input { before: Value, after: Value },
    Text(String),
}

pub(super) fn changed_fields(before: &Value, after: &Value) -> Vec<String> {
    let empty = serde_json::Map::new();
    let (before, after) = (
        before.as_object().unwrap_or(&empty),
        after.as_object().unwrap_or(&empty),
    );
    let mut names: Vec<String> = after
        .iter()
        .filter(|(key, value)| before.get(*key) != Some(*value))
        .map(|(key, _)| key.clone())
        .chain(
            before
                .keys()
                .filter(|key| !after.contains_key(*key))
                .cloned(),
        )
        .map(|key| one_line(&key))
        .collect();
    names.sort();
    names
}

pub(super) fn input_change(before: &Value, after: &Value, shown: bool, pen: &Pen) -> String {
    let names = changed_fields(before, after);
    if !shown {
        return copy::changed_fields(&names);
    }
    let values: Vec<String> = names
        .iter()
        .map(|name| match after.get(name) {
            Some(Value::String(text)) => {
                format!("{name} = {}", quoted(text, &pen.glyphs, pen.mode))
            }
            Some(value) => format!(
                "{name} = {}",
                quoted(&value.to_string(), &pen.glyphs, pen.mode)
            ),
            None => format!("{name} {}", copy::REMOVED_FIELD),
        })
        .collect();
    copy::changed_fields(&values)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentLink {
    pub state: String,
    pub cwd: Option<String>,
    pub report: Option<String>,
}

fn kept_result(
    outcome: &ToolOutcome,
    facts: &[Fact],
    attachments: &[AttachmentRef],
    text: &str,
) -> Option<String> {
    if refused(outcome) {
        return Some(sanitize(text));
    }
    if attachments.is_empty() && !facts.iter().any(describes_a_file) {
        return None;
    }
    first_text_line(text).map(sanitize)
}

fn refused(outcome: &ToolOutcome) -> bool {
    matches!(
        outcome,
        ToolOutcome::Completed {
            is_error: true,
            exit: None,
        }
    )
}

fn describes_a_file(fact: &Fact) -> bool {
    matches!(fact, Fact::Media { .. } | Fact::Part { .. })
}

fn first_text_line(text: &str) -> Option<&str> {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .map(str::trim_end)
}

fn full_text(input: &Value, target: &str) -> Option<String> {
    let map = input.as_object()?;
    map.values()
        .filter_map(Value::as_str)
        .find(|text| first_text_line(text) == Some(target.trim_end()))
        .map(|text| {
            let lines: Vec<&str> = text.lines().skip_while(|l| l.trim().is_empty()).collect();
            let end = lines
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map_or(0, |i| i + 1);
            lines
                .get(..end)
                .unwrap_or_default()
                .iter()
                .map(|l| sanitize(l.trim_end()))
                .collect::<Vec<_>>()
                .join("\n")
        })
}

fn folded_keys(input: &Value) -> String {
    let keys = input
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    format!("{{{}}}", keys.join(", "))
        .lines()
        .collect::<Vec<_>>()
        .join(" ")
}

struct CleanDisplay {
    action: ToolAction,
    verbs: Option<Box<Verbs>>,
    target: String,
}

fn literal_json_object_keys(trimmed: &str) -> Option<Vec<String>> {
    let Ok(Value::Object(_)) = serde_json::from_str::<Value>(trimmed) else {
        return None;
    };
    let mut rest = trimmed.strip_prefix('{')?.trim_start();
    let mut keys = Vec::new();
    while !rest.starts_with('}') {
        let mut key_stream = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let Some(Ok(key)) = key_stream.next() else {
            return None;
        };
        let key_raw = rest.get(..key_stream.byte_offset())?;
        if key_raw.contains('\\') || key_raw != format!("\"{key}\"") {
            return None;
        }
        let after_key = rest.get(key_stream.byte_offset()..)?.trim_start();
        let val_str = after_key.strip_prefix(':')?.trim_start();
        let mut val_stream = serde_json::Deserializer::from_str(val_str).into_iter::<Value>();
        let Some(Ok(_)) = val_stream.next() else {
            return None;
        };
        let after_val = val_str.get(val_stream.byte_offset()..)?.trim_start();
        keys.push(key);
        if let Some(next) = after_val.strip_prefix(',') {
            rest = next.trim_start();
        } else {
            rest = after_val;
            break;
        }
    }
    (rest.trim() == "}" && !keys.is_empty()).then_some(keys)
}

fn clean_other_brace_target(target: &str, input: &Value) -> String {
    let trimmed = target.trim();
    if trimmed == "{}" {
        return "{}".to_owned();
    }
    let expected = folded_keys(input);
    if trimmed == expected && one_line(&expected) == expected {
        return expected;
    }
    let Some(input_map) = input.as_object() else {
        return "{}".to_owned();
    };
    if let Some(keys) = literal_json_object_keys(trimmed)
        && keys
            .iter()
            .all(|key| input_map.contains_key(key) && one_line(key) == *key)
    {
        return format!("{{{}}}", keys.join(", "));
    }
    "{}".to_owned()
}

fn clean_display(display: &ToolDisplay, input: &Value) -> CleanDisplay {
    let target_sanitized = one_line(display.target.trim_end());
    let target = match &display.action {
        ToolAction::Other { .. }
            if display.target.trim_start().starts_with('{')
                || target_sanitized.trim_start().starts_with('{') =>
        {
            clean_other_brace_target(&display.target, input)
        }
        _ => target_sanitized,
    };
    CleanDisplay {
        action: match &display.action {
            ToolAction::Other { name } => {
                let sanitized = one_line(name);
                ToolAction::Other {
                    name: if sanitized == *name {
                        sanitized
                    } else {
                        "tool".to_owned()
                    },
                }
            }
            known => known.clone(),
        },
        verbs: display.verbs.as_ref().map(|v| {
            Box::new(Verbs {
                doing: one_line(&v.doing),
                done: one_line(&v.done),
                failed: one_line(&v.failed),
            })
        }),
        target,
    }
}

fn exit_failed(outcome: &ToolOutcome) -> bool {
    match outcome {
        ToolOutcome::Completed { is_error, exit } => {
            *is_error
                || match exit {
                    Some(ExitStatus::Code { code }) => *code != 0,
                    Some(ExitStatus::Signal { .. }) => true,
                    None => false,
                }
        }
        _ => true,
    }
}

pub(super) fn diffstat_spans(added: u64, removed: u64, g: &Glyphs) -> Vec<StyledSpan> {
    vec![
        StyledSpan::new(format!("+{added}"), Token::Ok),
        StyledSpan::plain(" "),
        StyledSpan::new(format!("{}{removed}", g.minus), Token::Error),
    ]
}

pub(super) fn clip(line: StyledLine, cols: usize, mode: WidthMode, more: &str) -> StyledLine {
    let bytes: usize = line.spans.iter().map(|s| s.text.len()).sum();
    if bytes <= cols || spans_width(&line.spans, mode) <= cols {
        return line;
    }
    let opens = line.opens;
    let more_w = width(more, mode);
    let room = cols.saturating_sub(more_w);
    let mut used = 0;
    let mut spans = vec![];
    let mut look = Look::default();
    'outer: for span in line.spans {
        let mut text = String::new();
        look = span.look;
        for g in span.text.graphemes(true) {
            let w = grapheme_width(g, mode);
            if used + w > room {
                spans.push(StyledSpan::new(text, look));
                break 'outer;
            }
            used += w;
            text.push_str(g);
        }
        spans.push(StyledSpan::new(text, span.look));
    }
    if more_w <= cols {
        spans.push(StyledSpan::new(more, look));
    }
    spans.retain(|s| !s.text.is_empty());
    StyledLine {
        opens,
        ..StyledLine::new(spans)
    }
}

pub(super) struct Entry {
    pub(super) line: StyledLine,
    pub(super) filter: String,
    pub(super) live: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Job {
    id: u32,
    end: Option<ProcessEnd>,
    restored: bool,
    kept: Keeps,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Keeps {
    #[default]
    No,
    Running,
    Unchecked,
}

fn said_of(fact: &Fact, g: &Glyphs) -> Vec<Said> {
    let kind = copy::fact_kind(fact).map(Said::count);
    let said = match fact {
        Fact::Unknown => return Vec::new(),
        Fact::Exit { code } if *code != 0 => Said::error(copy::fact(fact, g)),
        Fact::Process { .. } => Said::state(copy::fact(fact, g)),
        Fact::Diffstat { added, removed } => {
            Said::spans(diffstat_spans(*added, *removed, g), Weight::Count)
        }
        _ => Said::count(copy::fact(fact, g)),
    };
    kind.into_iter().chain([said]).collect()
}

fn end_mark(end: &ProcessEnd, g: &Glyphs) -> (&'static str, Token) {
    match end {
        ProcessEnd::Exited {
            exit: ExitStatus::Code { code: 0 },
        } => (g.done, Token::Ok),
        ProcessEnd::Exited { .. } | ProcessEnd::Failed { .. } => (g.failed, Token::Error),
        ProcessEnd::Stopped { .. } | ProcessEnd::Lost { .. } => (g.stopped, Token::TextMuted),
    }
}

fn clean_end(end: &ProcessEnd) -> ProcessEnd {
    match end {
        ProcessEnd::Failed { message } => ProcessEnd::Failed {
            message: one_line(message),
        },
        other => other.clone(),
    }
}

impl ToolCell {
    pub(super) fn preparing(name: &str, writing: Option<&str>, at: Duration) -> ToolCell {
        let name = one_line(name);
        let writing = writing
            .map(one_line)
            .filter(|w| !w.trim().is_empty())
            .unwrap_or_else(|| copy::writing_call(&name));
        ToolCell {
            name,
            writing,
            since: at,
            diff: None,
            call_id: None,
            display: None,
            title: None,
            purpose: ToolPurpose::Run,
            process: None,
            typed: None,
            program: None,
            about: None,
            job: None,
            full: None,
            description: None,
            state: ToolState::Preparing,
            started: None,
            cwd: None,
            live: Live::default(),
            finish: None,
            file: None,
            run_file: None,
            file_epoch: 0,
            file_stale: false,
            asked: None,
            stepped: None,
            withdrawn: None,
            interrupted: false,
            todos: Vec::new(),
            agent: None,
            hooked: None,
        }
    }

    pub(super) fn ask(&mut self, questions: &[Question]) {
        self.asked = Some(Box::new(Asked {
            questions: questions.to_vec(),
            end: None,
            late: false,
        }));
    }

    pub(super) fn answered(&self) -> Vec<(String, String)> {
        self.asked.as_deref().map_or_else(Vec::new, Asked::answered)
    }

    pub(super) fn ask_attach(&mut self, id: u32, reason: &str) {
        self.stepped = Some(Box::new(SteppedIn {
            id,
            reason: one_line(reason),
            typed: false,
            end: None,
        }));
    }

    pub(super) fn typed_in(&mut self) {
        if let Some(stepped) = &mut self.stepped {
            stepped.typed = true;
        }
    }

    pub(super) fn attach_closed(&mut self, how: AttachRequestEnd) {
        if let Some(stepped) = &mut self.stepped {
            stepped.end = Some(how);
        }
    }

    fn step_in_head(&self, stepped: &SteppedIn, pen: &Pen) -> Head {
        let g = &pen.glyphs;
        let muted = Look::fg(Token::TextMuted);
        let text = Look::fg(Token::Text);
        let name = copy::process_name(stepped.id);
        let head = |glyph: &str, glyph_look: Look, title: Vec<StyledSpan>, facts: Vec<Said>| Head {
            glyph: glyph.to_owned(),
            glyph_look,
            title,
            command: self.about.clone(),
            extra: None,
            facts,
        };
        let end = match stepped.end {
            Some(AttachRequestEnd::ProcessEnded) if stepped.typed => Some(AttachRequestEnd::Typed),
            end => end,
        };
        match end {
            None => head(
                g.typed,
                Look::fg(Token::Accent),
                title_spans(copy::WAITING_TO_TAKE_OVER, &name, text),
                vec![Said::count(stepped.reason.clone())],
            ),
            Some(AttachRequestEnd::Typed) => head(
                g.typed,
                muted,
                title_spans(copy::YOU_TYPED_INTO, &name, text),
                vec![],
            ),
            Some(AttachRequestEnd::AttachedOnly) => head(
                g.typed,
                muted,
                title_spans(
                    copy::YOU_LOOKED_AT,
                    &format!("{name} {}", copy::WITHOUT_TYPING),
                    text,
                ),
                vec![],
            ),
            Some(AttachRequestEnd::Declined) => head(
                g.stopped,
                muted,
                title_spans(copy::DECLINED_TAKE_OVER, &name, muted),
                vec![],
            ),
            Some(AttachRequestEnd::ProcessEnded) => head(
                g.stopped,
                muted,
                title_spans("", &copy::process_ended_first(stepped.id), muted),
                vec![],
            ),
            Some(AttachRequestEnd::Stopped) => head(
                g.stopped,
                muted,
                title_spans(copy::STEP_IN_NOT_ANSWERED, "", muted),
                vec![Said::state(name), Said::count(copy::ANSWER_STOPPED)],
            ),
        }
    }

    pub(super) fn answer(&mut self, how: &QuestionEnd) {
        let late = matches!(self.state, ToolState::Finished);
        if let Some(asked) = &mut self.asked {
            asked.end = Some(how.clone());
            asked.late = late;
        }
    }

    pub(super) fn request(
        &mut self,
        call_id: &str,
        tool: &str,
        input: &Value,
        display: &ToolDisplay,
    ) {
        self.name = one_line(tool);
        self.call_id = Some(call_id.to_owned());
        let cleaned = clean_display(display, input);
        let input = if matches!(cleaned.action, ToolAction::Other { .. }) && cleaned.target == "{}"
        {
            &Value::Null
        } else {
            input
        };
        self.diff = display.view.as_ref().and_then(DiffBody::of).map(Box::new);
        self.process = display
            .view
            .as_ref()
            .and_then(ProcessCall::of)
            .map(Box::new);
        let view_title = match &display.view {
            Some(ToolView::Diff { title, .. }) => title.as_deref(),
            _ => None,
        };
        self.title = display
            .title
            .as_deref()
            .or(view_title)
            .map(one_line)
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
        self.purpose = display.purpose;
        let process_like = ProcessCall::of_action(&cleaned.action, &cleaned.target).is_some();
        self.full = match (&self.diff, &self.process) {
            (None, None) if !process_like => full_text(input, &cleaned.target),
            _ => None,
        };
        self.display = Some(cleaned);
        self.description = input
            .get("description")
            .and_then(Value::as_str)
            .map(one_line)
            .filter(|d| !d.trim().is_empty() && !self.process_like());
        if self.state == ToolState::Preparing {
            self.state = ToolState::Queued;
        }
    }

    pub(super) fn process_id(&self) -> Option<u32> {
        self.process.as_ref().and_then(|p| p.id).or_else(|| {
            let action = self.display.as_ref().map(|d| &d.action)?;
            ProcessCall::of_action(action, self.target()?).and_then(|p| p.id)
        })
    }

    pub(super) fn name_process(&mut self, program: Option<&str>, about: Option<&str>) {
        if let Some(program) = program {
            self.program = Some(one_line(program));
        }
        if let Some(about) = about {
            self.about = Some(about.to_owned());
        }
    }

    pub(super) fn title_text(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub(super) fn verdict(&mut self, typed: Option<&Typed>) {
        self.typed = typed.copied();
    }

    pub(super) fn link_job(&mut self, id: u32) {
        if self.job.is_none() {
            self.job = Some(Box::new(Job {
                id,
                end: None,
                restored: false,
                kept: Keeps::No,
            }));
        }
    }

    pub(super) fn job_kept(&mut self, kept: Keeps, restored: bool) {
        if let Some(job) = &mut self.job {
            job.kept = kept;
            job.restored |= restored;
        }
    }

    pub(super) fn job_id(&self) -> Option<u32> {
        self.job.as_ref().map(|j| j.id)
    }

    pub(super) fn job_ended(&mut self, end: &ProcessEnd, restored: bool) {
        if let Some(job) = &mut self.job {
            job.end = Some(clean_end(end));
            job.restored = restored;
        }
        self.drop_live_file();
    }

    pub(super) fn job_restarted(&mut self) -> bool {
        match &mut self.job {
            Some(job) if job.end.is_none() && !job.restored => {
                job.restored = true;
                true
            }
            _ => false,
        }
    }

    pub(super) fn start(&mut self, at: Duration, cwd: &str, file: Option<(&str, Option<&str>)>) {
        if matches!(self.state, ToolState::Queued | ToolState::Preparing) {
            self.state = ToolState::Running;
            self.started = Some(at);
        }
        self.cwd = Some(one_line(cwd)).filter(|c| !c.trim().is_empty());
        if let Some((path, home)) = file {
            self.run_file = Some(Box::new(RunFile {
                path: path.to_owned(),
                shown: one_line(&home_short(path, home)),
            }));
        }
    }

    pub(super) fn output(&mut self, text: &str) -> bool {
        if self.state != ToolState::Running {
            return false;
        }
        self.live.push(text);
        self.file.is_none()
    }

    fn drop_live_file(&mut self) {
        if self.file.is_some() {
            self.file_stale = true;
            self.file_epoch = self.file_epoch.wrapping_add(1);
        }
    }

    pub(super) fn has_text(&self) -> bool {
        matches!(self.file.as_deref(), Some(ToolFile::Text { .. }))
    }

    pub(super) fn land(&mut self, epoch: u64, file: ToolFile) -> Landed {
        if epoch != self.file_epoch {
            return Landed::Stale;
        }
        let same = !self.file_stale && self.file.as_deref() == Some(&file);
        self.file_stale = false;
        if same {
            return Landed::Same;
        }
        self.file = Some(Box::new(file));
        Landed::Changed
    }

    pub(super) fn set_file(&mut self, file: Option<ToolFile>) {
        self.file = file.map(Box::new);
        self.file_stale = false;
    }

    pub(super) fn follows_output(&self) -> bool {
        self.state == ToolState::Running
            || (self.state == ToolState::Finished
                && self
                    .job
                    .as_ref()
                    .is_some_and(|j| j.end.is_none() && !j.restored))
    }

    pub(super) fn ticks(&self) -> bool {
        self.follows_output() || matches!(self.state, ToolState::Preparing)
    }

    pub(super) fn file_epoch(&self) -> u64 {
        self.file_epoch
    }

    pub(super) fn file_path(&self) -> Option<&str> {
        match self.state {
            ToolState::Finished => self.finish.as_ref().and_then(|f| f.path.as_deref()),
            ToolState::Running => self.run_file.as_ref().map(|f| f.path.as_str()),
            _ => None,
        }
    }

    fn shown_path(&self) -> Option<&str> {
        match self.state {
            ToolState::Finished => self.finish.as_ref().and_then(|f| f.output.file.as_deref()),
            _ => self.run_file.as_ref().map(|f| f.shown.as_str()),
        }
    }

    pub(super) fn finish(
        &mut self,
        outcome: &ToolOutcome,
        output: &ToolOutputView,
        facts: &[Fact],
        duration_ms: u64,
        (result, attachments, home): (Option<&str>, &[AttachmentRef], Option<&str>),
    ) {
        let shown = |path: &str| one_line(&home_short(path, home));
        self.drop_live_file();
        self.state = ToolState::Finished;
        self.live = Live::default();
        let clean = ToolOutputView {
            tail: output.tail.clone(),
            file: output.file.as_deref().map(shown),
            file_problem: output.file_problem.as_deref().map(one_line),
            ..output.clone()
        };
        let slice = match attachments {
            [only]
                if facts
                    .iter()
                    .filter(|f| matches!(f, Fact::Media { .. }))
                    .count()
                    == 1 =>
            {
                only.slice
            }
            _ => None,
        };
        self.finish = Some(Finish {
            outcome: clean_outcome(outcome),
            output: clean,
            path: output.file.clone(),
            facts: facts.to_vec(),
            duration_ms,
            slice,
            result: result.and_then(|text| kept_result(outcome, facts, attachments, text)),
            saved: attachments
                .iter()
                .filter_map(|a| a.saved.as_deref())
                .map(shown)
                .collect(),
        });
    }

    pub(super) fn withdraw(&mut self, reason: &str, others: usize) {
        self.withdrawn = Some(Box::new(Withdrawn {
            reason: one_line(reason),
            others,
        }));
    }

    pub(super) fn interrupt(&mut self, at: Duration) {
        self.interrupted = self.state == ToolState::Running;
        self.close(at);
    }

    pub(super) fn close(&mut self, at: Duration) {
        if self.state == ToolState::Running {
            let lines = self.live.tail();
            let mut tail = lines.join("\n");
            if !tail.is_empty() {
                tail.push('\n');
            }
            let ran = at.saturating_sub(self.started.unwrap_or(at));
            let output = ToolOutputView {
                bytes: u64::try_from(tail.len()).unwrap_or(u64::MAX),
                lines: u64::try_from(lines.len()).unwrap_or(u64::MAX),
                tail,
                ..ToolOutputView::default()
            };
            let ms = ms(ran);
            self.finish(&ToolOutcome::Cancelled, &output, &[], ms, (None, &[], None));
            return;
        }
        self.state = match self.state {
            ToolState::Preparing => ToolState::NotSent,
            ToolState::Queued => ToolState::Abandoned,
            done => done,
        };
        self.live = Live::default();
    }

    pub(super) fn next_change(&self, now: Duration, cx: MotionCtx) -> Option<Duration> {
        let since = match self.state {
            ToolState::Running => self.started?,
            ToolState::Preparing => self.since,
            _ => return None,
        };
        let local = now.saturating_sub(since);
        let second = Duration::from_secs(local.as_secs().saturating_add(1));
        let next = match comet_next_change(local, cx) {
            Some(c) => c.min(second),
            None => second,
        };
        Some(since.saturating_add(next))
    }

    pub(super) fn wants_file(&self) -> Option<&str> {
        if self.file.is_some() && !self.file_stale {
            return None;
        }
        self.file_path()
    }

    pub(super) fn target(&self) -> Option<&str> {
        self.display.as_ref().map(|d| d.target.as_str())
    }

    fn label(&self) -> (String, String) {
        if self.verbs().is_some() {
            return (String::new(), self.target().unwrap_or_default().to_owned());
        }
        match self.display.as_ref().map(|d| &d.action) {
            Some(ToolAction::Other { name }) => {
                (name.clone(), self.target().unwrap_or_default().to_owned())
            }
            Some(ToolAction::Unknown) => (
                self.name.clone(),
                self.target().unwrap_or_default().to_owned(),
            ),
            Some(_) => (String::new(), self.target().unwrap_or_default().to_owned()),
            None => (String::new(), self.name.clone()),
        }
    }

    fn shown_target(&self) -> String {
        let (name, target) = self.label();
        match (name.is_empty(), target.is_empty()) {
            (true, _) => target,
            (false, true) => name,
            (false, false) => format!("{name} {target}"),
        }
    }

    fn extra_lines(&self) -> usize {
        self.full
            .as_deref()
            .map_or(0, |f| f.lines().count().saturating_sub(1))
    }

    fn clock(ms: u64) -> String {
        copy::clock(ms / 1000)
    }

    fn never_started(&self) -> bool {
        self.finish.as_ref().is_some_and(|f| {
            f.refusal().is_some()
                || matches!(
                    f.outcome,
                    ToolOutcome::InvalidInput { .. }
                        | ToolOutcome::Failed {
                            before_start: true,
                            ..
                        }
                )
        })
    }

    pub(super) fn process_like(&self) -> bool {
        self.process.is_some()
            || self.display.as_ref().is_some_and(|d| {
                ProcessCall::of_action(&d.action, self.target().unwrap_or_default()).is_some()
            })
    }

    fn unrun_title_spans(&self, look: Look, pen: &Pen) -> Vec<StyledSpan> {
        if self.title.is_none() && !self.process_like() {
            let target = self.shown_target();
            if target.trim().is_empty() {
                let generic = ToolAction::Other {
                    name: self.name.clone(),
                };
                let verb = copy::tool_verb(Some(&generic), false);
                return title_spans(verb, self.name.trim(), look);
            }
            let search = matches!(
                self.display.as_ref().map(|d| &d.action),
                Some(ToolAction::Search)
            );
            let rest = if search {
                copy::quoted(&target)
            } else {
                target
            };
            return title_spans("", &rest, look);
        }
        self.title_spans(true, look, pen)
    }

    fn process_call(&self) -> Option<ProcessCall> {
        let action = self.display.as_ref().map(|d| &d.action)?;
        ProcessCall::of_action(action, self.target().unwrap_or_default())
    }

    fn process_words(
        &self,
        name: Option<&str>,
        running: bool,
        pen: &Pen,
    ) -> Option<(String, String)> {
        let action = self.display.as_ref().map(|d| &d.action);
        let marks = (&pen.glyphs, pen.mode);
        match &self.process {
            Some(call) => call.words(action, name, self.typed.as_ref(), running, marks),
            None => self
                .process_call()?
                .words(action, name, None, running, marks),
        }
    }

    fn title_words(&self, running: bool, pen: &Pen) -> (String, String) {
        if let Some(title) = &self.title {
            return (String::new(), title.clone());
        }
        let action = self.display.as_ref().map(|d| &d.action);
        if self.never_started() && self.process_like() {
            let rest = match (self.process_id(), action) {
                (Some(id), _) => format!("{} {id}", self.name),
                (None, Some(ToolAction::Other { .. })) => self.shown_target(),
                (None, _) => self.name.clone(),
            };
            let generic = ToolAction::Other {
                name: self.name.clone(),
            };
            return (copy::tool_verb(Some(&generic), running).to_owned(), rest);
        }
        if self.process_like() {
            let name = match self.process_id() {
                Some(id) => copy::process_name(id),
                None if self.process.is_some() => String::new(),
                None => self.target().unwrap_or_default().to_owned(),
            };
            return self
                .process_words(Some(&name), running, pen)
                .unwrap_or_default();
        }
        match action {
            None => (String::new(), self.name.clone()),
            Some(action) => (self.verb(action, running), self.shown_target()),
        }
    }

    fn verbs(&self) -> Option<&Verbs> {
        self.display.as_ref().and_then(|d| d.verbs.as_deref())
    }

    fn verb(&self, action: &ToolAction, running: bool) -> String {
        match self.verbs() {
            Some(verbs) if running => verbs.doing.clone(),
            Some(verbs) => verbs.done.clone(),
            None => copy::tool_verb(Some(action), running).to_owned(),
        }
    }

    fn could_not(&self) -> String {
        match self.verbs() {
            Some(verbs) => verbs.failed.clone(),
            None => copy::could_not(self.display.as_ref().map(|d| &d.action)).to_owned(),
        }
    }

    pub(super) fn doing(&self, pen: &Pen) -> String {
        let (verb, rest) = self.title_words(true, pen);
        match (verb.is_empty(), rest.trim().is_empty()) {
            (_, true) => String::new(),
            (true, false) => rest,
            (false, false) => format!("{verb} {rest}"),
        }
    }

    fn title_spans(&self, running: bool, look: Look, pen: &Pen) -> Vec<StyledSpan> {
        let (verb, rest) = self.title_words(running, pen);
        title_spans(&verb, &rest, look)
    }

    pub(super) fn command(&self, running: bool, pen: &Pen) -> Option<String> {
        if self.process_like() {
            return self.process_aside(running, pen);
        }
        if self.title.is_none() || self.purpose == ToolPurpose::Show {
            return None;
        }
        let target = self.shown_target();
        let target = target.trim_end();
        (!target.is_empty() && Some(target) != self.title.as_deref()).then(|| target.to_owned())
    }

    pub(super) fn extra(&self) -> Option<String> {
        (self.extra_lines() > 0).then(|| copy::more_lines(self.extra_lines()))
    }

    fn process_aside(&self, running: bool, pen: &Pen) -> Option<String> {
        if self.title.is_none() {
            return self.about.clone();
        }
        if self.never_started() {
            return None;
        }
        let (verb, rest) = self.process_words(None, running, pen)?;
        Some(if rest.is_empty() {
            verb
        } else {
            format!("{verb} {rest}")
        })
    }

    fn process_facts(&self) -> Vec<Said> {
        self.process_id()
            .filter(|_| self.title.is_some())
            .map(|id| vec![Said::state(copy::process_name(id))])
            .unwrap_or_default()
    }

    fn question_head(&self, asked: &Asked, pen: &Pen, show: Show) -> Head {
        let g = &pen.glyphs;
        let text = Look::fg(Token::Text);
        let headers = asked
            .questions
            .iter()
            .map(|q| cut_header(&q.header, g.set))
            .collect::<Vec<_>>()
            .join(", ");
        let head = |glyph: &str, glyph_look: Look, verb: &str, facts: Vec<Said>| Head {
            glyph: glyph.to_owned(),
            glyph_look,
            title: title_spans(verb, &headers, text),
            command: None,
            extra: None,
            facts,
        };
        let not_answered = |reason: &str| {
            head(
                g.stopped,
                Look::fg(Token::TextMuted),
                copy::ASKED,
                vec![Said::state(copy::NOT_ANSWERED), Said::count(reason)],
            )
        };
        match &asked.end {
            Some(QuestionEnd::Answered { .. }) => {
                head(g.done, Look::fg(Token::Ok), copy::ASKED, vec![])
            }
            Some(QuestionEnd::Stopped) if asked.late => not_answered(copy::ANSWER_CLOSED),
            Some(QuestionEnd::Stopped) => not_answered(copy::ANSWER_STOPPED),
            Some(QuestionEnd::MessageInstead) => not_answered(copy::ANSWER_MESSAGE_INSTEAD),
            Some(QuestionEnd::Interrupted) => not_answered(copy::ANSWER_INTERRUPTED),
            Some(QuestionEnd::Withdrawn) => not_answered(copy::ANSWER_WITHDRAWN),
            None if self.asking(asked) => {
                let local = self
                    .started
                    .map_or(Duration::ZERO, |s| show.now.saturating_sub(s));
                head(
                    comet(local, show.cx, g),
                    Look::fg(Token::Accent),
                    copy::ASKING,
                    vec![Said::time(copy::clock(local.as_secs()))],
                )
            }
            None if self.waiting(asked) => head(
                g.needs_you,
                Look::fg(Token::Accent),
                copy::ASKED,
                vec![Said::state(copy::waiting_fact())],
            ),
            None => not_answered(copy::ANSWER_STOPPED),
        }
    }

    fn asking(&self, asked: &Asked) -> bool {
        asked.end.is_none()
            && matches!(
                self.state,
                ToolState::Running | ToolState::Queued | ToolState::Preparing
            )
    }

    fn waiting(&self, asked: &Asked) -> bool {
        asked.end.is_none()
            && self.state == ToolState::Finished
            && self.finish.as_ref().is_some_and(|f| {
                matches!(
                    f.outcome,
                    ToolOutcome::Completed {
                        is_error: false,
                        ..
                    }
                )
            })
    }

    fn head(&self, pen: &Pen, show: Show) -> Head {
        if let Some(asked) = self.asked.as_ref().filter(|_| self.verbs().is_none()) {
            return self.question_head(asked, pen, show);
        }
        if let Some(stepped) = &self.stepped {
            return self.step_in_head(stepped, pen);
        }
        let g = &pen.glyphs;
        let muted = Look::fg(Token::TextMuted);
        let text = Look::fg(Token::Text);
        let call = |glyph: &str, glyph_look: Look, title: Vec<StyledSpan>, facts: Vec<Said>| Head {
            glyph: glyph.to_owned(),
            glyph_look,
            title,
            command: self.command(true, pen),
            extra: self.extra(),
            facts,
        };
        match self.state {
            ToolState::Preparing => {
                let local = show.now.saturating_sub(self.since);
                Head {
                    glyph: comet(local, show.cx, g).to_owned(),
                    glyph_look: muted,
                    title: vec![StyledSpan::new(copy::writing(&self.writing), text)],
                    command: None,
                    extra: None,
                    facts: vec![Said::time(copy::clock(local.as_secs()))],
                }
            }
            ToolState::Queued => {
                let mut facts = self.process_facts();
                facts.push(Said::state(copy::QUEUED_FACT));
                call(g.waiting, muted, self.title_spans(true, text, pen), facts)
            }
            ToolState::Running => {
                let local = self
                    .started
                    .map_or(Duration::ZERO, |s| show.now.saturating_sub(s));
                let mut facts = self.process_facts();
                facts.push(Said::time(copy::clock(local.as_secs())));
                call(
                    comet(local, show.cx, g),
                    Look::fg(Token::Accent),
                    self.title_spans(true, text, pen),
                    facts,
                )
            }
            ToolState::NotSent => Head {
                glyph: g.stopped.to_owned(),
                glyph_look: Look::fg(Token::TextFaint),
                title: vec![StyledSpan::new(copy::writing(&self.writing), muted)],
                command: None,
                extra: None,
                facts: vec![Said::state(copy::NOT_SENT_FACT)],
            },
            ToolState::Abandoned => {
                let mut facts = self.process_facts();
                facts.push(Said::state(copy::NOT_RUN_FACT));
                facts.push(Said::count(copy::TURN_STOPPED));
                call(g.stopped, muted, self.unrun_title_spans(muted, pen), facts)
            }
            ToolState::Finished => self.finished_head(pen),
        }
    }

    fn finished_head(&self, pen: &Pen) -> Head {
        let g = &pen.glyphs;
        let muted = Look::fg(Token::TextMuted);
        let text = Look::fg(Token::Text);
        let call =
            |glyph: &str, glyph_look: Look, running: bool, look: Look, facts: Vec<Said>| Head {
                glyph: glyph.to_owned(),
                glyph_look,
                title: self.title_spans(running, look, pen),
                command: self.command(running, pen),
                extra: self.extra(),
                facts,
            };
        let named = |more: Vec<Said>| {
            let mut facts = self.process_facts();
            facts.extend(more);
            facts
        };
        let unrun = |facts: Vec<Said>| Head {
            glyph: g.stopped.to_owned(),
            glyph_look: muted,
            title: self.unrun_title_spans(muted, pen),
            command: self.command(true, pen),
            extra: self.extra(),
            facts: named(facts),
        };
        let failed_mark = Look::fg(Token::Error);
        let Some(finish) = &self.finish else {
            return unrun(vec![Said::state(copy::NOT_RUN_FACT)]);
        };
        if self.withdrawn.is_some() {
            return call(
                g.failed,
                failed_mark,
                false,
                text,
                named(vec![Said::error(copy::COULD_NOT_ATTACH)]),
            );
        }
        if finish.refusal().is_some() {
            return call(
                g.failed,
                failed_mark,
                false,
                text,
                named(vec![Said::error(self.could_not())]),
            );
        }
        let clock = Said::time(Self::clock(finish.duration_ms));
        let summary_only = !finish.facts.is_empty()
            && finish
                .facts
                .iter()
                .all(|fact| matches!(fact, Fact::Hits { .. } | Fact::Tokens { .. }));
        let own = self.process_id();
        let lead = self.process_facts().len();
        let mut facts = named(
            finish
                .facts
                .iter()
                .filter(|f| !matches!(f, Fact::Process { id } if Some(*id) == own))
                .flat_map(|fact| {
                    if summary_only {
                        vec![Said::state(copy::fact(fact, g))]
                    } else {
                        let mut said = said_of(fact, g);
                        if matches!(fact, Fact::Media { .. })
                            && let Some(slice) = &finish.slice
                        {
                            said.push(Said::state(copy::file_slice(slice, *g)));
                        }
                        said
                    }
                })
                .collect(),
        );
        if let Some(process) = &self.process {
            facts.extend(process.facts(self.typed.as_ref()));
        }
        match &finish.outcome {
            ToolOutcome::Completed { exit, .. } => {
                if let Some(ExitStatus::Signal { signal }) = exit {
                    facts.push(Said::error(copy::signal(*signal)));
                }
                if let Some(link) = self.linked() {
                    facts.push(Said::state(link.state.clone()));
                    if let Some(cwd) = link.cwd.as_ref().filter(|c| !c.trim().is_empty()) {
                        facts.push(Said::state(copy::spawned_in(cwd)));
                    }
                } else if !summary_only && !finish.describes_a_file() && self.diff.is_none() {
                    facts.push(clock);
                }
                let (glyph, tone) = if exit_failed(&finish.outcome) {
                    (g.failed, Token::Error)
                } else {
                    (g.done, Token::Ok)
                };
                call(glyph, Look::fg(tone), false, text, facts)
            }
            ToolOutcome::Cancelled if self.interrupted => call(
                g.stopped,
                muted,
                false,
                text,
                named(vec![
                    Said::state(copy::INTERRUPTED_FACT),
                    Said::count(copy::STOPPED_WHILE_IT_RAN),
                ]),
            ),
            ToolOutcome::Cancelled => {
                facts.insert(lead, Said::state(copy::STOPPED_FACT));
                facts.push(clock);
                call(g.stopped, muted, false, text, facts)
            }
            ToolOutcome::TimedOut { after_ms } => {
                facts.insert(lead, Said::state(copy::TIMED_OUT_FACT));
                facts.push(Said::time(Self::clock(*after_ms)));
                call(g.stopped, muted, false, text, facts)
            }
            ToolOutcome::NotRun { reason } => unrun(vec![
                Said::state(copy::NOT_RUN_FACT),
                Said::count(copy::not_run(reason)),
            ]),
            ToolOutcome::InvalidInput { .. }
            | ToolOutcome::Failed {
                before_start: true, ..
            } => call(
                g.failed,
                failed_mark,
                false,
                text,
                named(vec![Said::error(copy::COULD_NOT_START)]),
            ),
            ToolOutcome::Failed { .. } => {
                facts.insert(lead, Said::error(copy::FAILED_FACT));
                facts.push(clock);
                call(g.failed, failed_mark, false, text, facts)
            }
            ToolOutcome::Backgrounded { reason } => self.background_head(*reason, finish, pen),
        }
    }

    fn background_head(&self, reason: BackgroundReason, finish: &Finish, pen: &Pen) -> Head {
        let g = &pen.glyphs;
        let text = Look::fg(Token::Text);
        let id = self.job_id().or_else(|| {
            finish.facts.iter().find_map(|f| match f {
                Fact::Process { id } => Some(*id),
                _ => None,
            })
        });
        let end = self.job.as_ref().and_then(|j| j.end.as_ref());
        let restored = self.job.as_ref().is_some_and(|j| j.restored);
        let kept = match end {
            Some(_) => Keeps::No,
            None => self.job.as_ref().map_or(Keeps::No, |j| j.kept),
        };
        let (glyph, tone) = match end {
            Some(end) => end_mark(end, g),
            None if restored && kept != Keeps::Running => (g.stopped, Token::TextMuted),
            None => (g.moved, Token::Accent),
        };
        let mut facts: Vec<Said> = id
            .map(|id| Said::state(copy::process_name(id)))
            .into_iter()
            .collect();
        match end {
            Some(end) => {
                let words = copy::process_end_words(end);
                facts.push(match end_mark(end, g).1 {
                    Token::Error => Said::error(words),
                    _ => Said::state(words),
                });
            }
            None if kept == Keeps::Unchecked => {
                facts.push(Said::state(copy::MAY_STILL_RUN_FACT));
            }
            None if restored && kept == Keeps::No => {}
            None => facts.push(Said::state(match reason {
                BackgroundReason::Quiet => copy::WAITING_FACT,
                BackgroundReason::Started
                | BackgroundReason::MovedByUser
                | BackgroundReason::StillRunning => copy::RUNNING_FACT,
            })),
        }
        if reason == BackgroundReason::StillRunning && end.is_none() && !restored {
            facts.push(Said::state(copy::PS_STOPS_IT));
        }
        if kept == Keeps::Running {
            facts.push(Said::state(copy::KEEPS_RUNNING_FACT));
        }
        if restored {
            facts.push(Said::state(copy::BEFORE_RESTART));
        }
        let running = end.is_none() && !restored;
        Head {
            glyph: glyph.to_owned(),
            glyph_look: Look::fg(tone),
            title: self.title_spans(running, text, pen),
            command: self.command(running, pen),
            extra: self.extra(),
            facts,
        }
    }

    fn row_line(&self, r: &Renderer<'_>, pen: &Pen, show: Show) -> StyledLine {
        let head = self.head(pen, show);
        let lead = pen.mark(&head.glyph, head.glyph_look);
        let sep = pen.sep();
        let pens = Pens {
            edge: r.layout().content_edge(),
            mode: pen.mode,
            sep: &sep,
            more: pen.glyphs.more,
        };
        trim_end(lay_out(head, lead, &pens)).opening(Opens::Tool)
    }

    fn output_indent(pen: &Pen) -> usize {
        if pen.gutter == 0 {
            0
        } else {
            pen.gutter + OUTPUT_INDENT
        }
    }

    fn long_lines(pen: &Pen, r: &Renderer<'_>, text: &str, look: Look) -> Vec<StyledLine> {
        Self::led_lines(pen, r, None, text, look)
    }

    fn output_room(pen: &Pen, r: &Renderer<'_>) -> usize {
        if pen.gutter == 0 {
            r.layout().width.max(1)
        } else {
            r.layout().wide().saturating_sub(OUTPUT_INDENT).max(1)
        }
    }

    fn led_lines(
        pen: &Pen,
        r: &Renderer<'_>,
        lead: Option<StyledSpan>,
        text: &str,
        look: Look,
    ) -> Vec<StyledLine> {
        let room = Self::output_room(pen, r);
        let text = expand_tabs(text, TAB_STOP, pen.mode);
        let text = text.trim_end();
        if text.is_empty() && lead.is_none() {
            return vec![StyledLine::default()];
        }
        let fits = if plain_ascii(text) {
            text.len() <= room
        } else {
            crate::text::width(text, pen.mode) <= room
        };
        if lead.is_none() && fits {
            return vec![pen.row(
                vec![StyledSpan::plain(" ".repeat(Self::output_indent(pen)))],
                StyledLine::new(vec![StyledSpan::new(text, look)]),
            )];
        }
        let body = text.trim_start();
        let mut head: Vec<StyledSpan> = lead.into_iter().collect();
        head.push(StyledSpan::new(&text[..text.len() - body.len()], look));
        let body = StyledSpan::new(body, look);
        if spans_width(&head, pen.mode) * 2 > room {
            head.push(body);
            return Self::hung_lines(pen, r, vec![], head);
        }
        Self::hung_lines(pen, r, head, vec![body])
    }

    fn hung_lines(
        pen: &Pen,
        r: &Renderer<'_>,
        lead: Vec<StyledSpan>,
        body: Vec<StyledSpan>,
    ) -> Vec<StyledLine> {
        let indent = StyledSpan::plain(" ".repeat(Self::output_indent(pen)));
        let hang = spans_width(&lead, pen.mode);
        let room = Self::output_room(pen, r).saturating_sub(hang).max(1);
        let mut first = Some(lead);
        wrap(&body, room, pen.mode)
            .into_iter()
            .map(|row| {
                let lead = first
                    .take()
                    .unwrap_or_else(|| vec![StyledSpan::plain(" ".repeat(hang))]);
                pen.row([vec![indent.clone()], lead].concat(), row)
            })
            .collect()
    }

    fn answer_rows(&self, pen: &Pen, r: &Renderer<'_>) -> Vec<StyledLine> {
        let Some(asked) = self
            .asked
            .as_ref()
            .filter(|a| !self.asking(a) && !self.waiting(a))
        else {
            return vec![];
        };
        let muted = Look::fg(Token::TextMuted);
        let g = &pen.glyphs;
        let mut out = vec![];
        for (i, q) in asked.questions.iter().enumerate() {
            out.extend(Self::todo_lines(
                pen,
                r,
                StyledSpan::plain(""),
                &q.question,
                muted,
            ));
            if let Some(answer) = asked.answers().get(i) {
                let look = if answer.is_skipped() {
                    muted
                } else {
                    Look::fg(Token::Text)
                };
                let mark = StyledSpan::new(format!("{} ", g.user), Look::fg(Token::User));
                out.extend(Self::todo_lines(
                    pen,
                    r,
                    mark,
                    &answer_words(q, answer, g.set),
                    look,
                ));
            }
        }
        out
    }

    fn question_full(&self, asked: &Asked, pen: &Pen, r: &Renderer<'_>) -> Vec<StyledLine> {
        let muted = Look::fg(Token::TextMuted);
        let text = Look::fg(Token::Text);
        let g = &pen.glyphs;
        let mut out = vec![];
        for (i, q) in asked.questions.iter().enumerate() {
            if i > 0 {
                out.push(StyledLine::default());
            }
            let title = format!(
                "[{}] {}",
                cut_header(&q.header, g.set),
                one_line(&q.question)
            );
            out.extend(Self::long_lines(pen, r, &title, text));
            let answer = asked.answers().get(i);
            for (j, option) in q.options.iter().enumerate() {
                let index = u32::try_from(j).unwrap_or(u32::MAX);
                let chosen = answer.is_some_and(|a| a.chosen.contains(&index));
                let look = if chosen { Look::fg(Token::Ok) } else { text };
                let mark = StyledSpan::new(format!("{} ", if chosen { g.done } else { " " }), look);
                let line = format!("{}. {}", j + 1, one_line(&option.label));
                out.extend(Self::led_lines(pen, r, Some(mark), &line, look));
                let description = one_line(&option.description);
                if !description.trim().is_empty() {
                    out.extend(Self::long_lines(
                        pen,
                        r,
                        &format!("     {description}"),
                        muted,
                    ));
                }
            }
            let ending = match (&asked.end, answer) {
                (Some(QuestionEnd::Answered { .. }), Some(a)) => {
                    let mut words = vec![];
                    if a.is_skipped() {
                        words.push(copy::SKIPPED.to_owned());
                    }
                    if let Some(own) = &a.own {
                        for line in own.lines() {
                            words.push(format!(
                                "{}{}",
                                if q.is_typed() { "" } else { copy::OWN_SHORT },
                                one_line(line)
                            ));
                        }
                    }
                    words
                }
                (Some(QuestionEnd::Stopped), _) if asked.late => {
                    vec![copy::ANSWER_CLOSED.to_owned()]
                }
                (Some(QuestionEnd::Stopped), _) => vec![copy::ANSWER_STOPPED.to_owned()],
                (Some(QuestionEnd::MessageInstead), _) => {
                    vec![copy::ANSWER_MESSAGE_INSTEAD.to_owned()]
                }
                (Some(QuestionEnd::Interrupted), _) => vec![copy::ANSWER_INTERRUPTED.to_owned()],
                (Some(QuestionEnd::Withdrawn), _) => vec![copy::ANSWER_WITHDRAWN.to_owned()],
                (None, _) if self.asking(asked) => vec![],
                (None, _) if self.waiting(asked) => vec![copy::waiting_fact()],
                _ => vec![copy::ANSWER_STOPPED.to_owned()],
            };
            for line in ending {
                out.extend(Self::long_lines(pen, r, &line, muted));
            }
        }
        out
    }

    pub(super) fn is_call_of(&self, tool: &str, target: &str) -> bool {
        self.name == tool && self.target() == Some(target)
    }

    pub(super) fn link_agent(&mut self, link: Option<AgentLink>) -> bool {
        if self.agent.as_deref() == link.as_ref() {
            return false;
        }
        self.agent = link.map(Box::new);
        true
    }

    fn linked(&self) -> Option<&AgentLink> {
        let finish = self.finish.as_ref()?;
        let started = matches!(finish.outcome, ToolOutcome::Completed { .. })
            && !exit_failed(&finish.outcome)
            && finish.refusal().is_none()
            && self.withdrawn.is_none();
        self.agent.as_deref().filter(|_| started)
    }

    fn agent_rows(&self, pen: &Pen, r: &Renderer<'_>) -> Vec<StyledLine> {
        let Some(report) = self.linked().and_then(|l| l.report.as_deref()) else {
            return Vec::new();
        };
        let muted = Look::fg(Token::TextMuted);
        Self::todo_lines(pen, r, StyledSpan::plain(""), report, muted)
    }

    pub(super) fn set_todos(&mut self, items: &[TodoItem]) {
        self.todos = items.to_vec();
    }

    pub(super) fn todo_rows(&self, pen: &Pen, r: &Renderer<'_>) -> Vec<StyledLine> {
        let g = &pen.glyphs;
        let text = Look::fg(Token::Text);
        let muted = Look::fg(Token::TextMuted);
        let mut section = None;
        let mut rows = Vec::new();
        for item in &self.todos {
            if item.section.as_deref() != section {
                section = item.section.as_deref();
                if let Some(name) = section {
                    rows.extend(Self::todo_lines(
                        pen,
                        r,
                        StyledSpan::new("", muted),
                        name,
                        muted.bold(),
                    ));
                }
            }
            let (glyph, glyph_look, look) = match item.status {
                TodoStatus::Completed => (g.done, Look::fg(Token::Ok), muted),
                TodoStatus::InProgress => (g.running, Look::fg(Token::Accent), text.bold()),
                TodoStatus::Pending => (g.waiting, muted, text),
                TodoStatus::Cancelled => (g.minus, muted, muted.strike()),
            };
            let step = if section.is_some() { "  " } else { "" };
            let mark = format!("{step}{glyph} ");
            rows.extend(Self::todo_lines(
                pen,
                r,
                StyledSpan::new(mark, glyph_look),
                &item.text,
                look,
            ));
        }
        rows
    }

    fn todo_lines(
        pen: &Pen,
        r: &Renderer<'_>,
        mark: StyledSpan,
        text: &str,
        look: Look,
    ) -> Vec<StyledLine> {
        let body = StyledSpan::new(one_line(text).trim().to_owned(), look);
        Self::hung_lines(pen, r, vec![mark], vec![body])
    }

    pub(super) fn diff_mut(&mut self) -> Option<&mut DiffBody> {
        self.diff.as_deref_mut()
    }

    pub(super) fn diff(&self) -> Option<&DiffBody> {
        self.diff.as_deref()
    }

    pub(super) fn code(&self) -> Option<&str> {
        self.diff.as_deref().and_then(DiffBody::code)
    }

    pub(super) fn set_copy_label(&mut self, label: CopyLabel) -> bool {
        self.diff
            .as_deref_mut()
            .is_some_and(|diff| diff.set_label(label))
    }

    pub(super) fn kept_version(&self) -> u64 {
        self.diff.as_deref().map_or(0, DiffBody::kept_version)
    }

    pub(super) fn hooked(&mut self, hook: &str, change: Hooked) {
        self.hooked = Some(Box::new((one_line(hook), change)));
    }

    pub(super) fn shows_typed(&self) -> bool {
        !self.process_like() || echoed(self.typed.as_ref())
    }

    pub(super) fn typed(&self) -> Option<Typed> {
        self.typed
    }

    fn hooked_text(&self, change: &Hooked, pen: &Pen) -> String {
        match change {
            Hooked::Input { before, after } if self.process_like() => {
                input_change(before, after, self.shows_typed(), pen)
            }
            Hooked::Input { after, .. } => {
                sanitize(&serde_json::to_string_pretty(after).unwrap_or_default())
            }
            Hooked::Text(text) => sanitize(text),
        }
    }

    fn hook_rows(&self, pen: &Pen, r: &Renderer<'_>, full: bool) -> Vec<StyledLine> {
        let muted = Look::fg(Token::TextMuted);
        let mut out = Vec::new();
        if let Some((hook, change)) = self.hooked.as_deref() {
            out.extend(Self::long_lines(pen, r, &copy::rewritten_by(hook), muted));
            if full {
                out.extend(Self::long_lines(
                    pen,
                    r,
                    &self.hooked_text(change, pen),
                    muted,
                ));
            }
        }
        out
    }

    fn raw_rows(&self, r: &Renderer<'_>, pen: &Pen, show: Show) -> Vec<StyledLine> {
        let mut out = vec![self.row_line(r, pen, show)];
        out.extend(self.hook_rows(pen, r, show.detail == Detail::Full));
        match show.detail {
            Detail::Scrollback => {
                if self.purpose == ToolPurpose::Show
                    && let Some(diff) = &self.diff
                {
                    out.extend(diff.rows(r, pen, true, show.mouse.then(|| diff.label())));
                }
                out.extend(self.todo_rows(pen, r));
                out.extend(self.agent_rows(pen, r));
                out.extend(self.answer_rows(pen, r));
            }
            Detail::Full => out.extend(self.full_body(r, pen, show).0),
        }
        out
    }

    fn full_body(&self, r: &Renderer<'_>, pen: &Pen, show: Show) -> (Vec<StyledLine>, usize) {
        match (&self.diff, &self.asked) {
            (Some(diff), _) if self.purpose == ToolPurpose::Show => {
                (diff.rows(r, pen, false, None), 0)
            }
            (_, Some(asked)) => (self.question_full(asked, pen, r), 0),
            _ => self.page_body(r, pen, show),
        }
    }

    pub(super) fn rows(&self, r: &Renderer<'_>, pen: &Pen, show: Show) -> Vec<StyledLine> {
        let width = r.layout().width.max(1);
        self.raw_rows(r, pen, show)
            .into_iter()
            .map(|row| clip(row, width, pen.mode, pen.glyphs.more))
            .collect()
    }

    pub(super) fn entry(&self, pen: &Pen, show: Show, width: usize) -> Entry {
        let head = self.head(pen, show);
        let filter = self.filter_of(&head, pen);
        let lead = vec![
            StyledSpan::new(head.glyph.clone(), head.glyph_look),
            StyledSpan::plain(" "),
        ];
        let sep = pen.sep();
        let pens = Pens {
            edge: width.max(1),
            mode: pen.mode,
            sep: &sep,
            more: pen.glyphs.more,
        };
        Entry {
            line: trim_end(lay_out(head, lead, &pens)),
            filter,
            live: matches!(self.state, ToolState::Preparing | ToolState::Running),
        }
    }

    fn filter_of(&self, head: &Head, pen: &Pen) -> String {
        let (facts, _) = entry_facts(&head.facts, &pen.sep());
        let mut words = vec![spans_text(&head.title)];
        words.extend(self.full.clone().or_else(|| head.command.clone()));
        words.extend(self.program.clone());
        words.push(spans_text(&facts));
        if head.glyph == pen.glyphs.failed && !words.iter().any(|w| w.contains(copy::FAILED_FACT)) {
            words.push(copy::FAILED_FACT.to_owned());
        }
        if let Some(diff) = &self.diff {
            words.extend(diff.paths().map(str::to_owned));
        }
        words.retain(|w| !w.trim().is_empty());
        words.join(" ")
    }

    pub(super) fn page_title(&self, r: &Renderer<'_>, pen: &Pen, scrollback: Show) -> StyledLine {
        let width = r.layout().width.max(1);
        let mut title = clip(
            self.row_line(r, pen, scrollback),
            width,
            pen.mode,
            pen.glyphs.more,
        );
        title.opens = None;
        title
    }

    pub(super) fn page(
        &self,
        r: &Renderer<'_>,
        pen: &Pen,
        (scrollback, full): (Show, Show),
    ) -> (StyledLine, Vec<StyledLine>, usize) {
        let title = self.page_title(r, pen, scrollback);
        let width = r.layout().width.max(1);
        let (body, output) = self.full_body(r, pen, full);
        let body = body
            .into_iter()
            .map(|row| clip(row, width, pen.mode, pen.glyphs.more))
            .collect();
        (title, body, output)
    }
}

fn clean_outcome(outcome: &ToolOutcome) -> ToolOutcome {
    match outcome {
        ToolOutcome::InvalidInput { message } => ToolOutcome::InvalidInput {
            message: sanitize(message),
        },
        ToolOutcome::Failed {
            message,
            before_start,
        } => ToolOutcome::Failed {
            message: sanitize(message),
            before_start: *before_start,
        },
        ToolOutcome::NotRun {
            reason:
                NotRunReason::ReplyStopped {
                    stop_reason: StopReason::Other(r),
                },
        } => ToolOutcome::NotRun {
            reason: NotRunReason::ReplyStopped {
                stop_reason: StopReason::Other(one_line(r)),
            },
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests;
