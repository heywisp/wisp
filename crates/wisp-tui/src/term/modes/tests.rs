use super::*;

const EVERYTHING: Modes = Modes {
    raw: true,
    alternate_screen: true,
    keyboard: Some(KeyboardFlags::DISAMBIGUATE),
    bracketed_paste: true,
    focus_reports: true,
    cursor_hidden: true,
    mouse: true,
    alternate_scroll_off: Some(ScrollBack::Set),
};

#[derive(Default)]
struct Recorder {
    log: Vec<String>,
    fail: Vec<&'static str>,
}

impl Recorder {
    fn step(&mut self, name: String) -> io::Result<()> {
        let failing = self.fail.iter().any(|f| name.contains(f));
        self.log.push(name);
        if failing {
            Err(io::Error::other("tty gone"))
        } else {
            Ok(())
        }
    }
}

impl Port for Recorder {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.step(String::from_utf8_lossy(bytes).into_owned())
    }

    fn termios(&mut self, raw: bool) -> io::Result<()> {
        self.step(format!("termios raw={raw}"))
    }
}

#[test]
fn restore_runs_in_the_documented_order() {
    let plan = restore_plan(&EVERYTHING);
    let names: Vec<&str> = plan.iter().map(|c| c.name()).collect();
    assert_eq!(names, RESTORE_ORDER);
}

#[test]
fn restore_writes_the_expected_bytes_in_order() {
    let mut port = Recorder::default();
    let mut state = EVERYTHING;
    let failures = execute(&mut port, &restore_plan(&EVERYTHING), &mut state);
    assert!(failures.is_empty());
    assert_eq!(
        port.log,
        [
            "\x1b\\\x1b]8;;\x1b\\",
            "\x1b[?2026l",
            "\x1b[0m",
            "\x1b[?25h",
            "\x1b[?7h",
            "\x1b[?1006l\x1b[?1002l\x1b[?1000l",
            "\x1b[?1007h",
            "\x1b[?1004l",
            "\x1b[?2004l",
            "\x1b[<1u",
            "\x1b[?1049l",
            "termios raw=false",
        ]
    );
    assert_eq!(state, Modes::COOKED);
    assert!(state.is_empty());
}

#[test]
fn alternate_scroll_goes_back_only_where_it_was_set() {
    let untouched = Modes {
        alternate_scroll_off: Some(ScrollBack::Untouched),
        ..EVERYTHING
    };
    let mut port = Recorder::default();
    let mut state = untouched;
    assert!(execute(&mut port, &restore_plan(&untouched), &mut state).is_empty());
    assert!(
        !port.log.iter().any(|l| l.contains("1007")),
        "{:?}",
        port.log
    );
    assert!(state.is_empty());
    assert_eq!(
        Change::AlternateScrollOff(ScrollBack::Untouched)
            .bytes()
            .as_deref(),
        Some("\x1b[?1007l")
    );
}

#[test]
fn restore_keeps_going_when_a_step_fails() {
    let mut port = Recorder {
        fail: vec!["\x1b[?25h", "\x1b[<1u", "\x1b[?1049l", "\x1b[?1000l"],
        ..Recorder::default()
    };
    let mut state = EVERYTHING;
    let failures = execute(&mut port, &restore_plan(&EVERYTHING), &mut state);
    assert_eq!(port.log.len(), RESTORE_ORDER.len());
    assert_eq!(
        port.log.last().map(String::as_str),
        Some("termios raw=false")
    );
    let failed: Vec<&str> = failures.iter().map(|f| f.change.name()).collect();
    assert_eq!(
        failed,
        [
            "show the cursor",
            "disable mouse reports",
            "pop keyboard flags",
            "leave the alternate screen"
        ]
    );
    assert!(state.cursor_hidden && state.keyboard.is_some() && state.alternate_screen);
    assert!(state.mouse);
    assert!(!state.raw && !state.bracketed_paste && state.alternate_scroll_off.is_none());

    port.fail.clear();
    port.log.clear();
    let retry = restore_plan(&state);
    assert!(execute(&mut port, &retry, &mut state).is_empty());
    assert!(port.log.contains(&"\x1b[<1u".to_owned()));
    assert!(port.log.contains(&"\x1b[?1049l".to_owned()));
    assert!(port.log.contains(&MOUSE_OFF.to_owned()));
    assert!(state.is_empty());
}

#[test]
fn restore_of_the_empty_state_only_resets_harmless_things() {
    let names: Vec<&str> = restore_plan(&Modes::COOKED)
        .iter()
        .map(|c| c.name())
        .collect();
    assert_eq!(
        names,
        [
            "end any link",
            "end synchronized update",
            "reset SGR",
            "show the cursor",
            "enable autowrap"
        ]
    );
}

