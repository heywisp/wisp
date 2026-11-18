use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use wisp_tool_text::paths::shorten_path;

use super::testing::Disk;
use super::*;

struct Tree {
    _dir: TempDir,
    base: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        for sub in [
            "home",
            "home/.config/wisp",
            "proj/.git/objects",
            "proj/.git/refs",
        ] {
            std::fs::create_dir_all(base.join(sub)).unwrap();
        }
        std::fs::write(base.join("proj/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        Self { _dir: dir, base }
    }

    fn home(&self) -> PathBuf {
        self.base.join("home")
    }

    fn proj(&self) -> PathBuf {
        self.base.join("proj")
    }

    fn config(&self) -> PathBuf {
        self.home().join(".config/wisp")
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.base.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn paths_from(&self, start: &Path) -> Paths {
        Paths {
            start: start.to_owned(),
            config_dir: Some(self.config()),
            home: Some(self.home()),
        }
    }

    fn load_with(&self, start: &Path, source: Source) -> Loaded {
        load(
            &self.paths_from(start),
            Options {
                source,
                warn_tokens: DEFAULT_WARN_TOKENS,
            },
            &LoadAll,
            &Disk,
        )
    }

    fn load(&self) -> Loaded {
        self.load_with(&self.proj(), Source::Auto)
    }
}

fn names(loaded: &Loaded) -> Vec<&str> {
    loaded.files.iter().map(|f| f.name.as_str()).collect()
}

fn problems(loaded: &Loaded) -> Vec<&Problem> {
    loaded
        .summary
        .diagnostics
        .iter()
        .filter_map(|d| match d {
            Diagnostic::Problem(problem) => Some(problem),
            Diagnostic::Large(_) => None,
        })
        .collect()
}

fn kinds(loaded: &Loaded) -> Vec<ProblemKind> {
    problems(loaded).into_iter().map(|p| p.kind).collect()
}

fn within_a_second<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(1))
        .expect("finished within a second")
}

#[test]
fn the_project_file_alone_loads() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "No comments.");
    let loaded = t.load();
    assert_eq!(loaded.chosen, Some(Scope::Project));
    assert_eq!(
        loaded.block(),
        Some(
            "<instructions>\nInstructions for this project follow, from its AGENTS.md. File names are relative to the project root. The user's messages take precedence over them.\n<file scope=\"project\" name=\"AGENTS.md\">\nNo comments.\n</file>\n</instructions>"
        )
    );
    let chosen = loaded.summary.chosen.as_ref().unwrap();
    assert_eq!(chosen.name, "AGENTS.md");
    assert_eq!(chosen.bytes, 12);
    assert_eq!(chosen.sha256, sha256_hex("No comments.\n"));
    assert!(loaded.summary.diagnostics.is_empty());
}

#[test]
fn the_default_loads_when_the_project_has_none() {
    let t = Tree::new();
    t.write("home/.config/wisp/AGENTS.md", "Be brief.\n");
    let loaded = t.load();
    assert_eq!(loaded.chosen, Some(Scope::Default));
    assert_eq!(
        loaded.block(),
        Some(
            "<instructions>\nThe user's default instructions follow. File names are relative to the user's Wisp config directory. The user's messages take precedence over them.\n<file scope=\"default\" name=\"AGENTS.md\">\nBe brief.\n</file>\n</instructions>"
        )
    );
    assert_eq!(
        loaded.summary.chosen.as_ref().unwrap().display_path,
        "~/.config/wisp/AGENTS.md"
    );
}

#[test]
fn the_project_wins_and_the_default_is_not_read() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "project\n");
    t.write("home/.config/wisp/AGENTS.md", "default\n");
    let loaded = t.load();
    assert_eq!(names(&loaded), ["AGENTS.md"]);
    assert_eq!(loaded.files[0].scope, Scope::Project);
    assert!(!loaded.block().unwrap().contains("default\n"));
    let default = &loaded.summary.candidates[1];
    assert_eq!(default.scope, Scope::Default);
    assert_eq!(default.bytes, Some(8));
    assert_eq!(default.sha256, None, "only stat, never read");
    assert_eq!(
        loaded.summary.candidates[0].sha256.as_deref(),
        Some(sha256_hex("project\n").as_str())
    );
}

