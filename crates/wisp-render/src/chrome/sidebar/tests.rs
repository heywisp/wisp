use std::time::Duration;

use ratatui_core::style::Color;
use wisp_protocol::plugin::{Catalog, PluginSource};
use wisp_protocol::slot::{
    NodeState, SlotBlock, SlotEntry, SlotFlow, SlotNode, SlotNoun, SlotRow, SlotSpan, SlotTone,
    SlotTopologyNode,
};

use super::*;
use crate::chrome::context::{SPACE_TOKEN, cell_bar};
use crate::motion::{MotionPolicy, tree_pulse};
use crate::testing::{Rng, trimmed};
use crate::text::{WidthMode, width};
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Depth, Ink, Mode, Theme, Token};
use wisp_protocol::context::{FillPart, PartTokens};

const NOW: u64 = 1_800_000_000_000;

fn text(t: &str, tone: SlotTone) -> SlotSpan {
    SlotSpan::Text {
        text: t.into(),
        tone,
        bold: false,
    }
}

fn bold(t: &str, tone: SlotTone) -> SlotSpan {
    SlotSpan::Text {
        text: t.into(),
        tone,
        bold: true,
    }
}

fn usd(micros: u64, tone: SlotTone) -> SlotSpan {
    SlotSpan::Usd {
        micros,
        at_least: false,
        tone,
    }
}

fn elapsed(secs: u64, tone: SlotTone) -> SlotSpan {
    SlotSpan::Elapsed {
        since_ms: NOW - secs * 1000,
        tone,
    }
}

fn node(
    depth: u8,
    state: NodeState,
    role: Option<&str>,
    label: &str,
    facts: Vec<SlotSpan>,
) -> SlotNode {
    SlotNode {
        depth,
        state,
        role: role.map(str::to_owned),
        label: label.into(),
        facts,
    }
}

fn line(t: &str) -> SlotBlock {
    SlotBlock::Line {
        spans: vec![text(t, SlotTone::Text)],
    }
}

fn lines(n: usize, word: &str) -> Vec<SlotBlock> {
    (0..n).map(|i| line(&format!("{word} {i}"))).collect()
}

fn content(blocks: Vec<SlotBlock>) -> SlotContent {
    SlotContent {
        note: vec![],
        blocks,
    }
}

fn plugin(slots: Vec<SlotEntry>) -> PluginEntry {
    PluginEntry {
        prompt_sections: Vec::new(),
        id: "demo".into(),
        version: "0.1.0".into(),
        summary: String::new(),
        source: PluginSource::Bundled,
        installed: true,
        enabled: true,
        tools: vec![],
        commands: vec![],
        hooks: vec![],
        slots,
        settings: vec![],
        problems: vec![],
    }
}

fn entry(name: &str, title: &str, order: u32) -> SlotEntry {
    SlotEntry {
        name: name.into(),
        slot: SlotPlace::Sidebar,
        title: title.into(),
        default_on: true,
        order,
        disabled: None,
    }
}

fn show(bar: &mut Sidebar, section: &str, content: SlotContent) {
    assert!(bar.event(&AgentEvent::SlotContent {
        section: section.into(),
        view: SlotView::Panel,
        content,
    }));
}

fn fail(bar: &mut Sidebar, section: &str, failure: SlotFailure, parked: bool, restarted: bool) {
    assert!(bar.event(&AgentEvent::SlotFailed {
        section: section.into(),
        view: SlotView::Panel,
        failure,
        parked,
        restarted,
    }));
}

fn sidebar(sections: Vec<(&str, &str, SlotContent)>) -> Sidebar {
    let mut bar = Sidebar::default();
    let entries = (0u32..)
        .zip(&sections)
        .map(|(i, (name, title, _))| entry(name, title, i))
        .collect();
    assert!(bar.event(&AgentEvent::CatalogChanged {
        catalog: Catalog {
            plugins: vec![plugin(entries)],
        },
    }));
    for (name, _, c) in sections {
        show(&mut bar, name, c);
    }
    bar
}

fn names(bar: &Sidebar) -> Vec<String> {
    bar.sections().iter().map(|e| e.name.clone()).collect()
}

const FULL: MotionCtx = MotionCtx {
    policy: MotionPolicy::Full,
    ascii: false,
    blend: true,
};

struct Look {
    on: Vec<String>,
    folded: Vec<String>,
    pinned: Vec<String>,
    position: Position,
    height: usize,
    layout: Layout,
    at: Duration,
    cx: MotionCtx,
    whole: bool,
    pictures: Option<u16>,
}

impl Look {
    fn new(bar: &Sidebar, width: usize, height: usize) -> Look {
        Look {
            on: names(bar),
            folded: vec![],
            pinned: vec![],
            position: Position::Top,
            height,
            layout: Layout::new(width, GlyphSet::Unicode),
            at: Duration::ZERO,
            cx: FULL,
            whole: false,
            pictures: None,
        }
    }

    fn view(&self) -> PanelView<'_> {
        PanelView {
            on: &self.on,
            folded: &self.folded,
            pinned: &self.pinned,
            position: &self.position,
            height: self.height,
            at: self.at,
            now_ms: NOW,
            cx: self.cx,
            layout: self.layout,
            whole: self.whole,
            pictures: self.pictures,
        }
    }
}

