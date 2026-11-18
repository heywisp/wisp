use std::time::Duration;
use wisp_protocol::settings::names;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Position, Rect};
use tokio::time::Instant;
use wisp_protocol::{AgentEvent, ClientCommand, RequestId};
use wisp_render::chrome::config_screen::{
    DETAIL_ROWS, Detail, LOADING, MARK_FROM, SCREEN_TITLE, TOO_SMALL, detail_block_view,
    detail_view, filter_line, fits, fold, list_lines, marked, screen_header, screen_note,
    screen_rule,
};
use wisp_render::chrome::{Effect, KeyHint, Rebuild, Scope, Told, logo, menu_rows, status_row};
use wisp_render::markdown::Layout;
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::copy;

use super::{AppError, Presenter, paint_lines};
use crate::app::config_screen::{
    ASK_KEYS, Act, BACK_KEY, CANCEL_KEY, CHOOSE_KEYS, CLEAR_KEY, DONE_KEY, EndpointOp, FILTER_KEY,
    HELP_KEY, MOVE_KEYS, Many, Pick, Quote, ResetCost, SAVE_KEY, STOP_KEY, Save, Screen, Sent,
    Spot, TOGGLE_KEY, Widget, after_reset, can_reset, widget,
};
use crate::app::picker::{Picked, Picker};
use crate::term::{Console, Key, Mouse, MouseKind};

const BODY_TOP: u16 = 2;
const MIN_LIST_ROWS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) struct ListArea {
    pub y: u16,
    pub rows: u16,
    pub width: u16,
}

impl Presenter {
    pub(super) fn open_settings(&mut self, words: &str, at: Duration) {
        if !fits(self.size.width, self.size.height) {
            self.hint(TOO_SMALL, at);
            return;
        }
        let Some(request) = self.ask_described(at) else {
            return;
        };
        self.clear_selection();
        let draft = self.settings_draft.take();
        let screen = match draft {
            Some(draft) if words.trim().is_empty() => Screen::restored(request, draft),
            _ => Screen::opening(request, words),
        };
        let mut screen = screen;
        screen.later = self.settings_later.clone();
        screen.credentials_shown(self.credentials_shown());
        screen.set_keys_file(self.keys_file_shown());
        screen.live_endpoint(self.facts.endpoint_name.clone());
        self.settings_screen = Some(screen);
        self.settings_list = None;
        self.painter.invalidate();
        self.frames.mark();
    }

    pub(super) fn close_settings(&mut self) {
        self.settings_draft = None;
        self.drop_settings();
    }

    pub(super) fn set_aside_settings(&mut self) {
        if let Some(screen) = &self.settings_screen {
            self.settings_draft = Some(screen.draft());
        }
        self.drop_settings();
    }

    fn drop_settings(&mut self) {
        if let Some(screen) = &self.settings_screen {
            self.settings_later = screen.later.clone();
        }
        if self.picker.as_ref().is_some_and(Picker::saves) {
            self.picker = None;
        }
        if self.settings_screen.take().is_some() {
            self.settings_list = None;
            self.painter.park(None);
            self.painter.invalidate();
            self.frames.mark();
        }
    }

    fn settings_save(&mut self, save: Save, at: Duration) {
        let request = self.next_request();
        let Save {
            name,
            change,
            told,
            save,
        } = save;
        let command = ClientCommand::ChangeSettings {
            request,
            changes: vec![change],
            save,
        };
        if !self.command(command, at) {
            return;
        }
        if save {
            self.tracker.asked_saved(request, Some(told));
        } else {
            self.tracker.asked_settings(request, None, None, None);
        }
        if let Some(screen) = &mut self.settings_screen {
            screen.sent(request, &[name], save);
        }
    }

    fn settings_endpoint_op(&mut self, sent: Sent, at: Duration) {
        let request = self.next_request();
        let command = match &sent.op {
            EndpointOp::Edit {
                name,
                edit,
                store,
                check,
            } => ClientCommand::EditEndpoint {
                request,
                name: name.clone(),
                edit: edit.clone(),
                store: *store,
                check: *check,
            },
            EndpointOp::Remove { name, keep_key } => ClientCommand::RemoveEndpoint {
                request,
                name: name.clone(),
                keep_key: *keep_key,
            },
        };
        if !self.command(command, at) {
            return;
        }
        self.tracker.asked_endpoint(request);
        if let Some(screen) = &mut self.settings_screen {
            screen.sent_op(request, sent);
        }
    }

