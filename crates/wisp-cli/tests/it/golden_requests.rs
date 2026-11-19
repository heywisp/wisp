#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket, file or process error"
)]

#[path = "golden_requests/resume.rs"]
mod resume;
use crate::support;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use support::prefix::{Rebuilt, assert_prefix_stable};
use support::pty::{
    PATIENCE, Pty, READY, Stdout, usable, wait_for_exit, wait_per_step, wait_until,
};
use support::server::{Server, is_child, is_count, json, sse_reply, tool_reply};
use support::{
    WISP,
    home::{CONFIG, Home, both, command_in},
};

const TZ: &str = "IST-5:30";
const API_KEY: &str = "golden-api-key-0001";
const GW_TOKEN: &str = "golden-gateway-token-0002";
const STORED_KEY: &str = "golden-stored-key-0003";
const LITERAL_KEY: &str = "golden-literal-key-0004";
const STORE_ID: &str = "5f0c2a9e41d7b388";
const MASK: &str = "<masked>";
const DROPPED: [&str; 4] = ["host", "content-length", "connection", "accept-encoding"];
const AUTH_HEADERS: [&str; 2] = ["x-api-key", "authorization"];
const MASKED_LINES: [&str; 4] = [
    "Working directory: ",
    "Platform: ",
    "Resources: ",
    "Shell for the bash tool: ",
];

impl Server {
    fn requests(&self) -> Vec<(String, String)> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| (s.head.clone(), s.body.clone()))
            .collect()
    }
}

fn reply() -> String {
    sse_reply(
        &serde_json::json!({"type": "text", "text": ""}),
        "GOLDEN-DONE",
    )
}

fn counted_tokens(body: &str) -> String {
    let answer = serde_json::json!({"input_tokens": 10 + body.len()}).to_string();
    json("200 OK", &answer)
}

type Answer = Arc<dyn Fn(&str) -> String + Send + Sync>;

fn serve() -> Server {
    serve_by(Arc::new(|_| reply()))
}

fn serve_by(answer: Answer) -> Server {
    Server::start(move |request| {
        if is_count(&request.head) {
            counted_tokens(&request.body)
        } else {
            answer(&request.body)
        }
    })
}

fn mask_after(text: &str, marker: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(marker) {
        let start = at + marker.len();
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let stop = [tail.find("\\n"), tail.find('"')]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or_else(|| panic!("{marker:?} runs to the end of the body"));
        out.push_str(MASK);
        rest = &tail[stop..];
    }
    out.push_str(rest);
    out
}

fn mask_stamp(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("Sent: ") {
        let start = at + "Sent: ".len();
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let line_end = tail.find("\\n").unwrap_or(tail.len());
        match tail[..line_end].rfind(" UTC") {
            Some(offset) => {
                out.push_str(MASK);
                rest = &tail[offset..];
            }
            None => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

fn mask_context_tokens(text: &str) -> String {
    let marker = "Context: ";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(marker) {
        let start = at + marker.len();
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let number = tail.len()
            - tail
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == ',' || c == '.')
                .len();
        let digits = number + usize::from(number > 0 && tail[number..].starts_with(['k', 'M']));
        if number > 0 && tail[digits..].starts_with(" of ") {
            out.push_str(MASK);
            rest = &tail[digits..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

fn normalise_body(body: &str) -> String {
    let mut text = mask_context_tokens(&mask_stamp(body));
    for marker in MASKED_LINES {
        text = mask_after(&text, marker);
    }
    text
}

fn normalise_head(head: &str) -> String {
    let mut lines = head.lines();
    let mut out = String::new();
    if let Some(first) = lines.next() {
        out.push_str(first.trim_end());
        out.push('\n');
    }
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if DROPPED.contains(&name.as_str()) {
            continue;
        }
        let value = value.trim();
        let value = if AUTH_HEADERS.contains(&name.as_str()) {
            match value.split_once(' ') {
                Some((scheme, _)) => format!("{scheme} {MASK}"),
                None => MASK.to_owned(),
            }
        } else {
            value.to_owned()
        };
        out.push_str(&format!("{name}: {value}\n"));
    }
    out
}

fn ordered(requests: &[(String, String)]) -> Vec<&(String, String)> {
    let (mut counts, mut ordered): (Vec<_>, Vec<_>) =
        requests.iter().partition(|(head, _)| is_count(head));
    counts.sort_by(|a, b| a.1.cmp(&b.1));
    ordered.extend(counts);
    ordered
}

fn normalise(requests: &[(String, String)]) -> String {
    ordered(requests)
        .into_iter()
        .enumerate()
        .map(|(i, (head, body))| {
            format!(
                "### request {}\n{}\n{}\n",
                i + 1,
                normalise_head(head),
                normalise_body(body)
            )
        })
        .collect()
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/requests")
}

fn check(name: &str, requests: &[(String, String)]) {
    assert!(
        !requests.is_empty(),
        "{name}: no request reached the server"
    );
    let text = normalise(requests);
    check_text(name, "http", &text);
}

pub(super) fn check_body(name: &str, body: &str) {
    check_text(name, "json", &normalise_body(body));
}

fn check_text(name: &str, extension: &str, text: &str) {
    for secret in [API_KEY, GW_TOKEN, STORED_KEY, LITERAL_KEY] {
        assert!(!text.contains(secret), "{name}: a key in the golden file");
    }

    let path = golden_dir().join(format!("{name}.{extension}"));
    if std::env::var_os("WISP_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let saved = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{name}: no golden file; run with WISP_BLESS=1"));
    assert!(
        saved == text,
        "{name}: the request changed; run with WISP_BLESS=1 if that is intended\n--- saved\n{saved}\n--- now\n{text}"
    );
}

impl Home {
    fn golden_command(&self, program: &str) -> Command {
        if let Some(repo) =
            wisp_core::instructions::repository(&self.0, &wisp_core::instructions::Disk)
        {
            panic!(
                "{} is inside a repository at {}, whose AGENTS.md would change the requests; point TMPDIR outside any repository",
                self.0.display(),
                repo.display()
            );
        }
        let mut command = command_in(&self.0, program);
        command.env("TMPDIR", self.dir("tmp")).env("TZ", TZ);
        command
    }
}

fn jsonl(
    name: &str,
    args: &[&str],
    setup: impl FnOnce(&Home, &Server, &mut Command),
) -> Vec<(String, String)> {
    jsonl_input(name, args, b"hello\n", setup)
}

fn drain(mut source: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = source.read_to_end(&mut bytes);
        bytes
    })
}

fn jsonl_input(
    name: &str,
    args: &[&str],
    input: &[u8],
    setup: impl FnOnce(&Home, &Server, &mut Command),
) -> Vec<(String, String)> {
    jsonl_chat(name, args, input, setup).1
}

fn jsonl_chat(
    name: &str,
    args: &[&str],
    input: &[u8],
    setup: impl FnOnce(&Home, &Server, &mut Command),
) -> (Home, Vec<(String, String)>) {
    let server = serve();
    let home = Home::new(name);
    let mut command = home.golden_command(WISP);
    command.args(["--output", "jsonl"]).args(args);
    setup(&home, &server, &mut command);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).unwrap();
    drop(stdin);
    wait_for_exit(&mut child);
    let status = child.wait().unwrap();
    let all = both(&Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    });
    assert!(status.success(), "{name}: {all}");
    assert!(all.contains("GOLDEN-DONE"), "{name}: {all}");
    (home, server.requests())
}