fn owned(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn check_widths(panel: &Panel, layout: Layout) {
    for line in &panel.lines {
        let used: usize = line
            .spans
            .iter()
            .map(|s| width(&s.text, layout.width_mode))
            .sum();
        assert!(used <= layout.width, "{used} > {}: {line:?}", layout.width);
    }
}

fn draw(bar: &Sidebar, look: &Look) -> Panel {
    let panel = bar.draw(&look.view());
    check_widths(&panel, look.layout);
    assert!(panel.lines.len() < look.height.max(1));
    assert_eq!(panel.lines.len(), panel.rows.len());
    panel
}

fn shown(bar: &Sidebar, look: &Look) -> Vec<String> {
    inside(&draw(bar, look).lines)
}

fn edge(row: &str) -> bool {
    row.chars().count() == 2 && !row.starts_with(' ')
}

fn inside(lines: &[StyledLine]) -> Vec<String> {
    trimmed(lines)
        .into_iter()
        .map(|row| {
            let mut chars = row.chars();
            let left = chars.next().map(String::from).unwrap_or_default();
            let rest = chars.as_str();
            match rest.trim_start_matches(['─', '-']) {
                right if right.len() < rest.len() && right.chars().count() == 1 => {
                    format!("{left}{right}")
                }
                _ => rest
                    .strip_suffix(['│', '|'])
                    .unwrap_or(rest)
                    .trim_end()
                    .to_owned(),
            }
        })
        .collect()
}

fn tree_nodes() -> Vec<SlotNode> {
    use NodeState::*;
    vec![
        node(
            0,
            Working,
            Some("main"),
            "main",
            vec![text("", SlotTone::Muted), usd(420_000, SlotTone::Text)],
        ),
        node(
            1,
            Working,
            Some("coordinator"),
            "cache-fingerprints",
            vec![elapsed(134, SlotTone::Muted), usd(310_000, SlotTone::Text)],
        ),
        node(
            2,
            Working,
            Some("researcher"),
            "researcher",
            vec![elapsed(41, SlotTone::Muted), usd(40_000, SlotTone::Text)],
        ),
        node(
            2,
            Waiting,
            Some("coder"),
            "coder",
            vec![text("held", SlotTone::Muted), usd(120_000, SlotTone::Text)],
        ),
        node(
            2,
            Waiting,
            Some("plan-reviewer"),
            "reviewer",
            vec![text("queued", SlotTone::Muted)],
        ),
        node(
            1,
            Done,
            Some("coordinator"),
            "tui-research",
            vec![text("done", SlotTone::Muted), usd(200_000, SlotTone::Text)],
        ),
        node(
            2,
            Done,
            Some("researcher"),
            "brand",
            vec![text("done", SlotTone::Muted)],
        ),
        node(
            2,
            Done,
            Some("researcher"),
            "colour",
            vec![text("done", SlotTone::Muted)],
        ),
        node(
            2,
            Done,
            Some("researcher"),
            "motion",
            vec![text("done", SlotTone::Muted)],
        ),
        node(
            2,
            Done,
            Some("researcher"),
            "glyphs",
            vec![text("done", SlotTone::Muted)],
        ),
    ]
}

fn agents() -> SlotContent {
    SlotContent {
        note: vec![
            text("3 working · ", SlotTone::Muted),
            usd(1_240_000, SlotTone::Muted),
        ],
        blocks: vec![SlotBlock::Tree {
            nodes: tree_nodes(),
        }],
    }
}

#[test]
fn the_agent_tree_in_38_columns_is_the_plans_drawing() {
    let bar = sidebar(vec![
        ("agent-viz/tree", "Agents", agents()),
        ("demo/notes", "Notes", content(lines(10, "note"))),
    ]);
    let mut look = Look::new(&bar, 39, 24);
    look.pinned = owned(&["agent-viz/tree"]);
    let panel = draw(&bar, &look);
    let want = [
        "╭╮",
        " Agents            3 working · $1.24",
        "├┤",
        " ● main                        $0.42",
        " ├─● cache-fingerpri…  2m 14s  $0.31",
        " │ ├─● researcher         41s  $0.04",
        " │ ├─○ coder             held  $0.12",
        " │ └─○ reviewer        queued",
        " └─✓ tui-research        done  $0.20",
        " … 4 more",
        "├┤",
    ];
    assert_eq!(inside(&panel.lines)[..11], want);
    assert_eq!(panel.pinned_rows, 11);
}

fn every_block() -> SlotContent {
    use NodeState::*;
    SlotContent {
        note: vec![
            text("2 items · ", SlotTone::Muted),
            usd(1_240_000, SlotTone::Muted),
        ],
        blocks: vec![
            SlotBlock::Line {
                spans: vec![
                    text("Plain ", SlotTone::Text),
                    bold("bold", SlotTone::Text),
                    text(" accent", SlotTone::Accent),
                    text(" ok", SlotTone::Ok),
                    text(" error", SlotTone::Error),
                    text(" muted", SlotTone::Muted),
                ],
            },
            SlotBlock::Line {
                spans: vec![text("only faint words", SlotTone::Faint)],
            },
            SlotBlock::Rows {
                rows: vec![
                    SlotRow {
                        key: "model".into(),
                        value: vec![text("opus", SlotTone::Text)],
                    },
                    SlotRow {
                        key: "spent".into(),
                        value: vec![usd(420_000, SlotTone::Text)],
                    },
                    SlotRow {
                        key: "a key long enough to be cut first".into(),
                        value: vec![text("value kept", SlotTone::Muted)],
                    },
                ],
            },
            SlotBlock::List {
                items: vec![
                    vec![text(
                        "a first item long enough to wrap onto a second row",
                        SlotTone::Text,
                    )],
                    vec![text("second", SlotTone::Muted)],
                ],
            },
            SlotBlock::Gauge {
                label: "fill".into(),
                value: 64,
                max: 100,
                tone: SlotTone::Text,
                note: vec![text("64k/100k", SlotTone::Muted)],
            },
            SlotBlock::Spark {
                label: "turns".into(),
                values: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 4, 2],
                max: 10,
                tone: SlotTone::Accent,
                note: vec![text("13 turns", SlotTone::Muted)],
            },
            SlotBlock::Tree {
                nodes: vec![
                    node(
                        0,
                        Working,
                        Some("main"),
                        "main",
                        vec![elapsed(75, SlotTone::Muted)],
                    ),
                    node(
                        1,
                        Waiting,
                        Some("coder"),
                        "coder",
                        vec![text("held", SlotTone::Muted)],
                    ),
                    node(
                        1,
                        Stopped,
                        Some("code-reviewer"),
                        "reviewer",
                        vec![text("stopped", SlotTone::Muted)],
                    ),
                    node(
                        1,
                        Done,
                        Some("researcher"),
                        "researcher",
                        vec![text("done", SlotTone::Muted)],
                    ),
                    node(
                        1,
                        Failed,
                        Some("designer"),
                        "designer",
                        vec![text("failed", SlotTone::Error)],
                    ),
                    node(
                        1,
                        Working,
                        None,
                        "helper",
                        vec![text("running", SlotTone::Muted)],
                    ),
                ],
            },
            SlotBlock::Fill {
                fill: wisp_protocol::slot::SlotFill {
                    used: 90_000,
                    estimated: false,
                    window: Some(200_000),
                    limit: Some(100_000),
                    auto_compact: true,
                    parts: [
                        (FillPart::System, 10_000),
                        (FillPart::Tools, 20_000),
                        (FillPart::Chat, 40_000),
                        (FillPart::Skills, 10_000),
                        (FillPart::Unknown, 10_000),
                    ]
                    .into_iter()
                    .map(|(part, tokens)| PartTokens { part, tokens })
                    .collect(),
                    cached: Some(0),
                },
            },
            SlotBlock::NextColumn,
            SlotBlock::Unknown,
        ],
    }
}

