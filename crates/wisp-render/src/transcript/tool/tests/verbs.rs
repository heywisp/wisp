use wisp_protocol::Verbs;

use super::*;
use crate::events::{Requested, tool_requested};

fn testkit_verbs() -> Verbs {
    let verbs = wisp_plugin_testkit::verbs();
    Verbs {
        doing: verbs.doing,
        done: verbs.done,
        failed: verbs.failed,
    }
}

fn call(
    block: usize,
    id: &str,
    tool: &str,
    action: ToolAction,
    verbs: Option<Verbs>,
) -> AgentEvent {
    tool_requested(Requested {
        block,
        call_id: id.to_owned(),
        tool: tool.to_owned(),
        input: json!({"what": "tea"}),
        action,
        verbs,
        target: "tea".to_owned(),
        ..Requested::default()
    })
}

fn refused(id: &str) -> AgentEvent {
    let mut event = finished(
        id,
        ToolOutcome::Completed {
            is_error: true,
            exit: None,
        },
        ToolOutputView::default(),
        0,
    );
    if let AgentEvent::ToolCallFinished { result, .. } = &mut event {
        *result = Some("this tool is turned off".to_owned());
    }
    event
}

fn turn(calls: &[(&str, &str, ToolAction, Option<Verbs>)], end: bool) -> Transcript {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(&AgentEvent::StepStarted { step: 1 }, ms(0));
    for (block, (id, tool, action, verbs)) in calls.iter().enumerate() {
        t.apply(&tool_block(block), ms(0));
        t.apply(&call(block, id, tool, action.clone(), verbs.clone()), ms(0));
        t.apply(&started(id), ms(0));
    }
    if end {
        for (id, ..) in calls {
            let done = if *id == "refused" {
                refused(id)
            } else {
                finished(id, ok(0), view("", 0), 1_000)
            };
            t.apply(&done, ms(1_000));
        }
    }
    t
}

fn brews() -> Vec<(&'static str, &'static str, ToolAction, Option<Verbs>)> {
    let brew = ToolAction::Other {
        name: "brew".to_owned(),
    };
    vec![
        ("brewed", "brew", brew.clone(), Some(testkit_verbs())),
        ("refused", "brew", brew, Some(testkit_verbs())),
    ]
}

#[test]
fn a_plugins_own_verbs_draw_its_rows_while_running_done_and_refused() {
    let r = r(80);
    let running = text(&turn(&brews(), false).rows(&r, Detail::Scrollback));
    assert!(running.contains("Brewing tea"), "{running}");
    let done = text(&turn(&brews(), true).rows(&r, Detail::Scrollback));
    assert!(done.contains("Brewed tea"), "{done}");
    assert!(done.contains("couldn't brew"), "{done}");
    assert!(!done.contains("Used"), "{done}");
    insta::assert_snapshot!(
        "plugin_verbs",
        format!("=== running ===\n{running}\n=== done ===\n{done}")
    );
}

#[test]
fn an_unknown_action_draws_as_a_use_of_the_named_tool() {
    let calls = [("teleport", "teleport", ToolAction::Unknown, None)];
    let r = r(80);
    let running = text(&turn(&calls, false).rows(&r, Detail::Scrollback));
    assert!(running.contains("Using teleport tea"), "{running}");
    let done = text(&turn(&calls, true).rows(&r, Detail::Scrollback));
    assert!(done.contains("Used teleport tea"), "{done}");
    insta::assert_snapshot!(
        "unknown_action",
        format!("=== running ===\n{running}\n=== done ===\n{done}")
    );
}

#[test]
fn verbs_are_cut_to_one_line_before_they_are_drawn() {
    let verbs = Verbs {
        doing: "Brew\ning".to_owned(),
        done: "Brew\ned".to_owned(),
        failed: "couldn't\nbrew".to_owned(),
    };
    let brew = ToolAction::Other {
        name: "brew".to_owned(),
    };
    let calls = [("brewed", "brew", brew, Some(verbs))];
    let done = text(&turn(&calls, true).rows(&r(80), Detail::Scrollback));
    assert_eq!(
        done.lines().filter(|l| l.contains("tea")).count(),
        1,
        "{done}"
    );
}
