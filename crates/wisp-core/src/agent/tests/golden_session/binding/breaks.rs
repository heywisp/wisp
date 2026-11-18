use super::*;

const WISP_ENGINE: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";

fn engines() -> [Build; 2] {
    [
        Build::default(),
        Build {
            models: Some(WISP_ENGINE),
            ..Build::default()
        },
    ]
}

async fn kept_then_compacted(case: &str, stage: bool, build: Build) -> (Bench, Live) {
    let bench = Bench::new(case, stage).await;
    bench.api.lock().unwrap().refuse_binding_for = Some(GATEWAY_KEY);
    let mut live = bench.start_with(build, bench.both());
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    let later = Build {
        tools: MORE_TOOLS,
        ..build
    };
    let mut live = bench.resume_with(later, bench.both());
    live.completed("u3", "Keep going.").await;
    live.compacted(1).await;
    live.completed("u4", "After the summary.").await;
    (bench, live)
}

fn thinking_sent(body: &str) -> usize {
    let body: Value = serde_json::from_str(body).unwrap();
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(content)
        .filter(thought)
        .count()
}

async fn moved_off_binding(bench: &Bench, live: &mut Live, request: u64) {
    let before = bench.settled();
    switch_endpoint(live, request, "gw").await;
    two_turns(live, 5).await;
    let ok = settled(&bench.served());
    assert!(
        thinking_sent(&ok[before].body) > 0,
        "{}: the thinking written since the compaction was cut",
        bench.case
    );
    assert!(
        ok[before..].iter().all(|s| !binds(&s.body)),
        "{}",
        bench.case
    );
}

#[tokio::test]
async fn a_compaction_after_a_kept_break_keeps_later_thinking_on_a_pair_without_binding() {
    for build in engines() {
        for stage in [false, true] {
            let (bench, mut live) = kept_then_compacted("breaks-compacted", stage, build).await;
            moved_off_binding(&bench, &mut live, 1).await;
            let agent = finish(&bench, live).await;
            assert!(
                bench.cuts().is_empty(),
                "{}: {:?}",
                bench.case,
                bench.cuts()
            );
            assert!(!agent.carries_kept_break(), "{}", bench.case);
        }
    }
}

#[tokio::test]
async fn a_resume_after_the_compaction_reaches_the_live_state() {
    for build in engines() {
        let (bench, live) = kept_then_compacted("breaks-resumed", false, build).await;
        let live_agent = finish(&bench, live).await;
        let later = Build {
            tools: MORE_TOOLS,
            ..build
        };
        let resumed = finish(&bench, bench.resume_with(later, bench.both())).await;
        assert!(live_agent.search.kept_break.is_some(), "{}", bench.case);
        assert_eq!(
            resumed.search.kept_break, live_agent.search.kept_break,
            "{}",
            bench.case
        );
        assert_eq!(
            resumed.carries_kept_break(),
            live_agent.carries_kept_break(),
            "{}",
            bench.case
        );
        let mut live = bench.resume_with(later, bench.both());
        moved_off_binding(&bench, &mut live, 1).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
    }
}

#[tokio::test]
async fn a_rewind_past_the_compaction_and_the_break_still_cuts_once() {
    for build in engines() {
        let (bench, mut live) = kept_then_compacted("breaks-rewound", false, build).await;
        live.send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(9),
        })
        .await;
        let done = live
            .until(|e| {
                matches!(e, AgentEvent::Rewound { .. }) || matches!(e, AgentEvent::Rejected { .. })
            })
            .await;
        assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
        switch_endpoint(&mut live, 1, "gw").await;
        two_turns(&mut live, 5).await;
        let agent = finish(&bench, live).await;
        let cuts = bench.cuts();
        assert_eq!(cuts.len(), 1, "{}: {cuts:?}", bench.case);
        assert_eq!(
            cuts[0].0,
            BlockPosition {
                message: 0,
                block: 0
            }
        );
        assert_eq!(agent.search.kept_break, None);
    }
}

