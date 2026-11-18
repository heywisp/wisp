use std::fs::File;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use super::*;
use crate::call::OutputDir;

const OTHER: &str = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
const RUNNING: &str = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c";
const RECENT: &str = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5d";
const STILL_WRITTEN: &str = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5e";
const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

fn days_ago(days: u64) -> SystemTime {
    now() - Duration::from_secs(days * 24 * 60 * 60)
}

struct Place {
    _root: tempfile::TempDir,
    temp: PathBuf,
    logs: PathBuf,
}

fn place() -> Place {
    let root = tempfile::tempdir().unwrap();
    let temp = root.path().join("tmp");
    let logs = root.path().join("sessions");
    fs::create_dir(&temp).unwrap();
    fs::create_dir(&logs).unwrap();
    Place {
        _root: root,
        temp,
        logs,
    }
}

fn touch(path: &Path, at: SystemTime) {
    let since = at.duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let stamp = rustix::fs::Timespec {
        tv_sec: i64::try_from(since.as_secs()).unwrap(),
        tv_nsec: 0,
    };
    rustix::fs::utimensat(
        rustix::fs::CWD,
        path,
        &rustix::fs::Timestamps {
            last_access: stamp,
            last_modification: stamp,
        },
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .unwrap();
}

fn folder_at(temp: &Path, name: &str, folder: SystemTime, file: SystemTime) -> PathBuf {
    let path = temp.join(name);
    let mut builder = fs::DirBuilder::new();
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&path).unwrap();
    let out = path.join("0001-bash.out");
    fs::write(&out, "output").unwrap();
    touch(&out, file);
    touch(&path, folder);
    path
}

fn session_folder(temp: &Path, session: &str, age_days: u64) -> PathBuf {
    let path = OutputDir::new(temp, session).path().to_owned();
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    folder_at(temp, &name, days_ago(age_days), days_ago(age_days))
}

fn logged(place: &Place, session: &str) {
    fs::write(place.logs.join(format!("{session}.jsonl")), "").unwrap();
}

fn sweep(place: &Place) {
    remove_old_outputs(&place.temp, &place.logs, RUNNING, WEEK, now());
}

#[test]
fn another_sessions_old_folder_goes_and_recent_ones_stay() {
    let place = place();
    let old = session_folder(&place.temp, OTHER, 8);
    let recent = session_folder(&place.temp, RECENT, 6);
    for session in [OTHER, RECENT, STILL_WRITTEN] {
        logged(&place, session);
    }
    let still_written = folder_at(
        &place.temp,
        &format!("wisp-{STILL_WRITTEN}"),
        days_ago(30),
        days_ago(1),
    );
    sweep(&place);
    assert!(!old.exists());
    assert!(recent.exists());
    assert!(still_written.exists());
}

#[test]
fn the_running_sessions_folder_stays_however_old() {
    let place = place();
    let running = session_folder(&place.temp, RUNNING, 90);
    logged(&place, RUNNING);
    sweep(&place);
    assert!(running.exists());
}

#[test]
fn a_folder_whose_log_another_wisp_holds_stays() {
    let place = place();
    let locked = session_folder(&place.temp, OTHER, 30);
    let log = File::create(place.logs.join(format!("{OTHER}.jsonl"))).unwrap();
    log.try_lock().unwrap();
    sweep(&place);
    assert!(locked.exists());
    drop(log);
    sweep(&place);
    assert!(!locked.exists());
    assert!(place.logs.join(format!("{OTHER}.jsonl")).exists());
}

#[test]
fn only_wisps_own_private_session_folders_are_touched() {
    let place = place();
    let old = days_ago(30);
    let kept: Vec<PathBuf> = [
        format!("wisp-nolog-{OTHER}"),
        "wisp-s1".to_owned(),
        format!("other-{OTHER}"),
        format!("wisp-{}", OTHER.to_uppercase()),
        format!("wisp-{}", OTHER.replace('-', "")),
        format!("wisp-{OTHER}-x"),
    ]
    .iter()
    .map(|name| folder_at(&place.temp, name, old, old))
    .collect();
    for session in [OTHER, RECENT, STILL_WRITTEN, &OTHER.to_uppercase()] {
        logged(&place, session);
    }
    let open = session_folder(&place.temp, RECENT, 30);
    fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
    let plain = place.temp.join(format!("wisp-{STILL_WRITTEN}"));
    fs::write(&plain, "not a folder").unwrap();
    touch(&plain, old);
    sweep(&place);
    for path in kept.iter().chain([&open, &plain]) {
        assert!(path.exists(), "{}", path.display());
    }
}

#[test]
fn symlinks_are_never_followed() {
    let place = place();
    let outside = folder_at(
        place.logs.parent().unwrap(),
        "outside",
        days_ago(30),
        days_ago(30),
    );
    let link = place.temp.join(format!("wisp-{RECENT}"));
    symlink(&outside, &link).unwrap();
    let old = session_folder(&place.temp, OTHER, 30);
    logged(&place, OTHER);
    logged(&place, RECENT);
    symlink(&outside, old.join("link")).unwrap();
    touch(&old.join("link"), days_ago(30));
    touch(&old, days_ago(30));
    sweep(&place);
    assert!(!old.exists());
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(outside.join("0001-bash.out").exists());
}

#[test]
fn a_folder_without_a_log_in_this_sessions_dir_stays() {
    let place = place();
    let elsewhere = session_folder(&place.temp, OTHER, 90);
    sweep(&place);
    assert!(elsewhere.exists());
}

#[test]
fn an_unreadable_temp_dir_is_left_alone() {
    let place = place();
    remove_old_outputs(
        &place.temp.join("missing"),
        &place.logs,
        RUNNING,
        WEEK,
        now(),
    );
    assert!(place.temp.exists());
}

#[test]
fn the_sweep_leaves_the_attachments_folder_and_its_blobs() {
    let place = place();
    let attachments = place.logs.parent().unwrap().join("attachments");
    fs::create_dir(&attachments).unwrap();
    let blob = attachments.join(format!("{}.png", "a".repeat(64)));
    fs::write(&blob, "png").unwrap();
    touch(&blob, days_ago(90));
    touch(&attachments, days_ago(90));
    let in_temp = folder_at(&place.temp, "attachments", days_ago(90), days_ago(90));
    let old = session_folder(&place.temp, OTHER, 30);
    let running = session_folder(&place.temp, RUNNING, 30);
    for session in [OTHER, RUNNING] {
        logged(&place, session);
    }
    sweep(&place);
    assert!(!old.exists());
    assert!(running.exists());
    assert!(blob.exists());
    assert!(in_temp.exists());
}
