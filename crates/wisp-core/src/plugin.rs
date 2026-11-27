pub mod caller;
pub mod commands;
pub mod convert;
pub mod hooks;
pub mod host;
pub mod http;
pub mod imports;
pub mod live;
pub mod registry;
#[cfg(test)]
#[cfg(unix)]
mod shell_tests;
pub mod slots;
#[cfg(test)]
pub(crate) mod testing;

pub use host::{Instructed, KernelHost, PluginHost, Prices, Snapshot};
pub use live::{Installed, Live, merged_tools};
pub use registry::{
    Make, PluginConfig, PluginSwitches, Prestarted, Problem, Registry, Source, Started,
    StartedHooks, Tools, unknown_plugin,
};
pub use wisp_protocol::plugin::Catalog;
use wisp_protocol::settings::names;

#[cfg(unix)]
pub const PLATFORM: &str = "unix";

#[cfg(not(unix))]
pub const PLATFORM: &str = "windows";

#[derive(Clone, Default)]
pub struct DoctorWorkspace {
    pub cwd: std::path::PathBuf,
    pub plugin_data: Option<std::path::PathBuf>,
}

pub async fn doctor_rows(
    registry: &Registry,
    on: &std::collections::BTreeSet<String>,
    mcp: crate::mcp::Service,
    redactions: &crate::session::Redactions,
    workspace: DoctorWorkspace,
) -> Vec<wisp_protocol::DoctorRow> {
    let rows = plugin_doctor_rows(
        registry,
        on,
        mcp,
        redactions,
        crate::workspace::home(),
        workspace,
    )
    .await;
    #[cfg(unix)]
    let rows = {
        let mut rows = rows;
        rows.extend(crate::jobs::doctor_rows());
        rows
    };
    rows
}

#[cfg(any(test, feature = "test-support"))]
pub async fn doctor_rows_with_home(
    registry: &Registry,
    on: &std::collections::BTreeSet<String>,
    mcp: crate::mcp::Service,
    redactions: &crate::session::Redactions,
    home: Option<std::path::PathBuf>,
) -> Vec<wisp_protocol::DoctorRow> {
    plugin_doctor_rows(
        registry,
        on,
        mcp,
        redactions,
        home,
        DoctorWorkspace {
            cwd: std::env::temp_dir(),
            plugin_data: None,
        },
    )
    .await
}

async fn plugin_doctor_rows(
    registry: &Registry,
    on: &std::collections::BTreeSet<String>,
    mcp: crate::mcp::Service,
    redactions: &crate::session::Redactions,
    home: Option<std::path::PathBuf>,
    workspace: DoctorWorkspace,
) -> Vec<wisp_protocol::DoctorRow> {
    crate::session::register(redactions);
    mcp.refresh_keys().await;
    let host = std::sync::Arc::new(KernelHost::new());
    host.set_mcp(mcp);
    host.set_data_dir(workspace.plugin_data);
    let temp = std::env::temp_dir();
    host.bind(
        &crate::jobs::Processes::default(),
        &crate::workspace::Workspace {
            root: workspace.cwd,
            home,
            temp,
        },
    );
    caller::Caller::default()
        .scope(registry.doctor(on, host))
        .await
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstructionsDoctor<'a> {
    pub start: std::path::PathBuf,
    pub home: Option<std::path::PathBuf>,
    pub config_dir: Option<std::path::PathBuf>,
    pub source: &'a wisp_protocol::InstructionsSource,
    pub warn_tokens: u64,
    pub priced: Option<(&'a str, &'a crate::pricing::ModelInfo)>,
}

pub async fn instructions_doctor(
    registry: &Registry,
    ask: InstructionsDoctor<'_>,
) -> Vec<wisp_protocol::DoctorRow> {
    let host = std::sync::Arc::new(KernelHost::new());
    host.bind(
        &crate::jobs::Processes::default(),
        &crate::workspace::Workspace {
            root: ask.start,
            home: ask.home,
            temp: std::env::temp_dir(),
        },
    );
    host.set_config_dir(ask.config_dir.map(|d| d.display().to_string()));
    host.publish(Snapshot {
        settings: vec![
            (
                names::INSTRUCTIONS_SOURCE.to_owned(),
                ask.source.as_str().to_owned(),
            ),
            (
                names::INSTRUCTIONS_WARN_TOKENS.to_owned(),
                ask.warn_tokens.to_string(),
            ),
        ],
        ..Snapshot::default()
    });
    let mut rows = registry
        .doctor_of(host::INSTRUCTIONS_PLUGIN, host.clone())
        .await;
    for warning in host.take_instructed().warnings {
        let worded = crate::instructions::diagnostic(warning, ask.priced);
        rows.push(wisp_protocol::DoctorRow::text("warning", worded.message));
    }
    rows
}

pub fn panicked(id: &str) -> String {
    format!("the {id} plugin panicked")
}
