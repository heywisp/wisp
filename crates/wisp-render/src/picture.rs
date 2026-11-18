use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hasher};
use std::ops::Range;
use std::sync::Arc;

use tiny_skia::{
    BlendMode, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, PremultipliedColorU8,
    Stroke, Transform,
};
use wisp_protocol::slot::NodeState;

use crate::theme::{Ink, Rgb, Theme, Token, mix};

const BASE_CELL: f32 = 9.0;
const LINE: f32 = 1.5;
const CORNER: f32 = 5.0;
const RING: f32 = 1.6;
const REACH: f32 = 2.2;
const FADE: f32 = 0.2;
const CORE: f32 = 0.45;
const GLEAM: f32 = 0.9;
const PICK_DISC: f32 = 6.0;
const PICK_RING: f32 = 3.5;
const PICK_WIDTH: f32 = 2.0;
const SQUARE_FIT: f32 = 0.8;
pub(crate) const SQUARES_PER_ROW: usize = 2;
const TAIL: f32 = 4.5;
const TAIL_STEPS: u16 = 14;
const LAND_GAP: f32 = 2.0;
const LAND_GROWTH: f32 = 5.0;
const LAND_WIDTH: f32 = 2.0;
const LAND_ALPHA: f32 = 0.85;
const CROSS_REACH: f32 = 1.3;

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
    pub halo: bool,
    pub picked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub path: Vec<(f32, f32)>,
    pub along: f32,
    pub radius: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    pub on: Dot,
    pub open: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub width: usize,
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

fn halo(dot: &Dot) -> Option<(f32, f32)> {
    match dot.mark {
        _ if !dot.halo => None,
        Mark::Root => Some((1.5, 0.3)),
        Mark::State(NodeState::Working) => Some((1.85, 0.43)),
        Mark::State(_) | Mark::Square => None,
    }
}

fn around((x, y): (f32, f32), reach: f32) -> Bounds {
    (x - reach, y - reach, x + reach, y + reach)
}

fn body(dot: &Dot, grid: Grid) -> Bounds {
    around(at(grid, dot.at), outer(dot, grid).max(edge(dot, grid)))
}

fn outer(dot: &Dot, grid: Grid) -> f32 {
    let disc = if dot.picked { PICK_DISC } else { 0.0 };
    size(dot, grid) + disc * grid.0 / BASE_CELL
}

fn size(dot: &Dot, grid: Grid) -> f32 {
    let r = dot.radius * grid.0;
    match dot.mark {
        Mark::Square => r.min(SQUARE_FIT * grid.1 / (2 * SQUARES_PER_ROW) as f32),
        Mark::Root | Mark::State(_) => r,
    }
}

fn edge(dot: &Dot, grid: Grid) -> f32 {
    let pick = if dot.picked {
        PICK_RING + PICK_WIDTH / 2.0
    } else {
        0.0
    };
    size(dot, grid) * reach(dot) + pick * grid.0 / BASE_CELL
}

fn reach(dot: &Dot) -> f32 {
    match dot.mark {
        Mark::State(NodeState::Failed) => CROSS_REACH,
        Mark::Root | Mark::State(_) | Mark::Square => 1.0,
    }
}

type Glow = ((f32, f32), f32, Token, f32);

type Disc = ((f32, f32), f32);

fn span(a: (f32, f32), b: (f32, f32)) -> f32 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

fn walk(points: &[(f32, f32)], by: f32) -> (f32, f32) {
    let mut left = by;
    for pair in points.windows(2) {
        let length = span(pair[0], pair[1]);
        if left <= length {
            return toward(pair[0], pair[1], left);
        }
        left -= length;
    }
    points.last().copied().unwrap_or_default()
}

fn sparks(light: &Light, grid: Grid) -> Vec<Glow> {
    let points: Vec<(f32, f32)> = light.path.iter().map(|&p| at(grid, p)).collect();
    let head = light.along * points.windows(2).map(|w| span(w[0], w[1])).sum::<f32>();
    let r = light.radius * grid.0;
    let mut out: Vec<Glow> = (0..=TAIL_STEPS)
        .filter_map(|i| {
            let k = f32::from(i) / f32::from(TAIL_STEPS);
            let back = head - TAIL * grid.0 * (1.0 - k);
            let size = r * (0.2 + 0.25 * k);
            (back >= 0.0).then(|| (walk(&points, back), size, Token::Working, 0.48 * k * k))
        })
        .collect();
    let tip = walk(&points, head);
    out.extend([
        (tip, 0.8 * r, Token::Working, 0.6),
        (tip, 0.6 * r, Token::Gleam, 0.95),
        (tip, 0.35 * r, Token::Gleam, 1.0),
    ]);
    out
}

