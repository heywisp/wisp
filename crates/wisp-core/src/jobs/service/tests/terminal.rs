use tokio::sync::mpsc;
use wisp_plugin_api::types::{AskReply, Key, NamedKey};
use wisp_protocol::TermSize;

use super::*;
use crate::call::user::{self, AskRequest, UserChannel};
use crate::jobs::exec::pty::TERMINAL_TESTS;
use crate::test_guard::has_ctty;

fn terminal(rig: &Rig, program: &str, args: &[&str]) -> SpawnSpec {
    SpawnSpec {
        mode: Mode::Terminal,
        size: Some(Size { cols: 80, rows: 24 }),
        ..rig.spec(program, args)
    }
}

fn terminal_sh(rig: &Rig, script: &str) -> SpawnSpec {
    terminal(rig, "sh", &["-c", script])
}

async fn until_file(path: &std::path::Path, want: &[u8]) -> Vec<u8> {
    crate::test_guard::until(|| fs::read(path).unwrap_or_default().len() >= want.len()).await;
    fs::read(path).unwrap_or_default()
}

#[tokio::test]
async fn split_held_keys_never_reach_terminal_rows_or_process_output() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let fake = format!("CANARY{}", "k".repeat(34));
    crate::session::register_secrets([fake.clone()]);
    let (first, rest) = fake.split_at(20);
    let script =
        format!("printf '%s' '{first}'; printf ready > stage; sleep 0.3; printf '%s\\n' '{rest}'");
    let mut spec = terminal_sh(&rig, &script);
    spec.size = Some(Size { cols: 20, rows: 10 });
    let job = rig.spawn(&Scope::new("writer"), spec).await;
    until_file(&rig.dir.path().join("stage"), b"ready").await;
    let id = job.info().await.id.unwrap();
    let reader = rig.jobs.adopt(id, &Scope::new("reader")).unwrap();
    let screen = reader.screen().await.unwrap();
    assert!(screen.rows.iter().all(|row| !row.contains(first)));
    let mut notes = rig.jobs.processes().notes(true);
    assert!(matches!(waited(&job, for_end()).await, Waited::Ended(_)));
    let screen = reader.screen().await.unwrap();
    assert!(screen.rows.iter().any(|row| row.contains("[redacted]")));
    assert!(rig.jobs.processes().take_masked("reader"));
    let rows = reader.lines(1, 10).await;
    assert!(rig.jobs.processes().take_masked("reader"));
    let output = reader.output().await;
    assert!(rig.jobs.processes().take_masked("reader"));
    let file = fs::read_to_string(output.file.path.unwrap()).unwrap();
    notes.extend(rig.jobs.processes().notes(true));
    let emitted: String = notes
        .iter()
        .filter_map(|note| match note {
            crate::jobs::state::Note::Output { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(emitted.contains("[redacted]"), "{notes:?}");
    for place in [screen.rows.join("\n"), format!("{rows:?}"), file, emitted] {
        assert!(!place.contains(first) && !place.contains(rest), "{place}");
    }
}

#[tokio::test]
async fn a_terminal_job_shows_its_current_screen_and_takes_keys() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, terminal(&rig, "cat", &[])).await;
    let info = job.info().await;
    assert_eq!(info.mode, Mode::Terminal);
    assert!(!info.full_screen && !info.echo_off);
    let sent = job.write(text("first"), Duration::from_secs(2)).await;
    assert_eq!((sent.left, sent.failed.clone()), (0, None));
    assert!(sent.typed.echoed);
    let enter = job
        .write(keys(&[Key::Named(NamedKey::Enter)]), Duration::from_secs(2))
        .await;
    assert_eq!(enter.failed, None);
    assert_eq!(waited(&job, until("first", 10_000)).await, Waited::Matched);
    let screen = job.screen().await.unwrap();
    assert!(screen.rows.iter().any(|row| row == "first"), "{screen:?}");
    assert_eq!(screen.size, Size { cols: 80, rows: 24 });
    assert!(
        screen.cursor.row < 24 && !screen.full_screen && screen.changed,
        "{screen:?}"
    );
    assert!(!job.screen().await.unwrap().changed);
    job.write(text("second\r"), Duration::from_secs(2)).await;
    assert_eq!(waited(&job, until("second", 10_000)).await, Waited::Matched);
    let screen = job.screen().await.unwrap();
    assert!(screen.changed && screen.rows.iter().any(|row| row == "second"));
    job.resize(Size {
        cols: 100,
        rows: 30,
    })
    .unwrap();
    let id = job.info().await.id.unwrap();
    let handle = rig.jobs.processes().handle(id).unwrap();
    assert_eq!(
        handle.state.terminal().unwrap().size(),
        TermSize {
            cols: 100,
            rows: 30
        }
    );
    assert!(
        job.signal(SignalKind::Interrupt, SOON)
            .await
            .typed
            .signalled
    );
    assert_eq!(
        waited(&job, for_end()).await,
        Waited::Ended(JobEnd::Exited(ExitStatus::Signal(2)))
    );
}

#[tokio::test]
async fn a_full_screen_program_and_hidden_input_show_in_the_jobs_info() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let script =
        "stty -echo; echo hidden; read line; printf '\\033[?1049h'; echo inside; exec sleep 30";
    let job = rig.spawn(&scope, terminal_sh(&rig, script)).await;
    assert_eq!(waited(&job, until("hidden", 10_000)).await, Waited::Matched);
    let info = job.info().await;
    assert!(info.echo_off && !info.full_screen, "{info:?}");
    job.write(text("go\r"), SOON).await;
    assert_eq!(waited(&job, until("inside", 10_000)).await, Waited::Matched);
    let info = job.info().await;
    assert!(info.full_screen, "{info:?}");
    assert!(job.screen().await.unwrap().full_screen);
    kill(&job).await;
}

