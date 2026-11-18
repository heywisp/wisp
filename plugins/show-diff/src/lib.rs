use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use wisp_diff::{FileDiff, Repairs, diff_texts, parse_diff};
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{
    Action, DiffFile, DiffView, Diffstat, Display, Fact, OutputKind, ToolRun, View,
};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::code::{MAX_SIDE_BYTES, MAX_SIDE_LINES, check_side, line_count, size};
use wisp_tool_text::output::{cut_chars, group};

pub mod code;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(_host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(ShowDiff)
}

fn invalid(message: impl Into<String>) -> String {
    message.into()
}

pub const NAME: &str = "show_diff";
pub const MAX_FILES: usize = 20;
pub const MAX_TOTAL_BYTES: usize = 1024 * 1024;
pub const MAX_PATH_BYTES: usize = 4096;
pub const TITLE_CHARS: usize = 200;
pub const SYSTEM_LINE: &str = "You can show the user a change as a diff with the show_diff tool.";
const TIME_LIMIT_MS: u64 = 5_000;
const FALLBACK_TARGET_CHARS: usize = 80;

const DESCRIPTION: &str = "Show the user a diff in the chat. Use it when you want the user to \
see a change: one you propose, one you made, or a comparison they asked for. It only displays \
the diff: it changes no files and runs nothing, so it never applies a change. The user sees the \
diff with changed words and syntax highlighted, so don't repeat it in your reply.\n\nGive each \
file as `before` and `after` (the old and new text; Wisp computes the diff and shows 3 unchanged \
lines around each change) or as `diff` (a unified diff you wrote; wrong hunk line counts are \
recounted, and hunk headers without line numbers are accepted). Give `path` so the code gets \
syntax colours, or `language` when there is no path.";

pub const SHOWN: &str = "Shown to the user: ";

pub fn limits() -> String {
    format!(
        "{} lines and {}",
        group(MAX_SIDE_LINES as u64),
        size(MAX_SIDE_BYTES)
    )
}

pub fn description() -> String {
    format!(
        "{DESCRIPTION} Limits: {MAX_FILES} files, {} for each side of a file, {} in all.",
        limits(),
        size(MAX_TOTAL_BYTES)
    )
}

pub const NOTHING_CHANGED: &str = "nothing to show: before and after are the same in every file";
pub const NOTHING_IN_DIFFS: &str = "nothing to show: no file's diff has a + or - line";
pub const NOTHING_AT_ALL: &str = "nothing to show: no file has a change";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "files": {
                "type": "array",
                "description": "The files to show, in order, one entry per file.",
                "minItems": 1,
                "maxItems": MAX_FILES,
                "items": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The file's path, shown to the user."
                        },
                        "language": {
                            "type": "string",
                            "description": "The language for syntax colours when path doesn't show it, such as rust."
                        },
                        "before": {
                            "type": "string",
                            "description": "The old text. Use \"\" for a new file."
                        },
                        "after": {
                            "type": "string",
                            "description": "The new text. Use \"\" for a deleted file."
                        },
                        "diff": {
                            "type": "string",
                            "description": "A unified diff of this file, instead of before and after."
                        }
                    },
                    "additionalProperties": false
                }
            },
            "title": {
                "type": "string",
                "description": "A short heading for the diff, such as what the change does."
            }
        },
        "required": ["files"],
        "additionalProperties": false
    })
}

enum Form {
    Texts { before: String, after: String },
    Diff(String),
}

struct Entry {
    path: Option<String>,
    language: Option<String>,
    form: Form,
}

struct Input {
    title: Option<String>,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub display: Display,
    pub result: String,
    pub facts: Vec<Fact>,
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{what} must be an object")))
}

fn optional_text(
    map: &Map<String, Value>,
    field: &str,
    owner: &str,
) -> Result<Option<String>, String> {
    match map.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|s| Some(s.to_owned()))
            .ok_or_else(|| invalid(format!("{owner}`{field}` must be a string"))),
    }
}

