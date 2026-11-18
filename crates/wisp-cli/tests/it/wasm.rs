#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket, file or process error"
)]

use crate::support;

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::prefix::assert_prefix_stable;
use support::pty::{
    PATIENCE, Pty, READY, Stdout, usable, wait_for_exit, wait_per_step, wait_until,
};
use support::server::{
    Server, agent_of, is_child, is_count, json as json_reply, message_start, sse, sse_reply,
    tool_reply,
};
use support::{
    WISP,
    home::{CONFIG, Home, both, command_in, err, mode, out},
};

const DONE: &str = "WASM-DONE";
const INSTRUCTION: &str = "Summarise the conversation for the WASM fixture.";
const FIXTURE_TOOLS: [&str; 11] = [
    "count", "echo", "files", "probe", "spawn", "spin", "stall", "stream", "trap", "wait", "whoami",
];

const FOLDER: &str = ".local/share/wisp/plugins/wasm-fixture";
const CACHE: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/wasm-cli");

fn sha(wasm: &[u8]) -> String {
    wisp_plugin_api::hex::encode(&Sha256::digest(wasm))
}

fn add(home: &Home, manifest: &Path, wasm: &Path) -> Output {
    locked_output(
        command(home, WISP)
            .args(["plugin", "add"])
            .arg(manifest)
            .arg(wasm),
    )
}

fn locked_output(command: &mut Command) -> Output {
    let cache = std::fs::File::open(CACHE).unwrap();
    cache.lock().unwrap();
    command.output().unwrap()
}

fn install(home: &Home) -> String {
    let manifest = Path::new(wisp_wasm_guests::WASM_FIXTURE_MANIFEST);
    let added = add(home, manifest, Path::new(wisp_wasm_guests::WASM_FIXTURE));
    assert!(added.status.success(), "{}", both(&added));
    sha(&fixture())
}

fn variant(home: &Home, name: &str, from: &str, to: &str) -> std::path::PathBuf {
    let text = std::fs::read_to_string(wisp_wasm_guests::WASM_FIXTURE_MANIFEST).unwrap();
    assert!(text.contains(from), "{from}");
    home.write(
        &format!("{name}/wisp-plugin.toml"),
        &text.replacen(from, to, 1),
    )
}

fn files(home: &Home) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(home.file(FOLDER))
        .into_iter()
        .flatten()
        .filter_map(|e| e.unwrap().file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

fn wisp(home: &Home, args: &[&str]) -> Output {
    command(home, WISP).args(args).output().unwrap()
}

fn done(turn: usize) -> String {
    sse_reply(
        &json!({"type": "text", "text": ""}),
        &format!("{DONE}-{turn}"),
    )
}

fn serve(replies: Vec<String>) -> Server {
    let replies = Mutex::new(replies.into_iter());
    Server::start(move |request| {
        if is_count(&request.head) {
            return json_reply("200 OK", r#"{"input_tokens":10}"#);
        }
        replies.lock().unwrap().next().unwrap_or_else(|| done(0))
    })
}

fn turns(server: &Server) -> Vec<Value> {
    server
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|s| !is_count(&s.head))
        .map(|s| serde_json::from_str(&s.body).unwrap())
        .collect()
}

fn command(home: &Home, program: &str) -> Command {
    let mut command = command_in(&home.0, program);
    std::fs::create_dir_all(CACHE).unwrap();
    command
        .env("TMPDIR", home.dir("tmp"))
        .env("XDG_CACHE_HOME", CACHE);
    command
}

fn session(home: &Home, dir: &Path, server: &Server, args: &[&str]) -> Pty {
    let mut command = command(home, "setsid");
    command
        .current_dir(dir)
        .args(["--ctty", "--wait", WISP, "--time", "off"])
        .args(args)
        .env("TERM", "xterm-256color")
        .env("ANTHROPIC_API_KEY", "wasm-test-key")
        .env("ANTHROPIC_BASE_URL", server.url());
    let pty = Pty::start(&mut command, 30, 160, Stdout::Tty);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty
}

fn say(pty: &mut Pty, text: &str, turn: usize) {
    pty.send(format!("{text}\r").as_bytes());
    pty.wait_for(format!("{DONE}-{turn}"));
    pty.wait_idle();
}

fn end(mut pty: Pty) {
    pty.send(b"\x04");
    let status = wait_for_exit(&mut pty.child);
    assert!(status.success(), "{status}");
}

fn names(request: &Value) -> Vec<&str> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect()
}

fn tool_result(turns: &[Value], id: &str) -> Value {
    turns
        .iter()
        .flat_map(|t| t["messages"].as_array().cloned().unwrap_or_default())
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == id)
        .unwrap_or_else(|| panic!("no result for {id}"))
}

fn result_text(turns: &[Value], id: &str) -> String {
    let block = tool_result(turns, id);
    match &block["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect(),
        other => other.to_string(),
    }
}

fn fixture() -> Vec<u8> {
    std::fs::read(wisp_wasm_guests::WASM_FIXTURE).unwrap()
}

fn newer(home: &Home) -> (std::path::PathBuf, String) {
    let mut wasm = fixture();
    wasm.extend([0, 4, 3, b'n', b'e', b'w']);
    let path = home.file("newer.wasm");
    std::fs::write(&path, &wasm).unwrap();
    (path, sha(&wasm))
}

fn pair(sha: &str) -> [String; 2] {
    [format!("{sha}.toml"), format!("{sha}.wasm")]
}

#[test]
fn adding_from_the_two_files_writes_the_pair_and_lists_it_as_wasm() {
    let server = serve(Vec::new());
    let home = Home::new("wasm-list");
    let added = locked_output(
        command(&home, WISP)
            .env("ANTHROPIC_API_KEY", "wasm-test-key")
            .env("ANTHROPIC_BASE_URL", server.url())
            .args(["plugin", "add"])
            .args([
                wisp_wasm_guests::WASM_FIXTURE_MANIFEST,
                wisp_wasm_guests::WASM_FIXTURE,
            ]),
    );
    assert!(added.status.success(), "{}", both(&added));
    let sha = sha(&fixture());
    let short = &sha[..8];
    assert_eq!(
        out(&added),
        format!(
            "added wasm-fixture 0.1.0 (wasm, {short}): tools count, echo, files, probe, spawn, spin, stall, stream, trap, wait and whoami, on from your next message\n"
        )
    );
    assert_eq!(files(&home), pair(&sha));
    assert_eq!(mode(&home.file(FOLDER)), 0o700);
    for name in pair(&sha) {
        assert_eq!(
            mode(&home.file(&format!("{FOLDER}/{name}"))),
            0o600,
            "{name}"
        );
    }
    assert_eq!(
        home.read(&format!("{FOLDER}/{sha}.toml")),
        std::fs::read_to_string(wisp_wasm_guests::WASM_FIXTURE_MANIFEST).unwrap()
    );
    let listed = out(&wisp(&home, &["plugin", "list"]));
    assert!(
        listed.contains(&format!("wasm-fixture 0.1.0 (wasm, {short}) on\n")),
        "{listed}"
    );
    assert!(
        listed.contains(&format!("tools: {}", FIXTURE_TOOLS.join(", "))),
        "{listed}"
    );
    let set = wisp(&home, &["config", "set", "plugins.wasm-fixture.depth", "4"]);
    assert!(set.status.success(), "{}", both(&set));
    assert!(home.read(CONFIG).contains("depth = 4"));
    let refused = wisp(&home, &["config", "set", "plugins.wasm-fixture.depth", "6"]);
    assert_eq!(refused.status.code(), Some(2), "{}", both(&refused));
    assert!(err(&refused).contains("from 1 to 5"), "{}", both(&refused));
    assert!(home.read(CONFIG).contains("depth = 4"));
    assert!(server.seen.lock().unwrap().is_empty());
}

