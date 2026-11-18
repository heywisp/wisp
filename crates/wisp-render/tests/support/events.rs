#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "render's and tui's libraries and test binaries each use part of the shared builders, and a zero count fails the test"
)]

use std::num::NonZeroU16;

use serde_json::{Value, json};
use wisp_protocol::settings::{AppliedChange, Restored, Withdrawal};
use wisp_protocol::{
    AgentEvent, AttachmentRef, CacheEffect, Cost, Effort, ErrorDetail, ErrorKind, ExitStatus, Fact,
    GaveUp, OutputKind, RequestId, RetryOffer, StopReason, ToolAction, ToolDisplay, ToolOutcome,
    ToolOutputView, ToolPurpose, ToolView, TurnOutcome, TurnStats, Typed, Usage, Verbs,
    WorkspaceChange,
};

pub(crate) fn cost(turn_usd: f64, session_usd: f64, lower: bool) -> Cost {
    Cost {
        turn_plan_usd: None,
        session_plan_usd: None,
        agents_plan_usd: None,
        agents_billing: Default::default(),
        turn_billing: Default::default(),
        session_billing: Default::default(),
        turn_usd,
        session_usd,
        turn_lower_bound: lower,
        session_lower_bound: lower,
        agents_usd: None,
        agents_lower_bound: false,
    }
}

pub(crate) fn usage() -> Usage {
    Usage {
        input_tokens: 40,
        output_tokens: 475,
        cache_read_input_tokens: 9_200,
        cache_creation_input_tokens: 760,
        cache_creation_5m_input_tokens: Some(0),
        cache_creation_1h_input_tokens: Some(760),
        web_search_requests: 0,
        ..Usage::default()
    }
}

#[derive(Default)]
pub(crate) enum Ending {
    #[default]
    Completed,
    Cancelled,
    Error(ErrorKind),
}

pub(crate) struct End {
    pub(crate) ending: Ending,
    pub(crate) stop_reason: StopReason,
    pub(crate) model: String,
    pub(crate) message: String,
    pub(crate) usage: Option<Usage>,
    pub(crate) cost: Option<Cost>,
    pub(crate) committed: bool,
    pub(crate) steps: u32,
    pub(crate) tool_calls: u32,
    pub(crate) committed_steps: u32,
    pub(crate) detail: Option<ErrorDetail>,
    pub(crate) attempts: u32,
    pub(crate) gave_up: Option<GaveUp>,
    pub(crate) offer: Option<RetryOffer>,
    pub(crate) stats: TurnStats,
}

impl Default for End {
    fn default() -> Self {
        Self {
            ending: Ending::Completed,
            stop_reason: StopReason::EndTurn,
            model: String::new(),
            message: String::new(),
            usage: None,
            cost: None,
            committed: true,
            steps: 1,
            tool_calls: 0,
            committed_steps: 0,
            detail: None,
            attempts: 0,
            gave_up: None,
            offer: None,
            stats: TurnStats::default(),
        }
    }
}

pub(crate) fn outcome(end: End) -> TurnOutcome {
    match end.ending {
        Ending::Completed => TurnOutcome::Completed {
            stop_reason: end.stop_reason,
            model: end.model,
            usage: end.usage.unwrap_or_default(),
            cost: end.cost,
            committed: end.committed,
            steps: end.steps,
            tool_calls: end.tool_calls,
        },
        Ending::Cancelled => TurnOutcome::Cancelled {
            usage: end.usage,
            cost: end.cost,
            committed_steps: end.committed_steps,
        },
        Ending::Error(kind) => TurnOutcome::Error {
            kind,
            message: end.message,
            usage: end.usage,
            cost: end.cost,
            committed_steps: end.committed_steps,
            detail: end.detail,
            attempts: end.attempts,
            gave_up: end.gave_up,
            offer: end.offer,
        },
    }
}

pub(crate) fn turn_ended(end: End) -> AgentEvent {
    AgentEvent::TurnEnded {
        stats: end.stats.clone(),
        outcome: outcome(end),
    }
}

pub(crate) fn ended(outcome: TurnOutcome, ms: u64) -> AgentEvent {
    AgentEvent::TurnEnded {
        outcome,
        stats: TurnStats {
            duration_ms: ms,
            ..TurnStats::default()
        },
    }
}

pub(crate) fn cancelled() -> TurnOutcome {
    outcome(End {
        ending: Ending::Cancelled,
        ..End::default()
    })
}

pub(crate) fn step_ended(step: u32, usd: f64) -> AgentEvent {
    AgentEvent::StepEnded {
        step,
        usage: Some(Usage::default()),
        cost_usd: Some(usd),
        lower_bound: false,
    }
}

pub(crate) struct Changed {
    pub(crate) answers: Option<RequestId>,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) effort: Effort,
    pub(crate) cache: CacheEffect,
    pub(crate) account_bound: Vec<String>,
    pub(crate) changes: Vec<AppliedChange>,
    pub(crate) withdrawn: Option<Withdrawal>,
    pub(crate) restored: Option<Restored>,
}

