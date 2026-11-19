use wisp_protocol::slot::{SlotDisabled, SlotEntry, SlotFailure};

use super::*;
use crate::shared::{catalog, plugin};

fn entry(name: &str, default_on: bool) -> SlotEntry {
    SlotEntry {
        name: name.to_owned(),
        slot: SlotPlace::Sidebar,
        title: name.to_owned(),
        default_on,
        order: 0,
        disabled: None,
    }
}

fn content(words: &str) -> AgentEvent {
    AgentEvent::SlotContent {
        section: "aa/usage".to_owned(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![],
            blocks: vec![SlotBlock::Line {
                spans: vec![SlotSpan::Text {
                    text: words.to_owned(),
                    tone: wisp_protocol::slot::SlotTone::Text,
                    bold: false,
                }],
            }],
        },
    }
}

fn none(_: &str) -> Option<bool> {
    None
}

#[test]
fn the_band_and_the_transcript_take_the_planned_widths() {
    for (width, band_cols, transcript) in [
        (120, 38, 81),
        (155, 38, 116),
        (160, 40, 119),
        (192, 48, 143),
        (200, 48, 151),
        (250, 48, 201),
    ] {
        assert_eq!(band(width), band_cols, "{width}");
        assert_eq!(transcript_width(width, true), transcript, "{width}");
        assert_eq!(transcript_width(width, false), width, "{width}");
    }
    assert_eq!(overlay_band(80), 38);
    assert_eq!(overlay_band(43), 38);
    assert_eq!(overlay_band(42), 41);
    assert_eq!(overlay_band(40), 39);
    assert_eq!(overlay_band(39), 38);
    assert_eq!(overlay_band(30), 29);
    use wisp_render::theme::glyphs::GlyphSet;
    for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
        let side = wisp_render::chrome::frame_for(set).down;
        assert_eq!(wisp_render::text::width(side, set.into()), RULE, "{set:?}");
    }
}

#[test]
fn a_section_is_on_only_while_its_plugin_is_enabled_its_switch_on_and_no_cycle_holds_it() {
    let mut side = Side::new(SidebarSetting::Auto, 160);
    let mut cycled = entry("aa/cycled", true);
    cycled.disabled = Some(SlotDisabled::Cycle {
        cycle: vec!["aa/cycled".to_owned()],
    });
    side.event(&catalog(vec![
        plugin(
            "aa",
            true,
            vec![entry("aa/usage", true), entry("aa/off", false), cycled],
        ),
        plugin("bb", false, vec![entry("bb/tree", true)]),
    ]));
    assert_eq!(side.on(&none), ["aa/usage"]);
    let on = |path: &str| (path == "plugins.aa.slots.off").then_some(true);
    assert_eq!(side.on(&on), ["aa/off", "aa/usage"]);
    let off = |path: &str| (path == "plugins.aa.slots.usage").then_some(false);
    assert!(side.on(&off).is_empty());
}

#[test]
fn it_docks_only_once_a_section_draws_and_a_note_or_a_failure_never_latches() {
    let mut side = Side::new(SidebarSetting::Auto, 160);
    side.event(&catalog(vec![plugin(
        "aa",
        true,
        vec![entry("aa/usage", true)],
    )]));
    let on = side.on(&none);
    assert!(side.holds(&on));
    assert!(!side.wants_dock(&on));
    side.event(&AgentEvent::SlotFailed {
        section: "aa/usage".to_owned(),
        view: SlotView::Panel,
        failure: SlotFailure::TimedOut,
        parked: false,
        restarted: false,
    });
    side.event(&AgentEvent::SlotContent {
        section: "aa/usage".to_owned(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![SlotSpan::Text {
                text: "note".to_owned(),
                tone: wisp_protocol::slot::SlotTone::Text,
                bold: false,
            }],
            blocks: vec![],
        },
    });
    assert!(!side.wants_dock(&on));
    side.event(&content("hello"));
    assert!(side.wants_dock(&on));
    side.event(&AgentEvent::SlotContent {
        section: "aa/usage".to_owned(),
        view: SlotView::Panel,
        content: SlotContent::default(),
    });
    assert!(side.wants_dock(&on), "emptied content keeps it docked");
}

