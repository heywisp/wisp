use super::*;
use crate::agent::tests::question;
use crate::tool::ToolEntry;
use crate::tool::question;
use wisp_plugin_ask_user as ask_user;

fn with_ask_user(mut tools: Vec<ToolEntry>) -> Toolbox {
    tools.push(crate::tool::testkit::ask_user());
    Toolbox::new(tools)
}

fn toolbox() -> Toolbox {
    with_ask_user(samples().into_iter().map(|s| s.tool).collect())
}

fn asker(provider: Arc<Fake>, env: &Env, user: bool) -> Agent {
    asker_with(provider, env, toolbox(), user)
}

fn ask(id: &str) -> B {
    call(id, "ask_user", question())
}

const KEEP: &str = "The user answered:\n\n1. [Plan] Which plan should go ahead?\n   Keep the plan";

fn expected(ending: Ending) -> (ToolOutcome, &'static str) {
    let completed = ToolOutcome::Completed {
        is_error: false,
        exit: None,
    };
    match ending {
        Ending::Answered => (completed, KEEP),
        Ending::Esc | Ending::HangUp => (ToolOutcome::Cancelled, question::STOPPED),
        Ending::NoChannel => (
            ToolOutcome::Failed {
                message: question::UNAVAILABLE_MESSAGE.into(),
                before_start: false,
            },
            question::UNAVAILABLE,
        ),
    }
}

