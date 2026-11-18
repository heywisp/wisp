use crate::config::LimitWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageState {
    Ok,
    Stale,
    Checking,
    SignedOut,
    Off,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsage {
    pub name: String,
    pub yours: bool,
    pub state: UsageState,
    pub limited: bool,
    pub retry_at: Option<i64>,
    pub windows: Vec<LimitWindow>,
}

pub trait Usage: Send + Sync {
    fn accounts(&self) -> Vec<AccountUsage>;
}

impl Usage for () {
    fn accounts(&self) -> Vec<AccountUsage> {
        Vec::new()
    }
}
