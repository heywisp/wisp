use wisp_protocol::CodeProblem;
use wisp_protocol::settings::{PROVIDER_GEMINI, PROVIDER_OPENAI, joined};
use wisp_protocol::subscription_auth::{
    AuthFailureKind, PresetGone, Stayed, SubscriptionProvider, Unusable, WayOrigin,
};

use super::settings::{Rebuild, Scope};
use super::{Credential, Endpoint, one_line};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{
    Long, WidthMode, fit_end, spans_width, width as width_of, wrap, wrap_keeping, wrap_plain,
    wrap_words,
};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy::{ENTER_KEY, KEEP, NOTHING_CACHED_SHORT, listed};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccountKind {
    ClaudeSubscription,
    ChatGpt,
    Antigravity,
    ClaudeApi,
    OpenAiApi,
    GeminiApi,
    Gateway,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccountState {
    Subscription,
    Api,
    Gone,
    Expired,
}

impl From<SubscriptionProvider> for AccountKind {
    fn from(plan: SubscriptionProvider) -> Self {
        match plan {
            SubscriptionProvider::ChatGpt => Self::ChatGpt,
            SubscriptionProvider::Claude => Self::ClaudeSubscription,
            SubscriptionProvider::Antigravity => Self::Antigravity,
        }
    }
}

impl AccountKind {
    pub const ALL: [Self; 7] = [
        Self::ClaudeSubscription,
        Self::ChatGpt,
        Self::Antigravity,
        Self::ClaudeApi,
        Self::OpenAiApi,
        Self::GeminiApi,
        Self::Gateway,
    ];

    pub fn of(credential: Credential, reach: Endpoint, provider: &str) -> Self {
        match (credential.plan(), reach) {
            (Some(plan), _) => plan.into(),
            (None, Endpoint::Custom) => Self::Gateway,
            (None, Endpoint::Official) if provider.eq_ignore_ascii_case(PROVIDER_OPENAI) => {
                Self::OpenAiApi
            }
            (None, Endpoint::Official) if provider.eq_ignore_ascii_case(PROVIDER_GEMINI) => {
                Self::GeminiApi
            }
            (None, Endpoint::Official) => Self::ClaudeApi,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeSubscription => "Claude subscription",
            Self::ChatGpt => "ChatGPT subscription",
            Self::Antigravity => "Antigravity subscription",
            Self::ClaudeApi => "Claude API",
            Self::OpenAiApi => "OpenAI API",
            Self::GeminiApi => "Gemini API",
            Self::Gateway => "Gateway",
        }
    }

    pub fn names_billing(self) -> bool {
        self != Self::Gateway
    }

    pub fn provider_word(self) -> &'static str {
        match self {
            Self::ClaudeSubscription | Self::ClaudeApi | Self::Gateway => "Claude",
            Self::ChatGpt => "ChatGPT",
            Self::Antigravity => "Antigravity",
            Self::OpenAiApi => "OpenAI",
            Self::GeminiApi => "Gemini",
        }
    }

    pub fn billed(provider: &str, subscription: bool) -> Self {
        if provider.eq_ignore_ascii_case(PROVIDER_OPENAI) {
            if subscription {
                Self::ChatGpt
            } else {
                Self::OpenAiApi
            }
        } else if provider.eq_ignore_ascii_case(PROVIDER_GEMINI) {
            if subscription {
                Self::Antigravity
            } else {
                Self::GeminiApi
            }
        } else if subscription {
            Self::ClaudeSubscription
        } else {
            Self::ClaudeApi
        }
    }

    pub fn billing(self) -> AccountState {
        match self {
            Self::ClaudeSubscription | Self::ChatGpt | Self::Antigravity => {
                AccountState::Subscription
            }
            Self::ClaudeApi | Self::OpenAiApi | Self::GeminiApi | Self::Gateway => {
                AccountState::Api
            }
        }
    }
}

impl AccountState {
    pub fn of(billing: wisp_protocol::Billing) -> Self {
        if billing.is_metered() {
            Self::Api
        } else {
            Self::Subscription
        }
    }

