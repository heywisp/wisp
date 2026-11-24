use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use wisp_protocol::jsonl;
use wisp_protocol::link::{self, Ready, Start};
use wisp_start::start::{self, StartAt};
use wisp_tui::present::choose::{Chosen, Presenter};

use crate::{Args, Setup, Stop};

pub(super) fn no_restart_answer() -> String {
    format!(
        "The chat didn't answer within {} s.",
        crate::keep::STARTS_WITHIN.as_secs()
    )
}

pub(super) fn build() -> u64 {
    static BUILD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BUILD.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|time| u64::try_from(time.as_nanos()).ok())
            .unwrap_or_default()
    })
}

pub(super) fn ready(value: Ready) -> io::Result<()> {
    let line = jsonl::line(&value).map_err(io::Error::other)?;
    let mut out = io::stdout().lock();
    out.write_all(line.as_bytes())?;
    out.flush()
}

pub(super) async fn run(args: Args, setup: Setup) -> ExitCode {
    match serve(args, setup).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(text) => {
            let _ = ready(Ready::Failed { text });
            ExitCode::FAILURE
        }
    }
}

fn in_window(args: &Args, mut built: start::Built, reason: String) -> Result<(), String> {
    if args.session_id.is_some() && args.resume.is_none() {
        built.discard_unstarted_log();
    }
    drop(built);
    ready(Ready::Window { reason }).map_err(|e| e.to_string())
}

async fn serve(args: Args, setup: Setup) -> Result<(), String> {
    let program = std::env::current_exe().map_err(|e| e.to_string())?;
    let folder = std::env::current_dir().map_err(|e| e.to_string())?;
    let _hangups = signal(SignalKind::hangup()).map_err(|e| e.to_string())?;
    let _interrupts = signal(SignalKind::interrupt()).map_err(|e| e.to_string())?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;
    let mut input: Start = link::read(&line).ok_or("the chat start could not be read")?;
    drop(line);
    let data = crate::keep::data().ok_or("no data directory (set HOME or XDG_DATA_HOME)")?;
    let mut socket = match wispd::bind(&data).await {
        Ok(socket) => socket,
        Err(error) => {
            return ready(Ready::Window {
                reason: crate::keep::unavailable(&link::run_dir(&data), &error),
            })
            .map_err(|e| e.to_string());
        }
    };
    if args.resume_unlocked {
        return ready(Ready::Window {
            reason: crate::keep::UNLOCKED.to_owned(),
        })
        .map_err(|e| e.to_string());
    }
    let keychain = Arc::new(wisp_start::run::load_keychain_on(
        args.config.as_deref(),
        &setup,
        input.terminal,
        Box::new(|| {
            let _ = ready(Ready::Notice {
                text: wisp_start::keychain::WAITING.to_owned(),
            });
        }),
    )?);
    let chosen = Chosen {
        presenter: Presenter::Tui,
        reason: "hosted chat".to_owned(),
    };
    let at = input.account.as_ref().map(|account| StartAt {
        account: account.clone(),
        follow: input.follow.clone(),
    });
    let (mut built, _) =
        match crate::build_session(&args, &setup, &keychain, &chosen, at.as_ref(), Some(&input))
            .await
        {
            Ok(built) => built,
            Err(Stop::Ask(_)) => return ready(Ready::Ask).map_err(|e| e.to_string()),
            Err(Stop::Failed(text)) => return Err(text),
        };
    let reason = built
        .unlocked
        .as_ref()
        .map(|reason| format!("the session log can't be locked ({reason})."))
        .or_else(|| {
            built
                .opening
                .log
                .is_none()
                .then(|| crate::keep::LOG_OFF.to_owned())
        });
    if let Some(reason) = reason {
        return in_window(&args, built, reason);
    }
    let id = built
        .opening
        .log
        .as_deref()
        .and_then(|log| std::path::Path::new(log).file_stem())
        .and_then(|id| id.to_str())
        .filter(|id| wisp_protocol::valid_id(id))
        .ok_or("the chat has no session id")?
        .to_owned();
    if let Err(error) = socket.publish(&id) {
        let reason = crate::keep::unavailable(&link::run_dir(&data), &error);
        return in_window(&args, built, reason);
    }
    let path = socket.path().to_owned();
    if let Some(Some(error)) = &args.host {
        std::fs::rename(error, link::error_file(&path)).map_err(|e| e.to_string())?;
        if let Ok(file) = std::fs::OpenOptions::new()
            .append(true)
            .open(link::error_file(&path))
        {
            let _ = rustix::stdio::dup2_stderr(&file);
        }
    }
    let redactions = built
        .book
        .log_context()
        .redactions
        .union([&built.sources.redactions(), &wisp_core::session::held()]);
    built.agent = built.agent.with_opening_replay();
    let started = start::start(built);
    let host = wispd::Host::new(wispd::Session {
        opening: started.opening,
        view: started.view.unwrap_or_default(),
        session: id.clone(),
        build: build(),
    });
    let _ = ready(Ready::Socket { path: path.clone() });
    let null = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .map_err(|e| e.to_string())?;
    rustix::stdio::dup2_stdout(&null).map_err(|e| e.to_string())?;
    let handle = host.handle();
    let stop = tokio::spawn(async move {
        let mut forced = false;
        while terminate.recv().await.is_some() {
            if !handle.end(forced).await {
                break;
            }
            forced = true;
        }
    });
    let wisp_client::Channels {
        commands, events, ..
    } = started.channels;
    let shutdown = host
        .run(socket, commands, events)
        .await
        .map_err(|e| e.to_string())?;
    let restart = shutdown.restart;
    let forced = shutdown.forced.clone();
    shutdown.finish(end(started.task, forced)).await;
    stop.abort();
    let _ = stop.await;
    if restart {
        input.asks = false;
        input.terminal = false;
        let next = crate::relaunch(&args, id.clone());
        start_again(
            &program,
            &data,
            &next.host_args,
            &folder,
            &id,
            &input,
            &redactions,
        )
        .await;
    }
    Ok(())
}

