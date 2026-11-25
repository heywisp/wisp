mod calls;
pub(crate) mod http;

use wasmtime::component::{Accessor, Resource};
use wisp_plugin_api::host::Session;
use wisp_plugin_api::strategy::{CallEntry, SaidEntry, View};

use crate::bindings::wisp::plugin0_1_0 as w1;
use crate::bindings::wisp::plugin0_2_0 as w2;
use crate::bindings::wisp::plugin0_3_0 as w3;
use crate::mirror::Mirror;
use crate::state::{Imports, State, forward};

#[allow(
    unreachable_pub,
    reason = "bindgen! re-exports the types it maps resources to"
)]
pub struct Viewed {
    said: Vec<SaidEntry>,
    calls: Vec<CallEntry>,
    log_pointer: Option<String>,
    output_pointer: Option<String>,
}

impl Viewed {
    pub(crate) fn of(view: &dyn View) -> Self {
        Self {
            said: view.said(),
            calls: view.calls(),
            log_pointer: view.log_pointer(),
            output_pointer: view.output_pointer(),
        }
    }
}

impl w1::types::Host for State {}
impl w1::hook_types::Host for State {}
impl w2::agent_types::Host for State {}
impl w2::slot_types::Host for State {}
impl w2::prompt_types::Host for State {}

impl w1::session::Host for State {
    fn current(&mut self) -> wasmtime::Result<w1::session::Facts> {
        self.sync(|host| Session::current(host))
    }
}

impl w1::workspace::Host for State {
    fn root(&mut self) -> wasmtime::Result<String> {
        self.sync(|host| host.root())
    }

    fn home(&mut self) -> wasmtime::Result<Option<String>> {
        self.sync(|host| host.home())
    }

    fn shorten(&mut self, path: String) -> wasmtime::Result<String> {
        self.sync(|host| host.shorten(&path))
    }

    fn kind_of(&mut self, path: String) -> wasmtime::Result<Option<w1::workspace::FileKind>> {
        self.sync(|host| host.kind_of(&path))
    }
}

impl w1::clock::Host for State {
    fn now(&mut self) -> wasmtime::Result<w1::clock::Instant> {
        self.sync(|host| host.now())
    }

    fn local_offset(&mut self, at: w1::clock::Instant) -> wasmtime::Result<Option<i32>> {
        let at = at.mirror();
        self.sync(|host| host.local_offset(at))
    }

    fn elapsed_ms(&mut self) -> wasmtime::Result<u64> {
        self.sync(|host| host.elapsed_ms())
    }
}

impl<T> w1::clock::HostWithStore<T> for Imports {
    async fn sleep(accessor: &Accessor<T, Self>, ms: u64) -> wasmtime::Result<()> {
        forward(
            accessor,
            State::host,
            |host| async move { host.sleep(ms).await },
        )
        .await
    }
}

impl w1::settings::Host for State {
    fn get(&mut self, key: String) -> wasmtime::Result<Option<String>> {
        self.sync(|host| host.get(&key))
    }
}

impl w1::context::Host for State {
    fn current_fill(&mut self) -> wasmtime::Result<w1::context::Fill> {
        self.sync(|host| host.current_fill())
    }

    fn estimate_text(&mut self, text: String) -> wasmtime::Result<u64> {
        self.sync(|host| host.estimate_text(&text))
    }
}

impl w1::media::Host for State {
    fn media_types(&mut self) -> wasmtime::Result<Vec<String>> {
        self.sync(|host| host.media_types())
    }

    fn image_edge(&mut self) -> wasmtime::Result<Option<u32>> {
        self.sync(|host| host.image_edge())
    }
}

impl<T> w1::media::HostWithStore<T> for Imports {
    async fn count_tokens(
        accessor: &Accessor<T, Self>,
        media_type: String,
        bytes: Vec<u8>,
    ) -> wasmtime::Result<w1::media::TokenCount> {
        forward(accessor, State::host, |host| async move {
            host.count_tokens(media_type, bytes, None).await
        })
        .await
    }
}

impl w1::log::Host for State {
    fn write(&mut self, level: w1::log::Level, line: String) -> wasmtime::Result<()> {
        let level = level.mirror();
        self.sync(|host| host.write(level, &line))
    }
}

impl w1::points::Host for State {}

impl<T> w1::points::HostWithStore<T> for Imports {
    async fn fire(
        accessor: &Accessor<T, Self>,
        point: String,
        payload: String,
    ) -> wasmtime::Result<Vec<w1::points::Answer>> {
        forward(accessor, State::host, |host| async move {
            host.fire(point, payload).await
        })
        .await
    }
}

