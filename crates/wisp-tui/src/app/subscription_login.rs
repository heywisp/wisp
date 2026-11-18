use crate::app::list::{Pick, Picked};
use crate::term::clipboard::{Outcome, link_words};
use crate::term::{Key, KeyCode};
use crate::widgets::line_edit::LineEdit;
use crate::widgets::secret::{Rule, SecretField};
use std::time::Duration;
use wisp_protocol::endpoint::StoreKind;
use wisp_protocol::subscription_auth::{AuthFailureKind, ChallengeKind, SubscriptionProvider};
use wisp_protocol::{
    ACCOUNT_NAME_LABEL, ACCOUNT_NAME_RULE, CodeProblem, KeyProblem, QuestionOption, RequestId,
    Secret, account_name, account_name_problem, account_name_taken,
};
#[cfg(test)]
use wisp_render::chrome::account::SaidWords;
use wisp_render::chrome::account::{
    AccountKind, CLEANUP_WARNING, LINK_NOT_COPIED, LINK_WORDS, Mistake, NO_BROWSER, SameAccount,
    Surface, TRIED_BROWSER, browser_words, login_again, mark_line, marked, mistake_line,
    mistake_of, switch_first,
};
use wisp_render::chrome::{
    CardHit, CardRows, Credential, EndpointFacts, FormAccount, FormField, FormValue, FormView,
    KeyHint, LinkRow, form_rows,
};
use wisp_render::markdown::Layout;
use wisp_render::styled::StyledLine;
use wisp_render::text::{WidthMode, width};
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::copy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub alias: String,
    pub provider: Option<SubscriptionProvider>,
    pub working: bool,
    pub mark: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Login {
        provider: Option<SubscriptionProvider>,
        alias: Option<String>,
        existing_alias: bool,
    },
    Status,
}

const STATUS: &str = "status";

pub fn route(words: &str, endpoints: &[Endpoint]) -> Result<Route, String> {
    let words: Vec<_> = words.split_whitespace().collect();
    match words.as_slice() {
        [] => Ok(Route::Login {
            provider: None,
            alias: None,
            existing_alias: false,
        }),
        [STATUS] => Ok(Route::Status),
        [STATUS, alias] if account_name(alias).is_some() => Ok(Route::Status),
        [word] => match SubscriptionProvider::from_name(word) {
            Some(provider) => Ok(Route::Login {
                provider: Some(provider),
                alias: None,
                existing_alias: false,
            }),
            None => by_name(word, endpoints),
        },
        ["--name", name] => by_name(name, endpoints),
        [provider, alias] if account_name(alias).is_some() => {
            SubscriptionProvider::from_name(provider)
                .map(|provider| Route::Login {
                    provider: Some(provider),
                    alias: account_name(alias),
                    existing_alias: false,
                })
                .ok_or_else(|| format!("Choose {}.", SubscriptionProvider::names_or()))
        }
        _ => Err(format!(
            "Use /login [{}] [NAME], or /login --name NAME.",
            SubscriptionProvider::names_pipe()
        )),
    }
}

pub fn by_name(word: &str, endpoints: &[Endpoint]) -> Result<Route, String> {
    let Some(word) = account_name(word) else {
        return Err(account_name_problem(word));
    };
    let existing = endpoints.iter().find(|endpoint| endpoint.alias == word);
    if existing.is_some_and(|endpoint| endpoint.provider.is_none()) {
        return Err("That account is not a subscription; choose a different account name.".into());
    }
    Ok(Route::Login {
        provider: existing.and_then(|endpoint| endpoint.provider),
        existing_alias: existing.is_some(),
        alias: Some(word),
    })
}

pub fn endpoints(facts: &[EndpointFacts]) -> Vec<Endpoint> {
    facts
        .iter()
        .map(|endpoint| Endpoint {
            alias: endpoint.name.clone(),
            provider: endpoint.credential.and_then(Credential::plan),
            working: !endpoint.expired && endpoint.available(),
            mark: endpoint.mark,
        })
        .collect()
}

pub fn reaches(name: &str) -> bool {
    matches!(route(name, &[]), Ok(Route::Login { alias: Some(alias), .. }) if alias == name)
}

pub fn login_hint(alias: &str) -> String {
    login_again(Surface::App, alias, reaches(alias))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub provider: SubscriptionProvider,
    pub alias: String,
    pub reauthorize: bool,
}

