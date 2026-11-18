use std::time::Duration;

use ratatui_core::buffer::Buffer;
use wisp_protocol::agents::NO_SUCH_AGENT;
use wisp_protocol::{
    AgentEvent, AgentPath, AttachInput, AttachItems, AttachKey, AttachKeyCode, ClientCommand,
    DetachReason, RejectKind, SecretRequestEnd, TermSize,
};
use wisp_render::chrome::{
    KeyHint, ProcessRow, PsView, ScreenMode, ScreenView, SecretView, StepInView, ps_next_change,
    ps_rows, screen_inner, screen_rows, step_in_rows,
};
use wisp_render::markdown::Layout;
use wisp_render::styled::StyledLine;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::copy;

use super::{AppError, Presenter, paint_lines};
use crate::app::keymap::take_over_key;
use crate::app::list::Picked;
use crate::app::list::{Confirm, ListCursor};
use crate::app::processes::{
    Key as ProcessKey, LINE_LIMIT, List, MAX_PASTE, MIN_COLS, MIN_ROWS, Opened, StepKind,
    told_on_exit,
};
use crate::app::question::Settle;
use crate::term::{Console, Key, KeyCode};

const NO_CODE_FOR: Duration = Duration::from_secs(2);
const LONG_LINE_FOR: Duration = Duration::from_secs(3);

fn stops_turn(key: Key) -> bool {
    !key.alt
        && ((!key.ctrl && key.code == KeyCode::Esc)
            || (key.ctrl && matches!(key.code, KeyCode::Char('c' | 'C'))))
}

pub(super) fn attach_input(key: Key) -> Option<AttachInput> {
    let (code, shift) = match key.code {
        KeyCode::Char(c) if !key.ctrl && !key.alt => {
            return (!c.is_control()).then(|| AttachInput::Text {
                text: c.to_string(),
            });
        }
        KeyCode::Char(c) => (AttachKeyCode::Char { char: c }, false),
        KeyCode::Enter => (AttachKeyCode::Enter, key.shift),
        KeyCode::Tab => (AttachKeyCode::Tab, key.shift),
        KeyCode::BackTab => (AttachKeyCode::Tab, true),
        KeyCode::Backspace => (AttachKeyCode::Backspace, key.shift),
        KeyCode::Delete => (AttachKeyCode::Delete, key.shift),
        KeyCode::Left => (AttachKeyCode::Left, key.shift),
        KeyCode::Right => (AttachKeyCode::Right, key.shift),
        KeyCode::Up => (AttachKeyCode::Up, key.shift),
        KeyCode::Down => (AttachKeyCode::Down, key.shift),
        KeyCode::Home => (AttachKeyCode::Home, key.shift),
        KeyCode::End => (AttachKeyCode::End, key.shift),
        KeyCode::PageUp => (AttachKeyCode::PageUp, key.shift),
        KeyCode::PageDown => (AttachKeyCode::PageDown, key.shift),
        KeyCode::Esc => (AttachKeyCode::Esc, key.shift),
        KeyCode::Insert => (AttachKeyCode::Insert, key.shift),
        KeyCode::F(n) => (AttachKeyCode::F { n }, key.shift),
        KeyCode::Other => return None,
    };
    AttachKey::new(code, shift, key.alt, key.ctrl).map(|key| AttachInput::Key { key })
}

