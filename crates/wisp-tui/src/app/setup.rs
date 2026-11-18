use std::io;
use std::time::Duration;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Rect, Size};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use wisp_protocol::ChatRow;
use wisp_protocol::subscription_auth::{AuthEvent, Way};
use wisp_protocol::{AgentEvent, Billing, ClientCommand, RequestId};
use wisp_render::chrome::account::{
    RAN_ON_IT, Said, SaidWords, WHERE_CONTINUE, WHERE_START, unmarked,
};
use wisp_render::chrome::endpoint_card::{banner, endpoint_card_rows};
use wisp_render::chrome::{CardHit, CardRows, ChatsPlace, Rebuild, chats_lines};
use wisp_render::markdown::Layout;
use wisp_render::styled::StyledLine;
use wisp_render::theme::Theme;
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::copy;

use super::chats::{Act, Chats};
use super::endpoint_card::{Answered, CardMode, CardSetup, EndpointCard, SaveRequest, Step};
use super::logout::{Intent, Leaving, SignOut, WayRow, way_rows};
use super::presenter::{
    AGENT_STOPPED, AppError, Exit, Finished, MAX_RESTORES, Options, halted_of, paint_lines,
    run_to_end, terminal_copy,
};
use super::resumed::StartNote;
use super::settings::Tracker;
use super::subscription_login::sign_in::{Reaction, SignIn, Wire};
use super::subscription_login::{self as login, Route, Wizard};
use super::{Session, SessionFacts};
use crate::term::input::{HUNG_UP, typed};
use crate::term::screen::{Link, Painter};
use crate::term::{Console, Halted, Input, Key, KeyCode, Mouse, MouseKind, Written};
use wisp_client::QuitBound;

pub const SAVE_WAIT: Duration = Duration::from_secs(85);

pub const SETUP_CANCELLED: &str = "Nothing was set up.";
pub const SAVE_ABANDONED: &str = "wisp: an account save was still running; it may not be saved.";

pub fn setup_saved(name: &str) -> String {
    format!("Saved \"{name}\". wisp starts on it next time.")
}

pub struct SetupChannels {
    pub events: mpsc::Receiver<AgentEvent>,
    pub commands: mpsc::Sender<ClientCommand>,
}

pub struct SessionStart {
    pub session: Session,
    pub started: Option<StartNote>,
    pub warnings: Vec<String>,
    pub quit: Option<QuitBound>,
}

pub struct Ask {
    pub lead: Said,
    pub resumed: bool,
    pub from: Option<Billing>,
    pub ways: Vec<Way>,
    pub facts: SessionFacts,
    pub size: Option<u64>,
    pub back: Option<(Vec<ChatRow>, String)>,
    pub again: Option<String>,
}

pub enum Opening {
    Session {
        session: Box<Session>,
        card: CardSetup,
        notes: Vec<String>,
        started: Option<StartNote>,
    },
    FirstRun {
        setup: SetupChannels,
        card: CardSetup,
        start: oneshot::Receiver<Result<SessionStart, String>>,
    },
    Choose {
        setup: SetupChannels,
        card: CardSetup,
        start: oneshot::Receiver<Result<SessionStart, String>>,
        ask: Box<Ask>,
    },
    Resume {
        chats: Vec<ChatRow>,
        focus: Option<String>,
    },
}

pub struct Opened {
    pub unsent: Option<super::presenter::Unsent>,
    pub exit: Result<Exit, AppError>,
    pub goodbye: Vec<StyledLine>,
    pub width: usize,
    pub theme: Theme,
    pub warnings: Vec<String>,
}

enum SetupEnd {
    Start {
        start: Box<SessionStart>,
        file_asked: bool,
    },
    Exit(Exit),
}

struct Screen {
    link: Option<Link>,
    painter: Painter,
    theme: Theme,
    glyphs: GlyphSet,
    restores: u8,
    dirty: bool,
}

enum Took {
    Done(Option<Exit>),
    Key(Key),
    Paste(String),
    Press(u16),
    Closed(String),
}

