use std::any::Any;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::index::Kind;
use wisp_plugin_api::manage as switch;
use wisp_plugin_api::manage::{Action as Verb, Kind as Noun};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{
    Action, Completed, Display, Fact, Failure, Hit, Outcome, OutputKind, ToolRun, Verbs,
};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::index::{INVALID_INPUT, hit_line, run_with};
use wisp_tool_text::manage;

mod skills;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");
pub const SYSTEM_LINE: &str = "Some tools, skills and documents may not be loaded here. Before you work around a missing one or say you can't do something, look for it with `search`; `use` opens or runs what it found.";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate: |host| Arc::new(Search { host }),
    }
}

pub fn defs() -> Vec<ToolDef> {
    let kinds = [Kind::Tool, Kind::Skill, Kind::Document].map(Kind::name);
    [
        ("search", "Look for tools, skills and documents you need. Use the words their descriptions would use, not synonyms. Tool hits tell you how to run them; read skills and documents with use.", json!({"type":"object","properties":{"query":{"type":"string","description":"Words describing what you need."},"kind":{"type":"string","enum":kinds,"description":"Restrict results to this kind; omit to search all kinds."},"limit":{"type":"integer","minimum":0,"description":"Maximum results; five by default."}},"required":["query"],"additionalProperties":false})),
        ("use", "Open a skill or document found by search, or run a found tool with its input.", json!({"type":"object","properties":{"name":{"type":"string","description":"The full name returned by search."},"input":{"type":"object","description":"Arguments for a tool; omit when reading a skill or document."}},"required":["name"],"additionalProperties":false})),
    ].into_iter().map(|(name, description, schema)| ToolDef {
        name: name.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description.to_owned(), input_schema: schema.to_string(),
            meta: ToolMeta { effect: Effect::ReadOnly, tags: Tags { reads_untrusted: true, network: name == "use", ..Tags::default() }, parallel_safe: false, timeouts: Timeouts { default_ms: 30_000, max_ms: Some(30_000) }, saves_output: false, needs_workspace: false, ends_reply: false, asks_user: false, reports_jobs: false, writing: None, purpose: Purpose::Run },
        }),
        system_line: (name == "search").then(|| SYSTEM_LINE.to_owned()),
        context_line: None,
    }).chain([manage_def()]).collect()
}

fn manage_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["on", "off", "add", "remove"], "description": "on or off switches it in this chat; add or remove changes a saved server or skill."},
            "kind": {"type": "string", "enum": ["tool", "skill", "server"], "description": "Whether name is a tool, skill or server."},
            "name": {"type": "string", "description": "The tool, skill or server name. Never put a key here."},
            "keys": {"type": "array", "items": {"type": "string"}, "description": "Names of environment variables the server reads, never their values. Wisp asks the user for missing keys. With add and an existing server name, sets those keys anew."},
            "from": {"type": "string", "description": "For add skill, the local skill folder. For add server, the command line or address after the name in /mcp add. Leave it out to sign in to an already added server. Never put a key here."},
        },
        "required": ["action", "kind", "name"],
        "additionalProperties": false,
    });
    ToolDef {
        name: "manage".to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: "Add or remove an MCP server or skill. For a skill, add copies the folder in from to Wisp's skills folder; remove only removes a skill there. Start a server's sign-in with add and its name. Turn a tool, skill or server off in this chat, or back on, from the next message. Turning on what the user's settings have off, or turning off search, use or manage, asks the user first. add or remove says how a tool is installed. Never put a key in name, from or keys; keys names environment variables and Wisp asks the user for their values.".to_owned(),
            input_schema: schema.to_string(),
            meta: ToolMeta {
                effect: Effect::IdempotentKeyed,
                tags: Tags {
                    reversible: true,
                    ..Tags::default()
                },
                parallel_safe: false,
                timeouts: Timeouts {
                    default_ms: manage::TIME_LIMIT_MS,
                    max_ms: Some(manage::TIME_LIMIT_MS),
                },
                saves_output: false,
                needs_workspace: false,
                ends_reply: false,
                asks_user: false,
                reports_jobs: false,
                writing: None,
                purpose: Purpose::Run,
            },
        }),
        system_line: None,
        context_line: None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    query: String,
    kind: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageInput {
    action: Verb,
    kind: Noun,
    name: String,
    from: Option<String>,
    #[serde(default)]
    keys: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    name: String,
    input: Option<serde_json::Map<String, Value>>,
}

