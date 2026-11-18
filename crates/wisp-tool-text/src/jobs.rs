use wisp_plugin_api::host::{JobEnd, StoppedBy};
use wisp_plugin_api::prompt::{Item, KeptJob, LostJob, RewoundJob};
use wisp_plugin_api::types::ExitStatus;

use crate::output::{INLINE_LIMIT, and_list, count, cut_chars, line_range, seconds};

pub const LIST_TARGET_CHARS: usize = 60;
pub const SHOWN_LINE: usize = 1024;
pub const READ_LIMIT: usize = INLINE_LIMIT;
pub const SEPARATOR: &str = "--";

pub fn are(a: u64, b: u64) -> &'static str {
    if a == b { "is" } else { "are" }
}

pub fn gap_text(a: u64, b: u64) -> String {
    format!("[… {} {} no longer kept …]", line_range(a, b), are(a, b))
}

pub fn cut_line(text: &str) -> String {
    if text.len() <= SHOWN_LINE {
        return text.to_owned();
    }
    let mut at = SHOWN_LINE;
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    format!(
        "{}[… {} more on this line …]",
        &text[..at],
        count((text.len() - at) as u64, "byte")
    )
}

pub const KEEP_IN_CHILD: &str = "`keep_running` isn't available to a child agent, whose processes end with it while Wisp keeps running";
pub const KEEP_NEEDS_BACKGROUND: &str = "`keep_running` needs `run_in_background`";
pub const KEEP_NO_TTY: &str = "a terminal process can't outlive Wisp, which owns its terminal; run it without tty and talk to it over a socket or port, for example a VM's serial console on a telnet port";
pub const KEEP_NO_LIMIT_WHY: &str = "nothing enforces a limit once Wisp has exited";
pub const KEPT_WORDS: &str = "running, keeps running after Wisp exits";
pub const KEPT_RESTORED_LIST: &str = "started before the restart with keep_running";

pub fn keep_no_limit() -> String {
    format!("`keep_running` takes no `timeout_ms` above 0: {KEEP_NO_LIMIT_WHY}")
}

pub fn kept_no_limit(id: u32) -> String {
    format!(
        "process {id} keeps running after Wisp exits, so it takes no time limit: {KEEP_NO_LIMIT_WHY}"
    )
}

pub fn kept_result_line(log: &str) -> String {
    format!(
        "It keeps running after Wisp exits; its output goes to {log}. Stop it with the process tool."
    )
}

pub fn kept_restored_refusal(id: u32, log: Option<&str>) -> String {
    let lead = format!("Process {id} was started before the restart, so Wisp can only stop it");
    match log {
        Some(log) => format!("{lead}; read its log with tail or grep in bash: {log}"),
        None => format!("{lead}."),
    }
}

pub fn unchecked_refusal(id: u32) -> String {
    format!(
        "Process {id} may still be running from before the restart, but Wisp can't check here that it is the process it started, so it won't signal it."
    )
}

pub fn no_process(id: impl std::fmt::Display) -> String {
    format!("there is no process {id}")
}

pub fn numbered(id: u32, label: Option<&str>) -> String {
    match label {
        Some(label) => format!("{id} ({})", cut_chars(label, LIST_TARGET_CHARS)),
        None => id.to_string(),
    }
}

pub fn kept_gone_line(id: u32, target: Option<&str>) -> String {
    format!(
        "Process {}, kept running from before the restart, is no longer running.",
        numbered(id, target)
    )
}

pub fn typed_line(id: u32) -> String {
    format!("You typed into process {id} (the user attached to it).")
}

pub fn ended_sentence(id: u32, end: &JobEnd) -> String {
    ended_sentence_named(&format!("Process {id}"), end)
}

pub fn ended_sentence_named(subject: &str, end: &JobEnd) -> String {
    match end {
        JobEnd::Exited(ExitStatus::Code(code)) => format!("{subject} exited with code {code}."),
        JobEnd::Exited(ExitStatus::Signal(signal)) => {
            format!("{subject} was ended by signal {signal}.")
        }
        JobEnd::Stopped(by) => match by {
            StoppedBy::Model => format!("{subject} was stopped."),
            StoppedBy::User => format!("{subject} was stopped by the user."),
            StoppedBy::SessionEnded => format!("{subject} was stopped when the session ended."),
            StoppedBy::TimeLimit(after_ms) => format!(
                "{subject} was stopped at its {} time limit.",
                seconds(*after_ms)
            ),
        },
        JobEnd::Lost(_) => format!("{subject} ended when Wisp restarted."),
        JobEnd::Failed(message) => format!("{subject} failed: {message}."),
    }
}

