use std::time::Duration;

use wisp_protocol::slot::NodeState;
use wisp_protocol::{
    AccountSpend, AgentEntry, AgentPath, AgentState, ContextFill, Effort, HoldReason, IdleOutcome,
    KeepWarm, TodoProgress,
};

use super::account::Chip;
use super::context::MeterView;
use super::facts::short_model;
use super::menu::key_row;
use super::processes::{STATE_GAP, split_at, title_row};
use super::row::{KEY_ROW, edge};
use super::summary::{ChatTurn, shown_total, spent};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{fit_end, spans_width, width};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy::{self, AgentKeys};

mod account;
mod list;
mod tree;

pub use super::account::AccountBilling;
pub(crate) use account::THIS_CHAT;
pub(super) use account::{TOTAL, tree_amount};
pub use tree::{AgentsTree, label as agent_label, list_width, tree as agents_tree};

pub const AGENTS_MIN_ROWS: usize = 4;
pub const PAGE_CHROME: usize = 6;
pub const AGENTS_LIST_GAP: usize = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub entry: AgentEntry,
    pub effort: Option<Effort>,
    pub last_outcome: Option<IdleOutcome>,
    pub last_mail: Option<(String, u64)>,
}

impl AgentRow {
    pub fn is_live(&self) -> bool {
        self.entry.state.is_live()
    }

    pub fn is_busy(&self) -> bool {
        self.entry.state.is_busy() || matches!(self.entry.state, AgentState::Held { .. })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MainRow {
    pub running_since: Option<Duration>,
    pub spend: AccountSpend,
    pub meter: Option<MeterView>,
    pub checklist: Option<TodoProgress>,
    pub model: String,
    pub effort: Option<String>,
    pub keep_warm: Option<KeepWarm>,
    pub endpoint: Option<String>,
    pub priced: bool,
    pub accounts: Vec<AccountBilling>,
}

fn working(row: &AgentRow) -> bool {
    row.entry.state.node_state() == NodeState::Working
}

fn clocked(row: &AgentRow) -> bool {
    working(row) || matches!(row.entry.state, AgentState::Idle { .. })
}

fn clock(row: &AgentRow, now_ms: u64) -> Option<String> {
    let since = row.entry.since_ms.filter(|_| clocked(row))?;
    Some(copy::clock(now_ms.saturating_sub(since) / 1000))
}

pub fn timed(row: &AgentRow, now_ms: u64) -> String {
    let word = copy::agent_state(row.entry.state);
    match clock(row, now_ms).filter(|_| working(row)) {
        Some(clock) => format!("{word} {clock}"),
        None => word,
    }
}

fn idle_timed(row: &AgentRow, now_ms: u64) -> String {
    match (row.entry.state, clock(row, now_ms)) {
        (AgentState::Idle { outcome }, Some(clock)) => {
            let word = copy::agent_state(AgentState::Idle { outcome: None });
            let outcome = outcome.map(|o| format!(": {o}")).unwrap_or_default();
            format!("{word} {clock}{outcome}")
        }
        _ => timed(row, now_ms),
    }
}

fn next_second(since_ms: u64, t: Duration, now_ms: u64) -> Duration {
    t.saturating_add(Duration::from_millis(
        1000 - now_ms.saturating_sub(since_ms) % 1000,
    ))
}

pub(crate) fn fill_view(fill: &ContextFill) -> MeterView {
    MeterView {
        used: fill.used,
        used_estimated: fill.estimated,
        window: fill.window,
        limit: fill.limit,
        auto_compact: fill.auto_compact,
    }
}

fn entry_model(row: &AgentRow) -> String {
    let model = short_model(&row.entry.model);
    match row.effort {
        Some(effort) => format!("{model} {}", effort.as_str()),
        None => model,
    }
}

fn entry_cost(entry: &AgentEntry, accounts: &[AccountBilling]) -> String {
    let chip = Chip::find(entry.endpoint.as_deref(), accounts);
    account::cell_cost(&entry.spend(), entry.priced, chip.as_ref())
}

pub fn agent_row<'a>(rows: &'a [AgentRow], path: &AgentPath) -> Option<&'a AgentRow> {
    rows.iter().find(|r| &r.entry.path == path)
}

