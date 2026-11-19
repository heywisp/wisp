use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use tiny_skia::{BlendMode, FillRule, Paint, Path, PathBuilder, Pixmap, Transform};
use wisp_protocol::slot::NodeState;

use crate::motion::{self, MotionCtx, MotionPolicy, Presence};
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Ink, Rgb, Theme, Token, mix};

const BASE_CELL: f32 = 9.0;
const DOT: f32 = 1.5;
const SQUARE_FIT: f32 = 0.8;
pub(crate) const SQUARES_PER_ROW: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Root,
    State(NodeState),
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dot {
    pub at: (f32, f32),
    pub mark: Mark,
    pub radius: f32,
    pub ink: Ink,
    pub frame: u8,
    pub picked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub path: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    pub on: Dot,
    pub step: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub width: usize,
    pub thin: bool,
    pub rows: Range<usize>,
    pub dots: Vec<Dot>,
    pub lines: Vec<Vec<(f32, f32)>>,
    pub lights: Vec<Light>,
    pub rings: Vec<Ring>,
    pub text_cells: Vec<(usize, usize)>,
}

impl Scene {
    pub fn moving(&self) -> bool {
        !self.lights.is_empty() || !self.rings.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Strip {
    pub scene: Arc<Scene>,
    pub row: usize,
    pub x: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub cells: usize,
}

type Grid = (f32, f32);

fn at(grid: Grid, (x, y): (f32, f32)) -> (f32, f32) {
    (x * grid.0, y * grid.1)
}

fn corner(grid: Grid, x: usize, y: usize) -> (f32, f32) {
    at(grid, (x as f32, y as f32))
}

type Bounds = (f32, f32, f32, f32);

fn around((x, y): (f32, f32), reach: f32) -> Bounds {
    (x - reach, y - reach, x + reach, y + reach)
}

fn extent(dot: &Dot, grid: Grid) -> ((i32, i32), f32) {
    let d = 2.0 * dot.radius * grid.0;
    match dot.mark {
        Mark::Root => ((6, 4), (d / 6.0).round().max(1.0)),
        Mark::Square => ((1, 1), d.min(SQUARE_FIT * grid.1 / SQUARES_PER_ROW as f32)),
        Mark::State(_) if d < 3.0 => ((2, 2), 1.0),
        Mark::State(_) => {
            let n = if d >= 7.0 {
                7
            } else if d >= 5.0 {
                5
            } else {
                3
            };
            ((n, n), (d / n as f32).floor().max(1.0))
        }
    }
}

fn sprite_box(dot: &Dot, grid: Grid, ((w, h), px): ((i32, i32), f32)) -> Bounds {
    let (cx, cy) = at(grid, dot.at);
    let (w, h) = (w as f32 * px, h as f32 * px);
    let (mut x, mut y) = (cx - w / 2.0, cy - h / 2.0);
    if dot.mark != Mark::Square {
        (x, y) = (x.round(), y.round());
    }
    (x, y, x + w, y + h)
}

fn inset((l, t, r, b): Bounds, by: f32) -> Bounds {
    (l + by, t + by, r - by, b - by)
}

fn body(dot: &Dot, grid: Grid) -> Bounds {
    let size @ (_, px) = extent(dot, grid);
    let frame = sprite_box(dot, grid, size);
    inset(frame, if dot.picked { -2.0 * px } else { 0.0 })
}

fn paint_box(dot: &Dot, grid: Grid) -> Bounds {
    let bounds = body(dot, grid);
    if dot.mark == Mark::Square {
        bounds
    } else {
        inset(bounds, -extent(dot, grid).1)
    }
}

fn tips(dot: &Dot, grid: Grid) -> u8 {
    if dot.mark != Mark::State(NodeState::Working) {
        return 0;
    }
    let ((n, _), _) = extent(dot, grid);
    [0, 1, 2, 1][usize::from(dot.frame % 4)].min(n as u8 / 2 - 1)
}

fn dot_side(scene: &Scene, grid: Grid) -> f32 {
    if scene.thin {
        1.0
    } else {
        (DOT * grid.0 / BASE_CELL).round().max(1.0)
    }
}

fn reach(side: f32) -> f32 {
    4.0 * side
}

fn snap(v: f32, pitch: f32) -> f32 {
    (v / pitch).round() * pitch
}

fn dots_along(
    points: &[(f32, f32)],
    grid: Grid,
    side: f32,
) -> impl Iterator<Item = ((f32, f32), usize)> + '_ {
    let pitch = 2.0 * side;
    points.windows(2).enumerate().flat_map(move |(leg, pair)| {
        let (a, b) = (at(grid, pair[0]), at(grid, pair[1]));
        let horizontal = a.1 == b.1;
        let (start, end, fixed) = if horizontal {
            (a.0, b.0, a.1)
        } else {
            (a.1, b.1, a.0)
        };
        let turns_here = leg
            .checked_sub(1)
            .and_then(|i| points.get(i))
            .is_some_and(|p| (p.1 == pair[0].1) != horizontal);
        let snapped = snap(fixed, pitch);
        let start = if turns_here && snapped == fixed {
            snap(start, pitch)
        } else {
            start
        };
        let first = (start.min(end) / pitch).ceil() as i32;
        let last = (start.max(end) / pitch).floor() as i32;
        (first..=last).filter_map(move |i| {
            let v = i as f32 * pitch;
            (v != end || leg + 2 == points.len()).then_some((
                if horizontal {
                    (v, snapped)
                } else {
                    (snapped, v)
                },
                leg,
            ))
        })
    })
}

fn ring_square(ring: &Ring, grid: Grid, side: f32) -> Bounds {
    let half = ring.on.radius * grid.0 + (1.0 + 2.0 * f32::from(ring.step)) * side;
    let (l, t, r, b) = around(at(grid, ring.on.at), half);
    let on_pitch = |v| snap(v, 2.0 * side);
    (on_pitch(l), on_pitch(t), on_pitch(r), on_pitch(b))
}

fn ring_box(ring: &Ring, grid: Grid, side: f32) -> Bounds {
    inset(ring_square(ring, grid, side), -reach(side))
}

fn tile_box(grid: Grid, rect: Rect) -> Bounds {
    let (l, t) = corner(grid, rect.x, rect.y);
    let (r, b) = corner(grid, rect.x + rect.cells, rect.y + 1);
    (l, t, r, b)
}

fn intersects((l, t, r, b): Bounds, (x0, y0, x1, y1): Bounds) -> bool {
    l < x1 && r > x0 && t < y1 && b > y0
}

fn segments(line: &[(f32, f32)], grid: Grid, side: f32) -> impl Iterator<Item = Bounds> {
    let reach = reach(side);
    line.windows(2).map(move |pair| {
        let ((ax, ay), (bx, by)) = (at(grid, pair[0]), at(grid, pair[1]));
        (
            ax.min(bx) - reach,
            ay.min(by) - reach,
            ax.max(bx) + reach,
            ay.max(by) + reach,
        )
    })
}

fn hash(scene: &Scene, grid: Grid, rect: Rect) -> Option<u64> {
    let side = dot_side(scene, grid);
    let bounds = tile_box(grid, rect);
    let hit = |b| intersects(b, bounds);
    let mut h = DefaultHasher::new();
    let mut hit_any = false;
    for dot in scene.dots.iter().filter(|d| hit(paint_box(d, grid))) {
        let visible = hit(body(dot, grid));
        let seen = Dot {
            frame: if visible { tips(dot, grid) } else { 0 },
            ..*dot
        };
        h.write(format!("{seen:?}").as_bytes());
        hit_any |= visible;
    }
    for line in scene
        .lines
        .iter()
        .filter(|l| segments(l, grid, side).any(hit))
    {
        h.write(format!("{line:?}").as_bytes());
        hit_any = true;
    }
    for light in scene
        .lights
        .iter()
        .filter(|l| segments(&l.path, grid, side).any(hit))
    {
        h.write(format!("{light:?}").as_bytes());
        hit_any = true;
    }
    for ring in scene.rings.iter().filter(|r| hit(ring_box(r, grid, side))) {
        let landing = (ring_square(ring, grid, side), ring.step);
        h.write(format!("{landing:?}").as_bytes());
        hit_any = true;
    }
    let frame = (rect, grid, scene.width, scene.thin, &scene.rows);
    h.write(format!("{frame:?}").as_bytes());
    hit_any.then(|| h.finish())
}

fn slots(scene: &Scene, grid: Grid) -> BTreeSet<(usize, usize)> {
    let side = dot_side(scene, grid);
    let mut out = BTreeSet::new();
    let mut mark = |(l, t, r, b): Bounds| {
        let left = (l / grid.0).floor().max(0.0) as usize / 2 * 2;
        let right = ((r / grid.0).ceil().max(0.0) as usize).min(scene.width);
        let top = ((t / grid.1).floor().max(0.0) as usize).max(scene.rows.start);
        let bottom = ((b / grid.1).ceil().max(0.0) as usize).min(scene.rows.end);
        for y in top..bottom {
            for x in (left..right).step_by(2) {
                out.insert((y, x));
            }
        }
    };
    for dot in &scene.dots {
        mark(paint_box(dot, grid));
    }
    for segment in scene.lines.iter().flat_map(|l| segments(l, grid, side)) {
        mark(segment);
    }
    for segment in scene
        .lights
        .iter()
        .flat_map(|l| segments(&l.path, grid, side))
    {
        mark(segment);
    }
    for ring in &scene.rings {
        mark(ring_box(ring, grid, side));
    }
    out
}

fn narrow(scene: &Scene, grid: Grid, rect: Rect) -> Scene {
    let side = dot_side(scene, grid);
    let hit = |b| intersects(b, tile_box(grid, rect));
    Scene {
        width: scene.width,
        thin: scene.thin,
        rows: scene.rows.clone(),
        dots: scene
            .dots
            .iter()
            .filter(|d| hit(paint_box(d, grid)))
            .copied()
            .collect(),
        lines: scene
            .lines
            .iter()
            .filter(|l| segments(l, grid, side).any(hit))
            .cloned()
            .collect(),
        lights: scene
            .lights
            .iter()
            .filter(|l| segments(&l.path, grid, side).any(hit))
            .cloned()
            .collect(),
        rings: scene
            .rings
            .iter()
            .filter(|r| hit(ring_box(r, grid, side)))
            .copied()
            .collect(),
        text_cells: Vec::new(),
    }
}

pub fn tiles(scene: &Scene, cell: (u16, u16)) -> Vec<(Rect, u64)> {
    let grid = (f32::from(cell.0), f32::from(cell.1));
    let mut out = Vec::new();
    for (y, x) in slots(scene, grid) {
        let free: Vec<usize> = (x..(x + 2).min(scene.width))
            .filter(|c| !scene.text_cells.contains(&(*c, y)))
            .collect();
        let Some(&first) = free.first() else {
            continue;
        };
        let rect = Rect {
            x: first,
            y,
            cells: free.len(),
        };
        if let Some(h) = hash(scene, grid, rect) {
            out.push((rect, h));
        }
    }
    out
}

fn lit_colours(theme: &Theme) -> [Rgb; 3] {
    let working = theme.rgb(Token::Working);
    [
        theme.rgb(Token::Gleam),
        working,
        mix(working, theme.rgb(Token::Line), 0.5),
    ]
}

pub fn ramps(scene: &Scene, theme: &Theme, behind: Rgb) -> Vec<(Rgb, Rgb)> {
    let lines = (!scene.lines.is_empty()).then(|| theme.rgb(Token::Line));
    let picked = scene
        .dots
        .iter()
        .any(|d| d.picked)
        .then(|| theme.rgb(Token::Accent));
    let orb = scene
        .dots
        .iter()
        .any(|d| d.mark == Mark::Root)
        .then(|| [theme.rgb(Token::OrbRing), theme.rgb(Token::Accent)]);
    let dots = scene.dots.iter().map(|d| theme.ink_rgb(d.ink));
    let lit = scene.moving().then(|| lit_colours(theme));
    lines
        .into_iter()
        .chain(picked)
        .chain(orb.into_iter().flatten())
        .chain(dots)
        .chain(lit.into_iter().flatten())
        .map(|rgb| (rgb, behind))
        .collect()
}

pub fn raster(
    scene: &Scene,
    theme: &Theme,
    cell: (u16, u16),
    tile: Rect,
    behind: Option<Rgb>,
) -> Vec<u8> {
    let grid = (f32::from(cell.0), f32::from(cell.1));
    draw(&narrow(scene, grid, tile), theme, cell, tile, behind)
}

fn draw(
    scene: &Scene,
    theme: &Theme,
    cell: (u16, u16),
    tile: Rect,
    behind: Option<Rgb>,
) -> Vec<u8> {
    let grid = (f32::from(cell.0), f32::from(cell.1));
    let width = u32::from(cell.0) * u32::try_from(tile.cells).unwrap_or(0);
    let Some(mut pixmap) = Pixmap::new(width, u32::from(cell.1)) else {
        return Vec::new();
    };
    let origin = corner(grid, tile.x, tile.y);
    let shift = Transform::from_translate(-origin.0, -origin.1);
    let side = dot_side(scene, grid);
    let wire = crisp(theme.rgb(Token::Line));
    for points in &scene.lines {
        for (at, _) in dots_along(points, grid, side) {
            blot(&mut pixmap, at, side, &wire, shift);
        }
    }
    for trail in &scene.lights {
        light(&mut pixmap, trail, theme, grid, side, shift);
    }
    for dot in &scene.dots {
        stamp(&mut pixmap, dot, theme, grid, shift);
    }
    for landing in &scene.rings {
        ring(&mut pixmap, landing, theme, grid, side, shift);
    }
    let all = tiny_skia::Rect::from_xywh(0.0, 0.0, width as f32, f32::from(cell.1));
    if let (Some(rgb), Some(all)) = (behind, all) {
        let under = Paint {
            blend_mode: BlendMode::DestinationOver,
            ..paint(rgb)
        };
        pixmap.fill_rect(all, &under, Transform::identity(), None);
    }
    pixmap.take_demultiplied()
}

fn mask(blend_mode: BlendMode) -> Paint<'static> {
    Paint {
        blend_mode,
        anti_alias: false,
        ..Paint::default()
    }
}

