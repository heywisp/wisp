use std::collections::BTreeSet;
use std::io::Read;
use std::time::Duration;

use flate2::read::ZlibDecoder;
use wisp_media::decode_base64;
use wisp_protocol::context::{FillPart, PartTokens};
use wisp_protocol::slot::{NodeState, SlotFill, SlotFlow, SlotTopologyNode};
use wisp_render::chrome::sidebar::fill_scenes;
use wisp_render::chrome::topology::{Topology, page, panel};
use wisp_render::markdown::Layout;
use wisp_render::motion::{MotionCtx, MotionPolicy};
use wisp_render::picture::{Rect, Scene, ramps, raster, tiles};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Rgb, Theme, Token, mix};

use super::*;

fn tile(x: u16, y: u16, cells: u16, hash: u64) -> Tile {
    Tile {
        at: Position { x, y },
        cells,
        hash,
    }
}

fn unpacked(payload: &[u8]) -> Vec<u8> {
    let packed = decode_base64(std::str::from_utf8(payload).unwrap()).unwrap();
    let mut out = Vec::new();
    ZlibDecoder::new(&packed[..]).read_to_end(&mut out).unwrap();
    out
}

fn view<'a>(nodes: &'a [SlotTopologyNode], flows: &'a [SlotFlow], ms: u64) -> Topology<'a> {
    Topology {
        root: Some("main"),
        nodes,
        flows,
        picked: None,
        label: None,
        layout: Layout::new(46, GlyphSet::Unicode),
        at: Duration::from_millis(ms),
        now_ms: ms,
        cx: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
        pictures: true,
    }
}

fn scene_of(nodes: &[SlotTopologyNode], flows: &[SlotFlow], ms: u64) -> Scene {
    panel(&view(nodes, flows, ms)).scenes.remove(0)
}

fn written(before: &Scene, after: &Scene) -> [(usize, usize); 2] {
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let listed = |rects: &[(Rect, u64)]| -> Vec<Tile> {
        let at = |v: usize| u16::try_from(v).unwrap();
        (rects.iter())
            .map(|&(r, hash)| tile(at(r.x), at(r.y), at(r.cells), hash))
            .collect()
    };
    let bg = theme.rgb(Token::Bg);
    let palette = palette(bg, &ramps(after, &theme, bg));
    [((9, 19), None), ((10, 20), Some(bg))].map(|(cell, behind)| {
        let shown = Slots::default()
            .change(&listed(&tiles(before, cell)), |_| false)
            .slots;
        let rects = tiles(after, cell);
        let list = listed(&rects);
        let step = shown.change(&list, |_| false);
        let mut out = Vec::new();
        for &(i, id) in &step.send {
            let rgba = raster(after, &theme, cell, rects[i].0, behind);
            match behind {
                None => transmit(&mut out, id, &list[i], cell, &rgba),
                Some(_) => sixel(&mut out, &list[i], cell, &rgba, &palette),
            }
        }
        step.delete.iter().for_each(|&id| delete(&mut out, id));
        (step.send.len(), out.len())
    })
}

