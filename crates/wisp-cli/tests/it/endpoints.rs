#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket or process error"
)]

use crate::support;

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use support::server::{Server, json, text_reply};
use support::{
    WISP,
    home::{CONFIG, CREDENTIALS, Home, KEYS_FILE, both, command_in, run},
};

const MODEL: &str = "claude-sonnet-5-5";
const DIRECT_KEY: &str = "sk-ant-CANARYDIRECTKEY01";
const GW_TOKEN: &str = "CANARYGATEWAYTOKEN02";
const BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";

fn serve() -> Server {
    Server::replying(text_reply("msg_1", MODEL, "REPLY-DONE", (1, 5)))
}

impl Home {
    fn command(&self) -> Command {
        self.command_on(MODEL)
    }

    fn command_on(&self, model: &str) -> Command {
        let mut command = command_in(&self.0, WISP);
        command.args(["--time", "off", "--model", model, "--output", "jsonl"]);
        command
    }
}

fn chat(command: &mut Command) -> Output {
    run(command, "hello\n")
}

fn assert_no_canary(where_: &str, text: &str) {
    for canary in [DIRECT_KEY, GW_TOKEN, STORE_ID, "CANARYPATH", "CANARYQUERY"] {
        assert!(!text.contains(canary), "{canary} in {where_}: {text}");
    }
}

fn config(direct: &Server, default: Option<&str>) -> String {
    let mut text = String::new();
    if let Some(default) = default {
        text.push_str(&format!("default = \"{default}\"\n"));
    }
    text.push_str(&format!(
        "[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"WISP_DIRECT_KEY\"\nurl = \"http://127.0.0.1:{}\"\n\n[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"MANIFEST_API_KEY\"\nurl_env = \"MANIFEST_BASE_URL\"\n",
        direct.port
    ));
    text
}

fn env_text(gw: &Server) -> String {
    format!(
        "WISP_DIRECT_KEY={DIRECT_KEY}\nMANIFEST_API_KEY={GW_TOKEN}\nMANIFEST_BASE_URL=http://127.0.0.1:{}/CANARYPATH?CANARYQUERY\nWISP_ENDPOINT=direct\n",
        gw.port
    )
}

#[test]
fn without_a_config_the_environment_endpoint_is_used() {
    let server = serve();
    let home = Home::configured("no-config");
    let env_file = home.write("empty.env", "");
    let output = chat(
        home.command()
            .arg("--env-file")
            .arg(&env_file)
            .env("ANTHROPIC_API_KEY", "test-key")
            .env(
                "ANTHROPIC_BASE_URL",
                format!("http://127.0.0.1:{}", server.port),
            ),
    );
    assert!(output.status.success(), "{}", both(&output));
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{}", both(&output));
    assert!(
        seen[0]
            .head
            .to_ascii_lowercase()
            .contains("x-api-key: test-key")
    );
    assert!(seen[0].body.contains("block_binding"), "{}", seen[0].body);
    assert!(seen[0].head.contains(BINDING_BETA), "{}", seen[0].head);
    let logs = home.logs();
    assert!(logs.contains("\"endpoint_name\":\"gateway\""), "{logs}");
}

#[test]
fn each_named_endpoint_reaches_its_own_server_with_its_own_header() {
    for (name, flag) in [("direct", true), ("manifest", true), ("default", false)] {
        let gw = serve();
        let direct = serve();
        let home = Home::configured(&format!("named-{name}"));
        home.write(CONFIG, &config(&direct, Some("manifest")));
        home.write(KEYS_FILE, &env_text(&gw));
        let mut command = home.command();
        if flag {
            command.args(["--endpoint", name]);
        }
        let output = chat(&mut command);
        let all = both(&output);
        let (hit, missed, header, endpoint) = match name {
            "direct" => (&direct, &gw, format!("x-api-key: {DIRECT_KEY}"), "direct"),
            _ => (
                &gw,
                &direct,
                format!("authorization: Bearer {GW_TOKEN}"),
                "manifest",
            ),
        };
        assert!(output.status.success(), "{name}: {all}");
        let seen = hit.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{name}: {all}");
        assert!(
            seen[0]
                .head
                .to_ascii_lowercase()
                .contains(&header.to_ascii_lowercase()),
            "{name}: {}",
            seen[0].head
        );
        assert!(
            !seen[0].body.contains(endpoint),
            "{name}: endpoint name sent"
        );
        assert!(missed.seen.lock().unwrap().is_empty(), "{name}");
        assert!(all.contains("REPLY-DONE"), "{name}: {all}");
        assert_no_canary(name, &all);
        let logs = home.logs();
        assert!(
            logs.contains(&format!("\"endpoint_name\":\"{endpoint}\"")),
            "{name}: {logs}"
        );
        assert_no_canary(&format!("{name} log"), &logs);
    }
}

#[test]
fn an_exported_key_wins_over_the_default() {
    let gw = serve();
    let direct = serve();
    let exported = serve();
    let home = Home::configured("exported");
    home.write(CONFIG, &config(&direct, Some("manifest")));
    home.write(KEYS_FILE, &env_text(&gw));
    let output = chat(home.command().env("ANTHROPIC_API_KEY", "exported-key").env(
        "ANTHROPIC_BASE_URL",
        format!("http://127.0.0.1:{}", exported.port),
    ));
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    assert_eq!(exported.seen.lock().unwrap().len(), 1, "{all}");
    assert!(gw.seen.lock().unwrap().is_empty(), "{all}");
    assert!(direct.seen.lock().unwrap().is_empty(), "{all}");
    let logs = home.logs();
    assert!(logs.contains("\"endpoint_name\":\"gateway\""), "{logs}");
}

