use std::collections::BTreeMap;

use wisp_protocol::plan_usage::{Percent, PlanUsage, UsageWindow, WINDOWS};

use super::account::{
    AccountKind, AccountState, MARK_WIDTH, billing_token, billing_word, mark, mark_gap,
};
use super::chats::{bottom, framed, inner_room, rule, top};
use super::row::clip;
use crate::markdown::Layout;
use crate::plan_usage::{UNKNOWN_USAGE, gauge, percent, severity, window_names};
use crate::styled::{StyledLine, StyledSpan};
use crate::text::{WidthMode, fit_end, spans_padded, spans_width, width, wrap, wrap_plain};
use crate::theme::Token;
use crate::theme::glyphs::{GlyphSet, Glyphs};

const STARTS_NEW_CHATS: &str = "starts new chats";
const SUBSCRIPTIONS: &str = "Subscriptions";
pub const USAGE_UNAVAILABLE: &str = "usage unavailable";
const GAP: usize = 2;
const NAME_MIN: usize = 10;
const SHARE: usize = 5;
const WINDOW_LABEL: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub enum Cells<'a> {
    Blank,
    Usage(&'a PlanUsage),
    Unavailable,
    Needs { words: String, fix: Option<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountRow<'a> {
    pub name: &'a str,
    pub kind: AccountKind,
    pub host: Option<&'a str>,
    pub cells: Cells<'a>,
    pub resets: BTreeMap<&'a str, (&'a str, String)>,
    pub starts: bool,
    pub mark: Option<u8>,
}

struct Grid {
    lead: usize,
    name: usize,
    provider: Option<usize>,
    bar: usize,
    reset: usize,
    stacked: bool,
    starts: Option<usize>,
    body: usize,
}

const fn limits(bar: usize, reset: usize) -> usize {
    2 * (bar + SHARE + reset) + GAP
}

impl Grid {
    fn of(rows: &[AccountRow<'_>], room: usize, mode: WidthMode, lead: usize) -> Self {
        let reset = rows
            .iter()
            .filter_map(|row| {
                let shown = windows(row);
                let reset = |window: &UsageWindow| {
                    row.resets
                        .get(window.key.as_str())
                        .map(|(label, time)| width(label, mode) + width(time, mode))
                };
                shown
                    .iter()
                    .filter_map(|(_, window)| reset(window))
                    .chain(supplementary_windows(row, &shown).filter_map(|window| reset(&window)))
                    .max()
            })
            .max()
            .unwrap_or(0);
        let starts = if rows.iter().any(|row| row.starts) {
            GAP + width(STARTS_NEW_CHATS, mode)
        } else {
            0
        };
        let provider = rows
            .iter()
            .filter(|row| row.kind != AccountKind::Gateway)
            .map(|row| width(row.kind.provider_word(), mode))
            .max()
            .unwrap_or(0);
        let name = |rest: usize| {
            rows.iter()
                .map(|row| lead + width(row.name, mode))
                .filter(|name| name + rest <= room)
                .max()
                .unwrap_or(0)
                .max(NAME_MIN)
        };
        if reset > 0 && NAME_MIN + GAP + limits(4, reset) > room {
            return Self {
                lead,
                name: name(GAP + width(USAGE_UNAVAILABLE, mode)),
                provider: None,
                bar: 4,
                reset,
                stacked: true,
                starts: None,
                body: room,
            };
        }
        let wide = GAP + provider + GAP + limits(10, reset) + starts;
        if NAME_MIN + wide <= room {
            let name = name(wide);
            let body = name + wide;
            return Self {
                lead,
                name,
                provider: Some(provider),
                bar: 10,
                reset,
                stacked: false,
                starts: (starts > 0).then(|| body - width(STARTS_NEW_CHATS, mode)),
                body,
            };
        }
        let name = name(GAP + limits(4, reset));
        let bar = (room.saturating_sub(name + GAP + limits(0, reset)) / 2).clamp(4, 10);
        Self {
            lead,
            name,
            provider: None,
            bar,
            reset,
            stacked: false,
            starts: None,
            body: name + GAP + limits(bar, reset),
        }
    }

    fn slot(&self) -> usize {
        self.bar + SHARE + self.reset
    }

    fn model_label(&self) -> &'static str {
        if self.provider.is_some() {
            "Model used"
        } else {
            "model used"
        }
    }

    fn used_end(&self) -> usize {
        let start = if self.stacked {
            self.lead + WINDOW_LABEL
        } else {
            self.cells_x()
        };
        start + self.bar + SHARE
    }

    fn cells_x(&self) -> usize {
        self.name + GAP + self.provider.map_or(0, |provider| provider + GAP)
    }
}

fn at(spans: &mut Vec<StyledSpan>, x: usize, more: Vec<StyledSpan>, mode: WidthMode) {
    *spans = spans_padded(std::mem::take(spans), x, mode);
    spans.extend(more);
}

fn whole(window: &UsageWindow) -> Option<UsageWindow> {
    let used_percent = window
        .used_percent
        .and_then(|used| Percent::try_from(used.value().floor()).ok());
    (used_percent.is_some() || window.resets_at.is_some()).then(|| UsageWindow {
        used_percent,
        ..window.clone()
    })
}

