use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use wisp_plugin_api::manifest::{is_id, is_name};
use wisp_protocol::log::LogEvent;
use wisp_protocol::plan_usage::{UsageScope, UsageWindow};
use wisp_protocol::{
    AgentEvent, CompactionFailure, ErrorKind, HookChange, HookOutcome, RejectKind, ToolOutcome,
    ToolView, TurnOutcome, WithdrawnBy,
};

use super::{RedactionKind, Redactions, scrub_json_strings};
use crate::plugin::hooks::BLOCKED_BY;
use crate::plugin::hooks::strategy::STRATEGY_REFUSED;
use crate::secrets::scan::{
    os_detection_home, scrub_checklist_spans, scrub_field, scrub_todo_fields,
};

struct Scrub<'a> {
    held: &'a Redactions,
    home: Option<&'a Path>,
    hook_audited: bool,
}

trait Content {
    fn redact(&mut self, redactions: &Scrub<'_>);
}

impl Content for String {
    fn redact(&mut self, redactions: &Scrub<'_>) {
        if !redactions.held.is_empty() {
            *self = redactions.held.scrub(self);
        }
    }
}

impl Content for Value {
    fn redact(&mut self, redactions: &Scrub<'_>) {
        if !redactions.held.is_empty() {
            redactions.held.scrub_value(self);
        }
    }
}

impl<T: Content> Content for Option<T> {
    fn redact(&mut self, redactions: &Scrub<'_>) {
        if let Some(value) = self {
            value.redact(redactions);
        }
    }
}

impl<T: Content> Content for Vec<T> {
    fn redact(&mut self, redactions: &Scrub<'_>) {
        for value in self {
            value.redact(redactions);
        }
    }
}

impl<T: Content> Content for Box<T> {
    fn redact(&mut self, redactions: &Scrub<'_>) {
        self.as_mut().redact(redactions);
    }
}

impl<T: Content> Content for BTreeMap<String, T> {
    fn redact(&mut self, redactions: &Scrub<'_>) {
        for value in self.values_mut() {
            value.redact(redactions);
        }
    }
}

macro_rules! content {
    ($ty:ty, $redactions:ident, {$($pattern:pat => [$($field:ident),*] $($extra:block)?),* $(,)?}) => {
        impl Content for $ty {
            fn redact(&mut self, $redactions: &Scrub<'_>) {
                match self {
                    $($pattern => {
                        $($field.redact($redactions);)*
                        $($extra)?
                    }),*
                }
            }
        }
    };
}

const COMPACTION_STOPPED: &str = "Compaction stopped: ";

const HOOK_COMPACTION_FAILURES: [CompactionFailure; 2] =
    [CompactionFailure::Blocked, CompactionFailure::CheckFailed];

pub(crate) fn hook_compaction_error_kind(failure: &CompactionFailure) -> Option<ErrorKind> {
    HOOK_COMPACTION_FAILURES
        .contains(failure)
        .then(|| failure.turn_kind().unwrap_or(ErrorKind::Other))
}

fn formatted_reason(message: &str) -> Option<(&str, &str)> {
    if let Some(reason) = message.strip_prefix(COMPACTION_STOPPED) {
        return Some((COMPACTION_STOPPED, reason));
    }
    let separator = if message.starts_with(BLOCKED_BY) {
        ": "
    } else if message.starts_with("The ") {
        STRATEGY_REFUSED
    } else {
        return None;
    };
    let (heading, reason) = message.split_once(separator)?;
    if let Some(hook) = heading.strip_prefix(BLOCKED_BY) {
        let (plugin, name) = hook.split_once('/')?;
        if !is_id(plugin) || !is_name(name) {
            return None;
        }
    } else if !heading.strip_prefix("The ").is_some_and(is_id) {
        return None;
    }
    Some((&message[..heading.len() + separator.len()], reason))
}

pub(crate) fn compaction_terminal_message(message: String) -> String {
    if formatted_reason(&message).is_some() {
        message
    } else {
        format!("{COMPACTION_STOPPED}{message}")
    }
}

pub(crate) fn turn_hook_audit(outcome: &mut TurnOutcome) -> Option<HookAudit<'_>> {
    let TurnOutcome::Error { kind, message, .. } = outcome else {
        return None;
    };
    (HOOK_COMPACTION_FAILURES
        .iter()
        .any(|failure| hook_compaction_error_kind(failure) == Some(*kind))
        && formatted_reason(message).is_some())
    .then_some(HookAudit::Formatted(message))
}

