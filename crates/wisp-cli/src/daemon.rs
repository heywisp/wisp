use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::Subcommand;
use tokio::net::UnixStream;
use tokio::process::Child;
use tokio::signal::unix::{SignalKind, signal};
use wisp_client::link as client;
use wisp_protocol::link::{self, Ready, Start};

use crate::keep::{self, ErrorFile};

const START_WITH: &str = "Start it with: wisp daemon start";

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
pub(super) enum Command {
    #[command(about = "Start Wisp's daemon")]
    Start,
    #[command(about = "Stop Wisp's daemon; chats stay open")]
    Stop,
    #[command(hide = true)]
    Serve,
}

#[derive(PartialEq, Eq)]
enum State {
    Running,
    Stopped,
    NotStarted,
}

fn error_file(data: &Path) -> PathBuf {
    link::daemon_dir(data).join("daemon.err")
}

fn stopped(data: &Path) -> PathBuf {
    link::daemon_dir(data).join("stopped")
}

async fn state(data: &Path) -> State {
    if UnixStream::connect(link::daemon_socket(data)).await.is_ok() {
        State::Running
    } else if stopped(data).exists() {
        State::Stopped
    } else {
        State::NotStarted
    }
}

pub(super) async fn run(command: Option<&Command>) -> ExitCode {
    let result = async {
        let data = keep::data().ok_or_else(|| io::Error::other(keep::DATA_UNKNOWN))?;
        match command {
            Some(Command::Serve) => serve(&data).await,
            Some(Command::Start) => start(&data).await.map(|started| {
                println!(
                    "Daemon: {}",
                    if started {
                        "started"
                    } else {
                        "already running"
                    }
                )
            }),
            Some(Command::Stop) => {
                let running = stop(&data).await?;
                wisp_core::session::create_private_dir(&link::daemon_dir(&data))?;
                fs::write(stopped(&data), [])?;
                if running {
                    println!(
                        "Daemon: stopped\nChats stay open.\nStart it again with: wisp daemon start"
                    );
                } else {
                    println!("Daemon: already stopped\n{START_WITH}");
                }
                Ok(())
            }
            None => status(&data).await,
        }
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if matches!(command, Some(Command::Serve)) {
                let _ = crate::host::ready(Ready::Failed {
                    text: error.to_string(),
                });
            }
            crate::manage::fail(error)
        }
    }
}

fn spawn(program: &Path, data: &Path) -> io::Result<Child> {
    wisp_core::session::create_private_dir(&link::daemon_dir(data))?;
    keep::spawn(
        program,
        &["daemon".into(), "serve".into()],
        &std::env::current_dir()?,
        &error_file(data),
        ErrorFile::Append,
    )
}

pub(super) async fn ensure(program: &Path, data: &Path) {
    if wisp_start::paths::in_job() || state(data).await != State::NotStarted {
        return;
    }
    if let Ok(child) = spawn(program, data) {
        tokio::spawn(async move {
            let mut child = child;
            let _ = child.wait().await;
        });
    }
}

pub(super) async fn start(data: &Path) -> io::Result<bool> {
    wisp_core::session::create_private_dir(&link::daemon_dir(data))?;
    match fs::remove_file(stopped(data)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let launched = if state(data).await == State::Running {
        Ok(None)
    } else {
        let program = std::env::current_exe()?;
        async {
            let mut child = spawn(&program, data)?;
            drop(child.stdin.take());
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("the daemon ready pipe is missing"))?;
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            Ok::<_, io::Error>(keep::read_ready(stdout, |_| {}).await)
        }
        .await
    };
    if matches!(launched, Ok(Some(Ready::Socket { .. }))) {
        Ok(true)
    } else if state(data).await == State::Running {
        Ok(false)
    } else {
        Err(not_started(data))
    }
}

fn not_started(data: &Path) -> io::Error {
    io::Error::other(format!(
        "the daemon didn't start; see {}",
        wisp_core::workspace::home_shown(&error_file(data))
    ))
}

