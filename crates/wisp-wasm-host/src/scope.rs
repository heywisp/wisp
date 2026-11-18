use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::future::BoxFuture;
use wasmtime::component::{TaskGroupHook, TaskGroupId};
use wisp_plugin_api::Scope;
use wisp_plugin_api::host::Host;

#[derive(Default)]
struct Filed {
    current: Option<TaskGroupId>,
    scopes: BTreeMap<TaskGroupId, Arc<dyn Scope>>,
}

#[derive(Default)]
pub(crate) struct Groups {
    filed: Mutex<Filed>,
}

impl Groups {
    fn filed(&self) -> MutexGuard<'_, Filed> {
        self.filed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn file(&self, group: TaskGroupId, scope: Arc<dyn Scope>) {
        self.filed().scopes.insert(group, scope);
    }

    pub(crate) fn current(&self) -> Arc<dyn Scope> {
        let filed = self.filed();
        filed
            .current
            .and_then(|group| filed.scopes.get(&group).cloned())
            .unwrap_or_else(|| Arc::new(Unscoped))
    }
}

pub(crate) struct Hook(pub(crate) Arc<Groups>);

impl TaskGroupHook for Hook {
    fn handle_start(&mut self, _id: TaskGroupId) -> wasmtime::Result<()> {
        Ok(())
    }

    fn handle_enter(&mut self, id: TaskGroupId) -> wasmtime::Result<()> {
        self.0.filed().current = Some(id);
        Ok(())
    }

    fn handle_exit(&mut self, _id: TaskGroupId) -> wasmtime::Result<()> {
        self.0.filed().current = None;
        Ok(())
    }

    fn handle_finish(&mut self, id: TaskGroupId) -> wasmtime::Result<()> {
        self.0.filed().scopes.remove(&id);
        Ok(())
    }
}

pub(crate) struct Unscoped;

impl Scope for Unscoped {
    fn run(&self, f: &mut dyn FnMut()) {
        f();
    }

    fn wrap<'a>(&self, f: BoxFuture<'a, ()>) -> BoxFuture<'a, ()> {
        f
    }
}

pub(crate) fn caller_of(host: &dyn Host) -> Arc<dyn Scope> {
    host.kernel().map_or_else(
        || Arc::new(Unscoped) as Arc<dyn Scope>,
        |kernel| kernel.caller(),
    )
}

pub(crate) fn run_in<R>(scope: &dyn Scope, f: impl FnOnce() -> R) -> Option<R> {
    let mut f = Some(f);
    let mut out = None;
    scope.run(&mut || {
        if let Some(f) = f.take() {
            out = Some(f());
        }
    });
    out
}

pub(crate) async fn wrap_in<R: Send>(
    scope: &dyn Scope,
    f: impl Future<Output = R> + Send,
) -> Option<R> {
    let mut out = None;
    scope
        .wrap(Box::pin(async {
            out = Some(f.await);
        }))
        .await;
    out
}
