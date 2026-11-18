use std::collections::BTreeMap;

use wisp_protocol::agents::AgentPath;
use wisp_protocol::{AgentEvent, SessionView};

const OUTPUT_CAP: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct Store(BTreeMap<AgentPath, Vec<AgentEvent>>);

impl Store {
    pub(crate) fn push(&mut self, event: AgentEvent, pending: bool) {
        if event.is_private_auth() {
            return;
        }
        let Some(event) = event.without_links() else {
            return;
        };
        let (agent, event) = unwrapped(event);
        let events = self.0.entry(agent).or_default();
        if matches!(
            event,
            AgentEvent::TurnStarted {
                origin: None,
                retry_of: None,
                ..
            }
        ) {
            for saved in events.iter_mut() {
                if let AgentEvent::Rewound { returned, .. } = saved {
                    returned.clear();
                }
            }
        }
        if matches!(event, AgentEvent::TurnEnded { .. }) && !pending {
            events.clear();
            return;
        }
        if let Some(last) = events.last_mut() {
            match (last, &event) {
                (
                    AgentEvent::TextDelta { index: a, text },
                    AgentEvent::TextDelta {
                        index: b,
                        text: more,
                    },
                )
                | (
                    AgentEvent::ThinkingDelta { index: a, text },
                    AgentEvent::ThinkingDelta {
                        index: b,
                        text: more,
                    },
                ) if a == b => {
                    text.push_str(more);
                    return;
                }
                (
                    AgentEvent::ToolOutput { call_id: a, text },
                    AgentEvent::ToolOutput {
                        call_id: b,
                        text: more,
                    },
                ) if a == b => {
                    text.push_str(more);
                    cap(events);
                    return;
                }
                (
                    AgentEvent::ProcessOutput {
                        id: a,
                        text,
                        skipped,
                    },
                    AgentEvent::ProcessOutput {
                        id: b,
                        text: more,
                        skipped: lost,
                    },
                ) if a == b => {
                    text.push_str(more);
                    *skipped = skipped.saturating_add(*lost);
                    cap(events);
                    return;
                }
                _ => {}
            }
        }
        events.push(event);
        cap(events);
    }

    pub(crate) fn since(&self, view: &SessionView, state: &[AgentEvent]) -> Vec<AgentEvent> {
        let cut = view.events.iter().rev().find_map(|event| match event {
            AgentEvent::TurnStarted { turn, .. } => Some(*turn),
            _ => None,
        });
        let listed: Vec<&AgentPath> = state
            .iter()
            .filter_map(|event| match event {
                AgentEvent::AgentsSnapshot { agents, .. } => Some(agents.iter().map(|a| &a.path)),
                _ => None,
            })
            .flatten()
            .collect();
        let mut replay = Vec::new();
        for (agent, events) in &self.0 {
            let mut turn = None;
            let mut start = 0;
            if agent.is_root() {
                for (i, event) in events.iter().enumerate() {
                    match event {
                        AgentEvent::TurnStarted { turn: n, .. } => turn = Some(*n),
                        AgentEvent::TurnEnded { .. } if turn.is_some() && turn == cut => {
                            start = i + 1;
                        }
                        _ => {}
                    }
                }
            }
            for event in &events[start..] {
                if let AgentEvent::AgentSpawned { agent, .. } = event
                    && listed.contains(&&agent.path)
                {
                    continue;
                }
                let event = event.clone();
                replay.push(if agent.is_root() {
                    event
                } else {
                    AgentEvent::Agent {
                        agent: agent.clone(),
                        event: Box::new(event),
                    }
                });
            }
        }
        replay
    }

    pub(crate) fn trim(&mut self) {
        for events in self.0.values_mut() {
            if let Some(cut) = events
                .iter()
                .rposition(|event| matches!(event, AgentEvent::TurnEnded { .. }))
            {
                events.drain(..=cut);
            }
        }
    }
}

fn unwrapped(event: AgentEvent) -> (AgentPath, AgentEvent) {
    match event {
        AgentEvent::Agent { agent, event } => match *event {
            AgentEvent::Agent { .. } => unwrapped(*event),
            event => (agent, event),
        },
        event => (AgentPath::root(), event),
    }
}