fn is_model_only(row: &AccountRow<'_>) -> bool {
    matches!(
        row.cells,
        Cells::Usage(found)
            if !found.windows.is_empty()
                && !found.windows.iter().any(|w| window_names(w).is_some())
    )
}

fn peak_model_window(found: &PlanUsage) -> Option<UsageWindow> {
    found
        .windows
        .iter()
        .filter(|w| w.used_percent.is_some() || w.resets_at.is_some())
        .max_by(|a, b| {
            a.used_percent
                .map(Percent::value)
                .partial_cmp(&b.used_percent.map(Percent::value))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.key.cmp(&a.key))
        })
        .and_then(whole)
}

fn windows(row: &AccountRow<'_>) -> Vec<(usize, UsageWindow)> {
    let Cells::Usage(found) = row.cells else {
        return Vec::new();
    };
    if is_model_only(row) {
        return peak_model_window(found)
            .map(|w| (0, w))
            .into_iter()
            .collect();
    }
    WINDOWS
        .into_iter()
        .enumerate()
        .filter_map(|(n, (_, short, _))| {
            found
                .windows
                .iter()
                .find(|w| window_names(w).is_some_and(|(name, _)| name == short))
                .and_then(whole)
                .map(|window| (n, window))
        })
        .collect()
}

fn window_cells(
    row: &AccountRow<'_>,
    window: &UsageWindow,
    grid: &Grid,
    layout: Layout,
) -> Vec<StyledSpan> {
    let look = severity(window).look();
    let mut cells = gauge(window, grid.bar, look, layout).unwrap_or_default();
    if let Some(used) = window.used_percent {
        cells.push(StyledSpan::new(format!(" {:>4}", percent(used)), look));
    } else {
        cells.push(StyledSpan::new(
            format!("{UNKNOWN_USAGE:>width$}", width = grid.bar + SHARE),
            Token::TextMuted,
        ));
    }
    if let Some((label, time)) = row.resets.get(window.key.as_str()) {
        let width = grid.reset.saturating_sub(width(label, layout.width_mode));
        cells.push(StyledSpan::new(
            format!("{label}{time:>width$}"),
            Token::TextMuted,
        ));
    }
    cells
}

fn usage(row: &AccountRow<'_>, grid: &Grid, layout: Layout) -> Vec<Vec<StyledSpan>> {
    let model_only = is_model_only(row);
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    for (n, window) in windows(row) {
        let cells = window_cells(row, &window, grid, layout);
        if grid.stacked {
            let label = if model_only {
                ""
            } else {
                window_names(&window).map_or("", |(short, _)| short)
            };
            let mut line = vec![StyledSpan::new(
                format!("{label:<WINDOW_LABEL$}"),
                Token::TextMuted,
            )];
            line.extend(cells);
            lines.push(line);
        } else {
            at(
                &mut spans,
                n * (grid.slot() + GAP),
                cells,
                layout.width_mode,
            );
        }
        if model_only {
            let label = if grid.stacked {
                window.label
            } else {
                fit_end(
                    &window.label,
                    grid.slot(),
                    Glyphs::of(layout.glyphs).more,
                    layout.width_mode,
                )
            };
            let label = vec![StyledSpan::new(label, Token::TextMuted)];
            if grid.stacked {
                lines.push(label);
            } else {
                at(&mut spans, grid.slot() + GAP, label, layout.width_mode);
            }
        }
    }
    if !spans.is_empty() {
        lines.push(spans);
    }
    lines
}

fn supplementary_windows<'a>(
    row: &'a AccountRow<'_>,
    shown: &'a [(usize, UsageWindow)],
) -> impl Iterator<Item = UsageWindow> + 'a {
    let windows = match row.cells {
        Cells::Usage(found) => found.windows.as_slice(),
        _ => &[],
    };
    let model_only = is_model_only(row);
    windows
        .iter()
        .filter(move |window| !model_only || window.used_percent.is_none())
        .filter(move |window| !shown.iter().any(|(_, shown)| shown.key == window.key))
        .filter_map(whole)
}

fn supplementary(row: &AccountRow<'_>, grid: &Grid, layout: Layout) -> Vec<Vec<StyledSpan>> {
    let model_only = is_model_only(row);
    let shown = windows(row);
    let mut lines = Vec::new();
    for window in supplementary_windows(row, &shown) {
        let column = WINDOWS
            .iter()
            .position(|(seconds, ..)| window.duration_seconds == Some(*seconds));
        let stacked = grid.stacked || column.is_none();
        let x = if grid.stacked || (column.is_none() && !model_only) {
            grid.lead + WINDOW_LABEL
        } else {
            grid.cells_x() + column.unwrap_or(0) * (grid.slot() + GAP)
        };
        let mut spans = vec![StyledSpan::new(
            format!("{}{}", " ".repeat(grid.lead), window.label),
            Token::TextMuted,
        )];
        if stacked || spans_width(&spans, layout.width_mode) + GAP > x {
            lines.push(std::mem::take(&mut spans));
        }
        at(
            &mut spans,
            x,
            window_cells(row, &window, grid, layout),
            layout.width_mode,
        );
        lines.push(spans);
    }
    lines
        .into_iter()
        .flat_map(|spans| wrap(&spans, grid.body, layout.width_mode))
        .map(|line| line.spans)
        .collect()
}

