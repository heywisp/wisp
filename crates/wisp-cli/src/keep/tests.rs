use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use super::*;

const WINDOWS_PATH: &str = "/mnt/c/Users/user/AppData/Local/Microsoft/WindowsApps";

#[test]
fn resume_args_replace_both_spellings_of_a_pinned_id() {
    for words in [
        &["--session-id", "pinned", "--verbose"][..],
        &["--session-id=pinned", "--verbose"][..],
    ] {
        let extra = words.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(resume_args(&extra, None, false), extra);
        assert_eq!(
            resume_args(&extra, Some("saved"), true),
            ["--verbose", "--resume=saved", "--restarted"]
        );
    }
}

#[test]
fn the_windows_check_uses_only_its_root_and_path() {
    let scratch = crate::scratch_dir("wsl-check");
    let root = &scratch.root;
    let release = scratch.write("proc/sys/kernel/osrelease", "6.6-microsoft-standard-WSL2");
    scratch.write("proc/stat", "cpu 0\nbtime 1\n");
    let user = scratch.dir("mnt/c/Users/user");
    let file = user.join(".wslconfig");
    for (text, offer, restart) in [
        ("", true, false),
        ("[general]\ninstanceIdleTimeout=-1\n", false, true),
        ("[general]\ninstanceIdleTimeout = -1\n", false, true),
        ("\u{feff}[general]\ninstanceIdleTimeout=-1\n", false, true),
        ("[wsl2]\ninstanceIdleTimeout=-1\n", true, false),
        ("[general]\n#instanceIdleTimeout=-1\n", true, false),
        ("[general]\ninstanceIdleTimeout=15\n", true, false),
    ] {
        fs::write(&file, text).unwrap();
        assert_eq!(
            windows_stops(root, OsStr::new(WINDOWS_PATH)),
            if restart {
                WindowsStops::Restart
            } else {
                WindowsStops::Missing {
                    file: Some(file.clone()),
                    offer,
                }
            },
            "{text}"
        );
    }
    fs::remove_file(&file).unwrap();
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Missing {
            file: Some(file.clone()),
            offer: true
        }
    );
    let path = format!("/mnt/c/Users/missing/AppData/Local/Microsoft/WindowsApps:{WINDOWS_PATH}");
    assert_eq!(
        windows_stops(root, OsStr::new(&path)),
        WindowsStops::Missing {
            file: Some(file.clone()),
            offer: true
        }
    );
    assert_eq!(
        windows_stops(root, OsStr::new("/usr/bin")),
        WindowsStops::Missing {
            file: None,
            offer: false
        }
    );
    fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Missing {
            file: Some(file.clone()),
            offer: false
        }
    );
    fs::remove_file(&file).unwrap();
    let target = scratch.write("linked", "");
    symlink(&target, &file).unwrap();
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Missing {
            file: Some(file.clone()),
            offer: false
        }
    );
    fs::write(&target, WSL_LINES).unwrap();
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Restart
    );
    scratch.write("proc/stat", "btime 9999999999\n");
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Nothing
    );
    scratch.write("proc/stat", "btime not-a-number\n");
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Nothing
    );
    scratch.write("proc/stat", "btime 1000\n");
    let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000);
    let handle = fs::File::open(&file).unwrap();
    handle.set_modified(stamp).unwrap();
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Nothing
    );
    handle
        .set_modified(stamp + std::time::Duration::from_nanos(1))
        .unwrap();
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Restart
    );
    fs::write(release, "4.4-Microsoft").unwrap();
    assert_eq!(
        windows_stops(root, OsStr::new(WINDOWS_PATH)),
        WindowsStops::Nothing
    );
}

#[test]
fn adding_the_windows_line_keeps_every_other_line_and_its_line_endings() {
    for (before, after) in [
        ("", WSL_LINES),
        ("[general]\n", WSL_LINES),
        ("[general]", WSL_LINES),
        ("[general]\ninstanceIdleTimeout=15\n", WSL_LINES),
        (
            "[wsl2]\nmemory=8GB",
            "[wsl2]\nmemory=8GB\n[general]\ninstanceIdleTimeout=-1\n",
        ),
        (
            "# keep\r\n[general]\r\n# other\r\n",
            "# keep\r\n[general]\r\ninstanceIdleTimeout=-1\r\n# other\r\n",
        ),
        (
            "\u{feff}[general]\r\n# keep\r\n",
            "\u{feff}[general]\r\ninstanceIdleTimeout=-1\r\n# keep\r\n",
        ),
        (
            "[wsl2]\ninstanceIdleTimeout=10\n[general]\nother=yes\n",
            "[wsl2]\ninstanceIdleTimeout=10\n[general]\ninstanceIdleTimeout=-1\nother=yes\n",
        ),
    ] {
        assert_eq!(with_line(before), after, "{before:?}");
        assert_eq!(with_line(after), after);
    }
}

