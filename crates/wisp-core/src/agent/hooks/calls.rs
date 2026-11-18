use serde_json::Value;
use tokio::sync::mpsc;
use wisp_plugin_api::hooks::{Event, ToolCall, ToolResult};
use wisp_protocol::{AgentEvent, ClientCommand, HookChange, NotRunReason, ToolOutcome};
use wisp_tool_text::output::invalid_input_text;

use super::super::{Agent, Finished, not_run_text};
use crate::plugin::hooks::points::{AFTER_TOOL, BEFORE_TOOL};
use crate::plugin::hooks::{Fire, What};
use crate::tool::Prepared;

pub(in crate::agent) enum Before {
    Run(Value, Prepared),
    Settled(Finished),
}

pub(in crate::agent) fn with_lines(text: &str, lines: &[String]) -> String {
    if lines.is_empty() {
        return text.to_owned();
    }
    format!("{text}\n{}", lines.join("\n"))
}

fn as_text(input: &Value) -> String {
    serde_json::to_string(input).unwrap_or_default()
}

pub(in crate::agent) fn blocked_text(hook: &str, reason: &str) -> String {
    format!("Blocked by {hook}: {reason}")
}

impl Agent {
    fn call_at(&mut self, call_id: &str) -> crate::plugin::caller::At {
        let step = self.track.as_ref().map(|t| t.step);
        self.at(step, Some(call_id.to_owned()))
    }

    pub(in crate::agent) async fn before_tool(
        &mut self,
        (call_id, name, model_input, forwarded): (&str, &str, &Value, bool),
        prepared: Prepared,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Before {
        if !self.hooks.engine.attached(BEFORE_TOOL) {
            return Before::Run(model_input.clone(), prepared);
        }
        let event = Event::BeforeTool(ToolCall {
            call_id: call_id.to_owned(),
            tool: name.to_owned(),
            input: as_text(model_input),
        });
        let at = self.call_at(call_id);
        let fire = Fire::new(BEFORE_TOOL, What::Event(event), at.clone());
        let progress = self.await_hooks(fire, commands, events).await;
        if self.track().stopped {
            let reason = NotRunReason::TurnStopped;
            let text = not_run_text(&reason);
            return Before::Settled(Finished::synthetic(ToolOutcome::NotRun { reason }, text));
        }
        if let Some((hook, reason)) = progress.blocked {
            let message = blocked_text(&hook, &reason);
            let outcome = ToolOutcome::Failed {
                message: message.clone(),
                before_start: true,
            };
            let mut finished = Finished::synthetic(outcome, message.clone());
            finished.result = Some(message);
            return Before::Settled(finished);
        }
        let Some(last) = progress.changes.last().map(|c| c.hook.clone()) else {
            return Before::Run(model_input.clone(), prepared);
        };
        let parse = |text: &str| serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
        let input = progress
            .value
            .as_deref()
            .map_or_else(|| model_input.clone(), parse);
        for change in progress.changes {
            let applied = AgentEvent::HookApplied {
                hook: change.hook,
                point: BEFORE_TOOL.to_owned(),
                turn: at.turn,
                step: at.step,
                call_id: at.call_id.clone(),
                input_id: None,
                change: HookChange::Input {
                    before: parse(&change.before),
                    after: parse(&change.after),
                },
            };
            self.emit(events, applied).await;
        }
        let prepared = if forwarded {
            self.prepare_runnable(name, &input).await
        } else {
            let sent = self.track().sent_search;
            self.prepare_call(name, &input, sent).await
        };
        match prepared {
            Ok(prepared) => Before::Run(input, prepared),
            Err(e) => {
                let message = format!("{last} rewrote the input: {}", e.message);
                let text = invalid_input_text(&message);
                Before::Settled(Finished::synthetic(
                    ToolOutcome::InvalidInput { message },
                    text,
                ))
            }
        }
    }

    pub(in crate::agent) async fn after_tool(
        &mut self,
        (call_id, name, input): (&str, &str, &Value),
        finished: &mut Finished,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Vec<AgentEvent> {
        let lines = std::mem::take(&mut finished.ends);
        let mut applied = Vec::new();
        let fires = finished.ran && self.hooks.engine.attached(AFTER_TOOL);
        if fires && !self.track().stopped {
            let event = Event::AfterTool(ToolResult {
                call_id: call_id.to_owned(),
                tool: name.to_owned(),
                input: as_text(input),
                result: finished.text.clone(),
                is_error: finished.outcome.is_error(),
            });
            let at = self.call_at(call_id);
            let fire = Fire::new(AFTER_TOOL, What::Event(event), at.clone());
            let progress = self.await_hooks(fire, commands, events).await;
            if !self.track().stopped && !progress.changes.is_empty() {
                if let Some(value) = progress.value {
                    finished.text = value;
                }
                applied = progress
                    .changes
                    .into_iter()
                    .map(|change| AgentEvent::HookApplied {
                        hook: change.hook,
                        point: AFTER_TOOL.to_owned(),
                        turn: at.turn,
                        step: at.step,
                        call_id: at.call_id.clone(),
                        input_id: None,
                        change: HookChange::Result {
                            before: with_lines(&change.before, &lines),
                            after: with_lines(&change.after, &lines),
                        },
                    })
                    .collect();
            }
        }
        finished.text = with_lines(&finished.text, &lines);
        applied
    }
}