pub(crate) enum HookAudit<'a> {
    Input(&'a mut Value, &'a mut Value),
    Text(&'a mut String),
    Formatted(&'a mut String),
    BlockedTool(Option<&'a mut String>, Option<&'a mut String>),
}

macro_rules! hook_audit {
    ($event:expr, $event_type:ident) => {
        match $event {
            $event_type::HookApplied {
                change: HookChange::Input { before, after },
                ..
            } => Some(HookAudit::Input(before, after)),
            $event_type::HookApplied {
                change: HookChange::Blocked { reason },
                ..
            }
            | $event_type::HookRan {
                outcome: HookOutcome::Blocked { reason } | HookOutcome::Failed { message: reason },
                ..
            }
            | $event_type::InputWithdrawn {
                by: WithdrawnBy::Hook { reason, .. },
                ..
            } => Some(HookAudit::Text(reason)),
            $event_type::Rejected {
                kind: Some(RejectKind::InputBlocked),
                reason,
                ..
            } => Some(HookAudit::Formatted(reason)),
            $event_type::CompactionFailed { kind, message, .. }
                if hook_compaction_error_kind(kind).is_some() =>
            {
                Some(HookAudit::Formatted(message))
            }
            $event_type::TurnEnded { outcome, .. } => turn_hook_audit(outcome),
            $event_type::ToolCallFinished {
                outcome, result, ..
            } => {
                let (message, saved) = hook_blocked_tool_fields(outcome, result.as_deref());
                let (audit_message, audit_result) = (message.is_some(), saved.is_some());
                let message = match outcome {
                    ToolOutcome::Failed { message, .. } if audit_message => Some(message),
                    _ => None,
                };
                let result = result.as_mut().filter(|_| audit_result);
                (audit_message || audit_result).then_some(HookAudit::BlockedTool(message, result))
            }
            _ => None,
        }
    };
}

pub(crate) fn agent_hook_audit(event: &mut AgentEvent) -> Option<HookAudit<'_>> {
    hook_audit!(event, AgentEvent)
}

pub(crate) fn log_hook_audit(event: &mut LogEvent) -> Option<HookAudit<'_>> {
    hook_audit!(event, LogEvent)
}

impl HookAudit<'_> {
    pub(crate) fn scrub(self, home: Option<&Path>, held: &Redactions) -> bool {
        let scrub = |text: &str| scrub_field(text, home, held);
        match self {
            Self::Input(before, after) => {
                scrub_json_strings(before, &scrub);
                scrub_json_strings(after, &scrub);
            }
            Self::Text(text) => *text = scrub(text),
            Self::Formatted(text) => scrub_hook_failure(text, home, held),
            Self::BlockedTool(message, result) => {
                let audited_message = message.is_some();
                for text in message.into_iter().chain(result) {
                    scrub_hook_failure(text, home, held);
                }
                return audited_message;
            }
        }
        true
    }
}

pub(crate) fn hook_blocked_tool_fields<'a>(
    outcome: &'a ToolOutcome,
    result: Option<&'a str>,
) -> (Option<&'a str>, Option<&'a str>) {
    let ToolOutcome::Failed {
        message,
        before_start: true,
    } = outcome
    else {
        return (None, None);
    };
    (
        message.starts_with(BLOCKED_BY).then_some(message),
        result.filter(|text| text.starts_with(BLOCKED_BY)),
    )
}

pub(crate) fn scrub_hook_failure(text: &mut String, home: Option<&Path>, held: &Redactions) {
    scrub_formatted_reason(text, &|text| scrub_field(text, home, held));
}

fn scrub_formatted_reason(text: &mut String, scrub: &impl Fn(&str) -> String) {
    *text = if let Some((heading, reason)) = formatted_reason(text) {
        let formatted = format!("{heading}{}", scrub(reason));
        if scrub(&formatted) == formatted {
            formatted
        } else {
            super::REDACTED.to_owned()
        }
    } else {
        scrub(text)
    };
}

fn has_checklist_todos(event: &LogEvent) -> bool {
    wisp_protocol::todos::listed(event).is_some_and(|items| !items.is_empty())
}

