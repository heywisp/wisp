use std::any::Any;
use std::sync::Arc;

use serde_json::{Value, json};
use wisp_plugin_api::commands::CommandReply;
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{Action, Display, Failure, Outcome, OutputKind, ToolRun, Verbs};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::chat::{COMMAND, DONE, NO_NAME, refused, renamed};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");
pub const NAME: &str = "rename_chat";
const TIME_LIMIT_MS: u64 = 5_000;
const DESCRIPTION: &str = "Rename this chat when the user asks you to. The name is the title this chat, or session, has in the user's list of chats; it replaces the name Wisp gave it.";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Chat { host })
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "The new name: one line, a few words."}
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

pub fn meta() -> ToolMeta {
    ToolMeta {
        effect: Effect::NonIdempotent,
        tags: Tags::default(),
        parallel_safe: false,
        timeouts: Timeouts {
            default_ms: TIME_LIMIT_MS,
            max_ms: Some(TIME_LIMIT_MS),
        },
        saves_output: false,
        needs_workspace: false,
        ends_reply: false,
        asks_user: false,
        reports_jobs: false,
        writing: None,
        purpose: Purpose::Run,
    }
}

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: DESCRIPTION.to_owned(),
            input_schema: schema().to_string(),
            meta: meta(),
        }),
        system_line: None,
        context_line: None,
    }
}

fn parse(input: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))?;
    let map = value.as_object().ok_or("input must be an object")?;
    if let Some(key) = map.keys().find(|key| key.as_str() != "name") {
        return Err(format!("unknown field `{key}`"));
    }
    let name = map
        .get("name")
        .and_then(Value::as_str)
        .ok_or("`name` must be a string")?;
    if name.trim().is_empty() {
        return Err("`name` is blank".to_owned());
    }
    Ok(name.to_owned())
}

fn display(name: &str) -> Display {
    Display {
        action: Action::Other(NAME.to_owned()),
        verbs: Some(Verbs {
            doing: "Renaming this chat".to_owned(),
            done: DONE.to_owned(),
            failed: "couldn't rename this chat".to_owned(),
        }),
        target: format!("to {name}"),
        output: OutputKind::None,
        view: None,
        title: None,
    }
}

struct Chat {
    host: Arc<dyn Host>,
}
struct Renaming(String);

impl Prepared for Renaming {
    fn display(&self) -> Display {
        display(&self.0)
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.0)
    }
}

impl Plugin for Chat {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { vec![def()] })
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let call: Box<dyn Prepared> = Box::new(Renaming(parse(input)?));
            Ok(call)
        })
    }

    fn time_limit_ms<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(Some(TIME_LIMIT_MS)))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            let Ok(name) = call.into_any().downcast::<String>() else {
                return ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT)
                    .with_result(wisp_tool_text::output::WRONG_INPUT);
            };
            match self.host.chat().rename(*name) {
                Ok(kept) => ToolRun::completed().with_result(renamed(&kept)),
                Err(message) => ToolRun::new(Outcome::Failed(Failure {
                    message: message.clone(),
                    before_start: false,
                }))
                .with_result(message),
            }
        })
    }

    fn run_command<'a>(&'a self, name: &'a str, args: &'a str) -> BoxFuture<'a, CommandReply> {
        Box::pin(async move {
            if name != COMMAND {
                return CommandReply::None;
            }
            let reply = if args.trim().is_empty() {
                refused(NO_NAME)
            } else {
                match self.host.chat().rename(args.to_owned()) {
                    Ok(kept) => renamed(&kept),
                    Err(reason) => refused(&reason),
                }
            };
            CommandReply::Note(reply)
        })
    }
}

#[cfg(test)]
mod tests;
