pub mod agents;
pub mod chat;
pub mod code;
pub mod compaction;
pub mod config;
pub mod files;
pub mod index;
pub mod jobs;
pub mod manage;
pub mod mcp;
pub mod output;
pub mod paths;
pub mod question;
pub mod search;
pub mod switches;
pub mod terminal;
pub mod todos;
pub mod voice;

pub const SEP: &str = " \u{b7} ";
pub const ENVIRONMENT_TAG: &str = "<environment>";
pub const SENT_PREFIX: &str = "Sent: ";
pub const CONTEXT_PREFIX: &str = "Context: ";
pub const DATA_BLOCK: &str = "data-block";
pub const STEER_NOTE: &str = "Sent while working: the user wrote the message just above before seeing anything you did after they sent it.";

pub fn flat(text: &str) -> String {
    text.replace('\n', "^J").replace('\t', "^I")
}
