use wisp_protocol::log::{LEAD_PROCESSES, LogEvent, LogRecord};
use wisp_protocol::{DeliveryMode, ImageData, Role};

use super::*;

fn each_heard_follows_a_commit(records: &[LogRecord]) {
    for (at, record) in records.iter().enumerate() {
        if matches!(record.event, LogEvent::ProcessHeard { .. }) {
            assert!(
                matches!(records[at - 1].event, LogEvent::MessageCommitted { .. }),
                "{:#?}",
                records[at - 1]
            );
        }
    }
}

fn notices(records: &[LogRecord]) -> Vec<(u64, Option<String>)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::TurnStarted { turn, lead, .. } => {
                Some((*turn, lead.text(LEAD_PROCESSES).map(str::to_owned)))
            }
            _ => None,
        })
        .collect()
}

fn last_texts(messages: &[Message]) -> Vec<String> {
    let last = messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    last.content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { .. } => "<image>".to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

async fn until_ended(h: &mut Harness, id: u32) {
    let events = h
        .drive(Vec::new(), nothing, process_ended(id), PATIENT)
        .await;
    assert!(
        process_ended(id)(&events),
        "process {id} never ended: {events:#?}"
    );
}

#[tokio::test]
async fn an_exit_during_a_turn_is_a_line_on_the_next_result_only() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 3);
    let mut h = jobs(
        "notice-in-turn",
        vec![
            uses(&[background("a", &a.command)]),
            uses(&[
                call("b", "bash", json!({"command": "sleep 2"})),
                call("c", "bash", json!({"command": "true"})),
            ]),
            uses(&[call("d", "bash", json!({"command": "true"}))]),
            done("ok"),
        ],
        false,
    );
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
    let b = result_of(&h, "b");
    assert!(b.ends_with(&format!("\n{line}")), "{b}");
    for id in ["a", "c", "d"] {
        let text = result_of(&h, id);
        assert!(!text.contains("exited with code 3"), "{id}: {text}");
    }
    let records = records(&h.store);
    assert_eq!(heard(&records), [(1, vec![1])]);
    each_heard_follows_a_commit(&records);
    h.assert_rebuilds();
}

#[tokio::test]
async fn an_end_follows_the_working_directory_note() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 3);
    let mut h = jobs(
        "notice-after-cwd",
        vec![
            uses(&[background("a", &a.command)]),
            uses(&[call(
                "b",
                "bash",
                json!({"command": "sleep 2; mkdir -p sub && cd sub"}),
            )]),
            done("ok"),
        ],
        false,
    );
    let events = h
        .drive(
            vec![steer("go")],
            a.opens_at(h._env.work.clone(), "b"),
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&events), 1, "{events:#?}");
    let b = result_of(&h, "b");
    let cwd = b.find("\nWorking directory: ").expect(&b);
    let end = b.find(&format!("\n{}", a.ended(1, 3))).expect(&b);
    assert!(cwd < end, "{b}");
    assert!(b.ends_with("exited with code 3."), "{b}");
}

#[tokio::test]
async fn a_start_that_ends_in_its_window_reports_its_own_end_once() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "notice-start",
        vec![
            uses(&[background("a", "exit 4")]),
            uses(&[call("b", "bash", json!({"command": "true"}))]),
            done("ok"),
            done("again"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    let a = result_of(&h, "a");
    assert_eq!(a.matches("exited with code 4").count(), 1);
    assert!(
        a.starts_with("Process 1 (exit 4) exited with code 4."),
        "{a}"
    );
    assert!(!result_of(&h, "b").contains("exited"));
    h.run(vec![steer("next")], 1).await;
    let requests = h.requests();
    assert!(
        !last_texts(requests.last().unwrap())
            .iter()
            .any(|t| t.contains("<processes>"))
    );
    let records = records(&h.store);
    assert_eq!(heard(&records), [(1, vec![1])]);
    each_heard_follows_a_commit(&records);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_process_answer_or_list_that_shows_the_end_gets_no_line() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 5);
    let b = ExitGate::new("b.go", 6);
    let mut h = jobs(
        "notice-process",
        vec![
            uses(&[background("a", &a.command), background("b", &b.command)]),
            uses(&[call("c", "process", json!({"id": 1, "wait_ms": 5000}))]),
            uses(&[call("d", "sleep", json!({"ms": 1500, "text": "x"}))]),
            uses(&[call("e", "process", json!({}))]),
            uses(&[call("f", "bash", json!({"command": "true"}))]),
            done("ok"),
        ],
        false,
    );
    let work = h._env.work.clone();
    let events = h
        .drive(
            vec![steer("go")],
            |e| {
                match e {
                    AgentEvent::ToolCallStarted { call_id, .. } if call_id == "c" => a.open(&work),
                    AgentEvent::ToolCallStarted { call_id, .. } if call_id == "d" => b.open(&work),
                    _ => {}
                }
                Vec::new()
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&events), 1, "{events:#?}");
    let c = result_of(&h, "c");
    assert!(c.starts_with("Process 1 exited with code 5."), "{c}");
    assert!(!c.contains("Process 1 ("), "{c}");
    let e = result_of(&h, "e");
    assert!(e.contains("exited with code 6"), "{e}");
    assert!(!e.contains("Process 2 ("), "{e}");
    assert!(!result_of(&h, "f").contains("exited"));
    let records = records(&h.store);
    assert_eq!(heard(&records), [(1, vec![1]), (1, vec![2])]);
    each_heard_follows_a_commit(&records);
}

