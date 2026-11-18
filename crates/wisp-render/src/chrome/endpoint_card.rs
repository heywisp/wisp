use std::ops::Range;

use super::account::fit_parts;
use super::band::{row_of, selection_lead};
use super::caret::field;
use super::menu::{key_row, menu_lead};
use super::one_line;
use super::question::{answer_options, form_question, form_view, option_rows};
use super::row::full;
use super::row::{KEY_ROW, KeyHint};
use super::{masked_secret, masked_secret_ends};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{fit_start, width, wrap_words};
use crate::theme::Token;
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::transcript::copy;

pub const CARD_LABEL_WIDTH: usize = 11;
pub const SETUP_TITLE: &str = "Let's connect a model.";
pub const ADD_TITLE: &str = "Add an API key";

pub fn add_row(set: GlyphSet) -> String {
    format!("+ {ADD_TITLE}{}", Glyphs::of(set).more)
}
pub const PROVIDER_LABEL: &str = "Provider";
pub const CONNECT_LABEL: &str = "Connect";
pub const URL_LABEL: &str = "URL";
pub const AUTH_LABEL: &str = "Sends as";
pub const KEY_LABEL: &str = "Key";
pub const NAME_LABEL: &str = "Name";
pub const GATEWAY_CHOICE: &str = "A gateway or proxy";
pub const URL_PLACEHOLDER: &str = "https://";
pub const KEY_PLACEHOLDER: &str = "paste the key";
pub const QUIT_AFTER_SAVE: &str = "Wisp quits when the save ends.";
pub const KEY_REFUSED: &str = "That key was refused.";
pub const FORBIDDEN_NOTE: &str = "The key works but may not count tokens here.";
pub const NOT_OFFERED_NOTE: &str = "Couldn't check this account: it doesn't offer count_tokens.";
pub const ENTER_KEY_AGAIN: &str = "Enter it again";
pub const ENTER_URL_AGAIN: &str = "Enter the URL again";
pub const TRY_AGAIN: &str = "Try again";
pub const SAVE_ANYWAY: &str = "Save anyway";
pub const CANCEL: &str = "Cancel";
pub const NOT_SAVED: &str = "Not saved.";
pub const NOT_HTTP: &str = "That isn't an http or https URL.";
pub const PASTE_AGAIN: &str = "Paste it again.";
pub const NOT_CHECKED: &str = "Not checked";
pub const NO_CARD: &str = "This session can't add accounts.";
pub const THIS_URL: &str = "this URL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddQuestion {
    KeyRefused,
    Forbidden,
    NotOffered,
    CheckFailed,
    KeychainFile,
    PlainHttp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddAnswer {
    EnterKeyAgain,
    EnterUrlAgain,
    TryAgain,
    SaveAnyway,
    Cancel,
    Yes,
    No,
}

impl AddAnswer {
    pub fn label(self) -> &'static str {
        match self {
            Self::EnterKeyAgain => ENTER_KEY_AGAIN,
            Self::EnterUrlAgain => ENTER_URL_AGAIN,
            Self::TryAgain => TRY_AGAIN,
            Self::SaveAnyway => SAVE_ANYWAY,
            Self::Cancel => CANCEL,
            Self::Yes => copy::YES,
            Self::No => copy::NO,
        }
    }
}

impl AddQuestion {
    pub fn answers(self, gateway: bool) -> (&'static [AddAnswer], usize) {
        use AddAnswer as A;
        match self {
            Self::KeyRefused => (&[A::EnterKeyAgain, A::SaveAnyway, A::Cancel], 0),
            Self::CheckFailed if gateway => (&[A::EnterUrlAgain, A::SaveAnyway, A::Cancel], 0),
            Self::CheckFailed => (&[A::TryAgain, A::SaveAnyway, A::Cancel], 0),
            Self::Forbidden | Self::NotOffered => (&[A::Yes, A::No], 0),
            Self::KeychainFile | Self::PlainHttp => (&[A::Yes, A::No], 1),
        }
    }
}

pub fn forbidden_ask() -> String {
    format!("{FORBIDDEN_NOTE} {SAVE_ANYWAY}?")
}

pub fn not_offered_ask() -> String {
    format!("{NOT_OFFERED_NOTE} {SAVE_ANYWAY}?")
}

pub fn checked_ok(set: GlyphSet) -> String {
    format!("Checking{} ok", Glyphs::of(set).more)
}

pub fn check_failed(reason: &str) -> String {
    format!("The check failed: {reason}.")
}