#[test]
fn adding_the_same_files_twice_says_so_and_a_newer_version_replaces_the_pair() {
    let home = Home::new("wasm-again");
    let old = install(&home);
    home.write(&format!("{FOLDER}/{}.wasm", "0".repeat(64)), "");
    let again = add(
        &home,
        Path::new(wisp_wasm_guests::WASM_FIXTURE_MANIFEST),
        Path::new(wisp_wasm_guests::WASM_FIXTURE),
    );
    assert!(again.status.success(), "{}", both(&again));
    assert_eq!(
        out(&again),
        format!(
            "wasm-fixture 0.1.0 (wasm, {}) is already added\n",
            &old[..8]
        )
    );
    assert_eq!(files(&home), pair(&old));
    let edited = variant(
        &home,
        "edited",
        "summary = \"Test",
        "summary = \"Edited test",
    );
    let added = add(&home, &edited, Path::new(wisp_wasm_guests::WASM_FIXTURE));
    assert!(
        out(&added).starts_with("added wasm-fixture 0.1.0"),
        "{}",
        both(&added)
    );
    assert_eq!(
        home.read(&format!("{FOLDER}/{old}.toml")),
        std::fs::read_to_string(&edited).unwrap()
    );
    assert_eq!(files(&home), pair(&old));
    let (wasm, new) = newer(&home);
    let manifest = variant(&home, "newer", "version = \"0.1.0\"", "version = \"0.2.0\"");
    let added = add(&home, &manifest, &wasm);
    assert!(added.status.success(), "{}", both(&added));
    let shown = format!("wasm-fixture 0.2.0 (wasm, {})", &new[..8]);
    assert!(
        out(&added).starts_with(&format!("added {shown}: ")),
        "{}",
        out(&added)
    );
    assert_eq!(files(&home), pair(&new));
    let listed = out(&wisp(&home, &["plugin", "list"]));
    assert!(listed.contains(&format!("{shown} on\n")), "{listed}");
}

#[test]
fn each_refused_add_says_why_and_adds_nothing_under_plugins() {
    let home = Home::new("wasm-refused");
    let summary = "summary = \"Test tools of the WASM test component\"";
    let tools = "\n[[tools]]";
    let cases = [
        (
            "clash",
            "name = \"count\"",
            "name = \"bash\"",
            "a tool of shell",
        ),
        (
            "cycle",
            tools,
            "\n[system]\ntext = \"Use the fixture.\"\nafter = [\"time\"]\nbefore = [\"time\"]\n\n[[tools]]",
            "cycle",
        ),
        (
            "setting",
            summary,
            "summary = \"Fixture\"\nsettings = [\"key\"]",
            "not a plain setting",
        ),
        (
            "reserved",
            "id = \"wasm-fixture\"",
            "id = \"shell\"",
            "reserved",
        ),
        (
            "unknown",
            summary,
            "summary = \"Fixture\"\ncolour = \"red\"",
            "colour",
        ),
        (
            "world",
            "world = \"0.3.0\"",
            "world = \"0.1.0\"",
            "WASM plugins name world 0.2.0 or 0.3.0",
        ),
        (
            "mismatch",
            "world = \"0.3.0\"",
            "world = \"0.2.0\"",
            "its component doesn't fit world 0.2.0",
        ),
    ];
    for (name, from, to, why) in cases {
        let manifest = variant(&home, name, from, to);
        if matches!(name, "world" | "mismatch" | "setting") {
            let text = std::fs::read_to_string(&manifest).unwrap();
            let text = text.split("\n[[settings]]").next().unwrap();
            let text = if name == "setting" {
                text
            } else {
                text.split("\n[[hooks]]").next().unwrap()
            };
            std::fs::write(&manifest, text.replace("index = true\n", "")).unwrap();
        }
        let refused = add(&home, &manifest, Path::new(wisp_wasm_guests::WASM_FIXTURE));
        assert_eq!(refused.status.code(), Some(2), "{name}: {}", both(&refused));
        let said = err(&refused);
        assert!(said.starts_with("wisp: "), "{name}: {said}");
        assert!(said.contains(why), "{name}: {said}");
        assert!(said.ends_with("; nothing was added\n"), "{name}: {said}");
        assert!(out(&refused).is_empty(), "{name}: {}", out(&refused));
        assert!(!home.exists(".local/share/wisp/plugins"), "{name}");
    }
}

fn doctor_plugins(home: &Home) -> String {
    let doctor = out(&wisp(home, &["doctor"]));
    doctor
        .split("\nplugins\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\n").next())
        .map_or_else(|| format!("no plugins section in\n{doctor}"), str::to_owned)
}

#[test]
fn a_stopped_add_keeps_the_old_version_and_two_complete_pairs_load_neither() {
    let home = Home::new("wasm-pairs");
    let old = install(&home);
    let (wasm, new) = newer(&home);
    std::fs::copy(&wasm, home.file(&format!("{FOLDER}/{new}.wasm"))).unwrap();
    let listed = out(&wisp(&home, &["plugin", "list"]));
    assert!(
        listed.contains(&format!("wasm-fixture 0.1.0 (wasm, {}) on\n", &old[..8])),
        "{listed}"
    );
    std::fs::copy(
        wisp_wasm_guests::WASM_FIXTURE_MANIFEST,
        home.file(&format!("{FOLDER}/{new}.toml")),
    )
    .unwrap();
    let listed = out(&wisp(&home, &["plugin", "list"]));
    assert!(!listed.contains("wasm-fixture"), "{listed}");
    let pad = " ".repeat(6);
    assert_eq!(
        doctor_plugins(&home),
        format!(
            "  wasm-fixture{pad} ~/{FOLDER}\n  wasm-fixture{pad} its folder holds more than one version; run wisp plugin add again"
        )
    );
    install(&home);
    assert_eq!(files(&home), pair(&old));
    let listed = out(&wisp(&home, &["plugin", "list"]));
    assert!(listed.contains("wasm-fixture 0.1.0 (wasm, "), "{listed}");
}

