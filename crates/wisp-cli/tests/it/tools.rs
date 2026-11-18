#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket, pty or process error"
)]

use crate::support;

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rustix::process::{Signal, kill_process};
use support::pty::{PATIENCE, Pty, READY, Stdout, usable, wait_for_exit, wait_until};
use support::server::{Server, message_start, sse};
use support::{
    WISP,
    home::{CONFIG_DIR, Home, SESSIONS, both, command_in},
};

const TUI_ONLY: &str = "tools are only available in the TUI for now";
const DONE: &str = "ALL-CHECKS-DONE";
const DEFAULT_TOOLS: [&str; 18] = [
    "ask_secret",
    "ask_user",
    "bash",
    "list_accounts",
    "list_agents",
    "list_settings",
    "process",
    "rename_agent",
    "search",
    "send_message",
    "show_code",
    "show_diff",
    "spawn_agent",
    "stop_agent",
    "use",
    "view_file",
    "web_search",
    "write_todos",
];
const TTY_COMMAND: &str = r"printf '\124\124\131-MARK\033[2J\033[H' > /dev/tty; echo exit-was-$?";

fn ending(stop_reason: &str) -> [serde_json::Value; 2] {
    [
        serde_json::json!({"type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": {"output_tokens": 5}}),
        serde_json::json!({"type": "message_stop"}),
    ]
}

fn tool_reply(command: &str) -> String {
    let input = serde_json::json!({"command": command, "description": "Tries the terminal"});
    support::server::tool_reply("toolu_01", "bash", &input)
}

fn text_reply(text: &str) -> String {
    support::server::text_reply("msg_text", "claude-sonnet-5-5", text, (1, 5))
}

fn serve(replies: Vec<String>) -> Server {
    let replies = Mutex::new(replies.into_iter());
    Server::start(move |_| {
        replies
            .lock()
            .unwrap()
            .next()
            .unwrap_or_else(|| text_reply("no more replies"))
    })
}

impl Home {
    fn tools_command(&self, program: &str, port: u16) -> Command {
        let mut command = command_in(&self.0, program);
        command
            .env("TMPDIR", self.dir("tmp"))
            .env("ANTHROPIC_API_KEY", "test-key-not-used")
            .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"));
        command
    }
}

