use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use wisp_protocol::settings::{
    AppliedChange, CACHE_KEPT, FileState, Layer, Place, ResolvedSetting, Saved, SavedValue,
    SettingChange, SettingName, SettingTarget, SettingValue, SettingsSnapshot, Shadowed, SitePath,
    SnapshotContext, When, find,
};
use wisp_protocol::{AgentEvent, CacheEffect, ClientCommand, ErrorKind, RequestId, RetryFocus};
use wisp_render::chrome::endpoint_card::ADD_TITLE;
use wisp_render::chrome::{Endpoint, EndpointFacts};
use wisp_render::motion::MotionPolicy;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Theme};
use wisp_tui::app::config_screen::{Display, STOP_KEY, placed};
use wisp_tui::app::endpoint_card::CardSetup;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::replay;
use wisp_tui::app::{Options, Session, run};
use wisp_tui::term::{Input, Key, KeyCode, Mouse, MouseKind};

use super::Screen;
use crate::events::{Changed, End, Ending, settings_changed, turn_ended};
use crate::live_display::value_of;
use crate::retry::endpoint;
use crate::shared::{catalog_tool, change, hook_entry, named, offer, switch_placed};
use crate::{Drive, eventually, shows};

#[derive(Clone, Copy, Debug)]
struct Look {
    mode: Mode,
    glyphs: GlyphSet,
    cols: u16,
    rows: u16,
}

impl Look {
    const fn dark(cols: u16, rows: u16) -> Look {
        Look {
            mode: Mode::Dark,
            glyphs: GlyphSet::Unicode,
            cols,
            rows,
        }
    }

    fn name(self) -> String {
        let mode = match self.mode {
            Mode::Dark => "dark",
            Mode::Light => "light",
        };
        let glyphs = match self.glyphs {
            GlyphSet::Unicode => "unicode",
            GlyphSet::AmbiguousWide => "ambiguous",
            GlyphSet::Ascii => "ascii",
        };
        format!("{mode}-{glyphs}-{}x{}", self.cols, self.rows)
    }
}

struct Stage {
    screen: Screen,
    inputs: mpsc::UnboundedSender<Input>,
    events: mpsc::Sender<AgentEvent>,
    commands: mpsc::Receiver<ClientCommand>,
    skip_usage: bool,
}

impl Stage {
    async fn close_settings(&self) {
        while on_screen(&self.screen.visible()) {
            let before = self.screen.steady();
            self.key(KeyCode::Esc);
            self.changed_from(&before).await;
        }
    }

    fn shifted(&self, code: KeyCode) -> &Stage {
        self.input(Input::Key(Key {
            shift: true,
            ..Key::plain(code)
        }))
    }

    fn char(&self, c: char) -> &Stage {
        self.key(KeyCode::Char(c))
    }

    fn click(&self, column: u16, row: u16) -> &Stage {
        self.input(Input::Mouse(Mouse {
            kind: MouseKind::Press,
            column,
            row,
            modified: false,
        }))
        .input(Input::Mouse(Mouse {
            kind: MouseKind::Release,
            column,
            row,
            modified: false,
        }))
    }

    fn wheel(&self, kind: MouseKind) -> &Stage {
        self.input(Input::Mouse(Mouse {
            kind,
            column: 4,
            row: 4,
            modified: false,
        }))
    }

    async fn emit(&self, event: AgentEvent) {
        self.events.send(event).await.unwrap();
        let taken = eventually(|| self.events.capacity() == self.events.max_capacity()).await;
        assert!(taken, "the app never took the event");
    }

    async fn next(&mut self) -> ClientCommand {
        loop {
            let command = timeout(PATIENCE, self.commands.recv())
                .await
                .unwrap_or_else(|_| panic!("no command within the patience"))
                .unwrap();
            if !self.skip_usage || !matches!(command, ClientCommand::PlanUsage { .. }) {
                return command;
            }
        }
    }

    async fn nothing_sent(&mut self) {
        assert!(
            timeout(Duration::from_millis(30), self.next())
                .await
                .is_err()
        );
    }

    async fn asked(&mut self) -> RequestId {
        match self.next().await {
            ClientCommand::DescribeSettings { request } => request,
            other => panic!("expected DescribeSettings, got {other:?}"),
        }
    }

    async fn open(&mut self, line: &str, answer: fn(RequestId) -> SettingsSnapshot) {
        self.typed(line).key(KeyCode::Enter);
        let request = self.asked().await;
        self.until(loading).await;
        self.emit(AgentEvent::SettingsSnapshot(answer(request)))
            .await;
        self.until(loaded).await;
    }

    async fn until(&self, done: impl Fn(&[String]) -> bool) -> Vec<String> {
        let mut rows = Vec::new();
        let held = eventually(|| {
            rows = self.screen.visible();
            done(&rows)
        })
        .await;
        assert!(held, "never reached: {rows:#?}");
        rows
    }

    async fn unchanged_for_a_while(&self) -> Vec<String> {
        sleep(Duration::from_millis(80)).await;
        self.screen.visible()
    }

    async fn changed_from(&self, before: &[String]) -> Vec<String> {
        let mut rows = Vec::new();
        let held = eventually(|| {
            rows = self.screen.steady();
            rows != before
        })
        .await;
        assert!(held, "never changed: {rows:#?}");
        rows
    }

    async fn shot_when(&self, done: impl Fn(&[String]) -> bool) -> String {
        self.until(done).await;
        self.screen.styled()
    }

    async fn quit(&mut self) {
        let last = timeout(PATIENCE, self.commands.recv())
            .await
            .unwrap_or_else(|_| panic!("the app never let go of its commands"));
        assert!(last.is_none(), "{last:?}");
    }
}

impl Drive for Stage {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input> {
        &self.inputs
    }
}

const PATIENCE: Duration = Duration::from_secs(10);

fn loading(rows: &[String]) -> bool {
    shows(rows, "Loading settings")
}

fn loaded(rows: &[String]) -> bool {
    on_screen(rows) && !loading(rows)
}

fn on_screen(rows: &[String]) -> bool {
    rows.first().is_some_and(|r| r.contains("wisp   Settings"))
}

fn footer(rows: &[String]) -> &str {
    rows.last().map_or("", String::as_str)
}

fn ready(rows: &[String]) -> bool {
    shows(rows, "enter send")
}

fn display() -> Display {
    let mut out = Display::default();
    out.set(
        "display.theme",
        SettingValue::Text("dark".to_owned()),
        "OSC 11 background #101010, OKLab L 0.15",
    );
    out.set(
        "display.color",
        SettingValue::Text("truecolor".to_owned()),
        "COLORTERM=truecolor",
    );
    out.set(
        "display.motion",
        SettingValue::Text("reduced".to_owned()),
        "WISP_MOTION=reduced",
    );
    out.set(
        "display.ambiguous",
        SettingValue::Text("narrow".to_owned()),
        "default",
    );
    out.set("display.mouse", SettingValue::Bool(true), "default");
    out.set("display.scroll_speed", SettingValue::Int(1), "default");
    out.set(
        "display.full_repaint",
        SettingValue::Bool(false),
        "default: only changed cells are written",
    );
    out.set(
        "display.copy_on_select",
        SettingValue::Bool(true),
        "default",
    );
    out
}

async fn staged<D, F>(look: Look, drive: D)
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    staged_with(look, display(), drive).await;
}

async fn staged_with<D, F>(look: Look, shown: Display, drive: D)
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    staged_options(look, shown, None, drive).await;
}

async fn staged_options<D, F>(look: Look, shown: Display, card: Option<CardSetup>, drive: D)
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    staged_on(look, shown, card, Vec::new(), drive).await;
}

async fn staged_on<D, F>(
    look: Look,
    shown: Display,
    card: Option<CardSetup>,
    endpoints: Vec<EndpointFacts>,
    drive: D,
) where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    let screen = Screen::new(look.rows, look.cols);
    let (commands, asked) = super::without_terminal(64);
    let (events_in, events) = mpsc::channel(256);
    let (inputs, received) = mpsc::unbounded_channel();
    let mut facts = super::facts();
    facts.model = MODEL.to_owned();
    facts.effort = "high".to_owned();
    if !endpoints.is_empty() {
        facts.endpoint_name = endpoints
            .iter()
            .find(|e| e.name == "manifest")
            .or(endpoints.first())
            .map(|e| e.name.clone());
        facts.endpoints = endpoints;
    }
    let skip_usage = facts.endpoints.iter().any(|e| {
        e.credential
            .and_then(wisp_tui::app::Credential::plan)
            .is_some()
    });
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts,
        events,
        commands,
        resumed: None,
    };
    let mut options = Options::new(Theme::new(look.mode, Depth::TrueColor), look.glyphs);
    options.code = Start::Stalled;
    options.motion = MotionPolicy::Reduced;
    options.display = shown;
    options.card_setup = card;
    let mut console = screen.clone();
    let app = run(&mut console, received, session, options);
    let stage = Stage {
        screen: screen.clone(),
        inputs,
        events: events_in,
        commands: asked,
        skip_usage,
    };
    let driver = async move {
        stage.until(ready).await;
        drive(stage).await;
    };
    let (exit, ()) = tokio::join!(app, driver);
    exit.unwrap();
}

fn snapshot(scene: &str, look: Look, shot: &str) {
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_snapshot_path("../snapshots/chrome");
    settings.bind(|| {
        insta::assert_snapshot!(format!("settings-{scene}-{}", look.name()), shot);
    });
}

fn resolved(name: &str) -> ResolvedSetting {
    let spec = find(name).unwrap();
    let value = spec.default_value();
    let origin = if value.is_some() {
        "default".to_owned()
    } else {
        spec.default_words()
    };
    let choices = spec
        .kind
        .choices()
        .iter()
        .map(|c| (*c).to_owned())
        .collect();
    ResolvedSetting {
        name: SettingName::of(spec),
        value,
        layer: Layer::Default,
        origin,
        shadowed: vec![],
        choices,
        applies: true,
    }
}

fn set(rows: &mut [ResolvedSetting], name: &str, layer: Layer, text: &str, origin: &str) {
    let value = value_of(name, text);
    for row in rows.iter_mut().filter(|r| r.name.spec().name == name) {
        if row.layer != Layer::Default {
            row.shadowed.insert(
                0,
                Shadowed {
                    layer: row.layer.clone(),
                    value: row.value.clone().unwrap(),
                    origin: row.origin.clone(),
                },
            );
        } else if let Some(old) = row.value.clone() {
            row.shadowed.push(Shadowed {
                layer: Layer::Default,
                value: old,
                origin: "default".to_owned(),
            });
        }
        row.value = Some(value.clone());
        row.layer = layer.clone();
        row.origin = origin.to_owned();
    }
}

fn saved(name: &str, place: Place, text: &str, line: u64) -> SavedValue {
    let spec = find(name).unwrap();
    SavedValue::of(SitePath::new(place, spec), value_of(name, text), Some(line))
}

const MODEL: &str = "claude-opus-5-5";

fn settings() -> Vec<ResolvedSetting> {
    let mut rows: Vec<ResolvedSetting> = placed()
        .map(|(_, spec)| spec.name)
        .chain(["context_window", "pricing"])
        .map(resolved)
        .collect();
    for row in rows.iter_mut() {
        match row.name.spec().name {
            "effort" => {
                row.choices = ["low", "medium", "high", "xhigh", "max"]
                    .map(str::to_owned)
                    .to_vec();
            }
            "endpoint" => {
                row.choices = vec!["direct".to_owned(), "manifest".to_owned()];
            }
            "model" => {
                row.choices = vec![MODEL.to_owned(), "claude-sonnet-5-5".to_owned()];
            }
            _ => {}
        }
    }
    set(&mut rows, "model", Layer::Config, MODEL, "model");
    set(&mut rows, "effort", Layer::Config, "medium", "effort");
    set(&mut rows, "effort", Layer::Flag, "high", "--effort");
    set(&mut rows, "endpoint", Layer::Config, "manifest", "default");
    set(
        &mut rows,
        "context_window",
        Layer::Shipped,
        "1000000",
        &format!("models.toml {MODEL}"),
    );
    set(
        &mut rows,
        "web_search",
        Layer::Shipped,
        "web_search_20250305",
        &format!("models.toml {MODEL}"),
    );
    set(
        &mut rows,
        "display.motion",
        Layer::Env,
        "reduced",
        "WISP_MOTION",
    );
    rows
}

