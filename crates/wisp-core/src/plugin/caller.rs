use std::future::Future;

use futures::future::BoxFuture;
use wisp_plugin_api::Scope;

use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Render {
    Slot,
    Start,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct At {
    pub turn: Option<u64>,
    pub step: Option<u32>,
    pub call_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Caller {
    pub render: Option<Render>,
    pub cancel: Option<CancellationToken>,
    pub deadline: Option<tokio::time::Instant>,
    pub depth: u8,
    pub at: At,
    pub quiet_jobs: bool,
    pub describing: bool,
}

tokio::task_local! {
    static CALLER: Caller;
}

impl Caller {
    pub fn call(cancel: CancellationToken, deadline: Option<tokio::time::Instant>) -> Self {
        Self {
            cancel: Some(cancel),
            deadline,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn quiet_jobs(mut self) -> Self {
        self.quiet_jobs = true;
        self
    }

    #[must_use]
    pub fn hook(mut self, depth: u8, at: At) -> Self {
        self.depth = depth;
        self.at = at;
        self
    }

    pub fn rendering(render: Render) -> Self {
        Self {
            render: Some(render),
            ..Self::default()
        }
    }

    pub fn describing() -> Self {
        Self {
            describing: true,
            ..Self::default()
        }
    }

    pub async fn scope<F: Future>(self, future: F) -> F::Output {
        CALLER.scope(self, future).await
    }

    pub fn sync_scope<R>(self, f: impl FnOnce() -> R) -> R {
        CALLER.sync_scope(self, f)
    }

    pub fn sees_agents(&self) -> bool {
        match self.render {
            Some(Render::Slot) => true,
            Some(_) => false,
            None => self.cancel.is_some(),
        }
    }

    pub fn current() -> Self {
        CALLER.try_with(Clone::clone).unwrap_or_default()
    }
}

impl Scope for Caller {
    fn run(&self, f: &mut dyn FnMut()) {
        self.clone().sync_scope(f);
    }

    fn wrap<'a>(&self, f: BoxFuture<'a, ()>) -> BoxFuture<'a, ()> {
        Box::pin(self.clone().scope(f))
    }
}

#[cfg(test)]
mod tests;
