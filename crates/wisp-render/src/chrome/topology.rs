use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Duration;

use wisp_protocol::slot::{NodeState, SlotFlow, SlotTopologyNode};

use super::band::split_at;
use crate::canvas::{self, Canvas, Cell};
use crate::markdown::Layout;
use unicode_segmentation::UnicodeSegmentation;

use crate::motion::{
    FRAME_MIN, MotionCtx, REVEAL, REVEAL_STEP_MS, ms, picture_pulse, reveal, tree_glow, tree_pulse,
};
use crate::picture::{Dot, Light, Mark, Ring, Scene};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{WidthMode, fit_end, spans_padded, spans_width, width};
use crate::theme::glyphs::{self, Glyphs};
use crate::theme::{Ink, Token};
use crate::transcript::copy;

const MIN_GAP: usize = 3;
const MAX_GAP: usize = 7;
const DOT_CELLS: usize = 2;
const LEGEND_GAP: usize = 2;
const UP: u8 = 1;
const DOWN: u8 = 2;
const RIGHT: u8 = 4;
const WIRE_RANK: u8 = 0;
const STATES: [NodeState; 5] = [
    NodeState::Working,
    NodeState::Waiting,
    NodeState::Done,
    NodeState::Stopped,
    NodeState::Failed,
];
const URGENT: [NodeState; 5] = [
    NodeState::Failed,
    NodeState::Working,
    NodeState::Waiting,
    NodeState::Stopped,
    NodeState::Done,
];
const FIGURES: &str = "+0123456789";
const LEGEND_ROW: usize = 3;
const DOT: f32 = 0.5;
const FOLDED_LESS: f32 = 0.8 / 9.0;
const MAIN_MORE: f32 = 1.4 / 9.0;
const LEGEND_DOT: f32 = 0.4;
const GAP_LESS: f32 = 4.0 / 9.0;
const PAGE_DOT: f32 = 7.5 / 9.0;
const LONE_GAP: usize = 6;
const FLIGHT_FRAMES: (u64, u64) = (12, 36);
const IN_FLIGHT: usize = 3;
const LIGHT: Token = Token::Gleam;
const LIGHT_RANK: u8 = 1;
const ASCII_LIGHT: &str = "#";
const TRAIL: usize = 3;
const HUB: (u8, u8) = (0, 1);
const GLOW_REACH: (f64, f64) = (3.0, 1.5);
const GLOW_STEPS: f64 = 8.0;

pub struct Topology<'a> {
    pub root: Option<&'a str>,
    pub nodes: &'a [SlotTopologyNode],
    pub flows: &'a [SlotFlow],
    pub picked: Option<&'a str>,
    pub label: Option<(Option<&'a str>, &'a str)>,
    pub layout: Layout,
    pub at: Duration,
    pub now_ms: u64,
    pub cx: MotionCtx,
    pub pictures: bool,
}

pub fn dot(state: NodeState, glyphs: &Glyphs) -> (&'static str, Token) {
    match state {
        NodeState::Working => (glyphs.dot_working, Token::Working),
        NodeState::Waiting | NodeState::Unknown => (glyphs.dot_waiting, Token::TextMuted),
        NodeState::Done => (glyphs.done, Token::Ok),
        NodeState::Stopped => (glyphs.dot_stopped, Token::TextFaint),
        NodeState::Failed => (glyphs.failed, Token::Error),
    }
}

struct Tree {
    kids: Vec<Vec<usize>>,
    up: Vec<Option<usize>>,
    tops: Vec<usize>,
    need: Vec<usize>,
    levels: Vec<usize>,
    hides: Vec<Tally>,
    folded: Vec<bool>,
    root_fold: Option<Tally>,
}

#[derive(Clone, Copy)]
struct Tally {
    count: usize,
    worst: NodeState,
}

fn urgency(state: NodeState) -> usize {
    URGENT
        .iter()
        .position(|s| *s == state)
        .unwrap_or(URGENT.len())
}

fn count_cells(count: usize) -> usize {
    copy::folded(count).len()
}

