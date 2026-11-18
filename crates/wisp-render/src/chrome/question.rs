use unicode_segmentation::UnicodeSegmentation;
use wisp_protocol::{Answer, Question, QuestionOption};

use super::account::{
    AccountKind, SIGNED_IN, SameAccount, billing_token, mark, mark_line, same_account_rows,
};
use super::band::{CHROME_EDGE, in_band, row_of, selection_lead, widened_all};
use super::caret::field;
use super::menu::{key_row, menu_lead};
use super::one_line;
use super::row::{KEY_ROW, KeyHint, Rows, clip, cut_at_word, edge, full};
use super::{masked_secret, masked_secret_ends};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{grapheme_width, sanitize, spans_width, width, wrap};
use crate::theme::Token;
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::transcript::copy;

pub const QUESTION_MIN_ROWS: usize = 8;
pub const HEADER_MAX: usize = 12;
pub const OWN_MAX_ROWS: usize = 8;
const COLUMN_GAP: usize = 2;
pub(super) const GAP_ROWS: usize = 2;
const MARK: &str = "?";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardPage {
    Question(usize),
    Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardHit {
    Option(usize),
    Own,
    Send,
    Back,
    Link,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardMode {
    Settling,
    Active,
    Sending,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub chosen: Vec<u32>,
    pub own: String,
    pub own_chosen: bool,
    pub answered: bool,
}

impl Draft {
    pub fn has_answer(&self) -> bool {
        self.answered || !self.chosen.is_empty() || (self.own_chosen && !self.own.trim().is_empty())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CardView<'a> {
    pub questions: &'a [Question],
    pub drafts: &'a [Draft],
    pub page: CardPage,
    pub row: usize,
    pub cursor: Option<(usize, usize)>,
    pub mode: CardMode,
}

pub type CardRows = Rows<CardHit>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkCells {
    pub line: usize,
    pub start: usize,
    pub width: usize,
}

pub fn cut_header(header: &str, set: GlyphSet) -> String {
    let clean = one_line(header);
    let graphemes: Vec<&str> = clean.graphemes(true).collect();
    if graphemes.len() <= HEADER_MAX {
        return clean;
    }
    let mut out: String = graphemes.into_iter().take(HEADER_MAX).collect();
    out.push_str(Glyphs::of(set).more);
    out
}

fn first_line(text: &str, more: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    let mut out = one_line(line.trim());
    if lines > 1 {
        out.push(' ');
        out.push_str(more);
    }
    out
}

pub fn answer_words(question: &Question, answer: &Answer, set: GlyphSet) -> String {
    if answer.is_skipped() {
        return copy::SKIPPED.to_owned();
    }
    let sep = format!(" {} ", Glyphs::of(set).separator);
    let mut parts: Vec<String> = answer
        .chosen
        .iter()
        .filter_map(|i| usize::try_from(*i).ok())
        .filter_map(|i| question.options.get(i))
        .map(|o| one_line(&o.label))
        .collect();
    if let Some(own) = &answer.own {
        let line = first_line(own, Glyphs::of(set).more);
        parts.push(if question.is_typed() {
            line
        } else {
            format!("{}{line}", copy::OWN_SHORT)
        });
    }
    parts.join(&sep)
}

fn draft_words(question: &Question, draft: &Draft, set: GlyphSet) -> String {
    if !draft.has_answer() {
        return copy::NOT_ANSWERED_YET.to_owned();
    }
    let own = draft.own_chosen && !draft.own.trim().is_empty();
    let answer = Answer {
        chosen: draft.chosen.clone(),
        own: own.then(|| draft.own.clone()),
    };
    answer_words(question, &answer, set)
}

fn marked(focused: bool, view: &CardView<'_>) -> bool {
    focused && view.mode != CardMode::Settling
}

fn blank(layout: Layout) -> StyledLine {
    full(vec![], layout)
}

fn lead(focused: bool, view: &CardView<'_>, layout: Layout) -> Vec<StyledSpan> {
    selection_lead(marked(focused, view), layout)
}

fn checkbox(on: bool) -> &'static str {
    if on { "[x] " } else { "[ ] " }
}

fn hint_row(keys: &[KeyHint], layout: Layout) -> StyledLine {
    key_row(
        keys,
        "",
        None,
        KEY_ROW,
        &" ".repeat(menu_lead(layout)),
        layout,
    )
}

fn several(view: &CardView<'_>) -> bool {
    view.questions.len() > 1
}

fn own_row(view: &CardView<'_>, question: usize) -> usize {
    view.questions.get(question).map_or(0, |q| q.options.len())
}

fn hints(view: &CardView<'_>, layout: Layout) -> Vec<KeyHint> {
    let set = layout.glyphs;
    match view.mode {
        CardMode::Settling => return vec![KeyHint::note(copy::CARD_SETTLING)],
        CardMode::Sending => {
            let sending = format!("{}{}", copy::CARD_SENDING, Glyphs::of(set).more);
            return vec![KeyHint::note(sending)];
        }
        CardMode::Active => {}
    }
    let send = KeyHint::new(copy::ENTER_KEY, copy::KEY_SEND);
    let enter = if several(view) {
        KeyHint::new(copy::ENTER_KEY, copy::NEXT_WORD)
    } else {
        send.clone()
    };
    let CardPage::Question(q) = view.page else {
        let unanswered = view.drafts.iter().any(|d| !d.has_answer());
        let skipped = unanswered.then(|| KeyHint::note(copy::CARD_SKIPPED_SENT));
        return [Some(send), skipped, Some(copy::page_scroll())]
            .into_iter()
            .flatten()
            .collect();
    };
    let multi = view.questions.get(q).is_some_and(|q| q.multi_select);
    let choosing = view.row != own_row(view, q) && !multi;
    let mut out = if view.row == own_row(view, q) {
        vec![enter, KeyHint::new(copy::NEW_LINE_KEY, copy::KEY_NEW_LINE)]
    } else if multi {
        vec![KeyHint::new(copy::SPACE_KEY, copy::TOGGLE_WORD), enter]
    } else {
        let rows = own_row(view, q) + 1;
        let digits = (rows <= 9).then(|| copy::card_pick(rows, set));
        [Some(enter), digits].into_iter().flatten().collect()
    };
    if several(view) {
        out.push(KeyHint::new(copy::TAB_KEY, copy::NEXT_QUESTION));
    }
    let typed = view.questions.get(q).is_some_and(Question::is_typed);
    if typed {
        out.push(copy::arrows(set));
    }
    out.push(copy::page_scroll());
    if choosing && !typed {
        out.push(copy::arrows(set));
    }
    out
}

fn tabs(view: &CardView<'_>, layout: Layout) -> Vec<StyledSpan> {
    let glyphs = Glyphs::of(layout.glyphs);
    let sep = format!(" {} ", glyphs.separator);
    let mut out = vec![];
    for (i, question) in view.questions.iter().enumerate() {
        if i > 0 {
            out.push(StyledSpan::new(sep.clone(), Token::TextMuted));
        }
        let current = view.page == CardPage::Question(i);
        let look = if current {
            Look::fg(Token::Accent)
        } else {
            Look::fg(Token::TextMuted)
        };
        out.push(StyledSpan::new(
            cut_header(&question.header, layout.glyphs),
            look,
        ));
        if view.drafts.get(i).is_some_and(Draft::has_answer) {
            out.push(StyledSpan::new(format!(" {}", glyphs.done), Token::Ok));
        }
    }
    out
}

fn header_row(view: &CardView<'_>, layout: Layout) -> StyledLine {
    let title = match view.page {
        CardPage::Question(q) => view
            .questions
            .get(q)
            .map(|q| cut_header(&q.header, layout.glyphs))
            .unwrap_or_default(),
        CardPage::Review => copy::REVIEW.to_owned(),
    };
    let count = match view.page {
        CardPage::Question(q) if several(view) => copy::page_of(q + 1, view.questions.len()),
        CardPage::Question(q) => copy::question_of(q + 1, view.questions.len()),
        CardPage::Review => String::new(),
    };
    let count_span = StyledSpan::new(count, Token::TextMuted);
    let mut rights = vec![];
    if several(view) {
        let mut right = tabs(view, layout);
        if !count_span.text.is_empty() {
            right.push(StyledSpan::plain(" ".repeat(COLUMN_GAP)));
        }
        right.push(count_span.clone());
        rights.push(right);
    }
    rights.push(vec![count_span]);
    header_line(vec![bold(title)], rights, layout)
}

fn bold(text: impl Into<String>) -> StyledSpan {
    StyledSpan::new(text, Look::fg(Token::Text).bold())
}

fn header_line(title: Vec<StyledSpan>, rights: Vec<Vec<StyledSpan>>, layout: Layout) -> StyledLine {
    let mut left = vec![
        StyledSpan::plain(" "),
        StyledSpan::new(MARK, Look::fg(Token::Accent).bold()),
        StyledSpan::plain(" "),
    ];
    left.extend(title);
    let edge = layout.width.saturating_sub(1).min(CHROME_EDGE);
    let left_w = spans_width(&left, layout.width_mode);
    let right = rights
        .into_iter()
        .find(|right| left_w + COLUMN_GAP + spans_width(right, layout.width_mode) <= edge)
        .unwrap_or_default();
    let mut spans = left;
    let right_w = spans_width(&right, layout.width_mode);
    if right_w > 0 {
        spans.push(StyledSpan::plain(
            " ".repeat(edge.saturating_sub(left_w + right_w)),
        ));
        spans.extend(right);
    }
    full(spans, layout)
}

fn question_lines(view: &CardView<'_>, q: usize, layout: Layout) -> Vec<StyledLine> {
    view.questions
        .get(q)
        .map(|question| indented(&question_spans(question), layout))
        .unwrap_or_default()
}

fn question_spans(question: &Question) -> Vec<StyledSpan> {
    let mut spans = vec![StyledSpan::new(
        sanitize(&question.question).replace('\t', " "),
        Token::Text,
    )];
    if question.multi_select {
        spans.push(StyledSpan::new(
            format!(" {}", copy::CHOOSE_ANY),
            Token::TextMuted,
        ));
    }
    if let Some(line) = question
        .source
        .as_ref()
        .and_then(|source| copy::asked_by(&source.asked_by))
    {
        spans.push(StyledSpan::new(format!("\n{line}"), Token::TextMuted));
    }
    spans
}

fn caret_row(lines: &[StyledLine]) -> usize {
    lines
        .iter()
        .position(|l| l.spans.iter().any(|s| s.look.reverse))
        .unwrap_or(0)
}

fn indented(spans: &[StyledSpan], layout: Layout) -> Vec<StyledLine> {
    let indent = menu_lead(layout);
    wrap(spans, lead_room(layout), layout.width_mode)
        .into_iter()
        .map(|row| {
            let mut out = vec![StyledSpan::plain(" ".repeat(indent))];
            out.extend(row.spans);
            full(out, layout)
        })
        .collect()
}

fn lead_room(layout: Layout) -> usize {
    layout.width.saturating_sub(menu_lead(layout) + 1).max(1)
}

fn marked_rows(mark: StyledSpan, rows: Vec<StyledLine>, layout: Layout) -> Vec<StyledLine> {
    let indent = menu_lead(layout);
    let pad = indent.saturating_sub(1 + width(&mark.text, layout.width_mode));
    let mut lead = vec![
        StyledSpan::plain(" "),
        mark,
        StyledSpan::plain(" ".repeat(pad)),
    ];
    rows.into_iter()
        .map(|row| {
            let hang = vec![StyledSpan::plain(" ".repeat(indent))];
            let mut out = std::mem::replace(&mut lead, hang);
            out.extend(row.spans);
            full(out, layout)
        })
        .collect()
}

pub(super) fn option_rows(
    view: &CardView<'_>,
    q: usize,
    i: usize,
    question: &Question,
    account: Option<Option<u8>>,
    layout: Layout,
) -> CardRows {
    let mut rows = CardRows::default();
    let Some(option) = question.options.get(i) else {
        return rows;
    };
    let draft = view.drafts.get(q);
    let focused = view.row == i;
    let index = u32::try_from(i).unwrap_or(u32::MAX);
    let chosen = draft.is_some_and(|d| d.chosen.contains(&index));
    let mut spans = lead(focused, view, layout);
    if question.multi_select {
        let look = if chosen {
            Look::fg(Token::Accent)
        } else {
            Look::fg(Token::TextMuted)
        };
        spans.push(StyledSpan::new(checkbox(chosen), look));
    }
    spans.push(StyledSpan::new(format!("{} ", i + 1), Token::TextMuted));
    let indent = spans_width(&spans, layout.width_mode);
    let band = marked(focused, view);
    let label = one_line(&option.label);
    match account {
        Some(account) => {
            let glyphs = Glyphs::of(layout.glyphs);
            spans.extend(mark_line(&label, Token::Text, account, &glyphs).spans);
        }
        None => spans.push(StyledSpan::new(label, Token::Text)),
    }
    if chosen && !question.multi_select {
        let glyphs = Glyphs::of(layout.glyphs);
        spans.push(StyledSpan::new(format!("  {}", glyphs.done), Token::Ok));
    }
    rows.push(row_of(spans, band, layout), Some(CardHit::Option(i)));
    let description = sanitize(&option.description).replace(['\t', '\r'], " ");
    if !description.trim().is_empty() {
        let room = layout.width.saturating_sub(indent + 1).max(1);
        for row in wrap(
            &[StyledSpan::new(description.trim(), Token::TextMuted)],
            room,
            layout.width_mode,
        ) {
            let mut out = vec![StyledSpan::plain(" ".repeat(indent))];
            out.extend(row.spans);
            rows.push(row_of(out, band, layout), Some(CardHit::Option(i)));
        }
    }
    rows
}

struct Piece {
    text: String,
    caret: Option<usize>,
}

fn split_own(
    text: &str,
    cursor: Option<(usize, usize)>,
    room: usize,
    layout: Layout,
) -> Vec<Piece> {
    let mut out = vec![];
    let mut caret_at = None;
    for (n, line) in text.split('\n').enumerate() {
        let caret_col = cursor.filter(|(l, _)| *l == n).map(|(_, c)| c);
        let mut row = String::new();
        let mut used = 0;
        let mut chars = 0;
        let mut caret = None;
        for g in line.graphemes(true) {
            let w = grapheme_width(g, layout.width_mode).max(1);
            if used + w > room && !row.is_empty() {
                out.push(Piece {
                    text: std::mem::take(&mut row),
                    caret: caret.take(),
                });
                used = 0;
            }
            if caret_col == Some(chars) {
                caret = Some(row.len());
            }
            row.push_str(g);
            used += w;
            chars += g.chars().count();
        }
        if caret_col.is_some_and(|c| c >= chars) && caret.is_none() {
            if used + 1 > room && !row.is_empty() {
                out.push(Piece {
                    text: std::mem::take(&mut row),
                    caret: None,
                });
            }
            caret = Some(row.len());
        }
        if caret.is_some() {
            caret_at = Some(out.len());
        }
        out.push(Piece { text: row, caret });
    }
    if let Some(at) = caret_at {
        let skip = (at + 1).saturating_sub(OWN_MAX_ROWS);
        out.drain(..skip);
    }
    out.truncate(OWN_MAX_ROWS);
    out
}

fn own_rows(view: &CardView<'_>, q: usize, question: &Question, layout: Layout) -> CardRows {
    let mut rows = CardRows::default();
    let i = question.options.len();
    let focused = view.row == i;
    let draft = view.drafts.get(q);
    let clean = sanitize(draft.map_or("", |d| d.own.as_str())).replace('\t', " ");
    let text = clean.as_str();
    let typed = !text.is_empty();
    let mut spans = lead(focused, view, layout);
    if question.multi_select {
        let on = !text.trim().is_empty();
        let look = if on {
            Look::fg(Token::Accent)
        } else {
            Look::fg(Token::TextMuted)
        };
        spans.push(StyledSpan::new(checkbox(on), look));
    }
    spans.push(StyledSpan::new(format!("{} ", i + 1), Token::TextMuted));
    let active = focused && view.mode == CardMode::Active;
    let band = marked(focused, view);
    let label_look = if band {
        Look::fg(Token::Text)
    } else {
        Look::fg(Token::TextFaint)
    };
    if !typed {
        let caret = Look::default().reverse();
        spans.extend(field("", active.then_some(0), Look::default(), caret));
        spans.push(StyledSpan::new(
            if question.is_typed() {
                copy::TYPE_ANSWER
            } else {
                copy::OWN_ANSWER
            },
            label_look,
        ));
        rows.push(row_of(spans, band, layout), Some(CardHit::Own));
        return rows;
    }
    if !question.is_typed() {
        spans.push(StyledSpan::new(copy::OWN_PREFIX, label_look));
    }
    let indent = spans_width(&spans, layout.width_mode);
    let room = layout.width.saturating_sub(indent + 1).max(1);
    let cursor = if active { view.cursor } else { None };
    let text_look = Look::fg(Token::Text);
    let caret = text_look.reverse();
    let pieces = split_own(text, cursor, room, layout);
    for (n, piece) in pieces.iter().enumerate() {
        let mut line = if n == 0 {
            spans.clone()
        } else {
            vec![StyledSpan::plain(" ".repeat(indent))]
        };
        line.extend(field(&piece.text, piece.caret, text_look, caret));
        if n == 0
            && !question.multi_select
            && !question.is_typed()
            && draft.is_some_and(|d| d.own_chosen && !d.own.trim().is_empty())
        {
            let glyphs = Glyphs::of(layout.glyphs);
            line.push(StyledSpan::new(format!("  {}", glyphs.done), Token::Ok));
        }
        rows.push(row_of(line, band, layout), Some(CardHit::Own));
    }
    rows
}

fn fit_rows(
    mut top: CardRows,
    question: Vec<StyledLine>,
    body: CardRows,
    focus: (usize, usize),
    hint: StyledLine,
    room: usize,
    layout: Layout,
) -> CardRows {
    let mut out = CardRows::default();
    let gaps = GAP_ROWS - usize::from(body.len() == 0);
    let natural = top.len() + question.len() + body.len() + 1 + gaps;
    if natural <= room {
        out.extend(top);
        for line in question {
            out.push(line, None);
        }
        if body.len() > 0 {
            out.push(blank(layout), None);
        }
        out.extend(body);
        out.push(blank(layout), None);
        out.push(hint, None);
        return out;
    }
    let kept = 1 + body.len() + usize::from(!question.is_empty());
    let top_room = room.saturating_sub(kept).max(1);
    top.lines = cut(top.lines, top_room, layout);
    top.hits.truncate(top_room);
    let fixed = top.len() + 1;
    let question_room = room
        .saturating_sub(fixed + usize::from(body.len() > 0))
        .max(usize::from(!question.is_empty()));
    let question = cut(question, question_room, layout);
    let body_room = room.saturating_sub(fixed + question.len());
    let start = focus.1.saturating_sub(body_room).min(focus.0);
    out.extend(top);
    for line in question {
        out.push(line, None);
    }
    for (line, hit) in body
        .lines
        .into_iter()
        .zip(body.hits)
        .skip(start)
        .take(body_room)
    {
        out.push(line, hit);
    }
    out.push(hint, None);
    out.lines.truncate(room);
    out.hits.truncate(room);
    out
}

fn question_page(view: &CardView<'_>, q: usize, room: usize, layout: Layout) -> CardRows {
    let Some(question) = view.questions.get(q) else {
        return CardRows::default();
    };
    let mut top = CardRows::default();
    top.push(header_row(view, layout), None);
    let lines = question_lines(view, q, layout);
    let mut body = CardRows::default();
    let mut focus = (0, 0);
    for i in 0..=question.options.len() {
        let rows = if i == question.options.len() {
            own_rows(view, q, question, layout)
        } else {
            option_rows(view, q, i, question, None, layout)
        };
        if view.row == i {
            focus = (body.len() + caret_row(&rows.lines), body.len() + rows.len());
        }
        body.extend(rows);
    }
    let hint = hint_row(&hints(view, layout), layout);
    fit_rows(top, lines, body, focus, hint, room, layout)
}

fn review_page(view: &CardView<'_>, room: usize, layout: Layout) -> CardRows {
    let mut top = CardRows::default();
    top.push(header_row(view, layout), None);
    let header_w = view
        .questions
        .iter()
        .map(|q| width(&cut_header(&q.header, layout.glyphs), layout.width_mode))
        .max()
        .unwrap_or(0);
    let indent = menu_lead(layout);
    let summary: Vec<StyledLine> = view
        .questions
        .iter()
        .zip(view.drafts)
        .map(|(q, d)| {
            let header = cut_header(&q.header, layout.glyphs);
            let pad = header_w.saturating_sub(width(&header, layout.width_mode));
            let words = draft_words(q, d, layout.glyphs);
            let look = if words == copy::NOT_ANSWERED_YET {
                Look::fg(Token::TextFaint)
            } else {
                Look::fg(Token::Text)
            };
            full(
                vec![
                    StyledSpan::plain(" ".repeat(indent)),
                    StyledSpan::new(header, Token::TextMuted),
                    StyledSpan::plain(" ".repeat(pad + COLUMN_GAP + 1)),
                    StyledSpan::new(words, look),
                ],
                layout,
            )
        })
        .collect();
    let mut body = CardRows::default();
    for (i, (label, hit)) in [
        (copy::SEND_ANSWERS, CardHit::Send),
        (copy::GO_BACK, CardHit::Back),
    ]
    .into_iter()
    .enumerate()
    {
        let focused = view.row == i;
        let mut spans = lead(focused, view, layout);
        spans.push(StyledSpan::new(label, Token::Text));
        body.push(row_of(spans, marked(focused, view), layout), Some(hit));
    }
    let focus = (view.row.min(1), view.row.min(1) + 1);
    let hint = hint_row(&hints(view, layout), layout);
    fit_rows(top, summary, body, focus, hint, room, layout)
}

fn compact(view: &CardView<'_>, room: usize, layout: Layout) -> CardRows {
    let glyphs = Glyphs::of(layout.glyphs);
    let sep = format!(" {} ", glyphs.separator);
    let edge = layout.width.saturating_sub(1).min(CHROME_EDGE);
    let mut out = CardRows::default();
    let (title, rest) = match view.page {
        CardPage::Question(q) => {
            let question = view.questions.get(q);
            (
                question
                    .map(|q| cut_header(&q.header, layout.glyphs))
                    .unwrap_or_default(),
                question.map(question_spans).unwrap_or_default(),
            )
        }
        CardPage::Review => {
            let answered = view.drafts.iter().filter(|d| d.has_answer()).count();
            (
                copy::REVIEW.to_owned(),
                vec![StyledSpan::new(
                    copy::answered_count(answered, view.questions.len()),
                    Token::Text,
                )],
            )
        }
    };
    let first = marked_rows(
        StyledSpan::new(MARK, Look::fg(Token::Accent).bold()),
        wrap(
            &[
                StyledSpan::new(title, Look::fg(Token::Text).bold()),
                StyledSpan::new(sep, Token::TextMuted),
            ]
            .into_iter()
            .chain(rest)
            .collect::<Vec<_>>(),
            lead_room(layout),
            layout.width_mode,
        ),
        layout,
    );
    let (spans, total, hit) = match view.page {
        CardPage::Question(q) => {
            let Some(question) = view.questions.get(q) else {
                return out;
            };
            let rows = if view.row >= question.options.len() {
                own_rows(view, q, question, layout)
            } else {
                option_rows(view, q, view.row, question, None, layout)
            };
            let hit = rows.hits.first().copied().flatten();
            let at = caret_row(&rows.lines);
            let line = rows.lines.into_iter().nth(at).unwrap_or_default();
            (trimmed(line), question.options.len() + 1, hit)
        }
        CardPage::Review => {
            let focused = view.row.min(1);
            let mut spans = lead(true, view, layout);
            let (label, hit) = if focused == 0 {
                (copy::SEND_ANSWERS, CardHit::Send)
            } else {
                (copy::GO_BACK, CardHit::Back)
            };
            spans.push(StyledSpan::new(label, Token::Text));
            (spans, 2, Some(hit))
        }
    };
    let position = copy::card_position(view.row + 1, total, layout.glyphs);
    let position_w = width(&position, layout.width_mode);
    let room_left = edge.saturating_sub(position_w + COLUMN_GAP);
    let mut second = clip(spans, room_left, glyphs.more, layout.width_mode);
    let used = spans_width(&second, layout.width_mode);
    if used + COLUMN_GAP + position_w <= edge {
        second.push(StyledSpan::plain(
            " ".repeat(edge.saturating_sub(used + position_w)),
        ));
        second.push(StyledSpan::new(position, Token::TextMuted));
    }
    let hint = hint_row(&hints(view, layout), layout);
    let second = row_of(second, view.mode != CardMode::Settling, layout);
    for line in cut(first, room.saturating_sub(2), layout) {
        out.push(line, None);
    }
    out.push(second, hit);
    if room >= 2 {
        out.push(hint, None);
    }
    out
}

fn cut(mut lines: Vec<StyledLine>, room: usize, layout: Layout) -> Vec<StyledLine> {
    if lines.len() <= room {
        return lines;
    }
    lines.truncate(room);
    if let Some(last) = lines.pop() {
        let more = Glyphs::of(layout.glyphs).more;
        let mut spans = trimmed(last);
        let look = spans.last().map(|s| s.look).unwrap_or_default();
        spans.push(StyledSpan::new(more, look));
        lines.push(full(cut_at_word(spans, edge(layout), layout), layout));
    }
    lines
}

fn trimmed(line: StyledLine) -> Vec<StyledSpan> {
    let mut spans = line.spans;
    while spans.last().is_some_and(|s| s.text.trim().is_empty()) {
        spans.pop();
    }
    spans
}

pub fn question_rows(view: &CardView<'_>, room: usize, layout: Layout) -> CardRows {
    let band = in_band(layout);
    let mut rows = card_rows(view, room, band);
    rows.lines = widened_all(rows.lines, band, layout);
    rows
}

fn card_rows(view: &CardView<'_>, room: usize, layout: Layout) -> CardRows {
    if room == 0 || view.questions.is_empty() {
        return CardRows::default();
    }
    if room < QUESTION_MIN_ROWS {
        return compact(view, room, layout);
    }
    match view.page {
        CardPage::Question(q) => question_page(view, q, room, layout),
        CardPage::Review => review_page(view, room, layout),
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FormValue<'a> {
    Text(&'a str),
    Secret(&'a wisp_protocol::Secret),
    Ends {
        value: &'a wisp_protocol::Secret,
        ends: wisp_protocol::Ends,
        counted: bool,
    },
    Closed,
}

#[derive(Clone, Copy, Debug)]
pub struct FormField<'a> {
    pub label: &'a str,
    pub value: FormValue<'a>,
}

#[derive(Clone, Copy, Debug)]
pub struct FormAccount<'a> {
    pub kind: AccountKind,
    pub name: &'a str,
    pub mark: Option<u8>,
}

#[derive(Clone, Copy)]
pub struct LinkRow<'a> {
    pub text: Option<&'a str>,
    pub browser: &'a str,
    pub copy: &'a str,
    pub copy_look: Token,
    pub slot: usize,
}

#[derive(Clone, Copy, Default)]
pub struct FormView<'a> {
    pub title: &'a str,
    pub title_line: Option<&'a [StyledSpan]>,
    pub note: &'a str,
    pub prompt: &'a str,
    pub prompt_lines: &'a [StyledLine],
    pub error: &'a str,
    pub options: &'a [QuestionOption],
    pub option_marks: &'a [Option<u8>],
    pub selected: Option<usize>,
    pub field: Option<FormField<'a>>,
    pub footer: &'a [KeyHint],
    pub account: Option<FormAccount<'a>>,
    pub done: bool,
    pub link: Option<LinkRow<'a>>,
    pub refused: Option<&'a SameAccount>,
}

impl std::fmt::Debug for FormView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FormView")
            .field("selected", &self.selected)
            .field("option_count", &self.options.len())
            .field("has_field", &self.field.is_some())
            .field("has_link", &self.link.is_some())
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

pub const SECRET_PLACEHOLDER: &str = "hidden as you type or paste";

pub(super) fn form_question(title: &str, prompt: &str, options: &[QuestionOption]) -> Question {
    Question {
        header: one_line(title),
        question: prompt.into(),
        options: options.to_vec(),
        multi_select: false,
        source: None,
    }
}

pub(super) fn form_view(questions: &[Question], selected: Option<usize>) -> CardView<'_> {
    CardView {
        questions,
        drafts: &[],
        page: CardPage::Question(0),
        row: selected.unwrap_or(usize::MAX),
        cursor: None,
        mode: CardMode::Active,
    }
}

