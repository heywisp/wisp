use std::collections::BTreeSet;
use std::sync::Arc;

use futures::FutureExt;
use wisp_plugin_api::hooks::Reply;
use wisp_plugin_api::host::Host;
use wisp_plugin_testkit::hooks::{self as kit, Calls};
use wisp_protocol::HookOutcome;

use super::*;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};

#[derive(Clone, Copy)]
enum Unworded {
    Rejected,
    Failed,
    TimedOut,
    SlowlyRejected,
}

fn unworded_hook(how: Unworded) -> (String, Make) {
    let script: kit::Script = Arc::new(move |host: Arc<dyn Host>, _, _| {
        drop(host.opening());
        match how {
            Unworded::Rejected => Box::pin(async { Reply::Append(vec!["  ".to_owned()]) }),
            Unworded::Failed => Box::pin(async { panic!("no words") }),
            Unworded::TimedOut => Box::pin(async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Reply::None
            }),
            Unworded::SlowlyRejected => Box::pin(async {
                let [waiting, go, heard] = late_end_marks();
                if !waiting.exists() {
                    return Reply::Append(vec!["  ".to_owned()]);
                }
                std::fs::write(&go, "").unwrap();
                while !heard.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Reply::Append(vec!["  ".to_owned()])
            }),
        }
    });
    let timeout = match how {
        Unworded::SlowlyRejected => 10_000,
        _ => 100,
    };
    let body = format!(
        "\n[[hooks]]\nname = \"words\"\npoint = \"before-turn\"\nin = [\"core/news\"]\njob_news = true\ntimeout_ms = {timeout}\ncontinues = \"core/processes\"\n"
    );
    let make: Make = kit::instantiate(script, Calls::default());
    (kit::manifest_in("0.2.0", "nn", &body), make)
}

fn late_end_marks() -> [std::path::PathBuf; 3] {
    let dir = std::env::temp_dir();
    let id = std::process::id();
    ["waiting", "go", "heard"].map(|mark| dir.join(format!("wisp-late-end-{mark}-{id}")))
}

fn unworded_jobs(name: &str, how: Unworded, plan: Vec<Step>) -> Harness {
    let Harness {
        agent,
        provider,
        tx,
        rx,
        store,
        _env,
    } = jobs(name, plan, false);
    let mut sources = crate::plugin::testing::turn_line_sources();
    sources.retain(|(manifest, _)| manifest != wisp_plugin_shell::MANIFEST);
    sources.push(unworded_hook(how));
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .now_or_never()
        .unwrap();
    Harness {
        agent: agent.with_hooks(started.hooks, started.host),
        provider,
        tx,
        rx,
        store,
        _env,
    }
}

