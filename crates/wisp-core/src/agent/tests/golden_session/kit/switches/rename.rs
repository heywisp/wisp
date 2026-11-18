use wisp_tool_text::chat as text;

use super::*;

fn rename(id: &'static str, name: &str) -> Part {
    call(
        id,
        "use",
        json!({"name": "rename_chat", "input": {"name": name}}),
    )
}

#[tokio::test]
async fn the_agent_renames_through_use_without_changing_its_request_prefix() {
    let env = Env::new("rename-chat");
    let server = Server::start(vec![
        calls("msg_rename", 400, vec![rename("toolu_rename", "Strada")]),
        says("msg_01", 500, "Done."),
        calls("msg_empty", 600, vec![rename("toolu_empty", "\"\"")]),
        says("msg_02", 700, "Empty."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    let first = session.turn("u1", "Rename this chat to Strada.").await;
    let records = store.read("s1").unwrap();
    let names: Vec<_> = records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| match &record.event {
            LogEvent::SessionNamed {
                name,
                usage,
                model,
                cost_usd,
                list_usd,
                detail,
                lower_bound,
            } => {
                assert!(
                    usage.is_none()
                        && model.is_none()
                        && cost_usd.is_none()
                        && list_usd.is_none()
                        && detail.is_none()
                        && !lower_bound
                );
                Some((index, name.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].1.as_deref(), Some("Strada"));
    let result = records.iter().position(|record| matches!(&record.event,
        LogEvent::ToolCallFinished { call_id, result: Some(result), .. } if call_id == "toolu_rename" && result == &text::renamed("Strada"))).unwrap();
    assert!(names[0].0 < result);
    assert!(first.iter().any(|event| matches!(event, AgentEvent::ToolCallFinished { call_id, result: Some(result), .. } if call_id == "toolu_rename" && result == &text::renamed("Strada"))));
    appends(&server, 0);
    let before = body(&server, 0);
    let after = body(&server, 1);
    let old = before["messages"].as_array().unwrap().len();
    assert_eq!(after["messages"].as_array().unwrap().len(), old + 2);
    assert_eq!(after["messages"][old]["content"][0]["name"], "use");
    assert!(result_text(&after, "toolu_rename").contains(&text::renamed("Strada")));
    assert!(!before["tools"].to_string().contains("rename_chat"));
    let second = session.turn("u2", "Rename it to quotes.").await;
    appends(&server, 1);
    appends(&server, 2);
    assert!(second.iter().any(|event| matches!(event, AgentEvent::ToolCallFinished { call_id, outcome: ToolOutcome::Failed { .. }, result: Some(reason), .. } if call_id == "toolu_empty" && reason == text::EMPTY)));
    assert_eq!(
        store
            .read("s1")
            .unwrap()
            .iter()
            .filter(|record| matches!(record.event, LogEvent::SessionNamed { .. }))
            .count(),
        1
    );
    session.end().await;
}
