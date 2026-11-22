use futures::future::BoxFuture;

use crate::host::CallContext;
use crate::types::Question;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posted {
    Open,
    Busy,
    Unavailable,
}

pub trait Questions: Send + Sync {
    fn post<'a>(&'a self, items: Vec<Question>, ctx: &'a dyn CallContext) -> BoxFuture<'a, Posted>;

    fn withdraw<'a>(&'a self, ctx: &'a dyn CallContext) -> BoxFuture<'a, bool>;
}

impl Questions for () {
    fn post<'a>(
        &'a self,
        _items: Vec<Question>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Posted> {
        Box::pin(std::future::ready(Posted::Unavailable))
    }

    fn withdraw<'a>(&'a self, _ctx: &'a dyn CallContext) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(false))
    }
}