pub(super) async fn restart(data: &Path) -> io::Result<bool> {
    tokio::time::timeout(keep::STARTS_WITHIN, async {
        if !stop(data).await? {
            return Ok(false);
        }
        loop {
            if UnixStream::connect(link::daemon_socket(data))
                .await
                .is_err()
                && wispd::daemon::lock(data).is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if start(data).await? {
            Ok(true)
        } else {
            Err(not_started(data))
        }
    })
    .await
    .unwrap_or_else(|_| Err(not_started(data)))
}

pub(super) async fn stop(data: &Path) -> io::Result<bool> {
    let running = state(data).await == State::Running;
    if running {
        client::stop(&link::daemon_socket(data))
            .await
            .map_err(io::Error::other)?;
    }
    Ok(running)
}

async fn serve(data: &Path) -> io::Result<()> {
    let program = std::env::current_exe()?;
    let _hangups = signal(SignalKind::hangup())?;
    let _interrupts = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let chat_data = data.to_owned();
    let daemon = wispd::daemon::Daemon::new(
        data,
        move |folder, resume| {
            let program = program.clone();
            let data = chat_data.clone();
            async move { start_chat(&program, &data, &folder, resume).await }
        },
        wispd::daemon::in_job,
    )?;
    OpenOptions::new()
        .write(true)
        .open(error_file(data))?
        .set_len(0)?;
    daemon
        .run(
            |path| {
                let _ = crate::host::ready(Ready::Socket {
                    path: path.to_owned(),
                });
                if let Ok(null) = OpenOptions::new().write(true).open("/dev/null") {
                    let _ = rustix::stdio::dup2_stdout(&null);
                }
            },
            async {
                terminate.recv().await;
            },
        )
        .await
}

async fn start_chat(
    program: &Path,
    data: &Path,
    folder: &Path,
    resume: Option<String>,
) -> io::Result<(
    Option<Ready>,
    impl Future<Output = ()> + Send + 'static + use<>,
)> {
    let (ready, child, _) = keep::start_host(
        program,
        data,
        &[],
        folder,
        resume.as_deref(),
        false,
        &Start::default(),
        |_| {},
    )
    .await;
    Ok((ready, async move {
        if let Some(mut child) = child {
            let _ = child.wait().await;
        }
    }))
}

async fn status(data: &Path) -> io::Result<()> {
    let warning = match keep::windows_stops_now() {
        missing @ keep::WindowsStops::Missing { .. } => Some(missing.warning()),
        keep::WindowsStops::Restart => Some(keep::wsl_restart()),
        keep::WindowsStops::Nothing => None,
    };
    if let Some(warning) = warning {
        use wisp_render::styled::{StyledLine, StyledSpan};
        use wisp_render::theme::Token;
        let caps = crate::prompt_caps(&wisp_tui::term::caps::SavedDisplay::default());
        let lines = warning
            .lines()
            .map(|line| StyledLine::new(vec![StyledSpan::new(line, Token::Warn)]))
            .collect::<Vec<_>>();
        crate::auth::login::print_lines(
            &lines,
            &caps,
            wisp_tui::term::print::Stream::Stdout,
            wisp_tui::term::prompt::terminal_columns(),
        )?;
        println!();
    }
    let state = state(data).await;
    println!(
        "Daemon: {}",
        match state {
            State::Running => "running",
            State::Stopped => "stopped",
            State::NotStarted => "not started",
        }
    );
    if state != State::Running {
        println!("{START_WITH}");
    }
    let rows = crate::restart::find(data)
        .await?
        .into_iter()
        .map(|found| found.chat.kept)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        println!("\nNo chat is kept open.");
    } else {
        let layout = wisp_render::markdown::Layout::new(
            wisp_tui::term::prompt::terminal_columns(),
            wisp_tui::term::prompt::glyphs().set,
        );
        println!("\nChats kept open");
        let caps = crate::prompt_caps(&wisp_tui::term::caps::SavedDisplay::default());
        crate::auth::login::print_lines(
            &wisp_render::chrome::restart::daemon(&rows, layout),
            &caps,
            wisp_tui::term::print::Stream::Stdout,
            layout.width,
        )?;
        println!("\nOpen one: wisp --resume <id>\nQuit one: wisp quit <id>");
        if rows.iter().any(|chat| chat.older) {
            let text = "Restart the chats kept open on the newest version: wisp\u{a0}restart";
            for line in wisp_render::text::wrap_plain(
                text,
                layout.width,
                "",
                "",
                &wisp_tui::term::prompt::glyphs(),
            ) {
                println!("{}", line.replace('\u{a0}', " "));
            }
        }
    }
    Ok(())
}