#[test]
fn the_rule_needs_auto_a_wide_terminal_and_a_section_on() {
    let mut side = Side::new(SidebarSetting::Auto, 119);
    side.event(&catalog(vec![plugin(
        "aa",
        true,
        vec![entry("aa/usage", true)],
    )]));
    side.event(&content("hello"));
    let on = side.on(&none);
    side.settle(119, &on);
    assert!(!side.docked());
    assert!(side.shown(&on).is_empty());
    side.settle(120, &on);
    assert!(side.docked());
    assert_eq!(side.shown(&on), on);
    side.set_setting(SidebarSetting::Hidden);
    side.settle(120, &on);
    assert!(!side.docked());
    assert!(side.shown(&on).is_empty());
    side.set_setting(SidebarSetting::Auto);
    side.settle(120, &[]);
    assert!(!side.docked());
}

#[test]
fn asking_docks_before_content_and_the_set_is_sent_once_per_change() {
    let mut side = Side::new(SidebarSetting::Auto, 160);
    side.event(&catalog(vec![plugin(
        "aa",
        true,
        vec![entry("aa/usage", true)],
    )]));
    let on = side.on(&none);
    let shown = |sections: &[String], pages: &[&str]| Shown {
        sections: sections.to_vec(),
        pages: pages.iter().map(|p| (*p).to_owned()).collect(),
    };
    assert_eq!(side.unsent(&on, None), Some(shown(&on, &[])));
    side.sent(shown(&on, &[]));
    assert_eq!(side.unsent(&on, None), None);
    assert!(!side.wants_dock(&on));
    side.ask();
    assert!(side.wants_dock(&on));
    side.set_setting(SidebarSetting::Hidden);
    assert_eq!(side.unsent(&on, None), Some(shown(&[], &[])));
    side.open_overlay(160);
    assert_eq!(side.unsent(&on, None), None);
    assert_eq!(
        side.unsent(&on, Some("aa/usage")),
        Some(shown(&on, &["aa/usage"]))
    );
    side.sent(shown(&on, &["aa/usage"]));
    assert_eq!(side.unsent(&on, None), Some(shown(&on, &[])));
}

#[test]
fn widening_closes_only_an_overlay_opened_narrow() {
    let mut side = Side::new(SidebarSetting::Hidden, 80);
    side.open_overlay(80);
    side.settle(119, &[]);
    assert!(side.overlay().is_some());
    side.settle(120, &[]);
    assert!(side.overlay().is_none());
    side.open_overlay(160);
    side.settle(200, &[]);
    assert!(side.overlay().is_some());
}

