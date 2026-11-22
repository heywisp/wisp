use std::collections::BTreeMap;
use std::path::Path;

use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::{
    AttachmentRef, ExitStatus, HookChange, Question, QuestionEnd, ToolOutcome, ToolOutputView,
};
use wisp_tool_text::output::{
    Captured, HELD_KEY_NOTE, invalid_input_text, seconds, trim_one_newline, with_held_key_note,
};

use crate::agent::not_run_text;
use crate::call::result_text;
use crate::plugin::hooks::points::AFTER_TOOL;
use crate::tool::question::{self, INTERRUPTED};
use crate::workspace::shorten_path;

pub const UNKNOWN: &str =
    "Unknown: Wisp stopped while this call was running; its effects are unknown.";
pub const NOT_RUN: &str = "Not run: Wisp stopped before this call ran.";
const RECOVERED: &str = "Recovered after Wisp stopped: ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairedCall {
    pub call_id: String,
    pub text: String,
    pub is_error: bool,
    pub interrupted_question: bool,
    pub attachments: Vec<AttachmentRef>,
}

struct Finished<'a> {
    outcome: &'a ToolOutcome,
    output: &'a ToolOutputView,
    result: Option<&'a String>,
    notes: Option<&'a Vec<String>>,
    attachments: &'a [AttachmentRef],
}

#[derive(Default)]
struct Facts<'a> {
    started: bool,
    process: Option<u32>,
    finished: Option<Finished<'a>>,
    questions: Option<&'a [Question]>,
    closed: Option<&'a QuestionEnd>,
    hooked: Option<&'a String>,
}

pub struct Context<'a> {
    pub home: Option<&'a Path>,
    pub exists: &'a dyn Fn(&str) -> bool,
    pub saves_output: &'a dyn Fn(&str) -> bool,
}

fn facts<'a>(records: &'a [LogRecord], calls: &[(String, String)]) -> BTreeMap<&'a str, Facts<'a>> {
    let mut facts: BTreeMap<&str, Facts<'_>> = BTreeMap::new();
    let wanted = |id: &str| calls.iter().any(|(call, _)| call == id);
    for record in records {
        match &record.event {
            LogEvent::ToolCallStarted { call_id, .. } if wanted(call_id) => {
                facts.entry(call_id).or_default().started = true;
            }
            LogEvent::ToolCallFinished {
                call_id,
                outcome,
                output,
                result,
                notes,
                attachments,
                ..
            } if wanted(call_id) => {
                facts.entry(call_id).or_default().finished = Some(Finished {
                    outcome,
                    output,
                    result: result.as_ref(),
                    notes: notes.as_ref(),
                    attachments,
                });
            }
            LogEvent::ProcessStarted { call_id, id, .. } if wanted(call_id) => {
                let facts = facts.entry(call_id).or_default();
                facts.started = true;
                facts.process = Some(*id);
            }
            LogEvent::QuestionAsked {
                call_id, questions, ..
            } if wanted(call_id) => {
                let facts = facts.entry(call_id).or_default();
                facts.questions = Some(questions);
                facts.closed = None;
            }
            LogEvent::QuestionClosed { call_id, how, .. } if wanted(call_id) => {
                facts.entry(call_id).or_default().closed = Some(how);
            }
            LogEvent::HookApplied {
                point,
                call_id: Some(call_id),
                change: HookChange::Result { after, .. },
                ..
            } if point == AFTER_TOOL && wanted(call_id) => {
                facts.entry(call_id).or_default().hooked = Some(after);
            }
            _ => {}
        }
    }
    facts
}

fn whole(output: &ToolOutputView) -> Option<Captured> {
    let covered = u64::try_from(output.tail.len()).ok() == Some(output.bytes);
    (output.inlined && covered).then(|| Captured {
        head: output.tail.clone(),
        tail: output.tail.clone(),
        bytes: output.bytes,
        lines: output.lines,
        path: None,
        problem: None,
    })
}

fn exact(finished: &Finished<'_>, notes: &[String], saves_output: bool) -> Option<String> {
    match finished.outcome {
        ToolOutcome::NotRun { reason } => Some(not_run_text(reason)),
        ToolOutcome::InvalidInput { message } => Some(invalid_input_text(message)),
        ToolOutcome::Failed { .. } if finished.output.bytes == 0 => Some(result_text(
            finished.outcome,
            &Captured::default(),
            None,
            notes,
        )),
        outcome if saves_output => {
            whole(finished.output).map(|captured| result_text(outcome, &captured, None, notes))
        }
        _ => None,
    }
}

