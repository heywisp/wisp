use super::*;
use wisp_protocol::RenderingForm;

async fn stopped_then_refused(stage: bool) -> (Bench, usize) {
    let bench = Bench::new("forms-tool-turn", stage).await;
    let build = Build {
        search: true,
        ..Build::default()
    };
    let mut live = bench.start(build);
    two_turns(&mut live, 1).await;
    bench.api.lock().unwrap().refuse_rendered = 1;
    let change = bench.settled() + 1;
    stopped_in_a_search(&bench, &mut live, "u3").await;
    two_turns(&mut live, 4).await;
    live.completed("u6", "Once more.").await;
    let agent = finish(&bench, live).await;
    assert_eq!(
        agent.search.form,
        RenderingForm::WithoutThinking,
        "{}",
        bench.case
    );
    (bench, change)
}

fn calls_answered(body: &str) -> bool {
    let request: Value = serde_json::from_str(body).unwrap();
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    messages.iter().enumerate().all(|(i, message)| {
        let results: Vec<Value> = messages
            .get(i + 1)
            .map(content)
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b["type"] == "tool_result")
            .map(|b| b["tool_use_id"].clone())
            .collect();
        content(message)
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .all(|b| results.contains(&b["id"]))
    })
}

#[tokio::test]
async fn a_rendered_tool_turn_without_thinking_is_sent_flat_and_settles() {
    for stage in [false, true] {
        let (bench, change) = stopped_then_refused(stage).await;
        let served = bench.served();
        assert_eq!(
            served.iter().filter(|s| s.status != 200).count(),
            1,
            "{}",
            bench.case
        );
        let ok = settled(&served);
        assert!(
            ok.iter().all(|s| calls_answered(&s.body)),
            "{}: a tool_use lost its result",
            bench.case
        );
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        assert_settles(&bench, &[change]);
    }
}
