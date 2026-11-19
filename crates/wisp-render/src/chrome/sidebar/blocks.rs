use std::sync::Arc;
use std::time::Duration;

use wisp_protocol::context::{FillPart, fill_percent, format_tokens};
use wisp_protocol::slot::{NodeState, SlotBlock, SlotFill, SlotNode, SlotSpan, SlotTone};

use super::super::context::{
    Budget, CACHED, Cell, MeterView, allocate, cell_bar, cell_run, free_look, free_square, meter,
    meter_base, meter_level, part_look, rounded,
};
use super::super::row::{clip, full};
use super::super::topology::{Topology, dot, panel};
use super::Failure;
use crate::canvas;
use crate::markdown::Layout;
use crate::motion::{MotionCtx, tree_pulse};
use crate::picture::{Dot, Mark, SQUARES_PER_ROW, Scene, Strip};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{spans_width, width, wrap};
use crate::theme::glyphs::Glyphs;
use crate::theme::{Ink, Token};
use crate::transcript::copy::{self, Fact};

pub(super) const GAP: usize = 2;
const MIN_CUT: usize = 2;
const MIN_METER: usize = 4;
const MIN_LABEL: usize = 8;
const FILL_PERCENT: usize = 4;
const BAR_COLUMNS: usize = 25;
const BAR_ROWS: usize = 2;
const SQUARE_RADIUS: f32 = 0.4;
const MICROS_PER_USD: f64 = 1_000_000.0;
const SPARK_DOTS: u64 = 4;
const LEAD: &str = " ";

pub(super) type Line = (StyledLine, Option<Strip>);

pub(super) struct Ctx {
    pub(super) layout: Layout,
    pub(super) glyphs: Glyphs,
    pub(super) at: Duration,
    pub(super) now_ms: u64,
    pub(super) cx: MotionCtx,
    pub(super) meter_max: usize,
    pub(super) pictures: Option<u16>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    Left,
    Right,
}

impl Ctx {
    fn inner(&self) -> usize {
        self.layout.width.saturating_sub(1)
    }

    fn w(&self, spans: &[StyledSpan]) -> usize {
        spans_width(spans, self.layout.width_mode)
    }

    fn clip(&self, spans: Vec<StyledSpan>, room: usize) -> Vec<StyledSpan> {
        clip(spans, room, self.glyphs.more, self.layout.width_mode)
    }

    pub(super) fn row(&self, spans: Vec<StyledSpan>) -> StyledLine {
        let mut all = vec![StyledSpan::plain(LEAD)];
        all.extend(spans);
        full(all, self.layout)
    }

    pub(super) fn blank(&self) -> StyledLine {
        full(vec![], self.layout)
    }

    pub(super) fn faint_row(&self, text: String) -> StyledLine {
        self.row(vec![StyledSpan::new(text, Token::TextFaint)])
    }

    fn pair(&self, left: Vec<StyledSpan>, right: Vec<StyledSpan>, cut: Cut) -> StyledLine {
        let inner = self.inner();
        let (lw, rw) = (self.w(&left), self.w(&right));
        if rw == 0 {
            return self.row(left);
        }
        let (left, right) = if lw + GAP + rw <= inner {
            (left, right)
        } else if cut == Cut::Right {
            let room = inner.saturating_sub(lw + GAP);
            if room < MIN_CUT {
                return self.row(left);
            }
            (left, self.clip(right, room))
        } else {
            let room = inner.saturating_sub(rw + GAP);
            if room < MIN_CUT {
                (vec![], self.clip(right, inner))
            } else {
                (self.clip(left, room), right)
            }
        };
        let pad = inner.saturating_sub(self.w(&left) + self.w(&right));
        let mut spans = left;
        spans.push(StyledSpan::plain(" ".repeat(pad)));
        spans.extend(right);
        self.row(spans)
    }

