use wisp_protocol::log::LEAD_FILL;

use super::*;
use crate::context::ModelSettings;
use crate::models::{ConfigModels, ModelRow, ModelTable};

const LINE_END: &str = "%). Wisp compacts at 850k.\n";

fn is_line(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Text { text } if text.starts_with("Context: "))
}

fn lines_in(message: &Message) -> Vec<String> {
    message
        .content
        .iter()
        .filter(|b| is_line(b))
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn session_on(model: &str, note: bool) -> SessionConfig {
    let mut session = default_session();
    session.model = model.into();
    session.context_note = note;
    session
}

fn loop_script(steps: usize) -> Vec<Step> {
    let mut scripts: Vec<Step> = (0..steps - 1)
        .map(|k| {
            let id = format!("c{k}");
            Step::from(reply_on(
                DEFAULT_MODEL,
                600,
                &[call(&id, "sleep", json!({"ms": 0}))],
                StopReason::ToolUse,
            ))
        })
        .collect();
    scripts.push(done("loop done"));
    scripts.push(done("second turn"));
    scripts
}

#[tokio::test]
async fn a_twenty_step_loop_sends_the_line_only_on_new_user_messages_and_keeps_its_prefix() {
    let env = Env::new("note-loop");
    let store = MemoryStore::default();
    let provider = scripted(loop_script(20));
    let mut agent = logged(provider.clone(), &env, &store);
    turn(&mut agent, "start").await;
    turn(&mut agent, "again").await;

    let requests = provider.sent();
    assert_eq!(requests.len(), 21);
    let bodies = provider.texts();
    for pair in bodies.windows(2) {
        let (head_a, msgs_a) = head_and_messages(&pair[0]);
        let (head_b, msgs_b) = head_and_messages(&pair[1]);
        assert_eq!(head_a, head_b);
        let open = msgs_a.strip_suffix(']').unwrap();
        let tail_open = msgs_a.strip_suffix("]}]").unwrap();
        assert!(
            msgs_b.starts_with(open) || msgs_b.starts_with(tail_open),
            "\n{msgs_a}\n{msgs_b}"
        );
    }

    let last = &requests[20].messages;
    let mut carrying = Vec::new();
    for (index, message) in last.iter().enumerate() {
        let lines = lines_in(message);
        let results = message
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
        if results {
            assert!(lines.is_empty(), "{index}: {message:?}");
        }
        if !lines.is_empty() {
            assert_eq!(lines.len(), 1, "{index}");
            assert_eq!(message.role, Role::User);
            carrying.push(index);
        }
    }
    assert_eq!(carrying, [0, last.len() - 1]);
    for message in [&last[0], &last[last.len() - 1]] {
        let stamp = message
            .content
            .iter()
            .position(|b| matches!(b, ContentBlock::Text { text } if text.starts_with("Sent: ")))
            .unwrap();
        let ContentBlock::Text { text } = &message.content[stamp + 1] else {
            panic!("{message:?}");
        };
        assert!(text.starts_with("Context: "), "{text}");
        assert!(text.contains(" of 1M tokens ("), "{text}");
        assert!(text.ends_with(LINE_END), "{text}");
    }

    let events = log_events(&store);
    let notes: Vec<Option<String>> = pick!(
        &events,
        LogEvent::TurnStarted { lead, .. } => lead.text(LEAD_FILL).map(str::to_owned)
    );
    assert_eq!(
        notes,
        [
            lines_in(&last[0]).pop(),
            lines_in(&last[last.len() - 1]).pop()
        ]
    );
    assert!(events.iter().any(|e| matches!(
        *e,
        LogEvent::SessionStarted {
            context_note_enabled: Some(true),
            ..
        }
    )));
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap().messages;
    assert_eq!(
        serde_json::to_string(&rebuilt).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
}

#[tokio::test]
async fn off_sends_no_line_and_the_system_prompt_is_the_same_as_on() {
    let run = |note: bool| async move {
        let env = Env::new("note-off");
        let store = MemoryStore::default();
        let provider = scripted(vec![done("one"), done("two")]);
        let context = log_context();
        let mut agent = Agent::new(provider.clone(), session_on(DEFAULT_MODEL, note))
            .with_clock(TickingClock::new())
            .with_tools(test_toolbox(), env.workspace())
            .with_platform("Linux")
            .with_resources(None)
            .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context);
        turn(&mut agent, "one").await;
        turn(&mut agent, "two").await;
        (provider, records(&store))
    };
    let (on, on_records) = run(true).await;
    let (off, off_records) = run(false).await;
    let on_bodies = on.texts();
    let off_bodies = off.texts();
    for (a, b) in on_bodies.iter().zip(&off_bodies) {
        assert_eq!(head_and_messages(a).0, head_and_messages(b).0);
        assert_eq!(tools_part(a), tools_part(b));
    }
    for request in off.sent() {
        assert!(request.messages.iter().all(|m| lines_in(m).is_empty()));
    }
    let carried: usize = on.sent()[1]
        .messages
        .iter()
        .map(|m| lines_in(m).len())
        .sum();
    assert_eq!(carried, 2);
    let enabled = |records: &[wisp_protocol::log::LogRecord]| {
        records.iter().find_map(|r| match &r.event {
            LogEvent::SessionStarted {
                context_note_enabled,
                ..
            } => Some(*context_note_enabled),
            _ => None,
        })
    };
    assert_eq!(enabled(&on_records), Some(Some(true)));
    assert_eq!(enabled(&off_records), Some(Some(false)));
    assert!(off_records.iter().all(|r| !matches!(
        &r.event,
        LogEvent::TurnStarted { lead, .. } if lead.has(LEAD_FILL)
    )));
}

#[tokio::test]
async fn every_model_gets_the_same_line_and_the_same_system_sentence() {
    let first = |model: &'static str| async move {
        let provider = scripted(vec![Step::from(reply_on(
            model,
            600,
            &[B::Text("hi")],
            StopReason::EndTurn,
        ))]);
        let mut agent =
            Agent::new(provider.clone(), session_on(model, true)).with_clock(TickingClock::new());
        turn(&mut agent, "how full is your context?").await;
        let request = provider.sent().remove(0);
        (
            request.session.system.clone(),
            lines_in(&request.messages[0]),
        )
    };
    let sonnet = first("claude-sonnet-4-6").await;
    let opus = first("claude-opus-5-5").await;
    assert_eq!(sonnet, opus);
    assert_eq!(sonnet.1.len(), 1);
    assert!(
        sonnet
            .0
            .as_deref()
            .unwrap()
            .ends_with(crate::plugin::testing::turn_line_tail().as_str())
    );
}