#[test]
fn remove_then_add_brings_the_plugin_back_with_its_files_and_data() {
    let home = Home::configured("wasm-remove");
    let sha = install(&home);
    let kept = home.write(".local/share/wisp/plugin-data/wasm-fixture/kept", "kept");
    let shown = format!("wasm-fixture 0.1.0 (wasm, {})", &sha[..8]);
    for (args, state) in [
        (["plugin", "remove", "wasm-fixture"], "removed"),
        (["plugin", "add", "wasm-fixture"], "on"),
        (["plugin", "remove", "wasm-fixture"], "removed"),
    ] {
        let run = wisp(&home, &args);
        assert!(run.status.success(), "{}", both(&run));
        let listed = out(&wisp(&home, &["plugin", "list"]));
        assert!(listed.contains(&format!("{shown} {state}\n")), "{listed}");
        assert_eq!(files(&home), pair(&sha));
    }
    let manifest = variant(&home, "back", "summary = \"Test", "summary = \"Back test");
    let back = add(&home, &manifest, Path::new(wisp_wasm_guests::WASM_FIXTURE));
    assert_eq!(
        out(&back),
        format!(
            "added {shown}: tools {}, on from your next session\nSaved plugins.wasm-fixture.installed = on (was off).\n",
            "count, echo, files, probe, spawn, spin, stall, stream, trap, wait and whoami"
        ),
        "{}",
        both(&back)
    );
    let listed = out(&wisp(&home, &["plugin", "list"]));
    assert!(listed.contains(&format!("{shown} on\n")), "{listed}");
    assert_eq!(std::fs::read_to_string(kept).unwrap(), "kept");
    let off = wisp(&home, &["plugin", "disable", "wasm-fixture"]);
    assert!(off.status.success(), "{}", both(&off));
    let manifest = variant(&home, "off", "summary = \"Test", "summary = \"Off test");
    let added = add(&home, &manifest, Path::new(wisp_wasm_guests::WASM_FIXTURE));
    assert!(
        out(&added).ends_with(", off (wisp plugin enable wasm-fixture turns it on)\n"),
        "{}",
        both(&added)
    );
    let listed = out(&wisp(&home, &["plugin", "list"]));
    assert!(listed.contains(&format!("{shown} off\n")), "{listed}");
}

#[test]
fn a_saved_tools_list_gains_the_default_on_tools_and_an_unsaved_one_stays_unsaved() {
    let home = Home::configured("wasm-tools-saved");
    let set = wisp(&home, &["config", "set", "tools", "bash"]);
    assert!(set.status.success(), "{}", both(&set));
    let added = add(
        &home,
        Path::new(wisp_wasm_guests::WASM_FIXTURE_MANIFEST),
        Path::new(wisp_wasm_guests::WASM_FIXTURE),
    );
    assert!(added.status.success(), "{}", both(&added));
    let sha = sha(&fixture());
    assert_eq!(
        out(&added),
        format!(
            "added wasm-fixture 0.1.0 (wasm, {}): tools {}, on from your next message\nSaved tools = bash, {} (was bash).\n",
            &sha[..8],
            "count, echo, files, probe, spawn, spin, stall, stream, trap, wait and whoami",
            FIXTURE_TOOLS.join(", ")
        )
    );
    let config = home.read(CONFIG);
    for tool in ["bash"].iter().chain(&FIXTURE_TOOLS) {
        assert!(
            config.contains(&format!("\"{tool}\"")),
            "{tool} in {config}"
        );
    }
    let without_echo = ["bash"]
        .iter()
        .chain(FIXTURE_TOOLS.iter().filter(|t| **t != "echo"))
        .copied()
        .collect::<Vec<_>>()
        .join(",");
    let set = wisp(&home, &["config", "set", "tools", &without_echo]);
    assert!(set.status.success(), "{}", both(&set));
    let wasm = Path::new(wisp_wasm_guests::WASM_FIXTURE);
    let renamed = variant(&home, "renamed", "name = \"count\"", "name = \"tally\"");
    let added = add(&home, &renamed, wasm);
    assert!(added.status.success(), "{}", both(&added));
    assert!(
        out(&added).ends_with(&format!(
            "Saved tools = bash, {} (was bash, {}).\n",
            "files, probe, spawn, spin, stall, stream, tally, trap, wait, whoami",
            "count, files, probe, spawn, spin, stall, stream, trap, wait, whoami"
        )),
        "{}",
        out(&added)
    );
    let config = home.read(CONFIG);
    assert!(
        config.contains("\"tally\"")
            && !config.contains("\"count\"")
            && !config.contains("\"echo\""),
        "{config}"
    );
    let ghost = config.replace("\"bash\"", "\"ghost_tool\"");
    home.write(CONFIG, &ghost);
    let manifest = variant(&home, "ghost", "summary = \"Test", "summary = \"Ghost test");
    let added = add(&home, &manifest, wasm);
    assert!(added.status.success(), "{}", both(&added));
    assert!(
        err(&added).starts_with(
            "wisp: wasm-fixture's tools weren't added to your saved tools list: ghost_tool is no tool"
        ),
        "{}",
        both(&added)
    );
    assert_eq!(home.read(CONFIG), ghost);
    let none = wisp(&home, &["config", "set", "tools", "none"]);
    assert!(none.status.success(), "{}", both(&none));
    let before = home.read(CONFIG);
    let manifest = variant(&home, "none", "summary = \"Test", "summary = \"None test");
    let added = add(&home, &manifest, wasm);
    assert!(added.status.success(), "{}", both(&added));
    assert_eq!(home.read(CONFIG), before);
    let home = Home::configured("wasm-tools-unsaved");
    install(&home);
    assert!(
        !home.read(CONFIG).contains("tools"),
        "{}",
        home.read(CONFIG)
    );
}

#[test]
fn two_data_folders_and_working_directories_send_the_same_prefix() {
    if !usable() {
        return;
    }
    let mut firsts = Vec::new();
    for name in ["wasm-prefix-a", "wasm-prefix-b"] {
        let server = serve(vec![
            tool_reply(
                "toolu_write",
                "board_write",
                &json!({"board": "notes", "cards": [{"id": "a", "title": "A", "log": "noted"}]}),
            ),
            done(1),
            tool_reply("toolu_read", "board_read", &json!({"board": "notes"})),
            done(2),
        ]);
        let home = Home::new(name);
        install(&home);
        install_board(&home);
        let dir = home.dir(&format!("work-{name}"));
        let mut pty = session(&home, &dir, &server, &[]);
        say(&mut pty, "hello", 1);
        say(&mut pty, "again", 2);
        end(pty);
        let turns = turns(&server);
        assert_prefix_stable(&turns, None);
        firsts.push(turns[0].clone());
    }
    assert_eq!(firsts[0]["tools"], firsts[1]["tools"]);
    assert_eq!(firsts[0]["system"], firsts[1]["system"]);
    let names = names(&firsts[0]);
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    for tool in FIXTURE_TOOLS.iter().chain(&["board_read", "board_write"]) {
        assert!(names.contains(tool), "{tool} in {names:?}");
    }
}

