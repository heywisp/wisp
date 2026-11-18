pub mod hooks;
pub mod host;
mod tools;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;
use wisp_plugin_api::commands::CommandReply;
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::prompt::{At, Prior, Rendered};
use wisp_plugin_api::slots::{Content, SlotView};
use wisp_plugin_api::tools::{BuildContext, Prepared, ToolDef};
use wisp_plugin_api::types::{Action, Display, OutputKind, ToolRun, Verbs};
use wisp_plugin_api::{Bundled, Plugin};
use wisp_tool_text::output::cut_chars;

pub use tools::{PDF, answered, png, question};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const ID: &str = "testkit";

pub const BLOCK_INPUT: &str = "block-input";

const BLOCK_INPUT_MANIFEST: &str = concat!(
    include_str!("../wisp-plugin.toml"),
    "\n[[hooks]]\nname = \"block-input\"\npoint = \"user-input\"\n"
);

const SIDEBAR_MANIFEST: &str = concat!(
    include_str!("../wisp-plugin.toml"),
    "\n[[slots]]\nslot = \"sidebar\"\nname = \"busy\"\ntitle = \"Busy\"\ndefault = \"off\"\n",
    "marks = [\"after-turn\", \"before-request\", \"after-response\"]\n"
);

const SECTIONS_MANIFEST: &str = concat!(
    include_str!("../wisp-plugin.toml"),
    "\n[[prompt_sections]]\nname = \"notes\"\nplace = \"lead\"\n"
);

pub const SETTING_ROW: &str = "[[settings]]\nname = \"minutes\"\nlabel = \"Minutes\"\nhelp = \"Minutes before away.\"\nkind = \"int\"\nmin = 1\nmax = 60\ndefault = 10\n";

pub fn with_settings(manifest: &str) -> String {
    format!(
        "{}\n{SETTING_ROW}",
        manifest.replace("world = \"0.2.0\"", "world = \"0.3.0\"")
    )
}

static SETTINGS_MANIFEST: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| with_settings(MANIFEST));

pub fn settings() -> Bundled {
    Bundled {
        manifest: &SETTINGS_MANIFEST,
        instantiate,
    }
}

pub const NOTES: &str = "testkit notes";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

pub fn block_input() -> Bundled {
    Bundled {
        manifest: BLOCK_INPUT_MANIFEST,
        instantiate: instantiate_block_input,
    }
}

pub fn sidebar() -> Bundled {
    Bundled {
        manifest: SIDEBAR_MANIFEST,
        instantiate: instantiate_sidebar,
    }
}

pub fn sections() -> Bundled {
    Bundled {
        manifest: SECTIONS_MANIFEST,
        instantiate: instantiate_sections,
    }
}

fn instantiate_sections(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Testkit {
        section: true,
        ..Testkit::new(host)
    })
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Testkit::new(host))
}

fn instantiate_sidebar(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Testkit {
        sidebar: true,
        ..Testkit::new(host)
    })
}

fn instantiate_block_input(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Testkit {
        blocks_input: true,
        ..Testkit::new(host)
    })
}

pub struct Testkit {
    host: Arc<dyn Host>,
    limits: BTreeMap<String, u64>,
    blocks_input: bool,
    sidebar: bool,
    section: bool,
}

impl Testkit {
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self {
            host,
            limits: BTreeMap::new(),
            blocks_input: false,
            sidebar: false,
            section: false,
        }
    }

    #[must_use]
    pub fn with_limit(mut self, tool: &str, ms: u64) -> Self {
        self.limits.insert(tool.to_owned(), ms);
        self
    }
}

pub struct Call<T> {
    display: Display,
    value: T,
}

impl<T: Send + 'static> Call<T> {
    pub fn boxed(display: Display, value: T) -> Box<dyn Prepared> {
        Box::new(Self { display, value })
    }

    pub fn take(call: Box<dyn Prepared>) -> Option<T> {
        call.into_any().downcast::<T>().ok().map(|b| *b)
    }
}

impl<T: Send + 'static> Prepared for Call<T> {
    fn display(&self) -> Display {
        self.display.clone()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.value)
    }
}

pub fn verbs() -> Verbs {
    Verbs {
        doing: "Brewing".to_owned(),
        done: "Brewed".to_owned(),
        failed: "couldn't brew".to_owned(),
    }
}

pub fn parse(input: &str) -> Result<Value, String> {
    serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))
}

pub fn fallback_display(name: &str, input: &str) -> Display {
    let keys = parse(input)
        .ok()
        .and_then(|v| v.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()))
        .unwrap_or_default();
    Display {
        action: Action::Other(name.to_owned()),
        verbs: None,
        target: cut_chars(&format!("{{{}}}", keys.join(", ")), 80),
        output: OutputKind::Text,
        view: None,
        title: None,
    }
}

impl Plugin for Testkit {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        let defs = tools::defs(&self.limits);
        Box::pin(async move { defs })
    }

    fn prepare<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(std::future::ready(tools::prepare(tool, input)))
    }

    fn time_limit_ms<'a>(&'a self, tool: &'a str, input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(tools::time_limit_ms(
            tool,
            input,
            &self.limits,
        )))
    }

    fn movable<'a>(&'a self, tool: &'a str, _input: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(tool == "stubborn"))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(tools::run(self.host.as_ref(), call, ctx))
    }

    fn run_command<'a>(&'a self, name: &'a str, args: &'a str) -> BoxFuture<'a, CommandReply> {
        let reply = match name {
            "note" => CommandReply::Note(args.to_owned()),
            "send" => CommandReply::Send(args.to_owned()),
            _ => CommandReply::None,
        };
        Box::pin(async move { reply })
    }

    fn on_hook<'a>(&'a self, hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        let reply = match event {
            Event::UserInput(input)
                if self.blocks_input && hook == BLOCK_INPUT && input.text.starts_with("block") =>
            {
                Reply::Block("the message starts with block".to_owned())
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    }

    fn render_slot<'a>(
        &'a self,
        name: &'a str,
        view: SlotView,
    ) -> BoxFuture<'a, Result<Content, String>> {
        if self.sidebar {
            return hooks::busy()(self.host.clone(), name.to_owned(), view);
        }
        Box::pin(async { Ok(Content::default()) })
    }

    fn render_section<'a>(
        &'a self,
        name: &'a str,
        _at: At,
        previous: Prior,
        _target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        let text = (self.section && previous != Prior::Snapshot(NOTES.to_owned()))
            .then(|| hooks::section_text(name, NOTES));
        let snapshot = if self.section {
            NOTES.to_owned()
        } else {
            String::new()
        };
        Box::pin(async move { Rendered { snapshot, text } })
    }
}
