use serde_json::json;
use wisp_protocol::{SignalKind, ToolAction, ToolDisplay, ToolView, Typed};

use super::*;
use wisp_plugin_shell::NO_ID;

fn display(input: Value) -> ToolDisplay {
    futures::executor::block_on(entry(NAME).prepare(&input))
        .unwrap()
        .display()
        .clone()
}

fn view(input: Value) -> ToolView {
    display(input).view.unwrap()
}

fn process_view(
    id: Option<u32>,
    text: Option<&str>,
    keys: &[&str],
    signal: Option<SignalKind>,
    read: Option<&str>,
    pattern: Option<&str>,
) -> ToolView {
    ToolView::Process {
        id,
        text: text.map(str::to_owned),
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        signal,
        read: read.map(str::to_owned),
        pattern: pattern.map(str::to_owned),
    }
}

fn verdict(echoed: bool, unsent: bool, signalled: bool) -> Option<Typed> {
    Some(Typed {
        echoed,
        unsent,
        signalled,
    })
}

#[test]
fn every_prepared_call_carries_its_process_view() {
    let typed = display(json!({"id": 27, "input": "yes", "keys": ["Enter", "C-c", "F5"]}));
    assert_eq!(typed.action, ToolAction::Input);
    assert_eq!(typed.target, "27");
    assert_eq!(typed.title, None);
    assert_eq!(
        typed.view,
        Some(process_view(
            Some(27),
            Some("yes"),
            &["Enter", "C-c", "F5"],
            None,
            None,
            None
        ))
    );
    assert_eq!(
        view(json!({"id": 3, "keys": ["Enter"]})),
        process_view(Some(3), None, &["Enter"], None, None, None)
    );
    let interrupted = display(json!({"id": 3, "input": "q\n", "signal": "interrupt"}));
    assert_eq!(
        interrupted.action,
        ToolAction::Signal {
            kind: SignalKind::Interrupt
        }
    );
    assert_eq!(
        interrupted.view,
        Some(process_view(
            Some(3),
            Some("q\n"),
            &[],
            Some(SignalKind::Interrupt),
            None,
            None
        ))
    );
    let killed = display(json!({"id": 3, "input": "q", "signal": "kill"}));
    assert_eq!(
        killed.action,
        ToolAction::Signal {
            kind: SignalKind::Kill
        }
    );
    assert_eq!(
        killed.view,
        Some(process_view(
            Some(3),
            Some("q"),
            &[],
            Some(SignalKind::Kill),
            None,
            None
        ))
    );
    let waited = display(json!({"id": 3, "wait_ms": 500}));
    assert_eq!(waited.action, ToolAction::Wait);
    assert_eq!(
        waited.view,
        Some(process_view(Some(3), None, &[], None, None, None))
    );
    let searched = display(json!({"id": 3, "read": "search", "pattern": "err(or)?"}));
    assert_eq!(searched.action, ToolAction::Search);
    assert_eq!(
        searched.view,
        Some(process_view(
            Some(3),
            None,
            &[],
            None,
            Some("search"),
            Some("err(or)?")
        ))
    );
    for read in ["new", "tail", "lines", "screen"] {
        let shown = display(json!({"id": 3, "read": read}));
        assert_eq!(shown.action, ToolAction::Read);
        assert_eq!(
            shown.view,
            Some(process_view(Some(3), None, &[], None, Some(read), None))
        );
    }
    let listed = display(json!({"read": "tail"}));
    assert_eq!(
        listed.action,
        ToolAction::Other {
            name: NAME.to_owned()
        }
    );
    assert_eq!(listed.target, "");
    assert_eq!(
        listed.view,
        Some(process_view(None, None, &[], None, None, None))
    );
    let attach = display(json!({"id": 4, "ask_user_to_attach": "type the password"}));
    assert_eq!(attach.action, ToolAction::Attach);
    assert_eq!(
        attach.view,
        Some(process_view(Some(4), None, &[], None, None, None))
    );
}

#[test]
fn a_process_view_round_trips_and_an_older_reader_sees_an_unknown_view() {
    let shown = view(json!({"id": 9, "input": "y", "keys": ["Enter"], "signal": "interrupt"}));
    let json = serde_json::to_value(&shown).unwrap();
    assert_eq!(
        json,
        json!({"type": "process", "id": 9, "text": "y", "keys": ["Enter"], "signal": "interrupt"})
    );
    assert_eq!(serde_json::from_value::<ToolView>(json).unwrap(), shown);
    assert_eq!(
        serde_json::to_value(view(json!({}))).unwrap(),
        json!({"type": "process"})
    );
}