impl Tree {
    fn new(nodes: &[SlotTopologyNode]) -> Tree {
        let mut index: BTreeMap<&str, usize> = BTreeMap::new();
        let mut kids = vec![Vec::new(); nodes.len()];
        let mut up = vec![None; nodes.len()];
        let mut tops = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            match node.parent.as_deref().and_then(|p| index.get(p)) {
                Some(&parent) => {
                    kids[parent].push(i);
                    up[i] = Some(parent);
                }
                None => tops.push(i),
            }
            index.entry(node.key.as_str()).or_insert(i);
        }
        let empty = Tally {
            count: 0,
            worst: NodeState::Unknown,
        };
        let mut tree = Tree {
            kids,
            up,
            tops,
            need: vec![DOT_CELLS; nodes.len()],
            levels: vec![1; nodes.len()],
            hides: vec![empty; nodes.len()],
            folded: vec![false; nodes.len()],
            root_fold: None,
        };
        for i in (0..nodes.len()).rev() {
            tree.hides[i] = tree.tally(nodes, &tree.kids[i]);
        }
        tree.measure();
        tree
    }

    fn tally(&self, nodes: &[SlotTopologyNode], of: &[usize]) -> Tally {
        let worst = of
            .iter()
            .flat_map(|&k| [nodes[k].state, self.hides[k].worst])
            .min_by_key(|&s| urgency(s));
        Tally {
            count: of.iter().map(|&k| 1 + self.hides[k].count).sum(),
            worst: worst.unwrap_or(NodeState::Unknown),
        }
    }

    fn measure(&mut self) {
        for i in (0..self.kids.len()).rev() {
            let kids = &self.kids[i];
            (self.need[i], self.levels[i]) = if self.folded[i] {
                (DOT_CELLS + count_cells(self.hides[i].count), 1)
            } else if kids.is_empty() {
                (DOT_CELLS, 1)
            } else {
                (
                    kids.iter().map(|&k| self.need[k]).sum(),
                    1 + kids.iter().map(|&k| self.levels[k]).max().unwrap_or(0),
                )
            };
        }
    }

    fn wide(&self, width: usize) -> bool {
        self.tops.iter().map(|&t| self.need[t]).sum::<usize>() >= width
    }

    fn shown(&self) -> Vec<bool> {
        let mut shown = vec![false; self.kids.len()];
        self.tops.iter().for_each(|&t| shown[t] = true);
        for i in 0..self.kids.len() {
            if shown[i] && !self.folded[i] {
                self.kids[i].iter().for_each(|&k| shown[k] = true);
            }
        }
        shown
    }

    fn fold(&mut self, nodes: &[SlotTopologyNode], width: usize, rooted: bool) {
        let mut made = Vec::new();
        while self.wide(width) {
            let shown = self.shown();
            let saves = |i: usize| {
                self.need[i].saturating_sub(DOT_CELLS + count_cells(self.hides[i].count))
            };
            let pick = (0..self.kids.len())
                .filter(|&i| shown[i] && !self.folded[i] && saves(i) > 0)
                .max_by_key(|&i| (urgency(self.hides[i].worst), saves(i), i));
            let Some(i) = pick else {
                if rooted {
                    self.root_fold = Some(self.tally(nodes, &self.tops));
                    self.tops.clear();
                }
                return;
            };
            self.folded[i] = true;
            made.push(i);
            self.measure();
        }
        for &i in made.iter().rev() {
            self.folded[i] = false;
            self.measure();
            if self.wide(width) {
                self.folded[i] = true;
                self.measure();
            }
        }
    }

    fn deepest(&self, tops: &[usize]) -> usize {
        tops.iter().map(|&t| self.levels[t]).max().unwrap_or(0)
    }
}

struct Bus {
    from: (usize, usize),
    to: Vec<(usize, usize)>,
    ry: usize,
}

struct Fold {
    at: (usize, usize),
    tally: Tally,
}

struct Placed {
    width: usize,
    height: usize,
    pos: Vec<Option<(usize, usize)>>,
    root: Option<(usize, usize)>,
    buses: Vec<Bus>,
    folds: Vec<Fold>,
    lanes: Vec<usize>,
    lane_row: usize,
}

impl Placed {
    fn page(tree: &Tree, rooted: bool, width: usize, height: usize) -> Placed {
        let lift = usize::from(rooted);
        let levels = lift + tree.deepest(&tree.tops);
        let gap =
            (height.saturating_sub(1) / levels.saturating_sub(1).max(1)).clamp(MIN_GAP, MAX_GAP);
        let used = levels.saturating_sub(1) * gap + 1;
        let first = height.saturating_sub(used) / 2;
        let rows: Vec<usize> = (0..levels).map(|d| first + d * gap).collect();
        let need: Vec<usize> = tree.tops.iter().map(|&t| tree.need[t]).collect();
        let sum: usize = need.iter().sum();
        let fits = sum < width;
        let spare = if fits {
            (width - sum) as f64 / need.len().max(1) as f64
        } else {
            0.0
        };
        let mut out = Placed {
            width: if fits { width } else { sum + 1 },
            height: height.max(used),
            pos: vec![None; tree.need.len()],
            root: None,
            buses: Vec::new(),
            folds: Vec::new(),
            lanes: Vec::new(),
            lane_row: first + lift * gap,
        };
        let mut a = 0.0;
        for (&top, n) in tree.tops.iter().zip(&need) {
            let w = *n as f64 + spare;
            out.lanes.push(a as usize);
            out.put(tree, top, a + w / 2.0, (a, a + w), lift, &rows);
            a += w;
        }
        if rooted {
            let extra = tree.root_fold.map_or(0, |t| count_cells(t.count));
            let root = (out.width.saturating_sub(extra) / 2, first);
            out.root = Some(root);
            if let Some(tally) = tree.root_fold {
                out.folds.push(Fold { at: root, tally });
            }
            if !tree.tops.is_empty() {
                out.join(root, &tree.tops, gap);
            }
        }
        out
    }

    fn put(
        &mut self,
        tree: &Tree,
        node: usize,
        centre: f64,
        (x0, x1): (f64, f64),
        depth: usize,
        rows: &[usize],
    ) {
        let tally = tree.hides[node];
        let extra = if tree.folded[node] {
            count_cells(tally.count)
        } else {
            0
        };
        let at = ((centre - extra as f64 / 2.0) as usize, rows[depth]);
        self.pos[node] = Some(at);
        if tree.folded[node] {
            self.folds.push(Fold { at, tally });
        } else {
            self.place(tree, node, x0, x1, depth + 1, rows);
        }
    }

