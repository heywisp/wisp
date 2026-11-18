use wisp_protocol::AgentPath;
use wisp_protocol::agents::ROOT_PATH;

use super::AgentRow;
use super::list::{NARROW, agent_label, main_label};
use crate::chrome::band::selected_line;
use crate::chrome::sidebar::rails;
use crate::markdown::Layout;
use crate::styled::{StyledLine, StyledSpan};
use crate::text::{fit_end, spans_padded, width};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;

#[derive(Debug, Default)]
pub struct AgentsTree {
    pub lines: Vec<StyledLine>,
    pub shown: Vec<AgentPath>,
}

pub fn label<'a>(row: Option<&'a AgentRow>, glyphs: &Glyphs) -> ((&'static str, Token), &'a str) {
    row.map_or_else(|| main_label(glyphs), |row| agent_label(row, glyphs))
}

fn depth(row: Option<&AgentRow>) -> u32 {
    row.map_or(0, |row| row.entry.path.depth())
}

pub fn list_width<'a>(rows: impl Iterator<Item = Option<&'a AgentRow>>, layout: Layout) -> usize {
    if layout.width < NARROW {
        return 0;
    }
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    rows.map(|row| {
        let ((mark, _), name) = label(row, &glyphs);
        2 * depth(row) as usize + width(mark, mode) + width(name, mode) + 4
    })
    .max()
    .unwrap_or(0)
    .min(layout.width / 3)
}

fn line(row: Option<&AgentRow>, rail: &str, edge: &str, layout: Layout) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let ((mark, ink), name) = label(row, &glyphs);
    let mode = layout.width_mode;
    let room = layout.width.saturating_sub(width(mark, mode) + 4);
    let name_room = width(name, mode).min(width(glyphs.more, mode));
    let rail_room = room.saturating_sub(name_room);
    let rail = fit_end(rail, rail_room, "", mode);
    let name = fit_end(name, room - width(&rail, mode), glyphs.more, mode);
    let mut spans = vec![
        StyledSpan::plain(" "),
        StyledSpan::new(rail, Token::Line),
        StyledSpan::new(mark, ink),
        StyledSpan::plain(" "),
        StyledSpan::new(name, Token::Text),
    ];
    if !edge.is_empty() {
        spans = spans_padded(spans, layout.width.saturating_sub(width(edge, mode)), mode);
        spans.push(StyledSpan::new(edge, Token::TextFaint));
    }
    StyledLine::new(spans)
}

pub fn tree<'a>(
    rows: impl Iterator<Item = Option<&'a AgentRow>> + Clone,
    picked: &AgentPath,
    height: usize,
    mut layout: Layout,
) -> AgentsTree {
    let cols = list_width(rows.clone(), layout);
    if cols == 0 || height == 0 {
        return AgentsTree::default();
    }
    layout.width = cols;
    let rows: Vec<_> = rows.collect();
    let depths: Vec<_> = rows.iter().copied().map(depth).collect();
    let path = |row: Option<&'a AgentRow>| row.map_or(ROOT_PATH, |r| r.entry.path.as_str());
    let selected = rows
        .iter()
        .position(|&r| path(r) == picked.as_str())
        .unwrap_or(0);
    let top = selected
        .saturating_sub(height / 2)
        .min(rows.len().saturating_sub(height));
    let bottom = (top + height).min(rows.len());
    let glyphs = Glyphs::of(layout.glyphs);
    let mut out = AgentsTree::default();
    for (i, &row) in rows.iter().enumerate().take(bottom).skip(top) {
        let edge = [
            (i == top && top > 0).then_some(glyphs.more_up),
            (i + 1 == bottom && bottom < rows.len()).then_some(glyphs.more_down),
        ];
        let edge: String = edge.into_iter().flatten().collect();
        let line = line(row, &rails(&depths, i, &glyphs), &edge, layout);
        out.lines.push(if i == selected {
            selected_line(line, cols, layout.width_mode)
        } else {
            line
        });
        out.shown.push(AgentPath::new(path(row)));
    }
    out
}