#[test]
fn a_refused_process_call_never_shows_its_input() {
    let tool = entry(NAME);
    let marker = "marker-c0ffee-9d1";
    let cases = [
        (
            json!({"id": 5, "input": marker, "keys": ["Bogus", "Enter"]}),
            "unknown key `Bogus`",
        ),
        (
            json!({"id": 5, "input": marker, "color": "red"}),
            "unknown field `color`",
        ),
        (json!({"input": marker}), NO_ID),
        (
            json!({"id": null, "input": marker, "keys": ["Enter"]}),
            NO_ID,
        ),
        (
            json!({"id": 5, "input": marker, "keys": [marker]}),
            "unknown key `marker-c0ffee-9d1`",
        ),
    ];
    for (input, refusal) in cases {
        let message = futures::executor::block_on(tool.prepare(&input))
            .unwrap_err()
            .message;
        assert!(message.starts_with(refusal), "{message}");
        let display = futures::executor::block_on(tool.rejected_display(&input)).unwrap();
        let json = serde_json::to_string(&display).unwrap();
        assert!(!json.contains(marker), "{json}");
        assert!(matches!(
            display.view,
            Some(ToolView::Process { text: None, .. })
        ));
        let toolbox = crate::tool::testkit::shell(Timing::default());
        assert_eq!(
            futures::executor::block_on(toolbox.rejected_display(NAME, &input)),
            display
        );
    }
    let refused = futures::executor::block_on(tool.rejected_display(
        &json!({"id": 5, "input": marker, "keys": ["Bogus", "Enter"], "signal": "interrupt"}),
    ))
    .unwrap();
    assert_eq!(
        refused.action,
        ToolAction::Signal {
            kind: SignalKind::Interrupt
        }
    );
    assert_eq!(refused.target, "5");
    assert_eq!(
        refused.view,
        Some(process_view(
            Some(5),
            None,
            &["Enter"],
            Some(SignalKind::Interrupt),
            None,
            None
        ))
    );
    let listing =
        futures::executor::block_on(tool.rejected_display(&json!({"input": marker}))).unwrap();
    assert_eq!(
        listing.action,
        ToolAction::Other {
            name: NAME.to_owned()
        }
    );
    assert_eq!(listing.target, "");
    assert_eq!(
        futures::executor::block_on(tool.rejected_display(&json!(marker)))
            .unwrap()
            .view,
        Some(process_view(None, None, &[], None, None, None))
    );
    assert_eq!(tool.meta().writing.as_deref(), Some("input for a process"));
}

async fn until_mode(rig: &Rig, id: u32, want: impl Fn(&Handle) -> bool) {
    let handle = rig.processes.handle(id).unwrap();
    assert!(
        until(|| want(&handle)).await,
        "the terminal never changed mode"
    );
}

fn line_mode(handle: &Handle) -> bool {
    handle
        .state
        .terminal()
        .is_some_and(|t| t.canonical() == Some(true))
}

fn raw_mode(handle: &Handle) -> bool {
    handle
        .state
        .terminal()
        .is_some_and(|t| t.canonical() == Some(false))
}

