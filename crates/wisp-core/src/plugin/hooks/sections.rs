use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::prompt::{At as PromptAt, Prior, Rendered};
use wisp_protocol::HookOutcome;

use super::engine::{Engine, Ran};
use super::points::{BEFORE_TURN, MAX_APPEND_CHARS};
use super::resolve::Name;
use crate::plugin::caller::{At, Caller, Render};
use crate::plugin::panicked;

pub const SECTION_TIMEOUT: Duration = Duration::from_secs(2);

pub const START_TIMEOUT: Duration = Duration::from_secs(10);

pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;

pub const START_POINT: &str = "section-start";

pub const HEAD_POINT: &str = "section-head";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionCall {
    pub at: PromptAt,
    pub previous: Prior,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Limits {
    pub cancel: CancellationToken,
    pub deadline: Option<Instant>,
    pub at: At,
}

pub fn point_of(at: PromptAt) -> &'static str {
    match at {
        PromptAt::Start => START_POINT,
        PromptAt::Turn => BEFORE_TURN,
        PromptAt::Head => HEAD_POINT,
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn checked(rendered: Rendered, bundled: bool) -> Result<Rendered, HookOutcome> {
    let rejected = |rule: &str| HookOutcome::Rejected {
        rule: rule.to_owned(),
    };
    if rendered.snapshot.len() > MAX_SNAPSHOT_BYTES {
        return Err(rejected("snapshot_too_large"));
    }
    let long = rendered
        .text
        .as_ref()
        .is_some_and(|text| text.chars().count() > MAX_APPEND_CHARS);
    if long && !bundled {
        return Err(rejected("too_large"));
    }
    Ok(rendered)
}

impl Engine {
    pub fn section_runnable(&self, name: &Name) -> bool {
        self.table().section(name).is_some() && !self.is_off(name) && self.has_plugin(&name.plugin)
    }

    pub fn render_section(
        self: &Arc<Self>,
        name: &Name,
        call: SectionCall,
        limits: Limits,
    ) -> BoxFuture<'static, (Option<Rendered>, Ran)> {
        let spec = self.table().section(name);
        let plugin = self.plugin(&name.plugin);
        let full = name.to_string();
        let point = point_of(call.at);
        let started = Instant::now();
        let caller = match call.at {
            PromptAt::Start => Caller::rendering(Render::Start),
            PromptAt::Turn | PromptAt::Head => {
                Caller::call(limits.cancel.clone(), limits.deadline).hook(1, limits.at.clone())
            }
        };
        let at = limits.at.clone();
        let short = name.hook.clone();
        let id = name.plugin.clone();
        caller
            .scope(async move {
                let ran = |outcome: HookOutcome| Ran {
                    hook: full.clone(),
                    point: point.to_owned(),
                    at: at.clone(),
                    outcome,
                    ms: millis(started.elapsed()),
                };
                let (Some(spec), Some(plugin)) = (spec, plugin) else {
                    return (None, ran(HookOutcome::None));
                };
                let target = call.target;
                let called = contain(|| {
                    plugin
                        .render_section(&short, call.at, call.previous, target.as_deref())
                        .map(Ok::<Rendered, ()>)
                        .boxed()
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
                let done = match limits.deadline {
                    Some(deadline) => tokio::select! {
                        biased;
                        done = contained => Some(done),
                        () = tokio::time::sleep_until(deadline) => None,
                        () = limits.cancel.cancelled() => {
                            return (None, ran(HookOutcome::Cancelled));
                        }
                    },
                    None => tokio::select! {
                        biased;
                        done = contained => Some(done),
                        () = limits.cancel.cancelled() => {
                            return (None, ran(HookOutcome::Cancelled));
                        }
                    },
                };
                let rendered = match done {
                    None => return (None, ran(HookOutcome::TimedOut)),
                    Some(Err(_) | Ok(Err(()))) => {
                        return (
                            None,
                            ran(HookOutcome::Failed {
                                message: panicked(&id),
                            }),
                        );
                    }
                    Some(Ok(Ok(rendered))) => rendered,
                };
                match checked(rendered, spec.bundled) {
                    Ok(rendered) => {
                        let outcome = if rendered.text.is_some() {
                            HookOutcome::Applied
                        } else {
                            HookOutcome::None
                        };
                        (Some(rendered), ran(outcome))
                    }
                    Err(outcome) => (None, ran(outcome)),
                }
            })
            .boxed()
    }
}
