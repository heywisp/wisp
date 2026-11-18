use std::any::Any;
use std::sync::Arc;

use serde_json::{Value, json};
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{Action, Completed, Display, Outcome, OutputKind, ToolRun, Verbs};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::config::{accounts, listed};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const ID: &str = "menus";

pub const NAME: &str = "list_settings";

pub const ACCOUNTS: &str = "list_accounts";

const LIMIT_MS: u64 = 5_000;

const ACCOUNTS_LIMIT_MS: u64 = 10_000;

const DESCRIPTION: &str = "List Wisp's settings as /config shows them for this chat: each setting's value and where it comes from (default, config, env, flag, session and so on). With filter, also what each matching setting does and the values it takes. Secrets are never shown. Changing a setting is the user's to do: point them to /config.";

const ACCOUNTS_DESCRIPTION: &str = "List the accounts the user has set up in Wisp, as /account, /model and /usage show them: each account's name and kind, whether it can be used and is signed in, which one you run on and where new chats start, the models it offers and, for a subscription, the usage limits Wisp last read. Keys and tokens are never shown.";

const FILTER: &str = "filter";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Menus { host })
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            FILTER: {
                "type": "string",
                "description": "Text to look for in a setting's name, label or help, such as theme or compact; leave it out to list every setting."
            }
        },
        "additionalProperties": false
    })
}

pub fn accounts_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

fn limit_ms(tool: &str) -> u64 {
    if tool == ACCOUNTS {
        ACCOUNTS_LIMIT_MS
    } else {
        LIMIT_MS
    }
}

pub fn meta(tool: &str) -> ToolMeta {
    ToolMeta {
        effect: Effect::ReadOnly,
        tags: Tags::default(),
        parallel_safe: false,
        timeouts: Timeouts {
            default_ms: limit_ms(tool),
            max_ms: Some(limit_ms(tool)),
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

pub fn defs() -> Vec<ToolDef> {
    [
        (NAME, DESCRIPTION, schema()),
        (ACCOUNTS, ACCOUNTS_DESCRIPTION, accounts_schema()),
    ]
    .into_iter()
    .map(|(name, description, schema)| ToolDef {
        name: name.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description.to_owned(),
            input_schema: schema.to_string(),
            meta: meta(name),
        }),
        system_line: None,
        context_line: None,
    })
    .collect()
}

pub fn parse(tool: &str, input: &str) -> Result<Option<String>, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))?;
    let Value::Object(map) = value else {
        return Err("input must be an object".to_owned());
    };
    if let Some(key) = map.keys().find(|k| *k != FILTER || tool == ACCOUNTS) {
        return Err(format!("unknown field `{key}`"));
    }
    match map.get(FILTER) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            Ok(Some(text.trim().to_owned()).filter(|text| !text.is_empty()))
        }
        Some(_) => Err(format!("`{FILTER}` must be a string")),
    }
}

enum Asked {
    Settings(Option<String>),
    Accounts,
}

impl Prepared for Asked {
    fn display(&self) -> Display {
        let (tool, what, target) = match self {
            Self::Settings(filter) => (NAME, "settings", filter.clone().unwrap_or_default()),
            Self::Accounts => (ACCOUNTS, "accounts", String::new()),
        };
        Display {
            action: Action::Other(tool.to_owned()),
            verbs: Some(Verbs {
                doing: format!("Listing {what}"),
                done: format!("Listed {what}"),
                failed: format!("couldn't list {what}"),
            }),
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

pub fn prepare(tool: &str, input: &str) -> Result<Box<dyn Prepared>, String> {
    let asked = match tool {
        NAME => Asked::Settings(parse(tool, input)?),
        ACCOUNTS => parse(tool, input).map(|_| Asked::Accounts)?,
        _ => return Err(format!("the {ID} plugin has no tool `{tool}`")),
    };
    Ok(Box::new(asked))
}

pub async fn run(host: &dyn Host, call: Box<dyn Prepared>, ctx: &dyn CallContext) -> ToolRun {
    let Ok(asked) = call.into_any().downcast::<Asked>() else {
        return ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT)
            .with_result(wisp_tool_text::output::WRONG_INPUT);
    };
    let answer = match *asked {
        Asked::Settings(filter) => host
            .settings(filter.clone(), ctx)
            .map(|rows| listed(&rows, filter.as_deref())),
        Asked::Accounts => host
            .list(ctx)
            .await
            .map(|list| accounts(&list, host.now().seconds)),
    };
    let (is_error, text) = match answer {
        Ok(text) => (false, text),
        Err(why) => (true, why),
    };
    ToolRun::new(Outcome::Completed(Completed {
        is_error,
        exit: None,
    }))
    .with_result(text)
}

pub struct Menus {
    host: Arc<dyn Host>,
}

impl Plugin for Menus {
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
        Box::pin(std::future::ready(Some(limit_ms(tool))))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(run(self.host.as_ref(), call, ctx))
    }
}

#[cfg(test)]
mod tests;
