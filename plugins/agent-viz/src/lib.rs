use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use wisp_plugin_api::host::{Host, Instant};
use wisp_plugin_api::slots::{
    Block, Content, Flow, NodeState, Noun, RENDER_GAP_MS, SlotView, Topology, TopologyNode,
};
use wisp_plugin_api::tree::{AgentState, TreeEvent};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::agents::ROOT_NAME;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

const MAX_FLOWS: usize = 16;

const FRESH_MS: u64 = 4 * RENDER_GAP_MS;

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(AgentViz {
        host,
        mail: Mutex::default(),
    })
}

pub struct AgentViz {
    host: Arc<dyn Host>,
    mail: Mutex<VecDeque<Mail>>,
}

struct Mail {
    from: Option<String>,
    to: Option<String>,
    heard_ms: u64,
    at_ms: Option<u64>,
}

fn unix_ms(at: Instant) -> u64 {
    let seconds = u64::try_from(at.seconds).unwrap_or(0);
    seconds.saturating_mul(1000) + u64::from(at.nanos / 1_000_000)
}

fn end(path: String) -> Option<String> {
    (path != "/").then_some(path)
}

impl AgentViz {
    fn topology(&self) -> Topology {
        let states = self.host.states();
        let agents = self.host.snapshot();
        let ended: Vec<String> = (agents.iter())
            .filter(|agent| agent.state == AgentState::Ended)
            .map(|agent| format!("{}/", agent.path))
            .collect();
        let mut nodes: Vec<TopologyNode> = (agents.into_iter())
            .filter(|agent| agent.state != AgentState::Ended)
            .filter(|agent| !ended.iter().any(|gone| agent.path.starts_with(gone)))
            .map(|agent| TopologyNode {
                state: (states.iter())
                    .find(|shown| shown.path == agent.path)
                    .map_or(NodeState::Waiting, |shown| shown.state),
                parent: agent.parent.and_then(end),
                key: agent.path,
            })
            .collect();
        nodes.sort_by(|a, b| a.key.cmp(&b.key));
        Topology {
            root: Some(ROOT_NAME.to_owned()),
            flows: self.flows(&nodes),
            nodes,
            counted: Some(Noun::Agents),
        }
    }

    fn flows(&self, nodes: &[TopologyNode]) -> Vec<Flow> {
        let now = unix_ms(self.host.now());
        let known =
            |end: &Option<String>| (end.as_ref()).is_none_or(|k| nodes.iter().any(|n| &n.key == k));
        let mut mail = self.mail.lock().unwrap_or_else(PoisonError::into_inner);
        mail.retain_mut(|m| {
            if m.at_ms.is_none() && now.saturating_sub(m.heard_ms) <= FRESH_MS {
                m.at_ms = Some(now);
            }
            m.at_ms.is_some() && known(&m.from) && known(&m.to)
        });
        (mail.iter())
            .filter_map(|m| {
                (m.at_ms).map(|at_ms| Flow {
                    from: m.from.clone(),
                    to: m.to.clone(),
                    at_ms,
                })
            })
            .collect()
    }
}

impl Plugin for AgentViz {
    fn render_slot<'a>(
        &'a self,
        _name: &'a str,
        view: SlotView,
    ) -> BoxFuture<'a, Result<Content, String>> {
        let topology = self.topology();
        let blocks = if view == SlotView::Page || !topology.nodes.is_empty() {
            vec![Block::Topology(topology)]
        } else {
            Vec::new()
        };
        Box::pin(async move {
            Ok(Content {
                note: Vec::new(),
                blocks,
            })
        })
    }

    fn on_agent_hook<'a>(&'a self, _hook: &'a str, event: TreeEvent) -> BoxFuture<'a, ()> {
        if let TreeEvent::AgentMail(sent) = event {
            let mut mail = self.mail.lock().unwrap_or_else(PoisonError::into_inner);
            mail.push_back(Mail {
                from: end(sent.from),
                to: end(sent.to),
                heard_ms: unix_ms(self.host.now()),
                at_ms: None,
            });
            if mail.len() > MAX_FLOWS {
                mail.pop_front();
            }
        }
        Box::pin(std::future::ready(()))
    }
}

#[cfg(test)]
mod tests;
