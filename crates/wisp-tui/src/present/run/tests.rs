use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ErrorKind, RetryFocus};

use crate::app::replay::cancelled;
use crate::events::{End, Ending, turn_ended};
use crate::shared::offer;
use wisp_client::{interrupts, turn};

fn interrupted() -> AgentEvent {
    turn_ended(End {
        ending: Ending::Error(ErrorKind::Interrupted),
        message: "Wisp stopped before this turn ended.".to_owned(),
        committed_steps: 1,
        offer: Some(offer(true, false, RetryFocus::Retry)),
        ..End::default()
    })
}

#[tokio::test]
async fn a_turn_closed_at_resume_before_the_first_message_does_not_end_that_message() {
    let (commands, _sent) = mpsc::channel(8);
    let (events, mut received) = mpsc::channel(16);
    for event in [
        interrupted(),
        AgentEvent::turn_started(4),
        cancelled(),
        AgentEvent::turn_started(5),
    ] {
        events.send(event).await.unwrap();
    }
    let mut interrupt = interrupts().unwrap();
    let mut shown = Vec::new();
    turn(&commands, &mut received, &mut interrupt, |event| {
        shown.push(event.clone());
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        shown,
        vec![interrupted(), AgentEvent::turn_started(4), cancelled()]
    );
    assert_eq!(received.try_recv().ok(), Some(AgentEvent::turn_started(5)));
}