pub fn recovered_text(
    outcome: &ToolOutcome,
    output: &ToolOutputView,
    notes: &[String],
    saves_output: bool,
    context: &Context<'_>,
) -> String {
    let what = match outcome {
        ToolOutcome::Completed {
            exit: Some(ExitStatus::Code { code }),
            ..
        } => format!("the command exited with code {code}, but its result wasn't saved."),
        ToolOutcome::Completed {
            exit: Some(ExitStatus::Signal { signal }),
            ..
        } => format!("the command was ended by signal {signal}, but its result wasn't saved."),
        ToolOutcome::Completed { exit: None, .. } => {
            "the call completed, but its result wasn't saved.".to_owned()
        }
        ToolOutcome::TimedOut { after_ms } => format!(
            "the call timed out after {} and was stopped.",
            seconds(*after_ms)
        ),
        ToolOutcome::Cancelled => "the call was stopped by the user.".to_owned(),
        ToolOutcome::Failed { message, .. } => format!("the call failed: {message}"),
        ToolOutcome::InvalidInput { message } => format!("the input was rejected: {message}"),
        ToolOutcome::NotRun { reason } => return not_run_text(reason),
        ToolOutcome::Backgrounded { .. } => {
            "the call moved to the background, but its result wasn't saved.".to_owned()
        }
    };
    let mut parts = vec![format!("{RECOVERED}{what}")];
    let shows_output = saves_output
        && !matches!(outcome, ToolOutcome::InvalidInput { .. })
        && (output.bytes > 0 || !matches!(outcome, ToolOutcome::Failed { .. }));
    if shows_output {
        parts.push(if output.bytes == 0 {
            "It printed no output.".to_owned()
        } else {
            format!(
                "Last lines of its output:\n{}",
                trim_one_newline(&output.tail)
            )
        });
    }
    parts.extend(notes.iter().cloned());
    if shows_output
        && output.bytes > 0
        && let Some(file) = output.file.as_deref().filter(|f| (context.exists)(f))
    {
        parts.push(format!(
            "Full output: {}",
            shorten_path(Path::new(file), context.home)
        ));
    }
    parts.join("\n")
}

fn repaired(
    call: &(String, String),
    facts: Option<&Facts<'_>>,
    context: &Context<'_>,
) -> RepairedCall {
    let (call_id, tool) = call;
    let masked = facts
        .and_then(|facts| facts.finished.as_ref())
        .and_then(|finished| finished.notes)
        .is_some_and(|notes| notes.iter().any(|note| note == HELD_KEY_NOTE));
    let made = |text: String, is_error: bool| RepairedCall {
        call_id: call_id.clone(),
        text: if masked {
            with_held_key_note(&text)
        } else {
            text
        },
        is_error,
        interrupted_question: false,
        attachments: Vec::new(),
    };
    let Some(facts) = facts else {
        return made(NOT_RUN.to_owned(), true);
    };
    if let Some(finished) = &facts.finished
        && let Some(result) = if masked {
            finished.result.or(facts.hooked)
        } else {
            facts.hooked.or(finished.result)
        }
    {
        return RepairedCall {
            attachments: finished.attachments.to_vec(),
            ..made(result.clone(), finished.outcome.is_error())
        };
    }
    if facts.finished.is_none()
        && let Some(questions) = facts.questions
    {
        if questions
            .iter()
            .any(wisp_protocol::Question::kept_from_model)
        {
            return RepairedCall {
                interrupted_question: facts.closed.is_none(),
                ..made(UNKNOWN.to_owned(), true)
            };
        }
        return match facts.closed {
            None => RepairedCall {
                interrupted_question: true,
                ..made(INTERRUPTED.to_owned(), true)
            },
            Some(how) => {
                let is_error = matches!(
                    how,
                    QuestionEnd::Stopped | QuestionEnd::Interrupted | QuestionEnd::Withdrawn
                );
                made(question::result_text(questions, how), is_error)
            }
        };
    }
    let saves_output = (context.saves_output)(tool);
    match &facts.finished {
        Some(finished) => {
            let is_error = finished.outcome.is_error();
            let logged = finished.notes.map(Vec::as_slice);
            let text = logged
                .and_then(|notes| exact(finished, notes, saves_output))
                .unwrap_or_else(|| {
                    recovered_text(
                        finished.outcome,
                        finished.output,
                        logged.unwrap_or_default(),
                        saves_output,
                        context,
                    )
                });
            RepairedCall {
                attachments: finished.attachments.to_vec(),
                ..made(text, is_error)
            }
        }
        None if facts.started => made(
            match facts.process {
                Some(id) => format!("{UNKNOWN} It was running as process {id}."),
                None => UNKNOWN.to_owned(),
            },
            true,
        ),
        None => made(NOT_RUN.to_owned(), true),
    }
}

pub fn repair_calls(
    records: &[LogRecord],
    calls: &[(String, String)],
    context: &Context<'_>,
) -> Vec<RepairedCall> {
    let facts = facts(records, calls);
    calls
        .iter()
        .map(|call| repaired(call, facts.get(call.0.as_str()), context))
        .collect()
}

#[cfg(test)]
mod tests;
