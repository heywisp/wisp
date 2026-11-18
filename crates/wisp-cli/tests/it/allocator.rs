#![cfg(all(target_os = "linux", target_env = "gnu"))]

use crate::support::WISP;
use crate::support::home::{Home, command_in, err, run};

#[test]
fn the_binary_allocates_through_jemalloc() {
    let home = Home::new("allocator");
    let mut command = command_in(&home.0, WISP);
    command
        .arg("--version")
        .env("_RJEM_MALLOC_CONF", "stats_print:true");
    let output = run(&mut command, "");
    let stderr = err(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("___ Begin jemalloc statistics ___"),
        "{stderr}"
    );
    assert!(stderr.contains("opt.background_thread: true"), "{stderr}");
}