fn model_heading(with_title: bool, grid: &Grid, mode: WidthMode) -> Vec<StyledSpan> {
    let label = grid.model_label();
    let end = grid.used_end();
    let x = end.saturating_sub(width(label, mode));
    let mut spans = if with_title {
        vec![StyledSpan::new(
            SUBSCRIPTIONS,
            billing_token(AccountState::Subscription),
        )]
    } else {
        Vec::new()
    };
    at(
        &mut spans,
        x,
        vec![StyledSpan::new(label, Token::TextMuted)],
        mode,
    );
    spans
}

fn heading(
    billing: AccountState,
    grid: &Grid,
    mode: WidthMode,
    has_limits: bool,
) -> Vec<Vec<StyledSpan>> {
    if billing != AccountState::Subscription {
        return vec![vec![StyledSpan::new(
            billing_word(billing),
            billing_token(billing),
        )]];
    }
    let mut spans = vec![StyledSpan::new(SUBSCRIPTIONS, billing_token(billing))];
    if grid.stacked || !has_limits {
        return vec![spans];
    }
    let mut lines = Vec::new();
    for (n, (_, short, long)) in WINDOWS.into_iter().enumerate() {
        let name = match grid.provider {
            Some(_) => format!("{long} used"),
            None => format!("{short} used"),
        };
        let end = grid.used_end() + n * (grid.slot() + GAP);
        let x = end.saturating_sub(width(&name, mode));
        if n == 0 && x < width(SUBSCRIPTIONS, mode) + GAP {
            lines.push(std::mem::take(&mut spans));
        }
        at(
            &mut spans,
            x,
            vec![StyledSpan::new(name, Token::TextMuted)],
            mode,
        );
    }
    lines.push(spans);
    lines
}

fn row_lines(row: &AccountRow<'_>, grid: &Grid, layout: Layout) -> Vec<Vec<StyledSpan>> {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let muted = |text: &str, room: usize| {
        StyledSpan::new(fit_end(text, room, glyphs.more, mode), Token::TextMuted)
    };
    let mut lines = Vec::new();
    let gap = || (grid.lead > 0).then(mark_gap);
    let mut first: Vec<StyledSpan> = (grid.lead > 0)
        .then(|| mark(row.mark, &glyphs))
        .into_iter()
        .collect();
    first.push(StyledSpan::new(row.name, Token::Text));
    if grid.lead + width(row.name, mode) > grid.name {
        lines.push(std::mem::replace(&mut first, gap().into_iter().collect()));
    }
    let x = grid.cells_x();
    let starts_x = grid.starts.filter(|_| row.starts);
    let room = starts_x
        .map_or(grid.body, |starts| starts.saturating_sub(GAP))
        .saturating_sub(x);
    let mut under = Vec::new();
    let mut usage = usage(row, grid, layout);
    let mut cells = match &row.cells {
        Cells::Usage(_) if grid.stacked => Vec::new(),
        Cells::Usage(_) => usage.pop().unwrap_or_default(),
        Cells::Unavailable => vec![muted(USAGE_UNAVAILABLE, room)],
        Cells::Blank => Vec::new(),
        Cells::Needs { words, fix } => {
            let words = StyledSpan::new(words.as_str(), Token::Error);
            let fix = fix.as_deref().map(|fix| StyledSpan::new(fix, Token::Text));
            let mut inline = vec![words.clone()];
            if let Some(fix) = &fix {
                let sep = StyledSpan::new(format!(" {} ", glyphs.separator), Token::TextFaint);
                inline.extend([sep, fix.clone()]);
            }
            if grid.provider.is_some() && spans_width(&inline, mode) <= room {
                inline
            } else if width(&words.text, mode) <= room {
                under.extend(fix);
                vec![words]
            } else {
                under.push(words);
                under.extend(fix);
                Vec::new()
            }
        }
    };
    let label = match row.kind {
        AccountKind::Gateway => row.host.unwrap_or_default(),
        kind => kind.provider_word(),
    };
    if let Some(provider) = grid.provider {
        let room = if cells.is_empty() {
            room + GAP + provider
        } else {
            provider
        };
        at(&mut first, grid.name + GAP, vec![muted(label, room)], mode);
    } else if row.kind.billing() == AccountState::Api && cells.is_empty() {
        cells = vec![muted(label, room)];
    }
    at(&mut first, x, cells, mode);
    let starts = StyledSpan::new(STARTS_NEW_CHATS, Token::Accent);
    match starts_x {
        Some(starts_x) => at(&mut first, starts_x, vec![starts], mode),
        None if row.starts => under.push(starts),
        None => {}
    }
    if first.iter().any(|span| !span.text.trim().is_empty()) {
        lines.push(first);
    }
    for cells in usage {
        lines.extend(
            wrap(&cells, grid.body.saturating_sub(grid.lead), mode)
                .into_iter()
                .map(|line| {
                    let mut spans: Vec<StyledSpan> = gap().into_iter().collect();
                    spans.extend(line.spans);
                    spans
                }),
        );
    }
    lines.extend(supplementary(row, grid, layout));
    for span in under {
        let wrapped = wrap_plain(
            &span.text,
            grid.body.saturating_sub(grid.lead),
            "",
            "",
            &glyphs,
        );
        lines.extend(wrapped.into_iter().map(|line| {
            let mut spans: Vec<StyledSpan> = gap().into_iter().collect();
            spans.push(StyledSpan::new(line, span.look));
            spans
        }));
    }
    lines
        .into_iter()
        .map(|line| clip(line, grid.body, glyphs.more, mode))
        .collect()
}