fn check_path(path: &str, n: usize) -> Result<(), String> {
    if path.len() > MAX_PATH_BYTES {
        return Err(invalid(format!(
            "file {n}'s `path` is {} bytes; the limit is {} bytes",
            group(path.len() as u64),
            group(MAX_PATH_BYTES as u64)
        )));
    }
    if path.contains('\0') || path.contains('\n') || path.contains('\r') {
        return Err(invalid(format!(
            "file {n}'s `path` holds a NUL or a line break"
        )));
    }
    Ok(())
}

fn needs_one_form(n: usize) -> String {
    invalid(format!(
        "file {n} needs either diff, or both before and after (use \"\" for a new or deleted file)"
    ))
}

fn parse_entry(value: &Value, n: usize) -> Result<Entry, String> {
    let owner = format!("file {n}'s ");
    let map = object(value, &format!("file {n}"))?;
    if let Some(key) = map
        .keys()
        .find(|k| !["after", "before", "diff", "language", "path"].contains(&k.as_str()))
    {
        return Err(invalid(format!("unknown field `{key}` in file {n}")));
    }
    let path = optional_text(map, "path", &owner)?;
    if let Some(path) = &path {
        check_path(path, n)?;
    }
    let language = optional_text(map, "language", &owner)?;
    let before = optional_text(map, "before", &owner)?;
    let after = optional_text(map, "after", &owner)?;
    let diff = optional_text(map, "diff", &owner)?;
    let form = match (before, after, diff) {
        (Some(before), Some(after), None) => Form::Texts { before, after },
        (None, None, Some(diff)) => Form::Diff(diff),
        _ => return Err(needs_one_form(n)),
    };
    Ok(Entry {
        path: path.filter(|p| !p.trim().is_empty()),
        language: language.and_then(|l| first_line(&l)),
        form,
    })
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_owned())
}

fn title_of(text: &str) -> Option<String> {
    first_line(text).map(|line| cut_chars(&line, TITLE_CHARS))
}

fn parse(input: &Value) -> Result<Input, String> {
    let map = object(input, "input")?;
    if let Some(key) = map
        .keys()
        .find(|k| !["files", "title"].contains(&k.as_str()))
    {
        return Err(invalid(format!("unknown field `{key}`")));
    }
    let title = optional_text(map, "title", "")?.and_then(|t| title_of(&t));
    let items = map
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("`files` must be an array of one to 20 files"))?;
    if items.is_empty() {
        return Err(invalid("`files` is empty; give one to 20 files"));
    }
    if items.len() > MAX_FILES {
        return Err(invalid(format!(
            "`files` has {} entries; the limit is {MAX_FILES}",
            items.len()
        )));
    }
    let entries = items
        .iter()
        .enumerate()
        .map(|(i, item)| parse_entry(item, i + 1))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Input { title, entries })
}

fn total_bytes(entries: &[Entry]) -> usize {
    entries
        .iter()
        .map(|e| match &e.form {
            Form::Texts { before, after } => before.len() + after.len(),
            Form::Diff(diff) => diff.len(),
        })
        .sum()
}

fn diff_sides(files: &[FileDiff]) -> ((usize, usize), (usize, usize)) {
    let (mut old, mut new) = ((0, 0), (0, 0));
    for line in files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines) {
        match line {
            wisp_diff::DiffLine::Context(text) => {
                old = (old.0 + 1, old.1 + text.len() + 1);
                new = (new.0 + 1, new.1 + text.len() + 1);
            }
            wisp_diff::DiffLine::Removed(text) => old = (old.0 + 1, old.1 + text.len() + 1),
            wisp_diff::DiffLine::Added(text) => new = (new.0 + 1, new.1 + text.len() + 1),
            wisp_diff::DiffLine::NoNewline => {}
        }
    }
    (old, new)
}

struct Computed {
    files: Vec<FileDiff>,
    repairs: Repairs,
}