    fn place(
        &mut self,
        tree: &Tree,
        parent: usize,
        x0: f64,
        x1: f64,
        depth: usize,
        rows: &[usize],
    ) {
        let kids = &tree.kids[parent];
        let Some(from) = self.pos[parent].filter(|_| !kids.is_empty()) else {
            return;
        };
        let total = kids.iter().map(|&k| tree.need[k]).sum::<usize>() as f64;
        let mut a = x0;
        for &kid in kids {
            let b = a + (x1 - x0) * tree.need[kid] as f64 / total;
            self.put(tree, kid, (a + b) / 2.0, (a, b), depth, rows);
            a = b;
        }
        self.join(from, kids, rows[depth] - rows[depth - 1]);
    }

    fn join(&mut self, from: (usize, usize), kids: &[usize], gap: usize) {
        self.buses.push(Bus {
            from,
            to: kids.iter().filter_map(|&k| self.pos[k]).collect(),
            ry: from.1 + (gap / 2).max(1),
        });
    }

    fn spot(&self, end: Option<usize>) -> Option<(usize, usize)> {
        end.map_or(self.root, |i| self.pos[i])
    }

    fn link(&self, parent: (usize, usize), kid: (usize, usize)) -> Option<[(usize, usize); 4]> {
        let bus = (self.buses.iter()).find(|b| b.from == parent && b.to.contains(&kid))?;
        Some([parent, (parent.0, bus.ry), (kid.0, bus.ry), kid])
    }

    fn wires(&self) -> BTreeMap<(usize, usize), u8> {
        let mut wires = BTreeMap::new();
        let mut mark = |x: usize, y: usize, flags: u8| *wires.entry((x, y)).or_insert(0) |= flags;
        for bus in &self.buses {
            let (px, py) = bus.from;
            (py + 1..bus.ry).for_each(|y| mark(px, y, UP | DOWN));
            mark(px, bus.ry, UP);
            let (lo, hi) = bus.span();
            (lo..=hi).for_each(|x| mark(x, bus.ry, if x < hi { RIGHT } else { 0 }));
            for &(kx, ky) in &bus.to {
                mark(kx, bus.ry, DOWN);
                (bus.ry + 1..ky).for_each(|y| mark(kx, y, UP | DOWN));
            }
        }
        wires
    }
}

fn wire() -> Look {
    Look::fg(Token::Line)
}

fn wire_glyph(flags: u8, g: &Glyphs) -> &'static str {
    match (flags & UP != 0, flags & DOWN != 0, flags & RIGHT != 0) {
        (true, true, true) => g.wire_cross,
        (true, true, false) => g.wire_drop,
        (false, true, true) => g.wire_first,
        (false, true, false) => g.wire_last,
        (true, false, _) => g.wire_join,
        (false, false, _) => g.wire_bar,
    }
}

fn draw(
    placed: &Placed,
    view: &Topology<'_>,
    lit: &Flights,
    picked: Option<(usize, usize)>,
) -> Canvas {
    let glyphs = Glyphs::of(view.layout.glyphs);
    let mut grid = Canvas::new(placed.width, placed.height);
    let wires = placed.wires();
    for (&at, &flags) in wires.iter().filter(|_| !view.pictures) {
        let glyph = wire_glyph(flags, &glyphs);
        match glyph.chars().next().and_then(canvas::braille) {
            Some(bits) => grid.bits(at, bits, WIRE_RANK, wire()),
            None => grid.text(at, glyph, wire()),
        }
    }
    for (path, h) in lit.lights.iter().filter(|_| !view.pictures) {
        light(&mut grid, &trace(path, &wires), *h, view.cx);
    }
    if let Some(root) = placed.root.filter(|_| !view.pictures) {
        grid.text(root, glyphs.wisp, Look::fg(Token::Accent));
    }
    for (node, at) in view.nodes.iter().zip(&placed.pos) {
        let Some(at) = at.filter(|_| !view.pictures) else {
            continue;
        };
        let (glyph, token) = dot(node.state, &glyphs);
        let ink = match node.state {
            NodeState::Working => tree_pulse(token, view.at, view.cx),
            _ => Ink::from(token),
        };
        grid.text(at, glyph, Look::fg(ink));
    }
    for fold in &placed.folds {
        let look = Look::fg(dot(fold.tally.worst, &glyphs).1);
        let (x, y) = fold.at;
        for (i, figure) in copy::folded(fold.tally.count).char_indices() {
            let cell = FIGURES.find(figure).and_then(|f| FIGURES.get(f..=f));
            grid.text((x + 1 + i, y), cell.unwrap_or(" "), look);
        }
    }
    if let Some((x, y)) = picked.filter(|_| !view.pictures) {
        bracket(&mut grid, x, y);
    }
    if !view.pictures {
        rings(&mut grid, placed, &lit.rings, view.cx);
    }
    grid
}

type Pip = ((usize, usize), (u8, u8));

fn trace(path: &[(usize, usize)], wires: &BTreeMap<(usize, usize), u8>) -> Vec<Pip> {
    let toward = |a: usize, b: usize| (a + usize::from(b > a)).saturating_sub(usize::from(b < a));
    let mut cells: Vec<(usize, usize)> = path.first().copied().into_iter().collect();
    for pair in path.windows(2) {
        let mut at = pair[0];
        while at != pair[1] {
            at = (toward(at.0, pair[1].0), toward(at.1, pair[1].1));
            cells.push(at);
        }
    }
    let line = |at| {
        let glyph = wire_glyph(wires.get(at).copied().unwrap_or(0), &glyphs::UNICODE);
        glyph.chars().next().and_then(canvas::braille).unwrap_or(0)
    };
    let mut out = Vec::new();
    for w in cells.windows(3) {
        let (bits, at) = (line(&w[1]), w[1]);
        let mut dots = spoke(at, w[0]).to_vec();
        dots.push(HUB);
        dots.extend(spoke(at, w[2]).iter().rev());
        out.extend(
            (dots.into_iter())
                .filter(|&(x, y)| bits & canvas::bit(x, y) != 0)
                .map(|dot| (at, dot)),
        );
    }
    out
}

