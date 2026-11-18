use std::fmt::Write as _;
use std::io::{self, Write};
use std::time::Instant;

use wisp_protocol::settings::{MOTIONS, THEMES, word_of};
pub use wisp_render::text::clean;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Rgb, oklab_lightness};
use wisp_render::transcript::copy;

use super::caps::{
    self, Caps, Choice, ENV_KEYS, Env, Flags, MODES, MOTION_POLICIES, PRESENCE_ONLY, ProbeSkip,
    SavedDisplay, Streams,
};
use super::clipboard::{self, Clipboard, Host};
use super::host::{self, TmuxMouse};
use super::modes::ScrollBack;
use crate::present::choose::{self, Requested};

use super::guard::{Entered, Terminal, TerminalOptions};
use super::probe::{LateReplies, Outcome, ProbeReport, SyncSupport};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    Skipped(ProbeSkip),
    Failed(String),
    Ran(ProbeReport),
}

pub fn probe(env: &Env, streams: Streams, flags: Flags) -> ProbeStatus {
    match caps::probe_plan(env, streams, flags) {
        Err(skip) => ProbeStatus::Skipped(skip),
        Ok(plan) => {
            let options = TerminalOptions {
                probe: Some(plan),
                ..TerminalOptions::default()
            };
            match Terminal::enter(options) {
                Err(e) => ProbeStatus::Failed(e.to_string()),
                Ok(Entered {
                    mut terminal,
                    probe,
                }) => {
                    let mut probe = probe;
                    let drained = match &mut probe {
                        Some(report) => terminal
                            .drain_late_replies(&mut LateReplies::after(report, Instant::now()))
                            .map(|late| report.typed.extend(late)),
                        None => Ok(()),
                    };
                    let failures = terminal.restore();
                    drop(terminal);
                    match (probe, drained, failures.first()) {
                        (None, _, _) => ProbeStatus::Failed("the probe did not run".to_owned()),
                        (_, Err(e), _) => {
                            ProbeStatus::Failed(format!("draining late replies: {e}"))
                        }
                        (_, _, Some(f)) => ProbeStatus::Failed(format!("restore failed: {f}")),
                        (Some(report), Ok(()), None) => ProbeStatus::Ran(report),
                    }
                }
            }
        }
    }
}

impl ProbeStatus {
    pub fn report(&self) -> Option<&ProbeReport> {
        match self {
            ProbeStatus::Ran(report) => Some(report),
            ProbeStatus::Skipped(_) | ProbeStatus::Failed(_) => None,
        }
    }
}

pub fn run(out: &mut impl Write, saved: &SavedDisplay) -> io::Result<()> {
    let env = Env::current();
    let streams = Streams::current();
    let flags = Flags::default();
    let status = probe(&env, streams, flags);
    let probe = status.report();
    let caps = Caps::resolve_saved(&env, streams, flags, saved, probe);
    let tmux = host::tmux_mouse(&env);
    let copying = Host::current(&env);
    let clipboard = Clipboard::current(&copying);
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|h| h.is_absolute());
    let text = report(&env, streams, &status, &caps, tmux)
        + &copy_rows(
            caps.copy_on_select.clone(),
            caps.mouse.value,
            &copying,
            &clipboard,
            home.as_deref(),
        );
    out.write_all(text.as_bytes())?;
    out.flush()
}

struct Out(String);

impl Out {
    fn heading(&mut self, title: &str) {
        let _ = writeln!(self.0, "\n{}", clean(title));
    }

    fn row(&mut self, key: &str, value: &str) {
        let _ = writeln!(self.0, "  {:<18} {}", clean(key), clean(value));
    }
}

fn rgb(c: Option<Rgb>) -> String {
    match c {
        Some(c) => format!("{c} (OKLab L {:.2})", oklab_lightness(c)),
        None => "no reply".to_owned(),
    }
}

pub const fn depth_name(d: Depth) -> &'static str {
    match d {
        Depth::TrueColor => "truecolor",
        Depth::Ansi256 => "256 colours",
        Depth::Ansi16 => "16 colours",
        Depth::None => "no colour",
    }
}

