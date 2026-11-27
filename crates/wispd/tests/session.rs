#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "scripted-session helpers fail the test on setup, transport or assertion errors"
)]

#[path = "support/script.rs"]
mod script;
#[path = "../../wisp-protocol/tests/support/mod.rs"]
mod support;

use std::path::PathBuf;
use std::time::Duration;

use script::{Raw, Script, WAIT, end, event, reopening, text};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use wisp_client::link::{self as client, LinkEnd};
use wisp_protocol::link::{self, CONTRACT, HOST_FIRST, REQUEST_RANGE, ToClient, ToHost};
use wisp_protocol::{AgentEvent, ClientCommand, InputId, SessionView};
use wispd::Host;

#[tokio::test]
async fn opening_orders_view_state_since_cut_and_live_and_preserves_an_input_before_its_turn() {
    let mut script = Script::new().await;
    let first = script.open().await;
    assert!(first.now.is_none());
    assert!(first.opening.start.is_some());
    assert_eq!(first.view.events, [AgentEvent::turn_started(1), end()]);
    assert_eq!(first.view.returned[0].id, Some(InputId("input-1".into())));
    drop(first);
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    let accepted = AgentEvent::InputAccepted {
        opening: false,
        id: InputId("input-2".into()),
        mode: wisp_protocol::DeliveryMode::Queue,
        text: "hello".into(),
        images: vec![],
        replaces: None,
        held: false,
        front: false,
        from: None,
    };
    for event in [accepted.clone(), AgentEvent::turn_started(2), text("since")] {
        script.events.send(event).await.unwrap();
    }
    let (opening, _) = reopening(&script).await;
    let request = script.describe().await;
    let state = AgentEvent::AgentsSnapshot {
        request: None,
        agents: vec![],
    };
    script
        .snapshot(request, SessionView::default(), vec![state.clone()])
        .await;
    let mut link = opening.await.unwrap();
    assert!(link.opening.start.is_none());
    assert!(link.now.is_some());
    for expected in [state, accepted, AgentEvent::turn_started(2), text("since")] {
        assert_eq!(event(&mut link).await, expected);
    }
    script.events.send(text("live")).await.unwrap();
    assert_eq!(event(&mut link).await, text("live"));
    script.close(&mut link, false).await;
}

#[tokio::test]
async fn a_second_window_takes_the_chat_without_resetting_its_live_state() {
    let mut script = Script::new().await;
    let mut first = script.open().await;
    let connection = script.connection(8).await;
    assert!(connection.welcome.window);
    let second = tokio::spawn(async move { client::open(connection).await.unwrap() });
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    let request = script.describe().await;
    assert_eq!(request, HOST_FIRST);
    script
        .snapshot(request, SessionView::default(), vec![])
        .await;
    let mut second = second.await.unwrap();
    tokio::time::timeout(WAIT, first.ended.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*first.ended.borrow(), Some(LinkEnd::Taken));
    second
        .channels
        .commands
        .send(ClientCommand::Cancel)
        .await
        .unwrap();
    assert_eq!(script.command().await, ClientCommand::Cancel);
    script.close(&mut second, false).await;
}

#[tokio::test]
async fn hello_and_a_frameless_probe_do_not_take_a_window_or_step_it_out_of_a_terminal() {
    let mut script = Script::new().await;
    let mut link = script.open().await;
    link.channels
        .commands
        .send(ClientCommand::AttachProcess { id: 1, size: None })
        .await
        .unwrap();
    assert!(matches!(
        script.command().await,
        ClientCommand::AttachProcess { id: 1, .. }
    ));
    let hello = client::connect(&script.path, 8).await.unwrap();
    assert!(hello.welcome.window);
    assert_eq!(hello.welcome.folder, PathBuf::from("/work/project"));
    drop(hello);
    drop(UnixStream::connect(&script.path).await.unwrap());
    link.channels
        .commands
        .send(ClientCommand::CheckProcesses)
        .await
        .unwrap();
    assert_eq!(script.command().await, ClientCommand::CheckProcesses);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), script.commands.recv())
            .await
            .is_err()
    );
    script.close(&mut link, false).await;
}

