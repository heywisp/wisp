use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use wisp_plugin_api::host::{CallContext, Host, SpawnInput};
use wisp_plugin_api::prompt::{At, Prior, Rendered};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::tree::{AgentNode, AgentState, IdleOutcome, QueueReason, ShownState};
use wisp_plugin_api::types::{Action, Completed, Display, Outcome, OutputKind, ToolRun, Verbs};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::agents::{
    LIST_AGENTS, MESSAGES_SENTENCE, QUEUED_SENTENCE, RENAME_AGENT, REPORT, REPORT_SENTENCE,
    REPORTED, SEND_MESSAGE, SPAWN_AGENT, STOP_AGENT, WAKE, kept_warm_line, listed, renamed,
    report_line, sent, started, stopped,
};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const ID: &str = "agents";

pub const KEPT_WARM: &str = "kept-warm";

pub const NAMES: [&str; 6] = [
    LIST_AGENTS,
    RENAME_AGENT,
    REPORT,
    SEND_MESSAGE,
    SPAWN_AGENT,
    STOP_AGENT,
];

const SPAWN_LIMIT_MS: u64 = 10_000;
const SPAWN_MAX_MS: u64 = 60_000;
const QUICK_MS: u64 = 5_000;

const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

const SPAWN_DESCRIPTION: &str = "Start a named child agent on a task. It works in the background with its own fresh context and reports back when it is done; the call returns at once. Use it for work that can go on by itself while you do something else: an independent investigation, a review of a change it didn't write, or a self-contained change. Do small or tightly linked steps yourself, and don't let two agents edit the same files. The child sees only its task, the project's instructions and what it finds itself, never your conversation, so write the task as a complete brief: the goal, the facts and files that matter, what to leave alone and what to hand back. It has your endpoint, model, effort and tools by default, except that it can't ask the user (it asks you instead); endpoint, model and effort may change that, tools_off and spawn only narrow it, and preset names a set of those choices, which may include instructions for the child's role. Its report arrives later as a message from it: don't poll it, wait for it or redo its work, but carry on with other work or end your reply. To run several children at once, start them in the same reply.";

const LIST_DESCRIPTION: &str = "List the agents you can reach: your parent, your siblings and every agent below you, each with its path, preset, model and effort, state, spend, checklist progress, working directory and the first line of its last report. Use it to find an agent's path or check how the work is spread, for example after older turns were summarised. Don't call it to wait for a child: its report arrives by itself.";

const REPORT_DESCRIPTION: &str = "Hand your result to the agent that gave you your task; it ends your turn. Call it when the task is done, or when you are blocked or have failed, saying why. Your parent may pass it on to the user, so use plain words and make each decision or question in it stand alone: what is decided, the facts and the options, never pointing to \"above\". Write it as your task and your instructions ask; its first line is what lists of agents show, so lead with the outcome.";

const SEND_DESCRIPTION: &str = "Send a message to another agent: your parent (`..`), one of your children (its name), a sibling (`../name`) or any agent below you (its path, as list_agents shows it). You can't message your parent's parent or a sibling's children: ask your parent to pass it on. The recipient sees only this text, never your conversation, so make it complete. A running recipient reads it at its next step; an idle one is woken to read it, unless wake is false, when it waits until the recipient next runs. A message with wake on to your own child is a new task, and the child reports back when it is done. To ask your parent or a sibling something, send the question and end your reply: the answer arrives as a message, so don't poll for it. Siblings can exchange only a limited number of messages before their parent writes to them again. Hand in your own result with report, not with a message.";

pub const NAME_RULE: &str = "a lowercase letter, then up to 31 of a-z, 0-9, _ and -";

const REACH: &str = "one of your children by its name, or a deeper agent by its path";

const BELOW_NAME: &str = "A child's name, or the path of an agent further below you.";

const STOP_EFFECT: &str = "Its running turn ends at once, and so does the turn of every agent below it; they stay idle with their context, and a message wakes them again. Each agent stopped below it tells its own parent. Set end to true once you no longer need it: it and every agent below it are retired, take no more messages, stop counting toward the limit on live agents and have their background processes stopped. Don't stop a child to wait for it, and don't end one whose report you still need.";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(AgentTools { host })
}

fn strings(description: &str) -> Value {
    json!({"type": "array", "description": description, "items": {"type": "string"}})
}

