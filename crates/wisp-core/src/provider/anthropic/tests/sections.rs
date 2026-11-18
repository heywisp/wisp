use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ClientCommand, RequestId};

use super::{THINKING_AND_TEXT, api_key, http, mock_server, split_body};
use crate::agent::Agent;
use crate::context::ModelSettings;
use crate::models::{ConfigModels, ModelTable};
use crate::provider::anthropic::AnthropicProvider;
use crate::session::{MemoryStore, SessionLog};
use crate::test_guard::records;
use crate::test_guard::{log_context, until_event};
use wisp_plugin_testkit::hooks::{self as kit, Calls, Knob};

const WISP: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
const WRITTEN: &str = "## Goal and standing instructions\nSay hello.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: the greeting.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

fn written() -> String {
    let text = serde_json::to_string(WRITTEN).unwrap();
    http::sse(&format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_s\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{{\"input_tokens\":5000,\"output_tokens\":1}}}}}}\n\n\
event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{text}}}}}\n\n\
event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\",\"stop_sequence\":null}},\"usage\":{{\"output_tokens\":60}}}}\n\n\
event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    ))
}

fn tools_of(body: &str) -> Value {
    serde_json::from_str::<Value>(body).unwrap()["tools"].clone()
}

fn assert_extends(earlier: &str, later: &str) {
    let (system_1, messages_1) = split_body(earlier);
    let (system_2, messages_2) = split_body(later);
    assert_eq!(system_1, system_2);
    assert_eq!(tools_of(earlier), tools_of(later));
    assert!(
        messages_2.starts_with(messages_1.strip_suffix(']').unwrap()),
        "\n{messages_1}\n{messages_2}"
    );
}

fn without_markers(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != "cache_control")
                .map(|(k, v)| (k.clone(), without_markers(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_markers).collect()),
        other => other.clone(),
    }
}

fn assert_reads_the_cache_of(earlier: &str, summariser: &str) {
    let (system_1, _) = split_body(earlier);
    let (system_2, _) = split_body(summariser);
    assert_eq!(system_1, system_2);
    assert_eq!(tools_of(earlier), tools_of(summariser));
    let earlier: Value = serde_json::from_str(earlier).unwrap();
    let summariser: Value = serde_json::from_str(summariser).unwrap();
    let kept = earlier["messages"].as_array().unwrap();
    let sent = summariser["messages"].as_array().unwrap();
    assert!(sent.len() > kept.len());
    assert_eq!(
        without_markers(&Value::Array(sent[..kept.len()].to_vec())),
        without_markers(&earlier["messages"])
    );
    let marked = sent[kept.len() - 1]["content"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert!(marked.get("cache_control").is_some(), "{marked}");
}

async fn sectioned(base: &str, knob: &Knob, log: SessionLog) -> Agent {
    let body = kit::prompt_section("notes", "lead", "");
    let make: crate::plugin::Make =
        kit::prompted(kit::none(), kit::changing(knob.clone()), Calls::default());
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![(kit::manifest_in("0.2.0", "bb", &body), make)],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(
            &std::collections::BTreeSet::new(),
            Arc::new(crate::plugin::KernelHost::new()),
        )
        .await;
    let provider = AnthropicProvider::new(&api_key("k"), base).unwrap();
    Agent::new(Arc::new(provider), crate::agent::default_session())
        .with_clock(crate::clock::TickingClock::new())
        .with_retry_policy(crate::agent::retry::quick())
        .with_model_settings(ModelSettings::new(
            ModelTable::shipped().unwrap(),
            ConfigModels::parse(WISP).unwrap(),
        ))
        .with_hooks(started.hooks, started.host)
        .with_log(log, log_context())
}

struct Served {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    task: tokio::task::JoinHandle<Agent>,
}

impl Served {
    fn start(mut agent: Agent) -> Self {
        let (commands, mut inbox) = mpsc::channel(16);
        let (sink, events) = mpsc::channel(4_096);
        let task = tokio::spawn(async move {
            agent.serve(&mut inbox, &sink).await;
            agent
        });
        Self {
            commands,
            events,
            task,
        }
    }

    async fn until(&mut self, pick: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        until_event(&mut self.events, &mut Vec::new(), "the event", pick).await
    }

    async fn turn(&mut self, id: &str) {
        self.commands
            .send(ClientCommand::send(id, id))
            .await
            .unwrap();
        let ended = self
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        assert!(
            matches!(
                ended,
                AgentEvent::TurnEnded {
                    outcome: wisp_protocol::TurnOutcome::Completed { .. },
                    ..
                }
            ),
            "{ended:?}"
        );
    }

    async fn compact(&mut self) {
        self.commands
            .send(ClientCommand::Compact {
                focus: None,
                request: RequestId(1),
            })
            .await
            .unwrap();
        let done = self
            .until(|e| {
                matches!(
                    e,
                    AgentEvent::Compacted { .. } | AgentEvent::CompactionFailed { .. }
                )
            })
            .await;
        assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    }

    async fn close(self) -> Agent {
        drop(self.commands);
        self.task.await.unwrap()
    }
}

#[tokio::test]
async fn a_section_and_its_head_keep_the_prefix_and_a_resume_extends_the_logged_one() {
    let responses = vec![
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
        written(),
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
    ];
    let (base, seen) = mock_server(responses).await;
    let knob = kit::knob("v1");
    let store = MemoryStore::default();
    let log = SessionLog::new(Box::new(store.clone()), "s1");
    let mut served = Served::start(sectioned(&base, &knob, log).await);
    served.turn("first").await;
    kit::turn_to(&knob, "v2");
    served.turn("second").await;
    served.compact().await;
    served.turn("third").await;
    served.turn("fourth").await;
    served.close().await;
    let records = records(&store);
    let next = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), "s1", next);
    let resumed = sectioned(&base, &knob, log)
        .await
        .resume(&records, 0)
        .unwrap();
    let mut served = Served::start(resumed);
    served.turn("fifth").await;
    served.close().await;

    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.len(), 6);
    let bodies: Vec<&str> = reqs.iter().map(|r| http::body(r)).collect();
    let text = |value: &str| serde_json::to_string(&kit::section_text("notes", value)).unwrap();
    assert_eq!(bodies[0].matches(&text("v1")).count(), 1);
    assert_eq!(bodies[1].matches(&text("v2")).count(), 1);
    assert_extends(bodies[0], bodies[1]);
    assert_reads_the_cache_of(bodies[1], bodies[2]);
    let (_, head) = split_body(bodies[3]);
    assert!(!head.contains(&text("v1")), "{head}");
    assert_eq!(head.matches(&text("v2")).count(), 1, "{head}");
    assert!(
        head.find(&text("v2")) < head.find("<compacted_context"),
        "{head}"
    );
    assert_eq!(split_body(bodies[2]).0, split_body(bodies[3]).0);
    assert_extends(bodies[3], bodies[4]);
    assert_extends(bodies[4], bodies[5]);
    assert_eq!(bodies[5].matches(&text("v2")).count(), 1);
}

