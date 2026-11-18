use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;
use wisp_plugin_api::tree::AgentNode;
use wisp_protocol::{Activity, AgentPath, AgentState, MailId, MailKind, ReportOutcome};

use crate::plugin::hooks::points::{
    AGENT_ENDED, AGENT_MAIL, AGENT_REPORTED, AGENT_SPAWNED, AGENT_STATE, AGENT_STOPPED,
};

#[derive(Debug, Clone, PartialEq)]
pub enum TreeNews {
    Spawned(Box<AgentNode>),
    State(StateNews),
    Mail(MailNews),
    Reported(ReportNews),
    Stopped(StopNews),
    Ended(StopNews),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateNews {
    pub agent: AgentPath,
    pub state: AgentState,
    pub activity: Activity,
    pub spent_usd: f64,
    pub lower_bound: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailNews {
    pub id: MailId,
    pub from: AgentPath,
    pub to: AgentPath,
    pub kind: MailKind,
    pub wake: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportNews {
    pub agent: AgentPath,
    pub turn: u64,
    pub outcome: ReportOutcome,
    pub cost_usd: Option<f64>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopNews {
    pub agent: AgentPath,
    pub target: AgentPath,
    pub by: Option<AgentPath>,
}

impl TreeNews {
    pub fn point(&self) -> &'static str {
        match self {
            Self::Spawned(_) => AGENT_SPAWNED,
            Self::State(_) => AGENT_STATE,
            Self::Mail(_) => AGENT_MAIL,
            Self::Reported(_) => AGENT_REPORTED,
            Self::Stopped(_) => AGENT_STOPPED,
            Self::Ended(_) => AGENT_ENDED,
        }
    }

    fn concerns(&self, agent: &AgentPath) -> bool {
        match self {
            Self::Spawned(node) => node.path == agent.as_str(),
            Self::State(news) => &news.agent == agent,
            Self::Mail(news) => &news.from == agent || &news.to == agent,
            Self::Reported(news) => &news.agent == agent,
            Self::Stopped(news) | Self::Ended(news) => &news.agent == agent,
        }
    }
}

#[derive(Default)]
struct Queue {
    items: VecDeque<TreeNews>,
    closed: bool,
}

#[derive(Default)]
pub struct News {
    queue: Mutex<Queue>,
    ready: Notify,
}

impl News {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn push(&self, item: TreeNews) {
        {
            let mut queue = self.queue();
            if queue.closed {
                return;
            }
            if let TreeNews::State(news) = &item {
                let newest = queue.items.iter().rposition(|i| i.concerns(&news.agent));
                if let Some(at) = newest
                    && matches!(queue.items[at], TreeNews::State(_))
                {
                    queue.items.remove(at);
                }
            }
            queue.items.push_back(item);
        }
        self.ready.notify_one();
    }

    pub async fn next(&self) -> Option<TreeNews> {
        loop {
            {
                let mut queue = self.queue();
                if queue.closed {
                    return None;
                }
                if let Some(item) = queue.items.pop_front() {
                    return Some(item);
                }
            }
            self.ready.notified().await;
        }
    }

    pub fn close(&self) {
        {
            let mut queue = self.queue();
            queue.closed = true;
            queue.items.clear();
        }
        self.ready.notify_one();
    }
}

#[cfg(test)]
mod tests;
