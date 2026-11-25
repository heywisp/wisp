use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use wisp_media::Tier;
use wisp_protocol::log::{LogEvent, LoggedInput};
use wisp_protocol::{
    AgentEvent, BusyWait, ClientCommand, ContentBlock, ErrorDetail, ErrorKind, GaveUp, InputId,
    Message, QueueReason, RetryClass, RetryFocus, RetryOffer, Usage,
};

use super::{Agent, Failure, emit};
use crate::pricing;
use crate::provider::{ProviderError, ServerToolSpec, classify};
use crate::tool::ToolSpec;

pub const BASE_DELAY: Duration = Duration::from_secs(1);
pub const MAX_DELAY: Duration = Duration::from_secs(300);
pub const LIMIT_WAIT: Duration = Duration::from_secs(5 * 60 * 60);
pub const JITTER: f64 = 0.2;
pub const INVALID_RESPONSE_RETRIES: u32 = 3;
pub const MALFORMED_CALL_NOTE: &str = "Your previous response contained an improperly formatted function call. Send the function call again, properly formatted.";
pub(crate) const CALL_NOTE_ID_PREFIX: &str = "wisp-call-note-";

pub(crate) fn is_call_note_id(id: &InputId) -> bool {
    id.as_str().starts_with(CALL_NOTE_ID_PREFIX)
}

pub(super) fn call_note_input(turn: u64, step: u32) -> LoggedInput {
    LoggedInput {
        id: InputId(format!("{CALL_NOTE_ID_PREFIX}{turn}-{step}")),
        text: MALFORMED_CALL_NOTE.to_owned(),
        images: Vec::new(),
        note: None,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub step_retries: Option<u32>,
    pub turn_retries: Option<u32>,
    pub budget_usd: Option<f64>,
    pub base_delay: Duration,
    pub jitter: fn() -> f64,
    pub busy_window: Option<Duration>,
    pub max_retry_after: Option<Duration>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            step_retries: None,
            turn_retries: None,
            budget_usd: None,
            base_delay: BASE_DELAY,
            jitter: clock_jitter,
            busy_window: None,
            max_retry_after: None,
        }
    }
}

impl RetryPolicy {
    pub(crate) fn for_fallback(self) -> Self {
        Self {
            step_retries: self.step_retries.or(Some(5)),
            turn_retries: self.turn_retries.or(Some(10)),
            max_retry_after: self.max_retry_after.or(Some(Duration::from_secs(60))),
            busy_window: self.busy_window.or(Some(Duration::from_secs(600))),
            ..self
        }
    }

    pub(crate) fn for_kind(self, error: &ProviderError) -> Self {
        if error.kind == ErrorKind::InvalidResponse {
            Self {
                step_retries: Some(self.step_retries.map_or(INVALID_RESPONSE_RETRIES, |max| {
                    max.min(INVALID_RESPONSE_RETRIES)
                })),
                ..self
            }
        } else {
            self
        }
    }

