use std::num::NonZeroU16;
use std::sync::Mutex as StdMutex;

use serde_json::json;
use wisp_protocol::{BackgroundReason, TermSize, ToolOutcome};

use super::*;
use crate::jobs::exec::HostSpawner;
use crate::jobs::exec::pty::{NO_CTTY, TERMINAL_TESTS};
use crate::jobs::screen::{DEFAULT_TTY_SIZE, TermPort, Terminal};
use crate::jobs::state::{JobState, Note, Spawned};

fn started(rig: &Rig, id: u32) -> Note {
    let handle = rig.processes.handle(id).unwrap();
    handle
        .state
        .notes(tokio::time::Instant::now(), true)
        .into_iter()
        .find(|n| matches!(n, Note::Started { .. }))
        .unwrap()
}

#[tokio::test]
async fn a_terminal_process_leads_its_session_and_owns_the_terminal() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-session");
    let script = "read -r _ _ _ _ _ sid tty_nr _ < /proc/$$/stat; \
                  echo \"pid=$$ sid=$sid tty_nr=$tty_nr term=$TERM\"; \
                  tty; exec 3</dev/tty && echo opened; exec sleep 30";
    let done = rig.bash(terminal(script)).await;
    assert_eq!(done.number, Some(1), "{}", done.text);
    assert!(
        done.text
            .starts_with("Process 1 is running in the background in a 100x30 terminal:"),
        "{}",
        done.text
    );
    let Note::Started { size, origin, .. } = started(&rig, 1) else {
        unreachable!()
    };
    assert_eq!(size, Some(DEFAULT_TTY_SIZE));
    let pid = origin.pid.unwrap();
    rig.process(json!({"id": 1, "wait_ms": 10_000, "until": "^opened"}))
        .await;
    let screen = rig.process(json!({"id": 1, "read": "screen"})).await;
    assert!(
        screen
            .text
            .contains(&format!("pid={pid} sid={pid} tty_nr=")),
        "{}",
        screen.text
    );
    assert!(!screen.text.contains("tty_nr=0 "), "{}", screen.text);
    assert!(
        screen.text.contains("term=xterm-256color"),
        "{}",
        screen.text
    );
    assert!(screen.text.contains("/dev/pts/"), "{}", screen.text);
    assert!(screen.text.contains("\nopened"), "{}", screen.text);
    rig.finish().await;
}