fn has_checklist_spans(event: &LogEvent) -> bool {
    matches!(event, LogEvent::InputAccepted { checklist_spans, .. } if !checklist_spans.is_empty())
}

fn has_tool_call_display(event: &LogEvent) -> bool {
    matches!(
        event,
        LogEvent::ToolCallRequested {
            display,
            refused,
            ..
        } if crate::tool::tool_call_display_may_need_scrub_without_held(display, *refused)
    )
}

pub(crate) fn log_needs_home(event: &mut LogEvent) -> bool {
    has_checklist_todos(event)
        || has_checklist_spans(event)
        || has_tool_call_display(event)
        || log_hook_audit(event).is_some()
}

pub(super) fn scrub(redactions: &Redactions, mut event: LogEvent, home: Option<&Path>) -> LogEvent {
    let has_checklist = has_checklist_todos(&event) || has_checklist_spans(&event);
    let has_tool_display = has_tool_call_display(&event);
    let audit = log_hook_audit(&mut event);
    let fallback = (home.is_none() && (has_checklist || has_tool_display || audit.is_some()))
        .then(os_detection_home)
        .flatten();
    let home = home.or(fallback.as_deref());
    let hook_audited = audit.is_some_and(|audit| audit.scrub(home, redactions));
    if !redactions.is_empty() || has_checklist || has_tool_display {
        event.redact(&Scrub {
            held: redactions,
            home,
            hook_audited,
        });
    }
    event
}

pub(super) fn holds(value: &Value, code: &str) -> bool {
    match value {
        Value::String(s) => s.contains(code),
        Value::Array(items) => items.iter().any(|v| holds(v, code)),
        Value::Object(map) => map.values().any(|v| holds(v, code)),
        _ => false,
    }
}

fn call(
    tool: &str,
    input: &mut Value,
    display: &mut wisp_protocol::ToolDisplay,
    refused: bool,
    redactions: &Scrub<'_>,
) {
    crate::tool::scrub_tool_call_display(
        tool,
        input,
        display,
        redactions.home,
        redactions.held,
        refused,
    );
    if redactions.held.is_empty() {
        return;
    }
    let send = tool == wisp_protocol::agents::SEND_MESSAGE;
    let to = send.then(|| input.get_mut("to").map(Value::take)).flatten();
    let code = match &mut display.view {
        Some(ToolView::Code { language: _, code }) if !refused && !send && holds(input, code) => {
            Some(std::mem::take(code))
        }
        _ => None,
    };
    input.redact(redactions);
    display.redact(redactions);
    if let (Some(to), Some(slot)) = (to, input.get_mut("to")) {
        *slot = to;
    }
    if let (
        Some(code),
        Some(ToolView::Code {
            language: _,
            code: field,
        }),
    ) = (code, &mut display.view)
    {
        *field = code;
    }
}

