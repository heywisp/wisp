use wisp_protocol::HistoryChange;

use super::*;
use crate::agent::testing::dropping;

#[tokio::test]
async fn a_grown_drop_of_wisps_own_thinking_is_noted_once() {
    let model = [("messages.1.content.0", "model_binding_mismatch")];
    let mut rig = rig(vec![
        done(&[B::Think("t1"), B::Text("a")]),
        dropping(&[B::Text("b")], &model),
        dropping(&[B::Text("c")], &model),
    ]);
    let (_, first) = rig.turn("one").await;
    let (_, second) = rig.turn("two").await;
    let (_, third) = rig.turn("three").await;
    assert!(adaptations(&first).is_empty());
    assert_eq!(
        adaptations(&second),
        vec![(
            None,
            HistoryChange::ThinkingDropped {
                count: 1,
                reason: Some("model_binding_mismatch".into()),
            }
        )]
    );
    assert!(adaptations(&third).is_empty());
}

#[tokio::test]
async fn a_drop_where_wisp_sent_no_thinking_is_left_to_the_injected_thinking_notice() {
    let inserted = [("messages.1.content.0", "prefix_binding_mismatch")];
    let mut rig = rig(vec![
        done(&[B::Text("a")]),
        dropping(&[B::Text("b")], &inserted),
    ]);
    rig.turn("one").await;
    let (_, events) = rig.turn("two").await;
    assert!(adaptations(&events).is_empty(), "{events:?}");
}

#[tokio::test]
async fn an_installed_compaction_clears_the_covered_drops() {
    let mut rig = rig(vec![done(&[B::Text("a")]), compacted_step()]);
    rig.turn("one").await;
    rig.agent.search.drops_covered = true;
    let events = compact(&mut rig).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{events:#?}"
    );
    assert!(!rig.agent.search.drops_covered);
}
