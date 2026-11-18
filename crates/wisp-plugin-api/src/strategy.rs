use crate::hooks::CompactInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaidEntry {
    pub turn: u64,
    pub text: String,
    pub seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEntry {
    pub turn: u64,
    pub tool: String,
    pub target: String,
    pub status: Option<String>,
    pub cwd: Option<String>,
    pub saved: Option<String>,
    pub attached: Vec<String>,
    pub withdrawn: bool,
}

pub trait View: Send + Sync {
    fn said(&self) -> Vec<SaidEntry>;
    fn calls(&self) -> Vec<CallEntry>;
    fn log_pointer(&self) -> Option<String>;
    fn output_pointer(&self) -> Option<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyInfo {
    pub compact: CompactInfo,
    pub epoch: u64,
    pub first_turn: u64,
    pub last_turn: u64,
    pub native: bool,
    pub cut_off: Option<String>,
    pub verbatim_tokens: u64,
}