fn with_env_endpoint(_: &Home, server: &Server, command: &mut Command) {
    command
        .env("ANTHROPIC_API_KEY", API_KEY)
        .env("ANTHROPIC_BASE_URL", server.url());
}

fn default_requests() -> Vec<(String, String)> {
    jsonl("default", &[], with_env_endpoint)
}

#[test]
fn jsonl_with_no_flags_and_no_file() {
    let setup = |home: &Home, server: &Server, command: &mut Command| {
        let mut doctor = home.golden_command(WISP);
        doctor.env("WISP_JOB", "1").arg("doctor");
        with_env_endpoint(home, server, &mut doctor);
        let output = doctor.output().unwrap();
        assert!(output.status.success(), "{}", both(&output));
        assert!(both(&output).contains("→ 127.0.0.1:"), "{}", both(&output));
        command.env("WISP_JOB", "1");
        with_env_endpoint(home, server, command);
    };
    let (home, requests) = jsonl_chat("default", &[], b"hello\n", setup);
    check("jsonl_default", &requests);
    let files: Vec<_> = std::fs::read_dir(home.file(".local/share/wisp/spend"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert_eq!(text.lines().count(), 1);
    let record: serde_json::Value = serde_json::from_str(&text).unwrap();
    let logged = home.records();
    let started = logged
        .iter()
        .find(|record| record["type"] == "session_started")
        .unwrap();
    assert_eq!(record["account"], started["endpoint_name"]);
    assert_eq!(record["model"], wisp_protocol::settings::DEFAULT_MODEL);
    assert!(record["session"].as_str().is_some_and(|id| {
        home.file(&format!(".local/share/wisp/sessions/{id}.jsonl"))
            .exists()
    }));
    assert!(record["usage"]["input_tokens"].as_u64().is_some());
    let (unlogged, requests) = jsonl_chat("no-log-spend", &["--no-log"], b"hello\n", setup);
    check("jsonl_default", &requests);
    assert!(!unlogged.exists(".local/share/wisp/spend"));
}

#[test]
fn the_inert_testkit_sends_the_default_bytes() {
    let requests = jsonl("testkit-inert", &[], |home, server, command| {
        with_env_endpoint(home, server, command);
        command.env("WISP_TESTKIT", "inert");
    });
    assert_eq!(normalise(&requests), normalise(&default_requests()));
    if std::env::var_os("WISP_BLESS").is_none() {
        check("jsonl_default", &requests);
    }
}

#[test]
fn the_self_check_switch_sends_the_default_bytes() {
    let requests = jsonl("self-check", &["--self-check"], with_env_endpoint);
    assert_eq!(normalise(&requests), normalise(&default_requests()));
    if std::env::var_os("WISP_BLESS").is_none() {
        check("jsonl_default", &requests);
    }
}

#[test]
fn each_moved_default_given_explicitly_sends_the_same_bytes() {
    let baseline = normalise(&default_requests());
    for (flag, value) in [
        ("--model", "claude-opus-5-5"),
        ("--effort", "medium"),
        ("--cache-ttl", "1h"),
        ("--time", "local"),
    ] {
        let given = normalise(&jsonl(
            &format!("explicit{flag}"),
            &[flag, value],
            with_env_endpoint,
        ));
        assert!(
            given == baseline,
            "{flag} {value}:\n{given}\n---\n{baseline}"
        );
    }
}

#[test]
fn each_flag_given_a_value_other_than_its_default() {
    for (name, args) in [
        ("model_sonnet", &["--model", "claude-sonnet-5-5"][..]),
        ("effort_high", &["--effort", "high"][..]),
        ("effort_low", &["--effort", "low"][..]),
        ("cache_ttl_5m", &["--cache-ttl", "5m"][..]),
        ("time_utc", &["--time", "utc"][..]),
        ("time_off", &["--time", "off"][..]),
        ("debug_cache", &["--debug-cache"][..]),
        ("no_fallback", &["--no-fallback"][..]),
    ] {
        check(name, &jsonl(name, args, with_env_endpoint));
    }
}

#[test]
fn two_turns_in_jsonl_send_the_history() {
    let requests = jsonl_input("two-turns", &[], b"hello\nagain\n", with_env_endpoint);
    assert_eq!(requests.len(), 2, "two turns send two requests");
    check("jsonl_two_turns", &requests);
}

#[test]
fn two_turns_with_debug_cache_name_the_previous_message() {
    let requests = jsonl_input(
        "two-turns-debug-cache",
        &["--debug-cache"],
        b"hello\nagain\n",
        with_env_endpoint,
    );
    assert_eq!(requests.len(), 2, "two turns send two requests");
    check("debug_cache_two_turns", &requests);
}

#[test]
fn a_config_with_todays_keys() {
    let requests = jsonl("config-today", &[], |home, server, command| {
        let config = home.write(
            "scratch/config.toml",
            "default = \"manifest\"\n\n[endpoints.manifest]\nprovider = \"anthropic\"\nauth = \"bearer\"\nkey_env = \"MANIFEST_API_KEY\"\nurl_env = \"MANIFEST_BASE_URL\"\npricing = \"none\"\n",
        );
        command
            .arg("--config")
            .arg(config)
            .env("MANIFEST_API_KEY", GW_TOKEN)
            .env("MANIFEST_BASE_URL", server.url());
    });
    check("config_today", &requests);
}

fn stored_config(home: &Home, server: &Server) -> PathBuf {
    home.write(
        "scratch/credentials.json",
        &format!(
            "{{\"version\": 1, \"stores\": {{\"{STORE_ID}\": {{\"stored\": {{\"key\": \"{STORED_KEY}\", \"url\": \"{}\"}}}}}}}}\n",
            server.url()
        ),
    );
    home.write(
        "scratch/config.toml",
        &format!(
            "store_id = \"{STORE_ID}\"\ndefault = \"stored\"\n\n[endpoints.stored]\nauth = \"api_key\"\nkey_store = \"file\"\n\n[endpoints.literal]\nauth = \"bearer\"\nkey_env = \"LITERAL_KEY\"\nurl = \"{}\"\n",
            server.url()
        ),
    )
}

#[test]
fn a_config_with_a_stored_key_and_a_literal_url() {
    let stored = jsonl("config-stored", &[], |home, server, command| {
        let config = stored_config(home, server);
        command
            .arg("--config")
            .arg(config)
            .env("LITERAL_KEY", LITERAL_KEY);
    });
    check("config_stored_key", &stored);
    let literal = jsonl(
        "config-literal",
        &["--endpoint", "literal"],
        |home, server, command| {
            let config = stored_config(home, server);
            command
                .arg("--config")
                .arg(config)
                .env("LITERAL_KEY", LITERAL_KEY);
        },
    );
    check("config_literal_url", &literal);
}

fn full_screen(home: &Home, server: &Server) -> Pty {
    full_screen_with(home, server, &[], &[])
}

fn full_screen_with(
    home: &Home,
    server: &Server,
    args: &[&std::ffi::OsStr],
    envs: &[(&str, &str)],
) -> Pty {
    full_screen_in(home, &home.0, server, args, envs)
}

fn full_screen_in(
    home: &Home,
    dir: &Path,
    server: &Server,
    args: &[&std::ffi::OsStr],
    envs: &[(&str, &str)],
) -> Pty {
    full_screen_sized(home, dir, server, args, envs, 100)
}

fn assert_hosted(home: &Home, pty: &Pty) {
    pty.wait_for(READY);
    assert!(
        support::hosted::socket(home).is_some(),
        "the golden must run through a chat host"
    );
}

fn picking(home: &Home, server: &Server, args: &[&std::ffi::OsStr], envs: &[(&str, &str)]) -> Pty {
    full_screen_launch(home, &home.0, server, args, envs, 100)
}

fn full_screen_sized(
    home: &Home,
    dir: &Path,
    server: &Server,
    args: &[&std::ffi::OsStr],
    envs: &[(&str, &str)],
    cols: u16,
) -> Pty {
    let pty = full_screen_launch(home, dir, server, args, envs, cols);
    if args.contains(&std::ffi::OsStr::new("--no-log")) {
        pty.wait_for("This chat can't stay open without its window: the session log is off.");
    } else {
        assert_hosted(home, &pty);
    }
    pty
}

fn full_screen_launch(
    home: &Home,
    dir: &Path,
    server: &Server,
    args: &[&std::ffi::OsStr],
    envs: &[(&str, &str)],
    cols: u16,
) -> Pty {
    let mut command = home.golden_command("setsid");
    command
        .current_dir(dir)
        .args(["--ctty", "--wait", WISP])
        .env("TERM", "xterm-256color");
    if envs.is_empty() {
        command
            .env("ANTHROPIC_API_KEY", API_KEY)
            .env("ANTHROPIC_BASE_URL", server.url());
    }
    command.args(args).envs(envs.iter().copied());
    Pty::start(&mut command, 30, cols, Stdout::Tty)
}

#[test]
fn the_full_screen_with_its_default_tools() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("full-screen");
    let mut pty = full_screen(&home, &server);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"hello\r").unwrap();
    pty.wait_for(b"GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    check("tui_default_tools", &server.requests());
}

#[test]
fn the_full_screen_with_the_context_section_docked_sends_the_default_tools_golden() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("full-screen-context-section");
    let args: Vec<&std::ffi::OsStr> =
        vec!["--set".as_ref(), "plugins.context.slots.window=on".as_ref()];
    let mut pty = full_screen_sized(&home, &home.0, &server, &args, &[], 160);
    pty.wait_shown(READY);
    pty.wait_shown("cached");
    pty.master.write_all(b"hello\r").unwrap();
    pty.wait_shown("GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    if std::env::var_os("WISP_BLESS").is_none() {
        let saved = std::fs::read_to_string(golden_dir().join("tui_default_tools.http")).unwrap();
        let docked = saved.replace("replies are 96 columns", "replies are 115 columns");
        assert_ne!(
            docked, saved,
            "the default tools golden has no terminal line"
        );
        assert_eq!(
            normalise(&server.requests()),
            docked,
            "the docked sidebar changes only the reply width"
        );
    }
}

