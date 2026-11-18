use wisp_protocol::AgentState;
use wisp_protocol::agents::ROOT_NAME;
use wisp_protocol::slot::NodeState;

use super::{AgentRow, AgentsView, account, fill_view, idle_timed};
use crate::chrome::account::{Chip, chip as billed};
use crate::chrome::band::CHROME_EDGE;
use crate::chrome::context::{MeterLevel, MeterView, meter, meter_level};
use crate::chrome::topology::dot;
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{fit_end, width};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

const WIDE: usize = 100;
pub(super) const NARROW: usize = 60;
const LEAD: &str = "   ";

pub(super) struct Cells {
    glyph: (&'static str, Token),
    name: String,
    model: String,
    state: String,
    activity: Option<String>,
    cost: String,
    context: Option<StyledSpan>,
    todos: Option<String>,
    chip: Option<Chip>,
    preset: Option<&'static str>,
    warm: Option<String>,
    mail: Option<String>,
}

fn context(view: &MeterView) -> StyledSpan {
    let shown = meter(view);
    match meter_level(view.used, view.limit, view.auto_compact) {
        MeterLevel::Calm => StyledSpan::new(shown.text, Token::TextMuted),
        _ => shown,
    }
}

type Mark = (&'static str, Token);

pub(super) fn main_label(glyphs: &Glyphs) -> (Mark, &'static str) {
    ((glyphs.wisp, Token::Accent), ROOT_NAME)
}

pub(super) fn agent_label<'a>(row: &'a AgentRow, glyphs: &Glyphs) -> (Mark, &'a str) {
    (dot(row.entry.state.node_state(), glyphs), &row.entry.name)
}

pub(super) fn main_cells(view: &AgentsView<'_>, glyphs: &Glyphs) -> Cells {
    let main = view.main;
    let chip = Chip::find(main.endpoint.as_deref(), &main.accounts);
    let cost = account::cell_cost(&main.spend, main.priced, chip.as_ref());
    let state = match main.running_since {
        Some(since) => format!(
            "{} {}",
            copy::agent_state(AgentState::Running),
            copy::clock(view.t.saturating_sub(since).as_secs())
        ),
        None => copy::agent_state(AgentState::Idle { outcome: None }),
    };
    let (glyph, name) = main_label(glyphs);
    Cells {
        glyph,
        name: name.to_owned(),
        model: match &main.effort {
            Some(effort) => format!("{} {effort}", main.model),
            None => main.model.clone(),
        },
        state,
        activity: None,
        cost,
        context: main.meter.as_ref().map(context),
        todos: main.checklist.map(copy::todo_progress),
        chip,
        preset: None,
        warm: main.keep_warm.map(copy::keep_warm_fact),
        mail: None,
    }
}

pub(super) fn agent_cells(row: &AgentRow, view: &AgentsView<'_>, glyphs: &Glyphs) -> Cells {
    let entry = &row.entry;
    let accounts = &view.main.accounts;
    let state = entry.state.node_state();
    let mail = row.last_mail.as_ref().map(|(label, at)| {
        let age = copy::ago(view.now_ms.saturating_sub(*at) / 1000);
        format!("{label}, {age}")
    });
    let (glyph, name) = agent_label(row, glyphs);
    Cells {
        glyph,
        name: name.to_owned(),
        model: super::entry_model(row),
        state: idle_timed(row, view.now_ms),
        activity: copy::agent_activity(&entry.activity).filter(|_| state == NodeState::Working),
        cost: super::entry_cost(entry, accounts),
        context: entry.context.as_ref().map(|fill| context(&fill_view(fill))),
        todos: entry.checklist.map(copy::todo_progress),
        chip: Chip::find(entry.endpoint.as_deref(), accounts),
        preset: entry.preset_change.map(copy::preset_change_mark),
        warm: entry.keep_warm.map(copy::keep_warm_fact),
        mail,
    }
}

pub(super) fn fitted(parts: &[String], room: usize, sep: &str, layout: Layout) -> Vec<String> {
    let mut kept: Vec<String> = vec![];
    let mut used = 0;
    for part in parts.iter().filter(|p| !p.is_empty()) {
        let add = width(part, layout.width_mode)
            + if kept.is_empty() {
                0
            } else {
                width(sep, layout.width_mode)
            };
        if !kept.is_empty() && used + add > room {
            break;
        }
        used += add;
        kept.push(part.clone());
    }
    kept
}

fn room(layout: Layout) -> usize {
    (layout.width.saturating_sub(1).min(CHROME_EDGE)).saturating_sub(LEAD.len())
}

fn row(lead: Vec<StyledSpan>, parts: Vec<(String, Look)>, sep: &str, layout: Layout) -> StyledLine {
    let room = room(layout);
    let parts: Vec<(String, Look)> = parts.into_iter().filter(|(t, _)| !t.is_empty()).collect();
    let texts: Vec<String> = parts.iter().map(|(t, _)| t.clone()).collect();
    let kept = fitted(&texts, room, sep, layout).len();
    let more = Glyphs::of(layout.glyphs).more;
    let mut spans = lead;
    for (i, (text, look)) in parts.into_iter().take(kept).enumerate() {
        if i > 0 {
            spans.push(StyledSpan::new(sep, Token::TextFaint));
        }
        let text = fit_end(&text, room, more, layout.width_mode);
        spans.push(StyledSpan::new(text, look));
    }
    StyledLine::new(spans)
}

pub(super) fn panel_rows(cells: &Cells, layout: Layout) -> [StyledLine; 2] {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let sep = format!(" {} ", glyphs.separator);
    let muted = Look::fg(Token::TextMuted);
    let faint = Look::fg(Token::TextFaint);
    let lead = vec![
        StyledSpan::plain(" "),
        StyledSpan::new(cells.glyph.0, cells.glyph.1),
        StyledSpan::plain(" "),
    ];
    let name = (cells.name.clone(), Look::fg(Token::Text).bold());
    let at = |text: &str| (text.to_owned(), muted);
    let context = cells.context.clone().map(|c| (c.text, c.look));
    let chip = cells.chip.as_ref();
    let (first, gap, second) = if layout.width < NARROW {
        let account = chip.map(|c| c.name.clone()).unwrap_or_default();
        let first = vec![name, at(&cells.state), at(&cells.cost)];
        let preset = cells.preset.unwrap_or_default().to_owned();
        let warm = cells.warm.clone().unwrap_or_default();
        (
            first,
            "  ",
            vec![cells.model.clone(), account, preset, warm],
        )
    } else {
        let wide = layout.width >= WIDE;
        let state = match cells.activity.as_deref().filter(|_| wide) {
            Some(activity) => format!("{}: {activity}", cells.state),
            None => cells.state.clone(),
        };
        let mut first = vec![name, at(&cells.model), at(&state), at(&cells.cost)];
        first.extend(context);
        first.extend(cells.todos.as_deref().filter(|_| wide).map(at));
        let account = chip.map(|c| {
            let keep = room(layout).saturating_sub(width(&billed("", c.billed, &glyphs), mode));
            billed(
                &fit_end(&c.name, keep, glyphs.more, mode),
                c.billed,
                &glyphs,
            )
        });
        let preset = cells.preset.map(str::to_owned);
        let (warm, mail) = (cells.warm.clone(), cells.mail.clone());
        let second = [account, preset, warm, mail]
            .map(Option::unwrap_or_default)
            .to_vec();
        (first, if wide { "   " } else { "  " }, second)
    };
    let second = second.into_iter().map(|t| (t, faint)).collect();
    [
        row(lead, first, gap, layout),
        row(vec![StyledSpan::plain(LEAD)], second, &sep, layout),
    ]
}
