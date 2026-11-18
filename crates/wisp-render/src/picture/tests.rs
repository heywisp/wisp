use wisp_protocol::slot::NodeState;

use super::*;
use crate::theme::{Depth, Mode};

const CELL: (u16, u16) = (9, 19);
const THEME: Theme = Theme::new(Mode::Dark, Depth::TrueColor);

fn dot(x: usize, y: usize, mark: Mark, token: Token) -> Dot {
    Dot {
        at: (x as f32 + 0.5, y as f32 + 0.5),
        mark,
        radius: 0.5,
        ink: Ink::from(token),
        halo: true,
        picked: false,
    }
}

fn one(dots: Vec<Dot>, width: usize, height: usize) -> Scene {
    Scene {
        width,
        rows: 0..height,
        dots,
        ..Scene::default()
    }
}

fn busy(along: f32) -> Scene {
    let mut dots = vec![
        dot(4, 2, Mark::Root, Token::Accent),
        dot(12, 2, Mark::State(NodeState::Working), Token::Working),
        dot(17, 4, Mark::State(NodeState::Failed), Token::Error),
        dot(7, 6, Mark::State(NodeState::Waiting), Token::TextMuted),
        dot(23, 5, Mark::Square, Token::Ok),
        Dot {
            radius: 0.479,
            ..dot(17, 0, Mark::State(NodeState::Waiting), Token::TextMuted)
        },
    ];
    dots[3].picked = true;
    let lines = vec![
        vec![dots[0].at, (4.5, 4.5), dots[2].at],
        vec![dots[1].at, (12.5, 5.5), dots[4].at],
    ];
    let lights = vec![Light {
        path: lines[0].clone(),
        along,
        radius: 0.5,
    }];
    let rings = vec![Ring {
        on: dots[1],
        open: 0.7,
    }];
    let text_cells = [(13, 2), (14, 2)]
        .into_iter()
        .chain((0..20).map(|x| (x, 0)))
        .collect();
    Scene {
        lines,
        lights,
        rings,
        text_cells,
        ..one(dots, 30, 10)
    }
}

#[test]
fn a_narrowed_tile_draws_what_the_whole_scene_draws() {
    for scene in [busy(0.3), busy(0.7)] {
        for cell in [CELL, (4, 40)] {
            let grid = (f32::from(cell.0), f32::from(cell.1));
            for y in scene.rows.clone() {
                for x in 0..scene.width {
                    for cells in 1..=2.min(scene.width - x) {
                        let tile = Rect { x, y, cells };
                        assert_eq!(
                            raster(&scene, &THEME, cell, tile, None),
                            draw(&scene, &scene, &THEME, cell, tile, None),
                            "{cell:?}, {tile:?}, light at {}",
                            scene.lights[0].along,
                        );
                    }
                }
            }
            let far = narrow(
                &scene,
                grid,
                Rect {
                    x: 28,
                    y: 9,
                    cells: 2,
                },
            );
            assert!(
                far.dots.is_empty()
                    && far.lines.is_empty()
                    && far.lights.is_empty()
                    && far.text_cells.is_empty()
            );
        }
    }
}

