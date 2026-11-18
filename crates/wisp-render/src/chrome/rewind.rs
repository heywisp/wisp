use wisp_protocol::PointInfo;

use super::menu::{Choice, Dock, Menu, Shape, Tone, key_row};
use super::row::{KEY_ROW, KeyHint, pad};
use super::{model_line, one_line};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{width, wrap_words};
use crate::theme::Token;
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::transcript::copy;

pub const REWIND_LEAD: usize = 3;
const MENU_LEAD: usize = 3;
const COLUMN_GAP: usize = 2;
const LABEL_MIN: usize = 16;

#[derive(Clone, Copy, Debug)]
pub struct RewindView<'a> {
    pub points: &'a [PointInfo],
    pub times: &'a [String],
    pub selected: usize,
    pub next_model: &'a str,
}

fn sep(set: GlyphSet) -> String {
    format!(" {} ", Glyphs::of(set).separator)
}

fn titles() -> [String; 3] {
    [
        copy::TURN_COLUMN.to_owned(),
        copy::MESSAGE_COLUMN.to_owned(),
        copy::CACHE_COLUMN.to_owned(),
    ]
}

pub fn rewind_dock() -> Dock {
    Dock {
        name: copy::REWIND_NAME.to_owned(),
        column: copy::TURN_COLUMN.to_owned(),
        keys: vec![copy::enter_choose(), copy::esc(copy::CLOSE_WORD)],
        ..Dock::default()
    }
}

pub fn rewind_loading(layout: Layout) -> Menu {
    let more = Glyphs::of(layout.glyphs).more;
    Menu {
        titles: titles(),
        choices: Vec::new(),
        selected: None,
        notes: vec![(format!("{}{more}", copy::REWIND_LOADING), Tone::Faint)],
        shape: Shape::default(),
    }
}

fn point_notes(point: &PointInfo) -> Vec<(String, Tone)> {
    let mut notes = Vec::new();
    if let Some(note) = point.compaction {
        if note.compacts_first {
            notes.push((copy::compacts_first(note.turn), Tone::Note));
        } else {
            notes.push((copy::before_compaction(note.turn), Tone::Faint));
        }
    }
    if !point.processes.is_empty() {
        notes.push((
            copy::processes_started_after(point.processes.len()),
            Tone::Faint,
        ));
    }
    notes
}

fn widest(texts: impl Iterator<Item = String>, layout: Layout) -> usize {
    texts
        .map(|t| width(&t, layout.width_mode))
        .max()
        .unwrap_or(0)
}

pub fn rewind_menu(view: &RewindView<'_>, layout: Layout) -> Menu {
    let set = layout.glyphs;
    let sep = sep(set);
    let next_model = model_line(view.next_model);
    let labels: Vec<String> = view
        .points
        .iter()
        .map(|p| copy::point_turn(p.point.turn()))
        .collect();
    let long: Vec<String> = view
        .points
        .iter()
        .map(|p| copy::quote_text(&p.quote, &next_model, &sep))
        .collect();
    let short: Vec<String> = view
        .points
        .iter()
        .map(|p| copy::quote_short(&p.quote))
        .collect();
    let turn_w =
        widest(labels.iter().cloned(), layout).max(width(copy::TURN_COLUMN, layout.width_mode));
    let time_w = widest(view.times.iter().cloned(), layout);
    let room_for = |label_w: usize, effects: &[String]| {
        let effect_w = widest(effects.iter().cloned(), layout);
        layout
            .width
            .saturating_sub(MENU_LEAD + label_w + COLUMN_GAP * 2 + effect_w)
    };
    let timed_w = turn_w + COLUMN_GAP + time_w;
    let fits = |label_w: usize, effects: &[String]| room_for(label_w, effects) >= LABEL_MIN;
    let long_fits = fits(turn_w, &long);
    let with_time = time_w > 0
        && if long_fits {
            fits(timed_w, &long)
        } else {
            fits(timed_w, &short)
        };
    let effects = if long_fits { long } else { short };
    let choices = view
        .points
        .iter()
        .zip(labels)
        .zip(effects)
        .enumerate()
        .map(|(i, ((point, turn), effect))| {
            let label = match view.times.get(i).filter(|_| with_time) {
                Some(time) => {
                    let pad = turn_w.saturating_sub(width(&turn, layout.width_mode));
                    format!("{turn}{}{}{time}", " ".repeat(pad), " ".repeat(COLUMN_GAP))
                }
                None => turn,
            };
            Choice {
                mark: None,
                current: false,
                label,
                detail: one_line(&point.label),
                effect,
                enabled: true,
                line: None,
            }
        })
        .collect();
    let selected = view.selected.min(view.points.len().saturating_sub(1));
    Menu {
        titles: titles(),
        choices,
        selected: (!view.points.is_empty()).then_some(selected),
        notes: view
            .points
            .get(selected)
            .map(point_notes)
            .unwrap_or_default(),
        shape: Shape::default(),
    }
}

