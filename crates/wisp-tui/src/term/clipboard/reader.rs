use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustix::fs::OFlags;
use tokio::sync::Notify;
use wisp_media::{MAX_SOURCE_BYTES, MediaError, Tier};
use wisp_render::chrome::ImageTier;
use wisp_render::text::sanitize;
use wisp_render::transcript::copy;

use super::{Host, Native, POWERSHELL, Raw, Running, Tools, native_routes, run_raw, span, utf16le};

pub const SLOW: Duration = Duration::from_millis(300);
pub const READ_CAP: u64 = MAX_SOURCE_BYTES as u64;
pub const BASE64_CAP: u64 = wisp_media::MAX_BASE64_INPUT as u64 + 64;
pub const HEX_CAP: u64 = READ_CAP * 2 + 64;
pub const LIST_CAP: u64 = 64 * 1024;
pub const IMAGE_TYPES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
];
pub const EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];
pub const WSLPATH: &str = "wslpath";
pub const WSL_READ_SCRIPT: &str = "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; if ([System.Windows.Forms.Clipboard]::ContainsImage()) { $i = [System.Windows.Forms.Clipboard]::GetImage(); $m = New-Object System.IO.MemoryStream; $i.Save($m, [System.Drawing.Imaging.ImageFormat]::Png); [Console]::Out.Write('image:' + [Convert]::ToBase64String($m.ToArray())) } elseif ([System.Windows.Forms.Clipboard]::ContainsFileDropList()) { foreach ($f in [System.Windows.Forms.Clipboard]::GetFileDropList()) { [Console]::Out.Write('file:' + $f + [char]10) } } else { [Console]::Out.Write('none') }";
pub const MAC_SCRIPT: &str = "the clipboard as \u{00AB}class PNGf\u{00BB}";
pub const WSL_TEXT_SCRIPT: &str = "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $t = Get-Clipboard -Raw; if ($t) { [Console]::Out.Write('text:' + $t) } else { [Console]::Out.Write('none') }";
pub const TEXT_CAP: u64 = READ_CAP;

