use serde_json::json;

use super::*;
use crate::settings::Place;

fn name(text: &str) -> SettingName {
    SettingName::parse(text).unwrap()
}

fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "{text}");
}

#[test]
fn a_setting_name_serialises_as_its_text_and_refuses_unknown_names() {
    assert_eq!(
        serde_json::to_string(&name("effort")).unwrap(),
        "\"effort\""
    );
    assert_eq!(
        serde_json::from_str::<SettingName>("\"display.theme\"").unwrap(),
        name("display.theme")
    );
    assert!(serde_json::from_str::<SettingName>("\"efort\"").is_err());
    assert!(serde_json::from_str::<SettingName>("\"agents.x.effort\"").is_err());
    assert!(SettingName::parse("models.\"claude-sonnet-5-5\".context_window").is_none());
}

#[test]
fn a_site_path_serialises_as_its_canonical_text() {
    let spec = find("context_window").unwrap();
    let path = SitePath::new(Place::Model("claude-sonnet-5-5".into()), spec);
    let text = serde_json::to_string(&path).unwrap();
    assert_eq!(text, r#""models.\"claude-sonnet-5-5\".context_window""#);
    assert_eq!(serde_json::from_str::<SitePath>(&text).unwrap(), path);
    let global = SitePath::global(find("effort").unwrap());
    assert_eq!(serde_json::to_string(&global).unwrap(), "\"effort\"");
    round_trip(&global);
    let endpoint = SitePath::global(find("endpoint").unwrap());
    assert_eq!(serde_json::to_string(&endpoint).unwrap(), "\"default\"");
    round_trip(&endpoint);
    assert!(serde_json::from_str::<SitePath>("\"models.x\"").is_err());
}

#[test]
fn targets_sort_names_before_paths_and_changes_keep_one_per_target() {
    let path = SettingTarget::Path(SitePath::new(
        Place::Model("m".into()),
        find("context_window").unwrap(),
    ));
    let changes = sorted_changes(vec![
        SettingChange {
            target: path.clone(),
            value: None,
        },
        SettingChange::named("model", SettingValue::Text("b".into())).unwrap(),
        SettingChange::named("effort", SettingValue::Text("low".into())).unwrap(),
        SettingChange::named("model", SettingValue::Text("c".into())).unwrap(),
    ]);
    let targets: Vec<String> = changes.iter().map(|c| c.target.to_string()).collect();
    assert_eq!(targets, ["effort", "model", "models.\"m\".context_window"]);
    assert_eq!(changes[1].value, Some(SettingValue::Text("b".into())));
}

#[test]
fn a_change_and_an_unset_round_trip() {
    let set = SettingChange::named("effort", SettingValue::Text("high".into())).unwrap();
    assert_eq!(
        serde_json::to_value(&set).unwrap(),
        json!({"target": {"name": "effort"}, "value": {"kind": "text", "text": "high"}})
    );
    round_trip(&set);
    let unset = SettingChange {
        target: SettingTarget::Name(name("model")),
        value: None,
    };
    assert_eq!(
        serde_json::to_value(&unset).unwrap(),
        json!({"target": {"name": "model"}})
    );
    round_trip(&unset);
}

#[test]
fn every_open_enum_keeps_a_value_it_doesnt_know() {
    for (text, layer) in [
        ("\"session\"", Layer::Session),
        ("\"config\"", Layer::Config),
        ("\"policy\"", Layer::Other("policy".into())),
    ] {
        assert_eq!(serde_json::from_str::<Layer>(text).unwrap(), layer);
        assert_eq!(serde_json::to_string(&layer).unwrap(), text);
    }
    assert_eq!(
        serde_json::from_str::<When>("\"later\"").unwrap(),
        When::Other("later".into())
    );
    assert_eq!(
        serde_json::from_str::<OverrideScope>("\"forever\"").unwrap(),
        OverrideScope::Other("forever".into())
    );
    assert_eq!(
        serde_json::from_str::<Withdrawal>("\"switch_compaction_lost\"").unwrap(),
        Withdrawal::Other("switch_compaction_lost".into())
    );
    assert_eq!(
        serde_json::from_str::<CredentialKind>("\"oauth\"").unwrap(),
        CredentialKind::Other("oauth".into())
    );
    let future = json!({"type": "compacts_before_agent_change", "estimate_micros": 1});
    assert_eq!(
        serde_json::from_value::<Consequence>(future.clone()).unwrap(),
        Consequence::Other(future.clone())
    );
    assert_eq!(
        serde_json::to_value(Consequence::Other(future.clone())).unwrap(),
        future
    );
    let target = json!({"agent": "reviewer"});
    assert_eq!(
        serde_json::from_value::<SwitchTarget>(target.clone()).unwrap(),
        SwitchTarget::Other(target)
    );
    let file = json!({"type": "locked"});
    assert_eq!(
        serde_json::from_value::<FileState>(file.clone()).unwrap(),
        FileState::Other(file)
    );
    let entry = json!({"type": "hidden"});
    assert_eq!(
        serde_json::from_value::<SavedEntry>(entry.clone()).unwrap(),
        SavedEntry::Other(entry)
    );
}

#[test]
fn an_applied_change_round_trips_with_and_without_its_optional_parts() {
    let bare = AppliedChange {
        target: SettingTarget::Name(name("effort")),
        value: Some(SettingValue::Text("high".into())),
        layer: Layer::Session,
        when: When::NextTurn,
        cache: None,
        consequence: None,
        saved: None,
        overridden: None,
        warnings: Vec::new(),
    };
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        json!({
            "target": {"name": "effort"},
            "value": {"kind": "text", "text": "high"},
            "layer": "session",
            "when": "next_turn"
        })
    );
    round_trip(&bare);
    let full = AppliedChange {
        cache: Some(CacheEffect::Kept),
        consequence: Some(Consequence::CompactsBeforeNextMessage {
            estimate_micros: 290_000,
            head_write_micros: 120_000,
        }),
        saved: Some(Saved {
            path_display: "~/.config/wisp/config.toml".into(),
            line: Some(3),
            notes: vec!["a note".into()],
        }),
        overridden: Some(Overridden {
            by: Layer::Env,
            scope: OverrideScope::NewSessions,
        }),
        warnings: vec!["background compaction rarely runs below about 60,000".into()],
        ..bare
    };
    round_trip(&full);
}

