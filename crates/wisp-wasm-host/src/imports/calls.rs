use wasmtime::component::{Accessor, Resource};

use crate::bindings::wisp::plugin0_1_0 as w1;
use crate::bindings::wisp::plugin0_2_0 as w2;
use crate::bindings::wisp::plugin0_3_0 as w3;
use crate::context::{CallLink, asking, mirrored, now};
use crate::mirror::Mirror;
use crate::state::{Imports, State};

impl w1::todos::Host for State {
    async fn replace(
        &mut self,
        items: Vec<w1::todos::Todo>,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<(), String>> {
        let items = items.mirror();
        mirrored(asking(self, &ctx, move |host, ctx| {
            now(host.replace(items, ctx))
        }))
        .await
    }
}

impl<T> w1::agents::HostWithStore<T> for Imports {
    async fn spawn_agent(
        accessor: &Accessor<T, Self>,
        input: w1::agents::SpawnInput,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<w1::agents::StartedAgent, String>> {
        let input = input.mirror();
        let asked = accessor.with(|mut access| {
            asking(access.get(), &ctx, move |host, ctx| {
                host.spawn_agent(input, ctx)
            })
        });
        mirrored(asked).await
    }
}

impl w1::agents::Host for State {
    async fn list_agents(
        &mut self,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<Vec<w1::agents::AgentRow>, String>> {
        mirrored(asking(self, &ctx, |host, ctx| now(host.list_agents(ctx)))).await
    }

    async fn report(
        &mut self,
        input: String,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<(), String>> {
        mirrored(asking(self, &ctx, move |host, ctx| {
            now(host.report(input, ctx))
        }))
        .await
    }

    async fn send_message(
        &mut self,
        to: String,
        text: String,
        wake: bool,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<w1::agents::Sent, String>> {
        mirrored(asking(self, &ctx, move |host, ctx| {
            now(host.send_message(to, text, wake, ctx))
        }))
        .await
    }

    async fn stop_agent(
        &mut self,
        name: String,
        end: bool,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<w1::agents::Stopped, String>> {
        mirrored(asking(self, &ctx, move |host, ctx| {
            now(host.stop_agent(name, end, ctx))
        }))
        .await
    }
}

impl w2::config::Host for State {
    async fn settings(
        &mut self,
        filter: Option<String>,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<Vec<w2::config::SettingRow>, String>> {
        mirrored(asking(self, &ctx, move |host, ctx| {
            now(host.settings(filter, ctx))
        }))
        .await
    }
}

impl<T> w2::accounts::HostWithStore<T> for Imports {
    async fn list(
        accessor: &Accessor<T, Self>,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<w2::accounts::AccountList, String>> {
        let asked =
            accessor.with(|mut access| asking(access.get(), &ctx, |host, ctx| host.list(ctx)));
        mirrored(asked).await
    }
}

impl w2::accounts::Host for State {}

impl<T> w3::secrets::HostWithStore<T> for Imports {
    async fn type_into(
        accessor: &Accessor<T, Self>,
        ask: w3::secrets::SecretAsk,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<w3::secrets::SecretEnd> {
        let ask = ask.mirror();
        let asked = accessor.with(|mut access| {
            asking(access.get(), &ctx, move |host, ctx| {
                host.type_into(ask, ctx)
            })
        });
        mirrored(asked).await
    }
}

impl w3::secrets::Host for State {}

impl w3::agent_rename::Host for State {
    async fn rename_agent(
        &mut self,
        name: String,
        new_name: String,
        ctx: Resource<CallLink>,
    ) -> wasmtime::Result<Result<w3::agent_rename::Renamed, String>> {
        mirrored(asking(self, &ctx, move |host, ctx| {
            now(host.rename_agent(name, new_name, ctx))
        }))
        .await
    }
}
