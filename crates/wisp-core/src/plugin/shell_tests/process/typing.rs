use serde_json::json;
use wisp_protocol::{BackgroundReason, ToolOutcome};

use super::*;
use crate::jobs::exec::pty::QUEUED_REPLIES;
use wisp_plugin_shell::line_limit_text;

const HAND_BACK: Duration = Duration::from_secs(1);

fn handing_back(name: &str) -> Rig {
    Rig::with_timing(
        name,
        Timing {
            hand_back_ms: u64::try_from(HAND_BACK.as_millis()).unwrap(),
            ..window(QUIET, WINDOW)
        },
    )
}

const SHELL: &str = "exec env PS1='ready> ' bash --norc --noprofile -i";

#[tokio::test]
async fn an_interactive_shell_takes_lines_interrupts_and_eof() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-shell");
    rig.bash(terminal(SHELL)).await;
    let prompt = rig
        .process(json!({"id": 1, "wait_ms": 10_000, "until": "^ready>"}))
        .await;
    assert!(prompt.text.contains("ready>"), "{}", prompt.text);
    let echoed = rig
        .process(json!({"id": 1, "input": "echo hi", "keys": ["Enter"], "until": "^hi$"}))
        .await;
    assert!(echoed.text.contains("\nhi"), "{}", echoed.text);
    rig.process(json!({
        "id": 1,
        "input": "sh -c 'echo started; exec sleep 30'; echo after",
        "keys": ["Enter"],
        "until": "^started$"
    }))
    .await;
    let interrupted = rig
        .process(json!({"id": 1, "keys": ["C-c"], "until": "^ready>$"}))
        .await;
    assert!(
        interrupted.text.starts_with("Process 1 is running."),
        "{}",
        interrupted.text
    );
    assert!(!interrupted.text.contains("after"), "{}", interrupted.text);
    rig.process(json!({
        "id": 1,
        "input": "sh -c 'echo again; exec sleep 30'",
        "keys": ["Enter"],
        "until": "^again$"
    }))
    .await;
    let signalled = rig
        .process(json!({"id": 1, "signal": "interrupt", "until": "^ready>$", "wait_ms": 5_000}))
        .await;
    assert!(
        signalled.text.starts_with("Process 1 is running."),
        "{}",
        signalled.text
    );
    let ended = rig
        .process(json!({"id": 1, "keys": ["C-d"], "wait_ms": 5_000}))
        .await;
    assert!(
        ended.text.starts_with("Process 1 exited with code 130."),
        "{}",
        ended.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_bracketed_paste_arrives_as_one_paste() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-paste");
    let script = "printf '\\033[?2004h'; stty raw -echo; echo armed; \
                  dd bs=1 count=15 of=got.bin 2>/dev/null; exec sleep 30";
    rig.bash(terminal(script)).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    let pasted = rig.process(json!({"id": 1, "input": "a\nb"})).await;
    assert!(
        matches!(pasted.run.outcome, ToolOutcome::Completed { .. }),
        "{}",
        pasted.text
    );
    until_file(&rig.path("got.bin"), "\x1b[201~").await;
    assert_eq!(
        fs::read(rig.path("got.bin")).unwrap(),
        b"\x1b[200~a\nb\x1b[201~"
    );
    rig.finish().await;
}

#[tokio::test]
async fn arrow_keys_follow_application_cursor_mode() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-arrows");
    let script = "stty raw -echo; echo normal; dd bs=1 count=3 of=normal.bin 2>/dev/null; \
                  printf '\\033[?1h'; echo application; \
                  dd bs=1 count=3 of=application.bin 2>/dev/null; exec sleep 30";
    rig.bash(terminal(script)).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "normal"}))
        .await;
    rig.process(json!({"id": 1, "keys": ["Up"], "until": "application"}))
        .await;
    rig.process(json!({"id": 1, "keys": ["Up"], "wait_ms": 200}))
        .await;
    until_file(&rig.path("application.bin"), "\x1bOA").await;
    assert_eq!(fs::read(rig.path("normal.bin")).unwrap(), b"\x1b[A");
    assert_eq!(fs::read(rig.path("application.bin")).unwrap(), b"\x1bOA");
    rig.finish().await;
}

