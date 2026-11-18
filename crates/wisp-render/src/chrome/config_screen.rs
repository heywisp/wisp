use super::band::{band_cols, row_of, selection_mark};
use super::banner::WORDMARK;
use super::menu::{fit_keys, key_spans};
use super::one_line;
use super::question::{CardRows, FormView, GAP_ROWS, answer_options, form_rows};
use super::row::{KEY_ROW, KeyHint, clip, edge, full};
use crate::markdown::Layout;
use crate::motion::OrbFrame;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{WidthMode, fit_end, fit_middle, padded, spans_width, width, wrap, wrap_words};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy::TAB_KEY;

pub const MIN_COLUMNS: u16 = 40;
pub const MIN_ROWS: u16 = 12;
pub const DETAIL_ROWS: usize = 3;
pub const MARK_FROM: usize = 60;
pub const LABEL_MAX: usize = 24;
pub const OFTEN_USED: &str = "Often used";
pub const SCREEN_TITLE: &str = "Settings";
pub const LOADING: &str = "Loading settings";
pub const FILTER_LABEL: &str = "Filter";
pub const FILTER_PLACEHOLDER: &str = "type / to filter";
pub const TOO_SMALL: &str = "Make the terminal 40x12 for settings, or use wisp config.";
pub const FOR_LABEL: &str = "For";
pub const EDIT_LABEL: &str = "New value";
pub use super::account::THIS_CHAT;
pub const SAVED: &str = "Saved";
pub const WISPS_OWN: &str = "Wisp's own";
pub const CHOOSE_LABEL: &str = "Choose";

