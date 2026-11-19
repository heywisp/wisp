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
        frame: 0,
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

fn busy(head: f32, step: u8) -> Scene {
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
        path: vec![
            (head - 3.0, 4.5),
            (head - 2.0, 4.5),
            (head - 1.0, 4.5),
            (head, 4.5),
        ],
    }];
    let rings = vec![Ring { on: dots[1], step }];
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
    for scene in [busy(5.5, 0), busy(12.5, 2)] {
        for cell in [CELL, (4, 40)] {
            let grid = (f32::from(cell.0), f32::from(cell.1));
            for y in scene.rows.clone() {
                for x in 0..scene.width {
                    for cells in 1..=2.min(scene.width - x) {
                        let tile = Rect { x, y, cells };
                        assert_eq!(
                            raster(&scene, &THEME, cell, tile, None),
                            draw(&scene, &THEME, cell, tile, None),
                            "{cell:?}, {tile:?}, {:?}",
                            scene.lights[0],
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
                    && far.rings.is_empty()
                    && far.text_cells.is_empty()
            );
        }
    }
}

#[test]
fn tiles_hash_only_the_slots_something_reaches() {
    for scene in [busy(5.5, 0), busy(12.5, 2)] {
        for cell in [CELL, (4, 40)] {
            let grid = (f32::from(cell.0), f32::from(cell.1));
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
                        if let Some(h) = hash(&scene, grid, rect) {
                            reference.push((rect, h));
                        }
                    }
                }
            }
            assert_eq!(
                tiles(&scene, cell),
                reference,
                "{cell:?}, {:?}",
                scene.lights[0]
            );
        }
    }
    let scene = one(vec![dot(150, 40, Mark::Root, Token::Accent)], 300, 80);
    let grid = (f32::from(CELL.0), f32::from(CELL.1));
    assert!(slots(&scene, grid).len() <= 9);
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
    dots.push(Dot {
        radius: 0.4,
        ..dot(20, 1, Mark::Root, Token::Accent)
    });
    let scene = one(dots, 24, 3);
    let centre = (4, 9);
    for (i, &(state, token)) in states.iter().enumerate() {
        let drawn = cell_of(&scene, 4 * i, 1);
        let at = |x, y| pixel(&drawn, 1, (x, y));
        let want = opaque(THEME.rgb(token));
        assert!(
            drawn
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == want || *p == [0; 4]),
            "{state:?}"
        );
        let (solid, clear) = match state {
            NodeState::Waiting | NodeState::Unknown => ((1, 9), centre),
            NodeState::Working => ((4, 6), (2, 7)),
            NodeState::Done => ((3, 11), centre),
            NodeState::Stopped => ((2, 7), (1, 6)),
            NodeState::Failed => ((1, 6), (4, 6)),
        };
        assert_eq!(at(solid.0, solid.1), want, "{state:?}");
        assert_eq!(at(clear.0, clear.1), [0; 4], "{state:?}");
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
    assert_eq!(pixel(&done, 1, (3, 11)), opaque(THEME.rgb(Token::Ok)));
    let main = cell_of(&scene, 20, 1);
    let main = main.as_chunks::<4>().0;
    let orb = [Token::OrbRing, Token::Accent].map(|token| opaque(THEME.rgb(token)));
    assert!(main.iter().all(|p| *p == [0; 4] || orb.contains(p)));
    assert!(orb.iter().all(|ink| main.contains(ink)));
    assert_eq!(main.iter().filter(|p| p[3] != 0).count(), 11);
    let plus = cell_of(&scene, 0, 1);
    let mut star = scene;
    star.dots[0].frame = 2;
    let star = cell_of(&star, 0, 1);
    assert_ne!(plus, star);
    let working = opaque(THEME.rgb(Token::Working));
    for frame in [&plus, &star] {
        assert!((1..=7).all(|x| pixel(frame, 1, (x, 9)) == working));
        assert!((6..=12).all(|y| pixel(frame, 1, (4, y)) == working));
        assert!(
            frame
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == working || *p == [0; 4])
        );
    }
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
    assert_eq!(
        super::tiles(&far_text, CELL),
        tiles,
        "text outside the tiles"
    );
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
        text_cells: vec![(2, 1)],
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
fn a_picked_mark_gets_accent_brackets_and_widens_its_tiles() {
    let waiting = dot(1, 1, Mark::State(NodeState::Waiting), Token::TextMuted);
    let picked = Dot {
        picked: true,
        ..waiting
    };
    let scene = one(vec![picked], 3, 3);
    let drawn = cell_of(&scene, 1, 1);
    let accent = opaque(THEME.rgb(Token::Accent));
    assert_eq!(
        pixel(&drawn, 1, (4, 9)),
        [0; 4],
        "no tint in the hollow mark"
    );
    assert_eq!(pixel(&drawn, 1, (0, 5)), accent, "left bracket's lip");
    assert_eq!(pixel(&drawn, 1, (8, 13)), accent, "right bracket's lip");
    assert_eq!(
        pixel(&drawn, 1, (0, 9)),
        [0; 4],
        "the gap inside the bracket"
    );
    assert_eq!(pixel(&cell_of(&scene, 2, 1), 1, (0, 9)), accent);
    assert_eq!(tiles(&one(vec![waiting], 3, 3), CELL).len(), 1);
    assert_eq!(tiles(&scene, CELL).len(), 2);

    let wide = Dot {
        at: (3.0, 1.5),
        radius: 7.5 / 9.0,
        ..picked
    };
    let scene = one(vec![wide], 6, 3);
    let edge = cell_of(&scene, 4, 1);
    assert_eq!(pixel(&edge, 1, (0, 9)), accent);
    assert_eq!(pixel(&edge, 1, (1, 9)), accent, "two-pixel bar");
    assert_eq!(pixel(&edge, 1, (2, 9)), [0; 4]);
    let inside = cell_of(&scene, 3, 1);
    assert_eq!(pixel(&inside, 1, (7, 9)), [0; 4]);
    assert_eq!(pixel(&inside, 1, (8, 9)), [0; 4], "two-pixel gap");
    assert_eq!(
        tiles(&scene, CELL)
            .iter()
            .filter(|(rect, _)| rect.y == 1)
            .count(),
        3
    );
}

