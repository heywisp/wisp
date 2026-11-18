#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a source scan fails the test on any read error"
)]

mod app_names;
#[path = "../../../../tests/support/purity.rs"]
mod purity;
mod setting_names;
mod start_gate;
mod tokens;
mod tool_paths;
mod variable_guard;
mod variant_lists;
