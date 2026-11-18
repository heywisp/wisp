use std::sync::atomic::AtomicU64;

use futures::executor::block_on;
use wisp_plugin_api::hooks::AfterTurn;
use wisp_plugin_api::tree::Identity;
use wisp_plugin_testkit::hooks::{before_turn, user_input};
use wisp_plugin_testkit::host::{FakeHost, identity};

use super::*;

const MINUTE: u64 = 60_000;

fn idle(identity: Option<Identity>) -> (Arc<dyn Plugin>, Arc<AtomicU64>) {
    let clock = Arc::new(AtomicU64::new(5 * MINUTE));
    let read = clock.clone();
    let host = FakeHost {
        identity,
        elapsed: Some(Arc::new(move || read.load(Ordering::SeqCst))),
        ..FakeHost::default()
    };
    (instantiate(Arc::new(host)), clock)
}

fn turn(plugin: &Arc<dyn Plugin>, event: BeforeTurn) -> Reply {
    block_on(plugin.on_hook("note", Event::BeforeTurn(event)))
}

fn unattended(plugin: &Arc<dyn Plugin>) -> Reply {
    turn(plugin, before_turn(1, ""))
}

fn end(plugin: &Arc<dyn Plugin>) {
    let event = Event::AfterTurn(AfterTurn {
        turn: 1,
        outcome: "completed".to_owned(),
    });
    assert_eq!(block_on(plugin.on_hook("done", event)), Reply::None);
}

fn wait(clock: &AtomicU64, ms: u64) {
    clock.fetch_add(ms, Ordering::SeqCst);
}

#[test]
fn a_turn_nobody_typed_gets_the_note_only_after_ten_quiet_minutes() {
    let (plugin, clock) = idle(Some(identity("/")));
    assert_eq!(unattended(&plugin), Reply::None);
    end(&plugin);
    wait(&clock, 10 * MINUTE - 1);
    assert_eq!(unattended(&plugin), Reply::None);
    wait(&clock, 1);
    let Reply::Append(lines) = unattended(&plugin) else {
        panic!("no note")
    };
    assert_eq!(lines.concat().as_bytes(), b"The user is idle (no message from them in the 10 minutes or more since your last reply to them): carry on with your work, but end each turn with no text at all, not even a status line, until they write again.\n");
    end(&plugin);
    assert_eq!(unattended(&plugin), Reply::Append(vec![note()]));
}

#[test]
fn a_typed_steered_or_retried_turn_means_the_user_is_back() {
    let (plugin, clock) = idle(None);
    let images = BeforeTurn {
        images: 1,
        ..before_turn(1, "")
    };
    let retry = BeforeTurn {
        retry: true,
        ..before_turn(1, "")
    };
    for back in [before_turn(1, "hello"), images, retry] {
        wait(&clock, 10 * MINUTE);
        assert_eq!(turn(&plugin, back.clone()), Reply::None, "{back:?}");
        end(&plugin);
        assert_eq!(unattended(&plugin), Reply::None, "{back:?}");
    }
    wait(&clock, 10 * MINUTE);
    assert_eq!(unattended(&plugin), Reply::Append(vec![note()]));
    assert_eq!(
        block_on(plugin.on_hook("seen", user_input("and", 0))),
        Reply::None
    );
    end(&plugin);
    assert_eq!(unattended(&plugin), Reply::None);
    wait(&clock, 10 * MINUTE);
    assert_eq!(turn(&plugin, before_turn(1, "hello")), Reply::None);
    wait(&clock, 9 * MINUTE);
    assert_eq!(unattended(&plugin), Reply::None);
    wait(&clock, MINUTE);
    assert_eq!(unattended(&plugin), Reply::Append(vec![note()]));
}

#[test]
fn the_ten_minutes_start_when_the_users_turn_ends() {
    let (plugin, clock) = idle(Some(identity("/")));
    assert_eq!(turn(&plugin, before_turn(1, "hello")), Reply::None);
    wait(&clock, 15 * MINUTE);
    end(&plugin);
    wait(&clock, 7 * MINUTE);
    assert_eq!(unattended(&plugin), Reply::None);
    end(&plugin);
    wait(&clock, 3 * MINUTE);
    assert_eq!(unattended(&plugin), Reply::Append(vec![note()]));
}

#[test]
fn only_the_main_agent_gets_the_note() {
    for (identity, noted) in [
        (Some(identity("/c")), false),
        (None, true),
        (Some(identity("/")), true),
    ] {
        let (plugin, clock) = idle(identity.clone());
        wait(&clock, 10 * MINUTE);
        let expected = if noted {
            Reply::Append(vec![note()])
        } else {
            Reply::None
        };
        assert_eq!(unattended(&plugin), expected, "{identity:?}");
    }
}
