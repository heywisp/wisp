use crate::session::REDACTED;
use crate::test_guard::ScratchDir;

use super::*;

const SECRET: &str = "v1-gw-path";

pub(super) fn scratch() -> ScratchDir {
    ScratchDir::new("wisp-compact-test-", "log")
}

fn echo_call(id: &str, text: &str, input: u64) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(tool_use(
        0,
        id,
        "echo",
        &[&json!({ "text": text }).to_string()],
    ));
    script.extend(finish(StopReason::ToolUse, input, 0, 20));
    Step::from(script)
}

#[tokio::test]
async fn the_head_and_block_are_logged_byte_for_byte_past_the_redactions() {
    let store = MemoryStore::default();
    let summary = format!("So far: the {SECRET} route.");
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(&summary),
        answer("Again.", 300),
    ]);
    let context = LogContext {
        redactions: Redactions::new([SECRET.to_owned()]),
        ..log_context()
    };
    let agent = Agent::new(provider, default_session())
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context);
    let mut live = Live::start(agent);
    live.turn("u1", &format!("look at {SECRET}")).await;
    live.compact(2, Some(&format!("the {SECRET} work"))).await;
    live.turn("u2", "next").await;
    let (agent, _) = live.close().await;
    let head = agent.compaction.head.as_ref().unwrap();
    assert!(head_text(&agent).contains(SECRET));
    let logged = log_events(&store);
    let compacted: Vec<&LogEvent> = logged
        .iter()
        .filter(|e| matches!(e, LogEvent::ContextCompacted { .. }))
        .collect();
    let [
        LogEvent::ContextCompacted {
            head: logged_head,
            block: Some(block),
            focus: Some(focus),
            ..
        },
    ] = compacted.as_slice()
    else {
        panic!("{compacted:?}");
    };
    assert_eq!(logged_head, &head.message);
    assert_eq!(block, &raw_block(&summary));
    assert_eq!(focus, &format!("the {REDACTED} work"));
    let rebuilt = rebuild_history(logged.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

#[tokio::test]
async fn two_compactions_with_tools_rebuild_the_same_history_and_view() {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        echo_call("t1", "one", 1_000),
        answer("Done one.", 1_100),
        compaction(SUMMARY),
        echo_call("t2", "two", 1_200),
        answer("Done two.", 1_300),
        compaction("Second summary."),
        answer("Third.", 1_400),
    ]);
    let agent = logged(provider.clone(), &store)
        .with_tools(crate::tool::test_toolbox(), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(matches!(
        live.compact(2, None).await,
        AgentEvent::Compacted { .. }
    ));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(matches!(
        live.compact(3, None).await,
        AgentEvent::Compacted { .. }
    ));
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    let last = provider.body(6);
    let blocks = last["messages"]
        .to_string()
        .matches("\"compaction\"")
        .count();
    assert_eq!(blocks, 1, "{last}");
    assert_eq!(
        last["messages"][0]["content"][0],
        raw_block("Second summary.")
    );
    let compacting = provider.body(5);
    assert_eq!(compacting["messages"][0]["content"][0], raw_block(SUMMARY));
    let text = head_text(&agent);
    assert!(text.contains("<environment>"), "{text}");
    assert!(text.contains("echo"), "{text}");
    let rebuilt = rebuild_history(log_events(&store).iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}
