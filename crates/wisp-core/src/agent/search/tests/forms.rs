use wisp_protocol::RenderingForm;
use wisp_protocol::log::BlockPosition;

use super::*;
use crate::provider::{Feature, Refused};

fn refusal(kind: ErrorKind, message: &str) -> ProviderError {
    classified(kind, message, FailurePhase::Response, RetryClass::Never)
        .refusing(Refused::Rendering)
}

fn refused_rendering(message: &str) -> Step {
    refused(refusal(ErrorKind::InvalidRequest, message))
}

fn roles(messages: &[Value]) -> Vec<String> {
    messages
        .iter()
        .map(|m| m["role"].as_str().unwrap().to_owned())
        .collect()
}

fn assert_alternates(messages: &[Value]) {
    let roles = roles(messages);
    assert_eq!(roles.first().map(String::as_str), Some("user"), "{roles:?}");
    for pair in roles.windows(2) {
        assert_ne!(pair[0], pair[1], "{roles:?}");
    }
}

fn text(value: &str) -> Value {
    json!({"type": "text", "text": value})
}

fn echo_input() -> String {
    let input = samples()
        .into_iter()
        .find(|s| s.tool.spec().name == "echo")
        .unwrap()
        .input;
    serde_json::to_string(&input).unwrap()
}

fn called_echo() -> String {
    format!("[called echo: {}]", echo_input())
}

fn result_of(body_message: &Value, block: usize) -> String {
    let result = &body_message["content"][block];
    assert_eq!(result["type"], "tool_result");
    let text: Vec<&str> = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["text"].as_str().unwrap())
        .collect();
    let label = if result["is_error"] == true {
        "error from"
    } else {
        "result of"
    };
    format!("[{label} echo: {}]", text.join("\n"))
}

fn closed_line(query: &str) -> String {
    format!("[web search for \"{query}\": the turn ended before its result arrived]")
}

fn hit_line(id: &str) -> String {
    format!("1. Page {id} \u{2014} https://example.com/{id}")
}

fn rebuilt_with_form(rig: &Rig) -> Value {
    let (messages, mut rendering) = rebuilt_rendering(rig);
    if let Some(LogEvent::RenderingForm { form }) = records_of(rig, "rendering_form").last() {
        rendering.form = *form;
    }
    body_of(rig, &messages, &rendering)
}

fn assert_records_rebuild_the_live_bytes(rig: &Rig) {
    let live = rig.agent.rendering(rig.agent.history().len());
    assert_eq!(
        rebuilt_with_form(rig),
        body_of(rig, &rig.agent.history(), &live)
    );
}

#[tokio::test]
async fn a_refused_rendering_is_resent_without_its_thinking_and_kept() {
    let mut rig = rig(vec![
        stopped_with_thinking(),
        refused_rendering("form 1 refused"),
        done(&[B::Think("fresh"), B::Text("ok")]),
        done(&[B::Text("ok again")]),
    ]);
    rig.turn("go").await;
    let (outcome, _) = rig.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 3);
    let first = rig.messages(1);
    let second = rig.messages(2);
    assert_eq!(
        first[1]["content"][0],
        json!({"type": "thinking", "thinking": "before", "signature": "sig-before"})
    );
    let tool_use = first[1]["content"][2].clone();
    assert_eq!(tool_use["type"], "tool_use");
    assert_eq!(second[1]["content"], json!([closed_text("rust"), tool_use]));
    assert_eq!(second[0], first[0]);
    assert_eq!(second[2], first[2]);
    assert_eq!(rig.agent.search.form, RenderingForm::WithoutThinking);
    assert!(rig.agent.search.trial.is_none());

    let kinds = rig.kinds();
    let form = position(&kinds, "rendering_form");
    assert_eq!(form.len(), 1);
    assert_eq!(
        records_of(&rig, "rendering_form"),
        vec![LogEvent::RenderingForm {
            form: RenderingForm::WithoutThinking
        }]
    );
    let cuts = position(&kinds, "thinking_cut");
    assert_eq!(cuts.len(), 2, "the closure's cut and the form's");
    assert_eq!(cuts[1], form[0] + 1);
    let LogEvent::ThinkingCut { from, through, .. } = &records(&rig.store)[cuts[1]].event else {
        panic!();
    };
    assert_eq!(
        *from,
        BlockPosition {
            message: 1,
            block: 0
        }
    );
    assert_eq!(*through, 2);
    let second_turn = position(&kinds, "turn_started")[1];
    let committed: Vec<usize> = position(&kinds, "message_committed")
        .into_iter()
        .filter(|i| *i > second_turn)
        .collect();
    assert!(form[0] < committed[0], "{kinds:?}");
    assert_records_rebuild_the_live_bytes(&rig);

    rig.turn("again").await;
    let third = rig.messages(3);
    assert_eq!(third[..second.len()], second[..]);
    assert_eq!(
        third[3]["content"][0],
        json!({"type": "thinking", "thinking": "fresh", "signature": "sig-fresh"}),
        "thinking after the form's cut is kept"
    );
    assert_records_rebuild_the_live_bytes(&rig);
}