fn colour(c: Option<Color>) -> String {
    match c {
        None => "-".to_owned(),
        Some(Color::Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
        Some(Color::Indexed(i)) => format!("i{i}"),
        Some(other) => format!("{other:?}"),
    }
}

fn painted(lines: &[StyledLine], theme: &Theme) -> String {
    let mut out = vec![];
    for (line, plain) in lines.iter().zip(trimmed(lines)) {
        out.push(format!("|{plain}"));
        for span in &line.spans {
            if span.text.trim().is_empty() {
                continue;
            }
            let style = theme.style(&span.look);
            let mods = if style.add_modifier.is_empty() {
                String::new()
            } else {
                format!(" {:?}", style.add_modifier)
            };
            out.push(format!("    {:?} {}{mods}", span.text, colour(style.fg)));
        }
    }
    out.join("\n")
}

fn every_block_drawn(theme: Theme, set: GlyphSet) -> String {
    let bar = sidebar(vec![("demo/blocks", "Blocks", every_block())]);
    let mut out = vec![];
    for width in [41, 51] {
        let mut look = Look::new(&bar, width, 60);
        look.layout = Layout::new(width, set);
        look.at = Duration::from_millis(500);
        look.cx = MotionCtx::new(MotionPolicy::Full, set, theme.depth());
        out.push(format!("== {width} columns"));
        out.push(painted(&draw(&bar, &look).lines, &theme));
    }
    out.join("\n")
}

#[test]
fn every_block_in_dark() {
    insta::assert_snapshot!(
        "sidebar_dark",
        every_block_drawn(Theme::new(Mode::Dark, Depth::TrueColor), GlyphSet::Unicode)
    );
}

#[test]
fn every_block_in_ansi16() {
    insta::assert_snapshot!(
        "sidebar_ansi16",
        every_block_drawn(Theme::new(Mode::Dark, Depth::Ansi16), GlyphSet::Unicode)
    );
}

#[test]
fn every_block_in_no_color() {
    let drawn = every_block_drawn(Theme::new(Mode::Dark, Depth::None), GlyphSet::Unicode);
    assert!(drawn.contains("├─○ coder"), "{drawn}");
    assert!(drawn.contains("├─■ reviewer"), "{drawn}");
    insta::assert_snapshot!("sidebar_no_color", drawn);
}

#[test]
fn every_block_in_ambiguous_wide() {
    insta::assert_snapshot!(
        "sidebar_ambiguous_wide",
        every_block_drawn(
            Theme::new(Mode::Dark, Depth::TrueColor),
            GlyphSet::AmbiguousWide
        )
    );
}

#[test]
fn every_block_in_ascii() {
    insta::assert_snapshot!(
        "sidebar_ascii",
        every_block_drawn(Theme::new(Mode::Dark, Depth::TrueColor), GlyphSet::Ascii)
    );
}

fn topology_box(set: GlyphSet) -> (Vec<String>, String) {
    use NodeState::*;
    let node = |key: &str, parent: Option<&str>, state| SlotTopologyNode {
        key: key.into(),
        parent: parent.map(str::to_owned),
        state,
    };
    let root = Some("agent-viz");
    let nodes = vec![
        node("agent-viz", None, Working),
        node("agent-viz/coder", root, Working),
        node("agent-viz/helper", root, Done),
        node("agent-viz/plan-review-a", root, Done),
        node("agent-viz/plan-review-b", root, Stopped),
        node("agent-viz/planner", root, Done),
    ];
    let topology = SlotBlock::Topology {
        root: Some("main".into()),
        nodes,
        flows: Vec::new(),
        counted: None,
    };
    let bar = sidebar(vec![(
        "agent-viz/topology",
        "Agents",
        content(vec![topology]),
    )]);
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let mut look = Look::new(&bar, 39, 24);
    look.layout = Layout::new(39, set);
    look.at = Duration::from_millis(500);
    look.cx = MotionCtx::new(MotionPolicy::Full, set, theme.depth());
    let panel = draw(&bar, &look);
    (inside(&panel.lines), painted(&panel.lines, &theme))
}

#[test]
fn a_topology_box_is_the_minimap_of_the_frames() {
    let (rows, painted) = topology_box(GlyphSet::Unicode);
    assert_eq!(
        rows,
        [
            "╭╮",
            " Agents",
            "├┤",
            "                  ⠶",
            "                  ⡇",
            "                  ⡇",
            "                  ●",
            "    ⡖⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡗⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡆",
            "    ⡇      ⡇      ⡇      ⡇      ⡇",
            "    ●      ✓      ✓      ■      ✓",
            "",
            " ⠶ main  ● working  ○ waiting",
            " ✓ done  ■ stopped  ⨯ failed",
            "├╯",
        ]
    );
    insta::assert_snapshot!("sidebar_topology_unicode", painted);
}

#[test]
fn a_topology_box_in_ascii() {
    let (rows, painted) = topology_box(GlyphSet::Ascii);
    assert_eq!(
        rows[6..10],
        [
            "                  @",
            "    .------.------|------.------.",
            "    |      |      |      |      |",
            "    @      +      +      =      +"
        ]
    );
    insta::assert_snapshot!("sidebar_topology_ascii", painted);
}

#[test]
fn the_agents_box_counts_its_agents_beside_its_title_as_the_strip_narrows() {
    use NodeState::{Failed, Waiting, Working};
    let topology = |states: &[NodeState], counted| {
        content(vec![SlotBlock::Topology {
            root: Some("main".into()),
            nodes: (states.iter().enumerate())
                .map(|(i, &state)| SlotTopologyNode {
                    key: format!("a{i}"),
                    parent: None,
                    state,
                })
                .collect(),
            flows: Vec::new(),
            counted,
        }])
    };
    let five = [Working, Waiting, Waiting, Waiting, Waiting];
    let six = [Working, Working, Failed, Waiting, Waiting, Waiting];
    let draw = |states: &[NodeState], set, folded: &[&str]| {
        let bar = sidebar(vec![
            (
                "agent-viz/topology",
                "Agents",
                topology(states, Some(SlotNoun::Agents)),
            ),
            ("demo/topology", "Other", topology(states, None)),
            ("agent-viz/tree", "Tree", agents()),
        ]);
        let mut look = Look::new(&bar, 39, 60);
        look.layout = Layout::new(39, set);
        look.folded = owned(folded);
        let panel = bar.draw(&look.view());
        check_widths(&panel, look.layout);
        panel
    };
    let titles = |panel: &Panel| -> Vec<String> {
        inside(&panel.lines)
            .into_iter()
            .filter(|row| {
                [" Agents", " Other", " Tree"]
                    .iter()
                    .any(|t| row.starts_with(t))
            })
            .collect()
    };
    let unicode = GlyphSet::Unicode;
    assert_eq!(
        titles(&draw(&five, unicode, &[])),
        [
            " Agents         5 agents · 1 working",
            " Other",
            " Tree              3 working · $1.24",
        ]
    );
    let panel = draw(&six, unicode, &[]);
    assert_eq!(titles(&panel)[0], " Agents     6 · 2 working · 1 failed");
    let look_of = |text: &str| {
        panel
            .lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.text == text)
            .map(|s| s.look)
    };
    for (text, token) in [
        ("6", Token::Text),
        (" · ", Token::TextFaint),
        ("2 working", Token::TextMuted),
        ("1 failed", Token::Error),
    ] {
        assert_eq!(
            look_of(text),
            Some(crate::styled::Look::fg(token)),
            "{text}"
        );
    }
    assert_eq!(
        titles(&draw(&six, GlyphSet::Ascii, &[]))[0],
        " Agents     6 - 2 working - 1 failed"
    );
    assert_eq!(
        titles(&draw(&five, unicode, &["agent-viz/topology"]))[0],
        " Agents ▸       5 agents · 1 working"
    );
}

#[test]
fn a_topology_keeps_its_flows_and_what_it_counts_through_ingest() {
    let topology = content(vec![SlotBlock::Topology {
        root: None,
        nodes: vec![SlotTopologyNode {
            key: "/a".into(),
            parent: None,
            state: NodeState::Working,
        }],
        flows: vec![SlotFlow {
            from: None,
            to: Some("/a".into()),
            at_ms: 7,
        }],
        counted: Some(SlotNoun::Agents),
    }]);
    let bar = sidebar(vec![("agent-viz/topology", "Agents", topology.clone())]);
    let kept = bar.panel("agent-viz/topology").unwrap();
    assert_eq!(kept.content, Some(topology));
}

#[test]
fn a_folded_section_is_its_title_with_the_mark_and_its_note() {
    let bar = sidebar(vec![
        ("agent-viz/tree", "Agents", agents()),
        ("demo/notes", "Notes", content(lines(2, "note"))),
    ]);
    let mut look = Look::new(&bar, 41, 30);
    look.folded = owned(&["agent-viz/tree"]);
    assert_eq!(
        shown(&bar, &look),
        [
            "╭╮",
            " Agents ▸            3 working · $1.24",
            "├┤",
            " Notes",
            "├┤",
            " note 0",
            " note 1",
            "├╯",
        ]
    );
    look.layout = Layout::new(41, GlyphSet::Ascii);
    assert_eq!(
        shown(&bar, &look)[..2],
        ["++", " Agents >            3 working · $1.24"]
    );
}

#[test]
fn a_failing_folded_section_keeps_its_failure_row() {
    let mut bar = sidebar(vec![("agent-viz/tree", "Agents", agents())]);
    fail(
        &mut bar,
        "agent-viz/tree",
        SlotFailure::Failed {
            message: "boom".into(),
        },
        false,
        false,
    );
    let mut look = Look::new(&bar, 41, 30);
    look.folded = owned(&["agent-viz/tree"]);
    let panel = draw(&bar, &look);
    assert_eq!(
        inside(&panel.lines),
        [
            "╭╮",
            " Agents ▸            3 working · $1.24",
            "├┤",
            " couldn't update: boom",
            "├╯",
        ]
    );
    assert_eq!(
        panel.rows,
        [
            None,
            Some(RowOf::Title("agent-viz/tree".into())),
            None,
            Some(RowOf::Body("agent-viz/tree".into())),
            None,
        ]
    );
}

fn gauge(label: &str) -> SlotBlock {
    SlotBlock::Gauge {
        label: label.into(),
        value: 50,
        max: 100,
        tone: SlotTone::Text,
        note: vec![],
    }
}

fn tree_of(n: usize) -> SlotContent {
    content(vec![SlotBlock::Tree {
        nodes: (0..n)
            .map(|i| {
                node(
                    u8::from(i > 0),
                    NodeState::Done,
                    None,
                    &format!("n{i}"),
                    vec![],
                )
            })
            .collect(),
    }])
}

