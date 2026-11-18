#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any setup error"
)]

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::future::Future;
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use ratatui_core::layout::Size;
use tokio::sync::mpsc;
use tokio::time::sleep;
use wisp_protocol::{AgentEvent, BlockKind, CacheTtl, ErrorKind, Usage};
use wisp_render::chrome::account::{AccountKind, mockup_mark};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::theme::{Depth, Mode, Theme, Token};
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::{Feed, Start, default_feed};
use wisp_tui::app::pager::return_keys;
use wisp_tui::app::replay::{self, Script};
use wisp_tui::app::{
    AGAIN_TO_QUIT, AppError, Credential, DRAG_CAPTURED, DRAG_NATIVE, Exit, Finished, MAX_RESTORES,
    NEW_KEY, NOTE_HANG, Options, Session, SessionFacts, TMUX_MOUSE_OFF, note_rows, recovered, run,
    run_to_end,
};
use wisp_tui::term::clipboard::Osc52;
use wisp_tui::term::input::HUNG_UP;
use wisp_tui::term::sync::{CursorAfter, frame};
use wisp_tui::term::{
    Console, Halted, Input, Key, KeyCode, Modes, Mouse, MouseKind, ProbePlan, ProbeReport, Written,
};

#[path = "app/pickers.rs"]
mod pickers;

#[path = "app/scenes.rs"]
mod scenes;

#[path = "support/models.rs"]
mod models;

#[path = "support/shared.rs"]
mod shared;

#[path = "../../wisp-render/tests/support/events.rs"]
mod events;

#[path = "support/fake_tool.rs"]
mod fake_tool;

#[path = "support/pick.rs"]
mod pick;

#[path = "support/scratch.rs"]
mod scratch;

#[path = "../../wisp-render/src/testing.rs"]
mod testing;

#[path = "support/spots.rs"]
mod spots;

use crate::events::{End, Ending, cost, nz, turn_ended};
use crate::shared::fixed_now;
use crate::spots::rows_of;
use crate::testing::grammars;
use models::{models, multipliers};
use wisp_render::chrome::Endpoint;

#[path = "app/select.rs"]
mod select;

#[path = "app/composer_box.rs"]
mod composer_box;

#[path = "app/docked.rs"]
mod docked;

#[path = "app/idle_orb.rs"]
mod idle_orb;

#[path = "app/images.rs"]
mod images;

#[path = "app/plan_usage.rs"]
mod plan_usage;

#[path = "app/steering.rs"]
mod steering;

#[path = "app/sidebar.rs"]
mod sidebar;

#[path = "app/terminal.rs"]
mod terminal;

#[path = "app/plugin_commands.rs"]
mod plugin_commands;

#[path = "app/folds.rs"]
mod folds;

#[path = "app/pages.rs"]
mod pages;

#[path = "app/compact.rs"]
mod compact;

#[path = "app/editing.rs"]
mod editing;

#[path = "app/questions.rs"]
mod questions;

#[path = "app/help.rs"]
mod help;

#[path = "app/suggest.rs"]
mod suggest;

#[path = "app/quit_wait.rs"]
mod quit_wait;

#[path = "app/endpoints.rs"]
mod endpoints;

#[path = "app/subscription_login.rs"]
mod subscription_login;

#[path = "app/retry.rs"]
mod retry;

#[path = "app/live_display.rs"]
mod live_display;

#[path = "app/resumed.rs"]
mod resumed;

#[path = "app/hosted.rs"]
mod hosted;

#[path = "app/settings_screen.rs"]
mod settings_screen;

#[path = "app/agent_keys.rs"]
mod agent_keys;
#[path = "app/agents.rs"]
mod agents;
#[path = "app/chats.rs"]
mod chats;
#[path = "app/processes.rs"]
mod processes;
#[path = "app/rewind.rs"]
mod rewind;
#[path = "app/secrets.rs"]
mod secrets;

fn without_terminal(
    capacity: usize,
) -> (
    mpsc::Sender<wisp_protocol::ClientCommand>,
    mpsc::Receiver<wisp_protocol::ClientCommand>,
) {
    let (commands, mut sent) = mpsc::channel(capacity);
    let (kept, asked) = mpsc::channel(capacity);
    tokio::spawn(async move {
        loop {
            let command = tokio::select! {
                () = kept.closed() => return,
                command = sent.recv() => command,
            };
            match command {
                Some(wisp_protocol::ClientCommand::Terminal { .. }) => {}
                Some(command) => {
                    if kept.send(command).await.is_err() {
                        return;
                    }
                }
                None => return,
            }
        }
    });
    (commands, asked)
}

#[derive(Default)]
struct Spy {
    events: Vec<String>,
}

impl vt100::Callbacks for Spy {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.events.push("bell".to_owned());
    }

    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, name: &[u8]) {
        self.events.push(format!("icon {name:?}"));
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.events.push(format!("title {title:?}"));
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _: &[u8], data: &[u8]) {
        self.events.push(format!("clipboard {data:?}"));
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        self.events.push(format!("osc {params:?}"));
    }
}

const SCRATCH: &str = "wisp-tui-";
const SHELL: &[u8] = b"$ echo before\r\nbefore\r\n$ wisp\r\n";
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1007l";
const LEAVE: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l";

struct State {
    parser: vt100::Parser<Spy>,
    main: Vec<String>,
    frames: Vec<usize>,
    bodies: Vec<Vec<u8>>,
    sized: std::cell::Cell<usize>,
    full: Vec<bool>,
    seen: Vec<String>,
    refuse: Option<Halted>,
    reapplied: usize,
    suspended: usize,
    stopped: bool,
    broken: Option<bool>,
    handed: Vec<Vec<u8>>,
    taken_back: usize,
    clipboard: Vec<Vec<u8>>,
    modes: Modes,
    set_modes: Vec<Modes>,
    mode_bytes: String,
    slow: Duration,
}

#[derive(Clone)]
struct Screen(Rc<RefCell<State>>);

fn reported() -> ProbeReport {
    let plan = ProbePlan {
        deadline: Duration::ZERO,
        sync_query: false,
        scroll_query: false,
        width_test: false,
        pictures: false,
    };
    ProbeReport {
        background: Some(Theme::new(Mode::Dark, Depth::TrueColor).rgb(Token::Bg)),
        ..ProbeReport::new(plan)
    }
}

const IDLE_LEFT: [char; 5] = ['\u{288E}', '\u{2880}', '\u{2884}', '\u{2806}', '\u{280A}'];
const IDLE_RIGHT: [char; 4] = ['\u{2870}', '\u{2810}', '\u{2830}', '\u{2860}'];

fn at_rest(line: &str) -> String {
    let mut chars: Vec<char> = line.chars().collect();
    for i in 1..chars.len().saturating_sub(1) {
        if chars[i] == '\u{2836}'
            && IDLE_LEFT.contains(&chars[i - 1])
            && IDLE_RIGHT.contains(&chars[i + 1])
        {
            chars[i - 1] = '\u{288E}';
            chars[i + 1] = '\u{2870}';
        }
    }
    chars.into_iter().collect()
}

fn is_caret(cell: &vt100::Cell) -> bool {
    let block = |mode| {
        let rgb = Theme::new(mode, Depth::TrueColor).rgb(Token::Text);
        vt100::Color::Rgb(rgb.r, rgb.g, rgb.b)
    };
    cell.inverse()
        || [Mode::Dark, Mode::Light]
            .map(block)
            .contains(&cell.bgcolor())
}

fn is_edge(row: &str, corners: [char; 2]) -> bool {
    let mut chars = row.chars();
    chars.next().is_some_and(|c| corners.contains(&c))
        && row.chars().count() > 2
        && row.chars().skip(1).all(|c| {
            c == '\u{2500}'
                || c == '-'
                || corners.contains(&c)
                || "\u{256E}\u{256F}\u{2524}".contains(c)
        })
}

fn is_any_edge(row: &str) -> bool {
    is_edge(row, ['\u{256D}', '+']) || is_edge(row, ['\u{2570}', '\u{251C}'])
}

fn is_framed(row: &str) -> bool {
    row.starts_with(['\u{2502}', '|']) && row.ends_with(['\u{2502}', '|'])
}

fn framed_status(rows: &[String]) -> bool {
    rows.len() >= 2 && is_edge(&rows[rows.len() - 1], ['\u{2570}', '+'])
}

fn composer_top(rows: &[String]) -> usize {
    let edges: Vec<usize> = (0..rows.len()).filter(|&i| is_any_edge(&rows[i])).collect();
    let skip = 2 + usize::from(framed_status(rows));
    edges
        .len()
        .checked_sub(skip)
        .map(|i| edges[i])
        .unwrap_or_else(|| panic!("no composer box: {rows:#?}"))
}

fn frame_top(rows: &[String]) -> usize {
    let last = rows.len().saturating_sub(1);
    (0..last)
        .rev()
        .take_while(|&i| is_any_edge(&rows[i]) || is_framed(&rows[i]))
        .last()
        .unwrap_or(last)
}

fn chat_row_shown(rows: &[String]) -> bool {
    rows[frame_top(rows)..]
        .windows(2)
        .any(|w| is_any_edge(&w[0]) && w[1].starts_with("\u{2502} \u{203A}"))
}

fn shows(lines: &[String], text: &str) -> bool {
    lines.iter().any(|l| l.contains(text))
}

fn status_line(rows: &[String]) -> String {
    if framed_status(rows) {
        unframed(&rows[rows.len() - 2])
    } else {
        rows.last().cloned().unwrap_or_default()
    }
}

fn unframed(row: &str) -> String {
    let row = row.strip_prefix(['\u{2502}', '|']).unwrap_or(row);
    let row = row.trim_end();
    let row = row.strip_suffix(['\u{2502}', '|']).unwrap_or(row);
    row.trim_end().to_owned()
}

fn is_full(bytes: &[u8], rows: u16) -> bool {
    let text = String::from_utf8_lossy(bytes);
    (1..=rows).all(|row| text.contains(&format!("\x1b[{row};1H")))
}

impl Screen {
    fn new(rows: u16, cols: u16) -> Screen {
        crossterm::style::Colored::set_ansi_color_disabled(false);
        let mut parser = vt100::Parser::new_with_callbacks(nz(rows), nz(cols), 0, Spy::default());
        parser.process(SHELL);
        let main = rows_of(parser.screen());
        parser.process(ENTER);
        Screen(Rc::new(RefCell::new(State {
            parser,
            main,
            frames: vec![],
            bodies: vec![],
            sized: std::cell::Cell::new(0),
            full: vec![],
            seen: vec![],
            refuse: None,
            reapplied: 0,
            suspended: 0,
            stopped: false,
            broken: None,
            handed: vec![],
            taken_back: 0,
            clipboard: vec![],
            modes: Modes {
                mouse: true,
                ..Modes::default()
            },
            set_modes: vec![],
            mode_bytes: String::new(),
            slow: Duration::ZERO,
        })))
    }

    fn slow_frames(&self, by: Duration) {
        self.0.borrow_mut().slow = by;
    }

