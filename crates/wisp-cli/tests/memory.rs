#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a measurement harness that fails loudly and prints what it measured"
)]

#[path = "memory/client.rs"]
mod client;
#[path = "../../wisp-render/tests/support/code.rs"]
mod code;
#[path = "memory/owners.rs"]
mod owners;
#[path = "memory/rss.rs"]
mod rss;
#[path = "../../wisp-tui/tests/support/scratch.rs"]
mod scratch;
#[path = "memory/script.rs"]
mod script;
#[path = "memory/server.rs"]
mod server;
#[path = "memory/session.rs"]
mod session;

use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use session::{Rig, on_stack};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const RATCHET: f64 = 1.15;
const MAX_R: f64 = 1.09;
const MAX_K: f64 = 16.34;
const MAX_H: f64 = 2.56;
const MAX_Q: f64 = 3.44;
const MAX_LEAK: u64 = 1024 * 1024;
const REMEASURE: &str =
    "heavy cargo nextest run -p wisp-cli --test memory -E 'test(/^memory_budget/)' --no-capture";

#[derive(Clone, Copy, Debug, Default)]
struct Mark {
    live: u64,
    total: u64,
    peak: u64,
    log: u64,
    sent: u64,
    largest: u64,
}

fn mark(rig: &Rig, log: Option<&Path>) -> Mark {
    let stats = dhat::HeapStats::get();
    Mark {
        live: stats.curr_bytes as u64,
        total: stats.total_bytes,
        peak: stats.max_bytes as u64,
        log: log.map_or(0, |p| std::fs::metadata(p).unwrap().len()),
        sent: rig.seen().bytes(),
        largest: rig.seen().largest(),
    }
}

fn media_bytes(value: &Value) -> u64 {
    match value {
        Value::Object(map) => {
            let own = match (
                map.get("sha256"),
                map.get("media_type"),
                map.get("bytes").and_then(Value::as_u64),
            ) {
                (Some(Value::String(_)), Some(Value::String(_)), Some(bytes)) => {
                    bytes.div_ceil(3) * 4
                }
                _ => 0,
            };
            own + map.values().map(media_bytes).sum::<u64>()
        }
        Value::Array(items) => items.iter().map(media_bytes).sum(),
        _ => 0,
    }
}

fn content(log: &Path, from: u64, to: u64) -> u64 {
    let bytes = std::fs::read(log).unwrap();
    let start = usize::try_from(from).unwrap();
    let end = usize::try_from(to).unwrap().min(bytes.len());
    let media: u64 = bytes[start..end]
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| media_bytes(&serde_json::from_slice(line).unwrap()))
        .sum();
    (to - from) + media
}

fn ratio(top: u64, bottom: u64) -> f64 {
    top as f64 / bottom.max(1) as f64
}

fn signed(a: u64, b: u64) -> i64 {
    i64::try_from(a).unwrap() - i64::try_from(b).unwrap()
}

struct Budget {
    lines: Vec<String>,
    failed: Vec<String>,
}

impl Budget {
    fn new() -> Budget {
        Budget {
            lines: Vec::new(),
            failed: Vec::new(),
        }
    }

    fn note(&mut self, line: String) {
        self.lines.push(line);
    }

    fn at_most(&mut self, name: &str, value: f64, limit: f64) {
        self.lines.push(format!(
            "{name} = {value:.4} (budget {limit:.4}; this run plus 15% would be {:.4})",
            value * RATCHET
        ));
        if value > limit {
            self.failed.push(format!("{name} {value:.4} > {limit:.4}"));
        }
    }

    fn finish(self, what: &str) {
        eprintln!("MEMORY {what}\n{}", self.lines.join("\n"));
        assert!(
            self.failed.is_empty(),
            "{what} is over budget: {}; every figure is printed above; re-measure with {REMEASURE} (docs/testing.md, \"Memory\")",
            self.failed.join(", ")
        );
    }
}

