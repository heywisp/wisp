#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket or process error"
)]

use crate::runner;
use crate::support;

use runner::KEY;
use support::home::{CONFIG, CREDENTIALS, Home, KEYS_FILE, assert_clean, both, err, mode, out};
use support::server::{Server, counted};

const NEW_KEY: &str = "sk-ant-CANARYNEWKEYc3d4";
const KEY_SHAPED: &str = "canary0000000000000key1";
const URL_SHAPED: &str = "canary1111111111111url2";
const NOT_A_NAME: &str = "sk-ant-CANARYPASTED0001";

fn saw_key(server: &Server, key: &str) -> bool {
    server
        .heads()
        .iter()
        .any(|h| h.to_ascii_lowercase().contains(&key.to_ascii_lowercase()))
}

fn add_stored(home: &Home, name: &str, url: &str, key: &str) {
    let output = home.endpoint_run(
        &[
            "endpoint",
            "add",
            name,
            "--key-stdin",
            "--no-check",
            "--url",
            url,
            "--auth",
            "api_key",
        ],
        key,
    );
    assert!(output.status.success(), "{}", both(&output));
}

#[test]
fn add_reads_a_key_once_from_an_env_file_or_the_environment() {
    let server = Server::replying(counted());
    let home = Home::new("key-var");
    let url = server.url();
    home.write(
        "work.env",
        &format!("WORK_KEY={KEY}\nWORK_URL={url}\nBAD_URL=ftp://x\n"),
    );
    let output = home.endpoint_run(
        &[
            "--env-file",
            "work.env",
            "endpoint",
            "add",
            "work",
            "--key-var",
            "WORK_KEY",
            "--url-var",
            "WORK_URL",
            "--auth",
            "bearer",
        ],
        "",
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_clean(&output, &[KEY]);
    assert!(
        saw_key(&server, &format!("Bearer {KEY}")),
        "{:?}",
        server.heads()
    );
    assert!(home.read(CREDENTIALS).contains(KEY));
    assert!(home.read(CREDENTIALS).contains(&url));
    assert!(!home.read(CONFIG).contains(&url));

    let output = home.run_env(
        &[
            "endpoint",
            "add",
            "mine",
            "--from-environment",
            "--key-var",
            "MY_KEY",
            "--url-var",
            "MY_URL",
            "--auth",
            "api_key",
        ],
        "",
        &[("MY_KEY", NEW_KEY), ("MY_URL", &url)],
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_clean(&output, &[NEW_KEY]);
    assert!(saw_key(&server, &format!("x-api-key: {NEW_KEY}")));
    assert!(home.read(CREDENTIALS).contains(NEW_KEY));
    assert_eq!(server.heads().len(), 2);

    let before = home.read(CONFIG);
    for (args, code, message) in [
        (
            vec!["endpoint", "add", "x", "--key-var", "WORK_KEY"],
            2,
            "wisp: --key-var and --url-var need --env-file FILE or --from-environment, to say where to read them\n",
        ),
        (
            vec![
                "--env-file",
                "work.env",
                "endpoint",
                "add",
                "x",
                "--from-environment",
                "--key-var",
                "WORK_KEY",
            ],
            2,
            "wisp: --env-file and --from-environment are two sources; give one\n",
        ),
        (
            vec![
                "--env-file",
                "work.env",
                "endpoint",
                "add",
                "x",
                "--key-var",
                NOT_A_NAME,
            ],
            2,
            "wisp: --key-var needs a variable name (letters, digits and _); what was given isn't shown, in case it is a key\n",
        ),
        (
            vec![
                "--env-file",
                "work.env",
                "endpoint",
                "add",
                "x",
                "--key-var",
                KEY_SHAPED,
            ],
            1,
            "wisp: the variable given to --key-var isn't set in work.env; nothing was saved\n",
        ),
        (
            vec![
                "--env-file",
                "work.env",
                "endpoint",
                "add",
                "x",
                "--key-var",
                "WORK_KEY",
                "--url-var",
                "BAD_URL",
                "--auth",
                "bearer",
            ],
            1,
            "wisp: the variable given to --url-var isn't an http or https URL; nothing was saved\n",
        ),
        (
            vec![
                "endpoint",
                "add",
                "x",
                "--from-environment",
                "--key-var",
                "WORK_KEY",
            ],
            1,
            "wisp: the variable given to --key-var isn't set in the environment; nothing was saved\n",
        ),
        (
            vec![
                "--env-file",
                "missing.env",
                "endpoint",
                "add",
                "x",
                "--key-var",
                "WORK_KEY",
            ],
            1,
            "wisp: the file given with --env-file doesn't exist\n",
        ),
    ] {
        let output = home.endpoint_run(&args, "");
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            both(&output)
        );
        assert_eq!(err(&output), message, "{args:?}");
        assert_clean(&output, &[KEY, NOT_A_NAME, KEY_SHAPED]);
    }
    assert_eq!(home.read(CONFIG), before);
    assert_eq!(server.heads().len(), 2, "no refusal reached the server");
}

#[test]
fn add_with_key_env_records_names_only() {
    let server = Server::replying(counted());
    let home = Home::new("key-env");
    let output = home.run_env(
        &[
            "endpoint",
            "add",
            "gw",
            "--auth",
            "bearer",
            "--key-env",
            "GW_KEY",
            "--url-env",
            "GW_URL",
        ],
        "",
        &[("GW_KEY", KEY), ("GW_URL", &server.url())],
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(
        out(&output),
        "Saved \"gw\". Its key is read from the variable GW_KEY at each start, and its URL from GW_URL; nothing was read now. wisp doctor --check gw checks it.\n"
    );
    assert_eq!(
        home.read(CONFIG),
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n"
    );
    assert!(!home.file(CREDENTIALS).exists());
    assert!(server.heads().is_empty());
    let output = home.run_env(
        &["doctor"],
        "",
        &[("GW_KEY", KEY), ("GW_URL", &server.url())],
    );
    assert!(
        out(&output).contains("GW_KEY \u{2192}"),
        "{}",
        both(&output)
    );
    assert_clean(&output, &[KEY]);

    let before = home.read(CONFIG);
    for (args, message) in [
        (
            vec!["endpoint", "add", "x", "--key-env", "K", "--store", "file"],
            "wisp: --key-env names the variable that holds the key, so it can't be used with --store; one source per account\n",
        ),
        (
            vec!["endpoint", "add", "x", "--key-env", "K", "--key-stdin"],
            "wisp: --key-env names the variable that holds the key, so it can't be used with --key-stdin; one source per account\n",
        ),
        (
            vec!["endpoint", "add", "x", "--key-env", "K", "--key-var", "K"],
            "wisp: --key-env names the variable that holds the key, so it can't be used with --key-var; one source per account\n",
        ),
        (
            vec!["endpoint", "add", "x", "--key-env", "K", "--url-var", "U"],
            "wisp: --key-env names the variable that holds the key, so it can't be used with --url-var; one source per account\n",
        ),
        (
            vec![
                "endpoint",
                "add",
                "x",
                "--key-env",
                "K",
                "--url-env",
                "U",
                "--url",
                "https://gw.example.com",
            ],
            "wisp: --url can't be used with --url-env; one source per account\n",
        ),
        (
            vec![
                "endpoint",
                "add",
                "x",
                "--key-env",
                "K",
                "--from-environment",
            ],
            "wisp account add: --key-env names a variable read at each start; --from-environment is for a one-time import with --key-var\n",
        ),
        (
            vec!["endpoint", "add", "x", "--key-env", KEY_SHAPED],
            "wisp: --key-env needs a variable name such as WORK_API_KEY; what was given looks like a key, so it wasn't saved or shown\n",
        ),
        (
            vec![
                "endpoint",
                "add",
                "x",
                "--key-env",
                "K",
                "--url-env",
                KEY_SHAPED,
            ],
            "wisp: --url-env needs a variable name such as WORK_API_KEY; what was given looks like a key, so it wasn't saved or shown\n",
        ),
        (
            vec!["endpoint", "add", "x", "--key-env", NOT_A_NAME],
            "wisp: --key-env needs a variable name (letters, digits and _); what was given isn't shown, in case it is a key\n",
        ),
    ] {
        let output = home.endpoint_run(&args, "");
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", both(&output));
        assert_eq!(err(&output), message, "{args:?}");
        assert_clean(&output, &[KEY_SHAPED, NOT_A_NAME]);
    }
    assert_eq!(home.read(CONFIG), before);
}

#[test]
fn a_key_shaped_variable_name_written_by_hand_is_shown_nowhere() {
    let home = Home::new("key-shaped");
    home.write(
        CONFIG,
        &format!(
            "[endpoints.gw]\nauth = \"api_key\"\nkey_env = \"{KEY_SHAPED}\"\n[endpoints.work]\nauth = \"bearer\"\nkey_env = \"WORK_KEY\"\nurl_env = \"{URL_SHAPED}\"\n"
        ),
    );
    let output = home.endpoint_run(&["doctor"], "");
    assert_clean(&output, &[KEY_SHAPED, URL_SHAPED]);
    let text = out(&output);
    assert!(
        text.contains(
            "gw's key_env looks like a key, not a variable name; move the key out of config.toml"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "work's url_env looks like a key, not a variable name; move the key out of config.toml"
        ),
        "{text}"
    );
    assert!(!text.contains("work's key_env"), "{text}");
    let output = home.run_env(&["endpoint", "list"], "", &[(KEY_SHAPED, KEY)]);
    assert_clean(&output, &[KEY_SHAPED, KEY]);
    let output = home.run_env(&["doctor"], "", &[(KEY_SHAPED, KEY)]);
    assert_clean(&output, &[KEY_SHAPED, KEY]);
    assert!(
        out(&output).contains("(looks like a key, not shown)"),
        "{}",
        both(&output)
    );
    let output = home.endpoint_run(&["endpoint", "list"], "");
    assert_clean(&output, &[KEY_SHAPED]);
    assert!(
        out(&output).contains("(looks like a key, not shown) not set"),
        "{}",
        both(&output)
    );
}

#[test]
fn edit_replaces_a_key_keeping_its_url_and_changes_fields() {
    let server = Server::replying(counted());
    let home = Home::new("edit-stored");
    let url = server.url();
    add_stored(&home, "direct", &url, KEY);
    let output = home.endpoint_run(&["endpoint", "default", "direct"], "");
    assert!(output.status.success(), "{}", both(&output));
    let output = home.endpoint_run(&["endpoint", "edit", "direct", "--key-stdin"], NEW_KEY);
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(out(&output), "Updated \"direct\": a new key.\n");
    assert_clean(&output, &[KEY, NEW_KEY]);
    assert!(
        saw_key(&server, &format!("x-api-key: {NEW_KEY}")),
        "{:?}",
        server.heads()
    );
    let credentials = home.read(CREDENTIALS);
    assert!(credentials.contains(NEW_KEY) && !credentials.contains(KEY));
    assert!(credentials.contains(&url), "the record keeps its URL");

    let heads = server.heads().len();
    let output = home.endpoint_run(&["endpoint", "edit", "direct", "--pricing", "none"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(out(&output), "Updated \"direct\": pricing none.\n");
    assert_eq!(server.heads().len(), heads, "no check for pricing");
    assert!(home.read(CONFIG).contains("pricing = \"none\""));

    let output = home.endpoint_run(&["endpoint", "edit", "direct", "--name", "main"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(out(&output), "Updated \"main\": renamed from direct.\n");
    let config = home.read(CONFIG);
    assert!(config.contains("default = \"main\""), "{config}");
    assert!(
        config.contains("[endpoints.main]") && !config.contains("direct"),
        "{config}"
    );
    let credentials = home.read(CREDENTIALS);
    assert!(
        credentials.contains("\"main\"") && !credentials.contains("\"direct\""),
        "{credentials}"
    );

    let before = (home.read(CONFIG), home.read(CREDENTIALS));
    for (args, code, message) in [
        (
            vec!["endpoint", "edit", "main", "--key-env", "MAIN_KEY"],
            1,
            "wisp: main's key is stored; to read a variable instead, remove it and add it again with --key-env\n",
        ),
        (
            vec![
                "endpoint",
                "edit",
                "main",
                "--url",
                "https://a.example.com",
                "--no-url",
            ],
            2,
            "wisp: give one of --url, --url-var and --no-url; one source per account\n",
        ),
        (
            vec!["endpoint", "edit", "main", "--replace-key"],
            1,
            "wisp: --replace-key asks for the new key on a terminal; without one, use --key-stdin or --key-var\n",
        ),
        (
            vec!["endpoint", "edit", "main"],
            1,
            "wisp: nothing to change: give a flag such as --replace-key, --url or --pricing (see wisp account edit --help)\n",
        ),
        (
            vec!["endpoint", "edit", "nope", "--pricing", "list"],
            1,
            "wisp: there is no account \"nope\"; nothing was changed\n",
        ),
        (
            vec!["endpoint", "edit", "main", "--key", NOT_A_NAME],
            2,
            "wisp: A key on the command line is saved in your shell history and visible to other programs, so nothing was saved. Replace that key with your provider, then use --key-stdin or the prompt.\n",
        ),
        (
            vec!["endpoint", "edit", "main", "--name", NOT_A_NAME],
            2,
            &format!("wisp: {}\n", wisp_protocol::bad_account_name()),
        ),
    ] {
        let output = home.endpoint_run(&args, "");
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            both(&output)
        );
        assert_eq!(err(&output), message, "{args:?}");
        assert_clean(&output, &[NOT_A_NAME, NEW_KEY]);
    }
    assert_eq!((home.read(CONFIG), home.read(CREDENTIALS)), before);

    let output = home.endpoint_run(&["endpoint", "edit", "main", "--no-url", "--no-check"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert!(!home.read(CREDENTIALS).contains(&url));
    assert!(home.read(CREDENTIALS).contains(NEW_KEY));
}

#[test]
fn a_new_url_moves_out_of_config_toml_into_the_record() {
    let server = Server::replying(counted());
    let home = Home::new("edit-url");
    let url = server.url();
    home.write(
        CONFIG,
        "store_id = \"0123456789abcdef\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\nurl = \"https://old.example.com\"\n",
    );
    let output = home.endpoint_run(
        &["endpoint", "edit", "gw", "--key-stdin", "--url", &url],
        KEY,
    );
    assert!(output.status.success(), "{}", both(&output));
    assert!(saw_key(&server, &format!("Bearer {KEY}")));
    assert_eq!(
        home.read(CONFIG),
        "store_id = \"0123456789abcdef\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\n"
    );
    let credentials = home.read(CREDENTIALS);
    assert!(
        credentials.contains(&url) && credentials.contains(KEY),
        "{credentials}"
    );
}

#[test]
fn a_lost_gateway_url_is_settled_before_the_key_is_read() {
    let server = Server::replying(counted());
    let home = Home::new("lost-url");
    home.write(
        CONFIG,
        "store_id = \"0123456789abcdef\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\n",
    );
    let output = home.endpoint_run(&["endpoint", "edit", "gw", "--key-stdin"], KEY);
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert_eq!(
        err(&output),
        "wisp: gw's URL was in its lost record; give --url, --url-var or --no-url\n"
    );
    assert!(!home.file(CREDENTIALS).exists());
    let output = home.endpoint_run(&["endpoint", "edit", "gw", "--pricing", "none"], "");
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert!(
        err(&output).ends_with("or wisp account edit gw --replace-key to give it a new key.\n"),
        "{}",
        both(&output)
    );
    let output = home.endpoint_run(&["endpoint", "edit", "gw", "--name", "other"], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).ends_with("give it a new key with wisp account edit gw --replace-key\n"),
        "{}",
        both(&output)
    );
    let output = home.endpoint_run(
        &[
            "endpoint",
            "edit",
            "gw",
            "--key-stdin",
            "--url",
            &server.url(),
        ],
        KEY,
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(server.heads().len(), 1);
    assert!(saw_key(&server, &format!("Bearer {KEY}")));
    assert!(home.read(CREDENTIALS).contains(&server.url()));
}

#[test]
fn a_variable_table_edits_names_only_and_refuses_key_flags() {
    let home = Home::new("edit-table");
    home.write(
        CONFIG,
        "default = \"gw\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n",
    );
    let output = home.endpoint_run(
        &[
            "endpoint",
            "edit",
            "gw",
            "--key-env",
            "NEW_KEY",
            "--url",
            "https://gw.example.com",
            "--name",
            "work",
        ],
        "",
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(
        out(&output),
        "Updated \"work\": a new URL, its key from NEW_KEY, renamed from gw.\nNothing was read or checked now; wisp doctor --check work checks it.\n"
    );
    assert_eq!(
        home.read(CONFIG),
        "default = \"work\"\n[endpoints.work]\nauth = \"bearer\"\nkey_env = \"NEW_KEY\"\nurl = \"https://gw.example.com\"\n"
    );
    let output = home.endpoint_run(&["endpoint", "edit", "work", "--no-url"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert!(!home.read(CONFIG).contains("url"));
    let output = home.endpoint_run(&["endpoint", "edit", "work", "--url-env", "WORK_URL"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert!(home.read(CONFIG).ends_with("url_env = \"WORK_URL\"\n"));
    let before = home.read(CONFIG);
    for args in [
        vec!["endpoint", "edit", "work", "--key-stdin"],
        vec!["endpoint", "edit", "work", "--replace-key"],
        vec![
            "--env-file",
            "x.env",
            "endpoint",
            "edit",
            "work",
            "--from-environment",
            "--key-var",
            "K",
        ],
    ] {
        let output = home.endpoint_run(&args, KEY);
        let message = if args.contains(&"--env-file") {
            "wisp: --env-file and --from-environment are two sources; give one\n".to_owned()
        } else {
            "wisp: work names its key as a variable; store it first with wisp account import, or change the variable with --key-env\n".to_owned()
        };
        assert_eq!(err(&output), message, "{args:?}");
        assert!(!output.status.success());
    }
    let output = home.endpoint_run(&["endpoint", "edit", "work", "--key-env", KEY_SHAPED], "");
    assert_eq!(output.status.code(), Some(2));
    assert_clean(&output, &[KEY_SHAPED]);
    let output = home.endpoint_run(
        &[
            "endpoint",
            "edit",
            "work",
            "--url-env",
            "U",
            "--from-environment",
        ],
        "",
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        err(&output),
        "wisp account edit: --url-env names a variable read at each start; --from-environment is for a one-time import with --key-var\n"
    );
    assert_eq!(home.read(CONFIG), before);
    assert!(!home.file(CREDENTIALS).exists());
}

const COMMENTED: &str = "# my endpoints\n\n# the gateway\n[endpoints.gw]\nauth = \"bearer\" # a token\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n";

#[test]
fn import_stores_the_default_env_file_and_converts_tables() {
    let server = Server::replying(counted());
    let home = Home::new("import");
    let url = server.url();
    home.write(CONFIG, COMMENTED);
    let env = format!(
        "GW_KEY={KEY}\nGW_URL={url}/gw\nANTHROPIC_API_KEY={NEW_KEY}\nANTHROPIC_BASE_URL={url}\nOTHER=1\n"
    );
    home.write(KEYS_FILE, &env);
    let output = home.endpoint_run(&["endpoint", "import"], "");
    assert!(output.status.success(), "{}", both(&output));
    assert_clean(&output, &[KEY, NEW_KEY]);
    let text = out(&output);
    assert!(
        text.starts_with("Found:\n  gw (GW_KEY and GW_URL) in ~/.config/wisp/.env\n  ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL in ~/.config/wisp/.env\n"),
        "{text}"
    );
    assert!(
        text.contains("Imported \"gw\" (GW_KEY and GW_URL) into ~/.config/wisp/credentials.json"),
        "{text}"
    );
    assert!(
        text.contains("Imported \"anthropic\" (ANTHROPIC_API_KEY and ANTHROPIC_BASE_URL)"),
        "{text}"
    );
    assert!(
        text.contains("\"gw\" is now the default account."),
        "{text}"
    );
    assert!(
        text.ends_with("Wisp no longer needs these lines in ~/.config/wisp/.env: ANTHROPIC_API_KEY, ANTHROPIC_BASE_URL, GW_KEY and GW_URL\n"),
        "{text}"
    );
    assert_eq!(home.read(KEYS_FILE), env, "the env file is never edited");
    let config = home.read(CONFIG);
    assert!(
        config.contains("# my endpoints\n\n# the gateway\n[endpoints.gw]\nauth = \"bearer\" # a token\nkey_store = \"file\"\n"),
        "{config}"
    );
    assert!(
        config.contains("[endpoints.anthropic]\nauth = \"api_key\"\nkey_store = \"file\"\n"),
        "{config}"
    );
    assert!(saw_key(&server, &format!("Bearer {KEY}")));
    assert!(saw_key(&server, &format!("x-api-key: {NEW_KEY}")));
    let credentials = home.read(CREDENTIALS);
    assert!(credentials.contains(&format!("{url}/gw")), "{credentials}");
    assert_eq!(mode(&home.file(CREDENTIALS)), 0o600);
    let output = home.endpoint_run(&["endpoint", "list"], "");
    assert_clean(&output, &[KEY, NEW_KEY]);
    assert!(!out(&output).contains("same as"), "{}", both(&output));
    let output = home.endpoint_run(&["doctor"], "");
    assert_clean(&output, &[KEY, NEW_KEY]);
    assert!(
        out(&output).contains("same as anthropic"),
        "{}",
        both(&output)
    );
}

#[test]
fn import_reads_only_the_chosen_source_and_skips_what_would_not_resolve() {
    let home = Home::new("import-skip");
    home.write(CONFIG, COMMENTED);
    home.write("only-key.env", &format!("GW_KEY={KEY}\n"));
    let before = home.read(CONFIG);
    let output = home.run_env(
        &[
            "--env-file",
            "only-key.env",
            "endpoint",
            "import",
            "--no-check",
        ],
        "",
        &[("GW_KEY", KEY), ("GW_URL", "https://gw.example.com")],
    );
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    assert_eq!(
        out(&output),
        "Skipped gw: GW_URL is not set in only-key.env, where GW_KEY was found; try --from-environment\nNothing was imported.\n"
    );
    assert_clean(&output, &[KEY]);
    assert_eq!(home.read(CONFIG), before);
    assert!(!home.file(CREDENTIALS).exists());

    home.write("token.env", &format!("ANTHROPIC_AUTH_TOKEN={KEY}\n"));
    let output = home.run_env(
        &[
            "--env-file",
            "token.env",
            "endpoint",
            "import",
            "--no-check",
        ],
        "",
        &[("ANTHROPIC_BASE_URL", "https://gw.example.com")],
    );
    assert_eq!(output.status.code(), Some(1), "{}", both(&output));
    let warning = "ANTHROPIC_BASE_URL is set in the environment, not in token.env; this token will be checked against the provider's own URL";
    assert!(
        err(&output).contains(&format!("warning: {warning}")),
        "{}",
        both(&output)
    );
    assert!(
        out(&output).contains(&format!(
            "Skipped ANTHROPIC_AUTH_TOKEN in token.env: {warning}"
        )),
        "{}",
        both(&output)
    );
    assert_eq!(home.read(CONFIG), before);
    let output = home.run_env(
        &[
            "--env-file",
            "token.env",
            "endpoint",
            "import",
            "--no-check",
            "--no-url",
        ],
        "",
        &[("ANTHROPIC_BASE_URL", "https://gw.example.com")],
    );
    assert!(output.status.success(), "{}", both(&output));
    let config = home.read(CONFIG);
    assert!(
        config.contains("[endpoints.anthropic]\nauth = \"bearer\"\nkey_store = \"file\"\n"),
        "{config}"
    );
    assert!(!home.read(CREDENTIALS).contains("gw.example.com"));
    assert_eq!(
        home.read("token.env"),
        format!("ANTHROPIC_AUTH_TOKEN={KEY}\n")
    );
    assert!(
        out(&output)
            .ends_with("Wisp no longer needs these lines in token.env: ANTHROPIC_AUTH_TOKEN\n"),
        "{}",
        both(&output)
    );
}

#[test]
fn import_from_the_environment_reads_it_once() {
    let server = Server::replying(counted());
    let home = Home::new("import-env");
    let output = home.run_env(
        &["endpoint", "import", "--from-environment"],
        "",
        &[
            ("ANTHROPIC_API_KEY", KEY),
            ("ANTHROPIC_BASE_URL", &server.url()),
        ],
    );
    assert!(output.status.success(), "{}", both(&output));
    assert_clean(&output, &[KEY]);
    assert!(saw_key(&server, &format!("x-api-key: {KEY}")));
    assert!(home.read(CONFIG).contains("default = \"anthropic\""));
    assert!(home.read(CREDENTIALS).contains(KEY));
    let output = home.run_env(
        &["endpoint", "import", "--from-environment", "--no-check"],
        "",
        &[],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        out(&output),
        "Nothing to import: no provider key and no key the config names is set in the environment.\n"
    );
    let output = home.endpoint_run(&["endpoint", "import"], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        err(&output),
        "wisp: there is no ~/.config/wisp/.env to import from; give --env-file FILE or --from-environment\n"
    );
    let output = home.endpoint_run(
        &[
            "--env-file",
            "x.env",
            "endpoint",
            "import",
            "--from-environment",
        ],
        "",
    );
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_copy_that_cannot_detach_is_refused_before_the_key_is_read() {
    let server = Server::replying(counted());
    let home = Home::new("no-detach");
    let config = "store_id = \"0123456789abcdef\"\n[endpoints.one]\nauth = \"bearer\"\nkey_store = \"file\"\n[endpoints.two]\nauth = \"bearer\"\nkey_store = \"file\"\n";
    home.write(CONFIG, config);
    home.write("key.env", &format!("ONE_KEY={KEY}\n"));
    for (args, stdin) in [
        (
            vec![
                "endpoint",
                "edit",
                "one",
                "--key-stdin",
                "--url",
                &server.url(),
            ],
            KEY,
        ),
        (
            vec![
                "--env-file",
                "key.env",
                "endpoint",
                "edit",
                "one",
                "--key-var",
                "ONE_KEY",
                "--url-var",
                "MISSING_URL",
            ],
            "",
        ),
    ] {
        let output = home.endpoint_run(&args, stdin);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", both(&output));
        assert!(
            err(&output).starts_with("wisp: nothing was changed: this config.toml must get its own copy of its keys first"),
            "{args:?}: {}",
            both(&output)
        );
        assert_clean(&output, &[KEY]);
    }
    assert!(server.heads().is_empty(), "nothing was probed");
    assert_eq!(home.read(CONFIG), config);
    assert!(!home.file(CREDENTIALS).exists());
}

const ENV_TABLE: &str = "\n[endpoints.env]\nnative_web_search = false # mine\npricing = \"none\"\n";

#[test]
fn the_env_table_survives_every_endpoint_command() {
    let server = Server::replying(counted());
    let home = Home::new("env-table");
    let url = server.url();
    home.write(
        CONFIG,
        &format!("[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n{ENV_TABLE}"),
    );
    home.write(KEYS_FILE, &format!("GW_KEY={KEY}\nGW_URL={url}/gw\n"));
    let steps: [&[&str]; 4] = [
        &["endpoint", "edit", "gw", "--key-env", "GW_KEY"],
        &["endpoint", "edit", "gw", "--name", "work"],
        &["endpoint", "import"],
        &["endpoint", "remove", "work", "--yes"],
    ];
    for args in steps {
        let output = home.endpoint_run(args, "");
        assert!(output.status.success(), "{args:?}: {}", both(&output));
        assert_clean(&output, &[KEY]);
        let config = home.read(CONFIG);
        assert!(config.contains(ENV_TABLE), "{args:?}: {config}");
        assert!(!config.contains("[endpoints.env]\nauth"), "{config}");
    }
}

#[test]
fn config_set_turns_search_off_for_env_and_refuses_its_connection() {
    let home = Home::new("env-set");
    let output = home.endpoint_run(
        &["config", "set", "endpoints.env.native_web_search", "off"],
        "",
    );
    assert!(output.status.success(), "{}", both(&output));
    assert!(
        home.read(CONFIG)
            .contains("[endpoints.env]\nnative_web_search = false\n"),
        "{}",
        home.read(CONFIG)
    );
    let before = home.read(CONFIG);
    let output = home.endpoint_run(&["config", "set", "endpoints.env.key_env", "GW_KEY"], "");
    assert!(!output.status.success());
    assert!(
        err(&output).contains("the keys-file account in config.toml takes no connection fields: its connection comes from the ANTHROPIC_* variables; add a named account to configure one"),
        "{}",
        both(&output)
    );
    assert_eq!(home.read(CONFIG), before);
}

#[test]
fn doctor_says_the_env_table_is_unused_when_env_is_a_twin() {
    let home = Home::new("env-twin");
    home.write(
        CONFIG,
        &format!("[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"ANTHROPIC_AUTH_TOKEN\"\nurl_env = \"ANTHROPIC_BASE_URL\"\nnative_web_search = true\n{ENV_TABLE}"),
    );
    let output = home.run_env(
        &["doctor"],
        "",
        &[
            ("ANTHROPIC_AUTH_TOKEN", KEY),
            ("ANTHROPIC_BASE_URL", "https://gw.example.com"),
        ],
    );
    let text = both(&output);
    assert_clean(&output, &[KEY]);
    assert!(
        text.contains(
            "the keys-file account in config.toml is unused: the ANTHROPIC_* key is gw's, whose settings apply"
        ),
        "{text}"
    );
    assert!(
        text.contains("on (web_search: shipped; native_web_search: account)"),
        "{text}"
    );
}
