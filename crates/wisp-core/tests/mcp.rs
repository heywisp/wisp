#![allow(
    clippy::unwrap_used,
    reason = "local server tests fail on any setup or protocol error"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use wisp_core::config::McpServer;
use wisp_core::mcp::{Service, State, tool_name};
use wisp_core::session::{MemoryStore, SessionLog, SessionStore};
use wisp_core::tool::canonical_specs;
use wisp_protocol::ToolOutcome;
use wisp_protocol::log::LogEvent;

#[path = "../../wisp-tui/tests/support/scratch.rs"]
mod scratch;
use scratch::{ScratchDir, alive};

fn stdio_server(mode: &str) -> McpServer {
    McpServer {
        keys: Vec::new(),
        command: env!("CARGO_BIN_EXE_mcp-test-server").into(),
        args: vec![mode.into()],
    }
}

fn service(scratch: &ScratchDir, name: &str, mode: &str) -> Service {
    Service::new(
        BTreeMap::from([(name.into(), stdio_server(mode))]),
        scratch.root.clone(),
        Some(scratch.root.join("mcp")),
    )
}

async fn exists(scratch: &ScratchDir, file: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !scratch.root.join(file).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn tools_include_every_page_sorted_and_canonical() {
    let scratch = ScratchDir::new("wisp-mcp-", "pages");
    let mcp = service(&scratch, "local", "modern");
    assert_eq!(mcp.rows()[0].state, State::Stopped);
    let list = mcp.list("local").await.unwrap();
    assert_eq!(
        list.tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["a-first", "slow", "z.echo"]
    );
    let canonical = canonical_specs(&list.tools);
    assert_eq!(
        canonical,
        r#"[{"description":"First","input_schema":{"properties":{"a":{"type":"integer"},"z":{"type":"string"}},"type":"object"},"name":"a-first"},{"description":"First","input_schema":{"properties":{"a":{"type":"integer"},"z":{"type":"string"}},"type":"object"},"name":"slow"},{"description":"First","input_schema":{"properties":{"a":{"type":"integer"},"z":{"type":"string"}},"type":"object"},"name":"z.echo"}]"#
    );
    assert!(
        canonical.contains(r#""properties":{"a":{"type":"integer"},"z":{"type":"string"}}"#),
        "{canonical}"
    );
    assert_eq!(list.ttl_ms, Some(5000));
    assert_eq!(mcp.rows()[0].state, State::Ready);
    assert_eq!(mcp.rows()[0].tools, 3);
    assert_eq!(mcp.revision(), 1);
    assert_eq!(
        fs::read_to_string(scratch.root.join("methods"))
            .unwrap()
            .lines()
            .next(),
        Some("server/discover")
    );
    assert!(
        mcp.tools()
            .iter()
            .all(|t| t.name.starts_with("mcp__local__"))
    );
}

#[tokio::test]
async fn a_handshake_era_server_is_reached_after_discovery() {
    let scratch = ScratchDir::new("wisp-mcp-", "legacy");
    let mcp = service(&scratch, "chosen", "legacy");
    mcp.connect("chosen").await.unwrap();
    let methods = fs::read_to_string(scratch.root.join("methods")).unwrap();
    assert!(
        methods.starts_with("server/discover\ninitialize\nnotifications/initialized\n"),
        "{methods}"
    );
    assert!(
        mcp.tools()
            .iter()
            .all(|t| t.name.starts_with("mcp__chosen__"))
    );
    assert!(!canonical_specs(&mcp.tools()).contains("not-the-configured-name"));
}

#[tokio::test]
async fn a_call_preserves_text_attachments_errors_and_current_parameters() {
    let scratch = ScratchDir::new("wisp-mcp-", "call");
    let mcp = service(&scratch, "local", "modern");
    mcp.connect("local").await.unwrap();
    let run = mcp
        .call(
            "mcp__local__z_echo",
            json!({}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = run.result.unwrap();
    assert!(text.starts_with("z.echo\nembedded\n"), "{text}");
    assert!(text.contains(r#""job":"1""#), "{text}");
    assert!(
        text.contains(scratch.root.file_name().unwrap().to_str().unwrap()),
        "{text}"
    );
    assert_eq!(
        run.attachments
            .iter()
            .map(|a| a.bytes.as_slice())
            .collect::<Vec<_>>(),
        [b"x", b"y", b"z"]
    );
    assert_eq!(run.attachments[0].media_type.as_str(), "image/png");
    assert_eq!(run.attachments[1].media_type.as_str(), "audio/wav");
    let run = mcp
        .call(
            "mcp__local__z_echo",
            json!({"fail":true}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        run.outcome,
        ToolOutcome::Completed { is_error: true, .. }
    ));
    let error = mcp
        .call(
            "mcp__local__z_echo",
            json!({"invalid":true}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.contains("current parameters:"), "{error}");
    assert!(error.contains(r#""type":"integer""#), "{error}");

    fs::write(scratch.root.join("structured"), "yes").unwrap();
    let run = mcp
        .call(
            "mcp__local__z_echo",
            json!({"distinct": true}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        run.result.unwrap(),
        concat!(r#"{"summary": true}"#, "\n", r#"{"a":[1],"b":2}"#)
    );
}

#[tokio::test]
async fn cancellation_and_call_deadlines_do_not_include_startup() {
    let scratch = ScratchDir::new("wisp-mcp-", "cancel");
    let mcp = service(&scratch, "local", "modern");
    mcp.connect("local").await.unwrap();
    drop(mcp);
    fs::write(scratch.root.join("delay"), "yes").unwrap();
    let mcp = service(&scratch, "local", "modern");
    mcp.call(
        "mcp__local__z_echo",
        json!({}),
        Duration::from_millis(100),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    let task = {
        let mcp = mcp.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            mcp.call(
                "mcp__local__slow",
                json!({}),
                Duration::from_secs(30),
                cancel,
            )
            .await
        })
    };
    exists(&scratch, "slow-started").await;
    cancel.cancel();
    assert_eq!(
        task.await.unwrap().unwrap_err(),
        wisp_tool_text::mcp::CALL_CANCELLED
    );
    exists(&scratch, "cancelled").await;
    fs::remove_file(scratch.root.join("cancelled")).unwrap();
    let error = mcp
        .call(
            "mcp__local__slow",
            json!({}),
            Duration::from_millis(30),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, "the MCP call timed out");
    exists(&scratch, "cancelled").await;
}

#[tokio::test]
async fn a_saved_list_is_read_without_starting_a_process() {
    let scratch = ScratchDir::new("wisp-mcp-", "saved");
    fs::write(scratch.root.join("large"), "yes").unwrap();
    let first = service(&scratch, "local", "modern");
    let list = first.list("local").await.unwrap();
    drop(first);
    let before = fs::read(scratch.root.join("methods")).unwrap();
    let path = fs::read_dir(scratch.root.join("mcp"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["prompts"][0]["futureField"] = json!({"placeholder":true});
    record["resources"][0]["futureField"] = json!(["placeholder"]);
    fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    let saved = service(&scratch, "local", "modern");
    assert_eq!(saved.prompts().len(), 2);
    assert_eq!(saved.resources().len(), 2);
    assert_eq!(saved.saved("local"), Some(list));
    assert_eq!(saved.rows()[0].state, State::Stopped);
    assert_eq!(fs::read(scratch.root.join("methods")).unwrap(), before);
    let files: Vec<_> = fs::read_dir(scratch.root.join("mcp"))
        .unwrap()
        .flatten()
        .map(|f| f.file_name())
        .collect();
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .all(|name| name.to_str().unwrap().split('.').next().unwrap().len() == 64)
    );
    assert!(
        service(&scratch, "renamed", "modern")
            .saved("renamed")
            .is_none()
    );
    assert!(
        service(&scratch, "local", "different-command-argument")
            .saved("local")
            .is_none()
    );
}

async fn announce_change(mcp: &Service) {
    mcp.call(
        "mcp__local__z_echo",
        json!({"announce":true}),
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_server_that_says_its_list_changed_is_listed_again() {
    for (mode, during_add) in [("modern", false), ("legacy", false), ("modern", true)] {
        let scratch = ScratchDir::new("wisp-mcp-", mode);
        let mcp = Service::new(
            BTreeMap::new(),
            scratch.root.clone(),
            Some(scratch.root.join("mcp")),
        );
        scratch.write("list-changed", "yes");
        if during_add {
            scratch.write("announce-on-list", "yes");
        }
        mcp.add("local", stdio_server(mode)).await.unwrap();
        let before = mcp.revision();
        if !during_add {
            announce_change(&mcp).await;
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while (!during_add && mcp.revision() == before)
                || !mcp.saved("local").is_some_and(|list| {
                    list.tools
                        .iter()
                        .any(|tool| tool.name == "z.echo" && tool.description == "Changed")
                })
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let methods = fs::read_to_string(scratch.root.join("methods")).unwrap();
        let count = |name| methods.lines().filter(|method| *method == name).count();
        assert_eq!(count("server/discover"), 1);
        assert_eq!(count("subscriptions/listen"), usize::from(mode == "modern"));
    }
}

#[tokio::test]
async fn a_new_notice_during_a_failed_relist_starts_another_attempt() {
    let scratch = ScratchDir::new("wisp-mcp-", "failed-relist");
    scratch.write("list-changed", "yes");
    scratch.write("fail-relist", "yes");
    let mcp = service(&scratch, "local", "modern");
    mcp.connect("local").await.unwrap();
    announce_change(&mcp).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !mcp
            .saved("local")
            .is_some_and(|list| list.tools.iter().any(|tool| tool.description == "Changed"))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!scratch.root.join("fail-relist").exists());
}

#[tokio::test]
async fn a_change_before_the_listen_acknowledgment_is_in_the_first_list() {
    let scratch = ScratchDir::new("wisp-mcp-", "change-on-listen");
    scratch.write("list-changed", "yes");
    scratch.write("change-on-listen", "yes");
    let mcp = service(&scratch, "local", "modern");
    let list = mcp.list("local").await.unwrap();
    assert_eq!(list.tools.len(), 3);
    assert!(list.tools.iter().all(|tool| tool.description == "Changed"));
    let methods = fs::read_to_string(scratch.root.join("methods")).unwrap();
    assert!(methods.starts_with("server/discover\nsubscriptions/listen\ntools/list\n"));
    assert_eq!(mcp.revision(), 1);
}

#[tokio::test]
async fn a_changed_list_is_noticed_but_a_timestamp_change_is_not() {
    let scratch = ScratchDir::new("wisp-mcp-", "changed");
    let first = service(&scratch, "local", "modern");
    first.connect("local").await.unwrap();
    drop(first);
    let same = service(&scratch, "local", "modern");
    same.connect("local").await.unwrap();
    assert_eq!(same.revision(), 0);
    let previous_pid = fs::read_to_string(scratch.root.join("server.pid")).unwrap();
    same.call(
        "mcp__local__z_echo",
        json!({"exit":true}),
        Duration::from_secs(2),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    let exited = previous_pid.parse::<i32>().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while alive(exited) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        fs::metadata(format!("/proc/{exited}")).is_ok(),
        "the unreaped child pins its group until teardown"
    );
    fs::write(scratch.root.join("changed"), "yes").unwrap();
    same.connect("local").await.unwrap();
    let replacement_pid = fs::read_to_string(scratch.root.join("server.pid")).unwrap();
    assert_ne!(
        previous_pid, replacement_pid,
        "a closed transport must start a new server"
    );
    assert_eq!(same.revision(), 1);
    let run = same
        .call(
            "mcp__local__z_echo",
            json!({}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(run.result.unwrap().starts_with("z.echo\n"));
    assert!(
        same.saved("local")
            .unwrap()
            .tools
            .iter()
            .all(|t| t.description == "Changed")
    );
    drop(same);
    assert!(
        service(&scratch, "local", "modern")
            .saved("local")
            .unwrap()
            .tools
            .iter()
            .all(|t| t.description == "Changed")
    );
}

#[tokio::test]
async fn a_server_that_cannot_start_has_one_safe_failure_sentence() {
    let scratch = ScratchDir::new("wisp-mcp-", "failure");
    let mcp = Service::new(
        BTreeMap::from([(
            "missing".into(),
            McpServer {
                keys: Vec::new(),
                command: scratch
                    .root
                    .join("not-an-executable")
                    .to_string_lossy()
                    .into(),
                args: vec!["private-argument".into()],
            },
        )]),
        scratch.root.clone(),
        None,
    );
    let error = mcp.connect("missing").await.unwrap_err();
    assert_eq!(
        error,
        "missing couldn't start: not-an-executable isn't installed"
    );
    assert_eq!(mcp.rows()[0].state, State::Failed(error));
    assert!(mcp.saved("missing").is_none());
    let closed = service(&scratch, "closed", "exit");
    assert_eq!(
        closed.connect("closed").await.unwrap_err(),
        "closed couldn't start: did not answer the MCP startup request"
    );
}

#[tokio::test]
async fn mapped_names_round_trip_and_provider_invalid_names_are_refused() {
    assert_eq!(
        tool_name("user.choice", "z.echo/next"),
        "mcp__user_choice__z_echo_next"
    );
    let scratch = ScratchDir::new("wisp-mcp-", "names");
    let mcp = service(&scratch, "user.choice", "modern");
    mcp.connect("user.choice").await.unwrap();
    let run = mcp
        .call(
            "mcp__user_choice__z_echo",
            json!({}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(run.result.unwrap().starts_with("z.echo\n"));
    let long = "x".repeat(64);
    let invalid = service(&scratch, &long, "modern");
    assert!(
        invalid
            .connect(&long)
            .await
            .unwrap_err()
            .contains("cannot be mapped uniquely")
    );
    assert!(invalid.tools().is_empty());
    let collision = service(&scratch, "local", "collision");
    assert!(
        collision
            .connect("local")
            .await
            .unwrap_err()
            .contains("cannot be mapped uniquely")
    );
    assert!(collision.tools().is_empty());
    let config = McpServer {
        keys: Vec::new(),
        command: "unused".into(),
        args: vec![],
    };
    let overlap = Service::new(
        BTreeMap::from([("a.b".into(), config.clone()), ("a_b".into(), config)]),
        scratch.root.clone(),
        None,
    );
    assert!(
        overlap
            .connect("a.b")
            .await
            .unwrap_err()
            .contains("overlaps after mapping")
    );
    assert!(
        overlap
            .rows()
            .iter()
            .all(|r| matches!(r.state, State::Failed(_)))
    );
    assert!(overlap.tools().is_empty());
    let config = McpServer {
        keys: Vec::new(),
        command: env!("CARGO_BIN_EXE_mcp-test-server").into(),
        args: vec!["separator".into()],
    };
    let delimiters = Service::new(
        BTreeMap::from([("a".into(), config.clone()), ("a__z".into(), config)]),
        scratch.root.clone(),
        None,
    );
    delimiters.connect("a").await.unwrap();
    assert!(
        delimiters
            .connect("a__z")
            .await
            .unwrap_err()
            .contains("overlapping mapped tool names")
    );
    assert_eq!(delimiters.tools().len(), 3);
}

#[tokio::test]
async fn the_last_session_tree_handle_stops_the_whole_process_group() {
    for mode in ["modern", "legacy"] {
        let scratch = ScratchDir::new("wisp-mcp-", "drop");
        scratch.write("descendant", "yes");
        scratch.write("list-changed", "yes");
        scratch.write("hang-relist", "yes");
        let mcp = service(&scratch, "local", mode);
        mcp.connect("local").await.unwrap();
        let child = mcp.clone();
        let server: i32 = fs::read_to_string(scratch.root.join("server.pid"))
            .unwrap()
            .parse()
            .unwrap();
        let descendant: i32 = fs::read_to_string(scratch.root.join("child.pid"))
            .unwrap()
            .parse()
            .unwrap();
        announce_change(&mcp).await;
        exists(&scratch, "relist-pending").await;
        assert!(alive(server) && alive(descendant));
        drop(mcp);
        assert!(alive(server) && alive(descendant));
        drop(child);
        tokio::time::timeout(Duration::from_secs(10), async {
            while alive(server) || alive(descendant) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn an_add_and_remove_are_shared_persisted_and_leave_only_routing_names() {
    let scratch = ScratchDir::new("wisp-mcp-", "mutating");
    let config = scratch.root.join("config.toml");
    let folder = scratch.root.join("mcp");
    let mcp = Service::new(BTreeMap::new(), scratch.root.clone(), Some(folder.clone()))
        .with_config(config.clone(), wisp_core::endpoints::PathShown::Given);
    let child = mcp.clone();
    let spec = McpServer {
        keys: Vec::new(),
        command: env!("CARGO_BIN_EXE_mcp-test-server").into(),
        args: vec!["text".into(), "an argument with spaces".into()],
    };
    assert_eq!(mcp.add("local", spec.clone()).await.unwrap(), 3);
    assert_eq!(child.rows()[0].state, State::Ready);
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("[mcp.servers.local]"));
    assert!(text.contains("an argument with spaces"));
    let pid = fs::read_to_string(scratch.root.join("server.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(alive(pid));
    child.remove("local").await.unwrap();
    assert!(mcp.rows().is_empty());
    tokio::time::timeout(Duration::from_secs(10), async {
        while alive(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !fs::read_to_string(config)
            .unwrap()
            .contains("[mcp.servers.local]")
    );
    assert!(fs::read_dir(folder).unwrap().all(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_none_or(|ext| ext != "json")
    }));
    let error = mcp
        .call(
            "mcp__local__z_echo",
            json!({}),
            Duration::from_secs(1),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, "local was removed");
    assert_eq!(mcp.revision(), 2);
}

#[tokio::test]
async fn adding_a_secret_starts_and_saves_nothing_and_never_repeats_the_value() {
    let scratch = ScratchDir::new("wisp-mcp-", "secrets");
    let config = scratch.root.join("config.toml");
    let mcp = Service::new(
        BTreeMap::new(),
        scratch.root.clone(),
        Some(scratch.root.join("mcp")),
    )
    .with_config(config.clone(), wisp_core::endpoints::PathShown::Given);
    for args in [
        vec!["--token", "CANARYVALUE"],
        vec!["API_KEY=CANARYVALUE"],
        vec!["token=CANARYVALUE"],
        vec!["--header", "Authorization: Bearer CANARYVALUE"],
        vec!["-H", "X-Api-Key: CANARYVALUE"],
        vec!["Bearer CANARYVALUE"],
        vec!["sk-ant-api03-CANARYVALUE0000"],
        vec!["ARG=sk-ant-api03-CANARYVALUE0000"],
        vec!["ghp_CANARY01234567890123456789"],
    ] {
        let error = mcp
            .add(
                "local",
                McpServer {
                    keys: Vec::new(),
                    command: env!("CARGO_BIN_EXE_mcp-test-server").into(),
                    args: args.into_iter().map(str::to_owned).collect(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "a key can't be saved here; when an agent adds the server with the key's name in keys, Wisp asks the user for it"
        );
        assert!(!error.contains("CANARY"));
        assert!(!config.exists());
        assert!(!scratch.root.join("server.pid").exists());
    }
}

#[tokio::test]
async fn a_gone_tool_and_a_failed_start_use_the_approved_sentences() {
    let scratch = ScratchDir::new("wisp-mcp-", "gone");
    let mcp = service(&scratch, "local", "text");
    mcp.connect("local").await.unwrap();
    let _ = mcp
        .call(
            "mcp__local__z_echo",
            json!({"exit":true}),
            Duration::from_secs(1),
            CancellationToken::new(),
        )
        .await;
    fs::write(scratch.root.join("gone"), "yes").unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let error = mcp
        .call(
            "mcp__local__z_echo",
            json!({}),
            Duration::from_secs(1),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, "local no longer has z.echo");
    let missing = Service::new(
        BTreeMap::from([(
            "github".into(),
            McpServer {
                keys: Vec::new(),
                command: "wisp-missing-mcp-server".into(),
                args: Vec::new(),
            },
        )]),
        scratch.root.clone(),
        None,
    );
    assert_eq!(
        missing.connect("github").await.unwrap_err(),
        "github couldn't start: wisp-missing-mcp-server isn't installed"
    );
}

#[tokio::test]
async fn a_local_servers_cold_and_warm_start_are_recorded_without_network() {
    let scratch = ScratchDir::new("wisp-mcp-", "start-time");
    let mcp = service(&scratch, "local", "text");
    let now = std::time::Instant::now();
    mcp.connect("local").await.unwrap();
    let cold = now.elapsed();
    let first = fs::read_to_string(scratch.root.join("methods")).unwrap();
    let now = std::time::Instant::now();
    mcp.connect("local").await.unwrap();
    let warm = now.elapsed();
    eprintln!("local MCP cold {cold:?}; warm {warm:?}");
    assert_eq!(
        fs::read_to_string(scratch.root.join("methods")).unwrap(),
        first
    );
    assert!(cold < Duration::from_secs(10) && warm < Duration::from_secs(10));
}

#[path = "support/mcp-http.rs"]
mod http;

async fn http_service(scratch: &ScratchDir, server: &http::Server) -> Service {
    let service = Service::new(
        BTreeMap::new(),
        scratch.root.clone(),
        Some(scratch.root.join("mcp")),
    )
    .with_config(
        scratch.root.join("config.toml"),
        wisp_core::endpoints::PathShown::Given,
    );
    let count = service
        .add(
            "local",
            McpServer {
                keys: Vec::new(),
                command: server.address(),
                args: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        count,
        if service.rows()[0].state == State::NeedsSignIn {
            0
        } else {
            1
        }
    );
    service
}

#[tokio::test]
async fn a_protected_http_listen_does_not_require_sign_in_for_tools() {
    let scratch = ScratchDir::new("wisp-mcp-", "protected-listen");
    let server = http::Server::start(false, None).await;
    server.protect_listen();
    let mcp = http_service(&scratch, &server).await;
    assert_eq!(mcp.rows()[0].state, State::Ready);
    assert_eq!(mcp.saved("local").unwrap().tools.len(), 1);
    mcp.call(
        "mcp__local__echo",
        json!({}),
        Duration::from_secs(2),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(mcp.rows()[0].state, State::Ready);
    server.requests(|requests| {
        let count = |name| {
            requests
                .iter()
                .filter(|request| request.body["method"] == name)
                .count()
        };
        assert_eq!(count("server/discover"), 1);
        assert_eq!(count("subscriptions/listen"), 1);
        assert_eq!(count("tools/list"), 1);
        assert_eq!(count("tools/call"), 1);
    });
}

#[tokio::test]
async fn http_sign_in_by_a_typed_address_is_scoped_saved_and_reloadable() {
    let scratch = ScratchDir::new("wisp-mcp-", "http-manual");
    let server = http::Server::start(true, None).await;
    let service = http_service(&scratch, &server).await;
    assert_eq!(service.rows()[0].state, State::NeedsSignIn);
    let link = service.sign_in_start("local").await.unwrap().unwrap();
    assert_eq!(
        service.sign_in_start("local").await.unwrap().as_deref(),
        Some(link.as_str())
    );
    let wrong = service
        .sign_in_finish(
            "local",
            &http::redirected(&link, Some("CANARY-wrong-state")),
        )
        .await
        .unwrap_err();
    assert_eq!(wrong, "that address is from another sign-in");
    assert_eq!(
        service
            .sign_in_finish("local", "https://example.invalid/not-a-redirect")
            .await
            .unwrap_err(),
        "copy the final address from the sign-in page"
    );
    let mut missing_code = reqwest::Url::parse(&http::redirected(&link, None)).unwrap();
    let query: Vec<_> = missing_code
        .query_pairs()
        .filter(|(name, _)| name != "code")
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    missing_code.query_pairs_mut().clear().extend_pairs(query);
    assert_eq!(
        service
            .sign_in_finish("local", missing_code.as_str())
            .await
            .unwrap_err(),
        "copy the final address from the sign-in page"
    );
    assert!(!wrong.contains(http::CODE));
    assert!(service.take_ended().is_empty());
    assert_eq!(
        service
            .sign_in_finish("local", &http::redirected(&link, None))
            .await
            .unwrap(),
        1
    );
    let ends = service.take_ended();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].line, "local signed in · 1 tool");
    assert!(service.take_ended().is_empty());
    assert_eq!(service.rows()[0].state, State::Stopped);
    assert_eq!(service.tools()[0].name, "mcp__local__echo");
    assert!(service.sign_in_start("local").await.unwrap().is_none());
    let config = fs::read_to_string(scratch.root.join("config.toml")).unwrap();
    assert!(
        [
            http::CLIENT,
            http::CLIENT_SECRET,
            http::ACCESS,
            http::REFRESH,
            http::CODE
        ]
        .iter()
        .all(|private| !config.contains(private))
    );
    server.requests(|requests| {
        let registration = &requests
            .iter()
            .find(|request| request.path == "/register")
            .unwrap()
            .body;
        assert_eq!(registration["client_name"], "Wisp");
        assert_eq!(registration["redirect_uris"].as_array().unwrap().len(), 1);
        assert!(registration.get("login_hint").is_none());
        assert!(requests.iter().any(
            |request| request.path == "/token" && request.headers.contains_key("authorization")
        ));
    });
    drop(service);
    let loaded = Service::new(
        BTreeMap::from([(
            "local".into(),
            McpServer {
                keys: Vec::new(),
                command: server.address(),
                args: Vec::new(),
            },
        )]),
        scratch.root.clone(),
        Some(scratch.root.join("mcp")),
    )
    .with_config(
        scratch.root.join("config.toml"),
        wisp_core::endpoints::PathShown::Given,
    );
    loaded.connect("local").await.unwrap();
    assert_eq!(loaded.rows()[0].state, State::Ready);
    loaded.remove("local").await.unwrap();
    assert_eq!(private_profiles(&scratch.root.join("oauth")), 0);
}

fn private_profiles(folder: &std::path::Path) -> usize {
    fs::read_dir(folder)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                private_profiles(&path)
            } else {
                usize::from(path.extension().is_some_and(|ext| ext == "json"))
            }
        })
        .sum()
}

#[tokio::test]
async fn http_loopback_sign_in_saves_the_catalog_and_records_its_end_once() {
    let scratch = ScratchDir::new("wisp-mcp-", "http-loopback");
    let server = http::Server::start(true, None).await;
    let service = http_service(&scratch, &server).await;
    let before = service.revision();
    let link = service.sign_in_start("local").await.unwrap().unwrap();
    service.await_sign_in("local");
    let page = reqwest::get(http::redirected(&link, None))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(page, "Signed in to local. You can close this page.");
    let ends = service.take_ended();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].line, "local signed in · 1 tool");
    assert!(ends[0].awaited);
    assert!(service.take_ended().is_empty());
    assert_eq!(service.rows()[0].state, State::Stopped);
    assert_eq!(service.revision(), before + 1);
    assert_eq!(service.saved("local").unwrap().tools.len(), 1);
    for secret in [
        http::CLIENT,
        http::CLIENT_SECRET,
        http::ACCESS,
        http::REFRESH,
        http::CODE,
    ] {
        assert!(!page.contains(secret));
    }
    assert!(
        fs::read_dir(scratch.root.join("mcp"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "json"))
    );
}

#[tokio::test]
async fn a_server_that_fails_to_start_after_the_exchange_is_not_called_not_signed_in() {
    let scratch = ScratchDir::new("wisp-mcp-", "http-start-failed");
    let server = http::Server::start(true, None).await;
    let service = http_service(&scratch, &server).await;
    let link = service.sign_in_start("local").await.unwrap().unwrap();
    server.down();
    let error = service
        .sign_in_finish("local", &http::redirected(&link, None))
        .await
        .unwrap_err();
    assert!(error.starts_with("local couldn't start: "), "{error}");
    let ends = service.take_ended();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].line, error);
    assert!(service.take_ended().is_empty());
}

#[tokio::test]
async fn another_server_naming_the_same_issuer_never_receives_the_first_token() {
    let scratch = ScratchDir::new("wisp-mcp-", "http-isolation");
    let issuer = http::Server::start(true, None).await;
    let service = http_service(&scratch, &issuer).await;
    let link = service.sign_in_start("local").await.unwrap().unwrap();
    service
        .sign_in_finish("local", &http::redirected(&link, None))
        .await
        .unwrap();
    let other = http::Server::start(true, Some(issuer.issuer())).await;
    let loaded = Service::new(
        BTreeMap::from([(
            "other".into(),
            McpServer {
                keys: Vec::new(),
                command: other.address(),
                args: Vec::new(),
            },
        )]),
        scratch.root.clone(),
        None,
    )
    .with_config(
        scratch.root.join("config.toml"),
        wisp_core::endpoints::PathShown::Given,
    );
    assert!(loaded.connect("other").await.is_err());
    assert_eq!(loaded.rows()[0].state, State::NeedsSignIn);
    other.requests(|requests| {
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("authorization"))
        )
    });
}

#[tokio::test]
async fn an_expired_http_sign_in_is_replaced_and_removing_it_closes_the_listener() {
    let scratch = ScratchDir::new("wisp-mcp-", "http-expired");
    let server = http::Server::start(true, None).await;
    let service = http_service(&scratch, &server).await;
    let first = service.sign_in_start("local").await.unwrap().unwrap();
    service.await_sign_in("local");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(601)).await;
    tokio::time::resume();
    let expired = service
        .sign_in_finish("local", &http::redirected(&first, None))
        .await
        .unwrap_err();
    assert!(
        matches!(
            expired.as_str(),
            "sign-in expired; sign in again" | "start with /mcp sign-in local"
        ),
        "{expired}"
    );
    assert!(service.take_ended().is_empty());
    let link = service.sign_in_start("local").await.unwrap().unwrap();
    assert_ne!(first, link);
    let callback = reqwest::Url::parse(&http::redirected(&link, None)).unwrap();
    let address = ("127.0.0.1", callback.port().unwrap());
    let one = TcpStream::connect(address).await.unwrap();
    let other = TcpStream::connect(address).await.unwrap();
    service.remove("local").await.unwrap();
    let (one_reply, other_reply) = tokio::join!(
        read_callback_reply(one, &[]),
        read_callback_reply(other, &[]),
    );
    assert!(
        one_reply.is_empty() && other_reply.is_empty(),
        "the removed sign-in answered its callback"
    );
    assert!(service.rows().is_empty());
    assert_eq!(private_profiles(&scratch.root.join("oauth")), 0);
}

#[tokio::test]
async fn an_http_argument_refusal_relists_before_returning_the_current_schema() {
    let scratch = ScratchDir::new("wisp-mcp-", "http-schema");
    let server = http::Server::start(false, None).await;
    let service = http_service(&scratch, &server).await;
    assert_eq!(
        service.saved("local").unwrap().tools[0].input_schema["properties"]["version"]["const"],
        1
    );
    server.version(2);
    let error = service
        .call(
            "mcp__local__echo",
            json!({"invalid":true}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.contains("\"const\":2"), "{error}");
    assert_eq!(
        service.saved("local").unwrap().tools[0].input_schema["properties"]["version"]["const"],
        2
    );
    let refusal = service
        .call(
            "mcp__local__echo",
            json!({"ask":true}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(refusal, "the server asked for input Wisp can't show");
}

fn another_http_service(scratch: &ScratchDir, server: &http::Server) -> Service {
    Service::new(
        BTreeMap::from([(
            "local".into(),
            McpServer {
                keys: Vec::new(),
                command: server.address(),
                args: Vec::new(),
            },
        )]),
        scratch.root.clone(),
        Some(scratch.root.join("mcp")),
    )
    .with_config(
        scratch.root.join("config.toml"),
        wisp_core::endpoints::PathShown::Given,
    )
}

async fn read_callback_reply(mut connection: TcpStream, request: &[u8]) -> Vec<u8> {
    let mut reply = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = connection.write_all(request).await;
        connection.read_to_end(&mut reply).await
    })
    .await;
    assert!(
        read.is_ok(),
        "the callback listener kept its connection open"
    );
    if let Ok(Err(error)) = read {
        assert_eq!(error.kind(), ErrorKind::ConnectionReset);
        assert!(reply.is_empty(), "the callback reset after sending a reply");
    }
    reply
}

#[tokio::test]
async fn a_declined_http_sign_in_drops_its_listener_and_retries_with_a_live_link() {
    for browser in [false, true] {
        let scratch = ScratchDir::new("wisp-mcp-", "declined");
        let server = http::Server::start(true, None).await;
        let service = http_service(&scratch, &server).await;
        let first = service.sign_in_start("local").await.unwrap().unwrap();
        if !browser {
            service.await_sign_in("local");
        }
        let mut wrong =
            reqwest::Url::parse(&http::redirected(&first, Some("CANARY-wrong-state"))).unwrap();
        wrong
            .query_pairs_mut()
            .append_pair("error", "access_denied");
        assert_eq!(
            service
                .sign_in_finish("local", wrong.as_str())
                .await
                .unwrap_err(),
            "that address is from another sign-in"
        );
        assert!(service.take_ended().is_empty());
        assert_eq!(
            service.sign_in_start("local").await.unwrap().as_deref(),
            Some(first.as_str())
        );
        let mut decline = reqwest::Url::parse(&http::redirected(&first, None)).unwrap();
        let state = decline
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        decline
            .query_pairs_mut()
            .clear()
            .append_pair("state", &state)
            .append_pair("error", "access_denied");
        let callback = ("127.0.0.1", decline.port().unwrap());
        let original_connection = TcpStream::connect(callback).await.unwrap();
        if browser {
            let second_connection = TcpStream::connect(callback).await.unwrap();
            let request = format!(
                "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                decline.path(),
                decline.query().unwrap(),
                callback.1
            );
            let (first_reply, second_reply) = tokio::join!(
                read_callback_reply(original_connection, request.as_bytes()),
                read_callback_reply(second_connection, request.as_bytes())
            );
            let reply = match (first_reply.as_slice(), second_reply.as_slice()) {
                ([], reply) | (reply, []) if !reply.is_empty() => reply,
                _ => panic!("expected exactly one browser decline reply"),
            };
            let (_, page) = str::from_utf8(reply)
                .unwrap()
                .split_once("\r\n\r\n")
                .unwrap();
            assert_eq!(page, "Sign-in declined. Return to Wisp.");
        } else {
            assert_eq!(
                service
                    .sign_in_finish("local", decline.as_str())
                    .await
                    .unwrap_err(),
                "sign-in was declined"
            );
            assert!(
                read_callback_reply(original_connection, b"\r\n")
                    .await
                    .is_empty(),
                "the declined listener answered on the original connection"
            );
        }
        let ends = service.take_ended();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].line, "local not signed in: sign-in was declined");
        assert_eq!(ends[0].awaited, !browser);
        assert!(service.take_ended().is_empty());
        assert_eq!(
            service
                .sign_in_finish("local", decline.as_str())
                .await
                .unwrap_err(),
            "start with /mcp sign-in local"
        );
        assert!(service.take_ended().is_empty());
        let next = service.sign_in_start("local").await.unwrap().unwrap();
        assert_ne!(first, next);
        let page = tokio::time::timeout(Duration::from_secs(10), async {
            reqwest::get(http::redirected(&next, None))
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(page, "Signed in to local. You can close this page.");
    }
}

#[tokio::test]
async fn a_revoked_running_server_needs_sign_in_and_can_authorize_again() {
    let scratch = ScratchDir::new("wisp-mcp-", "revoked");
    let server = http::Server::start(true, None).await;
    let service = http_service(&scratch, &server).await;
    let link = service.sign_in_start("local").await.unwrap().unwrap();
    service
        .sign_in_finish("local", &http::redirected(&link, None))
        .await
        .unwrap();
    service.connect("local").await.unwrap();
    server.revoke();
    assert!(
        service
            .call(
                "mcp__local__echo",
                json!({}),
                Duration::from_secs(2),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(service.rows()[0].state, State::NeedsSignIn);
    let next = service.sign_in_start("local").await.unwrap().unwrap();
    service
        .sign_in_finish("local", &http::redirected(&next, None))
        .await
        .unwrap();
    service.connect("local").await.unwrap();
    assert_eq!(service.rows()[0].state, State::Ready);
}

#[tokio::test]
async fn another_host_cannot_recreate_credentials_for_a_removed_server() {
    for browser in [false, true] {
        let scratch = ScratchDir::new("wisp-mcp-", "removed-other-host");
        let server = http::Server::start(true, None).await;
        let first = http_service(&scratch, &server).await;
        let second = another_http_service(&scratch, &server);
        let link = second.sign_in_start("local").await.unwrap().unwrap();
        first.remove("local").await.unwrap();
        assert_eq!(private_profiles(&scratch.root.join("oauth")), 0);
        if browser {
            let page = reqwest::get(http::redirected(&link, None))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            assert_eq!(page, "Couldn't finish the sign-in. Return to Wisp.");
        } else {
            assert!(
                second
                    .sign_in_finish("local", &http::redirected(&link, None))
                    .await
                    .is_err()
            );
        }
        let ends = second.take_ended();
        assert_eq!(ends.len(), 1);
        assert_eq!(
            ends[0].line,
            "local not signed in: couldn't finish the sign-in"
        );
        assert!(second.take_ended().is_empty());
        assert_eq!(private_profiles(&scratch.root.join("oauth")), 0);
        assert!(
            !fs::read_to_string(scratch.root.join("config.toml"))
                .unwrap()
                .contains("mcp.servers.local")
        );
        assert!(second.sign_in_start("local").await.is_err());
        assert_eq!(private_profiles(&scratch.root.join("oauth")), 0);
    }
}

#[tokio::test]
async fn an_older_registration_cannot_replace_a_newer_hosts_sign_in() {
    let scratch = ScratchDir::new("wisp-mcp-", "replaced-registration");
    let server = http::Server::start(true, None).await;
    let first = http_service(&scratch, &server).await;
    let second = another_http_service(&scratch, &server);
    let older = first.sign_in_start("local").await.unwrap().unwrap();
    let newer = second.sign_in_start("local").await.unwrap().unwrap();
    assert!(
        first
            .sign_in_finish("local", &http::redirected(&older, None))
            .await
            .is_err()
    );
    assert_eq!(
        second
            .sign_in_finish("local", &http::redirected(&newer, None))
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn insecure_sign_in_addresses_and_discovered_endpoints_send_no_credentials() {
    let scratch = ScratchDir::new("wisp-mcp-", "insecure");
    let server = http::Server::start(true, None).await;
    let service = http_service(&scratch, &server).await;
    server.plain_endpoints();
    assert_eq!(
        service.sign_in_start("local").await.unwrap_err(),
        "use an https address, or one on this computer, to sign in"
    );
    server.requests(|requests| {
        assert!(
            requests
                .iter()
                .all(|request| request.path != "/register" && request.path != "/token")
        )
    });
    let service = Service::new(
        BTreeMap::from([(
            "local".into(),
            McpServer {
                keys: Vec::new(),
                command: "http://server.example.invalid/mcp".into(),
                args: Vec::new(),
            },
        )]),
        scratch.root.clone(),
        None,
    );
    assert_eq!(
        service.sign_in_start("local").await.unwrap_err(),
        "use an https address, or one on this computer, to sign in"
    );
}

#[tokio::test]
async fn prompt_and_resource_catalogs_page_sort_and_follow_capabilities() {
    for mode in [
        "modern",
        "legacy",
        "tools-only",
        "undeclared-tools",
        "metadata-only",
    ] {
        let scratch = ScratchDir::new("wisp-mcp-", mode);
        let mcp = service(&scratch, "local", mode);
        let list = mcp.list("local").await.unwrap();
        assert_eq!(
            list.tools.len(),
            if mode == "metadata-only" { 0 } else { 3 }
        );
        let methods = fs::read_to_string(scratch.root.join("methods")).unwrap();
        assert!(methods.contains("tools/list"));
        if mode == "tools-only" {
            assert!(list.prompts.is_empty() && list.resources.is_empty());
            assert!(!methods.contains("prompts/list") && !methods.contains("resources/list"));
        } else {
            assert_eq!(
                list.prompts
                    .iter()
                    .map(|prompt| prompt.name.as_str())
                    .collect::<Vec<_>>(),
                ["review-required", "review.code"]
            );
            assert_eq!(
                list.resources
                    .iter()
                    .map(|resource| resource.uri.as_str())
                    .collect::<Vec<_>>(),
                ["test:///blob", "test:///notes"]
            );
            assert_eq!(
                methods
                    .lines()
                    .filter(|method| *method == "prompts/list")
                    .count(),
                2
            );
            assert_eq!(
                methods
                    .lines()
                    .filter(|method| *method == "resources/list")
                    .count(),
                2
            );
            let prompts = mcp.prompts();
            assert_eq!(prompts[0].name, "mcp__local__prompt:review-required");
            assert!(prompts[0].arguments[0].required);
            assert!(!prompts[1].arguments[0].required);
            assert_eq!(mcp.resources()[1].title, "Notes");
        }
    }
    for family in ["prompts", "resources"] {
        let scratch = ScratchDir::new("wisp-mcp-", family);
        fs::write(scratch.root.join(format!("repeat-{family}")), "yes").unwrap();
        let mcp = service(&scratch, "local", "modern");
        let list = mcp.list("local").await.unwrap();
        assert_eq!(mcp.rows()[0].state, State::Ready);
        assert_eq!(list.tools.len(), 3);
        assert_eq!(list.prompts.is_empty(), family == "prompts");
        assert_eq!(list.resources.is_empty(), family == "resources");
    }
}

#[tokio::test]
async fn prompt_and_resource_reads_keep_roles_original_identifiers_and_non_text_parts() {
    let scratch = ScratchDir::new("wisp-mcp-", "entry-reads");
    let first = service(&scratch, "local", "modern");
    first.connect("local").await.unwrap();
    drop(first);
    let mcp = service(&scratch, "local", "modern");
    assert_eq!(mcp.rows()[0].state, State::Stopped);
    let prompt = "mcp__local__prompt:review.code";
    let resource = "mcp__local__resource:test:///notes";
    let messages = mcp.get_prompt(prompt).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role.as_str(), message.text.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("user", Some("Placeholder prompt text.")),
            ("assistant", Some("Placeholder assistant text."))
        ]
    );
    assert_eq!(mcp.rows()[0].state, State::Ready);
    assert_eq!(
        mcp.read_resource(resource).await.unwrap(),
        [Some("Placeholder resource text.".into())]
    );
    assert_eq!(
        mcp.read_resource("mcp__local__resource:test:///blob")
            .await
            .unwrap(),
        [None]
    );
    fs::write(scratch.root.join("prompt-binary"), "yes").unwrap();
    assert_eq!(mcp.get_prompt(prompt).await.unwrap()[1].text, None);
    fs::write(scratch.root.join("resource-mixed"), "yes").unwrap();
    assert_eq!(
        mcp.read_resource(resource).await.unwrap(),
        [Some("Placeholder resource text.".into()), None]
    );
    fs::write(scratch.root.join("read-fails"), "yes").unwrap();
    assert_eq!(
        mcp.get_prompt(prompt).await.unwrap_err(),
        "local could not read the prompt"
    );
    assert_eq!(
        mcp.read_resource(resource).await.unwrap_err(),
        "local could not read the resource"
    );
    assert_eq!(
        mcp.get_prompt("missing").await.unwrap_err(),
        "missing is no longer available"
    );
    drop(mcp);
    fs::remove_file(scratch.root.join("read-fails")).unwrap();
    fs::write(scratch.root.join("gone-prompts"), "yes").unwrap();
    let changed = service(&scratch, "local", "modern");
    assert_eq!(
        changed.get_prompt(prompt).await.unwrap_err(),
        "local no longer has review.code"
    );
    drop(changed);
    fs::write(scratch.root.join("gone-resources"), "yes").unwrap();
    let changed = service(&scratch, "local", "modern");
    assert_eq!(
        changed.read_resource(resource).await.unwrap_err(),
        "local no longer has test:///notes"
    );
}

#[tokio::test]
async fn optional_listing_failures_keep_tools_and_good_saved_metadata_until_refresh() {
    for family in ["prompts", "resources"] {
        let scratch = ScratchDir::new("wisp-mcp-", family);
        let first = service(&scratch, "local", "modern");
        first.connect("local").await.unwrap();
        let tools = canonical_specs(&first.saved("local").unwrap().tools);
        drop(first);
        fs::write(scratch.root.join(format!("{family}-changed")), "yes").unwrap();
        let changed = service(&scratch, "local", "modern");
        changed.connect("local").await.unwrap();
        assert_eq!(changed.revision(), 1);
        assert_eq!(
            canonical_specs(&changed.saved("local").unwrap().tools),
            tools
        );
        let good = changed.saved("local").unwrap();
        assert!(if family == "prompts" {
            good.prompts
                .iter()
                .any(|prompt| prompt.description == "Changed prompt")
        } else {
            good.resources
                .iter()
                .any(|resource| resource.description == "Changed resource")
        });
        drop(changed);
        let path = fs::read_dir(scratch.root.join("mcp"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "json"))
            .unwrap();
        let bytes = fs::read(&path).unwrap();
        let failure = scratch.root.join(format!("fail-{family}"));
        fs::write(&failure, "yes").unwrap();
        let partial = service(&scratch, "local", "modern");
        partial.connect("local").await.unwrap();
        assert_eq!(partial.rows()[0].state, State::Ready);
        let list = partial.saved("local").unwrap();
        assert_eq!(list.prompts.is_empty(), family == "prompts");
        assert_eq!(list.resources.is_empty(), family == "resources");
        assert_eq!(canonical_specs(&list.tools), tools);
        assert!(
            partial
                .call(
                    "mcp__local__z_echo",
                    json!({}),
                    Duration::from_secs(2),
                    CancellationToken::new()
                )
                .await
                .unwrap()
                .result
                .is_some()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        drop(partial);
        fs::remove_file(failure).unwrap();
        let restored = service(&scratch, "local", "modern");
        assert_eq!(restored.saved("local"), Some(good.clone()));
        restored.connect("local").await.unwrap();
        assert_eq!(restored.revision(), 0);
        assert_eq!(restored.saved("local").unwrap().prompts, good.prompts);
        assert_eq!(restored.saved("local").unwrap().resources, good.resources);
    }
}

#[tokio::test]
async fn optional_lists_that_never_answer_do_not_block_tools() {
    for family in ["prompts", "resources", "listen"] {
        let scratch = ScratchDir::new("wisp-mcp-", family);
        scratch.write(&format!("hang-{family}"), "yes");
        if family == "listen" {
            scratch.write("list-changed", "yes");
        }
        let mcp = service(&scratch, "local", "modern");
        tokio::time::timeout(Duration::from_secs(15), mcp.connect("local"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mcp.rows()[0].state, State::Ready);
        assert_eq!(mcp.tools().len(), 3);
        let list = mcp.saved("local").unwrap();
        assert_eq!(list.prompts.is_empty(), family == "prompts");
        assert_eq!(list.resources.is_empty(), family == "resources");
        let persisted = fs::read_dir(scratch.root.join("mcp"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "json"));
        assert_eq!(persisted, family == "listen");
        if family == "listen" {
            exists(&scratch, "cancelled").await;
        }
        assert!(
            mcp.call(
                "mcp__local__z_echo",
                json!({}),
                Duration::from_secs(2),
                CancellationToken::new()
            )
            .await
            .unwrap()
            .result
            .is_some()
        );
    }
}

const KEY: &str = "CANARY-server-key-0123456789";

fn key_spec() -> McpServer {
    McpServer {
        command: env!("CARGO_BIN_EXE_mcp-test-server").into(),
        args: vec!["key".into()],
        keys: vec!["WISP_TEST_TOKEN".into()],
    }
}

fn key_service(scratch: &ScratchDir, servers: BTreeMap<String, McpServer>) -> Service {
    Service::new(
        servers,
        scratch.root.clone(),
        Some(scratch.root.join("mcp")),
    )
    .with_config(
        scratch.root.join("config.toml"),
        wisp_core::endpoints::PathShown::Given,
    )
    .with_store(wisp_core::secrets::StoreKind::File)
}

fn typed_key(value: &str) -> BTreeMap<String, wisp_protocol::Secret> {
    BTreeMap::from([("WISP_TEST_TOKEN".into(), wisp_protocol::Secret::new(value))])
}

fn key_profile(scratch: &ScratchDir) -> std::path::PathBuf {
    let config = scratch.root.canonicalize().unwrap().join("config.toml");
    let hash = wisp_core::session::sha256_bytes_hex(config.as_os_str().as_encoded_bytes());
    let identity = wisp_core::session::sha256_hex(&json!([hash, "keys/local"]).to_string());
    scratch
        .root
        .join("oauth")
        .join(hash)
        .join(format!("oauth-mcp-{identity}.json"))
}

async fn has_key(mcp: &Service) {
    let run = mcp
        .call(
            &tool_name("local", "has-key"),
            json!({}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(run.result.as_deref(), Some("true"));
}

async fn key_is_scrubbed(mcp: &Service, server: &str) {
    for fail in [false, true] {
        let run = mcp
            .call(
                &tool_name(server, "has-key"),
                json!({"echo-key": true, "fail": fail}),
                Duration::from_secs(2),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            run.result.as_deref(),
            Some(
                format!(
                    "key [redacted]\n{}",
                    json!({"key":"[redacted]","nested":["[redacted]"]})
                )
                .as_str()
            )
        );
        assert_eq!(run.attachments.len(), 1);
        assert_eq!(run.attachments[0].bytes, b"x");
        assert!(matches!(run.outcome, ToolOutcome::Completed { is_error, .. } if is_error == fail));
    }
    let run = mcp
        .call(
            &tool_name(server, "has-key"),
            json!({"escaped": true}),
            Duration::from_secs(2),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let named = json!({"named":{"[redacted]":["[redacted]"]}}).to_string();
    assert_eq!(run.result.unwrap(), format!("{}\n{named}", json!(named)));
    let messages = mcp
        .get_prompt(&format!("{}prompt:review.code", tool_name(server, "")))
        .await
        .unwrap();
    let resource = mcp
        .read_resource(&format!("{}resource:test:///notes", tool_name(server, "")))
        .await
        .unwrap();
    assert_eq!(
        (
            messages
                .into_iter()
                .map(|message| message.text)
                .collect::<Vec<_>>(),
            resource
        ),
        (
            vec![Some("[redacted] [redacted]".into()); 2],
            vec![Some("[redacted] [redacted]".into())]
        )
    );
}

#[tokio::test]
async fn a_typed_server_key_is_kept_after_add_and_reload_and_nowhere_else() {
    for key in [KEY, r#"CANARY-a"b\c"#] {
        let scratch = ScratchDir::new("wisp-mcp-", "key-add");
        let store = MemoryStore::default();
        let mut log = SessionLog::new(Box::new(store.clone()), "mcp-key");
        let mcp = key_service(&scratch, BTreeMap::new());
        let result = mcp
            .add_with_keys("local", key_spec(), typed_key(key))
            .await
            .unwrap();
        assert!(result.kept);
        assert_eq!(result.tools, 1);
        has_key(&mcp).await;
        key_is_scrubbed(&mcp, "local").await;
        assert!(!format!("{:?}", mcp.rows()).contains(key));
        let table = fs::read_to_string(scratch.root.join("config.toml")).unwrap();
        assert!(table.contains("keys = [\"WISP_TEST_TOKEN\"]"));
        assert!(!table.contains(key));
        for entry in fs::read_dir(scratch.root.join("mcp")).unwrap() {
            let entry = entry.unwrap();
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                assert!(!fs::read_to_string(entry.path()).unwrap().contains(key));
            }
        }
        assert!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(key_profile(&scratch)).unwrap())
                .unwrap()["keys"]["WISP_TEST_TOKEN"]
                == key
        );
        let loaded = key_service(&scratch, BTreeMap::from([("local".into(), key_spec())]));
        loaded.refresh_keys().await;
        assert_eq!(loaded.rows()[0].state, State::Stopped);
        loaded.connect("local").await.unwrap();
        assert_eq!(loaded.rows()[0].state, State::Ready);
        has_key(&loaded).await;
        key_is_scrubbed(&loaded, "local").await;
        log.record(LogEvent::Rejected {
            turn: Some(1),
            reason: format!("failed with {key:?}"),
            id: None,
            kind: None,
        });
        assert!(matches!(
            &store.read("mcp-key").unwrap()[0].event,
            LogEvent::Rejected { reason, .. } if reason == "failed with \"[redacted]\""
        ));
        let other = service(&scratch, "other", "key");
        assert!(other.connect("other").await.is_err());
    }
}

#[tokio::test]
async fn inherited_keys_and_a_same_command_readded_after_a_table_deletion_need_no_paste() {
    let scratch = ScratchDir::new("wisp-mcp-", "key-inherited");
    let mcp = key_service(&scratch, BTreeMap::new());
    let mut inherited = key_spec();
    inherited.args.push("PATH".into());
    inherited.keys = vec!["PATH".into()];
    assert!(
        mcp.add_with_keys("local", inherited, BTreeMap::new())
            .await
            .unwrap()
            .kept
    );
    has_key(&mcp).await;
    assert_eq!(private_profiles(&scratch.root.join("oauth")), 0);
    mcp.remove("local").await.unwrap();
    mcp.add_with_keys("local", key_spec(), typed_key(KEY))
        .await
        .unwrap();
    fs::write(scratch.root.join("config.toml"), "").unwrap();
    let readded = key_service(&scratch, BTreeMap::new());
    assert!(
        readded
            .add_with_keys("local", key_spec(), BTreeMap::new())
            .await
            .unwrap()
            .kept
    );
    has_key(&readded).await;
    assert_eq!(private_profiles(&scratch.root.join("oauth")), 1);
}

#[tokio::test]
async fn a_failed_key_start_writes_nothing_but_a_failed_keep_leaves_the_added_server_running() {
    let scratch = ScratchDir::new("wisp-mcp-", "key-fail");
    let mcp = key_service(&scratch, BTreeMap::new());
    assert!(
        mcp.add_with_keys("local", key_spec(), typed_key("CANARY-rejected-key"))
            .await
            .is_err()
    );
    assert!(mcp.rows().is_empty());
    assert!(!scratch.root.join("config.toml").exists());
    assert!(!key_profile(&scratch).exists());
    fs::write(
        scratch.root.join("block-key-store"),
        key_profile(&scratch).to_string_lossy().as_bytes(),
    )
    .unwrap();
    let result = mcp
        .add_with_keys("local", key_spec(), typed_key(KEY))
        .await
        .unwrap();
    assert!(!result.kept);
    assert_eq!(mcp.rows()[0].state, State::Ready);
    assert!(scratch.root.join("config.toml").exists());
    assert!(key_profile(&scratch).is_dir());
    assert_eq!(
        wisp_tool_text::mcp::key_not_kept("local"),
        "local added; its key couldn't be kept, so it will need the key again"
    );
    has_key(&mcp).await;
}

#[tokio::test]
async fn a_missing_or_mismatched_key_is_reported_before_start_and_remove_clears_by_name() {
    let scratch = ScratchDir::new("wisp-mcp-", "key-identity");
    let mcp = key_service(&scratch, BTreeMap::new());
    mcp.add_with_keys("local", key_spec(), typed_key(KEY))
        .await
        .unwrap();
    let spec = key_spec();
    let missing = key_service(&scratch, BTreeMap::from([("local".into(), spec.clone())]));
    let profile = key_profile(&scratch);
    let kept = fs::read(&profile).unwrap();
    fs::remove_file(&profile).unwrap();
    missing.refresh_keys().await;
    assert_eq!(missing.rows()[0].state, State::NeedsKey(spec.keys.clone()));
    assert_eq!(
        missing
            .call(
                &tool_name("local", "has-key"),
                json!({}),
                Duration::from_secs(2),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        wisp_tool_text::mcp::needs_key("local", &spec.keys)
    );
    fs::write(&profile, kept).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut changed = spec;
    changed.args.push("WISP_OTHER_TOKEN".into());
    let edited = key_service(
        &scratch,
        BTreeMap::from([("local".into(), changed.clone())]),
    );
    edited.refresh_keys().await;
    assert_eq!(edited.rows()[0].state, State::NeedsKey(changed.keys));
    assert!(edited.connect("local").await.is_err());
    let held = wisp_core::secrets::lock(
        &[(
            profile.parent().unwrap(),
            wisp_core::secrets::LockKind::Credentials,
        )],
        wisp_core::endpoints::PathShown::Given,
        wisp_core::secrets::LOCK_WAIT,
    )
    .unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(held);
    });
    edited.remove("local").await.unwrap();
    release.await.unwrap();
    assert!(!profile.exists());
}

#[tokio::test]
async fn setting_a_key_anew_restarts_then_keeps_it_and_preserves_an_old_key_after_failure() {
    let scratch = ScratchDir::new("wisp-mcp-", "key-replace");
    scratch.write("list-changed", "yes");
    scratch.write("slow-list", "yes");
    let mcp = key_service(&scratch, BTreeMap::new());
    let mut spec = key_spec();
    spec.keys.clear();
    spec.args[0] = "text".into();
    mcp.add("local", spec).await.unwrap();
    let before = fs::read_to_string(scratch.root.join("config.toml")).unwrap();
    fs::write(
        scratch.root.join("config.toml"),
        before.replace("command =", "# user's command\ncommand ="),
    )
    .unwrap();
    announce_change(&mcp).await;
    exists(&scratch, "slow-list-started").await;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        mcp.set_keys("local", typed_key(KEY)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        fs::read_to_string(scratch.root.join("methods"))
            .unwrap()
            .lines()
            .any(|method| method == "late-list-answered")
    );
    for service in [&mcp, &service(&scratch, "local", "text")] {
        assert!(!canonical_specs(&service.saved("local").unwrap().tools).contains("Late"));
    }
    assert!(result.kept);
    assert!(
        fs::read_to_string(scratch.root.join("config.toml"))
            .unwrap()
            .contains("# user's command\ncommand =")
    );
    let kept: serde_json::Value =
        serde_json::from_slice(&fs::read(key_profile(&scratch)).unwrap()).unwrap();
    assert!(kept["keys"]["WISP_TEST_TOKEN"].as_str() == Some(KEY));
    let path = fs::read_dir(scratch.root.join("mcp"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["observed"] = json!(1);
    fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    let config = scratch.root.join("config.toml");
    let table = fs::read(&config).unwrap();
    fs::write(&config, "").unwrap();
    assert!(mcp.set_keys("local", typed_key(KEY)).await.is_err());
    assert_eq!(
        service(&scratch, "local", "text")
            .saved("local")
            .map(|list| list.observed),
        Some(1)
    );
    fs::write(config, table).unwrap();
    let actual = key_service(&scratch, BTreeMap::new());
    actual
        .add_with_keys("keyed", key_spec(), typed_key(KEY))
        .await
        .unwrap();
    let old_profile = fs::read_dir(key_profile(&scratch).parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path != &key_profile(&scratch)
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    let old = fs::read(&old_profile).unwrap();
    let reason = actual
        .set_keys("keyed", typed_key("CANARY-rejected-key"))
        .await
        .unwrap_err();
    assert!(!reason.contains("CANARY"));
    assert!(matches!(&actual.rows()[0].state, State::Failed(_)));
    assert!(fs::read(old_profile).unwrap() == old);
    let replacement = "CANARY-replacement-key-0123456789";
    assert!(
        actual
            .set_keys("keyed", typed_key(replacement))
            .await
            .unwrap()
            .kept
    );
    assert_eq!(actual.rows()[0].state, State::Ready);
    key_is_scrubbed(&actual, "keyed").await;
    let address = McpServer {
        command: "https://server.example.invalid/mcp".into(),
        args: Vec::new(),
        keys: vec!["WISP_TEST_TOKEN".into()],
    };
    assert_eq!(
        actual
            .add_with_keys("address", address.clone(), typed_key(KEY))
            .await
            .unwrap_err(),
        wisp_tool_text::mcp::ADDRESS_KEYS
    );
    let address_service = key_service(
        &scratch,
        BTreeMap::from([(
            "address".into(),
            McpServer {
                keys: Vec::new(),
                ..address
            },
        )]),
    );
    assert_eq!(
        address_service
            .set_keys("address", typed_key(KEY))
            .await
            .unwrap_err(),
        wisp_tool_text::mcp::ADDRESS_KEYS
    );
}

async fn form_service(scratch: &ScratchDir, mode: &str, properties: serde_json::Value) -> Service {
    fs::write(scratch.root.join("form.json"), json!({"mode":"form","message":"Which value?","requestedSchema":{"type":"object","properties":properties}}).to_string()).unwrap();
    let mcp = service(scratch, "linear", mode).with_forms(true);
    mcp.connect("linear").await.unwrap();
    mcp
}

async fn form_call(
    mcp: &Service,
    user: wisp_core::call::user::UserChannel,
) -> wisp_core::tool::ToolRun {
    mcp.call_asking(
        "mcp__linear__z_echo",
        json!({}),
        Duration::from_secs(2),
        CancellationToken::new(),
        user,
    )
    .await
    .unwrap()
}

fn form_action(run: &wisp_core::tool::ToolRun) -> serde_json::Value {
    serde_json::from_str(run.result.as_deref().unwrap().lines().next().unwrap()).unwrap()
}

#[tokio::test]
async fn form_choice_completes() {
    use wisp_core::call::user::{AskReply, AskRequest, UserChannel};
    let scratch = ScratchDir::new("wisp-mcp-", "form-choice");
    let mcp = form_service(
        &scratch,
        "ask",
        json!({"team":{"type":"string","enum":["mobile","web"],"enumNames":["Mobile","Web"]}}),
    )
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let answer = tokio::spawn(async move {
        let AskRequest::Questions { questions, reply } = rx.recv().await.unwrap() else {
            panic!("expected a card")
        };
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].header, "Needs you");
        assert_eq!(questions[0].question, "Which value?");
        assert_eq!(
            questions[0]
                .options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            ["Mobile", "Web", "Not now"]
        );
        assert_eq!(
            questions[0].source.as_ref().unwrap().asked_by,
            wisp_protocol::AskedBy::Server {
                name: "linear".into()
            }
        );
        reply
            .send(AskReply::Answered {
                answers: vec![wisp_protocol::Answer::choose(1)],
            })
            .unwrap();
    });
    let run = form_call(&mcp, UserChannel::new(tx)).await;
    answer.await.unwrap();
    assert_eq!(
        form_action(&run),
        json!({"action":"accept","fields":{"team":"string"}})
    );
    let capabilities: serde_json::Value =
        serde_json::from_slice(&fs::read(scratch.root.join("client-capabilities")).unwrap())
            .unwrap();
    assert_eq!(
        capabilities["_meta"]["io.modelcontextprotocol/clientCapabilities"]["elicitation"],
        json!({"form":{}})
    );
}

#[tokio::test]
async fn form_declines_are_atomic() {
    use wisp_core::call::user::{AskReply, AskRequest, UserChannel};
    for kind in ["not-now", "own", "message"] {
        let scratch = ScratchDir::new("wisp-mcp-", kind);
        let mcp = form_service(
            &scratch,
            "ask",
            json!({"a":{"type":"string","enum":["One"]},"b":{"type":"string","enum":["Two"]}}),
        )
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let answer = tokio::spawn(async move {
            let AskRequest::Questions { questions, reply } = rx.recv().await.unwrap() else {
                panic!("expected card")
            };
            assert_eq!(questions.len(), 2);
            let own = wisp_protocol::Answer::own("Please do something else");
            reply
                .send(match kind {
                    "not-now" => AskReply::Answered {
                        answers: vec![
                            wisp_protocol::Answer::choose(0),
                            wisp_protocol::Answer::choose(1),
                        ],
                    },
                    "own" => AskReply::Answered {
                        answers: vec![wisp_protocol::Answer::choose(0), own],
                    },
                    _ => AskReply::MessageInstead,
                })
                .unwrap();
        });
        let run = form_call(&mcp, UserChannel::new(tx)).await;
        answer.await.unwrap();
        assert_eq!(form_action(&run), json!({"action":"decline","fields":{}}));
        assert_eq!(
            run.result
                .as_ref()
                .unwrap()
                .contains("Please do something else"),
            kind == "own"
        );
        if kind == "own" {
            assert!(
                run.result
                    .unwrap()
                    .contains("linear was told the user declined")
            );
        }
    }
}

#[tokio::test]
async fn form_types_round_trip() {
    use wisp_core::call::user::{AskReply, AskRequest, UserChannel};
    use wisp_protocol::Answer;
    let scratch = ScratchDir::new("wisp-mcp-", "form-types");
    let mcp = form_service(
        &scratch,
        "ask",
        json!({"a_text":{"type":"string"},"b_number":{"type":"number"},"c_yes":{"type":"boolean"},"d_list":{"type":"array","items":{"type":"string","enum":["One","Two"]}},"e_skip":{"type":"string"}}),
    ).await;
    fs::write(
        scratch.root.join("expected.json"),
        json!({"a_text":"typed field words","b_number":42,"c_yes":true,"d_list":["One","Two"]})
            .to_string(),
    )
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let answer = tokio::spawn(async move {
        let AskRequest::Questions { questions, reply } = rx.recv().await.unwrap() else {
            panic!("expected card")
        };
        assert!(questions[0].is_typed() && questions[1].is_typed());
        assert!(!questions[2].is_typed());
        assert!(questions[3].multi_select);
        reply
            .send(AskReply::Answered {
                answers: vec![
                    Answer::own("typed field words"),
                    Answer::own("42"),
                    Answer::choose(0),
                    Answer {
                        chosen: vec![0, 1],
                        own: None,
                    },
                    Answer::skipped(),
                ],
            })
            .unwrap();
    });
    let run = form_call(&mcp, UserChannel::new(tx)).await;
    answer.await.unwrap();
    assert_eq!(
        form_action(&run),
        json!({"action":"accept","fields":{"a_text":"string","b_number":"number","c_yes":"boolean","d_list":"array"},"matches_expected":true})
    );
    assert!(!run.result.unwrap().contains("typed field words"));
}

#[tokio::test]
async fn form_pages_follow_names() {
    use wisp_core::call::user::{AskReply, AskRequest, UserChannel};
    let scratch = ScratchDir::new("wisp-mcp-", "form-pages");
    let mcp = form_service(
        &scratch,
        "ask",
        json!({"z":{"type":"string","title":"Title","description":"Name the issue."},"a":{"type":"string","description":"First page."}}),
    ).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let answer = tokio::spawn(async move {
        let AskRequest::Questions { questions, reply } = rx.recv().await.unwrap() else {
            panic!("expected card")
        };
        assert_eq!(
            questions
                .iter()
                .map(|q| (q.header.as_str(), q.question.as_str()))
                .collect::<Vec<_>>(),
            [
                ("a", "Which value? First page."),
                ("Title", "Name the issue.")
            ]
        );
        reply
            .send(AskReply::Answered {
                answers: vec![
                    wisp_protocol::Answer::own("first"),
                    wisp_protocol::Answer::own("second"),
                ],
            })
            .unwrap();
    });
    let run = form_call(&mcp, UserChannel::new(tx)).await;
    answer.await.unwrap();
    assert_eq!(
        form_action(&run),
        json!({"action":"accept","fields":{"a":"string","z":"string"}})
    );
}

fn assert_no_form_secret(text: &str, secret: &str, numbers: &[&str]) {
    let mut spelling = secret.to_owned();
    for _ in 0..3 {
        assert!(!text.contains(&spelling), "a secret reached visible text");
        let json = serde_json::to_string(&spelling).unwrap();
        spelling = json[1..json.len() - 1].to_owned();
    }
    for &number in numbers {
        let canonical = number.parse::<serde_json::Number>().unwrap().to_string();
        assert!(
            !text.contains(number) && !text.contains(&canonical),
            "a numeric secret reached visible text"
        );
    }
}

#[tokio::test]
async fn form_secrets_and_bounds() {
    use wisp_core::call::user::{AskReply, AskRequest, SecretReply, UserChannel};
    for mode in ["ask", "ask-legacy"] {
        for (leak, canary) in [
            ("pdf", "CANARY-server-form-secret"),
            ("mime", "canary-server-form-secret"),
            ("escaped", r#"CANARY-a"b\c"#),
            ("unicode", "CANARY-e\u{301}\"b\\c"),
        ] {
            let scratch = ScratchDir::new("wisp-mcp-", "form-secret");
            let mcp = form_service(&scratch, mode, json!({
                "a_note":{"type":"string"},
                "admin_token":{"type":"string","title":"Admin token","description":"An admin token is needed to archive a project."},
                "b_one_time_password":{"type":"integer"},
                "c_number":{"type":"number","title":"Authentication token"},
                "d_integer":{"type":"integer","title":"Password"},
                "e_access_token":{"type":"number"}
            })).await;
            if mode == "ask" {
                fs::write(scratch.root.join("poll"), "yes").unwrap();
                fs::write(scratch.root.join("echo-form"), "yes").unwrap();
            }
            fs::write(scratch.root.join("echo-attachment"), leak).unwrap();
            fs::write(scratch.root.join("escaped"), "yes").unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel(2);
            let numeric_canaries = ["73129684", "8.514e9", "42869173", "6.715e8"];
            let answer = tokio::spawn(async move {
                let AskRequest::Questions { questions, reply } = rx.recv().await.unwrap() else {
                    panic!("expected card before secret")
                };
                assert_eq!(questions.len(), 1);
                assert_eq!(questions[0].header, "a_note");
                assert_no_form_secret(&format!("{questions:?}"), canary, &numeric_canaries);
                reply
                    .send(AskReply::Answered {
                        answers: vec![wisp_protocol::Answer::own("note")],
                    })
                    .unwrap();
                for (name, value) in [
                    "Admin token",
                    "b_one_time_password",
                    "Authentication token",
                    "Password",
                    "e_access_token",
                ]
                .into_iter()
                .zip(std::iter::once(canary).chain(numeric_canaries))
                {
                    let request = rx.recv().await.unwrap();
                    assert_no_form_secret(&format!("{request:?}"), canary, &numeric_canaries);
                    let AskRequest::Secret {
                        goes,
                        asked_by,
                        show,
                        name: title,
                        reply,
                        ..
                    } = request
                    else {
                        panic!("typed credential reached an ordinary question")
                    };
                    assert_eq!(title, name);
                    assert_eq!(show, wisp_protocol::Ends::default());
                    assert!(matches!(
                        goes,
                        wisp_core::call::user::Goes::Server(wisp_protocol::SecretDestination {
                            command: None,
                            ..
                        })
                    ));
                    assert_eq!(
                        asked_by,
                        Some(wisp_protocol::AskedBy::Server {
                            name: "linear".into()
                        })
                    );
                    reply
                        .send(SecretReply::Entered(wisp_protocol::Secret::new(value)))
                        .unwrap();
                }
                if mode == "ask" {
                    let AskRequest::Questions { questions, reply } = rx.recv().await.unwrap()
                    else {
                        panic!("expected scrubbed later card")
                    };
                    assert!(questions[0].question.contains("[redacted]"));
                    assert_no_form_secret(&format!("{questions:?}"), canary, &numeric_canaries);
                    reply
                        .send(AskReply::Answered {
                            answers: vec![wisp_protocol::Answer::own("again")],
                        })
                        .unwrap();
                }
                assert!(rx.recv().await.is_none());
            });
            let run = form_call(&mcp, UserChannel::new(tx)).await;
            answer.await.unwrap();
            assert!(run.attachments.is_empty());
            assert_no_form_secret(&format!("{run:?}"), canary, &numeric_canaries);
            let result = run.result.unwrap();
            assert_no_form_secret(&result, canary, &numeric_canaries);
            assert!(!result.contains(r"\u0043ANARY") && !result.contains(r"\u0063anary"));
            assert_eq!(result.matches(r#""by_name""#).count(), 1);
            assert!(result.contains("[redacted]"));
            assert!(result.contains("Wisp withheld linear's attachments, since this call received a value typed in the secret box."));
            assert_eq!(
                fs::read_to_string(scratch.root.join("secret-response-count")).unwrap(),
                "1"
            );
            let shape: serde_json::Value =
                serde_json::from_slice(&fs::read(scratch.root.join("secret-shape")).unwrap())
                    .unwrap();
            for field in [
                "b_one_time_password",
                "c_number",
                "d_integer",
                "e_access_token",
            ] {
                assert_eq!(shape["structuredContent"]["fields"][field], "number");
            }
            for file in fs::read_dir(&scratch.root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_file())
            {
                let text = fs::read_to_string(file.path()).unwrap();
                assert_no_form_secret(&text, canary, &numeric_canaries);
            }
        }
    }
    for kind in ["large", "url", "malformed"] {
        let scratch = ScratchDir::new("wisp-mcp-", kind);
        let mcp = form_service(
            &scratch,
            "ask",
            json!({"a":{"type":"string","title":"x".repeat(101)}}),
        )
        .await;
        if kind != "large" {
            fs::write(scratch.root.join("form.json"), if kind == "url" { json!({"mode":"url","message":"never drawn","url":"https://example.test/CANARY-url"}) } else { json!({"mode":"form","message":"CANARY-malformed","requestedSchema":{"type":"object","properties":{"a":{"type":"CANARY-invalid"}}}}) }.to_string()).unwrap();
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let result = mcp
            .call_asking(
                "mcp__linear__z_echo",
                json!({}),
                Duration::from_secs(2),
                CancellationToken::new(),
                UserChannel::new(tx),
            )
            .await;
        assert!(rx.try_recv().is_err());
        if kind == "large" {
            let run = result.unwrap();
            assert!(
                run.result
                    .unwrap()
                    .contains("linear's form is larger than Wisp shows, so Wisp declined it")
            );
        } else {
            assert!(!result.unwrap_err().contains("CANARY"));
        }
    }
}

#[tokio::test]
async fn form_cancellation_and_legacy() {
    use wisp_core::call::user::{AskReply, AskRequest, UserChannel};
    let scratch = ScratchDir::new("wisp-mcp-", "form-stop");
    let mcp = form_service(&scratch, "ask", json!({"a":{"type":"string"}})).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let answer = tokio::spawn(async move {
        let _request = rx.recv().await.unwrap();
        stop.cancel();
    });
    assert!(
        mcp.call_asking(
            "mcp__linear__z_echo",
            json!({}),
            Duration::from_secs(2),
            cancel,
            UserChannel::new(tx)
        )
        .await
        .is_err()
    );
    answer.await.unwrap();
    assert!(!scratch.root.join("response-shape").exists());
    for stopped in [false, true] {
        let scratch = ScratchDir::new("wisp-mcp-", "form-legacy");
        let mcp = form_service(&scratch, "ask-legacy", json!({"a":{"type":"string"}})).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let answer = tokio::spawn(async move {
            let AskRequest::Questions { reply, .. } = rx.recv().await.unwrap() else {
                panic!("expected legacy card")
            };
            if stopped {
                stop.cancel();
            } else {
                reply
                    .send(AskReply::Answered {
                        answers: vec![wisp_protocol::Answer::own("text")],
                    })
                    .unwrap();
            }
        });
        let run = mcp
            .call_asking(
                "mcp__linear__z_echo",
                json!({}),
                Duration::from_secs(2),
                cancel,
                UserChannel::new(tx),
            )
            .await;
        answer.await.unwrap();
        exists(&scratch, "legacy-action").await;
        assert_eq!(
            fs::read_to_string(scratch.root.join("legacy-action")).unwrap(),
            if stopped { "cancel" } else { "accept" }
        );
        assert_eq!(run.is_err(), stopped);
    }
    let scratch = ScratchDir::new("wisp-mcp-", "form-legacy-unowned");
    fs::write(scratch.root.join("ask-on-list"), "yes").unwrap();
    let _mcp = form_service(&scratch, "ask-legacy", json!({"a":{"type":"string"}})).await;
    exists(&scratch, "legacy-action").await;
    assert_eq!(
        fs::read_to_string(scratch.root.join("legacy-action")).unwrap(),
        "cancel"
    );
    let scratch = ScratchDir::new("wisp-mcp-", "form-legacy-cap");
    let mcp = form_service(&scratch, "ask-legacy", json!({"a":{"type":"string"}})).await;
    fs::write(scratch.root.join("legacy-repeat"), "yes").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let answer = tokio::spawn(async move {
        let mut count = 0;
        while let Some(AskRequest::Questions { reply, .. }) = rx.recv().await {
            count += 1;
            reply
                .send(AskReply::Answered {
                    answers: vec![wisp_protocol::Answer::own("text")],
                })
                .unwrap();
        }
        count
    });
    let run = form_call(&mcp, UserChannel::new(tx)).await;
    assert_eq!(form_action(&run)["action"], "cancel");
    assert_eq!(answer.await.unwrap(), rmcp::model::DEFAULT_MRTR_MAX_ROUNDS);
    let scratch = ScratchDir::new("wisp-mcp-", "form-legacy-url");
    let mcp = form_service(&scratch, "ask-legacy", json!({})).await;
    fs::write(
        scratch.root.join("form.json"),
        json!({"mode":"url","message":"never drawn","url":"https://example.test/CANARY-url"})
            .to_string(),
    )
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    assert_eq!(
        form_action(&form_call(&mcp, UserChannel::new(tx)).await)["action"],
        "decline"
    );
    assert!(rx.try_recv().is_err());
    let scratch = ScratchDir::new("wisp-mcp-", "form-ambiguous");
    let mcp = form_service(&scratch, "ask-legacy", json!({"a":{"type":"string"}})).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let first = mcp.call_asking(
        "mcp__linear__z_echo",
        json!({"wait":true}),
        Duration::from_secs(2),
        CancellationToken::new(),
        UserChannel::new(tx.clone()),
    );
    let second = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        mcp.call_asking(
            &tool_name("linear", "a-first"),
            json!({}),
            Duration::from_secs(2),
            CancellationToken::new(),
            UserChannel::new(tx),
        )
        .await
    };
    let (first, second) = tokio::join!(first, second);
    for result in [first, second] {
        assert_eq!(form_action(&result.unwrap())["action"], "cancel");
    }
    assert!(rx.try_recv().is_err());
}

const MCP_ENV_CHILD: &str = "WISP_MCP_ENV_CHILD";

fn conflicting_prompt_value(default: &str) -> String {
    format!("inherited-not-{default}")
}

#[test]
fn stdio_server_strips_askpass_and_sets_prompt_defaults() {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "stdio_env_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MCP_ENV_CHILD, "1");
    for name in wisp_core::jobs::STRIPPED_ENV {
        command.env(name, format!("inherited-{name}"));
    }
    for (name, default) in wisp_core::jobs::PROMPT_DEFAULTS {
        command.env(name, conflicting_prompt_value(default));
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "stdio_env_child failed ({:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
}

#[tokio::test]
#[ignore = "a helper that stdio_server_strips_askpass_and_sets_prompt_defaults runs with seeded env"]
async fn stdio_env_child() {
    if std::env::var_os(MCP_ENV_CHILD).is_none() {
        return;
    }
    for name in wisp_core::jobs::STRIPPED_ENV {
        assert!(std::env::var_os(name).is_some(), "missing seeded {name}");
    }
    for (name, default) in wisp_core::jobs::PROMPT_DEFAULTS {
        let expected = conflicting_prompt_value(default);
        assert_eq!(
            std::env::var_os(name).as_deref(),
            Some(std::ffi::OsStr::new(&expected)),
            "missing seeded {name}={expected}"
        );
    }
    let mut vars: Vec<&str> = wisp_core::jobs::STRIPPED_ENV.to_vec();
    vars.extend(wisp_core::jobs::PROMPT_DEFAULTS.iter().map(|(k, _)| *k));
    vars.push(wisp_protocol::WISP_JOB);

    let scratch = ScratchDir::new("wisp-mcp-", "stdio-env");
    let mcp = service(&scratch, "local", "modern");
    mcp.connect("local").await.unwrap();
    let run = mcp
        .call(
            "mcp__local__z_echo",
            json!({"vars": vars}),
            Duration::from_secs(5),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let seen: serde_json::Value = serde_json::from_str(&run.result.unwrap()).unwrap();
    for name in wisp_core::jobs::STRIPPED_ENV {
        assert!(seen[name].is_null(), "MCP server saw stripped {name}");
    }
    for (name, value) in wisp_core::jobs::PROMPT_DEFAULTS {
        assert_eq!(seen[name], json!(value), "MCP server {name} mismatch");
    }
    assert_eq!(seen[wisp_protocol::WISP_JOB], json!("1"));

    let mut keyed = stdio_server("modern");
    keyed.keys = vec!["GIT_ASKPASS".into(), "GIT_TERMINAL_PROMPT".into()];
    let mcp_keys = Service::new(BTreeMap::new(), scratch.root.clone(), None);
    mcp_keys
        .add_with_keys(
            "keyed",
            keyed,
            BTreeMap::from([
                ("GIT_ASKPASS".into(), wisp_protocol::Secret::new("ok")),
                (
                    "GIT_TERMINAL_PROMPT".into(),
                    wisp_protocol::Secret::new("1"),
                ),
            ]),
        )
        .await
        .unwrap();
    let run = mcp_keys
        .call(
            "mcp__keyed__z_echo",
            json!({"vars": vars}),
            Duration::from_secs(5),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let seen: serde_json::Value = serde_json::from_str(&run.result.unwrap()).unwrap();
    assert_eq!(seen["GIT_ASKPASS"], json!("ok"));
    assert_eq!(seen["GIT_TERMINAL_PROMPT"], json!("1"));
    for (name, value) in wisp_core::jobs::PROMPT_DEFAULTS {
        if name != "GIT_TERMINAL_PROMPT" {
            assert_eq!(seen[name], json!(value), "keyed MCP server {name} mismatch");
        }
    }
    assert_eq!(seen[wisp_protocol::WISP_JOB], json!("1"));
    for name in wisp_core::jobs::STRIPPED_ENV {
        if name != "GIT_ASKPASS" {
            assert!(seen[name].is_null(), "keyed MCP server saw stripped {name}");
        }
    }
}
