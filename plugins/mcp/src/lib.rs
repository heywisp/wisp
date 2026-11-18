use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use unicode_width::UnicodeWidthStr;
use wisp_plugin_api::commands::{CommandLine, CommandReply, CommandRow};
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::index::{Entry, Kind, SEARCH_CAPABILITY};
use wisp_plugin_api::mcp::{State, Tool};
use wisp_plugin_api::slots::{TextSpan, Tone};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, DoctorCell, DoctorRow, Effect, Prepared, Purpose, Tags, Timeouts,
    ToolDef, ToolKind, ToolMeta,
};
use wisp_plugin_api::types::{Action, Display, OutputKind, ToolRun};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::index::{SEARCH_OFF, already_in_view};
use wisp_tool_text::mcp as wording;

const LIMIT_MS: u64 = 120_000;
const TOTAL_MS: u64 =
    LIMIT_MS + wisp_plugin_api::mcp::START_LIMIT_MS + wisp_tool_text::question::TIME_LIMIT_MS;

struct McpCall {
    display: Display,
    name: String,
    input: String,
}

impl Prepared for McpCall {
    fn display(&self) -> Display {
        self.display.clone()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

pub fn bundled() -> Bundled {
    Bundled {
        manifest: include_str!("../wisp-plugin.toml"),
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Mcp {
        listing: AtomicBool::new(false),
        known: Mutex::default(),
        opened: Mutex::default(),
        revision: AtomicU64::new(host.mcp().revision()),
        host,
    })
}

pub struct Mcp {
    host: Arc<dyn Host>,
    known: Mutex<BTreeMap<String, (String, String)>>,
    opened: Mutex<BTreeSet<String>>,
    listing: AtomicBool,
    revision: AtomicU64,
}

impl Mcp {
    fn changed(&self) {
        let revision = self.host.mcp().revision();
        if self.revision.swap(revision, Ordering::AcqRel) != revision {
            self.opened
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
            self.host.index().changed();
        }
    }

    fn on(&self, server: &str) -> bool {
        !self
            .host
            .mcp()
            .servers()
            .iter()
            .any(|row| row.name == server && row.off)
    }

    fn tools(&self) -> Vec<Tool> {
        self.host
            .mcp()
            .tools()
            .into_iter()
            .filter(|tool| self.on(&tool.server))
            .collect()
    }

    fn search(&self) -> bool {
        self.host
            .current()
            .capabilities
            .iter()
            .any(|name| name == SEARCH_CAPABILITY)
    }

    fn list(&self) -> CommandReply {
        let servers = self.host.mcp().servers();
        let width = servers
            .iter()
            .map(|server| server.name.width())
            .max()
            .unwrap_or(0);
        let mut title = span("MCP servers", Tone::Text);
        title.bold = true;
        let mut lines = vec![CommandLine::Row(CommandRow {
            left: vec![title],
            right: if servers.is_empty() {
                Vec::new()
            } else {
                vec![span("tools", Tone::Muted)]
            },
        })];
        for server in &servers {
            let (state, tone) = if server.off {
                ("off", Tone::Muted)
            } else {
                state(&server.state)
            };
            lines.push(CommandLine::Row(CommandRow {
                left: vec![
                    span(format!("{}  ", padded(&server.name, width)), Tone::Text),
                    TextSpan {
                        bold: matches!(server.state, State::NeedsSignIn | State::NeedsKey(_)),
                        ..span(state, tone)
                    },
                ],
                right: vec![span(tool_count(server), Tone::Text)],
            }));
            if let State::Failed(message) = &server.state
                && !server.off
            {
                let sentence = message
                    .strip_prefix(&format!("{} ", server.name))
                    .map(|rest| format!("it {rest}"))
                    .unwrap_or_else(|| message.clone());
                lines.push(CommandLine::Text(vec![span(
                    format!("{}{sentence}", " ".repeat(width + 2)),
                    Tone::Muted,
                )]));
            }
        }
        if servers.is_empty() {
            lines.push(text("none yet: /mcp add <name> <command or address>"));
        }
        if servers.iter().any(|server| server.off) {
            lines.push(CommandLine::Text(Vec::new()));
            lines.push(text(wisp_tool_text::manage::TURN_ON_WHERE));
        }
        let waiting: Vec<_> = servers
            .iter()
            .filter(|server| !server.off && server.state == State::NeedsSignIn)
            .map(|server| format!("/mcp sign-in {}", server.name))
            .collect();
        if !waiting.is_empty() {
            lines.push(CommandLine::Text(Vec::new()));
            lines.push(text(format!("sign in: {}", waiting.join(", "))));
        }
        let keys: Vec<_> = servers
            .iter()
            .filter(|server| !server.off)
            .filter_map(|server| {
                let State::NeedsKey(keys) = &server.state else {
                    return None;
                };
                Some(format!("{} for {}", keys.join(" and "), server.name))
            })
            .collect();
        if !keys.is_empty() {
            lines.push(CommandLine::Text(Vec::new()));
            lines.push(text(format!(
                "set a key: ask your agent to set {}; Wisp asks you for it",
                keys.join(", ")
            )));
        }
        if !servers.is_empty() && !self.search() {
            lines.push(CommandLine::Text(Vec::new()));
            lines.push(text(SEARCH_OFF));
        }
        CommandReply::Lines(lines)
    }

    async fn add(&self, args: &[String]) -> CommandReply {
        let [name, command, tail @ ..] = args else {
            return answer(wisp_tool_text::mcp::MISSING_COMMAND);
        };
        if wisp_tool_text::manage::looks_like_key(name) {
            return answer(wisp_tool_text::manage::KEY_NAME);
        }
        match self.host.mcp().add(name, command, tail).await {
            Ok(count) => {
                self.changed();
                let waiting = self
                    .host
                    .mcp()
                    .servers()
                    .iter()
                    .any(|server| server.name == *name && server.state == State::NeedsSignIn);
                let mut lines = vec![text(if waiting {
                    format!("{name} added · sign in with /mcp sign-in {name}")
                } else {
                    format!("{name} added · {}", wording::tool_count(count))
                })];
                if !self.search() {
                    lines.push(text(SEARCH_OFF));
                }
                CommandReply::Lines(lines)
            }
            Err(error) => answer(format!(
                "{name} not added: {}",
                error
                    .strip_prefix(&format!("{name} couldn't start: "))
                    .or_else(|| error.strip_prefix(&format!("{name} ")))
                    .unwrap_or(&error)
            )),
        }
    }
    async fn sign_in(&self, args: &[String]) -> CommandReply {
        match args {
            [name] => match self.host.mcp().sign_in_start(name).await {
                Ok(Some(_)) => {
                    CommandReply::Lines(wording::sign_in_help(name).into_iter().map(text).collect())
                }
                Ok(None) => answer(format!("{name} already signed in")),
                Err(error) => answer(error),
            },
            [name, address] => match self.host.mcp().sign_in_finish(name, address).await {
                Ok(_) => {
                    self.changed();
                    CommandReply::None
                }
                Err(error) => answer(error),
            },
            _ => answer("use /mcp sign-in <name> or /mcp sign-in <name> <address>"),
        }
    }
}

impl Plugin for Mcp {
    fn entries(&self) -> BoxFuture<'_, Vec<Entry>> {
        Box::pin(async move {
            if !self.search() {
                return Vec::new();
            }
            let mut entries = Vec::new();
            let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
            for prompt in self
                .host
                .mcp()
                .prompts()
                .into_iter()
                .filter(|prompt| self.on(&prompt.server))
            {
                known.insert(
                    prompt.name.clone(),
                    (prompt.server, prompt.original.clone()),
                );
                let summary = wisp_index::first_line(&prompt.description);
                let arguments = prompt
                    .arguments
                    .iter()
                    .map(|arg| format!("{} {}", arg.name, arg.description))
                    .collect::<Vec<_>>()
                    .join(" ");
                entries.push(Entry {
                    kind: Kind::Skill,
                    summary: if summary.is_empty() {
                        prompt.original.clone()
                    } else {
                        summary.to_owned()
                    },
                    text: format!("{} {} {arguments}", prompt.original, prompt.description),
                    name: prompt.name,
                });
            }
            for resource in self
                .host
                .mcp()
                .resources()
                .into_iter()
                .filter(|resource| self.on(&resource.server))
            {
                known.insert(
                    resource.name.clone(),
                    (resource.server, resource.uri.clone()),
                );
                let summary = wisp_index::first_line(&resource.description);
                entries.push(Entry {
                    kind: Kind::Document,
                    summary: if summary.is_empty() {
                        resource.title.clone()
                    } else {
                        summary.to_owned()
                    },
                    text: format!(
                        "{} {} {}",
                        resource.title, resource.uri, resource.description
                    ),
                    name: resource.name,
                });
            }
            entries.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
            entries
        })
    }

    fn open_entry<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.changed();
            if let Some((server, _)) = self
                .known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                && !self.on(server)
            {
                return Err(wisp_tool_text::manage::turned_off(server));
            }
            if let Some(prompt) = self
                .host
                .mcp()
                .prompts()
                .into_iter()
                .find(|prompt| prompt.name == name && self.on(&prompt.server))
            {
                if self
                    .opened
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .contains(name)
                {
                    return Ok(already_in_view(name));
                }
                let messages = if prompt.arguments.iter().any(|arg| arg.required) {
                    Ok(Vec::new())
                } else {
                    self.host.mcp().get_prompt(name).await
                };
                self.changed();
                let text = wording::skill(&prompt, &messages?)?;
                self.opened
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(name.to_owned());
                return Ok(text);
            }
            if let Some(resource) = self
                .host
                .mcp()
                .resources()
                .into_iter()
                .find(|resource| resource.name == name && self.on(&resource.server))
            {
                let parts = self.host.mcp().read_resource(name).await;
                self.changed();
                return wording::document(&resource, &parts?);
            }
            let known = self
                .known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .cloned();
            match known {
                Some((server, original))
                    if self
                        .host
                        .mcp()
                        .servers()
                        .iter()
                        .any(|row| row.name == server) =>
                {
                    Err(wording::gone(&server, &original))
                }
                Some((server, _)) => Err(wording::removed(&server)),
                None => wisp_plugin_api::index::Index::open(&(), name).await,
            }
        })
    }

    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async move {
            let tools = self.tools();
            self.known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(tools.iter().map(|tool| {
                    (
                        tool.name.clone(),
                        (tool.server.clone(), tool.original.clone()),
                    )
                }));
            tools.into_iter().map(definition).collect()
        })
    }

    fn prepare<'a>(
        &'a self,
        name: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let (server, original) = self
                .known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .cloned()
                .ok_or_else(|| format!("there is no tool named `{name}`"))?;
            if !self.on(&server) {
                return Err(wisp_tool_text::manage::turned_off(&server));
            }
            if !self.tools().iter().any(|tool| tool.name == name) {
                return Err(
                    if self
                        .host
                        .mcp()
                        .servers()
                        .iter()
                        .any(|row| row.name == server)
                    {
                        wording::gone(&server, &original)
                    } else {
                        wording::removed(&server)
                    },
                );
            }
            let display = display(server, original);
            Ok(Box::new(McpCall {
                display,
                name: name.to_owned(),
                input: input.to_owned(),
            }) as Box<dyn Prepared>)
        })
    }

    fn rejected_display<'a>(
        &'a self,
        name: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        Box::pin(async move {
            self.known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .cloned()
                .map(|(server, original)| display(server, original))
        })
    }

    fn time_limit_ms<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(async { Some(TOTAL_MS) })
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            let call = match call.into_any().downcast::<McpCall>() {
                Ok(call) => call,
                Err(_) => return ToolRun::failed_to_start("that MCP call is not valid"),
            };
            let result = self
                .host
                .mcp()
                .call(ctx, &call.name, &call.input, LIMIT_MS)
                .await;
            self.changed();
            if ctx.cancelled() {
                return ToolRun::new(wisp_plugin_api::types::Outcome::Cancelled);
            }
            result.unwrap_or_else(|message| {
                ToolRun::new(wisp_plugin_api::types::Outcome::Failed(
                    wisp_plugin_api::types::Failure {
                        message,
                        before_start: false,
                    },
                ))
            })
        })
    }

    fn run_command<'a>(&'a self, name: &'a str, args: &'a str) -> BoxFuture<'a, CommandReply> {
        Box::pin(async move {
            if name != "mcp" {
                return CommandReply::None;
            }
            let Some(args) = shlex::split(args) else {
                return answer(wisp_tool_text::mcp::UNFINISHED_QUOTE);
            };
            match args.as_slice() {
                [] => self.list(),
                [word, rest @ ..] if word == "add" => self.add(rest).await,
                [word, rest @ ..] if word == "sign-in" => self.sign_in(rest).await,
                [word, name] if word == "remove" => match self.host.mcp().remove(name).await {
                    Ok(()) => {
                        self.changed();
                        answer(wording::remove_done(name))
                    }
                    Err(error) => answer(error),
                },
                _ => answer(
                    "use /mcp, /mcp add <name> <command or address>, /mcp remove <name>, or /mcp sign-in <name>",
                ),
            }
        })
    }

    fn on_hook<'a>(&'a self, hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        Box::pin(async move {
            match (hook, event) {
                ("compacted", Event::AfterCompact(info)) if info.failed.is_none() => {
                    self.opened
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clear();
                }
                ("rewound", Event::Rewound(_)) => {
                    self.opened
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clear();
                }
                _ => {}
            }
            if hook == "list" && !self.listing.swap(true, Ordering::AcqRel) {
                let missing: Vec<_> = self
                    .host
                    .mcp()
                    .servers()
                    .into_iter()
                    .filter(|server| !server.listed && server.state == State::Stopped)
                    .collect();
                futures::future::join_all(
                    missing
                        .iter()
                        .map(|server| self.host.mcp().connect(&server.name)),
                )
                .await;
            }
            self.changed();
            Reply::None
        })
    }

    fn doctor<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
        Box::pin(async move {
            let servers = self.host.mcp().servers();
            let count_width = servers
                .iter()
                .map(|server| server.tools.to_string().len())
                .max()
                .unwrap_or(0);
            let name_width = servers
                .iter()
                .map(|server| server.name.width())
                .max()
                .unwrap_or(0);
            servers
                .into_iter()
                .map(|server| DoctorRow {
                    key: format!("mcp {}", padded(&server.name, name_width)),
                    cells: vec![
                        DoctorCell::Text(state(&server.state).0.into()),
                        DoctorCell::Text(format!("{:>count_width$}", tool_count(&server))),
                    ],
                })
                .collect()
        })
    }
}