fn b_figures(
    budget: &mut Budget,
    rig: &Rig,
    log: &Path,
    start: Mark,
    at: &[Mark; 4],
    client: Mark,
    end: Mark,
) {
    let [t2, t9, t11, t16] = *at;
    let early = content(log, t2.log, t9.log);
    let late = content(log, t11.log, t16.log);
    let r_early = ratio(t9.live.saturating_sub(t2.live), early);
    let r_late = ratio(t16.live.saturating_sub(t11.live), late);
    let k = ratio(t16.total - t2.total, t16.sent - t2.sent);
    let h = ratio(t16.peak.saturating_sub(t16.live), t16.largest);
    for (name, m) in [
        ("START", start),
        ("T2", t2),
        ("T9", t9),
        ("T11", t11),
        ("T16", t16),
        ("CLIENT", client),
        ("END", end),
    ] {
        budget.note(format!(
            "{name}: live {} total {} peak {} log {} sent {} largest {}",
            m.live, m.total, m.peak, m.log, m.sent, m.largest
        ));
    }
    budget.note(format!("content turns 3-9 {early}, turns 12-16 {late}"));
    budget.note(format!(
        "R turns 3-9 = {r_early:.4}, R turns 12-16 = {r_late:.4}"
    ));
    budget.at_most("R", r_early.max(r_late), MAX_R);
    budget.at_most("K", k, MAX_K);
    budget.at_most("H", h, MAX_H);
    budget.note(format!(
        "client share = {} bytes, kernel share = {} bytes",
        signed(t16.live, client.live),
        signed(client.live, end.live)
    ));
    let leak = end.live.saturating_sub(start.live);
    budget.note(format!("L = {leak} bytes (budget {MAX_LEAK})"));
    if leak > MAX_LEAK {
        budget.failed.push(format!("L {leak} > {MAX_LEAK}"));
    }
    let seen = rig.seen();
    budget.note(format!(
        "requests {} (native asks {}, summaries {}, unscripted {})",
        seen.requests.load(std::sync::atomic::Ordering::SeqCst),
        seen.native.load(std::sync::atomic::Ordering::SeqCst),
        seen.summaries.load(std::sync::atomic::Ordering::SeqCst),
        seen.fallbacks.load(std::sync::atomic::Ordering::SeqCst)
    ));
}

const B_MARKS: [usize; 4] = [2, 9, 11, 16];

#[test]
fn memory_budget() {
    let started = Instant::now();
    let rig = Rig::new("b", script::b(16));
    let mut budget = Budget::new();
    on_stack(|| async {
        let profiler = dhat::Profiler::builder().testing().build();
        let start = mark(&rig, None);
        let mut live = rig.fresh(true).await;
        let mut at = [Mark::default(); 4];
        for n in 0..16 {
            live.turn(n, &rig, &[]).await;
            if let Some(i) = B_MARKS.iter().position(|t| *t == n + 1) {
                at[i] = mark(&rig, Some(&live.log));
            }
        }
        let compactions = live.compactions;
        let log = live.log.clone();
        live.drop_client();
        let client = mark(&rig, None);
        live.close().await;
        let end = mark(&rig, None);
        drop(profiler);
        b_figures(&mut budget, &rig, &log, start, &at, client, end);
        budget.note(format!("compactions {compactions}"));
        if compactions != 1 {
            budget
                .failed
                .push(format!("{compactions} compactions, not 1"));
        }
    });
    budget.note(format!("wall time {:?}", started.elapsed()));
    budget.finish("B");
}

#[test]
fn memory_budget_resume() {
    let started = Instant::now();
    let rig = Rig::new("b-resume", script::b(17));
    let mut budget = Budget::new();
    on_stack(|| async {
        let mut live = rig.fresh(false).await;
        for n in 0..16 {
            live.turn(n, &rig, &[]).await;
        }
        let log = live.log.clone();
        live.close().await;
        let whole = content(&log, 0, std::fs::metadata(&log).unwrap().len());
        let profiler = dhat::Profiler::builder().testing().build();
        let start = mark(&rig, None);
        let mut live = rig.resume(&log, true).await;
        let resumed = mark(&rig, None);
        live.turn(16, &rig, &[]).await;
        let after = mark(&rig, Some(&log));
        live.close().await;
        let end = mark(&rig, None);
        drop(profiler);
        let q = ratio(resumed.peak - start.live, whole);
        budget.note(format!(
            "whole log content {whole}; START live {}; resumed live {} peak {}; after turn 17 live {} peak {}; END live {}",
            start.live, resumed.live, resumed.peak, after.live, after.peak, end.live
        ));
        budget.at_most("Q", q, MAX_Q);
    });
    budget.note(format!("wall time {:?}", started.elapsed()));
    budget.finish("B-resume");
}

