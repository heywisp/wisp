use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Position, Rect, Size};
use ratatui_core::style::{Modifier, Style};
use tokio::sync::mpsc;
use tokio::time::Instant;

use wisp_media::MAX_IMAGES_PER_MESSAGE;
use wisp_protocol::context::ReportDetail;
use wisp_protocol::settings::takes_no_effort;
use wisp_protocol::{
    ALREADY_COMPACTING, AgentEvent, AgentPath, ClientCommand, DeliveryMode, Effort, InputId,
    RequestId,
};
use wisp_render::chrome::account::AccountState;
use wisp_render::chrome::{
    AGENTS_MIN_ROWS, Away, BOX_FROM_ROWS, ChatTurn, ChatsPlace, ComposerShape, DOCK_MIN_ROWS, Edge,
    EndpointFacts, FRAMED_STATUS_FROM_ROWS, ImageTier, Joins, KeyHint, PS_MIN_ROWS,
    STATUS_FROM_ROWS, State, Status, Summary, TOP_ROW_FROM_ROWS, WELCOME_SETTLED, add_kept,
    chats_fit, chats_whole, composer_edge, dock_room, down_arrow, fits_box, frame_inner, frame_row,
    releasable, rewind_notes, short_model, waiting_rows, welcome, welcome_next_change,
    without_models,
};
use wisp_render::highlight::Highlighter;
use wisp_render::markdown::{CodeStyle, Layout, Renderer};
use wisp_render::motion::{MotionCtx, MotionPolicy, OrbFrame, Pacing};
use wisp_render::picture::{ramps, raster};
use wisp_render::styled::{Look, StyledLine, StyledSpan};
use wisp_render::text::{width as text_width, wrap_words};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::theme::{Depth, Theme, Token};
use wisp_render::transcript::{
    CellKind, Config, Detail, Transcript, UserPart, View as TranscriptView, copy,
};

use super::agents::{Child, agent_facts};
use super::commands::{self, CommandId, Parsed};
use super::frames::Frames;
use super::help::first_keys;
use super::highlight::{Highlights, Reply, Start};
use super::inputs::Inputs;
use super::keymap::{Action, Context, ViewScroll, action};
use super::list::Pick;
use super::pager::{Pager, RowCounts, Seek};
use super::picker::{Picked, Picker, Step, display_effort, effort_levels, offers_typed, switch_to};
use super::settings::{Answer, CYCLE_HINT, Tracker};
use super::suggest::Suggest;
use super::tool_files::{Loader, PAGER_BUDGET};
use super::view::{Caret, Origin, SliceClock, Source, View, Wait};
use super::{Session, SessionFacts};
use crate::term::clipboard::reader::{self, Got, Job, Reader};
use crate::term::clipboard::{Clipboard, Osc52, Outcome};
use crate::term::contain;
use crate::term::input::{HUNG_UP, typed};
use crate::term::picture::{self, Encoder, Tiles};
use crate::term::screen::{Painter, Plan};
use crate::term::{
    Console, Halted, Input, Key, KeyCode, Mouse, MouseKind, Pictures, RESIZE_DEBOUNCE, Written,
};
use crate::widgets::composer::{Composer, ComposerView, Message, Saved};
use subscription_login::Then;
use wisp_client::QuitBound;
use wisp_protocol::valid_endpoint_name;

fn feed_status(
    status: &mut Status,
    transcript: &Transcript,
    event: &AgentEvent,
    effort: Option<&str>,
    at: Duration,
    cx: MotionCtx,
    layout: Layout,
) {
    match event {
        AgentEvent::TurnStarted { .. } => status.turn_effort(effort),
        AgentEvent::ToolCallStarted { call_id, .. } => {
            let doing = transcript.call_doing(call_id, layout).unwrap_or_default();
            status.name_call(call_id, &doing);
        }
        _ => {}
    }
    status.apply(event, at, cx);
}

pub const MAX_RESTORES: u8 = 3;
pub const WHEEL_ROWS: usize = 3;
pub const TERMINAL_PASTE_WINDOW: Duration = Duration::from_millis(500);
pub use super::view::SLICE;
pub const AGAIN_TO_QUIT: &str = "Press ctrl-c again to quit.";
pub const AGENT_STOPPED: &str = "The agent stopped, so nothing more can be sent.";
pub const NEWLINE_KEYS: &str = "alt-enter or ctrl-j";
pub const NEWLINE_KEYS_KITTY: &str = "shift-enter, alt-enter or ctrl-j";
pub const DRAG_CAPTURED: &str = SELECT_KEY;
pub const DRAG_NATIVE: &str = "drag";

pub(super) fn drag_key(mouse: bool, captured: &'static str) -> &'static str {
    if mouse { captured } else { DRAG_NATIVE }
}

pub use crate::term::clipboard::{SELECT_KEY, SELECT_KEY_REMOTE};
pub use crate::term::doctor::VS_CODE_WHEEL;
pub(super) const CONTEXT_USAGE: &str = "/context takes all";
pub(super) const CONTEXT_COUNTS: &str = "/context counts automatically now (context.count_tokens)";
pub const TMUX_MOUSE_OFF: &str = "tmux has the mouse off, so the wheel scrolls tmux. Add \"set -g mouse on\" to ~/.tmux.conf, or use PgUp and PgDn.";
pub const GNU_SCREEN_KEEPS: &str =
    "GNU screen keeps Wisp's screen after you quit unless \"altscreen on\" is in ~/.screenrc.";
pub const NEW_KEY: &str = "ctrl-end";

pub fn recovered(count: u8) -> String {
    format!(
        "Recovered from an internal error ({count} of {MAX_RESTORES}). The details are printed above your shell prompt after you quit."
    )
}

pub const NOTE_HANG: usize = 2;

pub fn note_rows(text: &str, layout: Layout) -> Vec<StyledLine> {
    let gutter = layout.gutter();
    let measure = layout.width.saturating_sub(gutter + NOTE_HANG).max(1);
    wrap_words(
        &[StyledSpan::new(text, Token::TextMuted)],
        measure,
        layout.width_mode,
    )
    .into_iter()
    .enumerate()
    .map(|(i, row)| {
        let lead = if i == 0 { gutter } else { gutter + NOTE_HANG };
        let mut spans = vec![StyledSpan::plain(" ".repeat(lead))];
        spans.extend(row.spans);
        StyledLine::new(spans)
    })
    .collect()
}

pub fn host_errors(path: &std::path::Path) -> Vec<String> {
    let errors = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<_> = errors
        .lines()
        .rev()
        .take(5)
        .map(wisp_render::text::sanitize)
        .collect();
    lines.reverse();
    lines
}

pub fn new_hint(count: usize, glyphs: GlyphSet) -> String {
    let g = Glyphs::of(glyphs);
    format!("{} {count} new {} {NEW_KEY}", down_arrow(&g), g.separator)
}

#[derive(Default)]
pub struct Unsent {
    composer: Composer,
    card: Option<super::question::Card>,
    unconfirmed: Vec<(InputId, Saved)>,
    returned: BTreeSet<InputId>,
    late: Option<(InputId, String)>,
    resend: Vec<Saved>,
    children: Vec<Child>,
}

fn printable(message: &Message) -> String {
    let mut text = String::new();
    let mut after_image = false;
    for part in &message.parts {
        let piece = match part {
            UserPart::Text(text) => text.as_str(),
            UserPart::Pasted { text, .. } => text.as_ref(),
            UserPart::Image { label } => label.as_str(),
        };
        if after_image && !piece.starts_with(char::is_whitespace) {
            text.push('\n');
        }
        text.push_str(piece);
        after_image = matches!(part, UserPart::Image { .. });
    }
    text
}

impl Unsent {
    pub fn texts(&self) -> Vec<String> {
        let text = printable(&self.composer.message());
        let mut texts: Vec<_> = self
            .unconfirmed
            .iter()
            .map(|(_, saved)| printable(&saved.message()))
            .collect();
        if !text.trim().is_empty() {
            texts.push(text);
        }
        if let Some(card) = &self.card {
            texts.extend(
                card.typed_texts()
                    .iter()
                    .map(|saved| printable(&saved.message())),
            );
        }
        for child in &self.children {
            let pieces: Vec<_> = child
                .unconfirmed
                .iter()
                .map(|(_, saved)| saved)
                .chain(&child.texts)
                .map(|saved| printable(&saved.message()))
                .collect();
            if !pieces.is_empty() {
                texts.push(format!(
                    "{}\n{}",
                    copy::to_agent(child.agent.as_str()),
                    pieces.join("\n\n")
                ));
            }
        }
        texts
    }

    pub fn is_empty(&self) -> bool {
        self.unconfirmed.is_empty() && self.composer.is_empty() && self.children.is_empty()
    }

    pub fn reopened(&mut self, known: &[String]) {
        let pending = |unconfirmed: &mut Vec<(InputId, Saved)>| {
            std::mem::take(unconfirmed)
                .into_iter()
                .filter(|(id, _)| !known.contains(&id.0))
                .map(|(_, saved)| saved)
                .collect::<Vec<_>>()
        };
        self.composer.restore_ahead(&pending(&mut self.unconfirmed));
        for child in &mut self.children {
            child.texts.splice(0..0, pending(&mut child.unconfirmed));
        }
        self.children.retain(|child| !child.texts.is_empty());
    }

    pub fn send_again<'a>(
        &'a mut self,
        channels: &'a mut wisp_client::Channels,
        used: &'a mut Vec<String>,
        returned: &'a [wisp_protocol::Returned],
        stop: impl Future<Output = ()> + 'a,
    ) -> std::pin::Pin<Box<impl Future<Output = bool> + 'a>> {
        Box::pin(async move {
            let mut number = InputId::highest_numbered(
                super::inputs::ID_PREFIX,
                used.iter()
                    .map(String::as_str)
                    .chain(self.returned.iter().map(InputId::as_str)),
            );
            let mut accepted = 0;
            let mut buffered = Vec::new();
            let mut in_flight = None;
            let sending = async {
                for saved in &self.resend {
                    number = number.saturating_add(1);
                    let id = InputId(format!("{}{number}", super::inputs::ID_PREFIX));
                    used.push(id.0.clone());
                    let command = waiting::send_command(
                        id.clone(),
                        &saved.message(),
                        DeliveryMode::Steer,
                        None,
                        None,
                    );
                    if channels.commands.send(command).await.is_err() {
                        return false;
                    }
                    in_flight = Some(id.clone());
                    if accepted + 1 == self.resend.len() {
                        let ids = returned
                            .iter()
                            .filter_map(|r| r.id.as_ref())
                            .filter(|id| self.returned.contains(*id))
                            .cloned()
                            .collect::<Vec<_>>();
                        if !ids.is_empty()
                            && channels
                                .commands
                                .send(ClientCommand::ReturnedTaken { ids })
                                .await
                                .is_err()
                        {
                            return false;
                        }
                    }
                    loop {
                        let Some(event) = channels.events.recv().await else {
                            return false;
                        };
                        let received = matches!(&event, AgentEvent::InputAccepted { id: got, .. } if *got == id);
                        let refused = matches!(&event, AgentEvent::Rejected { id: Some(got), .. } if *got == id);
                        buffered.push(event);
                        if received {
                            in_flight = None;
                            accepted += 1;
                            break;
                        }
                        if refused {
                            in_flight = None;
                            return false;
                        }
                    }
                }
                true
            };
            let sent = tokio::select! {
                biased;
                sent = sending => sent,
                _ = stop => false,
            };
            self.resend.drain(..accepted);
            if !sent {
                let pieces = self
                    .composer
                    .restore_ahead(&std::mem::take(&mut self.resend));
                self.late = in_flight.zip(pieces.into_iter().next());
            }
            if !buffered.is_empty() {
                let (output, events) = mpsc::channel(buffered.len().max(16));
                for event in buffered {
                    let _ = output.try_send(event);
                }
                let mut tail = std::mem::replace(&mut channels.events, events);
                tokio::spawn(async move {
                    while let Some(event) = tail.recv().await {
                        if output.send(event).await.is_err() {
                            break;
                        }
                    }
                });
            }
            sent
        })
    }
}

pub struct Options {
    pub unsent: Option<Unsent>,
    pub theme: Theme,
    pub glyphs: GlyphSet,
    pub typed: Vec<u8>,
    pub code: Start,
    pub motion: MotionPolicy,
    pub shift_enter: bool,
    pub mouse: bool,
    pub verbose: bool,
    pub scroll_speed: u16,
    pub full_repaint: bool,
    pub pictures: Option<(Pictures, (u16, u16))>,
    pub notes: Vec<String>,
    pub started: Option<super::resumed::StartNote>,
    pub copy_on_release: bool,
    pub clipboard: Clipboard,
    pub images: reader::Setup,
    pub select_key: &'static str,
    pub right_paste: bool,
    pub terminal_pastes_on_right_click: bool,
    pub verb_seed: u64,
    pub quit: Option<QuitBound>,
    pub card_setup: Option<super::endpoint_card::CardSetup>,
    pub file_asked: bool,
    pub display: super::config_screen::Display,
    pub kept: Option<super::display::Kept>,
    pub zone: jiff::tz::TimeZone,
    pub now: fn() -> jiff::Timestamp,
    pub open_processes: bool,
    pub list_agents: bool,
    pub sidebar: crate::term::SidebarSetting,
    pub attached: Option<super::Attached>,
}

impl Options {
    pub fn new(theme: Theme, glyphs: GlyphSet) -> Options {
        Options {
            unsent: None,
            attached: None,
            theme,
            glyphs,
            typed: vec![],
            code: Start::Worker(super::highlight::default_feed()),
            motion: MotionPolicy::Full,
            shift_enter: false,
            mouse: true,
            verbose: false,
            scroll_speed: 1,
            full_repaint: false,
            pictures: None,
            notes: vec![],
            started: None,
            copy_on_release: true,
            clipboard: Clipboard::none(),
            images: reader::Setup::none(),
            select_key: SELECT_KEY,
            right_paste: false,
            terminal_pastes_on_right_click: false,
            verb_seed: 0,
            quit: None,
            card_setup: None,
            file_asked: false,
            display: super::config_screen::Display::default(),
            kept: None,
            zone: jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC),
            now: jiff::Timestamp::now,
            open_processes: false,
            list_agents: false,
            sidebar: crate::term::SidebarSetting::Auto,
        }
    }
}

type Expiry = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    Taken,
    Lost,
    Cut,
    Restarting,
    Quit,
    Leave,
    Resume(String),
    Ended,
    Dying,
    InputStopped(String),
    Forced { save_waiting: bool },
    SetupCancelled,
    SetupSaved { name: String, forced: bool },
    SaveAbandoned,
    StartFailed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("the terminal was restored after {0} panics, so Wisp stopped")]
    Panics(u8),
}

pub struct Finished {
    pub unsent: Option<Unsent>,
    pub exit: Result<Exit, AppError>,
    pub goodbye: Vec<StyledLine>,
    pub width: usize,
    pub theme: Theme,
    pub unshown: Vec<String>,
}

pub(super) fn halted_of(e: &io::Error) -> Option<Halted> {
    e.get_ref()
        .and_then(|e| e.downcast_ref::<Halted>())
        .copied()
}

const SUSPENDED: &str = "the terminal is suspended";
const UNWRITTEN: &str = "the terminal couldn't be written";