fn spoke((x, y): (usize, usize), (nx, ny): (usize, usize)) -> &'static [(u8, u8)] {
    match (nx > x, ny.cmp(&y)) {
        (_, Ordering::Less) => &[(0, 0)],
        (_, Ordering::Greater) => &[(0, 3), (0, 2)],
        (true, Ordering::Equal) => &[(1, 1)],
        (false, Ordering::Equal) => &[],
    }
}

fn across(dots: &[Pip], i: usize, head: bool) -> u8 {
    let spot = |&((cx, cy), (dx, dy)): &Pip| (2 * cx + usize::from(dx), 4 * cy + usize::from(dy));
    let (_, (x, y)) = dots[i];
    let me = spot(&dots[i]);
    let near: Vec<(usize, usize)> = [i.checked_sub(1), Some(i + 1)]
        .into_iter()
        .flatten()
        .filter_map(|j| dots.get(j).map(spot))
        .collect();
    let upright = near.iter().any(|p| p.0 == me.0);
    let level = near.iter().any(|p| p.1 == me.1);
    let mut bits = 0;
    if upright {
        bits |= canvas::bit(x ^ 1, y);
    }
    if level && (head || upright) {
        bits |= canvas::bit(x, y + 1) | y.checked_sub(1).map_or(0, |up| canvas::bit(x, up));
    }
    bits
}

fn light(grid: &mut Canvas, dots: &[Pip], h: usize, cx: MotionCtx) {
    let Some(head) = (0..dots.len())
        .filter(|&i| dots.get(i + 1).is_none_or(|next| next.0 != dots[i].0))
        .nth(h)
    else {
        return;
    };
    if cx.ascii {
        grid.text(dots[head].0, ASCII_LIGHT, Look::fg(LIGHT));
        return;
    }
    let first = head
        .checked_sub(1)
        .filter(|&i| dots[i].0 == dots[head].0)
        .unwrap_or(head);
    let mut behind: Vec<(usize, usize)> = dots[..first].iter().map(|d| d.0).collect();
    behind.dedup();
    let behind = behind.into_iter().rev().filter(|&at| at != dots[first].0);
    for (k, at) in behind.take(TRAIL).enumerate() {
        let u = 1.0 - (k + 1) as f64 / (TRAIL + 1) as f64;
        let fade = Ink::mix(LIGHT, Token::Line, u, Token::Line);
        grid.bits(at, 0, LIGHT_RANK, Look::fg(fade));
    }
    for i in first..=head {
        grid.bits(
            dots[i].0,
            across(dots, i, i == head),
            LIGHT_RANK,
            Look::fg(LIGHT),
        );
    }
}

fn rings(grid: &mut Canvas, placed: &Placed, rings: &[((usize, usize), Duration)], cx: MotionCtx) {
    let counts: Vec<(usize, usize)> = (placed.folds.iter())
        .flat_map(|f| (1..=count_cells(f.tally.count)).map(move |i| (f.at.0 + i, f.at.1)))
        .collect();
    let mut rings = rings.to_vec();
    rings.sort_by_key(|&((x, y), _)| (y, x));
    for ((x, y), open) in rings {
        let ring = reveal(open + Duration::from_millis(REVEAL_STEP_MS), cx);
        for (at, half) in [
            (x.checked_sub(1), ring.cells[0]),
            (Some(x + 1), ring.cells[2]),
        ] {
            let Some(at) = at.map(|x| (x, y)).filter(|at| !counts.contains(at)) else {
                continue;
            };
            if let Some(slot) = grid.cell_mut(at) {
                let look = slot.map_or_else(Look::default, |cell| cell.look);
                *slot = Some(Cell {
                    mark: canvas::Mark::Char(half.ch),
                    look: Look {
                        fg: Some(Ink::from(LIGHT)),
                        ..look
                    },
                });
            }
        }
    }
}