impl Served {
    async fn switch(&mut self, request: u64, on: bool) {
        self.commands
            .send(crate::plugin::testing::plugin_switch(
                request,
                "bb",
                wisp_protocol::settings::ENABLED_KEY,
                on,
            ))
            .await
            .unwrap();
        let answer = self
            .until(|e| {
                matches!(
                    e,
                    AgentEvent::SettingsChanged { answers: Some(r), .. } | AgentEvent::Rejected { request: Some(r), .. }
                        if r.0 == request
                )
            })
            .await;
        assert!(
            matches!(answer, AgentEvent::SettingsChanged { .. }),
            "{answer:?}"
        );
    }
}

#[tokio::test]
async fn a_section_switched_off_and_back_on_across_a_head_keeps_the_prefix_and_sends_its_whole_text_once()
 {
    let responses = vec![
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
        written(),
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
    ];
    let (base, seen) = mock_server(responses).await;
    let knob = kit::knob("v1");
    let store = MemoryStore::default();
    let log = SessionLog::new(Box::new(store.clone()), "s1");
    let mut served = Served::start(sectioned(&base, &knob, log).await);
    served.turn("first").await;
    served.switch(10, false).await;
    kit::turn_to(&knob, "v2");
    served.turn("second").await;
    served.compact().await;
    served.switch(11, true).await;
    served.turn("third").await;
    served.turn("fourth").await;
    served.close().await;
    let records = records(&store);
    let next = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), "s1", next);
    let resumed = sectioned(&base, &knob, log)
        .await
        .resume(&records, 0)
        .unwrap();
    let mut served = Served::start(resumed);
    served.turn("fifth").await;
    served.close().await;

    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.len(), 6);
    let bodies: Vec<&str> = reqs.iter().map(|r| http::body(r)).collect();
    let text = |value: &str| serde_json::to_string(&kit::section_text("notes", value)).unwrap();
    let system = split_body(bodies[0]).0;
    for body in &bodies {
        assert_eq!(split_body(body).0, system);
    }
    assert_eq!(bodies[0].matches(&text("v1")).count(), 1);
    assert_extends(bodies[0], bodies[1]);
    assert!(!bodies[1].contains(&text("v2")));
    assert_reads_the_cache_of(bodies[1], bodies[2]);
    let (_, messages) = split_body(bodies[3]);
    assert!(!messages.contains(&text("v1")), "{messages}");
    assert_eq!(messages.matches(&text("v2")).count(), 1, "{messages}");
    assert!(
        messages.find(&text("v2")) > messages.find("<compacted_context"),
        "{messages}"
    );
    assert!(
        messages.find(&text("v2")) < messages.find("\"third\""),
        "{messages}"
    );
    assert_extends(bodies[3], bodies[4]);
    assert_eq!(bodies[4].matches(&text("v2")).count(), 1);
    assert_extends(bodies[4], bodies[5]);
    assert_eq!(bodies[5].matches(&text("v2")).count(), 1);
}