#[test]
fn a_pinned_gauge_sits_above_the_rule_with_a_tree_scrolled_under_it() {
    let bar = sidebar(vec![
        ("ctx/usage", "Context", content(vec![gauge("fill")])),
        ("agent-viz/tree", "Agents", tree_of(20)),
    ]);
    let mut look = Look::new(&bar, 41, 12);
    look.pinned = owned(&["ctx/usage"]);
    let first = draw(&bar, &look);
    assert_eq!(first.pinned_rows, 5);
    let rows = inside(&first.lines);
    assert_eq!(rows[..3], ["╭╮", " Context", "├┤"]);
    assert!(rows[3].starts_with(" fill  ⣿"), "{rows:?}");
    assert_eq!(rows[4..7], ["├┤", " Agents", "├┤"]);
    assert_eq!(rows[10], " ↓ 17 more");
    look.position = first.scroll(&first.position, Step::Down(4));
    let rows = shown(&bar, &look);
    assert_eq!(rows[..5], inside(&first.lines)[..5]);
    assert_eq!(rows[5], " ↑ 4 more");
    assert_eq!(rows[10], " ↓ 13 more");
}

fn rows_section(n: usize) -> SlotContent {
    content(lines(n, "row"))
}

#[test]
fn pinned_sections_share_their_rows_by_water_fill() {
    let gauges = content(vec![gauge("one"), gauge("two")]);
    let bar = sidebar(vec![
        ("ctx/usage", "Context", gauges.clone()),
        ("agent-viz/tree", "Agents", tree_of(29)),
        ("demo/notes", "Notes", rows_section(29)),
    ]);
    let mut look = Look::new(&bar, 41, 27);
    look.pinned = owned(&["ctx/usage", "agent-viz/tree"]);
    let panel = draw(&bar, &look);
    assert_eq!(panel.pinned_rows, 13);
    let rows = inside(&panel.lines);
    assert_eq!(rows[..3], ["╭╮", " Context", "├┤"]);
    assert!(rows[4].starts_with(" two"));
    assert_eq!(rows[5..8], ["├┤", " Agents", "├┤"]);
    assert_eq!(rows[8..11], [" ✓ n0", " ├─✓ n1", " ├─✓ n2"]);
    assert_eq!(rows[11], " … 26 more");
    assert_eq!(rows[12..14], ["├┤", " Notes"]);

    let bar = sidebar(vec![
        ("ctx/usage", "Context", gauges),
        ("agent-viz/tree", "Agents", tree_of(29)),
        ("demo/a", "More A", rows_section(3)),
        ("demo/b", "More B", rows_section(3)),
        ("demo/notes", "Notes", rows_section(29)),
    ]);
    look = Look::new(&bar, 41, 25);
    look.pinned = owned(&["ctx/usage", "agent-viz/tree", "demo/a", "demo/b"]);
    let panel = draw(&bar, &look);
    assert_eq!(panel.pinned_rows, 12);
    let rows = inside(&panel.lines);
    assert_eq!(
        rows[..13],
        [
            "╭╮",
            " Context",
            " … 2 more",
            "├┤",
            " Agents",
            " … 29 more",
            "├┤",
            " More A",
            " … 3 more",
            "├╯",
            " … 1 more section",
            "├╮",
            " Notes",
        ]
    );
    assert_eq!(panel.rows[10], None);
    assert_eq!(panel.rows[8], Some(RowOf::Body("demo/a".into())));
}

#[test]
fn a_pinned_part_that_fits_is_never_held_to_half() {
    let bar = sidebar(vec![
        ("agent-viz/tree", "Agents", tree_of(16)),
        ("demo/notes", "Notes", rows_section(3)),
    ]);
    let mut look = Look::new(&bar, 41, 27);
    look.pinned = owned(&["agent-viz/tree"]);
    let panel = draw(&bar, &look);
    assert_eq!(panel.pinned_rows, 20);
    let rows = inside(&panel.lines);
    assert_eq!(rows[18], " ├─✓ n15".replace('├', "└"));
    assert_eq!(rows[19], "├┤");
    assert_eq!(
        rows[20..],
        [" Notes", "├┤", " row 0", " row 1", " row 2", "├╯"]
    );
}

#[test]
fn with_every_section_pinned_they_take_the_whole_panel_and_end_in_a_foot() {
    let bar = sidebar(vec![
        ("demo/a", "A", rows_section(7)),
        ("demo/b", "B", rows_section(7)),
    ]);
    let mut look = Look::new(&bar, 41, 12);
    look.pinned = names(&bar);
    let panel = draw(&bar, &look);
    assert_eq!(panel.pinned_rows, 11);
    assert_eq!(
        inside(&panel.lines),
        [
            "╭╮",
            " A",
            "├┤",
            " row 0",
            " … 6 more",
            "├┤",
            " B",
            "├┤",
            " row 0",
            " … 6 more",
            "├╯",
        ]
    );
}

fn scroller() -> Sidebar {
    sidebar(vec![
        ("demo/a", "A", rows_section(4)),
        ("demo/b", "B", rows_section(4)),
        ("demo/c", "C", rows_section(4)),
    ])
}

#[test]
fn more_rows_show_at_the_top_in_the_middle_and_at_the_end() {
    let bar = scroller();
    let mut look = Look::new(&bar, 41, 9);
    let top = draw(&bar, &look);
    assert_eq!(
        inside(&top.lines),
        [
            "╭╮",
            " A",
            "├┤",
            " row 0",
            " row 1",
            " row 2",
            " row 3",
            " ↓ 10 more"
        ]
    );
    assert_eq!(top.rows[7], None);
    look.position = top.scroll(&top.position, Step::Down(8));
    let middle = draw(&bar, &look);
    assert_eq!(
        inside(&middle.lines),
        [
            " ↑ 6 more",
            "├┤",
            " row 0",
            " row 1",
            " row 2",
            " row 3",
            "├┤",
            " ↓ 5 more"
        ]
    );
    look.position = middle.scroll(&middle.position, Step::End);
    let end = draw(&bar, &look);
    assert_eq!(
        inside(&end.lines),
        [
            " ↑ 10 more",
            " C",
            "├┤",
            " row 0",
            " row 1",
            " row 2",
            " row 3",
            "├╯"
        ]
    );
    assert_eq!(end.rows[0], None);
    look.height = 3;
    look.position = Position::Top;
    let short = draw(&bar, &look);
    assert_eq!(inside(&short.lines), ["╭╮", " A"]);
    look.position = short.scroll(&short.position, Step::Down(5));
    assert_eq!(shown(&bar, &look), [" row 2", " row 3"]);
}

#[test]
fn a_section_growing_above_the_first_visible_one_moves_nothing() {
    let mut bar = scroller();
    let mut look = Look::new(&bar, 41, 9);
    let top = draw(&bar, &look);
    look.position = top.scroll(&top.position, Step::Down(8));
    let before = draw(&bar, &look);
    show(&mut bar, "demo/a", rows_section(9));
    look.position = before.position.clone();
    let after = draw(&bar, &look);
    assert_eq!(inside(&after.lines)[1..], inside(&before.lines)[1..]);
    assert_eq!(inside(&before.lines)[0], " ↑ 6 more");
    assert_eq!(inside(&after.lines)[0], " ↑ 11 more");
}

#[test]
fn the_position_follows_its_section_when_it_shrinks_or_leaves() {
    let mut bar = scroller();
    let mut look = Look::new(&bar, 41, 5);
    look.position = Position::At {
        section: "demo/b".into(),
        order: 1,
        row: 6,
    };
    let at = draw(&bar, &look);
    assert_eq!(at.position, look.position);
    assert_eq!(inside(&at.lines)[1..3], ["├┤", " C"]);
    show(&mut bar, "demo/b", rows_section(1));
    let shrunk = draw(&bar, &look);
    assert_eq!(
        shrunk.position,
        Position::At {
            section: "demo/b".into(),
            order: 1,
            row: 3,
        }
    );
    look.position = shrunk.position.clone();
    look.on = owned(&["demo/a", "demo/c"]);
    let gone = draw(&bar, &look);
    assert_eq!(
        gone.position,
        Position::At {
            section: "demo/c".into(),
            order: 2,
            row: 0,
        }
    );
    assert_eq!(inside(&gone.lines)[1..3], [" C", "├┤"]);
    look.position = Position::At {
        section: "demo/c".into(),
        order: 2,
        row: 2,
    };
    look.pinned = owned(&["demo/c"]);
    look.height = 16;
    let pinned = draw(&bar, &look);
    assert_eq!(pinned.position, Position::Top);
}

