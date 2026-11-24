#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket or process error"
)]

use crate::support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use support::pty::{wait_for_exit, wait_until};
use support::server::{Server, json, text_reply};
use support::{
    WISP,
    home::{CONFIG_DIR, Home, SESSIONS, both, command_in, err, fed, run},
};

const LINE: &str = "why does my terminal print [?62;22c and [24;80R after vim exits? also [?u";
const NEEDS_TERMINAL: &str =
    "wisp: the full screen needs a terminal; use --output jsonl for scripts and pipes\n";

impl Home {
    fn piped_command(&self, port: u16) -> Command {
        let mut command = self.logged(&[WISP], port);
        command.arg("--no-log");
        command
    }

    fn logged(&self, start: &[&str], port: u16) -> Command {
        let mut command = command_in(&self.0, start[0]);
        command
            .args(&start[1..])
            .args(["--time", "off", "--env-file"])
            .arg(self.env_file())
            .env("TERM", "xterm-256color")
            .env("ANTHROPIC_API_KEY", "test-key-not-used")
            .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

fn refused(output: &Output) {
    assert_eq!(output.status.code(), Some(2), "{}", both(output));
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn piped_input_reaches_the_session_unchanged() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = Home::new("input");
    let child = fed(
        home.piped_command(port).args(["--output", "jsonl"]),
        &format!("{LINE}\n"),
    );
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut request = vec![];
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("wisp never sent a request: {e}"),
        }
    };
    let mut stream = stream;
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut buf = [0u8; 8192];
    while Instant::now() < deadline && !String::from_utf8_lossy(&request).contains(LINE) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => request.extend_from_slice(&buf[..n]),
            Err(_) => {}
        }
    }
    let refusal = r#"{"type":"error","error":{"type":"invalid_request_error","message":"seen"}}"#;
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{refusal}",
            refusal.len()
        )
        .as_bytes(),
    );
    drop(stream);
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&request);
    assert!(
        text.contains(&format!("\"text\":\"{LINE}\"")),
        "{text:?} / {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "input_delivered" && e["ids"][0] == "jsonl-1"),
        "{stdout}"
    );
    assert!(events.iter().any(|e| e["type"] == "turn_ended"), "{stdout}");
}

#[test]
fn auto_through_a_pipe_exits_2_before_any_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let home = Home::new("auto");
    for extra in [&[][..], &["--output", "auto"]] {
        let output = run(home.piped_command(port).args(extra), "hello\n");
        refused(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            NEEDS_TERMINAL,
            "{extra:?}"
        );
    }
    assert!(
        listener.accept().is_err(),
        "wisp contacted the server while refusing to start"
    );
}

