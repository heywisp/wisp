use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use wisp_core::endpoints::ProviderKind;
use wisp_core::secrets::StoreKind;
use wisp_protocol::endpoint::StoreChoice;
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthFailureKind, AuthOutcome, ChallengeKind, PrivateChallenge,
    SubscriptionProvider,
};
use wisp_protocol::{
    ACCOUNT_NAME_LABEL, CodeProblem, RequestId, Secret, account_name, account_name_problem,
    account_name_taken,
};
use wisp_render::chrome::account::{
    AccountKind, AccountState, CLEANUP_WARNING, Ending, LINK_NOT_COPIED, Mistake, SIGN_OUT_OPTIONS,
    SameAccount, Surface, billing_word, browser_words, login_again, mark_line, marked,
    mistake_line, mistake_of, same_account_rows, sign_in_again_to, signed_in_lines, switch_first,
};
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::text::{WidthMode, padded, width as text_width, wrap_plain};
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::Glyphs;
use wisp_tui::term::Caps;
use wisp_tui::term::caps::Env;
use wisp_tui::term::clipboard::{Clipboard, Host, Outcome, Route, Running, copy_once, link_words};
use wisp_tui::term::print::{Stream, inline, styled};
use wisp_tui::term::prompt::{EnterWatch, ListRow, PromptError, Prompter, lead_width};

use super::accounts::{Account, Accounts, Health, Held};
use crate::manage::{Context, label_rows};

const WATCH_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) const CANCELLED: &str = "Sign-in cancelled; nothing was saved.";
pub(crate) fn no_word() -> String {
    let mut words = SubscriptionProvider::names().to_vec();
    words.push("key");
    format!(
        "say what to sign in to: wisp login {}.",
        wisp_protocol::settings::listed(&words, "or")
    )
}
pub(crate) const KEY_SCRIPT: &str =
    "a key needs a terminal to paste into; scripts use wisp account add NAME --key-stdin.";
pub(crate) const FIRST: &str = "Run wisp to start a chat.";
pub(crate) const SWITCH: &str = "In a chat, /account switches to it.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Goal {
    Pick {
        name: Option<String>,
    },
    Subscription {
        provider: SubscriptionProvider,
        name: Option<String>,
    },
    Key {
        name: Option<String>,
        provider: Option<SubscriptionProvider>,
    },
    Status(Option<String>),
    Again(Account),
    Refuse(String),
}

pub(crate) fn keyword(word: &str) -> Option<SubscriptionProvider> {
    SubscriptionProvider::from_name(word)
}

fn named(name: &str, accounts: &Accounts) -> Goal {
    match accounts.get(name) {
        Some(account) => Goal::Again(account.clone()),
        None => Goal::Pick {
            name: Some(name.to_owned()),
        },
    }
}

pub(crate) fn reaches(name: &str, accounts: &Accounts) -> bool {
    matches!(route(Some(name), None, None, None, accounts), Goal::Again(account) if account.name == name)
}

