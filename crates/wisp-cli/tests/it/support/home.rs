use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use crate::scratch::ScratchDir;

pub(crate) const DAEMON_STOPPED: &str = ".local/share/wisp/daemon/stopped";

pub(crate) const CONFIG_DIR: &str = ".config/wisp";
pub(crate) const SESSIONS: &str = ".local/share/wisp/sessions";
pub(crate) const CONFIG: &str = ".config/wisp/config.toml";
pub(crate) const CREDENTIALS: &str = ".config/wisp/credentials.json";
pub(crate) const KEYS_FILE: &str = ".config/wisp/.env";

pub(crate) struct Home(pub(crate) PathBuf, ScratchDir);

impl Home {
    pub(crate) fn new(name: &str) -> Home {
        let home = Home::bare(name);
        home.write(DAEMON_STOPPED, "");
        home
    }

    pub(crate) fn bare(name: &str) -> Home {
        let scratch = ScratchDir::new("wisp-cli-", name);
        Home(scratch.root.clone(), scratch)
    }

    pub(crate) fn configured(name: &str) -> Home {
        let home = Home::new(name);
        home.dir(CONFIG_DIR);
        home
    }

    pub(crate) fn file(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    pub(crate) fn dir(&self, relative: &str) -> PathBuf {
        self.1.dir(relative)
    }

    pub(crate) fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.file(relative)).unwrap_or_default()
    }

    pub(crate) fn exists(&self, relative: &str) -> bool {
        self.file(relative).exists()
    }

    pub(crate) fn write(&self, relative: &str, text: &str) -> PathBuf {
        self.1.write(relative, text)
    }

    pub(crate) fn env_file(&self) -> PathBuf {
        self.write("empty.env", "")
    }

    pub(crate) fn antigravity(&self, project_id: Option<&str>) -> (PathBuf, String) {
        let config = self.file(CONFIG);
        wisp_core::oauth::prepare(&config).unwrap();
        let handle = "oauth:11111111111111111111111111111111";
        let credentials = serde_json::json!({"version": 1, "stores": {"0123456789abcdef": {"antigravity": {"key": handle}}}}).to_string();
        wisp_core::secrets::write_atomic(&self.file(CREDENTIALS), credentials.as_bytes(), 0o600)
            .unwrap();
        let profile = wisp_core::oauth::profile_path(&config, handle).unwrap();
        let mut tokens = serde_json::json!({"version": 1, "kind": "antigravity", "access_token": "ya29.CANARY_antigravity_fixture", "refresh_token": "1//CANARY_antigravity_fixture", "expires_at": 4_102_444_800_u64, "generation": 1});
        if let Some(project_id) = project_id {
            tokens["project_id"] = project_id.into();
        }
        let tokens = tokens.to_string();
        wisp_core::secrets::write_atomic(&profile, tokens.as_bytes(), 0o600).unwrap();
        (profile, tokens)
    }

    pub(crate) fn logs(&self) -> String {
        let dir = self.file(SESSIONS);
        let mut text = String::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let bytes = std::fs::read(entry.path()).unwrap();
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
        text
    }

    pub(crate) fn records(&self) -> Vec<serde_json::Value> {
        let logs = self.logs();
        logs.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        crate::scratch::end_all(&self.0);
    }
}

pub(crate) fn command_in(home: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(home)
        .env_clear()
        .env("WISP_KEYCHAIN", "off")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("WISP_WSL_ROOT", home)
        .env("LANG", "C.UTF-8")
        .env("HTTPS_PROXY", "http://[::1]:9");
    command
}

pub(crate) fn fed(command: &mut Command, stdin: &str) -> Child {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut pipe = child.stdin.take().unwrap();
    if let Err(e) = pipe.write_all(stdin.as_bytes()) {
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
    }
    drop(pipe);
    child
}

pub(crate) fn run(command: &mut Command, stdin: &str) -> Output {
    fed(command, stdin).wait_with_output().unwrap()
}

pub(crate) fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub(crate) fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub(crate) fn both(output: &Output) -> String {
    format!("{}\n{}\n{}", out(output), err(output), output.status)
}

pub(crate) fn assert_clean(output: &Output, canaries: &[&str]) {
    let text = both(output);
    for canary in canaries.iter().chain(&["CANARY"]) {
        assert!(!text.contains(canary), "{canary} in {text}");
    }
}

pub(crate) fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}
