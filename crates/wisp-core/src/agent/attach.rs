use std::borrow::Cow;

use serde_json::Value;
use tokio::sync::mpsc;
use wisp_media::{MAX_ATTACHMENT_BYTES, MAX_SESSION_MEDIA_BASE64};
use wisp_plugin_api::types::SliceKind;
use wisp_protocol::{
    AgentEvent, AttachmentRef, ContentBlock, Fact, Role, ToolDisplay, ToolOutcome,
};

use super::images::media_budget_sentence;
use super::{Agent, Finished, emit, tool_call};
use crate::plugin::convert;
use crate::session::sha256_bytes_hex;
use crate::tool::{Attachment, SHELL, fallback_display};
use wisp_tool_text::files as sentences;

pub(super) struct Attached {
    pub(super) blocks: Vec<ContentBlock>,
    pub(super) refs: Vec<AttachmentRef>,
}

fn keep_process_facts(finished: &mut Finished) {
    finished
        .facts
        .retain(|fact| matches!(fact, Fact::Exit { .. } | Fact::Process { .. }));
}

fn replace_result(finished: &mut Finished, sentence: String) {
    finished.ends.clear();
    finished.text.clone_from(&sentence);
    finished.result = Some(sentence);
    keep_process_facts(finished);
}

pub(super) fn stopped(finished: &mut Finished, timed_out_after: Option<u64>) {
    let files = std::mem::take(&mut finished.attachments).len();
    if files == 0 {
        return;
    }
    if !matches!(
        finished.outcome,
        ToolOutcome::Cancelled | ToolOutcome::TimedOut { .. }
    ) {
        finished.outcome = match timed_out_after {
            Some(after_ms) => ToolOutcome::TimedOut { after_ms },
            None => ToolOutcome::Cancelled,
        };
    }
    let sentence = if files == 1 {
        "Stopped before the file was attached."
    } else {
        "Stopped before the files were attached."
    };
    replace_result(finished, sentence.to_owned());
}

fn refuse(finished: &mut Finished, sentence: String) {
    let exit = match finished.outcome {
        ToolOutcome::Completed { exit, .. } => exit,
        _ => None,
    };
    finished.outcome = ToolOutcome::Completed {
        is_error: true,
        exit,
    };
    replace_result(finished, sentence);
}

fn refusal(
    target: &str,
    attachments: &[Attachment],
    carried: &[&str],
    slices: &[SliceKind],
    shell: Option<&str>,
) -> Option<String> {
    if let Some(file) = attachments
        .iter()
        .find(|a| !sentences::carries(carried, a.media_type.as_str()))
    {
        return Some(sentences::uncarried(
            target,
            file.media_type.as_str(),
            carried,
            shell,
        ));
    }
    if let Some(message) = attachments.iter().find_map(|file| {
        let slice = file.slice?;
        sentences::slice_refusal(
            target,
            file.media_type.as_str(),
            convert::slice_kind(&slice),
            slices,
            shell,
        )
    }) {
        return Some(message);
    }
    attachments
        .iter()
        .find(|a| a.bytes.len() > MAX_ATTACHMENT_BYTES)
        .map(|file| {
            sentences::too_large(
                target,
                file.bytes.len() as u64,
                MAX_ATTACHMENT_BYTES as u64,
                "use that instead",
                shell,
            )
        })
}

impl Agent {
    pub(super) fn target_of(&self, call_id: &str, name: &str, input: &Value) -> String {
        self.targets.get(call_id).cloned().unwrap_or_else(|| {
            if self.hidden_mcp_name(name) {
                fallback_display(crate::mcp::PLUGIN, &Value::Null).target
            } else {
                self.safe_fallback_display(name, input).target
            }
        })
    }

    pub(super) fn calls(&self) -> impl Iterator<Item = (&str, &str, Cow<'_, Value>)> {
        self.history
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .flat_map(|m| m.content.iter())
            .filter_map(tool_call)
    }

    pub(super) async fn call_display(&mut self, call_id: &str) -> Option<ToolDisplay> {
        let (_, name, input) = self.calls().find(|(id, _, _)| *id == call_id)?;
        if self.hidden_mcp_name(name) {
            return Some(fallback_display(crate::mcp::PLUGIN, &Value::Null));
        }
        let prepared = match self.toolbox.get(name) {
            Some(tool) => tool.prepare(&input).await.ok(),
            None => None,
        };
        Some(match prepared {
            Some(prepared) => prepared.display().clone(),
            None => self.rejected_display_for(name, &input).await,
        })
    }

    pub(super) fn call_target(&self, call_id: &str) -> Option<String> {
        self.calls()
            .find(|(id, _, _)| *id == call_id)
            .map(|(id, name, input)| self.target_of(id, name, &input))
    }

    pub(super) async fn attach(
        &mut self,
        finished: &mut Finished,
        (call_id, name, input): (&str, &str, &Value),
        earlier: &[ContentBlock],
        events: &mpsc::Sender<AgentEvent>,
    ) -> Attached {
        let attachments = std::mem::take(&mut finished.attachments);
        let mut attached = Attached {
            blocks: Vec::new(),
            refs: Vec::new(),
        };
        if attachments.is_empty() {
            return attached;
        }
        let carried = self.carried_media();
        let shell = self.toolbox.provider_of(SHELL);
        if let Some(sentence) = refusal(
            &self.target_of(call_id, name, input),
            &attachments,
            carried,
            self.carried_slices(),
            shell,
        ) {
            refuse(finished, sentence);
            return attached;
        }
        let encoded: Vec<String> = attachments
            .iter()
            .map(|a| wisp_media::encode_base64(&a.bytes))
            .collect();
        let added = encoded
            .iter()
            .map(String::len)
            .fold(0, usize::saturating_add);
        let (_, used) = self.media_in_use(None, earlier);
        if used.saturating_add(added) > MAX_SESSION_MEDIA_BASE64 {
            refuse(finished, media_budget_sentence(attachments.len()));
            return attached;
        }
        for (attachment, base64) in attachments.into_iter().zip(encoded) {
            let sha256 = sha256_bytes_hex(&attachment.bytes);
            let (failure, saved) = match &mut self.log {
                Some((log, _)) => {
                    let mut log = log.lock();
                    let failure = log.put_blob(&sha256, &attachment.media_type, &attachment.bytes);
                    let saved = log.saved_path(&sha256, &attachment.media_type);
                    (failure, saved)
                }
                None => (None, None),
            };
            if let Some(reason) = failure {
                emit(events, AgentEvent::LogDisabled { reason }).await;
            }
            attached.refs.push(AttachmentRef {
                media_type: attachment.media_type.clone(),
                sha256,
                bytes: u64::try_from(attachment.bytes.len()).unwrap_or(u64::MAX),
                saved,
                slice: attachment.slice,
            });
            attached.blocks.push(ContentBlock::media_sliced(
                attachment.media_type,
                base64,
                attachment.slice,
            ));
        }
        attached
    }
}