pub fn answer_options(labels: &[&str]) -> Vec<QuestionOption> {
    labels
        .iter()
        .map(|label| QuestionOption {
            label: (*label).to_owned(),
            description: String::new(),
        })
        .collect()
}

pub fn form_rows(form: &FormView<'_>, room: usize, layout: Layout) -> CardRows {
    let band = in_band(layout);
    let mut rows = form_card_rows(form, room, band);
    rows.lines = widened_all(rows.lines, band, layout);
    rows
}

fn account_spans(account: FormAccount<'_>, layout: Layout) -> Vec<Vec<StyledSpan>> {
    let glyphs = Glyphs::of(layout.glyphs);
    let name = StyledSpan::new(sanitize(account.name), Token::Text);
    vec![
        vec![
            StyledSpan::new(account.kind.label(), billing_token(account.kind.billing())),
            StyledSpan::new(format!(" {} ", glyphs.separator), Token::TextFaint),
            name.clone(),
        ],
        vec![name],
    ]
}

fn done_rows(account: FormAccount<'_>, layout: Layout) -> Vec<StyledLine> {
    let glyphs = Glyphs::of(layout.glyphs);
    let mut spans = vec![
        StyledSpan::new(SIGNED_IN, Look::fg(Token::Text).bold()),
        StyledSpan::new(format!(" {} ", glyphs.separator), Token::TextFaint),
    ];
    let mut named = account_spans(account, layout).swap_remove(0);
    named.insert(named.len() - 1, mark(account.mark, &glyphs));
    spans.extend(named);
    marked_rows(
        StyledSpan::new(glyphs.done, Token::Ok),
        wrap(&spans, lead_room(layout), layout.width_mode),
        layout,
    )
}

