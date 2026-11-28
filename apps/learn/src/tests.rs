use super::*;

#[path = "../web/tests/support/agent.rs"]
mod agent;
#[path = "../web/tests/support/client.rs"]
#[allow(dead_code, reason = "the page tests use the rest of these helpers")]
mod client;

fn flow(name: &str) -> String {
    std::fs::read_to_string(Path::new(FLOW).join(name)).unwrap()
}

#[test]
fn the_flow_names_the_hand_off_labels_and_ignores_its_private_files() {
    use wisp_tool_text::voice::{ASKED, CUT_OFF, EARLIER, HEARD, SAID, TAPPED, finished};
    let brain = flow("BRAIN.md");
    for label in [HEARD, SAID, TAPPED, ASKED, CUT_OFF] {
        assert!(brain.contains(&format!("`{label}`")), "{label}: {brain}");
    }
    let changed = finished(wisp_plugin_page::NAME);
    for quoted in [changed.as_str(), EARLIER] {
        assert!(
            flow("VOICE.md").contains(&format!("`{quoted}`")),
            "{quoted}"
        );
    }
    use wisp_core::secrets::{CREDENTIALS, LOCK_FILE, OAUTH};
    let files = ["config.toml", CREDENTIALS, LOCK_FILE].map(|file| format!("/{file}"));
    let folders = [OAUTH, FOLDER].map(|folder| format!("/{folder}/"));
    let ignored: Vec<String> = files.into_iter().chain(folders).collect();
    assert_eq!(flow(".gitignore").lines().collect::<Vec<_>>(), ignored);
}

#[test]
fn the_flows_settings_are_given_from_its_folder_and_never_name_an_account() {
    let flow = std::fs::canonicalize(FLOW).unwrap();
    let (flags, _) = flow_settings(&flow).unwrap();
    let voice = flags
        .iter()
        .find(|given| matches!(&given.target, Target::Name(spec) if spec.name == names::VOICE_INSTRUCTIONS))
        .unwrap();
    let file = flow.join("VOICE.md").display().to_string();
    assert_eq!(voice.value, SettingValue::Text(file));
    assert!(flags.iter().all(|given| given.origin == START_ORIGIN));
    let scratch = tempfile::tempdir().unwrap();
    for accounts in [
        "[voice]\nendpoint = \"talk\"\n",
        "default = \"gw\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:9\"\n",
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:9\"\n",
    ] {
        std::fs::write(scratch.path().join(SETTINGS), accounts).unwrap();
        assert_eq!(
            flow_settings(scratch.path()).err().as_deref(),
            Some(ACCOUNTS),
            "{accounts}"
        );
    }
}

#[test]
fn a_withdrawn_switch_writes_the_lessons_pair_back_over_its_pick() {
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join(SETTINGS);
    let changed = |withdrawn: Option<&str>, model: &str| -> AgentEvent {
        let mut event = serde_json::json!({"type": "settings_changed", "answers": 7,
            "endpoint": "gw", "model": model, "effort": "low", "cache": {"type": "kept"},
            "changes": [{"target": {"name": "model"}, "value": {"kind": "text", "text": model},
                "layer": "session", "when": "next_turn"}]});
        if let Some(why) = withdrawn {
            event["withdrawn"] = serde_json::json!(why);
            event.as_object_mut().unwrap().remove("answers");
        }
        serde_json::from_value(event).unwrap()
    };
    remember(&changed(None, "claude-sonnet-5-5"), &file).unwrap();
    let picked = "account = \"gw\"\nmodel = \"claude-sonnet-5-5\"\n";
    assert_eq!(std::fs::read_to_string(&file).unwrap(), picked);
    let back = changed(Some("switch_compaction_failed"), "claude-opus-5-5");
    remember(&back, &file).unwrap();
    let restored = "account = \"gw\"\nmodel = \"claude-opus-5-5\"\n";
    assert_eq!(std::fs::read_to_string(&file).unwrap(), restored);
}

#[test]
fn the_replayed_lesson_is_scrubbed_after_a_late_secret() {
    use wisp_core::session::{REDACTED, register_secrets};
    use wisp_start::scrub::testing::{CANARY, queued, scrub};

    let opening: Opening = serde_json::from_str(include_str!(
        "../../../crates/wisp-protocol/tests/fixtures/opening.json"
    ))
    .unwrap();
    let view = SessionView {
        events: queued(),
        ..SessionView::default()
    };
    register_secrets([CANARY.to_owned()]);
    let events = replayed(Some(view), &opening, &scrub());
    let shown = serde_json::to_string(&events).unwrap();
    assert!(!shown.contains(CANARY), "{shown}");
    assert_eq!(shown.matches(REDACTED).count(), 4, "{shown}");
    assert!(replayed(None, &opening, &scrub()).is_empty());
}

