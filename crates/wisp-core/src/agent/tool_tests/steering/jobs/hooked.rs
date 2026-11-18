use wisp_plugin_api::hooks::Reply;
use wisp_plugin_testkit::hooks::Calls;
use wisp_protocol::HookChange;
use wisp_protocol::log::LogEvent;

use super::*;
use crate::plugin::testing::{hook_body, hook_source, replying, with_test_hooks};

#[tokio::test]
async fn a_job_end_comes_after_after_tool_notes_and_survives_a_rewrite_heard_once() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 3);
    let Harness {
        agent,
        provider,
        tx,
        rx,
        store,
        _env,
    } = jobs(
        "hooked-end",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": &a.command, "run_in_background": true}),
            )]),
            uses(&[call("b", "bash", json!({"command": "sleep 2"}))]),
            done("ok"),
        ],
        false,
    );
    let calls = Calls::default();
    let sources = vec![
        hook_source(
            "aa",
            &hook_body("wrap", "after-tool", ""),
            replying(Reply::Rewrite("R".into())),
            &calls,
        ),
        hook_source(
            "bb",
            &hook_body("note", "after-tool", "after = [\"aa/wrap\"]\n"),
            replying(Reply::Append(vec!["N".into()])),
            &calls,
        ),
    ];
    let mut h = Harness {
        agent: with_test_hooks(agent, sources),
        provider,
        tx,
        rx,
        store,
        _env,
    };
    let events = h
        .drive(
            vec![steer("go")],
            a.opens_at(h._env.work.clone(), "b"),
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&events), 1, "{events:#?}");
    check_events(&events);
    let line = a.ended(1, 3);
    assert_eq!(result_of(&h, "b"), format!("R\nN\n{line}"));
    assert_eq!(result_of(&h, "a"), "R\nN");
    let logged = log_events(&h.store);
    let heard: Vec<&LogEvent> = logged
        .iter()
        .filter(|e| matches!(e, LogEvent::ProcessHeard { .. }))
        .collect();
    assert_eq!(heard.len(), 1, "{heard:?}");
    let last = logged
        .iter()
        .rev()
        .find_map(|e| match e {
            LogEvent::HookApplied {
                call_id: Some(id),
                change: HookChange::Result { after, .. },
                ..
            } if id == "b" => Some(after.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last, format!("R\nN\n{line}"));
}