#[derive(Clone, Copy, Debug)]
pub enum SecretGoes<'a> {
    Process {
        id: u32,
        command: &'a str,
    },
    Server {
        name: &'a str,
        command: Option<&'a str>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct SecretView<'a> {
    pub name: &'a str,
    pub reason: &'a str,
    pub asked_by: Option<&'a wisp_protocol::AskedBy>,
    pub destination: SecretGoes<'a>,
    pub value: &'a wisp_protocol::Secret,
    pub ends: wisp_protocol::Ends,
    pub pasted: bool,
    pub hidden: bool,
    pub settling: bool,
    pub sent: bool,
    pub problem: &'a str,
}

impl SecretView<'_> {
    pub fn two_forms(&self, layout: Layout) -> bool {
        let room = field_room(copy::SECRET_FIELD, in_band(layout));
        let more = Glyphs::of(layout.glyphs).more;
        wisp_protocol::masked_ends(self.value.expose(), self.ends, self.pasted, room, more)
            != wisp_protocol::masked_ends(
                self.value.expose(),
                wisp_protocol::Ends::default(),
                self.pasted,
                room,
                more,
            )
    }
}

pub fn secret_rows(view: &SecretView<'_>, room: usize, layout: Layout) -> CardRows {
    let glyphs = Glyphs::of(layout.glyphs);
    let destination = copy::secret_destination(view.destination, glyphs.separator);
    let destination = if matches!(view.destination, SecretGoes::Process { .. }) {
        crate::text::fit_end(
            &destination,
            lead_room(in_band(layout)),
            glyphs.more,
            layout.width_mode,
        )
    } else {
        crate::text::clean(&destination)
    };
    let mut prompt = vec![StyledLine::new(vec![StyledSpan::new(
        one_line(view.reason),
        Token::Text,
    )])];
    if let Some(line) = view.asked_by.and_then(copy::asked_by) {
        prompt.push(StyledLine::new(vec![StyledSpan::new(
            line,
            Token::TextMuted,
        )]));
    }
    prompt.push(StyledLine::new(vec![StyledSpan::new(
        destination,
        Token::TextMuted,
    )]));
    let filled = !view.value.expose().is_empty();
    let footer = if view.sent {
        vec![KeyHint::note(format!(
            "{}{}",
            copy::SECRET_SENDING,
            glyphs.more
        ))]
    } else if view.settling {
        vec![KeyHint::note(copy::CARD_SETTLING)]
    } else {
        let mut keys = Vec::new();
        if filled {
            keys.push(KeyHint::new(copy::ENTER_KEY, copy::KEY_SEND));
        }
        if view.two_forms(layout) {
            keys.push(KeyHint::new(
                copy::TAB_KEY,
                if view.hidden {
                    copy::SECRET_SHOW
                } else {
                    copy::SECRET_HIDE
                },
            ));
        }
        if filled {
            keys.push(KeyHint::new("ctrl-u", copy::SECRET_CLEAR));
        }
        keys.push(copy::esc(copy::STOP_WORD));
        keys
    };
    form_rows(
        &FormView {
            title: view.name,
            prompt_lines: &prompt,
            field: Some(FormField {
                label: copy::SECRET_FIELD,
                value: if view.sent {
                    FormValue::Closed
                } else {
                    FormValue::Ends {
                        value: view.value,
                        ends: if view.hidden {
                            wisp_protocol::Ends::default()
                        } else {
                            view.ends
                        },
                        counted: view.pasted,
                    }
                },
            }),
            selected: (!view.sent && !view.settling).then_some(0),
            error: view.problem,
            footer: &footer,
            ..FormView::default()
        },
        room,
        layout,
    )
}