#[test]
fn a_pinned_last_section_sends_the_position_to_the_end() {
    let bar = scroller();
    let mut look = Look::new(&bar, 41, 17);
    look.position = Position::At {
        section: "demo/c".into(),
        order: 2,
        row: 2,
    };
    look.pinned = owned(&["demo/c"]);
    let panel = draw(&bar, &look);
    assert_eq!(panel.pinned_rows, 8);
    assert_eq!(
        panel.position,
        Position::At {
            section: "demo/b".into(),
            order: 1,
            row: 0,
        }
    );
    let rows = inside(&panel.lines);
    assert_eq!(rows[8..10], [" ↑ 5 more", " B"]);
    assert_eq!(rows[14..], [" row 3", "├╯"]);
}

#[test]
fn the_top_stays_the_top_when_a_section_appears_above() {
    let mut bar = sidebar(vec![
        ("demo/a", "A", content(vec![])),
        ("demo/b", "B", rows_section(9)),
    ]);
    let look = Look::new(&bar, 41, 6);
    let first = draw(&bar, &look);
    assert_eq!(first.position, Position::Top);
    assert_eq!(inside(&first.lines)[..2], ["╭╮", " B"]);
    show(&mut bar, "demo/a", rows_section(3));
    let mut look = look;
    look.position = first.position;
    assert_eq!(shown(&bar, &look)[..2], ["╭╮", " A"]);
}

#[test]
fn page_steps_and_the_top_move_by_the_scrolling_height() {
    let bar = scroller();
    let mut look = Look::new(&bar, 41, 9);
    let top = draw(&bar, &look);
    let down = top.scroll(&top.position, Step::PageDown);
    assert_eq!(
        down,
        Position::At {
            section: "demo/b".into(),
            order: 1,
            row: 0,
        }
    );
    look.position = down.clone();
    let middle = draw(&bar, &look);
    assert_eq!(inside(&middle.lines)[1], " B");
    let far = (0..5).fold(down, |at, _| middle.scroll(&at, Step::PageDown));
    assert_eq!(far, middle.scroll(&middle.position, Step::End));
    assert_eq!(middle.scroll(&middle.position, Step::PageUp), Position::Top);
    assert_eq!(middle.scroll(&middle.position, Step::Top), Position::Top);
    assert_eq!(
        middle.scroll(&middle.position, Step::Down(1000)),
        middle.scroll(&middle.position, Step::End)
    );
}

#[test]
fn past_the_end_the_first_step_back_moves_at_once() {
    let bar = scroller();
    let mut look = Look::new(&bar, 41, 6);
    let mut panel = draw(&bar, &look);
    look.position = panel.scroll(&panel.position, Step::End);
    panel = draw(&bar, &look);
    let end = trimmed(&panel.lines);
    for _ in 0..5 {
        look.position = panel.scroll(&panel.position, Step::Down(1));
        panel = draw(&bar, &look);
        assert_eq!(trimmed(&panel.lines), end);
        look.position = panel.position.clone();
    }
    look.position = panel.scroll(&panel.position, Step::Up(1));
    assert_ne!(shown(&bar, &look), end);
}

#[test]
fn the_returned_rows_name_each_sections_title_and_body() {
    let bar = sidebar(vec![
        (
            "ctx/usage",
            "Context",
            content(vec![gauge("fill"), SlotBlock::NextColumn]),
        ),
        ("demo/only", "Only", content(vec![SlotBlock::NextColumn])),
        ("demo/a", "A", rows_section(4)),
        ("demo/b", "B", rows_section(4)),
        ("demo/c", "C", rows_section(9)),
        ("demo/d", "D", rows_section(9)),
    ]);
    let mut look = Look::new(&bar, 41, 19);
    look.pinned = owned(&["demo/a", "demo/b", "demo/c"]);
    let panel = draw(&bar, &look);
    let rows = inside(&panel.lines);
    assert!(!rows.iter().any(|r| r.contains("Only")), "{rows:?}");
    let mut seen = vec![];
    for (row, of) in rows.iter().zip(&panel.rows) {
        match of {
            Some(RowOf::Title(name)) => {
                let entry = bar.sections().iter().find(|e| &e.name == name).unwrap();
                assert!(row.starts_with(&format!(" {}", entry.title)), "{row}");
                seen.push(name.clone());
            }
            Some(RowOf::Body(name)) => {
                assert!(seen.last() == Some(name), "{row} after {seen:?}");
                assert!(
                    row.starts_with(" row") || row.starts_with(" fill") || row.contains("more"),
                    "{row}"
                );
            }
            None => assert!(edge(row) || row.contains("more"), "{row}"),
        }
    }
    assert!(rows.iter().any(|r| r.contains("more section")), "{rows:?}");
    assert!(rows.iter().any(|r| edge(r)), "{rows:?}");
    assert!(rows.iter().any(|r| r.contains("↓")), "{rows:?}");
    let half = format!(" fill  {}{}  50%", "⣿".repeat(13), "⣀".repeat(13));
    assert_eq!(rows[9..12], [" Context".to_owned(), "├┤".to_owned(), half]);
}

#[test]
fn unknown_views_and_slots_are_dropped_at_ingest() {
    let mut bar = sidebar(vec![("demo/a", "A", rows_section(1))]);
    let before = bar.clone();
    assert!(!bar.event(&AgentEvent::SlotContent {
        section: "demo/b".into(),
        view: SlotView::Unknown,
        content: rows_section(3),
    }));
    assert!(!bar.event(&AgentEvent::SlotFailed {
        section: "demo/a".into(),
        view: SlotView::Unknown,
        failure: SlotFailure::TimedOut,
        parked: true,
        restarted: false,
    }));
    assert_eq!(bar, before);
    let mut odd = entry("demo/odd", "Odd", 0);
    odd.slot = SlotPlace::Unknown;
    assert!(bar.event(&AgentEvent::CatalogChanged {
        catalog: Catalog {
            plugins: vec![plugin(vec![entry("demo/a", "A", 1), odd])],
        },
    }));
    assert_eq!(names(&bar), ["demo/a"]);
    show(&mut bar, "demo/odd", rows_section(2));
    let mut look = Look::new(&bar, 41, 20);
    look.on = owned(&["demo/a", "demo/odd"]);
    assert_eq!(shown(&bar, &look), ["╭╮", " A", "├┤", " row 0", "├╯"]);
}

#[test]
fn the_page_view_is_kept_apart_from_the_panel() {
    let mut bar = sidebar(vec![("demo/a", "A", rows_section(1))]);
    assert!(bar.event(&AgentEvent::SlotContent {
        section: "demo/a".into(),
        view: SlotView::Page,
        content: rows_section(5),
    }));
    assert_eq!(bar.page("demo/a").unwrap().content, Some(rows_section(5)));
    assert_eq!(bar.panel("demo/a").unwrap().content, Some(rows_section(1)));
    assert_eq!(
        shown(&bar, &Look::new(&bar, 41, 20)),
        ["╭╮", " A", "├┤", " row 0", "├╯"]
    );
}

#[test]
fn the_title_note_is_cut_first() {
    let mut c = rows_section(1);
    c.note = vec![text(
        "a note far too long to fit beside the title",
        SlotTone::Muted,
    )];
    let bar = sidebar(vec![("demo/a", "A title", c)]);
    let rows = shown(&bar, &Look::new(&bar, 41, 20));
    assert_eq!(rows[1], " A title  a note far too long to fit …");
}

