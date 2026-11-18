use std::path::{Path, PathBuf};

use futures::executor::block_on;
use tempfile::TempDir;
use wisp_instructions::{ProblemKind, Scope};
use wisp_plugin_api::instructions::{LoadReport, Problem};
use wisp_plugin_api::manifest::{Manifest, Origin, SectionPlace};
use wisp_plugin_api::tools::DoctorCell;
use wisp_plugin_testkit::host::{FakeHost, Report};

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
            "home/.config/wisp",
            "home/proj/.git/objects",
            "home/proj/.git/refs",
            "home/proj/sub",
        ] {
            std::fs::create_dir_all(base.join(sub)).unwrap();
        }
        std::fs::write(base.join("home/proj/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        Self { _dir: dir, base }
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.base.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn home(&self) -> PathBuf {
        self.base.join("home")
    }

    fn host(&self, start: &str, settings: &[(&str, &str)]) -> FakeHost {
        FakeHost {
            root: self.home().join(start).display().to_string(),
            home: Some(self.home().display().to_string()),
            config_dir: Some(self.home().join(".config/wisp").display().to_string()),
            settings: settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..FakeHost::default()
        }
    }
}

fn start(host: &FakeHost) -> Rendered {
    let plugin = instantiate(Arc::new(host.clone()));
    block_on(plugin.render_section(SECTION, At::Start, Prior::None, None))
}

fn reports(host: &FakeHost) -> Vec<Report> {
    host.instructed.lock().unwrap().clone()
}

fn disk_load(start: &Path, home: &Path, source: Source, warn_tokens: u64) -> Loaded {
    struct Disk;
    impl Files for Disk {
        fn metadata(&self, path: &Path) -> io::Result<Meta> {
            HostFiles {
                host: &FakeHost::default(),
            }
            .metadata(path)
        }
        fn exists(&self, path: &Path) -> bool {
            std::fs::symlink_metadata(path).is_ok()
        }
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            std::fs::canonicalize(path)
        }
        fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
            std::fs::read(path).map(Some)
        }
    }
    wisp_instructions::load(
        &Paths {
            start: start.to_owned(),
            config_dir: Some(home.join(".config/wisp")),
            home: Some(home.to_owned()),
        },
        Options {
            source,
            warn_tokens,
        },
        &LoadAll,
        &Disk,
    )
}

#[test]
fn the_start_render_is_the_loaders_block_and_reports_the_summary_and_project_root() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Answer briefly.\n@notes.md\n");
    t.write("home/proj/notes.md", "Keep notes short.\n");
    let host = t.host("proj/sub", &[]);
    let rendered = start(&host);
    let loaded = disk_load(
        &t.home().join("proj/sub"),
        &t.home(),
        Source::Auto,
        DEFAULT_WARN_TOKENS,
    );
    assert_eq!(rendered.text, loaded.block);
    assert!(
        rendered
            .text
            .as_deref()
            .unwrap()
            .starts_with("<instructions>\n")
    );
    assert_eq!(rendered.snapshot, snapshot(&loaded.files));
    assert_eq!(rendered.snapshot.lines().count(), 2);
    let all = reports(&host);
    let [Report::Loaded(report)] = all.as_slice() else {
        panic!("{:?}", reports(&host));
    };
    assert_eq!(report.chosen, Some(Scope::Project));
    assert_eq!(report.summary, loaded.summary);
    assert_eq!(
        report.project_root.as_deref(),
        Some(t.home().join("proj").display().to_string().as_str())
    );
}

#[test]
fn the_project_root_is_reported_only_for_a_loaded_project_file_away_from_the_start() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Answer briefly.\n");
    t.write("home/.config/wisp/AGENTS.md", "Default.\n");
    let root = |host: &FakeHost| {
        start(host);
        match reports(host).first() {
            Some(Report::Loaded(LoadReport { project_root, .. })) => project_root.clone(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(root(&t.host("proj", &[])), None);
    assert_eq!(root(&t.host("proj/sub", &[(SOURCE, "default")])), None);
    assert_eq!(root(&t.host("proj/sub", &[(SOURCE, "none")])), None);
    t.write("home/proj/AGENTS.md", "\n");
    assert_eq!(root(&t.host("proj/sub", &[])), None);
}

#[test]
fn the_source_and_warning_settings_are_read_through_the_host() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", &"x".repeat(80_000));
    t.write("home/.config/wisp/AGENTS.md", "Default.\n");
    let host = t.host("proj", &[(WARN_TOKENS, "100")]);
    let rendered = start(&host);
    assert!(rendered.text.unwrap().contains("xxxx"));
    assert_eq!(
        reports(&host).last(),
        Some(&Report::Warning(Diagnostic::Large(20_000)))
    );
    let quiet = t.host("proj", &[(WARN_TOKENS, "20000")]);
    start(&quiet);
    assert_eq!(reports(&quiet).len(), 1);
    let default = t.host("proj", &[(SOURCE, "default")]);
    assert!(start(&default).text.unwrap().contains("Default.\n"));
    let none = t.host("proj", &[(SOURCE, "none")]);
    assert_eq!(start(&none).text, None);
}

#[test]
fn nothing_loads_or_reports_without_a_workspace_or_for_another_section() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Answer briefly.\n");
    let bare = FakeHost::default();
    let alone = instantiate(Arc::new(bare.clone()));
    for at in [At::Start, At::Turn, At::Head] {
        let rendered = block_on(alone.render_section(SECTION, at, Prior::None, None));
        assert_eq!(rendered, Rendered::default());
    }
    assert!(reports(&bare).is_empty());
    let host = t.host("proj", &[]);
    let plugin = instantiate(Arc::new(host.clone()));
    let other = block_on(plugin.render_section("other", At::Start, Prior::None, None));
    assert_eq!(other, Rendered::default());
    assert!(reports(&host).is_empty());
}

