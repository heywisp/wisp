use std::hint::black_box;
use std::process::{Command, ExitCode};

use super::*;

const FRAME_BYTES: usize = 64 * 1024;
const FRAMES: usize = 192;
const CHILD_MARKER: &str = "WISP_STACK_CHILD";
const SENTINEL: &str = "every thread had room for twelve MiB of frames";
const DEFAULT_TEST_STACK: &str = "2097152";

fn descend(depth: usize) -> usize {
    let mut frame = [u8::try_from(depth % 256).unwrap_or_default(); FRAME_BYTES];
    black_box(&mut frame);
    let below = if depth == 0 { 0 } else { descend(depth - 1) };
    black_box(below + usize::from(frame[depth % FRAME_BYTES]))
}

fn deep() -> usize {
    descend(black_box(FRAMES - 1))
}

fn expected() -> usize {
    (0..FRAMES).map(|depth| depth % 256).sum()
}

fn deep_everywhere() -> ExitCode {
    on_runtime(true, || async {
        let root = deep();
        let worker = tokio::spawn(async { deep() }).await;
        let blocking = tokio::task::spawn_blocking(deep).await;
        let all = [Ok(root), worker, blocking];
        if all
            .iter()
            .all(|depth| depth.as_ref().is_ok_and(|d| *d == expected()))
        {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    })
}

#[test]
fn the_root_workers_and_blocking_threads_get_the_thread_stack() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        assert_eq!(deep_everywhere(), ExitCode::SUCCESS);
        println!("{SENTINEL}");
        return;
    }
    let module = module_path!().split_once("::").unwrap().1;
    let name = format!("{module}::the_root_workers_and_blocking_threads_get_the_thread_stack");
    let child = Command::new(std::env::current_exe().unwrap())
        .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, "1")
        .env("RUST_MIN_STACK", DEFAULT_TEST_STACK)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(
        child.status.success() && stdout.contains(SENTINEL),
        "child at a 2 MiB default stack: {}\n{stdout}\n{stderr}",
        child.status
    );
}

#[test]
fn a_panicking_root_exits_like_an_unwinding_main() {
    let code = on_runtime(true, || async { panic!("the root fails") });
    assert_eq!(code, ExitCode::from(101));
}
