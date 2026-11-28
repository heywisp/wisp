pub mod agents;
pub mod context;
pub mod endpoints;
#[cfg(feature = "wasm")]
pub mod folder;
pub mod instructions;
pub mod keychain;
pub mod models;
pub mod paths;
pub mod presence;
pub mod resume;
pub mod run;
pub mod scrub;
pub mod settings;
pub mod start;
pub mod tools;
pub mod watch;

#[cfg(test)]
#[path = "../../wisp-tui/tests/support/scratch.rs"]
mod scratch;

#[cfg(test)]
fn scratch_dir(name: &str) -> scratch::ScratchDir {
    scratch::ScratchDir::new("wisp-start-", name)
}
