#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any pty or process error"
)]

use crate::support;

use std::ffi::OsString;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wisp_render::chrome::account::AccountKind;

use support::pty::{PATIENCE, Pty, READY, Stdout, usable, wait_until};
use support::screen;
use support::server::{Seen, Server, text_reply};
use support::{
    WISP,
    home::{CONFIG_DIR, Home, command_in},
};

const KEYS: &[u8] = b"enter choose \xc2\xb7 esc cancel";

fn start(name: &str, term: &str, stdout_on_tty: bool, extra: &[&str]) -> (Home, Pty) {
    spawn(name, term, stdout_on_tty, extra, false)
}

fn spawn(
    name: &str,
    term: &str,
    stdout_on_tty: bool,
    extra: &[&str],
    broken_log: bool,
) -> (Home, Pty) {
    let home = Home::new(name);
    let pty = spawn_in(&home, term, stdout_on_tty, extra, broken_log);
    (home, pty)
}

fn spawn_in(home: &Home, term: &str, stdout_on_tty: bool, extra: &[&str], broken_log: bool) -> Pty {
    let env_file = home.env_file();
    let mut args: Vec<OsString> = vec![];
    let mut envs: Vec<(&str, OsString)> = vec![];
    if broken_log {
        let file = home.file("not-a-directory");
        std::fs::write(&file, "").unwrap();
        envs.push(("XDG_DATA_HOME", file.into_os_string()));
    } else {
        args.push("--no-log".into());
    }
    args.extend(["--time".into(), "off".into(), "--env-file".into()]);
    args.push(env_file.into_os_string());
    args.extend(extra.iter().map(OsString::from));
    envs.push(("ANTHROPIC_API_KEY", "test-key-not-used".into()));
    launch(home, term, 24, stdout_on_tty, &args, &envs)
}

fn launch(
    home: &Home,
    term: &str,
    rows: u16,
    stdout_on_tty: bool,
    args: &[OsString],
    envs: &[(&str, OsString)],
) -> Pty {
    let mut command = Pty::command(&home.0);
    command
        .args(args)
        .env("TERM", term)
        .envs(envs.iter().map(|(name, value)| (*name, value)));
    let stdout = if stdout_on_tty {
        Stdout::Tty
    } else {
        Stdout::Piped
    };
    Pty::start(&mut command, rows, 80, stdout)
}

