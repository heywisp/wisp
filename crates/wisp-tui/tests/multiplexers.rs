#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any process or file error"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/app_child.rs"]
#[allow(
    dead_code,
    reason = "the tmux tests use only some of the shared child's constants"
)]
mod app_child;

#[path = "support/models.rs"]
#[allow(
    dead_code,
    reason = "the tmux tests use only the shared child's models"
)]
mod models;

#[path = "support/term_env.rs"]
mod term_env;

#[path = "support/scratch.rs"]
mod scratch;

#[path = "../../wisp-render/tests/support/events.rs"]
mod events;

use app_child::{BEFORE, EXITED, PAGE_END, PAGE_PARAGRAPHS, placeholder};
use scratch::ScratchDir;
use wisp_render::chrome::account::AccountKind;
use wisp_render::transcript::copy;
use wisp_tui::app::TMUX_MOUSE_OFF;

const ROLE: &str = "WISP_TUI_TMUX_ROLE";
const SCRATCH: &str = "wisp-tui-";
const HOME: &str = "/home/wisp-tmux";
const AFTER: &str = "SHELL-AFTER-MARK";
const LIMIT: Duration = Duration::from_secs(30);

#[test]
fn tmux_child() {
    if let Ok(role) = std::env::var(ROLE) {
        app_child::run(&role);
    }
}

fn installed(program: &str, version: &str) -> bool {
    Command::new(program)
        .arg(version)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

struct Tmux {
    dir: ScratchDir,
    socket: PathBuf,
    config: PathBuf,
}

impl Tmux {
    fn start(name: &str, mouse: bool) -> Option<Tmux> {
        if !installed("tmux", "-V") {
            eprintln!("tmux is not installed; skipping");
            return None;
        }
        let dir = ScratchDir::new(SCRATCH, name);
        std::fs::set_permissions(&dir.root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = dir.root.join("tmux.conf");
        let mouse = if mouse { "on" } else { "off" };
        std::fs::write(
            &config,
            format!(
                "set -g history-limit 50000\nset -g default-terminal \"tmux-256color\"\nset -s exit-empty off\nset -g mouse {mouse}\nset -g remain-on-exit on\n"
            ),
        )
        .unwrap();
        Some(Tmux {
            socket: dir.root.join("s"),
            dir,
            config,
        })
    }

    fn run(&self, args: &[&str]) -> Output {
        term_env::clean_term_env(&mut Command::new("tmux"))
            .arg("-S")
            .arg(&self.socket)
            .arg("-f")
            .arg(&self.config)
            .args(args)
            .output()
            .unwrap()
    }

    fn launch(&self, role: &str) {
        let exe = std::env::current_exe().unwrap();
        let script = format!(
            "(trap '' HUP; while kill -0 {} 2>/dev/null; do sleep 0.1; done; tmux kill-server; rm -rf '{}') & echo pre-start-1; echo pre-start-2; HOME={HOME} {ROLE}={role} '{}' --exact tmux_child --nocapture --test-threads=1; echo {AFTER}; exec sleep 60",
            std::process::id(),
            self.dir.root.display(),
            exe.display()
        );
        let started = self.run(&[
            "new-session",
            "-d",
            "-x",
            "80",
            "-y",
            "24",
            "-s",
            "app",
            "sh",
            "-c",
            &script,
        ]);
        assert!(started.status.success(), "{started:?}");
    }

    fn pane(&self) -> String {
        let out = self.run(&["capture-pane", "-p", "-t", "app"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn history(&self) -> String {
        let out = self.run(&["capture-pane", "-p", "-S", "-", "-E", "-", "-t", "app"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn until(&self, needle: &str, history: bool) -> String {
        let start = Instant::now();
        loop {
            let text = if history { self.history() } else { self.pane() };
            if text.contains(needle) {
                return text;
            }
            assert!(start.elapsed() < LIMIT, "{needle:?} never appeared: {text}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn quit(&self) -> String {
        let start = Instant::now();
        loop {
            self.keys(&["C-d"]);
            std::thread::sleep(Duration::from_millis(200));
            let history = self.history();
            if history.contains(AFTER) {
                return history;
            }
            assert!(start.elapsed() < LIMIT, "never quit: {history}");
        }
    }

    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", "app"];
        args.extend_from_slice(keys);
        let sent = self.run(&args);
        assert!(sent.status.success(), "{sent:?}");
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        drop(self.run(&["kill-server"]));
    }
}

#[test]
fn tmux_history_holds_only_what_came_before_and_the_summary() {
    let Some(tmux) = Tmux::start("history", true) else {
        return;
    };
    tmux.launch("app:page");
    tmux.until(&placeholder()[1..], false);
    tmux.keys(&["go", "Enter"]);
    tmux.until(PAGE_END, false);
    let history = tmux.quit();
    let lines: Vec<&str> = history.lines().map(str::trim_end).collect();
    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} missing: {lines:#?}"))
    };
    assert!(at("pre-start-1") < at("pre-start-2"));
    assert!(at("pre-start-2") < at(BEFORE));
    let goodbye = at("wisp \u{b7} Bye for now.");
    assert!(at(BEFORE) < goodbye, "{lines:#?}");
    assert_eq!(
        lines[goodbye + 3],
        "wisp --resume pty-session",
        "{lines:#?}"
    );
    assert!(goodbye + 3 < at(EXITED) && at(EXITED) < at(AFTER));
    for gone in [
        placeholder(),
        copy::TAGLINE_OWN,
        "Paragraph p",
        PAGE_END,
        AccountKind::ClaudeApi.label(),
        TMUX_MOUSE_OFF,
    ] {
        assert!(
            !lines.iter().any(|l| l.contains(gone)),
            "{gone:?} reached tmux history: {lines:#?}"
        );
    }
    const { assert!(PAGE_PARAGRAPHS > 24) };
}

fn notice_count(mouse: bool) -> usize {
    let Some(tmux) = Tmux::start(if mouse { "mouse-on" } else { "mouse-off" }, mouse) else {
        return usize::MAX;
    };
    tmux.launch("app:short");
    let pane = tmux.until(&placeholder()[1..], false);
    let start = &TMUX_MOUSE_OFF[..30];
    let shown = pane.matches(start).count();
    tmux.quit();
    shown
}

#[test]
fn tmux_with_the_mouse_off_shows_the_hint_once() {
    let off = notice_count(false);
    if off == usize::MAX {
        return;
    }
    assert_eq!(off, 1);
    assert_eq!(notice_count(true), 0);
}
