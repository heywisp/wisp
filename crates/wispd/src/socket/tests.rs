use super::*;

#[tokio::test]
async fn the_socket_is_private_published_and_removed_and_dead_private_names_are_swept() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(link::run_dir(dir.path())).unwrap();
    let dead = link::run_dir(dir.path()).join(format!("{PRIVATE}dead"));
    drop(UnixListener::bind(&dead).unwrap());
    let mut bound = bind(dir.path()).await.unwrap();
    assert!(!dead.exists());
    assert!(link::session_of(bound.path()).is_none());
    assert_eq!(
        fs::metadata(bound.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(link::run_dir(dir.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let stale = link::socket(dir.path(), "abcd");
    drop(UnixListener::bind(&stale).unwrap());
    bound.publish("abcd").unwrap();
    let stream = UnixStream::connect(&stale).await.unwrap();
    assert!(same_user(&stream).is_ok());
    assert_eq!(
        admit(&stream, false).unwrap(),
        if cfg!(target_os = "linux") {
            Admission::Checked
        } else {
            Admission::Unchecked
        }
    );
    assert_eq!(admit(&stream, true).is_ok(), cfg!(target_os = "linux"));
    drop(bound);
    assert!(!stale.exists());
}

#[test]
#[cfg(target_os = "linux")]
fn admission_identifies_a_descendant_and_handles_an_unavailable_process() {
    let pid = std::process::id();
    let parent = u32::try_from(rustix::process::getppid().unwrap().as_raw_pid()).unwrap();
    assert_eq!(descendant(pid, parent), Some(true));
    assert_eq!(
        admit_pid(Some(pid), parent, false).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(descendant(pid, pid), Some(false));
    assert_eq!(descendant(u32::MAX, pid), None);
    assert_eq!(admit_pid(None, pid, false).unwrap(), Admission::Unchecked);
    assert_eq!(
        admit_pid(None, pid, true).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
#[cfg(target_os = "linux")]
fn admission_refuses_unrelated_jobs_but_keeps_their_hosts_and_ancestors() {
    use std::io::{BufRead, Read};
    use std::process::{Command, Stdio};

    let mut ancestor = Command::new("sh")
        .args([
            "-c",
            "sleep 60 & child=$!; printf '%s\n' \"$child\"; read line; kill \"$child\"; wait \"$child\" 2>/dev/null; :",
        ])
        .env(wisp_protocol::WISP_JOB, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    io::BufReader::new(ancestor.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let host = line.trim().parse::<u32>().unwrap();
    let admission = |peer| {
        [false, true]
            .map(|window| admit_pid(Some(peer), host, window).map_err(|error| error.kind()))
    };
    let ancestors = [host, ancestor.id()].map(admission);
    let mut siblings = Vec::new();
    for (mark, expected) in [
        (Some("1"), Err(io::ErrorKind::PermissionDenied)),
        (Some(""), Err(io::ErrorKind::PermissionDenied)),
        (None, Ok(Admission::Checked)),
    ] {
        let mut command = Command::new("sh");
        command
            .args(["-c", "printf READY; read line"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .env_remove(wisp_protocol::WISP_JOB);
        if let Some(mark) = mark {
            command.env(wisp_protocol::WISP_JOB, mark);
        }
        let mut peer = command.spawn().unwrap();
        peer.stdout.take().unwrap().read_exact(&mut [0; 5]).unwrap();
        let descended = descendant(peer.id(), host);
        let got = admission(peer.id());
        let unknown_host = [false, true].map(|window| {
            admit_pid(Some(peer.id()), u32::MAX, window).map_err(|error| error.kind())
        });
        peer.kill().unwrap();
        peer.wait().unwrap();
        siblings.push((mark, descended, got, unknown_host, expected));
    }
    drop(ancestor.stdin.take());
    assert!(ancestor.wait().unwrap().success());
    for got in ancestors {
        assert_eq!(got, [Ok(Admission::Checked); 2]);
    }
    for (mark, descended, got, unknown_host, expected) in siblings {
        assert_eq!(descended, Some(false), "sibling mark {mark:?}");
        assert_eq!(got, [expected; 2], "sibling mark {mark:?}");
        assert_eq!(unknown_host, [expected; 2], "unknown host, mark {mark:?}");
    }
}
