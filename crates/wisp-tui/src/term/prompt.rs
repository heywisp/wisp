use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use signal_hook::consts::{SIGHUP, SIGINT};
use signal_hook::iterator::Signals;
use wisp_render::chrome::account::mark_line;
use wisp_render::chrome::masked_secret;
use wisp_render::chrome::selection_mark;
use wisp_render::markdown::Layout;
use wisp_render::styled::{Look, StyledLine, StyledSpan};
use wisp_render::text::{WidthMode, width as text_width, wrap_plain};
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::copy::{CANCEL_WORD, enter_choose, esc};

use super::caps::{Caps, Env};
use super::input::{Key, key_of};
use super::print::{RESET, Stream, inline, styled};
use super::signals;
use crate::app::list::{Pick, Picked};

pub const CANCEL_EXIT: u8 = 130;

const PASTE_ON: &str = "\u{1b}[?2004h";

const PASTE_OFF: &str = "\u{1b}[?2004l";

const CLEAR_TO_END: &str = "\u{1b}[K";

const CLEAR_BELOW: &str = "\u{1b}[J";

pub const ECHO_ROOM: usize = 24;

#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Io(#[from] io::Error),
}

static ACTIVE: Mutex<Option<String>> = Mutex::new(None);
static DELEGATED: AtomicBool = AtomicBool::new(false);

pub fn delegate_interrupts_and_hangups() {
    DELEGATED.store(true, Ordering::SeqCst);
}

fn active() -> MutexGuard<'static, Option<String>> {
    ACTIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn on_signal(signal: i32) {
    if DELEGATED.load(Ordering::SeqCst) && matches!(signal, SIGINT | SIGHUP) {
        return;
    }
    let mut slot = active();
    match slot.take() {
        Some(message) => {
            drop(crossterm::terminal::disable_raw_mode());
            let mut err = io::stderr();
            drop(write!(err, "{PASTE_OFF}{RESET}\r\n{message}\r\n"));
            drop(err.flush());
            signals::die(signal);
        }
        None if signal == SIGINT || (super::guard::active().is_none() && !signals::held()) => {
            signals::die(signal);
        }
        None => {}
    }
}