#[test]
fn an_explicit_tui_through_a_pipe_exits_2_with_the_reason() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let home = Home::new("tui");
    let output = home
        .piped_command(port)
        .args(["--output", "tui"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    refused(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "wisp: --output tui needs a terminal, but stdin is not a terminal; use --output jsonl for scripts and pipes\n"
    );
    assert!(listener.accept().is_err());
}

#[test]
fn plain_output_and_screen_reader_mode_are_rejected() {
    let home = Home::new("removed");
    for (args, flag) in [
        (
            &["--output", "plain"][..],
            "--output must be one of auto, tui, jsonl",
        ),
        (&["--screen-reader"], "unexpected argument"),
    ] {
        let output = home
            .piped_command(9)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", both(&output));
        assert!(stderr.contains(flag), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn the_working_directory_agents_md_is_announced_and_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = Home::new("instructions");
    wisp_core::instructions::make_repository(&home.0).unwrap();
    std::fs::write(home.0.join("AGENTS.md"), "piped-instructions-canary\n").unwrap();
    let child = fed(
        home.piped_command(port).args(["--output", "jsonl"]),
        "hello\n",
    );
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("wisp never sent a request: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut request = vec![];
    let mut buf = [0u8; 8192];
    while Instant::now() < deadline && !String::from_utf8_lossy(&request).contains("hello") {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => request.extend_from_slice(&buf[..n]),
            Err(_) => {}
        }
    }
    let refusal = r#"{"type":"error","error":{"type":"invalid_request_error","message":"seen"}}"#;
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{refusal}",
            refusal.len()
        )
        .as_bytes(),
    );
    drop(stream);
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&request);
    assert!(text.contains("piped-instructions-canary"), "{text:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let loaded = events
        .iter()
        .find(|e| e["type"] == "instructions_loaded")
        .unwrap_or_else(|| panic!("{stdout}"));
    assert_eq!(loaded["chosen"]["scope"], "project");
    assert_eq!(loaded["chosen"]["display_path"], "~/AGENTS.md");
}

fn one_request(home: &Home, args: &[&str]) -> (String, Output) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let child = fed(
        home.piped_command(port)
            .args(["--output", "jsonl"])
            .args(args),
        "hello\n",
    );
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("wisp never sent a request: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut request = vec![];
    let mut buf = [0u8; 8192];
    while Instant::now() < deadline && !String::from_utf8_lossy(&request).contains("hello") {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => request.extend_from_slice(&buf[..n]),
            Err(_) => {}
        }
    }
    let refusal = r#"{"type":"error","error":{"type":"invalid_request_error","message":"seen"}}"#;
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{refusal}",
            refusal.len()
        )
        .as_bytes(),
    );
    drop(stream);
    let output = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&request).into_owned(), output)
}

#[test]
fn a_per_place_set_reaches_the_session_and_an_unknown_place_is_warned_about() {
    let home = Home::new("per-place");
    let (plain, _) = one_request(&home, &[]);
    assert!(plain.contains(" of 1M tokens"), "{plain}");
    let (flagged, output) = one_request(
        &home,
        &[
            "--set",
            "models.\"claude-opus-5-5\".context_window=300000",
            "--set",
            "endpoints.typo.pricing=none",
        ],
    );
    assert!(flagged.contains(" of 300k tokens"), "{flagged}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("wisp: --set endpoints.typo.pricing names no account Wisp knows\n"),
        "{stderr}"
    );
}

const COUNT: &str = "POST /v1/messages/count_tokens ";