    fn settings_save_many(&mut self, many: Many, at: Duration) {
        let request = self.next_request();
        let command = ClientCommand::ChangeSettings {
            request,
            changes: many.changes.clone(),
            save: true,
        };
        if !self.command(command, at) {
            return;
        }
        self.tracker.asked_saved(request, None);
        if let Some(going) = super::gone::GoingOff::of(request, &many.changes) {
            self.authentication.going_off = Some(going);
        }
        if let Some(screen) = &mut self.settings_screen {
            screen.sent_many(request, &many);
        }
    }

    fn settings_use_endpoint(&mut self, name: String, at: Duration) {
        let sent = self.settings_picked(
            Picked::Settings {
                endpoint: Some(name.clone()),
                model: None,
                effort: None,
            },
            at,
        );
        if let Some(request) = sent
            && let Some(screen) = &mut self.settings_screen
        {
            screen.sent_spot(request, Spot::Endpoint(name));
        }
    }

    pub(super) fn settings_model_added(&mut self, picked: &Picked, at: Duration) {
        let id = match picked {
            Picked::Settings {
                model: Some(id), ..
            }
            | Picked::Effort { model: id, .. } => id,
            Picked::Settings { model: None, .. } => return,
        };
        if let Some(screen) = &mut self.settings_screen {
            let act = screen.open_model(id);
            if let Act::Hint(text) = act {
                self.hint(text, at);
            }
        }
    }

    pub(super) fn settings_endpoints_changed(&mut self) {
        if self.settings_screen.is_some() {
            self.describe_again();
        }
    }

    fn settings_pick(&mut self, pick: Pick) {
        let picker = match pick {
            Pick::Model => Picker::models(),
            Pick::Endpoint => return,
            Pick::Effort => Picker::efforts(self.tracker.wanted().model, false, None),
        };
        self.picker = Some(picker.saving());
    }

