#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any pty or process error"
)]

use crate::support;

use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process};

use support::pty::{PATIENCE, Pty, Stdout, usable, wait_until};
use support::server::{Server, counted};
use support::{
    WISP,
    home::{CONFIG, CONFIG_DIR, CREDENTIALS, Home, both, command_in},
};

const MODEL: &str = "claude-sonnet-5-5";
const KEY: &str = "sk-ant-CANARYTYPEDKEYa1b2";
const CANCELLED: &str = "Cancelled; nothing was saved.";
const KEYS: &str = "enter choose \u{b7} esc cancel";
const ASK_KEYS: &str = "\u{2191}\u{2193} move \u{b7} esc quit";

fn serve() -> Server {
    Server::start(|_| counted())
}

fn launch(home: &Path, args: &[&str]) -> Pty {
    launch_with(home, args, &[])
}

fn launch_with(home: &Path, args: &[&str], extra: &[(&str, PathBuf)]) -> Pty {
    launch_in(home, args, extra, false)
}

fn launch_in(home: &Path, args: &[&str], extra: &[(&str, PathBuf)], full_screen: bool) -> Pty {
    let mut command = Pty::command(home);
    command
        .args(["--model", MODEL])
        .args(args)
        .envs(extra.iter().map(|(k, v)| (*k, v.as_os_str())));
    let stdout = if full_screen {
        Stdout::Tty
    } else {
        Stdout::Piped
    };
    let mut pty = Pty::start(&mut command, 24, 100, stdout);
    pty.pause = Duration::from_millis(50);
    pty
}

impl Pty {
    fn wait_raw(&self) {
        assert!(
            wait_until(PATIENCE, || !self.cooked()),
            "never raw: {:?}",
            self.screen()
        );
    }

    fn signal(&self, signal: Signal) {
        let pid = Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        kill_process(pid, signal).unwrap();
    }
}

#[test]
fn an_interactive_add_never_shows_the_key() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("interactive");
    let mut session = launch(&home.0, &["endpoint", "add"]);
    session.wait_for("Where does it connect?");
    session.wait_for(KEYS);
    session.send(b"2\r");
    session.wait_for("Base URL: ");
    session.send(format!("http://127.0.0.1:{}\r", server.port).as_bytes());
    session.wait_for("How does it take the key?");
    session.wait_for_count(KEYS, 2);
    session.send(b"2\r");
    session.wait_for("Paste the key, then press Enter: ");
    session.send(KEY.as_bytes());
    session.send(b"\r");
    session.wait_for("\u{2026}a1b2  25 characters");
    session.wait_for("Name for this account [127]: ");
    session.send(b"gw\r");
    let screen = session.screen();
    let (status, out) = session.finish();
    assert!(status.success(), "{status}\n{screen}\n{out}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert!(
        out.starts_with("Saved \"gw\". Its key and URL are in ~/.config/wisp/credentials.json, readable only by you.\n"),
        "{out}"
    );
    assert!(
        out.ends_with("Add more any time with: wisp account add\n"),
        "{out}"
    );
    let heads = server.heads();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].to_ascii_lowercase().contains(&format!(
            "authorization: bearer {}",
            KEY.to_ascii_lowercase()
        )),
        "{}",
        heads[0]
    );
    assert!(
        home.read(CONFIG)
            .contains("[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\n")
    );
    assert!(home.read(CREDENTIALS).contains(KEY));
}

#[test]
fn ctrl_c_and_ctrl_d_cancel_and_restore_the_terminal() {
    if !usable() {
        return;
    }
    for (key, at_key_prompt) in [
        (b"\x03", false),
        (b"\x04", false),
        (b"\x1b", false),
        (b"\x03", true),
        (b"\x04", true),
    ] {
        let home = Home::new("cancel");
        let mut session = launch(&home.0, &["endpoint", "add", "direct", "--no-check"]);
        session.wait_for(KEYS);
        if at_key_prompt {
            session.send(b"\r");
            session.wait_for("Paste the key");
            session.send(b"sk-ant-CANARYHALF");
        }
        assert!(!session.cooked());
        session.send(key);
        session.wait_for(CANCELLED);
        let screen = session.screen();
        let cooked = session.cooked();
        let (status, out) = session.finish();
        assert_eq!(status.code(), Some(130), "{status}\n{screen}");
        assert!(cooked, "{screen}");
        assert!(out.is_empty(), "{out}");
        assert!(!screen.contains("CANARY"), "{screen}");
        assert_eq!(screen.matches(CANCELLED).count(), 1, "{screen}");
        assert!(!home.exists(CONFIG) && !home.exists(CREDENTIALS));
    }
}

