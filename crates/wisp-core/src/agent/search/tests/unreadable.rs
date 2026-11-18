use wisp_protocol::RequestId;
use wisp_protocol::log::{BlockPosition, Cut};

use super::native::{listed, native_rig, refusal, resumed_with};
use super::retry_choice::serve_until_rewound;
use super::*;
use crate::agent::tests::completed;
use crate::agent::tests::encrypted;
use crate::clock::StillClock;
use crate::provider::Refused;
use crate::test_guard::models;

fn unreadable_at(message: usize) -> Step {
    refused(refusal(Refused::CrossAccount, &owner_error(message)))
}

fn owner_error(message: usize) -> String {
    format!("messages.{message}.content.0: Invalid `encrypted_content` in `search_result` block")
}

fn unreadable() -> Step {
    unreadable_at(1)
}

fn searched() -> Step {
    done(&[
        B::Think("t1"),
        B::Search("s1", "rust"),
        B::Found("s1"),
        B::Think("t2"),
        B::Cited("Rust is fast.", "https://example.com/s1"),
        B::Text("a"),
    ])
}

fn still(steps: Vec<Step>) -> Rig {
    let mut rig = native_rig(steps, &[DEFAULT_MODEL]);
    let placeholder = Agent::new(rig.provider.clone(), default_session());
    rig.agent = std::mem::replace(&mut rig.agent, placeholder).with_clock(Arc::new(StillClock));
    rig
}

fn resumed(
    rig: &Rig,
    records: &[LogRecord],
    steps: Vec<Step>,
) -> (Agent, Arc<Fake>, mpsc::Receiver<ClientCommand>) {
    let (tx, rx) = mpsc::channel(64);
    let provider = searching(steps, tx);
    let agent = Agent::new(provider.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(
            Toolbox::new(samples().into_iter().map(|s| s.tool)),
            rig._env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_model_settings(models(&listed("env", &[DEFAULT_MODEL])))
        .with_web_search(true);
    (
        resumed_with(agent, records, implicit_context()),
        provider,
        rx,
    )
}

async fn resumed_turn(
    rig: &Rig,
    records: &[LogRecord],
    steps: Vec<Step>,
    text: &str,
) -> (TurnOutcome, Value) {
    let (mut agent, provider, mut rx) = resumed(rig, records, steps);
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    let outcome = agent.run_turn(text.into(), &mut rx, &ev_tx).await.unwrap();
    (outcome, provider.bodies()[0].clone())
}

fn text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

fn searched_text() -> Value {
    json!([
        {"type": "thinking", "thinking": "t1", "signature": "sig-t1"},
        text("[web search: \"rust\"]"),
        text("1. Page s1 \u{2014} https://example.com/s1"),
        text("Rust is fast."),
        text("a"),
    ])
}

fn count(kinds: &[String], kind: &str) -> usize {
    kinds.iter().filter(|k| *k == kind).count()
}

#[tokio::test]
async fn an_unreadable_history_is_resent_once_as_text_and_later_requests_extend_it() {
    let mut rig = still(vec![
        searched(),
        unreadable(),
        done(&[B::Text("b")]),
        done(&[B::Text("c")]),
    ]);
    rig.turn("one").await;
    let committed = rig.agent.history().to_vec();
    let (outcome, _) = rig.turn("two").await;
    assert!(completed(&outcome), "{outcome:?}");
    let records = records(&rig.store);
    rig.turn("three").await;

    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 4);
    assert!(encrypted(&bodies[1]));
    assert!(bodies[2..].iter().all(|b| !encrypted(b)));
    assert_eq!(rig.messages(2)[1]["content"], searched_text());
    assert_eq!(rig.messages(3)[..2], rig.messages(2)[..2]);
    assert_eq!(rig.messages(3)[2]["content"], rig.messages(2)[2]["content"]);

    let kinds = rig.kinds();
    assert_eq!(count(&kinds, "search_history_unreadable"), 1);
    let record = position(&kinds, "search_history_unreadable")[0];
    let cut = position(&kinds, "thinking_cut");
    assert_eq!(cut, vec![record + 1]);
    let last_reply = kinds
        .iter()
        .rposition(|k| k == "message_committed")
        .unwrap();
    assert!(record < last_reply);
    assert_eq!(
        rig.agent.search.cuts,
        vec![Cut {
            from: BlockPosition {
                message: 1,
                block: 1,
            },
            through: 1,
            tools: false,
        }]
    );
    assert_eq!(rig.agent.search.unreadable.get(IMPLICIT_ENDPOINT), Some(&2));
    assert_eq!(rig.agent.history()[..2], committed[..]);
    assert!(
        serde_json::to_string(&rig.agent.history()[1])
            .unwrap()
            .contains("enc-s1")
    );

    let (outcome, first) = resumed_turn(&rig, &records, vec![done(&[B::Text("c")])], "three").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(first, bodies[3]);
}

#[tokio::test]
async fn a_refusal_with_nothing_left_to_convert_climbs_the_ladder_and_ends_with_the_resends_error()
{
    let mut rig = still(vec![
        searched(),
        unreadable_at(1),
        unreadable_at(2),
        unreadable_at(3),
        unreadable_at(4),
    ]);
    rig.turn("one").await;
    let (outcome, _) = rig.turn("two").await;
    let TurnOutcome::Error { kind, message, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::InvalidRequest);
    assert_eq!(*message, owner_error(2));
    assert_eq!(rig.sent(), 5);
    let kinds = rig.kinds();
    assert_eq!(count(&kinds, "turn_ended"), 2);
    assert_eq!(count(&kinds, "search_history_unreadable"), 1);
    assert_eq!(count(&kinds, "rendering_form"), 0);
    assert_eq!(rig.agent.search.form, wisp_protocol::RenderingForm::Text);
}

#[tokio::test]
async fn a_search_after_the_record_stays_native_until_a_second_refusal_moves_the_boundary() {
    let mut rig = still(vec![
        searched(),
        unreadable(),
        done(&[B::Search("s2", "more"), B::Found("s2"), B::Text("b")]),
        done(&[B::Text("c")]),
        unreadable_at(3),
        done(&[B::Text("d")]),
    ]);
    for turn in ["one", "two", "three", "four"] {
        let (outcome, _) = rig.turn(turn).await;
        assert!(completed(&outcome), "{turn}: {outcome:?}");
    }
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 6);
    assert_eq!(rig.messages(3)[1]["content"], searched_text());
    assert_eq!(rig.messages(3)[3]["content"][0]["type"], "server_tool_use");
    assert!(encrypted(&bodies[3]));
    assert!(!encrypted(&bodies[5]));
    assert_eq!(rig.messages(5)[1], rig.messages(3)[1]);
    assert_eq!(
        rig.messages(5)[3]["content"][0],
        text("[web search: \"more\"]")
    );
    assert_eq!(count(&rig.kinds(), "search_history_unreadable"), 2);
    assert_eq!(rig.agent.search.unreadable.get(IMPLICIT_ENDPOINT), Some(&6));
}