#[test]
fn bad_configs_and_choices_stop_before_any_request() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("bad");
    home.write(KEYS_FILE, &env_text(&gw));
    let missing = home.0.join("missing.toml");
    let cases: Vec<(Vec<String>, Option<&str>, &str)> = vec![
        (
            vec!["--config".into(), missing.display().to_string()],
            None,
            "the file given with --config doesn't exist",
        ),
        (
            vec![],
            Some("[endpoints.m]\nauth = \"CANARYPATH\"\nkey_env = \"K\"\n"),
            "line 2: auth must be \"api_key\", \"bearer\" or \"anthropic_subscription\"",
        ),
        (
            vec![],
            Some("a = \"CANARYQUERY\" = 1\n"),
            "line 1 is not valid TOML; nothing was loaded",
        ),
        (
            vec!["--endpoint".into(), "nope".into()],
            Some(""),
            "no account \"nope\"",
        ),
    ];
    for (args, config, want) in cases {
        if let Some(config) = config {
            home.write(CONFIG, config);
        }
        let output = chat(home.command().args(&args));
        let all = both(&output);
        assert!(!output.status.success(), "{args:?}: {all}");
        assert!(all.contains(want), "{args:?}: {all}");
        assert!(!all.contains("wisp login"), "{args:?}: {all}");
        assert_no_canary("error", &all);
    }
    assert!(gw.seen.lock().unwrap().is_empty());
    assert!(direct.seen.lock().unwrap().is_empty());
}

#[test]
fn wisp_endpoint_is_read_from_the_environment_only() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("variable");
    home.write(CONFIG, &config(&direct, Some("manifest")));
    home.write(KEYS_FILE, &env_text(&gw));
    let output = chat(&mut home.command());
    assert_eq!(gw.seen.lock().unwrap().len(), 1, "{}", both(&output));
    assert!(direct.seen.lock().unwrap().is_empty());
    let output = chat(home.command().env("WISP_ENDPOINT", "direct"));
    assert_eq!(direct.seen.lock().unwrap().len(), 1, "{}", both(&output));
}

#[test]
fn a_config_in_the_current_directory_is_never_read() {
    let server = serve();
    let home = Home::configured("cwd");
    let work = home.0.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join("config.toml"),
        "default = \"evil\"\n[endpoints.evil]\nauth = \"api_key\"\nkey_env = \"ANTHROPIC_API_KEY\"\nurl = \"http://127.0.0.1:9\"\n",
    )
    .unwrap();
    let output = chat(
        home.command()
            .current_dir(&work)
            .env("ANTHROPIC_API_KEY", "test-key")
            .env(
                "ANTHROPIC_BASE_URL",
                format!("http://127.0.0.1:{}", server.port),
            ),
    );
    assert_eq!(server.seen.lock().unwrap().len(), 1, "{}", both(&output));
}

#[test]
fn doctor_maps_each_variable_to_its_host_without_secrets() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("doctor");
    home.write(CONFIG, &config(&direct, Some("manifest")));
    let env = home.write(KEYS_FILE, &env_text(&gw));
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = home
        .command()
        .arg("doctor")
        .env("ANTHROPIC_AUTH_TOKEN", GW_TOKEN)
        .env(
            "ANTHROPIC_BASE_URL",
            format!("http://127.0.0.1:{}/CANARYPATH?CANARYQUERY", gw.port),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    let section = all.split("\naccounts\n").nth(1).unwrap_or_default();
    let want = [
        "  config             ~/.config/wisp/config.toml".to_owned(),
        "  keys file          ~/.config/wisp/.env".to_owned(),
        "  would start on     manifest (ANTHROPIC_AUTH_TOKEN from the environment)".to_owned(),
        format!(
            "  direct             Gateway · WISP_DIRECT_KEY → 127.0.0.1:{} · from Wisp's keys file (~/.config/wisp/.env) · list prices",
            direct.port
        ),
        "  gateway            same as manifest".to_owned(),
        format!(
            "  manifest           Gateway · MANIFEST_API_KEY → 127.0.0.1:{} (url from MANIFEST_BASE_URL) · from Wisp's keys file (~/.config/wisp/.env) · list prices",
            gw.port
        ),
        "  warning            ~/.config/wisp/.env is readable by other users (chmod 600)"
            .to_owned(),
    ];
    for line in &want {
        assert!(
            section.lines().any(|l| l == line),
            "missing {line:?} in\n{section}"
        );
    }
    assert_no_canary("doctor", &all);
    assert!(gw.seen.lock().unwrap().is_empty());
    assert!(direct.seen.lock().unwrap().is_empty());

    home.write(CONFIG, "default = \"CANARYQUERY\"\n");
    let output = home
        .command()
        .arg("doctor")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let all = both(&output);
    assert!(!output.status.success(), "{all}");
    assert!(
        all.contains("line 1: default names no account in this file"),
        "{all}"
    );
    assert!(
        all.contains("\ntools\n") || all.contains("terminal"),
        "{all}"
    );
    assert_no_canary("broken doctor", &all);
}

