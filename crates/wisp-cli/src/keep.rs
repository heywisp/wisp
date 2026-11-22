use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::Instant;
use wisp_client::link::{self as client, Link, LinkError};
use wisp_protocol::jsonl;
use wisp_protocol::link::{self, Ready, Start};
use wisp_start::paths::{home_dir, xdg_data_home};
use wisp_tui::present::choose::Chosen;

use crate::output::RunEnd;
use crate::{Args, Setup};

const BACK_MAX: Duration = Duration::from_secs(30);
pub(super) const STARTS_WITHIN: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct Back {
    pub(super) restarting: bool,
    at: Option<Instant>,
    wait: Duration,
    block: Option<Vec<u8>>,
    pub(super) unsent: Option<wisp_tui::app::Unsent>,
}

impl Back {
    pub(super) fn next(
        &mut self,
        block: Vec<u8>,
        session: Option<String>,
        now: Instant,
    ) -> Result<String, Vec<u8>> {
        let Some(session) = session else {
            return Err(block);
        };
        self.wait = if self.wait.is_zero()
            || self.at.is_some_and(|at| now.duration_since(at) >= BACK_MAX)
        {
            Duration::from_secs(1)
        } else {
            self.wait.saturating_mul(2).min(BACK_MAX)
        };
        self.at = None;
        self.block = Some(block);
        Ok(session)
    }

    pub(super) fn note(&self) -> Option<String> {
        self.block
            .as_ref()
            .filter(|_| !self.restarting)
            .map(|_| wisp_render::transcript::copy::WINDOW_REOPENED.to_owned())
    }

    pub(super) fn failed(&mut self, block: Option<Vec<u8>>) -> bool {
        block.or(self.block.take()).is_some()
    }

    pub(super) fn started(&mut self) {
        self.at = Some(Instant::now());
        self.block = None;
        self.restarting = false;
    }

    fn close(&mut self, session: &str) -> Opened {
        use wisp_render::transcript::copy;
        self.started();
        println!(
            "\n{}\n{} {}",
            copy::CHAT_STAYS_OPEN,
            copy::RESUME_COMMAND,
            wisp_protocol::short_id(session)
        );
        hand_back(self.unsent.as_ref());
        Opened::Done(std::process::ExitCode::SUCCESS)
    }

    fn gone(&mut self) -> Opened {
        if let Some(block) = self.block.take() {
            let _ = io::stdout().write_all(&block);
        }
        hand_back(self.unsent.as_ref());
        Opened::Done(std::process::ExitCode::FAILURE)
    }

    fn open<'a>(
        &'a mut self,
        path: &'a Path,
        session: &'a str,
    ) -> std::pin::Pin<Box<impl Future<Output = Result<Opened, String>> + 'a>> {
        Box::pin(async move {
            use wisp_render::transcript::copy;

            let mut interrupt =
                signal(SignalKind::interrupt()).map_err(|error| error.to_string())?;
            let message = if self.restarting {
                copy::WINDOW_RESTARTING
            } else {
                copy::WINDOW_RECONNECTING
            };
            for line in wisp_render::text::wrap_plain(
                message,
                wisp_tui::term::prompt::terminal_columns(),
                "",
                "",
                &wisp_tui::term::prompt::glyphs(),
            ) {
                eprintln!("{line}");
            }
            let reconnect = async {
                if self.restarting {
                    let connected = match dial(path).await {
                        Ok(connection) => client::open(connection)
                            .await
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(error),
                    };
                    return match connected {
                        Ok(link) => Ok(Opened::Link(Box::new(link))),
                        Err(_) => {
                            for line in wisp_tui::app::host_errors(&link::error_file(path)) {
                                println!("{line}");
                            }
                            Ok(self.gone())
                        }
                    };
                }
                loop {
                    tokio::time::sleep(self.wait).await;
                    match attach(path, Some(BACK_MAX)).await {
                        Ok(link) => return Ok(Opened::Link(Box::new(link))),
                        Err(error) if error.is_gone() => {
                            return Ok(self.gone());
                        }
                        _ => {}
                    }
                    self.wait = self.wait.saturating_mul(2).min(BACK_MAX);
                }
            };
            tokio::select! {
                result = reconnect => result,
                _ = interrupt.recv() => Ok(self.close(session)),
            }
        })
    }
}