#[test]
fn writing_keeps_the_mode_and_falls_back_only_for_a_refused_operation() {
    let scratch = crate::scratch_dir("wsl-write");
    let file = scratch.root.join(".wslconfig");
    add(&file).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), WSL_LINES);
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::write(&file, "[wsl2]\nmemory=8GB\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    add(&file).unwrap();
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o640
    );
    fs::write(&file, "").unwrap();
    add_with(&file, |_, _, _| Err(io::ErrorKind::Unsupported.into())).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), WSL_LINES);
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let error = add(&scratch.root.join("missing/.wslconfig")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!scratch.root.join("missing").exists());
    let before = fs::read(&file).unwrap();
    assert!(add_with(&file, |_, _, _| Err(io::ErrorKind::Other.into())).is_ok());
    fs::write(&file, "old").unwrap();
    assert!(add_with(&file, |_, _, _| Err(io::ErrorKind::Other.into())).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(before, WSL_LINES.as_bytes());
    fs::remove_file(&file).unwrap();
    symlink(scratch.write("target", "unchanged"), &file).unwrap();
    assert!(add(&file).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), "unchanged");
}

#[test]
fn a_write_failure_is_shown_over_the_warning_only_while_the_line_is_missing() {
    let missing = WindowsStops::Missing {
        file: Some("/mnt/c/Users/user/.wslconfig".into()),
        offer: true,
    };
    let warning = missing.warning();
    assert_eq!(
        windows_notes_of(missing, Some("permission denied")),
        [
            "Wisp couldn't add the two lines: permission denied.".to_owned(),
            warning
        ]
    );
    let restart = [
        ".wslconfig changed since Linux",
        "started; Windows reads it only",
        "when Linux starts. Run",
        "wsl.exe --shutdown once, here or",
        "in Windows; it ends everything in",
        "Linux this one time.",
    ]
    .join("\n");
    assert_eq!(wsl_restart(), restart);
    assert_eq!(
        windows_notes_of(WindowsStops::Restart, Some("folder sync refused")),
        [restart]
    );
    assert!(windows_notes_of(WindowsStops::Nothing, Some("error")).is_empty());
    for text in [wsl_restart(), WSL_LINES.to_owned()] {
        assert!(!text.contains("/home/"));
    }
}