#[test]
fn a_flying_light_lights_three_cells_behind_its_head_and_changes_only_their_tiles() {
    let kid = dot(1, 4, Mark::State(NodeState::Waiting), Token::TextMuted);
    let path = vec![kid.at, (1.5, 1.5), (9.5, 1.5), (9.5, 0.5)];
    let flying = |head| Scene {
        lights: vec![Light {
            path: (0..=3).rev().map(|i| (head - i as f32, 1.5)).collect(),
        }],
        lines: vec![path.clone()],
        ..one(vec![kid], 12, 6)
    };
    let (before, after) = (tiles(&flying(5.5), CELL), tiles(&flying(6.5), CELL));
    assert_eq!(before.len(), after.len());
    let changed: Vec<Rect> = (before.iter().zip(&after))
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.0)
        .collect();
    assert!(!changed.is_empty());
    assert!(changed.iter().all(|r| r.y == 1 && r.x < 8), "{changed:?}");

    let bend = Scene {
        lights: vec![Light {
            path: vec![(4.0, 3.0), (4.0, 2.0), (4.0, 1.0), (5.0, 1.0)],
        }],
        ..Scene::default()
    };
    let at = |x: usize, y: usize| {
        let (w, h) = (usize::from(CELL.0), usize::from(CELL.1));
        pixel(&cell_of(&bend, x / w, y / h), 1, (x % w, y % h))
    };
    let [head, near, far] = lit_colours(&THEME).map(opaque);
    assert_eq!(at(39, 19), head, "the horizontal leg");
    assert_eq!(
        at(35, 19),
        head,
        "the snapped corner belongs to the outgoing leg"
    );
    assert_eq!(at(35, 27), near, "the first vertical leg");
    assert_eq!(at(35, 43), far, "the last vertical leg");
    assert!(
        (17..21).all(|y| (43..47).all(|x| at(x, y) == head)),
        "the head block"
    );

    let left_bend = [(1.5, 0.5), (1.5, 1.5), (0.5, 1.5)];
    assert!(
        dots_along(&left_bend, (12.0, 24.0), 2.0)
            .filter(|&(at, _)| at == (20.0, 36.0))
            .eq([((20.0, 36.0), 1)])
    );

    let on = dot(3, 2, Mark::State(NodeState::Waiting), Token::TextMuted);
    let landed = one(vec![on], 6, 5);
    let draw_rows = |scene: &Scene| {
        scene
            .rows
            .clone()
            .flat_map(|y| {
                raster(
                    scene,
                    &THEME,
                    CELL,
                    Rect {
                        x: 0,
                        y,
                        cells: scene.width,
                    },
                    None,
                )
            })
            .collect::<Vec<_>>()
    };
    let bare = draw_rows(&landed);
    for (step, dots) in [16, 24, 32].into_iter().enumerate() {
        let scene = Scene {
            rings: vec![Ring {
                on,
                step: step as u8,
            }],
            ..landed.clone()
        };
        let pixels = draw_rows(&scene);
        let pixels = pixels.as_chunks::<4>().0;
        let ink = [head, near, far][step];
        assert_eq!(pixels.iter().filter(|&&p| p == ink).count(), 4 * dots);
        assert!(
            bare.as_chunks::<4>()
                .0
                .iter()
                .zip(pixels)
                .all(|(before, after)| before[3] == 0 || before == after),
            "step {step} preserves the receiver"
        );
    }
}