const OTHER_VERSION: &str = "chat was started by a version of Wisp this one can't talk to";

pub(super) const DATA_UNKNOWN: &str = "the path to Wisp's data folder is unknown";

pub(super) const LOG_OFF: &str = "the session log is off.";
pub(super) const UNLOCKED: &str = "it was opened with --resume-unlocked.";

pub(super) fn unavailable(path: &Path, error: &io::Error) -> String {
    format!(
        "{} can't be used: {error}.",
        wisp_core::workspace::home_shown(path)
    )
}

pub(super) fn data() -> Option<PathBuf> {
    wisp_core::session::JsonlStore::default_dir(xdg_data_home(), home_dir())
        .and_then(|dir| dir.parent().map(Path::to_owned))
}

pub(super) fn wanted(args: &Args, log: bool) -> Result<PathBuf, String> {
    if !log {
        return Err(LOG_OFF.to_owned());
    }
    if args.resume_unlocked {
        return Err(UNLOCKED.to_owned());
    }
    let data = data().ok_or_else(|| format!("{DATA_UNKNOWN}."))?;
    let path = link::socket(&data, "00000000-0000-0000-0000-000000000000");
    if path.as_os_str().len() > if cfg!(target_os = "macos") { 103 } else { 107 } {
        return Err("the path to Wisp's data folder is too long.".to_owned());
    }
    Ok(data)
}

pub(super) enum ErrorFile {
    Replace,
    Append,
}

pub(super) fn spawn(
    program: &Path,
    arguments: &[OsString],
    folder: &Path,
    error_file: &Path,
    mode: ErrorFile,
) -> io::Result<Child> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(matches!(mode, ErrorFile::Replace))
        .append(matches!(mode, ErrorFile::Append))
        .write(true)
        .mode(0o600)
        .open(error_file)?;
    let mut command = if wisp_start::paths::in_job() {
        Command::new(program)
    } else if let Some(setsid) = wisp_core::jobs::exec::find_program("setsid") {
        let mut command = Command::new(setsid);
        command.arg(program);
        command
    } else {
        let mut command = Command::new(program);
        command.process_group(0);
        command
    };
    command
        .args(arguments)
        .current_dir(folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(file)
        .kill_on_drop(false)
        .spawn()
}

pub(super) enum Opened {
    Link(Box<Link>),
    Window(String),
    Ask,
    Done(std::process::ExitCode),
}

pub(super) fn hand_back(unsent: Option<&wisp_tui::app::Unsent>) {
    if let Some(unsent) = unsent {
        let texts = unsent.texts();
        if !texts.is_empty() {
            println!("\n{}", wisp_render::transcript::copy::NOT_SENT);
            println!(
                "{}",
                texts
                    .iter()
                    .map(|text| wisp_render::text::sanitize(text))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            );
        }
    }
}

