use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use wisp_plugin_api::hooks::{BeforeTurn, Event, Reply};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

const IDLE_MINUTES: u64 = 10;

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    let since = AtomicU64::new(host.elapsed_ms());
    Arc::new(Idle {
        host,
        since,
        wrote: AtomicBool::new(false),
    })
}

pub fn note() -> String {
    format!(
        "The user is idle (no message from them in the {IDLE_MINUTES} minutes or more since your last reply to them): carry on with your work, but end each turn with no text at all, not even a status line, until they write again.\n"
    )
}

struct Idle {
    host: Arc<dyn Host>,
    since: AtomicU64,
    wrote: AtomicBool,
}

impl Idle {
    fn wrote(&self) {
        self.wrote.store(true, Ordering::SeqCst);
    }

    fn done(&self) {
        if self.wrote.swap(false, Ordering::SeqCst) {
            self.since.store(self.host.elapsed_ms(), Ordering::SeqCst);
        }
    }

    fn note(&self, turn: &BeforeTurn) -> Reply {
        let now = self.host.elapsed_ms();
        if !turn.typed.is_empty() || turn.images > 0 || turn.retry {
            self.wrote();
            self.since.store(now, Ordering::SeqCst);
            return Reply::None;
        }
        let main = self
            .host
            .current_agent()
            .is_none_or(|me| me.parent.is_none());
        let quiet = now.saturating_sub(self.since.load(Ordering::SeqCst));
        if main && quiet >= IDLE_MINUTES * 60_000 {
            Reply::Append(vec![note()])
        } else {
            Reply::None
        }
    }
}

impl Plugin for Idle {
    fn on_hook<'a>(&'a self, _hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        let reply = match event {
            Event::UserInput(_) => {
                self.wrote();
                Reply::None
            }
            Event::BeforeTurn(turn) => self.note(&turn),
            Event::AfterTurn(_) => {
                self.done();
                Reply::None
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    }
}

#[cfg(test)]
mod tests;
