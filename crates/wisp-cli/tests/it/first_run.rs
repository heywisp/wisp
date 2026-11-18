#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any pty or process error"
)]

use crate::support;

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use support::pty::{Pty, READY, Stdout, usable, wait_until};
use support::server::{Server, counted, text_reply};
use support::{
    WISP,
    home::{CONFIG, CREDENTIALS, Home, KEYS_FILE, both, command_in, err},
};

const MODEL: &str = "claude-sonnet-5-5";
const KEYS: &str = "enter choose \u{b7} esc cancel";
const KEY: &str = "sk-ant-CANARYFIRSTRUNKEYa1b2";
const WELCOME: &str = "Welcome to Wisp. No model is set up yet, so let's add one.";
const CANCELLED: &str = "Nothing was set up.";
const HINT: &str = "Or store a key: run \"wisp setup\" in a terminal, or printenv VAR | wisp account add NAME --key-stdin";
const CARD_TITLE: &str = "Let's connect a model.";

type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str);

fn serve(text: &str) -> Server {
    let reply = text_reply("msg_1", MODEL, text, (1, 5));
    Server::start(move |request| {
        if request.head.contains("count_tokens") {
            counted()
        } else {
            reply.clone()
        }
    })
}

impl Home {
    fn first_run_command(&self, args: &[&str]) -> Command {
        let mut command = command_in(&self.0, WISP);
        command
            .env_remove("LANG")
            .args(["--model", MODEL, "--no-log"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

pub(super) fn key_card(session: &mut Pty) {
    session.wait_shown("Sign in to start");
    session.send(b"4");
    session.wait_shown(CARD_TITLE);
}

fn launch(home: &Path, args: &[&str], envs: &[(&str, &str)]) -> Pty {
    let mut command = Pty::command(home);
    command
        .args(["--model", MODEL, "--no-log", "--time", "off"])
        .args(args)
        .envs(envs.iter().copied());
    let mut pty = Pty::start(&mut command, 24, 100, Stdout::Tty);
    pty.pause = Duration::from_millis(50);
    pty
}

impl Pty {
    fn finish_screen(self) -> (ExitStatus, String) {
        let screen = Arc::clone(&self.screen);
        let (status, _) = self.finish();
        (
            status,
            String::from_utf8_lossy(&screen.lock().unwrap()).into_owned(),
        )
    }
}

pub(super) fn fill_gateway(session: &mut Pty, port: u16, name: &str) {
    session.send(b"\r");
    session.send(b"\x1b[B");
    session.wait_shown("URL        https://");
    session.send(b"\r");
    session.send(format!("http://127.0.0.1:{port}").as_bytes());
    session.send(b"\r");
    session.wait_shown("Sends as");
    session.send(b"\x1b[B");
    session.send(b"\r");
    session.send(KEY.as_bytes());
    session.wait_shown(&"*".repeat(KEY.chars().count()));
    session.send(b"\r");
    session.send(b"\x15");
    session.send(name.as_bytes());
    session.send(b"\r");
}

#[test]
fn a_fresh_start_sets_up_in_the_full_screen_card_then_opens_the_session_on_it() {
    if !usable() {
        return;
    }
    let server = serve("REPLY-AFTER-SETUP");
    let home = Home::new("fresh");
    let mut session = launch(&home.0, &[], &[]);
    key_card(&mut session);
    session.wait_shown("Provider   ● Anthropic");
    assert!(!session.screen().contains(WELCOME));
    fill_gateway(&mut session, server.port, "gw");
    session.wait_shown(READY);
    session.send(b"hello\r");
    session.wait_shown("REPLY-AFTER-SETUP");
    session.wait_idle();
    session.send(b"\x04");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    let heads: Vec<String> = server
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.head.clone())
        .collect();
    assert_eq!(heads.len(), 2, "{heads:?}");
    assert!(heads[0].contains("count_tokens"), "{}", heads[0]);
    assert!(heads[1].starts_with("POST /v1/messages "), "{}", heads[1]);
    let body = server.seen.lock().unwrap()[1].body.clone();
    let line = "Terminal: replies are 96 columns wide (wider code blocks wrap), 24 rows";
    assert_eq!(body.matches(line).count(), 1, "{body}");
    for head in &heads {
        assert!(
            head.to_ascii_lowercase().contains(&format!(
                "authorization: bearer {}",
                KEY.to_ascii_lowercase()
            )),
            "{head}"
        );
    }
    let config = home.read(CONFIG);
    assert!(config.contains("default = \"gw\""), "{config}");
    assert!(config.contains("[endpoints.gw]"), "{config}");
    assert!(!config.contains("CANARY"), "{config}");
    assert!(home.read(CREDENTIALS).contains(KEY));
}
#[test]
fn ctrl_c_and_ctrl_d_cancel_setup_and_restore_the_terminal() {
    if !usable() {
        return;
    }
    for (key, typed_key) in [
        (b"\x03", false),
        (b"\x04", false),
        (b"\x03", true),
        (b"\x04", true),
    ] {
        let home = Home::new("cancel");
        let mut session = launch(&home.0, &[], &[]);
        key_card(&mut session);
        if typed_key {
            session.send(b"\r");
            session.send(b"\r");
            let partial = "sk-ant-CANARYHALF";
            session.send(partial.as_bytes());
            session.wait_shown(&"*".repeat(partial.chars().count()));
        }
        assert!(!session.cooked());
        session.send(key);
        session.wait_for(CANCELLED);
        let cooked = session.cooked();
        let (status, screen) = session.finish_screen();
        assert_eq!(status.code(), Some(130), "{status}\n{screen}");
        assert!(cooked, "{screen}");
        assert!(!screen.contains("CANARY"), "{screen}");
        assert!(!screen.contains(READY), "{screen}");
        assert!(!home.exists(CONFIG) && !home.exists(CREDENTIALS));
    }
}

#[test]
fn a_keychain_that_is_down_asks_once_in_the_card_and_keeps_the_key_in_the_file() {
    if !usable() {
        return;
    }
    let server = serve("REPLY-AFTER-FILE");
    let home = Home::new("card-keychain-down");
    let dead = dead_keychain(&home.0);
    let envs: Vec<(&str, &str)> = dead.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut session = launch(&home.0, &[], &envs);
    key_card(&mut session);
    fill_gateway(&mut session, server.port, "gw");
    session.wait_shown("Keep this key in ~/.config/wisp/credentials.json");
    session.wait_shown("instead?");
    session.send(b"1");
    session.wait_shown(READY);
    session.send(b"hello\r");
    session.wait_shown("REPLY-AFTER-FILE");
    session.wait_idle();
    session.send(b"\x04");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert_eq!(screen.matches("Keep this key in").count(), 1, "{screen}");
    let config = home.read(CONFIG);
    assert!(config.contains("[endpoints.gw]"), "{config}");
    assert!(config.contains("key_store = \"file\""), "{config}");
    assert!(home.read(CREDENTIALS).contains(KEY));
    let heads = server.seen.lock().unwrap().len();
    assert_eq!(heads, 2, "the resend skips the check");
}

fn held_lock(home: &Home) -> File {
    let path = home.0.join(".config/wisp/.wisp.lock");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = File::create(path).unwrap();
    file.lock().unwrap();
    file
}

fn opened_elsewhere(path: &Path) -> bool {
    let own = std::process::id().to_string();
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return false;
    };
    processes.flatten().any(|process| {
        let name = process.file_name();
        let pid = name.to_string_lossy();
        if pid == own || !pid.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        std::fs::read_dir(process.path().join("fd")).is_ok_and(|fds| {
            fds.flatten()
                .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|link| link == path))
        })
    })
}

