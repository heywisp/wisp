use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use ratatui_core::layout::Position;
use wisp_media::encode_base64;
use wisp_render::theme::{Rgb, mix};

pub const FIRST_ID: u32 = 1 << 30;
const CHUNK: usize = 4096;
const REGISTERS: usize = 256;
const STEPS: usize = 16;
const BAND: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub at: Position,
    pub cells: u16,
    pub hash: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder<'a> {
    Kitty,
    Sixel(&'a [Rgb]),
}

pub struct Tiles<'a> {
    pub list: Vec<Tile>,
    pub cell: (u16, u16),
    pub encoder: Encoder<'a>,
    pub pixels: &'a dyn Fn(usize) -> Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Live {
    id: u32,
    cells: u16,
    hash: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Slots {
    live: BTreeMap<(u16, u16), Live>,
    next: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub send: Vec<(usize, u32)>,
    pub rewrite: Vec<Position>,
    pub delete: Vec<u32>,
    pub slots: Slots,
}

fn covers(at: Position, cells: u16) -> impl Iterator<Item = Position> {
    (at.x..at.x.saturating_add(cells)).map(move |x| Position { x, y: at.y })
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.send.is_empty() && self.rewrite.is_empty() && self.delete.is_empty()
    }
}

impl Slots {
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub fn change(&self, tiles: &[Tile], written: impl Fn(Position) -> bool) -> Change {
        let mut next = self.next.max(FIRST_ID);
        let mut live = BTreeMap::new();
        let mut send = Vec::new();
        for (i, tile) in tiles.iter().enumerate() {
            let key = (tile.at.y, tile.at.x);
            let kept = self.live.get(&key).filter(|old| {
                old.hash == tile.hash
                    && old.cells == tile.cells
                    && !covers(tile.at, tile.cells).any(&written)
            });
            let id = match kept {
                Some(old) => old.id,
                None => {
                    let id = next;
                    next = if next == u32::MAX { FIRST_ID } else { next + 1 };
                    send.push((i, id));
                    id
                }
            };
            let (cells, hash) = (tile.cells, tile.hash);
            live.insert(key, Live { id, cells, hash });
        }
        let kept: BTreeSet<u32> = live.values().map(|l| l.id).collect();
        let covered = |p: &Position| {
            live.iter()
                .any(|(&(y, x), l)| y == p.y && (x..x.saturating_add(l.cells)).contains(&p.x))
        };
        let mut rewrite = Vec::new();
        let mut delete = Vec::new();
        for (&(y, x), old) in &self.live {
            if !kept.contains(&old.id) {
                delete.push(old.id);
            }
            rewrite.extend(covers(Position { x, y }, old.cells).filter(|p| !covered(p)));
        }
        Change {
            send,
            rewrite,
            delete,
            slots: Slots { live, next },
        }
    }
}

pub fn transmit(out: &mut Vec<u8>, id: u32, tile: &Tile, cell: (u16, u16), rgba: &[u8]) {
    let mut packer = ZlibEncoder::new(Vec::new(), Compression::default());
    let Ok(packed) = packer.write_all(rgba).and_then(|()| packer.finish()) else {
        return;
    };
    let data = encode_base64(&packed);
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(CHUNK).collect();
    let (width, height) = (u32::from(cell.0) * u32::from(tile.cells), cell.1);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        let keys = if i == 0 {
            format!(
                "a=T,f=32,o=z,s={width},v={height},i={id},c={},r=1,C=1,z=0,q=1,m={more}",
                tile.cells
            )
        } else {
            format!("m={more}")
        };
        out.extend_from_slice(format!("\x1b_G{keys};").as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

pub fn delete(out: &mut Vec<u8>, id: u32) {
    out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={id},q=1\x1b\\").as_bytes());
}

pub fn palette(background: Rgb, ramps: &[(Rgb, Rgb)]) -> Vec<Rgb> {
    let mut kept: Vec<(Rgb, Rgb)> = Vec::new();
    for &ramp in ramps.iter().filter(|(from, to)| from != to) {
        if !kept.contains(&ramp) {
            kept.push(ramp);
        }
    }
    kept.truncate(REGISTERS - 1);
    let steps = ((REGISTERS - 1) / kept.len().max(1)).min(STEPS);
    let mut out = vec![background];
    for (from, to) in kept {
        for step in (0..steps).map(|s| 1.0 - s as f64 / steps as f64) {
            let rgb = mix(from, to, step);
            if !out.contains(&rgb) {
                out.push(rgb);
            }
        }
    }
    out
}

fn nearest(palette: &[Rgb], pixel: &[u8]) -> usize {
    let far = |c: &Rgb| {
        [(c.r, pixel[0]), (c.g, pixel[1]), (c.b, pixel[2])]
            .map(|(a, b)| u32::from(a.abs_diff(b)).pow(2))
            .iter()
            .sum::<u32>()
    };
    (0..palette.len())
        .min_by_key(|&i| far(&palette[i]))
        .unwrap_or(0)
}

fn percent(v: u8) -> u16 {
    (u16::from(v) * 100 + 127) / 255
}

fn runs(out: &mut Vec<u8>, bits: &[u8]) {
    let end = bits.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    for run in bits.get(..end).unwrap_or_default().chunk_by(|a, b| a == b) {
        let char = 63 + run[0];
        match run.len() {
            n @ ..=3 => out.extend(std::iter::repeat_n(char, n)),
            n => out.extend_from_slice(format!("!{n}{}", char as char).as_bytes()),
        }
    }
}

pub fn sixel(out: &mut Vec<u8>, tile: &Tile, cell: (u16, u16), rgba: &[u8], palette: &[Rgb]) {
    let width = usize::from(cell.0) * usize::from(tile.cells);
    let height = usize::from(cell.1);
    let palette = palette.get(..REGISTERS).unwrap_or(palette);
    if rgba.len() != width * height * 4 || palette.is_empty() {
        return;
    }
    let mut seen = BTreeMap::new();
    let index: Vec<usize> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| *seen.entry(&p[..3]).or_insert_with(|| nearest(palette, p)))
        .collect();
    out.extend_from_slice(format!("\x1bP0;1q\"1;1;{width};{height}").as_bytes());
    for n in index.iter().copied().collect::<BTreeSet<usize>>() {
        let Rgb { r, g, b } = palette[n];
        let (r, g, b) = (percent(r), percent(g), percent(b));
        out.extend_from_slice(format!("#{n};2;{r};{g};{b}").as_bytes());
    }
    for top in (0..height).step_by(BAND) {
        let rows = top..(top + BAND).min(height);
        if top > 0 {
            out.push(b'-');
        }
        let at = |x: usize, y: usize| index[y * width + x];
        let used: BTreeSet<usize> = rows
            .clone()
            .flat_map(|y| (0..width).map(move |x| at(x, y)))
            .collect();
        for (k, n) in used.into_iter().enumerate() {
            if k > 0 {
                out.push(b'$');
            }
            out.extend_from_slice(format!("#{n}").as_bytes());
            let bits: Vec<u8> = (0..width)
                .map(|x| {
                    rows.clone()
                        .enumerate()
                        .fold(0, |bits, (i, y)| bits | (u8::from(at(x, y) == n) << i))
                })
                .collect();
            runs(out, &bits);
        }
    }
    out.extend_from_slice(b"\x1b\\");
}

pub fn refused(reply: &str) -> bool {
    reply
        .split_once(';')
        .is_none_or(|(_, said)| !said.starts_with("OK"))
}

#[cfg(test)]
mod tests;