fn glow(grid: &mut Canvas, strength: f64, dots: impl Iterator<Item = (usize, usize)>) {
    let (rx, ry) = GLOW_REACH;
    let mut lit: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for (x, y) in dots {
        for dy in -(ry as isize)..=ry as isize {
            for dx in -(rx as isize)..=rx as isize {
                let d = ((dx as f64 / rx).powi(2) + (dy as f64 / ry).powi(2)).sqrt();
                let u = (strength * (1.0 - d).max(0.0).powi(2) * GLOW_STEPS).round() / GLOW_STEPS;
                let Some(at) = x.checked_add_signed(dx).zip(y.checked_add_signed(dy)) else {
                    continue;
                };
                if u > 0.0 {
                    let most = lit.entry(at).or_insert(0.0);
                    *most = most.max(u);
                }
            }
        }
    }
    for (at, u) in lit {
        let tint = Ink::mix(Token::HueCool, Token::Bg, u, Token::Bg);
        match grid.cell_mut(at) {
            Some(Some(cell)) if cell.look.bg.is_none() => cell.look.bg = Some(tint),
            Some(slot @ None) => {
                *slot = Some(Cell {
                    mark: canvas::Mark::Text(" "),
                    look: Look::default().on(tint),
                });
            }
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
pub struct Drawing {
    pub lines: Vec<StyledLine>,
    pub scenes: Vec<Scene>,
    pub moving: bool,
}

pub fn panel(view: &Topology<'_>) -> Drawing {
    if view.nodes.is_empty() {
        return Drawing::default();
    }
    let glyphs = Glyphs::of(view.layout.glyphs);
    let rooted = view.root.is_some();
    let mut tree = Tree::new(view.nodes);
    tree.fold(view.nodes, view.layout.width, rooted);
    let placed = Placed::page(&tree, rooted, view.layout.width, 0);
    let lit = flights(view, &tree, &placed);
    let moving = lit.any();
    let mut out = draw(&placed, view, &lit, None).lines();
    out.push(StyledLine::default());
    let mut marks = Vec::new();
    for row in legend_entries(view, &glyphs).chunks(LEGEND_ROW) {
        let mut spans = Vec::new();
        for (i, (mark, token, [glyph, word])) in row.iter().enumerate() {
            if i > 0 {
                spans.push(StyledSpan::plain(" ".repeat(LEGEND_GAP)));
            }
            legend_mark(
                view,
                (*mark, *token, glyph),
                (&mut spans, out.len()),
                &mut marks,
            );
            spans.push(word.clone());
        }
        out.push(StyledLine::new(spans));
    }
    let scene = view.pictures.then(|| {
        let window = Window {
            at: (0, 0),
            size: (placed.width, placed.height),
            dot: DOT,
            picked: None,
        };
        scene(&placed, view, &out, marks, &window, lit)
    });
    Drawing {
        lines: out,
        scenes: scene.into_iter().collect(),
        moving,
    }
}

fn legend_mark(
    view: &Topology<'_>,
    (mark, token, glyph): (Mark, Token, &StyledSpan),
    (spans, y): (&mut Vec<StyledSpan>, usize),
    marks: &mut Vec<Dot>,
) {
    let mode = view.layout.width_mode;
    if !view.pictures {
        spans.push(glyph.clone());
        return;
    }
    marks.push(Dot {
        at: centre((spans_width(spans, mode), y)),
        mark,
        radius: LEGEND_DOT,
        ink: Ink::from(token),
        halo: false,
        picked: false,
    });
    spans.push(StyledSpan::plain(" ".repeat(width(&glyph.text, mode))));
}

fn centre((x, y): (usize, usize)) -> (f32, f32) {
    (x as f32 + 0.5, y as f32 + 0.5)
}

struct Window {
    at: (usize, usize),
    size: (usize, usize),
    dot: f32,
    picked: Option<(usize, usize)>,
}

impl Window {
    fn inside(&self, (x, y): (usize, usize)) -> Option<(f32, f32)> {
        let (x, y) = (x.checked_sub(self.at.0)?, y.checked_sub(self.at.1)?);
        (x < self.size.0 && y < self.size.1).then(|| centre((x, y)))
    }
}

fn scene(
    placed: &Placed,
    view: &Topology<'_>,
    lines: &[StyledLine],
    marks: Vec<Dot>,
    window: &Window,
    lit: Flights,
) -> Scene {
    let glyphs = Glyphs::of(view.layout.glyphs);
    let root = placed.root.and_then(|at| {
        Some(Dot {
            at: window.inside(at)?,
            mark: Mark::Root,
            radius: window.dot + MAIN_MORE,
            ink: Ink::from(Token::Accent),
            halo: true,
            picked: window.picked == Some(at),
        })
    });
    let dots = view.nodes.iter().zip(&placed.pos).filter_map(|(node, at)| {
        let at = (*at)?;
        let token = dot(node.state, &glyphs).1;
        let folded = placed.folds.iter().any(|f| f.at == at);
        Some(Dot {
            at: window.inside(at)?,
            mark: Mark::State(node.state),
            radius: if folded {
                window.dot - FOLDED_LESS
            } else {
                window.dot
            },
            ink: match node.state {
                NodeState::Working => picture_pulse(token, view.at, view.cx),
                _ => Ink::from(token),
            },
            halo: true,
            picked: window.picked == Some(at),
        })
    });
    let drawn: Vec<Dot> = root.into_iter().chain(dots).collect();
    let (left, top) = (window.at.0 as f32, window.at.1 as f32);
    let shift = |(x, y): (f32, f32)| (x - left, y - top);
    let lights = lit.lights.into_iter().map(|(path, h)| {
        let along = (h + 1) as f32 / route_len(&path) as f32;
        Light {
            path: path.into_iter().map(|cell| shift(centre(cell))).collect(),
            along,
            radius: window.dot,
        }
    });
    let rings = lit.rings.into_iter().filter_map(|(cell, open)| {
        let at = window.inside(cell)?;
        let on = *drawn.iter().find(|d| d.at == at)?;
        let open = open.as_secs_f32() / REVEAL.as_secs_f32();
        Some(Ring { on, open })
    });
    Scene {
        width: view.layout.width,
        rows: 0..lines.len(),
        lights: lights.collect(),
        rings: rings.collect(),
        dots: drawn.into_iter().chain(marks).collect(),
        lines: (placed.buses.iter().flat_map(Bus::lines))
            .map(|line| line.into_iter().map(shift).collect())
            .collect(),
        text_cells: text_cells(lines, view.layout.width_mode),
    }
}

fn chain(tree: &Tree, placed: &Placed, end: Option<usize>) -> Vec<Option<usize>> {
    let mut out = Vec::new();
    let mut at = end;
    while let Some(i) = at {
        out.push(Some(i));
        at = tree.up[i];
    }
    out.push(None);
    let drawn = out.iter().position(|&e| placed.spot(e).is_some());
    out.split_off(drawn.unwrap_or(out.len()))
}

fn route(
    tree: &Tree,
    placed: &Placed,
    (from, to): (Option<usize>, Option<usize>),
) -> Option<Vec<(usize, usize)>> {
    let (up, down) = (chain(tree, placed, from), chain(tree, placed, to));
    let meet = up.iter().position(|e| down.contains(e))?;
    let fall = down.iter().position(|e| *e == up[meet])?;
    let link = |pair: &[Option<usize>]| placed.link(placed.spot(pair[1])?, placed.spot(pair[0])?);
    let mut cells = vec![placed.spot(*up.first()?)?];
    for pair in up[..=meet].windows(2) {
        cells.extend(link(pair)?.into_iter().rev());
    }
    for pair in down[..=fall].windows(2).rev() {
        cells.extend(link(pair)?);
    }
    let mut out: Vec<(usize, usize)> = Vec::new();
    for cell in cells {
        out.push(cell);
        while let &[.., a, b, c] = out.as_slice()
            && ((a.0 == b.0 && b.0 == c.0) || (a.1 == b.1 && b.1 == c.1))
        {
            out.remove(out.len() - 2);
        }
    }
    Some(out)
}

fn route_len(path: &[(usize, usize)]) -> usize {
    path.windows(2)
        .map(|w| w[0].0.abs_diff(w[1].0) + w[0].1.abs_diff(w[1].1))
        .sum()
}

struct Flights {
    lights: Vec<(Vec<(usize, usize)>, usize)>,
    rings: Vec<((usize, usize), Duration)>,
}

impl Flights {
    fn any(&self) -> bool {
        !self.lights.is_empty() || !self.rings.is_empty()
    }

    fn landing(&self) -> Vec<(usize, usize)> {
        let flying = self
            .lights
            .iter()
            .filter_map(|(path, _)| path.last().copied());
        flying.chain(self.rings.iter().map(|ring| ring.0)).collect()
    }
}

fn flights(view: &Topology<'_>, tree: &Tree, placed: &Placed) -> Flights {
    let (mut lights, mut rings, mut flying) = (Vec::new(), Vec::new(), Vec::new());
    if view.cx.reduced() {
        return Flights { lights, rings };
    }
    let end = |key: &Option<String>| match key {
        None => Some(None),
        Some(key) => view.nodes.iter().position(|n| n.key == *key).map(Some),
    };
    let mut flows: Vec<&SlotFlow> = view.flows.iter().collect();
    flows.sort_by_key(|flow| flow.at_ms);
    for flow in flows {
        let ends = end(&flow.from).zip(end(&flow.to));
        let Some(path) = ends.and_then(|ends| route(tree, placed, ends)) else {
            continue;
        };
        let Some(&to) = path.last() else {
            continue;
        };
        let n = route_len(&path).saturating_sub(1) as u64;
        flying.retain(|&lands| lands > flow.at_ms);
        let flies = n > 0 && flying.len() < IN_FLIGHT;
        let lands = if flies {
            let k = n.div_ceil(FLIGHT_FRAMES.1);
            let f = FLIGHT_FRAMES.0.div_ceil(n);
            let step = f * ms(FRAME_MIN) * view.cx.slowdown();
            let lands = flow.at_ms + n.div_ceil(k) * step;
            flying.push(lands);
            if view.now_ms < lands {
                let age = view.now_ms.saturating_sub(flow.at_ms);
                let h = (k * (age / step)) as usize;
                lights.push((path, h));
            }
            lands
        } else {
            flow.at_ms
        };
        let open = view.now_ms.checked_sub(lands).map(Duration::from_millis);
        if let Some(open) = open.filter(|open| *open < REVEAL) {
            rings.push((to, open));
        }
    }
    Flights { lights, rings }
}

impl Bus {
    fn span(&self) -> (usize, usize) {
        let px = self.from.0;
        let xs = self.to.iter().map(|k| k.0).chain([px]);
        (xs.clone().min().unwrap_or(px), xs.max().unwrap_or(px))
    }

    fn lines(&self) -> Vec<Vec<(f32, f32)>> {
        let px = self.from.0;
        let (lo, hi) = self.span();
        let side = |x: usize| (x == lo && lo < px) || (x == hi && hi > px);
        let ends = |x: usize| self.to.iter().filter(move |k| k.0 == x && side(x));
        let mut bar: Vec<(f32, f32)> = ends(lo).map(|&k| centre(k)).collect();
        bar.extend([centre((lo, self.ry)), centre((hi, self.ry))]);
        bar.extend(ends(hi).map(|&k| centre(k)));
        let mut out = vec![vec![centre(self.from), centre((px, self.ry))]];
        if lo < hi {
            out.push(bar);
        }
        let drops = self.to.iter().filter(|k| !side(k.0));
        out.extend(drops.map(|&(x, y)| vec![centre((x, self.ry)), centre((x, y))]));
        out
    }
}

fn text_cells(lines: &[StyledLine], mode: WidthMode) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (y, line) in lines.iter().enumerate() {
        let mut x = 0;
        for span in &line.spans {
            let tinted = span.look.bg == Some(Token::AccentTint.into());
            for grapheme in span.text.graphemes(true) {
                let w = width(grapheme, mode);
                if tinted || !grapheme.trim().is_empty() {
                    out.extend((x..x + w).map(|c| (c, y)));
                }
                x += w;
            }
        }
    }
    out
}

type Spot<'a> = (Option<&'a str>, (usize, usize));

fn spots<'a>(view: &Topology<'a>, placed: &Placed) -> Vec<Spot<'a>> {
    let root = placed.root.map(|at| (None, at));
    let nodes = view.nodes.iter().zip(&placed.pos);
    let nodes = nodes.filter_map(|(node, at)| Some((Some(node.key.as_str()), (*at)?)));
    root.into_iter().chain(nodes).collect()
}