pub(crate) fn route(
    what: Option<&str>,
    more: Option<&str>,
    name: Option<&str>,
    provider: Option<ProviderKind>,
    accounts: &Accounts,
) -> Goal {
    let provider = provider.map(super::session::wire_provider);
    let name = name.map(|given| account_name(given).unwrap_or_else(|| given.to_owned()));
    let unusable = |name: &str| !accounts.taken(name) && account_name(name).is_none();
    match (what, more) {
        (Some("status"), more) => Goal::Status(more.map(str::to_owned)),
        (Some(_), Some(extra)) => {
            Goal::Refuse(format!("wisp login takes one word; {extra} was left over"))
        }
        (Some("key"), None) => match name {
            Some(name) if accounts.taken(&name) => match accounts.get(&name) {
                Some(account) if matches!(account.held, Held::Subscription(_)) => {
                    Goal::Refuse(format!(
                        "{name} is a {} account, not a key; sign in to it again with {}, or give another --name",
                        account.kind.label(),
                        login_again(Surface::Terminal, &name, reaches(&name, accounts))
                    ))
                }
                _ => named(&name, accounts),
            },
            Some(name) if unusable(&name) => Goal::Refuse(account_name_problem(&name)),
            name => Goal::Key { name, provider },
        },
        (Some(word), None) => match keyword(word) {
            Some(provider) => Goal::Subscription { provider, name },
            None => named(word, accounts),
        },
        (None, _) => match (provider, name) {
            (Some(provider), name) => Goal::Subscription { provider, name },
            (None, Some(name)) if unusable(&name) => Goal::Refuse(account_name_problem(&name)),
            (None, Some(name)) => named(&name, accounts),
            (None, None) => Goal::Pick { name: None },
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    Failed(String),
    Said,
    SameAccount(SameAccount),
}

impl From<PromptError> for Stop {
    fn from(error: PromptError) -> Self {
        match error {
            PromptError::Cancelled => Self::Cancelled,
            PromptError::Io(e) => Self::Failed(format!("could not read the terminal: {e}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) word: String,
    pub(crate) mark: Option<u8>,
    pub(crate) label: String,
    pub(crate) detail: String,
    pub(crate) keep_detail: bool,
}

pub(crate) trait Front {
    fn say(&mut self, text: &str);
    fn marked(&mut self, text: &str, _mark: Option<u8>) {
        self.say(text);
    }
    fn link(&mut self, url: &Secret, browser: &str) -> Result<(), Stop> {
        self.say(url.expose());
        self.say(browser);
        Ok(())
    }
    fn glyphs(&self) -> Glyphs {
        wisp_tui::term::prompt::glyphs()
    }
    fn choose(&mut self, rows: &[Row]) -> Result<usize, Stop>;
    fn line(&mut self, question: &str, start: &str) -> Result<String, Stop>;
    fn code(&mut self, question: &str) -> Result<String, Stop>;
    fn enter(&mut self) -> Result<(), Stop>;
    fn wait(&mut self, done: &dyn Fn() -> bool) -> Result<(), Stop>;
    fn confirm(&mut self, question: &str) -> Result<bool, Stop> {
        self.say(question);
        Ok(self.choose(&label_rows(&SIGN_OUT_OPTIONS))? == 0)
    }
}

pub(crate) trait Auth: Clone + Send + Sync + 'static {
    fn send(&self, command: AuthCommand) -> BoxFuture<'static, AuthEvent>;
    fn prefer(&self, login: RequestId, store: Option<StoreKind>, make_default: bool);
}

impl Auth for crate::manage::host::CliEndpointHost {
    fn send(&self, command: AuthCommand) -> BoxFuture<'static, AuthEvent> {
        let host = self.clone();
        Box::pin(async move { super::session_auth(&host, command).await })
    }

    fn prefer(&self, login: RequestId, store: Option<StoreKind>, make_default: bool) {
        self.auth.prefer(login, store, make_default);
    }
}

pub(crate) fn rows_text(rows: &[Row], width: usize, glyphs: &Glyphs) -> Vec<ListRow> {
    let mode = WidthMode::from(glyphs.set);
    let room = width.saturating_sub(lead_width(rows.len()));
    let labels: Vec<String> = rows
        .iter()
        .map(|row| match (row.mark, row.label.rsplit_once(' ')) {
            (None, _) => row.label.clone(),
            (Some(_), Some((words, name))) => format!("{words} {}", marked(name, row.mark, glyphs)),
            (Some(_), None) => marked(&row.label, row.mark, glyphs),
        })
        .collect();
    let label_w = labels
        .iter()
        .map(|l| text_width(l, mode))
        .max()
        .unwrap_or(0);
    let wide = rows
        .iter()
        .all(|r| label_w + 3 + text_width(&r.detail, mode) <= room);
    let lines = |text: &str| wrap_plain(text, room, "", "", glyphs);
    rows.iter()
        .zip(&labels)
        .map(|(row, label)| match (wide, row.detail.is_empty()) {
            (_, true) => lines(label),
            (true, false) => lines(&format!(
                "{}   {}",
                padded(label, label_w, mode),
                row.detail
            )),
            (false, false) if row.keep_detail => [lines(label), lines(&row.detail)].concat(),
            (false, false) => lines(label),
        })
        .zip(rows.iter().map(|row| row.mark))
        .collect()
}

pub(crate) fn numbered_text(rows: &[Row], width: usize, glyphs: &Glyphs) -> String {
    let unmarked: Vec<Row> = rows
        .iter()
        .map(|row| Row {
            mark: None,
            ..row.clone()
        })
        .collect();
    wisp_tui::term::prompt::numbered(&rows_text(&unmarked, width, glyphs), glyphs)
}

pub(crate) fn wrapped(text: &str, width: usize, lead: &str, glyphs: &Glyphs) -> String {
    wrap_plain(text, width, lead, &" ".repeat(lead.len()), glyphs).join("\n")
}

pub(crate) fn pick_answer(answer: &str, rows: &[Row]) -> Option<usize> {
    let answer = answer.trim();
    crate::manage::picked(answer, rows.len(), None).or_else(|| {
        rows.iter()
            .position(|row| !answer.is_empty() && row.word.eq_ignore_ascii_case(answer))
    })
}

pub(crate) struct Terminal {
    prompter: Prompter,
    width: usize,
    glyphs: Glyphs,
    first: bool,
}

impl Terminal {
    pub(crate) fn new(prompter: Prompter, first: bool) -> Self {
        Self {
            width: wisp_tui::term::prompt::terminal_columns(),
            glyphs: prompter.glyphs(),
            prompter,
            first,
        }
    }

    fn copied(url: &Secret, clipboard: Clipboard, watch: &EnterWatch) -> Result<Outcome, Stop> {
        let running = Arc::new(Running::default());
        let (sent, done) = std::sync::mpsc::channel();
        let (text, copying) = (url.clone(), Arc::clone(&running));
        let spawned = std::thread::Builder::new()
            .name("wisp-login-copy".to_owned())
            .spawn(move || {
                let outcome =
                    copy_once(&clipboard, text.expose(), &mut std::io::stderr(), &copying);
                drop(sent.send(outcome));
            });
        if let Err(e) = spawned {
            return Ok(Outcome::Failed(e.to_string()));
        }
        loop {
            match done.try_recv() {
                Ok(outcome) => return Ok(outcome),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Ok(Outcome::Failed("the copy stopped".to_owned()));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            if let Err(stop) = watch.poll(WATCH_INTERVAL) {
                running.stop();
                let _ = std::io::stderr().write_all(b"\r\n");
                return Err(stop.into());
            }
        }
    }
}

fn muted(text: &str) -> StyledLine {
    StyledLine::new(vec![StyledSpan::new(text, Token::TextMuted)])
}

impl Front for Terminal {
    fn say(&mut self, text: &str) {
        for line in text.lines() {
            eprintln!("{}", wrapped(line, self.width, "", &self.glyphs));
        }
    }

    fn marked(&mut self, text: &str, mark: Option<u8>) {
        let lines = marked_lines(text, mark, self.width, &self.glyphs);
        let _ = print_lines(&lines, self.prompter.caps(), Stream::Stderr, 0);
    }

    fn link(&mut self, url: &Secret, browser: &str) -> Result<(), Stop> {
        let link = StyledLine::new(vec![StyledSpan::new(url.expose(), Token::Accent)]);
        let _ = print_lines(&[link], self.prompter.caps(), Stream::Stderr, 0);
        let clipboard = Clipboard::current(&Host::current(&Env::current()));
        let watch = (!Env::current().dumb() || clipboard.route != Route::Terminal)
            .then(|| self.prompter.watch_enter())
            .transpose()?;
        let part = inline(muted(browser), self.prompter.caps(), Stream::Stderr, 0);
        let _ = std::io::stderr().write_all(&part);
        let words = if let Some(watch) = &watch {
            link_words(&Self::copied(url, clipboard, watch)?)
        } else {
            LINK_NOT_COPIED
        };
        let line_end: &[u8] = if watch.is_some() { b"\r\n" } else { b"\n" };
        let mut err = std::io::stderr().lock();
        for line in status_tail(browser, words, self.width, &self.glyphs) {
            let _ = err.write_all(&inline(
                muted(&line),
                self.prompter.caps(),
                Stream::Stderr,
                0,
            ));
            let _ = err.write_all(line_end);
        }
        Ok(())
    }

    fn glyphs(&self) -> Glyphs {
        self.glyphs
    }

    fn choose(&mut self, rows: &[Row]) -> Result<usize, Stop> {
        Ok(crate::manage::pick_row(
            &self.prompter,
            "",
            rows,
            self.first.then_some(0),
            &crate::manage::choose_prompt(rows.len(), None),
            |answer| pick_answer(answer, rows),
        )?)
    }

    fn confirm(&mut self, question: &str) -> Result<bool, Stop> {
        match crate::manage::choose(&self.prompter, question, &SIGN_OUT_OPTIONS, 1) {
            crate::manage::Asked::Answer(row) => Ok(row == 0),
            crate::manage::Asked::Declined => Ok(false),
            crate::manage::Asked::Cancelled => Err(Stop::Cancelled),
            crate::manage::Asked::Failed(why) => Err(Stop::Failed(why)),
        }
    }

    fn line(&mut self, question: &str, start: &str) -> Result<String, Stop> {
        Ok(self.prompter.edit(question, start)?)
    }

    fn code(&mut self, question: &str) -> Result<String, Stop> {
        Ok(self.prompter.masked(question)?)
    }

    fn enter(&mut self) -> Result<(), Stop> {
        self.prompter.visible("")?;
        Ok(())
    }

    fn wait(&mut self, done: &dyn Fn() -> bool) -> Result<(), Stop> {
        let watch = self.prompter.watch_enter()?;
        while !done() {
            watch.poll(WATCH_INTERVAL)?;
        }
        Ok(())
    }
}

pub(crate) struct Asking<'a>(pub(crate) &'a Prompter);

impl Front for Asking<'_> {
    fn say(&mut self, text: &str) {
        eprintln!("{text}");
    }

    fn glyphs(&self) -> Glyphs {
        self.0.glyphs()
    }

    fn choose(&mut self, _: &[Row]) -> Result<usize, Stop> {
        Err(Stop::Said)
    }

    fn line(&mut self, question: &str, start: &str) -> Result<String, Stop> {
        Ok(self.0.edit(question, start)?)
    }

    fn code(&mut self, question: &str) -> Result<String, Stop> {
        Ok(self.0.masked(question)?)
    }

    fn enter(&mut self) -> Result<(), Stop> {
        Err(Stop::Said)
    }

    fn wait(&mut self, _: &dyn Fn() -> bool) -> Result<(), Stop> {
        Err(Stop::Said)
    }
}

pub(crate) struct Piped {
    read: bool,
}

impl Front for Piped {
    fn say(&mut self, text: &str) {
        eprintln!("{text}");
    }

    fn choose(&mut self, _: &[Row]) -> Result<usize, Stop> {
        Err(Stop::Failed(no_word()))
    }

    fn line(&mut self, _: &str, _: &str) -> Result<String, Stop> {
        Err(Stop::Said)
    }

    fn code(&mut self, _: &str) -> Result<String, Stop> {
        if std::mem::replace(&mut self.read, true) {
            return Err(Stop::Said);
        }
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|_| Stop::Failed("could not read the sign-in code".to_owned()))?;
        Ok(line)
    }

    fn enter(&mut self) -> Result<(), Stop> {
        Err(Stop::Said)
    }

    fn wait(&mut self, done: &dyn Fn() -> bool) -> Result<(), Stop> {
        while !done() {
            std::thread::sleep(WATCH_INTERVAL);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Want {
    pub(crate) provider: SubscriptionProvider,
    pub(crate) name: String,
    pub(crate) again: bool,
    pub(crate) another: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Done {
    pub(crate) name: String,
    pub(crate) mark: Option<u8>,
    pub(crate) ready: bool,
    pub(crate) cleanup_pending: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Prefs {
    pub(crate) store: Option<StoreKind>,
    pub(crate) make_default: bool,
}

pub(crate) fn name_step(
    front: &mut dyn Front,
    taken_by: &dyn Fn(&str) -> bool,
    suggested: &str,
) -> Result<String, Stop> {
    let question = format!("{ACCOUNT_NAME_LABEL}: ");
    let mut start = suggested;
    if taken_by(suggested) {
        front.say(&account_name_taken(suggested));
        start = "";
    }
    loop {
        let answer = front.line(&question, start)?;
        start = "";
        match account_name(&answer) {
            None => front.say(&account_name_problem(&answer)),
            Some(name) if taken_by(&name) => front.say(&account_name_taken(&name)),
            Some(name) => return Ok(name),
        }
    }
}

fn another_name(front: &mut dyn Front, accounts: &Accounts, taken: &str) -> Result<String, Stop> {
    name_step(front, &|name| accounts.taken(name) || name == taken, taken)
}

struct Ids(u64);

impl Ids {
    fn next(&mut self) -> RequestId {
        self.0 += 1;
        RequestId(self.0)
    }
}

enum Turn {
    Done(Done),
    NewPage,
}

fn intro(
    front: &mut dyn Front,
    want: &Want,
    challenge: &PrivateChallenge,
    mark: Option<u8>,
) -> Result<(), Stop> {
    let provider = want.provider.title();
    let who = match mark {
        Some(_) => marked(&want.name, mark, &front.glyphs()),
        None => want.name.clone(),
    };
    let again = if want.again { " again" } else { "" };
    let then = match (challenge.kind, want.provider) {
        (ChallengeKind::ManualCode, SubscriptionProvider::Antigravity) => {
            ", then paste the callback URL."
        }
        (ChallengeKind::ManualCode, _) => ", then paste the code it shows.",
        (ChallengeKind::BrowserLoopback, _) => "; wisp carries on by itself.",
    };
    front.marked(
        &format!("Sign in{again} on {provider}'s page for {who}{then}"),
        mark,
    );
    if want.another {
        front.say(&switch_first(provider));
    }
    front.link(
        &challenge.authorization_url,
        browser_words(challenge.opened),
    )
}

fn status_tail(browser: &str, words: &str, width: usize, glyphs: &Glyphs) -> Vec<String> {
    let joined = format!(" {} {words}", glyphs.separator);
    let mode = WidthMode::from(glyphs.set);
    if text_width(browser, mode) + text_width(&joined, mode) <= width {
        vec![joined]
    } else {
        vec![String::new(), words.to_owned()]
    }
}

fn finished(outcome: &AuthOutcome) -> Option<Done> {
    match outcome {
        AuthOutcome::Ready {
            endpoint,
            previous_cleanup_pending,
        } => Some(Done {
            name: endpoint.alias.clone(),
            mark: None,
            ready: true,
            cleanup_pending: *previous_cleanup_pending,
        }),
        AuthOutcome::SavedNotReady {
            endpoint,
            previous_cleanup_pending,
        } => Some(Done {
            name: endpoint.alias.clone(),
            mark: None,
            ready: false,
            cleanup_pending: *previous_cleanup_pending,
        }),
        _ => None,
    }
}

fn failure(
    outcome: AuthOutcome,
    provider: SubscriptionProvider,
    accounts: &Accounts,
) -> Result<(AuthFailureKind, Option<CodeProblem>), Stop> {
    match outcome {
        AuthOutcome::Failed {
            same_account: Some(name),
            ..
        } => Err(refused(provider, name, accounts)),
        AuthOutcome::Failed { kind, problem, .. } => Ok((kind, problem)),
        _ => Err(Stop::Cancelled),
    }
}

fn refused(provider: SubscriptionProvider, name: String, accounts: &Accounts) -> Stop {
    Stop::SameAccount(SameAccount {
        provider,
        short: reaches(&name, accounts),
        mark: accounts.get(&name).and_then(|a| a.mark),
        name,
        surface: Surface::Terminal,
    })
}

pub(crate) fn same_account_lines(
    refusal: &SameAccount,
    width: usize,
    glyphs: &Glyphs,
) -> Vec<StyledLine> {
    let lead = format!("{} ", glyphs.failed);
    let hang = " ".repeat(text_width(&lead, WidthMode::from(glyphs.set)));
    let room = width.saturating_sub(hang.len());
    same_account_rows(refusal, room, glyphs)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let mut spans = vec![if index == 0 {
                StyledSpan::new(lead.clone(), Token::Error)
            } else {
                StyledSpan::plain(hang.clone())
            }];
            spans.extend(row.spans);
            StyledLine::new(spans)
        })
        .collect()
}

async fn paste<A: Auth>(
    auth: &A,
    front: &mut dyn Front,
    accounts: &Accounts,
    ids: &mut Ids,
    login: RequestId,
    want: &mut Want,
) -> Result<Turn, Stop> {
    let provider = want.provider.title();
    let mut queued: Option<String> = None;
    let mut rename: Option<String> = None;
    let mut last: Option<String> = None;
    let mut unreachable = false;
    loop {
        let code = match queued.take() {
            Some(code) => code,
            None => {
                let prompt = if want.provider == SubscriptionProvider::Antigravity {
                    "Callback URL: "
                } else {
                    "Code: "
                };
                let typed = front.code(prompt)?;
                match (typed.trim().is_empty(), last.take()) {
                    (false, _) => typed,
                    (true, Some(code)) => code,
                    (true, None) => {
                        front.say(&mistake_line(Mistake::Pasted(CodeProblem::Empty), provider));
                        continue;
                    }
                }
            }
        };
        let event = auth
            .send(AuthCommand::Finish {
                request: ids.next(),
                login,
                response: Some(Secret::new(code.clone())),
                alias: rename.take(),
            })
            .await;
        if let Some(done) = finished(&event.outcome) {
            return Ok(Turn::Done(done));
        }
        let (kind, problem) = failure(event.outcome, want.provider, accounts)?;
        match kind {
            AuthFailureKind::AliasConflict if !want.again => {
                let name = another_name(front, accounts, &want.name)?;
                want.name.clone_from(&name);
                rename = Some(name);
                queued = Some(code);
            }
            AuthFailureKind::CodeRejected => {
                front.say(&mistake_line(mistake_of(kind, problem), provider));
            }
            AuthFailureKind::Unreachable if !unreachable => {
                unreachable = true;
                front.say(&mistake_line(Mistake::Unreachable, provider));
                last = Some(code);
            }
            _ => {
                front.say(&mistake_line(mistake_of(kind, problem), provider));
                front.enter()?;
                return Ok(Turn::NewPage);
            }
        }
    }
}

async fn loopback<A: Auth>(
    auth: &A,
    front: &mut dyn Front,
    accounts: &Accounts,
    ids: &mut Ids,
    login: RequestId,
    want: &mut Want,
) -> Result<Turn, Stop> {
    let provider = want.provider.title();
    let mut rename: Option<String> = None;
    let mut unreachable = false;
    loop {
        let task = tokio::spawn(auth.send(AuthCommand::Finish {
            request: ids.next(),
            login,
            response: None,
            alias: rename.take(),
        }));
        if !task.is_finished()
            && let Err(stop) = front.wait(&|| task.is_finished())
        {
            auth.send(AuthCommand::Cancel {
                request: ids.next(),
                login,
            })
            .await;
            return match task.await.ok().and_then(|event| finished(&event.outcome)) {
                Some(done) => Ok(Turn::Done(done)),
                None => Err(stop),
            };
        }
        let event = task
            .await
            .map_err(|_| Stop::Failed("the sign-in stopped".to_owned()))?;
        if let Some(done) = finished(&event.outcome) {
            return Ok(Turn::Done(done));
        }
        let (kind, problem) = failure(event.outcome, want.provider, accounts)?;
        match kind {
            AuthFailureKind::AliasConflict if !want.again => {
                let name = another_name(front, accounts, &want.name)?;
                want.name.clone_from(&name);
                rename = Some(name);
            }
            AuthFailureKind::Unreachable if !unreachable => {
                unreachable = true;
                front.say(&mistake_line(Mistake::Unreachable, provider));
                front.enter()?;
                auth.send(AuthCommand::Open {
                    request: ids.next(),
                    login,
                })
                .await;
            }
            _ => {
                front.say(&mistake_line(mistake_of(kind, problem), provider));
                front.enter()?;
                return Ok(Turn::NewPage);
            }
        }
    }
}

pub(crate) async fn subscribe<A: Auth>(
    auth: &A,
    front: &mut dyn Front,
    accounts: &Accounts,
    mut want: Want,
    prefs: Prefs,
) -> Result<Done, Stop> {
    let mut ids = Ids(0);
    loop {
        let login = ids.next();
        let started = auth
            .send(AuthCommand::Start {
                request: login,
                provider: want.provider,
                alias: Some(want.name.clone()),
                reauthorize: want.again,
            })
            .await;
        let challenge = match started.outcome {
            AuthOutcome::Challenge { challenge } => challenge,
            AuthOutcome::Failed {
                kind: AuthFailureKind::AliasConflict,
                ..
            } if !want.again => {
                want.name = another_name(front, accounts, &want.name)?;
                continue;
            }
            AuthOutcome::Failed { message, .. } => return Err(Stop::Failed(message)),
            _ => return Err(Stop::Failed("the sign-in didn't start".to_owned())),
        };
        auth.prefer(login, prefs.store, prefs.make_default);
        let mark = accounts.get(&want.name).and_then(|account| account.mark);
        let turn = match (intro(front, &want, &challenge, mark), challenge.kind) {
            (Err(stop), _) => Err(stop),
            (Ok(()), ChallengeKind::ManualCode) => {
                paste(auth, front, accounts, &mut ids, login, &mut want).await
            }
            (Ok(()), ChallengeKind::BrowserLoopback) => {
                loopback(auth, front, accounts, &mut ids, login, &mut want).await
            }
        };
        match turn {
            Ok(Turn::Done(done)) => return Ok(done),
            Ok(Turn::NewPage) => {}
            Err(Stop::Cancelled) => {
                auth.send(AuthCommand::Cancel {
                    request: ids.next(),
                    login,
                })
                .await;
                return Err(Stop::Cancelled);
            }
            Err(stop) => return Err(stop),
        }
    }
}

pub(crate) fn result_lines(
    kind: AccountKind,
    done: &Done,
    again: bool,
    first: bool,
    glyphs: &Glyphs,
    width: usize,
) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    if !done.ready {
        lines.push(format!(
            "It's saved but can't be used yet; wisp status {} shows why.",
            done.name
        ));
    } else if first && !again {
        lines.push(FIRST.to_owned());
    } else if !again {
        lines.push(SWITCH.to_owned());
    }
    signed_in_lines(again, kind, &done.name, done.mark, glyphs, width)
        .iter()
        .map(|line| mark_line(line, Token::Text, done.mark, glyphs))
        .chain(
            lines
                .iter()
                .flat_map(|line| marked_lines(line, done.mark, width, glyphs)),
        )
        .collect()
}

pub(crate) fn marked_lines(
    text: &str,
    mark: Option<u8>,
    width: usize,
    glyphs: &Glyphs,
) -> Vec<StyledLine> {
    wrap_plain(text, width, "", "", glyphs)
        .iter()
        .map(|line| mark_line(line, Token::Text, mark, glyphs))
        .collect()
}

pub(crate) fn print_lines(
    lines: &[StyledLine],
    caps: &Caps,
    stream: Stream,
    width: usize,
) -> std::io::Result<()> {
    let bytes = styled(lines, caps, stream, width);
    match stream {
        Stream::Stdout => std::io::stdout().write_all(&bytes),
        Stream::Stderr => std::io::stderr().write_all(&bytes),
    }
}

fn ordered(accounts: &Accounts, provider: SubscriptionProvider) -> Vec<&Account> {
    let mut list = accounts.of(provider);
    list.sort_by_key(|account| account.health == Health::SignedIn);
    list
}

pub(crate) fn plan_subscription(
    front: &mut dyn Front,
    accounts: &Accounts,
    provider: SubscriptionProvider,
    name: Option<String>,
    terminal: bool,
) -> Result<Want, Stop> {
    let suggested = provider.default_name();
    let existing = ordered(accounts, provider);
    if let Some(given) = name {
        let Some(name) = account_name(&given) else {
            return Err(Stop::Failed(account_name_problem(&given)));
        };
        return match accounts.get(&name) {
            Some(account) if account.held == Held::Subscription(provider) => Ok(Want {
                provider,
                another: existing.iter().any(|other| other.name != name),
                name,
                again: true,
            }),
            Some(_) => Err(Stop::Failed(format!(
                "{name} isn't a {} account; give another --name",
                provider.title()
            ))),
            None => Ok(Want {
                provider,
                name,
                again: false,
                another: !existing.is_empty(),
            }),
        };
    }
    if existing.is_empty() {
        let name = if terminal {
            name_step(front, &|name| accounts.taken(name), suggested)?
        } else if accounts.taken(suggested) {
            return Err(Stop::Failed(format!(
                "{suggested} is taken; give a name: wisp login {} --name NAME",
                provider.default_name()
            )));
        } else {
            suggested.to_owned()
        };
        return Ok(Want {
            provider,
            name,
            again: false,
            another: false,
        });
    }
    if !terminal {
        let names: Vec<&str> = existing.iter().map(|a| a.name.as_str()).collect();
        return Err(Stop::Failed(format!(
            "you have {} {} already; sign in again with wisp login --name NAME, or add another with wisp login {} --name NAME",
            provider.title(),
            if names.len() == 1 {
                format!("account {}", names.join(""))
            } else {
                format!("accounts {}", names.join(", "))
            },
            provider.default_name()
        )));
    }
    let mut rows: Vec<Row> = existing
        .iter()
        .map(|account| Row {
            word: account.name.clone(),
            mark: account.mark,
            label: sign_in_again_to(&account.name),
            detail: if account.health == Health::Expired {
                billing_word(AccountState::Expired).to_owned()
            } else {
                String::new()
            },
            keep_detail: false,
        })
        .collect();
    rows.push(Row {
        word: "another".to_owned(),
        mark: None,
        label: format!("Add another {} account", provider.title()),
        detail: String::new(),
        keep_detail: false,
    });
    let picked = front.choose(&rows)?;
    if let Some(account) = existing.get(picked) {
        return Ok(Want {
            provider,
            name: account.name.clone(),
            again: true,
            another: existing.len() > 1,
        });
    }
    Ok(Want {
        provider,
        name: name_step(front, &|name| accounts.taken(name), suggested)?,
        again: false,
        another: true,
    })
}

pub(crate) enum Picked {
    Subscription(SubscriptionProvider),
    Key,
}

pub(crate) fn first_question(front: &mut dyn Front) -> Result<Picked, Stop> {
    let mut rows: Vec<Row> = SubscriptionProvider::ALL
        .into_iter()
        .map(|provider| Row {
            word: provider.default_name().to_owned(),
            mark: None,
            label: provider.title().to_owned(),
            detail: provider.subscription_words().to_owned(),
            keep_detail: false,
        })
        .collect();
    rows.push(Row {
        word: "key".to_owned(),
        mark: None,
        label: wisp_render::chrome::account::API_KEY.to_owned(),
        detail: SubscriptionProvider::API_KEY_WORDS.to_owned(),
        keep_detail: false,
    });
    Ok(
        match SubscriptionProvider::ALL.get(front.choose(&rows)?).copied() {
            Some(provider) => Picked::Subscription(provider),
            None => Picked::Key,
        },
    )
}

pub(crate) fn setup_token_line(name: &str) -> String {
    format!(
        "{name} uses a setup token; give it a new one with wisp account edit {name} --replace-key"
    )
}

pub(crate) struct Args<'a> {
    pub(crate) what: Option<&'a str>,
    pub(crate) more: Option<&'a str>,
    pub(crate) name: Option<&'a str>,
    pub(crate) provider: Option<ProviderKind>,
    pub(crate) store: Option<StoreChoice>,
    pub(crate) make_default: bool,
}

fn store_of(ctx: &Context<'_>, store: Option<StoreChoice>) -> Result<Option<StoreKind>, Stop> {
    match store.and_then(StoreKind::chosen) {
        Some(StoreKind::Keyring) if !ctx.keychain.detected().available() => Err(Stop::Failed(
            "the keychain is unavailable; use --store file to keep the sign-in in a protected file"
                .to_owned(),
        )),
        kind => Ok(kind),
    }
}

fn finish(stop: Stop, caps: &Caps) -> ExitCode {
    match stop {
        Stop::Cancelled => {
            eprintln!("{CANCELLED}");
            ExitCode::from(wisp_tui::term::prompt::CANCEL_EXIT)
        }
        Stop::Failed(message) => {
            eprintln!("wisp: {message}");
            ExitCode::FAILURE
        }
        Stop::Said => ExitCode::FAILURE,
        Stop::SameAccount(refusal) => {
            let width = wisp_tui::term::prompt::terminal_columns();
            let lines = same_account_lines(&refusal, width, &wisp_tui::term::prompt::glyphs());
            let _ = print_lines(&lines, caps, Stream::Stderr, width);
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn login(
    ctx: &Context<'_>,
    keychain: Arc<crate::keychain::Keychain>,
    args: &Args<'_>,
) -> ExitCode {
    use std::io::IsTerminal;
    let accounts = match super::accounts::gather(ctx) {
        Ok(accounts) => accounts,
        Err(error) => return finish(Stop::Failed(error), &ctx.caps),
    };
    let terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let mut tty = Terminal::new(ctx.prompter(CANCELLED), true);
    let mut piped = Piped { read: false };
    let front: &mut dyn Front = if terminal { &mut tty } else { &mut piped };
    let goal = route(args.what, args.more, args.name, args.provider, &accounts);
    let prefs = match store_of(ctx, args.store) {
        Ok(store) => Prefs {
            store,
            make_default: args.make_default,
        },
        Err(stop) => return finish(stop, &ctx.caps),
    };
    let (provider, name) = match goal {
        Goal::Status(name) => return super::accounts::status(ctx, name.as_deref()),
        Goal::Refuse(message) => return finish(Stop::Failed(message), &ctx.caps),
        Goal::Again(account) => match &account.held {
            Held::Subscription(provider) => (*provider, Some(account.name.clone())),
            Held::Stored => {
                return match crate::manage::edit::key_args(&account.name) {
                    Ok(edit) => crate::manage::edit::run(&edit, ctx),
                    Err(error) => finish(Stop::Failed(error), &ctx.caps),
                };
            }
            Held::SetupToken => {
                return finish(Stop::Failed(setup_token_line(&account.name)), &ctx.caps);
            }
            Held::Variable { .. } => {
                let line = accounts.variable_line(&account.name, Ending::Change);
                println!("{}", line.unwrap_or_default());
                return ExitCode::SUCCESS;
            }
        },
        Goal::Key { name, provider } => return key(ctx, terminal, name, provider),
        Goal::Subscription { provider, name } => (provider, name),
        Goal::Pick { name } => {
            if !terminal {
                return finish(Stop::Failed(no_word()), &ctx.caps);
            }
            match first_question(front) {
                Ok(Picked::Subscription(provider)) => (provider, name),
                Ok(Picked::Key) => return key(ctx, terminal, name, None),
                Err(stop) => return finish(stop, &ctx.caps),
            }
        }
    };
    let want = match plan_subscription(front, &accounts, provider, name, terminal) {
        Ok(want) => want,
        Err(stop) => return finish(stop, &ctx.caps),
    };
    let host = crate::manage::host::CliEndpointHost::of(
        ctx,
        None,
        keychain,
        wisp_core::secrets::keyring::Budget::Fresh(wisp_core::secrets::keyring::TERMINAL_DEADLINE),
        &[],
    );
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            return finish(
                Stop::Failed("could not start authentication".to_owned()),
                &ctx.caps,
            );
        }
    };
    let result = runtime.block_on(subscribe(&host, front, &accounts, want.clone(), prefs));
    match result {
        Ok(mut done) => {
            if done.cleanup_pending {
                eprintln!("{CLEANUP_WARNING}");
            }
            done.mark = super::accounts::gather(ctx)
                .ok()
                .and_then(|a| a.get(&done.name)?.mark);
            let lines = result_lines(
                AccountKind::from(want.provider),
                &done,
                want.again,
                accounts.is_first(&done.name),
                &tty.glyphs,
                wisp_tui::term::prompt::terminal_columns(),
            );
            let _ = print_lines(&lines, &ctx.caps, Stream::Stdout, 0);
            ExitCode::SUCCESS
        }
        Err(stop) => finish(stop, &ctx.caps),
    }
}

fn key(
    ctx: &Context<'_>,
    terminal: bool,
    name: Option<String>,
    provider: Option<SubscriptionProvider>,
) -> ExitCode {
    if !terminal {
        return finish(Stop::Failed(KEY_SCRIPT.to_owned()), &ctx.caps);
    }
    crate::manage::add_signing_in(ctx, name, provider.map(super::session::provider_kind))
}

pub(crate) fn help(width: usize, glyphs: &Glyphs) -> String {
    let wide = width >= 60;
    let mut out = String::new();
    out.push_str(&wrapped(
        "Sign in to Claude, ChatGPT, Antigravity or an API key.",
        width,
        "",
        glyphs,
    ));
    out.push_str("\n\nUsage: wisp login WHAT\n\nWHAT is one of:\n");
    let key_words = format!("an API key: {}", SubscriptionProvider::API_KEY_WORDS);
    let whats = [
        ("claude", "a Claude Pro or Max subscription"),
        ("chatgpt", "a ChatGPT Plus or Pro subscription"),
        ("antigravity", "an Antigravity Gemini subscription"),
        ("key", key_words.as_str()),
        (
            "NAME",
            "one of your accounts, to sign in to it again; a new name adds an account with that name",
        ),
    ];
    let longest = whats.iter().map(|(word, _)| word.len()).max().unwrap_or(0);
    let column = 2 + longest + if wide { 4 } else { 2 };
    for (word, words) in whats {
        let lead = format!("  {word}{}", " ".repeat(column - 2 - word.len()));
        out.push_str(&wrapped(words, width, &lead, glyphs));
        out.push('\n');
    }
    out.push_str("Leave it out and wisp asks.\n\nFor scripts:\n");
    let options = [
        (
            "--name NAME",
            "the new account's name; skips the name question",
        ),
        ("-h, --help", "show this help"),
    ];
    for (flag, words) in options {
        if wide {
            let lead = format!("  {flag}{}", " ".repeat(16 - flag.len()));
            out.push_str(&wrapped(words, width, &lead, glyphs));
        } else {
            out.push_str(&format!("  {flag}\n"));
            out.push_str(&wrapped(words, width, "      ", glyphs));
        }
        out.push('\n');
    }
    out
}

pub(crate) fn status_help(width: usize, glyphs: &Glyphs) -> String {
    let mut out = String::new();
    out.push_str("Show your accounts and what needs you.\n\nUsage: wisp status NAME\n\n");
    out.push_str(&wrapped(
        "NAME is one of your accounts; leave it out to see all of them.",
        width,
        "",
        glyphs,
    ));
    out.push_str("\nwisp login status does the same.\n\n");
    if width >= 60 {
        out.push_str("  -h, --help      show this help\n");
    } else {
        out.push_str("  -h, --help\n      show this help\n");
    }
    out
}

#[cfg(test)]
mod tests;