fn profile(rig: &Rig, json: &Path, what: &str) {
    let text = std::fs::read_to_string(json).unwrap();
    let owners = owners::group(&text);
    let report = owners.report(what);
    let root = rig.scratch.root.display().to_string();
    assert!(!report.contains(&root), "a table holds the scratch path");
    eprintln!("{report}");
}

async fn play(live: &mut session::Live, rig: &Rig) {
    for n in 0..rig.turns.len() {
        let waits = if n + 1 == script::P_LEG {
            rig.children.clone()
        } else {
            Vec::new()
        };
        live.turn(n, rig, &waits).await;
    }
}

#[test]
#[ignore = "a heap profile, minutes long: heavy cargo nextest run -p wisp-cli --test memory --run-ignored only -E 'test(=memory_profile)' --no-capture"]
fn memory_profile() {
    let started = Instant::now();
    let rig = Rig::new("p", script::p(script::P_TURNS, true));
    let json = rig.scratch.root.join("dhat-heap.json");
    on_stack(|| async {
        let profiler = dhat::Profiler::builder()
            .file_name(&json)
            .trim_backtraces(None)
            .build();
        let mut live = rig.fresh(true).await;
        play(&mut live, &rig).await;
        let stats = dhat::HeapStats::get();
        eprintln!(
            "P: {} turns, {} compactions, live {} peak {} total {}",
            rig.turns.len(),
            live.compactions,
            stats.curr_bytes,
            stats.max_bytes,
            stats.total_bytes
        );
        drop(profiler);
        live.close().await;
    });
    profile(&rig, &json, "P");
    eprintln!("P wall time {:?}", started.elapsed());
}

#[test]
#[ignore = "a heap profile, minutes long: heavy cargo nextest run -p wisp-cli --test memory --run-ignored only -E 'test(=memory_profile_resume)' --no-capture"]
fn memory_profile_resume() {
    let started = Instant::now();
    let rig = Rig::new("p-resume", script::p(script::P_TURNS, true));
    let json = rig.scratch.root.join("dhat-heap.json");
    on_stack(|| async {
        let mut live = rig.fresh(false).await;
        play(&mut live, &rig).await;
        let log = live.log.clone();
        live.close().await;
        let profiler = dhat::Profiler::builder()
            .file_name(&json)
            .trim_backtraces(None)
            .build();
        let live = rig.resume(&log, true).await;
        let stats = dhat::HeapStats::get();
        eprintln!(
            "P-resume: live {} peak {} total {}",
            stats.curr_bytes, stats.max_bytes, stats.total_bytes
        );
        drop(profiler);
        live.close().await;
    });
    profile(&rig, &json, "P-resume");
    eprintln!("P-resume wall time {:?}", started.elapsed());
}