#[test]
fn doctor_warns_on_plain_http_and_credentials_in_a_url() {
    let home = Home::configured("doctor-warn");
    home.write(
        CONFIG,
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\nurl = \"http://user:CANARYPATH@gw.example.com/x?CANARYQUERY\"\n",
    );
    let output = home
        .command()
        .arg("doctor")
        .env("GW", GW_TOKEN)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let all = both(&output);
    assert!(
        all.contains("gw uses plain http to gw.example.com, so the key is sent unencrypted"),
        "{all}"
    );
    assert!(
        all.contains("gw: the url holds user info or a query; put credentials in ~/.config/wisp/.env and use url_env"),
        "{all}"
    );
    assert!(
        all.contains("would start on     gw (the only available account)"),
        "{all}"
    );
    assert_no_canary("doctor warnings", &all);

    home.write(
        CONFIG,
        "store_id = \"5f0c2a9e41d7b388\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\nurl = \"http://user:CANARYPATH@gw.example.com/x?CANARYQUERY\"\n",
    );
    home.write(
        CREDENTIALS,
        &format!("{{\"version\": 1, \"stores\": {{\"5f0c2a9e41d7b388\": {{\"gw\": {{\"key\": \"{GW_TOKEN}\"}}}}}}}}"),
    );
    let output = home
        .command()
        .arg("doctor")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let all = both(&output);
    assert!(
        all.contains("gw: the url holds user info or a query; store the URL with the key instead: run wisp account edit gw and change the URL"),
        "{all}"
    );
    assert!(!all.contains("put credentials in the env file"), "{all}");
    assert_no_canary("stored doctor warnings", &all);
}

fn doctor_output(home: &Home, envs: &[(&str, &str)]) -> Output {
    let mut command = home.command();
    command.arg("doctor").stdin(Stdio::null());
    for (k, v) in envs {
        command.env(k, v);
    }
    command.output().unwrap()
}

#[test]
fn doctor_fails_on_a_malformed_env_file() {
    let home = Home::configured("doctor-bad-env");
    home.write(KEYS_FILE, "GOOD=1\nnot a line CANARYPATH\n");
    let output = doctor_output(&home, &[]);
    let all = both(&output);
    assert!(!output.status.success(), "{all}");
    assert!(all.contains("line 2 is not KEY=VALUE"), "{all}");
    assert_no_canary("doctor env", &all);
}

#[test]
fn doctor_flags_a_group_readable_file_and_an_unavailable_default() {
    let home = Home::configured("doctor-group");
    let config = home.write(
        CONFIG,
        "default = \"work\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n",
    );
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).unwrap();
    let output = doctor_output(&home, &[]);
    let all = both(&output);
    assert!(
        all.contains(
            "warning            ~/.config/wisp/config.toml is readable by other users (chmod 600)"
        ),
        "{all}"
    );
    assert!(
        all.contains("warning            the default account \"work\" is unavailable"),
        "{all}"
    );
    assert!(
        all.contains("  work               unavailable: WORK_KEY is not set"),
        "{all}"
    );
}

#[test]
fn an_empty_wisp_endpoint_is_ignored() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("empty-variable");
    home.write(CONFIG, &config(&direct, Some("manifest")));
    home.write(KEYS_FILE, &env_text(&gw));
    let output = chat(home.command().env("WISP_ENDPOINT", "  "));
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(gw.seen.lock().unwrap().len(), 1, "{}", both(&output));
    assert!(direct.seen.lock().unwrap().is_empty());
}

#[test]
fn a_loose_env_file_gets_no_warning_without_a_config() {
    let server = serve();
    let home = Home::configured("loose-no-config");
    let env = home.write(
        KEYS_FILE,
        &format!(
            "ANTHROPIC_API_KEY=test-key\nANTHROPIC_BASE_URL=http://127.0.0.1:{}\n",
            server.port
        ),
    );
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = chat(&mut home.command());
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    assert!(!all.contains("readable by other users"), "{all}");
    assert_eq!(server.seen.lock().unwrap().len(), 1, "{all}");
}

fn check_config(servers: &[(&str, &Server)]) -> String {
    servers
        .iter()
        .map(|(name, server)| {
            format!(
                "[endpoints.{name}]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:{}\"\n",
                server.port
            )
        })
        .collect()
}