#[tokio::test]
async fn leaving_clears_the_window_and_reopening_uses_a_new_request_range() {
    let mut script = Script::new().await;
    let first = script.open().await;
    let range = first.welcome.first_request.0;
    assert!(!first.welcome.window);
    drop(first);
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    let probe = client::connect(&script.path, 7).await.unwrap();
    assert!(!probe.welcome.window);
    assert!(probe.welcome.first_request.0 >= range + REQUEST_RANGE);
    let (opening, range2) = reopening(&script).await;
    let request = script.describe().await;
    assert!(request.0 > range2.0 + REQUEST_RANGE);
    script
        .snapshot(request, SessionView::default(), vec![])
        .await;
    let mut link = opening.await.unwrap();
    script.close(&mut link, false).await;
}

#[tokio::test]
async fn end_and_forced_end_close_commands_and_wait_until_the_store_is_released() {
    for forced in [false, true] {
        let mut script = Script::new().await;
        let mut link = script.open().await;
        script.close(&mut link, forced).await;
    }
}

#[tokio::test]
async fn an_external_quit_cannot_end_a_window_opened_after_hello() {
    let mut script = Script::new().await;
    let mut quitter = script.connection(8).await;
    assert!(!quitter.welcome.window);
    let mut window = script.open().await;
    assert_eq!(
        tokio::time::timeout(WAIT, quitter.end_if_windowless())
            .await
            .unwrap()
            .unwrap(),
        None
    );
    window
        .channels
        .commands
        .send(ClientCommand::Cancel)
        .await
        .unwrap();
    assert_eq!(script.command().await, ClientCommand::Cancel);
    script.close(&mut window, false).await;
}

#[tokio::test]
async fn a_restart_relays_its_rows_then_announces_the_restart_before_the_saved_end() {
    for restart in [false, true] {
        let mut script = Script::new().await;
        let mut window = script.window().await;
        let mut peer = script.connection(8).await;
        assert!(peer.welcome.restart);
        if restart {
            peer.restart(true).await.unwrap();
            assert_eq!(script.command().await, ClientCommand::GetReady { on: true });
            for fixture in [
                "event_restart_getting_ready.json",
                "event_restart_ready.json",
            ] {
                let event: AgentEvent =
                    serde_json::from_str(&support::fixture_text(fixture)).unwrap();
                script.events.send(event.clone()).await.unwrap();
                assert_eq!(peer.next().await.unwrap(), ToClient::event(event.clone()));
                assert_eq!(window.next::<ToClient>().await, ToClient::event(event));
            }
        } else {
            window.send(&ToHost::End { forced: false }).await;
        }
        assert!(
            tokio::time::timeout(WAIT, &mut script.ending)
                .await
                .unwrap()
                .unwrap()
                .0
        );
        assert!(script.commands.recv().await.is_none());
        let withdrawn = |id: &str| AgentEvent::InputWithdrawn {
            ids: vec![InputId(id.into())],
            by: wisp_protocol::WithdrawnBy::SessionEnded,
        };
        let early = withdrawn("tui-2");
        script.events.send(early.clone()).await.unwrap();
        tokio::task::yield_now().await;
        let queued = withdrawn("tui-3");
        script.events.send(queued.clone()).await.unwrap();
        script.saved.take().unwrap().send(true).unwrap();
        if restart {
            for event in [early, queued] {
                assert_eq!(window.next::<ToClient>().await, ToClient::event(event));
            }
        }
        let terminal = if restart {
            vec![ToClient::Restarting, ToClient::Ended { saved: true }]
        } else {
            vec![ToClient::Ended { saved: true }]
        };
        for expected in terminal {
            assert_eq!(
                tokio::time::timeout(WAIT, peer.next())
                    .await
                    .unwrap()
                    .unwrap(),
                expected
            );
            assert_eq!(window.next::<ToClient>().await, expected);
        }
        assert!(!script.path.exists());
        assert!((&mut script.host).await.unwrap());
    }
}