fn room(scene: &Scene, dot: &Dot, grid: Grid) -> f32 {
    let centre = at(grid, dot.at);
    let others = scene.dots.iter().filter(|d| d.at != dot.at);
    others.fold(clearance(scene, grid, centre), |near, other| {
        near.min(span(centre, at(grid, other.at)) - edge(other, grid))
    })
}

fn landing(scene: &Scene, ring: &Ring, grid: Grid) -> f32 {
    let unit = grid.0 / BASE_CELL;
    let start = edge(&ring.on, grid) + LAND_GAP * unit;
    let room = room(scene, &ring.on, grid) - LAND_WIDTH * unit / 2.0;
    start.max(room.min(start + LAND_GROWTH * unit * ring.open))
}

fn glow_bounds(dot: &Dot, grid: Grid) -> Option<Bounds> {
    halo(dot).map(|(k, _)| around(at(grid, dot.at), REACH * k * dot.radius * grid.0))
}

fn paint_box(dot: &Dot, grid: Grid) -> Bounds {
    let (l, t, r, b) = body(dot, grid);
    let (gl, gt, gr, gb) = glow_bounds(dot, grid).unwrap_or((l, t, r, b));
    (
        l.min(gl) - 1.0,
        t.min(gt) - 1.0,
        r.max(gr) + 1.0,
        b.max(gb) + 1.0,
    )
}

fn spark_box(glow: &Glow) -> Bounds {
    let visible = (510.0 * glow.3).ln().max(0.0).sqrt().min(REACH);
    around(glow.0, visible * glow.1)
}

struct Shared {
    sparks: Vec<Vec<Glow>>,
    radii: Vec<f32>,
}

impl Shared {
    fn of(scene: &Scene, grid: Grid) -> Self {
        Self {
            sparks: scene.lights.iter().map(|l| sparks(l, grid)).collect(),
            radii: scene
                .rings
                .iter()
                .map(|r| landing(scene, r, grid))
                .collect(),
        }
    }
}

fn tile_box(grid: Grid, rect: Rect) -> Bounds {
    let (l, t) = corner(grid, rect.x, rect.y);
    let (r, b) = corner(grid, rect.x + rect.cells, rect.y + 1);
    (l, t, r, b)
}

fn intersects((l, t, r, b): Bounds, (x0, y0, x1, y1): Bounds) -> bool {
    l < x1 && r > x0 && t < y1 && b > y0
}

fn segments(line: &[(f32, f32)], grid: Grid) -> impl Iterator<Item = Bounds> {
    let reach = LINE * (grid.0 / BASE_CELL) / 2.0 + 1.0;
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

fn hash(scene: &Scene, grid: Grid, rect: Rect, shared: &Shared) -> Option<u64> {
    let bounds @ (x0, y0, x1, y1) = tile_box(grid, rect);
    let hit = |b| intersects(b, bounds);
    let mut h = DefaultHasher::new();
    let mut hit_any = false;
    for dot in &scene.dots {
        let seen = if hit(body(dot, grid)) {
            *dot
        } else if glow_bounds(dot, grid).is_some_and(hit) {
            Dot {
                ink: Ink::from(dot.ink.still()),
                ..*dot
            }
        } else {
            continue;
        };
        h.write(format!("{seen:?}").as_bytes());
        hit_any = true;
    }
    for line in scene.lines.iter().filter(|l| segments(l, grid).any(hit)) {
        h.write(format!("{line:?}").as_bytes());
        hit_any = true;
    }
    for (light, sparks) in scene.lights.iter().zip(&shared.sparks) {
        if sparks.iter().any(|s| hit(spark_box(s))) {
            h.write(format!("{light:?}").as_bytes());
            hit_any = true;
        }
    }
    for (ring, &radius) in scene.rings.iter().zip(&shared.radii) {
        let (cx, cy) = at(grid, ring.on.at);
        let near = (x0 - cx)
            .max(cx - x1)
            .max(0.0)
            .hypot((y0 - cy).max(cy - y1).max(0.0));
        let far = (cx - x0).max(x1 - cx).hypot((cy - y0).max(y1 - cy));
        let half = LAND_WIDTH * grid.0 / BASE_CELL;
        if near < radius + half && far >= radius - half {
            h.write(format!("{ring:?}{radius:?}").as_bytes());
            hit_any = true;
        }
    }
    let frame = (rect, grid, scene.width, &scene.rows);
    h.write(format!("{frame:?}").as_bytes());
    for cell in near_text(scene, grid, rect) {
        h.write(format!("{cell:?}").as_bytes());
    }
    hit_any.then(|| h.finish())
}

fn slots(scene: &Scene, grid: Grid, shared: &Shared) -> BTreeSet<(usize, usize)> {
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
    for segment in scene.lines.iter().flat_map(|l| segments(l, grid)) {
        mark(segment);
    }
    for spark in shared.sparks.iter().flatten() {
        mark(spark_box(spark));
    }
    for (ring, radius) in scene.rings.iter().zip(&shared.radii) {
        mark(around(
            at(grid, ring.on.at),
            radius + LAND_WIDTH * grid.0 / BASE_CELL,
        ));
    }
    out
}

fn gap(grid: Grid, (x, y): (usize, usize), (x0, y0, x1, y1): Bounds) -> f32 {
    let (l, t) = corner(grid, x, y);
    let dx = (l - x1).max(x0 - l - grid.0).max(0.0);
    let dy = (t - y1).max(y0 - t - grid.1).max(0.0);
    dx.hypot(dy)
}

fn near_text(scene: &Scene, grid: Grid, rect: Rect) -> impl Iterator<Item = (usize, usize)> + '_ {
    let bounds = tile_box(grid, rect);
    scene
        .text_cells
        .iter()
        .copied()
        .filter(move |&cell| gap(grid, cell, bounds) <= FADE * grid.1)
}