#[test]
fn with_neither_file_nothing_loads_and_nothing_is_created() {
    let t = Tree::new();
    let loaded = t.load();
    assert_eq!(loaded.chosen, None);
    assert_eq!(loaded.block(), None);
    assert!(loaded.summary.chosen.is_none());
    assert_eq!(loaded.summary.candidates.len(), 2);
    assert!(loaded.summary.candidates.iter().all(|c| c.bytes.is_none()));
    assert!(!t.config().join("AGENTS.md").exists());
}

#[test]
fn an_empty_project_file_opts_out_of_the_default() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "\u{feff}  \n\n");
    t.write("home/.config/wisp/AGENTS.md", "default\n");
    let loaded = t.load();
    assert_eq!(loaded.chosen, Some(Scope::Project));
    assert_eq!(loaded.block(), None);
    assert_eq!(names(&loaded), ["AGENTS.md"]);
    assert!(loaded.summary.chosen.is_some());
    assert!(loaded.summary.diagnostics.is_empty());
}

#[test]
fn a_broken_project_file_is_a_diagnostic_and_the_default_is_not_used() {
    let t = Tree::new();
    std::fs::write(t.proj().join("AGENTS.md"), b"bad \xff\n").unwrap();
    t.write("home/.config/wisp/AGENTS.md", "default\n");
    let loaded = t.load();
    assert_eq!(loaded.chosen, Some(Scope::Project));
    assert_eq!(loaded.block(), None);
    assert!(loaded.files.is_empty());
    assert_eq!(kinds(&loaded), [ProblemKind::NotUtf8]);
    let diagnostic = problems(&loaded)[0];
    let shown = shorten_path(&t.proj().join("AGENTS.md"), Some(&t.home()));
    assert_eq!(diagnostic.source.as_deref(), Some(shown.as_str()));
    assert_eq!(
        diagnostic.message,
        format!("{shown} isn't valid UTF-8; it was skipped. Save it as UTF-8.")
    );
}

#[test]
fn the_source_setting_picks_the_default_or_nothing() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "project\n");
    t.write("home/.config/wisp/AGENTS.md", "default\n");
    let default = t.load_with(&t.proj(), Source::Default);
    assert_eq!(default.chosen, Some(Scope::Default));
    assert!(default.block().unwrap().contains("default\n"));
    let none = t.load_with(&t.proj(), Source::None);
    assert_eq!(none.chosen, None);
    assert_eq!(none.block(), None);
    assert!(none.files.is_empty());
    std::fs::remove_file(t.config().join("AGENTS.md")).unwrap();
    assert_eq!(t.load_with(&t.proj(), Source::Default).chosen, None);
}

#[test]
fn the_default_lives_under_xdg_config_home_or_home() {
    let home = Path::new("/home/u");
    assert_eq!(
        Paths::config_dir_from(None, Some(home)),
        Some(PathBuf::from("/home/u/.config/wisp"))
    );
    assert_eq!(
        Paths::config_dir_from(Some("/xdg".into()), Some(home)),
        Some(PathBuf::from("/xdg/wisp"))
    );
    assert_eq!(
        Paths::config_dir_from(Some("rel".into()), Some(home)),
        Some(PathBuf::from("/home/u/.config/wisp"))
    );
    assert_eq!(Paths::config_dir_from(None, Some(Path::new("rel"))), None);
    assert_eq!(Paths::config_dir_from(None, None), None);
    let t = Tree::new();
    let xdg = t.base.join("xdg");
    t.write("xdg/wisp/AGENTS.md", "from xdg\n");
    let loaded = load(
        &Paths {
            start: t.proj(),
            config_dir: Paths::config_dir_from(Some(xdg), Some(&t.home())),
            home: Some(t.home()),
        },
        Options {
            source: Source::Auto,
            warn_tokens: DEFAULT_WARN_TOKENS,
        },
        &LoadAll,
        &Disk,
    );
    assert!(loaded.block().unwrap().contains("from xdg\n"));
}