pub(super) async fn open(
    program: &Path,
    args: &Args,
    setup: &Setup,
    keychain: &wisp_start::keychain::Keychain,
    chosen: &Chosen,
    at: Option<&wisp_start::start::StartAt>,
    back: Option<&mut Back>,
) -> Result<Opened, String> {
    if let Some(back) = back.filter(|back| back.block.is_some())
        && let Some(Some(id)) = &args.resume
    {
        let data = data().ok_or(DATA_UNKNOWN)?;
        let mut opened = back.open(&link::socket(&data, id), id).await?;
        if let Opened::Link(link) = &opened
            && let Some(unsent) = &mut back.unsent
        {
            unsent.reopened(&link.view.input_ids);
        }
        if let Opened::Link(link) = &mut opened
            && back.restarting
        {
            let mut interrupt =
                signal(SignalKind::interrupt()).map_err(|error| error.to_string())?;
            let stop = async {
                tokio::select! {
                    _ = tokio::time::sleep(STARTS_WITHIN) => {},
                    _ = interrupt.recv() => {},
                }
            };
            let sent = match &mut back.unsent {
                Some(unsent) => {
                    unsent
                        .send_again(
                            &mut link.channels,
                            &mut link.view.input_ids,
                            &link.view.returned,
                            stop,
                        )
                        .await
                }
                None => true,
            };
            if sent
                && back
                    .unsent
                    .as_ref()
                    .is_none_or(wisp_tui::app::Unsent::is_empty)
            {
                use std::os::unix::process::CommandExt;
                let error = std::process::Command::new(program)
                    .args(resume_args(
                        &args.host_args,
                        Some(&link.welcome.session),
                        true,
                    ))
                    .exec();
                return Err(error.to_string());
            }
        }
        return Ok(opened);
    }
    let chooser = crate::Chooser::of(args, setup, keychain);
    let data = match wanted(args, chooser.log) {
        Ok(data) => data,
        Err(reason) => return Ok(Opened::Window(window(&reason))),
    };
    crate::daemon::ensure(program, &data).await;
    if let Some(Some(prefix)) = &args.resume {
        let id = by_id(&data, prefix).ok().map(|(_, id)| id);
        if let Some(id) = id {
            let socket = link::socket(&data, &id);
            match attach(&socket, None).await {
                Ok(link) => return Ok(Opened::Link(Box::new(link))),
                Err(error) if error.is_gone() => {}
                Err(error @ LinkError::Refused { .. }) => {
                    if !may_ask(chosen, at) {
                        return Err(link_error(error));
                    }
                    if let Err(code) = restart(args, setup, keychain) {
                        return Ok(Opened::Done(code));
                    }
                    end(client::end(
                        client::dial(&socket).await.map_err(link_error)?,
                        false,
                    ))
                    .await
                    .map_err(|error| format!("wisp: {error}"))?;
                }
                Err(error) => return Err(link_error(error)),
            }
        }
    }
    let folder = std::env::current_dir().map_err(|e| e.to_string())?;
    let start = Start {
        asks: crate::asks(chosen, at),
        terminal: std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stderr()),
        account: at.map(|at| at.account.clone()),
        follow: at.map_or_else(Vec::new, |at| at.follow.clone()),
    };
    let (ready, child, error) = start_host(
        program,
        &data,
        &args.host_args,
        &folder,
        args.resume.as_ref().and_then(Option::as_deref),
        false,
        &start,
        |text| eprintln!("{text}"),
    )
    .await;
    if let Some(mut child) = child {
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
    }
    match ready {
        Some(Ready::Socket { path }) => {
            return attach(&path, None)
                .await
                .map(|link| Opened::Link(Box::new(link)))
                .map_err(link_error);
        }
        Some(Ready::Window { reason }) => return Ok(Opened::Window(window(&reason))),
        Some(Ready::Ask) => return Ok(Opened::Ask),
        Some(Ready::Failed { text }) => return Err(text),
        None | Some(Ready::Notice { .. }) => {}
    }

    Ok(Opened::Window(window(&format!(
        "it stopped while starting; see {}",
        wisp_core::workspace::home_shown(&error)
    ))))
}

#[allow(
    clippy::too_many_arguments,
    reason = "host launches keep their program, folder, resume mode and start line explicit"
)]
pub(super) async fn start_host(
    program: &Path,
    data: &Path,
    extra: &[OsString],
    folder: &Path,
    resume: Option<&str>,
    restarted: bool,
    start: &Start,
    notice: impl FnMut(String),
) -> (Option<Ready>, Option<Child>, PathBuf) {
    let error = link::error_file(&link::socket(data, &wisp_core::session::new_session_id()));
    if let Err(failure) = wisp_core::session::create_private_dir(&link::run_dir(data)) {
        return (
            Some(Ready::Window {
                reason: unavailable(&link::run_dir(data), &failure),
            }),
            None,
            error,
        );
    }
    let mut host = OsString::from("--host=");
    host.push(&error);
    let mut arguments = vec![host];
    arguments.extend(resume_args(extra, resume, restarted));
    let Ok(mut child) = spawn(program, &arguments, folder, &error, ErrorFile::Replace) else {
        return (None, None, error);
    };
    let ready = async {
        let mut stdin = child.stdin.take()?;
        stdin
            .write_all(jsonl::line(start).ok()?.as_bytes())
            .await
            .ok()?;
        drop(stdin);
        read_ready(child.stdout.take()?, notice).await
    }
    .await;
    if matches!(
        ready,
        Some(Ready::Window { .. } | Ready::Ask | Ready::Failed { .. })
    ) {
        let _ = std::fs::remove_file(&error);
    }
    (ready, Some(child), error)
}

