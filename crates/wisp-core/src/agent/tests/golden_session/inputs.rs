use wisp_protocol::Effort;

use super::*;

#[tokio::test]
async fn a_multi_input_turn_then_an_image_input() {
    let script = vec![
        says("msg_01", 400, "Hello.")
            .then(ClientCommand::queue("q1", "One more thing."))
            .then(ClientCommand::queue("q2", "And another.")),
        says("msg_02", 520, "Both noted."),
        says("msg_03", 640, "A red square."),
    ];
    let rig = Rig::new("inputs_multi_image", Setup::default(), script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.send(with_images("u3", "What is this?", vec![image()]))
        .await;
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn an_effort_change_between_turns() {
    let script = vec![
        says("msg_01", 400, "Hello."),
        says("msg_02", 520, "Thinking harder."),
        says("msg_03", 640, "Still here."),
    ];
    let rig = Rig::new("effort_change", Setup::default(), script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.send(ClientCommand::shortcut(
        request(1),
        None,
        None,
        Some(Effort::High),
    ))
    .await;
    let changed = live.until(|e| is_answer(e, 1)).await;
    assert!(
        matches!(changed, AgentEvent::SettingsChanged { .. }),
        "{changed:?}"
    );
    live.completed("u2", "think harder").await;
    live.completed("u3", "again").await;
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn a_context_report_with_the_token_counter() {
    let script = vec![says("msg_01", 400, "Hello."), says("msg_02", 520, "Again.")];
    let rig = Rig::new("context_counted", Setup::default(), script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.counted_report(1).await;
    live.completed("u2", "again").await;
    live.close().await;
    rig.check();
}