impl Screen {
    fn new(options: &Options) -> Screen {
        Screen {
            link: None,
            painter: Painter::new(options.full_repaint),
            theme: options.theme,
            glyphs: options.glyphs,
            restores: 0,
            dirty: true,
        }
    }

    fn recover<C: Console>(&mut self, console: &mut C, counted: bool) -> Result<(), AppError> {
        if counted {
            self.restores = self.restores.saturating_add(1);
            if self.restores > MAX_RESTORES {
                return Err(AppError::Panics(self.restores));
            }
        }
        console.reapply()?;
        self.painter.invalidate();
        self.dirty = true;
        Ok(())
    }

    fn halt<C: Console>(
        &mut self,
        console: &mut C,
        e: io::Error,
    ) -> Result<Option<Exit>, AppError> {
        match halted_of(&e) {
            Some(Halted::Restored) => self.recover(console, false).map(|()| None),
            Some(Halted::Dying) => Ok(Some(Exit::Dying)),
            _ => Err(AppError::Io(e)),
        }
    }

    fn take<C: Console>(&mut self, input: Input, console: &mut C) -> Result<Took, AppError> {
        self.dirty = true;
        match input {
            Input::Key(key) => {
                let ctrl = key.ctrl && !key.alt;
                match key.code {
                    KeyCode::Char('l' | 'L') if ctrl => {
                        self.painter.invalidate();
                        Ok(Took::Done(None))
                    }
                    KeyCode::Char('z' | 'Z') if ctrl => match console.suspend() {
                        Ok(()) => Ok(Took::Done(None)),
                        Err(e) => self.halt(console, e).map(Took::Done),
                    },
                    _ => Ok(Took::Key(key)),
                }
            }
            Input::Paste(text) => Ok(Took::Paste(text)),
            Input::Resumed => {
                if console.halted().is_some() {
                    self.recover(console, false)?;
                }
                self.painter.invalidate();
                Ok(Took::Done(None))
            }
            Input::Restored => self.recover(console, true).map(|()| Took::Done(None)),
            Input::Closed(reason) => Ok(Took::Closed(reason)),
            Input::Mouse(Mouse {
                kind: MouseKind::Press,
                row,
                modified: false,
                ..
            }) => Ok(Took::Press(row)),
            Input::Mouse(_) | Input::Resize(_) | Input::Focus(_) | Input::PictureRefused => {
                Ok(Took::Done(None))
            }
        }
    }

    fn draw<C: Console>(
        &mut self,
        console: &mut C,
        lines: impl FnOnce(Layout, usize) -> (Vec<StyledLine>, usize, Option<Link>),
    ) -> Result<Option<Exit>, AppError> {
        if !self.dirty {
            return Ok(None);
        }
        self.dirty = false;
        let size = console.size()?;
        let mut buffer = Buffer::empty(Rect::new(0, 0, size.width, size.height));
        let layout = Layout::new(usize::from(size.width), self.glyphs);
        let room = usize::from(size.height);
        let (lines, skip, link) = lines(layout, room);
        self.link = link;
        let shown = lines.iter().skip(skip).take(room);
        paint_lines(&mut buffer, 0, size.width, shown, &self.theme);
        self.paint(console, &buffer, size)
    }