#[test]
fn tiles_hash_only_the_slots_something_reaches() {
    for scene in [busy(0.3), busy(0.7)] {
        for cell in [CELL, (4, 40)] {
            let grid = (f32::from(cell.0), f32::from(cell.1));
            let shared = Shared::of(&scene, grid);
            let mut reference = Vec::new();
            for y in scene.rows.clone() {
                for x in (0..scene.width).step_by(2) {
                    let free: Vec<_> = (x..(x + 2).min(scene.width))
                        .filter(|c| !scene.text_cells.contains(&(*c, y)))
                        .collect();
                    if let Some(&x) = free.first() {
                        let rect = Rect {
                            x,
                            y,
                            cells: free.len(),
                        };
                        if let Some(h) = hash(&scene, grid, rect, &shared) {
                            reference.push((rect, h));
                        }
                    }
                }
            }
            assert_eq!(
                tiles(&scene, cell),
                reference,
                "{cell:?}, light at {}",
                scene.lights[0].along
            );
        }
    }
    let counted = busy(0.3);
    let uncounted = Scene {
        text_cells: counted
            .text_cells
            .iter()
            .copied()
            .filter(|&(_, y)| y != 2)
            .collect(),
        ..counted.clone()
    };
    let grid = (f32::from(CELL.0), f32::from(CELL.1));
    let rect = Rect {
        x: 10,
        y: 2,
        cells: 2,
    };
    assert_eq!(
        near_text(&counted, grid, rect).collect::<Vec<_>>(),
        near_text(&uncounted, grid, rect).collect::<Vec<_>>()
    );
    assert_ne!(
        hash(&counted, grid, rect, &Shared::of(&counted, grid)),
        hash(&uncounted, grid, rect, &Shared::of(&uncounted, grid)),
        "a distant count changes the landing radius, not the tile's text"
    );
    let scene = one(vec![dot(150, 40, Mark::Root, Token::Accent)], 300, 80);
    let grid = (f32::from(CELL.0), f32::from(CELL.1));
    assert!(slots(&scene, grid, &Shared::of(&scene, grid)).len() <= 9);
}

