use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use wisp_plugin_api::host::{CallContext, Host, Todo};
use wisp_plugin_api::prompt::{At, Prior, Rendered};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{Action, Display, Failure, Outcome, OutputKind, ToolRun};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::todos::{
    MAX_ITEMS, MIN_ITEMS, STATUSES, bad_status, check, check_count, normal, status_named, target,
    updated,
};

pub use wisp_tool_text::todos::NAME;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const SECTION: &str = "checklist";

const TIME_LIMIT_MS: u64 = 5_000;

const DESCRIPTION: &str = "Keep a checklist of the steps of your current task, which the user sees as you work. Use it for multi-step work: a task with three or more distinct steps, or several things asked for at once. Skip it for a question, a chat or a change of one or two steps. Write it before you start, then update it as you go. Each call replaces the whole list, so send every item each time, in order. Mark a step in_progress just before you start it, and completed as soon as it is fully done, one at a time rather than in batches; keep it in_progress while tests fail or the work is partial. At most one step is in_progress at a time. Add steps you discover, and mark ones that no longer apply cancelled.";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Todos { host })
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "todos": {
                "type": "array",
                "description": "The whole checklist, in order. It replaces the previous one.",
                "minItems": MIN_ITEMS,
                "maxItems": MAX_ITEMS,
                "items": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The step as a short imperative phrase, such as \"Run the tests\"."
                        },
                        "section": {
                            "type": "string",
                            "description": "A heading to show this step under, such as \"Next\". Steps with the same section are shown together."
                        },
                        "status": {
                            "type": "string",
                            "enum": STATUSES.map(|(name, _)| name)
                        }
                    },
                    "required": ["status", "text"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["todos"],
        "additionalProperties": false
    })
}

pub fn meta() -> ToolMeta {
    ToolMeta {
        effect: Effect::ReadOnly,
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
        writing: Some("a checklist".to_owned()),
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

fn only_fields(map: &Map<String, Value>, known: &[&str], within: &str) -> Result<(), String> {
    if map.keys().any(|k| !known.contains(&k.as_str())) {
        return Err(format!("unknown field{within}"));
    }
    Ok(())
}

fn parse_item(value: &Value, n: usize) -> Result<Todo, String> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("item {n} must be an object"))?;
    only_fields(
        map,
        &["section", "status", "text"],
        &format!(" in item {n}"),
    )?;
    let text = map
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("item {n}'s `text` must be a string"))?;
    let status = map
        .get("status")
        .and_then(Value::as_str)
        .and_then(status_named)
        .ok_or_else(|| format!("item {n}'s `status` must be {}", bad_status()))?;
    let section = match map.get("section") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name.clone()),
        Some(_) => return Err(format!("item {n}'s `section` must be a string")),
    };
    Ok(Todo {
        text: text.to_owned(),
        status,
        section,
    })
}

pub fn parse(input: &Value) -> Result<Vec<Todo>, String> {
    let map = input
        .as_object()
        .ok_or_else(|| "input must be an object".to_owned())?;
    only_fields(map, &["todos"], "")?;
    let list = map
        .get("todos")
        .and_then(Value::as_array)
        .ok_or_else(|| "`todos` must be an array".to_owned())?;
    check_count(list.len(), MIN_ITEMS)?;
    let items = list
        .iter()
        .enumerate()
        .map(|(i, item)| parse_item(item, i + 1))
        .collect::<Result<Vec<Todo>, String>>()?;
    check(&items)?;
    Ok(normal(items))
}

pub fn parse_text(input: &str) -> Result<Vec<Todo>, String> {
    let value = serde_json::from_str(input).map_err(|_| "input is not JSON".to_owned())?;
    parse(&value)
}

pub fn display(items: &[Todo]) -> Display {
    display_with_target(target(items))
}

fn display_with_target(target: String) -> Display {
    Display {
        action: Action::Other(NAME.to_owned()),
        verbs: None,
        target,
        output: OutputKind::None,
        view: None,
        title: None,
    }
}

pub fn write(items: Vec<Todo>, host: &dyn Host, ctx: &dyn CallContext) -> ToolRun {
    let line = updated(&items);
    match host.replace(items, ctx) {
        Ok(()) => ToolRun::completed().with_result(line),
        Err(message) => ToolRun::new(Outcome::Failed(Failure {
            message: message.clone(),
            before_start: false,
        }))
        .with_result(message),
    }
}

pub struct Todos {
    host: Arc<dyn Host>,
}

struct Writing(Vec<Todo>);

impl Prepared for Writing {
    fn display(&self) -> Display {
        display(&self.0)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.0)
    }
}

impl Plugin for Todos {
    fn render_section<'a>(
        &'a self,
        name: &'a str,
        at: At,
        _previous: Prior,
        _target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        Box::pin(async move {
            if name != SECTION || at != At::Head {
                return Rendered::default();
            }
            Rendered {
                snapshot: String::new(),
                text: wisp_tool_text::todos::checklist(&self.host.checklist()),
            }
        })
    }

    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { vec![def()] })
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let call: Box<dyn Prepared> = Box::new(Writing(parse_text(input)?));
            Ok(call)
        })
    }

    fn rejected_display<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        Box::pin(std::future::ready(Some(display_with_target(String::new()))))
    }

    fn time_limit_ms<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(Some(TIME_LIMIT_MS)))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            match call.into_any().downcast::<Vec<Todo>>() {
                Ok(items) => write(*items, self.host.as_ref(), ctx),
                Err(_) => ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT)
                    .with_result(wisp_tool_text::output::WRONG_INPUT),
            }
        })
    }
}

#[cfg(test)]
mod tests;