#[tokio::test]
async fn a_time_limit_stop_is_heard_once() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "notice-limit",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "sleep 30", "run_in_background": true, "timeout_ms": 500}),
            )]),
            uses(&[call("b", "bash", json!({"command": "sleep 1.5"}))]),
            uses(&[call("c", "bash", json!({"command": "true"}))]),
            done("ok"),
            done("again"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    h.run(vec![steer("next")], 1).await;
    let b = result_of(&h, "b");
    assert!(
        b.ends_with("\nProcess 1 (sleep 30) was stopped at its 500 ms time limit."),
        "{b}"
    );
    let bodies = h.provider.texts().join("\n");
    assert_eq!(bodies.matches("was stopped at its").count(), 3, "{bodies}");
    assert!(!result_of(&h, "c").contains("stopped"));
    let records = records(&h.store);
    assert_eq!(heard(&records), [(1, vec![1])]);
    assert_eq!(notices(&records), [(1, None), (2, None)]);
}

#[tokio::test]
async fn an_exit_between_turns_is_in_the_next_turns_notice_once() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "notice-between",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("two"),
            done("three"),
        ],
        false,
    );
    h.run(vec![steer("one")], 1).await;
    a.open(&h._env.work);
    until_ended(&mut h, 1).await;
    h.run(vec![steer("two")], 1).await;
    h.run(vec![steer("three")], 1).await;
    let block = notice(&[&a.ended(1, 2)]);
    let requests = h.requests();
    let two = last_texts(&requests[2]);
    assert_eq!(two[two.len() - 2..], [block.clone(), "two".to_owned()]);
    assert!(
        !last_texts(&requests[3])
            .iter()
            .any(|t| t.contains("<processes>"))
    );
    let records = records(&h.store);
    assert_eq!(notices(&records), [(1, None), (2, Some(block)), (3, None)]);
    assert_eq!(heard(&records), [(2, vec![1])]);
    each_heard_follows_a_commit(&records);
    h.assert_rebuilds();
}

