use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::settings::names::{DISPLAY_SIDEBAR_FOLDED, DISPLAY_SIDEBAR_PINNED};
use wisp_protocol::slot::{
    NodeState, SlotBlock, SlotContent, SlotFailure, SlotNode, SlotNoun, SlotRow, SlotSpan,
    SlotTone, SlotTopologyNode, SlotView,
};
use wisp_protocol::{AgentEvent, AgentState, ClientCommand};
use wisp_render::motion::MotionPolicy;
use wisp_render::theme::{Depth, Mode, Theme, Token};
use wisp_tui::app::sidebar::AGENTS_SECTION;
use wisp_tui::term::{Env, Input, KeyCode, MouseKind, ProbeReport, SavedDisplay};

use super::agents::{entry, spawned};
use super::folds::{click, list_set, with_saved};
use super::select::{Fake, locate, tinted};
use super::sidebar::{
    NOTES, ROWS, TREE, both, content, demo, drain, emptied, finish, kept_options,
    kept_options_with, mouse, names, no_rule, notes, options_in, quit, resized, rule_at, shot,
    slot, snapshot, stage, switched, talk, text, tree, viz,
};
use super::steering::{Agent, steering_on};
use super::{Screen, fixed_now};
use crate::pick::pick;
use crate::shared::{catalog, plugin};
use crate::{Drive, shows};

pub(super) fn now_ms() -> u64 {
    u64::try_from(fixed_now().as_millisecond()).unwrap()
}

fn page(section: &str, blocks: Vec<SlotBlock>, note: Vec<SlotSpan>) -> AgentEvent {
    AgentEvent::SlotContent {
        section: section.to_owned(),
        view: SlotView::Page,
        content: SlotContent { note, blocks },
    }
}

fn muted(words: &str) -> SlotSpan {
    SlotSpan::Text {
        text: words.to_owned(),
        tone: SlotTone::Muted,
        bold: false,
    }
}

fn ago(secs: u64) -> SlotSpan {
    SlotSpan::Elapsed {
        since_ms: now_ms() - secs * 1000,
        tone: SlotTone::Muted,
    }
}

fn usd(micros: u64) -> SlotSpan {
    SlotSpan::Usd {
        micros,
        at_least: false,
        tone: SlotTone::Text,
    }
}

fn board_node(
    depth: u8,
    state: NodeState,
    role: &str,
    label: &str,
    facts: Vec<SlotSpan>,
) -> SlotNode {
    SlotNode {
        depth,
        state,
        role: Some(role.to_owned()),
        label: label.to_owned(),
        facts,
    }
}

fn board() -> AgentEvent {
    use NodeState::*;
    let nodes = vec![
        board_node(
            0,
            Working,
            "main",
            "main",
            vec![muted("main"), ago(723), usd(420_000)],
        ),
        board_node(
            1,
            Working,
            "coordinator",
            "cache-fingerprints",
            vec![muted("coordinator"), ago(134), usd(310_000)],
        ),
        board_node(
            2,
            Working,
            "researcher",
            "researcher",
            vec![muted("researcher"), ago(41), usd(40_000)],
        ),
        board_node(
            2,
            Waiting,
            "coder",
            "coder",
            vec![muted("coder"), muted("held"), usd(120_000)],
        ),
        board_node(
            2,
            Waiting,
            "reviewer",
            "reviewer",
            vec![muted("reviewer"), muted("queued")],
        ),
        board_node(
            1,
            Done,
            "coordinator",
            "tui-research",
            vec![muted("coordinator"), muted("done"), usd(200_000)],
        ),
        board_node(
            2,
            Done,
            "researcher",
            "researcher",
            vec![muted("researcher"), muted("done"), usd(30_000)],
        ),
        board_node(
            2,
            Done,
            "designer",
            "designer",
            vec![muted("designer"), muted("done"), usd(50_000)],
        ),
        board_node(
            2,
            Done,
            "coder",
            "coder",
            vec![muted("coder"), muted("done"), usd(50_000)],
        ),
        board_node(
            2,
            Done,
            "reviewer",
            "reviewer",
            vec![muted("reviewer"), muted("done"), usd(20_000)],
        ),
    ];
    let count = |key: &str, n: &str| SlotRow {
        key: key.to_owned(),
        value: vec![text(n)],
    };
    page(
        TREE,
        vec![
            SlotBlock::Tree { nodes },
            SlotBlock::NextColumn,
            SlotBlock::Spark {
                label: "spend".to_owned(),
                values: vec![
                    1, 1, 1, 2, 2, 3, 3, 3, 4, 4, 5, 5, 5, 5, 6, 6, 6, 7, 7, 7, 7, 7, 8, 8, 8, 8,
                    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
                ],
                max: 8,
                tone: SlotTone::Accent,
                note: vec![usd(1_240_000)],
            },
            SlotBlock::Rows {
                rows: vec![
                    count("working", "3"),
                    count("waiting", "2"),
                    count("done", "5"),
                ],
            },
        ],
        vec![
            muted("3 working \u{b7} 2 waiting \u{b7} 5 done \u{b7} "),
            usd(1_240_000),
        ],
    )
}