#[tokio::test]
async fn a_search_after_a_rewind_to_before_the_boundary_is_sent_native_live_and_resumed() {
    let mut rig = still(vec![
        searched(),
        unreadable(),
        done(&[B::Text("b")]),
        done(&[B::Search("s3", "again"), B::Found("s3"), B::Text("found")]),
        done(&[B::Text("later")]),
    ]);
    rig.turn("one").await;
    rig.turn("two").await;
    let back = serve_until_rewound(
        &mut rig,
        ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 1 },
            request: RequestId(7),
        },
    )
    .await;
    assert!(
        back.iter().any(|e| matches!(e, AgentEvent::Rewound { .. })),
        "{back:#?}"
    );
    assert_eq!(rig.agent.search.unreadable.get(IMPLICIT_ENDPOINT), Some(&0));
    rig.turn("again").await;
    let records = records(&rig.store);
    let (outcome, _) = rig.turn("later").await;
    assert!(completed(&outcome), "{outcome:?}");
    let live = rig.provider.bodies().last().unwrap().clone();
    assert_eq!(live["messages"][1]["content"][0]["type"], "server_tool_use");
    assert!(encrypted(&live));

    let (outcome, first) =
        resumed_turn(&rig, &records, vec![done(&[B::Text("later")])], "later").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(first, live);
}

#[tokio::test]
async fn a_refused_pause_continuation_closes_the_tail_first_and_ends_the_turn() {
    let mut rig = still(vec![
        paused(&[B::Search("s1", "rust"), B::Found("s1")]),
        unreadable(),
        done(&[B::Text("b")]),
    ]);
    let (outcome, _) = rig.turn("one").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::PauseTurn,
                ..
            }
        ),
        "{outcome:?}"
    );
    rig.turn("two").await;
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed");
    let record = position(&kinds, "search_history_unreadable");
    assert_eq!((closed.len(), record.len()), (1, 1));
    assert!(closed[0] < record[0]);
    assert_eq!(rig.sent(), 3);
    let last = rig.provider.bodies()[2].clone();
    assert!(!encrypted(&last));
    assert_eq!(
        rig.messages(2)[1]["content"][0],
        text("[web search: \"rust\"]")
    );
}

#[tokio::test]
async fn a_failed_turn_after_the_record_resumes_already_converted() {
    let mut rig = still(vec![searched(), unreadable(), refused(rejected())]);
    rig.turn("one").await;
    let (outcome, _) = rig.turn("two").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(rig.sent(), 3);
    assert!(!encrypted(&rig.provider.bodies()[2]));
    let records = records(&rig.store);
    let (outcome, first) = resumed_turn(&rig, &records, vec![done(&[B::Text("b")])], "two").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(!encrypted(&first));
    assert_eq!(first["messages"][1]["content"], searched_text());
}

#[tokio::test]
async fn an_unreadable_history_says_once_that_searches_go_as_text_with_the_thinking_cut() {
    let mut rig = still(vec![
        searched(),
        unreadable(),
        done(&[B::Text("b")]),
        done(&[B::Text("c")]),
    ]);
    let (_, first) = rig.turn("one").await;
    let (outcome, second) = rig.turn("two").await;
    assert!(completed(&outcome), "{outcome:?}");
    let (_, third) = rig.turn("three").await;
    assert!(adaptations(&first).is_empty());
    assert_eq!(
        adaptations(&second),
        vec![(
            None,
            wisp_protocol::HistoryChange::SearchesAsText { thinking_cut: true }
        )]
    );
    assert!(adaptations(&third).is_empty());
}
