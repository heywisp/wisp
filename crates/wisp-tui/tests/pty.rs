#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any pty or process error"
)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, Signal, WaitOptions, WaitStatus, kill_process, waitpid};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{InputModes, LocalModes, OutputModes, Termios, Winsize, tcgetattr};
use wisp_protocol::settings::CACHE_KEPT;
use wisp_render::chrome::account::AccountKind;
use wisp_render::transcript::copy;
use wisp_tui::app::pager::{back_keys, return_keys};
use wisp_tui::app::{Exit, MAX_RESTORES, recovered};
use wisp_tui::term::doctor::{self, ProbeStatus};
use wisp_tui::term::input::HUNG_UP;
use wisp_tui::term::probe::LATE_GRACE;
use wisp_tui::term::{
    Entered, Env, Flags, Halted, KeyboardFlags, LateReplies, Modes, ProbePlan, ProbeReport,
    ResumeHook, ScrollBack, Streams, Terminal, TerminalOptions, Written, sync,
};

#[path = "support/app_child.rs"]
mod app_child;

#[path = "support/models.rs"]
mod models;

#[path = "support/fake_tool.rs"]
mod fake_tool;

#[path = "support/term_env.rs"]
mod term_env;

#[path = "support/scratch.rs"]
mod scratch;

#[allow(dead_code, reason = "idle is used by the CLI turn-end waits")]
#[path = "support/screen.rs"]
mod screen;

#[path = "../../wisp-render/tests/support/events.rs"]
mod events;

#[path = "support/spots.rs"]
mod spots;

use app_child::{
    AFTER_STEER, APP_ENDED, BEFORE, EXITED, FOLD_END, FOLD_THOUGHT, HOLD_MS, LIVE_END, LIVE_FILE,
    LIVE_ROW, LONG_SEEN, PAGE_END, PANIC_FILE, PANIC_MESSAGE, SHORT_END, STEP_WORDS, STEPS_END,
    placeholder,
};
use scratch::{ScratchDir, alive};
use spots::{fold_spot, row_spot, rows_of};

const ROLE: &str = "WISP_TUI_PTY_ROLE";
const DEADLINE: &str = "WISP_TUI_PTY_DEADLINE_MS";
const SCRATCH: &str = "wisp-tui-";

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

const MOUSE_ON: [&str; 3] = ["\x1b[?1000h", "\x1b[?1002h", "\x1b[?1006h"];
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";
const SCROLL_OFF: &str = "\x1b[?1007l";
const SCROLL_BACK: &str = "\x1b[?1007h";
const ENTER_ALT: &str = "\x1b[?1049h";
const LEAVE_ALT: &str = "\x1b[?1049l";
const ERASES: [&str; 5] = ["\x1b[J", "\x1b[0J", "\x1b[1J", "\x1b[2J", "\x1b[2K"];

const RESTORE_BYTES: [&str; 10] = [
    "\x1b[?2026l",
    "\x1b[0m",
    "\x1b[?25h",
    "\x1b[?7h",
    MOUSE_OFF,
    SCROLL_BACK,
    "\x1b[?1004l",
    "\x1b[?2004l",
    "\x1b[<1u",
    LEAVE_ALT,
];

fn enter(options: TerminalOptions) -> (Terminal, Option<ProbeReport>) {
    let Entered { terminal, probe } = Terminal::enter(options).unwrap();
    (terminal, probe)
}

fn arm(options: TerminalOptions) -> Terminal {
    let (mut terminal, _) = enter(options);
    terminal.apply(EVERYTHING).unwrap();
    let _ = terminal.write(sync::begin().as_bytes()).unwrap();
    let _ = terminal.write(b"\x1b[1;31marmed").unwrap();
    let _ = terminal.write(b"READY").unwrap();
    terminal
}

