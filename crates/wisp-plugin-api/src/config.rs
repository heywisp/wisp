use futures::future::BoxFuture;

use crate::host::CallContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    pub name: String,
    pub value: String,
    pub layer: String,
    pub help: String,
    pub rule: String,
}

pub trait Config {
    fn settings(
        &self,
        filter: Option<String>,
        ctx: &dyn CallContext,
    ) -> Result<Vec<SettingRow>, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    Working,
    Expired,
    Refreshable,
    Unavailable,
    ExpiryUnknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub resets_at: Option<i64>,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    pub windows: Vec<LimitWindow>,
    pub limited: bool,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountRow {
    pub name: String,
    pub kind: Option<String>,
    pub unavailable: bool,
    pub sign_in: Option<SignIn>,
    pub yours: bool,
    pub start: bool,
    pub models: Vec<String>,
    pub limits: Option<Limits>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountList {
    pub model: String,
    pub effort: String,
    pub rows: Vec<AccountRow>,
}

pub trait Accounts {
    fn list<'a>(&'a self, ctx: &'a dyn CallContext) -> BoxFuture<'a, Result<AccountList, String>>;
}