#[tokio::test]
async fn a_window_opened_while_finishing_gets_the_live_row_but_none_after_a_cancel() {
    for cancelled_event in [false, true] {
        let mut script = Script::new().await;
        if !cancelled_event {
            drop(script.open().await);
            assert_eq!(script.command().await, ClientCommand::WindowGone);
        }
        let mut peer = script.connection(8).await;
        peer.restart(true).await.unwrap();
        assert_eq!(script.command().await, ClientCommand::GetReady { on: true });
        let finishing: AgentEvent =
            serde_json::from_str(&support::fixture_text("event_restart_getting_ready.json"))
                .unwrap();
        script.events.send(finishing.clone()).await.unwrap();
        assert_eq!(peer.next().await.unwrap(), ToClient::event(finishing));
        let mut first = if cancelled_event {
            script.open().await
        } else {
            let (opening, _) = reopening(&script).await;
            let request = script.describe().await;
            script
                .snapshot(request, SessionView::default(), vec![])
                .await;
            opening.await.unwrap()
        };
        let AgentEvent::Restart { state, open } = event(&mut first).await else {
            panic!("the window missed the live restart row")
        };
        assert_eq!(state, wisp_protocol::RestartState::GettingReady);
        assert_eq!(open.len(), 1);
        assert!(open[0].running_ms >= 1250);
        if cancelled_event {
            let cancelled = AgentEvent::Restart {
                state: wisp_protocol::RestartState::Cancelled,
                open: Vec::new(),
            };
            script.events.send(cancelled.clone()).await.unwrap();
            assert_eq!(event(&mut first).await, cancelled);
        } else {
            drop(peer);
            assert_eq!(
                script.command().await,
                ClientCommand::GetReady { on: false }
            );
        }
        drop(first);
        assert_eq!(script.command().await, ClientCommand::WindowGone);
        let (opening, _) = reopening(&script).await;
        let request = script.describe().await;
        script
            .snapshot(request, SessionView::default(), vec![])
            .await;
        let mut reopened = opening.await.unwrap();
        script.events.send(text("after the cancel")).await.unwrap();
        assert_eq!(event(&mut reopened).await, text("after the cancel"));
        script.close(&mut reopened, false).await;
    }
}

#[tokio::test]
async fn leaving_the_restart_cancels_getting_ready_without_ending_the_chat() {
    let mut script = Script::new().await;
    let mut window = script.open().await;
    let mut peer = script.connection(8).await;
    peer.restart(true).await.unwrap();
    assert_eq!(script.command().await, ClientCommand::GetReady { on: true });
    drop(peer);
    assert_eq!(
        script.command().await,
        ClientCommand::GetReady { on: false }
    );
    let cancelled = AgentEvent::Restart {
        state: wisp_protocol::RestartState::Cancelled,
        open: vec![],
    };
    script.events.send(cancelled.clone()).await.unwrap();
    assert_eq!(event(&mut window).await, cancelled);
    window
        .channels
        .commands
        .send(ClientCommand::Cancel)
        .await
        .unwrap();
    assert_eq!(script.command().await, ClientCommand::Cancel);
    script.close(&mut window, false).await;
}

#[tokio::test]
async fn restarting_now_ends_without_waiting_for_a_ready_report() {
    let mut script = Script::new().await;
    let mut peer = script.connection(8).await;
    peer.restart(false).await.unwrap();
    assert!(
        tokio::time::timeout(WAIT, &mut script.ending)
            .await
            .unwrap()
            .unwrap()
            .0
    );
    assert!(script.commands.recv().await.is_none());
    script.saved.take().unwrap().send(true).unwrap();
    assert_eq!(
        tokio::time::timeout(WAIT, peer.next())
            .await
            .unwrap()
            .unwrap(),
        ToClient::Restarting
    );
    assert_eq!(
        tokio::time::timeout(WAIT, peer.next())
            .await
            .unwrap()
            .unwrap(),
        ToClient::Ended { saved: true }
    );
    assert!((&mut script.host).await.unwrap());
}

#[tokio::test]
async fn a_window_cannot_park_its_chat_and_another_peer_cannot_take_over_a_restart() {
    let mut script = Script::new().await;
    let mut window = script.open().await;
    for on in [true, false] {
        window
            .channels
            .commands
            .send(ClientCommand::GetReady { on })
            .await
            .unwrap();
        assert!(matches!(
            event(&mut window).await,
            AgentEvent::Rejected { .. }
        ));
    }
    let mut peer = script.connection(8).await;
    peer.restart(true).await.unwrap();
    assert_eq!(script.command().await, ClientCommand::GetReady { on: true });
    let mut other = script.connection(8).await;
    for wait in [true, false] {
        other.restart(wait).await.unwrap();
        assert!(
            matches!(tokio::time::timeout(WAIT, other.next()).await.unwrap().unwrap(), ToClient::Event(event) if matches!(*event, AgentEvent::Rejected { .. }))
        );
    }
    drop(peer);
    assert_eq!(
        script.command().await,
        ClientCommand::GetReady { on: false }
    );
    script.close(&mut window, false).await;
}