#[test]
fn a_signal_during_a_prompt_restores_the_terminal() {
    if !usable() {
        return;
    }
    for (signal, at_list) in [
        (Signal::INT, false),
        (Signal::TERM, false),
        (Signal::HUP, false),
        (Signal::TERM, true),
    ] {
        let home = Home::new("signal");
        let mut session = launch(&home.0, &["endpoint", "add", "direct", "--no-check"]);
        session.wait_for(KEYS);
        if !at_list {
            session.send(b"\r");
            session.wait_for("Paste the key");
        }
        assert!(!session.cooked());
        session.signal(signal);
        session.wait_for(CANCELLED);
        let screen = session.screen();
        let cooked = session.cooked();
        let (status, _) = session.finish();
        assert!(cooked, "{signal:?}: {screen}");
        let last_style = screen
            .rmatch_indices("\u{1b}[")
            .map(|(at, _)| &screen[at + 2..])
            .find_map(|rest| {
                let end = rest.find(|c: char| !c.is_ascii_digit() && c != ';')?;
                (rest[end..].starts_with('m')).then(|| &rest[..end])
            });
        assert_eq!(last_style, Some("0"), "{signal:?}: {screen:?}");
        assert_eq!(
            status.signal(),
            Some(signal.as_raw()),
            "{signal:?}: {status:?}"
        );
        assert!(!home.exists(CONFIG));
    }
}

