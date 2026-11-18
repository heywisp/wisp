use std::time::Duration;

use wisp_protocol::{AgentEvent, ClientCommand};
use wisp_render::chrome::{ChatsPlace, chats_lines};
use wisp_render::markdown::Layout;
use wisp_render::styled::StyledLine;
use wisp_render::transcript::copy;

use super::Presenter;
use crate::app::chats::{Act, Answered, Chats};
use crate::term::Key;

impl Presenter {
    pub(super) fn open_chats(&mut self, query: &str, at: Duration) {
        if self.card_picker_refused(at) {
            return;
        }
        if !self.chats_fit() {
            self.hint(copy::PICKER_NO_ROOM, at);
            return;
        }
        let request = self.next_request();
        if self.command(ClientCommand::ListChats { request }, at) {
            self.picker = None;
            self.chats_refused = false;
            self.chats = Some(Chats::asked(request, query));
        }
    }

    pub(super) fn chats_key(&mut self, key: Key, at: Duration) {
        let Some(chats) = &mut self.chats else {
            return;
        };
        match chats.key(key) {
            Act::Stay => {}
            Act::Close => self.chats = None,
            Act::Pick(_) if self.facts.resume_refused => self.chats_refused = true,
            Act::Pick(id) => {
                let request = self.next_request();
                if self.command(ClientCommand::ListChats { request }, at)
                    && let Some(chats) = &mut self.chats
                {
                    chats.checking(request, id);
                }
            }
        }
        self.frames.mark();
    }

    pub(super) fn chats_paste(&mut self, text: &str) {
        if let Some(chats) = &mut self.chats {
            chats.paste(text);
        }
    }

    pub(super) fn chats_event(&mut self, event: &AgentEvent) {
        let AgentEvent::ChatsListed { request, chats } = event else {
            return;
        };
        let Some(open) = &mut self.chats else {
            return;
        };
        match open.answered(*request, chats) {
            Answered::Other | Answered::Shown => {}
            Answered::Empty => {
                self.chats = None;
                self.transcript.note(&copy::no_other_chat(&self.facts.cwd));
            }
            Answered::Open(id) => {
                self.chats = None;
                self.resume_to = Some(id);
                self.quit();
            }
        }
        self.frames.mark();
    }

    pub(super) fn chats_view(&self, room: usize, chrome: Layout) -> Vec<StyledLine> {
        let Some(chats) = &self.chats else {
            return Vec::new();
        };
        let place = ChatsPlace::InChat(super::joins(self.rows_with(1, false)));
        chats_lines(&chats.view(place, self.chats_notes()), room, chrome)
    }

    pub(super) fn chats_notes(&self) -> Vec<String> {
        [
            self.facts.log.is_none().then(|| copy::NOT_SAVED.to_owned()),
            self.chats_refused
                .then(|| copy::RESUME_NEEDS_LOG.to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}
