use super::band::WIDE_FROM;
use super::one_line;
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{spans_width, width, wrap};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

pub const HELP_STACKED_BELOW: usize = 60;

pub const HELP_KEY_MAX: usize = 22;

pub const HELP_INDENT: usize = 2;

pub const HELP_GAP: usize = 2;

pub const HELP_STACKED_HANG: usize = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpRow {
    pub keys: String,
    pub args: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpSection {
    pub lead: Option<String>,
    pub title: String,
    pub rows: Vec<HelpRow>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HelpPage {
    pub rows: Vec<StyledLine>,
    pub starts: Vec<usize>,
}

pub fn help_page(sections: &[HelpSection], layout: Layout) -> HelpPage {
    let mut rows = heading(
        Some(copy::HELP_LEAD),
        copy::HELP_TITLE,
        layout.width,
        layout,
    );
    rows.push(StyledLine::new(vec![]));
    let top = rows.len();
    let key_column = key_column(sections, layout);
    if layout.width >= WIDE_FROM && sections.len() > 1 {
        let left_width = layout.width / 2 - 1;
        let right_width = layout.width - left_width - 1;
        let blocks: Vec<Vec<StyledLine>> = sections
            .iter()
            .map(|s| section(s, left_width.min(right_width), key_column, layout))
            .collect();
        let split = balance(&blocks);
        let (left, left_starts) = stack(&blocks[..split]);
        let (right, right_starts) = stack(&blocks[split..]);
        let rule = Glyphs::of(layout.glyphs).pipe.trim_end().to_owned();
        for index in 0..left.len().max(right.len()) {
            let mut spans = left.get(index).map(|l| l.spans.clone()).unwrap_or_default();
            let used = spans_width(&spans, layout.width_mode);
            spans.push(StyledSpan::plain(
                " ".repeat(left_width.saturating_sub(used)),
            ));
            spans.push(StyledSpan::new(rule.clone(), Token::Line));
            if let Some(line) = right.get(index) {
                spans.extend(line.spans.iter().cloned());
            }
            rows.push(StyledLine::new(spans));
        }
        let starts = left_starts
            .into_iter()
            .chain(right_starts)
            .map(|s| s + top)
            .collect();
        return HelpPage { rows, starts };
    }
    let blocks: Vec<Vec<StyledLine>> = sections
        .iter()
        .map(|s| section(s, layout.width, key_column, layout))
        .collect();
    let (body, starts) = stack(&blocks);
    rows.extend(body);
    HelpPage {
        rows,
        starts: starts.into_iter().map(|s| s + top).collect(),
    }
}

fn key_width(row: &HelpRow, layout: Layout) -> usize {
    width(&one_line(&row.keys), layout.width_mode) + width(&one_line(&row.args), layout.width_mode)
}

fn key_column(sections: &[HelpSection], layout: Layout) -> usize {
    sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .map(|r| key_width(r, layout))
        .filter(|&w| w <= HELP_KEY_MAX)
        .max()
        .unwrap_or(0)
}

fn balance(blocks: &[Vec<StyledLine>]) -> usize {
    let heights: Vec<usize> = blocks.iter().map(Vec::len).collect();
    let height = |part: &[usize]| part.iter().sum::<usize>() + part.len().saturating_sub(1);
    (1..heights.len())
        .min_by_key(|&split| {
            let (left, right) = heights.split_at(split);
            height(left).max(height(right))
        })
        .unwrap_or(heights.len())
}

fn stack(blocks: &[Vec<StyledLine>]) -> (Vec<StyledLine>, Vec<usize>) {
    let mut rows = vec![];
    let mut starts = vec![];
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            rows.push(StyledLine::new(vec![]));
        }
        starts.push(rows.len());
        rows.extend(block.iter().cloned());
    }
    (rows, starts)
}

fn heading(lead: Option<&str>, title: &str, columns: usize, layout: Layout) -> Vec<StyledLine> {
    let mut spans = vec![];
    if let Some(lead) = lead {
        spans.push(StyledSpan::new(
            format!("{} ", one_line(lead)),
            Token::TextMuted,
        ));
    }
    spans.push(StyledSpan::new(
        one_line(title),
        Look::fg(Token::Text).bold(),
    ));
    let margin = layout.gutter().saturating_sub(HELP_INDENT);
    let measure = columns.saturating_sub(margin).max(1);
    led(&spans, measure, margin, margin, layout)
}

fn led(
    spans: &[StyledSpan],
    measure: usize,
    first: usize,
    rest: usize,
    layout: Layout,
) -> Vec<StyledLine> {
    wrap(spans, measure.max(1), layout.width_mode)
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let lead = if i == 0 { first } else { rest };
            let mut out = vec![StyledSpan::plain(" ".repeat(lead))];
            out.extend(row.spans);
            StyledLine::new(out)
        })
        .collect()
}

fn section(
    section: &HelpSection,
    columns: usize,
    key_column: usize,
    layout: Layout,
) -> Vec<StyledLine> {
    let mut lines = heading(section.lead.as_deref(), &section.title, columns, layout);
    for row in &section.rows {
        lines.extend(help_row(row, columns, key_column, layout));
    }
    lines
}

fn help_row(row: &HelpRow, columns: usize, key_column: usize, layout: Layout) -> Vec<StyledLine> {
    let indent = layout.gutter();
    let keys = vec![
        StyledSpan::new(one_line(&row.keys), Token::Text),
        StyledSpan::new(one_line(&row.args), Token::TextMuted),
    ];
    let text = [StyledSpan::new(one_line(&row.text), Token::TextMuted)];
    let width_of_keys = key_width(row, layout);
    let described_at = indent + key_column + HELP_GAP;
    let stacked = layout.width < HELP_STACKED_BELOW || described_at + 1 > columns;
    if stacked {
        let hang = layout.gutter() + HELP_STACKED_HANG;
        let mut lines = led(
            &keys,
            columns.saturating_sub(indent),
            indent,
            indent,
            layout,
        );
        lines.extend(led(&text, columns.saturating_sub(hang), hang, hang, layout));
        return lines;
    }
    let measure = columns - described_at;
    if width_of_keys > key_column {
        let mut lines = led(
            &keys,
            columns.saturating_sub(indent),
            indent,
            indent,
            layout,
        );
        lines.extend(led(&text, measure, described_at, described_at, layout));
        return lines;
    }
    let mut lines = led(&text, measure, 0, described_at, layout);
    if let Some(first) = lines.first_mut() {
        let mut spans = vec![StyledSpan::plain(" ".repeat(indent))];
        spans.extend(keys);
        spans.push(StyledSpan::plain(
            " ".repeat(key_column - width_of_keys + HELP_GAP),
        ));
        spans.extend(first.spans.drain(..).skip(1));
        first.spans = spans;
    }
    lines
}

#[cfg(test)]
mod tests;