pub(super) fn key_name(key: Key) -> String {
    let base = match key.code {
        KeyCode::Char(' ') => copy::SPACE_KEY.to_owned(),
        KeyCode::Char(c) => c.to_lowercase().to_string(),
        KeyCode::Enter => copy::ENTER_KEY.to_owned(),
        KeyCode::Tab => copy::TAB_KEY.to_owned(),
        KeyCode::BackTab => "shift-tab".to_owned(),
        KeyCode::Backspace => "backspace".to_owned(),
        KeyCode::Delete => "del".to_owned(),
        KeyCode::Left => "left".to_owned(),
        KeyCode::Right => "right".to_owned(),
        KeyCode::Up => "up".to_owned(),
        KeyCode::Down => "down".to_owned(),
        KeyCode::Home => "home".to_owned(),
        KeyCode::End => "end".to_owned(),
        KeyCode::PageUp => "pgup".to_owned(),
        KeyCode::PageDown => "pgdn".to_owned(),
        KeyCode::Esc => copy::ESC_KEY.to_owned(),
        KeyCode::Insert => "insert".to_owned(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Other => "that key".to_owned(),
    };
    let mut name = String::new();
    if key.ctrl {
        name.push_str("ctrl-");
    }
    if key.alt {
        name.push_str("alt-");
    }
    if key.shift && !matches!(key.code, KeyCode::Char(_) | KeyCode::BackTab) {
        name.push_str("shift-");
    }
    name.push_str(&base);
    name
}

fn at_least(size: TermSize) -> TermSize {
    TermSize {
        cols: size.cols.max(MIN_COLS),
        rows: size.rows.max(MIN_ROWS),
    }
}

impl Presenter {
    pub(super) fn process_event(&mut self, event: &AgentEvent, at: Duration) {
        match event {
            AgentEvent::ToolCallStarted {
                call_id,
                movable: true,
                ..
            } => self.movable = Some(call_id.clone()),
            AgentEvent::ToolCallFinished { call_id, .. }
                if self.movable.as_deref() == Some(call_id.as_str()) =>
            {
                self.movable = None;
            }
            AgentEvent::TurnEnded { .. } => self.movable = None,
            AgentEvent::ProcessStarted { .. } if self.list_on_start => {
                self.list_on_start = false;
                self.open_processes();
            }
            _ => {}
        }
        self.step_in_event(event, at);
        self.feed_processes(&AgentPath::root(), event, at);
    }

    pub(super) fn feed_processes(&mut self, agent: &AgentPath, event: &AgentEvent, at: Duration) {
        let ended = self.processes.apply(agent, event, at);
        let call = match event {
            AgentEvent::ProcessStarted { id, call_id, .. } => Some((*id, call_id.as_str())),
            AgentEvent::ProcessRestored {
                id,
                call_id: Some(call_id),
                ..
            } => Some((*id, call_id.as_str())),
            _ => None,
        };
        if let Some((id, call_id)) = call {
            let transcript = if agent.is_root() {
                Some(&self.transcript)
            } else {
                self.agent_models.transcript(agent)
            };
            if let Some(parts) = transcript.and_then(|t| t.call_caption(call_id, self.layout())) {
                self.processes.caption(
                    &ProcessKey {
                        agent: agent.clone(),
                        id,
                    },
                    call_id,
                    parts,
                );
            }
        }
        if let Some(list) = self.ps.as_mut().filter(|list| list.focus.is_none()) {
            list.focus = self.processes.ids().into_iter().next();
        }
        if let Some((id, end)) = ended {
            let key = ProcessKey {
                agent: agent.clone(),
                id,
            };
            let taken = self.processes.attached() == Some(&key);
            if told_on_exit(&end) && (taken || (agent.is_root() && !self.busy())) {
                self.hint(&copy::process_ended(key.name(&self.here()), &end), at);
            }
        }
    }

    pub(super) fn open_processes(&mut self) {
        if self.processes.has_restored_kept()
            && let Some(commands) = &self.commands
        {
            drop(commands.try_send(ClientCommand::CheckProcesses));
        }
        self.ps = Some(List {
            focus: self.processes.ids().into_iter().next(),
            ..List::default()
        });
        self.frames.mark();
    }

    pub(super) fn ps_open(&self) -> bool {
        self.ps.is_some()
    }

    pub(super) fn take_over_keys(&self) -> Vec<KeyHint> {
        let here = self.here();
        if let Some(key) = self.processes.attached() {
            return vec![
                KeyHint::note(copy::typing_into(key.name(&here))),
                copy::go_back_hint(),
            ];
        }
        let mut keys = vec![];
        if (!self.step_in_shown() || self.docked())
            && let Some(key) = self.processes.newest_waiting()
        {
            keys.push(KeyHint::note(copy::waiting_for_password(key.name(&here))));
            if !self.docked() {
                keys.push(copy::take_over_hint());
            }
        }
        keys
    }

    pub(super) fn process_keys(&self) -> Vec<KeyHint> {
        let glyphs = Glyphs::of(self.glyphs);
        let mut keys = self.take_over_keys();
        if self.processes.attached().is_some() {
            return keys;
        }
        if self.movable.is_some() {
            keys.push(copy::background_key());
        }
        let running = self.processes.running().count();
        if running > 0 && self.ps.is_none() {
            keys.push(KeyHint::note(format!(
                "{} {}",
                glyphs.running,
                copy::running_count(running)
            )));
            keys.push(KeyHint::note(copy::PS_HINT));
        }
        keys
    }

    pub(super) fn move_to_background(&mut self, at: Duration) {
        if let Some(call_id) = self.movable.clone() {
            self.command(ClientCommand::Background { call_id }, at);
        }
    }

    pub(super) fn ps_key(&mut self, key: Key, at: Duration) {
        self.frames.mark();
        let Some(list) = &mut self.ps else {
            return;
        };
        if take_over_key(key) {
            list.confirm = Confirm::default();
            if !self.step_in_shown()
                && let Some(waiting) = self.processes.newest_waiting()
            {
                self.attach_to(&waiting, at);
            }
            return;
        }
        let plain = !key.ctrl && !key.alt;
        if let Some(shown) = list.screen.clone() {
            if (key.code == KeyCode::Esc && plain)
                || (key.ctrl && matches!(key.code, KeyCode::Char('c' | 'C')))
            {
                if list.chat {
                    self.ps = None;
                } else {
                    list.screen = None;
                }
            } else if key.code == KeyCode::Char('a') && plain {
                self.attach_to(&shown, at);
            }
            return;
        }
        let ids = self.processes.ids();
        let count = ids.len();
        let index = list.index(&ids);
        let focused = ids.get(index).cloned();
        if !(key.code == KeyCode::Char('x') && plain) {
            list.confirm = Confirm::default();
        }
        match key.code {
            KeyCode::Esc if plain => self.ps = None,
            KeyCode::Char('c' | 'C') if key.ctrl => self.ps = None,
            code if plain && let Some(to) = ListCursor::CLAMP.key(code, index, count) => {
                list.focus = ids.get(to).cloned();
            }
            KeyCode::Enter if plain => {
                let Some(focused) = focused else {
                    return;
                };
                match self.processes.open(&focused) {
                    Opened::Screen(shown) => list.screen = Some(shown),
                    Opened::Call(call_id) => {
                        self.close_agents(at);
                        self.open_call(&call_id);
                    }
                    Opened::Nothing => {}
                }
            }
            KeyCode::Char('a') if plain => {
                if let Some(focused) = focused.filter(|k| self.processes.attachable(k)) {
                    self.attach_to(&focused, at);
                }
            }
            KeyCode::Char('x') if plain => {
                let Some(focused) = focused.filter(|k| self.processes.is_stoppable(k)) else {
                    return;
                };
                if list.confirm.press(focused.clone(), at) {
                    self.processes.stop_sent(&focused);
                    let id = focused.id;
                    self.send_to(&focused.agent, ClientCommand::StopProcess { id }, at);
                }
            }
            _ => {}
        }
    }

    fn open_call(&mut self, call_id: &str) {
        if let Some(cell) = self
            .transcript
            .cells()
            .iter()
            .rposition(|c| c.tool_call() == Some(call_id))
        {
            self.open_reader(cell);
        }
    }

    pub(super) fn live_screen(&self, key: &ProcessKey) -> bool {
        self.processes
            .screen(key)
            .is_some_and(|(_, _, ended)| !ended)
    }

    pub(super) fn watch_screen(&mut self, key: ProcessKey) {
        if !self.live_screen(&key) {
            return;
        }
        self.ps = Some(List {
            focus: Some(key.clone()),
            screen: Some(key),
            ..List::default()
        });
        self.frames.mark();
    }

    pub(super) fn ps_due(&self, at: Duration) -> Option<Duration> {
        let list = self.ps.as_ref()?;
        let confirm = list.confirm.due(at);
        if list.screen.is_some() {
            return confirm;
        }
        let (rows, focus) = self.process_rows(list, at);
        let here = self.here();
        let view = PsView {
            rows: &rows,
            focus,
            confirm: list.confirm.confirming(at).is_some(),
            here: &here,
            t: at,
            cx: self.cx(),
        };
        [ps_next_change(&view), confirm].into_iter().flatten().min()
    }

    fn process_rows(&self, list: &List, at: Duration) -> (Vec<ProcessRow>, usize) {
        let mut rows = self.processes.rows(at);
        for row in rows.iter_mut().filter(|row| !row.agent.is_root()) {
            row.agent_ended = self.agents.row(&row.agent).is_some_and(|r| !r.is_live());
        }
        (rows, list.index(&self.processes.ids()))
    }

    pub(super) fn ps_lines(&self, room: usize, at: Duration, chrome: Layout) -> Vec<StyledLine> {
        let Some(list) = &self.ps else {
            return vec![];
        };
        let here = self.here();
        if let Some(key) = &list.screen
            && let Some((command, screen, ended)) = self.processes.screen(key)
        {
            let mode = if self.processes.attached() == Some(key) {
                ScreenMode::Typing {
                    asked: (self.processes.request())
                        .filter(|r| r.id.is_some_and(|id| ProcessKey::main(id) == *key))
                        .map(|r| r.reason.clone()),
                    warning: !self.warned,
                }
            } else {
                ScreenMode::Watching
            };
            let view = ScreenView {
                name: key.name(&here),
                command,
                screen,
                ended,
                attachable: self.processes.attachable(key),
                mode,
            };
            return screen_rows(&view, room, chrome);
        }
        let (rows, focus) = self.process_rows(list, at);
        let view = PsView {
            rows: &rows,
            focus,
            confirm: list.confirm.confirming(at).is_some(),
            here: &here,
            t: at,
            cx: self.cx(),
        };
        ps_rows(&view, room, chrome)
    }

    pub(super) fn draw_ps(
        &mut self,
        buffer: &mut Buffer,
        view_rows: u16,
        at: Duration,
        chrome: Layout,
    ) -> Option<u16> {
        self.follow_size(view_rows, at);
        let lines = self.ps_lines(usize::from(view_rows), at, chrome);
        let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let top = view_rows.saturating_sub(height);
        paint_lines(buffer, top, buffer.area.width, &lines, &self.theme);
        (!lines.is_empty()).then_some(top)
    }
}

impl Presenter {
    pub(super) fn attach_to(&mut self, key: &ProcessKey, at: Duration) {
        if !self.processes.attachable(key) {
            return;
        }
        let list = self.ps.get_or_insert_with(|| List {
            chat: true,
            ..List::default()
        });
        list.screen = Some(key.clone());
        list.confirm = Confirm::default();
        self.processes.set_attached(Some(key.clone()));
        self.attach_pending = true;
        self.attach_sent = None;
        self.shown_status_mut().clear_hint();
        let attach = ClientCommand::AttachProcess {
            id: key.id,
            size: None,
        };
        self.send_to(&key.agent, attach, at);
        self.frames.mark();
    }

    fn follow_size(&mut self, view_rows: u16, at: Duration) {
        let Some(key) = self.processes.attached().cloned() else {
            return;
        };
        if self.ps.as_ref().and_then(|l| l.screen.as_ref()) != Some(&key) {
            return;
        }
        let want = at_least(screen_inner(view_rows, self.size.width));
        if self.attach_sent == Some(want) {
            return;
        }
        self.attach_sent = Some(want);
        let attach = ClientCommand::AttachProcess {
            id: key.id,
            size: Some(want),
        };
        self.send_to(&key.agent, attach, at);
    }

    fn detach(&mut self, at: Duration) {
        let Some(ProcessKey { agent, id }) = self.processes.attached().cloned() else {
            return;
        };
        self.processes.set_attached(None);
        self.attach_pending = false;
        self.attach_sent = None;
        self.warned = true;
        self.step_settle.hold(at);
        self.leave_screen();
        self.send_to(&agent, ClientCommand::DetachProcess { id }, at);
        self.frames.mark();
    }

    fn leave_screen(&mut self) {
        if self.ps.as_ref().is_some_and(|list| list.chat) {
            self.ps = None;
        }
    }

    pub(super) fn attach_key(&mut self, key: Key, at: Duration) {
        let Some(ProcessKey { agent, id }) = self.processes.attached().cloned() else {
            return;
        };
        self.frames.mark();
        if take_over_key(key) {
            self.detach(at);
            return;
        }
        match attach_input(key) {
            Some(item) => {
                let items = AttachItems(vec![item]);
                self.send_to(&agent, ClientCommand::ProcessInput { id, items }, at);
            }
            None => {
                let text = copy::no_terminal_code(&key_name(key));
                self.shown_status_mut().hint(&text, at, Some(NO_CODE_FOR));
            }
        }
    }

    pub(super) fn attach_paste(&mut self, text: &str, at: Duration) {
        let Some(attached) = self.processes.attached().cloned() else {
            return;
        };
        self.frames.mark();
        if text.len() > MAX_PASTE {
            self.shown_status_mut()
                .hint(copy::PASTE_TOO_LARGE, at, Some(NO_CODE_FOR));
            return;
        }
        if text.is_empty() {
            return;
        }
        if !self.processes.bracketed(&attached)
            && text.split('\n').any(|line| line.len() > LINE_LIMIT)
        {
            self.shown_status_mut()
                .hint(copy::LINE_MODE_PASTE, at, Some(LONG_LINE_FOR));
        }
        let items = AttachItems(vec![AttachInput::Paste {
            text: text.to_owned(),
        }]);
        let id = attached.id;
        self.send_to(
            &attached.agent,
            ClientCommand::ProcessInput { id, items },
            at,
        );
    }

    pub(super) fn attach_refused(&mut self, event: &AgentEvent, at: Duration) {
        let (agent, event) = match event {
            AgentEvent::Agent { agent, event } => (agent.clone(), event.as_ref()),
            event => (AgentPath::root(), event),
        };
        let attached = self.processes.attached().cloned();
        let theirs = attached.as_ref().is_some_and(|key| key.agent == agent);
        let ours = |id: &u32| theirs && attached.as_ref().is_some_and(|key| key.id == *id);
        let ends = |kind: &RejectKind| match kind {
            RejectKind::NoProcess
            | RejectKind::ProcessEnded
            | RejectKind::NotATerminal
            | RejectKind::AlreadyAttached
            | RejectKind::NotAttached => theirs,
            RejectKind::Other(code) => code == NO_SUCH_AGENT && agent.is_root() && !theirs,
            _ => false,
        };
        match event {
            AgentEvent::ProcessAttached { id, .. } => {
                if ours(id) {
                    self.attach_pending = false;
                }
            }
            AgentEvent::ProcessDetached { id, reason, .. } => {
                if ours(id) && *reason == DetachReason::Ended {
                    self.leave_screen();
                }
                if ours(id) || self.attach_pending {
                    self.warned = true;
                    self.attach_pending = false;
                    self.attach_sent = None;
                    self.step_settle.hold(at);
                }
            }
            AgentEvent::Rejected {
                kind: Some(kind), ..
            } if attached.is_some() && ends(kind) => {
                self.processes.set_attached(None);
                self.attach_pending = false;
                self.attach_sent = None;
            }
            _ => {}
        }
    }

    pub(super) fn step_in_shown(&self) -> bool {
        self.processes.request().is_some_and(|r| {
            self.processes.attached().is_none()
                && match r.kind {
                    StepKind::Attach => {
                        r.id.is_some_and(|id| self.processes.attachable(&ProcessKey::main(id)))
                    }
                    StepKind::Secret { .. } => {
                        r.id.is_none_or(|id| self.processes.is_running(&ProcessKey::main(id)))
                    }
                }
        })
    }

    fn step_in_on_main(&self) -> bool {
        self.step_in_shown()
            && !self.docked()
            && self.settings_screen.is_none()
            && self.chat.is_none()
    }

    pub(super) fn step_in_holds(&self, at: Duration) -> bool {
        self.step_in_on_main() && self.step_settle.settled(at)
    }

    pub(super) fn step_in_hint(&mut self, at: Duration) {
        self.hint(&copy::step_in_hint(), at);
    }

    pub(super) fn step_in_event(&mut self, event: &AgentEvent, at: Duration) {
        if let AgentEvent::AttachRequestClosed { .. } | AgentEvent::TurnEnded { .. } = event
            && self.status.shown_hint(at) == Some(copy::step_in_hint().as_str())
        {
            self.status.clear_hint();
        }
        if let AgentEvent::SecretRequested { show, .. } = event {
            self.secret_field.ask(*show);
            self.secret_hidden = false;
            self.secret_sent = false;
            self.secret_problem.clear();
            self.secret_drop = None;
        }
        let open = self
            .processes
            .request()
            .filter(|r| matches!(r.kind, StepKind::Secret { .. }));
        let closed = match event {
            AgentEvent::SecretRequestClosed { call_id, .. } => {
                open.is_some_and(|r| &r.call_id == call_id)
            }
            AgentEvent::TurnEnded { .. } => open.is_some(),
            AgentEvent::ProcessEnded { id, .. } => open.is_some_and(|r| r.id == Some(*id)),
            _ => false,
        };
        let open = open.is_some();
        if closed {
            self.secret_field.clear();
            self.secret_problem.clear();
            self.secret_hidden = false;
            if !matches!(
                event,
                AgentEvent::SecretRequestClosed {
                    how: SecretRequestEnd::Entered,
                    ..
                }
            ) {
                if self.secret_drop.is_none() {
                    self.hint(copy::SECRET_CLOSED, at);
                }
                self.secret_drop.get_or_insert_default().hold(at);
            }
            self.secret_sent = false;
        }
        if open
            && matches!(
                event,
                AgentEvent::Rejected {
                    kind: Some(RejectKind::BadAnswer),
                    ..
                }
            )
        {
            self.secret_sent = false;
            self.secret_field.clear();
            self.secret_problem.clear();
        }
        if let AgentEvent::AttachRequested { .. } | AgentEvent::SecretRequested { .. } = event {
            self.step_in_seen = false;
            self.step_pick.focus = None;
            self.step_settle = Settle::after(self.last_typed, at);
            if self.pacing.unfocused() {
                self.bell = true;
            }
        }
    }

    pub(super) fn step_in_due(&self, at: Duration) -> Option<Duration> {
        self.step_settle.due(at).filter(|_| self.step_in_shown())
    }

    pub(super) fn step_in_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        let Some(request) = self.processes.request().cloned() else {
            return Ok(());
        };
        self.frames.mark();
        if matches!(request.kind, StepKind::Secret { .. }) {
            return self.secret_key(key, at, console);
        }
        if let Some(done) = self.held_key(key, at, console) {
            return done;
        }
        let Some(id) = request.id else {
            return Ok(());
        };
        let typing = !self.composer.is_empty();
        if typing {
            self.step_pick.focus = None;
        }
        match key.code {
            _ if stops_turn(key) => self.stop(),
            _ if take_over_key(key) => self.attach_to(&ProcessKey::main(id), at),
            KeyCode::Tab | KeyCode::BackTab => self.step_in_hint(at),
            code if typing && !matches!(code, KeyCode::Char('1' | '2')) => self.step_in_hint(at),
            _ => match self.step_pick.key(key) {
                Picked::Took(0) => self.attach_to(&ProcessKey::main(id), at),
                Picked::Took(_) => {
                    let call_id = request.call_id;
                    self.command(ClientCommand::DeclineAttach { call_id }, at);
                }
                Picked::Moved => {}
                Picked::Not => self.step_in_hint(at),
            },
        }
        Ok(())
    }

    pub(super) fn take_over_waiting(&mut self, at: Duration) {
        if self.step_in_shown() {
            return;
        }
        match self.processes.newest_waiting() {
            Some(key) => self.attach_to(&key, at),
            None => {
                let glyphs = Glyphs::of(self.glyphs);
                let text = copy::nothing_waiting(glyphs.separator);
                self.shown_status_mut().hint(&text, at, Some(NO_CODE_FOR));
            }
        }
    }

    pub(super) fn step_in_lines(
        &self,
        settling: bool,
        room: usize,
        chrome: Layout,
    ) -> Option<Vec<StyledLine>> {
        if !self.step_in_shown() {
            return None;
        }
        let request = self.processes.request()?;
        if let Some(view) = self.secret_view(settling) {
            return Some(wisp_render::chrome::secret_rows(&view, room, chrome).lines);
        }
        let view = StepInView {
            id: request.id?,
            reason: &request.reason,
            settling,
            selected: self.step_pick.focus,
            typing: !self.composer.is_empty(),
        };
        Some(step_in_rows(&view, room, chrome))
    }

    pub(super) fn draw_step_in(
        &mut self,
        buffer: &mut Buffer,
        view_rows: u16,
        at: Duration,
        chrome: Layout,
    ) -> Option<u16> {
        let settling = !self.step_settle.settled(at);
        let lines = self.step_in_lines(settling, usize::from(view_rows), chrome)?;
        let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let top = view_rows.saturating_sub(height);
        paint_lines(buffer, top, buffer.area.width, &lines, &self.theme);
        Some(top)
    }
    pub(super) fn secret_view(&self, settling: bool) -> Option<SecretView<'_>> {
        let request = self.processes.request()?;
        let StepKind::Secret {
            name,
            destination,
            asked_by,
            ..
        } = &request.kind
        else {
            return None;
        };
        let destination = match destination {
            Some(destination) => wisp_render::chrome::SecretGoes::Server {
                name: &destination.server,
                command: destination.command.as_deref(),
            },
            None => {
                let id = request.id?;
                wisp_render::chrome::SecretGoes::Process {
                    id,
                    command: self.processes.command(&ProcessKey::main(id))?,
                }
            }
        };
        Some(SecretView {
            name,
            reason: &request.reason,
            asked_by: asked_by.as_ref(),
            destination,
            value: self.secret_field.value(),
            ends: self.secret_field.ends().unwrap_or_default(),
            pasted: self.secret_field.pasted(),
            hidden: self.secret_hidden,
            sent: self.secret_sent,
            problem: &self.secret_problem,
            settling,
        })
    }

    pub(super) fn secret_visible(&self) -> bool {
        self.step_in_on_main()
            && self.secret_view(false).is_some()
            && self.pager.is_none()
            && !self.overlay_holds()
    }

    pub(super) fn secret_drops_input(&mut self, at: Duration) -> bool {
        if let Some(drop) = &mut self.secret_drop {
            let stopping = self
                .processes
                .request()
                .is_some_and(|r| matches!(r.kind, StepKind::Secret { .. }));
            if stopping || !drop.settled(at) {
                drop.hold(at);
                return true;
            }
            self.secret_drop = None;
        }
        if self.secret_visible() && !self.step_in_holds(at) {
            self.step_settle.hold(at);
            return true;
        }
        false
    }

    pub(super) fn secret_stop_key(&mut self, key: Key, at: Duration) -> bool {
        if stops_turn(key) {
            self.secret_field.clear();
            self.secret_drop.get_or_insert_default().hold(at);
            self.stop();
            return true;
        }
        false
    }

    pub(super) fn secret_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        if self.secret_stop_key(key, at) {
            return Ok(());
        }
        if self.secret_sent || self.cancel {
            return Ok(());
        }
        if let Some(done) = self.held_key(key, at, console) {
            return done;
        }
        self.secret_problem.clear();
        match key.code {
            KeyCode::Enter if !key.ctrl && !key.alt => {
                if !self.secret_field.filled() {
                    self.secret_problem = copy::SECRET_EMPTY.to_owned();
                } else if let Some(request) = self.processes.request() {
                    let command = ClientCommand::AnswerSecret {
                        call_id: request.call_id.clone(),
                        value: self.secret_field.take(),
                    };
                    self.secret_sent = self.command(command, at);
                }
            }
            KeyCode::Tab if !key.ctrl && !key.alt => {
                if self
                    .secret_view(false)
                    .is_some_and(|v| v.two_forms(self.chrome_layout()))
                {
                    self.secret_hidden = !self.secret_hidden;
                }
            }
            _ => {
                if let Err(problem) = self.secret_field.key(key) {
                    self.secret_problem = problem.of("secret");
                }
            }
        }
        Ok(())
    }

    pub(super) fn secret_paste(&mut self, text: &str) {
        if self.secret_sent || self.cancel {
            return;
        }
        self.secret_hidden = false;
        self.secret_problem = self
            .secret_field
            .paste(text)
            .err()
            .map(|p| p.of("secret"))
            .unwrap_or_default();
    }

    pub(super) fn step_in_paste(&mut self, text: &str, at: Duration) {
        if self.secret_view(false).is_some() {
            self.secret_paste(text);
        } else {
            self.step_in_hint(at);
        }
    }
}