fn wait_save_on_lock(home: &Home) {
    let path = std::fs::canonicalize(home.0.join(".config/wisp/.wisp.lock")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if opened_elsewhere(&path) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the save never reached the lock");
}

#[test]
fn quitting_while_a_save_waits_on_the_lock_finishes_the_save_and_exits_saved() {
    if !usable() {
        return;
    }
    let server = serve("REPLY-NEVER");
    let home = Home::new("card-lock");
    let lock = held_lock(&home);
    let mut session = launch(&home.0, &[], &[]);
    key_card(&mut session);
    fill_gateway(&mut session, server.port, "gw");
    session.wait_shown("saving the account");
    wait_save_on_lock(&home);
    session.send(b"\x1b");
    session.wait_shown("Wisp quits when the save ends.");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!home.read(CONFIG).contains("[endpoints.gw]"));
    drop(lock);
    session.wait_for("Saved \"gw\". wisp starts on it next time.");
    let (status, screen) = session.finish_screen();
    assert_eq!(status.code(), Some(0), "{status}\n{screen}");
    assert!(!screen.contains(READY), "{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert!(home.read(CONFIG).contains("[endpoints.gw]"));
    assert_eq!(server.seen.lock().unwrap().len(), 1);
}

#[test]
fn a_second_quit_while_the_lock_is_held_exits_at_once() {
    if !usable() {
        return;
    }
    let server = serve("REPLY-NEVER");
    let home = Home::new("card-lock-twice");
    let lock = held_lock(&home);
    let mut session = launch(&home.0, &[], &[]);
    key_card(&mut session);
    fill_gateway(&mut session, server.port, "gw");
    session.wait_shown("saving the account");
    wait_save_on_lock(&home);
    session.send(b"\x03");
    session.wait_shown("Wisp quits when the save ends.");
    let quit = Instant::now();
    session.send(b"\x03");
    session.wait_for("wisp: an account save was still running; it may not be saved.");
    let cooked = session.cooked();
    let (status, screen) = session.finish_screen();
    assert!(
        quit.elapsed() < Duration::from_secs(5),
        "{:?}",
        quit.elapsed()
    );
    assert_eq!(status.code(), Some(130), "{status}\n{screen}");
    assert!(cooked, "{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    drop(lock);
}

fn configured(home: &Home, server: &Server) -> Vec<(String, String)> {
    home.write(
        CONFIG,
        "default = \"first\"\n[endpoints.first]\nauth = \"bearer\"\nkey_env = \"FIRST_KEY\"\nurl_env = \"FIRST_URL\"\n",
    );
    vec![
        ("FIRST_KEY".to_owned(), "first-key-not-a-canary".to_owned()),
        (
            "FIRST_URL".to_owned(),
            format!("http://127.0.0.1:{}", server.port),
        ),
    ]
}

#[test]
fn an_endpoint_added_mid_session_can_be_switched_to_at_once() {
    if !usable() {
        return;
    }
    let first = serve("REPLY-FROM-FIRST");
    let second = serve("REPLY-FROM-SECOND");
    let home = Home::new("card-add");
    let vars = configured(&home, &first);
    let envs: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut session = launch(&home.0, &[], &envs);
    session.wait_shown(READY);
    session.send(b"/endpoint add\r");
    session.wait_shown("Add an API key");
    fill_gateway(&mut session, second.port, "second");
    session.wait_shown("Saved \"second\" in ~/.config/wisp/credentials.json.");
    session.send(b"/endpoint second\r");
    session.wait_shown("Switched to \u{25cf} second");
    session.send(b"hello\r");
    session.wait_shown("REPLY-FROM-SECOND");
    session.wait_idle();
    session.send(b"\x04");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert!(first.seen.lock().unwrap().is_empty());
    let heads: Vec<String> = second
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.head.clone())
        .collect();
    assert_eq!(heads.len(), 2, "{heads:?}");
    assert!(heads[1].starts_with("POST /v1/messages "), "{}", heads[1]);
    assert!(
        heads[1].to_ascii_lowercase().contains(&format!(
            "authorization: bearer {}",
            KEY.to_ascii_lowercase()
        )),
        "{}",
        heads[1]
    );
    let config = home.read(CONFIG);
    assert!(config.contains("default = \"first\""), "{config}");
    assert!(config.contains("[endpoints.second]"), "{config}");
}
#[test]
fn a_key_in_the_environment_starts_as_before_without_setup() {
    if !usable() {
        return;
    }
    let home = Home::new("exported");
    let mut session = launch(&home.0, &[], &[("ANTHROPIC_API_KEY", "test-key-not-used")]);
    session.wait_for(READY);
    let lists = home.0.join(".local/share/wisp/models");
    let json = |file: &std::fs::DirEntry| file.path().extension().is_some_and(|ext| ext == "json");
    let attempted = || {
        let files = std::fs::read_dir(&lists);
        files.is_ok_and(|files| files.flatten().filter(json).count() == 1)
    };
    assert!(wait_until(Duration::from_secs(10), attempted));
    session.send(b"\x04");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(!screen.contains(WELCOME), "{screen}");
    assert!(!home.exists(CONFIG));
}

#[test]
fn scripted_flags_on_a_terminal_never_open_setup() {
    if !usable() {
        return;
    }
    let home = Home::new("scripted");
    home.write("scratch/config.toml", "");
    home.write("scratch/empty.env", "");
    let cases: [Case<'_>; 4] = [
        (&["--endpoint", "work"], &[], "work"),
        (&[], &[("WISP_ENDPOINT", "work")], "work"),
        (&["--config", "scratch/config.toml"], &[], HINT),
        (&["--env-file", "scratch/empty.env"], &[], HINT),
    ];
    for (args, envs, expected) in cases {
        let session = launch(&home.0, args, envs);
        session.wait_for(expected);
        let (status, screen) = session.finish_screen();
        assert_eq!(status.code(), Some(1), "{status}\n{args:?}: {screen}");
        assert!(!screen.contains(WELCOME), "{args:?}: {screen}");
        assert!(!screen.contains("Choose [1]"), "{args:?}: {screen}");
        assert!(!screen.contains(CARD_TITLE), "{args:?}: {screen}");
        assert!(!screen.contains("isn't used by"), "{args:?}: {screen}");
    }
    assert!(!home.exists(CONFIG));
}

#[test]
fn without_a_terminal_nothing_found_keeps_todays_message_and_adds_the_hint() {
    let home = Home::new("no-terminal");
    let output = home
        .first_run_command(&["--output", "jsonl"])
        .output()
        .unwrap();
    let text = err(&output);
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert_eq!(
        text,
        format!(
            "wisp: no Anthropic credentials: set ANTHROPIC_API_KEY (an API key) or ANTHROPIC_AUTH_TOKEN (a bearer token for a gateway) in the environment\n{HINT}\n"
        )
    );
    assert!(output.stdout.is_empty());
    assert!(!home.exists(CONFIG));
}

#[test]
fn an_unavailable_default_keeps_its_reason_and_names_the_list() {
    let home = Home::new("unavailable-default");
    home.write(CONFIG, "store_id = \"0123456789abcdef\"\n");
    home.antigravity(Some("CANARY_PROJECT"));
    let bearer = |var: &str| {
        format!("auth = \"bearer\"\nkey_env = \"{var}\"\nurl = \"https://gw.example.com\"\n")
    };
    let subscription = "auth = \"anthropic_subscription\"\nkey_store = \"file\"\n";
    let unavailable = "wisp: the default account \"work\" is unavailable: ";
    let listed = "(wisp account list shows your accounts)";
    let cases = [
        (
            format!(
                "default = \"work\"\n\n[endpoints.work]\n{}",
                bearer("WORK_TOKEN")
            ),
            unavailable,
            listed.to_owned(),
        ),
        (
            format!("default = \"work\"\n\n[endpoints.work]\n{subscription}"),
            unavailable,
            format!("{listed}; sign in outside chat with wisp login, then start Wisp again"),
        ),
        (
            format!(
                "[endpoints.work]\n{subscription}\n[endpoints.a]\n{}\n[endpoints.b]\n{}",
                bearer("SET_TOKEN"),
                bearer("SET_TOKEN")
            ),
            "wisp: no account chosen: use --account (available: ",
            "a, b)".to_owned(),
        ),
        (
            "default = \"antigravity\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\n[plugins.gemini]\nenabled = false\n".to_owned(),
            "wisp: account \"antigravity\" is unavailable: ",
            "gemini is off; turn it on with wisp plugin enable gemini".to_owned(),
        ),
        (
            "[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\n[plugins.gemini]\nenabled = false\n".to_owned(),
            "wisp: no account is available: ",
            "antigravity: gemini is off".to_owned(),
        ),
    ];
    for (accounts, start, end) in cases {
        home.write(
            CONFIG,
            &format!("store_id = \"0123456789abcdef\"\n{accounts}"),
        );
        let output = home
            .first_run_command(&["--output", "jsonl"])
            .env("SET_TOKEN", "token")
            .output()
            .unwrap();
        let text = err(&output);
        assert_eq!(output.status.code(), Some(1), "{}", both(&output));
        assert!(text.starts_with(start), "{text}");
        assert!(text.trim_end().ends_with(&end), "{text}");
        assert!(!text.contains(HINT), "{text}");
    }
}

#[test]
fn wisp_setup_needs_a_terminal() {
    let home = Home::new("setup-no-terminal");
    let output = home.first_run_command(&["setup"]).output().unwrap();
    let text = err(&output);
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert_eq!(
        text,
        "wisp: wisp setup asks its questions on a terminal; without one, store a key with: printenv VAR | wisp account add NAME --key-stdin\n"
    );
    assert!(!home.exists(CONFIG));
}

#[test]
fn wisp_setup_lists_what_exists_and_asks_before_adding() {
    if !usable() {
        return;
    }
    let home = Home::new("setup-existing");
    let mut add =
        home.first_run_command(&["endpoint", "add", "direct", "--key-stdin", "--no-check"]);
    let mut child = add.stdin(Stdio::piped()).spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(KEY.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    let before = home.read(CONFIG);
    let mut session = launch(&home.0, &["setup"], &[]);
    session.wait_for("Accounts");
    session.wait_for("direct");
    session.wait_for("Add another?");
    session.send(b"\r");
    session.wait_for("Nothing was changed.");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert!(!screen.contains(WELCOME), "{screen}");
    assert!(!screen.contains("These endpoints are set up"), "{screen}");
    assert_eq!(home.read(CONFIG), before);
}

#[test]
fn wisp_setup_on_a_fresh_machine_saves_and_exits() {
    if !usable() {
        return;
    }
    let server = serve("UNUSED");
    let home = Home::new("setup-fresh");
    let mut session = launch(&home.0, &["setup"], &[]);
    session.wait_for(WELCOME);
    session.wait_for(KEYS);
    session.send(b"2\r");
    session.wait_for("Base URL: ");
    session.send(format!("http://127.0.0.1:{}\r", server.port).as_bytes());
    session.wait_for_count(KEYS, 2);
    session.send(b"\r");
    session.wait_for("Paste the key");
    session.send(KEY.as_bytes());
    session.send(b"\r");
    session.wait_for("Name for this account [127]: ");
    session.send(b"\r");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(
        screen.contains("Saved \"127\" as your default account."),
        "{screen}"
    );
    assert!(!screen.contains(READY), "{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    let heads = server.seen.lock().unwrap();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0]
            .head
            .to_ascii_lowercase()
            .contains(&format!("x-api-key: {}", KEY.to_ascii_lowercase())),
        "{}",
        heads[0].head
    );
    assert!(home.read(CONFIG).contains("default = \"127\""));
}

fn answer_gateway_setup(session: &mut Pty, port: u16) {
    session.wait_for(WELCOME);
    session.wait_for(KEYS);
    session.send(b"2\r");
    session.wait_for("Base URL: ");
    session.send(format!("http://127.0.0.1:{port}\r").as_bytes());
    session.wait_for_count(KEYS, 2);
    session.send(b"\r");
    session.wait_for("Paste the key");
    session.send(KEY.as_bytes());
    session.send(b"\r");
    session.wait_for("Name for this account [127]: ");
    session.send(b"\r");
}

#[test]
fn wisp_setup_creates_a_given_config_that_does_not_exist_yet() {
    if !usable() {
        return;
    }
    let server = serve("UNUSED");
    let home = Home::new("setup-new-config");
    home.write("scratch/keep", "");
    let mut session = launch(&home.0, &["--config", "scratch/new.toml", "setup"], &[]);
    answer_gateway_setup(&mut session, server.port);
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(!screen.contains("doesn't exist\n"), "{screen}");
    assert!(
        screen.contains("Saved \"127\" as your default account."),
        "{screen}"
    );
    let config = home.read("scratch/new.toml");
    assert!(config.contains("default = \"127\""), "{config}");
    assert!(home.read("scratch/credentials.json").contains(KEY));
    assert!(!home.exists(CONFIG));
}

#[test]
fn wisp_setup_keeps_a_default_you_set() {
    if !usable() {
        return;
    }
    let server = serve("UNUSED");
    let home = Home::new("setup-keeps-default");
    home.write(CONFIG, "default = \"env\"\n");
    let mut session = launch(&home.0, &["setup"], &[]);
    answer_gateway_setup(&mut session, server.port);
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(screen.contains("Saved \"127\". "), "{screen}");
    assert!(!screen.contains("as your default endpoint"), "{screen}");
    assert!(
        screen.contains("Your default account is unchanged. To start on \"127\" instead: wisp account default 127"),
        "{screen}"
    );
    let config = home.read(CONFIG);
    assert!(config.contains("default = \"env\""), "{config}");
    assert!(config.contains("[endpoints.127]"), "{config}");
}

#[test]
fn wisp_setup_with_an_env_file_asks_without_the_ignoring_note() {
    if !usable() {
        return;
    }
    let server = serve("UNUSED");
    let home = Home::new("setup-env-file");
    home.write("scratch/empty.env", "");
    let mut session = launch(&home.0, &["--env-file", "scratch/empty.env", "setup"], &[]);
    answer_gateway_setup(&mut session, server.port);
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(!screen.contains("isn't used by"), "{screen}");
    assert!(
        screen.contains("Saved \"127\" as your default account."),
        "{screen}"
    );
    assert!(
        !screen.contains("Your default account is unchanged"),
        "{screen}"
    );
}

fn dead_keychain(home: &Path) -> [(&'static str, String); 3] {
    [
        ("WISP_KEYCHAIN", "on".to_owned()),
        (
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", home.join("no-bus").display()),
        ),
        (
            "XDG_RUNTIME_DIR",
            home.join("no-runtime").display().to_string(),
        ),
    ]
}

#[test]
fn on_a_keychain_machine_setup_offers_to_store_the_keys_it_found() {
    if !usable() {
        return;
    }
    let server = serve("REPLY-AFTER-IMPORT");
    let home = Home::new("offer");
    let url = format!("http://127.0.0.1:{}", server.port);
    let dead = dead_keychain(&home.0);
    let mut envs: Vec<(&str, &str)> = dead.iter().map(|(k, v)| (*k, v.as_str())).collect();
    envs.push(("ANTHROPIC_API_KEY", KEY));
    envs.push(("ANTHROPIC_BASE_URL", &url));
    let mut session = launch(&home.0, &[], &envs);
    session.wait_for("Welcome to Wisp. No account is set up yet.");
    session.wait_for("This machine has a keychain, so Wisp keeps keys there and doesn't read them");
    session.wait_for("  ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL in your environment");
    session.wait_for("Store these in the keychain? Each is checked first.");
    session.send(b"\r");
    session.wait_for("Name for ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL [127]: ");
    session.send(b"gw\r");
    session.wait_for("Import ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL in the environment?");
    session.send(b"\r");
    session.wait_for(
        "The keychain isn't running. Keep these keys in ~/.config/wisp/credentials.json instead?",
    );
    session.send(b"1\r");
    session.wait_for(READY);
    session.send(b"hello\r");
    session.wait_for("REPLY-AFTER-IMPORT");
    session.wait_idle();
    session.send(b"\x04");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    let heads: Vec<String> = server
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.head.clone())
        .collect();
    assert_eq!(heads.len(), 2, "{heads:?}");
    assert!(heads[0].contains("count_tokens"), "{}", heads[0]);
    assert!(heads[1].starts_with("POST /v1/messages "), "{}", heads[1]);
    let config = home.read(CONFIG);
    assert!(config.contains("[endpoints.gw]"), "{config}");
    assert!(config.contains("key_store = \"file\""), "{config}");
    assert!(!config.contains("CANARY"), "{config}");
    assert!(home.read(CREDENTIALS).contains(KEY));
}

#[test]
fn declining_the_offer_opens_the_card_which_names_the_keys_found() {
    if !usable() {
        return;
    }
    let home = Home::new("decline");
    let dead = dead_keychain(&home.0);
    let mut envs: Vec<(&str, &str)> = dead.iter().map(|(k, v)| (*k, v.as_str())).collect();
    envs.push(("ANTHROPIC_API_KEY", KEY));
    let mut session = launch(&home.0, &[], &envs);
    session.wait_for("  ANTHROPIC_API_KEY in your environment");
    session.wait_for("Store these in the keychain? Each is checked first.");
    session.send(b"2\r");
    key_card(&mut session);
    session.wait_shown("ANTHROPIC_API_KEY");
    session.wait_shown("wisp account import");
    session.send(b"\x03");
    let (status, screen) = session.finish_screen();
    assert_eq!(status.code(), Some(130), "{status}\n{screen}");
    assert!(screen.contains(CANCELLED), "{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert!(!home.exists(CONFIG));

    let home = Home::new("decline-unread");
    home.write(CONFIG, "default = \"env\"\n");
    home.write(KEYS_FILE, &format!("ANTHROPIC_API_KEY={KEY}\n"));
    let dead = dead_keychain(&home.0);
    let envs: Vec<(&str, &str)> = dead.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut session = launch(&home.0, &[], &envs);
    session.wait_for("  ANTHROPIC_API_KEY in ~/.config/wisp/.env");
    session.wait_for("Store these in the keychain? Each is checked first.");
    session.send(b"2\r");
    session.wait_shown("Sign in to start");
    session.wait_shown("ANTHROPIC_* variables aren't read on a machine with a keychain");
    session.send(b"\x03");
    let (status, screen) = session.finish_screen();
    assert_eq!(status.code(), Some(130), "{status}\n{screen}");
    assert!(!screen.contains("No account is set up yet"), "{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert_eq!(home.read(CONFIG), "default = \"env\"\n");
}
#[test]
fn a_key_both_exported_and_in_the_env_file_is_stored_once_and_asked_about_once() {
    if !usable() {
        return;
    }
    let server = serve("REPLY-AFTER-BOTH");
    let home = Home::new("both");
    let url = format!("http://127.0.0.1:{}", server.port);
    home.write(
        KEYS_FILE,
        &format!("ANTHROPIC_API_KEY={KEY}\nANTHROPIC_BASE_URL={url}\n"),
    );
    let dead = dead_keychain(&home.0);
    let mut envs: Vec<(&str, &str)> = dead.iter().map(|(k, v)| (*k, v.as_str())).collect();
    envs.push(("ANTHROPIC_API_KEY", KEY));
    envs.push(("ANTHROPIC_BASE_URL", &url));
    let mut session = launch(&home.0, &[], &envs);
    session.wait_for("Store these in the keychain? Each is checked first.");
    session.send(b"\r");
    session.wait_for("Name for ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL [127]: ");
    session.send(b"gw\r");
    session.wait_for("Import ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL in the environment?");
    session.send(b"\r");
    session.wait_for("Keep these keys in ~/.config/wisp/credentials.json instead?");
    session.send(b"1\r");
    session
        .wait_for("Nothing more to import from ~/.config/wisp/.env: its keys were stored above.");
    session.wait_for(READY);
    session.send(b"\x04");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert_eq!(
        screen.matches("credentials.json instead?").count(),
        2,
        "{screen}"
    );
    assert!(!screen.contains("Nothing to import"), "{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
    assert_eq!(home.read(CREDENTIALS).matches(KEY).count(), 1);
}

const SECOND_KEY: &str = "sk-ant-CANARYSECONDKEYc3d4";

fn two_keys(name: &str, server: &Server) -> (Home, Pty) {
    let home = Home::new(name);
    let url = format!("http://127.0.0.1:{}", server.port);
    home.write(
        KEYS_FILE,
        &format!("ANTHROPIC_API_KEY={SECOND_KEY}\nANTHROPIC_BASE_URL={url}\n"),
    );
    let dead = dead_keychain(&home.0);
    let mut envs: Vec<(&str, &str)> = dead.iter().map(|(k, v)| (*k, v.as_str())).collect();
    envs.push(("ANTHROPIC_API_KEY", KEY));
    envs.push(("ANTHROPIC_BASE_URL", &url));
    let mut session = launch(&home.0, &[], &envs);
    session.wait_for("Store these in the keychain? Each is checked first.");
    session.send(b"\r");
    session.wait_for("Name for ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL [127]: ");
    session.send(b"gw\r");
    session.wait_for("Import ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL in the environment?");
    session.send(b"\r");
    session.wait_for(
        "The keychain isn't running. Keep these keys in ~/.config/wisp/credentials.json instead?",
    );
    (home, session)
}

#[test]
fn declining_the_file_after_a_keychain_failure_stops_setup_before_the_env_file() {
    if !usable() {
        return;
    }
    let server = serve("REPLY-NEVER");
    let (home, mut session) = two_keys("decline-file", &server);
    session.send(b"2\r");
    session.wait_for("Setup stopped: the rest of your environment's keys and the keys in ~/.config/wisp/.env weren't imported. Run wisp setup to try again.");
    let (status, screen) = session.finish_screen();
    assert_eq!(status.code(), Some(1), "{status}\n{screen}");
    assert_eq!(screen.matches("Name for").count(), 1, "{screen}");
    assert!(!home.exists(CREDENTIALS), "{screen}");
    assert!(!screen.contains("CANARY"), "{screen}");
}

#[test]
fn keeping_keys_in_the_file_is_asked_once_for_the_whole_setup() {
    if !usable() {
        return;
    }
    let server = serve("REPLY-AFTER-TWO");
    let (home, mut session) = two_keys("keep-file", &server);
    session.send(b"1\r");
    session.wait_for_count(
        "Name for ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL [127]: ",
        2,
    );
    session.send(b"\r");
    session.wait_for("and ANTHROPIC_BASE_URL in ~/.config/wisp/.env?");
    session.send(b"\r");
    session.wait_for(
        "Delete ~/.config/wisp/.env now? Its keys are stored, so Wisp no longer needs it.",
    );
    session.send(b"\r");
    session.wait_for(READY);
    session.send(b"\x04");
    let (status, screen) = session.finish_screen();
    assert!(status.success(), "{status}\n{screen}");
    assert!(home.exists(KEYS_FILE));
    assert_eq!(
        screen.matches("credentials.json instead?").count(),
        2,
        "{screen}"
    );
    let credentials = home.read(CREDENTIALS);
    assert!(credentials.contains(KEY));
    assert!(credentials.contains(SECOND_KEY));
    assert!(!screen.contains("CANARY"), "{screen}");
}