const LEAD: usize = 3;
const GAP: usize = 2;
const FIELD: usize = 15;
const LABEL_MIN: usize = 14;
const TITLE_GAP: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRow {
    pub label: String,
    pub value: String,
    pub marked: bool,
    pub numeric: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Heading(String),
    Gap,
    Row(ListRow),
    Note(String),
    Aside(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerLine {
    pub word: String,
    pub value: String,
    pub origin: String,
    pub winner: bool,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    Empty,
    Preview(String),
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditView {
    Choices {
        label: String,
        choices: Vec<String>,
        at: usize,
    },
    Text {
        text: String,
        cursor: usize,
        unit: String,
        check: Check,
        sites: Vec<String>,
        site: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskView {
    pub question: String,
    pub options: [&'static str; 2],
    pub selected: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detail {
    pub lead: String,
    pub title: String,
    pub name: String,
    pub line: String,
    pub fallback: String,
    pub layers: Vec<LayerLine>,
    pub default: String,
    pub default_wins: bool,
    pub default_origin: String,
    pub choices: Vec<String>,
    pub applies: String,
    pub notes: Vec<String>,
    pub keys: Vec<KeyHint>,
    pub edit: Option<EditView>,
    pub ask: Option<AskView>,
    pub label_width: usize,
}

pub fn fits(columns: u16, rows: u16) -> bool {
    columns >= MIN_COLUMNS && rows >= MIN_ROWS
}

pub fn fold(text: &str, layout: Layout) -> String {
    Glyphs::of(layout.glyphs).fold(&one_line(text))
}

fn cut(text: &str, room: usize, layout: Layout) -> String {
    fit_end(
        text,
        room,
        Glyphs::of(layout.glyphs).more,
        layout.width_mode,
    )
}

fn middle(text: &str, room: usize, layout: Layout) -> String {
    fit_middle(
        text,
        room,
        Glyphs::of(layout.glyphs).more,
        layout.width_mode,
    )
}

fn prefixed(mut prefix: Vec<StyledSpan>, row: StyledLine) -> StyledLine {
    prefix.extend(row.spans);
    StyledLine::new(prefix)
}

pub fn screen_rule(mark: Option<&str>, layout: Layout) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let tail = mark.map_or_else(String::new, |mark| format!(" {mark} {}", glyphs.rule));
    StyledLine::new(vec![StyledSpan::new(
        format!(
            "{}{}",
            glyphs
                .rule
                .repeat(layout.width.saturating_sub(width(&tail, layout.width_mode))),
            tail
        ),
        Token::Line,
    )])
}

pub fn screen_header(logo: &OrbFrame, facts: &[String], layout: Layout) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let mut spans = vec![StyledSpan::plain(" ")];
    spans.extend(logo.spans());
    spans.push(StyledSpan::plain(" "));
    spans.push(StyledSpan::new(WORDMARK, Look::fg(Token::Text).bold()));
    spans.push(StyledSpan::plain(" ".repeat(TITLE_GAP)));
    spans.push(StyledSpan::new(SCREEN_TITLE, Token::Text));
    let edge = edge(layout);
    let spans = clip(spans, edge, glyphs.more, mode);
    let used = spans_width(&spans, mode);
    let sep = format!(" {} ", glyphs.separator);
    let mut shown: Vec<String> = facts.iter().map(|f| fold(f, layout)).collect();
    let right = loop {
        let text = shown.join(&sep);
        if shown.is_empty() || used + GAP + width(&text, mode) <= edge {
            break text;
        }
        shown.pop();
    };
    let mut out = spans;
    if !right.is_empty() {
        let pad = edge.saturating_sub(used + width(&right, mode));
        out.push(StyledSpan::plain(" ".repeat(pad)));
        out.push(StyledSpan::new(right, Token::TextMuted));
    }
    StyledLine::new(out)
}

pub fn filter_line(filter: &str, editing: bool, layout: Layout) -> (StyledLine, Option<usize>) {
    let (spans, cursor) = filter_spans(filter, editing, FILTER_PLACEHOLDER, layout);
    (full(spans, layout), cursor)
}

pub(super) fn filter_spans(
    filter: &str,
    editing: bool,
    placeholder: &str,
    layout: Layout,
) -> (Vec<StyledSpan>, Option<usize>) {
    let mode = layout.width_mode;
    let mut spans = vec![
        StyledSpan::plain(" "),
        StyledSpan::new(FILTER_LABEL, Token::TextMuted),
        StyledSpan::plain(" ".repeat(GAP)),
    ];
    let lead = spans_width(&spans, mode);
    let text = one_line(filter);
    if text.is_empty() && !editing {
        spans.push(StyledSpan::new(placeholder, Token::TextFaint));
        return (spans, None);
    }
    let room = layout.width.saturating_sub(lead + 2);
    let shown = if width(&text, mode) > room {
        middle(&text, room, layout)
    } else {
        text
    };
    let cursor = lead + width(&shown, mode);
    let at = editing.then_some(shown.len());
    let caret = Look::default().on(Token::Accent);
    spans.extend(super::caret::field(&shown, at, Token::Text.into(), caret));
    (spans, editing.then_some(cursor))
}

pub fn screen_note(text: &str, token: Token, layout: Layout) -> Vec<StyledLine> {
    let measure = layout.width.saturating_sub(2).max(1);
    wrap(
        &[StyledSpan::new(fold(text, layout), token)],
        measure,
        layout.width_mode,
    )
    .into_iter()
    .map(|row| prefixed(vec![StyledSpan::plain(" ")], row))
    .collect()
}

fn hint_lines(hints: &[KeyHint], layout: Layout) -> Vec<StyledLine> {
    let mode = layout.width_mode;
    let measure = layout.width.saturating_sub(2).max(1);
    let gap = StyledSpan::new(
        format!(" {} ", Glyphs::of(layout.glyphs).separator),
        Token::TextFaint,
    );
    let mut rows: Vec<StyledLine> = vec![];
    let mut row: Vec<StyledSpan> = vec![];
    for hint in hints {
        let spans = hint.map(|s| fold(s, layout)).spans();
        let wide = spans_width(&spans, mode);
        let joined = spans_width(&row, mode) + width(&gap.text, mode) + wide;
        if !row.is_empty() && joined <= measure {
            row.push(gap.clone());
            row.extend(spans);
            continue;
        }
        if !row.is_empty() {
            rows.push(StyledLine::new(std::mem::take(&mut row)));
        }
        if wide <= measure {
            row = spans;
        } else {
            rows.extend(wrap_words(&spans, measure, mode));
        }
    }
    if !row.is_empty() {
        rows.push(StyledLine::new(row));
    }
    rows.into_iter()
        .map(|row| prefixed(vec![StyledSpan::plain(" ")], row))
        .collect()
}

fn mark_width(layout: Layout) -> usize {
    width(&fold(THIS_CHAT, layout), layout.width_mode)
}

fn value_width(value: &str, layout: Layout) -> usize {
    width(
        &fold(
            value
                .split(crate::transcript::copy::SEP)
                .next()
                .unwrap_or_default(),
            layout,
        ),
        layout.width_mode,
    )
}

fn numeric_width(entries: &[Entry], layout: Layout) -> usize {
    entries
        .iter()
        .filter_map(|e| match e {
            Entry::Row(row) if row.numeric => Some(value_width(&row.value, layout)),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

pub fn label_width(entries: &[Entry], layout: Layout) -> usize {
    let widest = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Row(row) if !row.value.is_empty() => {
                Some(width(&fold(&row.label, layout), layout.width_mode))
            }
            Entry::Row(_) => None,
            Entry::Heading(_) | Entry::Gap | Entry::Note(_) | Entry::Aside(_) => None,
        })
        .max()
        .unwrap_or(0)
        .min(LABEL_MAX);
    let numeric_w = numeric_width(entries, layout);
    let value_w = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Row(row) => Some(
                width(&fold(&row.value, layout), layout.width_mode)
                    + if row.numeric {
                        numeric_w.saturating_sub(value_width(&row.value, layout))
                    } else {
                        0
                    },
            ),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let tail = if marked(layout.width) {
        GAP + mark_width(layout)
    } else {
        0
    };
    let room = band_cols(layout.width).saturating_sub(LEAD + GAP + tail + 1);
    let floor = widest.min(LABEL_MIN + layout.width.saturating_sub(usize::from(MIN_COLUMNS)) / 2);
    widest.min(room.saturating_sub(value_w).max(floor))
}

pub fn marked(columns: usize) -> bool {
    columns >= MARK_FROM
}

pub fn list_lines(
    entries: &[Entry],
    selected: Option<usize>,
    (top, rows): (usize, usize),
    marked: bool,
    layout: Layout,
) -> Vec<StyledLine> {
    let mode = layout.width_mode;
    let label_w = label_width(entries, layout);
    let tail = if marked {
        GAP + mark_width(layout) + 1
    } else {
        1
    };
    let value_room = band_cols(layout.width).saturating_sub(LEAD + label_w + GAP + tail);
    let numeric_w = numeric_width(entries, layout).min(value_room);
    entries
        .iter()
        .enumerate()
        .skip(top)
        .take(rows)
        .map(|(i, entry)| match entry {
            Entry::Heading(title) => StyledLine::new(vec![
                StyledSpan::plain(" "),
                StyledSpan::new(fold(title, layout), Look::fg(Token::Text).bold()),
            ]),
            Entry::Gap => StyledLine::default(),
            Entry::Note(text) => note_line(text, Token::Error, layout),
            Entry::Aside(text) => note_line(text, Token::TextMuted, layout),
            Entry::Row(row) => {
                let chosen = selected == Some(i);
                let mark = if chosen {
                    selection_mark(layout)
                } else {
                    StyledSpan::plain(" ")
                };
                let (label_w, value_room, gap) = if row.value.is_empty() {
                    (label_w + GAP + value_room, 0, 0)
                } else {
                    (label_w, value_room, GAP)
                };
                let label = cut(&fold(&row.label, layout), label_w, layout);
                let value = fold(&row.value, layout);
                let padding = if row.numeric {
                    numeric_w.saturating_sub(value_width(&row.value, layout))
                } else {
                    0
                };
                let value = cut(
                    &format!("{}{value}", " ".repeat(padding)),
                    value_room,
                    layout,
                );
                let mut spans = vec![
                    StyledSpan::new(" ", Look::default()),
                    mark,
                    StyledSpan::new(" ", Look::default()),
                    StyledSpan::new(padded(&label, label_w, mode), Look::fg(Token::Text)),
                    StyledSpan::new(" ".repeat(gap), Look::default()),
                    StyledSpan::new(padded(&value, value_room, mode), Look::fg(Token::Text)),
                ];
                if marked {
                    let word = fold(THIS_CHAT, layout);
                    let source = if row.marked { word.as_str() } else { "" };
                    let pad = GAP + mark_width(layout).saturating_sub(width(source, mode));
                    spans.push(StyledSpan::new(" ".repeat(pad), Look::default()));
                    spans.push(StyledSpan::new(source, Look::fg(Token::TextMuted)));
                }
                row_of(spans, chosen, layout)
            }
        })
        .collect()
}

fn note_line(text: &str, token: Token, layout: Layout) -> StyledLine {
    StyledLine::new(vec![
        StyledSpan::plain(" ".repeat(LEAD)),
        StyledSpan::new(
            cut(
                &fold(text, layout),
                layout.width.saturating_sub(LEAD + 1),
                layout,
            ),
            token,
        ),
    ])
}

fn field(word: &str, body: Vec<StyledSpan>, column: usize, layout: Layout) -> Vec<StyledLine> {
    let mode = layout.width_mode;
    let measure = layout.width.saturating_sub(1 + column + 1).max(1);
    let rows = wrap_words(&body, measure, mode);
    let hang = " ".repeat(1 + column);
    let word = cut(word, column.saturating_sub(GAP), layout);
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            let lead = if i == 0 {
                vec![
                    StyledSpan::plain(" "),
                    StyledSpan::new(padded(&word, column, mode), Token::TextMuted),
                ]
            } else {
                vec![StyledSpan::plain(hang.clone())]
            };
            prefixed(lead, row)
        })
        .collect()
}

fn title_lines(detail: &Detail, layout: Layout) -> Vec<StyledLine> {
    let measure = layout.width.saturating_sub(2).max(1);
    let spans = vec![
        StyledSpan::new(
            format!("{}: ", fold(&detail.lead, layout)),
            Token::TextMuted,
        ),
        StyledSpan::new(fold(&detail.title, layout), Look::fg(Token::Text).bold()),
    ];
    wrap_words(&spans, measure, layout.width_mode)
        .into_iter()
        .map(|row| prefixed(vec![StyledSpan::plain(" ")], row))
        .collect()
}

fn numeric(value: &str) -> bool {
    value.chars().any(|c| c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | '%' | '$' | '-'))
}

fn layer_body(line: &LayerLine, value_w: usize, layout: Layout) -> Vec<StyledSpan> {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let value_look = if line.winner {
        Token::Text
    } else {
        Token::TextMuted
    };
    let value = fold(&line.value, layout);
    let value = if numeric(&value) {
        format!(
            "{}{value}",
            " ".repeat(value_w.saturating_sub(width(&value, mode)))
        )
    } else {
        padded(&value, value_w, mode)
    };
    let mut spans = vec![StyledSpan::new(value, value_look)];
    let mut origin = fold(&line.origin, layout);
    if let Some(note) = &line.note {
        if !origin.is_empty() {
            origin.push_str(&format!(" {} ", glyphs.separator));
        }
        origin.push_str(&fold(note, layout));
    }
    if !origin.is_empty() {
        spans.push(StyledSpan::plain(" ".repeat(GAP)));
        spans.push(StyledSpan::new(origin, Token::TextMuted));
    }
    spans
}

fn window(text: &str, cursor: usize, room: usize, mode: WidthMode) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let room = room.max(1);
    let wide = |part: &[char]| width(&part.iter().collect::<String>(), mode);
    let mut start = 0;
    while start < cursor && wide(&chars[start..cursor]) + 1 > room {
        start += 1;
    }
    let mut end = cursor;
    while end < chars.len() && wide(&chars[start..=end]) < room {
        end += 1;
    }
    let shown: String = chars[start..end].iter().collect();
    (shown, wide(&chars[start..cursor]))
}

fn choices_line(
    label: &str,
    key: Option<&str>,
    choices: &[String],
    at: usize,
    layout: Layout,
) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let mut spans = vec![
        StyledSpan::plain(" "),
        StyledSpan::new(
            padded(&fold(label, layout), FIELD, layout.width_mode),
            Token::TextMuted,
        ),
    ];
    let chosen = |choice: &str| {
        let look = Look::fg(Token::Text).bold().on(Token::AccentTint);
        [
            StyledSpan::new(glyphs.prompt, Look::fg(Token::Accent).on(Token::AccentTint)),
            StyledSpan::new(format!(" {}", fold(choice, layout)), look),
        ]
    };
    let mut all = spans.clone();
    for (i, choice) in choices.iter().enumerate() {
        if i > 0 {
            all.push(StyledSpan::plain(" ".repeat(GAP)));
        }
        if i == at {
            all.extend(chosen(choice));
        } else {
            all.push(StyledSpan::new(
                format!("  {}", fold(choice, layout)),
                Token::TextMuted,
            ));
        }
    }
    if spans_width(&all, layout.width_mode) <= edge(layout) || choices.len() < 2 {
        return StyledLine::new(clip(all, layout.width, glyphs.more, layout.width_mode));
    }
    if let Some(key) = key {
        spans[1] = StyledSpan::new(
            padded(&format!("{label} ({key})"), FIELD, layout.width_mode),
            Token::TextMuted,
        );
    }
    if let Some(choice) = choices.get(at) {
        spans.extend(chosen(choice));
        spans.push(StyledSpan::new(
            format!("  {} of {}", at + 1, choices.len()),
            Token::TextMuted,
        ));
    }
    StyledLine::new(clip(spans, layout.width, glyphs.more, layout.width_mode))
}

fn text_line(text: &str, cursor: usize, layout: Layout) -> (StyledLine, usize) {
    let mode = layout.width_mode;
    let lead = 1 + FIELD;
    let room = layout.width.saturating_sub(lead + 1);
    let (shown, at) = window(&one_line(text), cursor, room, mode);
    let spans = vec![
        StyledSpan::plain(" "),
        StyledSpan::new(padded(EDIT_LABEL, FIELD, mode), Token::TextMuted),
        StyledSpan::new(
            padded(&shown, room, mode),
            Look::fg(Token::Text).on(Token::AccentTint),
        ),
    ];
    (StyledLine::new(spans), lead + at)
}

fn check_spans(unit: &str, check: &Check, layout: Layout) -> Vec<StyledSpan> {
    let mut spans = match check {
        Check::Empty => vec![],
        Check::Preview(preview) => vec![StyledSpan::new(fold(preview, layout), Token::Text)],
        Check::Invalid(rule) => return vec![StyledSpan::new(fold(rule, layout), Token::Error)],
    };
    if unit.is_empty() {
        return spans;
    }
    let hint = if spans.is_empty() {
        fold(unit, layout)
    } else {
        format!(
            " {} {}",
            Glyphs::of(layout.glyphs).separator,
            fold(unit, layout)
        )
    };
    if spans_width(&spans, layout.width_mode) + width(&hint, layout.width_mode)
        <= layout.width.saturating_sub(1 + FIELD)
    {
        spans.push(StyledSpan::new(hint, Token::TextMuted));
    }
    spans
}

fn hung(spans: Vec<StyledSpan>, layout: Layout) -> Vec<StyledLine> {
    let measure = layout.width.saturating_sub(1 + FIELD + 1).max(1);
    let hang = " ".repeat(1 + FIELD);
    wrap_words(&spans, measure, layout.width_mode)
        .into_iter()
        .map(|row| prefixed(vec![StyledSpan::plain(hang.clone())], row))
        .collect()
}

fn edit_lines(edit: &EditView, layout: Layout) -> (Vec<StyledLine>, Option<(usize, usize)>) {
    match edit {
        EditView::Choices { label, choices, at } => {
            (vec![choices_line(label, None, choices, *at, layout)], None)
        }
        EditView::Text {
            text,
            cursor,
            unit,
            check,
            sites,
            site,
        } => {
            let (line, x) = text_line(text, *cursor, layout);
            let mut out = vec![line];
            if sites.len() > 1 {
                out.push(choices_line(FOR_LABEL, Some(TAB_KEY), sites, *site, layout));
            }
            out.extend(hung(check_spans(unit, check, layout), layout));
            (out, Some((0, x)))
        }
    }
}

fn ask_rows(ask: &AskView, keys: &[KeyHint], room: usize, layout: Layout) -> CardRows {
    let options = answer_options(&ask.options);
    let form = FormView {
        title: &ask.question,
        options: &options,
        selected: ask.selected,
        footer: keys,
        ..FormView::default()
    };
    form_rows(&form, room, layout)
}

pub fn detail_lines(detail: &Detail, layout: Layout) -> Vec<StyledLine> {
    detail_view(detail, usize::MAX, layout).0
}

pub fn detail_view(
    detail: &Detail,
    rows: usize,
    layout: Layout,
) -> (Vec<StyledLine>, Option<(usize, usize)>) {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let sep = format!(" {} ", glyphs.separator);
    let mut out = title_lines(detail, layout);
    out.push(StyledLine::new(vec![]));
    let column = if detail.layers.is_empty() {
        FIELD
    } else {
        (detail.label_width + GAP).min(layout.width / 2).max(FIELD)
    };
    let room = layout.width.saturating_sub(1 + column + 1);
    let value_w = detail
        .layers
        .iter()
        .map(|l| l.value.as_str())
        .chain([detail.default.as_str()])
        .map(|value| width(&fold(value, layout), mode))
        .max()
        .unwrap_or(0)
        .min(room / 2);
    for line in &detail.layers {
        out.extend(field(
            &line.word,
            layer_body(line, value_w, layout),
            column,
            layout,
        ));
    }
    if !detail.default.is_empty() {
        out.extend(field(
            WISPS_OWN,
            layer_body(
                &LayerLine {
                    word: String::new(),
                    value: detail.default.clone(),
                    origin: detail.default_origin.clone(),
                    winner: detail.default_wins,
                    note: None,
                },
                value_w,
                layout,
            ),
            column,
            layout,
        ));
    }
    out.push(StyledLine::new(vec![]));
    if !detail.choices.is_empty() {
        let choices: Vec<String> = detail.choices.iter().map(|c| fold(c, layout)).collect();
        out.extend(field(
            "Choices",
            vec![StyledSpan::new(choices.join(&sep), Token::Text)],
            column,
            layout,
        ));
    }
    if !detail.name.is_empty() {
        out.extend(field(
            "Name",
            vec![StyledSpan::new(
                fold(&detail.name, layout),
                Token::TextMuted,
            )],
            column,
            layout,
        ));
    }
    let mut cursor = None;
    if let Some(edit) = &detail.edit {
        out.push(StyledLine::new(vec![]));
        let (lines, at) = edit_lines(edit, layout);
        cursor = at.map(|(y, x)| (out.len() + y, x));
        out.extend(lines);
    }
    for note in &detail.notes {
        out.push(StyledLine::new(vec![]));
        out.extend(screen_note(note, Token::Text, layout));
    }
    if let Some(ask) = &detail.ask {
        out.push(StyledLine::new(vec![]));
        out.extend(ask_rows(ask, &detail.keys, rows, layout).lines);
    } else if !detail.keys.is_empty() {
        out.push(StyledLine::new(vec![]));
        out.extend(hint_lines(&detail.keys, layout));
    }
    (out, cursor)
}

pub fn detail_block(detail: &Detail, layout: Layout) -> Vec<StyledLine> {
    detail_block_view(detail, usize::MAX, layout).0
}

pub fn detail_block_view(
    detail: &Detail,
    rows: usize,
    layout: Layout,
) -> (Vec<StyledLine>, Option<(usize, usize)>) {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let room = edge(layout);
    let line = |spans: Vec<StyledSpan>| {
        let mut all = vec![StyledSpan::plain(" ")];
        all.extend(spans);
        StyledLine::new(clip(all, room, glyphs.more, mode))
    };
    let title = vec![
        StyledSpan::new(
            format!("{}: ", fold(&detail.lead, layout)),
            Token::TextMuted,
        ),
        StyledSpan::new(fold(&detail.title, layout), Look::fg(Token::Text).bold()),
    ];
    let folded: Vec<KeyHint> = detail
        .keys
        .iter()
        .map(|h| h.map(|s| fold(s, layout)))
        .collect();
    let spans = |kept: &[KeyHint]| key_spans(kept, None, KEY_ROW, "", layout);
    let cost: Vec<KeyHint> = detail
        .applies
        .split(crate::transcript::copy::SEP)
        .filter(|part| !part.is_empty())
        .map(|part| KeyHint::note(fold(part, layout)))
        .collect();
    let actions: Vec<KeyHint> = folded
        .iter()
        .filter(|hint| !cost.contains(hint))
        .cloned()
        .collect();
    let actions = fit_keys(&actions, "", room.saturating_sub(1), |kept| {
        spans_width(&spans(kept), mode)
    });
    let with_cost: Vec<KeyHint> = folded
        .iter()
        .filter(|hint| cost.contains(hint) || actions.contains(hint))
        .cloned()
        .collect();
    let kept = if spans_width(&spans(&with_cost), mode) < room {
        with_cost
    } else {
        actions
    };
    let keys = line(spans(&kept));
    match &detail.edit {
        Some(EditView::Choices { label, choices, at }) => {
            let under = vec![StyledSpan::new(
                fold(detail.notes.first().unwrap_or(&detail.line), layout),
                Token::Text,
            )];
            (
                vec![
                    line(title),
                    choices_line(label, None, choices, *at, layout),
                    line(under),
                    StyledLine::default(),
                ],
                None,
            )
        }
        Some(EditView::Text {
            text,
            cursor,
            unit,
            check,
            sites,
            site,
        }) => {
            let (field, x) = text_line(text, *cursor, layout);
            let mut under = vec![StyledSpan::plain(" ".repeat(FIELD))];
            under.extend(check_spans(unit, check, layout));
            let mut lines = vec![line(title), field];
            if sites.len() > 1 {
                lines.push(choices_line(FOR_LABEL, Some(TAB_KEY), sites, *site, layout));
            }
            lines.push(line(under));
            if sites.len() < 2 {
                lines.push(StyledLine::default());
            }
            (lines, Some((1, x)))
        }
        None => {
            let lines = if let Some(ask) = &detail.ask {
                let all = ask_rows(ask, &detail.keys, usize::MAX, layout).len();
                ask_rows(
                    ask,
                    &detail.keys,
                    all.saturating_sub(GAP_ROWS).min(rows),
                    layout,
                )
                .lines
            } else {
                let cost = cost
                    .iter()
                    .any(|hint| folded.contains(hint) && !kept.contains(hint));
                let under = if !detail.line.is_empty() {
                    &detail.line
                } else if cost {
                    &detail.applies
                } else {
                    &detail.fallback
                };
                vec![
                    line(title),
                    line(vec![StyledSpan::new(fold(under, layout), Token::Text)]),
                    keys,
                ]
            };
            (lines, None)
        }
    }
}

#[cfg(test)]
mod tests;
