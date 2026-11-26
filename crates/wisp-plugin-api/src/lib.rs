pub mod chat;
pub mod commands;
pub mod config;
pub mod hex;
pub mod hooks;
pub mod host;
pub mod http;
pub mod index;
pub mod instructions;
pub mod manage;
pub mod manifest;
pub mod mcp;
pub mod model;
pub mod pricing;
pub mod private_files;
pub mod project;
pub mod prompt;
pub mod questions;
pub mod secret_check;
pub mod slots;
pub mod strategy;
pub mod tools;
pub mod tree;
pub mod types;
pub mod usage;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

pub use futures::future::BoxFuture;

use crate::commands::CommandReply;
use crate::hooks::{Event, Reply};
use crate::host::{CallContext, Host};
use crate::prompt::{At, Prior, Rendered};
use crate::slots::{Content, SlotView};
use crate::strategy::{StrategyInfo, View};
use crate::tools::{BuildContext, DoctorRow, Prepared, ToolDef};
use crate::tree::TreeEvent;
use crate::types::{Display, ToolRun};

const NO_TOOLS: &str = "this plugin has no tools";

pub trait Plugin: Send + Sync {
    fn entries(&self) -> BoxFuture<'_, Vec<index::Entry>> {
        Box::pin(std::future::ready(Vec::new()))
    }
    fn open_entry<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(std::future::ready(Err(format!(
            "nothing named `{name}` was found"
        ))))
    }

    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(std::future::ready(Vec::new()))
    }
    fn doctor<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
        Box::pin(std::future::ready(Vec::new()))
    }
    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(std::future::ready(Err(NO_TOOLS.to_owned())))
    }
    fn rejected_display<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        Box::pin(std::future::ready(None))
    }
    fn time_limit_ms<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(None))
    }
    fn movable<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(false))
    }
    fn interrupted_result<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(std::future::ready(None))
    }
    fn run<'a>(
        &'a self,
        _call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(std::future::ready(
            ToolRun::failed_to_start(NO_TOOLS).with_result(NO_TOOLS),
        ))
    }
    fn run_command<'a>(&'a self, _name: &'a str, _args: &'a str) -> BoxFuture<'a, CommandReply> {
        Box::pin(std::future::ready(CommandReply::None))
    }
    fn on_hook<'a>(&'a self, _hook: &'a str, _event: Event) -> BoxFuture<'a, Reply> {
        Box::pin(std::future::ready(Reply::None))
    }
    fn render_slot<'a>(
        &'a self,
        _name: &'a str,
        _view: SlotView,
    ) -> BoxFuture<'a, Result<Content, String>> {
        Box::pin(std::future::ready(Ok(Content::default())))
    }
    fn lock_for_render(&self) -> Option<Box<dyn Send>> {
        Some(Box::new(()))
    }
    fn restarts_when_cut(&self) -> bool {
        false
    }
    fn render_section<'a>(
        &'a self,
        _name: &'a str,
        _at: At,
        _previous: Prior,
        _target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        Box::pin(std::future::ready(Rendered::default()))
    }
    fn on_agent_hook<'a>(&'a self, _hook: &'a str, _event: TreeEvent) -> BoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
    fn instruction<'a>(&'a self, _info: StrategyInfo) -> BoxFuture<'a, Option<String>> {
        Box::pin(std::future::ready(None))
    }
    fn accept<'a>(
        &'a self,
        _info: StrategyInfo,
        _summary: String,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(std::future::ready(Ok(())))
    }
    fn head<'a>(
        &'a self,
        _info: StrategyInfo,
        _view: &'a dyn View,
        _summary: Option<String>,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(std::future::ready(Vec::new()))
    }
}

pub trait Scope: Send + Sync {
    fn run(&self, f: &mut dyn FnMut());
    fn wrap<'a>(&self, f: BoxFuture<'a, ()>) -> BoxFuture<'a, ()>;
}

pub trait Kernel: Send + Sync {
    fn caller(&self) -> Arc<dyn Scope>;
    fn restarted(&self) -> BoxFuture<'_, ()>;
}

#[derive(Clone, Copy)]
pub struct Bundled {
    pub manifest: &'static str,
    pub instantiate: fn(Arc<dyn Host>) -> Arc<dyn Plugin>,
}

#[must_use = "a yield does nothing unless awaited"]
#[derive(Debug, Default)]
pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            return Poll::Ready(());
        }
        self.yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

pub fn yield_now() -> YieldNow {
    YieldNow::default()
}

#[cfg(test)]
mod tests;
