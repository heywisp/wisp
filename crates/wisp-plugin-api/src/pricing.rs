#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub web_searches: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpendTotals {
    pub turn: Option<u64>,
    pub turn_at_least: bool,
    pub session: Option<u64>,
    pub session_at_least: bool,
}

pub trait Pricing {
    fn estimate(&self, usage: Usage) -> Option<u64>;
    fn spend(&self) -> SpendTotals;
}