#[tokio::test(start_paused = true)]
async fn repeated_cuts_back_off_to_the_cap_and_reset_only_after_a_stable_window() {
    let mut back = Back::default();
    let block = b"closing block".to_vec();
    for seconds in [1, 2, 4, 8, 16, 30, 30] {
        back.unsent = Some(wisp_tui::app::Unsent::default());
        assert_eq!(
            back.next(block.clone(), Some("first".into()), Instant::now()),
            Ok("first".into())
        );
        assert_eq!(back.wait, Duration::from_secs(seconds));
        assert_eq!(
            back.note().as_deref(),
            Some(wisp_render::transcript::copy::WINDOW_REOPENED)
        );
        tokio::time::advance(BACK_MAX * 2).await;
        back.started();
        assert!(back.unsent.take().is_some());
        assert!(back.note().is_none());
        assert!(!back.failed(None));
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    tokio::time::advance(BACK_MAX).await;
    assert_eq!(
        back.next(block.clone(), Some("second".into()), Instant::now()),
        Ok("second".into())
    );
    assert_eq!(back.wait, Duration::from_secs(1));
    assert!(back.failed(None));
    assert!(!back.failed(None));
    assert_eq!(back.next(block.clone(), None, Instant::now()), Err(block));
}

#[tokio::test(start_paused = true)]
async fn reconnect_retries_a_silent_host_then_stops_when_its_socket_is_gone() {
    let scratch = crate::scratch_dir("retry");
    let path = scratch.root.join("chat.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let mut back = Back::default();
    back.next(Vec::new(), Some("chat".into()), Instant::now())
        .unwrap();
    let retry = tokio::spawn(async move { back.open(&path, "chat").await });
    let (first, _) = listener.accept().await.unwrap();
    tokio::time::advance(BACK_MAX).await;
    let (second, _) = listener.accept().await.unwrap();
    drop((first, second, listener));
    fs::remove_file(scratch.root.join("chat.sock")).unwrap();
    assert!(matches!(
        retry.await.unwrap().unwrap(),
        Opened::Done(code) if code == std::process::ExitCode::FAILURE
    ));
}

#[tokio::test(start_paused = true)]
async fn a_reconnect_does_not_time_out_an_opening_after_the_host_welcomes_it() {
    use wisp_protocol::link::{CONTRACT, ToClient, ToHost, Welcome};

    for restarting in [false, true] {
        let scratch = crate::scratch_dir("retry-opening");
        let path = scratch.root.join("chat.sock");
        let mut back = Back {
            restarting,
            ..Back::default()
        };
        back.next(Vec::new(), Some("chat".into()), Instant::now())
            .unwrap();
        let retry = tokio::spawn({
            let path = path.clone();
            async move { back.open(&path, "chat").await }
        });
        tokio::task::yield_now().await;
        if restarting {
            tokio::time::advance(Duration::from_secs(3)).await;
            assert!(!retry.is_finished());
        }
        fs::write(link::error_file(&path), "a healthy host also has this file").unwrap();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        assert!(matches!(
            wisp_protocol::link::read(&line),
            Some(ToHost::Hello { .. })
        ));
        let welcome = ToClient::Welcome(Welcome {
            contract: CONTRACT,
            wisp: "test".into(),
            build: crate::host::build(),
            session: "chat".into(),
            first_request: wisp_protocol::RequestId(1),
            folder: "/work".into(),
            window: false,
            restart: true,
        });
        stream
            .write_all(jsonl::line(&welcome).unwrap().as_bytes())
            .await
            .unwrap();
        line.clear();
        stream.read_line(&mut line).await.unwrap();
        assert!(matches!(
            wisp_protocol::link::read(&line),
            Some(ToHost::Open)
        ));
        tokio::time::advance(BACK_MAX * 2).await;
        assert!(!retry.is_finished());
        let opening = ToClient::Opened {
            opening: Box::new(
                serde_json::from_str(include_str!(
                    "../../../wisp-protocol/tests/fixtures/opening.json"
                ))
                .unwrap(),
            ),
            view: wisp_protocol::SessionView::default(),
            now: None,
        };
        stream
            .write_all(jsonl::line(&opening).unwrap().as_bytes())
            .await
            .unwrap();
        assert!(matches!(retry.await.unwrap().unwrap(), Opened::Link(_)));
    }
}

#[tokio::test(start_paused = true)]
async fn a_restart_without_a_socket_stops_at_its_limit_or_an_early_error_file() {
    for early in [false, true] {
        let scratch = crate::scratch_dir("retry-restart-lost");
        let path = scratch.root.join("chat.sock");
        let mut back = Back {
            restarting: true,
            ..Back::default()
        };
        back.next(
            b"The chat ended unexpectedly.\nOpen it again with: wisp --resume chat\n".to_vec(),
            Some("chat".into()),
            Instant::now(),
        )
        .unwrap();
        let retry = tokio::spawn({
            let path = path.clone();
            async move { back.open(&path, "chat").await }
        });
        tokio::task::yield_now().await;
        assert!(!retry.is_finished());
        if early {
            fs::write(
                link::error_file(&path),
                "old line\nline 1\nline 2\nline 3\nline 4\nline 5\n",
            )
            .unwrap();
            tokio::time::advance(Duration::from_millis(100)).await;
        } else {
            tokio::time::advance(STARTS_WITHIN).await;
        }
        assert!(
            matches!(retry.await.unwrap().unwrap(), Opened::Done(code) if code == std::process::ExitCode::FAILURE)
        );
    }
}

#[tokio::test]
async fn a_host_gets_only_this_launchs_resume_and_restarted_flags() {
    let scratch = crate::scratch_dir("host-flags");
    let program = scratch.write(
        "host",
        "#!/bin/sh\nIFS= read -r start\nprintf '%s\\n' \"$@\" > \"$0.args\"\nprintf '{\"type\":\"ask\"}\\n'\n",
    );
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let extra = [
        "--resume",
        "old",
        "--restarted",
        "--resume=another",
        "--config=kept",
    ]
    .map(OsString::from);
    for restarted in [false, true] {
        let (ready, child, _) = start_host(
            &program,
            &scratch.root,
            &extra,
            &scratch.root,
            Some("new"),
            restarted,
            &Start::default(),
            |_| {},
        )
        .await;
        assert_eq!(ready, Some(Ready::Ask));
        assert!(child.unwrap().wait().await.unwrap().success());
        let args = fs::read_to_string(program.with_extension("args")).unwrap();
        let args = args.lines().collect::<Vec<_>>();
        assert!(args[0].starts_with("--host="));
        assert_eq!(
            &args[1..],
            if restarted {
                vec!["--config=kept", "--resume=new", "--restarted"]
            } else {
                vec!["--config=kept", "--resume=new"]
            }
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_restart_dial_checks_the_file_before_connecting_and_a_live_host_wins() {
    use wisp_protocol::link::{ToClient, ToHost};
    let scratch = crate::scratch_dir("restart-dial");
    let path = scratch.root.join("abcd-1234.sock");
    let task = tokio::spawn({
        let path = path.clone();
        async move { dial(&path).await }
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    fs::write(link::error_file(&path), "placeholder start failure").unwrap();
    tokio::time::advance(Duration::from_millis(100)).await;
    assert!(matches!(task.await.unwrap(), Err(reason) if reason == "placeholder start failure"));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let task = tokio::spawn({
        let path = path.clone();
        async move { dial(&path).await }
    });
    let (stream, _) = listener.accept().await.unwrap();
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    stream.read_line(&mut line).await.unwrap();
    assert!(matches!(link::read(&line), Some(ToHost::Hello { .. })));
    tokio::time::advance(Duration::from_secs(3)).await;
    assert!(!task.is_finished());
    let welcome: ToClient = serde_json::from_str(include_str!(
        "../../../wisp-protocol/tests/fixtures/link_welcome.json"
    ))
    .unwrap();
    stream
        .write_all(jsonl::line(&welcome).unwrap().as_bytes())
        .await
        .unwrap();
    assert!(task.await.unwrap().is_ok());
}