#[tokio::test]
async fn the_line_reads_your_limit_and_says_when_the_window_is_unknown() {
    let line_for = |model: &'static str, limit: Option<u64>| async move {
        let mut config = ConfigModels::default();
        if let Some(limit) = limit {
            let mut row = ModelRow::new(model);
            row.context_window = Some(limit);
            config.models.push(row);
        }
        let settings = ModelSettings::new(ModelTable::shipped().unwrap(), config);
        let provider = scripted(vec![Step::from(reply_on(
            model,
            600,
            &[B::Text("hi")],
            StopReason::EndTurn,
        ))]);
        let mut agent = Agent::new(provider.clone(), session_on(model, true))
            .with_clock(TickingClock::new())
            .with_model_settings(settings);
        turn(&mut agent, "hello").await;
        lines_in(&provider.sent()[0].messages[0]).pop().unwrap()
    };
    let limited = line_for(DEFAULT_MODEL, Some(400_000)).await;
    assert!(limited.contains(" of 400k tokens ("), "{limited}");
    assert!(
        limited.ends_with("%). Wisp compacts at 336k.\n"),
        "{limited}"
    );
    let unknown = line_for("m-unknown", None).await;
    assert!(unknown.starts_with("Context: "), "{unknown}");
    assert!(unknown.ends_with(" tokens; window unknown.\n"), "{unknown}");
}