#[test]
fn a_switch_consequence_round_trips_with_a_known_and_an_unknown_cause() {
    let known = Consequence::CompactsBeforeSwitch {
        estimate_micros: 480_000,
        head_tokens: 15_000,
        cause: SwitchCompactionCause::Convert,
    };
    assert_eq!(
        serde_json::to_value(&known).unwrap(),
        json!({
            "type": "compacts_before_switch",
            "estimate_micros": 480_000,
            "head_tokens": 15_000,
            "cause": "convert"
        })
    );
    round_trip(&known);
    let later = json!({
        "type": "compacts_before_switch",
        "estimate_micros": 1,
        "head_tokens": 2,
        "cause": "new_tokenizer"
    });
    let read = serde_json::from_value::<Consequence>(later.clone()).unwrap();
    assert_eq!(
        read,
        Consequence::CompactsBeforeSwitch {
            estimate_micros: 1,
            head_tokens: 2,
            cause: SwitchCompactionCause::Other("new_tokenizer".into()),
        }
    );
    assert_eq!(serde_json::to_value(&read).unwrap(), later);
}

#[test]
fn a_snapshot_round_trips() {
    let mut snapshot = SettingsSnapshot {
        often: Vec::new(),
        answers: RequestId(4),
        context: SnapshotContext {
            endpoint: "env".into(),
            model: "claude-opus-5-5".into(),
            agent: None,
            max_tokens: 64_000,
            window: None,
            room: None,
        },
        settings: vec![ResolvedSetting {
            name: name("effort"),
            value: Some(SettingValue::Text("high".into())),
            layer: Layer::Flag,
            origin: "--effort".into(),
            shadowed: vec![Shadowed {
                layer: Layer::Default,
                value: SettingValue::Text("medium".into()),
                origin: "default".into(),
            }],
            choices: vec!["low".into(), "high".into()],
            applies: true,
        }],
        saved: vec![
            SavedValue::of(
                SitePath::global(find("effort").unwrap()),
                SettingValue::Text("low".into()),
                Some(2),
            ),
            SavedValue {
                path: SitePath::new(Place::Endpoint("gw".into()), find("url").unwrap()),
                entry: SavedEntry::Host {
                    host: "gw.example.com".into(),
                },
                line: Some(7),
            },
        ],
        endpoints: vec![EndpointSummary {
            models: Vec::new(),
            name: "gw".into(),
            provider: "anthropic".into(),
            credential: Some(CredentialKind::AuthToken),
            official: Some(false),
            host: Some("gw.example.com".into()),
            unavailable: None,
            priced: false,
            default: true,
            keys_file: false,
            expired: false,
            key_var: None,
            mark: Some(3),
            complete: false,
            default_model: None,
            voice_model: None,
        }],
        models: vec![ModelEntry {
            id: "claude-opus-5-5".into(),
            shipped: true,
            configured: false,
            context_window: None,
            listed: Some(0),
            provider: None,
        }],
        file: FileState::Ok {
            path_display: "~/.config/wisp/config.toml".into(),
        },
        switches: vec![
            SwitchQuote {
                target: SwitchTarget::Model("claude-sonnet-5-5".into()),
                consequence: None,
                remembered: None,
                retry_compaction: Some(CompactionCost {
                    estimate_micros: 1,
                    head_write_micros: 2,
                }),
            },
            SwitchQuote {
                target: SwitchTarget::Pair {
                    endpoint: "work".into(),
                    model: "claude-haiku-5-5".into(),
                },
                consequence: None,
                remembered: None,
                retry_compaction: None,
            },
        ],
        compaction: Some(CompactionQuote {
            fill_tokens: 850_000,
            max_tokens: 64_000,
            estimate_micros: 290_000,
            head_write_micros: 120_000,
        }),
        plugins: Vec::new(),
        placed: vec![PlacedValue {
            path: SitePath::plugin_switch("time", "enabled").unwrap(),
            value: Some(SettingValue::Bool(false)),
            layer: Layer::Session,
        }],
    };
    assert!(
        serde_json::to_value(&snapshot)
            .unwrap()
            .get("often")
            .is_none()
    );
    let old = serde_json::to_value(&snapshot).unwrap();
    assert!(old["switches"][0].get("remembered").is_none());
    assert!(old["context"].get("window").is_none());
    assert!(old["context"].get("room").is_none());
    assert_eq!(
        serde_json::from_value::<SettingsSnapshot>(old).unwrap(),
        snapshot
    );
    snapshot.context.window = Some(470_589);
    snapshot.context.room = Some(WindowRoom {
        base: 272_000,
        max: 872_000,
    });
    assert_eq!(
        serde_json::to_value(&snapshot.context).unwrap(),
        serde_json::json!({
            "endpoint": "env", "model": "claude-opus-5-5", "max_tokens": 64000,
            "window": 470589, "room": {"base": 272000, "max": 872000}
        })
    );
    snapshot.switches[0].remembered = Some(super::super::Remembered {
        model: "claude-sonnet-5-5".into(),
        effort: Effort::High,
    });
    round_trip(&snapshot);
    snapshot.often = vec!["time".into()];
    round_trip(&snapshot);
    let older: EndpointSummary = serde_json::from_value(serde_json::json!({
        "name": "gw", "provider": "anthropic", "priced": true, "default": false
    }))
    .unwrap();
    assert_eq!(older.mark, None);
    assert!(!snapshot.plugin_on("time", "enabled"));
    assert!(snapshot.plugin_on("time", "installed"));
    assert!(snapshot.plugin_on("context", "enabled"));
    round_trip(&Restored {
        effort: Effort::High,
        cache_ttl: CacheTtl::OneHour,
    });
}

#[test]
fn a_secret_saved_value_holds_only_its_state() {
    let key = SavedValue::of(
        SitePath::new(Place::Endpoint("gw".into()), find("key").unwrap()),
        SettingValue::Text("sk-ant-api03-not-CANARY-key".into()),
        None,
    );
    let text = serde_json::to_string(&key).unwrap();
    assert!(!text.contains("sk-ant"), "{text}");
    assert_eq!(
        key.entry,
        SavedEntry::Secret {
            state: SecretState::Set
        }
    );
}
