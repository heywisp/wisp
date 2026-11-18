use std::os::fd::OwnedFd;
use std::sync::mpsc;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, tcgetattr};

use super::*;
use crate::term::modes::{KeyboardFlags, ScrollBack};
use crate::term::tty::{Background, Placement};

const EVERYTHING: Modes = Modes {
    raw: true,
    alternate_screen: true,
    keyboard: Some(KeyboardFlags::DISAMBIGUATE),
    bracketed_paste: true,
    focus_reports: true,
    cursor_hidden: true,
    mouse: true,
    hover: true,
    alternate_scroll_off: Some(ScrollBack::Set),
};

fn pty() -> (OwnedFd, OwnedFd) {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    (master, slave)
}

fn drain(master: &OwnedFd) -> Vec<u8> {
    let mut out = vec![];
    let mut buf = [0u8; 4096];
    loop {
        let wait = Timespec {
            tv_sec: 0,
            tv_nsec: 50_000_000,
        };
        let mut fds = [PollFd::new(master, PollFlags::IN)];
        if poll(&mut fds, Some(&wait)).unwrap() == 0 {
            return out;
        }
        let n = rustix::io::read(master, &mut buf).unwrap();
        out.extend_from_slice(&buf[..n]);
    }
}

fn cooked(slave: &OwnedFd) -> bool {
    tcgetattr(slave)
        .unwrap()
        .local_modes
        .contains(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG)
}

const ISOLATED: &str = "WISP_TUI_GUARD_ISOLATED";
const THIS_TEST: &str =
    "term::guard::tests::a_panic_under_the_state_lock_restores_without_deadlock";

#[test]
fn a_panic_under_the_state_lock_restores_without_deadlock() {
    if std::env::var_os(ISOLATED).is_some() {
        panic_under_the_state_lock();
        return;
    }
    let run = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", THIS_TEST, "--test-threads=1", "--nocapture"])
        .env(ISOLATED, "1")
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("1 passed"),
        "{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

fn panic_under_the_state_lock() {
    signals::install().unwrap();
    let (master, slave) = pty();
    let mut terminal = start_on(&slave);
    terminal.apply(EVERYTHING).unwrap();
    assert!(!cooked(&slave));
    drain(&master);

    let shared = Arc::clone(&terminal.shared);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let held = shared.state();
        let caught = catch_unwind(AssertUnwindSafe(|| {
            std::panic::panic_any("injected panic under the state lock")
        }));
        drop(held);
        tx.send(caught.is_err()).ok();
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)),
        Ok(true),
        "the hook hung"
    );
    assert!(cooked(&slave));
    let out = String::from_utf8_lossy(&drain(&master)).into_owned();
    for seq in [
        "\x1b[?1000l",
        "\x1b[?1007h",
        "\x1b[?2004l",
        "\x1b[<1u",
        "\x1b[?1049l",
    ] {
        assert!(out.contains(seq), "{seq:?} missing in {out:?}");
    }

    let after = terminal.applied();
    assert!(after.is_empty(), "{after:?}");
    terminal.apply(EVERYTHING).unwrap();
    assert!(!cooked(&slave));
    drop(terminal);
    assert!(cooked(&slave));
}

#[test]
fn reentry_waits_for_every_report_and_a_death_wakes_it() {
    for dying in [false, true] {
        let (master, slave) = pty();
        let mut terminal = start_on(&slave);
        terminal.apply(EVERYTHING).unwrap();
        drain(&master);
        let shared = Arc::clone(&terminal.shared);
        drop(shared.suspend());
        let first = shared.panic_report();
        let second = shared.panic_report();
        drop(shared.restore_by(Instant::now() + Duration::from_secs(1), URGENT_PATIENCE));
        assert!(matches!(shared.resume(), Continued::Running));
        assert!(!terminal.is_suspended());
        assert!(cooked(&slave));
        let restored = String::from_utf8_lossy(&drain(&master)).into_owned();
        assert!(!restored.contains("\x1b[?1049h"), "{restored:?}");
        let (tx, rx) = mpsc::channel();
        let reapplying = std::thread::spawn(move || {
            tx.send(None).unwrap();
            let applied = terminal.apply(EVERYTHING).is_ok();
            tx.send(Some(applied)).unwrap();
            terminal
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(None));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        assert!(cooked(&slave));
        drop(first);
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        if dying {
            drop(
                shared.restore_for_death(Instant::now() + Duration::from_secs(1), URGENT_PATIENCE),
            );
        } else {
            drop(second);
        }
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(Some(!dying)));
        drop(reapplying.join().unwrap());
        assert!(cooked(&slave));
    }
}