#[test]
fn a_session_without_wasm_plugins_sends_what_an_empty_plugins_folder_sends() {
    if !usable() {
        return;
    }
    let mut firsts = Vec::new();
    for (name, folder) in [("wasm-none", false), ("wasm-empty", true)] {
        let server = serve(vec![done(1)]);
        let home = Home::new(name);
        if folder {
            home.dir(".local/share/wisp/plugins");
        }
        let mut pty = session(&home, &home.0, &server, &[]);
        say(&mut pty, "hello", 1);
        end(pty);
        let first = turns(&server).remove(0);
        assert!(!names(&first).contains(&"whoami"));
        firsts.push((first["tools"].clone(), first["system"].clone()));
    }
    assert_eq!(firsts[0], firsts[1]);
}

#[test]
fn a_cut_wait_restarts_the_instance_and_leaves_the_prefix_unchanged() {
    if !usable() {
        return;
    }
    let server = serve(vec![
        tool_reply("toolu_count1", "count", &json!({})),
        done(1),
        tool_reply(
            "toolu_wait",
            "wait",
            &json!({"ms": 600_000, "stubborn": true}),
        ),
        tool_reply("toolu_count2", "count", &json!({})),
        done(2),
    ]);
    let home = Home::new("wasm-cut");
    install(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "count", 1);
    pty.send(b"wait\r");
    pty.wait_screen("the running wait", |s| s.matches("Using wait").count() >= 2);
    pty.send(b"\x1b");
    pty.wait_idle();
    say(&mut pty, "count again", 2);
    end(pty);
    let turns = turns(&server);
    assert_eq!(result_text(&turns, "toolu_count1"), "1");
    assert_eq!(result_text(&turns, "toolu_count2"), "1");
    for pair in turns.windows(2) {
        assert_eq!(pair[0]["tools"], pair[1]["tools"]);
        assert_eq!(pair[0]["system"], pair[1]["system"]);
    }
}

#[test]
fn switching_a_fixture_tool_off_and_on_changes_it_inline() {
    if !usable() {
        return;
    }
    let server = serve(vec![done(1), done(2), done(3)]);
    let home = Home::new("wasm-switch");
    install(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "hello", 1);
    pty.switch_tool("whoami", "[x]");
    say(&mut pty, "again", 2);
    pty.switch_tool("whoami", "[ ]");
    say(&mut pty, "once more", 3);
    end(pty);
    let turns = turns(&server);
    assert!(
        !home.logs().contains("compaction_failed"),
        "{}",
        home.logs()
    );
    assert_eq!(turns.len(), 3);
    assert_prefix_stable(&turns, None);
    let text = serde_json::to_string(&turns[2]["messages"]).unwrap();
    assert!(text.contains("whoami"), "{text}");
}

fn only_session(home: &Home) -> String {
    let names: Vec<String> = std::fs::read_dir(home.file(".local/share/wisp/sessions"))
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".jsonl").map(str::to_owned))
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    names[0].clone()
}

fn resumed(name: &str, between: impl FnOnce(&mut Pty)) -> Vec<Value> {
    let server = serve(vec![
        tool_reply("toolu_count1", "count", &json!({})),
        done(1),
        tool_reply("toolu_count2", "count", &json!({})),
        done(2),
    ]);
    let home = Home::new(name);
    let sha = install(&home);
    install_board(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "count", 1);
    between(&mut pty);
    end(pty);
    let source = format!(
        r#"{{"id":"wasm-fixture","version":"0.1.0","source":{{"type":"wasm","sha256":"{sha}"}}}}"#
    );
    assert!(home.logs().contains(&source), "{}", home.logs());
    let id = only_session(&home);
    let mut pty = session(&home, &home.0, &server, &["--resume", &id]);
    say(&mut pty, "count again", 2);
    end(pty);
    let turns = turns(&server);
    assert_eq!(result_text(&turns, "toolu_count2"), "1");
    turns
}

#[test]
fn a_resume_with_the_config_unchanged_sends_the_logged_prefix_through_a_fresh_instance() {
    if !usable() {
        return;
    }
    let turns = resumed("wasm-resume", |_| {});
    assert_eq!(turns.len(), 4);
    assert_prefix_stable(&turns, None);
    assert!(
        !serde_json::to_string(&turns[2])
            .unwrap()
            .contains("tools_changed")
    );
}

#[test]
fn a_resume_after_an_inline_change_sends_the_same_prefix() {
    if !usable() {
        return;
    }
    let turns = resumed("wasm-resume-inline", |pty| pty.switch_tool("whoami", "[x]"));
    assert_eq!(turns.len(), 4);
    assert_prefix_stable(&turns, None);
}

fn changed_on_disk(
    name: &str,
    before: impl FnOnce(&Home),
    change: impl FnOnce(&Home),
    shows: &str,
) -> (Home, Vec<Value>) {
    let server = serve(vec![done(1), done(2), done(3)]);
    let home = Home::new(name);
    install(&home);
    before(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "hello", 1);
    change(&home);
    say(&mut pty, "again", 2);
    pty.wait_for(shows);
    end(pty);
    let id = only_session(&home);
    let mut pty = session(&home, &home.0, &server, &["--resume", &id]);
    say(&mut pty, "once more", 3);
    end(pty);
    let turns = turns(&server);
    assert_eq!(turns.len(), 3);
    assert_prefix_stable(&turns, None);
    assert!(
        home.logs().contains("\"type\":\"plugins_changed\""),
        "{}",
        home.logs()
    );
    (home, turns)
}

fn tool_blocks(turn: &Value) -> String {
    serde_json::to_string(&turn["messages"]).unwrap()
}

#[test]
fn a_plugin_installed_mid_session_on_a_trial_pair_waits_and_joins_inline_on_resume() {
    if !usable() {
        return;
    }
    let (home, turns) = changed_on_disk(
        "wasm-hot-install",
        |_| {},
        install_board,
        "plugin board: its tools come on in new sessions",
    );
    assert!(home.logs().contains("\"added\":[{\"id\":\"board\""));
    assert!(!names(&turns[0]).contains(&"board_read"));
    assert_eq!(turns[0]["tools"], turns[2]["tools"]);
    assert!(!tool_blocks(&turns[1]).contains("board_read"));
    let sent = tool_blocks(&turns[2]);
    assert!(
        sent.contains("tool_addition") && sent.contains("board_read"),
        "{sent}"
    );
}

