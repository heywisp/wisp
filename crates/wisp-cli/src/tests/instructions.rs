use std::path::PathBuf;

use wisp_core::instructions::DEFAULT_WARN_TOKENS;
use wisp_protocol::InstructionsSource;
use wisp_start::instructions::{paths, settings};

use super::*;

struct Tree {
    _dir: crate::scratch::ScratchDir,
    base: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let dir = crate::scratch_dir("instructions");
        let base = dir.root.clone();
        wisp_core::instructions::make_repository(&base.join("home/p")).unwrap();
        Self { _dir: dir, base }
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.base.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn rows(&self, source: InstructionsSource) -> Vec<(String, String)> {
        pairs(&self.doctor(source))
    }

    fn doctor(&self, source: InstructionsSource) -> Vec<wisp_protocol::DoctorRow> {
        let paths = paths(self.base.join("home/p"), None, Some(self.base.join("home")));
        doctor_rows(
            &crate::tools::registry().0,
            &paths,
            &Settings {
                source,
                warn_tokens: DEFAULT_WARN_TOKENS,
            },
            wisp_core::pricing::model_info("claude-opus-5-5").map(|info| ("claude-opus-5-5", info)),
        )
        .unwrap()
    }
}

fn pairs(rows: &[wisp_protocol::DoctorRow]) -> Vec<(String, String)> {
    rows.iter()
        .zip(wisp_render::doctor::table(rows, 0))
        .map(|(row, line)| {
            let value = line.strip_prefix(&format!("{} ", row.key)).unwrap_or("");
            (row.key.clone(), value.to_owned())
        })
        .collect()
}
#[test]
fn a_scratch_config_never_moves_the_default() {
    let t = Tree::new();
    t.write("xdg/wisp/AGENTS.md", "from xdg\n");
    t.write("other/config.toml", "");
    let paths = paths(
        t.base.join("home/p"),
        Some(t.base.join("xdg")),
        Some(t.base.join("home")),
    );
    assert_eq!(paths.config_dir, Some(t.base.join("xdg/wisp")));
    let rows = doctor_rows(
        &crate::tools::registry().0,
        &paths,
        &settings(None, None, None),
        wisp_core::pricing::model_info("claude-opus-5-5").map(|info| ("claude-opus-5-5", info)),
    )
    .unwrap();
    let rows = pairs(&rows);
    let shown = format!("{}/xdg/wisp/AGENTS.md", t.base.display());
    assert!(
        rows.iter()
            .any(|(k, v)| k == "default" && v.starts_with(&shown) && v.ends_with("loads")),
        "{rows:#?}"
    );
}

#[test]
fn doctor_names_what_loads_and_why() {
    let t = Tree::new();
    t.write(
        "home/p/AGENTS.md",
        "rules @docs/style.md @~/notes/team.md\n",
    );
    t.write("home/p/docs/style.md", "style\n");
    t.write("home/notes/team.md", "team\n");
    t.write("home/.config/wisp/AGENTS.md", "default\n");
    let rows = t.rows(InstructionsSource::Auto);
    let text: Vec<String> = rows.iter().map(|(k, v)| format!("{k} {v}")).collect();
    assert_eq!(text.len(), 5, "{text:#?}");
    assert!(text[0].starts_with("project ~/p/AGENTS.md"), "{text:#?}");
    assert!(text[0].ends_with("loads"), "{text:#?}");
    assert!(text[1].starts_with("import ~/p/docs/style.md"));
    assert!(text[1].ends_with("from AGENTS.md"));
    assert!(text[2].ends_with("from AGENTS.md, outside the repository"));
    assert!(text[3].starts_with("default ~/.config/wisp/AGENTS.md"));
    assert!(text[3].ends_with("not used: the project has AGENTS.md"));
    assert!(text[4].starts_with("total "), "{text:#?}");
    assert!(text[4].ends_with("about 12 tokens"), "{text:#?}");
}

