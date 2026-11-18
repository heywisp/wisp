#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed step fails the test"
)]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use wisp_plugin_api::Plugin;
use wisp_plugin_testkit::hooks::user_input;
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use support::{ROOT_ID, Rig, board_host, build, place, sha256};

async fn timed<F: Future>(f: F) -> (Duration, F::Output) {
    let started = Instant::now();
    let output = f.await;
    (started.elapsed(), output)
}

async fn echo(plugin: &dyn Plugin, input: &str) -> Duration {
    timed(async {
        let prepared = plugin.prepare("echo", input).await.unwrap();
        let call = FakeCall::default();
        plugin.run(prepared, &call).await;
        assert_eq!(call.written().len(), 1 << 20);
    })
    .await
    .0
}

#[test]
#[ignore = "the measurement gate, run on its own"]
fn gate() {
    let runtimes = [
        ("multi-thread", tokio::runtime::Builder::new_multi_thread()),
        (
            "current-thread",
            tokio::runtime::Builder::new_current_thread(),
        ),
    ];
    for (name, mut builder) in runtimes {
        let runtime = builder.enable_all().build().unwrap();
        runtime.block_on(async { tokio::spawn(measure(name)).await.unwrap() });
    }
}

struct Native(String);

impl board::Host for Native {
    fn me(&self) -> Option<board::Me> {
        Some(board::Me {
            path: "/".to_owned(),
            session: ROOT_ID.to_owned(),
            root: ROOT_ID.to_owned(),
        })
    }

    fn agents(&self) -> Vec<board::Agent> {
        Vec::new()
    }

    fn now(&self) -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    fn offset(&self, _: jiff::Timestamp) -> i32 {
        0
    }

    fn data_dir(&self) -> Result<String, String> {
        Ok(self.0.clone())
    }
}

async fn wasm_board(plugin: &dyn Plugin, tool: &str, input: &str) -> Duration {
    timed(async {
        let prepared = plugin.prepare(tool, input).await.unwrap();
        let call = FakeCall::default();
        let run = plugin.run(prepared, &call).await;
        assert!(run.result.is_some() || !call.written().is_empty());
    })
    .await
    .0
}

async fn native_board(host: &Native, tool: &str, input: &str) -> Duration {
    timed(async {
        let call = board::prepare(tool, input).unwrap();
        let reply = board::run(host, &call.request);
        assert!(matches!(
            reply,
            board::Reply::View(_) | board::Reply::Line(_)
        ));
    })
    .await
    .0
}

async fn board_rows(runtime: &str, rig: &Rig) {
    let plugin = (rig.board().make)(Arc::new(board_host(rig)));
    plugin.describe(&build()).await;
    let native = Native(rig.scratch.root.join("native").display().to_string());
    let cards: Vec<serde_json::Value> = (0..200)
        .map(|i| serde_json::json!({"id": format!("c{i}"), "title": format!("Card number {i}"), "note": "A first line of steps\nand a second"}))
        .collect();
    let seed = serde_json::json!({"board": "gate", "cards": cards}).to_string();
    wasm_board(&*plugin, "board_write", &seed).await;
    native_board(&native, "board_write", &seed).await;
    let (mut wasm, mut bundled) = ([Duration::ZERO; 2], [Duration::ZERO; 2]);
    for i in 0..100 {
        let write = format!(
            r#"{{"board":"gate","cards":[{{"id":"c{}","log":"step {i} done"}}]}}"#,
            i * 2
        );
        let read = r#"{"board":"gate"}"#;
        wasm[0] += wasm_board(&*plugin, "board_write", &write).await;
        bundled[0] += native_board(&native, "board_write", &write).await;
        wasm[1] += wasm_board(&*plugin, "board_read", read).await;
        bundled[1] += native_board(&native, "board_read", read).await;
    }
    for (tool, wasm, native) in [
        ("board_write", wasm[0], bundled[0]),
        ("board_read", wasm[1], bundled[1]),
    ] {
        let ratio = wasm.as_secs_f64() / native.as_secs_f64();
        println!(
            "{runtime}: {tool} on 200 cards over 100 calls: wasm {wasm:?}, native {native:?}, ratio {ratio:.2}"
        );
    }
}

async fn measure(runtime: &str) {
    let rig = Rig::new("gate");
    let found = rig.fixture();
    let (instantiate, _) = timed(async {
        let plugin = (found.make)(Arc::new(FakeHost::default()));
        plugin.describe(&build()).await;
        plugin
    })
    .await;
    let plugin = (found.make)(Arc::new(FakeHost::default()));
    plugin.describe(&build()).await;

    let mut crossings = Vec::with_capacity(1000);
    let mut hooks = Vec::with_capacity(1000);
    let typed = user_input("", 0);
    for _ in 0..1000 {
        crossings.push(timed(plugin.time_limit_ms("echo", "{}")).await.0);
        hooks.push(timed(plugin.on_hook("fires", typed.clone())).await.0);
    }
    let export = p99(crossings);
    let hook = p99(hooks);

    let input = format!(r#"{{"text":"{}"}}"#, "x".repeat(1 << 20));
    let bundled = (wisp_plugin_testkit::bundled().instantiate)(Arc::new(FakeHost::default()));
    let (mut wasm, mut native) = (Duration::ZERO, Duration::ZERO);
    for _ in 0..100 {
        wasm += echo(&*plugin, &input).await;
        native += echo(&*bundled, &input).await;
    }
    let ratio = wasm.as_secs_f64() / native.as_secs_f64();

    println!("{runtime}: instantiate and describe: {instantiate:?}");
    println!("{runtime}: no-op export p99 over 1000 calls: {export:?}");
    println!("{runtime}: no-op hook p99 over 1000 calls: {hook:?}");
    println!(
        "{runtime}: 1 MiB echo over 100 calls: wasm {wasm:?}, bundled {native:?}, ratio {ratio:.2}"
    );
    board_rows(runtime, &rig).await;
    let (cold, cached) = board_reads(&rig);
    println!("{runtime}: board folder read, cold {cold:?}, cached {cached:?}");
    assert!(cached <= Duration::from_millis(100));
    assert!(instantiate <= Duration::from_millis(50));
    assert!(export <= Duration::from_millis(1));
    assert!(hook <= Duration::from_millis(1));
}

fn board_reads(rig: &Rig) -> (Duration, Duration) {
    let mut wasm = std::fs::read(wisp_wasm_guests::BOARD).unwrap();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_le_bytes();
    wasm.extend([0, 20, 3, b'r', b'u', b'n']);
    wasm.extend(unique);
    let manifest = std::fs::read_to_string(wisp_wasm_guests::BOARD_MANIFEST).unwrap();
    place(
        &rig.dirs.data,
        "board-read",
        &manifest,
        &wasm,
        &sha256(&wasm),
    );
    let folder = rig.dirs.data.join("plugins/board-read");
    let read = || {
        let started = Instant::now();
        let found = wisp_wasm_host::read(&rig.dirs, &folder).map_err(|(_, e)| e);
        assert_eq!(found.unwrap().id, "board");
        started.elapsed()
    };
    (read(), read())
}

fn p99(mut durations: Vec<Duration>) -> Duration {
    durations.sort();
    durations[durations.len() * 99 / 100 - 1]
}