    fn styled(&self, span: &SlotSpan, all_faint: bool) -> StyledSpan {
        let toned = |text: String, tone: SlotTone, bold: bool| {
            let look = Look::fg(if all_faint {
                Token::TextMuted
            } else {
                token(tone)
            });
            StyledSpan::new(text, if bold { look.bold() } else { look })
        };
        match span {
            SlotSpan::Text { text, tone, bold } => toned(text.clone(), *tone, *bold),
            SlotSpan::Elapsed { since_ms, tone } => toned(
                copy::clock(self.now_ms.saturating_sub(*since_ms) / 1000),
                *tone,
                false,
            ),
            SlotSpan::Usd {
                micros,
                at_least,
                tone,
            } => toned(
                Fact::cost(*micros as f64 / MICROS_PER_USD, *at_least).text(),
                *tone,
                false,
            ),
            SlotSpan::Fill { fill } => meter(&meter_view(fill)),
            SlotSpan::Unknown => toned(String::new(), SlotTone::Text, false),
        }
    }

    pub(super) fn spans(&self, list: &[SlotSpan]) -> Vec<StyledSpan> {
        let shown = || list.iter().filter_map(faint);
        let all_faint = shown().count() > 0 && shown().all(|faint| faint);
        list.iter().map(|s| self.styled(s, all_faint)).collect()
    }
}

fn faint(span: &SlotSpan) -> Option<bool> {
    match span {
        SlotSpan::Text { text, tone, .. } => (!text.is_empty()).then_some(*tone == SlotTone::Faint),
        SlotSpan::Elapsed { tone, .. } | SlotSpan::Usd { tone, .. } => {
            Some(*tone == SlotTone::Faint)
        }
        SlotSpan::Fill { .. } => Some(false),
        SlotSpan::Unknown => None,
    }
}

fn meter_view(fill: &SlotFill) -> MeterView {
    MeterView {
        used: fill.used,
        used_estimated: fill.estimated,
        window: fill.window,
        limit: fill.limit,
        auto_compact: fill.auto_compact,
    }
}

pub(crate) fn token(tone: SlotTone) -> Token {
    match tone {
        SlotTone::Text | SlotTone::Unknown => Token::Text,
        SlotTone::Muted => Token::TextMuted,
        SlotTone::Faint => Token::TextFaint,
        SlotTone::Accent => Token::Accent,
        SlotTone::Ok => Token::Ok,
        SlotTone::Error => Token::Error,
    }
}

fn figure(tone: SlotTone) -> Token {
    match tone {
        SlotTone::Faint => Token::TextMuted,
        other => token(other),
    }
}

fn role_token(role: Option<&str>) -> Option<Token> {
    let role = match role? {
        "main" => return Some(Token::Accent),
        name if name.ends_with("reviewer") => "reviewer",
        name => name,
    };
    Token::ALL
        .into_iter()
        .find(|token| token.name().strip_prefix("role.") == Some(role))
}

pub(super) fn title(title: &str, rungs: &[Vec<StyledSpan>], folded: bool, ctx: &Ctx) -> StyledLine {
    let mut left = vec![StyledSpan::new(title, Look::fg(Token::Text).bold())];
    if folded {
        left.push(StyledSpan::plain(" "));
        left.push(StyledSpan::new(ctx.glyphs.collapsed, Token::TextFaint));
    }
    let room = ctx.inner().saturating_sub(ctx.w(&left) + GAP);
    let right = rungs
        .iter()
        .find(|rung| ctx.w(rung) <= room)
        .or(rungs.last())
        .cloned()
        .unwrap_or_default();
    ctx.pair(left, right, Cut::Right)
}