fn fill_output() {
    let out = std::io::stdout();
    let flags = rustix::fs::fcntl_getfl(&out).unwrap();
    rustix::fs::fcntl_setfl(&out, flags | OFlags::NONBLOCK).unwrap();
    let chunk = vec![b'y'; 1024];
    loop {
        let mut wrote = 0;
        while let Ok(n) = rustix::io::write(&out, &chunk) {
            wrote += n;
        }
        if wrote == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    rustix::fs::fcntl_setfl(&out, flags).unwrap();
}

fn halted(result: std::io::Result<Written>) -> Result<Written, Option<Halted>> {
    result.map_err(|e| {
        e.get_ref()
            .and_then(|e| e.downcast_ref::<Halted>())
            .copied()
    })
}

fn idle() -> ! {
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn probe_plan() -> ProbePlan {
    let ms: u64 = std::env::var(DEADLINE).unwrap().parse().unwrap();
    ProbePlan {
        deadline: Duration::from_millis(ms),
        sync_query: true,
        scroll_query: false,
        width_test: true,
        pictures: false,
    }
}

fn probe_options() -> TerminalOptions {
    TerminalOptions {
        probe: Some(probe_plan()),
        ..TerminalOptions::default()
    }
}

#[test]
fn pty_child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    match role.as_str() {
        "panic" => {
            let _terminal = arm(TerminalOptions::default());
            panic!("wisp-pty-panic");
        }
        "hook" => {
            std::mem::forget(arm(TerminalOptions::default()));
            panic!("wisp-pty-panic");
        }
        "drop" => {
            drop(arm(TerminalOptions::default()));
            println!("wisp-pty-dropped");
        }
        "probe" => {
            let (terminal, probe) = enter(probe_options());
            drop(terminal);
            let r = probe.unwrap();
            println!(
                "REPORT outcome={:?} bg={} sync={:?} kitty={:?} dot={:?} typed={:?} ms={}",
                r.outcome,
                r.background.map(|c| c.to_string()).unwrap_or_default(),
                r.synchronized_output,
                r.kitty_keyboard,
                r.dot_width,
                String::from_utf8_lossy(&r.typed),
                r.elapsed.as_millis(),
            );
        }
        "dot" => {
            print!("abc");
            std::io::stdout().flush().unwrap();
            let (terminal, probe) = enter(probe_options());
            drop(terminal);
            let r = probe.unwrap();
            println!("|{:?} dot={:?}", r.outcome, r.dot_width);
        }
        "late" => {
            let (mut terminal, probe) = enter(probe_options());
            let report = probe.unwrap();
            let _ = terminal.write(b"TIMED-OUT").unwrap();
            let mut late = LateReplies::after(&report, Instant::now());
            let typed = terminal.drain_late_replies(&mut late).unwrap();
            drop(terminal);
            println!(
                "LATE outcome={:?} typed={:?}",
                report.outcome,
                String::from_utf8_lossy(&typed)
            );
        }
        "doctor-probe" => {
            let status = doctor::probe(&Env::current(), Streams::current(), Flags::default());
            match status {
                ProbeStatus::Ran(r) => println!(
                    "DOCTOR outcome={:?} typed={:?}",
                    r.outcome,
                    String::from_utf8_lossy(&r.typed)
                ),
                other => println!("DOCTOR {other:?}"),
            }
        }
        "signal" => {
            let _terminal = arm(TerminalOptions::default());
            idle();
        }
        "held-stuck" => {
            wisp_tui::term::hold_deaths(Duration::from_secs(2)).unwrap();
            let _terminal = arm(TerminalOptions::default());
            idle();
        }
        "tstp" => {
            let terminal = arm(TerminalOptions::default());
            while !terminal.take_resumed() {
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut terminal = terminal;
            let _ = terminal.write(b"RESUMED").unwrap();
            let mut b = [0u8; 1];
            std::io::stdin().read_exact(&mut b).unwrap();
            drop(terminal);
            println!("tstp-done");
        }
        "blocked-lock" => {
            let mut terminal = arm(TerminalOptions::default());
            let big = vec![b'x'; 8 << 20];
            terminal.write(&big).ok();
            idle();
        }
        "blocked-nolock" => {
            let _terminal = arm(TerminalOptions::default());
            std::thread::spawn(|| {
                let big = vec![b'y'; 8 << 20];
                std::io::stdout().write_all(&big).ok();
            });
            idle();
        }
        "apply-loop" => {
            let mut terminal = arm(TerminalOptions::default());
            loop {
                terminal.apply(EVERYTHING).ok();
            }
        }
        "render-loop" => {
            let mut terminal = arm(TerminalOptions::default());
            let body = vec![b'x'; 512];
            loop {
                terminal.apply(EVERYTHING).ok();
                terminal.write(&body).ok();
            }
        }
        "panic-recover" => {
            let mut terminal = arm(TerminalOptions::default());
            std::thread::spawn(|| panic!("worker blew up")).join().ok();
            let first = halted(terminal.write(b"W1"));
            eprintln!("\r\nfirst-write={first:?}\r");
            while !terminal.take_resumed() {
                std::thread::sleep(Duration::from_millis(10));
            }
            let second = halted(terminal.write(b"W2"));
            let applied = terminal.applied() == EVERYTHING;
            drop(terminal);
            println!("\r\nrecovered second-write={second:?} applied={applied}\r");
        }
        "resume-panic" => {
            let hook: ResumeHook = Box::new(|| panic!("on_resume blew up"));
            let _terminal = arm(TerminalOptions {
                on_resume: Some(hook),
                ..TerminalOptions::default()
            });
            idle();
        }
        "drop-race" => {
            let me = rustix::process::getpid();
            std::thread::spawn(move || {
                loop {
                    kill_process(me, Signal::CONT).ok();
                    std::thread::sleep(Duration::from_micros(20));
                }
            });
            let stdin = std::io::stdin();
            for i in 0..1500 {
                let (mut terminal, _) = enter(TerminalOptions::default());
                terminal
                    .apply(Modes {
                        raw: true,
                        ..Modes::COOKED
                    })
                    .unwrap();
                drop(terminal);
                std::thread::sleep(Duration::from_micros(300));
                if !tcgetattr(&stdin)
                    .unwrap()
                    .local_modes
                    .contains(LocalModes::ICANON)
                {
                    println!("\r\nrace-leak at {i}\r");
                    std::process::exit(3);
                }
            }
            println!("race-noleak");
        }
        "suspend-fail" => {
            let mut terminal = arm(TerminalOptions::default());
            let out = std::io::stdout();
            let flags = rustix::fs::fcntl_getfl(&out).unwrap();
            rustix::fs::fcntl_setfl(&out, flags | OFlags::NONBLOCK).unwrap();
            rustix::termios::tcflow(&out, rustix::termios::Action::OOff).unwrap();
            let suspended = terminal.suspend();
            rustix::termios::tcflow(&out, rustix::termios::Action::OOn).unwrap();
            rustix::fs::fcntl_setfl(&out, flags).unwrap();
            while !terminal.take_resumed() {
                std::thread::sleep(Duration::from_millis(10));
            }
            let still_suspended = terminal.is_suspended();
            let written = terminal.write(b"AFTER");
            let raw = !tcgetattr(&out)
                .unwrap()
                .local_modes
                .contains(LocalModes::ICANON);
            drop(terminal);
            println!(
                "\r\nSUSPEND failed={} still_suspended={still_suspended} written={written:?} raw={raw}\r",
                suspended.is_err()
            );
        }
        "slow-drop" => {
            let terminal = arm(TerminalOptions::default());
            fill_output();
            drop(terminal);
            println!("\r\nslow-dropped\r");
        }
        "slow-suspend" => {
            let mut terminal = arm(TerminalOptions::default());
            fill_output();
            let suspended = terminal.suspend();
            while !terminal.take_resumed() {
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(terminal);
            println!("\r\nslow-suspended ok={}\r", suspended.is_ok());
        }
        "signal-filled" => {
            let _terminal = arm(TerminalOptions::default());
            fill_output();
            idle();
        }
        "held-filled" => {
            wisp_tui::term::hold_deaths(Duration::from_secs(10)).unwrap();
            let terminal = arm(TerminalOptions::default());
            fill_output();
            while terminal.halted().is_none() {
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(terminal);
            wisp_tui::term::die_if_dying();
            idle();
        }
        "signal-busy" => {
            let mut terminal = arm(TerminalOptions::default());
            let big = vec![b'x'; 2 << 20];
            let written = terminal.write(&big);
            eprintln!("\r\nbusy-write-returned {}\r", written.is_ok());
            idle();
        }
        "contained-panic" => {
            let mut terminal = arm(TerminalOptions::default());
            let caught = wisp_tui::term::contain(|| -> u8 { panic!("parser blew up") });
            let raw = !tcgetattr(std::io::stdout())
                .unwrap()
                .local_modes
                .contains(LocalModes::ICANON);
            let halt = terminal.halted();
            let written = halted(terminal.write(b"AFTER"));
            let recorded = wisp_tui::term::contained_panics().len();
            drop(terminal);
            println!(
                "\r\nCONTAINED caught={} raw={raw} halted={halt:?} written={written:?} recorded={recorded}\r",
                caught.is_err()
            );
        }
        "plugin-panic" => {
            let mut terminal = arm(TerminalOptions::default());
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_time()
                .build()
                .unwrap();
            let caught = runtime.block_on(async {
                tokio::spawn(async {
                    let sleep: Arc<wisp_plugin_testkit::host::SleepFn> =
                        Arc::new(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                    let host = Arc::new(wisp_plugin_testkit::host::FakeHost {
                        sleep: Some(sleep),
                        ..wisp_plugin_testkit::host::FakeHost::default()
                    });
                    let plugin = (wisp_plugin_testkit::bundled().instantiate)(host);
                    let call = plugin.prepare("panic", r#"{"ms": 20}"#).await.unwrap();
                    let ctx = wisp_plugin_testkit::host::FakeCall::default();
                    wisp_contain::Contained::new(plugin.run(call, &ctx))
                        .await
                        .is_err()
                })
                .await
                .unwrap()
            });
            drop(runtime);
            let raw = !tcgetattr(std::io::stdout())
                .unwrap()
                .local_modes
                .contains(LocalModes::ICANON);
            let halt = terminal.halted();
            let written = halted(terminal.write(b"AFTER"));
            let recorded = wisp_tui::term::contained_panics().len();
            drop(terminal);
            println!(
                "\r\nPLUGIN caught={caught} raw={raw} halted={halt:?} written={written:?} recorded={recorded}\r"
            );
        }
        "restore-hook" => {
            let restored = Arc::new(AtomicBool::new(false));
            let seen = Arc::clone(&restored);
            let mut terminal = arm(TerminalOptions {
                on_restore: Some(Box::new(move || seen.store(true, Ordering::SeqCst))),
                ..TerminalOptions::default()
            });
            std::thread::spawn(|| panic!("worker blew up")).join().ok();
            let hooked = restored.load(Ordering::SeqCst);
            let halt = terminal.halted();
            let refused = halted(terminal.write(b"W1"));
            terminal.apply(EVERYTHING).unwrap();
            let again = halted(terminal.write(b"W2"));
            let applied = terminal.applied() == EVERYTHING;
            drop(terminal);
            println!(
                "\r\nHOOK hooked={hooked} halted={halt:?} refused={refused:?} again={again:?} applied={applied}\r"
            );
        }
        "busy-frame" => {
            let mut terminal = arm(TerminalOptions::default());
            fill_output();
            let body = b"\x1b[1mframe-bytes ".repeat(120);
            std::fs::write(busy_marker(rustix::process::getpid()), b"").unwrap();
            let written = terminal.write_frame(&body, sync::CursorAfter::Shown);
            eprintln!("\r\nbusy-frame-returned {}\r", written.is_ok());
            idle();
        }
        "panic-race" => {
            let mut terminal = arm(TerminalOptions::default());
            let big = vec![b'x'; 16 * 1024 * 1024];
            let panicker = std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(5));
                panic!("worker blew up");
            });
            let first = halted(terminal.write(&big));
            if terminal.halted() == Some(Halted::Restored) {
                terminal.apply(EVERYTHING).unwrap();
            }
            panicker.join().ok();
            std::thread::sleep(Duration::from_millis(300));
            let is_raw = || {
                !tcgetattr(std::io::stdout())
                    .unwrap()
                    .local_modes
                    .contains(LocalModes::ICANON)
            };
            let raw = is_raw();
            let halt = terminal.halted();
            if halt == Some(Halted::Restored) {
                terminal.apply(EVERYTHING).unwrap();
            }
            let again = halted(terminal.write(b"W2"));
            let raw_again = is_raw();
            drop(terminal);
            println!(
                "\r\nRACE first={first:?} raw={raw} halted={halt:?} again={again:?} raw_again={raw_again}\r"
            );
        }
        other if other.starts_with("app:") => app_child::run(other),
        other => panic!("unknown role {other}"),
    }
}
fn open_pty() -> (OwnedFd, File) {
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
    rustix::termios::tcsetwinsize(
        &slave,
        Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    (master, File::from(slave))
}

struct Pty {
    master: Option<OwnedFd>,
    slave: File,
    child: Child,
    out: Vec<u8>,
    reaped: Option<WaitStatus>,
    alive: Arc<AtomicBool>,
}

const HARD_LIMIT: Duration = Duration::from_secs(120);

fn watchdog(pid: Pid, role: String, alive: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let start = Instant::now();
        while alive.load(Ordering::SeqCst) {
            if start.elapsed() > HARD_LIMIT {
                eprintln!(
                    "pty watchdog: role {role} still running after {HARD_LIMIT:?}; killing it"
                );
                kill_process(pid, Signal::KILL).ok();
                std::thread::sleep(Duration::from_secs(5));
                if alive.load(Ordering::SeqCst) {
                    eprintln!("pty watchdog: the test for role {role} is hung; failing the run");
                    std::process::exit(101);
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

impl Pty {
    fn spawn(role: &str, deadline_ms: u64) -> Pty {
        Pty::spawn_with(role, deadline_ms, &[])
    }

    fn spawn_with(role: &str, deadline_ms: u64, env: &[(&str, &str)]) -> Pty {
        Pty::start(role, deadline_ms, env, true)
    }

    fn spawn_without_a_session(role: &str, env: &[(&str, &str)]) -> Pty {
        Pty::start(role, 0, env, false)
    }

    fn start(role: &str, deadline_ms: u64, env: &[(&str, &str)], session: bool) -> Pty {
        let (master, slave) = open_pty();
        assert!(cooked(&tcgetattr(&slave).unwrap()));
        let exe = std::env::current_exe().unwrap();
        let mut command = if session {
            let mut command = Command::new("setsid");
            command.arg("-c").arg(exe);
            command
        } else {
            Command::new(exe)
        };
        let child = term_env::clean_term_env(&mut command)
            .args(["--exact", "pty_child", "--nocapture", "--test-threads=1"])
            .env(ROLE, role)
            .env(DEADLINE, deadline_ms.to_string())
            .env("RUST_BACKTRACE", "0")
            .envs(env.iter().copied())
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .unwrap();
        let alive = Arc::new(AtomicBool::new(true));
        let pty = Pty {
            master: Some(master),
            slave,
            child,
            out: vec![],
            reaped: None,
            alive: Arc::clone(&alive),
        };
        watchdog(pty.pid(), role.to_owned(), alive);
        pty
    }

    fn shell() -> Option<Pty> {
        let (master, slave) = open_pty();
        let child = term_env::clean_term_env(&mut Command::new("setsid"))
            .args(["-c", "bash", "--norc", "--noprofile", "-i"])
            .env("PS1", "$ ")
            .env("RUST_BACKTRACE", "0")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .ok()?;
        let alive = Arc::new(AtomicBool::new(true));
        let pty = Pty {
            master: Some(master),
            slave,
            child,
            out: vec![],
            reaped: None,
            alive: Arc::clone(&alive),
        };
        watchdog(pty.pid(), "shell".to_owned(), alive);
        Some(pty)
    }

    fn reap(&mut self, status: WaitStatus) -> WaitStatus {
        self.reaped = Some(status);
        self.alive.store(false, Ordering::SeqCst);
        status
    }

    fn read_chunk(&mut self, max: usize, wait: Duration) -> usize {
        let Some(master) = &self.master else {
            return 0;
        };
        let timeout = Timespec {
            tv_sec: i64::try_from(wait.as_secs()).unwrap(),
            tv_nsec: i64::from(wait.subsec_nanos()),
        };
        let mut fds = [PollFd::new(master, PollFlags::IN)];
        if poll(&mut fds, Some(&timeout)).unwrap_or(0) == 0 {
            return 0;
        }
        let mut buf = vec![0u8; max];
        match rustix::io::read(master, &mut buf) {
            Ok(n) => {
                self.out.extend_from_slice(&buf[..n]);
                n
            }
            Err(_) => 0,
        }
    }

    fn pid(&self) -> Pid {
        Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap()
    }

    fn goes_quiet(&mut self, quiet: Duration, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if !self.pump(quiet) {
                return true;
            }
        }
        false
    }

    fn drain_idle(&mut self) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) && self.pump(Duration::from_millis(50)) {}
    }

    fn pump(&mut self, wait: Duration) -> bool {
        self.read_chunk(65536, wait) > 0
    }

    fn pump_until(&mut self, limit: Duration, mut done: impl FnMut(&[u8]) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if done(&self.out) {
                return true;
            }
            self.pump(Duration::from_millis(20));
        }
        done(&self.out)
    }

    fn until(&mut self, needle: &str, limit: Duration) -> bool {
        self.pump_until(limit, |out| find(out, needle, 0).is_some())
    }

    fn send(&self, bytes: &[u8]) {
        rustix::io::write(self.master.as_ref().unwrap(), bytes).unwrap();
    }

    fn wait_exit(&mut self, limit: Duration, drain: bool) -> Option<WaitStatus> {
        let start = Instant::now();
        while start.elapsed() < limit {
            if drain {
                self.pump(Duration::from_millis(10));
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some((_, status)) = waitpid(Some(self.pid()), WaitOptions::NOHANG).unwrap() {
                let drained = Instant::now();
                while drain
                    && drained.elapsed() < Duration::from_secs(5)
                    && self.pump(Duration::from_millis(30))
                {}
                return Some(self.reap(status));
            }
        }
        None
    }

    fn wait_stopped(&mut self, limit: Duration, drain: bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if drain {
                self.pump(Duration::from_millis(10));
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some(status) = self.poll_status() {
                return status.stopped();
            }
        }
        false
    }

    fn poll_status(&mut self) -> Option<WaitStatus> {
        let (_, status) = waitpid(
            Some(self.pid()),
            WaitOptions::NOHANG | WaitOptions::UNTRACED,
        )
        .unwrap()?;
        if status.stopped() {
            Some(status)
        } else {
            Some(self.reap(status))
        }
    }

    fn termios(&self) -> Termios {
        tcgetattr(&self.slave).unwrap()
    }

    fn kill(&self, sig: Signal) {
        kill_process(self.pid(), sig).unwrap();
    }

    fn finish(mut self, limit: Duration) -> (WaitStatus, Vec<u8>, Termios) {
        let status = self.wait_exit(limit, true);
        if status.is_none() {
            kill_process(self.pid(), Signal::KILL).ok();
            self.child.wait().ok();
            self.alive.store(false, Ordering::SeqCst);
            panic!("child hung: {:?}", String::from_utf8_lossy(&self.out));
        }
        let termios = self.termios();
        (status.unwrap(), std::mem::take(&mut self.out), termios)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if self.reaped.is_none() && self.child.try_wait().ok().flatten().is_none() {
            kill_process(self.pid(), Signal::KILL).ok();
            self.child.wait().ok();
        }
        self.alive.store(false, Ordering::SeqCst);
    }
}

fn find(hay: &[u8], needle: &str, from: usize) -> Option<usize> {
    if hay.len() < needle.len() + from {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .map(|i| i + from)
}

fn cooked(t: &Termios) -> bool {
    t.local_modes
        .contains(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG | LocalModes::IEXTEN)
        && t.output_modes.contains(OutputModes::OPOST)
        && t.input_modes.contains(InputModes::ICRNL)
}

fn screen(output: &[u8]) -> vt100::Parser<screen::Updates> {
    screen::parsed(output, 24, 80)
}

fn assert_restored(out: &[u8], termios: &Termios, after_restore: Option<&str>) {
    let text = String::from_utf8_lossy(out);
    assert!(cooked(termios), "{termios:?}");
    let parser = screen(out);
    let s = parser.screen();
    assert!(!s.alternate_screen(), "{text:?}");
    assert!(!s.hide_cursor(), "{text:?}");
    assert!(!s.bracketed_paste(), "{text:?}");
    assert!(!s.bold(), "{text:?}");
    assert_eq!(s.fgcolor(), vt100::Color::Default);
    assert!(!s.contents().contains("armed"), "{}", s.contents());
    let armed = find(out, "armed", 0).unwrap();
    let mut at = armed;
    for seq in RESTORE_BYTES {
        at = find(out, seq, at).unwrap_or_else(|| panic!("{seq:?} missing after {at} in {text:?}"));
    }
    if let Some(message) = after_restore {
        assert!(s.contents().contains(message), "{}", s.contents());
        let printed = find(out, message, armed).unwrap();
        assert!(printed > at, "{message} printed before restore: {text:?}");
    }
}

const LIMIT: Duration = Duration::from_secs(20);

#[test]
fn a_panic_restores_the_terminal_before_printing() {
    let (status, out, termios) = Pty::spawn("panic", 0).finish(LIMIT);
    assert!(!status.exited() || status.exit_status() != Some(0));
    assert_restored(&out, &termios, Some("wisp-pty-panic"));
}

#[test]
fn the_panic_hook_restores_without_the_guard() {
    let (status, out, termios) = Pty::spawn("hook", 0).finish(LIMIT);
    assert!(!status.exited() || status.exit_status() != Some(0));
    assert_restored(&out, &termios, Some("wisp-pty-panic"));
    let leaves = String::from_utf8_lossy(&out).matches("\x1b[?1049l").count();
    assert_eq!(leaves, 1);
}

#[test]
fn dropping_the_guard_restores_the_terminal() {
    let (status, out, termios) = Pty::spawn("drop", 0).finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert_restored(&out, &termios, Some("wisp-pty-dropped"));
}

fn transcript(out: &[u8]) -> Vec<String> {
    let mut parser = vt100::Parser::new(24.try_into().unwrap(), 80.try_into().unwrap(), 100_000);
    parser.process(out);
    parser.screen_mut().set_scrollback(usize::MAX);
    let depth = parser.screen().scrollback();
    let mut lines = vec![];
    for offset in (1..=depth).rev() {
        parser.screen_mut().set_scrollback(offset);
        lines.extend(parser.screen().rows(0, 80).take(1));
    }
    parser.screen_mut().set_scrollback(0);
    lines.extend(parser.screen().rows(0, 80));
    lines
}

#[test]
fn the_probe_reads_replies_and_keeps_typed_keys() {
    let mut pty = Pty::spawn("probe", 5000);
    assert!(pty.until("\x1b[c", LIMIT));
    pty.send(b"h\x1b]11;rgb:fafa/fafa/fafa\x1b\\i");
    pty.send(b"\x1b[?2026;2$y\x1b[?1u\x1b]10;rgb:1c/1c/1c\x07");
    pty.send(b"\x1b[3;1R\x1b[3;3R\x1b[?62;22c");
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    let text = String::from_utf8_lossy(&out);
    for want in [
        "outcome=Answered",
        "bg=#FAFAFA",
        "sync=Supported",
        "kitty=Some(1)",
        "dot=Some(2)",
        "typed=\"hi\"",
    ] {
        assert!(text.contains(want), "missing {want} in {text:?}");
    }
}

#[test]
fn a_silent_terminal_times_out_at_the_deadline() {
    let (status, out, termios) = Pty::spawn("probe", 300).finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("outcome=TimedOut"), "{text:?}");
    let ms: u128 = text
        .split("ms=")
        .nth(1)
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse().ok())
        .unwrap();
    assert!((300..2000).contains(&ms), "{ms} ms");
}

#[test]
fn the_width_dot_is_erased_and_earlier_text_kept() {
    let mut pty = Pty::spawn("dot", 5000);
    assert!(pty.until("\x1b[c", LIMIT));
    pty.send(b"\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\\x1b[?2026;2$y\x1b[1;4R\x1b[1;5R\x1b[?62;22c");
    let (status, out, _) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    let contents = screen(&out).screen().contents();
    assert!(!contents.contains('\u{b7}'), "{contents:?}");
    assert!(
        contents.contains("abc|Answered dot=Some(1)"),
        "{contents:?}"
    );
}

#[test]
fn late_replies_are_filtered_out_of_input() {
    let mut pty = Pty::spawn("late", 200);
    assert!(pty.until("TIMED-OUT", LIMIT));
    pty.send(b"k\x1b]11;rgb:0c0c/0c");
    std::thread::sleep(Duration::from_millis(30));
    pty.send(b"0c/0c0c\x1b\\e\x1b[?2026;2$y\x1b[1;4");
    std::thread::sleep(Duration::from_millis(30));
    pty.send(b"R\x1b[Ay\x1b[?62;22c");
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("LATE outcome=TimedOut typed=\"ke\\u{1b}[Ay\""),
        "{text:?}"
    );
}

#[test]
fn keys_typed_in_the_grace_window_reach_the_probe_report() {
    let mut pty = Pty::spawn("doctor-probe", 0);
    assert!(pty.until("\x1b[c", LIMIT));
    pty.send(b"abc");
    std::thread::sleep(Duration::from_millis(700));
    pty.send(b"d\x1b]11;rgb:0c0c/0c0c/0c0c\x07ef");
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("DOCTOR outcome=TimedOut typed=\"abcdef\""),
        "{text:?}"
    );
}

#[test]
fn sigterm_and_sighup_restore_then_die_by_the_signal() {
    for signal in [Signal::TERM, Signal::HUP] {
        let mut pty = Pty::spawn("signal", 0);
        assert!(pty.until("READY", LIMIT));
        pty.kill(signal);
        let (status, out, termios) = pty.finish(Duration::from_secs(5));
        assert_eq!(status.terminating_signal(), Some(signal.as_raw()));
        assert_restored(&out, &termios, None);
    }
}

#[test]
fn a_held_death_with_a_stuck_client_dies_at_its_limit_restored() {
    let mut pty = Pty::spawn("held-stuck", 0);
    assert!(pty.until("READY", LIMIT));
    let sent = Instant::now();
    pty.kill(Signal::TERM);
    std::thread::sleep(Duration::from_millis(500));
    assert!(pty.poll_status().is_none(), "died before its limit");
    assert!(cooked(&pty.termios()), "not restored at once");
    pty.kill(Signal::TSTP);
    std::thread::sleep(Duration::from_millis(100));
    assert!(pty.poll_status().is_none(), "stopped while dying");
    let (status, out, termios) = pty.finish(Duration::from_millis(3400));
    let took = sent.elapsed();
    assert_eq!(status.terminating_signal(), Some(Signal::TERM.as_raw()));
    assert!(took >= Duration::from_secs(2), "{took:?}");
    assert_restored(&out, &termios, None);
}

#[test]
fn sighup_with_the_master_closed_does_not_hang() {
    let mut pty = Pty::spawn("signal", 0);
    assert!(pty.until("READY", LIMIT));
    pty.master = None;
    std::thread::sleep(Duration::from_millis(100));
    pty.kill(Signal::HUP);
    let status = pty.wait_exit(Duration::from_secs(3), false).unwrap();
    assert_eq!(status.terminating_signal(), Some(Signal::HUP.as_raw()));
}

#[test]
fn sigtstp_restores_and_sigcont_reenters() {
    let mut pty = Pty::spawn("tstp", 0);
    assert!(pty.until("READY", LIMIT));
    let before = pty.out.len();
    pty.kill(Signal::TSTP);
    assert!(
        pty.wait_stopped(Duration::from_secs(5), true),
        "never stopped"
    );
    pty.drain_idle();
    assert!(cooked(&pty.termios()), "raw while stopped");
    assert!(!screen(&pty.out).screen().alternate_screen());
    assert!(find(&pty.out, "\x1b[?1049l", before).is_some());
    pty.kill(Signal::CONT);
    assert!(pty.until("RESUMED", Duration::from_secs(5)));
    assert!(!cooked(&pty.termios()), "not raw after SIGCONT");
    assert!(screen(&pty.out).screen().alternate_screen());
    pty.send(b"q");
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    assert!(!screen(&out).screen().alternate_screen());
}

fn dies_quickly_while_output_is_blocked(role: &str) {
    let mut pty = Pty::spawn(role, 0);
    assert!(pty.until("READY", LIMIT));
    std::thread::sleep(Duration::from_millis(300));
    let start = Instant::now();
    pty.kill(Signal::TERM);
    let status = pty.wait_exit(Duration::from_secs(2), false);
    assert!(status.is_some(), "{role}: still alive 2 s after SIGTERM");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(
        status.unwrap().terminating_signal(),
        Some(Signal::TERM.as_raw())
    );
    assert!(cooked(&pty.termios()), "{role}: termios not restored");
}

#[test]
fn sigterm_kills_while_a_locked_write_is_blocked() {
    dies_quickly_while_output_is_blocked("blocked-lock");
}

#[test]
fn sigterm_kills_while_output_is_full_without_the_lock() {
    dies_quickly_while_output_is_blocked("blocked-nolock");
}

#[test]
fn a_panicking_resume_hook_keeps_signals_working() {
    let mut pty = Pty::spawn("resume-panic", 0);
    assert!(pty.until("READY", LIMIT));
    pty.kill(Signal::CONT);
    assert!(pty.until("on_resume blew up", Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(200));
    assert!(!cooked(&pty.termios()), "the hook's panic restored the tty");
    pty.drain_idle();
    assert!(screen(&pty.out).screen().alternate_screen());
    pty.kill(Signal::TERM);
    let (status, _, termios) = pty.finish(Duration::from_secs(5));
    assert_eq!(status.terminating_signal(), Some(Signal::TERM.as_raw()));
    assert!(cooked(&termios));
}

#[test]
fn sigcont_racing_drop_never_leaves_the_tty_raw() {
    let mut pty = Pty::spawn("drop-race", 0);
    assert!(
        pty.until("race-", Duration::from_secs(60)),
        "{:?}",
        String::from_utf8_lossy(&pty.out)
    );
    let (status, out, termios) = pty.finish(LIMIT);
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("race-noleak"), "{text:?}");
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
}

#[test]
fn a_failed_suspend_still_stops_and_recovers() {
    let mut pty = Pty::spawn("suspend-fail", 0);
    assert!(pty.until("READY", LIMIT));
    assert!(
        pty.wait_stopped(Duration::from_secs(15), false),
        "never stopped"
    );
    assert!(cooked(&pty.termios()), "raw while stopped");
    pty.drain_idle();
    pty.kill(Signal::CONT);
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("SUSPEND failed=true still_suspended=false written=Ok(Sent) raw=true"),
        "{text:?}"
    );
}

#[derive(Clone, Copy, Debug)]
enum Reader {
    PauseThenFast(u64),
    ChunkEvery(u64),
}

impl Pty {
    fn read_slowly(&mut self, reader: Reader, started: Instant) {
        match reader {
            Reader::PauseThenFast(ms) => {
                if started.elapsed() < Duration::from_millis(ms) {
                    std::thread::sleep(Duration::from_millis(10));
                } else {
                    self.pump(Duration::from_millis(10));
                }
            }
            Reader::ChunkEvery(ms) => {
                std::thread::sleep(Duration::from_millis(ms));
                self.read_chunk(4096, Duration::ZERO);
            }
        }
    }

    fn exited(&mut self) -> Option<WaitStatus> {
        let found = waitpid(Some(self.pid()), WaitOptions::NOHANG).unwrap();
        found.map(|(_, s)| self.reap(s))
    }
}

fn assert_fully_restored(out: &[u8], termios: &Termios, label: &str) {
    let text = String::from_utf8_lossy(out);
    let parser = screen(out);
    let s = parser.screen();
    assert!(cooked(termios), "{label}: termios");
    assert!(!s.alternate_screen(), "{label}: alternate screen");
    assert!(!s.hide_cursor(), "{label}: cursor hidden");
    assert!(!s.bracketed_paste(), "{label}: bracketed paste");
    let armed = find(out, "armed", 0).unwrap();
    let mut at = armed;
    for seq in RESTORE_BYTES {
        at = find(out, seq, at).unwrap_or_else(|| panic!("{label}: {seq:?} missing"));
    }
    assert!(text.len() > at);
}

const SLOW_READERS: [Reader; 2] = [Reader::PauseThenFast(800), Reader::ChunkEvery(300)];

#[test]
fn a_normal_drop_on_a_slow_tty_restores_everything() {
    for reader in SLOW_READERS {
        let mut pty = Pty::spawn("slow-drop", 0);
        assert!(pty.until("READY", LIMIT));
        let started = Instant::now();
        while find(&pty.out, "slow-dropped", 0).is_none() {
            assert!(started.elapsed() < LIMIT, "{reader:?}: hung");
            pty.read_slowly(reader, started);
        }
        let (status, out, termios) = pty.finish(LIMIT);
        assert_eq!(status.exit_status(), Some(0), "{reader:?}");
        assert_fully_restored(&out, &termios, &format!("{reader:?}"));
    }
}

#[test]
fn a_suspend_on_a_slow_tty_restores_everything() {
    for reader in SLOW_READERS {
        let mut pty = Pty::spawn("slow-suspend", 0);
        assert!(pty.until("READY", LIMIT));
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < LIMIT, "{reader:?}: never stopped");
            pty.read_slowly(reader, started);
            if let Some(status) = pty.poll_status() {
                assert!(status.stopped(), "{reader:?}: {status:?}");
                break;
            }
        }
        pty.drain_idle();
        assert_fully_restored(
            &pty.out,
            &pty.termios(),
            &format!("{reader:?} while stopped"),
        );
        pty.kill(Signal::CONT);
        let (status, out, termios) = pty.finish(LIMIT);
        assert_eq!(status.exit_status(), Some(0), "{reader:?}");
        assert!(String::from_utf8_lossy(&out).contains("slow-suspended ok=true"));
        assert_fully_restored(&out, &termios, &format!("{reader:?} at exit"));
    }
}