#[test]
fn failure_rows_cut_the_reason_and_say_restarted_or_parked() {
    let message = "the plugin could not read its own state file after the update";
    let mut bar = sidebar(vec![("demo/a", "A", rows_section(1))]);
    let look = Look::new(&bar, 41, 20);
    let failed = SlotFailure::Failed {
        message: message.into(),
    };
    fail(&mut bar, "demo/a", failed.clone(), false, false);
    assert_eq!(
        shown(&bar, &look),
        [
            "╭╮",
            " A",
            "├┤",
            " couldn't update: the plugin could no…",
            "├┤",
            " row 0",
            "├╯"
        ]
    );
    fail(&mut bar, "demo/a", failed, false, true);
    assert_eq!(
        shown(&bar, &look)[3],
        " couldn't update: the plugin could no…"
    );
    let wide = Look::new(&bar, 51, 20);
    fail(&mut bar, "demo/a", SlotFailure::TimedOut, false, true);
    assert_eq!(
        shown(&bar, &look)[3],
        " couldn't update: timed out · plugin …"
    );
    assert_eq!(
        shown(&bar, &wide)[3],
        " couldn't update: timed out · plugin restarted"
    );
    fail(&mut bar, "demo/a", SlotFailure::TimedOut, true, false);
    assert_eq!(shown(&bar, &look)[3], " stopped updating · /config");
    fail(&mut bar, "demo/a", SlotFailure::TimedOut, true, true);
    assert_eq!(
        shown(&bar, &look)[3],
        " stopped updating · /config · plugin …"
    );
    let narrow = Look::new(&bar, 23, 20);
    assert_eq!(shown(&bar, &narrow)[3], " stopped updating ·…");
    show(&mut bar, "demo/a", rows_section(1));
    assert_eq!(shown(&bar, &look), ["╭╮", " A", "├┤", " row 0", "├╯"]);
}

#[test]
fn each_failure_has_its_reason() {
    let mut bar = Sidebar::default();
    bar.catalog(&[plugin(vec![entry("demo/a", "A", 0)])]);
    let look = Look::new(&bar, 51, 20);
    assert!(shown(&bar, &look).is_empty());
    for (failure, want) in [
        (SlotFailure::TimedOut, " couldn't update: timed out"),
        (
            SlotFailure::TooLarge { bytes: 20_000 },
            " couldn't update: content too large (20 KB)",
        ),
        (
            SlotFailure::Invalid {
                rule: "tree_depth".into(),
            },
            " couldn't update: invalid content (tree_depth)",
        ),
        (SlotFailure::Unknown, " couldn't update: unknown error"),
    ] {
        fail(&mut bar, "demo/a", failure, false, false);
        assert_eq!(shown(&bar, &look), ["╭╮", " A", "├┤", want, "├╯"]);
    }
}

#[test]
fn every_unknown_draws_something_safe() {
    let bar = sidebar(vec![(
        "demo/a",
        "A",
        content(vec![
            SlotBlock::Unknown,
            SlotBlock::Line {
                spans: vec![
                    text("x", SlotTone::Unknown),
                    SlotSpan::Unknown,
                    text("y", SlotTone::Text),
                ],
            },
            SlotBlock::Tree {
                nodes: vec![node(
                    0,
                    NodeState::Unknown,
                    Some("tester"),
                    "who",
                    vec![SlotSpan::Unknown],
                )],
            },
        ]),
    )]);
    let panel = draw(&bar, &Look::new(&bar, 41, 20));
    assert_eq!(
        inside(&panel.lines),
        [
            "╭╮",
            " A",
            "├┤",
            " needs a newer Wisp",
            " xy",
            " ○ who",
            "├╯"
        ]
    );
    let who = &panel.lines[5].spans;
    assert!(
        who.iter()
            .any(|s| s.text == "○" && s.look.fg == Some(Token::TextMuted.into()))
    );
    assert!(
        who.iter()
            .any(|s| s.text == "who" && s.look.fg == Some(Token::Text.into()))
    );
    assert_eq!(panel.lines[4].spans[2].look.fg, Some(Token::Text.into()));
}

#[test]
fn elapsed_ticks_from_now_and_a_lower_bound_cost_says_so() {
    let bar = sidebar(vec![(
        "demo/a",
        "A",
        content(vec![SlotBlock::Line {
            spans: vec![
                elapsed(134, SlotTone::Muted),
                text(" · ", SlotTone::Muted),
                SlotSpan::Usd {
                    micros: 420_000,
                    at_least: true,
                    tone: SlotTone::Text,
                },
            ],
        }]),
    )]);
    assert_eq!(shown(&bar, &Look::new(&bar, 41, 20))[3], " 2m 14s · $0.42+");
    let mut look = Look::new(&bar, 41, 20);
    look.layout = Layout::new(41, GlyphSet::Ascii);
    assert_eq!(shown(&bar, &look)[3], " 2m 14s · $0.42+");
}

#[test]
fn fact_columns_drop_together_when_a_label_would_get_under_8_cells() {
    let deep = content(vec![SlotBlock::Tree {
        nodes: vec![
            node(
                0,
                NodeState::Done,
                None,
                "root",
                vec![text("done", SlotTone::Muted), usd(420_000, SlotTone::Text)],
            ),
            node(
                1,
                NodeState::Done,
                None,
                "child",
                vec![text("done", SlotTone::Muted), usd(10_000, SlotTone::Text)],
            ),
            node(
                2,
                NodeState::Done,
                None,
                "grandchild",
                vec![text("done", SlotTone::Muted)],
            ),
            node(
                3,
                NodeState::Done,
                None,
                "great-grandchild",
                vec![text("done", SlotTone::Muted)],
            ),
            node(
                4,
                NodeState::Done,
                None,
                "deepest",
                vec![text("done", SlotTone::Muted)],
            ),
        ],
    }]);
    let bar = sidebar(vec![("demo/a", "A", deep)]);
    assert_eq!(
        shown(&bar, &Look::new(&bar, 41, 20))[3..8],
        [
            " ✓ root                    done  $0.42",
            " └─✓ child                 done  $0.01",
            "   └─✓ grandchild          done",
            "     └─✓ great-grandchild  done",
            "       └─✓ deepest         done",
        ]
    );
    assert_eq!(
        shown(&bar, &Look::new(&bar, 33, 20))[3..8],
        [
            " ✓ root                   done",
            " └─✓ child                done",
            "   └─✓ grandchild         done",
            "     └─✓ great-grandchi…  done",
            "       └─✓ deepest        done",
        ]
    );
}

#[test]
fn a_line_of_only_faint_spans_steps_up_to_muted() {
    let bar = sidebar(vec![(
        "demo/a",
        "A",
        content(vec![
            SlotBlock::Line {
                spans: vec![
                    text("all", SlotTone::Faint),
                    text(" faint", SlotTone::Faint),
                ],
            },
            SlotBlock::Line {
                spans: vec![
                    text("mixed", SlotTone::Text),
                    text(" faint", SlotTone::Faint),
                ],
            },
        ]),
    )]);
    let panel = draw(&bar, &Look::new(&bar, 41, 20));
    let fg = |row: usize, text: &str| {
        panel.lines[row]
            .spans
            .iter()
            .find(|s| s.text.contains(text))
            .and_then(|s| s.look.fg)
    };
    assert_eq!(fg(3, "all"), Some(Token::TextMuted.into()));
    assert_eq!(fg(3, "faint"), Some(Token::TextMuted.into()));
    assert_eq!(fg(4, "faint"), Some(Token::TextFaint.into()));
}

const HOSTILE: &str = "a\u{1b}[31mb\u{9b}c\u{202e}d\u{200b}e\nf\tg\rh\u{85}i\u{1b}]0;t\u{7}\u{1b}P1\u{9d}\u{2028}\u{2029}\u{feff}\u{61c}\u{2066}\u{2067}\u{2068}\u{2069}\u{e0041}\u{0}\u{7f}\u{b}\u{c}\u{1c}z";

fn clean(row: &str) -> bool {
    !row.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{200b}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{061c}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{feff}'
                    | '\u{e0000}'..='\u{e007f}'
            )
    })
}

