use std::sync::LazyLock;

use wisp_protocol::StoppedBy;

use super::*;
use crate::events::nz;
use crate::motion::MotionPolicy;
use crate::testing::{line_text, trimmed};
use crate::text::WidthMode;
use crate::theme::glyphs::GlyphSet;

static MAIN: LazyLock<AgentPath> = LazyLock::new(AgentPath::root);

const FULL: MotionCtx = MotionCtx {
    policy: MotionPolicy::Full,
    ascii: false,
    blend: true,
};

fn row(id: u32, command: &str, end: Option<ProcessEnd>, title: Option<&str>) -> ProcessRow {
    ProcessRow {
        id,
        command: command.to_owned(),
        terminal: None,
        clock_ms: 724_000,
        caption: CallCaption {
            title: title.map(str::to_owned),
            command: title.map(|_| command.to_owned()),
            extra: None,
        },
        waiting: false,
        restored: false,
        kept: Keeps::No,
        pid: None,
        log: None,
        agent: AgentPath::root(),
        agent_ended: false,
        opens: true,
        stoppable: end.is_none(),
        attachable: false,
        end,
    }
}

fn name(id: u32) -> ProcessName {
    ProcessName::of(id, &MAIN, &MAIN)
}

fn exit(code: i32) -> Option<ProcessEnd> {
    Some(ProcessEnd::Exited {
        exit: ExitStatus::Code { code },
    })
}

fn sample() -> Vec<ProcessRow> {
    vec![
        row(3, "npm run dev", None, Some("Preview site")),
        ProcessRow {
            clock_ms: 72_000,
            ..row(4, "cargo test --workspace", exit(0), Some("Check code"))
        },
        ProcessRow {
            terminal: Some(TermSize {
                cols: 100,
                rows: 30,
            }),
            clock_ms: 40_000,
            attachable: true,
            ..row(5, "python3 -i", None, Some("Python shell"))
        },
    ]
}

fn view(rows: &[ProcessRow], focus: usize, confirm: bool) -> PsView<'_> {
    PsView {
        rows,
        focus,
        confirm,
        here: &MAIN,
        t: Duration::from_millis(300),
        cx: FULL,
    }
}

#[test]
fn the_list_shows_each_process_on_one_line_with_its_caption_state_and_clock() {
    let rows = sample();
    let out = ps_rows(
        &view(&rows, 0, false),
        20,
        Layout::new(80, GlyphSet::Unicode),
    );
    let text = trimmed(&out);
    assert_eq!(text[0], " Processes");
    assert!(
        text[1].starts_with(" Running") && text[1].ends_with('2'),
        "{text:?}"
    );
    assert!(
        text[2].starts_with(" › ⠘ 3  Preview site  npm run dev"),
        "{text:?}"
    );
    assert!(text[2].ends_with("running · 12m 04s"), "{text:?}");
    assert!(text[3].contains("Python shell  python3 -i"), "{text:?}");
    assert!(text[3].ends_with("terminal 100x30 · 40s"), "{text:?}");
    assert!(
        text[4].starts_with(" History") && text[4].ends_with('1'),
        "{text:?}"
    );
    assert!(
        text[5].starts_with("   ✓ 4  Check code  cargo test --workspace"),
        "{text:?}"
    );
    assert!(text[5].ends_with("exit 0 · 1m 12s"), "{text:?}");
    assert_eq!(text[6], "   enter open · x stop · ↑↓ move · esc close");
    assert_eq!(text.len(), 7);
    for (index, tone, bold) in [
        (0, Token::Text, true),
        (1, Token::TextMuted, true),
        (4, Token::TextMuted, true),
    ] {
        let span = &out[index].spans[1];
        assert_eq!(span.look.fg, Some(tone.into()));
        assert_eq!(span.look.bold, bold);
    }
    assert!(!out[1].spans.last().unwrap().look.bold);
    assert!(
        out.iter()
            .all(|l| width(&line_text(l), WidthMode::Normal) == 80)
    );
}