fn resume_args(extra: &[OsString], resume: Option<&str>, restarted: bool) -> Vec<OsString> {
    let mut arguments = Vec::new();
    let mut raw = extra.iter().peekable();
    while let Some(arg) = raw.next() {
        if arg == "--resume" {
            if raw
                .peek()
                .is_some_and(|next| !next.to_string_lossy().starts_with('-'))
            {
                raw.next();
            }
        } else if arg != "--restarted" && !arg.to_string_lossy().starts_with("--resume=") {
            arguments.push(arg.clone());
        }
    }
    if let Some(id) = resume {
        arguments.push(format!("--resume={id}").into());
    }
    if restarted {
        arguments.push("--restarted".into());
    }
    arguments
}

pub(super) async fn read_ready<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    mut notice: impl FnMut(String),
) -> Option<Ready> {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match link::read(&line) {
            Some(Ready::Notice { text }) => notice(text),
            Some(ready) => return Some(ready),
            None => {}
        }
    }
    None
}

fn may_ask(chosen: &Chosen, at: Option<&wisp_start::start::StartAt>) -> bool {
    crate::asks(chosen, at) && !wisp_start::paths::in_job()
}

fn restart(
    args: &Args,
    setup: &Setup,
    keychain: &wisp_start::keychain::Keychain,
) -> Result<(), std::process::ExitCode> {
    use crate::manage::{self, Asked};
    const DECLINED: &str = "not restarted; it stays open in the Wisp it started with";
    let model = crate::model_of(args, setup);
    let context = crate::manage_context(args, setup, &model, keychain);
    let prompter = context.prompter(&format!("wisp: {DECLINED}"));
    match manage::confirm(
        &prompter,
        &format!(
            "This {OTHER_VERSION}.\nRestarting it stops any turn it is running, reopens it from its log and may rebuild each agent's cache.\nRestart it to open it here?"
        ),
        false,
    ) {
        Asked::Answer(true) => {
            eprintln!("Restarting the chat…");
            Ok(())
        }
        Asked::Answer(false) | Asked::Declined => Err(manage::fail(DECLINED)),
        Asked::Cancelled => Err(manage::cancelled(&prompter)),
        Asked::Failed(error) => Err(manage::fail(error)),
    }
}

async fn attach(path: &Path, wait: Option<Duration>) -> Result<Link, LinkError> {
    let connecting = client::connect(path, crate::host::build());
    let connection = match wait {
        Some(wait) => tokio::time::timeout(wait, connecting)
            .await
            .map_err(|_| LinkError::Io(io::ErrorKind::TimedOut))??,
        None => connecting.await?,
    };
    client::open(connection).await
}

pub(super) async fn dial(
    path: &Path,
) -> Result<client::Connection<tokio::net::UnixStream>, String> {
    let by = Instant::now() + STARTS_WITHIN;
    loop {
        let failed = link::error_file(path).exists();
        match tokio::time::timeout_at(by, client::connect(path, crate::host::build())).await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(error)) if error.is_gone() => {
                if failed {
                    return Err(std::fs::read_to_string(link::error_file(path)).unwrap_or_default());
                }
            }
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_) => return Err(crate::host::no_restart_answer()),
        }
        if Instant::now() >= by {
            return Err(crate::host::no_restart_answer());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn by_id(data: &Path, prefix: &str) -> Result<(wisp_core::session::JsonlStore, String), String> {
    let store = wisp_core::session::JsonlStore::open(data.join("sessions"))
        .map_err(|error| error.to_string())?;
    let id = wisp_core::resume::find(&store, wisp_core::resume::Wanted::Id(prefix))?.id;
    Ok((store, id))
}

pub(super) async fn quit(ids: &[String]) -> std::process::ExitCode {
    if wisp_start::paths::in_job() {
        return crate::manage::fail("run by an agent, wisp quit quits nothing");
    }
    let Some(data) = data() else {
        return crate::manage::fail(DATA_UNKNOWN);
    };
    let mut code = std::process::ExitCode::SUCCESS;
    for prefix in ids {
        match quit_one(&data, prefix).await {
            Ok(id) => println!("Quit {}: it has ended.", wisp_protocol::short_id(&id)),
            Err(error) => code = crate::manage::fail(error),
        }
    }
    code
}

async fn quit_one(data: &Path, prefix: &str) -> Result<String, String> {
    let (store, id) = by_id(data, prefix)?;
    let window = || {
        format!(
            "{} is open in a window; quit it there",
            wisp_protocol::short_id(&id)
        )
    };
    match client::connect(&link::socket(data, &id), crate::host::build()).await {
        Ok(mut connection) => {
            let saved = connection
                .end_if_windowless()
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(window)?;
            end(async { Ok(saved) }).await?;
            Ok(id)
        }
        Err(error) if error.is_gone() => match store.probe(&id) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(window()),
            Err(error) => Err(error.to_string()),
            Ok(_) => Err(format!(
                "{} already ended; nothing to quit",
                wisp_protocol::short_id(&id)
            )),
        },
        Err(error) => Err(error.to_string()),
    }
}