fn field_room(label: &str, layout: Layout) -> usize {
    layout.width.saturating_sub(
        menu_lead(layout) + width(&format!("{}  ", one_line(label)), layout.width_mode) + 1,
    )
}

fn field_row(field: FormField<'_>, view: &CardView<'_>, layout: Layout) -> StyledLine {
    let label = one_line(field.label);
    let focused = match field.value {
        FormValue::Closed => false,
        FormValue::Ends { .. } => view.row == 0,
        _ => true,
    };
    let mut spans = lead(focused, view, layout);
    match field.value {
        FormValue::Text(text) => {
            spans.push(StyledSpan::new(format!("{label}: "), Token::TextMuted));
            spans.push(StyledSpan::new(one_line(text), Token::Text));
        }
        FormValue::Secret(secret) | FormValue::Ends { value: secret, .. }
            if secret.expose().is_empty() =>
        {
            spans.push(StyledSpan::new(format!("{label}  "), Token::TextMuted));
            spans.push(StyledSpan::new(SECRET_PLACEHOLDER, Token::TextFaint));
        }
        FormValue::Secret(secret) => {
            let label = format!("{label}  ");
            let room = field_room(field.label, layout);
            spans.push(StyledSpan::new(label, Token::TextMuted));
            spans.push(masked_secret(
                secret.expose(),
                room,
                Glyphs::of(layout.glyphs).more,
            ));
        }
        FormValue::Ends {
            value,
            ends,
            counted,
        } => {
            spans.push(StyledSpan::new(format!("{label}  "), Token::TextMuted));
            spans.push(masked_secret_ends(
                value.expose(),
                ends,
                counted,
                field_room(field.label, layout),
                Glyphs::of(layout.glyphs).more,
            ));
        }
        FormValue::Closed => spans.push(StyledSpan::new(label, Token::TextMuted)),
    }
    row_of(spans, focused, layout)
}

