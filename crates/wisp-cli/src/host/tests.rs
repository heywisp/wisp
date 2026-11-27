#![allow(
    clippy::unwrap_used,
    reason = "hosted regression tests fail on transport or task errors"
)]

#[path = "../../../wispd/tests/support/script.rs"]
mod script;

use std::path::PathBuf;

use script::{Script, end, event, reopening, text};
use wisp_client::link as client;
use wisp_core::session::{REDACTED, register_secrets};
use wisp_protocol::{AgentEvent, AgentPath, ClientCommand, SessionView, TodoItem, TodoStatus};

fn todo(text: &str, section: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        text: text.into(),
        section: Some(section.into()),
        status,
    }
}

#[tokio::test]
async fn hosted_first_open_and_reattach_scrub_cached_checklists_against_current_secrets_and_home() {
    let early = "CANARYOpaqueInFirstHostOpen";
    let late = "CANARYOpaqueRegisteredAfterFirstHostOpen";
    let home_section = "/opt/wisp-test-home/notes";
    let safe = todo("Read the plan", "Next", TodoStatus::Completed);
    let checklist = |text, section| AgentEvent::TodosUpdated {
        call_id: Some("t1".into()),
        items: vec![todo(text, section, TodoStatus::InProgress), safe.clone()],
    };
    let child = |event| AgentEvent::Agent {
        agent: AgentPath::new("/worker"),
        event: Box::new(event),
    };
    let view = |events| SessionView {
        events,
        ..SessionView::default()
    };
    let mut script = Script::with(view(vec![checklist(early, home_section)]), 1, |session| {
        super::build_host(session, Some(PathBuf::from("/opt/wisp-test-home")))
    })
    .await;
    let connection = script
        .buffer_before_open(child(checklist("Draft notes", early)))
        .await;
    register_secrets([early.to_owned()]);
    let mut first = client::open(connection).await.unwrap();
    assert_eq!(first.view.events, [checklist(REDACTED, REDACTED)]);
    let first_child = child(checklist("Draft notes", REDACTED));
    assert_eq!(event(&mut first).await, first_child);

    let cached_child = child(checklist(late, "Next"));
    for live in [
        AgentEvent::turn_started(2),
        child(AgentEvent::turn_started(1)),
        cached_child,
        end(),
    ] {
        script.events.send(live.clone()).await.unwrap();
        assert_eq!(event(&mut first).await, live);
    }
    drop(first);
    assert_eq!(script.command().await, ClientCommand::WindowGone);
    register_secrets([late.to_owned()]);

    let (opening, _) = reopening(&script).await;
    let request = script.describe().await;
    script
        .snapshot(request, view(vec![checklist(late, home_section)]), vec![])
        .await;
    let mut second = opening.await.unwrap();
    assert_eq!(second.view.events, [checklist(REDACTED, REDACTED)]);
    for expected in [
        first_child,
        child(AgentEvent::turn_started(1)),
        child(checklist(REDACTED, "Next")),
    ] {
        assert_eq!(event(&mut second).await, expected);
    }
    let live = text("still streaming");
    script.events.send(live.clone()).await.unwrap();
    assert_eq!(event(&mut second).await, live);
    script.close(&mut second, false).await;
}
