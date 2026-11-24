use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::pin::Pin;
use std::process::ExitCode;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use wisp_client::link::{self as client, Connection, LinkError};
use wisp_protocol::link::{self, Start, ToClient};
use wisp_protocol::{AgentEvent, RestartState, Unready};
use wisp_render::chrome::restart::{self as render, Chat, End, Kept, State};
use wisp_render::markdown::Layout;
use wisp_render::styled::StyledLine;
use wisp_tui::term::prompt::{self, PromptError, Prompter};
use wisp_tui::term::{caps, print};

use crate::keep;

const ANSWERS_WITHIN: Duration = Duration::from_secs(10);
const WAIT: Duration = Duration::from_secs(5 * 60);

pub(super) struct Found {
    pub(super) chat: Chat,
    connection: Option<Connection<UnixStream>>,
    inside: bool,
}

#[derive(Clone, Copy)]
enum Control {
    Wait,
    Go,
    Cancel,
}

pub(super) async fn find(data: &Path, ids: Vec<String>) -> Vec<Found> {
    let titles = if !wisp_start::paths::in_job() {
        let data = data.to_owned();
        let ids = ids.clone();
        tokio::task::spawn_blocking(move || {
            keep::store(&data)
                .map(|store| wisp_core::resume::rows_of(&store, &ids))
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    futures::future::join_all(ids.into_iter().map(|id| {
        let titles = &titles;
        async move {
            let mut inside = false;
            let (connection, state, inside) = match tokio::time::timeout(ANSWERS_WITHIN, async {
                let stream = client::dial(&link::socket(data, &id)).await?;
                inside = wispd::runs_inside(&stream);
                client::handshake(stream, crate::host::build()).await
            })
            .await
            {
                Ok(Ok(connection)) => (Some(connection), State::Restarting, false),
                Ok(Err(error)) => (
                    None,
                    State::Failed(error.to_string()),
                    inside && matches!(error, LinkError::Io(_) | LinkError::Closed),
                ),
                Err(_) => (None, State::NoAnswer, false),
            };
            let welcome = connection.as_ref().map(|connection| &connection.welcome);
            let title = titles
                .iter()
                .find(|row| row.id == id)
                .map(wisp_render::chrome::title_of)
                .filter(|title| !title.is_empty())
                .or_else(|| {
                    welcome.map(|welcome| {
                        wisp_render::text::clean(&wisp_core::workspace::home_shown(&welcome.folder))
                    })
                })
                .unwrap_or_default();
            let kept = Kept {
                id,
                title,
                window: welcome.is_some_and(|welcome| welcome.window),
                older: welcome.is_some_and(|welcome| welcome.build < crate::host::build()),
            };
            Found {
                chat: Chat { kept, state },
                connection,
                inside,
            }
        }
    }))
    .await
}

async fn kept(data: &Path, prefix: &str) -> Result<String, String> {
    let (store, id) = keep::by_id(data, prefix)?;
    client::dial(&link::socket(data, &id))
        .await
        .map_err(|error| {
            keep::unreached(
                &store,
                &id,
                error,
                "lives in its window; quit it there and open it again",
                "already ended; nothing to restart",
            )
        })?;
    Ok(id)
}

async fn named(data: &Path, prefixes: &[String]) -> (Vec<String>, ExitCode) {
    let mut ids = Vec::new();
    let mut code = ExitCode::SUCCESS;
    for prefix in prefixes {
        match kept(data, prefix).await {
            Ok(id) if !ids.contains(&id) => ids.push(id),
            Ok(_) => {}
            Err(error) => code = crate::manage::fail(wisp_render::chrome::one_line(&error)),
        }
    }
    (ids, code)
}

fn lost(data: &Path, id: &str, reason: String) -> State {
    let held = keep::store(data)
        .and_then(|store| keep::held(&store, id))
        .unwrap_or(false);
    if held {
        State::StillRunning(reason)
    } else {
        State::Failed(reason)
    }
}

fn ask_older(older: &[Kept], terminal: bool, prompter: &Prompter, layout: Layout) -> bool {
    if older.is_empty() {
        return true;
    }
    let question = render::older_question(older, layout);
    if !terminal {
        let _ = io::stdout().write_all(&print::styled(
            &question,
            prompter.caps(),
            print::Stream::Stdout,
            layout.width,
        ));
        return false;
    }
    let lead = question
        .iter()
        .map(|line| {
            wisp_render::text::spans_text(&line.spans)
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    matches!(
        crate::manage::confirm(prompter, &lead, false),
        crate::manage::Asked::Answer(true)
    )
}

pub(super) async fn run(ids: &[String]) -> ExitCode {
    if wisp_start::paths::in_job() {
        let caps = crate::prompt_caps(&caps::SavedDisplay::default());
        let layout = Layout::new(prompt::terminal_columns(), prompt::glyphs().set);
        let _ = io::stderr().write_all(&print::styled(
            &render::in_a_job(layout),
            &caps,
            print::Stream::Stderr,
            layout.width,
        ));
        return ExitCode::FAILURE;
    }
    match restart(ids).await {
        Ok(code) => code,
        Err(error) => {
            let text = wisp_render::chrome::one_line(&format!("wisp: {error}"));
            let _ = writeln!(io::stdout(), "{text}");
            ExitCode::FAILURE
        }
    }
}

async fn restart(prefixes: &[String]) -> io::Result<ExitCode> {
    let data = keep::data().ok_or_else(|| io::Error::other(keep::DATA_UNKNOWN))?;
    let program = std::env::current_exe()?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangups = signal(SignalKind::hangup())?;
    let caps = crate::prompt_caps(&caps::SavedDisplay::default());
    let prompter = Prompter::new(render::DECLINED, caps.clone());
    let terminal =
        io::stdout().is_terminal() && io::stdin().is_terminal() && io::stderr().is_terminal();
    let mut layout = Layout::new(prompt::terminal_columns(), prompter.glyphs().set);
    let (ids, mut names) = if prefixes.is_empty() {
        (wispd::daemon::running(&data).await?, ExitCode::SUCCESS)
    } else {
        named(&data, prefixes).await
    };
    if ids.is_empty() && !prefixes.is_empty() {
        return Ok(names);
    }
    let mut found = find(&data, ids).await;
    let inside = found.iter().any(|found| found.inside);
    for found in &mut found {
        if found.inside {
            names = crate::manage::fail(format!(
                "this chat ({}) can't restart itself; restart it from outside",
                wisp_protocol::short_id(&found.chat.kept.id)
            ));
        } else if let State::Failed(reason) = &found.chat.state {
            found.chat.state = lost(&data, &found.chat.kept.id, reason.clone());
        }
    }
    found.retain(|found| !found.inside);
    if inside && (prefixes.is_empty() || found.is_empty()) {
        return Ok(names);
    }
    let older = found
        .iter()
        .filter(|found| {
            found
                .connection
                .as_ref()
                .is_some_and(|connection| !connection.welcome.restart)
        })
        .map(|found| found.chat.kept.clone())
        .collect::<Vec<_>>();
    if !ask_older(&older, terminal, &prompter, layout) {
        let _ = writeln!(io::stdout(), "{}", render::DECLINED);
        return Ok(names);
    }
    prompt::delegate_interrupts_and_hangups();
    let mut end = End {
        older: older.into_iter().map(|chat| chat.id).collect(),
        ..End::default()
    };
    let mut rows = found
        .iter()
        .map(|found| (found.chat.clone(), Instant::now()))
        .collect::<Vec<_>>();
    let (control, controls) = watch::channel(Control::Wait);
    let (updates, mut changed) = mpsc::channel(64);
    let mut tasks = JoinSet::new();
    for (index, found) in found.into_iter().enumerate() {
        let updates = updates.clone();
        let controls = controls.clone();
        let data = data.clone();
        let program = program.clone();
        tasks.spawn(async move {
            chat(found, index, &data, &program, &updates, controls).await;
        });
    }
    drop(updates);
    let mut drawn = 0;
    let mut previous = Vec::new();
    let mut key_task: Option<tokio::task::JoinHandle<Option<bool>>> = None;
    let mut keys_on: Option<watch::Sender<bool>> = None;
    let mut went = !end.older.is_empty() && end.older.len() == rows.len();
    let by = Instant::now() + WAIT;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        let chats = lines(&rows);
        let finishing = chats
            .iter()
            .any(|chat| matches!(chat.state, State::Finishing(_)));
        layout.width = prompt::terminal_columns();
        let height = rustix::termios::tcgetwinsize(io::stdout())
            .map_or(24, |size| usize::from(size.ws_row))
            .saturating_sub(1);
        if !finishing {
            if let Some(on) = keys_on.take() {
                on.send_replace(false);
            }
            if let Some(task) = key_task.take()
                && !task.is_finished()
            {
                let _ = task.await;
            }
        }
        let left = (!went && !end.cancelled).then(|| {
            by.saturating_duration_since(Instant::now())
                .as_secs()
                .max(1)
        });
        let start_keys = terminal && finishing && !went && !end.cancelled && key_task.is_none();
        if start_keys {
            prompt::flush_typed_ahead();
        }
        let next = render::live(&chats, left, height, layout);
        if !chats.is_empty() {
            draw(&next, terminal, &caps, layout, &mut drawn, &mut previous);
        }
        if start_keys {
            let (on, task) = keys(Prompter::new("", caps.clone()));
            keys_on = Some(on);
            key_task = Some(task);
        }
        let mut key_finished = false;
        let heard = tokio::select! {
            update = changed.recv() => match update { Some((index, chat)) => { rows[index] = (chat, Instant::now()); None }, None => break },
            result = async { match &mut key_task { Some(task) => task.await.ok().flatten(), None => std::future::pending().await } }, if !went && !end.cancelled => { key_finished = true; result },
            _ = interrupt.recv(), if !went && !end.cancelled => Some(false),
            _ = hangups.recv(), if !went && !end.cancelled => Some(false),
            _ = tick.tick() => { if !went && !end.cancelled && Instant::now() >= by { end.waited = Some(WAIT.as_secs()); Some(true) } else { None } },
        };
        if key_finished {
            key_task.take();
            keys_on.take();
        }
        if let Some(go) = heard {
            if let Some(on) = keys_on.take() {
                on.send_replace(false);
            }
            if let Some(task) = key_task.take()
                && !task.is_finished()
            {
                let _ = task.await;
            }
            if go {
                went = true;
                control.send_replace(Control::Go);
            } else {
                end.cancelled = true;
                control.send_replace(Control::Cancel);
            }
        }
    }
    if let Some(on) = keys_on {
        on.send_replace(false);
    }
    if let Some(task) = key_task {
        let _ = task.await;
    }
    while tasks.join_next().await.is_some() {}
    let chats = lines(&rows);
    let failed = chats.iter().any(|chat| {
        matches!(
            chat.state,
            State::Failed(_) | State::StillRunning(_) | State::NoAnswer | State::InAnotherRestart
        ) || chat.kept.older
    });
    if !end.cancelled && prefixes.is_empty() {
        end.daemon = crate::daemon::restart(&data).await?;
    }
    let last = render::end(&chats, &end, layout);
    draw(&last, terminal, &caps, layout, &mut drawn, &mut previous);
    Ok(if end.cancelled {
        ExitCode::from(130)
    } else if failed {
        ExitCode::FAILURE
    } else {
        names
    })
}

async fn chat(
    mut found: Found,
    index: usize,
    data: &Path,
    program: &Path,
    updates: &mpsc::Sender<(usize, Chat)>,
    mut control: watch::Receiver<Control>,
) {
    let Some(mut connection) = found.connection.take() else {
        let _ = updates.send((index, found.chat)).await;
        return;
    };
    let path = link::socket(data, &found.chat.kept.id);
    if !connection.welcome.restart {
        older(found.chat, connection, index, data, program, updates).await;
        return;
    }
    if let Err(error) = connection.restart(true).await {
        found.chat.state = lost(data, &found.chat.kept.id, error.to_string());
        let _ = updates.send((index, found.chat)).await;
        return;
    }
    let mut open = Vec::new();
    let mut stopped = Vec::new();
    let mut at = Instant::now();
    let mut heard = false;
    let answer = tokio::time::sleep(ANSWERS_WITHIN);
    tokio::pin!(answer);
    let mut stop_by = None;
    let mut going = false;
    let final_state = loop {
        tokio::select! {
            frame = connection.next() => match frame {
                Ok(ToClient::Event(event)) => match *event {
                    AgentEvent::Restart { state, open: reported } => {
                        heard = true;
                        going |= state == RestartState::Ready;
                        open = reported;
                        at = Instant::now();
                        found.chat.state = if state == RestartState::GettingReady && stop_by.is_none() { State::Finishing(open.clone()) } else { State::Restarting };
                        let _ = updates.send((index, found.chat.clone())).await;
                    }
                    AgentEvent::Rejected { .. } => break State::InAnotherRestart,
                    _ => {}
                },
                Ok(ToClient::Restarting) => { going = true; found.chat.state = State::Restarting; let _ = updates.send((index, found.chat.clone())).await; }
                Ok(ToClient::Ended { .. }) => {
                    break match keep::dial(&path).await {
                        Ok(next) => { found.chat.kept.older = next.welcome.build < crate::host::build(); State::Restarted(stopped) }
                        Err(reason) => lost(data, &found.chat.kept.id, reason),
                    };
                }
                Ok(_) => {}
                Err(error) => break lost(data, &found.chat.kept.id, error.to_string()),
            },
            _ = &mut answer, if !heard && stop_by.is_none() => {
                heard = true;
                found.chat.state = State::NotAnswering;
                let _ = updates.send((index, found.chat.clone())).await;
            }
            changed = control.changed() => {
                if changed.is_err() { break State::Cancelled; }
                let action = *control.borrow_and_update();
                match action {
                    Control::Wait => {}
                    Control::Cancel if !going => break State::Cancelled,
                    Control::Cancel => {},
                    Control::Go => {
                        going = true;
                        stopped = open.clone();
                        wisp_protocol::age_unready(&mut stopped, at.elapsed());
                        if let Err(error) = connection.restart(false).await { break lost(data, &found.chat.kept.id, error.to_string()); }
                        found.chat.state = State::Restarting;
                        let _ = updates.send((index, found.chat.clone())).await;
                        stop_by = Some(Instant::now() + wisp_start::start::SIGNAL_LIMIT + ANSWERS_WITHIN);
                    }
                }
            }
            _ = async { match stop_by { Some(by) => tokio::time::sleep_until(by).await, None => std::future::pending().await } } => break State::NoAnswer,
        }
    };
    found.chat.state = final_state;
    let _ = updates.send((index, found.chat)).await;
}

fn older<'a>(
    mut chat: Chat,
    mut connection: Connection<UnixStream>,
    index: usize,
    data: &'a Path,
    program: &'a Path,
    updates: &'a mpsc::Sender<(usize, Chat)>,
) -> Pin<Box<impl Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let folder = connection.welcome.folder.clone();
        let ended = tokio::time::timeout(
            wisp_start::start::SIGNAL_LIMIT + ANSWERS_WITHIN,
            connection.end(false),
        )
        .await;
        chat.state = if matches!(ended, Ok(Ok(_))) {
            let waiting = {
                let data = data.to_owned();
                let id = chat.kept.id.clone();
                tokio::task::spawn_blocking(move || older_waiting(&data, &id))
                    .await
                    .unwrap_or_default()
            };
            crate::host::start_again(
                program,
                data,
                &[],
                &folder,
                &chat.kept.id,
                &Start::default(),
                &wisp_start::keychain::KeychainInputs::load(false, None, false, None)
                    .redactions(None),
            )
            .await;
            match keep::dial(&link::socket(data, &chat.kept.id)).await {
                Ok(next) => {
                    chat.kept.older = next.welcome.build < crate::host::build();
                    State::Restarted(waiting)
                }
                Err(reason) => lost(data, &chat.kept.id, reason),
            }
        } else {
            match ended {
                Ok(Err(error)) => lost(data, &chat.kept.id, error.to_string()),
                _ => State::NoAnswer,
            }
        };
        let _ = updates.send((index, chat)).await;
    })
}

