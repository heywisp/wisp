use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY};

use super::*;
use crate::plugin::testing::plugin_switch;

fn off(hint: &str) -> CommandOutcome {
    CommandOutcome::Refused {
        kind: CommandRefusal::PluginOff,
        hint: hint.to_owned(),
    }
}

async fn switched(live: &mut Live, request: u64, key: &str, on: bool) {
    live.send(plugin_switch(request, ID, key, on)).await;
    live.until(
        "the change's answer",
        |e| matches!(e, AgentEvent::SettingsChanged { answers: Some(r), .. } if r.0 == request),
    )
    .await;
}

#[tokio::test]
async fn a_command_of_a_plugin_switched_off_runs_until_the_next_turn_and_is_refused_after() {
    let env = Env::new("commands-switched-off");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("one"), says("two")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    switched(&mut live, 90, ENABLED_KEY, false).await;
    live.send(run(1, "note", "before")).await;
    assert_eq!(live.answer(1).await, note("before"));
    live.send(ClientCommand::send("m", "hi")).await;
    live.ended().await;
    live.send(run(2, "note", "after")).await;
    assert_eq!(
        live.answer(2).await,
        off("testkit is off; turn it on in /config")
    );
    switched(&mut live, 91, ENABLED_KEY, true).await;
    live.send(run(3, "note", "still")).await;
    assert!(matches!(
        live.answer(3).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::PluginOff,
            ..
        }
    ));
    live.send(ClientCommand::send("n", "again")).await;
    live.ended().await;
    live.send(run(4, "note", "back")).await;
    assert_eq!(live.answer(4).await, note("back"));
    live.close().await;
    assert_eq!(ran_names(&calls), ["note", "note"]);
}

#[tokio::test]
async fn a_command_waiting_in_the_lane_when_the_switch_applies_is_refused_when_its_turn_comes() {
    let env = Env::new("commands-switched-waiting");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![held("one", &gate)]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(run(1, "wait", "k note first")).await;
    gates.started("k").await;
    live.send(run(2, "note", "second")).await;
    switched(&mut live, 90, ENABLED_KEY, false).await;
    live.send(ClientCommand::send("m", "hi")).await;
    live.until("the turn's start", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    gates.release("k");
    assert_eq!(live.answer(1).await, note("first"));
    assert!(matches!(
        live.answer(2).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::PluginOff,
            ..
        }
    ));
    gate.notify_one();
    live.ended().await;
    live.close().await;
    assert_eq!(ran_names(&calls), ["wait"]);
}

#[tokio::test]
async fn a_removed_plugins_command_is_refused_as_off_and_an_unknown_name_as_unknown() {
    let env = Env::new("commands-removed");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("one")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    switched(&mut live, 90, INSTALLED_KEY, false).await;
    live.send(ClientCommand::send("m", "hi")).await;
    live.ended().await;
    live.send(run(1, "note", "gone")).await;
    assert_eq!(
        live.answer(1).await,
        off("testkit is removed; add it back in /config")
    );
    live.send(ClientCommand::RunPluginCommand {
        name: "nobody:note".to_owned(),
        args: wisp_protocol::Secret::new(String::new()),
        request: RequestId(2),
    })
    .await;
    assert!(matches!(
        live.answer(2).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::Unknown,
            ..
        }
    ));
    live.close().await;
    assert!(ran_names(&calls).is_empty());
}

#[tokio::test]
async fn a_removed_plugins_running_and_waiting_commands_are_cancelled_when_it_leaves() {
    let env = Env::new("commands-removed-running");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("one")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(run(1, "wait", "k note first")).await;
    gates.started("k").await;
    live.send(run(2, "note", "second")).await;
    switched(&mut live, 90, INSTALLED_KEY, false).await;
    live.send(ClientCommand::send("m", "hi")).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    assert_eq!(live.answer(2).await, CommandOutcome::Cancelled);
    live.ended().await;
    live.close().await;
    assert_eq!(ran_names(&calls), ["wait"]);
}
