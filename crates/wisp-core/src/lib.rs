pub mod agent;
pub mod agents;
#[cfg(test)]
mod amount_marks_tests;
pub mod call;
pub mod clock;
pub mod config;
pub mod config_file;
pub mod context;
pub mod endpoints;
mod file_lock;
pub mod instructions;
pub mod jobs;
pub mod kit;
pub mod lanes;
pub mod mcp;
pub mod models;
pub mod plan_usage;
pub mod plugin;
pub mod policy;
pub mod pricing;
pub mod provider;
pub mod resume;
pub mod runtime;
pub mod search;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod spend;
#[cfg(test)]
pub(crate) mod stack_alarm;
pub mod subscription_login;
#[cfg(test)]
pub(crate) mod test_guard;
pub mod tool;
pub mod voice;
pub mod warmth;
pub mod workspace;

#[cfg(feature = "test-support")]
pub use file_lock::copies as lock_copies;

pub mod oauth;
pub use provider::gemini::GeminiProvider;

#[cfg(test)]
#[path = "../tests/support/mcp-http.rs"]
mod mcp_test;