async fn end(ending: impl Future<Output = Result<bool, LinkError>>) -> Result<(), String> {
    if ending.await.map_err(|error| error.to_string())? {
        Ok(())
    } else {
        Err("the chat ended before its log was saved".to_owned())
    }
}

fn link_error(error: LinkError) -> String {
    if matches!(error, LinkError::Refused { .. }) {
        format!("wisp: this {OTHER_VERSION}")
    } else {
        format!("wisp: {error}")
    }
}

pub(super) fn window(reason: &str) -> String {
    format!("This chat can't stay open without its window: {reason}")
}

pub(super) fn session(
    link: &mut Link,
    notes: Vec<String>,
    restarted: bool,
    unsent: bool,
) -> (
    wisp_tui::app::setup::SessionStart,
    wisp_protocol::opening::CardSetup,
) {
    let mut opening = link.opening.clone();
    let start = opening.start.get_or_insert_with(Default::default);
    start.warnings.extend(notes);
    if restarted {
        start
            .warnings
            .push(wisp_render::transcript::copy::window_restarted(
                unsent,
                wisp_tui::app::resumed::question_interrupted(&link.view),
            ));
    }
    if link.welcome.build < crate::host::build() {
        start
            .warnings
            .push(wisp_render::transcript::copy::CHAT_OLDER.to_owned());
    }
    let events = std::mem::replace(&mut link.channels.events, tokio::sync::mpsc::channel(1).1);
    let channels = wisp_client::Channels {
        commands: link.channels.commands.clone(),
        events,
        quit: None,
    };
    let view = (link.now.is_some() || link.view != wisp_protocol::SessionView::default())
        .then(|| std::mem::take(&mut link.view));
    let (mut start, card) = crate::tui::session(opening, view, channels);
    start.session.attached = Some(wisp_tui::app::Attached {
        first_request: link.welcome.first_request,
        now: link.now.take(),
        end: link.end.clone(),
        ended: link.ended.clone(),
        error_file: data()
            .map(|data| {
                wisp_protocol::link::error_file(&wisp_protocol::link::socket(
                    &data,
                    &link.welcome.session,
                ))
            })
            .unwrap_or_default(),
    });
    (start, card)
}