#[tokio::test]
async fn a_terminal_write_pastes_with_the_programs_modes() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let script = "printf '\\033[?2004h'; stty raw -echo; echo armed; \
                  dd bs=1 count=15 of=got.bin 2>/dev/null; exec sleep 30";
    let job = rig.spawn(&scope, terminal_sh(&rig, script)).await;
    assert_eq!(waited(&job, until("armed", 10_000)).await, Waited::Matched);
    let sent = job.write(text("a\nb"), Duration::from_secs(2)).await;
    assert_eq!((sent.total, sent.left, sent.failed.clone()), (15, 0, None));
    let got = until_file(&rig.dir.path().join("got.bin"), b"\x1b[200~a\nb\x1b[201~").await;
    assert_eq!(got, b"\x1b[200~a\nb\x1b[201~");
    kill(&job).await;
}

#[tokio::test]
async fn a_line_too_long_for_line_mode_is_refused_before_anything_is_written() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, terminal(&rig, "cat", &[])).await;
    let long = "x".repeat(5000);
    let sent = job.write(text(&long), SOON).await;
    assert_eq!(sent.failed, Some(JobError::LineTooLong(5000)));
    assert_eq!((sent.total, sent.typed.echoed), (0, false));
    let short = job.write(text(&"y".repeat(4095)), SOON).await;
    assert_eq!(short.failed, None);
    kill(&job).await;
}

#[tokio::test]
async fn until_after_a_write_waits_for_the_screen_to_change() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let shell = "exec env PS1='ready> ' bash --norc --noprofile -i";
    let job = rig.spawn(&scope, terminal_sh(&rig, shell)).await;
    assert_eq!(
        waited(&job, until("^ready>", 10_000)).await,
        Waited::Matched
    );
    let typing = Typing {
        keys: vec![Key::Named(NamedKey::Enter)],
        ..text("sleep 1; echo slept")
    };
    let started = Instant::now();
    job.write(typing, Duration::from_secs(2)).await;
    assert_eq!(
        waited(&job, until("^ready>$", 10_000)).await,
        Waited::Matched
    );
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    let screen = job.screen().await.unwrap();
    assert!(screen.rows.iter().any(|row| row == "slept"), "{screen:?}");
    kill(&job).await;
}

#[tokio::test]
async fn a_terminal_the_user_is_typing_into_refuses_the_model() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, terminal(&rig, "cat", &[])).await;
    let handle = rig
        .jobs
        .processes()
        .handle(job.info().await.id.unwrap())
        .unwrap();
    handle.state.set_attached(true);
    assert_eq!(
        job.write(text("x"), SOON).await.failed,
        Some(JobError::Driven)
    );
    assert_eq!(
        job.write(keys(&[Key::Named(NamedKey::Enter)]), SOON)
            .await
            .failed,
        Some(JobError::Driven)
    );
    assert_eq!(
        job.resize(Size { cols: 90, rows: 30 }),
        Err(JobError::Driven)
    );
    assert_eq!(kill(&job).await.failed, Some(JobError::Driven));
    handle.state.set_attached(false);
    assert_eq!(
        kill(&job).await.after_kill,
        Some(Waited::Ended(JobEnd::Stopped(StoppedBy::Model)))
    );
}

#[tokio::test]
async fn asking_the_user_to_attach_goes_through_the_calls_channel_and_esc_stops_it() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let (asks, mut asked) = mpsc::channel(1);
    let scope = Scope {
        user: UserChannel::new(asks),
        ..Scope::new("c1")
    };
    let job = rig.spawn(&scope, terminal(&rig, "cat", &[])).await;
    let id = job.info().await.id.unwrap();
    let answer = tokio::spawn(async move {
        let first = match asked.recv().await {
            Some(AskRequest::Attach {
                id: asked_id,
                reason,
                reply,
            }) => {
                let _ = reply.send(user::AskReply::Attached { typed: true });
                Some((asked_id, reason))
            }
            _ => None,
        };
        let held = asked.recv().await;
        (first, held)
    });
    assert_eq!(
        job.ask_attach("log in".into()).await,
        AskReply::Attached(true)
    );
    let cancel = scope.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let stopped = tokio::time::timeout(PATIENCE, job.ask_attach("again".into()))
        .await
        .expect("Esc ended the question");
    assert_eq!(stopped, AskReply::Stopped);
    let (first, held) = answer.await.unwrap();
    assert_eq!(first, Some((id, "log in".to_owned())));
    assert!(held.is_some());
    let pipe = rig.spawn(&Scope::new("c2"), rig.spec("cat", &[])).await;
    assert_eq!(
        pipe.ask_attach("log in".into()).await,
        AskReply::Unavailable
    );
    let free = Scope::new("c3");
    kill(&rig.jobs.adopt(id, &free).unwrap()).await;
    kill(&pipe).await;
}

#[tokio::test]
async fn quiet_info_reads_the_hidden_flag_without_checking_the_terminal() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig
        .spawn(
            &scope,
            terminal_sh(&rig, "stty -echo; echo hidden; exec sleep 30"),
        )
        .await;
    assert_eq!(waited(&job, until("hidden", 10_000)).await, Waited::Matched);
    assert!(!job.info_quiet().await.echo_off);
    assert!(job.info().await.echo_off);
    assert!(job.info_quiet().await.echo_off);
    kill(&job).await;
}