content!(LogEvent, redactions, {
    Self::SessionStarted {
        session_id: _, wisp_version: _, model, effort: _, cache_ttl: _, system_prompt,
        system_prompt_sha256: _, system_base_sha256: _, endpoint, credential_source: _, endpoint_name,
        message_time: _, tools, tools_sha256: _, whole, cwd: _, output_dir, instructions: _,
        instructions_sha256: _, instructions_source: _, instructions_extra: _, preset: _,
        context_note_enabled: _, debug_cache: _, refusal_fallback: _, parent, headless: _, plugins,
        sections: _,
    } => [model, system_prompt, endpoint, endpoint_name, tools, whole, output_dir, parent, plugins],
    Self::TurnStarted {
        turn: _, origin: _, lead: _, ends: _, text: _, model: _, effort: _, effort_marker: _,
        tool_message: _, images: _, endpoint_name: _, input_id: _, inputs: _, cache_ttl: _, debug_cache: _,
        refusal_fallback: _, binding_controls: _, retry_of: _, kept_tools_sha256: _,
    } => [],
    Self::InputAccepted { turn: _, id: _, mode: _, text, images: _, replaces: _, held: _, from: _, checklist_spans } => [] {
        if !checklist_spans.is_empty() {
            scrub_checklist_spans(text, checklist_spans, redactions.home, redactions.held);
        }
    },
    Self::InputHeld { turn: _, ids: _, by } => [by],
    Self::InputUnheld { turn: _, ids: _ } => [],
    Self::ReturnedTaken { ids: _ } => [],
    Self::InputCommitted { turn: _, step: _, inputs: _ } => [],
    Self::InputRevised { turn: _, inputs: _ } => [],
    Self::InputWithdrawn { turn: _, ids: _, by: _ } => [],
    Self::InputDelivered { turn: _, step: _, ids: _ } => [],
    Self::Retrying {
        turn: _, attempt: _, delay_ms: _, reason, step: _, kind: _, detail, usage: _, partial_output,
        model, cost_usd: _, list_usd: _, lower_bound: _, resend: _, busy: _,
    } => [reason, detail, partial_output, model],
    Self::StepEnded {
        turn: _, step: _, usage: _, cost_usd: _, list_usd: _, model, lower_bound: _, prompt_tokens: _,
        response_id: _, window: _, compact_at: _, used: _, first_pass: _, accepted: _, view_len: _,
        started_at: _, ttl: _, prefix: _, betas, queued_ms: _, thread: _, thread_error,
    } => [model, betas, thread_error],
    Self::RebuildMarked { turn: _, reason } => [reason],
    Self::AttachmentsWithdrawn { turn: _, step: _, items } => [items],
    Self::Fallback { turn: _, from, to } => [from, to],
    Self::CacheMiss {
        turn: _, previous_prompt_tokens: _, cache_read_input_tokens: _, cache_creation_input_tokens: _,
        diagnostics, cause,
    } => [diagnostics, cause],
    Self::CacheMissRepeated { billing: _, turn: _, cause, count: _, usd: _ } => [cause],
    Self::CacheRefresh {
        turn: _, outcome: _, detail, started_at: _, count: _, usage: _, cost_usd: _, list_usd: _, model,
        lower_bound: _,
    } => [detail, model],
    Self::ModelAsked {
        call_id: _, plugin: _, turn: _, detail, usage: _, cost_usd: _, list_usd: _, plan_usd: _, model,
        lower_bound: _,
    } => [detail, model],
    Self::SessionNamed { name, detail, usage: _, cost_usd: _, list_usd: _, model, lower_bound: _ } => [name, detail, model],
    Self::VoiceCall {
        call: _, outcome, seconds: _, lines, unknown: _, endpoint_name, usage: _, cost_usd: _,
        list_usd: _, model, lower_bound: _,
    } => [outcome, lines, endpoint_name, model],
    Self::UnknownProviderEvent { turn: _, provider: _, event } => [event],
    Self::InputTransformations { turn: _, provider: _, transformations } => [transformations],
    Self::Rejected { turn: _, reason, id: _, kind: _ } => [] {
        if !redactions.hook_audited { reason.redact(redactions); }
    },
    Self::WakeHeld { turn: _, sources: _, reason: _ } => [],
    Self::AgentRefused { turn: _, by: _, action: _, reason } => [reason],
    Self::AgentSpawned {
        turn: _, agent: _, session_id: _, depth: _, model, effort: _, cwd: _, config, endpoint, provider,
    } => [model, config, endpoint, provider],
    Self::AgentEnded { turn: _, agent: _, spent_usd: _, plan_usd: _, billing: _ } => [],
    Self::AgentRenamed { turn: _, agent: _, to: _ } => [],
    Self::AgentReported { turn: _, agent: _, outcome: _, report, duration_ms: _ } => [report],
    Self::MessageCommitted { turn: _, message: _, model: _, stop_reason: _ } => [],
    Self::TurnEnded { turn: _, outcome, stats, partial_output } => [outcome, partial_output, stats],
    Self::ToolCallRequested { turn: _, step: _, block: _, call_id: _, tool, input, display, refused } => [] { call(tool, input, display, *refused, redactions); },
    Self::ToolCallStarted { turn: _, call_id: _, cwd, movable: _, file: _ } => [cwd],
    Self::ToolCallFinished {
        turn: _, call_id: _, outcome, output, facts: _, state: _, duration_ms: _, result: _, process: _,
        attachments, notes, typed: _,
    } => [output, notes, attachments] {
        if !redactions.hook_audited { outcome.redact(redactions); }
    },
    Self::QuestionAsked { turn: _, call_id: _, questions: _ } => [],
    Self::QuestionClosed { turn: _, call_id: _, how: _ } => [],
    Self::SessionEnded { turns: _, interrupted: _ } => [],
    Self::ImageLost { turn: _, sha256: _, note } => [note],
    Self::ResumeFellBack {
        kept_through: _, dropped_through: _, covered_through: _, last_turn: _, turns_seen: _,
        processes_seen: _, spent_usd: _, plan_usd: _, billing: _, damage: _,
    } => [],
    Self::SessionResumed {
        wisp_version: _, endpoint, endpoint_name, credential_source: _, system_prompt_sha256: _, tools,
        tools_sha256: _, system_prompt, system_base_sha256: _, whole, tools_on, instructions_sha256: _,
        instructions_source: _, instructions: _, instructions_extra: _, cwd: _, gap_ms: _,
        truncated_bytes: _, sections: _, returned: _,
    } => [endpoint, endpoint_name, tools, system_prompt, whole, tools_on],
    Self::SettingsChanged {
        model, effort: _, cache: _, endpoint_name, endpoint, credential_source: _, account_bound, tools,
        provider, tools_sha256: _, changes, withdrawn: _,
    } => [model, endpoint_name, provider, endpoint, account_bound, tools, changes],
    Self::PerMessageEffort { model, state: _, endpoint_name } => [model, endpoint_name],
    Self::BindingControls { model, state: _, endpoint_name } => [model, endpoint_name],
    Self::NativeWebSearch { endpoint_name, state: _ } => [endpoint_name],
    Self::NativeCompaction { model, state: _, endpoint_name } => [model, endpoint_name],
    Self::InlineTools { model, state: _, endpoint_name } => [model, endpoint_name],
    Self::DeferredTools { model, state: _, endpoint_name } => [model, endpoint_name],
    Self::PluginsChanged { turn: _, added, updated, removed, kept, notes } => [added, updated, removed, kept, notes],
    Self::ToolsChanged { turn: _, via: _, added, removed, copy } => [added, removed, copy],
    Self::ContextCompacted {
        plan_usd: _, turn: _, epoch: _, trigger: _, engine: _, focus, before: _, after: _, partial: _,
        covers: _, from: _, head: _, summary, block: _, usage: _, cost_usd: _, list_usd: _, model,
        lower_bound: _, duration_ms: _, switch: _, placement: _, cut: _, additions: _, sections: _,
        instructions_by, strategy, instruction, requested_by,
    } => [focus, summary, model, instructions_by, strategy, instruction, requested_by],
    Self::CompactionPrepared {
        plan_usd: _, turn: _, epoch: _, covers: _, block: _, usage: _, cost_usd: _, list_usd: _, model,
        lower_bound: _, duration_ms: _,
    } => [model],
    Self::CompactionFailed {
        plan_usd: _, turn: _, trigger: _, engine: _, kind, message, usage: _, cost_usd: _, list_usd: _,
        model, lower_bound: _, switch: _, strategy, instruction, requested_by,
    } => [model, strategy, instruction, requested_by, kind] {
        if !redactions.hook_audited { message.redact(redactions); }
    },
    Self::TailClosed { through: _, calls: _, reason: _ } => [],
    Self::RenderingForm { form: _ } => [],
    Self::ThinkingCut { from: _, through: _, tools_sha256: _ } => [],
    Self::SearchHistoryRendered { endpoint_name } => [endpoint_name],
    Self::SearchHistoryUnreadable { endpoint_name } => [endpoint_name],
    Self::SystemMarkerDropped { endpoint_name } => [endpoint_name],
    Self::AttachmentsAsNotes { endpoint_name, model } => [endpoint_name, model],
    Self::WebSearchNotRun { turn: _, endpoint_name, model } => [endpoint_name, model],
    Self::SearchStarted { turn: _, step: _, attempt: _, call_id: _, query, action: _ } => [query],
    Self::SearchEnded { turn: _, step: _, attempt: _, call_id: _, outcome } => [outcome],
    Self::Citations { turn: _, step: _, attempt: _, index: _, sources } => [sources],
    Self::InstructionsLoaded { turn: _, chosen: _, imports: _, candidates: _, diagnostics } => [] {
        for wisp_protocol::Diagnostic { source: _, kind: _, message } in diagnostics {
            message.redact(redactions);
        }
    },
    Self::InstructionsChanged {
        turn: _, epoch: _, retry_of: _, ahead: _, from, to, changed, chosen, imports, candidates, sent: _,
        diagnostics, text,
    } => [from, to, changed, chosen, imports, candidates, diagnostics, text],
    Self::ProcessStarted {
        turn: _, id: _, call_id: _, reason: _, tty: _, stdin: _, size: _, file: _, pid: _, sid: _,
        start_ticks: _, boot_id: _, keeps_running: _, log,
    } => [log],
    Self::ProcessEnded { id: _, end, bytes: _, lines: _, duration_ms: _ } => [end],
    Self::ProcessResized { id: _, size: _ } => [],
    Self::ProcessAttached { id: _, size: _ } => [],
    Self::ProcessDetached { id: _, size: _, reason: _, typed: _, answered: _ } => [],
    Self::ProcessHeard { turn: _, ids: _, typed: _ } => [],
    Self::ProcessNeedsInput { id: _, kind: _ } => [],
    Self::ProcessNeedsInputCleared { id: _ } => [],
    Self::EndpointSaved { turn: _, name, store: _, check } => [name, check],
    Self::EndpointNotSaved { turn: _, name, why } => [name, why],
    Self::EndpointEdited { turn: _, name, changed: _, store: _, check } => [name, check],
    Self::EndpointRemoved { turn: _, name, kept_item: _, default_cleared: _ } => [name],
    Self::EndpointsChanged { turn: _, endpoints } => [endpoints],
    Self::WindowLowered { endpoint, model, tokens: _ } => [endpoint, model],
    Self::TodosUpdated { turn: _, call_id: _, items } => [items],
    Self::Rewound { to: _, point: _, reason: _, messages: _, blocks: _, dropped: _, cache: _ } => [],
    Self::HookRan { hook: _, point: _, turn: _, step: _, call_id: _, outcome, ms: _ } => [outcome],
    Self::HookApplied { hook: _, point: _, turn: _, step: _, call_id: _, input_id: _, change } => [change],
    Self::Unknown => [],
});