    pub fn is_subscription(self) -> bool {
        matches!(self, Self::Subscription | Self::Expired)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountBilling {
    pub name: String,
    pub state: AccountState,
    pub mark: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub name: String,
    pub billed: AccountState,
    pub mark: Option<u8>,
}

pub const MARK_WIDTH: usize = 2;

const GLUE: char = '\u{a0}';

fn mark_token(mark: Option<u8>) -> Option<Token> {
    mark.and_then(|n| Token::MARKS.get(usize::from(n.checked_sub(1)?)).copied())
}

fn glyph(mark: Option<u8>, glyphs: &Glyphs) -> (&'static str, Token) {
    match mark_token(mark) {
        Some(token) => (glyphs.mark, token),
        None => (glyphs.hollow, Token::MarkGone),
    }
}

pub fn mark(mark: Option<u8>, glyphs: &Glyphs) -> StyledSpan {
    let (glyph, token) = glyph(mark, glyphs);
    StyledSpan::new(format!("{glyph} "), Look::fg(token).glued())
}

pub fn marked(name: &str, account: Option<u8>, glyphs: &Glyphs) -> String {
    format!("{}{GLUE}{name}", glyph(account, glyphs).0)
}

pub fn mark_line(
    text: &str,
    tone: impl Into<Look>,
    account: Option<u8>,
    glyphs: &Glyphs,
) -> StyledLine {
    let tone: Look = tone.into();
    let starts = |at: usize| at == 0 || text[..at].ends_with([' ', GLUE]);
    let mut spans = Vec::new();
    let mut from = 0;
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let found = [glyph(account, glyphs), glyph(None, glyphs)]
            .into_iter()
            .find(|(glyph, _)| {
                rest.strip_prefix(glyph)
                    .is_some_and(|after| after.is_empty() || after.starts_with(GLUE))
                    && starts(at)
            });
        let Some((glyph, token)) = found else {
            at += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        if at > from {
            spans.push(StyledSpan::new(&text[from..at], tone));
        }
        let glued = rest.len() > glyph.len();
        spans.push(StyledSpan::new(
            if glued {
                format!("{glyph} ")
            } else {
                glyph.to_owned()
            },
            Look::fg(token).glued(),
        ));
        at += glyph.len() + if glued { GLUE.len_utf8() } else { 0 };
        from = at;
    }
    if from < text.len() {
        spans.push(StyledSpan::new(&text[from..], tone));
    }
    StyledLine::new(spans)
}

pub fn mark_spans(mut line: StyledLine, account: Option<u8>, glyphs: &Glyphs) -> StyledLine {
    let spans = std::mem::take(&mut line.spans).into_iter();
    let marked = spans.flat_map(|span| mark_line(&span.text, span.look, account, glyphs).spans);
    line.spans = marked.collect();
    line
}

pub fn mark_gap() -> StyledSpan {
    StyledSpan::new(" ".repeat(MARK_WIDTH), Look::fg(Token::MarkGone).glued())
}

pub fn is_mark(span: &StyledSpan) -> bool {
    span.look.fg.is_some_and(|ink| ink.still().is_mark())
}

pub fn unmarked(line: &StyledLine) -> String {
    let kept = line.spans.iter().filter(|span| !is_mark(span));
    kept.map(|span| span.text.as_str()).collect()
}

pub fn account_name(name: &str) -> String {
    one_line(name).trim().to_owned()
}

impl Chip {
    pub fn new(name: &str, billed: AccountState, mark: Option<u8>) -> Chip {
        Chip {
            name: account_name(name),
            billed,
            mark,
        }
    }

    pub fn find(name: Option<&str>, accounts: &[AccountBilling]) -> Option<Chip> {
        let name = name?;
        let found = accounts.iter().find(|a| a.name == name);
        let billed = match found {
            Some(account) => account.state,
            None if accounts.is_empty() => return None,
            None => AccountState::Gone,
        };
        Some(Chip::new(name, billed, found.and_then(|a| a.mark)))
    }

    fn shown_mark(&self) -> Option<u8> {
        self.mark.filter(|_| self.billed != AccountState::Gone)
    }

    pub fn mark_span(&self, glyphs: &Glyphs) -> StyledSpan {
        mark(self.shown_mark(), glyphs)
    }

    pub fn text(&self, glyphs: &Glyphs) -> String {
        chip(
            &marked(&self.name, self.shown_mark(), glyphs),
            self.billed,
            glyphs,
        )
    }

    pub fn width(&self, cap: usize, glyphs: &Glyphs, mode: WidthMode) -> usize {
        MARK_WIDTH
            + width_of(&self.name, mode).min(cap)
            + width_of(&format!(" {} ", glyphs.separator), mode)
            + width_of(billing_word(self.billed), mode)
    }

    pub fn spans(&self, room: usize, glyphs: &Glyphs, mode: WidthMode) -> Vec<StyledSpan> {
        let sep = format!(" {} ", glyphs.separator);
        let word = billing_word(self.billed);
        let tail = MARK_WIDTH + width_of(&sep, mode) + width_of(word, mode);
        let tone = billing_token(self.billed);
        let name_room = room.saturating_sub(tail).max(width_of(glyphs.more, mode));
        vec![
            self.mark_span(glyphs),
            StyledSpan::new(fit_end(&self.name, name_room, glyphs.more, mode), tone),
            StyledSpan::new(sep, Token::TextFaint),
            StyledSpan::new(word, tone),
        ]
    }
}

pub fn kind_words(kind: AccountKind, glyphs: &Glyphs) -> String {
    if kind.names_billing() {
        kind.label().to_owned()
    } else {
        format!(
            "{} {} {}",
            kind.label(),
            glyphs.separator,
            billing_word(kind.billing())
        )
    }
}

pub fn billing_word(state: AccountState) -> &'static str {
    match state {
        AccountState::Subscription => wisp_protocol::SUBSCRIPTION_WORD,
        AccountState::Api => "API",
        AccountState::Gone => "gone",
        AccountState::Expired => "sign-in expired",
    }
}

pub fn billing_token(state: AccountState) -> Token {
    match state {
        AccountState::Subscription => Token::Ok,
        AccountState::Api => Token::TextMuted,
        AccountState::Gone | AccountState::Expired => Token::Error,
    }
}

pub fn title(kind: AccountKind, name: &str, glyphs: &Glyphs) -> String {
    format!("{} {} {name}", kind.label(), glyphs.separator)
}

pub const SIGNED_IN: &str = "Signed in";

pub fn signed_in(
    again: bool,
    kind: AccountKind,
    name: &str,
    account: Option<u8>,
    glyphs: &Glyphs,
) -> String {
    signed_in_lines(again, kind, name, account, glyphs, usize::MAX).concat()
}

pub fn signed_in_lines(
    again: bool,
    kind: AccountKind,
    name: &str,
    account: Option<u8>,
    glyphs: &Glyphs,
    width: usize,
) -> Vec<String> {
    let words = if again { "Signed in again" } else { SIGNED_IN };
    let named = title(kind, &marked(name, account, glyphs), glyphs);
    fit_parts(
        &[words.to_owned()],
        &[(named.clone(), named)],
        "",
        glyphs,
        width,
    )
}

pub fn chip(name: &str, state: AccountState, glyphs: &Glyphs) -> String {
    format!("{name} {} {}", glyphs.separator, billing_word(state))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Switched,
    Switching,
    Started,
    Continuing,
}

impl Move {
    fn words(self) -> &'static str {
        match self {
            Self::Switched => "Switched to",
            Self::Switching => "Switching to",
            Self::Started => "Started on",
            Self::Continuing => "Continuing on",
        }
    }
}

pub const FROM_NEXT_MESSAGE: &str = "From your next message.";
pub const ACCOUNT_TITLE: &str = "Account";
pub const THIS_CHAT_NOW: &str = "This chat uses it now";
pub const ONLY_THE_MODEL: &str = "Only the model changes";
pub const ADDS_AN_ACCOUNT: &str = "Adds a Claude, ChatGPT, Antigravity, API or gateway account";
pub const SIGNS_IN_THEN_SWITCHES: &str = "enter sign in again, then switch";
pub const SWITCH_WORD: &str = "switch";
pub const ACCOUNTS_WORD: &str = "accounts";
pub const MODELS_WORD: &str = "models";
pub const NOW: &str = "now";
pub const REBUILD_TITLE: &str = crate::transcript::copy::CACHE_REBUILD;
pub const NONE: &str = "none";

pub fn sign_in_row(glyphs: &Glyphs) -> String {
    format!("+ Sign in to another account{}", glyphs.more)
}

pub fn moves_to(name: &str, from: AccountState, to: AccountState) -> String {
    match billing_move(from, to) {
        Some(words) => format!("Moves this chat to {name}, {words}"),
        None => format!("Moves this chat to {name}"),
    }
}

pub const COVERED: &str = "covered by your subscription";
pub const START_WORD: &str = "start";
pub const STAY_WORD: &str = "stay";
pub const CHOOSE_A_MODEL: &str = "choose a model";

pub fn starts_on(name: &str, state: AccountState) -> Vec<(String, String)> {
    let billed = if state == AccountState::Api {
        ", on the API"
    } else {
        ""
    };
    let mut parts = vec![(
        format!("Starts this chat on {name}{billed}"),
        format!("Starts on {name}{billed}"),
    )];
    if state.is_subscription() {
        parts.push((COVERED.to_owned(), COVERED.to_owned()));
    }
    parts
}

pub fn stay_on(name: &str) -> String {
    format!("{STAY_WORD} on {name}")
}

pub fn doesnt_offer(model: &str) -> String {
    format!("doesn't offer {model}")
}

pub fn none_offers(model: &str) -> String {
    format!("No account offers {model}")
}

pub fn no_carry(name: &str) -> String {
    format!("On {name}, earlier reasoning won't carry over.")
}

pub use wisp_protocol::opening::{Said, Who};

enum Piece<'a> {
    Name(&'a Who),
    Text(String),
}