#[test]
fn doctor_says_missing_and_hints_at_the_default() {
    let t = Tree::new();
    let rows = t.rows(InstructionsSource::Auto);
    assert_eq!(rows.len(), 3, "{rows:#?}");
    assert!(rows[0].1.ends_with("missing"));
    assert!(rows[1].1.ends_with("missing"));
    assert_eq!(rows[2].0, "hint");
    assert!(rows[2].1.contains("~/.config/wisp/AGENTS.md"));
    let gap = " ".repeat(3 + 9 + 3);
    assert_eq!(
        doctor_text(Some(t.doctor(InstructionsSource::Auto)), None),
        [
            "",
            "instructions",
            &format!("  project   ~/p/AGENTS.md{}{gap}missing", " ".repeat(11)),
            &format!("  default   ~/.config/wisp/AGENTS.md{gap}missing"),
            "  hint      write ~/.config/wisp/AGENTS.md for instructions in every project without its own AGENTS.md",
            "",
        ]
        .join("\n")
    );
}

#[test]
fn doctor_shows_diagnostics_on_their_line_and_the_large_warning() {
    let t = Tree::new();
    std::fs::write(t.base.join("home/p/AGENTS.md"), b"\xff").unwrap();
    let broken = t.rows(InstructionsSource::Auto);
    assert!(
        broken[0]
            .1
            .ends_with("isn't valid UTF-8; it was skipped. Save it as UTF-8.")
    );
    t.write(
        "home/p/AGENTS.md",
        &format!("{}\n@gone.md\n", "x".repeat(40_000)),
    );
    let rows = t.rows(InstructionsSource::Auto);
    assert!(
        rows.iter()
            .any(|(k, v)| k == "import" && v.contains("doesn't exist"))
    );
    let warning = rows.iter().find(|(k, _)| k == "warning").unwrap();
    assert!(warning.1.starts_with("instructions are large: about 10,"));
}

#[test]
fn doctor_says_when_a_setting_leaves_a_file_unused() {
    let t = Tree::new();
    t.write("home/p/AGENTS.md", "rules\n");
    t.write("home/.config/wisp/AGENTS.md", "default\n");
    let none = t.rows(InstructionsSource::None);
    assert!(none[0].1.ends_with("not used: --instructions none"));
    let default = t.rows(InstructionsSource::Default);
    assert!(default[0].1.ends_with("not used: --instructions default"));
    assert!(default[1].1.ends_with("loads"));
    t.write("home/p/AGENTS.md", "");
    let empty = t.rows(InstructionsSource::Auto);
    assert!(empty[0].1.contains("empty"), "{empty:#?}");
}

#[test]
fn a_file_of_only_a_missing_import_is_not_called_empty() {
    let t = Tree::new();
    t.write("home/p/AGENTS.md", "@missing.md\n");
    let rows = t.rows(InstructionsSource::Auto);
    assert!(rows[0].1.ends_with("loads"), "{rows:#?}");
    assert!(
        rows.iter()
            .any(|(k, v)| k == "import" && v.contains("doesn't exist")),
        "{rows:#?}"
    );
}

#[test]
fn doctor_cleans_control_characters_from_names() {
    let t = Tree::new();
    t.write("home/p/AGENTS.md", "@x\u{1b}[31m.md\n");
    t.write("home/p/x\u{1b}[31m.md", "red\n");
    let rows = t.doctor(InstructionsSource::Auto);
    assert_eq!(rows.iter().filter(|row| row.key == "import").count(), 1);
    let text = doctor_text(Some(rows), None);
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.starts_with("\ninstructions\n"));
}

#[test]
fn the_doctor_section_keeps_its_bytes() {
    let t = Tree::new();
    t.write(
        "home/p/AGENTS.md",
        "rules\n@docs/a.md\n@../shared.md\n@gone.md\n",
    );
    t.write("home/p/docs/a.md", &"a".repeat(3000));
    t.write("home/shared.md", "shared\n");
    let text = doctor_text(Some(t.doctor(InstructionsSource::Auto)), None);
    assert_eq!(
        text,
        [
            "",
            "instructions",
            "  project   ~/p/AGENTS.md                0.0 KiB   loads",
            "  import    ~/p/docs/a.md                2.9 KiB   from AGENTS.md",
            "  import    ~/shared.md                  0.0 KiB   from AGENTS.md, outside the repository",
            "  default   ~/.config/wisp/AGENTS.md               missing",
            "  import    ~/p/gone.md                            ~/p/gone.md, imported by AGENTS.md, doesn't exist; the import was skipped.",
            "  total     3.0 KiB, about 761 tokens",
            "",
        ]
        .join("\n")
    );
}