    fn paint<C: Console>(
        &mut self,
        console: &mut C,
        buffer: &Buffer,
        size: Size,
    ) -> Result<Option<Exit>, AppError> {
        let plan = match self.painter.plan(buffer, self.link.as_ref()) {
            Ok(Some(plan)) => plan,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let started = Instant::now();
        match console.frame(plan.bytes(), size) {
            Ok(Written::Sent) => {
                self.painter.sent(plan, started.elapsed());
                Ok(None)
            }
            Ok(Written::Suspended) => Ok(None),
            Ok(Written::Resized(_)) => {
                self.dirty = true;
                Ok(None)
            }
            Ok(Written::Refused(halted)) => self.halt(console, halted.into()),
            Err(e) => self.halt(console, e),
        }
    }
}

struct Host {
    sign_in: Option<SignIn>,
    hits: Vec<Option<CardHit>>,
    clock: Instant,
    screen: Screen,
    commands: Option<mpsc::Sender<ClientCommand>>,
    requests: u64,
    signed: Vec<String>,
    saved: Option<String>,
    file_asked: bool,
    starting: bool,
    quitting: bool,
    deadline: Option<Instant>,
    input_stopped: Option<String>,
}

fn leaves(key: Key) -> bool {
    let ctrl = key.ctrl && !key.alt;
    matches!(key.code, KeyCode::Esc)
        || (ctrl && matches!(key.code, KeyCode::Char('c' | 'C' | 'd' | 'D')))
}

impl Host {
    fn new(commands: mpsc::Sender<ClientCommand>, options: &Options) -> Host {
        Host {
            sign_in: None,
            hits: Vec::new(),
            clock: Instant::now(),
            screen: Screen::new(options),
            commands: Some(commands),
            requests: 0,
            signed: Vec::new(),
            saved: None,
            file_asked: options.file_asked,
            starting: false,
            quitting: false,
            deadline: None,
            input_stopped: None,
        }
    }

    fn wire(&mut self) -> (Option<&mut SignIn>, Wire<'_>) {
        let wire = Wire {
            requests: &mut self.requests,
            commands: self.commands.as_ref(),
            unsent: false,
        };
        (self.sign_in.as_mut(), wire)
    }

    fn open(&mut self, route: Route, first: Option<&str>) -> Option<Reaction> {
        let (mut wizard, intent) = Wizard::new(route, Vec::new(), RequestId(self.requests)).ok()?;
        if let Some(lead) = first {
            wizard.first_run(lead);
        }
        self.sign_in = Some(SignIn::new(wizard));
        let (sign_in, mut wire) = self.wire();
        sign_in?.act(intent?, &mut wire)
    }

    fn choosing(&self) -> bool {
        self.sign_in.as_ref().is_some_and(|s| s.wizard.choosing())
    }

    fn finishing(&self) -> bool {
        self.sign_in.as_ref().is_some_and(|s| s.wizard.finishing())
    }

    fn tick_at(&self) -> Option<Instant> {
        Some(self.clock + self.sign_in.as_ref()?.wizard.deadline()?)
    }

    fn took(&mut self, took: Took) -> Option<Reaction> {
        let at = self.clock.elapsed();
        let hit = match &took {
            Took::Press(row) => Some(self.hits.get(usize::from(*row)).copied().flatten()?),
            _ => None,
        };
        let (sign_in, mut wire) = self.wire();
        let sign_in = sign_in?;
        match (took, hit) {
            (Took::Key(key), _) => sign_in.key(key, at, &mut wire),
            (Took::Paste(text), _) => {
                sign_in.wizard.paste(&text);
                None
            }
            (Took::Press(_), Some(hit)) => sign_in.click(hit, false, at, &mut wire),
            _ => sign_in.tick(at, &mut wire),
        }
    }

    fn event(&mut self, event: &AgentEvent) -> Option<Reaction> {
        let AgentEvent::SubscriptionAuth {
            event: AuthEvent { request, outcome },
        } = event
        else {
            return None;
        };
        let at = self.clock.elapsed();
        let (sign_in, mut wire) = self.wire();
        sign_in?.event(*request, outcome, at, &mut wire)
    }

    fn start_on(&mut self, account: String, follow: Vec<String>) {
        self.requests += 1;
        let command = ClientCommand::StartOn {
            request: RequestId(self.requests),
            account,
            follow,
        };
        let sent = self.commands.as_ref().map(|c| c.try_send(command).is_ok());
        self.starting |= sent.unwrap_or(false);
    }

    fn signed_in(&mut self, alias: String) {
        let sign_in = self.sign_in.as_ref();
        let glyphs = Glyphs::of(self.screen.glyphs);
        let lines = sign_in.map_or_else(Vec::new, |s| s.wizard.signed_in_lines(None, &glyphs));
        self.signed = lines.iter().map(unmarked).collect();
        self.saved = Some(alias.clone());
        self.start_on(alias, Vec::new());
    }