fn cap(events: &mut [AgentEvent]) {
    let mut calls = BTreeMap::<String, usize>::new();
    let mut processes = BTreeMap::<u32, usize>::new();
    for event in events.iter_mut().rev() {
        let (text, left, skipped) = match event {
            AgentEvent::ToolOutput { call_id, text } => (
                text,
                calls.entry(call_id.clone()).or_insert(OUTPUT_CAP),
                None,
            ),
            AgentEvent::ProcessOutput { id, text, skipped } => (
                text,
                processes.entry(*id).or_insert(OUTPUT_CAP),
                Some(skipped),
            ),
            _ => continue,
        };
        let mut cut = text.len().saturating_sub(*left);
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        if cut > 0 {
            text.drain(..cut);
            if let Some(skipped) = skipped {
                *skipped = skipped.saturating_add(u64::try_from(cut).unwrap_or(u64::MAX));
            }
        }
        *left = left.saturating_sub(text.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_turn_expires_only_its_agents_rewound_returns() {
        use wisp_protocol::TurnOrigin;
        let rewound: AgentEvent = serde_json::from_str(include_str!(
            "../../wisp-protocol/tests/fixtures/rewound_event.json"
        ))
        .unwrap();
        for agent in [AgentPath::root(), AgentPath::new("/child")] {
            for (origin, retry_of) in [
                (None, None),
                (Some(TurnOrigin::Mail), None),
                (Some(TurnOrigin::ProcessExit), None),
                (None, Some(3)),
            ] {
                let mut store = Store::default();
                let saved = |store: &Store| {
                    store
                        .since(&SessionView::default(), &[])
                        .into_iter()
                        .map(unwrapped)
                        .find(|(path, event)| {
                            path == &agent && matches!(event, AgentEvent::Rewound { .. })
                        })
                        .map(|(_, event)| event)
                };
                store.push(
                    AgentEvent::Agent {
                        agent: agent.clone(),
                        event: Box::new(rewound.clone()),
                    },
                    false,
                );
                let other = if agent.is_root() {
                    AgentPath::new("/child")
                } else {
                    AgentPath::root()
                };
                store.push(
                    AgentEvent::Agent {
                        agent: other,
                        event: Box::new(AgentEvent::turn_started(4)),
                    },
                    false,
                );
                assert_eq!(
                    saved(&store),
                    Some(rewound.clone()),
                    "other agent's turn: {agent:?}"
                );
                let mut started = AgentEvent::turn_started(4);
                if let AgentEvent::TurnStarted {
                    origin: by,
                    retry_of: retry,
                    ..
                } = &mut started
                {
                    *by = origin;
                    *retry = retry_of;
                }
                store.push(
                    AgentEvent::Agent {
                        agent: agent.clone(),
                        event: Box::new(started),
                    },
                    false,
                );
                let mut expected = rewound.clone();
                if origin.is_none()
                    && retry_of.is_none()
                    && let AgentEvent::Rewound { returned, .. } = &mut expected
                {
                    returned.clear();
                }
                assert_eq!(
                    saved(&store),
                    Some(expected),
                    "{agent:?}, origin {origin:?}, retry {retry_of:?}"
                );
            }
        }
    }

    #[test]
    fn replay_keeps_server_notes_without_keeping_sign_in_links() {
        let event: AgentEvent = serde_json::from_value(serde_json::json!({"type":"plugins_changed","notes":[{"plugin":"mcp","message":"child added server local"}],"links":[{"plugin":"mcp","message":"sign in","link":"https://example.invalid/authorize?CANARY-placeholder"}]})).unwrap();
        for event in [
            event.clone(),
            AgentEvent::Agent {
                agent: AgentPath::new("/child"),
                event: Box::new(event),
            },
        ] {
            let mut store = Store::default();
            store.push(event, true);
            let saved = serde_json::to_string(&store.0).unwrap();
            assert!(!saved.contains("CANARY-placeholder"));
            assert!(saved.contains("child added server local"));
            assert!(!saved.contains("\"links\""));
        }
        let only: AgentEvent = serde_json::from_value(serde_json::json!({"type":"plugins_changed","links":[{"plugin":"mcp","message":"sign in","link":"https://example.invalid/authorize?CANARY-placeholder"}]})).unwrap();
        for event in [
            only.clone(),
            AgentEvent::Agent {
                agent: AgentPath::new("/child"),
                event: Box::new(only),
            },
        ] {
            let mut store = Store::default();
            store.push(event, true);
            assert!(store.0.is_empty());
        }
    }
}