#[tokio::test]
async fn the_line_limit_applies_only_in_line_mode() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-line-limit");
    rig.bash(terminal(
        "stty -echo; echo armed; head -n 1 > line.txt; exec sleep 30",
    ))
    .await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    let fits = "x".repeat(4095);
    let written = rig
        .process(json!({"id": 1, "input": fits, "keys": ["Enter"], "wait_ms": 200}))
        .await;
    assert!(
        matches!(written.run.outcome, ToolOutcome::Completed { .. }),
        "{}",
        written.text
    );
    until_file(&rig.path("line.txt"), "\n").await;
    assert_eq!(
        fs::read_to_string(rig.path("line.txt")).unwrap(),
        format!("{fits}\n")
    );
    rig.bash(terminal("stty -echo; echo armed; exec cat > canonical.txt"))
        .await;
    rig.process(json!({"id": 2, "wait_ms": 10_000, "until": "armed"}))
        .await;
    let refused = rig
        .process(json!({"id": 2, "input": format!("short\n{}", "y".repeat(4096))}))
        .await;
    assert_eq!(refused.invalid(), Some(line_limit_text(2, 4096).as_str()));
    assert_eq!(
        line_limit_text(5, 9120),
        "Process 5's terminal is in line mode, which keeps only the first 4,095 bytes of a line and drops the rest; this input has a line of 9,120 bytes. Write long text to a file and pass its path, or start the program with run_in_background and no tty, so its stdin is a pipe."
    );
    rig.process(json!({"id": 2, "keys": ["C-d"], "wait_ms": 5_000}))
        .await;
    assert_eq!(
        fs::read_to_string(rig.path("canonical.txt")).unwrap(),
        "",
        "nothing of the refused input was written"
    );
    rig.bash(terminal(
        "stty raw -echo; echo armed; head -c 9216 > raw.txt; exec sleep 30",
    ))
    .await;
    rig.process(json!({"id": 3, "wait_ms": 10_000, "until": "armed"}))
        .await;
    let long = "z".repeat(9216);
    let raw = rig
        .process(json!({"id": 3, "input": long, "wait_ms": 200}))
        .await;
    assert!(
        matches!(raw.run.outcome, ToolOutcome::Completed { .. }),
        "{}",
        raw.text
    );
    until_file(&rig.path("raw.txt"), &"z".repeat(9216)).await;
    rig.finish().await;
}

#[tokio::test]
async fn quiet_ms_reads_the_screen_not_the_bytes() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-quiet");
    let script = format!(
        "printf '\\033[2J'; while {}; do printf '\\033[Hsame'; sleep 0.05; done",
        runner_alive()
    );
    rig.bash(terminal(&script)).await;
    let started = Instant::now();
    let waited = rig
        .process(json!({"id": 1, "wait_ms": 10_000, "quiet_ms": 500}))
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{}",
        waited.text
    );
    assert!(
        waited.text.starts_with("Process 1 is running."),
        "{}",
        waited.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_quiet_foreground_terminal_is_handed_back_and_takes_input() {
    if !has_ctty() {
        return;
    }
    let mut rig = handing_back("tty-hand-back");
    let done = rig
        .bash(json!({
            "command": "printf 'name? '; read -r name; echo \"hi $name\"; exec sleep 30",
            "tty": true,
            "timeout_ms": 60_000
        }))
        .await;
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Quiet
        }
    );
    assert_eq!(done.number, Some(1));
    let lines: Vec<&str> = done.text.lines().collect();
    assert_eq!(
        lines[0],
        "Process 1's screen has not changed for 1 s, so it may be waiting for input; it keeps running in its 100x30 terminal."
    );
    assert!(
        lines[1].starts_with("It has ")
            && lines[1].ends_with(
                " s left of its 60 s time limit; to give it longer, call process with id 1 and timeout_ms (0 for no limit)."
            ),
        "{}",
        lines[1]
    );
    assert_eq!(lines[2..4], ["Screen:", "name?"]);
    assert_eq!(
        lines[4],
        format!("The working directory stays {}.", rig.work.display())
    );
    assert_eq!(
        lines[5],
        "Use the process tool with id 1 to type into it, wait for it or stop it."
    );
    let again = rig.process(json!({"id": 1, "read": "screen"})).await;
    assert_eq!(
        again.text,
        "Process 1 is running. The screen is unchanged since you last looked."
    );
    let answered = rig
        .process(json!({"id": 1, "input": "bob", "keys": ["Enter"], "until": "^hi bob$"}))
        .await;
    assert!(answered.text.contains("hi bob"), "{}", answered.text);
    rig.finish().await;
}

