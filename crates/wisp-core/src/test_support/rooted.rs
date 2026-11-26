use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ClientCommand, InstructionsSource};

use crate::agent::{Agent, AgentTask, Connection, LogContext};
use crate::agents::{TreeSetup, start_root};
use crate::clock::Clock;
use crate::context::ModelSettings;
use crate::kit::{
    BuildTools, Built, Connect, Connected, InstructionsChoice, InstructionsDirs, Kit, KitError,
    OpenLog, OpenedLog, Spec,
};
use crate::provider::Provider;
use crate::session::{MemoryStore, SessionLog};
use crate::settings::host::StartLayers;
use crate::workspace::Workspace;

pub struct Rooted {
    pub kit: Kit,
    pub built: Built,
    pub tree: TreeSetup,
}

impl Rooted {
    pub async fn build(kit: Kit, spec: Spec<'_>, tree: TreeSetup) -> Result<Self, KitError> {
        let built = kit.build(spec).await?;
        Ok(Self { kit, built, tree })
    }

    pub fn start(
        self,
    ) -> (
        mpsc::Sender<ClientCommand>,
        mpsc::Receiver<AgentEvent>,
        AgentTask,
    ) {
        serve(self.kit, self.tree, self.built.agent)
    }
}

pub fn serve(
    kit: Kit,
    tree: TreeSetup,
    agent: Agent,
) -> (
    mpsc::Sender<ClientCommand>,
    mpsc::Receiver<AgentEvent>,
    AgentTask,
) {
    start_root(kit, tree, agent)
}

pub fn tree() -> TreeSetup {
    TreeSetup {
        presets: Default::default(),
        tools_off: Vec::new(),
        followers: Vec::new(),
        limits: Default::default(),
        instructions: InstructionsChoice {
            source: InstructionsSource::None,
            warn_tokens: 8_000,
        },
        loaded: Default::default(),
        keep_warm_main: false,
        restarted: false,
        start: None,
        unusable: Vec::new(),
    }
}

#[derive(Clone)]
pub struct Inputs {
    pub connect: Arc<dyn Connect>,
    pub tools: Arc<dyn BuildTools>,
    pub logs: Arc<dyn OpenLog>,
    pub log_context: LogContext,
    pub workspace: Workspace,
    pub plugin_data: Option<PathBuf>,
    pub clock: Arc<dyn Clock>,
    pub models: ModelSettings,
    pub start_layers: StartLayers,
    pub auto_compact: bool,
    pub user_input: bool,
}

impl Inputs {
    pub fn kit(self) -> Kit {
        let instructions = InstructionsDirs {
            config_dir: None,
            home: self.workspace.home.clone(),
        };
        Kit {
            connect: self.connect,
            tools: self.tools,
            logs: self.logs,
            log_context: self.log_context,
            models: self.models,
            options: Default::default(),
            retry_budget: None,
            count_tokens: false,
            auto_compact: self.auto_compact,
            user_input: self.user_input,
            naming: false,
            workspace: Ok(self.workspace),
            #[cfg(test)]
            pin_test_environment: false,
            instructions,
            plugin_data: self.plugin_data,
            start_layers: self.start_layers,
            settings_host: None,
            endpoint_host: None,
            clock: self.clock,
            plan_usage: Default::default(),
            usage_display: crate::plan_usage::Display::Disabled,
            warmth: Default::default(),
            lanes: Default::default(),
            mcp: Default::default(),
        }
    }
}

pub fn single_provider(provider: Arc<dyn Provider>, context: LogContext) -> Arc<dyn Connect> {
    let active = "fixture".to_owned();
    Arc::new(Connected {
        provider: provider.clone(),
        ready: vec![Connection {
            name: active.clone(),
            provider,
            context,
            priced: false,
        }],
        active,
        unavailable: Vec::new(),
        env_account: None,
    })
}

pub struct Logs {
    pub id: &'static str,
    pub store: MemoryStore,
}

impl OpenLog for Logs {
    fn open(&self, _: &str) -> Result<Option<OpenedLog>, String> {
        Ok(Some(OpenedLog {
            log: SessionLog::new(Box::new(self.store.clone()), self.id),
            path: PathBuf::from(format!("{}.jsonl", self.id)),
            unlocked: None,
            fresh: None,
        }))
    }
}

pub fn spec(choices: crate::kit::Choices) -> Spec<'static> {
    Spec {
        path: crate::kit::ROOT.to_owned(),
        choices,
        instructions: tree().instructions,
        instructions_flag: None,
        extra: None,
        extra_flag: None,
        preset: None,
        start: crate::kit::Start::Fresh,
    }
}
