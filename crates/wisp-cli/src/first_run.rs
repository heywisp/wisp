use std::io::IsTerminal;
use std::process::ExitCode;

use wisp_core::endpoints::ConfigFile;
use wisp_tui::present::choose::Presenter;
use wisp_tui::term::prompt::CANCEL_EXIT;

use wisp_tui::app::endpoint_card::CardSetup;

use crate::endpoints::{Flags, Sources};
use crate::manage::import::ImportRun;
use crate::manage::{self, Asked, Context, SETUP};
use crate::output::{self, OutputMode};
use crate::presence::{Place, Presence};

pub(crate) const STORE_HINT: &str = "Or store a key: run \"wisp setup\" in a terminal, or printenv VAR | wisp account add NAME --key-stdin";

const WELCOME: &str = "Welcome to Wisp. No model is set up yet, so let's add one.";

const NEEDS_TERMINAL: &str = "wisp setup asks its questions on a terminal; without one, store a key with: printenv VAR | wisp account add NAME --key-stdin";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Output {
    FullScreen,
    Jsonl,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    pub(crate) nothing_found: bool,
    pub(crate) nothing_left: bool,
    pub(crate) given_source: bool,
    pub(crate) chosen: bool,
    pub(crate) terminals: bool,
    pub(crate) output: Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opening {
    Proceed,
    Setup,
    Hint,
}

pub(crate) fn opening(facts: Facts) -> Opening {
    if !(facts.nothing_found || facts.nothing_left)
        || facts.chosen
        || facts.output == Output::Refused
    {
        return Opening::Proceed;
    }
    if !facts.given_source && facts.terminals && facts.output == Output::FullScreen {
        Opening::Setup
    } else if facts.nothing_found {
        Opening::Hint
    } else {
        Opening::Proceed
    }
}

pub(crate) fn unconfigured(config: Option<&ConfigFile>) -> bool {
    config.is_none_or(|c| c.endpoints.is_empty() && c.default.is_none())
}

pub(crate) fn nothing_found(sources: &Sources) -> Option<bool> {
    let entries = sources.entries().ok()?;
    let config = sources.config.as_ref().ok()?;
    Some(unconfigured(config.as_ref()) && entries.is_empty())
}

fn terminals() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
}

pub(crate) enum Start {
    Session,
    Done(ExitCode),
    Card(CardSetup),
}

pub(crate) fn start(ctx: &Context<'_>, endpoint: Option<&str>, mode: OutputMode) -> Start {
    let flags = Flags {
        env_file: ctx.env_file,
        config: ctx.config,
        endpoint,
        keychain: ctx.keychain,
        set: &[],
    };
    let sources = Sources::load(&flags, ctx.xdg_config_home.clone(), ctx.home.clone());
    let Some(nothing_found) = nothing_found(&sources) else {
        return Start::Session;
    };
    let output = match output::choose(mode) {
        Ok(chosen) => match chosen.presenter {
            Presenter::Tui => Output::FullScreen,
            Presenter::Jsonl => Output::Jsonl,
        },
        Err(_) => Output::Refused,
    };
    let lead = crate::resume::nothing_left(&sources);
    let facts = Facts {
        nothing_found,
        nothing_left: lead.is_some(),
        given_source: ctx.config.is_some() || ctx.env_file.is_some(),
        chosen: sources.choice.is_some(),
        terminals: terminals(),
        output,
    };
    match opening(facts) {
        Opening::Proceed => Start::Session,
        Opening::Hint => {
            let Some(error) = sources.resolve().err() else {
                return Start::Session;
            };
            eprintln!("{}", sources.start_failure(&error));
            if !sources.inputs.available() {
                eprintln!("{STORE_HINT}");
            }
            Start::Done(ExitCode::FAILURE)
        }
        Opening::Setup => {
            let config = sources.config.as_ref().ok().and_then(Option::as_ref);
            let presence = Presence::scan(&sources.inputs, config);
            if sources.inputs.available() && presence.any_importable() {
                if lead.is_none() {
                    eprintln!("{WELCOME_UNSET}");
                    eprintln!();
                }
                match offer(ctx, &presence, &sources.file_words()) {
                    Offered::Stored => return Start::Session,
                    Offered::Stop(code) => return Start::Done(code),
                    Offered::Declined => {}
                }
            }
            Start::Card(CardSetup {
                lead,
                ..crate::card::setup(&sources, ctx)
            })
        }
    }
}

fn offer_on_command(ctx: &Context<'_>) -> Option<ExitCode> {
    let flags = Flags {
        env_file: None,
        config: ctx.config,
        endpoint: None,
        keychain: ctx.keychain,
        set: &[],
    };
    let sources = Sources::load(&flags, ctx.xdg_config_home.clone(), ctx.home.clone());
    if !sources.inputs.available() {
        return None;
    }
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    let presence = Presence::scan(&sources.inputs, config);
    if !presence.any_importable() {
        return None;
    }
    if unconfigured(config) {
        eprintln!("{WELCOME_UNSET}");
        eprintln!();
    }
    match offer(ctx, &presence, &sources.file_words()) {
        Offered::Stored => Some(ExitCode::SUCCESS),
        Offered::Stop(code) => Some(code),
        Offered::Declined => None,
    }
}

