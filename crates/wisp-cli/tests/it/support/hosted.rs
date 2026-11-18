use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use super::home::Home;

pub(crate) fn socket(home: &Home) -> Option<PathBuf> {
    let data = home.file(".local/share/wisp");
    std::fs::read_dir(wisp_protocol::link::run_dir(&data))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            wisp_protocol::link::session_of(path).is_some() && UnixStream::connect(path).is_ok()
        })
}

pub(crate) fn pid(home: &Home) -> rustix::process::Pid {
    let stream = UnixStream::connect(socket(home).unwrap()).unwrap();
    crate::scratch::peer_pid(stream).unwrap()
}