#[test]
fn owners_credit_the_first_wisp_frame_and_never_show_a_path() {
    let json = serde_json::json!({
        "ftbl": [
            "[root]",
            "0x10: alloc::alloc::alloc (/rustc/abc/library/alloc/src/alloc.rs:1:1)",
            "0x20: <wisp_protocol::Message as core::clone::Clone>::clone (crates/wisp-protocol/src/lib.rs:2:3)",
            "0x30: wisp_core::agent::Agent::request_view::{{closure}} (/home/someone/src/agent.rs:4:5)",
            "0x40: wisp_core::agent::Agent::serve (crates/wisp-core/src/agent.rs:6:7)",
            "0x50: tokio::runtime::task::poll (src/task.rs:8:9)",
            "0x60: wisp_render::transcript::Transcript::apply (~/x.rs:1:1)",
            "0x70: <wisp_core::agent::Agent>::serve_turn::{closure#0}::{closure#1} (crates/wisp-core/src/agent.rs:9:9)",
            "0x80: <wisp_contain::Contained<F> as core::future::Future>::poll (crates/wisp-contain/src/lib.rs:1:1)",
            "0x90: wisp_core::agent::epoch::digest::<(&core::option::Option<alloc::string::String>, &dyn Fn() -> u8)> (src/epoch.rs:1:1)",
            "0xa0: <wisp_protocol::Message as core::clone::Clone>::clone (src/lib.rs:1:1)",
            "0xb0: {async_fn#0} (crates/wisp-core/src/agent.rs:1:1)",
            "0xc0: poll<alloc::boxed::Box<wisp_core::agent::{impl#7}::run_steps_unboxed::{async_fn_env#0}, alloc::alloc::Global>> (src/future.rs:1:1)"
        ],
        "pps": [
            {"tb": 100, "gb": 40, "eb": 10, "fs": [1, 2, 3, 4]},
            {"tb": 50, "gb": 5, "eb": 0, "fs": [1, 3]},
            {"tb": 7, "gb": 7, "eb": 7, "fs": [1, 5, 0]},
            {"tb": 30, "gb": 30, "eb": 30, "fs": [6, 4]},
            {"tb": 3, "gb": 2, "eb": 1, "fs": [1, 8, 7, 4]},
            {"tb": 9, "gb": 0, "eb": 0, "fs": [9, 4]},
            {"tb": 11, "gb": 0, "eb": 0, "fs": [1, 10, 11, 12, 4]}
        ]
    })
    .to_string();
    let owners = owners::group(&json);
    let view = owners.rows["wisp_core::agent::Agent::request_view"];
    assert_eq!((view.total, view.peak, view.end), (150, 45, 10));
    assert_eq!(owners.rows[owners::NO_OWNER].total, 7);
    assert_eq!(owners.rows["wisp_core::agent::Agent::serve_turn"].total, 3);
    assert_eq!(owners.rows["wisp_core::agent::epoch::digest"].total, 9);
    assert_eq!(owners.rows["wisp_core::agent::run_steps_unboxed"].total, 11);
    assert_eq!(
        owners.rows["wisp_render::transcript::Transcript::apply"].end,
        30
    );
    assert!(
        !owners
            .rows
            .keys()
            .any(|l| l.starts_with('<') && l != owners::NO_OWNER)
    );
    assert_eq!(owners.crates()["wisp_core"].total, 173);
    let report = owners.report("fixture");
    assert!(report.contains(owners::NO_OWNER));
    for label in owners.rows.keys() {
        assert!(
            !label.starts_with('/') && !label.starts_with('~'),
            "{label}"
        );
        assert!(
            !label.contains("/home/") && !label.contains(".rs"),
            "{label}"
        );
    }
    let quiet = owners::group(
        &serde_json::json!({"ftbl": ["[root]"], "pps": [{"tb": 1, "fs": [0]}]}).to_string(),
    );
    assert!(quiet.report("empty").contains(owners::NO_OWNER));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "the real binary's RSS, minutes long: heavy cargo nextest run -p wisp-cli --test memory --run-ignored only -E 'test(=memory_rss)' --no-capture"]
fn memory_rss() {
    let started = Instant::now();
    let rig = Rig::new("s", script::p(S_TURNS + S_RESUMED, false));
    let mut anon = Vec::new();
    for variant in rss::VARIANTS {
        let begun = Instant::now();
        let samples = rss::variant(&rig, variant, S_TURNS);
        eprintln!(
            "{}wall time {:?}\n",
            rss::table(variant.0, &samples),
            begun.elapsed()
        );
        anon.push((
            rss::anon_at(&samples, &format!("turn {S_TURNS}")),
            rss::anon_at(
                &samples,
                &format!("after the resume, turn {}", S_TURNS + S_RESUMED),
            ),
        ));
    }
    let share = |of: usize, at: usize| {
        let pick = |pair: (u64, u64)| if at == 0 { pair.0 } else { pair.1 };
        1.0 - ratio(pick(anon[of]), pick(anon[0]))
    };
    eprintln!(
        "arena share at turn {S_TURNS} {:.3}, after the resume {:.3}\nlarge-buffer share at turn {S_TURNS} {:.3}, after the resume {:.3}\nS wall time {:?}",
        share(1, 0),
        share(1, 1),
        share(2, 0),
        share(2, 1),
        started.elapsed()
    );
}

const S_TURNS: usize = 120;
const S_RESUMED: usize = 10;
