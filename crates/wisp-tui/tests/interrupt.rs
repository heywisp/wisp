#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests fail on a missing value"
)]

use std::time::Duration;

use crate::events::{End, Ending, turn_ended};
use signal_hook::consts::SIGINT;
use signal_hook::low_level::raise;
use tokio::sync::mpsc;
use wisp_client::{interrupts, turn};
use wisp_protocol::{AgentEvent, BlockKind, ClientCommand};

#[path = "../../wisp-render/tests/support/events.rs"]
mod events;

#[tokio::test]
async fn a_sigint_delivered_while_rendering_still_cancels() {
    for round in 0..5 {
        let (commands, mut sent) = mpsc::channel(8);
        let (events, mut received) = mpsc::channel(64);
        events.send(AgentEvent::turn_started(1)).await.unwrap();
        events
            .send(AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            })
            .await
            .unwrap();
        for i in 0..20 {
            events
                .send(AgentEvent::TextDelta {
                    index: 0,
                    text: format!("line {i}\n"),
                })
                .await
                .unwrap();
        }
        let agent = tokio::spawn(async move {
            let command = sent.recv().await;
            events
                .send(turn_ended(End {
                    ending: Ending::Cancelled,
                    ..End::default()
                }))
                .await
                .unwrap();
            command
        });
        let mut interrupt = interrupts().unwrap();
        let mut shown = 0;
        let ended = tokio::time::timeout(
            Duration::from_secs(10),
            turn(&commands, &mut received, &mut interrupt, |event| {
                shown += 1;
                if matches!(event, AgentEvent::TextDelta { .. }) && shown == 3 + round {
                    raise(SIGINT).unwrap();
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(())
            }),
        )
        .await;
        assert!(matches!(ended, Ok(Ok(()))), "round {round}: {ended:?}");
        assert_eq!(
            agent.await.unwrap(),
            Some(ClientCommand::Cancel),
            "round {round}"
        );
    }
}
