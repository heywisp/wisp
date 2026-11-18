use std::fs::{self, File};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use wisp_core::resume::{Found, Wanted, find};
use wisp_core::session::{JsonlStore, LogLock, new_session_id};

use super::shared::{OPUS, at, lines, started_in, usage};

const A: &str = "0001aaaa-0000-7000-8000-000000000000";
const B: &str = "0002bbbb-0000-7000-8000-000000000000";
const C: &str = "0003cccc-0000-7000-8000-000000000000";

fn full_turn(n: u64) -> Vec<Value> {
    let said = |role: &str| {
        json!({
            "type": "message_committed",
            "turn": n,
            "message": {"role": role, "content": [{"type": "text", "text": format!("{role} {n}")}]},
            "model": (role == "assistant").then_some(OPUS),
        })
    };
    vec![
        json!({"type": "turn_started", "turn": n, "text": format!("message {n}")}),
        said("user"),
        said("assistant"),
        json!({
            "type": "step_ended",
            "turn": n,
            "step": 1,
            "usage": usage(1_000 * n, 40),
            "cost_usd": 0.01,
            "lower_bound": false,
            "prompt_tokens": 1_000 * n,
            "response_id": format!("msg_{n}_1"),
            "used": 1_000 * n,
            "accepted": true,
        }),
        json!({
            "type": "turn_ended",
            "turn": n,
            "outcome": {
                "status": "completed",
                "stop_reason": "end_turn",
                "model": OPUS,
                "usage": usage(1, 1),
                "cost": {"turn_usd": 0.1, "session_usd": 0.1},
                "committed": true,
            },
            "stats": {"started_at": at(0), "duration_ms": 1, "retries": 0},
        }),
    ]
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wisp-resume-{name}-{}-{}",
        std::process::id(),
        new_session_id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn session_here(
    store: &JsonlStore,
    id: &str,
    cwd: &str,
    tools: Option<&str>,
    turns: u64,
    age_minutes: u64,
) {
    let names: &[&str] = if tools.is_some() { &["bash"] } else { &[] };
    let mut events = vec![started_in(id, cwd, names, tools)];
    for n in 1..=turns {
        events.extend(full_turn(n));
    }
    let path = store.path(id);
    fs::write(&path, lines(&events)).unwrap();
    let modified = SystemTime::now() - Duration::from_secs(age_minutes * 60);
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

#[test]
fn the_latest_session_here_is_resumed() {
    let dir = temp_dir("latest");
    let store = JsonlStore::open(&dir).unwrap();
    session_here(&store, A, "~/wisp", None, 2, 30);
    session_here(&store, B, "~/elsewhere", None, 1, 1);
    session_here(&store, C, "~/wisp", None, 0, 2);
    let latest = |here: &str| find(&store, Wanted::Latest { here });
    assert_eq!(
        latest("~/wisp").unwrap(),
        Found {
            id: A.into(),
            skipped: Vec::new(),
            unreadable: Vec::new(),
        },
        "the newer session with no turns is passed over"
    );
    assert_eq!(latest("~/elsewhere").unwrap().id, B);
    assert_eq!(
        latest("~/new").unwrap_err(),
        "no session to resume in ~/new"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_session_open_elsewhere_is_skipped_and_named() {
    let dir = temp_dir("skip-open");
    let store = JsonlStore::open(&dir).unwrap();
    session_here(&store, A, "~/wisp", None, 1, 30);
    session_here(&store, B, "~/wisp", None, 1, 1);
    let mut other = JsonlStore::open(&dir).unwrap();
    assert_eq!(other.open_session(B).unwrap(), LogLock::Held);
    let found = find(&store, Wanted::Latest { here: "~/wisp" }).unwrap();
    assert_eq!(found.id, A);
    assert_eq!(found.skipped, ["0002bbbb-0000"]);

    let mut held = JsonlStore::open(&dir).unwrap();
    assert_eq!(held.open_session(A).unwrap(), LogLock::Held);
    assert_eq!(
        find(&store, Wanted::Latest { here: "~/wisp" }).unwrap_err(),
        "no session to resume in ~/wisp: sessions 0002bbbb-0000, 0001aaaa-0000 are open in another Wisp"
    );
    drop((other, held));
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_log_that_cannot_be_opened_is_skipped_with_its_own_reason() {
    let dir = temp_dir("skip-link");
    let elsewhere = temp_dir("skip-link-target");
    let store = JsonlStore::open(&dir).unwrap();
    let target = JsonlStore::open(&elsewhere).unwrap();
    session_here(&store, A, "~/wisp", None, 1, 30);
    session_here(&target, B, "~/wisp", None, 1, 1);
    std::os::unix::fs::symlink(target.path(B), store.path(B)).unwrap();
    let found = find(&store, Wanted::Latest { here: "~/wisp" }).unwrap();
    assert_eq!(found.id, A);
    assert!(found.skipped.is_empty(), "{found:?}");
    let [(id, why)] = found.unreadable.as_slice() else {
        panic!("{found:?}")
    };
    assert_eq!(id, "0002bbbb-0000");
    assert!(!why.contains("another Wisp"), "{why}");

    fs::remove_file(store.path(A)).unwrap();
    let err = find(&store, Wanted::Latest { here: "~/wisp" }).unwrap_err();
    assert_eq!(
        err,
        format!("no session to resume in ~/wisp: session 0002bbbb-0000 can't be opened ({why})")
    );
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&elsewhere).unwrap();
}

#[test]
fn the_newest_session_here_resumes_whatever_tools_it_ran_with() {
    let dir = temp_dir("other-tools");
    let store = JsonlStore::open(&dir).unwrap();
    session_here(&store, A, "~/wisp", None, 1, 200);
    session_here(&store, B, "~/wisp", Some("abc"), 2, 125);
    assert_eq!(
        find(&store, Wanted::Latest { here: "~/wisp" }).unwrap(),
        Found {
            id: B.into(),
            skipped: Vec::new(),
            unreadable: Vec::new(),
        },
        "an older session with other tools never wins"
    );
    fs::remove_dir_all(&dir).unwrap();
}