fn compute(entry: &Entry, n: usize) -> Result<Computed, String> {
    match &entry.form {
        Form::Texts { before, after } => {
            check_side(
                line_count(before),
                before.len(),
                &format!("file {n}'s before"),
            )?;
            check_side(line_count(after), after.len(), &format!("file {n}'s after"))?;
            Ok(Computed {
                files: vec![diff_texts(before, after)],
                repairs: Repairs::default(),
            })
        }
        Form::Diff(text) => {
            let parsed = parse_diff(text);
            let ((old_lines, old_bytes), (new_lines, new_bytes)) = diff_sides(&parsed.files);
            check_side(
                old_lines,
                old_bytes,
                &format!("file {n}'s diff on its old side"),
            )?;
            check_side(
                new_lines,
                new_bytes,
                &format!("file {n}'s diff on its new side"),
            )?;
            Ok(Computed {
                files: parsed.files,
                repairs: parsed.repairs,
            })
        }
    }
}

fn named(path: Option<&str>, n: usize) -> String {
    path.map_or_else(|| format!("file {n}"), str::to_owned)
}

fn lines_noun(n: u32) -> (&'static str, &'static str) {
    if n == 1 {
        ("line", "was")
    } else {
        ("lines", "were")
    }
}

#[derive(Default)]
struct Notes {
    recounted: Vec<String>,
    unprefixed: Vec<String>,
    split: Vec<String>,
    left_out: Vec<String>,
}

impl Notes {
    fn lines(self) -> Vec<String> {
        [self.recounted, self.unprefixed, self.split, self.left_out]
            .into_iter()
            .flatten()
            .collect()
    }
}

pub fn show(input: &Value) -> Result<Shown, String> {
    let Input { title, entries } = parse(input)?;
    let total = total_bytes(&entries);
    if total > MAX_TOTAL_BYTES {
        return Err(invalid(format!(
            "the files hold {} of text in all; the limit is {}",
            size(total),
            size(MAX_TOTAL_BYTES)
        )));
    }
    let mut notes = Notes::default();
    let mut views: Vec<DiffFile> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let n = i + 1;
        let computed = compute(entry, n)?;
        let subject = named(entry.path.as_deref(), n);
        if computed.repairs.recounted {
            notes.recounted.push(format!(
                "The hunk line counts in {subject} didn't match their lines; they were recounted."
            ));
        }
        if computed.repairs.unprefixed > 0 {
            let (noun, verb) = lines_noun(computed.repairs.unprefixed);
            notes.unprefixed.push(format!(
                "{} {noun} in {subject} had no +, - or space in front and {verb} shown as unchanged.",
                computed.repairs.unprefixed
            ));
        }
        let several = computed.files.len() > 1;
        if several {
            notes.split.push(format!(
                "The diff in file {n} held {} files; each was shown on its own.",
                computed.files.len()
            ));
        }
        if computed.files.is_empty() {
            notes
                .left_out
                .push(format!("{subject} had no changes and was left out."));
        }
        for file in computed.files {
            let path = file.path.clone().or_else(|| entry.path.clone());
            if !file.has_changes() {
                let left = named(path.as_deref(), n);
                notes
                    .left_out
                    .push(format!("{left} had no changes and was left out."));
                continue;
            }
            views.push(DiffFile {
                path,
                old_path: file.old_path.clone(),
                language: entry.language.clone(),
                hunks: file.text(),
                added: file.added,
                removed: file.removed,
            });
        }
    }
    if views.is_empty() {
        let texts = entries.iter().all(|e| matches!(e.form, Form::Texts { .. }));
        let diffs = entries.iter().all(|e| matches!(e.form, Form::Diff(_)));
        let message = match (texts, diffs) {
            (true, _) => NOTHING_CHANGED,
            (_, true) => NOTHING_IN_DIFFS,
            _ => NOTHING_AT_ALL,
        };
        return Err(invalid(message));
    }
    let added: u64 = views.iter().map(|v| v.added).sum();
    let removed: u64 = views.iter().map(|v| v.removed).sum();
    let single_path = match views.as_slice() {
        [only] => only.path.clone(),
        _ => None,
    };
    let subject = match (&single_path, views.len()) {
        (Some(path), _) => path.clone(),
        (None, 1) => "1 file".to_owned(),
        (None, n) => format!("{n} files"),
    };
    let mut result = format!("{SHOWN}{subject}, +{added} -{removed}.");
    for note in notes.lines() {
        result.push('\n');
        result.push_str(&note);
    }
    let target = match (&title, &single_path, views.len()) {
        (Some(title), _, _) => title.clone(),
        (None, Some(path), _) => path.clone(),
        (None, None, 1) => "1 file".to_owned(),
        (None, None, n) => format!("{n} files"),
    };
    let display = Display {
        action: Action::Show,
        verbs: None,
        target: cut_chars(&target, FALLBACK_TARGET_CHARS),
        output: OutputKind::None,
        view: Some(View::Diff(DiffView {
            title: title.clone(),
            files: views,
        })),
        title,
    };
    Ok(Shown {
        display,
        result,
        facts: vec![Fact::Diffstat(Diffstat { added, removed })],
    })
}

