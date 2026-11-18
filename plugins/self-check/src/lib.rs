use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const WORKED: &str = "worked";

pub const REVIEW: &str = "review";

pub const SETTING: &str = "self_check.before_end";

pub const TEXT: &str = "Before you end: re-read the task. List each requirement it states and, for each, the evidence that it is met: a command you ran and what it showed, or a file you checked. Fix any gap you find, then end your turn.";

const TOOL_USE: &str = "tool_use";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(SelfCheck {
        host,
        worked: AtomicU64::new(0),
        checked: AtomicU64::new(0),
    })
}

struct SelfCheck {
    host: Arc<dyn Host>,
    worked: AtomicU64,
    checked: AtomicU64,
}

impl SelfCheck {
    fn review(&self, turn: u64) -> Reply {
        let root = self
            .host
            .current_agent()
            .is_some_and(|me| me.parent.is_none());
        let due = root
            && self.worked.load(Ordering::SeqCst) == turn
            && self.checked.swap(turn, Ordering::SeqCst) != turn;
        if due {
            Reply::Continue(TEXT.to_owned())
        } else {
            Reply::None
        }
    }
}

impl Plugin for SelfCheck {
    fn on_hook<'a>(&'a self, hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        let reply = match (hook, event) {
            (WORKED, Event::AfterResponse(info)) => {
                if info.stop_reason == TOOL_USE {
                    self.worked.store(info.turn, Ordering::SeqCst);
                }
                Reply::None
            }
            (REVIEW, Event::BeforeEnd(end)) => self.review(end.turn),
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    }
}

#[cfg(test)]
mod tests;
