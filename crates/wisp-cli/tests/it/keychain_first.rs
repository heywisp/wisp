#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket or process error"
)]

use crate::support;

use std::process::{Command, Stdio};

use support::server::{Server, message_start, sse, text_reply};
use support::{
    WISP,
    home::{CONFIG, CREDENTIALS, Home, KEYS_FILE, both, command_in, err, run},
};

const MODEL: &str = "claude-sonnet-5-5";
const STORE_ID: &str = "5f0c2a9e41d7b388";
const STORED_KEY: &str = "sk-ant-storedkeyCANARY0001";
const EXPORTED: &str = "sk-ant-CANARYEXPORTED0001";
const FILE_KEY: &str = "sk-ant-CANARYFILEKEY0002";
const GIVEN_KEY: &str = "sk-ant-CANARYGIVENFILE0003";
const TABLE_KEY: &str = "CANARYTABLEKEY0004";
const URL_SECRET: &str = "CANARYURLPASS0005";
const CANARIES: [&str; 6] = [
    "CANARYEXPORTED",
    "CANARYFILEKEY",
    "CANARYGIVENFILE",
    "CANARYTABLEKEY",
    "CANARYURLPASS",
    "CANARYURLPATH",
];

impl Server {
    fn request_count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn all(&self) -> String {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| format!("{}\n{}\n", s.head, s.body))
            .collect()
    }
}

fn start_events() -> Vec<serde_json::Value> {
    vec![
        message_start("msg_1", MODEL, 1),
        serde_json::json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
    ]
}

fn reply() -> String {
    text_reply("msg_1", MODEL, "REPLY-DONE", (1, 5))
}

fn echoing(text: &str) -> String {
    let mut events = start_events();
    events.extend([
        serde_json::json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": text}}),
        serde_json::json!({"type": "error",
            "error": {"type": "invalid_request_error", "message": "stopped"}}),
    ]);
    sse(&events)
}

impl Home {
    fn keychain_command(&self, args: &[&str]) -> Command {
        let mut command = command_in(&self.0, WISP);
        command
            .env("WISP_KEYCHAIN", "on")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", self.0.join("no-bus").display()),
            )
            .env("XDG_RUNTIME_DIR", self.0.join("no-runtime"))
            .args(["--time", "off", "--model", MODEL])
            .args(args);
        command
    }
}

fn assert_no_canary(where_: &str, text: &str) {
    for canary in CANARIES {
        assert!(!text.contains(canary), "{canary} in {where_}:\n{text}");
    }
}

struct Canaries {
    trap: Server,
}

fn plant(home: &Home, stored: Option<&Server>) -> Canaries {
    let trap = Server::replying(reply());
    let trap_url = format!(
        "http://u:{URL_SECRET}@127.0.0.1:{}/CANARYURLPATH",
        trap.port
    );
    let mut config = format!("store_id = \"{STORE_ID}\"\n");
    if stored.is_some() {
        config.push_str(
            "default = \"direct\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"file\"\n",
        );
    } else {
        config.push_str("default = \"work\"\n");
    }
    config.push_str(&format!(
        "[endpoints.work]\nauth = \"bearer\"\nkey_env = \"WORK_KEY\"\nurl_env = \"WORK_URL\"\n[endpoints.gw]\nauth = \"api_key\"\nkey_env = \"GW_KEY\"\nurl = \"{}\"\n",
        trap.url()
    ));
    home.write(CONFIG, &config);
    if let Some(server) = stored {
        home.write(
            CREDENTIALS,
            &serde_json::json!({"version": 1, "stores": {STORE_ID: {
                "direct": {"key": STORED_KEY, "url": server.url()}
            }}})
            .to_string(),
        );
    }
    home.write(
        KEYS_FILE,
        &format!("ANTHROPIC_API_KEY={FILE_KEY}\nGW_KEY={FILE_KEY}\nWORK_URL={trap_url}\n"),
    );
    home.write("given.env", &format!("ANTHROPIC_AUTH_TOKEN={GIVEN_KEY}\n"));
    Canaries { trap }
}

fn planted_env(command: &mut Command, canaries: &Canaries) {
    let trap_url = format!(
        "http://u:{URL_SECRET}@127.0.0.1:{}/CANARYURLPATH",
        canaries.trap.port
    );
    command
        .env("ANTHROPIC_API_KEY", EXPORTED)
        .env("ANTHROPIC_BASE_URL", &trap_url)
        .env("WORK_KEY", TABLE_KEY)
        .env("WORK_URL", &trap_url)
        .env("GW_KEY", TABLE_KEY);
}