fn older_waiting(data: &Path, id: &str) -> Vec<Unready> {
    use wisp_core::session::SessionStore;
    use wisp_protocol::log::LogEvent;
    let waiting = keep::store(data).ok().and_then(|store| store.read(id).ok()).and_then(|records| {
        let interrupted = records.iter().rev().find_map(|record| if let LogEvent::SessionEnded { interrupted, .. } = record.event { Some(interrupted) } else { None }).flatten();
        let logged = wisp_core::resume::LoggedSession::read(&records).ok()?;
        logged.last_turn.filter(|turn| Some(turn.turn) == interrupted && !turn.replied && records.iter().any(|record| matches!(&record.event, LogEvent::TurnStarted { turn: started, origin: None, .. } if *started == turn.turn)))
    }).is_some();
    if waiting {
        vec![Unready {
            agent: wisp_protocol::AgentPath::root(),
            what: wisp_protocol::Unfinished::Reply,
            title: None,
            target: String::new(),
            running_ms: 0,
            opening: true,
        }]
    } else {
        Vec::new()
    }
}

fn lines(rows: &[(Chat, Instant)]) -> Vec<Chat> {
    rows.iter()
        .map(|(chat, at)| {
            let mut chat = chat.clone();
            if let State::Finishing(open) = &mut chat.state {
                wisp_protocol::age_unready(open, at.elapsed());
            }
            chat
        })
        .collect()
}