#[test]
fn sigterm_on_a_slow_tty_restores_everything_before_dying() {
    for role in ["signal-filled", "held-filled"] {
        let mut pty = Pty::spawn(role, 0);
        assert!(pty.until("READY", LIMIT));
        std::thread::sleep(Duration::from_millis(100));
        pty.kill(Signal::TERM);
        let started = Instant::now();
        let status = loop {
            if let Some(status) = pty.exited() {
                break status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{role}: still alive"
            );
            pty.read_slowly(Reader::ChunkEvery(400), started);
        };
        pty.drain_idle();
        assert_eq!(status.terminating_signal(), Some(Signal::TERM.as_raw()));
        let termios = pty.termios();
        assert_fully_restored(&pty.out, &termios, &format!("{role}: SIGTERM every 400 ms"));
    }
}

#[test]
fn sigterm_during_a_big_write_stops_it_and_restores_everything() {
    for period in [5u64, 20, 50] {
        let mut pty = Pty::spawn("signal-busy", 0);
        assert!(pty.until("READY", LIMIT));
        let started = Instant::now();
        let mut sent = None;
        let status = loop {
            if sent.is_none() && started.elapsed() > Duration::from_millis(300) {
                pty.kill(Signal::TERM);
                sent = Some(Instant::now());
            }
            if let Some(status) = pty.exited() {
                break status;
            }
            assert!(started.elapsed() < LIMIT, "every {period} ms: still alive");
            pty.read_slowly(Reader::ChunkEvery(period), started);
        };
        let died_after = sent.map(|at| at.elapsed());
        pty.drain_idle();
        assert_eq!(
            status.terminating_signal(),
            Some(Signal::TERM.as_raw()),
            "every {period} ms"
        );
        assert!(
            died_after.is_some_and(|d| d < Duration::from_millis(1500)),
            "every {period} ms: died {died_after:?} after the signal"
        );
        let termios = pty.termios();
        assert_fully_restored(
            &pty.out,
            &termios,
            &format!("SIGTERM mid-write every {period} ms"),
        );
    }
}