    #[must_use]
    pub fn with_budget(mut self, budget_usd: Option<f64>) -> Self {
        self.budget_usd = budget_usd.filter(|b| b.is_finite()).map(|b| b.max(0.0));
        self
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn quick() -> RetryPolicy {
    RetryPolicy {
        base_delay: Duration::from_millis(1),
        jitter: || 0.0,
        busy_window: Some(Duration::ZERO),
        ..RetryPolicy::default().for_fallback()
    }
}

pub fn clock_jitter() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos % 1000) / 1000.0
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RetryState {
    pub step: u32,
    pub turn: u32,
    pub tries: u32,
    pub busy_waited: Duration,
    pub spent_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    Retry { delay: Duration },
    GiveUp(GaveUp),
}

pub fn decide(
    policy: &RetryPolicy,
    state: &RetryState,
    detail: &ErrorDetail,
    busy: bool,
    next_estimate_usd: Option<f64>,
    child: bool,
) -> Decision {
    let after = match detail.retry {
        RetryClass::Never => return Decision::GiveUp(GaveUp::NotRetryable),
        RetryClass::Transient => None,
        RetryClass::After { ms } => Some(Duration::from_millis(ms)),
    };
    let limit_wait = if child { MAX_DELAY } else { LIMIT_WAIT };
    if detail.limit_reached.is_some() && after.is_some_and(|after| after > limit_wait) {
        return Decision::GiveUp(GaveUp::RetryAfterTooLong);
    }
    if busy {
        return wait_while_busy(policy, state, after);
    }
    if after
        .zip(policy.max_retry_after)
        .is_some_and(|(a, max)| a > max)
    {
        return Decision::GiveUp(GaveUp::RetryAfterTooLong);
    }
    if policy.step_retries.is_some_and(|max| state.step >= max) {
        return Decision::GiveUp(GaveUp::StepLimit);
    }
    if policy.turn_retries.is_some_and(|max| state.turn >= max) {
        return Decision::GiveUp(GaveUp::TurnLimit);
    }
    if let (Some(budget), Some(next)) = (policy.budget_usd, next_estimate_usd)
        && detail.possibly_billed
        && state.spent_usd + next > budget
    {
        return Decision::GiveUp(GaveUp::Budget);
    }
    Decision::Retry {
        delay: backoff(policy, state.tries.saturating_add(1), after),
    }
}

fn wait_while_busy(policy: &RetryPolicy, state: &RetryState, after: Option<Duration>) -> Decision {
    let Some(window) = policy.busy_window else {
        return Decision::Retry {
            delay: backoff(policy, state.tries.saturating_add(1), after),
        };
    };
    if after.is_some_and(|a| a > window) {
        return Decision::GiveUp(GaveUp::RetryAfterTooLong);
    }
    let left = window.saturating_sub(state.busy_waited);
    if left.is_zero() || after.is_some_and(|a| a > left) {
        return Decision::GiveUp(GaveUp::BusyWindow);
    }
    Decision::Retry {
        delay: backoff(policy, state.tries.saturating_add(1), after).min(left),
    }
}

pub fn is_busy(policy: &RetryPolicy, error: &ProviderError, billed: bool) -> bool {
    policy.busy_window != Some(Duration::ZERO)
        && classify::load_refusal(error.kind)
        && error.detail.retry != RetryClass::Never
        && error.refused.is_none()
        && !billed
}

pub fn backoff(policy: &RetryPolicy, retry: u32, after: Option<Duration>) -> Duration {
    let doubling = 1u32 << retry.saturating_sub(1).min(16);
    let base = policy.base_delay.saturating_mul(doubling).min(MAX_DELAY);
    let jitter = (policy.jitter)();
    let jitter = if jitter.is_finite() {
        jitter.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let computed = base.saturating_sub(base.mul_f64(JITTER * jitter));
    match after {
        Some(after) if after > computed => after,
        _ => computed,
    }
}

pub fn request_tokens(
    system: Option<&str>,
    instructions: Option<&str>,
    tools: &[ToolSpec],
    deferred: &[ToolSpec],
    server_tools: &[ServerToolSpec],
    messages: &[Message],
) -> u64 {
    let system = system.map_or(0, |s| s.len() as u64);
    let system = system.saturating_add(instructions.map_or(0, |s| s.len() as u64));
    let tools = crate::tool::array_json_len(tools, deferred, server_tools) as u64;
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .map(block_tokens)
        .fold(system.saturating_add(tools), u64::saturating_add)
}

fn block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Image { .. } | ContentBlock::Media { .. } => u64::from(Tier::HIGH.max_tokens),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } => content
            .iter()
            .map(block_tokens)
            .fold(tool_use_id.len() as u64, u64::saturating_add),
        other => serde_json::to_string(other).map_or(0, |b| b.len() as u64),
    }
}