#[test]
fn glyphs_follow_the_brand_for_every_end() {
    let ends = [
        (exit(0), "\u{2713}", Token::Ok, "exit 0"),
        (exit(2), "\u{2A2F}", Token::Error, "exit 2"),
        (
            Some(ProcessEnd::Exited {
                exit: ExitStatus::Signal { signal: 9 },
            }),
            "\u{2A2F}",
            Token::Error,
            "signal 9",
        ),
        (
            Some(ProcessEnd::Failed {
                message: "spawn".into(),
            }),
            "\u{2A2F}",
            Token::Error,
            "failed",
        ),
        (
            Some(ProcessEnd::Stopped {
                by: StoppedBy::User,
            }),
            "\u{25E6}",
            Token::TextMuted,
            "stopped by you",
        ),
        (
            Some(ProcessEnd::Stopped {
                by: StoppedBy::TimeLimit { after_ms: 1 },
            }),
            "\u{25E6}",
            Token::TextMuted,
            "time limit",
        ),
        (
            Some(ProcessEnd::Lost {
                reason: wisp_protocol::LostReason::Gone,
            }),
            "\u{25E6}",
            Token::TextMuted,
            "lost",
        ),
    ];
    for (end, mark, tone, words) in ends {
        let rows = vec![row(7, "job", end, None)];
        let out = ps_rows(
            &view(&rows, 0, false),
            10,
            Layout::new(60, GlyphSet::Unicode),
        );
        let entry = out
            .iter()
            .find(|l| line_text(l).contains("7  job"))
            .unwrap();
        let glyph = &entry.spans[3];
        assert_eq!(glyph.text, mark);
        assert_eq!(glyph.look.fg, Some(tone.into()));
        assert!(line_text(entry).contains(words), "{:?}", line_text(entry));
        assert!(
            !line_text(out.last().unwrap()).contains("x stop"),
            "{:?}",
            line_text(out.last().unwrap())
        );
    }
}

#[test]
fn captions_fit_descriptions_commands_scripts_and_missing_titles_on_one_line() {
    let mut rows = vec![
        row(
            3,
            "demo inspect --source samples --recursive --details",
            None,
            Some("Inspect samples"),
        ),
        ProcessRow {
            caption: CallCaption {
                title: Some("Build samples".into()),
                command: Some("make".into()),
                extra: Some("(+3 lines)".into()),
            },
            ..row(4, "make", None, None)
        },
        row(5, "demo watch --brief", None, None),
        ProcessRow {
            terminal: Some(TermSize { cols: 40, rows: 6 }),
            ..row(6, "demo shell", None, Some("Shell"))
        },
        ProcessRow {
            caption: CallCaption {
                extra: Some("(+2 lines)".into()),
                ..CallCaption::default()
            },
            ..row(7, "make", None, None)
        },
    ];
    for row in &mut rows {
        row.clock_ms = 20_000;
    }
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let out = ps_rows(&view(&rows, 0, false), 20, Layout::new(width, glyphs));
            let text = trimmed(&out);
            assert_eq!(out.len(), rows.len() + 4);
            assert!(text[2].contains("Inspect samples"), "{text:?}");
            assert!(text[3].contains("Build samples"), "{text:?}");
            assert!(text[4].contains("demo watch"), "{text:?}");
            assert!(text[5].contains("Shell"), "{text:?}");
            assert!(text[6].contains("make (+2 lines)"), "{text:?}");
            if width == 40 {
                assert!(
                    !text[2].contains("demo inspect") && !text[3].contains("make"),
                    "{text:?}"
                );
            } else {
                assert!(
                    text[2].contains("demo inspect --source samples"),
                    "{text:?}"
                );
                assert!(
                    text[3].contains("Build samples  make (+3 lines)"),
                    "{text:?}"
                );
                let command = out[3]
                    .spans
                    .iter()
                    .find(|s| s.text == "make (+3 lines)")
                    .unwrap();
                assert_eq!(command.look.fg, Some(Token::TextMuted.into()));
            }
        }
    }
}

#[test]
fn the_first_x_asks_for_a_second() {
    let rows = sample();
    let out = ps_rows(
        &view(&rows, 0, true),
        20,
        Layout::new(80, GlyphSet::Unicode),
    );
    let hint = out.last().unwrap();
    assert_eq!(
        line_text(hint).trim_end(),
        "   press x again to stop process 3"
    );
    assert_eq!(hint.spans[1].look.fg, Some(Token::Error.into()));
}

