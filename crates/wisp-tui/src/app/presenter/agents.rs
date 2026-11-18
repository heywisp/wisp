use std::rc::Rc;
use std::time::Duration;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use wisp_protocol::slot::{NodeState, SlotBlock, SlotFlow, SlotTopologyNode};
use wisp_protocol::{AgentEvent, AgentPath, ClientCommand, DeliveryMode};
use wisp_render::chrome::account::fit_parts;
use wisp_render::chrome::topology::{Drawing, Topology, Toward, key_at, page, step};
use wisp_render::chrome::{
    AGENTS_LIST_GAP, AGENTS_MIN_ROWS, AccountBilling, AgentRow, AgentsTree, AgentsView, ChatTurn,
    MainRow, PAGE_CHROME, agent_label, agent_row, agents_next_change, agents_note, agents_page,
    agents_tree, beside, chat_header, clocks_next_change, list_width, panel_rows, releasable,
};
use wisp_render::markdown::Layout;
use wisp_render::motion::FRAME_MIN;
use wisp_render::picture::Scene;
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::copy::{AgentKeys, Moves};
use wisp_render::transcript::{UserPart, copy};

use super::sidebar::{Shown, shown_tiles};
use super::waiting::{await_sent, send_command};
use super::{AGAIN_TO_QUIT, AppError, Console, Presenter, Seek, feed_status, paint_lines};
use crate::app::agents::Change;
use crate::app::commands::{self, CommandId, InChat, Parsed};
use crate::app::endpoint_card::endpoint_facts_of;
use crate::app::help::first_keys;
use crate::app::inputs::Applied;
use crate::app::keymap::{Action, Context, action};
use crate::app::sidebar::{AGENTS_SECTION, PULSE_STEP, section_off};
use crate::term::{Key, KeyCode, Mouse};

pub(super) struct AgentsList {
    picked: AgentPath,
    drawn: Option<Paged>,
}

struct Paged {
    top: u16,
    height: usize,
    mouse_cell: Option<(u16, u16)>,
    listed: Vec<AgentPath>,
    scenes: Vec<Rc<Scene>>,
    moving: bool,
}

impl AgentsList {
    pub(super) fn is_drawn(&self) -> bool {
        self.drawn.is_some()
    }

    pub(super) fn shows_picture(&self) -> bool {
        self.drawn.as_ref().is_some_and(|d| !d.scenes.is_empty())
    }
}

type Drawn<'a> = (Option<&'a str>, &'a [SlotTopologyNode], &'a [SlotFlow]);

fn key(path: &AgentPath) -> Option<&str> {
    (!path.is_root()).then(|| path.as_str())
}

pub(super) struct AgentChat {
    pub(super) path: AgentPath,
}

impl Presenter {
    pub(super) fn ask_agents(&mut self) {
        let request = self.next_request();
        let at = self.at(tokio::time::Instant::now());
        self.command(ClientCommand::ListAgents { request }, at);
    }