pub fn spawn_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": format!("The child's name, unique among your children for the whole session: {NAME_RULE}. Its path is yours plus this name.")
            },
            "task": {
                "type": "string",
                "description": "The brief: everything the child needs to do the task without your conversation, and what it should hand back."
            },
            "todos": strings("Optional steps of the task, in order; they become the child's starting checklist."),
            "cwd": {
                "type": "string",
                "description": "The child's working directory: absolute, under ~/ or relative to yours. Yours when left out."
            },
            "preset": {
                "type": "string",
                "description": "A preset listed in your <environment>; this call's own fields override its choices."
            },
            "endpoint": {
                "type": "string",
                "description": "One of the session's endpoints for the child, by name. Set it only when the user asks for a particular endpoint or provider. Leave it out to use the preset's, or else yours."
            },
            "model": {
                "type": "string",
                "description": "Another model for the child, one its endpoint serves. Leave it out to use the preset's, or else yours."
            },
            "effort": {
                "type": "string",
                "enum": EFFORTS,
                "description": "The child's effort. Leave it out to use the preset's, or else yours."
            },
            "tools_off": strings("Names of your tools the child shouldn't have."),
            "spawn": {
                "type": "boolean",
                "description": "false stops the child from starting agents of its own."
            },
            "keep_warm": {
                "type": "boolean",
                "description": "true if you will message it again after its report; while it idles, Wisp refreshes its prompt cache a limited number of times. End it when you won't."
            }
        },
        "required": ["name", "task"],
        "additionalProperties": false
    })
}

pub fn list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

pub fn send_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": {
                "type": "string",
                "description": "The recipient: `..` for your parent, a child's name, `../name` for a sibling, or the path of an agent below you."
            },
            "text": {
                "type": "string",
                "description": "The message, complete in itself."
            },
            "wake": {
                "type": "boolean",
                "description": "false lets an idle recipient read it when it next runs instead of waking it. true when left out."
            }
        },
        "required": ["to", "text"],
        "additionalProperties": false
    })
}

pub fn stop_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": BELOW_NAME
            },
            "end": {
                "type": "boolean",
                "description": "true also retires it and every agent below it for good. false when left out."
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

pub fn rename_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": BELOW_NAME
            },
            "new_name": {
                "type": "string",
                "description": format!("Its new name: {NAME_RULE}.")
            }
        },
        "required": ["name", "new_name"],
        "additionalProperties": false
    })
}

pub fn report_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {
                "type": "string",
                "description": "The whole report, in the form your task or instructions ask for."
            }
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

pub fn meta(tool: &str) -> ToolMeta {
    let (effect, timeouts, writing) = match tool {
        SPAWN_AGENT => (
            Effect::NonIdempotent,
            Timeouts {
                default_ms: SPAWN_LIMIT_MS,
                max_ms: Some(SPAWN_MAX_MS),
            },
            Some("a task for an agent".to_owned()),
        ),
        REPORT => (
            Effect::NonIdempotent,
            Timeouts {
                default_ms: QUICK_MS,
                max_ms: Some(QUICK_MS),
            },
            Some("a report".to_owned()),
        ),
        SEND_MESSAGE => (
            Effect::NonIdempotent,
            Timeouts {
                default_ms: QUICK_MS,
                max_ms: Some(QUICK_MS),
            },
            Some("a message".to_owned()),
        ),
        STOP_AGENT | RENAME_AGENT => (
            Effect::NonIdempotent,
            Timeouts {
                default_ms: QUICK_MS,
                max_ms: Some(QUICK_MS),
            },
            None,
        ),
        _ => (
            Effect::ReadOnly,
            Timeouts {
                default_ms: QUICK_MS,
                max_ms: Some(QUICK_MS),
            },
            None,
        ),
    };
    ToolMeta {
        effect,
        tags: Tags::default(),
        parallel_safe: false,
        timeouts,
        saves_output: false,
        needs_workspace: false,
        ends_reply: false,
        asks_user: false,
        reports_jobs: false,
        writing,
        purpose: Purpose::Run,
    }
}

fn def(name: &str, description: &str, schema: &Value, system_line: &str) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description.to_owned(),
            input_schema: schema.to_string(),
            meta: meta(name),
        }),
        system_line: Some(system_line.to_owned()),
        context_line: None,
    }
}

pub fn defs() -> Vec<ToolDef> {
    vec![
        def(
            LIST_AGENTS,
            LIST_DESCRIPTION,
            &list_schema(),
            MESSAGES_SENTENCE,
        ),
        def(
            RENAME_AGENT,
            &format!(
                "Rename an agent below you when the user asks you to: {REACH}. It keeps running and its own agents move with it. Its old name still reaches it and is never given to another agent."
            ),
            &rename_schema(),
            MESSAGES_SENTENCE,
        ),
        def(
            REPORT,
            REPORT_DESCRIPTION,
            &report_schema(),
            REPORT_SENTENCE,
        ),
        def(
            SEND_MESSAGE,
            SEND_DESCRIPTION,
            &send_schema(),
            MESSAGES_SENTENCE,
        ),
        def(
            SPAWN_AGENT,
            SPAWN_DESCRIPTION,
            &spawn_schema(),
            MESSAGES_SENTENCE,
        ),
        def(
            STOP_AGENT,
            &format!("Stop an agent below you: {REACH}. {STOP_EFFECT}"),
            &stop_schema(),
            MESSAGES_SENTENCE,
        ),
    ]
}