fn rfind(hay: &[u8], needle: &str) -> Option<usize> {
    hay.windows(needle.len())
        .rposition(|w| w == needle.as_bytes())
}

#[test]
fn a_contained_parser_panic_leaves_the_terminal_raw_and_drawing() {
    let mut pty = Pty::spawn("contained-panic", 0);
    assert!(pty.until("READY", LIMIT));
    let (status, out, termios) = pty.finish(LIMIT);
    let text = String::from_utf8_lossy(&out);
    assert_eq!(status.exit_status(), Some(0), "{text:?}");
    assert!(
        text.contains("CONTAINED caught=true raw=true halted=None written=Ok(Sent) recorded=1"),
        "{text:?}"
    );
    assert!(!text.contains("parser blew up"), "{text:?}");
    assert!(find(&out, "AFTER", 0).is_some());
    assert_restored(&out, &termios, Some("CONTAINED"));
}

#[test]
fn a_plugin_panic_on_a_runtime_worker_keeps_the_terminal_raw_and_drawing() {
    let mut pty = Pty::spawn("plugin-panic", 0);
    assert!(pty.until("READY", LIMIT));
    let (status, out, termios) = pty.finish(LIMIT);
    let text = String::from_utf8_lossy(&out);
    assert_eq!(status.exit_status(), Some(0), "{text:?}");
    assert!(
        text.contains("PLUGIN caught=true raw=true halted=None written=Ok(Sent) recorded=1"),
        "{text:?}"
    );
    assert!(!text.contains("panic tool panicked"), "{text:?}");
    assert!(find(&out, "AFTER", 0).is_some());
    assert_restored(&out, &termios, Some("PLUGIN"));
}

#[test]
fn a_real_panic_restores_calls_the_hook_and_drawing_resumes_after_reapplying() {
    let mut pty = Pty::spawn("restore-hook", 0);
    assert!(pty.until("READY", LIMIT));
    let (status, out, termios) = pty.finish(LIMIT);
    let text = String::from_utf8_lossy(&out);
    assert_eq!(status.exit_status(), Some(0), "{text:?}");
    assert!(
        text.contains(
            "HOOK hooked=true halted=Some(Restored) refused=Err(Some(Restored)) again=Ok(Sent) applied=true"
        ),
        "{text:?}"
    );
    assert!(text.contains("worker blew up"), "{text:?}");
    assert!(cooked(&termios));
}

#[test]
fn a_panic_restore_racing_a_reapply_never_leaves_the_app_drawing_in_cooked_mode() {
    for round in 0..5 {
        let mut pty = Pty::spawn("panic-race", 0);
        assert!(pty.until("READY", LIMIT));
        let (status, out, termios) = pty.finish(LIMIT);
        let text = String::from_utf8_lossy(&out);
        assert_eq!(status.exit_status(), Some(0), "round {round}: {text:?}");
        let at = text.find("RACE first=").unwrap();
        let line = &text[at..];
        let consistent = line.contains("raw=true halted=None")
            || line.contains("raw=false halted=Some(Restored)");
        assert!(consistent, "round {round}: {line:?}");
        assert!(
            line.contains("again=Ok(Sent) raw_again=true"),
            "round {round}: {line:?}"
        );
        assert!(cooked(&termios));
    }
}

#[test]
fn a_frame_finishing_after_an_emergency_restore_is_undone_by_the_second_locked_pass() {
    for round in 0..3 {
        let label = format!("SIGTERM mid-frame, round {round}");
        let mut pty = Pty::spawn("busy-frame", 0);
        assert!(pty.until("READY", LIMIT));
        let marker = busy_marker(pty.pid());
        let waiting = Instant::now();
        while !marker.exists() {
            assert!(
                waiting.elapsed() < LIMIT,
                "{label}: the frame write never started"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_file(&marker).ok();
        std::thread::sleep(Duration::from_millis(50));
        pty.kill(Signal::TERM);
        std::thread::sleep(PAST_WRITER_PATIENCE);
        pty.read_chunk(4096, Duration::ZERO);
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = pty.exited() {
                break status;
            }
            assert!(started.elapsed() < LIMIT, "{label}: still alive");
            pty.pump(Duration::from_millis(5));
        };
        pty.drain_idle();
        assert_eq!(
            status.terminating_signal(),
            Some(Signal::TERM.as_raw()),
            "{label}"
        );
        let termios = pty.termios();
        assert_fully_restored(&pty.out, &termios, &label);
        let last = rfind(&pty.out, "\x1b[1m").unwrap();
        for seq in ["\x1b[0m", "\x1b[?25h"] {
            assert!(
                find(&pty.out, seq, last).is_some(),
                "{label}: {seq:?} not after the last frame byte"
            );
        }
        let parser = screen(&pty.out);
        assert!(!parser.screen().bold(), "{label}");
        assert!(!parser.screen().hide_cursor(), "{label}");
        let armed = find(&pty.out, "armed", 0).unwrap();
        let leaves = String::from_utf8_lossy(&pty.out[armed..])
            .matches("\x1b[?1049l")
            .count();
        assert_eq!(leaves, 2, "{label}: the emergency pass and the locked pass");
    }
}

const PAST_WRITER_PATIENCE: Duration = Duration::from_millis(600);

fn busy_marker(pid: Pid) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("wisp-tui-busy-frame-{}", pid.as_raw_nonzero()))
}

fn dies_restored_while_the_app_keeps_drawing(role: &str) {
    for signal in [Signal::TERM, Signal::HUP] {
        for round in 0..5 {
            let label = format!("{role} {signal:?} round {round}");
            let mut pty = Pty::spawn(role, 0);
            assert!(pty.until("READY", LIMIT), "{label}");
            std::thread::sleep(Duration::from_millis(150));
            pty.kill(signal);
            let status = pty.wait_exit(LIMIT, true).unwrap();
            assert_eq!(
                status.terminating_signal(),
                Some(signal.as_raw()),
                "{label}"
            );
            let termios = pty.termios();
            assert_fully_restored(&pty.out, &termios, &label);
        }
    }
}

const PTY_HOME: &str = "/home/wisp-pty";
const PROBE_REPLY_TAIL: &str = "\x1b[?1007;1$y\x1b[?62;22c";

fn answer_probe(pty: &mut Pty) {
    assert!(
        pty.until("\x1b[c", LIMIT),
        "{:?}",
        String::from_utf8_lossy(&pty.out)
    );
    let (row, col) = screen(&pty.out).screen().cursor_position();
    pty.send(format!("\x1b[{};{}R{PROBE_REPLY_TAIL}", row + 1, col + 1).as_bytes());
    assert!(
        pty.until(&placeholder()[1..], LIMIT),
        "no composer: {:?}",
        String::from_utf8_lossy(&pty.out)
    );
}

fn spawn_app(role: &str, extra: &[(&str, &str)]) -> Pty {
    let mut env = vec![("HOME", PTY_HOME)];
    env.extend_from_slice(extra);
    let mut pty = Pty::spawn_with(role, 0, &env);
    answer_probe(&mut pty);
    pty
}

fn visible(out: &[u8]) -> String {
    screen::shown(out, 24, 80)
}

fn rows(out: &[u8]) -> Vec<String> {
    rows_of(screen(out).screen())
}

fn screen_wait(mut done: impl FnMut(&str) -> bool) -> impl FnMut(&[u8]) -> bool {
    let mut parser = screen::parsed(&[], 24, 80);
    let mut received = 0;
    move |out| {
        screen::process(&mut parser, &out[received..]);
        received = out.len();
        done(&screen::contents(&parser))
    }
}

fn until_screen(pty: &mut Pty, needle: &str) -> bool {
    pty.pump_until(LIMIT, screen_wait(|shown| shown.contains(needle)))
}

fn until_gone(pty: &mut Pty, needle: &str) -> bool {
    pty.pump_until(LIMIT, screen_wait(|shown| !shown.contains(needle)))
}

