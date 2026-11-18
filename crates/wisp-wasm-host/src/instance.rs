use std::sync::Arc;
use std::task::Poll;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use tokio::sync::{mpsc, watch};
use tokio::task::AbortHandle;
use wasmtime::component::{Accessor, ComponentNamedList, Lift, Lower, ResourceTable, TypedFunc};
use wasmtime::{Store, Trap, UpdateDeadline};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder};
use wisp_contain::Contained;
use wisp_plugin_api::Scope;
use wisp_plugin_api::host::Host;

use crate::exports::{Exports, Pre};
use crate::plugin::UNBOUNDED;
use crate::scope::{Groups, Hook};
use crate::state::State;

pub(crate) type Work = Box<
    dyn for<'a> FnOnce(&'a Accessor<State>, &'a Exports) -> BoxFuture<'a, wasmtime::Result<()>>
        + Send,
>;

const SEED: u128 = 0x5769_7370_5741_534d_5365_6564_0000_0001;

const REASON_CHARS: usize = 200;

pub(crate) const CUT: &str = "cut";

pub(crate) struct Ended(watch::Sender<Option<String>>);

impl Default for Ended {
    fn default() -> Self {
        Self(watch::Sender::new(None))
    }
}

impl Ended {
    pub(crate) fn set(&self, reason: String) {
        self.0.send_if_modified(|slot| {
            let unset = slot.is_none();
            if unset {
                *slot = Some(reason.chars().take(REASON_CHARS).collect());
            }
            unset
        });
    }

    fn is_set(&self) -> bool {
        self.0.borrow().is_some()
    }

    pub(crate) async fn reason(&self) -> String {
        let mut set = self.0.subscribe();
        set.wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|reason| reason.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone)]
pub(crate) struct Live {
    pub(crate) work: mpsc::UnboundedSender<Work>,
    pub(crate) generation: u64,
    pub(crate) ended: Arc<Ended>,
    ready: Arc<watch::Sender<bool>>,
    task: AbortHandle,
}

impl Live {
    pub(crate) fn running(&self) -> bool {
        !self.ended.is_set() && !self.task.is_finished() && !self.work.is_closed()
    }

    pub(crate) fn cut(&self) {
        self.ended.set(CUT.to_owned());
        self.task.abort();
    }

    pub(crate) fn set_up(&self) {
        self.ready.send_replace(true);
    }

    pub(crate) async fn settled(&self) -> bool {
        let mut ready = self.ready.subscribe();
        tokio::select! {
            biased;
            set_up = ready.wait_for(|ready| *ready) => set_up.is_ok(),
            _ = self.ended.reason() => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mark {
    pub(crate) plugin: usize,
    pub(crate) generation: u64,
}

tokio::task_local! {
    pub(crate) static SETTING_UP: Mark;
}

pub(crate) struct Marked {
    pub(crate) scope: Arc<dyn Scope>,
    pub(crate) mark: Mark,
}

impl Scope for Marked {
    fn run(&self, f: &mut dyn FnMut()) {
        SETTING_UP.sync_scope(self.mark, || self.scope.run(f));
    }

    fn wrap<'a>(&self, f: BoxFuture<'a, ()>) -> BoxFuture<'a, ()> {
        Box::pin(SETTING_UP.scope(self.mark, self.scope.wrap(f)))
    }
}

pub(crate) fn reason_of(error: &wasmtime::Error) -> String {
    match error.downcast_ref::<Trap>() {
        Some(trap) => trap.to_string(),
        None => error
            .to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
    }
}

fn wasi(env: &[(String, String)]) -> Result<WasiCtx, String> {
    let mut builder = WasiCtxBuilder::new();
    builder
        .envs(env)
        .inherit_network()
        .allow_ip_name_lookup(true)
        .insecure_random_seed(SEED)
        .allow_blocking_current_thread(true)
        .preopened_dir("/", "/", FsPerms::ReadWrite)
        .map_err(|e| reason_of(&e))?;
    Ok(builder.build())
}

pub(crate) struct Start {
    pub(crate) pre: Pre,
    pub(crate) host: Arc<dyn Host>,
    pub(crate) env: Arc<[(String, String)]>,
    pub(crate) generation: u64,
    pub(crate) first: Option<Work>,
    pub(crate) ready: bool,
}

pub(crate) fn start(start: Start) -> Live {
    let (work, works) = mpsc::unbounded_channel();
    let ended = Arc::new(Ended::default());
    let generation = start.generation;
    let ready = Arc::new(watch::Sender::new(start.ready));
    let task = tokio::spawn(serve(start, works, ended.clone())).abort_handle();
    Live {
        work,
        generation,
        ended,
        ready,
        task,
    }
}

async fn serve(start: Start, mut works: mpsc::UnboundedReceiver<Work>, ended: Arc<Ended>) {
    let reason = match Contained::new(run(start, &mut works, &ended)).await {
        Ok(Ok(())) => "it ended".to_owned(),
        Ok(Err(reason)) => reason,
        Err(_) => "it panicked".to_owned(),
    };
    ended.set(reason);
}

async fn run(
    start: Start,
    works: &mut mpsc::UnboundedReceiver<Work>,
    ended: &Ended,
) -> Result<(), String> {
    let state = State {
        host: start.host,
        groups: Arc::new(Groups::default()),
        table: ResourceTable::new(),
        wasi: wasi(&start.env)?,
    };
    let pre = start.pre;
    let mut store = Store::new(pre.engine(), state);
    store.epoch_deadline_callback(|_| Ok(UpdateDeadline::Yield(1)));
    store.set_epoch_deadline(1);
    let groups = store.data().groups.clone();
    store.task_group_hook(Hook(groups));
    let exports = pre
        .instantiate(&mut store)
        .await
        .map_err(|e| reason_of(&e))?;
    let served = store
        .run_concurrent(async |accessor| {
            if let Some(first) = start.first {
                tokio::time::timeout(UNBOUNDED, first(accessor, &exports))
                    .await
                    .map_err(|_| wasmtime::format_err!("{CUT}"))??;
            }
            let mut running = FuturesUnordered::new();
            std::future::poll_fn(|cx| {
                loop {
                    match running.poll_next_unpin(cx) {
                        Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                        Poll::Ready(Some(Ok(()))) => continue,
                        Poll::Ready(None) | Poll::Pending => {}
                    }
                    match works.poll_recv(cx) {
                        Poll::Ready(Some(work)) => running.push(work(accessor, &exports)),
                        Poll::Ready(None) => return Poll::Ready(Ok(())),
                        Poll::Pending => return Poll::Pending,
                    }
                }
            })
            .await
        })
        .await;
    served.and_then(|served| served).map_err(|error| {
        let reason = reason_of(&error);
        ended.set(reason.clone());
        reason
    })
}

pub(crate) async fn call<P, R>(
    accessor: &Accessor<State>,
    scope: Arc<dyn Scope>,
    func: TypedFunc<P, R>,
    params: P,
) -> wasmtime::Result<R>
where
    P: ComponentNamedList + Lower + Send + Sync + 'static,
    R: ComponentNamedList + Lift + Send + Sync + 'static,
{
    let started = accessor.with(|mut access| {
        let started = func.start_call_concurrent(&mut access, params)?;
        access.get().groups.file(started.group(), scope);
        Ok::<_, wasmtime::Error>(started)
    })?;
    func.finish_call_concurrent(accessor, started).await
}
