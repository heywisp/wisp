use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process, kill_process_group};

use tokio::sync::Notify;
use wisp_render::chrome::account::{LINK_COPIED, LINK_COPIED_TO_TMUX, LINK_NOT_COPIED, LINK_SENT};
use wisp_render::transcript::CopyLabel;
use wisp_render::transcript::copy::{self, grouped};

use super::caps::{Choice, Env};
use super::host::wsl_root;

pub const SELECT_KEY: &str = "shift-drag";
pub const SELECT_KEY_REMOTE: &str = "shift-drag (option-drag on macOS)";
pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const OSC52_MAX: usize = 100_000;
const OSC52_WRITER: &str = "OSC 52, sent to your terminal";
pub const POWERSHELL: &str = "powershell.exe";
pub const POWERSHELL_FALLBACK: &str =
    "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";
const WSL_SCRIPT: &str = "[Console]::InputEncoding = [System.Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd()) -ErrorAction Stop";
pub const STDOUT_CAP: u64 = 64 * 1024;
const TEXT_BUSY: i32 = 26;
const SPAWN_TRIES: u32 = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Host {
    pub ssh: bool,
    pub tmux: bool,
    pub zellij: bool,
    pub wsl: bool,
    pub macos: bool,
    pub wayland: bool,
    pub x11: bool,
    pub vscode: bool,
    pub windows_terminal: bool,
    pub screen: bool,
}

impl Host {
    pub fn of(env: &Env, osrelease: Option<&str>, macos: bool) -> Host {
        let release = osrelease.is_some_and(|r| r.to_ascii_lowercase().contains("microsoft"));
        Host {
            ssh: env.present("SSH_CONNECTION") || env.present("SSH_TTY"),
            tmux: env.tmux(),
            zellij: env.zellij(),
            wsl: env.present("WSL_DISTRO_NAME") || release,
            macos,
            wayland: env.present("WAYLAND_DISPLAY"),
            x11: env.present("DISPLAY"),
            vscode: env.vscode(),
            windows_terminal: env.windows_terminal(),
            screen: env.screen(),
        }
    }

    pub fn current(env: &Env) -> Host {
        Host::at_wsl_root(env, &wsl_root())
    }

    fn at_wsl_root(env: &Env, root: &Path) -> Host {
        let release = std::fs::read_to_string(root.join("proc/sys/kernel/osrelease")).ok();
        Host::of(env, release.as_deref(), cfg!(target_os = "macos"))
    }
}

pub fn select_key(host: &Host) -> &'static str {
    if host.ssh || host.tmux {
        SELECT_KEY_REMOTE
    } else {
        SELECT_KEY
    }
}

pub fn right_click_pastes(host: &Host) -> bool {
    !host.ssh && !host.vscode && host.windows_terminal
}

pub fn terminal_pastes_on_right_click(host: &Host) -> bool {
    host.vscode
}