#[tokio::test]
async fn conditional_quit_falls_back_for_an_old_host_without_ending_its_window() {
    for (window, saved) in [(false, true), (false, false), (true, false)] {
        let (stream, peer) = tokio::io::duplex(4096);
        let host = tokio::spawn(async move {
            let mut raw = Raw(BufReader::new(peer));
            assert!(matches!(raw.next::<ToHost>().await, ToHost::Hello { .. }));
            let ToClient::Welcome(mut welcome) =
                serde_json::from_str(&support::fixture_text("link_welcome.json")).unwrap()
            else {
                panic!("expected Welcome");
            };
            welcome.window = window;
            let mut frame = serde_json::to_value(ToClient::Welcome(welcome)).unwrap();
            frame["data"].as_object_mut().unwrap().remove("restart");
            raw.send(&frame).await;
            assert_eq!(raw.next::<ToHost>().await, ToHost::EndIfWindowless);
            raw.send(&ToClient::event(AgentEvent::rejected(
                "placeholder refusal",
            )))
            .await;
            if !window {
                assert_eq!(raw.next::<ToHost>().await, ToHost::End { forced: false });
                raw.send(&ToClient::Ended { saved }).await;
            }
            let mut byte = [0];
            assert_eq!(
                tokio::time::timeout(WAIT, raw.0.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        });
        let mut quitter = client::handshake(stream, 8).await.unwrap();
        assert!(!quitter.welcome.restart);
        assert_eq!(
            tokio::time::timeout(WAIT, quitter.end_if_windowless())
                .await
                .unwrap()
                .unwrap(),
            (!window).then_some(saved)
        );
        drop(quitter);
        host.await.unwrap();
    }
}

#[tokio::test]
async fn end_from_a_client_that_never_opened_is_answered_and_another_contract_can_still_end() {
    for (contract, frame, saved) in [
        (CONTRACT, ToHost::End { forced: false }, true),
        (CONTRACT + 1, ToHost::End { forced: false }, true),
        (CONTRACT, ToHost::EndIfWindowless, true),
        (CONTRACT, ToHost::EndIfWindowless, false),
    ] {
        let mut script = Script::new().await;
        let mut raw = Raw::connect(&script.path).await;
        raw.send(&ToHost::Hello { contract, build: 8 }).await;
        match raw.next::<ToClient>().await {
            ToClient::Welcome(welcome) => assert!(!welcome.window),
            ToClient::Refused {
                contract: actual, ..
            } => assert_eq!(actual, CONTRACT),
            other => panic!("{other:?}"),
        }
        raw.send(&frame).await;
        let (requested, forced) = tokio::time::timeout(WAIT, &mut script.ending)
            .await
            .unwrap()
            .unwrap();
        assert!(requested && !*forced.borrow());
        assert!(script.commands.recv().await.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), raw.next::<ToClient>())
                .await
                .is_err()
        );
        script.saved.take().unwrap().send(saved).unwrap();
        assert_eq!(raw.next::<ToClient>().await, ToClient::Ended { saved });
    }
}

#[tokio::test]
async fn a_later_forced_end_reaches_the_shutdown_wait() {
    let mut script = Script::new().await;
    let mut raw = Raw::connect(&script.path).await;
    raw.send(&ToHost::End { forced: false }).await;
    let (_, mut forced) = tokio::time::timeout(WAIT, &mut script.ending)
        .await
        .unwrap()
        .unwrap();
    raw.send(&ToHost::End { forced: true }).await;
    tokio::time::timeout(WAIT, forced.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*forced.borrow());
    script.saved.take().unwrap().send(false).unwrap();
    assert_eq!(
        raw.next::<ToClient>().await,
        ToClient::Ended { saved: false }
    );
}