    pub(super) fn agents_event(&mut self, event: &AgentEvent, at: Duration) {
        if let AgentEvent::AgentRenamed {
            agent: from, to, ..
        } = event
        {
            self.agents.renamed(from, to);
            self.agent_models.follow(from, to);
            self.processes.follow(from, to);
            self.ps.iter_mut().for_each(|ps| ps.follow(from, to));
            let chat = self.chat.iter_mut().map(|chat| &mut chat.path);
            for path in chat.chain(self.agents_list.iter_mut().map(|l| &mut l.picked)) {
                *path = path.clone().renamed(from, to);
            }
        }
        match event {
            AgentEvent::TurnStarted { .. } => self.main_since = Some(at),
            AgentEvent::TurnEnded { .. } => self.main_since = None,
            _ => {}
        }
        self.agents.apply(event, self.now_ms(), &self.facts);
        if let AgentEvent::Agent { agent, event } = event
            && (self.rewind_before(Some(agent), event) || self.agent_answered(agent, event, at))
        {
            return;
        }
        if let AgentEvent::Agent { agent, event } = event {
            self.feed_processes(agent, event, at);
        }
        let lost = match self.agent_models.apply(event, at, &self.facts) {
            Some(Change::Applied(agent, applied)) => {
                let inner = match event {
                    AgentEvent::Agent { event, .. } => event.as_ref(),
                    event => event,
                };
                let (cx, layout) = (self.cx(), self.layout());
                let effort = self
                    .agents
                    .row(&agent)
                    .and_then(|r| r.effort)
                    .map(|e| e.as_str());
                if let Some(model) = self.agent_models.get_mut(&agent) {
                    feed_status(
                        &mut model.status,
                        &model.transcript,
                        inner,
                        effort,
                        at,
                        cx,
                        layout,
                    );
                    await_sent(&mut model.status, &model.inputs, inner, at, cx);
                }
                self.agent_applied(agent.clone(), applied, at);
                self.rewind_event(Some(&agent), inner, at);
                None
            }
            Some(Change::Lost(ended)) if self.inputs.restarting() => {
                self.agent_models.hold(ended);
                None
            }
            Some(Change::Lost(ended)) => {
                for saved in ended.iter().flat_map(|(_, kept)| kept) {
                    self.composer.remember(saved);
                }
                (ended.into_iter().map(|(agent, kept)| (agent, kept.len())))
                    .rfind(|(_, kept)| *kept > 0)
            }
            None => None,
        };
        let gone = (self.chat.as_ref())
            .map(|chat| chat.path.clone())
            .filter(|path| !self.agent_models.has(path));
        if gone.is_some() {
            self.close_agents(at);
        }
        if let Some((agent, kept)) = lost.or(gone
            .filter(|_| !self.inputs.restarting())
            .map(|path| (path, 0)))
        {
            let sep = format!(" {} ", Glyphs::of(self.glyphs).separator);
            self.hint(&copy::agent_ended(agent.as_str(), kept, &sep), at);
        }
    }

    fn agent_answered(&mut self, agent: &AgentPath, event: &AgentEvent, at: Duration) -> bool {
        if !self.agent_models.has(agent) {
            return false;
        }
        let facts = self.pair_for(Some(agent)).0.into_owned();
        let glyphs = self.glyphs;
        let Some(model) = self.agent_models.get_mut(agent) else {
            return false;
        };
        let before = model.tracker.session().clone();
        let answer = model.tracker.apply(event, &facts, at, glyphs);
        let started = matches!(event, AgentEvent::TurnStarted { .. });
        let moved = started && *model.tracker.session() != before;
        if let AgentEvent::SettingsSnapshot(snapshot) = event {
            model.listed = Some(snapshot.endpoints.iter().map(endpoint_facts_of).collect());
            if let Some(picker) = &mut self.picker {
                picker.answered(snapshot.answers);
            }
            self.account_described(agent, snapshot, at);
        }
        self.plugin_answered(Some(agent), event);
        if moved {
            self.ask_agents();
        }
        self.answer_notes(Some(agent), answer, at)
    }

    fn agent_applied(&mut self, agent: AgentPath, applied: Applied, at: Duration) {
        let sep = format!(" {} ", Glyphs::of(self.glyphs).separator);
        let mut notes = Vec::new();
        if !applied.returned.is_empty() {
            if let Some(model) = self.agent_models.get_mut(&agent) {
                let returned: Vec<_> = applied
                    .returned
                    .into_iter()
                    .map(|(_, saved)| saved)
                    .collect();
                model.composer.restore_ahead(&returned);
            }
            if self.chat.as_ref().is_some_and(|chat| chat.path == agent) {
                notes.push(copy::message_back(agent.as_str()));
            } else {
                notes.push(copy::message_came_back(agent.as_str(), &sep));
            }
            self.typed_changed(at);
            self.frames.mark();
        }
        if applied.held > 0 {
            notes.push(copy::agent_held(
                agent.as_str(),
                applied.failed,
                applied.held,
                &sep,
            ));
        }
        if let Some(model) = self.agent_models.get_mut(&agent) {
            for note in &notes {
                model.transcript.note(note);
            }
        }
        if let Some(note) = notes.last() {
            self.hint(note, at);
        }
    }

