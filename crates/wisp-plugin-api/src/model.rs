use futures::future::BoxFuture;

use crate::host::CallContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: String,
    pub text: String,
    pub max_tokens: u32,
    pub native: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskError {
    NotAllowed(String),
    Cancelled,
    Refused(String),
    Failed(String),
}

pub trait Model: Send + Sync {
    fn ask<'a>(
        &'a self,
        req: Prompt,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<Reply, AskError>>;
}

impl Model for () {
    fn ask<'a>(
        &'a self,
        _req: Prompt,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<Reply, AskError>> {
        Box::pin(std::future::ready(Err(AskError::NotAllowed(
            "model requests are unavailable".to_owned(),
        ))))
    }
}
