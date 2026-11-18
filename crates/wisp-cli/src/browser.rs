use std::process::{Command, Stdio};

use tokio::sync::mpsc;
use wisp_protocol::AgentEvent;
use wisp_tui::term::caps::Env;

fn from_browser(browser: &str, url: &str) -> Option<Vec<String>> {
    let first = browser.split(':').next()?.trim();
    let mut words: Vec<String> = first.split_whitespace().map(str::to_owned).collect();
    let program = words.first()?;
    if program.rsplit('/').next() == Some("www-browser") {
        return None;
    }
    if words.iter().any(|word| word.contains("%s")) {
        for word in &mut words {
            *word = word.replace("%s", url);
        }
    } else {
        words.push(url.to_owned());
    }
    Some(words)
}

pub(crate) fn candidates(
    env: &Env,
    live: &dyn Fn(&str) -> bool,
    macos: bool,
    url: &str,
) -> Vec<Vec<String>> {
    let display = env.set("DISPLAY").or(env.set("WAYLAND_DISPLAY")).is_some();
    let remote = env.ssh() && !display;
    if env.set("CI").is_some() || (remote && !env.set("VSCODE_IPC_HOOK_CLI").is_some_and(live)) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(command) = env.set("BROWSER").and_then(|b| from_browser(b, url)) {
        out.push(command);
    }
    if remote {
        return out;
    }
    let words = |list: &[&str]| -> Vec<String> {
        list.iter()
            .map(|word| (*word).to_owned())
            .chain([url.to_owned()])
            .collect()
    };
    if env
        .set("WSL_DISTRO_NAME")
        .or(env.set("WSL_INTEROP"))
        .is_some()
    {
        out.push(words(&["wslview"]));
        out.push(words(&["rundll32.exe", "url.dll,FileProtocolHandler"]));
    } else if macos {
        out.push(words(&["open"]));
    } else if display {
        out.push(words(&["xdg-open"]));
    }
    out
}

pub(crate) fn open_with(
    env: &Env,
    live: &dyn Fn(&str) -> bool,
    macos: bool,
    url: &str,
    allowed: fn(&str) -> bool,
    spawn: &mut dyn FnMut(&[String]) -> bool,
) -> bool {
    allowed(url)
        && candidates(env, live, macos, url)
            .iter()
            .any(|command| spawn(command))
}

fn spawn(command: &[String]) -> bool {
    let Some((program, args)) = command.split_first() else {
        return false;
    };
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(mut child) => {
            let reaped = std::thread::Builder::new()
                .name("wisp-browser".to_owned())
                .spawn(move || drop(child.wait()));
            drop(reaped);
            true
        }
        Err(_) => false,
    }
}

pub(crate) fn can_open_in(env: &Env, live: &dyn Fn(&str) -> bool, macos: bool) -> bool {
    !candidates(env, live, macos, "https://localhost").is_empty()
}

#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "unit tests never probe the host display; SessionAuth swaps can_open"
    )
)]
pub(crate) fn can_open() -> bool {
    can_open_in(
        &Env::current(),
        &|path| std::path::Path::new(path).exists(),
        cfg!(target_os = "macos"),
    )
}

#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "unit tests never open a browser; SessionAuth swaps the opener"
    )
)]
pub(crate) fn open(url: &str, allowed: fn(&str) -> bool) -> bool {
    open_with(
        &Env::current(),
        &|path| std::path::Path::new(path).exists(),
        cfg!(target_os = "macos"),
        url,
        allowed,
        &mut spawn,
    )
}

pub(crate) fn opening(
    mut events: mpsc::Receiver<AgentEvent>,
    open: crate::auth::Opener,
) -> mpsc::Receiver<AgentEvent> {
    let (send, receive) = mpsc::channel(1);
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                biased;
                _ = send.closed() => break,
                event = events.recv() => event,
            };
            let Some(mut event) = event else {
                break;
            };
            for note in event.links_mut() {
                note.opened = Some(open(note.link.expose()));
            }
            if send.send(event).await.is_err() {
                break;
            }
        }
    });
    receive
}

#[cfg(test)]
mod tests;