pub(super) fn start_again<'a>(
    program: &'a Path,
    data: &'a Path,
    extra: &'a [std::ffi::OsString],
    folder: &'a Path,
    id: &'a str,
    input: &'a Start,
    redactions: &'a wisp_core::session::Redactions,
) -> Pin<Box<impl Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let launched = tokio::time::timeout(
            crate::keep::STARTS_WITHIN,
            crate::keep::start_host(program, data, extra, folder, Some(id), true, input, |_| {}),
        )
        .await;
        let reason = match launched {
            Ok((Some(Ready::Socket { .. }), _, _)) => return,
            Ok((Some(Ready::Failed { text }), _, _)) => text,
            Ok((Some(Ready::Window { reason }), _, _)) => reason,
            Ok((Some(Ready::Ask), _, _)) => {
                "The chat needs an answer before it can start.".to_owned()
            }
            Ok(_) => "The chat stopped while starting.".to_owned(),
            Err(_) => no_restart_answer(),
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(link::error_file(&link::socket(data, id)))
        {
            let _ = writeln!(file, "{}", redactions.scrub(&reason));
        }
    })
}

async fn end(mut task: wisp_core::agent::AgentTask, mut forced: watch::Receiver<bool>) -> bool {
    loop {
        if *forced.borrow() {
            break;
        }
        let limit = task.limit();
        tokio::select! {
            done = &mut task.handle => return done.is_ok(),
            changed = task.worst.changed() => {
                if changed.is_err() {
                    match tokio::time::timeout(limit, &mut task.handle).await {
                        Ok(done) => return done.is_ok(),
                        Err(_) => break,
                    }
                }
            },
            _ = forced.changed() => {},
            () = tokio::time::sleep(limit) => break,
        }
    }
    task.handle.abort();
    let _ = task.handle.await;
    false
}