#[derive(Debug)]
pub enum Intent {
    Start {
        login: RequestId,
        selection: Selection,
    },
    Finish {
        login: RequestId,
        response: Option<Secret>,
    },
    Cancel {
        login: RequestId,
    },
    Reload {
        alias: String,
    },
    Open {
        login: RequestId,
    },
    AddKey,
    Use {
        alias: String,
    },
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Provider,
    Account,
    Name,
    Starting,
    ManualCode,
    Finishing,
    SavedNotReady,
    Reloading,
    Done,
    Cancelled,
    Ended,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Provider(SubscriptionProvider),
    Key,
    Again(usize),
    Another,
    Use,
    NotNow,
}

pub struct Wizard {
    phase: Phase,
    endpoints: Vec<Endpoint>,
    provider: Option<SubscriptionProvider>,
    kind: Option<ChallengeKind>,
    alias: String,
    explicit_alias: bool,
    reauthorize: bool,
    pick: Pick,
    login: RequestId,
    pending: RequestId,
    url: Option<Secret>,
    link_text: Option<String>,
    opened: bool,
    copy_due: bool,
    copy_asked: bool,
    copied: Option<&'static str>,
    way_left: bool,
    code: SecretField,
    deadline: Option<Duration>,
    message: String,
    offer: bool,
    pub note: String,
    previous_cleanup_pending: bool,
    refused: Option<SameAccount>,
    first: bool,
}

fn way_left(provider: SubscriptionProvider) -> String {
    format!("quit, then run wisp login {}", provider.default_name())
}

fn link_text(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let shown = rest[..end].trim_end_matches('/');
    (!shown.is_empty() && shown.chars().all(|c| c.is_ascii_graphic())).then(|| shown.to_owned())
}

fn slot(provider: SubscriptionProvider, glyphs: &Glyphs, mode: WidthMode) -> usize {
    let way = way_left(provider);
    let sep = width(glyphs.separator, mode) + 2;
    let widest = |first: &str, seconds: &[&str]| {
        seconds
            .iter()
            .map(|second| width(first, mode) + sep + width(second, mode))
            .max()
            .unwrap_or(0)
    };
    let mut lonely = LINK_WORDS.to_vec();
    lonely.push(&way);
    widest(TRIED_BROWSER, &LINK_WORDS).max(widest(NO_BROWSER, &lonely))
}