#[test]
fn the_root_is_the_nearest_directory_holding_git() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "root rules\n");
    t.write("proj/crates/AGENTS.md", "between\n");
    t.write("proj/crates/core/AGENTS.md", "start\n");
    t.write("proj/crates/core/sub/AGENTS.md", "below\n");
    t.write("AGENTS.md", "above\n");
    let start = t.proj().join("crates/core");
    let loaded = t.load_with(&start, Source::Auto);
    assert_eq!(loaded.root, t.proj());
    assert_eq!(names(&loaded), ["AGENTS.md"]);
    assert!(loaded.block().unwrap().contains("root rules\n"));
    for other in ["between", "start", "below", "above"] {
        assert!(!loaded.block().unwrap().contains(other), "{other}");
    }
    assert_eq!(loaded.project_root(), Some(t.proj().as_path()));
    assert_eq!(t.load().project_root(), None);

    t.write("proj/wt/.git", "gitdir: ../.git/worktrees/wt\n");
    t.write("proj/wt/AGENTS.md", "worktree\n");
    let worktree = t.load_with(&t.proj().join("wt/src"), Source::Auto);
    assert_eq!(worktree.root, t.proj().join("wt"));
    assert!(worktree.block().unwrap().contains("worktree\n"));

    let outside = t.base.join("loose/dir");
    std::fs::create_dir_all(&outside).unwrap();
    t.write("loose/dir/AGENTS.md", "loose\n");
    let loose = t.load_with(&outside, Source::Auto);
    assert_eq!(loose.root, outside);
    assert!(loose.block().unwrap().contains("loose\n"));
}

#[test]
fn only_a_real_git_directory_or_gitdir_file_marks_a_repository() {
    let t = Tree::new();
    assert!(is_repository(&t.proj(), &Disk));

    std::fs::create_dir_all(t.base.join("proj/empty/.git")).unwrap();
    std::fs::create_dir_all(t.base.join("proj/empty/sub")).unwrap();
    assert!(!is_repository(&t.base.join("proj/empty"), &Disk));
    assert_eq!(
        project_root(&t.base.join("proj/empty/sub"), &Disk),
        t.proj()
    );

    t.write("proj/head_only/.git/HEAD", "ref: refs/heads/main\n");
    assert!(!is_repository(&t.base.join("proj/head_only"), &Disk));

    t.write("proj/linked/.git", "gitdir: ../.git/worktrees/linked\n");
    assert!(is_repository(&t.base.join("proj/linked"), &Disk));

    let huge = "gitdir: ".to_owned() + &"x".repeat(GITFILE_MAX_BYTES as usize - 7);
    assert_eq!(huge.len() as u64, GITFILE_MAX_BYTES + 1);
    t.write("proj/huge/.git", &huge);
    assert!(!is_repository(&t.base.join("proj/huge"), &Disk));

    t.write("proj/other/.git", "not a gitfile\n");
    std::fs::create_dir_all(t.base.join("proj/other/sub")).unwrap();
    assert!(!is_repository(&t.base.join("proj/other"), &Disk));
    assert_eq!(
        project_root(&t.base.join("proj/other/sub"), &Disk),
        t.proj()
    );

    std::fs::create_dir_all(t.base.join("loose/.git")).unwrap();
    std::fs::create_dir_all(t.base.join("loose/dir")).unwrap();
    let loose = t.base.join("loose/dir");
    assert_eq!(project_root(&loose, &Disk), loose);
}