const WELCOME_UNSET: &str = "Welcome to Wisp. No account is set up yet.";

const OFFER_LEAD: &str = "This machine has a keychain, so Wisp keeps keys there and doesn't read them\nfrom environment variables or keys files. It found:";

pub(crate) enum Offered {
    Stored,
    Declined,
    Stop(ExitCode),
}

pub(crate) fn offer(ctx: &Context<'_>, presence: &Presence, file_words: &str) -> Offered {
    eprintln!("{OFFER_LEAD}");
    for line in presence.offer_lines(file_words) {
        eprintln!("  {line}");
    }
    let prompter = ctx.prompter(SETUP.cancel());
    match manage::confirm(
        &prompter,
        "Store these in the keychain? Each is checked first.",
        true,
    ) {
        Asked::Answer(true) => {}
        Asked::Answer(false) | Asked::Declined => return Offered::Declined,
        Asked::Cancelled => return Offered::Stop(manage::cancelled(&prompter)),
        Asked::Failed(e) => return Offered::Stop(manage::fail(e)),
    }
    let mut run = ImportRun::across_places();
    let importable = |place| presence.importable(place);
    let mut pass = |place, run: &mut ImportRun| manage::import::offer(ctx, place, run);
    if let Some((code, place)) = offer_passes(&mut run, &importable, &mut pass) {
        if code != ExitCode::from(CANCEL_EXIT) {
            eprintln!("{}", stopped_words(place, &importable, file_words));
        }
        return Offered::Stop(code);
    }
    if !run.stored_any() {
        Offered::Declined
    } else {
        Offered::Stored
    }
}

const PLACES: [Place; 2] = [Place::Environment, Place::File];

pub(crate) fn offer_passes(
    run: &mut ImportRun,
    importable: &dyn Fn(Place) -> bool,
    pass: &mut dyn FnMut(Place, &mut ImportRun) -> ExitCode,
) -> Option<(ExitCode, Place)> {
    for place in PLACES {
        if !importable(place) {
            continue;
        }
        let code = pass(place, run);
        if code == ExitCode::from(CANCEL_EXIT) || run.stopped() {
            return Some((code, place));
        }
    }
    None
}

pub(crate) fn stopped_words(
    stopped_in: Place,
    importable: &dyn Fn(Place) -> bool,
    file_words: &str,
) -> String {
    let mut left = Vec::new();
    let mut after = false;
    for place in PLACES {
        let rest = place == stopped_in;
        if !(rest || (after && importable(place))) {
            continue;
        }
        after |= rest;
        left.push(match (place, rest) {
            (Place::Environment, true) => "the rest of your environment's keys".to_owned(),
            (Place::Environment, false) => "the keys in your environment".to_owned(),
            (Place::File, true) => format!("the rest of the keys in {file_words}"),
            (Place::File, false) => format!("the keys in {file_words}"),
        });
    }
    format!(
        "Setup stopped: {} weren't imported. Run wisp setup to try again.",
        wisp_core::secrets::import::and_words(&left)
    )
}

pub(crate) fn command(ctx: &Context<'_>) -> ExitCode {
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        eprintln!("wisp: {NEEDS_TERMINAL}");
        return ExitCode::FAILURE;
    }
    let fresh = match ctx.target(false) {
        Ok((path, _)) => ctx.config.is_some() && std::fs::symlink_metadata(path).is_err(),
        Err(e) => {
            eprintln!("wisp: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(code) = offer_on_command(ctx) {
        return code;
    }
    let accounts = if fresh {
        Ok(Default::default())
    } else {
        crate::auth::accounts::gather(ctx)
    };
    let accounts = match accounts {
        Ok(accounts) => accounts,
        Err(e) => {
            eprintln!("wisp: {e}");
            return ExitCode::FAILURE;
        }
    };
    let first = !ctx
        .sources()
        .config
        .ok()
        .flatten()
        .is_some_and(|config| config.default.is_some());
    if accounts.list.is_empty() {
        eprintln!("{WELCOME}");
        eprintln!();
        return manage::add_asking(ctx, first, &SETUP);
    }
    let stream = wisp_tui::term::print::Stream::Stderr;
    if let Err(e) = crate::auth::accounts::show(ctx, &accounts, None, stream) {
        eprintln!("wisp: {e}");
        return ExitCode::FAILURE;
    }
    let prompter = ctx.prompter(SETUP.cancel());
    match manage::confirm(&prompter, "Add another?", false) {
        Asked::Answer(true) => {}
        Asked::Answer(false) | Asked::Declined => {
            eprintln!("Nothing was changed.");
            return ExitCode::SUCCESS;
        }
        Asked::Cancelled => return manage::cancelled(&prompter),
        Asked::Failed(e) => return manage::fail(e),
    }
    manage::add_asking(ctx, first, &SETUP)
}

#[cfg(test)]
mod tests;
