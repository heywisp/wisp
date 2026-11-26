#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any file, socket or process error"
)]

#[path = "../../../../tests/support/dependencies.rs"]
mod dependencies;
use crate::support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use dependencies::cargo_metadata;
use support::{
    WISP,
    home::{Home, both, command_in, fed},
};

const TESTKIT_TOOLS: &[&str] = &[
    "confirm",
    "echo",
    "fetch",
    "instantly",
    "limited",
    "panic",
    "sleep",
    "snap",
    "starter",
    "stubborn",
];

impl Home {
    fn testkit_command(&self, testkit: Option<&str>) -> Command {
        let mut command = command_in(&self.0, WISP);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(mode) = testkit {
            command.env("WISP_TESTKIT", mode);
        }
        command
    }
}

fn help(testkit: Option<&str>) -> String {
    let home = Home::new("help");
    let output = home
        .testkit_command(testkit)
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", both(&output));
    both(&output)
}

fn value_line<'a>(help: &'a str, tool: &str) -> Option<&'a str> {
    help.lines()
        .find(|line| line.trim_start().starts_with(&format!("- {tool}:")))
}

#[test]
fn without_the_variable_help_names_no_testkit_tool() {
    let help = help(None);
    for tool in TESTKIT_TOOLS {
        assert!(value_line(&help, tool).is_none(), "{tool}\n{help}");
    }
    assert!(!help.contains("Test tools"), "{help}");
}

#[test]
fn help_lists_remember_as_a_default_tool() {
    let help = help(None);
    let remember = value_line(&help, "remember").unwrap();
    assert!(remember.ends_with("on by default in the tui"), "{remember}");
}

#[test]
fn the_inert_testkit_lists_each_tool_as_off_unless_named() {
    let help = help(Some("inert"));
    for tool in TESTKIT_TOOLS {
        let line = value_line(&help, tool).unwrap_or_else(|| panic!("{tool}\n{help}"));
        assert!(line.ends_with("off unless named"), "{line}");
    }
}

#[test]
fn without_the_variable_doctor_shows_no_testkit() {
    let home = Home::new("doctor");
    let output = home.testkit_command(None).arg("doctor").output().unwrap();
    let all = both(&output);
    assert!(all.contains("plugins") || all.contains("tools"), "{all}");
    assert!(!all.contains("testkit"), "{all}");
    assert!(!all.contains("Test tools"), "{all}");
}

fn refused_turn(home: &Home, testkit: Option<&str>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let child = fed(
        home.testkit_command(testkit)
            .args(["--output", "jsonl"])
            .env("ANTHROPIC_API_KEY", "test-key-not-used")
            .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}")),
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
    child.wait_with_output().unwrap();
}

fn started_plugins(testkit: Option<&str>) -> Vec<String> {
    let home = Home::new("started");
    refused_turn(&home, testkit);
    let logs = home.logs();
    let started: Vec<Value> = logs
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["type"] == "session_started")
        .collect();
    assert_eq!(started.len(), 1, "{logs}");
    started[0]["plugins"]
        .as_array()
        .unwrap_or_else(|| panic!("{logs}"))
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn without_the_variable_a_session_records_no_testkit_plugin() {
    let plain = started_plugins(None);
    assert!(!plain.is_empty());
    assert!(!plain.iter().any(|id| id == "testkit"), "{plain:?}");
    let inert = started_plugins(Some("inert"));
    assert!(inert.iter().any(|id| id == "testkit"), "{inert:?}");
}

#[test]
fn only_dev_dependencies_turn_the_testkit_on() {
    let metadata = cargo_metadata(&["--no-deps"]);
    let packages = metadata["packages"].as_array().unwrap();
    let mut seen_plugins = false;
    for package in packages {
        let name = package["name"].as_str().unwrap();
        for dependency in package["dependencies"].as_array().unwrap() {
            let gate = match dependency["name"].as_str() {
                Some("wisp-plugins") => "testkit",
                Some("wisp-core") => "test-support",
                _ => continue,
            };
            let features: Vec<&str> = dependency["features"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f.as_str().unwrap())
                .collect();
            if dependency["kind"].is_null() {
                assert!(!features.contains(&gate), "{name}: {dependency}");
            }
        }
        for (feature, enables) in package["features"].as_object().unwrap() {
            for enabled in enables.as_array().unwrap() {
                let enabled = enabled.as_str().unwrap();
                assert!(
                    !matches!(
                        enabled,
                        "wisp-plugins/testkit"
                            | "wisp-plugins?/testkit"
                            | "wisp-core/test-support"
                            | "wisp-core?/test-support"
                    ),
                    "{name}'s feature {feature} enables {enabled}"
                );
            }
        }
        if name == "wisp-plugins" {
            seen_plugins = true;
            let defaults: Vec<&str> = package["features"]["default"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f.as_str().unwrap())
                .collect();
            assert!(!defaults.contains(&"testkit"), "{defaults:?}");
            assert!(package["features"].get("testkit").is_some());
        }
    }
    assert!(seen_plugins);
}
