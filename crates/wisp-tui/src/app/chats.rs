use wisp_protocol::filter::Query;
use wisp_protocol::{ChatRow, RequestId};
use wisp_render::chrome::{ChatsPlace, ChatsView, short_model};
use wisp_render::transcript::copy;

use crate::app::list::ListCursor;
use crate::term::{Key, KeyCode};
use crate::widgets::line_edit::LineEdit;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Stay,
    Close,
    Pick(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answered {
    Other,
    Shown,
    Empty,
    Open(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chats {
    asked: Option<RequestId>,
    picked: Option<String>,
    chats: Vec<ChatRow>,
    query: String,
    focus: Option<String>,
}

impl Chats {
    pub fn asked(request: RequestId, query: &str) -> Chats {
        let mut chats = Chats {
            asked: Some(request),
            ..Chats::default()
        };
        chats.paste(query);
        chats
    }

    pub fn listed(chats: Vec<ChatRow>, focus: Option<&str>) -> Chats {
        Chats {
            chats,
            focus: focus.map(str::to_owned),
            ..Chats::default()
        }
    }

    pub fn loading(&self) -> bool {
        self.asked.is_some() && self.picked.is_none()
    }

    pub fn shown(&self) -> Vec<&ChatRow> {
        let query = Query::new(&self.query);
        self.chats
            .iter()
            .filter(|chat| {
                let model = chat.model.as_deref().map(short_model);
                let texts = [
                    chat.name.as_deref(),
                    Some(chat.topic.as_str()),
                    Some(chat.typed.as_str()),
                    chat.account.as_deref(),
                    model.as_deref(),
                ];
                query.matches(&texts.into_iter().flatten().collect::<Vec<_>>())
            })
            .collect()
    }

    pub fn focused(&self) -> Option<usize> {
        self.focus_in(&self.shown())
    }

    fn focus_in(&self, shown: &[&ChatRow]) -> Option<usize> {
        self.focus
            .as_deref()
            .and_then(|id| shown.iter().position(|chat| chat.id == id))
            .or_else(|| shown.iter().position(|chat| chat.can_open()))
            .or_else(|| (!shown.is_empty()).then_some(0))
    }

    pub fn chosen(&self) -> Option<&ChatRow> {
        let shown = self.shown();
        self.focus_in(&shown).and_then(|i| shown.get(i).copied())
    }

    pub fn view(&self, place: ChatsPlace, mut below: Vec<String>) -> ChatsView<'_> {
        let shown = self.shown();
        let focus = self.focus_in(&shown);
        if focus
            .and_then(|i| shown.get(i))
            .is_some_and(|chat| !chat.can_open())
        {
            below.push(copy::CONTINUE_THERE.to_owned());
        }
        ChatsView {
            chats: shown,
            listed: &self.chats,
            focus,
            query: &self.query,
            loading: self.loading(),
            below,
            place,
        }
    }

    pub fn checking(&mut self, request: RequestId, id: String) {
        self.asked = Some(request);
        self.picked = Some(id);
    }

    pub fn answered(&mut self, request: RequestId, chats: &[ChatRow]) -> Answered {
        if self.asked != Some(request) {
            return Answered::Other;
        }
        self.asked = None;
        self.chats = chats.to_vec();
        match self.picked.take() {
            Some(id) if chats.iter().any(|c| c.id == id && c.can_open()) => Answered::Open(id),
            Some(id) => {
                self.focus = Some(id);
                Answered::Shown
            }
            None if chats.is_empty() => Answered::Empty,
            None => Answered::Shown,
        }
    }

    fn step(&mut self, by: isize) {
        let shown = self.shown();
        let Some(at) = self.focus_in(&shown) else {
            return;
        };
        let to = ListCursor::CLAMP.by(at, shown.len(), by);
        self.focus = shown.get(to).map(|chat| chat.id.clone());
    }

    pub fn paste(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' | '\r' | '\t' => self.query.push(' '),
                c if c.is_control() => {}
                c => self.query.push(c),
            }
        }
        self.focus = None;
    }

    pub fn key(&mut self, key: Key) -> Act {
        if key.ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            return Act::Close;
        }
        if key.ctrl || key.alt {
            return Act::Stay;
        }
        match key.code {
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.focus = None;
                Act::Stay
            }
            KeyCode::Esc => Act::Close,
            _ if LineEdit::ANY.key(&mut self.query, key) => {
                self.focus = None;
                Act::Stay
            }
            _ if self.asked.is_some() => Act::Stay,
            code if let Some(by) = ListCursor::PICK.delta(code) => {
                self.step(by);
                Act::Stay
            }
            KeyCode::Enter => match self.chosen() {
                Some(chat) if chat.can_open() => Act::Pick(chat.id.clone()),
                _ => Act::Stay,
            },
            _ => Act::Stay,
        }
    }
}

#[cfg(test)]
mod tests;