pub fn rejected(input: &Value) -> Display {
    let map = input.as_object();
    let title = map
        .and_then(|m| m.get("title"))
        .and_then(Value::as_str)
        .and_then(title_of);
    let path = || {
        map.and_then(|m| m.get("files"))
            .and_then(Value::as_array)
            .and_then(|files| files.first())
            .and_then(|first| first.get("path"))
            .and_then(Value::as_str)
            .and_then(title_of)
    };
    let target = title
        .clone()
        .or_else(path)
        .unwrap_or_else(|| "diff".to_owned());
    Display {
        action: Action::Show,
        verbs: None,
        target: cut_chars(&target, FALLBACK_TARGET_CHARS),
        output: OutputKind::None,
        view: None,
        title,
    }
}

pub fn meta(writing: &str) -> ToolMeta {
    ToolMeta {
        effect: Effect::ReadOnly,
        tags: Tags::default(),
        parallel_safe: true,
        timeouts: Timeouts {
            default_ms: TIME_LIMIT_MS,
            max_ms: Some(TIME_LIMIT_MS),
        },
        saves_output: false,
        needs_workspace: false,
        ends_reply: false,
        asks_user: false,
        reports_jobs: false,
        writing: Some(writing.to_owned()),
        purpose: Purpose::Show,
    }
}

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description(),
            input_schema: schema().to_string(),
            meta: meta("a diff"),
        }),
        system_line: Some(SYSTEM_LINE.to_owned()),
        context_line: None,
    }
}

pub struct ShowDiff;

impl Prepared for Shown {
    fn display(&self) -> Display {
        self.display.clone()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

fn parsed(input: &str) -> Result<Value, String> {
    serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))
}

pub fn finish(shown: Shown) -> ToolRun {
    ToolRun {
        facts: shown.facts,
        ..ToolRun::completed().with_result(shown.result)
    }
}

impl Plugin for ShowDiff {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { vec![def(), code::def()] })
    }

    fn prepare<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        let show = if tool == code::NAME { code::show } else { show };
        Box::pin(async move {
            let shown: Box<dyn Prepared> = Box::new(show(&parsed(input)?)?);
            Ok(shown)
        })
    }

    fn rejected_display<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        let rejected = if tool == code::NAME {
            code::rejected
        } else {
            rejected
        };
        Box::pin(std::future::ready(Some(rejected(
            &parsed(input).unwrap_or(Value::Null),
        ))))
    }

    fn time_limit_ms<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(Some(TIME_LIMIT_MS)))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        let run = match call.into_any().downcast::<Shown>() {
            Ok(shown) => finish(*shown),
            Err(_) => ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT),
        };
        Box::pin(std::future::ready(run))
    }
}

#[cfg(test)]
mod tests;