#[test]
fn context_in_the_full_screen_counts_without_touching_the_message_request() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("full-screen-context");
    let mut pty = full_screen(&home, &server);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"hello\r").unwrap();
    pty.wait_for(b"GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"/context\r").unwrap();
    pty.wait_for(b"counted");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    let requests = server.requests();
    let messages: Vec<&(String, String)> = requests
        .iter()
        .filter(|(head, _)| !is_count(head))
        .collect();
    assert_eq!(messages.len(), 1, "one turn sends one message request");
    let counts = requests.len() - messages.len();
    assert_eq!(counts, 3, "the base, system prompt and tools parts");
    if std::env::var_os("WISP_BLESS").is_none() {
        let alone = normalise(&[messages[0].clone()]);
        let saved = std::fs::read_to_string(golden_dir().join("tui_default_tools.http")).unwrap();
        assert_eq!(
            alone.split("### request 2").next(),
            saved.split("### request 2").next(),
            "the turn's request is tui_default_tools's first"
        );
    }
    check("tui_context", &requests);
}

fn without_turn_lines(body: &str) -> serde_json::Value {
    let mut value: serde_json::Value = serde_json::from_str(body).unwrap();
    let per_turn = |block: &serde_json::Value| {
        block["text"]
            .as_str()
            .is_some_and(|t| t.starts_with("Sent: ") || t.starts_with("Context: "))
    };
    if let Some(messages) = value["messages"].as_array_mut() {
        for message in messages {
            if let Some(content) = message["content"].as_array_mut() {
                content.retain(|block| !per_turn(block));
            }
        }
    }
    value
}

