#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "the harness helpers fail the test on any file, socket, pty or process error"
)]

pub(crate) mod home;
pub(crate) mod prefix;
pub(crate) mod pty;
#[path = "../../../../wisp-tui/tests/support/screen.rs"]
pub(crate) mod screen;
pub(crate) mod server;

pub(crate) const WISP: &str = env!("CARGO_BIN_EXE_wisp");

pub(crate) mod hosted;
