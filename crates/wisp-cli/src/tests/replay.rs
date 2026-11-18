use std::ffi::OsString;

use wisp_protocol::settings::{FileState, Layer, ResolvedSetting, SettingChange};
use wisp_protocol::{AgentEvent, CacheEffect, ClientCommand, RebuildReason, RequestId};
use wisp_render::chrome::Endpoint;
use wisp_tui::app::SessionFacts;
use wisp_tui::app::replay::{self, Script, Settings};

use super::*;

const MANIFEST: &str = "default = \"manifest\"\ncache_ttl = \"5m\"\n\n[endpoints.manifest]\nprovider = \"anthropic\"\nauth = \"bearer\"\nkey_env = \"WISP_TEST_UNSET_GATEWAY_KEY\"\nurl = \"https://gw.example.com\"\n";

const STORE_ID: &str = "5f0c2a9e41d7b388";

struct Replayed {
    settings: Settings,
    facts: SessionFacts,
}

impl Replayed {
    fn row(&self, name: &str) -> &ResolvedSetting {
        self.settings
            .described
            .settings
            .iter()
            .find(|r| r.name.spec().name == name)
            .unwrap()
    }

    fn endpoint(&self) -> (&str, Layer, Endpoint) {
        let row = self.row("endpoint");
        let shown = match &row.value {
            Some(SettingValue::Text(text)) => text.as_str(),
            other => panic!("{other:?}"),
        };
        assert_eq!(self.settings.endpoint, shown);
        assert_eq!(self.facts.endpoint_name.as_deref(), Some(shown));
        (shown, row.layer.clone(), self.facts.endpoint)
    }
}

async fn ask(settings: Settings, command: ClientCommand) -> AgentEvent {
    let script = Script::new(Vec::new()).with_settings(settings);
    let (commands, mut events) = replay::spawn(script, std::time::Duration::ZERO);
    commands.send(command).await.unwrap();
    events.recv().await.unwrap()
}

async fn described(settings: Settings) -> wisp_protocol::settings::SettingsSnapshot {
    match ask(
        settings,
        ClientCommand::DescribeSettings {
            request: RequestId(4),
        },
    )
    .await
    {
        AgentEvent::SettingsSnapshot(snapshot) => snapshot,
        other => panic!("{other:?}"),
    }
}

fn replayed(config: &Path, extra: &[&str], variable: Option<&str>) -> Replayed {
    let mut all: Vec<OsString> = ["wisp", "--replay", "turns.jsonl", "--config"]
        .iter()
        .map(OsString::from)
        .collect();
    all.push(config.as_os_str().to_owned());
    all.extend(extra.iter().map(OsString::from));
    let args = usage::parse_from::<Args, _>(all).unwrap();
    let setup = setup_of(&args).unwrap();
    endpoints::TEST_CHOICE.with(|c| *c.borrow_mut() = variable.map(str::to_owned));
    let paths = ReplayPaths {
        xdg_config_home: None,
        home: None,
        home_shown: None,
    };
    let (settings, facts) = replay_start(&args, &setup, paths);
    endpoints::TEST_CHOICE.with(|c| *c.borrow_mut() = None);
    Replayed { settings, facts }
}

#[test]
fn a_replay_starts_on_the_saved_default_then_the_variable_then_the_flag() {
    let dir = crate::scratch_dir("replay-default");
    let config = dir.write("config.toml", MANIFEST);

    let saved = replayed(&config, &[], None);
    assert_eq!(
        saved.endpoint(),
        ("manifest", Layer::Config, Endpoint::Custom)
    );
    assert_eq!(saved.facts.cache_ttl, CacheTtl::FiveMinutes);
    assert_eq!(
        saved.facts.write_multipliers,
        tui::write_multipliers(CacheTtl::FiveMinutes)
    );
    let ttl = saved.row("cache_ttl");
    assert_eq!(ttl.value, Some(SettingValue::Text("5m".to_owned())));
    assert_eq!(ttl.layer, Layer::Config);
    assert!(matches!(
        saved.settings.described.file,
        FileState::Ok { .. }
    ));
    assert!(
        saved
            .settings
            .described
            .saved
            .iter()
            .any(|s| s.path.spec().name == "cache_ttl")
    );

    let variable = replayed(&config, &[], Some("direct"));
    assert_eq!(
        variable.endpoint(),
        ("direct", Layer::Env, Endpoint::Custom)
    );
    let row = variable.row("endpoint");
    assert_eq!(row.origin, endpoints::CHOICE_VAR);
    assert_eq!(row.shadowed.len(), 1);
    assert_eq!(row.shadowed[0].layer, Layer::Config);

    let flag = replayed(&config, &["--endpoint", "env"], Some("direct"));
    assert_eq!(flag.endpoint(), ("env", Layer::Flag, Endpoint::Official));
}