#[tokio::test]
async fn a_handed_back_terminal_keeps_its_time_limit() {
    if !has_ctty() {
        return;
    }
    let mut rig = handing_back("tty-hand-back-limit");
    let done = rig
        .bash(json!({"command": "exec sleep 30", "tty": true, "timeout_ms": 3_000}))
        .await;
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Quiet
        }
    );
    let waited = rig.process(json!({"id": 1, "wait_ms": 10_000})).await;
    assert!(
        waited
            .text
            .starts_with("Process 1 was stopped at its 3 s time limit."),
        "{}",
        waited.text
    );
    let unlimited = rig
        .bash(json!({"command": "exec sleep 30", "tty": true, "timeout_ms": 0}))
        .await;
    assert_eq!(unlimited.number, Some(2));
    assert!(
        unlimited.text.contains("\nIt has no time limit.\n"),
        "{}",
        unlimited.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_foreground_pipe_command_is_never_handed_back() {
    if !have_bash() {
        return;
    }
    let mut rig = handing_back("pipe-no-hand-back");
    let done = rig.bash(bash("sleep 2; echo done")).await;
    assert!(
        matches!(done.run.outcome, ToolOutcome::Completed { .. }),
        "{:?}",
        done.run.outcome
    );
    assert!(done.text.contains("done"), "{}", done.text);
    rig.finish().await;
}

#[tokio::test]
async fn a_start_window_waits_while_a_terminal_draws_without_newlines() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::with_window("tty-start-drawing", QUIET, Duration::from_secs(8));
    let script = "for i in $(seq 1 20); do printf '\\r%s' $i; sleep 0.1; done; exec sleep 30";
    let started = Instant::now();
    let done = rig.bash(terminal(script)).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(2),
        "{elapsed:?} {}",
        done.text
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "{elapsed:?} {}",
        done.text
    );
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started
        }
    );
    let screen = rig.process(json!({"id": 1, "read": "screen"})).await;
    assert!(screen.text.ends_with("Screen:\n20"), "{}", screen.text);
    rig.finish().await;
}

#[tokio::test]
async fn until_after_typing_waits_for_the_screen_to_change_before_it_matches() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-until-typed");
    rig.bash(terminal(SHELL)).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "^ready>"}))
        .await;
    let started = Instant::now();
    let typed = rig
        .process(json!({
            "id": 1,
            "input": "sleep 1; echo slept",
            "keys": ["Enter"],
            "until": "^ready>$",
            "wait_ms": 10_000
        }))
        .await;
    assert!(typed.text.contains("\nslept"), "{}", typed.text);
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{}",
        typed.text
    );
    let plain = rig
        .process(json!({"id": 1, "until": "^ready>$", "wait_ms": 10_000}))
        .await;
    assert!(
        plain.text.starts_with("Process 1 is running."),
        "{}",
        plain.text
    );
    rig.finish().await;
}

fn not_written(text: &str, total: &str) -> usize {
    let tail = format!(" of {total} bytes weren't written");
    let end = text
        .find(&tail)
        .unwrap_or_else(|| panic!("no unwritten count in {text}"));
    let start = text[..end]
        .rfind(|c: char| !(c.is_ascii_digit() || c == ','))
        .map_or(0, |i| i + 1);
    text[start..end].replace(',', "").parse().unwrap()
}

fn cut_short() -> String {
    format!(
        "stty raw -echo; echo armed; {}; timeout --foreground 3 cat > got.bin; echo done; exec sleep 30",
        wait_for("go")
    )
}

