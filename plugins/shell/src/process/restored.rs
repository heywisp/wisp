use std::path::Path;

use wisp_plugin_api::host::{
    Job, JobEnd, JobError, JobInfo, LostReason, Mode, SavedFile, Unreadable,
};
use wisp_plugin_api::types::{ReadKind, ToolRun};
use wisp_tool_text::jobs::{ended_sentence, kept_no_limit, kept_restored_refusal};
use wisp_tool_text::paths::shorten_path;

use super::input::{self, Call};
use super::run::{
    Env, answered, found, has_ended, lines_u32, refused, refused_as, screen_of_pipe, split_header,
};
use super::view;

pub const POSITION_LOST: &str = "The read position was lost when Wisp restarted.";

pub(crate) fn unreadable(saved: &SavedFile, home: Option<&Path>) -> Option<String> {
    let shown = |path: &str| shorten_path(Path::new(path), home);
    match saved {
        SavedFile::Readable(_) => None,
        SavedFile::NoneKept => Some(
            "Output file is missing: no output file was kept, so there is no output to read."
                .to_owned(),
        ),
        SavedFile::Missing(path) => Some(format!(
            "Output file is missing: {} no longer exists, so there is no output to read.",
            shown(path)
        )),
        SavedFile::Unreadable(Unreadable { path, error }) => Some(format!(
            "Output file can't be read: {}: {error}.",
            shown(path)
        )),
    }
}

pub(super) async fn answer(
    id: u32,
    job: &dyn Job,
    info: &JobInfo,
    call: &Call,
    env: &Env<'_>,
) -> ToolRun {
    if info.keep && info.end.is_none() {
        return kept(id, job, info, call, env).await;
    }
    if call.changes() {
        return has_ended(id);
    }
    let kind = call.read_kind();
    let end = info.end.clone().unwrap_or(JobEnd::Lost(LostReason::Gone));
    let first = ended_sentence(id, &end);
    if kind == ReadKind::Screen {
        if info.mode == Mode::Pipe {
            return screen_of_pipe(id);
        }
        return answered(
            Some(id),
            format!("{first} Its screen was lost when Wisp restarted."),
        );
    }
    let home = env.home();
    let saved = job.saved().await.unwrap_or(SavedFile::NoneKept);
    if let Some(problem) = unreadable(&saved, home.as_deref()) {
        return answered(Some(id), format!("{first}\n{problem}"));
    }
    let shown = match &saved {
        SavedFile::Readable(path) => Some(shorten_path(Path::new(path), home.as_deref())),
        _ => None,
    };
    let body = match kind {
        ReadKind::Search => {
            let (header, rest) = split_header(found(job, call).await.unwrap_or_default());
            let file_line = file_line(shown.as_deref(), rest.as_deref().unwrap_or(&header));
            let text = std::iter::once(format!("{first} {header}"))
                .chain(rest)
                .chain(file_line)
                .collect::<Vec<_>>()
                .join("\n");
            return answered(Some(id), text);
        }
        ReadKind::Lines => {
            let from = call.from.unwrap_or(1);
            let n = call.lines.unwrap_or(input::RANGE_LINES);
            view::rows_text(&job.lines(from, lines_u32(n)).await, from)
        }
        ReadKind::New | ReadKind::Tail | ReadKind::Screen => {
            let n = call.lines.unwrap_or(input::TAIL_LINES);
            view::tail_text(&job.tail(lines_u32(n)).await, home.as_deref())
        }
    };
    let note = (kind == ReadKind::New).then(|| POSITION_LOST.to_owned());
    let file_line = file_line(shown.as_deref(), &body);
    let text = std::iter::once(first)
        .chain((!body.is_empty()).then_some(body))
        .chain(file_line)
        .chain(note)
        .collect::<Vec<_>>()
        .join("\n");
    answered(Some(id), text)
}

async fn kept(id: u32, job: &dyn Job, info: &JobInfo, call: &Call, env: &Env<'_>) -> ToolRun {
    if call.timeout_ms.is_some_and(|ms| ms > 0) {
        return refused(kept_no_limit(id));
    }
    let log = info.log.as_deref().map(|log| env.host.shorten(log));
    let Some(kind) = call
        .signal
        .filter(|_| !call.writes() && call.size.is_none())
    else {
        return refused_as(kept_restored_refusal(id, log.as_deref()));
    };
    let written = job.signal(kind, 0).await;
    match written.failed {
        Some(JobError::Invalid(message)) => refused_as(message),
        Some(_) => has_ended(id),
        None => answered(Some(id), format!("Process {id} was stopped.")),
    }
}

fn file_line(shown: Option<&str>, body: &str) -> Option<String> {
    shown
        .filter(|path| !body.contains(path))
        .map(|path| format!("Output file: {path}"))
}
