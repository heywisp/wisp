use super::*;

fn results() -> Value {
    json!([
        {"type": "web_search_result", "title": "Wisp", "url": "https://example.com/wisp", "encrypted_content": "RW5jcnlwdGVkMQ==", "page_age": "October 1, 2026"},
        {"type": "web_search_result", "title": "Wisp docs", "url": "https://example.com/wisp/docs", "encrypted_content": "RW5jcnlwdGVkMg=="}
    ])
}

#[tokio::test]
async fn a_native_web_search_pair_is_sent_back_whole() {
    let setup = Setup {
        search: true,
        ..Setup::default()
    };
    let script = vec![
        reply(
            "msg_01",
            400,
            vec![
                Part::Think,
                Part::Search("srvtoolu_01", json!({"query": "wisp terminal"}), results()),
                Part::Text("Wisp is a terminal assistant."),
            ],
            "end_turn",
        ),
        says("msg_02", 900, "You're welcome."),
    ];
    let rig = Rig::new("web_search_native", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "What is Wisp?").await;
    live.completed("u2", "Thanks.").await;
    live.close().await;
    rig.check();
}

const UNREADABLE: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.1.content.0: Invalid `encrypted_content` in `search_result` block"}}"#;

fn refused_unreadable() -> Reply {
    Reply {
        response: http::json("400 Bad Request", UNREADABLE),
        before: Vec::new(),
        gate: None,
    }
}

fn kinds(records: &[LogRecord]) -> Vec<String> {
    records
        .iter()
        .map(|r| {
            serde_json::to_value(&r.event).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn search_results_another_account_cant_read_are_resent_once_as_text() {
    let setup = Setup {
        search: true,
        ..Setup::default()
    };
    let citation = json!({"type": "web_search_result_location", "cited_text": "a terminal assistant", "url": "https://example.com/wisp", "title": "Wisp", "encrypted_index": "RWk="});
    let script = vec![
        reply(
            "msg_01",
            400,
            vec![
                Part::Think,
                Part::Search("srvtoolu_01", json!({"query": "wisp terminal"}), results()),
                Part::Think,
                Part::Cited("Wisp is a terminal assistant.", citation),
            ],
            "end_turn",
        ),
        refused_unreadable(),
        says("msg_02", 900, "You're welcome."),
        says("msg_03", 1_000, "Bye."),
    ];
    let rig = Rig::new("web_search_unreadable", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "What is Wisp?").await;
    let committed = records(&rig.store);
    live.completed("u2", "Thanks.").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let logged = records(&rig.store);
    assert_eq!(logged[..committed.len()], committed[..]);
    let kinds = kinds(&logged);
    let record: Vec<usize> = kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| *k == "search_history_unreadable")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(record.len(), 1);
    assert!(record[0] >= committed.len());
    assert!(!kinds.iter().any(|k| k == "thinking_cut"));
    let mut resumed = rig.resume();
    resumed.completed("u3", "Bye.").await;
    resumed.close().await;
    rig.check();

    let bodies = rig.server.seen().bodies;
    assert_eq!(bodies.len(), 4);
    assert!(bodies[1].contains("encrypted_"));
    assert!(!bodies[2].contains("encrypted_"));
    assert!(!bodies[3].contains("encrypted_"));
    let resent: Value = serde_json::from_str(&bodies[2]).unwrap();
    assert!(resent["thinking"].get("block_binding").is_some());
    let reply = &resent["messages"][1]["content"];
    assert_eq!(reply[0]["type"], "thinking");
    assert_eq!(
        reply[1],
        json!({"type": "text", "text": "[web search: \"wisp terminal\"]"})
    );
    assert_eq!(
        reply[2],
        json!({"type": "text", "text": "1. Wisp \u{2014} https://example.com/wisp\n2. Wisp docs \u{2014} https://example.com/wisp/docs"})
    );
    assert_eq!(reply[3]["type"], "thinking");
    assert_eq!(
        reply[4],
        json!({"type": "text", "text": "Wisp is a terminal assistant."})
    );
    let later: Value = serde_json::from_str(&bodies[3]).unwrap();
    assert_eq!(later["messages"][1], resent["messages"][1]);
}