pub fn first_request_usage(request_tokens: u64) -> Usage {
    Usage {
        cache_creation_input_tokens: request_tokens,
        cache_creation_1h_input_tokens: Some(request_tokens),
        ..Usage::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Endpoints {
    pub others: bool,
    pub suggest: Option<String>,
}

pub(super) fn rule(
    kind: ErrorKind,
    detail: Option<&ErrorDetail>,
    gave_up: Option<GaveUp>,
) -> Option<(bool, bool, RetryFocus)> {
    Some(match kind {
        ErrorKind::InvalidResponse
            if detail.is_some_and(|d| d.malformed_call.is_some())
                && gave_up == Some(GaveUp::StepLimit) =>
        {
            (true, true, RetryFocus::Switch)
        }
        ErrorKind::Server
        | ErrorKind::Overloaded
        | ErrorKind::Network
        | ErrorKind::Timeout
        | ErrorKind::InvalidResponse
        | ErrorKind::Interrupted
        | ErrorKind::Other => (true, true, RetryFocus::Retry),
        ErrorKind::RateLimit if gave_up == Some(GaveUp::RetryAfterTooLong) => {
            (true, true, RetryFocus::Switch)
        }
        ErrorKind::RateLimit => (true, true, RetryFocus::Retry),
        ErrorKind::Billing => (true, true, RetryFocus::Switch),
        ErrorKind::Auth => (true, true, RetryFocus::Retry),
        ErrorKind::ThinkingBinding => (false, true, RetryFocus::Switch),
        ErrorKind::InvalidRequest if detail.is_some() => (false, true, RetryFocus::Switch),
        ErrorKind::InvalidRequest | ErrorKind::ContextOverflow | ErrorKind::Refusal => {
            return None;
        }
    })
}

pub fn offer(
    kind: ErrorKind,
    detail: Option<&ErrorDetail>,
    gave_up: Option<GaveUp>,
    endpoints: &Endpoints,
) -> Option<RetryOffer> {
    let (retry, switch, focus) = rule(kind, detail, gave_up)?;
    let switch = switch && endpoints.others;
    if !retry && !switch {
        return None;
    }
    let focus = match focus {
        RetryFocus::Retry if !retry => RetryFocus::Switch,
        RetryFocus::Switch if !switch => RetryFocus::Retry,
        kept => kept,
    };
    let suggest = (switch && kind == ErrorKind::ThinkingBinding)
        .then(|| endpoints.suggest.clone())
        .flatten();
    Some(RetryOffer {
        retry,
        switch,
        focus,
        suggest,
        retries_at: None,
    })
}

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Request,
    NativeCompaction,
}

pub(super) enum AfterFailure {
    Retry,
    Cancelled,
    Stopped,
    GiveUp(GaveUp),
}

pub(super) fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn retry_reason(error: &ProviderError) -> String {
    let message = error.reported_message();
    match error.detail.status {
        Some(status) if status != 200 => format!("HTTP {status}: {message}"),
        _ => message,
    }
}

impl Agent {
    fn retry_model<'a>(&'a self, served: &'a str) -> &'a str {
        if served.is_empty() {
            self.session.model.as_str()
        } else {
            served
        }
    }

    fn retry_estimate(
        &self,
        served: &str,
        usage: Option<Usage>,
        request_tokens: u64,
    ) -> Option<f64> {
        let input = match (usage, &self.last) {
            (Some(u), _) => Usage {
                output_tokens: 0,
                ..u
            },
            (None, Some(last)) => Usage {
                cache_read_input_tokens: last.prompt_tokens,
                ..Usage::default()
            },
            (None, None) => first_request_usage(request_tokens),
        };
        self.estimate(self.retry_model(served), &input, &[])
    }

    pub(super) fn give_up_message(
        &self,
        message: String,
        detail: &ErrorDetail,
        gave_up: Option<GaveUp>,
        served: &str,
    ) -> String {
        let applies = self.retry.budget_usd.is_some()
            && detail.possibly_billed
            && !matches!(gave_up, None | Some(GaveUp::NotRetryable));
        match self.unpriced_because(served).filter(|_| applies) {
            Some(why) => format!("{message} (the retry budget couldn't be checked: {why})"),
            None => message,
        }
    }

    fn unpriced_because(&self, served: &str) -> Option<String> {
        let model = self.retry_model(served);
        if !self.priced() {
            Some(format!(
                "account {} has pricing turned off",
                self.endpoint_name()
            ))
        } else if self.rates(self.endpoint, model).is_none() {
            Some(format!(
                "Wisp has no price for {}",
                wisp_protocol::shown_model(model)
            ))
        } else {
            None
        }
    }

    pub(super) fn after_failure(
        &mut self,
        step: u32,
        failure: &Failure,
        mode: Mode,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = AfterFailure>>> {
        Box::pin(self.after_failure_unboxed(step, failure, mode, commands, events))
    }

    async fn after_failure_unboxed(
        &mut self,
        step: u32,
        failure: &Failure,
        mode: Mode,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> AfterFailure {
        let policy = match mode {
            Mode::Request => self.retry,
            Mode::NativeCompaction => self.retry.for_fallback(),
        };
        let error = &failure.error;
        let billed = error.maybe_billed(failure.usage.is_some());
        let search_usd = if self.priced() {
            let carried = failure.usage.map_or(0, |u| u.web_search_requests);
            pricing::web_search_usd(
                self.provider_kind(),
                failure.searches.saturating_sub(carried),
            )
        } else {
            0.0
        };
        let base = self.retry_estimate(&failure.model, failure.usage, failure.request_tokens);
        let own_cost = failure
            .usage
            .and_then(|u| self.estimate(&failure.model, &u, &failure.iterations));
        let searched = if billed { search_usd } else { 0.0 };
        let spent = own_cost
            .or(base.filter(|_| error.possibly_billed))
            .unwrap_or(0.0)
            + searched;
        let estimate = base.map(|e| e + search_usd);
        let policy =
            if policy.budget_usd.is_some() && error.possibly_billed && estimate.is_none() {
                policy.for_fallback()
            } else {
                policy
            }
            .for_kind(error);
        let counted = !self.provider().busy_waits_5xx()
            && error.detail.status.is_some_and(classify::status_billed);
        let may_wait =
            !matches!(mode, Mode::NativeCompaction) || error.kind != ErrorKind::Overloaded;
        let busy = may_wait && !counted && is_busy(&policy, error, billed);
        let track = self.track();
        let state = RetryState {
            step: track.step_retries.saturating_sub(track.step_busy),
            turn: track.retries.saturating_sub(track.turn_busy),
            tries: track.step_retries,
            busy_waited: track.busy_waited,
            spent_usd: track.retry_spent + spent,
        };
        let delay = match decide(
            &policy,
            &state,
            &error.detail,
            busy,
            estimate,
            self.is_child(),
        ) {
            Decision::Retry { delay } => delay,
            Decision::GiveUp(reason) => return AfterFailure::GiveUp(reason),
        };
        let charged = if self.compacting() {
            failure
                .usage
                .map(|u| super::compact::summed(&u, &failure.iterations))
        } else {
            failure.usage
        };
        let spend = self.charge(
            charged,
            &failure.model,
            &failure.iterations,
            error.uncounted(failure.usage.is_some()),
        );
        let (cost_usd, lower_bound) = (spend.usd, spend.lower_bound);
        let model = Some(failure.model.clone()).filter(|m| !m.is_empty());
        let turn = self.running_turn();
        let logged_step = turn.filter(|_| !self.compacting()).map(|_| step);
        let window = policy.busy_window;
        let step_limit = policy.step_retries.unwrap_or(0);
        let tell_after = self.agents.wake_when_stuck().filter(|_| self.is_child());
        let track = self.track();
        if track.step_retries == 0 {
            track.retry_due = tell_after.map(|delay| Box::new(tokio::time::Instant::now() + delay));
        }
        track.retry_spent += spent;
        track.retries = track.retries.saturating_add(1);
        track.step_retries = track.step_retries.saturating_add(1);
        track.partial_output.clear();
        let busy = busy.then(|| {
            let waited = track.busy_waited;
            track.step_busy = track.step_busy.saturating_add(1);
            track.turn_busy = track.turn_busy.saturating_add(1);
            track.busy_waited = track.busy_waited.saturating_add(delay);
            BusyWait {
                waited_ms: millis(waited),
                window_ms: window.map_or(0, millis),
            }
        });
        let max_attempts = if track.step_busy > 0 { 0 } else { step_limit };
        let attempt = track.step_retries;
        let delay_ms = millis(delay);
        let reason = retry_reason(error);
        let record = LogEvent::Retrying {
            turn,
            attempt,
            delay_ms,
            reason: reason.clone(),
            step: logged_step,
            kind: Some(error.kind),
            detail: Some((*error.detail).clone()),
            usage: charged,
            partial_output: Some(failure.discarded.text.clone()).filter(|t| !t.is_empty()),
            model: model.clone(),
            cost_usd,
            list_usd: spend.list_usd,
            lower_bound,
            resend: None,
            busy,
        };
        self.log(events, record).await;
        let event = AgentEvent::Retrying {
            attempt,
            delay_ms,
            reason,
            step,
            kind: error.kind,
            detail: Some((*error.detail).clone()),
            max_attempts,
            discarded_blocks: failure.discarded.blocks,
            discarded_chars: failure.discarded.chars,
            usage: charged,
            model,
            cost_usd,
            lower_bound,
            resend: None,
            busy,
        };
        emit(events, event).await;
        self.wait_to_retry(delay, commands, events).await
    }

    fn tell_retrying(&mut self) {
        self.track().retry_due = None;
        if let Some(delay) = self.agents.wake_when_stuck() {
            let text = crate::agents::retrying_text(&self.path(), delay.as_secs() / 60);
            self.agents
                .mail_parent(wisp_protocol::MailKind::Notice, true, text);
        }
    }

    async fn wait_to_retry(
        &mut self,
        delay: Duration,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> AfterFailure {
        let deadline = tokio::time::Instant::now() + delay;
        let processes = self.processes.clone();
        let mailbox = self.mailbox.clone();
        self.waiting_as(Some(QueueReason::Retry), events).await;
        loop {
            let tell_at = self.track().retry_due.as_deref().copied();
            tokio::select! {
                biased;
                command = commands.recv() => {
                    let stopped = matches!(command, Some(ClientCommand::Cancel));
                    if self.on_command(command, events).await {
                        return if stopped && !self.closed && !self.is_child() {
                            AfterFailure::Stopped
                        } else {
                            AfterFailure::Cancelled
                        };
                    }
                }
                done = self.queue.done() => self.finish(done, events).await,
                () = processes.changed() => self.drain_processes(events).await,
                () = mailbox.arrived() => self.accept_mail(events).await,
                () = tokio::time::sleep_until(tell_at.unwrap_or_else(tokio::time::Instant::now)),
                    if tell_at.is_some() => self.tell_retrying(),
                () = tokio::time::sleep_until(deadline) => {
                    self.waiting_as(None, events).await;
                    return AfterFailure::Retry;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
