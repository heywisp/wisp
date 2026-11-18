use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;
use wisp_plugin_api::manifest::Origin;
use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY};
use wisp_protocol::{AgentEvent, ClientCommand};

use super::{THINKING_AND_TEXT, api_key, http, mock_server, split_body};
use crate::agent::Agent;
use crate::plugin::testing::plugin_switch;
use crate::plugin::{Make, PluginConfig, Registry, Source};
use crate::provider::anthropic::AnthropicProvider;
use crate::test_guard::until_event;

async fn with_tools(base: &str) -> Agent {
    let sources: Vec<Source> = wisp_plugins::bundled()
        .into_iter()
        .map(|b| {
            let make: Make = Arc::new(move |host| (b.instantiate)(host));
            Source::in_process(b.manifest.to_owned(), make, Origin::Bundled)
        })
        .collect();
    let (registry, problems) = Registry::from_origins(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on: BTreeSet<String> = ["show_diff", "web_search"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let started = registry
        .start(&on, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    let provider = AnthropicProvider::new(&api_key("k"), base).unwrap();
    Agent::new(Arc::new(provider), crate::agent::default_session())
        .with_clock(crate::clock::TickingClock::new())
        .with_retry_policy(crate::agent::retry::quick())
        .with_catalog(started.catalog)
        .with_tools(started.toolbox, workspace)
        .with_platform("Linux")
        .with_resources(None)
        .with_hooks(started.hooks, started.host)
}

async fn until(events: &mut mpsc::Receiver<AgentEvent>, pick: impl Fn(&AgentEvent) -> bool) {
    until_event(events, &mut Vec::new(), "the event", pick).await;
}

#[tokio::test]
async fn a_mid_session_disable_and_removal_of_plugins_with_tools_keep_every_byte_before_the_messages()
 {
    let responses = vec![
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
    ];
    let (base, seen) = mock_server(responses).await;
    let mut agent = with_tools(&base).await;
    let (commands, mut inbox) = mpsc::channel(16);
    let (sink, mut events) = mpsc::channel(4_096);
    let task = tokio::spawn(async move {
        agent.serve(&mut inbox, &sink).await;
    });
    let turn = |id: &'static str| ClientCommand::send(id, id);
    let ended = |e: &AgentEvent| matches!(e, AgentEvent::TurnEnded { .. });
    commands.send(turn("first")).await.unwrap();
    until(&mut events, ended).await;
    for (request, id, key) in [
        (1, "show-diff", ENABLED_KEY),
        (2, "web-search", INSTALLED_KEY),
    ] {
        commands
            .send(plugin_switch(request, id, key, false))
            .await
            .unwrap();
        until(
            &mut events,
            |e| matches!(e, AgentEvent::SettingsChanged { answers: Some(r), .. } if r.0 == request),
        )
        .await;
        commands
            .send(turn(if request == 1 { "second" } else { "third" }))
            .await
            .unwrap();
        until(&mut events, ended).await;
    }
    drop(commands);
    task.await.unwrap();
    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.len(), 3);
    let bodies: Vec<Value> = reqs
        .iter()
        .map(|r| serde_json::from_str(http::body(r)).unwrap())
        .collect();
    let names: Vec<&str> = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"show_diff") && names.contains(&"web_search"),
        "{names:?}"
    );
    for pair in reqs.windows(2) {
        let (system_1, messages_1) = split_body(http::body(&pair[0]));
        let (system_2, messages_2) = split_body(http::body(&pair[1]));
        assert_eq!(system_1, system_2);
        assert!(
            messages_2.starts_with(messages_1.strip_suffix(']').unwrap()),
            "\n{messages_1}\n{messages_2}"
        );
    }
    for body in &bodies[1..] {
        assert_eq!(body["tools"], bodies[0]["tools"]);
        assert_eq!(body["system"], bodies[0]["system"]);
    }
}

#[tokio::test]
async fn the_time_plugin_switched_off_or_removed_stops_stamps_from_the_next_turn_and_keeps_the_cache()
 {
    for key in [ENABLED_KEY, INSTALLED_KEY] {
        let responses = vec![http::sse(THINKING_AND_TEXT), http::sse(THINKING_AND_TEXT)];
        let (base, seen) = mock_server(responses).await;
        let mut agent = with_tools(&base).await;
        let (commands, mut inbox) = mpsc::channel(16);
        let (sink, mut events) = mpsc::channel(4_096);
        let task = tokio::spawn(async move {
            agent.serve(&mut inbox, &sink).await;
        });
        let ended = |e: &AgentEvent| matches!(e, AgentEvent::TurnEnded { .. });
        commands
            .send(ClientCommand::send("a", "first"))
            .await
            .unwrap();
        until(&mut events, ended).await;
        commands
            .send(plugin_switch(1, "time", key, false))
            .await
            .unwrap();
        let (tx, mut answer) = mpsc::channel(1);
        until(&mut events, |e| {
            let hit = matches!(e, AgentEvent::SettingsChanged { answers: Some(r), .. } if r.0 == 1);
            if hit {
                let _ = tx.try_send(e.clone());
            }
            hit
        })
        .await;
        let Some(AgentEvent::SettingsChanged { changes, .. }) = answer.recv().await else {
            panic!("no answer");
        };
        assert!(
            changes
                .iter()
                .all(|c| c.cache == Some(wisp_protocol::CacheEffect::Kept)),
            "{changes:?}"
        );
        commands
            .send(ClientCommand::send("b", "second"))
            .await
            .unwrap();
        until(&mut events, ended).await;
        drop(commands);
        task.await.unwrap();
        let reqs = seen.lock().unwrap();
        let (system_1, messages_1) = split_body(http::body(&reqs[0]));
        let (system_2, messages_2) = split_body(http::body(&reqs[1]));
        assert_eq!(system_1, system_2, "{key}");
        assert!(
            messages_2.starts_with(messages_1.strip_suffix(']').unwrap()),
            "{key}"
        );
        let last: Value = serde_json::from_str(messages_2).unwrap();
        let last = last.as_array().unwrap().last().unwrap().to_string();
        assert!(messages_1.contains("Sent: "), "{key}");
        assert!(!last.contains("Sent: "), "{key} {last}");
    }
}