pub fn accounts_panel(
    rows: &[AccountRow<'_>],
    columns: usize,
    set: GlyphSet,
    marked: bool,
) -> Vec<StyledLine> {
    let mode = WidthMode::from(set);
    let room = inner_room(Layout::new(columns, set));
    let grid = Grid::of(rows, room, mode, if marked { MARK_WIDTH } else { 0 });
    let layout = Layout::new(columns + grid.body - room, set);
    let mut out = vec![top(
        vec![StyledSpan::new("Accounts", Token::Text)],
        false,
        layout,
    )];
    for billing in [AccountState::Subscription, AccountState::Api] {
        let mut group: Vec<_> = rows
            .iter()
            .filter(|row| row.kind.billing() == billing)
            .collect();
        if group.is_empty() {
            continue;
        }
        group.sort_by_key(|row| row.name);
        if out.len() > 1 {
            out.push(rule(layout));
        }
        if billing == AccountState::Subscription {
            let (standard, model_only): (Vec<_>, Vec<_>) =
                group.into_iter().partition(|row| !is_model_only(row));
            if !standard.is_empty() {
                let has_limits = standard.iter().any(|row| !windows(row).is_empty());
                out.extend(
                    heading(billing, &grid, mode, has_limits)
                        .into_iter()
                        .map(|line| framed(line, false, layout)),
                );
                for line in standard
                    .iter()
                    .flat_map(|row| row_lines(row, &grid, layout))
                {
                    out.push(framed(line, false, layout));
                }
            }
            if !model_only.is_empty() {
                let only_models = standard.is_empty();
                let inline_title = only_models
                    && grid
                        .used_end()
                        .saturating_sub(width(grid.model_label(), mode))
                        >= width(SUBSCRIPTIONS, mode) + GAP;
                if only_models && !inline_title {
                    out.push(framed(
                        vec![StyledSpan::new(SUBSCRIPTIONS, billing_token(billing))],
                        false,
                        layout,
                    ));
                }
                out.push(framed(
                    model_heading(inline_title, &grid, mode),
                    false,
                    layout,
                ));
                for line in model_only
                    .iter()
                    .flat_map(|row| row_lines(row, &grid, layout))
                {
                    out.push(framed(line, false, layout));
                }
            }
            continue;
        }
        out.extend(
            heading(billing, &grid, mode, false)
                .into_iter()
                .map(|line| framed(line, false, layout)),
        );
        for line in group.iter().flat_map(|row| row_lines(row, &grid, layout)) {
            out.push(framed(line, false, layout));
        }
    }
    out.push(bottom(false, layout));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::text;
    use wisp_protocol::plan_usage::{UsageScope, UsageSource};

    fn std_window(label: &str, pct: f64, duration: u64) -> UsageWindow {
        UsageWindow {
            key: label.into(),
            label: label.into(),
            duration_seconds: Some(duration),
            scope: UsageScope::All,
            used_percent: Some(Percent::try_from(pct).unwrap()),
            resets_at: None,
            observed_at: 1_893_508_000,
            source: UsageSource::UsageEndpoint,
        }
    }

    fn model_window(id: &str, label: &str, pct: f64) -> UsageWindow {
        UsageWindow {
            key: id.into(),
            label: label.into(),
            duration_seconds: None,
            scope: UsageScope::Model(id.into()),
            used_percent: Some(Percent::try_from(pct).unwrap()),
            resets_at: None,
            observed_at: 1_893_508_000,
            source: UsageSource::UsageEndpoint,
        }
    }

    fn resets<'a>(
        found: &'a PlanUsage,
        times: &[Option<&str>],
    ) -> BTreeMap<&'a str, (&'static str, String)> {
        found
            .windows
            .iter()
            .zip(times)
            .filter_map(|(window, time)| {
                Some((
                    window.key.as_str(),
                    (wisp_tool_text::config::RESET_IN, (*time)?.to_owned()),
                ))
            })
            .collect()
    }

    #[test]
    fn mixed_accounts_panel_matches_mockups_across_widths_and_glyphs() {
        let claude = PlanUsage::new(
            None,
            vec![
                std_window("5-hour", 42.0, 18000),
                std_window("Weekly", 18.0, 604800),
            ],
        )
        .unwrap();
        let minutes = PlanUsage::new(
            None,
            vec![
                std_window("5-hour", 7.0, 18000),
                std_window("Weekly", 5.0, 604800),
            ],
        )
        .unwrap();
        let seconds = PlanUsage::new(
            None,
            vec![
                std_window("5-hour", 65.0, 18000),
                std_window("Weekly", 88.0, 604800),
            ],
        )
        .unwrap();
        let antigravity = PlanUsage::new(
            None,
            vec![
                model_window("gemini-3-flash-preview", "Gemini 3 Flash", 10.0),
                model_window("gemini-3.1-pro-preview", "Gemini 3.1 Pro", 25.0),
            ],
        )
        .unwrap();
        let rows = [
            AccountRow {
                name: "sub-a",
                kind: AccountKind::ClaudeSubscription,
                host: None,
                cells: Cells::Usage(&claude),
                resets: resets(&claude, &[Some("2h10m"), Some("4d06h")]),
                starts: false,
                mark: Some(1),
            },
            AccountRow {
                name: "sub-b",
                kind: AccountKind::ChatGpt,
                host: None,
                cells: Cells::Unavailable,
                resets: BTreeMap::new(),
                starts: false,
                mark: Some(2),
            },
            AccountRow {
                name: "sub-c",
                cells: Cells::Usage(&minutes),
                resets: resets(&minutes, &[Some("12m05s"), None]),
                kind: AccountKind::ClaudeSubscription,
                host: None,
                starts: false,
                mark: Some(3),
            },
            AccountRow {
                name: "sub-d",
                kind: AccountKind::ChatGpt,
                cells: Cells::Usage(&seconds),
                resets: resets(&seconds, &[Some("45s"), Some("1d20h")]),
                host: None,
                starts: false,
                mark: Some(4),
            },
            AccountRow {
                name: "models-demo",
                kind: AccountKind::Antigravity,
                host: None,
                cells: Cells::Usage(&antigravity),
                resets: resets(&antigravity, &[None, Some("3h20m")]),
                starts: false,
                mark: Some(6),
            },
            AccountRow {
                name: "api-a",
                kind: AccountKind::GeminiApi,
                host: None,
                cells: Cells::Blank,
                resets: BTreeMap::new(),
                starts: true,
                mark: Some(5),
            },
        ];
        assert_eq!(
            text(&accounts_panel(&rows, 40, GlyphSet::Unicode, true)),
            [
                "╭─ Accounts ───────────────────────────╮",
                "│   Subscriptions                      │",
                "│   ● sub-a                            │",
                "│     5h   ⣿⣀⣀⣀  42%, resets in  2h10m │",
                "│     week ⣿⣀⣀⣀  18%, resets in  4d06h │",
                "│   ● sub-b        usage unavailable   │",
                "│   ● sub-c                            │",
                "│     5h   ⣿⣀⣀⣀   7%, resets in 12m05s │",
                "│     week ⣿⣀⣀⣀   5%                   │",
                "│   ● sub-d                            │",
                "│     5h   ⣿⣿⣀⣀  65%, resets in    45s │",
                "│     week ⣿⣿⣿⣀  88%, resets in  1d20h │",
                "│         model used                   │",
                "│   ● models-demo                      │",
                "│          ⣿⣀⣀⣀  25%, resets in  3h20m │",
                "│     Gemini 3.1 Pro                   │",
                "├┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┤",
                "│   API                                │",
                "│   ● api-a        Gemini              │",
                "│     starts new chats                 │",
                "╰──────────────────────────────────────╯",
            ]
            .join("\n")
        );
        assert_eq!(
            text(&accounts_panel(&rows, 40, GlyphSet::Ascii, true)),
            [
                "+- Accounts ---------------------------+",
                "|   Subscriptions                      |",
                "|   * sub-a                            |",
                "|     5h   #...  42%, resets in  2h10m |",
                "|     week #...  18%, resets in  4d06h |",
                "|   * sub-b        usage unavailable   |",
                "|   * sub-c                            |",
                "|     5h   #...   7%, resets in 12m05s |",
                "|     week #...   5%                   |",
                "|   * sub-d                            |",
                "|     5h   ##..  65%, resets in    45s |",
                "|     week ###.  88%, resets in  1d20h |",
                "|         model used                   |",
                "|   * models-demo                      |",
                "|          #...  25%, resets in  3h20m |",
                "|     Gemini 3.1 Pro                   |",
                "|--------------------------------------|",
                "|   API                                |",
                "|   * api-a        Gemini              |",
                "|     starts new chats                 |",
                "+--------------------------------------+",
            ]
            .join("\n")
        );
        assert_eq!(
            text(&accounts_panel(&rows, 80, GlyphSet::Unicode, true)),
            [
                "╭─ Accounts ──────────────────────────────────────────────────────────────────╮",
                "│   Subscriptions     5h used                     week used                   │",
                "│   ● sub-a        ⣿⣿⣀⣀⣀  42%, resets in  2h10m  ⣿⣀⣀⣀⣀  18%, resets in  4d06h │",
                "│   ● sub-b        usage unavailable                                          │",
                "│   ● sub-c        ⣿⣀⣀⣀⣀   7%, resets in 12m05s  ⣿⣀⣀⣀⣀   5%                   │",
                "│   ● sub-d        ⣿⣿⣿⣀⣀  65%, resets in    45s  ⣿⣿⣿⣿⣀  88%, resets in  1d20h │",
                "│                  model used                                                 │",
                "│   ● models-demo  ⣿⣀⣀⣀⣀  25%, resets in  3h20m  Gemini 3.1 Pro               │",
                "├┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┤",
                "│   API                                                                       │",
                "│   ● api-a        Gemini                                                     │",
                "│     starts new chats                                                        │",
                "╰─────────────────────────────────────────────────────────────────────────────╯",
            ]
            .join("\n")
        );
        assert_eq!(
            text(&accounts_panel(&rows, 80, GlyphSet::Ascii, true)),
            [
                "+- Accounts ------------------------------------------------------------------+",
                "|   Subscriptions     5h used                     week used                   |",
                "|   * sub-a        ##...  42%, resets in  2h10m  #....  18%, resets in  4d06h |",
                "|   * sub-b        usage unavailable                                          |",
                "|   * sub-c        #....   7%, resets in 12m05s  #....   5%                   |",
                "|   * sub-d        ###..  65%, resets in    45s  ####.  88%, resets in  1d20h |",
                "|                  model used                                                 |",
                "|   * models-demo  #....  25%, resets in  3h20m  Gemini 3.1 Pro               |",
                "|-----------------------------------------------------------------------------|",
                "|   API                                                                       |",
                "|   * api-a        Gemini                                                     |",
                "|     starts new chats                                                        |",
                "+-----------------------------------------------------------------------------+",
            ]
            .join("\n")
        );
        assert_eq!(
            text(&accounts_panel(&rows, 120, GlyphSet::Unicode, true)),
            [
                "╭─ Accounts ───────────────────────────────────────────────────────────────────────────────────────────────────────────╮",
                "│   Subscriptions                   5-hour used                        Weekly used                                     │",
                "│   ● sub-a        Claude       ⣿⣿⣿⣿⣀⣀⣀⣀⣀⣀  42%, resets in  2h10m  ⣿⣀⣀⣀⣀⣀⣀⣀⣀⣀  18%, resets in  4d06h                   │",
                "│   ● sub-b        ChatGPT      usage unavailable                                                                      │",
                "│   ● sub-c        Claude       ⣿⣀⣀⣀⣀⣀⣀⣀⣀⣀   7%, resets in 12m05s  ⣿⣀⣀⣀⣀⣀⣀⣀⣀⣀   5%                                     │",
                "│   ● sub-d        ChatGPT      ⣿⣿⣿⣿⣿⣿⣀⣀⣀⣀  65%, resets in    45s  ⣿⣿⣿⣿⣿⣿⣿⣿⣀⣀  88%, resets in  1d20h                   │",
                "│                                    Model used                                                                        │",
                "│   ● models-demo  Antigravity  ⣿⣿⣀⣀⣀⣀⣀⣀⣀⣀  25%, resets in  3h20m  Gemini 3.1 Pro                                      │",
                "├┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┤",
                "│   API                                                                                                                │",
                "│   ● api-a        Gemini                                                                             starts new chats │",
                "╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯",
            ]
            .join("\n")
        );
        assert_eq!(
            text(&accounts_panel(&rows, 120, GlyphSet::Ascii, true)),
            [
                "+- Accounts -----------------------------------------------------------------------------------------------------------+",
                "|   Subscriptions                   5-hour used                        Weekly used                                     |",
                "|   * sub-a        Claude       ####......  42%, resets in  2h10m  #.........  18%, resets in  4d06h                   |",
                "|   * sub-b        ChatGPT      usage unavailable                                                                      |",
                "|   * sub-c        Claude       #.........   7%, resets in 12m05s  #.........   5%                                     |",
                "|   * sub-d        ChatGPT      ######....  65%, resets in    45s  ########..  88%, resets in  1d20h                   |",
                "|                                    Model used                                                                        |",
                "|   * models-demo  Antigravity  ##........  25%, resets in  3h20m  Gemini 3.1 Pro                                      |",
                "|----------------------------------------------------------------------------------------------------------------------|",
                "|   API                                                                                                                |",
                "|   * api-a        Gemini                                                                             starts new chats |",
                "+----------------------------------------------------------------------------------------------------------------------+",
            ]
            .join("\n")
        );

        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for columns in [40, 80, 120] {
                for reset in [None, Some("4d06h")] {
                    let mut found = claude.clone();
                    for (key, label, duration, pct) in [
                        ("Fable", "Fable weekly", 604800, 100.0),
                        ("Opus", "Opus weekly", 604800, 22.0),
                        ("unknown", "Other period", 123, 9.0),
                    ] {
                        found.windows.push(UsageWindow {
                            duration_seconds: Some(duration),
                            label: label.into(),
                            ..model_window(key, label, pct)
                        });
                    }
                    let row = AccountRow {
                        cells: Cells::Usage(&found),
                        resets: resets(&found, &[Some("2h10m"), Some("4d06h"), reset, None, None]),
                        ..rows[0].clone()
                    };
                    let shown = text(&accounts_panel(
                        std::slice::from_ref(&row),
                        columns,
                        set,
                        true,
                    ));
                    assert!(shown.contains("Fable weekly"), "{shown}");
                    assert!(shown.contains("Opus weekly"), "{shown}");
                    assert!(shown.contains("Other period"), "{shown}");
                    assert_eq!(shown.matches("100%").count(), 1, "{shown}");
                    assert!(
                        shown.lines().all(|line| line.chars().count() <= columns),
                        "{shown}"
                    );
                    if columns > 40 {
                        let weekly = shown.lines().find(|line| line.contains(" 18%")).unwrap();
                        let scoped = shown.lines().find(|line| line.contains("100%")).unwrap();
                        let x = |line: &str, value: &str| {
                            line[..line.find(value).unwrap()].chars().count()
                        };
                        assert_eq!(x(weekly, "18%"), x(scoped, "00%"), "{shown}");
                        assert!(scoped.contains("Fable weekly"), "{shown}");
                    }
                    assert_eq!(
                        shown.matches("4d06h").count(),
                        1 + usize::from(reset.is_some()),
                        "{shown}"
                    );
                    let mut partial = found.clone();
                    for window in &mut partial.windows {
                        if window.key == "Weekly" || window.key == "Fable" {
                            window.used_percent = None;
                        }
                    }
                    let partial_row = AccountRow {
                        cells: Cells::Usage(&partial),
                        ..row
                    };
                    let shown = text(&accounts_panel(&[partial_row], columns, set, true));
                    assert!(!shown.contains("unknown"), "{shown}");
                    assert!(!shown.contains("Fable weekly"), "{shown}");
                    assert!(
                        !shown.lines().any(|line| line
                            .trim_start_matches(['│', '|', ' '])
                            .starts_with("Weekly ")),
                        "{shown}"
                    );
                    assert!(shown.contains("Opus weekly"), "{shown}");
                }
            }
        }
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for (columns, subset) in [
                (38, &rows[..1]),
                (39, &rows[..1]),
                (72, &rows[..1]),
                (73, &rows[..1]),
                (74, &rows[..]),
                (75, &rows[..]),
            ] {
                let mut probe = subset.to_vec();
                if columns < 40 {
                    for row in &mut probe {
                        row.resets.clear();
                    }
                }
                let shown = text(&accounts_panel(&probe, columns, set, true));
                let title = shown
                    .lines()
                    .find(|line| line.contains(SUBSCRIPTIONS))
                    .unwrap();
                assert_eq!(
                    title.trim_matches(['│', '|', ' ']),
                    SUBSCRIPTIONS,
                    "{shown}"
                );
                assert!(shown.contains("5h used"), "{shown}");
                assert!(
                    shown.lines().all(|line| line.chars().count() <= columns),
                    "{shown}"
                );
            }
        }
        for time in [None, Some("45s"), Some("12345d00h")] {
            let one = [AccountRow {
                resets: resets(&claude, &[time, None]),
                ..rows[0].clone()
            }];
            let shown = text(&accounts_panel(&one, 40, GlyphSet::Unicode, true));
            assert_eq!(shown.contains("resets in"), time.is_some(), "{shown}");
            if let Some(time) = time {
                assert!(shown.contains(time), "{shown}");
            }
            assert!(
                shown.lines().all(|line| line.chars().count() <= 40),
                "{shown}"
            );
        }
        for columns in [40, 80, 120] {
            let shown = text(&accounts_panel(
                &[rows[4].clone()],
                columns,
                GlyphSet::Unicode,
                true,
            ));
            assert_eq!(shown.matches("resets in").count(), 1, "{shown}");
            assert!(shown.contains("Gemini 3.1 Pro"), "{shown}");
        }
    }

    fn quota_row<'a>(
        name: &'a str,
        kind: AccountKind,
        found: &'a PlanUsage,
        times: &[Option<&str>],
    ) -> AccountRow<'a> {
        AccountRow {
            name,
            kind,
            host: None,
            cells: Cells::Usage(found),
            resets: resets(found, times),
            starts: false,
            mark: Some(1),
        }
    }

    #[test]
    fn gemini_summary_and_fallback_use_their_own_slots_at_every_width() {
        let standard = |first, weekly| {
            PlanUsage::new(
                None,
                vec![
                    std_window(WINDOWS[0].2, first, WINDOWS[0].0),
                    std_window(WINDOWS[1].2, weekly, WINDOWS[1].0),
                ],
            )
            .unwrap()
        };
        let chatgpt = standard(7.0, 5.0);
        let claude = standard(42.0, 18.0);
        let summary = standard(16.0, 38.0);
        let mut partial = summary.clone();
        partial.windows[0].used_percent = None;
        partial.windows[0].resets_at = Some(1_893_509_000);
        let model = model_window("gemini-3.1-pro", "Gemini 3.1 Pro", 13.0);
        let reset_only = UsageWindow {
            used_percent: None,
            resets_at: Some(1_893_509_000),
            ..model_window("gemini-3-flash", "Gemini 3 Flash", 0.0)
        };
        let mut with_unknown = summary.clone();
        with_unknown.windows.push(reset_only.clone());
        let fallback = PlanUsage::new(None, vec![model.clone()]).unwrap();
        let unmeasured = PlanUsage::new(None, vec![reset_only.clone()]).unwrap();
        let mixed_fallback = PlanUsage::new(None, vec![model, reset_only]).unwrap();
        let claude = AccountRow {
            starts: true,
            ..quota_row(
                "claude-test",
                AccountKind::ClaudeSubscription,
                &claude,
                &[Some("2h10m"), Some("4d06h")],
            )
        };
        let chatgpt = quota_row(
            "chatgpt-test",
            AccountKind::ChatGpt,
            &chatgpt,
            &[Some("12m05s"), Some("2d07h")],
        );
        for columns in [40, 80, 120] {
            for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
                let panel = |rows: &[_]| text(&accounts_panel(rows, columns, glyphs, true));
                let column = |panel: &str, word: &str| {
                    let line = panel.lines().find(|line| line.contains(word)).unwrap();
                    width(line.split_once(word).unwrap().0, WidthMode::from(glyphs)) + word.len()
                };
                let gemini = quota_row(
                    "gemini-test",
                    AccountKind::Antigravity,
                    &summary,
                    &[Some("1h45m"), Some("4d03h")],
                );
                let primary = panel(&[claude.clone(), gemini, chatgpt.clone()]);
                assert!(
                    primary.find("chatgpt-test").unwrap() < primary.find("claude-test").unwrap()
                );
                assert_eq!(column(&primary, "16%"), column(&primary, "42%"));
                assert_eq!(column(&primary, "38%"), column(&primary, "18%"));
                assert!(!primary.to_ascii_lowercase().contains("model used"));
                let gemini = quota_row(
                    "gemini-test",
                    AccountKind::Antigravity,
                    &with_unknown,
                    &[Some("1h45m"), Some("4d03h"), Some("40m00s")],
                );
                let standard_extra = panel(&[claude.clone(), gemini]);
                assert!(
                    standard_extra
                        .lines()
                        .any(|line| line.contains("unknown, resets in 40m00s")),
                    "{standard_extra}"
                );
                assert!(!standard_extra.to_ascii_lowercase().contains("model used"));
                let standard_value = column(&standard_extra, "16%");
                let unknown_value = column(&standard_extra, "unknown");
                if columns == 40 {
                    assert_eq!(unknown_value, standard_value, "{standard_extra}");
                } else {
                    assert!(unknown_value < standard_value, "{standard_extra}");
                }
                let mut aligned = Vec::new();
                for (usage, times, needle, model_only, supplementary) in [
                    (
                        &partial,
                        &[Some("40m00s"), Some("4d03h")][..],
                        "unknown",
                        false,
                        false,
                    ),
                    (&fallback, &[Some("40m00s")][..], "13%", true, false),
                    (&unmeasured, &[Some("40m00s")][..], "unknown", true, false),
                    (
                        &mixed_fallback,
                        &[Some("2h10m"), Some("40m00s")][..],
                        "13%",
                        true,
                        true,
                    ),
                ] {
                    let gemini = quota_row("gemini-test", AccountKind::Antigravity, usage, times);
                    let shown = panel(&[gemini, claude.clone()]);
                    assert_eq!(
                        shown.to_ascii_lowercase().contains("model used"),
                        model_only,
                        "{shown}"
                    );
                    assert!(
                        shown
                            .lines()
                            .all(|line| width(line, WidthMode::from(glyphs)) <= columns)
                    );
                    let reset_line = shown.lines().find(|line| line.contains("40m00s")).unwrap();
                    assert!(
                        reset_line.contains(&format!("{needle}, resets in")),
                        "{shown}"
                    );
                    if supplementary {
                        assert!(shown.contains("Gemini 3 Flash"), "{shown}");
                        assert!(
                            shown.lines().any(|line| line.contains("unknown, resets in")
                                && line.contains("2h10m")),
                            "{shown}"
                        );
                        assert_eq!(column(&shown, "unknown"), column(&shown, "13%"), "{shown}");
                    }
                    aligned.push(column(&shown, needle));
                }
                assert!(aligned.windows(2).all(|pair| pair[0] == pair[1]));
            }
        }
    }

    #[test]
    fn single_antigravity_account_places_model_heading_without_collision() {
        let offline = [AccountRow {
            name: "antigravity",
            kind: AccountKind::Antigravity,
            host: None,
            cells: Cells::Unavailable,
            resets: BTreeMap::new(),
            starts: true,
            mark: Some(6),
        }];
        let narrow_40 = text(&accounts_panel(&offline, 40, GlyphSet::Unicode, true));
        assert!(!narrow_40.contains("5h used"), "{narrow_40}");
        assert!(narrow_40.contains("│   Subscriptions"), "{narrow_40}");
        assert!(!narrow_40.contains("model used"), "{narrow_40}");
        assert!(narrow_40.contains(USAGE_UNAVAILABLE), "{narrow_40}");

        let medium_80 = text(&accounts_panel(&offline, 80, GlyphSet::Unicode, true));
        assert!(medium_80.contains("│   Subscriptions"), "{medium_80}");
        assert!(!medium_80.contains("model used"), "{medium_80}");

        let tied = PlanUsage::new(
            None,
            vec![
                model_window("gemini-3.1-pro-preview", "Gemini 3.1 Pro", 30.0),
                model_window("gemini-2.5-pro", "Gemini 2.5 Pro", 30.0),
            ],
        )
        .unwrap();
        let active = [AccountRow {
            name: "antigravity",
            kind: AccountKind::Antigravity,
            host: None,
            cells: Cells::Usage(&tied),
            resets: BTreeMap::new(),
            starts: true,
            mark: Some(6),
        }];
        let wide_120 = text(&accounts_panel(&active, 120, GlyphSet::Unicode, true));
        assert!(wide_120.contains("Subscriptions"), "{wide_120}");
        assert!(wide_120.contains("Model used"), "{wide_120}");
        assert!(wide_120.contains("Gemini 2.5 Pro"), "{wide_120}");
    }
}