pub fn chat_header(row: &AgentRow, accounts: &[AccountBilling], layout: Layout) -> [StyledLine; 2] {
    let glyphs = Glyphs::of(layout.glyphs);
    let sep = format!(" {} ", glyphs.separator);
    let entry = &row.entry;
    let state = entry
        .activity
        .waiting_text()
        .unwrap_or_else(|| copy::agent_state(entry.state));
    let lead = format!(" {} ", glyphs.agents);
    let room = layout
        .width
        .saturating_sub(2 + width(&lead, layout.width_mode));
    let path = fit_end(entry.path.as_str(), room, glyphs.more, layout.width_mode);
    let parts = [path, state, entry_cost(entry, accounts)];
    let mut spans = vec![StyledSpan::new(lead, Token::Accent)];
    for (i, part) in list::fitted(&parts, room, &sep, layout)
        .into_iter()
        .enumerate()
    {
        if i == 0 {
            spans.push(StyledSpan::new(part, Look::fg(Token::Text).bold()));
        } else {
            spans.push(StyledSpan::new(sep.clone(), Token::TextFaint));
            spans.push(StyledSpan::new(part, Token::TextMuted));
        }
    }
    [
        StyledLine::new(spans),
        super::config_screen::screen_rule(None, layout),
    ]
}

pub fn count_ladder(live: &[NodeState], glyphs: &Glyphs) -> Vec<Vec<StyledSpan>> {
    if live.is_empty() {
        return vec![vec![]];
    }
    let sep = format!(" {} ", glyphs.separator);
    let count = |state| live.iter().filter(|s| **s == state).count();
    let (working, failed) = (count(NodeState::Working), count(NodeState::Failed));
    let working = (working > 0).then(|| (copy::agents_working(working), Token::TextMuted));
    let failed = (failed > 0).then(|| (copy::agents_failed(failed), Token::Error));
    let count = live.len().to_string();
    [
        (
            copy::agents_count(live.len()),
            vec![working.clone(), failed.clone()],
        ),
        (count.clone(), vec![working.clone(), failed.clone()]),
        (count.clone(), vec![failed.or(working)]),
        (count, vec![]),
    ]
    .into_iter()
    .map(|(count, states)| {
        let mut spans = vec![StyledSpan::new(count, Token::Text)];
        for (text, token) in states.into_iter().flatten() {
            spans.push(StyledSpan::new(sep.clone(), Token::TextFaint));
            spans.push(StyledSpan::new(text, token));
        }
        spans
    })
    .collect()
}

fn strip_counts(rows: &[AgentRow], glyphs: &Glyphs) -> Vec<Vec<StyledSpan>> {
    let lead = [
        StyledSpan::plain(" "),
        StyledSpan::new(glyphs.agents, Token::Accent),
        StyledSpan::new(" ", Token::Text),
    ];
    let live: Vec<NodeState> = (rows.iter())
        .filter(|r| r.is_live())
        .map(|r| r.entry.state.node_state())
        .collect();
    count_ladder(&live, glyphs)
        .into_iter()
        .map(|rung| {
            if rung.is_empty() {
                rung
            } else {
                lead.iter().cloned().chain(rung).collect()
            }
        })
        .collect()
}

pub fn strip_line(
    rows: &[AgentRow],
    main: AccountSpend,
    turn: ChatTurn,
    layout: Layout,
) -> Option<StyledLine> {
    let glyphs = Glyphs::of(layout.glyphs);
    let counts = strip_counts(rows, &glyphs);
    let amount = Some(shown_total(main, rows, turn))
        .filter(spent)
        .map(|total| account::tree_amount(&total).to_string());
    if counts.iter().all(Vec::is_empty) && amount.is_none() {
        return None;
    }
    let narrowest = counts.last()?;
    let (wide, short) = match amount {
        Some(amount) => (account::total_label(&amount), amount),
        None => (String::new(), String::new()),
    };
    let room = edge(layout);
    let mode = layout.width_mode;
    let (left, total) = counts
        .iter()
        .map(|left| (left, &wide))
        .chain([(narrowest, &short)])
        .find(|(left, total)| spans_width(left, mode) + STATE_GAP + width(total, mode) <= room)
        .unwrap_or((narrowest, &short));
    let right = if total.is_empty() {
        vec![]
    } else {
        vec![StyledSpan::new(total.clone(), Token::TextMuted)]
    };
    Some(split_at(left.clone(), right, room, layout))
}