#[tokio::test]
async fn cat_in_a_terminal_echoes_what_was_typed() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("verdict-cat");
    rig.bash(terminal("echo armed; exec cat")).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    until_mode(&rig, 1, line_mode).await;
    let typed = rig
        .process(json!({"id": 1, "input": "hello", "keys": ["Enter"], "until": "^hello$"}))
        .await;
    assert_eq!(
        typed.run.typed,
        verdict(true, false, false),
        "{}",
        typed.text
    );
    let read = rig.process(json!({"id": 1})).await;
    assert_eq!(read.run.typed, None);
    let stopped = rig.process(json!({"id": 1, "signal": "interrupt"})).await;
    assert_eq!(
        stopped.run.typed,
        verdict(false, false, true),
        "{}",
        stopped.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_hidden_prompt_is_never_echoed() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("verdict-read-s");
    rig.bash(terminal(
        "read -s secret; echo \"got ${#secret}\"; exec sleep 30",
    ))
    .await;
    until_mode(&rig, 1, |h| h.state.check_hidden()).await;
    let typed = rig
        .process(json!({"id": 1, "input": "pw", "keys": ["Enter"], "until": "^got 2"}))
        .await;
    assert_eq!(
        typed.run.typed,
        verdict(false, false, false),
        "{}",
        typed.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_raw_mode_program_is_never_echoed() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("verdict-raw");
    rig.bash(terminal(
        "stty raw -echo; echo armed; head -c 3 > got.bin; echo done; exec sleep 30",
    ))
    .await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    until_mode(&rig, 1, raw_mode).await;
    let typed = rig
        .process(json!({"id": 1, "input": "abc", "until": "done"}))
        .await;
    assert_eq!(
        typed.run.typed,
        verdict(false, false, false),
        "{}",
        typed.text
    );
    assert_eq!(fs::read(rig.path("got.bin")).unwrap(), b"abc");
    rig.finish().await;
}

fn python_with_readline() -> bool {
    let found = crate::jobs::exec::find_program("python3").is_some_and(|python| {
        std::process::Command::new(python)
            .args(["-c", "import readline"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    });
    if !found {
        eprintln!("skipped: no python3 with readline");
    }
    found
}

#[tokio::test]
async fn pythons_prompt_is_never_echoed() {
    if !has_ctty() || !python_with_readline() {
        return;
    }
    let mut rig = Rig::new("verdict-python");
    rig.bash(terminal("exec python3 -q -i")).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "^>>>"}))
        .await;
    until_mode(&rig, 1, raw_mode).await;
    let typed = rig
        .process(json!({"id": 1, "input": "print(6 * 7)", "keys": ["Enter"], "until": "^42"}))
        .await;
    assert_eq!(
        typed.run.typed,
        verdict(false, false, false),
        "{}",
        typed.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn the_mode_at_the_write_decides_even_when_echo_goes_off_after_it() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("verdict-after");
    rig.bash(terminal(
        "echo armed; read line; stty -echo; echo changed; exec sleep 30",
    ))
    .await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    until_mode(&rig, 1, line_mode).await;
    let typed = rig
        .process(json!({"id": 1, "input": "hi", "keys": ["Enter"], "until": "^changed"}))
        .await;
    assert!(typed.text.contains("changed"), "{}", typed.text);
    until_mode(&rig, 1, |h| h.state.check_hidden()).await;
    assert_eq!(
        typed.run.typed,
        verdict(true, false, false),
        "{}",
        typed.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_terminal_that_is_killed_reports_the_kill_as_sent() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("verdict-tty-kill");
    rig.bash(terminal("echo armed; exec cat")).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    until_mode(&rig, 1, line_mode).await;
    let killed = rig.process(json!({"id": 1, "signal": "kill"})).await;
    assert_eq!(
        killed.run.typed,
        verdict(false, false, true),
        "{}",
        killed.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_pipe_is_never_echoed_and_sends_its_signals() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("verdict-pipe");
    rig.bash(background("cat > got.txt")).await;
    let typed = rig
        .process(json!({"id": 1, "input": "hi", "keys": ["Enter"], "wait_ms": 0}))
        .await;
    assert_eq!(
        typed.run.typed,
        verdict(false, false, false),
        "{}",
        typed.text
    );
    until_file(&rig.path("got.txt"), "hi\n").await;
    let killed = rig.process(json!({"id": 1, "signal": "kill"})).await;
    assert_eq!(
        killed.run.typed,
        verdict(false, false, true),
        "{}",
        killed.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_pipe_sends_its_interrupt_after_a_short_write() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("verdict-pipe-short");
    rig.bash(background("exec sleep 30")).await;
    let fill = "a".repeat(wisp_plugin_shell::MAX_INPUT);
    let first = rig
        .process(json!({"id": 1, "input": fill, "wait_ms": 0}))
        .await;
    assert!(first.run.typed.is_some(), "{}", first.text);
    let short = rig
        .process(json!({"id": 1, "input": fill, "signal": "interrupt", "wait_ms": 0}))
        .await;
    assert!(short.text.contains("weren't written"), "{}", short.text);
    assert_eq!(
        short.run.typed,
        verdict(false, true, true),
        "{}",
        short.text
    );
    rig.finish().await;
}
