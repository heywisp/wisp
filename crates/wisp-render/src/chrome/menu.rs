use std::ops::Range;

use super::account::{fit_parts, mark_line};
use super::band::{in_band, selected_row, selection_lead, widened, widened_all};
use super::one_line;
use super::row::{KEY_ROW, KeyHint, KeyLook, clip, fit_hints, pad};
use super::summary::{down_arrow, up_arrow};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{fit_end, spans_text, spans_width, width, wrap_plain, wrap_words};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

pub const MENU_MAX_ROWS: usize = 12;
const COLUMN_GAP: usize = 2;
const DETAIL_MIN_WIDTH: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub current: bool,
    pub label: String,
    pub detail: String,
    pub effect: String,
    pub enabled: bool,
    pub line: Option<StyledLine>,
    pub mark: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Faint,
    Note,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub at: usize,
    pub short: String,
    pub after: String,
    pub tone: Token,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Under {
    pub mark: Option<u8>,
    pub first: Vec<String>,
    pub parts: Vec<(String, String)>,
    pub after: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    pub heads: Vec<Head>,
    pub under: Option<Under>,
    pub reserve: Vec<Under>,
    pub effect_from: usize,
}

impl Shape {
    fn head(&self, at: usize) -> Option<&Head> {
        self.heads.iter().find(|h| h.at == at)
    }

    fn head_above(&self, at: usize) -> Option<usize> {
        self.heads.iter().map(|h| h.at).filter(|h| *h <= at).max()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    pub titles: [String; 3],
    pub choices: Vec<Choice>,
    pub selected: Option<usize>,
    pub notes: Vec<(String, Tone)>,
    pub shape: Shape,
}

impl Menu {
    fn effect_title(&self, width: usize) -> &str {
        if width >= self.shape.effect_from {
            &self.titles[2]
        } else {
            ""
        }
    }