#[test]
fn synchronized_frames_are_seen_at_their_end_not_at_read_boundaries() {
    use wisp_render::transcript::copy::CARD_SETTLING;
    use wisp_tui::term::sync::CursorAfter;

    let before: &[u8] = b"\x1b[?25lbefore";
    let question = "Which colour should the button be?";
    let body = format!("\x1b[2J\x1b[H{question}\r\n{CARD_SETTLING}");
    let frame = sync::frame(body.as_bytes(), CursorAfter::Hidden);
    for split in 0..=frame.len() {
        let mut bytes = before.to_vec();
        let mut parser = screen::parsed(&bytes, 30, 100);
        assert_eq!(screen::complete(&parser, &bytes), before);
        let mut appears = screen_wait(|shown| shown.contains(question));
        let mut gone = screen_wait(|shown| !shown.contains("before"));
        assert!(!appears(&bytes) && !gone(&bytes));
        let mut exact = screen_wait(|shown| shown == "before");
        assert!(exact(&bytes) && exact(&bytes));
        for byte in &frame[..split] {
            bytes.push(*byte);
            screen::process(&mut parser, std::slice::from_ref(byte));
            let complete = bytes.len() == before.len() + frame.len();
            assert_eq!((appears(&bytes), gone(&bytes)), (complete, complete));
        }
        let shown = screen::contents(&parser);
        let raw = String::from_utf8_lossy(screen::complete(&parser, &bytes));
        if split < frame.len() {
            assert_eq!(shown, "before", "split {split}");
            assert!(!raw.contains(question), "split {split}: {raw:?}");
        } else {
            assert!(shown.contains(question) && shown.contains(CARD_SETTLING));
            assert!(raw.contains(question) && raw.contains(CARD_SETTLING));
        }
        if (sync::begin().len()..frame.len()).contains(&split) {
            assert_eq!(screen::complete(&parser, &bytes), before, "split {split}");
        }
        bytes.extend_from_slice(&frame[split..]);
        screen::process(&mut parser, &frame[split..]);
        assert!(screen::contents(&parser).contains(CARD_SETTLING));
        assert_eq!(screen::complete(&parser, &bytes), bytes);
        assert!(appears(&bytes) && gone(&bytes));
    }
    let next = sync::frame(
        format!("\x1b[2J{}\x1b[Hafter", sync::begin()).as_bytes(),
        CursorAfter::Shown,
    );
    let mut bytes = [before, &frame, &next[..next.len() - 1]].concat();
    let mut parser = screen::parsed(&bytes, 30, 100);
    assert!(screen::contents(&parser).contains(CARD_SETTLING));
    assert_eq!(screen::complete(&parser, &bytes), [before, &frame].concat());
    let tail = [&next[next.len() - 1..], b"\x1b[?25htail"].concat();
    bytes.extend_from_slice(&tail);
    screen::process(&mut parser, &tail);
    assert_eq!(screen::contents(&parser), "aftertail");
    assert_eq!(screen::complete(&parser, &bytes), bytes);
}

fn quit_app(pty: &mut Pty, stop: bool) {
    if stop {
        pty.send(b"\x03");
    }
    let start = Instant::now();
    while find(&pty.out, EXITED, 0).is_none() {
        assert!(
            start.elapsed() < LIMIT,
            "never quit: {:?}",
            String::from_utf8_lossy(&pty.out)
        );
        pty.send(b"\x04");
        pty.until(EXITED, Duration::from_millis(300));
    }
}

fn mid_answer(role: &str) -> Pty {
    let mut pty = spawn_app(role, &[]);
    streaming(&mut pty, role);
    pty
}

fn streaming(pty: &mut Pty, label: &str) {
    pty.send(b"go\r");
    assert!(
        until_screen(pty, &format!("w{LONG_SEEN} ")),
        "{label}: nothing streamed"
    );
    assert!(
        until_screen(pty, &copy::esc_to_stop().text()),
        "{label}: no status row"
    );
}

fn answered(role: &str, extra: &[(&str, &str)], end: &str) -> Pty {
    let mut pty = spawn_app(role, extra);
    pty.send(b"go\r");
    assert!(
        until_screen(&mut pty, end),
        "{role}: no answer: {}",
        visible(&pty.out)
    );
    assert!(
        until_screen(&mut pty, copy::COST_UNKNOWN),
        "{role}: no turn footer: {}",
        visible(&pty.out)
    );
    pty.drain_idle();
    pty
}

fn is_full_repaint(bytes: &[u8]) -> bool {
    (1..=24).all(|row| find(bytes, &format!("\x1b[{row};1H"), 0).is_some())
}

fn assert_main_screen_clean(out: &[u8], label: &str) {
    let parser = screen(out);
    let s = parser.screen();
    assert!(
        !s.alternate_screen(),
        "{label}: still on the alternate screen"
    );
    assert!(!s.hide_cursor(), "{label}: cursor hidden");
    assert!(!s.bracketed_paste(), "{label}: bracketed paste");
    assert_eq!(
        s.mouse_protocol_mode(),
        vt100::MouseProtocolMode::None,
        "{label}: mouse reports left on"
    );
    let text = transcript(out).join("\n");
    assert!(text.contains(BEFORE), "{label}: {text}");
    for gone in [
        placeholder(),
        &copy::esc_to_stop().text(),
        AccountKind::ClaudeApi.label(),
        copy::TAGLINE_OWN,
    ] {
        assert!(!text.contains(gone), "{label}: {gone:?} left in {text}");
    }
}
#[test]
fn a_fatal_signal_restore_survives_an_apply_loop() {
    dies_restored_while_the_app_keeps_drawing("apply-loop");
}

#[test]
fn a_fatal_signal_restore_survives_a_render_loop() {
    dies_restored_while_the_app_keeps_drawing("render-loop");
}

#[test]
fn after_a_recovered_panic_writes_refuse_until_resume_reapplies_the_modes() {
    let mut pty = Pty::spawn("panic-recover", 0);
    assert!(pty.until("first-write=", LIMIT));
    pty.drain_idle();
    let text = String::from_utf8_lossy(&pty.out).into_owned();
    assert!(text.contains("first-write=Err(Some(Restored))"), "{text:?}");
    assert!(cooked(&pty.termios()));
    pty.kill(Signal::TSTP);
    assert!(pty.wait_stopped(LIMIT, true));
    pty.kill(Signal::CONT);
    assert!(pty.until("recovered", LIMIT));
    let (status, out, termios) = pty.finish(LIMIT);
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("second-write=Ok(Sent) applied=true"),
        "{text:?}"
    );
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
}

fn until_after(pty: &mut Pty, needle: &str, from: usize) -> bool {
    pty.pump_until(LIMIT, |out| find(out, needle, from).is_some())
}

fn frame_after(pty: &mut Pty, needle: &str, from: usize) -> Option<usize> {
    let mut end = None;
    pty.pump_until(LIMIT, |out| {
        end = find(out, needle, from).and_then(|at| find(out, "\x1b[?2026l", at));
        end.is_some()
    });
    end
}