#[test]
fn the_focused_row_stands_out_and_stays_in_view() {
    use crate::chrome::band::check::assert_selected;
    let mut rows: Vec<ProcessRow> = (1..=12)
        .map(|n| row(n, &format!("job {n}"), None, None))
        .collect();
    rows[10].waiting = true;
    let out = ps_rows(
        &view(&rows, 10, false),
        8,
        Layout::new(60, GlyphSet::Unicode),
    );
    let text = trimmed(&out);
    assert_eq!(text.len(), 8);
    let focused = text.iter().position(|l| l.contains("job 11")).unwrap();
    let span = out[focused]
        .spans
        .iter()
        .find(|s| s.text == "job 11")
        .unwrap();
    assert_eq!(span.look.fg, Some(Token::Text.into()));
    assert_selected(&out[focused], 60, "\u{203A}");
    assert!(text[focused - 1].starts_with("   \u{2818} 10"), "{text:?}");
    assert!(text.iter().all(|l| !l.contains("job 1 ")), "{text:?}");
    let top = ps_rows(
        &view(&rows, 0, false),
        8,
        Layout::new(60, GlyphSet::Unicode),
    );
    assert!(trimmed(&top)[2].contains("job 1"), "{:?}", trimmed(&top));
}

#[test]
fn a_list_with_an_agents_programs_heads_each_agents_rows() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let alone = trimmed(&ps_rows(&view(&sample(), 0, false), 20, layout));
    assert!(!alone.iter().any(|l| l.trim() == "main"), "{alone:?}");
    let of = |agent: &str, ended, row: ProcessRow| ProcessRow {
        agent: AgentPath::new(agent),
        agent_ended: ended,
        ..row
    };
    let rows = vec![
        row(3, "npm run dev", None, None),
        ProcessRow {
            opens: false,
            ..of("/lead/coder", false, row(3, "cargo test", None, None))
        },
        ProcessRow {
            stoppable: false,
            terminal: Some(TermSize { cols: 40, rows: 6 }),
            ..of("/lead/coder", false, row(4, "python3 -i", None, None))
        },
        of("/lead/review", true, row(2, "cargo clippy", exit(0), None)),
    ];
    let text = trimmed(&ps_rows(&view(&rows, 1, false), 20, layout));
    let headings = [&text[2], &text[4], &text[8]];
    let ended = "   /lead/review \u{b7} ended";
    assert_eq!(headings, ["   main", "   /lead/coder", ended], "{text:?}");
    let out = ps_rows(&view(&rows, 1, false), 20, layout);
    assert_eq!(out[2].spans[1].look.fg, Some(Token::TextMuted.into()));
    assert_eq!(
        text.last().unwrap().trim(),
        "x stop \u{b7} \u{2191}\u{2193} move \u{b7} esc close"
    );
    let short = trimmed(&ps_rows(&view(&rows, 2, false), 7, layout));
    assert!(short.contains(&"   /lead/coder".to_owned()), "{short:?}");
    let keys = "enter open \u{b7} \u{2191}\u{2193} move \u{b7} esc close";
    assert_eq!(short.last().unwrap().trim(), keys);
    let mut long = vec![row(3, "npm run dev", None, None)];
    long.extend((1..=15).map(|n| of("/w", false, row(n, "cargo test", exit(0), None))));
    for (focus, room, captions) in [
        (15, 10, vec![" History", "   /w"]),
        (15, 5, vec![" History", "   /w"]),
        (15, 4, vec!["   /w"]),
        (3, 6, vec![" History", "   /w"]),
    ] {
        let text = trimmed(&ps_rows(&view(&long, focus, false), room, layout));
        assert!(
            text.iter()
                .any(|l| l.contains(&format!("{focus}  cargo test"))),
            "{text:?}"
        );
        for (i, caption) in captions.iter().enumerate() {
            assert!(text[i + 1].starts_with(caption), "{text:?}");
        }
        assert!(text.len() <= room);
    }
    let mut repeated = rows.clone();
    repeated.push(of("/lead/coder", false, row(8, "done", exit(0), None)));
    let text = trimmed(&ps_rows(&view(&repeated, 4, false), 20, layout));
    assert_eq!(
        text.iter().filter(|l| l.trim() == "/lead/coder").count(),
        2,
        "{text:?}"
    );
    let named = |width| {
        let out = ps_rows(
            &view(&rows, 1, true),
            20,
            Layout::new(width, GlyphSet::Unicode),
        );
        trimmed(&out).last().unwrap().clone()
    };
    assert_eq!(
        named(80),
        "   press x again to stop process 3 of /lead/coder"
    );
    assert_eq!(named(40), "   press x again to stop process 3");
}