#[test]
fn a_session_on_a_keychain_machine_sends_only_its_stored_key() {
    let home = Home::configured("session");
    let direct = Server::replying(reply());
    let canaries = plant(&home, Some(&direct));
    let mut command = home.keychain_command(&["--output", "jsonl"]);
    planted_env(&mut command, &canaries);
    let output = run(&mut command, "hello\n");
    assert!(output.status.success(), "{}", both(&output));
    assert!(both(&output).contains("REPLY-DONE"), "{}", both(&output));
    assert_eq!(direct.request_count(), 1);
    let seen = direct.all();
    assert!(seen.contains(&format!("x-api-key: {STORED_KEY}")), "{seen}");
    assert_no_canary("the request", &seen);
    assert_eq!(canaries.trap.request_count(), 0);
    assert_no_canary("the output", &both(&output));
    assert_no_canary("the log", &home.logs());
    assert!(home.logs().contains(
        "\"credential_source\":{\"mode\":{\"billing\":\"metered\",\"source\":\"credential_file\"}}"
    ));
}

#[test]
fn the_log_scrubs_every_variable_value_it_never_read() {
    let home = Home::configured("scrub");
    let echoed = format!(
        "exported {EXPORTED} file {FILE_KEY} table {TABLE_KEY} pass {URL_SECRET} path /CANARYURLPATH"
    );
    let direct = Server::replying(echoing(&echoed));
    let canaries = plant(&home, Some(&direct));
    let mut command = home.keychain_command(&["--output", "jsonl"]);
    planted_env(&mut command, &canaries);
    let output = run(&mut command, "hello\n");
    assert_eq!(canaries.trap.request_count(), 0, "{}", both(&output));
    let logs = home.logs();
    let ended: Vec<serde_json::Value> = logs
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|r| r["type"] == "turn_ended")
        .collect();
    assert_eq!(ended.len(), 1, "{logs}");
    let partial = ended[0]["partial_output"].as_str().unwrap_or_default();
    assert!(
        partial.starts_with("exported [redacted] file [redacted]"),
        "{partial}"
    );
    assert_no_canary("turn_ended", &ended[0].to_string());
}

#[test]
fn env_file_and_the_env_endpoint_are_refused_on_a_session() {
    let home = Home::configured("refused");
    let direct = Server::replying(reply());
    let canaries = plant(&home, Some(&direct));
    let mut command = home.keychain_command(&["--output", "jsonl", "--env-file", "given.env"]);
    planted_env(&mut command, &canaries);
    let output = run(&mut command, "hello\n");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        err(&output).trim_end(),
        "wisp: --env-file supplies variables, which Wisp doesn't read on a machine with a keychain. Store its keys once with: wisp account import --env-file FILE"
    );
    let mut command = home.keychain_command(&["--output", "jsonl", "--endpoint", "env"]);
    planted_env(&mut command, &canaries);
    let output = run(&mut command, "hello\n");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        err(&output).trim_end(),
        "wisp: the keys-file account is unavailable: ~/.config/wisp/.env and the ANTHROPIC_* variables aren't read on a machine with a keychain; wisp account import --from-environment stores them"
    );
    assert_eq!(direct.request_count(), 0);
    assert_eq!(canaries.trap.request_count(), 0);
    assert_no_canary("the output", &both(&output));
}

#[test]
fn without_a_stored_endpoint_the_keychain_message_wins() {
    let home = Home::configured("message");
    let canaries = plant(&home, None);
    let mut command = home.keychain_command(&["--output", "jsonl"]);
    planted_env(&mut command, &canaries);
    let output = run(&mut command, "hello\n");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        err(&output).trim_end(),
        "wisp: no account is set up yet. ~/.config/wisp/.env holds ANTHROPIC_API_KEY, but this machine has a keychain, so Wisp doesn't read it. Store it once with: wisp account import\nFor one run without the keychain: --keychain off"
    );
    assert_eq!(canaries.trap.request_count(), 0);
    let output = run(
        home.keychain_command(&["--output", "jsonl", "--endpoint", "work"])
            .env("WORK_KEY", TABLE_KEY),
        "hello\n",
    );
    assert_eq!(
        err(&output).trim_end(),
        "wisp: account \"work\" is unavailable: its key is in the variable WORK_KEY, which Wisp doesn't read on a machine with a keychain; wisp account import stores it"
    );
    assert_no_canary("the output", &both(&output));
}

