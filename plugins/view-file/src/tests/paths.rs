use super::*;

#[test]
fn paths_resolve_against_the_working_directory_and_home() {
    let cwd = Path::new("/w/project");
    let home = Path::new("/u/me");
    let ok = |written: &str, home: Option<&Path>| resolve(written, cwd, home).unwrap();
    assert_eq!(ok("a.png", Some(home)), Path::new("/w/project/a.png"));
    assert_eq!(ok("/etc/hosts", Some(home)), Path::new("/etc/hosts"));
    assert_eq!(ok("~", Some(home)), Path::new("/u/me"));
    assert_eq!(
        ok("~/shots/a.png", Some(home)),
        Path::new("/u/me/shots/a.png")
    );
    assert_eq!(
        ok("~other/a.png", Some(home)),
        Path::new("/w/project/~other/a.png")
    );
    assert_eq!(
        resolve("~/a.png", cwd, None).unwrap_err(),
        "~/a.png starts with ~, but the user's home directory isn't known; use an absolute path."
    );
}

#[tokio::test]
async fn relative_home_and_absolute_paths_name_the_same_file() {
    let rig = Rig::new();
    let path = rig.file("logs/a.txt", b"hi\n");
    let elsewhere = rig.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    for written in [
        "logs/a.txt".to_owned(),
        "~/work/logs/a.txt".to_owned(),
        path.display().to_string(),
    ] {
        let from = if written.starts_with("logs") {
            rig.cwd.clone()
        } else {
            elsewhere.clone()
        };
        let viewed = rig.view_from(&from, &written).await;
        assert!(
            viewed
                .completed()
                .starts_with("~/work/logs/a.txt: text, 1 line"),
            "{written}: {}",
            viewed.result()
        );
    }
}

#[tokio::test]
async fn a_symlink_is_followed() {
    let rig = Rig::new();
    rig.file("real.png", &png(2, 2));
    std::os::unix::fs::symlink(rig.cwd.join("real.png"), rig.cwd.join("link.png")).unwrap();
    let viewed = rig.view("link.png").await;
    assert!(
        viewed
            .completed()
            .starts_with("~/work/link.png: image/png, "),
        "{}",
        viewed.result()
    );
    std::os::unix::fs::symlink(rig.cwd.join("gone.png"), rig.cwd.join("dangling.png")).unwrap();
    let viewed = rig.view("dangling.png").await;
    assert_eq!(
        viewed.refused(),
        "dangling.png does not exist (resolved to ~/work/dangling.png)."
    );
    let absolute = rig.cwd.join("missing.txt").display().to_string();
    let viewed = rig.view(&absolute).await;
    assert_eq!(
        viewed.refused(),
        format!("{absolute} does not exist (resolved to ~/work/missing.txt).")
    );
}

#[tokio::test]
async fn a_missing_file_names_up_to_three_similar_ones() {
    let rig = Rig::new();
    for name in [
        "build.log",
        "build.log.1",
        "built.txt",
        "build-old.log",
        "other.txt",
    ] {
        rig.file(name, b"x\n");
    }
    let viewed = rig.view("build.txt").await;
    assert_eq!(
        viewed.refused(),
        "build.txt does not exist (resolved to ~/work/build.txt). Files in that folder with \
similar names: built.txt, build.log, build.log.1."
    );
    let viewed = rig.view("nothing-like-it.md").await;
    assert_eq!(
        viewed.refused(),
        "nothing-like-it.md does not exist (resolved to ~/work/nothing-like-it.md)."
    );
    let viewed = rig.view("no/such/dir.txt").await;
    assert_eq!(
        viewed.refused(),
        "no/such/dir.txt does not exist (resolved to ~/work/no/such/dir.txt)."
    );
}

#[tokio::test]
async fn things_that_are_not_files_are_named() {
    let rig = Rig::new();
    fs::create_dir_all(rig.cwd.join("logs")).unwrap();
    let viewed = rig.view("logs").await;
    assert_eq!(
        viewed.refused(),
        "logs (resolved to ~/work/logs) is a directory; list it with bash (ls)."
    );
    let fifo = rig.cwd.join("pipe");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    let viewed = rig.view(&fifo.display().to_string()).await;
    assert_eq!(
        viewed.refused(),
        format!(
            "{} (resolved to ~/work/pipe) is a FIFO, not a file.",
            fifo.display()
        )
    );
    let viewed = rig.view("/dev/null").await;
    assert_eq!(viewed.refused(), "/dev/null is a device, not a file.");
}

#[tokio::test]
async fn an_unreadable_file_says_permission_denied() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    let path = rig.file("secret.txt", b"x\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&path).is_ok() {
        return;
    }
    let viewed = rig.view("secret.txt").await;
    assert_eq!(
        viewed.refused(),
        "secret.txt (resolved to ~/work/secret.txt) can't be read: permission denied."
    );
}
