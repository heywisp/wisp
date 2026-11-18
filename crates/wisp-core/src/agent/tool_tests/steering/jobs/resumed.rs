use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::{RequestId, Role};

use super::*;

pub(super) fn reopened(name: &str, plan: Vec<Step>, records: &[LogRecord]) -> Harness {
    let env = Env::new(name);
    let (tx, rx) = mpsc::channel(128);
    let (staged, provider) = staged(plan);
    let (store, log) = continued(records);
    let agent = Agent::new(staged, default_session())
        .with_clock(TickingClock::new())
        .with_tools(job_tools(), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap();
    Harness {
        agent,
        provider,
        tx,
        rx,
        store,
        _env: env,
    }
}

fn texts(message: &Message) -> Vec<String> {
    assert_eq!(message.role, Role::User);
    message
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::ToolResult { content, .. } => content
                .iter()
                .map(|c| match c {
                    ContentBlock::Text { text } => text.clone(),
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            other => format!("{other:?}"),
        })
        .collect()
}

fn heard(records: &[LogRecord]) -> Vec<u32> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::ProcessHeard { ids, .. } => Some(ids.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn ended_early() -> ExitGate {
    ExitGate::new("b.go", 4)
}

fn before() -> String {
    format!(
        "Process 2 ({}) exited with code 4 before Wisp restarted.",
        ended_early().shown()
    )
}

async fn an_earlier_run(h: &mut Harness) -> Vec<LogRecord> {
    let events = h.run(vec![steer("go")], 1).await;
    let numbered = events.iter().find_map(|e| match e {
        AgentEvent::ToolCallFinished {
            call_id, process, ..
        } if call_id == "a" => Some(*process),
        _ => None,
    });
    assert_eq!(numbered, Some(Some(1)), "{events:#?}");
    ended_early().open(&h._env.work);
    let later = h
        .drive(Vec::new(), nothing, process_ended(2), PATIENT)
        .await;
    assert!(process_ended(2)(&later), "{later:#?}");
    records(&h.store)
}

fn earlier_plan() -> Vec<Step> {
    vec![
        uses(&[
            call("a", "bash", json!({"command": "seq 1 4000"})),
            call(
                "b",
                "bash",
                json!({"command": ended_early().command, "run_in_background": true}),
            ),
        ]),
        done("started"),
    ]
}

#[tokio::test]
async fn after_a_resume_process_reads_a_foreground_number_with_its_real_end() {
    if !have_bash() {
        return;
    }
    let mut first = jobs("resume-foreground", earlier_plan(), false);
    let records = an_earlier_run(&mut first).await;
    let mut h = reopened(
        "resume-foreground-2",
        vec![
            uses(&[
                call("c", "process", json!({"id": 1, "read": "tail", "lines": 2})),
                call("d", "process", json!({"id": 1, "read": "screen"})),
                call("e", "process", json!({"id": 2, "read": "screen"})),
            ]),
            done("read"),
        ],
        &records,
    );
    h.run(vec![steer("next")], 1).await;
    let requests = h.requests();
    let opening = texts(requests[0].last().unwrap());
    assert!(
        opening.contains(&format!("<processes>\n{}\n</processes>", before())),
        "{opening:?}"
    );
    let results = texts(requests[1].last().unwrap());
    assert!(
        results[0].starts_with("Process 1 exited with code 0.\n"),
        "{results:?}"
    );
    assert!(results[0].contains("3999\n4000"), "{results:?}");
    assert!(results[1].contains("reads a pipe"), "{results:?}");
    assert!(results[2].contains("reads a pipe"), "{results:?}");
    assert_eq!(heard(&test_guard::records(&h.store)), [2]);
}

#[tokio::test]
async fn a_retry_resends_the_restart_notice_and_its_commit_hears_it() {
    if !have_bash() {
        return;
    }
    let mut first = jobs("resume-retry", earlier_plan(), false);
    let records = an_earlier_run(&mut first).await;
    let mut h = reopened(
        "resume-retry-2",
        vec![server_error_step(), done("again")],
        &records,
    );
    let failed = h.run(vec![steer("next")], 1).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    assert!(heard(&test_guard::records(&h.store)).is_empty());
    let turn = records
        .iter()
        .filter(|r| matches!(r.event, LogEvent::TurnStarted { .. }))
        .count() as u64
        + 1;
    h.run(
        vec![ClientCommand::Retry {
            turn,
            endpoint: None,
            request: RequestId(1),
        }],
        1,
    )
    .await;
    let requests = h.requests();
    assert_eq!(requests.len(), 2);
    let block = format!("<processes>\n{}\n</processes>", before());
    assert!(texts(requests[0].last().unwrap()).contains(&block));
    assert!(texts(requests[1].last().unwrap()).contains(&block));
    assert_eq!(heard(&test_guard::records(&h.store)), [2]);
}