#[test]
fn a_silent_terminal_draws_before_the_late_reply_grace_ends_and_still_drops_late_replies() {
    let mut pty = Pty::spawn_with("app:short", 0, &[("HOME", PTY_HOME)]);
    assert!(pty.until("\x1b[c", LIMIT));
    let asked = Instant::now();
    assert!(until_screen(&mut pty, &placeholder()[1..]), "no composer");
    assert!(
        asked.elapsed() < LATE_GRACE,
        "drawn after {:?}",
        asked.elapsed()
    );
    pty.send(b"z\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\q\x1b[?62;22c");
    assert!(until_screen(&mut pty, "zq"), "{}", visible(&pty.out));
    assert!(!visible(&pty.out).contains("rgb"), "{}", visible(&pty.out));
    quit_app(&mut pty, true);
    let (status, _, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
}

#[test]
fn starting_then_quitting_leaves_the_main_screen_as_it_was_and_prints_the_goodbye() {
    let mut pty = spawn_app("app:short", &[]);
    let entered = find(&pty.out, ENTER_ALT, 0).unwrap();
    let before = screen(&pty.out[..entered]);
    pty.send(b"go\r");
    assert!(until_screen(&mut pty, SHORT_END), "no answer");
    quit_app(&mut pty, false);
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    let after = screen(&out);
    let s = after.screen();
    assert!(!s.alternate_screen());
    let (row, _) = before.screen().cursor_position();
    let row = usize::from(row);
    let old: Vec<String> = before.screen().rows_formatted(0, 80).take(row).collect();
    let new: Vec<String> = s.rows_formatted(0, 80).take(row).collect();
    assert_eq!(new, old, "the main screen changed");
    let rows: Vec<String> = s.rows(0, 80).map(|l| l.trim_end().to_owned()).collect();
    assert!(
        rows[row].ends_with("   wisp \u{b7} Bye for now."),
        "{rows:#?}"
    );
    assert!(
        rows[row + 2].ends_with("   Come back any time with:"),
        "{rows:#?}"
    );
    assert_eq!(rows[row + 3], "wisp --resume pty-session", "{rows:#?}");
    assert!(rows[row + 4].starts_with(EXITED), "{rows:#?}");
}

#[test]
fn shift_tab_sent_as_csi_z_cycles_effort_and_slash_model_opens_the_picker() {
    let mut pty = spawn_app("app:pickers", &[]);
    pty.send(b"\x1b[Z");
    assert!(
        until_screen(
            &mut pty,
            format!("Effort \u{2192} medium \u{b7} {CACHE_KEPT}").as_str()
        ),
        "{}",
        visible(&pty.out)
    );
    assert!(until_screen(&mut pty, "opus-5-5 medium"));
    pty.send(b"/model\r");
    assert!(
        until_screen(&mut pty, "type to filter"),
        "{}",
        visible(&pty.out)
    );
    pty.send(b"\x1b");
    assert!(
        until_gone(&mut pty, "type to filter"),
        "{}",
        visible(&pty.out)
    );
    pty.send(b"go\r");
    assert!(until_screen(&mut pty, SHORT_END), "no answer");
    quit_app(&mut pty, false);
    let (status, _, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
}

#[test]
fn mouse_reports_are_on_by_default_and_wisp_mouse_0_leaves_them_off() {
    for (extra, mouse) in [(vec![], true), (vec![("WISP_MOUSE", "0")], false)] {
        let select = "Select in the terminal";
        let mut pty = spawn_app("app:short", &extra);
        assert!(
            until_screen(&mut pty, "/help \u{b7} /model switch model"),
            "mouse {mouse}: no key line"
        );
        pty.send(b"/help\r");
        assert!(
            until_screen(&mut pty, copy::HELP_LEAD),
            "mouse {mouse}: no help page: {}",
            visible(&pty.out)
        );
        pty.send(b"G");
        assert!(
            until_screen(&mut pty, select),
            "mouse {mouse}: no help: {}",
            visible(&pty.out)
        );
        let shown = visible(&pty.out);
        assert_eq!(
            shown.contains("shift-drag"),
            mouse,
            "mouse {mouse}: {shown}"
        );
        pty.send(b"q");
        assert!(
            until_gone(&mut pty, select),
            "mouse {mouse}: help never closed: {}",
            visible(&pty.out)
        );
        pty.send(b"\x04");
        let (status, out, termios) = pty.finish(LIMIT);
        assert_eq!(status.exit_status(), Some(0), "mouse {mouse}");
        assert!(cooked(&termios));
        let text = String::from_utf8_lossy(&out);
        for on in MOUSE_ON {
            assert_eq!(text.contains(on), mouse, "mouse {mouse}: {on:?}");
        }
        assert!(!text.contains("\x1b[?1003h"), "any-motion tracking");
        let off = find(&out, SCROLL_OFF, 0).unwrap_or_else(|| panic!("mouse {mouse}: no ?1007l"));
        assert!(
            rfind(&out, SCROLL_BACK).is_some_and(|back| back > off),
            "mouse {mouse}: 1007 not put back"
        );
        assert_main_screen_clean(&out, &format!("mouse {mouse}"));
    }
}

#[test]
fn sgr_wheel_reports_scroll_the_view_and_ctrl_end_returns_to_the_bottom() {
    let mut pty = answered("app:page", &[], PAGE_END);
    let first = "Paragraph p0 of";
    assert!(!visible(&pty.out).contains(first));
    for _ in 0..30 {
        pty.send(b"\x1b[<64;10;5M");
    }
    assert!(until_screen(&mut pty, first), "the wheel never scrolled up");
    pty.drain_idle();
    assert!(!visible(&pty.out).contains(PAGE_END));
    pty.send(b"\x1b[<65;10;5M");
    pty.send(b"\x1b[1;5F");
    assert!(until_screen(&mut pty, PAGE_END), "ctrl-end never returned");
    pty.drain_idle();
    assert!(!visible(&pty.out).contains(first));
    pty.send(b"\x04");
    let (status, out, _) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert_main_screen_clean(&out, "wheel");
}

#[test]
fn a_worker_panic_restores_prints_and_the_app_reenters_and_repaints() {
    let mut pty = mid_answer("app:long:1");
    let recovered_line = "Recovered from an internal error (1 of 3)";
    assert!(recovered(1).starts_with(recovered_line));
    assert!(until_screen(&mut pty, recovered_line), "no recovery notice");
    assert!(
        frame_after(&mut pty, recovered_line, 0).is_some(),
        "no frame with the recovery notice"
    );
    let out = pty.out.clone();
    let entered = find(&out, ENTER_ALT, 0).unwrap();
    let boom = find(&out, PANIC_MESSAGE, entered).unwrap();
    let left = rfind(&out[..boom], LEAVE_ALT).unwrap();
    let off = rfind(&out[..left], MOUSE_OFF).unwrap();
    let back = rfind(&out[..left], SCROLL_BACK).unwrap();
    assert!(entered < off && off < back && back < left, "restore order");
    let again = find(&out, ENTER_ALT, boom).unwrap();
    let rest = &out[again..];
    for on in MOUSE_ON {
        assert!(find(rest, on, 0).is_some(), "{on:?} not re-sent");
    }
    assert!(find(rest, SCROLL_OFF, 0).is_some());
    assert!(is_full_repaint(rest), "no full repaint after re-entering");
    quit_app(&mut pty, true);
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    assert_main_screen_clean(&out, "after a recovered panic");
    assert!(visible(&out).contains(PANIC_MESSAGE));
}

#[test]
fn a_worker_panic_with_the_reader_open_restores_the_terminal_in_order() {
    let scratch = ScratchDir::new(SCRATCH, "fold");
    let signal = scratch.root.join("signal");
    let path = signal.to_string_lossy().into_owned();
    let mut pty = answered("app:fold:1", &[(PANIC_FILE, path.as_str())], FOLD_END);
    let (col, row) = fold_spot(&rows(&pty.out), FOLD_THOUGHT)
        .unwrap_or_else(|| panic!("no fold row: {}", visible(&pty.out)));
    pty.send(format!("\x1b[<0;{};{}M", col + 1, row + 1).as_bytes());
    pty.send(format!("\x1b[<0;{};{}m", col + 1, row + 1).as_bytes());
    assert!(
        until_screen(&mut pty, &back_keys().text()),
        "{}",
        visible(&pty.out)
    );
    assert!(
        visible(&pty.out).contains("Thought line 1 about"),
        "{}",
        visible(&pty.out)
    );
    panics_and_restores(pty, &signal, &back_keys().text(), b"q");
}

#[test]
fn a_worker_panic_with_a_reloading_tool_page_open_restores_the_terminal_in_order() {
    let scratch = ScratchDir::new(SCRATCH, "live");
    let signal = scratch.root.join("signal");
    let log = scratch.root.join("live.out");
    std::fs::write(&log, "live line 1\n").unwrap();
    let signal_path = signal.to_string_lossy().into_owned();
    let log_path = log.to_string_lossy().into_owned();
    let mut pty = answered(
        "app:live:1",
        &[
            (PANIC_FILE, signal_path.as_str()),
            (LIVE_FILE, log_path.as_str()),
        ],
        LIVE_END,
    );
    let (col, row) = row_spot(&rows(&pty.out), LIVE_ROW)
        .unwrap_or_else(|| panic!("no tool row: {}", visible(&pty.out)));
    pty.send(format!("\x1b[<0;{};{}M", col + 1, row + 1).as_bytes());
    pty.send(format!("\x1b[<0;{};{}m", col + 1, row + 1).as_bytes());
    assert!(
        until_screen(&mut pty, &back_keys().text()),
        "{}",
        visible(&pty.out)
    );
    assert!(
        until_screen(&mut pty, "live line 1"),
        "{}",
        visible(&pty.out)
    );
    let mut more = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    std::io::Write::write_all(&mut more, b"live line 2\n").unwrap();
    assert!(
        until_screen(&mut pty, "live line 2"),
        "{}",
        visible(&pty.out)
    );
    panics_and_restores(pty, &signal, &back_keys().text(), b"q");
}

#[test]
fn a_worker_panic_with_the_reader_opened_from_the_picker_restores_the_terminal_in_order() {
    let scratch = ScratchDir::new(SCRATCH, "pick");
    let signal = scratch.root.join("signal");
    let path = signal.to_string_lossy().into_owned();
    let mut pty = answered("app:fold:1", &[(PANIC_FILE, path.as_str())], FOLD_END);
    pty.send(b"\x0f");
    assert!(
        until_screen(&mut pty, copy::EVERY_CELL),
        "{}",
        visible(&pty.out)
    );
    pty.send(b"\r");
    assert!(
        until_screen(&mut pty, &back_keys().text()),
        "{}",
        visible(&pty.out)
    );
    panics_and_restores(pty, &signal, &back_keys().text(), b"q");
}

#[test]
fn a_worker_panic_with_the_picker_open_restores_the_terminal_in_order() {
    let scratch = ScratchDir::new(SCRATCH, "picker");
    let signal = scratch.root.join("signal");
    let path = signal.to_string_lossy().into_owned();
    let mut pty = answered("app:fold:1", &[(PANIC_FILE, path.as_str())], FOLD_END);
    pty.send(b"\x0f");
    assert!(
        until_screen(&mut pty, copy::EVERY_CELL),
        "{}",
        visible(&pty.out)
    );
    panics_and_restores(pty, &signal, copy::EVERY_CELL, b"\x1b");
}

fn panics_and_restores(mut pty: Pty, signal: &std::path::Path, shown: &str, close: &[u8]) {
    let mark = pty.out.len();
    std::fs::write(signal, b"").unwrap();
    assert!(pty.until(PANIC_MESSAGE, LIMIT), "no panic printed");
    let printed = find(&pty.out, PANIC_MESSAGE, mark).unwrap();
    assert!(
        frame_after(&mut pty, ENTER_ALT, printed).is_some(),
        "never re-entered: {}",
        visible(&pty.out)
    );
    assert!(until_screen(&mut pty, shown), "{}", visible(&pty.out));
    let out = pty.out.clone();
    let boom = find(&out, PANIC_MESSAGE, mark).unwrap();
    let left = rfind(&out[..boom], LEAVE_ALT).unwrap();
    let off = rfind(&out[..left], MOUSE_OFF).unwrap();
    let back = rfind(&out[..left], SCROLL_BACK).unwrap();
    assert!(mark < off && off < back && back < left, "restore order");
    let again = find(&out, ENTER_ALT, boom).unwrap();
    for on in MOUSE_ON {
        assert!(find(&out[again..], on, 0).is_some(), "{on:?} not re-sent");
    }
    pty.send(close);
    assert!(until_gone(&mut pty, shown), "{}", visible(&pty.out));
    quit_app(&mut pty, false);
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    assert_main_screen_clean(&out, shown);
}

#[test]
fn more_panics_than_the_limit_end_the_app_with_its_error() {
    let restores = usize::from(MAX_RESTORES) + 1;
    let pty = mid_answer(&format!("app:long:{restores}"));
    let (status, out, termios) = pty.finish(LIMIT);
    let text = String::from_utf8_lossy(&out);
    assert_eq!(status.exit_status(), Some(0), "{text:?}");
    assert!(text.contains(&format!("Panics({restores})")), "{text:?}");
    assert!(cooked(&termios));
    assert_main_screen_clean(&out, "too many panics");
}

#[test]
fn sigterm_and_sighup_restore_the_app_without_erasing_the_main_screen() {
    for signal in [Signal::TERM, Signal::HUP] {
        let pty = mid_answer("app:long");
        let mark = pty.out.len();
        pty.kill(signal);
        let (status, out, termios) = pty.finish(LIMIT);
        assert_eq!(status.terminating_signal(), Some(signal.as_raw()));
        assert!(cooked(&termios), "{signal:?}");
        let left = find(&out, LEAVE_ALT, mark).unwrap();
        for erase in ERASES {
            assert!(
                find(&out, erase, left).is_none(),
                "{signal:?}: {erase:?} after leaving"
            );
        }
        assert_main_screen_clean(&out, &format!("{signal:?}"));
    }
}

#[test]
fn a_held_death_ends_an_idle_app_at_once_restored() {
    let extra = [(HOLD_MS, "3000"), ("WISP_MOTION", "reduced")];
    let mut pty = answered("app:short", &extra, SHORT_END);
    let mark = pty.out.len();
    let sent = Instant::now();
    pty.kill(Signal::TERM);
    let status = pty.wait_exit(Duration::from_secs(5), false).unwrap();
    let took = sent.elapsed();
    pty.drain_idle();
    assert_eq!(status.terminating_signal(), Some(Signal::TERM.as_raw()));
    assert!(took < Duration::from_millis(900), "{took:?}");
    assert!(cooked(&pty.termios()));
    assert!(find(&pty.out, EXITED, mark).is_none());
    assert_main_screen_clean(&pty.out, "held");
}

#[test]
fn an_outside_sigtstp_leaves_the_screen_and_sigcont_repaints_it_in_full() {
    let mut pty = mid_answer("app:long");
    pty.kill(Signal::TSTP);
    assert!(pty.wait_stopped(LIMIT, true), "never stopped");
    pty.drain_idle();
    assert!(cooked(&pty.termios()));
    assert_main_screen_clean(&pty.out, "stopped");
    let mark = pty.out.len();
    pty.kill(Signal::CONT);
    let end = frame_after(&mut pty, &placeholder()[1..], mark)
        .unwrap_or_else(|| panic!("never repainted: {}", visible(&pty.out)));
    assert!(is_full_repaint(&pty.out[mark..end]));
    assert!(screen(&pty.out).screen().alternate_screen());
    pty.kill(Signal::TERM);
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.terminating_signal(), Some(Signal::TERM.as_raw()));
    assert!(cooked(&termios));
    assert_main_screen_clean(&out, "terminated after resuming");
}

#[test]
fn an_outside_sigtstp_in_transcript_mode_leaves_the_screen_cleanly() {
    let mut pty = mid_answer("app:long");
    let mark = pty.out.len();
    pty.send(b"\x0f");
    assert!(
        frame_after(&mut pty, return_keys().key(), mark).is_some(),
        "no transcript frame"
    );
    assert!(
        find(&pty.out, ENTER_ALT, mark).is_none(),
        "transcript mode re-entered the alternate screen"
    );
    pty.kill(Signal::TSTP);
    assert!(pty.wait_stopped(LIMIT, true), "never stopped");
    pty.drain_idle();
    assert!(cooked(&pty.termios()));
    assert_main_screen_clean(&pty.out, "stopped in transcript mode");
    pty.kill(Signal::TERM);
    pty.kill(Signal::CONT);
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.terminating_signal(), Some(Signal::TERM.as_raw()));
    assert!(cooked(&termios));
    assert_main_screen_clean(&out, "terminated from transcript mode");
}
fn app_line(role: &str) -> String {
    let exe = std::env::current_exe().unwrap();
    format!(
        "HOME={PTY_HOME} {DEADLINE}=0 {ROLE}={role} '{}' --exact pty_child --nocapture --test-threads=1",
        exe.display()
    )
}

fn killed_by_its_shell() -> Option<String> {
    let mut sh = Pty::shell()?;
    if !sh.until("$ ", LIMIT) {
        return None;
    }
    sh.send(format!("{}\r", app_line("app:long")).as_bytes());
    answer_probe(&mut sh);
    sh.send(b"\x1a");
    assert!(
        sh.until("Stopped", LIMIT),
        "{:?}",
        String::from_utf8_lossy(&sh.out)
    );
    let mark = sh.out.len();
    sh.send(b"kill %1; sleep 1.5; jobs -l; kill -9 %1 2>/dev/null; echo JOBS-$((1+1))\r");
    assert!(
        sh.until("JOBS-2", LIMIT),
        "{:?}",
        String::from_utf8_lossy(&sh.out)
    );
    let after = String::from_utf8_lossy(&sh.out[mark..]).into_owned();
    sh.send(b"exit\r");
    sh.pump(Duration::from_millis(100));
    sh.send(b"exit\r");
    drop(sh.finish(LIMIT));
    Some(after)
}

#[test]
fn a_suspended_app_killed_by_its_shell_dies_instead_of_stopping() {
    let Some(after) = killed_by_its_shell() else {
        eprintln!("skipped: setsid or bash is not usable");
        return;
    };
    assert!(after.contains("Terminated"), "{after:?}");
    assert!(!after.contains("Stopped"), "{after:?}");
}