#[test]
fn hostile_strings_are_cleaned_everywhere() {
    let h = || HOSTILE.to_owned();
    let hostile = SlotContent {
        note: vec![text(HOSTILE, SlotTone::Muted)],
        blocks: vec![
            SlotBlock::Line {
                spans: vec![text(HOSTILE, SlotTone::Text)],
            },
            SlotBlock::Rows {
                rows: vec![SlotRow {
                    key: h(),
                    value: vec![text(HOSTILE, SlotTone::Text)],
                }],
            },
            SlotBlock::List {
                items: vec![vec![text(HOSTILE, SlotTone::Text)]],
            },
            SlotBlock::Gauge {
                label: h(),
                value: 1,
                max: 2,
                tone: SlotTone::Text,
                note: vec![text(HOSTILE, SlotTone::Muted)],
            },
            SlotBlock::Spark {
                label: h(),
                values: vec![1],
                max: 2,
                tone: SlotTone::Text,
                note: vec![text(HOSTILE, SlotTone::Muted)],
            },
            SlotBlock::Tree {
                nodes: vec![node(
                    0,
                    NodeState::Done,
                    None,
                    HOSTILE,
                    vec![text(HOSTILE, SlotTone::Muted)],
                )],
            },
        ],
    };
    let mut bar = Sidebar::default();
    bar.catalog(&[plugin(vec![entry(HOSTILE, HOSTILE, 0)])]);
    let name = bar.sections()[0].name.clone();
    assert!(clean(&name) && clean(&bar.sections()[0].title), "{name:?}");
    show(&mut bar, HOSTILE, hostile);
    for failure in [
        SlotFailure::Failed { message: h() },
        SlotFailure::Invalid { rule: h() },
    ] {
        fail(&mut bar, HOSTILE, failure, false, false);
        for width in [41, 163] {
            let rows = shown(&bar, &Look::new(&bar, width, 60));
            assert!(rows.len() >= 11, "{rows:?}");
            for row in &rows {
                assert!(clean(row), "{row:?}");
            }
            assert!(
                width == 41
                    || rows[5].contains(
                        "a^[[31mbM-^[c<U+202E>de f g h i^[]0;t^G^[P1M-^]<U+2028><U+2029><U+061C>\
                         <U+2066><U+2067><U+2068><U+2069><U+E0041>^@^?  ^\\z"
                    ),
                "{rows:?}"
            );
        }
    }
}

const WORDS: [&str; 10] = [
    "a",
    "agent",
    "·",
    "─",
    "世界",
    "naïve",
    "longer-word-here",
    "$0.42",
    "é",
    "x",
];

fn words(rng: &mut Rng) -> String {
    (0..rng.below(6))
        .map(|_| WORDS[rng.below(WORDS.len())])
        .collect::<Vec<_>>()
        .join(" ")
}

fn tone(rng: &mut Rng) -> SlotTone {
    [
        SlotTone::Text,
        SlotTone::Muted,
        SlotTone::Faint,
        SlotTone::Accent,
        SlotTone::Ok,
        SlotTone::Error,
    ][rng.below(6)]
}

fn spans(rng: &mut Rng) -> Vec<SlotSpan> {
    (0..rng.below(4))
        .map(|_| match rng.below(4) {
            0 => elapsed(rng.below(5000) as u64, tone(rng)),
            1 => usd(rng.below(10_000_000) as u64, tone(rng)),
            _ => text(&words(rng), tone(rng)),
        })
        .collect()
}

fn random_block(rng: &mut Rng) -> SlotBlock {
    match rng.below(9) {
        0 => SlotBlock::Line { spans: spans(rng) },
        1 => SlotBlock::Rows {
            rows: (0..rng.below(4))
                .map(|_| SlotRow {
                    key: words(rng),
                    value: spans(rng),
                })
                .collect(),
        },
        2 => SlotBlock::List {
            items: (0..rng.below(4)).map(|_| spans(rng)).collect(),
        },
        3 => {
            let max = 1 + rng.below(100) as u64;
            SlotBlock::Gauge {
                label: words(rng),
                value: rng.below(max as usize + 1) as u64,
                max,
                tone: tone(rng),
                note: spans(rng),
            }
        }
        4 => {
            let max = 1 + rng.below(100) as u64;
            SlotBlock::Spark {
                label: words(rng),
                values: (0..rng.below(120))
                    .map(|_| rng.below(max as usize + 1) as u64)
                    .collect(),
                max,
                tone: tone(rng),
                note: spans(rng),
            }
        }
        5 | 6 => {
            let mut depth = 0u8;
            SlotBlock::Tree {
                nodes: (0..rng.below(12))
                    .map(|i| {
                        depth = if i == 0 {
                            0
                        } else {
                            (rng.below(usize::from(depth) + 2) as u8).min(8)
                        };
                        let state = [
                            NodeState::Working,
                            NodeState::Waiting,
                            NodeState::Done,
                            NodeState::Failed,
                            NodeState::Stopped,
                        ][rng.below(5)];
                        let role =
                            [None, Some("main"), Some("coder"), Some("reviewer")][rng.below(4)];
                        node(depth, state, role, &words(rng), spans(rng))
                    })
                    .collect(),
            }
        }
        7 => SlotBlock::NextColumn,
        _ => SlotBlock::Unknown,
    }
}

fn random_content(rng: &mut Rng) -> SlotContent {
    SlotContent {
        note: spans(rng),
        blocks: (0..rng.below(6)).map(|_| random_block(rng)).collect(),
    }
}

fn alone(bar: &Sidebar, name: &str, look: &Look) -> usize {
    let mut solo = Look::new(bar, look.layout.width, 100_000);
    solo.layout = look.layout;
    solo.on = vec![name.to_owned()];
    solo.folded = look.folded.clone();
    bar.draw(&solo.view()).lines.len()
}

fn stack(sizes: &[usize]) -> usize {
    sizes.iter().map(|s| s - 1).sum::<usize>() + usize::from(!sizes.is_empty())
}

#[test]
fn random_panels_stay_inside_their_box_and_pin_by_the_rule() {
    let mut rng = Rng(0x5eed_cafe_f00d_1234);
    for case in 0..600 {
        let count = rng.below(6);
        let sections: Vec<(String, String)> = (0..count)
            .map(|i| (format!("demo/s{i}"), words(&mut rng)))
            .collect();
        let mut bar = Sidebar::default();
        bar.catalog(&[plugin(
            (0u32..)
                .zip(&sections)
                .map(|(i, (n, t))| entry(n, t, i))
                .collect(),
        )]);
        for (name, _) in &sections {
            if rng.chance(85) {
                show(&mut bar, name, random_content(&mut rng));
            }
            if rng.chance(15) {
                fail(
                    &mut bar,
                    name,
                    SlotFailure::Failed {
                        message: words(&mut rng),
                    },
                    rng.chance(50),
                    rng.chance(50),
                );
            }
        }
        let names = names(&bar);
        let pick = |rng: &mut Rng, pct: usize| -> Vec<String> {
            names.iter().filter(|_| rng.chance(pct)).cloned().collect()
        };
        for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide] {
            let width = 20 + rng.below(41);
            let height = 1 + rng.below(60);
            let mut look = Look::new(&bar, width, height);
            look.layout = Layout::new(width, set);
            look.on = pick(&mut rng, 85);
            look.folded = pick(&mut rng, 25);
            look.pinned = pick(&mut rng, 35);
            look.whole = rng.chance(50);
            look.at = Duration::from_millis(rng.below(10_000) as u64);
            look.position = match names.get(rng.below(names.len() + 1)) {
                Some(name) => Position::At {
                    section: name.clone(),
                    order: 0,
                    row: rng.below(40),
                },
                None => Position::Top,
            };
            let panel = draw(&bar, &look);
            let sizes = |pinned: bool| -> Vec<usize> {
                look.on
                    .iter()
                    .filter(|n| look.pinned.contains(n) == pinned)
                    .map(|n| alone(&bar, n, &look))
                    .filter(|n| *n > 0)
                    .collect()
            };
            let (p, u) = (sizes(true), sizes(false));
            let shared = usize::from(!p.is_empty() && !u.is_empty());
            let below = stack(&u) - shared;
            let laid = height - 1;
            let want = stack(&p).min((laid / 2).max(laid.saturating_sub(below)));
            assert_eq!(
                panel.pinned_rows, want,
                "case {case} {set:?} {width}x{height}"
            );
            assert_eq!(
                panel.lines.len(),
                want + below.min(laid - want),
                "case {case}"
            );
            for (row, of) in inside(&panel.lines).iter().zip(&panel.rows) {
                if of.is_none() {
                    assert!(
                        row.is_empty() || row.contains("more") || edge(row),
                        "case {case}: {row:?}"
                    );
                }
            }
            let start = look.position.clone();
            look.position = panel.position.clone();
            assert_eq!(draw(&bar, &look).lines, panel.lines, "case {case}");
            look.position = panel.scroll(&start, Step::End);
            let end = inside(&draw(&bar, &look).lines);
            if below > 0 && laid > want {
                assert!(end.last().is_some_and(|r| edge(r)), "case {case}: {end:?}");
                let down = format!("{} ", down_arrow(&Glyphs::of(set)));
                assert!(!end.iter().any(|r| r.contains(&down) && r.contains("more")));
            }
            look.position = panel.scroll(&start, Step::Down(rng.below(30)));
            draw(&bar, &look);
        }
    }
}