pub(super) async fn finish(link: &mut Link, exit: &RunEnd) -> std::process::ExitCode {
    let done = exit.exit_code();
    if exit.leave || exit.resume.is_some() || exit.code != 0 {
        if exit.forced && exit.resume.is_none() && link.ended.borrow().is_none() {
            let _ = link.end.try_send(true);
        }
        return done;
    }
    let ended = link.ended.borrow().clone();
    let saved = match ended {
        Some(wisp_client::link::LinkEnd::Ended { saved }) => saved,
        Some(wisp_client::link::LinkEnd::Taken | wisp_client::link::LinkEnd::Restarting) => {
            return done;
        }
        Some(wisp_client::link::LinkEnd::Lost | wisp_client::link::LinkEnd::Cut) => false,
        None => end(link.end(false)).await.is_ok(),
    };
    if saved {
        done
    } else {
        std::process::ExitCode::from(wisp_start::start::FORCED_EXIT)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WindowsStops {
    Nothing,
    Missing { file: Option<PathBuf>, offer: bool },
    Restart,
}

const WSL_LINES: &str = "[general]\ninstanceIdleTimeout=-1\n";
const WSL_STOPS: &str =
    "15 seconds after the last terminal closes, Windows usually stops Linux, ending every chat.";
const WSL_SHUTDOWN: &str =
    "wsl.exe --shutdown once, here or in Windows; it ends everything in Linux this one time.";

fn wsl_wrap(text: &str, width: usize) -> String {
    wisp_render::text::wrap_plain(
        text,
        width,
        "",
        "",
        &wisp_render::theme::glyphs::Glyphs::of(wisp_render::theme::glyphs::GlyphSet::Unicode),
    )
    .join("\n")
}

pub(super) fn wsl_restart() -> String {
    format!(
        ".wslconfig changed since Linux\nstarted; Windows reads it only\nwhen Linux starts. Run\n{}",
        wsl_wrap(WSL_SHUTDOWN, 33)
    )
}

fn wsl_setting() -> (&'static str, &'static str) {
    WSL_LINES
        .lines()
        .find_map(|line| line.split_once('='))
        .unwrap_or_default()
}

fn indented_wsl_lines() -> String {
    WSL_LINES
        .lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn general(text: &str) -> (Option<usize>, Vec<(usize, &str)>) {
    let mut header = None;
    let mut inside = false;
    let mut values = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.starts_with('[') {
            inside = Some(line) == WSL_LINES.lines().next();
            if inside && header.is_none() {
                header = Some(index);
            }
        } else if inside
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == wsl_setting().0
        {
            values.push((index, value.trim()));
        }
    }
    (header, values)
}

pub(super) fn windows_stops(root: &Path, path: &OsStr) -> WindowsStops {
    use wisp_core::secrets::detect::{WslFacts, wsl2_release};
    let facts = WslFacts::read(root);
    if !facts.osrelease.as_deref().is_some_and(wsl2_release) {
        return WindowsStops::Nothing;
    }
    let file = std::env::split_paths(path)
        .filter(|entry| entry.ends_with("AppData/Local/Microsoft/WindowsApps"))
        .filter_map(|entry| entry.ancestors().nth(4).map(Path::to_owned))
        .filter_map(|folder| {
            folder
                .strip_prefix("/")
                .ok()
                .map(|folder| root.join(folder))
        })
        .find(|folder| folder.is_dir())
        .map(|folder| folder.join(".wslconfig"));
    let Some(file) = file else {
        return WindowsStops::Missing {
            file: None,
            offer: false,
        };
    };
    let meta = std::fs::symlink_metadata(&file);
    let text = std::fs::read_to_string(&file);
    if let Ok(text) = &text
        && general(text)
            .1
            .last()
            .is_some_and(|(_, value)| *value == wsl_setting().1)
    {
        let boot = std::fs::read_to_string(root.join("proc/stat"))
            .ok()
            .and_then(|text| wisp_core::jobs::exec::boot_time(&text))
            .and_then(|seconds| u64::try_from(seconds).ok())
            .map(std::time::Duration::from_secs);
        let modified = std::fs::metadata(&file)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok());
        return if boot
            .zip(modified)
            .is_some_and(|(boot, modified)| modified > boot)
        {
            WindowsStops::Restart
        } else {
            WindowsStops::Nothing
        };
    }
    let readable = text.is_ok()
        || text
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
    let plain = meta
        .as_ref()
        .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
        || meta
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
    WindowsStops::Missing {
        file: Some(file),
        offer: readable && plain,
    }
}

pub(super) fn windows_stops_now() -> WindowsStops {
    let root = wisp_tui::term::host::wsl_root();
    windows_stops(&root, &std::env::var_os("PATH").unwrap_or_default())
}

impl WindowsStops {
    pub(super) fn warning(&self) -> String {
        let file = match self {
            Self::Missing {
                file: Some(file), ..
            } => file.display().to_string(),
            _ => "%UserProfile%\\.wslconfig".to_owned(),
        };
        let lines = indented_wsl_lines();
        let lead = wsl_wrap(
            &format!("About {WSL_STOPS} To prevent it, add these two lines to"),
            34,
        );
        let restart = wsl_wrap(&format!("Then run {WSL_SHUTDOWN}"), 34);
        format!("{lead}\n{file}:\n{lines}\n{restart}")
    }
}

pub(super) fn windows_notes(error: Option<&str>) -> Vec<String> {
    windows_notes_of(windows_stops_now(), error)
}

fn windows_notes_of(stops: WindowsStops, error: Option<&str>) -> Vec<String> {
    match stops {
        WindowsStops::Restart => vec![wsl_restart()],
        missing @ WindowsStops::Missing { .. } if error.is_some() => vec![
            format!(
                "Wisp couldn't add the two lines: {}.",
                error.unwrap_or_default()
            ),
            missing.warning(),
        ],
        _ => Vec::new(),
    }
}

fn with_line(text: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let (header, values) = general(text);
    let (key, value) = wsl_setting();
    let setting_line = format!("{key}={value}");
    let setting = format!("{setting_line}{newline}");
    let mut out = String::new();
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if values.iter().any(|(at, _)| *at == index) {
            let ending = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            out.push_str(&setting_line);
            out.push_str(ending);
        } else {
            out.push_str(line);
        }
        if header == Some(index) && values.is_empty() {
            if !out.ends_with('\n') {
                out.push_str(newline);
            }
            out.push_str(&setting);
        }
    }
    if header.is_none() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(newline);
        }
        out.push_str(&WSL_LINES.replace('\n', newline));
    }
    out
}