#[tokio::test]
async fn a_failed_escalation_keeps_the_form_and_ends_with_the_original_error() {
    let mut rig = rig(vec![
        stopped_with_thinking(),
        refused_rendering("the first refusal"),
        refused_rendering("the second refusal"),
        refused_rendering("the third refusal"),
        done(&[B::Text("ok")]),
    ]);
    rig.turn("go").await;
    let cuts_before = rig.agent.search.cuts.clone();
    let (outcome, _) = rig.turn("next").await;
    let TurnOutcome::Error { kind, message, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::InvalidRequest);
    assert!(message.contains("the first refusal"), "{message}");
    assert_eq!(rig.sent(), 4);
    assert_eq!(rig.agent.search.form, RenderingForm::Text);
    assert!(rig.agent.search.trial.is_none());
    assert_eq!(rig.agent.search.cuts, cuts_before, "no cut was kept");
    assert!(position(&rig.kinds(), "rendering_form").is_empty());
    assert_eq!(position(&rig.kinds(), "thinking_cut").len(), 1);
    assert_eq!(roles(&rig.messages(3)).len(), 3);
    assert_ne!(rig.messages(2)[1], rig.messages(1)[1], "form 2 was tried");
    assert_ne!(rig.messages(3)[1], rig.messages(2)[1], "form 3 was tried");

    rig.turn("again").await;
    let retried = rig.messages(4);
    assert_eq!(retried[..2], rig.messages(1)[..2], "back to form 1's bytes");
}