#[test]
fn a_leading_head_after_a_kept_break_keeps_its_kept_thinking_off_binding() {
    crate::stack_alarm::on_alarm_stack(crate::stack_alarm::Timer::Real, || async {
        let bench = Bench::new("breaks-leading", false).await;
        bench.api.lock().unwrap().refuse_binding_for = Some(GATEWAY_KEY);
        let first = Build {
            tools: MORE_TOOLS,
            ..Build::default()
        };
        let mut live = bench.start(first);
        live.completed("u1", "Look around.").await;
        finish(&bench, live).await;
        let kept = bench.settled();
        let banded = Build {
            band: true,
            models: Some("[models.\"claude-opus-5-5\"]\ncompact_at = 15000\n"),
            ..Build::default()
        };
        let mut live = bench.resume(banded);
        two_turns(&mut live, 2).await;
        let (agent, _) = live.close().await;
        let head = agent.compaction.head.as_ref().unwrap();
        assert!(head.leading(), "{}", bench.case);
        assert!(
            agent
                .search
                .kept_break
                .is_some_and(|until| until <= head.cut)
        );
        assert!(!agent.carries_kept_break());
        let away = bench.settled();
        let served_before = bench.served().len();
        let mut live = bench.resume_with(Build::default(), bench.both());
        switch_endpoint(&mut live, 1, "gw").await;
        two_turns(&mut live, 4).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        let served = bench.served();
        let refused: Vec<&Served> = served[served_before..]
            .iter()
            .filter(|s| s.status != 200)
            .collect();
        assert!(
            refused.len() == 1 && binds(&refused[0].body),
            "{}: a request after the swap was refused for more than the gateway's binding controls",
            bench.case
        );
        let ok = settled(&served);
        assert!(ok[away..].iter().all(|s| !binds(&s.body)), "{}", bench.case);
        assert!(
            ok[away..].iter().any(|s| sends_thinking(&s.body)),
            "{}: the kept thinking was not sent",
            bench.case
        );
        let mut changes = compactions(&ok);
        changes.extend([kept, away]);
        changes.sort_unstable();
        changes.dedup();
        assert_settles(&bench, &changes);
    });
}

fn swap_request(system: &str, messages: Value, compaction: bool) -> String {
    let mut request = json!({
        "model": OPUS,
        "thinking": {"type": "adaptive"},
        "system": [{"type": "text", "text": system}],
        "tools": [],
        "messages": messages
    });
    if compaction {
        request["compaction"] = json!({"type": "summary"});
    }
    request.to_string()
}

fn after_the_swap(system: &str) -> String {
    let asked = json!([{"role": "user", "content": [{"type": "text", "text": "Look around."}]}]);
    let mut api = Api::default();
    let answered = api.answer("", &swap_request("a", asked.clone(), false));
    assert!(answered.starts_with("HTTP/1.1 200"), "{answered}");
    let compacted = api.answer("", &swap_request("a", asked, true));
    assert!(compacted.starts_with("HTTP/1.1 200"), "{compacted}");
    let swapped = json!([
        {"role": "assistant", "content": [{"type": "compaction", "content": compaction_content(1)}]},
        {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "Thought 0.", "signature": "sig-0"},
            {"type": "text", "text": "Done 0."}
        ]},
        {"role": "user", "content": [{"type": "text", "text": "Next."}]}
    ]);
    api.answer("", &swap_request(system, swapped, false))
}

#[test]
fn the_fake_accepts_thinking_kept_after_a_native_swap() {
    let answered = after_the_swap("a");
    assert!(answered.starts_with("HTTP/1.1 200"), "{answered}");
}

#[test]
fn the_fake_refuses_kept_thinking_under_a_changed_system_with_the_apis_wording() {
    let answered = after_the_swap("b");
    assert!(answered.starts_with("HTTP/1.1 400"), "{answered}");
    assert!(
        answered.contains(&format!("messages.1.content.0: {BOUND_ELSEWHERE}")),
        "{answered}"
    );
}