async fn lands_after_go(mut rig: Rig, left: usize, text: &str) {
    fs::write(rig.path("go"), "").unwrap();
    rig.process(json!({"id": 1, "wait_ms": 15_000, "until": "done$"}))
        .await;
    let got = fs::read(rig.path("got.bin")).unwrap();
    assert_eq!(got.len(), 40_960 - left, "{text}");
    assert!(got.iter().all(|b| *b == b'a'));
    rig.finish().await;
}

#[tokio::test]
async fn input_cut_short_counts_exactly_the_bytes_the_program_gets() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-exact-count");
    rig.bash(terminal(&cut_short())).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    let input = "a".repeat(40_960);
    let typed = rig
        .process(json!({"id": 1, "input": input, "wait_ms": 1_000}))
        .await;
    let left = not_written(&typed.text, "40,960");
    assert!(left > 0 && left < 40_960, "{}", typed.text);
    lands_after_go(rig, left, &typed.text).await;
}

#[tokio::test]
async fn an_interrupt_behind_input_cut_short_is_reported_unsent() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-interrupt-unsent");
    rig.bash(terminal(&cut_short())).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    let input = "a".repeat(40_960);
    let typed = rig
        .process(json!({"id": 1, "input": input, "signal": "interrupt", "wait_ms": 1_000}))
        .await;
    assert!(
        typed
            .text
            .contains("weren't written and the interrupt wasn't sent"),
        "{}",
        typed.text
    );
    let left = not_written(&typed.text, "40,960");
    assert!(left > 0 && left < 40_960, "{}", typed.text);
    assert_eq!(
        typed.run.typed,
        Some(wisp_protocol::Typed {
            echoed: false,
            unsent: true,
            signalled: false
        })
    );
    lands_after_go(rig, left, &typed.text).await;
}

#[tokio::test]
async fn query_answers_held_behind_blocked_input_are_capped_at_64() {
    if !has_ctty() {
        return;
    }
    let mut rig = Rig::new("tty-reply-cap");
    let typed = 40_960;
    let held = typed + QUEUED_REPLIES * b"\x1b[0n".len();
    let script = format!(
        "stty raw -echo; echo armed; until read -t 0; do sleep 0.01; done; \
         for i in $(seq 150); do printf '\\033[5n'; sleep 0.01; done; echo queried; \
         {}; \
         timeout --foreground 20 head -c {held} > got.bin; echo done; exec sleep 30",
        wait_for("go")
    );
    rig.bash(terminal(&script)).await;
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "armed"}))
        .await;
    let screen = rig
        .processes
        .handle(1)
        .unwrap()
        .state
        .terminal()
        .unwrap()
        .clone();
    let go = rig.path("go");
    let release = async {
        assert!(
            until(|| screen.view().rows.iter().any(|row| row.contains("queried"))).await,
            "the queries never ended"
        );
        fs::write(&go, "").unwrap();
    };
    let input = "a".repeat(typed);
    let (typing, ()) = tokio::join!(
        rig.process(json!({"id": 1, "input": input, "wait_ms": 20_000})),
        release
    );
    assert!(!typing.text.contains("weren't written"), "{}", typing.text);
    rig.process(json!({"id": 1, "wait_ms": 30_000, "until": "done$"}))
        .await;
    let got = fs::read(rig.path("got.bin")).unwrap();
    let answers = got.windows(4).filter(|w| *w == b"\x1b[0n").count();
    assert_eq!(answers, QUEUED_REPLIES);
    assert_eq!(got.iter().filter(|b| **b == b'a').count(), typed);
    rig.finish().await;
}

#[test]
fn an_interrupt_alone_left_unwritten_is_reported_unsent() {
    use wisp_plugin_shell::unwritten_before_interrupt;
    assert_eq!(
        unwritten_before_interrupt(3, 1, 1),
        "The interrupt wasn't sent: process 3 isn't reading its input."
    );
    assert_eq!(
        unwritten_before_interrupt(3, 1, 9),
        "The interrupt wasn't sent: process 3 isn't reading its input."
    );
    assert_eq!(
        unwritten_before_interrupt(3, 5, 9),
        "4 of 8 bytes weren't written and the interrupt wasn't sent: process 3 isn't reading its input."
    );
}
