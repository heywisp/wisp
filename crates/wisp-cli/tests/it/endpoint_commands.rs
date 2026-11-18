#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket or process error"
)]

use crate::runner;
use crate::support;

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Output};

use runner::KEY;
use support::home::{
    CONFIG, CONFIG_DIR, CREDENTIALS, Home, KEYS_FILE, assert_clean, both, err, mode, out, run,
};
use support::server::{Server, counted, json};

const SHORT_KEY: &str = "CANARYSHORT";
const KEY_ON_COMMAND_LINE: &str = "wisp: A key on the command line is saved in your shell history and visible to other programs, so nothing was saved. Replace that key with your provider, then use --key-stdin or the prompt.";

#[test]
fn add_list_default_and_remove_round_trip() {
    let server = Server::replying(counted());
    let home = Home::new("round-trip");
    let url = server.url();
    let output = home.endpoint_run(
        &[
            "endpoint",
            "add",
            "direct",
            "--key-stdin",
            "--url",
            &url,
            "--auth",
            "api_key",
            "--default",
        ],
        &format!("{KEY}\n"),
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(
        out(&output),
        "Saved \"direct\" as your default account. Its key and URL are in ~/.config/wisp/credentials.json, readable only by you.\n"
    );
    assert!(err(&output).contains("Checking it with one count_tokens request"));
    assert_clean(&output, &[KEY]);
    let heads = server.heads();
    assert_eq!(heads.len(), 1, "{heads:?}");
    assert!(
        heads[0].contains("/v1/messages/count_tokens"),
        "{}",
        heads[0]
    );
    assert!(
        heads[0]
            .to_ascii_lowercase()
            .contains(&format!("x-api-key: {}", KEY.to_ascii_lowercase())),
        "{}",
        heads[0]
    );
    let config = home.read(CONFIG);
    assert!(config.contains("default = \"direct\""), "{config}");
    assert!(
        config.contains("[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"file\"\n"),
        "{config}"
    );
    assert!(!config.contains(KEY) && !config.contains(&url), "{config}");
    assert_eq!(mode(&home.file(CONFIG)), 0o600);
    assert_eq!(mode(&home.file(CREDENTIALS)), 0o600);
    assert_eq!(mode(&home.file(CONFIG_DIR)), 0o700);
    assert!(home.read(CREDENTIALS).contains(KEY));

    let output = home.endpoint_run(
        &[
            "endpoint",
            "add",
            "gw",
            "--key-stdin",
            "--no-check",
            "--url",
            &url,
            "--auth",
            "bearer",
            "--pricing",
            "none",
        ],
        SHORT_KEY,
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(server.heads().len(), 1);
    assert!(!err(&output).contains("Checking"));

    let output = home.endpoint_run(&["endpoint", "list"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert_clean(&output, &[KEY, SHORT_KEY]);
    let list = out(&output);
    let row = |name: &str| list.lines().find(|l| l.contains(name)).unwrap_or_default();
    assert!(row("direct").contains("127.0.0.1"), "{list}");
    assert!(row("direct").contains("starts new chats"), "{list}");
    assert!(row("gw ").contains("127.0.0.1"), "{list}");
    assert!(!row("gw ").contains("starts new chats"), "{list}");
    assert!(!list.contains("credentials.json"), "{list}");
    assert!(!list.contains("a1b2"), "{list}");

    let output = home.endpoint_run(&["endpoint", "default", "gw"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(out(&output), "\"gw\" is now the default account.\n");
    assert!(home.read(CONFIG).contains("default = \"gw\""));

    let output = home.endpoint_run(&["endpoint", "remove", "gw"], "");
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert!(err(&output).contains("pass --yes"), "{}", both(&output));
    assert!(home.read(CREDENTIALS).contains(SHORT_KEY));

    let output = home.endpoint_run(&["endpoint", "remove", "gw", "--yes"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(
        out(&output),
        "Removed \"gw\" and deleted its key from ~/.config/wisp/credentials.json.\nNew chats now start on direct\u{a0}\u{b7}\u{a0}API.\n"
    );
    assert!(!home.read(CREDENTIALS).contains(SHORT_KEY));
    let config = home.read(CONFIG);
    assert!(
        !config.contains("gw") && config.contains("default = \"direct\""),
        "{config}"
    );

    let output = home.endpoint_run(&["endpoint", "remove", "gw", "--yes"], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        err(&output),
        "wisp: there is no account \"gw\"; nothing was changed\n"
    );
    let output = home.endpoint_run(
        &["endpoint", "add", "direct", "--key-stdin", "--no-check"],
        KEY,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(err(&output).contains("already exists"), "{}", both(&output));
}

#[test]
fn a_stored_endpoint_starts_a_session() {
    let server = Server::replying(counted());
    let home = Home::new("session");
    let url = server.url();
    let output = home.endpoint_run(
        &[
            "endpoint",
            "add",
            "gw",
            "--key-stdin",
            "--no-check",
            "--url",
            &url,
            "--auth",
            "bearer",
        ],
        KEY,
    );
    assert!(output.status.success(), "{}", both(&output));
    let output = home.endpoint_run(&["doctor"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert!(
        out(&output).contains("gw (the only available account)"),
        "{}",
        out(&output)
    );
    assert_clean(&output, &[KEY]);
}

#[test]
fn each_check_outcome_saves_or_stops_without_a_terminal() {
    let cases = [
        (
            "401 Unauthorized",
            r#"{"error":"x"}"#,
            false,
            "the key was refused (401), so nothing was saved",
        ),
        (
            "403 Forbidden",
            r#"{"error":"x"}"#,
            true,
            "The key works but may not count tokens here.",
        ),
        (
            "404 Not Found",
            r#"{"error":"x"}"#,
            true,
            "Couldn't check this account: it doesn't offer count_tokens.",
        ),
        (
            "500 Internal Server Error",
            r#"{"error":"x"}"#,
            false,
            "the check failed (unexpected answer (HTTP 500)), so nothing was saved; --no-check saves without it",
        ),
        (
            "200 OK",
            "not json CANARYBODY",
            false,
            "the check failed (unexpected answer (HTTP 200): the body is not JSON), so nothing was saved",
        ),
    ];
    for (status, body, saved, words) in cases {
        let server = Server::replying(json(status, body));
        let home = Home::new("outcomes");
        let url = server.url();
        let output = home.endpoint_run(
            &[
                "endpoint",
                "add",
                "gw",
                "--key-stdin",
                "--url",
                &url,
                "--auth",
                "bearer",
            ],
            KEY,
        );
        assert_clean(&output, &[KEY]);
        assert!(both(&output).contains(words), "{status}: {}", both(&output));
        assert_eq!(
            output.status.success(),
            saved,
            "{status}: {}",
            both(&output)
        );
        assert_eq!(home.file(CONFIG).exists(), saved, "{status}");
        assert_eq!(home.read(CREDENTIALS).contains(KEY), saved, "{status}");
        assert_eq!(server.heads().len(), 1, "{status}");
    }
    let held = tokio::net::TcpSocket::new_v4().unwrap();
    held.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("http://127.0.0.1:{}", held.local_addr().unwrap().port());
    let home = Home::new("unreachable");
    let output = home.endpoint_run(
        &[
            "endpoint",
            "add",
            "gw",
            "--key-stdin",
            "--url",
            &url,
            "--auth",
            "bearer",
        ],
        KEY,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).contains("the check failed (unreachable: could not connect)"),
        "{}",
        both(&output)
    );
    assert!(!home.file(CONFIG).exists());
}

#[test]
fn the_local_key_check_refuses_with_its_reason_and_no_echo() {
    let printable = "has a space, a control character or a non-ASCII character, so it can't be sent in a header";
    let long = "k".repeat(wisp_protocol::KEY_MAX + 1);
    let too_long = format!("is longer than {} characters", wisp_protocol::KEY_MAX);
    let cases: [(&str, &str); 6] = [
        ("sk-ant-CANARY one", printable),
        ("sk-ant-CANARY\tone", printable),
        ("sk-ant-CANARY\u{7}one", printable),
        ("sk-ant-CANARY\u{e9}one", printable),
        ("", "is empty"),
        (&long, &too_long),
    ];
    let home = Home::new("key-check");
    for (key, reason) in cases {
        let output = home.endpoint_run(
            &["endpoint", "add", "direct", "--key-stdin", "--no-check"],
            &format!("{key}\n"),
        );
        assert_eq!(output.status.code(), Some(1), "{}", both(&output));
        assert_eq!(
            err(&output),
            format!("wisp: That key {reason}. Nothing was saved.\n")
        );
        assert!(!both(&output).contains(&long));
        assert_clean(&output, &[]);
        assert!(!home.file(CONFIG).exists());
    }
}

#[test]
fn keys_and_bad_values_on_the_command_line_are_refused_without_echo() {
    let home = Home::new("canaries");
    let canary = "sk-ant-CANARYARG0001";
    for flag in ["--key", "--api-key", "--token"] {
        let output = home.endpoint_run(&["endpoint", "add", "direct", flag, canary], "");
        assert_eq!(output.status.code(), Some(2), "{flag}: {}", both(&output));
        assert_eq!(err(&output), format!("{KEY_ON_COMMAND_LINE}\n"));
        assert_clean(&output, &[canary]);
    }
    let usage_cases: [&[&str]; 9] = [
        &[canary],
        &["endpoint", canary],
        &["endpoint", "add", "direct", canary],
        &["endpoint", "add", "Sk-Ant-CANARYNAME"],
        &["endpoint", "add", "--auth", canary],
        &["--effort", canary],
        &["--output", canary],
        &["--retry-budget", canary],
        &["--time", canary],
    ];
    for args in usage_cases {
        let output = home.endpoint_run(args, "");
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", both(&output));
        assert_clean(&output, &[canary]);
        assert!(out(&output).is_empty(), "{args:?}");
    }
    let output = home.endpoint_run(&["--effort", canary], "");
    assert!(
        err(&output).contains("low, medium, high, xhigh, max"),
        "{}",
        both(&output)
    );
    let output = home.endpoint_run(&["--efort", "high"], "");
    assert!(
        err(&output).contains("tip: a similar argument exists: --effort"),
        "{}",
        both(&output)
    );
    let output = home.endpoint_run(&["endpoint"], "");
    assert!(
        both(&output).contains("Usage: wisp account"),
        "{}",
        both(&output)
    );
    assert!(!home.file(CONFIG).exists());
}

#[test]
fn a_given_config_path_is_never_echoed() {
    let home = Home::new("given");
    let canary = "sk-ant-CANARYCONFIG.toml";
    let missing_folder = "sk-ant-CANARYFOLDER/config.toml";
    for args in [
        vec!["endpoint", "list"],
        vec!["endpoint", "remove", "x", "--yes"],
        vec!["endpoint", "default", "x"],
        vec!["doctor"],
    ] {
        let mut all = vec!["--config", canary];
        all.extend(&args);
        let output = home.endpoint_run(&all, "");
        assert!(!output.status.success(), "{args:?}");
        assert!(
            both(&output).contains("the file given with --config doesn't exist"),
            "{args:?}: {}",
            both(&output)
        );
        assert_clean(&output, &[canary]);
    }
    for args in [
        vec!["endpoint", "list"],
        vec!["endpoint", "add", "x", "--key-stdin", "--no-check"],
        vec!["endpoint", "default", "x"],
    ] {
        let mut all = vec!["--config", missing_folder];
        all.extend(&args);
        let output = home.endpoint_run(&all, KEY);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            err(&output).contains("the folder of the file given with --config doesn't exist"),
            "{args:?}: {}",
            both(&output)
        );
        assert_clean(&output, &[missing_folder]);
    }
    let output = home.endpoint_run(
        &[
            "endpoint",
            "--config",
            canary,
            "add",
            "direct",
            "--key-stdin",
            "--no-check",
        ],
        KEY,
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(
        out(&output),
        "Saved \"direct\". Its key is in credentials.json beside the file given with --config, readable only by you.\nCreated the file given with --config.\n"
    );
    assert!(err(&output).contains("the file given with --config doesn't exist yet"));
    assert_clean(&output, &[canary, KEY]);
    assert_eq!(mode(&home.file(canary)), 0o600);
    assert!(home.read("credentials.json").contains(KEY));
    let output = home.endpoint_run(&["--config", canary, "endpoint", "list"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert_clean(&output, &[canary, KEY]);
    std::fs::set_permissions(home.file(canary), std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = home.endpoint_run(&["--config", canary, "endpoint", "default", "direct"], "");
    assert!(output.status.success(), "{}", both(&output));
    let output = home.endpoint_run(&["--config", canary, "doctor"], "");
    let doctor = out(&output);
    assert!(
        doctor.contains("config             the file given with --config\n"),
        "{doctor}"
    );
    assert!(
        doctor.contains("direct (default in the file given with --config)"),
        "{doctor}"
    );
    assert!(
        doctor.contains("from credentials.json beside the file given with --config"),
        "{doctor}"
    );
    assert_clean(&output, &[canary, KEY]);
    home.write(canary, "[endpoints.x\nkey = \"sk-ant-CANARYBROKEN\"\n");
    let output = home.endpoint_run(
        &["--config", canary, "endpoint", "add", "y", "--key-stdin"],
        KEY,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).starts_with("wisp: the file given with --config line 1"),
        "{}",
        both(&output)
    );
    assert_clean(&output, &[canary, KEY]);
}

#[test]
fn env_file_has_no_effect_on_writes_and_is_never_read() {
    let home = Home::new("env-file");
    home.write("broken.env", "not a line sk-ant-CANARYENV\n");
    let output = home.endpoint_run(
        &[
            "--env-file",
            "broken.env",
            "endpoint",
            "add",
            "direct",
            "--key-stdin",
            "--no-check",
        ],
        KEY,
    );
    assert!(output.status.success(), "{}", both(&output));
    assert!(
        err(&output)
            .contains("--env-file isn't used by wisp account add without --key-var; ignoring it"),
        "{}",
        both(&output)
    );
    for (command, args) in [
        ("default", vec!["default", "direct"]),
        ("remove", vec!["remove", "direct", "--yes"]),
    ] {
        let mut all = vec!["--env-file", "broken.env", "endpoint"];
        all.extend(&args);
        let output = home.endpoint_run(&all, "");
        assert!(output.status.success(), "{command}: {}", both(&output));
        assert_eq!(
            err(&output),
            format!("--env-file isn't used by wisp account {command}; ignoring it\n")
        );
        assert_clean(&output, &[]);
    }
}

#[test]
fn two_writers_at_once_both_land() {
    let home = Home::new("two-writers");
    home.dir(CONFIG_DIR);
    let children: Vec<Child> = (0..4)
        .map(|i| {
            let name = format!("e{i}");
            home.spawn(
                &["endpoint", "add", &name, "--key-stdin", "--no-check"],
                &format!("{KEY}{i}"),
            )
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", both(&output));
    }
    let config = home.read(CONFIG);
    let credentials = home.read(CREDENTIALS);
    for i in 0..4 {
        assert!(config.contains(&format!("[endpoints.e{i}]")), "{config}");
        assert!(credentials.contains(&format!("{KEY}{i}")));
    }
    assert_eq!(config.matches("store_id").count(), 1, "{config}");
}

#[test]
fn a_linked_config_is_written_through_its_link() {
    let home = Home::new("linked");
    let mine = home.write("dotfiles/wisp/config.toml", "# mine\n");
    std::fs::set_permissions(mine, std::fs::Permissions::from_mode(0o644)).unwrap();
    home.dir(CONFIG_DIR);
    std::os::unix::fs::symlink(home.file("dotfiles/wisp/config.toml"), home.file(CONFIG)).unwrap();
    let output = home.endpoint_run(
        &["endpoint", "add", "direct", "--key-stdin", "--no-check"],
        KEY,
    );
    assert!(output.status.success(), "{}", both(&output));
    assert!(
        std::fs::symlink_metadata(home.file(CONFIG))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        home.read("dotfiles/wisp/config.toml")
            .starts_with("# mine\n")
    );
    assert_eq!(mode(&home.file("dotfiles/wisp/config.toml")), 0o644);
    assert!(home.read(CREDENTIALS).contains(KEY));
    assert!(!home.file("dotfiles/wisp/credentials.json").exists());
}

#[test]
fn a_copied_config_gets_its_own_keys_on_its_first_write() {
    let home = Home::new("copied");
    let output = home.endpoint_run(
        &["endpoint", "add", "direct", "--key-stdin", "--no-check"],
        KEY,
    );
    assert!(output.status.success(), "{}", both(&output));
    std::fs::copy(home.file(CONFIG), home.file(".config/wisp/copy.toml")).unwrap();
    let output = home.endpoint_run(&["doctor", "--config", ".config/wisp/copy.toml"], "");
    assert!(
        out(&output).contains(
            "direct: shares its key with ~/.config/wisp/config.toml, which it was copied from; its next change gives this config its own copy"
        ),
        "{}",
        out(&output)
    );
    let output = home.endpoint_run(
        &[
            "--config",
            ".config/wisp/copy.toml",
            "endpoint",
            "default",
            "direct",
        ],
        "",
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(
        out(&output),
        "\"direct\" is now the default account.\nThis config.toml now keeps its own copy of its keys (they were written by ~/.config/wisp/config.toml).\n"
    );
    let original = home.read(CONFIG);
    let copy = home.read(".config/wisp/copy.toml");
    let id = |text: &str| {
        text.lines()
            .find(|l| l.starts_with("store_id"))
            .unwrap()
            .to_owned()
    };
    assert_ne!(id(&original), id(&copy));
    assert_eq!(home.read(CREDENTIALS).matches(KEY).count(), 2);
    for config in [None, Some(".config/wisp/copy.toml")] {
        let mut args = vec![];
        if let Some(config) = config {
            args.extend(["--config", config]);
        }
        args.extend(["endpoint", "list"]);
        let output = home.endpoint_run(&args, "");
        assert!(out(&output).contains("direct"), "{}", both(&output));
        assert!(!out(&output).contains("key missing"), "{}", both(&output));
    }
}

#[test]
fn a_dangling_config_link_leaves_no_key_behind() {
    let server = Server::replying(counted());
    let home = Home::new("dangling");
    home.dir(CONFIG_DIR);
    std::os::unix::fs::symlink(home.file("dotfiles/wisp/config.toml"), home.file(CONFIG)).unwrap();
    let url = server.url();
    for _ in 0..2 {
        let output = home.endpoint_run(
            &[
                "endpoint",
                "add",
                "gw",
                "--key-stdin",
                "--url",
                &url,
                "--auth",
                "bearer",
            ],
            KEY,
        );
        assert_eq!(output.status.code(), Some(1), "{}", both(&output));
        assert!(
            err(&output).contains("is a link to a file that doesn't exist"),
            "{}",
            both(&output)
        );
        assert_clean(&output, &[KEY]);
    }
    assert!(server.heads().is_empty(), "no probe before the refusal");
    assert!(!home.file(CREDENTIALS).exists());
    assert!(!home.file("dotfiles").exists());
}

#[test]
fn refusals_against_a_missing_default_config_create_nothing() {
    let home = Home::new("missing-default");
    for args in [
        vec!["endpoint", "default", "direct"],
        vec!["endpoint", "remove", "direct", "--yes"],
    ] {
        let output = home.endpoint_run(&args, "");
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", both(&output));
        assert_eq!(
            err(&output),
            "wisp: there is no account \"direct\"; nothing was changed\n"
        );
        assert!(!home.file(".config").exists(), "{args:?}");
    }
}

#[test]
fn an_env_file_under_a_file_is_missing_without_its_path() {
    let home = Home::new("env-not-dir");
    home.write("sk-ant-CANARYENVFILE", "");
    let output = home.endpoint_run(&["--env-file", "sk-ant-CANARYENVFILE/x.env", "doctor"], "");
    assert!(
        both(&output).contains("the file given with --env-file doesn't exist"),
        "{}",
        both(&output)
    );
    assert_clean(&output, &[]);
}

#[test]
fn refusals_take_no_lock_and_the_first_real_write_still_gets_the_note() {
    let home = Home::new("no-lock-refusals");
    wisp_core::instructions::make_repository(&home.0).unwrap();
    home.write(
        CONFIG,
        "[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"DIRECT_KEY\"\n",
    );
    let lock = home.file(".config/wisp/.wisp.lock");
    for args in [
        vec!["endpoint", "default", "typo"],
        vec!["endpoint", "remove", "typo", "--yes"],
        vec!["endpoint", "remove", "typo"],
    ] {
        let output = home.endpoint_run(&args, "");
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", both(&output));
        assert_eq!(
            err(&output),
            "wisp: there is no account \"typo\"; nothing was changed\n",
            "{args:?}"
        );
        assert!(!lock.exists(), "{args:?}");
    }
    let output = home.endpoint_run(&["endpoint", "default", "direct"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert!(
        err(&output).contains("note: config.toml's lock file .wisp.lock is in ~/.config/wisp, inside the git repository"),
        "{}",
        both(&output)
    );
    assert!(lock.exists());

    let dangling = Home::new("no-lock-dangling");
    dangling.dir(CONFIG_DIR);
    std::os::unix::fs::symlink(dangling.file("gone/config.toml"), dangling.file(CONFIG)).unwrap();
    for args in [
        vec!["endpoint", "default", "direct"],
        vec!["endpoint", "default", "env"],
        vec!["endpoint", "remove", "direct", "--yes"],
    ] {
        let output = dangling.endpoint_run(&args, "");
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", both(&output));
        assert!(
            err(&output).contains("is a link to a file that doesn't exist"),
            "{args:?}: {}",
            both(&output)
        );
        assert!(
            !dangling.file(".config/wisp/.wisp.lock").exists(),
            "{args:?}"
        );
    }
}

#[test]
fn refusals_before_the_locks_leave_no_lock_and_the_note_comes_later() {
    let home = Home::new("refuse-early");
    for (name, key) in [("a1", KEY), ("a2", SHORT_KEY)] {
        let output =
            home.endpoint_run(&["endpoint", "add", name, "--key-stdin", "--no-check"], key);
        assert!(output.status.success(), "{}", both(&output));
    }
    wisp_core::instructions::make_repository(&home.file("repo")).unwrap();
    std::fs::create_dir_all(home.file("repo/sub")).unwrap();
    std::fs::copy(home.file(CONFIG), home.file("repo/sub/config.toml")).unwrap();
    let lock = home.file("repo/sub/.wisp.lock");
    for args in [
        vec!["endpoint", "default", "a2"],
        vec!["endpoint", "remove", "a1", "--yes"],
    ] {
        let mut all = vec!["--config", "repo/sub/config.toml"];
        all.extend(&args);
        let output = home.endpoint_run(&all, "");
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", both(&output));
        assert!(
            err(&output).contains("this config.toml must get its own copy of its keys first"),
            "{args:?}: {}",
            both(&output)
        );
        assert!(!lock.exists(), "{args:?}");
    }
    std::fs::copy(
        home.file(CREDENTIALS),
        home.file("repo/sub/credentials.json"),
    )
    .unwrap();
    let output = home.endpoint_run(
        &[
            "--config",
            "repo/sub/config.toml",
            "endpoint",
            "default",
            "a2",
        ],
        "",
    );
    assert!(output.status.success(), "{}", both(&output));
    assert!(
        err(&output).contains("lock file .wisp.lock beside the file given with --config"),
        "{}",
        both(&output)
    );
    assert!(lock.exists());
}

#[test]
fn a_corrupt_credentials_file_is_worded_alike_and_takes_no_lock() {
    let home = Home::new("corrupt-store");
    let output = home.endpoint_run(&["endpoint", "add", "a1", "--key-stdin", "--no-check"], KEY);
    assert!(output.status.success(), "{}", both(&output));
    std::fs::remove_file(home.file(".config/wisp/.wisp.lock")).unwrap();
    home.write(CREDENTIALS, "{ not json sk-ant-CANARYBROKEN");
    for args in [
        vec!["endpoint", "default", "a1"],
        vec!["endpoint", "remove", "a1", "--yes"],
        vec!["endpoint", "add", "a2", "--key-stdin", "--no-check"],
    ] {
        let output = home.endpoint_run(&args, KEY);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", both(&output));
        assert_eq!(
            err(&output),
            "wisp: credentials.json line 1: not valid JSON; fix or move aside ~/.config/wisp/credentials.json first; nothing was changed\n",
            "{args:?}"
        );
        assert_clean(&output, &[KEY]);
        assert!(!home.file(".config/wisp/.wisp.lock").exists(), "{args:?}");
    }
}

#[test]
fn remove_env_says_it_has_no_table() {
    let home = Home::new("remove-env");
    let output = home.endpoint_run(&["endpoint", "remove", "env", "--yes"], "");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        err(&output),
        "wisp: the keys-file account has no table to remove\n"
    );
}

#[test]
fn a_linked_config_in_an_unwritable_folder_says_so() {
    let home = Home::new("unwritable-target");
    home.write("dotfiles/config.toml", "");
    home.dir(CONFIG_DIR);
    std::os::unix::fs::symlink(home.file("dotfiles/config.toml"), home.file(CONFIG)).unwrap();
    std::fs::set_permissions(
        home.file("dotfiles"),
        std::fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let writable = std::fs::write(home.file("dotfiles/probe"), "").is_ok();
    let output = home.endpoint_run(&["endpoint", "default", "env"], "");
    std::fs::set_permissions(
        home.file("dotfiles"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    if writable {
        return;
    }
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert!(
        err(&output)
            == "wisp: ~/.config/wisp/config.toml \u{2192} ~/dotfiles/config.toml: Wisp can't write that folder; nothing was changed\n",
        "{}",
        both(&output)
    );
    assert!(!err(&output).contains("endpoint lock"), "{}", both(&output));
    assert!(!home.file(".config/wisp/.wisp.lock").exists());
}

const KEYCHAIN_CONFIG: &str = "store_id = \"0123456789abcdef\"\n\n[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"keyring\"\n";

const OFF_REFUSAL: &str =
    "direct's key is in the keychain, which is off (WISP_KEYCHAIN); run this with --keychain on";

#[test]
fn under_keychain_off_store_keyring_is_refused() {
    let home = Home::new("off-store-keyring");
    let output = home.endpoint_run(
        &[
            "endpoint",
            "add",
            "direct",
            "--key-stdin",
            "--no-check",
            "--store",
            "keyring",
        ],
        &format!("{KEY}\n"),
    );
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert!(
        err(&output).contains(
            "the keychain is off (WISP_KEYCHAIN), so --store keyring can't be used; run this with --keychain on"
        ),
        "{}",
        err(&output)
    );
    assert_clean(&output, &[KEY]);
    assert!(!home.file(CONFIG).exists());
    assert!(!home.file(CREDENTIALS).exists());
    let file_config = "store_id = \"0123456789abcdef\"\n\n[endpoints.f]\nauth = \"api_key\"\nkey_store = \"file\"\n";
    home.write(CONFIG, file_config);
    let output = home.endpoint_run(&["endpoint", "edit", "f", "--store", "keyring"], "");
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert!(
        err(&output).contains("so --store keyring can't be used; run this with --keychain on"),
        "{}",
        err(&output)
    );
    assert_eq!(home.read(CONFIG), file_config);
}

#[test]
fn under_keychain_off_writes_that_touch_a_keychain_item_are_refused() {
    let home = Home::new("off-refusals");
    home.write(CONFIG, KEYCHAIN_CONFIG);
    let cases: [&[&str]; 7] = [
        &["endpoint", "remove", "direct", "--yes"],
        &[
            "endpoint",
            "edit",
            "direct",
            "--store",
            "file",
            "--no-check",
        ],
        &["endpoint", "edit", "direct", "--name", "main"],
        &["endpoint", "edit", "direct", "--key-stdin", "--no-check"],
        &[
            "endpoint",
            "edit",
            "direct",
            "--url",
            "https://gw.example",
            "--no-check",
        ],
        &["endpoint", "edit", "direct", "--no-url", "--no-check"],
        &[
            "endpoint",
            "edit",
            "direct",
            "--provider",
            "anthropic",
            "--no-check",
        ],
    ];
    for args in cases {
        let output = home.endpoint_run(args, &format!("{KEY}\n"));
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", both(&output));
        assert!(
            err(&output).contains(OFF_REFUSAL),
            "{args:?}: {}",
            err(&output)
        );
        assert_clean(&output, &[KEY]);
        assert_eq!(home.read(CONFIG), KEYCHAIN_CONFIG, "{args:?}");
        assert!(!home.file(CREDENTIALS).exists(), "{args:?}");
    }
    let output = home.endpoint_run(&["endpoint", "edit", "direct", "--pricing", "none"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert!(home.read(CONFIG).contains("pricing = \"none\""));
}

#[test]
fn remove_keep_key_leaves_the_item_and_names_its_account() {
    let home = Home::new("keep-key");
    home.write(CONFIG, KEYCHAIN_CONFIG);
    let output = home.endpoint_run(&["endpoint", "remove", "direct", "--keep-key", "--yes"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(
        out(&output),
        "Removed \"direct\". Its key is still in the keychain (item 0123456789abcdef/direct); run wisp account remove direct when the keychain answers to delete it.\n"
    );
    assert!(!home.read(CONFIG).contains("[endpoints.direct]"));

    home.write(
        CONFIG,
        "[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n",
    );
    let output = home.endpoint_run(&["endpoint", "remove", "work", "--keep-key", "--yes"], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).contains(
            "--keep-key is for keys in the keychain; work's key is in a variable, which remove never touches"
        ),
        "{}",
        err(&output)
    );
    assert!(home.read(CONFIG).contains("[endpoints.work]"));
}

#[test]
fn the_keychain_setting_comes_from_the_flag_the_variable_or_config() {
    let home = Home::new("keychain-setting");
    let output = home.endpoint_run(&["config", "set", "keychain", "off"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert!(
        home.read(CONFIG).contains("keychain = \"off\""),
        "{}",
        home.read(CONFIG)
    );
    let output = home
        .endpoint_command(&["doctor"])
        .env_remove("WISP_KEYCHAIN")
        .output()
        .unwrap();
    assert!(
        out(&output).contains("keychain           off (config.toml)"),
        "{}",
        out(&output)
    );
    let output = home
        .endpoint_command(&["--keychain", "off", "doctor"])
        .env("WISP_KEYCHAIN", "on")
        .output()
        .unwrap();
    assert!(
        out(&output).contains("keychain           off (--keychain)"),
        "{}",
        out(&output)
    );
    let output = home.endpoint_run(&["doctor"], "");
    assert!(
        out(&output).contains("keychain           off (WISP_KEYCHAIN)"),
        "{}",
        out(&output)
    );
    let output = home
        .endpoint_command(&["doctor"])
        .env("WISP_KEYCHAIN", "CANARYKEYCHAIN")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).contains("WISP_KEYCHAIN must be auto, on or off (what was given isn't shown)"),
        "{}",
        err(&output)
    );
    assert_clean(&output, &[]);
    let output = home.endpoint_run(&["--keychain", "CANARYMODE", "doctor"], "");
    assert_eq!(output.status.code(), Some(2));
    assert_clean(&output, &[]);
}

#[cfg(target_os = "linux")]
fn dead_keychain(command: &mut Command, home: &Home) {
    command
        .env("WISP_KEYCHAIN", "on")
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", home.file("no-bus").display()),
        )
        .env("XDG_RUNTIME_DIR", home.file("no-runtime"));
}

#[cfg(target_os = "linux")]
fn run_dead(home: &Home, args: &[&str], stdin: &str) -> Output {
    let mut command = home.endpoint_command(args);
    dead_keychain(&mut command, home);
    run(&mut command, stdin)
}

#[cfg(target_os = "linux")]
#[test]
fn a_keychain_that_fails_a_write_under_auto_without_a_terminal_saves_nothing() {
    let home = Home::new("dead-keychain-add");
    let output = run_dead(
        &home,
        &["endpoint", "add", "direct", "--key-stdin", "--no-check"],
        &format!("{KEY}\n"),
    );
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert!(
        err(&output).contains(
            "the keychain isn't running, so nothing was saved; unlock it and try again, or pass --store file to keep the key in ~/.config/wisp/credentials.json"
        ),
        "{}",
        err(&output)
    );
    assert_clean(&output, &[KEY]);
    assert!(!home.file(CONFIG).exists());
    assert!(!home.file(CREDENTIALS).exists());
    let output = run_dead(
        &home,
        &[
            "--config",
            "given.toml",
            "endpoint",
            "add",
            "direct",
            "--key-stdin",
            "--no-check",
        ],
        &format!("{KEY}\n"),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).contains(
            "--store file to keep the key in credentials.json beside the file given with --config"
        ),
        "{}",
        err(&output)
    );
    let output = run_dead(
        &home,
        &[
            "endpoint",
            "add",
            "direct",
            "--key-stdin",
            "--no-check",
            "--store",
            "file",
        ],
        &format!("{KEY}\n"),
    );
    assert!(output.status.success(), "{}", both(&output));
    assert!(home.read(CREDENTIALS).contains(KEY));
}

#[cfg(target_os = "linux")]
#[test]
fn a_keychain_endpoint_whose_keychain_is_down_is_unavailable_never_read_from_the_file() {
    let home = Home::new("dead-keychain-read");
    home.write(CONFIG, KEYCHAIN_CONFIG);
    home.write(
        CREDENTIALS,
        &format!(
            "{{\"version\":1,\"stores\":{{\"0123456789abcdef\":{{\"direct\":{{\"key\":\"{KEY}\"}}}}}}}}"
        ),
    );
    let output = run_dead(&home, &["doctor"], "");
    let text = out(&output);
    assert!(text.contains("unavailable"), "{text}");
    assert!(
        text.contains("keychain           Secret Service (keychain = on, from WISP_KEYCHAIN)"),
        "{text}"
    );
    assert_clean(&output, &[KEY]);
    let output = run_dead(&home, &["endpoint", "remove", "direct", "--yes"], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(err(&output).contains("--keep-key"), "{}", err(&output));
    assert!(err(&output).contains("isn't running"), "{}", err(&output));
    assert!(home.read(CONFIG).contains("[endpoints.direct]"));
    let output = run_dead(
        &home,
        &["endpoint", "remove", "direct", "--keep-key", "--yes"],
        "",
    );
    assert!(output.status.success(), "{}", both(&output));
    assert!(!home.read(CONFIG).contains("[endpoints.direct]"));
}

#[test]
fn claude_logout_removes_setup_token_endpoints_even_as_the_start_or_a_presets_account() {
    for blocker in ["none", "default", "preset"] {
        let home = Home::new(&format!("claude-logout-{blocker}"));
        let added = home.endpoint_run(
            &[
                "endpoint",
                "add",
                "plan",
                "--provider",
                "anthropic",
                "--auth",
                "anthropic_subscription",
                "--store",
                "file",
                "--key-stdin",
                "--no-check",
            ],
            "synthetic-setup-token-canary\n",
        );
        assert!(added.status.success(), "{}", both(&added));
        if blocker == "default" {
            let key = home.endpoint_run(
                &[
                    "endpoint",
                    "add",
                    "work-api",
                    "--provider",
                    "anthropic",
                    "--auth",
                    "api_key",
                    "--store",
                    "file",
                    "--key-stdin",
                    "--no-check",
                ],
                &format!("{SHORT_KEY}\n"),
            );
            assert!(key.status.success(), "{}", both(&key));
            assert!(
                home.endpoint_run(&["endpoint", "default", "plan"], "")
                    .status
                    .success()
            );
        } else if blocker == "preset" {
            home.write(
                CONFIG,
                &format!(
                    "{}\n[agents.presets.worker]\ndescription = \"worker\"\nendpoint = \"plan\"\n",
                    home.read(CONFIG)
                ),
            );
        }
        let removed = home.endpoint_run(&["logout", "plan"], "");
        assert!(!both(&removed).contains("synthetic-setup-token-canary"));
        assert!(removed.status.success(), "{}", both(&removed));
        assert!(!home.read(CONFIG).contains("[endpoints.plan]"));
        assert!(
            !home
                .read(CREDENTIALS)
                .contains("synthetic-setup-token-canary")
        );
        let next = if blocker == "default" {
            "New chats now start on work-api\u{a0}\u{b7}\u{a0}API."
        } else {
            "Run wisp login to sign in again."
        };
        assert_eq!(
            out(&removed),
            format!("Signed out \u{b7} Claude subscription \u{b7} plan\n{next}\n")
        );
        if blocker == "default" {
            assert!(home.read(CONFIG).contains("default = \"work-api\""));
            let asked = home.endpoint_run(&["logout", "work-api"], "");
            assert_eq!(asked.status.code(), Some(1), "{}", both(&asked));
            assert_eq!(
                err(&asked),
                "wisp: signing out of work-api deletes its key; add --yes to confirm.\n"
            );
            assert!(home.read(CREDENTIALS).contains(SHORT_KEY));
            let said = home.endpoint_run(&["logout"], "");
            assert_eq!(err(&said), "wisp: say which: work-api.\n");
            let gone = home.endpoint_run(&["logout", "work-api", "--yes"], "");
            assert!(gone.status.success(), "{}", both(&gone));
            assert!(!home.read(CREDENTIALS).contains(SHORT_KEY));
            assert_clean(&gone, &[SHORT_KEY]);
        }
        if blocker == "none" {
            home.write(KEYS_FILE, &format!("ANTHROPIC_API_KEY={SHORT_KEY}\n"));
            home.write(
                CONFIG,
                "[endpoints.claude-api]\nauth = \"api_key\"\nkey_env = \"ANTHROPIC_API_KEY\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n[endpoints.shell]\nauth = \"api_key\"\nkey_env = \"SHELL_KEY\"\n",
            );
            let shell = home
                .endpoint_command(&["logout", "shell"])
                .env("SHELL_KEY", "CANARYSHELL")
                .output()
                .unwrap();
            assert!(shell.status.success(), "{}", both(&shell));
            assert!(!home.read(CONFIG).contains("[endpoints.shell]"));
            let line = "wisp: claude-api's key comes from ~/.config/wisp/.env: remove ANTHROPIC_API_KEY there to sign out of claude-api.\n";
            let kept = home.endpoint_run(&["logout", "claude-api"], "");
            assert_eq!(kept.status.code(), Some(1), "{}", both(&kept));
            assert_eq!(err(&kept), line);
            assert!(home.read(CONFIG).contains("[endpoints.claude-api]"));
            let table = home.endpoint_run(&["logout", "work"], "");
            assert!(table.status.success(), "{}", both(&table));
            assert!(
                out(&table).starts_with("Signed out \u{b7} "),
                "{}",
                both(&table)
            );
            assert!(!home.read(CONFIG).contains("[endpoints.work]"));
            let all = home.endpoint_run(&["logout", "all", "--yes"], "");
            assert_eq!(all.status.code(), Some(1), "{}", both(&all));
            assert_eq!(err(&all), line);
            assert!(home.read(CONFIG).contains("[endpoints.claude-api]"));
            assert_clean(&all, &[SHORT_KEY]);
        }
    }
}