pub(super) fn snapshot_of(request: RequestId) -> SettingsSnapshot {
    SettingsSnapshot {
        often: Vec::new(),
        answers: request,
        context: SnapshotContext {
            endpoint: "manifest".to_owned(),
            model: MODEL.to_owned(),
            agent: None,
            max_tokens: 64_000,
            window: Some(1_000_000),
            room: None,
        },
        settings: settings(),
        saved: vec![
            saved("endpoint", Place::Global, "manifest", 1),
            saved("model", Place::Global, MODEL, 2),
            saved("effort", Place::Global, "medium", 3),
        ],
        endpoints: vec![],
        models: vec![],
        file: FileState::Ok {
            path_display: "~/.config/wisp/config.toml".to_owned(),
        },
        switches: vec![],
        compaction: None,
        plugins: Vec::new(),
        placed: Vec::new(),
    }
}

fn broken_of(request: RequestId) -> SettingsSnapshot {
    SettingsSnapshot {
        saved: vec![],
        file: FileState::Broken {
            line: Some(12),
            reason: "expected a value".to_owned(),
        },
        ..snapshot_of(request)
    }
}

fn changed_of(request: RequestId) -> SettingsSnapshot {
    let mut out = snapshot_of(request);
    out.saved = vec![
        saved("endpoint", Place::Global, "manifest", 1),
        saved("model", Place::Global, MODEL, 2),
        saved("effort", Place::Global, "max", 3),
    ];
    out
}

fn refreshed_of(request: RequestId) -> SettingsSnapshot {
    let mut out = snapshot_of(request);
    set(
        &mut out.settings,
        "effort",
        Layer::Session,
        "max",
        "/effort",
    );
    out
}

const SIZES: [(u16, u16); 3] = [(120, 40), (80, 30), (40, 16)];

async fn opened(look: Look) -> String {
    let shot = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let kept = std::rc::Rc::clone(&shot);
    staged(look, |mut stage| async move {
        stage.open("/config", plugins_of).await;
        if look.cols == 40 && look.rows == 16 {
            let glyphs = wisp_render::theme::glyphs::Glyphs::of(look.glyphs);
            assert!(shows(&stage.screen.visible(), glyphs.more_down));
            stage.key(KeyCode::End);
            stage.until(|r| shows(r, glyphs.more_up)).await;
            stage.key(KeyCode::Home);
            stage.until(|r| marked(r) == "Model").await;
        }
        stage.key(KeyCode::Down);
        *kept.borrow_mut() = stage.shot_when(|r| marked(r) == "Effort").await;
    })
    .await;
    shot.take()
}

#[tokio::test]
async fn the_screen_in_both_themes_both_glyph_sets_and_three_widths() {
    let mode = Mode::Dark;
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for (cols, rows) in SIZES {
            let look = Look {
                mode,
                glyphs,
                cols,
                rows,
            };
            let shot = opened(look).await;
            snapshot("open", look, &shot);
        }
    }
}

fn modelless_of(request: RequestId) -> SettingsSnapshot {
    let mut out = plugins_of(request);
    for tool in out
        .plugins
        .iter_mut()
        .flat_map(|p| &mut p.tools)
        .filter(|t| t.name == "web_search")
    {
        tool.unavailable = Some("not on this model".to_owned());
    }
    out
}

#[tokio::test]
async fn the_web_search_row_says_why_the_model_has_no_search() {
    let look = Look::dark(80, 30);
    staged(look, |mut stage| async move {
        stage.open("/config Web search", modelless_of).await;
        let shot = stage.shot_when(|r| shows(r, "not on this model")).await;
        snapshot("web-search", look, &shot);
    })
    .await;
}