#[test]
fn a_down_keychain_default_keeps_its_own_reason_and_sends_nothing() {
    let home = Home::configured("down");
    let canaries = plant(&home, None);
    home.write(
        CONFIG,
        &format!(
            "store_id = \"{STORE_ID}\"\ndefault = \"direct\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"keyring\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n"
        ),
    );
    let mut command = home.keychain_command(&["--output", "jsonl"]);
    planted_env(&mut command, &canaries);
    let output = run(&mut command, "hello\n");
    assert_eq!(output.status.code(), Some(1));
    let text = err(&output);
    assert!(
        text.starts_with("wisp: the default account \"direct\" is unavailable: the keychain"),
        "{text}"
    );
    assert!(!text.contains("no account is set up yet"), "{text}");
    assert_eq!(canaries.trap.request_count(), 0);
    assert_no_canary("the output", &both(&output));
}

#[test]
fn a_broken_env_file_beside_a_stored_endpoint_does_not_stop_a_start() {
    let home = Home::configured("broken");
    let direct = Server::replying(reply());
    let canaries = plant(&home, Some(&direct));
    home.write(
        KEYS_FILE,
        &format!("ANTHROPIC_API_KEY={FILE_KEY}\nthis is not a line {FILE_KEY}\n"),
    );
    let output = run(
        &mut home.keychain_command(&["--output", "jsonl"]),
        "hello\n",
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(direct.request_count(), 1);
    assert_no_canary("the output", &both(&output));
    let doctor = home.keychain_command(&["doctor"]).output().unwrap();
    let text = both(&doctor);
    assert!(
        text.contains("~/.config/wisp/.env doesn't parse (line 2); it isn't read on this machine"),
        "{text}"
    );
    assert!(!text.contains("broken"), "{text}");
    assert_no_canary("doctor", &text);
    assert_eq!(canaries.trap.request_count(), 0);
}

#[test]
fn doctor_and_its_check_never_build_an_endpoint_from_a_variable() {
    let home = Home::configured("doctor");
    let direct = Server::replying(reply());
    let canaries = plant(&home, Some(&direct));
    let config = home.0.join(CONFIG);
    let planted = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        format!("{planted}\n[endpoints.env]\nnative_web_search = false\n"),
    )
    .unwrap();
    let mut command = home.keychain_command(&["doctor", "--check"]);
    planted_env(&mut command, &canaries);
    let output = command.output().unwrap();
    let text = both(&output);
    assert!(
        text.contains("  environment        ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL are set, not read: this machine has a keychain"),
        "{text}"
    );
    let keys_file_row = |l: &str| {
        ["  env ", "  claude-api ", "  gateway "]
            .iter()
            .any(|row| l.starts_with(row))
    };
    assert!(!text.lines().any(keys_file_row), "{text}");
    assert!(
        text.contains(
            "the keys-file account in config.toml is unused: ~/.config/wisp/.env and the ANTHROPIC_* variables aren't read on a machine with a keychain"
        ),
        "{text}"
    );
    let search = text
        .split("\nweb search\n")
        .nth(1)
        .unwrap_or_default()
        .split("\n\n")
        .next()
        .unwrap_or_default();
    assert!(search.lines().any(|l| l.starts_with("  direct ")), "{text}");
    assert!(!search.lines().any(keys_file_row), "{search}");
    assert!(
        text.contains("  work               not checked: its key is in the variable WORK_KEY, which Wisp doesn't read on a machine with a keychain; wisp account import stores it"),
        "{text}"
    );
    assert!(
        text.contains(
            "  warning            ~/.config/wisp/.env holds keys Wisp doesn't read on this machine"
        ),
        "{text}"
    );
    assert_eq!(canaries.trap.request_count(), 0, "{text}");
    assert_eq!(direct.request_count(), 1);
    assert_no_canary("the check", &direct.all());
    assert_no_canary("doctor", &text);
    let on_env = planted.replace("default = \"direct\"", "default = \"env\"");
    std::fs::write(
        &config,
        format!("{on_env}\n[endpoints.env]\npricing = \"none\"\n"),
    )
    .unwrap();
    let mut command = home.keychain_command(&["doctor"]);
    planted_env(&mut command, &canaries);
    let text = both(&command.output().unwrap());
    let account = "the keys-file account, where new chats start, is unavailable";
    assert!(
        text.contains("  settings           endpoint = the keys-file account\n"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l == format!("  warning            {account}")),
        "{text}"
    );
    assert!(!text.lines().any(keys_file_row), "{text}");
    assert_no_canary("doctor on env", &text);
}

