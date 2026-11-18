use std::fs;
use std::path::PathBuf;

use wisp_plugin_api::index::SEARCH_CAPABILITY;
use wisp_plugin_testkit::host::FakeHost;

pub(super) struct Tree {
    pub(super) _temp: tempfile::TempDir,
    pub(super) project: PathBuf,
    pub(super) home: PathBuf,
    pub(super) config: PathBuf,
}

impl Tree {
    pub(super) fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        Self {
            project: temp.path().join("project"),
            home: temp.path().join("home"),
            config: temp.path().join("config"),
            _temp: temp,
        }
    }

    pub(super) fn host(&self) -> FakeHost {
        FakeHost {
            root: self.project.display().to_string(),
            home: Some(self.home.display().to_string()),
            config_dir: Some(self.config.display().to_string()),
            capabilities: vec![SEARCH_CAPABILITY.to_owned()],
            ..Default::default()
        }
    }

    pub(super) fn write(&self, source: &str, name: &str, text: &str) -> PathBuf {
        let root = match source {
            "project" => self.project.join(".agents/skills"),
            "yours" => self.home.join(".agents/skills"),
            "config" => self.config.join("skills"),
            "source" => self._temp.path().join("source"),
            _ => unreachable!(),
        };
        let folder = root.join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("SKILL.md"), text).unwrap();
        folder
    }
}

pub(super) fn skill(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n")
}
