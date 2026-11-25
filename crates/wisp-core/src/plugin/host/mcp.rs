use std::time::Duration;

use wisp_plugin_api::mcp::{Mcp, Prompt, PromptMessage, Resource, Server, State, Tool};
use wisp_plugin_api::types::{Attachment, Completed, Outcome, ToolRun};

use super::*;
use crate::config::McpServer;
use crate::policy::{HostCall, Verdict};

const SIGN_IN_DENIED: &str = "an installed plugin can't sign in to MCP servers";

impl Mcp for PluginHost {
    fn prompts(&self) -> Vec<Prompt> {
        if self.refuses(Import::McpPrompts) {
            return Vec::new();
        }
        self.kernel.mcp().prompts()
    }

    fn get_prompt<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PromptMessage>, String>> {
        Box::pin(async move {
            if self.refuses(Import::McpGetPrompt) {
                return Err(in_render());
            }
            self.kernel.mcp().get_prompt(name).await
        })
    }

    fn resources(&self) -> Vec<Resource> {
        if self.refuses(Import::McpResources) {
            return Vec::new();
        }
        self.kernel.mcp().resources()
    }

    fn read_resource<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Option<String>>, String>> {
        Box::pin(async move {
            if self.refuses(Import::McpReadResource) {
                return Err(in_render());
            }
            self.kernel.mcp().read_resource(name).await
        })
    }

    fn servers(&self) -> Vec<Server> {
        if self.refuses(Import::McpServers) {
            return Vec::new();
        }
        self.kernel
            .mcp()
            .rows()
            .into_iter()
            .map(|row| Server {
                listed: self.kernel.mcp().saved(&row.name).is_some(),
                off: row.off,
                fresh: row.fresh,
                name: row.name,
                tools: u32::try_from(row.tools).unwrap_or(u32::MAX),
                state: match row.state {
                    crate::mcp::State::Stopped => State::Stopped,
                    crate::mcp::State::Starting => State::Starting,
                    crate::mcp::State::Ready => State::Ready,
                    crate::mcp::State::NeedsSignIn => State::NeedsSignIn,
                    crate::mcp::State::NeedsKey(keys) => State::NeedsKey(keys),
                    crate::mcp::State::Failed(error) => State::Failed(error),
                },
            })
            .collect()
    }

    fn tools(&self) -> Vec<Tool> {
        if self.refuses(Import::McpTools) {
            return Vec::new();
        }
        self.kernel.mcp().definitions()
    }

    fn revision(&self) -> u64 {
        if self.refuses(Import::McpRevision) {
            return 0;
        }
        self.kernel.mcp().revision()
    }

    fn connect<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if self.refuses(Import::McpConnect) {
                return Err(in_render());
            }
            self.kernel.mcp().connect(name).await
        })
    }

    fn call<'a>(
        &'a self,
        ctx: &'a dyn CallContext,
        name: &'a str,
        input: &'a str,
        limit_ms: u64,
    ) -> BoxFuture<'a, Result<ToolRun, String>> {
        Box::pin(async move {
            if self.refuses(Import::McpCall) {
                return Err(in_render());
            }
            let input =
                serde_json::from_str(input).map_err(|_| "MCP tool arguments must be JSON")?;
            let service = self.kernel.mcp();
            let call = self
                .kernel
                .call(&ctx.id())
                .filter(|call| call.plugin == self.id);
            let child = locked(&self.kernel.index).managing.child;
            let user = call
                .as_ref()
                .filter(|_| !child)
                .map(|call| call.user.clone())
                .unwrap_or_default();
            let cancel = tokio_util::sync::CancellationToken::new();
            let run = tokio::select! {
                result = service.call_asking(name, input, Duration::from_millis(limit_ms), cancel, user) => result.map_err(|error| {
                    if child && error == wisp_tool_text::mcp::INPUT_REQUIRED {
                        let server = service.definitions().into_iter().find(|tool| tool.name == name).map(|tool| tool.server).unwrap_or_default();
                        wisp_tool_text::mcp::child_question(&server, name)
                    } else { error }
                })?,
                () = ctx.wait_cancelled() => return Err(wisp_tool_text::mcp::CALL_CANCELLED.into()),
            };
            let outcome = match run.outcome {
                wisp_protocol::ToolOutcome::Completed { is_error, .. } => {
                    Outcome::Completed(Completed {
                        is_error,
                        exit: None,
                    })
                }
                wisp_protocol::ToolOutcome::Failed {
                    message,
                    before_start,
                } => Outcome::Failed(wisp_plugin_api::types::Failure {
                    message,
                    before_start,
                }),
                _ => return Err("the MCP call did not complete".into()),
            };
            let mut result = ToolRun::new(outcome);
            result.result = run.result;
            result.attachments = run
                .attachments
                .into_iter()
                .map(|attachment| Attachment {
                    media_type: attachment.media_type.to_string(),
                    bytes: attachment.bytes,
                    slice: None,
                })
                .collect();
            Ok(result)
        })
    }

    fn add<'a>(
        &'a self,
        name: &'a str,
        command: &'a str,
        args: &'a [String],
    ) -> BoxFuture<'a, Result<u32, String>> {
        Box::pin(async move {
            if self.refuses(Import::McpAdd) {
                return Err(in_render());
            }
            if self.origin != Origin::Bundled {
                return Err("an installed plugin can't add MCP servers".into());
            }
            self.add_server(name, command, args, None).await
        })
    }

    fn remove<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if self.refuses(Import::McpRemove) {
                return Err(in_render());
            }
            if self.origin != Origin::Bundled {
                return Err("an installed plugin can't remove MCP servers".into());
            }
            self.kernel.mcp().remove(name).await
        })
    }
    fn sign_in_start<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            if self.refuses(Import::McpSignInStart) {
                return Err(in_render());
            }
            if self.origin != Origin::Bundled {
                return Err(SIGN_IN_DENIED.into());
            }
            let link = self.kernel.mcp().sign_in_start(name).await?;
            if let Some(link) = link
                .as_ref()
                .and_then(|link| crate::mcp::link_note(name, link.clone(), false))
            {
                self.kernel.show_link(link);
            }
            Ok(link)
        })
    }

    fn sign_in_finish<'a>(
        &'a self,
        name: &'a str,
        address: &'a str,
    ) -> BoxFuture<'a, Result<u32, String>> {
        Box::pin(async move {
            if self.refuses(Import::McpSignInFinish) {
                return Err(in_render());
            }
            if self.origin != Origin::Bundled {
                return Err(SIGN_IN_DENIED.into());
            }
            self.kernel.mcp().sign_in_finish(name, address).await
        })
    }
}

impl PluginHost {
    pub(super) async fn add_server(
        &self,
        name: &str,
        command: &str,
        args: &[String],
        by: Option<&str>,
    ) -> Result<u32, String> {
        self.check_server(name, command, args)?;
        self.kernel
            .mcp()
            .add_by(
                name,
                McpServer {
                    keys: Vec::new(),
                    command: command.into(),
                    args: args.to_vec(),
                },
                by,
            )
            .await
    }
    pub(super) fn check_server(
        &self,
        name: &str,
        command: &str,
        args: &[String],
    ) -> Result<(), String> {
        crate::mcp::check_add(name, command, args)?;
        let target = std::iter::once(command)
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        match self.kernel.snapshot().gate.check_host(&HostCall {
            plugin: &self.id,
            capability: if crate::mcp::is_address(command) {
                Capability::Net
            } else {
                Capability::Exec
            },
            target: &target,
        }) {
            Verdict::Allow => {}
        }
        Ok(())
    }
}