pub fn end_line(id: u32, target: &str, end: &JobEnd, unread: u64) -> String {
    let subject = format!("Process {}", numbered(id, Some(target)));
    let sentence = ended_sentence_named(&subject, end);
    if unread == 0 {
        return sentence;
    }
    let bare = sentence.strip_suffix('.').unwrap_or(&sentence);
    format!("{bare}; {} you haven't read.", count(unread, "line"))
}

pub const WAKE_LINE: &str =
    "The user didn't send this message; Wisp sent it because background processes ended.";

pub const STARTED_IN_REWOUND_TURNS: &str = "it was started in turns the user rewound";

fn listed(ids: &[u32]) -> String {
    and_list(&ids.iter().map(u32::to_string).collect::<Vec<_>>())
}

pub fn restarted_line(ids: &[u32]) -> Option<String> {
    match ids {
        [] => None,
        [id] => Some(format!(
            "Process {id} was still running when Wisp exited, so it has stopped; its output file \
remains if it still exists."
        )),
        _ => Some(format!(
            "Processes {} were still running when Wisp exited, so they have stopped; their output \
files remain where the files still exist.",
            listed(ids)
        )),
    }
}

pub fn kept_line(kept: &[KeptJob]) -> Option<String> {
    match kept {
        [] => None,
        [KeptJob { id, log }] => Some(format!(
            "Process {id} keeps running from before the restart (started with keep_running); its \
output goes to {log}."
        )),
        _ => {
            let ids: Vec<u32> = kept.iter().map(|k| k.id).collect();
            Some(format!(
                "Processes {} keep running from before the restart (started with keep_running); \
their output goes to their logs, listed by the process tool.",
                listed(&ids)
            ))
        }
    }
}

pub fn unchecked_line(ids: &[u32]) -> Option<String> {
    match ids {
        [] => None,
        [id] => Some(format!(
            "Process {id} was still running when Wisp exited, and Wisp can't check here whether it \
still runs, so it wasn't stopped; its output file remains if it still exists."
        )),
        _ => Some(format!(
            "Processes {} were still running when Wisp exited, and Wisp can't check here whether \
they still run, so they weren't stopped; their output files remain where the files still exist.",
            listed(ids)
        )),
    }
}

pub fn lost_line(job: &LostJob) -> String {
    let LostJob { id, target, end } = job;
    let subject = format!("Process {}", numbered(*id, target.as_deref()));
    match end {
        JobEnd::Lost(_) => kept_gone_line(*id, target.as_deref()),
        JobEnd::Failed(message) => {
            let message = message.trim_end_matches(['.', ' ']);
            format!("{subject} failed before Wisp restarted: {message}.")
        }
        _ => {
            let sentence = ended_sentence_named(&subject, end);
            let bare = sentence.strip_suffix('.').unwrap_or(&sentence);
            format!("{bare} before Wisp restarted.")
        }
    }
}

pub fn rewound_line(job: &RewoundJob) -> String {
    format!(
        "Process {} is running; {STARTED_IN_REWOUND_TURNS}.",
        numbered(job.id, Some(&job.target))
    )
}

pub fn item_line(item: &Item) -> Option<String> {
    match item {
        Item::Restarted(ids) => restarted_line(ids),
        Item::Kept(kept) => kept_line(kept),
        Item::Unchecked(ids) => unchecked_line(ids),
        Item::Lost(job) => Some(lost_line(job)),
        Item::Ended(job) => Some(end_line(job.id, &job.target, &job.end, job.unread)),
        Item::Typed(id) => Some(typed_line(*id)),
        Item::Rewound(job) => Some(rewound_line(job)),
    }
}

pub fn state_sentence(id: u32, end: Option<&JobEnd>) -> String {
    match end {
        None => format!("Process {id} is running."),
        Some(end) => ended_sentence(id, end),
    }
}

pub fn state_words_for(end: Option<&JobEnd>, terminal: bool) -> String {
    match end {
        None if terminal => "running in a terminal".to_owned(),
        _ => state_words(end),
    }
}

pub fn state_words(end: Option<&JobEnd>) -> String {
    let Some(end) = end else {
        return "running".to_owned();
    };
    match end {
        JobEnd::Exited(ExitStatus::Code(code)) => format!("exited with code {code}"),
        JobEnd::Exited(ExitStatus::Signal(signal)) => format!("ended by signal {signal}"),
        JobEnd::Stopped(by) => match by {
            StoppedBy::Model => "stopped".to_owned(),
            StoppedBy::User => "stopped by the user".to_owned(),
            StoppedBy::SessionEnded => "stopped at the session's end".to_owned(),
            StoppedBy::TimeLimit(after_ms) => {
                format!("stopped at its {} limit", seconds(*after_ms))
            }
        },
        JobEnd::Lost(_) => "ended when Wisp restarted".to_owned(),
        JobEnd::Failed(_) => "failed".to_owned(),
    }
}

#[cfg(test)]
mod tests;