#[test]
fn entering_is_the_reverse_of_restoring() {
    let enter: Vec<&str> = transition(&Modes::COOKED, &EVERYTHING)
        .iter()
        .map(|c| c.name())
        .collect();
    assert_eq!(
        enter,
        [
            "enter raw mode",
            "enter the alternate screen",
            "push keyboard flags",
            "enable bracketed paste",
            "enable focus reports",
            "enable mouse reports",
            "turn alternate scroll off",
            "hide the cursor",
        ]
    );
    let mut port = Recorder::default();
    let mut state = Modes::COOKED;
    execute(
        &mut port,
        &transition(&Modes::COOKED, &EVERYTHING),
        &mut state,
    );
    assert_eq!(state, EVERYTHING);
    assert!(port.log.contains(&"\x1b[>1u".to_owned()));
    assert!(
        port.log
            .contains(&"\x1b[?1000h\x1b[?1002h\x1b[?1006h".to_owned())
    );
    assert!(
        !port.log.iter().any(|l| l.contains("1003")),
        "{:?}",
        port.log
    );
    assert!(port.log.contains(&"\x1b[?1007l".to_owned()));
}

#[test]
fn a_failed_enable_still_counts_as_on_so_restore_undoes_it() {
    for (fail, to) in [
        (
            "\x1b[?2004h",
            Modes {
                bracketed_paste: true,
                ..Modes::COOKED
            },
        ),
        (
            "\x1b[?1000h",
            Modes {
                mouse: true,
                ..Modes::COOKED
            },
        ),
    ] {
        let mut port = Recorder {
            fail: vec![fail],
            ..Recorder::default()
        };
        let mut state = Modes::COOKED;
        let failures = execute(&mut port, &transition(&Modes::COOKED, &to), &mut state);
        assert_eq!(failures.len(), 1);
        assert_eq!(state, to);
        assert!(restore_plan(&state).len() > 4, "{fail:?}");
    }
}

#[test]
fn transitions_change_only_what_differs() {
    let from = Modes {
        raw: true,
        keyboard: Some(KeyboardFlags(1)),
        mouse: true,
        ..Modes::COOKED
    };
    let to = Modes {
        raw: true,
        keyboard: Some(KeyboardFlags(3)),
        mouse: false,
        ..Modes::COOKED
    };
    assert_eq!(
        transition(&from, &to),
        [
            Change::Mouse(false),
            Change::Keyboard(None),
            Change::Keyboard(Some(KeyboardFlags(3))),
        ]
    );
    assert!(transition(&to, &to).is_empty());
}

#[test]
fn the_lock_free_mirror_keeps_what_restore_needs() {
    for bits in 0..512u16 {
        let modes = Modes::from_bits(bits);
        let canonical = if bits & (1 << 7) == 0 {
            bits & !(1 << 8)
        } else {
            bits
        };
        assert_eq!(modes.bits(), canonical);
        let counted = canonical & !(1 << 8);
        assert_eq!(
            restore_plan(&modes).len(),
            5 + counted.count_ones() as usize - usize::from(modes.cursor_hidden)
        );
    }
    assert_eq!(
        restore_plan(&Modes::from_bits(EVERYTHING.bits())),
        restore_plan(&EVERYTHING)
    );
}

#[test]
fn applying_stops_between_changes_once_the_process_is_dying() {
    let mut port = Recorder::default();
    let mut state = Modes::COOKED;
    let plan = transition(&Modes::COOKED, &EVERYTHING);
    assert!(plan.len() > 3);
    let dying = std::cell::Cell::new(false);
    let result = execute_until(&mut port, &plan, &mut state, || {
        let now = dying.get();
        dying.set(true);
        now
    });
    assert!(matches!(result, Err(Halted::Dying)));
    assert_eq!(port.log.len(), 1);
    assert_eq!(state.bits().count_ones(), 1);
    let mut port = Recorder::default();
    let mut state = Modes::COOKED;
    let all = execute_until(&mut port, &plan, &mut state, || false);
    assert!(matches!(all, Ok(ref f) if f.is_empty()));
    assert_eq!(state, EVERYTHING);
}

#[test]
fn the_mouse_goes_off_before_the_alternate_screen_is_left() {
    let plan = restore_plan(&EVERYTHING);
    let at = |c: Change| plan.iter().position(|x| *x == c).unwrap();
    assert!(at(Change::Mouse(false)) < at(Change::AlternateScrollBack(ScrollBack::Set)));
    assert!(at(Change::AlternateScrollBack(ScrollBack::Set)) < at(Change::FocusReports(false)));
    assert!(at(Change::Keyboard(None)) < at(Change::AlternateScreen(false)));
    assert!(at(Change::AlternateScreen(false)) < at(Change::Termios { raw: false }));
}
