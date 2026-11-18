use futures::future::BoxFuture;

use crate::host::CallContext;

pub const UNAVAILABLE: &str = "switching tools and skills is unavailable here";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    On,
    Off,
    Add,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Tool,
    Skill,
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub action: Action,
    pub kind: Kind,
    pub name: String,
    pub from: Option<String>,
    pub keys: Vec<String>,
}

pub trait Manage: Send + Sync {
    fn manage<'a>(
        &'a self,
        change: Change,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<String, String>>;
}

impl Manage for () {
    fn manage<'a>(
        &'a self,
        _change: Change,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(std::future::ready(Err(UNAVAILABLE.to_owned())))
    }
}
