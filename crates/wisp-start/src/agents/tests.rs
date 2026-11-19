use std::path::Path;
use std::sync::Arc;

use wisp_core::agent::{Connection, LogContext};
use wisp_core::endpoints::PathShown;
use wisp_core::provider::anthropic::{AnthropicProvider, Credential};
use wisp_core::session::Redactions;
use wisp_protocol::log::CredentialSource;
use wisp_protocol::settings::{SettingValue, Target, find};

use super::*;

fn file(text: &str) -> ConfigFile {
    ConfigFile::parse(Path::new("/h/config.toml"), PathShown::Default, text).unwrap()
}

fn connection(name: &str) -> Connection {
    let provider =
        AnthropicProvider::new(&Credential::ApiKey("sk-test".into()), "http://127.0.0.1:9")
            .unwrap();
    Connection {
        name: name.into(),
        provider: Arc::new(provider),
        context: LogContext {
            endpoint: "http://127.0.0.1:9".into(),
            endpoint_name: name.into(),
            credential_source: CredentialSource::Environment,
            redactions: Redactions::new(Vec::<String>::new()),
        },
        priced: true,
    }
}

fn view() -> EndpointView {
    EndpointView::new(
        &[connection("env"), connection("direct")],
        &[("gpt".into(), "no key in /opt/keys/gpt.env".into())],
        None,
    )
}

fn rows(config: Config<'_>, flags: &[Given]) -> Vec<(String, String)> {
    doctor_rows(config, flags, &PresetPlace::default(), Some(&view()))
}