#[test]
fn a_file_the_workspace_does_not_call_regular_is_not_read() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Answer briefly.\n");
    let host = FakeHost {
        kind_of: Some(Arc::new(|_: &str| Some(FileKind::Fifo))),
        ..t.host("proj", &[])
    };
    assert_eq!(start(&host).text, None);
    let all = reports(&host);
    let [Report::Loaded(report)] = all.as_slice() else {
        panic!("{:?}", reports(&host));
    };
    let shown = "~/proj/AGENTS.md";
    assert_eq!(
        report.summary.diagnostics,
        [Diagnostic::Problem(Problem {
            source: Some(shown.to_owned()),
            kind: ProblemKind::NotAFile,
            message: format!(
                "{shown} isn't a regular file; it was skipped. Instructions must be regular files."
            ),
        })]
    );
}

#[test]
fn the_doctor_rows_list_the_files_and_report_the_warning() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Rules.\n@notes.md\n");
    t.write("home/proj/notes.md", &"x".repeat(1000));
    t.write("home/.config/wisp/AGENTS.md", "Default.\n");
    let host = t.host("proj/sub", &[(WARN_TOKENS, "100")]);
    let plugin = instantiate(Arc::new(host.clone()));
    let context = BuildContext {
        platform: "unix".to_owned(),
        capabilities: Vec::new(),
    };
    let rows = block_on(plugin.doctor(&context));
    let text = |t: &str| DoctorCell::Text(t.to_owned());
    let row = |key: &str, cells: Vec<DoctorCell>| DoctorRow {
        key: key.to_owned(),
        cells,
    };
    assert_eq!(
        rows,
        [
            row(
                "project",
                vec![
                    text("~/proj/AGENTS.md"),
                    DoctorCell::Bytes(Some(17)),
                    text("loads")
                ]
            ),
            row(
                "import",
                vec![
                    text("~/proj/notes.md"),
                    DoctorCell::Bytes(Some(1000)),
                    text("from AGENTS.md")
                ]
            ),
            row(
                "default",
                vec![
                    text("~/.config/wisp/AGENTS.md"),
                    DoctorCell::Bytes(Some(9)),
                    text("not used: the project has AGENTS.md")
                ]
            ),
            row("total", vec![text("1.0 KiB, about 254 tokens")]),
        ]
    );
    assert_eq!(
        reports(&host).last(),
        Some(&Report::Warning(Diagnostic::Large(254)))
    );
    let empty = Tree::new();
    let hint = block_on(instantiate(Arc::new(empty.host("proj", &[]))).doctor(&context));
    assert_eq!(hint.last().map(|r| r.key.as_str()), Some("hint"));
    assert!(block_on(instantiate(Arc::new(FakeHost::default())).doctor(&context)).is_empty());
}

#[test]
fn the_manifest_has_one_system_section_and_owns_the_two_settings() {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, "instructions");
    assert_eq!(manifest.names().collect::<Vec<_>>(), [SOURCE, WARN_TOKENS]);
    let [section] = manifest.prompt_sections.as_slice() else {
        panic!("{:?}", manifest.prompt_sections);
    };
    assert_eq!(section.name, SECTION);
    assert_eq!(section.place, SectionPlace::System);
    assert_eq!(section.sets, ["core/state"]);
    assert_eq!(section.continues, None);
}

#[test]
fn a_child_renders_the_same_block_as_its_root_in_the_same_project() {
    let tree = Tree::new();
    tree.write("home/proj/AGENTS.md", "Answer briefly.\n");
    let root = tree.host("proj/sub", &[]);
    let child = FakeHost {
        identity: Some(wisp_plugin_api::tree::Identity {
            path: "/review".to_owned(),
            name: "review".to_owned(),
            parent: Some("/".to_owned()),
            preset: Some("reviewer".to_owned()),
            depth: 1,
            session_id: "s-child".to_owned(),
            root_session_id: "s".to_owned(),
        }),
        ..tree.host("proj/sub", &[])
    };
    let rendered = start(&root);
    assert!(
        rendered
            .text
            .as_deref()
            .unwrap()
            .contains("Answer briefly.")
    );
    assert_eq!(start(&child), rendered);
    assert_eq!(reports(&child), reports(&root));
}

mod deltas;
