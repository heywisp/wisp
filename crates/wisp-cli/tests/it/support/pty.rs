use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, Winsize, tcgetattr};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::copy::PLACEHOLDER;
use wisp_tui::app::config_screen::CHECK_LABEL;

use super::screen;
use super::server::{Server, is_count};
use super::{WISP, home::command_in};

pub(crate) const READY: &str = PLACEHOLDER.split_at(1).1;
pub(crate) const PATIENCE: Duration = Duration::from_secs(20);
const EXIT_WAIT: Duration = Duration::from_secs(30);

pub(crate) enum Stdout {
    Tty,
    Piped,
}

pub(crate) struct Pty {
    pub(crate) master: File,
    pub(crate) screen: Arc<Mutex<Vec<u8>>>,
    pub(crate) child: Child,
    pub(crate) pause: Duration,
    parser: Arc<Mutex<vt100::Parser<screen::Updates>>>,
    reading: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

fn setsid_available() -> bool {
    Command::new("setsid")
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub(crate) fn usable() -> bool {
    if setsid_available() {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "setsid is not usable, and CI must not skip a terminal test"
    );
    eprintln!("skipped: setsid is not usable");
    false
}

pub(crate) fn run(cmd: &mut Command, text: &str, expected: &str, key: &str) -> String {
    let mut pty = Pty::start(cmd, 35, 110, Stdout::Tty);
    pty.wait_shown(READY);
    pty.send(text.replace('\n', "\r").as_bytes());
    pty.wait_shown(expected);
    pty.wait_idle();
    let shown = pty.shown();
    std::thread::sleep(Duration::from_millis(150));
    pty.send(b"\x04");
    let (status, _) = pty.finish();
    assert!(status.success(), "{status}\n{shown}");
    assert!(!shown.contains(key));
    shown
}

pub(crate) fn wait_until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

pub(crate) fn wait_per_step(server: &Server, mut done: impl FnMut() -> bool) -> bool {
    let steps = || server.heads().iter().filter(|h| !is_count(h)).count();
    let mut seen = steps();
    while wait_until(PATIENCE, || done() || steps() > seen) {
        if done() {
            return true;
        }
        seen = steps();
    }
    false
}

pub(crate) fn wait_for_exit(child: &mut Child) -> ExitStatus {
    let mut status = None;
    wait_until(EXIT_WAIT, || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    status.unwrap_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        panic!("wisp did not exit");
    })
}

fn chosen(shown: &str) -> &str {
    let prompt = Glyphs::of(GlyphSet::Unicode).prompt;
    shown
        .lines()
        .find_map(|line| {
            line.split(CHECK_LABEL)
                .skip(1)
                .map(str::trim_start)
                .find(|rest| rest.starts_with(prompt))
        })
        .unwrap_or("")
}

fn counter(row: &str) -> (usize, usize) {
    match row.split_whitespace().rev().take(3).collect::<Vec<_>>()[..] {
        [n, "of", k] => (k.parse().unwrap_or(0), n.parse().unwrap_or(0)),
        _ => (0, 0),
    }
}

impl Pty {
    pub(crate) fn command(home: &Path) -> Command {
        let mut command = command_in(home, "setsid");
        command
            .args(["--ctty", "--wait", WISP])
            .env("TERM", "xterm-256color");
        command
    }