#[tokio::test]
async fn a_pipe_process_has_no_terminal() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("pipe-no-tty");
    let done = rig
        .bash(background(
            "(exec 3</dev/tty) 2>/dev/null && echo opened || echo no-terminal; echo term=$TERM; exec sleep 30",
        ))
        .await;
    assert!(done.text.contains("no-terminal"), "{}", done.text);
    assert!(done.text.contains("term=dumb"), "{}", done.text);
    let screen = rig.process(json!({"id": 1, "read": "screen"})).await;
    assert_eq!(
        screen.invalid(),
        Some("read screen only applies to a terminal process, and process 1 reads a pipe")
    );
    let sized = rig.process(json!({"id": 1, "size": "120x40"})).await;
    assert_eq!(
        sized.invalid(),
        Some("`size` only applies to a terminal process, and process 1 reads a pipe")
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_full_screen_program_is_read_as_a_screen_until_it_leaves() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-alternate");
    let script = format!(
        "printf 'before\\n'; printf '\\033[?1049h\\033[2J\\033[3;5Hhello'; \
         {}; printf '\\033[?1049l'; echo back; exec sleep 30",
        wait_for("go.txt")
    );
    let done = rig.bash(terminal(&script)).await;
    assert!(
        done.text.contains("\nScreen:\n\n\n    hello\n"),
        "{}",
        done.text
    );
    let again = rig.process(json!({"id": 1})).await;
    assert_eq!(
        again.text,
        "Process 1 is running. The screen is unchanged since you last looked."
    );
    fs::write(rig.path("go.txt"), "").unwrap();
    let back = rig
        .process(json!({"id": 1, "wait_ms": 10_000, "until": "^back"}))
        .await;
    assert!(back.text.contains("back"), "{}", back.text);
    assert!(!back.text.contains("Screen:"), "{}", back.text);
    let screen = rig.process(json!({"id": 1, "read": "screen"})).await;
    assert!(
        screen.text.starts_with(
            "Process 1 is running in a 100x30 terminal; the cursor is at row 3, column 1.\nScreen:\nbefore\n"
        ),
        "{}",
        screen.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_program_waiting_for_a_cursor_report_gets_one() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-query");
    let script = "stty -icanon -echo; printf 'ab\\033[6n'; IFS= read -r -s -d R reply; \
                  stty sane; printf '\\ngot %s\\n' \"${reply#*[}\"";
    let done = rig
        .bash(json!({"command": script, "tty": true, "timeout_ms": 10_000}))
        .await;
    assert!(
        matches!(done.run.outcome, ToolOutcome::Completed { .. }),
        "{:?} {}",
        done.run.outcome,
        done.text
    );
    assert!(done.text.contains("got 1;3"), "{}", done.text);
    assert_eq!(done.number, Some(1));
    let screen = rig.process(json!({"id": 1, "read": "screen"})).await;
    assert!(
        screen
            .text
            .starts_with("Process 1 exited with code 0. Its final screen, in a 100x30 terminal, had the cursor at row 3, column 1.\nScreen:\nab\ngot 1;3"),
        "{}",
        screen.text
    );
    let sized = rig.process(json!({"id": 1, "size": "120x40"})).await;
    assert_eq!(
        sized.invalid(),
        Some("process 1 has ended, so it takes no input, signals, size or time limit")
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_resize_reaches_the_program_and_the_emulator() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-resize");
    let script = format!("trap 'stty size' WINCH; echo ready; {}", hold());
    rig.bash(terminal(&script)).await;
    started(&rig, 1);
    let resized = rig
        .process(json!({"id": 1, "size": "120x40", "wait_ms": 10_000, "until": "^40 120$"}))
        .await;
    assert!(
        resized
            .text
            .starts_with("Process 1 is running.\nProcess 1's terminal is now 120x40.\n"),
        "{}",
        resized.text
    );
    assert!(resized.text.contains("40 120"), "{}", resized.text);
    let handle = rig.processes.handle(1).unwrap();
    assert_eq!(
        handle.state.terminal().unwrap().size(),
        TermSize {
            cols: 120,
            rows: 40
        }
    );
    let notes = handle.state.notes(tokio::time::Instant::now(), true);
    assert!(
        notes.contains(&Note::Resized {
            id: 1,
            size: TermSize {
                cols: 120,
                rows: 40
            }
        }),
        "{notes:?}"
    );
    let listed = rig.process(json!({})).await;
    assert!(
        listed.text.contains("1  running in a terminal  "),
        "{}",
        listed.text
    );
    let bad = rig.process(json!({"id": 1, "size": "10x10"})).await;
    assert_eq!(
        bad.invalid(),
        Some("`size` must be from 20x5 to 500x200, not 10x10")
    );
    rig.finish().await;
}

#[tokio::test]
async fn tty_is_refused_without_setsid_ctty() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::with_host(
        "tty-refused",
        KernelHost::new().with_spawner(Arc::new(HostSpawner::with_setsid(None))),
        window(QUIET, WINDOW),
    );
    for input in [json!({"command": "true", "tty": true}), terminal("true")] {
        let done = rig.bash(input).await;
        assert_eq!(
            done.run.outcome,
            ToolOutcome::Failed {
                message: NO_CTTY.to_owned(),
                before_start: true,
            }
        );
        assert_eq!(done.number, None);
    }
    for n in 1..=2 {
        assert!(!rig.file(n).exists(), "{}", rig.file(n).display());
    }
    rig.finish().await;
}

#[tokio::test]
async fn a_spawn_that_fails_to_start_leaves_no_output_file() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::with_host(
        "spawn-io",
        KernelHost::new().with_spawner(Arc::new(HostSpawner::with_setsid(Some(
            "/nonexistent/setsid".into(),
        )))),
        window(QUIET, WINDOW),
    );
    let done = rig.bash(json!({"command": "echo hi"})).await;
    assert!(
        matches!(
            done.run.outcome,
            ToolOutcome::Failed {
                before_start: true,
                ..
            }
        ),
        "{:?}",
        done.run.outcome
    );
    assert_eq!(done.number, None);
    assert!(!rig.file(1).exists(), "{}", rig.file(1).display());
    rig.finish().await;
}

#[tokio::test]
async fn a_foreground_terminal_stopped_at_its_limit_keeps_a_number() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-limit");
    let done = rig
        .bash(json!({"command": "echo started; exec sleep 30", "tty": true, "timeout_ms": 3000}))
        .await;
    assert_eq!(done.run.outcome, ToolOutcome::TimedOut { after_ms: 3000 });
    assert_eq!(done.number, Some(1));
    let screen = rig.process(json!({"id": 1, "read": "screen"})).await;
    assert!(screen.text.contains("\nstarted"), "{}", screen.text);
    rig.finish().await;
}

