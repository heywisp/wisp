use std::future::Future;
use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::settings::{
    AppliedChange, Layer, Saved, SettingName, SettingTarget, SettingValue, Target, When, find,
    parse_target,
};
use wisp_protocol::slot::{
    NodeState, SlotBlock, SlotContent, SlotDisabled, SlotEntry, SlotFailure, SlotNode, SlotPlace,
    SlotSpan, SlotTone, SlotView,
};
use wisp_protocol::{
    AgentEvent, AgentState, BlockKind, ClientCommand, ExitStatus, Fact, PluginEntry, Question,
    QuestionOption, ToolOutcome, ToolOutputView,
};
use wisp_protocol::{ErrorKind, RetryFocus};
use wisp_render::motion::MotionPolicy;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Theme};
use wisp_tui::app::Options;
use wisp_tui::app::display::Kept;
use wisp_tui::term::clipboard::Host;
use wisp_tui::term::{Env, Flags, SavedDisplay, Streams};
use wisp_tui::term::{Input, KeyCode, Mouse, MouseKind};

use super::agents::{entry, priced_end, spawned};
use super::processes::{described_start, waiting_terminal};
use super::select::{locate, tinted};
use super::steering::{Agent, steering_facts, steering_on};
use super::{Screen, completed};
use crate::events::{
    Changed, End, Ending, Finished, Requested, settings_changed, tool_finished, tool_requested,
    turn_ended,
};
use crate::pick::pick;
use crate::shared::{catalog, change, offer, plugin};
use crate::{Drive, eventually, frozen, shows};

pub(super) const TREE: &str = "agent-viz/tree";
pub(super) const NOTES: &str = "demo/notes";
pub(super) const ROWS: u16 = 30;
const PIPE: &str = "\u{2502}";

pub(super) fn slot(name: &str, title: &str) -> SlotEntry {
    SlotEntry {
        name: name.to_owned(),
        slot: SlotPlace::Sidebar,
        title: title.to_owned(),
        default_on: true,
        order: 0,
        disabled: None,
    }
}

pub(super) fn viz(enabled: bool) -> PluginEntry {
    plugin("agent-viz", enabled, vec![slot(TREE, "Agents")])
}

pub(super) fn demo(enabled: bool) -> PluginEntry {
    plugin("demo", enabled, vec![slot(NOTES, "Notes")])
}

pub(super) fn both() -> AgentEvent {
    catalog(vec![viz(true), demo(true)])
}

pub(super) fn text(words: &str) -> SlotSpan {
    SlotSpan::Text {
        text: words.to_owned(),
        tone: SlotTone::Text,
        bold: false,
    }
}

pub(super) fn content(section: &str, blocks: Vec<SlotBlock>) -> AgentEvent {
    AgentEvent::SlotContent {
        section: section.to_owned(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![],
            blocks,
        },
    }
}

pub(super) fn notes(count: usize) -> AgentEvent {
    content(
        NOTES,
        vec![SlotBlock::List {
            items: (1..=count)
                .map(|i| vec![text(&format!("note {i}"))])
                .collect(),
        }],
    )
}

pub(super) fn node(depth: u8, state: NodeState, role: &str, label: &str, usd: u64) -> SlotNode {
    SlotNode {
        depth,
        state,
        role: Some(role.to_owned()),
        label: label.to_owned(),
        facts: vec![SlotSpan::Usd {
            micros: usd,
            at_least: false,
            tone: SlotTone::Muted,
        }],
    }
}

pub(super) fn tree() -> AgentEvent {
    content(
        TREE,
        vec![SlotBlock::Tree {
            nodes: vec![
                node(0, NodeState::Working, "main", "main", 1_420_000),
                node(1, NodeState::Done, "coordinator", "plan", 610_000),
                node(2, NodeState::Working, "coder", "coder", 380_000),
                node(2, NodeState::Waiting, "reviewer", "review", 0),
            ],
        }],
    )
}

pub(super) fn emptied(section: &str) -> AgentEvent {
    content(section, vec![])
}

pub(super) fn options_in(mode: Mode) -> Options {
    let mut options = frozen(Options::new(
        Theme::new(mode, Depth::TrueColor),
        GlyphSet::Unicode,
    ));
    options.motion = MotionPolicy::Reduced;
    options
}

pub(super) async fn stage<D, F>(cols: u16, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    steering_on(Screen::new(ROWS, cols), options_in(Mode::Dark), drive).await;
}

pub(super) async fn drain(a: &mut Agent) -> Vec<ClientCommand> {
    sleep(Duration::from_millis(30)).await;
    let mut out = Vec::new();
    while let Ok(command) = a.commands.try_recv() {
        out.push(command);
    }
    out
}

pub(super) async fn shown(a: &mut Agent) -> Vec<Vec<String>> {
    pick!(drain(a).await, ClientCommand::SlotsShown { sections, .. } => sections)
}

pub(super) fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

pub(super) fn rule_at(screen: &Screen, column: u16) -> bool {
    (0..4).all(|row| {
        ["\u{2502}", "\u{256d}", "\u{251c}"].contains(&screen.symbol_at(row, column).as_str())
    })
}

pub(super) fn no_rule(screen: &Screen) -> bool {
    [41, 81, 91, 119, 201]
        .into_iter()
        .all(|column| (0..4).all(|row| screen.symbol_at(row, column) != PIPE))
}

pub(super) async fn talk(a: &mut Agent, lines: usize) {
    drain(a).await;
    a.start("Show me the plan", 1).await;
    let body: String = (1..=lines)
        .map(|i| format!("line {i} of the answer\n\n"))
        .collect();
    a.emit(AgentEvent::TextDelta {
        index: 0,
        text: body,
    })
    .await;
    a.emit(AgentEvent::BlockStopped { index: 0 }).await;
    a.emit(completed()).await;
}

pub(super) async fn quit(a: &Agent) {
    a.key(KeyCode::Esc);
    sleep(Duration::from_millis(30)).await;
    a.ctrl('c');
    a.ctrl('c');
}