#[test]
fn too_little_room_draws_nothing_and_both_empty_groups_keep_their_zero_counts() {
    let rows = sample();
    let layout = Layout::new(60, GlyphSet::Unicode);
    assert!(ps_rows(&view(&rows, 0, false), PS_MIN_ROWS - 1, layout).is_empty());
    for room in [4, 5, 10] {
        let none = trimmed(&ps_rows(&view(&[], 0, false), room, layout));
        assert_eq!(none.len(), 4);
        assert!(none[1].starts_with(" Running") && none[1].ends_with('0'));
        assert!(none[2].starts_with(" History") && none[2].ends_with('0'));
        assert_eq!(none[3], "   esc close");
    }
    for end in [None, exit(0)] {
        let rows = vec![row(1, "job", end, None)];
        let text = trimmed(&ps_rows(&view(&rows, 0, false), 10, layout));
        assert_eq!(
            text.iter().filter(|l| l.ends_with('0')).count(),
            1,
            "{text:?}"
        );
    }
}

#[test]
fn ascii_draws_the_list_without_unicode() {
    let rows = sample();
    let out = ps_rows(&view(&rows, 0, false), 20, Layout::new(80, GlyphSet::Ascii));
    for l in trimmed(&out) {
        assert!(l.is_ascii(), "{l:?}");
    }
    assert!(trimmed(&out).last().unwrap().contains("up/down move"));
}

#[test]
fn the_list_wakes_for_the_comet_and_the_clock_only_while_something_runs() {
    let rows = sample();
    let due = ps_next_change(&view(&rows, 0, false)).unwrap();
    assert!(due > Duration::from_millis(300) && due <= Duration::from_secs(1));
    let done = vec![row(4, "cargo test", exit(0), None)];
    assert_eq!(ps_next_change(&view(&done, 0, false)), None);
}

#[test]
fn the_screen_view_draws_the_program_screen_through_theme_tokens() {
    let mut parser = vt100::Parser::new(nz(4), nz(20), 0);
    parser.process(b"\x1b[31mred\x1b[0m \x1b[1mbold\x1b[0m\r\n>>> ");
    let view = ScreenView {
        name: name(5),
        command: "python3 -i",
        screen: parser.screen(),
        ended: false,
        attachable: true,
        mode: ScreenMode::Watching,
    };
    let out = screen_rows(&view, 10, Layout::new(40, GlyphSet::Unicode));
    let text = trimmed(&out);
    assert_eq!(text[0], " Process 5  python3 -i");
    assert_eq!(text[1], " red bold");
    assert_eq!(text[2], " >>>");
    assert_eq!(text.len(), 6);
    assert_eq!(text[5], " a take over \u{b7} esc close");
    let red = out[1].spans.iter().find(|s| s.text == "red").unwrap();
    assert_eq!(red.look, Look::fg(Token::Error));
    let bold = out[1].spans.iter().find(|s| s.text == "bold").unwrap();
    assert_eq!(bold.look, Look::fg(Token::Text).bold());
    let caret = out[2].spans.iter().find(|s| s.look.reverse).unwrap();
    assert_eq!(caret.text, " ");
}

