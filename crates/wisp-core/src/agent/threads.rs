use std::collections::BTreeSet;

use jiff::{SignedDuration, Timestamp};
use wisp_protocol::StopReason;
use wisp_protocol::log::ThreadSend;

use crate::clock::Clock;
use crate::provider::{AssembledMessage, Provider, ProviderError, Thread, WirePrint};

#[cfg(test)]
mod tests;

const LIFE: SignedDuration = SignedDuration::from_mins(5);

#[derive(Debug, Default)]
pub(super) struct Threads {
    head: Option<Head>,
    dropped: BTreeSet<usize>,
    sent: Option<Sent>,
    logged: Option<(ThreadSend, Option<String>)>,
}

#[derive(Debug)]
struct Head {
    id: String,
    endpoint: usize,
    pinned: String,
    messages: Vec<String>,
    at: Timestamp,
    replay: bool,
}

#[derive(Debug, Clone, Copy)]
struct Sent {
    endpoint: usize,
    thread: Thread,
    from_replay: bool,
}

impl Threads {
    pub(super) fn usable(&self, endpoint: usize) -> bool {
        !self.dropped.contains(&endpoint)
    }

    pub(super) fn choose(
        &mut self,
        may: bool,
        endpoint: usize,
        wire: Option<&WirePrint>,
        clock: &dyn Clock,
    ) -> (Option<Thread>, Option<String>) {
        self.sent = None;
        if !may {
            return (None, None);
        }
        if !self.usable(endpoint) {
            self.head = None;
            return (None, None);
        }
        let continued = self.head.take().filter(|head| {
            wire.is_some_and(|wire| {
                head.endpoint == endpoint
                    && clock.now().duration_since(head.at) < LIFE
                    && wire.pinned == head.pinned
                    && wire.messages.len() > head.messages.len()
                    && wire.messages.starts_with(&head.messages)
            })
        });
        let (thread, id, from_replay) = match continued {
            Some(head) => {
                let held = head.messages.len();
                (Thread::Continue { held }, Some(head.id), head.replay)
            }
            None => (Thread::Create, None, false),
        };
        self.sent = Some(Sent {
            endpoint,
            thread,
            from_replay,
        });
        self.logged = match (thread, self.logged.take()) {
            (Thread::Create, Some((ThreadSend::Replay, error))) => {
                Some((ThreadSend::Replay, error))
            }
            (Thread::Create, _) => Some((ThreadSend::Create, None)),
            (Thread::Continue { .. }, _) => Some((ThreadSend::Continue, None)),
        };
        (Some(thread), id)
    }

    pub(super) fn continued(&self) -> bool {
        matches!(
            self.sent,
            Some(Sent {
                thread: Thread::Continue { .. },
                ..
            })
        )
    }

    pub(super) fn refused(&mut self, error: &ProviderError) -> bool {
        let Some(sent) = self.sent else {
            return false;
        };
        let replay = matches!(sent.thread, Thread::Continue { .. }) && !sent.from_replay;
        if !replay {
            self.dropped.insert(sent.endpoint);
        }
        let send = if replay {
            ThreadSend::Replay
        } else {
            ThreadSend::Dropped
        };
        self.logged = Some((send, error.detail.error_type.clone()));
        true
    }

    pub(super) fn keep(
        &mut self,
        reply: &AssembledMessage,
        from_fallback: bool,
        wire: Option<&WirePrint>,
        provider: &dyn Provider,
        endpoint: usize,
        clock: &dyn Clock,
    ) {
        let stopped = matches!(reply.stop_reason, StopReason::EndTurn | StopReason::ToolUse);
        if self.sent.take().is_none() || from_fallback || !stopped || reply.reencoded_input {
            return;
        }
        let (Some(id), Some(wire), Some(digest)) =
            (reply.id.clone(), wire, provider.reply_print(&reply.content))
        else {
            return;
        };
        let mut messages = wire.messages.clone();
        messages.push(digest);
        self.head = Some(Head {
            id,
            endpoint,
            pinned: wire.pinned.clone(),
            messages,
            at: clock.now(),
            replay: matches!(self.logged, Some((ThreadSend::Replay, _))),
        });
    }

    pub(super) fn logged(&mut self) -> (Option<ThreadSend>, Option<String>) {
        self.logged
            .take()
            .map_or((None, None), |(send, error)| (Some(send), error))
    }
}
