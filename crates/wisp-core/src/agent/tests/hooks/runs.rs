use super::*;

fn shown(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TurnStarted { turn, .. } => Some(format!("turn {turn}")),
            AgentEvent::TurnEnded { .. } => Some("ended".to_owned()),
            AgentEvent::Retrying { attempt, .. } => Some(format!("retry {attempt}")),
            AgentEvent::HookRan {
                hook,
                point,
                outcome,
                ..
            } => Some(format!("ran {hook} {point} {outcome:?}")),
            AgentEvent::HookApplied {
                hook,
                input_id,
                change,
                ..
            } => Some(format!("applied {hook} {input_id:?} {change:?}")),
            _ => None,
        })
        .collect()
}

fn rewrites() -> HookScript {
    Arc::new(|_, _, event| {
        Box::pin(async move {
            match event {
                Event::UserInput(input) => Reply::Rewrite(format!("{} [sent]", input.text)),
                _ => Reply::None,
            }
        })
    })
}

#[tokio::test]
async fn a_resume_replays_every_run_and_change_where_they_went_live_across_a_retry() {
    let calls = Calls::default();
    let overloaded = failure(ErrorKind::Overloaded, RetryClass::Transient, false);
    let mut script = vec![Ok(ProviderEvent::Sending)];
    script.extend(reply("hello", 600, 0, StopReason::EndTurn));
    let provider = Fake::new(vec![
        Step::from(vec![Ok(ProviderEvent::Sending), Err(overloaded)]),
        Step::from(script),
    ]);
    let store = MemoryStore::default();
    let body = hook_body("u", "user-input", "")
        + &hook_body("r", "before-request", "")
        + &hook_body("t", "after-turn", "");
    let agent = with_sections(
        logged(provider, &store).with_retry_policy(crate::agent::retry::quick()),
        vec![source("aa", &body, rewrites(), &calls)],
    )
    .await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("m", "hi")).await.unwrap();
    let mut live = until_seen(
        &mut events,
        |e| matches!(e, AgentEvent::HookRan { point, .. } if point == "after-turn"),
    )
    .await;
    drop(commands);
    while let Some(e) = events.recv().await {
        live.push(e);
    }
    let live = shown(&live);
    assert_eq!(
        live.iter().filter(|s| s.starts_with("ran aa/r")).count(),
        2,
        "{live:#?}"
    );
    assert!(live.iter().any(|s| s.starts_with("retry")), "{live:#?}");
    assert!(
        live.iter().any(|s| s.starts_with("applied aa/u")),
        "{live:#?}"
    );
    let records = records(&store);
    let replayed = crate::resume::transcript(&records, &crate::tool::Toolbox::default(), None);
    let replayed = shown(&replayed);
    let through = |all: &[String]| {
        all.iter()
            .position(|s| s == "ended")
            .map(|end| all[..=end].to_vec())
            .unwrap_or_default()
    };
    assert_eq!(through(&replayed), through(&live));
    let after: Vec<&String> = live.iter().skip_while(|s| *s != "ended").collect();
    let replayed_after: Vec<&String> = replayed.iter().skip_while(|s| *s != "ended").collect();
    assert_eq!(replayed_after, after);
}
