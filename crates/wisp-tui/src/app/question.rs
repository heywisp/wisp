use std::time::Duration;

use wisp_protocol::{Answer, Question, check_answers};
use wisp_render::chrome::{CardHit, CardMode, CardPage, CardView, Draft};

use crate::app::list::{Pick, Picked};
use crate::term::{Key, KeyCode};
use crate::widgets::composer::{Composer, Saved};

pub const SETTLE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settle(Duration);

impl Settle {
    pub fn after(last_typed: Option<Duration>, at: Duration) -> Settle {
        Settle(last_typed.map_or(at, |typed| typed.saturating_add(SETTLE).max(at)))
    }

    pub fn hold(&mut self, at: Duration) {
        self.0 = self.0.max(at.saturating_add(SETTLE));
    }

    pub fn settled(self, at: Duration) -> bool {
        at >= self.0
    }

    pub fn due(self, at: Duration) -> Option<Duration> {
        (self.0 > at).then_some(self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Send(Vec<Answer>),
    Stop,
}

struct Page {
    pick: Pick,
    chosen: Vec<u32>,
    own_chosen: bool,
    answered: bool,
    own: Composer,
}

impl Page {
    fn draft(&self, multi: bool) -> Draft {
        let own = self.own.text();
        Draft {
            chosen: self.chosen.clone(),
            own_chosen: if multi {
                !own.trim().is_empty()
            } else {
                self.own_chosen
            },
            own,
            answered: self.answered,
        }
    }
}

pub struct Card {
    call_id: String,
    questions: Vec<Question>,
    pages: Vec<Page>,
    page: CardPage,
    review: Pick,
    sending: bool,
}

pub struct Shown {
    pub drafts: Vec<Draft>,
    pub cursor: Option<(usize, usize)>,
}

fn printable(key: Key) -> Option<char> {
    match key.code {
        KeyCode::Char(c) if !key.ctrl && !key.alt && !c.is_control() => Some(c),
        _ => None,
    }
}

fn newline_key(key: Key) -> bool {
    match key.code {
        KeyCode::Enter => key.shift || key.alt || key.ctrl,
        KeyCode::Char('j' | 'J') => key.ctrl && !key.alt,
        _ => false,
    }
}

impl Card {
    pub fn new(call_id: String, questions: Vec<Question>) -> Card {
        let pages = questions
            .iter()
            .map(|q| Page {
                pick: Pick {
                    rows: q.options.len() + 1,
                    held: (0..=q.options.len())
                        .map(|row| row == q.options.len())
                        .collect(),
                    focus: Some(if q.is_typed() { q.options.len() } else { 0 }),
                },
                chosen: Vec::new(),
                own_chosen: false,
                answered: false,
                own: Composer::new(),
            })
            .collect();
        Card {
            call_id,
            questions,
            pages,
            page: CardPage::Question(0),
            review: Pick {
                rows: 2,
                focus: Some(0),
                ..Pick::default()
            },
            sending: false,
        }
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn questions(&self) -> &[Question] {
        &self.questions
    }

    pub fn page(&self) -> CardPage {
        self.page
    }

    pub fn row(&self) -> usize {
        match self.page {
            CardPage::Question(q) => self
                .pages
                .get(q)
                .map_or(0, |p| p.pick.focus.unwrap_or_default()),
            CardPage::Review => self.review.focus.unwrap_or_default(),
        }
    }

    pub fn is_sending(&self) -> bool {
        self.sending
    }

    pub fn sent(&mut self) {
        self.sending = true;
    }

    pub fn refused(&mut self) {
        self.sending = false;
    }

    pub fn mode(&self, holds: bool) -> CardMode {
        if self.sending {
            CardMode::Sending
        } else if holds {
            CardMode::Active
        } else {
            CardMode::Settling
        }
    }

    pub fn shown(&self) -> Shown {
        let drafts = self
            .questions
            .iter()
            .zip(&self.pages)
            .map(|(q, p)| p.draft(q.multi_select))
            .collect();
        let cursor = match self.page {
            CardPage::Question(q) => self
                .pages
                .get(q)
                .filter(|p| p.pick.focus == Some(self.own_row(q)))
                .map(|p| p.own.cursor()),
            CardPage::Review => None,
        };
        Shown { drafts, cursor }
    }

    pub fn view<'a>(&'a self, shown: &'a Shown, holds: bool) -> CardView<'a> {
        CardView {
            questions: &self.questions,
            drafts: &shown.drafts,
            page: self.page,
            row: self.row(),
            cursor: shown.cursor,
            mode: self.mode(holds),
        }
    }

    pub fn typed_texts(&self) -> Vec<Saved> {
        self.pages
            .iter()
            .zip(&self.questions)
            .filter(|(_, question)| !question.is_typed())
            .map(|(page, _)| page.own.text())
            .filter(|t| !t.trim().is_empty())
            .map(|t| Saved::from_text(&t))
            .collect()
    }

    pub fn answers(&self) -> Vec<Answer> {
        self.questions
            .iter()
            .zip(&self.pages)
            .map(|(q, p)| {
                let own = p.own.text();
                let has_own = !own.trim().is_empty();
                if q.multi_select {
                    let mut chosen = p.chosen.clone();
                    chosen.sort_unstable();
                    chosen.dedup();
                    Answer {
                        chosen,
                        own: has_own.then_some(own),
                    }
                } else if p.own_chosen && has_own {
                    Answer::own(own)
                } else {
                    Answer {
                        chosen: p.chosen.first().copied().into_iter().collect(),
                        own: None,
                    }
                }
            })
            .collect()
    }

    fn has_answer(&self, q: usize) -> bool {
        let multi = self.questions.get(q).is_some_and(|q| q.multi_select);
        self.pages
            .get(q)
            .is_some_and(|p| p.draft(multi).has_answer())
    }

    fn own_row(&self, q: usize) -> usize {
        self.questions.get(q).map_or(0, |q| q.options.len())
    }

    fn several(&self) -> bool {
        self.questions.len() > 1
    }

    fn next_page(&mut self) {
        if !self.several() {
            return;
        }
        self.page = match self.page {
            CardPage::Question(q) if q + 1 < self.questions.len() => CardPage::Question(q + 1),
            CardPage::Question(_) | CardPage::Review => {
                self.review.focus = Some(0);
                CardPage::Review
            }
        };
    }

    fn previous_page(&mut self) {
        if !self.several() {
            return;
        }
        self.page = match self.page {
            CardPage::Question(q) => CardPage::Question(q.saturating_sub(1)),
            CardPage::Review => CardPage::Question(self.questions.len().saturating_sub(1)),
        };
    }

    fn send(&self) -> Step {
        let answers = self.answers();
        if check_answers(&self.questions, &answers).is_err() {
            return Step::Stay;
        }
        if !self.several() && answers.iter().all(Answer::is_skipped) {
            return Step::Stay;
        }
        Step::Send(answers)
    }

    fn go_on(&mut self, from: usize) -> Step {
        if !self.several() {
            return self.send();
        }
        let n = self.questions.len();
        let next = (1..n)
            .map(|k| (from + k) % n)
            .find(|&i| !self.has_answer(i));
        match next {
            Some(i) => self.page = CardPage::Question(i),
            None => {
                self.page = CardPage::Review;
                self.review.focus = Some(0);
            }
        }
        Step::Stay
    }

    fn pick(&mut self, q: usize, i: usize) -> Step {
        let Some(page) = self.pages.get_mut(q) else {
            return Step::Stay;
        };
        page.pick.focus = Some(i);
        page.chosen = vec![u32::try_from(i).unwrap_or(u32::MAX)];
        page.own_chosen = false;
        page.answered = true;
        self.go_on(q)
    }

    fn toggle(&mut self, q: usize, i: usize) {
        let Some(page) = self.pages.get_mut(q) else {
            return;
        };
        page.pick.focus = Some(i);
        let index = u32::try_from(i).unwrap_or(u32::MAX);
        if let Some(at) = page.chosen.iter().position(|c| *c == index) {
            page.chosen.remove(at);
        } else {
            page.chosen.push(index);
            page.chosen.sort_unstable();
        }
    }

    fn enter_own(&mut self, q: usize) -> Step {
        let multi = self.questions.get(q).is_some_and(|q| q.multi_select);
        let Some(page) = self.pages.get_mut(q) else {
            return Step::Stay;
        };
        let blank = page.own.is_blank();
        if !multi {
            if blank {
                return Step::Stay;
            }
            page.own_chosen = true;
            page.chosen.clear();
        }
        page.answered = true;
        self.go_on(q)
    }

    fn choose(&mut self, q: usize, i: usize) -> Step {
        let multi = self.questions.get(q).is_some_and(|q| q.multi_select);
        if multi {
            self.toggle(q, i);
            Step::Stay
        } else {
            self.pick(q, i)
        }
    }

    pub fn key(&mut self, key: Key) -> Step {
        let ctrl_c = matches!(key.code, KeyCode::Char('c' | 'C')) && key.ctrl && !key.alt;
        if key.code == KeyCode::Esc || ctrl_c {
            return Step::Stop;
        }
        if self.sending {
            return Step::Stay;
        }
        match key.code {
            KeyCode::BackTab => {
                self.previous_page();
                return Step::Stay;
            }
            KeyCode::Tab if !key.ctrl && !key.alt => {
                if key.shift {
                    self.previous_page();
                } else {
                    self.next_page();
                }
                return Step::Stay;
            }
            _ => {}
        }
        match self.page {
            CardPage::Question(q) => self.question_key(q, key),
            CardPage::Review => self.review_key(key),
        }
    }

    fn question_key(&mut self, q: usize, key: Key) -> Step {
        let own = self.own_row(q);
        let multi = self.questions.get(q).is_some_and(|q| q.multi_select);
        let row = self.row();
        let on_own = row == own;
        let plain = !key.ctrl && !key.alt;
        if newline_key(key) {
            if on_own && let Some(page) = self.pages.get_mut(q) {
                page.own.newline();
            }
            return Step::Stay;
        }
        match key.code {
            KeyCode::Up | KeyCode::Down if plain => {
                if let Some(page) = self.pages.get_mut(q) {
                    page.pick.key(key);
                }
                Step::Stay
            }
            KeyCode::Right if plain && !on_own => {
                self.next_page();
                Step::Stay
            }
            KeyCode::Left if plain && !on_own => {
                self.previous_page();
                Step::Stay
            }
            KeyCode::Enter if on_own => self.enter_own(q),
            KeyCode::Enter if multi => {
                if let Some(page) = self.pages.get_mut(q) {
                    page.answered = true;
                }
                self.go_on(q)
            }
            KeyCode::Enter => self.pick(q, row),
            _ if on_own => {
                if let Some(page) = self.pages.get_mut(q) {
                    page.own.edit(key);
                }
                Step::Stay
            }
            KeyCode::Char(' ' | '1'..='9') if plain => {
                match self.pages.get_mut(q).map(|page| page.pick.key(key)) {
                    Some(Picked::Took(i)) => self.choose(q, i),
                    Some(Picked::Moved) => Step::Stay,
                    _ => self.type_into(q, key),
                }
            }
            _ if printable(key).is_some() => self.type_into(q, key),
            _ => Step::Stay,
        }
    }

    fn type_into(&mut self, q: usize, key: Key) -> Step {
        let own = self.own_row(q);
        if let Some(page) = self.pages.get_mut(q) {
            page.pick.focus = Some(own);
            page.own.edit(key);
        }
        Step::Stay
    }

    fn review_key(&mut self, key: Key) -> Step {
        if key.code == KeyCode::Left && !key.ctrl && !key.alt {
            self.previous_page();
            return Step::Stay;
        }
        match self.review.key(key).took() {
            Some(0) => self.send(),
            Some(_) => {
                self.go_back();
                Step::Stay
            }
            None => Step::Stay,
        }
    }

    fn go_back(&mut self) {
        let first_open = (0..self.pages.len()).find(|&i| !self.has_answer(i));
        let last = self.questions.len().saturating_sub(1);
        self.page = CardPage::Question(first_open.unwrap_or(last));
    }

    pub fn paste(&mut self, text: &str) {
        if self.sending {
            return;
        }
        if let CardPage::Question(q) = self.page {
            let own = self.own_row(q);
            if let Some(page) = self.pages.get_mut(q) {
                page.pick.focus = Some(own);
                page.own.insert(text);
            }
        }
    }

    pub fn click(&mut self, hit: CardHit) -> Step {
        if self.sending {
            return Step::Stay;
        }
        match (self.page, hit) {
            (CardPage::Question(q), CardHit::Option(i)) => self
                .pages
                .get_mut(q)
                .and_then(|p| p.pick.click(i).took())
                .map_or(Step::Stay, |i| self.choose(q, i)),
            (CardPage::Question(q), CardHit::Own) => {
                let own = self.own_row(q);
                if let Some(page) = self.pages.get_mut(q) {
                    page.pick.focus = Some(own);
                }
                Step::Stay
            }
            (CardPage::Review, CardHit::Send) => self.send(),
            (CardPage::Review, CardHit::Back) => {
                self.go_back();
                Step::Stay
            }
            _ => Step::Stay,
        }
    }
}

#[cfg(test)]
mod tests;