fn page_dot(spots: &[Spot<'_>]) -> f32 {
    let mut rows: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &(_, (x, y)) in spots {
        rows.entry(y).or_default().push(x);
    }
    let gaps = rows.into_values().flat_map(|mut xs| {
        xs.sort_unstable();
        xs.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>()
    });
    let gap = gaps.filter(|g| *g > 0).min().unwrap_or(LONE_GAP);
    (gap as f32 / 2.0 - GAP_LESS).clamp(DOT, PAGE_DOT)
}

struct Paged<'a> {
    tree: Tree,
    placed: Placed,
    spots: Vec<Spot<'a>>,
    picked: Option<(usize, usize)>,
    at: (usize, usize),
    canvas: usize,
}

fn paged<'a>(view: &Topology<'a>, height: usize, legend: usize) -> Paged<'a> {
    let (width, canvas) = (view.layout.width, height.saturating_sub(legend).max(1));
    let tree = Tree::new(view.nodes);
    let placed = Placed::page(&tree, view.root.is_some(), width, canvas);
    let spots = spots(view, &placed);
    let picked = spots
        .iter()
        .find(|(key, _)| *key == view.picked)
        .map(|s| s.1);
    let (px, py) = picked.unwrap_or((0, 0));
    let top = py.saturating_sub(canvas / 2).min(placed.height - canvas);
    let lanes = &placed.lanes;
    let start = lanes.iter().copied().rev().find(|&s| s <= px).unwrap_or(0);
    let end = lanes
        .iter()
        .copied()
        .find(|&s| s > px)
        .unwrap_or(placed.width);
    let left = if end - start < width {
        lanes
            .iter()
            .copied()
            .find(|&s| s + width > px + 1)
            .unwrap_or(0)
    } else {
        (px + 2).saturating_sub(width)
    }
    .min(placed.width - width);
    Paged {
        tree,
        placed,
        spots,
        picked,
        at: (left, top),
        canvas,
    }
}