impl std::fmt::Debug for Wizard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginWizard")
            .field("phase", &self.phase)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl Wizard {
    pub fn new(
        route: Route,
        endpoints: Vec<Endpoint>,
        login: RequestId,
    ) -> Result<(Self, Option<Intent>), &'static str> {
        let Route::Login {
            provider,
            alias,
            existing_alias,
        } = route
        else {
            return Err("Status is not a sign-in wizard.");
        };
        let explicit_alias = alias.is_some();
        let mut wizard = Self {
            phase: Phase::Provider,
            endpoints,
            provider,
            kind: None,
            alias: alias.unwrap_or_default(),
            explicit_alias,
            reauthorize: existing_alias,
            pick: Pick {
                focus: Some(0),
                ..Pick::default()
            },
            login,
            pending: login,
            url: None,
            link_text: None,
            opened: false,
            copy_due: false,
            copy_asked: false,
            copied: None,
            way_left: false,
            code: SecretField::new(Rule::Code),
            deadline: None,
            message: String::new(),
            offer: false,
            note: String::new(),
            previous_cleanup_pending: false,
            refused: None,
            first: false,
        };
        let intent = match provider {
            Some(_) if explicit_alias => wizard.selected()?,
            Some(provider) => {
                wizard.picked(provider);
                None
            }
            None => None,
        };
        Ok((wizard, intent))
    }
    pub fn first_run(&mut self, lead: &str) {
        self.first = true;
        lead.clone_into(&mut self.note);
    }
    pub fn choosing(&self) -> bool {
        self.phase == Phase::Provider
    }
    pub fn finishing(&self) -> bool {
        self.phase == Phase::Finishing
    }
    pub fn reauthorizing(&self) -> bool {
        self.reauthorize
    }
    pub fn refused(&self) -> Option<&SameAccount> {
        self.refused.as_ref()
    }
    pub fn waits_on(&mut self, request: RequestId) {
        self.pending = request;
    }
    pub fn start_sent(&mut self, request: RequestId) {
        if self.phase == Phase::Starting {
            self.login = request;
            self.pending = request;
        }
    }
    pub fn deadline(&self) -> Option<Duration> {
        self.deadline
    }
    pub fn live_link(&self) -> Option<&Secret> {
        self.url.as_ref().filter(|_| self.link_text.is_some())
    }
    pub fn link(&mut self) -> Option<Intent> {
        if self.ended() {
            return self.restart();
        }
        self.url.as_ref()?;
        if self.opened {
            return Some(Intent::Open { login: self.login });
        }
        self.ask_copy();
        None
    }
    fn ask_copy(&mut self) {
        if self.url.is_some() {
            self.copy_due = true;
            self.copy_asked = true;
        }
    }
    fn ended(&self) -> bool {
        self.phase == Phase::Ended || (self.phase == Phase::Starting && self.link_text.is_some())
    }
    fn restart(&mut self) -> Option<Intent> {
        if self.phase != Phase::Ended {
            return None;
        }
        self.selected().ok().flatten()
    }
    pub fn reopened(&mut self, login: RequestId, opened: bool) {
        if login == self.login && self.url.is_some() {
            self.opened = opened;
        }
    }
    pub fn open_failed(&mut self, login: RequestId) {
        self.reopened(login, false);
    }
    pub fn take_copy(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.copy_due) {
            return None;
        }
        self.url.as_ref().map(|url| url.expose().to_owned())
    }
    pub fn copied(&mut self, outcome: &Outcome) {
        if self.url.is_none() {
            return;
        }
        let words = link_words(outcome);
        self.way_left =
            std::mem::take(&mut self.copy_asked) && !self.opened && words == LINK_NOT_COPIED;
        self.copied = Some(words);
    }
    fn choices(&self) -> Vec<Choice> {
        match self.phase {
            Phase::Provider => SubscriptionProvider::ALL
                .into_iter()
                .map(Choice::Provider)
                .chain([Choice::Key])
                .collect(),
            Phase::Account => {
                let mut out: Vec<Choice> = self
                    .endpoints
                    .iter()
                    .enumerate()
                    .filter(|(_, endpoint)| {
                        endpoint.provider.is_some() && endpoint.provider == self.provider
                    })
                    .map(|(index, _)| Choice::Again(index))
                    .collect();
                out.sort_by_key(|choice| {
                    matches!(choice, Choice::Again(index) if self.endpoints.get(*index).is_some_and(|e| e.working))
                });
                out.push(Choice::Another);
                out
            }
            Phase::Done if self.offer => vec![Choice::Use, Choice::NotNow],
            _ => vec![],
        }
    }
    fn picked(&mut self, provider: SubscriptionProvider) {
        self.provider = Some(provider);
        self.kind = None;
        self.pick.focus = Some(0);
        if self
            .endpoints
            .iter()
            .any(|endpoint| endpoint.provider == Some(provider))
        {
            self.phase = Phase::Account;
        } else {
            self.name_step(provider);
        }
    }
    fn name_step(&mut self, provider: SubscriptionProvider) {
        let suggested = provider.default_name();
        self.phase = Phase::Name;
        self.pick.focus = Some(0);
        if self.taken(suggested) {
            self.alias.clear();
            self.message = account_name_taken(suggested);
        } else {
            self.alias = suggested.to_owned();
            self.message.clear();
        }
    }
    fn taken(&self, alias: &str) -> bool {
        self.endpoints
            .iter()
            .any(|endpoint| endpoint.alias == alias)
    }
    fn named(&mut self) -> Result<Option<Intent>, &'static str> {
        match account_name(&self.alias) {
            None => {
                self.message = account_name_problem(&self.alias);
                Ok(None)
            }
            Some(name) if self.taken(&name) => {
                self.message = account_name_taken(&name);
                Ok(None)
            }
            Some(name) => {
                self.alias = name;
                self.selected()
            }
        }
    }
    fn wipe(&mut self) {
        self.code.clear();
        self.url = None;
        self.copy_due = false;
        self.copy_asked = false;
        self.copied = None;
        self.way_left = false;
        self.deadline = None;
    }
    fn selection(&self) -> Option<Selection> {
        Some(Selection {
            provider: self.provider?,
            alias: self.alias.clone(),
            reauthorize: self.reauthorize,
        })
    }
    fn selected(&mut self) -> Result<Option<Intent>, &'static str> {
        if account_name(&self.alias).as_deref() != Some(self.alias.as_str()) {
            return Err(&ACCOUNT_NAME_RULE);
        }
        let Some(provider) = self.provider else {
            return Err("Choose a provider first.");
        };
        if let Some(existing) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.alias == self.alias)
        {
            if existing.provider != Some(provider) {
                return Err(
                    "That account name belongs to another provider or authentication mode; choose a different name.",
                );
            }
            self.reauthorize = true;
        }
        self.phase = Phase::Starting;
        Ok(self.selection().map(|selection| Intent::Start {
            login: self.login,
            selection,
        }))
    }
    fn chose(&mut self) -> Result<Option<Intent>, &'static str> {
        let row = self.pick.focus.unwrap_or_default();
        let Some(choice) = self.choices().get(row).copied() else {
            return Ok(None);
        };
        match choice {
            Choice::Provider(provider) if self.explicit_alias || self.first => {
                if self.first {
                    provider.default_name().clone_into(&mut self.alias);
                }
                self.provider = Some(provider);
                self.selected()
            }
            Choice::Provider(provider) => {
                self.picked(provider);
                Ok(None)
            }
            Choice::Key => Ok(Some(Intent::AddKey)),
            Choice::Again(index) => {
                if let Some(endpoint) = self.endpoints.get(index) {
                    self.alias.clone_from(&endpoint.alias);
                }
                self.selected()
            }
            Choice::Another => {
                if let Some(provider) = self.provider {
                    self.name_step(provider);
                }
                Ok(None)
            }
            Choice::Use => Ok(Some(Intent::Use {
                alias: self.alias.clone(),
            })),
            Choice::NotNow => Ok(Some(Intent::Close)),
        }
    }
    fn submit(&mut self) -> Option<Intent> {
        let result = match self.phase {
            Phase::Provider | Phase::Account => self.chose(),
            Phase::Done if self.offer => self.chose(),
            Phase::Name => self.named(),
            Phase::ManualCode if self.code.filled() => {
                self.phase = Phase::Finishing;
                Ok(Some(Intent::Finish {
                    login: self.login,
                    response: Some(self.code.value().clone()),
                }))
            }
            Phase::ManualCode => {
                self.message = self.mistake(Mistake::Pasted(CodeProblem::Empty));
                Ok(None)
            }
            Phase::Finishing if self.kind == Some(ChallengeKind::BrowserLoopback) => {
                Ok(self.link())
            }
            Phase::Ended => Ok(self.restart()),
            Phase::SavedNotReady => {
                self.phase = Phase::Reloading;
                Ok(Some(Intent::Reload {
                    alias: self.alias.clone(),
                }))
            }
            Phase::Done | Phase::Failed | Phase::Cancelled => Ok(Some(Intent::Close)),
            _ => Ok(None),
        };
        match result {
            Ok(intent) => intent,
            Err(message) => {
                self.message = message.into();
                None
            }
        }
    }
    fn mistake(&self, mistake: Mistake) -> String {
        mistake_line(
            mistake,
            self.provider.map_or("", SubscriptionProvider::title),
        )
    }
    fn browser_step(&self) -> bool {
        matches!(self.phase, Phase::ManualCode | Phase::Finishing) || self.ended()
    }
    pub fn key(&mut self, key: Key, now: Duration) -> Option<Intent> {
        if let Some(intent) = self.tick(now) {
            return Some(intent);
        }
        if key.code == KeyCode::Esc || (key.ctrl && matches!(key.code, KeyCode::Char('c' | 'C'))) {
            return Some(self.cancel());
        }
        self.pick.rows = self.choices().len();
        match self.pick.key(key) {
            Picked::Took(_) => return self.submit(),
            Picked::Moved => return None,
            Picked::Not => {}
        }
        if key.code == KeyCode::Enter && !key.alt && !key.ctrl && !key.shift {
            return self.submit();
        }
        if self.browser_step() && !key.alt {
            let loopback = self.kind == Some(ChallengeKind::BrowserLoopback);
            match key.code {
                KeyCode::Char('o' | 'O') if key.ctrl && (self.opened || self.ended()) => {
                    return self.link();
                }
                KeyCode::Char('l' | 'L') if key.ctrl => {
                    self.ask_copy();
                    return None;
                }
                KeyCode::Char('c') if loopback && !key.ctrl => {
                    self.ask_copy();
                    return None;
                }
                _ => {}
            }
        }
        match self.phase {
            Phase::Name if LineEdit::ENDPOINT_NAME.key(&mut self.alias, key) => {
                self.message.clear();
            }
            Phase::ManualCode if self.code.key(key) == Ok(true) => {
                self.message.clear();
            }
            _ => {}
        }
        None
    }
    pub fn click(&mut self, hit: CardHit, now: Duration) -> Option<Intent> {
        if let Some(intent) = self.tick(now) {
            return Some(intent);
        }
        match hit {
            CardHit::Option(index) if index < self.choices().len() => {
                self.pick.focus = Some(index);
                self.submit()
            }
            CardHit::Link => self.link(),
            _ => None,
        }
    }

    pub fn paste(&mut self, text: &str) {
        match self.phase {
            Phase::Name => match account_name(text) {
                Some(name) => {
                    self.alias = name;
                    self.message.clear();
                }
                None => self.message = account_name_problem(text),
            },
            Phase::ManualCode => {
                self.message = match self.code.paste(text) {
                    Ok(()) => String::new(),
                    Err(KeyProblem::Empty) => self.mistake(Mistake::Pasted(CodeProblem::Empty)),
                    Err(KeyProblem::NotPrintable | KeyProblem::ManyLines | KeyProblem::Control) => {
                        self.mistake(Mistake::Pasted(CodeProblem::Spaced))
                    }
                    Err(KeyProblem::TooLong) => self.mistake(Mistake::Pasted(CodeProblem::TooLong)),
                };
            }
            _ => {}
        }
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is one field of the challenge reply, which the wizard takes apart from the wire type"
    )]
    pub fn challenged(
        &mut self,
        request: RequestId,
        login: RequestId,
        kind: ChallengeKind,
        url: Secret,
        opened: bool,
        expires: Duration,
        now: Duration,
    ) -> Option<Intent> {
        if self.phase != Phase::Starting || request != self.pending || login != self.login {
            return None;
        }
        self.message.clear();
        self.kind = Some(kind);
        self.link_text = link_text(url.expose());
        self.url = Some(url);
        self.opened = opened;
        self.copy_due = true;
        self.copy_asked = false;
        self.copied = None;
        self.way_left = false;
        self.deadline = now.checked_add(expires);
        if self.deadline.is_none() || expires.is_zero() {
            return Some(self.time_out());
        }
        match kind {
            ChallengeKind::ManualCode => {
                self.phase = Phase::ManualCode;
                None
            }
            ChallengeKind::BrowserLoopback => {
                self.phase = Phase::Finishing;
                Some(Intent::Finish {
                    login: self.login,
                    response: None,
                })
            }
        }
    }
    fn time_out(&mut self) -> Intent {
        self.wipe();
        self.phase = Phase::Ended;
        self.message = self.mistake(Mistake::TimedOut);
        Intent::Cancel { login: self.login }
    }
    pub fn tick(&mut self, now: Duration) -> Option<Intent> {
        self.deadline
            .is_some_and(|deadline| now >= deadline)
            .then(|| self.time_out())
    }
    pub fn cancel(&mut self) -> Intent {
        if self.phase == Phase::Done {
            return Intent::Close;
        }
        let pending = matches!(
            self.phase,
            Phase::Starting | Phase::ManualCode | Phase::Finishing
        );
        self.wipe();
        self.phase = Phase::Cancelled;
        if pending {
            Intent::Cancel { login: self.login }
        } else {
            Intent::Close
        }
    }
    pub fn ready(&mut self, request: RequestId, cleanup_pending: bool) -> bool {
        if request != self.pending || !matches!(self.phase, Phase::Finishing | Phase::Reloading) {
            return false;
        }
        self.wipe();
        self.message.clear();
        self.phase = Phase::Done;
        self.pick.focus = Some(0);
        self.previous_cleanup_pending |= cleanup_pending;
        true
    }
    pub fn offer_use(&mut self, chat: &str) {
        if self.phase == Phase::Done {
            self.offer = true;
        }
        self.chat_on(chat);
    }
    pub fn chat_on(&mut self, chat: &str) {
        self.offer &= self.alias != chat;
    }
    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn mark(&self) -> Option<u8> {
        let own = self.endpoints.iter().find(|e| e.alias == self.alias);
        own.and_then(|endpoint| endpoint.mark)
    }

    pub fn signed_in_lines(&self, mark: Option<u8>, glyphs: &Glyphs) -> Vec<StyledLine> {
        let Some(provider) = self.provider.filter(|_| self.phase == Phase::Done) else {
            return vec![];
        };
        let signed_in = wisp_render::chrome::account::signed_in(
            self.reauthorize,
            AccountKind::from(provider),
            &self.alias,
            mark,
            glyphs,
        );
        let mut lines = vec![format!("{} {signed_in}", glyphs.done)];
        if self.previous_cleanup_pending {
            lines.push(CLEANUP_WARNING.to_owned());
        }
        let line = |line: String| mark_line(&line, Token::TextMuted, mark, glyphs);
        lines.into_iter().map(line).collect()
    }
    pub fn saved_not_ready(
        &mut self,
        request: RequestId,
        store: Option<StoreKind>,
        is_default: bool,
        cleanup_pending: bool,
    ) -> bool {
        if !self.ready(request, cleanup_pending) {
            return false;
        }
        self.phase = Phase::SavedNotReady;
        self.message = format!(
            "Saved: {} · {} · {} · {} · saved, not usable yet; Enter retries loading without signing in again{}",
            self.alias,
            self.provider
                .map_or("subscription", SubscriptionProvider::title),
            storage(store),
            if is_default {
                wisp_render::chrome::account::STARTS_HERE
            } else {
                "new chats start elsewhere"
            },
            if self.previous_cleanup_pending {
                format!(". {CLEANUP_WARNING}")
            } else {
                String::new()
            }
        );
        true
    }
    pub fn selection_matches(&self, alias: &str, provider: SubscriptionProvider) -> bool {
        self.alias == alias && self.provider == Some(provider)
    }

    pub fn failure(
        &mut self,
        request: RequestId,
        kind: AuthFailureKind,
        problem: Option<CodeProblem>,
        message: &str,
        same_account: Option<&str>,
    ) -> bool {
        if request != self.pending
            || !matches!(
                self.phase,
                Phase::Starting | Phase::ManualCode | Phase::Finishing | Phase::Reloading
            )
        {
            return false;
        }
        if self.phase == Phase::Reloading {
            self.phase = Phase::SavedNotReady;
            return true;
        }
        if let (Some(name), Some(provider)) = (same_account, self.provider) {
            self.wipe();
            self.phase = Phase::Failed;
            self.message.clear();
            let mark = self
                .endpoints
                .iter()
                .find(|e| e.alias == name)
                .and_then(|e| e.mark);
            self.refused = Some(SameAccount {
                provider,
                mark,
                name: name.to_owned(),
                surface: Surface::App,
                short: reaches(name),
            });
            return true;
        }
        let mistake = mistake_of(kind, problem);
        let manual = self.kind == Some(ChallengeKind::ManualCode);
        let answered = matches!(self.phase, Phase::ManualCode | Phase::Finishing);
        match mistake {
            Mistake::Pasted(_) | Mistake::Earlier | Mistake::Unreachable if manual && answered => {
                self.phase = Phase::ManualCode;
                self.message = self.mistake(mistake);
                return true;
            }
            Mistake::TimedOut if answered => {
                self.time_out();
                return true;
            }
            Mistake::Refused if manual && answered => {
                self.wipe();
                self.phase = Phase::Ended;
                self.message = self.mistake(mistake);
                return true;
            }
            _ => {}
        }
        self.wipe();
        self.phase = Phase::Failed;
        self.message = message.to_owned();
        true
    }

    pub fn release(&self) -> Option<Intent> {
        matches!(self.phase, Phase::Failed | Phase::Ended)
            .then_some(Intent::Cancel { login: self.login })
    }

    pub fn cancelled(&mut self, request: RequestId, login: RequestId) {
        if request == self.pending
            && login == self.login
            && matches!(
                self.phase,
                Phase::Starting | Phase::ManualCode | Phase::Finishing
            )
        {
            self.wipe();
            self.phase = Phase::Cancelled;
        }
    }

    fn choice_mark(&self, choice: Choice, mark: Option<u8>) -> Option<Option<u8>> {
        match choice {
            Choice::Again(index) => self.endpoints.get(index).map(|endpoint| endpoint.mark),
            Choice::Use => Some(mark),
            _ => None,
        }
    }

    fn choice_label(
        &self,
        choice: Choice,
        mark: Option<u8>,
        glyphs: &Glyphs,
    ) -> (String, &'static str) {
        match choice {
            Choice::Provider(provider) => (provider.title().into(), provider.subscription_words()),
            Choice::Key => (
                wisp_render::chrome::account::API_KEY.into(),
                SubscriptionProvider::API_KEY_WORDS,
            ),
            Choice::Again(index) => (
                wisp_render::chrome::account::sign_in_again_to(
                    &self
                        .endpoints
                        .get(index)
                        .map_or_else(String::new, |endpoint| {
                            marked(&endpoint.alias, endpoint.mark, glyphs)
                        }),
                ),
                "",
            ),
            Choice::Another => (
                format!(
                    "Add another {} account",
                    self.provider.map_or("", SubscriptionProvider::title)
                ),
                "",
            ),
            Choice::Use => (
                format!("Use {} for this chat", marked(&self.alias, mark, glyphs)),
                "",
            ),
            Choice::NotNow => ("Not now".into(), ""),
        }
    }

    fn options(&self, layout: Layout, mark: Option<u8>) -> (Vec<QuestionOption>, Vec<Option<u8>>) {
        let glyphs = Glyphs::of(layout.glyphs);
        let choices = self.choices();
        let rows = choices.iter().map(|&choice| {
            let (label, description) = self.choice_label(choice, mark, &glyphs);
            (label, vec![description.to_owned()])
        });
        let marks = choices
            .iter()
            .map(|&choice| self.choice_mark(choice, mark).flatten())
            .collect();
        (columns(rows.collect(), layout), marks)
    }

    fn other_account(&self) -> bool {
        self.endpoints.iter().any(|endpoint| {
            endpoint.provider.is_some()
                && endpoint.provider == self.provider
                && endpoint.alias != self.alias
        })
    }

    pub fn render(&self, room: usize, layout: Layout, mark: Option<u8>) -> CardRows {
        let glyphs = Glyphs::of(layout.glyphs);
        let (options, option_marks) = self.options(layout, mark);
        let account = self.provider.map(|provider| FormAccount {
            kind: AccountKind::from(provider),
            name: &self.alias,
            mark,
        });
        let title = self
            .provider
            .map(|provider| format!("Sign in to {}", provider.title()));
        let title = title.as_deref().unwrap_or("Sign in");
        if self.browser_step()
            && let Some(provider) = self.provider
        {
            return self.browser_card(provider, title, account, room, layout, &glyphs);
        }
        let blank = FormView {
            title,
            title_line: None,
            note: "",
            prompt: "",
            prompt_lines: &[],
            error: "",
            options: &options,
            option_marks: &option_marks,
            selected: self.pick.focus,
            field: None,
            footer: &[],
            account: None,
            done: false,
            link: None,
            refused: None,
        };
        let name_field;
        let (form, footer): (FormView<'_>, Vec<KeyHint>) = match self.phase {
            Phase::Provider if self.first => (
                FormView {
                    title: SIGN_IN_TO_START,
                    note: &self.note,
                    ..blank
                },
                vec![copy::enter_choose(), copy::esc(copy::QUIT_WORD)],
            ),
            Phase::Provider => (
                FormView {
                    title: "Sign in",
                    note: &self.note,
                    ..blank
                },
                choose_keys(),
            ),
            Phase::Account => (blank, choose_keys()),
            Phase::Name => {
                name_field = FormField {
                    label: ACCOUNT_NAME_LABEL,
                    value: FormValue::Text(&self.alias),
                };
                (
                    FormView {
                        title: "Sign in",
                        error: &self.message,
                        field: Some(name_field),
                        ..blank
                    },
                    vec![
                        KeyHint::new(copy::ENTER_KEY, "sign in"),
                        KeyHint::new("ctrl-u", copy::CLEAR_WORD),
                        cancel(),
                    ],
                )
            }
            Phase::Starting => (
                FormView {
                    title: "Sign in",
                    prompt: "Preparing sign-in…",
                    ..blank
                },
                vec![cancel()],
            ),
            Phase::Reloading => (
                FormView {
                    title: "Sign in",
                    prompt: "Loading the saved sign-in…",
                    ..blank
                },
                vec![copy::esc(copy::CLOSE_WORD)],
            ),
            Phase::Done => (
                FormView {
                    account,
                    done: true,
                    ..blank
                },
                if self.offer {
                    vec![copy::enter_choose(), copy::esc(copy::CLOSE_WORD)]
                } else {
                    vec![enter_close()]
                },
            ),
            Phase::Cancelled => (
                FormView {
                    title: "Sign in",
                    prompt: "Sign-in closed.",
                    ..blank
                },
                vec![enter_close()],
            ),
            Phase::SavedNotReady => (
                FormView {
                    title: "Sign in",
                    prompt: &self.message,
                    ..blank
                },
                vec![
                    KeyHint::new(copy::ENTER_KEY, "retry loading"),
                    copy::esc(copy::CLOSE_WORD),
                ],
            ),
            _ => (
                FormView {
                    prompt: &self.message,
                    refused: self.refused.as_ref(),
                    ..blank
                },
                vec![enter_close()],
            ),
        };
        form_rows(
            &FormView {
                footer: &footer,
                ..form
            },
            room,
            layout,
        )
    }

    fn browser_card(
        &self,
        provider: SubscriptionProvider,
        title: &str,
        account: Option<FormAccount<'_>>,
        room: usize,
        layout: Layout,
        glyphs: &Glyphs,
    ) -> CardRows {
        let ended = self.ended();
        let way = way_left(provider);
        let copied = match (self.url.is_some(), self.way_left, self.copied) {
            (false, _, _) | (true, false, None) => "",
            (true, true, _) => way.as_str(),
            (true, false, Some(copied)) => copied,
        };
        let switch = switch_first(provider.title());
        let manual = self.kind == Some(ChallengeKind::ManualCode);
        let key = |key: &str, word: &str| KeyHint::new(key, word);
        let (open, copy_link) = (copy::OPEN_WORD, copy::COPY_WORD);
        let finish = key(copy::ENTER_KEY, copy::FINISH_WORD);
        let footer = match (ended, manual, self.opened) {
            (true, _, true) => vec![key(copy::ENTER_KEY, open), cancel()],
            (true, _, false) => vec![key(copy::ENTER_KEY, copy_link), cancel()],
            (false, true, true) => vec![
                finish,
                key(copy::KEY_CTRL_O, open),
                key("ctrl-l", copy_link),
                cancel(),
            ],
            (false, true, false) => vec![finish, key("ctrl-l", copy_link), cancel()],
            (false, false, true) => vec![key(copy::ENTER_KEY, open), key("c", copy_link), cancel()],
            (false, false, false) => vec![key(copy::ENTER_KEY, copy_link), cancel()],
        };
        let field = manual.then(|| FormField {
            label: if provider == SubscriptionProvider::Antigravity {
                "Callback URL"
            } else {
                "Code"
            },
            value: if ended {
                FormValue::Closed
            } else {
                self.code.form_value()
            },
        });
        form_rows(
            &FormView {
                title,
                title_line: None,
                note: "",
                prompt: if self.other_account() { &switch } else { "" },
                prompt_lines: &[],
                error: &self.message,
                options: &[],
                option_marks: &[],
                selected: Some(0),
                field,
                footer: &footer,
                account,
                done: false,
                link: Some(LinkRow {
                    text: self.link_text.as_deref(),
                    browser: browser_words(self.opened),
                    copy: copied,
                    copy_look: if self.way_left {
                        Token::Text
                    } else {
                        Token::TextMuted
                    },
                    slot: slot(provider, glyphs, layout.width_mode),
                }),
                refused: None,
            },
            room,
            layout,
        )
    }
}

