use wisp_protocol::{BackgroundReason, ExitStatus, TurnOutcome, TurnStats};

use super::*;

fn started(id: u32, tty: bool, size: Option<TermSize>) -> AgentEvent {
    AgentEvent::ProcessStarted {
        id,
        call_id: format!("toolu_{id}"),
        target: "npm run dev\nsecond line".into(),
        tty,
        stdin: tty,
        size,
        file: None,
        reason: BackgroundReason::Started,
        log: None,
        keeps_running: false,
        pid: None,
    }
}

fn output(id: u32, text: &str, skipped: u64) -> AgentEvent {
    AgentEvent::ProcessOutput {
        id,
        text: text.into(),
        skipped,
    }
}

fn ended(id: u32, end: ProcessEnd, duration_ms: u64) -> AgentEvent {
    AgentEvent::ProcessEnded {
        id,
        end,
        bytes: 0,
        lines: 0,
        duration_ms,
    }
}

fn exited(code: i32) -> ProcessEnd {
    ProcessEnd::Exited {
        exit: ExitStatus::Code { code },
    }
}

const SECOND: Duration = Duration::from_secs(1);

fn root() -> AgentPath {
    AgentPath::root()
}

#[test]
fn a_pipe_process_keeps_its_caption_without_buffering_output() {
    let mut p = Processes::default();
    p.apply(&root(), &started(3, false, None), SECOND);
    p.caption(
        &Key::main(3),
        "toolu_3",
        CallCaption {
            title: Some("Preview site".into()),
            command: Some("npm run dev".into()),
            extra: Some("(+1 line)".into()),
        },
    );
    p.apply(&root(), &output(3, "building\nready\n", 2048), SECOND);
    let rows = p.rows(SECOND * 13);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.command, "npm run dev");
    assert_eq!(row.caption.title.as_deref(), Some("Preview site"));
    assert_eq!(row.caption.command.as_deref(), Some("npm run dev"));
    assert_eq!(row.caption.extra.as_deref(), Some("(+1 line)"));
    assert_eq!(row.clock_ms, 12_000);
    assert!(row.terminal.is_none() && row.running());
    assert!(p.screen(&Key::main(3)).is_none());
}

#[test]
fn a_terminal_process_keeps_a_screen_sized_by_its_events() {
    let mut p = Processes::default();
    p.apply(
        &root(),
        &started(5, true, Some(TermSize { cols: 40, rows: 6 })),
        SECOND,
    );
    p.apply(&root(), &output(5, "Python 3\r\n>>> ", 0), SECOND);
    let (command, screen, done) = p.screen(&Key::main(5)).unwrap();
    assert_eq!(command, "npm run dev");
    assert!(!done);
    assert_eq!(screen.size().1.get(), 40);
    assert_eq!(screen.cursor_position(), (1, 4));
    let row = &p.rows(SECOND)[0];
    assert_eq!(row.terminal, Some(TermSize { cols: 40, rows: 6 }));
    p.apply(
        &root(),
        &AgentEvent::ProcessResized {
            id: 5,
            size: TermSize {
                cols: 100,
                rows: 30,
            },
        },
        SECOND,
    );
    assert_eq!(p.screen(&Key::main(5)).unwrap().1.size().0.get(), 30);
    assert_eq!(
        p.rows(SECOND)[0].terminal,
        Some(TermSize {
            cols: 100,
            rows: 30
        })
    );
    assert_eq!(p.open(&Key::main(5)), Opened::Screen(Key::main(5)));
}