#[test]
fn a_program_background_colour_maps_to_the_surface_token() {
    let mut parser = vt100::Parser::new(nz(2), nz(20), 0);
    parser.process(b"\x1b[44mblue\x1b[0m \x1b[48;2;1;2;3mrgb\x1b[0m plain");
    let view = ScreenView {
        name: name(5),
        command: "htop",
        screen: parser.screen(),
        ended: true,
        attachable: false,
        mode: ScreenMode::Watching,
    };
    let out = screen_rows(&view, 10, Layout::new(40, GlyphSet::Unicode));
    let look = |text: &str| {
        out[1]
            .spans
            .iter()
            .find(|s| s.text == text)
            .unwrap_or_else(|| panic!("{text:?} in {:?}", out[1]))
            .look
    };
    assert_eq!(look("blue"), Look::fg(Token::Text).on(Token::Surface));
    assert_eq!(look("rgb"), Look::fg(Token::Text).on(Token::Surface));
    assert!(
        out[1]
            .spans
            .iter()
            .any(|s| s.text.contains("plain") && s.look.bg.is_none())
    );
}

#[test]
fn no_byte_from_the_program_reaches_the_terminal_as_an_escape() {
    let mut parser = vt100::Parser::new(nz(3), nz(30), 0);
    parser.process(b"a\x1b]0;title\x07b\x1b[2Jc\x9bd\x07e\x1bPq#0\x1b\\f");
    let view = ScreenView {
        name: name(1),
        command: "x",
        screen: parser.screen(),
        ended: true,
        attachable: false,
        mode: ScreenMode::Watching,
    };
    let out = screen_rows(&view, 10, Layout::new(40, GlyphSet::Unicode));
    for l in &out {
        for s in &l.spans {
            assert!(!s.text.chars().any(char::is_control), "{:?}", s.text);
        }
    }
    assert!(
        line_text(&out[0]).contains("ended"),
        "{:?}",
        line_text(&out[0])
    );
    assert!(out.iter().all(|l| !l.spans.iter().any(|s| s.look.reverse)));
}

#[test]
fn a_screen_taller_than_the_room_keeps_the_cursor_row_in_view() {
    let mut parser = vt100::Parser::new(nz(10), nz(10), 0);
    parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
    let view = ScreenView {
        name: name(2),
        command: "sh",
        screen: parser.screen(),
        ended: false,
        attachable: true,
        mode: ScreenMode::Watching,
    };
    let out = screen_rows(&view, 5, Layout::new(20, GlyphSet::Unicode));
    let text = trimmed(&out);
    assert_eq!(text[1..4], [" four", " five", " six"]);
    let top = screen_rows(&view, 11, Layout::new(20, GlyphSet::Unicode));
    assert_eq!(trimmed(&top)[1], " one");
    assert_eq!(top.len(), 11);
}

#[test]
fn wide_characters_take_their_two_cells_once() {
    let mut parser = vt100::Parser::new(nz(2), nz(10), 0);
    parser.process("\u{4F60}\u{597D}!".as_bytes());
    let view = ScreenView {
        name: name(2),
        command: "sh",
        screen: parser.screen(),
        ended: true,
        attachable: false,
        mode: ScreenMode::Watching,
    };
    let out = screen_rows(&view, 3, Layout::new(20, GlyphSet::Unicode));
    assert_eq!(trimmed(&out)[1], " \u{4F60}\u{597D}!");
}

#[test]
fn the_facts_stop_at_the_content_edge_on_wide_screens() {
    for (screen, end) in [(80, 79), (120, 119), (200, 119)] {
        let rows = sample();
        let out = ps_rows(
            &view(&rows, 0, false),
            20,
            Layout::new(screen, GlyphSet::Unicode),
        );
        let text = trimmed(&out);
        for at in [1, 3, 4] {
            assert_eq!(text[at].chars().count(), end, "{screen}: {:?}", text[at]);
        }
        assert!(
            out.iter()
                .all(|l| width(&line_text(l), WidthMode::Normal) == screen)
        );
    }
}