    pub(super) fn settings_picked(&mut self, picked: Picked, at: Duration) -> Option<RequestId> {
        let Picked::Settings {
            endpoint,
            model,
            effort,
        } = &picked
        else {
            if let Picked::Effort {
                model, preselect, ..
            } = picked
            {
                self.picker = Some(Picker::efforts(model, true, preselect).saving());
            }
            return None;
        };
        let names: Vec<&'static str> = [
            (names::ENDPOINT, endpoint.is_some()),
            (names::MODEL, model.is_some()),
            (names::EFFORT, effort.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect();
        let tolds: Vec<Told> = names
            .iter()
            .filter_map(|name| {
                self.settings_screen
                    .as_ref()
                    .and_then(|screen| screen.told_for(name))
            })
            .collect();
        let told = joined_told(tolds)?;
        let request = self.next_request();
        let command = ClientCommand::ChangeSettings {
            request,
            changes: ClientCommand::shortcut_changes(endpoint.clone(), model.clone(), *effort),
            save: true,
        };
        if !self.command(command, at) {
            return None;
        }
        self.tracker.asked_saved_pair(request, &picked, told);
        if let Some(screen) = &mut self.settings_screen {
            screen.sent(request, &names, true);
        }
        Some(request)
    }

    fn reset_effect(&self, at: Duration) -> ResetCost {
        let Some(row) = self.settings_screen.as_ref().and_then(|s| s.selected()) else {
            return ResetCost::Applies;
        };
        let Some(Widget::Picker(pick)) = widget(row.spec) else {
            return ResetCost::Applies;
        };
        if !can_reset(row) {
            return ResetCost::Applies;
        }
        let to = if crate::app::config_screen::chat_only(row) {
            crate::app::config_screen::new_chats(row)
        } else {
            after_reset(row).map_or_else(|| row.default.clone(), |f| f.value.clone())
        };
        let effect = match pick {
            Pick::Model => self.tracker.model_effect(&self.facts, &to, at),
            Pick::Endpoint => {
                if self.facts.endpoint_facts(&to).is_none() {
                    return ResetCost::Unknown;
                }
                self.tracker.endpoint_effect(&self.facts, &to, at)
            }
            Pick::Effort => {
                let Ok(level) = to.parse() else {
                    return ResetCost::Applies;
                };
                let model = self.tracker.wanted().model;
                self.tracker.effort_effect(&self.facts, &model, level, at)
            }
        };
        if matches!(
            effect,
            Effect::Current | Effect::Kept | Effect::KeptIfPassedOn
        ) {
            return ResetCost::Quoted(String::new());
        }
        ResetCost::Quoted(effect.text(self.glyphs))
    }

    fn settings_quote(&self, at: Duration) -> Quote {
        let model = self.tracker.wanted().model;
        let rebuild = self.tracker.rebuild(&self.facts, &model, Scope::UpTo, at);
        let tools = match self.tracker.rebuild(&self.facts, &model, Scope::Exact, at) {
            Rebuild::Empty | Rebuild::Expired | Rebuild::Unknown => String::new(),
            cost => cost.line(self.glyphs),
        };
        Quote {
            rebuild: rebuild.amount(self.glyphs),
            reset: self.reset_effect(at),
            endpoint: self.endpoint_cost(at),
            tools,
            drops: self.tracker.holds_reasoning(),
        }
    }

    fn endpoint_cost(&self, at: Duration) -> Option<String> {
        let name = self.settings_screen.as_ref()?.selected_endpoint()?;
        self.facts.endpoint_facts(name)?;
        let effect = self.tracker.endpoint_effect(&self.facts, name, at);
        if matches!(
            effect,
            Effect::Current | Effect::Kept | Effect::KeptIfPassedOn
        ) {
            return None;
        }
        Some(effect.text(self.glyphs))
    }

    fn describe_again(&mut self) {
        let at = self.at(Instant::now());
        if let Some(request) = self.ask_described(at)
            && let Some(screen) = &mut self.settings_screen
        {
            screen.ask(request);
        }
    }

    pub(super) fn settings_event(&mut self, event: &AgentEvent) {
        let busy = self.busy();
        let Some(screen) = &mut self.settings_screen else {
            return;
        };
        let names = screen.used(event);
        if screen.saved(event, busy) {
            self.frames.mark();
        }
        if matches!(event, AgentEvent::TurnStarted { .. }) && screen.turn_started() {
            self.frames.mark();
        }
        if !names.is_empty() {
            let at = self.at(Instant::now());
            self.command(ClientCommand::SettingsUsed { names }, at);
        }
        let Some(screen) = &mut self.settings_screen else {
            return;
        };
        match event {
            AgentEvent::CatalogChanged { catalog } => {
                if screen.catalog_changed(catalog) {
                    self.frames.mark();
                }
                self.describe_again();
            }
            AgentEvent::SettingsSnapshot(snapshot) => {
                screen.live_endpoint(self.facts.endpoint_name.clone());
                if screen.answered(snapshot, &self.display) {
                    self.frames.mark();
                }
            }
            AgentEvent::Rejected {
                request: Some(request),
                ..
            } if screen.asked() == Some(*request) && screen.loading() => {
                if screen.edit().is_some() {
                    self.set_aside_settings();
                } else {
                    self.close_settings();
                }
            }
            AgentEvent::SettingsChanged { .. }
            | AgentEvent::EndpointsChanged { .. }
            | AgentEvent::EndpointEdited { .. }
            | AgentEvent::EndpointRemoved { .. } => {
                self.describe_again();
            }
            _ => {}
        }
    }

    pub(super) fn settings_turn_ended(&mut self, event: &AgentEvent) {
        let open = self
            .settings_screen
            .as_ref()
            .is_some_and(|screen| !screen.loading());
        if open && matches!(event, AgentEvent::TurnEnded { .. }) {
            self.describe_again();
        }
    }

    pub(super) fn settings_card_opened(&mut self, event: &AgentEvent) {
        let opened = matches!(event, AgentEvent::QuestionAsked { .. });
        if opened && self.card.is_some() {
            self.set_aside_settings();
        }
    }

    pub(super) fn settings_paste(&mut self, text: &str) {
        if self.card_open() {
            self.endpoint_paste(text);
        } else if let Some(picker) = &mut self.picker {
            picker.paste(text);
        } else if let Some(screen) = &mut self.settings_screen {
            screen.paste(text);
        }
    }

    pub(super) fn settings_mouse(&mut self, mouse: Mouse) {
        let rows = isize::try_from(self.wheel_rows()).unwrap_or(isize::MAX);
        let area = self.settings_list;
        if self.picker.is_some() {
            if mouse.kind == MouseKind::Press {
                self.picker = None;
                self.frames.mark();
            }
            return;
        }
        let Some(screen) = &mut self.settings_screen else {
            return;
        };
        match mouse.kind {
            MouseKind::WheelUp => screen.scroll(-rows),
            MouseKind::WheelDown => screen.scroll(rows),
            MouseKind::Press => {
                if let Some(area) = area
                    && mouse.column < area.width
                    && (area.y..area.y + area.rows).contains(&mouse.row)
                {
                    let entry = screen.top() + usize::from(mouse.row - area.y);
                    screen.click(entry);
                }
            }
            MouseKind::Drag | MouseKind::Release | MouseKind::RightPress | MouseKind::Move => {}
        }
        self.frames.mark();
    }

    pub(super) fn settings_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        if self.card_open() && !self.quitting {
            self.endpoint_key(key, at, console)?;
            self.frames.mark();
            return Ok(());
        }
        if self.picker.is_some() {
            if self.quitting {
                self.picker = None;
            } else {
                self.disarm();
                self.picker_key(key, at);
                self.frames.mark();
                return Ok(());
            }
        }
        let Some(screen) = &mut self.settings_screen else {
            return Ok(());
        };
        let act = screen.key(key);
        if act != Act::Interrupt {
            self.disarm();
        }
        match act {
            Act::Stay => {}
            Act::Close => self.close_settings(),
            Act::Hint(text) => self.hint(text, at),
            Act::Note(text) => self.hint(&text, at),
            Act::Save(save) => self.settings_save(*save, at),
            Act::Pick(pick) => self.settings_pick(pick),
            Act::Interrupt => self.settings_interrupt(at, console)?,
            Act::Repaint => self.painter.invalidate(),
            Act::Suspend => self.suspend(console)?,
            Act::Endpoint(sent) => self.settings_endpoint_op(*sent, at),
            Act::SaveMany(many) => self.settings_save_many(*many, at),
            Act::UseEndpoint(name) => self.settings_use_endpoint(name, at),
            Act::Prompt { name, url } => self.open_edit_card(&name, url, at),
            Act::AddEndpoint => self.open_endpoint_card(at),
            Act::AddModel => {
                self.adding_model = true;
                self.picker = Some(Picker::models().saving());
            }
        }
        self.frames.mark();
        Ok(())
    }