fn processes_runs(records: &[LogRecord]) -> Vec<(Option<u64>, HookOutcome)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::HookRan {
                hook,
                turn,
                outcome,
                ..
            } if hook == LEAD_PROCESSES || hook == "nn/words" => Some((*turn, outcome.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn news_while_the_notice_is_off_stays_unheard_wakes_nothing_and_arrives_when_it_is_back_on() {
    news_while_off("notice-off", hook_switch).await;
}

#[tokio::test]
async fn news_while_the_shell_plugin_is_off_stays_unheard_wakes_nothing_and_arrives_when_it_is_on()
{
    news_while_off("notice-plugin-off", |request, on| {
        crate::plugin::testing::plugin_switch(
            request,
            "shell",
            wisp_protocol::settings::ENABLED_KEY,
            on,
        )
    })
    .await;
}

async fn news_while_off(name: &str, switch: fn(u64, bool) -> ClientCommand) {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        name,
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("off"),
            done("on"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    let work = h._env.work.clone();
    let off = h
        .drive(
            vec![switch(1, false)],
            |_: &AgentEvent| {
                a.open(&work);
                Vec::new()
            },
            never,
            SILENCE,
        )
        .await;
    assert!(process_ended(1)(&off), "{off:#?}");
    assert_eq!(starts(&off), 0, "{off:#?}");
    let unworded = h.run(vec![steer("while off")], 1).await;
    assert_eq!(reported(&unworded), [Vec::new()]);
    assert!(
        last_sent(&h).iter().all(|t| !t.contains("<processes>")),
        "{:?}",
        last_sent(&h)
    );
    assert!(heard(&records(&h.store)).is_empty());
    let back = h
        .drive(vec![switch(2, true)], nothing, never, SILENCE)
        .await;
    assert_eq!(starts(&back), 0, "{back:#?}");
    let worded = h.run(vec![steer("back on")], 1).await;
    assert_eq!(reported(&worded), [vec![exited(1, 2)]]);
    let line = &a.ended(1, 2);
    assert!(
        last_sent(&h).contains(&notice(&[line])),
        "{:?}",
        last_sent(&h)
    );
    assert_eq!(heard(&records(&h.store)), [(3, vec![1])]);
    assert_eq!(
        processes_runs(&records(&h.store)),
        [
            (Some(1), HookOutcome::None),
            (Some(3), HookOutcome::Applied)
        ]
    );
}

#[tokio::test]
async fn a_notice_back_on_with_nothing_unworded_wakes_for_the_news_it_missed() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "notice-back",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("woke"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    let work = h._env.work.clone();
    let off = h
        .drive(
            vec![hook_switch(1, false)],
            |_: &AgentEvent| {
                a.open(&work);
                Vec::new()
            },
            never,
            SILENCE,
        )
        .await;
    assert_eq!(starts(&off), 0, "{off:#?}");
    let back = h
        .drive(vec![hook_switch(2, true)], nothing, turns(1), PATIENT)
        .await;
    assert_eq!(wakes(&back), 1, "{back:#?}");
    assert_eq!(last_sent(&h).last().unwrap(), &woken(&[&a.ended(1, 2)]));
}

async fn an_unworded_wake_turn_stops_process_wakes_until_a_new_end(name: &str, how: Unworded) {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let b = ExitGate::new("b.go", 3);
    let mut h = unworded_jobs(
        name,
        how,
        vec![
            uses(&[background("a", &a.command), background("b", &b.command)]),
            done("started"),
            done("woke"),
            done("woke again"),
        ],
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let first = woke(&mut h).await;
    assert_eq!(reported(&first), [Vec::new()]);
    assert!(!process_ended(2)(&first), "{first:#?}");
    b.open(&h._env.work);
    let second = woke(&mut h).await;
    let ended_at = second
        .iter()
        .position(|e| matches!(e, AgentEvent::ProcessEnded { id: 2, .. }));
    let started_at = second
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { .. }));
    assert!(ended_at < started_at, "{second:#?}");
    assert!(ended_at.is_some(), "{second:#?}");
    assert_eq!(starts(&quiet(&mut h).await), 0);
    assert!(heard(&records(&h.store)).is_empty());
    assert!(h.requests().iter().all(|r| {
        r.iter()
            .all(|m| sent_texts(m).iter().all(|t| !t.contains("<processes>")))
    }),);
}

fn sent_texts(message: &Message) -> Vec<String> {
    pick!(&message.content, ContentBlock::Text { text } => text.clone())
}

#[tokio::test]
async fn a_rejected_wake_notice_stops_process_wakes_until_a_new_end() {
    an_unworded_wake_turn_stops_process_wakes_until_a_new_end(
        "unworded-wake-rejected",
        Unworded::Rejected,
    )
    .await;
}

#[tokio::test]
async fn a_failed_wake_notice_stops_process_wakes_until_a_new_end() {
    an_unworded_wake_turn_stops_process_wakes_until_a_new_end(
        "unworded-wake-failed",
        Unworded::Failed,
    )
    .await;
}

#[tokio::test]
async fn a_timed_out_wake_notice_stops_process_wakes_until_a_new_end() {
    an_unworded_wake_turn_stops_process_wakes_until_a_new_end(
        "unworded-wake-timed-out",
        Unworded::TimedOut,
    )
    .await;
}

async fn an_unworded_user_turn_stops_process_wakes(name: &str, how: Unworded) {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = unworded_jobs(
        name,
        how,
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("next"),
        ],
    );
    h.run(vec![steer("go")], 1).await;
    let work = h._env.work.clone();
    let off = h
        .drive(
            vec![hook_switch_of(1, "nn", "words", false)],
            |_: &AgentEvent| {
                a.open(&work);
                Vec::new()
            },
            process_ended(1),
            PATIENT,
        )
        .await;
    assert_eq!(starts(&off), 0, "{off:#?}");
    let next = h
        .drive(
            vec![hook_switch_of(2, "nn", "words", true), steer("next")],
            nothing,
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(wakes(&next), 0, "{next:#?}");
    assert_eq!(reported(&next), [Vec::new()]);
    assert_eq!(starts(&quiet(&mut h).await), 0);
    assert!(heard(&records(&h.store)).is_empty());
}

#[tokio::test]
async fn a_rejected_notice_on_a_user_turn_stops_process_wakes() {
    an_unworded_user_turn_stops_process_wakes("unworded-user-rejected", Unworded::Rejected).await;
}

#[tokio::test]
async fn a_failed_notice_on_a_user_turn_stops_process_wakes() {
    an_unworded_user_turn_stops_process_wakes("unworded-user-failed", Unworded::Failed).await;
}

#[tokio::test]
async fn a_timed_out_notice_on_a_user_turn_stops_process_wakes() {
    an_unworded_user_turn_stops_process_wakes("unworded-user-timed-out", Unworded::TimedOut).await;
}

#[tokio::test]
async fn a_retry_replays_its_notice_without_the_hook_and_an_end_while_its_offer_stood_wakes_after()
{
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let b = ExitGate::new("b.go", 3);
    let mut h = jobs(
        "retry-keeps-wakes",
        vec![
            uses(&[background("a", &a.command), background("b", &b.command)]),
            done("started"),
            server_error_step(),
            done("retried"),
            done("woke"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let failed = woke(&mut h).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    b.open(&h._env.work);
    let waiting = h
        .drive(Vec::new(), nothing, process_ended(2), PATIENT)
        .await;
    assert_eq!(starts(&waiting), 0, "{waiting:#?}");
    let retried = h
        .run(
            vec![ClientCommand::Retry {
                turn: 2,
                endpoint: None,
                request: RequestId(1),
            }],
            1,
        )
        .await;
    assert_eq!(reported(&retried), [vec![exited(1, 2)]]);
    let first = woken(&[&a.ended(1, 2)]);
    assert_eq!(last_sent(&h).last(), Some(&first));
    let woke_after = woke(&mut h).await;
    assert_eq!(reported(&woke_after), [vec![exited(2, 3)]]);
    assert_eq!(last_sent(&h).last().unwrap(), &woken(&[&b.ended(2, 3)]));
    assert_eq!(
        processes_runs(&records(&h.store)),
        [
            (Some(1), HookOutcome::None),
            (Some(2), HookOutcome::Applied),
            (Some(4), HookOutcome::Applied)
        ]
    );
}

#[tokio::test]
async fn a_resumed_retry_offer_keeps_its_notice_and_replays_it_without_the_hook() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut first = jobs(
        "resumed-notice",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            server_error_step(),
        ],
        false,
    );
    first.run(vec![steer("go")], 1).await;
    a.open(&first._env.work);
    let failed = woke(&mut first).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    let records = records(&first.store);
    let mut h = super::super::resumed::reopened("resumed-notice-2", vec![done("again")], &records);
    h.run(
        vec![ClientCommand::Retry {
            turn: 2,
            endpoint: None,
            request: RequestId(1),
        }],
        1,
    )
    .await;
    let block = woken(&[&a.ended(1, 2)]);
    assert_eq!(last_sent(&h).last(), Some(&block));
    assert_eq!(
        processes_runs(&test_guard::records(&h.store)),
        [
            (Some(1), HookOutcome::None),
            (Some(2), HookOutcome::Applied)
        ]
    );
}

#[tokio::test]
async fn a_notice_for_300_ended_jobs_is_delivered_whole() {
    let mut h = jobs("news-300", vec![done("read")], false);
    let mut records = Vec::new();
    for id in 1..=300u32 {
        let started: LogEvent = serde_json::from_value(json!({
            "type": "process_started",
            "turn": 1,
            "id": id,
            "call_id": format!("c{id}"),
            "reason": "started",
            "tty": false,
            "stdin": false,
        }))
        .unwrap();
        records.push(started);
        records.push(LogEvent::ProcessEnded {
            id,
            end: ProcessEnd::Exited {
                exit: ExitStatus::Code { code: 0 },
            },
            bytes: 0,
            lines: 0,
            duration_ms: 0,
        });
    }
    let records: Vec<LogRecord> = records
        .into_iter()
        .zip(0..)
        .map(|(event, seq)| LogRecord {
            schema_version: wisp_protocol::log::LOG_SCHEMA_VERSION,
            seq,
            at: "2026-10-25T12:00:00Z".into(),
            event,
        })
        .collect();
    let masked = vec![false; records.len()];
    h.agent.restore_processes((&records, &masked), &[], 0);
    h.run(vec![steer("go")], 1).await;
    let lines: Vec<String> = (1..=300)
        .map(|id| format!("Process {id} exited with code 0 before Wisp restarted."))
        .collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let block = notice(&lines);
    assert!(block.chars().count() > 10_000, "{}", block.len());
    assert!(last_sent(&h).contains(&block), "{:?}", last_sent(&h));
    let heard = heard(&test_guard::records(&h.store));
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].1.len(), 300);
}

#[tokio::test]
async fn an_end_during_an_unworded_opening_still_wakes_once_the_turn_ends() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let [waiting, go, heard] = late_end_marks();
    let late = format!(
        "touch '{}'; {}; exit 3",
        waiting.display(),
        crate::test_guard::wait_for(&format!("'{}'", go.display())),
    );
    let mut h = unworded_jobs(
        "unworded-late-end",
        Unworded::SlowlyRejected,
        vec![
            uses(&[background("a", &a.command), background("b", &late)]),
            done("started"),
            done("next"),
            done("woke"),
        ],
    );
    h.run(vec![steer("go")], 1).await;
    let work = h._env.work.clone();
    let off = h
        .drive(
            vec![hook_switch_of(1, "nn", "words", false)],
            |_: &AgentEvent| {
                a.open(&work);
                Vec::new()
            },
            process_ended(1),
            PATIENT,
        )
        .await;
    assert_eq!(starts(&off), 0, "{off:#?}");
    let next = h
        .drive(
            vec![hook_switch_of(2, "nn", "words", true), steer("next")],
            |e: &AgentEvent| {
                if matches!(e, AgentEvent::ProcessEnded { id: 2, .. }) {
                    std::fs::write(&heard, "").unwrap();
                }
                Vec::new()
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(process_ended(2)(&next), "{next:#?}");
    assert_eq!(wakes(&next), 0, "{next:#?}");
    let woke = h.drive(Vec::new(), nothing, turns(1), PATIENT).await;
    assert_eq!(wakes(&woke), 1, "{woke:#?}");
    for mark in [waiting, go, heard] {
        drop(std::fs::remove_file(mark));
    }
}
