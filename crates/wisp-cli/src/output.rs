use std::io::{BufRead, Write};

use clap::ValueEnum;
use std::process::ExitCode;
use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ClientCommand, InputId, Opening, SessionView};

use wisp_client::{AGENT_STOPPED, Channels, QuitBound, drain_within, interrupts, turn};
pub(crate) use wisp_start::start::{FORCED_EXIT, SAVE_CUT};
use wisp_tui::present::choose::{self, Chosen, Presenter, Requested};
use wisp_tui::term::{Env, Streams, death};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputMode {
    #[default]
    #[value(
        help = "The tui when stdin and stdout are terminals and TERM isn't dumb; otherwise Wisp exits with an error"
    )]
    Auto,
    #[value(help = "The tui, even when TERM is dumb; fails when not on a terminal")]
    Tui,
    #[value(
        help = "One protocol event per line as JSON, one message per input line, for scripts and pipes. Unstable until M6"
    )]
    Jsonl,
}

impl OutputMode {
    pub(crate) const fn requested(self) -> Requested {
        match self {
            OutputMode::Auto => Requested::Auto,
            OutputMode::Tui => Requested::Tui,
            OutputMode::Jsonl => Requested::Jsonl,
        }
    }
}

pub(crate) fn choose(mode: OutputMode) -> Result<Chosen, String> {
    choose::resolve(mode.requested(), &Env::current(), Streams::current())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Presentation {
    pub(crate) presenter: Presenter,
    pub(crate) verbose: bool,
    pub(crate) tui_verbose: bool,
    pub(crate) display: wisp_tui::term::SavedDisplay,
}

pub(crate) fn jsonl_warnings(p: &Presentation) -> Vec<String> {
    if p.verbose {
        vec!["wisp: --verbose has no effect with --output jsonl".to_owned()]
    } else {
        vec![]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunEnd {
    pub(crate) code: u8,
    pub(crate) forced: bool,
    pub(crate) leave: bool,
    pub(crate) resume: Option<String>,
    pub(crate) cut: Option<Vec<u8>>,
}

impl RunEnd {
    pub(crate) const fn code(code: u8) -> RunEnd {
        RunEnd {
            code,
            forced: false,
            leave: false,
            resume: None,
            cut: None,
        }
    }

    pub(crate) const fn forced() -> RunEnd {
        RunEnd {
            code: FORCED_EXIT,
            forced: true,
            leave: false,
            resume: None,
            cut: None,
        }
    }

    pub(crate) fn exit_code(&self) -> ExitCode {
        ExitCode::from(self.code)
    }
}

pub(crate) async fn run(
    p: &Presentation,
    channels: Channels,
    opening: Opening,
    view: Option<SessionView>,
) -> RunEnd {
    match p.presenter {
        Presenter::Tui => crate::tui::run(opening, view, channels, &p.display, p.tui_verbose).await,
        Presenter::Jsonl => {
            let used = view.map_or(0, |view| {
                InputId::highest_numbered(JSONL_PREFIX, view.input_ids.iter().map(String::as_str))
            });
            let warnings = opening.start.map_or_else(Vec::new, |start| start.warnings);
            let warnings = warnings.into_iter().chain(jsonl_warnings(p));
            piped(warnings, jsonl(channels, used)).await
        }
    }
}

async fn piped(
    warnings: impl IntoIterator<Item = String>,
    run: impl Future<Output = Result<Drained, String>>,
) -> RunEnd {
    let mut err = std::io::stderr();
    for warning in warnings {
        drop(writeln!(err, "{}", wisp_render::text::sanitize(&warning)));
    }
    match run.await {
        Ok(Drained::Closed) => RunEnd::code(0),
        Ok(Drained::Forced) => RunEnd::forced(),
        Err(e) => {
            drop(writeln!(err, "wisp: {}", wisp_render::text::sanitize(&e)));
            RunEnd::code(1)
        }
    }
}

async fn drained(
    events: &mut mpsc::Receiver<AgentEvent>,
    quit: Option<QuitBound>,
    show: impl FnMut(&AgentEvent) -> Result<(), String>,
) -> Result<Drained, String> {
    Ok(if drain_within(events, quit, show).await? {
        Drained::Forced
    } else {
        Drained::Closed
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drained {
    Closed,
    Forced,
}

pub(crate) fn jsonl_line(event: &AgentEvent) -> Result<String, String> {
    if event.is_account_telemetry() || event.is_private_auth() {
        return Ok(String::new());
    }
    let Some(event) = event.clone().without_links() else {
        return Ok(String::new());
    };
    wisp_protocol::jsonl::line(&event).map_err(|e| format!("could not encode an event: {e}"))
}

fn write_event(event: &AgentEvent) -> Result<(), String> {
    let line = jsonl_line(event)?;
    let mut out = std::io::stdout().lock();
    out.write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| format!("could not write to stdout: {e}"))
}

pub(crate) fn read_lines(
    input: impl BufRead + Send + 'static,
) -> mpsc::Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel(1);
    std::thread::spawn(move || {
        for line in input.lines() {
            let line = line.map_err(|e| format!("could not read a line from stdin: {e}"));
            let failed = line.is_err();
            if tx.blocking_send(line).is_err() || failed {
                break;
            }
        }
    });
    rx
}

const JSONL_PREFIX: &str = "jsonl-";

async fn jsonl(channels: Channels, mut sent: u64) -> Result<Drained, String> {
    let Channels {
        commands,
        mut events,
        quit,
    } = channels;
    let mut lines = read_lines(std::io::BufReader::new(std::io::stdin()));
    let mut waiting = interrupts()?;
    let mut failure = None;
    loop {
        let line = tokio::select! {
            line = lines.recv() => line,
            _ = waiting.recv() => None,
            _ = death() => None,
        };
        let text = match line {
            None => break,
            Some(Err(e)) => {
                failure = Some(e);
                break;
            }
            Some(Ok(line)) => line,
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        sent = sent.saturating_add(1);
        let command = ClientCommand::queue(format!("{JSONL_PREFIX}{sent}"), text);
        let mut interrupt = interrupts()?;
        if commands.send(command).await.is_err() {
            return Err(AGENT_STOPPED.to_owned());
        }
        tokio::select! {
            ended = turn(&commands, &mut events, &mut interrupt, write_event) => ended?,
            _ = death() => break,
        }
        waiting = interrupts()?;
    }
    drop(commands);
    let ended = drained(&mut events, quit, write_event).await?;
    if let Some(failure) = failure {
        return Err(failure);
    }
    Ok(ended)
}

#[cfg(test)]
mod tests;
