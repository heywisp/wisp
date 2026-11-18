use std::sync::Arc;

use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::{Fill, Host};
use wisp_plugin_api::slots::{Block, Content, SlotView, Span};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::CONTEXT_PREFIX;
use wisp_tool_text::agents::tokens;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const HOOK: &str = "fill";

pub const AUTO_COMPACT_OFF: &str = "Automatic compaction is off.";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(ContextNote { host })
}

pub fn context_line(estimate: u64, window: Option<u64>, auto_point: Option<u64>) -> String {
    match window.filter(|w| *w > 0) {
        Some(window) => {
            let share = u128::from(estimate) * 100 / u128::from(window);
            let second = match auto_point {
                Some(point) => format!("Wisp compacts at {}.", tokens(point)),
                None => AUTO_COMPACT_OFF.to_owned(),
            };
            format!(
                "{CONTEXT_PREFIX}{} of {} tokens ({share}%). {second}\n",
                tokens(estimate),
                tokens(window)
            )
        }
        None => format!(
            "{CONTEXT_PREFIX}{} tokens; window unknown.\n",
            tokens(estimate)
        ),
    }
}

pub fn fill_line(fill: &Fill) -> String {
    context_line(
        fill.estimate,
        fill.window,
        fill.point.filter(|_| fill.auto_compact),
    )
}

pub struct ContextNote {
    host: Arc<dyn Host>,
}

impl Plugin for ContextNote {
    fn on_hook<'a>(&'a self, hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        let reply = match (hook, event) {
            (HOOK, Event::BeforeTurn(_)) => {
                Reply::Append(vec![fill_line(&self.host.current_fill())])
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    }

    fn render_slot<'a>(
        &'a self,
        _name: &'a str,
        _view: SlotView,
    ) -> BoxFuture<'a, Result<Content, String>> {
        let fill = self.host.current_fill();
        let content = Content {
            note: vec![Span::Fill(fill.clone())],
            blocks: vec![Block::Fill(fill)],
        };
        Box::pin(async move { Ok(content) })
    }
}

#[cfg(test)]
mod tests;