content!(wisp_protocol::log::ToolsCopy, redactions, {
    Self { tool_definitions, deferred_definitions, tool_context, history_len: _ } => [tool_definitions, deferred_definitions, tool_context],
});

content!(wisp_protocol::log::PrefixCopy, redactions, {
    Self {
        system_prompt, system_prompt_sha256: _, system_base_sha256: _, tools, tools_sha256: _, whole,
        tools_on,
    } => [system_prompt, tools, whole, tools_on],
});

content!(wisp_protocol::log::ToolContext, redactions, {
    Self { platform, capabilities } => [platform, capabilities],
});

content!(wisp_protocol::agents::ParentLink, redactions, {
    Self { session_id: _, agent: _, depth: _, config, todos } => [config, todos],
});

content!(wisp_protocol::SpawnChoices, redactions, {
    Self { preset, tools_off, spawn: _, instructions: _, preset_values, keep_warm: _, choices } => [preset, tools_off, preset_values, choices],
});

content!(wisp_protocol::agents::Choice, redactions, {
    Self { endpoint, model, effort: _ } => [endpoint, model],
});

content!(wisp_protocol::agents::Preset, redactions, {
    Self {
        name, description, model, effort: _, tools_off, preload, spawn: _, instructions: _, endpoint,
        fallback, worker: _,
    } => [name, description, model, tools_off, preload, endpoint, fallback],
});