pub fn keychain_question(which: &str, file: &str, many: bool) -> String {
    let these = if many { "these keys" } else { "this key" };
    format!("The keychain {which}. Keep {these} in {file} instead?")
}

pub fn keychain_stopped(which: &str) -> String {
    format!("The keychain {which}, so nothing was saved; unlock it and try again.")
}

pub fn plain_http_risk(subject: &str, host: &str) -> String {
    format!("{subject} uses plain http to {host}, so the key is sent unencrypted")
}

pub fn plain_http(host: &str) -> String {
    format!("Warning: {}. Continue?", plain_http_risk(THIS_URL, host))
}

pub fn remove_question(name: &str, place: Option<&str>) -> String {
    match place {
        Some(place) => format!("Remove \"{name}\" and delete its key from {place}?"),
        None => format!("Remove \"{name}\"?"),
    }
}

pub fn bad_name() -> String {
    format!(
        "{}.",
        copy::sentence_case(&wisp_protocol::bad_account_name())
    )
}

pub fn taken_name(name: &str) -> String {
    format!("An account named \"{name}\" already exists; choose another name.")
}

pub fn saved_line(check: Option<&str>, place: &str, set: GlyphSet) -> String {
    let sep = Glyphs::of(set).separator;
    match check {
        Some(check) => format!("{check} {sep} saved in {place}"),
        None => format!("Saved in {place}"),
    }
}

pub fn saved_note(name: &str, place: &str) -> String {
    format!("Saved \"{name}\" in {place}. Switch to it with /account {name}.")
}

pub fn not_saved_note(name: &str, why: &str) -> String {
    format!("\"{name}\" wasn't saved: {why}")
}

pub fn saving_words(set: GlyphSet) -> String {
    format!("saving the account{}", Glyphs::of(set).more)
}

pub fn edit_title(name: &str, url: bool) -> String {
    let field = if url { "URL" } else { "key" };
    format!("Change the {field} for \"{name}\"")
}

pub fn field_words(field: &wisp_protocol::endpoint::ConnectionField) -> String {
    use wisp_protocol::endpoint::ConnectionField;
    match field {
        ConnectionField::Provider => "provider".to_owned(),
        ConnectionField::Auth => "auth".to_owned(),
        ConnectionField::Url => "URL".to_owned(),
        ConnectionField::Key => "key".to_owned(),
        ConnectionField::Store => "key store".to_owned(),
        ConnectionField::KeyEnv => "key variable".to_owned(),
        ConnectionField::UrlEnv => "URL variable".to_owned(),
        ConnectionField::Other(word) => crate::text::sanitize(word),
    }
}

pub fn edited_note(name: &str, fields: &[String], current: bool) -> String {
    let when = if current {
        "applies next session"
    } else {
        "applies when you switch to it"
    };
    if fields.is_empty() {
        return format!("Saved \"{name}\"; {when}.");
    }
    format!("Changed \"{name}\": {}; {when}.", fields.join(", "))
}

pub fn removed_note(name: &str, kept_item: bool, default_cleared: bool) -> String {
    let mut out = format!("Removed \"{name}\".");
    if kept_item {
        out.push_str(" Its keychain item was kept.");
    }
    if default_cleared {
        out.push_str(" Wisp no longer starts on it; the start rules choose.");
    }
    out
}

pub fn official_choice(provider: &str, host: &str) -> Vec<String> {
    let mut parts = vec![format!("{provider}'s API")];
    parts.extend((!host.is_empty()).then(|| host.to_owned()));
    parts
}

pub fn import_note(names: &[String]) -> Option<String> {
    let names = match names {
        [] => return None,
        many => copy::listed(many),
    };
    Some(format!(
        "Keys were found in {names}. To store them, quit and run wisp account import (or wisp setup)."
    ))
}