#[test]
fn tools_bash_is_refused_outside_the_tui() {
    let server = serve(vec![]);
    let home = Home::new("refused");
    let output = home
        .tools_command(env!("CARGO_BIN_EXE_wisp"), server.port)
        .args(["--no-log", "--env-file"])
        .arg(home.env_file())
        .args(["--output", "jsonl", "--tools", "bash"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{}", both(&output));
    assert!(stderr.contains(TUI_ONLY), "{stderr}");
    assert!(server.bodies().is_empty());
}

#[test]
fn jsonl_sessions_send_no_tools() {
    for (name, extra, input) in [
        (
            "none",
            &["--output", "jsonl", "--tools", "none"][..],
            "hello\n",
        ),
        ("jsonl", &["--output", "jsonl"], "hello\n"),
    ] {
        let server = serve(vec![text_reply(DONE)]);
        let home = Home::new(name);
        let mut child = home
            .tools_command(env!("CARGO_BIN_EXE_wisp"), server.port)
            .args(["--no-log", "--time", "off", "--env-file"])
            .arg(home.env_file())
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
        let sent = wait_until(Duration::from_secs(20), || !server.bodies().is_empty());
        drop(stdin);
        let status = wait_for_exit(&mut child);
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(sent, "{name}: no request: {stderr}");
        assert!(status.success(), "{name}: {status:?} {stderr}");
        for body in server.bodies().iter() {
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            assert!(request.get("tools").is_none(), "{name}: {body}");
        }
        if name == "jsonl" {
            let mut stdout = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut stdout)
                .unwrap();
            let lines: Vec<&str> = stdout.lines().collect();
            let started = lines
                .iter()
                .position(|l| *l == r#"{"type":"turn_started","turn":1}"#)
                .unwrap_or_else(|| panic!("{stdout}"));
            let step = lines
                .iter()
                .position(|l| *l == r#"{"type":"step_started","step":1}"#)
                .unwrap_or_else(|| panic!("{stdout}"));
            let delivered = lines
                .iter()
                .position(|l| *l == r#"{"type":"input_delivered","ids":["jsonl-1"],"step":1}"#)
                .unwrap_or_else(|| panic!("{stdout}"));
            assert!(started < step && step + 1 == delivered, "{stdout}");
        }
    }
}

fn tui(home: &Home, port: u16, extra: &[&str]) -> Pty {
    tui_at(home, port, extra, &Launch::default())
}

struct Launch<'a> {
    cols: u16,
    log: bool,
    env: &'a [(&'a str, &'a str)],
}

impl Default for Launch<'_> {
    fn default() -> Self {
        Launch {
            cols: 100,
            log: false,
            env: &[],
        }
    }
}

fn tui_at(home: &Home, port: u16, extra: &[&str], launch: &Launch) -> Pty {
    let mut command = home.tools_command("setsid", port);
    command.args(["--ctty", "--wait", WISP]);
    if !launch.log {
        command.arg("--no-log");
    }
    command
        .envs(launch.env.iter().copied())
        .args(["--time", "off", "--env-file"])
        .arg(home.env_file())
        .args(extra)
        .env("TERM", "xterm-256color");
    Pty::start(&mut command, 30, launch.cols, Stdout::Tty)
}

fn tool_results(requests: &[String]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .flat_map(|request| {
            request["messages"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
                .filter(|block| block["type"] == "tool_result")
                .map(|block| block.to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn run_in_tui(name: &str, command: &str) -> (Vec<u8>, String, Vec<String>) {
    let server = serve(vec![tool_reply(command), text_reply(DONE)]);
    let home = Home::new(name);
    let mut pty = tui(&home, server.port, &[]);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.send(b"try the terminal\r");
    pty.wait_for(DONE.as_bytes());
    pty.wait_idle();
    let frame = pty.shown();
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let screen = pty.screen.lock().unwrap().clone();
    let requests = server.bodies();
    (screen, frame, requests)
}

#[test]
fn a_no_log_chat_masks_an_exported_held_key_before_the_next_request() {
    if !usable() {
        return;
    }
    const KEY: &str = "CANARY-cli-held-credential";
    const OTHER_KEY: &str = "CANARY-cli-other-provider-credential";
    let server = serve(vec![
        tool_reply("printenv ANTHROPIC_API_KEY OPENAI_API_KEY"),
        text_reply(DONE),
    ]);
    let home = Home::new("masked-no-log");
    let launch = Launch {
        env: &[("ANTHROPIC_API_KEY", KEY), ("OPENAI_API_KEY", OTHER_KEY)],
        ..Launch::default()
    };
    let doctor = home
        .tools_command(WISP, server.port)
        .envs(launch.env.iter().copied())
        .args(["doctor", "--no-log", "--time", "off", "--env-file"])
        .arg(home.env_file())
        .output()
        .unwrap();
    assert!(doctor.status.success(), "{}", both(&doctor));
    assert!(
        String::from_utf8_lossy(&doctor.stdout).contains("127.0.0.1"),
        "{}",
        both(&doctor)
    );
    let mut pty = tui_at(&home, server.port, &[], &launch);
    pty.wait_for(READY);
    pty.send(b"read the variable\r");
    pty.wait_for(DONE.as_bytes());
    pty.wait_idle();
    pty.send(b"\x04");
    assert!(wait_for_exit(&mut pty.child).success());
    let turns = turns(&server.bodies());
    assert_eq!(turns.len(), 2);
    assert_prefix_stable(&turns);
    let result = result_text(&result_of(&turns, "toolu_01"));
    assert!(!result.contains(KEY));
    assert!(!result.contains(OTHER_KEY));
    assert!(result.contains("[redacted]"));
    assert_eq!(
        result
            .matches(wisp_tool_text::output::HELD_KEY_NOTE)
            .count(),
        1
    );
}

#[test]
fn a_command_that_opens_dev_tty_cannot_disturb_the_screen() {
    if !usable() {
        return;
    }
    let (screen, frame, requests) = run_in_tui("tty", TTY_COMMAND);
    let shown = String::from_utf8_lossy(&screen);
    assert!(!shown.contains("TTY-MARK"), "{shown:?}");
    assert!(frame.contains("Tries the terminal"), "{frame:?}");
    let first: serde_json::Value = serde_json::from_str(&requests[0]).unwrap();
    let tools: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(tools, DEFAULT_TOOLS);
    assert!(!tools.contains(&"report"));
    let results = tool_results(&requests);
    assert_eq!(results.len(), 1, "{requests:#?}");
    assert!(!results[0].contains("exit-was-0"), "{}", results[0]);
    assert!(results[0].contains("exit-was-"), "{}", results[0]);
}

#[test]
fn the_tui_runs_bash_in_its_working_directory_and_shows_the_row() {
    if !usable() {
        return;
    }
    let (screen, frame, requests) = run_in_tui("cwd", "ls; echo listed-$((6*7))");
    let shown = String::from_utf8_lossy(&screen);
    assert!(frame.contains("Tries the terminal"), "{frame:?}");
    assert!(frame.contains("echo listed-$((6*7))"), "{frame:?}");
    assert!(!shown.contains("listed-42"), "{shown:?}");
    let results = tool_results(&requests);
    assert_eq!(results.len(), 1, "{requests:#?}");
    assert!(results[0].contains("empty.env"), "{}", results[0]);
    assert!(results[0].contains("listed-42"), "{}", results[0]);
}

fn welcome(name: &str, extra: &[&str], config: Option<&str>) -> (String, String, String) {
    let server = serve(vec![]);
    let home = Home::new(name);
    if let Some(text) = config {
        let dir = home.0.join(CONFIG_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }
    let mut pty = tui(&home, server.port, extra);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(300));
    pty.send(b"\x0f");
    let page = pty.wait_shown("ctrl-o back");
    pty.send(b"q");
    let main = pty.wait_screen("the main view", |shown| !shown.contains("ctrl-o back"));
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    assert!(server.bodies().is_empty());
    let screen = pty.screen.lock().unwrap().clone();
    (String::from_utf8_lossy(&screen).into_owned(), page, main)
}

const ENV_OFF: &str = "[endpoints.env]\nnative_web_search = false\n";

#[test]
fn the_welcome_panel_says_a_gateway_searches_unless_turned_off() {
    if !usable() {
        return;
    }
    let (default, _, _) = welcome("search-default", &[], None);
    assert!(!default.contains("web search: none"), "{default:?}");
    assert!(!default.contains("web search off"), "{default:?}");
    assert!(!default.contains("chat only"), "{default:?}");
    let (only, _, _) = welcome("search-only", &["--tools", "web_search"], None);
    assert!(!only.contains("web search"), "{only:?}");
    assert!(!only.contains("chat only"), "{only:?}");
    let (_, page, main) = welcome("search-only-off", &["--tools", "web_search"], Some(ENV_OFF));
    assert!(
        main.contains("web search off \u{b7} chat only \u{b7} ctrl-o for why"),
        "{main:?}"
    );
    assert!(
        page.contains("web search: off (off for this account)"),
        "{page:?}"
    );
    let (off, _, main) = welcome("search-off", &[], Some(ENV_OFF));
    assert!(
        main.contains("web search off \u{b7} ctrl-o for why"),
        "{main:?}"
    );
    assert!(!off.contains("chat only"), "{off:?}");
    let (none, _, _) = welcome("search-none", &["--tools", "bash"], None);
    assert!(!none.contains("web search"), "{none:?}");
}

fn members(leader: rustix::process::Pid) -> Vec<rustix::process::Pid> {
    wisp_core::jobs::exec::session_members(leader)
}

fn leader(marker: &Path) -> rustix::process::Pid {
    let leader = std::cell::Cell::new(None);
    let started = wait_until(Duration::from_secs(20), || {
        let pid = std::fs::read_to_string(marker)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            .and_then(rustix::process::Pid::from_raw);
        leader.set(pid);
        pid.is_some_and(|pid| !members(pid).is_empty())
    });
    assert!(started, "the command never started");
    leader.get().unwrap()
}

#[test]
fn a_second_quit_exits_130_at_once_and_kills_the_running_commands_session() {
    if !usable() {
        return;
    }
    let command = "trap '' TERM; echo $$ > leader.pid; sleep 60";
    let server = serve(vec![tool_reply(command), text_reply(DONE)]);
    let home = Home::new("forced");
    let marker = home.0.join("leader.pid");
    let mut pty = tui(&home, server.port, &[]);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.send(b"start the long command\r");
    let leader = leader(&marker);
    pty.send(b"/exit\r");
    std::thread::sleep(Duration::from_millis(300));
    pty.send(b"\x03");
    let quit = Instant::now();
    let status = wait_for_exit(&mut pty.child);
    let took = quit.elapsed();
    let emptied = wait_until(Duration::from_secs(1), || members(leader).is_empty());
    let left = members(leader);
    for pid in &left {
        let _ = rustix::process::kill_process(*pid, rustix::process::Signal::KILL);
    }
    assert_eq!(status.code(), Some(130), "{status:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert!(emptied, "the command's session outlived wisp: {left:?}");
    let screen = String::from_utf8_lossy(&pty.screen.lock().unwrap()).into_owned();
    assert!(
        !screen.contains("settings save was still waiting"),
        "{screen:?}"
    );
}

#[test]
fn sigterm_to_the_host_stops_the_programs_keeps_the_kept_one_and_logs_the_ends() {
    if !usable() {
        return;
    }
    let wait = format!(
        "while kill -0 {} 2>/dev/null; do sleep 0.05; done",
        std::process::id()
    );
    let program = |id: &str, marker: &str, kept: bool| {
        let command = format!("trap '' TERM; echo $$ > {marker}; {wait}");
        let input = serde_json::json!({"command": command, "description": "Starts a program",
            "run_in_background": true, "keep_running": kept});
        support::server::tool_reply(id, "bash", &input)
    };
    {
        let name = "sigterm";
        let first = program("toolu_01", "one.pid", false);
        let second = program("toolu_02", "kept.pid", true);
        let server = serve(vec![first, second, text_reply(DONE)]);
        let home = Home::new(name);
        let launch = Launch {
            log: true,
            ..Launch::default()
        };
        let mut pty = tui_at(&home, server.port, &[], &launch);
        pty.wait_for(READY);
        std::thread::sleep(Duration::from_millis(200));
        pty.send(b"start the programs\r");
        pty.wait_for(DONE.as_bytes());
        let one = leader(&home.0.join("one.pid"));
        let kept = leader(&home.0.join("kept.pid"));
        let host = support::hosted::pid(&home);
        let sent = Instant::now();
        kill_process(host, Signal::TERM).unwrap();
        let status = wait_for_exit(&mut pty.child);
        let took = sent.elapsed();
        let left = members(one);
        let kept_left = members(kept);
        for pid in left.iter().chain(&kept_left) {
            let _ = kill_process(*pid, Signal::KILL);
        }
        assert!(status.success(), "{name}: {status}");
        assert!(took < wisp_start::start::SIGNAL_LIMIT, "{name}: {took:?}");
        assert!(pty.cooked(), "{name}");
        assert!(
            left.is_empty(),
            "{name}: the program outlived wisp: {left:?}"
        );
        assert!(
            !kept_left.is_empty(),
            "{name}: the kept program was stopped"
        );
        let screen = pty.screen.lock().unwrap().clone();
        let leave = b"\x1b[?1049l";
        let restored = screen
            .windows(leave.len())
            .rposition(|word| word == leave)
            .unwrap();
        let after = support::screen::shown(&screen[restored + leave.len()..], 30, launch.cols);
        assert!(
            after.contains(wisp_render::transcript::copy::BYE),
            "{after}"
        );
        assert!(!after.contains(DONE), "{after}");
        let records = home.records();
        let ended: Vec<&serde_json::Value> = records
            .iter()
            .filter(|r| r["type"] == "process_ended")
            .collect();
        assert_eq!(ended.len(), 1, "{name}: {ended:?}");
        assert_eq!(ended[0]["id"], 1, "{name}: {ended:?}");
        assert_eq!(ended[0]["end"]["type"], "stopped", "{name}: {ended:?}");
        assert_eq!(ended[0]["end"]["by"]["type"], "session_ended", "{name}");
        assert_eq!(records.last().unwrap()["type"], "session_ended", "{name}");
    }
}

fn ask_reply(questions: &serde_json::Value, then_bash: Option<&str>) -> String {
    let ask = serde_json::json!({"questions": questions});
    let mut events = vec![
        message_start("msg_ask", "claude-sonnet-5-5", 1),
        serde_json::json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_ask", "name": "ask_user", "input": {}}}),
        serde_json::json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": ask.to_string()}}),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
    ];
    if let Some(command) = then_bash {
        let input = serde_json::json!({"command": command, "description": "Makes the marker"});
        events.extend([
            serde_json::json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "tool_use", "id": "toolu_bash", "name": "bash", "input": {}}}),
            serde_json::json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
            serde_json::json!({"type": "content_block_stop", "index": 1}),
        ]);
    }
    events.extend(ending("tool_use"));
    sse(&events)
}

fn choice(header: &str, question: &str) -> serde_json::Value {
    serde_json::json!({"header": header, "question": question, "options": [
        {"label": "Red", "description": "The warm one."},
        {"label": "Blue", "description": "The cool one."},
    ]})
}

fn scripted(questions: &serde_json::Value) -> Vec<wisp_protocol::Question> {
    wisp_plugin_ask_user::parse(&serde_json::json!({"questions": questions}))
        .unwrap()
        .into_iter()
        .map(wisp_core::plugin::convert::question)
        .collect()
}

fn strip_cache_control(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("cache_control");
            map.values_mut().for_each(strip_cache_control);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_cache_control),
        _ => {}
    }
}

fn turns(requests: &[String]) -> Vec<serde_json::Value> {
    requests
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .filter(|request| request["stream"] == true)
        .map(|mut request| {
            strip_cache_control(&mut request);
            request
        })
        .collect()
}

fn assert_prefix_stable(turns: &[serde_json::Value]) {
    assert!(turns.len() >= 2, "{turns:#?}");
    let tools: Vec<&str> = turns[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(tools, DEFAULT_TOOLS);
    assert!(!tools.contains(&"report"));
    support::prefix::assert_prefix_stable(turns, None);
}

fn result_of(turns: &[serde_json::Value], id: &str) -> serde_json::Value {
    turns
        .iter()
        .flat_map(|t| t["messages"].as_array().cloned().unwrap_or_default())
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == id)
        .unwrap_or_else(|| panic!("no result for {id}: {turns:#?}"))
}

fn result_text(block: &serde_json::Value) -> String {
    match &block["content"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        other => panic!("{other}"),
    }
}

impl Pty {
    fn wait_for_card(&self, question: &str) {
        let settling = wisp_render::transcript::copy::CARD_SETTLING;
        let held = |shown: &str| shown.contains(question) && !shown.contains(settling);
        if !wait_until(Duration::from_secs(20), || held(&self.shown())) {
            panic!("the card never held the keys: {}", self.shown());
        }
    }

    fn press(&mut self, bytes: &[u8]) {
        self.send(bytes);
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn ask_in_tui(
    name: &str,
    replies: Vec<String>,
    drive: impl FnOnce(&Home, &mut Pty),
) -> Vec<String> {
    let server = serve(replies);
    let home = Home::new(name);
    let mut pty = tui(&home, server.port, &[]);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.send(b"pick a colour\r");
    drive(&home, &mut pty);
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    server.bodies()
}

#[test]
fn a_question_answered_by_number_skips_the_call_after_it() {
    if !usable() {
        return;
    }
    let questions = serde_json::json!([choice("Colour", "Which colour should the button be?")]);
    let requests = ask_in_tui(
        "ask-number",
        vec![
            ask_reply(&questions, Some("touch MARKER")),
            text_reply(DONE),
        ],
        |home, pty| {
            pty.wait_for_card("Which colour should the button be?");
            pty.press(b"2");
            pty.wait_for(DONE.as_bytes());
            pty.wait_idle();
            assert!(!home.0.join("MARKER").exists());
        },
    );
    let turns = turns(&requests);
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert_prefix_stable(&turns);
    let asked = result_of(&turns, "toolu_ask");
    let expected = wisp_core::tool::question::result_text(
        &scripted(&questions),
        &wisp_protocol::QuestionEnd::Answered {
            answers: vec![wisp_protocol::Answer::choose(1)],
        },
    );
    assert_eq!(result_text(&asked), expected);
    assert_ne!(asked["is_error"], true, "{asked}");
    let skipped = result_of(&turns, "toolu_bash");
    assert_eq!(
        result_text(&skipped),
        wisp_core::agent::not_run_text(&wisp_protocol::NotRunReason::AfterQuestion)
    );
}

#[test]
fn a_question_set_takes_an_own_answer_and_a_skip_from_the_review_page() {
    if !usable() {
        return;
    }
    let questions = serde_json::json!([
        choice("Colour", "Which colour should the button be?"),
        choice("Border", "Which colour should the border be?"),
    ]);
    let requests = ask_in_tui(
        "ask-set",
        vec![ask_reply(&questions, None), text_reply(DONE)],
        |_, pty| {
            pty.wait_for_card("Which colour should the button be?");
            pty.press(b"green like moss");
            pty.press(b"\r");
            pty.wait_for_card("Which colour should the border be?");
            pty.press(b"\t");
            pty.wait_shown(wisp_render::transcript::copy::SEND_ANSWERS);
            pty.press(b"1");
            pty.wait_for(DONE.as_bytes());
            pty.wait_idle();
        },
    );
    let turns = turns(&requests);
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert_prefix_stable(&turns);
    let expected = wisp_core::tool::question::result_text(
        &scripted(&questions),
        &wisp_protocol::QuestionEnd::Answered {
            answers: vec![
                wisp_protocol::Answer::own("green like moss"),
                wisp_protocol::Answer::skipped(),
            ],
        },
    );
    assert_eq!(result_text(&result_of(&turns, "toolu_ask")), expected);
}

#[test]
fn esc_on_a_question_stops_the_turn_and_gives_the_typed_text_back() {
    if !usable() {
        return;
    }
    let questions = serde_json::json!([choice("Colour", "Which colour should the button be?")]);
    let requests = ask_in_tui(
        "ask-esc",
        vec![ask_reply(&questions, None), text_reply(DONE)],
        |_, pty| {
            pty.wait_for_card("Which colour should the button be?");
            pty.press(b"teal instead");
            pty.press(b"\x1b");
            pty.wait_shown(wisp_render::transcript::copy::ANSWER_BACK);
            pty.wait_idle();
            assert!(
                !pty.shown()
                    .contains(wisp_render::transcript::copy::OWN_PREFIX),
                "{}",
                pty.shown()
            );
            pty.press(b"\r");
            pty.wait_for(DONE.as_bytes());
            pty.wait_idle();
        },
    );
    let turns = turns(&requests);
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert_prefix_stable(&turns);
    let expected = wisp_core::tool::question::result_text(
        &scripted(&questions),
        &wisp_protocol::QuestionEnd::Stopped,
    );
    let stopped = result_of(&turns, "toolu_ask");
    assert_eq!(result_text(&stopped), expected);
    assert_eq!(stopped["is_error"], true, "{stopped}");
    let last = turns[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    let content = last["content"].as_array().unwrap();
    let at_result = content
        .iter()
        .position(|b| b["type"] == "tool_result")
        .unwrap_or_else(|| panic!("{last}"));
    let at_message = content
        .iter()
        .position(|b| {
            b["type"] == "text"
                && b["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("teal instead"))
        })
        .unwrap_or_else(|| panic!("{last}"));
    assert!(at_result < at_message, "{last}");
}

#[test]
fn a_saved_tool_no_plugin_provides_is_left_off_with_a_warning_and_stays_saved() {
    if !usable() {
        return;
    }
    let saved = "tools = [\"bash\", \"telepathy\"]\n";
    let server = serve(vec![]);
    let home = Home::new("tool-gone-start");
    let dir = home.0.join(CONFIG_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), saved).unwrap();
    let mut pty = tui(&home, server.port, &[]);
    pty.wait_shown("which no plugin provides");
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(300));
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("config.toml")).unwrap(),
        saved
    );
}

const CANARY: &str = "SIDEBAR-CANARY";

fn masked(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                if key == "at" || key.ends_with("_at") || key == "ms" || key.ends_with("_ms") {
                    *inner = serde_json::Value::Null;
                } else {
                    masked(inner);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(masked),
        _ => {}
    }
}

fn session_records(home: &Home, port: u16) -> Vec<serde_json::Value> {
    let dir = home.file(SESSIONS);
    let mut records = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let session = entry
            .path()
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(entry.path())
            .unwrap()
            .replace(home.0.to_str().unwrap(), "HOME")
            .replace(&session, "SID")
            .replace(&format!("127.0.0.1:{port}"), "127.0.0.1:PORT");
        records.extend(text.lines().map(|l| {
            let mut record = serde_json::from_str(l).unwrap();
            masked(&mut record);
            record
        }));
    }
    records
}

fn panel_rows(shown: &str) -> Vec<String> {
    shown
        .lines()
        .map(|line| line.chars().skip(120).collect())
        .collect()
}

fn sidebar_session(cols: u16) -> (Vec<String>, Vec<serde_json::Value>) {
    let server = serve(vec![text_reply("first answer"), text_reply(DONE)]);
    let home = Home::new(&format!("sidebar-{cols}"));
    let launch = Launch {
        cols,
        log: true,
        env: &[("WISP_TESTKIT", "sidebar")],
    };
    let mut pty = tui_at(
        &home,
        server.port,
        &["--set", "plugins.testkit.slots.busy=on"],
        &launch,
    );
    pty.wait_for(READY);
    if cols >= 120 {
        assert!(
            wait_until(Duration::from_secs(20), || docked(&pty)),
            "the busy section never docked: {}",
            pty.shown()
        );
    }
    pty.send(b"first question\r");
    pty.wait_shown("first answer");
    pty.wait_idle();
    if cols >= 120 {
        let top = panel_rows(&pty.shown());
        for _ in 0..3 {
            pty.send(b"\x1b[<65;140;6M");
        }
        let scrolled = wait_until(Duration::from_secs(20), || {
            let rows = panel_rows(&pty.shown());
            rows != top
                && rows
                    .iter()
                    .any(|r| r.contains("\u{2191}") && r.contains("more"))
        });
        assert!(scrolled, "the panel never scrolled: {}", pty.shown());
    } else {
        assert!(!pty.shown().contains(CANARY), "{}", pty.shown());
    }
    pty.send(b"second question\r");
    pty.wait_for(DONE.as_bytes());
    pty.wait_idle();
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let requests = server.bodies();
    (requests, session_records(&home, server.port))
}

#[test]
fn a_docked_scrolled_sidebar_changes_no_request_and_adds_no_record() {
    let (wide, wide_log) = terminal_free(sidebar_session(160), 115);
    let (narrow, narrow_log) = terminal_free(sidebar_session(100), 96);
    assert_prefix_stable(&turns(&wide));
    assert_eq!(wide.len(), 2, "{wide:#?}");
    assert_eq!(wide, narrow);
    for body in &wide {
        for leak in [CANARY, "testkit/busy", "\"Busy\""] {
            assert!(!body.contains(leak), "{leak}: {body}");
        }
    }
    assert_eq!(wide_log, narrow_log);
    for record in &wide_log {
        let text = record.to_string();
        assert!(!text.contains(CANARY), "{text}");
        assert!(!text.contains("slots_shown"), "{text}");
    }
}

const TERMINAL_BLOCK: &str = "{\"type\":\"text\",\"text\":\"Terminal: replies are ";
const TERMINAL_SECTION: &str = "workspace/terminal";

fn terminal_free(
    (bodies, records): (Vec<String>, Vec<serde_json::Value>),
    cols: u16,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let last = bodies.last().unwrap();
    let reply_width = format!("{TERMINAL_BLOCK}{cols} columns");
    assert!(last.contains(&reply_width), "{last}");
    let bodies = bodies
        .into_iter()
        .map(|mut body| {
            while let Some(start) = body.find(TERMINAL_BLOCK) {
                let end = body[start..].find("\"},").unwrap() + start + 3;
                body.replace_range(start..end, "");
            }
            body
        })
        .collect();
    let records = records
        .into_iter()
        .filter(|r| r["hook"] != TERMINAL_SECTION)
        .map(|mut record| {
            if let Some(lead) = record["lead"].as_array_mut() {
                lead.retain(|entry| entry["by"] != TERMINAL_SECTION);
            }
            if let Some(map) = record.as_object_mut() {
                map.remove("prefix");
            }
            record
        })
        .collect();
    (bodies, records)
}

fn unsequenced(records: &[serde_json::Value]) -> Vec<serde_json::Value> {
    records
        .iter()
        .map(|record| {
            let mut record = record.clone();
            if let Some(map) = record.as_object_mut() {
                map.remove("seq");
            }
            record
        })
        .collect()
}

fn is_fold_record(record: &serde_json::Value) -> bool {
    record["type"] == "settings_changed"
        && record["changes"].as_array().is_some_and(|changes| {
            !changes.is_empty()
                && changes
                    .iter()
                    .all(|c| c["target"]["name"] == "display.sidebar_folded")
        })
}

fn count_records(home: &Home, which: impl Fn(&serde_json::Value) -> bool) -> usize {
    home.records().iter().filter(|record| which(record)).count()
}

fn busy_title(pty: &Pty, open: bool) -> Option<usize> {
    let rows = panel_rows(&pty.shown());
    let title = rows.iter().position(|r| {
        let r = r.trim_start_matches(['\u{2502}', ' ']);
        r.starts_with("Busy") && r.contains("\u{25b8}") != open
    })?;
    let body = rows.iter().any(|r| r.contains(CANARY));
    (body || !open).then_some(title)
}

fn click_row(pty: &mut Pty, row: usize) {
    let row = row + 1;
    pty.send(format!("\x1b[<0;124;{row}M\x1b[<0;124;{row}m").as_bytes());
}

fn folded_session(cols: u16) -> (Vec<String>, Vec<serde_json::Value>) {
    let server = serve(vec![text_reply("first answer"), text_reply(DONE)]);
    let home = Home::new(&format!("sidebar-fold-{cols}"));
    let launch = Launch {
        cols,
        log: true,
        env: &[("WISP_TESTKIT", "sidebar")],
    };
    let mut pty = tui_at(
        &home,
        server.port,
        &[
            "--set",
            "plugins.testkit.slots.busy=on",
            "--set",
            "display.sidebar_pinned=testkit/busy",
        ],
        &launch,
    );
    pty.wait_shown(READY);
    if cols >= 120 {
        assert!(
            wait_until(Duration::from_secs(20), || docked(&pty)),
            "the busy section never docked: {}",
            pty.shown()
        );
    }
    pty.send(b"first question\r");
    pty.wait_shown("first answer");
    pty.wait_idle();
    if cols >= 120 {
        let mut title = None;
        let docked = wait_until(Duration::from_secs(20), || {
            title = busy_title(&pty, true);
            title.is_some()
        });
        assert!(docked, "the busy section never docked: {}", pty.shown());
        click_row(&mut pty, title.unwrap_or_default());
        let folded = wait_until(Duration::from_secs(20), || {
            title = busy_title(&pty, false);
            title.is_some()
        });
        assert!(folded, "the busy section never folded: {}", pty.shown());
        std::thread::sleep(Duration::from_millis(500));
        click_row(&mut pty, title.unwrap_or_default());
        let opened = wait_until(Duration::from_secs(20), || busy_title(&pty, true).is_some());
        assert!(
            opened,
            "the busy section never opened again: {}",
            pty.shown()
        );
        let saved = wait_until(PATIENCE, || count_records(&home, is_fold_record) == 2);
        assert!(
            saved,
            "the fold saves never finished: {}",
            count_records(&home, is_fold_record)
        );
    } else {
        assert!(!pty.shown().contains(CANARY), "{}", pty.shown());
    }
    pty.send(b"second question\r");
    pty.wait_shown(DONE);
    pty.wait_idle();
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let requests = server.bodies();
    (requests, session_records(&home, server.port))
}

#[test]
fn a_pinned_section_folded_and_opened_by_clicks_changes_no_request_and_logs_only_the_folds() {
    let (wide, wide_log) = terminal_free(folded_session(160), 115);
    let (narrow, narrow_log) = terminal_free(folded_session(100), 96);
    assert_prefix_stable(&turns(&wide));
    assert_eq!(wide.len(), 2, "{wide:#?}");
    assert_eq!(wide, narrow);
    let (folds, rest): (Vec<serde_json::Value>, Vec<serde_json::Value>) =
        wide_log.iter().cloned().partition(is_fold_record);
    let values: Vec<&serde_json::Value> = folds.iter().map(|f| &f["changes"][0]["value"]).collect();
    assert_eq!(
        values,
        [
            &serde_json::json!({"kind": "list", "text": "testkit/busy"}),
            &serde_json::json!({"kind": "list", "text": ""}),
        ],
        "{wide_log:#?}"
    );
    assert_eq!(unsequenced(&rest), unsequenced(&narrow_log));
    for record in &wide_log {
        let text = record.to_string();
        assert!(!text.contains(CANARY), "{text}");
        assert!(!text.contains("slots_shown"), "{text}");
    }
}

const PAGE_KEYS: &str = "q esc back";

fn key_row(shown: &str) -> String {
    shown
        .lines()
        .find(|l| l.contains(PAGE_KEYS))
        .unwrap_or_default()
        .to_owned()
}

fn page_session(open: bool) -> (Vec<String>, Vec<serde_json::Value>) {
    let server = serve(vec![text_reply("first answer"), text_reply(DONE)]);
    let home = Home::new(&format!("sidebar-page-{open}"));
    let launch = Launch {
        cols: 160,
        log: true,
        env: &[("WISP_TESTKIT", "sidebar")],
    };
    let mut pty = tui_at(
        &home,
        server.port,
        &["--set", "plugins.testkit.slots.busy=on"],
        &launch,
    );
    pty.wait_shown(READY);
    assert!(
        wait_until(Duration::from_secs(20), || docked(&pty)),
        "the busy section never docked: {}",
        pty.shown()
    );
    pty.send(b"first question\r");
    pty.wait_shown("first answer");
    pty.wait_idle();
    if open {
        pty.send(b"/sidebar testkit/busy\r");
        pty.wait_shown(PAGE_KEYS);
        pty.wait_shown(&format!("{CANARY} busy Page"));
        let page = pty.shown();
        let first = page.lines().next().unwrap_or_default();
        assert!(first.trim_start().starts_with("Busy"), "{page}");
        assert!(first.contains(&format!("{CANARY} busy Page")), "{page}");
        assert!(page.contains(PAGE_KEYS), "{page}");
        let before = key_row(&page);
        pty.send(b"\x1b[<65;60;10M");
        let scrolled = wait_until(Duration::from_secs(20), || {
            let row = key_row(&pty.shown());
            !row.is_empty() && row != before
        });
        assert!(scrolled, "the page never scrolled: {}", pty.shown());
        pty.send(b"\x1b");
        let closed = wait_until(Duration::from_secs(20), || {
            let shown = pty.shown();
            !shown.contains(PAGE_KEYS) && panel_rows(&shown).iter().any(|r| r.contains(CANARY))
        });
        assert!(closed, "the page never closed: {}", pty.shown());
    }
    pty.send(b"second question\r");
    pty.wait_shown(DONE);
    pty.wait_idle();
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let requests = server.bodies();
    (requests, unsequenced(&session_records(&home, server.port)))
}

#[test]
fn an_open_section_page_changes_no_request_and_adds_no_record() {
    let (opened, opened_log) = page_session(true);
    let (closed, closed_log) = page_session(false);
    assert_prefix_stable(&turns(&opened));
    assert_eq!(opened.len(), 2, "{opened:#?}");
    assert_eq!(opened, closed);
    for body in &opened {
        for leak in [CANARY, "testkit/busy", "\"Busy\""] {
            assert!(!body.contains(leak), "{leak}: {body}");
        }
    }
    assert_eq!(opened_log.len(), closed_log.len());
    for (a, b) in opened_log.iter().zip(&closed_log) {
        assert_eq!(a, b);
    }
    for record in &opened_log {
        let text = record.to_string();
        assert!(!text.contains(CANARY), "{text}");
        assert!(!text.contains("slots_shown"), "{text}");
        assert!(!text.contains("testkit/busy"), "{text}");
    }
}

fn context_section_session(cols: u16) -> (Vec<String>, Vec<serde_json::Value>) {
    let server = serve(vec![
        text_reply("first answer"),
        text_reply("second answer"),
        text_reply(DONE),
    ]);
    let home = Home::new(&format!("context-section-{cols}"));
    let launch = Launch {
        cols,
        log: true,
        env: &[],
    };
    let mut pty = tui_at(
        &home,
        server.port,
        &["--set", "plugins.context.slots.usage=on"],
        &launch,
    );
    pty.wait_shown(READY);
    let docked = |shown: &str| panel_rows(shown).iter().any(|r| r.contains("cached"));
    if cols >= 120 {
        pty.wait_screen("the context section docked", docked);
    }
    for (question, answer) in [
        ("first question", "first answer"),
        ("second question", "second answer"),
        ("third question", DONE),
    ] {
        pty.wait_idle();
        pty.send(format!("{question}\r").as_bytes());
        pty.wait_shown(answer);
        pty.wait_idle();
        if cols >= 120 {
            pty.wait_screen("the context section docked", docked);
        } else {
            assert!(!docked(&pty.shown()), "{}", pty.shown());
        }
    }
    let named = wait_until(PATIENCE, || {
        count_records(&home, |record| record["type"] == "session_named") == 1
    });
    assert!(named, "the naming request never finished: {}", pty.shown());
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let requests = server.bodies();
    (requests, session_records(&home, server.port))
}

fn turn_starts(records: &[serde_json::Value]) -> Vec<serde_json::Value> {
    records
        .iter()
        .filter(|r| r["type"] == "turn_started")
        .cloned()
        .collect()
}

#[test]
fn the_context_section_docked_over_several_turns_changes_no_request_and_no_record() {
    if !usable() {
        return;
    }
    let (wide, wide_log) = terminal_free(context_section_session(160), 115);
    let (narrow, narrow_log) = terminal_free(context_section_session(100), 96);
    assert_prefix_stable(&turns(&wide));
    assert_eq!(wide.len(), 3, "{wide:#?}");
    assert_eq!(wide, narrow);
    for body in &wide {
        assert!(!body.contains("context/usage"), "{body}");
        assert!(!body.contains("context/window"), "{body}");
        assert!(!body.contains("plugins.context.slots.window"), "{body}");
        assert!(body.contains("Context: "), "{body}");
    }
    let starts = turn_starts(&wide_log);
    assert_eq!(starts.len(), 3, "{wide_log:#?}");
    assert_eq!(starts, turn_starts(&narrow_log));
    assert!(
        starts.iter().all(|s| s.to_string().contains("Context: ")),
        "{starts:#?}"
    );
    assert_eq!(wide_log, narrow_log);
    for record in &wide_log {
        let text = record.to_string();
        assert!(!text.contains("context/usage"), "{text}");
        assert!(!text.contains("context/window"), "{text}");
        assert!(!text.contains("plugins.context.slots.window"), "{text}");
        assert!(!text.contains("slots_shown"), "{text}");
        assert!(!text.contains("slot_content"), "{text}");
    }
}

#[test]
fn a_prompt_section_is_sent_once_and_keeps_the_prefix() {
    let server = serve(vec![text_reply("first answer"), text_reply(DONE)]);
    let home = Home::new("prompt-section");
    let launch = Launch {
        env: &[("WISP_TESTKIT", "sections")],
        ..Launch::default()
    };
    let mut pty = tui_at(&home, server.port, &[], &launch);
    pty.wait_for(READY);
    pty.send(b"first question\r");
    pty.wait_shown("first answer");
    pty.wait_idle();
    pty.send(b"second question\r");
    pty.wait_for(DONE.as_bytes());
    pty.wait_idle();
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let requests = server.bodies();
    let turns = turns(&requests);
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert_prefix_stable(&turns);
    let notes = serde_json::to_string(&format!(
        "<notes>\n{}\n</notes>\n",
        wisp_plugin_testkit::NOTES
    ))
    .unwrap();
    for turn in &turns {
        assert_eq!(
            turn["messages"].to_string().matches(&notes).count(),
            1,
            "{turn}"
        );
    }
}

fn docked(pty: &Pty) -> bool {
    panel_rows(&pty.shown()).iter().any(|r| r.contains(CANARY))
}

#[test]
fn a_sidebar_plugin_switched_off_in_config_undocks_at_the_next_turn_and_adds_one_record() {
    if !usable() {
        return;
    }
    let server = serve(vec![text_reply("first answer"), text_reply(DONE)]);
    let home = Home::new("sidebar-switched-off");
    let launch = Launch {
        cols: 160,
        log: true,
        env: &[("WISP_TESTKIT", "sidebar")],
    };
    let mut pty = tui_at(
        &home,
        server.port,
        &["--set", "plugins.testkit.slots.busy=on"],
        &launch,
    );
    pty.wait_for(READY);
    pty.send(b"first question\r");
    pty.wait_shown("first answer");
    pty.wait_idle();
    assert!(
        wait_until(Duration::from_secs(20), || docked(&pty)),
        "the busy section never docked: {}",
        pty.shown()
    );
    pty.send(b"/config testkit\r");
    pty.wait_shown("enter turn off");
    pty.send(b"\r");
    pty.wait_shown("off \u{b7} p");
    pty.close_settings();
    assert!(
        wait_until(Duration::from_secs(5), || docked(&pty)),
        "the panel undocked before the next turn: {}",
        pty.shown()
    );
    pty.send(b"second question\r");
    pty.wait_for(DONE.as_bytes());
    pty.wait_idle();
    assert!(
        wait_until(Duration::from_secs(20), || !docked(&pty)),
        "the panel never undocked: {}",
        pty.shown()
    );
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let requests = server.bodies();
    assert_eq!(requests.len(), 2, "{requests:#?}");
    assert_prefix_stable(&turns(&requests));
    let records = session_records(&home, server.port);
    let changes: Vec<&serde_json::Value> = records
        .iter()
        .filter(|r| r.to_string().contains("plugins.testkit.enabled"))
        .collect();
    assert_eq!(changes.len(), 1, "{changes:#?}");
    assert_eq!(changes[0]["type"], "settings_changed", "{:#}", changes[0]);
}

#[test]
fn a_plugin_disabled_by_another_process_stays_on_in_the_running_session() {
    if !usable() {
        return;
    }
    let server = serve(vec![text_reply("first answer"), text_reply(DONE)]);
    let home = Home::new("plugin-disabled-elsewhere");
    let launch = Launch {
        cols: 160,
        log: false,
        env: &[],
    };
    let mut pty = tui_at(&home, server.port, &[], &launch);
    pty.wait_for(READY);
    pty.send(b"first question\r");
    pty.wait_shown("first answer");
    pty.wait_idle();
    let disabled = home
        .tools_command(env!("CARGO_BIN_EXE_wisp"), server.port)
        .args(["plugin", "disable", "todos"])
        .output()
        .unwrap();
    assert!(disabled.status.success(), "{disabled:?}");
    pty.send(b"second question\r");
    pty.wait_for(DONE.as_bytes());
    pty.wait_idle();
    pty.send(b"/config todos\r");
    pty.wait_shown("enter turn off");
    let shown = pty.shown();
    assert!(
        shown
            .lines()
            .any(|line| line.trim_start().starts_with("› todos  on")),
        "{shown}"
    );
    assert!(!shown.contains("pending"), "{shown}");
    pty.close_settings();
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let requests = server.bodies();
    assert_eq!(requests.len(), 2, "{requests:#?}");
    assert_prefix_stable(&turns(&requests));
    assert!(requests[1].contains("write_todos"), "{}", requests[1]);
}

#[test]
fn a_tool_turned_off_in_the_config_while_closed_goes_to_the_resumed_session_as_a_removal() {
    if !usable() {
        return;
    }
    let server = serve(vec![text_reply("first answer"), text_reply(DONE)]);
    let home = Home::new("resume-config-changed");
    let launch = Launch {
        cols: 160,
        log: true,
        env: &[],
    };
    let mut pty = tui_at(&home, server.port, &[], &launch);
    pty.wait_for(READY);
    pty.send(b"first question\r");
    pty.wait_shown("first answer");
    pty.wait_idle();
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let disabled = home
        .tools_command(env!("CARGO_BIN_EXE_wisp"), server.port)
        .args(["plugin", "disable", "show-diff"])
        .output()
        .unwrap();
    assert!(disabled.status.success(), "{disabled:?}");
    let mut pty = tui_at(&home, server.port, &["--resume"], &launch);
    pty.wait_for(READY);
    pty.send(b"second question\r");
    pty.wait_for(DONE.as_bytes());
    pty.wait_idle();
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status:?}");
    let requests = server.bodies();
    let turns = turns(&requests);
    assert_eq!(turns.len(), 2, "{requests:#?}");
    assert_eq!(turns[0]["system"], turns[1]["system"]);
    assert_eq!(turns[0]["tools"], turns[1]["tools"]);
    let before = turns[0]["messages"].as_array().unwrap();
    let after = turns[1]["messages"].as_array().unwrap();
    assert!(after.len() > before.len(), "{after:#?}");
    assert_eq!(before[..], after[..before.len()]);
    let removals: Vec<&serde_json::Value> = after[before.len()..]
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .filter(|b| b["type"] == "tool_removal")
        .collect();
    let names: Vec<&serde_json::Value> = removals.iter().map(|b| &b["tool"]["name"]).collect();
    assert_eq!(names, ["show_code", "show_diff"], "{after:#?}");
}

fn is_chat(request: &serde_json::Value) -> bool {
    request["messages"]
        .as_array()
        .and_then(|m| m.last())
        .is_some_and(|last| {
            last["role"] != "assistant" && !last.to_string().contains("Open questions")
        })
}

fn counted_server() -> Server {
    let count = std::sync::atomic::AtomicUsize::new(0);
    Server::start(move |request| {
        if support::server::is_count(&request.head) {
            return support::server::json("200 OK", r#"{"input_tokens":10}"#);
        }
        let sections = [
            "Goal and standing instructions",
            "Decisions",
            "State of the work",
            "Files, branches and artefacts",
            "Commands, errors and fixes",
            "Open questions",
        ];
        let summary: Vec<String> = sections.iter().map(|s| format!("## {s}\nNone.")).collect();
        let body: serde_json::Value = serde_json::from_str(&request.body).unwrap_or_default();
        let chat = is_chat(&body);
        let n = count.fetch_add(usize::from(chat), std::sync::atomic::Ordering::SeqCst) + 1;
        match chat {
            true => text_reply(&format!("REPLY-{n}")),
            false => {
                let mut start = message_start("msg_summary", "claude-sonnet-5-5", 1);
                start["message"]["usage"]["input_tokens"] = serde_json::json!(90_000);
                let text = format!("REPLY-{n}\n\n{}", summary.join("\n\n"));
                sse(&[
                    start,
                    serde_json::json!({"type": "content_block_start", "index": 0,
                        "content_block": {"type": "text", "text": ""}}),
                    serde_json::json!({"type": "content_block_delta", "index": 0,
                        "delta": {"type": "text_delta", "text": text}}),
                    serde_json::json!({"type": "content_block_stop", "index": 0}),
                    ending("end_turn")[0].clone(),
                    ending("end_turn")[1].clone(),
                ])
            }
        }
    })
}

fn said(pty: &mut Pty, text: &str, reply: usize) {
    pty.send(format!("{text}\r").as_bytes());
    pty.wait_for(format!("REPLY-{reply}"));
    pty.wait_idle();
}

fn plugin_row(pty: &mut Pty, id: &str, steps: &[(&[u8], &str)]) {
    pty.send(format!("/config {id}\r").as_bytes());
    pty.wait_shown(&format!("Filter  {id}"));
    let row = format!("{id}:");
    for _ in 0..30 {
        if wait_until(Duration::from_millis(300), || pty.shown().contains(&row)) {
            break;
        }
        pty.send(b"\x1b[B");
    }
    pty.wait_shown(&row);
    for (keys, shown) in steps {
        pty.send(keys);
        pty.wait_shown(shown);
    }
    pty.close_settings();
}

fn system_kinds(turn: &serde_json::Value) -> Vec<String> {
    turn["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .map(|b| {
            let name = b["tool"]["name"].as_str().unwrap_or_default();
            format!("{} {name}", b["type"].as_str().unwrap_or_default())
        })
        .collect()
}

fn rewound(pty: &mut Pty, reply: usize) {
    pty.send(b"/undo\r");
    pty.wait_for(b"enter choose");
    pty.send(b"\r");
    pty.wait_for(b"enter rewind");
    pty.send(b"\r");
    pty.wait_for(b"unchanged");
    std::thread::sleep(Duration::from_millis(200));
    said(pty, "", reply);
}

fn sentences(turn: &serde_json::Value) -> usize {
    serde_json::to_string(&turn["messages"])
        .unwrap()
        .matches("'Context:' line")
        .count()
}

#[test]
fn a_plugin_removed_and_added_back_in_config_changes_its_tools_inline_from_the_next_message() {
    if !usable() {
        return;
    }
    let server = counted_server();
    let home = Home::configured("plugin-remove-add-back");
    std::fs::write(
        home.file(&format!("{CONFIG_DIR}/config.toml")),
        "[plugins.context]\ninstalled = false\n",
    )
    .unwrap();
    let mut pty = tui(&home, server.port, &[]);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    said(&mut pty, "one", 1);
    plugin_row(
        &mut pty,
        "todos",
        &[
            (b"d", "Remove todos?"),
            (b"\x1b[A\r", "removed \u{b7} pending \u{b7} next turn"),
        ],
    );
    said(&mut pty, "two", 2);
    pty.wait_shown("plugins removed: todos");
    rewound(&mut pty, 3);
    for id in ["todos", "context"] {
        plugin_row(
            &mut pty,
            id,
            &[(b"\r", "on \u{b7} pending \u{b7} next turn")],
        );
    }
    said(&mut pty, "four", 4);
    pty.wait_shown("plugins added: context");
    rewound(&mut pty, 5);
    pty.send(b"/compact\r");
    pty.wait_shown("context compacted");
    pty.wait_idle();
    said(&mut pty, "six", 6);
    pty.send(b"\x04");
    assert!(wait_for_exit(&mut pty.child).success());
    let turns: Vec<serde_json::Value> = turns(&server.bodies())
        .into_iter()
        .filter(is_chat)
        .collect();
    assert_eq!(turns.len(), 6, "{turns:#?}");
    assert_prefix_stable(&turns[..2]);
    assert_prefix_stable(&[turns[0].clone(), turns[2].clone()]);
    assert_prefix_stable(&turns[2..4]);
    assert_prefix_stable(&[turns[2].clone(), turns[4].clone()]);
    for turn in [1, 2] {
        assert_eq!(system_kinds(&turns[turn]), ["tool_removal write_todos"]);
    }
    for turn in [3, 4] {
        let kinds = system_kinds(&turns[turn]);
        assert!(
            kinds.contains(&"tool_addition write_todos".to_owned()),
            "{kinds:?}"
        );
        assert_eq!(sentences(&turns[turn]), 1, "{:#?}", turns[turn]);
    }
    assert_eq!(turns[5]["tools"], turns[0]["tools"]);
    assert_eq!(turns[5]["system"], turns[0]["system"]);
    assert_eq!(sentences(&turns[5]), 1, "{:#?}", turns[5]);
}

#[test]
fn found_tool_requests_keep_their_prefix_across_steers_resume_rewind_and_copies() {
    let requests = |case: &str| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../wisp-core/tests/fixtures/requests/session")
            .join(case);
        let mut files: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        files
            .into_iter()
            .map(|path| {
                serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).unwrap()
            })
            .collect::<Vec<_>>()
    };
    for case in [
        "found_claude",
        "found_openai",
        "found_unlisted",
        "found_steered",
    ] {
        support::prefix::assert_prefix_stable(&requests(case), None);
    }
    let resumed = requests("found_resumed");
    support::prefix::assert_prefix_stable(&resumed[..3], None);
    support::prefix::assert_prefix_stable(
        &[resumed[0].clone(), resumed[3].clone(), resumed[4].clone()],
        None,
    );
    assert_eq!(resumed[1]["messages"], resumed[3]["messages"]);
    let rewound = requests("found_rewound");
    support::prefix::assert_prefix_stable(&rewound[..3], None);
    support::prefix::assert_prefix_stable(
        &[rewound[0].clone(), rewound[3].clone(), rewound[4].clone()],
        None,
    );
    assert!(!rewound[3]["messages"].to_string().contains("tool_addition"));
    let copied = requests("found_copies");
    support::prefix::assert_prefix_stable(&copied[..2], None);
    let mut summary = [copied[1].clone(), copied[3].clone()];
    summary.iter_mut().for_each(strip_cache_control);
    support::prefix::assert_prefix_stable(&summary, None);
    assert_eq!(copied[1]["messages"], copied[2]["messages"]);
    assert_eq!(copied[1]["tools"], copied[2]["tools"]);
    assert_eq!(copied[1]["system"], copied[2]["system"]);
    assert_eq!(copied[4], copied[5]);
}

