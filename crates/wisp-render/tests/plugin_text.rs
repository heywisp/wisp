use std::time::Duration;

use wisp_protocol::plugin::{Catalog, PluginSource};
use wisp_protocol::slot::{
    SlotBlock, SlotContent, SlotEntry, SlotPlace, SlotSpan, SlotTone, SlotView,
};
use wisp_protocol::{AgentEvent, PluginEntry};
use wisp_render::chrome::config_screen::{
    Detail, Entry, ListRow, detail_block, detail_lines, list_lines,
};
use wisp_render::chrome::sidebar::{PanelView, Position, Sidebar};
use wisp_render::chrome::{
    HelpRow, HelpSection, Suggestion, Suggestions, help_page, suggestion_rows,
};
use wisp_render::markdown::Layout;
use wisp_render::motion::{MotionCtx, MotionPolicy};
use wisp_render::styled::StyledLine;
use wisp_render::text::width;
use wisp_render::theme::glyphs::GlyphSet;

const WIDTHS: [usize; 3] = [40, 80, 120];
const SETS: [GlyphSet; 2] = [GlyphSet::Unicode, GlyphSet::Ascii];

fn summary() -> String {
    "界".repeat(60)
}

fn command_summary() -> String {
    "界".repeat(32)
}

fn title() -> String {
    "界".repeat(24)
}

fn fits(what: &str, lines: &[StyledLine], layout: Layout) {
    for line in lines {
        let used: usize = line
            .spans
            .iter()
            .map(|s| width(&s.text, layout.width_mode))
            .sum();
        assert!(
            used <= layout.width,
            "{what} at {} ({:?}): {used} cells: {line:?}",
            layout.width,
            layout.glyphs
        );
    }
}

fn layouts() -> impl Iterator<Item = Layout> {
    WIDTHS
        .into_iter()
        .flat_map(|w| SETS.into_iter().map(move |set| Layout::new(w, set)))
}

#[test]
fn the_slash_list_cuts_a_long_cjk_command_summary() {
    let list = Suggestions {
        rows: vec![
            Suggestion {
                name: "/testkit:go".into(),
                bold: 0..1,
                args: " [fast|slow]".into(),
                summary: command_summary(),
                enabled: true,
            },
            Suggestion {
                name: "/testkit:note".into(),
                bold: 0..1,
                args: String::new(),
                summary: command_summary(),
                enabled: true,
            },
        ],
        selected: Some(0),
        empty: String::new(),
    };
    for layout in layouts() {
        fits("/ list", &suggestion_rows(&list, 8, layout).lines, layout);
    }
}

#[test]
fn help_cuts_or_wraps_a_long_cjk_command_summary() {
    let sections = [
        HelpSection {
            lead: None,
            title: "Commands".into(),
            rows: vec![HelpRow {
                keys: "/testkit:go".into(),
                args: " [fast|slow]".into(),
                text: command_summary(),
            }],
        },
        HelpSection {
            lead: None,
            title: "Plugins".into(),
            rows: vec![HelpRow {
                keys: "/testkit:note".into(),
                args: String::new(),
                text: command_summary(),
            }],
        },
    ];
    for layout in layouts() {
        fits("help", &help_page(&sections, layout).rows, layout);
    }
}

#[test]
fn the_config_plugins_rows_cut_a_long_cjk_summary_and_title() {
    let entries = [
        Entry::Heading("Plugins".into()),
        Entry::Row(ListRow {
            label: "testkit".into(),
            value: summary(),
            marked: false,
            numeric: false,
        }),
        Entry::Row(ListRow {
            label: "demo/notes".into(),
            value: format!("{} \u{b7} sidebar section", title()),
            marked: false,
            numeric: false,
        }),
    ];
    let detail = Detail {
        title: "testkit".into(),
        notes: vec![summary(), title()],
        ..Detail::default()
    };
    for layout in layouts() {
        for sourced in [false, true] {
            fits(
                "/config list",
                &list_lines(&entries, Some(1), (0, 3), sourced, layout),
                layout,
            );
        }
        fits("/config detail", &detail_lines(&detail, layout), layout);
        fits(
            "/config detail block",
            &detail_block(&detail, layout),
            layout,
        );
    }
}

#[test]
fn the_sidebar_cuts_a_long_cjk_title() {
    let mut bar = Sidebar::default();
    let entry = SlotEntry {
        name: "demo/notes".into(),
        slot: SlotPlace::Sidebar,
        title: title(),
        default_on: true,
        order: 0,
        disabled: None,
    };
    bar.event(&AgentEvent::CatalogChanged {
        catalog: Catalog {
            plugins: vec![PluginEntry {
                prompt_sections: vec![],
                id: "demo".into(),
                version: "0.1.0".into(),
                summary: summary(),
                source: PluginSource::Bundled,
                installed: true,
                enabled: true,
                tools: vec![],
                commands: vec![],
                hooks: vec![],
                slots: vec![entry],
                settings: vec![],
                problems: vec![],
            }],
        },
    });
    bar.event(&AgentEvent::SlotContent {
        section: "demo/notes".into(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![SlotSpan::Text {
                text: "3 notes".into(),
                tone: SlotTone::Muted,
                bold: false,
            }],
            blocks: vec![SlotBlock::Line {
                spans: vec![SlotSpan::Text {
                    text: "first".into(),
                    tone: SlotTone::Text,
                    bold: false,
                }],
            }],
        },
    });
    let on = vec!["demo/notes".to_owned()];
    for layout in layouts() {
        for folded in [vec![], on.clone()] {
            let panel = bar.draw(&PanelView {
                on: &on,
                folded: &folded,
                pinned: &[],
                position: &Position::Top,
                height: 20,
                at: Duration::ZERO,
                now_ms: 0,
                cx: MotionCtx {
                    policy: MotionPolicy::Full,
                    ascii: layout.glyphs == GlyphSet::Ascii,
                    blend: true,
                },
                layout,
                whole: false,
                pictures: None,
            });
            assert!(!panel.lines.is_empty());
            fits("sidebar", &panel.lines, layout);
        }
    }
}