fn narrow(scene: &Scene, grid: Grid, rect: Rect) -> Scene {
    let hit = |b| intersects(b, tile_box(grid, rect));
    Scene {
        width: scene.width,
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
            .filter(|l| segments(l, grid).any(hit))
            .cloned()
            .collect(),
        lights: scene
            .lights
            .iter()
            .filter(|l| sparks(l, grid).iter().any(|s| hit(spark_box(s))))
            .cloned()
            .collect(),
        rings: scene.rings.clone(),
        text_cells: near_text(scene, grid, rect).collect(),
    }
}

pub fn tiles(scene: &Scene, cell: (u16, u16)) -> Vec<(Rect, u64)> {
    let grid = (f32::from(cell.0), f32::from(cell.1));
    let shared = Shared::of(scene, grid);
    let mut out = Vec::new();
    for (y, x) in slots(scene, grid, &shared) {
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
        if let Some(h) = hash(scene, grid, rect, &shared) {
            out.push((rect, h));
        }
    }
    out
}

pub fn ramps(scene: &Scene, theme: &Theme, behind: Rgb) -> Vec<(Rgb, Rgb)> {
    let tint = theme.rgb(Token::AccentTint);
    let picked = scene.dots.iter().any(|d| d.picked);
    let pick = [(tint, behind), (theme.rgb(Token::Accent), tint)];
    let lines = (!scene.lines.is_empty()).then(|| (theme.rgb(Token::Line), behind));
    let dots = scene.dots.iter().flat_map(|dot| {
        let ink = theme.ink_rgb(dot.ink);
        let gleam = theme.rgb(Token::Gleam);
        let core = (dot.mark == Mark::Root).then(|| (mix(gleam, ink, f64::from(GLEAM)), ink));
        let still = halo(dot).map(|_| theme.rgb(dot.ink.still()));
        let base = [Some((ink, behind)), core, still.map(|s| (s, behind))];
        let tinted = [Some((ink, tint)), still.map(|s| (s, tint))];
        let tinted = tinted.into_iter().filter(|_| picked);
        base.into_iter().chain(tinted).flatten().collect::<Vec<_>>()
    });
    let pick = pick.into_iter().filter(|_| picked);
    let base: Vec<(Rgb, Rgb)> = lines.into_iter().chain(pick).chain(dots).collect();
    let (gleam, cool) = (theme.rgb(Token::Gleam), theme.rgb(Token::Working));
    let line = theme.rgb(Token::Line);
    let inks = scene.dots.iter().map(|dot| theme.ink_rgb(dot.ink));
    let over = inks.flat_map(|ink| [(cool, ink), (gleam, ink)]);
    let lit = [
        (gleam, behind),
        (cool, behind),
        (gleam, cool),
        (cool, line),
        (gleam, mix(cool, line, 0.5)),
    ]
    .into_iter()
    .chain(over);
    let lit = lit.filter(|_| scene.moving()).collect::<Vec<_>>();
    base.into_iter().chain(lit).collect()
}

pub fn raster(
    scene: &Scene,
    theme: &Theme,
    cell: (u16, u16),
    tile: Rect,
    behind: Option<Rgb>,
) -> Vec<u8> {
    let grid = (f32::from(cell.0), f32::from(cell.1));
    draw(&narrow(scene, grid, tile), scene, theme, cell, tile, behind)
}