#[test]
fn a_plugin_updated_mid_session_replaces_the_old_and_a_resume_keeps_the_prefix() {
    if !usable() {
        return;
    }
    let (home, turns) = changed_on_disk(
        "wasm-hot-update",
        |_| {},
        |home| {
            let (wasm, _) = newer(home);
            let manifest = variant(home, "newer", "version = \"0.1.0\"", "version = \"0.2.0\"");
            let added = add(home, &manifest, &wasm);
            assert!(added.status.success(), "{}", both(&added));
        },
        "plugins updated: wasm-fixture 0.2.0 (wasm, ",
    );
    assert_eq!(turns[0]["tools"], turns[2]["tools"]);
    assert!(
        home.logs()
            .contains("\"updated\":[{\"id\":\"wasm-fixture\""),
        "{}",
        home.logs()
    );
}

#[test]
fn a_plugin_folder_deleted_mid_session_leaves_inline_and_a_resume_keeps_the_prefix() {
    if !usable() {
        return;
    }
    let (_, turns) = changed_on_disk(
        "wasm-hot-delete",
        install_board,
        |home| std::fs::remove_dir_all(home.file(".local/share/wisp/plugins/board")).unwrap(),
        "plugins removed: board 0.1.0 (wasm, ",
    );
    assert!(names(&turns[0]).contains(&"board_read"));
    assert_eq!(turns[0]["tools"], turns[1]["tools"]);
    let sent = tool_blocks(&turns[1]);
    assert!(
        sent.contains("tool_removal") && sent.contains("board_write"),
        "{sent}"
    );
}

#[test]
fn two_children_send_identical_prefixes_and_each_has_its_own_instance() {
    if !usable() {
        return;
    }
    let counting = || {
        vec![
            step(&[], "toolu_count", "count", json!({})),
            step(&[], "toolu_report", "report", json!({"text": "Counted."})),
        ]
    };
    let root = vec![
        spawn("toolu_spawn_a", "a"),
        spawn("toolu_spawn_b", "b"),
        step(&["/a reported", "/b reported"], "", "", Value::Null),
    ];
    let (_, turns) = run_tree(
        "wasm-children",
        install,
        vec![("/", root), ("/a", counting()), ("/b", counting())],
    );
    let kids: Vec<&Value> = turns.iter().filter(|t| is_child(&t.to_string())).collect();
    let firsts: Vec<&&Value> = kids
        .iter()
        .filter(|t| t["messages"].as_array().unwrap().len() == 1)
        .collect();
    assert_eq!(firsts.len(), 2);
    assert_eq!(firsts[0]["tools"], firsts[1]["tools"]);
    assert_eq!(firsts[0]["system"], firsts[1]["system"]);
    assert!(names(firsts[0]).contains(&"count"));
    let counts: Vec<String> = kids
        .iter()
        .filter(|t| t["messages"].as_array().unwrap().len() == 3)
        .map(|t| result_text(&[(*t).clone()], "toolu_count"))
        .collect();
    assert_eq!(counts, ["1", "1"]);
    let root = turns.iter().rfind(|t| !is_child(&t.to_string())).unwrap();
    assert!(root.to_string().contains("2 agents spawned"), "{root}");
}

fn outcomes(home: &Home, hook: &str) -> Vec<Value> {
    home.logs()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["type"] == "hook_ran" && record["hook"] == hook)
        .map(|record| record["outcome"].clone())
        .collect()
}

fn kinds(home: &Home, hook: &str) -> Vec<String> {
    outcomes(home, hook)
        .iter()
        .map(|outcome| outcome["type"].as_str().unwrap().to_owned())
        .collect()
}

fn typed(turn: &Value) -> String {
    let messages = turn["messages"].as_array().unwrap();
    serde_json::to_string(messages.last().unwrap()).unwrap()
}

#[test]
fn the_fixtures_hooks_shape_the_requests_and_keep_the_prefix_stable() {
    if !usable() {
        return;
    }
    let server = serve(vec![
        tool_reply("toolu_block", "echo", &json!({"text": "block me"})),
        tool_reply("toolu_rewrite", "echo", &json!({"text": "rewrite me"})),
        done(1),
        done(2),
    ]);
    let home = Home::new("wasm-hooks");
    install(&home);
    let on = [
        "--set",
        "plugins.wasm-fixture.slots.status=on",
        "--set",
        "plugins.wasm-fixture.slots.loops=on",
    ];
    let mut pty = session(&home, &home.0, &server, &on);
    pty.wait_shown("wasm-fixture status: panel");
    say(&mut pty, "hello", 1);
    pty.wait_shown("couldn't update: timed out");
    say(&mut pty, "again", 2);
    end(pty);
    let turns = turns(&server);
    assert_prefix_stable(&turns, None);
    for turn in &turns {
        let turn = turn.to_string();
        for leak in [
            "wasm-fixture status",
            "wasm-fixture loops",
            "wasm-fixture/status",
        ] {
            assert!(!turn.contains(leak), "{leak}: {turn}");
        }
    }
    let last = typed(turns.last().unwrap());
    assert!(last.contains("set up (resumed: true)"), "{last}");
    let system = turns[0]["system"].to_string();
    assert!(system.contains("wasm-fixture clock: 0"), "{system}");
    let first = typed(&turns[0]);
    assert!(
        first.contains("wasm-fixture set up (resumed: false), 0 agents spawned"),
        "{first}"
    );
    assert_eq!(
        result_text(&turns, "toolu_block"),
        "Blocked by wasm-fixture/block: the fixture blocks this input"
    );
    assert_eq!(
        result_text(&turns, "toolu_rewrite"),
        "rewritten by the fixture"
    );
    assert_eq!(kinds(&home, "wasm-fixture/setup"), ["none", "none"]);
    assert_eq!(kinds(&home, "wasm-fixture/lead"), ["applied", "applied"]);
    assert_eq!(kinds(&home, "wasm-fixture/block"), ["blocked", "none"]);
    assert_eq!(kinds(&home, "wasm-fixture/rewrite"), ["applied"]);
}

#[test]
fn a_cut_or_trapped_hook_restarts_the_plugin_and_session_start_runs_again_first() {
    if !usable() {
        return;
    }
    let server = serve(vec![done(1), done(2)]);
    let home = Home::new("wasm-restart");
    install(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "loop", 1);
    say(&mut pty, "trap", 2);
    end(pty);
    let turns = turns(&server);
    assert_prefix_stable(&turns, None);
    for turn in &turns {
        let typed = typed(turn);
        assert!(typed.contains("set up (resumed: true)"), "{typed}");
    }
    assert_eq!(kinds(&home, "wasm-fixture/loops"), ["timed_out", "none"]);
    assert_eq!(kinds(&home, "wasm-fixture/traps")[0], "none");
    assert_eq!(
        outcomes(&home, "wasm-fixture/traps")[1],
        json!({"type": "failed", "message": "the wasm-fixture plugin panicked"})
    );
    assert_eq!(kinds(&home, "wasm-fixture/setup"), ["none", "none", "none"]);
}