#[test]
fn a_configured_name_that_looks_like_a_key_shows_in_full_in_a_replay() {
    let name = "gateway-eu-central-1x";
    let dir = crate::scratch_dir("replay-key-shaped-name");
    let config = dir.write("config.toml", &MANIFEST.replace("manifest", name));
    for replay in [
        replayed(&config, &[], None),
        replayed(&config, &[], Some(name)),
        replayed(&config, &["--endpoint", name], None),
    ] {
        assert_eq!(replay.endpoint().0, name);
        let seen = format!("{:?} {:?}", replay.settings, replay.facts);
        assert!(!seen.contains("not shown"), "{seen}");
    }
}

#[test]
fn a_key_or_a_bad_name_chosen_for_a_replay_is_never_shown() {
    let dir = crate::scratch_dir("replay-wrong-place");
    let config = dir.write("config.toml", MANIFEST);
    let key = "sk-ant-api03-CANARYREPLAYKEY0001";
    let cases = [
        (replayed(&config, &[], Some(key)), Layer::Env),
        (replayed(&config, &["--endpoint", key], None), Layer::Flag),
        (replayed(&config, &[], Some("not a name")), Layer::Env),
    ];
    let words = [
        wisp_protocol::NOT_SHOWN,
        wisp_protocol::NOT_SHOWN,
        wisp_start::run::NOT_AN_ENDPOINT,
    ];
    for ((replay, layer), words) in cases.into_iter().zip(words) {
        assert_eq!(replay.endpoint(), (words, layer, Endpoint::Custom));
        let seen = format!("{:?} {:?}", replay.settings, replay.facts);
        assert!(!seen.contains("CANARY"), "{seen}");
        assert!(!seen.contains("not a name"), "{seen}");
    }
}

#[test]
fn with_no_file_flag_or_variable_a_replay_reads_env_by_default() {
    let dir = crate::scratch_dir("replay-nothing");
    let none = replayed(&dir.root.join("absent.toml"), &[], None);
    assert_eq!(none.endpoint(), ("env", Layer::Default, Endpoint::Official));
    assert_eq!(none.settings.described.file, FileState::Missing);
    assert!(none.settings.described.saved.is_empty());
    assert_eq!(none.facts.cache_ttl, CacheTtl::OneHour);
}

#[test]
fn a_broken_file_gives_no_saved_rows_and_the_endpoint_falls_back() {
    let dir = crate::scratch_dir("replay-broken");
    let config = dir.write(
        "config.toml",
        "default = \"manifest\"\nthis line = = is not toml\n",
    );
    let broken = replayed(&config, &[], None);
    assert!(matches!(
        broken.settings.described.file,
        FileState::Broken { .. }
    ));
    assert!(
        broken
            .settings
            .described
            .settings
            .iter()
            .all(|r| r.layer != Layer::Config)
    );
    assert_eq!(
        broken.endpoint(),
        ("env", Layer::Default, Endpoint::Official)
    );
    let variable = replayed(&config, &[], Some("manifest"));
    assert_eq!(
        variable.endpoint(),
        ("manifest", Layer::Env, Endpoint::Custom)
    );
    let flag = replayed(&config, &["--endpoint", "work"], Some("manifest"));
    assert_eq!(flag.endpoint(), ("work", Layer::Flag, Endpoint::Custom));
}