#[tokio::test]
async fn a_spend_limit_on_a_rendered_request_escalates_twice_then_ends_the_turn() {
    let limit = "You have reached your specified workspace API usage limits.";
    let mut rig = rig(vec![
        stopped_with_thinking(),
        refused_rendering(limit),
        refused_rendering(limit),
        refused_rendering(limit),
    ]);
    rig.turn("go").await;
    let (outcome, _) = rig.turn("next").await;
    let TurnOutcome::Error { message, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert!(message.contains(limit), "{message}");
    assert_eq!(rig.sent(), 4, "two re-sends after the first refusal");
    assert_eq!(rig.agent.search.form, RenderingForm::Text);
}

#[tokio::test]
async fn a_thinking_binding_refusal_on_a_rendered_request_escalates_and_keeps_form_2() {
    let mut rig = rig(vec![
        stopped_with_thinking(),
        refused(refusal(
            ErrorKind::ThinkingBinding,
            "Invalid `signature` in `thinking` block. The block is bound to a different conversation",
        )),
        done(&[B::Text("ok")]),
    ]);
    rig.turn("go").await;
    let (outcome, events) = rig.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(adaptations(&events).is_empty(), "{events:?}");
    assert_eq!(rig.agent.search.form, RenderingForm::WithoutThinking);
    assert_eq!(rig.agent.search.cuts.len(), 2);
    assert_eq!(rig.messages(2)[1]["content"][0], closed_text("rust"));
}

#[tokio::test]
async fn a_thinking_binding_refused_at_every_form_climbs_the_ladder_once_cuts_and_ends_with_its_kind_and_message()
 {
    let binding =
        "Invalid `signature` in `thinking` block. The block is bound to a different conversation";
    let mut steps = vec![stopped_with_thinking()];
    steps.extend((0..6).map(|_| refused(refusal(ErrorKind::ThinkingBinding, binding))));
    let mut rig = rig(steps);
    rig.turn("go").await;
    let (outcome, _) = rig.turn("next").await;
    let TurnOutcome::Error { kind, message, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::ThinkingBinding);
    assert!(
        message.contains("rejected earlier thinking blocks as bound to a different conversation"),
        "{message}"
    );
    assert!(message.contains(binding), "{message}");
    assert_eq!(rig.sent(), 5);
    assert_eq!(rig.agent.search.form, RenderingForm::Text);
    let whole: Vec<LogEvent> = records_of(&rig, "thinking_cut")
        .into_iter()
        .filter(|e| {
            matches!(e, LogEvent::ThinkingCut { from, .. } if *from == BlockPosition { message: 0, block: 0 })
        })
        .collect();
    assert_eq!(whole.len(), 1, "{whole:?}");
    let bodies = rig.provider.bodies();
    let thought = |b: &Value| b["messages"].to_string().contains("\"thinking\"");
    assert!(thought(&bodies[1]));
    assert!(bodies[4..].iter().all(|b| !thought(b)));
}

#[tokio::test]
async fn an_unrefused_error_on_a_rendered_request_takes_its_own_path() {
    for kind in [ErrorKind::ContextOverflow, ErrorKind::ThinkingBinding] {
        let error = classified(
            kind,
            "not a rendering",
            FailurePhase::Response,
            RetryClass::Never,
        );
        let mut rig = rig(vec![
            stopped_with_thinking(),
            refused(error.clone()),
            refused(error),
        ]);
        rig.turn("go").await;
        let (outcome, _) = rig.turn("next").await;
        let TurnOutcome::Error { kind: ended, .. } = &outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(*ended, kind);
        let cut_once = usize::from(kind == ErrorKind::ThinkingBinding);
        assert_eq!(rig.sent(), 2 + cut_once, "{kind:?} is never escalated");
        assert_eq!(rig.agent.search.form, RenderingForm::Text);
        assert!(position(&rig.kinds(), "rendering_form").is_empty());
    }
}

#[tokio::test]
async fn a_refusal_while_the_kept_form_is_the_last_ends_the_turn() {
    let mut rig = rig(vec![
        stopped_with_thinking(),
        refused_rendering("one"),
        refused_rendering("two"),
        done(&[B::Text("flat")]),
        refused_rendering("three"),
    ]);
    rig.turn("go").await;
    rig.turn("next").await;
    assert_eq!(rig.agent.search.form, RenderingForm::Flattened);
    let (outcome, _) = rig.turn("again").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(rig.sent(), 5);
    assert_eq!(rig.agent.search.form, RenderingForm::Flattened);
}

fn capable(inner: &Fake) -> Arc<Fake> {
    inner.view(|k| {
        k.caps = Some(Caps::Fn(crate::provider::anthropic::capabilities::row));
        k.documented = None;
        k.wire_print = false;
    })
}

fn binding_rejected() -> ProviderError {
    classified(
        ErrorKind::InvalidRequest,
        "thinking.block_binding: Extra inputs are not permitted",
        FailurePhase::Response,
        RetryClass::Never,
    )
    .rejecting(vec![Feature::BindingControls])
}

#[tokio::test]
async fn a_pair_fallen_back_from_binding_controls_escalates_without_the_beta() {
    let mut rig = rig(vec![
        refused(binding_rejected()),
        stopped_with_thinking(),
        refused_rendering("form 1 refused"),
        done(&[B::Text("ok")]),
    ]);
    rig.agent.connections[0].provider = capable(&rig.provider);
    rig.agent.settings.binding_controls = true;
    rig.turn("go").await;
    let bodies = rig.provider.bodies();
    assert!(
        bodies[0]["thinking"].get("block_binding").is_some(),
        "the trial sent the beta"
    );
    let (outcome, _) = rig.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 4);
    for body in &bodies[1..] {
        assert!(body["thinking"].get("block_binding").is_none(), "{body}");
    }
    assert_eq!(rig.agent.search.form, RenderingForm::WithoutThinking);
    assert_eq!(bodies[3]["messages"][1]["content"][0], closed_text("rust"));
}