fn install() -> io::Result<()> {
    static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
    INSTALLED
        .get_or_init(|| {
            let caught = std::iter::once(SIGINT).chain(signals::deaths());
            let mut signals = Signals::new(caught).map_err(|e| e.to_string())?;
            std::thread::Builder::new()
                .name("wisp-prompt-signals".to_owned())
                .spawn(move || {
                    for signal in signals.forever() {
                        on_signal(signal);
                    }
                })
                .map(drop)
                .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(io::Error::other)
}

struct Raw;

impl Raw {
    fn enter(cancel: &str) -> io::Result<Self> {
        let mut slot = active();
        crossterm::terminal::enable_raw_mode()?;
        *slot = Some(cancel.to_owned());
        drop(slot);
        let raw = Self;
        let mut err = io::stderr();
        write!(err, "{PASTE_ON}")?;
        err.flush()?;
        Ok(raw)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let mut slot = active();
        let mut err = io::stderr();
        drop(write!(err, "{PASTE_OFF}{RESET}"));
        drop(err.flush());
        drop(crossterm::terminal::disable_raw_mode());
        *slot = None;
    }
}

pub struct EnterWatch(());

impl EnterWatch {
    pub fn poll(&self, wait: Duration) -> Result<bool, PromptError> {
        if !crossterm::event::poll(wait)? {
            return Ok(false);
        }
        match step(&mut String::new(), &crossterm::event::read()?, Echo::Plain) {
            Step::Done => Ok(true),
            Step::Cancel => Err(PromptError::Cancelled),
            _ => Ok(false),
        }
    }
}

impl Drop for EnterWatch {
    fn drop(&mut self) {
        let mut slot = active();
        drop(crossterm::terminal::disable_raw_mode());
        *slot = None;
    }
}

pub fn flush_typed_ahead() {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        let _flushed = rustix::termios::tcflush(&stdin, rustix::termios::QueueSelector::IFlush);
    }
    while crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Echo {
    Plain,
    Masked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EchoLine {
    pub own_line: bool,
    pub room: usize,
}

pub fn echo_line(question: &str, width: usize) -> EchoLine {
    let width = if width == 0 { 80 } else { width };
    let used = question.chars().count() % width;
    let inline = width.saturating_sub(used + 1);
    if inline >= ECHO_ROOM || inline + 1 >= width {
        EchoLine {
            own_line: false,
            room: inline,
        }
    } else {
        EchoLine {
            own_line: true,
            room: width.saturating_sub(1),
        }
    }
}

fn masked_echo(text: &str, room: usize, ellipsis: &str, caps: &Caps) -> Vec<u8> {
    let line = StyledLine::new(vec![masked_secret(text, room, ellipsis)]);
    inline(line, caps, Stream::Stderr, room)
}

pub fn terminal_columns() -> usize {
    crossterm::terminal::size()
        .ok()
        .map(|(columns, _)| usize::from(columns))
        .filter(|columns| *columns > 0)
        .unwrap_or(80)
}

pub fn glyphs() -> Glyphs {
    Glyphs::of(super::caps::plain_glyphs(&super::caps::Env::current()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Ignore,
    Typed(String),
    Replaced,
    Erased,
    Done,
    Cancel,
}

fn key_step(text: &mut String, key: &KeyEvent) -> Step {
    if key.kind == KeyEventKind::Release {
        return Step::Ignore;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c' | 'd') if control => Step::Cancel,
        KeyCode::Enter => Step::Done,
        KeyCode::Char('j' | 'm') if control => Step::Done,
        KeyCode::Backspace => match text.pop() {
            Some(_) => Step::Erased,
            None => Step::Ignore,
        },
        KeyCode::Char('h') if control => match text.pop() {
            Some(_) => Step::Erased,
            None => Step::Ignore,
        },
        KeyCode::Char('u') if control && !text.is_empty() => {
            text.clear();
            Step::Replaced
        }
        KeyCode::Char(c) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
            text.push(c);
            Step::Typed(c.to_string())
        }
        _ => Step::Ignore,
    }
}

fn step(text: &mut String, event: &Event, echo: Echo) -> Step {
    match event {
        Event::Key(key) => key_step(text, key),
        Event::Paste(pasted) => {
            let clean: String = pasted.chars().filter(|c| !c.is_control()).collect();
            match echo {
                Echo::Plain => {
                    text.push_str(&clean);
                    Step::Typed(clean)
                }
                Echo::Masked => {
                    *text = clean;
                    Step::Replaced
                }
            }
        }
        _ => Step::Ignore,
    }
}

const DIGITS_BELOW: usize = 10;

fn pick_step(pick: &mut Pick, event: &Event) -> Result<Picked, PromptError> {
    let Event::Key(key) = event else {
        return Ok(Picked::Not);
    };
    let Some(pressed) = key_of(*key) else {
        return Ok(Picked::Not);
    };
    match key_step(&mut String::new(), key) {
        Step::Cancel => Err(PromptError::Cancelled),
        Step::Done => Ok(pick.key(Key::plain(super::KeyCode::Enter))),
        _ if pressed.code == super::KeyCode::Esc => Err(PromptError::Cancelled),
        _ => Ok(pick.key(pressed)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Numbered,
    Picking(Option<usize>),
}

pub fn lead_width(count: usize) -> usize {
    count.to_string().len() + 4
}

pub type ListRow = (Vec<String>, Option<u8>);

pub fn list_lines(rows: &[ListRow], shown: Shown, glyphs: &Glyphs) -> Vec<StyledLine> {
    let count = rows.len();
    let digits = count.to_string().len();
    let mark = match shown {
        Shown::Picking(mark) => mark,
        Shown::Numbered => None,
    };
    let numbers = shown == Shown::Numbered || count < DIGITS_BELOW;
    let indent = " ".repeat(lead_width(count));
    let mut out = vec![];
    for (index, (lines, account)) in rows.iter().enumerate() {
        let marked = mark == Some(index);
        let look = Look {
            bold: marked,
            ..Look::default()
        };
        let number = if numbers {
            format!("{:>digits$}", index + 1)
        } else {
            " ".repeat(digits)
        };
        for (at, line) in lines.iter().enumerate() {
            let mut spans = match (at, marked) {
                (0, true) => {
                    let mark = selection_mark(Layout::new(indent.len(), glyphs.set));
                    let used = text_width(&mark.text, WidthMode::from(glyphs.set));
                    vec![
                        mark,
                        StyledSpan::plain(" ".repeat(2_usize.saturating_sub(used))),
                    ]
                }
                (0, false) => vec![StyledSpan::plain("  ")],
                _ => vec![StyledSpan::plain(indent.clone())],
            };
            if at == 0 {
                spans.push(StyledSpan::new(number.clone(), Token::TextFaint));
                spans.push(StyledSpan::plain("  "));
            }
            match account {
                Some(_) => spans.extend(mark_line(line, look, *account, glyphs).spans),
                None => spans.push(StyledSpan::new(line.clone(), look)),
            }
            out.push(StyledLine::new(spans));
        }
    }
    if let Shown::Picking(_) = shown {
        let keys = [enter_choose(), esc(CANCEL_WORD)].map(|hint| hint.text());
        out.push(StyledLine::new(vec![StyledSpan::new(
            keys.join(&format!(" {} ", glyphs.separator)),
            Token::TextFaint,
        )]));
    }
    out
}

pub fn numbered(rows: &[ListRow], glyphs: &Glyphs) -> String {
    String::from_utf8_lossy(&super::print::plain(&list_lines(
        rows,
        Shown::Numbered,
        glyphs,
    )))
    .into_owned()
}

pub fn lead_lines(lead: &str, width: usize, glyphs: &Glyphs) -> Vec<StyledLine> {
    lead.lines()
        .flat_map(|line| wrap_plain(line, width, "", "", glyphs))
        .map(|line| StyledLine::new(vec![StyledSpan::plain(line)]))
        .collect()
}

fn frame(lines: &[StyledLine], caps: &Caps, up: usize, width: usize) -> Vec<u8> {
    let mut out = match up {
        0 => b"\r".to_vec(),
        up => format!("\u{1b}[{up}A\r").into_bytes(),
    };
    for line in styled(lines, caps, Stream::Stderr, width)
        .split(|b| *b == b'\n')
        .take(lines.len())
    {
        out.extend_from_slice(CLEAR_TO_END.as_bytes());
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out
}

pub struct Prompter {
    cancel: String,
    caps: Caps,
}

impl Prompter {
    pub fn new(cancel: impl Into<String>, caps: Caps) -> Self {
        Self {
            cancel: cancel.into(),
            caps,
        }
    }

    pub fn glyphs(&self) -> Glyphs {
        Glyphs::of(self.caps.glyphs.value)
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    pub fn fits(&self, lines: usize) -> bool {
        let height = crossterm::terminal::size().map_or(0, |(_, rows)| usize::from(rows));
        !Env::current().dumb() && lines + 2 <= height
    }

    pub fn select(
        &self,
        lead: &str,
        rows: &dyn Fn(usize) -> Vec<ListRow>,
        start: Option<usize>,
    ) -> Result<usize, PromptError> {
        install()?;
        let glyphs = self.glyphs();
        let mut width = terminal_columns();
        let mut shown = rows(width);
        let mut pick = Pick {
            rows: shown.len(),
            held: vec![true; shown.len()],
            focus: start,
        };
        let mut err = io::stderr();
        let raw = Raw::enter(&self.cancel)?;
        flush_typed_ahead();
        let mut draw = |shown: &[ListRow], state: Shown, up: usize, width: usize| {
            let mut lines = lead_lines(lead, width, &glyphs);
            lines.extend(list_lines(shown, state, &glyphs));
            err.write_all(&frame(&lines, &self.caps, up, width))?;
            err.flush()?;
            Ok::<usize, io::Error>(lines.len())
        };
        let mut drawn = draw(&shown, Shown::Picking(pick.focus), 0, width)?;
        let outcome = loop {
            let event = match crossterm::event::read() {
                Ok(event) => event,
                Err(e) => break Err(PromptError::Io(e)),
            };
            if let Event::Resize(columns, _) = event {
                if usize::from(columns).max(1) == width {
                    continue;
                }
                width = usize::from(columns).max(1);
                shown = rows(width);
                drawn = draw(&shown, Shown::Picking(pick.focus), 0, width)?;
                continue;
            }
            match pick_step(&mut pick, &event) {
                Ok(Picked::Not) => {}
                Ok(Picked::Moved) => {
                    drawn = draw(&shown, Shown::Picking(pick.focus), drawn, width)?;
                }
                Ok(Picked::Took(index)) => break Ok(index),
                Err(e) => break Err(e),
            }
        };
        err.write_all(&frame(&[], &self.caps, drawn, width))?;
        write!(err, "{CLEAR_BELOW}")?;
        err.flush()?;
        drop(raw);
        outcome
    }

    pub fn cancel_message(&self) -> &str {
        &self.cancel
    }

    pub fn visible(&self, question: &str) -> Result<String, PromptError> {
        self.read(question, Echo::Plain, "")
    }

    pub fn edit(&self, question: &str, start: &str) -> Result<String, PromptError> {
        self.read(question, Echo::Plain, start)
    }

    pub fn masked(&self, question: &str) -> Result<String, PromptError> {
        self.read(question, Echo::Masked, "")
    }

    pub fn watch_enter(&self) -> Result<EnterWatch, PromptError> {
        install()?;
        let mut slot = active();
        crossterm::terminal::enable_raw_mode()?;
        *slot = Some(self.cancel.clone());
        Ok(EnterWatch(()))
    }

    fn read(&self, question: &str, echo: Echo, start: &str) -> Result<String, PromptError> {
        install()?;
        let mut err = io::stderr();
        let width = terminal_columns();
        let line = echo_line(question, width);
        let lead = if line.own_line { "" } else { question };
        let ellipsis = self.glyphs().more;
        let raw = Raw::enter(&self.cancel)?;
        if echo == Echo::Masked {
            flush_typed_ahead();
        }
        write!(err, "{question}{start}")?;
        if echo == Echo::Masked && line.own_line {
            write!(err, "\r\n")?;
        }
        err.flush()?;
        let mut text = start.to_owned();
        let outcome = loop {
            let event = match crossterm::event::read() {
                Ok(event) => event,
                Err(e) => break Err(PromptError::Io(e)),
            };
            match (step(&mut text, &event, echo), echo) {
                (Step::Ignore, _) => {}
                (Step::Done, _) => break Ok(()),
                (Step::Cancel, _) => break Err(PromptError::Cancelled),
                (Step::Typed(_) | Step::Erased | Step::Replaced, Echo::Masked) => {
                    write!(err, "\r{lead}")?;
                    err.write_all(&masked_echo(&text, line.room, ellipsis, &self.caps))?;
                    write!(err, "{CLEAR_TO_END}")?;
                    err.flush()?;
                }
                (Step::Typed(typed), Echo::Plain) => {
                    write!(err, "{typed}")?;
                    err.flush()?;
                }
                (Step::Erased, Echo::Plain) => {
                    write!(err, "\u{8} \u{8}")?;
                    err.flush()?;
                }
                (Step::Replaced, Echo::Plain) => {
                    write!(err, "\r{question}{text}{CLEAR_TO_END}")?;
                    err.flush()?;
                }
            }
        };
        drop(raw);
        write!(err, "\r\n")?;
        err.flush()?;
        outcome.map(|()| text)
    }
}

#[cfg(test)]
mod tests;