#[test]
fn typing_into_a_screen_names_the_way_out_and_warns_once() {
    let mut parser = vt100::Parser::new(nz(3), nz(30), 0);
    parser.process(b"Password: ");
    let mut view = ScreenView {
        name: name(5),
        command: "sudo true",
        screen: parser.screen(),
        ended: false,
        attachable: true,
        mode: ScreenMode::Typing {
            asked: None,
            warning: true,
        },
    };
    let layout = Layout::new(100, GlyphSet::Unicode);
    let out = screen_rows(&view, 6, layout);
    let text = trimmed(&out);
    assert!(text[0].starts_with(" Process 5  sudo true "), "{text:?}");
    assert!(text[0].ends_with("  f12 to go back"), "{text:?}");
    let look = |text: &str| out[0].spans.iter().find(|s| s.text == text).map(|s| s.look);
    assert_eq!(look("Process 5"), Some(Look::fg(Token::Accent).bold()));
    assert_eq!(look("sudo true"), Some(Look::fg(Token::TextMuted)));
    assert_eq!(look("f12 to go back"), Some(Look::fg(Token::Accent).bold()));
    assert_eq!(
        text[text.len() - 2..],
        [
            format!(" {} If", copy::ATTACH_WARNING),
            " f12 does nothing here, ctrl-] goes back too.".to_owned(),
        ]
    );
    assert!(!text[0].contains("esc"));
    let narrow = Layout::new(40, GlyphSet::Unicode);
    let wrapped = [
        " The agent that started it can read the",
        " program's output. Password prompts",
        " hide what you type. If f12 does",
        " nothing here, ctrl-] goes back too.",
    ];
    let text = trimmed(&screen_rows(&view, 6, narrow));
    assert_eq!(text.len(), 6, "{text:?}");
    assert_eq!(text[2..], wrapped);
    let text = trimmed(&screen_rows(&view, 4, narrow));
    assert_eq!(text[1], " Password:");
    assert_eq!(text[2..], wrapped[..2]);
    view.mode = ScreenMode::Typing {
        asked: Some("sudo password for apt install".into()),
        warning: false,
    };
    let out = screen_rows(&view, 6, layout);
    let text = trimmed(&out);
    assert!(
        text[0].starts_with(" Process 5  sudo password for apt install "),
        "{text:?}"
    );
    assert!(text[0].ends_with("  f12 to go back"), "{text:?}");
    let reason = out[0]
        .spans
        .iter()
        .find(|s| s.text.starts_with("sudo password"));
    assert_eq!(reason.map(|s| s.look), Some(Look::fg(Token::Text)));
    assert_eq!(text.last().map(String::as_str), Some(""));
    view.name = ProcessName::of(4, &AgentPath::new("/lead/coder"), &MAIN);
    let title = |width| {
        trimmed(&screen_rows(
            &view,
            6,
            Layout::new(width, GlyphSet::Unicode),
        ))[0]
            .clone()
    };
    assert!(title(100).starts_with(" Process 4 of /lead/coder  sudo password"));
    assert_eq!(title(40), " Process 4 of /lead/co…  f12 to go back");
}

#[test]
fn an_ended_screen_offers_no_typing() {
    let parser = vt100::Parser::new(nz(3), nz(30), 0);
    let mut view = ScreenView {
        name: name(5),
        command: "sh",
        screen: parser.screen(),
        ended: true,
        attachable: false,
        mode: ScreenMode::Watching,
    };
    let text = trimmed(&screen_rows(&view, 6, Layout::new(60, GlyphSet::Unicode)));
    assert_eq!(text.last().map(String::as_str), Some(" esc close"));
    view.ended = false;
    let text = trimmed(&screen_rows(&view, 6, Layout::new(60, GlyphSet::Unicode)));
    assert_eq!(text.last().map(String::as_str), Some(" esc close"));
}

#[test]
fn the_step_in_card_waits_for_typing_to_settle() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut view = StepInView {
        id: 5,
        reason: "sudo password to install build-essential",
        settling: false,
        selected: None,
        typing: false,
    };
    let card = [
        " ? Take over process 5?",
        "   sudo password to install build-essential",
        "",
        "   1 Yes",
        "   2 No",
        "",
        "   1\u{2013}2 choose \u{b7} \u{2191}\u{2193} move",
    ];
    assert_eq!(trimmed(&step_in_rows(&view, 10, layout)), card);
    assert_eq!(
        trimmed(&step_in_rows(&view, 3, layout)),
        [card[0], card[1], card[6]]
    );
    assert_eq!(
        trimmed(&step_in_rows(&view, 10, Layout::new(40, GlyphSet::Unicode)))[1..3],
        ["   sudo password to install", "   build-essential"]
    );
    view.typing = true;
    assert_eq!(
        trimmed(&step_in_rows(&view, 10, layout))[6],
        "   1\u{2013}2 choose"
    );
    view.typing = false;
    view.selected = Some(0);
    assert_eq!(
        trimmed(&step_in_rows(&view, 10, layout))[3..],
        [" \u{203a} 1 Yes", "   2 No", "", "   enter choose"]
    );
    view.settling = true;
    assert_eq!(
        trimmed(&step_in_rows(&view, 10, layout))[6],
        "   waiting for you to finish typing"
    );
}