fn paint(rgb: Rgb) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(rgb.r, rgb.g, rgb.b, 255);
    paint
}

fn crisp(rgb: Rgb) -> Paint<'static> {
    Paint {
        anti_alias: false,
        ..paint(rgb)
    }
}

fn fill(pixmap: &mut Pixmap, (l, t, r, b): Bounds, paint: &Paint<'_>, shift: Transform) {
    if let Some(rect) = tiny_skia::Rect::from_ltrb(l, t, r, b) {
        pixmap.fill_rect(rect, paint, shift, None);
    }
}

fn blot(pixmap: &mut Pixmap, (x, y): (f32, f32), side: f32, paint: &Paint<'_>, shift: Transform) {
    let (l, t) = ((x - side / 2.0).round(), (y - side / 2.0).round());
    fill(pixmap, (l, t, l + side, t + side), paint, shift);
}

fn light(
    pixmap: &mut Pixmap,
    light: &Light,
    theme: &Theme,
    grid: Grid,
    side: f32,
    shift: Transform,
) {
    let Some(&head) = light.path.last() else {
        return;
    };
    let inks = lit_colours(theme).map(crisp);
    for (at, leg) in dots_along(&light.path, grid, side) {
        let back = light.path.len() - 2 - leg;
        if let Some(ink) = inks.get(back) {
            blot(pixmap, at, side, ink, shift);
        }
    }
    blot(pixmap, at(grid, head), 2.0 * side, &inks[0], shift);
}