#[tokio::test]
async fn form_3_flattens_a_two_message_mixed_exchange() {
    let mut rig = rig(vec![
        uses(&[B::Think("plan"), B::Search("s1", "rust"), echo("e1")]),
        uses(&[
            B::Found("s1"),
            B::Text("found it"),
            B::Search("s2", "more"),
            echo("e2"),
        ])
        .then(ClientCommand::Cancel),
        refused_rendering("form 1 refused"),
        refused_rendering("form 2 refused"),
        done(&[B::Think("fresh"), B::Text("ok")]),
        done(&[B::Text("ok again")]),
    ]);
    rig.turn("go").await;
    assert_eq!(rig.agent.search.closed.calls, BTreeSet::from(["s2".into()]));
    rig.turn("next").await;
    assert_eq!(rig.sent(), 5);
    let original = rig.messages(2);
    let flat = rig.messages(4);
    let history = rig.agent.history();
    assert_eq!(history.len(), 6);
    let tail: Vec<Value> = original[4]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] != "tool_result")
        .cloned()
        .collect();
    let mut last_user = vec![text(&result_of(&original[4], 0))];
    last_user.extend(tail);
    let expected = json!([
        original[0],
        {"role": "assistant", "content": [text(&format!(
            "[web search: \"rust\"]\n{}",
            called_echo()
        ))]},
        {"role": "user", "content": [text(&result_of(&original[2], 0))]},
        {"role": "assistant", "content": [text(&format!(
            "{}\nfound it\n{}\n{}",
            hit_line("s1"),
            closed_line("more"),
            called_echo()
        ))]},
        {"role": "user", "content": last_user},
    ]);
    assert_eq!(Value::Array(flat.clone()), expected);
    assert_alternates(&flat);
    let sent = &history[..history.len() - 1];
    let flags = rig.agent.paused_flags(sent.len());
    check_layout(
        sent,
        &flags,
        &rig.agent.search.closed.calls,
        rig.provider.as_ref(),
    )
    .unwrap();
    assert_eq!(rig.agent.search.form, RenderingForm::Flattened);
    let LogEvent::ThinkingCut { from, through, .. } =
        records_of(&rig, "thinking_cut").last().unwrap().clone()
    else {
        panic!();
    };
    assert_eq!(
        from,
        BlockPosition {
            message: 1,
            block: 0
        }
    );
    assert_eq!(through, 4);
    assert_records_rebuild_the_live_bytes(&rig);

    rig.turn("again").await;
    let after = rig.messages(5);
    assert_eq!(after[..flat.len()], flat[..]);
    assert_eq!(
        after[5]["content"][0],
        json!({"type": "thinking", "thinking": "fresh", "signature": "sig-fresh"})
    );
    assert_records_rebuild_the_live_bytes(&rig);
}

#[tokio::test]
async fn form_3_joins_a_paused_reply_and_its_continuation() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "rust")]),
        uses(&[
            B::Found("s1"),
            B::Text("found it"),
            B::Search("s2", "more"),
            echo("e2"),
        ])
        .then(ClientCommand::Cancel),
        refused_rendering("form 1 refused"),
        refused_rendering("form 2 refused"),
        done(&[B::Text("ok")]),
    ]);
    rig.turn("go").await;
    rig.turn("next").await;
    let original = rig.messages(2);
    let flat = rig.messages(4);
    let tail: Vec<Value> = original[3]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] != "tool_result")
        .cloned()
        .collect();
    let mut last_user = vec![text(&result_of(&original[3], 0))];
    last_user.extend(tail);
    let expected = json!([
        original[0],
        {"role": "assistant", "content": [text(&format!(
            "[web search: \"rust\"]\n{}\nfound it\n{}\n{}",
            hit_line("s1"),
            closed_line("more"),
            called_echo()
        ))]},
        {"role": "user", "content": last_user},
    ]);
    assert_eq!(Value::Array(flat.clone()), expected);
    assert_alternates(&flat);
    let history = rig.agent.history();
    let sent = &history[..history.len() - 1];
    let flags = rig.agent.paused_flags(sent.len());
    check_layout(
        sent,
        &flags,
        &rig.agent.search.closed.calls,
        rig.provider.as_ref(),
    )
    .unwrap();
}