fn wait_for_requests(server: &Server, count: usize) {
    assert!(
        wait_until(PATIENCE, || server.requests().len() >= count),
        "never saw {count} requests: {:?}",
        server.requests().len()
    );
}

#[test]
fn a_rewind_in_the_full_screen_resends_the_kept_prefix() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("full-screen-rewind");
    let mut pty = full_screen(&home, &server);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"hello\r").unwrap();
    wait_for_requests(&server, 1);
    pty.wait_for(b"GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"again\r").unwrap();
    wait_for_requests(&server, 2);
    pty.wait_screen("GOLDEN-DONE", |s| s.matches("GOLDEN-DONE").count() >= 2);
    pty.wait_idle();
    for key in [&b"\x1b"[..], b"\x1b"] {
        pty.master.write_all(key).unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }
    pty.wait_screen("the rewind turn rows", |shown| {
        shown.contains("turn 1") && shown.contains("turn 2")
    });
    pty.master.write_all(b"\r").unwrap();
    pty.wait_for(b"enter rewind");
    pty.master.write_all(b"\r").unwrap();
    pty.wait_for(b"unchanged");
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b", edited\r").unwrap();
    wait_for_requests(&server, 3);
    pty.wait_screen("GOLDEN-DONE", |s| s.matches("GOLDEN-DONE").count() >= 2);
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    let requests = server.requests();
    let messages: Vec<&(String, String)> = requests
        .iter()
        .filter(|(head, _)| !is_count(head))
        .collect();
    assert_eq!(messages.len(), 3, "three turns send three message requests");
    assert_eq!(
        without_turn_lines(&messages[2].1.replace("again, edited", "again")),
        without_turn_lines(&messages[1].1),
        "the third request is the second with only its last text edited"
    );
    check("tui_rewind", &requests);
}

fn answered(body: &str) -> bool {
    body.contains("\"tool_result\"")
}

fn agents_answer(body: &str) -> String {
    spawning(
        body,
        &serde_json::json!({"name": "helper", "task": "Say hello back to main."}),
    )
}

fn wait_for_report(server: &Server) {
    let reported = || {
        server
            .requests()
            .iter()
            .any(|(head, body)| !is_count(head) && !is_child(body) && body.contains("Hello back."))
    };
    assert!(
        wait_per_step(server, reported),
        "the report never reached main, and no step for {PATIENCE:?} after {} requests",
        server.requests().len()
    );
}

fn spawning(body: &str, spawn: &serde_json::Value) -> String {
    if !is_child(body) && body.contains("Hello back.") {
        return sse_reply(
            &serde_json::json!({"type": "text", "text": ""}),
            "GOLDEN-REPORT-DONE",
        );
    }
    match (is_child(body), answered(body)) {
        (_, true) => reply(),
        (false, false) => tool_reply("toolu_spawn", "spawn_agent", spawn),
        (true, false) => tool_reply(
            "toolu_report",
            "report",
            &serde_json::json!({"text": "Hello back."}),
        ),
    }
}

#[test]
fn the_full_screen_spawning_one_child_with_the_default_tools() {
    if !usable() {
        return;
    }
    let server = serve_by(Arc::new(agents_answer));
    let home = Home::new("full-screen-agents");
    let mut pty = full_screen(&home, &server);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"spawn a helper\r").unwrap();
    wait_for_report(&server);
    pty.wait_shown("GOLDEN-REPORT-DONE");
    pty.wait_screen("the report turn's end", |shown| {
        shown
            .split_once("GOLDEN-REPORT-DONE")
            .is_some_and(|(_, tail)| tail.lines().any(|line| line.trim().ends_with("cached 0%")))
    });
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    let exited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for_exit(&mut pty.child);
    }));
    assert!(exited.is_ok(), "{}", pty.shown());
    let requests = server.requests();
    let messages: Vec<&(String, String)> = requests
        .iter()
        .filter(|(head, _)| !is_count(head))
        .collect();
    let root = messages
        .iter()
        .find(|(_, body)| !is_child(body))
        .copied()
        .unwrap()
        .clone();
    let child = messages
        .iter()
        .find(|(_, body)| is_child(body))
        .copied()
        .unwrap()
        .clone();
    check("tui_agents", &[root, child]);
}

#[test]
fn the_normaliser_puts_count_requests_last_in_body_order() {
    let message = (
        "POST /v1/messages HTTP/1.1\r\n".to_owned(),
        "{\"m\":1}".to_owned(),
    );
    let count = |body: &str| {
        (
            "POST /v1/messages/count_tokens HTTP/1.1\r\n".to_owned(),
            body.to_owned(),
        )
    };
    let arrived = [count("{\"b\":2}"), message.clone(), count("{\"a\":1}")];
    let other = [count("{\"a\":1}"), count("{\"b\":2}"), message.clone()];
    assert_eq!(normalise(&arrived), normalise(&other));
    let text = normalise(&arrived);
    let at = |needle: &str| text.find(needle).unwrap();
    assert!(at("{\"m\":1}") < at("{\"a\":1}"));
    assert!(at("{\"a\":1}") < at("{\"b\":2}"));
    let plain = [message.clone(), message];
    assert_eq!(
        normalise(&plain),
        plain
            .iter()
            .enumerate()
            .map(|(i, (h, b))| format!(
                "### request {}\n{}\n{}\n",
                i + 1,
                normalise_head(h),
                normalise_body(b)
            ))
            .collect::<String>()
    );
}