#[test]
fn a_writer_waiting_at_a_prompt_holds_no_lock() {
    if !usable() {
        return;
    }
    let home = Home::new("no-lock");
    let mut session = launch(&home.0, &["endpoint", "add", "first", "--no-check"]);
    session.wait_for(KEYS);
    session.send(b"\r");
    session.wait_for("Paste the key");
    let mut second = command_in(&home.0, WISP)
        .args(["endpoint", "add", "second", "--key-stdin", "--no-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    second
        .stdin
        .take()
        .unwrap()
        .write_all(b"sk-ant-CANARYSECONDKEY01")
        .unwrap();
    let output = second.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", both(&output));
    session.send(KEY.as_bytes());
    session.send(b"\r");
    let (status, out) = session.finish();
    assert!(status.success(), "{status}\n{out}");
    let config = home.read(CONFIG);
    assert!(
        config.contains("[endpoints.first]") && config.contains("[endpoints.second]"),
        "{config}"
    );
}

#[test]
fn a_deliberate_no_exits_1_not_130() {
    if !usable() {
        return;
    }
    let home = Home::new("declined");
    let mut session = launch(
        &home.0,
        &[
            "endpoint",
            "add",
            "gw",
            "--url",
            "http://gw.example.invalid",
            "--auth",
            "bearer",
        ],
    );
    session.wait_for("so the key is sent unencrypted. Continue?");
    session.send(b"2\r");
    session.wait_for("Nothing was saved.");
    let screen = session.screen();
    let cooked = session.cooked();
    let (status, out) = session.finish();
    assert_eq!(status.code(), Some(1), "{status}\n{screen}");
    assert!(cooked);
    assert!(out.is_empty(), "{out}");
    assert!(!home.exists(CONFIG));
}

#[test]
fn remove_and_logout_ask_by_what_will_happen() {
    if !usable() {
        return;
    }
    let home = Home::new("remove-question");
    let mut add = command_in(&home.0, WISP)
        .args(["endpoint", "add", "direct", "--key-stdin", "--no-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    add.stdin.take().unwrap().write_all(KEY.as_bytes()).unwrap();
    assert!(add.wait().unwrap().success());
    std::fs::copy(home.0.join(CONFIG), home.0.join(".config/wisp/copy.toml")).unwrap();
    for (config, question) in [
        (
            CONFIG,
            "Remove \"direct\" and delete its key from credentials.json beside the file given with --config?",
        ),
        (".config/wisp/copy.toml", "Remove \"direct\"?"),
    ] {
        let mut session = launch(
            &home.0,
            &["--config", config, "endpoint", "remove", "direct"],
        );
        session.wait_for(question);
        session.send(b"\r");
        let (status, out) = session.finish();
        assert!(status.success());
        assert_eq!(out, "Nothing was changed.\n");
    }
    let mut session = launch(&home.0, &["logout", "direct"]);
    session.wait_for("Sign out of direct?");
    session.send(b"\r");
    session.wait_for("Nothing was signed out.");
    let (status, out) = session.finish();
    assert_eq!(status.code(), Some(130), "{status}");
    assert_eq!(out, "");
    assert!(home.read(CREDENTIALS).contains(KEY));
    let session = launch(&home.0, &["endpoint", "remove", "typo"]);
    session.wait_for("there is no account \"typo\"");
    let screen = session.screen();
    let (status, _) = session.finish();
    assert_eq!(status.code(), Some(1));
    assert!(!screen.contains(KEYS), "{screen}");
}

fn dead_keychain(home: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("WISP_KEYCHAIN", PathBuf::from("on")),
        (
            "DBUS_SESSION_BUS_ADDRESS",
            PathBuf::from(format!("unix:path={}", home.join("no-bus").display())),
        ),
        ("XDG_RUNTIME_DIR", home.join("no-runtime")),
    ]
}

#[test]
fn a_keychain_that_fails_a_write_under_auto_asks_before_using_the_file() {
    if !usable() {
        return;
    }
    let home = Home::new("keychain-question");
    let mut session = launch_with(
        &home.0,
        &["endpoint", "add", "direct", "--no-check"],
        &dead_keychain(&home.0),
    );
    session.wait_for("Where does it connect?");
    session.wait_for(KEYS);
    session.send(b"1\r");
    session.wait_for("Paste the key, then press Enter: ");
    session.send(KEY.as_bytes());
    session.send(b"\r");
    session.wait_for(
        "The keychain isn't running. Keep this key in ~/.config/wisp/credentials.json instead?",
    );
    session.send(b"1\r");
    let screen = session.screen();
    let (status, out) = session.finish();
    assert!(status.success(), "{status}\n{screen}\n{out}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert!(
        out.starts_with(
            "Saved \"direct\". Its key is in ~/.config/wisp/credentials.json, readable only by you.\n"
        ),
        "{out}"
    );
    assert!(home.read(CONFIG).contains("key_store = \"file\""));
    assert!(home.read(CREDENTIALS).contains(KEY));

    let other = Home::new("keychain-question-no");
    let mut session = launch_with(
        &other.0,
        &["endpoint", "add", "direct", "--no-check"],
        &dead_keychain(&other.0),
    );
    session.wait_for(KEYS);
    session.send(b"1\r");
    session.wait_for("Paste the key, then press Enter: ");
    session.send(KEY.as_bytes());
    session.send(b"\r");
    session.wait_for("Keep this key in ~/.config/wisp/credentials.json instead?");
    session.send(b"\r");
    let (status, _) = session.finish();
    assert_eq!(status.code(), Some(1));
    assert!(!other.exists(CONFIG));
    assert!(!other.exists(CREDENTIALS));
}

const SESSION: &str = "01a167d6-0000-7000-8000-000000000000";
const SESSION_LOG: &str = ".local/share/wisp/sessions/01a167d6-0000-7000-8000-000000000000.jsonl";

fn gone_subscription(name: &str, server: &Server) -> Home {
    let home = Home::new(name);
    home.dir(CONFIG_DIR);
    std::fs::write(
        home.0.join(CONFIG),
        format!(
            "[endpoints.api]\nauth = \"api_key\"\nkey_env = \"API_TEST_KEY\"\nurl = \"http://127.0.0.1:{}\"\n",
            server.port
        ),
    )
    .unwrap();
    let start = serde_json::json!({"schema_version":wisp_protocol::log::LOG_SCHEMA_VERSION,"seq":0,"at":"2026-10-25T10:00:00Z","type":"session_started","session_id":SESSION,"wisp_version":"0.1.0","model":MODEL,"effort":"medium","cache_ttl":"one_hour","endpoint":"https://api.anthropic.com","credential_source":{"mode":{"billing":"anthropic_plan","source":"keyring"}},"endpoint_name":"claude-subscription","cwd":home.0.to_str().unwrap()});
    std::fs::create_dir_all(home.0.join(".local/share/wisp/sessions")).unwrap();
    std::fs::write(home.0.join(SESSION_LOG), format!("{start}\n")).unwrap();
    home
}

fn resume_gone(home: &Home) -> Pty {
    let session = launch_in(
        &home.0,
        &["--instructions", "none", "--resume", "01a167d6"],
        &[("API_TEST_KEY", PathBuf::from("synthetic-api-key"))],
        true,
    );
    session.wait_for("It ran on");
    session.wait_for("This chat moves from your subscription to the API.");
    session.wait_for(ASK_KEYS);
    session.wait_raw();
    session
}

#[test]
fn a_gone_subscription_asks_before_the_api_and_enter_alone_never_picks() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = gone_subscription("resume-pick", &server);
    let mut session = resume_gone(&home);
    assert!(
        !session.screen().contains('\u{203a}'),
        "{}",
        session.screen()
    );
    session.send(b"\r");
    session.send(b"1");
    session.wait_for("\u{203a}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!session.screen().contains("Continuing on"));
    assert!(server.seen.lock().unwrap().is_empty());
    session.send(b"\r");
    session.wait_for("Continuing on");
    session.send(b"hello\r");
    assert!(
        wait_until(PATIENCE, || !server.seen.lock().unwrap().is_empty()),
        "{}",
        session.screen()
    );
    let first = server.seen.lock().unwrap()[0].head.to_ascii_lowercase();
    assert!(first.contains("x-api-key: synthetic-api-key"), "{first}");
    session.signal(Signal::TERM);
    session.finish();
}

#[test]
fn a_start_account_that_cant_be_used_starts_elsewhere_and_moves_new_chats_only_when_gone() {
    if !usable() {
        return;
    }
    let api = "[endpoints.api]\nauth = \"api_key\"\nkey_env = \"API_TEST_KEY\"\n";
    let work = "[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n";
    for (start, line, moved) in [
        (
            "env",
            "'s key is missing from ~/.config/wisp/.env, so new chats now start on",
            true,
        ),
        ("work", ": WORK_KEY not set.", false),
    ] {
        let home = Home::new(&format!("start-{start}"));
        home.dir(CONFIG_DIR);
        let config = format!("default = \"{start}\"\n{api}{work}");
        std::fs::write(home.0.join(CONFIG), &config).unwrap();
        let session = launch_in(
            &home.0,
            &["--instructions", "none"],
            &[("API_TEST_KEY", PathBuf::from("synthetic-api-key"))],
            true,
        );
        session.wait_for("Started on");
        session.wait_for(line);
        session.signal(Signal::TERM);
        session.finish();
        let now = home.read(CONFIG);
        if moved {
            assert_eq!(now, config.replace("\"env\"", "\"api\""));
        } else {
            assert_eq!(now, config);
        }
    }
}

#[test]
fn esc_or_ctrl_c_on_the_card_resumes_nothing() {
    if !usable() {
        return;
    }
    for key in [b"\x1b", b"\x03"] {
        let server = serve();
        let home = gone_subscription("resume-quit", &server);
        let log = home.read(SESSION_LOG);
        let config = home.read(CONFIG);
        let mut session = resume_gone(&home);
        session.send(key);
        let (status, out) = session.finish();
        assert!(status.success(), "{status}\n{out}");
        assert!(out.is_empty(), "{out}");
        assert!(server.seen.lock().unwrap().is_empty());
        assert_eq!(home.read(SESSION_LOG), log);
        assert_eq!(home.read(CONFIG), config);
    }
}