fn shell_with_the_app(prefix: &str, go: bool) -> Option<(Pty, Pid)> {
    let mut sh = Pty::shell()?;
    if !sh.until("$ ", LIMIT) {
        return None;
    }
    let line = app_line("app:long").replace('\'', "\"");
    sh.send(format!("{prefix}sh -c 'echo APP-PID-$$-; exec env {line}'\r").as_bytes());
    assert!(sh.until("APP-PID-", LIMIT), "never started");
    answer_probe(&mut sh);
    let text = String::from_utf8_lossy(&sh.out).into_owned();
    let pid = text
        .split("APP-PID-")
        .filter_map(|rest| rest.split('-').next()?.parse::<i32>().ok())
        .last()
        .and_then(Pid::from_raw)
        .unwrap_or_else(|| panic!("{text:?}"));
    if go {
        streaming(&mut sh, "shell");
    }
    Some((sh, pid))
}

fn shell_running_the_app(prefix: &str) -> Option<Pty> {
    let (mut sh, _) = shell_with_the_app(prefix, false)?;
    sh.send(b"\x1a");
    assert!(
        sh.until("Stopped", LIMIT),
        "{:?}",
        String::from_utf8_lossy(&sh.out)
    );
    Some(sh)
}

fn stopped_from_outside(sh: &mut Pty, pid: Pid, signal: Signal) {
    let mark = sh.out.len();
    rustix::process::kill_process(pid, signal).unwrap();
    assert!(
        until_after(sh, "Stopped", mark),
        "{:?}",
        String::from_utf8_lossy(&sh.out[mark..])
    );
    assert!(until_after(sh, "$ ", mark), "no prompt");
}

fn screen_lines(out: &[u8]) -> Vec<String> {
    transcript(out)
        .into_iter()
        .map(|l| l.trim_end().to_owned())
        .collect()
}

fn moves_the_cursor(bytes: &str) -> bool {
    let mut rest = bytes;
    while let Some(at) = rest.find("\x1b[") {
        let tail = &rest[at + 2..];
        let params: String = tail
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == ';')
            .collect();
        if tail[params.len()..].starts_with(['H', 'f', 'A', 'B', 'C', 'D', 'd', 'G']) {
            return true;
        }
        rest = tail;
    }
    false
}

#[test]
fn fg_after_an_outside_stop_repaints_in_full_and_keeps_the_shells_lines() {
    let Some((mut sh, _)) = shell_with_the_app("", true) else {
        eprintln!("skipped: setsid or bash is not usable");
        return;
    };
    let mark = sh.out.len();
    sh.send(b"\x1a");
    assert!(until_after(&mut sh, "Stopped", mark), "never stopped");
    assert!(until_after(&mut sh, "$ ", mark), "no prompt");
    let mark = sh.out.len();
    sh.send(b"fg\r");
    assert!(until_after(&mut sh, ENTER_ALT, mark), "never re-entered");
    let entered = find(&sh.out, ENTER_ALT, mark).unwrap();
    assert!(
        frame_after(&mut sh, &placeholder()[1..], entered).is_some(),
        "never repainted"
    );
    assert!(is_full_repaint(&sh.out[entered..]));
    quit_app(&mut sh, true);
    assert!(until_after(&mut sh, "$ ", mark), "no prompt after quitting");
    sh.drain_idle();
    let lines = screen_lines(&sh.out);
    let stopped = lines
        .iter()
        .rposition(|l| l.starts_with("[1]+  Stopped"))
        .unwrap_or_else(|| panic!("{lines:#?}"));
    let fg = stopped
        + lines[stopped..]
            .iter()
            .position(|l| l == "$ fg")
            .unwrap_or_else(|| panic!("the fg line is gone: {lines:#?}"));
    assert!(
        lines
            .get(fg + 1)
            .is_some_and(|l| l.starts_with("sh -c 'echo APP-PID")),
        "the job line is gone: {lines:#?}"
    );
    assert!(
        lines[fg + 1..]
            .iter()
            .any(|l| l.ends_with("wisp \u{b7} Bye for now.")),
        "{lines:#?}"
    );
    for gone in [
        placeholder(),
        &copy::esc_to_stop().text(),
        AccountKind::ClaudeApi.label(),
    ] {
        assert!(
            !lines.iter().any(|l| l.contains(gone)),
            "{gone}: {lines:#?}"
        );
    }
    leave_shell(sh);
}

fn wisp_bytes_since(sh: &Pty, mark: usize) -> String {
    String::from_utf8_lossy(&sh.out[mark..]).into_owned()
}

#[test]
fn after_an_unseen_stop_a_background_restore_only_resets_modes() {
    for (prefix, then) in [
        ("", "bg"),
        ("", "kill"),
        ("stty tostop; ", "bg"),
        ("stty tostop; ", "kill"),
    ] {
        let then = &format!("{prefix}{then}");
        let Some((mut sh, pid)) = shell_with_the_app(prefix, false) else {
            eprintln!("skipped: setsid or bash is not usable");
            return;
        };
        stopped_from_outside(&mut sh, pid, Signal::STOP);
        let mark = sh.out.len();
        let after = if then.ends_with("bg") {
            run_in(&mut sh, "bg; wait %1; echo BG-$?", "BGDONE")
        } else {
            run_in(
                &mut sh,
                "p=$(jobs -p %1); kill %1; while kill -0 $p 2>/dev/null; do sleep 0.02; done; jobs",
                "KILLDONE",
            )
        };
        if then.ends_with("bg") {
            assert!(after.contains("BG-147"), "{then}: {after:?}");
            assert!(!after.contains("tty output"), "{then}: {after:?}");
        } else {
            assert!(after.contains("Terminated"), "{then}: {after:?}");
        }
        let written = wisp_bytes_since(&sh, mark);
        assert!(written.contains("\x1b[?25h"), "{then}: {written:?}");
        assert!(!screen(&sh.out).screen().hide_cursor(), "{then}");
        let off = written.find(MOUSE_OFF);
        let back = written.find(SCROLL_BACK);
        let left = written.find(LEAVE_ALT);
        assert!(
            off.is_some() && off < back && back < left,
            "{then}: {written:?}"
        );
        for wrote in [ENTER_ALT, SCROLL_OFF, "\x1b[?2026h"]
            .into_iter()
            .chain(MOUSE_ON)
        {
            assert!(!written.contains(wrote), "{then}: {wrote:?} in {written:?}");
        }
        for erase in ERASES {
            assert!(!written.contains(erase), "{then}: {erase:?} in {written:?}");
        }
        assert!(!moves_the_cursor(&written), "{then}: {written:?}");
        let typed = run_in(&mut sh, "echo NEXT-$((40+2))", "TYPED");
        assert!(typed.contains("NEXT-42"), "{then}: {typed:?}");
        let lines = screen_lines(&sh.out);
        let echoed = lines
            .iter()
            .rposition(|l| l.contains("echo NEXT-$((40+2))"))
            .unwrap_or_else(|| panic!("{then}: {lines:#?}"));
        assert!(
            lines[echoed].starts_with("$ echo NEXT"),
            "{then}: {lines:#?}"
        );
        let typed_at = find(&sh.out, "TYPED-2", 0).unwrap();
        assert!(
            until_after(&mut sh, "\x1b[?2004h$ ", typed_at),
            "{then}: no prompt"
        );
        assert!(screen(&sh.out).screen().bracketed_paste(), "{then}");
        let _ = rustix::process::kill_process(pid, Signal::KILL);
        leave_shell(sh);
    }
}

fn run_in(sh: &mut Pty, command: &str, marker: &str) -> String {
    let mark = sh.out.len();
    sh.send(format!("{command}; echo {marker}-$((1+1))\r").as_bytes());
    let done = format!("{marker}-2");
    assert!(
        until_after(sh, &done, mark),
        "{:?}",
        String::from_utf8_lossy(&sh.out[mark..])
    );
    String::from_utf8_lossy(&sh.out[mark..]).into_owned()
}

fn leave_shell(mut sh: Pty) {
    sh.send(b"kill -9 %1 2>/dev/null; exit\r");
    sh.pump(Duration::from_millis(100));
    sh.send(b"exit\r");
    drop(sh.finish(LIMIT));
}

#[test]
fn bg_stops_the_app_again_without_touching_the_shell_and_kill_ends_it() {
    for prefix in ["", "trap '' TTOU; "] {
        let Some(mut sh) = shell_running_the_app(prefix) else {
            eprintln!("skipped: setsid or bash is not usable");
            return;
        };
        let bg = run_in(
            &mut sh,
            "bg; wait %1; echo BG-$?; jobs -l; stty -a | tr ' ;' '\\n\\n' | grep -x -- '-\\?icanon'",
            "BGDONE",
        );
        assert!(bg.contains("BG-147"), "{prefix}: {bg:?}");
        for wrote in [
            ENTER_ALT,
            LEAVE_ALT,
            MOUSE_OFF,
            SCROLL_OFF,
            SCROLL_BACK,
            "\x1b[?2026",
            "\x1b[?25",
        ]
        .into_iter()
        .chain(MOUSE_ON)
        {
            assert!(
                !bg.contains(wrote),
                "{prefix}: SIGCONT in the background wrote {wrote:?}: {bg:?}"
            );
        }
        assert!(bg.contains("Stopped (signal)"), "{prefix}: {bg:?}");
        assert!(!bg.contains("tty output"), "{prefix}: {bg:?}");
        assert!(
            bg.contains("\nicanon"),
            "{prefix}: the shell's tty went raw: {bg:?}"
        );
        let kill = run_in(
            &mut sh,
            "p=$(jobs -p %1); kill %1; while kill -0 $p 2>/dev/null; do sleep 0.02; done; jobs",
            "KILLDONE",
        );
        assert!(kill.contains("Terminated"), "{prefix}: {kill:?}");
        assert!(!kill.contains("Stopped"), "{prefix}: {kill:?}");
        leave_shell(sh);
    }
}

#[test]
fn fg_after_bg_resumes_the_app_and_it_quits_cooked() {
    let Some(mut sh) = shell_running_the_app("") else {
        eprintln!("skipped: setsid or bash is not usable");
        return;
    };
    let bg = run_in(&mut sh, "bg; wait %1; echo BG-$?", "BGDONE");
    assert!(bg.contains("BG-147"), "{bg:?}");
    let mark = sh.out.len();
    sh.send(b"fg\r");
    assert!(until_after(&mut sh, ENTER_ALT, mark), "never re-entered");
    assert!(
        frame_after(&mut sh, &placeholder()[1..], mark).is_some(),
        "{:?}",
        String::from_utf8_lossy(&sh.out[mark..])
    );
    sh.send(b"\x04");
    assert!(until_after(&mut sh, EXITED, mark), "never quit");
    let status = run_in(
        &mut sh,
        "echo FG-$?; stty -a | tr ' ;' '\\n\\n' | grep -x -- '-\\?icanon'",
        "FGDONE",
    );
    assert!(status.contains("FG-0"), "{status:?}");
    assert!(status.contains("\nicanon"), "{status:?}");
    leave_shell(sh);
}

const QUIET: Duration = Duration::from_millis(1500);

fn hung_up_without_a_session(go: bool) {
    let label = if go { "mid-answer" } else { "idle" };
    let scratch = ScratchDir::new(SCRATCH, label);
    let ended = scratch.root.join("ended");
    let path = ended.to_str().unwrap();
    let mut pty = Pty::spawn_without_a_session(
        "app:long",
        &[
            (APP_ENDED, path),
            ("HOME", PTY_HOME),
            ("WISP_MOTION", "reduced"),
        ],
    );
    answer_probe(&mut pty);
    if go {
        streaming(&mut pty, label);
    } else {
        pty.send(b"\x1b[O");
        assert!(pty.goes_quiet(QUIET, LIMIT), "{label}: never went quiet");
    }
    pty.master = None;
    let status = pty.wait_exit(Duration::from_secs(3), false);
    assert!(status.is_some(), "{label}: still running after the hangup");
    assert_eq!(status.unwrap().terminating_signal(), None, "{label}");
    let reason = std::fs::read_to_string(&ended).unwrap_or_default();
    assert_eq!(
        reason,
        format!("Ok({:?})", Exit::InputStopped(HUNG_UP.to_owned())),
        "{label}"
    );
}