    fn quit(&mut self, busy: bool, cancelled: Exit) -> Option<Exit> {
        if self.quitting {
            return Some(match &self.saved {
                Some(name) => Exit::SetupSaved {
                    name: name.clone(),
                    forced: true,
                },
                None => Exit::SaveAbandoned,
            });
        }
        self.commands = None;
        if !busy && !self.finishing() && self.saved.is_none() {
            return Some(
                self.input_stopped
                    .take()
                    .map_or(cancelled, Exit::InputStopped),
            );
        }
        self.quitting = true;
        self.deadline = Some(Instant::now() + SAVE_WAIT);
        None
    }

    fn ended(&mut self, cancelled: Exit) -> Option<Exit> {
        if !self.quitting {
            return None;
        }
        Some(match self.saved.take() {
            Some(name) => Exit::SetupSaved {
                name,
                forced: false,
            },
            None => cancelled,
        })
    }

    async fn handed_off(
        &mut self,
        start: oneshot::Receiver<Result<SessionStart, String>>,
    ) -> SetupEnd {
        match start.await {
            Ok(Ok(mut start)) => {
                start
                    .warnings
                    .splice(0..0, std::mem::take(&mut self.signed));
                SetupEnd::Start {
                    start: Box::new(start),
                    file_asked: self.file_asked,
                }
            }
            Ok(Err(e)) => SetupEnd::Exit(Exit::StartFailed(e)),
            Err(_) => SetupEnd::Exit(Exit::Ended),
        }
    }

