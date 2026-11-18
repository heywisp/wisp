use std::sync::Arc;

use wisp_plugin_api::host::{Host, JobInfo};
use wisp_plugin_api::strategy::{CallEntry, SaidEntry, StrategyInfo, View};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::compaction::{BYTES_PER_TOKEN, instruction, text_tokens};
use wisp_tool_text::jobs::LIST_TARGET_CHARS;
use wisp_tool_text::output::{count, cut_chars};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const SUMMARY_INSTRUCTION: &str = "Write a summary of the conversation so far, so that \
the work can continue from the summary alone. Use these six sections, in this order, each \
under its heading word for word:\n\
1. Goal and standing instructions: what the user asked for and the constraints they set, in \
their terms.\n\
2. Decisions: what was decided, and why.\n\
3. State of the work: Done, In progress, Blocked.\n\
4. Files, branches and artefacts: those that matter, with paths.\n\
5. Commands, errors and fixes: those whose results matter, with outcomes.\n\
6. Open questions: what is still open, and the exact next step if a task is under way.\n\
Don't call tools. Carry forward anything from an earlier summary in the conversation that \
still holds. Keep the summary under about 6,000 tokens. If a task is under way, keep the \
content of the latest tool results that the next step needs.";

pub const SECTIONS: [(&str, &str); 6] = [
    ("Goal and standing instructions", "goal"),
    ("Decisions", "decisions"),
    ("State of the work", "state of the work"),
    ("Files, branches and artefacts", "files"),
    ("Commands, errors and fixes", "commands"),
    ("Open questions", "open questions"),
];

pub const ATTACHED_LEFT: &str = "The files these calls attached are no longer in the \
conversation; to see one again, repeat its call.";

pub const CUT_OFF_INTRO: &str = "The end of the last reply, which the context window cut off:";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Compaction { host })
}

pub fn summary_instruction(focus: Option<&str>) -> String {
    instruction(SUMMARY_INSTRUCTION, focus)
}

fn normalised(text: &str) -> String {
    let words: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    words.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn missing_section(summary: &str) -> Option<&'static str> {
    let words = format!(" {}", normalised(summary));
    SECTIONS
        .into_iter()
        .find(|(_, key)| !words.contains(&format!(" {key}")))
        .map(|(heading, _)| heading)
}

pub fn refusal(section: &str) -> String {
    format!(
        "The summary has no \u{201c}{section}\u{201d} section, so Wisp kept the conversation as it was."
    )
}

fn cut_at(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub fn verbatim(said: &[SaidEntry], budget: u64) -> Vec<String> {
    let mut left = budget;
    let mut lines = Vec::new();
    for said in said.iter().rev() {
        if left == 0 {
            break;
        }
        let tokens = text_tokens(&said.text);
        if tokens <= left {
            left -= tokens;
            lines.push(format!("Turn {}: {}", said.turn, said.text));
            continue;
        }
        let bytes = usize::try_from(left.saturating_mul(BYTES_PER_TOKEN)).unwrap_or(usize::MAX);
        let kept = cut_at(&said.text, bytes);
        let pointer = match said.seq {
            Some(seq) => {
                format!(" [cut here; the whole message is record {seq} in the session log]")
            }
            None => " [cut here]".to_owned(),
        };
        lines.push(format!("Turn {}: {kept}{pointer}", said.turn));
        left = 0;
    }
    lines
}

pub fn ledger(calls: &[CallEntry]) -> Vec<String> {
    calls
        .iter()
        .map(|call| {
            let mut parts = vec![format!("turn {}: {} {}", call.turn, call.tool, call.target)];
            parts.push(
                call.status
                    .clone()
                    .unwrap_or_else(|| "no result".to_owned()),
            );
            if let Some(cwd) = call.cwd.as_ref().filter(|cwd| !cwd.is_empty()) {
                parts.push(format!("in {cwd}"));
            }
            if let Some(saved) = &call.saved {
                parts.push(format!("output saved to {saved}"));
            }
            if !call.attached.is_empty() {
                let withdrawn = if call.withdrawn { " (withdrawn)" } else { "" };
                parts.push(format!("attached {}{withdrawn}", call.attached.join(", ")));
            }
            format!("- {}", parts.join(" \u{b7} "))
        })
        .collect()
}

fn ledger_attached(calls: &[CallEntry]) -> bool {
    calls
        .iter()
        .any(|call| !call.attached.is_empty() && !call.withdrawn)
}

pub fn pointers(log: Option<String>, outputs: Option<String>) -> Option<String> {
    match (log, outputs) {
        (None, None) => None,
        (Some(log), None) => Some(format!(
            "The full earlier conversation can be read in the session log, {log}."
        )),
        (None, Some(outputs)) => Some(format!(
            "Every tool output is saved under {outputs}, where it can be read."
        )),
        (Some(log), Some(outputs)) => Some(format!(
            "The full earlier conversation can be read in the session log, {log}, and every tool output under {outputs}."
        )),
    }
}

pub fn running_line(jobs: &[JobInfo]) -> Option<String> {
    let parts: Vec<String> = jobs
        .iter()
        .filter(|job| !job.restored && job.end.is_none())
        .filter_map(|job| {
            let id = job.id?;
            let target = cut_chars(&job.label, LIST_TARGET_CHARS);
            Some(if job.unread > 0 {
                format!("{id} ({target}, {})", count(job.unread, "unread line"))
            } else {
                format!("{id} ({target})")
            })
        })
        .collect();
    (!parts.is_empty()).then(|| format!("Running processes: {}.", parts.join(", ")))
}

pub struct Layout<'a> {
    pub info: &'a StrategyInfo,
    pub summary: Option<&'a str>,
    pub said: &'a [SaidEntry],
    pub calls: &'a [CallEntry],
    pub running: Option<String>,
    pub pointers: Option<String>,
}

