use super::*;
use crate::test_guard::{continued, log_context};

fn resumed(name: &str, records: &[LogRecord], plan: Vec<Step>) -> Harness {
    let env = Env::new(name);
    let (tx, rx) = mpsc::channel(128);
    let (staged, provider) = staged(plan);
    let (store, log) = continued(records);
    let agent = Agent::new(staged, default_session())
        .with_clock(TickingClock::new())
        .with_tools(job_tools(), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(QUICK)
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

struct Live {
    records: Vec<LogRecord>,
    ends: Vec<ReportedEnd>,
    notice: String,
}

async fn live_wake(name: &str, wake: Step) -> Live {
    let mut h = jobs(
        name,
        vec![
            uses(&[
                background("a", "exec sleep 30"),
                background(
                    "b",
                    &format!("{}; exit 2", crate::test_guard::wait_for("go")),
                ),
            ]),
            done("started"),
            wake,
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    std::fs::write(h._env.work.join("go"), "").unwrap();
    let events = woke(&mut h).await;
    let ends = reported(&events).concat();
    assert_eq!(ends, [exited(2, 2)]);
    let records = records(&h.store);
    let notice = opened(&records)
        .into_iter()
        .find_map(|(turn, _, processes, _)| processes.filter(|_| turn == 2))
        .unwrap();
    Live {
        records,
        ends,
        notice,
    }
}

fn retry(turn: u64) -> ClientCommand {
    ClientCommand::Retry {
        turn,
        endpoint: None,
        request: RequestId(1),
    }
}

async fn retried(h: &mut Harness, turn: u64, ended: usize) -> (Vec<ReportedEnd>, Option<String>) {
    let events = h.run(vec![retry(turn)], ended).await;
    assert!(
        matches!(
            outcomes(&events).last(),
            Some(TurnOutcome::Completed { .. })
        ),
        "{events:#?}"
    );
    let ends = reported(&events).concat();
    let records = records(&h.store);
    let notice = opened(&records)
        .into_iter()
        .next_back()
        .and_then(|(_, _, processes, _)| processes);
    (ends, notice)
}

fn logged_ends(records: &[LogRecord], at: u64) -> Vec<ReportedEnd> {
    records
        .iter()
        .find_map(|r| match &r.event {
            LogEvent::TurnStarted { turn, ends, .. } if *turn == at => Some(ends.clone()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn a_crashed_wake_turn_retries_with_the_ends_and_notice_the_live_turn_sent() {
    if !have_bash() {
        return;
    }
    let live = live_wake("wake-crash-ends", done("woke")).await;
    assert_eq!(logged_ends(&live.records, 2), live.ends);
    let start = live
        .records
        .iter()
        .position(|r| matches!(r.event, LogEvent::TurnStarted { turn: 2, .. }))
        .unwrap();
    let cut = &live.records[..=start];
    let mut h = resumed("wake-crash-ends-resumed", cut, vec![done("again")]);
    let (ends, notice) = retried(&mut h, 2, 2).await;
    assert_eq!(ends, live.ends);
    assert_eq!(notice.as_deref(), Some(live.notice.as_str()));
    assert!(last_sent(&h).contains(&live.notice), "{:#?}", last_sent(&h));
}

#[tokio::test]
async fn a_failed_wake_turn_retries_after_a_resume_with_the_same_ends_and_notice() {
    if !have_bash() {
        return;
    }
    let live = live_wake("wake-failed-ends", server_error_step()).await;
    let mut h = resumed(
        "wake-failed-ends-resumed",
        &live.records,
        vec![done("again")],
    );
    let (ends, notice) = retried(&mut h, 2, 1).await;
    assert_eq!(ends, live.ends);
    assert_eq!(notice.as_deref(), Some(live.notice.as_str()));
    assert_eq!(logged_ends(&records(&h.store), 3), live.ends);
}

#[tokio::test]
async fn an_old_log_without_ends_retries_its_notice_with_no_ends() {
    if !have_bash() {
        return;
    }
    let live = live_wake("wake-old-ends", server_error_step()).await;
    let old: Vec<LogRecord> = live
        .records
        .iter()
        .map(|r| {
            let mut value = serde_json::to_value(r).unwrap();
            value.as_object_mut().unwrap().remove("ends");
            serde_json::from_value(value).unwrap()
        })
        .collect();
    assert!(logged_ends(&old, 2).is_empty());
    let mut h = resumed("wake-old-ends-resumed", &old, vec![done("again")]);
    let (ends, notice) = retried(&mut h, 2, 1).await;
    assert!(ends.is_empty(), "{ends:?}");
    assert_eq!(notice.as_deref(), Some(live.notice.as_str()));
}
