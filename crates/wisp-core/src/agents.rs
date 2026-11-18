use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use toml::de::{DeTable, DeValue};
use wisp_protocol::agents::{
    Choice, KEPT_TOOLS, Preset, REPORT, SPAWN_AGENT, STOP_AGENT, preset_keys, preset_name_rule,
    shown_preset_name, valid_preset_name,
};
use wisp_protocol::settings::names::{
    AGENTS_MAX_AGENTS, AGENTS_MAX_DEPTH, AGENTS_MAX_RUNNING, AGENTS_MAX_TREE_RUNNING,
    AGENTS_WAKE_WHEN_STUCK, AGENTS_WARM_REFRESHES, EFFORT, ENDPOINT, KEEP_WARM, MODEL,
};
use wisp_protocol::settings::{
    A_STRING, CredentialKind, PRELOAD_CORE, PRELOAD_NONE, SettingValue, TOOL_NAMES, TRUE_OR_FALSE,
    find, is_tool_name, listed, nearest_in, nearest_name, preload_names, quotable,
};
use wisp_protocol::{
    Effort, MODEL_ID_CHARS, check_model_id, model_looks_like_key, shown_model, shown_text,
};

use crate::models::in_order;
use crate::settings::{Current, Layer, Layers, resolve_one};
use endpoints::{EndpointView, plain};

mod brief;
pub mod endpoints;
pub mod fold;
mod guarantee;
mod names;
pub(crate) mod news;
pub mod report;
mod service;
pub(crate) use service::{OnAccount, ROOT_COMMANDS, ROOT_EVENTS};
pub mod state;
#[cfg(test)]
pub(crate) mod testing;

pub use brief::{
    Facts, MAX_REPORT_BYTES, NUDGE, NUDGE_REPORT_ONLY, Spent, cut_report, failed_text,
    invalid_report_text, needs_cut, no_report_text, reported_text, retrying_text, stopped_text,
    unanswered_text, waiting_text,
};
pub(crate) use brief::{fallback_text, path_lines};
pub use guarantee::{EndDecision, EndFacts, Ended, StopCause, ToParent, decide};
pub use names::{RESERVED_NAMES, check_name};
pub(crate) use service::Settled;
pub use service::{
    AgentsHandle, AgentsService, Caller, ChangedPreset, LoadedTree, MAX_PEER_MESSAGES,
    NO_SUCH_AGENT, NOT_FOR_AGENTS, REFUSED, Reach, Spawned, TreeSetup, TurnSignals, load_tree,
    start_root,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_running: Option<u32>,
    pub max_tree_running: Option<u32>,
    pub max_agents: Option<u32>,
    pub max_depth: Option<u32>,
    pub warm_refreshes: u32,
    pub wake_when_stuck: u32,
}

impl Default for Limits {
    fn default() -> Self {
        let current = Current {
            endpoint: None,
            model: String::new(),
        };
        Self::resolve(&Layers::default(), &current)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    pub name: &'static str,
    pub value: Option<u32>,
    pub source: &'static str,
    pub line: Option<usize>,
}

impl Limits {
    pub fn resolve(layers: &Layers<'_>, current: &Current) -> Self {
        let [max_running, max_tree_running, max_agents, max_depth] =
            Self::explained(layers, current);
        Self {
            max_running: max_running.value,
            max_tree_running: max_tree_running.value,
            max_agents: max_agents.value,
            max_depth: max_depth.value,
            wake_when_stuck: limit(AGENTS_WAKE_WHEN_STUCK, layers, current, None)
                .value
                .unwrap_or(0),
            warm_refreshes: limit(AGENTS_WARM_REFRESHES, layers, current, None)
                .value
                .unwrap_or(0),
        }
    }