    pub(crate) fn start(command: &mut Command, rows: u16, cols: u16, stdout: Stdout) -> Pty {
        let master =
            openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let path = ptsname(&master, Vec::new()).unwrap();
        let slave = rustix::fs::open(
            path.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        rustix::termios::tcsetwinsize(
            &slave,
            Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        let slave = File::from(slave);
        let stdout = match stdout {
            Stdout::Tty => Stdio::from(slave.try_clone().unwrap()),
            Stdout::Piped => Stdio::piped(),
        };
        let child = command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(stdout)
            .stderr(Stdio::from(slave))
            .spawn()
            .unwrap();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let master = File::from(master);
        let screen = Arc::new(Mutex::new(Vec::new()));
        let parser = Arc::new(Mutex::new(screen::parsed(&[], rows, cols)));
        let mut reader = master.try_clone().unwrap();
        let sink = Arc::clone(&screen);
        let feed = Arc::clone(&parser);
        let reading = Arc::new(AtomicBool::new(true));
        let active = Arc::clone(&reading);
        let reader = std::thread::spawn(move || {
            use rustix::event::{PollFd, PollFlags, Timespec, poll};
            let wait = Timespec {
                tv_sec: 0,
                tv_nsec: 50_000_000,
            };
            let mut buf = [0u8; 4096];
            while active.load(Ordering::SeqCst) {
                let mut fds = [PollFd::new(&reader, PollFlags::IN)];
                if poll(&mut fds, Some(&wait)).unwrap_or(0) == 0 {
                    continue;
                }
                let Ok(n) = reader.read(&mut buf) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                let mut parser = feed.lock().unwrap();
                let mut bytes = sink.lock().unwrap();
                screen::process(&mut parser, &buf[..n]);
                bytes.extend_from_slice(&buf[..n]);
            }
        });
        Pty {
            master,
            screen,
            child,
            pause: Duration::ZERO,
            parser,
            reading,
            reader: Some(reader),
        }
    }

    pub(crate) fn disconnect(&mut self) {
        self.reading.store(false, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
        self.master = File::open("/dev/null").unwrap();
    }

    fn dump(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()).into_owned()
    }

    pub(crate) fn screen(&self) -> String {
        let parser = self.parser.lock().unwrap();
        let bytes = self.screen.lock().unwrap();
        String::from_utf8_lossy(screen::complete(&parser, &bytes)).into_owned()
    }

    pub(crate) fn shown(&self) -> String {
        screen::contents(&self.parser.lock().unwrap())
    }

    pub(crate) fn saw(&self, needle: impl AsRef<[u8]>) -> bool {
        let needle = needle.as_ref();
        let raw = {
            let parser = self.parser.lock().unwrap();
            let bytes = self.screen.lock().unwrap();
            screen::complete(&parser, &bytes)
                .windows(needle.len())
                .any(|w| w == needle)
        };
        raw || std::str::from_utf8(needle).is_ok_and(|text| self.shown().contains(text))
    }

    pub(crate) fn wait_for(&self, needle: impl AsRef<[u8]>) {
        let needle = needle.as_ref();
        if !wait_until(PATIENCE, || self.saw(needle)) {
            let needle = String::from_utf8_lossy(needle);
            panic!("never saw {needle:?}: {:?}\n{}", self.dump(), self.shown());
        }
    }

    #[allow(dead_code, reason = "used by Linux-only first_run, endpoint_prompts")]
    pub(crate) fn wait_for_count(&self, needle: &str, count: usize) {
        let seen = |text: String| text.matches(needle).count() >= count;
        if !wait_until(PATIENCE, || seen(self.screen()) || seen(self.shown())) {
            panic!(
                "never saw {needle:?} {count} times: {:?}\n{}",
                self.dump(),
                self.shown()
            );
        }
    }

    pub(crate) fn wait_screen(&self, what: &str, mut done: impl FnMut(&str) -> bool) -> String {
        let mut shown = String::new();
        let found = wait_until(PATIENCE, || {
            shown = self.shown();
            done(&shown)
        });
        assert!(found, "never showed {what:?}: {shown}\n{:?}", self.dump());
        shown
    }

    pub(crate) fn wait_shown(&self, needle: &str) -> String {
        self.wait_screen(needle, |shown| shown.contains(needle))
    }

    pub(crate) fn wait_idle(&self) {
        let mut seen = false;
        self.wait_screen("the turn's end", |shown| {
            let idle = screen::idle(shown);
            let twice = seen && idle;
            seen = idle;
            twice
        });
    }

    pub(crate) fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
        std::thread::sleep(self.pause);
    }

    #[allow(dead_code, reason = "used by Linux-only first_run, endpoint_prompts")]
    pub(crate) fn cooked(&self) -> bool {
        let termios = tcgetattr(&self.master).unwrap();
        termios
            .local_modes
            .contains(LocalModes::ECHO | LocalModes::ICANON)
    }

    pub(crate) fn close_settings(&mut self) {
        for _ in 0..3 {
            self.send(b"\x1b");
            if wait_until(Duration::from_secs(2), || self.shown().contains(READY)) {
                return;
            }
        }
        panic!("/config never closed: {}", self.shown());
    }

    pub(crate) fn switch_tool(&mut self, tool: &str, from: &str) {
        self.master.write_all(b"/config tools\r").unwrap();
        self.wait_shown("Tools: which tools");
        self.master.write_all(b"\r").unwrap();
        let mut shown = self.wait_shown("enter save");
        let item = format!("{from} {tool}");
        while !chosen(&shown).contains(&item) {
            let (k, n) = counter(chosen(&shown));
            assert!(k < n, "never reached {item}: {shown}");
            self.master.write_all(b"\x1b[C").unwrap();
            let what = format!("{} of {n} on the way to {item}", k + 1);
            shown = self.wait_screen(&what, |s| counter(chosen(s)) == (k + 1, n));
        }
        self.master.write_all(b" \r").unwrap();
        self.wait_shown("enter choose");
        self.close_settings();
    }

    pub(crate) fn switch_search(&mut self) {
        self.master.write_all(b"/config web search\r").unwrap();
        self.wait_shown("Web search: lets the model");
        self.master.write_all(b"\r").unwrap();
        self.wait_shown("pending");
        self.close_settings();
    }

    pub(crate) fn finish(mut self) -> (ExitStatus, String) {
        let exited = wait_until(EXIT_WAIT, || self.child.try_wait().unwrap().is_some());
        assert!(exited, "wisp did not exit: {}", self.shown());
        let status = self.child.wait().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let mut out = vec![];
        if let Some(mut stdout) = self.child.stdout.take() {
            stdout.read_to_end(&mut out).unwrap();
        }
        (status, String::from_utf8_lossy(&out).into_owned())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.reading.store(false, Ordering::SeqCst);
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