fn replying_server() -> Server {
    Server::start(|request| {
        if request.head.starts_with(COUNT) {
            json("200 OK", r#"{"input_tokens": 10}"#)
        } else {
            text_reply("msg_piped", "claude-opus-5-5", "PIPED-DONE", (1, 5))
        }
    })
}

fn turns(server: &Server) -> Vec<String> {
    let seen = server.seen.lock().unwrap();
    let turns = seen.iter().filter(|s| !s.head.starts_with(COUNT));
    turns.map(|s| s.body.clone()).collect()
}

#[test]
fn a_blocked_line_is_rejected_and_the_next_line_runs_a_turn() {
    let server = replying_server();
    let port = server.port;
    let home = Home::new("block-input");
    let mut child = fed(
        home.piped_command(port)
            .args(["--output", "jsonl"])
            .env("WISP_TESTKIT", "block-input"),
        "block this\nhello\n",
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("wisp did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{}", both(&output));
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let at = |pick: &dyn Fn(&serde_json::Value) -> bool| {
        events
            .iter()
            .position(pick)
            .unwrap_or_else(|| panic!("{stdout}"))
    };
    let withdrawn = at(&|e| {
        e["type"] == "input_withdrawn"
            && e["ids"][0] == "jsonl-1"
            && e["by"]["hook"]["reason"] == "the message starts with block"
    });
    let rejected = at(&|e| {
        e["type"] == "rejected" && e["kind"] == "input_blocked" && e.get("request").is_none()
    });
    let started = at(&|e| e["type"] == "turn_started");
    let ended = at(&|e| e["type"] == "turn_ended");
    assert!(
        withdrawn < rejected && rejected < started && started < ended,
        "{stdout}"
    );
    assert_eq!(
        events.iter().filter(|e| e["type"] == "turn_ended").count(),
        1,
        "{stdout}"
    );
    let ran: Vec<(&str, &str)> = events[rejected..]
        .iter()
        .filter(|e| e["type"] == "hook_ran")
        .map(|e| {
            (
                e["hook"].as_str().unwrap(),
                e["outcome"]["type"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ran,
        [
            ("idle/seen", "none"),
            ("testkit/block-input", "none"),
            ("mcp/refresh", "none"),
            ("skills/scan", "none"),
            ("workspace/environment", "none"),
            ("instructions/instructions", "none"),
            ("workspace/terminal", "none"),
            ("skills/nudge", "none"),
            ("context/fill", "applied"),
            ("idle/note", "none"),
            ("agents/kept-warm", "none"),
            ("shell/processes", "none"),
            ("idle/done", "none")
        ],
        "{stdout}"
    );
    let bodies = turns(&server);
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    assert!(bodies[0].contains(r#""text":"hello""#), "{}", bodies[0]);
    assert!(!bodies[0].contains("block this"), "{}", bodies[0]);
}

fn delivered(home: &Home, port: u16, extra: &[&str], line: &str) -> Vec<String> {
    let output = run(
        home.logged(&[WISP], port)
            .args(["--output", "jsonl"])
            .args(extra),
        &format!("{line}\n"),
    );
    assert_eq!(output.status.code(), Some(0), "{}", both(&output));
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|e| e["type"] == "input_delivered")
        .map(|e| e["ids"][0].as_str().unwrap().to_owned())
        .collect()
}

fn session_logs(home: &Home) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(home.file(SESSIONS))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

#[test]
fn a_resumed_jsonl_session_numbers_its_inputs_past_the_log() {
    let server = replying_server();
    let port = server.port;
    let home = Home::new("resume-ids");
    assert_eq!(delivered(&home, port, &[], "hello"), ["jsonl-1"]);
    assert_eq!(delivered(&home, port, &["--resume"], "again"), ["jsonl-2"]);
    assert_eq!(turns(&server).len(), 2);
    assert_eq!(session_logs(&home).len(), 1);
    let started: Vec<String> = home
        .records()
        .into_iter()
        .filter(|r| r["type"] == "turn_started")
        .map(|r| r["input_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(started, ["jsonl-1", "jsonl-2"], "{}", home.logs());
}

const PINNED_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

#[test]
fn a_pinned_id_starts_a_chat_and_the_next_run_continues_it() {
    let server = replying_server();
    let home = Home::new("pinned-id");
    let file = home.file(&format!("{SESSIONS}/{PINNED_ID}.jsonl"));
    assert_eq!(
        delivered(
            &home,
            server.port,
            &["--session-id", "550E8400E29B41D4A716446655440000"],
            "hello"
        ),
        ["jsonl-1"]
    );
    assert_eq!(session_logs(&home).as_slice(), std::slice::from_ref(&file));
    assert_eq!(
        delivered(&home, server.port, &["--session-id", PINNED_ID], "again"),
        ["jsonl-2"]
    );
    assert_eq!(session_logs(&home), [file]);
    let requests: Vec<_> = turns(&server)
        .iter()
        .map(|request| serde_json::from_str(request).unwrap())
        .collect();
    assert_eq!(requests.len(), 2);
    support::prefix::assert_prefix_stable(&requests, None);
}

#[test]
fn session_id_refusals_send_nothing() {
    let home = Home::new("pinned-refusals");
    let server = replying_server();
    let invalid = "CANARY_not_a_uuid";
    let refuse = |home: &Home, args: &[&str], code| {
        let output = run(
            home.logged(&[WISP], server.port)
                .args(["--output", "jsonl"])
                .args(args),
            "not sent\n",
        );
        assert_eq!(output.status.code(), Some(code), "{}", both(&output));
        assert!(output.stdout.is_empty(), "{}", both(&output));
        err(&output)
    };
    let check = |args: &[&str], code, message| assert_eq!(refuse(&home, args, code), message);
    check(
        &["--session-id", invalid],
        2,
        "wisp: --session-id takes a UUID, such as uuidgen prints (what was given isn't shown)\n",
    );
    for (flag, message) in [
        (
            "--resume",
            "wisp: --session-id can't be used with --resume\n",
        ),
        (
            "--resume-unlocked",
            "wisp: --session-id can't be used with --resume-unlocked\n",
        ),
        (
            "--no-log",
            "wisp: --session-id can't be used with --no-log\n",
        ),
    ] {
        check(&["--session-id", PINNED_ID, flag], 2, message);
    }
    let logging_off = || {
        check(
            &["--session-id", PINNED_ID, "--set", "log=false"],
            1,
            "wisp: --session-id needs the session log, and logging is off\n",
        );
    };
    logging_off();
    assert!(!home.exists(SESSIONS));
    home.write(&format!("{SESSIONS}/{PINNED_ID}.jsonl"), "");
    logging_off();

    let blocked = Home::new("pinned-log-fails");
    blocked.write(SESSIONS, "not a directory");
    let error = refuse(&blocked, &["--session-id", PINNED_ID], 1);
    assert!(error.starts_with("wisp: could not open "), "{error}");
    assert!(server.seen.lock().unwrap().is_empty());
}

#[test]
fn a_missing_preset_endpoint_is_retained_for_live_activation() {
    let home = Home::new("presets");
    let config = home.0.join(CONFIG_DIR);
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[agents.presets.lost]\ndescription = \"Lost\"\nendpoint = \"nowhere\"\n\n[agents.presets.here]\ndescription = \"Here\"\nendpoint = \"env\"\nmodel = \"claude-haiku-4-5\"\n",
    )
    .unwrap();
    let output = home
        .piped_command(9)
        .args(["--output", "jsonl"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("preset lost"), "{stderr}");
    assert!(!stderr.contains("preset here"), "{stderr}");
}

const OWNERS_PRESETS: &str = "default = \"direct\"\n[endpoints.direct]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"DIRECT_KEY\"\nurl = \"http://127.0.0.1:9\"\n\n[endpoints.gpt]\nprovider = \"openai\"\nauth = \"api_key\"\nkey_env = \"GPT_KEY\"\nurl = \"http://127.0.0.1:9\"\n\n[agents.presets.coder]\ndescription = \"Codes\"\nendpoint = \"direct\"\nmodel = \"claude-opus-5-5\"\n\n[agents.presets.reviewer]\ndescription = \"Reviews\"\nendpoint = \"gpt\"\nmodel = \"gpt-6.1-sol\"\n";

fn two_providers(home: &Home, extra: &str) -> Command {
    let config = home.0.join(CONFIG_DIR);
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!("{OWNERS_PRESETS}{extra}"),
    )
    .unwrap();
    let mut command = command_in(&home.0, WISP);
    command
        .args(["--no-log", "--time", "off"])
        .env("DIRECT_KEY", "synthetic-anthropic-key")
        .env("GPT_KEY", "synthetic-openai-key")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn agents_section(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{}", both(output));
    stdout
        .split("\nagents\n")
        .nth(1)
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn the_owners_presets_check_on_with_an_anthropic_and_an_openai_endpoint() {
    let home = Home::new("two-providers");
    let output = two_providers(&home, "")
        .args(["--output", "jsonl"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("preset"), "{stderr}");
    let doctor = two_providers(&home, "").arg("doctor").output().unwrap();
    let section = agents_section(&doctor);
    for line in [
        "  preset coder       Codes · account direct · model claude-opus-5-5",
        "  preset reviewer    Reviews · account gpt · model gpt-6.1-sol",
    ] {
        assert!(
            section.lines().any(|l| l == line),
            "missing {line:?} in\n{section}"
        );
    }
    for output in [&output, &doctor] {
        let all = both(output);
        assert!(!all.contains("synthetic-"), "{all}");
    }
}

#[test]
fn a_known_model_preset_keeps_its_definition_until_spawn_compatibility_is_known() {
    let home = Home::new("two-providers-off");
    let off = "\n[agents.presets.critic]\ndescription = \"Critiques\"\nendpoint = \"gpt\"\nmodel = \"claude-opus-5-5\"\n";
    let output = two_providers(&home, off)
        .args(["--output", "jsonl"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("preset critic"), "{stderr}");
    assert!(!stderr.contains("preset reviewer"), "{stderr}");
    let section = agents_section(&two_providers(&home, off).arg("doctor").output().unwrap());
    assert!(
        section.lines().any(|l| l
            == "  preset critic      Critiques · account gpt (not checked) · model claude-opus-5-5 (not checked)"),
        "{section}"
    );
}

#[test]
fn a_preset_on_another_provider_without_a_model_stays_on_without_warning() {
    let home = Home::new("two-providers-inherit");
    let helper = "\n[agents.presets.helper]\ndescription = \"Helps\"\nendpoint = \"gpt\"\n";
    let output = two_providers(&home, helper)
        .args(["--output", "jsonl"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("preset helper"), "{stderr}");
    assert!(!stderr.contains("preset reviewer"), "{stderr}");
    let on_gpt = two_providers(&home, helper)
        .args([
            "--output",
            "jsonl",
            "--endpoint",
            "gpt",
            "--model",
            "gpt-6.1-sol",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&on_gpt.stderr);
    assert!(!stderr.contains("preset helper"), "{stderr}");
    let section = agents_section(&two_providers(&home, helper).arg("doctor").output().unwrap());
    assert!(
        section
            .lines()
            .any(|l| l == "  preset helper      Helps · account gpt"),
        "{section}"
    );
}

fn run_logged(home: &Home, port: u16, args: &[&str], line: &str) -> Output {
    let output = run(
        home.logged(&[WISP], port)
            .args(["--output", "jsonl"])
            .args(args),
        &format!("{line}\n"),
    );
    assert!(output.status.success(), "{}", both(&output));
    output
}

#[test]
fn resume_flags_for_the_instructions_reach_the_resumed_request() {
    const CANARY: &str = "piped-resume-canary";
    let server = replying_server();
    let port = server.port;
    let home = Home::new("resume-instructions");
    wisp_core::instructions::make_repository(&home.0).unwrap();
    std::fs::write(home.0.join("AGENTS.md"), format!("{CANARY}\n")).unwrap();
    run_logged(&home, port, &[], "hello");
    run_logged(
        &home,
        port,
        &[
            "--resume",
            "--instructions",
            "none",
            "--instructions-extra",
            "autonomous",
        ],
        "again",
    );
    let bodies = turns(&server);
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    let autonomous = wisp_core::instructions::Extra::Autonomous
        .text()
        .lines()
        .next()
        .unwrap();
    assert!(bodies[0].contains(CANARY), "{}", bodies[0]);
    assert!(!bodies[0].contains(autonomous), "{}", bodies[0]);
    assert!(!bodies[1].contains(CANARY), "{}", bodies[1]);
    assert!(bodies[1].contains(autonomous), "{}", bodies[1]);
}

#[test]
fn sigterm_mid_turn_and_sighup_when_idle_end_a_jsonl_session_cleanly_and_die_by_the_signal() {
    for (name, signal) in [("sigterm", Signal::TERM), ("sighup", Signal::HUP)] {
        let asked = Arc::new(AtomicBool::new(false));
        let dead = Arc::new(AtomicBool::new(false));
        let (held, freed) = (Arc::clone(&asked), Arc::clone(&dead));
        let server = Server::start(move |request| {
            if request.head.starts_with(COUNT) {
                return json("200 OK", r#"{"input_tokens": 10}"#);
            }
            held.store(true, Ordering::SeqCst);
            while signal == Signal::TERM && !freed.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
            text_reply("msg_piped", "claude-opus-5-5", "PIPED-DONE", (1, 5))
        });
        let home = Home::new(name);
        let mut command = home.logged(&[WISP], server.port);
        let mut child = command.args(["--output", "jsonl"]).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"hello\n").unwrap();
        let ready = wait_until(Duration::from_secs(20), || match signal {
            Signal::TERM => asked.load(Ordering::SeqCst),
            _ => home.logs().contains(r#""type":"turn_ended""#),
        });
        assert!(ready, "{name}: {}", home.logs());
        if signal == Signal::HUP {
            std::thread::sleep(Duration::from_millis(300));
        }
        let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        kill_process(pid, signal).unwrap();
        let status = wait_for_exit(&mut child);
        dead.store(true, Ordering::SeqCst);
        drop(stdin);
        assert_eq!(status.signal(), Some(signal.as_raw()), "{name}: {status:?}");
        let records = home.records();
        let last = records.last().unwrap();
        assert_eq!(last["type"], "session_ended", "{name}: {records:?}");
        let interrupted = (signal == Signal::TERM).then_some(1);
        assert_eq!(last["interrupted"].as_u64(), interrupted, "{name}: {last}");
    }
}

#[test]
fn sigint_mid_turn_ends_the_turn_cancelled_and_the_run_goes_on() {
    let asked = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (held, freed) = (Arc::clone(&asked), Arc::clone(&cancelled));
    let server = Server::start(move |request| {
        if request.head.starts_with(COUNT) {
            return json("200 OK", r#"{"input_tokens": 10}"#);
        }
        if request.body.contains("first line") && !request.body.contains("second line") {
            held.store(true, Ordering::SeqCst);
            wait_until(Duration::from_secs(20), || freed.load(Ordering::SeqCst));
        }
        text_reply("msg_piped", "claude-opus-5-5", "PIPED-DONE", (1, 5))
    });
    let home = Home::new("sigint");
    let mut child = home
        .logged(&[WISP], server.port)
        .args(["--output", "jsonl"])
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"first line\n").unwrap();
    assert!(wait_until(Duration::from_secs(20), || asked.load(Ordering::SeqCst)));
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    kill_process(pid, Signal::INT).unwrap();
    assert!(
        wait_until(Duration::from_secs(20), || {
            home.records()
                .iter()
                .any(|e| e["type"] == "turn_ended" && e["outcome"]["status"] == "cancelled")
        }),
        "{}",
        home.logs()
    );
    cancelled.store(true, Ordering::SeqCst);
    stdin.write_all(b"second line\n").unwrap();
    drop(stdin);
    let status = wait_for_exit(&mut child);
    let output = child.wait_with_output().unwrap();
    assert!(status.success(), "{}", both(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let outcomes: Vec<_> = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|e| e["type"] == "turn_ended")
        .map(|e| e["outcome"]["status"].clone())
        .collect();
    assert_eq!(outcomes, ["cancelled", "completed"], "{stdout}");
}

#[test]
fn a_failed_turn_prints_its_kind() {
    let server = Server::start(|request| {
        if request.head.starts_with(COUNT) {
            return json("200 OK", r#"{"input_tokens": 10}"#);
        }
        json(
            "401 Unauthorized",
            r#"{"type":"error","error":{"type":"authentication_error","message":"refused by test server"}}"#,
        )
    });
    let home = Home::new("jsonl-auth");
    let output = run(
        home.piped_command(server.port).args(["--output", "jsonl"]),
        "hello\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let turn = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|e| e["type"] == "turn_ended")
        .unwrap_or_else(|| panic!("{}", both(&output)));
    assert_eq!(turn["outcome"]["status"], "error", "{turn}");
    assert_eq!(turn["outcome"]["kind"], "auth", "{turn}");
}

#[test]
fn a_hangup_ignored_at_start_stays_ignored_in_jsonl() {
    let server = replying_server();
    let home = Home::new("nohup");
    let mut command = home.logged(&["nohup", WISP], server.port);
    let mut child = command.args(["--output", "jsonl"]).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"hello\n").unwrap();
    let ended = || home.logs().contains(r#""type":"turn_ended""#);
    assert!(
        wait_until(Duration::from_secs(20), ended),
        "{}",
        home.logs()
    );
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    kill_process(pid, Signal::HUP).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(child.try_wait().unwrap().is_none(), "a hangup ended it");
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", both(&output));
    let records = home.records();
    assert_eq!(records.last().unwrap()["type"], "session_ended");
}