#[test]
fn other_file_names_are_never_read_or_listed() {
    let t = Tree::new();
    for name in ["CLAUDE.md", "CLAUDE.local.md", "AGENTS.override.md"] {
        t.write(&format!("proj/{name}"), "CANARY\n");
    }
    let loaded = t.load();
    assert_eq!(loaded.chosen, None);
    let shown = format!("{:?}", loaded.summary);
    assert!(
        !shown.contains("CLAUDE") && !shown.contains("override"),
        "{shown}"
    );
    t.write("proj/AGENTS.md", "@CLAUDE.md\n");
    let imported = t.load();
    assert_eq!(names(&imported), ["AGENTS.md", "CLAUDE.md"]);
    assert!(imported.block().unwrap().contains("CANARY"));
}

#[test]
fn reading_normalises_without_cutting() {
    let t = Tree::new();
    let big = "x".repeat(2 * 1024 * 1024 - 1) + "\n";
    t.write("proj/AGENTS.md", &big);
    let loaded = t.load();
    assert_eq!(loaded.files[0].text.len(), big.len());
    assert_eq!(
        loaded.summary.chosen.as_ref().unwrap().bytes,
        big.len() as u64
    );

    t.write("proj/AGENTS.md", "\u{feff}a\r\nb\rc");
    let loaded = t.load();
    assert_eq!(loaded.files[0].text, "a\nb\nc\n");
}

#[cfg(unix)]
#[test]
fn a_symlinked_agents_md_is_followed() {
    let t = Tree::new();
    let target = t.write("dotfiles/AGENTS.md", "linked rules\n@docs/style.md\n");
    t.write("dotfiles/docs/style.md", "wrong style\n");
    t.write("proj/docs/style.md", "project style\n");
    std::os::unix::fs::symlink(&target, t.proj().join("AGENTS.md")).unwrap();
    let loaded = t.load();
    let block = loaded.block().unwrap();
    assert!(block.contains("linked rules\n"));
    assert!(block.contains("project style\n"), "{block}");
    assert!(!block.contains("wrong style"));
    assert_eq!(names(&loaded), ["AGENTS.md", "docs/style.md"]);
    assert!(!loaded.files[1].external);
}