#[test]
fn doctor_without_check_contacts_nothing() {
    let server = serve();
    let home = Home::configured("no-check");
    home.write(CONFIG, &check_config(&[("gw", &server)]));
    let output = doctor_output(&home, &[("GW_TOKEN", GW_TOKEN)]);
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    assert!(!all.contains("account check"), "{all}");
    assert!(server.seen.lock().unwrap().is_empty(), "{all}");
    let refused = home
        .command()
        .args(["doctor", "gw"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(server.seen.lock().unwrap().is_empty());
}

#[test]
fn doctor_check_maps_each_answer_without_echoing_it() {
    let ok = Server::replying(json("200 OK", r#"{"input_tokens": 3}"#));
    let refused = Server::replying(json("401 Unauthorized", r#"{"error":"CANARYPATH"}"#));
    let chunk = Server::replying(json(
        "200 OK",
        &format!("data: {{\"error\":{{\"message\":\"{GW_TOKEN} CANARYQUERY\"}}}}\n\n"),
    ));
    let home = Home::configured("check");
    home.write(
        CONFIG,
        &format!(
            "{}[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n\n[agents.presets.coder]\ndescription = \"Codes\"\nendpoint = \"good\"\n",
            check_config(&[("good", &ok), ("refused", &refused), ("chunky", &chunk)])
        ),
    );
    let mut command = home.command();
    command
        .args(["doctor", "--check"])
        .env("GW_TOKEN", GW_TOKEN)
        .stdin(Stdio::null());
    let output = command.output().unwrap();
    let all = both(&output);
    assert!(!output.status.success(), "{all}");
    let section = all.split("\naccount check\n").nth(1).unwrap_or_default();
    for line in [
        format!("  request            count_tokens with {MODEL}"),
        "  cost               free on Anthropic's side; a gateway may count it".to_owned(),
        "  chunky             unexpected answer (HTTP 200): the body reports an error".to_owned(),
        "  good               ok".to_owned(),
        "  refused            key refused (401)".to_owned(),
        "  work               not checked: WORK_KEY is not set in the environment".to_owned(),
    ] {
        assert!(
            section.lines().any(|l| l == line),
            "missing {line:?} in\n{section}"
        );
    }
    assert!(all.contains("Codes \u{b7} account good"), "{all}");
    for line in all.lines() {
        let shown = ["--endpoint", "endpoints.", "endpoint = "]
            .iter()
            .fold(line.to_owned(), |l, form| l.replace(form, ""));
        let slips = wisp_render::chrome::account::vocabulary_slips(&shown);
        assert!(!slips.iter().any(|s| s.starts_with("endpoint")), "{line}");
    }
    assert_no_canary("check", &all);
    for server in [&ok, &refused, &chunk] {
        let seen = server.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{all}");
        assert!(
            seen[0].head.starts_with("POST /v1/messages/count_tokens "),
            "{}",
            seen[0].head
        );
        assert!(!seen[0].head.to_ascii_lowercase().contains("anthropic-beta"));
    }

    let only = home
        .command()
        .args(["doctor", "--check", "good", "Bad Name", "missing"])
        .env("GW_TOKEN", GW_TOKEN)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let all = both(&only);
    assert!(!only.status.success(), "{all}");
    assert!(all.contains("  good               ok"), "{all}");
    assert!(
        all.contains("  (invalid name)     not an account name"),
        "{all}"
    );
    assert!(
        all.contains("  missing            no such account"),
        "{all}"
    );
    assert!(!all.contains("Bad Name"), "{all}");
    assert_eq!(ok.seen.lock().unwrap().len(), 2);
    assert_eq!(refused.seen.lock().unwrap().len(), 1);

    let unknown = home
        .command()
        .args(["doctor", "--check", "good", "missing"])
        .env("GW_TOKEN", GW_TOKEN)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let all = both(&unknown);
    assert!(!unknown.status.success(), "{all}");
    assert!(all.contains("  good               ok"), "{all}");
    assert!(
        all.contains("  missing            no such account"),
        "{all}"
    );

    let good_only = home
        .command()
        .args(["doctor", "--check", "good"])
        .env("GW_TOKEN", GW_TOKEN)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(good_only.status.success(), "{}", both(&good_only));
}

fn check_run(home: &Home, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let mut command = home.command();
    command
        .args(["doctor", "--check"])
        .args(args)
        .stdin(Stdio::null());
    for (k, v) in envs {
        command.env(k, v);
    }
    let output = command.output().unwrap();
    let all = both(&output);
    assert_no_canary("check", &all);
    (output.status.success(), all)
}

#[test]
fn a_hidden_duplicate_and_an_unset_endpoint_pass_unless_named() {
    let ok = Server::replying(json("200 OK", r#"{"input_tokens": 3}"#));
    let home = Home::configured("check-twin");
    home.write(
        CONFIG,
        &format!(
            "{}[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n",
            check_config(&[("ok", &ok)])
        ),
    );
    let url = format!("http://127.0.0.1:{}", ok.port);
    let envs = [
        ("GW_TOKEN", GW_TOKEN),
        ("ANTHROPIC_AUTH_TOKEN", GW_TOKEN),
        ("ANTHROPIC_BASE_URL", url.as_str()),
    ];
    let (passed, all) = check_run(&home, &[], &envs);
    assert!(passed, "{all}");
    assert!(all.contains("  gateway            same as ok\n"), "{all}");
    assert!(all.contains("  ok                 ok\n"), "{all}");
    assert!(
        all.contains("  work               not checked: WORK_KEY is not set"),
        "{all}"
    );
    assert_eq!(ok.seen.lock().unwrap().len(), 1);

    let (passed, all) = check_run(&home, &["env"], &envs);
    assert!(passed, "{all}");
    assert!(
        all.contains("  env                same as ok: ok\n"),
        "{all}"
    );
    assert_eq!(ok.seen.lock().unwrap().len(), 2);

    let (passed, all) = check_run(&home, &["work"], &envs);
    assert!(!passed, "{all}");
    assert!(
        all.contains("  work               not checked: WORK_KEY is not set"),
        "{all}"
    );
    assert_eq!(ok.seen.lock().unwrap().len(), 2);
}

#[test]
fn a_single_refused_key_fails_the_check() {
    let refused = Server::replying(json("401 Unauthorized", "{}"));
    let home = Home::configured("check-refused");
    home.write(CONFIG, &check_config(&[("refused", &refused)]));
    let (passed, all) = check_run(&home, &["refused"], &[("GW_TOKEN", GW_TOKEN)]);
    assert!(!passed, "{all}");
    assert!(
        all.contains("  refused            key refused (401)"),
        "{all}"
    );
}

#[test]
fn a_gateway_without_count_tokens_is_inconclusive_not_a_failure() {
    let good = Server::replying(json("200 OK", r#"{"input_tokens": 3}"#));
    let gateway = Server::replying(json("404 Not Found", "{}"));
    let home = Home::configured("check-not-offered");
    home.write(
        CONFIG,
        &check_config(&[("good", &good), ("gateway", &gateway)]),
    );
    let (passed, all) = check_run(&home, &[], &[("GW_TOKEN", GW_TOKEN)]);
    assert!(passed, "{all}");
    assert!(all.contains("  good               ok"), "{all}");
    assert!(
        all.contains(
            "  gateway            count_tokens not offered here (404): couldn't check this account"
        ),
        "{all}"
    );
    assert_eq!(gateway.seen.lock().unwrap().len(), 1);
}

#[test]
fn an_unreachable_endpoint_fails_the_check() {
    let held = tokio::net::TcpSocket::new_v4().unwrap();
    held.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let closed = Server {
        port: held.local_addr().unwrap().port(),
        seen: Arc::new(Mutex::new(Vec::new())),
        named: Arc::default(),
    };
    let home = Home::configured("check-unreachable");
    home.write(CONFIG, &check_config(&[("closed", &closed)]));
    let (passed, all) = check_run(&home, &[], &[("GW_TOKEN", GW_TOKEN)]);
    assert!(!passed, "{all}");
    assert!(
        all.contains("  closed             unreachable: could not connect"),
        "{all}"
    );
}

#[test]
fn a_broken_config_skips_the_check() {
    let server = serve();
    let home = Home::configured("check-broken");
    home.write(CONFIG, "default = \"CANARYPATH\"\n");
    let (passed, all) = check_run(&home, &[], &[]);
    assert!(!passed, "{all}");
    assert!(
        all.contains("\naccount check\n  skipped: the config or ~/.config/wisp/.env has errors\n"),
        "{all}"
    );
    assert!(server.seen.lock().unwrap().is_empty());
}

fn records(logs: &str, kind: &str) -> Vec<serde_json::Value> {
    logs.lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["type"] == kind)
        .collect()
}

#[test]
fn two_configured_endpoints_send_binding_controls_and_log_the_current_version() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("binding-two");
    home.write(CONFIG, &config(&direct, None));
    home.write(KEYS_FILE, &env_text(&gw));
    let output = chat(home.command().args(["--endpoint", "direct"]));
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    let seen = direct.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{all}");
    let body: serde_json::Value = serde_json::from_str(&seen[0].body).unwrap();
    assert_eq!(
        body["thinking"]["block_binding"]["prefix_mismatch_behavior"], "drop_block",
        "{}",
        seen[0].body
    );
    assert!(seen[0].head.contains(BINDING_BETA), "{}", seen[0].head);
    assert!(gw.seen.lock().unwrap().is_empty());
    let logs = home.logs();
    assert_no_canary("log", &logs);
    for line in logs.lines() {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            record["schema_version"],
            wisp_protocol::log::LOG_SCHEMA_VERSION,
            "{line}"
        );
    }
    let started = records(&logs, "turn_started");
    assert_eq!(started.len(), 1, "{logs}");
    assert_eq!(started[0]["endpoint_name"], "direct", "{logs}");
}

const STORE_ID: &str = "5f0c2a9e41d7b388";

fn stored_files(home: &Home, direct: &Server, gw: &Server, default: &str) {
    home.write(
        CONFIG,
        &format!(
            "store_id = \"{STORE_ID}\"\ndefault = \"{default}\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"file\"\nurl = \"http://127.0.0.1:{}\"\n\n[endpoints.manifest]\nauth = \"bearer\"\nkey_store = \"file\"\n",
            direct.port
        ),
    );
    home.write(
        CREDENTIALS,
        &format!(
            "{{\n  \"version\": 1,\n  \"stores\": {{\n    \"{STORE_ID}\": {{\n      \"direct\": {{ \"key\": \"{DIRECT_KEY}\" }},\n      \"manifest\": {{ \"key\": \"{GW_TOKEN}\", \"url\": \"http://127.0.0.1:{}/CANARYPATH?CANARYQUERY\" }}\n    }}\n  }}\n}}\n",
            gw.port
        ),
    );
}

#[test]
fn a_hand_made_credentials_file_endpoint_works() {
    for (default, key_header) in [
        ("direct", format!("x-api-key: {DIRECT_KEY}")),
        ("manifest", format!("authorization: bearer {GW_TOKEN}")),
    ] {
        let gw = serve();
        let direct = serve();
        let home = Home::configured(&format!("stored-{default}"));
        stored_files(&home, &direct, &gw, default);
        let output = chat(&mut home.command());
        let all = both(&output);
        assert!(output.status.success(), "{all}");
        let server = if default == "direct" { &direct } else { &gw };
        let seen = server.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{all}");
        assert!(
            seen[0]
                .head
                .to_ascii_lowercase()
                .contains(&key_header.to_ascii_lowercase()),
            "{}",
            seen[0].head
        );
        if default == "manifest" {
            assert!(seen[0].head.contains("/CANARYPATH"), "{}", seen[0].head);
        }
        assert_no_canary("output", &all);
        let logs = home.logs();
        assert_no_canary("log", &logs);
        let started = records(&logs, "session_started");
        assert_eq!(started.len(), 1, "{logs}");
        assert_eq!(
            started[0]["credential_source"],
            serde_json::json!({"mode": {"billing": "metered", "source": "credential_file"}}),
            "{logs}"
        );
        assert_eq!(
            started[0]["schema_version"],
            wisp_protocol::log::LOG_SCHEMA_VERSION,
            "{logs}"
        );
        assert_eq!(started[0]["endpoint_name"], default, "{logs}");
    }
}

#[test]
fn an_exported_key_still_wins_over_a_stored_default() {
    let gw = serve();
    let direct = serve();
    let env_server = serve();
    let home = Home::configured("stored-exported");
    stored_files(&home, &direct, &gw, "direct");
    let output = chat(home.command().env("ANTHROPIC_API_KEY", "test-key").env(
        "ANTHROPIC_BASE_URL",
        format!("http://127.0.0.1:{}", env_server.port),
    ));
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    assert_eq!(env_server.seen.lock().unwrap().len(), 1, "{all}");
    assert!(direct.seen.lock().unwrap().is_empty());
    assert_no_canary("output", &all);
}

#[test]
fn a_missing_record_stops_the_start_with_its_reason() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("stored-missing");
    stored_files(&home, &direct, &gw, "direct");
    home.write(CREDENTIALS, "{\"version\": 1, \"stores\": {}}\n");
    let output = chat(&mut home.command());
    let all = both(&output);
    assert!(!output.status.success(), "{all}");
    assert!(
        all.contains(
            "the default account \"direct\" is unavailable: no key for direct in ~/.config/wisp/credentials.json. If this config.toml was copied from another folder"
        ),
        "{all}"
    );
    assert!(!all.contains(&home.0.display().to_string()), "{all}");
    assert!(direct.seen.lock().unwrap().is_empty());
}

#[test]
fn a_loose_credentials_file_is_warned_about_at_start() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("stored-loose");
    stored_files(&home, &direct, &gw, "direct");
    let credentials = home.0.join(CREDENTIALS);
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = chat(&mut home.command());
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "wisp: ~/.config/wisp/credentials.json is readable by other users (chmod 600)"
        ),
        "{all}"
    );
    std::fs::rename(home.0.join(CONFIG), home.0.join("CANARYCONF.toml")).unwrap();
    std::fs::rename(&credentials, home.0.join("credentials.json")).unwrap();
    let output = chat(home.command().args(["--config", "CANARYCONF.toml"]));
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    assert!(
        all.contains(
            "wisp: credentials.json beside the file given with --config is readable by other users (chmod 600)"
        ),
        "{all}"
    );
    assert!(!all.contains("CANARYCONF"), "{all}");
}

#[test]
fn doctor_names_stored_endpoints_by_their_store() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("stored-doctor");
    stored_files(&home, &direct, &gw, "direct");
    let output = doctor_output(&home, &[("ANTHROPIC_API_KEY", "sk-ant-CANARYENVKEY")]);
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    let section = all.split("\naccounts\n").nth(1).unwrap_or_default();
    for line in [
        "  credentials        ~/.config/wisp/credentials.json (2 keys)".to_owned(),
        format!(
            "  direct             Gateway · API key in credentials.json → 127.0.0.1:{} · from ~/.config/wisp/credentials.json · list prices",
            direct.port
        ),
        format!(
            "  manifest           Gateway · auth token and URL in credentials.json → 127.0.0.1:{} · from ~/.config/wisp/credentials.json · list prices",
            gw.port
        ),
    ] {
        assert!(
            section.lines().any(|l| l == line),
            "missing {line:?} in\n{section}"
        );
    }
    for row in section
        .lines()
        .filter(|l| l.starts_with("  direct ") || l.starts_with("  manifest "))
    {
        assert!(!row.contains("ANTHROPIC_"), "{row}");
    }
    assert_no_canary("doctor", &all);
    assert!(!all.contains("CANARYENVKEY"));
    assert!(gw.seen.lock().unwrap().is_empty());
    assert!(direct.seen.lock().unwrap().is_empty());
}

#[test]
fn model_tables_in_the_config_load_and_set_the_window() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("model-tables");
    let tables = format!(
        "{}\n[models.\"{MODEL}\"]\ncontext_window = 300000\n\n[endpoints.direct.models.\"{MODEL}\"]\ncontext_window = 250000\n",
        config(&direct, Some("direct"))
    );
    home.write(CONFIG, &tables);
    home.write(KEYS_FILE, &env_text(&gw));
    let output = chat(home.command().args(["--endpoint", "direct"]));
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    assert!(all.contains("\"type\":\"context_changed\""), "{all}");
    assert!(all.contains("\"window\":250000"), "{all}");
    assert!(home.logs().contains("\"window\":250000"), "{all}");
    let output = chat(home.command().args(["--endpoint", "manifest"]));
    let all = both(&output);
    assert!(output.status.success(), "{all}");
    assert!(all.contains("\"window\":300000"), "{all}");
}

#[test]
fn a_model_window_below_the_floor_stops_before_any_request() {
    let gw = serve();
    let direct = serve();
    let home = Home::configured("model-floor");
    let tables = format!(
        "{}\n[models.\"{MODEL}\"]\ncontext_window = 100000\n",
        config(&direct, Some("direct"))
    );
    home.write(CONFIG, &tables);
    home.write(KEYS_FILE, &env_text(&gw));
    let output = chat(&mut home.command());
    let all = both(&output);
    assert!(!output.status.success(), "{all}");
    assert!(
        all.contains("config.toml line 12: context_window 100000 for claude-sonnet-5-5 puts the compaction point at 36,000"),
        "{all}"
    );
    assert!(direct.seen.lock().unwrap().is_empty());
    assert!(gw.seen.lock().unwrap().is_empty());
}

#[test]
fn status_and_doctor_name_the_keys_file_account_and_a_saved_one_that_holds_its_name() {
    const KEYS_FILE_KEY: &str = "sk-ant-CANARYKEYSFILE0001";
    let home = Home::configured("keys-file-name");
    home.write(KEYS_FILE, &format!("ANTHROPIC_API_KEY={KEYS_FILE_KEY}\n"));
    let run = |command: &str| {
        let output = home
            .command()
            .arg(command)
            .env("SAVED_KEY", "sk-ant-CANARYSAVED0002")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let all = both(&output);
        assert!(!all.contains("CANARY"), "{command}: {all}");
        all.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let only = run("status");
    assert!(only.contains("claude-api"), "{only}");
    assert_eq!(
        wisp_render::chrome::account::vocabulary_slips(&only),
        Vec::<&str>::new(),
        "{only}"
    );
    home.write(
        CONFIG,
        "[endpoints.claude-api]\nauth = \"api_key\"\nkey_env = \"SAVED_KEY\"\n",
    );
    let line = "claude-api is a saved account, so the key in Wisp's keys file (~/.config/wisp/.env) isn't used. Rename one to use both.";
    for command in ["status", "doctor"] {
        let shown = run(command);
        assert!(shown.contains(line), "{command}: {shown}");
    }
    home.write(
        CONFIG,
        "default = \"env\"\n[endpoints.claude-api]\nauth = \"api_key\"\nkey_env = \"SAVED_KEY\"\n",
    );
    let doctor = run("doctor");
    assert!(
        doctor.contains("settings endpoint = the keys-file account "),
        "{doctor}"
    );
    assert!(!doctor.contains("endpoint = claude-api"), "{doctor}");
    assert!(doctor.contains(&format!("unavailable: {line}")), "{doctor}");
}

#[test]
fn a_typed_model_starts_by_its_catalog_id_on_an_account_that_offers_it() {
    let direct = serve();
    let home = Home::configured("start-model");
    let config = |start: &str| {
        home.write(
            CONFIG,
            &format!(
                "{start}[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"WISP_DIRECT_KEY\"\nurl = \"http://127.0.0.1:{}\"\n[endpoints.oa]\nprovider = \"openai\"\nauth = \"api_key\"\nkey_env = \"WISP_OA_KEY\"\nurl = \"http://127.0.0.1:1\"\n",
                direct.port
            ),
        )
    };
    config("default = \"oa\"\n");
    let run = |model: &str, account: &[&str]| {
        chat(
            home.command_on(model)
                .args(account)
                .env("WISP_DIRECT_KEY", DIRECT_KEY)
                .env("WISP_OA_KEY", GW_TOKEN),
        )
    };
    let moved = run("opus-5-5", &[]);
    assert!(moved.status.success(), "{}", both(&moved));
    for model in ["opus-5-5", "gw-own-model"] {
        let named = run(model, &["--account", "direct"]);
        assert!(named.status.success(), "{}", both(&named));
    }
    let none = run("opus-55", &[]);
    assert_eq!(none.status.code(), Some(1), "{}", both(&none));
    let sent: Vec<String> = direct
        .bodies()
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .filter_map(|body| body["model"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(sent, ["claude-opus-5-5", "claude-opus-5-5", "gw-own-model"]);
    let resumed = run("opus-5-5", &["--resume"]);
    assert!(
        !both(&resumed).contains("doesn't offer"),
        "{}",
        both(&resumed)
    );
    config(
        "default = \"work\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WISP_WORK_KEY\"\n",
    );
    let gone = home
        .command_on("gpt-6.1-sol")
        .env("WISP_DIRECT_KEY", DIRECT_KEY)
        .env("WISP_OA_KEY", GW_TOKEN)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        both(&gone).contains("can't be used now")
            && both(&gone).contains("; this chat starts on oa.\n"),
        "{}",
        both(&gone)
    );
}

#[test]
fn an_off_subscriptions_start_and_resume_name_it_without_offering_a_sign_in() {
    use wisp_render::chrome::account::SaidWords;
    let model = wisp_core::provider::gemini::default_model();
    let ag = Server::start(move |request| {
        if request.head.contains("fetchAvailableModels") {
            json(
                "200 OK",
                &serde_json::json!({"models": {(model): {}}}).to_string(),
            )
        } else {
            json(
                "200 OK",
                &super::gemini::sse_chunk(
                    model,
                    vec![serde_json::json!({"text": "Initial."})],
                    Some("STOP"),
                ),
            )
            .replacen("application/json", "text/event-stream", 1)
        }
    });
    let other = serve();
    let home = Home::configured("provider-off-start-and-resume");
    let configured = |default: &str, off: bool, another: bool| {
        let extra = if another {
            format!(
                "[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"WISP_DIRECT_KEY\"\nurl = \"http://127.0.0.1:{}\"\n",
                other.port
            )
        } else {
            String::new()
        };
        home.write(CONFIG, &format!("store_id = \"0123456789abcdef\"\ndefault = \"{default}\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\nurl = \"http://127.0.0.1:{}\"\n{extra}[plugins.gemini]\nenabled = {}\n", ag.port, !off));
    };
    configured("antigravity", false, true);
    let (profile, _) = home.antigravity(Some("CANARY_PROJECT"));
    let before = wisp_core::oauth::status(
        &profile,
        wisp_core::provider::credential::SubscriptionKind::Antigravity,
    )
    .unwrap();
    let initial = chat(home.command_on(model).env("WISP_DIRECT_KEY", DIRECT_KEY));
    assert!(initial.status.success(), "{}", both(&initial));
    configured("direct", true, true);
    let resumed = chat(
        home.command()
            .arg("--resume")
            .env("WISP_DIRECT_KEY", DIRECT_KEY),
    );
    assert!(!resumed.status.success(), "{}", both(&resumed));
    assert!(
        both(&resumed).contains("antigravity, which can't be used now (gemini is off)"),
        "{}",
        both(&resumed)
    );
    configured("antigravity", true, true);
    let started = chat(home.command().env("WISP_DIRECT_KEY", DIRECT_KEY));
    assert!(!started.status.success(), "{}", both(&started));
    assert!(
        both(&started)
            .contains("antigravity, where new chats start, can't be used now (gemini is off)"),
        "{}",
        both(&started)
    );
    let keychain = wisp_start::keychain::Keychain::off();
    let path = home.file(CONFIG);
    let sources = wisp_start::endpoints::Sources::load(
        &wisp_start::endpoints::Flags {
            env_file: None,
            config: Some(&path),
            endpoint: None,
            keychain: &keychain,
            set: &[],
        },
        None,
        None,
    );
    let (left, ..) = wisp_start::resume::start_left(&sources, true).unwrap();
    assert_eq!(
        left.lead(&|_| None).plain(),
        "antigravity can't be used now (gemini is off)."
    );
    configured("g1", true, true);
    home.write(CONFIG, &(home.read(CONFIG) + &format!("\n[endpoints.g1]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"GEMINI_UNUSED\"\nurl = \"http://127.0.0.1:{}\"\n", ag.port)));
    let automatic = chat(
        home.command()
            .env("WISP_DIRECT_KEY", DIRECT_KEY)
            .env("GEMINI_UNUSED", "CANARY_GEMINI_OFF_START"),
    );
    assert!(automatic.status.success(), "{}", both(&automatic));
    assert!(both(&automatic).contains("g1, where new chats start, can't be used now (gemini is off); this chat starts on direct."), "{}", both(&automatic));
    configured("antigravity", true, false);
    for args in [vec![], vec!["--resume"], vec!["--account", "antigravity"]] {
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let shown = both(&output);
        assert!(!output.status.success(), "{shown}");
        assert!(
            shown.contains("account \"antigravity\" is unavailable: gemini is off"),
            "{shown}"
        );
        assert_eq!(
            shown.contains("wisp plugin enable gemini"),
            !args.contains(&"--account"),
            "{shown}"
        );
        assert!(!shown.contains("sign in"), "{shown}");
    }
    assert_eq!(
        wisp_core::oauth::status(
            &profile,
            wisp_core::provider::credential::SubscriptionKind::Antigravity
        )
        .unwrap(),
        before
    );
}

#[test]
fn a_provider_switch_flag_stops_the_keys_file_start_without_a_config() {
    let home = Home::configured("provider-off-implicit-start");
    let server = serve();
    let file = home.write(
        "synthetic-keys",
        &format!(
            "ANTHROPIC_API_KEY=sk-ant-CANARY_IMPLICIT\nANTHROPIC_BASE_URL=http://127.0.0.1:{}\n",
            server.port
        ),
    );
    let provider = wisp_core::endpoints::ProviderKind::IMPLICIT.as_str();
    let switch = format!("plugins.{provider}.enabled=off");
    let output = home
        .command()
        .arg("--env-file")
        .arg(&file)
        .args(["--set", &switch])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let shown = both(&output);
    assert!(!output.status.success(), "{shown}");
    assert!(shown.contains(&format!("{provider} is off")), "{shown}");
    assert!(!shown.contains("sign in"), "{shown}");
    assert!(server.bodies().is_empty());
    let config = home.write(
        CONFIG,
        &format!("default = \"env\"\n[plugins.{provider}]\nenabled = false\n"),
    );
    let keychain = wisp_start::keychain::Keychain::off();
    let mut sources = wisp_start::endpoints::Sources::load(
        &wisp_start::endpoints::Flags {
            env_file: Some(&file),
            config: Some(&config),
            endpoint: None,
            keychain: &keychain,
            set: &[],
        },
        None,
        None,
    );
    sources.inputs = sources.inputs.with_env(&|_| None);
    sources.inputs.explicit_env_file = false;
    sources.choice = None;
    let (left, ..) = wisp_start::resume::start_left(&sources, true).unwrap();
    assert_eq!(left.state, Some(wisp_start::resume::ask::State::Off));
    assert!(wisp_start::resume::nothing_left(&sources).is_none());
}
