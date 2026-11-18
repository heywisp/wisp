use wisp_plugin_self_check::TEXT;
use wisp_protocol::HookChange;
use wisp_protocol::settings::{
    SettingChange, SettingName, SettingTarget, SettingValue, Target, names,
};

use super::*;

fn switched_on() -> Vec<crate::settings::Given> {
    vec![crate::settings::Given {
        target: Target::Name(wisp_protocol::settings::find(names::SELF_CHECK_BEFORE_END).unwrap()),
        value: SettingValue::Bool(true),
        origin: "--self-check".into(),
    }]
}

fn on() -> Setup {
    Setup {
        flags: switched_on(),
        ..Setup::default()
    }
}

fn worked() -> Vec<Step> {
    vec![
        uses(&[call("l1", "list_agents", json!({}))]),
        done("Done."),
        done("Checked."),
    ]
}

fn continues(run: &Run, agent: &str) -> Vec<String> {
    pick!(
        run.records(agent),
        LogEvent::HookApplied {
            hook,
            change: HookChange::Continue { text },
            ..
        } => format!("{hook}: {text}")
    )
}

fn reviews(run: &Run, agent: &str) -> usize {
    run.records(agent)
        .into_iter()
        .filter(|e| matches!(e, LogEvent::HookRan { hook, .. } if hook.starts_with("self-check/")))
        .count()
}

fn holds_the_text(request: &Sent) -> bool {
    request.all_text().contains(TEXT)
}

async fn one_turn(name: &str, setup: Setup, steps: Vec<Step>) -> Run {
    let mut run = start(name, setup, vec![("/", steps)]).await;
    run.say("Do the task.").await;
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    run.quit().await
}

#[tokio::test(start_paused = true)]
async fn on_a_root_turn_that_ran_a_tool_is_continued_once_with_the_text() {
    let run = one_turn("self-check-on", on(), worked()).await;
    let requests = run.requests("/");
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].last_user_texts(), [TEXT]);
    assert!(!holds_the_text(&requests[1]));
    assert_eq!(continues(&run, "/"), [format!("self-check/review: {TEXT}")]);
}

#[tokio::test(start_paused = true)]
async fn every_request_across_the_continue_keeps_the_prefix_and_extends_the_one_before() {
    let run = one_turn("self-check-prefix", on(), worked()).await;
    let requests = run.requests("/");
    assert_eq!(requests.len(), 3);
    for pair in requests.windows(2) {
        grows_from(&pair[1], &pair[0], "across the continue");
        assert!(pair[1].messages.len() > pair[0].messages.len());
    }
}

#[tokio::test(start_paused = true)]
async fn off_the_same_turn_sends_two_requests_and_runs_no_self_check_hook() {
    let run = one_turn("self-check-off", Setup::default(), worked()).await;
    let requests = run.requests("/");
    assert_eq!(requests.len(), 2);
    assert!(!requests.iter().any(holds_the_text));
    assert_eq!(reviews(&run, "/"), 0);
    assert!(continues(&run, "/").is_empty());
}

#[tokio::test(start_paused = true)]
async fn on_a_plain_answer_is_never_continued() {
    let run = one_turn("self-check-plain", on(), vec![done("Hi.")]).await;
    assert_eq!(run.requests("/").len(), 1);
    assert!(continues(&run, "/").is_empty());
    assert!(reviews(&run, "/") > 0);
}

#[tokio::test(start_paused = true)]
async fn a_child_is_never_continued_while_its_root_gets_its_one() {
    let child_gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "self-check-child",
        on(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "c", "Work.")]),
                    done("Spawned."),
                    done("Checked."),
                    done("Noted."),
                    done("Noted."),
                ],
            ),
            (
                "/c",
                vec![
                    gated(uses(&[send_call("m1", "..", "Which file?")]), &child_gate),
                    done("Asked."),
                ],
            ),
        ],
    )
    .await;
    run.say("Delegate the task.").await;
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    child_gate.add_permits(10);
    assert!(matches!(
        run.child_ended("/c").await,
        TurnOutcome::Completed { .. }
    ));
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let child = run.requests("/c");
    assert_eq!(child.len(), 2);
    assert!(!child.iter().any(holds_the_text));
    assert!(continues(&run, "/c").is_empty());
    assert!(
        run.records("/c").iter().any(|e| matches!(
            e,
            LogEvent::HookRan { hook, point, outcome: wisp_protocol::HookOutcome::None, .. }
                if hook == "self-check/review" && point == "before-end"
        )),
        "{:#?}",
        run.records("/c")
    );
    let root = run.requests("/");
    assert_eq!(root[2].last_user_texts(), [TEXT]);
    assert_eq!(continues(&run, "/"), [format!("self-check/review: {TEXT}")]);
}