fn link_forms(text: &str, layout: Layout) -> Vec<String> {
    let glyphs = Glyphs::of(layout.glyphs);
    let text = one_line(text);
    let text = text.as_str();
    let (host, path) = text.split_once('/').unwrap_or((text, ""));
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut out = vec![format!("{} {text}{}", glyphs.arrow, glyphs.more)];
    for kept in (0..segments.len()).rev() {
        let mut shown = format!("{} {host}/", glyphs.arrow);
        for segment in &segments[..kept] {
            shown.push_str(segment);
            shown.push('/');
        }
        shown.push_str(glyphs.more);
        out.push(shown);
    }
    out
}

fn status_spans<'a>(
    browser: &'a str,
    copy: &'a str,
    link: &LinkRow<'_>,
    layout: Layout,
) -> Vec<StyledSpan> {
    let mut spans = vec![];
    if !browser.is_empty() {
        spans.push(StyledSpan::new(browser, Token::TextMuted));
    }
    if !browser.is_empty() && !copy.is_empty() {
        spans.push(StyledSpan::new(
            format!(" {} ", Glyphs::of(layout.glyphs).separator),
            Token::TextFaint,
        ));
    }
    if !copy.is_empty() {
        spans.push(StyledSpan::new(copy, link.copy_look));
    }
    spans
}