    fn draw<C: Console>(
        &mut self,
        console: &mut C,
        card: impl FnOnce(usize, Layout) -> CardRows,
    ) -> Result<Option<Exit>, AppError> {
        let Host {
            sign_in,
            hits,
            screen,
            ..
        } = self;
        if let Some(sign_in) = sign_in.as_mut()
            && let Some(text) = sign_in.wizard.take_copy()
        {
            let (outcome, halted) = terminal_copy(console, &text);
            sign_in.wizard.copied(&outcome);
            screen.dirty = true;
            if let Some(e) = halted
                && let Some(exit) = screen.halt(console, e)?
            {
                return Ok(Some(exit));
            }
        }
        screen.draw(console, |layout, room| {
            let top = banner(layout);
            let room = room.saturating_sub(top.len());
            let rows = match sign_in {
                Some(sign_in) => sign_in.wizard.render(room, layout, sign_in.wizard.mark()),
                None => card(room, layout),
            };
            *hits = vec![None; top.len()];
            hits.extend(rows.hits.iter().copied());
            let at = u16::try_from(top.len()).unwrap_or(u16::MAX);
            let link = sign_in
                .as_ref()
                .and_then(|s| s.link(&rows, at, 0..rows.lines.len()));
            (top.into_iter().chain(rows.lines).collect(), 0, link)
        })
    }
}

trait Hosted {
    fn host(&mut self) -> &mut Host;
    fn busy(&self) -> bool;
    fn cancelled(&self) -> Exit;
    fn reacted(&mut self, reaction: Reaction, rows: bool) -> Option<Exit>;
    fn card(&mut self, took: Took) -> Option<Exit>;
    fn answer(&mut self, event: &AgentEvent);
    fn draw<C: Console>(&mut self, console: &mut C) -> Result<Option<Exit>, AppError>;
}

fn quit<L: Hosted>(lp: &mut L) -> Option<Exit> {
    let (busy, cancelled) = (lp.busy(), lp.cancelled());
    lp.host().quit(busy, cancelled)
}

fn input<L: Hosted>(lp: &mut L, took: Took) -> Option<Exit> {
    let host = lp.host();
    let leaving = matches!(took, Took::Key(key) if leaves(key));
    match took {
        Took::Done(exit) => exit,
        Took::Closed(reason) => {
            host.input_stopped.get_or_insert(reason);
            quit(lp)
        }
        _ if leaving && (host.starting || host.quitting || host.finishing()) => quit(lp),
        _ if host.starting => None,
        took if host.sign_in.is_some() => {
            let rows = host.choosing();
            let reaction = host.took(took)?;
            lp.reacted(reaction, rows)
        }
        took => lp.card(took),
    }
}

async fn hosted<L: Hosted, C: Console>(
    lp: &mut L,
    console: &mut C,
    inputs: &mut mpsc::UnboundedReceiver<Input>,
    events: &mut mpsc::Receiver<AgentEvent>,
    start: oneshot::Receiver<Result<SessionStart, String>>,
) -> Result<SetupEnd, AppError> {
    let mut inputs_open = true;
    loop {
        if let Some(exit) = lp.draw(console)? {
            return Ok(SetupEnd::Exit(exit));
        }
        let wait = until(lp.host().deadline);
        let tick = until(lp.host().tick_at());
        let exit = tokio::select! {
            biased;
            input = inputs.recv(), if inputs_open => {
                let took = match input {
                    Some(input) => lp.host().screen.take(input, console)?,
                    None => {
                        inputs_open = false;
                        Took::Closed(HUNG_UP.to_owned())
                    }
                };
                self::input(lp, took)
            }
            event = events.recv() => match event {
                Some(event) => {
                    let host = lp.host();
                    host.screen.dirty = true;
                    if host.sign_in.is_some() {
                        let reaction = host.event(&event);
                        reaction.and_then(|r| lp.reacted(r, false));
                    } else {
                        lp.answer(&event);
                    }
                    None
                }
                None => {
                    let cancelled = lp.cancelled();
                    return Ok(match lp.host().ended(cancelled) {
                        Some(exit) => SetupEnd::Exit(exit),
                        None => lp.host().handed_off(start).await,
                    });
                }
            },
            () = wait => Some(quit(lp).unwrap_or(Exit::SaveAbandoned)),
            () = tick => {
                let host = lp.host();
                host.screen.dirty = true;
                let rows = host.choosing();
                host.took(Took::Done(None)).and_then(|r| lp.reacted(r, rows))
            }
        };
        if let Some(exit) = exit {
            return Ok(SetupEnd::Exit(exit));
        }
    }
}

struct SetupLoop {
    card: EndpointCard,
    host: Host,
    notes: Vec<String>,
}

impl SetupLoop {
    fn new(card: CardSetup, commands: mpsc::Sender<ClientCommand>, options: &Options) -> SetupLoop {
        let mut lp = SetupLoop {
            card: EndpointCard::new(CardMode::FirstRun, card, Vec::new()),
            host: Host::new(commands, options),
            notes: Vec::new(),
        };
        lp.rows();
        lp
    }

    fn rows(&mut self) {
        let route = Route::Login {
            provider: None,
            alias: None,
            existing_alias: false,
        };
        let lead = self.card.setup.lead.clone().unwrap_or_default();
        self.host.open(route, Some(&lead));
    }

    fn send(&mut self, save: SaveRequest) {
        self.host.requests += 1;
        let request = RequestId(self.host.requests);
        let command = ClientCommand::SaveEndpoint {
            request,
            endpoint: save.endpoint,
            key: save.key,
            check: save.check,
        };
        let commands = self.host.commands.as_ref();
        if commands.is_some_and(|c| c.try_send(command).is_ok()) {
            self.card.sent(request);
        } else {
            self.card.unsent(AGENT_STOPPED);
        }
    }
}

impl Hosted for SetupLoop {
    fn host(&mut self) -> &mut Host {
        &mut self.host
    }

    fn busy(&self) -> bool {
        self.card.saving()
    }

    fn cancelled(&self) -> Exit {
        Exit::SetupCancelled
    }

