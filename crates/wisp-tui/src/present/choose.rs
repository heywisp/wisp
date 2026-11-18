use crate::term::caps::{Env, Streams};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requested {
    Auto,
    Tui,
    Jsonl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presenter {
    Tui,
    Jsonl,
}

impl Presenter {
    pub const fn name(self) -> &'static str {
        match self {
            Presenter::Tui => "tui",
            Presenter::Jsonl => "jsonl",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    pub presenter: Presenter,
    pub reason: String,
}

impl Chosen {
    fn new(presenter: Presenter, reason: impl Into<String>) -> Chosen {
        Chosen {
            presenter,
            reason: reason.into(),
        }
    }

    pub fn describe(&self) -> String {
        format!("{} ({})", self.presenter.name(), self.reason)
    }
}

pub const NEEDS_TERMINAL: &str =
    "the full screen needs a terminal; use --output jsonl for scripts and pipes";

pub const DUMB_TERMINAL: &str = "the full screen can't be drawn on a dumb terminal (TERM=dumb); --output tui forces it, or use --output jsonl for scripts and pipes";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    NotATerminal(&'static str),
    Dumb,
}

pub fn blocked(env: &Env, streams: Streams) -> Option<Blocked> {
    if !streams.stdin_tty {
        Some(Blocked::NotATerminal("stdin is not a terminal"))
    } else if !streams.stdout_tty {
        Some(Blocked::NotATerminal("stdout is not a terminal"))
    } else if env.dumb() {
        Some(Blocked::Dumb)
    } else {
        None
    }
}

pub fn resolve(requested: Requested, env: &Env, streams: Streams) -> Result<Chosen, String> {
    match (requested, blocked(env, streams)) {
        (Requested::Jsonl, _) => Ok(Chosen::new(Presenter::Jsonl, "--output jsonl")),
        (Requested::Auto, None) => Ok(Chosen::new(
            Presenter::Tui,
            "stdin and stdout are terminals",
        )),
        (Requested::Auto, Some(Blocked::NotATerminal(_))) => Err(NEEDS_TERMINAL.to_owned()),
        (Requested::Auto, Some(Blocked::Dumb)) => Err(DUMB_TERMINAL.to_owned()),
        (Requested::Tui, None) => Ok(Chosen::new(Presenter::Tui, "--output tui")),
        (Requested::Tui, Some(Blocked::Dumb)) => Ok(Chosen::new(
            Presenter::Tui,
            "--output tui, forced on TERM=dumb",
        )),
        (Requested::Tui, Some(Blocked::NotATerminal(why))) => Err(format!(
            "--output tui needs a terminal, but {why}; use --output jsonl for scripts and pipes"
        )),
    }
}

#[cfg(test)]
mod tests;