fn draw(
    scene: &Scene,
    whole: &Scene,
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
    let wire = paint(theme.rgb(Token::Line), 1.0);
    let line = Stroke {
        width: LINE * (grid.0 / BASE_CELL),
        ..Stroke::default()
    };
    for points in &scene.lines {
        let at = points.iter().map(|p| at(grid, *p));
        if let Some(path) = rounded(at, CORNER * (grid.0 / BASE_CELL)) {
            pixmap.stroke_path(&path, &wire, &line, shift, None);
        }
    }
    let picked = scene.dots.iter().filter(|d| d.picked);
    let tint = paint(theme.rgb(Token::AccentTint), 1.0);
    let accent = paint(theme.rgb(Token::Accent), 1.0);
    let ring = Stroke {
        width: PICK_WIDTH * (grid.0 / BASE_CELL),
        ..Stroke::default()
    };
    let circle = |dot: &Dot, r: f32| {
        let (cx, cy) = at(grid, dot.at);
        PathBuilder::from_circle(cx, cy, r)
    };
    let bare: Vec<_> = (scene.dots.iter().filter(|d| !d.picked))
        .map(|d| (at(grid, d.at), size(d, grid) * reach(d)))
        .collect();
    let lights = scene.lights.iter().flat_map(|light| sparks(light, grid));
    glow(&mut pixmap, scene, theme, (grid, origin, &bare), lights);
    for disc in picked.clone().filter_map(|d| circle(d, outer(d, grid))) {
        pixmap.fill_path(&disc, &tint, FillRule::Winding, shift, None);
    }
    let halos = scene.dots.iter().filter_map(|dot| {
        let (k, strength) = halo(dot)?;
        let radius = k * dot.radius * grid.0;
        Some((at(grid, dot.at), radius, dot.ink.still(), strength))
    });
    glow(&mut pixmap, scene, theme, (grid, origin, &[]), halos);
    for dot in &scene.dots {
        shape(&mut pixmap, dot, theme, grid, shift);
    }
    let rim = |d: &Dot| circle(d, d.radius * grid.0 + PICK_RING * (grid.0 / BASE_CELL));
    for path in picked.filter_map(rim) {
        pixmap.stroke_path(&path, &accent, &ring, shift, None);
    }
    let land = Stroke {
        width: LAND_WIDTH * (grid.0 / BASE_CELL),
        ..Stroke::default()
    };
    for ring in &scene.rings {
        let fade = paint(theme.rgb(Token::Gleam), LAND_ALPHA * (1.0 - ring.open));
        if let Some(path) = circle(&ring.on, landing(whole, ring, grid)) {
            pixmap.stroke_path(&path, &fade, &land, shift, None);
        }
    }
    let all = tiny_skia::Rect::from_xywh(0.0, 0.0, width as f32, f32::from(cell.1));
    if let (Some(rgb), Some(all)) = (behind, all) {
        let under = Paint {
            blend_mode: BlendMode::DestinationOver,
            ..paint(rgb, 1.0)
        };
        pixmap.fill_rect(all, &under, Transform::identity(), None);
    }
    pixmap.take_demultiplied()
}

fn mask(blend_mode: BlendMode) -> Paint<'static> {
    Paint {
        blend_mode,
        ..Paint::default()
    }
}

fn paint(rgb: Rgb, alpha: f32) -> Paint<'static> {
    let mut paint = Paint::default();
    let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    paint.set_color_rgba8(rgb.r, rgb.g, rgb.b, alpha);
    paint
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

fn glow(
    pixmap: &mut Pixmap,
    scene: &Scene,
    theme: &Theme,
    (grid, origin, bare): (Grid, (f32, f32), &[Disc]),
    glows: impl Iterator<Item = Glow>,
) {
    let halos: Vec<((f32, f32), f32, Rgb, f32)> = glows
        .map(|(centre, radius, token, strength)| (centre, radius, theme.rgb(token), strength))
        .collect();
    if halos.is_empty() {
        return;
    }
    let width = pixmap.width().max(1) as usize;
    for (i, pixel) in pixmap.pixels_mut().iter_mut().enumerate() {
        let x = origin.0 + (i % width) as f32 + 0.5;
        let y = origin.1 + (i / width) as f32 + 0.5;
        let mut fade = None;
        for &((cx, cy), radius, rgb, strength) in &halos {
            let d2 = ((x - cx).powi(2) + (y - cy).powi(2)) / radius.powi(2);
            if d2 < REACH * REACH {
                let fade = *fade.get_or_insert_with(|| {
                    let open = |&(centre, r): &Disc| span((x, y), centre) - r + 0.5;
                    let open = bare.iter().map(open).fold(1.0, f32::min).clamp(0.0, 1.0);
                    edge_fade(scene, grid, (x, y)) * open
                });
                *pixel = over(*pixel, rgb, strength * (-d2).exp() * fade);
            }
        }
    }
}