pub fn page(view: &Topology<'_>, height: usize) -> Drawing {
    if view.nodes.is_empty() {
        return Drawing::default();
    }
    let glyphs = Glyphs::of(view.layout.glyphs);
    let (legend, marks) = legend(view, &glyphs);
    let Paged {
        tree,
        placed,
        spots,
        picked,
        at: (left, top),
        canvas,
    } = paged(view, height, legend.len());
    let width = view.layout.width;
    let lit = flights(view, &tree, &placed);
    let moving = lit.any();
    let full = draw(&placed, view, &lit, picked);
    let mut shown = full.crop((left, top), width, canvas);
    let faint = Look::fg(Token::TextFaint);
    let side = placed
        .lane_row
        .checked_sub(top)
        .filter(|r| *r < canvas)
        .unwrap_or(canvas / 2);
    let marks_at = [
        (top > 0, 0, 0, glyphs.more_up),
        (
            top + canvas < placed.height,
            canvas - 1,
            0,
            glyphs.more_down,
        ),
        (left > 0, side, 0, glyphs.more_left),
        (
            left + width < placed.width,
            side,
            width.saturating_sub(1),
            glyphs.more_right,
        ),
    ];
    for (on, y, x, glyph) in marks_at {
        let dotted = spots.iter().any(|s| s.1 == (x + left, y + top));
        let free = shown
            .cell_mut((x, y))
            .is_some_and(|c| c.is_none_or(|c| c.look == wire()));
        if on && free && !dotted {
            shown.text((x, y), glyph, faint);
        }
    }
    if let Some(strength) = tree_glow(view.at, view.cx).filter(|_| !view.pictures) {
        let landing = lit.landing();
        let working = (view.nodes.iter().zip(&placed.pos))
            .filter(|(node, _)| node.state == NodeState::Working)
            .filter_map(|(_, at)| at.filter(|at| !landing.contains(at)));
        let inside = working.filter_map(|(x, y)| Some((x.checked_sub(left)?, y.checked_sub(top)?)));
        glow(&mut shown, strength, inside);
    }
    let mut out = shown.lines();
    hover_label(view, &spots, (left, top), &mut out, &glyphs);
    let below = out.len() as f32;
    let marks = marks.into_iter().map(|mark| Dot {
        at: (mark.at.0, mark.at.1 + below),
        ..mark
    });
    out.extend(legend);
    if !view.pictures {
        return Drawing {
            lines: out,
            scenes: Vec::new(),
            moving,
        };
    }
    let window = Window {
        at: (left, top),
        size: (width, canvas),
        dot: page_dot(&spots),
        picked,
    };
    let topology = Scene {
        rows: 0..canvas,
        ..scene(&placed, view, &out, Vec::new(), &window, lit)
    };
    let legend = Scene {
        width,
        rows: canvas..out.len(),
        dots: marks.collect(),
        text_cells: topology.text_cells.clone(),
        ..Scene::default()
    };
    Drawing {
        lines: out,
        scenes: vec![topology, legend],
        moving,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toward {
    Up,
    Down,
    Left,
    Right,
}

pub fn step<'a>(view: &Topology<'a>, toward: Toward) -> Option<Option<&'a str>> {
    let placed = Placed::page(
        &Tree::new(view.nodes),
        view.root.is_some(),
        view.layout.width,
        0,
    );
    let spots = spots(view, &placed);
    let (fx, fy) = spots.iter().find(|(key, _)| *key == view.picked)?.1;
    let ahead = |&(x, y): &(usize, usize)| match toward {
        Toward::Up => y < fy,
        Toward::Down => y > fy,
        Toward::Left => x < fx,
        Toward::Right => x > fx,
    };
    let near = spots.into_iter().filter(|(_, at)| ahead(at));
    near.min_by_key(|&(_, (x, y))| (y.abs_diff(fy), x.abs_diff(fx), x))
        .map(|(key, _)| key)
}