fn ring(pixmap: &mut Pixmap, ring: &Ring, theme: &Theme, grid: Grid, side: f32, shift: Transform) {
    let (l, t, r, b) = ring_square(ring, grid, side);
    let corners = [(l, t), (r, t), (r, b), (l, b), (l, t)];
    let ink = crisp(lit_colours(theme)[usize::from(ring.step.min(2))]);
    for (at, _) in dots_along(&corners, (1.0, 1.0), side) {
        blot(pixmap, at, side, &ink, shift);
    }
}

fn span(a: (f32, f32), b: (f32, f32)) -> f32 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

fn toward(from: (f32, f32), to: (f32, f32), by: f32) -> (f32, f32) {
    let length = span(from, to);
    if length <= f32::EPSILON {
        return from;
    }
    let k = by / length;
    (from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k)
}

fn rounded(points: impl Iterator<Item = (f32, f32)>, radius: f32) -> Option<Path> {
    let points: Vec<(f32, f32)> = points.collect();
    let &first = points.first()?;
    let mut path = PathBuilder::new();
    path.move_to(first.0, first.1);
    for window in points.windows(3) {
        let (a, b, c) = (window[0], window[1], window[2]);
        let half = |p: (f32, f32), q: (f32, f32)| span(p, q) / 2.0;
        let r = radius.min(half(a, b)).min(half(b, c));
        let (enter, leave) = (toward(b, a, r), toward(b, c, r));
        path.line_to(enter.0, enter.1);
        path.quad_to(b.0, b.1, leave.0, leave.1);
    }
    let last = points.last().copied().unwrap_or(first);
    path.line_to(last.0, last.1);
    path.finish()
}