#[test]
fn the_replayed_endpoint_is_official_only_by_the_endpoint_config_rule() {
    let dir = crate::scratch_dir("replay-official");
    let config = dir.write("config.toml", &format!(
        "store_id = \"{STORE_ID}\"\n\n\
         [endpoints.literal]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"WISP_TEST_A\"\nurl = \"https://api.anthropic.com\"\n\n\
         [endpoints.keyed]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"WISP_TEST_B\"\n\n\
         [endpoints.gateway]\nprovider = \"anthropic\"\nauth = \"bearer\"\nkey_env = \"WISP_TEST_C\"\nurl = \"https://gw.example.com\"\n\n\
         [endpoints.fromenv]\nprovider = \"anthropic\"\nauth = \"bearer\"\nkey_env = \"WISP_TEST_D\"\nurl_env = \"WISP_TEST_URL\"\n\n\
         [endpoints.stored]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_store = \"file\"\n"
    ));
    for (name, reach) in [
        ("literal", Endpoint::Official),
        ("keyed", Endpoint::Official),
        ("gateway", Endpoint::Custom),
        ("fromenv", Endpoint::Custom),
        ("stored", Endpoint::Custom),
        ("untabled", Endpoint::Custom),
        ("env", Endpoint::Official),
    ] {
        let r = replayed(&config, &["--endpoint", name], None);
        assert_eq!(r.endpoint(), (name, Layer::Flag, reach), "{name}");
    }
}

#[tokio::test]
async fn a_replay_names_no_credential_and_describes_what_it_resolved() {
    let dir = crate::scratch_dir("replay-credential");
    let config = dir.write("config.toml", MANIFEST);
    let r = replayed(&config, &[], None);
    assert_eq!(r.facts.credential, None);
    assert_eq!(r.facts.host, None);
    assert!(r.facts.endpoints.is_empty());
    let rows = r.settings.described.settings.clone();
    let snapshot = described(r.settings).await;
    assert_eq!(snapshot.answers, RequestId(4));
    assert_eq!(snapshot.context.endpoint, "manifest");
    assert!(snapshot.endpoints.is_empty());
    assert!(snapshot.switches.is_empty());
    assert_eq!(snapshot.compaction, None);
    assert_eq!(snapshot.settings, rows);
    assert!(matches!(snapshot.file, FileState::Ok { .. }));
}

#[tokio::test]
async fn a_model_answer_in_replay_names_no_endpoint_switch_and_reads_session() {
    let dir = crate::scratch_dir("replay-switch");
    let config = dir.write("config.toml", MANIFEST);
    let r = replayed(&config, &[], None);
    let spec = wisp_protocol::settings::find("model").unwrap();
    let change = ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![SettingChange::set(
            spec,
            SettingValue::Text("claude-sonnet-5-5".to_owned()),
        )],
        save: false,
    };
    let script = Script::new(Vec::new()).with_settings(r.settings);
    let (commands, mut events) = replay::spawn(script, std::time::Duration::ZERO);
    commands.send(change).await.unwrap();
    let answered = events.recv().await.unwrap();
    let AgentEvent::SettingsChanged {
        endpoint,
        cache,
        answers,
        ..
    } = answered
    else {
        panic!("{answered:?}");
    };
    assert_eq!(endpoint, "manifest");
    assert_eq!(answers, Some(RequestId(9)));
    assert_ne!(
        cache,
        CacheEffect::Rebuilt {
            reason: RebuildReason::EndpointSwitch
        }
    );
    commands
        .send(ClientCommand::DescribeSettings {
            request: RequestId(10),
        })
        .await
        .unwrap();
    let AgentEvent::SettingsSnapshot(snapshot) = events.recv().await.unwrap() else {
        panic!("no snapshot");
    };
    let row = |name: &str| {
        snapshot
            .settings
            .iter()
            .find(|r| r.name.spec().name == name)
            .unwrap()
            .clone()
    };
    let model = row("model");
    assert_eq!(model.layer, Layer::Session);
    assert_eq!(
        model.value,
        Some(SettingValue::Text("claude-sonnet-5-5".to_owned()))
    );
    assert_eq!(row("endpoint").layer, Layer::Config);
    assert_ne!(row("effort").layer, Layer::Session);
}
