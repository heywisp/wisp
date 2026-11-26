use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Instant;

use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use wisp_plugin_api::hooks::HeadInfo;
use wisp_protocol::context::Category;
use wisp_protocol::log::{BlockPosition, Cut, KeptAt, LogEvent, LoggedHead, additions_at};
use wisp_protocol::settings::{
    Consequence, Place, SettingValue, SwitchQuote, SwitchTarget, Withdrawal, find, names,
};
use wisp_protocol::{
    ALREADY_COMPACTING, AgentEvent, COMPACT_BETWEEN_TURNS, CONTINUE_NOTE, CacheEffect,
    ClientCommand, CompactionEngine, CompactionFailure, CompactionTrigger, ContentBlock, Cost,
    Covers, Effort, ErrorKind, ExitStatus, Message, NOTHING_TO_COMPACT, Placement, Question,
    QuestionEnd, RebuildReason, RejectKind, RequestId, Role, SessionConfig, StopReason,
    SwitchCompaction, SwitchCompactionCause, ToolOutcome, UnfinishedReason, Usage,
};

use super::fill::used_of;
use super::retry::{self, AfterFailure};
use super::search::{holds_thinking_from, open_calls};
use super::sections::{HeadTexts, Room};
use super::{
    Agent, Failure, Pending, Streamed, TurnTrack, add_usage, check_layout, emit, translate,
};
use crate::context::{Appended, SUMMARY_TOKENS_ESTIMATE, blocks_tokens, fork_request, text_tokens};
use crate::plugin::hooks::points::COMPACTION_HEAD;
use crate::provider::{
    AssembledMessage, Assembler, CompactionAsk, Feature, Features, Iteration, ProviderEvent,
    Refused, Rendering, Request, TailMarker,
};
use crate::tool::question::answered_text;
use crate::workspace::shorten_path;
use gate::Instruction;
use strategy::{HeadAsk, Run};
use wisp_tool_text::compaction::instruction;

pub(super) mod auto;
pub(super) mod background;
pub(super) mod gate;
pub(super) mod overflow;
pub(super) mod retry_turn;
pub(super) mod strategy;
mod threshold;

pub(super) const SUMMARY_INSTRUCTION: &str = "Write a summary of the conversation so far, so \
that the work can continue from the summary alone. Don't call tools.";
pub(super) const VERBATIM_USER_TOKENS: u64 = 8_000;
pub(super) const LEDGER_CALLS: usize = 30;
pub const COMPACTION_MAX_TOKENS: u32 = 32_000;
pub(super) const FIT_MARGIN_TOKENS: u64 = 8_000;
pub(super) const CANCELLED_BEFORE_START: &str = "The compaction was cancelled before it started.";
pub(super) const COMPACTION_CANCELLED: &str = "The compaction was cancelled.";
pub(super) const COMPACT_EFFORT: Effort = Effort::High;
const NATIVE_BLOCK: &str = "compaction";
pub(super) const CUT_OFF_TOKENS: u64 = 2_000;
const WINDOW_EXCEEDED: &str =
    "The compaction request ran out of context window before the summary was written.";

pub(super) fn summary_instruction(focus: Option<&str>) -> String {
    instruction(SUMMARY_INSTRUCTION, focus)
}

fn summary_text(message: &AssembledMessage) -> String {
    let mut text = String::new();
    let mut joins = false;
    for block in &message.content {
        match block {
            ContentBlock::Text { text: part } => {
                if !joins && !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(part);
                joins = true;
            }
            _ => joins = false,
        }
    }
    text
}

fn native_falls_back(stop_reason: &StopReason, wrote_text: bool) -> bool {
    match stop_reason {
        StopReason::Refusal | StopReason::ContextWindowExceeded => false,
        StopReason::EndTurn => wrote_text,
        _ => true,
    }
}

pub(super) fn instruction_tokens() -> u64 {
    text_tokens(&summary_instruction(None))
}

pub(super) fn is_native_block(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Opaque { raw, .. }
        if raw.get("type").and_then(Value::as_str) == Some(NATIVE_BLOCK))
}

fn block_text(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Opaque { raw, .. } => raw
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

fn block_raw(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Opaque { raw, .. } => Some(raw.clone()),
        _ => None,
    }
}

pub(super) fn summed(usage: &Usage, iterations: &[Iteration]) -> Usage {
    if iterations.is_empty() {
        return *usage;
    }
    iterations
        .iter()
        .fold(Usage::default(), |sum, i| add_usage(&sum, &i.usage))
}

fn outcome_text(outcome: &ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Completed {
            exit: Some(ExitStatus::Code { code }),
            ..
        } => format!("exit {code}"),
        ToolOutcome::Completed {
            exit: Some(ExitStatus::Signal { signal }),
            ..
        } => format!("killed by signal {signal}"),
        ToolOutcome::Completed { is_error: true, .. } => "failed".to_owned(),
        ToolOutcome::Completed { .. } => "done".to_owned(),
        ToolOutcome::TimedOut { after_ms } => format!("timed out after {}s", after_ms / 1000),
        ToolOutcome::Cancelled => "cancelled".to_owned(),
        ToolOutcome::NotRun { .. } => "not run".to_owned(),
        ToolOutcome::InvalidInput { .. } => "invalid input".to_owned(),
        ToolOutcome::Failed { .. } => "failed to run".to_owned(),
        ToolOutcome::Backgrounded { .. } => "running in the background".to_owned(),
    }
}