    fn settings_interrupt<C: Console>(
        &mut self,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        if self.select.showing() {
            return self.copy(console, at, true);
        }
        if self.quitting {
            self.exit = Some(self.forced_exit());
            return Ok(());
        }
        if self.busy() {
            self.armed = false;
            self.stop();
            return Ok(());
        }
        if self
            .settings_screen
            .as_mut()
            .is_some_and(Screen::clear_filter)
        {
            self.disarm();
            return Ok(());
        }
        if self.armed {
            self.quit();
        } else {
            self.armed = true;
            let hint = self.quit_hint();
            self.hint(&hint, at);
        }
        Ok(())
    }

    fn settings_facts(&self, layout: Layout) -> Vec<String> {
        let glyphs = Glyphs::of(layout.glyphs);
        let mut out = vec![self.facts.short_model()];
        if !self.facts.effort.is_empty() {
            out.push(self.facts.effort.clone());
        }
        out.push(match self.facts.credential() {
            Some(label) => label.full(&glyphs),
            None => copy::NO_API_CALLS.to_owned(),
        });
        out
    }

    fn settings_keys(&self, layout: Layout) -> Vec<KeyHint> {
        let screen = self.settings_screen.as_ref();
        let editing = screen.is_some_and(Screen::editing);
        let widget = screen.and_then(Screen::edit);
        let ask = screen.and_then(Screen::asking);
        let mut keys: Vec<(&str, &str)> = vec![];
        if self.busy() && !editing && widget.is_none() && ask.is_none() && self.picker.is_none() {
            keys.push(STOP_KEY);
        }
        if self.picker.is_some() {
            keys.extend([CANCEL_KEY, SAVE_KEY]);
        } else if ask.is_some() {
            keys.extend(ASK_KEYS.iter().copied());
        } else if let Some(edit) = widget {
            let wide = layout.width >= MARK_FROM;
            keys.push(CANCEL_KEY);
            if edit.is_checklist() {
                if wide {
                    keys.push(MOVE_KEYS);
                    if edit.toggles() {
                        keys.push(TOGGLE_KEY);
                    }
                }
                keys.push(SAVE_KEY);
            } else {
                if !edit.is_text() && wide {
                    keys.push(CHOOSE_KEYS);
                }
                keys.push(SAVE_KEY);
            }
        } else if editing {
            keys.extend([DONE_KEY, CLEAR_KEY]);
        } else if screen.is_some_and(Screen::expanded) {
            keys.extend([("?", copy::BACK_WORD), BACK_KEY]);
        } else {
            keys.push(FILTER_KEY);
            if layout.width >= MARK_FROM {
                keys.push(HELP_KEY);
            }
            keys.push(BACK_KEY);
        }
        let mut keys: Vec<KeyHint> = keys
            .into_iter()
            .map(|(key, does)| KeyHint::new(fold(key, layout), fold(does, layout)))
            .collect();
        if let Some(edit) = widget.filter(|e| e.sites.len() > 1)
            && let Some(next) = edit.sites.get((edit.site + 1) % edit.sites.len())
        {
            keys.insert(1, KeyHint::new(copy::TAB_KEY, next.label.clone()));
        }
        keys
    }