    pub(super) fn sync_agent_links(&mut self) {
        let sep = format!(" {} ", Glyphs::of(self.glyphs).separator);
        for (name, link) in self.agents.links(self.now_ms(), &sep) {
            self.transcript.link_agent(&name, Some(link));
        }
    }

    pub(super) fn session_id(&self) -> Option<String> {
        self.facts
            .log
            .as_deref()
            .and_then(|log| std::path::Path::new(log).file_stem())
            .map(|id| id.to_string_lossy().into_owned())
    }

    pub(super) fn agents_busy(&self) -> usize {
        self.agents.busy()
    }

    pub(super) fn quit_hint(&self) -> String {
        match self.agents_busy() {
            0 => AGAIN_TO_QUIT.to_owned(),
            busy => copy::quit_with_agents(busy),
        }
    }

    pub(super) fn exit_command(&mut self, at: Duration) {
        if self.agents_busy() == 0 || self.armed || self.exit_armed {
            self.quit();
            return;
        }
        self.armed = true;
        let hint = self.quit_hint();
        self.hint(&hint, at);
    }

    pub(super) fn exit_typing(&self) -> bool {
        let text = self.composer.text();
        let text = text.trim();
        text.strip_prefix('/').is_some_and(|typed| {
            commands::get(CommandId::Exit)
                .into_iter()
                .flat_map(commands::names)
                .any(|name| name.starts_with(typed))
        })
    }

    pub(super) fn stopped_hint(&mut self, at: Duration) {
        let busy = self.agents_busy();
        if busy > 0 {
            let sep = format!(" {} ", Glyphs::of(self.glyphs).separator);
            self.hint(&copy::stopped_main(busy, &sep), at);
        }
    }

    pub(super) fn agent_line(&self, chrome: Layout) -> Option<StyledLine> {
        let turn = if self.status.is_running() {
            ChatTurn::Running
        } else {
            ChatTurn::Idle
        };
        let rows = self.agents.rows();
        wisp_render::chrome::strip_line(rows, self.agents.main_spend(), turn, chrome)
    }

    pub(super) fn mail_line(&self, chrome: Layout) -> Vec<StyledLine> {
        let count = self.inputs.mail_waiting();
        let hold = self.agents.mail_hold();
        if self.status.is_running() || count == 0 || hold.is_none() {
            return vec![];
        }
        let glyphs = Glyphs::of(chrome.glyphs);
        let parts = copy::mail_waits(count, hold);
        let (first, rest) = parts.split_at(1);
        let rest: Vec<_> = rest.iter().map(|p| (p.clone(), p.clone())).collect();
        let room = chrome.width.saturating_sub(1);
        fit_parts(first, &rest, "", &glyphs, room)
            .into_iter()
            .map(|text| {
                StyledLine::new(vec![
                    StyledSpan::plain(" "),
                    StyledSpan::new(text, Token::Text),
                ])
            })
            .collect()
    }

    pub(super) fn open_agents(&mut self) {
        self.agents_list = Some(AgentsList {
            picked: AgentPath::root(),
            drawn: None,
        });
        self.sidebar_changed();
        self.frames.mark();
    }