#[tokio::test(start_paused = true)]
async fn turned_on_mid_session_it_checks_from_the_next_turn() {
    let mut steps = vec![done("Hi.")];
    steps.extend(worked());
    let mut run = start("self-check-mid", Setup::default(), vec![("/", steps)]).await;
    run.say("Hello.").await;
    run.root_ended().await;
    run.send(ClientCommand::ChangeSettings {
        request: RequestId(100),
        changes: wisp_protocol::settings::sorted_changes(vec![SettingChange {
            target: SettingTarget::Name(SettingName::of(
                wisp_protocol::settings::find(names::SELF_CHECK_BEFORE_END).unwrap(),
            )),
            value: Some(SettingValue::Bool(true)),
        }]),
        save: false,
    })
    .await;
    let changed = run
        .until("the self-check change", |e| {
            matches!(
                e,
                AgentEvent::SettingsChanged {
                    answers: Some(RequestId(100)),
                    ..
                }
            )
        })
        .await;
    let AgentEvent::SettingsChanged { changes, .. } = &changed else {
        panic!("{changed:?}")
    };
    let [change] = &changes[..] else {
        panic!("{changes:?}")
    };
    assert_eq!(change.when, wisp_protocol::settings::When::NextTurn);
    assert_eq!(change.cache, Some(wisp_protocol::CacheEffect::Kept));
    run.say("Do the task.").await;
    run.root_ended().await;
    let run = run.quit().await;
    let requests = run.requests("/");
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[3].last_user_texts(), [TEXT]);
}

async fn plugin_change(run: &mut Run, request: u64, change: ClientCommand) -> AgentEvent {
    run.send(change).await;
    run.until("the plugin change", |e| match e {
        AgentEvent::SettingsChanged {
            answers: Some(r), ..
        } => r.0 == request,
        AgentEvent::Rejected {
            request: Some(r), ..
        } => r.0 == request,
        _ => false,
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn switching_the_plugin_off_ends_its_continuations_from_the_next_turn_and_on_brings_them_back()
 {
    use crate::plugin::testing::plugin_switch;
    use wisp_protocol::settings::ENABLED_KEY;
    let mut steps = worked();
    steps.extend([uses(&[call("l2", "list_agents", json!({}))]), done("Done.")]);
    steps.extend(worked());
    let mut run = start("self-check-switched", on(), vec![("/", steps)]).await;
    run.say("Do the task.").await;
    run.root_ended().await;
    let off = plugin_change(
        &mut run,
        100,
        plugin_switch(100, "self-check", ENABLED_KEY, false),
    )
    .await;
    assert!(matches!(off, AgentEvent::SettingsChanged { .. }), "{off:?}");
    let hook = ClientCommand::ChangeSettings {
        request: RequestId(101),
        changes: vec![SettingChange {
            target: match wisp_protocol::settings::parse_target("plugins.self-check.hooks.review")
                .unwrap()
            {
                Target::Path(path) => SettingTarget::Path(path),
                other => panic!("{other:?}"),
            },
            value: Some(SettingValue::Bool(false)),
        }],
        save: false,
    };
    let refused = plugin_change(&mut run, 101, hook).await;
    assert!(
        matches!(refused, AgentEvent::Rejected { .. }),
        "{refused:?}"
    );
    run.say("Again.").await;
    run.root_ended().await;
    plugin_change(
        &mut run,
        102,
        plugin_switch(102, "self-check", ENABLED_KEY, true),
    )
    .await;
    run.say("Once more.").await;
    run.root_ended().await;
    let run = run.quit().await;
    let requests = run.requests("/");
    assert_eq!(requests.len(), 8);
    assert_ne!(requests[4].last_user_texts(), [TEXT]);
    assert_eq!(requests[7].last_user_texts(), [TEXT]);
    assert_eq!(continues(&run, "/").len(), 2);
}