fn clearance(scene: &Scene, grid: Grid, (x, y): (f32, f32)) -> f32 {
    let (w, top) = corner(grid, scene.width, scene.rows.start);
    let bottom = corner(grid, 0, scene.rows.end).1;
    scene
        .text_cells
        .iter()
        .fold(x.min(w - x).min(y - top).min(bottom - y), |near, &cell| {
            near.min(gap(grid, cell, (x, y, x, y)))
        })
}

fn edge_fade(scene: &Scene, grid: Grid, at: (f32, f32)) -> f32 {
    smooth((clearance(scene, grid, at) / (FADE * grid.1)).clamp(0.0, 1.0))
}

pub(crate) fn smooth(u: f32) -> f32 {
    u * u * (3.0 - 2.0 * u)
}

fn over(pixel: PremultipliedColorU8, rgb: Rgb, alpha: f32) -> PremultipliedColorU8 {
    let a = alpha.clamp(0.0, 1.0);
    let mix = |src: u8, dst: u8| (f32::from(src) * a + f32::from(dst) * (1.0 - a)).round() as u8;
    let out = mix(255, pixel.alpha());
    let channel = |src: u8, dst: u8| mix(src, dst).min(out);
    PremultipliedColorU8::from_rgba(
        channel(rgb.r, pixel.red()),
        channel(rgb.g, pixel.green()),
        channel(rgb.b, pixel.blue()),
        out,
    )
    .unwrap_or(pixel)
}

fn shape(pixmap: &mut Pixmap, dot: &Dot, theme: &Theme, grid: Grid, shift: Transform) {
    let (cx, cy) = at(grid, dot.at);
    let r = size(dot, grid);
    let unit = grid.0 / BASE_CELL;
    let ink = paint(theme.ink_rgb(dot.ink), 1.0);
    let mut fill = |path: Option<Path>, paint: &Paint<'_>| {
        if let Some(path) = path {
            pixmap.fill_path(&path, paint, FillRule::Winding, shift, None);
        }
    };
    if !dot.picked {
        fill(PathBuilder::from_circle(cx, cy, r), &mask(BlendMode::Clear));
    }
    let at = |dx: f32, dy: f32| (cx + dx * r, cy + dy * r);
    let (outline, width, cut) = match dot.mark {
        Mark::Root => {
            fill(PathBuilder::from_circle(cx, cy, r), &ink);
            let core = paint(theme.rgb(Token::Gleam), GLEAM);
            fill(PathBuilder::from_circle(cx, cy, r * CORE), &core);
            return;
        }
        Mark::State(NodeState::Working) => {
            fill(PathBuilder::from_circle(cx, cy, r), &ink);
            return;
        }
        Mark::State(NodeState::Stopped) => {
            fill(square((cx, cy), 0.85 * r), &ink);
            return;
        }
        Mark::Square => {
            fill(square((cx, cy), r), &ink);
            return;
        }
        Mark::State(NodeState::Done) => {
            fill(PathBuilder::from_circle(cx, cy, r), &ink);
            let tick = [at(-0.5, 0.02), at(-0.12, 0.4), at(0.52, -0.38)];
            (
                rounded(tick.into_iter(), 0.0),
                (0.32 * r).max(1.4 * unit),
                true,
            )
        }
        Mark::State(NodeState::Failed) => {
            let q = 0.72;
            let mut cross = PathBuilder::new();
            for (from, to) in [(at(-q, -q), at(q, q)), (at(-q, q), at(q, -q))] {
                cross.move_to(from.0, from.1);
                cross.line_to(to.0, to.1);
            }
            (cross.finish(), (0.5 * r).max(1.9 * unit), false)
        }
        Mark::State(NodeState::Waiting | NodeState::Unknown) => {
            let width = RING * unit;
            (
                PathBuilder::from_circle(cx, cy, r - width / 2.0),
                width,
                false,
            )
        }
    };
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let paint = if cut {
        mask(BlendMode::DestinationOut)
    } else {
        ink
    };
    if let Some(path) = outline {
        pixmap.stroke_path(&path, &paint, &stroke, shift, None);
    }
}

#[cfg(test)]
mod tests;