#[test]
fn a_tile_goes_out_with_every_key_in_chunks_and_is_deleted_by_its_id() {
    let rgba: Vec<u8> = (0..9 * 19 * 4).map(|i| (i % 7) as u8).collect();
    let mut out = Vec::new();
    transmit(&mut out, FIRST_ID, &tile(3, 2, 1, 7), (9, 19), &rgba);
    let head = b"\x1b_Ga=T,f=32,o=z,s=9,v=19,i=1073741824,c=1,r=1,C=1,z=0,q=1,m=0;";
    assert!(out.starts_with(head), "{:?}", String::from_utf8_lossy(&out));
    assert!(out.ends_with(b"\x1b\\"));
    assert_eq!(unpacked(&out[head.len()..out.len() - 2]), rgba);

    let mut seed = 1u32;
    let noise: Vec<u8> = (0..64 * 64 * 4)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        })
        .collect();
    let mut out = Vec::new();
    transmit(&mut out, FIRST_ID + 1, &tile(0, 0, 1, 0), (64, 64), &noise);
    let text = String::from_utf8(out).unwrap();
    let chunks: Vec<&str> = text.split_terminator("\x1b\\").collect();
    assert!(chunks.len() > 2, "{}", chunks.len());
    let mut data = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let (keys, payload) = chunk
            .strip_prefix("\x1b_G")
            .unwrap()
            .split_once(';')
            .unwrap();
        let last = i + 1 == chunks.len();
        let more = if last { "m=0" } else { "m=1" };
        match i {
            0 => assert!(
                keys.starts_with("a=T,f=32,o=z,s=64,v=64,i=1073741825,") && keys.ends_with(more)
            ),
            _ => assert_eq!(keys, more),
        }
        assert!(payload.len() <= 4096 && (last || payload.len() == 4096));
        data.extend_from_slice(payload.as_bytes());
    }
    assert_eq!(unpacked(&data), noise);

    let mut out = Vec::new();
    delete(&mut out, FIRST_ID);
    assert_eq!(out, b"\x1b_Ga=d,d=I,i=1073741824,q=1\x1b\\");
}

#[test]
fn ids_count_up_from_the_base_and_wrap_back_to_it() {
    let tiles = [tile(0, 0, 2, 1), tile(2, 0, 2, 2)];
    let first = Slots::default().change(&tiles, |_| false);
    assert_eq!(first.send, [(0, FIRST_ID), (1, FIRST_ID + 1)]);
    let last = Slots {
        next: u32::MAX,
        ..Slots::default()
    };
    let wrapped = last.change(&tiles, |_| false);
    assert_eq!(wrapped.send, [(0, u32::MAX), (1, FIRST_ID)]);
}

#[test]
fn a_sixel_tile_runs_its_colours_across_six_row_bands() {
    let (bg, red) = (Rgb::hex(0x000000), Rgb::hex(0xff0000));
    let palette = palette(bg, &[(red, bg)]);
    let mut rgba = [0, 0, 0, 255].repeat(5 * 7);
    rgba[..8].copy_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255]);
    let mut out = Vec::new();
    sixel(&mut out, &tile(3, 2, 1, 7), (5, 7), &rgba, &palette);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "\x1bP0;1q\"1;1;5;7#0;2;0;0;0#1;2;100;0;0#0}}~~~$#1@@-#0!5@\x1b\\"
    );
}

