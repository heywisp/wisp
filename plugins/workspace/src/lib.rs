use std::sync::Arc;

use wisp_plugin_api::host::{Host, Size};
use wisp_plugin_api::prompt::{At, EnvironmentFacts, Prior, Rendered};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::ENVIRONMENT_TAG;
use wisp_tool_text::agents::agent_lines_in;
use wisp_tool_text::terminal::{size_text, terminal_line};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const SECTION: &str = "environment";

pub const TERMINAL: &str = "terminal";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(WorkspacePlugin { host })
}

pub fn environment_block(facts: &EnvironmentFacts) -> String {
    let mut block = format!("{ENVIRONMENT_TAG}\nWorking directory: {}\n", facts.cwd);
    if let Some(root) = &facts.project_root {
        block.push_str(&format!("Project root: {root}\n"));
    }
    block.push_str(&format!("Platform: {}\n", facts.platform));
    let lines = facts.resources.iter().chain(&facts.tool_lines);
    for line in lines.chain(&facts.agent_lines) {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str("</environment>\n");
    block
}

pub fn render(facts: Option<EnvironmentFacts>, previous: &Prior) -> Rendered {
    let Some(facts) = facts else {
        return Rendered::default();
    };
    let text = environment_block(&facts);
    let send = match previous {
        Prior::None => true,
        Prior::Legacy(None) => false,
        Prior::Snapshot(seen) | Prior::Legacy(Some(seen)) => {
            agent_lines_in(seen) != facts.agent_lines
        }
    };
    Rendered {
        text: send.then(|| text.clone()),
        snapshot: text,
    }
}

pub fn render_terminal(size: Option<Size>, previous: &Prior) -> Rendered {
    let seen = match previous {
        Prior::Snapshot(seen) | Prior::Legacy(Some(seen)) => Some(seen.as_str()),
        Prior::None | Prior::Legacy(None) => None,
    };
    let Some(size) = size else {
        return Rendered {
            snapshot: seen.unwrap_or_default().to_owned(),
            text: None,
        };
    };
    let snapshot = size_text(size);
    Rendered {
        text: (seen != Some(snapshot.as_str())).then(|| terminal_line(size)),
        snapshot,
    }
}

pub struct WorkspacePlugin {
    host: Arc<dyn Host>,
}

impl Plugin for WorkspacePlugin {
    fn render_section<'a>(
        &'a self,
        name: &'a str,
        _at: At,
        previous: Prior,
        _target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        let rendered = match name {
            SECTION => render(self.host.environment(), &previous),
            TERMINAL => render_terminal(self.host.terminal(), &previous),
            _ => Rendered::default(),
        };
        Box::pin(async move { rendered })
    }
}

#[cfg(test)]
mod tests;