struct Silent(Arc<StdMutex<Vec<TermSize>>>);

impl TermPort for Silent {
    fn resize(&self, size: TermSize) -> std::io::Result<()> {
        self.0.lock().unwrap().push(size);
        Ok(())
    }

    fn write(&self, _job: crate::jobs::screen::WriteJob) {}

    fn canonical(&self) -> Option<bool> {
        None
    }
}

fn terminal_state() -> (Arc<JobState>, Arc<Terminal>) {
    let terminal = Arc::new(Terminal::new(
        TermSize { cols: 30, rows: 6 },
        Box::new(Silent(Arc::default())),
    ));
    let (state, _) = JobState::new(Spawned {
        call_id: "c".into(),
        target: "t".into(),
        file: None,
        origin: Default::default(),
        stdin: None,
        terminal: Some(terminal.clone()),
        limit_ms: None,
        deadline: None,
        log: None,
    });
    (state, terminal)
}

fn output_of(notes: &[Note]) -> (String, u64) {
    notes
        .iter()
        .find_map(|n| match n {
            Note::Output { text, skipped, .. } => Some((text.clone(), *skipped)),
            _ => None,
        })
        .unwrap()
}

fn replayed(text: &str) -> Vec<String> {
    let mut client =
        vt100::Parser::new(NonZeroU16::new(6).unwrap(), NonZeroU16::new(30).unwrap(), 0);
    client.process(b"\x1b[?1049hstale\x1b[3;3Hjunk");
    client.process(text.as_bytes());
    let mut rows: Vec<String> = client
        .screen()
        .rows(0, 30)
        .map(|r| r.trim_end().to_owned())
        .collect();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

#[tokio::test(start_paused = true)]
async fn a_move_starts_the_events_with_a_repaint() {
    let (state, terminal) = terminal_state();
    let bytes = b"\x1b[?1049h\x1b[2;2Hmoved here";
    terminal.feed(bytes);
    state.output(bytes, true);
    assert!(state.number(4, Some(BackgroundReason::MovedByUser), false));
    let late = b"\x1b[3;1Hafter the move";
    terminal.feed(late);
    state.output(late, false);
    state.events_stopped(true);
    let notes = state.notes(tokio::time::Instant::now(), false);
    assert!(
        matches!(
            &notes[0],
            Note::Started {
                size: Some(TermSize { cols: 30, rows: 6 }),
                ..
            }
        ),
        "{notes:?}"
    );
    let (text, skipped) = output_of(&notes);
    assert_eq!(skipped, 0);
    assert_eq!(text.as_bytes(), terminal.repaint());
    assert_eq!(replayed(&text), terminal.view().rows);
}

#[tokio::test(start_paused = true)]
async fn an_overflow_is_replaced_by_a_repaint() {
    let (state, terminal) = terminal_state();
    assert!(state.number(2, Some(BackgroundReason::Started), true));
    state.notes(tokio::time::Instant::now(), true);
    let line = "0123456789".repeat(10) + "\r\n";
    let mut total = 0;
    while total <= crate::jobs::state::PENDING_BYTES {
        terminal.feed(line.as_bytes());
        state.output(line.as_bytes(), true);
        total += line.len();
    }
    terminal.feed(b"\x1b[Hlast");
    state.output(b"\x1b[Hlast", true);
    let notes = state.notes(tokio::time::Instant::now(), true);
    let (text, skipped) = output_of(&notes);
    assert!(skipped > 0);
    assert!(text.starts_with("\x1bc\x1b[?1049l"), "{text:?}");
    assert_eq!(replayed(&text), terminal.view().rows);
    assert!(replayed(&text)[0].starts_with("last"));
}

#[tokio::test]
async fn esc_stops_a_foreground_terminal_and_everything_in_it() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-esc");
    let ctl = Ctl::default();
    let cancel = ctl.cancel.clone();
    let path = rig.path("pid.txt");
    let fire = async {
        let pid = pid_in(&path).await;
        cancel.cancel();
        pid
    };
    let (done, pid) = tokio::join!(
        rig.bash_with(
            json!({"command": "sleep 30 & echo $! > pid.txt; wait", "tty": true}),
            ctl
        ),
        fire
    );
    assert_eq!(done.run.outcome, ToolOutcome::Cancelled);
    assert!(gone(pid).await);
    rig.finish().await;
}