#[test]
fn points_fire_from_a_wasm_hook_stops_at_the_depth_cap() {
    if !usable() {
        return;
    }
    let server = serve(vec![done(1)]);
    let home = Home::new("wasm-depth");
    install(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "fire", 1);
    end(pty);
    assert_eq!(
        outcomes(&home, "wasm-fixture/relay"),
        [
            json!({"type": "rejected", "rule": "depth"}),
            json!({"type": "none"}),
            json!({"type": "none"}),
            json!({"type": "none"}),
        ]
    );
}

#[test]
fn the_fixtures_send_command_starts_a_turn() {
    if !usable() {
        return;
    }
    let server = serve(vec![done(1)]);
    let home = Home::new("wasm-command");
    install(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    pty.send(b"/wasm-fixture:send hello from the fixture\r");
    pty.wait_for(format!("{DONE}-1"));
    pty.wait_idle();
    end(pty);
    let typed = typed(&turns(&server)[0]);
    assert!(typed.contains("hello from the fixture"), "{typed}");
}

#[test]
fn a_compaction_through_the_fixtures_strategy_keeps_the_prefix() {
    if !usable() {
        return;
    }
    let mut start = message_start("msg_summary", "claude-opus-5-5", 1);
    start["message"]["usage"]["input_tokens"] = json!(90_000);
    let summary = sse(&[
        start,
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "WASM-SUMMARY"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "usage": {"output_tokens": 5},
            "delta": {"stop_reason": "end_turn", "stop_sequence": null}}),
        json!({"type": "message_stop"}),
    ]);
    let server = Server::start(move |request| {
        if is_count(&request.head) {
            json_reply("200 OK", r#"{"input_tokens":10}"#)
        } else if request.body.contains(INSTRUCTION) {
            summary.clone()
        } else {
            done(if request.body.contains("second words") {
                2
            } else {
                1
            })
        }
    });
    let home = Home::new("wasm-strategy");
    install(&home);
    let strategy = ["--set", "context.strategy=wasm-fixture"];
    let mut pty = session(&home, &home.0, &server, &strategy);
    say(&mut pty, "hello", 1);
    pty.send(b"/compact\r");
    pty.wait_idle();
    say(&mut pty, "second words", 2);
    end(pty);
    let turns: Vec<Value> = turns(&server)
        .into_iter()
        .filter(|turn| turn["tools"].is_array())
        .collect();
    for turn in &turns[1..] {
        assert_eq!(turn["tools"], turns[0]["tools"]);
        assert_eq!(turn["system"], turns[0]["system"]);
    }
    let instruction = format!("{INSTRUCTION}\\nKeep the fixture's notes.");
    assert!(turns.iter().any(|turn| typed(turn).contains(&instruction)));
    let after = turns.last().unwrap().to_string();
    assert!(
        after.contains("The fixture's summary of 1 messages:"),
        "{after}"
    );
    assert!(after.contains("WASM-SUMMARY"), "{after}");
}

const DOCS: &str = include_str!("../../../../docs/plugins.md");
const BUILD_BOARD: &str = "cargo build --release --target wasm32-wasip2 -p board";
const ADD_BOARD: &str =
    "wisp plugin add plugins/board/wisp-plugin.toml target/wasm32-wasip2/release/board.wasm";
const BOARDS: &str = ".local/share/wisp/plugin-data/board/boards";

fn install_board(home: &Home) {
    assert!(DOCS.contains(BUILD_BOARD) && DOCS.contains(ADD_BOARD));
    let mut args: Vec<&str> = ADD_BOARD.split_whitespace().skip(1).collect();
    args.pop();
    let added = locked_output(
        command(home, WISP)
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .args(args)
            .arg(wisp_wasm_guests::BOARD),
    );
    assert!(added.status.success(), "{}", both(&added));
    assert!(
        out(&added).contains("tools board_read and board_write, on from your next message"),
        "{}",
        out(&added)
    );
}

type Step = (&'static [&'static str], &'static str, &'static str, Value);

fn step(
    after: &'static [&'static str],
    id: &'static str,
    tool: &'static str,
    input: Value,
) -> Step {
    (after, id, tool, input)
}

fn scripted(body: &str, steps: &[Step]) -> String {
    for (after, id, tool, input) in steps {
        if !id.is_empty() && body.contains(&format!("\"{id}\"")) {
            continue;
        }
        return match (after.iter().all(|needle| body.contains(needle)), id) {
            (false, _) => done(1),
            (true, &"") => done(2),
            (true, _) => tool_reply(id, tool, input),
        };
    }
    done(2)
}

fn tree(scripts: Vec<(&'static str, Vec<Step>)>) -> Server {
    Server::start(move |request| {
        if is_count(&request.head) {
            return json_reply("200 OK", r#"{"input_tokens":10}"#);
        }
        let agent = agent_of(&request.body);
        let steps = scripts
            .iter()
            .find(|(path, _)| *path == agent)
            .map_or(&[][..], |(_, steps)| steps.as_slice());
        scripted(&request.body, steps)
    })
}

fn ready(child: &'static str, text: &'static str) -> Step {
    let id = if child == "a" {
        "toolu_a_ready"
    } else {
        "toolu_b_ready"
    };
    step(&[], id, "report", json!({"text": text}))
}

fn spawn(id: &'static str, name: &str) -> Step {
    step(
        &[],
        id,
        "spawn_agent",
        json!({"name": name, "task": "Work on your card."}),
    )
}

fn send(after: &'static [&'static str], id: &'static str, to: &str, text: &str) -> Step {
    step(after, id, "send_message", json!({"to": to, "text": text}))
}

fn write(after: &'static [&'static str], id: &'static str, input: Value) -> Step {
    step(after, id, "board_write", input)
}

fn read(after: &'static [&'static str], id: &'static str, input: Value) -> Step {
    step(after, id, "board_read", input)
}