fn padded(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

fn display(server: String, original: String) -> Display {
    Display {
        action: Action::Other(original),
        verbs: None,
        target: server,
        output: OutputKind::Text,
        view: None,
        title: None,
    }
}

fn state(state: &State) -> (&'static str, Tone) {
    match state {
        State::Ready => ("running", Tone::Ok),
        State::NeedsSignIn => ("needs sign-in", Tone::Text),
        State::NeedsKey(_) => ("needs a key", Tone::Text),
        State::Failed(_) => ("failed", Tone::Error),
        State::Stopped | State::Starting => ("starts on use", Tone::Muted),
    }
}

fn span(text: impl Into<String>, tone: Tone) -> TextSpan {
    TextSpan {
        text: text.into(),
        tone,
        bold: false,
    }
}

fn text(value: impl Into<String>) -> CommandLine {
    CommandLine::Text(vec![span(value, Tone::Text)])
}

fn answer(value: impl Into<String>) -> CommandReply {
    CommandReply::Lines(vec![text(value)])
}

fn definition(tool: Tool) -> ToolDef {
    ToolDef {
        name: tool.name,
        kind: ToolKind::Client(ClientTool {
            description: tool.description,
            input_schema: tool.input_schema,
            meta: ToolMeta {
                effect: Effect::NonIdempotent,
                tags: Tags {
                    reads_untrusted: true,
                    network: true,
                    ..Tags::default()
                },
                parallel_safe: true,
                timeouts: Timeouts {
                    default_ms: TOTAL_MS,
                    max_ms: Some(TOTAL_MS),
                },
                saves_output: true,
                needs_workspace: true,
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

#[cfg(test)]
mod tests;

fn tool_count(server: &wisp_plugin_api::mcp::Server) -> String {
    if matches!(
        server.state,
        State::Failed(_) | State::NeedsSignIn | State::NeedsKey(_)
    ) || !server.listed
    {
        "—".into()
    } else {
        server.tools.to_string()
    }
}
