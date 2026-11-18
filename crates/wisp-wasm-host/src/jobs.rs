use std::sync::Arc;

use wasmtime::component::{Accessor, Resource};
use wisp_plugin_api::host::Job;

use crate::bindings::wisp::plugin0_1_0::call::CallContext;
use crate::bindings::wisp::plugin0_1_0::jobs::{
    Captured, Found, HoldSpec, Host, HostJob, HostJobWithStore, HostWithStore, JobError, JobInfo,
    JobOutput, Read, Rows, SavedFile, Screen, Settled, SignalKind, Size, SpawnSpec, Typing,
    WaitFor, Waited, Written,
};
use crate::bindings::wisp::plugin0_1_0::types::AskReply;
use crate::context::{asking, mirrored, now};
use crate::mirror::Mirror;
use crate::state::{Imports, State, forward};

#[allow(
    unreachable_pub,
    reason = "bindgen! re-exports the types it maps resources to"
)]
pub struct JobHandle(pub(crate) Arc<dyn Job>);

fn keep(state: &mut State, job: Box<dyn Job>) -> wasmtime::Result<Resource<JobHandle>> {
    Ok(state.table.push(JobHandle(Arc::from(job)))?)
}

impl<T> HostJobWithStore<T> for Imports {
    async fn info(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
    ) -> wasmtime::Result<JobInfo> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.info().await },
        )
        .await
    }

    async fn write(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        input: Typing,
        within_ms: u64,
    ) -> wasmtime::Result<Written> {
        let input = input.mirror();
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.write(input, within_ms).await },
        )
        .await
    }

    async fn signal(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        kind: SignalKind,
        within_ms: u64,
    ) -> wasmtime::Result<Written> {
        let kind = kind.mirror();
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.signal(kind, within_ms).await },
        )
        .await
    }

    async fn wait(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        how: WaitFor,
    ) -> wasmtime::Result<Result<Waited, String>> {
        let how = how.mirror();
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.wait(how).await },
        )
        .await
    }

    async fn hold(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        spec: HoldSpec,
    ) -> wasmtime::Result<Waited> {
        let spec = spec.mirror();
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.hold(spec).await },
        )
        .await
    }

    async fn settle(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        quiet_ms: u64,
        max_ms: u64,
    ) -> wasmtime::Result<Settled> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.settle(quiet_ms, max_ms).await },
        )
        .await
    }

    async fn screen(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
    ) -> wasmtime::Result<Option<Screen>> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.screen().await },
        )
        .await
    }

    async fn new_output(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
    ) -> wasmtime::Result<Read> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.new_output().await },
        )
        .await
    }

    async fn tail(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        lines: u32,
    ) -> wasmtime::Result<Read> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.tail(lines).await },
        )
        .await
    }

    async fn output(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
    ) -> wasmtime::Result<JobOutput> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.output().await },
        )
        .await
    }

    async fn saved(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
    ) -> wasmtime::Result<Option<SavedFile>> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.saved().await },
        )
        .await
    }

    async fn lines(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        first: u64,
        count: u32,
    ) -> wasmtime::Result<Rows> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.lines(first, count).await },
        )
        .await
    }

    async fn search(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        pattern: String,
        from: u64,
        context: u32,
        max: u32,
    ) -> wasmtime::Result<Result<Found, String>> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.search(pattern, from, context, max).await },
        )
        .await
    }

    async fn background(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
    ) -> wasmtime::Result<Option<u32>> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.background().await },
        )
        .await
    }

    async fn ask_attach(
        accessor: &Accessor<T, Self>,
        self_: Resource<JobHandle>,
        reason: String,
    ) -> wasmtime::Result<AskReply> {
        forward(
            accessor,
            |s| s.job(&self_),
            |job| async move { job.ask_attach(reason).await },
        )
        .await
    }
}

impl HostJob for State {
    fn resize(
        &mut self,
        self_: Resource<JobHandle>,
        to: Size,
    ) -> wasmtime::Result<Result<(), JobError>> {
        let job = self.job(&self_)?;
        let to = to.mirror();
        self.sync(|_| job.resize(to))
    }

    fn set_limit(&mut self, self_: Resource<JobHandle>, ms: Option<u64>) -> wasmtime::Result<()> {
        let job = self.job(&self_)?;
        self.sync(|_| job.set_limit(ms))
    }

    fn drop(&mut self, rep: Resource<JobHandle>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl<T> HostWithStore<T> for Imports {
    async fn spawn(
        accessor: &Accessor<T, Self>,
        spec: SpawnSpec,
        ctx: Resource<CallContext>,
    ) -> wasmtime::Result<Result<Resource<JobHandle>, JobError>> {
        let spec = spec.mirror();
        let asked = accessor
            .with(|mut access| asking(access.get(), &ctx, move |host, ctx| host.spawn(spec, ctx)));
        match asked?.await? {
            Ok(job) => accessor.with(|mut access| keep(access.get(), job).map(Ok)),
            Err(error) => Ok(Err(error.mirror())),
        }
    }

    async fn all(accessor: &Accessor<T, Self>) -> wasmtime::Result<Vec<JobInfo>> {
        forward(
            accessor,
            State::host,
            |host| async move { host.all().await },
        )
        .await
    }

    async fn capture(
        accessor: &Accessor<T, Self>,
        program: String,
        args: Vec<String>,
        timeout_ms: u64,
    ) -> wasmtime::Result<Captured> {
        forward(accessor, State::host, |host| async move {
            host.capture(program, args, timeout_ms).await
        })
        .await
    }
}

impl Host for State {
    async fn adopt(
        &mut self,
        id: u32,
        ctx: Resource<CallContext>,
    ) -> wasmtime::Result<Option<Resource<JobHandle>>> {
        let asked = asking(self, &ctx, move |host, ctx| now(host.adopt(id, ctx)));
        let adopted = asked?.await?;
        adopted.map(|job| keep(self, job)).transpose()
    }

    async fn mark_reported(&mut self, id: u32, ctx: Resource<CallContext>) -> wasmtime::Result<()> {
        mirrored(asking(self, &ctx, move |host, ctx| {
            host.mark_reported(id, ctx);
            now(())
        }))
        .await
    }

    fn which(&mut self, program: String) -> wasmtime::Result<Option<String>> {
        self.sync(|host| host.which(&program))
    }
}