#[tokio::test]
async fn every_ending_of_ask_user_gives_one_close_one_finish_and_its_exact_text() {
    let arrangements: [&[&str]; 4] = [
        &["ask_user"],
        &["ask_user", "sleep"],
        &["sleep", "ask_user"],
        &["ask_user", "ask_user"],
    ];
    for ending in Ending::ALL {
        for arrangement in arrangements {
            let label = format!("{ending:?} {arrangement:?}");
            let env = Env::new("ask-user-matrix");
            let ids: Vec<String> = arrangement
                .iter()
                .enumerate()
                .map(|(i, name)| format!("{name}{i}"))
                .collect();
            let reply: Vec<B> = arrangement
                .iter()
                .zip(&ids)
                .map(|(name, id)| match *name {
                    "ask_user" => ask(id),
                    _ => run(id),
                })
                .collect();
            let provider = scripted(vec![uses(&reply), done("ok")]);
            let mut agent = asker(provider.clone(), &env, ending != Ending::NoChannel);
            let (outcome, events) = reacting(&mut agent, "go", ending.react()).await;
            check_events(&events);
            check_questions(&events);
            assert_layout(&provider);

            let first = arrangement.iter().position(|n| *n == "ask_user").unwrap();
            let question = &ids[first];
            let expected_close: Vec<(String, QuestionEnd)> = ending
                .closed()
                .map(|how| (question.clone(), how))
                .into_iter()
                .collect();
            assert_eq!(closings(&events), expected_close, "{label}");

            let (outcome_q, text_q) = expected(ending);
            let done_calls = finished(&events);
            assert_eq!(done_calls.len(), ids.len(), "{label}");
            let messages = agent.history();
            let results_message = messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User && !results(m).is_empty())
                .unwrap();
            let texts = results(results_message);
            assert_eq!(texts.len(), ids.len(), "{label}");
            for (i, id) in ids.iter().enumerate() {
                let (_, outcome) = &done_calls[i];
                let (_, text, is_error) = texts[i];
                if i < first {
                    assert!(matches!(outcome, ToolOutcome::Completed { .. }), "{label}");
                } else if i == first {
                    assert_eq!(outcome, &outcome_q, "{label}");
                    assert_eq!(text, text_q, "{label}");
                    assert_eq!(is_error, outcome_q.is_error(), "{label}");
                    assert_eq!(
                        finished_result(&events, id).as_deref(),
                        Some(text_q),
                        "{label}"
                    );
                } else {
                    let reason = if ending.stops() {
                        NotRunReason::TurnStopped
                    } else {
                        NotRunReason::AfterQuestion
                    };
                    assert_eq!(text, not_run_text(&reason), "{label}");
                    assert_eq!(outcome, &ToolOutcome::NotRun { reason }, "{label}");
                    assert!(is_error, "{label}");
                }
            }
            assert_eq!(started_ids(&events), ids[..=first].to_vec(), "{label}");
            if ending.stops() {
                assert!(
                    matches!(
                        outcome,
                        TurnOutcome::Cancelled {
                            committed_steps: 1,
                            ..
                        }
                    ),
                    "{label}: {outcome:?}"
                );
                assert_eq!(provider.sent().len(), 1, "{label}");
            } else {
                assert!(
                    matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
                    "{label}: {outcome:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_card_gets_the_questions_exactly_as_the_model_wrote_them() {
    let env = Env::new("ask-user-asked");
    let long = json!({"questions": [{
        "header": "Deployment target",
        "question": "Where will it run?",
        "multi_select": true,
        "options": [
            {"label": "Fly.io", "description": ""},
            {"label": "A VPS", "description": "One box."}
        ]
    }]});
    let provider = scripted(vec![uses(&[call("q", "ask_user", long)]), done("ok")]);
    let mut agent = asker(provider.clone(), &env, true);
    let (_, events) = reacting(
        &mut agent,
        "go",
        on_question(|id| {
            React::Send(vec![answer(
                id,
                vec![Answer {
                    chosen: vec![0, 1],
                    own: Some("and a Dockerfile".into()),
                }],
            )])
        }),
    )
    .await;
    let asked: Vec<&Vec<wisp_protocol::Question>> = pick!(
        &events,
        AgentEvent::QuestionAsked { questions, .. } => questions
    );
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0][0].header, "Deployment target");
    assert!(asked[0][0].multi_select);
    let display = events.iter().find_map(|e| match e {
        AgentEvent::ToolCallRequested { display, .. } => Some(display.clone()),
        _ => None,
    });
    assert_eq!(display.map(|d| d.target), Some("Deployment …".to_owned()));
    let history = agent.history();
    let (_, text, is_error) = results(&history[2])[0];
    assert_eq!(
        text,
        "The user answered:\n\n1. [Deployment target] Where will it run?\n   - Fly.io\n   - A VPS\n   In their own words: and a Dockerfile\n\n\
Read the answers in the user's own words carefully: they may ask for something else, or ask you not to go on. Follow what they say."
    );
    assert!(!is_error);
}

#[tokio::test]
async fn invalid_ask_user_input_never_opens_a_card_and_still_holds_back_later_calls() {
    let env = Env::new("ask-user-invalid");
    let provider = scripted(vec![
        uses(&[
            call(
                "q",
                "ask_user",
                json!({"questions": [{"header": "H", "question": "Q?", "options": [
                    {"label": "Only", "description": ""}
                ]}]}),
            ),
            run("s"),
        ]),
        done("ok"),
    ]);
    let mut agent = asker(provider.clone(), &env, true);
    let (_, events) = reacting(&mut agent, "go", Ending::Answered.react()).await;
    check_events(&events);
    assert!(closings(&events).is_empty());
    assert!(started_ids(&events).is_empty());
    assert_eq!(
        finished(&events),
        [
            (
                "q".to_owned(),
                ToolOutcome::InvalidInput {
                    message: ask_user::TOO_FEW_OPTIONS.into()
                }
            ),
            (
                "s".to_owned(),
                ToolOutcome::NotRun {
                    reason: NotRunReason::AfterQuestion
                }
            ),
        ]
    );
}

#[tokio::test]
async fn an_own_answer_of_a_mebibyte_comes_back_whole_through_ask_user() {
    let env = Env::new("ask-user-big");
    let own = format!("{}\nend", "x".repeat(1 << 20));
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env, true);
    let reply = own.clone();
    let (_, events) = reacting(
        &mut agent,
        "go",
        on_question(move |id| React::Send(vec![answer(id, vec![Answer::own(reply.clone())])])),
    )
    .await;
    let words = own.replace('\n', "\n   ");
    let expected = format!(
        "The user answered:\n\n1. [Plan] Which plan should go ahead?\n   In their own words: {words}\n\n{}",
        question::OWN_WORDS_CAUTION
    );
    let messages = agent.history();
    let (_, text, _) = results(&messages[2])[0];
    assert_eq!(text.len(), expected.len());
    assert_eq!(text, expected);
    assert_eq!(finished_result(&events, "q"), Some(expected));
}

#[tokio::test(start_paused = true)]
async fn ask_user_has_no_timeout() {
    let env = Env::new("ask-user-wait");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env, true);
    let day = Duration::from_secs(24 * 60 * 60);
    let (outcome, events) = reacting(
        &mut agent,
        "go",
        on_question(move |id| React::Later(day, vec![answer(id, vec![Answer::choose(0)])])),
    )
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(finished_result(&events, "q").as_deref(), Some(KEEP));
}

