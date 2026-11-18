use wasmtime::component::InstancePre;
use wasmtime::{Engine, Store};
use wisp_plugin_api::manifest::at_least;

use crate::bindings::exports::wisp::{plugin0_1_0 as x1, plugin0_2_0 as x2, plugin0_3_0 as x3};
use crate::state::State;

const NEWER: &str = "0.3.0";

#[derive(Clone)]
enum ToolsIndices {
    Older(x1::tools::GuestIndices),
    Newer(x3::tools::GuestIndices),
}

pub(crate) enum Tools {
    Older(x1::tools::Guest),
    Newer(x3::tools::Guest),
}

#[derive(Clone)]
enum SlotsIndices {
    Older(x2::slot_render::GuestIndices),
    Newer(x3::slot_render::GuestIndices),
}

pub(crate) enum Slots {
    Older(x2::slot_render::Guest),
    Newer(x3::slot_render::Guest),
}

#[derive(Clone)]
enum CommandsIndices {
    Older(x1::commands::GuestIndices),
    Newer(x3::commands::GuestIndices),
}

pub(crate) enum Commands {
    Older(x1::commands::Guest),
    Newer(x3::commands::Guest),
}

#[derive(Clone)]
pub(crate) struct Pre {
    instance: InstancePre<State>,
    tools: ToolsIndices,
    commands: CommandsIndices,
    hooks: x1::hooks::GuestIndices,
    slots: SlotsIndices,
    index_source: Option<x3::index_source::GuestIndices>,
    prompt_sections: x2::prompt_sections::GuestIndices,
    agent_hooks: x2::agent_hooks::GuestIndices,
    strategy: x2::strategy::GuestIndices,
}

pub(crate) struct Exports {
    pub(crate) tools: Tools,
    pub(crate) commands: Commands,
    pub(crate) hooks: x1::hooks::Guest,
    pub(crate) slots: Slots,
    pub(crate) index_source: Option<x3::index_source::Guest>,
    pub(crate) prompt_sections: x2::prompt_sections::Guest,
    pub(crate) agent_hooks: x2::agent_hooks::Guest,
    pub(crate) strategy: x2::strategy::Guest,
}

impl Pre {
    pub(crate) fn new(instance: InstancePre<State>, world: &str) -> wasmtime::Result<Self> {
        let newer = at_least(world, NEWER);
        Ok(Self {
            slots: if newer {
                SlotsIndices::Newer(x3::slot_render::GuestIndices::new(&instance)?)
            } else {
                SlotsIndices::Older(x2::slot_render::GuestIndices::new(&instance)?)
            },
            tools: if newer {
                ToolsIndices::Newer(x3::tools::GuestIndices::new(&instance)?)
            } else {
                ToolsIndices::Older(x1::tools::GuestIndices::new(&instance)?)
            },
            commands: if newer {
                CommandsIndices::Newer(x3::commands::GuestIndices::new(&instance)?)
            } else {
                CommandsIndices::Older(x1::commands::GuestIndices::new(&instance)?)
            },
            index_source: if newer {
                Some(x3::index_source::GuestIndices::new(&instance)?)
            } else {
                None
            },
            prompt_sections: x2::prompt_sections::GuestIndices::new(&instance)?,
            agent_hooks: x2::agent_hooks::GuestIndices::new(&instance)?,
            strategy: x2::strategy::GuestIndices::new(&instance)?,
            hooks: x1::hooks::GuestIndices::new(&instance)?,
            instance,
        })
    }

    pub(crate) fn engine(&self) -> &Engine {
        self.instance.engine()
    }

    pub(crate) async fn instantiate(&self, store: &mut Store<State>) -> wasmtime::Result<Exports> {
        let instance = self.instance.instantiate_async(&mut *store).await?;
        Ok(Exports {
            slots: match &self.slots {
                SlotsIndices::Older(indices) => Slots::Older(indices.load(&mut *store, &instance)?),
                SlotsIndices::Newer(indices) => Slots::Newer(indices.load(&mut *store, &instance)?),
            },
            tools: match &self.tools {
                ToolsIndices::Older(indices) => Tools::Older(indices.load(&mut *store, &instance)?),
                ToolsIndices::Newer(indices) => Tools::Newer(indices.load(&mut *store, &instance)?),
            },
            commands: match &self.commands {
                CommandsIndices::Older(indices) => {
                    Commands::Older(indices.load(&mut *store, &instance)?)
                }
                CommandsIndices::Newer(indices) => {
                    Commands::Newer(indices.load(&mut *store, &instance)?)
                }
            },
            index_source: self
                .index_source
                .as_ref()
                .map(|indices| indices.load(&mut *store, &instance))
                .transpose()?,
            prompt_sections: self.prompt_sections.load(&mut *store, &instance)?,
            agent_hooks: self.agent_hooks.load(&mut *store, &instance)?,
            strategy: self.strategy.load(&mut *store, &instance)?,
            hooks: self.hooks.load(&mut *store, &instance)?,
        })
    }
}

macro_rules! with_tools {
    ($exports:expr, |$guest:ident| $call:expr) => {
        match $exports {
            $crate::exports::Tools::Older($guest) => $call,
            $crate::exports::Tools::Newer($guest) => $call,
        }
    };
}

macro_rules! with_slots {
    ($exports:expr, |$guest:ident| $call:expr) => {
        match $exports {
            $crate::exports::Slots::Older($guest) => $call,
            $crate::exports::Slots::Newer($guest) => $call,
        }
    };
}

macro_rules! with_commands {
    ($exports:expr, |$guest:ident| $call:expr) => {
        match $exports {
            $crate::exports::Commands::Older($guest) => $call,
            $crate::exports::Commands::Newer($guest) => $call,
        }
    };
}

pub(crate) use {with_commands, with_slots, with_tools};