#[tokio::test]
async fn form_3_pairs_a_pause_with_no_open_call_by_the_paused_set() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "rust"), B::Found("s1"), B::Text("a")]),
        uses(&[B::Text("b"), B::Search("s2", "more"), echo("e2")]).then(ClientCommand::Cancel),
        refused_rendering("form 1 refused"),
        refused_rendering("form 2 refused"),
        done(&[B::Text("ok")]),
    ]);
    rig.turn("go").await;
    rig.turn("next").await;
    let original = rig.messages(2);
    let flat = rig.messages(4);
    let tail: Vec<Value> = original[3]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] != "tool_result")
        .cloned()
        .collect();
    let mut last_user = vec![text(&result_of(&original[3], 0))];
    last_user.extend(tail);
    let expected = json!([
        original[0],
        {"role": "assistant", "content": [text(&format!(
            "[web search: \"rust\"]\n{}\na\nb\n{}\n{}",
            hit_line("s1"),
            closed_line("more"),
            called_echo()
        ))]},
        {"role": "user", "content": last_user},
    ]);
    assert_eq!(Value::Array(flat.clone()), expected);
    assert_alternates(&flat);
    let history = rig.agent.history();
    let sent = &history[..history.len() - 1];
    let flags = rig.agent.paused_flags(sent.len());
    check_layout(
        sent,
        &flags,
        &rig.agent.search.closed.calls,
        rig.provider.as_ref(),
    )
    .unwrap();
}

#[tokio::test]
async fn a_feature_rejection_on_a_rendered_request_falls_back_and_keeps_the_form() {
    let mut rig = rig(vec![
        stopped_with_thinking(),
        refused(binding_rejected()),
        done(&[B::Text("ok")]),
    ]);
    rig.agent.connections[0].provider = capable(&rig.provider);
    rig.turn("go").await;
    rig.agent.settings.binding_controls = true;
    let cuts_before = rig.agent.search.cuts.clone();
    let (outcome, _) = rig.turn("next").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 3);
    assert!(bodies[1]["thinking"].get("block_binding").is_some());
    assert!(bodies[2]["thinking"].get("block_binding").is_none());
    let unthought = |messages: &Value| -> Value {
        let mut messages = messages.clone();
        for message in messages.as_array_mut().unwrap() {
            message["content"]
                .as_array_mut()
                .unwrap()
                .retain(|b| b["type"] != "thinking" && b["type"] != "redacted_thinking");
        }
        messages
    };
    assert!(bodies[1]["messages"].to_string().contains("\"thinking\""));
    assert!(!bodies[2]["messages"].to_string().contains("\"thinking\""));
    assert_eq!(bodies[2]["messages"], unthought(&bodies[1]["messages"]));
    assert_eq!(bodies[2]["messages"][1]["content"][0], closed_text("rust"));
    assert_eq!(rig.agent.search.form, RenderingForm::Text);
    assert_eq!(rig.agent.search.cuts.len(), cuts_before.len() + 1);
    assert_eq!(position(&rig.kinds(), "thinking_cut").len(), 1);
    assert_eq!(rig.agent.search.kept_break, None);
    assert!(position(&rig.kinds(), "rendering_form").is_empty());
}

#[tokio::test]
async fn the_context_count_measures_the_kept_form() {
    let mut rig = rig(vec![
        stopped_with_thinking(),
        refused_rendering("form 1 refused"),
        done(&[B::Text("ok")]),
    ]);
    let counting = rig.provider.view(|k| {
        k.count = Some(1);
        k.documented = None;
        k.wire_print = false;
    });
    rig.agent.connections[0].provider = counting.clone();
    rig.turn("go").await;
    rig.turn("next").await;
    assert_eq!(rig.agent.search.form, RenderingForm::WithoutThinking);
    if let Some(last) = rig.agent.last.as_mut() {
        last.model = "a-fallback-model".into();
    }
    let (tx, _rx) = mpsc::channel(64);
    rig.agent
        .context_command(
            wisp_protocol::context::ReportDetail::All,
            wisp_protocol::RequestId(1),
            &tx,
        )
        .await;
    let counted: Vec<Value> = counting
        .counted()
        .iter()
        .map(|sent| serde_json::from_slice(&sent.count_body()).unwrap())
        .filter(|body: &Value| body["messages"].as_array().unwrap().len() > 1)
        .collect();
    assert_eq!(counted.len(), 1);
    let sent = rig.messages(2);
    let messages = counted[0]["messages"].as_array().unwrap();
    assert_eq!(messages[1], sent[1], "the kept form, without its thinking");
    assert_eq!(messages[1]["content"][0], closed_text("rust"));
    assert!(!counted[0].to_string().contains("sig-before"));
    assert!(counted[0].get("max_tokens").is_none());
}