#[tokio::test]
async fn an_image_only_message_keeps_its_image_and_empty_text_after_the_notice() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "notice-image",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("a pixel"),
        ],
        false,
    );
    h.run(vec![steer("one")], 1).await;
    a.open(&h._env.work);
    until_ended(&mut h, 1).await;
    let image = ClientCommand::Send {
        id: InputId("tui-2".into()),
        text: String::new(),
        images: vec![ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    };
    h.run(vec![image], 1).await;
    let block = notice(&[&a.ended(1, 2)]);
    let requests = h.requests();
    let sent = last_texts(&requests[2]);
    assert_eq!(
        sent[sent.len() - 3..],
        [block, "<image>".to_owned(), String::new()]
    );
    assert_eq!(heard(&records(&h.store)), [(2, vec![1])]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn esc_while_the_first_request_streams_puts_the_notice_back() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "notice-esc",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            hanging(&[B::Text("partial")]),
            done("three"),
        ],
        false,
    );
    h.run(vec![steer("one")], 1).await;
    a.open(&h._env.work);
    until_ended(&mut h, 1).await;
    let stopped = h.run(vec![steer("two"), ClientCommand::Cancel], 1).await;
    assert!(matches!(
        outcomes(&stopped)[..],
        [TurnOutcome::Cancelled { .. }]
    ));
    h.run(vec![steer("three")], 1).await;
    let block = notice(&[&a.ended(1, 2)]);
    let requests = h.requests();
    assert!(last_texts(&requests[2]).contains(&block));
    assert!(last_texts(&requests[3]).contains(&block));
    let records = records(&h.store);
    assert_eq!(
        notices(&records),
        [(1, None), (2, Some(block.clone())), (3, Some(block))]
    );
    assert_eq!(heard(&records), [(3, vec![1])]);
    each_heard_follows_a_commit(&records);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_failed_first_request_puts_the_notice_back_for_a_new_message() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "notice-failed",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            server_error_step(),
            done("three"),
        ],
        false,
    );
    h.run(vec![steer("one")], 1).await;
    a.open(&h._env.work);
    until_ended(&mut h, 1).await;
    let failed = h.run(vec![steer("two")], 1).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    h.run(vec![steer("three")], 1).await;
    let block = notice(&[&a.ended(1, 2)]);
    let requests = h.requests();
    assert!(last_texts(&requests[2]).contains(&block));
    assert!(last_texts(&requests[3]).contains(&block));
    let records = records(&h.store);
    assert_eq!(
        notices(&records),
        [(1, None), (2, Some(block.clone())), (3, Some(block))]
    );
    assert_eq!(heard(&records), [(3, vec![1])]);
    each_heard_follows_a_commit(&records);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_line_on_a_stopped_steps_result_still_commits_and_is_repeated_until_heard() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs_with(
        "notice-stopped-step",
        vec![
            uses(&[background("a", &a.command)]),
            uses(&[call("b", "bash", json!({"command": "sleep 30"}))]),
            done("next"),
        ],
        true,
        job_tools(),
        crate::agent::CANCEL_GRACE,
    );
    let mut sent = false;
    let work = h._env.work.clone();
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolCallStarted { call_id, .. } if call_id == "b" => {
                    a.open(&work);
                    Vec::new()
                }
                AgentEvent::ProcessEnded { id: 1, .. } if !sent => {
                    sent = true;
                    vec![ClientCommand::Cancel]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Cancelled { .. }]
    ));
    let b = result_of(&h, "b");
    assert!(b.ends_with(&format!("\n{}", a.ended(1, 2))), "{b}");
    h.run(vec![steer("next")], 1).await;
    let requests = h.requests();
    assert!(last_texts(requests.last().unwrap()).contains(&notice(&[&a.ended(1, 2)])));
    let records = records(&h.store);
    assert_eq!(heard(&records), [(2, vec![1])]);
    each_heard_follows_a_commit(&records);
    h.assert_rebuilds();
}

#[derive(Default)]
struct Grabber {
    host: std::sync::Mutex<Option<Arc<dyn wisp_plugin_api::host::Host>>>,
}

impl crate::plugin::testing::TestTool for Grabber {
    fn name(&self) -> String {
        "grab".to_owned()
    }

    fn description(&self) -> String {
        "Take the process ends, then never stop.".to_owned()
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn meta(&self) -> wisp_plugin_api::tools::ToolMeta {
        wisp_plugin_api::tools::ToolMeta {
            saves_output: false,
            needs_workspace: false,
            ..crate::plugin::testing::meta(600_000, None)
        }
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        Ok(crate::plugin::testing::Call::boxed(
            crate::plugin::testing::fallback("grab", input),
            (),
        ))
    }

    fn attach(&self, host: Arc<dyn wisp_plugin_api::host::Host>) {
        *self.host.lock().unwrap() = Some(host);
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn wisp_plugin_api::tools::Prepared>,
        ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        use futures::FutureExt;
        let host = self.host.lock().unwrap().clone().unwrap();
        async move {
            host.mark_reported(1, ctx);
            std::future::pending().await
        }
        .boxed()
    }
}

#[tokio::test]
async fn an_abandoned_call_returns_the_ends_it_took() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let tools = shell(timing(QUICK_MS, WINDOW_MS)).with_entries([
        sleep_tool(),
        crate::plugin::testing::entry(Arc::new(Grabber::default())),
    ]);
    let mut h = jobs_with(
        "notice-abandoned",
        vec![
            uses(&[background("a", &a.command)]),
            uses(&[call("b", "sleep", json!({"ms": 1500, "text": "x"}))]),
            uses(&[call("c", "grab", json!({}))]),
            done("next"),
        ],
        true,
        tools,
        GRACE,
    );
    let work = h._env.work.clone();
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolCallStarted { call_id, .. } if call_id == "b" => {
                    a.open(&work);
                    Vec::new()
                }
                AgentEvent::ToolCallStarted { call_id, .. } if call_id == "c" => {
                    vec![ClientCommand::Cancel]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Cancelled { .. }]
    ));
    let c = result_of(&h, "c");
    assert!(c.contains(crate::agent::ABANDONED_NOTE), "{c}");
    assert!(!c.contains("exited"), "{c}");
    assert!(heard(&records(&h.store)).is_empty());
    h.run(vec![steer("next")], 1).await;
    let block = notice(&[&a.ended(1, 2)]);
    let requests = h.requests();
    assert!(last_texts(requests.last().unwrap()).contains(&block));
    let records = records(&h.store);
    assert_eq!(heard(&records), [(2, vec![1])]);
    each_heard_follows_a_commit(&records);
    h.assert_rebuilds();
}