    fn settings_footer(&self, at: Duration, layout: Layout) -> StyledLine {
        let cx = self.cx();
        let glyphs = Glyphs::of(layout.glyphs);
        let mut sentence = vec![StyledSpan::new(SCREEN_TITLE, Token::Text)];
        if self.status.is_running() || self.status.shown_hint(at).is_some() {
            let live = self.status.sentence_stopless(at, cx, &glyphs);
            if !live.is_empty() {
                sentence.push(StyledSpan::new(
                    format!(" {} ", glyphs.separator),
                    Token::TextMuted,
                ));
                sentence.extend(live);
            }
        }
        let keys = self.settings_keys(layout);
        status_row(&self.status.orb(at, cx), sentence, &keys, &[], layout)
    }

    pub(super) fn settings_view(&mut self, at: Duration) -> Buffer {
        let cx = self.cx();
        self.transcript.tick(at, cx);
        self.seats.clear();
        self.hint_area = None;
        self.settings_list = None;
        let size = self.size;
        let theme = self.theme;
        let layout = self.chrome_layout();
        let mut buffer = Buffer::empty(Rect::new(0, 0, size.width, size.height));
        let put = |buffer: &mut Buffer, x: u16, y: u16, line: &StyledLine, width: u16| {
            if y < size.height {
                buffer.set_line(x, y, &theme.line(line), width);
            }
        };
        if !fits(size.width, size.height) {
            for (y, line) in screen_note(TOO_SMALL, Token::Text, layout)
                .iter()
                .enumerate()
            {
                put(
                    &mut buffer,
                    0,
                    u16::try_from(y).unwrap_or(u16::MAX),
                    line,
                    size.width,
                );
            }
            self.painter.park(None);
            self.chrome_due = None;
            return buffer;
        }
        let header = screen_header(&logo(cx), &self.settings_facts(layout), layout);
        put(&mut buffer, 0, 0, &header, size.width);
        let rule = screen_rule(None, layout);
        put(&mut buffer, 0, 1, &rule, size.width);
        let end = size.height - 2;
        put(&mut buffer, 0, end, &rule, size.width);
        let footer = self.settings_footer(at, layout);
        put(&mut buffer, 0, size.height - 1, &footer, size.width);
        let footer_at = Position {
            x: 1,
            y: size.height - 1,
        };
        self.seat_orb(&buffer, footer_at, at);
        self.place_idle(&mut buffer, at);
        self.chrome_due = self.status.next_change(at, cx);
        let quote = self.settings_quote(at);
        let mut park = self.settings_body(&mut buffer, end, layout, &quote);
        if let Some(top) = self.settings_menu(&mut buffer, end, layout, at) {
            park = Some(Position { x: 0, y: top });
        }
        if self.settings_card(&mut buffer, end, layout) {
            park = None;
        }
        self.painter.park(park);
        buffer
    }