content!(wisp_protocol::ToolDisplay, redactions, {
    Self { action, verbs, target, output, view, title, purpose: _ } => [action, verbs, target, output, view, title],
});

content!(wisp_protocol::Verbs, redactions, {
    Self { doing, done, failed } => [doing, done, failed],
});

content!(wisp_protocol::DiffView, redactions, {
    Self { path, old_path, language, hunks, added: _, removed: _ } => [path, old_path, language, hunks],
});

content!(wisp_protocol::ToolOutputView, redactions, {
    Self { tail, bytes: _, lines: _, file: _, file_problem, inlined: _ } => [tail, file_problem],
});

content!(wisp_protocol::ErrorDetail, redactions, {
    Self {
        limit_reached, model_limit, stream, malformed_call, phase: _, source: _, status: _, error_type, request_id, model,
        retry: _, possibly_billed: _,
    } => [stream, malformed_call, error_type, request_id, model] {
        omit_redacted_window(limit_reached, redactions);
        omit_redacted_window(model_limit, redactions);
    },
});

content!(wisp_protocol::MalformedCall, redactions, {
    Self {
        form: _, function, undeclared: _, chars: _, balanced: _, stop_string: _, mail_tag: _,
    } => [function],
});

content!(wisp_protocol::StreamDiagnostics, redactions, {
    Self {
        bytes: _, events: _, text_parts: _, thinking_parts: _, tool_use_parts: _, last_event_keys,
        partial_event: _, stop_reason_seen: _, first_byte_ms: _, last_byte_ms: _,
    } => [last_event_keys],
});

