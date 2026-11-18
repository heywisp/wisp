#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "each including binary uses part of it, and a failed setup fails the test"
)]
use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};

pub(crate) fn alive(pid: i32) -> bool {
    let running = || {
        fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| !matches!(state, "Z" | "X"))
        })
    };
    Pid::from_raw(pid).is_some_and(|pid| test_kill_process(pid) != Err(Errno::SRCH)) && running()
}

pub(crate) struct ScratchDir {
    pub(crate) root: PathBuf,
}

impl ScratchDir {
    pub(crate) fn new(prefix: &str, name: &str) -> Self {
        let name: String = name.chars().take(12).collect();
        sweep_dead(prefix);
        let dir = tempfile::Builder::new()
            .prefix(&format!("{prefix}{}-{name}-", std::process::id()))
            .tempdir()
            .unwrap()
            .keep();
        Self {
            root: fs::canonicalize(dir).unwrap(),
        }
    }

    pub(crate) fn dir(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let mut dirs = vec![self.root.clone()];
        while let Some(dir) = dirs.pop() {
            if fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
                let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
                if let Ok(entries) = fs::read_dir(&dir) {
                    dirs.extend(entries.flatten().map(|e| e.path()));
                }
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn sweep_dead(prefix: &str) {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name
            .to_str()
            .and_then(|n| n.strip_prefix(prefix))
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse().ok());
        if let Some(pid) = pid
            && !alive(pid)
        {
            end_all(&entry.path());
            drop(ScratchDir { root: entry.path() });
        }
    }
}

pub(crate) fn end_all(root: &Path) {
    let mut folders = vec![root.to_owned()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            }
            if !kind.is_socket() {
                continue;
            }
            let Ok(stream) = std::os::unix::net::UnixStream::connect(entry.path()) else {
                continue;
            };
            let Some(pid) = peer_pid(stream) else {
                continue;
            };
            if pid.as_raw_nonzero().get() == std::process::id() as i32 {
                continue;
            }
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            let until = Instant::now() + Duration::from_secs(5);
            while alive(pid.as_raw_nonzero().get()) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            if alive(pid.as_raw_nonzero().get()) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn peer_pid(stream: std::os::unix::net::UnixStream) -> Option<Pid> {
    rustix::net::sockopt::socket_peercred(&stream)
        .ok()
        .map(|cred| cred.pid)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn peer_pid(stream: std::os::unix::net::UnixStream) -> Option<Pid> {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .ok()?;
        let _entered = runtime.enter();
        stream.set_nonblocking(true).ok()?;
        let stream = tokio::net::UnixStream::from_std(stream).ok()?;
        stream.peer_cred().ok()?.pid().and_then(Pid::from_raw)
    })
    .join()
    .ok()
    .flatten()
}