fn square((cx, cy): (f32, f32), half: f32) -> Option<Path> {
    let corners = [
        (0.0, -1.0),
        (1.0, -1.0),
        (1.0, 1.0),
        (-1.0, 1.0),
        (-1.0, -1.0),
        (0.0, -1.0),
    ];
    let at = |(x, y): (f32, f32)| (cx + x * half, cy + y * half);
    rounded(corners.into_iter().map(at), 0.35 * half)
}

fn sprite(mark: NodeState, tips: u8, n: i32) -> impl Iterator<Item = (i32, i32)> {
    let h = n / 2;
    let tick: &[(i32, i32)] = match n {
        3 => &[(-1, 0), (0, 1), (1, -1)],
        5 => &[(-2, 0), (-1, 1), (0, 2), (1, 1), (2, 0), (2, -1)],
        _ => &[(-3, 0), (-2, 1), (-1, 2), (0, 1), (1, 0), (2, -1), (3, -2)],
    };
    (-h..=h).flat_map(move |j| {
        (-h..=h).filter_map(move |i| {
            let (x, y) = (i.abs(), j.abs());
            let on = match mark {
                NodeState::Working => i == 0 || j == 0 || (x == y && x <= i32::from(tips)),
                NodeState::Waiting | NodeState::Unknown => {
                    (h * h - h + 1..=h * h + h / 2).contains(&(i * i + j * j))
                }
                NodeState::Done => tick.contains(&(i, j)),
                NodeState::Stopped => x.max(y) <= (h - 1).max(1),
                NodeState::Failed => x == y,
            };
            on.then_some((i + h, j + h))
        })
    })
}