#[tokio::test]
async fn ask_user_needs_no_workspace_and_alone_sends_no_environment_block() {
    let env = Env::new("ask-user-only");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let only = with_ask_user(Vec::new());
    let mut agent = asker_with(provider.clone(), &env, only, true);
    fs::remove_dir_all(&env.work).unwrap();
    let (_, events) = reacting(&mut agent, "go", Ending::Answered.react()).await;
    let cwds: Vec<&str> = pick!(&events, AgentEvent::ToolCallStarted { cwd, .. } => cwd.as_str());
    assert_eq!(cwds, [""]);
    let messages = agent.history();
    let (_, text, _) = results(&messages[2])[0];
    assert_eq!(text, KEEP);
    let requests = provider.sent();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let json = serde_json::to_string(&request.messages).unwrap();
        assert!(!json.contains("<environment>"), "{json}");
        assert_eq!(
            request.tools,
            [crate::tool::testkit::ask_user().spec().clone()]
        );
    }
    let bodies = provider.texts();
    same_prefix(&bodies);
}

#[tokio::test]
async fn every_logged_question_ending_rebuilds_the_committed_text() {
    for ending in Ending::ALL {
        let Some(how) = ending.closed() else {
            continue;
        };
        let env = Env::new("ask-user-log");
        let store = MemoryStore::default();
        let provider = scripted(vec![uses(&[ask("q"), run("s")]), done("ok")]);
        let mut agent = asker(provider.clone(), &env, true).with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
        reacting(&mut agent, "go", ending.react()).await;
        let logged = log_events(&store);
        let results: Vec<(String, Option<String>)> = pick!(
            &logged,
            LogEvent::ToolCallFinished {
                call_id, result, ..
            } => (call_id.clone(), result.clone())
        );
        let (_, text) = expected(ending);
        assert_eq!(
            results,
            [
                ("q".to_owned(), Some(text.to_owned())),
                ("s".to_owned(), None)
            ],
            "{ending:?}"
        );
        let questions = logged
            .iter()
            .find_map(|e| match e {
                LogEvent::QuestionAsked { questions, .. } => Some(questions.clone()),
                _ => None,
            })
            .unwrap();
        let closed: Vec<QuestionEnd> = pick!(
            &logged,
            LogEvent::QuestionClosed { how, .. } => how.clone()
        );
        assert_eq!(closed, std::slice::from_ref(&how), "{ending:?}");
        assert_eq!(
            Some(question::result_text(&questions, &how)),
            results[0].1,
            "{ending:?}"
        );
    }
}

#[cfg(unix)]
mod with_bash {
    use super::*;

    fn bash(id: &str) -> B {
        call(id, "bash", json!({"command": "touch marker"}))
    }

    fn bash_toolbox() -> Toolbox {
        with_ask_user(vec![crate::tool::testkit::bash_entry()])
    }

    #[tokio::test]
    async fn bash_after_ask_user_never_starts_whatever_the_ending() {
        for ending in Ending::ALL {
            let env = Env::new("ask-user-bash-after");
            let provider = scripted(vec![uses(&[ask("q"), bash("b")]), done("ok")]);
            let mut agent = asker_with(
                provider.clone(),
                &env,
                bash_toolbox(),
                ending != Ending::NoChannel,
            );
            let (_, events) = reacting(&mut agent, "go", ending.react()).await;
            check_events(&events);
            check_questions(&events);
            assert_eq!(started_ids(&events), ["q"], "{ending:?}");
            assert!(!env.work.join("marker").exists(), "{ending:?}");
            let reason = if ending.stops() {
                NotRunReason::TurnStopped
            } else {
                NotRunReason::AfterQuestion
            };
            let text = not_run_text(&reason);
            let messages = agent.history();
            let last = messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User && !results(m).is_empty())
                .cloned()
                .unwrap();
            assert_eq!(results(&last)[1], ("b", text.as_str(), true), "{ending:?}");
            assert_eq!(
                finished(&events)[1],
                ("b".to_owned(), ToolOutcome::NotRun { reason }),
                "{ending:?}"
            );
        }
    }

    #[tokio::test]
    async fn bash_before_ask_user_runs_first_and_the_turn_goes_on_after_the_answer() {
        let env = Env::new("ask-user-bash-before");
        let provider = scripted(vec![uses(&[bash("b"), ask("q")]), done("ok")]);
        let mut agent = asker_with(provider.clone(), &env, bash_toolbox(), true);
        let (outcome, events) = reacting(&mut agent, "go", Ending::Answered.react()).await;
        check_events(&events);
        check_questions(&events);
        assert!(
            matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
            "{outcome:?}"
        );
        assert_eq!(started_ids(&events), ["b", "q"]);
        assert!(env.work.join("marker").exists());
        let finished_b = events
            .iter()
            .position(
                |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "b"),
            )
            .unwrap();
        let asked = events
            .iter()
            .position(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
            .unwrap();
        assert!(finished_b < asked);
        let last = provider.sent()[1].messages.last().cloned().unwrap();
        let texts = results(&last);
        assert_eq!(texts[1], ("q", KEEP, false));
        let tools: Vec<String> = provider.sent()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(tools, ["ask_user", "bash"]);
    }
}