fn pixel(rgba: &[u8], cells: usize, (x, y): (usize, usize)) -> [u8; 4] {
    let i = (y * usize::from(CELL.0) * cells + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

fn opaque(rgb: Rgb) -> [u8; 4] {
    [rgb.r, rgb.g, rgb.b, 255]
}

fn cell_of(scene: &Scene, x: usize, y: usize) -> Vec<u8> {
    raster(scene, &THEME, CELL, Rect { x, y, cells: 1 }, None)
}

#[test]
fn every_state_has_its_shape_and_colour_and_empty_pixels_stay_clear() {
    let states = [
        (NodeState::Working, Token::Working),
        (NodeState::Done, Token::Ok),
        (NodeState::Stopped, Token::TextFaint),
        (NodeState::Failed, Token::Error),
        (NodeState::Waiting, Token::TextMuted),
    ];
    let mut dots: Vec<Dot> = states
        .iter()
        .enumerate()
        .map(|(i, &(s, t))| dot(4 * i, 1, Mark::State(s), t))
        .collect();
    dots.push(dot(20, 1, Mark::Root, Token::Accent));
    let scene = one(dots, 24, 3);
    let centre = (4, 9);
    for (i, &(state, token)) in states.iter().enumerate() {
        let drawn = cell_of(&scene, 4 * i, 1);
        let at = |x, y| pixel(&drawn, 1, (x, y));
        let want = opaque(THEME.rgb(token));
        match state {
            NodeState::Waiting => {
                assert_eq!(at(4, 9)[3], 0, "a waiting dot is hollow");
                assert_eq!(at(0, 9), want, "its ring");
            }
            NodeState::Done => {
                assert_eq!(at(4, 13), want, "the disc under the tick");
                assert!(at(4, 11)[3] < 128, "the tick is cut out: {:?}", at(4, 11));
            }
            NodeState::Failed => {
                assert_eq!(at(4, 9), want, "the cross");
                assert_eq!(at(4, 6)[3], 0, "no disc between the arms");
            }
            _ => assert_eq!(at(centre.0, centre.1), want, "{state:?}"),
        }
        if state != NodeState::Working {
            assert_eq!(at(0, 0)[3], 0, "{state:?}'s empty corner");
        }
    }
    let bg = THEME.rgb(Token::Surface);
    let done = raster(
        &scene,
        &THEME,
        CELL,
        Rect {
            x: 4,
            y: 1,
            cells: 1,
        },
        Some(bg),
    );
    assert!(
        done.chunks(4).all(|p| p[3] == 255),
        "opaque on a background"
    );
    assert_eq!(pixel(&done, 1, (0, 0)), opaque(bg), "the empty corner");
    assert_eq!(pixel(&done, 1, (4, 13)), opaque(THEME.rgb(Token::Ok)));
    let main = cell_of(&scene, 20, 1);
    assert_eq!(pixel(&main, 1, (4, 13)), opaque(THEME.rgb(Token::Accent)));
    let core = pixel(&main, 1, centre);
    let light = opaque(crate::theme::mix(
        THEME.rgb(Token::Gleam),
        THEME.rgb(Token::Accent),
        0.9,
    ));
    let near = core.iter().zip(light).all(|(a, b)| a.abs_diff(b) <= 1);
    assert!(near, "{core:?} against {light:?}");
    let inks = [Token::Ok, Token::Error];
    let stacked = (0..SQUARES_PER_ROW).map(|k| Dot {
        at: (0.5, (k as f32 + 0.5) / SQUARES_PER_ROW as f32),
        radius: 0.4,
        ..dot(0, 0, Mark::Square, inks[k])
    });
    let stack = one(stacked.collect(), 1, 1);
    let drawn = cell_of(&stack, 0, 0);
    assert_eq!(pixel(&drawn, 1, (4, 4)), opaque(THEME.rgb(Token::Ok)));
    assert_eq!(pixel(&drawn, 1, (4, 14)), opaque(THEME.rgb(Token::Error)));
    assert_eq!(
        pixel(&drawn, 1, (4, 9))[3],
        0,
        "the gap between two squares"
    );
    let square = raster(
        &stack,
        &THEME,
        (16, 16),
        Rect {
            x: 0,
            y: 0,
            cells: 1,
        },
        None,
    );
    let edge = (6 * 16 + 8) * 4;
    assert_eq!(square[edge..edge + 4], opaque(THEME.rgb(Token::Ok)));
}

#[test]
fn main_s_halo_fades_out_at_the_top_edge_of_the_picture() {
    let main = |y| one(vec![dot(0, y, Mark::Root, Token::Accent)], 1, 3);
    let top = pixel(&cell_of(&main(0), 0, 0), 1, (4, 0));
    let inside = pixel(&cell_of(&main(1), 0, 1), 1, (4, 0));
    assert!(top[3] <= 2, "faded at the edge: {top:?}");
    assert!(inside[3] > 10 * top[3].max(1), "{inside:?} against {top:?}");
}

#[test]
fn tiles_skip_empty_and_text_cells_and_keep_their_hashes() {
    let folded = Dot {
        radius: 0.4,
        ..dot(0, 0, Mark::State(NodeState::Done), Token::Ok)
    };
    let scene = Scene {
        text_cells: vec![(1, 0), (2, 0), (3, 0)],
        ..one(vec![folded], 6, 2)
    };
    let tiles = tiles(&scene, CELL);
    let rects: Vec<Rect> = tiles.iter().map(|(r, _)| *r).collect();
    assert_eq!(
        rects,
        [Rect {
            x: 0,
            y: 0,
            cells: 1
        }],
        "beside +30, one cell"
    );
    assert_eq!(super::tiles(&scene.clone(), CELL), tiles, "the same scene");
    let far_text = Scene {
        text_cells: [scene.text_cells.clone(), vec![(5, 1)]].concat(),
        ..scene.clone()
    };
    assert_eq!(super::tiles(&far_text, CELL), tiles, "text beyond the fade");
    let failed = Scene {
        dots: vec![Dot {
            mark: Mark::State(NodeState::Failed),
            ..folded
        }],
        ..scene.clone()
    };
    assert_ne!(super::tiles(&failed, CELL)[0].1, tiles[0].1);
    let pair = one(
        vec![dot(3, 1, Mark::State(NodeState::Done), Token::Ok)],
        6,
        2,
    );
    let rects: Vec<Rect> = super::tiles(&pair, CELL).iter().map(|(r, _)| *r).collect();
    assert_eq!(
        rects,
        [Rect {
            x: 2,
            y: 1,
            cells: 2
        }]
    );
    let pair = Scene {
        dots: vec![folded, pair.dots[0]],
        ..pair
    };
    let before = super::tiles(&pair, CELL);
    let near_text = Scene {
        text_cells: vec![(4, 1)],
        ..pair
    };
    let after = super::tiles(&near_text, CELL);
    let changed: Vec<_> = before
        .iter()
        .zip(&after)
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.0)
        .collect();
    assert_eq!(
        changed,
        [Rect {
            x: 2,
            y: 1,
            cells: 2
        }],
        "only the nearby dot's tile"
    );
}

#[test]
fn a_picked_dot_sits_on_a_tinted_disc_under_an_accent_ring_and_widens_its_tiles() {
    let waiting = dot(1, 1, Mark::State(NodeState::Waiting), Token::TextMuted);
    let picked = Dot {
        picked: true,
        ..waiting
    };
    let scene = one(vec![picked], 3, 3);
    let drawn = cell_of(&scene, 1, 1);
    let at = |y| pixel(&drawn, 1, (4, y));
    let tint = opaque(THEME.rgb(Token::AccentTint));
    let accent = opaque(THEME.rgb(Token::Accent));
    assert_eq!(at(9), tint, "the hollow dot shows the disc");
    assert_eq!(at(15), tint, "6 px out, between the dot and the ring");
    assert_eq!(at(17), accent, "the ring 3.5 px beyond the dot");
    let left = cell_of(&scene, 0, 1);
    assert_eq!(
        pixel(&left, 1, (8, 9)),
        tint,
        "the disc reaches the next cell"
    );
    assert_eq!(
        pixel(&left, 1, (0, 9))[3],
        0,
        "and ends 6 px beyond the dot"
    );
    assert_eq!(tiles(&one(vec![waiting], 3, 3), CELL).len(), 1);
    assert_eq!(tiles(&scene, CELL).len(), 6);
}

#[test]
fn a_flying_light_changes_only_the_tiles_around_it_and_a_ring_keeps_to_its_room() {
    let main = Dot {
        radius: 0.5 + 1.4 / 9.0,
        ..dot(9, 0, Mark::Root, Token::Accent)
    };
    let kid = dot(1, 4, Mark::State(NodeState::Waiting), Token::TextMuted);
    let path = vec![kid.at, (1.5, 1.5), (9.5, 1.5), main.at];
    let scene = one(vec![main, kid], 12, 6);
    let flying = |along| Scene {
        lights: vec![Light {
            path: path.clone(),
            along,
            radius: 0.5,
        }],
        lines: vec![path.clone()],
        ..scene.clone()
    };
    let (before, after) = (tiles(&flying(0.65), CELL), tiles(&flying(0.7), CELL));
    assert_eq!(before.len(), after.len());
    let changed: Vec<Rect> = (before.iter().zip(&after))
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.0)
        .collect();
    assert!(!changed.is_empty());
    assert!(changed.iter().all(|r| r.y == 1 && r.x < 8), "{changed:?}");

    let grid = (f32::from(CELL.0), f32::from(CELL.1));
    let ring = |on: Dot, scene: &Scene| landing(scene, &Ring { on, open: 1.0 }, grid);
    let start = |on: Dot| edge(&on, grid) + LAND_GAP;
    let top = ring(main, &scene) + LAND_WIDTH / 2.0;
    assert!(top <= 9.5, "main's ring stays in its row: {top}");
    assert!(ring(main, &scene) > start(main), "and grows a little");
    assert_eq!(ring(kid, &scene), start(kid) + LAND_GROWTH, "room to grow");
    let count = Scene {
        text_cells: vec![(2, 4)],
        ..scene.clone()
    };
    assert_eq!(ring(kid, &count), start(kid), "cut where it has no room");
    let picked = Dot {
        picked: true,
        ..dot(3, 4, Mark::State(NodeState::Working), Token::Working)
    };
    let pair = Scene {
        dots: vec![kid, picked],
        ..scene.clone()
    };
    let gap = 2.0 * f32::from(CELL.0) - edge(&picked, grid);
    assert_eq!(
        ring(kid, &pair),
        gap - LAND_WIDTH / 2.0,
        "up to a picked neighbour's ring"
    );
}