    fn resize(&self, rows: u16, cols: u16) -> Size {
        self.0
            .borrow_mut()
            .parser
            .screen_mut()
            .set_size(nz(rows), nz(cols));
        Size {
            width: cols,
            height: rows,
        }
    }

    fn frames(&self) -> usize {
        self.0.borrow().frames.len()
    }

    fn bodies_since(&self, frame: usize) -> Vec<Vec<u8>> {
        self.0.borrow().bodies.iter().skip(frame).cloned().collect()
    }

    fn sized(&self) -> usize {
        self.0.borrow().sized.get()
    }

    fn fg_at(&self, row: u16, col: u16) -> String {
        let state = self.0.borrow();
        let cell = state.parser.screen().cell(row, col);
        format!("{:?}", cell.map(vt100::Cell::fgcolor))
    }

    fn inverse_rows(&self) -> Vec<(u16, String)> {
        let state = self.0.borrow();
        let screen = state.parser.screen();
        let (rows, cols) = screen.size();
        (0..rows.get())
            .filter_map(|row| {
                let text: String = (0..cols.get())
                    .filter_map(|col| screen.cell(row, col))
                    .filter(|c| c.inverse() && !c.is_wide_continuation())
                    .map(|c| c.contents().to_owned())
                    .collect();
                (!text.is_empty()).then_some((row, text))
            })
            .collect()
    }

    fn symbol_at(&self, row: u16, col: u16) -> String {
        let state = self.0.borrow();
        let cell = state.parser.screen().cell(row, col);
        cell.map(|c| c.contents().to_owned()).unwrap_or_default()
    }

    fn last_frame(&self) -> usize {
        self.0.borrow().frames.last().copied().unwrap_or(0)
    }

    fn full_frames(&self) -> usize {
        self.0.borrow().full.iter().filter(|f| **f).count()
    }

    fn last_full(&self) -> bool {
        self.0.borrow().full.last().copied().unwrap_or(false)
    }

    fn full_since(&self, frame: usize) -> bool {
        self.0.borrow().full.iter().skip(frame).any(|f| *f)
    }

    fn spied(&self) -> Vec<String> {
        self.0.borrow().parser.callbacks().events.clone()
    }

    fn visible(&self) -> Vec<String> {
        rows_of(self.0.borrow().parser.screen())
    }

    fn steady(&self) -> Vec<String> {
        self.visible().iter().map(|l| at_rest(l)).collect()
    }

    fn seen(&self) -> Vec<String> {
        self.0.borrow().seen.clone()
    }

    fn colours_of(&self, needle: &str) -> BTreeSet<String> {
        let state = self.0.borrow();
        let screen = state.parser.screen();
        let (rows, cols) = screen.size();
        let mut out = BTreeSet::new();
        for row in 0..rows.get() {
            let text: String = screen
                .rows(0, cols.get())
                .nth(usize::from(row))
                .unwrap_or_default();
            if !text.contains(needle) {
                continue;
            }
            for col in 0..cols.get() {
                if let Some(cell) = screen.cell(row, col)
                    && !cell.contents().trim().is_empty()
                {
                    out.insert(format!("{:?}", cell.fgcolor()));
                }
            }
        }
        out
    }

    fn caret_line(&self) -> String {
        let state = self.0.borrow();
        let screen = state.parser.screen();
        let (rows, cols) = screen.size();
        assert!(screen.hide_cursor(), "the real cursor is shown");
        (0..rows.get())
            .rev()
            .find(|&row| (0..cols.get()).any(|col| screen.cell(row, col).is_some_and(is_caret)))
            .and_then(|row| screen.rows(0, cols.get()).nth(usize::from(row)))
            .unwrap_or_default()
    }

    fn on_alternate(&self) -> bool {
        self.0.borrow().parser.screen().alternate_screen()
    }

    fn in_pager(&self) -> bool {
        pager_hint(&self.visible())
    }

    fn fg(&self) {
        let mut state = self.0.borrow_mut();
        state.parser.process(b"fg\r\n");
        state.parser.process(ENTER);
        state.stopped = false;
    }

    fn panic_restore(&self) {
        let mut state = self.0.borrow_mut();
        state.parser.process(LEAVE);
        state.parser.process(b"wisp panicked: boom\r\n");
        state.refuse = Some(Halted::Restored);
    }

    fn leave(&self) -> Vec<String> {
        let mut state = self.0.borrow_mut();
        state.parser.process(LEAVE);
        rows_of(state.parser.screen())
    }

    fn main_before(&self) -> Vec<String> {
        self.0.borrow().main.clone()
    }

    fn suspended(&self) -> usize {
        self.0.borrow().suspended
    }

    fn refuse(&self, halted: Halted) {
        self.0.borrow_mut().refuse = Some(halted);
    }

    fn break_output(&self, hung_up: bool) {
        self.0.borrow_mut().broken = Some(hung_up);
    }

    fn reapplied(&self) -> usize {
        self.0.borrow().reapplied
    }

    fn handed(&self) -> Vec<Vec<u8>> {
        self.0.borrow().handed.clone()
    }

    fn taken_back(&self) -> usize {
        self.0.borrow().taken_back
    }

    fn sent_to_clipboard(&self) -> Vec<Vec<u8>> {
        self.0.borrow().clipboard.clone()
    }
}

impl Console for Screen {
    fn size(&self) -> io::Result<Size> {
        let state = self.0.borrow();
        state.sized.set(state.sized.get() + 1);
        drop(state);
        let (rows, cols) = self.0.borrow().parser.screen().size();
        Ok(Size {
            width: cols.get(),
            height: rows.get(),
        })
    }

    fn suspend(&mut self) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        state.suspended += 1;
        state.parser.process(LEAVE);
        state.parser.process(b"\r\n[1]+ Stopped\r\n$ ");
        state.stopped = true;
        Ok(())
    }

    fn reapply(&mut self) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        state.reapplied += 1;
        state.refuse = None;
        if !state.parser.screen().alternate_screen() {
            state.parser.process(ENTER);
        }
        Ok(())
    }

    fn halted(&self) -> Option<Halted> {
        self.0.borrow().refuse
    }

    fn hung_up(&self) -> bool {
        self.0.borrow().broken == Some(true)
    }

    fn frame(&mut self, body: &[u8], size: Size) -> io::Result<Written> {
        std::thread::sleep(self.0.borrow().slow);
        let mut state = self.0.borrow_mut();
        if state.broken.is_some() {
            return Err(io::Error::from_raw_os_error(5));
        }
        if let Some(Halted::Dying) = state.refuse {
            return Err(io::Error::other(Halted::Dying));
        }
        if let Some(halted) = state.refuse {
            return Ok(Written::Refused(halted));
        }
        if state.stopped {
            return Ok(Written::Suspended);
        }
        let (rows, cols) = state.parser.screen().size();
        let live = Size {
            width: cols.get(),
            height: rows.get(),
        };
        if size != live {
            return Ok(Written::Resized(live));
        }
        assert!(
            state.parser.screen().alternate_screen(),
            "a frame was drawn on the main screen"
        );
        let bytes = frame(body, CursorAfter::Hidden);
        state.frames.push(bytes.len());
        state.bodies.push(body.to_vec());
        state.full.push(is_full(body, rows.get()));
        state.parser.process(&bytes);
        let shown = rows_of(state.parser.screen());
        state.seen.extend(shown);
        Ok(Written::Sent)
    }

    fn clipboard(&mut self, sequence: &Osc52) -> io::Result<Written> {
        let mut state = self.0.borrow_mut();
        if let Some(halted) = state.refuse {
            return Ok(Written::Refused(halted));
        }
        state.clipboard.push(sequence.bytes().to_vec());
        state.parser.process(sequence.bytes());
        Ok(Written::Sent)
    }

    fn hand_off(&mut self, body: &[u8]) -> io::Result<Written> {
        let mut state = self.0.borrow_mut();
        state.handed.push(body.to_vec());
        state.parser.process(LEAVE);
        state.parser.process(body);
        Ok(Written::Sent)
    }

    fn take_back(&mut self) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        state.taken_back += 1;
        state.parser.process(ENTER);
        Ok(())
    }

    fn bell(&mut self) -> io::Result<Written> {
        self.0.borrow_mut().parser.process(b"\x07");
        Ok(Written::Sent)
    }

    fn modes(&self) -> Modes {
        self.0.borrow().modes
    }

    fn set_modes(&mut self, modes: Modes) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        for bytes in wisp_tui::term::modes::transition(&state.modes, &modes)
            .into_iter()
            .filter_map(wisp_tui::term::modes::Change::bytes)
        {
            state.mode_bytes.push_str(&bytes);
        }
        state.modes = modes;
        state.set_modes.push(modes);
        Ok(())
    }
}