fn link_rows(link: &LinkRow<'_>, layout: Layout) -> (CardRows, Option<LinkCells>) {
    let mode = layout.width_mode;
    let indent = menu_lead(layout);
    let room = layout.width.saturating_sub(indent + 1);
    let gap = 3;
    let forms = link
        .text
        .map(|text| link_forms(text, layout))
        .unwrap_or_default();
    let row = |mut spans: Vec<StyledSpan>| {
        spans.insert(0, StyledSpan::plain(" ".repeat(indent)));
        full(spans, layout)
    };
    let mut rows = CardRows::default();
    let both = status_spans(link.browser, link.copy, link, layout);
    let beside = forms
        .iter()
        .find(|form| width(form, mode) + gap + link.slot <= room);
    let shown = match (beside, forms.is_empty()) {
        (_, true) => None,
        (Some(form), false) => Some(form.clone()),
        (None, false) => forms
            .iter()
            .find(|form| width(form, mode) <= room)
            .or(forms.last())
            .cloned(),
    };
    let cells = shown.as_ref().map(|form| LinkCells {
        line: 0,
        start: indent,
        width: width(form, mode).min(room),
    });
    if let Some(form) = &shown {
        let mut spans = vec![StyledSpan::new(form.clone(), Token::Accent)];
        if beside.is_some() {
            spans.push(StyledSpan::plain(" ".repeat(gap)));
            spans.extend(both.clone());
        }
        rows.push(row(spans), Some(CardHit::Link));
    }
    if beside.is_none() || shown.is_none() {
        if link.slot <= room {
            rows.push(row(both), None);
        } else {
            rows.push(row(status_spans(link.browser, "", link, layout)), None);
            rows.push(row(status_spans("", link.copy, link, layout)), None);
        }
    }
    (rows, cells)
}