    fn reacted(&mut self, reaction: Reaction, rows: bool) -> Option<Exit> {
        match reaction {
            Reaction::Close if rows => return quit(self),
            Reaction::Close => self.rows(),
            Reaction::AddKey => self.host.sign_in = None,
            Reaction::Ready { alias, .. } => self.host.signed_in(alias),
            Reaction::Saved(alias) => self.host.saved = Some(alias),
            Reaction::Use(_) => {}
        }
        None
    }

    fn card(&mut self, took: Took) -> Option<Exit> {
        let key = match took {
            Took::Key(key) => key,
            Took::Paste(text) => {
                self.card.paste(&text);
                return None;
            }
            _ => return None,
        };
        match self.card.key(key) {
            Step::Stay | Step::Close | Step::Edit(_) => None,
            Step::Quit
                if key.code == KeyCode::Esc && !self.card.saving() && self.host.saved.is_none() =>
            {
                self.rows();
                None
            }
            Step::Quit => quit(self),
            Step::Send(save) => {
                self.send(save);
                None
            }
        }
    }

    fn answer(&mut self, event: &AgentEvent) {
        let glyphs = self.host.screen.glyphs;
        if let Answered::Saved { name, notes, .. } =
            self.card.answer(event, &mut self.host.file_asked, glyphs)
        {
            self.host.saved = Some(name.clone());
            self.host.start_on(name, Vec::new());
            self.notes.extend(notes);
        }
    }

    fn draw<C: Console>(&mut self, console: &mut C) -> Result<Option<Exit>, AppError> {
        if self.host.sign_in.is_some() {
            return self.host.draw(console, |_, _| CardRows::default());
        }
        let view = self.card.view(self.host.screen.glyphs);
        self.host.screen.draw(console, |layout, room| {
            let rows = endpoint_card_rows(&view, layout);
            let skip = rows.skip(room);
            (rows.lines, skip, None)
        })
    }
}

async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
}

async fn setup_loop<C: Console>(
    console: &mut C,
    inputs: &mut mpsc::UnboundedReceiver<Input>,
    setup: SetupChannels,
    start: oneshot::Receiver<Result<SessionStart, String>>,
    card: CardSetup,
    options: &Options,
    typed_ahead: &[u8],
) -> (Result<SetupEnd, AppError>, Vec<String>) {
    let SetupChannels {
        mut events,
        commands,
    } = setup;
    let mut lp = SetupLoop::new(card, commands, options);
    for input in typed(typed_ahead) {
        if let Input::Key(key) = input
            && let Some(exit) = self::input(&mut lp, Took::Key(key))
        {
            return (Ok(SetupEnd::Exit(exit)), lp.notes);
        }
    }
    let result = hosted(&mut lp, console, inputs, &mut events, start).await;
    (result, lp.notes)
}

async fn resume_loop<C: Console>(
    console: &mut C,
    inputs: &mut mpsc::UnboundedReceiver<Input>,
    mut chats: Chats,
    options: &Options,
) -> Result<Exit, AppError> {
    let mut screen = Screen::new(options);
    let mut ahead = std::collections::VecDeque::from(typed(&options.typed));
    loop {
        if let Some(exit) = screen.draw(console, |layout, room| {
            let view = chats.view(ChatsPlace::Start, Vec::new());
            (chats_lines(&view, room, layout), 0, None)
        })? {
            return Ok(exit);
        }
        let input = match ahead.pop_front() {
            Some(input) => Some(input),
            None => inputs.recv().await,
        };
        let Some(input) = input else {
            return Ok(Exit::InputStopped(HUNG_UP.to_owned()));
        };
        let act = match screen.take(input, console)? {
            Took::Done(Some(exit)) => return Ok(exit),
            Took::Done(None) | Took::Press(_) => Act::Stay,
            Took::Key(key) => chats.key(key),
            Took::Paste(text) => {
                chats.paste(&text);
                Act::Stay
            }
            Took::Closed(reason) => return Ok(Exit::InputStopped(reason)),
        };
        match act {
            Act::Stay => {}
            Act::Close => return Ok(Exit::Quit),
            Act::Pick(id) => return Ok(Exit::Resume(id)),
        }
    }
}

