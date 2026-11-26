use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

#[cfg(target_os = "linux")]
use rustix::fs::{CWD, Mode, mkfifoat};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::private_files::{PrivateFileError as Error, Status};

use super::*;
use crate::plugin::KernelHost;
use crate::test_guard::ScratchDir;

fn fixture() -> (ScratchDir, PathBuf, Arc<KernelHost>, PluginHost) {
    let scratch = ScratchDir::new("wisp-private-fixture-", "files");
    let data = scratch.root.join("plugin-data");
    let kernel = Arc::new(KernelHost::new());
    kernel.set_data_dir(Some(data.clone()));
    let host = PluginHost::new("fixture", "0.4.0", &[], kernel.clone());
    (scratch, data, kernel, host)
}

fn write(
    host: &PluginHost,
    path: &str,
    before: Expected,
    bytes: &[u8],
    cap: Option<CreateCap>,
) -> Result<(), Error> {
    Caller::default().allow_private_write().sync_scope(|| {
        host.private_files()
            .change(path, before, ChangeAction::Write(bytes.to_vec()), cap)
    })
}

#[test]
fn reads_do_not_create_storage_and_old_world_and_read_only_calls_cannot_change_it() {
    let (_scratch, data, kernel, host) = fixture();
    let unavailable = PluginHost::new("fixture", "0.3.0", &[], kernel.clone());
    assert_eq!(
        unavailable.private_files().list("", 1),
        Err(Error::Unavailable)
    );
    assert_eq!(
        unavailable.private_files().read("a", 1),
        Err(Error::Unavailable)
    );
    assert_eq!(
        write(&unavailable, "a", Expected::Absent, b"a", None),
        Err(Error::Unavailable)
    );
    assert_eq!(host.private_files().list("", 1), Err(Error::NotFound));
    assert_eq!(host.private_files().read("a", 1), Err(Error::NotFound));
    let denied = || {
        host.private_files().change(
            "nested/a",
            Expected::Absent,
            ChangeAction::Write(vec![1]),
            None,
        )
    };
    assert_eq!(denied(), Err(Error::Unavailable));
    assert!(!data.exists());
}