fn wrapped(text: &str, token: Token, layout: Layout) -> Vec<StyledLine> {
    let lead = " ".repeat(REWIND_LEAD);
    let measure = layout.width.saturating_sub(REWIND_LEAD).max(1);
    wrap_words(
        &[StyledSpan::new(one_line(text), token)],
        measure,
        layout.width_mode,
    )
    .into_iter()
    .map(|row| {
        let mut spans = vec![StyledSpan::plain(lead.clone())];
        spans.extend(row.spans);
        pad(spans, Look::default(), layout)
    })
    .collect()
}

pub fn rewind_panel(point: &PointInfo, layout: Layout, room: usize) -> Vec<StyledLine> {
    let set = layout.glyphs;
    let sep = sep(set);
    let label = one_line(&point.label);
    let ranked = |lines: Vec<StyledLine>, first: usize, rest: usize| {
        lines
            .into_iter()
            .enumerate()
            .map(move |(i, line)| (if i == 0 { first } else { rest }, line))
    };
    let mut out: Vec<(usize, StyledLine)> = vec![(
        0,
        StyledLine::new(vec![StyledSpan::new(
            Glyphs::of(set).rule.repeat(layout.width),
            Token::Line,
        )]),
    )];
    out.extend(ranked(
        wrapped(
            &copy::rewind_to(point.point.turn(), &label, &sep),
            Token::Text,
            layout,
        ),
        1,
        4,
    ));
    out.extend(ranked(
        wrapped(
            &copy::next_message_reads(&point.quote),
            Token::TextMuted,
            layout,
        ),
        2,
        3,
    ));
    if let Some(note) = point.compaction {
        let (text, token) = if note.compacts_first {
            (copy::compacts_first(note.turn), Token::Text)
        } else {
            (copy::before_compaction(note.turn), Token::TextMuted)
        };
        out.extend(ranked(wrapped(&text, token, layout), 5, 5));
    }
    if !point.processes.is_empty() {
        out.extend(ranked(
            wrapped(
                &copy::processes_keep_running(&point.processes, &sep),
                Token::TextMuted,
                layout,
            ),
            6,
            6,
        ));
    }
    out.push((
        0,
        key_row(
            &[
                KeyHint::new(copy::ENTER_KEY, copy::REWIND_WORD),
                copy::esc(copy::BACK_WORD),
            ],
            "",
            None,
            KEY_ROW,
            &" ".repeat(REWIND_LEAD),
            layout,
        ),
    ));
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by_key(|&i| (out[i].0, i));
    order.truncate(room);
    order.sort_unstable();
    order.into_iter().map(|i| out[i].1.clone()).collect()
}

pub fn rewind_notes(notes: &[String], room: usize, layout: Layout) -> Vec<StyledLine> {
    notes
        .iter()
        .filter(|note| !note.is_empty())
        .flat_map(|note| wrapped(note, Token::Text, layout))
        .take(room)
        .collect()
}

#[cfg(test)]
mod tests;