#[tokio::test]
async fn a_moved_foreground_terminal_starts_its_events_with_a_repaint() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-move");
    let ctl = Ctl::default();
    let processes = rig.processes.clone();
    let call_id = rig.next_call();
    let path = rig.path("pid.txt");
    let fire = async {
        pid_in(&path).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(processes.fire_move(&call_id));
    };
    let script = "printf '\\033[?1049h\\033[2;3Hdrawn'; echo $$ > pid.txt; exec sleep 30";
    let (done, ()) = tokio::join!(
        rig.bash_with(
            json!({"command": script, "tty": true, "timeout_ms": 1000}),
            ctl
        ),
        fire
    );
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::MovedByUser
        }
    );
    assert_eq!(done.number, Some(1));
    let handle = rig.processes.handle(1).unwrap();
    let terminal = handle.state.terminal().unwrap().clone();
    let notes = handle.state.notes(tokio::time::Instant::now(), true);
    assert!(
        matches!(
            &notes[0],
            Note::Started {
                size: Some(DEFAULT_TTY_SIZE),
                reason: BackgroundReason::MovedByUser,
                ..
            }
        ),
        "{notes:?}"
    );
    let (text, _) = output_of(&notes);
    assert!(text.starts_with("\x1bc\x1b[?1049h"), "{text:?}");
    let mut client = vt100::Parser::new(
        NonZeroU16::new(30).unwrap(),
        NonZeroU16::new(100).unwrap(),
        0,
    );
    client.process(text.as_bytes());
    assert_eq!(client.screen().contents(), terminal.view().rows.join("\n"));
    let screen = rig.process(json!({"id": 1})).await;
    assert!(
        screen.text.starts_with(
            "Process 1 is running in a 100x30 terminal showing a full-screen program; the cursor is at row 2, column 8.\nScreen:\n\n  drawn"
        ),
        "{}",
        screen.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn crlf_lines_keep_their_text_in_a_terminal_and_a_pipe() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-crlf");
    let command = "printf 'HTTP/1.1 200 OK\\r\\nServer: x\\r\\n\\r\\nbody\\n'";
    for tty in [true, false] {
        let done = rig.bash(json!({"command": command, "tty": tty})).await;
        assert!(
            done.text.contains("HTTP/1.1 200 OK\nServer: x\n\nbody"),
            "{tty} {}",
            done.text
        );
    }
    let first = rig.process(json!({"id": 1, "read": "screen"})).await;
    assert!(
        first
            .text
            .ends_with("Screen:\nHTTP/1.1 200 OK\nServer: x\n\nbody"),
        "{}",
        first.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_finished_terminal_result_names_its_final_screen() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("tty-final-screen");
    let done = rig.bash(json!({"command": "echo hi", "tty": true})).await;
    assert!(
        done.text.ends_with(
            "Use the process tool with id 1 to read or search this output or show its final screen."
        ),
        "{}",
        done.text
    );
    rig.finish().await;
}

fn terminal_fds() -> usize {
    fs::read_dir("/proc/self/fd")
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter(|e| fs::read_link(e.path()).is_ok_and(|t| t == Path::new("/dev/ptmx")))
                .count()
        })
        .unwrap_or(0)
}

fn terminal_threads() -> usize {
    fs::read_dir("/proc/self/task")
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter(|e| {
                    fs::read_to_string(e.path().join("comm"))
                        .is_ok_and(|name| name.starts_with("wisp-terminal"))
                })
                .count()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn an_ended_terminal_gives_back_its_pty_and_threads() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let fds = terminal_fds();
    let threads = terminal_threads();
    let mut rig = Rig::new("tty-release");
    for n in 1..=6 {
        let done = rig.bash(json!({"command": "echo hi", "tty": true})).await;
        assert_eq!(done.number, Some(n));
        let handle = rig.processes.handle(n).unwrap();
        assert!(!handle.state.terminal().unwrap().is_open());
    }
    assert!(
        until(|| terminal_fds() <= fds && terminal_threads() <= threads).await,
        "{} pty fds (from {fds}) and {} threads (from {threads})",
        terminal_fds(),
        terminal_threads()
    );
    let screen = rig.process(json!({"id": 6, "read": "screen"})).await;
    assert!(screen.text.ends_with("Screen:\nhi"), "{}", screen.text);
    rig.finish().await;
}

