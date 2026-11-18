use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use wisp_protocol::{AgentEvent, CacheTtl};
use wisp_render::chrome::{Endpoint, Facts, WriteMultipliers};
use wisp_render::highlight::{Grammars, Highlighter};
use wisp_render::markdown::{CodeStyle, Layout, Renderer};
use wisp_render::styled::StyledLine;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Config, Transcript, UserPart};
use wisp_tui::app::resumed::{History, Resumed};
use wisp_tui::app::view::{Paint, View};

use crate::code::Code;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

struct Pane {
    transcript: Transcript,
    code: Code,
}

impl Pane {
    fn new(grammars: Arc<Grammars>) -> Pane {
        let mut transcript = Transcript::new(Config {
            provider: "anthropic".to_owned(),
            cache_ttl: CacheTtl::OneHour,
            logging: true,
        })
        .with_code(Highlighter::pending().key());
        transcript.wait_for_code(true);
        Pane {
            transcript,
            code: Code::new(grammars),
        }
    }

    fn highlight(&mut self) {
        loop {
            let feeds = self.transcript.take_code();
            if feeds.is_empty() {
                return;
            }
            for (stream, feed) in feeds {
                for update in self.code.feed(stream, &feed) {
                    self.transcript.apply_code(stream, update);
                }
            }
            self.code.keep_live(&self.transcript);
        }
    }
}

pub(crate) struct Client {
    grammars: Arc<Grammars>,
    root: Pane,
    children: BTreeMap<String, Pane>,
    view: View,
    banner: Vec<StyledLine>,
    clock: Duration,
    user: Option<Vec<UserPart>>,
}

impl Client {
    pub(crate) fn new() -> Client {
        let grammars = Arc::new(Grammars::load().unwrap());
        Client {
            root: Pane::new(Arc::clone(&grammars)),
            grammars,
            children: BTreeMap::new(),
            view: View::new(usize::from(WIDTH)),
            banner: vec![StyledLine::default(); 3],
            clock: Duration::ZERO,
            user: None,
        }
    }

    pub(crate) fn typed(&mut self, text: &str, image: bool) {
        let mut parts = vec![UserPart::Text(text.to_owned())];
        if image {
            parts.push(UserPart::Image {
                label: "[Image 1]".to_owned(),
            });
        }
        self.user = Some(parts);
    }

    pub(crate) fn apply(&mut self, event: &AgentEvent) {
        self.clock += Duration::from_millis(1);
        match event {
            AgentEvent::Agent { agent, event } => {
                let pane = self
                    .children
                    .entry(agent.0.clone())
                    .or_insert_with(|| Pane::new(Arc::clone(&self.grammars)));
                pane.transcript.apply(event, self.clock);
            }
            _ => {
                self.root.transcript.apply(event, self.clock);
                if matches!(event, AgentEvent::TurnStarted { .. })
                    && let Some(parts) = self.user.take()
                {
                    self.root.transcript.deliver(parts, None);
                }
            }
        }
    }

    pub(crate) fn resumed(&mut self, shown: &Resumed) {
        let facts = facts();
        let mut history = History::new(GlyphSet::Unicode, shown.view.started.clone())
            .retried_on(shown.view.retried_on.clone());
        for event in &shown.view.events {
            if !history.apply(event, &facts, &mut self.root.transcript) {
                self.root.transcript.replay(event, Duration::ZERO);
            }
        }
        self.root.transcript.resume_done();
    }

    pub(crate) fn paint(&mut self) {
        self.clock += Duration::from_millis(33);
        let layout = Layout::new(usize::from(WIDTH), GlyphSet::Unicode);
        self.root.highlight();
        for pane in self.children.values_mut() {
            pane.highlight();
        }
        let _ = self
            .root
            .transcript
            .frame_at(&Renderer::new(layout), self.clock);
        self.view.sync(self.root.transcript.cells().len());
        let mut paint = Paint {
            transcript: &mut self.root.transcript,
            layout,
            banner: &self.banner,
            banner_live: false,
            caret: None,
        };
        let shown = self
            .view
            .layout(usize::from(HEIGHT.saturating_sub(4)), &mut paint);
        assert!(!shown.rows.is_empty());
    }
}

fn facts() -> Facts {
    Facts {
        version: "0.0.0".to_owned(),
        provider: "anthropic".to_owned(),
        model: wisp_core::agent::DEFAULT_MODEL.to_owned(),
        effort: "medium".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        credential: None,
        host: None,
        log: None,
        home: None,
        cwd: "~/work".to_owned(),
        endpoint: Endpoint::Official,
        models: Vec::new(),
        on_accounts: Vec::new(),
        write_multipliers: WriteMultipliers {
            prefix: 2.0,
            tail: 2.0,
            five_minutes: 1.25,
            one_hour: 2.0,
        },
        next: None,
        max_tokens: 32_000,
        endpoint_name: Some("env".to_owned()),
        endpoints: Vec::new(),
        endpoint_note: None,
        resume_refused: false,
        instructions: Default::default(),
        search: Default::default(),
        providers_off: Default::default(),
        resumed: None,
        claude_usage: false,
        start_pick: None,
    }
}