#[tokio::test]
async fn the_page_and_show_get_only_scrubbed_calls_after_a_late_secret() {
    use agent::{describe, page_call, page_done, port};
    use futures::StreamExt;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;
    use wisp_core::session::{REDACTED, register_secrets};
    use wisp_start::scrub::testing::{CANARY, scrub};

    let raw = || json!({"html": format!("<p>{CANARY}</p>")});
    let view = SessionView {
        events: vec![page_call("old", raw()), page_done("old", false)],
        ..SessionView::default()
    };
    let opening: Opening = serde_json::from_str(include_str!(
        "../../../crates/wisp-protocol/tests/fixtures/opening.json"
    ))
    .unwrap();
    let (commands, mut asked) = mpsc::channel(1);
    let (emit, events) = mpsc::channel(8);
    let (up, _heard) = mpsc::channel(1);
    let (_down, lane_down) = mpsc::channel(1);
    let (stop, stopped) = oneshot::channel::<()>();
    let (told, address) = oneshot::channel::<String>();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (heard, mut told) = (Arc::clone(&seen), Some(told));
    let show = move |shown: Shown<'_>| match shown {
        Shown::Address(line) => {
            if let Some(told) = told.take() {
                let _ = told.send(line.to_owned());
            }
        }
        Shown::Event(event) => heard.lock().unwrap().push(event.clone()),
    };
    let answer = emit.clone();
    tokio::spawn(async move { describe(&mut asked, &answer, "home", &[]).await });
    let setup = WebSetup {
        port: 0,
        folder: None,
        address: None,
    };
    let channels = Channels {
        commands,
        events,
        quit: None,
    };
    let stopping = async move {
        let _ = stopped.await;
    };
    let served = serve(
        setup,
        (Some(view), &opening, scrub()),
        channels,
        (up, lane_down),
        stopping,
        show,
    );
    let driving = async {
        let port = port(&address.await.unwrap());
        register_secrets([CANARY.to_owned()]);
        let host = format!("127.0.0.1:{port}");
        let mut socket = client::open(port, &host, Some(&format!("http://{host}")))
            .await
            .unwrap();
        for _ in 0..2 {
            client::next_text(&mut socket).await;
        }
        let requested = |call: &str| {
            json!({"type": "tool_call_requested", "call_id": call, "tool": "show_page",
                "input": {"html": format!("<p>{REDACTED}</p>")}})
        };
        assert_eq!(client::next_text(&mut socket).await, Some(requested("old")));
        client::next_text(&mut socket).await;
        emit.send(page_call("new", raw())).await.unwrap();
        assert_eq!(client::next_text(&mut socket).await, Some(requested("new")));
        let _ = stop.send(());
        while let Some(Ok(_)) = socket.next().await {}
        drop(emit);
    };
    let (served, ()) = tokio::join!(served, driving);
    assert_eq!(served, Ok(false));
    let shown = serde_json::to_string(&*seen.lock().unwrap()).unwrap();
    assert!(shown.contains("\"call_id\":\"new\""), "{shown}");
    assert!(!shown.contains(CANARY), "{shown}");
}

#[test]
fn a_kept_page_line_is_masked_against_a_secret_registered_after_it_was_kept() {
    use serde_json::json;
    use wisp_core::session::{REDACTED, register_secrets};

    let secret = "CANARYOpaque\"quoted\\later";
    let page = |value: Value| wisp_protocol::jsonl::line(&value).unwrap();
    let requested = page(json!({"type": "tool_call_requested", "call_id": "c1",
        "tool": "show_page", "input": {"html": format!("<p>{secret}</p>")}}));
    let finished = page(json!({"type": "tool_call_finished", "call_id": "c1",
        "outcome": {"status": "completed", "is_error": false}}));
    register_secrets([secret.to_owned(), "tool_call_requested".to_owned()]);
    let masked = redacted(&requested);
    assert!(!masked.contains("Opaque"), "{masked}");
    let kept = json!({"type": "tool_call_requested", "call_id": "c1", "tool": "show_page",
        "input": {"html": format!("<p>{REDACTED}</p>")}});
    assert_eq!(serde_json::from_str::<Value>(&masked).unwrap(), kept);
    assert_eq!(redacted(&finished), finished);
}