fn link_card_rows(
    form: &FormView<'_>,
    link: &LinkRow<'_>,
    view: &CardView<'_>,
    mut top: CardRows,
    layout: Layout,
) -> CardRows {
    let (rows, cells) = link_rows(link, layout);
    top.link = cells.map(|cells| LinkCells {
        line: top.len(),
        ..cells
    });
    top.extend(rows);
    if !form.prompt.is_empty() {
        for line in indented(
            &[StyledSpan::new(one_line(form.prompt), Token::TextMuted)],
            layout,
        ) {
            top.push(line, None);
        }
    }
    if let Some(field) = form.field {
        top.push(field_row(field, view, layout), Some(CardHit::Own));
    }
    if !form.error.is_empty() {
        for line in indented(
            &[StyledSpan::new(one_line(form.error), Token::Error)],
            layout,
        ) {
            top.push(line, None);
        }
    }
    top.push(hint_row(form.footer, layout), None);
    top
}

fn form_card_rows(form: &FormView<'_>, room: usize, layout: Layout) -> CardRows {
    if room == 0 {
        return CardRows::default();
    }
    let questions = [form_question(form.title, form.prompt, form.options)];
    let view = form_view(&questions, form.selected);
    let mut top = CardRows::default();
    if let Some(refused) = form.refused {
        let glyphs = Glyphs::of(layout.glyphs);
        let rows = same_account_rows(refused, lead_room(layout), &glyphs);
        for line in marked_rows(StyledSpan::new(glyphs.failed, Token::Error), rows, layout) {
            top.push(line, None);
        }
        top.push(hint_row(form.footer, layout), None);
        return top;
    }
    match form.account {
        Some(account) if form.done => {
            for line in done_rows(account, layout) {
                top.push(line, None);
            }
        }
        Some(account) => top.push(
            header_line(
                vec![bold(one_line(form.title))],
                account_spans(account, layout),
                layout,
            ),
            None,
        ),
        None => {
            let plain = [bold(one_line(form.title))];
            let title = form.title_line.unwrap_or(&plain);
            let rows = wrap(title, lead_room(layout), layout.width_mode);
            for (index, row) in rows.into_iter().enumerate() {
                let lines = match index {
                    0 => vec![header_line(row.spans, vec![], layout)],
                    _ => indented(&row.spans, layout),
                };
                for line in lines {
                    top.push(line, None);
                }
            }
        }
    }
    if !form.note.is_empty() {
        let glyphs = Glyphs::of(layout.glyphs);
        let note = mark_line(&one_line(form.note), Token::TextMuted, None, &glyphs);
        for line in indented(&note.spans, layout) {
            top.push(line, None);
        }
    }
    if let Some(link) = &form.link {
        return link_card_rows(form, link, &view, top, layout);
    }
    let mut body = CardRows::default();
    let mut focus = (0, 0);
    for index in 0..form.options.len() {
        let account = form.option_marks.get(index).copied();
        let rows = option_rows(&view, 0, index, &questions[0], account, layout);
        if Some(index) == form.selected {
            focus = (body.len(), body.len() + rows.len());
        }
        body.extend(rows);
    }
    if let Some(field) = form.field {
        focus = (body.len(), body.len() + 1);
        body.push(field_row(field, &view, layout), Some(CardHit::Own));
    }
    let mut prompt = if form.prompt.is_empty() {
        form.prompt_lines
            .iter()
            .flat_map(|line| indented(&line.spans, layout))
            .collect()
    } else {
        question_lines(&view, 0, layout)
    };
    if !form.error.is_empty() {
        let lines = indented(
            &[StyledSpan::new(one_line(form.error), Token::Error)],
            layout,
        );
        if matches!(form.field.map(|f| f.value), Some(FormValue::Ends { .. })) {
            for line in lines {
                body.push(line, None);
            }
        } else {
            prompt.extend(lines);
        }
    }
    fit_rows(
        top,
        prompt,
        body,
        focus,
        hint_row(form.footer, layout),
        room,
        layout,
    )
}

#[cfg(test)]
mod tests;