fn full_screen_turn(
    name: &str,
    home: &Home,
    server: &Server,
    config: Option<&str>,
    envs: &[(&str, &str)],
) {
    if !usable() {
        return;
    }
    let config = config.map(|text| home.write("scratch/config.toml", text));
    let args: Vec<&std::ffi::OsStr> = match &config {
        Some(path) => vec!["--config".as_ref(), path.as_os_str()],
        None => Vec::new(),
    };
    let mut pty = full_screen_with(home, server, &args, envs);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"hello\r").unwrap();
    pty.wait_for(b"GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    check(name, &server.requests());
}

fn full_screen_tools(name: &str, tools: &str) {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new(name);
    let args: Vec<&std::ffi::OsStr> = vec!["--tools".as_ref(), tools.as_ref()];
    let mut pty = full_screen_with(&home, &server, &args, &[]);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"hello\r").unwrap();
    pty.wait_for(b"GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    check(name, &server.requests());
}

#[test]
fn the_full_screen_with_only_show_diff() {
    full_screen_tools("tui_show_diff_only", "show_diff");
}

#[test]
fn the_full_screen_with_only_ask_user() {
    full_screen_tools("tui_ask_user_only", "ask_user");
}

#[test]
fn the_full_screen_with_no_tools() {
    full_screen_tools("tui_no_tools", "none");
    full_screen_set("tui-preload-none", "tui_preload_none", "preload=none");
}

#[test]
fn the_full_screen_in_a_subdirectory_of_a_project_with_instructions() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("full-screen-instructions");
    let project = home.0.join("project");
    wisp_core::instructions::make_repository(&project).unwrap();
    std::fs::create_dir_all(project.join("sub")).unwrap();
    home.write(
        "project/AGENTS.md",
        "Answer briefly.\nRead @notes.md before you start.\n",
    );
    home.write("project/notes.md", "Keep notes short.\n");
    let mut pty = full_screen_in(&home, &project.join("sub"), &server, &[], &[]);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"hello\r").unwrap();
    pty.wait_for(b"GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    check("tui_instructions", &server.requests());
}

const GW_CONFIG: &str = "default = \"gw\"\n\n[endpoints.gw]\nprovider = \"anthropic\"\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl_env = \"GW_URL\"\n";

#[test]
fn the_full_screen_on_a_named_gateway_sends_native_search_by_default() {
    if !usable() {
        return;
    }
    let server = serve_by(Arc::new(|body| {
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        if body["tools"].as_array().unwrap().iter().any(|tool| {
            tool["type"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("web_search_"))
        }) {
            use support::server::{message_start, sse};
            let events = [
                message_start("msg_search", "claude-opus-5-5", 1),
                serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"s1","name":"web_search","input":{}}}),
                serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"fixture query\"}"}}),
                serde_json::json!({"type":"content_block_stop","index":0}),
                serde_json::json!({"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"s1","content":[{"type":"web_search_result","title":"Example","url":"https://example.com/facts","encrypted_content":"placeholder"}]}}),
                serde_json::json!({"type":"content_block_stop","index":1}),
                serde_json::json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}),
                serde_json::json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"Findings"}}),
                serde_json::json!({"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","title":"Example","url":"https://example.com/facts","encrypted_index":"placeholder","cited_text":"Facts"}}}),
                serde_json::json!({"type":"content_block_stop","index":2}),
                serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5,"server_tool_use":{"web_search_requests":1}}}),
                serde_json::json!({"type":"message_stop"}),
            ];
            sse(&events)
        } else if body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"].to_string().contains("tool_result"))
        {
            reply()
        } else {
            tool_reply(
                "q",
                "web_search",
                &serde_json::json!({"query":"fixture query"}),
            )
        }
    }));
    let home = Home::new("full-screen-search");
    let url = server.url();
    full_screen_turn(
        "tui_native_search",
        &home,
        &server,
        Some(GW_CONFIG),
        &[("GW_TOKEN", GW_TOKEN), ("GW_URL", url.as_str())],
    );
    let requests: Vec<_> = server
        .requests()
        .iter()
        .filter(|(head, _)| !is_count(head))
        .map(|(_, body)| serde_json::from_str::<serde_json::Value>(body).unwrap())
        .collect();
    assert_eq!(requests.len(), 3);
    assert_prefix_stable(&[requests[0].clone(), requests[2].clone()], None);
    assert!(!requests[0].to_string().contains("web_search_20250305"));
    assert!(requests[1].to_string().contains("web_search_20250305"));
    assert!(
        requests[2]
            .to_string()
            .contains("Findings\\n\\n1. Example — https://example.com/facts")
    );
}

#[test]
fn the_full_screen_on_a_named_gateway_with_search_turned_off() {
    let server = serve();
    let home = Home::new("full-screen-search-off");
    let url = server.url();
    full_screen_turn(
        "tui_search_off",
        &home,
        &server,
        Some(&format!("{GW_CONFIG}native_web_search = false\n")),
        &[("GW_TOKEN", GW_TOKEN), ("GW_URL", url.as_str())],
    );
}

#[test]
fn the_full_screen_on_the_env_gateway_sends_native_search_by_default() {
    let server = serve();
    let home = Home::new("full-screen-env-gateway");
    let url = server.url();
    full_screen_turn(
        "tui_env_gateway",
        &home,
        &server,
        None,
        &[
            ("ANTHROPIC_AUTH_TOKEN", GW_TOKEN),
            ("ANTHROPIC_BASE_URL", url.as_str()),
        ],
    );
}

#[test]
fn the_full_screen_on_the_env_gateway_with_search_turned_off() {
    let server = serve();
    let home = Home::new("full-screen-env-gateway-off");
    let url = server.url();
    full_screen_turn(
        "tui_env_search_off",
        &home,
        &server,
        Some("[endpoints.env]\nnative_web_search = false\n"),
        &[
            ("ANTHROPIC_AUTH_TOKEN", GW_TOKEN),
            ("ANTHROPIC_BASE_URL", url.as_str()),
        ],
    );
}