fn run_tree<T>(
    name: &str,
    plugin: impl FnOnce(&Home) -> T,
    scripts: Vec<(&'static str, Vec<Step>)>,
) -> (Home, Vec<Value>) {
    let server = tree(scripts);
    let home = Home::new(name);
    plugin(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    pty.send(b"start\r");
    let last = format!("{DONE}-2");
    assert!(
        wait_per_step(&server, || pty.saw(&last)),
        "never saw {last:?}, and no step for {PATIENCE:?} after {} requests: {}",
        turns(&server).len(),
        pty.shown()
    );
    pty.wait_idle();
    end(pty);
    let turns = turns(&server);
    (home, turns)
}

fn answer(turns: &[Value], id: &str) -> (bool, String) {
    let block = tool_result(turns, id);
    (block["is_error"] == true, result_text(turns, id))
}

fn newest_file(home: &Home, tree: Option<&str>, name: &str) -> PathBuf {
    let boards = home.file(BOARDS);
    let folder = match tree {
        Some(tree) => boards.join(tree),
        None => {
            let trees: Vec<_> = std::fs::read_dir(&boards)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            assert_eq!(trees.len(), 1, "{trees:?}");
            trees[0].clone()
        }
    };
    let newest = std::fs::read_dir(&folder)
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().into_string().ok())
        .filter_map(|file| {
            let n = file
                .strip_prefix(&format!("{name}."))?
                .strip_suffix(".json")?;
            n.parse::<u64>().ok()
        })
        .max()
        .unwrap();
    folder.join(format!("{name}.{newest}.json"))
}

fn on_disk(home: &Home, tree: Option<&str>, name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(newest_file(home, tree, name)).unwrap()).unwrap()
}

fn card<'a>(board: &'a Value, id: &str) -> &'a Value {
    board["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
}

#[test]
fn two_children_move_their_own_cards_at_once_and_are_refused_the_rest() {
    if !usable() {
        return;
    }
    let child = |me: &'static str,
                 other: &'static str,
                 ids: [&'static str; 4],
                 go: &'static [&'static str],
                 done_text: &'static str| {
        vec![
            ready(me, if me == "a" { "READY-A" } else { "READY-B" }),
            write(
                go,
                ids[0],
                json!({"board": "phase2", "cards": [{"id": me, "column": "doing", "log": format!("{me} moved")}]}),
            ),
            write(
                &[],
                ids[1],
                json!({"board": "phase2", "cards": [{"id": other, "column": "done"}]}),
            ),
            write(
                &[],
                ids[2],
                json!({"board": "phase2", "cards": [{"id": me, "title": "Renamed"}]}),
            ),
            step(&[], ids[3], "report", json!({"text": done_text})),
        ]
    };
    let root = vec![
        spawn("toolu_r_spawn_a", "a"),
        spawn("toolu_r_spawn_b", "b"),
        write(
            &["READY-A", "READY-B"],
            "toolu_r_create",
            json!({"board": "phase2", "cards": [
            {"id": "a", "title": "A", "assignee": "/a"},
            {"id": "b", "title": "B", "assignee": "/b"},
            {"id": "c", "title": "C"}]}),
        ),
        send(&[], "toolu_r_go_a", "a", "GO-A"),
        send(&[], "toolu_r_go_b", "b", "GO-B"),
        read(
            &["DONE-A", "DONE-B"],
            "toolu_r_read",
            json!({"board": "phase2"}),
        ),
    ];
    let (home, turns) = run_tree(
        "board-children",
        install_board,
        vec![
            ("/", root),
            (
                "/a",
                child(
                    "a",
                    "b",
                    [
                        "toolu_a_move",
                        "toolu_a_other",
                        "toolu_a_title",
                        "toolu_a_done",
                    ],
                    &["GO-A"],
                    "DONE-A",
                ),
            ),
            (
                "/b",
                child(
                    "b",
                    "a",
                    [
                        "toolu_b_move",
                        "toolu_b_other",
                        "toolu_b_title",
                        "toolu_b_done",
                    ],
                    &["GO-B"],
                    "DONE-B",
                ),
            ),
        ],
    );
    assert_eq!(
        answer(&turns, "toolu_r_create"),
        (false, "board phase2: created, 3 cards added".to_owned())
    );
    for (me, other) in [("a", "b"), ("b", "a")] {
        assert_eq!(
            answer(&turns, &format!("toolu_{me}_move")),
            (false, "board phase2: 1 card updated, 1 log line".to_owned())
        );
        assert_eq!(
            answer(&turns, &format!("toolu_{me}_other")),
            (
                true,
                "board phase2 belongs to main; only it and each card's assignee may change it"
                    .to_owned()
            ),
            "{me} moving {other}'s card"
        );
        assert_eq!(
            answer(&turns, &format!("toolu_{me}_title")),
            (
                true,
                format!(
                    "you may change card {me}'s column, note and log; only main may change the rest"
                )
            )
        );
    }
    let board = on_disk(&home, None, "phase2");
    for me in ["a", "b"] {
        let card = card(&board, me);
        assert_eq!(
            (card["column"].as_str(), card["title"].as_str()),
            (Some("doing"), Some(me.to_uppercase().as_str()))
        );
        let log = &card["log"][0];
        assert_eq!(
            (
                log["author"].clone(),
                log["column"].clone(),
                log["text"].clone()
            ),
            (
                json!(format!("/{me}")),
                json!("doing"),
                json!(format!("{me} moved"))
            )
        );
    }
    assert_eq!(card(&board, "c")["column"], "todo");
    let shown = result_text(&turns, "toolu_r_read");
    assert!(
        shown.starts_with(
            "Output:\nboard phase2 · owner main · edit owner\ntodo (1)\n  c · C\ndoing (2)\n"
        ),
        "{shown}"
    );
}