pub(super) fn failure(failure: &Failure, ctx: &Ctx) -> StyledLine {
    let sep = format!(" {} ", ctx.glyphs.separator);
    let head = if failure.parked {
        copy::stopped_updating(&sep)
    } else {
        copy::couldnt_update(&copy::slot_failure(&failure.failure))
    };
    let tail = if failure.restarted {
        vec![StyledSpan::new(
            copy::plugin_restarted(&sep),
            Token::TextMuted,
        )]
    } else {
        vec![]
    };
    let mut spans = ctx.clip(vec![StyledSpan::new(head, Token::TextMuted)], ctx.inner());
    let room = ctx.inner().saturating_sub(ctx.w(&spans));
    if room >= MIN_CUT {
        spans.extend(ctx.clip(tail, room));
    }
    ctx.row(spans)
}

pub(super) fn body(blocks: &[SlotBlock], ctx: &Ctx) -> Vec<StyledLine> {
    let groups: Vec<Vec<StyledLine>> = groups(blocks, ctx)
        .0
        .into_iter()
        .map(|g| g.into_iter().map(|(line, _)| line).collect())
        .collect();
    groups.join(&[ctx.blank()][..])
}

pub(super) fn groups(blocks: &[SlotBlock], ctx: &Ctx) -> (Vec<Vec<Line>>, bool) {
    let (mut groups, mut moving) = (vec![vec![]], false);
    for b in blocks {
        if let SlotBlock::Fill { fill } = b {
            groups.extend(fill_groups(fill, ctx));
            groups.push(vec![]);
        } else if let Some(last) = groups.last_mut() {
            let (lines, moves) = lines(b, ctx);
            last.extend(lines);
            moving |= moves;
        }
    }
    groups.retain(|g| !g.is_empty());
    (groups, moving)
}

fn plain(lines: Vec<StyledLine>) -> Vec<Line> {
    lines.into_iter().map(|line| (line, None)).collect()
}

fn lines(block: &SlotBlock, ctx: &Ctx) -> (Vec<Line>, bool) {
    let SlotBlock::Topology {
        root, nodes, flows, ..
    } = block
    else {
        return (plain(self::block(block, ctx)), false);
    };
    let drawing = panel(&Topology {
        root: root.as_deref(),
        nodes,
        flows,
        picked: None,
        label: None,
        layout: Layout {
            width: ctx.inner(),
            ..ctx.layout
        },
        at: ctx.at,
        now_ms: ctx.now_ms,
        cx: ctx.cx,
        pictures: ctx.pictures,
    });
    let scene = drawing.scenes.into_iter().next().map(Arc::new);
    let lines = (drawing.lines.into_iter().enumerate())
        .map(|(row, line)| (ctx.row(line.spans), strip(scene.as_ref(), row)))
        .collect();
    (lines, drawing.moving)
}

fn strip(scene: Option<&Arc<Scene>>, row: usize) -> Option<Strip> {
    scene.map(|scene| Strip {
        scene: Arc::clone(scene),
        row,
        x: LEAD.len(),
    })
}

fn block(block: &SlotBlock, ctx: &Ctx) -> Vec<StyledLine> {
    match block {
        SlotBlock::Line { spans } => {
            let spans = ctx.spans(spans);
            if spans.iter().all(|s| s.text.is_empty()) {
                vec![]
            } else {
                wrapped(spans, "", ctx)
            }
        }
        SlotBlock::Rows { rows } => rows
            .iter()
            .map(|row| {
                ctx.pair(
                    vec![StyledSpan::new(row.key.clone(), Token::TextMuted)],
                    ctx.spans(&row.value),
                    Cut::Left,
                )
            })
            .collect(),
        SlotBlock::List { items } => items
            .iter()
            .flat_map(|item| wrapped(ctx.spans(item), ctx.glyphs.bullet, ctx))
            .collect(),
        SlotBlock::Gauge {
            label,
            value,
            max,
            tone,
            note,
        } => vec![gauge(label, *value, *max, figure(*tone), note, ctx)],
        SlotBlock::Spark {
            label,
            values,
            max,
            tone,
            note,
        } => vec![spark(label, values, *max, figure(*tone), note, ctx)],
        SlotBlock::Tree { nodes } => tree(nodes, ctx),
        SlotBlock::Topology { .. } | SlotBlock::Fill { .. } | SlotBlock::NextColumn => vec![],
        SlotBlock::Unknown => vec![ctx.row(vec![StyledSpan::new(
            copy::NEEDS_NEWER_WISP,
            Token::TextMuted,
        )])],
    }
}