fn keyed(row: &str) -> bool {
    row.trim_end().ends_with("q esc back") && row.trim_start().starts_with("/ search")
}

fn page_open(lines: &[String], title: &str) -> bool {
    lines
        .first()
        .is_some_and(|l| l.trim_start().starts_with(title))
        && lines
            .get(1)
            .is_some_and(|l| l.trim().starts_with('\u{2500}'))
        && keyed(&super::status_line(lines))
}

fn pages_sent(commands: &[ClientCommand]) -> Vec<(Vec<String>, Vec<String>)> {
    pick!(
        commands,
        ClientCommand::SlotsShown { sections, pages } => (sections.clone(), pages.clone())
    )
}

async fn clicked(a: &Agent, needle: &str) {
    sleep(Duration::from_millis(450)).await;
    let (column, row) = locate(&a.screen, needle);
    click(a, column + 1, row);
}

async fn clicked_at(a: &Agent, column: u16, row: u16) {
    sleep(Duration::from_millis(450)).await;
    click(a, column, row);
}

pub(crate) async fn docked(a: &mut Agent) {
    a.emit(both()).await;
    a.emit(tree()).await;
    a.emit(notes(4)).await;
    a.settle().await;
    drain(a).await;
}

#[tokio::test(start_paused = true)]
async fn a_body_click_opens_the_page_from_the_docked_panel_and_esc_returns_to_it() {
    stage(160, |mut a| async move {
        talk(&mut a, 6).await;
        docked(&mut a).await;
        let before = a.settle().await;
        clicked(&a, "note 2").await;
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        assert!(shows(&lines, "note 4"), "{lines:#?}");
        let sent = pages_sent(&drain(&mut a).await);
        assert_eq!(sent, vec![(names(&[TREE, NOTES]), names(&[NOTES]))]);
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 119), "{lines:#?}");
        assert_eq!(lines, before);
        let sent = pages_sent(&drain(&mut a).await);
        assert_eq!(sent, vec![(names(&[TREE, NOTES]), vec![])]);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_body_click_in_the_overlay_opens_the_page_and_esc_returns_to_the_overlay() {
    stage(80, |mut a| async move {
        docked(&mut a).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 41), "{lines:#?}");
        drain(&mut a).await;
        clicked(&a, "coder").await;
        let lines = a.settle().await;
        assert!(page_open(&lines, "Agents"), "{lines:#?}");
        let sent = pages_sent(&drain(&mut a).await);
        assert_eq!(sent, vec![(names(&[TREE, NOTES]), names(&[TREE]))]);
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 41), "{lines:#?}");
        assert!(shows(&lines, "scroll \u{b7} esc close"), "{lines:#?}");
        assert_eq!(
            pages_sent(&drain(&mut a).await),
            vec![(names(&[TREE, NOTES]), vec![])]
        );
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_failure_row_opens_the_page_and_a_title_click_still_folds() {
    stage(160, |mut a| async move {
        docked(&mut a).await;
        a.emit(AgentEvent::SlotFailed {
            section: NOTES.to_owned(),
            view: SlotView::Panel,
            failure: SlotFailure::TimedOut,
            parked: false,
            restarted: false,
        })
        .await;
        a.settle().await;
        clicked(&a, "couldn't update").await;
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        assert!(shows(&lines, "couldn't update: timed out"), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.settle().await;
        clicked(&a, " Agents").await;
        let lines = a.settle().await;
        assert!(!page_open(&lines, "Agents"), "{lines:#?}");
        assert!(shows(&lines, "Agents \u{25b8}"), "{lines:#?}");
        clicked(&a, "Agents \u{25b8}").await;
        let lines = a.settle().await;
        assert!(!page_open(&lines, "Agents"), "{lines:#?}");
        assert!(!shows(&lines, "Agents \u{25b8}"), "{lines:#?}");
        assert!(shows(&lines, "coder"), "{lines:#?}");
        let sent = drain(&mut a).await;
        assert!(
            pages_sent(&sent)
                .iter()
                .all(|(_, pages)| pages.is_empty() || pages == &names(&[NOTES])),
            "{sent:?}"
        );
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn more_rows_edges_and_dropped_sections_open_nothing_but_a_cut_does() {
    let options = with_saved(SavedDisplay::from_pairs([]));
    steering_on(Screen::new(13, 160), options, |mut a| async move {
        a.emit(catalog(vec![
            viz(true),
            demo(true),
            plugin("aaa", true, vec![slot("aaa/log", "Log")]),
        ]))
        .await;
        a.emit(tree()).await;
        a.emit(notes(30)).await;
        a.emit(content(
            "aaa/log",
            vec![SlotBlock::List {
                items: (1..=10)
                    .map(|i| vec![text(&format!("entry {i}"))])
                    .collect(),
            }],
        ))
        .await;
        a.emit(list_set(DISPLAY_SIDEBAR_PINNED, &[TREE, NOTES, "aaa/log"]))
            .await;
        let lines = a.settle().await;
        let panel: Vec<String> = lines
            .iter()
            .map(|l| l.chars().skip(120).collect())
            .collect();
        assert!(shows(&panel, "more section"), "{panel:#?}");
        let cut = panel
            .iter()
            .position(|l| l.contains("more") && !l.contains("section"))
            .unwrap_or_else(|| panic!("{panel:#?}"));
        let dropped = panel
            .iter()
            .position(|l| l.contains("more section"))
            .unwrap();
        let edge = panel
            .iter()
            .position(|l| l.starts_with('\u{2500}'))
            .unwrap_or_else(|| panic!("{panel:#?}"));
        for row in [dropped, edge] {
            clicked_at(&a, 124, u16::try_from(row).unwrap()).await;
            let lines = a.settle().await;
            assert!(!keyed(&super::status_line(&lines)), "row {row}: {lines:#?}");
        }
        drain(&mut a).await;
        clicked_at(&a, 124, u16::try_from(cut).unwrap()).await;
        let lines = a.settle().await;
        assert!(page_open(&lines, "Log"), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.settle().await;
        a.emit(list_set(DISPLAY_SIDEBAR_PINNED, &[TREE])).await;
        a.emit(notes(40)).await;
        let lines = a.settle().await;
        let panel: Vec<String> = lines
            .iter()
            .map(|l| l.chars().skip(120).collect())
            .collect();
        let edge = panel
            .iter()
            .position(|l| l.starts_with('\u{2500}'))
            .unwrap_or_else(|| panic!("{panel:#?}"));
        let down = panel
            .iter()
            .position(|l| l.contains('\u{2193}'))
            .unwrap_or_else(|| panic!("{panel:#?}"));
        for row in [edge, down] {
            clicked_at(&a, 124, u16::try_from(row).unwrap()).await;
            let lines = a.settle().await;
            assert!(!keyed(&super::status_line(&lines)), "row {row}: {lines:#?}");
        }
        for _ in 0..3 {
            mouse(&a, MouseKind::WheelDown, 140, u16::try_from(down).unwrap());
        }
        let lines = a.settle().await;
        let panel: Vec<String> = lines
            .iter()
            .map(|l| l.chars().skip(120).collect())
            .collect();
        let up = panel
            .iter()
            .position(|l| l.contains('\u{2191}'))
            .unwrap_or_else(|| panic!("{panel:#?}"));
        clicked_at(&a, 124, u16::try_from(up).unwrap()).await;
        let lines = a.settle().await;
        assert!(!keyed(&super::status_line(&lines)), "{lines:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn sidebar_with_a_section_opens_its_page_at_160_and_at_80() {
    for cols in [160, 80] {
        stage(cols, |mut a| async move {
            docked(&mut a).await;
            a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(page_open(&lines, "Notes"), "{cols}: {lines:#?}");
            let sent = pages_sent(&drain(&mut a).await);
            assert_eq!(sent.last().map(|s| s.1.clone()), Some(names(&[NOTES])));
            a.key(KeyCode::Char('q'));
            let lines = a.settle().await;
            assert!(!keyed(&super::status_line(&lines)), "{lines:#?}");
            quit(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_body_click_on_the_agents_box_docked_or_in_the_overlay_or_sidebar_with_its_name_opens_agents()
 {
    for cols in [160, 80] {
        stage(cols, |mut a| async move {
            let agents = plugin("agent-viz", true, vec![slot(AGENTS_SECTION, "Agents")]);
            a.emit(catalog(vec![agents, demo(true)])).await;
            let coder = SlotTopologyNode {
                key: "/coder".into(),
                parent: None,
                state: NodeState::Working,
            };
            let topology = SlotBlock::Topology {
                root: Some("main".into()),
                nodes: vec![coder],
                flows: Vec::new(),
                counted: Some(SlotNoun::Agents),
            };
            a.emit(content(AGENTS_SECTION, vec![topology])).await;
            a.emit(notes(4)).await;
            let coder = super::agents::entry("/coder", wisp_protocol::AgentState::Running, 0.0);
            a.emit(super::agents::spawned(coder)).await;
            a.settle().await;
            if cols < 120 {
                a.paste("/sidebar").key(KeyCode::Enter);
                let lines = a.settle().await;
                assert!(rule_at(&a.screen, 41), "{lines:#?}");
            }
            let lines = a.settle().await;
            let title = lines.iter().find(|l| l.contains("│ Agents"));
            assert!(
                title.is_some_and(|l| l.contains("1 agent · 1 working")),
                "{cols}: {lines:#?}"
            );
            drain(&mut a).await;
            clicked(&a, "● working").await;
            let lines = a.settle().await;
            assert!(shows(&lines, "enter open"), "{cols}: {lines:#?}");
            assert!(!keyed(&super::status_line(&lines)), "{cols}: {lines:#?}");
            assert!(!shows(&lines, "scroll"), "{cols}: {lines:#?}");
            if cols < 120 {
                assert!(no_rule(&a.screen), "{lines:#?}");
            }
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert!(!shows(&lines, "enter open"), "{cols}: {lines:#?}");
            a.paste(&format!("/sidebar {AGENTS_SECTION}"))
                .key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(shows(&lines, "enter open"), "{cols}: {lines:#?}");
            let sent = pages_sent(&drain(&mut a).await);
            let paged = sent
                .last()
                .is_some_and(|(_, pages)| pages == &[AGENTS_SECTION]);
            assert!(paged, "{sent:?}");
            a.key(KeyCode::Esc);
            quit(&a).await;
        })
        .await;
    }
}

fn tiles_at(sent: &str) -> Vec<(u16, u16)> {
    let parts: Vec<&str> = sent.split("\x1b_Ga=T,").collect();
    parts
        .windows(2)
        .filter_map(|w| {
            let at = w[0].rsplit_once("\x1b[")?.1.strip_suffix('H')?;
            let (row, column) = at.split_once(';')?;
            Some((
                column.parse::<u16>().ok()? - 1,
                row.parse::<u16>().ok()? - 1,
            ))
        })
        .collect()
}

async fn one_working_agent(a: &Agent) {
    working_agents(a, ["/coder".into()]).await;
}

async fn working_agents(a: &Agent, keys: impl IntoIterator<Item = String>) {
    let agents = plugin("agent-viz", true, vec![slot(AGENTS_SECTION, "Agents")]);
    a.emit(catalog(vec![agents])).await;
    let nodes = keys
        .into_iter()
        .map(|key| SlotTopologyNode {
            key,
            parent: None,
            state: NodeState::Working,
        })
        .collect();
    let topology = SlotBlock::Topology {
        root: Some("main".into()),
        nodes,
        flows: Vec::new(),
        counted: Some(SlotNoun::Agents),
    };
    a.emit(content(AGENTS_SECTION, vec![topology])).await;
}

#[tokio::test]
async fn windows_terminal_gets_sixel_tiles_on_the_box_s_own_background() {
    for (cols, ground) in [(160, Token::Bg), (80, Token::Surface)] {
        let mut kept = super::sidebar::kept();
        kept.env = Env::from_pairs([
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("LANG", "en_US.UTF-8"),
            ("WISP_MOTION", "reduced"),
            ("WT_SESSION", "1"),
        ]);
        let probe = ProbeReport {
            device_attributes: Some(vec![61, 4]),
            cell_pixels: Some((10, 20)),
            ..super::reported()
        };
        let theme = Theme::new(Mode::Dark, Depth::TrueColor).with_background(probe.background);
        kept.probe = Some(probe);
        steering_on(
            Screen::new(ROWS, cols),
            kept_options_with(kept),
            |a| async move {
                one_working_agent(&a).await;
                a.settle().await;
                if cols < 120 {
                    a.paste("/sidebar").key(KeyCode::Enter);
                    a.settle().await;
                }
                let sent = String::from_utf8_lossy(&a.screen.bodies_since(0).concat()).into_owned();
                let last = sent.rsplit("\x1bP0;1q").next().unwrap_or_default();
                let rgb = theme.rgb(ground);
                let percent = |v: u8| (u16::from(v) * 100 + 127) / 255;
                let (r, g, b) = (percent(rgb.r), percent(rgb.g), percent(rgb.b));
                assert!(
                    sent.contains("\x1bP0;1q") && last.contains(&format!("#0;2;{r};{g};{b}#")),
                    "{cols}: {last:?}"
                );
                quit(&a).await;
            },
        )
        .await;
    }
}

#[tokio::test]
async fn the_agents_box_is_a_picture_until_the_terminal_refuses_one() {
    let mut kept = super::sidebar::kept();
    kept.probe = Some(ProbeReport {
        graphics: Some(true),
        cell_pixels: Some((9, 19)),
        ..super::reported()
    });
    let options = kept_options_with(kept);
    steering_on(Screen::new(ROWS, 160), options, |a| async move {
        one_working_agent(&a).await;
        let lines = a.settle().await;
        let sent =
            |from| String::from_utf8_lossy(&a.screen.bodies_since(from).concat()).into_owned();
        assert!(
            sent(0).contains("\x1b_Ga=T,f=32,o=z,s=18,v=19,"),
            "{lines:#?}"
        );
        assert!(!shows(&lines, "\u{25cf}"), "{lines:#?}");
        let tiles = tiles_at(&sent(0));
        let from = a.screen.frames();
        for i in 0..14 {
            a.emit(spawned(entry(&format!("/r{i}"), AgentState::Running, 0.4)))
                .await;
        }
        a.ctrl('t');
        a.settle().await;
        let legend = locate(&a.screen, "   main    working").1;
        let page = tiles_at(&sent(from));
        assert!(page.iter().any(|t| t.1 > 0 && t.1 < legend), "{page:?}");
        assert!(page.iter().any(|t| t.1 == legend), "{page:?}");
        assert!(page.iter().all(|t| !tiles.contains(t)), "{page:?}");
        a.key(KeyCode::Esc);
        a.settle().await;
        let from = a.screen.frames();
        a.input(Input::PictureRefused);
        let lines = a.settle().await;
        assert!(sent(from).contains("\x1b_Ga=d,d=I,"), "{lines:#?}");
        assert!(shows(&lines, "\u{25cf} working"), "{lines:#?}");
        assert!(
            tiles.contains(&locate(&a.screen, "\u{2836} main")),
            "{tiles:?}"
        );
        a.emit(super::sidebar::sidebar_set("auto")).await;
        a.settle().await;
        assert!(!sent(from).contains("a=T"));
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_list_walks_with_up_and_down_and_a_click_opens_its_row() {
    let options = options_in(Mode::Dark);
    steering_on(Screen::new(24, 80), options, |mut a| async move {
        let keys = (0..20).map(|i| format!("/lane-{i:02}"));
        for key in keys.clone() {
            a.emit(spawned(entry(&key, AgentState::Running, 0.4))).await;
        }
        working_agents(&a, keys).await;
        a.ctrl('t').key(KeyCode::Down);
        let lines = a.shows("● lane-00  opus-5-5 high").await;
        let (bracket, row) = locate(&a.screen, "[●]");
        let right = lines[usize::from(row)]
            .chars()
            .enumerate()
            .skip(usize::from(bracket) + 3)
            .find(|&(_, c)| c == '●')
            .unwrap()
            .0;
        a.key(KeyCode::Right);
        a.shows("● lane-01  opus-5-5 high").await;
        assert_eq!(
            locate(&a.screen, "[●]"),
            (u16::try_from(right - 1).unwrap(), row)
        );
        for i in 2..20 {
            a.key(KeyCode::Down);
            a.shows(&format!("● lane-{i:02}  opus-5-5 high")).await;
        }
        a.key(KeyCode::Up);
        a.shows("● lane-18  opus-5-5 high").await;
        let (col, row) = locate(&a.screen, "● lane-19");
        clicked_at(&a, col + 2, row).await;
        a.shows("esc back").await;
        assert!(
            drain(&mut a)
                .await
                .contains(&super::agent_keys::watch("/lane-19", true))
        );
        a.key(KeyCode::Esc);
        quit(&a).await;
    })
    .await;
}

#[tokio::test]
async fn a_chat_opened_from_the_page_leaves_no_page_tile_and_no_dot_to_click() {
    let mut kept = super::sidebar::kept();
    kept.probe = Some(ProbeReport {
        graphics: Some(true),
        cell_pixels: Some((9, 19)),
        ..super::reported()
    });
    let options = kept_options_with(kept);
    steering_on(Screen::new(ROWS, 160), options, |mut a| async move {
        one_working_agent(&a).await;
        a.emit(spawned(entry("/coder", AgentState::Running, 0.4)))
            .await;
        a.settle().await;
        let sent =
            |from| String::from_utf8_lossy(&a.screen.bodies_since(from).concat()).into_owned();
        let from = a.screen.frames();
        a.ctrl('t');
        a.settle().await;
        a.key(KeyCode::Down);
        a.settle().await;
        let page = tiles_at(&sent(from));
        assert!(!page.is_empty());
        let from = a.screen.frames();
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(shows(&lines, "esc back"), "{lines:#?}");
        let later = tiles_at(&sent(from));
        assert!(later.iter().all(|t| !page.contains(t)), "{later:?}");
        a.key(KeyCode::Esc);
        a.ctrl('t');
        a.input(Input::PictureRefused);
        a.settle().await;
        drain(&mut a).await;
        a.key(KeyCode::Down);
        a.settle().await;
        let (left, _) = locate(&a.screen, "● coder");
        let (bracket, row) = locate(&a.screen, "[●]");
        let column = bracket + 1;
        assert!(column > left + 7);
        clicked_at(&a, column, row).await;
        assert!(
            drain(&mut a)
                .await
                .contains(&super::agent_keys::watch("/coder", true))
        );
        clicked_at(&a, column, row).await;
        let lines = a.settle().await;
        assert!(shows(&lines, "esc back"), "{lines:#?}");
        let sent = drain(&mut a).await;
        assert!(
            !sent
                .iter()
                .any(|c| matches!(c, ClientCommand::WatchAgent { .. })),
            "{sent:?}"
        );
        a.key(KeyCode::Esc);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn sidebar_names_an_off_or_unknown_section_and_only_echoes_a_name_with_the_grammar() {
    steering_on(Screen::new(ROWS, 160), kept_options(), |mut a| async move {
        docked(&mut a).await;
        a.emit(switched(NOTES, false)).await;
        a.settle().await;
        drain(&mut a).await;
        for (typed, said) in [
            (NOTES, "demo/notes is off \u{b7} /config"),
            ("demo/gone", "No sidebar section named demo/gone"),
            ("sk-ant-api03-xyz", "No sidebar section by that name"),
        ] {
            a.paste(&format!("/sidebar {typed}")).key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(shows(&lines, said), "{typed}: {lines:#?}");
            assert!(!keyed(&super::status_line(&lines)), "{lines:#?}");
            if typed.starts_with("sk-") {
                assert!(!shows(&lines, typed), "{lines:#?}");
            }
        }
        a.paste("/sidebar open extra").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(
            shows(&lines, "/sidebar takes open, a section, or nothing"),
            "{lines:#?}"
        );
        assert!(pages_sent(&drain(&mut a).await).is_empty());
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn sidebar_completes_open_and_the_sections_that_are_on() {
    steering_on(Screen::new(ROWS, 160), kept_options(), |mut a| async move {
        docked(&mut a).await;
        a.emit(switched(NOTES, false)).await;
        a.settle().await;
        a.paste("/sidebar ");
        let lines = a.settle().await;
        assert!(shows(&lines, "Open it over the transcript"), "{lines:#?}");
        let row = lines
            .iter()
            .find(|l| l.contains(TREE))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(row.contains("Agents"), "{row}");
        assert!(!shows(&lines, NOTES), "{lines:#?}");
        a.paste("ag").key(KeyCode::Tab);
        let lines = a.settle().await;
        assert!(shows(&lines, &format!("/sidebar {TREE}")), "{lines:#?}");
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(page_open(&lines, "Agents"), "{lines:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

fn long_notes(count: usize) -> AgentEvent {
    page(
        NOTES,
        vec![SlotBlock::List {
            items: (1..=count)
                .map(|i| vec![text(&format!("page note {i}"))])
                .collect(),
        }],
        vec![muted("the note")],
    )
}

#[tokio::test(start_paused = true)]
async fn the_page_scrolls_and_searches_with_the_readers_keys_and_keeps_its_title_and_top_row() {
    stage(160, |mut a| async move {
        docked(&mut a).await;
        a.emit(long_notes(80)).await;
        a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
        a.settle().await;
        drain(&mut a).await;
        a.key(KeyCode::Char('j')).key(KeyCode::Char('j'));
        a.key(KeyCode::PageDown);
        let lines = a.settle().await;
        assert!(lines[0].trim_start().starts_with("Notes"), "{lines:#?}");
        assert!(lines[0].trim_end().ends_with("the note"), "{lines:#?}");
        let top = lines[2].clone();
        assert!(!top.contains("page note 1 "), "{lines:#?}");
        let keys = super::status_line(&lines);
        for absent in ["ctrl-o", "s screen", "G follow", "[ to scrollback"] {
            assert!(!keys.contains(absent), "{keys}");
        }
        a.emit(long_notes(90)).await;
        let lines = a.settle().await;
        assert_eq!(lines[2], top, "{lines:#?}");
        assert!(super::status_line(&lines).contains("/90"), "{lines:#?}");
        a.key(KeyCode::Char('/'));
        a.paste("page note 77").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(shows(&lines, "page note 77"), "{lines:#?}");
        a.key(KeyCode::Char('g'));
        let lines = a.settle().await;
        assert!(lines[2].contains("page note 1"), "{lines:#?}");
        a.ctrl('o');
        a.key(KeyCode::Char('['));
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        let sent = drain(&mut a).await;
        assert!(pages_sent(&sent).is_empty(), "{sent:?}");
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_selection_on_the_page_copies() {
    let fake = Fake::new("section-page");
    let mut options = options_in(Mode::Dark);
    options.clipboard = fake.clipboard();
    options.copy_on_release = false;
    let fake = &fake;
    steering_on(Screen::new(ROWS, 160), options, |mut a| async move {
        docked(&mut a).await;
        a.emit(long_notes(5)).await;
        a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
        a.settle().await;
        let (col, row) = locate(&a.screen, "page note 2");
        mouse(&a, MouseKind::Press, col, row);
        mouse(&a, MouseKind::Drag, col + 4, row);
        mouse(&a, MouseKind::Drag, col + 10, row);
        mouse(&a, MouseKind::Release, col + 10, row);
        a.settle().await;
        assert!(!tinted(&a.screen).is_empty(), "nothing selected");
        a.ctrl('c');
        super::eventually(|| !fake.copies().is_empty()).await;
        let copies = fake.copies();
        assert_eq!(copies.len(), 1, "{copies:?}");
        assert!(copies[0].starts_with("page note"), "{copies:?}");
        drain(&mut a).await;
        a.key(KeyCode::Esc);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn before_page_content_the_page_draws_the_panels_then_the_pages() {
    stage(160, |mut a| async move {
        docked(&mut a).await;
        a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(shows(&lines, "note 3"), "{lines:#?}");
        a.emit(long_notes(2)).await;
        let lines = a.settle().await;
        assert!(shows(&lines, "page note 2"), "{lines:#?}");
        assert!(!shows(&lines, " note 3"), "{lines:#?}");
        a.emit(notes(9)).await;
        let lines = a.settle().await;
        assert!(!shows(&lines, "note 9"), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.emit(emptied(NOTES)).await;
        a.emit(page(NOTES, vec![], vec![])).await;
        a.settle().await;
        a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(shows(&lines, "Nothing to show yet"), "{lines:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn switching_the_section_off_or_disabling_or_removing_its_plugin_closes_the_page() {
    let off: Vec<AgentEvent> = vec![switched(NOTES, false)];
    let disabled = vec![catalog(vec![viz(true), demo(false)])];
    let removed = vec![catalog(vec![viz(true)])];
    for events in [off, disabled, removed] {
        steering_on(Screen::new(ROWS, 160), kept_options(), |mut a| async move {
            docked(&mut a).await;
            a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(page_open(&lines, "Notes"), "{lines:#?}");
            drain(&mut a).await;
            for event in events {
                a.emit(event).await;
            }
            let lines = a.settle().await;
            assert!(!keyed(&super::status_line(&lines)), "{lines:#?}");
            assert!(rule_at(&a.screen, 119), "{lines:#?}");
            let sent = pages_sent(&drain(&mut a).await);
            assert_eq!(sent.last(), Some(&(names(&[TREE]), vec![])), "{sent:?}");
            quit(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_resize_across_120_under_an_open_page_closes_back_to_the_right_state() {
    stage(160, |mut a| async move {
        docked(&mut a).await;
        a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
        a.settle().await;
        resized(&a, 100);
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        assert!(!shows(&lines, "esc close"), "{lines:#?}");
        a.paste("/sidebar").key(KeyCode::Enter);
        a.settle().await;
        assert!(rule_at(&a.screen, 61));
        clicked(&a, "note 1").await;
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        resized(&a, 160);
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 119), "{lines:#?}");
        assert!(!shows(&lines, "esc close"), "{lines:#?}");
        let sent = pages_sent(&drain(&mut a).await);
        assert_eq!(
            sent.last(),
            Some(&(names(&[TREE, NOTES]), vec![])),
            "{sent:?}"
        );
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_open_pages_clocks_step_each_second_with_no_new_content() {
    static CLOCK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    fn moving() -> jiff::Timestamp {
        jiff::Timestamp::from_millisecond(CLOCK.load(std::sync::atomic::Ordering::SeqCst)).unwrap()
    }
    let start = fixed_now().as_millisecond();
    CLOCK.store(start, std::sync::atomic::Ordering::SeqCst);
    let mut options = options_in(Mode::Dark);
    options.now = moving;
    steering_on(Screen::new(ROWS, 160), options, |mut a| async move {
        a.emit(catalog(vec![viz(true)])).await;
        a.emit(tree()).await;
        let mut done = board_node(0, NodeState::Done, "main", "main", vec![ago(5)]);
        done.facts.push(muted("finished"));
        a.emit(page(
            TREE,
            vec![SlotBlock::Tree { nodes: vec![done] }],
            vec![muted("up "), ago(65)],
        ))
        .await;
        a.paste(&format!("/sidebar {TREE}")).key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(lines[0].contains("up 1m 05s"), "{lines:#?}");
        assert!(lines[2].contains("5s"), "{lines:#?}");
        drain(&mut a).await;
        CLOCK.store(start + 1_000, std::sync::atomic::Ordering::SeqCst);
        sleep(Duration::from_millis(1_300)).await;
        let lines = a.screen.visible();
        assert!(lines[0].contains("up 1m 06s"), "{lines:#?}");
        assert!(lines[2].contains("6s"), "{lines:#?}");
        sleep(Duration::from_millis(1_100)).await;
        let lines = a.screen.visible();
        assert!(
            lines[0].contains("up 1m 06s"),
            "a frozen clock moved: {lines:#?}"
        );
        assert!(drain(&mut a).await.is_empty());
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_page_with_no_elapsed_fact_gets_no_wake_from_its_clock() {
    stage(160, |mut a| async move {
        docked(&mut a).await;
        a.emit(long_notes(3)).await;
        a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
        a.settle().await;
        let frames = a.screen.frames();
        sleep(Duration::from_millis(1_500)).await;
        assert_eq!(a.screen.frames(), frames);
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

async fn page_colours(motion: MotionPolicy) -> usize {
    let out = std::rc::Rc::new(std::cell::Cell::new(0));
    let kept = out.clone();
    let mut options = options_in(Mode::Dark);
    options.motion = motion;
    steering_on(Screen::new(ROWS, 120), options, |mut a| async move {
        a.emit(catalog(vec![viz(true)])).await;
        a.emit(board()).await;
        a.emit(tree()).await;
        a.paste(&format!("/sidebar {TREE}")).key(KeyCode::Enter);
        a.settle().await;
        let (col, row) = locate(&a.screen, "\u{25CF} main");
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..12 {
            seen.insert(a.screen.fg_at(row, col));
            sleep(Duration::from_millis(130)).await;
        }
        kept.set(seen.len());
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
    out.get()
}

#[tokio::test(start_paused = true)]
async fn a_working_node_on_a_page_is_still_under_full_calm_and_reduced_motion() {
    for motion in [
        MotionPolicy::Full,
        MotionPolicy::Calm,
        MotionPolicy::Reduced,
    ] {
        assert_eq!(page_colours(motion).await, 1, "{motion:?}");
    }
}

async fn page_scenes(mode: Mode) -> Vec<(String, String)> {
    let mut shots = Vec::new();
    for (cols, label) in [
        (120, "120 columns, the board in two columns"),
        (80, "80 columns, the board stacked"),
        (250, "250 columns, each column at 120"),
    ] {
        let styled = shot(mode, cols, |a| async move {
            a.emit(catalog(vec![viz(true)])).await;
            a.emit(tree()).await;
            a.emit(board()).await;
            a.paste(&format!("/sidebar {TREE}")).key(KeyCode::Enter);
            finish(a).await
        })
        .await;
        shots.push((label.to_owned(), styled));
    }
    let styled = shot(mode, 120, |a| async move {
        a.emit(catalog(vec![viz(true)])).await;
        a.emit(tree()).await;
        a.emit(AgentEvent::SlotFailed {
            section: TREE.to_owned(),
            view: SlotView::Page,
            failure: SlotFailure::Failed {
                message: "the agent tree snapshot was refused because the session it names ended before the render started, so there is nothing to draw".to_owned(),
            },
            parked: true,
            restarted: false,
        })
        .await;
        a.paste(&format!("/sidebar {TREE}")).key(KeyCode::Enter);
        finish(a).await
    })
    .await;
    shots.push(("120 columns, a failing page".to_owned(), styled));
    shots
}

#[tokio::test(start_paused = true)]
async fn section_page_scenes_in_dark() {
    let shots = page_scenes(Mode::Dark).await;
    snapshot("section-page-dark-truecolor-unicode", &shots);
}

#[tokio::test(start_paused = true)]
async fn a_saved_fold_leaves_a_body_click_on_the_other_section_opening_its_page() {
    let options =
        with_saved(SavedDisplay::from_pairs([]).with_flags([(DISPLAY_SIDEBAR_FOLDED, TREE)]));
    steering_on(Screen::new(ROWS, 160), options, |mut a| async move {
        docked(&mut a).await;
        clicked(&a, "note 1").await;
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn closing_the_page_of_the_only_section_on_undocks_the_panel() {
    let off: Vec<AgentEvent> = vec![switched(TREE, false)];
    let disabled = vec![catalog(vec![viz(false)])];
    let removed = vec![catalog(vec![])];
    for events in [off, disabled, removed] {
        steering_on(Screen::new(ROWS, 160), kept_options(), |mut a| async move {
            a.emit(catalog(vec![viz(true)])).await;
            a.emit(tree()).await;
            let lines = a.settle().await;
            assert!(rule_at(&a.screen, 119), "{lines:#?}");
            a.paste(&format!("/sidebar {TREE}")).key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(page_open(&lines, "Agents"), "{lines:#?}");
            drain(&mut a).await;
            for event in events {
                a.emit(event).await;
            }
            let lines = a.settle().await;
            assert!(!keyed(&super::status_line(&lines)), "{lines:#?}");
            assert!(no_rule(&a.screen), "still docked: {lines:#?}");
            let sent = pages_sent(&drain(&mut a).await);
            assert_eq!(sent.last(), Some(&(vec![], vec![])), "{sent:?}");
            quit(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_question_card_closes_an_open_page() {
    stage(160, |mut a| async move {
        docked(&mut a).await;
        a.start("plan it", 1).await;
        sleep(Duration::from_millis(600)).await;
        a.paste(&format!("/sidebar {NOTES}")).key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        drain(&mut a).await;
        super::help::ask(&a).await;
        let lines = a.settle().await;
        assert!(!keyed(&super::status_line(&lines)), "{lines:#?}");
        assert!(shows(&lines, "Which database"), "{lines:#?}");
        let sent = pages_sent(&drain(&mut a).await);
        assert_eq!(
            sent.last(),
            Some(&(names(&[TREE, NOTES]), vec![])),
            "{sent:?}"
        );
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_quick_reopen_opens_and_shows_the_panels_content_until_new_page_content() {
    stage(160, |mut a| async move {
        docked(&mut a).await;
        clicked(&a, "note 2").await;
        a.emit(long_notes(3)).await;
        let lines = a.settle().await;
        assert!(shows(&lines, "page note 3"), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.settle().await;
        let (column, row) = locate(&a.screen, "note 2");
        click(&a, column + 1, row);
        let lines = a.settle().await;
        assert!(
            page_open(&lines, "Notes"),
            "a quick reopen was swallowed: {lines:#?}"
        );
        assert!(!shows(&lines, "page note"), "{lines:#?}");
        assert!(shows(&lines, "note 4"), "{lines:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refocus_click_or_a_click_over_a_selection_on_a_body_row_opens_nothing() {
    steering_on(Screen::new(ROWS, 160), kept_options(), |mut a| async move {
        talk(&mut a, 4).await;
        docked(&mut a).await;
        let start = a.settle().await;
        let (column, row) = locate(&a.screen, "note 2");
        a.inputs.send(wisp_tui::term::Input::Focus(true)).unwrap();
        click(&a, column + 1, row);
        assert_eq!(a.settle().await, start, "a refocus click opened the page");
        sleep(Duration::from_millis(450)).await;
        let (col, line) = locate(&a.screen, "line 1 of");
        mouse(&a, MouseKind::Press, col, line);
        mouse(&a, MouseKind::Drag, col + 6, line);
        mouse(&a, MouseKind::Release, col + 6, line);
        a.settle().await;
        assert!(!tinted(&a.screen).is_empty(), "nothing selected");
        click(&a, column + 1, row);
        let lines = a.settle().await;
        assert!(tinted(&a.screen).is_empty(), "the selection stayed");
        assert!(!keyed(&super::status_line(&lines)), "{lines:#?}");
        assert!(pages_sent(&drain(&mut a).await).is_empty());
        clicked(&a, "note 2").await;
        let lines = a.settle().await;
        assert!(page_open(&lines, "Notes"), "{lines:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}