pub const fn glyphs_name(g: GlyphSet) -> &'static str {
    match g {
        GlyphSet::Unicode => "unicode",
        GlyphSet::AmbiguousWide => "ambiguous-wide",
        GlyphSet::Ascii => "ascii",
    }
}

pub const VS_CODE_WHEEL: &str = "With the mouse off, VS Code turns the wheel into up and down, which recall messages and move a question's choice. Turn the mouse on to scroll.";

pub const KNOWN_LIMITS: [&str; 3] = [
    "If Wisp is killed (SIGKILL), the terminal stays on Wisp's screen with mouse reporting on; run `reset` or close the tab.",
    "iTerm2's tmux integration (tmux -CC) breaks the alternate screen and the mouse.",
    VS_CODE_WHEEL,
];

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

pub fn report(
    env: &Env,
    streams: Streams,
    status: &ProbeStatus,
    caps: &Caps,
    tmux: Option<TmuxMouse>,
) -> String {
    let mut out = Out(format!(
        "wisp doctor {} ({} {})\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    out.heading("streams");
    out.row("stdin terminal", yes(streams.stdin_tty));
    out.row("stdout terminal", yes(streams.stdout_tty));

    out.heading("environment");
    for key in ENV_KEYS {
        let value = match env.get(key) {
            None => "(unset)".to_owned(),
            Some(_) if PRESENCE_ONLY.contains(&key) => "(set)".to_owned(),
            Some("") => "(empty)".to_owned(),
            Some(v) => caps::shown_env(key, v).to_owned(),
        };
        out.row(key, &value);
    }

    out.heading("probe");
    match status {
        ProbeStatus::Skipped(skip) => out.row("status", &format!("skipped: {}", skip.reason())),
        ProbeStatus::Failed(e) => out.row("status", &format!("failed: {e}")),
        ProbeStatus::Ran(r) => probe_rows(&mut out, r),
    }

    out.heading("resolved");
    out.row(
        "colour depth",
        &format!("{} ({})", depth_name(caps.depth.value), caps.depth.source),
    );
    out.row(
        "theme",
        &format!(
            "{} ({})",
            word_of(THEMES, &MODES, &Some(caps.mode.value)).unwrap_or_default(),
            caps.mode.source
        ),
    );
    out.row(
        "glyphs",
        &format!(
            "{} ({})",
            glyphs_name(caps.glyphs.value),
            caps.glyphs.source
        ),
    );
    out.row(
        "motion",
        &format!(
            "{} ({})",
            word_of(MOTIONS, &MOTION_POLICIES, &Some(caps.motion.value)).unwrap_or_default(),
            caps.motion.source
        ),
    );
    out.row(
        "presenter",
        &format!(
            "full screen on the alternate screen ({})",
            caps.alternate_screen.source
        ),
    );
    out.row(
        "mouse capture",
        &format!(
            "{} ({})",
            if caps.mouse.value {
                "on: the wheel scrolls, shift-drag selects in the terminal"
            } else {
                "off"
            },
            caps.mouse.source
        ),
    );
    out.row(
        "alternate scroll",
        &format!(
            "off while Wisp runs, then {} ({})",
            match caps.alternate_scroll.value {
                ScrollBack::Set => "set again",
                ScrollBack::Untouched => "left alone",
            },
            caps.alternate_scroll.source
        ),
    );
    out.row(
        "repaint",
        &format!(
            "{} ({})",
            if caps.full_repaint.value {
                "every cell, every frame"
            } else {
                "changed cells"
            },
            caps.full_repaint.source
        ),
    );
    out.row(
        "pictures",
        &format!(
            "{} ({})",
            match caps.pictures.value {
                caps::Pictures::Kitty | caps::Pictures::Sixel => "on",
                caps::Pictures::Braille => "Braille dots",
            },
            caps.pictures.source
        ),
    );
    out.row(
        "wheel step",
        &format!(
            "{} rows ({})",
            3 * caps.scroll_speed.value,
            caps.scroll_speed.source
        ),
    );
    out.row(
        "tmux mouse",
        match tmux {
            None => "not inside tmux",
            Some(TmuxMouse::On) => "on",
            Some(TmuxMouse::Off) => "off: the wheel scrolls tmux; add \"set -g mouse on\"",
            Some(TmuxMouse::Unknown) => "unknown",
        },
    );
    out.row(
        "auto output",
        &match choose::resolve(Requested::Auto, env, streams) {
            Ok(chosen) => chosen.describe(),
            Err(e) => e,
        },
    );
    out.row("kitty keys", yes(caps.kitty_keyboard));
    out.row(
        "sync output",
        match caps.synchronized_output {
            Some(true) => "held (DEC 2026 supported)",
            Some(false) => "not held (sent anyway)",
            None => "unknown (sent anyway)",
        },
    );
    out.heading("known limits");
    for limit in KNOWN_LIMITS {
        out.row("-", limit);
    }
    if !caps.warnings.is_empty() {
        out.heading("warnings");
        for w in &caps.warnings {
            out.row("-", w);
        }
    }
    out.0
}

pub const NATIVE_SELECTION: &str =
    "mouse capture is off, so drag selects in the terminal and Wisp selects nothing";

pub fn copy_rows(
    chosen: Option<Choice<bool>>,
    mouse: bool,
    host: &Host,
    clipboard: &Clipboard,
    home: Option<&std::path::Path>,
) -> String {
    let mut out = Out(String::new());
    out.heading("selection");
    if !mouse {
        out.row("drag", NATIVE_SELECTION);
        out.row("clipboard", &clipboard.tools.writer(&clipboard.route, home));
        return out.0;
    }
    let release = clipboard::copy_on_release(chosen, host);
    out.row(
        "copy on release",
        &format!("{} ({})", on_off(release.value), release.source),
    );
    out.row(
        copy::CTRL_C_KEY,
        &format!(
            "copies a selection; {} selects in the terminal",
            clipboard::select_key(host)
        ),
    );
    out.row("clipboard", &clipboard.tools.writer(&clipboard.route, home));
    out.0
}

fn probe_rows(out: &mut Out, r: &ProbeReport) {
    let ms = r.elapsed.as_millis();
    let deadline = r.plan.deadline.as_millis();
    let status = match r.outcome {
        Outcome::Answered => format!("answered in {ms} ms (deadline {deadline} ms)"),
        Outcome::TimedOut => format!("no DA1 reply within {deadline} ms"),
    };
    out.row("status", &status);
    out.row("foreground", &rgb(r.foreground));
    out.row("background", &rgb(r.background));
    let sync = match r.synchronized_output {
        SyncSupport::NotAsked => "not asked (Apple Terminal)",
        SyncSupport::NoReply => "no reply",
        SyncSupport::Supported => "supported",
        SyncSupport::Unsupported => "not supported",
    };
    out.row("DECRQM 2026", sync);
    let kitty = match r.kitty_keyboard {
        Some(flags) => format!("supported (flags {flags})"),
        None => "no reply".to_owned(),
    };
    out.row("kitty keyboard", &kitty);
    let origin = match r.origin {
        Some(p) => format!("row {}, column {}", p.row, p.col),
        None => "no reply".to_owned(),
    };
    out.row("cursor", &origin);
    let width = match (r.plan.width_test, r.dot_width) {
        (false, _) => "not measured".to_owned(),
        (true, Some(w)) => format!("{w} cell(s)"),
        (true, None) => "no reply".to_owned(),
    };
    out.row("U+00B7 width", &width);
    let da1 = match &r.device_attributes {
        Some(params) => params
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(";"),
        None => "no reply".to_owned(),
    };
    out.row("DA1", &da1);
    out.row(
        "typed meanwhile",
        &format!("{} byte(s), not replayed by doctor", r.typed.len()),
    );
    if r.dropped > 0 {
        out.row("unparsed", &format!("{} byte(s) dropped", r.dropped));
    }
}

#[cfg(test)]
mod tests;