fn facts() -> SessionFacts {
    SessionFacts {
        version: "0.0.0".to_owned(),
        provider: "anthropic".to_owned(),
        model: "test-model".to_owned(),
        effort: "low".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        credential: Some(Credential::ApiKey),
        host: None,
        log: None,
        home: None,
        cwd: "~/project".to_owned(),
        endpoint: Endpoint::Official,
        models: models(),
        on_accounts: Vec::new(),
        write_multipliers: multipliers(2.0),
        next: None,
        max_tokens: 64_000,
        endpoint_name: None,
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

fn session_account(reach: Endpoint) -> wisp_render::chrome::EndpointFacts {
    wisp_render::chrome::EndpointFacts {
        models: Vec::new(),
        mark: mockup_mark("claude-api"),
        name: "claude-api".to_owned(),
        provider: "anthropic".to_owned(),
        credential: Some(Credential::ApiKey),
        reach: Some(reach),
        host: None,
        unavailable: None,
        priced: true,
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }
}

fn described(request: u64) -> AgentEvent {
    AgentEvent::SettingsSnapshot(wisp_protocol::settings::SettingsSnapshot {
        often: Vec::new(),
        answers: wisp_protocol::RequestId(request),
        context: wisp_protocol::settings::SnapshotContext {
            endpoint: "claude-api".to_owned(),
            model: "test-model".to_owned(),
            agent: None,
            max_tokens: 0,
            window: None,
            room: None,
        },
        settings: Vec::new(),
        saved: Vec::new(),
        endpoints: Vec::new(),
        models: Vec::new(),
        file: wisp_protocol::settings::FileState::Missing,
        switches: Vec::new(),
        compaction: None,
        plugins: Vec::new(),
        placed: Vec::new(),
    })
}

fn frozen(mut options: Options) -> Options {
    options.code = Start::Stalled;
    options.zone = jiff::tz::TimeZone::UTC;
    options.now = fixed_now;
    options
}

fn options(code: Start) -> Options {
    let mut options = frozen(Options::new(
        Theme::new(Mode::Dark, Depth::TrueColor),
        GlyphSet::Unicode,
    ));
    options.code = code;
    options
}

fn completed() -> AgentEvent {
    turn_ended(End {
        model: "test-model".to_owned(),
        ..End::default()
    })
}

fn answer(chunks: impl IntoIterator<Item = String>) -> Vec<AgentEvent> {
    let mut out = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
    ];
    out.extend(
        chunks
            .into_iter()
            .map(|text| AgentEvent::TextDelta { index: 0, text }),
    );
    out.push(AgentEvent::BlockStopped { index: 0 });
    out.push(completed());
    out
}

fn chunked(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(size).map(|c| c.iter().collect()).collect()
}

fn key(code: KeyCode) -> Input {
    Input::Key(Key::plain(code))
}

fn ctrl_key(code: KeyCode) -> Input {
    Input::Key(Key {
        ctrl: true,
        ..Key::plain(code)
    })
}

fn ctrl(c: char) -> Input {
    Input::Key(Key::ctrl(c))
}

trait Drive {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input>;

    fn input(&self, input: Input) -> &Self {
        self.inputs().send(input).unwrap();
        self
    }

    fn key(&self, code: KeyCode) -> &Self {
        self.input(key(code))
    }

    fn ctrl(&self, c: char) -> &Self {
        self.input(ctrl(c))
    }

    fn paste(&self, text: &str) -> &Self {
        self.input(Input::Paste(text.to_owned()))
    }

    fn say(&self, text: &str) -> &Self {
        self.paste(text).key(KeyCode::Enter)
    }

    fn typed(&self, text: &str) -> &Self {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
        self
    }
}

fn wheel(kind: MouseKind) -> Input {
    Input::Mouse(Mouse {
        kind,
        column: 10,
        row: 3,
        modified: false,
    })
}

struct Run {
    screen: Screen,
    inputs: mpsc::UnboundedSender<Input>,
}

impl Run {
    fn send(&self, input: Input) {
        self.input(input);
    }

    fn quit(&self) {
        self.ctrl('c').ctrl('c').ctrl('c');
    }

    async fn open_transcript(&self) {
        self.send(ctrl('o'));
        sleep(Duration::from_millis(100)).await;
        if picker_shown(&self.screen.visible()) {
            self.send(ctrl('o'));
            sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drive for Run {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input> {
        &self.inputs
    }
}

fn picker_shown(lines: &[String]) -> bool {
    lines.iter().any(|l| l.contains(copy::EVERY_CELL))
}

async fn session<F, D>(
    screen: Screen,
    turns: Vec<Vec<AgentEvent>>,
    code: Start,
    drive: D,
) -> Result<Exit, AppError>
where
    D: FnOnce(Run) -> F,
    F: Future<Output = ()>,
{
    session_with(screen, turns, options(code), drive).await
}

async fn session_with<F, D>(
    screen: Screen,
    turns: Vec<Vec<AgentEvent>>,
    options: Options,
    drive: D,
) -> Result<Exit, AppError>
where
    D: FnOnce(Run) -> F,
    F: Future<Output = ()>,
{
    finished_with(screen, turns, options, facts(), drive)
        .await
        .exit
}

async fn finished_with<F, D>(
    screen: Screen,
    turns: Vec<Vec<AgentEvent>>,
    options: Options,
    facts: SessionFacts,
    drive: D,
) -> Finished
where
    D: FnOnce(Run) -> F,
    F: Future<Output = ()>,
{
    let events: Vec<AgentEvent> = turns.into_iter().flatten().collect();
    let (commands, events) = replay::spawn(Script::new(events), replay::PACE);
    let (inputs, received) = mpsc::unbounded_channel();
    let mut console = screen.clone();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts,
        events,
        commands,
        resumed: None,
    };
    let app = run_to_end(&mut console, received, session, options);
    let driver = drive(Run {
        screen: screen.clone(),
        inputs,
    });
    let (finished, ()) = tokio::join!(app, driver);
    finished
}

const LONG_PROSE: &str = "The quick brown fox jumps over the lazy dog while the band plays on and \
the river keeps running past the old mill, where nobody has worked for a very long time now.";

fn rich_answer() -> String {
    let mut text = String::new();
    text.push_str(LONG_PROSE);
    text.push_str("\n\n- first item\n- second item\n  - nested item\n\n");
    text.push_str("| name | value |\n|------|-------|\n| alpha | 1 |\n| beta | 2 |\n\n");
    text.push_str("```rust\n");
    for i in 0..60 {
        text.push_str(&format!("let v{i} = {i};\n"));
    }
    text.push_str("```\n\n");
    text.push_str(LONG_PROSE);
    text.push_str(" Closing words follow here.\n");
    text
}

fn words_in_order(lines: &[String], text: &str) -> bool {
    let all = lines.join(" ");
    let mut rest = all.as_str();
    for word in text.split_whitespace() {
        match rest.find(word) {
            Some(at) => rest = &rest[at + word.len()..],
            None => return false,
        }
    }
    true
}

fn pager_hint(lines: &[String]) -> bool {
    lines.iter().any(|l| l.contains(&return_keys().text()))
}

fn prompt() -> &'static str {
    Glyphs::of(GlyphSet::Unicode).prompt
}

fn numbered(turns: usize) -> Vec<Vec<AgentEvent>> {
    (0..turns)
        .map(|i| {
            answer([format!(
                "answer {i} begins\n\nmiddle of answer {i}\n\nanswer {i} ends\n"
            )])
        })
        .collect()
}

async fn tick() {
    std::thread::sleep(Duration::from_millis(20));
    sleep(Duration::from_millis(20)).await;
}

async fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !done() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tick().await;
    }
    true
}

async fn real_wait(ms: u64) {
    for _ in 0..ms / 20 {
        tick().await;
    }
}

async fn ask_all(run: &Run, turns: usize) {
    for i in 0..turns {
        run.say(&format!("question {i}"));
        sleep(Duration::from_secs(3)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn esc_stops_a_streaming_reply_on_the_next_tick() {
    let screen = Screen::new(24, 80);
    let words = (0..2000).map(|i| format!("w{i} "));
    let exit = session(
        screen.clone(),
        vec![answer(words)],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(300)).await;
            run.say("hello");
            sleep(Duration::from_millis(500)).await;
            run.send(key(KeyCode::Enter));
            run.say("again");
            sleep(Duration::from_millis(50)).await;
            let lines = run.screen.visible();
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(copy::NEXT_STEP) && l.contains("again")),
                "{lines:#?}"
            );
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(60)).await;
            let lines = run.screen.visible();
            assert!(
                lines.iter().any(|l| l.contains(&copy::stopped(0))),
                "{lines:#?}"
            );
            assert!(!lines.iter().any(|l| l.contains("w1999")));
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("Your unsent steer is back in the composer.")),
                "{lines:#?}"
            );
            assert!(run.screen.caret_line().contains("again"), "{lines:#?}");
            run.send(ctrl('c'));
            sleep(Duration::from_secs(1)).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert_eq!(exit, Exit::Quit);
}

#[tokio::test(start_paused = true)]
async fn an_idle_screen_writes_nothing_and_streaming_stays_under_thirty() {
    let screen = Screen::new(24, 80);
    let words = (0..3000).map(|i| format!("w{i} "));
    session(
        screen.clone(),
        vec![answer(words)],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_secs(5)).await;
            let idle = run.screen.frames();
            sleep(Duration::from_secs(10)).await;
            let moved = run.screen.frames() - idle;
            assert_eq!(moved, 0, "an idle screen wrote {moved} frames");
            run.say("go");
            sleep(Duration::from_secs(2)).await;
            let before = run.screen.frames();
            sleep(Duration::from_secs(10)).await;
            let drawn = run.screen.frames() - before;
            assert!(drawn > 100, "{drawn} frames while streaming");
            assert!(drawn <= 10_000 / 33 + 1, "{drawn} frames in 10 s");
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_secs(8)).await;
            let idle = run.screen.frames();
            sleep(Duration::from_secs(10)).await;
            let moved = run.screen.frames() - idle;
            assert_eq!(moved, 0, "an idle screen wrote {moved} frames");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn only_changed_cells_are_written_except_after_a_repaint() {
    let screen = Screen::new(24, 80);
    session(screen.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(1000)).await;
        assert!(run.screen.full_frames() >= 1);
        let full = run.screen.full_frames();
        run.send(Input::Paste("a".to_owned()));
        sleep(Duration::from_millis(100)).await;
        assert!(!run.screen.last_full());
        assert!(run.screen.last_frame() < 600, "{}", run.screen.last_frame());
        run.send(ctrl('l'));
        sleep(Duration::from_millis(100)).await;
        assert_eq!(run.screen.full_frames(), full + 1);
        run.quit();
    })
    .await
    .unwrap();
    let screen = Screen::new(24, 80);
    let mut always = options(Start::Stalled);
    always.full_repaint = true;
    session_with(screen.clone(), vec![], always, |run| async move {
        sleep(Duration::from_millis(500)).await;
        for c in ["a", "b", "c"] {
            run.send(Input::Paste(c.to_owned()));
            sleep(Duration::from_millis(100)).await;
            assert!(run.screen.last_full());
        }
        run.quit();
    })
    .await
    .unwrap();
    assert_eq!(screen.full_frames(), screen.frames());
}

