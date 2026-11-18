use std::fmt;
use std::io;

use super::guard::Halted;

use crossterm::Command;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::style::{Attribute, SetAttribute};
use crossterm::terminal::{
    EnableLineWrap, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyboardFlags(pub u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollBack {
    Set,
    Untouched,
}

pub const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
pub const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";
pub const ALTERNATE_SCROLL_OFF: &str = "\x1b[?1007l";
pub const ALTERNATE_SCROLL_ON: &str = "\x1b[?1007h";

impl KeyboardFlags {
    pub const DISAMBIGUATE: KeyboardFlags = KeyboardFlags(0b1);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modes {
    pub raw: bool,
    pub alternate_screen: bool,
    pub keyboard: Option<KeyboardFlags>,
    pub bracketed_paste: bool,
    pub focus_reports: bool,
    pub cursor_hidden: bool,
    pub mouse: bool,
    pub alternate_scroll_off: Option<ScrollBack>,
}

impl Modes {
    pub const COOKED: Modes = Modes {
        raw: false,
        alternate_screen: false,
        keyboard: None,
        bracketed_paste: false,
        focus_reports: false,
        cursor_hidden: false,
        mouse: false,
        alternate_scroll_off: None,
    };

    pub(super) const fn bits(&self) -> u16 {
        (self.raw as u16)
            | (self.alternate_screen as u16) << 1
            | (self.keyboard.is_some() as u16) << 2
            | (self.bracketed_paste as u16) << 3
            | (self.focus_reports as u16) << 4
            | (self.cursor_hidden as u16) << 5
            | (self.mouse as u16) << 6
            | (self.alternate_scroll_off.is_some() as u16) << 7
            | (matches!(self.alternate_scroll_off, Some(ScrollBack::Set)) as u16) << 8
    }

    pub(super) fn from_bits(bits: u16) -> Modes {
        let on = |i: u16| bits & (1 << i) != 0;
        Modes {
            raw: on(0),
            alternate_screen: on(1),
            keyboard: if on(2) { Some(KeyboardFlags(0)) } else { None },
            bracketed_paste: on(3),
            focus_reports: on(4),
            cursor_hidden: on(5),
            mouse: on(6),
            alternate_scroll_off: match (on(7), on(8)) {
                (false, _) => None,
                (true, true) => Some(ScrollBack::Set),
                (true, false) => Some(ScrollBack::Untouched),
            },
        }
    }

    pub(super) fn reconcile(self, bits: u16) -> Modes {
        let mirror = Modes::from_bits(bits);
        Modes {
            keyboard: if mirror.keyboard.is_some() && self.keyboard.is_some() {
                self.keyboard
            } else {
                mirror.keyboard
            },
            ..mirror
        }
    }

    pub const fn is_empty(&self) -> bool {
        !self.raw
            && !self.alternate_screen
            && self.keyboard.is_none()
            && !self.bracketed_paste
            && !self.focus_reports
            && !self.cursor_hidden
            && !self.mouse
            && self.alternate_scroll_off.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    EndLink,
    EndSynchronizedUpdate,
    ResetSgr,
    Cursor { hidden: bool },
    Autowrap,
    FocusReports(bool),
    BracketedPaste(bool),
    Keyboard(Option<KeyboardFlags>),
    AlternateScreen(bool),
    Mouse(bool),
    AlternateScrollOff(ScrollBack),
    AlternateScrollBack(ScrollBack),
    Termios { raw: bool },
}

pub const RESTORE_ORDER: [&str; 12] = [
    "end any link",
    "end synchronized update",
    "reset SGR",
    "show the cursor",
    "enable autowrap",
    "disable mouse reports",
    "put alternate scroll back",
    "disable focus reports",
    "disable bracketed paste",
    "pop keyboard flags",
    "leave the alternate screen",
    "restore termios",
];

pub(super) fn ansi(command: impl Command) -> String {
    let mut out = String::new();
    match command.write_ansi(&mut out) {
        Ok(()) => out,
        Err(fmt::Error) => String::new(),
    }
}

impl Change {
    pub fn name(self) -> &'static str {
        match self {
            Change::EndLink => "end any link",
            Change::EndSynchronizedUpdate => "end synchronized update",
            Change::ResetSgr => "reset SGR",
            Change::Cursor { hidden: false } => "show the cursor",
            Change::Cursor { hidden: true } => "hide the cursor",
            Change::Autowrap => "enable autowrap",
            Change::FocusReports(false) => "disable focus reports",
            Change::FocusReports(true) => "enable focus reports",
            Change::BracketedPaste(false) => "disable bracketed paste",
            Change::BracketedPaste(true) => "enable bracketed paste",
            Change::Keyboard(None) => "pop keyboard flags",
            Change::Keyboard(Some(_)) => "push keyboard flags",
            Change::AlternateScreen(false) => "leave the alternate screen",
            Change::AlternateScreen(true) => "enter the alternate screen",
            Change::Mouse(false) => "disable mouse reports",
            Change::Mouse(true) => "enable mouse reports",
            Change::AlternateScrollOff(_) => "turn alternate scroll off",
            Change::AlternateScrollBack(_) => "put alternate scroll back",
            Change::Termios { raw: false } => "restore termios",
            Change::Termios { raw: true } => "enter raw mode",
        }
    }

    pub fn bytes(self) -> Option<String> {
        Some(match self {
            Change::EndLink => format!("\x1b\\{}", super::screen::LINK_CLOSE),
            Change::EndSynchronizedUpdate => ansi(EndSynchronizedUpdate),
            Change::ResetSgr => ansi(SetAttribute(Attribute::Reset)),
            Change::Cursor { hidden: true } => ansi(Hide),
            Change::Cursor { hidden: false } => ansi(Show),
            Change::Autowrap => ansi(EnableLineWrap),
            Change::FocusReports(true) => ansi(EnableFocusChange),
            Change::FocusReports(false) => ansi(DisableFocusChange),
            Change::BracketedPaste(true) => ansi(EnableBracketedPaste),
            Change::BracketedPaste(false) => ansi(DisableBracketedPaste),
            Change::Keyboard(Some(flags)) => ansi(PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::from_bits_truncate(flags.0),
            )),
            Change::Keyboard(None) => ansi(PopKeyboardEnhancementFlags),
            Change::AlternateScreen(true) => ansi(EnterAlternateScreen),
            Change::AlternateScreen(false) => ansi(LeaveAlternateScreen),
            Change::Mouse(true) => MOUSE_ON.to_owned(),
            Change::Mouse(false) => MOUSE_OFF.to_owned(),
            Change::AlternateScrollOff(_) => ALTERNATE_SCROLL_OFF.to_owned(),
            Change::AlternateScrollBack(ScrollBack::Set) => ALTERNATE_SCROLL_ON.to_owned(),
            Change::AlternateScrollBack(ScrollBack::Untouched) => String::new(),
            Change::Termios { .. } => return None,
        })
    }

    const fn enables(self) -> bool {
        match self {
            Change::EndLink
            | Change::EndSynchronizedUpdate
            | Change::ResetSgr
            | Change::Autowrap
            | Change::AlternateScrollBack(_) => false,
            Change::Cursor { hidden } => hidden,
            Change::FocusReports(on)
            | Change::BracketedPaste(on)
            | Change::AlternateScreen(on)
            | Change::Mouse(on) => on,
            Change::AlternateScrollOff(_) => true,
            Change::Keyboard(flags) => flags.is_some(),
            Change::Termios { raw } => raw,
        }
    }

    fn record(self, modes: &mut Modes) {
        match self {
            Change::EndLink
            | Change::EndSynchronizedUpdate
            | Change::ResetSgr
            | Change::Autowrap => {}
            Change::Cursor { hidden } => modes.cursor_hidden = hidden,
            Change::FocusReports(on) => modes.focus_reports = on,
            Change::BracketedPaste(on) => modes.bracketed_paste = on,
            Change::Keyboard(flags) => modes.keyboard = flags,
            Change::AlternateScreen(on) => modes.alternate_screen = on,
            Change::Mouse(on) => modes.mouse = on,
            Change::AlternateScrollOff(back) => modes.alternate_scroll_off = Some(back),
            Change::AlternateScrollBack(_) => modes.alternate_scroll_off = None,
            Change::Termios { raw } => modes.raw = raw,
        }
    }
}

pub fn restore_plan(from: &Modes) -> Vec<Change> {
    let mut plan = vec![
        Change::EndLink,
        Change::EndSynchronizedUpdate,
        Change::ResetSgr,
        Change::Cursor { hidden: false },
        Change::Autowrap,
    ];
    plan.extend(disables(from, &Modes::COOKED));
    plan
}

fn disables(from: &Modes, to: &Modes) -> Vec<Change> {
    let mut out = vec![];
    if from.mouse && !to.mouse {
        out.push(Change::Mouse(false));
    }
    if let Some(back) = from.alternate_scroll_off
        && to.alternate_scroll_off.is_none()
    {
        out.push(Change::AlternateScrollBack(back));
    }
    if from.focus_reports && !to.focus_reports {
        out.push(Change::FocusReports(false));
    }
    if from.bracketed_paste && !to.bracketed_paste {
        out.push(Change::BracketedPaste(false));
    }
    if from.keyboard.is_some() && from.keyboard != to.keyboard {
        out.push(Change::Keyboard(None));
    }
    if from.alternate_screen && !to.alternate_screen {
        out.push(Change::AlternateScreen(false));
    }
    if from.raw && !to.raw {
        out.push(Change::Termios { raw: false });
    }
    out
}

pub fn transition(from: &Modes, to: &Modes) -> Vec<Change> {
    let mut out = vec![];
    if from.cursor_hidden && !to.cursor_hidden {
        out.push(Change::Cursor { hidden: false });
    }
    out.extend(disables(from, to));
    if to.raw && !from.raw {
        out.push(Change::Termios { raw: true });
    }
    if to.alternate_screen && !from.alternate_screen {
        out.push(Change::AlternateScreen(true));
    }
    if to.keyboard.is_some() && to.keyboard != from.keyboard {
        out.push(Change::Keyboard(to.keyboard));
    }
    if to.bracketed_paste && !from.bracketed_paste {
        out.push(Change::BracketedPaste(true));
    }
    if to.focus_reports && !from.focus_reports {
        out.push(Change::FocusReports(true));
    }
    if to.mouse && !from.mouse {
        out.push(Change::Mouse(true));
    }
    if let Some(back) = to.alternate_scroll_off
        && from.alternate_scroll_off.is_none()
    {
        out.push(Change::AlternateScrollOff(back));
    }
    if to.cursor_hidden && !from.cursor_hidden {
        out.push(Change::Cursor { hidden: true });
    }
    out
}

pub trait Port {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn termios(&mut self, raw: bool) -> io::Result<()>;
}

#[derive(Debug)]
pub struct Failure {
    pub change: Change,
    pub error: io::Error,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.change.name(), self.error)
    }
}

pub fn execute(port: &mut impl Port, changes: &[Change], state: &mut Modes) -> Vec<Failure> {
    execute_until(port, changes, state, || false).unwrap_or_default()
}

pub fn execute_until(
    port: &mut impl Port,
    changes: &[Change],
    state: &mut Modes,
    dying: impl Fn() -> bool,
) -> Result<Vec<Failure>, Halted> {
    let mut failures = vec![];
    for change in changes {
        if dying() {
            return Err(Halted::Dying);
        }
        let result = match change.bytes() {
            Some(bytes) => port.write(bytes.as_bytes()),
            None => match change {
                Change::Termios { raw } => port.termios(*raw),
                _ => Ok(()),
            },
        };
        let ok = result.is_ok();
        if ok || change.enables() {
            change.record(state);
        }
        if let Err(error) = result {
            failures.push(Failure {
                change: *change,
                error,
            });
        }
    }
    Ok(failures)
}

#[cfg(test)]
mod tests;