fn omit_redacted_window(window: &mut Option<Box<UsageWindow>>, redactions: &Scrub<'_>) {
    let affected = |text: &str| redactions.held.has_match(text, RedactionKind::LogOnly);
    if window.as_deref().is_some_and(|w| {
        affected(&w.key)
            || affected(&w.label)
            || match &w.scope {
                UsageScope::All => false,
                UsageScope::Model(name) | UsageScope::ModelFamily(name) => affected(name),
            }
    }) {
        *window = None;
    }
}

content!(wisp_protocol::RetryOffer, redactions, {
    Self { retry: _, switch: _, focus: _, suggest, retries_at: _ } => [suggest],
});

content!(wisp_protocol::TurnStats, _redactions, {
    Self { started_at: _, first_byte_ms: _, duration_ms: _, retries: _, previous_prompt_tokens: _ } => [],
});

content!(wisp_protocol::InputTransformation, redactions, {
    Self { kind: _, path, reason, extra } => [path, reason, extra],
});

content!(wisp_protocol::log::WithdrawnBlock, redactions, {
    Self { ordinal: _, sha256: _, note } => [note],
});

content!(wisp_protocol::AttachmentRef, redactions, {
    Self { media_type: _, sha256: _, bytes: _, saved, slice: _ } => [saved],
});

content!(wisp_protocol::PluginRecord, _redactions, {
    Self { id: _, version: _, source: _ } => [],
});

content!(wisp_protocol::plugin::PluginNote, redactions, {
    Self { plugin: _, message } => [message],
});

content!(wisp_protocol::settings::LoggedChange, redactions, {
    Self { target: _, value, layer: _, when: _, saved, unset: _, by: _ } => [value, saved],
});

content!(wisp_protocol::SourceInfo, redactions, {
    Self { scope: _, name, display_path, bytes: _, sha256: _, imported_by, external: _ } => [name, display_path, imported_by],
});

content!(wisp_protocol::SourceChange, redactions, {
    Self { display_path, old_sha256: _, new_sha256: _ } => [display_path],
});

content!(wisp_protocol::CandidateInfo, redactions, {
    Self { scope: _, display_path, bytes: _, mtime: _, sha256: _ } => [display_path],
});

content!(wisp_protocol::Diagnostic, redactions, {
    Self { source, kind: _, message } => [source, message],
});

content!(wisp_protocol::SearchHit, redactions, {
    Self { title, url, snippet, page_age } => [title, url, snippet, page_age],
});

content!(wisp_protocol::CitedSource, redactions, {
    Self { url, title } => [url, title],
});

content!(wisp_protocol::endpoint::CheckResult, redactions, {
    Self { outcome: _, reason } => [reason],
});

content!(wisp_protocol::settings::EndpointSummary, redactions, {
    Self {
        name, provider, credential: _, official: _, host, unavailable, priced: _, default: _,
        keys_file: _, expired: _, key_var, mark: _, models, complete: _, default_model, voice_model,
    } => [name, provider, host, unavailable, key_var, models, default_model, voice_model],
});

content!(wisp_protocol::settings::AccountModel, redactions, {
    Self { id, listed: _ } => [id],
});

content!(wisp_protocol::settings::KeyVariable, redactions, {
    Self { name, in_keys_file: _ } => [name],
});

content!(wisp_protocol::TodoItem, redactions, {
    Self { text, status: _, section } => [] {
        scrub_todo_fields(text, section, redactions.home, redactions.held);
    },
});