impl w2::workspace_facts::Host for State {
    fn environment(&mut self) -> wasmtime::Result<Option<w2::workspace_facts::EnvironmentFacts>> {
        self.sync(|host| host.environment())
    }

    fn config_dir(&mut self) -> wasmtime::Result<Option<String>> {
        self.sync(|host| host.config_dir())
    }

    fn data_dir(&mut self) -> wasmtime::Result<Result<String, String>> {
        self.sync(|host| host.data_dir())
    }

    fn terminal(&mut self) -> wasmtime::Result<Option<w2::workspace_facts::Size>> {
        self.sync(|host| host.terminal())
    }
}

impl w2::instructions_report::Host for State {
    fn loaded(&mut self, report: w2::instructions_report::LoadReport) -> wasmtime::Result<()> {
        let report = report.mirror();
        self.sync(|host| host.loaded(report))
    }

    fn warning(&mut self, diagnostic: w2::instructions_report::Diagnostic) -> wasmtime::Result<()> {
        let diagnostic = diagnostic.mirror();
        self.sync(|host| host.warning(diagnostic))
    }

    fn changed(
        &mut self,
        change: w2::instructions_report::Change,
        text: String,
    ) -> wasmtime::Result<()> {
        let change = change.mirror();
        self.sync(|host| host.changed(change, text))
    }
}

impl w2::job_news::Host for State {
    fn opening(&mut self) -> wasmtime::Result<Option<w2::job_news::News>> {
        self.sync(|host| host.opening())
    }
}

impl w2::pricing::Host for State {
    fn estimate(&mut self, usage: w2::pricing::Usage) -> wasmtime::Result<Option<u64>> {
        let usage = usage.mirror();
        self.sync(|host| host.estimate(usage))
    }

    fn spend(&mut self) -> wasmtime::Result<w2::pricing::SpendTotals> {
        self.sync(|host| host.spend())
    }
}

impl w2::agent::Host for State {
    fn current(&mut self) -> wasmtime::Result<Option<w2::agent::Identity>> {
        self.sync(|host| host.current_agent())
    }
}

impl w2::agent_tree::Host for State {
    fn snapshot(&mut self) -> wasmtime::Result<Vec<w2::agent_tree::AgentNode>> {
        self.sync(|host| host.snapshot())
    }
}

impl w2::history::HostView for State {
    fn said(&mut self, view: Resource<Viewed>) -> wasmtime::Result<Vec<w2::history::SaidEntry>> {
        Ok(self.table.get(&view)?.said.clone().mirror())
    }

    fn calls(&mut self, view: Resource<Viewed>) -> wasmtime::Result<Vec<w2::history::CallEntry>> {
        Ok(self.table.get(&view)?.calls.clone().mirror())
    }

    fn log_pointer(&mut self, view: Resource<Viewed>) -> wasmtime::Result<Option<String>> {
        Ok(self.table.get(&view)?.log_pointer.clone())
    }

    fn output_pointer(&mut self, view: Resource<Viewed>) -> wasmtime::Result<Option<String>> {
        Ok(self.table.get(&view)?.output_pointer.clone())
    }

    fn drop(&mut self, view: Resource<Viewed>) -> wasmtime::Result<()> {
        self.table.delete(view)?;
        Ok(())
    }
}
impl w2::history::Host for State {}

impl w3::types::Host for State {}
impl w3::slot_types::Host for State {}

impl w3::agent_states::Host for State {
    fn states(&mut self) -> wasmtime::Result<Vec<w3::agent_states::ShownState>> {
        self.sync(|host| host.states())
    }
}

impl w3::screen_report::Host for State {}

impl<T> w3::screen_report::HostWithStore<T> for Imports {
    async fn wait(
        accessor: &Accessor<T, Self>,
        call: String,
        within_ms: u64,
    ) -> wasmtime::Result<Option<w3::screen_report::ScreenReport>> {
        forward(accessor, State::host, |host| async move {
            wisp_plugin_api::host::ScreenReports::wait(host.as_ref(), call, within_ms).await
        })
        .await
    }
}

impl w3::index::Host for State {
    fn entries(&mut self) -> wasmtime::Result<Vec<w3::index::Entry>> {
        self.sync(|host| host.index().entries())
    }

    fn changed(&mut self) -> wasmtime::Result<()> {
        self.sync(|host| host.index().changed())
    }
}

impl<T> w3::index::HostWithStore<T> for Imports {
    async fn open(
        accessor: &Accessor<T, Self>,
        name: String,
    ) -> wasmtime::Result<Result<String, String>> {
        forward(accessor, State::host, |host| async move {
            host.index().open(&name).await
        })
        .await
    }
}