fn searched_after_the_swap(first: bool) -> String {
    let asked = json!([{"role": "user", "content": [{"type": "text", "text": "Look around."}]}]);
    let mut api = Api {
        tools_per_turn: 1,
        search_once: true,
        ..Api::default()
    };
    let answered = api.answer("", &swap_request("a", asked.clone(), false));
    assert!(answered.contains("srvtoolu_00"), "{answered}");
    let compacted = api.answer("", &swap_request("a", asked, true));
    assert!(compacted.starts_with("HTTP/1.1 200"), "{compacted}");
    let mut kept = vec![
        json!({"type": "server_tool_use", "id": "srvtoolu_00", "name": "web_search", "input": {"query": "rust"}}),
        json!({"type": "thinking", "thinking": "Thought 0, after.", "signature": "sig-0-after"}),
        json!({"type": "text", "text": "Done."}),
    ];
    if first {
        kept.insert(
            0,
            json!({"type": "thinking", "thinking": "Thought 0.", "signature": "sig-0"}),
        );
    }
    let swapped = json!([
        {"role": "assistant", "content": [{"type": "compaction", "content": compaction_content(1)}]},
        {"role": "assistant", "content": kept},
        {"role": "user", "content": [{"type": "text", "text": "Next."}]}
    ]);
    api.answer("", &swap_request("a", swapped, false))
}

fn kept_after_a_second_compaction(stale: bool) -> String {
    let user = |text: &str| json!({"role": "user", "content": [{"type": "text", "text": text}]});
    let reply = |n: usize| {
        json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": format!("Thought {n}."), "signature": format!("sig-{n}")},
            {"type": "text", "text": format!("Done {n}.")}
        ]})
    };
    let first = json!([user("Look around.")]);
    let second = json!([user("Look around."), reply(0), user("More.")]);
    let mut api = Api::default();
    for (messages, compaction) in [
        (&first, false),
        (&first, true),
        (&second, false),
        (&second, true),
    ] {
        let answered = api.answer("", &swap_request("a", messages.clone(), compaction));
        assert!(answered.starts_with("HTTP/1.1 200"), "{answered}");
    }
    let block = compaction_content(if stale { 1 } else { 3 });
    let swapped = json!([
        {"role": "assistant", "content": [{"type": "compaction", "content": block}]},
        reply(2),
        user("Next.")
    ]);
    api.answer("", &swap_request("a", swapped, false))
}

#[test]
fn the_fake_accepts_thinking_kept_after_the_latest_compaction_block() {
    let answered = kept_after_a_second_compaction(false);
    assert!(answered.starts_with("HTTP/1.1 200"), "{answered}");
}

#[test]
fn the_fake_refuses_thinking_kept_after_a_stale_compaction_block() {
    let answered = kept_after_a_second_compaction(true);
    assert!(answered.starts_with("HTTP/1.1 400"), "{answered}");
    assert!(
        answered.contains(&format!("messages.1.content.0: {BOUND_ELSEWHERE}")),
        "{answered}"
    );
}

#[test]
fn the_fake_accepts_a_kept_replys_second_thinking_after_its_first() {
    let answered = searched_after_the_swap(true);
    assert!(answered.starts_with("HTTP/1.1 200"), "{answered}");
}

#[test]
fn the_fake_refuses_a_kept_replys_second_thinking_once_its_first_is_dropped() {
    let answered = searched_after_the_swap(false);
    assert!(answered.starts_with("HTTP/1.1 400"), "{answered}");
    assert!(
        answered.contains(&format!("messages.1.content.1: {BOUND_ELSEWHERE}")),
        "{answered}"
    );
}

#[tokio::test]
async fn a_prompt_change_found_at_resume_cuts_once_off_binding() {
    let bench = Bench::new("breaks-resumed-prompt", false).await;
    bench.api.lock().unwrap().refuse_binding_for = Some(GATEWAY_KEY);
    let mut live = bench.start_with(Build::default(), bench.both());
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    let later = Build {
        system: Some("You are Wisp, a careful assistant."),
        ..Build::default()
    };
    let mut live = bench.resume_with(later, bench.both());
    live.completed("u3", "Keep going.").await;
    assert!(bench.cuts().is_empty(), "{}", bench.case);
    switch_endpoint(&mut live, 1, "gw").await;
    two_turns(&mut live, 4).await;
    let agent = finish(&bench, live).await;
    let cuts = bench.cuts();
    assert_eq!(cuts.len(), 1, "{}: {cuts:?}", bench.case);
    assert_eq!(cuts[0].0, WHOLE);
    assert_eq!(agent.search.kept_break, None);
    assert!(
        bench
            .served()
            .iter()
            .filter(|s| s.status != 200)
            .all(|s| binds(&s.body)),
        "{}: a request failed for more than binding",
        bench.case
    );
}
