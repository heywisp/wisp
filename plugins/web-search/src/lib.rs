use std::any::Any;
use std::sync::Arc;

use serde::Deserialize;
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::model::{AskError, Prompt};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{Action, Display, Outcome, OutputKind, ToolRun};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::search::{INSTRUCTION, MAX_TOKENS, REPORT, call_query, numbered, schema};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");
pub const NAME: &str = "web_search";
pub const FAMILY: &str = "web_search";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(WebSearch { host })
}

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: format!("Search the web. {REPORT}"),
            input_schema: schema(&call_query()),
            meta: ToolMeta {
                effect: Effect::ReadOnly,
                tags: Tags {
                    reads_untrusted: true,
                    network: true,
                    ..Tags::default()
                },
                parallel_safe: true,
                timeouts: Timeouts {
                    default_ms: 300_000,
                    max_ms: Some(300_000),
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
struct Query {
    query: String,
}

impl Prepared for Query {
    fn display(&self) -> Display {
        Display {
            action: Action::Search,
            verbs: None,
            target: format!("\"{}\"", self.query),
            output: OutputKind::Text,
            view: None,
            title: None,
        }
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

pub struct WebSearch {
    host: Arc<dyn Host>,
}

impl Plugin for WebSearch {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { vec![def()] })
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let query: Query =
                serde_json::from_str(input).map_err(|_| "query must be a string".to_owned())?;
            if query.query.trim().is_empty() {
                return Err("query must not be empty".to_owned());
            }
            Ok(Box::new(query) as Box<dyn Prepared>)
        })
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            let Ok(query) = call.into_any().downcast::<Query>() else {
                return ToolRun::failed_to_start("the search query is unavailable");
            };
            let prompt = Prompt {
                system: INSTRUCTION.to_owned(),
                text: query.query,
                max_tokens: MAX_TOKENS,
                native: Some(FAMILY.to_owned()),
            };
            match self.host.model().ask(prompt, ctx).await {
                Ok(reply) => {
                    let text = if reply.sources.is_empty() {
                        "no results".to_owned()
                    } else {
                        format!(
                            "{}\n\n{}",
                            reply.text,
                            numbered(
                                reply
                                    .sources
                                    .iter()
                                    .map(|source| (source.title.as_str(), source.url.as_str()))
                            )
                        )
                    };
                    ToolRun::completed().with_result(text)
                }
                Err(AskError::Cancelled) => {
                    ToolRun::new(Outcome::Cancelled).with_result("web search cancelled")
                }
                Err(
                    AskError::NotAllowed(reason)
                    | AskError::Refused(reason)
                    | AskError::Failed(reason),
                ) => {
                    let text = format!("web search failed: {reason}");
                    ToolRun::failed_to_start(&text).with_result(text)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