fn keys(prompter: Prompter) -> (watch::Sender<bool>, tokio::task::JoinHandle<Option<bool>>) {
    let (on, active) = watch::channel(true);
    let task = tokio::task::spawn_blocking(move || {
        let watch = prompter.watch_enter().ok()?;
        while *active.borrow() {
            match watch.poll(Duration::from_millis(100)) {
                Ok(true) => return Some(true),
                Err(PromptError::Cancelled) => return Some(false),
                Err(_) => return None,
                Ok(false) => {}
            }
        }
        None
    });
    (on, task)
}

fn draw(
    lines: &[StyledLine],
    terminal: bool,
    caps: &caps::Caps,
    layout: Layout,
    drawn: &mut usize,
    previous: &mut Vec<StyledLine>,
) {
    if lines == previous {
        return;
    }
    let mut bytes = Vec::new();
    if terminal {
        if *drawn > 0 {
            bytes.extend_from_slice(format!("\x1b[{}A\r\x1b[J", *drawn).as_bytes());
        }
        for byte in print::styled(lines, caps, print::Stream::Stdout, layout.width) {
            if byte == b'\n' {
                bytes.push(b'\r');
            }
            bytes.push(byte);
        }
        *drawn = lines.len();
    } else {
        let changed = lines
            .iter()
            .enumerate()
            .filter(|(index, line)| previous.get(*index) != Some(*line))
            .map(|(_, line)| line.clone())
            .collect::<Vec<_>>();
        bytes.extend(print::styled(
            &changed,
            caps,
            print::Stream::Stdout,
            layout.width,
        ));
    }
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(&bytes);
    let _ = stdout.flush();
    *previous = lines.to_vec();
}