    fn settings_card(&self, buffer: &mut Buffer, end: u16, layout: Layout) -> bool {
        if !self.card_open() {
            return false;
        }
        let room = end.saturating_sub(BODY_TOP);
        let mut area = Buffer::empty(Rect::new(0, 0, self.size.width, room));
        if self.draw_endpoint_card(&mut area, room, layout).is_none() {
            return false;
        }
        let from = first_drawn(&area);
        for y in from..room {
            for x in 0..self.size.width {
                if let (Some(cell), Some(to)) =
                    (area.cell((x, y)), buffer.cell_mut((x, y + BODY_TOP)))
                {
                    *to = cell.clone();
                }
            }
        }
        true
    }

    fn settings_menu(
        &self,
        buffer: &mut Buffer,
        end: u16,
        layout: Layout,
        at: Duration,
    ) -> Option<u16> {
        let picker = self.picker.as_ref()?;
        let menu = picker.menu(&self.facts, &self.tracker, at, self.glyphs);
        let room = usize::from(end.saturating_sub(BODY_TOP));
        let lines = menu_rows(&menu, room, layout);
        let top = end.saturating_sub(u16::try_from(lines.len()).unwrap_or(u16::MAX));
        paint_lines(buffer, top, self.size.width, &lines, &self.theme);
        (!lines.is_empty()).then_some(top)
    }

    fn settings_body(
        &mut self,
        buffer: &mut Buffer,
        end: u16,
        layout: Layout,
        quote: &Quote,
    ) -> Option<Position> {
        let theme = self.theme;
        let width = self.size.width;
        let put = |buffer: &mut Buffer, x: u16, y: u16, line: &StyledLine, w: u16| {
            if y < end {
                buffer.set_line(x, y, &theme.line(line), w);
            }
        };
        let screen = self.settings_screen.as_ref()?;
        let mut y = BODY_TOP;
        if let Some(text) = screen.broken() {
            for line in screen_note(&text, Token::Error, layout) {
                put(buffer, 0, y, &line, width);
                y += 1;
            }
        }
        if screen.loading() {
            let glyphs = Glyphs::of(layout.glyphs);
            let text = format!("{LOADING}{}", glyphs.more);
            for line in screen_note(&text, Token::TextMuted, layout) {
                put(buffer, 0, y, &line, width);
                y += 1;
            }
            return None;
        }
        self.settings_content(buffer, (y, end), layout, quote)
    }

    fn settings_list_at(
        &mut self,
        buffer: &mut Buffer,
        y: u16,
        rows: u16,
        layout: Layout,
    ) -> Option<Position> {
        let theme = self.theme;
        let width = u16::try_from(layout.width).unwrap_or(u16::MAX);
        let height = self.size.height;
        let screen = self.settings_screen.as_mut()?;
        screen.spaced(height, usize::from(rows));
        screen.fit(usize::from(rows));
        let (entries, _) = screen.entries();
        let selected = screen.selected_entry();
        let top = screen.top();
        let marked = marked(usize::from(self.size.width));
        let lines = list_lines(&entries, selected, (top, usize::from(rows)), marked, layout);
        for (i, line) in lines.iter().enumerate() {
            let row = y + u16::try_from(i).unwrap_or(0);
            buffer.set_line(0, row, &theme.line(line), width);
        }
        let glyphs = Glyphs::of(layout.glyphs);
        if top > 0 {
            let above = y - 1;
            buffer.set_line(
                0,
                above,
                &theme.line(&screen_rule(Some(glyphs.more_up), layout)),
                width,
            );
        }
        if top + usize::from(rows) < entries.len() {
            buffer.set_line(
                0,
                y + rows,
                &theme.line(&screen_rule(Some(glyphs.more_down), layout)),
                width,
            );
        }
        self.settings_list = Some(ListArea { y, rows, width });
        let at = selected?.checked_sub(top)?;
        let at = u16::try_from(at).ok().filter(|&at| at < rows)?;
        Some(Position { x: 0, y: y + at })
    }