pub fn tier_of(tier: ImageTier) -> Tier {
    Tier {
        max_edge: tier.max_edge,
        max_tokens: tier.max_tokens,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    pub ssh: bool,
    pub wsl: bool,
    pub natives: Vec<Native>,
    pub tools: Tools,
    pub home: Option<PathBuf>,
}

impl Setup {
    pub fn none() -> Setup {
        Setup {
            ssh: false,
            wsl: false,
            natives: vec![],
            tools: Tools::none(),
            home: None,
        }
    }

    pub fn current(host: &Host) -> Setup {
        Setup {
            ssh: host.ssh,
            wsl: host.wsl,
            natives: native_routes(host),
            tools: Tools::current(),
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|h| h.is_absolute()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    Image(Vec<u8>),
    Files(Vec<PathBuf>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Miss {
    OverSsh,
    NoClipboard,
    NoImage,
    Missing(Vec<&'static str>),
    TooLarge,
    Failed(String),
}

impl Miss {
    pub fn sentence(&self) -> String {
        match self {
            Miss::OverSsh => copy::NO_CLIPBOARD_OVER_SSH.to_owned(),
            Miss::NoClipboard => copy::NO_CLIPBOARD.to_owned(),
            Miss::NoImage => copy::NO_CLIPBOARD_IMAGE.to_owned(),
            Miss::Missing(what) => copy::clipboard_unreadable(&what.join("; ")),
            Miss::TooLarge => copy::CLIPBOARD_TOO_LARGE.to_owned(),
            Miss::Failed(why) => copy::clipboard_unreadable(&shown(why)),
        }
    }
}

pub const fn missing_reader(native: Native) -> &'static str {
    match native {
        Native::Windows | Native::Wayland => native.missing(),
        Native::Mac => "install pngpaste",
        Native::X11 => "install xclip",
    }
}

pub fn shown(text: &str) -> String {
    sanitize(text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn raw_failure(program: &str, raw: &Raw, timeout: Duration) -> Miss {
    match raw {
        Raw::Exited {
            code: Some(code), ..
        } => Miss::Failed(format!("{program} failed (exit {code})")),
        Raw::Exited { code: None, .. } => {
            Miss::Failed(format!("{program} was stopped by a signal"))
        }
        Raw::Over => Miss::TooLarge,
        Raw::Missing => Miss::Failed(format!("{program} wasn't found")),
        Raw::TimedOut => Miss::Failed(format!("{program} took longer than {}", span(timeout))),
        Raw::Unstarted(e) => Miss::Failed(format!("{program} couldn't start: {e}")),
    }
}

pub fn read_clipboard(setup: &Setup, running: &Running) -> Result<Found, Miss> {
    if setup.ssh {
        return Err(Miss::OverSsh);
    }
    if setup.natives.is_empty() {
        return Err(Miss::NoClipboard);
    }
    let mut first: Option<Miss> = None;
    let mut missing = vec![];
    for native in &setup.natives {
        match read_native(*native, setup, running) {
            Ok(found) => return Ok(found),
            Err(Miss::Missing(what)) => missing.extend(what),
            Err(Miss::TooLarge) => return Err(Miss::TooLarge),
            Err(miss) => {
                first.get_or_insert(miss);
            }
        }
    }
    Err(first.unwrap_or(Miss::Missing(missing)))
}

fn read_native(native: Native, setup: &Setup, running: &Running) -> Result<Found, Miss> {
    match native {
        Native::Windows => windows(setup, running),
        Native::Mac => mac(setup, running),
        Native::Wayland => listed(setup, running, &WAYLAND_READ),
        Native::X11 => listed(setup, running, &X11_READ),
    }
}

fn found(tools: &Tools, name: &'static str, native: Native) -> Result<PathBuf, Miss> {
    tools
        .find(name)
        .ok_or_else(|| Miss::Missing(vec![missing_reader(native)]))
}

pub fn encoded_read_script() -> String {
    wisp_media::encode_base64(&utf16le(WSL_READ_SCRIPT))
}

fn windows(setup: &Setup, running: &Running) -> Result<Found, Miss> {
    let tools = &setup.tools;
    let program = found(tools, POWERSHELL, Native::Windows)?;
    let script = encoded_read_script();
    let args = [
        "-NoProfile",
        "-NonInteractive",
        "-Sta",
        "-EncodedCommand",
        script.as_str(),
    ];
    let out = match run_raw(&program, &args, None, Some(BASE64_CAP), tools, running) {
        Raw::Exited { code: Some(0), out } => out,
        other => return Err(raw_failure(POWERSHELL, &other, tools.timeout)),
    };
    let text = String::from_utf8_lossy(&out);
    let text = text.trim_start_matches('\u{FEFF}').trim();
    if let Some(data) = text.strip_prefix("image:") {
        return wisp_media::decode_base64(data.trim())
            .map(Found::Image)
            .map_err(|_| {
                Miss::Failed("PowerShell returned an image Wisp couldn't decode".to_owned())
            });
    }
    if text.starts_with("file:") {
        let windows: Vec<String> = text
            .lines()
            .filter_map(|l| l.trim_end_matches('\r').strip_prefix("file:"))
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        let mut paths = vec![];
        for path in windows {
            paths.push(wslpath(&path, setup, running)?);
        }
        return Ok(Found::Files(paths));
    }
    if text == "none" || text.is_empty() {
        return Err(Miss::NoImage);
    }
    Err(Miss::Failed(
        "PowerShell gave an answer Wisp didn't expect".to_owned(),
    ))
}

pub fn wslpath(windows: &str, setup: &Setup, running: &Running) -> Result<PathBuf, Miss> {
    if windows.starts_with('-') || windows.chars().any(char::is_control) {
        return Err(Miss::Failed(format!(
            "{} isn't a Windows path",
            shown(windows)
        )));
    }
    let tools = &setup.tools;
    let Some(program) = tools.find(WSLPATH) else {
        return Err(Miss::Failed("wslpath wasn't found".to_owned()));
    };
    match run_raw(
        &program,
        &["-u", windows],
        None,
        Some(LIST_CAP),
        tools,
        running,
    ) {
        Raw::Exited { code: Some(0), out } => {
            let text = String::from_utf8_lossy(&out);
            let path = text.trim_end_matches(['\n', '\r']);
            if path.starts_with('/') && !path.contains('\n') {
                Ok(PathBuf::from(path))
            } else {
                Err(Miss::Failed(format!(
                    "wslpath couldn't convert {}",
                    shown(windows)
                )))
            }
        }
        other => Err(raw_failure(WSLPATH, &other, tools.timeout)),
    }
}

struct Listing {
    native: Native,
    program: &'static str,
    list: &'static [&'static str],
    fetch: fn(&str) -> Vec<String>,
}

fn wayland_fetch(kind: &str) -> Vec<String> {
    vec!["--type".to_owned(), kind.to_owned()]
}

fn x11_fetch(kind: &str) -> Vec<String> {
    vec![
        "-selection".to_owned(),
        "clipboard".to_owned(),
        "-t".to_owned(),
        kind.to_owned(),
        "-o".to_owned(),
    ]
}

const WAYLAND_READ: Listing = Listing {
    native: Native::Wayland,
    program: "wl-paste",
    list: &["--list-types"],
    fetch: wayland_fetch,
};

const X11_READ: Listing = Listing {
    native: Native::X11,
    program: "xclip",
    list: &["-selection", "clipboard", "-t", "TARGETS", "-o"],
    fetch: x11_fetch,
};

pub fn pick_type(listing: &str) -> Option<&'static str> {
    let offered: Vec<String> = listing
        .lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .collect();
    IMAGE_TYPES
        .iter()
        .find(|t| offered.iter().any(|o| o == *t))
        .copied()
}

fn listed(setup: &Setup, running: &Running, how: &Listing) -> Result<Found, Miss> {
    let tools = &setup.tools;
    let program = found(tools, how.program, how.native)?;
    let listing = match run_raw(&program, how.list, None, Some(LIST_CAP), tools, running) {
        Raw::Exited { code: Some(0), out } => String::from_utf8_lossy(&out).into_owned(),
        Raw::Exited { .. } => return Err(Miss::NoImage),
        other => return Err(raw_failure(how.program, &other, tools.timeout)),
    };
    let Some(kind) = pick_type(&listing) else {
        return Err(Miss::NoImage);
    };
    let args = (how.fetch)(kind);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match run_raw(&program, &args, None, Some(READ_CAP), tools, running) {
        Raw::Exited { code: Some(0), out } if out.is_empty() => Err(Miss::NoImage),
        Raw::Exited { code: Some(0), out } => Ok(Found::Image(out)),
        other => Err(raw_failure(how.program, &other, tools.timeout)),
    }
}

pub fn mac_hex(out: &str) -> Option<Vec<u8>> {
    let inner = out
        .trim()
        .strip_prefix("\u{00AB}data PNGf")?
        .strip_suffix('\u{00BB}')?;
    if inner.is_empty() || inner.len() % 2 != 0 {
        return None;
    }
    inner
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(text, 16).ok()
        })
        .collect()
}

fn mac(setup: &Setup, running: &Running) -> Result<Found, Miss> {
    let tools = &setup.tools;
    let mut tried = None;
    if let Some(program) = tools.find("osascript") {
        match run_raw(
            &program,
            &["-e", MAC_SCRIPT],
            None,
            Some(HEX_CAP),
            tools,
            running,
        ) {
            Raw::Exited { code: Some(0), out } => {
                if let Some(bytes) = mac_hex(&String::from_utf8_lossy(&out)) {
                    return Ok(Found::Image(bytes));
                }
                tried = Some(Miss::NoImage);
            }
            Raw::Exited { code: Some(_), .. } => tried = Some(Miss::NoImage),
            Raw::Over => return Err(Miss::TooLarge),
            timed_out @ Raw::TimedOut => {
                return Err(raw_failure("osascript", &timed_out, tools.timeout));
            }
            other => tried = Some(raw_failure("osascript", &other, tools.timeout)),
        }
    }
    if let Some(program) = tools.find("pngpaste") {
        return match run_raw(&program, &["-"], None, Some(READ_CAP), tools, running) {
            Raw::Exited { code: Some(0), out } if !out.is_empty() => Ok(Found::Image(out)),
            Raw::Exited { .. } => Err(Miss::NoImage),
            other => Err(raw_failure("pngpaste", &other, tools.timeout)),
        };
    }
    Err(tried.unwrap_or_else(|| Miss::Missing(vec![missing_reader(Native::Mac)])))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Text {
    Found(String),
    Empty,
    Failed(String),
}

pub fn encoded_text_script() -> String {
    wisp_media::encode_base64(&utf16le(WSL_TEXT_SCRIPT))
}

fn text_reader(native: Native, script: &str) -> (&'static str, Vec<String>) {
    let owned = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect();
    match native {
        Native::Windows => (
            POWERSHELL,
            owned(&[
                "-NoProfile",
                "-NonInteractive",
                "-Sta",
                "-EncodedCommand",
                script,
            ]),
        ),
        Native::Mac => ("pbpaste", vec![]),
        Native::Wayland => ("wl-paste", owned(&["--no-newline", "--type", "text"])),
        Native::X11 => (
            "xclip",
            owned(&["-selection", "clipboard", "-t", "UTF8_STRING", "-o"]),
        ),
    }
}

fn windows_text(out: &str) -> Option<String> {
    out.trim_start_matches('\u{FEFF}')
        .strip_prefix("text:")
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

pub fn read_text(setup: &Setup, running: &Running) -> Text {
    if setup.ssh {
        return Text::Empty;
    }
    let script = encoded_text_script();
    let tools = &setup.tools;
    for native in &setup.natives {
        let (name, args) = text_reader(*native, &script);
        let Some(program) = tools.find(name) else {
            continue;
        };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        return match run_raw(&program, &args, None, Some(TEXT_CAP), tools, running) {
            Raw::Exited { code: Some(0), out } => {
                let text = String::from_utf8_lossy(&out);
                let found = if *native == Native::Windows {
                    windows_text(&text)
                } else {
                    Some(text.into_owned()).filter(|t| !t.is_empty())
                };
                found.map_or(Text::Empty, Text::Found)
            }
            Raw::Exited { code: Some(_), .. } => Text::Empty,
            Raw::Over => Text::Failed(copy::CLIPBOARD_TEXT_TOO_LARGE.to_owned()),
            other => match raw_failure(name, &other, tools.timeout) {
                Miss::Failed(why) => Text::Failed(copy::clipboard_unreadable(&shown(&why))),
                miss => Text::Failed(miss.sentence()),
            },
        };
    }
    Text::Empty
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Candidate {
    Unix(PathBuf),
    Windows(String),
}

fn special(c: char) -> bool {
    c.is_whitespace() || "'\"\\()[]{}&;|<>*?$!#`~=,".contains(c)
}

fn words(text: &str) -> Option<Vec<String>> {
    let mut out = vec![];
    let mut word = String::new();
    let mut started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' if chars.peek() == Some(&'"') => {
                            word.push('"');
                            chars.next();
                        }
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                started = true;
                match chars.peek() {
                    Some(&next) if special(next) => {
                        word.push(next);
                        chars.next();
                    }
                    _ => word.push('\\'),
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        out.push(word);
    }
    Some(out)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn windows_path(word: &str) -> bool {
    let mut chars = word.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), Some('\\' | '/')) if drive.is_ascii_alphabetic()
    )
}

fn image_extension(word: &str) -> bool {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
    })
}

fn candidate(word: &str, home: Option<&Path>, wsl: bool) -> Option<Candidate> {
    if word.chars().any(char::is_control) || !image_extension(word) {
        return None;
    }
    if let Some(rest) = word.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let decoded = percent_decode(rest)?;
        if decoded.chars().any(char::is_control) {
            return None;
        }
        let drive = decoded.strip_prefix('/').filter(|d| windows_path(d));
        return match drive {
            Some(drive) if wsl => Some(Candidate::Windows(drive.to_owned())),
            Some(_) => None,
            None if decoded.starts_with('/') => Some(Candidate::Unix(PathBuf::from(decoded))),
            None => None,
        };
    }
    if word.starts_with('/') {
        return Some(Candidate::Unix(PathBuf::from(word)));
    }
    if let Some(rest) = word.strip_prefix("~/") {
        return home.map(|h| Candidate::Unix(h.join(rest)));
    }
    if wsl && windows_path(word) {
        return Some(Candidate::Windows(word.to_owned()));
    }
    None
}