pub(super) fn mouse(a: &Agent, kind: MouseKind, column: u16, row: u16) {
    a.inputs
        .send(Input::Mouse(Mouse {
            kind,
            column,
            row,
            modified: false,
        }))
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn nothing_docks_before_content_and_the_first_content_docks_it_once() {
    stage(120, |mut a| async move {
        a.emit(both()).await;
        a.settle().await;
        assert!(no_rule(&a.screen), "{:#?}", a.screen.visible());
        assert_eq!(shown(&mut a).await, vec![names(&[TREE, NOTES])]);
        let mark = a.screen.full_frames();
        a.emit(tree()).await;
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert!(shows(&lines, "Agents"), "{lines:#?}");
        assert_eq!(a.screen.full_frames() - mark, 1);
        let mark = a.screen.full_frames();
        a.emit(notes(3)).await;
        a.emit(tree()).await;
        a.settle().await;
        assert_eq!(a.screen.full_frames(), mark);
        assert!(shown(&mut a).await.is_empty());
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn emptied_content_stays_docked_and_says_nothing_to_show_now() {
    stage(120, |mut a| async move {
        a.emit(catalog(vec![demo(true)])).await;
        a.emit(notes(2)).await;
        a.settle().await;
        assert!(rule_at(&a.screen, 81));
        let mark = a.screen.full_frames();
        a.emit(emptied(NOTES)).await;
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert!(shows(&lines, "Nothing to show now"), "{lines:#?}");
        assert_eq!(a.screen.full_frames(), mark);
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

pub(super) fn resized(a: &Agent, cols: u16) {
    let size = a.screen.resize(ROWS, cols);
    a.inputs.send(Input::Resize(size)).unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_resize_across_120_under_the_debounce_rewraps_once_per_settled_change() {
    stage(126, |mut a| async move {
        talk(&mut a, 6).await;
        a.emit(both()).await;
        a.emit(tree()).await;
        a.settle().await;
        assert!(rule_at(&a.screen, 87));
        drain(&mut a).await;
        let mark = a.screen.full_frames();
        resized(&a, 125);
        sleep(Duration::from_millis(20)).await;
        resized(&a, 124);
        a.settle().await;
        assert!(rule_at(&a.screen, 85));
        let settles = a.screen.full_frames() - mark - 2;
        assert_eq!(settles, 1);
        assert!(shown(&mut a).await.is_empty());
        let mark = a.screen.full_frames();
        resized(&a, 119);
        sleep(Duration::from_millis(20)).await;
        resized(&a, 120);
        sleep(Duration::from_millis(20)).await;
        assert!(
            rule_at(&a.screen, 85),
            "the dock moved before the resize settled"
        );
        resized(&a, 119);
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        assert!(shows(&lines, "line 1 of the answer"), "{lines:#?}");
        assert_eq!(a.screen.full_frames() - mark, 3 + settles);
        assert_eq!(shown(&mut a).await, vec![Vec::<String>::new()]);
        let mark = a.screen.full_frames();
        resized(&a, 120);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert_eq!(a.screen.full_frames() - mark, 1 + settles);
        assert_eq!(shown(&mut a).await, vec![names(&[TREE, NOTES])]);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_disabled_removed_or_cycled_plugin_undocks_and_a_section_back_on_docks_again() {
    let cycle = SlotDisabled::Cycle {
        cycle: names(&[TREE]),
    };
    let mut cycled = slot(TREE, "Agents");
    cycled.disabled = Some(cycle);
    let undocking = [
        catalog(vec![viz(false)]),
        catalog(vec![]),
        catalog(vec![plugin("agent-viz", true, vec![cycled])]),
    ];
    for undock in undocking {
        stage(120, |mut a| async move {
            a.emit(catalog(vec![viz(true)])).await;
            a.emit(tree()).await;
            a.settle().await;
            assert!(rule_at(&a.screen, 81));
            let mark = a.screen.full_frames();
            a.emit(undock).await;
            let lines = a.settle().await;
            assert!(no_rule(&a.screen), "{lines:#?}");
            assert_eq!(a.screen.full_frames() - mark, 1);
            let mark = a.screen.full_frames();
            a.emit(catalog(vec![viz(true)])).await;
            let lines = a.settle().await;
            assert!(rule_at(&a.screen, 81), "{lines:#?}");
            assert_eq!(a.screen.full_frames() - mark, 1);
            assert_eq!(
                shown(&mut a).await,
                vec![names(&[TREE]), vec![], names(&[TREE])]
            );
            quit(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn sidebar_with_no_section_on_changes_nothing() {
    stage(120, |mut a| async move {
        a.emit(catalog(vec![viz(false)])).await;
        a.settle().await;
        drain(&mut a).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(
            shows(&lines, "No sidebar sections are on \u{b7} /config"),
            "{lines:#?}"
        );
        assert!(no_rule(&a.screen));
        assert!(drain(&mut a).await.is_empty());
        a.paste("/sidebar wide").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(
            shows(&lines, "No sidebar section by that name"),
            "{lines:#?}"
        );
        a.paste("/sidebar open wide").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(
            shows(&lines, "/sidebar takes open, a section, or nothing"),
            "{lines:#?}"
        );
        quit(&a).await;
    })
    .await;
}

fn saved_sidebar(command: &ClientCommand) -> Option<String> {
    assert!(!matches!(command, ClientCommand::SettingsUsed { .. }));
    let ClientCommand::ChangeSettings { changes, save, .. } = command else {
        return None;
    };
    assert!(save);
    let [change] = changes.as_slice() else {
        return None;
    };
    let json = serde_json::to_value(change).ok()?;
    Some(json.to_string())
}

#[tokio::test(start_paused = true)]
async fn sidebar_docks_before_content_then_hides_with_the_setting_saved() {
    stage(160, |mut a| async move {
        a.emit(both()).await;
        a.settle().await;
        assert!(no_rule(&a.screen));
        drain(&mut a).await;
        let mark = a.screen.full_frames();
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 119), "{lines:#?}");
        assert!(shows(&lines, "Nothing to show yet"), "{lines:#?}");
        assert_eq!(a.screen.full_frames() - mark, 1);
        let sent: Vec<String> = drain(&mut a)
            .await
            .iter()
            .filter_map(saved_sidebar)
            .collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("display.sidebar") && sent[0].contains("auto"));
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        let commands = drain(&mut a).await;
        let sent: Vec<String> = commands.iter().filter_map(saved_sidebar).collect();
        assert_eq!(sent.len(), 1, "{commands:?}");
        assert!(sent[0].contains("hidden"));
        assert!(commands.contains(&ClientCommand::SlotsShown {
            sections: vec![],
            pages: vec![],
        }));
        resized(&a, 40);
        a.settle().await;
        a.paste("/sidebar open").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(shows(&lines, "Nothing to show yet"), "{lines:#?}");
        assert!(rule_at(&a.screen, 0), "{lines:#?}");
        a.key(KeyCode::Esc);
        resized(&a, 160);
        a.emit(tree()).await;
        a.settle().await;
        assert!(no_rule(&a.screen));
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_overlay_at_80_scrolls_with_its_keys_and_closes() {
    stage(80, |mut a| async move {
        a.emit(both()).await;
        a.emit(notes(60)).await;
        a.settle().await;
        assert!(shown(&mut a).await.is_empty());
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 41), "{lines:#?}");
        assert!(shows(&lines, "scroll \u{b7} esc close"), "{lines:#?}");
        assert_eq!(shown(&mut a).await, vec![names(&[TREE, NOTES])]);
        a.key(KeyCode::End);
        let lines = a.settle().await;
        assert!(shows(&lines, "note 60"), "{lines:#?}");
        a.key(KeyCode::Home);
        let lines = a.settle().await;
        assert!(!shows(&lines, "note 60"), "{lines:#?}");
        a.key(KeyCode::PageDown).key(KeyCode::Down).key(KeyCode::Up);
        a.settle().await;
        assert!(shown(&mut a).await.is_empty());
        a.key(KeyCode::Char('q'));
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        assert_eq!(shown(&mut a).await, vec![Vec::<String>::new()]);
        a.paste("/sidebar open").key(KeyCode::Enter);
        a.settle().await;
        assert!(rule_at(&a.screen, 41));
        a.ctrl('c');
        a.settle().await;
        assert!(no_rule(&a.screen));
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn sidebar_open_at_160_docked_adds_no_rewrap_and_esc_keeps_the_scroll() {
    stage(160, |mut a| async move {
        a.emit(both()).await;
        a.emit(notes(80)).await;
        a.settle().await;
        assert!(rule_at(&a.screen, 119));
        mouse(&a, MouseKind::WheelDown, 140, 4);
        mouse(&a, MouseKind::WheelDown, 140, 4);
        let scrolled = a.settle().await;
        drain(&mut a).await;
        let mark = a.screen.full_frames();
        a.paste("/sidebar open").key(KeyCode::Enter);
        a.settle().await;
        assert!(rule_at(&a.screen, 119));
        assert_eq!(a.screen.full_frames(), mark);
        let commands = drain(&mut a).await;
        assert!(
            commands.iter().all(|c| saved_sidebar(c).is_none()),
            "{commands:?}"
        );
        assert!(shown_in(&commands).is_empty(), "{commands:?}");
        a.key(KeyCode::Esc);
        let after = a.settle().await;
        let panel = |lines: &[String]| -> Vec<String> {
            lines
                .iter()
                .map(|l| l.chars().skip(121).collect())
                .collect()
        };
        assert_eq!(panel(&after)[..20], panel(&scrolled)[..20]);
        assert_eq!(a.screen.full_frames(), mark);
        quit(&a).await;
    })
    .await;
}

pub(super) fn shown_in(commands: &[ClientCommand]) -> Vec<Vec<String>> {
    pick!(commands, ClientCommand::SlotsShown { sections, .. } => sections.clone())
}

#[tokio::test(start_paused = true)]
async fn widening_past_120_closes_an_overlay_opened_narrow_and_the_panel_docks() {
    stage(100, |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        a.settle().await;
        assert!(shows(&a.screen.visible(), "scroll \u{b7} esc close"));
        let size = a.screen.resize(ROWS, 130);
        a.inputs.send(Input::Resize(size)).unwrap();
        let lines = a.settle().await;
        assert!(!shows(&lines, "scroll \u{b7} esc close"), "{lines:#?}");
        assert!(rule_at(&a.screen, 91), "{lines:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn sidebar_open_at_160_hidden_opens_without_docking() {
    let mut options = options_in(Mode::Dark);
    options.sidebar = wisp_tui::term::SidebarSetting::Hidden;
    steering_on(Screen::new(ROWS, 160), options, |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        a.settle().await;
        assert!(no_rule(&a.screen));
        assert!(shown(&mut a).await.is_empty());
        let mark = a.screen.full_frames();
        a.paste("/sidebar open").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 119), "{lines:#?}");
        assert!(shows(&lines, "scroll \u{b7} esc close"), "{lines:#?}");
        assert_eq!(a.screen.full_frames(), mark);
        assert_eq!(shown(&mut a).await, vec![names(&[TREE, NOTES])]);
        a.key(KeyCode::Esc);
        a.settle().await;
        assert!(no_rule(&a.screen));
        assert_eq!(shown(&mut a).await, vec![Vec::<String>::new()]);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_question_card_closes_the_overlay_and_ctrl_t_opens_agents() {
    stage(80, |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        a.emit(spawned(entry("/review", AgentState::Running, 0.4)))
            .await;
        a.paste("/sidebar").key(KeyCode::Enter);
        a.settle().await;
        a.ctrl('t');
        let lines = a.settle().await;
        assert!(!shows(&lines, "scroll \u{b7} esc close"), "{lines:#?}");
        assert!(shows(&lines, "⠶ main"), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.settle().await;
        drain(&mut a).await;
        a.start("plan it", 1).await;
        sleep(Duration::from_millis(600)).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 41), "{lines:#?}");
        a.emit(AgentEvent::QuestionAsked {
            call_id: "q1".to_owned(),
            questions: vec![Question {
                header: "Database".to_owned(),
                question: "Which database?".to_owned(),
                options: vec![
                    QuestionOption {
                        label: "SQLite".to_owned(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "PostgreSQL".to_owned(),
                        description: String::new(),
                    },
                ],
                multi_select: false,
                source: None,
            }],
        })
        .await;
        sleep(Duration::from_millis(600)).await;
        drain(&mut a).await;
        a.key(KeyCode::Char('2'));
        let answered = drain(&mut a).await;
        assert!(
            answered
                .iter()
                .any(|c| matches!(c, ClientCommand::Answer { call_id, .. } if call_id == "q1")),
            "{answered:?}"
        );
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_drag_or_click_inside_the_panel_selects_nothing_and_a_drag_stops_at_the_rule() {
    stage(120, |mut a| async move {
        talk(&mut a, 4).await;
        a.emit(both()).await;
        a.emit(tree()).await;
        a.settle().await;
        drain(&mut a).await;
        mouse(&a, MouseKind::Press, 100, 2);
        mouse(&a, MouseKind::Drag, 104, 3);
        mouse(&a, MouseKind::Drag, 110, 5);
        sleep(Duration::from_millis(60)).await;
        assert!(tinted(&a.screen).is_empty(), "{:#?}", tinted(&a.screen));
        mouse(&a, MouseKind::Release, 110, 5);
        a.settle().await;
        assert!(tinted(&a.screen).is_empty());
        assert!(drain(&mut a).await.is_empty());
        let (col, row) = locate(&a.screen, "line 1 of");
        mouse(&a, MouseKind::Press, col, row);
        mouse(&a, MouseKind::Drag, col + 3, row);
        mouse(&a, MouseKind::Drag, 110, row);
        sleep(Duration::from_millis(60)).await;
        let selected = tinted(&a.screen);
        assert!(!selected.is_empty(), "nothing selected");
        assert!(
            selected.iter().all(|text| !text.contains("Agents")
                && !text.contains(PIPE)
                && !text.contains("main")),
            "{selected:#?}"
        );
        a.key(KeyCode::Esc);
        a.settle().await;
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_wheel_over_the_panel_scrolls_it_and_leaves_the_transcript_alone() {
    for width in [40, 80, 120] {
        stage(width, |mut a| async move {
            talk(&mut a, 40).await;
            a.emit(both()).await;
            a.emit(notes(60)).await;
            a.settle().await;
            if width < 120 {
                a.paste("/sidebar").key(KeyCode::Enter);
            }
            let start = a.settle().await;
            let rule = usize::from(width.saturating_sub(39));
            let main = |lines: &[String]| -> Vec<String> {
                lines
                    .iter()
                    .map(|l| l.chars().take(rule).collect())
                    .collect()
            };
            let side = |lines: &[String]| -> Vec<String> {
                lines
                    .iter()
                    .map(|l| l.chars().skip(rule + 1).collect())
                    .collect()
            };
            mouse(&a, MouseKind::WheelUp, width - 2, 4);
            let lines = a.settle().await;
            assert_eq!(lines, start, "the wheel up at the top moved something");
            mouse(&a, MouseKind::WheelDown, width - 2, 4);
            let lines = a.settle().await;
            if width > 40 {
                assert_eq!(main(&lines), main(&start));
            }
            assert_ne!(side(&lines), side(&start));
            if width > 40 {
                mouse(&a, MouseKind::WheelUp, 10, 4);
                let lines = a.settle().await;
                assert_ne!(main(&lines), main(&start));
            }
            quit(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_wheel_with_help_open_scrolls_help_not_the_panel() {
    stage(120, |a| async move {
        a.emit(both()).await;
        a.emit(notes(60)).await;
        a.settle().await;
        a.paste("/help").key(KeyCode::Enter);
        let help = a.settle().await;
        mouse(&a, MouseKind::WheelDown, 100, 4);
        let lines = a.settle().await;
        assert_ne!(lines, help);
        assert!(!shows(&lines, "note 1"), "{lines:#?}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(shows(&lines, "note 1"), "{lines:#?}");
        quit(&a).await;
    })
    .await;
}

async fn shell_call(a: &Agent, block: usize, id: &str) {
    a.emit(AgentEvent::BlockStarted {
        index: block,
        kind: BlockKind::ToolUse {
            writing: Some("a command".to_owned()),
            name: "shell".to_owned(),
        },
    })
    .await;
    a.emit(tool_requested(Requested {
        block,
        call_id: id.to_owned(),
        input: serde_json::json!({"command": format!("cargo test {id}")}),
        target: format!("cargo test {id}"),
        ..Requested::default()
    }))
    .await;
    a.emit(AgentEvent::call_started(id, "~/Projects/Wisp"))
        .await;
    let tail: String = (1..=8).map(|i| format!("test {i} ... ok\n")).collect();
    a.emit(tool_finished(Finished {
        call_id: id.to_owned(),
        outcome: ToolOutcome::Completed {
            is_error: false,
            exit: Some(ExitStatus::Code { code: 101 }),
        },
        output: ToolOutputView {
            bytes: tail.len() as u64,
            tail,
            lines: 8,
            file: None,
            file_problem: None,
            inlined: true,
        },
        facts: vec![Fact::Exit { code: 101 }],
        duration_ms: 3_000,
        ..Finished::default()
    }))
    .await;
}

async fn two_calls(a: &mut Agent) {
    drain(a).await;
    a.start("Run the tests", 1).await;
    shell_call(a, 1, "a").await;
    shell_call(a, 2, "b").await;
    a.emit(AgentEvent::BlockStarted {
        index: 3,
        kind: BlockKind::Text,
    })
    .await;
    let body: String = (1..=40)
        .map(|i| format!("line {i} of the answer\n\n"))
        .collect();
    a.emit(AgentEvent::TextDelta {
        index: 3,
        text: body,
    })
    .await;
    a.emit(AgentEvent::BlockStopped { index: 3 }).await;
    a.emit(completed()).await;
}

#[tokio::test(start_paused = true)]
async fn after_ctrl_o_a_wheel_over_the_panel_never_stops_esc_restoring_the_transcript() {
    stage(120, |mut a| async move {
        two_calls(&mut a).await;
        a.emit(both()).await;
        a.emit(notes(60)).await;
        let start = a.settle().await;
        drain(&mut a).await;
        let main = |lines: &[String]| -> Vec<String> {
            let end = lines
                .iter()
                .position(|l| l.starts_with('\u{256D}'))
                .unwrap_or(lines.len());
            lines[..end]
                .iter()
                .map(|l| l.chars().take(80).collect())
                .collect()
        };
        a.ctrl('o');
        a.settle().await;
        a.key(KeyCode::Down).key(KeyCode::Down).key(KeyCode::Down);
        let previewed = a.settle().await;
        assert!(!a.screen.in_pager(), "{previewed:#?}");
        assert_ne!(main(&previewed), main(&start), "the picker moved nothing");
        mouse(&a, MouseKind::WheelDown, 100, 4);
        a.settle().await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert_eq!(main(&lines), main(&start));
        assert!(shown(&mut a).await.is_empty());
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn in_the_reader_the_wheel_over_the_panel_columns_scrolls_the_reader() {
    stage(120, |mut a| async move {
        talk(&mut a, 40).await;
        a.emit(both()).await;
        a.emit(notes(60)).await;
        a.settle().await;
        drain(&mut a).await;
        a.ctrl('o');
        a.settle().await;
        assert!(a.screen.in_pager());
        let reader = a.screen.visible();
        mouse(&a, MouseKind::WheelUp, 100, 4);
        let lines = a.settle().await;
        assert_ne!(
            lines, reader,
            "the wheel over the panel's columns left the reader still"
        );
        a.key(KeyCode::Esc);
        a.settle().await;
        assert!(!a.screen.in_pager());
        assert!(shown(&mut a).await.is_empty());
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_press_and_drag_inside_the_overlay_select_and_open_nothing() {
    stage(80, |mut a| async move {
        two_calls(&mut a).await;
        a.emit(both()).await;
        a.emit(tree()).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let open = a.settle().await;
        assert!(rule_at(&a.screen, 41), "{open:#?}");
        drain(&mut a).await;
        for row in [3, 8, 12, 16, 20] {
            mouse(&a, MouseKind::Press, 60, row);
            mouse(&a, MouseKind::Drag, 64, row + 1);
            mouse(&a, MouseKind::Drag, 70, row + 2);
            mouse(&a, MouseKind::Release, 70, row + 2);
        }
        let lines = a.settle().await;
        assert!(!a.screen.in_pager(), "{lines:#?}");
        assert_eq!(lines, open);
        assert!(drain(&mut a).await.is_empty());
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn hostile_strings_reach_the_screen_inert() {
    stage(120, |mut a| async move {
        let mut hostile = viz(true);
        hostile.slots[0].title = "Age\u{1b}[31mnts\u{202e}x".to_owned();
        a.emit(catalog(vec![hostile, demo(true)])).await;
        a.emit(content(
            TREE,
            vec![SlotBlock::Line {
                spans: vec![text("ok\u{1b}]52;c;aGk=\u{7}\u{202e}\tend\nnext")],
            }],
        ))
        .await;
        a.emit(AgentEvent::SlotFailed {
            section: NOTES.to_owned(),
            view: SlotView::Panel,
            failure: SlotFailure::Failed {
                message: "bad\u{1b}[2Jthing\u{200b}".to_owned(),
            },
            parked: false,
            restarted: false,
        })
        .await;
        let lines = a.settle().await;
        let all = lines.join("\n");
        assert!(!all.contains('\u{1b}'), "{lines:#?}");
        assert!(!all.contains('\u{202e}'), "{lines:#?}");
        assert!(all.contains("^["), "{lines:#?}");
        assert!(all.contains("<U+202E>"), "{lines:#?}");
        assert!(all.contains("Age^[[31mnts"), "{lines:#?}");
        assert!(all.contains("bad^[[2Jthing"), "{lines:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

async fn attach_size(dock: bool) -> wisp_protocol::TermSize {
    let out = std::rc::Rc::new(std::cell::Cell::new(None));
    let kept = out.clone();
    stage(120, |mut a| async move {
        if dock {
            a.emit(both()).await;
            a.emit(tree()).await;
        }
        a.settle().await;
        assert_eq!(rule_at(&a.screen, 81), dock);
        a.emit(described_start(5)).await;
        a.emit(waiting_terminal(5)).await;
        a.emit(AgentEvent::ProcessNeedsInput {
            id: 5,
            kind: wisp_protocol::NeedsInputKind::HiddenInput,
        })
        .await;
        a.settle().await;
        drain(&mut a).await;
        a.ctrl(']');
        a.settle().await;
        a.settle().await;
        let sizes: Vec<_> = drain(&mut a)
            .await
            .into_iter()
            .filter_map(|command| match command {
                ClientCommand::AttachProcess { size, .. } => size,
                _ => None,
            })
            .collect();
        kept.set(sizes.last().copied());
        a.ctrl(']');
        a.settle().await;
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
    out.get().unwrap()
}

#[tokio::test(start_paused = true)]
async fn the_attach_size_ignores_the_dock() {
    assert_eq!(attach_size(true).await, attach_size(false).await);
}

#[tokio::test(start_paused = true)]
async fn the_agents_strip_sits_under_a_docked_panel_and_watching_a_child_keeps_it() {
    stage(120, |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        drain(&mut a).await;
        a.start("Review the plan", 1).await;
        a.emit(spawned(entry("/review", AgentState::Running, 0.4)))
            .await;
        a.emit_spent(priced_end(0.42, 0.4)).await;
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        let rule_colour = a.screen.fg_at(2, 81);
        drain(&mut a).await;
        a.ctrl('t');
        a.key(KeyCode::Down).key(KeyCode::Enter);
        let lines = a.settle().await;
        let commands = drain(&mut a).await;
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, ClientCommand::WatchAgent { live: true, .. })),
            "{commands:?}"
        );
        assert!(shown_in(&commands).is_empty(), "{commands:?}");
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert!(shows(&lines, "Agents"), "{lines:#?}");
        assert_eq!(a.screen.fg_at(2, 81), rule_colour);
        let header: String = lines[0].chars().take(81).collect();
        assert!(!header.contains("esc back"), "{lines:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.chars().take(81).collect::<String>().contains("esc back")),
            "{lines:#?}"
        );
        a.key(KeyCode::Esc);
        a.settle().await;
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

pub(super) async fn base(a: &mut Agent) {
    a.emit(both()).await;
    talk(a, 30).await;
    a.emit(tree()).await;
    a.emit(notes(40)).await;
    a.settle().await;
    drain(a).await;
}

pub(super) async fn shot<D, F>(mode: Mode, cols: u16, drive: D) -> String
where
    D: FnOnce(Agent) -> F + 'static,
    F: Future<Output = String>,
{
    let out = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let kept = out.clone();
    steering_on(Screen::new(ROWS, cols), options_in(mode), |a| async move {
        let text = drive(a).await;
        *kept.borrow_mut() = text;
    })
    .await;
    out.borrow().clone()
}

pub(super) async fn finish(mut a: Agent) -> String {
    a.settle().await;
    let styled = a.screen.styled();
    drain(&mut a).await;
    quit(&a).await;
    styled
}

async fn scenes(mode: Mode) -> Vec<(String, String)> {
    let mut shots = Vec::new();
    for cols in [120, 250, 119] {
        let styled = shot(mode, cols, |mut a| async move {
            base(&mut a).await;
            finish(a).await
        })
        .await;
        shots.push((format!("{cols} columns"), styled));
    }
    let styled = shot(mode, 160, |mut a| async move {
        base(&mut a).await;
        for _ in 0..4 {
            mouse(&a, MouseKind::WheelDown, 140, 4);
        }
        finish(a).await
    })
    .await;
    shots.push(("160 columns, the panel scrolled".to_owned(), styled));
    let styled = shot(mode, 80, |mut a| async move {
        base(&mut a).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        finish(a).await
    })
    .await;
    shots.push(("80 columns, the overlay open".to_owned(), styled));
    let styled = shot(mode, 120, |mut a| async move {
        base(&mut a).await;
        a.paste("/effort").key(KeyCode::Enter);
        finish(a).await
    })
    .await;
    shots.push(("a picker under the panel".to_owned(), styled));
    let styled = shot(mode, 120, |mut a| async move {
        base(&mut a).await;
        drain(&mut a).await;
        a.start("Review the plan", 2).await;
        a.emit(spawned(entry("/review", AgentState::Running, 0.4)))
            .await;
        a.emit(spawned(entry("/review/coder", AgentState::Running, 0.8)))
            .await;
        a.emit_spent(priced_end(0.42, 1.2)).await;
        finish(a).await
    })
    .await;
    shots.push(("the agents strip under the panel".to_owned(), styled));
    let styled = shot(mode, 120, |mut a| async move {
        base(&mut a).await;
        a.emit(spawned(entry("/review", AgentState::Running, 0.4)))
            .await;
        a.ctrl('t');
        finish(a).await
    })
    .await;
    shots.push(("/agents over the panel".to_owned(), styled));
    shots
}

pub(super) fn snapshot(name: &str, shots: &[(String, String)]) {
    let body: String = shots
        .iter()
        .map(|(name, shot)| format!("== {name}\n{shot}"))
        .collect();
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_snapshot_path("../snapshots/chrome");
    settings.bind(|| {
        insta::assert_snapshot!(name.to_owned(), body);
    });
}

#[tokio::test(start_paused = true)]
async fn sidebar_scenes_in_dark() {
    let shots = scenes(Mode::Dark).await;
    snapshot("sidebar-dark-truecolor-unicode", &shots);
}

pub(super) fn kept() -> Kept {
    Kept {
        env: Env::from_pairs([
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("LANG", "en_US.UTF-8"),
            ("WISP_MOTION", "reduced"),
        ]),
        streams: Streams {
            stdin_tty: true,
            stdout_tty: true,
            stderr_tty: true,
        },
        flags: Flags::default(),
        saved: SavedDisplay::from_pairs([]),
        probe: Some(super::reported()),
        host: Host::default(),
        tmux: None,
    }
}

pub(super) fn kept_options() -> Options {
    kept_options_with(kept())
}

pub(super) fn kept_options_with(kept: Kept) -> Options {
    let caps = kept.resolve();
    let mut options = frozen(kept.options(&caps));
    options.kept = Some(kept);
    options
}

pub(super) fn applied(target: SettingTarget, value: SettingValue) -> AgentEvent {
    settings_changed(Changed {
        endpoint: "claude-api".into(),
        model: "test-model".into(),
        changes: vec![AppliedChange {
            saved: Some(Saved {
                path_display: "~/.config/wisp/config.toml".to_owned(),
                line: Some(3),
                notes: vec![],
            }),
            ..change(target, Some(value), Layer::Config, When::Now)
        }],
        ..Changed::default()
    })
}

pub(super) fn switched(section: &str, on: bool) -> AgentEvent {
    let Ok(Target::Path(path)) =
        parse_target(&format!("plugins.{}", section.replace('/', ".slots.")))
    else {
        panic!("{section} is no section switch");
    };
    applied(SettingTarget::Path(path), SettingValue::Bool(on))
}

pub(super) fn sidebar_set(word: &str) -> AgentEvent {
    let spec = find("display.sidebar").unwrap();
    applied(
        SettingTarget::Name(SettingName::of(spec)),
        spec.parse(word).unwrap(),
    )
}

#[tokio::test(start_paused = true)]
async fn the_last_section_switched_off_undocks_and_switching_it_on_docks_again() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        a.settle().await;
        assert!(rule_at(&a.screen, 81));
        assert_eq!(shown(&mut a).await, vec![names(&[TREE, NOTES])]);
        a.emit(switched(NOTES, false)).await;
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert!(!shows(&lines, "Notes"), "{lines:#?}");
        assert_eq!(shown(&mut a).await, vec![names(&[TREE])]);
        let mark = a.screen.full_frames();
        a.emit(switched(TREE, false)).await;
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        assert_eq!(a.screen.full_frames() - mark, 1);
        assert_eq!(shown(&mut a).await, vec![Vec::<String>::new()]);
        let mark = a.screen.full_frames();
        a.emit(switched(TREE, true)).await;
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert_eq!(a.screen.full_frames() - mark, 1);
        assert_eq!(shown(&mut a).await, vec![names(&[TREE])]);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn display_sidebar_hidden_from_config_undocks_and_auto_docks_again() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        a.settle().await;
        assert!(rule_at(&a.screen, 81));
        drain(&mut a).await;
        let mark = a.screen.full_frames();
        a.emit(sidebar_set("hidden")).await;
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        assert_eq!(a.screen.full_frames() - mark, 1);
        assert_eq!(shown(&mut a).await, vec![Vec::<String>::new()]);
        a.emit(sidebar_set("auto")).await;
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert_eq!(shown(&mut a).await, vec![names(&[TREE, NOTES])]);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn suggestions_over_the_panel_keep_the_wheel_from_the_panel() {
    stage(120, |mut a| async move {
        a.emit(both()).await;
        a.emit(notes(60)).await;
        a.settle().await;
        drain(&mut a).await;
        a.paste("/");
        let open = a.settle().await;
        let row = open
            .iter()
            .position(|l| l.contains("/ps  "))
            .unwrap_or_else(|| panic!("{open:#?}"));
        mouse(&a, MouseKind::WheelDown, 100, u16::try_from(row).unwrap());
        let lines = a.settle().await;
        let side = |lines: &[String]| -> Vec<String> {
            lines
                .iter()
                .take(4)
                .map(|l| l.chars().skip(82).collect())
                .collect()
        };
        assert_eq!(side(&lines), side(&open), "{lines:#?}");
        assert!(
            drain(&mut a)
                .await
                .iter()
                .all(|c| !matches!(c, ClientCommand::SlotsShown { .. }))
        );
        a.key(KeyCode::Esc);
        quit(&a).await;
    })
    .await;
}

fn real_now() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

fn now_ms() -> u64 {
    u64::try_from(jiff::Timestamp::now().as_millisecond()).unwrap()
}

fn panel_of(lines: &[String]) -> Vec<String> {
    lines.iter().map(|l| l.chars().skip(82).collect()).collect()
}

#[tokio::test]
async fn an_elapsed_fact_ticks_with_no_events() {
    let mut options = options_in(Mode::Dark);
    options.now = real_now;
    steering_on(Screen::new(ROWS, 120), options, |mut a| async move {
        a.emit(catalog(vec![viz(true)])).await;
        let mut done = node(0, NodeState::Done, "main", "main", 0);
        done.facts = vec![SlotSpan::Elapsed {
            since_ms: now_ms() - 5_000,
            tone: SlotTone::Muted,
        }];
        a.emit(content(TREE, vec![SlotBlock::Tree { nodes: vec![done] }]))
            .await;
        let drawn = a
            .until("the tree", |lines| shows(&panel_of(lines), "\u{2713} main"))
            .await;
        let before = panel_of(&drawn);
        let moved = eventually(|| panel_of(&a.screen.visible()) != before).await;
        assert!(moved, "the elapsed time never moved: {before:#?}");
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

async fn pulse_colours(motion: MotionPolicy, full: bool) -> usize {
    let out = std::rc::Rc::new(std::cell::Cell::new(0));
    let kept = out.clone();
    let mut options = options_in(Mode::Dark);
    options.motion = motion;
    options.full_repaint = full;
    steering_on(Screen::new(ROWS, 120), options, |mut a| async move {
        a.emit(catalog(vec![viz(true)])).await;
        a.emit(content(
            TREE,
            vec![SlotBlock::Tree {
                nodes: vec![node(0, NodeState::Working, "main", "main", 0)],
            }],
        ))
        .await;
        a.settle().await;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..12 {
            seen.insert(a.screen.fg_at(3, 83));
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
async fn a_working_node_pulses_under_full_motion_and_is_still_under_reduced_or_full_repaint() {
    let full = pulse_colours(MotionPolicy::Full, false).await;
    let reduced = pulse_colours(MotionPolicy::Reduced, false).await;
    let repainted = pulse_colours(MotionPolicy::Full, true).await;
    assert!(full > 2, "full motion showed {full} colours");
    assert_eq!(reduced, 1);
    assert_eq!(repainted, 1);
}

#[tokio::test(start_paused = true)]
async fn the_new_output_hint_sits_left_of_the_rule() {
    stage(120, |mut a| async move {
        talk(&mut a, 40).await;
        a.emit(both()).await;
        a.emit(tree()).await;
        a.settle().await;
        for _ in 0..3 {
            mouse(&a, MouseKind::WheelUp, 10, 4);
        }
        a.settle().await;
        a.emit(AgentEvent::turn_started(2)).await;
        a.emit(AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        })
        .await;
        a.emit(AgentEvent::TextDelta {
            index: 0,
            text: "more words\n\nand more\n".to_owned(),
        })
        .await;
        let lines = a.settle().await;
        let row = lines
            .iter()
            .find(|l| l.contains(" new "))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        let at = row.find(" new ").unwrap();
        assert!(row[..at].chars().count() < 81, "{row}");
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        a.emit(AgentEvent::BlockStopped { index: 0 }).await;
        a.emit(completed()).await;
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn docking_clears_a_selection_in_the_transcript() {
    stage(120, |mut a| async move {
        talk(&mut a, 4).await;
        a.emit(both()).await;
        a.settle().await;
        drain(&mut a).await;
        let (col, row) = locate(&a.screen, "line 1 of");
        mouse(&a, MouseKind::Press, col, row);
        mouse(&a, MouseKind::Drag, col + 3, row);
        mouse(&a, MouseKind::Drag, col + 12, row);
        mouse(&a, MouseKind::Release, col + 12, row);
        a.settle().await;
        assert!(!tinted(&a.screen).is_empty(), "nothing selected");
        a.emit(tree()).await;
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 81), "{lines:#?}");
        assert!(tinted(&a.screen).is_empty(), "{:#?}", tinted(&a.screen));
        drain(&mut a).await;
        a.key(KeyCode::Esc);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn sidebar_before_the_catalog_says_the_sections_are_not_known_yet() {
    stage(120, |mut a| async move {
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(
            shows(&lines, "The sidebar's sections aren't known yet"),
            "{lines:#?}"
        );
        assert!(drain(&mut a).await.is_empty());
        a.emit(catalog(vec![])).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(shows(&lines, "No sidebar sections are on"), "{lines:#?}");
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_step_in_card_closes_the_overlay_and_takes_the_keys() {
    stage(80, |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        drain(&mut a).await;
        a.start("install it", 1).await;
        a.emit(waiting_terminal(5)).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 41), "{lines:#?}");
        drain(&mut a).await;
        a.emit(AgentEvent::AttachRequested {
            call_id: "toolu_ask".to_owned(),
            id: 5,
            reason: "sudo password".to_owned(),
        })
        .await;
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        a.key(KeyCode::Char('2'));
        let sent = drain(&mut a).await;
        assert!(
            sent.iter()
                .any(|c| matches!(c, ClientCommand::DeclineAttach { .. })),
            "{sent:?}"
        );
        a.emit(super::steering::cancelled()).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_retry_choice_closes_the_overlay_and_takes_the_keys() {
    stage(80, |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        drain(&mut a).await;
        a.start("go", 1).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(rule_at(&a.screen, 41), "{lines:#?}");
        drain(&mut a).await;
        a.emit(turn_ended(End {
            ending: Ending::Error(ErrorKind::Server),
            message: "Upstream provider stream was interrupted.".to_owned(),
            offer: Some(offer(true, false, RetryFocus::Retry)),
            ..End::default()
        }))
        .await;
        for command in drain(&mut a).await {
            if let ClientCommand::DescribeSettings { request } = command {
                a.emit(super::described(request.0)).await;
            }
        }
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        a.key(KeyCode::Enter);
        let sent = drain(&mut a).await;
        assert!(
            sent.iter()
                .any(|c| matches!(c, ClientCommand::Retry { .. })),
            "{sent:?}"
        );
        quit(&a).await;
    })
    .await;
}

fn forced_options() -> Options {
    let mut kept = kept();
    kept.env = Env::from_pairs([
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("LANG", "en_US.UTF-8"),
        ("WISP_MOTION", "reduced"),
        ("WISP_SIDEBAR", "hidden"),
    ]);
    let caps = kept.resolve();
    let mut options = frozen(kept.options(&caps));
    options.kept = Some(kept);
    options
}

#[tokio::test(start_paused = true)]
async fn sidebar_under_a_forced_setting_opens_the_overlay_and_saves_nothing() {
    steering_on(
        Screen::new(ROWS, 160),
        forced_options(),
        |mut a| async move {
            a.emit(both()).await;
            a.emit(tree()).await;
            a.settle().await;
            assert!(no_rule(&a.screen));
            drain(&mut a).await;
            a.paste("/sidebar").key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(rule_at(&a.screen, 119), "{lines:#?}");
            assert!(shows(&lines, "WISP_SIDEBAR=hidden sets it"), "{lines:#?}");
            let sent = drain(&mut a).await;
            assert!(sent.iter().all(|c| saved_sidebar(c).is_none()), "{sent:?}");
            a.key(KeyCode::Esc);
            a.settle().await;
            assert!(no_rule(&a.screen));
            drain(&mut a).await;
            quit(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_overlay_keeps_a_paste_out_of_the_composer() {
    let dir = super::images::Dir::new("under-overlay");
    let options = super::images::pasting_into(&dir, options_in(Mode::Dark));
    let seeing = super::images::seeing(super::facts());
    steering_facts(Screen::new(ROWS, 80), options, seeing, |mut a| async move {
        a.emit(both()).await;
        a.emit(tree()).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let open = a.settle().await;
        assert!(rule_at(&a.screen, 41), "{open:#?}");
        super::images::paste_every_way(&a.inputs).await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(no_rule(&a.screen), "{lines:#?}");
        assert_eq!(
            super::images::leaks(&a.screen, &dir),
            super::images::NO_LEAKS
        );
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

const WINDOW: &str = "context/window";

fn context_catalog(default_on: bool) -> AgentEvent {
    let entry = SlotEntry {
        default_on,
        ..slot(WINDOW, "Context")
    };
    catalog(vec![plugin("context", true, vec![entry])])
}

fn usage_content() -> AgentEvent {
    let content: SlotContent =
        serde_json::from_str(include_str!("../fixtures/context-usage.json")).unwrap();
    AgentEvent::SlotContent {
        section: WINDOW.to_owned(),
        view: SlotView::Panel,
        content,
    }
}

#[tokio::test(start_paused = true)]
async fn the_context_section_off_by_default_is_never_shown_and_never_docks() {
    stage(160, |mut a| async move {
        a.emit(context_catalog(false)).await;
        a.settle().await;
        assert!(shown(&mut a).await.iter().all(Vec::is_empty));
        talk(&mut a, 3).await;
        a.settle().await;
        assert!(shown(&mut a).await.iter().all(Vec::is_empty));
        assert!(no_rule(&a.screen), "{:#?}", a.screen.visible());
        quit(&a).await;
    })
    .await;
}

async fn context_shot(set: GlyphSet, cols: u16) -> String {
    let out = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let kept = out.clone();
    let mut options = options_in(Mode::Dark);
    options.glyphs = set;
    steering_on(Screen::new(ROWS, cols), options, |mut a| async move {
        a.emit(context_catalog(true)).await;
        a.settle().await;
        if cols >= 120 {
            assert_eq!(shown(&mut a).await, vec![names(&[WINDOW])]);
        }
        talk(&mut a, 30).await;
        a.emit(usage_content()).await;
        if cols < 120 {
            a.settle().await;
            a.paste("/sidebar").key(KeyCode::Enter);
        }
        *kept.borrow_mut() = finish(a).await;
    })
    .await;
    out.borrow().clone()
}

#[tokio::test(start_paused = true)]
async fn the_context_section_docked_at_120_and_over_the_transcript_at_80_and_40() {
    for (set, name) in [
        (GlyphSet::Unicode, "unicode"),
        (GlyphSet::AmbiguousWide, "ambiguous-wide"),
        (GlyphSet::Ascii, "ascii"),
    ] {
        let mut shots = Vec::new();
        for (cols, how) in [(120, "docked"), (80, "the overlay"), (40, "the overlay")] {
            let styled = context_shot(set, cols).await;
            let frame = styled
                .lines()
                .filter_map(|l| l.strip_prefix('|'))
                .rfind(|l| l.starts_with(['\u{256D}', '+']))
                .unwrap_or_default();
            assert_eq!(frame.chars().count(), usize::from(cols), "{styled}");
            shots.push((format!("{cols} columns, {how}"), styled));
        }
        snapshot(
            &format!("sidebar-context-usage-dark-truecolor-{name}"),
            &shots,
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_covering_card_keeps_keys_out_of_a_reopened_sidebar() {
    for question in [true, false] {
        stage(80, |mut a| async move {
            a.emit(both()).await;
            a.emit(notes(40)).await;
            drain(&mut a).await;
            a.start("install it", 1).await;
            a.emit(waiting_terminal(5)).await;
            a.emit(if question {
                AgentEvent::QuestionAsked {
                    call_id: "q1".to_owned(),
                    questions: vec![super::questions::database()],
                }
            } else {
                super::processes::asked(5)
            })
            .await;
            a.paste("/sidebar").key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(rule_at(&a.screen, 41), "{question}: {lines:?}");
            let (_, covered) = locate(
                &a.screen,
                if question {
                    "2 SQLite"
                } else {
                    "sudo password"
                },
            );
            mouse(&a, MouseKind::WheelDown, 78, covered);
            assert_eq!(
                a.settle().await[..4],
                lines[..4],
                "covered sidebar stays still"
            );
            mouse(&a, MouseKind::WheelDown, 78, 3);
            let moved = a.settle().await;
            assert_ne!(moved[..4], lines[..4], "exposed sidebar still scrolls");
            mouse(&a, MouseKind::WheelUp, 78, 3);
            assert_eq!(a.settle().await[..4], lines[..4]);
            a.key(KeyCode::Char('q'))
                .key(KeyCode::Down)
                .key(KeyCode::Enter);
            drain(&mut a).await;
            if question {
                sleep(Duration::from_millis(600)).await;
                a.key(KeyCode::Char('3')).key(KeyCode::Char('d'));
                a.nothing_sent().await;
            }
            a.emit(if question {
                AgentEvent::QuestionClosed {
                    call_id: "q1".to_owned(),
                    how: wisp_protocol::QuestionEnd::Stopped,
                }
            } else {
                super::processes::ask_closed()
            })
            .await;
            let lines = a.settle().await;
            assert!(rule_at(&a.screen, 41), "{question}: {lines:?}");
            assert!(lines[3].contains("note 1 "), "{question}: {lines:?}");
            if question {
                for rows in [1, ROWS] {
                    let size = a.screen.resize(rows, 80);
                    a.inputs.send(Input::Resize(size)).unwrap();
                    a.settle().await;
                    if rows == 1 {
                        a.key(KeyCode::Char('q'));
                        a.settle().await;
                    }
                }
                assert!(rule_at(&a.screen, 41));
            }
            a.key(KeyCode::Char('q'));
            a.settle().await;
            assert!(no_rule(&a.screen));
            super::processes::finish(&a).await;
        })
        .await;
    }
}