#[tokio::test(start_paused = true)]
async fn a_short_transcript_sits_at_the_bottom_under_the_banner() {
    let screen = Screen::new(30, 80);
    session(
        screen.clone(),
        vec![answer(["Hi there.\n".to_owned()])],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hello");
            sleep(Duration::from_secs(3)).await;
            let rows = run.screen.visible();
            assert!(rows[0].starts_with('\u{256D}'), "{rows:#?}");
            assert!(rows[2].contains("wisp"), "{rows:#?}");
            assert!(rows[3].contains(copy::TAGLINE_OWN), "{rows:#?}");
            assert!(rows[7].starts_with('\u{2570}'), "{rows:#?}");
            assert!(rows[8].starts_with("  enter send"), "{rows:#?}");
            let reply = rows.iter().position(|l| l.contains("Hi there.")).unwrap();
            let rule = frame_top(&rows);
            assert!(reply + 5 >= rule && reply < rule, "{rows:#?}");
            assert!(rows[9..reply - 3].iter().all(String::is_empty), "{rows:#?}");
            assert!(rows[rule].starts_with('\u{256D}'), "{rows:#?}");
            assert!(is_framed(&rows[rule + 1]), "{rows:#?}");
            assert!(rows[rule + 2].starts_with('\u{251C}'), "{rows:#?}");
            assert!(rows[rule + 3].contains(prompt()), "{rows:#?}");
            assert!(rows[rule + 4].starts_with('\u{251C}'), "{rows:#?}");
            assert!(rows[rule + 5].contains("Ready") || rows[rule + 5].contains("Done"));
            assert!(rows[rule + 6].starts_with('\u{2570}'), "{rows:#?}");
            assert_eq!(rule + 7, rows.len());
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_long_transcript_scrolls_as_one_and_follows_new_output() {
    let screen = Screen::new(20, 80);
    session(
        screen.clone(),
        numbered(6),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 6).await;
            let rows = run.screen.visible();
            assert!(
                !rows.iter().any(|l| l.contains(copy::TAGLINE_OWN)),
                "{rows:#?}"
            );
            assert!(
                rows.iter().any(|l| l.contains("answer 5 ends")),
                "{rows:#?}"
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

fn top_row(screen: &Screen) -> String {
    screen
        .visible()
        .into_iter()
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

#[tokio::test(start_paused = true)]
async fn every_scroll_key_and_the_wheel_move_the_view() {
    let screen = Screen::new(20, 80);
    session(
        screen.clone(),
        numbered(8),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 8).await;
            let bottom = run.screen.steady();
            let view = 20 - 7;
            run.send(key(KeyCode::PageUp));
            sleep(Duration::from_millis(50)).await;
            let up = run.screen.steady();
            assert_eq!(up[view / 2..view], bottom[..view - view / 2], "{up:#?}");
            run.send(key(KeyCode::PageDown));
            sleep(Duration::from_millis(50)).await;
            assert_eq!(run.screen.steady(), bottom);
            run.send(ctrl_key(KeyCode::Home));
            sleep(Duration::from_millis(50)).await;
            assert!(top_row(&run.screen).starts_with('\u{256D}'));
            run.send(ctrl_key(KeyCode::End));
            sleep(Duration::from_millis(50)).await;
            assert_eq!(run.screen.steady(), bottom);
            run.send(key(KeyCode::Home));
            sleep(Duration::from_millis(50)).await;
            assert!(top_row(&run.screen).starts_with('\u{256D}'));
            run.send(key(KeyCode::End));
            sleep(Duration::from_millis(50)).await;
            assert_eq!(run.screen.steady(), bottom);
            run.send(wheel(MouseKind::WheelUp));
            sleep(Duration::from_millis(50)).await;
            let wheeled = run.screen.steady();
            assert_eq!(wheeled[3..view], bottom[..view - 3], "{wheeled:#?}");
            run.send(wheel(MouseKind::WheelDown));
            sleep(Duration::from_millis(50)).await;
            assert_eq!(run.screen.steady(), bottom);
            run.send(key(KeyCode::PageUp));
            run.send(key(KeyCode::PageUp));
            sleep(Duration::from_millis(50)).await;
            assert_ne!(run.screen.steady(), bottom);
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(50)).await;
            assert_eq!(run.screen.steady(), bottom);
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_scroll_speed_multiplies_the_wheel_step() {
    let screen = Screen::new(30, 80);
    let mut opts = options(Start::Stalled);
    opts.scroll_speed = 2;
    session_with(screen.clone(), numbered(8), opts, |run| async move {
        sleep(Duration::from_millis(100)).await;
        ask_all(&run, 8).await;
        let bottom = run.screen.visible();
        run.send(wheel(MouseKind::WheelUp));
        sleep(Duration::from_millis(50)).await;
        let wheeled = run.screen.visible();
        assert_eq!(wheeled[6..23], bottom[..17], "{wheeled:#?}");
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn home_and_end_edit_a_typed_message_and_up_and_down_recall_history() {
    let screen = Screen::new(20, 80);
    session(
        screen.clone(),
        numbered(8),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 8).await;
            let bottom = run.screen.visible();
            run.send(Input::Paste("draft".to_owned()));
            run.send(key(KeyCode::Home));
            run.send(Input::Paste(">".to_owned()));
            run.send(key(KeyCode::End));
            run.send(Input::Paste("<".to_owned()));
            sleep(Duration::from_millis(50)).await;
            assert!(run.screen.caret_line().contains(">draft<"));
            assert_eq!(run.screen.visible()[..16], bottom[..16]);
            run.send(ctrl('c'));
            run.send(key(KeyCode::Up));
            sleep(Duration::from_millis(50)).await;
            assert!(run.screen.caret_line().contains("question 7"));
            run.send(key(KeyCode::Up));
            sleep(Duration::from_millis(50)).await;
            assert!(run.screen.caret_line().contains("question 6"));
            run.send(key(KeyCode::Down));
            sleep(Duration::from_millis(50)).await;
            assert!(run.screen.caret_line().contains("question 7"));
            assert_eq!(run.screen.visible()[..16], bottom[..16]);
            run.quit();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn scrolled_up_output_waits_below_a_hint_that_counts_and_jumps_back() {
    let screen = Screen::new(20, 80);
    session(
        screen.clone(),
        numbered(8),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 6).await;
            run.say("question 6");
            run.send(key(KeyCode::PageUp));
            sleep(Duration::from_millis(50)).await;
            let held = run.screen.visible();
            assert!(!held.iter().any(|l| l.contains("answer 6")), "{held:#?}");
            sleep(Duration::from_secs(3)).await;
            let rows = run.screen.visible();
            assert_eq!(rows[..12], held[..12], "new output moved the view");
            let hint = rows[12].clone();
            assert!(
                hint.ends_with(&format!("new \u{b7} {NEW_KEY}")),
                "{rows:#?}"
            );
            assert!(hint.contains("\u{2193} "), "{hint:?}");
            let count: usize = hint
                .split('\u{2193}')
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse().ok())
                .unwrap();
            assert!(count >= 2, "{hint:?}");
            let col = u16::try_from(hint.chars().count() - 3).unwrap();
            run.send(Input::Mouse(Mouse {
                kind: MouseKind::Press,
                column: col,
                row: 12,
                modified: false,
            }));
            sleep(Duration::from_millis(50)).await;
            let rows = run.screen.visible();
            assert!(
                rows.iter().any(|l| l.contains("answer 6 ends")),
                "{rows:#?}"
            );
            assert!(!rows.iter().any(|l| l.contains(NEW_KEY)), "{rows:#?}");
            run.send(key(KeyCode::PageUp));
            sleep(Duration::from_millis(50)).await;
            run.say("question 7");
            sleep(Duration::from_secs(3)).await;
            let rows = run.screen.visible();
            assert!(
                rows.iter().any(|l| l.contains("answer 7 ends")),
                "{rows:#?}"
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_long_reply_reads_whole_at_the_new_width_after_a_resize() {
    let screen = Screen::new(150, 70);
    let text = rich_answer();
    let turn = answer(chunked(&text, 7));
    session(
        screen.clone(),
        vec![turn],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(200)).await;
            run.say("show me everything");
            sleep(Duration::from_millis(900)).await;
            let size = run.screen.resize(150, 100);
            run.send(Input::Resize(size));
            sleep(Duration::from_secs(20)).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    let lines = screen.visible();
    for i in 0..60 {
        let row = format!("let v{i} = {i};");
        let seen = lines.iter().filter(|l| l.contains(&row)).count();
        assert_eq!(seen, 1, "{row} seen {seen} times: {lines:#?}");
    }
    for item in ["first item", "second item", "nested item", "alpha", "beta"] {
        assert!(lines.iter().any(|l| l.contains(item)), "{item}: {lines:#?}");
    }
    assert!(words_in_order(&lines, LONG_PROSE), "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Closing words follow here."))
    );
    assert!(
        lines.iter().any(|l| l.chars().count() > 70),
        "rows after the resize still use the old width: {lines:#?}"
    );
}

const WIDE_TABLE: &str = "| Approach | Unit reversed | Example |\n|---|---|---|\n| Reverse the characters | Each grapheme, so combined marks and emoji stay whole | a long example that says what happens to the text |\n| Reverse the words | Each whitespace-separated word, keeping the letters of each word | another example that needs a sentence of its own |\n";

fn wide_layout(lines: &[String], width: usize) {
    let prose = lines
        .iter()
        .filter(|l| l.contains("quick brown"))
        .map(|l| l.trim_end().chars().count())
        .max()
        .unwrap_or(0);
    assert!(prose > width - 16 && prose < width, "{prose}: {lines:#?}");
    let label = lines
        .iter()
        .find(|l| l.trim_end().ends_with("rust"))
        .map(|l| l.trim_end().chars().count());
    assert!(label == Some(width - 1), "{label:?}: {lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains("\u{2500}\u{2500}\u{2500}")),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.contains("Approach:")), "{lines:#?}");
    assert!(
        lines.iter().all(|l| l.chars().count() <= width),
        "{lines:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_wide_screen_uses_its_width_and_a_resize_to_narrow_and_back_rewraps() {
    let screen = Screen::new(40, 160);
    let text = format!("{LONG_PROSE}\n\n{WIDE_TABLE}\n```rust\nfn main() {{}}\n```\n");
    let seen = screen.clone();
    session(
        screen.clone(),
        vec![answer([text])],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(200)).await;
            run.say("show me");
            sleep(Duration::from_secs(3)).await;
            wide_layout(&seen.visible(), 160);
            let size = run.screen.resize(40, 80);
            run.send(Input::Resize(size));
            sleep(Duration::from_secs(1)).await;
            let narrow = seen.visible();
            assert!(
                narrow.iter().all(|l| l.chars().count() <= 80),
                "{narrow:#?}"
            );
            assert!(
                narrow
                    .iter()
                    .any(|l| l.contains("Approach: Reverse the characters")),
                "{narrow:#?}"
            );
            assert!(words_in_order(&narrow, LONG_PROSE), "{narrow:#?}");
            let size = run.screen.resize(40, 160);
            run.send(Input::Resize(size));
            sleep(Duration::from_secs(1)).await;
            wide_layout(&seen.visible(), 160);
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_resize_is_debounced_and_keeps_an_anchored_view_on_its_item() {
    let screen = Screen::new(20, 80);
    session(
        screen.clone(),
        numbered(8),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 8).await;
            for _ in 0..4 {
                run.send(key(KeyCode::PageUp));
            }
            sleep(Duration::from_millis(50)).await;
            let top = top_row(&run.screen);
            assert!(!top.is_empty());
            let mark = run.screen.frames();
            let size = run.screen.resize(20, 60);
            run.send(Input::Resize(size));
            sleep(Duration::from_millis(20)).await;
            assert!(run.screen.full_since(mark));
            assert_eq!(top_row(&run.screen), top, "clipped while settling");
            sleep(Duration::from_millis(200)).await;
            assert_eq!(top_row(&run.screen), top);
            let rows = run.screen.visible();
            assert!(rows.iter().all(|l| l.chars().count() <= 60), "{rows:#?}");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn small_screens_keep_the_composer_and_status() {
    for (rows, cols, want) in [(5u16, 80u16, 1usize), (2, 80, 0), (3, 80, 1), (10, 20, 8)] {
        let screen = Screen::new(rows, cols);
        session(screen.clone(), vec![], Start::Stalled, |run| async move {
            sleep(Duration::from_millis(1000)).await;
            run.send(Input::Paste("one\ntwo\nthree".to_owned()));
            sleep(Duration::from_millis(100)).await;
            let shown = run.screen.visible();
            assert!(run.screen.caret_line().contains("three"), "{shown:#?}");
            let boxed = shown.len() >= 2 && shown[shown.len() - 2].starts_with('\u{2570}');
            assert_eq!(boxed, cols > 20 && rows >= 5, "{shown:#?}");
            if rows >= 2 {
                assert!(
                    shown.last().is_some_and(|l| {
                        l.contains("Listening")
                            || l.contains("test-model")
                            || l.contains(AccountKind::ClaudeApi.label())
                    }),
                    "{shown:#?}"
                );
            }
            let blank_or_banner = shown.iter().take(want).count();
            assert_eq!(blank_or_banner, want.min(shown.len()));
            run.quit();
        })
        .await
        .unwrap();
    }
}

const CODE_ANSWER: &str = "Here it is:\n\n```rust\nfn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}\n```\n\nDone.\n";

fn one_chunk() -> Vec<AgentEvent> {
    answer([CODE_ANSWER.to_owned()])
}

#[tokio::test]
async fn code_sent_in_one_chunk_is_highlighted_once_the_worker_replies() {
    let screen = Screen::new(24, 80);
    let code = Start::Loaded(grammars(), default_feed());
    session(screen.clone(), vec![one_chunk()], code, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.say("code please");
        eventually(|| run.screen.colours_of("let answer = 42;").len() > 1).await;
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
    let colours = screen.colours_of("let answer = 42;");
    assert!(colours.len() > 1, "{colours:?}");
}

fn stalling() -> Feed {
    Box::new(|_, _, cancel| {
        while !cancel.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        None
    })
}

#[tokio::test(start_paused = true)]
async fn a_stalled_worker_never_blocks_the_answer_and_its_code_stays_plain() {
    let screen = Screen::new(24, 80);
    let code = Start::Loaded(grammars(), stalling());
    session(screen.clone(), vec![one_chunk()], code, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.say("code please");
        sleep(Duration::from_millis(1500)).await;
        let held = run.screen.colours_of("let answer = 42;");
        assert!(
            run.screen
                .visible()
                .iter()
                .any(|l| l.contains("let answer = 42;"))
        );
        assert_eq!(held.len(), 1, "{held:?}");
        sleep(Duration::from_secs(2)).await;
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
    assert_eq!(screen.colours_of("let answer = 42;").len(), 1);
}

#[tokio::test]
async fn a_panicking_highlighter_leaves_code_plain_and_the_app_running() {
    let screen = Screen::new(24, 80);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let tried = Arc::clone(&calls);
    let boom: Feed = Box::new(move |_, _, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        panic!("a grammar blew up");
    });
    let code = Start::Loaded(grammars(), boom);
    let exit = session(screen.clone(), vec![one_chunk()], code, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.say("code please");
        eventually(|| shows(&run.screen.visible(), "Done.") && tried.load(Ordering::SeqCst) >= 1)
            .await;
        assert!(run.screen.visible().iter().any(|l| l.contains("Done.")));
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
    assert_eq!(exit, Exit::Quit);
    assert!(calls.load(Ordering::SeqCst) >= 1);
    assert!(
        screen
            .visible()
            .iter()
            .any(|l| l.contains("let answer = 42;"))
    );
}

#[tokio::test(start_paused = true)]
async fn after_a_panic_restore_the_app_reenters_repaints_in_full_and_says_so_until_the_limit() {
    let screen = Screen::new(24, 80);
    let result = session(screen.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.screen.panic_restore();
        assert!(!run.screen.on_alternate());
        let mark = run.screen.frames();
        run.send(Input::Restored);
        sleep(Duration::from_millis(100)).await;
        assert_eq!(run.screen.reapplied(), 1);
        assert!(run.screen.on_alternate());
        assert!(run.screen.full_since(mark));
        let rows = run.screen.visible();
        assert!(
            status_line(&rows).contains("Recovered from an internal error (1 of 3)"),
            "{rows:#?}"
        );
        assert!(recovered(1).contains("printed above your shell prompt after you quit"));
        let before = run.screen.frames();
        run.send(Input::Paste("still here".to_owned()));
        sleep(Duration::from_millis(100)).await;
        assert!(run.screen.frames() > before);
        assert!(run.screen.caret_line().contains("still here"));
        for _ in 0..MAX_RESTORES {
            run.screen.panic_restore();
            run.send(Input::Restored);
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        matches!(result, Err(AppError::Panics(n)) if n > MAX_RESTORES),
        "{result:?}"
    );
    let main = screen.leave();
    assert!(
        main.iter().any(|l| l.contains("wisp panicked")),
        "{main:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_refused_frame_reapplies_the_modes_even_when_the_terminal_looks_fine() {
    let screen = Screen::new(24, 80);
    session(screen.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.screen.refuse(Halted::Restored);
        run.send(Input::Paste("x".to_owned()));
        sleep(Duration::from_millis(100)).await;
        assert_eq!(run.screen.reapplied(), 1);
        run.send(Input::Restored);
        sleep(Duration::from_millis(100)).await;
        assert_eq!(run.screen.reapplied(), 2);
        run.quit();
    })
    .await
    .unwrap();
}

const CORPUS: [&str; 10] = [
    "\x1b]0;pwned\x07",
    "\x1b]2;pwned\x1b\\",
    "\x1b]52;c;cHduZWQ=\x07",
    "\x1b[2J\x1b[H",
    "\u{9b}31m",
    "\x1bP+q\x1b\\",
    "\x07",
    "\r\x1b[A\x1b[K",
    "\x1b]8;;https://evil.example\x07link\x1b]8;;\x07",
    "\u{202e}reversed",
];

fn hostile_turns(hostile: &str) -> Vec<Vec<AgentEvent>> {
    let mut thinking = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: hostile.to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 10,
            reason: hostile.to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
    ];
    thinking.extend(
        answer([hostile.to_owned(), format!("\n\n```\n{hostile}\n```\n")])
            .into_iter()
            .skip(1),
    );
    let error = vec![
        AgentEvent::turn_started(1),
        turn_ended(End {
            ending: Ending::Error(ErrorKind::InvalidResponse),
            message: hostile.to_owned(),
            ..End::default()
        }),
    ];
    vec![thinking, error]
}

#[tokio::test(start_paused = true)]
async fn escape_sequences_in_answers_thinking_errors_and_pastes_stay_inert() {
    let screen = Screen::new(60, 100);
    let hostile: String = CORPUS.iter().map(|s| format!("A{s}Z ")).collect();
    let pasted = hostile.clone();
    session(
        screen.clone(),
        hostile_turns(&hostile),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say(&pasted);
            sleep(Duration::from_secs(5)).await;
            run.say("next");
            sleep(Duration::from_secs(5)).await;
            let line = unframed(&run.screen.caret_line());
            assert!(line.trim_start().starts_with(prompt()), "{line:?}");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert_eq!(screen.spied(), Vec::<String>::new());
    let lines = screen.seen();
    assert!(
        lines.iter().any(|l| l.contains("A^[]0;pwned^GZ")),
        "{:#?}",
        screen.visible()
    );
    let shown = screen.visible();
    assert!(
        shown
            .iter()
            .filter(|l| l.contains("A^[]0;pwned^GZ"))
            .count()
            >= 3,
        "{shown:#?}"
    );
    assert_eq!(screen.leave(), screen.main_before());
}

#[tokio::test(start_paused = true)]
async fn a_frame_costs_the_same_however_long_the_transcript() {
    async fn last_hint_frame(turns: usize) -> usize {
        let screen = Screen::new(24, 80);
        let script = (0..turns)
            .map(|i| answer([format!("answer {i}")]))
            .collect();
        let watched = screen.clone();
        session(screen, script, Start::Stalled, |run| async move {
            sleep(Duration::from_millis(100)).await;
            for i in 0..turns {
                run.say(&format!("question {i}"));
                sleep(Duration::from_secs(3)).await;
            }
            run.send(ctrl('c'));
            sleep(Duration::from_millis(100)).await;
            assert!(
                run.screen
                    .visible()
                    .iter()
                    .any(|l| l.contains(AGAIN_TO_QUIT))
            );
            run.send(ctrl('c'));
        })
        .await
        .unwrap();
        watched.last_frame()
    }
    let short = last_hint_frame(10).await;
    let long = last_hint_frame(40).await;
    assert_eq!(short, long);
}

#[tokio::test(start_paused = true)]
async fn quit_and_exit_in_any_case_quit() {
    for typed in ["/quit", "/EXIT", "/exit now"] {
        let screen = Screen::new(24, 80);
        let exit = session(screen.clone(), vec![], Start::Stalled, |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say(typed);
            sleep(Duration::from_millis(100)).await;
        })
        .await
        .unwrap();
        assert_eq!(exit, Exit::Quit, "{typed}");
    }
}

#[tokio::test(start_paused = true)]
async fn quitting_while_answering_stops_the_turn_and_waits_for_the_agent() {
    let screen = Screen::new(24, 80);
    let words = (0..2000).map(|i| format!("w{i} "));
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let exit = session(
        screen.clone(),
        vec![answer(words)],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hello");
            sleep(Duration::from_millis(300)).await;
            run.say("/exit");
            flag.store(true, Ordering::SeqCst);
        },
    )
    .await
    .unwrap();
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(exit, Exit::Quit);
    let seen = screen.seen();
    assert!(seen.iter().any(|l| l.contains("w10 ")), "{seen:#?}");
    assert!(!seen.iter().any(|l| l.contains("w1999")), "{seen:#?}");
}

#[tokio::test(start_paused = true)]
async fn ctrl_z_leaves_the_screen_and_fg_repaints_it_in_full() {
    let screen = Screen::new(24, 80);
    let words = (0..400).map(|i| format!("w{i} "));
    session(
        screen.clone(),
        vec![answer(words)],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.send(Input::Paste("draft".to_owned()));
            run.say("hello");
            sleep(Duration::from_millis(1500)).await;
            run.send(Input::Paste("draft".to_owned()));
            run.send(ctrl('z'));
            sleep(Duration::from_millis(300)).await;
            assert_eq!(run.screen.suspended(), 1);
            assert!(!run.screen.on_alternate());
            let shell = run.screen.visible();
            assert!(
                shell.iter().any(|l| l.contains("[1]+ Stopped")),
                "{shell:#?}"
            );
            assert!(shell.iter().any(|l| l == "before"), "{shell:#?}");
            let frames = run.screen.frames();
            sleep(Duration::from_secs(2)).await;
            assert_eq!(run.screen.frames(), frames, "drawn while stopped");
            run.screen.fg();
            run.send(Input::Resumed);
            sleep(Duration::from_millis(100)).await;
            assert!(run.screen.full_since(frames));
            assert!(run.screen.caret_line().contains("draft"));
            sleep(Duration::from_secs(10)).await;
            let rows = run.screen.visible();
            assert!(rows.iter().any(|l| l.contains("w399")), "{rows:#?}");
            run.quit();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_closed_input_reader_ends_the_app_with_its_reason() {
    let screen = Screen::new(24, 80);
    let exit = session(screen, vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.send(Input::Closed("tty gone".to_owned()));
        sleep(Duration::from_secs(1)).await;
    })
    .await
    .unwrap();
    assert_eq!(exit, Exit::InputStopped("tty gone".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn an_output_failure_after_the_input_closed_keeps_the_input_stopped_exit() {
    let screen = Screen::new(24, 80);
    let (commands, mut asked) = mpsc::channel(16);
    let (agent, events) = mpsc::channel(256);
    let (inputs, received) = mpsc::unbounded_channel();
    let mut console = screen.clone();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts: facts(),
        events,
        commands,
        resumed: None,
    };
    let app = run(&mut console, received, session, options(Start::Stalled));
    let drive = async move {
        for event in answer((0..50).map(|i| format!("w{i} "))) {
            agent.send(event).await.unwrap();
        }
        sleep(Duration::from_secs(1)).await;
        assert!(screen.visible().iter().any(|l| l.contains("w49")));
        screen.break_output(false);
        inputs.send(Input::Closed("tty gone".to_owned())).unwrap();
        while asked.recv().await.is_some() {}
        drop(agent);
    };
    let (exit, ()) = tokio::join!(app, drive);
    assert_eq!(exit.unwrap(), Exit::InputStopped("tty gone".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn an_output_failure_on_a_hung_up_terminal_ends_as_input_stopped() {
    for hung_up in [true, false] {
        let screen = Screen::new(24, 80);
        let exit = session(screen, vec![], Start::Stalled, |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.screen.break_output(hung_up);
            run.send(key(KeyCode::Char('x')));
            sleep(Duration::from_secs(1)).await;
        })
        .await;
        if hung_up {
            assert_eq!(exit.unwrap(), Exit::InputStopped(HUNG_UP.to_owned()));
        } else {
            assert!(matches!(exit, Err(AppError::Io(_))), "{exit:?}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_goodbye_shows_the_cost_and_the_resume_command_after_a_turn() {
    let screen = Screen::new(24, 80);
    let turn = |out: u64, session_usd: f64| {
        let mut events = answer(["Hi.\n".to_owned()]);
        events.pop();
        events.push(agents::main_spent((session_usd, Some(0.0), false), &[]));
        events.push(turn_ended(End {
            model: "test-model".to_owned(),
            usage: Some(Usage {
                output_tokens: out,
                ..Usage::default()
            }),
            cost: Some(cost(0.04, session_usd, false)),
            ..End::default()
        }));
        events
    };
    let mut logged = facts();
    logged.log = Some("/home/u/.local/share/wisp/sessions/019a.jsonl".to_owned());
    let finished = finished_with(
        screen,
        vec![turn(2_100, 0.04), turn(2_000, 0.083)],
        options(Start::Stalled),
        logged,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 2).await;
            run.send(ctrl('d'));
        },
    )
    .await;
    let rows = testing::texts(&finished.goodbye);
    assert_eq!(finished.exit.unwrap(), Exit::Quit);
    assert_eq!(rows.len(), 4, "{rows:#?}");
    assert!(
        rows[0].ends_with("   wisp \u{b7} Bye for now."),
        "{rows:#?}"
    );
    assert!(rows[1].ends_with(", total $0.083."), "{rows:#?}");
    assert!(
        rows[2].ends_with("   Come back any time with:"),
        "{rows:#?}"
    );
    assert_eq!(rows[3], "wisp --resume 019a");
    let screen = Screen::new(24, 80);
    let finished = finished_with(
        screen,
        vec![],
        options(Start::Stalled),
        facts(),
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.send(ctrl('d'));
        },
    )
    .await;
    assert!(finished.goodbye.is_empty());
}

#[tokio::test(start_paused = true)]
async fn startup_notes_show_once_in_the_view() {
    let screen = Screen::new(30, 160);
    let mut opts = options(Start::Stalled);
    opts.notes = vec![TMUX_MOUSE_OFF.to_owned()];
    session_with(screen.clone(), vec![], opts, |run| async move {
        sleep(Duration::from_millis(1000)).await;
        let rows = run.screen.visible();
        assert_eq!(
            rows.iter()
                .filter(|l| l.contains("tmux has the mouse off"))
                .count(),
            1,
            "{rows:#?}"
        );
        run.quit();
    })
    .await
    .unwrap();
}

fn words_whole(text: &str, lines: &[String]) {
    let words: Vec<&str> = lines.iter().flat_map(|l| l.split_whitespace()).collect();
    for word in text.split_whitespace() {
        assert!(words.contains(&word), "{word:?} was split: {lines:#?}");
    }
}

#[test]
fn note_rows_hang_under_the_first_word() {
    use wisp_render::markdown::Layout;
    let layout = Layout::new(40, GlyphSet::Unicode);
    let text = "esc or ctrl-c stops a reply \u{b7} up and down recall this session's messages";
    let rows = testing::texts(&note_rows(text, layout));
    assert!(rows.len() > 1, "{rows:#?}");
    let gutter = layout.gutter();
    assert!(rows[0].starts_with(&" ".repeat(gutter)));
    for row in &rows[1..] {
        assert!(
            row.starts_with(&" ".repeat(gutter + NOTE_HANG)),
            "{rows:#?}"
        );
    }
    words_whole(text, &rows);
    assert!(rows.iter().all(|r| r.chars().count() <= 40), "{rows:#?}");
}

#[tokio::test(start_paused = true)]
async fn verbose_puts_the_full_turn_line_in_the_footer() {
    for verbose in [false, true] {
        let screen = Screen::new(24, 100);
        let mut opts = options(Start::Stalled);
        opts.verbose = verbose;
        let turn = answer(["Hi there.\n".to_owned()]);
        session_with(screen.clone(), vec![turn], opts, |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hello");
            sleep(Duration::from_millis(3000)).await;
            run.send(ctrl('d'));
        })
        .await
        .unwrap();
        let lines = screen.visible();
        let full = lines
            .iter()
            .any(|l| l.contains("end_turn") && l.contains("in 0") && l.contains("out 0"));
        assert_eq!(full, verbose, "{lines:#?}");
    }
}

#[tokio::test(start_paused = true)]
async fn ctrl_o_while_streaming_keeps_consuming_the_stream_and_ctrl_c_still_stops_it() {
    let screen = Screen::new(24, 80);
    let words = (0..3000).map(|i| format!("w{i} "));
    session(
        screen.clone(),
        vec![answer(words)],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hello");
            sleep(Duration::from_millis(1000)).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(100)).await;
            assert!(run.screen.in_pager());
            assert!(run.screen.last_full());
            let frames = run.screen.frames();
            sleep(Duration::from_secs(3)).await;
            assert!(run.screen.frames() - frames <= 1, "a snapshot repainted");
            run.send(ctrl('c'));
            sleep(Duration::from_millis(500)).await;
            assert!(
                run.screen.in_pager(),
                "ctrl-c on a reply stops it, not the pager"
            );
            let shown = run.screen.visible();
            assert!(
                shown.iter().any(|l| l.contains(&copy::stopped(0))),
                "the turn's end refreshes the pager: {shown:#?}"
            );
            let closing = run.screen.frames();
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(200)).await;
            assert!(!run.screen.in_pager());
            assert!(run.screen.full_since(closing));
            let lines = run.screen.visible();
            assert!(
                lines.iter().any(|l| l.contains(&copy::stopped(0))),
                "{lines:#?}"
            );
            let all = lines.join(" ");
            assert!(!all.contains("w2999"), "{lines:#?}");
            assert!(
                unframed(&run.screen.caret_line())
                    .trim_start()
                    .starts_with(prompt())
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn esc_closes_the_transcript_and_then_stops_the_reply() {
    let screen = Screen::new(24, 80);
    let words = (0..3000).map(|i| format!("w{i} "));
    session(
        screen.clone(),
        vec![answer(words)],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hello");
            sleep(Duration::from_millis(500)).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(100)).await;
            assert!(run.screen.in_pager());
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(100)).await;
            assert!(!run.screen.in_pager());
            assert!(
                !run.screen
                    .visible()
                    .iter()
                    .any(|l| l.contains(&copy::stopped(0)))
            );
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(500)).await;
            assert!(
                run.screen
                    .visible()
                    .iter()
                    .any(|l| l.contains(&copy::stopped(0)))
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn transcript_mode_shows_full_detail_and_returns_to_the_same_view() {
    let screen = Screen::new(20, 80);
    let mut thinking = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: (0..30).map(|i| format!("thought {i}\n")).collect(),
        },
        AgentEvent::BlockStopped { index: 0 },
    ];
    thinking.extend(answer(["Done thinking.\n".to_owned()]).into_iter().skip(1));
    let mut script = numbered(5);
    script.insert(0, thinking);
    session(screen.clone(), script, Start::Stalled, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.say("think");
        sleep(Duration::from_secs(4)).await;
        ask_all(&run, 5).await;
        run.send(Input::Paste("a draft".to_owned()));
        sleep(Duration::from_millis(100)).await;
        let shown = run.screen.visible();
        for close in [key(KeyCode::Char('q')), key(KeyCode::Esc), ctrl('o')] {
            run.open_transcript().await;
            let pager = run.screen.visible();
            assert!(pager_hint(&pager), "{pager:#?}");
            assert!(
                pager.iter().any(|l| l.contains("answer 4 ends")),
                "{pager:#?}"
            );
            run.send(key(KeyCode::Char('g')));
            sleep(Duration::from_millis(100)).await;
            let head = run.screen.visible();
            assert!(
                head.iter().any(|l| l.contains(copy::TAGLINE_OWN)),
                "{head:#?}"
            );
            assert!(
                head.iter()
                    .any(|l| l.contains("cache 1h \u{b7} not logging this session")),
                "{head:#?}"
            );
            assert!(!shown.iter().any(|l| l.contains("cache 1h")), "{shown:#?}");
            for _ in 0..7 {
                run.send(wheel(MouseKind::WheelDown));
            }
            sleep(Duration::from_millis(100)).await;
            let top = run.screen.visible();
            assert!(top.iter().any(|l| l.contains("thought 25")), "{top:#?}");
            run.send(close);
            sleep(Duration::from_millis(100)).await;
            assert!(!run.screen.in_pager());
            assert_eq!(run.screen.visible(), shown);
            assert!(run.screen.caret_line().contains("a draft"));
        }
        run.quit();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn transcript_mode_opens_at_the_main_views_top_item_or_at_the_bottom() {
    let screen = Screen::new(20, 80);
    session(
        screen.clone(),
        numbered(8),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 8).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(100)).await;
            let pager = run.screen.visible();
            assert!(
                pager[..19].iter().any(|l| l.contains("answer 7 ends")),
                "{pager:#?}"
            );
            run.send(key(KeyCode::Char('q')));
            run.send(ctrl_key(KeyCode::Home));
            for _ in 0..6 {
                run.send(key(KeyCode::PageDown));
            }
            sleep(Duration::from_millis(100)).await;
            let main = run.screen.visible();
            let first = main
                .iter()
                .find(|l| l.contains("question ") || l.contains("answer "))
                .cloned()
                .unwrap();
            run.send(ctrl('o'));
            sleep(Duration::from_millis(100)).await;
            let pager = run.screen.visible();
            let n: String = first.chars().filter(char::is_ascii_digit).collect();
            let item_top = format!("answer {n} begins");
            assert!(
                pager[..3]
                    .iter()
                    .any(|l| first.contains(l.trim()) && !l.trim().is_empty())
                    || pager[..4].join(" ").contains(first.trim())
                    || (first.contains("answer") && pager[0].contains(&item_top))
                    || (first.contains("answer")
                        && pager[..3]
                            .iter()
                            .any(|l| l.contains(&format!("question {n}")))),
                "{first:?} {pager:#?}"
            );
            run.send(key(KeyCode::Char('q')));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_resize_while_paging_rewraps_once_after_the_debounce() {
    let screen = Screen::new(24, 60);
    let text = format!("{LONG_PROSE} {LONG_PROSE}");
    let expected = text.clone();
    session(
        screen.clone(),
        vec![answer([text])],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("wrap me");
            sleep(Duration::from_secs(3)).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(100)).await;
            let narrow = run.screen.visible();
            let longest = |rows: &[String]| rows.iter().map(|l| l.chars().count()).max().unwrap();
            for step in 0..15u16 {
                let size = run.screen.resize(24, 70 + (step % 5) * 10);
                run.send(Input::Resize(size));
                sleep(Duration::from_millis(20)).await;
                let clipped = run.screen.visible();
                assert!(run.screen.in_pager());
                assert_eq!(
                    longest(&clipped[..22]),
                    longest(&narrow[..22]),
                    "step {step}: {clipped:#?}"
                );
            }
            let size = run.screen.resize(24, 110);
            run.send(Input::Resize(size));
            sleep(Duration::from_millis(300)).await;
            let wide = run.screen.visible();
            assert!(longest(&wide[..22]) > longest(&narrow[..22]), "{wide:#?}");
            assert!(words_in_order(&wide, &expected), "{wide:#?}");
            let frames = run.screen.frames();
            sleep(Duration::from_secs(1)).await;
            assert_eq!(run.screen.frames(), frames, "an idle pager repaints");
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(300)).await;
            assert!(!run.screen.in_pager());
            let line = unframed(&run.screen.caret_line());
            assert!(line.trim_start().starts_with(prompt()), "{line:?}");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn ctrl_z_in_transcript_mode_returns_to_it_repainted() {
    let screen = Screen::new(30, 80);
    session(screen.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.send(Input::Paste("draft".to_owned()));
        run.send(ctrl('o'));
        sleep(Duration::from_millis(100)).await;
        assert!(run.screen.in_pager());
        run.send(ctrl('z'));
        sleep(Duration::from_millis(300)).await;
        assert_eq!(run.screen.suspended(), 1);
        assert!(!run.screen.on_alternate());
        run.screen.fg();
        run.send(Input::Resumed);
        sleep(Duration::from_millis(100)).await;
        assert!(run.screen.in_pager());
        assert!(run.screen.last_full());
        run.send(key(KeyCode::Char('q')));
        sleep(Duration::from_millis(100)).await;
        assert!(run.screen.caret_line().contains("draft"));
        run.quit();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_panic_restore_while_paging_repaints_the_transcript() {
    let screen = Screen::new(24, 80);
    session(screen.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.send(ctrl('o'));
        sleep(Duration::from_millis(100)).await;
        assert!(run.screen.in_pager());
        run.screen.panic_restore();
        run.send(Input::Restored);
        sleep(Duration::from_millis(100)).await;
        assert_eq!(run.screen.reapplied(), 1);
        assert!(run.screen.in_pager());
        assert!(run.screen.last_full());
        run.send(key(KeyCode::Char('q')));
        run.send(Input::Paste("still here".to_owned()));
        sleep(Duration::from_millis(100)).await;
        assert!(run.screen.caret_line().contains("still here"));
        run.quit();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn quitting_while_paging_ends_the_app() {
    let screen = Screen::new(24, 80);
    let exit = session(screen.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.send(ctrl('o'));
        sleep(Duration::from_millis(100)).await;
        assert!(run.screen.in_pager());
        run.send(Input::Closed("tty gone".to_owned()));
    })
    .await
    .unwrap();
    assert_eq!(exit, Exit::InputStopped("tty gone".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn escape_sequences_stay_inert_in_transcript_mode() {
    let screen = Screen::new(40, 100);
    let hostile: String = CORPUS.iter().map(|s| format!("A{s}Z ")).collect();
    let pasted = hostile.clone();
    session(
        screen.clone(),
        hostile_turns(&hostile),
        Start::Loaded(grammars(), default_feed()),
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say(&pasted);
            sleep(Duration::from_secs(5)).await;
            run.say("next");
            sleep(Duration::from_secs(5)).await;
            let before = run.screen.steady();
            run.open_transcript().await;
            assert!(run.screen.in_pager());
            let mut seen = vec![];
            for _ in 0..10 {
                seen.extend(run.screen.visible());
                run.send(key(KeyCode::PageUp));
                sleep(Duration::from_millis(50)).await;
            }
            assert!(
                seen.iter().filter(|l| l.contains("A^[]0;pwned^GZ")).count() >= 4,
                "{seen:#?}"
            );
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(100)).await;
            assert_eq!(run.screen.steady(), before);
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert_eq!(screen.spied(), Vec::<String>::new());
}

#[tokio::test(start_paused = true)]
async fn the_echo_replay_fixture_renders_through_the_tool_cell() {
    let fixture = include_str!("fixtures/tools-echo.jsonl");
    let events = replay::parse(fixture).unwrap();
    let screen = Screen::new(30, 80);
    session(
        screen.clone(),
        vec![events],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("echo it");
            sleep(Duration::from_secs(2)).await;
            let lines = run.screen.visible();
            let row = lines
                .iter()
                .find(|l| l.contains("Used echo hello from echo (+1 line)"))
                .unwrap_or_else(|| panic!("{lines:#?}"));
            assert!(row.trim_start().starts_with('\u{2713}'), "{row}");
            assert!(row.trim_end().ends_with("0s"), "{row}");
            assert!(
                !lines.iter().any(|l| l.contains("and a second line")),
                "{lines:#?}"
            );
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("2 requests") && l.contains("1 tool")),
                "{lines:#?}"
            );
            run.open_transcript().await;
            let pager = run.screen.visible();
            assert!(pager.iter().any(|l| l.contains("in ~/demo")), "{pager:#?}");
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(100)).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_view_files_replay_fixture_draws_file_rows_in_place() {
    let fixture = include_str!("fixtures/view-files.jsonl");
    let events = replay::parse(fixture).unwrap();
    let screen = Screen::new(60, 120);
    session(
        screen.clone(),
        vec![events],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("look at these");
            sleep(Duration::from_secs(2)).await;
            let lines = run.screen.visible();
            let row = |target: &str| {
                lines
                    .iter()
                    .find(|l| l.contains(target))
                    .cloned()
                    .unwrap_or_else(|| panic!("{target}: {lines:#?}"))
            };
            let image = row("Read screenshots/home.png");
            assert!(image.trim_start().starts_with('\u{2713}'), "{image}");
            assert!(
                image.contains("image/png \u{b7} 1.6 MB \u{b7} \u{2248}"),
                "{image}"
            );
            assert!(
                row("docs/spec.pdf")
                    .trim_end()
                    .ends_with("application/pdf \u{b7} 912 KB")
            );
            assert!(row("build.log").contains("text \u{b7} 2,000 of 9,412 lines"));
            for moved in [
                "build step 2000 ok",
                "song.mp3 is audio/mpeg",
                "withdrawn with 1 other attachment",
            ] {
                assert!(!lines.iter().any(|l| l.contains(moved)), "{lines:#?}");
            }
            assert!(
                row("Read song.mp3").trim_end().ends_with("couldn't read"),
                "{lines:#?}"
            );
            for path in ["shots/a.png", "shots/b.png"] {
                assert!(
                    row(&format!("Read {path}"))
                        .trim_end()
                        .ends_with("couldn't attach"),
                    "{lines:#?}"
                );
            }
            assert!(!lines.iter().any(|l| l.contains("from call")), "{lines:#?}");
            run.open_transcript().await;
            let pager = run.screen.visible().join("\n");
            assert!(
                pager.contains("Saved copy: /tmp/wisp-demo/attachments/"),
                "{pager}"
            );
            assert!(
                pager.contains("docs/spec.pdf: application/pdf, 912 KB, attached as is."),
                "{pager}"
            );
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(100)).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn any_key_jumps_the_welcome_reveal_to_its_final_frame() {
    let ring = "\u{2850}\u{2808} \u{2801}\u{2801}";
    let unkeyed = Screen::new(24, 80);
    session(unkeyed.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(50)).await;
        let rows = run.screen.visible();
        assert!(!rows[2].contains(ring), "{rows:#?}");
        sleep(Duration::from_millis(1200)).await;
        let rows = run.screen.visible();
        assert!(rows[2].contains(ring), "{rows:#?}");
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
    let keyed = Screen::new(24, 80);
    session(keyed.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(10)).await;
        run.send(key(KeyCode::Char('h')));
        sleep(Duration::from_millis(40)).await;
        let rows = run.screen.visible();
        assert!(rows[2].contains(ring), "{rows:#?}");
        assert!(
            rows[3].contains("\u{2830}  \u{28BE}\u{2877}  \u{2806}"),
            "{rows:#?}"
        );
        run.send(ctrl('c'));
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn escape_sequences_in_the_welcome_facts_stay_inert() {
    let hostile: String = CORPUS.iter().map(|s| format!("A{s}Z ")).collect();
    for width in [120, 50, 40] {
        let screen = Screen::new(60, width);
        let facts = SessionFacts {
            version: hostile.clone(),
            model: format!("claude-{hostile}"),
            effort: hostile.clone(),
            cwd: format!("~/{hostile}"),
            log: Some(format!("/tmp/{hostile}.jsonl")),
            host: Some(format!("gw{hostile}")),
            endpoint: Endpoint::Custom,
            ..facts()
        };
        finished_with(
            screen.clone(),
            vec![],
            options(Start::Stalled),
            facts,
            |run| async move {
                sleep(Duration::from_millis(500)).await;
                run.send(ctrl('d'));
            },
        )
        .await
        .exit
        .unwrap();
        assert_eq!(screen.spied(), Vec::<String>::new(), "{width}");
        let shown = screen.visible().join("\n");
        assert!(shown.contains("A^[]0;pwned^GZ"), "{width}: {shown}");
        assert_eq!(screen.leave(), screen.main_before(), "{width}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_tips_and_help_name_the_drag_and_newline_keys_this_terminal_has() {
    for (mouse, shift_enter) in [(true, false), (false, true), (true, true), (false, false)] {
        let screen = Screen::new(40, 120);
        let mut options = options(Start::Stalled);
        options.mouse = mouse;
        options.shift_enter = shift_enter;
        let tips = Rc::new(RefCell::new(vec![]));
        let seen = Rc::clone(&tips);
        session_with(screen.clone(), vec![], options, |run| async move {
            sleep(Duration::from_millis(100)).await;
            *seen.borrow_mut() = run.screen.visible();
            run.say("/help");
            sleep(Duration::from_millis(100)).await;
            run.send(ctrl('d'));
        })
        .await
        .unwrap();
        let tips = tips.borrow().clone();
        let lines = screen.visible();
        let tipped = |text: &str| tips.iter().any(|l| l.contains(text));
        let shows = |text: &str| lines.iter().any(|l| l.contains(text));
        let drag = if mouse { DRAG_CAPTURED } else { DRAG_NATIVE };
        assert!(!tipped("selects in the terminal"), "{mouse}: {tips:#?}");
        let kitty =
            "  enter send \u{b7} shift-enter new line \u{b7} /help \u{b7} /model switch model";
        let plain = "  enter send \u{b7} ctrl-j new line \u{b7} /help \u{b7} /model switch model";
        assert_eq!(tipped(kitty), shift_enter, "{tips:#?}");
        assert_eq!(tipped(plain), !shift_enter, "{tips:#?}");
        let select = lines
            .iter()
            .find(|l| l.contains("Select in the terminal"))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(select.contains(&format!("   {drag} ")), "{select}");
        assert_eq!(shows("Select, then copy"), mouse, "{lines:#?}");
        assert_eq!(shows("shift-drag"), mouse, "{lines:#?}");
        assert_eq!(shows("shift-enter"), shift_enter, "{lines:#?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_large_paste_is_a_chip_in_the_composer_and_the_transcript_and_full_in_ctrl_o() {
    let screen = Screen::new(30, 80);
    let pasted: String = (0..12)
        .map(|i| format!("pasted line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    session(
        screen.clone(),
        vec![answer(["noted".to_owned()])],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(300)).await;
            run.send(Input::Paste("Review ".to_owned()));
            run.send(Input::Paste(pasted.clone()));
            sleep(Duration::from_millis(100)).await;
            let lines = run.screen.visible();
            assert!(
                lines
                    .iter()
                    .any(|l| unframed(l).ends_with("Review [Pasted text #1 +12 lines]")),
                "{lines:#?}"
            );
            assert!(!lines.join(" ").contains("pasted line"), "{lines:#?}");
            run.send(key(KeyCode::Enter));
            sleep(Duration::from_secs(2)).await;
            let lines = run.screen.visible();
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("\u{203A} Review [Pasted text #1 +12 lines]")),
                "{lines:#?}"
            );
            assert!(lines.iter().any(|l| l.contains("noted")), "{lines:#?}");
            assert!(!lines.join(" ").contains("pasted line"), "{lines:#?}");
            run.send(ctrl('o'));
            sleep(Duration::from_millis(200)).await;
            assert!(run.screen.in_pager());
            let lines = run.screen.visible();
            for i in [0, 11] {
                assert!(
                    lines.iter().any(|l| l.trim() == format!("pasted line {i}")),
                    "{lines:#?}"
                );
            }
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(200)).await;
            run.send(key(KeyCode::Up));
            sleep(Duration::from_millis(100)).await;
            assert!(
                run.screen
                    .caret_line()
                    .contains("Review [Pasted text #1 +12 lines]"),
                "{:?}",
                run.screen.caret_line()
            );
            run.send(ctrl('c'));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

fn show_diff_events() -> Vec<AgentEvent> {
    replay::parse(include_str!("fixtures/show-diff.jsonl")).unwrap()
}

#[tokio::test]
async fn the_show_diff_replay_fixture_draws_a_highlighted_diff_cell() {
    let screen = Screen::new(40, 120);
    let code = Start::Loaded(grammars(), default_feed());
    session(
        screen.clone(),
        vec![show_diff_events()],
        code,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("show me");
            eventually(|| {
                shows(&run.screen.visible(), "\u{2713} Rename the cache key")
                    && run.screen.colours_of("fn key(req").len() > 2
            })
            .await;
            let lines = run.screen.visible();
            let has = |needle: &str| lines.iter().any(|l| l.contains(needle));
            assert!(has("\u{2713} Rename the cache key"), "{lines:#?}");
            assert!(has("docs/key.md → docs/cache.md"), "{lines:#?}");
            assert!(
                has(" 207 -         let key = hash(&req.system);"),
                "{lines:#?}"
            );
            assert!(
                has(" 207 +         let key = Fingerprint::of(req)?;"),
                "{lines:#?}"
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert!(screen.colours_of("fn key(req").len() > 2);
}

#[tokio::test]
async fn the_checklist_replay_draws_the_list_in_its_row_and_no_progress_in_the_status_line() {
    let screen = Screen::new(30, 120);
    let events = replay::parse(include_str!("fixtures/checklist.jsonl")).unwrap();
    session(
        screen.clone(),
        vec![events],
        Start::Loaded(grammars(), default_feed()),
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("plan it");
            eventually(|| shows(&run.screen.visible(), "\u{2212} Keep the old renderer")).await;
            let lines = run.screen.visible();
            let has = |needle: &str| lines.iter().any(|l| l.contains(needle));
            assert!(
                has("\u{2713} Used write_todos 2 of 5 completed"),
                "{lines:#?}"
            );
            assert!(has("     \u{2713} Read the plan"), "{lines:#?}");
            assert!(has("     \u{25CC} Write the parser"), "{lines:#?}");
            assert!(
                has("     \u{25E6} Draw the list in the tool row"),
                "{lines:#?}"
            );
            assert!(has("     \u{2212} Keep the old renderer"), "{lines:#?}");
            assert!(!has("todo 2/5"), "{lines:#?}");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_diff_cell_finished_before_the_grammars_arrive_is_highlighted_when_they_do() {
    let screen = Screen::new(40, 120);
    let (arrive, later) = std::sync::mpsc::channel();
    let code = Start::Later(later, default_feed());
    session(
        screen.clone(),
        vec![show_diff_events()],
        code,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("show me");
            eventually(|| shows(&run.screen.visible(), "say the word and I'll apply it")).await;
            assert!(
                run.screen
                    .visible()
                    .iter()
                    .any(|l| l.contains("say the word and I'll apply it")),
                "{:#?}",
                run.screen.visible()
            );
            assert_eq!(run.screen.colours_of("fn key(req").len(), 2);
            arrive.send(grammars()).unwrap();
            eventually(|| run.screen.colours_of("fn key(req").len() > 2).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert!(screen.colours_of("fn key(req").len() > 2);
}

struct OpenOnDrop(Arc<AtomicBool>);

impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn gated_feed(open: &Arc<AtomicBool>, free: usize) -> Feed {
    let gate = Arc::clone(open);
    let mut fed = 0;
    Box::new(move |worker, delta, cancel| {
        fed += 1;
        while fed > free && !gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.apply_with_cancel(delta, cancel)
    })
}

fn show_diff_then_a_long_reply() -> Vec<AgentEvent> {
    let mut events = show_diff_events();
    let at = events
        .iter()
        .rposition(|e| matches!(e, AgentEvent::TextDelta { .. }))
        .unwrap();
    let words: Vec<AgentEvent> = (0..2000)
        .map(|i| AgentEvent::TextDelta {
            index: 0,
            text: format!(" w{i}"),
        })
        .collect();
    events.splice(at + 1..at + 1, words);
    events
}

#[tokio::test]
async fn esc_during_the_turn_leaves_a_diff_cell_to_be_highlighted() {
    let screen = Screen::new(40, 120);
    let (arrive, later) = std::sync::mpsc::channel();
    let code = Start::Later(later, default_feed());
    session(
        screen.clone(),
        vec![show_diff_then_a_long_reply()],
        code,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("show me");
            eventually(|| shows(&run.screen.visible(), "Rename the cache key")).await;
            let shown = run.screen.visible();
            assert!(
                shown.iter().any(|l| l.contains("Rename the cache key")),
                "{shown:#?}"
            );
            assert!(!shown.iter().any(|l| l.contains("Stopped.")), "{shown:#?}");
            run.send(key(KeyCode::Esc));
            eventually(|| shows(&run.screen.visible(), "Stopped.")).await;
            assert!(
                run.screen.visible().iter().any(|l| l.contains("Stopped.")),
                "{:#?}",
                run.screen.visible()
            );
            assert_eq!(run.screen.colours_of("fn key(req").len(), 2);
            arrive.send(grammars()).unwrap();
            eventually(|| run.screen.colours_of("fn key(req").len() > 2).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert!(screen.colours_of("fn key(req").len() > 2);
}

#[tokio::test]
async fn after_the_grammars_fail_a_diff_cell_stays_structural() {
    let screen = Screen::new(40, 120);
    let code = Start::FailedButServing(grammars(), default_feed());
    session(
        screen.clone(),
        vec![show_diff_events()],
        code,
        |run| async move {
            sleep(Duration::from_millis(200)).await;
            run.say("show me");
            sleep(Duration::from_millis(1500)).await;
            assert!(
                run.screen
                    .visible()
                    .iter()
                    .any(|l| l.contains("say the word and I'll apply it")),
                "{:#?}",
                run.screen.visible()
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert_eq!(screen.colours_of("fn key(req").len(), 2);
}

fn code_answer_events() -> Vec<AgentEvent> {
    answer([
        "Here it is:\n\n```rust\nfn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n```\n\nDone.\n".to_owned(),
    ])
}

#[tokio::test]
async fn a_fence_finished_before_the_grammars_arrive_is_highlighted_when_they_do() {
    let screen = Screen::new(30, 100);
    let (arrive, later) = std::sync::mpsc::channel();
    let code = Start::Later(later, default_feed());
    session(
        screen.clone(),
        vec![code_answer_events()],
        code,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("code please");
            eventually(|| shows(&run.screen.visible(), "Done.")).await;
            assert!(
                run.screen.visible().iter().any(|l| l.contains("Done.")),
                "{:#?}",
                run.screen.visible()
            );
            assert_eq!(run.screen.colours_of("println!").len(), 1);
            arrive.send(grammars()).unwrap();
            eventually(|| run.screen.colours_of("println!").len() > 1).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert!(screen.colours_of("println!").len() > 1);
}

fn long_fence_then_words() -> Vec<AgentEvent> {
    let body: String = (0..40)
        .map(|i| format!("    let v{i} = {i} + 1;\n"))
        .collect();
    let mut chunks = vec![format!("Here:\n\n```rust\nfn main() {{\n{body}}}\n```\n\n")];
    chunks.extend((0..2000).map(|i| format!(" w{i}")));
    answer(chunks)
}

#[tokio::test]
async fn a_fence_esc_stopped_mid_highlight_ends_fully_highlighted() {
    let screen = Screen::new(70, 100);
    let open = Arc::new(AtomicBool::new(false));
    let code = Start::Loaded(grammars(), gated_feed(&open, 20));
    session(
        screen.clone(),
        vec![long_fence_then_words()],
        code,
        |run| async move {
            let _open = OpenOnDrop(Arc::clone(&open));
            sleep(Duration::from_millis(100)).await;
            run.say("code please");
            eventually(|| shows(&run.screen.visible(), "let v39 =")).await;
            run.send(key(KeyCode::Esc));
            eventually(|| shows(&run.screen.visible(), "Stopped.")).await;
            assert!(
                run.screen.visible().iter().any(|l| l.contains("Stopped.")),
                "{:#?}",
                run.screen.visible()
            );
            assert_eq!(run.screen.colours_of("let v39 =").len(), 1);
            open.store(true, Ordering::SeqCst);
            eventually(|| run.screen.colours_of("let v39 =").len() > 1).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    for i in [0, 20, 39] {
        assert!(
            screen.colours_of(&format!("let v{i} =")).len() > 1,
            "line {i}"
        );
    }
}