#[test]
fn the_palette_ramps_each_colour_in_16_steps_and_holds_the_picture() {
    let bg = Rgb::hex(0x101010);
    let toward = |inks: &[Rgb]| inks.iter().map(|&ink| (ink, bg)).collect::<Vec<_>>();
    let primaries = toward(&[0xff0000, 0x00ff00, 0x0000ff].map(Rgb::hex));
    let few = palette(bg, &primaries);
    assert_eq!(few.len(), 1 + 3 * 16);
    assert_eq!(
        (few[0], few[1], few[17]),
        (bg, primaries[0].0, primaries[1].0)
    );
    assert_eq!(few[16], mix(primaries[0].0, bg, 1.0 / 16.0));
    assert_eq!(
        palette(bg, &[primaries[0], primaries[0]]),
        palette(bg, &primaries[..1])
    );
    let inks: Vec<Rgb> = (0..300u32)
        .map(|i| Rgb::hex(0x203040 + i * 0x010101))
        .collect();
    for n in [15, 16, 300] {
        let wide = palette(bg, &toward(&inks[..n]));
        assert!(wide.len() <= 256, "{n}: {}", wide.len());
        assert!(
            inks[..n.min(255)].iter().all(|ink| wide.contains(ink)),
            "{n}"
        );
    }

    let states = [
        NodeState::Working,
        NodeState::Waiting,
        NodeState::Done,
        NodeState::Stopped,
        NodeState::Failed,
    ];
    let nodes: Vec<SlotTopologyNode> = (0..40)
        .map(|i| SlotTopologyNode {
            key: format!("n{i}"),
            parent: (i >= 2).then(|| format!("n{}", if i < 10 { (i - 2) / 4 } else { 3 })),
            state: states[i % states.len()],
        })
        .collect();
    let panel = scene_of(&nodes, &[], 600);
    let kinds: Vec<SlotTopologyNode> = [("a", states[0])]
        .into_iter()
        .chain(
            states
                .iter()
                .enumerate()
                .map(|(i, &s)| (["a/b", "a/c", "a/d", "a/e", "a/f"][i], s)),
        )
        .map(|(key, state)| SlotTopologyNode {
            key: key.to_owned(),
            parent: key.rsplit_once('/').map(|(parent, _)| parent.to_owned()),
            state,
        })
        .collect();
    let (mut lit, mut boxes) = (Vec::new(), Vec::new());
    for end in [
        None,
        Some("a/b"),
        Some("a/c"),
        Some("a/d"),
        Some("a/e"),
        Some("a/f"),
    ] {
        for (from, to) in [(end, Some("a")), (Some("a"), end)] {
            let flows = [SlotFlow {
                from: from.map(str::to_owned),
                to: to.map(str::to_owned),
                at_ms: 1000,
            }];
            let at = |ms| Topology {
                picked: end,
                ..view(&kinds, &flows, ms)
            };
            let boxed = |ms| scene_of(&kinds, &flows, ms);
            let paged = |ms| page(&at(ms), 30).scenes.remove(0);
            for (scene, sidebar) in [(&boxed as &dyn Fn(u64) -> Scene, true), (&paged, false)] {
                let ringing = (1500..2600)
                    .step_by(20)
                    .map(scene)
                    .find(|s| !s.rings.is_empty());
                let frames = [scene(1040)].into_iter().chain(ringing);
                boxes.extend(frames.clone().filter(|_| sidebar));
                lit.extend(frames);
            }
        }
    }
    assert_eq!(lit.len(), 6 * 2 * 2 * 2);
    let alone = panel.dots.iter().map(|&dot| Scene {
        dots: vec![dot],
        lines: Vec::new(),
        ..panel.clone()
    });
    let lines = Scene {
        dots: Vec::new(),
        ..panel.clone()
    };
    let picked = ["n0", "n1", "n2", "n3", "n4"].map(|key| {
        let at = Topology {
            picked: Some(key),
            ..view(&nodes, &[], 600)
        };
        page(&at, 30).scenes
    });
    let parts = [
        (FillPart::System, 12_000),
        (FillPart::Tools, 18_000),
        (FillPart::Chat, 218_000),
    ];
    let fill = SlotFill {
        used: 248_000,
        estimated: false,
        window: Some(1_000_000),
        limit: Some(400_000),
        auto_compact: true,
        parts: parts
            .map(|(part, tokens)| PartTokens { part, tokens })
            .to_vec(),
        cached: Some(96),
    };
    let context = fill_scenes(&fill, 37);
    assert_eq!(context.len(), 2);
    let both: Vec<Scene> = context
        .iter()
        .chain([&panel])
        .chain(&boxes)
        .cloned()
        .collect();
    for (scene, with) in [panel.clone(), lines]
        .into_iter()
        .chain(lit)
        .chain(alone)
        .chain(picked.into_iter().flatten())
        .chain(context)
        .map(|scene| (scene, None))
        .chain(both.iter().map(|scene| (scene.clone(), Some(&both))))
    {
        let rects = tiles(&scene, (10, 20));
        for mode in [Mode::Dark, Mode::Light] {
            for ground in [Token::Bg, Token::Surface] {
                let theme = Theme::new(mode, Depth::TrueColor);
                let behind = theme.rgb(ground);
                let shown = with.map_or(std::slice::from_ref(&scene), |all| &all[..]);
                let ramps: Vec<_> = shown
                    .iter()
                    .flat_map(|s| ramps(s, &theme, behind))
                    .collect();
                let palette = palette(behind, &ramps);
                let mut seen = BTreeSet::new();
                for &(rect, _) in &rects {
                    let rgba = raster(&scene, &theme, (10, 20), rect, Some(behind));
                    seen.extend(rgba.as_chunks::<4>().0.iter().copied());
                }
                for p in seen {
                    let near = palette[nearest(&palette, &p)];
                    let off = [(near.r, p[0]), (near.g, p[1]), (near.b, p[2])]
                        .map(|(a, b)| a.abs_diff(b));
                    assert!(
                        off.iter().all(|&d| d <= 21),
                        "{mode:?} on {ground:?}: {p:?} is far from {near:?} in {:?} {:?}",
                        scene.lights.first().map(|l| l.along),
                        scene.rings.first().map(|r| r.on.mark),
                    );
                }
            }
        }
    }
}

