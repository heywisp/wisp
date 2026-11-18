use std::future::Future;
use std::sync::Arc;

use wasmtime::component::{Accessor, HasData, Resource, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wisp_plugin_api::host::{Host, Job};

use crate::context::CallLink;
use crate::jobs::JobHandle;
use crate::mirror::Mirror;
use crate::scope::{Groups, run_in, wrap_in};

pub(crate) struct State {
    pub(crate) host: Arc<dyn Host>,
    pub(crate) groups: Arc<Groups>,
    pub(crate) table: ResourceTable,
    pub(crate) wasi: WasiCtx,
}

pub(crate) struct Imports;

impl HasData for Imports {
    type Data<'a> = &'a mut State;
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

fn unanswered() -> wasmtime::Error {
    wasmtime::format_err!("the kernel didn't run an import of this plugin")
}

impl State {
    pub(crate) fn sync<A: Mirror<G>, G>(
        &mut self,
        f: impl FnOnce(&dyn Host) -> A,
    ) -> wasmtime::Result<G> {
        let scope = self.groups.current();
        let host = &*self.host;
        run_in(&*scope, || f(host))
            .map(Mirror::mirror)
            .ok_or_else(unanswered)
    }

    pub(crate) fn host(&mut self) -> wasmtime::Result<Arc<dyn Host>> {
        Ok(self.host.clone())
    }

    pub(crate) fn job(&mut self, job: &Resource<JobHandle>) -> wasmtime::Result<Arc<dyn Job>> {
        Ok(self.table.get(job)?.0.clone())
    }

    pub(crate) fn link(&mut self, link: &Resource<CallLink>) -> wasmtime::Result<CallLink> {
        Ok(self.table.get(link)?.clone())
    }
}

pub(crate) async fn forward_raw<T, X, Fut, A>(
    accessor: &Accessor<T, Imports>,
    take: impl FnOnce(&mut State) -> wasmtime::Result<X>,
    f: impl FnOnce(X) -> Fut,
) -> wasmtime::Result<A>
where
    Fut: Future<Output = A> + Send,
    A: Send,
{
    let (taken, scope) = accessor.with(|mut access| {
        let state = access.get();
        take(state).map(|taken| (taken, state.groups.current()))
    })?;
    wrap_in(&*scope, f(taken)).await.ok_or_else(unanswered)
}

pub(crate) async fn forward<T, X, Fut, A, G>(
    accessor: &Accessor<T, Imports>,
    take: impl FnOnce(&mut State) -> wasmtime::Result<X>,
    f: impl FnOnce(X) -> Fut,
) -> wasmtime::Result<G>
where
    Fut: Future<Output = A> + Send,
    A: Mirror<G> + Send,
{
    forward_raw(accessor, take, f).await.map(Mirror::mirror)
}
