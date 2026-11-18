#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any file, socket or process error"
)]

#[path = "../../web/tests/support/client.rs"]
mod client;
#[allow(dead_code, reason = "learn uses part of wisp-cli's test helpers")]
#[path = "../../../../crates/wisp-cli/tests/it/support/home.rs"]
mod home;
#[path = "../../../../crates/wisp-cli/tests/it/support/prefix.rs"]
mod prefix;
#[path = "../../../../crates/wisp-tui/tests/support/scratch.rs"]
mod scratch;
#[allow(dead_code, reason = "learn uses part of wisp-cli's test helpers")]
#[path = "../../../../crates/wisp-cli/tests/it/support/server.rs"]
mod server;

mod flow_prefix;
mod web;

const LEARN: &str = env!("CARGO_BIN_EXE_learn");
