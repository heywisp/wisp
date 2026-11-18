use super::*;
use crate::testing::texts;

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn plain_step(text: &mut String, event: &Event) -> Step {
    step(text, event, Echo::Plain)
}

fn plain(c: char) -> Event {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

fn all_held(event: &Event) -> Result<Picked, PromptError> {
    let rows = 3;
    let mut pick = Pick {
        rows,
        held: vec![true; rows],
        focus: Some(1),
    };
    pick_step(&mut pick, event)
}

#[test]
fn typed_characters_build_the_answer_and_backspace_erases() {
    let mut text = String::new();
    assert_eq!(plain_step(&mut text, &plain('a')), Step::Typed("a".into()));
    assert_eq!(
        plain_step(&mut text, &key(KeyCode::Char('B'), KeyModifiers::SHIFT)),
        Step::Typed("B".into())
    );
    assert_eq!(
        plain_step(&mut text, &key(KeyCode::Backspace, KeyModifiers::NONE)),
        Step::Erased
    );
    assert_eq!(text, "a");
    assert_eq!(
        plain_step(&mut text, &key(KeyCode::Char('h'), KeyModifiers::CONTROL)),
        Step::Erased
    );
    assert_eq!(
        plain_step(&mut text, &key(KeyCode::Backspace, KeyModifiers::NONE)),
        Step::Ignore
    );
    assert_eq!(text, "");
}

#[test]
fn control_c_and_control_d_cancel_and_enter_finishes() {
    for c in ['c', 'd'] {
        let mut text = String::from("partial");
        assert_eq!(
            plain_step(&mut text, &key(KeyCode::Char(c), KeyModifiers::CONTROL)),
            Step::Cancel
        );
    }
    let mut text = String::new();
    assert_eq!(
        plain_step(&mut text, &key(KeyCode::Enter, KeyModifiers::NONE)),
        Step::Done
    );
    assert_eq!(
        plain_step(&mut text, &key(KeyCode::Char('m'), KeyModifiers::CONTROL)),
        Step::Done
    );
    for cancel in [
        key(KeyCode::Esc, KeyModifiers::NONE),
        key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        key(KeyCode::Char('d'), KeyModifiers::CONTROL),
    ] {
        assert!(
            matches!(all_held(&cancel), Err(PromptError::Cancelled)),
            "{cancel:?}"
        );
    }
    for enter in [
        key(KeyCode::Enter, KeyModifiers::NONE),
        key(KeyCode::Char('m'), KeyModifiers::CONTROL),
        key(KeyCode::Char('j'), KeyModifiers::CONTROL),
    ] {
        assert!(matches!(all_held(&enter), Ok(Picked::Took(1))), "{enter:?}");
    }
    assert!(matches!(all_held(&plain('3')), Ok(Picked::Moved)));
}

#[test]
fn other_keys_and_releases_are_ignored_and_pastes_lose_control_characters() {
    let mut text = String::new();
    for event in [
        key(KeyCode::Up, KeyModifiers::NONE),
        key(KeyCode::Char('x'), KeyModifiers::ALT),
        key(KeyCode::Char('u'), KeyModifiers::CONTROL),
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('z'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
        Event::FocusGained,
    ] {
        assert_eq!(plain_step(&mut text, &event), Step::Ignore, "{event:?}");
    }
    for event in [
        key(KeyCode::Left, KeyModifiers::NONE),
        plain('x'),
        plain('0'),
        plain('4'),
        key(KeyCode::Char('2'), KeyModifiers::ALT),
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Down,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
        Event::Paste("2".into()),
        Event::FocusGained,
    ] {
        assert!(matches!(all_held(&event), Ok(Picked::Not)), "{event:?}");
    }
    assert_eq!(
        plain_step(&mut text, &Event::Paste("sk-\u{1b}[0ab\n".into())),
        Step::Typed("sk-[0ab".into())
    );
    assert_eq!(text, "sk-[0ab");
}

#[test]
fn a_paste_replaces_the_secret_and_typing_appends() {
    let mut text = String::new();
    assert_eq!(
        step(&mut text, &Event::Paste("c#s".into()), Echo::Masked),
        Step::Replaced
    );
    assert_eq!(
        step(&mut text, &Event::Paste("c#s\r\n".into()), Echo::Masked),
        Step::Replaced
    );
    assert_eq!(text, "c#s");
    assert_eq!(
        step(&mut text, &plain('x'), Echo::Masked),
        Step::Typed("x".into())
    );
    assert_eq!(text, "c#sx");
    assert_eq!(
        step(&mut text, &Event::Paste("new".into()), Echo::Masked),
        Step::Replaced
    );
    assert_eq!(text, "new");
    let mut plain_text = String::from("a");
    step(&mut plain_text, &Event::Paste("b".into()), Echo::Plain);
    assert_eq!(plain_text, "ab");
}

#[test]
fn the_echo_stays_on_the_question_line_when_it_has_room() {
    assert_eq!(
        echo_line("Code: ", 80),
        EchoLine {
            own_line: false,
            room: 73
        }
    );
    assert_eq!(
        echo_line("Code: ", 40),
        EchoLine {
            own_line: false,
            room: 33
        }
    );
    assert_eq!(
        echo_line("Paste the API key; it stays hidden: ", 40),
        EchoLine {
            own_line: true,
            room: 39
        }
    );
    assert_eq!(
        echo_line("Paste the API key; it stays hidden: ", 80),
        EchoLine {
            own_line: false,
            room: 43
        }
    );
    assert_eq!(echo_line("Code: ", 0).room, 73);
}

fn true_colour_caps() -> Caps {
    use crate::term::caps::{Env, Flags, Streams};
    Caps::resolve(
        &Env::from_pairs([("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]),
        Streams {
            stdin_tty: true,
            stdout_tty: false,
            stderr_tty: true,
        },
        Flags::default(),
        None,
    )
}

#[test]
fn the_masked_echo_fits_forty_columns() {
    crossterm::style::force_color_output(true);
    let caps = true_colour_caps();
    let theme =
        wisp_render::theme::Theme::new(caps.mode.value, wisp_render::theme::Depth::TrueColor);
    let secret = theme.rgb(Token::Secret);
    let colour = format!("38;2;{};{};{}", secret.r, secret.g, secret.b);
    let code = format!("ye5b{}he2E", "k".repeat(79));
    for (question, value) in [
        ("Code: ", code.clone()),
        (
            "Paste the API key; it stays hidden: ",
            format!("sk-ant-api03-{}", "x".repeat(95)),
        ),
    ] {
        let line = echo_line(question, 40);
        for ellipsis in ["\u{2026}", "..."] {
            let shown = wisp_protocol::masked(&value, line.room, ellipsis);
            let echo = String::from_utf8(masked_echo(&value, line.room, ellipsis, &caps)).unwrap();
            assert!(echo.contains(&colour) && echo.contains(&shown), "{echo:?}");
            assert!(echo.ends_with(RESET), "{echo:?}");
            let lead = if line.own_line { 0 } else { question.len() };
            assert!(lead + shown.chars().count() < 40, "{question}{shown}");
            assert!(!shown.contains(&value[6..20]));
        }
    }
}

#[test]
fn control_u_clears_a_filled_answer() {
    let mut text = "claude".to_owned();
    assert_eq!(
        plain_step(&mut text, &key(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        Step::Replaced
    );
    assert_eq!(text, "");
}

fn picker_rows() -> Vec<ListRow> {
    vec![
        (vec!["Claude".into()], None),
        (
            vec!["Sign in again to".into(), "claude-subscription".into()],
            None,
        ),
        (vec!["API key".into()], None),
    ]
}

#[test]
fn the_list_marks_only_the_marked_row_with_faint_digits_and_the_key_line_while_picking() {
    let glyphs = Glyphs::of(wisp_render::theme::glyphs::GlyphSet::Unicode);
    let mark = "\u{203a}";
    let keys = "enter choose \u{b7} esc cancel".to_owned();
    let rows = picker_rows();
    let marked = |row: usize| format!("{mark} {}  ", row + 1);
    assert_eq!(
        texts(&list_lines(&rows, Shown::Picking(Some(1)), &glyphs)),
        [
            "  1  Claude".to_owned(),
            format!("{}Sign in again to", marked(1)),
            "     claude-subscription".to_owned(),
            "  3  API key".to_owned(),
            keys.clone(),
        ]
    );
    let none = texts(&list_lines(&rows, Shown::Picking(None), &glyphs));
    assert!(none.iter().all(|line| !line.starts_with(mark)), "{none:?}");
    assert_eq!(none.last(), Some(&keys));
    let lines = list_lines(&rows, Shown::Picking(Some(0)), &glyphs);
    for (line, digit) in [(&lines[0], "1"), (&lines[1], "2"), (&lines[3], "3")] {
        let faint = line.spans.iter().find(|span| span.text == digit);
        assert_eq!(
            faint.map(|span| span.look),
            Some(Token::TextFaint.into()),
            "{line:?}"
        );
    }
    assert_eq!(lines[0].spans[0].look, Token::Accent.into());
    let label = |line: &StyledLine| line.spans.last().map(|span| span.look);
    let plain = Look::default();
    assert_eq!(label(&lines[0]), Some(plain.bold()));
    for line in &lines[1..4] {
        assert_eq!(label(line), Some(plain), "{line:?}");
    }
    let many: Vec<ListRow> = (0..10).map(|n| (vec![format!("row {n}")], None)).collect();
    let numbered_rows = texts(&list_lines(
        &many,
        Shown::Numbered,
        &Glyphs::of(wisp_render::theme::glyphs::GlyphSet::Ascii),
    ));
    assert_eq!(numbered_rows[9], "  10  row 9");
    let picking = texts(&list_lines(
        &many,
        Shown::Picking(Some(9)),
        &Glyphs::of(wisp_render::theme::glyphs::GlyphSet::Ascii),
    ));
    assert_eq!(picking[0], "      row 0");
    assert_eq!(picking[9], ">     row 9");
}

#[test]
fn the_frame_goes_to_stderr_redraws_in_place_and_clears_each_line() {
    let caps = true_colour_caps();
    let lines = list_lines(
        &picker_rows(),
        Shown::Picking(Some(0)),
        &Glyphs::of(caps.glyphs.value),
    );
    let first = String::from_utf8(frame(&lines, &caps, 0, 80)).unwrap();
    assert!(first.starts_with('\r'), "{first:?}");
    assert!(first.contains('\u{1b}'), "{first:?}");
    assert_eq!(first.matches("\u{1b}[K").count(), lines.len(), "{first:?}");
    let again = String::from_utf8(frame(&lines, &caps, 5, 80)).unwrap();
    assert!(again.starts_with("\u{1b}[5A\r"), "{again:?}");
}

#[test]
fn delegated_interrupts_and_hangups_leave_the_command_running() {
    delegate_interrupts_and_hangups();
    on_signal(SIGINT);
    on_signal(SIGHUP);
    assert!(DELEGATED.load(Ordering::SeqCst));
}