#[test]
fn rejects_escaped_paths_links_hardlinks_fifos_and_oversized_files() {
    let (scratch, data, _, host) = fixture();
    for path in [
        "../a",
        "/a",
        "a//b",
        "a/../b",
        "a\\b",
        "a\0b",
        "a/.wisp.lock",
        "a/.WISP.LOCK",
        "a/.WISP-TMP-entry",
        "a/.wisp-tmp-test",
        "a/./b",
    ] {
        assert_eq!(
            write(&host, path, Expected::Absent, b"a", None),
            Err(Error::InvalidPath)
        );
    }
    assert!(!data.exists());
    assert_eq!(
        write(&host, "notes/ok", Expected::Absent, b"safe", None),
        Ok(())
    );
    let dir = data.join("fixture/notes");
    for d in [&data, &data.join("fixture"), &dir] {
        assert_eq!(d.metadata().unwrap().permissions().mode() & 0o777, 0o700);
    }
    assert_eq!(
        dir.join("ok").metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    let external = scratch.dir("external");
    fs::write(external.join("untouched"), b"outside").unwrap();
    symlink(&external, dir.join("elsewhere")).unwrap();
    symlink(external.join("untouched"), dir.join("link")).unwrap();
    fs::hard_link(external.join("untouched"), dir.join("hard")).unwrap();
    let mut refused = vec!["notes/elsewhere/untouched", "notes/link", "notes/hard"];
    #[cfg(target_os = "linux")]
    {
        mkfifoat(CWD, dir.join("pipe"), Mode::RUSR).unwrap();
        refused.push("notes/pipe");
    }
    let n = refused.len();
    for path in refused {
        assert_eq!(host.read(path, 100), Err(Error::Refused));
        assert_eq!(
            write(
                &host,
                path,
                Expected::hash(wisp_instructions::sha256_hex("outside"), 64),
                b"none",
                None
            ),
            Err(Error::Refused)
        );
    }
    assert_eq!(host.list("notes", n as u32), Err(Error::OverLimit));
    assert_eq!(
        host.list("notes", n as u32 + 1)
            .unwrap()
            .iter()
            .filter(|e| e.status == Status::Refused)
            .count(),
        n
    );
    assert_eq!(host.read("notes/ok", 3), Err(Error::OverLimit));
    assert_eq!(host.read("notes/ok", 4), Ok(b"safe".to_vec()));
    assert_eq!(fs::read(external.join("untouched")).unwrap(), b"outside");
}

#[test]
fn rechecks_leaf_parent_and_lock_after_a_successful_read() {
    let (scratch, data, _, host) = fixture();
    let expected = Expected::hash(wisp_instructions::sha256_hex("first"), 64);
    let dir = data.join("fixture/box");
    let external = scratch.dir("outside");
    fs::write(external.join("file"), b"first").unwrap();
    assert_eq!(
        write(&host, "box/file", Expected::Absent, b"first", None),
        Ok(())
    );
    assert_eq!(host.read("box/file", 10), Ok(b"first".to_vec()));
    fs::rename(&dir, dir.with_extension("old")).unwrap();
    symlink(&external, &dir).unwrap();
    assert_eq!(
        write(&host, "box/file", expected.clone(), b"second", None),
        Err(Error::Refused)
    );
    fs::remove_file(&dir).unwrap();
    fs::rename(dir.with_extension("old"), &dir).unwrap();
    let inside = dir.join("file");
    fs::remove_file(&inside).unwrap();
    symlink(external.join("file"), &inside).unwrap();
    assert_eq!(
        write(&host, "box/file", expected.clone(), b"second", None),
        Err(Error::Refused)
    );
    fs::remove_file(&inside).unwrap();
    fs::write(&inside, b"first").unwrap();
    let lock = dir.join(crate::secrets::LOCK_FILE);
    fs::remove_file(&lock).unwrap();
    symlink(external.join("file"), lock).unwrap();
    assert_eq!(
        write(&host, "box/file", expected, b"second", None),
        Err(Error::Refused)
    );
    assert_eq!(fs::read(&inside).unwrap(), b"first");
    assert_eq!(fs::read(external.join("file")).unwrap(), b"first");
}

#[test]
fn creates_for_distinct_names_share_the_last_slot_and_full_directories_allow_changes() {
    let (_scratch, data, kernel, host) = fixture();
    let cap = CreateCap {
        suffix: ".entry".into(),
        max_regular: 2,
    };
    let write_new = |name: &str| write(&host, name, Expected::Absent, b"start", Some(cap.clone()));
    assert_eq!(write_new("nested/base.wrong"), Err(Error::InvalidPath));
    assert_eq!(write_new("nested/base.entry"), Ok(()));
    let dir = data.join("fixture/nested");
    fs::write(dir.join(".WISP-TMP-foreign.entry"), b"temp").unwrap();
    fs::write(dir.join("ignored"), b"not counted").unwrap();
    let barrier = Barrier::new(2);
    let racing_host = PluginHost::new("fixture", "0.4.0", &[], kernel);
    let (first, second) = std::thread::scope(|s| {
        let child = s.spawn(|| {
            barrier.wait();
            write(
                &racing_host,
                "nested/b.entry",
                Expected::Absent,
                b"second",
                Some(cap.clone()),
            )
        });
        barrier.wait();
        (write_new("nested/a.entry"), child.join().unwrap())
    });
    assert!(matches!(
        (first, second),
        (Ok(()), Err(Error::CapReached)) | (Err(Error::CapReached), Ok(()))
    ));
    assert_eq!(
        write(
            &host,
            "nested/base.entry",
            Expected::hash("stale".into(), 64),
            b"changed",
            None
        ),
        Err(Error::Changed)
    );
    assert_eq!(
        write(
            &host,
            "nested/base.entry",
            Expected::hash(wisp_instructions::sha256_hex("start"), 4),
            b"bounded",
            None
        ),
        Err(Error::OverLimit)
    );
    let before = Expected::hash(wisp_instructions::sha256_hex("start"), 64);
    assert_eq!(
        write(
            &host,
            "nested/base.entry",
            before.clone(),
            b"changed",
            Some(cap)
        ),
        Err(Error::InvalidPath)
    );
    assert_eq!(
        write(&host, "nested/base.entry", before.clone(), b"changed", None),
        Ok(())
    );
    let del = |hash| {
        Caller::default()
            .allow_private_write()
            .sync_scope(|| host.change("nested/base.entry", hash, ChangeAction::Delete, None))
    };
    assert_eq!(del(before), Err(Error::Changed));
    assert_eq!(host.read("nested/base.entry", 100), Ok(b"changed".to_vec()));
    assert_eq!(
        del(Expected::hash(wisp_instructions::sha256_hex("changed"), 64)),
        Ok(())
    );
    assert_eq!(
        fs::read(dir.join(".WISP-TMP-foreign.entry")).unwrap(),
        b"temp"
    );
    assert_eq!(
        fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(".wisp-tmp-"))
            .count(),
        1
    );
}

#[test]
fn only_ancestors_above_the_data_root_may_be_symlinks() {
    let (scratch, _, kernel, host) = fixture();
    let real_home = scratch.dir("real-home");
    let linked_home = scratch.root.join("linked-home");
    symlink(&real_home, &linked_home).unwrap();
    let data = linked_home.join("plugin-data");
    kernel.set_data_dir(Some(data));
    assert_eq!(
        write(&host, "notes/kept", Expected::Absent, b"kept", None),
        Ok(())
    );
    assert_eq!(host.read("notes/kept", 10), Ok(b"kept".to_vec()));
    assert_eq!(host.list("notes", 1).unwrap()[0].name, "kept");

    let data = real_home.join("plugin-data");
    let elsewhere = real_home.join("elsewhere");
    fs::rename(&data, &elsewhere).unwrap();
    symlink(&elsewhere, &data).unwrap();
    let refuse = || {
        assert_eq!(host.list("notes", 1), Err(Error::Refused));
        assert_eq!(host.read("notes/kept", 10), Err(Error::Refused));
        assert_eq!(
            write(&host, "notes/new", Expected::Absent, b"new", None),
            Err(Error::Refused)
        );
    };
    refuse();
    fs::remove_file(&data).unwrap();
    crate::secrets::create_folder(&data).unwrap();
    symlink(elsewhere.join("fixture"), data.join("fixture")).unwrap();
    refuse();
    assert!(!elsewhere.join("fixture/notes/new").exists());
    assert_eq!(
        wisp_plugin_api::private_files::LOCK_FILE,
        crate::secrets::LOCK_FILE
    );
}
