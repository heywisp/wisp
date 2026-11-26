use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::Barrier;

use super::*;
use crate::instructions::make_repository;
use crate::test_guard::ScratchDir;

fn gitfile(top: &Path, target: &Path) {
    fs::create_dir_all(top).unwrap();
    let pointer = [
        GITFILE,
        b" ",
        target.as_os_str().as_encoded_bytes(),
        b"\r\n",
    ]
    .concat();
    fs::write(top.join(".git"), pointer).unwrap();
}

fn worktree_admin(git: &Path) -> PathBuf {
    let admin = git.join("worktrees/linked admin");
    fs::create_dir_all(&admin).unwrap();
    fs::write(admin.join("commondir"), b"../..\n").unwrap();
    admin
}

#[test]
fn linked_worktrees_and_submodules_find_their_common_dir_and_broken_pointers_none() {
    let scratch = ScratchDir::new("wisp-project-key-", "common-dir");
    let repo = scratch.dir("repo");
    make_repository(&repo).unwrap();
    let git = repo.join(".git");
    let canonical = fs::canonicalize(&git).unwrap();
    assert_eq!(common_dir(&repo).as_ref(), Some(&canonical));

    let admin = worktree_admin(&git);
    let absolute = scratch.root.join("absolute");
    let relative = scratch.root.join("relative");
    gitfile(&absolute, &admin);
    gitfile(&relative, Path::new("../repo/.git/worktrees/linked admin"));
    for top in [absolute, relative] {
        assert_eq!(common_dir(&top).as_ref(), Some(&canonical));
    }

    let sub = git.join("modules/sub");
    fs::create_dir_all(sub.parent().unwrap()).unwrap();
    let nested = repo.join("nested");
    make_repository(&nested).unwrap();
    fs::rename(nested.join(".git"), &sub).unwrap();
    gitfile(&nested, Path::new("../.git/modules/sub"));
    assert_eq!(common_dir(&nested), Some(fs::canonicalize(sub).unwrap()));

    let broken = scratch.dir("broken");
    let marker = broken.join(".git");
    fs::create_dir(&marker).unwrap();
    assert!(common_dir(&broken).is_none(), "empty Git folder");
    fs::remove_dir(&marker).unwrap();
    for (name, pointer) in [
        ("missing target", "gitdir: ../missing"),
        ("target not a Git folder", "gitdir: ../broken"),
        ("no separator", "gitdir:../repo/.git"),
        ("empty path", "gitdir: \n"),
    ] {
        fs::write(&marker, pointer).unwrap();
        assert!(common_dir(&broken).is_none(), "{name}");
    }
    gitfile(&broken, &admin);
    fs::write(admin.join("commondir"), b"missing\n").unwrap();
    assert!(common_dir(&broken).is_none(), "missing common Git folder");

    #[cfg(target_os = "linux")]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let repo = scratch.root.join(OsStr::from_bytes(b"repo-\xff"));
        make_repository(&repo).unwrap();
        let git = repo.join(".git");
        let linked = scratch.root.join("native-linked");
        gitfile(&linked, &worktree_admin(&git));
        assert_eq!(common_dir(&linked), Some(fs::canonicalize(git).unwrap()));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn worktrees_share_one_keyed_key_and_a_reused_path_keeps_it() {
    let scratch = ScratchDir::new("wisp-project-key-", "location");
    let repo = scratch.dir("repo");
    make_repository(&repo).unwrap();
    let src = scratch.dir("repo/src");
    let git = repo.join(".git");
    let linked = scratch.root.join("linked");
    gitfile(&linked, &worktree_admin(&git));
    let data = scratch.root.join("plugin-data");
    let key = project_key(&src, &data).unwrap();
    assert_eq!(key.len(), 32);
    assert!(key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    assert!(project_key(&linked, &data).as_ref() == Some(&key));
    let canonical = fs::canonicalize(git).unwrap();
    let plain = crate::session::sha256_bytes_hex(canonical.as_os_str().as_encoded_bytes());
    assert!(key != plain[..32]);
    assert!(project_key(&scratch.dir("loose"), &data).is_none());
    let other = project_key(&repo, &scratch.root.join("other-data")).unwrap();
    assert!(other != key);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let core = data.join(CORE_ID);
        let secret = fs::metadata(core.join(SECRET_FILE)).unwrap();
        assert_eq!(secret.len(), 32);
        assert_eq!(secret.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(core).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    let moved = scratch.root.join("moved");
    fs::rename(&repo, &moved).unwrap();
    assert!(project_key(&moved, &data).unwrap() != key);
    make_repository(&repo).unwrap();
    assert!(project_key(&repo, &data).as_ref() == Some(&key));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_linked_core_folder_or_a_bad_secret_gives_none_and_is_left_alone() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let scratch = ScratchDir::new("wisp-project-key-", "bad-entries");
    let repo = scratch.dir("repo");
    make_repository(&repo).unwrap();
    let data = scratch.root.join("plugin-data");
    crate::secrets::create_folder(&data).unwrap();
    let core = data.join(CORE_ID);
    let target = scratch.dir("target");
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&target, &core).unwrap();
    assert!(project_key(&repo, &data).is_none());
    assert_eq!(mode(&target), 0o755);
    assert!(fs::read_dir(&target).unwrap().next().is_none());
    fs::remove_file(&core).unwrap();
    crate::secrets::create_folder(&core).unwrap();

    let path = core.join(SECRET_FILE);
    let file = target.join("file");
    fs::write(&file, [0; 32]).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&file, &path).unwrap();
    assert!(project_key(&repo, &data).is_none());
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(mode(&file), 0o644);
    fs::remove_file(&path).unwrap();

    for length in [31, 33] {
        let invalid = vec![0; length];
        fs::write(&path, &invalid).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(project_key(&repo, &data).is_none());
        assert!(fs::read(&path).unwrap() == invalid);
        assert_eq!(mode(&path), 0o644);
    }
    fs::remove_file(&path).unwrap();
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::FileTypeExt;
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert!(project_key(&repo, &data).is_none());
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_fifo());
        fs::remove_file(&path).unwrap();
    }
    fs::rename(file, &path).unwrap();
    assert!(project_key(&repo, &data).is_some());
    assert_eq!(mode(&path), 0o600);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn first_uses_at_once_agree_on_one_secret() {
    let scratch = ScratchDir::new("wisp-project-key-", "first-use");
    let repo = scratch.dir("repo");
    make_repository(&repo).unwrap();
    let data = scratch.root.join("plugin-data");
    let barrier = Barrier::new(8);
    std::thread::scope(|scope| {
        let mut threads = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    project_key(&repo, &data)
                })
            })
            .collect::<Vec<_>>()
            .into_iter();
        let first = threads.next().unwrap().join().unwrap();
        assert!(first.is_some());
        for thread in threads {
            assert!(thread.join().unwrap() == first);
        }
    });
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn without_safe_private_file_opens_project_keys_create_nothing() {
    let scratch = ScratchDir::new("wisp-project-key-", "unsupported");
    let repo = scratch.dir("repo");
    make_repository(&repo).unwrap();
    let data = scratch.root.join("plugin-data");
    assert!(project_key(&repo, &data).is_none());
    assert!(!data.join(CORE_ID).exists());
}
