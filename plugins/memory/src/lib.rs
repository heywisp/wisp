use std::future::ready;
use std::sync::Arc;

use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::prompt::{At, Prior, Rendered};
use wisp_plugin_api::tools::{BuildContext, Prepared, ToolDef};
use wisp_plugin_api::types::{Display, ToolRun};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};

mod lead;
mod note;
mod store;
mod tool;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate: |host| Arc::new(Memory(host)),
    }
}

struct Memory(Arc<dyn Host>);

impl Plugin for Memory {
    fn describe<'a>(&'a self, _: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(ready(vec![tool::definition()]))
    }

    fn prepare<'a>(
        &'a self,
        _: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(ready(tool::parse(input)))
    }

    fn rejected_display<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Option<Display>> {
        Box::pin(ready(Some(tool::fixed_display())))
    }

    fn run<'a>(
        &'a self,
        prepared: Box<dyn Prepared>,
        _: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move { tool::run(self.0.as_ref(), prepared) })
    }

    fn render_section<'a>(
        &'a self,
        name: &'a str,
        _: At,
        previous: Prior,
        target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        Box::pin(async move {
            if name == lead::NAME {
                lead::render(self.0.as_ref(), previous, target)
            } else {
                Rendered::default()
            }
        })
    }
}

#[cfg(test)]
mod tests;
