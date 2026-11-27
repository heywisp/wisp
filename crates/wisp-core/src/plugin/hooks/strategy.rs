use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::time::Instant;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::strategy::{CallEntry, SaidEntry, StrategyInfo, View};
use wisp_protocol::HookOutcome;

use super::engine::{Engine, Ran};
use super::sections::Limits;
use crate::plugin::caller::Caller;
use crate::plugin::panicked;

pub(crate) const STRATEGY_REFUSED: &str =
    " strategy refused the summary, so Wisp kept the conversation as it was: ";

pub const STRATEGY_TIMEOUT: Duration = Duration::from_secs(2);

pub const MAX_STRATEGY_BYTES: usize = 64 * 1024;

pub const STRATEGY_NODE: &str = "strategy";

pub const INSTRUCTION_POINT: &str = "strategy-instruction";

pub const ACCEPT_POINT: &str = "strategy-accept";

pub const HEAD_POINT: &str = "strategy-head";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryView {
    pub said: Vec<SaidEntry>,
    pub calls: Vec<CallEntry>,
    pub log_pointer: Option<String>,
    pub output_pointer: Option<String>,
}

impl View for HistoryView {
    fn said(&self) -> Vec<SaidEntry> {
        self.said.clone()
    }

    fn calls(&self) -> Vec<CallEntry> {
        self.calls.clone()
    }

    fn log_pointer(&self) -> Option<String> {
        self.log_pointer.clone()
    }

    fn output_pointer(&self) -> Option<String> {
        self.output_pointer.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    Instruction,
    Accept(String),
    Head(HistoryView, Option<String>),
}

impl Ask {
    pub fn point(&self) -> &'static str {
        match self {
            Self::Instruction => INSTRUCTION_POINT,
            Self::Accept(_) => ACCEPT_POINT,
            Self::Head(..) => HEAD_POINT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Instruction(Option<String>),
    Accept(Result<(), String>),
    Head(Vec<String>),
}

impl Answer {
    fn bytes(&self) -> usize {
        match self {
            Self::Instruction(text) => text.as_ref().map_or(0, String::len),
            Self::Accept(Ok(())) => 0,
            Self::Accept(Err(sentence)) => sentence.len(),
            Self::Head(texts) => texts.iter().map(String::len).sum(),
        }
    }

    fn outcome(&self) -> HookOutcome {
        match self {
            Self::Instruction(None) | Self::Accept(Ok(())) => HookOutcome::None,
            Self::Head(texts) if texts.is_empty() => HookOutcome::None,
            Self::Accept(Err(sentence)) => HookOutcome::Blocked {
                reason: sentence.clone(),
            },
            Self::Instruction(Some(_)) | Self::Head(_) => HookOutcome::Applied,
        }
    }
}

pub fn strategy_hook(plugin: &str) -> String {
    format!("{plugin}/{STRATEGY_NODE}")
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl Engine {
    pub fn call_strategy(
        self: &Arc<Self>,
        id: &str,
        info: StrategyInfo,
        ask: Ask,
        limits: Limits,
    ) -> BoxFuture<'static, (Option<Answer>, Ran)> {
        let plugin = self.plugin(id);
        let hook = strategy_hook(id);
        let point = ask.point();
        let started = Instant::now();
        let caller = Caller::call(limits.cancel.clone(), limits.deadline)
            .hook(1, limits.at.clone())
            .quiet_jobs();
        let at = limits.at.clone();
        let id = id.to_owned();
        caller
            .scope(async move {
                let ran = |outcome: HookOutcome| Ran {
                    hook: hook.clone(),
                    point: point.to_owned(),
                    at: at.clone(),
                    outcome,
                    ms: millis(started.elapsed()),
                };
                let Some(plugin) = plugin else {
                    return (
                        None,
                        ran(HookOutcome::Failed {
                            message: format!("the {id} plugin isn't installed"),
                        }),
                    );
                };
                let (view, summary, sentence) = match ask {
                    Ask::Instruction => (None, None, None),
                    Ask::Accept(summary) => (None, None, Some(summary)),
                    Ask::Head(view, summary) => (Some(view), summary, None),
                };
                let called = contain(|| match (&view, sentence) {
                    (Some(view), _) => plugin
                        .head(info, view, summary)
                        .map(|texts| Ok::<Answer, ()>(Answer::Head(texts)))
                        .boxed(),
                    (None, Some(summary)) => plugin
                        .accept(info, summary)
                        .map(|verdict| Ok::<Answer, ()>(Answer::Accept(verdict)))
                        .boxed(),
                    (None, None) => plugin
                        .instruction(info)
                        .map(|text| Ok::<Answer, ()>(Answer::Instruction(text)))
                        .boxed(),
                });
                let Ok(future) = called else {
                    return (
                        None,
                        ran(HookOutcome::Failed {
                            message: panicked(&id),
                        }),
                    );
                };
                let contained = Contained::new(future);
                let deadline = limits
                    .deadline
                    .unwrap_or_else(|| Instant::now() + STRATEGY_TIMEOUT);
                let done = tokio::select! {
                    biased;
                    done = contained => done,
                    () = tokio::time::sleep_until(deadline) => {
                        return (None, ran(HookOutcome::TimedOut));
                    }
                    () = limits.cancel.cancelled() => {
                        return (None, ran(HookOutcome::Cancelled));
                    }
                };
                let answer = match done {
                    Ok(Ok(answer)) => answer,
                    Err(_) | Ok(Err(())) => {
                        return (
                            None,
                            ran(HookOutcome::Failed {
                                message: panicked(&id),
                            }),
                        );
                    }
                };
                if answer.bytes() > MAX_STRATEGY_BYTES {
                    return (
                        None,
                        ran(HookOutcome::Rejected {
                            rule: "too_large".to_owned(),
                        }),
                    );
                }
                let outcome = answer.outcome();
                (Some(answer), ran(outcome))
            })
            .boxed()
    }
}