#[test]
fn two_agents_numbers_stay_apart_in_tree_order() {
    let mut p = Processes::default();
    let (w, c, w2) = (
        AgentPath::new("/w"),
        AgentPath::new("/w/c"),
        AgentPath::new("/w-2"),
    );
    for agent in [&w2, &c, &w, &root()] {
        p.apply(agent, &started(3, false, None), SECOND);
    }
    p.apply(&w, &output(3, "only w\n", 0), SECOND);
    let rows = p.rows(SECOND);
    let order: Vec<&str> = rows.iter().map(|r| r.agent.as_str()).collect();
    assert_eq!(order, ["/", "/w", "/w/c", "/w-2"]);
    let theirs = Key {
        agent: w.clone(),
        id: 3,
    };
    assert_eq!(p.open(&theirs), Opened::Nothing);
    assert_eq!(p.open(&Key::main(3)), Opened::Call("toolu_3".into()));
    assert!(!rows[1].opens && rows[0].opens && rows[1].stoppable);
    let selected = List {
        focus: Some(theirs.clone()),
        ..List::default()
    };
    p.apply(&w, &ended(3, exited(0), 100), SECOND);
    p.apply(&root(), &ended(3, exited(1), 200), SECOND);
    p.apply(&w, &started(2, false, None), SECOND);
    let ids = p.ids();
    let rows = p.rows(SECOND);
    assert_eq!(
        ids,
        rows.iter()
            .map(|r| Key {
                agent: r.agent.clone(),
                id: r.id
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(
        rows.iter()
            .map(|r| (r.agent.as_str(), r.id, r.running()))
            .collect::<Vec<_>>(),
        [
            ("/w", 2, true),
            ("/w/c", 3, true),
            ("/w-2", 3, true),
            ("/", 3, false),
            ("/w", 3, false)
        ]
    );
    assert_eq!(selected.index(&ids), 4);
    assert_eq!(rows[4].end, Some(exited(0)));
    let asked = AgentEvent::AttachRequested {
        call_id: "toolu_3".into(),
        id: 3,
        reason: "a password".into(),
    };
    p.apply(&root(), &asked, SECOND);
    let turn_ended = AgentEvent::TurnEnded {
        outcome: TurnOutcome::Cancelled {
            usage: None,
            cost: None,
            committed_steps: 0,
        },
        stats: TurnStats::default(),
    };
    p.apply(&w, &turn_ended, SECOND);
    assert!(p.request().is_some());
    p.apply(&root(), &turn_ended, SECOND);
    assert!(p.request().is_none());
}

#[test]
fn a_terminal_without_a_size_gets_the_default_one() {
    let mut p = Processes::default();
    p.apply(&root(), &started(1, true, None), SECOND);
    assert_eq!(p.rows(SECOND)[0].terminal, Some(DEFAULT_SIZE));
}

#[test]
fn an_end_is_reported_once_and_freezes_the_clock() {
    let mut p = Processes::default();
    p.apply(&root(), &started(3, false, None), SECOND);
    assert_eq!(p.running().count(), 1);
    assert_eq!(
        p.apply(&root(), &ended(3, exited(1), 72_000), SECOND * 5),
        Some((3, exited(1)))
    );
    assert_eq!(
        p.apply(&root(), &ended(3, exited(1), 72_000), SECOND * 5),
        None
    );
    assert_eq!(p.apply(&root(), &ended(9, exited(1), 1), SECOND), None);
    assert_eq!(p.running().count(), 0);
    let row = &p.rows(SECOND * 500)[0];
    assert_eq!(row.clock_ms, 72_000);
    assert_eq!(row.end, Some(exited(1)));
    assert!(!p.is_running(&Key::main(3)));
    assert_eq!(p.open(&Key::main(3)), Opened::Call("toolu_3".to_owned()));
    assert_eq!(p.open(&Key::main(4)), Opened::Nothing);
}

#[test]
fn only_an_end_you_did_not_cause_is_announced() {
    assert!(told_on_exit(&exited(0)));
    assert!(told_on_exit(&ProcessEnd::Stopped {
        by: StoppedBy::TimeLimit { after_ms: 1 }
    }));
    assert!(told_on_exit(&ProcessEnd::Stopped {
        by: StoppedBy::Model
    }));
    assert!(!told_on_exit(&ProcessEnd::Stopped {
        by: StoppedBy::User
    }));
    assert!(!told_on_exit(&ProcessEnd::Stopped {
        by: StoppedBy::SessionEnded
    }));
}

#[test]
fn x_stops_only_when_pressed_twice_within_two_seconds_on_the_same_process() {
    let mut list = List::default();
    assert!(!list.confirm.press(Key::main(3), SECOND));
    assert_eq!(list.confirm.confirming(SECOND), Some(&Key::main(3)));
    assert_eq!(list.confirm.due(SECOND), Some(SECOND * 3));
    assert!(list.confirm.press(Key::main(3), SECOND * 2));
    assert_eq!(list.confirm.confirming(SECOND * 2), None);
    assert!(!list.confirm.press(Key::main(3), SECOND));
    assert!(!list.confirm.press(Key::main(3), SECOND * 3));
    assert!(!list.confirm.press(Key::main(4), SECOND * 3));
    assert!(list.confirm.press(Key::main(4), SECOND * 4));
}

#[test]
fn a_process_from_before_the_restart_is_listed_ended_and_opens_its_call() {
    let mut list = Processes::default();
    let at = Duration::from_secs(5);
    list.apply(
        &root(),
        &AgentEvent::ProcessRestored {
            id: 2,
            call_id: Some("toolu_2".into()),
            target: "make\nmore".into(),
            tty: true,
            end: Some(exited(4)),
            duration_ms: 3_000,
            keeps_running: false,
            unchecked: false,
            pid: None,
            log: None,
        },
        at,
    );
    list.apply(
        &root(),
        &AgentEvent::ProcessRestored {
            id: 3,
            call_id: None,
            target: "sleep 9".into(),
            tty: false,
            end: Some(exited(0)),
            duration_ms: 0,
            keeps_running: false,
            unchecked: false,
            pid: None,
            log: None,
        },
        at,
    );
    let rows = list.rows(at + Duration::from_secs(60));
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.restored && !r.running()));
    assert_eq!(rows[0].command, "make");
    assert_eq!(rows[0].end, Some(exited(4)));
    assert_eq!(rows[0].clock_ms, 3_000);
    assert_eq!(rows[0].terminal, None);
    assert_eq!(list.running().count(), 0);
    assert!(!list.attachable(&Key::main(2)));
    assert_eq!(list.open(&Key::main(2)), Opened::Call("toolu_2".into()));
    assert_eq!(list.open(&Key::main(3)), Opened::Nothing);
}

#[test]
fn a_restored_number_never_replaces_a_live_one() {
    let mut list = Processes::default();
    let at = Duration::from_secs(1);
    list.apply(&root(), &started(2, false, None), at);
    list.apply(
        &root(),
        &AgentEvent::ProcessRestored {
            id: 2,
            call_id: None,
            target: "old".into(),
            tty: false,
            end: Some(exited(1)),
            duration_ms: 0,
            keeps_running: false,
            unchecked: false,
            pid: None,
            log: None,
        },
        at,
    );
    list.caption(
        &Key::main(2),
        "toolu_2",
        CallCaption {
            title: Some("Live caption".into()),
            ..CallCaption::default()
        },
    );
    list.caption(
        &Key::main(2),
        "old_call",
        CallCaption {
            title: Some("Old caption".into()),
            ..CallCaption::default()
        },
    );
    let rows = list.rows(at);
    assert!(rows[0].running() && !rows[0].restored);
    assert_eq!(rows[0].caption.title.as_deref(), Some("Live caption"));
}

fn kept(id: u32, command: &str, pid: u32) -> AgentEvent {
    AgentEvent::ProcessStarted {
        id,
        call_id: format!("toolu_{id}"),
        target: command.into(),
        tty: false,
        stdin: false,
        size: None,
        file: None,
        reason: BackgroundReason::Started,
        keeps_running: true,
        pid: Some(pid),
        log: Some(format!("/tmp/wisp-s1/000{id}-job.log")),
    }
}

fn restored_kept(id: u32, unchecked: bool) -> AgentEvent {
    AgentEvent::ProcessRestored {
        id,
        call_id: None,
        target: "python3 app.py".into(),
        tty: false,
        end: None,
        duration_ms: 0,
        keeps_running: true,
        unchecked,
        pid: Some(4_321),
        log: Some("/tmp/wisp-s1/0007-job.log".into()),
    }
}

type RowFacts = (u32, bool, Keeps, Option<u32>, Option<String>);

fn row_facts(list: &Processes) -> Vec<RowFacts> {
    list.rows(SECOND)
        .into_iter()
        .map(|r| (r.id, r.running(), r.kept, r.pid, r.log))
        .collect()
}

#[test]
fn the_rows_carry_each_kept_process_its_pid_and_log() {
    let mut list = Processes::default();
    list.apply(
        &root(),
        &kept(1, "python3 -m http.server 8000", 4_321),
        SECOND,
    );
    list.apply(&root(), &kept(2, "redis-server", 4_400), SECOND);
    list.apply(&root(), &ended(2, exited(0), 10), SECOND);
    list.apply(&root(), &started(3, false, None), SECOND);
    let log = |id: u32| Some(format!("/tmp/wisp-s1/000{id}-job.log"));
    assert_eq!(
        row_facts(&list),
        [
            (1, true, Keeps::Running, Some(4_321), log(1)),
            (3, true, Keeps::No, None, None),
            (2, false, Keeps::Running, Some(4_400), log(2)),
        ]
    );
    assert_eq!(list.running().count(), 2);
    assert_eq!(list.stopping(), 1);
}

#[test]
fn after_a_resume_a_kept_row_keeps_its_log_and_an_unchecked_one_is_not_stoppable() {
    let mut list = Processes::default();
    list.apply(&root(), &restored_kept(7, false), SECOND);
    list.apply(&root(), &restored_kept(8, true), SECOND);
    let log = Some("/tmp/wisp-s1/0007-job.log".to_owned());
    assert_eq!(
        row_facts(&list),
        [
            (7, true, Keeps::Running, Some(4_321), log.clone()),
            (8, true, Keeps::Unchecked, Some(4_321), log),
        ]
    );
    assert!(list.is_stoppable(&Key::main(7)));
    assert!(!list.is_stoppable(&Key::main(8)));
    assert!(list.has_restored_kept());
    list.apply(
        &root(),
        &ended(
            7,
            ProcessEnd::Lost {
                reason: wisp_protocol::LostReason::Gone,
            },
            0,
        ),
        SECOND,
    );
    assert!(!list.is_running(&Key::main(7)));
    assert!(!list.has_restored_kept());
}

#[test]
fn a_kept_process_the_user_stops_is_no_longer_kept_and_counts_as_stopping() {
    let mut list = Processes::default();
    list.apply(&root(), &restored_kept(7, false), SECOND);
    list.apply(&root(), &kept(1, "redis-server", 4_400), SECOND);
    list.stop_sent(&Key::main(7));
    list.stop_sent(&Key::main(1));
    assert!(list.rows(SECOND).iter().all(|r| r.kept == Keeps::No));
    assert_eq!(list.stopping(), 2);
}

#[test]
fn a_hostile_attach_reason_is_drawn_inert_in_the_card_and_the_typing_title() {
    use wisp_render::chrome::{ScreenMode, ScreenView, StepInView, screen_rows, step_in_rows};
    use wisp_render::markdown::Layout;
    use wisp_render::styled::StyledLine;
    use wisp_render::theme::glyphs::GlyphSet;
    let hostile = "will run rm -rf ~ next\rplease type\u{7} the \u{9b}2Jcode\u{7f} a\u{202e}b";
    let bad = |text: &str| {
        text.chars()
            .any(|c| c.is_control() || matches!(c, '\u{202A}'..='\u{202E}'))
    };
    let mut p = Processes::default();
    p.apply(&root(), &started(5, true, None), Duration::ZERO);
    p.apply(
        &root(),
        &AgentEvent::AttachRequested {
            call_id: "toolu_5".into(),
            id: 5,
            reason: hostile.into(),
        },
        Duration::ZERO,
    );
    let reason = &p.request().unwrap().reason;
    let layout = Layout::new(120, GlyphSet::Unicode);
    let text = |lines: &[StyledLine]| -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect()
    };
    let card = text(&step_in_rows(
        &StepInView {
            id: 5,
            reason,
            settling: false,
            selected: None,
            typing: false,
        },
        10,
        layout,
    ));
    let parser = vt100::Parser::new(NonZeroU16::MIN, NonZeroU16::MIN, 0);
    let screen = ScreenView {
        name: Key::main(5).name(&root()),
        command: "sudo true",
        screen: parser.screen(),
        ended: false,
        attachable: true,
        mode: ScreenMode::Typing {
            asked: Some(reason.clone()),
            warning: false,
        },
    };
    let title = text(&screen_rows(&screen, 3, layout)[..1]);
    for shown in [card, title] {
        assert!(!bad(&shown), "{shown:?}");
        assert!(shown.contains("a<U+202E>b"), "{shown:?}");
        assert!(
            shown.contains("will run rm -rf ~ next^Mplease type^G the M-^[2Jcode^?"),
            "{shown:?}"
        );
    }
}