struct ChooseLoop {
    card: SignOut,
    host: Host,
    again: Option<(String, Route)>,
    rebuild: Option<Rebuild>,
}

impl ChooseLoop {
    fn new(ask: &Ask, commands: mpsc::Sender<ClientCommand>, options: &Options) -> ChooseLoop {
        let mut tracker = Tracker::new(&ask.facts);
        if let Some(size) = ask.size {
            tracker.sized(size);
        }
        let from = ask
            .from
            .unwrap_or_else(|| ask.facts.billing_on(&tracker.wanted().endpoint));
        let lead = ask.lead.marked(&Glyphs::of(options.glyphs));
        let mark = ask.lead.mark();
        let (leaving, heading) = if ask.resumed {
            let heading = match ask.again {
                Some(_) => (lead, RAN_ON_IT.to_owned(), mark),
                None => (WHERE_CONTINUE.to_owned(), lead, mark),
            };
            (Leaving::Chat(from), heading)
        } else {
            (Leaving::Start(from), (lead, WHERE_START.to_owned(), mark))
        };
        let mut rows = way_rows(
            &ask.facts,
            &tracker,
            options.glyphs,
            &ask.ways,
            leaving,
            None,
            Duration::ZERO,
        );
        if ask.size.is_none() {
            for row in &mut rows {
                row.price.clear();
                row.rebuild = None;
            }
        }
        let again = ask.again.as_ref().and_then(|name| {
            let endpoints = login::endpoints(&ask.facts.endpoints);
            Some((name.clone(), login::by_name(name, &endpoints).ok()?))
        });
        if let Some((name, _)) = &again {
            let row = WayRow {
                mark: ask.facts.mark_of(name),
                chat: name.clone(),
                again: true,
                ..WayRow::default()
            };
            rows.insert(0, row);
        }
        let esc = copy::esc(if ask.back.is_some() {
            copy::BACK_WORD
        } else {
            copy::QUIT_WORD
        });
        ChooseLoop {
            card: SignOut::gone(Vec::new(), heading, rows, esc),
            host: Host::new(commands, options),
            again,
            rebuild: None,
        }
    }

    fn intent(&mut self, intent: Option<Intent>) -> Option<Exit> {
        match intent? {
            Intent::Close => quit(self),
            Intent::Send {
                chat: Some(account),
                follow,
                ..
            } => {
                if let Some((_, route)) = self.again.as_ref().filter(|(name, _)| *name == account) {
                    let route = route.clone();
                    let reaction = self.host.open(route, None)?;
                    return self.reacted(reaction, false);
                }
                self.rebuild = self.card.rebuild_of(&account);
                self.host.start_on(account, follow);
                None
            }
            Intent::Send { chat: None, .. } => None,
        }
    }
}

impl Hosted for ChooseLoop {
    fn host(&mut self) -> &mut Host {
        &mut self.host
    }

    fn busy(&self) -> bool {
        false
    }

    fn cancelled(&self) -> Exit {
        Exit::Quit
    }

    fn reacted(&mut self, reaction: Reaction, _: bool) -> Option<Exit> {
        match reaction {
            Reaction::Close => self.host.sign_in = None,
            Reaction::Ready { alias, .. } => self.host.signed_in(alias),
            Reaction::Saved(alias) => self.host.saved = Some(alias),
            Reaction::AddKey | Reaction::Use(_) => {}
        }
        None
    }

    fn card(&mut self, took: Took) -> Option<Exit> {
        let intent = match took {
            Took::Key(key) => self.card.key(key),
            Took::Press(row) => {
                let hit = self.host.hits.get(usize::from(row)).copied().flatten();
                hit.and_then(|hit| self.card.click(hit))
            }
            _ => None,
        };
        self.intent(intent)
    }

    fn answer(&mut self, _: &AgentEvent) {}

    fn draw<C: Console>(&mut self, console: &mut C) -> Result<Option<Exit>, AppError> {
        let card = &mut self.card;
        let exit = self
            .host
            .draw(console, |room, layout| card.render(room, layout));
        if self.host.sign_in.is_none() {
            self.card.drawn = true;
        }
        exit
    }
}