    fn amounts_only(&self) -> bool {
        self.titles[0].is_empty() && self.titles[1].is_empty() && !self.titles[2].is_empty()
    }
}

fn pad_text(text: &str, to: usize, layout: Layout) -> String {
    let w = width(text, layout.width_mode);
    format!("{text}{}", " ".repeat(to.saturating_sub(w)))
}

fn pad_start(text: &str, to: usize, layout: Layout) -> String {
    let w = width(text, layout.width_mode);
    format!("{}{text}", " ".repeat(to.saturating_sub(w)))
}

fn cut(text: &str, room: usize, layout: Layout) -> String {
    if width(text, layout.width_mode) <= room {
        return text.to_owned();
    }
    let glyphs = Glyphs::of(layout.glyphs);
    let kept = fit_end(text, room, glyphs.more, layout.width_mode);
    let body = kept.strip_suffix(glyphs.more).unwrap_or(&kept);
    let body = body
        .trim_end()
        .trim_end_matches(glyphs.separator)
        .trim_end();
    format!("{body}{}", glyphs.more)
}

fn detail_column(detail_w: usize, title_w: usize, room: Option<usize>) -> Option<usize> {
    let room = room?;
    if detail_w <= room {
        return Some(detail_w);
    }
    let floor = title_w.saturating_sub(COLUMN_GAP - 1).max(DETAIL_MIN_WIDTH);
    (room >= floor).then_some(room)
}

fn trimmed_before_more(mut spans: Vec<StyledSpan>) -> Vec<StyledSpan> {
    let Some(more) = spans.pop() else {
        return spans;
    };
    while let Some(last) = spans.last_mut() {
        let kept = last.text.trim_end().len();
        last.text.truncate(kept);
        if !last.text.is_empty() {
            break;
        }
        spans.pop();
    }
    spans.push(more);
    spans
}

fn full(spans: Vec<StyledSpan>, look: Look, layout: Layout) -> StyledLine {
    let more = Glyphs::of(layout.glyphs).more;
    let room = layout.width;
    let wide = spans_width(&spans, layout.width_mode);
    let mut spans = clip(spans, room, more, layout.width_mode);
    if wide > room && room >= width(more, layout.width_mode) {
        spans = trimmed_before_more(spans);
    }
    pad(spans, look, layout)
}

pub fn menu_lead(layout: Layout) -> usize {
    spans_width(&selection_lead(false, layout), layout.width_mode)
}

pub fn menu_rows(menu: &Menu, room: usize, layout: Layout) -> Vec<StyledLine> {
    menu_lines(menu, room, layout).lines
}

struct Parts {
    lead: String,
    header: StyledLine,
    choices: Vec<StyledLine>,
    notes: Vec<StyledLine>,
}

fn parts(menu: &Menu, header_ink: Token, layout: Layout) -> Parts {
    let band = in_band(layout);
    let Parts {
        lead,
        header,
        choices,
        notes,
    } = banded_parts(menu, header_ink, band);
    Parts {
        lead,
        header: widened(header, band, layout),
        choices: widened_all(choices, band, layout),
        notes: widened_all(notes, band, layout),
    }
}

fn banded_parts(menu: &Menu, header_ink: Token, layout: Layout) -> Parts {
    let glyphs = Glyphs::of(layout.glyphs);
    let mark = glyphs.done;
    let current_w = width(mark, layout.width_mode);
    let clean = |s: &str| one_line(s);
    let lead = " ".repeat(menu_lead(layout));
    let measure = layout.width.saturating_sub(lead.len()).max(1);
    let under_lines = |under: &Under| {
        let mut lines = fit_parts(&under.first, &under.parts, "", &glyphs, measure);
        for after in &under.after {
            lines.extend(wrap_plain(after, measure, "", "", &glyphs));
        }
        lines
    };
    let mut under_text: Vec<(String, Option<u8>)> = menu
        .shape
        .under
        .iter()
        .flat_map(|under| {
            under_lines(under)
                .into_iter()
                .map(|line| (line, under.mark))
        })
        .collect();
    let tallest = menu
        .shape
        .reserve
        .iter()
        .map(|under| under_lines(under).len())
        .max()
        .unwrap_or(0);
    under_text.resize(under_text.len().max(tallest), (String::new(), None));
    let under: Vec<StyledLine> = under_text
        .into_iter()
        .map(|(text, mark)| {
            let mut spans = vec![StyledSpan::plain(lead.clone())];
            spans.extend(mark_line(&clean(&text), Token::TextMuted, mark, &glyphs).spans);
            full(spans, Look::default(), layout)
        })
        .collect();
    let shows_effects = layout.width >= menu.shape.effect_from;
    let effect_of = |c: &Choice| {
        if shows_effects {
            clean(&c.effect)
        } else {
            String::new()
        }
    };
    let notes: Vec<StyledLine> = under
        .into_iter()
        .chain(
            menu.notes
                .iter()
                .flat_map(|(text, tone)| {
                    let token = match tone {
                        Tone::Faint => Token::TextFaint,
                        Tone::Note => Token::Text,
                    };
                    wrap_words(
                        &[StyledSpan::new(clean(text), token)],
                        measure,
                        layout.width_mode,
                    )
                })
                .map(|row| {
                    let mut spans = vec![StyledSpan::plain(lead.clone())];
                    spans.extend(row.spans);
                    full(spans, Look::default(), layout)
                }),
        )
        .collect();
    let label_w = menu
        .choices
        .iter()
        .filter(|c| c.current || !c.detail.is_empty() || !effect_of(c).is_empty())
        .map(|c| {
            let own = width(&clean(&c.label), layout.width_mode);
            if c.current { own + 1 + current_w } else { own }
        })
        .chain([width(&menu.titles[0], layout.width_mode)])
        .max()
        .unwrap_or(0);
    let kinds_w = menu
        .shape
        .heads
        .iter()
        .filter_map(|h| menu.choices.get(h.at))
        .map(|c| width(&clean(&c.detail), layout.width_mode))
        .max()
        .unwrap_or(0);
    let held_w = |head: &Head| kinds_w + COLUMN_GAP + width(&clean(&head.after), layout.width_mode);
    let detail_w = menu
        .choices
        .iter()
        .map(|c| width(&clean(&c.detail), layout.width_mode))
        .chain(
            menu.shape
                .heads
                .iter()
                .filter(|h| !h.after.is_empty())
                .map(held_w),
        )
        .chain([width(&menu.titles[1], layout.width_mode)])
        .max()
        .unwrap_or(0);
    let effect_title = clean(menu.effect_title(layout.width));
    let effect_w = menu
        .choices
        .iter()
        .map(|c| width(&effect_of(c), layout.width_mode))
        .chain([width(&effect_title, layout.width_mode)])
        .max()
        .unwrap_or(0);
    let gap = " ".repeat(COLUMN_GAP);
    let after_detail = if effect_w > 0 { COLUMN_GAP } else { 0 };
    let amounts_right = menu.amounts_only();
    let detail_title = clean(&menu.titles[1]);
    let detail_title_w = width(&detail_title, layout.width_mode);
    let column = detail_column(
        detail_w,
        detail_title_w,
        layout
            .width
            .checked_sub(lead.len() + label_w + COLUMN_GAP + after_detail + effect_w),
    );
    let mut header = vec![
        StyledSpan::plain(lead.clone()),
        StyledSpan::plain(pad_text(&clean(&menu.titles[0]), label_w, layout)),
        StyledSpan::plain(gap.clone()),
    ];
    if let Some(column) = column {
        header.push(StyledSpan::plain(pad_text(&detail_title, column, layout)));
        header.push(StyledSpan::plain(" ".repeat(
            (column + after_detail).saturating_sub(detail_title_w.max(column)),
        )));
    }
    header.push(StyledSpan::plain(if amounts_right {
        pad_start(&effect_title, effect_w, layout)
    } else {
        effect_title
    }));
    let header = full(
        header
            .into_iter()
            .map(|s| StyledSpan::new(s.text, header_ink))
            .collect(),
        Look::default(),
        layout,
    );
    let choices = menu
        .choices
        .iter()
        .enumerate()
        .map(|(i, choice)| {
            let chosen = menu.selected == Some(i);
            let head = menu.shape.head(i);
            let base = if choice.enabled || head.is_some() {
                Look::fg(Token::Text)
            } else {
                Look::fg(Token::TextFaint)
            };
            let detail_look = head.map_or(base, |h| Look::fg(h.tone));
            let effect_look = if choice.enabled {
                Look::fg(Token::TextMuted)
            } else {
                base
            };
            let mut spans = selection_lead(chosen, layout);
            if let Some(line) = &choice.line {
                spans.extend(line.spans.iter().cloned());
            } else {
                let label = clean(&choice.label);
                let bare =
                    !choice.current && choice.detail.is_empty() && effect_of(choice).is_empty();
                if bare {
                    spans.extend(mark_line(&label, base, choice.mark, &glyphs).spans);
                } else if choice.current {
                    let slack =
                        label_w.saturating_sub(width(&label, layout.width_mode)) + COLUMN_GAP;
                    spans.extend(mark_line(&label, base, choice.mark, &glyphs).spans);
                    spans.push(StyledSpan::plain(" "));
                    spans.push(StyledSpan::new(mark, Token::Accent));
                    spans.push(StyledSpan::plain(
                        " ".repeat(slack.saturating_sub(1 + current_w)),
                    ));
                } else {
                    spans.extend(
                        mark_line(
                            &pad_text(&label, label_w, layout),
                            base,
                            choice.mark,
                            &glyphs,
                        )
                        .spans,
                    );
                    spans.push(StyledSpan::plain(gap.clone()));
                }
                if let Some(column) = column.filter(|_| !bare) {
                    let held = head.filter(|h| !h.after.is_empty() && held_w(h) <= column);
                    if let Some(h) = held {
                        let kind = pad_text(&clean(&choice.detail), kinds_w, layout);
                        let room = column.saturating_sub(kinds_w + COLUMN_GAP);
                        spans.push(StyledSpan::new(kind, detail_look));
                        spans.push(StyledSpan::plain(gap.clone()));
                        spans.push(StyledSpan::new(
                            pad_text(&clean(&h.after), room, layout),
                            Look::fg(Token::TextMuted),
                        ));
                    } else {
                        let detail = match head {
                            Some(h)
                                if width(&clean(&choice.detail), layout.width_mode) > column =>
                            {
                                clean(&h.short)
                            }
                            _ => clean(&choice.detail),
                        };
                        spans.push(StyledSpan::new(
                            pad_text(&cut(&detail, column, layout), column, layout),
                            detail_look,
                        ));
                    }
                    spans.push(StyledSpan::plain(" ".repeat(after_detail)));
                }
                if !bare {
                    let cell = effect_of(choice);
                    let cell = if amounts_right {
                        pad_start(&cell, effect_w, layout)
                    } else {
                        cell
                    };
                    spans.push(StyledSpan::new(cell, effect_look));
                }
                let room = layout.width.saturating_sub(lead.len() + 1 + current_w);
                if choice.current && width(&label, layout.width_mode) > room {
                    spans = selection_lead(chosen, layout);
                    let cut = fit_end(&label, room, glyphs.more, layout.width_mode);
                    spans.extend(mark_line(&cut, base, choice.mark, &glyphs).spans);
                    spans.push(StyledSpan::plain(" "));
                    spans.push(StyledSpan::new(mark, Token::Accent));
                }
            }
            if chosen {
                selected_row(spans, layout)
            } else {
                full(spans, Look::default(), layout)
            }
        })
        .collect();
    Parts {
        lead,
        header,
        choices,
        notes,
    }
}

pub fn menu_lines(menu: &Menu, room: usize, layout: Layout) -> SuggestRows {
    let Parts {
        header,
        choices,
        notes: note_rows,
        ..
    } = parts(menu, Token::TextMuted, layout);
    let wanted = 1 + menu.choices.len() + note_rows.len();
    let height = wanted.min(MENU_MAX_ROWS).min(room);
    if height == 0 {
        return SuggestRows::default();
    }
    let mut out = SuggestRows {
        lines: vec![header],
        hits: vec![None],
        selected: None,
    };
    let choice_room = usize::from(!menu.choices.is_empty());
    let notes = note_rows.len().min(height.saturating_sub(1 + choice_room));
    let slots = height.saturating_sub(1 + notes);
    let selected = menu.selected.unwrap_or(0);
    let first = selected
        .saturating_sub(slots.saturating_sub(1))
        .min(menu.choices.len().saturating_sub(slots));
    for (i, line) in choices.into_iter().enumerate().skip(first).take(slots) {
        if menu.selected == Some(i) {
            out.selected = Some(out.lines.len());
        }
        out.lines.push(line);
        out.hits.push(Some(i));
    }
    for row in note_rows.into_iter().take(notes) {
        out.lines.push(row);
        out.hits.push(None);
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dock {
    pub name: String,
    pub column: String,
    pub keys: Vec<KeyHint>,
    pub head: String,
    pub exit_short: String,
    pub mark: Option<u8>,
}

pub const DOCK_KEEPS: usize = 3;

pub const DOCK_MIN_ROWS: usize = 3;

const DOCK_LIST_FIRST: usize = 3;

pub fn dock_room(view: usize) -> usize {
    view.saturating_sub(DOCK_KEEPS).max(DOCK_MIN_ROWS.min(view))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Window {
    pub(super) first: usize,
    pub(super) shown: usize,
    pub(super) above: usize,
    pub(super) below: usize,
}

pub(super) fn window(len: usize, rows: usize, selected: usize, counts_apart: bool) -> Window {
    if len <= rows {
        return Window {
            first: 0,
            shown: len,
            above: 0,
            below: 0,
        };
    }
    if counts_apart {
        let first = (selected + 1).saturating_sub(rows).min(len - rows);
        return Window {
            first,
            shown: rows,
            above: first,
            below: len - first - rows,
        };
    }
    if rows < 3 {
        let first = selected
            .saturating_sub(rows.saturating_sub(1))
            .min(len - rows);
        return Window {
            first,
            shown: rows,
            above: 0,
            below: 0,
        };
    }
    let one = rows - 1;
    let (first, shown) = if selected < one {
        (0, one)
    } else if selected >= len - one {
        (len - one, one)
    } else {
        let shown = rows - 2;
        let first = (selected + 1)
            .saturating_sub(shown)
            .max(1)
            .min(len - 1 - shown);
        (first, shown)
    };
    Window {
        first,
        shown,
        above: first,
        below: len - first - shown,
    }
}

pub(super) fn more_span(arrow: &str, n: usize) -> StyledSpan {
    StyledSpan::new(format!("{arrow} {n} more"), Token::TextFaint)
}

pub(super) fn more_row(arrow: &str, n: usize, lead: &str, layout: Layout) -> StyledLine {
    full(
        vec![StyledSpan::plain(lead.to_owned()), more_span(arrow, n)],
        Look::default(),
        layout,
    )
}

pub(super) fn key_spans(
    keys: &[KeyHint],
    mark: Option<u8>,
    look: KeyLook,
    lead: &str,
    layout: Layout,
) -> Vec<StyledSpan> {
    let glyphs = Glyphs::of(layout.glyphs);
    let mut spans = vec![StyledSpan::plain(lead.to_owned())];
    for (i, hint) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(StyledSpan::new(
                format!(" {} ", glyphs.separator),
                look.word,
            ));
        }
        spans.push(StyledSpan::new(one_line(hint.key()), look.key));
        if !hint.does().is_empty() {
            let word = format!(" {}", one_line(hint.does()));
            spans.extend(mark_line(&word, look.word, mark, &glyphs).spans);
        }
    }
    spans.retain(|s| !s.text.is_empty());
    spans
}

pub(super) fn fit_keys(
    keys: &[KeyHint],
    exit_short: &str,
    room: usize,
    fits: impl Fn(&[KeyHint]) -> usize,
) -> Vec<KeyHint> {
    let fits = |kept: &[KeyHint]| kept.len() < 2 || fits(kept) <= room;
    let mut kept = keys.to_vec();
    if !fits(&kept)
        && !exit_short.is_empty()
        && let Some(last) = kept.last_mut().filter(|h| h.is_exit())
    {
        *last = KeyHint::new(last.key(), exit_short).as_exit();
    }
    let exit = kept.iter().find(|h| h.is_exit()).cloned();
    fit_hints(&mut kept, exit.as_ref(), fits);
    kept
}

pub fn key_row(
    keys: &[KeyHint],
    exit_short: &str,
    mark: Option<u8>,
    look: KeyLook,
    lead: &str,
    layout: Layout,
) -> StyledLine {
    let spans = |kept: &[KeyHint]| key_spans(kept, mark, look, lead, layout);
    let kept = fit_keys(keys, exit_short, layout.width, |kept| {
        spans_width(&spans(kept), layout.width_mode)
    });
    full(spans(&kept), Look::default(), layout)
}

pub fn docked_rows(menu: &Menu, dock: &Dock, room: usize, layout: Layout) -> Vec<StyledLine> {
    docked_lines(menu, dock, room, layout).lines
}

pub fn docked_lines(menu: &Menu, dock: &Dock, room: usize, layout: Layout) -> SuggestRows {
    let named = Menu {
        titles: [
            dock.column.clone(),
            menu.titles[1].clone(),
            menu.effect_title(layout.width).to_owned(),
        ],
        ..menu.clone()
    };
    let Parts {
        lead,
        header,
        choices,
        notes,
    } = parts(&named, Token::TextFaint, layout);
    if room < DOCK_MIN_ROWS {
        return SuggestRows::default();
    }
    let mut left = room - 2;
    let mut take = |want: usize| {
        let got = want.min(left);
        left -= got;
        got
    };
    let listed = take(choices.len().min(DOCK_LIST_FIRST));
    let mode = layout.width_mode;
    let effect_at = named
        .amounts_only()
        .then(|| {
            let text = spans_text(&header.spans);
            let at = width(text.get(..text.rfind(named.titles[2].as_str())?)?, mode);
            let said = width(&copy::dock_count(choices.len(), choices.len()), mode);
            (width(&lead, mode) + width(&dock.name, mode) + said + 2 * COLUMN_GAP <= at)
                .then_some(at)
        })
        .flatten();
    let titled = named.titles.iter().any(|t| !t.is_empty());
    let headed = titled && effect_at.is_none() && take(1) == 1;
    let keyed = !dock.keys.is_empty() && take(1) == 1;
    let glyphs = Glyphs::of(layout.glyphs);
    let head_at = width(&lead, mode) + width(&dock.name, mode) + COLUMN_GAP;
    let mut head = wrap_plain(
        &dock.head,
        layout.width.saturating_sub(head_at),
        "",
        "",
        &glyphs,
    );
    head.retain(|row| !row.is_empty());
    head.truncate(1 + take(head.len().saturating_sub(1)));
    let body_cap = MENU_MAX_ROWS.saturating_sub(head.len().max(1) + listed);
    let noted = take(notes.len().min(body_cap));
    let extra = take(
        choices
            .len()
            .saturating_sub(listed)
            .min(body_cap.saturating_sub(noted)),
    );
    let shown = listed + extra;
    let selected = menu.selected.unwrap_or(0);
    let counted = shown < choices.len() && shown < DOCK_LIST_FIRST;
    let said = match head.first() {
        Some(first) => mark_line(&one_line(first), Token::TextMuted, dock.mark, &glyphs).spans,
        None if counted => vec![StyledSpan::new(
            copy::dock_count(selected.min(choices.len() - 1) + 1, choices.len()),
            Token::TextFaint,
        )],
        None => Vec::new(),
    };
    let mut title = vec![
        StyledSpan::plain(lead.clone()),
        StyledSpan::new(one_line(&dock.name), Look::fg(Token::Text).bold()),
    ];
    if !said.is_empty() {
        title.push(StyledSpan::plain(" ".repeat(COLUMN_GAP)));
        title.extend(said);
    }
    if let Some(at) = effect_at {
        let so_far = spans_width(&title, mode);
        title.push(StyledSpan::new(
            " ".repeat(at.saturating_sub(so_far)),
            Token::TextMuted,
        ));
        title.push(StyledSpan::new(named.titles[2].clone(), Token::TextFaint));
    }
    let mut out = vec![
        StyledLine::new(vec![StyledSpan::new(
            glyphs.rule.repeat(layout.width),
            Token::Line,
        )]),
        full(title, Look::default(), layout),
    ];
    out.extend(head.iter().skip(1).map(|row| {
        let mut spans = vec![StyledSpan::plain(" ".repeat(head_at))];
        spans.extend(mark_line(&one_line(row), Token::TextMuted, dock.mark, &glyphs).spans);
        full(spans, Look::default(), layout)
    }));
    if headed {
        out.push(header);
    }
    let mut w = window(choices.len(), shown, selected, false);
    let extra = usize::from(w.below == 0);
    if menu.selected == Some(w.first)
        && w.above > 0
        && w.shown >= 2 + extra
        && menu
            .shape
            .head_above(w.first)
            .is_some_and(|head| head < w.first)
    {
        w.first -= 1;
        w.above -= 1;
        w.shown -= extra;
        w.below += 1 + extra;
    }
    if w.above > 0 {
        out.push(more_row(up_arrow(&glyphs), w.above, &lead, layout));
    }
    let mut hits = vec![None; out.len()];
    let mut chosen = None;
    let pinned = menu
        .selected
        .and_then(|at| menu.shape.head_above(at))
        .filter(|head| *head < w.first && w.shown > 1 && menu.selected != Some(w.first))
        .and_then(|head| choices.get(head).cloned());
    for (i, line) in choices.into_iter().enumerate().skip(w.first).take(w.shown) {
        if menu.selected == Some(i) {
            chosen = Some(out.len());
        }
        match &pinned {
            Some(head) if i == w.first => {
                out.push(head.clone());
                hits.push(None);
            }
            _ => {
                out.push(line);
                hits.push(Some(i));
            }
        }
    }
    if w.below > 0 {
        out.push(more_row(down_arrow(&glyphs), w.below, &lead, layout));
    }
    out.extend(notes.into_iter().take(noted));
    if keyed {
        out.push(key_row(
            &dock.keys,
            &dock.exit_short,
            dock.mark,
            KEY_ROW,
            &lead,
            layout,
        ));
    }
    hits.resize(out.len(), None);
    SuggestRows {
        lines: out,
        hits,
        selected: chosen,
    }
}

pub const SUGGEST_MAX_ROWS: usize = 8;
pub const SUGGEST_SUMMARIES_FROM: usize = 60;
pub const SUGGEST_ARGS_ABOVE: usize = 30;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Suggestion {
    pub name: String,
    pub bold: Range<usize>,
    pub args: String,
    pub summary: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Suggestions {
    pub rows: Vec<Suggestion>,
    pub selected: Option<usize>,
    pub empty: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SuggestRows {
    pub lines: Vec<StyledLine>,
    pub hits: Vec<Option<usize>>,
    pub selected: Option<usize>,
}

pub(super) fn name_spans(name: &str, bold: &[Range<usize>], look: Look) -> Vec<StyledSpan> {
    let cut = |at: usize| {
        let at = at.min(name.len());
        if name.is_char_boundary(at) { at } else { 0 }
    };
    let mut spans = Vec::new();
    let mut done = 0;
    for range in bold {
        let (from, to) = (cut(range.start), cut(range.end));
        if from < done || from >= to {
            continue;
        }
        spans.push(StyledSpan::new(&name[done..from], look));
        spans.push(StyledSpan::new(&name[from..to], look.bold()));
        done = to;
    }
    spans.push(StyledSpan::new(&name[done..], look));
    spans
}

pub fn suggestion_rows(list: &Suggestions, room: usize, layout: Layout) -> SuggestRows {
    let band = in_band(layout);
    let mut rows = banded_suggestions(list, room, band);
    rows.lines = widened_all(rows.lines, band, layout);
    rows
}

fn banded_suggestions(list: &Suggestions, room: usize, layout: Layout) -> SuggestRows {
    let lead = " ".repeat(menu_lead(layout));
    if room == 0 {
        return SuggestRows::default();
    }
    if list.rows.is_empty() {
        let spans = vec![
            StyledSpan::plain(lead),
            StyledSpan::new(one_line(&list.empty), Token::TextMuted),
        ];
        return SuggestRows {
            lines: vec![full(spans, Look::default(), layout)],
            hits: vec![None],
            selected: None,
        };
    }
    let slots = list.rows.len().min(SUGGEST_MAX_ROWS).min(room);
    let at = list.selected.unwrap_or(0);
    let first = at
        .saturating_sub(slots.saturating_sub(1))
        .min(list.rows.len().saturating_sub(slots));
    let shown: Vec<(usize, &Suggestion)> = list
        .rows
        .iter()
        .enumerate()
        .skip(first)
        .take(slots)
        .collect();
    let args = layout.width > SUGGEST_ARGS_ABOVE;
    let summaries = layout.width >= SUGGEST_SUMMARIES_FROM;
    let synopsis = |s: &Suggestion| {
        let mut text = one_line(&s.name);
        if args {
            text.push_str(&one_line(&s.args));
        }
        width(&text, layout.width_mode)
    };
    let synopsis_w = shown.iter().map(|(_, s)| synopsis(s)).max().unwrap_or(0);
    let gap = " ".repeat(COLUMN_GAP);
    let mut out = SuggestRows::default();
    for (i, row) in shown {
        let chosen = list.selected == Some(i);
        let name_look = if row.enabled {
            Look::fg(Token::Text)
        } else {
            Look::fg(Token::TextMuted)
        };
        let muted = Look::fg(Token::TextMuted);
        let mut spans = selection_lead(chosen, layout);
        spans.extend(name_spans(
            &one_line(&row.name),
            std::slice::from_ref(&row.bold),
            name_look,
        ));
        if args {
            spans.push(StyledSpan::new(one_line(&row.args), muted));
        }
        if summaries {
            let pad = synopsis_w.saturating_sub(synopsis(row));
            spans.push(StyledSpan::plain(format!("{}{gap}", " ".repeat(pad))));
            spans.push(StyledSpan::new(one_line(&row.summary), muted));
        }
        spans.retain(|s| !s.text.is_empty());
        if chosen {
            out.selected = Some(out.lines.len());
            out.lines.push(selected_row(spans, layout));
        } else {
            out.lines.push(full(spans, Look::default(), layout));
        }
        out.hits.push(Some(i));
    }
    out
}

#[cfg(test)]
mod tests;