    fn settings_filter(&self, buffer: &mut Buffer, y: u16, layout: Layout) -> Option<Position> {
        let screen = self.settings_screen.as_ref()?;
        let (line, cursor) = filter_line(screen.filter(), screen.editing(), layout);
        let width = u16::try_from(layout.width).unwrap_or(u16::MAX);
        buffer.set_line(0, y, &self.theme.line(&line), width);
        cursor.and_then(|x| u16::try_from(x).ok().map(|x| Position { x, y }))
    }

    fn settings_content(
        &mut self,
        buffer: &mut Buffer,
        (mut y, end): (u16, u16),
        layout: Layout,
        quote: &Quote,
    ) -> Option<Position> {
        let width = self.size.width;
        let screen = self.settings_screen.as_ref()?;
        let mut caret = None;
        if screen.editing() || !screen.filter().is_empty() {
            caret = self.settings_filter(buffer, y, layout);
            y += 1;
        }
        let screen = self.settings_screen.as_ref()?;
        let shown = screen.detail(quote);
        let place = |top: u16, at: Option<(usize, usize)>| {
            at.and_then(|(row, x)| {
                let row = top.checked_add(u16::try_from(row).ok()?)?;
                let x = u16::try_from(x).ok()?;
                (row < end).then_some(Position { x, y: row })
            })
        };
        if screen.expanded() {
            let mut cursor = None;
            if let Some(shown) = &shown {
                let room = usize::from(end.saturating_sub(y));
                let (lines, at) = detail_view(shown, room, layout);
                let skip = overflow(shown, lines.len(), room);
                cursor = place(y, at);
                for line in lines.into_iter().skip(skip) {
                    if y >= end {
                        break;
                    }
                    buffer.set_line(0, y, &self.theme.line(&line), width);
                    y += 1;
                }
            }
            return cursor.or(caret);
        }
        let room = usize::from(end.saturating_sub(y))
            .saturating_sub(MIN_LIST_ROWS + 1)
            .max(DETAIL_ROWS + 1);
        let (lines, at) = shown
            .as_ref()
            .map_or_else(Default::default, |s| detail_block_view(s, room, layout));
        let asking = shown.as_ref().is_some_and(|s| s.ask.is_some());
        let height = if asking || screen.edit().is_some() {
            lines.len()
        } else {
            DETAIL_ROWS
        };
        let block = u16::try_from(height).unwrap_or(u16::MAX) + 1;
        let rows = end.saturating_sub(y).saturating_sub(block);
        let rule_y = y + rows;
        buffer.set_line(
            0,
            rule_y,
            &self.theme.line(&screen_rule(None, layout)),
            width,
        );
        let selected = self.settings_list_at(buffer, y, rows, layout);
        let cursor = place(rule_y + 1, at);
        for (i, line) in lines.iter().enumerate() {
            let at = rule_y + 1 + u16::try_from(i).unwrap_or(u16::MAX);
            if at < end {
                buffer.set_line(0, at, &self.theme.line(line), width);
            }
        }
        cursor.or(caret).or(selected)
    }
}

fn overflow(detail: &Detail, lines: usize, room: usize) -> usize {
    if detail.ask.is_some() {
        lines.saturating_sub(room)
    } else {
        0
    }
}

fn first_drawn(area: &Buffer) -> u16 {
    let width = area.area.width;
    let blank = ratatui_core::buffer::Cell::EMPTY;
    (0..area.area.height)
        .find(|&y| (0..width).any(|x| area.cell((x, y)).is_some_and(|c| *c != blank)))
        .map_or(area.area.height, |y| y.saturating_sub(1))
}

fn joined_told(tolds: Vec<Told>) -> Option<Told> {
    if tolds.len() < 2 {
        return tolds.into_iter().next();
    }
    let was = tolds
        .iter()
        .map(|t| t.was.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let saved: Option<Vec<&str>> = tolds.iter().map(|t| t.saved.as_deref()).collect();
    Some(Told {
        was,
        saved: saved.map(|s| s.join(", ")),
        wins: None,
    })
}