    pub fn explained(layers: &Layers<'_>, current: &Current) -> [Limit; 4] {
        [
            AGENTS_MAX_RUNNING,
            AGENTS_MAX_TREE_RUNNING,
            AGENTS_MAX_AGENTS,
            AGENTS_MAX_DEPTH,
        ]
        .map(|name| limit(name, layers, current, None))
    }
}

pub(crate) fn limit(
    name: &'static str,
    layers: &Layers<'_>,
    current: &Current,
    credential: Option<&CredentialKind>,
) -> Limit {
    let read = |value: Option<&SettingValue>| match value {
        Some(SettingValue::Int(n)) => u32::try_from(*n).ok().map(Some),
        Some(SettingValue::Text(text)) if text == "none" => Some(None),
        _ => None,
    };
    let Some(spec) = find(name) else {
        return Limit {
            name,
            value: None,
            source: Layer::Default.source(),
            line: None,
        };
    };
    let resolution = resolve_one(spec, layers, current);
    match read(resolution.value()) {
        Some(value) => Limit {
            name,
            value,
            source: resolution.source(),
            line: resolution.winner().and_then(|f| f.line),
        },
        None => Limit {
            name,
            value: read(
                spec.default_for(credential)
                    .or_else(|| spec.default_value())
                    .as_ref(),
            )
            .flatten(),
            source: Layer::Default.source(),
            line: None,
        },
    }
}

pub fn keep_warm_main(layers: &Layers<'_>, current: &Current) -> bool {
    find(KEEP_WARM).is_some_and(|spec| {
        matches!(
            resolve_one(spec, layers, current).value(),
            Some(SettingValue::Bool(true))
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetProblem {
    NotATable,
    BadName,
    UnknownKey {
        key: Option<String>,
        near: Option<&'static str>,
        allowed: Vec<&'static str>,
    },
    Fallback {
        index: usize,
        problem: Box<Self>,
    },
    NoEndpoint,
    NoDescription,
    BadDescription,
    WrongType {
        key: &'static str,
        expected: &'static str,
    },
    BadModel,
    BadEffort,
    ReportRemoved,
    BadInstructions,
    Instructions(String),
    Preload(String),
    UnknownTool {
        tool: Option<String>,
        near: Option<String>,
        names: Vec<String>,
    },
    NotAToolName,
    AgentTool {
        tool: &'static str,
    },
    UnknownModel {
        model: String,
    },
    EffortNotTaken {
        model: String,
        effort: Effort,
    },
    BadEndpoint,
    ModelNotServed {
        model: String,
        endpoint: String,
    },
}

pub(crate) fn agent_tool_text(tool: &str, spawn_off: &str, has: impl Fn(&str) -> bool) -> String {
    if tool == SPAWN_AGENT {
        let also = if has(STOP_AGENT) {
            format!(", which also turns off {STOP_AGENT}")
        } else {
            String::new()
        };
        return format!("tools_off can't name {SPAWN_AGENT}; use {spawn_off}{also}");
    }
    let kept: Vec<&str> = KEPT_TOOLS
        .into_iter()
        .filter(|t| *t == REPORT || has(t))
        .collect();
    format!(
        "tools_off can't name {tool}: every child keeps {}; use {spawn_off} to stop it spawning",
        listed(&kept, "and")
    )
}

impl fmt::Display for PresetProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotATable => f.write_str("a preset must be a table"),
            Self::BadName => write!(f, "a preset's name must be {}", preset_name_rule()),
            Self::UnknownKey { key, near, allowed } => {
                match key {
                    Some(key) => write!(f, "unknown key {key}")?,
                    None => f.write_str("unknown key")?,
                }
                match near {
                    Some(near) => write!(f, "; did you mean {near}?"),
                    None => write!(f, "; allowed: {}", allowed.join(", ")),
                }
            }
            Self::Fallback { index, problem } => write!(f, "fallback {index}: {problem}"),
            Self::NoEndpoint => f.write_str("endpoint is required"),
            Self::NoDescription => f.write_str("a preset needs a description"),
            Self::BadDescription => f.write_str("description must be one line of text"),
            Self::WrongType { key, expected } => write!(f, "{key} must be {expected}"),
            Self::BadModel => {
                write!(f, "model must be a model ID ({MODEL_ID_CHARS})")
            }
            Self::BadEffort => {
                write!(f, "effort must be {}", listed(&Effort::WORDS, "or"))
            }
            Self::ReportRemoved => f.write_str(
                "report was removed: reports are free text now; shape them in the preset's instructions",
            ),
            Self::BadInstructions => {
                f.write_str("instructions must name a file or a directory holding AGENTS.md")
            }
            Self::Instructions(text) => f.write_str(text),
            Self::Preload(text) => write!(f, "preload: {text}"),
            Self::UnknownTool { tool, near, names } => {
                match tool {
                    Some(tool) => write!(f, "tools_off names {tool}, which is no tool")?,
                    None => f.write_str("tools_off names something that is no tool")?,
                }
                match near {
                    Some(near) => write!(f, "; did you mean {near}?"),
                    None => write!(f, "; tools: {}", names.join(", ")),
                }
            }
            Self::NotAToolName => write!(f, "tools_off must be {TOOL_NAMES}"),
            Self::AgentTool { tool } => {
                f.write_str(&agent_tool_text(tool, "spawn = false", |_| true))
            }
            Self::UnknownModel { model } => write!(
                f,
                "model {} isn't in the capability table",
                shown_model(model)
            ),
            Self::EffortNotTaken { model, effort } => write!(
                f,
                "model {} doesn't take effort {}",
                shown_model(model),
                effort.as_str()
            ),
            Self::BadEndpoint => write!(
                f,
                "endpoint must be an account's name ({})",
                wisp_protocol::endpoint_name_rule()
            ),
            Self::ModelNotServed { model, endpoint } => write!(
                f,
                "model {} isn't one account {} serves",
                shown_model(model),
                plain(endpoint)
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetOff {
    pub name: Option<String>,
    pub line: usize,
    pub problem: PresetProblem,
}

impl PresetOff {
    pub fn shown_name(&self) -> &str {
        self.name
            .as_deref()
            .unwrap_or("(not a preset name, not shown)")
    }
}

impl fmt::Display for PresetOff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "preset {} (line {}) is off: {}",
            self.shown_name(),
            self.line,
            self.problem
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetInstructions {
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub large: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetPlace {
    pub config_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub warn_tokens: u64,
}

impl PresetPlace {
    pub fn of_config(config: Option<&Path>, home: Option<PathBuf>, warn_tokens: u64) -> Self {
        Self {
            config_dir: config
                .and_then(|c| std::path::absolute(c).ok())
                .and_then(|c| c.parent().map(Path::to_owned)),
            home,
            warn_tokens,
        }
    }
}

fn check_instructions(
    preset: &Preset,
    place: &PresetPlace,
) -> Result<Option<PresetInstructions>, PresetProblem> {
    let Some(written) = &preset.instructions else {
        return Ok(None);
    };
    let hidden = |_| {
        PresetProblem::Instructions(format!(
            "instructions {} can't be loaded",
            shown_text(written)
        ))
    };
    let worded = |text: String| {
        if model_looks_like_key(written) {
            hidden(text)
        } else {
            PresetProblem::Instructions(text)
        }
    };
    let home = place.home.as_deref();
    let path = crate::instructions::resolve_preset(written, place.config_dir.as_deref(), home)
        .map_err(worded)?;
    let files = crate::instructions::load_preset(
        &path,
        home,
        shown_preset_name(&preset.name).unwrap_or("(not shown)"),
        place.warn_tokens,
        None,
        crate::instructions::Holder::Child,
    )
    .map_err(worded)?;
    Ok(Some(PresetInstructions {
        files: files.sources.len(),
        bytes: files.bytes(),
        large: files.large.map(|d| d.message),
        path,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointNote {
    Unavailable(String),
    NotChecked,
    NotSetUp,
}

#[derive(Clone, PartialEq, Eq)]
struct Listed {
    preset: Preset,
    line: usize,
    instructions: Option<PresetInstructions>,
    note: Option<EndpointNote>,
    fallback_notes: Vec<Option<EndpointNote>>,
}

impl fmt::Debug for Listed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Listed")
            .field("name", &shown_preset_name(&self.preset.name))
            .field("line", &self.line)
            .field("instructions", &self.instructions)
            .field("note", &self.note)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presets {
    listed: Vec<Listed>,
    off: Vec<PresetOff>,
}

impl Presets {
    pub fn parse(table: &DeTable<'_>, line: &dyn Fn(Range<usize>) -> usize) -> Self {
        let mut out = Self::default();
        for (key, value) in table.iter() {
            let name: &str = key.get_ref();
            let at = line(key.span());
            match preset(name, value.get_ref()) {
                Ok(preset) => out.listed.push(Listed {
                    preset,
                    line: at,
                    instructions: None,
                    note: None,
                    fallback_notes: Vec::new(),
                }),
                Err(problem) => out.off.push(PresetOff {
                    name: shown_preset_name(name).map(str::to_owned),
                    line: at,
                    problem,
                }),
            }
        }
        out.sort();
        out
    }

    pub fn rooted(mut self, folder: &std::path::Path) -> Self {
        for listed in &mut self.listed {
            if let Some(file) = listed.preset.instructions.as_mut() {
                *file = folder.join(&*file).display().to_string();
            }
        }
        self
    }

    pub fn joined(mut self, over: Presets) -> Self {
        let named = |name: &str| over.listed.iter().any(|o| o.preset.name == name);
        self.listed.retain(|l| !named(&l.preset.name));
        self.listed.extend(over.listed);
        self.off.extend(over.off);
        self.sort();
        self
    }

    fn sort(&mut self) {
        self.listed
            .sort_by(|a, b| a.preset.name.cmp(&b.preset.name));
        self.off
            .sort_by(|a, b| (&a.name, a.line).cmp(&(&b.name, b.line)));
    }

    pub fn checked(
        self,
        view: Option<&EndpointView>,
        tool_names: &[String],
        place: &PresetPlace,
    ) -> CheckedPresets {
        let mut out = Self {
            listed: Vec::new(),
            off: self.off,
        };
        for mut listed in self.listed {
            let tool_problem = listed
                .preset
                .tools_off
                .iter()
                .find_map(|tool| tool_off(tool, tool_names).err());
            let tool_problem = tool_problem.or_else(|| {
                let mut leaders: Vec<String> = tool_names
                    .iter()
                    .filter(|name| name.as_str() != STOP_AGENT)
                    .cloned()
                    .chain([SPAWN_AGENT.to_owned()])
                    .collect();
                leaders.sort();
                leaders.dedup();
                listed
                    .preset
                    .preload
                    .iter()
                    .flatten()
                    .filter(|name| name.as_str() != PRELOAD_CORE && name.as_str() != PRELOAD_NONE)
                    .find_map(|name| named_tool(name, &leaders).err())
                    .map(|error| {
                        PresetProblem::Preload(error.to_string().replace("tools_off", "tool list"))
                    })
            });
            let reached = endpoint_check(
                listed.preset.endpoint.as_deref(),
                listed.preset.model.as_deref(),
                listed.preset.effort,
                view,
            );
            let fallbacks = listed
                .preset
                .fallback
                .iter()
                .enumerate()
                .map(|(index, choice)| {
                    endpoint_check(
                        Some(&choice.endpoint),
                        choice.model.as_deref(),
                        choice.effort,
                        view,
                    )
                    .map_err(|problem| PresetProblem::Fallback {
                        index: index + 1,
                        problem: Box::new(problem),
                    })
                })
                .collect::<Result<Vec<_>, _>>();
            let problem = match tool_problem
                .or_else(|| reached.clone().err())
                .or_else(|| fallbacks.as_ref().err().cloned())
            {
                Some(problem) => Some(problem),
                None => match check_instructions(&listed.preset, place) {
                    Ok(instructions) => {
                        listed.instructions = instructions;
                        listed.note = reached.ok().flatten();
                        listed.fallback_notes = fallbacks.unwrap_or_default();
                        None
                    }
                    Err(problem) => Some(problem),
                },
            };
            match problem {
                None => out.listed.push(listed),
                Some(problem) => out.off.push(PresetOff {
                    name: shown_preset_name(&listed.preset.name).map(str::to_owned),
                    line: listed.line,
                    problem,
                }),
            }
        }
        out.sort();
        CheckedPresets(out)
    }

    pub fn usable(&self) -> Vec<&Preset> {
        self.listed.iter().map(|l| &l.preset).collect()
    }

    pub fn off(&self) -> &[PresetOff] {
        &self.off
    }

    pub fn is_empty(&self) -> bool {
        self.listed.is_empty() && self.off.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckedPresets(Presets);

impl CheckedPresets {
    pub fn usable(&self) -> Vec<&Preset> {
        self.0.usable()
    }

    pub fn find(&self, name: &str) -> Option<&Preset> {
        self.0
            .listed
            .iter()
            .map(|l| &l.preset)
            .find(|p| p.name == name)
    }

    pub fn worker(&self, name: Option<&str>) -> bool {
        name.and_then(|name| self.find(name))
            .and_then(|p| p.worker)
            .unwrap_or(true)
    }

    pub fn named(&self, name: &str) -> Result<&Preset, String> {
        if let Some(preset) = self.find(name) {
            return Ok(preset);
        }
        if let Some(off) = self.off_named(name) {
            return Err(off.to_string());
        }
        let usable: Vec<&str> = self.usable().iter().map(|p| p.name.as_str()).collect();
        Err(if usable.is_empty() {
            format!("there is no preset {name:?}; no presets are set")
        } else {
            format!(
                "there is no preset {name:?}; presets: {}",
                usable.join(", ")
            )
        })
    }

    pub fn instructions(&self, name: &str) -> Option<&PresetInstructions> {
        self.0
            .listed
            .iter()
            .find(|l| l.preset.name == name)
            .and_then(|l| l.instructions.as_ref())
    }

    pub fn note(&self, name: &str) -> Option<&EndpointNote> {
        self.0
            .listed
            .iter()
            .find(|l| l.preset.name == name)
            .and_then(|l| l.note.as_ref())
    }

    pub fn fallback_notes(&self, name: &str) -> &[Option<EndpointNote>] {
        self.0
            .listed
            .iter()
            .find(|listed| listed.preset.name == name)
            .map_or(&[], |listed| &listed.fallback_notes)
    }

    pub fn off_named(&self, name: &str) -> Option<&PresetOff> {
        self.off().iter().find(|o| o.name.as_deref() == Some(name))
    }

    pub fn off(&self) -> &[PresetOff] {
        self.0.off()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn warnings(&self, spawns: bool, own: Option<&str>) -> Vec<String> {
        let usable = |name: Option<&str>| spawns || name.is_some_and(|name| Some(name) == own);
        let listed = || {
            self.0
                .listed
                .iter()
                .filter(move |l| usable(Some(&l.preset.name)))
        };
        let large = listed()
            .filter(|l| Some(l.preset.name.as_str()) == own)
            .filter_map(|l| l.instructions.as_ref()?.large.clone());
        let unavailable = listed().filter(|listed| listed.preset.fallback.is_empty()).filter_map(|l| {
            let Some(EndpointNote::Unavailable(reason)) = &l.note else {
                return None;
            };
            let endpoint = l.preset.endpoint.as_deref()?;
            Some(format!(
                "preset {}'s account {} is unavailable: {reason}; its spawns are refused until the account becomes available",
                l.preset.name,
                plain(endpoint)
            ))
        });
        self.off()
            .iter()
            .filter(|off| usable(off.name.as_deref()))
            .map(ToString::to_string)
            .chain(unavailable)
            .chain(large)
            .collect()
    }
}

fn endpoint_check(
    endpoint: Option<&str>,
    model: Option<&str>,
    effort: Option<Effort>,
    view: Option<&EndpointView>,
) -> Result<Option<EndpointNote>, PresetProblem> {
    if endpoint.is_none() && model.is_none() {
        return Ok(None);
    }
    let Some(view) = view else {
        return Ok(Some(EndpointNote::NotChecked));
    };
    let ready = match endpoint {
        Some(name) => match view.ready(name) {
            Some(ready) => Some(ready),
            None => match view.unavailable(name) {
                Some(reason) => return Ok(Some(EndpointNote::Unavailable(reason.to_owned()))),
                None => return Ok(Some(EndpointNote::NotSetUp)),
            },
        },
        None => None,
    };
    let Some(model) = model else {
        return Ok(None);
    };
    let row = match ready {
        Some(ready) => ready.row(model),
        None => view.serving(model).and_then(|r| r.row(model)),
    };
    let Some(row) = row else {
        let discoverable = ready.map_or_else(
            || view.can_discover(model),
            |ready| ready.can_discover(model),
        );
        return if crate::models::ModelTable::shared().find(model).is_some() || discoverable {
            Ok(Some(EndpointNote::NotChecked))
        } else {
            Err(match endpoint {
                Some(endpoint) => PresetProblem::ModelNotServed {
                    model: model.to_owned(),
                    endpoint: endpoint.to_owned(),
                },
                None => PresetProblem::UnknownModel {
                    model: model.to_owned(),
                },
            })
        };
    };
    match effort {
        Some(effort) if !row.accepts_effort(Some(effort), effort) => {
            Err(PresetProblem::EffortNotTaken {
                model: model.to_owned(),
                effort,
            })
        }
        _ => Ok(None),
    }
}

fn text<'v>(key: &'static str, value: &'v DeValue<'_>) -> Result<&'v str, PresetProblem> {
    value.as_str().ok_or(PresetProblem::WrongType {
        key,
        expected: A_STRING,
    })
}

fn spelled(tool: &str) -> Result<String, PresetProblem> {
    if let Some(kept) = KEPT_TOOLS
        .into_iter()
        .chain([SPAWN_AGENT])
        .find(|t| *t == tool)
    {
        return Err(PresetProblem::AgentTool { tool: kept });
    }
    if is_tool_name(tool) {
        Ok(tool.to_owned())
    } else {
        Err(PresetProblem::NotAToolName)
    }
}

pub fn tool_off(tool: &str, names: &[String]) -> Result<String, PresetProblem> {
    let tool = match spelled(tool) {
        Err(PresetProblem::NotAToolName) => {
            return Err(PresetProblem::UnknownTool {
                tool: None,
                near: None,
                names: names.to_vec(),
            });
        }
        spelled => spelled?,
    };
    named_tool(&tool, names)
}

fn named_tool(tool: &str, names: &[String]) -> Result<String, PresetProblem> {
    if names.iter().any(|name| name == tool) {
        return Ok(tool.to_owned());
    }
    let shown = wisp_protocol::plugin::shows_tool_name(tool);
    Err(PresetProblem::UnknownTool {
        tool: shown.then(|| tool.to_owned()),
        near: shown.then(|| nearest_name(tool, names)).flatten(),
        names: names.to_vec(),
    })
}

fn tools_off(value: &DeValue<'_>) -> Result<Vec<String>, PresetProblem> {
    let wrong = PresetProblem::WrongType {
        key: "tools_off",
        expected: TOOL_NAMES,
    };
    let Some(items) = value.as_array() else {
        return Err(wrong);
    };
    let mut tools = Vec::new();
    for item in items.iter() {
        let tool = item.get_ref().as_str().ok_or_else(|| wrong.clone())?;
        tools.push(spelled(tool)?);
    }
    tools.sort();
    tools.dedup();
    Ok(tools)
}

fn choice_key(
    key: &str,
    value: &DeValue<'_>,
    endpoint: &mut Option<String>,
    model: &mut Option<String>,
    effort: &mut Option<Effort>,
) -> Result<bool, PresetProblem> {
    match key {
        MODEL => {
            let given = text(MODEL, value)?;
            check_model_id(given).map_err(|_| PresetProblem::BadModel)?;
            *model = Some(given.to_owned());
        }
        EFFORT => {
            *effort = Some(
                text(EFFORT, value)?
                    .parse()
                    .map_err(|_| PresetProblem::BadEffort)?,
            )
        }
        ENDPOINT => {
            let given = text(ENDPOINT, value)?;
            if crate::endpoints::EndpointName::parse(given).is_none() {
                return Err(PresetProblem::BadEndpoint);
            }
            *endpoint = Some(given.to_owned());
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn fallback_choices(value: &DeValue<'_>) -> Result<Vec<Choice>, PresetProblem> {
    let entries = value.as_array().ok_or(PresetProblem::WrongType {
        key: "fallback",
        expected: "a list of tables",
    })?;
    entries
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let read = || {
                let table = value.get_ref().as_table().ok_or(PresetProblem::WrongType {
                    key: "fallback entry",
                    expected: "a table",
                })?;
                let (mut endpoint, mut model, mut effort) = (None, None, None);
                for (key, value) in in_order(table) {
                    let key: &str = key.get_ref();
                    if !choice_key(key, value.get_ref(), &mut endpoint, &mut model, &mut effort)? {
                        let shown = quotable(key);
                        let allowed = vec![ENDPOINT, MODEL, EFFORT];
                        return Err(PresetProblem::UnknownKey {
                            key: shown.then(|| key.to_owned()),
                            near: shown.then(|| nearest_in(key, &allowed)).flatten(),
                            allowed,
                        });
                    }
                }
                Ok(Choice {
                    endpoint: endpoint.ok_or(PresetProblem::NoEndpoint)?,
                    model,
                    effort,
                })
            };
            read().map_err(|problem| PresetProblem::Fallback {
                index: index + 1,
                problem: Box::new(problem),
            })
        })
        .collect()
}

fn preset(name: &str, value: &DeValue<'_>) -> Result<Preset, PresetProblem> {
    if !valid_preset_name(name) {
        return Err(PresetProblem::BadName);
    }
    let Some(table) = value.as_table() else {
        return Err(PresetProblem::NotATable);
    };
    let mut out = Preset {
        name: name.to_owned(),
        description: String::new(),
        model: None,
        effort: None,
        tools_off: Vec::new(),
        preload: None,
        spawn: None,
        instructions: None,
        endpoint: None,
        fallback: Vec::new(),
        worker: None,
    };
    for (key, value) in in_order(table) {
        let key: &str = key.get_ref();
        let value = value.get_ref();
        if choice_key(
            key,
            value,
            &mut out.endpoint,
            &mut out.model,
            &mut out.effort,
        )? {
            continue;
        }
        match key {
            "description" => {
                let given = text("description", value)?.trim();
                if given.is_empty() {
                    return Err(PresetProblem::NoDescription);
                }
                if given.chars().any(char::is_control) {
                    return Err(PresetProblem::BadDescription);
                }
                given.clone_into(&mut out.description);
            }
            "fallback" => out.fallback = fallback_choices(value)?,
            "report" => return Err(PresetProblem::ReportRemoved),
            "instructions" => {
                let given = text("instructions", value)?.trim();
                if given.is_empty() || given.chars().any(char::is_control) {
                    return Err(PresetProblem::BadInstructions);
                }
                out.instructions = Some(given.to_owned());
            }
            "spawn" => {
                out.spawn = Some(value.as_bool().ok_or(PresetProblem::WrongType {
                    key: "spawn",
                    expected: TRUE_OR_FALSE,
                })?);
            }
            "tools_off" => out.tools_off = tools_off(value)?,
            "preload" => {
                let names = crate::config::strings(value).ok_or_else(|| {
                    PresetProblem::Preload(format!("tool list must be {TOOL_NAMES}"))
                })?;
                out.preload = Some(
                    preload_names(&names).map_err(|error| PresetProblem::Preload(error.rule))?,
                );
            }
            "worker" => {
                out.worker = Some(value.as_bool().ok_or(PresetProblem::WrongType {
                    key: "worker",
                    expected: TRUE_OR_FALSE,
                })?);
            }
            other => {
                let shown = quotable(other);
                return Err(PresetProblem::UnknownKey {
                    key: shown.then(|| other.to_owned()),
                    near: shown.then(|| nearest_in(other, &preset_keys())).flatten(),
                    allowed: preset_keys(),
                });
            }
        }
    }
    if out.description.is_empty() {
        return Err(PresetProblem::NoDescription);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
