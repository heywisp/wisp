use std::fs;
use std::path::Path;

use futures::executor::block_on;
use wisp_plugin_api::manage::{Action, Change, Kind as ManagedKind};
use wisp_plugin_testkit::host::FakeCall;

use crate::skills as manage;

#[path = "../../../skills/src/tests/tree.rs"]
mod tree;

use tree::{Tree, skill};

impl Tree {
    fn manage(&self, action: Action, name: &str, from: Option<&Path>) -> Result<String, String> {
        block_on(manage::change(
            &self.host(),
            &skill_change(action, name, from),
            &FakeCall::default(),
        ))
    }
}

fn skill_change(action: Action, name: &str, from: Option<&Path>) -> Change {
    Change {
        action,
        kind: ManagedKind::Skill,
        name: name.to_owned(),
        from: from.map(|path| path.to_string_lossy().into_owned()),
        keys: Vec::new(),
    }
}

#[test]
fn managed_skills_copy_references_and_remove_without_changing_the_source() {
    let tree = Tree::new();
    let source = tree.write(
        "source",
        "guide",
        &skill("guide", "Guide", "Placeholder body."),
    );
    fs::create_dir(source.join("scripts")).unwrap();
    fs::write(source.join("scripts/run"), "placeholder").unwrap();
    fs::create_dir(source.join("references")).unwrap();
    fs::write(source.join("references/guide.md"), "Placeholder reference.").unwrap();
    fs::write(source.join(".private"), "placeholder").unwrap();
    fs::create_dir(source.join("node_modules")).unwrap();
    fs::write(source.join("node_modules/unused"), "placeholder").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            source.join("scripts/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        for folder in [&source, &source.join("references")] {
            fs::set_permissions(folder, fs::Permissions::from_mode(0o555)).unwrap();
        }
    }
    assert_eq!(
        tree.manage(Action::Add, "guide", Some(&source)).unwrap(),
        "guide added"
    );
    let installed = tree.config.join("skills/guide");
    assert_eq!(
        fs::read(installed.join("references/guide.md")).unwrap(),
        b"Placeholder reference."
    );
    assert!(!installed.join(".private").exists());
    assert!(!installed.join("node_modules").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(installed.join("scripts/run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    assert_eq!(
        tree.manage(Action::Remove, "guide", None).unwrap(),
        "guide removed"
    );
    assert!(!installed.exists());
    assert!(source.join("SKILL.md").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for folder in [&source, &source.join("references")] {
            assert_eq!(
                fs::metadata(folder).unwrap().permissions().mode() & 0o777,
                0o555
            );
            fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[test]
fn invalid_skill_installs_and_duplicate_names_never_overwrite_files() {
    let tree = Tree::new();
    for name in ["", ".", "..", "../escape", "a/b", "a\\b", ".hidden"] {
        assert_eq!(
            tree.manage(Action::Add, name, None).unwrap_err(),
            "a skill name must be one folder name"
        );
    }
    assert_eq!(
        tree.manage(Action::Add, "guide", None).unwrap_err(),
        "add skill needs from: the skill's folder"
    );
    for text in [
        "Placeholder body.".to_owned(),
        "---\nname: guide\n---\n".to_owned(),
        skill("other", "Guide", "Placeholder."),
    ] {
        let source = tree.write("source", "guide", &text);
        assert!(tree.manage(Action::Add, "guide", Some(&source)).is_err());
        assert!(!tree.config.join("skills/guide").exists());
    }
    let source = tree.write(
        "source",
        "guide",
        &skill("guide", "Guide", "Placeholder original."),
    );
    assert_eq!(
        tree.manage(Action::Add, "other", Some(&source))
            .unwrap_err(),
        "SKILL.md name must match the skill name"
    );
    tree.manage(Action::Add, "guide", Some(&source)).unwrap();
    fs::write(
        source.join("SKILL.md"),
        skill("guide", "Changed", "Placeholder changed."),
    )
    .unwrap();
    assert_eq!(
        tree.manage(Action::Add, "guide", Some(&source))
            .unwrap_err(),
        "skill `guide` already exists"
    );
    assert!(
        fs::read_to_string(tree.config.join("skills/guide/SKILL.md"))
            .unwrap()
            .contains("Placeholder original.")
    );
}

#[test]
fn project_and_shared_skills_cannot_be_removed_or_shadowed_by_manage() {
    for origin in ["project", "yours"] {
        let tree = Tree::new();
        let protected = tree.write(
            origin,
            "guide",
            &skill("guide", "Guide", "Placeholder protected."),
        );
        let source = tree.write(
            "source",
            "guide",
            &skill("guide", "Guide", "Placeholder source."),
        );
        assert_eq!(
            tree.manage(Action::Remove, "guide", None).unwrap_err(),
            "skill `guide` is not in Wisp's skills folder; turn it off instead"
        );
        assert_eq!(
            tree.manage(Action::Add, "guide", Some(&source))
                .unwrap_err(),
            "skill `guide` already exists"
        );
        assert!(protected.join("SKILL.md").exists());
        tree.write(
            "config",
            "guide",
            &skill("guide", "Hidden", "Placeholder hidden."),
        );
        assert!(tree.manage(Action::Remove, "guide", None).is_err());
        assert!(tree.config.join("skills/guide/SKILL.md").exists());
    }
}

#[test]
fn overlap_and_cancellation_leave_no_partial_skill() {
    let mut tree = Tree::new();
    let source = tree.write("source", "guide", &skill("guide", "Guide", "Placeholder."));
    let host = tree.host();
    let change = skill_change(Action::Add, "guide", Some(&source));
    let call = FakeCall::default();
    let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
    for dropped in [true, false] {
        let mut first = Box::pin(manage::change(&host, &change, &call));
        assert!(first.as_mut().poll(&mut context).is_pending());
        assert!(tree.manage(Action::Remove, "guide", None).is_err());
        let staging = fs::read_dir(tree.config.join("skills"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let staging_name = staging.file_name().unwrap().to_str().unwrap();
        assert!(staging_name.starts_with('.'));
        assert_eq!(
            tree.manage(Action::Remove, staging_name, None).unwrap_err(),
            "a skill name must be one folder name"
        );
        assert!(staging.exists());
        assert_eq!(
            tree.manage(Action::Add, "guide", Some(&source)).unwrap(),
            "guide added"
        );
        let installed = tree.config.join("skills/guide/SKILL.md");
        assert!(installed.exists());
        if dropped {
            drop(first);
        } else {
            assert_eq!(block_on(first).unwrap_err(), "skill `guide` already exists");
        }
        assert!(
            installed.exists(),
            "an earlier copy must not delete a later installation"
        );
        assert!(!staging.exists());
        tree.manage(Action::Remove, "guide", None).unwrap();
        assert!(
            fs::read_dir(tree.config.join("skills"))
                .unwrap()
                .next()
                .is_none()
        );
    }
    let mut future = Box::pin(manage::change(&host, &change, &call));
    assert!(future.as_mut().poll(&mut context).is_pending());
    call.cancel.set();
    assert_eq!(
        block_on(future).unwrap_err(),
        wisp_tool_text::manage::NO_ANSWER
    );
    assert!(!tree.config.join("skills/guide").exists());
    assert!(source.join("SKILL.md").exists());
    tree.config = source.join("config");
    assert_eq!(
        block_on(manage::change(&tree.host(), &change, &FakeCall::default())).unwrap_err(),
        "the source folder cannot contain Wisp's skills folder"
    );
    assert!(!tree.config.join("skills/guide").exists());
}

#[cfg(unix)]
#[test]
fn skill_copy_refuses_nested_links_and_special_files_and_remove_only_unlinks() {
    use std::os::unix::fs::symlink;
    let tree = Tree::new();
    let source = tree.write("source", "guide", &skill("guide", "Guide", "Placeholder."));
    symlink(source.join("SKILL.md"), source.join("linked")).unwrap();
    assert_eq!(
        tree.manage(Action::Add, "guide", Some(&source))
            .unwrap_err(),
        "skill folders cannot contain symlinks or special files"
    );
    assert!(!tree.config.join("skills/guide").exists());
    fs::remove_file(source.join("linked")).unwrap();
    let socket = std::os::unix::net::UnixListener::bind(source.join("socket")).unwrap();
    assert!(tree.manage(Action::Add, "guide", Some(&source)).is_err());
    assert!(!tree.config.join("skills/guide").exists());
    drop(socket);
    fs::remove_file(source.join("socket")).unwrap();
    fs::remove_dir(tree.config.join("skills")).unwrap();
    symlink(source.parent().unwrap(), tree.config.join("skills")).unwrap();
    assert_eq!(
        tree.manage(Action::Remove, "guide", None).unwrap_err(),
        "the skills folder must not be a symlink"
    );
    fs::remove_file(tree.config.join("skills")).unwrap();
    fs::create_dir(tree.config.join("skills")).unwrap();
    symlink(&source, tree.config.join("skills/guide")).unwrap();
    tree.manage(Action::Remove, "guide", None).unwrap();
    assert!(fs::symlink_metadata(tree.config.join("skills/guide")).is_err());
    assert!(source.join("SKILL.md").exists());
    fs::create_dir(&tree.home).unwrap();
    symlink(&source, tree.home.join("guide")).unwrap();
    tree.manage(Action::Add, "guide", Some(Path::new("~/guide")))
        .unwrap();
    assert!(tree.config.join("skills/guide/SKILL.md").exists());
    tree.manage(Action::Remove, "guide", None).unwrap();
    assert!(source.join("SKILL.md").exists());
}