enum Asked {
    Search {
        query: String,
        kind: Option<Kind>,
        limit: Option<usize>,
    },
    Open {
        name: String,
        kind: Option<Kind>,
    },
    Manage {
        verb: Verb,
        noun: Noun,
        name: String,
        from: Option<String>,
        keys: Vec<String>,
    },
}

impl Prepared for Asked {
    fn display(&self) -> Display {
        let (action, verbs, target) = match self {
            Self::Search { query, .. } => (Action::Search, None, format!("\"{query}\"")),
            Self::Open { name, kind } => {
                let noun = kind.map_or("entry", Kind::name);
                (
                    Action::Read,
                    Some(Verbs {
                        doing: format!("Reading {noun}"),
                        done: format!("Read {noun}"),
                        failed: format!("couldn't read {noun}"),
                    }),
                    name.clone(),
                )
            }
            Self::Manage {
                verb, noun, name, ..
            } => {
                let verb = match verb {
                    Verb::On => manage::TURN_ON,
                    Verb::Off => manage::TURN_OFF,
                    Verb::Add => manage::ADD,
                    Verb::Remove => manage::REMOVE,
                };
                let noun = manage::kind_name(*noun);
                let words = format!("{verb} {noun}");
                (
                    Action::Run,
                    Some(Verbs {
                        doing: words.clone(),
                        done: words,
                        failed: format!("couldn't {} {noun}", verb.to_lowercase()),
                    }),
                    name.clone(),
                )
            }
        };
        Display {
            action,
            verbs,
            target,
            output: OutputKind::Text,
            view: None,
            title: None,
        }
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

pub struct Search {
    host: Arc<dyn Host>,
}

impl Search {
    async fn run_search(&self, query: &str, kind: Option<Kind>, limit: Option<usize>) -> ToolRun {
        let hits = wisp_index::Index::new(self.host.index().entries()).search(query, kind, limit);
        let mut lines = Vec::new();
        for hit in &hits {
            let next = if hit.kind == Kind::Tool {
                match self.host.index().open(&hit.name).await {
                    Ok(parameters) => run_with(&parameters),
                    Err(error) => error,
                }
            } else {
                "read it with use".to_owned()
            };
            lines.push(hit_line(hit, &next));
        }
        let mut run = ToolRun::completed().with_result(if lines.is_empty() {
            "nothing found".to_owned()
        } else {
            lines.join("\n")
        });
        run.facts.push(Fact::Hits(
            hits.into_iter()
                .map(|entry| Hit {
                    kind: entry.kind,
                    name: entry.name,
                })
                .collect(),
        ));
        run
    }

    async fn run_manage(
        &self,
        verb: Verb,
        noun: Noun,
        name: String,
        from: Option<String>,
        keys: Vec<String>,
        ctx: &dyn CallContext,
    ) -> ToolRun {
        let kind = noun;
        let action = match (verb, kind) {
            (Verb::On, _) => switch::Action::On,
            (Verb::Off, _) => switch::Action::Off,
            (Verb::Add, switch::Kind::Tool) => return refused(manage::ADD_TOOL.to_owned()),
            (Verb::Remove, switch::Kind::Tool) => return refused(manage::REMOVE_TOOL.to_owned()),
            (Verb::Add, switch::Kind::Server | switch::Kind::Skill) => switch::Action::Add,
            (Verb::Remove, switch::Kind::Server | switch::Kind::Skill) => switch::Action::Remove,
        };
        let change = switch::Change {
            action,
            kind,
            name,
            from,
            keys,
        };
        let skill = kind == switch::Kind::Skill && matches!(verb, Verb::Add | Verb::Remove);
        let result = if skill {
            skills::change(self.host.as_ref(), &change, ctx).await
        } else {
            self.host.manage().manage(change, ctx).await
        };
        if ctx.cancelled() && !(skill && result.is_ok()) {
            return ToolRun::new(Outcome::Cancelled);
        }
        match result {
            Ok(text) => ToolRun::completed().with_result(text),
            Err(refusal) => refused(refusal),
        }
    }

    async fn run_use(&self, name: &str) -> ToolRun {
        let result = self.host.index().open(name).await;
        let is_error = result.is_err();
        let text = result.unwrap_or_else(|error| error);
        let mut run = ToolRun::new(Outcome::Completed(Completed {
            is_error,
            exit: None,
        }))
        .with_result(text.clone());
        if !is_error {
            run.facts.push(Fact::Tokens(self.host.estimate_text(&text)));
        }
        run
    }
}

fn refused(message: String) -> ToolRun {
    ToolRun::new(Outcome::Failed(Failure {
        message,
        before_start: false,
    }))
}

impl Plugin for Search {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { defs() })
    }
    fn prepare<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let asked = match tool {
                "search" => {
                    let query: Query =
                        serde_json::from_str(input).map_err(|_| INVALID_INPUT.to_owned())?;
                    let kind = query
                        .kind
                        .as_deref()
                        .map(|kind| Kind::parse(kind).ok_or_else(|| INVALID_INPUT.to_owned()))
                        .transpose()?;
                    Asked::Search {
                        query: query.query,
                        kind,
                        limit: query.limit,
                    }
                }
                "use" => {
                    let open: Open =
                        serde_json::from_str(input).map_err(|_| INVALID_INPUT.to_owned())?;
                    if open.input.is_some() {
                        return Err(
                            "input is only for a tool; search for its full name first".to_owned()
                        );
                    }
                    let kind = self
                        .host
                        .index()
                        .entries()
                        .into_iter()
                        .find(|entry| entry.name == open.name)
                        .map(|entry| entry.kind);
                    Asked::Open {
                        name: open.name,
                        kind,
                    }
                }
                "manage" => {
                    let input: ManageInput =
                        serde_json::from_str(input).map_err(|_| INVALID_INPUT.to_owned())?;
                    if manage::looks_like_key(&input.name) {
                        return Err(manage::KEY_NAME.to_owned());
                    }
                    if input.keys.iter().any(|key| !manage::plain_variable(key)) {
                        return Err(wisp_tool_text::mcp::KEY_VARIABLE.to_owned());
                    }
                    Asked::Manage {
                        verb: input.action,
                        noun: input.kind,
                        name: input.name,
                        from: input.from,
                        keys: input.keys,
                    }
                }
                _ => return Err(format!("the search plugin has no tool `{tool}`")),
            };
            Ok(Box::new(asked) as Box<dyn Prepared>)
        })
    }
    fn rejected_display<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        Box::pin(async move {
            if tool != "manage" {
                return None;
            }
            let input: ManageInput = serde_json::from_str(input).ok()?;
            let name = if manage::looks_like_key(&input.name) {
                String::new()
            } else {
                input.name
            };
            Some(
                Asked::Manage {
                    verb: input.action,
                    noun: input.kind,
                    name,
                    from: None,
                    keys: Vec::new(),
                }
                .display(),
            )
        })
    }
    fn time_limit_ms<'a>(&'a self, tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(async move { (tool == "manage").then_some(manage::TIME_LIMIT_MS) })
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            let Ok(asked) = call.into_any().downcast::<Asked>() else {
                return ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT);
            };
            match *asked {
                Asked::Search { query, kind, limit } => self.run_search(&query, kind, limit).await,
                Asked::Open { name, .. } => self.run_use(&name).await,
                Asked::Manage {
                    verb,
                    noun,
                    name,
                    from,
                    keys,
                } => self.run_manage(verb, noun, name, from, keys, ctx).await,
            }
        })
    }
}

#[cfg(test)]
mod tests;
