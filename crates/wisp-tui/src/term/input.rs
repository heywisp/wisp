use std::cell::Cell;
use std::io::{self, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode as CtKey, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui_core::layout::Size;
use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use signal_hook::SigId;
use signal_hook::consts::SIGWINCH;

use super::contain::contain;
use super::picture::refused;
use super::probe::{self, LateReplies, PASTE_END, PASTE_START};
use super::tty::{read_ready, timespec};

pub const POLL: Duration = Duration::from_millis(20);
pub const DRAIN: Duration = Duration::from_millis(1);
pub const STOP_PATIENCE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Esc,
    F(u8),
    Insert,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    pub const fn plain(code: KeyCode) -> Key {
        Key {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub const fn ctrl(c: char) -> Key {
        Key {
            code: KeyCode::Char(c),
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    pub const fn alt(code: KeyCode) -> Key {
        Key {
            code,
            ctrl: false,
            alt: true,
            shift: false,
        }
    }

    pub const fn shift(code: KeyCode) -> Key {
        Key {
            code,
            ctrl: false,
            alt: false,
            shift: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseKind {
    WheelUp,
    WheelDown,
    Press,
    Drag,
    Release,
    RightPress,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mouse {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
    pub modified: bool,
}

pub fn mouse_of(event: MouseEvent) -> Option<Mouse> {
    let kind = match event.kind {
        MouseEventKind::Moved => MouseKind::Move,
        MouseEventKind::ScrollUp => MouseKind::WheelUp,
        MouseEventKind::ScrollDown => MouseKind::WheelDown,
        MouseEventKind::Down(MouseButton::Left) => MouseKind::Press,
        MouseEventKind::Drag(MouseButton::Left) => MouseKind::Drag,
        MouseEventKind::Up(MouseButton::Left) => MouseKind::Release,
        MouseEventKind::Down(MouseButton::Right) => MouseKind::RightPress,
        _ => return None,
    };
    Some(Mouse {
        kind,
        column: event.column,
        row: event.row,
        modified: event
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Key(Key),
    Mouse(Mouse),
    Paste(String),
    Resize(Size),
    Focus(bool),
    PictureRefused,
    Resumed,
    Restored,
    Closed(String),
}

fn code(key: CtKey) -> KeyCode {
    match key {
        CtKey::Char(c) => KeyCode::Char(c),
        CtKey::Enter => KeyCode::Enter,
        CtKey::Tab => KeyCode::Tab,
        CtKey::BackTab => KeyCode::BackTab,
        CtKey::Backspace => KeyCode::Backspace,
        CtKey::Delete => KeyCode::Delete,
        CtKey::Left => KeyCode::Left,
        CtKey::Right => KeyCode::Right,
        CtKey::Up => KeyCode::Up,
        CtKey::Down => KeyCode::Down,
        CtKey::Home => KeyCode::Home,
        CtKey::End => KeyCode::End,
        CtKey::PageUp => KeyCode::PageUp,
        CtKey::PageDown => KeyCode::PageDown,
        CtKey::Esc => KeyCode::Esc,
        CtKey::F(n) => KeyCode::F(n),
        CtKey::Insert => KeyCode::Insert,
        _ => KeyCode::Other,
    }
}

pub fn key_of(event: KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let mods = event.modifiers;
    Some(Key {
        code: code(event.code),
        ctrl: mods.contains(KeyModifiers::CONTROL),
        alt: mods.contains(KeyModifiers::ALT),
        shift: mods.contains(KeyModifiers::SHIFT),
    })
}

pub fn convert(event: Event) -> Option<Input> {
    match event {
        Event::Key(key) => key_of(key).map(Input::Key),
        Event::Paste(text) => Some(Input::Paste(text)),
        Event::Resize(width, height) => Some(Input::Resize(Size { width, height })),
        Event::FocusGained => Some(Input::Focus(true)),
        Event::FocusLost => Some(Input::Focus(false)),
        Event::Mouse(mouse) => mouse_of(mouse).map(Input::Mouse),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    More,
    Final,
}

enum Step {
    Take(Option<Input>, usize),
    Need(Option<Input>),
}

fn key(code: KeyCode) -> Step {
    Step::Take(Some(Input::Key(Key::plain(code))), 1)
}

fn ctrl(c: u8) -> Step {
    Step::Take(Some(Input::Key(Key::ctrl(char::from(c)))), 1)
}

fn char_at(b: &[u8]) -> Step {
    let head = &b[..b.len().min(4)];
    let valid = match std::str::from_utf8(head) {
        Ok(s) => s,
        Err(e) if e.valid_up_to() > 0 => {
            std::str::from_utf8(&head[..e.valid_up_to()]).unwrap_or("")
        }
        Err(e) => {
            let replaced = Some(Input::Key(Key::plain(KeyCode::Char('\u{fffd}'))));
            return match e.error_len() {
                Some(n) => Step::Take(replaced, n),
                None => Step::Need(replaced),
            };
        }
    };
    match valid.chars().next() {
        Some(c) => Step::Take(
            Some(Input::Key(Key {
                shift: c.is_uppercase(),
                ..Key::plain(KeyCode::Char(c))
            })),
            c.len_utf8(),
        ),
        None => Step::Need(None),
    }
}

fn key_at(b: &[u8]) -> Step {
    match b[0] {
        0x1b => escape(b),
        b'\r' => key(KeyCode::Enter),
        b'\t' => key(KeyCode::Tab),
        0x7f => key(KeyCode::Backspace),
        0x00 => ctrl(b' '),
        c @ 0x01..=0x1a => ctrl(c - 0x01 + b'a'),
        c @ 0x1c..=0x1f => ctrl(c - 0x1c + b'4'),
        _ => char_at(b),
    }
}

fn escape(b: &[u8]) -> Step {
    let esc = Some(Input::Key(Key::plain(KeyCode::Esc)));
    match b.get(1) {
        None => Step::Take(esc, 1),
        Some(0x1b) => Step::Take(esc, 2),
        Some(b'[') => csi(b),
        Some(b'O') => match b.get(2) {
            None => Step::Need(None),
            Some(f) => match char_at(&b[2..]) {
                Step::Take(_, n) => {
                    Step::Take(ss3(*f).map(|code| Input::Key(Key::plain(code))), 2 + n)
                }
                Step::Need(_) => Step::Need(None),
            },
        },
        Some(b']' | b'P' | b'_' | b'^' | b'X') => string(b),
        Some(_) => match key_at(&b[1..]) {
            Step::Take(input, n) => Step::Take(
                input.map(|input| match input {
                    Input::Key(k) => Input::Key(Key { alt: true, ..k }),
                    other => other,
                }),
                n + 1,
            ),
            Step::Need(_) => Step::Need(None),
        },
    }
}

fn ss3(f: u8) -> Option<KeyCode> {
    Some(match f {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        f @ b'P'..=b'S' => KeyCode::F(1 + f - b'P'),
        _ => return None,
    })
}

fn csi(b: &[u8]) -> Step {
    let Some(last) = b[2..].iter().position(|c| (0x40..=0x7e).contains(c)) else {
        return Step::Need(None);
    };
    let last = last + 2;
    if b[..=last] == *PASTE_START {
        let body = &b[last + 1..];
        let text = |body: &[u8]| Some(Input::Paste(String::from_utf8_lossy(body).into_owned()));
        return match probe::find(body, PASTE_END) {
            Some(n) => Step::Take(text(&body[..n]), last + 1 + n + PASTE_END.len()),
            None => Step::Need(text(body)),
        };
    }
    if b[2] == b'<' && matches!(b[last], b'M' | b'm') {
        return Step::Take(sgr_mouse(&b[3..last], b[last] == b'm'), last + 1);
    }
    let code = match (&b[2..last], b[last]) {
        (b"", b'A') => KeyCode::Up,
        (b"", b'B') => KeyCode::Down,
        (b"", b'C') => KeyCode::Right,
        (b"", b'D') => KeyCode::Left,
        (b"", b'H') | (b"1" | b"7", b'~') => KeyCode::Home,
        (b"", b'F') | (b"4" | b"8", b'~') => KeyCode::End,
        (b"", b'I' | b'O') => return Step::Take(Some(Input::Focus(b[last] == b'I')), last + 1),
        (b"", b'M') if b.len() < last + 4 => return Step::Need(None),
        (b"", b'M') => return Step::Take(None, last + 4),
        (b"", b'Z') => {
            return Step::Take(Some(Input::Key(Key::shift(KeyCode::BackTab))), last + 1);
        }
        (b"2", b'~') => KeyCode::Insert,
        (b"3", b'~') => KeyCode::Delete,
        (b"5", b'~') => KeyCode::PageUp,
        (b"6", b'~') => KeyCode::PageDown,
        (b"24", b'~') => KeyCode::F(12),
        _ => return Step::Take(None, last + 1),
    };
    Step::Take(Some(Input::Key(Key::plain(code))), last + 1)
}

fn sgr_mouse(params: &[u8], released: bool) -> Option<Input> {
    let text = std::str::from_utf8(params).ok()?;
    let mut fields = text.split(';');
    let cb: u8 = fields.next()?.parse().ok()?;
    let column = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let row = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let button = (cb & 0b11) | ((cb & 0b1100_0000) >> 4);
    let kind = match (button, cb & 0b10_0000 != 0, released) {
        (0, false, false) => MouseEventKind::Down(MouseButton::Left),
        (0, false, true) | (3, false, _) => MouseEventKind::Up(MouseButton::Left),
        (0, true, _) => MouseEventKind::Drag(MouseButton::Left),
        (3, true, false) => MouseEventKind::Moved,
        (2, false, false) => MouseEventKind::Down(MouseButton::Right),
        (4, false, _) => MouseEventKind::ScrollUp,
        (5, false, _) => MouseEventKind::ScrollDown,
        _ => return None,
    };
    let mut modifiers = KeyModifiers::NONE;
    for (bit, modifier) in [
        (0b100, KeyModifiers::SHIFT),
        (0b1000, KeyModifiers::ALT),
        (0b1_0000, KeyModifiers::CONTROL),
    ] {
        if cb & bit != 0 {
            modifiers |= modifier;
        }
    }
    mouse_of(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
    .map(Input::Mouse)
}

fn string(b: &[u8]) -> Step {
    let mut i = 2;
    while let Some(&c) = b.get(i) {
        i += 1;
        if c == 0x07 {
            return Step::Take(None, i);
        }
        if c == 0x1b {
            let reply = b.get(3..i - 1).map(String::from_utf8_lossy);
            let picture = b.get(1..3) == Some(b"_G") && reply.is_some_and(|r| refused(&r));
            match b.get(i) {
                Some(b'\\') => return Step::Take(picture.then_some(Input::PictureRefused), i + 1),
                None => return Step::Need(None),
                Some(_) => {}
            }
        }
    }
    Step::Need(None)
}

pub fn keys(bytes: &[u8], end: End) -> (Vec<Input>, usize) {
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        match key_at(&bytes[i..]) {
            Step::Take(input, n) => {
                out.extend(input);
                i += n;
            }
            Step::Need(input) => {
                if end == End::More {
                    return (out, i);
                }
                out.extend(input);
                return (out, bytes.len());
            }
        }
    }
    (out, i)
}

pub fn typed(bytes: &[u8]) -> Vec<Input> {
    keys(bytes, End::Final)
        .0
        .into_iter()
        .flat_map(|input| match input {
            Input::Paste(text) => typed(text.as_bytes()),
            Input::Key(Key {
                code: KeyCode::Char(c),
                ctrl: false,
                alt: false,
                ..
            }) if !c.is_control() => vec![Input::Key(Key::plain(KeyCode::Char(c)))],
            Input::Key(Key {
                code: KeyCode::Tab,
                ctrl: false,
                alt: false,
                ..
            }) => vec![Input::Key(Key::plain(KeyCode::Tab))],
            _ => vec![],
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    Run,
    Pause,
    Stop,
}

struct Control {
    want: Want,
    paused: bool,
    finished: bool,
}

type Shared = Arc<(Mutex<Control>, Condvar)>;

fn lock(m: &Mutex<Control>) -> MutexGuard<'_, Control> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct Reader {
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

fn gate(shared: &Shared) -> bool {
    let (m, cv) = &**shared;
    let mut c = lock(m);
    loop {
        match c.want {
            Want::Stop => return false,
            Want::Run => {
                c.paused = false;
                return true;
            }
            Want::Pause => {
                if !c.paused {
                    c.paused = true;
                    cv.notify_all();
                }
                c = cv.wait(c).unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    wisp_contain::message(payload).map_or_else(
        || "the input reader panicked".to_owned(),
        |text| format!("the input reader panicked: {text}"),
    )
}

#[derive(Default)]
enum Apc {
    #[default]
    Idle,
    Opened(Event),
    Body(String),
}

const APC_MAX: usize = 512;

impl Apc {
    fn step(&mut self, event: Option<Event>) -> Vec<Input> {
        let key = match &event {
            Some(Event::Key(k)) => Some((k.code, k.modifiers.contains(KeyModifiers::ALT))),
            _ => None,
        };
        match (std::mem::take(self), key) {
            (Apc::Idle, Some((CtKey::Char('_'), true))) => {
                *self = event.map_or(Apc::Idle, Apc::Opened);
                vec![]
            }
            (Apc::Opened(_), Some((CtKey::Char('G'), false))) => {
                *self = Apc::Body(String::new());
                vec![]
            }
            (Apc::Opened(held), _) => [Some(held), event]
                .into_iter()
                .flatten()
                .filter_map(convert)
                .collect(),
            (Apc::Body(reply), Some((CtKey::Char('\\'), true) | (CtKey::Esc, _))) => {
                refused(&reply)
                    .then_some(Input::PictureRefused)
                    .into_iter()
                    .collect()
            }
            (Apc::Body(mut reply), Some((CtKey::Char(c), _))) if reply.len() < APC_MAX => {
                reply.push(c);
                *self = Apc::Body(reply);
                vec![]
            }
            (Apc::Body(reply), None) if event.is_none() => {
                *self = Apc::Body(reply);
                vec![]
            }
            (Apc::Body(_), None) | (Apc::Idle, _) => event.and_then(convert).into_iter().collect(),
            (Apc::Body(_), _) => vec![],
        }
    }
}

fn read_loop(
    shared: &Shared,
    mut next: impl FnMut() -> io::Result<Option<Event>>,
    send: &dyn Fn(Input) -> bool,
) {
    let ended = contain(|| {
        let mut apc = Apc::default();
        while gate(shared) {
            match next() {
                Ok(event) => {
                    if !apc.step(event).into_iter().all(send) {
                        return None;
                    }
                }
                Err(e) => return Some(e.to_string()),
            }
        }
        None
    });
    let reason = match ended {
        Ok(reason) => reason,
        Err(payload) => Some(panic_text(payload.as_ref())),
    };
    if let Some(reason) = reason {
        send(Input::Closed(reason));
    }
}

pub const HUNG_UP: &str = "the terminal hung up";

fn hung_up() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, HUNG_UP)
}

pub(crate) fn has_hung_up(fd: impl AsFd) -> bool {
    check_hangup(fd).is_err_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
}

fn gone(revents: PollFlags) -> bool {
    revents.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL)
}

fn check_hangup(fd: impl AsFd) -> io::Result<()> {
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    match poll(&mut fds, Some(&timespec(Duration::ZERO))) {
        Ok(_) => {}
        Err(Errno::INTR) => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    if gone(fds[0].revents()) {
        Err(hung_up())
    } else {
        Ok(())
    }
}

struct Watch {
    tty: OwnedFd,
    winch: UnixStream,
    sender: Option<UnixStream>,
    registered: Option<SigId>,
    pending: bool,
}

impl Watch {
    fn open() -> io::Result<Watch> {
        let tty = input_fd()?;
        let (winch, sender) = UnixStream::pair()?;
        winch.set_nonblocking(true)?;
        sender.set_nonblocking(true)?;
        Ok(Watch {
            tty,
            winch,
            sender: Some(sender),
            registered: None,
            pending: false,
        })
    }

    fn settle(
        &mut self,
        shared: &Shared,
        late: &mut LateReplies,
        send: &dyn Fn(Input) -> bool,
    ) -> io::Result<()> {
        let size = crossterm::terminal::size().ok();
        let mut held = vec![];
        let read_ended_on_esc = Cell::new(false);
        let esc_ended_last_read = Cell::new(false);
        probe::drain(
            |buf, wait| {
                let n = read_ready(&self.tty, buf, wait)?;
                if n > 0 {
                    esc_ended_last_read.set(read_ended_on_esc.replace(buf[..n].ends_with(b"\x1b")));
                }
                Ok(n)
            },
            late,
            |bytes| {
                let alone = !bytes.is_empty()
                    && esc_ended_last_read.take()
                    && held.is_empty()
                    && bytes[0] == 0x1b
                    && !matches!(bytes.get(1), Some(b'[' | b']'));
                if alone && !send(Input::Key(Key::plain(KeyCode::Esc))) {
                    return false;
                }
                held.extend(&bytes[usize::from(alone)..]);
                let (inputs, used) = keys(&held, End::More);
                held.drain(..used);
                inputs.into_iter().all(send) && gate(shared)
            },
        )?;
        if !gate(shared) {
            return Ok(());
        }
        let mut buf = [0u8; 1024];
        while held.starts_with(PASTE_START)
            && probe::find(&held, PASTE_END).is_none()
            && gate(shared)
        {
            let n = read_ready(&self.tty, &mut buf, POLL)?;
            if n == 0 && has_hung_up(&self.tty) {
                break;
            }
            held.extend(&buf[..n]);
        }
        for input in keys(&held, End::Final).0 {
            send(input);
        }
        event::poll(Duration::ZERO)?;
        if let Some(sender) = self.sender.take() {
            self.registered = Some(signal_hook::low_level::pipe::register(SIGWINCH, sender)?);
        }
        if let Ok((width, height)) = crossterm::terminal::size()
            && size != Some((width, height))
        {
            send(Input::Resize(Size { width, height }));
        }
        Ok(())
    }

    fn read(&mut self) -> io::Result<Option<Event>> {
        if event::poll(DRAIN)? {
            self.pending = true;
            event::read().map(Some)
        } else {
            self.pending = false;
            Ok(None)
        }
    }

    fn next(&mut self) -> io::Result<Option<Event>> {
        if self.pending {
            return self.read();
        }
        let mut fds = [
            PollFd::new(&self.tty, PollFlags::IN),
            PollFd::new(&self.winch, PollFlags::IN),
        ];
        match poll(&mut fds, Some(&timespec(POLL))) {
            Ok(_) => {}
            Err(Errno::INTR) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let tty = fds[0].revents();
        let winch = fds[1].revents();
        if gone(tty) {
            return Err(hung_up());
        }
        if winch.contains(PollFlags::IN) {
            let mut drained = [0u8; 64];
            while matches!((&self.winch).read(&mut drained), Ok(n) if n > 0) {}
        }
        if tty.contains(PollFlags::IN) || winch.contains(PollFlags::IN) {
            self.read()
        } else {
            Ok(None)
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(registered) = self.registered.take() {
            signal_hook::low_level::unregister(registered);
        }
    }
}

fn input_fd() -> io::Result<OwnedFd> {
    let stdin = io::stdin();
    if rustix::termios::isatty(&stdin) {
        Ok(rustix::io::dup(stdin.as_fd())?)
    } else {
        Ok(std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")?
            .into())
    }
}

fn finish(shared: &Shared) {
    let (m, cv) = &**shared;
    lock(m).finished = true;
    cv.notify_all();
}

impl Reader {
    pub fn spawn(
        late: LateReplies,
        send: impl Fn(Input) -> bool + Send + 'static,
    ) -> io::Result<Reader> {
        drop(crossterm::terminal::enable_raw_mode());
        let shared: Shared = Arc::new((
            Mutex::new(Control {
                want: Want::Run,
                paused: false,
                finished: false,
            }),
            Condvar::new(),
        ));
        let inner = Arc::clone(&shared);
        let opened = Watch::open();
        let thread = std::thread::Builder::new()
            .name("wisp-tui-input".to_owned())
            .spawn(move || {
                let mut watched = opened;
                let mut late = Some(late);
                let (shared, sender) = (&inner, &send);
                let next = move || match &mut watched {
                    Ok(watch) => match late.take() {
                        Some(mut late) => watch.settle(shared, &mut late, sender).map(|()| None),
                        None => watch.next(),
                    },
                    Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
                };
                read_loop(&inner, next, &send);
                finish(&inner);
            })?;
        Ok(Reader {
            shared,
            thread: Some(thread),
        })
    }

    pub fn pause(&self, patience: Duration) -> bool {
        let (m, cv) = &*self.shared;
        let deadline = Instant::now() + patience;
        let mut c = lock(m);
        if c.want == Want::Stop {
            return true;
        }
        c.want = Want::Pause;
        cv.notify_all();
        while !c.paused {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.thread.as_ref().is_none_or(JoinHandle::is_finished) {
                return c.paused || self.thread.as_ref().is_none_or(JoinHandle::is_finished);
            }
            c = cv
                .wait_timeout(c, left)
                .map(|(c, _)| c)
                .unwrap_or_else(|e| e.into_inner().0);
        }
        true
    }

    pub fn resume(&self) {
        let (m, cv) = &*self.shared;
        let mut c = lock(m);
        if c.want == Want::Pause {
            c.want = Want::Run;
        }
        cv.notify_all();
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        {
            let (m, cv) = &*self.shared;
            lock(m).want = Want::Stop;
            cv.notify_all();
        }
        let (m, cv) = &*self.shared;
        let deadline = Instant::now() + STOP_PATIENCE;
        let mut c = lock(m);
        while !c.finished {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            c = cv
                .wait_timeout(c, left)
                .map(|(c, _)| c)
                .unwrap_or_else(|e| e.into_inner().0);
        }
        drop(c);
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}

#[cfg(test)]
mod tests;