fn missing(from: Option<&String>) -> Piece<'static> {
    Piece::Text(match from {
        Some(file) => format!("'s key is missing from {file}"),
        None => "'s key is missing".to_owned(),
    })
}

fn pieces(said: &Said) -> Vec<Piece<'_>> {
    use Piece::{Name, Text};
    match said {
        Said::SignInExpired(name) => vec![Name(name), Text("'s sign-in expired".to_owned())],
        Said::SignedOut(name) => vec![Name(name), Text(" is signed out".to_owned())],
        Said::CantBeRead(name) => vec![Name(name), Text(format!(" {CANT_BE_READ}"))],
        Said::KeyMissing(name, from) => vec![Name(name), missing(from.as_ref())],
        Said::RanOn(name, why) => vec![
            Text("It ran on ".to_owned()),
            Name(name),
            Text(
                why.as_ref()
                    .map_or(".".to_owned(), |why| format!(", which {why}.")),
            ),
        ],
        Said::Is(name, why) => vec![Name(name), Text(format!(" {why}."))],
        Said::Lacks(name, model) => vec![Name(name), Text(format!(" {}", doesnt_offer(model)))],
        Said::State(name, words) => vec![Name(name), Text(format!(": {words}."))],
        Said::StartsOn(name, from, to) => vec![
            Name(name),
            missing(from.as_ref()),
            Text(", so new chats now start on ".to_owned()),
            Name(to),
            Text(".".to_owned()),
        ],
        Said::Text(text) => vec![Text(text.clone())],
    }
}

fn join_said(said: &Said, name: impl Fn(&str, Option<u8>) -> String) -> String {
    let piece = |piece: Piece<'_>| match piece {
        Piece::Name(Who::Account(text, mark)) => name(text, *mark),
        Piece::Name(Who::Words(text)) => text.clone(),
        Piece::Text(text) => text,
    };
    pieces(said).into_iter().map(piece).collect()
}

pub trait SaidWords {
    fn plain(&self) -> String;
    fn marked(&self, glyphs: &Glyphs) -> String;
    fn mark(&self) -> Option<u8>;
    fn line(&self, tone: Token, glyphs: &Glyphs) -> StyledLine;
}

impl SaidWords for Said {
    fn plain(&self) -> String {
        join_said(self, |name, _| name.to_owned())
    }

    fn marked(&self, glyphs: &Glyphs) -> String {
        join_said(self, |name, account| marked(name, account, glyphs))
    }

    fn mark(&self) -> Option<u8> {
        let account = |piece: Piece<'_>| match piece {
            Piece::Name(Who::Account(_, mark)) => Some(*mark),
            _ => None,
        };
        pieces(self).into_iter().find_map(account).flatten()
    }

    fn line(&self, tone: Token, glyphs: &Glyphs) -> StyledLine {
        let spans = pieces(self).into_iter().flat_map(|piece| match piece {
            Piece::Name(Who::Account(name, account)) => {
                vec![mark(*account, glyphs), StyledSpan::new(name, tone)]
            }
            Piece::Name(Who::Words(text)) => vec![StyledSpan::new(text, tone)],
            Piece::Text(text) => vec![StyledSpan::new(text, tone)],
        });
        StyledLine::new(spans.collect())
    }
}

pub fn sign_in_expired(name: &str) -> String {
    Said::SignInExpired(Who::Words(name.to_owned())).plain()
}

pub use wisp_protocol::subscription_auth::CANT_BE_READ;

pub const SIGN_IN_AGAIN: &str = "Sign in again";

pub fn sign_in_again_to(name: &str) -> String {
    format!("{SIGN_IN_AGAIN} to {name}")
}

pub const SIGN_IN_AGAIN_WITH: &str = "Sign in again with";

pub fn key_refused(provider: &str, chip: &str) -> String {
    format!("{provider} refused the key for {chip}")
}

pub fn nothing_cached(agents: &[String]) -> String {
    let has = if agents.len() == 1 { "has" } else { "have" };
    format!("{} {has} {NOTHING_CACHED_SHORT}", listed(agents))
}

pub fn billing_move(from: AccountState, to: AccountState) -> Option<&'static str> {
    match (from, to) {
        (from, AccountState::Api) if from.is_subscription() => {
            Some("from your subscription to the API")
        }
        (AccountState::Api, to) if to.is_subscription() => {
            Some("from the API to your subscription")
        }
        _ => None,
    }
}

pub fn quoted(usd: f64, state: AccountState) -> Rebuild {
    if state.is_subscription() {
        Rebuild::PlanUsd(usd, Scope::Exact)
    } else {
        Rebuild::Usd(usd)
    }
}