#[test]
fn a_kitty_reply_is_a_refusal_unless_it_says_ok() {
    assert!(!refused("i=1073741824;OK"));
    assert!(refused("i=1073741824;EINVAL:bad parameters"));
    assert!(refused("garbage"));
}

#[test]
fn a_breathing_step_with_ten_working_agents_writes_under_12_kb() {
    let nodes: Vec<SlotTopologyNode> = (0..10)
        .map(|i| SlotTopologyNode {
            key: format!("w{i}"),
            parent: None,
            state: NodeState::Working,
        })
        .collect();
    let scene = |ms| scene_of(&nodes, &[], ms);
    for (sent, bytes) in written(&scene(0), &scene(600)) {
        assert_eq!(sent, 10);
        assert!(bytes < 12 * 1024, "{bytes}");
    }
}

#[test]
fn every_frame_of_a_burst_of_mail_writes_under_12_kb() {
    const THREADS: usize = 4;
    let nodes: Vec<SlotTopologyNode> = ["a", "b", "c", "a/x", "a/y", "b/x", "b/y", "c/x", "c/y"]
        .map(|key| SlotTopologyNode {
            key: key.to_owned(),
            parent: key.rsplit_once('/').map(|(parent, _)| parent.to_owned()),
            state: NodeState::Waiting,
        })
        .to_vec();
    let ends = [
        ("a/x", Some("a")),
        ("b/y", None),
        ("c/x", Some("b/x")),
        ("c/y", Some("a/y")),
    ];
    for (apart, count) in [(0, 4), (150, 4), (300, 3), (450, 3)] {
        let flows: Vec<SlotFlow> = (0..count)
            .zip(ends)
            .map(|(i, (from, to))| SlotFlow {
                from: Some(from.to_owned()),
                to: to.map(str::to_owned),
                at_ms: 1000 + apart * i,
            })
            .collect();
        let scene = |ms| scene_of(&nodes, &flows, ms);
        let landing = |ms| {
            [scene(ms), scene(ms + 33)]
                .iter()
                .any(|s| !s.rings.is_empty())
        };
        let starts: Vec<u64> = (989..1000 + apart * 3 + 1200 + 280)
            .filter(|&ms| ms % 11 == 0 || landing(ms))
            .collect();
        let worse = |a: [(usize, u64); 2], b: [(usize, u64); 2]| [a[0].max(b[0]), a[1].max(b[1])];
        let worst = std::thread::scope(|scope| {
            let runs: Vec<_> = (0..THREADS)
                .map(|t| {
                    let starts = starts.iter().skip(t).step_by(THREADS);
                    let bytes = |&ms: &u64| written(&scene(ms), &scene(ms + 33)).map(|w| (w.1, ms));
                    scope.spawn(move || starts.map(bytes).fold([(0, 0); 2], worse))
                })
                .collect();
            let runs = runs.into_iter().map(|run| run.join().unwrap());
            runs.fold([(0, 0); 2], worse)
        });
        for (bytes, ms) in worst {
            assert!(bytes < 12 * 1024, "{apart} ms apart, at {ms} ms: {bytes}");
        }
    }
}