#[cfg(unix)]
#[test]
fn files_that_are_not_regular_are_skipped_without_hanging() {
    for setup in ["dir", "fifo", "zero", "stdin"] {
        let t = Tree::new();
        let path = t.proj().join("AGENTS.md");
        match setup {
            "dir" => std::fs::create_dir(&path).unwrap(),
            "fifo" => rustix::fs::mknodat(
                rustix::fs::CWD,
                &path,
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_raw_mode(0o600),
                0,
            )
            .unwrap(),
            "zero" => std::os::unix::fs::symlink("/dev/zero", &path).unwrap(),
            _ => std::os::unix::fs::symlink("/dev/stdin", &path).unwrap(),
        }
        let loaded = within_a_second(move || t.load());
        assert_eq!(kinds(&loaded), [ProblemKind::NotAFile], "{setup}");
        assert_eq!(loaded.block(), None);
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_loop_is_unreadable() {
    let t = Tree::new();
    std::os::unix::fs::symlink(t.proj().join("b"), t.proj().join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(t.proj().join("AGENTS.md"), t.proj().join("b")).unwrap();
    let loaded = t.load();
    assert_eq!(kinds(&loaded), [ProblemKind::Unreadable]);
    assert_eq!(loaded.chosen, Some(Scope::Project));

    let dangling = Tree::new();
    std::os::unix::fs::symlink(
        dangling.proj().join("gone"),
        dangling.proj().join("AGENTS.md"),
    )
    .unwrap();
    assert_eq!(kinds(&dangling.load()), [ProblemKind::Unreadable]);
}

#[test]
fn imports_load_after_their_importer_depth_first() {
    let t = Tree::new();
    t.write(
        "proj/AGENTS.md",
        "Top. Follow @docs/style.md.\n@docs/b.md\n",
    );
    t.write("proj/docs/style.md", "style @inner.md\n");
    t.write("proj/docs/inner.md", "inner\n");
    t.write("proj/docs/b.md", "b\n");
    let loaded = t.load();
    assert_eq!(
        names(&loaded),
        ["AGENTS.md", "docs/style.md", "docs/inner.md", "docs/b.md"]
    );
    let block = loaded.block().unwrap();
    assert!(block.contains("Top. Follow @docs/style.md.\n"));
    assert!(block.contains(
        "<file scope=\"project\" name=\"docs/style.md\" imported_by=\"AGENTS.md\">\nstyle @inner.md\n</file>\n"
    ));
    assert!(block.contains("name=\"docs/inner.md\" imported_by=\"docs/style.md\""));
    let imports = &loaded.summary.imports;
    assert_eq!(imports.len(), 3);
    assert_eq!(imports[0].imported_by.as_deref(), Some("AGENTS.md"));
    assert!(imports.iter().all(|i| !i.external));
}

#[test]
fn home_and_absolute_imports_are_named_by_rule() {
    let t = Tree::new();
    t.write(
        "home/notes/team-rules.md",
        "team @style/extra.md @../b.md\n",
    );
    t.write("home/notes/style/extra.md", "extra\n");
    t.write("home/b.md", "b\n");
    t.write("proj/docs/x.md", "x\n");
    t.write("shared/x.md", "shared\n");
    let inside = t.proj().join("docs/x.md");
    t.write(
        "proj/AGENTS.md",
        &format!("@~/notes/team-rules.md\n@{}\n", inside.display()),
    );
    t.write("proj/docs/style.md", "@../../shared/x.md\n");
    let loaded = t.load();
    assert_eq!(
        names(&loaded),
        [
            "AGENTS.md",
            "~/notes/team-rules.md",
            "~/notes/style/extra.md",
            "~/b.md",
            "docs/x.md"
        ]
    );
    assert!(loaded.files[1].external);
    assert!(!loaded.files[4].external);
    let block = loaded.block().unwrap();
    assert!(!block.contains(&t.base.display().to_string()), "{block}");
    assert!(block.contains("external=\"true\""));

    t.write("proj/AGENTS.md", "@docs/style.md\n");
    let relative = t.load();
    assert_eq!(
        names(&relative),
        ["AGENTS.md", "docs/style.md", "../shared/x.md"]
    );
    assert!(relative.files[2].external);
}

#[test]
fn a_tilde_anchor_is_never_popped() {
    let t = Tree::new();
    t.write("home/a.md", "@../b.md\n");
    t.write("b.md", "outside home\n");
    t.write("proj/AGENTS.md", "@~/a.md\n");
    let loaded = t.load();
    assert_eq!(names(&loaded), ["AGENTS.md", "~/a.md", "~/../b.md"]);
    assert_eq!(normalise_name("/../x"), "/x");
    assert_eq!(normalise_name("docs/../../shared/x.md"), "../shared/x.md");
    assert_eq!(normalise_name("~/a/./b/../c"), "~/a/c");
}

#[test]
fn the_defaults_imports_are_named_from_the_config_directory_and_never_external() {
    let t = Tree::new();
    t.write(
        "home/.config/wisp/AGENTS.md",
        "@rules/tone.md\n@~/elsewhere.md\n",
    );
    t.write("home/.config/wisp/rules/tone.md", "tone\n");
    t.write("home/elsewhere.md", "elsewhere\n");
    let loaded = t.load();
    assert_eq!(
        names(&loaded),
        ["AGENTS.md", "rules/tone.md", "~/elsewhere.md"]
    );
    assert!(loaded.files.iter().all(|f| !f.external));
    let block = loaded.block().unwrap();
    assert!(!block.contains("<file scope=\"default\" name=\"AGENTS.md\""));
    assert!(block.contains(
        "<file scope=\"default\" name=\"rules/tone.md\" imported_by=\"AGENTS.md\">\ntone\n</file>"
    ));
}

#[cfg(unix)]
#[test]
fn a_symlink_cannot_hide_a_file_from_outside_the_repository() {
    let t = Tree::new();
    let secret = t.write("home/secrets.md", "secret\n");
    std::fs::create_dir_all(t.proj().join("docs")).unwrap();
    std::os::unix::fs::symlink(&secret, t.proj().join("docs/x")).unwrap();
    t.write("proj/AGENTS.md", "@docs/x\n");
    let loaded = t.load();
    assert_eq!(names(&loaded), ["AGENTS.md", "docs/x"]);
    assert!(loaded.files[1].external);
    assert!(loaded.summary.imports[0].external);
}

#[test]
fn code_and_mail_addresses_are_not_imports() {
    let t = Tree::new();
    t.write(
        "proj/AGENTS.md",
        "Mail maintainer@example.com. Write `@README`.\n```\n@fenced.md\n```\n@\"quoted.md\"\n@my\\ file.md\n",
    );
    t.write("proj/README", "readme\n");
    t.write("proj/fenced.md", "fenced\n");
    t.write("proj/quoted.md", "quoted\n");
    t.write("proj/my file.md", "spaced\n");
    let loaded = t.load();
    assert_eq!(names(&loaded), ["AGENTS.md", "my file.md"]);
}

#[test]
fn an_escaped_full_stop_stays_in_the_path() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "See @docs/end\\.\n");
    t.write("proj/docs/end.", "end\n");
    assert_eq!(names(&t.load()), ["AGENTS.md", "docs/end."]);
}

#[test]
fn depth_four_loads_and_depth_five_is_a_diagnostic() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "@d1.md\n");
    for depth in 1..=5 {
        t.write(
            &format!("proj/d{depth}.md"),
            &format!("d{depth} @d{}.md\n", depth + 1),
        );
    }
    let loaded = t.load();
    assert_eq!(
        names(&loaded),
        ["AGENTS.md", "d1.md", "d2.md", "d3.md", "d4.md"]
    );
    assert_eq!(kinds(&loaded), [ProblemKind::ImportTooDeep]);
}