fn bracket(pixmap: &mut Pixmap, (l, t, r, b): Bounds, px: f32, theme: &Theme, shift: Transform) {
    let accent = crisp(theme.rgb(Token::Accent));
    for (x, outer, inner) in [(l - 2.0 * px, l - 2.0 * px, l), (r + px, r, r + 2.0 * px)] {
        fill(pixmap, (x, t - px, x + px, b + px), &accent, shift);
        fill(pixmap, (outer, t - px, inner, t), &accent, shift);
        fill(pixmap, (outer, b, inner, b + px), &accent, shift);
    }
}

fn stamp(pixmap: &mut Pixmap, dot: &Dot, theme: &Theme, grid: Grid, shift: Transform) {
    let size @ ((n, _), px) = extent(dot, grid);
    let bounds @ (x, y, _, _) = sprite_box(dot, grid, size);
    let ink = theme.ink_rgb(dot.ink);
    if dot.mark == Mark::Square {
        if let Some(path) = square(at(grid, dot.at), px / 2.0) {
            pixmap.fill_path(&path, &paint(ink), FillRule::Winding, shift, None);
        }
        return;
    }
    if n == 2 {
        fill(pixmap, bounds, &crisp(ink), shift);
        return;
    }
    fill(pixmap, inset(bounds, -px), &mask(BlendMode::Clear), shift);
    let mut pixel = |i: f32, j: f32, ink: &Paint<'_>| {
        let (l, t) = (x + i * px, y + j * px);
        fill(pixmap, (l, t, l + px, t + px), ink, shift);
    };
    match dot.mark {
        Mark::Root => {
            let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, theme.depth());
            for (i, j, ink, _) in motion::pose(Presence::Ready, Duration::ZERO, cx).dots() {
                pixel(f32::from(i), f32::from(j), &crisp(theme.ink_rgb(ink)));
            }
        }
        Mark::State(state) => {
            let ink = crisp(ink);
            for (i, j) in sprite(state, tips(dot, grid), n) {
                pixel(i as f32, j as f32, &ink);
            }
        }
        Mark::Square => {}
    }
    if dot.picked {
        bracket(pixmap, bounds, px, theme, shift);
    }
}

#[cfg(test)]
mod tests;