#[tokio::test]
async fn a_terminal_child_does_not_inherit_columns_or_lines() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let scratch = scratch("tty-columns");
    let work = scratch.root.join("work");
    let bash = crate::jobs::exec::find_program("bash").unwrap();
    let args = [
        "--noprofile",
        "--norc",
        "-c",
        "echo \"c=${COLUMNS-unset} l=${LINES-unset}\" > env.txt; stty size >> env.txt",
    ]
    .map(std::ffi::OsString::from);
    let launch = crate::jobs::exec::Launch {
        program: &bash,
        args: &args,
        cwd: &work,
        env: &[("COLUMNS", "7"), ("LINES", "3")],
        stdin: false,
        log: None,
    };
    let process = crate::jobs::exec::Spawner::spawn_tty(
        &HostSpawner::detect(),
        &launch,
        TermSize {
            cols: 100,
            rows: 30,
        },
    )
    .unwrap();
    let mut sink = OutputSink::detached("columns");
    crate::jobs::exec::supervise(
        process,
        &mut sink,
        &CancellationToken::new(),
        tokio::time::Instant::now() + PATIENCE,
    )
    .await;
    assert_eq!(
        fs::read_to_string(work.join("env.txt")).unwrap(),
        "c=unset l=unset\n30 100\n"
    );
}

#[tokio::test(start_paused = true)]
async fn the_events_repaint_only_once_per_job() {
    let (state, terminal) = terminal_state();
    terminal.feed(b"before");
    assert!(state.number(3, Some(BackgroundReason::MovedByUser), false));
    state.events_stopped(true);
    let later = b"\r\nlater";
    terminal.feed(later);
    state.output(later, true);
    state.events_stopped(false);
    let notes = state.notes(tokio::time::Instant::now(), true);
    let (text, _) = output_of(&notes);
    assert_eq!(text.matches("\x1bc").count(), 1, "{text:?}");
    assert!(text.ends_with("\r\nlater"), "{text:?}");
}

#[tokio::test(start_paused = true)]
async fn output_before_a_background_start_is_numbered_is_repainted() {
    let (state, terminal) = terminal_state();
    let early = b"\x1b[?1049h\x1b[2;2Hearly";
    terminal.feed(early);
    state.output(early, true);
    let mid = b"\x1b[3;1Hmid-write";
    terminal.feed(mid);
    assert!(state.number(1, Some(BackgroundReason::Started), true));
    state.output(mid, true);
    state.repaint_unnumbered_output();
    let notes = state.notes(tokio::time::Instant::now(), true);
    let (text, _) = output_of(&notes);
    assert!(text.starts_with("\x1bc\x1b[?1049h"), "{text:?}");
    assert_eq!(text.matches("mid-write").count(), 1, "{text:?}");
    assert_eq!(replayed(&text), terminal.view().rows);
    state.repaint_unnumbered_output();
    let again = state.notes(tokio::time::Instant::now(), true);
    assert!(
        again.iter().all(|n| !matches!(n, Note::Output { .. })),
        "{again:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_job_numbered_without_a_reason_never_repaints() {
    let (state, terminal) = terminal_state();
    terminal.feed(b"foreground");
    assert!(state.number(2, None, false));
    state.events_stopped(true);
    let notes = state.notes(tokio::time::Instant::now(), true);
    assert!(
        notes.iter().all(|n| !matches!(n, Note::Output { .. })),
        "{notes:?}"
    );
}

#[tokio::test]
async fn long_commands_run_in_tty_and_clean_up() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("long-tty");
    for size in crate::plugin::shell_tests::LONG_SIZES {
        let cmd = crate::plugin::shell_tests::long_command(
            size,
            "test -t 0 && echo isatty; IFS= read -r line; printf 'tty:%s:%s\n' \"$line\" \"$0\"; exit 4",
        );
        let done = rig.bash(terminal(&cmd)).await;
        let id = done.number.expect("tty process number");
        let sh = rig.file(rig.shell.calls()).with_extension("sh");
        assert!(!sh.exists(), "{}", sh.display());
        assert!(!done.text.contains(".sh"), "{}", done.text);
        rig.process(json!({"id": id, "wait_ms": 10_000, "until": "isatty"}))
            .await;
        let typed = rig
            .process(json!({"id": id, "input": "from-tty", "keys": ["Enter"], "wait_ms": 10_000, "quiet_ms": 10_000}))
            .await;
        assert!(typed.text.contains("tty:from-tty:"), "{}", typed.text);
        assert!(typed.text.contains("bash"), "{}", typed.text);
        assert!(typed.text.contains("exited with code 4"), "{}", typed.text);
        assert!(!typed.text.contains(".sh"), "{}", typed.text);
    }
    rig.finish().await;
}