pub fn image_paths(text: &str, home: Option<&Path>, wsl: bool) -> Option<Vec<Candidate>> {
    if text.len() > 64 * 1024 {
        return None;
    }
    let words = words(text)?;
    if words.is_empty() {
        return None;
    }
    words.iter().map(|w| candidate(w, home, wsl)).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileMiss {
    NotFiles,
    Refused(String),
}

pub fn display_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
    shown(&name)
}

pub fn read_file(path: &Path) -> Result<Vec<u8>, FileMiss> {
    let name = display_name(path);
    let refused = |why: &str| FileMiss::Refused(copy::not_attached(&name, why));
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(OFlags::NONBLOCK.bits().cast_signed())
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(FileMiss::NotFiles),
        Err(e) => return Err(refused(&format!("{}.", shown(&e.to_string())))),
    };
    let meta = file
        .metadata()
        .map_err(|e| refused(&format!("{}.", shown(&e.to_string()))))?;
    if !meta.is_file() {
        return Err(FileMiss::NotFiles);
    }
    if meta.len() > READ_CAP {
        return Err(refused(&MediaError::SourceTooLarge.to_string()));
    }
    let mut bytes = vec![];
    file.take(READ_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| refused(&format!("{}.", shown(&e.to_string()))))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > READ_CAP {
        return Err(refused(&MediaError::SourceTooLarge.to_string()));
    }
    Ok(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attached {
    pub data: Arc<str>,
    pub width: u32,
    pub height: u32,
    pub tokens: u64,
}

pub fn prepare(bytes: &[u8], tier: ImageTier) -> Result<Attached, MediaError> {
    let normalised = wisp_media::normalise(bytes, tier_of(tier))?;
    Ok(Attached {
        data: Arc::from(wisp_media::encode_base64(bytes)),
        width: normalised.width,
        height: normalised.height,
        tokens: wisp_media::tokens(normalised.width, normalised.height),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    Clipboard,
    Paths { text: String, paths: Vec<Candidate> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Got {
    Images {
        images: Vec<Attached>,
        pasted: Option<String>,
    },
    Text(String),
    Failed(String),
    Refused {
        text: String,
        why: String,
    },
    Pasted(String),
    NoText,
}

fn named(bytes: Vec<u8>, name: &str, tier: ImageTier) -> Result<Attached, String> {
    prepare(&bytes, tier).map_err(|e| copy::not_attached(name, &e.to_string()))
}

fn files(paths: &[PathBuf], tier: ImageTier) -> Result<Vec<Attached>, FileMiss> {
    let mut out = vec![];
    for path in paths {
        let bytes = read_file(path)?;
        out.push(named(bytes, &display_name(path), tier).map_err(FileMiss::Refused)?);
    }
    Ok(out)
}

pub fn perform(job: Job, tier: ImageTier, setup: &Setup, running: &Running) -> Got {
    match job {
        Job::Clipboard => match read_clipboard(setup, running) {
            Ok(Found::Image(bytes)) => match named(bytes, copy::CLIPBOARD_IMAGE, tier) {
                Ok(image) => Got::Images {
                    images: vec![image],
                    pasted: None,
                },
                Err(why) => Got::Failed(why),
            },
            Ok(Found::Files(paths)) => {
                let images: Vec<PathBuf> = paths
                    .into_iter()
                    .filter(|p| image_extension(&p.to_string_lossy()))
                    .collect();
                if images.is_empty() {
                    return Got::Failed(copy::NO_CLIPBOARD_IMAGE.to_owned());
                }
                match files(&images, tier) {
                    Ok(images) => Got::Images {
                        images,
                        pasted: None,
                    },
                    Err(FileMiss::NotFiles) => Got::Failed(copy::NO_CLIPBOARD_IMAGE.to_owned()),
                    Err(FileMiss::Refused(why)) => Got::Failed(why),
                }
            }
            Err(miss) => Got::Failed(miss.sentence()),
        },
        Job::Paths { text, paths } => {
            let mut resolved = vec![];
            for path in paths {
                match path {
                    Candidate::Unix(path) => resolved.push(path),
                    Candidate::Windows(windows) => match wslpath(&windows, setup, running) {
                        Ok(path) => resolved.push(path),
                        Err(_) => return Got::Text(text),
                    },
                }
            }
            match files(&resolved, tier) {
                Ok(images) => Got::Images {
                    images,
                    pasted: Some(text),
                },
                Err(FileMiss::NotFiles) => Got::Text(text),
                Err(FileMiss::Refused(why)) => Got::Refused { text, why },
            }
        }
    }
}

#[derive(Default)]
struct Shared {
    results: Mutex<Vec<Got>>,
    running: Running,
}

impl Shared {
    fn results(&self) -> std::sync::MutexGuard<'_, Vec<Got>> {
        self.results.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct Reader {
    setup: Arc<Setup>,
    shared: Arc<Shared>,
    wake: Arc<Notify>,
    busy: bool,
}

impl Reader {
    pub fn new(setup: Setup, wake: Arc<Notify>) -> Reader {
        Reader {
            setup: Arc::new(setup),
            shared: Arc::new(Shared::default()),
            wake,
            busy: false,
        }
    }

    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn start(&mut self, job: Job, tier: ImageTier) -> Result<(), String> {
        self.spawn(move |setup, running| perform(job, tier, setup, running))
    }

    pub fn start_text(&mut self) -> Result<(), String> {
        self.spawn(|setup, running| match read_text(setup, running) {
            Text::Found(text) => Got::Pasted(text),
            Text::Empty => Got::NoText,
            Text::Failed(why) => Got::Failed(why),
        })
    }

    fn spawn(
        &mut self,
        work: impl FnOnce(&Setup, &Running) -> Got + Send + 'static,
    ) -> Result<(), String> {
        if self.busy {
            return Err(copy::STILL_READING_CLIPBOARD.to_owned());
        }
        let shared = Arc::clone(&self.shared);
        let setup = Arc::clone(&self.setup);
        let wake = Arc::clone(&self.wake);
        let spawned = std::thread::Builder::new()
            .name("wisp-image".to_owned())
            .spawn(move || {
                let got = work(&setup, &shared.running);
                shared.results().push(got);
                wake.notify_one();
            });
        match spawned {
            Ok(_) => {
                self.busy = true;
                Ok(())
            }
            Err(e) => Err(copy::clipboard_unreadable(&shown(&e.to_string()))),
        }
    }

    pub fn take(&mut self) -> Vec<Got> {
        let got = std::mem::take(&mut *self.shared.results());
        if !got.is_empty() {
            self.busy = false;
        }
        got
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.shared.running.stop();
    }
}

#[cfg(test)]
mod tests;
