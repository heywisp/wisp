use std::collections::BTreeSet;

use wisp_protocol::{InputId, MailFrom, MailKind, TurnOutcome};
use wisp_render::transcript::Transcript;

#[derive(Clone, Debug, Default)]
pub(crate) struct MailBook {
    mail: Vec<Mailed>,
    drawn: BTreeSet<InputId>,
}

#[derive(Clone, Debug)]
struct Mailed {
    id: InputId,
    from: MailFrom,
    text: String,
    delivered: bool,
}

impl MailBook {
    pub(crate) fn accepted(
        &mut self,
        id: &InputId,
        from: &MailFrom,
        text: &str,
        idle: bool,
        transcript: &mut Transcript,
    ) {
        if from.kind == MailKind::Question && !from.wake {
            return;
        }
        if !self.mail.iter().any(|m| &m.id == id) {
            self.mail.push(Mailed {
                id: id.clone(),
                from: from.clone(),
                text: text.to_owned(),
                delivered: false,
            });
        }
        if idle && from.kind != MailKind::Question {
            self.draw(id, transcript);
        }
    }

    pub(crate) fn delivered(&mut self, ids: &[InputId], transcript: &mut Transcript) {
        for id in ids {
            if let Some(m) = self.mail.iter_mut().find(|m| &m.id == id) {
                m.delivered = true;
            }
            self.draw(id, transcript);
        }
    }

    pub(crate) fn ended(&mut self, outcome: &TurnOutcome, transcript: &mut Transcript) {
        if matches!(outcome, TurnOutcome::Completed { .. }) {
            self.mail.retain(|m| !m.delivered);
        } else {
            self.mail.iter_mut().for_each(|m| m.delivered = false);
        }
        let ids: Vec<InputId> = self
            .mail
            .iter()
            .filter(|m| m.from.kind != MailKind::Question)
            .map(|m| m.id.clone())
            .collect();
        for id in &ids {
            self.draw(id, transcript);
        }
    }

    pub(crate) fn waiting(&self) -> usize {
        self.mail
            .iter()
            .filter(|m| !m.delivered && m.from.kind != MailKind::Question)
            .count()
    }

    pub(crate) fn waking(&self) -> bool {
        self.mail
            .iter()
            .any(|m| !m.delivered && m.from.wake && m.from.kind != MailKind::Question)
    }

    pub(crate) fn seed(&mut self, drawn: impl IntoIterator<Item = InputId>) {
        self.drawn.extend(drawn);
    }

    pub(crate) fn drawn(&self) -> impl Iterator<Item = InputId> + '_ {
        self.drawn.iter().cloned()
    }

    pub(crate) fn clear(&mut self) {
        self.mail.clear();
    }

    fn draw(&mut self, id: &InputId, transcript: &mut Transcript) {
        if let Some(m) = self.mail.iter().find(|m| &m.id == id)
            && self.drawn.insert(id.clone())
        {
            if m.from.kind == MailKind::Question {
                transcript.answer_given(m.from.mail.as_str());
            } else {
                transcript.mail(&m.from, &m.text);
            }
        }
    }
}