fn object(input: &str) -> Result<Map<String, Value>, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err("input must be an object".to_owned()),
    }
}

fn only_fields(map: &Map<String, Value>, known: &[&str]) -> Result<(), String> {
    match map.keys().find(|k| !known.contains(&k.as_str())) {
        Some(key) => Err(format!("unknown field `{key}`")),
        None => Ok(()),
    }
}

fn text(map: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn required(map: &Map<String, Value>, key: &str) -> Result<String, String> {
    text(map, key)?.ok_or_else(|| format!("`{key}` is required"))
}

fn list(map: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("every item of `{key}` must be a string"))
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be an array of strings")),
    }
}

fn flag(map: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("`{key}` must be true or false")),
    }
}

const SPAWN_FIELDS: [&str; 11] = [
    "cwd",
    "effort",
    "endpoint",
    "keep_warm",
    "model",
    "name",
    "preset",
    "spawn",
    "task",
    "todos",
    "tools_off",
];

pub fn parse_spawn(input: &str) -> Result<SpawnInput, String> {
    let map = object(input)?;
    only_fields(&map, &SPAWN_FIELDS)?;
    Ok(SpawnInput {
        name: required(&map, "name")?,
        task: required(&map, "task")?,
        todos: list(&map, "todos")?,
        cwd: text(&map, "cwd")?,
        preset: text(&map, "preset")?,
        endpoint: text(&map, "endpoint")?,
        model: text(&map, "model")?,
        effort: text(&map, "effort")?,
        tools_off: list(&map, "tools_off")?,
        spawn: flag(&map, "spawn")?,
        keep_warm: flag(&map, "keep_warm")?,
    })
}

enum Call {
    Spawn(SpawnInput),
    List,
    Report(String),
    Send {
        to: String,
        text: String,
        wake: bool,
    },
    Stop {
        name: String,
        end: bool,
    },
    Rename {
        name: String,
        new_name: String,
    },
}

struct Asked {
    display: Display,
    call: Call,
}

impl Prepared for Asked {
    fn display(&self) -> Display {
        self.display.clone()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.call)
    }
}

fn verbs(call: &Call) -> Verbs {
    let (doing, done, failed) = match call {
        Call::Spawn(_) => ("Starting", "Started", "couldn't start"),
        Call::Send { .. } => ("Messaging", "Messaged", "couldn't message"),
        Call::Stop { end: false, .. } => ("Stopping", "Stopped", "couldn't stop"),
        Call::Stop { end: true, .. } => ("Ending", "Ended", "couldn't end"),
        Call::Rename { .. } => ("Renaming", "Renamed", "couldn't rename"),
        Call::List => ("Listing agents", "Listed agents", "couldn't list agents"),
        Call::Report(_) => ("Reporting", "Reported", "couldn't report"),
    };
    Verbs {
        doing: doing.to_owned(),
        done: done.to_owned(),
        failed: failed.to_owned(),
    }
}

fn display(tool: &str, target: String, call: &Call) -> Display {
    Display {
        action: Action::Other(tool.to_owned()),
        verbs: Some(verbs(call)),
        target,
        output: OutputKind::Text,
        view: None,
        title: None,
    }
}

fn report_target(map: &Map<String, Value>) -> String {
    map.get("text")
        .and_then(Value::as_str)
        .and_then(report_line)
        .unwrap_or_default()
}

pub fn prepare(tool: &str, input: &str) -> Result<Box<dyn Prepared>, String> {
    let (target, call) = match tool {
        SPAWN_AGENT => {
            let spawn = parse_spawn(input)?;
            (spawn.name.clone(), Call::Spawn(spawn))
        }
        LIST_AGENTS => {
            let map = object(input)?;
            only_fields(&map, &[])?;
            (String::new(), Call::List)
        }
        REPORT => {
            let map = object(input)?;
            (report_target(&map), Call::Report(input.to_owned()))
        }
        SEND_MESSAGE => {
            let map = object(input)?;
            only_fields(&map, &["text", "to", WAKE])?;
            let to = required(&map, "to")?;
            let text = required(&map, "text")?;
            let wake = flag(&map, WAKE)?.unwrap_or(true);
            (to.clone(), Call::Send { to, text, wake })
        }
        STOP_AGENT => {
            let map = object(input)?;
            only_fields(&map, &["end", "name"])?;
            let name = required(&map, "name")?;
            let end = flag(&map, "end")?.unwrap_or(false);
            (name.clone(), Call::Stop { name, end })
        }
        RENAME_AGENT => {
            let map = object(input)?;
            only_fields(&map, &["name", "new_name"])?;
            let name = required(&map, "name")?;
            let new_name = required(&map, "new_name")?;
            (
                format!("{name} to {new_name}"),
                Call::Rename { name, new_name },
            )
        }
        other => return Err(format!("the agents plugin has no tool `{other}`")),
    };
    Ok(Box::new(Asked {
        display: display(tool, target, &call),
        call,
    }))
}

