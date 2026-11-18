use std::any::Any;
use std::sync::Arc;

use serde_json::{Value, json};
use wisp_plugin_api::host::{CallContext, Host, ScreenReport};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{Action, Display, OutputKind, ToolRun};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::output::{WRONG_INPUT, cut_chars, group};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const NAME: &str = "show_page";
pub const MAX_BYTES: usize = 16 * 1024;
pub const MAX_PART_CHARS: usize = 64;
pub const TELL: &str = "tell";
pub const SOUND: &str = "sound";
pub const SHOWN: &str = "Shown on the board";
pub const NO_SCREEN: &str = "no screen answered";
pub const REPORT_FIELDS: [&str; 4] = ["width", "height", "zoom", "errors"];
pub const MAX_ERRORS: usize = 5;
pub const MAX_ERROR_CHARS: usize = 200;
const TIME_LIMIT_MS: u64 = 5_000;
const REPORT_WAIT_MS: u64 = 4_000;
const WHOLE_PAGE: &str = "page";

fn description() -> String {
    format!(
        "Show a web page on the board, the screen the user sees beside \
the chat. Without `part`, `html` is a whole page and replaces the board. With `part`, `html` \
becomes the content of the element whose id is `part`, or a new block with that id added at the \
end of the page, so a page grows without being sent again; scripts in it run.\n\nThe page has no \
network: put every script, style, image and font inline (images and fonts as data: URLs) and \
link to nothing outside. A script on the page can call {TELL}(text) to send one short line back, \
such as which answer was tapped. Limits: `html` up to {} KiB; `part` 1 to {MAX_PART_CHARS} letters, digits, - \
and _, starting with a letter.",
        MAX_BYTES / 1024
    )
}

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(ShowPage { host })
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "html": {
                "type": "string",
                "description": "The whole page, or the part's content when part is given."
            },
            "part": {
                "type": "string",
                "description": "The id of the part to fill or add; leave it out to replace the whole page."
            }
        },
        "required": ["html"],
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
        writing: Some("a page".to_owned()),
        purpose: Purpose::Show,
    }
}

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description(),
            input_schema: schema().to_string(),
            meta: meta(),
        }),
        system_line: None,
        context_line: None,
    }
}

fn valid_part(part: &str) -> bool {
    let mut chars = part.chars();
    part.len() <= MAX_PART_CHARS
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub part: Option<String>,
}

pub fn parse(input: &str) -> Result<Page, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))?;
    let map = value.as_object().ok_or("input must be an object")?;
    if let Some(key) = map.keys().find(|k| !matches!(k.as_str(), "html" | "part")) {
        return Err(format!("unknown field `{key}`"));
    }
    let html = map
        .get("html")
        .and_then(Value::as_str)
        .ok_or("`html` must be a string")?;
    if html.trim().is_empty() {
        return Err("`html` is empty".to_owned());
    }
    if html.len() > MAX_BYTES {
        return Err(format!(
            "`html` is {} bytes; the limit is {} bytes ({} KiB), so show the page in parts",
            group(html.len() as u64),
            group(MAX_BYTES as u64),
            MAX_BYTES / 1024
        ));
    }
    let part = match map.get("part") {
        None => None,
        Some(Value::String(part)) if valid_part(part) => Some(part.clone()),
        Some(_) => {
            return Err(format!(
                "`part` must be an id of 1 to {MAX_PART_CHARS} letters, digits, - and _, starting with a letter"
            ));
        }
    };
    Ok(Page { part })
}

impl Prepared for Page {
    fn display(&self) -> Display {
        Display {
            action: Action::Show,
            verbs: None,
            target: self.part.as_deref().unwrap_or(WHOLE_PAGE).to_owned(),
            output: OutputKind::None,
            view: None,
            title: None,
        }
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

pub fn answer(report: Option<&ScreenReport>) -> String {
    let Some(report) = report else {
        return format!("{SHOWN}; {NO_SCREEN}.");
    };
    let report: Value = serde_json::from_str(&report.report).unwrap_or_default();
    let [width, height, zoom, errors] = REPORT_FIELDS.map(|field| &report[field]);
    let mut answer = SHOWN.to_owned();
    if let (Some(width), Some(height)) = (width.as_f64(), height.as_f64()) {
        answer.push_str(&format!(", {} × {} px", width.round(), height.round()));
    }
    answer.push('.');
    if let Some(zoom) = zoom.as_f64().filter(|zoom| *zoom < 1.0) {
        answer.push_str(&format!(" Scaled to {} % to fit.", (zoom * 100.0).floor()));
    }
    let errors: Vec<String> = errors
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(MAX_ERRORS)
        .map(|error| cut_chars(error, MAX_ERROR_CHARS))
        .collect();
    if !errors.is_empty() {
        answer.push_str(&format!(" Script errors: {}.", errors.join("; ")));
    }
    answer
}

pub struct ShowPage {
    host: Arc<dyn Host>,
}

impl Plugin for ShowPage {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { vec![def()] })
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let page: Box<dyn Prepared> = Box::new(parse(input)?);
            Ok(page)
        })
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
            if call.into_any().downcast::<Page>().is_err() {
                return ToolRun::failed_to_start(WRONG_INPUT);
            }
            let report = self.host.wait(ctx.id(), REPORT_WAIT_MS).await;
            let mut run = ToolRun::completed().with_result(answer(report.as_ref()));
            run.attachments = report.map(|report| report.images).unwrap_or_default();
            run
        })
    }
}

#[cfg(test)]
mod tests;
