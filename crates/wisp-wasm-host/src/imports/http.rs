use std::sync::Arc;

use wasmtime::component::{Accessor, Resource};
use wisp_plugin_api::http::Body;

use crate::bindings::wisp::plugin0_2_0::http::{
    Host, HostBody, HostBodyWithStore, HostWithStore, HttpError, Request, Response,
};
use crate::mirror::Mirror;
use crate::state::{Imports, State, forward_raw};

#[allow(
    unreachable_pub,
    reason = "bindgen! re-exports the types it maps resources to"
)]
pub struct BodyHandle(Arc<dyn Body>);

impl<T> HostBodyWithStore<T> for Imports {
    async fn read(
        accessor: &Accessor<T, Self>,
        self_: Resource<BodyHandle>,
        max: u32,
    ) -> wasmtime::Result<Result<Option<Vec<u8>>, String>> {
        let body = |state: &mut State| Ok(state.table.get(&self_)?.0.clone());
        forward_raw(accessor, body, |body| async move { body.read(max).await }).await
    }
}

impl HostBody for State {
    fn drop(&mut self, rep: Resource<BodyHandle>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl<T> HostWithStore<T> for Imports {
    async fn fetch(
        accessor: &Accessor<T, Self>,
        req: Request,
    ) -> wasmtime::Result<Result<Response, HttpError>> {
        let req = req.mirror();
        let fetched = forward_raw(accessor, State::host, |host| async move {
            host.fetch(req).await
        })
        .await?;
        match fetched {
            Ok(response) => accessor.with(|mut access| {
                let body = access
                    .get()
                    .table
                    .push(BodyHandle(Arc::from(response.body)))?;
                Ok(Ok(Response {
                    status: response.status,
                    headers: response.headers,
                    body,
                }))
            }),
            Err(error) => Ok(Err(error.mirror())),
        }
    }
}

impl Host for State {}