pub(crate) fn cancel() -> KeyHint {
    copy::esc(copy::CANCEL_WORD)
}

fn enter_close() -> KeyHint {
    KeyHint::new(copy::ENTER_KEY, copy::CLOSE_WORD)
}

pub(crate) fn choose_keys() -> Vec<KeyHint> {
    vec![copy::enter_choose(), cancel()]
}

const SIGN_IN_TO_START: &str = "Sign in to start";

pub(crate) fn columns(rows: Vec<(String, Vec<String>)>, layout: Layout) -> Vec<QuestionOption> {
    let mode = layout.width_mode;
    let column = rows
        .iter()
        .map(|(label, _)| width(label, mode))
        .max()
        .unwrap_or(0)
        + 3;
    let fits = |detail: &String| 5 + column + width(detail, mode) < layout.width;
    let levels = rows
        .iter()
        .map(|(_, details)| details.len())
        .max()
        .unwrap_or(0);
    let level = (0..levels).find(|&at| rows.iter().all(|(_, d)| d.get(at).is_none_or(fits)));
    rows.into_iter()
        .map(|(label, details)| {
            let detail = level
                .and_then(|at| details.get(at))
                .filter(|d| !d.is_empty());
            QuestionOption {
                label: match detail {
                    Some(detail) => {
                        let pad = column - width(&label, mode);
                        format!("{label}{}{detail}", " ".repeat(pad))
                    }
                    None => label,
                },
                description: String::new(),
            }
        })
        .collect()
}

fn storage(store: Option<StoreKind>) -> &'static str {
    match store {
        Some(StoreKind::File) => "protected file storage",
        Some(StoreKind::Keyring) => "keychain storage",
        Some(StoreKind::Other(_)) => "unrecognized storage kind",
        None => "storage not reported",
    }
}

pub mod sign_in;

#[cfg(test)]
mod tests;