#[test]
fn the_normaliser_masks_only_the_context_token_count() {
    let body = "{\"text\":\"Context: 636 of 1M tokens (0%). Automatic compaction is off.\\n\"}";
    let other = "{\"text\":\"Context: 1637 of 1M tokens (0%). Automatic compaction is off.\\n\"}";
    assert_eq!(normalise_body(body), normalise_body(other));
    for rounded in ["1k", "1.2k", "12,345"] {
        assert_eq!(
            normalise_body(&body.replace("636", rounded)),
            normalise_body(body),
            "{rounded}"
        );
    }
    assert_ne!(
        normalise_body(&body.replace("636", "k")),
        normalise_body(body)
    );
    assert_eq!(
        normalise_body(body),
        "{\"text\":\"Context: <masked> of 1M tokens (0%). Automatic compaction is off.\\n\"}"
    );
    let prose = "{\"text\":\"A 'Context:' line. Context: about 5 of them. Context: 12 tokens\"}";
    assert_eq!(normalise_body(prose), prose);
    for changed in [
        body.replace("of 1M", "of 200K"),
        body.replace("(0%)", "(1%)"),
        body.replace("Automatic compaction is off.", "Wisp compacts at 80%."),
        body.replace("Context: ", "Context is: "),
    ] {
        assert_ne!(normalise_body(&changed), normalise_body(body), "{changed}");
    }
}

#[test]
fn the_normaliser_masks_only_what_varies() {
    let body = "{\"text\":\"Sent: Thursday 2026-10-22 09:28 UTC+05:30\\nhello\",\"system\":\"<environment>\\nWorking directory: ~/a\\nPlatform: Linux (WSL2)\\nResources: 24 CPUs, 47 GiB memory\\nShell for the bash tool: GNU bash, version 5.2\\n</environment>\\n\"}";
    let other = "{\"text\":\"Sent: Friday 2027-01-01 23:59 UTC+05:30\\nhello\",\"system\":\"<environment>\\nWorking directory: ~/elsewhere\\nPlatform: Linux (container)\\nResources: 1 CPU and 2 GiB memory (cgroup limits; nproc and free show the machine's 24 CPUs and 47 GiB)\\nShell for the bash tool: GNU bash, version 5.1\\n</environment>\\n\"}";
    assert_eq!(normalise_body(body), normalise_body(other));
    let seconds = body.replace("09:28", "09:28:41");
    assert_eq!(normalise_body(&seconds), normalise_body(body));
    assert_eq!(
        normalise_body("{\"a\":\"Platform: Linux\",\"b\":\"kept\"}"),
        "{\"a\":\"Platform: <masked>\",\"b\":\"kept\"}"
    );
    assert!(std::panic::catch_unwind(|| normalise_body("Platform: Linux")).is_err());
    assert_eq!(
        normalise_body(body),
        "{\"text\":\"Sent: <masked> UTC+05:30\\nhello\",\"system\":\"<environment>\\nWorking directory: <masked>\\nPlatform: <masked>\\nResources: <masked>\\nShell for the bash tool: <masked>\\n</environment>\\n\"}"
    );
    for changed in [
        body.replace("UTC+05:30", "UTC+00:00"),
        body.replace("\\nhello", "\\nhullo"),
        body.replace("Platform: ", "Platform is: "),
        body.replace("Resources: ", "Resources are: "),
        body.replace("Resources: 24 CPUs, 47 GiB memory\\n", ""),
        body.replace("<environment>\\n", "<env>\\n"),
        body.replace("Shell for the bash tool: GNU bash, version 5.2\\n", ""),
    ] {
        assert_ne!(normalise_body(&changed), normalise_body(body), "{changed}");
    }
    let head = "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1:41234\r\nContent-Length: 99\r\nconnection: keep-alive\r\naccept-encoding: gzip\r\nx-api-key: golden-api-key-0001\r\nanthropic-version: 2023-06-01\r\n";
    assert_eq!(
        normalise_head(head),
        "POST /v1/messages HTTP/1.1\nx-api-key: <masked>\nanthropic-version: 2023-06-01\n"
    );
    let bearer = head.replace("x-api-key: golden-api-key-0001", "authorization: Bearer t");
    assert_eq!(
        normalise_head(&bearer),
        "POST /v1/messages HTTP/1.1\nauthorization: Bearer <masked>\nanthropic-version: 2023-06-01\n"
    );
    assert_ne!(
        normalise_head(&bearer.replace("Bearer t", "Basic t")),
        normalise_head(&bearer)
    );
    assert_ne!(
        normalise_head(&head.replace("2023-06-01", "2024-01-01")),
        normalise_head(head)
    );
}

fn full_screen_set(name: &str, golden: &str, set: &str) {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new(name);
    let args: Vec<&std::ffi::OsStr> = vec!["--set".as_ref(), set.as_ref()];
    let mut pty = full_screen_with(&home, &server, &args, &[]);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"hello\r").unwrap();
    pty.wait_for(b"GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    check(golden, &server.requests());
}

#[test]
fn the_full_screen_with_the_time_plugin_removed_sends_no_time_sentence() {
    full_screen_set(
        "full-screen-time-removed",
        "tui_time_removed",
        "plugins.time.installed=off",
    );
}

#[test]
fn the_full_screen_with_the_time_plugin_added_back_sends_the_default_bytes() {
    if std::env::var_os("WISP_BLESS").is_none() {
        full_screen_set(
            "full-screen-time-back",
            "tui_default_tools",
            "plugins.time.installed=on",
        );
    }
}

#[test]
fn a_jsonl_session_with_the_time_plugin_off_sends_the_time_off_bytes() {
    let requests = jsonl(
        "time-plugin-off",
        &["--set", "plugins.time.enabled=off"],
        with_env_endpoint,
    );
    if std::env::var_os("WISP_BLESS").is_none() {
        check("time_off", &requests);
    }
}

fn full_screen_switch(name: &str, golden: &str, args: &[&str], tool: &str, from: &str) {
    full_screen_switch_around(name, golden, args, |pty| pty.switch_tool(tool, from), None);
}