impl Default for Changed {
    fn default() -> Self {
        Self {
            answers: None,
            endpoint: String::new(),
            model: String::new(),
            effort: Effort::Low,
            cache: CacheEffect::Kept,
            account_bound: Vec::new(),
            changes: Vec::new(),
            withdrawn: None,
            restored: None,
        }
    }
}

pub(crate) fn settings_changed(changed: Changed) -> AgentEvent {
    AgentEvent::SettingsChanged {
        answers: changed.answers,
        endpoint: changed.endpoint,
        model: changed.model,
        effort: changed.effort,
        cache: changed.cache,
        account_bound: changed.account_bound,
        changes: changed.changes,
        withdrawn: changed.withdrawn,
        restored: changed.restored,
    }
}

pub(crate) struct Requested {
    pub(crate) step: u32,
    pub(crate) block: usize,
    pub(crate) call_id: String,
    pub(crate) tool: String,
    pub(crate) input: Value,
    pub(crate) action: ToolAction,
    pub(crate) verbs: Option<Verbs>,
    pub(crate) target: String,
    pub(crate) output: OutputKind,
    pub(crate) view: Option<ToolView>,
    pub(crate) title: Option<String>,
    pub(crate) purpose: ToolPurpose,
}

impl Default for Requested {
    fn default() -> Self {
        Self {
            step: 1,
            block: 0,
            call_id: String::new(),
            tool: "shell".to_owned(),
            input: json!({}),
            action: ToolAction::Run,
            verbs: None,
            target: String::new(),
            output: OutputKind::Text,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        }
    }
}

pub(crate) fn tool_requested(requested: Requested) -> AgentEvent {
    AgentEvent::ToolCallRequested {
        step: requested.step,
        block: requested.block,
        call_id: requested.call_id,
        tool: requested.tool,
        input: requested.input,
        display: ToolDisplay {
            action: requested.action,
            verbs: requested.verbs,
            target: requested.target,
            output: requested.output,
            view: requested.view,
            title: requested.title,
            purpose: requested.purpose,
        },
    }
}

pub(crate) fn show_requested(
    id: &str,
    target: &str,
    title: Option<&str>,
    view: ToolView,
) -> AgentEvent {
    tool_requested(Requested {
        call_id: id.to_owned(),
        tool: "show".to_owned(),
        action: ToolAction::Show,
        target: target.to_owned(),
        output: OutputKind::None,
        view: Some(view),
        title: title.map(str::to_owned),
        purpose: ToolPurpose::Show,
        ..Requested::default()
    })
}

pub(crate) fn show_finished(id: &str, facts: Vec<Fact>, result: &str) -> AgentEvent {
    tool_finished(Finished {
        call_id: id.to_owned(),
        facts,
        duration_ms: 3,
        result: Some(result.to_owned()),
        ..Finished::default()
    })
}

pub(crate) fn at_block(mut event: AgentEvent, at: usize) -> AgentEvent {
    if let AgentEvent::ToolCallRequested { block, .. } = &mut event {
        *block = at;
    }
    event
}

pub(crate) fn output(id: &str, text: &str) -> AgentEvent {
    AgentEvent::ToolOutput {
        call_id: id.to_owned(),
        text: text.to_owned(),
    }
}

pub(crate) fn ok(code: i32) -> ToolOutcome {
    ToolOutcome::Completed {
        is_error: false,
        exit: Some(ExitStatus::Code { code }),
    }
}

pub(crate) fn typed(echoed: bool, unsent: bool, signalled: bool) -> Option<Typed> {
    Some(Typed {
        echoed,
        unsent,
        signalled,
    })
}

pub(crate) struct Finished {
    pub(crate) call_id: String,
    pub(crate) outcome: ToolOutcome,
    pub(crate) output: ToolOutputView,
    pub(crate) facts: Vec<Fact>,
    pub(crate) state: Option<WorkspaceChange>,
    pub(crate) duration_ms: u64,
    pub(crate) result: Option<String>,
    pub(crate) process: Option<u32>,
    pub(crate) attachments: Vec<AttachmentRef>,
    pub(crate) notes: Vec<String>,
    pub(crate) typed: Option<Typed>,
}

impl Default for Finished {
    fn default() -> Self {
        Self {
            call_id: String::new(),
            outcome: ToolOutcome::Completed {
                is_error: false,
                exit: None,
            },
            output: ToolOutputView::default(),
            facts: Vec::new(),
            state: None,
            duration_ms: 0,
            result: None,
            process: None,
            attachments: Vec::new(),
            notes: Vec::new(),
            typed: None,
        }
    }
}

pub(crate) fn tool_finished(finished: Finished) -> AgentEvent {
    AgentEvent::ToolCallFinished {
        call_id: finished.call_id,
        outcome: finished.outcome,
        output: finished.output,
        facts: finished.facts,
        state: finished.state,
        duration_ms: finished.duration_ms,
        result: finished.result,
        process: finished.process,
        attachments: finished.attachments,
        notes: finished.notes,
        typed: finished.typed,
    }
}

pub(crate) fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap()
}