#[test]
fn cycles_and_repeats_load_once() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "@a.md @b.md @a.md\n");
    t.write("proj/a.md", "a @AGENTS.md @b.md\n");
    t.write("proj/b.md", "b @./a.md\n");
    let loaded = t.load();
    assert_eq!(names(&loaded), ["AGENTS.md", "a.md", "b.md"]);
    assert!(loaded.summary.diagnostics.is_empty());
}

#[test]
fn a_cycle_back_past_the_depth_limit_ends_quietly() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "top @a.md\n");
    t.write("proj/a.md", "a @b.md\n");
    t.write("proj/b.md", "b @c.md\n");
    t.write("proj/c.md", "c @d.md\n");
    t.write("proj/d.md", "d @AGENTS.md\n");
    let loaded = t.load();
    assert_eq!(
        names(&loaded),
        ["AGENTS.md", "a.md", "b.md", "c.md", "d.md"]
    );
    assert!(
        loaded.summary.diagnostics.is_empty(),
        "{:#?}",
        loaded.summary.diagnostics
    );
}

#[test]
fn a_missing_import_is_a_diagnostic_and_the_rest_loads() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "rules @gone.md @here.md\n");
    t.write("proj/here.md", "here\n");
    let loaded = t.load();
    assert_eq!(names(&loaded), ["AGENTS.md", "here.md"]);
    assert_eq!(kinds(&loaded), [ProblemKind::ImportMissing]);
    t.write("proj/AGENTS.md", "rules @~/x.md\n");
    let without_home = load(
        &Paths {
            start: t.proj(),
            config_dir: None,
            home: None,
        },
        Options {
            source: Source::Auto,
            warn_tokens: DEFAULT_WARN_TOKENS,
        },
        &LoadAll,
        &Disk,
    );
    assert_eq!(without_home.summary.candidates.len(), 1);
    assert_eq!(kinds(&without_home), [ProblemKind::ImportMissing]);
    assert_eq!(problems(&without_home)[0].source.as_deref(), Some("~/x.md"));
}