#[tokio::test]
async fn the_web_search_row_quotes_no_cost_while_nothing_is_cached() {
    staged(Look::dark(80, 30), |mut stage| async move {
        stage.open("/config Web search", plugins_of).await;
        let rows = stage.until(|r| shows(r, "enter turn off")).await;
        assert!(!shows(&rows, "nothing cached"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn the_screen_with_ambiguous_wide_glyphs() {
    for (cols, rows) in SIZES {
        let look = Look {
            glyphs: GlyphSet::AmbiguousWide,
            ..Look::dark(cols, rows)
        };
        let shot = opened(look).await;
        snapshot("open", look, &shot);
    }
}

#[tokio::test]
async fn the_loading_state_shows_until_the_snapshot_arrives() {
    for (cols, rows) in [(120, 40), (80, 30)] {
        let look = Look::dark(cols, rows);
        staged(look, |mut stage| async move {
            stage.typed("/config").key(KeyCode::Enter);
            stage.asked().await;
            let shot = stage.shot_when(loading).await;
            snapshot("loading", look, &shot);
            let rows = stage.screen.visible();
            assert!(rows.iter().any(|r| r.contains("Loading settings\u{2026}")));
        })
        .await;
    }
}

#[tokio::test]
async fn a_broken_file_says_so_under_the_header() {
    for (cols, rows) in [(120, 40), (80, 30)] {
        let look = Look::dark(cols, rows);
        staged(look, |mut stage| async move {
            stage.open("/config", broken_of).await;
            let shot = stage.screen.styled();
            snapshot("broken", look, &shot);
            let rows = stage.screen.visible();
            assert!(
                rows[2].contains(
                    "config.toml has an error at line 12; saving is off until it's fixed"
                )
            );
        })
        .await;
    }
}

#[tokio::test]
async fn a_value_changed_in_the_file_shows_with_applies_next_session() {
    for (cols, rows) in [(120, 40), (80, 30)] {
        let look = Look::dark(cols, rows);
        staged(look, |mut stage| async move {
            stage.open("/config effort", changed_of).await;
            let shot = stage.screen.styled();
            snapshot("changed", look, &shot);
            let text = stage.screen.visible().join("\n");
            assert!(text.contains("config.toml now has max"), "{text}");
            assert!(text.contains("new chats"), "{text}");
        })
        .await;
    }
}

impl Screen {
    fn cursor(&self) -> (u16, u16) {
        self.0.borrow().parser.screen().cursor_position()
    }
}

fn marked(rows: &[String]) -> String {
    rows.iter()
        .find_map(|r| {
            r.strip_prefix(" \u{203a} ")
                .or_else(|| r.strip_prefix(" > "))
        })
        .and_then(|r| r.split("  ").next())
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn selected(rows: &[String]) -> String {
    rows.iter()
        .find(|r| r.starts_with(" \u{203a} "))
        .map(|r| r.trim_start_matches(" \u{203a} ").to_owned())
        .unwrap_or_default()
}

fn label(rows: &[String]) -> String {
    selected(rows)
        .split("  ")
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

const NARROW: Look = Look::dark(80, 30);
const WIDE: Look = Look::dark(120, 40);
const TALL: Look = Look::dark(120, 65);

#[tokio::test]
async fn arrows_and_j_k_move_between_rows_skipping_headings() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        assert_eq!(label(&stage.screen.visible()), "Model");
        stage.key(KeyCode::Down).char('j');
        stage.until(|r| label(r) == "Account").await;
        stage.char('k');
        stage.until(|r| label(r) == "Effort").await;
        stage.key(KeyCode::Up);
        stage.until(|r| label(r) == "Model").await;
        stage.key(KeyCode::Up).key(KeyCode::Tab);
        stage.until(|r| label(r) == "Model").await;
        stage.key(KeyCode::End).key(KeyCode::Right);
        stage.until(|r| label(r) == "Chat").await;
        stage.key(KeyCode::Down).key(KeyCode::Up);
        stage.until(|r| label(r) == "Chat").await;
    })
    .await;
}

#[tokio::test]
async fn pages_first_last_and_sections_jump_as_the_keys_say() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        stage.char('G');
        stage.until(|r| label(r) == "More settings…").await;
        stage.char('g');
        stage.until(|r| label(r) == "Model").await;
        stage.key(KeyCode::PageDown);
        let rows = stage
            .until(|r| !label(r).is_empty() && label(r) != "Model")
            .await;
        let paged = label(&rows);
        stage.key(KeyCode::PageUp);
        stage.until(|r| label(r) == "Model").await;
        stage.char('/').typed("cache").key(KeyCode::Enter);
        stage.key(KeyCode::Home);
        stage.until(|r| label(r) == "Keep-warm refreshes").await;
        stage.key(KeyCode::Tab);
        stage.until(|r| label(r) == "Verbose").await;
        stage.key(KeyCode::Tab);
        stage.until(|r| label(r) == "Cache lifetime").await;
        stage.key(KeyCode::BackTab);
        stage.until(|r| label(r) == "Verbose").await;
        stage.shifted(KeyCode::Tab);
        stage.until(|r| label(r) == "Keep-warm refreshes").await;
        stage.key(KeyCode::End);
        stage.until(|r| label(r) == "Cache diagnostics").await;
        stage.key(KeyCode::Home);
        stage.until(|r| label(r) == "Keep-warm refreshes").await;
        assert_ne!(paged, "Model");
    })
    .await;
}

#[tokio::test]
async fn each_row_shows_its_value_and_chat_mark_and_details_name_the_layers() {
    staged(TALL, |mut stage| async move {
        stage.open("/config effort", snapshot_of).await;
        let rows = stage.screen.visible();
        assert!(selected(&rows).starts_with("Effort"), "{rows:#?}");
        assert!(
            selected(&rows).trim_end().ends_with("this chat"),
            "{rows:#?}"
        );
        assert!(shows(&rows, "This chat: high. New chats: medium."));
        assert!(!shows(&rows, "--effort"));
        stage.char('?');
        let rows = stage.until(|r| shows(r, "Name")).await;
        assert!(shows(&rows, "This chat      high"), "{rows:#?}");
        assert!(shows(&rows, "--effort"), "{rows:#?}");
        assert!(shows(&rows, "Saved          medium"), "{rows:#?}");
        assert!(shows(&rows, "config.toml line 3"), "{rows:#?}");
        assert!(shows(&rows, "Wisp's own     medium"), "{rows:#?}");
        assert!(!shows(&rows, "not used:"));
    })
    .await;
}

#[tokio::test]
async fn the_filter_matches_label_name_and_help_and_collapses_empty_sections() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        stage.char('/').typed("cache");
        let rows = stage.until(|r| shows(r, " Filter  cache")).await;
        assert!(shows(&rows, "Cache lifetime"));
        assert!(shows(&rows, "Cache diagnostics"));
        assert!(shows(&rows, "Verbose"), "help mentions the cache line");
        assert!(!shows(&rows, " Chat"), "{rows:#?}");
        assert!(!shows(&rows, "Effort"), "{rows:#?}");
        assert_eq!(label(&rows), "Keep-warm refreshes");
        stage.key(KeyCode::Enter).key(KeyCode::End);
        stage.until(|r| label(r) == "Cache diagnostics").await;
        stage.key(KeyCode::Esc);
        let rows = stage.until(|r| label(r) == "Model").await;
        assert!(!shows(&rows, " Filter"), "{rows:#?}");
        assert_eq!(label(&rows), "Model");
        stage.char('/').typed("zzz");
        let rows = stage.until(|r| shows(r, " Filter  zzz")).await;
        assert!(!shows(&rows, " Advanced"), "{rows:#?}");
        stage.ctrl('c');
        stage
            .until(|r| label(r) == "Model" && shows(r, " Filter"))
            .await;
        stage.key(KeyCode::Esc);
        stage.char('q');
        let rows = stage.until(|r| !on_screen(r)).await;
        assert!(!shows(&rows, " Advanced"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn config_with_a_word_opens_filtered_or_on_one_setting() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config cache", snapshot_of).await;
        let rows = stage.screen.visible();
        assert!(shows(&rows, " Filter  cache"), "{rows:#?}");
        assert!(!shows(&rows, "Effort"));
        stage.key(KeyCode::Esc).key(KeyCode::Esc);
        stage.open("/config compact_at", snapshot_of).await;
        let rows = stage.screen.visible();
        assert_eq!(label(&rows), "Compact at");
        assert!(shows(&rows, "Effort"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn single_place_rows_have_no_for_choice_and_e_does_nothing() {
    staged(NARROW, |mut stage| async move {
        for command in ["/config display.theme", "/config cache_ttl"] {
            stage.open(command, snapshot_of).await;
            stage.char('e').key(KeyCode::Enter);
            let rows = stage
                .until(|r| shows(r, "Choose") && footer(r).contains("esc cancel"))
                .await;
            assert!(
                !rows.iter().any(|r| r.trim_start().starts_with("For")),
                "{rows:#?}"
            );
            stage.nothing_sent().await;
            stage.close_settings().await;
        }
    })
    .await;
}

fn switched(request: RequestId, model: &str, change: AppliedChange) -> AgentEvent {
    settings_changed(Changed {
        answers: Some(request),
        endpoint: "manifest".into(),
        model: model.into(),
        effort: wisp_protocol::Effort::High,
        cache: CacheEffect::Rebuilt {
            reason: wisp_protocol::RebuildReason::ModelSwitch,
        },
        changes: vec![change],
        ..Changed::default()
    })
}

fn pair_saved(command: ClientCommand) -> (RequestId, Vec<SettingChange>) {
    match command {
        ClientCommand::ChangeSettings {
            request,
            changes,
            save: true,
        } => (request, changes),
        other => panic!("expected a saved pair change, got {other:?}"),
    }
}

#[tokio::test]
async fn the_model_row_opens_the_picker_whose_enter_saves_and_esc_keeps_the_screen() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config model", snapshot_of).await;
        stage.key(KeyCode::Enter);
        let rows = stage.until(|r| footer(r).contains("enter save")).await;
        assert!(shows(&rows, "sonnet-5-5"), "{rows:#?}");
        stage.nothing_sent().await;
        stage.key(KeyCode::Esc);
        let rows = stage.until(|r| !shows(r, "sonnet-5-5")).await;
        assert!(selected(&rows).contains("Model"), "{rows:#?}");
        stage
            .key(KeyCode::Enter)
            .key(KeyCode::Down)
            .key(KeyCode::Down)
            .key(KeyCode::Enter);
        let (request, changes) = pair_saved(stage.next().await);
        assert_eq!(
            changes,
            ClientCommand::shortcut_changes(None, Some("claude-sonnet-5-5".to_owned()), None)
        );
        stage
            .until(|r| selected(r).contains("saving\u{2026}"))
            .await;
        let change = AppliedChange {
            cache: Some(CacheEffect::Rebuilt {
                reason: wisp_protocol::RebuildReason::ModelSwitch,
            }),
            ..applied(
                "model",
                Some(SettingValue::Text("claude-sonnet-5-5".to_owned())),
                When::NextTurn,
            )
        };
        stage
            .emit(switched(request, "claude-sonnet-5-5", change))
            .await;
        let again = stage.asked().await;
        stage
            .emit(AgentEvent::SettingsSnapshot(snapshot_of(again)))
            .await;
        stage.until(|r| !selected(r).contains("saving")).await;
        stage.close_settings().await;
        let rows = stage.until(|r| !on_screen(r)).await;
        assert!(
            said(
                &rows,
                "saved to ~/.config/wisp/config.toml (was claude-opus-5-5)"
            ),
            "{rows:#?}"
        );
        assert!(said(&rows, "sonnet-5-5"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn the_effort_row_picks_a_level_of_the_current_model_and_saves_it() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config effort", snapshot_of).await;
        stage
            .key(KeyCode::Enter)
            .key(KeyCode::Up)
            .key(KeyCode::Enter);
        let (_, changes) = pair_saved(stage.next().await);
        assert_eq!(
            changes,
            ClientCommand::shortcut_changes(None, None, Some(wisp_protocol::Effort::Medium))
        );
        stage
            .until(|r| selected(r).contains("saving\u{2026}"))
            .await;
    })
    .await;
}

#[tokio::test]
async fn model_typed_into_the_composer_still_changes_only_this_session() {
    staged(NARROW, |mut stage| async move {
        stage.typed("/model").key(KeyCode::Enter);
        stage.until(|r| shows(r, "sonnet-5-5")).await;
        stage
            .key(KeyCode::Down)
            .key(KeyCode::Down)
            .key(KeyCode::Enter);
        let mut command = stage.next().await;
        if matches!(command, ClientCommand::DescribeSettings { .. }) {
            command = stage.next().await;
        }
        assert!(
            matches!(command, ClientCommand::ChangeSettings { save: false, .. }),
            "{command:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_turn_that_ends_with_the_screen_open_reads_a_new_snapshot() {
    staged(NARROW, |mut stage| async move {
        stage.emit(AgentEvent::turn_started(1)).await;
        stage.open("/config", snapshot_of).await;
        stage.nothing_sent().await;
        stage.emit(super::completed()).await;
        let again = stage.asked().await;
        stage
            .emit(AgentEvent::SettingsSnapshot(snapshot_of(again)))
            .await;
        stage.nothing_sent().await;
    })
    .await;
}

fn tools_of(request: RequestId) -> SettingsSnapshot {
    let mut out = snapshot_of(request);
    for row in out
        .settings
        .iter_mut()
        .filter(|r| r.name.spec().name == "tools")
    {
        row.choices = vec!["bash".to_owned(), "web_search".to_owned()];
    }
    out
}

#[tokio::test]
async fn the_tools_checklist_saves_the_checked_tools_for_the_next_session() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config tools", tools_of).await;
        stage.key(KeyCode::Enter);
        stage.until(|r| shows(r, "[x] bash")).await;
        stage.key(KeyCode::Right).char(' ').key(KeyCode::Enter);
        let (request, change) = saving(stage.next().await);
        assert_eq!(change.target, named("tools"));
        assert_eq!(
            change.value,
            Some(SettingValue::List(vec!["bash".to_owned()]))
        );
        stage
            .emit(answered(
                request,
                applied(
                    "tools",
                    Some(SettingValue::List(vec!["bash".to_owned()])),
                    When::NextSession,
                ),
            ))
            .await;
        stage.asked().await;
        stage.close_settings().await;
        let rows = stage.until(|r| !on_screen(r)).await;
        assert!(
            said(&rows, "Tools \u{2192} bash \u{b7} new chats"),
            "{rows:#?}"
        );
        stage.open("/config tools", tools_of).await;
        let rows = stage.screen.visible();
        assert!(selected(&rows).ends_with("new chats"), "{rows:#?}");
    })
    .await;
}

fn sited_of(request: RequestId) -> SettingsSnapshot {
    let mut out = snapshot_of(request);
    out.saved.push(saved(
        "compact_at",
        Place::Model(MODEL.to_owned()),
        "60%",
        9,
    ));
    out
}

#[tokio::test]
async fn tab_saves_to_the_for_place_as_its_path() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config compact_at", sited_of).await;
        stage.key(KeyCode::Enter);
        stage
            .until(|r| shows(r, "For") && footer(r).contains("tab all models"))
            .await;
        stage.key(KeyCode::Tab);
        stage.until(|r| footer(r).contains("tab this model")).await;
        stage.typed("90%").key(KeyCode::Enter);
        let (_, change) = saving(stage.next().await);
        assert_eq!(
            change.target,
            SettingTarget::Path(SitePath::global(find("compact_at").unwrap()))
        );
        assert_eq!(
            change.value,
            Some(find("compact_at").unwrap().parse("90%").unwrap())
        );
    })
    .await;
}

fn saved_to() -> Option<Saved> {
    Some(Saved {
        path_display: "~/.config/wisp/config.toml".to_owned(),
        line: Some(4),
        notes: Vec::new(),
    })
}

fn applied(name: &str, value: Option<SettingValue>, when: When) -> AppliedChange {
    AppliedChange {
        cache: Some(CacheEffect::Kept),
        saved: saved_to(),
        ..change(named(name), value, Layer::Session, when)
    }
}

fn answered(request: RequestId, change: AppliedChange) -> AgentEvent {
    settings_changed(Changed {
        answers: Some(request),
        endpoint: "manifest".into(),
        model: MODEL.into(),
        effort: wisp_protocol::Effort::High,
        changes: vec![change],
        ..Changed::default()
    })
}

fn saving(command: ClientCommand) -> (RequestId, SettingChange) {
    match command {
        ClientCommand::ChangeSettings {
            request,
            mut changes,
            save: true,
        } if changes.len() == 1 => (request, changes.remove(0)),
        other => panic!("expected one saved change, got {other:?}"),
    }
}

fn said(rows: &[String], text: &str) -> bool {
    let all = rows.join(" ");
    let all: Vec<&str> = all.split_whitespace().collect();
    let want: Vec<&str> = text.split_whitespace().collect();
    all.windows(want.len()).any(|w| w == want.as_slice())
}

#[tokio::test]
async fn enter_on_a_switch_saves_it_and_the_note_names_the_value_it_replaced() {
    staged(WIDE, |mut stage| async move {
        stage.open("/config debug_cache", snapshot_of).await;
        let rows = stage.screen.visible();
        assert!(shows(&rows, "enter turn on"), "{rows:#?}");
        assert!(
            !rows.last().unwrap().contains("enter change"),
            "{rows:#?}"
        );
        stage.key(KeyCode::Enter);
        let (request, change) = saving(stage.next().await);
        assert_eq!(change.target, named("debug_cache"));
        assert_eq!(change.value, Some(SettingValue::Bool(true)));
        stage
            .until(|r| selected(r).contains("saving\u{2026}"))
            .await;
        stage
            .emit(answered(
                request,
                applied("debug_cache", Some(SettingValue::Bool(true)), When::NextTurn),
            ))
            .await;
        assert_eq!(
            stage.next().await,
            ClientCommand::SettingsUsed {
                names: vec!["debug_cache".into()]
            }
        );
        let again = stage.asked().await;
        let mut refreshed = snapshot_of(again);
        set(
            &mut refreshed.settings,
            "debug_cache",
            Layer::Session,
            "on",
            "session",
        );
        stage.emit(AgentEvent::SettingsSnapshot(refreshed)).await;
        let rows = stage
            .until(|r| {
                selected(r)
                    .split('\u{2502}')
                    .next()
                    .is_some_and(|list| list.trim_end().ends_with("this chat"))
            })
            .await;
        assert!(!selected(&rows).contains("saving"), "{rows:#?}");
        assert!(selected(&rows).contains("on"), "{rows:#?}");
        stage.close_settings().await;
        let rows = stage.until(|r| !on_screen(r)).await;
        assert!(
            said(
                &rows,
                format!("Cache diagnostics \u{2192} on \u{b7} next turn \u{b7} {CACHE_KEPT} \u{b7} saved to ~/.config/wisp/config.toml (was off)").as_str()
            ),
            "{rows:#?}"
        );
    })
    .await;
}

fn provider_off_of(request: RequestId) -> SettingsSnapshot {
    let mut out = plugins_of(request);
    out.context.endpoint = "antigravity".into();
    out.plugins = vec![catalog_plugin("gemini", "Gemini", Vec::new())];
    out
}

fn provider_accounts() -> Vec<EndpointFacts> {
    let mut gemini = endpoint("antigravity", Endpoint::Official);
    gemini.provider = "gemini".into();
    gemini.credential = Some(wisp_tui::app::Credential::AntigravityPlan);
    gemini.priced = false;
    let mut claude = endpoint("claude-sub", Endpoint::Official);
    claude.credential = Some(wisp_tui::app::Credential::AnthropicPlan);
    claude.priced = false;
    vec![gemini, claude]
}

impl Stage {
    async fn provider_off(&mut self, remove: bool, live: bool) -> (RequestId, SettingChange) {
        self.open("/config gemini", provider_off_of).await;
        if remove {
            self.char('d');
            self.until(|r| shows(r, "Remove gemini?")).await;
            self.key(KeyCode::Up);
        }
        self.key(KeyCode::Enter);
        let (request, switch) = saving(self.next().await);
        use wisp_protocol::subscription_auth::{Way, WayOrigin};
        let origins = ["/", "worker"]
            .into_iter()
            .map(|path| {
                (
                    path.to_owned(),
                    WayOrigin {
                        account: "gemini".into(),
                        provider: Some("gemini".into()),
                        billing: Some(wisp_protocol::Billing::Metered),
                        model: MODEL.into(),
                        effort: wisp_protocol::Effort::High,
                    },
                )
            })
            .collect();
        let live_way = Way {
            chat: Some("claude-sub".into()),
            follow: vec!["worker".into()],
            origins,
            ..Way::default()
        };
        self.emit(AgentEvent::LeaveNeeds {
            request,
            with_followers: if live { vec![live_way] } else { Vec::new() },
            ways: vec![wisp_protocol::subscription_auth::Way {
                chat: Some("claude-sub".into()),
                stay: vec!["worker".into()],
                ..Default::default()
            }],
        })
        .await;
        self.until(|r| {
            shows(
                r,
                if live {
                    "This chat and worker move"
                } else {
                    "worker stays behind"
                },
            )
        })
        .await;
        (request, switch)
    }

    async fn provider_refused(&self, request: RequestId) {
        self.emit(AgentEvent::refused(
            Some(request),
            "This chat is on gemini. Switch account first (/account).",
        ))
        .await;
        let rows = self.unchanged_for_a_while().await;
        assert!(
            !shows(&rows, "Switch account first") && !shows(&rows, "Settings not changed"),
            "{rows:#?}"
        );
    }

    async fn provider_move(&mut self) -> (RequestId, SettingChange) {
        let (request, switch) = self.provider_off(false, false).await;
        self.provider_refused(request).await;
        self.key(KeyCode::Enter);
        let ClientCommand::ChangeSettings {
            request,
            changes,
            save: false,
        } = self.next().await
        else {
            panic!("expected an unsaved move and switch")
        };
        let mut expected = ClientCommand::shortcut_changes(Some("claude-sub".into()), None, None);
        expected.push(switch.clone());
        assert_eq!(changes, expected);
        self.nothing_sent().await;
        (request, switch)
    }
}

fn provider_answer(request: RequestId, switch: &SettingChange, saved: bool) -> AgentEvent {
    let mut off = change(
        switch.target.clone(),
        switch.value.clone(),
        Layer::Session,
        When::Now,
    );
    off.saved = if saved { saved_to() } else { None };
    off.warnings
        .push("worker waits until gemini is on again".into());
    let changes = if saved {
        vec![off]
    } else {
        vec![
            change(
                named("endpoint"),
                Some(SettingValue::Text("claude-sub".into())),
                Layer::Session,
                When::Now,
            ),
            off,
        ]
    };
    settings_changed(Changed {
        answers: Some(request),
        endpoint: "claude-sub".into(),
        model: MODEL.into(),
        effort: wisp_protocol::Effort::High,
        changes,
        ..Changed::default()
    })
}

#[tokio::test]
async fn a_provider_switch_opens_the_where_card_and_esc_keeps_it_on() {
    staged_on(
        WIDE,
        display(),
        None,
        provider_accounts(),
        |mut stage| async move {
            for (remove, late) in [(false, false), (true, false), (false, true)] {
                let (request, _) = stage.provider_off(remove, false).await;
                if !late {
                    stage.provider_refused(request).await;
                }
                let rows = stage.screen.visible();
                assert!(
                    said(
                        &rows,
                        if remove {
                            "gemini is being removed."
                        } else {
                            "gemini is going off."
                        }
                    ),
                    "{rows:#?}"
                );
                stage.key(KeyCode::Esc);
                stage
                    .until(|r| !shows(r, "Where should this chat go?"))
                    .await;
                if late {
                    stage.provider_refused(request).await;
                }
                let rows = stage.unchanged_for_a_while().await;
                assert!(
                    !shows(&rows, "Settings not changed") && !shows(&rows, "Switch account first"),
                    "{rows:#?}"
                );
                stage.nothing_sent().await;
                stage.open("/config", provider_off_of).await;
                let rows = stage.unchanged_for_a_while().await;
                assert!(
                    selected(&rows).contains("gemini") && selected(&rows).contains("on"),
                    "{rows:#?}"
                );
                assert!(
                    !shows(&rows, "Switch account first") && !shows(&rows, "Settings not changed"),
                    "{rows:#?}"
                );
                stage.close_settings().await;
            }
        },
    )
    .await;
}

#[tokio::test]
async fn the_where_card_moves_the_chat_then_saves_only_the_switch_and_prints_it_once() {
    staged_on(
        WIDE,
        display(),
        None,
        provider_accounts(),
        |mut stage| async move {
            let (offer, switch) = stage.provider_off(false, true).await;
            stage.provider_refused(offer).await;
            stage.key(KeyCode::Enter);
            let ClientCommand::AnswerProviderSwitch {
                request,
                changes,
                follow,
                accept_api,
            } = stage.next().await
            else {
                panic!("expected confirmed switch")
            };
            assert_eq!(follow, ["worker"]);
            assert!(!accept_api);
            let mut expected =
                ClientCommand::shortcut_changes(Some("claude-sub".into()), None, None);
            expected.push(switch.clone());
            assert_eq!(changes, expected);
            stage
                .emit(AgentEvent::rejected("This answer is not understood."))
                .await;
            stage
                .until(|r| shows(r, "This answer is not understood."))
                .await;
            stage
                .emit(AgentEvent::refused(offer, "Unrelated answer."))
                .await;
            stage.nothing_sent().await;
            stage.emit(provider_answer(request, &switch, false)).await;
            let (saved, sent) = saving(stage.next().await);
            assert_eq!(sent, switch);
            stage
                .emit(AgentEvent::FollowersMoved {
                    request,
                    to: "claude-sub".into(),
                    moved: vec![wisp_protocol::subscription_auth::Followed {
                        agent: "worker".into(),
                        model: MODEL.into(),
                        effort: wisp_protocol::Effort::High,
                        rebuild_usd: Some(0.08),
                        from: None,
                        to: None,
                    }],
                })
                .await;
            stage.until(|r| shows(r, "~$0.08")).await;
            stage.emit(provider_answer(request, &switch, false)).await;
            stage.nothing_sent().await;
            let rows = stage.until(|r| shows(r, "Switched to")).await;
            assert!(shows(&rows, "claude-sub"), "{rows:#?}");
            assert!(!shows(&rows, "Plugin for gemini"), "{rows:#?}");
            stage.emit(provider_answer(saved, &switch, true)).await;
            let rows = stage.until(|r| shows(r, "saved to")).await;
            assert!(
                said(
                    &rows,
                    "Plugin for gemini → off · applies now · saved to ~/.config/wisp/config.toml"
                ),
                "{rows:#?}"
            );
            assert_eq!(
                rows.join(" ").matches("Plugin for gemini").count(),
                1,
                "{rows:#?}"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn a_refused_where_card_move_does_not_save_the_switch() {
    staged_on(
        WIDE,
        display(),
        None,
        provider_accounts(),
        |mut stage| async move {
            let (request, _) = stage.provider_move().await;
            stage
                .emit(AgentEvent::refused(
                    Some(request),
                    "The account is no longer available.",
                ))
                .await;
            let rows = stage
                .until(|r| {
                    shows(
                        r,
                        "Settings not changed: The account is no longer available.",
                    )
                })
                .await;
            assert!(!shows(&rows, "Plugin for gemini"), "{rows:#?}");
            stage.nothing_sent().await;
        },
    )
    .await;
}

#[tokio::test]
async fn a_refused_where_card_save_prints_the_unsaved_switch_before_the_refusal() {
    staged_on(
        WIDE,
        display(),
        None,
        provider_accounts(),
        |mut stage| async move {
            let (request, switch) = stage.provider_move().await;
            stage.emit(provider_answer(request, &switch, false)).await;
            let (saved, _) = saving(stage.next().await);
            stage
                .emit(AgentEvent::refused(
                    Some(saved),
                    "The settings file is not writable.",
                ))
                .await;
            let rows = stage
                .until(|r| {
                    shows(
                        r,
                        "Settings not changed: The settings file is not writable.",
                    )
                })
                .await;
            let shown = rows.join(" ");
            assert!(
                shown.find("Plugin for gemini").unwrap()
                    < shown.find("Settings not changed").unwrap(),
                "{rows:#?}"
            );
            assert_eq!(shown.matches("Plugin for gemini").count(), 1, "{rows:#?}");
            assert!(
                shows(&rows, "worker waits until gemini is on again") && !shows(&rows, "saved to"),
                "{rows:#?}"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn the_where_card_keeps_the_move_and_switch_visible_when_the_save_cannot_be_sent() {
    staged_on(
        WIDE,
        display(),
        None,
        provider_accounts(),
        |mut stage| async move {
            let (request, switch) = stage.provider_move().await;
            stage.commands.close();
            tokio::task::yield_now().await;
            stage.emit(provider_answer(request, &switch, false)).await;
            let rows = stage
                .until(|r| shows(r, "The agent stopped") && shows(r, "Plugin for gemini"))
                .await;
            assert!(
                shows(&rows, "claude-sub") && shows(&rows, "worker waits until gemini is on again"),
                "{rows:#?}"
            );
            assert!(!shows(&rows, "saved to"), "{rows:#?}");
            assert_eq!(
                rows.join(" ").matches("Plugin for gemini").count(),
                1,
                "{rows:#?}"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn a_choice_row_and_an_editor_save_only_what_enter_confirms() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config cache_ttl", snapshot_of).await;
        stage.key(KeyCode::Enter).key(KeyCode::Left);
        let rows = stage.until(|r| shows(r, "\u{203a} 5m")).await;
        assert!(shows(&rows, "esc cancel"), "{rows:#?}");
        stage.nothing_sent().await;
        stage.key(KeyCode::Esc);
        stage.nothing_sent().await;
        stage
            .key(KeyCode::Enter)
            .key(KeyCode::Left)
            .key(KeyCode::Enter);
        let (_, change) = saving(stage.next().await);
        assert_eq!(change.value, Some(SettingValue::Text("5m".to_owned())));
        stage.close_settings().await;
        stage.open("/config retry.budget_usd", snapshot_of).await;
        stage.key(KeyCode::Enter).ctrl('u').typed("half");
        stage
            .until(|r| said(r, "needs a number of dollars, such as 0.50, or none"))
            .await;
        stage.key(KeyCode::Enter);
        stage.nothing_sent().await;
        stage.ctrl('u').typed("0.50");
        stage.until(|r| shows(r, "= $0.50")).await;
        stage.key(KeyCode::Enter);
        let (_, change) = saving(stage.next().await);
        assert_eq!(change.value, Some(SettingValue::Usd { micros: 500_000 }));
    })
    .await;
}

#[tokio::test]
async fn a_pasted_newline_or_escape_never_reaches_the_editor() {
    staged(NARROW, |mut stage| async move {
        stage
            .open("/config context.verbatim_user_tokens", snapshot_of)
            .await;
        stage
            .key(KeyCode::Enter)
            .ctrl('u')
            .input(Input::Paste("1\n2\x1b3".to_owned()))
            .key(KeyCode::Enter);
        let (_, change) = saving(stage.next().await);
        assert_eq!(change.value, Some(SettingValue::Tokens(123)));
    })
    .await;
}

#[tokio::test]
async fn a_refused_save_shows_its_reason_under_the_row_and_in_the_transcript() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config debug_cache", snapshot_of).await;
        stage.key(KeyCode::Enter);
        let (request, _) = saving(stage.next().await);
        stage
            .emit(AgentEvent::Rejected {
                id: None,
                kind: None,
                request: Some(request),
                reason: "config.toml stayed locked".to_owned(),
            })
            .await;
        let rows = stage.until(|r| shows(r, "config.toml stayed locked")).await;
        let at = rows
            .iter()
            .position(|r| r.contains("Cache diagnostics"))
            .unwrap();
        assert!(
            rows[at + 1].contains("config.toml stayed locked"),
            "{rows:#?}"
        );
        assert!(rows[at].contains("off"), "{rows:#?}");
        stage.nothing_sent().await;
        stage.close_settings().await;
        let rows = stage.until(|r| !on_screen(r)).await;
        assert!(
            said(&rows, "Settings not changed: config.toml stayed locked."),
            "{rows:#?}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_change_made_during_a_turn_reads_pending_until_the_next_turn() {
    staged(NARROW, |mut stage| async move {
        stage.emit(AgentEvent::turn_started(1)).await;
        stage.open("/config cache_ttl", snapshot_of).await;
        stage
            .key(KeyCode::Enter)
            .key(KeyCode::Left)
            .key(KeyCode::Enter);
        let (request, _) = saving(stage.next().await);
        stage
            .emit(answered(
                request,
                applied(
                    "cache_ttl",
                    Some(SettingValue::Text("5m".to_owned())),
                    When::NextTurn,
                ),
            ))
            .await;
        assert_eq!(
            stage.next().await,
            ClientCommand::SettingsUsed {
                names: vec!["cache_ttl".into()]
            }
        );
        let again = stage.asked().await;
        let mut refreshed = snapshot_of(again);
        set(
            &mut refreshed.settings,
            "cache_ttl",
            Layer::Session,
            "5m",
            "session",
        );
        stage.emit(AgentEvent::SettingsSnapshot(refreshed)).await;
        let rows = stage
            .until(|r| selected(r).contains("5m \u{b7} pending \u{b7} next turn"))
            .await;
        assert!(
            footer(&rows).contains(&format!("{} {}", STOP_KEY.0, STOP_KEY.1)),
            "{rows:#?}"
        );
        stage.emit(super::completed()).await;
        let rows = stage
            .until(|r| !footer(r).contains(&format!("{} {}", STOP_KEY.0, STOP_KEY.1)))
            .await;
        assert!(selected(&rows).contains("pending"), "{rows:#?}");
        stage.emit(AgentEvent::turn_started(2)).await;
        stage.until(|r| !selected(r).contains("pending")).await;
    })
    .await;
}

fn timed_of(request: RequestId) -> SettingsSnapshot {
    let mut out = snapshot_of(request);
    set(&mut out.settings, "time", Layer::Config, "utc", "time");
    set(&mut out.settings, "time", Layer::Session, "off", "session");
    out.saved.push(saved("time", Place::Global, "utc", 4));
    out
}

#[tokio::test]
async fn d_shows_its_outcome_first_then_resets_and_says_what_now_wins() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config time", timed_of).await;
        let rows = stage.screen.visible();
        assert!(said(&rows, "d back to utc"), "{rows:#?}");
        stage.char('d');
        let ClientCommand::ChangeSettings {
            request,
            changes,
            save: false,
        } = stage.next().await
        else {
            panic!("d must change only this chat");
        };
        assert_eq!(
            changes,
            [SettingChange {
                target: named("time"),
                value: None
            }]
        );
        stage
            .emit(answered(
                request,
                change(named("time"), None, Layer::Session, When::NextTurn),
            ))
            .await;
        stage.asked().await;
        stage.close_settings().await;
        let rows = stage.until(|r| !on_screen(r)).await;
        assert!(
            said(&rows, "Message time reset \u{b7} next turn"),
            "{rows:#?}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_broken_file_saves_nothing_and_enter_says_why() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config debug_cache", broken_of).await;
        stage.key(KeyCode::Enter).char('d');
        stage
            .until(|r| footer(r).contains("saving is off until it's fixed"))
            .await;
        stage.nothing_sent().await;
    })
    .await;
}

#[tokio::test]
async fn the_error_choice_closes_the_screen_and_the_next_config_restores_the_edit() {
    staged(NARROW, |mut stage| async move {
        stage.emit(AgentEvent::turn_started(1)).await;
        stage.open("/config retry.budget_usd", snapshot_of).await;
        stage.key(KeyCode::Enter).ctrl('u').typed("0.7");
        stage
            .emit(turn_ended(End {
                ending: Ending::Error(ErrorKind::Server),
                message: "Upstream provider stream was interrupted.".to_owned(),
                committed_steps: 1,
                offer: Some(offer(true, false, RetryFocus::Retry)),
                ..End::default()
            }))
            .await;
        stage.asked().await;
        let rows = stage.until(|r| !on_screen(r)).await;
        assert!(!shows(&rows, " Settings"), "{rows:#?}");
        assert!(shows(&rows, "Retry"), "{rows:#?}");
        stage.nothing_sent().await;
        stage.open("/config", snapshot_of).await;
        let rows = stage.screen.visible();
        assert_eq!(label(&rows), "Retry budget");
        assert!(shows(&rows, "New value"), "{rows:#?}");
        stage.key(KeyCode::Enter);
        let (_, change) = saving(stage.next().await);
        assert_eq!(change.value, Some(SettingValue::Usd { micros: 700_000 }));
    })
    .await;
}

#[tokio::test]
async fn a_key_pasted_into_an_editor_is_not_kept_for_reopening() {
    staged(NARROW, |mut stage| async move {
        stage
            .open("/config context.verbatim_user_tokens", snapshot_of)
            .await;
        stage.key(KeyCode::Enter).ctrl('u');
        stage.input(Input::Paste(CANARY.to_owned()));
        stage
            .until(|r| shows(r, "needs a token count, such as 8000 or 8k"))
            .await;
        stage
            .emit(AgentEvent::QuestionAsked {
                call_id: "q1".to_owned(),
                questions: vec![wisp_protocol::Question {
                    header: "Pick".to_owned(),
                    question: "Which one?".to_owned(),
                    options: vec![
                        wisp_protocol::QuestionOption {
                            label: "A".to_owned(),
                            description: String::new(),
                        },
                        wisp_protocol::QuestionOption {
                            label: "B".to_owned(),
                            description: String::new(),
                        },
                    ],
                    multi_select: false,
                    source: None,
                }],
            })
            .await;
        let rows = stage.until(|r| shows(r, "Which one?")).await;
        assert!(!shows(&rows, " Settings"), "{rows:#?}");
        assert!(!shows(&rows, CANARY), "{rows:#?}");
        stage.nothing_sent().await;
    })
    .await;
}

const CANARY: &str = "sk9canary4key7shaped2text";

#[tokio::test]
async fn esc_returns_to_the_transcript_where_it_was() {
    staged(NARROW, |mut stage| async move {
        let before = stage.screen.visible();
        stage.open("/config", snapshot_of).await;
        let rows = stage.screen.visible();
        assert!(shows(&rows, " Settings"), "{rows:#?}");
        stage.key(KeyCode::Esc);
        let after = stage.until(|r| !on_screen(r)).await;
        assert_eq!(after[..10], before[..10]);
        assert!(!shows(&after, "Cache lifetime"));
    })
    .await;
}

#[tokio::test]
async fn a_settings_change_reads_a_new_snapshot() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        stage.char('j');
        stage.emit(effort_changed()).await;
        let request = stage.asked().await;
        let rows = stage.until(|r| label(r) == "Effort").await;
        assert!(
            selected(&rows).contains("this chat"),
            "the old rows stay until the answer"
        );
        stage
            .emit(AgentEvent::SettingsSnapshot(refreshed_of(request)))
            .await;
        let rows = stage.until(|r| selected(r).contains("max")).await;
        assert!(
            selected(&rows).trim_end().ends_with("this chat"),
            "{rows:#?}"
        );
        assert!(
            shows(&rows, "This chat: max. New chats: medium."),
            "{rows:#?}"
        );
    })
    .await;
}

fn effort_changed() -> AgentEvent {
    settings_changed(Changed {
        endpoint: "manifest".into(),
        model: MODEL.into(),
        effort: wisp_protocol::Effort::Max,
        ..Changed::default()
    })
}

#[tokio::test]
async fn a_refused_second_question_leaves_the_loaded_screen_open() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config effort", snapshot_of).await;
        stage.emit(effort_changed()).await;
        let request = stage.asked().await;
        stage
            .emit(AgentEvent::Rejected {
                id: None,
                kind: None,
                request: Some(request),
                reason: "the settings file is locked".to_owned(),
            })
            .await;
        stage.char('j');
        let rows = stage.until(|r| label(r) == "Account").await;
        assert!(shows(&rows, "Plugins"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn the_layers_not_used_are_listed_from_the_strongest_down() {
    staged(WIDE, |mut stage| async move {
        stage.open("/config effort", refreshed_of).await;
        stage.char('?');
        let rows = stage.until(|r| shows(r, "Name")).await;
        let at = |word: &str| rows.iter().position(|r| r.contains(word)).unwrap();
        assert!(at("/effort") < at("--effort"), "{rows:#?}");
        assert!(at("--effort") < at("Saved"), "{rows:#?}");
        assert!(at("Saved") < at("Wisp's own"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn a_late_answer_to_a_closed_screen_is_dropped() {
    staged(NARROW, |mut stage| async move {
        stage.typed("/config").key(KeyCode::Enter);
        let request = stage.asked().await;
        stage.until(loading).await;
        stage.key(KeyCode::Esc);
        stage.until(|r| !on_screen(r)).await;
        stage
            .emit(AgentEvent::SettingsSnapshot(snapshot_of(request)))
            .await;
        let rows = stage.unchanged_for_a_while().await;
        assert!(!shows(&rows, "Cache lifetime"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn a_refused_describe_closes_the_screen_with_the_reason() {
    staged(NARROW, |mut stage| async move {
        stage.typed("/config").key(KeyCode::Enter);
        let request = stage.asked().await;
        stage
            .emit(AgentEvent::Rejected {
                id: None,
                kind: None,
                request: Some(request),
                reason: "set up an account first".to_owned(),
            })
            .await;
        let rows = stage.until(|r| shows(r, "set up an account first")).await;
        assert!(!shows(&rows, "Loading settings"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn a_question_that_waits_on_you_closes_the_screen() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        stage
            .emit(AgentEvent::QuestionAsked {
                call_id: "q1".to_owned(),
                questions: vec![wisp_protocol::Question {
                    header: "Database".to_owned(),
                    question: "Which database should the service use?".to_owned(),
                    options: vec![
                        wisp_protocol::QuestionOption {
                            label: "PostgreSQL".to_owned(),
                            description: String::new(),
                        },
                        wisp_protocol::QuestionOption {
                            label: "SQLite".to_owned(),
                            description: String::new(),
                        },
                    ],
                    multi_select: false,
                    source: None,
                }],
            })
            .await;
        let rows = stage
            .until(|r| shows(r, "Which database should the service use?"))
            .await;
        assert!(!shows(&rows, "Cache lifetime"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn the_screen_needs_forty_columns_and_twelve_rows() {
    for (cols, rows) in [(39, 30), (80, 11)] {
        staged(Look::dark(cols, rows), |mut stage| async move {
            stage.typed("/config").key(KeyCode::Enter);
            stage.until(|r| said(r, "Make the terminal 40x12")).await;
            stage.nothing_sent().await;
        })
        .await;
    }
}

#[tokio::test]
async fn shrinking_below_the_minimum_says_so_and_growing_brings_it_back() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        let size = stage.screen.resize(10, 80);
        stage.input(Input::Resize(size));
        let rows = stage
            .until(|r| shows(r, "Make the terminal 40x12 for settings"))
            .await;
        assert!(
            rows[0].contains("Make the terminal 40x12 for settings"),
            "{rows:#?}"
        );
        let size = stage.screen.resize(30, 80);
        stage.input(Input::Resize(size));
        stage.until(|r| label(r) == "Model").await;
    })
    .await;
}

#[tokio::test]
async fn resizing_keeps_the_selected_setting_in_view() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config display.motion", snapshot_of).await;
        stage.char('G');
        stage.until(|r| label(r) == "Verbose").await;
        let size = stage.screen.resize(14, 80);
        stage.input(Input::Resize(size));
        let rows = stage
            .until(|r| shows(r, "enter turn on") && label(r) == "Verbose")
            .await;
        assert_eq!(label(&rows), "Verbose", "{rows:#?}");
        let size = stage.screen.resize(14, 120);
        stage.input(Input::Resize(size));
        stage
            .until(|r| label(r) == "Verbose" && shows(r, "▴"))
            .await;
    })
    .await;
}

#[tokio::test]
async fn ctrl_c_arms_quit_then_quits_when_idle() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        stage.ctrl('c');
        let rows = stage
            .until(|r| footer(r).contains("Press ctrl-c again to quit."))
            .await;
        assert!(shows(&rows, "Plugins"));
        stage.char('j');
        stage
            .until(|r| label(r) == "Effort" && !footer(r).contains("again to quit"))
            .await;
        stage.ctrl('c').ctrl('c');
        stage.quit().await;
    })
    .await;
}

#[tokio::test]
async fn ctrl_c_stops_a_running_turn_and_the_turn_runs_on_behind_the_screen() {
    staged(NARROW, |mut stage| async move {
        stage.typed("hello").key(KeyCode::Enter);
        let ClientCommand::Send { .. } = stage.next().await else {
            panic!("expected a send");
        };
        stage.emit(AgentEvent::turn_started(1)).await;
        stage.open("/config", snapshot_of).await;
        let rows = stage.screen.visible();
        assert!(rows.last().unwrap().contains("ctrl-c to stop"), "{rows:#?}");
        stage.ctrl('c');
        assert!(matches!(stage.next().await, ClientCommand::Cancel));
        let rows = stage.unchanged_for_a_while().await;
        assert!(shows(&rows, "Plugins"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn while_a_filter_is_typed_during_a_turn_ctrl_c_clears_it_as_the_footer_says() {
    staged(NARROW, |mut stage| async move {
        stage.typed("hello").key(KeyCode::Enter);
        let ClientCommand::Send { .. } = stage.next().await else {
            panic!("expected a send");
        };
        stage.emit(AgentEvent::turn_started(1)).await;
        stage.open("/config", snapshot_of).await;
        stage.char('/').typed("cache");
        let rows = stage.until(|r| shows(r, " Filter  cache")).await;
        let shown = footer(&rows);
        assert!(!shown.contains("ctrl-c to stop"), "{rows:#?}");
        assert!(shown.contains("enter done"), "{rows:#?}");
        stage.ctrl('c');
        let rows = stage.until(|r| label(r) == "Model").await;
        assert!(shows(&rows, " Filter"), "{rows:#?}");
        stage.nothing_sent().await;
    })
    .await;
}

#[tokio::test]
async fn a_click_selects_a_row_and_the_wheel_scrolls() {
    staged(Look::dark(80, 16), |mut stage| async move {
        stage.open("/config display.motion", snapshot_of).await;
        let rows = stage.screen.visible();
        let at = rows.iter().position(|r| r.starts_with("   Mouse")).unwrap();
        stage.click(10, u16::try_from(at).unwrap());
        stage.until(|r| label(r) == "Mouse").await;
        for _ in 0..2 {
            let before = stage.screen.steady();
            stage.wheel(MouseKind::WheelDown);
            stage.changed_from(&before).await;
        }
        let rows = stage.screen.visible();
        assert!(!shows(&rows, "   Model  "), "{rows:#?}");
        assert!(
            rows.iter().any(|r| r.starts_with(" \u{203a} ")),
            "{rows:#?}"
        );
    })
    .await;
}

#[tokio::test]
async fn the_cursor_sits_at_the_first_column_of_the_selected_row() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        stage.char('j');
        let rows = stage.until(|r| label(r) == "Effort").await;
        let at = rows
            .iter()
            .position(|r| r.starts_with(" \u{203a} Effort"))
            .unwrap();
        assert_eq!(stage.screen.cursor(), (u16::try_from(at).unwrap(), 0));
        stage.char('/');
        let rows = stage.until(|r| footer(r).contains("enter done")).await;
        let filter = rows.iter().position(|r| r.starts_with(" Filter")).unwrap();
        assert_eq!(stage.screen.cursor().0, u16::try_from(filter).unwrap());
    })
    .await;
}

#[tokio::test]
async fn question_mark_shows_the_whole_help_at_every_width() {
    for look in [Look::dark(40, 16), NARROW, WIDE] {
        staged(look, |mut stage| async move {
            stage.open("/config cache_ttl", snapshot_of).await;
            stage.char('?');
            let rows = stage.until(|r| shows(r, "Choices")).await;
            let text = rows.join(" ");
            assert!(
                text.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("5m for continuous loops"),
                "{rows:#?}"
            );
            stage.key(KeyCode::Esc);
            let rows = stage.until(|r| !shows(r, "Choices")).await;
            assert_eq!(label(&rows), "Cache lifetime");
        })
        .await;
    }
}

#[tokio::test]
async fn the_display_rows_take_the_terminal_s_own_resolution() {
    let mut forced = display();
    forced.set(
        "display.color",
        SettingValue::Text("none".to_owned()),
        "NO_COLOR is set",
    );
    staged_with(WIDE, forced, |mut stage| async move {
        stage.open("/config display.color", snapshot_of).await;
        let rows = stage.screen.visible();
        assert!(selected(&rows).contains("none"), "{rows:#?}");
        assert!(!selected(&rows).contains("forced"), "{rows:#?}");
        assert!(shows(&rows, "Set outside Wisp."), "{rows:#?}");
        assert!(!shows(&rows, "NO_COLOR"), "{rows:#?}");
        stage.char('?');
        stage.until(|r| shows(r, "NO_COLOR is set")).await;
    })
    .await;
}

#[tokio::test]
async fn suggestions_list_setting_names_after_config() {
    staged(NARROW, |mut stage| async move {
        stage.typed("/config comp");
        stage.until(|r| shows(r, "compact_at")).await;
        stage.key(KeyCode::Tab).key(KeyCode::Enter);
        let request = stage.asked().await;
        stage.until(loading).await;
        stage
            .emit(AgentEvent::SettingsSnapshot(snapshot_of(request)))
            .await;
        let rows = stage.until(loaded).await;
        assert_eq!(label(&rows), "Compact automatically");
    })
    .await;
}

#[tokio::test]
async fn a_change_to_the_endpoints_reads_a_new_snapshot_keeping_the_filter_and_selection() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config", snapshot_of).await;
        stage.char('/').typed("cache");
        stage.key(KeyCode::Enter).key(KeyCode::End);
        let before = stage.until(|r| label(r) == "Cache diagnostics").await;
        stage
            .emit(AgentEvent::EndpointsChanged {
                endpoints: Vec::new(),
            })
            .await;
        assert!(!shows(&before, "5m"), "{before:#?}");
        let request = stage.asked().await;
        let mut refreshed = snapshot_of(request);
        set(
            &mut refreshed.settings,
            "cache_ttl",
            Layer::Session,
            "5m",
            "session",
        );
        stage.emit(AgentEvent::SettingsSnapshot(refreshed)).await;
        let rows = stage
            .until(|r| {
                r.iter()
                    .any(|r| r.contains("Cache lifetime") && r.contains("5m"))
            })
            .await;
        assert!(shows(&rows, " Filter  cache"), "{rows:#?}");
        assert!(!shows(&rows, " Messages"), "{rows:#?}");
        assert!(!shows(&rows, "Effort"), "{rows:#?}");
        assert_eq!(label(&rows), "Cache diagnostics");
    })
    .await;
}

#[tokio::test]
async fn config_typed_into_the_endpoint_card_stays_in_the_card() {
    let card = Some(super::endpoints::card_setup());
    staged_options(NARROW, display(), card, |mut stage| async move {
        stage
            .input(Input::Paste("/endpoint add".to_owned()))
            .key(KeyCode::Enter);
        stage.until(|r| shows(r, ADD_TITLE)).await;
        stage.typed("/config").key(KeyCode::Enter);
        stage.nothing_sent().await;
        let rows = stage.unchanged_for_a_while().await;
        assert!(shows(&rows, ADD_TITLE), "{rows:#?}");
        assert!(!shows(&rows, " Settings"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn endpoint_add_typed_on_the_settings_screen_opens_no_card() {
    let card = Some(super::endpoints::card_setup());
    staged_options(NARROW, display(), card, |mut stage| async move {
        stage.open("/config zzqq", snapshot_of).await;
        stage
            .input(Input::Paste("/endpoint add".to_owned()))
            .key(KeyCode::Enter);
        stage.typed("/endpoint add").key(KeyCode::Enter);
        stage.nothing_sent().await;
        let rows = stage.unchanged_for_a_while().await;
        assert!(shows(&rows, " Settings"), "{rows:#?}");
        assert!(!shows(&rows, ADD_TITLE), "{rows:#?}");
        for _ in 0..3 {
            let before = stage.screen.steady();
            if !on_screen(&before) {
                break;
            }
            stage.key(KeyCode::Esc);
            stage.changed_from(&before).await;
        }
        let rows = stage.screen.visible();
        assert!(!shows(&rows, " Settings"), "{rows:#?}");
        assert!(!shows(&rows, ADD_TITLE), "{rows:#?}");
    })
    .await;
}

async fn replayed<D, F>(look: Look, drive: D)
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    let screen = Screen::new(look.rows, look.cols);
    let harness = replay::Settings {
        model: MODEL.to_owned(),
        described: replay::Described {
            settings: settings(),
            saved: snapshot_of(RequestId(0)).saved,
            file: FileState::Ok {
                path_display: "the file given with --config".to_owned(),
            },
            max_tokens: 64_000,
            ..replay::Described::default()
        },
        ..replay::Settings::default()
    };
    let script = replay::Script::new(Vec::new()).with_settings(harness);
    let (commands, events) = replay::spawn(script, replay::PACE);
    let (inputs, received) = mpsc::unbounded_channel();
    let (_, unused) = mpsc::channel(1);
    let (events_in, _) = mpsc::channel(1);
    let mut facts = super::facts();
    facts.model = MODEL.to_owned();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts,
        events,
        commands,
        resumed: None,
    };
    let mut options = Options::new(Theme::new(look.mode, Depth::TrueColor), look.glyphs);
    options.code = Start::Stalled;
    options.motion = MotionPolicy::Reduced;
    options.display = display();
    let mut console = screen.clone();
    let app = run(&mut console, received, session, options);
    let stage = Stage {
        screen: screen.clone(),
        inputs,
        events: events_in,
        commands: unused,
        skip_usage: false,
    };
    let driver = async move {
        stage.until(ready).await;
        drive(stage).await;
    };
    let (exit, ()) = tokio::join!(app, driver);
    exit.unwrap();
}

#[tokio::test]
async fn under_replay_a_save_is_refused_and_the_row_leaves_saving() {
    replayed(NARROW, |stage| async move {
        stage.typed("/config debug_cache").key(KeyCode::Enter);
        let rows = stage.until(|r| label(r) == "Cache diagnostics").await;
        assert_eq!(label(&rows), "Cache diagnostics");
        stage.key(KeyCode::Enter);
        let rows = stage
            .until(|r| shows(r, replay::NO_SAVES) && !selected(r).contains("saving"))
            .await;
        assert!(!selected(&rows).contains("saving"), "{rows:#?}");
        assert!(shows(&rows, replay::NO_SAVES), "{rows:#?}");
        stage.close_settings().await;
        stage.typed("/config retry.budget_usd").key(KeyCode::Enter);
        stage.until(|r| label(r) == "Retry budget").await;
        stage
            .key(KeyCode::Enter)
            .ctrl('u')
            .typed("0.5")
            .key(KeyCode::Enter);
        let rows = stage
            .until(|r| {
                let row = selected(r);
                shows(r, replay::NO_SAVES)
                    && row.contains("none")
                    && !row.contains("saving")
                    && !row.contains("0.5")
            })
            .await;
        assert!(shows(&rows, replay::NO_SAVES), "{rows:#?}");
        assert!(selected(&rows).contains("none"), "{rows:#?}");
        stage.close_settings().await;
        let rows = stage
            .until(|r| said(r, "Settings not changed: replay doesn't save settings."))
            .await;
        assert!(
            said(&rows, "Settings not changed: replay doesn't save settings."),
            "{rows:#?}"
        );
    })
    .await;
}

async fn editing(look: Look, line: &'static str, keys: &'static [&'static str]) -> String {
    let shot = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let kept = std::rc::Rc::clone(&shot);
    staged(look, |mut stage| async move {
        stage.open(line, snapshot_of).await;
        stage.key(KeyCode::Enter);
        for key in keys {
            match *key {
                "left" => stage.key(KeyCode::Left),
                "tab" => stage.key(KeyCode::Tab),
                "clear" => stage.ctrl('u'),
                text => stage.typed(text),
            };
        }
        let typed: String = keys
            .iter()
            .filter(|k| !matches!(**k, "left" | "clear" | "tab"))
            .copied()
            .collect();
        let chose = keys.contains(&"left");
        *kept.borrow_mut() = stage
            .shot_when(|r| {
                let edited = if chose {
                    shows(r, "\u{203a} 5m") || shows(r, "> 5m")
                } else {
                    shows(r, &format!("New value      {typed}"))
                };
                edited
                    && footer(r).contains("esc cancel")
                    && (!keys.contains(&"tab") || footer(r).contains("tab all models"))
            })
            .await;
    })
    .await;
    shot.take()
}

#[tokio::test]
async fn an_open_editor_and_choice_row_in_three_widths_and_both_glyph_sets() {
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for (cols, rows) in SIZES {
            let look = Look {
                glyphs,
                ..Look::dark(cols, rows)
            };
            let shot = editing(look, "/config compact_at", &["95%"]).await;
            snapshot("editing", look, &shot);
            let shot = editing(look, "/config cache_ttl", &["left"]).await;
            snapshot("choosing", look, &shot);
        }
    }
    let look = Look::dark(80, 30);
    let shot = editing(look, "/config retry.budget_usd", &["clear", "half"]).await;
    snapshot("invalid", look, &shot);
}

async fn shown_after(
    look: Look,
    line: &'static str,
    answer: fn(RequestId) -> SettingsSnapshot,
    keys: &'static [KeyCode],
    done: fn(&[String]) -> bool,
) -> String {
    let shot = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let kept = std::rc::Rc::clone(&shot);
    staged(look, |mut stage| async move {
        stage.open(line, answer).await;
        for key in keys {
            stage.key(*key);
        }
        *kept.borrow_mut() = stage.shot_when(done).await;
    })
    .await;
    shot.take()
}

#[tokio::test]
async fn a_picker_a_checklist_and_the_for_row_on_the_screen() {
    for look in [NARROW, Look::dark(40, 16)] {
        let shot = shown_after(look, "/config model", snapshot_of, &[KeyCode::Enter], |r| {
            footer(r).contains("esc cancel")
        })
        .await;
        snapshot("picking", look, &shot);
    }
    let shot = shown_after(NARROW, "/config tools", tools_of, &[KeyCode::Enter], |r| {
        shows(r, "[x] bash")
    })
    .await;
    snapshot("checklist", NARROW, &shot);
    let shot = editing(NARROW, "/config compact_at", &["600000", "tab"]).await;
    snapshot("for", NARROW, &shot);
}

#[tokio::test]
async fn the_endpoint_reset_quotes_no_cost_when_the_start_rules_choose() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config endpoint", snapshot_of).await;
        let rows = stage.screen.visible();
        assert!(said(&rows, "d back to the start rules"), "{rows:#?}");
        assert!(
            !said(&rows, "the start rules (default) \u{b7}"),
            "{rows:#?}"
        );
    })
    .await;
}

fn two_endpoints() -> Vec<EndpointFacts> {
    vec![
        endpoint("direct", Endpoint::Official),
        endpoint("manifest", Endpoint::Custom),
    ]
}

const MODEL_CANARY: &str = "sk-ant-api03-CANARYMODEL0000";

fn no_canary(rows: &[String]) {
    let all = rows.join("\n");
    assert!(!all.contains("CANARY"), "{all}");
    assert!(!all.contains("sk-ant-api03"), "{all}");
}

#[tokio::test]
async fn a_key_typed_or_pasted_as_a_model_is_never_saved_or_shown() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config model", snapshot_of).await;
        stage.key(KeyCode::Enter).typed(MODEL_CANARY);
        let rows = stage.until(|r| said(r, "this looks like a key,")).await;
        let typed_from = stage.screen.seen().len();
        no_canary(&rows);
        stage.key(KeyCode::Enter);
        stage.nothing_sent().await;
        stage.key(KeyCode::Esc);
        stage.until(|r| !said(r, "this looks like a key,")).await;
        stage
            .key(KeyCode::Enter)
            .input(Input::Paste(MODEL_CANARY.to_owned()));
        let rows = stage.until(|r| said(r, "this looks like a key,")).await;
        no_canary(&rows);
        stage.key(KeyCode::Enter);
        stage.nothing_sent().await;
        stage.key(KeyCode::Esc).key(KeyCode::Esc);
        let rows = stage.until(|r| !on_screen(r)).await;
        no_canary(&rows);
        let seen = stage.screen.seen();
        assert!(!seen[typed_from..].iter().any(|r| r.contains("CANARY")));
    })
    .await;
}

#[tokio::test]
async fn a_turn_that_ends_while_the_screen_loads_asks_nothing_more() {
    staged(NARROW, |mut stage| async move {
        stage.emit(AgentEvent::turn_started(1)).await;
        stage.typed("/config").key(KeyCode::Enter);
        let request = stage.asked().await;
        stage.emit(super::completed()).await;
        stage.nothing_sent().await;
        stage
            .emit(AgentEvent::SettingsSnapshot(snapshot_of(request)))
            .await;
        stage.nothing_sent().await;
    })
    .await;
}

#[tokio::test]
async fn enter_on_the_current_model_or_effort_in_the_screen_saves_it() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config model", snapshot_of).await;
        stage.key(KeyCode::Enter).key(KeyCode::Enter);
        let (_, changes) = pair_saved(stage.next().await);
        assert_eq!(
            changes,
            ClientCommand::shortcut_changes(None, Some(MODEL.to_owned()), None)
        );
        stage.key(KeyCode::Esc);
        stage.open("/config effort", snapshot_of).await;
        stage.key(KeyCode::Enter).key(KeyCode::Enter);
        let (_, changes) = pair_saved(stage.next().await);
        assert_eq!(
            changes,
            ClientCommand::shortcut_changes(None, None, Some(wisp_protocol::Effort::High))
        );
    })
    .await;
}

#[tokio::test]
async fn closing_the_screen_drops_its_picker_and_a_paste_goes_to_the_picker() {
    staged(NARROW, |mut stage| async move {
        stage.open("/config model", snapshot_of).await;
        stage.key(KeyCode::Enter);
        stage.input(Input::Paste("sonnet".to_owned()));
        stage.until(|r| said(r, "matching \"sonnet\"")).await;
        stage
            .emit(AgentEvent::QuestionAsked {
                call_id: "q1".to_owned(),
                questions: vec![wisp_protocol::Question {
                    header: "Pick".to_owned(),
                    question: "Which one?".to_owned(),
                    options: vec![
                        wisp_protocol::QuestionOption {
                            label: "A".to_owned(),
                            description: String::new(),
                        },
                        wisp_protocol::QuestionOption {
                            label: "B".to_owned(),
                            description: String::new(),
                        },
                    ],
                    multi_select: false,
                    source: None,
                }],
            })
            .await;
        let rows = stage.until(|r| shows(r, "Which one?")).await;
        assert!(!shows(&rows, " Settings"), "{rows:#?}");
        assert!(!said(&rows, "matching \"sonnet\""), "{rows:#?}");
        assert!(!shows(&rows, "sonnet-5-5"), "{rows:#?}");
        stage.nothing_sent().await;
    })
    .await;
}

#[tokio::test]
async fn the_footer_hides_stops_while_a_picker_is_open() {
    staged(NARROW, |mut stage| async move {
        stage.emit(AgentEvent::turn_started(1)).await;
        stage.open("/config model", snapshot_of).await;
        let rows = stage.screen.visible();
        assert!(
            rows.last()
                .unwrap()
                .contains(&format!("{} {}", STOP_KEY.0, STOP_KEY.1)),
            "{rows:#?}"
        );
        stage.key(KeyCode::Enter);
        let rows = stage.until(|r| footer(r).contains("enter save")).await;
        assert!(
            !rows
                .last()
                .unwrap()
                .contains(&format!("{} {}", STOP_KEY.0, STOP_KEY.1)),
            "{rows:#?}"
        );
    })
    .await;
}

#[tokio::test]
async fn the_accounts_page_saves_the_chosen_account() {
    staged_on(
        NARROW,
        display(),
        None,
        two_endpoints(),
        |mut stage| async move {
            stage.open("/config endpoint", entries::with_entries).await;
            stage.key(KeyCode::Enter);
            let rows = stage.until(|r| shows(r, "enter use it")).await;
            assert!(shows(&rows, "direct"), "{rows:#?}");
            stage.key(KeyCode::Enter);
            let (_, changes) = pair_saved(stage.next().await);
            assert_eq!(
                changes,
                ClientCommand::shortcut_changes(Some("direct".to_owned()), None, None)
            );
            stage
                .until(|r| selected(r).contains("saving\u{2026}"))
                .await;
        },
    )
    .await;
}

#[tokio::test]
async fn a_chained_model_and_effort_pick_saves_both_and_names_both_old_values() {
    staged(NARROW, |mut stage| async move {
        let mut xhigh = effort_changed();
        if let AgentEvent::SettingsChanged { effort, .. } = &mut xhigh {
            *effort = wisp_protocol::Effort::Xhigh;
        }
        stage.emit(xhigh).await;
        stage.open("/config model", snapshot_of).await;
        stage.key(KeyCode::Enter).typed("sonnet-4-6");
        stage.key(KeyCode::Enter);
        stage.until(|r| shows(r, "medium")).await;
        stage.nothing_sent().await;
        stage.key(KeyCode::Enter);
        let (request, changes) = pair_saved(stage.next().await);
        let model = changes.iter().find(|c| c.target == named("model")).unwrap();
        assert_eq!(
            model.value,
            Some(SettingValue::Text("claude-sonnet-4-6".to_owned()))
        );
        assert!(
            changes.iter().any(|c| c.target == named("effort")),
            "{changes:?}"
        );
        let change = applied(
            "model",
            Some(SettingValue::Text("claude-sonnet-4-6".to_owned())),
            When::NextTurn,
        );
        stage
            .emit(switched(request, "claude-sonnet-4-6", change))
            .await;
        let again = stage.asked().await;
        stage
            .emit(AgentEvent::SettingsSnapshot(snapshot_of(again)))
            .await;
        stage.close_settings().await;
        let rows = stage.until(|r| !on_screen(r)).await;
        assert!(said(&rows, "(was claude-opus-5-5, high this"), "{rows:#?}");
        assert!(said(&rows, "claude-opus-5-5, medium saved)"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn the_first_screen_leaves_a_gap_before_plugins_whenever_it_fits() {
    for (rows, spaced) in [(16, false), (17, true), (18, true), (30, true), (40, true)] {
        staged(Look::dark(80, rows), |mut stage| async move {
            stage.open("/config", snapshot_of).await;
            let shown = stage.until(loaded).await;
            let at = shown
                .iter()
                .position(|l| l.trim_start().starts_with("Plugins "))
                .unwrap();
            assert_eq!(shown[at - 1].trim().is_empty(), spaced, "{shown:#?}");
        })
        .await;
    }
}

#[tokio::test]
async fn a_short_filtered_list_is_spaced_only_from_40_rows() {
    for rows in [39, 40] {
        staged(Look::dark(120, rows), |mut stage| async move {
            stage.open("/config", snapshot_of).await;
            stage.char('/').typed("cache");
            let shown = stage.until(|r| shows(r, " Filter  cache")).await;
            let list: Vec<_> = shown
                .iter()
                .map(|r| r.split('│').next().unwrap_or_default().trim_end())
                .collect();
            let headings: Vec<_> = list
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    ["Agents", "Display", "Advanced"]
                        .iter()
                        .any(|title| **line == format!(" {title}"))
                })
                .map(|(i, _)| i)
                .collect();
            assert!(headings.len() >= 2, "{shown:#?}");
            assert_eq!(list[headings[1] - 1].is_empty(), rows >= 40, "{shown:#?}");
        })
        .await;
    }
}

#[path = "settings_screen/entries.rs"]
mod entries;

pub(super) fn catalog_plugin(
    id: &str,
    summary: &str,
    tools: Vec<wisp_protocol::plugin::ToolEntry>,
) -> wisp_protocol::PluginEntry {
    wisp_protocol::PluginEntry {
        prompt_sections: Vec::new(),
        id: id.to_owned(),
        version: "0.1.0".to_owned(),
        summary: summary.to_owned(),
        source: wisp_protocol::PluginSource::Bundled,
        installed: true,
        enabled: true,
        tools,
        commands: Vec::new(),
        hooks: Vec::new(),
        slots: Vec::new(),
        settings: Vec::new(),
        problems: Vec::new(),
    }
}

fn plugins_of(request: RequestId) -> SettingsSnapshot {
    let mut out = snapshot_of(request);
    let mut search = catalog_tool("web_search", None, true);
    search.native = true;
    out.plugins = vec![
        catalog_plugin(
            "shell",
            "Runs shell commands without asking, and their processes",
            vec![
                catalog_tool("bash", None, true),
                catalog_tool("process", Some("bash"), true),
            ],
        ),
        catalog_plugin(
            "todos",
            "Keeps the agent's checklist for multi-step work",
            vec![catalog_tool("write_todos", None, false)],
        ),
        catalog_plugin(
            "web-search",
            "Provider web search, where the account and model have it",
            vec![search],
        ),
    ];
    for row in out
        .settings
        .iter_mut()
        .filter(|r| r.name.spec().name == "tools")
    {
        row.choices = ["bash", "web_search", "write_todos"]
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
    }
    out
}

fn plugin_page_of(request: RequestId) -> SettingsSnapshot {
    let mut out = snapshot_of(request);
    let mut window = crate::sidebar::slot("context/window", "Context");
    window.default_on = false;
    let fill = crate::sidebar::slot("context/fill", "Fill");
    out.plugins = vec![crate::shared::plugin("context", true, vec![window, fill])];
    out
}

#[tokio::test]
async fn a_plugins_page_names_its_local_parts_and_the_selected_switch() {
    let look = Look::dark(80, 30);
    let shot = shown_after(
        look,
        "/config",
        plugin_page_of,
        &[KeyCode::End, KeyCode::Up, KeyCode::Enter, KeyCode::Right],
        |r| shows(r, "Context line: tell the model"),
    )
    .await;
    assert!(shot.contains("Context line  on"), "{shot}");
    assert!(shot.contains("window        off"), "{shot}");
    assert!(shot.contains("fill          on"), "{shot}");
    assert!(shot.contains("enter turn off"), "{shot}");
    snapshot("plugin-page", look, &shot);
}

fn defined_of(request: RequestId) -> SettingsSnapshot {
    let mut plugin = crate::shared::defined_plugin("idle");
    let mut out = plugins_of(request);
    out.context.endpoint = "work".into();
    out.context.model = "opus-5-5".into();
    for (name, value) in [
        ("endpoint", "work"),
        ("model", "opus-5-5"),
        ("effort", "high"),
    ] {
        set(&mut out.settings, name, Layer::Config, value, name);
        out.saved.retain(|saved| saved.path.spec().name != name);
        out.saved.push(saved(name, Place::Global, value, 4));
    }
    for tool in out.plugins.iter_mut().flat_map(|plugin| &mut plugin.tools) {
        tool.on = true;
        tool.default_on = true;
    }
    plugin.hooks = ["seen", "note", "done"]
        .into_iter()
        .enumerate()
        .map(|(order, name)| {
            hook_entry(
                &format!("idle/{name}"),
                "before-turn",
                order as u32,
                None,
                None,
            )
        })
        .collect();
    out.plugins.push(plugin);
    out.often = vec!["plugins.idle.minutes".into()];
    out
}

#[tokio::test]
async fn a_catalog_declares_a_plugins_setting_before_it_is_edited_on_its_page() {
    let look = Look::dark(80, 30);
    staged(look, |mut stage| async move {
        assert!(find("plugins.idle.minutes").is_none());
        stage
            .emit(crate::shared::catalog(defined_of(RequestId(0)).plugins))
            .await;
        assert!(find("plugins.idle.minutes").is_some());
        stage.open("/config plugins.idle.minutes", defined_of).await;
        stage.key(KeyCode::Enter).ctrl('u').typed("5");
        let shot = stage.shot_when(|r| shows(r, "New value      5")).await;
        assert!(shot.contains("a whole number, 1 to 60"), "{shot}");
        snapshot("plugin-editing", look, &shot);
    })
    .await;
}

#[tokio::test]
async fn a_snapshot_declares_a_plugins_setting_before_it_is_shown_under_often_used() {
    let look = Look::dark(40, 16);
    staged(look, |mut stage| async move {
        assert!(find("plugins.idle.minutes").is_none());
        stage
            .emit(AgentEvent::SettingsSnapshot(defined_of(RequestId(0))))
            .await;
        assert!(find("plugins.idle.minutes").is_some());
        let mut shot = defined_of(RequestId(1));
        set(
            &mut shot.settings,
            "plugins.idle.minutes",
            Layer::Config,
            "5",
            "plugins.idle.minutes",
        );
        shot.saved
            .push(saved("plugins.idle.minutes", Place::Global, "5", 4));
        stage.typed("/config").key(KeyCode::Enter);
        shot.answers = stage.asked().await;
        stage.emit(AgentEvent::SettingsSnapshot(shot)).await;
        stage.until(loaded).await;
        for _ in 0..7 {
            stage.key(KeyCode::Down);
        }
        let shot = stage.shot_when(|r| marked(r) == "Idle after").await;
        assert!(shot.contains("Often used"), "{shot}");
        snapshot("plugin-often", look, &shot);
    })
    .await;
}

fn sectioned_of(request: RequestId) -> SettingsSnapshot {
    use wisp_protocol::plugin::{PromptSectionEntry, SectionPlace};
    let mut out = plugins_of(request);
    let mut plugin = catalog_plugin("rules", "Project rules and notes for the model", Vec::new());
    plugin.prompt_sections = vec![
        PromptSectionEntry {
            name: "rules".to_owned(),
            place: SectionPlace::System,
        },
        PromptSectionEntry {
            name: "notes".to_owned(),
            place: SectionPlace::Lead,
        },
    ];
    out.plugins.push(plugin);
    out
}

#[tokio::test]
async fn a_plugins_prompt_sections_are_listed_read_only_in_its_detail() {
    let look = Look::dark(120, 40);
    let shot = shown_after(
        look,
        "/config rules",
        sectioned_of,
        &[KeyCode::Char('?')],
        |r| shows(r, "prompt section: notes"),
    )
    .await;
    assert!(
        shot.contains("prompt section: rules \u{b7} in the project block"),
        "{shot}"
    );
    snapshot("plugin-sections", look, &shot);
}

#[tokio::test]
async fn the_tools_checklist_and_the_plugins_section_in_both_themes_and_two_widths() {
    for (cols, rows) in [(120, 40), (80, 30)] {
        let look = Look::dark(cols, rows);
        let shot = shown_after(
            look,
            "/config tools",
            plugins_of,
            &[KeyCode::Enter, KeyCode::Right],
            |r| shows(r, "\u{203a}     [x] process (comes with bash)"),
        )
        .await;
        snapshot("tools-catalog", look, &shot);
        let shot = shown_after(look, "/config shell", plugins_of, &[], |r| {
            shows(r, "shell: runs shell commands")
        })
        .await;
        snapshot("plugins", look, &shot);
    }
}

fn switched_of(request: RequestId) -> SettingsSnapshot {
    use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY};
    let mut out = plugins_of(request);
    let mut gone = catalog_plugin(
        "time",
        "Stamps each message with the time it was sent",
        Vec::new(),
    );
    gone.installed = false;
    gone.enabled = false;
    let mut back = catalog_plugin(
        "context",
        "Tells the model how full its context is",
        Vec::new(),
    );
    back.installed = false;
    back.enabled = false;
    out.plugins.extend([back, gone]);
    out.plugins.sort_by(|a, b| a.id.cmp(&b.id));
    out.placed = vec![
        switch_placed("context", INSTALLED_KEY, true),
        switch_placed("time", INSTALLED_KEY, false),
        switch_placed("todos", ENABLED_KEY, false),
        switch_placed("web-search", INSTALLED_KEY, false),
    ];
    out
}

fn hooked_of(request: RequestId) -> SettingsSnapshot {
    use wisp_protocol::plugin::HookDisabled;
    let mut out = plugins_of(request);
    let mut time = catalog_plugin(
        "time",
        "Stamps each message with the time it was sent",
        Vec::new(),
    );
    time.hooks = vec![hook_entry(
        "time/sent",
        "before-turn",
        0,
        Some("time"),
        None,
    )];
    let mut check = catalog_plugin(
        "self-check",
        "Asks the model to check its work before a turn ends",
        Vec::new(),
    );
    check.enabled = false;
    check.hooks = vec![
        hook_entry(
            "self-check/review",
            "before-end",
            0,
            Some("self_check.before_end"),
            Some(HookDisabled::PluginOff),
        ),
        hook_entry(
            "self-check/worked",
            "after-response",
            0,
            Some("self_check.before_end"),
            Some(HookDisabled::PluginOff),
        ),
    ];
    for plugin in &mut out.plugins {
        if plugin.id == "shell" {
            plugin.hooks = vec![hook_entry("shell/processes", "before-turn", 1, None, None)];
        }
    }
    out.plugins.extend([time, check]);
    out.plugins.sort_by(|a, b| a.id.cmp(&b.id));
    out.placed = vec![wisp_protocol::settings::PlacedValue {
        path: wisp_protocol::settings::SitePath::plugin_hook("shell", "processes").unwrap(),
        value: Some(SettingValue::Bool(false)),
        layer: Layer::Session,
    }];
    out
}

#[tokio::test]
async fn hook_rows_show_their_state_place_and_switch() {
    for (cols, rows) in [(120, 40), (80, 30)] {
        let look = Look::dark(cols, rows);
        let shot = shown_after(look, "/config hook", hooked_of, &[KeyCode::Down], |r| {
            shows(r, "enter its setting")
        })
        .await;
        snapshot("hook-rows", look, &shot);
    }
}

#[tokio::test]
async fn each_plugin_row_state_its_keys_the_remove_question_and_the_tools_refusal() {
    for (cols, rows) in [(120, 40), (80, 30)] {
        let look = Look::dark(cols, rows);
        let shot = shown_after(look, "/config todos", switched_of, &[], |r| {
            shows(r, "enter turn on")
        })
        .await;
        snapshot("plugin-states", look, &shot);
    }
    let look = Look::dark(120, 40);
    let shot = shown_after(
        look,
        "/config todos",
        switched_of,
        &[KeyCode::Char('d')],
        |r| shows(r, "Remove todos?"),
    )
    .await;
    snapshot("plugin-remove-question", look, &shot);
    let shot = shown_after(look, "/config web-search", switched_of, &[], |r| {
        shows(r, "enter add back")
    })
    .await;
    snapshot("plugin-removed", look, &shot);
    let shot = shown_after(
        look,
        "/config tools",
        switched_of,
        &[
            KeyCode::Enter,
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Right,
        ],
        |r| shows(r, "which is off; turn it on in"),
    )
    .await;
    snapshot("plugin-tools-refusal", look, &shot);
}

#[tokio::test]
async fn the_applying_catalog_clears_a_rows_pending_mark_on_the_open_screen() {
    staged(Look::dark(120, 40), |mut stage| async move {
        stage.open("/config todos", switched_of).await;
        stage
            .until(|r| shows(r, "off \u{b7} pending \u{b7} next turn"))
            .await;
        let mut catalog = switched_of(RequestId(0)).plugins;
        for plugin in &mut catalog {
            if plugin.id == "todos" {
                plugin.enabled = false;
            }
        }
        stage
            .emit(AgentEvent::CatalogChanged {
                catalog: wisp_protocol::Catalog { plugins: catalog },
            })
            .await;
        let rows = stage
            .until(|r| {
                !shows(r, "pending") && r.iter().any(|r| r.trim_end().ends_with("todos  off"))
            })
            .await;
        assert!(!shows(&rows, "pending"), "{rows:#?}");
        stage.asked().await;
    })
    .await;
}

#[tokio::test]
async fn more_settings_a_group_and_accounts_have_their_own_pages() {
    let look = Look::dark(80, 30);
    staged(look, |mut stage| async move {
        stage.open("/config endpoint", entries::with_entries).await;
        stage.key(KeyCode::Enter);
        let shot = stage
            .shot_when(|r| shows(r, " Accounts") && shows(r, "Add an API key"))
            .await;
        snapshot("accounts", look, &shot);
        stage
            .key(KeyCode::Left)
            .key(KeyCode::End)
            .key(KeyCode::Enter);
        let shot = stage
            .shot_when(|r| shows(r, " More settings") && label(r) == "Chat")
            .await;
        snapshot("more", look, &shot);
        stage.key(KeyCode::Down).key(KeyCode::Enter);
        let shot = stage
            .shot_when(|r| shows(r, " Compaction") && label(r) == "Compact automatically")
            .await;
        snapshot("group", look, &shot);
    })
    .await;
}

fn often_of(request: RequestId) -> SettingsSnapshot {
    let mut out = plugins_of(request);
    out.often = ["context.verbatim_user_tokens", "display.mouse", "time"]
        .map(str::to_owned)
        .to_vec();
    for (name, value) in [
        ("context.verbatim_user_tokens", "6000"),
        ("display.mouse", "off"),
        ("time", "utc"),
    ] {
        set(&mut out.settings, name, Layer::Config, value, name);
        out.saved.push(saved(name, Place::Global, value, 4));
    }
    out
}

#[tokio::test]
async fn often_used_shows_three_rows_on_the_first_screen() {
    let look = Look::dark(80, 30);
    let mut shown = display();
    shown.set(
        "display.mouse",
        SettingValue::Bool(false),
        "config.toml display.mouse",
    );
    staged_with(look, shown, |mut stage| async move {
        stage.open("/config", often_of).await;
        for _ in 0..8 {
            stage.key(KeyCode::Down);
        }
        let shot = stage.shot_when(|r| marked(r) == "Mouse").await;
        assert!(shot.contains("Often used"));
        assert!(shot.find("Your messages kept").unwrap() < shot.find("Mouse").unwrap());
        snapshot("often", look, &shot);
    })
    .await;
}
