use std::collections::BTreeSet;

use jiff::{SignedDuration, Timestamp};
use wisp_protocol::{HistoryChange, MissCause, Usage};

use crate::provider::{Iteration, ThinkingDrop, WirePrint};
use crate::warmth::{Component, Fingerprint};

pub(super) const LOOKBACK_BLOCKS: usize = 20;

#[derive(Debug, Clone)]
pub(super) struct Sent {
    pub(super) fingerprint: Option<Fingerprint>,
    pub(super) wire: Option<WirePrint>,
}

#[derive(Debug, Clone)]
pub(super) struct Print {
    pub(super) sent: Sent,
    pub(super) drops: Vec<ThinkingDrop>,
}

pub(super) struct Before<'a> {
    pub(super) print: Option<&'a Print>,
    pub(super) started_at: Option<Timestamp>,
    pub(super) life: Option<SignedDuration>,
    pub(super) switch: Option<Component>,
}

pub(super) fn first_pass(usage: &Usage, iterations: &[Iteration]) -> u64 {
    iterations
        .first()
        .map_or_else(|| usage.prompt_tokens(), |i| i.usage.prompt_tokens())
}

pub(super) fn cause(
    expected: Option<&str>,
    before: &Before<'_>,
    now: &Sent,
    at: Option<Timestamp>,
    drops: &[ThinkingDrop],
) -> MissCause {
    if let Some(reason) = expected {
        return MissCause::Expected {
            reason: reason.to_owned(),
        };
    }
    if let Some(changed) = before.switch {
        return component(changed);
    }
    let print = before.print;
    let fingerprints = print
        .and_then(|p| p.sent.fingerprint.as_ref())
        .zip(now.fingerprint.as_ref());
    let compared = fingerprints.is_some();
    if let Some(changed) = fingerprints.and_then(|(was, is)| is.changed(was).first().copied()) {
        return component(changed);
    }
    let wire = print
        .and_then(|p| p.sent.wire.as_ref().map(|w| (p, w)))
        .zip(now.wire.as_ref());
    let compared = compared
        && wire
            .is_some_and(|((_, was), is)| is.blocks.saturating_sub(was.blocks) <= LOOKBACK_BLOCKS);
    if let Some(cause) = wire.and_then(|((print, was), is)| wire_change(print, was, is, drops)) {
        return cause;
    }
    let (Some(started_at), Some(life), Some(at)) = (before.started_at, before.life, at) else {
        return MissCause::Unexplained;
    };
    let gap = at.duration_since(started_at);
    if gap >= life {
        return MissCause::TtlExpired {
            gap_s: u64::try_from(gap.as_secs()).unwrap_or(0),
        };
    }
    if compared {
        MissCause::RemoteLoss
    } else {
        MissCause::Unexplained
    }
}

fn component(changed: Component) -> MissCause {
    match changed {
        Component::Endpoint => MissCause::EndpointChanged,
        Component::Model => MissCause::ModelChanged,
        Component::Effort => MissCause::EffortChanged,
        Component::Thinking => MissCause::ThinkingChanged,
        Component::Tools => MissCause::ToolsChanged,
        Component::System => MissCause::SystemChanged,
    }
}

fn wire_change(
    before: &Print,
    was: &WirePrint,
    is: &WirePrint,
    drops: &[ThinkingDrop],
) -> Option<MissCause> {
    if was.betas != is.betas {
        return Some(MissCause::BetasChanged);
    }
    let shared = was.messages.len().min(is.messages.len());
    if let Some(index) = was
        .messages
        .iter()
        .zip(&is.messages)
        .position(|(a, b)| a != b)
    {
        return Some(MissCause::MessageChanged { index });
    }
    if is.messages.len() < was.messages.len() {
        return Some(MissCause::MessageChanged { index: shared });
    }
    let earlier = placed(&before.drops, shared);
    let later = placed(drops, shared);
    let moved: Vec<&Drop<'_>> = earlier.symmetric_difference(&later).collect();
    let first = moved.first()?;
    let injected: Vec<&&Drop<'_>> = moved
        .iter()
        .filter(|d| !is.thinking.contains(&(d.message, d.block)))
        .collect();
    Some(match injected.first() {
        Some(at) => MissCause::InjectedThinking {
            first_path: at.path.to_owned(),
            count: injected.len(),
        },
        None => MissCause::ProviderDroppedThinking {
            first_path: first.path.to_owned(),
            count: moved.len(),
        },
    })
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Drop<'a> {
    message: usize,
    block: usize,
    path: &'a str,
}

fn placed(drops: &[ThinkingDrop], within: usize) -> BTreeSet<Drop<'_>> {
    drops
        .iter()
        .filter_map(|d| {
            let (message, block) = position(&d.path)?;
            Some(Drop {
                message,
                block,
                path: &d.path,
            })
        })
        .filter(|d| d.message < within)
        .collect()
}

fn position(path: &str) -> Option<(usize, usize)> {
    let rest = path.strip_prefix("messages.")?;
    let (message, block) = rest.split_once(".content.")?;
    Some((message.parse().ok()?, block.parse().ok()?))
}

pub(super) fn dropped_thinking(
    before: Option<&Print>,
    is: &WirePrint,
    drops: &[ThinkingDrop],
) -> Option<HistoryChange> {
    let earlier: BTreeSet<&str> = before
        .into_iter()
        .flat_map(|p| &p.drops)
        .map(|d| d.path.as_str())
        .collect();
    let grown: Vec<&ThinkingDrop> = drops
        .iter()
        .filter(|d| !earlier.contains(d.path.as_str()))
        .filter(|d| position(&d.path).is_some_and(|at| is.thinking.contains(&at)))
        .collect();
    let first = grown.first()?;
    let shared = grown.iter().all(|d| d.reason == first.reason);
    Some(HistoryChange::ThinkingDropped {
        count: grown.len(),
        reason: first.reason.clone().filter(|_| shared),
    })
}

#[derive(Debug, Default)]
pub(super) struct Repeats {
    kind: Option<String>,
    count: u32,
    usd: Option<f64>,
}

impl Repeats {
    pub(super) fn missed(
        &mut self,
        cause: &MissCause,
        usd: Option<f64>,
    ) -> Option<(u32, Option<f64>)> {
        if !cause.repeats() {
            self.hit();
            return None;
        }
        if self.kind.as_deref() == Some(cause.as_str()) {
            self.count = self.count.saturating_add(1);
            self.usd = self.usd.zip(usd).map(|(a, b)| a + b);
        } else {
            *self = Self {
                kind: Some(cause.as_str().to_owned()),
                count: 1,
                usd,
            };
        }
        (self.count >= 2).then_some((self.count, self.usd))
    }

    pub(super) fn hit(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests;