fn full_screen_switch_around(
    name: &str,
    golden: &str,
    args: &[&str],
    switch: impl FnOnce(&mut Pty),
    rebuilt: Option<Rebuilt>,
) {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new(name);
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_ref()).collect();
    let mut pty = full_screen_sized(&home, &home.0, &server, &args, &[], 160);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"hello\r").unwrap();
    wait_for_requests(&server, 1);
    pty.wait_for(b"GOLDEN-DONE");
    pty.wait_idle();
    switch(&mut pty);
    pty.master.write_all(b"again\r").unwrap();
    wait_for_requests(&server, 2);
    pty.wait_screen("GOLDEN-DONE", |s| s.matches("GOLDEN-DONE").count() >= 2);
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    let requests = server.requests();
    let messages: Vec<&(String, String)> = requests
        .iter()
        .filter(|(head, _)| !is_count(head))
        .collect();
    assert_eq!(messages.len(), 2, "two turns send two message requests");
    let bodies: Vec<serde_json::Value> = messages
        .iter()
        .map(|(_, body)| serde_json::from_str::<serde_json::Value>(body).unwrap())
        .collect();
    assert_prefix_stable(&bodies, rebuilt);
    check(golden, &requests);
}

#[test]
fn a_tool_turned_off_on_a_model_without_inline_tools_is_noted_in_the_next_message() {
    full_screen_switch(
        "full-screen-tool-off",
        "tui_tool_off_next_message",
        &["--model", "claude-opus-4-7"],
        "show_diff",
        "[x]",
    );
}

#[test]
fn a_tool_turned_on_where_the_model_cant_take_blocks_rebuilds_once() {
    full_screen_switch_around(
        "full-screen-tool-rebuild",
        "tui_tool_rebuild",
        &["--model", "claude-opus-4-7", "--tools", "view_file"],
        |pty| pty.switch_tool("show_diff", "[ ]"),
        Some(Rebuilt {
            at: 1,
            system_changes: true,
        }),
    );
}

#[test]
fn a_tool_turned_on_on_a_capable_model_is_added_inline() {
    full_screen_switch(
        "full-screen-tool-add",
        "tui_tool_inline_add",
        &["--tools", "view_file"],
        "show_diff",
        "[ ]",
    );
}

#[test]
fn web_search_turned_off_in_a_running_session_rebuilds_once() {
    full_screen_switch_around(
        "full-screen-web-search-off",
        "tui_web_search_off",
        &[],
        Pty::switch_search,
        Some(Rebuilt {
            at: 1,
            system_changes: false,
        }),
    );
}

#[test]
fn a_first_tool_turned_on_in_a_session_started_without_tools_rebuilds_once() {
    full_screen_switch_around(
        "full-screen-first-tool",
        "tui_first_tool",
        &["--tools", "none"],
        |pty| pty.switch_tool("bash", "[ ]"),
        Some(Rebuilt {
            at: 1,
            system_changes: true,
        }),
    );
}

const BRAIN: &str = "preset = \"brain\"\ntools = [\"view_file\", \"show_diff\"]\n\n[instructions]\nsource = \"none\"\n\n[agents.presets.brain]\ndescription = \"Teaches\"\ninstructions = \"BRAIN.md\"\nmodel = \"gpt-6.1-sol\"\neffort = \"max\"\ntools_off = [\"show_diff\"]\n";

fn turn(pty: &mut Pty, server: &Server, text: &str) {
    std::thread::sleep(Duration::from_millis(200));
    let done = pty.shown().matches("GOLDEN-DONE").count();
    pty.master
        .write_all(format!("{text}\r").as_bytes())
        .unwrap();
    wait_for_requests(server, server.requests().len() + 1);
    pty.wait_screen("GOLDEN-DONE", |s| s.matches("GOLDEN-DONE").count() > done);
    pty.wait_idle();
}