pub(super) fn wrapped(spans: Vec<StyledSpan>, bullet: &str, ctx: &Ctx) -> Vec<StyledLine> {
    let hang = if bullet.is_empty() {
        0
    } else {
        width(bullet, ctx.layout.width_mode) + 1
    };
    let room = ctx.inner().saturating_sub(hang).max(1);
    let mut lines = wrap(&spans, room, ctx.layout.width_mode);
    if lines.is_empty() {
        lines.push(StyledLine::default());
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let mut row = match (i, hang) {
                (_, 0) => vec![],
                (0, _) => vec![
                    StyledSpan::new(bullet, Token::TextMuted),
                    StyledSpan::plain(" "),
                ],
                _ => vec![StyledSpan::plain(" ".repeat(hang))],
            };
            row.extend(line.spans);
            ctx.row(row)
        })
        .collect()
}

fn label_and_note(
    label: &str,
    used: usize,
    note: &[SlotSpan],
    ctx: &Ctx,
) -> (Vec<StyledSpan>, usize, Vec<StyledSpan>) {
    let inner = ctx.inner();
    let label = vec![StyledSpan::new(label, Token::Text)];
    let note = ctx.spans(note);
    let (lw, nw) = (ctx.w(&label), ctx.w(&note));
    let fixed = lw + GAP + used;
    if nw > 0 && inner.saturating_sub(fixed + GAP + nw) >= MIN_METER {
        return (label, inner - fixed - GAP - nw, note);
    }
    if inner.saturating_sub(fixed) >= MIN_METER {
        return (label, inner - fixed, vec![]);
    }
    let label = ctx.clip(label, inner.saturating_sub(MIN_METER + GAP + used));
    let room = inner.saturating_sub(ctx.w(&label) + GAP + used);
    (label, room, vec![])
}

fn joined(
    label: Vec<StyledSpan>,
    parts: Vec<Vec<StyledSpan>>,
    note: Vec<StyledSpan>,
) -> Vec<StyledSpan> {
    let gap = || StyledSpan::plain(" ".repeat(GAP));
    let mut spans = label;
    for part in parts {
        spans.push(gap());
        spans.extend(part);
    }
    if !note.is_empty() {
        spans.push(gap());
        spans.extend(note);
    }
    spans
}

fn gauge(
    label: &str,
    value: u64,
    max: u64,
    tone: Token,
    note: &[SlotSpan],
    ctx: &Ctx,
) -> StyledLine {
    let pct = format!("{}%", fill_percent(value, max));
    let used = GAP + width(&pct, ctx.layout.width_mode);
    let (label, cells, note) = label_and_note(label, used, note, ctx);
    let cells = cells.max(MIN_METER).min(ctx.meter_max);
    let bar = cell_bar(value, max, cells, tone.into(), &ctx.glyphs);
    ctx.row(joined(
        label,
        vec![bar, vec![StyledSpan::new(pct, tone)]],
        note,
    ))
}

fn column(x: u8, height: u64) -> u8 {
    (0..4)
        .rev()
        .take(usize::try_from(height).unwrap_or(0))
        .fold(0, |bits, y| bits | canvas::bit(x, y))
}