fn refused_operation(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
    )
}

fn add(file: &Path) -> io::Result<()> {
    add_with(file, wisp_core::secrets::write_atomic)
}

fn add_with(
    file: &Path,
    write: impl FnOnce(&Path, &[u8], u32) -> io::Result<()>,
) -> io::Result<()> {
    use std::io::Write;
    let metadata = match std::fs::symlink_metadata(file) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the file is a link",
            ));
        }
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let text = if metadata.is_some() {
        std::fs::read_to_string(file)?
    } else {
        String::new()
    };
    let text = with_line(&text);
    let mode = metadata
        .as_ref()
        .map_or(0o644, wisp_core::config_file::file_mode);
    match write(file, text.as_bytes(), mode) {
        Ok(()) => Ok(()),
        Err(_) if std::fs::read(file).is_ok_and(|bytes| bytes == text.as_bytes()) => Ok(()),
        Err(error) if refused_operation(&error) => {
            let mut temp = tempfile::NamedTempFile::new_in(file.parent().ok_or(error)?)?;
            if let Err(error) = temp
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(mode))
                && !refused_operation(&error)
            {
                return Err(error);
            }
            temp.write_all(text.as_bytes())?;
            temp.as_file().sync_all()?;
            temp.persist(file).map_err(|error| error.error)?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub(super) fn offer(
    args: &Args,
    setup: &Setup,
    keychain: &wisp_start::keychain::Keychain,
    chosen: &Chosen,
) -> Result<Option<String>, std::process::ExitCode> {
    use crate::manage;
    use crate::manage::Asked;
    if !may_ask(chosen, None) {
        return Ok(None);
    }
    let Some(data) = data() else {
        return Ok(None);
    };
    let declined = data.join("wsl-declined");
    let WindowsStops::Missing {
        file: Some(file),
        offer: true,
    } = windows_stops_now()
    else {
        return Ok(None);
    };
    if declined.exists() {
        return Ok(None);
    }
    let model = crate::model_of(args, setup);
    let context = crate::manage_context(args, setup, &model, keychain);
    let prompter = context.prompter("Cancelled; nothing was changed.");
    let lines = indented_wsl_lines();
    let lead = format!(
        "Chats stay open when their window closes, but about {WSL_STOPS}\nOne Windows setting prevents that:\n{lines}\nAdd these two lines to {}?",
        file.display()
    );
    match manage::choose(&prompter, &lead, &["Yes", "No, don't ask again"], 1) {
        Asked::Answer(0) => Ok(add(&file).err().map(|error| error.to_string())),
        Asked::Answer(_) => {
            wisp_core::session::create_private_dir(&data)
                .and_then(|()| wisp_core::secrets::write_atomic(&declined, b"", 0o600))
                .map_err(manage::fail)?;
            Ok(None)
        }
        Asked::Declined => Ok(None),
        Asked::Cancelled => Err(manage::cancelled(&prompter)),
        Asked::Failed(error) => Err(manage::fail(error)),
    }
}

#[cfg(test)]
mod tests;