fn mixed(ink: Option<Ink>) -> f64 {
    match ink {
        Some(Ink::Mix { u, .. }) => (u.get() * 1000.0).round() / 1000.0,
        Some(Ink::Token(_)) => 1.0,
        None => -1.0,
    }
}

#[test]
fn the_pulse_frames_follow_the_brand() {
    let at = |ms: u64, cx: MotionCtx| {
        mixed(Some(tree_pulse(
            Token::RoleCoder,
            Duration::from_millis(ms),
            cx,
        )))
    };
    let frames: Vec<f64> = [0, 500, 1000, 1049, 1500, 2000]
        .iter()
        .map(|ms| at(*ms, FULL))
        .collect();
    assert_eq!(frames, [1.0, 0.8, 0.6, 0.6, 0.8, 1.0]);
    let calm = MotionCtx {
        policy: MotionPolicy::Calm,
        ..FULL
    };
    assert_eq!(at(2000, calm), 0.8);
    assert_eq!(at(1000, calm), 0.9);
    match tree_pulse(Token::RoleCoder, Duration::from_millis(1000), FULL) {
        Ink::Mix { still, .. } => assert_eq!(still, Token::RoleCoder),
        other => panic!("{other:?}"),
    }
    let still = |cx: MotionCtx| tree_pulse(Token::RoleCoder, Duration::from_millis(1000), cx);
    assert_eq!(
        still(MotionCtx {
            policy: MotionPolicy::Reduced,
            ..FULL
        }),
        Ink::Token(Token::RoleCoder)
    );
    assert_eq!(
        still(MotionCtx {
            blend: false,
            ..FULL
        }),
        Ink::Token(Token::RoleCoder)
    );
}

#[test]
fn the_tree_pulses_in_full_motion_and_is_still_under_reduced_and_ansi16() {
    let bar = sidebar(vec![("agent-viz/tree", "Agents", agents())]);
    let frame = |ms: u64, cx: MotionCtx| {
        let mut look = Look::new(&bar, 41, 20);
        look.at = Duration::from_millis(ms);
        look.cx = cx;
        draw(&bar, &look).lines
    };
    assert_ne!(frame(0, FULL), frame(1000, FULL));
    for cx in [
        MotionCtx {
            policy: MotionPolicy::Reduced,
            ..FULL
        },
        MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::Ansi16),
    ] {
        assert_eq!(frame(0, cx), frame(1000, cx));
        assert_eq!(frame(0, cx)[3].spans[2].look.fg, Some(Token::Accent.into()));
    }
}

#[test]
fn the_meter_and_the_spark_draw_the_brands_levels() {
    let unicode = Glyphs::of(GlyphSet::Unicode);
    let cells = |value: u64| -> String {
        let bar = cell_bar(value, 100, 8, Token::Accent.into(), &unicode);
        bar.into_iter().map(|s| s.text).collect()
    };
    assert_eq!(cells(94), "⣿⣿⣿⣿⣿⣿⣿⣀");
    assert_eq!(cells(64), "⣿⣿⣿⣿⣿⣀⣀⣀");
    assert_eq!(blocks::spark_text(&[0, 3, 6, 9], 9, &unicode), "⣠⣾");
    assert_eq!(blocks::spark_text(&[9, 0, 9], 9, &unicode), "⢸⣸");
    let ascii = Glyphs::of(GlyphSet::Ascii);
    assert_eq!(
        blocks::spark_text(&[0, 1, 2, 3, 4, 5, 6], 6, &ascii),
        ".:-=+*#"
    );
    assert_eq!(width("⣠⣾", WidthMode::AmbiguousWide), 2);
}

#[test]
fn a_spark_keeps_its_newest_values_and_a_gauge_keeps_four_meter_cells() {
    let values: Vec<u64> = (0..40).collect();
    let bar = sidebar(vec![(
        "demo/a",
        "A",
        content(vec![
            SlotBlock::Spark {
                label: "t".into(),
                values: values.clone(),
                max: 39,
                tone: SlotTone::Faint,
                note: vec![],
            },
            SlotBlock::Gauge {
                label: "a label far too long for the row".into(),
                value: 995,
                max: 1000,
                tone: SlotTone::Faint,
                note: vec![text("note", SlotTone::Muted)],
            },
        ]),
    )]);
    let panel = draw(&bar, &Look::new(&bar, 23, 20));
    let rows = inside(&panel.lines);
    let unicode = Glyphs::of(GlyphSet::Unicode);
    assert_eq!(
        rows[3],
        format!(" t  {}", blocks::spark_text(&values[8..], 39, &unicode))
    );
    assert_eq!(rows[4], " a label…  ⣿⣿⣿⣀  99%");
    for row in &panel.lines[3..5] {
        for span in row
            .spans
            .iter()
            .filter(|s| s.text.contains(['⣿', '⣀', '%']))
        {
            let free = span.text.chars().all(|c| c == '⣀');
            let token = if free { SPACE_TOKEN } else { Token::TextMuted };
            assert_eq!(span.look.fg, Some(token.into()), "{span:?}");
        }
    }
}

#[test]
fn an_empty_line_is_not_drawable() {
    let bar = sidebar(vec![
        (
            "demo/a",
            "A",
            content(vec![
                SlotBlock::Line { spans: vec![] },
                SlotBlock::Line {
                    spans: vec![SlotSpan::Unknown, text("", SlotTone::Text)],
                },
            ]),
        ),
        ("demo/b", "B", rows_section(1)),
    ]);
    assert_eq!(
        shown(&bar, &Look::new(&bar, 41, 20)),
        ["╭╮", " B", "├┤", " row 0", "├╯"]
    );
}

#[test]
fn spare_pinned_rows_go_below_the_dropped_sections_note() {
    let bar = sidebar(vec![
        ("demo/a", "A", rows_section(1)),
        ("demo/b", "B", rows_section(9)),
        ("demo/c", "C", rows_section(9)),
    ]);
    let mut look = Look::new(&bar, 41, 7);
    look.pinned = owned(&["demo/a", "demo/b"]);
    let panel = draw(&bar, &look);
    assert_eq!(panel.pinned_rows, 3);
    assert_eq!(
        inside(&panel.lines)[..4],
        [" … 2 more sections", "", "├╮", " C"]
    );
    look.height = 13;
    let panel = draw(&bar, &look);
    assert_eq!(panel.pinned_rows, 6);
    assert_eq!(
        inside(&panel.lines)[..7],
        [
            "╭╮",
            " A",
            " … 1 more",
            "├╯",
            " … 1 more section",
            "├╮",
            " C"
        ]
    );
}

mod fill;
mod page;
