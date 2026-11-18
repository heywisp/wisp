#[path = "support/live.rs"]
mod live;
#[path = "../../../wisp-tui/tests/support/pick.rs"]
mod pick;
#[cfg(unix)]
#[path = "endpoint_commands/runner.rs"]
mod runner;
#[cfg(unix)]
#[path = "../../../wisp-tui/tests/support/scratch.rs"]
mod scratch;
#[cfg(unix)]
mod support;

mod allocator;
mod config_commands;
mod doctor_images;
mod endpoint_commands;
mod endpoint_edit_import;
mod endpoint_prompts;
mod endpoints;
mod first_run;
mod gemini;
mod golden_requests;
mod hosted;
mod keychain_first;
mod live_ask;
mod live_images;
mod live_keep_running;
mod live_retry;
mod live_show_diff;
mod live_steering;
mod live_tool_changes;
mod live_tools;
mod live_view_file;
mod live_web_search;
mod live_working_line;
mod openai;
mod paste;
mod piped;
mod pty;
mod testkit_gate;
mod tools;
mod variable_guard;
mod wasm;