fn start_on(slave: &OwnedFd) -> Terminal {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let tty = || {
            let input = rustix::io::dup(slave).map_err(io::Error::from)?;
            let output = rustix::io::dup(slave).map_err(io::Error::from)?;
            Ok(Tty::from_fds(input, output)?)
        };
        match Terminal::start(tty, TerminalOptions::default()) {
            Ok(entered) => return entered.terminal,
            Err(EnterError::AlreadyActive) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

const MOUSE_OFF_END: &str = "\x1b[?1000l";

fn set_size(slave: &OwnedFd, width: u16, height: u16) {
    rustix::termios::tcsetwinsize(
        slave,
        rustix::termios::Winsize {
            ws_row: height,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
}

fn shown_frame(terminal: &mut Terminal, master: &OwnedFd) {
    assert_eq!(
        terminal
            .write_frame(b"\x1b[1;1Hframe", CursorAfter::Hidden)
            .unwrap(),
        Written::Sent
    );
    drain(master);
}

fn death_restore(terminal: &Terminal, master: &OwnedFd) -> String {
    let shared = Arc::clone(&terminal.shared);
    drop(shared.restore_for_death(Instant::now() + Duration::from_millis(800), URGENT_PATIENCE));
    String::from_utf8_lossy(&drain(master)).into_owned()
}

#[test]
fn a_death_restore_resets_every_mode_on_both_passes_and_frames_are_refused() {
    let (master, slave) = pty();
    set_size(&slave, 10, 5);
    let mut terminal = start_on(&slave);
    terminal.apply(EVERYTHING).unwrap();
    shown_frame(&mut terminal, &master);
    let out = death_restore(&terminal, &master);
    assert!(cooked(&slave));
    assert_eq!(out.matches("\x1b[?1049l").count(), 1, "{out:?}");
    assert_eq!(out.matches(MOUSE_OFF_END).count(), 1, "{out:?}");
    assert!(!out.contains("\x1b[2K"), "{out:?}");
    assert!(!out.contains("\x1b[J"), "{out:?}");
    let refused = terminal.write_frame(b"late", CursorAfter::Hidden);
    assert!(refused.is_err());
    assert!(drain(&master).is_empty());
    drop(terminal);
    assert!(drain(&master).is_empty());
}

#[test]
fn a_restore_never_erases_or_moves_the_cursor() {
    let (master, slave) = pty();
    set_size(&slave, 10, 5);
    let mut terminal = start_on(&slave);
    terminal.apply(EVERYTHING).unwrap();
    shown_frame(&mut terminal, &master);
    drop(terminal.restore());
    let out = String::from_utf8_lossy(&drain(&master)).into_owned();
    assert!(out.contains("\x1b[?1049l"), "{out:?}");
    assert!(!moves_or_erases(&out), "{out:?}");
    assert!(cooked(&slave));
}

#[test]
fn a_suspend_leaves_the_alternate_screen_and_a_resume_reenters_it() {
    let (master, slave) = pty();
    set_size(&slave, 10, 5);
    let mut terminal = start_on(&slave);
    terminal.apply(EVERYTHING).unwrap();
    shown_frame(&mut terminal, &master);
    drop(terminal.shared.suspend());
    let out = String::from_utf8_lossy(&drain(&master)).into_owned();
    assert!(
        out.contains("\x1b[?1049l") && out.contains(MOUSE_OFF_END),
        "{out:?}"
    );
    assert!(cooked(&slave));
    assert_eq!(
        terminal.write_frame(b"held", CursorAfter::Hidden).unwrap(),
        Written::Suspended
    );
    assert!(matches!(terminal.shared.resume(), Continued::Running));
    let back = String::from_utf8_lossy(&drain(&master)).into_owned();
    assert!(back.contains("\x1b[?1049h"), "{back:?}");
    assert!(
        back.contains("\x1b[?1000h\x1b[?1002h\x1b[?1006h"),
        "{back:?}"
    );
    assert!(back.contains("\x1b[?1007l"), "{back:?}");
    assert!(!moves_or_erases(&back), "{back:?}");
    assert!(!cooked(&slave));
    assert_eq!(
        terminal.write_frame(b"drawn", CursorAfter::Hidden).unwrap(),
        Written::Sent
    );
    drop(terminal);
}

fn start_placed(slave: &OwnedFd, placement: Placement) -> Terminal {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let tty = || {
            let input = rustix::io::dup(slave).map_err(io::Error::from)?;
            let output = rustix::io::dup(slave).map_err(io::Error::from)?;
            Ok(Tty::from_fds(input, output)?.placed(placement))
        };
        match Terminal::start(tty, TerminalOptions::default()) {
            Ok(entered) => return entered.terminal,
            Err(EnterError::AlreadyActive) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_suspend_in_the_background_writes_but_keeps_termios() {
    let (master, slave) = pty();
    let background = Placement { foreground: false };
    let mut terminal = start_placed(&slave, background);
    terminal.apply(EVERYTHING).unwrap();
    drop(drain(&master));
    let failures = terminal.shared.suspend();
    let names: Vec<&str> = failures.iter().map(|f| f.change.name()).collect();
    assert_eq!(failures.len(), 1, "{names:?}");
    assert!(
        failures[0]
            .error
            .get_ref()
            .is_some_and(|e| e.is::<Background>()),
        "{}",
        failures[0]
    );
    let out = String::from_utf8_lossy(&drain(&master)).into_owned();
    assert!(out.contains("\x1b[?25h"), "{out:?}");
    assert!(out.contains("\x1b[?1049l"), "{out:?}");
    assert!(!cooked(&slave));
    drop(terminal);
}

#[test]
fn a_continue_in_the_background_after_an_unseen_stop_restores_and_stays_suspended() {
    let (master, slave) = pty();
    let background = Placement { foreground: false };
    let mut terminal = start_placed(&slave, background);
    terminal.apply(EVERYTHING).unwrap();
    drop(drain(&master));
    assert!(!terminal.is_suspended());
    assert!(matches!(terminal.shared.resume(), Continued::Background));
    assert!(terminal.is_suspended());
    assert!(!terminal.take_resumed());
    let out = String::from_utf8_lossy(&drain(&master)).into_owned();
    assert!(out.contains("\x1b[?25h"), "{out:?}");
    drop(terminal);
}

fn moves_or_erases(out: &str) -> bool {
    let bytes = out.as_bytes();
    bytes.windows(2).enumerate().any(|(i, w)| {
        w == b"\x1b[" && {
            let rest = &bytes[i + 2..];
            let end = rest
                .iter()
                .position(|b| b.is_ascii_alphabetic())
                .unwrap_or(rest.len());
            matches!(rest.get(end), Some(b'H' | b'J' | b'K' | b'A' | b'B'))
        }
    }) || out.contains("\r\n")
}

#[test]
fn a_restore_in_the_background_resets_the_modes_but_leaves_the_cursor_and_rows_to_the_shell() {
    for death in [false, true] {
        let (master, slave) = pty();
        set_size(&slave, 10, 5);
        let mut terminal = start_placed(&slave, Placement { foreground: false });
        terminal.apply(EVERYTHING).unwrap();
        drop(drain(&master));
        shown_frame(&mut terminal, &master);
        let out = if death {
            death_restore(&terminal, &master)
        } else {
            drop(terminal.shared.suspend());
            String::from_utf8_lossy(&drain(&master)).into_owned()
        };
        assert!(out.contains("\x1b[?25h"), "{death}: {out:?}");
        assert!(out.contains("\x1b[?1004l"), "{death}: {out:?}");
        let mouse = out
            .find(MOUSE_OFF_END)
            .unwrap_or_else(|| panic!("{death}: {out:?}"));
        let scroll = out
            .find("\x1b[?1007h")
            .unwrap_or_else(|| panic!("{death}: {out:?}"));
        let left = out
            .find("\x1b[?1049l")
            .unwrap_or_else(|| panic!("{death}: {out:?}"));
        assert!(mouse < scroll && scroll < left, "{death}: {out:?}");
        assert!(!out.contains("\x1b[?2004l"), "{death}: {out:?}");
        assert!(!moves_or_erases(&out), "{death}: {out:?}");
        assert!(!terminal.applied().bracketed_paste, "{death}");
        drop(terminal);
    }
}

const MOUSE_ON_ALL: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";

fn live_on(slave: &OwnedFd, master: &OwnedFd, mouse: bool) -> crate::term::Live {
    let terminal = start_on(slave);
    let live = crate::term::Live::new(
        terminal,
        Modes {
            mouse,
            hover: false,
            ..EVERYTHING
        },
    )
    .unwrap();
    drop(drain(master));
    live
}

fn mouse(live: &mut crate::term::Live, on: bool) {
    use crate::term::Console;
    let modes = Modes {
        mouse: on,
        ..live.modes()
    };
    live.set_modes(modes).unwrap();
}

fn said(master: &OwnedFd) -> String {
    String::from_utf8_lossy(&drain(master)).into_owned()
}

#[test]
fn mouse_off_set_live_writes_mouse_off_and_every_return_path_keeps_it_off() {
    use crate::term::Console;
    let (master, slave) = pty();
    set_size(&slave, 10, 5);
    let mut live = live_on(&slave, &master, true);
    mouse(&mut live, false);
    let out = said(&master);
    assert!(out.contains(MOUSE_OFF_END), "{out:?}");
    assert!(!out.contains("\x1b[?1007h"), "{out:?}");
    assert!(!live.modes().mouse);
    assert!(!live.terminal().modes().mouse);
    assert!(!live.terminal().applied().mouse);
    assert_eq!(
        live.terminal().applied().alternate_scroll_off,
        Some(ScrollBack::Set)
    );
    drop(live.terminal().restore());
    drop(drain(&master));
    live.reapply().unwrap();
    let out = said(&master);
    assert!(out.contains("\x1b[?1049h"), "{out:?}");
    assert!(!out.contains(MOUSE_ON_ALL), "{out:?}");
    drop(live.terminal().shared.suspend());
    drop(drain(&master));
    assert!(matches!(
        live.terminal().shared.resume(),
        Continued::Running
    ));
    let out = said(&master);
    assert!(out.contains("\x1b[?1049h"), "{out:?}");
    assert!(!out.contains(MOUSE_ON_ALL), "{out:?}");
    assert_eq!(live.hand_off(b"handed").unwrap(), Written::Sent);
    live.take_back().unwrap();
    let out = said(&master);
    assert!(!out.contains(MOUSE_ON_ALL), "{out:?}");
    assert!(!live.terminal().applied().mouse);
    drop(live);
}

#[test]
fn mouse_on_set_while_suspended_is_applied_on_resume() {
    let (master, slave) = pty();
    set_size(&slave, 10, 5);
    let mut live = live_on(&slave, &master, false);
    drop(live.terminal().shared.suspend());
    drop(drain(&master));
    mouse(&mut live, true);
    let out = said(&master);
    assert!(!out.contains(MOUSE_ON_ALL), "{out:?}");
    assert!(matches!(
        live.terminal().shared.resume(),
        Continued::Running
    ));
    let out = said(&master);
    assert!(out.contains(MOUSE_ON_ALL), "{out:?}");
    assert!(live.terminal().applied().mouse);
    drop(live);
}

#[test]
fn mouse_on_set_while_handed_off_waits_for_take_back() {
    use crate::term::Console;
    let (master, slave) = pty();
    set_size(&slave, 10, 5);
    let mut live = live_on(&slave, &master, false);
    assert_eq!(live.hand_off(b"handed").unwrap(), Written::Sent);
    drop(drain(&master));
    mouse(&mut live, true);
    let out = said(&master);
    assert!(!out.contains(MOUSE_ON_ALL), "{out:?}");
    assert!(!live.terminal().applied().mouse);
    assert!(!live.terminal().applied().alternate_screen);
    live.take_back().unwrap();
    let out = said(&master);
    assert!(out.contains(MOUSE_ON_ALL), "{out:?}");
    assert!(out.contains("\x1b[?1049h"), "{out:?}");
    assert!(live.modes().mouse);
    drop(live);
}

#[test]
fn mouse_off_set_while_handed_off_keeps_it_off_after_take_back() {
    use crate::term::Console;
    let (master, slave) = pty();
    set_size(&slave, 10, 5);
    let mut live = live_on(&slave, &master, true);
    assert_eq!(live.hand_off(b"handed").unwrap(), Written::Sent);
    drop(drain(&master));
    mouse(&mut live, false);
    live.take_back().unwrap();
    let out = said(&master);
    assert!(out.contains("\x1b[?1049h"), "{out:?}");
    assert!(!out.contains(MOUSE_ON_ALL), "{out:?}");
    assert!(!live.terminal().applied().mouse);
    drop(live);
}
