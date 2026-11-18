use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use wisp_protocol::{link, valid_id};

const PRIVATE: &str = ".host-";

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct Socket {
    pub(crate) listener: UnixListener,
    path: PathBuf,
    data: PathBuf,
}

pub async fn bind(data: &Path) -> io::Result<Socket> {
    let dir = link::run_dir(data);
    private_dir(&dir)?;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with(PRIVATE)
            || !entry.file_type()?.is_socket()
        {
            continue;
        }
        if dead(&entry.path()).await {
            fs::remove_file(entry.path())?;
        }
    }
    let path = dir.join(format!(
        "{PRIVATE}{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    Socket::at(data, path)
}

impl Socket {
    pub(crate) fn at(data: &Path, path: PathBuf) -> io::Result<Self> {
        let listener = UnixListener::bind(&path)?;
        let socket = Self {
            listener,
            path,
            data: data.to_owned(),
        };
        fs::set_permissions(socket.path(), fs::Permissions::from_mode(0o600))?;
        Ok(socket)
    }

    pub fn publish(&mut self, session: &str) -> io::Result<()> {
        if !valid_id(session) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                wisp_protocol::INVALID_SESSION_ID,
            ));
        }
        let path = link::socket(&self.data, session);
        fs::rename(&self.path, &path)?;
        self.path = path;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

pub(crate) async fn dead(path: &Path) -> bool {
    matches!(tokio::time::timeout(Duration::from_millis(100), UnixStream::connect(path)).await,
        Ok(Err(error)) if matches!(error.kind(), io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound))
}

pub fn same_user(stream: &UnixStream) -> io::Result<()> {
    if stream.peer_cred()?.uid() == rustix::process::getuid().as_raw() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a different user cannot open this chat",
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Checked,
    Unchecked,
}

pub fn admit(stream: &UnixStream, window: bool) -> io::Result<Admission> {
    same_user(stream)?;
    let pid = stream
        .peer_cred()?
        .pid()
        .and_then(|pid| u32::try_from(pid).ok());
    admit_pid(pid, std::process::id(), window)
}

fn admit_pid(pid: Option<u32>, host: u32, window: bool) -> io::Result<Admission> {
    let descended = pid.and_then(|pid| descendant(pid, host));
    let job = pid.is_some_and(|pid| {
        pid != host && crate::daemon::pid_in_job(pid) && descendant(host, pid) != Some(true)
    });
    if job || descended == Some(true) || (descended.is_none() && window) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this process cannot open this chat",
        ));
    }
    Ok(if descended.is_some() {
        Admission::Checked
    } else {
        Admission::Unchecked
    })
}

#[cfg(target_os = "linux")]
fn descendant(mut pid: u32, ancestor: u32) -> Option<bool> {
    if pid == ancestor {
        return Some(false);
    }
    while pid > 1 {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = stat.rsplit_once(')')?.1;
        let parent = after.split_whitespace().nth(1)?.parse::<u32>().ok()?;
        if parent == ancestor {
            return Some(true);
        }
        if parent == pid {
            return None;
        }
        pid = parent;
    }
    Some(false)
}

#[cfg(not(target_os = "linux"))]
fn descendant(_pid: u32, _ancestor: u32) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests;
