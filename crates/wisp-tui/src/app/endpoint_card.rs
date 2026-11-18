use std::fmt;

use wisp_protocol::endpoint::{
    CheckKind, CheckResult, ConnectionField, EndpointEdit, EndpointSpec, NotSavedReason,
    StoreChoice, StoreKind, UrlEdit, loopback_host,
};
use wisp_protocol::{
    AgentEvent, IN_KEYCHAIN, MAX_ENDPOINT_NAME, RequestId, Secret, check_key, endpoint_char,
    valid_endpoint_name,
};
use wisp_render::chrome::KeyHint;
use wisp_render::chrome::endpoint_card::{
    self as words, AddAnswer, AddQuestion, EndpointCardView, FieldRow, FieldValue, MenuView,
    StatusTone,
};
use wisp_render::text::sanitize;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::copy;

use crate::app::list::{ListCursor, Pick};
use crate::term::{Key, KeyCode};
use crate::widgets::line_edit::LineEdit;
use crate::widgets::secret::SecretField;

pub const MAX_URL: usize = 2048;
const URL: LineEdit = LineEdit {
    max: MAX_URL,
    ..LineEdit::TEXT
};

pub use wisp_protocol::opening::{AuthEntry, CardSetup, ProviderEntry};
pub use wisp_render::chrome::endpoint_facts_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardMode {
    FirstRun,
    Add,
    Edit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Provider,
    Connect,
    Url,
    Auth,
    Key,
    Name,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveRequest {
    pub endpoint: EndpointSpec,
    pub key: Secret,
    pub check: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRequest {
    pub name: String,
    pub edit: EndpointEdit,
    pub store: Option<StoreChoice>,
    pub check: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Request {
    Save(SaveRequest),
    Edit(EditRequest),
}

impl Request {
    fn name(&self) -> &str {
        match self {
            Request::Save(save) => &save.endpoint.name,
            Request::Edit(edit) => &edit.name,
        }
    }

    fn set_check(&mut self, check: bool) {
        match self {
            Request::Save(save) => save.check = check,
            Request::Edit(edit) => edit.check = check,
        }
    }

    fn keep_in_file(&mut self) {
        match self {
            Request::Save(save) => save.endpoint.store = StoreChoice::File,
            Request::Edit(edit) => edit.store = Some(StoreChoice::File),
        }
        self.set_check(false);
    }

    fn step(self) -> Step {
        match self {
            Request::Save(save) => Step::Send(save),
            Request::Edit(edit) => Step::Edit(edit),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    Quit,
    Send(SaveRequest),
    Edit(EditRequest),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answered {
    Not,
    Shown,
    Saved {
        name: String,
        store: StoreKind,
        notes: Vec<String>,
    },
    Edited {
        name: String,
        changed: Vec<ConnectionField>,
        notes: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Asking {
    ask: AddQuestion,
    question: String,
    answers: &'static [AddAnswer],
    pick: Pick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    Editing,
    Asking(Asking),
    Saving(Option<RequestId>),
    Saved,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EndpointCard {
    mode: CardMode,
    pub(super) setup: CardSetup,
    taken: Vec<String>,
    provider: usize,
    gateway: bool,
    url: String,
    url_confirmed: Option<String>,
    auth: usize,
    key: SecretField,
    name: String,
    name_edited: bool,
    focus: Field,
    phase: Phase,
    status: Vec<(String, StatusTone)>,
    last: Option<Request>,
    quitting: bool,
    target: String,
    current: bool,
}

fn redacted(text: &str) -> &'static str {
    if text.is_empty() { "" } else { "<redacted>" }
}

impl fmt::Debug for EndpointCard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EndpointCard")
            .field("mode", &self.mode)
            .field("setup", &self.setup)
            .field("taken", &self.taken)
            .field("provider", &self.provider)
            .field("gateway", &self.gateway)
            .field("url", &redacted(&self.url))
            .field(
                "url_confirmed",
                &self.url_confirmed.as_deref().map(redacted),
            )
            .field("auth", &self.auth)
            .field("key", &self.key)
            .field("name", &self.name)
            .field("name_edited", &self.name_edited)
            .field("focus", &self.focus)
            .field("phase", &self.phase)
            .field("status", &self.status)
            .field("last", &self.last)
            .field("quitting", &self.quitting)
            .finish()
    }
}

fn host_of(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some(inner) = host_port.strip_prefix('[') {
        format!("[{}]", inner.split(']').next().unwrap_or_default())
    } else {
        host_port.split(':').next().unwrap_or_default().to_owned()
    };
    (!host.is_empty() && host != "[]").then_some(host)
}

fn http_scheme(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://")
}

pub fn suggested_name(provider: &str, url: Option<&str>, taken: &[String]) -> String {
    let base = match url.and_then(host_of) {
        None => wisp_protocol::default_api_account_name(provider).to_owned(),
        Some(host) => host
            .trim_start_matches('[')
            .split(['.', ':', ']'])
            .next()
            .unwrap_or_default()
            .to_owned(),
    };
    let cleaned: String = base
        .to_ascii_lowercase()
        .chars()
        .map(|c| if endpoint_char(c) { c } else { '-' })
        .collect();
    let cleaned: String = cleaned
        .trim_start_matches(['-', '_'])
        .chars()
        .take(MAX_ENDPOINT_NAME - 4)
        .collect();
    let base = if valid_endpoint_name(&cleaned) && cleaned != wisp_protocol::KEYS_FILE_ALIAS {
        cleaned
    } else {
        wisp_protocol::GATEWAY.to_owned()
    };
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|n| !taken.contains(n))
        .unwrap_or(base)
}

fn store_words(store: &StoreKind, file: &str) -> String {
    match store {
        StoreKind::Keyring => IN_KEYCHAIN.to_owned(),
        StoreKind::File => file.to_owned(),
        StoreKind::Other(text) => sanitize(text),
    }
}

impl EndpointCard {
    pub fn new(mode: CardMode, setup: CardSetup, taken: Vec<String>) -> EndpointCard {
        let mut card = EndpointCard {
            mode,
            setup,
            taken,
            provider: 0,
            gateway: false,
            url: String::new(),
            url_confirmed: None,
            auth: 0,
            key: SecretField::default(),
            name: String::new(),
            name_edited: false,
            focus: Field::Connect,
            phase: Phase::Editing,
            status: Vec::new(),
            last: None,
            quitting: false,
            target: String::new(),
            current: false,
        };
        card.focus = card.fields().first().copied().unwrap_or(Field::Connect);
        card.auth = card.official_auth();
        card.suggest();
        card
    }

    pub fn editing(setup: CardSetup, name: &str, url: bool, current: bool) -> EndpointCard {
        let mut card = EndpointCard::new(CardMode::Edit, setup, Vec::new());
        card.target = name.to_owned();
        card.current = current;
        card.gateway = url;
        card.focus = if url { Field::Url } else { Field::Key };
        card
    }

    pub fn mode(&self) -> CardMode {
        self.mode
    }

    pub fn focus(&self) -> Field {
        self.focus
    }

    pub fn saving(&self) -> bool {
        matches!(self.phase, Phase::Saving(_))
    }

    pub fn pending(&self) -> Option<RequestId> {
        match self.phase {
            Phase::Saving(request) => request,
            _ => None,
        }
    }

    pub fn pending_name(&self) -> Option<String> {
        self.pending()?;
        self.last.as_ref().map(|last| last.name().to_owned())
    }

    pub fn is_saved(&self) -> bool {
        self.phase == Phase::Saved
    }

    fn entry(&self) -> Option<&ProviderEntry> {
        self.setup.catalogue.get(self.provider)
    }

    fn official_auth(&self) -> usize {
        self.entry()
            .and_then(|p| p.auths.iter().position(|a| a.name == p.official_auth))
            .unwrap_or(0)
    }

    fn fields(&self) -> Vec<Field> {
        if self.mode == CardMode::Edit {
            return vec![if self.gateway { Field::Url } else { Field::Key }];
        }
        let mut fields = Vec::new();
        if self.setup.catalogue.len() > 1 {
            fields.push(Field::Provider);
        }
        fields.push(Field::Connect);
        if self.gateway {
            fields.push(Field::Url);
            if self.entry().is_some_and(|p| p.auths.len() > 1) {
                fields.push(Field::Auth);
            }
        }
        fields.push(Field::Key);
        fields.push(Field::Name);
        fields
    }

    fn suggest(&mut self) {
        if self.name_edited {
            return;
        }
        let provider = self.entry().map_or("", |p| p.name.as_str());
        let url = self.gateway.then_some(self.url.as_str());
        self.name = suggested_name(provider, url, &self.taken);
    }

    fn move_focus(&mut self, forward: bool) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        if let Some(field) = fields.get(ListCursor::CLAMP.step(at, fields.len(), forward)) {
            self.focus = *field;
        }
    }

    fn choice_count(&self, field: Field) -> usize {
        match field {
            Field::Provider => self.setup.catalogue.len(),
            Field::Connect => 2,
            Field::Auth => self.entry().map_or(0, |p| p.auths.len()),
            Field::Url | Field::Key | Field::Name => 0,
        }
    }

    fn choose(&mut self, field: Field, index: usize) {
        if index >= self.choice_count(field) {
            return;
        }
        match field {
            Field::Provider => {
                self.provider = index;
                self.auth = self.official_auth();
            }
            Field::Connect => {
                self.gateway = index == 1;
                if !self.gateway {
                    self.auth = self.official_auth();
                }
            }
            Field::Auth => self.auth = index,
            Field::Url | Field::Key | Field::Name => {}
        }
        self.suggest();
    }

    fn chosen(&self, field: Field) -> usize {
        match field {
            Field::Provider => self.provider,
            Field::Connect => usize::from(self.gateway),
            Field::Auth => self.auth,
            Field::Url | Field::Key | Field::Name => 0,
        }
    }

    fn cycle(&mut self, field: Field, down: bool) {
        let n = self.choice_count(field);
        if n == 0 {
            return;
        }
        let at = self.chosen(field);
        self.choose(field, ListCursor::WRAP.step(at, n, down));
    }

    fn type_text(&mut self, text: &str) {
        match self.focus {
            Field::Url => {
                URL.paste(&mut self.url, text);
                self.suggest();
            }
            Field::Name => {
                if LineEdit::ENDPOINT_NAME.paste(&mut self.name, text) {
                    self.name_edited = true;
                }
            }
            Field::Provider | Field::Connect | Field::Auth | Field::Key => {}
        }
    }

    fn erase(&mut self, all: bool) {
        match self.focus {
            Field::Url => {
                LineEdit::erase(&mut self.url, all);
                self.suggest();
            }
            Field::Name => {
                LineEdit::erase(&mut self.name, all);
                self.name_edited = true;
            }
            Field::Provider | Field::Connect | Field::Auth | Field::Key => {}
        }
    }

    fn problem(&mut self, field: Field) -> Option<Result<(), String>> {
        match field {
            Field::Url if self.gateway => {
                let url = self.url.trim().to_owned();
                let Some(host) = host_of(&url) else {
                    return Some(Err(words::NOT_HTTP.to_owned()));
                };
                if http_scheme(&url)
                    && !loopback_host(&host)
                    && self.url_confirmed.as_deref() != Some(url.as_str())
                {
                    self.ask(AddQuestion::PlainHttp, words::plain_http(&host));
                    return Some(Ok(()));
                }
                None
            }
            Field::Key => match check_key(self.key.value().expose()) {
                Ok(_) => None,
                Err(problem) => Some(Err(format!("{problem} {}", words::PASTE_AGAIN))),
            },
            Field::Name => {
                if self.name == wisp_protocol::KEYS_FILE_ALIAS {
                    Some(Err(wisp_protocol::account_name_reserved()))
                } else if !valid_endpoint_name(&self.name) {
                    Some(Err(words::bad_name()))
                } else if self.taken.contains(&self.name) {
                    Some(Err(words::taken_name(&self.name)))
                } else {
                    None
                }
            }
            Field::Provider | Field::Connect | Field::Auth | Field::Url => None,
        }
    }

    fn stop_at(&mut self, field: Field, problem: Result<(), String>) {
        self.focus = field;
        if let Err(text) = problem {
            self.status = vec![(text, StatusTone::Error)];
        }
    }

    fn advance(&mut self) -> Step {
        let fields = self.fields();
        let last = fields.last().copied() == Some(self.focus);
        if last {
            return self.submit();
        }
        if let Some(problem) = self.problem(self.focus) {
            let field = self.focus;
            self.stop_at(field, problem);
            return Step::Stay;
        }
        self.status.clear();
        self.move_focus(true);
        Step::Stay
    }

    fn request(&self, check: bool) -> Option<SaveRequest> {
        let entry = self.entry()?;
        let auth = if self.gateway {
            entry.auths.get(self.auth)?.name.clone()
        } else {
            entry.official_auth.clone()
        };
        let key = check_key(self.key.value().expose()).ok()?;
        Some(SaveRequest {
            endpoint: EndpointSpec {
                name: self.name.clone(),
                provider: entry.name.clone(),
                auth,
                url: self
                    .gateway
                    .then(|| Secret::new(self.url.trim().to_owned())),
                store: StoreChoice::Auto,
                default: self.mode == CardMode::FirstRun,
            },
            key: Secret::new(key),
            check,
        })
    }

    fn edit_request(&self) -> Option<EditRequest> {
        let mut edit = EndpointEdit::default();
        if self.gateway {
            edit.url = Some(UrlEdit::Set(Secret::new(self.url.trim().to_owned())));
        } else {
            edit.key = Some(Secret::new(check_key(self.key.value().expose()).ok()?));
        }
        Some(EditRequest {
            name: self.target.clone(),
            edit,
            store: None,
            check: true,
        })
    }

    fn submit(&mut self) -> Step {
        for field in self.fields() {
            if let Some(problem) = self.problem(field) {
                self.stop_at(field, problem);
                return Step::Stay;
            }
        }
        let request = if self.mode == CardMode::Edit {
            self.edit_request().map(Request::Edit)
        } else {
            self.request(true).map(Request::Save)
        };
        match request {
            Some(request) => self.send(request),
            None => Step::Stay,
        }
    }

    fn send(&mut self, request: Request) -> Step {
        self.status.clear();
        self.last = Some(request.clone());
        self.phase = Phase::Saving(None);
        request.step()
    }

    fn resend(&mut self, change: impl FnOnce(&mut Request)) -> Step {
        let Some(mut request) = self.last.clone() else {
            return self.back(None);
        };
        change(&mut request);
        self.send(request)
    }

    fn back(&mut self, focus: Option<Field>) -> Step {
        self.phase = Phase::Editing;
        if let Some(focus) = focus {
            self.focus = focus;
        }
        Step::Stay
    }

    fn not_saved(&mut self) -> Step {
        self.status = vec![(words::NOT_SAVED.to_owned(), StatusTone::Muted)];
        self.back(None)
    }

    fn ask(&mut self, ask: AddQuestion, question: String) {
        let (answers, opening) = ask.answers(self.gateway);
        self.status.clear();
        self.phase = Phase::Asking(Asking {
            ask,
            question,
            answers,
            pick: Pick {
                rows: answers.len(),
                focus: Some(opening),
                ..Pick::default()
            },
        });
    }

    fn picked(&mut self, ask: AddQuestion, answer: AddAnswer) -> Step {
        use AddAnswer as A;
        use AddQuestion as Q;
        match (ask, answer) {
            (Q::KeyRefused, A::EnterKeyAgain) => {
                self.key.clear();
                self.back(Some(Field::Key))
            }
            (Q::KeyRefused | Q::CheckFailed, A::SaveAnyway)
            | (Q::Forbidden | Q::NotOffered, A::Yes) => self.resend(|r| r.set_check(false)),
            (Q::CheckFailed, A::EnterUrlAgain) => self.back(Some(Field::Url)),
            (Q::CheckFailed, A::TryAgain) => self.resend(|r| r.set_check(true)),
            (Q::KeychainFile, A::Yes) => self.resend(Request::keep_in_file),
            (Q::PlainHttp, A::Yes) => {
                self.url_confirmed = Some(self.url.trim().to_owned());
                self.phase = Phase::Editing;
                self.advance()
            }
            (Q::PlainHttp, A::No) => self.back(Some(Field::Url)),
            _ => self.not_saved(),
        }
    }

    fn leave(&mut self) -> Step {
        match self.mode {
            CardMode::FirstRun => {
                if self.saving() {
                    self.quitting = true;
                }
                Step::Quit
            }
            CardMode::Add | CardMode::Edit => Step::Close,
        }
    }

    pub fn key(&mut self, key: Key) -> Step {
        let ctrl = key.ctrl && !key.alt;
        if matches!(key.code, KeyCode::Esc)
            && let Phase::Asking(asking) = &self.phase
            && let Some(&last) = asking.answers.last()
        {
            let ask = asking.ask;
            return self.picked(ask, last);
        }
        if matches!(key.code, KeyCode::Esc)
            || (ctrl && matches!(key.code, KeyCode::Char('c' | 'C' | 'd' | 'D')))
        {
            return self.leave();
        }
        match &mut self.phase {
            Phase::Saving(_) | Phase::Saved => Step::Stay,
            Phase::Asking(asking) => {
                let answer = asking.pick.key(key).took();
                match answer.and_then(|row| asking.answers.get(row).copied()) {
                    Some(answer) => {
                        let ask = asking.ask;
                        self.picked(ask, answer)
                    }
                    None => Step::Stay,
                }
            }
            Phase::Editing => self.edit(key, ctrl),
        }
    }

    fn edit(&mut self, key: Key, ctrl: bool) -> Step {
        if self.focus == Field::Key {
            let edit = if key.code == KeyCode::Backspace {
                Key::plain(KeyCode::Backspace)
            } else {
                key
            };
            match self.key.key(edit) {
                Ok(false) => {}
                _ => return Step::Stay,
            }
        }
        let choices = self.choice_count(self.focus) > 0;
        match key.code {
            KeyCode::Tab => {
                self.move_focus(true);
                Step::Stay
            }
            KeyCode::BackTab => {
                self.move_focus(false);
                Step::Stay
            }
            KeyCode::Up | KeyCode::Left if choices => {
                self.cycle(self.focus, false);
                Step::Stay
            }
            KeyCode::Down | KeyCode::Right if choices => {
                self.cycle(self.focus, true);
                Step::Stay
            }
            KeyCode::Up => {
                self.move_focus(false);
                Step::Stay
            }
            KeyCode::Down => {
                self.move_focus(true);
                Step::Stay
            }
            KeyCode::Enter => self.advance(),
            KeyCode::Char(c @ '1'..='9') if choices && !ctrl => {
                self.choose(self.focus, c as usize - '1' as usize);
                Step::Stay
            }
            KeyCode::Char('u' | 'U') if ctrl => {
                self.erase(true);
                Step::Stay
            }
            KeyCode::Backspace => {
                self.erase(false);
                Step::Stay
            }
            KeyCode::Char(c) if !ctrl && !key.alt => {
                let mut buf = [0u8; 4];
                self.type_text(c.encode_utf8(&mut buf));
                Step::Stay
            }
            _ => Step::Stay,
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.phase != Phase::Editing {
            return;
        }
        match self.focus {
            Field::Key => {
                self.status.clear();
                if let Err(problem) = self.key.paste(text) {
                    self.status = vec![(
                        format!("{problem} {}", words::PASTE_AGAIN),
                        StatusTone::Error,
                    )];
                }
            }
            Field::Url | Field::Name => {
                let line: String = text.lines().collect::<Vec<_>>().join("");
                self.type_text(line.trim());
            }
            Field::Provider | Field::Connect | Field::Auth => {}
        }
    }

    pub fn sent(&mut self, request: RequestId) {
        if let Phase::Saving(pending) = &mut self.phase {
            *pending = Some(request);
        }
    }

    pub fn unsent(&mut self, reason: &str) {
        self.status = vec![(reason.to_owned(), StatusTone::Error)];
        self.phase = Phase::Editing;
    }

    pub fn answer(&mut self, event: &AgentEvent, file_asked: &mut bool, set: GlyphSet) -> Answered {
        let Some(pending) = self.pending() else {
            return Answered::Not;
        };
        match event {
            AgentEvent::EndpointSaved {
                answers,
                name,
                store,
                check,
                notes,
            } if *answers == pending => {
                let check_words = match check.outcome {
                    CheckKind::Ok => Some(words::checked_ok(set)),
                    CheckKind::Skipped => Some(words::NOT_CHECKED.to_owned()),
                    _ => None,
                };
                let place = store_words(store, &self.setup.credentials_shown);
                let mut status = vec![(
                    words::saved_line(check_words.as_deref(), &place, set),
                    StatusTone::Ok,
                )];
                status.extend(notes.iter().map(|n| (sanitize(n), StatusTone::Muted)));
                self.status = status;
                self.phase = Phase::Saved;
                Answered::Saved {
                    name: name.clone(),
                    store: store.clone(),
                    notes: notes.clone(),
                }
            }
            AgentEvent::EndpointEdited {
                answers,
                name,
                changed,
                check,
                notes,
                ..
            } if *answers == pending => {
                let check_words = match check.outcome {
                    CheckKind::Ok => Some(words::checked_ok(set)),
                    CheckKind::Skipped => Some(words::NOT_CHECKED.to_owned()),
                    _ => None,
                };
                let fields: Vec<String> = changed.iter().map(words::field_words).collect();
                let mut status = vec![(
                    match check_words {
                        Some(check) => {
                            format!(
                                "{check}. {}",
                                words::edited_note(&sanitize(name), &fields, self.current)
                            )
                        }
                        None => words::edited_note(&sanitize(name), &fields, self.current),
                    },
                    StatusTone::Ok,
                )];
                status.extend(notes.iter().map(|n| (sanitize(n), StatusTone::Muted)));
                self.status = status;
                self.phase = Phase::Saved;
                Answered::Edited {
                    name: name.clone(),
                    changed: changed.clone(),
                    notes: notes.clone(),
                }
            }
            AgentEvent::EndpointNotSaved { answers, why, .. } if *answers == pending => {
                self.quitting = false;
                match why {
                    NotSavedReason::Check(result) => self.checked(result),
                    NotSavedReason::KeychainFailed(which) => {
                        let which = sanitize(which);
                        if *file_asked {
                            self.status =
                                vec![(words::keychain_stopped(&which), StatusTone::Error)];
                            self.phase = Phase::Editing;
                        } else {
                            *file_asked = true;
                            let file = self.setup.credentials_shown.clone();
                            self.ask(
                                AddQuestion::KeychainFile,
                                words::keychain_question(&which, &file, false),
                            );
                        }
                    }
                    NotSavedReason::Other(_) => {
                        self.not_saved();
                    }
                }
                Answered::Shown
            }
            AgentEvent::Rejected {
                request: Some(request),
                reason,
                ..
            } if *request == pending => {
                self.quitting = false;
                self.status = vec![(sanitize(reason), StatusTone::Error)];
                self.phase = Phase::Editing;
                Answered::Shown
            }
            _ => Answered::Not,
        }
    }

    fn checked(&mut self, result: &CheckResult) {
        match &result.outcome {
            CheckKind::KeyRefused => {
                self.ask(AddQuestion::KeyRefused, words::KEY_REFUSED.to_owned())
            }
            CheckKind::Forbidden => self.ask(AddQuestion::Forbidden, words::forbidden_ask()),
            CheckKind::NotOffered => self.ask(AddQuestion::NotOffered, words::not_offered_ask()),
            CheckKind::Unexpected | CheckKind::Unreachable => {
                let reason = result
                    .reason
                    .as_deref()
                    .map_or_else(|| words::NOT_SAVED.to_owned(), sanitize);
                self.ask(
                    AddQuestion::CheckFailed,
                    words::check_failed(reason.trim_end_matches('.')),
                );
            }
            CheckKind::Ok | CheckKind::Skipped | CheckKind::Other(_) => {
                self.not_saved();
            }
        }
    }

    fn field_row(&self, field: Field) -> FieldRow {
        let focused = self.focus == field;
        let entry = self.entry();
        let (label, value) = match field {
            Field::Provider => (
                words::PROVIDER_LABEL,
                if self.setup.catalogue.len() > 1 {
                    FieldValue::Choices {
                        options: self
                            .setup
                            .catalogue
                            .iter()
                            .map(|p| vec![p.title.clone()])
                            .collect(),
                        chosen: self.provider,
                    }
                } else {
                    FieldValue::Fixed(entry.map_or_else(String::new, |p| p.title.clone()))
                },
            ),
            Field::Connect => {
                let (title, url) = entry.map_or(("", ""), |p| (p.title.as_str(), &p.default_url));
                let host = host_of(url).unwrap_or_default();
                (
                    words::CONNECT_LABEL,
                    FieldValue::Choices {
                        options: vec![
                            words::official_choice(title, &host),
                            vec![words::GATEWAY_CHOICE.to_owned()],
                        ],
                        chosen: usize::from(self.gateway),
                    },
                )
            }
            Field::Url => (
                words::URL_LABEL,
                FieldValue::Text {
                    text: self.url.clone(),
                    placeholder: words::URL_PLACEHOLDER.to_owned(),
                },
            ),
            Field::Auth => (
                words::AUTH_LABEL,
                FieldValue::Choices {
                    options: entry.map_or_else(Vec::new, |p| {
                        p.auths.iter().map(|a| vec![a.words.clone()]).collect()
                    }),
                    chosen: self.auth,
                },
            ),
            Field::Key => (
                words::KEY_LABEL,
                FieldValue::Secret(self.key.value().clone(), self.key.ends()),
            ),
            Field::Name => (
                words::NAME_LABEL,
                FieldValue::Text {
                    text: self.name.clone(),
                    placeholder: String::new(),
                },
            ),
        };
        FieldRow {
            label: label.to_owned(),
            value,
            focused,
        }
    }

    fn hints(&self) -> Vec<KeyHint> {
        let leave = copy::esc(match self.mode {
            CardMode::FirstRun => copy::QUIT_WORD,
            CardMode::Add | CardMode::Edit => copy::CLOSE_WORD,
        });
        match &self.phase {
            Phase::Editing => {
                let last = self.fields().last().copied() == Some(self.focus);
                let enter = if last {
                    copy::SAVE_WORD
                } else {
                    copy::NEXT_WORD
                };
                vec![
                    KeyHint::new(copy::ENTER_KEY, enter),
                    KeyHint::new(copy::TAB_KEY, copy::MOVE_WORD),
                    leave,
                ]
            }
            Phase::Asking(_) => vec![copy::enter_choose(), copy::esc(copy::BACK_WORD)],
            Phase::Saving(_) if !self.quitting => vec![leave],
            Phase::Saving(_) | Phase::Saved => vec![],
        }
    }

    pub fn view(&self, set: GlyphSet) -> EndpointCardView {
        let mut fields = if self.mode == CardMode::Edit {
            vec![]
        } else {
            vec![Field::Provider]
        };
        fields.extend(self.fields().into_iter().filter(|f| *f != Field::Provider));
        let names: Vec<String> = self
            .setup
            .presence
            .iter()
            .map(|n| n.shown().to_owned())
            .collect();
        let mut status = self.status.clone();
        if self.saving() {
            status.push((words::saving_words(set), StatusTone::Muted));
            if self.quitting {
                status.push((words::QUIT_AFTER_SAVE.to_owned(), StatusTone::Muted));
            }
        }
        EndpointCardView {
            banner: self.mode == CardMode::FirstRun,
            title: match self.mode {
                CardMode::FirstRun => words::SETUP_TITLE.to_owned(),
                CardMode::Add => words::ADD_TITLE.to_owned(),
                CardMode::Edit => words::edit_title(&sanitize(&self.target), self.gateway),
            },
            note: if self.mode == CardMode::Edit {
                None
            } else {
                words::import_note(&names)
            },
            fields: fields
                .into_iter()
                .map(|field| self.field_row(field))
                .collect(),
            status,
            menu: match &self.phase {
                Phase::Asking(asking) => Some(MenuView {
                    question: asking.question.clone(),
                    options: asking
                        .answers
                        .iter()
                        .map(|a| a.label().to_owned())
                        .collect(),
                    selected: asking.pick.focus.unwrap_or_default(),
                }),
                _ => None,
            },
            hints: self.hints(),
            active: self.phase == Phase::Editing,
        }
    }
}

#[cfg(test)]
mod tests;