fn last_of(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn closing_flags(mut flags: Vec<bool>, view: &[Message]) -> Vec<bool> {
    if view.last().is_some_and(|m| m.role == Role::Assistant)
        && let Some(last) = flags.last_mut()
    {
        *last = true;
    }
    flags
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Head {
    pub(super) epoch: u64,
    pub(super) from: usize,
    pub(super) message: Message,
    pub(super) summary: String,
    pub(super) native: bool,
    pub(super) turns: (u64, u64),
    pub(super) cut: usize,
    pub(super) placement: Placement,
    pub(super) additions: Vec<ContentBlock>,
    pub(super) join: Option<usize>,
}

impl Head {
    pub(super) fn leading(&self) -> bool {
        self.placement == Placement::Leading
    }

    pub(super) fn logged(&self) -> LoggedHead {
        LoggedHead {
            epoch: self.epoch,
            from: self.from,
            message: self.message.clone(),
            placement: self.placement,
            cut: self.cut,
            additions: self.additions.clone(),
            join: self.join,
        }
    }

    fn joined_at(&self, history: &[Message], message: usize) -> Option<usize> {
        if !self.leading() || self.additions.is_empty() {
            return None;
        }
        let at = KeptAt {
            cut: self.cut,
            from: self.from,
            join: self.join,
        };
        match additions_at(history, at) {
            Some((target, start))
                if target == message
                    && history.get(target).is_some_and(|m| m.role == Role::User) =>
            {
                Some(start)
            }
            _ => None,
        }
    }

    fn shift(&self, history: &[Message], message: usize, block: usize) -> usize {
        match self.joined_at(history, message) {
            Some(start) if block >= start => block + self.additions.len(),
            _ => block,
        }
    }

    fn unshift(&self, history: &[Message], message: usize, block: usize) -> usize {
        match self.joined_at(history, message) {
            Some(start) if block >= start => block.saturating_sub(self.additions.len()).max(start),
            _ => block,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Said {
    turn: u64,
    seq: Option<u64>,
    text: String,
    call_id: Option<String>,
}

pub(super) type Ledger = VecDeque<LoggedCall>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LoggedCall {
    call_id: String,
    turn: u64,
    tool: String,
    target: String,
    cwd: Option<String>,
    status: Option<String>,
    saved: Option<String>,
    attached: Vec<String>,
    withdrawn: bool,
}

pub(super) struct Running {
    pub(super) track: TurnTrack,
}

#[derive(Default)]
pub(super) struct CompactionState {
    pub(super) head: Option<Head>,
    pub(super) compactions: u64,
    pub(super) cost_usd: Option<f64>,
    pub(super) plan_usd: f64,
    pub(super) plan_unknown: bool,
    pub(super) running: Option<Running>,
    said: Vec<Said>,
    opening: Vec<Said>,
    asked: BTreeMap<String, Vec<Question>>,
    calls: VecDeque<LoggedCall>,
    since_seq: u64,
    highest: u64,
    pub(super) auto: auto::Auto,
    pre_turn: bool,
    instructions_by: Vec<String>,
    pub(super) background: background::Background,
    pub(super) overflow: overflow::Overflow,
    pub(super) strategy: Option<String>,
    strategy_run: Option<Run>,
    strategy_estimate: Option<(String, u64, Vec<String>)>,
    strategy_unestimated: Option<(String, u64)>,
    strategy_instruction: Option<(String, u64)>,
    strategy_converted: Option<(String, u64, Vec<String>)>,
}

impl CompactionState {
    pub(super) fn resumed_since(&mut self, seq: u64) {
        self.since_seq = seq;
    }

    pub(super) fn resumed_highest(&mut self, epoch: u64) {
        self.highest = self.highest.max(epoch);
    }

    pub(super) fn since(&self) -> u64 {
        self.since_seq
    }

    pub(super) fn calls_now(&self) -> Ledger {
        self.calls.clone()
    }

    pub(super) fn withdraw_call(&mut self, call_id: &str) {
        if let Some(call) = self.calls.iter_mut().find(|c| c.call_id == call_id) {
            call.withdrawn = true;
        }
    }

    #[cfg(test)]
    pub(super) fn said_texts(&self) -> Vec<&str> {
        self.said.iter().map(|s| s.text.as_str()).collect()
    }

    pub(super) fn rewound_to(
        &mut self,
        turn: u64,
        (head, since_seq): (Option<Head>, u64),
        ledger: Ledger,
    ) {
        let current = self.head.as_ref().map_or(0, |h| h.epoch);
        self.highest = self.highest.max(current);
        self.head = head;
        self.since_seq = since_seq;
        self.said.retain(|s| s.turn < turn);
        self.opening.clear();
        self.calls = ledger;
    }
}

type Fresh = (CompactionEngine, Option<ContentBlock>, String, u64, bool);

impl Outcome {
    fn fresh(self) -> Option<Fresh> {
        match self {
            Self::Native {
                block,
                before,
                partial,
            } => {
                let summary = block_text(&block);
                Some((
                    CompactionEngine::Native,
                    Some(block),
                    summary,
                    before,
                    partial,
                ))
            }
            Self::Written {
                engine,
                summary,
                before,
                partial,
            } => Some((engine, None, summary, before, partial)),
            Self::Convert | Self::Failed { .. } => None,
        }
    }
}

enum Outcome {
    Native {
        block: ContentBlock,
        before: u64,
        partial: bool,
    },
    Written {
        engine: CompactionEngine,
        summary: String,
        before: u64,
        partial: bool,
    },
    Convert,
    Failed {
        engine: CompactionEngine,
        kind: CompactionFailure,
        message: String,
        falls_back: bool,
        too_large: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fit {
    View,
    Below(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Earlier {
    index: usize,
    view_len: usize,
    prompt_tokens: u64,
    tail: Option<TailMarker>,
}

struct Origin {
    trigger: CompactionTrigger,
    turn: Option<u64>,
    focus: Option<String>,
    switch: Option<SwitchCompaction>,
    instructions_by: Vec<String>,
    requested_by: Option<String>,
}

impl Origin {
    fn convert(turn: Option<u64>) -> Self {
        Self {
            trigger: CompactionTrigger::Convert,
            turn,
            focus: None,
            switch: None,
            instructions_by: Vec::new(),
            requested_by: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PreTurn {
    Ready,
    Cancelled,
    Ended(ErrorKind, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SwitchFrom {
    Settled,
    Failing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchDue {
    Compaction,
    KeptTurns,
    SmallerWindow,
    Convert,
}

impl SwitchDue {
    fn cause(self) -> SwitchCompactionCause {
        match self {
            Self::KeptTurns => SwitchCompactionCause::KeptTurns,
            Self::SmallerWindow => SwitchCompactionCause::SmallerWindow,
            Self::Compaction | Self::Convert => SwitchCompactionCause::Convert,
        }
    }

    fn compacts(self) -> bool {
        self != Self::Convert
    }
}

enum SwitchRun {
    Compacted,
    Failed,
    Cancelled,
}

struct Install {
    plan_usd: Option<f64>,
    origin: Origin,
    texts: HeadTexts,
    engine: CompactionEngine,
    block: Option<ContentBlock>,
    summary: String,
    turns: (u64, u64),
    covers: Option<Covers>,
    before: u64,
    partial: bool,
    usage: Option<Usage>,
    cost_usd: Option<f64>,
    list_usd: Option<f64>,
    model: Option<String>,
    lower_bound: bool,
    started: Instant,
}

#[derive(Default)]
struct Spent {
    usage: Option<Usage>,
    usd: Option<f64>,
    list_usd: Option<f64>,
    model: Option<String>,
    cost: Option<Cost>,
    lower_bound: bool,
}

struct FailedCompaction {
    trigger: CompactionTrigger,
    turn: Option<u64>,
    engine: Option<CompactionEngine>,
    kind: CompactionFailure,
    message: String,
    switch: Option<SwitchCompaction>,
    strategy: Option<String>,
    instruction: Option<String>,
    requested_by: Option<String>,
    answers: Option<RequestId>,
}

fn failed(engine: CompactionEngine, failure: &Failure) -> Outcome {
    Outcome::Failed {
        engine,
        kind: CompactionFailure::Error(failure.error.kind),
        message: failure.error.reported_message(),
        falls_back: engine == CompactionEngine::Native
            && (failure.error.refused == Some(Refused::CompactionUnavailable)
                || failure.error.kind == ErrorKind::Overloaded),
        too_large: None,
    }
}

fn too_large(engine: CompactionEngine, message: &str, limit: u64) -> Outcome {
    Outcome::Failed {
        engine,
        kind: CompactionFailure::Error(ErrorKind::ContextOverflow),
        message: message.to_owned(),
        falls_back: false,
        too_large: Some(limit),
    }
}

fn falling_back(mut outcome: Outcome) -> Outcome {
    if let Outcome::Failed { falls_back, .. } = &mut outcome {
        *falls_back = true;
    }
    outcome
}

fn cancelled(engine: CompactionEngine) -> Outcome {
    Outcome::Failed {
        engine,
        kind: CompactionFailure::Cancelled,
        message: COMPACTION_CANCELLED.to_owned(),
        falls_back: false,
        too_large: None,
    }
}

fn malformed(engine: CompactionEngine, problem: &str) -> Outcome {
    Outcome::Failed {
        engine,
        kind: CompactionFailure::Error(ErrorKind::InvalidRequest),
        message: format!("Wisp refused to send a malformed history: {problem}"),
        falls_back: false,
        too_large: None,
    }
}

fn check_failed(message: String) -> Outcome {
    Outcome::Failed {
        engine: CompactionEngine::Wisp,
        kind: CompactionFailure::CheckFailed,
        message,
        falls_back: false,
        too_large: None,
    }
}

pub(super) fn not_smaller(after: u64, before: u64) -> String {
    format!(
        "The summary would leave about {after} tokens, no fewer than the {before} it replaces, so Wisp kept the conversation as it was."
    )
}

fn no_summary(engine: CompactionEngine, stop_reason: StopReason, falls_back: bool) -> Outcome {
    Outcome::Failed {
        engine,
        message: format!(
            "The model stopped ({}) without writing a summary.",
            stop_reason.as_str()
        ),
        kind: CompactionFailure::NoSummary { stop_reason },
        falls_back,
        too_large: None,
    }
}

impl Agent {
    pub(super) fn compacting(&self) -> bool {
        self.compaction.running.is_some()
    }

    pub(super) fn epoch_number(&self) -> u64 {
        self.compaction.head.as_ref().map_or(0, |h| h.epoch)
    }

    pub(super) fn next_epoch(&self) -> u64 {
        self.epoch_number().max(self.compaction.highest) + 1
    }

    pub(super) fn view_from(&self) -> usize {
        self.compaction
            .head
            .as_ref()
            .map_or(0, |h| h.cut.min(h.from))
    }

    pub(super) fn head_blocks(&self) -> Vec<&ContentBlock> {
        self.compaction
            .head
            .iter()
            .flat_map(|h| h.message.content.iter().chain(h.additions.iter()))
            .collect()
    }

    pub(super) fn head_holds_block(&self) -> bool {
        self.compaction.head.as_ref().is_some_and(|h| h.native)
    }

    pub(super) fn head_summary(&self) -> Option<String> {
        self.compaction.head.as_ref().map(|h| h.summary.clone())
    }

    fn head_summary_tokens(&self) -> Option<u64> {
        self.compaction
            .head
            .as_ref()
            .map(|h| text_tokens(&h.summary))
    }

    pub(super) fn at_head(&self) -> bool {
        self.compaction
            .head
            .as_ref()
            .is_some_and(|h| !h.leading() && h.from == self.history.len())
    }

    pub(super) fn compacted(&self, messages: &[Message]) -> Vec<Message> {
        match &self.compaction.head {
            Some(head) => head.logged().view(messages),
            None => messages.to_vec(),
        }
    }

    pub(super) fn view_messages(&self) -> Vec<Message> {
        self.compacted(&self.history)
    }

    pub(super) fn request_view(&self, pending: &Pending) -> Vec<Message> {
        let start = self.compaction.head.as_ref().map_or(0, |head| {
            head.cut
                .min(head.from.saturating_sub(1))
                .min(self.history.len().saturating_sub(1))
        });
        let mut messages = self.history[start..].to_vec();
        if pending.user.is_some() && self.at_head() {
            messages.push(Message {
                role: Role::User,
                content: Vec::new(),
            });
        }
        pending.place(&mut messages);
        let view = match &self.compaction.head {
            Some(head) => {
                let mut logged = head.logged();
                logged.cut -= start;
                logged.from -= start;
                logged.view(&messages)
            }
            None => messages,
        };
        #[cfg(test)]
        tests::views::matches_the_whole_view(self, pending, &view);
        #[cfg(test)]
        assert!(self.nothing_evicted_in_view());
        view
    }

    pub(super) async fn place_pending(
        &mut self,
        pending: &Pending,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if pending.user.is_some()
            && let Some(turn) = self.running_turn()
        {
            self.mark_opening(turn);
        }
        if pending.user.is_some() && self.at_head() {
            self.history.push(Message {
                role: Role::User,
                content: Vec::new(),
            });
        }
        pending.place(&mut self.history);
        if pending.continue_note
            && let Some(user) = &pending.user
        {
            let note = Message {
                role: Role::User,
                content: user[..pending.opening.min(user.len())].to_vec(),
            };
            self.commit_record(note, None, None, events).await;
        }
        if let Some(note) = &pending.tool_note
            && let Some(at) = self.history.iter().rposition(|m| {
                m.role == Role::User
                    && m.content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Text { text } if text == note))
            })
        {
            self.keep_tool_note(at, Some(note));
        }
        let late_marker = pending.late_marker && pending.marker.is_some();
        if (late_marker || pending.late_tools)
            && let Some(system) = pending.system()
        {
            self.commit_record(system, None, None, events).await;
        }
    }

    pub(super) fn full_index(&self, view: usize) -> Option<usize> {
        match &self.compaction.head {
            None => Some(view),
            Some(_) if view == 0 => None,
            Some(head) if head.leading() => Some(head.cut + view - 1),
            Some(head) => Some(head.from + view),
        }
    }

    pub(super) fn view_index(&self, full: usize) -> usize {
        match &self.compaction.head {
            None => full,
            Some(head) if head.leading() => {
                if full < head.cut {
                    0
                } else {
                    full - head.cut + 1
                }
            }
            Some(head) => full.saturating_sub(head.from),
        }
    }

    pub(super) fn history_len(&self, view_len: usize) -> usize {
        match &self.compaction.head {
            None => view_len,
            Some(head) if head.leading() => (head.cut + view_len).saturating_sub(1),
            Some(head) => head.from + view_len,
        }
    }

    pub(super) fn to_view(&self, at: BlockPosition) -> BlockPosition {
        let Some(head) = &self.compaction.head else {
            return at;
        };
        if head.leading() {
            if at.message < head.cut {
                return BlockPosition {
                    message: 0,
                    block: 0,
                };
            }
            return BlockPosition {
                message: at.message - head.cut + 1,
                block: head.shift(&self.history, at.message, at.block),
            };
        }
        match at.message.cmp(&head.from) {
            std::cmp::Ordering::Less => BlockPosition {
                message: 0,
                block: 0,
            },
            std::cmp::Ordering::Equal => BlockPosition {
                message: 0,
                block: head.message.content.len() + at.block,
            },
            std::cmp::Ordering::Greater => BlockPosition {
                message: at.message - head.from,
                block: at.block,
            },
        }
    }

    pub(super) fn to_history(&self, at: BlockPosition) -> BlockPosition {
        let Some(head) = &self.compaction.head else {
            return at;
        };
        if head.leading() {
            if at.message == 0 {
                return BlockPosition {
                    message: head.cut,
                    block: 0,
                };
            }
            let message = head.cut + at.message - 1;
            return BlockPosition {
                message,
                block: head.unshift(&self.history, message, at.block),
            };
        }
        if at.message == 0 {
            BlockPosition {
                message: head.from,
                block: at.block.saturating_sub(head.message.content.len()),
            }
        } else {
            BlockPosition {
                message: head.from + at.message,
                block: at.block,
            }
        }
    }

    pub(super) fn view_cut(&self, cut: Cut) -> Option<Cut> {
        let Some(head) = &self.compaction.head else {
            return Some(cut);
        };
        if head.leading() {
            if cut.through < head.cut {
                return None;
            }
            let from = if cut.from.message < head.cut {
                BlockPosition {
                    message: 0,
                    block: 0,
                }
            } else {
                self.to_view(cut.from)
            };
            return Some(Cut {
                from,
                through: cut.through - head.cut + 1,
                ..cut
            });
        }
        if cut.through <= head.from {
            return None;
        }
        let from = if cut.from.message <= head.from {
            BlockPosition {
                message: 0,
                block: 0,
            }
        } else {
            self.to_view(cut.from)
        };
        Some(Cut {
            from,
            through: cut.through - head.from,
            ..cut
        })
    }

    pub(super) fn logged_seq(&self) -> Option<u64> {
        self.log.as_ref().map(|(log, _)| log.next_seq())
    }

    pub(super) fn note_opening(&mut self, turn: u64, texts: Vec<String>, seq: Option<u64>) {
        self.compaction.opening = texts
            .into_iter()
            .filter(|text| !text.trim().is_empty())
            .map(|text| Said {
                turn,
                seq,
                text,
                call_id: None,
            })
            .collect();
    }

    pub(super) fn note_inputs(&mut self, turn: u64, texts: Vec<String>, seq: Option<u64>) {
        let opening = std::mem::take(&mut self.compaction.opening);
        self.compaction.said.extend(opening);
        self.compaction.said.extend(
            texts
                .into_iter()
                .filter(|text| !text.trim().is_empty())
                .map(|text| Said {
                    turn,
                    seq,
                    text,
                    call_id: None,
                }),
        );
    }

    pub(super) fn note_event(&mut self, event: &AgentEvent, seq: Option<u64>) {
        let turn = self.running_turn().unwrap_or(self.turns);
        let state = &mut self.compaction;
        match event {
            AgentEvent::ToolCallRequested {
                call_id,
                tool,
                display,
                ..
            } => {
                self.targets.insert(call_id.clone(), display.target.clone());
                state.calls.push_back(LoggedCall {
                    call_id: call_id.clone(),
                    turn,
                    tool: tool.clone(),
                    target: display.target.clone(),
                    cwd: None,
                    status: None,
                    saved: None,
                    attached: Vec::new(),
                    withdrawn: false,
                });
                while state.calls.len() > LEDGER_CALLS {
                    state.calls.pop_front();
                }
            }
            AgentEvent::ToolCallStarted { call_id, cwd, .. } => {
                if let Some(call) = state.calls.iter_mut().find(|c| c.call_id == *call_id) {
                    call.cwd = Some(cwd.clone());
                }
            }
            AgentEvent::ToolCallFinished {
                call_id,
                outcome,
                output,
                attachments,
                ..
            } => {
                let home = self.tools.as_ref().and_then(|e| e.workspace.home.clone());
                if let Some(call) = state.calls.iter_mut().find(|c| c.call_id == *call_id) {
                    call.status = Some(outcome_text(outcome));
                    call.saved = output
                        .file
                        .as_deref()
                        .map(|file| shorten_path(Path::new(file), home.as_deref()));
                    call.attached = attachments
                        .iter()
                        .map(|a| match a.slice {
                            Some(slice) => format!("{} ({})", a.media_type, slice.read_only("-")),
                            None => a.media_type.to_string(),
                        })
                        .collect();
                }
            }
            AgentEvent::AttachmentsWithdrawn { items, .. } => {
                for id in items.iter().filter_map(|item| item.call_id.as_deref()) {
                    state.withdraw_call(id);
                }
            }
            AgentEvent::QuestionAsked { call_id, questions }
                if !questions
                    .iter()
                    .any(wisp_protocol::Question::kept_from_model) =>
            {
                state
                    .said
                    .retain(|s| s.call_id.as_deref() != Some(call_id.as_str()));
                state.asked.insert(call_id.clone(), questions.clone());
            }
            AgentEvent::QuestionClosed { call_id, how } => {
                let questions = state.asked.remove(call_id).unwrap_or_default();
                if let QuestionEnd::Answered { answers } = how {
                    for (question, answer) in questions.iter().zip(answers) {
                        if answer.is_skipped() {
                            continue;
                        }
                        let text = answered_text(
                            std::slice::from_ref(question),
                            std::slice::from_ref(answer),
                        );
                        state.said.push(Said {
                            turn,
                            seq,
                            text,
                            call_id: Some(call_id.clone()),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn verbatim_budget(&self) -> u64 {
        let global = || match find(names::CONTEXT_VERBATIM_USER_TOKENS)
            .and_then(|spec| self.resolved_at(spec, &Place::Global))
        {
            Some(SettingValue::Tokens(n)) => Some(n),
            _ => None,
        };
        self.models
            .verbatim_user_tokens(self.endpoint_name(), &self.session.model)
            .or_else(global)
            .unwrap_or(VERBATIM_USER_TOKENS)
    }

    fn shown(&self, path: &Path) -> Option<String> {
        let home = self.tools.as_ref()?.workspace.home.as_deref()?;
        Some(shorten_path(path, Some(home)))
    }

    fn log_pointer(&self) -> Option<String> {
        let (log, _) = self.log.as_ref()?;
        let path = log.location()?;
        Some(self.shown(&path).unwrap_or_else(|| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{}.jsonl", self.session_id));
            format!("{name} in Wisp's sessions folder")
        }))
    }

    fn output_pointer(&self) -> Option<String> {
        self.output_dir().map(|dir| {
            self.shown(dir.path())
                .unwrap_or_else(|| dir.path().display().to_string())
        })
    }

    fn compacted_context(&self, epoch: u64, turns: (u64, u64), summary: Option<&str>) -> String {
        let mut text = format!(
            "<compacted_context epoch=\"{epoch}\" turns=\"{}\u{2013}{}\">\n",
            turns.0, turns.1
        );
        if let Some(summary) = summary {
            text.push_str("<summary>\n");
            text.push_str(summary.trim_end());
            text.push_str("\n</summary>\n");
        }
        text.push_str("</compacted_context>\n");
        text
    }

    pub(super) fn build_head(
        &self,
        block: Option<ContentBlock>,
        summary: &str,
        (epoch, turns): (u64, (u64, u64)),
        texts: &HeadTexts,
    ) -> Message {
        let native = block.is_some();
        let mut content: Vec<ContentBlock> = block.into_iter().collect();
        content.extend(texts.blocks());
        match texts.strategy_blocks() {
            Some(blocks) => content.extend(blocks),
            None => content.push(ContentBlock::Text {
                text: self.compacted_context(epoch, turns, (!native).then_some(summary)),
            }),
        }
        content.extend(texts.hook_blocks());
        Message {
            role: Role::User,
            content,
        }
    }

    pub(super) fn compaction_head_tokens(&self) -> u64 {
        self.head_summary_tokens()
            .unwrap_or(SUMMARY_TOKENS_ESTIMATE)
            .saturating_add(self.head_additions_tokens())
    }

    fn head_additions_tokens(&self) -> u64 {
        self.head_additions_for(self.next_turns())
    }

    pub(super) fn head_additions_for(&self, turns: (u64, u64)) -> u64 {
        let epoch = self.next_epoch();
        self.head_section_tokens()
            .saturating_add(self.layout_tokens(epoch, turns))
            .saturating_add(self.head_reserve_tokens())
    }

    pub(super) fn head_reserve_tokens(&self) -> u64 {
        self.hooks
            .engine
            .reserves(COMPACTION_HEAD)
            .iter()
            .map(|text| text_tokens(text))
            .fold(0, u64::saturating_add)
    }

    pub(super) fn head_tokens(&self, block: &ContentBlock, turns: (u64, u64)) -> u64 {
        blocks_tokens(std::slice::from_ref(block)).saturating_add(self.head_additions_for(turns))
    }

    pub(super) fn turn_message_tokens(&self, typed: u64) -> u64 {
        blocks_tokens(&self.provisional_head()).saturating_add(typed)
    }

    fn last_turn(&self) -> u64 {
        if self.compaction.pre_turn {
            self.turns.saturating_sub(1)
        } else {
            self.turns
        }
    }

    fn next_turns(&self) -> (u64, u64) {
        let first = self
            .compaction
            .head
            .as_ref()
            .map_or(1, |h| h.turns.1.saturating_add(1));
        let last = self.last_turn();
        (first.min(last.max(1)), last)
    }

    fn covers(&self, turns: (u64, u64)) -> Option<Covers> {
        self.covers_until(turns, self.logged_seq())
    }

    fn covers_until(&self, turns: (u64, u64), next: Option<u64>) -> Option<Covers> {
        let next = next?;
        Some(Covers {
            first_seq: self.compaction.since_seq,
            last_seq: next.saturating_sub(1),
            first_turn: turns.0,
            last_turn: turns.1,
        })
    }

    pub(super) fn fill_estimate(&self) -> u64 {
        let categories = self.categories(self.history.len(), self.last.is_none());
        self.next_tokens(&categories)
    }

    fn has_new_content(&self) -> bool {
        self.history.len() > self.view_from()
    }

    pub(super) fn needs_convert(&self) -> bool {
        self.head_holds_block()
            && self
                .feature_use(&self.session.model, Feature::NativeCompaction)
                .is_none()
    }

    fn compaction_max_tokens(&self) -> u32 {
        let documented = self.provider().capabilities(&self.session.model);
        let documented = documented.and_then(|row| row.max_output);
        self.models
            .max_output(self.endpoint_name(), &self.session.model, documented)
            .map_or(COMPACTION_MAX_TOKENS, |cap| cap.min(COMPACTION_MAX_TOKENS))
    }

    fn compaction_rendering(&self, len: usize) -> Rendering {
        let mut rendering = self.rendering(len);
        let open = open_calls(self.provider().as_ref(), &self.history, &self.search.closed);
        if let (Some(from), Some(through)) = (
            open.iter().map(|(at, _)| *at).min(),
            self.history.len().checked_sub(1),
        ) && holds_thinking_from(&self.history, from, through)
            && let Some(cut) = self.view_cut(Cut {
                from,
                through,
                tools: false,
            })
        {
            rendering.cuts.push(cut);
        }
        rendering.closed.extend(open.into_iter().map(|(_, id)| id));
        rendering
    }

    async fn refuse_compact(
        &mut self,
        request: RequestId,
        kind: RejectKind,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let reason = match kind {
            RejectKind::TurnRunning => COMPACT_BETWEEN_TURNS,
            RejectKind::NothingToCompact => NOTHING_TO_COMPACT,
            _ => ALREADY_COMPACTING,
        };
        let event = AgentEvent::Rejected {
            reason: reason.to_owned(),
            id: None,
            kind: Some(kind),
            request: Some(request),
        };
        self.emit(events, event).await;
    }

    fn manual_compaction_endpoint(&self) -> usize {
        if self.bound_endpoint().is_none() {
            self.next_endpoint()
        } else {
            self.endpoint
        }
    }

    pub(super) async fn compact_command(
        &mut self,
        focus: Option<String>,
        request: RequestId,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.start_log(events).await;
        let target = self.manual_compaction_endpoint();
        if let Err(reason) = self.check_resume_destination(target) {
            self.emit(events, AgentEvent::refused(Some(request), reason))
                .await;
            return;
        }
        if self.track.is_some() {
            self.refuse_compact(request, RejectKind::TurnRunning, events)
                .await;
            return;
        }
        if self.compacting() || self.queue.holds_compact() {
            self.refuse_compact(request, RejectKind::Compacting, events)
                .await;
            return;
        }
        self.enqueue(super::queue::Queued::Compact { focus, request }, events)
            .await;
    }

    pub(super) async fn cancel_compact(
        &mut self,
        request: RequestId,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let failure = FailedCompaction {
            trigger: CompactionTrigger::Manual,
            turn: None,
            engine: None,
            kind: CompactionFailure::Cancelled,
            message: CANCELLED_BEFORE_START.to_owned(),
            switch: None,
            strategy: None,
            instruction: None,
            requested_by: None,
            answers: Some(request),
        };
        let event = self
            .compaction_failed(failure, &Spent::default(), events)
            .await;
        emit(events, event).await;
    }

    async fn compaction_failed(
        &mut self,
        failure: FailedCompaction,
        spent: &Spent,
        events: &mpsc::Sender<AgentEvent>,
    ) -> AgentEvent {
        let record = LogEvent::CompactionFailed {
            plan_usd: spent.cost.and_then(|c| c.turn_plan_usd),
            turn: failure.turn,
            trigger: failure.trigger,
            engine: failure.engine,
            kind: failure.kind.clone(),
            message: failure.message.clone(),
            usage: spent.usage,
            cost_usd: spent.usd,
            list_usd: spent.list_usd,
            model: spent.model.clone(),
            lower_bound: spent.lower_bound,
            switch: failure.switch,
            strategy: failure.strategy,
            instruction: failure.instruction,
            requested_by: failure.requested_by.clone(),
        };
        self.log(events, record).await;
        AgentEvent::CompactionFailed {
            trigger: failure.trigger,
            engine: failure.engine,
            kind: failure.kind,
            message: failure.message,
            usage: spent.usage,
            cost: spent.cost,
            answers: failure.answers,
            turn: failure.turn,
            paused: false,
            requested_by: failure.requested_by,
        }
    }

    pub(super) fn manual_compaction(
        &mut self,
        focus: Option<String>,
        request: RequestId,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.manual_compaction_unboxed(focus, request, commands, events))
    }

    async fn manual_compaction_unboxed(
        &mut self,
        focus: Option<String>,
        request: RequestId,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.start_log(events).await;
        self.plugin_moment(false, events).await;
        if !self.has_new_content() {
            self.refuse_compact(request, RejectKind::NothingToCompact, events)
                .await;
            return;
        }
        let target = self.manual_compaction_endpoint();
        let admission = self.check_resume_destination(target).and_then(|()| {
            crate::endpoints::generation::admit(self.provider_at(target).generation())
                .map_err(|error| error.to_string())
        });
        let bound = match admission {
            Ok(lease) => self.bind_unbound(events).await.map(|()| lease),
            Err(reason) => Err(reason),
        };
        let _admission = match bound {
            Ok(lease) => lease,
            Err(reason) => {
                self.emit(events, AgentEvent::refused(Some(request), reason))
                    .await;
                return;
            }
        };
        let trigger = CompactionTrigger::Manual;
        let gate = self.gate(trigger).discarding(gate::Discard::Any(None));
        let gate = self.hand(gate, events).await;
        let typed = focus.clone().unwrap_or_default();
        self.compaction.running = Some(Running {
            track: TurnTrack::new(self.turns),
        });
        let started_event = AgentEvent::CompactionStarted {
            trigger,
            turn: None,
            switch: None,
            requested_by: None,
        };
        emit(events, started_event).await;
        let started = Instant::now();
        let result = self
            .summarise((gate, focus.as_deref()), true, Fit::View, commands, events)
            .await;
        let by = std::mem::take(&mut self.compaction.instructions_by);
        let (result, texts) = self
            .head_step_for(result, (trigger, Fit::View), commands, events)
            .await;
        let spent = self.spent();
        self.add_compaction_cost(spent.cost);
        let cache_effect = CacheEffect::Rebuilt {
            reason: RebuildReason::Compaction,
        };
        let installed = match result {
            Outcome::Convert => {
                self.close_compacted_tail(events).await;
                let origin = Origin {
                    trigger,
                    turn: None,
                    focus: focus.clone(),
                    switch: None,
                    instructions_by: by.clone(),
                    requested_by: None,
                };
                let (before, after, partial) = self
                    .convert(origin, (&spent, texts.clone()), started, events)
                    .await;
                self.measure_next(auto::Measure::releasing(0));
                let event = AgentEvent::Compacted {
                    trigger,
                    engine: CompactionEngine::Wisp,
                    before,
                    after,
                    partial,
                    converted: true,
                    cache_effect: cache_effect.clone(),
                    usage: spent.usage,
                    cost: spent.cost,
                    answers: Some(request),
                    turn: None,
                    summary: self.head_summary(),
                    paused: false,
                };
                emit(events, event).await;
                emit(events, self.context_changed()).await;
                None
            }
            Outcome::Failed {
                engine,
                kind,
                message,
                ..
            } => {
                let failure = FailedCompaction {
                    trigger,
                    turn: None,
                    engine: Some(engine),
                    kind,
                    message,
                    switch: None,
                    strategy: texts.chosen.clone(),
                    instruction: texts.instruction.clone(),
                    requested_by: None,
                    answers: Some(request),
                };
                let event = self.compaction_failed(failure, &spent, events).await;
                emit(events, event).await;
                None
            }
            fresh => fresh.fresh(),
        };
        if let Some((engine, block, summary, before, partial)) = installed {
            self.close_unkept_tail(events).await;
            let turns = self.next_turns();
            let install = Install {
                plan_usd: spent.cost.and_then(|c| c.turn_plan_usd),
                origin: Origin {
                    trigger,
                    turn: None,
                    focus,
                    switch: None,
                    instructions_by: by,
                    requested_by: None,
                },
                texts,
                engine,
                block,
                summary: summary.clone(),
                turns,
                covers: self.covers(turns),
                before,
                partial,
                usage: spent.usage,
                cost_usd: spent.usd,
                list_usd: spent.list_usd,
                model: spent.model.clone(),
                lower_bound: spent.lower_bound,
                started,
            };
            let after = self.install(install, events).await;
            self.measure_next(auto::Measure::releasing(0));
            let event = AgentEvent::Compacted {
                trigger,
                engine,
                before,
                after,
                partial,
                converted: false,
                cache_effect,
                usage: spent.usage,
                cost: spent.cost,
                answers: Some(request),
                turn: None,
                summary: Some(summary),
                paused: false,
            };
            emit(events, event).await;
            emit(events, self.context_changed()).await;
        }
        self.sync_log(events).await;
        self.defer_command("compact".to_owned(), typed);
    }

    fn summarise_view<'a>(
        &'a mut self,
        instruction: &'a Instruction,
        retries: bool,
        earlier: Option<&'a Earlier>,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Outcome> + 'a>> {
        Box::pin(self.summarise_view_unboxed(instruction, retries, earlier, commands, events))
    }

    async fn summarise_view_unboxed(
        &mut self,
        instruction: &Instruction,
        retries: bool,
        earlier: Option<&Earlier>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Outcome {
        if self
            .feature_use(&self.session.model, Feature::NativeCompaction)
            .is_none()
        {
            return if self.head_holds_block() {
                Outcome::Convert
            } else {
                self.send_compaction(
                    CompactionEngine::Wisp,
                    instruction,
                    retries,
                    earlier,
                    commands,
                    events,
                )
                .await
            };
        }
        match self
            .send_compaction(
                CompactionEngine::Native,
                instruction,
                retries,
                earlier,
                commands,
                events,
            )
            .await
        {
            Outcome::Failed {
                falls_back: true, ..
            } => {
                self.send_compaction(
                    CompactionEngine::Wisp,
                    instruction,
                    retries,
                    earlier,
                    commands,
                    events,
                )
                .await
            }
            outcome => outcome,
        }
    }

    async fn head_step_for(
        &mut self,
        result: Outcome,
        (trigger, fit): (CompactionTrigger, Fit),
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (Outcome, HeadTexts) {
        let run = self.compaction.strategy_run.take();
        let Some(room) = self.head_room(&result, trigger, fit) else {
            let texts = HeadTexts {
                chosen: run.as_ref().map(|r| r.plugin.clone()),
                instruction: run.and_then(|r| r.supplied),
                ..HeadTexts::default()
            };
            return (result, texts);
        };
        let ask = run.map(|run| self.head_ask(run, &result));
        let texts = self.head_step(room, ask, commands, events).await;
        match texts.refused.clone() {
            Some(message) => (check_failed(message), texts),
            None => (result, texts),
        }
    }

    fn head_ask(&self, run: Run, result: &Outcome) -> HeadAsk {
        let (summary, native, before) = match result {
            Outcome::Native { .. } => (None, true, None),
            Outcome::Written {
                engine,
                summary,
                before,
                ..
            } => (
                Some(summary.clone()),
                false,
                (*engine == CompactionEngine::Wisp).then_some(*before),
            ),
            Outcome::Convert | Outcome::Failed { .. } => (self.head_summary(), false, None),
        };
        HeadAsk {
            run,
            summary,
            native,
            before,
        }
    }

    fn head_info(
        &self,
        trigger: CompactionTrigger,
        engine: CompactionEngine,
        turns: (u64, u64),
    ) -> HeadInfo {
        HeadInfo {
            epoch: self.next_epoch(),
            trigger: trigger.as_str().to_owned(),
            engine: engine.as_str().to_owned(),
            first_turn: turns.0,
            last_turn: turns.1,
        }
    }

    fn head_room(&self, result: &Outcome, trigger: CompactionTrigger, fit: Fit) -> Option<Room> {
        let reserve = self.head_reserve_tokens();
        let fixed = self.fixed_tokens();
        let kept = self.compaction.overflow.kept;
        let (engine, turns, smaller, size) = match result {
            Outcome::Failed { .. } => return None,
            Outcome::Native { block, before, .. } => {
                let next = self.next_turns();
                let turns = kept.map_or(next, |k| (next.0, k.last_turn.max(next.0)));
                let head = self.head_tokens(block, turns);
                let after = fixed.saturating_add(head).saturating_sub(reserve);
                let size = match kept {
                    Some(k) => self.kept_size(k.view_len, head).saturating_sub(reserve),
                    None => self.fitted(after),
                };
                (
                    CompactionEngine::Native,
                    turns,
                    Some((*before, after)),
                    size,
                )
            }
            Outcome::Written {
                engine,
                summary,
                before,
                ..
            } => {
                let after = self.head_fill(summary);
                (
                    *engine,
                    self.next_turns(),
                    Some((*before, after)),
                    self.fitted(after),
                )
            }
            Outcome::Convert => {
                let turns = self.converted_turns().unwrap_or_else(|| self.next_turns());
                let after = fixed
                    .saturating_add(self.converted_head_tokens())
                    .saturating_sub(reserve);
                (CompactionEngine::Wisp, turns, None, self.fitted(after))
            }
        };
        let fits = match fit {
            Fit::View => None,
            Fit::Below(limit) => Some((limit, size)),
        };
        Some(Room {
            info: self.head_info(trigger, engine, turns),
            smaller,
            fits,
        })
    }

    fn fitted(&self, after: u64) -> u64 {
        after
            .saturating_add(self.compaction.overflow.reserve)
            .saturating_add(u64::from(self.session.max_tokens))
    }

    fn summary_engine(&self) -> CompactionEngine {
        if self
            .feature_use(&self.session.model, Feature::NativeCompaction)
            .is_some()
        {
            CompactionEngine::Native
        } else {
            CompactionEngine::Wisp
        }
    }

    fn spent(&mut self) -> Spent {
        let track = self.compaction.running.take().map(|r| r.track);
        let spend = track.as_ref().map(TurnTrack::spend).unwrap_or_default();
        let plan = track.as_ref().and_then(TurnTrack::plan_split);
        let cost = spend
            .usd
            .map(|turn_usd| self.cost_now(turn_usd, plan, &spend));
        Spent {
            usage: spend.usage,
            usd: spend.usd,
            list_usd: spend.list_usd,
            model: track.and_then(|t| t.served),
            cost,
            lower_bound: spend.lower_bound,
        }
    }

    fn count_in_turn(&mut self, spent: &Spent) {
        self.track().absorb(spent);
    }

    fn add_compaction_cost(&mut self, cost: Option<Cost>) {
        if let Some(cost) = cost {
            self.compaction.cost_usd =
                Some(self.compaction.cost_usd.unwrap_or(0.0) + cost.turn_usd);
            self.compaction.plan_usd += cost.turn_plan_usd.unwrap_or(0.0);
            self.compaction.plan_unknown |=
                cost.turn_billing == wisp_protocol::Billing::Mixed && cost.turn_plan_usd.is_none();
        }
    }

    pub(super) async fn close_compacted_tail(&mut self, events: &mpsc::Sender<AgentEvent>) {
        match self.search.failed_tail.take() {
            Some(billed) => {
                self.close_tail_records(UnfinishedReason::Failed, &billed, events)
                    .await;
            }
            None => {
                self.close_tail_records(UnfinishedReason::Stopped, &BTreeSet::new(), events)
                    .await;
            }
        }
    }

    async fn close_unkept_tail(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.compaction.overflow.kept.is_none() {
            self.close_compacted_tail(events).await;
        }
    }

    async fn install(&mut self, install: Install, events: &mpsc::Sender<AgentEvent>) -> u64 {
        let kept = self.compaction.overflow.kept.take();
        if let (Some(kept), Some(block)) = (kept, install.block.as_ref())
            && let Some(cut) = self.full_index(kept.view_len)
        {
            self.compaction.overflow.fell_back = false;
            if self.tail_live() {
                match self.track.as_ref().map(|t| t.carried_billed.clone()) {
                    Some(billed) => {
                        self.close_tail(UnfinishedReason::Compacted, &billed, events)
                            .await;
                    }
                    None => self.close_compacted_tail(events).await,
                }
            }
            let turns = (install.turns.0, kept.last_turn.max(install.turns.0));
            let leading = background::Leading {
                plan_usd: install.plan_usd,
                block: block.clone(),
                cut,
                turns,
                covers: self.covers_until(turns, kept.seq),
                trigger: install.origin.trigger,
                turn: install.origin.turn,
                focus: install.origin.focus,
                requested_by: install.origin.requested_by,
                instructions_by: install.origin.instructions_by,
                texts: install.texts,
                before: install.before,
                usage: install.usage,
                cost_usd: install.cost_usd,
                list_usd: install.list_usd,
                model: install.model,
                lower_bound: install.lower_bound,
                duration_ms: u64::try_from(install.started.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
            };
            return self.install_leading(leading, events).await.0;
        }
        self.lapse_offer(super::choice::COMPACTED);
        let epoch = self.next_epoch();
        let from = self.history.len();
        let native = install.block.is_some();
        let raw = install.block.as_ref().and_then(block_raw);
        if std::mem::take(&mut self.compaction.overflow.fell_back) {
            self.compaction.overflow.cut_off = None;
        }
        let message = self.build_head(
            install.block,
            &install.summary,
            (epoch, install.turns),
            &install.texts,
        );
        self.compaction.overflow.cut_off = None;
        self.compaction.overflow.due = false;
        Box::pin(self.log_head_instructions(&install.texts, (install.origin.turn, epoch), events))
            .await;
        self.install_sections(&install.texts);
        self.compaction.head = Some(Head {
            epoch,
            from,
            message: message.clone(),
            summary: install.summary.clone(),
            native,
            turns: install.turns,
            cut: from,
            placement: Placement::Head,
            additions: Vec::new(),
            join: None,
        });
        self.compaction.compactions += 1;
        self.last = None;
        self.search.drops_covered = false;
        self.rewind.compacted();
        self.epoch.clear();
        self.accepted = super::withdraw::end_of(&self.history);
        let after = self.fill_estimate();
        let record = LogEvent::ContextCompacted {
            plan_usd: install.plan_usd,
            sections: install.texts.logged(),
            turn: install.origin.turn,
            epoch,
            trigger: install.origin.trigger,
            engine: install.engine,
            focus: install.origin.focus,
            before: install.before,
            after,
            partial: install.partial,
            covers: install.covers,
            from,
            head: message,
            summary: (!native).then_some(install.summary),
            block: raw,
            usage: install.usage,
            cost_usd: install.cost_usd,
            list_usd: install.list_usd,
            model: install.model,
            lower_bound: install.lower_bound,
            duration_ms: u64::try_from(install.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            switch: install.origin.switch,
            placement: Placement::Head,
            cut: None,
            additions: Vec::new(),
            instructions_by: install.origin.instructions_by,
            strategy: install.texts.built_by(),
            instruction: install.texts.instruction.clone(),
            requested_by: install.origin.requested_by,
        };
        self.log(events, record).await;
        self.evict_media();
        self.compaction.since_seq = self.logged_seq().unwrap_or(0);
        self.refresh_strategy(super::hooks::Mode::Turn, None, events)
            .await;
        after
    }

    async fn convert(
        &mut self,
        origin: Origin,
        (spent, texts): (&Spent, HeadTexts),
        started: Instant,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (u64, u64, bool) {
        let Some(old) = self.compaction.head.clone() else {
            return (0, 0, false);
        };
        let before = self.fill_estimate();
        let partial = self.history.len() > old.cut;
        let turns = (old.turns.0, self.last_turn().max(old.turns.1));
        let install = Install {
            plan_usd: spent.cost.and_then(|c| c.turn_plan_usd),
            origin,
            texts,
            engine: CompactionEngine::Wisp,
            block: None,
            summary: old.summary,
            turns,
            covers: None,
            before,
            partial,
            usage: spent.usage,
            cost_usd: spent.usd,
            list_usd: spent.list_usd,
            model: spent.model.clone(),
            lower_bound: spent.lower_bound,
            started,
        };
        let after = self.install(install, events).await;
        (before, after, partial)
    }

    pub(super) fn convert_in_turn<'a>(
        &'a mut self,
        pending: &'a mut Pending,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = bool> + 'a>> {
        Box::pin(self.convert_in_turn_unboxed(pending, commands, events))
    }

    async fn convert_in_turn_unboxed(
        &mut self,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        if !self.head_holds_block() {
            return false;
        }
        self.restore(pending);
        let kept = {
            let track = self.track();
            track.committed_steps > 0 || track.continues
        };
        if kept {
            let billed = self.track().carried_billed.clone();
            self.close_tail(UnfinishedReason::Compacted, &billed, events)
                .await;
        }
        let turn = self.running_turn();
        let model = self.session.model.clone();
        let target = (self.endpoint, model.as_str(), auto::pending_tokens(pending));
        self.convert_now(turn, target, commands, events).await;
        self.continue_after_head(pending);
        true
    }

    pub(super) fn continue_after_head(&mut self, pending: &mut Pending) {
        pending.retry_note = false;
        let leading = self.compaction.head.as_ref().is_some_and(Head::leading);
        let kept = {
            let track = self.track();
            track.committed_steps > 0 || track.continues
        } && !leading;
        if kept {
            let mut rest = pending.user.take().unwrap_or_default();
            if pending.continue_note {
                rest.drain(..pending.opening.min(rest.len()));
            }
            let mut user = vec![ContentBlock::Text {
                text: CONTINUE_NOTE.to_owned(),
            }];
            pending.opening = user.len();
            user.extend(rest);
            pending.user = Some(user);
            pending.continue_note = true;
        }
        self.recompute_marker(pending);
    }

    pub(super) fn recompute_marker(&self, pending: &mut Pending) {
        let had_system = pending.has_system();
        if pending.marker.is_none() {
            pending.marker = self.turn_marker();
            pending.late_marker = pending.marker.is_some();
        }
        self.recompute_tools(pending);
        if !had_system
            && pending.has_system()
            && let Some(user) = pending.user.as_mut()
        {
            let steers = user.split_off(pending.opening.min(user.len()));
            pending.after.splice(0..0, steers);
        }
    }

    pub(super) fn recompute_tools(&self, pending: &mut Pending) {
        let tools = self.restated_tools();
        if pending.tools != tools {
            pending.tools = tools;
            pending.late_tools = !pending.tools.is_empty();
        }
        if !pending.has_system() && pending.user.is_none() && !pending.after.is_empty() {
            pending.user = Some(std::mem::take(&mut pending.after));
        }
    }

    async fn convert_now(
        &mut self,
        turn: Option<u64>,
        (endpoint, model, pending): (usize, &str, u64),
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let trigger = CompactionTrigger::Convert;
        let started_event = AgentEvent::CompactionStarted {
            trigger,
            turn,
            switch: None,
            requested_by: None,
        };
        emit(events, started_event).await;
        let reserve = self.head_reserve_tokens();
        let fits = self
            .converted_bound(endpoint, model, pending)
            .map(|(limit, estimate)| (limit, estimate.saturating_sub(reserve)));
        let turns = self.converted_turns().unwrap_or_else(|| self.next_turns());
        let room = Room {
            info: self.head_info(trigger, CompactionEngine::Wisp, turns),
            smaller: None,
            fits,
        };
        let compact = self.convert_compact_info(trigger);
        let run = self
            .open_strategy(compact, crate::plugin::hooks::strategy::HEAD_POINT, events)
            .await;
        let ask = run.map(|run| self.head_ask(run, &Outcome::Convert));
        let texts = self.head_step(room, ask, commands, events).await;
        let (before, after, partial) = self
            .convert(
                Origin::convert(turn),
                (&Spent::default(), texts),
                Instant::now(),
                events,
            )
            .await;
        let event = AgentEvent::Compacted {
            trigger,
            engine: CompactionEngine::Wisp,
            before,
            after,
            partial,
            converted: true,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: None,
            cost: None,
            answers: None,
            turn,
            summary: self.head_summary(),
            paused: false,
        };
        emit(events, event).await;
        emit(events, self.context_changed()).await;
    }

    fn keeps_turns(&self) -> bool {
        self.compaction.head.as_ref().is_some_and(Head::leading)
    }

    fn switch_due(
        &self,
        endpoint: usize,
        model: &str,
        (from, pending): (SwitchFrom, Option<u64>),
    ) -> Option<SwitchDue> {
        if self.bound_endpoint().is_none() {
            return (self.head_holds_block() && !self.takes_block(endpoint, model))
                .then_some(SwitchDue::Convert);
        }
        if self.next_auto_compact()
            && pending.is_some_and(|tokens| self.over_new_point(endpoint, model, tokens))
        {
            return Some(SwitchDue::SmallerWindow);
        }
        if from == SwitchFrom::Settled && self.keeps_turns() && model != self.session.model {
            return Some(SwitchDue::KeptTurns);
        }
        if !self.head_holds_block() || self.takes_block(endpoint, model) {
            return None;
        }
        let switching = endpoint != self.endpoint || model != self.session.model;
        let old_takes = self
            .feature_use(&self.session.model, Feature::NativeCompaction)
            .is_some();
        Some(if switching && old_takes && self.has_new_content() {
            SwitchDue::Compaction
        } else {
            SwitchDue::Convert
        })
    }

    fn compaction_estimate(&self) -> u64 {
        self.compact_now_cost().map_or(0, |c| c.estimate_micros)
    }

    pub(super) fn converted_turns(&self) -> Option<(u64, u64)> {
        let old = self.compaction.head.as_ref()?;
        Some((old.turns.0, self.last_turn().max(old.turns.1)))
    }

    fn converted_head_tokens(&self) -> u64 {
        let (Some(old), Some(turns)) = (&self.compaction.head, self.converted_turns()) else {
            return 0;
        };
        let mut texts = self.estimated_head_texts();
        texts.strategy = self.converted_estimate();
        let head = self.build_head(None, &old.summary, (old.epoch + 1, turns), &texts);
        blocks_tokens(&head.content).saturating_add(self.head_reserve_tokens())
    }

    fn switch_consequence(&self, due: SwitchDue) -> Consequence {
        let (estimate_micros, head_tokens) = match due {
            SwitchDue::Compaction | SwitchDue::KeptTurns | SwitchDue::SmallerWindow => {
                (self.compaction_estimate(), self.compaction_head_tokens())
            }
            SwitchDue::Convert => (0, self.converted_head_tokens()),
        };
        Consequence::CompactsBeforeSwitch {
            estimate_micros,
            head_tokens,
            cause: due.cause(),
        }
    }

    pub(super) fn past_point_notice(&self, endpoint: usize, model: &str) -> Option<AgentEvent> {
        (!self.next_auto_compact() && self.over_new_point(endpoint, model, 0)).then(|| {
            AgentEvent::PastCompactionPoint {
                endpoint: self.connection_name(endpoint).to_owned(),
                model: model.to_owned(),
            }
        })
    }

    pub(super) fn switch_quote(&self, endpoint: usize, model: &str) -> Option<Consequence> {
        let due = self.switch_due(endpoint, model, (SwitchFrom::Settled, Some(0)))?;
        Some(self.switch_consequence(due))
    }

    pub(super) fn switch_quotes(
        &self,
        targets: Vec<(
            SwitchTarget,
            usize,
            String,
            Option<wisp_protocol::settings::Remembered>,
        )>,
    ) -> Vec<SwitchQuote> {
        let standing = self.standing_size();
        let mut quotes: Vec<SwitchQuote> = targets
            .into_iter()
            .filter_map(|(target, endpoint, model, remembered)| {
                let consequence = self.switch_quote(endpoint, &model);
                let retry_compaction = match (&target, standing) {
                    (SwitchTarget::Endpoint(_), Some((failed, size))) if endpoint != failed => {
                        self.retry_compaction(endpoint, size)
                    }
                    _ => None,
                };
                (consequence.is_some() || retry_compaction.is_some() || remembered.is_some())
                    .then_some(SwitchQuote {
                        remembered,
                        target,
                        consequence,
                        retry_compaction,
                    })
            })
            .collect();
        quotes.sort_by_key(|q| quote_key(&q.target));
        quotes
    }

    fn converted_fits(&self, endpoint: usize, model: &str, pending_tokens: u64) -> bool {
        self.converted_bound(endpoint, model, pending_tokens)
            .is_none_or(|(limit, estimate)| estimate <= limit)
    }

    fn converted_bound(
        &self,
        endpoint: usize,
        model: &str,
        pending_tokens: u64,
    ) -> Option<(u64, u64)> {
        let name = self.connection_name(endpoint);
        let max_tokens = self.max_tokens_on(endpoint, model);
        let resolved = self.models.resolve(
            name,
            model,
            self.listed_row(name, model).as_ref(),
            max_tokens,
        );
        let window = resolved.window?;
        let estimate = self
            .fixed_tokens()
            .saturating_add(self.converted_head_tokens())
            .saturating_add(pending_tokens);
        Some((window.tokens.saturating_sub(FIT_MARGIN_TOKENS), estimate))
    }

    pub(super) fn pre_turn_compaction(
        &mut self,
        turn: u64,
        target: (usize, String),
        pending_tokens: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = PreTurn>>> {
        Box::pin(self.pre_turn_compaction_unboxed(turn, target, pending_tokens, commands, events))
    }

    async fn pre_turn_compaction_unboxed(
        &mut self,
        turn: u64,
        target: (usize, String),
        pending_tokens: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> PreTurn {
        self.compaction.pre_turn = true;
        let ready = self
            .before_turn(turn, target, pending_tokens, commands, events)
            .await;
        self.compaction.pre_turn = false;
        ready
    }

    async fn before_turn(
        &mut self,
        turn: u64,
        (mut endpoint, mut model): (usize, String),
        pending_tokens: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> PreTurn {
        let from = SwitchFrom::Settled;
        let due = self.switch_due(endpoint, &model, (from, Some(pending_tokens)));
        let mut failed_kept = false;
        if let Some(due) = due.filter(|d| d.compacts()) {
            let run = self
                .switch_compaction(turn, (from, due.cause()), commands, events)
                .await;
            (endpoint, model) = self.next_endpoint_model();
            match run {
                SwitchRun::Compacted => {}
                SwitchRun::Cancelled => {
                    if self.bound_endpoint().is_some() {
                        self.settings.withdraw_after = Some(Withdrawal::SwitchCompactionCancelled);
                        return PreTurn::Cancelled;
                    }
                    if self.track().stopped {
                        return PreTurn::Cancelled;
                    }
                }
                SwitchRun::Failed => {
                    let fits = if self.head_holds_block() && !self.takes_block(endpoint, &model) {
                        self.converted_fits(endpoint, &model, pending_tokens)
                    } else {
                        due != SwitchDue::SmallerWindow
                            || !self.over_new_point(endpoint, &model, pending_tokens)
                    };
                    if !fits && self.bound_endpoint().is_some() {
                        self.withdraw_switch(Withdrawal::SwitchCompactionFailed, events)
                            .await;
                        return PreTurn::Ready;
                    }
                    failed_kept = due == SwitchDue::KeptTurns;
                }
            }
        }
        let leaves_kept = failed_kept && self.keeps_turns() && model != self.session.model;
        if self.head_holds_block() && (leaves_kept || !self.takes_block(endpoint, &model)) {
            self.close_compacted_tail(events).await;
            let target = (endpoint, model.as_str(), pending_tokens);
            self.convert_now(Some(turn), target, commands, events).await;
        }
        PreTurn::Ready
    }

    async fn switch_compaction(
        &mut self,
        turn: u64,
        (from, cause): (SwitchFrom, SwitchCompactionCause),
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> SwitchRun {
        let trigger = CompactionTrigger::Switch;
        let switch = SwitchCompaction {
            estimate_micros: self.compaction_estimate(),
            head_tokens: self.compaction_head_tokens(),
            cause,
        };
        let mut origin = Origin {
            trigger,
            turn: Some(turn),
            focus: None,
            switch: Some(switch.clone()),
            instructions_by: Vec::new(),
            requested_by: None,
        };
        let mut track = TurnTrack::new(turn);
        track.stopped = self.track.as_ref().is_some_and(|t| t.stopped);
        self.compaction.running = Some(Running { track });
        let started_event = AgentEvent::CompactionStarted {
            trigger,
            turn: Some(turn),
            switch: Some(switch.clone()),
            requested_by: None,
        };
        emit(events, started_event).await;
        let started = Instant::now();
        let retries = from == SwitchFrom::Settled;
        self.compaction.overflow.switching = true;
        let gate = self.gate(trigger);
        let result = self
            .summarise((gate, None), retries, Fit::View, commands, events)
            .await;
        self.compaction.overflow.switching = false;
        origin.instructions_by = std::mem::take(&mut self.compaction.instructions_by);
        let (result, texts) = self
            .head_step_for(result, (trigger, Fit::View), commands, events)
            .await;
        let stopped = self
            .compaction
            .running
            .as_ref()
            .is_some_and(|r| r.track.stopped);
        let spent = self.spent();
        self.add_compaction_cost(spent.cost);
        self.count_in_turn(&spent);
        if stopped && let Some(track) = self.track.as_mut() {
            track.stopped = true;
        }
        let written = match result {
            Outcome::Failed {
                engine,
                kind,
                message,
                ..
            } => {
                let cancelled = kind == CompactionFailure::Cancelled;
                let failure = FailedCompaction {
                    trigger,
                    turn: Some(turn),
                    engine: Some(engine),
                    kind,
                    message,
                    switch: Some(switch),
                    strategy: texts.chosen.clone(),
                    instruction: texts.instruction.clone(),
                    requested_by: None,
                    answers: None,
                };
                let event = self.compaction_failed(failure, &spent, events).await;
                emit(events, event).await;
                return if cancelled {
                    SwitchRun::Cancelled
                } else {
                    SwitchRun::Failed
                };
            }
            fresh => fresh.fresh(),
        };
        self.close_unkept_tail(events).await;
        let (engine, converted, (before, after, partial)) = match written {
            Some((engine, block, summary, before, partial)) => {
                let turns = self.next_turns();
                let install = Install {
                    plan_usd: spent.cost.and_then(|c| c.turn_plan_usd),
                    origin,
                    texts,
                    engine,
                    block,
                    summary,
                    turns,
                    covers: self.covers(turns),
                    before,
                    partial,
                    usage: spent.usage,
                    cost_usd: spent.usd,
                    list_usd: spent.list_usd,
                    model: spent.model.clone(),
                    lower_bound: spent.lower_bound,
                    started,
                };
                let after = self.install(install, events).await;
                (engine, false, (before, after, partial))
            }
            None => {
                let converted = self.convert(origin, (&spent, texts), started, events).await;
                (CompactionEngine::Wisp, true, converted)
            }
        };
        let event = AgentEvent::Compacted {
            trigger,
            engine,
            before,
            after,
            partial,
            converted,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: spent.usage,
            cost: spent.cost,
            answers: None,
            turn: Some(turn),
            summary: self.head_summary(),
            paused: false,
        };
        emit(events, event).await;
        emit(events, self.context_changed()).await;
        SwitchRun::Compacted
    }

    async fn send_compaction(
        &mut self,
        engine: CompactionEngine,
        instruction: &Instruction,
        retries: bool,
        earlier: Option<&Earlier>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Outcome {
        let native = engine == CompactionEngine::Native;
        let mut search_refused = false;
        let mut fell_back = Vec::new();
        'rebuild: loop {
            let view = self.compaction_view(earlier);
            let tail = self.compaction_tail(earlier, &view);
            let (messages, flags) = if native {
                let flags = closing_flags(self.paused_flags(view.len()), &view);
                (view, flags)
            } else {
                let fork = self.summary_fork(&view, &instruction.text);
                let flags = self.paused_flags(fork.len());
                (fork, flags)
            };
            let rendering = self.compaction_rendering(messages.len());
            if let Err(problem) = check_layout(
                &messages,
                &flags,
                &rendering.closed,
                self.provider().as_ref(),
            ) {
                return malformed(engine, &problem.problem);
            }
            let (mut session, mut features) = self.request_parts();
            if native {
                features.native_compaction =
                    self.feature_use(&self.session.model, Feature::NativeCompaction);
            }
            session.max_tokens = self.compaction_max_tokens();
            let ask = CompactionAsk {
                native_instructions: native.then(|| instruction.text.clone()),
                tail,
            };
            self.track().start_step();
            let mut sent_at;
            let streamed = loop {
                let Some(pass) = self.enter_endpoint_lane(0, commands, events).await else {
                    return cancelled(engine);
                };
                if self.bound_endpoint().is_none()
                    || self.check_resume_destination(self.endpoint).is_err()
                {
                    return cancelled(engine);
                }
                sent_at = self.clock.now();
                let streamed = self
                    .stream_compaction(
                        &messages, &session, features, &rendering, &ask, pass, commands, events,
                    )
                    .await;
                let Streamed::Failed(failure) = streamed else {
                    break streamed;
                };
                let rejected = self.falls_back(features, &failure.error.rejected, &fell_back);
                if !rejected.is_empty() {
                    fell_back.extend_from_slice(&rejected);
                    self.fall_back(&rejected, &mut None, events).await;
                    let lost_native = rejected.contains(&Feature::NativeCompaction);
                    if native && lost_native {
                        return if self.head_holds_block() {
                            Outcome::Convert
                        } else {
                            falling_back(failed(engine, &failure))
                        };
                    }
                    if lost_native && self.head_holds_block() {
                        return Outcome::Convert;
                    }
                    continue 'rebuild;
                }
                if failure.error.refused == Some(Refused::CompactionBlock) {
                    if self.head_holds_block() {
                        return Outcome::Convert;
                    }
                    if native {
                        return failed(engine, &failure);
                    }
                }
                if failure.error.refused == Some(Refused::NativeSearch)
                    && !search_refused
                    && !crate::search::sent(self.server_tools()).is_empty()
                {
                    search_refused = true;
                    self.mark_search_refused(events).await;
                    continue 'rebuild;
                }
                if failure.error.refused == Some(Refused::CrossAccount)
                    && self.recover_unreadable(events).await
                {
                    continue 'rebuild;
                }
                if failure.error.kind == ErrorKind::ThinkingBinding
                    && self
                        .recover_refused_thinking(&messages, &rendering, events)
                        .await
                        .is_some()
                {
                    continue 'rebuild;
                }
                let billed = failure.error.uncounted(failure.usage.is_some());
                if failure.error.kind == ErrorKind::ContextOverflow {
                    let usage = failure.usage.map(|u| summed(&u, &failure.iterations));
                    self.charge(usage, &failure.model, &failure.iterations, billed);
                    let limit = self
                        .rejected_limit(&failure, (earlier, instruction.tokens()), events)
                        .await;
                    return too_large(engine, &failure.error.reported_message(), limit);
                }
                if !retries {
                    let usage = failure.usage.map(|u| summed(&u, &failure.iterations));
                    self.charge(usage, &failure.model, &failure.iterations, billed);
                    return failed(engine, &failure);
                }
                let mode = if native {
                    retry::Mode::NativeCompaction
                } else {
                    retry::Mode::Request
                };
                match self
                    .after_failure(0, &failure, mode, commands, events)
                    .await
                {
                    AfterFailure::Retry => {}
                    AfterFailure::Cancelled | AfterFailure::Stopped => return cancelled(engine),
                    AfterFailure::GiveUp(_) => {
                        let usage = failure.usage.map(|u| summed(&u, &failure.iterations));
                        self.charge(usage, &failure.model, &failure.iterations, billed);
                        return failed(engine, &failure);
                    }
                }
            };
            let asked = (earlier, instruction);
            return if native {
                self.native_reply(streamed, &messages, features, asked, sent_at, events)
                    .await
            } else {
                self.wisp_reply(
                    streamed, &messages, features, asked, sent_at, commands, events,
                )
                .await
            };
        }
    }

    fn settle(
        &mut self,
        streamed: Streamed,
        engine: CompactionEngine,
    ) -> Result<AssembledMessage, Outcome> {
        match streamed {
            Streamed::Done(message) => Ok(message),
            Streamed::Cancelled {
                usage,
                model,
                iterations,
                in_flight,
            } => {
                let billed = usage.is_some() || in_flight;
                let usage = usage.map(|u| summed(&u, &iterations));
                self.charge(usage, &model, &iterations, billed);
                Err(cancelled(engine))
            }
            Streamed::Failed(failure) => {
                let billed = failure.error.uncounted(failure.usage.is_some());
                let usage = failure.usage.map(|u| summed(&u, &failure.iterations));
                self.charge(usage, &failure.model, &failure.iterations, billed);
                Err(failed(engine, &failure))
            }
            Streamed::Ended(end) => Err(no_summary(engine, end.stop_reason, false)),
        }
    }

    async fn native_reply(
        &mut self,
        streamed: Streamed,
        view: &[Message],
        features: Features,
        (earlier, instruction): (Option<&Earlier>, &Instruction),
        sent_at: jiff::Timestamp,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Outcome {
        let engine = CompactionEngine::Native;
        let message = match self.settle(streamed, engine) {
            Ok(message) => message,
            Err(outcome) => return outcome,
        };
        let usage = summed(&message.usage, &message.iterations);
        let expected = self.epoch.expected();
        self.check_cache_with(&message, &usage, expected, Some(sent_at), events)
            .await;
        self.charge(Some(usage), &message.model, &message.iterations, false);
        let before = used_of(&message.usage, &message.iterations);
        if message.stop_reason == StopReason::ContextWindowExceeded {
            let limit = self.stopped_limit(&message, engine, (earlier, instruction.tokens()));
            return too_large(engine, WINDOW_EXCEEDED, limit);
        }
        if message.stop_reason != StopReason::Compaction {
            let wrote_text = !summary_text(&message).trim().is_empty();
            let falls_back = native_falls_back(&message.stop_reason, wrote_text);
            return no_summary(engine, message.stop_reason.clone(), falls_back);
        }
        let blocks: Vec<&ContentBlock> = message
            .content
            .iter()
            .filter(|b| is_native_block(b))
            .collect();
        match blocks.as_slice() {
            [ContentBlock::Opaque { deltas, .. }] if !deltas.is_empty() => Outcome::Failed {
                engine,
                kind: CompactionFailure::Error(ErrorKind::InvalidResponse),
                message: "The compaction block arrived in pieces, so Wisp didn't keep it."
                    .to_owned(),
                falls_back: true,
                too_large: None,
            },
            [one] => {
                let block = (*one).clone();
                self.prove(features, view, events).await;
                Outcome::Native {
                    block,
                    before,
                    partial: false,
                }
            }
            more => Outcome::Failed {
                engine,
                kind: CompactionFailure::Error(ErrorKind::InvalidResponse),
                message: format!(
                    "The reply held {} compaction blocks; Wisp expects exactly one.",
                    more.len()
                ),
                falls_back: true,
                too_large: None,
            },
        }
    }

    fn compact_effort(&self) -> Effort {
        find(names::CONTEXT_COMPACT_EFFORT)
            .and_then(|spec| self.resolved_at(spec, &Place::Global))
            .and_then(|value| match value {
                SettingValue::Text(text) => text.parse().ok(),
                _ => None,
            })
            .unwrap_or(COMPACT_EFFORT)
    }

    pub(super) fn summary_fork(&self, view: &[Message], instruction: &str) -> Vec<Message> {
        let appended = Appended {
            user: vec![ContentBlock::Text {
                text: instruction.to_owned(),
            }],
            marker: self.summary_marker(view, self.compact_effort()),
        };
        fork_request(view, appended)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the accept call needs the commands receiver beside the reply's parts"
    )]
    async fn wisp_reply(
        &mut self,
        streamed: Streamed,
        fork: &[Message],
        features: Features,
        (earlier, instruction): (Option<&Earlier>, &Instruction),
        sent_at: jiff::Timestamp,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Outcome {
        let engine = CompactionEngine::Wisp;
        let message = match self.settle(streamed, engine) {
            Ok(message) => message,
            Err(outcome) => return outcome,
        };
        let usage = summed(&message.usage, &message.iterations);
        let expected = self.epoch.expected();
        self.check_cache_with(&message, &usage, expected, Some(sent_at), events)
            .await;
        self.charge(Some(usage), &message.model, &message.iterations, false);
        self.prove(features, fork, events).await;
        let before = used_of(&message.usage, &message.iterations);
        let summary = summary_text(&message);
        if message.stop_reason == StopReason::ContextWindowExceeded {
            let limit = self.stopped_limit(&message, engine, (earlier, instruction.tokens()));
            return too_large(engine, WINDOW_EXCEEDED, limit);
        }
        if message.stop_reason != StopReason::EndTurn || summary.trim().is_empty() {
            return no_summary(engine, message.stop_reason.clone(), false);
        }
        if !instruction.rewritten
            && let Some(message) = self.strategy_accept(&summary, commands, events).await
        {
            return check_failed(message);
        }
        let after = self.head_fill(&summary);
        if !self.live_strategy() && after >= before {
            return check_failed(not_smaller(after, before));
        }
        Outcome::Written {
            engine,
            summary,
            before,
            partial: false,
        }
    }

    fn head_fill(&self, summary: &str) -> u64 {
        let epoch = self.next_epoch();
        let texts = self.estimated_head_texts();
        let head = self.build_head(None, summary, (epoch, self.next_turns()), &texts);
        self.fixed_tokens()
            .saturating_add(blocks_tokens(&head.content))
    }

    pub(super) fn fixed_tokens(&self) -> u64 {
        self.categories(0, false)
            .iter()
            .filter(|c| {
                matches!(
                    c.category,
                    Category::SystemPrompt | Category::Tools | Category::Instructions
                )
            })
            .fold(0_u64, |sum, c| sum.saturating_add(c.tokens))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one compaction request's parts, its lane pass and the session's channels"
    )]
    async fn stream_compaction(
        &mut self,
        messages: &[Message],
        session: &SessionConfig,
        features: Features,
        rendering: &Rendering,
        ask: &CompactionAsk,
        pass: crate::lanes::Pass,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Streamed {
        self.commit_lost(events).await;
        let (wire_tools, server_tools) = self.wire(&self.sent, self.server_tools());
        let request = Request {
            session,
            tools: &wire_tools,
            deferred: super::tool_changes::deferred_for(&self.deferred, features),
            by_value: super::tool_changes::by_value_for(&self.deferred, features),
            server_tools,
            messages,
            previous_response_id: self
                .last
                .as_ref()
                .filter(|l| l.endpoint == self.endpoint)
                .and_then(|l| l.response_id.as_deref()),
            features,
            options: self.request_options(),
            rendering,
            compaction: Some(ask),
        };
        let print = self.prefix_print(&request);
        let sent = self.sent(&request, print.clone());
        let watch = self.watch();
        self.sent_sound = self.sent_sound(request.messages, request.rendering);
        let mut stream = self.provider().stream(&request);
        self.sending = Some(sent);
        let (mut warming, mut stagger) = self.hold_for_warmth(print, events).await;
        let mut asm = Assembler::default();
        let mut in_flight = false;
        let mut sent_at = None;
        let processes = self.processes.clone();
        let mailbox = self.mailbox.clone();
        let result = loop {
            tokio::select! {
                biased;
                command = commands.recv() => {
                    if self.on_command(command, events).await {
                        drop(stream);
                        drop(pass);
                        return Streamed::Cancelled {
                            usage: asm.usage(),
                            model: asm.model().to_owned(),
                            iterations: asm.iterations().to_vec(),
                            in_flight,
                        };
                    }
                },
                done = self.queue.done() => self.finish(done, events).await,
                () = processes.changed() => self.drain_processes(events).await,
                () = mailbox.arrived() => self.accept_mail(events).await,
                ticket = super::staggering(&mut stagger), if stagger.is_some() => {
                    stagger = None;
                    warming = Some(ticket);
                    self.waiting_as(None, events).await;
                },
                item = stream.next(), if stagger.is_none() => match item {
                    Some(Ok(event)) => {
                        if let ProviderEvent::PlanUsage(observation) = event {
                            watch.observe(observation);
                            continue;
                        }
                        if matches!(event, ProviderEvent::Sending) {
                            sent_at = watch.sending(&session.model);
                            in_flight = true;
                        } else {
                            if let Some(ticket) = warming.as_mut() {
                                ticket.streamed();
                            }
                            let track = self.track();
                            if track.first_byte_ms.is_none() {
                                track.first_byte_ms = Some(track.elapsed_ms());
                            }
                        }
                        if matches!(
                            event,
                            ProviderEvent::Unknown { .. }
                                | ProviderEvent::InputTransformations { .. }
                        ) && let Some(out) = translate(&event, &self.toolbox, &asm)
                        {
                            self.emit(events, out).await;
                        }
                        if let Err(e) = asm.apply(event) {
                            break Err(e);
                        }
                    }
                    Some(Err(e)) => {
                        watch.failed(&e);
                        break Err(e);
                    }
                    None => break Ok(()),
                },
            }
        };
        drop(stream);
        drop(pass);
        self.refresh_plan_usage();
        let usage = asm.usage();
        let model = asm.model().to_owned();
        let iterations = asm.iterations().to_vec();
        let discarded = asm.discarded();
        match result.and_then(|()| asm.finish()) {
            Ok(message) => {
                watch.succeeded(&session.model, sent_at);
                Streamed::Done(message)
            }
            Err(error) => Streamed::Failed(Box::new(Failure {
                error,
                usage,
                model,
                iterations,
                discarded,
                request_tokens: retry::request_tokens(
                    session.system.as_deref(),
                    session.instructions.as_deref(),
                    &wire_tools,
                    super::tool_changes::deferred_for(&self.deferred, features),
                    server_tools,
                    messages,
                ),
                attempts: 0,
                gave_up: None,
                searches: 0,
            })),
        }
    }
}

fn quote_key(target: &SwitchTarget) -> (u8, String) {
    match target {
        SwitchTarget::Model(model) => (0, model.clone()),
        SwitchTarget::Endpoint(endpoint) => (1, endpoint.clone()),
        SwitchTarget::Pair { endpoint, model } => (2, format!("{endpoint}\n{model}")),
        SwitchTarget::Other(value) => (3, value.to_string()),
    }
}

impl TurnTrack {
    fn absorb(&mut self, spent: &Spent) {
        if let Some(usage) = spent.usage {
            self.usage = Some(match &self.usage {
                Some(sum) => add_usage(sum, &usage),
                None => usage,
            });
        }
        if let Some(usd) = spent.usd {
            self.usd = Some(self.usd.map_or(usd, |sum| sum + usd));
        }
        self.plan_usd += spent.cost.and_then(|c| c.turn_plan_usd).unwrap_or(0.0);
        self.plan_unknown |= spent.cost.is_some_and(|c| {
            c.turn_billing == wisp_protocol::Billing::Mixed && c.turn_plan_usd.is_none()
        });
        self.lower_bound |= spent.lower_bound;
    }
}

#[cfg(test)]
pub(in crate::agent) mod tests;