pub fn clocks_next_change(rows: &[AgentRow], t: Duration, now_ms: u64) -> Option<Duration> {
    rows.iter()
        .filter(|r| working(r))
        .filter_map(|r| r.entry.since_ms)
        .map(|since| next_second(since, t, now_ms))
        .min()
}

pub fn releasable(row: &AgentRow) -> bool {
    row.entry.state
        == AgentState::Held {
            reason: HoldReason::HeldMessages,
        }
}

#[derive(Clone, Copy, Debug)]
pub struct AgentsView<'a> {
    pub main: &'a MainRow,
    pub rows: &'a [AgentRow],
    pub t: Duration,
    pub now_ms: u64,
}

pub fn panel_rows(view: &AgentsView<'_>, picked: &AgentPath, layout: Layout) -> [StyledLine; 2] {
    let glyphs = Glyphs::of(layout.glyphs);
    let cells = match agent_row(view.rows, picked) {
        _ if picked.is_root() => list::main_cells(view, &glyphs),
        Some(row) => list::agent_cells(row, view, &glyphs),
        None => return Default::default(),
    };
    list::panel_rows(&cells, layout)
}

pub fn beside(
    tree: Vec<StyledLine>,
    picture: Vec<StyledLine>,
    room: usize,
    layout: Layout,
) -> Vec<StyledLine> {
    if room == 0 {
        return picture;
    }
    let tree = tree
        .into_iter()
        .map(|mut line| {
            if let Some((pad, look)) = line.band_pad() {
                line.spans.push(StyledSpan::new(" ".repeat(pad), look));
            }
            line.spans
        })
        .collect();
    let picture = picture.into_iter().map(|line| line.spans).collect();
    super::context::beside(tree, picture, room, layout.width_mode)
        .into_iter()
        .map(StyledLine::new)
        .collect()
}

pub fn agents_page(
    body: Vec<StyledLine>,
    panel: [StyledLine; 2],
    keys: AgentKeys,
    room: usize,
    layout: Layout,
) -> Vec<StyledLine> {
    let rule = Glyphs::of(layout.glyphs).rule;
    let title = StyledSpan::new(copy::AGENTS_TITLE, Look::fg(Token::Text).bold());
    let mut out = vec![title_row(vec![title], layout)];
    let height = room.saturating_sub(PAGE_CHROME);
    out.extend(
        body.into_iter()
            .chain(std::iter::repeat_with(StyledLine::default))
            .take(height),
    );
    out.push(StyledLine::default());
    out.push(StyledLine::new(vec![
        StyledSpan::plain(" "),
        StyledSpan::new(rule.repeat(layout.width.saturating_sub(2)), Token::Line),
    ]));
    out.extend(panel);
    let keys = copy::agents_keys(layout.glyphs, keys, copy::CLOSE_WORD);
    out.push(key_row(&keys, "", None, KEY_ROW, " ", layout));
    let over = out.len().saturating_sub(room);
    out.drain(1..out.len().min(1 + over));
    out.truncate(room);
    out
}

pub fn agents_note(text: &str) -> Vec<StyledLine> {
    let note = StyledSpan::new(text, Token::TextMuted);
    vec![
        StyledLine::default(),
        StyledLine::new(vec![StyledSpan::plain(" "), note]),
    ]
}

pub fn agents_next_change(view: &AgentsView<'_>, picked: &AgentPath) -> Option<Duration> {
    let main = view.main.running_since.map(|since| {
        let local = view.t.saturating_sub(since);
        since.saturating_add(Duration::from_secs(local.as_secs().saturating_add(1)))
    });
    let agent = agent_row(view.rows, picked)
        .filter(|row| clocked(row))
        .and_then(|row| row.entry.since_ms)
        .map(|since| next_second(since, view.t, view.now_ms));
    [main, agent].into_iter().flatten().min()
}

#[cfg(test)]
mod tests;
