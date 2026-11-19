use std::process::ExitCode;
use std::sync::Arc;

use wisp_protocol::RequestId;
use wisp_protocol::subscription_auth::{AuthCommand, AuthOutcome};
use wisp_render::chrome::account::{
    self, AccountState, Asked, CLEANUP_WARNING, Ending, NO_ACCOUNT, NOTHING_SIGNED_OUT, Start,
    Surface,
};
use wisp_render::styled::StyledLine;
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::copy::{listed, listed_or};
use wisp_tui::term::print::Stream;

use super::accounts::{Account, Accounts, Health, Held};
use super::login::{Auth, Front, Row, Stop, Terminal};
use crate::manage::Context;

pub(crate) const ALL: &str = "all";

fn keys_file(account: &Account) -> bool {
    account.health == Health::SignedIn
        && matches!(
            account.held,
            Held::Variable {
                keys_file: true,
                ..
            }
        )
}

fn billed(accounts: &Accounts, name: &str) -> AccountState {
    accounts
        .get(name)
        .map_or(AccountState::Subscription, |a| a.kind.billing())
}

pub(crate) fn rows(accounts: &Accounts, glyphs: &Glyphs) -> Vec<Row> {
    let mut rows: Vec<Row> = accounts
        .list
        .iter()
        .map(|account| Row {
            word: account.name.clone(),
            mark: account.mark,
            label: account.name.clone(),
            detail: [
                Some(account::kind_words(account.kind, glyphs)),
                account::row_notes(
                    false,
                    accounts.default.as_deref() == Some(account.name.as_str()),
                    keys_file(account).then_some(accounts.keys_file.as_str()),
                    &[],
                )
                .into_iter()
                .next(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(&format!(" {} ", glyphs.separator)),
            keep_detail: false,
        })
        .collect();
    rows.push(Row {
        word: ALL.to_owned(),
        mark: None,
        label: ALL.to_owned(),
        detail: String::new(),
        keep_detail: false,
    });
    rows
}

pub(crate) struct Answer {
    pub(crate) yes: bool,
    pub(crate) terminal: bool,
}

fn ask(
    front: &mut dyn Front,
    answer: &Answer,
    names: &[String],
    lines: Vec<String>,
    keys: &[String],
) -> Result<(), Stop> {
    if answer.yes {
        return Ok(());
    }
    let named = listed(names);
    if !answer.terminal {
        return Err(Stop::Failed(match keys {
            [] => format!("signing out of {named} needs a yes; add --yes to confirm."),
            [_] if names.len() == 1 => {
                format!("signing out of {named} deletes its key; add --yes to confirm.")
            }
            _ => format!(
                "signing out of {named} deletes the keys of {}; add --yes to confirm.",
                listed(keys)
            ),
        }));
    }
    if front.confirm(&lines.join("\n"))? {
        Ok(())
    } else {
        Err(Stop::Cancelled)
    }
}

pub(crate) async fn sign_out<A: Auth>(
    auth: &A,
    front: &mut dyn Front,
    accounts: &Accounts,
    name: Option<&str>,
    answer: &Answer,
    glyphs: &Glyphs,
    width: usize,
) -> Result<AuthOutcome, Stop> {
    let word = match name {
        Some(name) => name.to_owned(),
        None if !answer.terminal => {
            let names: Vec<&str> = accounts.list.iter().map(|a| a.name.as_str()).collect();
            let said = match names.as_slice() {
                [] => NO_ACCOUNT.to_owned(),
                [one] => format!("say which: {one}."),
                many => format!("say which: {}.", listed_or(many)),
            };
            return Err(Stop::Failed(said));
        }
        None => {
            let rows = rows(accounts, glyphs);
            let picked = front.choose(&rows)?;
            rows.get(picked).map(|r| r.word.clone()).unwrap_or_default()
        }
    };
    let all = word == ALL;
    let names: Vec<String> = if all {
        accounts
            .list
            .iter()
            .filter(|a| !keys_file(a))
            .map(|a| a.name.clone())
            .collect()
    } else {
        vec![word]
    };
    if names.is_empty() {
        let line = accounts
            .list
            .iter()
            .find_map(|a| accounts.variable_line(&a.name, Ending::SignOut));
        return Err(Stop::Failed(line.unwrap_or_else(|| NO_ACCOUNT.to_owned())));
    }
    let keys = if all && names.len() == 1 {
        let stored = |name: &&String| accounts.get(name).is_some_and(|a| a.held == Held::Stored);
        names.iter().filter(stored).cloned().collect()
    } else {
        match auth
            .send(AuthCommand::logout(
                RequestId(1),
                &names,
                false,
                None,
                Vec::new(),
            ))
            .await
            .outcome
        {
            AuthOutcome::LogoutNeeds {
                deletes_keys_of, ..
            } => deletes_keys_of,
            first => return Ok(first),
        }
    };
    let named = |name: &String| (name.clone(), accounts.get(name).and_then(|a| a.mark));
    let asked = Asked {
        names: names.iter().map(named).collect(),
        keys: keys.iter().map(named).collect(),
        ..Asked::default()
    };
    let (title, asked) = asked.titled(width, glyphs);
    let title = account::unmarked(&StyledLine::new(title));
    let lines = account::logout_question(&asked, width, glyphs);
    let lines = std::iter::once(title)
        .chain(lines.iter().map(account::unmarked))
        .collect();
    ask(front, answer, &names, lines, &keys)?;
    Ok(auth
        .send(AuthCommand::logout(
            RequestId(2),
            &names,
            true,
            None,
            Vec::new(),
        ))
        .await
        .outcome)
}

pub(crate) fn result_lines(
    outcome: &AuthOutcome,
    accounts: &Accounts,
    glyphs: &Glyphs,
    width: usize,
) -> Result<(Vec<StyledLine>, Option<String>), Stop> {
    let AuthOutcome::LoggedOut {
        alias,
        also,
        left_sign_ins,
        left_keys,
        new_start,
        stopped,
        ..
    } = outcome
    else {
        return Err(match outcome {
            AuthOutcome::Failed { message, .. } => Stop::Failed(message.clone()),
            _ => Stop::Failed("nothing was signed out".to_owned()),
        });
    };
    let gone: Vec<(account::AccountKind, String)> = std::iter::once(alias)
        .chain(also)
        .map(|name| {
            let kind = accounts
                .get(name)
                .map_or(account::AccountKind::ClaudeSubscription, |a| a.kind);
            (kind, name.clone())
        })
        .collect();
    let wrap = |line: &str, mark| super::login::marked_lines(line, mark, width, glyphs);
    let mut lines: Vec<StyledLine> =
        account::signed_out_lines(&gone, Surface::Terminal, glyphs, width)
            .iter()
            .map(|line| account::mark_line(line, Token::Text, None, glyphs))
            .collect();
    lines.extend(
        left_sign_ins
            .iter()
            .flat_map(|_| wrap(CLEANUP_WARNING, None)),
    );
    lines.extend(
        left_keys
            .iter()
            .flat_map(|name| wrap(&account::key_left(name), None)),
    );
    let left = accounts
        .list
        .iter()
        .any(|a| !gone.iter().any(|(_, name)| *name == a.name));
    match new_start {
        Some(name) => {
            let state = billed(accounts, name);
            let mark = accounts.get(name).and_then(|a| a.mark);
            let line = account::start_line(name, mark, state, Start::Now, glyphs);
            lines.extend(wrap(&line, mark));
        }
        None if !left => lines.extend(wrap(&account::sign_in_again(Surface::Terminal), None)),
        None => {}
    }
    Ok((lines, stopped.clone()))
}

pub(crate) fn logout(
    ctx: &Context<'_>,
    keychain: Arc<crate::keychain::Keychain>,
    name: Option<&str>,
    yes: bool,
) -> ExitCode {
    use std::io::IsTerminal;
    let fail = |stop: Stop| match stop {
        Stop::Cancelled => {
            eprintln!("{NOTHING_SIGNED_OUT}");
            ExitCode::from(wisp_tui::term::prompt::CANCEL_EXIT)
        }
        Stop::Failed(message) => {
            eprintln!("wisp: {message}");
            ExitCode::FAILURE
        }
        Stop::Said | Stop::SameAccount(_) => ExitCode::FAILURE,
    };
    let accounts = match super::accounts::gather(ctx) {
        Ok(accounts) => accounts,
        Err(error) => return fail(Stop::Failed(error)),
    };
    let answer = Answer {
        yes,
        terminal: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
    };
    let mut tty = Terminal::new(ctx.prompter(NOTHING_SIGNED_OUT), false);
    let glyphs = tty.glyphs();
    let host = crate::manage::host::CliEndpointHost::of(
        ctx,
        None,
        keychain,
        wisp_core::secrets::keyring::Budget::Fresh(wisp_core::secrets::keyring::TERMINAL_DEADLINE),
    );
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return fail(Stop::Failed("could not start signing out".to_owned()));
    };
    let width = wisp_tui::term::prompt::terminal_columns();
    let asked = sign_out(&host, &mut tty, &accounts, name, &answer, &glyphs, width);
    let outcome = runtime.block_on(asked);
    match outcome.and_then(|outcome| result_lines(&outcome, &accounts, &glyphs, width)) {
        Ok((lines, stopped)) => {
            let _ = super::login::print_lines(&lines, &ctx.caps, Stream::Stdout, 0);
            stopped.map_or(ExitCode::SUCCESS, |stopped| fail(Stop::Failed(stopped)))
        }
        Err(stop) => fail(stop),
    }
}