#[test]
fn ticks_follow_working_nodes_and_elapsed_facts_in_view() {
    let mut side = Side::new(SidebarSetting::Auto, 160);
    side.event(&catalog(vec![plugin(
        "aa",
        true,
        vec![entry("aa/usage", true)],
    )]));
    side.event(&AgentEvent::SlotContent {
        section: "aa/usage".to_owned(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![],
            blocks: vec![SlotBlock::Tree {
                nodes: vec![wisp_protocol::slot::SlotNode {
                    depth: 0,
                    state: NodeState::Working,
                    role: None,
                    label: "main".to_owned(),
                    facts: vec![SlotSpan::Elapsed {
                        since_ms: 0,
                        tone: wisp_protocol::slot::SlotTone::Muted,
                    }],
                }],
            }],
        },
    });
    assert_eq!(side.ticks(), Ticks::default(), "nothing drawn yet");
    let on = side.on(&none);
    let frame = Frame {
        height: 10,
        at: Duration::ZERO,
        now_ms: 0,
        cx: MotionCtx::new(
            wisp_render::motion::MotionPolicy::Full,
            wisp_render::theme::glyphs::GlyphSet::Unicode,
            wisp_render::theme::Depth::TrueColor,
        ),
        layout: Layout::new(37, wisp_render::theme::glyphs::GlyphSet::Unicode),
        whole: false,
        pictures: false,
    };
    side.draw(&on, frame);
    assert_eq!(
        side.ticks(),
        Ticks {
            pulse: true,
            clock: true,
            flight: false
        }
    );
    side.event(&AgentEvent::SlotContent {
        section: "aa/usage".to_owned(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![],
            blocks: vec![SlotBlock::Topology {
                root: Some("main".to_owned()),
                nodes: vec![wisp_protocol::slot::SlotTopologyNode {
                    key: "a".to_owned(),
                    parent: None,
                    state: NodeState::Working,
                }],
                flows: vec![wisp_protocol::slot::SlotFlow {
                    from: Some("a".to_owned()),
                    to: None,
                    at_ms: 1000,
                }],
                counted: None,
            }],
        },
    });
    let drawn = |side: &mut Side, now_ms, pictures| {
        side.draw(
            &on,
            Frame {
                now_ms,
                pictures,
                ..frame
            },
        );
        side.ticks()
    };
    let still = Ticks {
        pulse: true,
        clock: false,
        flight: false,
    };
    let flying = Ticks {
        flight: true,
        ..still
    };
    for pictures in [false, true] {
        assert_eq!(drawn(&mut side, 1198, pictures), flying, "33 ms frames");
        assert_eq!(drawn(&mut side, 1675, pictures), flying, "its ring");
        assert_eq!(drawn(&mut side, 1676, pictures), still, "gone");
    }
    let both = vec![entry("aa/list", true), entry("aa/usage", true)];
    side.event(&catalog(vec![plugin("aa", true, both)]));
    side.event(&listed("aa/list", 60));
    let on = side.on(&none);
    let lit = Frame {
        now_ms: 1198,
        ..frame
    };
    side.draw(&on, lit);
    assert!(!side.ticks().flight, "the box is scrolled out of view");
    assert!(side.scroll(Step::End));
    side.draw(&on, lit);
    assert!(side.ticks().flight, "and back in");
    side.draw(&[], frame);
    assert_eq!(side.ticks(), Ticks::default());
}

fn listed(section: &str, count: usize) -> AgentEvent {
    AgentEvent::SlotContent {
        section: section.to_owned(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![],
            blocks: vec![SlotBlock::List {
                items: (0..count)
                    .map(|i| {
                        vec![SlotSpan::Text {
                            text: format!("item {i}"),
                            tone: wisp_protocol::slot::SlotTone::Text,
                            bold: false,
                        }]
                    })
                    .collect(),
            }],
        },
    }
}

#[test]
fn the_position_is_kept_from_each_drawing_so_a_shrink_clamps_it() {
    let mut side = Side::new(SidebarSetting::Auto, 160);
    side.event(&catalog(vec![plugin(
        "aa",
        true,
        vec![entry("aa/usage", true)],
    )]));
    side.event(&listed("aa/usage", 60));
    let on = side.on(&none);
    let frame = Frame {
        height: 10,
        at: Duration::ZERO,
        now_ms: 0,
        cx: MotionCtx::new(
            wisp_render::motion::MotionPolicy::Reduced,
            wisp_render::theme::glyphs::GlyphSet::Unicode,
            wisp_render::theme::Depth::TrueColor,
        ),
        layout: Layout::new(38, wisp_render::theme::glyphs::GlyphSet::Unicode),
        whole: false,
        pictures: false,
    };
    side.draw(&on, frame);
    assert!(side.scroll(Step::End));
    let scrolled = side.position().clone();
    assert_ne!(scrolled, Position::Top);
    side.event(&listed("aa/usage", 3));
    let drawn = side.draw(&on, frame).position.clone();
    assert_ne!(drawn, scrolled);
    assert_eq!(side.position(), &drawn);
    assert!(
        !side.scroll(Step::Down(1)),
        "a clamped position scrolled on"
    );
}