pub(super) fn spark_text(values: &[u64], max: u64, glyphs: &Glyphs) -> String {
    if let Some(levels) = glyphs.spark {
        let steps = levels.len().saturating_sub(1) as u64;
        return values
            .iter()
            .map(|v| {
                let at = usize::try_from(rounded(*v, max, steps)).unwrap_or(0);
                levels.get(at).copied().unwrap_or_default()
            })
            .collect();
    }
    let height = |v: Option<&u64>| v.map_or(0, |v| 1 + rounded(*v, max, SPARK_DOTS - 1));
    let odd = values.len() % 2;
    let mut out = String::new();
    if odd == 1 {
        out.push(canvas::glyph(column(1, height(values.first()))));
    }
    for pair in values[odd..].chunks(2) {
        let bits = column(0, height(pair.first())) | column(1, height(pair.get(1)));
        out.push(canvas::glyph(bits));
    }
    out
}

fn spark(
    label: &str,
    values: &[u64],
    max: u64,
    tone: Token,
    note: &[SlotSpan],
    ctx: &Ctx,
) -> StyledLine {
    let per_cell = if ctx.glyphs.spark.is_some() { 1 } else { 2 };
    let (label, cells, note) = label_and_note(label, 0, note, ctx);
    let room = cells.max(MIN_METER) * per_cell;
    let shown = &values[values.len().saturating_sub(room)..];
    let text = spark_text(shown, max, &ctx.glyphs);
    ctx.row(joined(label, vec![vec![StyledSpan::new(text, tone)]], note))
}

fn later_at(depths: &[u32], i: usize, depth: u32) -> bool {
    depths
        .iter()
        .skip(i + 1)
        .take_while(|&&n| n >= depth)
        .any(|&n| n == depth)
}

pub(in crate::chrome) fn rails(depths: &[u32], i: usize, glyphs: &Glyphs) -> String {
    let Some(&depth) = depths.get(i).filter(|&&d| d != 0) else {
        return String::new();
    };
    let mut out: String = (1..depth)
        .map(|level| {
            if later_at(depths, i, level) {
                glyphs.pipe
            } else {
                "  "
            }
        })
        .collect();
    out.push_str(if later_at(depths, i, depth) {
        glyphs.tee
    } else {
        glyphs.elbow
    });
    out
}

fn mark(node: &SlotNode, ctx: &Ctx) -> (&'static str, Ink) {
    let (glyph, token) = dot(node.state, &ctx.glyphs);
    match node.state {
        NodeState::Working => {
            let ink = role_token(node.role.as_deref()).unwrap_or(token);
            (glyph, tree_pulse(ink, ctx.at, ctx.cx))
        }
        _ => (glyph, token.into()),
    }
}

fn label_look(role: Option<&str>) -> Look {
    match (role, role_token(role)) {
        (Some("main"), _) => Look::fg(Token::Accent).bold(),
        (_, Some(token)) => Look::fg(token),
        (_, None) => Look::fg(Token::Text),
    }
}

