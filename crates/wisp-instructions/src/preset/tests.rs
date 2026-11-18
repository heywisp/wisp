use super::super::testing::Disk;
use super::*;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Dir {
    let root =
        std::env::temp_dir().join(format!("wisp-preset-loader-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("roles/planner/parts")).unwrap();
    Dir(root)
}

#[test]
fn a_preset_section_is_the_opening_line_and_its_files_named_from_their_directory() {
    let home = dir("block");
    let roles = home.0.join("roles/planner");
    std::fs::write(roles.join("AGENTS.md"), "Plan.\n@parts/style.md\n").unwrap();
    std::fs::write(roles.join("parts/style.md"), "Be short.\n").unwrap();
    let file = resolve_preset("~/roles/planner", None, Some(&home.0), &Disk).unwrap();
    assert_eq!(file, roles.join("AGENTS.md"));
    let loaded = load_preset(&file, Some(&home.0), 8_000, Holder::Child, &Disk).unwrap();
    assert_eq!(
        loaded.block,
        "<instructions>\nInstructions for your preset follow, from the file named by Preset instructions in <environment>. File names are relative to that file's directory. Your parent's messages take precedence over them.\n<file scope=\"preset\" name=\"AGENTS.md\">\nPlan.\n@parts/style.md\n</file>\n<file scope=\"preset\" name=\"parts/style.md\" imported_by=\"AGENTS.md\">\nBe short.\n</file>\n</instructions>"
    );
    assert!(!loaded.block.contains(&home.0.display().to_string()));
    assert_eq!(loaded.sources.len(), 2);
    assert!(loaded.sources.iter().all(|s| s.scope == Scope::Preset));
    assert_eq!(loaded.bytes(), 32);
    assert_eq!(loaded.large, None);
    let main = load_preset(&file, Some(&home.0), 8_000, Holder::Main, &Disk).unwrap();
    assert_eq!(
        main.block,
        loaded
            .block
            .replacen(" Your parent's messages take precedence over them.", "", 1)
    );
}

#[test]
fn a_path_resolves_relative_to_the_config_directory_never_the_working_directory() {
    let home = dir("relative");
    std::fs::write(home.0.join("roles/one.md"), "One.\n").unwrap();
    let conf = home.0.join("roles");
    assert_eq!(
        resolve_preset("one.md", Some(&conf), Some(&home.0), &Disk),
        Ok(conf.join("one.md"))
    );
    assert_eq!(
        resolve_preset("one.md", None, Some(&home.0), &Disk),
        Err("instructions one.md can't be found: there is no config.toml directory".to_owned())
    );
    assert_eq!(
        resolve_preset("~/x.md", None, None, &Disk),
        Err("instructions ~/x.md can't be found: there is no home directory".to_owned())
    );
}