#[test]
fn an_empty_or_blank_variable_is_not_present() {
    let home = Home::configured("blank");
    for value in ["", "   "] {
        let output = home
            .keychain_command(&["doctor"])
            .env("ANTHROPIC_API_KEY", value)
            .output()
            .unwrap();
        let text = both(&output);
        assert!(!text.contains("  environment "), "{value:?}: {text}");
    }
}

#[test]
fn with_nothing_found_the_keychain_machine_does_not_suggest_variables() {
    let home = Home::configured("nothing");
    let output = run(
        &mut home.keychain_command(&["--output", "jsonl"]),
        "hello\n",
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        err(&output).trim_end(),
        "wisp: no account is set up yet. Run \"wisp setup\" in a terminal, or store a key with:\nprintenv VAR | wisp account add NAME --key-stdin"
    );
    let output = run(
        home.keychain_command(&["--output", "jsonl"])
            .env("WISP_KEYCHAIN", "off"),
        "hello\n",
    );
    assert_eq!(
        err(&output).trim_end(),
        "wisp: no Anthropic credentials: set ANTHROPIC_API_KEY (an API key) or ANTHROPIC_AUTH_TOKEN (a bearer token for a gateway) in the environment\nOr store a key: run \"wisp setup\" in a terminal, or printenv VAR | wisp account add NAME --key-stdin"
    );
    let output = run(
        home.keychain_command(&["--output", "jsonl"])
            .env("ANTHROPIC_API_KEY", EXPORTED),
        "hello\n",
    );
    assert_eq!(
        err(&output).trim_end(),
        "wisp: no account is set up yet. ANTHROPIC_API_KEY is set, but this machine has a keychain, so Wisp doesn't read it. Store it once with: wisp account import --from-environment\nFor one run without the keychain: --keychain off"
    );
    assert_no_canary("the output", &both(&output));
    assert_eq!(home.read(CONFIG), "");
}

#[test]
fn keychain_off_keeps_todays_variables() {
    let home = Home::configured("off");
    let server = Server::replying(reply());
    let output = run(
        home.keychain_command(&["--output", "jsonl", "--keychain", "off"])
            .env("ANTHROPIC_API_KEY", EXPORTED)
            .env("ANTHROPIC_BASE_URL", server.url()),
        "hello\n",
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(server.request_count(), 1);
    assert!(server.all().contains(&format!("x-api-key: {EXPORTED}")));
}

#[test]
fn a_variable_table_added_on_a_keychain_machine_says_it_is_not_read_here() {
    let home = Home::configured("add-table");
    let output = home
        .keychain_command(&["endpoint", "add", "work", "--key-env", "WORK_KEY"])
        .env("WORK_KEY", TABLE_KEY)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", both(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines().any(|l| l
            == "WORK_KEY isn't read on this machine, which has a keychain; wisp account import stores its key"),
        "{text}"
    );
    let output = home
        .keychain_command(&["endpoint", "edit", "work", "--key-env", "OTHER_KEY"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", both(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(
            "checks it. OTHER_KEY isn't read on this machine, which has a keychain; wisp account import stores its key"
        ),
        "{}",
        both(&output)
    );
    assert_no_canary("the output", &both(&output));
}

#[test]
fn doctor_names_unread_variables_even_when_the_config_is_broken() {
    let home = Home::configured("broken-config");
    home.write(CONFIG, "this is = = not toml\n");
    let output = home
        .keychain_command(&["doctor"])
        .env("ANTHROPIC_API_KEY", EXPORTED)
        .output()
        .unwrap();
    let text = both(&output);
    assert!(
        text.contains(
            "  environment        ANTHROPIC_API_KEY is set, not read: this machine has a keychain"
        ),
        "{text}"
    );
    assert_no_canary("doctor", &text);
}