pub fn compacted_context(layout: &Layout<'_>) -> String {
    let info = layout.info;
    let mut text = format!(
        "<compacted_context epoch=\"{}\" turns=\"{}\u{2013}{}\">\n",
        info.epoch, info.first_turn, info.last_turn
    );
    if let Some(summary) = layout.summary {
        text.push_str("<summary>\n");
        text.push_str(summary.trim_end());
        text.push_str("\n</summary>\n");
    }
    let said = verbatim(layout.said, info.verbatim_tokens);
    if !said.is_empty() {
        text.push_str(
            "<user_messages>\nThe user's recent messages, word for word, newest first:\n",
        );
        for line in said {
            text.push_str(&line);
            text.push('\n');
        }
        text.push_str("</user_messages>\n");
    }
    let calls = ledger(layout.calls);
    if !calls.is_empty() {
        text.push_str("<tool_calls>\nThe last tool calls, oldest first:\n");
        for line in calls {
            text.push_str(&line);
            text.push('\n');
        }
        if ledger_attached(layout.calls) {
            text.push_str(ATTACHED_LEFT);
            text.push('\n');
        }
        text.push_str("</tool_calls>\n");
    }
    if let Some(tail) = &info.cut_off {
        text.push_str("<cut_off_reply>\n");
        text.push_str(CUT_OFF_INTRO);
        text.push('\n');
        text.push_str(tail.trim_end());
        text.push_str("\n</cut_off_reply>\n");
    }
    if let Some(running) = &layout.running {
        text.push_str(running);
        text.push('\n');
    }
    if let Some(pointers) = &layout.pointers {
        text.push_str("<pointers>\n");
        text.push_str(pointers);
        text.push_str("\n</pointers>\n");
    }
    text.push_str("</compacted_context>\n");
    text
}

pub struct Compaction {
    host: Arc<dyn Host>,
}

impl Plugin for Compaction {
    fn instruction<'a>(&'a self, info: StrategyInfo) -> BoxFuture<'a, Option<String>> {
        let text = summary_instruction(info.compact.focus.as_deref());
        Box::pin(async move { Some(text) })
    }

    fn accept<'a>(
        &'a self,
        _info: StrategyInfo,
        summary: String,
    ) -> BoxFuture<'a, Result<(), String>> {
        let verdict = missing_section(&summary).map_or(Ok(()), |section| Err(refusal(section)));
        Box::pin(async move { verdict })
    }

    fn head<'a>(
        &'a self,
        info: StrategyInfo,
        view: &'a dyn View,
        summary: Option<String>,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move {
            let jobs = self.host.all().await;
            let said = view.said();
            let calls = view.calls();
            let layout = Layout {
                info: &info,
                summary: summary.as_deref(),
                said: &said,
                calls: &calls,
                running: running_line(&jobs),
                pointers: pointers(view.log_pointer(), view.output_pointer()),
            };
            vec![compacted_context(&layout)]
        })
    }
}

#[cfg(test)]
mod tests;