pub fn radio(on: bool, set: GlyphSet) -> &'static str {
    match (set, on) {
        (GlyphSet::Unicode, true) => "\u{25CF}",
        (GlyphSet::Unicode, false) => "\u{25CB}",
        (_, true) => "(*)",
        (_, false) => "( )",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue {
    Fixed(String),
    Choices {
        options: Vec<Vec<String>>,
        chosen: usize,
    },
    Text {
        text: String,
        placeholder: String,
    },
    Secret(wisp_protocol::Secret, Option<wisp_protocol::Ends>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldRow {
    pub label: String,
    pub value: FieldValue,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusTone {
    Muted,
    Ok,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuView {
    pub question: String,
    pub options: Vec<String>,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointCardView {
    pub banner: bool,
    pub title: String,
    pub note: Option<String>,
    pub fields: Vec<FieldRow>,
    pub status: Vec<(String, StatusTone)>,
    pub menu: Option<MenuView>,
    pub hints: Vec<KeyHint>,
    pub active: bool,
}

fn blank(layout: Layout) -> StyledLine {
    full(vec![], layout)
}

fn label_span(text: &str, focused: bool, layout: Layout) -> StyledSpan {
    let shown = one_line(text);
    let pad = CARD_LABEL_WIDTH.saturating_sub(width(&shown, layout.width_mode));
    let tone = if focused {
        Token::Text
    } else {
        Token::TextMuted
    };
    StyledSpan::new(format!("{shown}{}", " ".repeat(pad)), tone)
}

fn caret(active: bool, focused: bool) -> Vec<StyledSpan> {
    let caret = Look::fg(Token::Text).reverse();
    field("", (active && focused).then_some(0), Look::default(), caret)
}

fn field_lines(row: &FieldRow, active: bool, layout: Layout) -> (Vec<StyledLine>, usize) {
    let focused = row.focused && active;
    let label = label_span(&row.label, focused, layout);
    let line = |spans| row_of(spans, focused, layout);
    let lead_w = menu_lead(layout);
    let value_room = layout.width.saturating_sub(lead_w + CARD_LABEL_WIDTH + 1);
    let glyphs = Glyphs::of(layout.glyphs);
    match &row.value {
        FieldValue::Fixed(text) => {
            let mut spans = selection_lead(focused, layout);
            spans.extend([label, StyledSpan::new(one_line(text), Token::Text)]);
            (vec![line(spans)], 0)
        }
        FieldValue::Choices { options, chosen } => {
            let parts: Vec<Vec<StyledLine>> = options
                .iter()
                .enumerate()
                .map(|(i, option)| {
                    let on = i == *chosen;
                    let look = Look::fg(if on { Token::Text } else { Token::TextMuted });
                    let mark_look = if on && focused {
                        Look::fg(Token::Accent)
                    } else {
                        look
                    };
                    let mark = format!("{} ", radio(on, layout.glyphs));
                    let mark_w = width(&mark, layout.width_mode);
                    let room = layout
                        .width
                        .saturating_sub(lead_w + CARD_LABEL_WIDTH + mark_w)
                        .max(1);
                    let pieces: Vec<(String, String)> = option
                        .iter()
                        .map(|part| (one_line(part), one_line(part)))
                        .collect();
                    fit_parts(&[], &pieces, "", &glyphs, room)
                        .into_iter()
                        .enumerate()
                        .map(|(n, text)| {
                            let mut spans = selection_lead(focused && i + n == 0, layout);
                            spans.push(match (i, n) {
                                (0, 0) => label.clone(),
                                (_, 0) => StyledSpan::plain(" ".repeat(CARD_LABEL_WIDTH)),
                                _ => StyledSpan::plain(" ".repeat(CARD_LABEL_WIDTH + mark_w)),
                            });
                            if n == 0 {
                                spans.push(StyledSpan::new(mark.clone(), mark_look));
                            }
                            spans.push(StyledSpan::new(text, look));
                            line(spans)
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            let at = parts.iter().take(*chosen).map(Vec::len).sum();
            (parts.concat(), at)
        }
        FieldValue::Text { text, placeholder } => {
            let mut spans = selection_lead(focused, layout);
            spans.push(label);
            let clean = one_line(text);
            if clean.is_empty() {
                spans.extend(caret(active, row.focused));
                spans.push(StyledSpan::new(one_line(placeholder), Token::TextFaint));
            } else {
                spans.push(StyledSpan::new(
                    fit_start(&clean, value_room, glyphs.more, layout.width_mode),
                    Token::Text,
                ));
                spans.extend(caret(active, row.focused));
            }
            (vec![line(spans)], 0)
        }
        FieldValue::Secret(key, ends) => {
            let mut spans = selection_lead(focused, layout);
            spans.push(label);
            if key.expose().is_empty() {
                spans.extend(caret(active, row.focused));
                spans.push(StyledSpan::new(KEY_PLACEHOLDER, Token::TextFaint));
            } else {
                spans.push(match ends {
                    Some(ends) => {
                        masked_secret_ends(key.expose(), *ends, false, value_room, glyphs.more)
                    }
                    None => masked_secret(key.expose(), value_room, glyphs.more),
                });
                spans.extend(caret(active, row.focused));
            }
            (vec![line(spans)], 0)
        }
    }
}

fn wrapped(text: &str, look: Look, layout: Layout) -> Vec<StyledLine> {
    let lead_w = menu_lead(layout);
    let room = layout.width.saturating_sub(lead_w).max(1);
    wrap_words(
        &[StyledSpan::new(one_line(text), look)],
        room,
        layout.width_mode,
    )
    .into_iter()
    .map(|row| {
        let mut spans = vec![StyledSpan::plain(" ".repeat(lead_w))];
        spans.extend(row.spans);
        full(spans, layout)
    })
    .collect()
}

fn tone_look(tone: StatusTone) -> Look {
    match tone {
        StatusTone::Muted => Look::fg(Token::TextMuted),
        StatusTone::Ok => Look::fg(Token::Ok),
        StatusTone::Error => Look::fg(Token::Error),
    }
}

fn menu_lines(menu: &MenuView, layout: Layout) -> Vec<StyledLine> {
    let labels: Vec<&str> = menu.options.iter().map(String::as_str).collect();
    let questions = [form_question("", &menu.question, &answer_options(&labels))];
    let view = form_view(&questions, Some(menu.selected));
    let mut lines = wrapped(&menu.question, Look::fg(Token::Text), layout);
    for i in 0..menu.options.len() {
        lines.extend(option_rows(&view, 0, i, &questions[0], None, layout).lines);
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointCardRows {
    pub lines: Vec<StyledLine>,
    pub focus: Range<usize>,
}

impl EndpointCardRows {
    pub fn skip(&self, room: usize) -> usize {
        self.lines
            .len()
            .saturating_sub(room)
            .min(self.focus.start)
            .max(self.focus.end.saturating_sub(room))
    }
}

pub fn banner(layout: Layout) -> [StyledLine; 2] {
    let glyphs = Glyphs::of(layout.glyphs);
    let mark = match layout.glyphs {
        GlyphSet::Ascii => glyphs.wisp,
        GlyphSet::Unicode | GlyphSet::AmbiguousWide => glyphs.needs_you,
    };
    let wordmark = vec![
        StyledSpan::plain(" "),
        StyledSpan::new(mark, Token::Accent),
        StyledSpan::plain(" "),
        StyledSpan::new(super::WORDMARK, Look::fg(Token::Text).bold()),
    ];
    [full(wordmark, layout), blank(layout)]
}

pub fn endpoint_card_rows(view: &EndpointCardView, layout: Layout) -> EndpointCardRows {
    let mut lines = Vec::new();
    if view.banner {
        lines.extend(banner(layout));
        lines.extend(wrapped(&view.title, Look::fg(Token::Text), layout));
    } else {
        lines.push(full(
            vec![
                StyledSpan::plain(" "),
                StyledSpan::new("+", Look::fg(Token::Accent).bold()),
                StyledSpan::plain(" "),
                StyledSpan::new(one_line(&view.title), Look::fg(Token::Text).bold()),
            ],
            layout,
        ));
    }
    if let Some(note) = &view.note {
        lines.extend(wrapped(note, Look::fg(Token::Text), layout));
    }
    lines.push(blank(layout));
    let active = view.active && view.menu.is_none();
    let mut focus = None;
    for row in &view.fields {
        let start = lines.len();
        let (field, chosen) = field_lines(row, active, layout);
        lines.extend(field);
        if active && row.focused {
            focus = Some(start..(start + chosen + 1).min(lines.len()));
        }
    }
    if !view.status.is_empty() || view.menu.is_some() {
        lines.push(blank(layout));
    }
    for (text, tone) in &view.status {
        lines.extend(wrapped(text, tone_look(*tone), layout));
    }
    if let Some(menu) = &view.menu {
        lines.extend(menu_lines(menu, layout));
        let first = lines.len() - menu.options.len();
        focus = Some(first..(first + menu.selected + 1).min(lines.len()));
    }
    if !view.hints.is_empty() {
        lines.push(blank(layout));
        let lead = " ".repeat(menu_lead(layout));
        lines.push(key_row(&view.hints, "", None, KEY_ROW, &lead, layout));
    }
    let focus = focus.unwrap_or(lines.len()..lines.len());
    EndpointCardRows { lines, focus }
}

#[cfg(test)]
mod tests;