impl Pty {
    fn outside_full_screen_after_exit(self) -> String {
        let screen = Arc::clone(&self.screen);
        let (status, _) = self.finish();
        assert!(status.success(), "{status:?}");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let bytes = screen.lock().unwrap().clone();
            let leave = bytes.windows(8).rposition(|w| w == b"\x1b[?1049l");
            if let Some(leave) = leave {
                let enter = bytes.windows(8).position(|w| w == b"\x1b[?1049h").unwrap();
                let mut outside = bytes[..enter].to_vec();
                outside.extend_from_slice(&bytes[leave + 8..]);
                return String::from_utf8_lossy(&outside).into_owned();
            }
            assert!(Instant::now() < deadline, "the full screen never closed");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn auto_with_redirected_stdout_exits_2_with_a_hint() {
    if !usable() {
        return;
    }
    let (_home, s) = start("redirect", "xterm-256color", false, &[]);
    s.wait_for(b"wisp: the full screen needs a terminal; use --output jsonl for scripts and pipes");
    assert!(!s.saw(READY), "piped stdout started the tui");
    let (status, out) = s.finish();
    assert_eq!(status.code(), Some(2), "{status:?}");
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn auto_on_a_dumb_terminal_exits_2_and_names_the_override() {
    if !usable() {
        return;
    }
    let (_home, s) = start("dumb", "dumb", true, &[]);
    s.wait_for(
        b"wisp: the full screen can't be drawn on a dumb terminal (TERM=dumb); --output tui forces it, or use --output jsonl for scripts and pipes",
    );
    assert!(!s.saw(READY), "TERM=dumb started the tui");
    let (status, _) = s.finish();
    assert_eq!(status.code(), Some(2), "{status:?}");
}

#[test]
fn with_no_output_flag_a_terminal_starts_the_tui() {
    if !usable() {
        return;
    }
    let (_home, mut s) = start("auto", "xterm-256color", true, &[]);
    s.wait_for(READY);
    s.wait_for(b"\x1b[?2004h");
    s.send(b"\x04");
    let (status, _) = s.finish();
    assert!(status.success(), "{status:?}");
}

#[test]
fn an_explicit_tui_forces_the_full_screen_on_a_dumb_terminal() {
    if !usable() {
        return;
    }
    let (_home, mut s) = start("forced", "dumb", true, &["--output", "tui"]);
    s.wait_for(READY);
    s.send(b"\x04");
    let (status, _) = s.finish();
    assert!(status.success(), "{status:?}");
}

#[test]
fn an_explicit_tui_off_a_terminal_fails_with_a_reason() {
    if !usable() {
        return;
    }
    let (_home, s) = start("forced-pipe", "xterm-256color", false, &["--output", "tui"]);
    s.wait_for(b"--output tui needs a terminal, but stdout is not a terminal; use --output jsonl");
    let (status, _) = s.finish();
    assert_eq!(status.code(), Some(2), "{status:?}");
}

const LOG_FAILED: &str = "not logging this session: could not open";

#[test]
fn the_tui_says_why_logging_failed_in_a_note_and_never_on_stderr() {
    if !usable() {
        return;
    }
    for extra in [&["--output", "tui"][..], &[]] {
        let (_home, mut s) = spawn("broken-log", "xterm-256color", true, extra, true);
        s.wait_for(READY);
        s.wait_shown(LOG_FAILED);
        s.send(b"\x04");
        let outside = s.outside_full_screen_after_exit();
        assert!(!outside.contains(LOG_FAILED), "{outside:?}");
    }
}

const PRESET_OFF: &str = "preset lost (line 1) is off";

#[test]
fn no_start_warning_shows_outside_the_full_screen() {
    if !usable() {
        return;
    }
    let home = Home::new("start-notes");
    let config = home.dir(CONFIG_DIR);
    std::fs::write(
        config.join("config.toml"),
        "[agents.presets.lost]\ndescription = \"Lost\"\nendpoint = \"Not A Name\"\n",
    )
    .unwrap();
    let mut s = spawn_in(&home, "xterm-256color", true, &[], true);
    s.wait_for(READY);
    s.wait_shown(PRESET_OFF);
    s.wait_shown(LOG_FAILED);
    s.send(b"\x04");
    let outside = s.outside_full_screen_after_exit();
    for warning in [PRESET_OFF, LOG_FAILED, "wisp: "] {
        assert!(!outside.contains(warning), "{warning}: {outside:?}");
    }
}

const KEY_A: &str = "sk-ant-CANARYPTYKEYA01";
const TOKEN_B: &str = "CANARYPTYTOKENB02";

fn serve(text: &str) -> (u16, Arc<Mutex<Vec<Seen>>>) {
    let server = Server::replying(text_reply("msg_1", "claude-sonnet-5-5", text, (1, 5)));
    (server.port, server.seen)
}

fn texts(body: &str) -> Vec<String> {
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| {
            m["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|b| b["text"].as_str().map(str::to_owned))
        })
        .collect()
}

#[test]
fn endpoint_switches_the_next_turn_to_the_other_server_with_the_history() {
    if !usable() {
        return;
    }
    let (port_a, seen_a) = serve("REPLY-FROM-A");
    let (port_b, seen_b) = serve("REPLY-FROM-B");
    let home = Home::new("switch");
    home.write(
        "xdg/wisp/config.toml",
        &format!(
            "default = \"a\"\n\n[endpoints.a]\nauth = \"api_key\"\nkey_env = \"WISP_KEY_A\"\nurl = \"http://127.0.0.1:{port_a}\"\n\n[endpoints.b]\nauth = \"bearer\"\nkey_env = \"WISP_TOKEN_B\"\nurl = \"http://127.0.0.1:{port_b}\"\n"
        ),
    );
    home.write(
        "xdg/wisp/.env",
        &format!("WISP_KEY_A={KEY_A}\nWISP_TOKEN_B={TOKEN_B}\n"),
    );
    let args: Vec<OsString> = ["--time", "off", "--model", "claude-sonnet-5-5"]
        .iter()
        .map(OsString::from)
        .collect();
    let envs = [("XDG_CONFIG_HOME", home.file("xdg").into_os_string())];
    let mut s = launch(&home, "xterm-256color", 24, true, &args, &envs);
    s.wait_for(READY);
    s.send(b"first question\r");
    s.wait_for(b"REPLY-FROM-A");
    s.wait_idle();
    s.send(b"/endpoint b\r");
    s.wait_for("Switched to ".as_bytes());
    s.send(b"second question\r");
    s.wait_for(b"REPLY-FROM-B");
    s.wait_idle();
    s.send(b"\x04");
    let logs_dir = home.file(".local/share/wisp/sessions");
    let screen = s.screen.clone();
    let (status, _) = s.finish();
    assert!(status.success(), "{status:?}");
    let a = seen_a.lock().unwrap();
    let b = seen_b.lock().unwrap();
    assert_eq!(a.len(), 1, "server a");
    assert_eq!(b.len(), 1, "server b");
    assert!(
        a[0].head
            .to_ascii_lowercase()
            .contains(&format!("x-api-key: {}", KEY_A.to_ascii_lowercase())),
        "{}",
        a[0].head
    );
    assert!(
        b[0].head.contains(&format!("Bearer {TOKEN_B}"))
            || b[0].head.to_ascii_lowercase().contains(&format!(
                "authorization: bearer {}",
                TOKEN_B.to_ascii_lowercase()
            )),
        "{}",
        b[0].head
    );
    assert!(!b[0].head.contains(KEY_A), "{}", b[0].head);
    let history = texts(&b[0].body);
    assert!(
        history.iter().any(|t| t.contains("first question")),
        "{history:?}"
    );
    assert!(history.iter().any(|t| t == "REPLY-FROM-A"), "{history:?}");
    assert!(
        history.iter().any(|t| t.contains("second question")),
        "{history:?}"
    );
    let shown = String::from_utf8_lossy(&screen.lock().unwrap()).into_owned();
    let mut logs = String::new();
    for entry in std::fs::read_dir(&logs_dir).into_iter().flatten().flatten() {
        logs.push_str(&std::fs::read_to_string(entry.path()).unwrap());
    }
    assert!(logs.contains("\"endpoint_name\":\"b\""), "{logs}");
    for canary in [KEY_A, TOKEN_B] {
        assert!(
            !shown.contains(canary),
            "{canary} on screen or in the goodbye"
        );
        assert!(!logs.contains(canary), "{canary} in the log");
    }
    assert!(shown.contains("Bye for now."), "{shown:?}");
}

#[test]
fn esc_on_the_start_picker_keeps_the_accounts_own_model() {
    if !usable() {
        return;
    }
    let (port, seen) = serve("REPLY-FROM-A");
    let home = Home::new("start-pick");
    home.write(
        "xdg/wisp/config.toml",
        &format!(
            "default = \"a\"\n[endpoints.a]\nauth = \"api_key\"\nkey_env = \"WISP_KEY_A\"\nurl = \"http://127.0.0.1:{port}\"\n"
        ),
    );
    home.write("xdg/wisp/.env", &format!("WISP_KEY_A={KEY_A}\n"));
    let args: Vec<OsString> = ["--time", "off", "--no-log", "--model", "opus-55"]
        .iter()
        .map(OsString::from)
        .collect();
    let envs = [("XDG_CONFIG_HOME", home.file("xdg").into_os_string())];
    let mut s = launch(&home, "xterm-256color", 24, true, &args, &envs);
    s.wait_shown("No account offers opus-55");
    s.send(b"\x1b");
    assert!(
        wait_until(Duration::from_secs(20), || !s
            .shown()
            .contains("No account offers opus-55")),
        "the start picker never closed"
    );
    s.send(b"hello\r");
    s.wait_for(b"REPLY-FROM-A");
    s.wait_idle();
    s.send(b"\x04");
    let (status, _) = s.finish();
    assert!(status.success(), "{status:?}");
    let body: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()[0].body).unwrap();
    assert!(
        body["model"]
            .as_str()
            .is_some_and(|m| m.starts_with("claude-")),
        "{body}"
    );
}

fn dead_proxy() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    format!("http://127.0.0.1:{port}")
}

struct SignIn {
    session: Pty,
    opened: PathBuf,
    home: Home,
}

fn sign_in(name: &str, words: &[&str], config_text: &str, extra: &[(&str, OsString)]) -> SignIn {
    let home = Home::new(name);
    let opened = home.file("opened");
    let browser = home.file("browser");
    std::fs::write(
        &browser,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", opened.display()),
    )
    .unwrap();
    std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = home.file("config.toml");
    if !config_text.is_empty() {
        std::fs::write(&config, config_text).unwrap();
    }
    let mut args: Vec<OsString> = vec!["--config".into(), config.into_os_string(), "login".into()];
    args.extend(words.iter().map(OsString::from));
    let proxy = dead_proxy();
    let hook = home.file("vscode-hook");
    std::fs::write(&hook, "").unwrap();
    let mut envs: Vec<(&str, OsString)> = vec![
        ("BROWSER", browser.into_os_string()),
        ("HTTPS_PROXY", proxy.clone().into()),
        ("HTTP_PROXY", proxy.clone().into()),
        ("ALL_PROXY", proxy.into()),
        ("SSH_CONNECTION", "192.0.2.1 50000 192.0.2.2 22".into()),
        ("VSCODE_IPC_HOOK_CLI", hook.into_os_string()),
    ];
    envs.extend_from_slice(extra);
    SignIn {
        session: launch(&home, "xterm-256color", 24, true, &args, &envs),
        opened,
        home,
    }
}

impl SignIn {
    fn state(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(url) = std::fs::read_to_string(&self.opened)
                && let Some((_, rest)) = url.split_once("&state=")
            {
                assert!(
                    url.starts_with("https://claude.ai/oauth/authorize?"),
                    "{url}"
                );
                return rest.split('&').next().unwrap().to_owned();
            }
            assert!(Instant::now() < deadline, "the browser was never opened");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn times_shown(&self, needle: &str) -> usize {
        let bytes = self.session.screen.lock().unwrap().clone();
        screen::shown(&bytes, 400, 80).matches(needle).count()
    }

    fn wait_times(&self, needle: &str, times: usize) {
        assert!(
            wait_until(PATIENCE, || self.times_shown(needle) >= times),
            "{needle:?} shown fewer than {times} times"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    fn codes_asked(&self) -> usize {
        let bytes = self.session.screen.lock().unwrap().clone();
        screen::shown(&bytes, 400, 80)
            .lines()
            .filter(|line| line.starts_with("Code:"))
            .count()
    }

    fn wait_asked(&self, times: usize) {
        assert!(
            wait_until(PATIENCE, || self.codes_asked() >= times),
            "the code was asked fewer than {times} times"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn login_reprompts_after_an_empty_line_or_a_doubled_paste() {
    if !usable() {
        return;
    }
    let mut login = sign_in("login-reprompt", &["claude", "--name", "claude"], "", &[]);
    let state = login.state();
    login
        .session
        .wait_shown("tried your browser \u{b7} link sent to your terminal");
    login.wait_asked(1);
    let url = std::fs::read_to_string(&login.opened).unwrap();
    let seen = String::from_utf8_lossy(&login.session.screen.lock().unwrap()).into_owned();
    assert!(
        seen.find(&url).unwrap() < seen.find("Code:").unwrap(),
        "{seen}"
    );
    let osc = format!(
        "\x1b]52;c;{}\x07",
        wisp_media::encode_base64(url.as_bytes())
    );
    assert!(seen.contains(&osc), "{seen}");
    login.session.send(b"\r");
    login
        .session
        .wait_shown("Paste the code from Claude's page first.");
    login.wait_asked(2);
    let doubled = format!("FAKECODE-not-real#{state}FAKECODE-not-real#{state}\r");
    login.session.send(doubled.as_bytes());
    login
        .session
        .wait_shown("That's the code twice. Paste again; the new paste replaces this one.");
    login.wait_asked(3);
    login.session.send(b"FAKECODE-not-real#another-sign-in\r");
    login
        .session
        .wait_shown("That code is from an earlier sign-in");
    login.wait_asked(4);
    login
        .session
        .send(format!("FAKECODE-not-real#{state}\r").as_bytes());
    login
        .session
        .wait_shown("Couldn't reach Claude. enter tries again.");
    let bytes = login.session.screen.lock().unwrap().clone();
    let seen = String::from_utf8_lossy(&bytes).into_owned();
    assert!(seen.contains("FAKE\u{2026}"), "{seen}");
    assert_eq!(
        seen.matches(&state[..20]).count(),
        1,
        "the code was echoed: {seen}"
    );
    assert!(
        !seen.contains("FAKECODE-not-real"),
        "the code was echoed: {seen}"
    );
    assert!(seen.contains("\u{1b}[?2004h") && seen.contains("\u{1b}[?2004l"));
    login.wait_asked(5);
    login.session.send(b"\x03");
    login
        .session
        .wait_shown("Sign-in cancelled; nothing was saved.");
    let (status, _) = login.session.finish();
    assert!(!status.success());
}

#[test]
fn ctrl_c_while_the_link_is_copying_ends_the_line_and_cancels() {
    if !usable() {
        return;
    }
    let tools = Home::new("login-copying-tools");
    let started = tools.file("started");
    let finished = tools.file("finished");
    let tmux = tools.file("tmux");
    std::fs::write(
        &tmux,
        format!(
            "#!/bin/sh\ntouch '{}'\nsleep 2\ntouch '{}'\n",
            started.display(),
            finished.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(tools.0.clone()).chain(std::env::split_paths(&path));
    let extra = [
        ("TMUX", OsString::from("/tmp/tmux-test/default,1,0")),
        ("PATH", std::env::join_paths(dirs).unwrap()),
    ];
    let mut login = sign_in("login-copying", &["claude", "--name", "claude"], "", &extra);
    login.state();
    login.session.wait_shown("tried your browser");
    assert!(
        wait_until(PATIENCE, || started.exists()),
        "tmux never started"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(!login.session.cooked(), "the copy's prompt left raw mode");
    login.session.send(b"\x03");
    let shown = login
        .session
        .wait_shown("Sign-in cancelled; nothing was saved.");
    assert!(
        shown.contains("tried your browser\nSign-in cancelled; nothing was saved."),
        "{shown}"
    );
    assert_eq!(login.codes_asked(), 0, "{shown}");
    let (status, _) = login.session.finish();
    assert_eq!(
        status.code(),
        Some(i32::from(wisp_tui::term::prompt::CANCEL_EXIT))
    );
    std::thread::sleep(Duration::from_millis(2500));
    assert!(!finished.exists(), "the copy's tmux outlived the sign-in");
}

#[test]
fn ctrl_c_while_waiting_for_the_browser_callback_cancels() {
    if !usable() {
        return;
    }
    let extra = [("TERM", OsString::from("dumb"))];
    let mut login = sign_in(
        "login-callback",
        &["antigravity", "--name", "antigravity"],
        "",
        &extra,
    );
    assert!(
        wait_until(PATIENCE, || login.opened.exists()),
        "the browser was never opened"
    );
    login.session.wait_shown("tried your browser");
    login.session.wait_shown("couldn't copy the link");
    std::thread::sleep(Duration::from_millis(500));
    let until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < until {
        assert!(!login.session.cooked(), "the callback wait left raw mode");
    }
    login.session.send(b"\x03");
    login
        .session
        .wait_shown("Sign-in cancelled; nothing was saved.");
    let (status, _) = login.session.finish();
    assert_eq!(
        status.code(),
        Some(i32::from(wisp_tui::term::prompt::CANCEL_EXIT)),
        "{status:?}"
    );
}

#[test]
fn a_bracketed_paste_with_a_line_break_is_one_answer_and_a_second_paste_replaces_it() {
    if !usable() {
        return;
    }
    let mut login = sign_in("login-bracketed", &["claude", "--name", "claude"], "", &[]);
    let state = login.state();
    login.wait_asked(1);
    let paste = format!("\x1b[200~FAKECODE-not-real#{state}\r\n\x1b[201~");
    login.session.send(paste.as_bytes());
    login.session.send(paste.as_bytes());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        login.codes_asked(),
        1,
        "a line break inside a paste submitted it"
    );
    login.session.wait_shown("  61 characters");
    login.session.send(b"\r");
    login
        .session
        .wait_shown("Couldn't reach Claude. enter tries again.");
    login.wait_asked(2);
    login.session.send(b"\x03");
    let (status, _) = login.session.finish();
    assert!(!status.success());
}

#[test]
fn login_without_a_word_or_terminal_says_what_to_give() {
    let home = Home::new("login-piped");
    let output = command_in(&home.0, WISP)
        .args(["--config"])
        .arg(home.file("config.toml"))
        .arg("login")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "wisp: say what to sign in to: wisp login claude, chatgpt, antigravity or key.\n"
    );
    let status = command_in(&home.0, WISP)
        .args(["--config"])
        .arg(home.file("config.toml"))
        .arg("status")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "No accounts yet. Run wisp login to sign in.\n"
    );
}

#[test]
fn login_key_asks_which_kind_of_key_unless_the_provider_is_given() {
    if !usable() {
        return;
    }
    for (provider, question, pick) in [
        (None, "Which kind of key is it?", &b"\x1b[B\r"[..]),
        (Some("openai"), "OpenAI's API (api.openai.com)", &b"1\r"[..]),
    ] {
        let home = Home::new("login-key");
        let config = home.file("config.toml");
        let mut args: Vec<OsString> = vec![
            "--config".into(),
            config.clone().into_os_string(),
            "login".into(),
            "key".into(),
        ];
        args.extend(
            provider
                .map(|word| ["--provider".into(), word.into()])
                .into_iter()
                .flatten(),
        );
        let proxy = dead_proxy();
        let envs: Vec<(&str, OsString)> = vec![
            ("HTTPS_PROXY", proxy.clone().into()),
            ("HTTP_PROXY", proxy.clone().into()),
            ("ALL_PROXY", proxy.into()),
        ];
        let mut session = launch(&home, "xterm-256color", 24, true, &args, &envs);
        session.wait_shown(question);
        session.wait_for(KEYS);
        session.send(pick);
        session.wait_for(b"Paste the API key: ");
        session.send(b"sk-proj-CANARY0123456789abcdefWXYZ\r");
        session.wait_shown("Save anyway");
        session.wait_shown("esc cancel");
        session.send(b"2\r");
        session.wait_shown("Name for this account: openai-api");
        session.send(b"\r");
        let (status, _) = session.finish();
        assert!(status.success(), "{status:?}");
        let saved = std::fs::read_to_string(&config).unwrap();
        assert!(saved.contains("[endpoints.openai-api]"), "{saved}");
        assert!(saved.contains("provider = \"openai\""), "{saved}");
    }
}

#[test]
fn the_name_step_keeps_the_suggestion_says_why_and_skips_for_a_given_name() {
    if !usable() {
        return;
    }
    let mut kept = sign_in("name-kept", &["claude"], "", &[]);
    kept.wait_times("Name for this account: claude", 1);
    kept.session.send(b"\r");
    kept.state();
    kept.session.send(b"\x03");
    let _ = kept.session.finish();

    let mut taken = sign_in(
        "name-taken",
        &["claude"],
        "[endpoints.claude]\nauth = \"api_key\"\nkey_env = \"WISP_TEST_NAME_STEP_UNSET\"\n",
        &[],
    );
    taken.wait_times("Name for this account: ", 1);
    let shown = taken.session.wait_shown("claude is taken.");
    assert!(
        shown.contains("claude is taken.\nName for this account:"),
        "{shown}"
    );
    assert!(!shown.contains("Name for this account: claude"), "{shown}");
    taken.session.send(b"claude\r");
    taken.wait_times("claude is taken.", 2);
    taken.session.send(b"Bad!\r");
    taken.wait_times(wisp_protocol::ACCOUNT_NAME_RULE.as_str(), 1);
    taken.session.send(b"Claude1\r");
    taken.state();
    taken.session.send(b"\x03");
    let _ = taken.session.finish();

    let dumb = [("TERM", OsString::from("dumb"))];
    let mut given = sign_in("name-given", &["claude", "--name", "work"], "", &dumb);
    given.state();
    given.session.wait_shown("couldn't copy the link");
    assert!(!given.session.saw(b"\x1b]52;"));
    assert_eq!(given.times_shown("Name for this account"), 0);
    given.session.send(b"\x03");
    let _ = given.session.finish();
}

#[test]
fn login_lists_take_the_arrow_keys_and_numbers_only_when_they_cannot() {
    if !usable() {
        return;
    }
    for (term, rows, key) in [
        ("xterm-256color", 4, false),
        ("dumb", 24, false),
        ("xterm-256color", 6, true),
        ("dumb", 24, true),
    ] {
        let home = Home::new("login-numbered");
        let mut args: Vec<OsString> = vec![
            "--config".into(),
            home.file("config.toml").into_os_string(),
            "login".into(),
        ];
        args.extend(key.then(|| "key".into()));
        let mut session = launch(&home, term, rows, true, &args, &[]);
        session.wait_for(if key { "Choose [1]: " } else { "Choose 1-4: " });
        assert!(!session.saw(b"esc cancel"), "{term} {rows}");
        let shown = session.shown();
        let lead = shown.contains(&format!(
            "Which kind of key is it?\n  1  {}",
            AccountKind::ClaudeApi.label()
        ));
        assert_eq!(lead, key, "{term} {rows} {shown}");
        session.send(b"\x03");
        let _ = session.finish();
    }
    let saved = "store_id = \"0123456789abcdef\"\n\n[endpoints.claude-work]\nauth = \"anthropic_subscription\"\nkey_store = \"file\"\n";
    let mut another = sign_in("login-picker", &[], saved, &[]);
    another.session.wait_for(KEYS);
    another.session.send(b"\r");
    another.session.wait_shown("Add another Claude account");
    another.session.wait_shown("esc cancel");
    another.session.send(b"\x1b[B\r");
    let shown = another.session.wait_shown("Name for this account: ");
    assert!(!shown.contains("Add another Claude account"), "{shown}");
    assert!(!shown.contains("Pro or Max subscription"), "{shown}");
    another.session.send(b"\x03");
    let _ = another.session.finish();

    let mut esc = sign_in("login-picker-esc", &[], saved, &[]);
    esc.session.wait_for(KEYS);
    esc.session.send(b"\r");
    esc.session.wait_shown("Add another Claude account");
    esc.session.wait_shown("esc cancel");
    esc.session.send(b"\x1b");
    esc.session
        .wait_shown("Sign-in cancelled; nothing was saved.");
    assert_eq!(esc.times_shown("Sign-in cancelled; nothing was saved."), 1);
    assert_eq!(esc.times_shown("esc cancel"), 0);
    assert_eq!(esc.times_shown("Add another Claude account"), 0);
    assert_eq!(esc.times_shown("Pro or Max subscription"), 0);
    let (status, _) = esc.session.finish();
    assert!(!status.success());
    assert_eq!(
        std::fs::read_to_string(esc.home.file("config.toml")).unwrap(),
        saved
    );
}

fn key_sign_in(name: &str, rename: &[u8]) -> (vt100::Parser<screen::Updates>, String) {
    let home = Home::new(name);
    let config = home.file("config.toml");
    let args: Vec<OsString> = vec![
        "--config".into(),
        config.clone().into_os_string(),
        "login".into(),
        "key".into(),
    ];
    let proxy = dead_proxy();
    let envs: Vec<(&str, OsString)> = vec![
        ("HTTPS_PROXY", proxy.clone().into()),
        ("HTTP_PROXY", proxy.clone().into()),
        ("ALL_PROXY", proxy.into()),
    ];
    let mut session = launch(&home, "xterm-256color", 24, true, &args, &envs);
    session.wait_shown("Which kind of key is it?");
    session.wait_for(KEYS);
    session.send(b"1\r");
    session.wait_for(b"Paste the API key: ");
    session.send(b"sk-ant-api03-CANARY0123456789abcdefWXYZ\r");
    session.wait_shown("Save anyway");
    session.wait_shown("esc cancel");
    session.send(b"2\r");
    session.wait_shown("Name for this account: claude-api");
    session.send(rename);
    let screen = Arc::clone(&session.screen);
    let (status, _) = session.finish();
    assert!(status.success(), "{status:?}");
    let parsed = screen::parsed(&screen.lock().unwrap(), 24, 80);
    let saved = std::fs::read_to_string(&config).unwrap();
    (parsed, saved)
}

#[test]
fn erasing_the_suggested_name_and_typing_claude1_saves_claude1() {
    if !usable() {
        return;
    }
    let (parsed, saved) = key_sign_in(
        "name-claude1",
        b"\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7fclaude1\r",
    );
    assert!(saved.contains("[endpoints.claude1]"), "{saved}");
    assert!(!saved.contains("claude-subscription"), "{saved}");
    assert!(!saved.contains("[endpoints.claude-api]"), "{saved}");
    let shown = parsed.screen().contents();
    let shown: Vec<&str> = shown.trim_end().lines().map(str::trim_end).collect();
    assert_eq!(
        shown,
        [
            "the file given with --config doesn't exist yet; it will be created.",
            "Paste the API key: sk-ant-api03-\u{2026}************\u{2026}WXYZ  39 characters",
            "Checking the key\u{2026} unreachable: could not connect",
            "Name for this account: claude1",
            &format!(
                "Signed in \u{b7} {} \u{b7} \u{25cf} claude1",
                AccountKind::ClaudeApi.label()
            ),
            "Run wisp to start a chat.",
        ]
    );
    let mask = parsed.screen().cell(1, 19).unwrap();
    assert_eq!(
        (mask.contents(), mask.fgcolor()),
        ("s", vt100::Color::Idx(173))
    );
    let (_, saved) = key_sign_in("name-capitals", b"\x15Claude1\r");
    assert!(saved.contains("[endpoints.claude1]"), "{saved}");
    let (_, saved) = key_sign_in("name-enter", b"\r");
    assert!(saved.contains("[endpoints.claude-api]"), "{saved}");
}

#[test]
fn a_provider_switch_asks_where_the_chat_goes_and_applies_now() {
    if !usable() {
        return;
    }
    let server = Server::start(|_| support::server::json("200 OK", "{\"input_tokens\": 3}"));
    let home = Home::configured("provider-switch-now");
    let base = format!("http://127.0.0.1:{}", server.port);
    home.write(support::home::CONFIG, &format!("default = \"g1\"\n[endpoints.g1]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"GEMINI_API_KEY\"\nurl = \"{base}\"\n[endpoints.other]\nauth = \"api_key\"\nkey_env = \"ANTHROPIC_API_KEY\"\nurl = \"{base}\"\n"));
    let envs = vec![
        ("ANTHROPIC_API_KEY", OsString::from("CANARY_OTHER")),
        ("ANTHROPIC_BASE_URL", OsString::from(&base)),
        ("GEMINI_API_KEY", OsString::from("CANARY_GEMINI")),
    ];
    let doctor = command_in(&home.0, WISP)
        .args(["doctor"])
        .envs(envs.iter().map(|(key, value)| (*key, value)))
        .output()
        .unwrap();
    assert!(doctor.status.success(), "{}", support::home::both(&doctor));
    let args = [
        "--no-log",
        "--time",
        "off",
        "--model",
        wisp_core::provider::gemini::default_model(),
    ]
    .map(OsString::from);
    let mut session = launch(&home, "xterm-256color", 30, true, &args, &envs);
    let before_gemini = wisp_plugins::bundled()
        .iter()
        .filter(|bundled| {
            wisp_plugin_api::manifest::Manifest::parse(
                bundled.manifest,
                wisp_plugin_api::manifest::Origin::Bundled,
            )
            .unwrap()
            .id
            .as_str()
                < "gemini"
        })
        .count();
    let plugin = |session: &mut Pty| {
        session.send(b"/config model\r");
        session.wait_shown("More settings");
        session.send(b"\x1b[F\x1b[A\r");
        session.wait_shown("agent-viz:");
        session.send(b"\x1b[H");
        session.send(&b"\x1b[B".repeat(before_gemini));
        session.wait_shown("gemini:");
    };
    session.wait_for(READY);
    plugin(&mut session);
    session.wait_shown("enter turn off");
    session.send(b"\r");
    session.wait_shown("Where should this chat go?");
    std::thread::sleep(Duration::from_millis(80));
    let shown = session.shown();
    assert!(
        !shown.contains("Switch account first") && !shown.contains("Settings not changed"),
        "{shown}"
    );
    session.send(b"\x1b");
    session.wait_screen("the provider choice to close", |shown| {
        !shown.contains("Where should this chat go?")
    });
    std::thread::sleep(Duration::from_millis(80));
    let shown = session.shown();
    assert!(
        !shown.contains("Settings not changed") && !shown.contains("Switch account first"),
        "{shown}"
    );
    plugin(&mut session);
    session.wait_shown("enter turn off");
    session.send(b"\r");
    session.wait_shown("Where should this chat go?");
    session.send(b"\r");
    session.wait_shown("applies now");
    plugin(&mut session);
    let shown = session.wait_shown("enter turn on");
    let row = shown
        .lines()
        .find(|line| line.contains("gemini") && line.contains("off") && !line.contains("accounts"))
        .unwrap();
    assert!(
        !row.contains("pending") && !row.contains("next turn"),
        "{shown}"
    );
    session.close_settings();
    session.wait_shown("applies now");
    session.send(b"/account\r");
    session.wait_shown("gemini is off");
    session.send(b"\x1b");
    session.wait_screen("the account picker to close", |shown| {
        !shown.lines().any(|line| line.trim() == "Account")
    });
    plugin(&mut session);
    session.wait_shown("enter turn on");
    session.send(b"\r");
    let shown = session.wait_shown("enter turn off");
    assert!(!shown.contains("gemini is off"), "{shown}");
    session.close_settings();
    session.send(b"\x04");
    assert!(session.finish().0.success());
}