#[test]
fn a_saved_provider_switch_changes_account_list_and_doctor_until_enabled_again() {
    let server = serve(vec![]);
    let home = Home::new("provider-commands");
    home.write(support::home::CONFIG, &format!("[endpoints.g1]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"GEMINI_TEST_KEY\"\nurl = \"http://127.0.0.1:{}\"\n", server.port));
    let command = |args: &[&str]| {
        home.tools_command(WISP, server.port)
            .env("GEMINI_TEST_KEY", "CANARY_GEMINI_COMMANDS")
            .args(args)
            .output()
            .unwrap()
    };
    let run = command(&["plugin", "disable", "gemini"]);
    assert!(run.status.success(), "{}", both(&run));
    let listed = command(&["account", "list"]);
    let shown = both(&listed);
    assert!(listed.status.success(), "{shown}");
    assert!(
        shown.contains("g1")
            && shown.contains("gemini is off")
            && shown.contains("wisp plugin enable gemini"),
        "{shown}"
    );
    let doctor = command(&["doctor", "--check", "g1"]);
    assert!(
        both(&doctor).contains("not checked: gemini is off"),
        "{}",
        both(&doctor)
    );
    assert!(server.bodies().is_empty());
    let run = command(&["plugin", "enable", "gemini"]);
    assert!(run.status.success(), "{}", both(&run));
    for args in [vec!["account", "list"], vec!["doctor"]] {
        let run = command(&args);
        assert!(run.status.success(), "{}", both(&run));
        assert!(both(&run).contains("g1"));
        assert!(!both(&run).contains("gemini is off"));
    }
}
