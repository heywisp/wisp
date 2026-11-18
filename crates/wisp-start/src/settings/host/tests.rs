use wisp_protocol::settings::{SavedEntry, SettingValue, site_path};

use super::*;

struct Folder(crate::scratch::ScratchDir);

impl Folder {
    fn new(name: &str) -> Self {
        Self(crate::scratch_dir(&format!("settings-host-{name}")))
    }

    fn config(&self) -> PathBuf {
        self.0.root.join("config.toml")
    }
}

fn effort(value: &str) -> SiteEdit {
    SiteEdit::Set(
        site_path("effort").unwrap(),
        SettingValue::Text(value.into()),
    )
}

#[test]
fn a_missing_file_reads_as_missing_with_nothing_saved() {
    let folder = Folder::new("missing");
    let read = read(&folder.config(), PathShown::Given, None);
    assert_eq!(read.file, FileState::Missing);
    assert!(read.saved.is_empty());
}

#[tokio::test]
async fn a_save_writes_the_file_and_reports_the_line_and_the_shown_path() {
    let folder = Folder::new("save");
    let saved = write(&folder.config(), PathShown::Given, None, &[effort("high")]).unwrap();
    assert_eq!(saved.path_display, GIVEN_CONFIG);
    assert_eq!(saved.written.len(), 1);
    assert_eq!(
        saved.written[0].value,
        Some(SettingValue::Text("high".into()))
    );
    assert!(saved.written[0].line.is_some());
    let text = std::fs::read_to_string(folder.config()).unwrap();
    assert!(text.contains("effort = \"high\""), "{text}");
    let read = read(&folder.config(), PathShown::Given, None);
    assert_eq!(
        read.file,
        FileState::Ok {
            path_display: GIVEN_CONFIG.into()
        }
    );
    assert_eq!(
        read.saved[0].entry,
        SavedEntry::Value {
            value: SettingValue::Text("high".into())
        }
    );
    let removed = write(
        &folder.config(),
        PathShown::Given,
        None,
        &[SiteEdit::Unset(site_path("effort").unwrap())],
    )
    .unwrap();
    assert_eq!(removed.written[0].value, None);
    assert_eq!(removed.written[0].line, None);
    assert!(read_saved_is_empty(&folder));
    let used_path = folder.0.root.join("settings-used.json");
    let host = FileHost {
        path: folder.config(),
        used_path: Some(used_path.clone()),
        shown: PathShown::Given,
        home: None,
    };
    assert!(host.read().await.unwrap().often.is_empty());
    host.used(vec![names::TIME.into(), names::DISPLAY_MOUSE.into()])
        .await;
    host.used(vec![
        names::TIME.into(),
        "models.example-model.effort".into(),
        "endpoints.example-account.model".into(),
    ])
    .await;
    assert_eq!(
        host.read().await.unwrap().often,
        [names::TIME, names::DISPLAY_MOUSE]
    );
    let file: std::collections::BTreeMap<String, serde_json::Value> =
        serde_json::from_slice(&std::fs::read(used_path).unwrap()).unwrap();
    assert!(
        file.keys()
            .all(|name| wisp_protocol::settings::find(name).is_some())
    );
    assert_eq!(file.len(), 2);
}

fn read_saved_is_empty(folder: &Folder) -> bool {
    read(&folder.config(), PathShown::Given, None)
        .saved
        .is_empty()
}

#[test]
fn a_broken_file_reads_as_broken_and_refuses_a_save() {
    let folder = Folder::new("broken");
    std::fs::write(folder.config(), "effort = [").unwrap();
    let read = read(&folder.config(), PathShown::Given, None);
    assert!(matches!(read.file, FileState::Broken { .. }), "{read:?}");
    assert!(write(&folder.config(), PathShown::Given, None, &[effort("low")]).is_err());
}

#[tokio::test]
async fn the_host_saves_off_the_runtime_thread() {
    let folder = Folder::new("host");
    let host = FileHost {
        used_path: Some(folder.0.root.join("settings-used.json")),
        path: folder.config(),
        shown: PathShown::Given,
        home: None,
    };
    let saved = host.save(vec![effort("low")]).await.unwrap();
    assert_eq!(saved.written[0].path, site_path("effort").unwrap());
    let read = host.read().await.unwrap();
    assert_eq!(read.saved.len(), 1);
    assert!(read.often.is_empty());
}