    fn drawn_nodes(&self) -> Option<Drawn<'_>> {
        if !self.sidebar.offers(AGENTS_SECTION) {
            return None;
        }
        let content = self.sidebar.page_content(AGENTS_SECTION)?;
        content.blocks.iter().find_map(|block| match block {
            SlotBlock::Topology {
                root, nodes, flows, ..
            } => Some((root.as_deref(), &nodes[..], &flows[..])),
            _ => None,
        })
    }

    fn listed_agents(&self) -> impl Iterator<Item = Option<&AgentRow>> + Clone {
        std::iter::once(None).chain(self.agents.rows().iter().filter(|r| r.is_live()).map(Some))
    }

    fn list_room(&self) -> usize {
        match list_width(self.listed_agents(), self.chrome_layout()) {
            0 => 0,
            width => width + AGENTS_LIST_GAP,
        }
    }

    fn agent_picked(&self) -> AgentPath {
        let drawn = self.drawn_nodes().filter(|_| self.list_room() == 0);
        let shown = |path: &AgentPath| match drawn {
            _ if path.is_root() => true,
            Some((_, nodes, _)) => nodes.iter().any(|n| n.key == path.as_str()),
            None => self.agents.row(path).is_some_and(AgentRow::is_live),
        };
        let mut picked =
            (self.agents_list.as_ref()).map_or_else(AgentPath::root, |l| l.picked.clone());
        while !shown(&picked) {
            picked = picked.parent().unwrap_or_else(AgentPath::root);
        }
        picked
    }

    fn topology<'a>(
        &self,
        (root, nodes, flows): Drawn<'a>,
        picked: &'a AgentPath,
        at: Duration,
    ) -> Topology<'a> {
        Topology {
            root,
            nodes,
            flows,
            picked: key(picked),
            label: None,
            layout: Layout {
                width: usize::from(self.size.width) - self.list_room(),
                ..self.chrome_layout()
            },
            at,
            now_ms: self.now_ms(),
            cx: self.cx(),
            pictures: self.pictures.is_some(),
        }
    }

    fn move_pick(&mut self, code: KeyCode, at: Duration) {
        let picked = self.agent_picked();
        let drawn = self.drawn_nodes();
        let moved = match code {
            KeyCode::Up | KeyCode::Down if drawn.is_none() || self.list_room() > 0 => {
                let mut listed = self
                    .listed_agents()
                    .map(|r| r.map(|r| r.entry.path.as_str()));
                let i = listed.clone().position(|p| p == key(&picked)).unwrap_or(0);
                let i = if code == KeyCode::Up {
                    i.saturating_sub(1)
                } else {
                    i + 1
                };
                listed
                    .nth(i)
                    .map(|p| p.map_or_else(AgentPath::root, AgentPath::new))
            }
            _ => drawn.and_then(|drawn| {
                let toward = match code {
                    KeyCode::Up => Toward::Up,
                    KeyCode::Down => Toward::Down,
                    KeyCode::Left => Toward::Left,
                    _ => Toward::Right,
                };
                step(&self.topology(drawn, &picked, at), toward)
                    .map(|key| key.map_or_else(AgentPath::root, AgentPath::new))
            }),
        };
        if let (Some(moved), Some(list)) = (moved, &mut self.agents_list) {
            list.picked = moved;
        }
    }

    fn enter_agent(&mut self, path: AgentPath, at: Duration) {
        if path.is_root() {
            self.close_agents(at);
        } else if self.agents.row(&path).is_some_and(AgentRow::is_live) {
            self.open_agent(path, at);
        }
    }

    fn dot_at(&self, (column, row): (u16, u16), at: Duration) -> Option<AgentPath> {
        let drawn = self.agents_list.as_ref()?.drawn.as_ref()?;
        let x = usize::from(column).checked_sub(self.list_room())?;
        let y = usize::from(row.checked_sub(drawn.top)?);
        let picked = self.agent_picked();
        let view = self.topology(self.drawn_nodes()?, &picked, at);
        key_at(&view, drawn.height, (x, y))
            .map(|hit| hit.map_or_else(AgentPath::root, AgentPath::new))
    }

    pub(super) fn agents_hover(&mut self, cell: (u16, u16), at: Duration) -> bool {
        if !self.mouse {
            return false;
        }
        let Some(drawn) = self.agents_list.as_mut().and_then(|l| l.drawn.as_mut()) else {
            return false;
        };
        let before = drawn.mouse_cell.replace(cell);
        let hit = |cell| {
            (self.mouse_layer(cell) == Some(super::Layer::Agents))
                .then(|| self.dot_at(cell, at))
                .flatten()
        };
        before != Some(cell) && before.and_then(hit) != hit(cell)
    }

    pub(super) fn forget_agents_hover(&mut self) {
        if let Some(drawn) = self.agents_list.as_mut().and_then(|l| l.drawn.as_mut()) {
            drawn.mouse_cell = None;
        }
    }

    pub(super) fn agents_click(&mut self, mouse: Mouse, at: Duration) -> bool {
        let hit = if usize::from(mouse.column) < self.list_room() {
            self.agents_list
                .as_ref()
                .and_then(|l| l.drawn.as_ref())
                .and_then(|d| {
                    d.listed
                        .get(usize::from(mouse.row.checked_sub(d.top)?))
                        .cloned()
                })
        } else {
            self.dot_at((mouse.column, mouse.row), at)
        };
        let Some(hit) = hit else { return false };
        if let Some(list) = &mut self.agents_list {
            list.picked = hit.clone();
        }
        self.frames.mark();
        self.enter_agent(hit, at);
        true
    }

    pub(super) fn agents_pictures(&self) -> Vec<Shown> {
        let drawn = self.agents_list.as_ref().and_then(|l| l.drawn.as_ref());
        let (Some((_, cell)), Some(Paged { top, scenes, .. })) = (self.pictures, drawn) else {
            return Vec::new();
        };
        let offset = (self.list_room(), usize::from(*top));
        scenes
            .iter()
            .flat_map(|scene| shown_tiles(scene, cell, offset, 0))
            .collect()
    }

    pub(super) fn agents_open(&self) -> bool {
        self.agents_list.is_some()
    }

    fn main_row(&self) -> MainRow {
        let tally = self.status.tally();
        let endpoint = self
            .facts
            .endpoint_name
            .as_deref()
            .and_then(|name| self.facts.endpoint_facts(name));
        MainRow {
            running_since: self.main_since,
            spend: self.agents.main_spend(),
            meter: tally.meter(),
            checklist: tally.todo_progress(),
            model: self.facts.short_model(),
            effort: (!self.facts.effort.is_empty()).then(|| self.facts.effort.clone()),
            keep_warm: self.agents.main_warm(),
            endpoint: self.facts.endpoint_name.clone(),
            priced: endpoint.is_none_or(|e| e.priced),
            accounts: self.account_billings(),
        }
    }

    fn account_billings(&self) -> Vec<AccountBilling> {
        self.facts
            .endpoints
            .iter()
            .map(|e| AccountBilling {
                name: e.name.clone(),
                state: self.facts.account_state(&e.name),
                mark: e.mark,
            })
            .collect()
    }

    fn close_chat(&mut self, at: Duration) {
        let Some(agent) = self.chat.as_ref().map(|chat| chat.path.clone()) else {
            return;
        };
        self.clear_selection();
        self.sync_selection_hint(at);
        self.chat = None;
        self.picker = None;
        self.rewind = None;
        if self.agent_models.has(&agent) {
            self.command(ClientCommand::WatchAgent { agent, live: false }, at);
        }
    }

    pub(super) fn close_agents(&mut self, at: Duration) {
        self.close_chat(at);
        self.agents_list = None;
        self.sidebar_changed();
        self.frames.mark();
    }

    pub(super) fn open_agent(&mut self, agent: AgentPath, at: Duration) -> bool {
        if !self.agent_models.has(&agent) {
            return false;
        }
        self.clear_selection();
        self.sync_selection_hint(at);
        self.chat = Some(AgentChat {
            path: agent.clone(),
        });
        self.command(ClientCommand::WatchAgent { agent, live: true }, at);
        true
    }

    pub(super) fn agents_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        if self.chat.is_some() {
            return self.chat_key(key, at, console);
        }
        self.frames.mark();
        let plain = !key.ctrl && !key.alt;
        if key.ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            self.close_agents(at);
            return Ok(());
        }
        if self.agents_list.is_none() {
            return Ok(());
        }
        let picked = self.agent_picked();
        match key.code {
            KeyCode::Esc if plain => self.close_agents(at),
            KeyCode::Enter if plain => self.enter_agent(picked, at),
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right if plain => {
                self.move_pick(key.code, at);
            }
            _ => self.agent_key(&picked, key, at),
        }
        Ok(())
    }

    pub(super) fn agent_key(&mut self, path: &AgentPath, key: Key, at: Duration) {
        let release = key.code == KeyCode::Char('r') && !key.ctrl && !key.alt;
        if release && self.agents.row(path).is_some_and(releasable) {
            self.child_command(path.clone(), ClientCommand::Release, at);
        }
    }

    pub(super) fn child_command(
        &mut self,
        agent: AgentPath,
        command: ClientCommand,
        at: Duration,
    ) -> bool {
        let command = Box::new(command);
        self.command(ClientCommand::Child { agent, command }, at)
    }

    pub(super) fn here(&self) -> AgentPath {
        (self.chat.as_ref()).map_or_else(AgentPath::root, |chat| chat.path.clone())
    }

    pub(super) fn send_to(
        &mut self,
        agent: &AgentPath,
        command: ClientCommand,
        at: Duration,
    ) -> bool {
        match agent.is_root() {
            true => self.command(command, at),
            false => self.child_command(agent.clone(), command, at),
        }
    }

    fn chat_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        self.frames.mark();
        self.shown_status_mut().clear_hint();
        if key.code == KeyCode::Esc {
            if self.select.showing() {
                self.clear_selection();
            } else {
                self.close_agents(at);
            }
            return Ok(());
        }
        if key.ctrl && matches!(key.code, KeyCode::Char('d' | 'D')) {
            return Ok(());
        }
        let Some(chat) = &self.chat else {
            return Ok(());
        };
        let path = chat.path.clone();
        let cx = Context {
            running: self.agent_models.running_turn(&path).is_some(),
            held: self.agents.row(&path).is_some_and(releasable),
            scrolled: !self.shown_view().is_following(),
            selection: self.select.showing(),
            ..Context::default()
        };
        let shape = self.composer_shape();
        let Some(model) = self.agent_models.get_mut(&path) else {
            return Ok(());
        };
        let composer = &mut model.composer;
        let (first_row, last_row) = composer.row_edges(key, shape);
        let cx = Context {
            empty: composer.is_empty(),
            blank: composer.is_blank(),
            tab_queues: composer.tab_queues(),
            browsing: composer.is_browsing(),
            first_row,
            last_row,
            ..cx
        };
        match action(key, cx) {
            Action::Newline => composer.newline(),
            Action::HistoryPrevious => {
                composer.older();
            }
            Action::HistoryNext => {
                composer.newer();
            }
            Action::Edit(Key {
                code: code @ (KeyCode::Up | KeyCode::Down),
                ctrl: false,
                alt: false,
                ..
            }) => {
                composer.vertical(code == KeyCode::Up, shape);
            }
            Action::Edit(key) => {
                composer.edit(key);
            }
            Action::Stop | Action::Clear | Action::ArmQuit | Action::Quit
                if !composer.is_empty() =>
            {
                composer.clear();
            }
            Action::Stop | Action::Clear | Action::ArmQuit | Action::Quit => self.close_agents(at),
            Action::CycleEffort => self.cycle_effort(at),
            Action::TakeOver => self.take_over_waiting(at),
            Action::Send | Action::Steer => self.chat_send(DeliveryMode::Steer, at),
            Action::Queue => self.chat_send(DeliveryMode::Queue, at),
            Action::Release => {
                self.child_command(path, ClientCommand::Release, at);
            }
            Action::Help => {
                self.leave_chat_for(CommandId::Help, at);
                self.open_help(Seek::Cell(first_keys(self.suggest.plugins())));
            }
            Action::Agents => {
                self.leave_chat_for(CommandId::Agents, at);
                self.open_agents();
            }
            Action::Scroll(scroll) => self.scroll(scroll, None, at),
            Action::Copy => return self.copy(console, at, true),
            Action::Deselect => self.clear_selection(),
            Action::Repaint => self.painter.invalidate(),
            Action::Suspend => return self.suspend(console),
            _ => {}
        }
        self.typed_changed(at);
        Ok(())
    }

    fn leave_chat_for(&mut self, id: CommandId, at: Duration) {
        if commands::in_chat(id) == InChat::Main {
            self.close_agents(at);
        }
    }

    fn chat_send(&mut self, mode: DeliveryMode, at: Duration) {
        let Some(path) = self.chat.as_ref().map(|chat| chat.path.clone()) else {
            return;
        };
        let Some(typed) = self.chat_model().map(|model| model.composer.message()) else {
            return;
        };
        let attachments = (typed.parts.iter()).any(|part| !matches!(part, UserPart::Text(_)));
        let plugins = self.suggest.plugins().to_vec();
        let own = self
            .chat_model()
            .is_some_and(|m| super::busy(&m.status, &m.inputs));
        match commands::parse(&plugins, &typed.text, attachments) {
            Parsed::Message => {}
            Parsed::Unknown(name) => return self.hint(&copy::unknown_command(name), at),
            Parsed::Command { id, args } if self.drops_key_shaped(id, args, at) => return,
            Parsed::Command { id, .. } if self.already_compacting(id, at) => return,
            Parsed::Command {
                id: CommandId::Rewind,
                ..
            } if own => return self.open_rewind(at),
            Parsed::Command { id, args } => {
                let busy = match commands::in_chat(id) {
                    InChat::Agent => own,
                    InChat::Over | InChat::Main => self.busy(),
                };
                if busy && let Some(hint) = commands::between_turns(id) {
                    return self.hint(hint, at);
                }
                if let Some(model) = self.chat_model_mut() {
                    model.composer.take_saved();
                }
                if (id, args) == (CommandId::Endpoint, copy::ADD_WORD) {
                    self.close_agents(at);
                }
                self.leave_chat_for(id, at);
                return self.run_command(id, args, at);
            }
            Parsed::Plugin {
                command,
                base,
                args,
            } => {
                self.typed_plugin(command, base, args, own, at);
                return;
            }
        }
        let Some(model) = self.agent_models.get_mut(&path) else {
            return;
        };
        let turn = model.inputs.running_turn();
        let mode = if turn.is_some() {
            mode
        } else {
            DeliveryMode::Steer
        };
        let (message, saved) = model.composer.take_saved();
        let id = self.inputs.next_id();
        let send = send_command(id.clone(), &message, mode, turn, None);
        if !self.child_command(path.clone(), send, at) {
            if let Some(model) = self.agent_models.get_mut(&path) {
                model.composer.restore_ahead(&[saved]);
            }
            return;
        }
        let cx = self.cx();
        let effort = self
            .agents
            .row(&path)
            .and_then(|r| r.effort)
            .map(|e| e.as_str());
        if let Some(model) = self.agent_models.get_mut(&path) {
            model.inputs.sent(id, mode, message.parts, saved);
            model.view.to_bottom();
            if turn.is_none() {
                model.status.submitted(at, cx);
                model.status.turn_effort(effort);
            }
        }
    }

    pub(super) fn agents_shown(&self, room: usize) -> bool {
        self.chat.is_none() && room >= AGENTS_MIN_ROWS
    }

    pub(super) fn hide_agents(&mut self) {
        if let Some(list) = &mut self.agents_list {
            list.drawn = None;
        }
    }

    fn agents_view_lines(
        &mut self,
        room: usize,
        bottom: u16,
        at: Duration,
        chrome: Layout,
    ) -> Vec<StyledLine> {
        let shown = self.agents_shown(room);
        let Some(list) = &mut self.agents_list else {
            return vec![];
        };
        if !shown {
            list.drawn = None;
            return vec![];
        }
        let mouse_cell = list
            .drawn
            .as_ref()
            .and_then(|d| d.mouse_cell)
            .filter(|_| self.mouse);
        let hovered = mouse_cell
            .filter(|&(_, row)| row < bottom)
            .and_then(|cell| self.dot_at(cell, at));
        let glyphs = Glyphs::of(chrome.glyphs);
        let label = hovered.as_ref().and_then(|path| {
            let row = if path.is_root() {
                None
            } else {
                Some(self.agents.row(path)?)
            };
            Some((key(path), agent_label(row, &glyphs).1))
        });
        let picked = self.agent_picked();
        let height = room.saturating_sub(PAGE_CHROME);
        let drawn = self.drawn_nodes();
        let Drawing {
            lines: body,
            scenes,
            moving,
        } = match drawn {
            Some(drawn) => page(
                &Topology {
                    label,
                    ..self.topology(drawn, &picked, at)
                },
                height,
            ),
            None if self.sidebar.offers(AGENTS_SECTION) => Drawing::default(),
            None => {
                let plugin = AGENTS_SECTION.split('/').next().unwrap_or(AGENTS_SECTION);
                let sep = Glyphs::of(chrome.glyphs).separator;
                Drawing {
                    lines: agents_note(&section_off(plugin, sep)),
                    ..Drawing::default()
                }
            }
        };
        let (body, moves) = match drawn {
            Some(_) if body.is_empty() => {
                let moves = if self.list_room() > 0 && self.agents.any_live() {
                    Moves::UpDown
                } else {
                    Moves::Not
                };
                (agents_note(copy::NO_AGENTS), moves)
            }
            Some(_) => (body, Moves::Around),
            None if self.agents.any_live() => (body, Moves::UpDown),
            None => (body, Moves::Not),
        };
        let rows = self.agents.rows();
        let main = self.main_row();
        let view = AgentsView {
            main: &main,
            rows,
            t: at,
            now_ms: self.now_ms(),
        };
        let keys = AgentKeys {
            opens: moves != Moves::Not,
            release: agent_row(rows, &picked).is_some_and(releasable),
            moves,
        };
        let panel = panel_rows(&view, &picked, chrome);
        let AgentsTree {
            lines: tree,
            shown: listed,
        } = agents_tree(self.listed_agents(), &picked, height, chrome);
        let body = beside(tree, body, self.list_room(), chrome);
        let lines = agents_page(body, panel, keys, room, chrome);
        let top = room.saturating_sub(lines.len()) + 1;
        if let Some(list) = &mut self.agents_list {
            let top = u16::try_from(top).unwrap_or(u16::MAX);
            let fits = scenes.iter().all(|s| s.rows.end <= height);
            let visible = usize::from(bottom.saturating_sub(top));
            let scenes = scenes
                .into_iter()
                .filter(|s| fits && s.rows.start < visible)
                .map(|mut s| {
                    s.rows.end = s.rows.end.min(visible);
                    Rc::new(s)
                });
            list.drawn = Some(Paged {
                top,
                height,
                mouse_cell,
                listed,
                scenes: scenes.collect(),
                moving,
            });
        }
        lines
    }

    pub(super) fn chat_head(&self, room: u16) -> Vec<StyledLine> {
        let rows = self.agents.rows();
        let Some(row) =
            (self.chat.as_ref().filter(|_| room >= 2)).and_then(|chat| agent_row(rows, &chat.path))
        else {
            return vec![];
        };
        Vec::from(chat_header(row, &self.account_billings(), self.layout()))
    }

    pub(super) fn agents_due(&self, at: Duration) -> Option<Duration> {
        let rows = self.agents.rows();
        let main = self.main_row();
        let now_ms = self.now_ms();
        let clocks = clocks_next_change(rows, at, now_ms);
        let open = self.agents_list.as_ref().filter(|_| self.chat.is_none());
        let view = AgentsView {
            main: &main,
            rows,
            t: at,
            now_ms,
        };
        let list = open.and_then(|list| agents_next_change(&view, &list.picked));
        let working =
            |(_, nodes, _): Drawn<'_>| nodes.iter().any(|n| n.state == NodeState::Working);
        let pulse = open
            .and_then(|_| self.drawn_nodes())
            .filter(|drawn| working(*drawn) && self.cx().pulses())
            .map(|_| at.saturating_add(PULSE_STEP));
        let flight = (open.and_then(|list| list.drawn.as_ref()))
            .filter(|drawn| drawn.moving)
            .map(|_| at.saturating_add(FRAME_MIN));
        let watched = self
            .chat_model()
            .and_then(|model| model.transcript.next_change());
        [clocks, list, pulse, flight, watched]
            .into_iter()
            .flatten()
            .min()
    }

    pub(super) fn draw_agents(
        &mut self,
        buffer: &mut Buffer,
        view_rows: u16,
        bottom: u16,
        at: Duration,
        chrome: Layout,
    ) -> Option<Rect> {
        if bottom == 0 {
            return None;
        }
        let lines = self.agents_view_lines(usize::from(view_rows), bottom, at, chrome);
        let top = view_rows.saturating_sub(u16::try_from(lines.len()).unwrap_or(u16::MAX));
        let height = bottom.checked_sub(top)?.min(view_rows.saturating_sub(top));
        if height == 0 {
            return None;
        }
        let width = buffer.area.width;
        paint_lines(
            buffer,
            top,
            width,
            lines.iter().take(usize::from(height)),
            &self.theme,
        );
        Some(Rect::new(0, top, width, height))
    }
}