#[test]
fn a_terminal_hangup_without_a_signal_ends_the_app() {
    hung_up_without_a_session(false);
    hung_up_without_a_session(true);
}

fn narrow_rows(bytes: &[u8]) -> Vec<String> {
    screen::parsed(bytes, 24, 60).screen().rows(0, 60).collect()
}

#[test]
fn a_resize_before_any_input_redraws_at_the_new_width() {
    let mut pty = spawn_app("app:short", &[("WISP_MOTION", "reduced")]);
    assert!(pty.goes_quiet(QUIET, LIMIT), "never went quiet");
    redraws_at_sixty_columns(pty);
}

#[test]
fn a_resize_in_a_silent_terminals_late_reply_grace_redraws_at_the_new_width() {
    let mut pty = Pty::spawn_with(
        "app:short",
        0,
        &[("HOME", PTY_HOME), ("WISP_MOTION", "reduced")],
    );
    assert!(until_screen(&mut pty, &placeholder()[1..]), "no composer");
    redraws_at_sixty_columns(pty);
}

fn redraws_at_sixty_columns(mut pty: Pty) {
    let mark = pty.out.len();
    rustix::termios::tcsetwinsize(
        &pty.slave,
        Winsize {
            ws_row: 24,
            ws_col: 60,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    let rule = format!("\u{256D}{}\u{256E}", "\u{2500}".repeat(58));
    let drawn = frame_after(&mut pty, &rule, mark);
    let rows = narrow_rows(&pty.out[mark..]);
    assert!(
        drawn.is_some() && is_full_repaint(&pty.out[mark..]) && rows.contains(&rule),
        "the resize was never seen: {:?}",
        String::from_utf8_lossy(&pty.out[mark..])
    );
    let account = AccountKind::ClaudeApi.label();
    let suffix = format!("{account}   pty-model low \u{2502}");
    assert!(
        rows.iter()
            .any(|row| row.ends_with(&suffix) && row.chars().count() == 60),
        "{rows:#?}"
    );
    quit_app(&mut pty, false);
}

const HANDED_OFF: &str = "wisp: transcript above";

#[test]
fn the_hand_off_turns_mouse_reports_off_before_leaving_and_back_on_after() {
    let mut pty = answered("app:page", &[], PAGE_END);
    let mark = pty.out.len();
    pty.send(b"\x0f");
    assert!(
        frame_after(&mut pty, return_keys().key(), mark).is_some(),
        "no transcript frame"
    );
    pty.drain_idle();
    let mark = pty.out.len();
    pty.send(b"[");
    assert!(until_after(&mut pty, HANDED_OFF, mark), "no hand-off");
    pty.drain_idle();
    let leave = find(&pty.out, LEAVE_ALT, mark).unwrap_or_else(|| panic!("never left"));
    let off = find(&pty.out, MOUSE_OFF, mark).unwrap_or_else(|| panic!("mouse left on"));
    let back = find(&pty.out, SCROLL_BACK, mark).unwrap_or_else(|| panic!("1007 not put back"));
    assert!(off < leave && back < leave, "{off} {back} {leave}");
    let printed = String::from_utf8_lossy(&pty.out[leave..]).into_owned();
    assert!(!printed.contains(ENTER_ALT), "{printed:?}");
    let shown = screen(&pty.out);
    assert!(!shown.screen().alternate_screen());
    assert_eq!(
        shown.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );
    assert!(visible(&pty.out).contains(PAGE_END));
    let idle = pty.out.len();
    pty.send(b"\x1b[<64;10;5M");
    pty.send(b"\x1b[<0;10;5M\x1b[<0;10;5m");
    assert!(pty.goes_quiet(Duration::from_millis(400), LIMIT));
    assert_eq!(pty.out.len(), idle, "drew while handed off");
    pty.send(b"q");
    assert!(until_after(&mut pty, ENTER_ALT, idle), "never came back");
    assert!(
        frame_after(&mut pty, return_keys().key(), idle).is_some(),
        "no transcript frame after returning"
    );
    pty.drain_idle();
    let returned = &pty.out[idle..];
    for on in MOUSE_ON {
        assert!(find(returned, on, 0).is_some(), "{on:?} not reapplied");
    }
    assert!(
        find(returned, SCROLL_OFF, 0).is_some(),
        "1007 not turned off"
    );
    assert!(is_full_repaint(returned), "no full repaint");
    pty.send(b"q");
    pty.drain_idle();
    quit_app(&mut pty, false);
    let (status, out, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
    let lines = transcript(&out);
    let p0 = lines
        .iter()
        .position(|l| l.contains("Paragraph p0 of"))
        .unwrap_or_else(|| panic!("the hand-off left no scrollback: {lines:#?}"));
    let end = lines
        .iter()
        .position(|l| l.contains(HANDED_OFF))
        .unwrap_or_else(|| panic!("{lines:#?}"));
    assert!(p0 < end);
    assert!(lines[p0..end].iter().any(|l| l.contains(PAGE_END)));
    assert_main_screen_clean(&out, "after the hand-off");
}

const PROBE_TOOL: &str = r#"#!/bin/sh
d="$(dirname "$0")"
n="$(basename "$0")"
t=no
if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then t=yes; fi
read -r pid comm state ppid pgrp session tty tpgid rest < /proc/$$/stat
o=closed
if ( exec 3<>/dev/tty ) 2>/dev/null; then o=opened; fi
echo "$t $pgrp $tpgid $o" >> "$d/$n.probe"
printf '%s\n' "$*" >> "$d/$n.args"
{ /bin/cat; printf '\n--\n'; } >> "$d/$n.in"
if [ "$n" = powershell.exe ] && [ -f "$d/no-windows" ]; then exit 1; fi
if [ -f "$d/hang" ]; then
  /bin/sleep 60 &
  echo $! > "$d/grandchild"
  wait
fi
"#;

#[test]
fn a_clipboard_writer_runs_off_the_terminal_with_the_selection_on_stdin() {
    let dir = probe_tools("probe");
    std::fs::write(dir.root.join("no-windows"), "").unwrap();
    let path = format!("{}:/usr/bin:/bin", dir.root.display());
    let mut pty = answered(
        "app:short",
        &[
            ("PATH", path.as_str()),
            ("WISP_COPY_ON_SELECT", "1"),
            ("DISPLAY", ":0"),
            ("WAYLAND_DISPLAY", ""),
            ("SSH_CONNECTION", ""),
            ("SSH_TTY", ""),
            ("ZELLIJ", ""),
        ],
        SHORT_END,
    );
    let (col, row) = row_spot(&rows(&pty.out), "short answer")
        .unwrap_or_else(|| panic!("{}", visible(&pty.out)));
    let (y, x) = (row + 1, col + 1);
    let last = x + 11;
    pty.send(format!("\x1b[<0;{x};{y}M\x1b[<32;{last};{y}M\x1b[<0;{last};{y}m").as_bytes());
    assert!(
        until_screen(&mut pty, "Copied 12 characters"),
        "no copy toast"
    );
    pty.drain_idle();
    let probes: Vec<_> = ["powershell.exe", "xclip", "xsel", "wl-copy", "pbcopy"]
        .iter()
        .filter_map(|t| {
            let probe = std::fs::read_to_string(dir.root.join(format!("{t}.probe"))).ok()?;
            let input = std::fs::read_to_string(dir.root.join(format!("{t}.in"))).ok()?;
            let args = std::fs::read_to_string(dir.root.join(format!("{t}.args"))).ok()?;
            Some((t.to_owned(), probe, input, args))
        })
        .collect();
    quit_app(&mut pty, false);
    let (status, _, _) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    let ran: Vec<&str> = probes.iter().map(|(t, ..)| *t).collect();
    assert!(
        ran == ["xclip"] || ran == ["powershell.exe", "xclip"],
        "{ran:?}"
    );
    for (tool, probe, input, args) in &probes {
        let calls: Vec<&str> = args.lines().collect();
        let inputs: Vec<&str> = input.split_terminator("\n--\n").collect();
        assert_eq!(inputs.len(), calls.len(), "{tool}: {inputs:?} {calls:?}");
        assert!(
            inputs.iter().all(|i| *i == "short answer"),
            "{tool}: {inputs:?}"
        );
        if *tool == "xclip" {
            assert_eq!(
                calls,
                ["-selection clipboard -i", "-selection primary -i"],
                "one copy runs xclip once per selection"
            );
        } else {
            assert_eq!(calls.len(), 1, "{tool}: {calls:?}");
        }
        for line in probe.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(fields.first(), Some(&"no"), "{tool} got the tty: {line}");
            assert_ne!(
                fields.get(1),
                fields.get(2),
                "{tool} is in the foreground: {line}"
            );
            assert_eq!(
                fields.get(3),
                Some(&"closed"),
                "{tool} could open the terminal: {line}"
            );
        }
    }
}

fn probe_tools(name: &str) -> ScratchDir {
    let dir = ScratchDir::new(SCRATCH, name);
    for tool in ["powershell.exe", "xclip", "xsel", "wl-copy", "pbcopy"] {
        fake_tool::install(&dir.root.join(tool), PROBE_TOOL);
    }
    dir
}

#[test]
fn a_hanging_writer_is_killed_with_its_children_at_the_timeout() {
    let dir = probe_tools("hang");
    std::fs::write(dir.root.join("hang"), "").unwrap();
    let path = format!("{}:/usr/bin:/bin", dir.root.display());
    let mut pty = answered(
        "app:short",
        &[
            ("PATH", path.as_str()),
            ("WISP_COPY_ON_SELECT", "0"),
            ("WSL_DISTRO_NAME", "Fake"),
            ("DISPLAY", ""),
            ("WAYLAND_DISPLAY", ""),
            ("SSH_CONNECTION", ""),
            ("SSH_TTY", ""),
            ("ZELLIJ", ""),
        ],
        SHORT_END,
    );
    let (col, row) = row_spot(&rows(&pty.out), "short answer")
        .unwrap_or_else(|| panic!("{}", visible(&pty.out)));
    let (y, x) = (row + 1, col + 1);
    let last = x + 4;
    pty.send(format!("\x1b[<0;{x};{y}M\x1b[<32;{last};{y}M\x1b[<0;{last};{y}m").as_bytes());
    pty.send(b"\x03");
    assert!(
        until_screen(&mut pty, "took longer than 5s"),
        "no timeout toast: {}",
        visible(&pty.out)
    );
    let grandchild = std::fs::read_to_string(dir.root.join("grandchild")).unwrap_or_default();
    let grandchild: Option<i32> = grandchild.trim().parse().ok();
    pty.pump_until(Duration::from_secs(2), |_| !grandchild.is_some_and(alive));
    let killed = grandchild.is_some_and(|pid| !alive(pid));
    quit_app(&mut pty, false);
    let (status, _, _) = pty.finish(LIMIT);
    if !killed && let Some(pid) = grandchild.and_then(Pid::from_raw) {
        kill_process(pid, Signal::KILL).ok();
    }
    assert_eq!(status.exit_status(), Some(0));
    assert!(
        killed,
        "the writer's child {grandchild:?} outlived the timeout"
    );
}

#[test]
fn a_message_typed_during_a_turn_lands_as_a_cell_at_the_next_step() {
    let mut pty = spawn_app("app:steps", &[]);
    pty.send(b"go\r");
    assert!(until_screen(&mut pty, "s1 "), "{}", visible(&pty.out));
    pty.send(b"also this\r");
    assert!(until_screen(&mut pty, STEPS_END), "{}", visible(&pty.out));
    let shown = visible(&pty.out);
    let last = format!("s{} ", STEP_WORDS - 1);
    let words = shown.find(last.trim_end()).unwrap_or(usize::MAX);
    let steer = shown.find("also this").filter(|&s| {
        shown[s..]
            .lines()
            .next()
            .is_some_and(|l| l.contains("sent while working"))
    });
    let after = shown.find(AFTER_STEER);
    assert!(
        matches!((steer, after), (Some(s), Some(a)) if words < s && s < a),
        "{shown}"
    );
    quit_app(&mut pty, false);
    let (status, _, termios) = pty.finish(LIMIT);
    assert_eq!(status.exit_status(), Some(0));
    assert!(cooked(&termios));
}
