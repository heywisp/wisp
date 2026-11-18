use wisp_protocol::RequestId;
use wisp_protocol::settings::{
    FileState, Layer, ModelEntry, Place, ResolvedSetting, SavedValue, SettingName, SettingValue,
    SettingsSnapshot, Shadowed, SitePath, SnapshotContext, SwitchQuote, SwitchTarget, find,
};

use super::scrubbed;

const KEY: &str = "sk-ant-api03-CANARYMODEL0000";

fn snapshot() -> SettingsSnapshot {
    let model = find("model").unwrap();
    let window = find("context_window").unwrap();
    SettingsSnapshot {
        often: Vec::new(),
        answers: RequestId(1),
        context: SnapshotContext {
            endpoint: "manifest".to_owned(),
            model: KEY.to_owned(),
            agent: None,
            max_tokens: 0,
            window: None,
            room: None,
        },
        settings: vec![ResolvedSetting {
            name: SettingName::of(model),
            value: Some(SettingValue::Text(KEY.to_owned())),
            layer: Layer::Flag,
            origin: format!("--model {KEY}"),
            shadowed: vec![Shadowed {
                layer: Layer::Config,
                value: SettingValue::Text(KEY.to_owned()),
                origin: "model".to_owned(),
            }],
            choices: vec![KEY.to_owned(), "claude-opus-5-5".to_owned()],
            applies: true,
        }],
        saved: vec![
            SavedValue::of(
                SitePath::new(Place::Global, model),
                SettingValue::Text(KEY.to_owned()),
                Some(1),
            ),
            SavedValue::of(
                SitePath::new(Place::Model(KEY.to_owned()), window),
                SettingValue::Tokens(300_000),
                Some(3),
            ),
        ],
        endpoints: Vec::new(),
        models: vec![ModelEntry {
            id: KEY.to_owned(),
            shipped: false,
            configured: true,
            context_window: None,
            listed: None,
            provider: None,
        }],
        file: FileState::Missing,
        switches: vec![
            SwitchQuote {
                target: SwitchTarget::Model(KEY.to_owned()),
                consequence: None,
                retry_compaction: None,
                remembered: Some(wisp_protocol::settings::Remembered {
                    model: KEY.into(),
                    effort: wisp_protocol::Effort::High,
                }),
            },
            SwitchQuote {
                target: SwitchTarget::Pair {
                    endpoint: "work".into(),
                    model: KEY.into(),
                },
                consequence: None,
                retry_compaction: None,
                remembered: None,
            },
        ],
        compaction: None,
        plugins: Vec::new(),
        placed: Vec::new(),
    }
}

#[test]
fn a_key_shaped_model_is_taken_out_of_every_part_of_the_snapshot() {
    let raw = snapshot();
    assert!(format!("{raw:?}").contains("CANARY"));
    let clean = scrubbed(&raw);
    let text = format!("{clean:?}");
    assert!(!text.contains("CANARY"), "{text}");
    assert_eq!(clean.context.model, wisp_protocol::NOT_SHOWN);
    assert_eq!(
        clean.settings[0].value,
        Some(SettingValue::Text(wisp_protocol::NOT_SHOWN.to_owned()))
    );
    assert_eq!(clean.settings[0].choices[1], "claude-opus-5-5");
    assert_eq!(
        clean.saved[1].path.place,
        Place::Model(wisp_protocol::NOT_SHOWN.to_owned())
    );
    assert_eq!(clean.saved[1].line, Some(3));
}

#[test]
fn a_snapshot_without_a_hidden_model_is_kept_as_it_is() {
    let mut raw = snapshot();
    raw.context.model = "claude-opus-5-5".to_owned();
    raw.settings.clear();
    raw.saved.clear();
    raw.models.clear();
    raw.switches.clear();
    assert_eq!(scrubbed(&raw), raw);
}

#[test]
fn a_second_scrub_changes_nothing_and_never_names_a_placeholder_as_no_model_id() {
    let mut given = snapshot();
    given.settings[0].shadowed[0].origin = format!("models.\"{KEY}\".context_window");
    let once = scrubbed(&given);
    assert_eq!(scrubbed(&once), once);
    let text = format!("{once:?}");
    assert!(!text.contains(wisp_protocol::NOT_A_MODEL_ID), "{text}");
    assert_eq!(
        once.settings[0].shadowed[0].origin,
        "models.\"MODEL\".context_window"
    );
    assert_eq!(once.context.model, wisp_protocol::NOT_SHOWN);
}

#[test]
fn a_table_under_another_hidden_model_is_dropped_so_lines_stay_with_this_one() {
    const OTHER: &str = "sk-ant-api03-OTHERCANARY0000";
    let window = find("context_window").unwrap();
    let mut given = snapshot();
    given.saved.insert(
        0,
        SavedValue::of(
            SitePath::new(Place::Model(OTHER.to_owned()), window),
            SettingValue::Tokens(200_000),
            Some(1),
        ),
    );
    let once = scrubbed(&given);
    let windows: Vec<Option<u64>> = once
        .saved
        .iter()
        .filter(|s| s.path.spec().name == "context_window")
        .map(|s| s.line)
        .collect();
    assert_eq!(windows, [Some(3)]);
    assert!(!format!("{once:?}").contains(OTHER));
    assert_eq!(scrubbed(&once), once);
}