fn answered(result: Result<String, String>) -> ToolRun {
    let (is_error, text) = match result {
        Ok(text) => (false, text),
        Err(text) => (true, text),
    };
    ToolRun::new(Outcome::Completed(Completed {
        is_error,
        exit: None,
    }))
    .with_result(text)
}

pub async fn run(host: &dyn Host, call: Box<dyn Prepared>, ctx: &dyn CallContext) -> ToolRun {
    let Ok(call) = call.into_any().downcast::<Call>() else {
        return ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT)
            .with_result(wisp_tool_text::output::WRONG_INPUT);
    };
    let result = match *call {
        Call::Spawn(input) => {
            let preset = input.preset.clone();
            host.spawn_agent(input, ctx).await.map(|agent| {
                let queued = host.snapshot().into_iter().any(|node| {
                    node.path == agent.path
                        && node.state == AgentState::Queued(QueueReason::Concurrency)
                });
                let text = started(&agent, preset.as_deref());
                if queued {
                    format!("{text} {QUEUED_SENTENCE}")
                } else {
                    text
                }
            })
        }
        Call::List => host.list_agents(ctx).map(|rows| listed(&rows)),
        Call::Report(input) => host.report(input, ctx).map(|()| REPORTED.to_owned()),
        Call::Send { to, text, wake } => host
            .send_message(to, text, wake, ctx)
            .map(|delivered| sent(&delivered)),
        Call::Stop { name, end } => host.stop_agent(name, end, ctx).map(|done| stopped(&done)),
        Call::Rename { name, new_name } => host
            .rename_agent(name, new_name, ctx)
            .map(|done| renamed(&done)),
    };
    answered(result)
}

fn listed_refreshes(node: &AgentNode, shown: &ShownState) -> Option<u32> {
    let waits = |outcome| matches!(outcome, Some(IdleOutcome::Asked | IdleOutcome::Waiting));
    let idle = matches!(node.state, AgentState::Idle(outcome) if !waits(outcome));
    let warm = shown.keep_warm.filter(|_| idle)?;
    (!warm.paused && (1..warm.limit).contains(&warm.refreshes)).then_some(warm.refreshes)
}

fn now_ms(host: &dyn Host) -> u64 {
    let now = host.now();
    let seconds = u64::try_from(now.seconds).unwrap_or(0);
    seconds.saturating_mul(1000) + u64::from(now.nanos / 1_000_000)
}

fn kept_warm(host: &dyn Host, previous: &Prior) -> Rendered {
    let Some(me) = host.current_agent() else {
        return Rendered::default();
    };
    let states = host.states();
    let children: Vec<(AgentNode, String)> = host
        .snapshot()
        .into_iter()
        .filter(|n| n.parent.as_deref() == Some(me.path.as_str()))
        .filter_map(|n| {
            let refreshes = states
                .iter()
                .find(|s| s.path == n.path)
                .and_then(|s| listed_refreshes(&n, s))?;
            let line = format!("{}\t{}\t{refreshes}", n.path, n.since_ms);
            Some((n, line))
        })
        .collect();
    let seen = match previous {
        Prior::Snapshot(seen) | Prior::Legacy(Some(seen)) => seen.as_str(),
        Prior::None | Prior::Legacy(None) => "",
    };
    let now = now_ms(host);
    let named: Vec<(&str, u64)> = children
        .iter()
        .filter(|(_, line)| !seen.lines().any(|l| l == line))
        .map(|(n, _)| (n.path.as_str(), now.saturating_sub(n.since_ms)))
        .collect();
    let lines: Vec<&str> = children.iter().map(|(_, line)| line.as_str()).collect();
    Rendered {
        text: (!named.is_empty()).then(|| kept_warm_line(&named)),
        snapshot: lines.join("\n"),
    }
}

pub struct AgentTools {
    host: Arc<dyn Host>,
}

impl Plugin for AgentTools {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { defs() })
    }

    fn prepare<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(std::future::ready(prepare(tool, input)))
    }

    fn time_limit_ms<'a>(&'a self, tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(Some(meta(tool).timeouts.default_ms)))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(run(self.host.as_ref(), call, ctx))
    }

    fn render_section<'a>(
        &'a self,
        name: &'a str,
        _at: At,
        previous: Prior,
        _target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        let rendered = match name {
            KEPT_WARM => kept_warm(self.host.as_ref(), &previous),
            _ => Rendered::default(),
        };
        Box::pin(std::future::ready(rendered))
    }
}

#[cfg(test)]
mod tests;