fn pairs(rows: &[(String, String)]) -> Vec<(&str, &str)> {
    rows.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn preset_pairs(rows: &[(String, String)]) -> Vec<(&str, &str)> {
    let mut shown = pairs(rows);
    shown.retain(|(key, _)| key.starts_with("preset "));
    shown
}

#[test]
fn without_a_file_the_rows_show_the_defaults_and_no_presets() {
    let shown = rows(Config::Missing, &[]);
    assert_eq!(
        pairs(&shown),
        [
            ("max_running", "4 (default)"),
            ("max_tree_running", "none (default)"),
            ("max_agents", "50 (default)"),
            ("max_depth", "2 (default)"),
            ("presets", "none"),
        ]
    );
    assert_eq!(
        doctor_text(&shown),
        "\nagents\n  max_running        4 (default)\n  max_tree_running   none (default)\n  max_agents         50 (default)\n  max_depth          2 (default)\n  presets            none\n"
    );
}

#[test]
fn a_broken_file_says_the_presets_were_not_read() {
    let shown = rows(Config::Broken, &[]);
    assert_eq!(shown.last().unwrap().1, NOT_LOADED);
    assert_eq!(shown.len(), 5);
}

#[test]
fn the_rows_name_where_each_limit_comes_from_and_every_preset() {
    let config = file(
        "[agents]\nmax_running = 2\n\n[agents.presets.reviewer]\ndescription = \"Reviews\"\neffort = \"high\"\ntools_off = [\"show_diff\"]\nspawn = false\nworker = false\n\n[agents.presets.fast]\ndescription = \"Quick\"\nmodel = \"claude-haiku-4-5\"\neffort = \"high\"\n\n[agents.presets.broken]\ndescription = \"B\"\ntools_off = [\"shwo_diff\"]\n",
    );
    let flag = Given {
        target: Target::Name(find("agents.max_depth").unwrap()),
        value: SettingValue::Int(3),
        origin: "--set agents.max_depth=3".into(),
    };
    let shown = rows(Config::Read(&config), &[flag]);
    assert_eq!(
        pairs(&shown),
        [
            ("max_running", "2 (config.toml line 2)"),
            ("max_tree_running", "none (default)"),
            ("max_agents", "50 (default)"),
            ("max_depth", "3 (flag)"),
            (
                "preset reviewer",
                "Reviews \u{b7} effort high \u{b7} tools off show_diff \u{b7} no spawning \u{b7} not a worker"
            ),
            (
                "preset broken",
                "off (config.toml line 16): tools_off names shwo_diff, which is no tool; did you mean show_diff?"
            ),
            (
                "preset fast",
                "off (config.toml line 11): model claude-haiku-4-5 doesn't take effort high"
            ),
        ]
    );
}

#[test]
fn a_key_pasted_as_a_description_is_never_shown() {
    let config =
        file("[agents.presets.y]\ndescription = \"sk-ant-api03-CANARY0123456789abcdef\"\n");
    let shown = doctor_text(&rows(Config::Read(&config), &[]));
    assert!(!shown.contains("CANARY"), "{shown}");
    assert!(
        shown.contains("preset y           (looks like a key, not shown)"),
        "{shown}"
    );
}

fn dir(name: &str) -> crate::scratch::ScratchDir {
    let dir = crate::scratch_dir(&format!("presets-{name}"));
    dir.dir("conf/roles/planner");
    dir
}

#[test]
fn a_preset_row_names_its_instructions_and_a_leftover_report_key_turns_it_off() {
    let home = dir("rows");
    let conf = home.root.join("conf");
    std::fs::write(
        conf.join("roles/planner/AGENTS.md"),
        "Plan from the code.\n@style.md\n",
    )
    .unwrap();
    std::fs::write(conf.join("roles/planner/style.md"), "Be short.\n").unwrap();
    let text = "[agents.presets.planner]\ndescription = \"Plans\"\ninstructions = \"roles/planner\"\n\n[agents.presets.old]\ndescription = \"Old\"\nreport = \"review\"\n\n[agents.presets.lost]\ndescription = \"Lost\"\ninstructions = \"roles/missing.md\"\n";
    let config = ConfigFile::parse(&conf.join("config.toml"), PathShown::Default, text).unwrap();
    let place = PresetPlace::of_config(
        Some(&conf.join("config.toml")),
        Some(home.root.clone()),
        8_000,
    );
    let shown = doctor_rows(Config::Read(&config), &[], &place, Some(&view()));
    let presets = preset_pairs(&shown);
    assert_eq!(
        presets,
        [
            (
                "preset planner",
                "Plans \u{b7} instructions ~/conf/roles/planner/AGENTS.md (2 files, about 10 tokens)"
            ),
            (
                "preset lost",
                "off (config.toml line 9): instructions ~/conf/roles/missing.md doesn't exist"
            ),
            (
                "preset old",
                "off (config.toml line 5): report was removed: reports are free text now; shape them in the preset's instructions"
            ),
        ]
    );
}

#[test]
fn a_preset_row_names_its_endpoint_and_says_what_went_unchecked() {
    let config = file(
        "[agents.presets.coder]\ndescription = \"Codes\"\nendpoint = \"direct\"\nmodel = \"claude-haiku-4-5\"\n\n[agents.presets.reviewer]\ndescription = \"Reviews\"\nendpoint = \"gpt\"\nmodel = \"gpt-6-sol\"\n\n[agents.presets.lost]\ndescription = \"Lost\"\nendpoint = \"nowhere\"\n\n[agents.presets.keys]\ndescription = \"Keys\"\nendpoint = \"env\"\n",
    );
    let shown = rows(Config::Read(&config), &[]);
    let presets = preset_pairs(&shown);
    assert_eq!(
        presets,
        [
            (
                "preset coder",
                "Codes \u{b7} account direct \u{b7} model claude-haiku-4-5"
            ),
            ("preset keys", "Keys \u{b7} the keys-file account"),
            ("preset lost", "Lost \u{b7} account nowhere isn't set up"),
            (
                "preset reviewer",
                "Reviews \u{b7} account gpt (unavailable: no key in /opt/keys/gpt.env) \u{b7} model gpt-6-sol (not checked)"
            ),
        ]
    );
    let unchecked = doctor_rows(Config::Read(&config), &[], &PresetPlace::default(), None);
    let presets = preset_pairs(&unchecked);
    assert_eq!(
        presets,
        [
            (
                "preset coder",
                "Codes \u{b7} account direct (not checked) \u{b7} model claude-haiku-4-5 (not checked)"
            ),
            (
                "preset keys",
                "Keys \u{b7} the keys-file account (not checked)"
            ),
            ("preset lost", "Lost \u{b7} account nowhere (not checked)"),
            (
                "preset reviewer",
                "Reviews \u{b7} account gpt (not checked) \u{b7} model gpt-6-sol (not checked)"
            ),
        ]
    );
    let keyed = file(
        "[agents.presets.y]\ndescription = \"Y\"\nendpoint = \"sk-ant-api03-canary012345CANARYcdef\"\n",
    );
    for view in [Some(view()), None] {
        let shown = doctor_text(&doctor_rows(
            Config::Read(&keyed),
            &[],
            &PresetPlace::default(),
            view.as_ref(),
        ));
        assert!(!shown.contains("canary"), "{shown}");
    }
}

#[test]
fn a_preset_row_names_each_fallback_and_its_own_account_note() {
    let config = file(
        "[agents.presets.review]\ndescription = 'Reviews'\nendpoint = 'direct'\nfallback = [{endpoint = 'direct', model = 'claude-opus-5-5', effort = 'high'}, {endpoint = 'later', model = 'claude-opus-5-5'}, {endpoint = 'gpt'}]\n",
    );
    assert_eq!(
        preset_pairs(&rows(Config::Read(&config), &[])),
        [(
            "preset review",
            "Reviews · account direct · fallback 1: account direct, model claude-opus-5-5, effort high · fallback 2: account later isn't set up, model claude-opus-5-5 · fallback 3: account gpt (unavailable: no key in /opt/keys/gpt.env)"
        )]
    );
    let bad = file(
        "[agents.presets.review]\ndescription = 'Reviews'\nfallback = [{endpoint = 'direct'}, {endpoint = 'direct', model = 'gpt-9'}]\n",
    );
    assert!(
        preset_pairs(&rows(Config::Read(&bad), &[]))[0]
            .1
            .contains("fallback 2: model gpt-9 isn't one account direct serves")
    );
    let live = wisp_core::endpoints::ResolvedEndpoint {
        client_version: None,
        name: wisp_core::endpoints::EndpointName::parse("direct").unwrap(),
        provider: wisp_core::endpoints::ProviderKind::Anthropic,
        credential: Credential::ApiKey("CANARYKEY".into()),
        base_url: wisp_core::provider::anthropic::DEFAULT_BASE_URL.into(),
        source: wisp_core::provider::credential::Source::Environment,
        key_env: None,
        url_source: wisp_core::endpoints::UrlSource::Official,
        pricing: wisp_core::endpoints::PricingMode::List,
    };
    let mut ready = connection("direct");
    ready.provider = Arc::new(AnthropicProvider::from_endpoint(&live).unwrap());
    let view = EndpointView::new(&[ready], &[], None);
    for (pair, deferred) in [
        ("endpoint = 'direct'\nmodel = 'claude-opus-new'\n", true),
        ("model = 'claude-opus-new'\n", true),
        ("endpoint = 'direct'\nmodel = 'gemini-nova-new'\n", false),
        ("model = 'gemini-nova-new'\n", false),
        ("model = 'invalid model'\n", false),
    ] {
        let file = file(&format!(
            "[agents.presets.review]\ndescription = 'Reviews'\n{pair}"
        ));
        let checked = checked(file.presets.clone(), Some(&view), &PresetPlace::default());
        assert_eq!(checked.named("review").is_ok(), deferred, "{pair}");
        if deferred {
            assert_eq!(checked.note("review"), Some(&EndpointNote::NotChecked));
            assert!(
                preset_pairs(&doctor_rows(
                    Config::Read(&file),
                    &[],
                    &PresetPlace::default(),
                    Some(&view)
                ))[0]
                    .1
                    .contains("model claude-opus-new (not checked)")
            );
        }
    }
}