#[tokio::test]
async fn a_listed_agent_is_not_spawned_twice_and_private_sign_in_stays_with_its_window() {
    for (wrapped, through_daemon) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut script = Script::new().await;
        let daemon = wispd::daemon::Daemon::new(
            &script._scratch.root,
            |_, _| async { Ok((None, async {})) },
            |_| false,
        )
        .unwrap();
        let (ready, bound) = oneshot::channel();
        let (stop, stopped) = oneshot::channel();
        let daemon = tokio::spawn(daemon.run(
            move |path| {
                ready.send(path.to_owned()).unwrap();
            },
            async {
                let _ = stopped.await;
            },
        ));
        let daemon_path = bound.await.unwrap();
        if through_daemon {
            script.daemon = Some(daemon_path);
        }
        let mut first = script.open().await;
        let request = first.welcome.first_request;
        let spawn: AgentEvent =
            serde_json::from_str(&support::fixture_text("event_agent_spawned.json")).unwrap();
        let private = AgentEvent::SubscriptionAuth {
            event: wisp_protocol::subscription_auth::AuthEvent {
                request,
                outcome: wisp_protocol::subscription_auth::AuthOutcome::Challenge {
                    challenge: wisp_protocol::subscription_auth::PrivateChallenge {
                        login: request,
                        provider: wisp_protocol::subscription_auth::SubscriptionProvider::Claude,
                        alias: "work".into(),
                        kind: wisp_protocol::subscription_auth::ChallengeKind::ManualCode,
                        authorization_url: wisp_protocol::Secret::new("CANARY-SIGN-IN"),
                        expires_in_secs: 60,
                        opened: false,
                    },
                },
            },
        };
        let private = if wrapped {
            AgentEvent::Agent {
                agent: wisp_protocol::AgentPath::new("/worker"),
                event: Box::new(private),
            }
        } else {
            private
        };
        for expected in [spawn.clone(), private.clone()] {
            script.events.send(expected.clone()).await.unwrap();
            assert_eq!(event(&mut first).await, expected);
        }
        drop(first);
        assert_eq!(script.command().await, ClientCommand::WindowGone);
        let (opening, _) = reopening(&script).await;
        let request = script.describe().await;
        let AgentEvent::AgentSpawned { agent, .. } = spawn else {
            unreachable!()
        };
        let state = AgentEvent::AgentsSnapshot {
            request: None,
            agents: vec![agent],
        };
        script
            .snapshot(request, SessionView::default(), vec![state.clone()])
            .await;
        let mut link = opening.await.unwrap();
        assert_eq!(event(&mut link).await, state);
        script.events.send(private).await.unwrap();
        script.events.send(text("live")).await.unwrap();
        assert_eq!(event(&mut link).await, text("live"));
        script.close(&mut link, false).await;
        stop.send(()).unwrap();
        daemon.await.unwrap().unwrap();
        for folder in [
            script._scratch.root.join("run"),
            link::daemon_dir(&script._scratch.root),
        ] {
            for entry in std::fs::read_dir(folder).unwrap().flatten() {
                if entry.file_type().unwrap().is_file() {
                    assert!(
                        !std::fs::read(entry.path())
                            .unwrap()
                            .windows(b"CANARY-SIGN-IN".len())
                            .any(|bytes| bytes == b"CANARY-SIGN-IN")
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn an_unknown_line_is_rejected_without_its_text_and_the_connection_stays_usable() {
    let mut script = Script::new().await;
    let mut raw = Raw::connect(&script.path).await;
    raw.line("{\"type\":\"future\",\"secret\":\"CANARY-UNKNOWN\"}\n")
        .await;
    let rejected = raw.next::<ToClient>().await;
    assert!(
        matches!(&rejected, ToClient::Event(event) if matches!(event.as_ref(), AgentEvent::Rejected { .. }))
    );
    assert!(!format!("{rejected:?}").contains("CANARY"));
    raw.send(&ToHost::Hello {
        contract: CONTRACT,
        build: 7,
    })
    .await;
    assert!(matches!(raw.next::<ToClient>().await, ToClient::Welcome(_)));
    script.ended(&mut raw).await;
}

#[tokio::test]
async fn a_nonreading_client_never_blocks_the_session() {
    let mut script = Script::new().await;
    let mut raw = script.window().await;
    let numbered = |n| text(&format!("{n}:{}", "x".repeat(8192)));
    tokio::time::timeout(WAIT, async {
        for n in 0..512 {
            script.events.send(numbered(n)).await.unwrap();
        }
        script.events.send(end()).await.unwrap();
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(1100), script.command())
            .await
            .is_err()
    );
    for n in 0..512 {
        assert_eq!(raw.next::<ToClient>().await, ToClient::event(numbered(n)));
    }
    assert_eq!(raw.next::<ToClient>().await, ToClient::event(end()));
    script.ended(&mut raw).await;
}

#[tokio::test]
async fn a_window_past_the_waiting_bound_is_closed_and_noted_once() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut script = Script::new().await;
        let _raw = script.window().await;
        let output = AgentEvent::ToolOutput {
            call_id: "call".into(),
            text: "x".repeat(8 << 20),
        };
        for _ in 0..(wispd::WAITING / (8 << 20) + 2) {
            script.events.send(output.clone()).await.unwrap();
        }
        assert_eq!(
            script.commands.recv().await,
            Some(ClientCommand::WindowGone)
        );
        let errors = std::fs::read_to_string(link::error_file(&script.path)).unwrap();
        let lines: Vec<_> = errors.lines().collect();
        assert_eq!(lines.len(), 1);
        let (time, words) = lines[0].split_once(' ').unwrap();
        assert_eq!(time.len(), 20);
        assert!(time.ends_with('Z'));
        assert!(time.parse::<jiff::Timestamp>().is_ok());
        assert_eq!(
            words,
            format!(
                "window closed: it stopped reading, and more than {} MiB was waiting for it",
                wispd::WAITING >> 20
            )
        );
        let probe = client::connect(&script.path, 7).await.unwrap();
        assert!(!probe.welcome.window);
        drop(probe);
        let mut raw = Raw::connect(&script.path).await;
        script.ended(&mut raw).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn replay_joins_deltas_and_caps_each_calls_and_processes_output_on_utf8_boundaries() {
    let mut script = Script::new().await;
    let first = script.open().await;
    drop(first);
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    for item in [
        text("one"),
        text("two"),
        AgentEvent::ToolOutput {
            call_id: "a".into(),
            text: "é".repeat(40000),
        },
        AgentEvent::ToolOutput {
            call_id: "a".into(),
            text: "last".into(),
        },
        AgentEvent::ProcessOutput {
            id: 1,
            text: "z".repeat(70000),
            skipped: 0,
        },
        AgentEvent::ProcessOutput {
            id: 1,
            text: "last".into(),
            skipped: 0,
        },
    ] {
        script.events.send(item).await.unwrap();
    }
    let (opening, _) = reopening(&script).await;
    let request = script.describe().await;
    script
        .snapshot(request, SessionView::default(), vec![])
        .await;
    let mut link = opening.await.unwrap();
    assert_eq!(event(&mut link).await, text("onetwo"));
    let AgentEvent::ToolOutput { text, .. } = event(&mut link).await else {
        unreachable!()
    };
    assert!(text.ends_with("last") && text.len() <= 65536);
    let AgentEvent::ProcessOutput { text, skipped, .. } = event(&mut link).await else {
        unreachable!()
    };
    assert_eq!(text.len(), 65536);
    assert!(text.ends_with("last") && skipped > 0);
    script.close(&mut link, false).await;
}

#[tokio::test]
async fn a_turn_ending_during_a_description_is_shown_and_one_in_the_view_is_shown_only_once() {
    for in_view in [false, true] {
        let mut script = Script::new().await;
        let first = script.open().await;
        drop(first);
        assert_eq!(script.command().await, ClientCommand::WindowGone);
        let (opening, _) = reopening(&script).await;
        let request = script.describe().await;
        let turn = vec![
            AgentEvent::turn_started(2),
            text("placeholder answer"),
            end(),
        ];
        for event in &turn {
            script.events.send(event.clone()).await.unwrap();
        }
        script
            .snapshot(
                request,
                SessionView {
                    events: if in_view { turn.clone() } else { vec![] },
                    ..Default::default()
                },
                vec![],
            )
            .await;
        let mut link = opening.await.unwrap();
        let mut shown = link.view.events.clone();
        script.events.send(text("live")).await.unwrap();
        loop {
            let next = event(&mut link).await;
            if next == text("live") {
                break;
            }
            shown.push(next);
        }
        assert_eq!(shown, turn);
        script.close(&mut link, false).await;
    }
    let mut script = Script::new().await;
    let first = script.open().await;
    drop(first);
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    let connection = script.connection(8).await;
    let opening = tokio::spawn(async move { client::open(connection).await });
    let request = script.describe().await;
    for item in [
        AgentEvent::turn_started(2),
        text("completed before refusal"),
        end(),
        AgentEvent::refused(request, "CANARY-INTERNAL-DESCRIBE"),
    ] {
        script.events.send(item).await.unwrap();
    }
    let failed = tokio::time::timeout(WAIT, opening).await.unwrap().unwrap();
    let error = match failed {
        Err(error) => error,
        Ok(_) => panic!("a rejected description opened a window"),
    };
    assert!(!format!("{error:?} {error}").contains("CANARY"));
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    let (opening, _) = reopening(&script).await;
    let request = script.describe().await;
    script
        .snapshot(request, SessionView::default(), vec![])
        .await;
    let mut link = opening.await.unwrap();
    script
        .events
        .send(text("after recovered description"))
        .await
        .unwrap();
    assert_eq!(event(&mut link).await, text("after recovered description"));
    script.close(&mut link, false).await;
}

#[tokio::test]
async fn the_same_link_serves_an_admitted_non_socket_stream() {
    let (window, host) = tokio::io::duplex(4096);
    let mut host = Raw(BufReader::new(host));
    let welcome: ToClient =
        serde_json::from_str(&support::fixture_text("link_welcome.json")).unwrap();
    host.send(&welcome).await;
    let mut connection = client::handshake(window, 7).await.unwrap();
    assert!(matches!(host.next::<ToHost>().await, ToHost::Hello { .. }));
    for frame in [ToClient::Restarting, ToClient::Ended { saved: true }] {
        let encoded = wisp_protocol::jsonl::line(&frame).unwrap();
        let (first, rest) = encoded.as_bytes().split_at(encoded.len() / 2);
        host.0.write_all(first).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), connection.next())
                .await
                .is_err()
        );
        host.0.write_all(rest).await.unwrap();
        assert_eq!(
            tokio::time::timeout(WAIT, connection.next())
                .await
                .unwrap()
                .unwrap(),
            frame
        );
    }
    drop(connection);
    let mut script = Script::new().await;
    let (window, host) = tokio::io::duplex(64);
    tokio::spawn(wispd::serve(host, script.handle.clone()));
    let connection = client::handshake(window, 7).await.unwrap();
    let mut link = client::open(connection).await.unwrap();
    for n in 0..client::READ_AHEAD {
        script.events.send(text(&n.to_string())).await.unwrap();
    }
    tokio::time::timeout(WAIT, async {
        while link.channels.events.len() < client::READ_AHEAD {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for n in 0..client::READ_AHEAD {
        assert_eq!(event(&mut link).await, text(&n.to_string()));
    }
    script.close(&mut link, false).await;
}

#[tokio::test]
async fn each_agent_keeps_only_events_after_its_own_turn_end() {
    let mut script = Script::new().await;
    let mut first = script.open().await;
    let child = |event| AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::new("/worker"),
        event: Box::new(event),
    };
    for expected in [
        AgentEvent::turn_started(2),
        text("main completed"),
        child(AgentEvent::turn_started(1)),
        child(text("child in flight")),
        end(),
    ] {
        script.events.send(expected.clone()).await.unwrap();
        assert_eq!(event(&mut first).await, expected);
    }
    drop(first);
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    let (opening, _) = reopening(&script).await;
    let request = script.describe().await;
    script
        .snapshot(
            request,
            SessionView {
                events: vec![AgentEvent::turn_started(2), end()],
                ..Default::default()
            },
            vec![],
        )
        .await;
    let mut link = opening.await.unwrap();
    assert_eq!(event(&mut link).await, child(AgentEvent::turn_started(1)));
    assert_eq!(event(&mut link).await, child(text("child in flight")));
    script.close(&mut link, false).await;
}

#[tokio::test]
async fn an_unrequested_kernel_stop_is_not_reported_as_a_saved_end() {
    let mut script = Script::new().await;
    let mut link = script.open().await;
    let (unread, host) = tokio::io::duplex(1);
    let blocked = tokio::spawn(wispd::serve(host, script.handle.clone()));
    let mut raw = Raw(BufReader::new(unread));
    raw.send(&ToHost::Hello {
        contract: CONTRACT,
        build: 7,
    })
    .await;
    raw.0.read_u8().await.unwrap();
    let error_file = link::error_file(&script.path);
    std::fs::write(&error_file, "start placeholder\n").unwrap();
    let (unused, _) = mpsc::channel(1);
    drop(std::mem::replace(&mut script.events, unused));
    let (requested, _) = tokio::time::timeout(WAIT, &mut script.ending)
        .await
        .unwrap()
        .unwrap();
    assert!(!requested);
    script.saved.take().unwrap().send(true).unwrap();
    tokio::time::timeout(WAIT, link.ended.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*link.ended.borrow(), Some(LinkEnd::Lost));
    assert!(error_file.exists());
    tokio::time::timeout(WAIT, blocked).await.unwrap().unwrap();
}

#[tokio::test]
async fn unchecked_preconnected_clients_cannot_replace_the_window() {
    let mut script = Script::new().await;
    let (first, host) = tokio::io::duplex(128);
    tokio::spawn(wispd::serve(host, script.handle.clone()));
    let first = client::handshake(first, 7).await.unwrap();
    let (second, host) = tokio::io::duplex(128);
    tokio::spawn(wispd::serve(host, script.handle.clone()));
    let second = client::handshake(second, 7).await.unwrap();
    assert!(!first.welcome.window && !second.welcome.window);
    let mut first = client::open(first).await.unwrap();
    assert!(
        tokio::time::timeout(WAIT, client::open(second))
            .await
            .unwrap()
            .is_err()
    );
    first
        .channels
        .commands
        .send(ClientCommand::CheckProcesses)
        .await
        .unwrap();
    assert_eq!(script.command().await, ClientCommand::CheckProcesses);
    assert!(script.handle.end(false).await);
    assert!(script.handle.end(true).await);
    script.finish(&mut first, true).await;
}

#[tokio::test]
async fn shutdown_waits_until_the_ended_frame_is_written() {
    for reads in [true, false] {
        let mut script = Script::new().await;
        let (window, host) = tokio::io::duplex(1);
        tokio::spawn(wispd::serve(host, script.handle.clone()));
        let mut raw = Raw(BufReader::new(window));
        raw.send(&ToHost::Hello {
            contract: CONTRACT,
            build: 7,
        })
        .await;
        assert!(matches!(raw.next::<ToClient>().await, ToClient::Welcome(_)));
        raw.send(&ToHost::End { forced: false }).await;
        let (requested, forced) = tokio::time::timeout(WAIT, &mut script.ending)
            .await
            .unwrap()
            .unwrap();
        assert!(requested && !*forced.borrow());
        script.saved.take().unwrap().send(true).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !script.host.is_finished(),
            "the host returned before Ended could fit in the pipe"
        );
        if reads {
            assert_eq!(
                raw.next::<ToClient>().await,
                ToClient::Ended { saved: true }
            );
        }
        assert!(
            tokio::time::timeout(WAIT, &mut script.host)
                .await
                .unwrap()
                .unwrap()
        );
    }
}

#[tokio::test]
async fn outbound_hook_wraps_opening_replay_and_live_batches_and_fails_closed() {
    let (seen, mut batches) = mpsc::unbounded_channel();
    let omitted = ToClient::event(text("omit"));
    let skip = omitted.clone();
    let mut script = Script::with(SessionView::default(), 1, |session| {
        Host::new(session).with_outbound(move |frames, publish| {
            for frame in &mut *frames {
                if let ToClient::Event(event) = frame
                    && let AgentEvent::TextDelta { text, .. } = event.as_mut()
                    && text == "live"
                {
                    *text = "projected".into();
                }
            }
            if !matches!(frames, [ToClient::Welcome(_)]) {
                seen.send(frames.to_vec()).unwrap();
            }
            if frames != [skip.clone()] {
                publish(frames);
                publish(frames);
            }
        })
    })
    .await;
    let buffered = ToClient::event(text("pre-open"));
    let connection = script.buffer_before_open(text("pre-open")).await;
    let mut first = client::open(connection).await.unwrap();
    assert_eq!(event(&mut first).await, text("pre-open"));
    let batch = batches.try_recv().unwrap();
    assert!(matches!(&batch[..], [ToClient::Opened { now: None, .. }, e] if *e == buffered));
    drop(first);
    assert_eq!(script.command().await, ClientCommand::WindowGone);

    let (opening, _) = reopening(&script).await;
    let request = script.describe().await;
    let state = AgentEvent::AgentsSnapshot {
        request: None,
        agents: vec![],
    };
    script
        .snapshot(request, SessionView::default(), vec![state.clone()])
        .await;
    let mut second = opening.await.unwrap();
    assert_eq!(event(&mut second).await, state);
    assert_eq!(event(&mut second).await, text("pre-open"));
    let batch = batches.try_recv().unwrap();
    assert!(
        matches!(&batch[..], [ToClient::Opened { now: Some(_), .. }, s, e]
        if *s == ToClient::event(state) && *e == buffered)
    );

    script.events.send(text("live")).await.unwrap();
    assert_eq!(event(&mut second).await, text("projected"));
    assert_eq!(
        batches.try_recv().unwrap(),
        [ToClient::event(text("projected"))]
    );
    script.events.send(text("omit")).await.unwrap();
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    assert_eq!(batches.try_recv().unwrap(), [omitted]);
    assert!(batches.try_recv().is_err());
    assert_eq!(
        tokio::time::timeout(WAIT, second.channels.events.recv())
            .await
            .unwrap(),
        None
    );
    assert_eq!(*second.ended.borrow(), Some(LinkEnd::Lost));
}