fn tree(nodes: &[SlotNode], ctx: &Ctx) -> Vec<StyledLine> {
    let mode = ctx.layout.width_mode;
    let inner = ctx.inner();
    let depths: Vec<_> = nodes.iter().map(|n| u32::from(n.depth)).collect();
    let lefts: Vec<(String, &'static str, Ink)> = (0..nodes.len())
        .zip(nodes)
        .map(|(i, node)| {
            let (glyph, ink) = mark(node, ctx);
            (rails(&depths, i, &ctx.glyphs), glyph, ink)
        })
        .collect();
    let facts: Vec<Vec<StyledSpan>> = nodes.iter().map(|n| ctx.spans(&n.facts)).collect();
    let leads: Vec<usize> = lefts
        .iter()
        .map(|(r, g, _)| width(r, mode) + width(g, mode) + 1)
        .collect();
    let lead = |i: usize| leads.get(i).copied().unwrap_or(0);
    let widths = |cols: usize| -> Vec<usize> {
        (0..cols)
            .map(|j| {
                facts
                    .iter()
                    .filter_map(|f| f.get(j))
                    .map(|s| width(&s.text, mode))
                    .max()
                    .unwrap_or(0)
            })
            .collect()
    };
    let block = |cols: &[usize]| {
        if cols.is_empty() {
            0
        } else {
            cols.iter().sum::<usize>() + GAP * cols.len()
        }
    };
    let mut cols = widths(facts.iter().map(Vec::len).max().unwrap_or(0));
    while !cols.is_empty()
        && (0..nodes.len()).any(|i| inner.saturating_sub(lead(i) + block(&cols)) < MIN_LABEL)
    {
        cols.pop();
    }
    let facts_w = block(&cols).saturating_sub(GAP);
    nodes
        .iter()
        .zip(lefts)
        .zip(facts)
        .enumerate()
        .map(|(i, ((node, (rail, glyph, ink)), facts))| {
            let room = inner.saturating_sub(lead(i) + block(&cols));
            let label = ctx.clip(
                vec![StyledSpan::new(
                    node.label.clone(),
                    label_look(node.role.as_deref()),
                )],
                room,
            );
            let mut spans = vec![
                StyledSpan::new(rail, Token::Line),
                StyledSpan::new(glyph, Look::fg(ink)),
                StyledSpan::plain(" "),
            ];
            spans.extend(label);
            if !cols.is_empty() {
                let pad = inner.saturating_sub(ctx.w(&spans) + facts_w);
                spans.push(StyledSpan::plain(" ".repeat(pad)));
                for (j, col) in cols.iter().enumerate() {
                    let fact = facts.get(j).cloned().unwrap_or_default();
                    let gap = if j == 0 { 0 } else { GAP };
                    let fill = col.saturating_sub(width(&fact.text, mode)) + gap;
                    spans.push(StyledSpan::plain(" ".repeat(fill)));
                    spans.push(fact);
                }
            }
            ctx.row(spans)
        })
        .collect()
}

pub(super) fn fill_groups(fill: &SlotFill, ctx: &Ctx) -> Vec<Vec<Line>> {
    let width = ctx
        .layout
        .width
        .min(ctx.meter_max.saturating_add(GAP + FILL_PERCENT + 1));
    let ctx = Ctx {
        layout: Layout {
            width,
            ..ctx.layout
        },
        ..*ctx
    };
    let tokens = |part: FillPart| {
        fill.parts
            .iter()
            .filter(|p| p.part == part)
            .fold(0u64, |s, p| s.saturating_add(p.tokens))
    };
    let base = meter_base(fill.limit, fill.window);
    let legend = |glyph: &'static str, token: Token, word: &'static str, figure: u64| {
        ctx.pair(
            vec![
                StyledSpan::new(glyph, token),
                StyledSpan::plain(" "),
                StyledSpan::new(word, Token::TextMuted),
            ],
            vec![StyledSpan::new(format_tokens(figure), Token::Text)],
            Cut::Left,
        )
    };
    let mut percent: Vec<StyledSpan> = base
        .map(|base| {
            let look = meter_level(fill.used, fill.limit, fill.auto_compact).fill_look();
            let pct = format!("{}%", fill_percent(fill.used, base));
            StyledSpan::new(format!("{pct:>FILL_PERCENT$}"), look)
        })
        .into_iter()
        .collect();
    let fits = base.filter(|_| BAR_COLUMNS + GAP + ctx.w(&percent) <= ctx.inner());
    let cells = |per: usize| fits.map(|base| fill_cells(fill, base, &tokens, BAR_COLUMNS * per));
    let shown: Vec<FillPart> = FillPart::ORDER
        .into_iter()
        .filter(|&part| tokens(part) > 0)
        .collect();
    let (rows, scene, markers) = match ctx
        .pictures
        .is_some()
        .then(|| cells(BAR_ROWS * SQUARES_PER_ROW))
    {
        Some(Some(cells)) => {
            let marks = shown
                .iter()
                .enumerate()
                .map(|(i, &part)| ((0.5, i as f32 + 0.5), Ink::from(part_look(part).1)));
            (
                vec![vec![StyledSpan::plain(" ".repeat(BAR_COLUMNS))]; BAR_ROWS],
                Some(Arc::new(fill_scene(&cells))),
                Some(Arc::new(square_scene(1, shown.len(), marks))),
            )
        }
        _ => (
            cells(BAR_ROWS).map_or(vec![], |cells| fill_bar(&cells, &ctx)),
            None,
            None,
        ),
    };
    let bar: Vec<Line> = rows
        .into_iter()
        .enumerate()
        .map(|(row, spans)| {
            let line = ctx.pair(spans, std::mem::take(&mut percent), Cut::Left);
            (line, strip(scene.as_ref(), row))
        })
        .collect();
    let mut parts = vec![];
    if !percent.is_empty() {
        parts.push((ctx.pair(vec![], percent, Cut::Left), None));
    }
    for (i, &part) in shown.iter().enumerate() {
        let (word, token) = part_look(part);
        let glyph = if markers.is_some() {
            " "
        } else {
            ctx.glyphs.cell_full
        };
        let line = legend(glyph, token, word, tokens(part));
        parts.push((line, strip(markers.as_ref(), i)));
    }
    if let Some(base) = base {
        let (word, token) = free_look();
        let free = base.saturating_sub(fill.used);
        parts.push((legend(ctx.glyphs.cell_free, token, word, free), None));
    }
    let cached = match fill.cached {
        Some(pct) => vec![StyledSpan::new(format!("{pct}%"), Token::Text)],
        None => vec![StyledSpan::new(ctx.glyphs.absent, Token::TextMuted)],
    };
    let cached = ctx.pair(
        vec![StyledSpan::new(CACHED, Token::TextMuted)],
        cached,
        Cut::Left,
    );
    vec![bar, parts, vec![(cached, None)]]
}

fn fill_part(row: usize) -> FillPart {
    FillPart::ORDER
        .get(row)
        .copied()
        .unwrap_or(FillPart::Unknown)
}

fn fill_cells(
    fill: &SlotFill,
    base: u64,
    tokens: &dyn Fn(FillPart) -> u64,
    cells: usize,
) -> Vec<Cell> {
    let rows: Vec<u64> = (0..=FillPart::ORDER.len())
        .map(|row| tokens(fill_part(row)))
        .collect();
    allocate(&Budget {
        rows: &rows,
        free: base.saturating_sub(fill.used),
        reserve: 0,
        scale: base.max(fill.used),
        cells,
    })
}

fn fill_bar(cells: &[Cell], ctx: &Ctx) -> Vec<Vec<StyledSpan>> {
    let used = |row| Look::fg(part_look(fill_part(row)).1);
    (0..BAR_ROWS)
        .map(|r| {
            cell_run(
                cells.iter().skip(r).step_by(BAR_ROWS).copied(),
                used,
                &ctx.glyphs,
            )
        })
        .collect()
}

fn fill_scene(cells: &[Cell]) -> Scene {
    let per = BAR_ROWS * SQUARES_PER_ROW;
    let squares = cells.iter().enumerate().map(|(i, cell)| {
        let at = (
            (i / per) as f32 + 0.5,
            ((i % per) as f32 + 0.5) / SQUARES_PER_ROW as f32,
        );
        let ink = match *cell {
            Cell::Used { row } => Ink::from(part_look(fill_part(row)).1),
            Cell::Free | Cell::Reserve => free_square(),
        };
        (at, ink)
    });
    square_scene(BAR_COLUMNS, BAR_ROWS, squares)
}

fn square_scene(
    width: usize,
    rows: usize,
    squares: impl Iterator<Item = ((f32, f32), Ink)>,
) -> Scene {
    let dots = squares.map(|(at, ink)| Dot {
        at,
        mark: Mark::Square,
        radius: SQUARE_RADIUS,
        ink,
        frame: 0,
        picked: false,
    });
    Scene {
        width,
        rows: 0..rows,
        dots: dots.collect(),
        ..Scene::default()
    }
}
