use std::future::Future;

use futures::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};
use wasmtime::component::{Accessor, Resource};
use wisp_plugin_api::host::CallContext;
use wisp_plugin_api::host::Host;

use crate::bindings::wisp::plugin0_1_0::call::{Host as CallHost, OutputSummary};
use crate::bindings::wisp::plugin0_1_0::call::{HostCallContext, HostCallContextWithStore};
use crate::bindings::wisp::plugin0_1_0::types::{AskReply, Question};
use crate::mirror::Mirror;
use crate::state::{Imports, State};

pub(crate) type Request = Box<dyn for<'c> FnOnce(&'c dyn CallContext) -> BoxFuture<'c, ()> + Send>;

#[derive(Clone)]
#[allow(
    unreachable_pub,
    reason = "bindgen! re-exports the types it maps resources to"
)]
pub struct CallLink {
    requests: mpsc::UnboundedSender<Request>,
}

fn call_ended() -> wasmtime::Error {
    wasmtime::format_err!("the call this context belongs to has ended")
}

impl CallLink {
    pub(crate) fn new() -> (Self, mpsc::UnboundedReceiver<Request>) {
        let (requests, received) = mpsc::unbounded_channel();
        (Self { requests }, received)
    }

    pub(crate) fn ask<A: Send + 'static>(
        self,
        f: impl for<'c> FnOnce(&'c dyn CallContext) -> BoxFuture<'c, A> + Send + 'static,
    ) -> impl Future<Output = wasmtime::Result<A>> + Send {
        let (reply, answer) = oneshot::channel();
        let request: Request = Box::new(move |ctx| {
            Box::pin(async move {
                let _ = reply.send(f(ctx).await);
            })
        });
        let sent = self.requests.send(request).map_err(|_| call_ended());
        async move {
            sent?;
            answer.await.map_err(|_| call_ended())
        }
    }
}

pub(crate) fn now<'c, A: Send + 'c>(value: A) -> BoxFuture<'c, A> {
    Box::pin(std::future::ready(value))
}

pub(crate) fn asking<A, F>(
    state: &mut State,
    ctx: &Resource<CallLink>,
    f: F,
) -> wasmtime::Result<impl Future<Output = wasmtime::Result<A>> + Send + use<A, F>>
where
    A: Send + 'static,
    F: for<'h> FnOnce(&'h dyn Host, &'h dyn CallContext) -> BoxFuture<'h, A> + Send + 'static,
{
    let link = state.link(ctx)?;
    let host = state.host.clone();
    Ok(link.ask(move |ctx| Box::pin(async move { f(&*host, ctx).await })))
}

pub(crate) async fn mirrored<A: Mirror<G>, G>(
    asked: wasmtime::Result<impl Future<Output = wasmtime::Result<A>>>,
) -> wasmtime::Result<G> {
    asked?.await.map(Mirror::mirror)
}

impl<T> HostCallContextWithStore<T> for Imports {
    async fn wait_cancelled(
        accessor: &Accessor<T, Self>,
        self_: Resource<CallLink>,
    ) -> wasmtime::Result<()> {
        let asked =
            accessor.with(|mut access| asking(access.get(), &self_, |_, ctx| ctx.wait_cancelled()));
        mirrored(asked).await
    }

    async fn ask(
        accessor: &Accessor<T, Self>,
        self_: Resource<CallLink>,
        items: Vec<Question>,
    ) -> wasmtime::Result<AskReply> {
        let items = items.mirror();
        let asked =
            accessor.with(|mut access| asking(access.get(), &self_, move |_, ctx| ctx.ask(items)));
        mirrored(asked).await
    }
}

impl HostCallContext for State {
    async fn drop(&mut self, rep: Resource<CallLink>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }

    async fn id(&mut self, self_: Resource<CallLink>) -> wasmtime::Result<String> {
        mirrored(asking(self, &self_, |_, ctx| now(ctx.id()))).await
    }

    async fn cwd(&mut self, self_: Resource<CallLink>) -> wasmtime::Result<String> {
        mirrored(asking(self, &self_, |_, ctx| now(ctx.cwd()))).await
    }

    async fn deadline_ms(&mut self, self_: Resource<CallLink>) -> wasmtime::Result<Option<u64>> {
        mirrored(asking(self, &self_, |_, ctx| now(ctx.deadline_ms()))).await
    }

    async fn cancelled(&mut self, self_: Resource<CallLink>) -> wasmtime::Result<bool> {
        mirrored(asking(self, &self_, |_, ctx| now(ctx.cancelled()))).await
    }

    async fn moved(&mut self, self_: Resource<CallLink>) -> wasmtime::Result<bool> {
        mirrored(asking(self, &self_, |_, ctx| now(ctx.moved()))).await
    }

    async fn steered(&mut self, self_: Resource<CallLink>) -> wasmtime::Result<bool> {
        mirrored(asking(self, &self_, |_, ctx| now(ctx.steered()))).await
    }

    async fn live_tail(&mut self, self_: Resource<CallLink>) -> wasmtime::Result<bool> {
        mirrored(asking(self, &self_, |_, ctx| now(ctx.live_tail()))).await
    }

    async fn write_output(
        &mut self,
        self_: Resource<CallLink>,
        bytes: Vec<u8>,
    ) -> wasmtime::Result<()> {
        mirrored(asking(self, &self_, move |_, ctx| {
            ctx.write_output(&bytes);
            now(())
        }))
        .await
    }

    async fn output_summary(
        &mut self,
        self_: Resource<CallLink>,
    ) -> wasmtime::Result<OutputSummary> {
        mirrored(asking(self, &self_, |_, ctx| now(ctx.output_summary()))).await
    }

    async fn private_file(
        &mut self,
        self_: Resource<CallLink>,
        suffix: String,
    ) -> wasmtime::Result<Result<String, String>> {
        mirrored(asking(self, &self_, move |_, ctx| {
            now(ctx.private_file(&suffix))
        }))
        .await
    }
}

impl CallHost for State {}
