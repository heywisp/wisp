use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::caps::Env;

pub const WSL_ROOT_ENV: &str = "WISP_WSL_ROOT";

pub fn wsl_root() -> PathBuf {
    std::env::var_os(WSL_ROOT_ENV).map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

pub const TMUX_PATIENCE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TmuxMouse {
    On,
    Off,
    Unknown,
}

pub fn tmux_mouse_of(output: &str) -> TmuxMouse {
    match output.trim() {
        "on" => TmuxMouse::On,
        "off" => TmuxMouse::Off,
        _ => TmuxMouse::Unknown,
    }
}

pub fn tmux_mouse(env: &Env) -> Option<TmuxMouse> {
    env.tmux()
        .then(|| ask_tmux().map_or(TmuxMouse::Unknown, |out| tmux_mouse_of(&out)))
}

fn ask_tmux() -> Option<String> {
    let mut child = Command::new("tmux")
        .args(["show", "-gv", "mouse"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + TMUX_PATIENCE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                drop(child.kill());
                drop(child.wait());
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_answers_are_read_strictly() {
        assert_eq!(tmux_mouse_of("on\n"), TmuxMouse::On);
        assert_eq!(tmux_mouse_of("off"), TmuxMouse::Off);
        assert_eq!(tmux_mouse_of("maybe"), TmuxMouse::Unknown);
        assert_eq!(tmux_mouse(&Env::from_pairs([("TERM", "xterm")])), None);
    }
}