async fn choose_loop<C: Console>(
    console: &mut C,
    inputs: &mut mpsc::UnboundedReceiver<Input>,
    setup: SetupChannels,
    start: oneshot::Receiver<Result<SessionStart, String>>,
    ask: &Ask,
    options: &Options,
) -> (Result<SetupEnd, AppError>, Option<Rebuild>) {
    let SetupChannels {
        mut events,
        commands,
    } = setup;
    let mut lp = ChooseLoop::new(ask, commands, options);
    let result = hosted(&mut lp, console, inputs, &mut events, start).await;
    (result, lp.rebuild)
}

fn started(start: SessionStart, card: CardSetup, options: &mut Options) -> Session {
    let SessionStart {
        session,
        started,
        warnings,
        quit,
    } = start;
    options.quit = quit;
    options.started = started;
    options.card_setup = Some(card);
    options.notes.extend(warnings);
    session
}

fn ended_early(exit: Result<Exit, AppError>, warnings: Vec<String>, theme: Theme) -> Opened {
    Opened {
        unsent: None,
        exit,
        goodbye: vec![],
        width: 0,
        theme,
        warnings,
    }
}

pub async fn open<C: Console>(
    console: &mut C,
    mut inputs: mpsc::UnboundedReceiver<Input>,
    opening: Opening,
    mut options: Options,
) -> Opened {
    let (session, warnings) = match opening {
        Opening::Session {
            session,
            card,
            notes,
            started,
        } => {
            options.card_setup = Some(card);
            options.started = started;
            options.notes.extend(notes);
            (*session, Vec::new())
        }
        Opening::FirstRun { setup, card, start } => {
            let typed_ahead = std::mem::take(&mut options.typed);
            let (end, notes) = setup_loop(
                console,
                &mut inputs,
                setup,
                start,
                card.clone(),
                &options,
                &typed_ahead,
            )
            .await;
            match end {
                Ok(SetupEnd::Start { start, file_asked }) => {
                    options.file_asked = file_asked;
                    (started(*start, card, &mut options), notes)
                }
                Ok(SetupEnd::Exit(exit)) => return ended_early(Ok(exit), notes, options.theme),
                Err(e) => return ended_early(Err(e), notes, options.theme),
            }
        }
        Opening::Choose {
            setup,
            card,
            start,
            ask,
        } => {
            let (end, rebuild) =
                choose_loop(console, &mut inputs, setup, start, &ask, &options).await;
            match end {
                Ok(SetupEnd::Start { start, .. }) => {
                    let mut session = started(*start, card, &mut options);
                    let resumed = session.resumed.as_mut();
                    if let Some(line) = resumed.and_then(|r| r.continuing.as_mut()) {
                        line.rebuild = rebuild.or(line.rebuild);
                    }
                    (session, Vec::new())
                }
                Ok(SetupEnd::Exit(exit)) => {
                    let exit = match (exit, ask.back) {
                        (Exit::Quit, Some((chats, focus))) => {
                            let chats = Chats::listed(chats, Some(&focus));
                            resume_loop(console, &mut inputs, chats, &options).await
                        }
                        (exit, _) => Ok(exit),
                    };
                    return ended_early(exit, Vec::new(), options.theme);
                }
                Err(e) => return ended_early(Err(e), Vec::new(), options.theme),
            }
        }
        Opening::Resume { chats, focus } => {
            let chats = Chats::listed(chats, focus.as_deref());
            let exit = resume_loop(console, &mut inputs, chats, &options).await;
            return ended_early(exit, Vec::new(), options.theme);
        }
    };
    let Finished {
        unsent,
        exit,
        goodbye,
        width,
        theme,
        unshown,
    } = run_to_end(console, inputs, session, options).await;
    let mut warnings = warnings;
    warnings.extend(unshown);
    Opened {
        unsent,
        exit,
        goodbye,
        width,
        theme,
        warnings,
    }
}