pub(super) fn terminal_copy<C: Console>(
    console: &mut C,
    text: &str,
) -> (Outcome, Option<io::Error>) {
    let Ok(sequence) = Osc52::new(text) else {
        return (Outcome::TooLarge, None);
    };
    match console.clipboard(&sequence) {
        Ok(Written::Sent | Written::Resized(_)) => (Outcome::Sent, None),
        Ok(Written::Suspended) => (Outcome::Failed(SUSPENDED.to_owned()), None),
        Ok(Written::Refused(halted)) => {
            (Outcome::Failed(UNWRITTEN.to_owned()), Some(halted.into()))
        }
        Err(e) if halted_of(&e).is_some() => (Outcome::Failed(UNWRITTEN.to_owned()), Some(e)),
        Err(e) => (Outcome::Failed(format!("{UNWRITTEN}: {e}")), None),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rows {
    pub view: u16,
    pub top_row: bool,
    pub top_edge: bool,
    pub composer: u16,
    pub bottom_edge: bool,
    pub status: bool,
    pub framed: bool,
}

const PICKER_MIN_ROWS: usize = DOCK_MIN_ROWS;
const VIEW_MIN_ROWS: usize = 2;

fn caret_style(theme: &Theme) -> Style {
    match theme.depth() {
        Depth::TrueColor | Depth::Ansi256 => theme.style(&Look::fg(Token::Bg).on(Token::Text)),
        Depth::Ansi16 | Depth::None => Style::default().add_modifier(Modifier::REVERSED),
    }
}

pub(super) fn paint_lines<'a>(
    buffer: &mut Buffer,
    top: u16,
    width: u16,
    lines: impl IntoIterator<Item = &'a StyledLine>,
    theme: &Theme,
) {
    for (i, line) in lines.into_iter().enumerate() {
        let row = top.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        buffer.set_string(0, row, " ".repeat(usize::from(width)), Style::reset());
        buffer.set_line(0, row, &theme.line(line), width);
    }
}

fn boxed_at(size: Size) -> bool {
    fits_box(usize::from(size.width), usize::from(size.height))
}

fn folded(rows: Rows) -> Rows {
    let bottom_edge = rows.bottom_edge && rows.framed;
    Rows {
        view: rows.view
            + rows.composer
            + u16::from(rows.top_edge)
            + u16::from(rows.bottom_edge && !bottom_edge),
        top_edge: false,
        composer: 0,
        bottom_edge,
        ..rows
    }
}

fn chip_cells(
    buffer: &Buffer,
    y: u16,
    chip: &wisp_render::chrome::account::Chip,
    glyphs: &Glyphs,
) -> Option<std::ops::Range<u16>> {
    let cells: Vec<&str> = (0..buffer.area.width)
        .map(|x| buffer.cell(Position { x, y }).map_or("", |c| c.symbol()))
        .collect();
    let tail: Vec<String> = format!(
        " {} {}",
        glyphs.separator,
        wisp_render::chrome::account::billing_word(chip.billed)
    )
    .chars()
    .map(String::from)
    .collect();
    (0..cells.len()).find_map(|at| {
        let end = at + tail.len();
        let spelled = cells.get(at..end)?.iter().zip(&tail).all(|(c, t)| c == t);
        let start = cells[..at]
            .iter()
            .rposition(|c| *c == " " || c.is_empty())
            .map_or(0, |i| i + 1);
        let shown: String = cells[start..at].concat();
        let named = !shown.is_empty()
            && (shown == chip.name
                || shown
                    .strip_suffix(glyphs.more)
                    .is_some_and(|head| chip.name.starts_with(head)));
        (spelled && named).then(|| {
            u16::try_from(start).unwrap_or(u16::MAX)..u16::try_from(end).unwrap_or(u16::MAX)
        })
    })
}

pub fn rows_for(size: Size, composer: usize) -> Rows {
    let h = size.height;
    let tall = |from: usize| usize::from(h) >= from;
    let boxed = boxed_at(size);
    let top_row = boxed && tall(TOP_ROW_FROM_ROWS);
    let framed = boxed && tall(FRAMED_STATUS_FROM_ROWS);
    let status = tall(STATUS_FROM_ROWS);
    let composer = if tall(BOX_FROM_ROWS) { composer } else { 1 };
    let composer = u16::try_from(composer.max(1)).unwrap_or(u16::MAX);
    let fixed =
        u16::from(status) + 2 * u16::from(boxed) + 2 * u16::from(top_row) + u16::from(framed);
    let composer = composer.min(h.saturating_sub(fixed + 1)).max(1).min(h);
    Rows {
        view: h.saturating_sub(fixed + composer),
        top_row,
        top_edge: boxed,
        composer,
        bottom_edge: boxed,
        status,
        framed,
    }
}

fn shed(rows: Rows) -> Option<Rows> {
    let gain = match (rows.top_row, rows.framed) {
        (true, _) => 2,
        (false, true) => 1,
        (false, false) => return None,
    };
    Some(Rows {
        view: rows.view + gain,
        top_row: false,
        framed: rows.framed && rows.top_row,
        ..rows
    })
}

fn joins(rows: Rows) -> Joins {
    Joins {
        above: rows.top_row,
        below: rows.framed,
    }
}

fn rows_fitting(size: Size, composer: usize, fold: bool, fits: impl Fn(Rows) -> bool) -> Rows {
    let shaped = |rows| if fold { folded(rows) } else { rows };
    let mut rows = rows_for(size, composer);
    while !fits(shaped(rows))
        && let Some(next) = shed(rows)
    {
        rows = next;
    }
    shaped(rows)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Dock(retry::Docked),
    Sidebar,
    Usage,
    Question,
    Endpoint,
    Processes,
    Agents,
    StepIn,
    Suggestions,
    Auth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PasteTarget {
    Drop,
    Composer,
    Chat,
    Attached,
    Pager,
    Settings,
    Layer(Layer),
}

impl PasteTarget {
    fn is_composer(self) -> bool {
        self == Self::Composer
    }
}

struct MainLayout {
    shape: ComposerShape,
    composer: ComposerView,
    rows: Rows,
    waiting: Vec<StyledLine>,
    dock: Vec<StyledLine>,
    above: u16,
}

pub(super) struct Presenter {
    pub(super) transcript: Transcript,
    view: View,
    composer: Composer,
    returned: BTreeSet<InputId>,
    late: Option<(InputId, String)>,
    resend: Vec<crate::widgets::composer::Saved>,
    status: Status,
    plan_usage: BTreeMap<String, wisp_protocol::plan_usage::UsageState>,
    usage_page: Option<usage_page::Page>,
    usage_endpoint: Option<String>,
    usage_catalog: bool,
    usage_initial: Option<RequestId>,
    usage_refreshes: BTreeMap<RequestId, String>,
    spend_answer: Option<(Vec<wisp_protocol::SpendDay>, wisp_protocol::SpendWindow)>,
    spend_request: Option<RequestId>,
    spend_again: bool,
    refused_keys: BTreeSet<String>,
    claude_usage: bool,
    pacing: Pacing,
    banner_done: bool,
    chrome_due: Option<Duration>,
    idle_due: Option<Duration>,
    seats: Vec<idle::Seat>,
    chip_at: Option<(u16, std::ops::Range<u16>)>,
    pub(super) frames: Frames,
    painter: Painter,
    pictures: Option<(Pictures, (u16, u16))>,
    pictures_lost: bool,
    code: Highlights,
    shift_enter: bool,
    mouse: bool,
    select_key: &'static str,
    right_paste: bool,
    terminal_pastes_on_right_click: bool,
    terminal_paste_until: Option<Duration>,
    suspended: bool,
    input_stopped: Option<String>,
    commands: Option<mpsc::Sender<ClientCommand>>,
    facts: SessionFacts,
    theme: Theme,
    glyphs: GlyphSet,
    start: Instant,
    cancel: bool,
    quitting: bool,
    armed: bool,
    restores: u8,
    exit: Option<Exit>,
    quit_note: Option<String>,
    pager: Option<Pager>,
    pager_rows: RowCounts,
    pager_welcome: Vec<StyledLine>,
    files: Loader,
    size: Size,
    resize_at: Option<Instant>,
    hint_area: Option<Rect>,
    scroll_speed: u16,
    turns: u32,
    stopped: Option<usize>,
    picker: Option<Picker>,
    account_describing: Option<(RequestId, AgentPath, String)>,
    opener: Option<super::opener::Opener>,
    opener_hits: Vec<(u16, usize)>,
    tracker: Tracker,
    select: Selecting,
    inputs: Inputs,
    images: Reader,
    reading: Option<Reading>,
    card: Option<super::question::Card>,
    card_back: Option<super::question::Card>,
    card_settle: super::question::Settle,
    card_hits: Vec<(u16, wisp_render::chrome::CardHit)>,
    card_seen: bool,
    step_in_seen: bool,
    step_pick: Pick,
    secret_field: crate::widgets::secret::SecretField,
    secret_hidden: bool,
    secret_sent: bool,
    secret_problem: String,
    secret_drop: Option<super::question::Settle>,
    last_typed: Option<Duration>,
    bell: bool,
    requests: u64,
    leave_note: Option<String>,
    host_end: Option<mpsc::Sender<bool>>,
    host_quitting: bool,
    host_ended: Option<tokio::sync::watch::Receiver<Option<wisp_client::link::LinkEnd>>>,
    host_error: Option<std::path::PathBuf>,
    suggest: Suggest,
    listed: Option<suggest::Listed>,
    adding: endpoint::Adding,
    authentication: subscription_login::Authentication,
    continuing: Option<resumed::Held>,
    retry: super::retry::Choice,
    settings_screen: Option<super::config_screen::Screen>,
    settings_list: Option<config_screen::ListArea>,
    settings_draft: Option<super::config_screen::Draft>,
    settings_later: std::collections::BTreeSet<super::config_screen::Mark>,
    adding_model: bool,
    display: super::config_screen::Display,
    kept: Option<super::display::Kept>,
    modes_due: Option<bool>,
    rewind: Option<super::rewind::Rewind>,
    chats: Option<super::chats::Chats>,
    chats_refused: bool,
    resume_to: Option<String>,
    rewind_primed: Option<Duration>,
    rewind_sent: Option<RequestId>,
    rewind_notes: Vec<String>,
    rewind_hits: Vec<(u16, usize)>,
    zone: jiff::tz::TimeZone,
    now: fn() -> jiff::Timestamp,
    processes: super::processes::Processes,
    ps: Option<super::processes::List>,
    movable: Option<String>,
    list_on_start: bool,
    step_settle: super::question::Settle,
    prompt_dropped: Option<Duration>,
    warned: bool,
    attach_pending: bool,
    attach_sent: Option<wisp_protocol::TermSize>,
    composer_keys: bool,
    agents: super::agents::Agents,
    agents_list: Option<agents::AgentsList>,
    chat: Option<agents::AgentChat>,
    agent_models: super::agents::Models,
    main_since: Option<Duration>,
    exit_armed: bool,
    sidebar: super::sidebar::Side,
    panel_area: Option<sidebar::PanelArea>,
    painted: Vec<(Layer, Rect)>,
    chat_area: Rect,
    sidebar_click: Option<crate::app::selection::Click>,
    plugin_runs: BTreeMap<RequestId, u64>,
    terminal_sent: Option<wisp_protocol::TermSize>,
}

struct Parts<'a> {
    transcript: &'a mut Transcript,
    inputs: &'a mut Inputs,
    composer: &'a mut Composer,
    status: &'a mut Status,
    view: &'a mut View,
    tracker: &'a mut Tracker,
    runs: &'a mut BTreeMap<RequestId, u64>,
}

fn host_exit(ended: Option<wisp_client::link::LinkEnd>, quitting: bool, saving: bool) -> Exit {
    use wisp_client::link::LinkEnd;
    match ended {
        Some(LinkEnd::Taken) => Exit::Taken,
        Some(LinkEnd::Restarting) => Exit::Restarting,
        Some(LinkEnd::Ended { saved: true }) => Exit::Quit,
        Some(LinkEnd::Ended { saved: false }) => Exit::Forced {
            save_waiting: saving,
        },
        Some(LinkEnd::Lost | LinkEnd::Cut) | None if quitting => Exit::Forced {
            save_waiting: saving,
        },
        Some(LinkEnd::Lost) => Exit::Lost,
        Some(LinkEnd::Cut) => Exit::Cut,
        None => Exit::Ended,
    }
}

fn busy(status: &Status, inputs: &Inputs) -> bool {
    status.is_running() || inputs.due()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reading {
    since: Duration,
    clipboard: bool,
    shown: bool,
    quiet: bool,
}

mod agents;
mod card;
mod chats;
mod config_screen;
mod display;
mod endpoint;
mod gone;
mod idle;
mod opener;
mod pager;
mod plugin;
mod processes;
mod resumed;
mod retry;
mod rewind;
mod select;
mod sidebar;
mod subscription_login;
mod suggest;
#[cfg(test)]
mod tests;
mod usage_page;
mod waiting;

use select::Selecting;
pub use select::{NOTHING_TO_COPY, TOAST, copies_hint, handed_off_line};

pub async fn run<C: Console>(
    console: &mut C,
    inputs: mpsc::UnboundedReceiver<Input>,
    session: Session,
    options: Options,
) -> Result<Exit, AppError> {
    run_to_end(console, inputs, session, options).await.exit
}

pub async fn run_to_end<C: Console>(
    console: &mut C,
    mut inputs: mpsc::UnboundedReceiver<Input>,
    session: Session,
    mut options: Options,
) -> Finished {
    let mut quit_bound = options.quit.take();
    let Session {
        leave_note,
        attached,
        alerts,
        facts,
        mut events,
        commands,
        resumed,
    } = session;
    options.attached = attached.or(options.attached.take());
    let notes = std::mem::take(&mut options.notes);
    let started = options.started.take();
    let theme = options.theme;
    let mut p = match Presenter::new(console, facts, commands, options) {
        Ok(p) => p,
        Err(e) => {
            return Finished {
                unsent: None,
                exit: Err(e),
                goodbye: vec![],
                width: 0,
                theme,
                unshown: notes,
            };
        }
    };
    p.leave_note = leave_note;
    match resumed {
        Some(resumed) => p.show_resumed(resumed, &notes),
        None => {
            p.started_on(started);
            p.open_start_pick();
            p.show_start_notes(&notes);
        }
    }
    if !p.composer.is_empty() {
        p.typed_changed(p.at(Instant::now()));
    }
    for alert in alerts {
        p.view
            .warning(alert.lines().map(wisp_render::text::sanitize).collect());
    }
    let mut unshown = notes;
    let wake = p.code.wake();
    let mut events_open = true;
    let mut inputs_open = true;
    let mut expiry: Option<Expiry> = None;
    let exit = loop {
        if let Err(e) = p.tick(console) {
            break Err(e);
        }
        unshown.clear();
        if let Some(exit) = p.exit.take() {
            break Ok(exit);
        }
        if p.quitting && !events_open {
            break Ok(p.quit_exit());
        }
        if p.quitting
            && expiry.is_none()
            && let Some(bound) = quit_bound.take()
        {
            expiry = Some(Box::pin(bound.expired()));
        }
        let mut expired = false;
        let wake_at = p.next_wake(Instant::now());
        let sleep = async move {
            match wake_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        let step = tokio::select! {
            biased;
            input = inputs.recv(), if inputs_open => match input {
                Some(input) => p.input(input, console),
                None => {
                    inputs_open = false;
                    if !p.quitting {
                        p.quit();
                    }
                    Ok(())
                }
            },
            event = events.recv(), if events_open => {
                match event {
                    Some(event) => p.event(&event),
                    None => {
                        events_open = false;
                        p.events_closed();
                    }
                }
                Ok(())
            },
            () = wake.notified() => {
                p.frames.mark();
                Ok(())
            },
            () = sleep => Ok(()),
            () = async {
                match expiry.as_mut() {
                    Some(expiry) => expiry.await,
                    None => std::future::pending().await,
                }
            } => {
                expired = true;
                Ok(())
            },
        };
        if let Err(e) = step {
            break Err(e);
        }
        if expired {
            break Ok(p.forced_exit());
        }
    };
    let exit = match exit {
        Err(AppError::Io(e)) => match p.input_stopped.take() {
            Some(reason) => Ok(Exit::InputStopped(reason)),
            None if console.hung_up() => Ok(Exit::InputStopped(HUNG_UP.to_owned())),
            None => Err(AppError::Io(e)),
        },
        other => other,
    };
    let away = matches!(
        exit,
        Ok(Exit::Taken | Exit::Lost | Exit::Cut | Exit::Restarting)
    );
    let ended_outside = !p.quitting && matches!(exit, Ok(Exit::Quit | Exit::Forced { .. }));
    let mut goodbye = match exit {
        Ok(Exit::Resume(_)) => vec![],
        Ok(Exit::Leave) => p.summary().leave(p.chrome_layout()),
        Ok(Exit::InputStopped(_)) if p.host_ended.is_some() => vec![],
        _ if away => {
            let mut lines = Vec::new();
            if matches!(exit, Ok(Exit::Lost | Exit::Cut))
                && let Some(path) = &p.host_error
            {
                lines.extend(
                    host_errors(path)
                        .into_iter()
                        .map(|line| StyledLine::new(vec![StyledSpan::plain(line)])),
                );
            }
            let away = match exit {
                Ok(Exit::Taken) => Away::Taken,
                _ => Away::Lost,
            };
            lines.extend(p.summary().away(away, p.chrome_layout()));
            lines
        }
        _ => p.summary().goodbye(p.chrome_layout()),
    };
    let rows = p.processes.rows(p.at(Instant::now()));
    if p.host_ended.is_none() || matches!(exit, Ok(Exit::Quit | Exit::Forced { .. })) {
        add_kept(&mut goodbye, &rows, p.chrome_layout());
    }
    p.cancel_edit();
    let unsent = (away || ended_outside).then_some(Unsent {
        composer: p.composer,
        card: p.card.or(p.card_back),
        returned: p.returned,
        unconfirmed: p.inputs.take_sent(),
        late: None,
        resend: p.resend,
        children: p.agent_models.unsent(),
    });
    Finished {
        unsent,
        exit,
        goodbye,
        width: usize::from(p.size.width),
        theme: p.theme,
        unshown,
    }
}

impl Presenter {
    pub(super) fn new<C: Console>(
        console: &mut C,
        mut facts: SessionFacts,
        commands: mpsc::Sender<ClientCommand>,
        options: Options,
    ) -> Result<Presenter, AppError> {
        let size = console.size()?;
        let config = Config {
            provider: facts.provider.clone(),
            cache_ttl: facts.cache_ttl,
            logging: facts.log.is_some(),
        };
        let view = TranscriptView {
            detail: if options.verbose {
                Detail::Full
            } else {
                Detail::Scrollback
            },
            bands: options.theme.fills(Token::Surface),
            mouse: options.mouse,
        };
        let child_zone = options.zone.clone();
        let mut agent_models = super::agents::Models::new(config.clone(), facts.home.clone())
            .with_reset_time(move |at| super::reset_time::absolute(&child_zone, at));
        agent_models.set_view(view);
        let reset_zone = options.zone.clone();
        let transcript = Transcript::new(config)
            .with_reset_time(move |at| super::reset_time::absolute(&reset_zone, at))
            .with_home(facts.home.clone())
            .with_code(Highlighter::pending().key())
            .with_view(view);
        let cx = MotionCtx::new(options.motion, options.glyphs, options.theme.depth());
        let code = Highlights::start(options.code);
        let files = Loader::new(PAGER_BUDGET, code.wake());
        let (tracker, requests, host_end, host_ended, host_error) = match options.attached {
            Some(attached) => (
                match attached.now {
                    Some(now) => Tracker::at(&mut facts, now),
                    None => Tracker::new(&facts),
                },
                attached.first_request.0.saturating_sub(1),
                Some(attached.end),
                Some(attached.ended),
                Some(attached.error_file),
            ),
            None => (Tracker::new(&facts), 0, None, None, None),
        };
        facts.effort = display_effort(&facts, &facts.model, facts.effort_level())
            .map_or_else(String::new, |e| e.as_str().to_owned());
        let select = Selecting::new(options.clipboard, options.copy_on_release, code.wake());
        let images = Reader::new(options.images, code.wake());
        let Unsent {
            mut composer,
            card,
            returned,
            unconfirmed,
            late,
            resend,
            children,
        } = options.unsent.unwrap_or_default();
        agent_models.hold(children.into_iter().map(Child::kept));
        composer.restore_ahead(
            &resend
                .into_iter()
                .chain(unconfirmed.into_iter().map(|(_, saved)| saved))
                .collect::<Vec<_>>(),
        );
        let mut p = Presenter {
            transcript,
            view: View::new(usize::from(size.width)),
            composer,
            returned,
            late,
            resend: Vec::new(),
            plan_usage: BTreeMap::new(),
            usage_page: None,
            usage_endpoint: None,
            usage_catalog: false,
            usage_initial: None,
            usage_refreshes: BTreeMap::new(),
            spend_answer: None,
            spend_request: None,
            spend_again: false,
            refused_keys: BTreeSet::new(),
            claude_usage: facts.claude_usage,
            status: Status::new()
                .with_seed(options.verb_seed)
                .with_tail(super::settings::ttl_of(facts.cache_ttl)),
            pacing: Pacing::new(cx),
            banner_done: false,
            chrome_due: None,
            idle_due: None,
            seats: vec![],
            chip_at: None,
            frames: Frames::default(),
            painter: Painter::new(options.full_repaint),
            pictures: options.pictures,
            pictures_lost: false,
            code,
            shift_enter: options.shift_enter,
            mouse: options.mouse,
            select_key: options.select_key,
            right_paste: options.right_paste,
            terminal_pastes_on_right_click: options.terminal_pastes_on_right_click,
            terminal_paste_until: None,
            suspended: false,
            input_stopped: None,
            commands: Some(commands),
            facts,
            theme: options.theme,
            glyphs: options.glyphs,
            start: Instant::now(),
            cancel: false,
            quitting: false,
            quit_note: None,
            armed: false,
            restores: 0,
            exit: None,
            pager: None,
            pager_rows: RowCounts::default(),
            pager_welcome: vec![],
            files,
            size,
            resize_at: None,
            hint_area: None,
            scroll_speed: options.scroll_speed.max(1),
            turns: 0,
            stopped: None,
            picker: None,
            account_describing: None,
            opener: None,
            opener_hits: Vec::new(),
            tracker,
            select,
            inputs: Inputs::new(),
            images,
            reading: None,
            card: None,
            card_back: card,
            card_settle: super::question::Settle::default(),
            card_hits: Vec::new(),
            card_seen: false,
            step_in_seen: false,
            secret_field: crate::widgets::secret::SecretField::new(
                crate::widgets::secret::Rule::Any,
            ),
            secret_hidden: false,
            secret_sent: false,
            secret_problem: String::new(),
            secret_drop: None,
            step_pick: Pick {
                rows: copy::YES_NO.len(),
                ..Pick::default()
            },
            last_typed: None,
            bell: false,
            requests,
            host_end,
            leave_note: None,
            host_quitting: false,
            host_ended,
            host_error,
            suggest: Suggest::default(),
            listed: None,
            adding: endpoint::Adding::new(options.card_setup, options.file_asked),
            authentication: subscription_login::Authentication::default(),
            continuing: None,
            retry: super::retry::Choice::default(),
            settings_screen: None,
            settings_list: None,
            settings_draft: None,
            settings_later: std::collections::BTreeSet::new(),
            adding_model: false,
            display: options.display,
            kept: options.kept,
            modes_due: None,
            rewind: None,
            chats: None,
            chats_refused: false,
            resume_to: None,
            rewind_primed: None,
            rewind_sent: None,
            rewind_notes: Vec::new(),
            rewind_hits: Vec::new(),
            zone: options.zone,
            now: options.now,
            processes: super::processes::Processes::default(),
            ps: None,
            movable: None,
            list_on_start: options.open_processes,
            step_settle: super::question::Settle::default(),
            prompt_dropped: None,
            warned: false,
            attach_pending: false,
            attach_sent: None,
            composer_keys: true,
            agents: super::agents::Agents::default(),
            agents_list: None,
            chat: None,
            agent_models,
            main_since: None,
            exit_armed: false,
            sidebar: super::sidebar::Side::new(options.sidebar, usize::from(size.width)),
            panel_area: None,
            painted: Vec::new(),
            chat_area: Rect::default(),
            sidebar_click: None,
            plugin_runs: BTreeMap::new(),
            terminal_sent: None,
        };
        if let Some(caps) = p.kept.as_ref().map(super::display::Kept::resolve) {
            p.sidebar
                .set_lists(caps.sidebar_folded.value, caps.sidebar_pinned.value);
        }
        p.inputs.seed_ids(p.returned.iter().map(InputId::as_str));
        p.replay(&options.typed);
        if options.list_agents {
            p.ask_agents();
        }
        p.subscribe_usage(Duration::ZERO);
        p.sync_terminal();
        p.frames.mark();
        Ok(p)
    }

    fn summary(&self) -> Summary {
        Summary {
            turns: self.turns,
            stopped: self.stopped.unwrap_or_else(|| self.agents_busy()),
            duration_ms: u64::try_from(self.at(Instant::now()).as_millis()).unwrap_or(u64::MAX),
            total: wisp_render::chrome::shown_total(
                self.agents.main_spend(),
                self.agents.rows(),
                ChatTurn::Idle,
            ),
            session: self.session_id(),
        }
    }

    fn at(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.start)
    }

    fn instant(&self, at: Duration) -> Option<Instant> {
        self.start.checked_add(at)
    }

    fn cx(&self) -> MotionCtx {
        let cx = self.pacing.ctx();
        if self.painter.always_full() {
            return MotionCtx {
                policy: MotionPolicy::Reduced,
                ..cx
            };
        }
        cx
    }

    fn typed_changed(&mut self, at: Duration) {
        let cx = self.cx();
        let parts = self.shown_parts();
        parts.status.composer(parts.composer.is_empty(), at, cx);
    }

    fn hint(&mut self, text: &str, at: Duration) {
        self.shown_status_mut().hint(text, at, None);
    }

    fn chat_model(&self) -> Option<&super::agents::Model> {
        self.agent_models.get(&self.chat.as_ref()?.path)
    }

    fn chat_model_mut(&mut self) -> Option<&mut super::agents::Model> {
        self.agent_models.get_mut(&self.chat.as_ref()?.path)
    }

    fn shown_status(&self) -> &Status {
        if self.status.is_restarting() {
            return &self.status;
        }
        self.chat_model()
            .map_or(&self.status, |model| &model.status)
    }

    fn shown_status_mut(&mut self) -> &mut Status {
        if self.status.is_restarting() {
            return &mut self.status;
        }
        self.shown_parts().status
    }

    fn shown_composer(&self) -> &Composer {
        self.chat_model()
            .map_or(&self.composer, |model| &model.composer)
    }

    fn shown_composer_mut(&mut self) -> &mut Composer {
        self.shown_parts().composer
    }

    fn shown_view(&self) -> &View {
        self.chat_model().map_or(&self.view, |model| &model.view)
    }

    fn shown_view_mut(&mut self) -> &mut View {
        self.shown_parts().view
    }

    fn shown_parts(&mut self) -> Parts<'_> {
        let agent = self.chat.as_ref().map(|chat| chat.path.clone());
        self.parts_mut(agent.as_ref())
    }

    fn parts_mut(&mut self, agent: Option<&AgentPath>) -> Parts<'_> {
        match agent.and_then(|agent| self.agent_models.get_mut(agent)) {
            Some(m) => Parts {
                transcript: &mut m.transcript,
                inputs: &mut m.inputs,
                composer: &mut m.composer,
                status: &mut m.status,
                view: &mut m.view,
                tracker: &mut m.tracker,
                runs: &mut m.runs,
            },
            None => Parts {
                transcript: &mut self.transcript,
                inputs: &mut self.inputs,
                composer: &mut self.composer,
                status: &mut self.status,
                view: &mut self.view,
                tracker: &mut self.tracker,
                runs: &mut self.plugin_runs,
            },
        }
    }

    fn pair_for(&self, agent: Option<&AgentPath>) -> (Cow<'_, SessionFacts>, &Tracker) {
        let found =
            agent.and_then(|a| Some((&self.agents.row(a)?.entry, self.agent_models.get(a)?)));
        let Some((entry, model)) = found else {
            return (Cow::Borrowed(&self.facts), &self.tracker);
        };
        let mut facts = agent_facts(&self.facts, entry, model.listed.as_deref());
        model.tracker.show(&mut facts);
        (Cow::Owned(facts), &model.tracker)
    }

    fn chat_pair(&self) -> (Cow<'_, SessionFacts>, &Tracker) {
        let chat = self.chat.as_ref().map(|chat| &chat.path);
        let (mut facts, tracker) = self.pair_for(chat);
        let unlisted = chat.and_then(|a| Some((self.agents.row(a)?, self.agent_models.get(a)?)));
        if let Some((row, _)) = unlisted.filter(|(_, model)| model.listed.is_none()) {
            let own = |e: &EndpointFacts| row.entry.endpoint.as_ref() == Some(&e.name);
            facts.to_mut().endpoints.retain(own);
        }
        (facts, tracker)
    }

    fn chat_models(&self) -> Picker {
        Picker::models().opened(self.chat_pair().1.wanted().endpoint)
    }

    fn aimed(&mut self, command: ClientCommand, at: Duration) -> bool {
        self.send_to(&self.here(), command, at)
    }

    fn disarm(&mut self) {
        if self.rewind_primed.take().is_some() {
            self.frames.mark();
        }
        if self.armed {
            self.armed = false;
            self.status.clear_hint();
        }
    }

    fn picker_open(&self) -> bool {
        self.picker.is_some() || self.opener_open()
    }

    fn prompt_open(&self) -> bool {
        self.docked()
            || self.adding.open()
            || self.auth_open()
            || self.ps_open()
            || self.agents_open()
            || self.usage_page.is_some()
    }

    fn overlay_open(&self) -> bool {
        self.prompt_open() || self.card.is_some() || self.step_in_shown()
    }

    fn busy(&self) -> bool {
        busy(&self.status, &self.inputs)
    }

    fn top_row(&self, chrome: Layout) -> StyledLine {
        let line = self.agent_line(frame_inner(chrome)).unwrap_or_default();
        frame_row(line, chrome)
    }

    fn waiting_lines(&self, view: u16, chrome: Layout) -> Vec<StyledLine> {
        let short = usize::from(self.size.height) < TOP_ROW_FROM_ROWS;
        let chat = self.chat_model();
        if short || (self.overlay_open() && chat.is_none()) || self.suggesting(view) {
            return vec![];
        }
        let boxed = boxed_at(self.size);
        let inner = if boxed {
            Layout {
                width: frame_inner(chrome).width.saturating_sub(1),
                ..chrome
            }
        } else {
            chrome
        };
        let rows = chat.map_or(&self.inputs, |m| &m.inputs).rows(self.glyphs);
        let room = usize::from(view).saturating_sub(VIEW_MIN_ROWS);
        let mut lines = waiting_rows(&rows, room, inner);
        if chat.is_none() {
            let left = room.saturating_sub(lines.len());
            lines.extend(rewind_notes(&self.rewind_notes, left, inner));
            let left = room.saturating_sub(lines.len());
            lines.extend(self.mail_line(inner).into_iter().take(left));
        }
        if !boxed {
            return lines;
        }
        lines.into_iter().map(|l| frame_row(l, chrome)).collect()
    }

    fn chat_keys(&self, at: Duration) -> Vec<KeyHint> {
        let Some((chat, model)) = self.chat.as_ref().zip(self.chat_model()) else {
            return vec![];
        };
        if self.processes.attached().is_some() {
            return self.take_over_keys();
        }
        let needs_you = copy::main_needs_you();
        let waits = self.card.is_some() || self.step_in_shown();
        let shown = self.shown_status().shown_hint(at) == Some(needs_you.as_str());
        let mut keys: Vec<KeyHint> = (waits && !shown)
            .then(|| KeyHint::note(needs_you))
            .into_iter()
            .collect();
        keys.extend(self.take_over_keys());
        keys.push(copy::esc(copy::BACK_WORD));
        let busy = self.agent_models.running_turn(&chat.path).is_some();
        let tab = model.composer.tab_queues();
        let waiting = model.inputs.waiting() > 0;
        let held = self.agents.row(&chat.path).is_some_and(releasable);
        keys.extend(waiting::box_keys(busy, tab, waiting, held, None));
        keys
    }

    fn keys_hint(&self, at: Duration) -> Vec<KeyHint> {
        let waiting = self.inputs.waiting();
        if self.chat.is_some() {
            self.chat_keys(at)
        } else if self.usage_page.is_some() && self.status.is_running() {
            vec![copy::to_stop(copy::CTRL_C_KEY).as_exit()]
        } else if self.rewind_primed(at) {
            vec![copy::again_to_rewind()]
        } else if waiting > 0 && self.overlay_open() {
            vec![KeyHint::note(copy::waiting_count(waiting))]
        } else if self.card.is_some() {
            vec![]
        } else if self.overlay_holds() {
            self.overlay_keys()
        } else {
            let mut keys = self.waiting_hint();
            keys.extend(self.process_keys());
            keys
        }
    }

    fn replay(&mut self, bytes: &[u8]) {
        for input in typed(bytes) {
            if let Input::Key(key) = input {
                self.composer.edit(key);
            }
        }
        let at = self.at(Instant::now());
        self.typed_changed(at);
    }

    fn layout(&self) -> Layout {
        Layout::new(self.view.width(), self.glyphs)
    }

    fn chrome_layout(&self) -> Layout {
        Layout::new(usize::from(self.size.width), self.glyphs)
    }

    fn composer_shape(&self) -> ComposerShape {
        ComposerShape::on_screen(self.chrome_layout(), usize::from(self.size.height))
            .with_keys(self.composer_keys)
    }

    fn prompt_has_keys(&self, at: Duration) -> bool {
        self.prompt_open() || self.card_holds(at) || self.step_in_holds(at) || self.overlay_holds()
    }

    fn chat_overlay(&self) -> bool {
        self.dock_open(retry::Docked::Picker)
            || self.dock_open(retry::Docked::Rewind)
            || self.ps_open()
    }

    fn input_folds(&self, at: Duration) -> bool {
        let overlay = match self.chat {
            Some(_) => self.chat_overlay(),
            None => self.prompt_has_keys(at),
        };
        overlay && self.shown_composer().is_blank()
    }

    fn composer_has_keys(&self, at: Duration) -> bool {
        match self.chat {
            Some(_) => !self.chat_overlay(),
            None => {
                !(self.prompt_has_keys(at) || self.retry_holds() || self.inputs.focused().is_some())
            }
        }
    }

    pub(super) fn next_wake(&self, now: Instant) -> Option<Instant> {
        let mut wake: Option<Instant> = None;
        let mut take = |at: Option<Instant>| {
            if let Some(at) = at {
                wake = Some(wake.map_or(at, |w| w.min(at)));
            }
        };
        if self.cancel || self.bell {
            take(Some(now));
        }
        if self.suspended {
            return wake;
        }
        let at = self.at(now);
        take(
            self.frames
                .due(at, self.painter.pace().rate())
                .and_then(|due| self.instant(due)),
        );
        let wall = (self.now)().as_second();
        if self.usage_page.is_some() || !self.plan_usage.is_empty() {
            take(now.checked_add(Duration::from_secs(
                u64::try_from(60 - wall.rem_euclid(60)).unwrap_or(60),
            )));
        }
        for snapshot in self
            .plan_usage
            .iter()
            .filter(|(endpoint, _)| {
                self.usage_page.is_some() || *endpoint == &self.tracker.session().endpoint
            })
            .filter_map(|(_, state)| state.snapshot.as_ref())
        {
            for window in &snapshot.windows {
                for deadline in [
                    window
                        .observed_at
                        .checked_add(wisp_protocol::plan_usage::USAGE_MAX_AGE),
                    window.resets_at,
                ]
                .into_iter()
                .flatten()
                .filter(|at| *at > wall)
                {
                    if let Ok(seconds) = u64::try_from(deadline - wall) {
                        take(now.checked_add(Duration::from_secs(seconds)));
                    }
                }
            }
        }
        if let Some(reset) = self
            .retry
            .failed()
            .and_then(|f| f.resets_at)
            .filter(|r| *r > wall)
            && let Ok(seconds) = u64::try_from(reset - wall)
        {
            take(now.checked_add(Duration::from_secs(seconds)));
        }
        let ahead = |at: Instant| at > now;
        take(self.resize_at);
        take(self.auth_deadline().and_then(|due| self.instant(due)));
        take(self.select.edge_due().and_then(|due| self.instant(due)));
        take(self.select.open_due().and_then(|due| self.instant(due)));
        take(self.select.toast_due().and_then(|due| self.instant(due)));
        take(self.reading_due().and_then(|due| self.instant(due)));
        if let Some(pager) = &self.pager {
            take(pager.wake(now, self.frames.is_dirty() || self.select.handed_off()));
            return wake;
        }
        if self.shown_view().sweeping() {
            take(Some(now));
        }
        take(
            self.transcript
                .next_release()
                .and_then(|timer| self.instant(timer))
                .filter(|&at| ahead(at)),
        );
        for timer in [self.chrome_due, self.idle_due].into_iter().flatten() {
            take(self.instant(timer));
        }
        wake
    }

    pub(super) fn tick<C: Console>(&mut self, console: &mut C) -> Result<(), AppError> {
        let now = Instant::now();
        let at = self.at(now);
        if self.cancel
            && let Some(commands) = &self.commands
        {
            match commands.try_send(ClientCommand::Cancel) {
                Ok(()) => self.cancel = false,
                Err(mpsc::error::TrySendError::Closed(_)) => self.cancel = false,
                Err(mpsc::error::TrySendError::Full(_)) => {}
            }
        }
        if self.quitting && !self.cancel {
            if let Some(end) = &self.host_end
                && !self.host_quitting
            {
                self.host_quitting = end.try_send(false).is_ok();
            }
            self.commands = None;
        }
        if self.quitting {
            self.quit_status(at);
        }
        self.set_modes(console)?;
        self.auth_copy(console, at)?;
        if self.suspended {
            return Ok(());
        }
        self.ring(console)?;
        if self.resize_at.is_some_and(|due| due <= now) {
            self.settle_resize();
        }
        self.auth_tick(at);
        self.usage_status();
        self.refresh_usage_page();
        self.take_copies(at);
        self.take_images(at);
        self.sync_choice(at);
        self.edge_tick(at);
        self.open_tick(at);
        if self.transcript.next_release().is_some_and(|r| r <= at)
            || self.chrome_due.is_some_and(|c| c <= at)
            || self
                .pager
                .as_ref()
                .and_then(|p| p.rewrap_at)
                .is_some_and(|(_, due)| due <= now)
            || self
                .pager
                .as_ref()
                .and_then(|p| p.reload_at)
                .is_some_and(|due| due <= now)
        {
            self.chrome_due = None;
            self.frames.mark();
        }
        if self.idle_due.is_some_and(|due| due <= at) {
            self.idle_due = None;
            if !self.frames.is_dirty() {
                self.idle_step(console, at)?;
            }
        }
        if self.frames.ready(at, self.painter.pace().rate()) {
            self.draw(console)?;
        } else if self.pager.is_none() && self.shown_view().sweeping() {
            self.slice(now);
        } else if self.search_pending() {
            self.search_slice();
        }
        Ok(())
    }

    fn settle_resize(&mut self) {
        self.resize_at = None;
        let on = self.sections_on();
        let docked = self.sidebar.docked();
        self.sidebar.settle(usize::from(self.size.width), &on);
        let width = self.transcript_width();
        if width != self.view.width() {
            self.view.set_width(width);
            if docked != self.sidebar.docked() {
                self.clear_selection();
            }
        }
        self.painter.invalidate();
        self.frames.mark();
        self.sync_slots(&on);
    }

    fn noticed_size(&mut self, size: Size, now: Instant) {
        if size == self.size || size.width == 0 || size.height == 0 {
            return;
        }
        self.forget_agents_hover();
        self.size = size;
        self.resize_at = Some(now + RESIZE_DEBOUNCE);
        self.painter.invalidate();
        self.frames.mark();
    }

    fn slice(&mut self, now: Instant) {
        let at = self.at(now);
        let mut clock = SliceClock::new(Instant::now().into_std());
        self.with_paint(at, |view, paint| {
            view.slice(paint, || clock.out_of_time(Instant::now().into_std()))
        });
    }

    fn drain_code(&mut self) {
        while let Some(reply) = self.code.try_recv() {
            match reply {
                Reply::Ready(_) => self.transcript.wait_for_code(true),
                Reply::Failed(_) => self.transcript.code_failed(),
                Reply::Updates { stream, updates } => {
                    for update in updates {
                        self.transcript.apply_code(stream, update);
                    }
                }
            }
        }
    }

    fn feed_code(&mut self) {
        for (stream, feed) in self.transcript.take_code() {
            self.code.send(stream, feed);
        }
        let live: BTreeSet<usize> = self.transcript.code_streams().into_iter().collect();
        for gone in self.code.streams().difference(&live) {
            self.code.finish(*gone);
        }
    }

    fn banner_rows(&self, at: Duration, layout: Layout) -> Vec<StyledLine> {
        let cx = self.cx();
        let at = if self.banner_done {
            WELCOME_SETTLED
        } else {
            at
        };
        let keys = super::welcome::keys(self.shift_enter, self.facts.endpoints.len());
        welcome(&self.facts, &keys, at, cx, layout)
    }

    pub(super) fn draw<C: Console>(&mut self, console: &mut C) -> Result<(), AppError> {
        if self.suspended {
            return Ok(());
        }
        let now = Instant::now();
        let at = self.at(now);
        let size = console.size()?;
        self.noticed_size(size, now);
        self.usage_status();
        self.drain_code();
        if self.select.handed_off() {
            self.hide_agents();
            self.set_modes(console)?;
            self.frames.drew(at);
            self.idle_due = None;
            return Ok(());
        }
        self.check_selection_width();
        self.authentication.link = None;
        let paged = self.pager.is_some().then(|| self.pager_view(now));
        let mut shown = Vec::new();
        self.painted.clear();
        self.chat_area = Rect::default();
        self.chip_at = None;
        let view = match paged {
            Some(view) if self.pager.is_some() => {
                self.hide_agents();
                view
            }
            _ if self.settings_screen.is_some() => {
                self.hide_agents();
                Some(self.settings_view(at))
            }
            _ => {
                self.card_seen = self.card_holds(at);
                self.step_in_seen = self.step_in_holds(at);
                let view = self.main_view(at);
                shown = self.panel_pictures();
                shown.extend(self.agents_pictures());
                Some(view)
            }
        };
        self.frames.drew(at);
        self.idle_due = self.idle_next(at);
        if let Some(view) = view {
            self.paint(console, &view, &shown)?;
        }
        self.set_modes(console)
    }

    fn paint<C: Console>(
        &mut self,
        console: &mut C,
        view: &Buffer,
        shown: &[sidebar::Shown],
    ) -> Result<(), AppError> {
        let broken = std::cell::Cell::new(false);
        let theme = self.theme;
        let (method, cell) = self.pictures.unzip();
        let sixel = method == Some(Pictures::Sixel);
        let page = self.agents_list.as_ref().is_some_and(|l| l.shows_picture());
        let ground = match self.panel_area {
            Some(area) if !page => area.ground,
            _ => theme.rgb(Token::Bg),
        };
        let behind = sixel.then_some(ground);
        let palette = behind.map(|bg| {
            let ramps: Vec<_> = shown
                .iter()
                .flat_map(|(_, s, _)| ramps(s, &theme, bg))
                .collect();
            picture::palette(bg, &ramps)
        });
        let pixels = |i: usize| {
            let (_, scene, rect) = shown.get(i)?;
            let cell = cell?;
            let drawn = contain(|| raster(scene, &theme, cell, *rect, behind));
            broken.set(broken.get() || drawn.is_err());
            drawn.ok()
        };
        let tiles = Tiles {
            list: shown.iter().map(|(tile, _, _)| *tile).collect(),
            cell: cell.unwrap_or_default(),
            encoder: palette.as_deref().map_or(Encoder::Kitty, Encoder::Sixel),
            pixels: &pixels,
        };
        let link = self.authentication.link.as_ref();
        let plan = self.painter.plan_tiles(view, link, &tiles);
        if broken.get() {
            self.lose_pictures();
        }
        let plan = match plan {
            Ok(Some(plan)) => plan,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        self.send_plan(console, plan, view.area.as_size())
    }

    pub(super) fn lose_pictures(&mut self) {
        self.pictures = None;
        self.pictures_lost = true;
        self.frames.mark();
    }

    fn send_plan<C: Console>(
        &mut self,
        console: &mut C,
        plan: Plan,
        size: Size,
    ) -> Result<(), AppError> {
        let started = Instant::now();
        match console.frame(plan.bytes(), size) {
            Ok(Written::Sent) => {
                self.painter.sent(plan, started.elapsed());
                if let Some(pager) = &mut self.pager {
                    pager.painted(size);
                }
                Ok(())
            }
            Ok(Written::Suspended) => Ok(()),
            Ok(Written::Resized(size)) => {
                self.noticed_size(size, Instant::now());
                Ok(())
            }
            Ok(Written::Refused(halted)) => self.halt(console, halted.into()),
            Err(e) => self.halt(console, e),
        }
    }

    fn halt<C: Console>(&mut self, console: &mut C, e: io::Error) -> Result<(), AppError> {
        match halted_of(&e) {
            Some(Halted::Restored) => self.recover(console, false),
            Some(Halted::Dying) => {
                self.exit = Some(Exit::Dying);
                Ok(())
            }
            None => Err(e.into()),
        }
    }

    fn waits(&self, orb: OrbFrame) -> Wait {
        let mail = self.inputs.mail_waking() && self.agents.mail_hold().is_none();
        if self.status.is_running() || mail {
            Wait::Waiting(orb)
        } else if self.agents_busy() > 0 {
            Wait::Reports(orb)
        } else {
            Wait::Off
        }
    }

    fn main_layout(&mut self, at: Duration) -> MainLayout {
        let chrome = self.chrome_layout();
        self.forget_stale_draft();
        self.composer_keys = self.composer_has_keys(at);
        let mut shape = self.composer_shape();
        let mut composer = self.shown_composer_mut().view(shape);
        let mut rows = self.rows_with(composer.rows.len(), self.input_folds(at));
        let cramped = self.chats.is_some() && !chats_fit(dock_room(usize::from(rows.view)));
        if cramped || (usize::from(rows.view) < PICKER_MIN_ROWS && self.docked()) {
            self.hold_hidden(at);
            for dock in retry::Docked::ALL {
                self.close_dock(dock);
            }
            self.composer_keys = self.composer_has_keys(at);
            shape = self.composer_shape();
            composer = self.shown_composer_mut().view(shape);
            rows = self.rows_with(composer.rows.len(), self.input_folds(at));
        }
        let waiting = self.waiting_lines(rows.view, chrome);
        let waiting_h = u16::try_from(waiting.len()).unwrap_or(0);
        rows.view = rows.view.saturating_sub(waiting_h);
        let above = rows.view;
        self.refresh_opener();
        let dock = self.dock_lines(rows.view, chrome, at);
        let dock_h = u16::try_from(dock.len()).unwrap_or(0);
        rows.view = rows.view.saturating_sub(dock_h);
        MainLayout {
            shape,
            composer,
            rows,
            waiting,
            dock,
            above,
        }
    }

    fn layers(&self, room: u16, at: Duration) -> Vec<Layer> {
        let picker = self.picker_open();
        let main = !picker && self.chat.is_none() && self.processes.attached().is_none();
        let chrome = self.chrome_layout();
        let space = usize::from(room);
        [
            retry::Docked::ALL
                .into_iter()
                .find(|&dock| self.dock_open(dock))
                .map(Layer::Dock),
            (room > 0).then_some(Layer::Sidebar),
            (!picker
                && self.usage_page.is_some()
                && !self.auth_open()
                && !self.adding.open()
                && room > 0)
                .then_some(Layer::Usage),
            (main
                && self
                    .card_height(space, at, chrome)
                    .is_some_and(|height| height > 0))
            .then_some(Layer::Question),
            (self.adding.open() && room > 0).then_some(Layer::Endpoint),
            (!picker
                && !(main && self.card.is_some())
                && !self.ps_lines(space, at, chrome).is_empty())
            .then_some(Layer::Processes),
            (!picker && self.agents_shown(space) && self.agents_list.is_some())
                .then_some(Layer::Agents),
            (main
                && self
                    .step_in_lines(!self.step_settle.settled(at), space, chrome)
                    .is_some_and(|lines| !lines.is_empty()))
            .then_some(Layer::StepIn),
            self.suggesting(room).then_some(Layer::Suggestions),
            (self.auth_open() && room > 0).then_some(Layer::Auth),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    fn paint_layer(
        &mut self,
        layer: Layer,
        buffer: &mut Buffer,
        room: u16,
        at: Duration,
        chrome: Layout,
    ) -> Option<Rect> {
        let top = match layer {
            Layer::Dock(_) | Layer::Agents => return None,
            Layer::Sidebar => return self.draw_sidebar(buffer, room, at),
            Layer::Usage => self.draw_usage_page(buffer, room, chrome),
            Layer::Question => self.draw_card(buffer, room, at, chrome),
            Layer::Endpoint => self.draw_endpoint_card(buffer, room, chrome),
            Layer::Processes => self.draw_ps(buffer, room, at, chrome),
            Layer::StepIn => self.draw_step_in(buffer, room, at, chrome),
            Layer::Suggestions => self.draw_suggestions(buffer, room, chrome, at),
            Layer::Auth => self.draw_auth(buffer, room, chrome),
        }?;
        Some(Rect::new(
            0,
            top,
            buffer.area.width,
            room.saturating_sub(top),
        ))
    }

    fn main_view(&mut self, at: Duration) -> Buffer {
        let cx = self.cx();
        let layout = self.layout();
        self.transcript.tick(at, cx);
        self.sync_agent_links();
        let frame = self.transcript.frame_at(&Renderer::new(layout), at);
        self.feed_code();
        self.view.sync(self.transcript.cells().len());
        let width = self.view.width();
        if let Some(model) = self.chat_model_mut() {
            model.transcript.tick(at, cx);
            model.view.set_width(width);
            model.view.sync(model.transcript.cells().len());
        }
        if !self.banner_done && welcome_next_change(at, cx, layout).is_none() {
            self.banner_done = true;
        }
        let caret = frame
            .prose
            .and(self.transcript.live_cell())
            .filter(|_| self.chat.is_none())
            .filter(|&cell| {
                self.transcript
                    .cells()
                    .get(cell)
                    .is_some_and(|c| c.kind() == CellKind::Prose)
            })
            .map(|cell| Caret {
                cell,
                core: self.status.core(at, cx),
                cx,
            });
        let size = self.size;
        let chrome = self.chrome_layout();
        let MainLayout {
            shape,
            composer,
            rows,
            waiting,
            dock,
            above,
        } = self.main_layout(at);
        let layers = self.layers(rows.view, at);
        let view_rows = rows
            .view
            .saturating_sub(self.agent_card_rows(rows.view, at, chrome));
        let head = self.chat_head(view_rows);
        let below = view_rows.saturating_sub(u16::try_from(head.len()).unwrap_or(0));
        let wait = if caret.is_some() || self.status.state(at, cx) == State::Asking {
            Wait::Taken
        } else if self.chat.is_none() && rows.status {
            self.waits(self.status.orb(at, cx))
        } else {
            Wait::Off
        };
        self.view.set_wait(wait);
        let shown = self.with_paint(at, |view, paint| {
            paint.caret = caret;
            view.layout(usize::from(below), paint)
        });
        let theme = self.theme;
        let mut buffer = Buffer::empty(Rect::new(0, 0, size.width, size.height));
        let mut y = 0u16;
        let transcript = if self.panel_docked() {
            u16::try_from(self.transcript_width()).unwrap_or(size.width)
        } else {
            size.width
        };
        let following = shown.following;
        let shown_end = u16::try_from(head.len() + shown.rows.len())
            .unwrap_or(u16::MAX)
            .min(view_rows);
        for line in head.iter().chain(&shown.rows).take(usize::from(view_rows)) {
            buffer.set_line(0, y, &theme.line(line), transcript);
            y += 1;
        }
        self.chat_area = Rect::new(0, 0, transcript, y);
        y = rows.view;
        let (over, inside) = if self.chats.is_some() {
            (&[][..], &dock[..])
        } else {
            (&dock[..], &[][..])
        };
        let mut frame = over.to_vec();
        let mut dock_rows = 0..over.len();
        if rows.top_row {
            frame.push(composer_edge(shape, Edge::Top));
            frame.push(self.top_row(chrome));
        }
        frame.extend(waiting);
        if !inside.is_empty() {
            dock_rows = frame.len()..frame.len() + inside.len();
        }
        frame.extend_from_slice(inside);
        if rows.top_edge {
            let edge = if rows.top_row {
                Edge::Divider
            } else {
                Edge::Top
            };
            frame.push(composer_edge(shape, edge));
        }
        for line in &frame {
            buffer.set_line(0, y, &theme.line(line), size.width);
            y += 1;
        }
        if !dock_rows.is_empty()
            && let Some(&layer @ Layer::Dock(_)) = layers.first()
            && let (Ok(start), Ok(height)) = (
                u16::try_from(dock_rows.start),
                u16::try_from(dock_rows.len()),
            )
        {
            self.painted.push((
                layer,
                Rect::new(0, rows.view.saturating_add(start), size.width, height),
            ));
        }
        let shown_rows = usize::from(rows.composer);
        let first = composer
            .cursor
            .0
            .saturating_sub(shown_rows.saturating_sub(1))
            .min(composer.rows.len().saturating_sub(shown_rows));
        let composer_top = y;
        let origins: Vec<Origin> = head.iter().map(|_| None).chain(shown.origins).collect();
        if self.chat.is_none() && !origins.iter().any(|o| matches!(o, Some((0, _)))) {
            self.banner_done = true;
        }
        self.seen_main(
            origins.clone(),
            view_rows,
            (composer_top, rows.composer, first, composer.clone()),
            rows.top_edge,
        );
        self.paint_selection(&mut buffer, (0, view_rows), None);
        self.sync_selection_hint(at);
        self.hint_area = None;
        let mut covered = !dock.is_empty();
        let room = dock_room(usize::from(above));
        self.rewind_hits = card::row_hits(rows.view, self.rewind_view(room, chrome).hits);
        self.opener_hits = card::row_hits(rows.view, self.opener_view(room, chrome).hits);
        if let Some(opener) = &mut self.opener {
            opener.set_page(self.opener_hits.len());
        }
        self.panel_area = None;
        if !layers.contains(&Layer::Sidebar) {
            self.sidebar.forget_drawn();
        }
        self.listed = None;
        let mut agents_at = None;
        self.card_hits.clear();
        for layer in layers {
            if layer == Layer::Agents {
                agents_at = Some(self.painted.len());
                continue;
            }
            if let Some(area) = self.paint_layer(layer, &mut buffer, rows.view, at, chrome) {
                self.painted.push((layer, area));
                if layer != Layer::Sidebar {
                    covered = true;
                    self.panel_covered(area.y);
                }
            }
        }
        let visible_agents = agents_at.and_then(|index| {
            let bottom = self.painted[index..]
                .iter()
                .map(|(_, area)| area.y)
                .min()
                .unwrap_or(rows.view);
            self.draw_agents(&mut buffer, rows.view, bottom, at, chrome)
                .map(|area| (index, area))
        });
        if let Some((index, area)) = visible_agents {
            self.painted.insert(index, (Layer::Agents, area));
            covered = true;
            self.panel_covered(area.y);
        } else {
            self.hide_agents();
        }
        let unseen = self.shown_view().unseen();
        let edge = self.main_edge().unwrap_or(size.width);
        if unseen > 0 && rows.view > 0 && !covered {
            let text = new_hint(unseen, self.glyphs);
            let w = u16::try_from(text_width(&text, chrome.width_mode)).unwrap_or(u16::MAX);
            if w + 1 < edge {
                let x = edge - w - 1;
                let line = StyledLine::new(vec![StyledSpan::new(
                    text,
                    Look::fg(Token::Accent).on(Token::Surface),
                )]);
                buffer.set_line(x, rows.view - 1, &theme.line(&line), w);
                self.hint_area = Some(Rect::new(x, rows.view - 1, w, 1));
            }
        }
        for (i, line) in composer
            .rows
            .iter()
            .skip(first)
            .take(shown_rows)
            .enumerate()
        {
            let row = y + u16::try_from(i).unwrap_or(0);
            buffer.set_line(0, row, &theme.line(line), size.width);
        }
        self.paint_input_selection(&mut buffer);
        let caret_at = Position {
            x: u16::try_from(composer.cursor.1).unwrap_or(u16::MAX),
            y: y + u16::try_from(composer.cursor.0.saturating_sub(first)).unwrap_or(0),
        };
        if self.composer_keys
            && let Some(cell) = buffer.cell_mut(caret_at)
        {
            cell.set_style(caret_style(&theme));
        }
        y += rows.composer;
        if rows.bottom_edge {
            let opened = rows.top_row || rows.top_edge || !inside.is_empty();
            let edge = match (rows.framed, opened) {
                (true, true) => Edge::Divider,
                (true, false) => Edge::Top,
                (false, _) => Edge::Bottom,
            };
            buffer.set_line(0, y, &theme.line(&composer_edge(shape, edge)), size.width);
            y += 1;
        }
        self.seats.clear();
        let keys = self.keys_hint(at);
        self.shown_status_mut().keys(&keys);
        let here = self.here();
        let interacting =
            !self.take_over_keys().is_empty() || self.step_in_shown() || self.card.is_some();
        let processes = self
            .processes
            .running()
            .filter(|key| !interacting && key.agent == here)
            .map(|key| key.id)
            .collect();
        self.shown_status_mut().background_processes(processes);
        if rows.status {
            let (facts, _) = self.chat_pair();
            let status_row = |layout| self.shown_status().row(at, cx, &facts, layout);
            let status = if rows.framed {
                frame_row(status_row(frame_inner(chrome)), chrome)
            } else {
                status_row(chrome)
            };
            buffer.set_line(0, y, &theme.line(&status), size.width);
            let x = 1 + u16::from(rows.framed);
            self.seat_orb(&buffer, Position { x, y }, at);
            if !self.seats.is_empty() && shown_end > 0 && following {
                self.seat_orb(
                    &buffer,
                    Position {
                        x: 0,
                        y: shown_end - 1,
                    },
                    at,
                );
            }
            self.chip_at = (self.facts.chip())
                .filter(|_| self.chat.is_none())
                .and_then(|chip| chip_cells(&buffer, y, &chip, &Glyphs::of(self.glyphs)))
                .map(|cols| (y, cols));
            y += 1;
        }
        if rows.framed {
            buffer.set_line(
                0,
                y,
                &theme.line(&composer_edge(shape, Edge::Bottom)),
                size.width,
            );
        }
        self.seat_welcome(&buffer, &origins, layout);
        self.place_idle(&mut buffer, at);
        self.chrome_due = [
            self.shown_status().next_change(at, cx),
            welcome_next_change(at, cx, layout).filter(|_| !self.banner_done),
            self.transcript.next_change(),
            self.card_due(at),
            self.ps_due(at),
            self.agents_due(at),
            self.step_in_due(at),
            self.rewind_primed.filter(|&until| until > at),
            self.sidebar_due(at),
        ]
        .into_iter()
        .flatten()
        .min();
        buffer
    }

    fn recover<C: Console>(&mut self, console: &mut C, counted: bool) -> Result<(), AppError> {
        if counted {
            self.restores = self.restores.saturating_add(1);
            if self.restores > MAX_RESTORES {
                return Err(AppError::Panics(self.restores));
            }
            let at = self.at(Instant::now());
            let text = recovered(self.restores);
            self.hint(&text, at);
        }
        self.reentered();
        console.reapply()?;
        let size = console.size()?;
        self.noticed_size(size, Instant::now());
        self.painter.invalidate();
        self.frames.mark();
        Ok(())
    }

    fn resumed<C: Console>(&mut self, console: &mut C) -> Result<(), AppError> {
        if console.halted().is_some() {
            return self.recover(console, false);
        }
        let size = console.size()?;
        self.noticed_size(size, Instant::now());
        if self.resize_at.is_some() {
            self.settle_resize();
        }
        self.painter.invalidate();
        if let Some(pager) = &mut self.pager {
            pager.invalidate();
        }
        self.frames.mark();
        Ok(())
    }

    pub(super) fn input<C: Console>(
        &mut self,
        input: Input,
        console: &mut C,
    ) -> Result<(), AppError> {
        let now = Instant::now();
        let at = self.at(now);
        if let Input::Mouse(mouse) = input
            && mouse.kind == MouseKind::Move
        {
            if self.agents_hover((mouse.column, mouse.row), at) {
                self.frames.mark();
            }
            return Ok(());
        }
        if matches!(input, Input::Paste(_)) && self.swallows_terminal_paste(at) {
            return Ok(());
        }
        if self.auth_open() {
            let handled = match &input {
                Input::Key(key) => {
                    self.auth_key(*key, at);
                    true
                }
                Input::Paste(text) => {
                    self.auth_paste(text, at);
                    true
                }
                Input::Mouse(mouse) => {
                    self.auth_mouse(*mouse, at);
                    true
                }
                _ => false,
            };
            if handled {
                self.frames.mark();
                return Ok(());
            }
        }
        if matches!(input, Input::Key(_) | Input::Paste(_)) && self.secret_drops_input(at) {
            if let Input::Key(key) = input {
                self.secret_stop_key(key, at);
            }
            self.frames.mark();
            return Ok(());
        }
        if matches!(input, Input::Key(_) | Input::Paste(_) | Input::Resize(_)) {
            self.select.drop_open();
        }
        if matches!(input, Input::Key(_) | Input::Paste(_)) {
            self.settle_click(at);
        }
        self.forget_stale_draft();
        match input {
            Input::Key(key) => {
                self.banner_done = true;
                self.noted_typing(at);
                self.key(key, at, console)?;
            }
            Input::Mouse(mouse) => {
                if mouse.kind == MouseKind::Press {
                    self.banner_done = true;
                }
                self.mouse(mouse, at, console)?;
            }
            Input::Paste(text) => {
                self.noted_typing(at);
                let target = self.paste_target(at);
                if !text.is_empty() {
                    self.paste_text(target, &text, at);
                } else if target.is_composer() {
                    self.status.clear_hint();
                    self.inputs.unfocus();
                    self.attach_image(target, at, true);
                } else if matches!(target, PasteTarget::Layer(Layer::StepIn)) {
                    self.paste_to(target, "", at);
                }
            }
            Input::Resize(size) => {
                self.forget_agents_hover();
                self.noticed_size(size, now);
            }
            Input::PictureRefused => self.lose_pictures(),
            Input::Focus(focused) => {
                if focused && self.painter.has_tiles() {
                    self.painter.invalidate();
                }
                if focused {
                    self.select.focus_gained(at);
                } else {
                    self.select.drag_lost();
                    self.forget_agents_hover();
                }
                self.pacing.focus(focused);
            }
            Input::Resumed => {
                self.suspended = false;
                self.resumed(console)?;
            }
            Input::Restored => self.recover(console, true)?,
            Input::Closed(reason) => {
                self.input_stopped.get_or_insert(reason);
                if !self.quitting {
                    self.quit();
                }
            }
        }
        self.frames.mark();
        Ok(())
    }

    fn paste_target(&mut self, at: Duration) -> PasteTarget {
        if self.quitting || self.select.handed_off() {
            return PasteTarget::Drop;
        }
        if self.processes.attached().is_some() {
            return PasteTarget::Attached;
        }
        if self.pager.is_some() {
            return PasteTarget::Pager;
        }
        if self.settings_screen.is_some() {
            return PasteTarget::Settings;
        }
        match self.input_layer(at) {
            Some(Layer::Question) if !self.card_holds(at) => PasteTarget::Drop,
            Some(Layer::StepIn) if !self.step_in_holds(at) => PasteTarget::Drop,
            Some(Layer::Suggestions) | None if self.chat.is_some() => PasteTarget::Chat,
            Some(Layer::Suggestions) | None => PasteTarget::Composer,
            Some(layer) => PasteTarget::Layer(layer),
        }
    }

    fn paste_to(&mut self, target: PasteTarget, text: &str, at: Duration) {
        match target {
            PasteTarget::Drop => self.hold_hidden(at),
            PasteTarget::Attached => self.attach_paste(text, at),
            PasteTarget::Pager => {
                if let Some(pager) = self.pager.as_mut().filter(|pager| pager.prompting()) {
                    pager.prompt_paste(text);
                }
            }
            PasteTarget::Settings => self.settings_paste(text),
            PasteTarget::Chat if self.step_in_shown() => self.hint(&copy::main_needs_you(), at),
            PasteTarget::Chat => {
                self.shown_composer_mut().paste(text);
                self.typed_changed(at);
            }
            PasteTarget::Composer => {
                self.status.clear_hint();
                self.inputs.unfocus();
                self.composer.paste(text);
                self.suggest_edited();
                self.typed_changed(at);
            }
            PasteTarget::Layer(layer) => match layer {
                Layer::Question => self.card_paste(text),
                Layer::StepIn => self.step_in_paste(text, at),
                Layer::Endpoint => self.endpoint_paste(text),
                Layer::Auth => self.auth_paste(text, at),
                Layer::Dock(retry::Docked::Picker) => {
                    if let Some(picker) = &mut self.picker {
                        picker.paste(text);
                    }
                }
                Layer::Dock(retry::Docked::Chats) => self.chats_paste(text),
                Layer::Dock(retry::Docked::Opener) => self.opener_paste(text),
                _ => {}
            },
        }
    }

    fn paste_text(&mut self, target: PasteTarget, text: &str, at: Duration) {
        if target.is_composer() {
            self.status.clear_hint();
            self.inputs.unfocus();
            if self.paste_paths(text, at) {
                self.typed_changed(at);
                return;
            }
        }
        self.paste_to(target, text, at);
    }

    fn swallows_terminal_paste(&mut self, at: Duration) -> bool {
        self.terminal_paste_until
            .take()
            .is_some_and(|until| at < until)
    }

    fn right_click<C: Console>(&mut self, at: Duration, console: &mut C) -> Result<(), AppError> {
        if self.select.dragging() {
            return Ok(());
        }
        if self.select.showing() {
            if self.terminal_pastes_on_right_click {
                self.terminal_paste_until = Some(at.saturating_add(TERMINAL_PASTE_WINDOW));
            }
            self.disarm();
            return self.copy(console, at, true);
        }
        if self.secret_drops_input(at) {
            return Ok(());
        }
        let target = self.paste_target(at);
        let secret = matches!(target, PasteTarget::Layer(Layer::StepIn))
            && self.secret_view(false).is_some();
        if !self.right_paste || (!secret && !target.is_composer()) {
            if self.right_paste && matches!(target, PasteTarget::Layer(Layer::StepIn)) {
                self.step_in_hint(at);
            }
            return Ok(());
        }
        self.noted_typing(at);
        self.disarm();
        self.status.clear_hint();
        self.inputs.unfocus();
        if self.images.busy() {
            self.hint(copy::STILL_READING_CLIPBOARD, at);
            return Ok(());
        }
        match self.images.start_text() {
            Ok(()) => {
                self.reading = Some(Reading {
                    since: at,
                    clipboard: true,
                    shown: false,
                    quiet: true,
                });
            }
            Err(why) => self.hint(&why, at),
        }
        Ok(())
    }

    fn wheel_rows(&self) -> usize {
        WHEEL_ROWS * usize::from(self.scroll_speed)
    }

    fn drew(&self, layer: Layer) -> bool {
        self.painted.iter().any(|(shown, _)| *shown == layer)
    }

    fn mouse_layer(&self, (column, row): (u16, u16)) -> Option<Layer> {
        let point = Position::new(column, row);
        self.painted
            .iter()
            .rev()
            .find(|(_, area)| area.contains(point))
            .map(|&(layer, _)| layer)
    }

    fn mouse<C: Console>(
        &mut self,
        mouse: Mouse,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        if self.select.handed_off() {
            return Ok(());
        }
        if mouse.kind == MouseKind::RightPress {
            self.select.cancel_fold();
            self.sidebar.cancel_press();
            return self.right_click(at, console);
        }
        let rows = self.wheel_rows();
        if mouse.kind == MouseKind::Press {
            self.sidebar.cancel_press();
            self.disarm();
            if self.docked() {
                self.hold_hidden(at);
            }
            self.select.drop_open();
        }
        let layer = self.mouse_layer((mouse.column, mouse.row));
        let panel = layer == Some(Layer::Sidebar);
        let held = self.overlay_open() || self.listed().is_some();
        if matches!(mouse.kind, MouseKind::WheelUp | MouseKind::WheelDown) {
            self.select.cancel_fold();
            self.sidebar.cancel_press();
            if self.pager.is_none() && self.settings_screen.is_none() && !panel {
                self.opener_wheeled();
            }
        }
        match mouse.kind {
            MouseKind::WheelUp | MouseKind::WheelDown
                if layer == Some(Layer::Usage) || (layer.is_none() && self.drew(Layer::Usage)) =>
            {
                self.usage_page_wheel(mouse.kind == MouseKind::WheelDown, rows);
            }
            MouseKind::WheelUp | MouseKind::WheelDown if self.pager.is_some() => {
                self.pager_wheel(mouse, rows);
            }
            _ if self.settings_screen.is_some() => self.settings_mouse(mouse),
            MouseKind::WheelUp | MouseKind::WheelDown if panel => self.panel_wheel(mouse, rows),
            MouseKind::WheelUp | MouseKind::WheelDown
                if self
                    .chat_area
                    .positions()
                    .all(|point| self.painted.iter().any(|(_, area)| area.contains(point))) => {}
            MouseKind::WheelUp => self.scroll(ViewScroll::HalfUp, Some(rows), at),
            MouseKind::WheelDown => self.scroll(ViewScroll::HalfDown, Some(rows), at),
            MouseKind::Press if layer == Some(Layer::Agents) && self.agents_click(mouse, at) => {}
            MouseKind::Press
                if self.drew(Layer::Dock(retry::Docked::Rewind))
                    && self.rewind_click(mouse, at) => {}
            MouseKind::Press
                if self.drew(Layer::Dock(retry::Docked::Opener)) && self.opener_click(mouse) => {}
            MouseKind::Press
                if layer == Some(Layer::Question) && self.card_click(mouse.row, at) => {}
            MouseKind::Press if self.drew(Layer::Suggestions) && self.suggest_click(mouse, at) => {}
            MouseKind::Press if self.chip_pressed(mouse) => {
                self.picker = None;
                self.select.swallow();
                self.open_accounts(Picker::endpoints(), at);
            }
            MouseKind::Press if self.drew(Layer::Dock(retry::Docked::Picker)) => {
                self.picker = None;
                self.select.swallow();
                self.frames.mark();
            }
            MouseKind::Press if panel => self.panel_press(mouse, at, held),
            MouseKind::Press if layer.is_some() => {
                self.clear_selection();
                self.select.swallow();
            }
            MouseKind::Press | MouseKind::Drag | MouseKind::Release if self.select.swallowing() => {
                match mouse.kind {
                    MouseKind::Release => {
                        self.select.swallowed();
                        self.panel_release(mouse, at);
                    }
                    _ => self.sidebar.moved_to((mouse.column, mouse.row)),
                }
            }
            MouseKind::Press | MouseKind::Drag | MouseKind::Release => {
                return self.pointer(mouse, at, console);
            }
            MouseKind::RightPress | MouseKind::Move => {}
        }
        Ok(())
    }

    fn scroll(&mut self, scroll: ViewScroll, rows: Option<usize>, at: Duration) {
        self.with_paint(at, |view, paint| {
            let step = rows.unwrap_or((view.height() / 2).max(1));
            match scroll {
                ViewScroll::HalfUp => view.scroll_up(step, paint),
                ViewScroll::HalfDown => view.scroll_down(step, paint),
                ViewScroll::Top => view.to_top(),
                ViewScroll::Bottom => view.to_bottom(),
            }
        });
    }

    fn quit_exit(&self) -> Exit {
        match (&self.input_stopped, &self.resume_to) {
            (Some(reason), _) => Exit::InputStopped(reason.clone()),
            (None, Some(id)) => Exit::Resume(id.clone()),
            (None, None) => Exit::Quit,
        }
    }

    fn forced_exit(&self) -> Exit {
        if self.input_stopped.is_none()
            && let Some(end) = &self.host_end
        {
            let _ = end.try_send(true);
        }
        match &self.input_stopped {
            Some(reason) => Exit::InputStopped(reason.clone()),
            None => Exit::Forced {
                save_waiting: self.tracker.saving(),
            },
        }
    }

    fn quit_status(&mut self, at: Duration) {
        let glyphs = Glyphs::of(self.glyphs);
        let running = self.processes.stopping();
        let note = match self.tracker.quit_status() {
            Some(phrase) => Some(copy::ongoing(phrase, &glyphs)),
            None if running > 0 => Some(copy::ongoing(&copy::stopping_processes(running), &glyphs)),
            None => None,
        };
        if note == self.quit_note {
            return;
        }
        if let Some(old) = &self.quit_note
            && self.status.shown_hint(at) == Some(old.as_str())
        {
            self.status.clear_hint();
        }
        if let Some(note) = &note {
            self.status.hint(note, at, None);
        }
        self.quit_note = note;
        self.frames.mark();
    }

    fn quit(&mut self) {
        if self.host_end.is_some() && (self.resume_to.is_some() || self.input_stopped.is_some()) {
            self.exit = Some(self.quit_exit());
            return;
        }
        if self.quitting {
            self.exit = Some(self.forced_exit());
            return;
        }
        self.quitting = true;
        self.stopped = Some(self.agents_busy());
        let at = self.at(Instant::now());
        self.close_agents(at);
        self.auth_close(at);
        if self.status.is_running() {
            self.stop();
        }
    }

    fn stop(&mut self) {
        self.cancel = true;
        let views = self.transcript.views().clone();
        for stream in self.transcript.code_streams() {
            if !views.contains(&stream) {
                self.code.cancel(stream);
            }
        }
    }

    fn input_layer(&mut self, at: Duration) -> Option<Layer> {
        let room = self.main_layout(at).rows.view;
        let layers = self.layers(room, at);
        for (index, &layer) in layers.iter().enumerate().rev() {
            match layer {
                Layer::StepIn | Layer::Question => {
                    if self.docked() {
                        continue;
                    }
                    if layer == Layer::Question && !self.composer.is_blank() {
                        return None;
                    }
                    let settled = match layer {
                        Layer::Question => self.card_settle.settled(at),
                        _ => self.step_settle.settled(at),
                    };
                    let covered = (layer == Layer::Question && self.ps.is_some())
                        || layers[..index].iter().any(|layer| match layer {
                            Layer::Sidebar => self.overlay_holds(),
                            Layer::Dock(_) | Layer::Suggestions => false,
                            _ => true,
                        });
                    return (settled || covered).then_some(layer);
                }
                Layer::Sidebar if !self.overlay_holds() => continue,
                _ => return Some(layer),
            }
        }
        None
    }

    fn layer_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<bool, AppError> {
        match self.input_layer(at) {
            Some(Layer::Question) if self.card_holds(at) => self.card_key(key, at, console)?,
            Some(Layer::StepIn) if self.step_in_holds(at) => self.step_in_key(key, at, console)?,
            Some(Layer::Question | Layer::StepIn) => self.hold_hidden(at),
            Some(Layer::Sidebar) => self.overlay_key(key),
            Some(Layer::Usage) => self.usage_page_key(key, at),
            Some(Layer::Endpoint) => self.endpoint_key(key, at, console)?,
            Some(Layer::Processes) => self.ps_key(key, at),
            Some(Layer::Agents) => self.agents_key(key, at, console)?,
            Some(Layer::Suggestions) => {
                if !self.suggest_key(key, at) {
                    return Ok(false);
                }
            }
            Some(Layer::Auth) => self.auth_key(key, at),
            Some(Layer::Dock(dock)) => match dock {
                retry::Docked::Rewind => self.rewind_key(key, at),
                retry::Docked::Chats => self.chats_key(key, at),
                retry::Docked::Picker => self.picker_key(key, at),
                retry::Docked::Opener => self.opener_key(key),
                retry::Docked::Retry => return Ok(false),
            },
            None => return Ok(false),
        }
        Ok(true)
    }

    fn key<C: Console>(&mut self, key: Key, at: Duration, console: &mut C) -> Result<(), AppError> {
        if self.select.handed_off() {
            return self.handed_off_key(key, console);
        }
        if self.processes.attached().is_some() && !self.quitting {
            self.attach_key(key, at);
            return Ok(());
        }
        if self.pager.is_some() {
            return self.pager_key(key, at, console);
        }
        if self.drops_prompt_key(key, at) {
            return Ok(());
        }
        if self.settings_screen.is_some() {
            return self.settings_key(key, at, console);
        }
        if self.quitting {
            self.close_overlay();
            for dock in retry::Docked::ALL
                .into_iter()
                .filter(|dock| *dock != retry::Docked::Retry)
            {
                self.close_dock(dock);
            }
        } else if self.layer_key(key, at, console)? {
            return Ok(());
        }
        if self.chat.is_some() && !self.quitting {
            return self.agents_key(key, at, console);
        }
        if !self.picker_open() && !self.quitting && self.retry_key(key, at) {
            return Ok(());
        }
        if self.inputs.focused().is_some()
            && !self.quitting
            && matches!(self.row_key(key, at), waiting::Routed::Done)
        {
            self.armed = false;
            self.typed_changed(at);
            return Ok(());
        }
        let (first_row, last_row) = self.composer.row_edges(key, self.composer_shape());
        let cx = Context {
            running: self.busy() || self.quitting,
            commanding: self.commanding(),
            empty: self.composer.is_empty(),
            blank: self.composer.is_blank(),
            armed: self.armed,
            first_row,
            last_row,
            scrolled: !self.shown_view().is_following(),
            selection: self.select.showing(),
            tab_queues: self.composer.tab_queues(),
            held: self.inputs.held(),
            editing: self.inputs.editing().is_some(),
            rows: self.inputs.waiting() > 0,
            browsing: self.composer.is_browsing(),
            primed: self.rewind_primed(at),
            movable: self.movable.is_some(),
            agents: self.agents_busy() > 0,
        };
        let act = action(key, cx);
        self.rewind_primed = None;
        if act == Action::Copy {
            self.disarm();
            return self.copy(console, at, true);
        }
        if self.quitting {
            if matches!(act, Action::Stop | Action::Quit | Action::ArmQuit) {
                self.exit = Some(self.forced_exit());
            }
            return Ok(());
        }
        let was_armed = self.armed;
        self.exit_armed = was_armed;
        self.armed = act == Action::ArmQuit;
        self.status.clear_hint();
        if matches!(
            act,
            Action::Send
                | Action::Steer
                | Action::Queue
                | Action::Release
                | Action::Newline
                | Action::Clear
                | Action::HistoryPrevious
                | Action::HistoryNext
                | Action::EnterRows
                | Action::SaveEdit
                | Action::CancelEdit
                | Action::Edit(_)
                | Action::Deselect
        ) {
            self.clear_selection();
        }
        match act {
            Action::Send | Action::Steer => self.send(DeliveryMode::Steer, at),
            Action::Queue => self.send(DeliveryMode::Queue, at),
            Action::Release => self.release_held(at),
            Action::Newline => self.composer.newline(),
            Action::Stop => {
                let running = self.status.is_running();
                self.stop();
                if running {
                    self.stopped_hint(at);
                }
            }
            Action::Clear => {
                self.rewind_notes.clear();
                self.composer.clear();
                self.suggest.reset();
            }
            Action::ArmQuit => {
                self.rewind_notes.clear();
                let hint = self.quit_hint();
                self.hint(&hint, at);
            }
            Action::Agents => self.open_agents(),
            Action::Quit => self.quit(),
            Action::HistoryPrevious => {
                self.composer.older();
                self.suggest.quiet();
            }
            Action::HistoryNext => {
                self.composer.newer();
                self.suggest.quiet();
            }
            Action::EnterRows => self.enter_rows(),
            Action::SaveEdit => self.save_edit(at),
            Action::CancelEdit => self.cancel_edit(),
            Action::Pager => self.open_opener(),
            Action::Help => self.open_help(Seek::Cell(first_keys(self.suggest.plugins()))),
            Action::Repaint => self.painter.invalidate(),
            Action::Suspend => self.suspend(console)?,
            Action::Scroll(scroll) => self.scroll(scroll, None, at),
            Action::CycleEffort => self.cycle_effort(at),
            Action::AttachImage => {
                let target = self.paste_target(at);
                self.attach_image(target, at, false);
            }
            Action::PrimeRewind => self.prime_rewind(at),
            Action::Rewind => self.open_rewind(at),
            Action::Background => self.move_to_background(at),
            Action::TakeOver => self.take_over_waiting(at),
            Action::Copy | Action::Deselect => {}
            Action::Edit(Key {
                code: code @ (KeyCode::Up | KeyCode::Down),
                ctrl: false,
                alt: false,
                ..
            }) => {
                self.composer
                    .vertical(code == KeyCode::Up, self.composer_shape());
            }
            Action::Edit(key) => {
                if self.composer.edit(key) {
                    self.suggest_edited();
                }
            }
            Action::Nothing => {}
        }
        if was_armed && !self.armed && self.agents_busy() > 0 && self.exit_typing() {
            self.armed = true;
        }
        self.exit_armed = false;
        self.typed_changed(at);
        Ok(())
    }

    fn suspend<C: Console>(&mut self, console: &mut C) -> Result<(), AppError> {
        if self.resize_at.is_some() {
            self.settle_resize();
        }
        match console.suspend() {
            Ok(()) => {
                self.suspended = true;
                Ok(())
            }
            Err(e) => self.halt(console, e),
        }
    }

    fn send(&mut self, mode: DeliveryMode, at: Duration) {
        let busy = self.busy();
        let typed = self.composer.message();
        let attachments = typed
            .parts
            .iter()
            .any(|part| !matches!(part, UserPart::Text(_)));
        let plugins = self.suggest.plugins().to_vec();
        match commands::parse(&plugins, &typed.text, attachments) {
            Parsed::Message => {}
            Parsed::Plugin {
                command,
                base,
                args,
            } => {
                if self.typed_plugin(command, base, args, busy, at) {
                    self.rewind_notes.clear();
                    self.suggest.reset();
                }
                return;
            }
            Parsed::Unknown(name) => {
                self.hint(&copy::unknown_command(name), at);
                return;
            }
            Parsed::Command { id, args } => {
                if self.already_compacting(id, at) {
                    return;
                }
                if id == CommandId::Rewind && (busy || self.status.is_compacting()) {
                    self.open_rewind(at);
                    return;
                }
                if busy && let Some(hint) = commands::between_turns(id) {
                    self.hint(hint, at);
                    return;
                }
                if self.drops_key_shaped(id, args, at) {
                    return;
                }
                let args = args.to_owned();
                self.rewind_notes.clear();
                self.composer.take_saved();
                self.suggest.reset();
                self.run_command(id, &args, at);
                return;
            }
        }
        if self.authentication.none_left {
            self.login_command("", at);
            return;
        }
        let mode = if busy { mode } else { DeliveryMode::Steer };
        self.rewind_notes.clear();
        let (message, saved) = self.composer.take_saved();
        self.suggest.reset();
        let id = self.inputs.next_id();
        let command = waiting::send_command(id.clone(), &message, mode, self.turn_for_send(), None);
        if !self.command(command, at) {
            self.composer.restore_ahead(&[saved]);
            return;
        }
        self.inputs.sent(id, mode, message.parts, saved);
        self.view.to_bottom();
        if !busy {
            self.submitted(at);
        }
    }

    fn already_compacting(&mut self, id: CommandId, at: Duration) -> bool {
        let compacting = id == CommandId::Compact && self.shown_status().is_compacting();
        if compacting {
            self.hint(ALREADY_COMPACTING, at);
        }
        compacting
    }

    fn drops_key_shaped(&mut self, id: CommandId, args: &str, at: Duration) -> bool {
        let drops = id == CommandId::Model && wisp_protocol::model_looks_like_key(args);
        if drops {
            self.shown_composer_mut().clear();
            self.suggest.reset();
            self.hint(copy::KEY_SHAPED_MODEL, at);
        }
        drops
    }

    fn release_held(&mut self, at: Duration) {
        if self.command(ClientCommand::Release, at) {
            self.view.to_bottom();
            self.submitted(at);
        }
    }

    fn command(&mut self, command: ClientCommand, at: Duration) -> bool {
        if matches!(command, ClientCommand::Send { .. }) {
            self.sync_terminal();
        }
        let sent = self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.try_send(command).is_ok());
        if !sent {
            self.hint(AGENT_STOPPED, at);
        }
        sent
    }

    fn ask_described(&mut self, at: Duration) -> Option<RequestId> {
        let request = self.next_request();
        if !self.aimed(ClientCommand::DescribeSettings { request }, at) {
            return None;
        }
        self.shown_parts().tracker.asked_described(request);
        Some(request)
    }

    fn submitted(&mut self, at: Duration) {
        let cx = self.cx();
        self.status.submitted(at, cx);
        let next = self.tracker.next();
        self.status
            .turn_effort(display_effort(&self.facts, &next.model, next.effort).map(Effort::as_str));
    }

    fn image_tier(&self) -> Result<ImageTier, String> {
        let model = self.tracker.wanted().model;
        self.facts
            .model_facts(&model)
            .and_then(|m| m.image_tier)
            .ok_or_else(|| copy::no_images_for(&model))
    }

    fn reading_due(&self) -> Option<Duration> {
        self.reading
            .filter(|r| r.clipboard && !r.shown)
            .map(|r| r.since.saturating_add(reader::SLOW))
    }

    fn reading_hint(&self) -> String {
        copy::reading_clipboard(Glyphs::of(self.glyphs).more)
    }

    fn start_reading(&mut self, job: Job, tier: ImageTier, at: Duration, quiet: bool) -> bool {
        let clipboard = matches!(job, Job::Clipboard);
        match self.images.start(job, tier) {
            Ok(()) => {
                self.reading = Some(Reading {
                    since: at,
                    clipboard,
                    shown: false,
                    quiet,
                });
                true
            }
            Err(why) => {
                self.hint(&why, at);
                false
            }
        }
    }

    fn attach_image(&mut self, target: PasteTarget, at: Duration, quiet: bool) {
        if !target.is_composer() {
            return;
        }
        if self.images.busy() {
            if !quiet {
                self.hint(copy::STILL_READING_CLIPBOARD, at);
            }
            return;
        }
        if self.images.setup().ssh {
            self.hint(copy::NO_CLIPBOARD_OVER_SSH, at);
            return;
        }
        let tier = match self.image_tier() {
            Ok(tier) => tier,
            Err(why) => {
                self.hint(&why, at);
                return;
            }
        };
        if self.composer.image_count() >= MAX_IMAGES_PER_MESSAGE {
            self.hint(&copy::too_many_images(MAX_IMAGES_PER_MESSAGE), at);
            return;
        }
        self.start_reading(Job::Clipboard, tier, at, quiet);
    }

    fn paste_paths(&mut self, text: &str, at: Duration) -> bool {
        if self.images.busy() {
            return false;
        }
        let setup = self.images.setup();
        let Some(paths) = reader::image_paths(text, setup.home.as_deref(), setup.wsl) else {
            return false;
        };
        let tier = match self.image_tier() {
            Ok(tier) => tier,
            Err(why) => {
                self.hint(&why, at);
                return false;
            }
        };
        if self.composer.image_count() + paths.len() > MAX_IMAGES_PER_MESSAGE {
            self.hint(&copy::too_many_images(MAX_IMAGES_PER_MESSAGE), at);
            return false;
        }
        let job = Job::Paths {
            text: text.to_owned(),
            paths,
        };
        self.start_reading(job, tier, at, false)
    }

    pub(super) fn take_images(&mut self, at: Duration) {
        if let Some(reading) = &mut self.reading
            && reading.clipboard
            && !reading.shown
            && at >= reading.since.saturating_add(reader::SLOW)
        {
            reading.shown = true;
            let text = self.reading_hint();
            self.hint(&text, at);
            self.frames.mark();
        }
        for got in self.images.take() {
            let reading = self.reading.take();
            if reading.is_some_and(|r| r.shown)
                && self.status.shown_hint(at) == Some(self.reading_hint().as_str())
            {
                self.status.clear_hint();
            }
            let quiet = reading.is_some_and(|r| r.quiet);
            self.frames.mark();
            if self.quitting {
                continue;
            }
            if self.secret_drops_input(at) {
                continue;
            }
            let target = self.paste_target(at);
            if matches!(target, PasteTarget::Layer(Layer::StepIn))
                && self.secret_view(false).is_some()
            {
                match got {
                    Got::Text(text) | Got::Pasted(text) | Got::Refused { text, .. } => {
                        self.secret_paste(&text)
                    }
                    _ => self.secret_paste(""),
                }
                continue;
            }
            match got {
                Got::Images { images, pasted } => {
                    if !target.is_composer() {
                        continue;
                    }
                    if self.composer.image_count() + images.len() > MAX_IMAGES_PER_MESSAGE {
                        if let Some(text) = pasted {
                            self.paste_to(target, &text, at);
                        }
                        self.hint(&copy::too_many_images(MAX_IMAGES_PER_MESSAGE), at);
                        continue;
                    }
                    let glyphs = self.glyphs;
                    for image in images {
                        let size = (image.width, image.height);
                        self.composer.attach(image.data, |n| {
                            copy::image_chip(n, size, image.tokens, glyphs)
                        });
                    }
                    self.typed_changed(at);
                }
                Got::Text(text) => self.paste_to(target, &text, at),
                Got::Failed(why) if quiet && why == copy::NO_CLIPBOARD_IMAGE => {}
                Got::Failed(why) => self.hint(&why, at),
                Got::Refused { text, why } => {
                    self.paste_to(target, &text, at);
                    self.hint(&why, at);
                }
                Got::Pasted(text) => self.paste_text(target, &text, at),
                Got::NoText => {
                    if target.is_composer() {
                        self.attach_image(target, at, true);
                        if reading.is_some_and(|r| r.shown)
                            && let Some(next) = &mut self.reading
                        {
                            next.shown = true;
                            let text = self.reading_hint();
                            self.hint(&text, at);
                        }
                    }
                }
            }
        }
    }

    fn run_command(&mut self, id: CommandId, args: &str, at: Duration) {
        match (id, args) {
            (CommandId::Exit, _) => self.exit_command(at),
            (CommandId::Leave, _) => {
                if let Some(note) = &self.leave_note {
                    self.hint(&note.clone(), at);
                } else if self.host_end.is_some() {
                    self.exit = Some(Exit::Leave);
                }
            }
            (CommandId::Resume, query) => self.open_chats(query, at),
            (CommandId::Agents, _) => self.open_agents(),
            (CommandId::Sidebar, words) => self.sidebar_command(words, at),
            (CommandId::Help, _) => self.open_help(Seek::Top),
            (CommandId::Processes, _) => self.open_processes(),
            (CommandId::Model, "") => self.open_accounts(self.chat_models(), at),
            (CommandId::Model, id) => {
                let (facts, tracker) = self.chat_pair();
                let typed = offers_typed(&facts, tracker, id)
                    .then(|| switch_to(id, &facts, tracker.wanted().effort, None));
                match typed {
                    Some(picked) => self.picked(picked, at),
                    None => self.open_accounts(self.chat_models().queried(id), at),
                }
            }
            (CommandId::Effort, "") => {
                let (facts, tracker) = self.chat_pair();
                let model = tracker.wanted().model;
                if takes_no_effort(effort_levels(&facts, &model)) {
                    let text = copy::no_effort_levels(&short_model(&model));
                    self.hint(&text, at);
                } else {
                    self.open(Picker::efforts(model, false, None), at);
                }
            }
            (CommandId::Effort, level) => match level.parse::<Effort>() {
                Ok(level) => {
                    self.change_settings(None, None, Some(level), at);
                }
                Err(_) => self.hint(&copy::unknown_effort(level), at),
            },
            (CommandId::Endpoint, "") => self.open_accounts(Picker::endpoints(), at),
            (CommandId::Endpoint, copy::ADD_WORD) => self.open_endpoint_card(at),
            (CommandId::Endpoint, name) => self.endpoint_command(name, at),
            (CommandId::Context, words) => self.context_command(words, at),
            (CommandId::Usage, words) => self.usage_command(words, at),
            (CommandId::Login, words) => self.login_command(words, at),
            (CommandId::Logout, words) => self.logout_command(words, at),
            (CommandId::Compact, focus) => {
                let request = self.next_request();
                let focus = (!focus.is_empty()).then(|| focus.to_owned());
                let focused = focus.is_some();
                if self.aimed(ClientCommand::Compact { focus, request }, at) {
                    self.shown_parts()
                        .transcript
                        .compact_asked(request, focused);
                }
            }
            (CommandId::Rewind, _) => self.open_rewind(at),
            (CommandId::Config, words) => self.open_settings(words, at),
        }
    }

    fn context_command(&mut self, words: &str, at: Duration) {
        let mut all = false;
        for word in words.split_whitespace() {
            match word {
                "all" => all = true,
                "count" => {
                    self.hint(CONTEXT_COUNTS, at);
                    return;
                }
                _ => {
                    self.hint(CONTEXT_USAGE, at);
                    return;
                }
            }
        }
        let request = self.next_request();
        let command = ClientCommand::ContextReport {
            detail: if all {
                ReportDetail::All
            } else {
                ReportDetail::Summary
            },
            request,
        };
        self.aimed(command, at);
    }

    fn endpoint_command(&mut self, name: &str, at: Duration) {
        let mut available: Vec<String> = self
            .facts
            .available_endpoints()
            .into_iter()
            .map(|e| e.name.clone())
            .collect();
        if available.is_empty() {
            available.push(self.tracker.session().endpoint.clone());
        }
        let valid = valid_endpoint_name(name);
        let expired = self.facts.account_state(name) == AccountState::Expired;
        if let Some(reason) = self
            .facts
            .endpoint_facts(name)
            .and_then(|e| e.unavailable.as_deref())
            .filter(|_| !expired)
        {
            let text = copy::endpoint_unavailable(name, reason);
            self.hint(&text, at);
        } else if valid && (expired || available.iter().any(|a| a == name)) {
            if let Some(request) = self.ask_described(at) {
                self.account_describing = Some((request, self.here(), name.to_owned()));
            }
        } else {
            let text = copy::no_endpoint(valid.then_some(name), &available);
            self.hint(&text, at);
        }
    }

    fn account_now(&mut self, name: &str, at: Duration) {
        let (facts, tracker) = self.chat_pair();
        match Picker::account_step(name, &facts, tracker) {
            Step::Done(picked) => self.picked(picked, at),
            Step::SignIn { account, then } => self.sign_in_for(account, then, at),
            Step::Close => self.hint(wisp_render::chrome::account::THIS_CHAT_NOW, at),
            _ => {}
        }
    }

    fn account_described(
        &mut self,
        agent: &AgentPath,
        snapshot: &wisp_protocol::settings::SettingsSnapshot,
        at: Duration,
    ) {
        if let Some((_, origin, name)) = self
            .account_describing
            .take_if(|(request, origin, _)| *request == snapshot.answers && *origin == *agent)
            && origin == self.here()
        {
            self.account_now(&name, at);
        }
    }

    pub(super) fn docked(&self) -> bool {
        retry::Docked::ALL
            .into_iter()
            .any(|dock| self.dock_open(dock))
    }

    fn rows_with(&self, composer: usize, fold: bool) -> Rows {
        let chrome = self.chrome_layout();
        let notes = self.chats.as_ref().map(|chats| {
            let place = ChatsPlace::InChat(Joins::default());
            chats.view(place, self.chats_notes()).below
        });
        let need = self.overlay_rows();
        rows_fitting(self.size, composer, fold || notes.is_some(), |rows| {
            let view = usize::from(rows.view);
            match &notes {
                Some(notes) => chats_whole(dock_room(view), notes, joins(rows), chrome),
                None => view >= need,
            }
        })
    }

    fn agent_card_rows(&self, room: u16, at: Duration, chrome: Layout) -> u16 {
        if self.picker_open() || self.chat.is_some() {
            return 0;
        }
        let room = usize::from(room);
        let card = self.card_height(room, at, chrome);
        let step_in = self
            .step_in_lines(!self.step_settle.settled(at), room, chrome)
            .map(|lines| lines.len());
        let rows = card.into_iter().chain(step_in).max().unwrap_or(0);
        u16::try_from(rows).unwrap_or(u16::MAX)
    }

    fn overlay_rows(&self) -> usize {
        let chrome = self.chrome_layout();
        [
            self.docked().then_some(PICKER_MIN_ROWS),
            self.agents_open().then_some(AGENTS_MIN_ROWS),
            self.ps_open().then_some(PS_MIN_ROWS),
            self.step_in_lines(false, usize::MAX, chrome)
                .map(|lines| lines.len()),
            self.auth_card(chrome).map(|rows| rows.lines.len()),
            self.card_height(usize::from(u16::MAX), Duration::MAX, chrome),
            self.endpoint_card_height(chrome),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0)
    }

    pub(super) fn chats_fit(&mut self) -> bool {
        let shape = self.composer_shape();
        let composer = self.composer.view(shape).rows.len();
        let fits = |rows: Rows| chats_fit(dock_room(usize::from(rows.view)));
        fits(rows_fitting(self.size, composer, true, fits))
    }

    pub(super) fn dock_fits(&mut self) -> bool {
        let shape = self.composer_shape();
        let composer = self.shown_composer_mut().view(shape).rows.len();
        let fold = self.shown_composer().is_blank();
        let fits = |rows: Rows| usize::from(rows.view) >= PICKER_MIN_ROWS;
        fits(rows_fitting(self.size, composer, fold, fits))
    }

    fn open(&mut self, picker: Picker, at: Duration) {
        if self.card_picker_refused(at) {
            return;
        }
        if !self.dock_fits() {
            self.picker = None;
            self.hint(copy::PICKER_NO_ROOM, at);
        } else {
            self.picker = Some(picker);
        }
    }

    fn chip_pressed(&self, mouse: Mouse) -> bool {
        self.chip_at
            .as_ref()
            .is_some_and(|(y, cols)| *y == mouse.row && cols.contains(&mouse.column))
    }

    fn open_accounts(&mut self, picker: Picker, at: Duration) {
        self.ask_accounts(at);
        self.open_quoted(picker, at);
    }

    fn open_quoted(&mut self, picker: Picker, at: Duration) {
        match self.ask_described(at) {
            Some(request) => self.open(picker.asking(request), at),
            None => self.open(picker, at),
        }
    }

    fn picker_key(&mut self, key: Key, at: Duration) {
        let Some(mut picker) = self.picker.take() else {
            return;
        };
        let saves = picker.saves();
        let (facts, tracker) = self.chat_pair();
        let step = picker.key(key, &facts, tracker);
        self.picker = Some(picker);
        match step {
            Step::Stay => {}
            Step::Close => {
                self.picker = None;
                self.adding_model = false;
            }
            Step::Done(picked) => {
                self.picker = None;
                if std::mem::take(&mut self.adding_model) {
                    self.settings_model_added(&picked, at);
                } else if saves {
                    self.settings_picked(picked, at);
                } else {
                    self.picked(picked, at);
                }
            }
            Step::Add if saves => self.open_endpoint_card(at),
            Step::Add => {
                self.picker = None;
                self.close_agents(at);
                self.login_command("", at);
            }
            Step::SignIn { account, then } => {
                self.picker = None;
                self.sign_in_for(account, then, at);
            }
        }
    }

    fn sign_in_for(&mut self, account: String, then: Picked, at: Duration) {
        if self.chat.is_some() {
            self.close_agents(at);
            self.login_command(&account, at);
        } else {
            self.sign_in_then(Some(account), Then::Pick(then), at);
        }
    }

    pub(super) fn picked(&mut self, picked: Picked, at: Duration) {
        match picked {
            Picked::Settings {
                endpoint,
                model,
                effort,
            } => {
                self.change_settings(endpoint, model, effort, at);
            }
            Picked::Effort {
                endpoint,
                model,
                preselect,
            } => {
                self.open(Picker::efforts_on(endpoint, model, true, preselect), at);
            }
        }
    }

    fn change_settings(
        &mut self,
        endpoint: Option<String>,
        model: Option<String>,
        effort: Option<Effort>,
        at: Duration,
    ) -> bool {
        let request = self.next_request();
        let command = ClientCommand::shortcut(request, endpoint.clone(), model.clone(), effort);
        if !self.aimed(command, at) {
            return false;
        }
        (self.shown_parts().tracker).asked_settings(
            request,
            endpoint.as_deref(),
            model.as_deref(),
            effort,
        );
        true
    }

    fn next_request(&mut self) -> RequestId {
        self.requests += 1;
        RequestId(self.requests)
    }

    fn cycle_effort(&mut self, at: Duration) {
        let (facts, tracker) = self.chat_pair();
        match tracker.cycle(&facts, at, self.glyphs) {
            Ok((level, hint)) => {
                if self.change_settings(None, None, Some(level), at) {
                    self.shown_status_mut().hint(&hint, at, Some(CYCLE_HINT));
                }
            }
            Err(why) => self.hint(&why, at),
        }
    }

    pub(super) fn event(&mut self, event: &AgentEvent) {
        let at = self.at(Instant::now());
        if self.auth_event(event, at) {
            return;
        }
        let wanted = self.tracker.wanted();
        let models = [
            self.tracker.session().model.as_str(),
            self.tracker.next().model.as_str(),
            wanted.model.as_str(),
            self.facts.model.as_str(),
        ];
        let followed = self.agents.followed(event);
        let event = followed.as_ref().unwrap_or(event);
        match without_models(event, &models) {
            Some(shown) => self.shown_event(&shown),
            None => self.shown_event(event),
        }
        self.cards_take_the_keys();
    }

    fn shown_event(&mut self, event: &AgentEvent) {
        self.display_changed(event);
        self.sidebar_event(event);
        self.plugin_event(event);
        let at = self.at(Instant::now());
        self.attach_refused(event, at);
        self.agents_event(event, at);
        self.resumed_agents(event);
        let cx = self.cx();
        self.gone_event(event);
        if self.going_off_event(event, at) || self.endpoint_event(event, at) {
            self.view.sync(self.transcript.cells().len());
            self.frames.mark();
            return;
        }
        self.settings_event(event);
        let before = self.tracker.wanted().endpoint;
        let answer = self.tracker.apply(event, &self.facts, at, self.glyphs);
        if let Some(sign_in) = &mut self.authentication.sign_in {
            sign_in.wizard.chat_on(&self.tracker.next().endpoint);
        }
        let answer = self.retry_answer(event, answer);
        let answer = self.held_move(answer, &before);
        let answer = self.continued(answer);
        self.transcript.set_cache_ttl(self.tracker.cache_ttl());
        if self.answer_notes(None, answer, at) {
            self.view.sync(self.transcript.cells().len());
            self.frames.mark();
            return;
        }
        self.usage_event(event, at);
        match event {
            AgentEvent::SettingsSnapshot(snapshot) => {
                if let Some(picker) = &mut self.picker {
                    picker.answered(snapshot.answers);
                }
                self.logout_described(snapshot, at);
                self.account_described(&AgentPath::root(), snapshot, at);
            }
            AgentEvent::TurnStarted { .. } => {
                self.turns = self.turns.saturating_add(1);
            }
            _ => {}
        }
        if !self.rewind_before(None, event) {
            self.transcript
                .endpoint(Some(&self.tracker.session().endpoint));
            self.transcript.apply(event, at);
        }
        self.rewind_event(None, event, at);
        self.chats_event(event);
        self.process_event(event, at);
        self.retry_event(event, at);
        let session = self.tracker.session();
        let effort =
            display_effort(&self.facts, &session.model, session.effort).map(Effort::as_str);
        let layout = self.layout();
        feed_status(
            &mut self.status,
            &self.transcript,
            event,
            effort,
            at,
            cx,
            layout,
        );
        if matches!(event, AgentEvent::QuestionAsked { .. }) {
            self.close_help();
            self.close_section_page();
            self.suggest.quiet();
        }
        if matches!(
            event,
            AgentEvent::Restart {
                state: wisp_protocol::RestartState::Cancelled,
                ..
            }
        ) {
            self.show_start_notes(&[copy::RESTART_CANCELLED.to_owned()]);
            self.ask_agents();
        }
        self.card_event(event, at);
        self.settings_card_opened(event);
        self.settings_turn_ended(event);
        let applied = self.inputs.apply(event, &mut self.transcript);
        self.applied(applied, at);
        self.late_accepted(event, at);
        self.await_inputs(event, at);
        self.gone_refused(event, at);
        if self.tracker.show(&mut self.facts) {
            self.transcript.set_provider(&self.facts.provider);
            self.view.refresh_banner();
        }
        self.view.sync(self.transcript.cells().len());
        self.frames.mark();
    }

    fn answer_notes(&mut self, agent: Option<&AgentPath>, answer: Answer, at: Duration) -> bool {
        let parts = self.parts_mut(agent);
        match answer {
            Answer::Refused(text) => {
                parts.transcript.note(&text);
                return true;
            }
            Answer::Hint(text) => parts.status.hint(&text, at, None),
            Answer::Note(text) => parts.transcript.note(&text),
            Answer::Moved(moved, after) => {
                parts.transcript.moved(*moved);
                for text in after {
                    parts.transcript.note(&text);
                }
            }
            Answer::Notes(texts) => {
                for text in texts {
                    parts.transcript.note(&text);
                }
            }
            Answer::Other => {}
        }
        false
    }

    fn events_closed(&mut self) {
        self.commands = None;
        if let Some(ended) = &self.host_ended {
            self.exit = Some(host_exit(
                ended.borrow().clone(),
                self.quitting,
                self.tracker.saving(),
            ));
        } else if !self.quitting {
            let at = self.at(Instant::now());
            self.hint(AGENT_STOPPED, at);
            self.exit = Some(Exit::Ended);
        }
        self.frames.mark();
    }

    pub(super) fn pager_seek(&self) -> Seek {
        if self.view.is_following() {
            return Seek::Bottom;
        }
        let Some((top, _)) = self.view_top() else {
            return Seek::Bottom;
        };
        let mut index = top;
        while index < self.view.len() {
            match self.view.source(index) {
                Some(Source::Cell(cell)) => return Seek::Cell(*cell),
                Some(Source::Banner) => return Seek::Cell(0),
                _ => index += 1,
            }
        }
        Seek::Bottom
    }

    fn view_top(&self) -> Option<(usize, usize)> {
        match self.view.anchor() {
            super::view::Anchor::At { item, offset } => Some((item, offset)),
            super::view::Anchor::Follow => None,
        }
    }
}