#[test]
fn a_row_waiting_for_a_password_says_so_and_offers_typing() {
    let mut waiting = row(5, "sudo apt install x", None, None);
    waiting.terminal = Some(wisp_protocol::TermSize {
        cols: 100,
        rows: 30,
    });
    waiting.waiting = true;
    waiting.attachable = true;
    let rows = [waiting];
    let view = PsView {
        rows: &rows,
        focus: 0,
        confirm: false,
        here: &MAIN,
        t: Duration::ZERO,
        cx: FULL,
    };
    let text = trimmed(&ps_rows(&view, 10, Layout::new(100, GlyphSet::Unicode)));
    assert!(
        text[2].ends_with("waiting for a password \u{b7} a take over"),
        "{text:?}"
    );
    assert!(
        text.last().is_some_and(|l| l.contains("a take over")),
        "{text:?}"
    );
}

#[test]
fn a_process_from_before_the_restart_says_so_instead_of_its_clock() {
    let rows = vec![
        ProcessRow {
            restored: true,
            ..row(2, "make", exit(4), None)
        },
        ProcessRow {
            restored: true,
            ..row(
                3,
                "npm run dev",
                Some(ProcessEnd::Lost {
                    reason: wisp_protocol::LostReason::StoppedAtResume,
                }),
                None,
            )
        },
    ];
    let out = trimmed(&ps_rows(
        &view(&rows, 0, false),
        10,
        Layout::new(80, GlyphSet::Unicode),
    ));
    let two = out.iter().find(|l| l.contains("make")).unwrap();
    assert!(two.ends_with("exit 4 \u{b7} before the restart"), "{two:?}");
    let three = out.iter().find(|l| l.contains("npm run dev")).unwrap();
    assert!(
        three.ends_with("lost \u{b7} before the restart"),
        "{three:?}"
    );
    assert!(!out.iter().any(|l| l.contains("12:04")), "{out:?}");
}

#[test]
fn a_kept_row_says_it_keeps_running_and_an_unchecked_one_offers_no_stop() {
    let rows = vec![
        ProcessRow {
            kept: Keeps::Running,
            ..row(1, "python3 app.py", None, None)
        },
        ProcessRow {
            kept: Keeps::Running,
            restored: true,
            ..row(2, "redis-server", None, None)
        },
        ProcessRow {
            kept: Keeps::Unchecked,
            restored: true,
            pid: Some(4_321),
            stoppable: false,
            ..row(3, "qemu", None, None)
        },
    ];
    let layout = Layout::new(110, GlyphSet::Unicode);
    let text = trimmed(&ps_rows(&view(&rows, 0, false), 20, layout));
    assert!(
        text[2].ends_with("running, keeps running after Wisp exits \u{b7} 12m 04s"),
        "{text:?}"
    );
    assert!(
        text[3].ends_with("running since before the restart, keeps running after Wisp exits"),
        "{text:?}"
    );
    assert!(
        text[4].ends_with("may still be running as pid 4321 \u{b7} before the restart"),
        "{text:?}"
    );
    assert!(text.last().unwrap().contains("x stop"), "{text:?}");
    let unchecked = trimmed(&ps_rows(&view(&rows, 2, false), 20, layout));
    assert!(
        !unchecked.last().unwrap().contains("x stop"),
        "{unchecked:?}"
    );
    let narrow = trimmed(&ps_rows(
        &view(&rows, 1, false),
        20,
        Layout::new(40, GlyphSet::Unicode),
    ));
    assert!(narrow[3].contains("redis-server"), "{narrow:?}");
    assert!(!narrow[3].contains("running since"), "{narrow:?}");
    let stale = trimmed(&ps_rows(&view(&rows, 2, true), 20, layout));
    assert!(!stale.last().unwrap().contains("press x"), "{stale:?}");
}
