use std::time::Instant;

use tokio::sync::mpsc;

use super::display::Kept;
use super::presenter::{AppError, Exit, Unsent};
use super::setup::{Opened, Opening, open};
use crate::term::caps::probe_plan;
use crate::term::clipboard::reader;
use crate::term::clipboard::{self, Clipboard, Host};
use crate::term::host;
use crate::term::print;
use crate::term::{
    Caps, EnterError, Entered, Env, Flags, Input, KeyboardFlags, LateReplies, Live, Modes, Reader,
    SavedDisplay, Streams, Terminal, TerminalOptions, death, dying,
};
use wisp_client::QuitBound;

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error(transparent)]
    Enter(#[from] EnterError),
    #[error(transparent)]
    App(#[from] AppError),
}

pub struct Launched {
    pub unsent: Option<Unsent>,
    pub exit: Result<Exit, LaunchError>,
    pub goodbye: Vec<u8>,
    pub warnings: Vec<String>,
    pub unshown: Vec<String>,
}

pub fn modes(caps: &Caps) -> Modes {
    Modes {
        raw: true,
        alternate_screen: true,
        keyboard: caps.kitty_keyboard.then_some(KeyboardFlags::DISAMBIGUATE),
        bracketed_paste: true,
        focus_reports: true,
        cursor_hidden: true,
        mouse: caps.mouse.value,
        hover: false,
        alternate_scroll_off: Some(caps.alternate_scroll.value),
    }
}

pub fn verb_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher, RandomState};
    RandomState::new().build_hasher().finish()
}

fn unshown(opening: &Opening) -> Vec<String> {
    match opening {
        Opening::Session { notes, .. } => notes.clone(),
        Opening::FirstRun { .. } | Opening::Choose { .. } | Opening::Resume { .. } => Vec::new(),
    }
}

fn ended(exit: Result<Exit, LaunchError>, notes: Vec<String>) -> Launched {
    Launched {
        unsent: None,
        exit,
        goodbye: Vec::new(),
        warnings: Vec::new(),
        unshown: notes,
    }
}

pub async fn launch(
    opening: Opening,
    flags: Flags,
    saved: &SavedDisplay,
    verbose: bool,
    quit: Option<QuitBound>,
    unsent: Option<Unsent>,
) -> Launched {
    if dying().is_some() {
        return ended(Ok(Exit::Dying), vec![]);
    }
    let notes = unshown(&opening);
    let env = Env::current();
    let streams = Streams::current();
    let (send, inputs) = mpsc::unbounded_channel::<Input>();
    let resumed = send.clone();
    let restored = send.clone();
    let options = TerminalOptions {
        on_resume: Some(Box::new(move || drop(resumed.send(Input::Resumed)))),
        on_restore: Some(Box::new(move || drop(restored.send(Input::Restored)))),
        probe: probe_plan(&env, streams, flags).ok(),
    };
    let tmux = host::tmux_mouse(&env);
    let Entered { terminal, probe } = match Terminal::enter(options) {
        Ok(entered) => entered,
        Err(e) => return ended(Err(e.into()), notes),
    };
    let typed = probe.as_ref().map(|r| r.typed.clone()).unwrap_or_default();
    let late = probe.as_ref().map_or_else(LateReplies::default, |r| {
        LateReplies::after(r, Instant::now())
    });
    let host = Host::current(&env);
    let kept = Kept {
        env,
        streams,
        flags,
        saved: saved.clone(),
        probe,
        host,
        tmux,
    };
    let caps = kept.resolve();
    let mut console = match Live::new(terminal, modes(&caps)) {
        Ok(console) => console,
        Err(e) => return ended(Err(AppError::from(e).into()), notes),
    };
    let reader = match Reader::spawn(late, move |input| send.send(input).is_ok()) {
        Ok(reader) => reader,
        Err(e) => {
            let mut terminal = console.close();
            drop(terminal.restore());
            return ended(Err(AppError::from(e).into()), notes);
        }
    };
    console.listen(reader);
    let mut options = kept.options(&caps);
    options.typed = typed;
    options.verbose = verbose;
    options.clipboard = Clipboard::current(&kept.host);
    options.images = reader::Setup::current(&kept.host);
    options.select_key = clipboard::select_key(&kept.host);
    options.right_paste = clipboard::right_click_pastes(&kept.host);
    options.terminal_pastes_on_right_click = clipboard::terminal_pastes_on_right_click(&kept.host);
    options.verb_seed = verb_seed();
    options.quit = quit;
    options.unsent = unsent;
    options.list_agents = true;
    let opened = tokio::select! {
        opened = open(&mut console, inputs, opening, options) => Some(opened),
        _ = death() => None,
    };
    let Some(Opened {
        unsent,
        exit,
        goodbye,
        width,
        theme,
        warnings,
    }) = opened.filter(|opened| !matches!(opened.exit, Ok(Exit::Dying)))
    else {
        console.terminal().await_death();
        drop(console.close());
        return ended(Ok(Exit::Dying), vec![]);
    };
    let mut terminal = console.close();
    drop(terminal.restore());
    Launched {
        unsent,
        exit: exit.map_err(LaunchError::from),
        goodbye: print::styled(
            &goodbye,
            &caps.with_theme(theme),
            print::Stream::Stdout,
            width,
        ),
        warnings,
        unshown: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::{mpsc, oneshot};
    use wisp_protocol::CacheTtl;
    use wisp_render::chrome::Endpoint;

    use super::unshown;
    use crate::app::Session;
    use crate::app::endpoint_card::CardSetup;
    use crate::app::fixture::facts;
    use crate::app::setup::{Opening, SetupChannels};

    #[test]
    fn a_session_opening_hands_back_its_notes_and_a_first_run_none() {
        let (commands, _) = mpsc::channel(1);
        let (_, events) = mpsc::channel(1);
        let session = Opening::Session {
            session: Box::new(Session {
                leave_note: None,
                attached: None,
                alerts: Vec::new(),
                facts: facts(Endpoint::Official, CacheTtl::OneHour),
                events,
                commands,
                resumed: None,
            }),
            card: CardSetup::default(),
            notes: vec!["a start warning".into(), "another".into()],
            started: None,
        };
        assert_eq!(unshown(&session), ["a start warning", "another"]);
        let (commands, _) = mpsc::channel(1);
        let (_, events) = mpsc::channel(1);
        let (_, start) = oneshot::channel();
        let first = Opening::FirstRun {
            setup: SetupChannels { events, commands },
            card: CardSetup::default(),
            start,
        };
        assert!(unshown(&first).is_empty());
    }
}