#[test]
fn a_file_of_only_imports_has_no_section_of_its_own() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "@docs/agents/rules.md\n\n");
    t.write("proj/docs/agents/rules.md", "rules\n");
    let loaded = t.load();
    let block = loaded.block().unwrap();
    assert!(!block.contains("name=\"AGENTS.md\""));
    assert!(block.contains(
        "<file scope=\"project\" name=\"docs/agents/rules.md\" imported_by=\"AGENTS.md\">"
    ));
}

#[cfg(unix)]
#[test]
fn device_imports_are_not_files() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", "rules @/dev/stdin @/dev/zero\n");
    let loaded = within_a_second(move || t.load());
    assert_eq!(
        kinds(&loaded),
        [ProblemKind::NotAFile, ProblemKind::NotAFile]
    );
    assert_eq!(names(&loaded), ["AGENTS.md"]);
}

#[test]
fn large_instructions_report_their_token_count() {
    let t = Tree::new();
    t.write("proj/AGENTS.md", &"x".repeat(80_000));
    let loaded = t.load();
    assert_eq!(loaded.large, Some(20_000));
    assert!(loaded.summary.diagnostics.is_empty());
    assert_eq!(loaded.files[0].text.len(), 80_001, "nothing is cut");
    assert_eq!(large_tokens(32_000, 8_000), None);
    assert_eq!(large_tokens(32_004, 8_000), Some(8_001));
    assert_eq!(large_tokens(80_004, 20_001), None);
}

#[test]
fn the_hash_is_lowercase_hex_sha256() {
    assert_eq!(
        sha256_hex(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex("caf\u{e9} \u{2603}"),
        "ce335642bab85530c756f72c1afb3bfc2067dd39b92e2368e4475c8dbeacc48e"
    );
}

#[test]
fn two_clones_at_different_paths_give_the_same_block() {
    let a = Tree::new();
    let b = Tree::new();
    for t in [&a, &b] {
        t.write("proj/AGENTS.md", "rules @docs/style.md\n");
        t.write("proj/docs/style.md", "style\n");
    }
    let (la, lb) = (a.load(), b.load());
    assert_eq!(la.block(), lb.block());
    assert!(la.block().is_some());
}

#[test]
fn every_file_passes_through_the_policy() {
    struct Count(std::sync::Mutex<Vec<(Scope, bool, bool)>>);
    impl SourcePolicy for Count {
        fn admit(&self, source: &Candidate<'_>) -> Admission {
            self.0.lock().unwrap().push((
                source.scope,
                source.external,
                source.imported_by.is_some(),
            ));
            Admission::Load
        }
    }
    let t = Tree::new();
    t.write("proj/AGENTS.md", "@a.md @~/b.md\n");
    t.write("proj/a.md", "a\n");
    t.write("home/b.md", "b\n");
    let policy = Count(std::sync::Mutex::new(Vec::new()));
    load(
        &t.paths_from(&t.proj()),
        Options {
            source: Source::Auto,
            warn_tokens: DEFAULT_WARN_TOKENS,
        },
        &policy,
        &Disk,
    );
    assert_eq!(
        *policy.0.lock().unwrap(),
        [
            (Scope::Project, false, false),
            (Scope::Project, false, true),
            (Scope::Project, true, true)
        ]
    );
}

#[test]
fn the_block_keeps_names_as_they_are_on_disk() {
    let t = Tree::new();
    let esc = "x\u{1b}[31m.md";
    let del = "y\u{7f}\u{9b}.md";
    t.write("proj/AGENTS.md", &format!("@{esc}\n@{del}\n"));
    t.write(&format!("proj/{esc}"), "red\n");
    t.write(&format!("proj/{del}"), "del\n");
    let loaded = t.load();
    assert_eq!(names(&loaded), ["AGENTS.md", esc, del]);
    let block = loaded.block().unwrap();
    assert!(block.contains(&format!("name=\"{esc}\"")));
    assert!(block.contains(&format!("name=\"{del}\"")));
}