pub fn moved_from(from: AccountState, to: AccountState) -> Option<String> {
    billing_move(from, to).map(|words| format!("Moved {words}."))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Moved {
    pub how: Move,
    pub name: String,
    pub mark: Option<u8>,
    pub state: AccountState,
    pub from: Option<AccountState>,
    pub model: String,
    pub effort: Option<String>,
    pub rebuild: Option<Rebuild>,
}

pub fn fit_parts(
    first: &[String],
    parts: &[(String, String)],
    hang: &str,
    glyphs: &Glyphs,
    width: usize,
) -> Vec<String> {
    let mode = WidthMode::from(glyphs.set);
    let sep = format!(" {} ", glyphs.separator);
    let fits = |text: &str| width_of(text, mode) <= width;
    if first.is_empty()
        && let Some(((full, tight), rest)) = parts.split_first()
    {
        let lead = if fits(full) { full } else { tight };
        return fit_parts(std::slice::from_ref(lead), rest, hang, glyphs, width);
    }
    let lead = first.join(" ");
    let mut whole = vec![lead.clone()];
    whole.extend(parts.iter().map(|(full, _)| full.clone()));
    let one = whole.join(&sep);
    if fits(&one) {
        return vec![one];
    }
    let mut lines: Vec<String> = if fits(&lead) {
        vec![lead]
    } else {
        first
            .iter()
            .enumerate()
            .flat_map(|(i, part)| {
                let start = if i == 0 { "" } else { hang };
                wrap_plain(part, width, start, hang, glyphs)
            })
            .collect()
    };
    for (full, tight) in parts {
        let piece = if fits(&format!("{hang}{full}")) {
            full
        } else {
            tight
        };
        match lines.last_mut() {
            Some(last) if fits(&format!("{last}{sep}{piece}")) => {
                last.push_str(&sep);
                last.push_str(piece);
            }
            _ => lines.extend(wrap_plain(piece, width, hang, hang, glyphs)),
        }
    }
    lines
}

fn moved_parts(
    moved: &Moved,
    glyphs: &Glyphs,
    room: usize,
) -> ([String; 2], Vec<(String, String)>) {
    let model = match &moved.effort {
        Some(effort) => format!("{} {effort}", moved.model),
        None => moved.model.clone(),
    };
    let named = Chip::new(&moved.name, moved.state, moved.mark);
    let mode = WidthMode::from(glyphs.set);
    let rest: String = named
        .spans(room, glyphs, mode)
        .into_iter()
        .skip(1)
        .map(|span| span.text)
        .collect();
    let shown = marked(&rest, named.shown_mark(), glyphs);
    let first = [moved.how.words().to_owned(), shown];
    let mut parts = vec![(model.clone(), model)];
    parts.extend(moved.rebuild.as_ref().map(|r| r.parts(glyphs.set, true)));
    (first, parts)
}

fn moved_after(moved: &Moved) -> Vec<String> {
    let mut after: Vec<String> = moved
        .from
        .and_then(|from| moved_from(from, moved.state))
        .into_iter()
        .collect();
    if moved.how == Move::Switching {
        after.push(FROM_NEXT_MESSAGE.to_owned());
    }
    after
}

pub fn moved_body(moved: &Moved, glyphs: &Glyphs, width: usize) -> Vec<String> {
    let (first, parts) = moved_parts(moved, glyphs, width);
    let mut lines = fit_parts(&first, &parts, "", glyphs, width);
    for line in moved_after(moved) {
        lines.extend(wrap_plain(&line, width, "", "", glyphs));
    }
    lines
}

pub fn moved_lines(moved: &Moved, glyphs: &Glyphs, width: usize) -> Vec<String> {
    let (first, parts) = moved_parts(moved, glyphs, width.saturating_sub(2));
    let lead = [format!("{} {}", glyphs.bullet, first[0]), first[1].clone()];
    let mut lines = fit_parts(&lead, &parts, "  ", glyphs, width);
    for line in moved_after(moved) {
        lines.extend(wrap_plain(&line, width, "  ", "  ", glyphs));
    }
    lines
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mistake {
    Pasted(CodeProblem),
    Earlier,
    Refused,
    Unreachable,
    TimedOut,
}

pub fn mistake_line(mistake: Mistake, provider: &str) -> String {
    let callback_url = provider == "Antigravity";
    match mistake {
        Mistake::Pasted(CodeProblem::Empty) if callback_url => {
            "Paste the callback URL from your browser's address bar first.".to_owned()
        }
        Mistake::Pasted(CodeProblem::Empty) => {
            format!("Paste the code from {provider}'s page first.")
        }
        Mistake::Pasted(CodeProblem::NoState) if callback_url => {
            "That's only part of the callback URL: copy the full address from your browser's address bar."
                .to_owned()
        }
        Mistake::Pasted(CodeProblem::NoState) => {
            format!("That's only part of the code: use the Copy button on {provider}'s page.")
        }
        Mistake::Pasted(CodeProblem::Repeated) if callback_url => {
            "That's the callback URL twice. Paste again; the new paste replaces this one."
                .to_owned()
        }
        Mistake::Pasted(CodeProblem::Repeated) => {
            "That's the code twice. Paste again; the new paste replaces this one.".to_owned()
        }
        Mistake::Pasted(CodeProblem::Spaced) if callback_url => {
            "That callback URL has spaces or line breaks in it. Paste it again.".to_owned()
        }
        Mistake::Pasted(CodeProblem::Spaced) => {
            "That code has spaces or line breaks in it. Paste it again.".to_owned()
        }
        Mistake::Pasted(CodeProblem::TooLong) if callback_url => {
            "That's longer than any callback URL. Paste it again.".to_owned()
        }
        Mistake::Pasted(CodeProblem::TooLong) => {
            "That's longer than any code. Paste it again.".to_owned()
        }
        Mistake::Earlier if callback_url => {
            "That callback URL is from an earlier sign-in: paste the one from the newest page."
                .to_owned()
        }
        Mistake::Earlier => {
            "That code is from an earlier sign-in: paste the one from the newest page.".to_owned()
        }
        Mistake::Refused if callback_url => {
            format!(
                "{provider} didn't accept that callback URL; each works once. {ENTER_KEY} opens a new page."
            )
        }
        Mistake::Refused => {
            format!(
                "{provider} didn't accept that code; each works once. {ENTER_KEY} opens a new page."
            )
        }
        Mistake::Unreachable => format!("Couldn't reach {provider}. {ENTER_KEY} tries again."),
        Mistake::TimedOut => format!("This sign-in timed out. {ENTER_KEY} opens a new page."),
    }
}

pub fn mistake_of(kind: AuthFailureKind, problem: Option<CodeProblem>) -> Mistake {
    match (kind, problem) {
        (AuthFailureKind::CodeRejected, Some(problem)) => Mistake::Pasted(problem),
        (AuthFailureKind::CodeRejected, None) => Mistake::Earlier,
        (AuthFailureKind::Unreachable, _) => Mistake::Unreachable,
        (AuthFailureKind::Expired | AuthFailureKind::StaleRequest, _) => Mistake::TimedOut,
        _ => Mistake::Refused,
    }
}

pub fn switch_first(provider: &str) -> String {
    format!("If the page shows your other {provider} account, switch accounts there first.")
}

pub const TRIED_BROWSER: &str = "tried your browser";
pub const NO_BROWSER: &str = "no browser here";
pub const LINK_COPIED: &str = "link copied";
pub const LINK_COPIED_TO_TMUX: &str = "link copied to tmux";
pub const LINK_SENT: &str = "link sent to your terminal";
pub const LINK_NOT_COPIED: &str = "couldn't copy the link";
pub const LINK_WORDS: [&str; 4] = [LINK_COPIED, LINK_COPIED_TO_TMUX, LINK_SENT, LINK_NOT_COPIED];

pub fn browser_words(opened: bool) -> &'static str {
    if opened { TRIED_BROWSER } else { NO_BROWSER }
}

pub fn chip_whole(name: &str, state: AccountState, glyphs: &Glyphs) -> String {
    whole(&chip(name, state, glyphs))
}

fn whole(text: &str) -> String {
    text.replace(' ', "\u{a0}")
}

pub const NOT_RESUMED: &str = "Not resumed";

pub fn not_resumed_line(stayed: &Stayed, command: &str) -> Option<String> {
    let from = stayed.from.as_ref()?;
    let fix = if from.sign_in {
        format!("; run {command}")
    } else {
        String::new()
    };
    Some(format!(
        "{} not resumed: {}: {}{fix}.",
        stayed.agent, from.account, from.state
    ))
}

pub type TableRow = (String, Vec<StyledSpan>, Vec<StyledSpan>);

pub fn row_table(
    rows: &[TableRow],
    room: usize,
    right_edge: Option<usize>,
    glyphs: &Glyphs,
) -> Vec<StyledLine> {
    let mode = WidthMode::from(glyphs.set);
    let pad = |n: usize| StyledSpan::plain(" ".repeat(n));
    let names = rows.iter().map(|(name, ..)| width_of(name, mode)).max();
    let column = names.unwrap_or_default() + 5;
    let inlines: Vec<Vec<StyledSpan>> = rows
        .iter()
        .map(|(_, value, fix)| {
            let mut inline = value.clone();
            if !fix.is_empty() {
                let sep = format!(" {} ", glyphs.separator);
                inline.push(StyledSpan::new(sep, Token::TextFaint));
                inline.extend(fix.iter().cloned());
            }
            inline
        })
        .collect();
    let widest = inlines.iter().map(|inline| spans_width(inline, mode)).max();
    let edge = right_edge.map(|at_least| (column + widest.unwrap_or_default()).max(at_least));
    let slot = room.saturating_sub(4);
    let unglued = |part: &[StyledSpan]| -> Vec<StyledSpan> {
        let fits = |span: &StyledSpan| span.text.split(' ').all(|w| width_of(w, mode) <= slot);
        let open = |span: &StyledSpan| StyledSpan::new(span.text.replace('\u{a0}', " "), span.look);
        part.iter()
            .map(|span| if fits(span) { span.clone() } else { open(span) })
            .collect()
    };
    let mut lines = Vec::new();
    for ((name, value, fix), inline) in rows.iter().zip(inlines) {
        let wide = spans_width(&inline, mode);
        let at = edge.map_or(column, |edge| edge.saturating_sub(wide).max(column));
        let named = vec![pad(2), StyledSpan::new(name.clone(), Token::Text)];
        if at + wide <= room {
            let gap = pad(at.saturating_sub(2 + width_of(name, mode)));
            lines.push(StyledLine::new([named, vec![gap], inline].concat()));
            continue;
        }
        lines.push(StyledLine::new(named));
        let parts = if wide <= slot {
            vec![inline]
        } else {
            vec![value.clone(), fix.clone()]
        };
        for part in parts.iter().filter(|part| !part.is_empty()) {
            for row in wrap(&unglued(part), slot, mode) {
                lines.push(StyledLine::new([vec![pad(4)], row.spans].concat()));
            }
        }
    }
    lines
}

fn not_resumed_rows(
    rows: &[(String, Unusable, Option<u8>, Option<String>)],
    width: usize,
    glyphs: &Glyphs,
) -> Vec<StyledLine> {
    let table: Vec<TableRow> = rows
        .iter()
        .map(|(agent, from, account, fix)| {
            let value = vec![
                mark(*account, glyphs),
                StyledSpan::new(format!("{}: ", whole(&from.account)), Token::Text),
                StyledSpan::new(whole(&from.state), Token::Error),
            ];
            let fix = fix
                .iter()
                .map(|fix| StyledSpan::new(whole(fix), Token::Text));
            (agent.clone(), value, fix.collect())
        })
        .collect();
    let heading = StyledLine::new(vec![StyledSpan::new(NOT_RESUMED, Token::Text)]);
    [vec![heading], row_table(&table, width, None, glyphs)].concat()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Change,
    SignOut,
}

pub fn variable_line(name: &str, var: &str, keys_file: Option<&str>, ending: Ending) -> String {
    match (keys_file, ending) {
        (Some(file), Ending::Change) => format!("Change {var} in {file}."),
        (Some(file), Ending::SignOut) => {
            format!("{name}'s key comes from {file}: remove {var} there to sign out of {name}.")
        }
        (None, Ending::Change) => format!("Change {var} in your shell."),
        (None, Ending::SignOut) => format!(
            "{name}'s key comes from your shell: remove {var} from your shell profile to sign out of {name}."
        ),
    }
}

pub const SIGNED_OUT: &str = "Signed out";
pub const KEY_DELETED: &str = "Its key is deleted from Wisp.";
pub const WHERE_SHOULD_IT_GO: &str = "This chat uses it. Where should it go?";
pub const WHERE_TITLE: &str = "Where should this chat go?";
pub const WHERE_START: &str = "Where should this chat start?";
pub const WHERE_CONTINUE: &str = "Where should this chat continue?";
pub const RAN_ON_IT: &str = "This chat ran on it.";

pub const WHICH_TITLE: &str = "Sign out of which account?";
pub const NOTHING_SIGNED_OUT: &str = "Nothing was signed out.";
pub const SIGN_OUT_OPTIONS: [&str; 2] = ["Sign out", KEEP];
pub const NO_ACCOUNT: &str = "No account is set up.";
pub const STARTS_HERE: &str = "new chats start here";
pub const API_KEY: &str = "API key";
pub const THIS_CHAT: &str = "this chat";

pub fn row_notes(
    chat: bool,
    start: bool,
    keys_file: Option<&str>,
    users: &[String],
) -> Vec<String> {
    [
        chat.then(|| THIS_CHAT.to_owned()),
        start.then(|| STARTS_HERE.to_owned()),
        keys_file.map(|file| format!("from {file}")),
        (!users.is_empty()).then(|| format!("{} uses it", listed(users))),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub fn no_account(name: &str, yours: &[String]) -> String {
    if yours.is_empty() {
        NO_ACCOUNT.to_owned()
    } else {
        format!(
            "there's no account called {name}; yours are {}.",
            listed(yours)
        )
    }
}

pub type Named = (String, Option<u8>);

pub fn named_spans(names: &[Named], tone: impl Into<Look>, glyphs: &Glyphs) -> Vec<StyledSpan> {
    let tone: Look = tone.into();
    let items = names
        .iter()
        .map(|(name, account)| vec![mark(*account, glyphs), StyledSpan::new(name, tone)])
        .collect();
    joined(items, "and", |word| vec![StyledSpan::new(word, tone)]).concat()
}

pub fn sign_out_title(names: &[Named], room: usize, glyphs: &Glyphs) -> (Vec<StyledSpan>, bool) {
    let tone = Look::fg(Token::Text).bold();
    let mut named = vec![StyledSpan::new("Sign out of ", tone)];
    named.extend(named_spans(names, tone, glyphs));
    named.push(StyledSpan::new("?", tone));
    if names.len() < 2 || spans_width(&named, WidthMode::from(glyphs.set)) <= room {
        (named, false)
    } else {
        let counted = format!("Sign out of {} accounts?", names.len());
        (vec![StyledSpan::new(counted, tone)], true)
    }
}

pub fn signed_out_lines(
    gone: &[(AccountKind, String)],
    surface: Surface,
    glyphs: &Glyphs,
    width: usize,
) -> Vec<String> {
    let mode = WidthMode::from(glyphs.set);
    if let [(kind, name)] = gone {
        let kind_w = width_of(&format!("{} {} ", kind.label(), glyphs.separator), mode);
        let name_room = width
            .saturating_sub(kind_w + MARK_WIDTH)
            .max(width_of(glyphs.more, mode) + 1);
        let tight = fit_end(name, name_room, glyphs.more, mode);
        let tight = title(*kind, &marked(&tight, None, glyphs), glyphs);
        let parts = [(title(*kind, &marked(name, None, glyphs), glyphs), tight)];
        return fit_parts(&[SIGNED_OUT.to_owned()], &parts, "", glyphs, width);
    }
    let names: Vec<String> = gone
        .iter()
        .map(|(_, name)| marked(name, None, glyphs))
        .collect();
    let end = if surface == Surface::Terminal {
        "."
    } else {
        ""
    };
    let line = format!("{SIGNED_OUT} of {}{end}", listed(&names));
    wrap_plain(&line, width, "", "", glyphs)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Now,
    Too,
}

pub fn start_line(
    name: &str,
    account: Option<u8>,
    state: AccountState,
    start: Start,
    glyphs: &Glyphs,
) -> String {
    let name = marked(name, account, glyphs);
    let named = || match state {
        AccountState::Subscription => name.clone(),
        _ => chip_whole(&name, state, glyphs),
    };
    match start {
        Start::Now => format!("New chats now start on {}.", named()),
        Start::Too => format!("New chats start on {name} too."),
    }
}

pub fn key_left(name: &str) -> String {
    format!("{name}'s key may still be saved: run wisp account remove {name} to delete it.")
}

pub fn stays_on(
    agent: &str,
    to: &str,
    account: &str,
    charges: bool,
    glyphs: &Glyphs,
) -> [String; 2] {
    let first = if charges {
        format!("{agent} didn't move to the API:")
    } else {
        format!("{agent} {} yet:", cant_move(to))
    };
    [
        first,
        format!(
            "it stays on {} and can't run.",
            chip_whole(&marked(account, None, glyphs), AccountState::Gone, glyphs)
        ),
    ]
}

pub fn working_moves(agent: &str, name: &str, state: AccountState, glyphs: &Glyphs) -> [String; 2] {
    [
        format!("{agent} is working:"),
        format!(
            "it stops after this step, then moves to {}.",
            chip_whole(name, state, glyphs)
        ),
    ]
}

pub const MESSAGE_NOT_SENT: &str = "Your message wasn't sent. Where should this chat go?";

fn were(names: &[String]) -> &'static str {
    if names.len() == 1 { "was" } else { "were" }
}

pub fn signed_out_elsewhere(names: &[String], glyphs: &Glyphs) -> String {
    let gone: Vec<String> = names
        .iter()
        .map(|name| marked(name, None, glyphs))
        .collect();
    format!(
        "{} {} signed out in another window.",
        listed(&gone),
        were(names)
    )
}

pub fn message_waits(names: &[String], glyphs: &Glyphs) -> String {
    format!(
        "{} Your message waits until this chat has an account.",
        signed_out_elsewhere(names, glyphs)
    )
}

pub fn gone_title(names: &[String], glyphs: &Glyphs) -> (String, String) {
    let mut prompt = Vec::new();
    let gone: Vec<String> = names
        .iter()
        .map(|name| marked(name, None, glyphs))
        .collect();
    let title = match gone.as_slice() {
        [first, rest @ ..] => {
            if !rest.is_empty() {
                prompt.push(format!("{} {} signed out too.", listed(rest), were(rest)));
            }
            format!("{first} was signed out in another window")
        }
        [] => String::new(),
    };
    prompt.push(MESSAGE_NOT_SENT.to_owned());
    (title, prompt.join("\n"))
}

pub fn off_title(plugin: &str, removed: bool) -> (String, String) {
    let action = if removed {
        "is being removed"
    } else {
        "is going off"
    };
    (WHERE_TITLE.to_owned(), format!("{plugin} {action}."))
}

pub fn cant_move(to: &str) -> String {
    format!("can't move to {to}")
}

fn left_behind(agents: &[String]) -> String {
    let verb = if agents.len() == 1 { "stays" } else { "stay" };
    format!("{} {verb} behind", listed(agents))
}

fn movers(follow: &[String]) -> String {
    let mut all = vec!["This chat".to_owned()];
    all.extend(follow.iter().cloned());
    let verb = if follow.is_empty() { "moves" } else { "move" };
    format!("{} {verb}", listed(&all))
}

pub fn consequence(
    follow: &[String],
    stay: &[String],
    carries: bool,
    from: AccountState,
    to: AccountState,
) -> Vec<String> {
    let mut lines: Vec<String> = billing_move(from, to)
        .map(|words| format!("{} {words}.", movers(follow)))
        .into_iter()
        .collect();
    let behind = (!stay.is_empty()).then(|| left_behind(stay));
    match (carries, behind) {
        (false, None) => lines.push("Earlier reasoning won't carry over.".to_owned()),
        (false, Some(behind)) => {
            lines.push(format!("Earlier reasoning won't carry over, and {behind}."));
        }
        (true, Some(behind)) if lines.is_empty() => lines.push(format!("{behind}.")),
        (true, _) => {}
    }
    lines
}

pub fn live_consequences(
    movers: &[(&str, &WayOrigin)],
    provider: &str,
    dest: AccountState,
    fixed: bool,
) -> Vec<String> {
    let subject = |name: &str| {
        if name == "/" {
            THIS_CHAT.to_owned()
        } else {
            name.to_owned()
        }
    };
    let sentence = |names: &[String], tail: Option<&str>| {
        let subject = listed(names);
        let subject = subject
            .strip_prefix(THIS_CHAT)
            .map_or_else(|| subject.clone(), |rest| format!("This chat{rest}"));
        let verb = if names.len() == 1 { "moves" } else { "move" };
        match tail {
            Some(tail) => format!("{subject} {verb} {tail}."),
            None => format!("{subject} {verb}."),
        }
    };
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    let mut unnamed_child = false;
    for &(name, origin) in movers {
        let transition = match origin.billing {
            Some(from) => billing_move(AccountState::of(from), dest),
            None => (dest == AccountState::Api).then_some("to the API"),
        };
        if let Some(transition) = transition {
            if let Some((_, names)) = groups.iter_mut().find(|(tail, _)| *tail == transition) {
                names.push(subject(name));
            } else {
                groups.push((transition, vec![subject(name)]));
            }
        } else {
            unnamed_child |= name != "/";
        }
    }
    let mut lines = Vec::new();
    if !fixed && !movers.is_empty() && (groups.is_empty() || unnamed_child) {
        let names: Vec<_> = movers.iter().map(|(name, _)| subject(name)).collect();
        lines.push(sentence(&names, None));
    }
    lines.extend(
        groups
            .into_iter()
            .map(|(tail, names)| sentence(&names, Some(tail))),
    );
    let crossing: Vec<_> = movers
        .iter()
        .filter(|(_, origin)| {
            origin
                .provider
                .as_deref()
                .is_some_and(|from| from != provider)
        })
        .map(|(name, _)| subject(name))
        .collect();
    if !crossing.is_empty() {
        if crossing.len() == movers.len() {
            lines.push("Earlier reasoning won't carry over.".to_owned());
        } else {
            lines.push(format!(
                "Earlier reasoning won't carry over for {}.",
                listed(&crossing)
            ));
        }
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moving {
    pub to: Named,
    pub state: AccountState,
    pub rows: Vec<(String, Option<String>)>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asked {
    pub names: Vec<Named>,
    pub counted: bool,
    pub keys: Vec<Named>,
    pub moving: Option<Moving>,
    pub behind: Vec<(String, String)>,
    pub presets: Vec<PresetGone>,
    pub nothing_left: bool,
}

impl Asked {
    pub fn titled(&self, room: usize, glyphs: &Glyphs) -> (Vec<StyledSpan>, Asked) {
        let says_more = !self.keys.is_empty()
            || self.moving.is_some()
            || !self.behind.is_empty()
            || !self.presets.is_empty();
        let fill = if says_more { 0 } else { room };
        let (title, counted) = sign_out_title(&self.names, fill, glyphs);
        let asked = Asked {
            counted,
            ..self.clone()
        };
        (title, asked)
    }
}

pub fn preset_line(gone: &PresetGone, again_with: Option<&str>) -> String {
    let PresetGone {
        preset, account, ..
    } = gone;
    if gone.runs {
        return format!("Preset {preset} used {account}; it now runs on its parent's account.");
    }
    let line =
        format!("Preset {preset} used {account}; new agents from it won't start on the API.");
    match again_with {
        Some(command) => format!("{line} {SIGN_IN_AGAIN_WITH} {}.", whole(command)),
        None => line,
    }
}

fn gone_names(asked: &Asked, glyphs: &Glyphs) -> String {
    let named = asked.names.iter().map(|named| {
        let key = if asked.keys.contains(named) {
            " (key deleted)"
        } else {
            ""
        };
        whole(&format!("{}{key}", marked(&named.0, None, glyphs)))
    });
    named
        .collect::<Vec<_>>()
        .join(&format!(" {}\u{a0}", glyphs.separator))
}

fn right_column(asked: &Asked, room: usize, glyphs: &Glyphs) -> Vec<String> {
    let mode = WidthMode::from(glyphs.set);
    let movers = asked.moving.iter().flat_map(|m| &m.rows);
    let movers = movers.map(|(name, amount)| (name, amount.as_deref().unwrap_or(glyphs.absent)));
    let behind = asked
        .behind
        .iter()
        .map(|(name, reason)| (name, reason.as_str()));
    let rows: Vec<(&String, &str)> = movers.chain(behind).collect();
    let end = |name: &str, value: &str| 5 + width_of(name, mode) + width_of(value, mode);
    let edge = |head: &str| width_of(head, mode) + 3 + width_of(REBUILD_TITLE, mode);
    let heading = asked.moving.as_ref().map(|m| {
        let (to, mark) = &m.to;
        let to = marked(to, *mark, glyphs);
        format!("Moving to {}", chip_whole(&to, m.state, glyphs))
    });
    let shared = heading.as_deref().is_some_and(|head| edge(head) <= room);
    let ends = rows.iter().map(|(name, value)| match end(name, value) {
        fits if fits <= room => fits,
        _ => 4 + width_of(value, mode),
    });
    let head_end = heading.as_deref().filter(|_| shared).map(edge);
    let at = ends.chain(head_end).max().unwrap_or_default().min(room);
    let table = |rows: &[(&String, &str)]| -> Vec<String> {
        rows.iter()
            .flat_map(|(name, value)| {
                let row = ((*name).clone(), vec![StyledSpan::plain(*value)], Vec::new());
                row_table(&[row], room, Some(at), glyphs)
            })
            .map(|line| crate::text::spans_text(&line.spans))
            .collect()
    };
    let pad = |used: usize| " ".repeat(at.saturating_sub(used + width_of(REBUILD_TITLE, mode)));
    let mut lines = Vec::new();
    let moved = asked.moving.as_ref().map_or(0, |m| m.rows.len());
    if let Some(head) = heading {
        if shared {
            lines.push(format!(
                "{head}{}{REBUILD_TITLE}",
                pad(width_of(&head, mode))
            ));
        } else {
            lines.extend([head, format!("{}{REBUILD_TITLE}", pad(0))]);
        }
        lines.extend(table(&rows[..moved]));
    }
    if moved < rows.len() {
        lines.push(STAYING_BEHIND.to_owned());
        lines.extend(table(&rows[moved..]));
    }
    lines
}

pub const STAYING_BEHIND: &str = "Staying behind";

pub fn logout_question(asked: &Asked, room: usize, glyphs: &Glyphs) -> Vec<StyledLine> {
    let text = |words: &str| StyledSpan::new(words, Token::Text);
    let mut lines = Vec::new();
    if asked.counted {
        lines.push(mark_line(
            &gone_names(asked, glyphs),
            Token::Text,
            None,
            glyphs,
        ));
    }
    let deleted = match asked.keys.as_slice() {
        [] => None,
        _ if asked.counted => None,
        [one] if asked.names.len() == 1 && *one == asked.names[0] => Some(vec![text(KEY_DELETED)]),
        keys => {
            let mut spans = named_spans(keys, Token::Text, glyphs);
            spans.push(text(if keys.len() == 1 {
                "'s key is deleted."
            } else {
                "'s keys are deleted."
            }));
            Some(spans)
        }
    };
    lines.extend(deleted.map(StyledLine::new));
    let mark = asked.moving.as_ref().and_then(|m| m.to.1);
    let sections = right_column(asked, room, glyphs);
    if !sections.is_empty() {
        lines.push(StyledLine::default());
        lines.extend(
            sections
                .iter()
                .map(|line| mark_line(line, Token::Text, mark, glyphs)),
        );
        if let Some(moving) = &asked.moving
            && !moving.notes.is_empty()
        {
            lines.push(StyledLine::default());
            lines.extend(
                moving
                    .notes
                    .iter()
                    .map(|line| mark_line(line, Token::Text, None, glyphs)),
            );
        }
        if !asked.presets.is_empty() {
            lines.push(StyledLine::default());
        }
    }
    lines.extend(
        asked
            .presets
            .iter()
            .map(|gone| StyledLine::new(vec![text(&preset_line(gone, None))])),
    );
    lines
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lead {
    Done,
    Bullet,
    Hang,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Kept {
    pub agent: String,
    pub to: String,
    pub account: String,
    pub charges: bool,
    pub busy: Option<(String, AccountState)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SignedOut {
    pub gone: Vec<(AccountKind, String)>,
    pub left_sign_ins: usize,
    pub left_keys: Vec<String>,
    pub chat: Option<Moved>,
    pub reason: Option<Said>,
    pub start: Option<(String, AccountState, Start)>,
    pub start_mark: Option<u8>,
    pub agents: Vec<(String, Moved, Option<Said>)>,
    pub kept: Vec<Kept>,
    pub presets: Vec<(PresetGone, String)>,
    pub not_resumed: Vec<(String, Unusable, Option<u8>, Option<String>)>,
    pub again: bool,
    pub again_with: Option<String>,
    pub elsewhere: bool,
}

pub fn sign_in_again(surface: Surface) -> String {
    sign_in_again_with(login_word(surface))
}

pub fn sign_in_again_with(command: &str) -> String {
    format!("Run {command} to sign in again.")
}

pub fn signed_out_body(out: &SignedOut, glyphs: &Glyphs, width: usize) -> Vec<(Lead, StyledLine)> {
    let wrap = |text: &str| wrap_plain(text, width, "", "", glyphs);
    let led = |first: Lead, lines: Vec<String>, mark: Option<u8>| {
        lines.into_iter().enumerate().map(move |(i, line)| {
            let line = mark_line(&line, Token::TextMuted, mark, glyphs);
            (if i == 0 { first } else { Lead::Hang }, line)
        })
    };
    let said = |said: &Said| {
        let line = said.line(Token::TextMuted, glyphs);
        let mode = WidthMode::from(glyphs.set);
        let lines = wrap_keeping(&line.spans, width, mode, Long::Whole, glyphs.separator);
        lines.into_iter().map(|line| (Lead::Hang, line))
    };
    let mut rows: Vec<(Lead, StyledLine)> = Vec::new();
    if out.elsewhere {
        let names: Vec<String> = out.gone.iter().map(|(_, name)| name.clone()).collect();
        let reason = wrap(&signed_out_elsewhere(&names, glyphs)).into_iter();
        let reason = reason.filter(|_| !names.is_empty());
        if let Some(moved) = &out.chat {
            rows.extend(led(
                Lead::Bullet,
                moved_body(moved, glyphs, width),
                moved.mark,
            ));
            rows.extend(led(Lead::Hang, reason.collect(), None));
        } else {
            rows.extend(led(Lead::Bullet, reason.collect(), None));
        }
    } else {
        if !out.gone.is_empty() {
            let lines = signed_out_lines(&out.gone, Surface::App, glyphs, width);
            rows.extend(led(Lead::Done, lines, None));
        }
        let mut notes: Vec<String> = vec![CLEANUP_WARNING.to_owned(); out.left_sign_ins];
        notes.extend(out.left_keys.iter().map(|name| key_left(name)));
        rows.extend(
            notes
                .iter()
                .flat_map(|note| led(Lead::Hang, wrap(note), None)),
        );
        if let Some(moved) = &out.chat {
            rows.extend(led(
                Lead::Bullet,
                moved_body(moved, glyphs, width),
                moved.mark,
            ));
        }
    }
    rows.extend(out.reason.iter().flat_map(said));
    if let Some(command) = &out.again_with {
        let lines = fit_command(SIGN_IN_AGAIN_WITH, &format!("{command}."), width, glyphs);
        rows.extend(led(Lead::Hang, lines, None));
    }
    if let Some((name, state, start)) = &out.start {
        rows.extend(led(
            Lead::Hang,
            wrap(&start_line(name, out.start_mark, *state, *start, glyphs)),
            out.start_mark,
        ));
    }
    for (agent, moved, reason) in &out.agents {
        let (mut first, parts) = moved_parts(moved, glyphs, width);
        first[0] = format!("{agent} switched to");
        rows.extend(led(
            Lead::Bullet,
            fit_parts(&first, &parts, "", glyphs, width),
            moved.mark,
        ));
        rows.extend(reason.iter().flat_map(said));
    }
    for kept in &out.kept {
        let lines = match &kept.busy {
            Some((name, state)) => working_moves(&kept.agent, name, *state, glyphs),
            None => stays_on(&kept.agent, &kept.to, &kept.account, kept.charges, glyphs),
        };
        rows.extend(
            lines
                .iter()
                .flat_map(|line| led(Lead::Hang, wrap(line), None)),
        );
    }
    for (gone, command) in &out.presets {
        rows.extend(led(
            Lead::Hang,
            wrap(&preset_line(gone, Some(command))),
            None,
        ));
    }
    if !out.not_resumed.is_empty() {
        if !rows.is_empty() {
            rows.push((Lead::Hang, StyledLine::default()));
        }
        let table = not_resumed_rows(&out.not_resumed, width, glyphs);
        rows.extend(table.into_iter().map(|line| (Lead::Hang, line)));
    }
    if out.again {
        rows.extend(led(Lead::Hang, wrap(&sign_in_again(Surface::App)), None));
    }
    rows
}

pub const CLEANUP_WARNING: &str =
    "The old sign-in may still be saved: remove Wisp's unused OAuth profile.";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    App,
    Terminal,
}

fn login_word(surface: Surface) -> &'static str {
    match surface {
        Surface::App => "/login",
        Surface::Terminal => "wisp login",
    }
}

pub fn login_again(surface: Surface, name: &str, short: bool) -> String {
    format!("{} {}", login_word(surface), login_words(name, short))
}

pub fn login_words(name: &str, short: bool) -> String {
    if short {
        name.to_owned()
    } else {
        format!("--name {name}")
    }
}

pub fn same_account_reason(provider: SubscriptionProvider, name: &str) -> String {
    format!("this is the same {} account as {name}", provider.title())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SameAccount {
    pub provider: SubscriptionProvider,
    pub name: String,
    pub mark: Option<u8>,
    pub surface: Surface,
    pub short: bool,
}

pub fn fit_command(lead: &str, command: &str, room: usize, glyphs: &Glyphs) -> Vec<String> {
    let joined = format!("{lead} {command}");
    if width_of(&joined, WidthMode::from(glyphs.set)) <= room {
        return vec![joined];
    }
    let mut rows = wrap_plain(lead, room, "", "", glyphs);
    rows.extend(wrap_plain(command, room, "", "", glyphs));
    rows
}

pub fn same_account_rows(refusal: &SameAccount, room: usize, glyphs: &Glyphs) -> Vec<StyledLine> {
    let name = marked(&refusal.name, refusal.mark, glyphs);
    let reason = same_account_reason(refusal.provider, &name);
    let text = format!("Not signed in: {reason}.");
    let mut rows = wrap_words(
        &mark_line(&text, Token::Error, refusal.mark, glyphs).spans,
        room,
        WidthMode::from(glyphs.set),
    );
    let command = login_again(refusal.surface, &refusal.name, refusal.short);
    rows.extend(
        fit_command("To sign it in again:", &command, room, glyphs)
            .into_iter()
            .map(|row| StyledLine::new(vec![StyledSpan::new(row, Token::TextMuted)])),
    );
    rows
}

pub const BANNED_WORDS: [&str; 8] = [
    "plan",
    "paid",
    "env",
    "endpoint",
    "endpoints",
    "charged per use",
    "\u{2248}",
    "\u{2265}",
];

pub fn vocabulary_slips(text: &str) -> Vec<&'static str> {
    let lower = text
        .to_lowercase()
        .replace("--env-file", "")
        .replace(concat!("/", wisp_protocol::keys_file!()), "");
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .collect();
    let parts: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).collect();
    BANNED_WORDS
        .iter()
        .copied()
        .filter(|banned| {
            if banned.contains(' ') || !banned.is_ascii() {
                lower.contains(banned)
            } else if banned.starts_with("endpoint") {
                parts.contains(banned)
            } else {
                words.contains(banned)
            }
        })
        .collect()
}

#[cfg(any(test, feature = "testing"))]
pub fn mockup_mark(name: &str) -> Option<u8> {
    match name {
        "claude-subscription" => Some(1),
        "claude-work" => Some(2),
        "chatgpt" => Some(3),
        "claude-api" => Some(4),
        "claude" | "work-api" => Some(5),
        _ => Some(6),
    }
}

#[cfg(test)]
pub(crate) mod tests;