#[test]
fn a_main_preset_keeps_its_prefix_and_tools_across_turns_and_resumes() {
    if !usable() {
        return;
    }
    let sent = |server: &Server| -> Vec<serde_json::Value> {
        let requests = server.requests();
        let asked = requests.iter().filter(|(head, _)| !is_count(head));
        asked
            .map(|(_, body)| serde_json::from_str(body).unwrap())
            .collect()
    };
    let server = serve();
    let home = Home::new("main-preset");
    home.write(CONFIG, BRAIN);
    home.write(".config/wisp/BRAIN.md", "Teach gently.\n");
    let mut pty = full_screen(&home, &server);
    pty.wait_for(READY);
    turn(&mut pty, &server, "hello");
    turn(&mut pty, &server, "again");
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    drop(pty);
    home.write(".config/wisp/BRAIN.md", "Teach firmly.\n");
    home.write(CONFIG, &BRAIN.replace("\"brain\"\n", "\"ghost\"\n"));
    let mut pty = full_screen_with(&home, &server, &["--resume".as_ref()], &[]);
    pty.wait_shown("\u{203a} again");
    turn(&mut pty, &server, "once more");
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    drop(pty);
    home.write(CONFIG, BRAIN.split("\n\n[agents").next().unwrap());
    let mut pty = full_screen_with(&home, &server, &["--resume".as_ref()], &[]);
    pty.wait_shown("\u{203a} once more");
    turn(&mut pty, &server, "and again");
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    let bodies = sent(&server);
    assert_eq!(bodies.len(), 4);
    assert_eq!(bodies[0]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(bodies[0]["tools"][0]["name"], "view_file");
    let system: String = bodies[0]["system"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    assert!(
        system.contains(
            "that file's directory.\n<file scope=\"preset\" name=\"BRAIN.md\">\nTeach gently.\n"
        ),
        "{system}"
    );
    assert!(!system.contains("take precedence"), "{system}");
    let opened = bodies[0]["messages"][0].to_string();
    assert!(
        opened.contains("Preset instructions: ~/.config/wisp/BRAIN.md"),
        "{opened}"
    );
    let added = |at: usize| {
        let kept = bodies[at - 1]["messages"].as_array().unwrap().len();
        format!("{:?}", &bodies[at]["messages"].as_array().unwrap()[kept..])
    };
    assert!(!added(2).contains("<environment>"), "{}", added(2));
    assert!(!added(2).contains("show_diff"), "{}", added(2));
    assert!(added(3).contains("show_diff"), "{}", added(3));
    let update = "<instructions-update>\nThese files changed since they were loaded. Each version below replaces the earlier one.\n<file scope=\"preset\" name=\"BRAIN.md\" path=\"~/.config/wisp/BRAIN.md\">\nTeach firmly.\n</file>\n</instructions-update>\n";
    let updates = |at: usize| added(at).matches("<instructions-update>").count();
    assert_eq!([updates(1), updates(2), updates(3)], [0, 1, 0]);
    assert!(added(2).contains(&format!("{update:?}")), "{}", added(2));
    for body in &bodies {
        assert_eq!(body["system"], bodies[0]["system"]);
        assert_eq!(body["tools"], bodies[0]["tools"]);
    }
    assert_prefix_stable(&bodies, None);
}

#[test]
fn fresh_main_chats_refuse_models_without_adaptive_thinking() {
    for (source, args, config) in [
        ("flag", vec!["--model", "claude-opus-4-5"], ""),
        ("config", vec![], "model = \"claude-opus-4-5\"\n"),
        (
            "preset",
            vec![],
            "preset = \"legacy\"\n[agents.presets.legacy]\ndescription = \"Legacy\"\nmodel = \"claude-opus-4-5\"\n",
        ),
    ] {
        let server = serve();
        let home = Home::new("non-adaptive-main");
        home.write(CONFIG, config);
        let mut doctor = home.golden_command(WISP);
        doctor.env("WISP_JOB", "1").args(&args).arg("doctor");
        with_env_endpoint(&home, &server, &mut doctor);
        let output = doctor.output().unwrap();
        assert!(output.status.success(), "{}", both(&output));
        assert!(both(&output).contains("→ 127.0.0.1:"), "{}", both(&output));
        let mut command = home.golden_command(WISP);
        command
            .env("WISP_JOB", "1")
            .args(["--output", "jsonl"])
            .args(&args);
        with_env_endpoint(&home, &server, &mut command);
        let output = support::home::run(&mut command, "hello\n");
        assert_eq!(output.status.code(), Some(1), "{source}: {}", both(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(
                "wisp: claude-opus-4-5 doesn't support adaptive thinking, which Wisp needs"
            ),
            "{source}: {}",
            both(&output)
        );
        assert!(server.requests().is_empty(), "{source}: a request was sent");
    }
}

#[test]
fn a_main_preset_without_tools_names_its_file_and_drops_what_its_account_lacks() {
    let first = |config: &str| {
        let requests = jsonl("main-preset-jsonl", &[], |home, server, command| {
            home.write(CONFIG, config);
            home.write(".config/wisp/BRAIN.md", "Teach gently.\n");
            with_env_endpoint(home, server, command);
        });
        let (_, body) = requests.iter().find(|(head, _)| !is_count(head)).unwrap();
        serde_json::from_str::<serde_json::Value>(body).unwrap()
    };
    let older = first(&format!(
        "model = \"claude-opus-4-6\"\n{}",
        BRAIN.replace("effort = \"max\"", "effort = \"xhigh\"")
    ));
    assert_eq!(older["model"], "claude-opus-4-6");
    assert_eq!(older["output_config"]["effort"], "medium");
    let body = first(BRAIN);
    assert!(body["tools"].is_null(), "{body}");
    assert_eq!(body["model"], wisp_protocol::settings::DEFAULT_MODEL);
    assert_eq!(body["output_config"]["effort"], "max");
    let opened = body["messages"][0].to_string();
    assert!(
        opened.contains("<environment>")
            && opened.contains("Preset instructions: ~/.config/wisp/BRAIN.md"),
        "{opened}"
    );
}

const REV: &str = "[agents.presets.rev]\ndescription = \"Reviews\"\neffort = \"low\"\n";

fn rev_answer(body: &str) -> String {
    spawning(
        body,
        &serde_json::json!({"name": "helper", "task": "Say hello back to main.", "preset": "rev"}),
    )
}

#[test]
fn a_changed_preset_shows_one_line_at_resume_and_nothing_on_stderr() {
    if !usable() {
        return;
    }
    let server = serve_by(Arc::new(rev_answer));
    let home = Home::new("changed-preset");
    home.write(CONFIG, REV);
    let mut pty = full_screen(&home, &server);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"spawn a helper\r").unwrap();
    wait_for_report(&server);
    pty.wait_shown("GOLDEN-REPORT-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    drop(pty);

    home.write(
        CONFIG,
        &REV.replace("effort = \"low\"", "effort = \"medium\""),
    );
    let resume: Vec<&std::ffi::OsStr> = vec!["--resume".as_ref()];
    let mut pty = full_screen_sized(&home, &home.0, &server, &resume, &[], 120);
    pty.wait_shown("1 preset changed since agents started; see /agents");
    pty.master.write_all(b"\x14").unwrap();
    pty.wait_shown("1 agent");
    pty.master.write_all(b"\x1b[B").unwrap();
    pty.wait_shown("\u{b7} preset changed");
    pty.master.write_all(b"\x1b").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    let stream = String::from_utf8_lossy(&pty.screen.lock().unwrap()).into_owned();
    for said in ["has changed", "keeps the configuration", "wisp: preset"] {
        assert!(!stream.contains(said), "{said}: {stream:?}");
    }
    drop(pty);

    let mut command = home.golden_command(WISP);
    let output = command
        .args(["--resume", "--output", "jsonl"])
        .env("ANTHROPIC_API_KEY", API_KEY)
        .env("ANTHROPIC_BASE_URL", server.url())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let sentence = "wisp: preset rev has changed; /helper keeps the configuration it was spawned with, and new spawns use the preset as it is now\n";
    assert_eq!(stderr.matches(sentence).count(), 1, "{stderr}");
    let resumed = stderr
        .find("Resumed session")
        .unwrap_or_else(|| panic!("{stderr}"));
    assert!(stderr.find(sentence).unwrap() < resumed, "{stderr}");
}