pub fn key_at<'a>(
    view: &Topology<'a>,
    height: usize,
    (x, y): (usize, usize),
) -> Option<Option<&'a str>> {
    if view.nodes.is_empty() {
        return None;
    }
    let legend = legend(view, &Glyphs::of(view.layout.glyphs)).0.len();
    let paged = paged(view, height, legend);
    if y >= paged.canvas || x >= view.layout.width {
        return None;
    }
    let (x, y) = (x + paged.at.0, y + paged.at.1);
    let near = paged.spots.into_iter();
    let near = near.filter(|&(_, (sx, sy))| sy == y && sx.abs_diff(x) <= 1);
    near.min_by_key(|&(_, (sx, _))| (sx.abs_diff(x), sx))
        .map(|(key, _)| key)
}

fn hover_label(
    view: &Topology<'_>,
    spots: &[Spot<'_>],
    (left, top): (usize, usize),
    lines: &mut [StyledLine],
    glyphs: &Glyphs,
) {
    let Some((key, name)) = view.label else {
        return;
    };
    let Some(&(_, (x, y))) = spots.iter().find(|(spot, _)| *spot == key) else {
        return;
    };
    let (Some(x), Some(y)) = (x.checked_sub(left), y.checked_sub(top)) else {
        return;
    };
    if x >= view.layout.width || y >= lines.len() || lines.len() < 2 {
        return;
    }
    let row = if y + 1 == lines.len() { y - 1 } else { y + 1 };
    let mode = view.layout.width_mode;
    let name = fit_end(name, view.layout.width.saturating_sub(2), glyphs.more, mode);
    if name.is_empty() {
        return;
    }
    let cols = width(&name, mode) + 2;
    let start = x.saturating_sub(cols / 2).min(view.layout.width - cols);
    let [head, tail] = split_at(std::mem::take(&mut lines[row].spans), start, mode);
    let [_, tail] = split_at(tail, cols, mode);
    let look = Look::fg(Token::Text).on(Token::AccentTint).bold();
    let mut spans = spans_padded(head, start, mode);
    spans.push(StyledSpan::new(format!(" {name} "), look));
    spans.extend(tail);
    lines[row] = StyledLine::new(spans);
}

fn bracket(canvas: &mut Canvas, x: usize, y: usize) {
    let band = |look: Look| look.on(Token::AccentTint).bold_without_colour();
    if let Some(Some(cell)) = canvas.cell_mut((x, y)) {
        cell.look = band(cell.look);
    }
    let edges = [(x.checked_sub(1), "["), (Some(x + 1), "]")];
    for (at, glyph) in edges {
        if let Some(at) = at {
            canvas.text((at, y), glyph, band(Look::fg(Token::Accent)));
        }
    }
}

fn legend_entries(view: &Topology<'_>, glyphs: &Glyphs) -> Vec<(Mark, Token, [StyledSpan; 2])> {
    let root = view
        .root
        .map(|word| (Mark::Root, glyphs.wisp, Token::Accent, word));
    let states = STATES.map(|s| {
        let (glyph, token) = dot(s, glyphs);
        (Mark::State(s), glyph, token, copy::node_word(s))
    });
    root.into_iter()
        .chain(states)
        .map(|(mark, glyph, token, word)| {
            let word = StyledSpan::new(format!(" {word}"), Token::TextFaint);
            (mark, token, [StyledSpan::new(glyph, token), word])
        })
        .collect()
}

fn legend(view: &Topology<'_>, glyphs: &Glyphs) -> (Vec<StyledLine>, Vec<Dot>) {
    let mode = view.layout.width_mode;
    let room = view.layout.width.saturating_sub(2);
    let mut rows: Vec<Vec<StyledSpan>> = Vec::new();
    let mut marks = Vec::new();
    let mut used = 0;
    for (mark, token, [glyph, word]) in legend_entries(view, glyphs) {
        let size = width(&glyph.text, mode) + width(&word.text, mode);
        let fits = !rows.is_empty() && used + LEGEND_GAP + size <= room;
        let lead = if fits {
            used += LEGEND_GAP + size;
            " ".repeat(LEGEND_GAP)
        } else {
            rows.push(Vec::new());
            used = size;
            " ".to_owned()
        };
        let y = rows.len() - 1;
        if let Some(row) = rows.last_mut() {
            row.push(StyledSpan::plain(lead));
            legend_mark(view, (mark, token, &glyph), (&mut *row, y), &mut marks);
            row.push(word);
        }
    }
    (rows.into_iter().map(StyledLine::new).collect(), marks)
}

#[cfg(test)]
mod tests;