pub fn copy_on_release(chosen: Option<Choice<bool>>, host: &Host) -> Choice<bool> {
    if let Some(chosen) = chosen {
        return chosen;
    }
    if host.tmux || host.zellij {
        return Choice::new(
            true,
            "inside a multiplexer, which copies when a drag ends too",
        );
    }
    if host.vscode && host.wsl {
        return Choice::new(
            false,
            "VS Code on a Windows host passes ctrl-c through when nothing is selected",
        );
    }
    if host.vscode {
        return Choice::new(true, "VS Code");
    }
    if host.windows_terminal {
        return Choice::new(
            false,
            "Windows Terminal passes its copy key through when nothing is selected",
        );
    }
    Choice::new(true, "default")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Native {
    Windows,
    Mac,
    Wayland,
    X11,
}

impl Native {
    pub const fn destination(self) -> &'static str {
        match self {
            Native::Windows => "Windows clipboard",
            Native::Mac | Native::Wayland | Native::X11 => "clipboard",
        }
    }

    pub const fn missing(self) -> &'static str {
        match self {
            Native::Windows => "powershell.exe wasn't found",
            Native::Mac => "pbcopy wasn't found",
            Native::Wayland => "install wl-clipboard",
            Native::X11 => "install xclip or xsel",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Route {
    Terminal,
    Local { tmux: bool, natives: Vec<Native> },
    Blocked(&'static str),
}

pub const SCREEN_BLOCKS: &str =
    "GNU screen over SSH can't pass a copy on to your terminal; shift-drag selects in the terminal";

pub fn route(host: &Host) -> Route {
    if host.ssh && !host.tmux && host.screen {
        return Route::Blocked(SCREEN_BLOCKS);
    }
    if host.ssh && !host.tmux {
        return Route::Terminal;
    }
    Route::Local {
        tmux: host.tmux,
        natives: native_routes(host),
    }
}

pub fn native_routes(host: &Host) -> Vec<Native> {
    if host.ssh {
        return vec![];
    }
    if host.wsl {
        return vec![Native::Windows];
    }
    let mut natives = vec![];
    if host.macos {
        natives.push(Native::Mac);
    }
    if host.wayland {
        natives.push(Native::Wayland);
    }
    if host.x11 {
        natives.push(Native::X11);
    }
    natives
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    pub path: Option<OsString>,
    pub powershell: Vec<PathBuf>,
    pub setsid: Option<PathBuf>,
    pub timeout: Duration,
}

fn on_path(path: Option<&OsString>, name: &str) -> Option<PathBuf> {
    path.and_then(|path| {
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .find(|candidate| executable(candidate))
    })
}

pub fn system_setsid() -> Option<PathBuf> {
    on_path(std::env::var_os("PATH").as_ref(), "setsid")
}

fn executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

impl Tools {
    pub fn none() -> Tools {
        Tools {
            path: None,
            powershell: vec![],
            setsid: None,
            timeout: TIMEOUT,
        }
    }

    pub fn current() -> Tools {
        Tools::at_wsl_root(&wsl_root())
    }

    fn at_wsl_root(root: &Path) -> Tools {
        Tools {
            path: std::env::var_os("PATH"),
            powershell: vec![root.join(POWERSHELL_FALLBACK.trim_start_matches('/'))],
            setsid: system_setsid(),
            timeout: TIMEOUT,
        }
    }

    pub fn find(&self, name: &str) -> Option<PathBuf> {
        let found = on_path(self.path.as_ref(), name);
        if found.is_some() || name != POWERSHELL {
            return found;
        }
        self.powershell.iter().find(|p| executable(p)).cloned()
    }

    pub fn writer(&self, route: &Route, home: Option<&Path>) -> String {
        match route {
            Route::Terminal => OSC52_WRITER.to_owned(),
            Route::Blocked(why) => format!("none: {why}"),
            Route::Local { tmux, natives } => {
                let mut found: Vec<String> = natives
                    .iter()
                    .filter_map(|n| {
                        writers(*n).iter().find_map(|w| {
                            self.find(w.program)
                                .map(|p| wisp_protocol::shorten_path(&p, home))
                        })
                    })
                    .collect();
                if *tmux {
                    match self.find("tmux") {
                        Some(p) => found.push(format!(
                            "{} (load-buffer)",
                            wisp_protocol::shorten_path(&p, home)
                        )),
                        None => found.push("tmux, not found".to_owned()),
                    }
                }
                if found.is_empty() {
                    match natives.first() {
                        Some(n) => format!("none: {}", n.missing()),
                        None => "none for this session".to_owned(),
                    }
                } else {
                    found.join(", ")
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Writer {
    program: &'static str,
    args: &'static [&'static str],
    also: Option<&'static [&'static str]>,
}

const WINDOWS: [Writer; 1] = [Writer {
    program: POWERSHELL,
    args: &[],
    also: None,
}];
const MAC: [Writer; 1] = [Writer {
    program: "pbcopy",
    args: &[],
    also: None,
}];
const WAYLAND: [Writer; 1] = [Writer {
    program: "wl-copy",
    args: &[],
    also: Some(&["--primary"]),
}];
const X11: [Writer; 2] = [
    Writer {
        program: "xclip",
        args: &["-selection", "clipboard", "-i"],
        also: Some(&["-selection", "primary", "-i"]),
    },
    Writer {
        program: "xsel",
        args: &["--clipboard", "--input"],
        also: Some(&["--primary", "--input"]),
    },
];

fn powershell_args(encoded: &str) -> [&str; 4] {
    ["-NoProfile", "-NonInteractive", "-EncodedCommand", encoded]
}

fn writers(native: Native) -> &'static [Writer] {
    match native {
        Native::Windows => &WINDOWS,
        Native::Mac => &MAC,
        Native::Wayland => &WAYLAND,
        Native::X11 => &X11,
    }
}

pub fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Osc52 {
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLarge;

impl Osc52 {
    pub fn new(text: &str) -> Result<Osc52, TooLarge> {
        if text.len() > OSC52_MAX {
            return Err(TooLarge);
        }
        let mut bytes = b"\x1b]52;c;".to_vec();
        bytes.extend_from_slice(wisp_media::encode_base64(text.as_bytes()).as_bytes());
        bytes.push(0x07);
        Ok(Osc52 { bytes })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    Done(String),
    Missing,
    Failed(Option<i32>),
    TimedOut,
    Unstarted(String),
}

impl Ran {
    pub fn ok(&self) -> bool {
        matches!(self, Ran::Done(_))
    }
}

#[derive(Debug, Default)]
pub struct Running {
    group: Mutex<Option<Pid>>,
    stopped: AtomicBool,
}

impl Running {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Pid>> {
        self.group.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(group) = *self.lock() {
            kill_process(group, Signal::KILL).ok();
            kill_process_group(group, Signal::KILL).ok();
        }
    }
}

pub fn run(
    program: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    capture: bool,
    tools: &Tools,
) -> Ran {
    run_in(program, args, input, capture, tools, &Running::default())
}

fn run_in(
    program: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    capture: bool,
    tools: &Tools,
    running: &Running,
) -> Ran {
    let cap = capture.then_some(STDOUT_CAP);
    match run_raw(program, args, input, cap, tools, running) {
        Raw::Exited { code: Some(0), out } => Ran::Done(String::from_utf8_lossy(&out).into_owned()),
        Raw::Exited { code, .. } => Ran::Failed(code),
        Raw::Over => Ran::Done(String::new()),
        Raw::Missing => Ran::Missing,
        Raw::TimedOut => Ran::TimedOut,
        Raw::Unstarted(e) => Ran::Unstarted(e),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Raw {
    Exited { code: Option<i32>, out: Vec<u8> },
    Over,
    Missing,
    TimedOut,
    Unstarted(String),
}

fn kill_all(leader: Option<Pid>, child: &mut std::process::Child) {
    drop(child.kill());
    if let Some(leader) = leader {
        kill_process_group(leader, Signal::KILL).ok();
    }
    drop(child.wait());
}

fn grouped_command(program: &Path, tools: &Tools) -> Command {
    match &tools.setsid {
        Some(setsid) => {
            let mut command = Command::new(setsid);
            command.arg(program);
            command
        }
        None => {
            let mut command = Command::new(program);
            command.process_group(0);
            command
        }
    }
}

fn spawn_group(command: &mut Command, running: &Running) -> Result<(Child, Option<Pid>), Raw> {
    let mut tries = 0;
    let mut group = running.lock();
    if running.stopped.load(Ordering::SeqCst) {
        return Err(Raw::Unstarted("Wisp is exiting".to_owned()));
    }
    let child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Raw::Missing),
            Err(e) if e.raw_os_error() == Some(TEXT_BUSY) && tries < SPAWN_TRIES => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(Raw::Unstarted(e.to_string())),
        }
    };
    let leader = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
    *group = leader;
    Ok((child, leader))
}

pub(crate) fn run_raw(
    program: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    capture: Option<u64>,
    tools: &Tools,
    running: &Running,
) -> Raw {
    let timeout = tools.timeout;
    let mut command = grouped_command(program, tools);
    command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if capture.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null());
    let (mut child, leader) = match spawn_group(&mut command, running) {
        Ok(spawned) => spawned,
        Err(raw) => return raw,
    };
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let input = input.to_vec();
        drop(
            std::thread::Builder::new()
                .name("wisp-clipboard-in".to_owned())
                .spawn(move || {
                    drop(stdin.write_all(&input));
                }),
        );
    }
    let over = Arc::new(AtomicBool::new(false));
    let (sent, read) = mpsc::channel();
    match (child.stdout.take(), capture) {
        (Some(stdout), Some(cap)) => {
            let over = Arc::clone(&over);
            drop(
                std::thread::Builder::new()
                    .name("wisp-clipboard-out".to_owned())
                    .spawn(move || {
                        let mut out = Vec::new();
                        let mut limited = stdout.take(cap.saturating_add(1));
                        drop(limited.read_to_end(&mut out));
                        drop(limited);
                        if u64::try_from(out.len()).unwrap_or(u64::MAX) > cap {
                            over.store(true, Ordering::SeqCst);
                            out.clear();
                        }
                        drop(sent.send(out));
                    }),
            );
        }
        _ => drop(sent.send(Vec::new())),
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        let mut group = running.lock();
        if over.load(Ordering::SeqCst) {
            kill_all(leader, &mut child);
            *group = None;
            return Raw::Over;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                *group = None;
                break status;
            }
            Ok(None) if Instant::now() >= deadline => {
                kill_all(leader, &mut child);
                *group = None;
                return Raw::TimedOut;
            }
            Ok(None) => {
                drop(group);
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                *group = None;
                return Raw::Unstarted(e.to_string());
            }
        }
    };
    let left = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(100));
    let out = read.recv_timeout(left).unwrap_or_default();
    if over.load(Ordering::SeqCst) {
        return Raw::Over;
    }
    Raw::Exited {
        code: status.code(),
        out,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Tmux { forwards: bool },
    Native(Native),
}

impl Destination {
    pub const fn label(self) -> &'static str {
        match self {
            Destination::Tmux { forwards: true } => "tmux buffer and clipboard",
            Destination::Tmux { forwards: false } => "tmux buffer",
            Destination::Native(native) => native.destination(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Copied(Vec<Destination>),
    Sent,
    TooLarge,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Selection,
    Link,
    Code(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    pub chars: usize,
    pub outcome: Outcome,
    pub purpose: Purpose,
}

const REPLACED: &str = "another copy replaced it";

fn characters(n: usize) -> String {
    if n == 1 {
        "1 character".to_owned()
    } else {
        format!("{} characters", grouped(n))
    }
}

pub fn toast(done: &Done, separator: &str) -> String {
    match &done.outcome {
        Outcome::Copied(to) => {
            let mut labels: Vec<&str> = vec![];
            for label in to.iter().map(|d| d.label()) {
                if !labels.iter().any(|shown| shown.ends_with(label)) {
                    labels.push(label);
                }
            }
            format!(
                "Copied {} {separator} {}",
                characters(done.chars),
                labels.join(" and ")
            )
        }
        Outcome::Sent => format!("Sent {} to your terminal (OSC 52)", characters(done.chars)),
        Outcome::TooLarge => format!(
            "Too large to send to your terminal (over {} bytes).",
            grouped(OSC52_MAX)
        ),
        Outcome::Failed(why) => format!("Couldn't copy: {why}"),
    }
}

fn failure(program: &str, ran: &Ran, timeout: Duration) -> String {
    match ran {
        Ran::Done(_) => String::new(),
        Ran::Missing => format!("{program} wasn't found"),
        Ran::Failed(Some(code)) => format!("{program} failed (exit {code})"),
        Ran::Failed(None) => format!("{program} was stopped by a signal"),
        Ran::TimedOut => format!("{program} took longer than {}", span(timeout)),
        Ran::Unstarted(e) => format!("{program} couldn't start: {e}"),
    }
}

pub fn span(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{}ms", d.as_millis())
    } else {
        let secs = d.as_millis().div_ceil(1000);
        copy::clock(u64::try_from(secs).unwrap_or(u64::MAX))
    }
}

fn native(
    text: &str,
    native: Native,
    tools: &Tools,
    running: &Running,
    windows: &mut Option<windows::Helper<'_>>,
) -> Result<(), Option<String>> {
    if native == Native::Windows {
        if windows
            .as_mut()
            .is_some_and(|helper| helper.copy(text, tools.timeout))
        {
            return Ok(());
        }
        drop(windows.take());
    }
    let mut tried = None;
    for writer in writers(native) {
        let Some(program) = tools.find(writer.program) else {
            continue;
        };
        let encoded;
        let args;
        let args = if native == Native::Windows {
            encoded = wisp_media::encode_base64(&utf16le(WSL_SCRIPT));
            args = powershell_args(&encoded);
            &args
        } else {
            writer.args
        };
        let ran = run_in(&program, args, Some(text.as_bytes()), false, tools, running);
        if ran.ok() {
            if let Some(also) = writer.also {
                drop(run_in(
                    &program,
                    also,
                    Some(text.as_bytes()),
                    false,
                    tools,
                    running,
                ));
            }
            return Ok(());
        }
        tried = Some(failure(writer.program, &ran, tools.timeout));
    }
    Err(tried)
}

fn session_id(id: &str) -> bool {
    id.strip_prefix('$')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn client_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && !name.chars().any(char::is_control)
}

pub fn latest_client(listing: &str) -> Option<String> {
    listing
        .lines()
        .filter_map(|line| {
            let (activity, name) = line.trim().split_once(' ')?;
            let activity: u64 = activity.parse().ok()?;
            client_name(name).then(|| (activity, name.to_owned()))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, name)| name)
}

fn tmux(text: &str, tools: &Tools, running: &Running) -> Result<Destination, String> {
    let Some(program) = tools.find("tmux") else {
        return Err("tmux wasn't found".to_owned());
    };
    let t = tools.timeout;
    let ask = |args: &[&str]| match run_in(&program, args, None, true, tools, running) {
        Ran::Done(out) => Some(out.trim().to_owned()),
        _ => None,
    };
    let forwards = ask(&["show-options", "-gv", "set-clipboard"]).is_none_or(|v| v != "off");
    let input = Some(text.as_bytes());
    let ran = if forwards {
        let client = ask(&["display-message", "-p", "#{session_id}"])
            .filter(|id| session_id(id))
            .and_then(|id| {
                ask(&[
                    "list-clients",
                    "-t",
                    &id,
                    "-F",
                    "#{client_activity} #{client_name}",
                ])
            })
            .and_then(|listing| latest_client(&listing));
        match client {
            Some(client) => run_in(
                &program,
                &["load-buffer", "-w", "-t", &client, "-"],
                input,
                false,
                tools,
                running,
            ),
            None => run_in(
                &program,
                &["load-buffer", "-w", "-"],
                input,
                false,
                tools,
                running,
            ),
        }
    } else {
        run_in(
            &program,
            &["load-buffer", "-"],
            input,
            false,
            tools,
            running,
        )
    };
    if ran.ok() {
        Ok(Destination::Tmux { forwards })
    } else {
        Err(failure("tmux", &ran, t))
    }
}

pub fn copy_local(text: &str, tmux_too: bool, natives: &[Native], tools: &Tools) -> Outcome {
    copy_in(
        text,
        tmux_too,
        natives,
        tools,
        &Running::default(),
        &mut None,
    )
}

pub fn copy_once(
    clipboard: &Clipboard,
    text: &str,
    out: &mut dyn Write,
    running: &Running,
) -> Outcome {
    match &clipboard.route {
        Route::Blocked(why) => Outcome::Failed((*why).to_owned()),
        Route::Local { tmux, natives } => {
            copy_in(text, *tmux, natives, &clipboard.tools, running, &mut None)
        }
        Route::Terminal => match Osc52::new(text) {
            Ok(osc) => match out.write_all(osc.bytes()) {
                Ok(()) => Outcome::Sent,
                Err(e) => Outcome::Failed(e.to_string()),
            },
            Err(TooLarge) => Outcome::TooLarge,
        },
    }
}

pub fn link_words(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Copied(to) if to.iter().any(|d| matches!(d, Destination::Native(_))) => {
            LINK_COPIED
        }
        Outcome::Copied(to) if to.contains(&Destination::Tmux { forwards: false }) => {
            LINK_COPIED_TO_TMUX
        }
        Outcome::Copied(_) | Outcome::Sent => LINK_SENT,
        Outcome::TooLarge | Outcome::Failed(_) => LINK_NOT_COPIED,
    }
}

pub fn code_label(outcome: &Outcome) -> CopyLabel {
    match outcome {
        Outcome::Copied(_) => CopyLabel::Copied,
        Outcome::Sent => CopyLabel::Sent,
        Outcome::TooLarge | Outcome::Failed(_) => CopyLabel::NotCopied,
    }
}

fn copy_in(
    text: &str,
    tmux_too: bool,
    natives: &[Native],
    tools: &Tools,
    running: &Running,
    windows: &mut Option<windows::Helper<'_>>,
) -> Outcome {
    let mut to: Vec<Destination> = vec![];
    let mut why: Option<String> = None;
    if tmux_too {
        match tmux(text, tools, running) {
            Ok(dest) => to.push(dest),
            Err(e) => why = Some(e),
        }
    }
    let mut first_missing = None;
    for n in natives {
        match native(text, *n, tools, running, windows) {
            Ok(()) => {
                to.push(Destination::Native(*n));
                break;
            }
            Err(Some(e)) => {
                why.get_or_insert(e);
            }
            Err(None) => {
                first_missing.get_or_insert(n.missing());
            }
        }
    }
    if !to.is_empty() {
        return Outcome::Copied(to);
    }
    let reason = why
        .or_else(|| first_missing.map(str::to_owned))
        .unwrap_or_else(|| "no clipboard was found for this session".to_owned());
    Outcome::Failed(reason)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clipboard {
    pub route: Route,
    pub tools: Tools,
}

impl Clipboard {
    pub fn none() -> Clipboard {
        Clipboard {
            route: Route::Local {
                tmux: false,
                natives: vec![],
            },
            tools: Tools::none(),
        }
    }

    pub fn current(host: &Host) -> Clipboard {
        Clipboard {
            route: route(host),
            tools: Tools::current(),
        }
    }
}

#[derive(Default)]
struct Queue {
    pending: Option<(String, Purpose)>,
    results: Vec<Done>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    running: Running,
    windows: Running,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub enum Sender {
    Terminal,
    Blocked(&'static str),
    Local { copier: Copier },
}

impl Sender {
    pub fn new(clipboard: Clipboard, wake: Arc<Notify>) -> Sender {
        match clipboard.route {
            Route::Terminal => Sender::Terminal,
            Route::Blocked(why) => Sender::Blocked(why),
            Route::Local { tmux, natives } => Sender::Local {
                copier: Copier::new(tmux, natives, clipboard.tools, wake),
            },
        }
    }
}

pub struct Copier {
    tmux: bool,
    natives: Vec<Native>,
    tools: Arc<Tools>,
    shared: Arc<Shared>,
    wake: Arc<Notify>,
    working: bool,
    started: u64,
}

impl Copier {
    pub fn new(tmux: bool, natives: Vec<Native>, tools: Tools, wake: Arc<Notify>) -> Copier {
        let mut copier = Copier {
            tmux,
            natives,
            tools: Arc::new(tools),
            shared: Arc::new(Shared::default()),
            wake,
            working: false,
            started: 0,
        };
        if copier.natives.contains(&Native::Windows) {
            drop(copier.start_worker());
        }
        copier
    }

    pub fn started(&self) -> u64 {
        self.started
    }

    pub fn start(&mut self, text: String, purpose: Purpose) -> Option<Done> {
        self.started += 1;
        let chars = text.chars().count();
        let replaced = self.shared.lock().pending.replace((text, purpose));
        self.shared.ready.notify_one();
        if self.working {
            return replaced
                .filter(|(_, purpose)| *purpose == Purpose::Link)
                .map(|(text, purpose)| Done {
                    chars: text.chars().count(),
                    outcome: Outcome::Failed(REPLACED.to_owned()),
                    purpose,
                });
        }
        match self.start_worker() {
            Ok(()) => None,
            Err(e) => {
                self.shared.lock().pending = None;
                Some(Done {
                    chars,
                    outcome: Outcome::Failed(format!("the copy couldn't start: {e}")),
                    purpose,
                })
            }
        }
    }

    fn start_worker(&mut self) -> std::io::Result<()> {
        let shared = Arc::clone(&self.shared);
        let tools = Arc::clone(&self.tools);
        let wake = Arc::clone(&self.wake);
        let (tmux, natives) = (self.tmux, self.natives.clone());
        std::thread::Builder::new()
            .name("wisp-clipboard".to_owned())
            .spawn(move || work(&shared, tmux, &natives, &tools, &wake))?;
        self.working = true;
        Ok(())
    }

    pub fn take(&self) -> Vec<Done> {
        std::mem::take(&mut self.shared.lock().results)
    }
}

impl Drop for Copier {
    fn drop(&mut self) {
        self.shared.windows.stopped.store(true, Ordering::SeqCst);
        self.shared.lock().closed = true;
        self.shared.ready.notify_all();
        self.shared.running.stop();
    }
}

fn work(shared: &Shared, tmux: bool, natives: &[Native], tools: &Tools, wake: &Notify) {
    let warm_windows = || {
        natives
            .contains(&Native::Windows)
            .then(|| windows::Helper::start(tools, &shared.windows))
            .flatten()
    };
    let mut windows = warm_windows();
    loop {
        let (text, purpose) = {
            let mut queue = shared.lock();
            loop {
                if let Some(pending) = queue.pending.take() {
                    break pending;
                }
                if queue.closed {
                    return;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let chars = text.chars().count();
        let outcome = copy_in(&text, tmux, natives, tools, &shared.running, &mut windows);
        {
            let mut queue = shared.lock();
            queue.results.push(Done {
                chars,
                outcome,
                purpose,
            });
        }
        wake.notify_one();
        if windows.is_none() {
            windows = warm_windows();
        }
    }
}

pub mod reader;
mod windows;

#[cfg(test)]
#[path = "../../tests/support/fake_tool.rs"]
mod fake_tool;

#[cfg(test)]
mod tests;
