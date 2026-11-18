use std::sync::Arc;

use crate::resume::tests::Silent;
use crate::turn_lines::with_turn_lines;
use wisp_core::agent::{self, Agent, default_session};
use wisp_core::config::{SavedSettings, shown_value};
use wisp_core::models::ModelTable;
use wisp_core::settings::host::StartLayers;
use wisp_core::settings::{Current, Given, Layer, Layers, resolve_one};
use wisp_protocol::settings::{
    Default, Place, Secrecy, SettingValue, SettingsSnapshot, SitePath, Status, Target, find,
};
use wisp_protocol::{AgentEvent, ClientCommand, Effort, RequestId};
use wisp_tui::app::config_screen::{DEFAULT, Display, Row, placed, rows};

const MODEL: &str = "claude-opus-5-5";

fn given(name: &str, value: SettingValue, origin: &str) -> Given {
    Given {
        target: Target::Name(find(name).unwrap()),
        value,
        origin: origin.to_owned(),
    }
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

fn flags() -> Vec<Given> {
    vec![
        given("effort", text("high"), "--effort"),
        given("display.theme", text("light"), "--set display.theme"),
    ]
}

fn env() -> Vec<Given> {
    vec![given("display.motion", text("reduced"), "WISP_MOTION")]
}

fn saved() -> SavedSettings {
    let mut saved = SavedSettings::default();
    let global = |name: &str| SitePath::new(Place::Global, find(name).unwrap());
    saved.insert(global("model"), text(MODEL), 1);
    saved.insert(global("cache_ttl"), text("5m"), 2);
    saved.insert(global("effort"), text("medium"), 3);
    saved.insert(global("debug_cache"), SettingValue::Bool(true), 4);
    saved.insert(
        SitePath::new(
            Place::Model(MODEL.to_owned()),
            find("context_window").unwrap(),
        ),
        SettingValue::Tokens(400_000),
        6,
    );
    saved.insert(global("display.motion"), text("full"), 7);
    saved
}

const RUNNING: Effort = Effort::High;

fn agent_with(layers: StartLayers) -> Agent {
    let mut session = default_session();
    session.model = MODEL.to_owned();
    session.effort = RUNNING;
    with_turn_lines(Agent::new(Arc::new(Silent(None)), session)).with_start_layers(layers)
}

async fn ask(agent: Agent, before: Vec<ClientCommand>) -> SettingsSnapshot {
    ask_seeing(agent, before).await.1
}

async fn ask_seeing(
    agent: Agent,
    before: Vec<ClientCommand>,
) -> (Vec<AgentEvent>, SettingsSnapshot) {
    let (commands, mut events, task) = agent::spawn(agent);
    for command in before {
        commands.send(command).await.unwrap();
    }
    commands
        .send(ClientCommand::DescribeSettings {
            request: RequestId(9),
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    let snapshot = loop {
        match events.recv().await {
            Some(AgentEvent::SettingsSnapshot(snapshot)) if snapshot.answers == RequestId(9) => {
                break snapshot;
            }
            Some(event) => seen.push(event),
            None => panic!("the kernel ended without a snapshot"),
        }
    };
    drop(commands);
    while events.recv().await.is_some() {}
    task.handle.await.unwrap();
    (seen, snapshot)
}

async fn described() -> SettingsSnapshot {
    let agent = agent_with(StartLayers {
        remembered: std::default::Default::default(),
        forced: Vec::new(),
        flags: flags(),
        env: env(),
        saved: saved(),
        endpoints: Vec::new(),
        models: Vec::new(),
        full_screen: true,
    });
    ask(agent, Vec::new()).await
}

fn check(row: &Row, layers: &Layers<'_>, current: &Current, keys_file: Option<&str>) {
    let spec = row.spec;
    let name = spec.name;
    let hidden = |text: &str| wisp_protocol::hide_models(text, [current.model.as_str()]);
    let origin = |text: &str| hidden(&wisp_protocol::hide_quoted_models(text));
    let resolution = resolve_one(spec, layers, current).shown_for_keys_file(keys_file);
    let set: Vec<_> = resolution
        .found
        .iter()
        .filter(|f| f.layer != Layer::Default)
        .collect();
    let Some((winner, rest)) = set.split_first() else {
        match resolution.winner() {
            Some(_) if name == "effort" => {
                assert_eq!(row.value, RUNNING.as_str());
                assert_eq!(row.source, Layer::Default.source());
            }
            Some(default) => {
                assert_eq!(row.value, shown_value(spec, &default.value), "{name}");
                assert_eq!(row.source, Layer::Default.source(), "{name}");
            }
            None if name == "endpoint" => {
                assert_eq!(row.source, Layer::Default.source(), "{name}");
            }
            None => {
                let word = match spec.default {
                    Default::Rule(_) => "rule",
                    Default::Const(_) | Default::Detected(_) | Default::None => "default",
                };
                assert_eq!(row.source, word, "{name}");
            }
        }
        assert!(row.winner.is_none(), "{name}");
        assert!(row.shadowed.is_empty(), "{name}");
        return;
    };
    assert_eq!(
        row.value,
        hidden(&shown_value(spec, &winner.value)),
        "{name}"
    );
    assert_eq!(row.source, winner.layer.source(), "{name}");
    let shown = row.winner.as_ref().unwrap();
    assert_eq!(shown.origin, origin(&winner.origin), "{name}");
    assert_eq!(
        shown.line,
        winner.line.map(|l| u64::try_from(l).unwrap()),
        "{name}"
    );
    let rows: Vec<(String, String, String)> = row
        .shadowed
        .iter()
        .map(|f| (f.value.clone(), f.word(), f.origin.clone()))
        .collect();
    let kernel: Vec<(String, String, String)> = rest
        .iter()
        .map(|f| {
            (
                hidden(&shown_value(spec, &f.value)),
                f.layer.source().to_owned(),
                origin(&f.origin),
            )
        })
        .collect();
    assert_eq!(rows, kernel, "{name}");
}

#[tokio::test]
async fn every_row_the_settings_screen_shows_matches_the_kernel() {
    let snapshot = described().await;
    let shown = rows(&snapshot, &Display::default());
    let want: Vec<&str> = placed()
        .map(|(_, spec)| spec.name)
        .filter(|name| {
            find(name).is_some_and(|s| s.status == Status::Built && s.secrecy == Secrecy::Plain)
        })
        .collect();
    let got: Vec<&str> = shown.iter().map(|r| r.spec.name).collect();
    assert_eq!(got, want);
    let saved = saved();
    let shipped = ModelTable::shipped().ok();
    let layers = Layers {
        session: Vec::new(),
        flags: flags(),
        env: env(),
        saved: Some(&saved),
        shipped: shipped.as_ref(),
    };
    let current = Current {
        endpoint: Some(snapshot.context.endpoint.clone()),
        model: snapshot.context.model.clone(),
    };
    for row in &shown {
        check(
            row,
            &layers,
            &current,
            wisp_core::config::keys_file_of(&snapshot.endpoints),
        );
    }
    let row = |name: &str| shown.iter().find(|r| r.spec.name == name).unwrap();
    assert_eq!(row("effort").source, "flag");
    assert_eq!(row("effort").shadowed[0].line, Some(3));
    let window = window_row(&snapshot);
    check(
        &window,
        &layers,
        &current,
        wisp_core::config::keys_file_of(&snapshot.endpoints),
    );
    assert_eq!(window.source, "model");
    assert_eq!(window.shadowed[0].word(), "shipped");
    assert_eq!(row("display.motion").source, "variable");
    assert_eq!(row("display.theme").source, "flag");
    assert_eq!(row("compact_at").source, "rule");
    assert!(
        row("compact_at").value.contains(" of "),
        "{}",
        row("compact_at").value
    );
}

#[tokio::test]
async fn the_display_rows_keep_the_kernel_layers_under_the_terminal_s_own() {
    let snapshot = described().await;
    let mut display = Display::default();
    display.set("display.motion", text("off"), "the terminal asked");
    let shown = rows(&snapshot, &display);
    let motion = shown
        .iter()
        .find(|r| r.spec.name == "display.motion")
        .unwrap();
    assert_eq!(motion.value, "off");
    assert_eq!(motion.source, "detected");
    let under: Vec<String> = motion.shadowed.iter().map(|f| f.word()).collect();
    assert_eq!(under, ["variable", "config"]);
}

#[test]
fn the_screen_names_each_kernel_layer_as_the_kernel_does() {
    for layer in [
        Layer::Forced,
        Layer::Session,
        Layer::Flag,
        Layer::Environment,
        Layer::Mode,
        Layer::Agent,
        Layer::EndpointModel,
        Layer::Model,
        Layer::Endpoint,
        Layer::Global,
        Layer::Shipped,
        Layer::Default,
    ] {
        assert_eq!(layer.wire().word(), layer.source(), "{layer:?}");
    }
    assert_eq!(Layer::Default.wire().word(), DEFAULT);
    assert_eq!(Layer::Environment.source(), "variable");
    assert_eq!(Layer::Endpoint.source(), "account");
}

fn layers_of(start: &StartLayers) -> (Vec<Given>, Vec<Given>, SavedSettings) {
    (start.flags.clone(), start.env.clone(), start.saved.clone())
}

fn agrees(shown: &[Row], start: &StartLayers, snapshot: &SettingsSnapshot) {
    let (flags, env, saved) = layers_of(start);
    let shipped = ModelTable::shipped().ok();
    let session = snapshot
        .settings
        .iter()
        .filter(|r| r.layer == wisp_protocol::settings::Layer::Session)
        .filter_map(|r| {
            Some(Given {
                target: Target::Name(r.name.spec()),
                value: r.value.clone()?,
                origin: r.origin.clone(),
            })
        })
        .collect();
    let layers = Layers {
        session,
        flags,
        env,
        saved: Some(&saved),
        shipped: shipped.as_ref(),
    };
    let current = Current {
        endpoint: Some(snapshot.context.endpoint.clone()),
        model: snapshot.context.model.clone(),
    };
    let keys_file = wisp_core::config::keys_file_of(&snapshot.endpoints);
    for row in shown {
        check(
            row,
            &layers,
            &current,
            Some(wisp_core::config::keys_file_shown(keys_file)),
        );
    }
}

const GATEWAY: &str = "default = \"manifest\"\n\n[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"https://gw.example.com\"\n\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"DIRECT_KEY\"\n";

fn gateway_keys(name: &str) -> Option<String> {
    match name {
        "GW_TOKEN" => Some("gw-token".to_owned()),
        "DIRECT_KEY" => Some("sk-ant-direct".to_owned()),
        _ => None,
    }
}

fn an_exported_key(name: &str) -> Option<String> {
    match name {
        "ANTHROPIC_API_KEY" => Some("sk-ant-exported".to_owned()),
        other => gateway_keys(other),
    }
}

struct Started {
    agent: Agent,
    layers: StartLayers,
}

fn started(
    dir: &crate::scratch::ScratchDir,
    config: &std::path::Path,
    env: crate::keychain::Lookup,
    variable: Option<&str>,
    extra: &[&str],
    keychain_variable: Option<&str>,
) -> Started {
    let mut all = vec![
        std::ffi::OsString::from("wisp"),
        std::ffi::OsString::from("--config"),
        config.as_os_str().to_owned(),
    ];
    all.extend(extra.iter().map(std::ffi::OsString::from));
    let args = crate::usage::parse_from::<crate::Args, _>(all).unwrap();
    let setup = crate::setup_of(&args).unwrap();
    let keychain = if extra.contains(&"--keychain") {
        crate::keychain::Keychain::assemble(
            wisp_core::secrets::detect::KeychainChoice {
                setting: wisp_core::secrets::detect::KeychainSetting::Off,
                origin: Some("--keychain".to_owned()),
            },
            wisp_core::secrets::detect::Detected::Off {
                origin: "--keychain".to_owned(),
            },
            false,
            None,
        )
    } else {
        crate::keychain::Keychain::off()
    };
    crate::endpoints::TEST_CHOICE.with(|c| *c.borrow_mut() = variable.map(str::to_owned));
    let mut sources = crate::endpoints::Sources::load(
        &crate::flags(&args, &setup, &keychain),
        Some(dir.root.clone()),
        None,
    );
    crate::endpoints::TEST_CHOICE.with(|c| *c.borrow_mut() = None);
    sources.inputs = sources.inputs.with_env(env);
    let book = sources.resolve().map_err(|e| e.to_string()).unwrap();
    let (ready, refused) = crate::endpoints::connections(&book.entries, None);
    let layers = StartLayers {
        forced: crate::forced_layer(
            true,
            &wisp_tui::term::Env::current(),
            wisp_tui::term::Streams::current(),
        ),
        ..wisp_start::run::start_layers(
            &setup,
            &sources,
            &book,
            &keychain,
            keychain_variable,
            wisp_start::run::start_summaries(&sources, &book, &refused, &ready),
            true,
        )
    };
    let agent = agent_with(layers.clone()).with_connections(ready, refused, book.start.as_str());
    Started { agent, layers }
}

fn endpoint_row(shown: &[Row]) -> &Row {
    shown.iter().find(|r| r.spec.name == "endpoint").unwrap()
}

#[tokio::test]
async fn an_exported_key_over_the_saved_default_shows_as_the_session_runs() {
    let dir = crate::scratch_dir("screen-agreement-exported");
    let config = dir.write("config.toml", GATEWAY);
    let start = started(&dir, &config, &an_exported_key, None, &[], None);
    let snapshot = ask(start.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        ("claude-api", "variable")
    );
    let under: Vec<(&str, String)> = endpoint
        .shadowed
        .iter()
        .map(|f| (f.value.as_str(), f.word()))
        .collect();
    assert_eq!(under, [("manifest", "config".to_owned())]);
    let keychain = shown.iter().find(|r| r.spec.name == "keychain").unwrap();
    assert_eq!(
        (keychain.value.as_str(), keychain.source.as_str()),
        ("off", "variable")
    );
    agrees(&shown, &start.layers, &snapshot);
}

#[tokio::test]
async fn the_endpoint_variable_over_the_saved_default_shows_as_the_session_runs() {
    let dir = crate::scratch_dir("screen-agreement-variable");
    let config = dir.write("config.toml", GATEWAY);
    let start = started(&dir, &config, &gateway_keys, Some("direct"), &[], None);
    let snapshot = ask(start.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        ("direct", "variable")
    );
    assert_eq!(
        endpoint.winner.as_ref().map(|w| w.origin.as_str()),
        Some(crate::endpoints::CHOICE_VAR)
    );
    assert_eq!(endpoint.shadowed[0].value, "manifest");
    agrees(&shown, &start.layers, &snapshot);
    let saved_default = started(&dir, &config, &gateway_keys, None, &[], None);
    let snapshot = ask(saved_default.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        ("manifest", "config")
    );
    agrees(&shown, &saved_default.layers, &snapshot);
}

const SONNET: &str = "claude-sonnet-5-5";

fn windows(saved: &[(&str, u64, usize)]) -> StartLayers {
    let mut file = SavedSettings::default();
    for (model, window, line) in saved {
        file.insert(
            SitePath::new(
                Place::Model((*model).to_owned()),
                find("context_window").unwrap(),
            ),
            SettingValue::Tokens(*window),
            *line,
        );
    }
    StartLayers {
        saved: file,
        full_screen: true,
        ..StartLayers::default()
    }
}

async fn after_switching_to_sonnet(start: StartLayers) -> SettingsSnapshot {
    let switch = ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![
            wisp_protocol::settings::SettingChange::named("model", text(SONNET)).unwrap(),
        ],
        save: false,
    };
    let snapshot = ask(agent_with(start.clone()), vec![switch]).await;
    let shown = rows(&snapshot, &Display::default());
    agrees(&shown, &start, &snapshot);
    agrees(&[window_row(&snapshot)], &start, &snapshot);
    snapshot
}

fn window_row(snapshot: &SettingsSnapshot) -> Row {
    let mut screen =
        wisp_tui::app::config_screen::Screen::opening(snapshot.answers, "context_window");
    assert!(screen.answered(snapshot, &Display::default()));
    screen.selected().unwrap().clone()
}

#[tokio::test]
async fn a_pending_model_reads_its_own_saved_values_with_no_changed_note() {
    let both = windows(&[(MODEL, 400_000, 6), (SONNET, 300_000, 7)]);
    let snapshot = after_switching_to_sonnet(both).await;
    assert_eq!(snapshot.context.model, SONNET);
    let window = window_row(&snapshot);
    assert_eq!(
        (window.value.as_str(), window.source.as_str()),
        ("300,000", "model")
    );
    assert_eq!(window.changed, None);
    assert_eq!(
        wisp_tui::app::config_screen::set_command(&window),
        format!("wisp config set 'models.\"{SONNET}\".context_window' VALUE")
    );
}

#[tokio::test]
async fn a_pending_model_with_the_only_saved_value_shows_no_removed_note() {
    let only_sonnet = windows(&[(SONNET, 300_000, 7)]);
    let snapshot = after_switching_to_sonnet(only_sonnet).await;
    let window = window_row(&snapshot);
    assert_eq!(window.source, "model");
    assert_eq!(window.changed, None);
}

const GATEWAY_NO_DEFAULT: &str = "[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"https://gw.example.com\"\n";

const KEY_IN_THE_FILE: &str = "ANTHROPIC_API_KEY=sk-ant-from-the-file\n";

#[tokio::test]
async fn a_key_only_in_the_default_env_file_is_the_start_rule_not_the_environment() {
    let dir = crate::scratch_dir("screen-agreement-env-file");
    dir.write("wisp/.env", KEY_IN_THE_FILE);
    let config = dir.write("config.toml", GATEWAY_NO_DEFAULT);
    let start = started(&dir, &config, &gateway_keys, None, &[], None);
    assert!(
        start
            .layers
            .env
            .iter()
            .all(|g| g.target.spec().name != "endpoint"),
        "{:?}",
        start.layers.env
    );
    let snapshot = ask(start.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        ("claude-api", "default")
    );
    assert!(endpoint.winner.is_none());
    assert!(endpoint.shadowed.is_empty());
    agrees(&shown, &start.layers, &snapshot);

    let chosen = dir.write(
        "config.toml",
        &format!("default = \"env\"\n{GATEWAY_NO_DEFAULT}"),
    );
    let start = started(&dir, &chosen, &gateway_keys, None, &[], None);
    let snapshot = ask(start.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        ("claude-api", "config")
    );
    agrees(&shown, &start.layers, &snapshot);

    let bare = crate::scratch_dir("screen-agreement-no-env-file");
    let chosen = bare.write(
        "config.toml",
        &format!("default = \"env\"\n{GATEWAY_NO_DEFAULT}"),
    );
    let start = started(
        &bare,
        &chosen,
        &gateway_keys,
        None,
        &["--endpoint", "manifest"],
        None,
    );
    let snapshot = ask(start.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let under: Vec<(&str, String)> = endpoint_row(&shown)
        .shadowed
        .iter()
        .map(|f| (f.value.as_str(), f.word()))
        .collect();
    assert_eq!(
        under,
        [(wisp_protocol::KEYS_FILE_ACCOUNT, "config".to_owned())]
    );
    let saved_value = snapshot
        .saved
        .iter()
        .find(|v| v.path.spec().name == "endpoint");
    assert!(
        matches!(saved_value.map(|v| &v.entry), Some(wisp_protocol::settings::SavedEntry::Value { value }) if value.text() == wisp_protocol::KEYS_FILE_ACCOUNT),
        "{saved_value:?}"
    );
    agrees(&shown, &start.layers, &snapshot);

    let saved = dir.write("config.toml", GATEWAY);
    let start = started(&dir, &saved, &gateway_keys, None, &[], None);
    let snapshot = ask(start.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        ("manifest", "config")
    );
    assert!(endpoint.shadowed.is_empty(), "{:?}", endpoint.shadowed);
    agrees(&shown, &start.layers, &snapshot);
}

#[tokio::test]
async fn variables_and_an_exported_key_are_listed_under_the_flag_or_variable_that_won() {
    let dir = crate::scratch_dir("screen-agreement-under");
    let config = dir.write("config.toml", GATEWAY);
    let flagged = started(
        &dir,
        &config,
        &an_exported_key,
        Some("direct"),
        &["--endpoint", "manifest", "--keychain", "off"],
        Some("on"),
    );
    let snapshot = ask(flagged.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        ("manifest", "flag")
    );
    let under: Vec<(&str, String)> = endpoint
        .shadowed
        .iter()
        .map(|f| (f.value.as_str(), f.word()))
        .collect();
    assert_eq!(
        under,
        [
            ("direct", "variable".to_owned()),
            ("claude-api", "variable".to_owned()),
            ("manifest", "config".to_owned())
        ]
    );
    let origins: Vec<&str> = snapshot
        .settings
        .iter()
        .find(|r| r.name.spec().name == "endpoint")
        .unwrap()
        .shadowed
        .iter()
        .map(|f| f.origin.as_str())
        .collect();
    assert_eq!(origins[0], crate::endpoints::CHOICE_VAR);
    let keychain = shown.iter().find(|r| r.spec.name == "keychain").unwrap();
    assert_eq!(
        (keychain.value.as_str(), keychain.source.as_str()),
        ("off", "flag")
    );
    let under: Vec<(&str, String)> = keychain
        .shadowed
        .iter()
        .map(|f| (f.value.as_str(), f.word()))
        .collect();
    assert_eq!(under, [("on", "variable".to_owned())]);
    agrees(&shown, &flagged.layers, &snapshot);

    let chosen = started(&dir, &config, &an_exported_key, Some("direct"), &[], None);
    let snapshot = ask(chosen.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        ("direct", "variable")
    );
    let under: Vec<(&str, String)> = endpoint
        .shadowed
        .iter()
        .map(|f| (f.value.as_str(), f.word()))
        .collect();
    assert_eq!(
        under,
        [
            ("claude-api", "variable".to_owned()),
            ("manifest", "config".to_owned())
        ]
    );
    agrees(&shown, &chosen.layers, &snapshot);
}

const WRONG_PLACE: &str = "sk-ant-api03-CANARYWRONGPLACE0000";

#[tokio::test]
async fn a_key_or_a_bad_name_in_the_endpoint_variable_is_never_shown() {
    let dir = crate::scratch_dir("screen-agreement-wrong-place");
    let config = dir.write("config.toml", GATEWAY);
    for (variable, words) in [
        (WRONG_PLACE, wisp_protocol::NOT_SHOWN),
        ("not a name", wisp_start::run::NOT_AN_ENDPOINT),
    ] {
        let start = started(
            &dir,
            &config,
            &gateway_keys,
            Some(variable),
            &["--endpoint", "manifest"],
            None,
        );
        assert!(!format!("{:?}", start.layers).contains(variable));
        let snapshot = ask(start.agent, Vec::new()).await;
        assert!(!format!("{snapshot:?}").contains(variable));
        let shown = rows(&snapshot, &Display::default());
        assert!(!format!("{shown:?}").contains(variable));
        let endpoint = endpoint_row(&shown);
        assert_eq!(
            (endpoint.value.as_str(), endpoint.source.as_str()),
            ("manifest", "flag")
        );
        let under: Vec<(&str, String)> = endpoint
            .shadowed
            .iter()
            .map(|f| (f.value.as_str(), f.word()))
            .collect();
        assert_eq!(
            under,
            [
                (words, "variable".to_owned()),
                ("manifest", "config".to_owned())
            ]
        );
        agrees(&shown, &start.layers, &snapshot);
    }
}

const KEY_AS_MODEL: &str = "sk-ant-api03-CANARYMODEL0000";

fn model_row(shown: &[Row]) -> &Row {
    shown.iter().find(|r| r.spec.name == "model").unwrap()
}

#[tokio::test]
async fn a_key_given_as_the_model_is_never_shown() {
    let dir = crate::scratch_dir("screen-agreement-key-as-model");
    let saved = format!("model = \"{KEY_AS_MODEL}\"\n{GATEWAY}");
    let config = dir.write("config.toml", &saved);
    let start = started(&dir, &config, &gateway_keys, None, &[], None);
    let snapshot = ask(start.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    assert!(!format!("{shown:?}").contains("CANARY"));
    let model = model_row(&shown);
    assert_eq!(
        (model.value.as_str(), model.source.as_str()),
        (wisp_protocol::NOT_SHOWN, "config")
    );
    assert_eq!(
        model.current,
        Some(SettingValue::Text(wisp_protocol::NOT_SHOWN.to_owned()))
    );
    let mut screen = wisp_tui::app::config_screen::Screen::opening(snapshot.answers, "model");
    assert!(screen.answered(&snapshot, &Display::default()));
    let quote = wisp_tui::app::config_screen::Quote::default();
    let state = format!("{screen:?} {:?}", screen.detail(&quote));
    assert!(!state.contains("CANARY"), "{state}");
    assert!(!state.contains(wisp_protocol::NOT_A_MODEL_ID));
    let row = model_row(screen.rows());
    assert_eq!(row.value, wisp_protocol::NOT_SHOWN);
    assert_eq!(
        row.current,
        Some(SettingValue::Text(wisp_protocol::NOT_SHOWN.to_owned()))
    );
    let pane = screen.detail(&quote).unwrap();
    assert!(
        pane.layers
            .iter()
            .any(|l| l.value == wisp_protocol::NOT_SHOWN),
        "{pane:?}"
    );
    agrees(&shown, &start.layers, &snapshot);
    let start = started(
        &dir,
        &config,
        &gateway_keys,
        None,
        &["--model", "claude-sonnet-4-5-20250929"],
        None,
    );
    let snapshot = ask(start.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let model = model_row(&shown);
    assert_eq!(model.value, "claude-sonnet-4-5-20250929");
    let under: Vec<&str> = model.shadowed.iter().map(|f| f.value.as_str()).collect();
    assert_eq!(under, [wisp_protocol::NOT_SHOWN]);
    assert!(!format!("{shown:?}").contains("CANARY"));
    agrees(&shown, &start.layers, &snapshot);
}

#[tokio::test]
async fn a_key_as_the_context_model_stays_out_of_every_row_and_detail() {
    let saved = windows(&[(KEY_AS_MODEL, 300_000, 3)]);
    let switch = ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![
            wisp_protocol::settings::SettingChange::named("model", text(KEY_AS_MODEL)).unwrap(),
        ],
        save: false,
    };
    let snapshot = ask(agent_with(saved.clone()), vec![switch]).await;
    assert_eq!(snapshot.context.model, KEY_AS_MODEL);
    let shown = rows(&snapshot, &Display::default());
    agrees(&shown, &saved, &snapshot);
    let window = window_row(&snapshot);
    agrees(std::slice::from_ref(&window), &saved, &snapshot);
    assert_eq!(
        (window.value.as_str(), window.source.as_str()),
        ("300,000", "model")
    );
    assert_eq!(
        wisp_tui::app::config_screen::set_command(&window),
        "wisp config set 'models.\"MODEL\".context_window' VALUE"
    );
    let quote = wisp_tui::app::config_screen::Quote::default();
    let detail = wisp_tui::app::config_screen::detail(&window, idle_view(&quote));
    assert!(
        detail
            .layers
            .iter()
            .any(|l| l.origin.starts_with("models.\"MODEL\".context_window")),
        "{detail:?}"
    );
    let mut screen =
        wisp_tui::app::config_screen::Screen::opening(snapshot.answers, "context_window");
    assert!(screen.answered(&snapshot, &Display::default()));
    let state = format!("{screen:?} {:?}", screen.detail(&quote));
    assert!(!state.contains("CANARY"), "{state}");
    assert!(!state.contains(wisp_protocol::NOT_A_MODEL_ID), "{state}");
    let model = model_row(&shown);
    assert_eq!(model.value, wisp_protocol::NOT_SHOWN);
    let window = screen.selected().unwrap();
    assert_eq!(
        wisp_tui::app::config_screen::set_command(window),
        "wisp config set 'models.\"MODEL\".context_window' VALUE"
    );
    let pane = screen.detail(&quote).unwrap();
    assert!(
        pane.layers
            .iter()
            .any(|l| l.origin.starts_with("models.\"MODEL\".context_window")),
        "{pane:?}"
    );
    for row in screen.rows() {
        let text = format!(
            "{} {:?}",
            wisp_tui::app::config_screen::set_command(row),
            wisp_tui::app::config_screen::detail(row, idle_view(&quote))
        );
        assert!(!text.contains(wisp_protocol::NOT_A_MODEL_ID), "{text}");
    }
    for row in &shown {
        let text = format!(
            "{} {} {} {:?}",
            wisp_tui::app::config_screen::set_command(row),
            row.value,
            row.source,
            wisp_tui::app::config_screen::detail(row, idle_view(&quote))
        );
        assert!(!text.contains("CANARY"), "{text}");
    }
}

fn idle_view(
    quote: &wisp_tui::app::config_screen::Quote,
) -> wisp_tui::app::config_screen::View<'_> {
    wisp_tui::app::config_screen::View {
        quote,
        edit: None,
        context: wisp_tui::app::config_screen::Context {
            window: None,
            room: None,
            max_tokens: 0,
        },
        locked: None,
        sites: false,
    }
}

#[tokio::test]
async fn a_key_in_a_given_env_file_is_named_with_its_file_under_the_flag() {
    let dir = crate::scratch_dir("screen-agreement-given-file");
    let config = dir.write("config.toml", GATEWAY);
    let file = dir.write("keys.env", KEY_IN_THE_FILE);
    let start = started(
        &dir,
        &config,
        &gateway_keys,
        None,
        &[
            "--endpoint",
            "manifest",
            "--env-file",
            file.to_str().unwrap(),
        ],
        None,
    );
    let snapshot = ask(start.agent, Vec::new()).await;
    let row = snapshot
        .settings
        .iter()
        .find(|r| r.name.spec().name == "endpoint")
        .unwrap();
    let under: Vec<(String, &str)> = row
        .shadowed
        .iter()
        .map(|f| (f.value.text(), f.origin.as_str()))
        .collect();
    let named = format!("ANTHROPIC_API_KEY is set in {}", file.display());
    assert_eq!(
        under[0],
        ("claude-api".to_owned(), named.as_str()),
        "{under:?}"
    );
    assert!(!format!("{snapshot:?}").contains("from-the-file"));
    let shown = rows(&snapshot, &Display::default());
    agrees(&shown, &start.layers, &snapshot);
}

const KEY_SHAPED_NAME: &str = "gateway-eu-central-1x";

const KEY_SHAPED: &str = "default = \"direct\"\n\n[endpoints.gateway-eu-central-1x]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"https://gw.example.com\"\n\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"DIRECT_KEY\"\n";

#[tokio::test]
async fn a_configured_name_that_looks_like_a_key_shows_in_full() {
    assert!(wisp_protocol::looks_like_key(KEY_SHAPED_NAME));
    let dir = crate::scratch_dir("screen-agreement-key-shaped-name");
    let config = dir.write("config.toml", KEY_SHAPED);
    let flagged = started(
        &dir,
        &config,
        &gateway_keys,
        None,
        &["--endpoint", KEY_SHAPED_NAME],
        None,
    );
    let snapshot = ask(flagged.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        (KEY_SHAPED_NAME, "flag")
    );
    assert!(!format!("{snapshot:?}").contains(wisp_protocol::NOT_SHOWN));
    agrees(&shown, &flagged.layers, &snapshot);

    let chosen = started(
        &dir,
        &config,
        &gateway_keys,
        Some(KEY_SHAPED_NAME),
        &[],
        None,
    );
    let snapshot = ask(chosen.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(
        (endpoint.value.as_str(), endpoint.source.as_str()),
        (KEY_SHAPED_NAME, "variable")
    );
    agrees(&shown, &chosen.layers, &snapshot);

    let under = started(
        &dir,
        &config,
        &gateway_keys,
        Some(KEY_SHAPED_NAME),
        &["--endpoint", "direct"],
        None,
    );
    let snapshot = ask(under.agent, Vec::new()).await;
    let shown = rows(&snapshot, &Display::default());
    let endpoint = endpoint_row(&shown);
    assert_eq!(endpoint.shadowed[0].value, KEY_SHAPED_NAME);
    assert_eq!(endpoint.shadowed[0].word(), "variable");
    agrees(&shown, &under.layers, &snapshot);
}

#[tokio::test]
async fn clearing_a_session_endpoint_falls_back_to_the_flag_s_real_name() {
    let dir = crate::scratch_dir("screen-agreement-clear-to-flag");
    let config = dir.write("config.toml", KEY_SHAPED);
    let start = started(
        &dir,
        &config,
        &gateway_keys,
        None,
        &["--endpoint", KEY_SHAPED_NAME],
        None,
    );
    let endpoint = find("endpoint").unwrap();
    let set = ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![wisp_protocol::settings::SettingChange::set(
            endpoint,
            text("direct"),
        )],
        save: false,
    };
    let clear = ClientCommand::ChangeSettings {
        request: RequestId(2),
        changes: vec![wisp_protocol::settings::SettingChange {
            target: wisp_protocol::settings::SettingTarget::Name(
                wisp_protocol::settings::SettingName::of(endpoint),
            ),
            value: None,
        }],
        save: false,
    };
    let (seen, snapshot) = ask_seeing(start.agent, vec![set, clear]).await;
    let answered: Vec<String> = seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SettingsChanged {
                answers: Some(request),
                endpoint,
                ..
            } => Some(format!("{request:?} {endpoint}")),
            AgentEvent::Rejected {
                request: Some(request),
                reason,
                ..
            } => Some(format!("{request:?} refused: {reason}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        answered,
        [
            format!("{:?} direct", RequestId(1)),
            format!("{:?} {KEY_SHAPED_NAME}", RequestId(2)),
        ]
    );
    let row = snapshot
        .settings
        .iter()
        .find(|r| r.name.spec().name == "endpoint")
        .unwrap();
    assert_eq!(row.value, Some(text(KEY_SHAPED_NAME)));
    assert_eq!(row.layer, Layer::Flag.wire());
}