#[test]
fn a_handed_over_board_obeys_its_new_owner_and_its_edit_setting() {
    if !usable() {
        return;
    }
    let root = vec![
        spawn("toolu_r_spawn_a", "a"),
        spawn("toolu_r_spawn_b", "b"),
        write(
            &["READY-A", "READY-B"],
            "toolu_r_create",
            json!({"board": "phase2", "cards": [
            {"id": "a", "title": "A", "assignee": "/a"},
            {"id": "b", "title": "B", "assignee": "/b"}]}),
        ),
        write(
            &[],
            "toolu_r_hand_a",
            json!({"board": "phase2", "owner": "/a"}),
        ),
        write(
            &[],
            "toolu_r_after",
            json!({"board": "phase2", "cards": [{"id": "c", "title": "C"}]}),
        ),
        send(&[], "toolu_r_go_a", "a", "GO-A"),
        send(&["DONE-A"], "toolu_r_go_b", "b", "GO-B"),
        step(
            &["DONE-B"],
            "toolu_r_end_a",
            "stop_agent",
            json!({"name": "a", "end": true}),
        ),
        send(&[], "toolu_r_take_b", "b", "TAKE-B"),
        write(
            &["TOOK-B"],
            "toolu_r_hand_b",
            json!({"board": "phase2", "owner": "/b"}),
        ),
    ];
    let a = vec![
        ready("a", "READY-A"),
        write(
            &["GO-A"],
            "toolu_a_tree",
            json!({"board": "phase2", "edit": "tree"}),
        ),
        step(&[], "toolu_a_done", "report", json!({"text": "DONE-A"})),
    ];
    let b = vec![
        ready("b", "READY-B"),
        write(
            &["GO-B"],
            "toolu_b_add",
            json!({"board": "phase2", "columns": ["todo", "doing", "review", "done"],
            "cards": [{"id": "x", "title": "X", "column": "review"}]}),
        ),
        write(
            &[],
            "toolu_b_edit",
            json!({"board": "phase2", "edit": "owner"}),
        ),
        step(&[], "toolu_b_done", "report", json!({"text": "DONE-B"})),
        write(
            &["TAKE-B"],
            "toolu_b_take",
            json!({"board": "phase2", "owner": "/b"}),
        ),
        step(&[], "toolu_b_took", "report", json!({"text": "TOOK-B"})),
    ];
    let (home, turns) = run_tree(
        "board-handover",
        install_board,
        vec![("/", root), ("/a", a), ("/b", b)],
    );
    let only =
        "only board phase2's owner, /a, may change its edit setting, delete it or hand it over";
    let expected = [
        (
            "toolu_r_hand_a",
            false,
            "board phase2 now belongs to /a".to_owned(),
        ),
        (
            "toolu_r_after",
            true,
            "board phase2 belongs to /a; only it and each card's assignee may change it".to_owned(),
        ),
        ("toolu_a_tree", false, "board phase2: edit tree".to_owned()),
        (
            "toolu_b_add",
            false,
            "board phase2: 1 card added, columns set".to_owned(),
        ),
        ("toolu_b_edit", true, only.to_owned()),
        (
            "toolu_b_take",
            true,
            format!("{only}; /a has ended, and main can hand the board over"),
        ),
        (
            "toolu_r_hand_b",
            false,
            "board phase2 now belongs to /b".to_owned(),
        ),
    ];
    for (id, error, text) in expected {
        assert_eq!(answer(&turns, id), (error, text), "{id}");
    }
    let board = on_disk(&home, None, "phase2");
    assert_eq!(board["owner"], "/b");
    assert_eq!(board["edit"], "tree");
    assert_eq!(card(&board, "x")["column"], "review");
}

#[test]
fn boards_outlive_their_session_belong_to_their_tree_and_another_tree_only_reads_them() {
    if !usable() {
        return;
    }
    let home = Home::new("board-trees");
    install_board(&home);
    let server = serve(vec![
        tool_reply(
            "toolu_create",
            "board_write",
            &json!({"board": "phase2", "cards": [{"id": "a", "title": "A"}]}),
        ),
        done(1),
    ]);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "create", 1);
    end(pty);
    let old = only_session(&home);
    let server = serve(vec![
        tool_reply("toolu_read", "board_read", &json!({"board": "phase2"})),
        tool_reply(
            "toolu_log",
            "board_write",
            &json!({"board": "phase2", "cards": [{"id": "a", "log": "resumed"}]}),
        ),
        done(2),
    ]);
    let mut pty = session(&home, &home.0, &server, &["--resume", &old]);
    say(&mut pty, "again", 2);
    end(pty);
    let turns_resumed = turns(&server);
    assert!(result_text(&turns_resumed, "toolu_read").contains("  a · A\n"));
    assert_eq!(
        result_text(&turns_resumed, "toolu_log"),
        "board phase2: 1 log line"
    );
    let qualified = format!("{old}/phase2");
    let server = serve(vec![
        tool_reply("toolu_list", "board_read", &json!({})),
        tool_reply("toolu_own", "board_write", &json!({"board": "phase2"})),
        tool_reply("toolu_old", "board_read", &json!({"board": qualified})),
        tool_reply(
            "toolu_theirs",
            "board_write",
            &json!({"board": qualified, "cards": [{"id": "a", "log": "x"}]}),
        ),
        tool_reply("toolu_mine", "board_read", &json!({})),
        done(3),
    ]);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "fresh", 3);
    end(pty);
    let turns = turns(&server);
    let listed = result_text(&turns, "toolu_list");
    let fresh = listed
        .strip_prefix("Output:\nno boards in tree ")
        .and_then(|rest| rest.strip_suffix(" yet\n").or(rest.strip_suffix(" yet")))
        .unwrap_or_else(|| panic!("{listed}"))
        .to_owned();
    assert_ne!(fresh, old);
    assert_eq!(result_text(&turns, "toolu_own"), "board phase2: created");
    let theirs = result_text(&turns, "toolu_old");
    assert!(
        theirs.starts_with(&format!(
            "Output:\nboard phase2 of tree {old} · owner main · edit owner\n"
        )),
        "{theirs}"
    );
    assert!(
        theirs.contains("    ") && theirs.contains(" · todo · main · resumed"),
        "{theirs}"
    );
    assert_eq!(
        answer(&turns, "toolu_theirs"),
        (
            true,
            format!(
                "Invalid input: board {qualified} belongs to another tree; you can read it with board_read"
            )
        )
    );
    let mine = result_text(&turns, "toolu_mine");
    assert!(
        mine.contains(&format!(
            "boards of tree {fresh}\nphase2 · owner main · edit owner · 0 cards · "
        )),
        "{mine}"
    );
    assert_eq!(
        card(&on_disk(&home, Some(&old), "phase2"), "a")["log"][0]["text"],
        "resumed"
    );
    assert_eq!(on_disk(&home, Some(&fresh), "phase2")["cards"], json!([]));
}

#[test]
fn esc_ends_a_board_call_whose_file_never_answers_and_the_session_goes_on() {
    if !usable() {
        return;
    }
    let server = serve(vec![
        tool_reply(
            "toolu_create",
            "board_write",
            &json!({"board": "phase2", "cards": [{"id": "a", "title": "A"}]}),
        ),
        done(1),
        tool_reply("toolu_stuck", "board_read", &json!({"board": "phase2"})),
        tool_reply(
            "toolu_other",
            "board_write",
            &json!({"board": "other", "cards": [{"id": "b", "title": "B"}]}),
        ),
        done(2),
    ]);
    let home = Home::new("board-hung");
    install_board(&home);
    let mut pty = session(&home, &home.0, &server, &[]);
    say(&mut pty, "create", 1);
    let version = newest_file(&home, None, "phase2");
    std::fs::remove_file(&version).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(&version)
            .status()
            .unwrap()
            .success()
    );
    pty.send(b"read\r");
    pty.wait_screen("the stuck read", |s| s.contains("Reading board phase2"));
    pty.send(b"\x1b");
    pty.wait_screen("the stopped turn", |s| s.contains("Stopped."));
    pty.wait_idle();
    say(&mut pty, "go on", 2);
    let turns = turns(&server);
    assert_eq!(
        result_text(&turns, "toolu_other"),
        "board other: created, 1 card added"
    );
    assert!(wait_until(PATIENCE, || {
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed())
            .open(&version)
            .is_ok()
    }));
    end(pty);
}