content!(wisp_protocol::ToolAction, redactions, {
    Self::Run | Self::Read | Self::Write | Self::Edit | Self::Search | Self::Fetch |
    Self::Input | Self::Wait | Self::Attach | Self::Show | Self::Unknown => [],
    Self::Other { name } => [name],
    Self::Signal { kind: _ } => [],
});
content!(wisp_protocol::OutputKind, redactions, {
    Self::Text | Self::Diff | Self::None | Self::Unknown => [],
    Self::Code { lang } => [lang],
});
content!(ToolView, redactions, {
    Self::Diff { title, files } => [title, files],
    Self::Process { id: _, text, keys, signal: _, read, pattern } => [text, keys, read, pattern],
    Self::Code { language, code } => [language, code],
    Self::Unknown => [],
});
content!(wisp_protocol::ToolOutcome, redactions, {
    Self::Completed { is_error: _, exit: _ } | Self::TimedOut { after_ms: _ } |
    Self::Cancelled | Self::NotRun { reason: _ } | Self::Backgrounded { reason: _ } => [],
    Self::InvalidInput { message } | Self::Failed { message, before_start: _ } => [message],
});
content!(wisp_protocol::TurnOutcome, redactions, {
    Self::Completed { stop_reason: _, model, usage: _, cost: _, committed: _, steps: _, tool_calls: _ } => [model],
    Self::Cancelled { usage: _, cost: _, committed_steps: _ } => [],
    Self::Error {
        kind: _, message, usage: _, cost: _, committed_steps: _, detail, attempts: _, gave_up: _, offer,
    } => [detail, offer] {
        if !redactions.hook_audited { message.redact(redactions); }
    },
});
content!(wisp_protocol::HookOutcome, redactions, {
    Self::None | Self::Applied | Self::Continued | Self::TimedOut | Self::Cancelled | Self::Unknown => [],
    Self::Blocked { reason: _ } | Self::Failed { message: _ } => [],
    Self::Rejected { rule } => [rule],
});
content!(wisp_protocol::HookChange, redactions, {
    Self::Input { before: _, after: _ } => [],
    Self::Result { before, after } | Self::Text { before, after } => [before, after],
    Self::Continue { text } => [text],
    Self::Blocked { reason: _ } => [],
    Self::Unknown => [],
});
content!(wisp_protocol::SearchEnd, redactions, {
    Self::Done(outcome) => [outcome],
    Self::Unfinished { reason: _, possibly_billed: _ } => [],
});
content!(wisp_protocol::SearchOutcome, redactions, {
    Self::Results { hits } => [hits],
    Self::Failed { failure: _, code } => [code],
});
content!(wisp_protocol::endpoint::NotSavedReason, redactions, {
    Self::Check(check) => [check],
    Self::KeychainFailed(message) => [message],
    Self::Other(value) => [value],
});
content!(wisp_protocol::settings::SettingValue, redactions, {
    Self::Bool(_) | Self::Int(_) | Self::Tokens(_) | Self::Percent(_) | Self::Usd { micros: _ } => [],
    Self::Text(text) => [text],
    Self::List(items) => [items],
    Self::Other { kind: _, text } => [text],
});
content!(wisp_protocol::MissCause, redactions, {
    Self::EndpointChanged | Self::ModelChanged | Self::EffortChanged | Self::ThinkingChanged |
    Self::ToolsChanged | Self::SystemChanged | Self::BetasChanged | Self::MessageChanged { index: _ } |
    Self::TtlExpired { gap_s: _ } | Self::RemoteLoss | Self::Unexplained => [],
    Self::InjectedThinking { first_path, count: _ } |
    Self::ProviderDroppedThinking { first_path, count: _ } => [first_path],
    Self::Expected { reason } => [reason],
    Self::Other(value) => [value],
});
content!(wisp_protocol::CompactionFailure, redactions, {
    Self::Cancelled | Self::CheckFailed | Self::Blocked | Self::Error(_) | Self::Other(_) => [],
    Self::NoSummary { stop_reason: _ } => [],
    Self::Discarded { reason } => [reason],
});
content!(wisp_protocol::ProcessEnd, redactions, {
    Self::Exited { exit: _ } | Self::Stopped { by: _ } | Self::Lost { reason: _ } => [],
    Self::Failed { message } => [message],
});
content!(wisp_protocol::WithdrawnBy, redactions, {
    Self::User | Self::TurnStopped | Self::TurnFailed | Self::SessionEnded | Self::Replaced |
    Self::NotKept | Self::Rewound => [],
    Self::Hook { hook: _, reason } => [reason],
});

content!(wisp_protocol::settings::Saved, redactions, {
    Self { path_display, line: _, notes } => [path_display, notes],
});

content!(wisp_protocol::voice::CallOutcome, redactions, {
    Self::Completed { end: _ } => [],
    Self::Failed { kind: _, message } => [message],
});
