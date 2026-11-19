use std::path::Path;

use super::*;
use crate::provider::anthropic::{ConfigError, Settings};
use crate::secrets::NoSecrets;
use crate::test_guard::absent;
use crate::test_guard::name;
use wisp_protocol::log::CredentialSource;
use wisp_protocol::settings::{Applies, Kind, SETTINGS, Secrecy, find};

fn var(text: &str) -> VariableName {
    VariableName::parse(text).unwrap()
}

const PATH: &str = "/home/u/.config/wisp/config.toml";
const ENV_PATH: &str = "/home/u/.config/wisp/.env";

fn parse(text: &str) -> Result<ConfigFile, EndpointError> {
    ConfigFile::parse(Path::new(PATH), PathShown::Default, text)
}

fn error(text: &str) -> String {
    parse(text).unwrap_err().to_string()
}

fn at(line: usize, message: &str) -> String {
    format!("{PATH} line {line}: {message}")
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let vars = vars(pairs);
    move |name| vars.get(name).cloned()
}

fn env_file(pairs: &[(&str, &str)]) -> EnvFile {
    EnvFile {
        path: ENV_PATH.into(),
        vars: vars(pairs),
    }
}

const TWO: &str = r#"
default = "manifest"

[endpoints.direct]
auth = "api_key"
key_env = "WISP_DIRECT_KEY"

[endpoints.manifest]
provider = "anthropic"
auth = "bearer"
key_env = "MANIFEST_API_KEY"
url_env = "MANIFEST_BASE_URL"
pricing = "none"
"#;

#[test]
fn a_full_file_parses_every_field() {
    let config = parse(TWO).unwrap();
    assert_eq!(config.path, Path::new(PATH));
    assert_eq!(config.default, Some(name("manifest")));
    let names: Vec<&str> = config.endpoints.keys().map(EndpointName::as_str).collect();
    assert_eq!(names, ["direct", "manifest"]);
    assert_eq!(
        config.endpoints[&name("direct")],
        EndpointConfig {
            client_version: None,
            provider: ProviderKind::Anthropic,
            auth: AuthKind::ApiKey,
            key: KeyFrom::Variable(var("WISP_DIRECT_KEY")),
            url: None,
            url_env: None,
            pricing: PricingMode::List,
        }
    );
    assert_eq!(
        config.endpoints[&name("manifest")],
        EndpointConfig {
            client_version: None,
            provider: ProviderKind::Anthropic,
            auth: AuthKind::Bearer,
            key: KeyFrom::Variable(var("MANIFEST_API_KEY")),
            url: None,
            url_env: Some(var("MANIFEST_BASE_URL")),
            pricing: PricingMode::None,
        }
    );
    let literal = parse(
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\nurl = \"http://127.0.0.1:8080/x\"\n",
    )
    .unwrap();
    assert_eq!(
        literal.endpoints[&name("gw")].url.as_deref(),
        Some("http://127.0.0.1:8080/x")
    );
    assert_eq!(parse("").unwrap().endpoints.len(), 0);
    assert_eq!(parse("[endpoints]\n").unwrap().endpoints.len(), 0);
    assert_eq!(
        parse("default = \"env\"\n").unwrap().default,
        Some(EndpointName::env())
    );
    let inline = parse("endpoints.a = { auth = \"api_key\", key_env = \"A\" }\n").unwrap();
    assert_eq!(
        inline.endpoints[&name("a")]
            .key_env()
            .map(VariableName::raw),
        Some("A")
    );
}

#[test]
fn a_plugins_slots_table_holds_one_switch_per_section() {
    let file =
        parse("[plugins.context.slots]\nusage = true\n\n[plugins.context.hooks]\nfill = false\n")
            .unwrap();
    let saved: Vec<(String, SettingValue)> = file
        .settings
        .iter()
        .map(|(path, saved)| (path.to_string(), saved.value.clone()))
        .collect();
    assert_eq!(
        saved,
        vec![
            (
                "plugins.context.hooks.fill".to_owned(),
                SettingValue::Bool(false)
            ),
            (
                "plugins.context.slots.window".to_owned(),
                SettingValue::Bool(true)
            ),
        ]
    );
    for table in [
        "[plugins.context.slots]\nusage = false\nwindow = true\n",
        "[plugins.context.slots]\nwindow = true\nusage = false\n",
    ] {
        let file = parse(table).unwrap();
        let saved: Vec<(String, &SettingValue)> = file
            .settings
            .iter()
            .map(|(path, saved)| (path.to_string(), &saved.value))
            .collect();
        let want = (
            "plugins.context.slots.window".to_owned(),
            &SettingValue::Bool(true),
        );
        assert_eq!(saved, [want], "{table}");
    }
}

#[test]
fn the_instructions_table_holds_the_source_and_the_threshold() {
    assert_eq!(
        parse("").unwrap().instructions,
        InstructionsConfig::default()
    );
    let both = parse("[instructions]\nsource = \"default\"\nwarn_tokens = 100\n").unwrap();
    assert_eq!(
        both.instructions,
        InstructionsConfig {
            source: Some(wisp_protocol::InstructionsSource::Default),
            warn_tokens: Some(100),
        }
    );
    let tokens = parse("[instructions]\nwarn_tokens = \"8k\"\nsource = \"none\"\n").unwrap();
    assert_eq!(tokens.instructions.warn_tokens, Some(8_000));
    assert_eq!(
        tokens.instructions.source,
        Some(wisp_protocol::InstructionsSource::None)
    );
    for (written, value) in [("0x10", 16), ("0o10", 8), ("0b10", 2), ("8_000", 8_000)] {
        let file = parse(&format!("[instructions]\nwarn_tokens = {written}\n")).unwrap();
        assert_eq!(file.instructions.warn_tokens, Some(value), "{written}");
    }
}

#[test]
fn every_error_class_has_a_wisp_message() {
    let base = "[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"K\"\n";
    let cases: Vec<(String, String)> = vec![
        (
            format!("{base}color = \"blue\"\n"),
            at(
                4,
                "unknown field in [endpoints.manifest]; allowed: auth, background_compaction, background_tokens, client_version, key_env, key_store, mark, max_in_flight, models, native_web_search, pricing, provider, url, url_env",
            ),
        ),
        (
            "colour = 1\n".into(),
            at(
                1,
                "unknown field; allowed: agents, cache_ttl, claude_usage, compact_at, context, debug_cache, default, display, effort, endpoints, instructions, keep_warm, keychain, log, mcp, message_threads, model, models, plugins, preload, preset, refusal_fallback, retry, self_check, store_id, time, tool_output, tools, turned_off, voice",
            ),
        ),
        (
            "[plugins.Bad]\n".into(),
            at(
                1,
                "a plugin id, hook name or section name must be a-z, 0-9 and -, starting with a letter",
            ),
        ),
        (
            "[plugins.good.hooks]\nNo = false\n".into(),
            at(
                2,
                "a plugin id, hook name or section name must be a-z, 0-9 and -, starting with a letter",
            ),
        ),
        (
            "[plugins.good.slots]\nx = false\n".into(),
            at(
                2,
                "a plugin id, hook name or section name must be a-z, 0-9 and -, starting with a letter",
            ),
        ),
        (
            "[plugins.good.slots]\nusage = \"maybe\"\n".into(),
            at(2, "plugins.<id>.slots.<name> must be true or false"),
        ),
        (
            "[plugins.good]\nremoved = {}\n".into(),
            at(
                2,
                "[plugins.<id>] holds only the enabled and installed switches, the plugin's own settings and [plugins.<id>.hooks] and [plugins.<id>.slots] tables of switches",
            ),
        ),
        (
            "[plugins.good]\nenabled = \"maybe\"\n".into(),
            at(2, "plugins.<id>.enabled must be true or false"),
        ),
        (
            "[plugins.good.hooks]\nh = \"maybe\"\n".into(),
            at(2, "plugins.<id>.hooks.<hook> must be true or false"),
        ),
        (
            "[instructions]\nsize = 1\n".into(),
            at(
                2,
                "unknown field in [instructions]; allowed: extra, source, warn_tokens",
            ),
        ),
        (
            "[context]\nnote = false\n".into(),
            at(
                2,
                "unknown field in [context]; allowed: auto_compact, background_compaction, background_tokens, compact_effort, context_note, count_tokens, strategy, verbatim_user_tokens",
            ),
        ),
        (
            "instructions = 1\n".into(),
            at(1, "instructions must be a table"),
        ),
        (
            "[instructions]\nsource = \"all\"\n".into(),
            at(
                2,
                "instructions.source must be \"auto\", \"default\" or \"none\"",
            ),
        ),
        (
            "[instructions]\nwarn_tokens = -1\n".into(),
            at(
                2,
                "instructions.warn_tokens must be a token count, such as 8000 or \"8k\"",
            ),
        ),
        (
            "[instructions]\nwarn_tokens = \"\"\n".into(),
            at(
                2,
                "instructions.warn_tokens must be a token count, such as 8000 or \"8k\"",
            ),
        ),
        (
            format!("{base}key = \"sk-ant\"\n"),
            at(
                4,
                "secrets never go in config.toml: store the key with wisp account add, or name its variable with key_env",
            ),
        ),
        (
            "[endpoints.m]\nauth = 7\nkey_env = \"K\"\n".into(),
            at(2, "auth must be a string"),
        ),
        (
            "[endpoints.m]\nauth = \"basic\"\nkey_env = \"K\"\n".into(),
            at(
                2,
                "auth must be \"api_key\", \"bearer\" or \"anthropic_subscription\"",
            ),
        ),
        (
            format!("{base}pricing = \"cheap\"\n"),
            at(4, "pricing must be \"list\" or \"none\""),
        ),
        (
            format!("{base}provider = \"openai\"\n"),
            at(2, "auth must be \"api_key\" or \"chatgpt_subscription\""),
        ),
        (
            format!("{base}native_web_search = \"yes\"\n"),
            at(4, "native_web_search must be true or false"),
        ),
        (
            format!("{base}background_compaction = 1\n"),
            at(4, "background_compaction must be true or false"),
        ),
        (
            format!("{base}provider = \"unknown\"\n"),
            at(
                4,
                "provider must be \"anthropic\", \"openai\" or \"gemini\"",
            ),
        ),
        (
            format!("{base}provider = 1\n"),
            at(4, "provider must be a string"),
        ),
        (
            "[endpoints.manifest]\nkey_env = \"K\"\n".into(),
            format!("{PATH}: [endpoints.manifest] needs auth"),
        ),
        (
            "[endpoints.manifest]\nauth = \"bearer\"\n".into(),
            format!("{PATH}: [endpoints.manifest] needs key_env or key_store"),
        ),
        (
            "\n\n[endpoints.Manifest]\nauth = \"bearer\"\n".into(),
            at(3, &wisp_protocol::account_name_error()),
        ),
        (
            "[endpoints.env]\nauth = \"bearer\"\nkey_env = \"K\"\n".into(),
            at(
                2,
                "the keys-file account in config.toml takes no connection fields: its connection comes from the ANTHROPIC_* variables; add a named account to configure one",
            ),
        ),
        (
            "[endpoints.m]\nauth = \"bearer\"\nkey_env = \"1K\"\n".into(),
            at(3, "key_env must be a variable name (letters, digits and _)"),
        ),
        (
            format!("{base}url_env = \"MY-URL\"\n"),
            at(4, "url_env must be a variable name (letters, digits and _)"),
        ),
        (
            format!("{base}url = \"ftp://gw.example.com\"\n"),
            at(4, "url must be an http or https URL"),
        ),
        (
            format!("{base}url = \"https://gw.example.com\"\nurl_env = \"U\"\n"),
            at(5, "[endpoints.manifest] has both url and url_env; keep one"),
        ),
        (
            format!("default = \"other\"\n{base}"),
            at(1, "default names no account in this file"),
        ),
        ("default = 3\n".into(), at(1, "default must be a string")),
        (
            "\nendpoints = 3\n".into(),
            at(2, "endpoints must be a table of [endpoints.<name>] tables"),
        ),
        (
            "\n[endpoints]\nm = \"x\"\n".into(),
            at(3, "[endpoints] entries must be tables"),
        ),
        (
            "a = 1\nb = = 2\n".into(),
            format!("{PATH} line 2 is not valid TOML; nothing was loaded"),
        ),
    ];
    for (text, want) in cases {
        assert_eq!(error(&text), want, "{text}");
    }
}

#[test]
fn name_rules() {
    for good in ["a", "0", "direct", "my-gw_2", &"a".repeat(32)] {
        assert!(EndpointName::parse(good).is_some(), "{good}");
    }
    for bad in ["", "-a", "_a", "A", "a b", "a.b", "é", &"a".repeat(33)] {
        assert!(EndpointName::parse(bad).is_none(), "{bad}");
    }
    assert!(EndpointName::env().is_env());
    for secret in ["key", "api_key", "token", "secret"] {
        let text = format!("[endpoints.m]\nauth = \"bearer\"\n{secret} = \"v\"\n");
        assert!(error(&text).contains("name its variable with key_env"));
    }
    assert!(error("token = \"v\"\n").contains("name its variable with key_env"));
}

const CANARIES: &[(&str, &str)] = &[
    (
        "auth",
        "[endpoints.m]\nauth = \"CANARYAUTH01\"\nkey_env = \"K\"\n",
    ),
    (
        "pricing",
        "[endpoints.m]\nauth = \"bearer\"\nkey_env = \"K\"\npricing = \"CANARYPRICE02\"\n",
    ),
    (
        "provider",
        "[endpoints.m]\nauth = \"bearer\"\nkey_env = \"K\"\nprovider = \"CANARYPROV03\"\n",
    ),
    ("default", "default = \"CANARYDEFAULT04\"\n"),
    ("auth int", "[endpoints.m]\nauth = 7\nkey_env = \"K\"\n"),
    (
        "key_env array",
        "[endpoints.m]\nauth = \"bearer\"\nkey_env = [\"CANARYARRAY05\"]\n",
    ),
    (
        "key_env value",
        "[endpoints.m]\nauth = \"bearer\"\nkey_env = \"CANARY-VAR06\"\n",
    ),
    (
        "secret field",
        "[endpoints.m]\nauth = \"bearer\"\nkey = \"sk-ant-CANARYKEY07\"\n",
    ),
    (
        "unknown key name",
        "[endpoints.m]\nauth = \"bearer\"\nCANARYKEYNAME08 = 1\n",
    ),
    (
        "unknown key value",
        "[endpoints.m]\nauth = \"bearer\"\nother = \"CANARYVALUE09\"\n",
    ),
    ("top unknown key", "CANARYTOP10 = \"CANARYTOPVALUE11\"\n"),
    (
        "table name",
        "[endpoints.CANARYNAME12]\nauth = \"bearer\"\n",
    ),
    ("endpoints string", "endpoints = \"CANARYSHAPE13\"\n"),
    ("entry string", "[endpoints]\nm = \"CANARYENTRY14\"\n"),
    ("syntax", "a = 1\nb = \"CANARYSYNTAX15\" = = 2\n"),
    (
        "url",
        "[endpoints.m]\nauth = \"bearer\"\nkey_env = \"K\"\nurl = \"ftp://CANARYUSER16:CANARYPASS17@h/CANARYPATH18?CANARYQUERY19\"\n",
    ),
    (
        "both urls",
        "[endpoints.m]\nauth = \"bearer\"\nkey_env = \"K\"\nurl = \"https://CANARYUSER20:CANARYPASS21@h/CANARYPATH22?CANARYQUERY23\"\nurl_env = \"U\"\n",
    ),
];

#[test]
fn no_error_or_debug_output_holds_a_value_from_the_file() {
    for (case, text) in CANARIES {
        let err = parse(text).unwrap_err();
        let shown = format!("{err} {err:?}").to_ascii_uppercase();
        assert!(!shown.contains("CANARY"), "{case}: {shown}");
    }
}

#[test]
fn a_literal_url_never_shows_in_debug_output() {
    let text = "[endpoints.m]\nauth = \"bearer\"\nkey_env = \"K\"\nurl = \"https://CANARYUSER:CANARYPASS@gw.example.com/CANARYPATH?k=CANARYQUERY\"\n";
    let config = parse(text).unwrap();
    assert!(config.endpoints[&name("m")].url_has_credentials());
    let shown = format!("{config:?}");
    assert!(!shown.contains("CANARY"), "{shown}");
    assert!(shown.contains("https://gw.example.com"), "{shown}");
    let env = env_of(&[("K", "tok-CANARYSECRET")]);
    let book =
        EndpointBook::resolve(Some(&config), &NoSecrets, &absent(&env, None, false), None).unwrap();
    let shown = format!("{book:?}");
    assert!(!shown.contains("CANARY"), "{shown}");
    let plain = parse(
        "[endpoints.m]\nauth = \"bearer\"\nkey_env = \"K\"\nurl = \"https://gw.example.com/x\"\n",
    )
    .unwrap();
    assert!(!plain.endpoints[&name("m")].url_has_credentials());
}

fn config(text: &str) -> ConfigFile {
    parse(text).unwrap()
}

fn ready(book: &BTreeMap<EndpointName, Availability>, n: &str) -> ResolvedEndpoint {
    let typed = name(n);
    match &book[account(book, &typed)] {
        Availability::Ready(r) => r.clone(),
        other => panic!("{n}: {other:?}"),
    }
}

fn reason(book: &BTreeMap<EndpointName, Availability>, n: &str) -> String {
    let typed = name(n);
    match &book[account(book, &typed)] {
        Availability::Unavailable(r) => r.reason.clone(),
        other => panic!("{n}: {other:?}"),
    }
}

#[test]
fn each_endpoint_reads_all_its_variables_from_one_source() {
    let config = config(TWO);
    let file = env_file(&[
        ("MANIFEST_API_KEY", "file-tok"),
        ("MANIFEST_BASE_URL", "https://gw.example.com/anthropic"),
    ]);
    let entries = EndpointBook::entries(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[]), Some(&file), false),
    );
    let manifest = ready(&entries, "manifest");
    assert_eq!(
        manifest.credential,
        Credential::AuthToken("file-tok".into())
    );
    assert_eq!(manifest.base_url, "https://gw.example.com/anthropic");
    assert_eq!(manifest.source, Source::File(ENV_PATH.into()));
    assert_eq!(
        manifest.url_source,
        UrlSource::Variable(var("MANIFEST_BASE_URL"), Source::File(ENV_PATH.into()))
    );
    assert_eq!(manifest.key_var().raw(), "MANIFEST_API_KEY");
    assert_eq!(manifest.pricing, PricingMode::None);
    assert!(!manifest.priced());
    assert_eq!(manifest.gateway_host().as_deref(), Some("gw.example.com"));
    assert_eq!(
        reason(&entries, "direct"),
        format!("WISP_DIRECT_KEY is not set in the environment or {ENV_PATH}")
    );

    let env = env_of(&[("MANIFEST_API_KEY", "env-tok")]);
    let entries =
        EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, Some(&file), false));
    assert_eq!(
        reason(&entries, "manifest"),
        "MANIFEST_BASE_URL is not set in the environment, where MANIFEST_API_KEY was found"
    );

    let env = env_of(&[
        ("MANIFEST_API_KEY", " env-tok "),
        ("MANIFEST_BASE_URL", "http://127.0.0.1:9"),
        ("WISP_DIRECT_KEY", ""),
    ]);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    let manifest = ready(&entries, "manifest");
    assert_eq!(manifest.credential, Credential::AuthToken("env-tok".into()));
    assert_eq!(manifest.source, Source::Environment);
    assert!(!manifest.plain_http());
    assert_eq!(
        reason(&entries, "direct"),
        "WISP_DIRECT_KEY is not set in the environment"
    );

    let env = env_of(&[
        ("MANIFEST_API_KEY", "t"),
        ("MANIFEST_BASE_URL", "not a url CANARYURL"),
    ]);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    assert_eq!(
        reason(&entries, "manifest"),
        "MANIFEST_BASE_URL is not an http or https URL"
    );

    let env = env_of(&[("WISP_DIRECT_KEY", "sk-ant-x")]);
    let direct = ready(
        &EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false)),
        "direct",
    );
    assert_eq!(direct.base_url, "https://api.anthropic.com");
    assert_eq!(direct.url_source, UrlSource::Official);
    assert!(direct.official());
    assert_eq!(direct.host().as_deref(), Some("api.anthropic.com"));
    assert_eq!(direct.credential, Credential::ApiKey("sk-ant-x".into()));
}

#[test]
fn values_are_never_expanded_and_bad_header_values_are_unavailable() {
    let config = config(TWO);
    let file = env_file(&[
        ("MANIFEST_API_KEY", "${HOME}"),
        ("MANIFEST_BASE_URL", "https://gw.example.com"),
        ("WISP_DIRECT_KEY", "bad\u{7f}CANARYBAD"),
    ]);
    let entries = EndpointBook::entries(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[("HOME", "/h")]), Some(&file), false),
    );
    assert_eq!(
        ready(&entries, "manifest").credential,
        Credential::AuthToken("${HOME}".into())
    );
    assert_eq!(
        reason(&entries, "direct"),
        "WISP_DIRECT_KEY contains characters that can't be sent in an HTTP header"
    );
}

#[test]
fn the_env_endpoint_sits_beside_named_ones_and_hides_as_a_duplicate() {
    let config = config(TWO);
    let env = env_of(&[
        ("ANTHROPIC_AUTH_TOKEN", "same-tok"),
        ("ANTHROPIC_BASE_URL", "https://gw.example.com/anthropic/"),
        ("MANIFEST_API_KEY", "same-tok"),
        ("MANIFEST_BASE_URL", "https://gw.example.com/anthropic"),
    ]);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    assert_eq!(
        entries[&EndpointName::env()],
        Availability::SameAs(name("manifest"))
    );
    assert_eq!(
        entries[&name("gateway")],
        Availability::SameAs(name("manifest"))
    );
    assert_eq!(env_account(&entries), Some(&name("manifest")));
    assert_eq!(keys_file_account(&entries), None);

    let env = env_of(&[
        ("ANTHROPIC_AUTH_TOKEN", "other-tok"),
        ("ANTHROPIC_BASE_URL", "https://gw2.example.com"),
        ("MANIFEST_API_KEY", "same-tok"),
        ("MANIFEST_BASE_URL", "https://gw.example.com/anthropic"),
    ]);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    let implicit = ready(&entries, "env");
    assert_eq!(implicit.name, name("gateway"));
    assert_eq!(
        entries[&EndpointName::env()],
        Availability::SameAs(name("gateway"))
    );
    assert_eq!(keys_file_account(&entries), Some(&name("gateway")));
    assert_eq!(implicit.key_var().raw(), "ANTHROPIC_AUTH_TOKEN");
    assert_eq!(implicit.key_env, None);
    assert_eq!(
        implicit.url_source,
        UrlSource::Variable(var("ANTHROPIC_BASE_URL"), Source::Environment)
    );

    let env = env_of(&[("ANTHROPIC_API_KEY", "k"), ("ANTHROPIC_AUTH_TOKEN", "t")]);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    assert!(reason(&entries, "env").contains("are both set"));
    assert!(reason(&entries, "claude-api").contains("are both set"));
    let err = EndpointBook::resolve(Some(&config), &NoSecrets, &absent(&env, None, false), None)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        Settings::resolve(&env, None).unwrap_err().to_string()
    );

    let entries = EndpointBook::entries(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[]), None, false),
    );
    assert!(!entries.contains_key(&EndpointName::env()));

    for pair in [
        ("ANTHROPIC_API_KEY", "sk-ant-CANARY"),
        ("ANTHROPIC_AUTH_TOKEN", "CANARY-token"),
    ] {
        let entries = EndpointBook::entries(
            Some(&config),
            &NoSecrets,
            &absent(&env_of(&[pair]), None, false),
        );
        assert_eq!(
            ready(&entries, "env").name,
            name("claude-api"),
            "{}",
            pair.0
        );
        assert_eq!(
            listed(&entries)
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["claude-api", "direct", "manifest"]
        );
    }
}

#[test]
fn a_saved_account_holding_the_keys_file_name_keeps_it_and_says_so() {
    let table = "[endpoints.claude-api]\nauth = \"api_key\"\nkey_env = \"SAVED_KEY\"\n";
    let defaulted = config(&format!("default = \"env\"\n{table}"));
    let config = config(table);
    let saved = [("SAVED_KEY", "sk-ant-CANARY-saved")];
    let other = [
        ("SAVED_KEY", "sk-ant-CANARY-saved"),
        ("ANTHROPIC_API_KEY", "sk-ant-CANARY-other"),
    ];
    let env = env_of(&other);
    let variables = absent(&env, None, false);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &variables);
    assert_eq!(
        listed(&entries).map(|(n, _)| n).collect::<Vec<_>>(),
        [&name("claude-api")]
    );
    assert_eq!(
        ready(&entries, "claude-api").key_env,
        Some(var("SAVED_KEY"))
    );
    let said = "claude-api is a saved account, so the key in ANTHROPIC_API_KEY isn't used. Rename one to use both.";
    assert_eq!(shadowed(&entries), Some(said));
    for reason in [StartReason::Flag, StartReason::Variable] {
        let chosen = EndpointBook::resolve(
            Some(&config),
            &NoSecrets,
            &variables,
            Some(Choice {
                name: "env",
                reason,
            }),
        );
        assert!(
            matches!(
                chosen,
                Err(EndpointError::EndpointUnavailable { ref name, ref reason })
                    if name.is_env() && reason == said
            ),
            "{chosen:?}"
        );
    }
    let defaulted = EndpointBook::resolve(Some(&defaulted), &NoSecrets, &variables, None);
    assert!(
        matches!(
            defaulted,
            Err(EndpointError::DefaultUnavailable { ref name, ref reason })
                if name.is_env() && reason == said
        ),
        "{defaulted:?}"
    );
    assert_eq!(
        EndpointBook::resolve(Some(&config), &NoSecrets, &variables, None)
            .unwrap()
            .start,
        name("claude-api")
    );

    let file = env_file(&[("ANTHROPIC_API_KEY", "sk-ant-CANARY-file")]);
    let env = env_of(&saved);
    let variables = absent(&env, Some(&file), false);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &variables);
    let line = shadowed(&entries).unwrap();
    assert!(
        line.starts_with("claude-api is a saved account, so the key in Wisp's keys file (")
            && line.ends_with(".env) isn't used. Rename one to use both."),
        "{line}"
    );
    assert!(!line.contains("CANARY"), "{line}");

    let same = [
        ("SAVED_KEY", "sk-ant-CANARY-saved"),
        ("ANTHROPIC_API_KEY", "sk-ant-CANARY-saved"),
    ];
    let env = env_of(&same);
    let variables = absent(&env, None, false);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &variables);
    assert_eq!(
        entries[&EndpointName::env()],
        Availability::SameAs(name("claude-api"))
    );
    assert_eq!(shadowed(&entries), None);

    let env = env_of(&saved);
    let variables = absent(&env, None, false);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &variables);
    assert_eq!(shadowed(&entries), None);
}

#[test]
fn the_log_context_redacts_every_ready_endpoint() {
    let config = config(TWO);
    let env = env_of(&[
        ("WISP_DIRECT_KEY", "sk-ant-CANARYDIRECT"),
        ("MANIFEST_API_KEY", "CANARYTOKEN"),
        (
            "MANIFEST_BASE_URL",
            "https://u:CANARYPASS@gw.example.com/CANARYPATH?CANARYQUERY",
        ),
    ]);
    let book =
        EndpointBook::resolve(Some(&config), &NoSecrets, &absent(&env, None, false), None).unwrap();
    assert_eq!(book.start, name("manifest"));
    let context = book.log_context();
    assert_eq!(context.endpoint, "https://gw.example.com");
    assert_eq!(context.endpoint_name, "manifest");
    assert_eq!(
        context.credential_source,
        CredentialSource::Mode {
            billing: wisp_protocol::Billing::Metered,
            source: Box::new(CredentialSource::Environment),
        }
    );
    let scrubbed = context
        .redactions
        .scrub("sk-ant-CANARYDIRECT CANARYTOKEN CANARYPASS /CANARYPATH CANARYQUERY");
    assert!(!scrubbed.contains("CANARY"), "{scrubbed}");
}

#[test]
fn choosing_an_unknown_or_unavailable_endpoint_fails() {
    let config = config(TWO);
    let env = env_of(&[("WISP_DIRECT_KEY", "k")]);
    let flag = |n| {
        Some(Choice {
            name: n,
            reason: StartReason::Flag,
        })
    };
    let resolve = |choice| {
        EndpointBook::resolve(
            Some(&config),
            &NoSecrets,
            &absent(&env, None, false),
            choice,
        )
    };
    assert_eq!(
        resolve(flag("work")).unwrap_err().to_string(),
        "no account \"work\" (available: direct)"
    );
    assert_eq!(
        resolve(flag("CANARY Name")).unwrap_err().to_string(),
        "no account by that name (available: direct)"
    );
    let key = &"sk-ant-api03-canarywrCANARYce01".to_ascii_lowercase();
    assert!(EndpointName::parse(key).is_some());
    for reason in [StartReason::Flag, StartReason::Variable] {
        let said = resolve(Some(Choice { name: key, reason }))
            .unwrap_err()
            .to_string();
        assert_eq!(
            said,
            "no account by that name (looks like a key, not shown) (available: direct)"
        );
    }
    assert_eq!(
        resolve(flag("manifest")).unwrap_err().to_string(),
        "account \"manifest\" is unavailable: MANIFEST_API_KEY is not set in the environment"
    );
    assert!(
        resolve(flag("env"))
            .unwrap_err()
            .to_string()
            .starts_with("the keys-file account is unavailable: no Anthropic credentials")
    );
    let book = resolve(flag("direct")).unwrap();
    assert_eq!(book.start_reason, StartReason::Flag);
}

#[derive(Clone, Copy, Debug)]
enum Pick {
    None,
    Direct,
    Gone,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    flag: Pick,
    variable: Pick,
    exported: bool,
    explicit_file: bool,
    file_credential: bool,
    default: Pick,
    direct_ready: bool,
    manifest_ready: bool,
}

const ALL_PICKS: [Pick; 3] = [Pick::None, Pick::Direct, Pick::Gone];

fn expected(c: Case) -> Result<(&'static str, StartReason), &'static str> {
    let lookup = |pick: Pick| match pick {
        Pick::Direct if c.direct_ready => Ok("direct"),
        Pick::Direct => Err("unavailable"),
        _ => Err("unknown"),
    };
    let chosen = match (c.flag, c.variable) {
        (Pick::None, Pick::None) => None,
        (Pick::None, v) => Some((v, StartReason::Variable)),
        (f, _) => Some((f, StartReason::Flag)),
    };
    if let Some((pick, reason)) = chosen {
        return lookup(pick).map(|n| (n, reason));
    }
    let env_available = c.exported || c.file_credential;
    if c.exported || (c.explicit_file && c.file_credential) {
        return Ok((wisp_protocol::CLAUDE_API, StartReason::Exported));
    }
    match c.default {
        Pick::None => {}
        pick => {
            return lookup(pick)
                .map(|n| (n, StartReason::Default))
                .map_err(|e| if e == "unavailable" { "default" } else { e });
        }
    }
    if env_available {
        return Ok((wisp_protocol::CLAUDE_API, StartReason::EnvFile));
    }
    match (c.direct_ready, c.manifest_ready) {
        (true, false) => Ok(("direct", StartReason::OnlyOne)),
        (false, true) => Ok(("manifest", StartReason::OnlyOne)),
        (true, true) => Err("not chosen"),
        (false, false) => Err("none available"),
    }
}

#[test]
fn the_start_precedence_table_holds_for_every_combination() {
    let mut checked = 0;
    for flag in ALL_PICKS {
        for variable in ALL_PICKS {
            for default in [Pick::None, Pick::Direct] {
                for bits in 0..32u8 {
                    let case = Case {
                        flag,
                        variable,
                        exported: bits & 1 != 0,
                        explicit_file: bits & 2 != 0,
                        file_credential: bits & 4 != 0,
                        default,
                        direct_ready: bits & 8 != 0,
                        manifest_ready: bits & 16 != 0,
                    };
                    check(case);
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 576);
}

fn check(c: Case) {
    let mut text = String::new();
    if let Pick::Direct = c.default {
        text.push_str("default = \"direct\"\n");
    }
    text.push_str(
        "[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"DIRECT_KEY\"\n[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl = \"https://gw.example.com\"\n",
    );
    let config = config(&text);
    let mut env_pairs = vec![];
    if c.exported {
        env_pairs.push(("ANTHROPIC_API_KEY", "exported-key"));
    }
    if c.direct_ready {
        env_pairs.push(("DIRECT_KEY", "direct-key"));
    }
    if c.manifest_ready {
        env_pairs.push(("GW_KEY", "gw-key"));
    }
    let file = c
        .file_credential
        .then(|| env_file(&[("ANTHROPIC_AUTH_TOKEN", "file-token")]));
    let name_of = |p: Pick| match p {
        Pick::None => None,
        Pick::Direct => Some("direct"),
        Pick::Gone => Some("gone"),
    };
    let choice = match (name_of(c.flag), name_of(c.variable)) {
        (Some(n), _) => Some(Choice {
            name: n,
            reason: StartReason::Flag,
        }),
        (None, Some(n)) => Some(Choice {
            name: n,
            reason: StartReason::Variable,
        }),
        (None, None) => None,
    };
    let got = EndpointBook::resolve(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&env_pairs), file.as_ref(), c.explicit_file),
        choice,
    );
    match (expected(c), got) {
        (Ok((start, reason)), Ok(book)) => {
            assert_eq!(book.start.as_str(), start, "{c:?}");
            assert_eq!(book.start_reason, reason, "{c:?}");
            assert_eq!(book.active.name.as_str(), start, "{c:?}");
            let noted = reason == StartReason::Exported && matches!(c.default, Pick::Direct);
            assert_eq!(book.overridden.is_some(), noted, "{c:?}");
        }
        (Err(kind), Err(e)) => {
            let text = e.to_string();
            let fits = match kind {
                "unknown" => text.starts_with("no account \"gone\""),
                "unavailable" => text.starts_with("account \"direct\" is unavailable"),
                "default" => text.starts_with("the default account \"direct\" is unavailable"),
                "not chosen" => {
                    text == "no account chosen: use --account (available: direct, manifest)"
                }
                _ => text.starts_with("no account is available: direct: DIRECT_KEY is not set"),
            };
            assert!(fits, "{c:?}: {text}");
        }
        (want, got) => panic!("{c:?}: wanted {want:?}, got {got:?}"),
    }
}

#[test]
fn an_exported_key_wins_over_the_default_with_a_note() {
    let config = config(TWO);
    let env = env_of(&[
        ("ANTHROPIC_API_KEY", "sk-ant-exported"),
        ("MANIFEST_API_KEY", "t"),
        ("MANIFEST_BASE_URL", "https://gw.example.com"),
    ]);
    let book =
        EndpointBook::resolve(Some(&config), &NoSecrets, &absent(&env, None, false), None).unwrap();
    assert_eq!(book.start, EndpointName::keys_file(true));
    assert_eq!(
        book.overridden.unwrap().note(),
        "Using ANTHROPIC_API_KEY from the environment, not the default account \"manifest\" (--account manifest to use it)"
    );
    let explicit = env_file(&[("ANTHROPIC_AUTH_TOKEN", "file-token")]);
    let env = env_of(&[
        ("MANIFEST_API_KEY", "t"),
        ("MANIFEST_BASE_URL", "https://gw.example.com"),
    ]);
    let book = EndpointBook::resolve(
        Some(&config),
        &NoSecrets,
        &absent(&env, Some(&explicit), true),
        None,
    )
    .unwrap();
    assert_eq!(
        book.overridden.unwrap().note(),
        format!(
            "Using ANTHROPIC_AUTH_TOKEN from {ENV_PATH}, not the default account \"manifest\" (--account manifest to use it)"
        )
    );
    let book = EndpointBook::resolve(
        Some(&config),
        &NoSecrets,
        &absent(&env, Some(&explicit), false),
        None,
    )
    .unwrap();
    assert_eq!(book.start, name("manifest"));
    assert_eq!(book.overridden, None);

    let twin = env_of(&[
        ("ANTHROPIC_AUTH_TOKEN", "t"),
        ("ANTHROPIC_BASE_URL", "https://gw.example.com"),
        ("MANIFEST_API_KEY", "t"),
        ("MANIFEST_BASE_URL", "https://gw.example.com"),
    ]);
    let book = EndpointBook::resolve(Some(&config), &NoSecrets, &absent(&twin, None, false), None)
        .unwrap();
    assert_eq!(book.start, name("manifest"));
    assert_eq!(book.start_reason, StartReason::Exported);
    assert_eq!(book.overridden, None);
}

#[test]
fn without_a_config_resolution_is_todays() {
    let file_cases: [Option<EnvFile>; 4] = [
        None,
        Some(env_file(&[])),
        Some(env_file(&[("ANTHROPIC_API_KEY", "file-key")])),
        Some(env_file(&[
            ("ANTHROPIC_AUTH_TOKEN", "file-tok"),
            ("ANTHROPIC_BASE_URL", "https://gw.example.com/x"),
        ])),
    ];
    let env_cases: [&[(&str, &str)]; 6] = [
        &[],
        &[("ANTHROPIC_API_KEY", "env-key")],
        &[("ANTHROPIC_AUTH_TOKEN", " env-tok ")],
        &[("ANTHROPIC_API_KEY", "k"), ("ANTHROPIC_AUTH_TOKEN", "t")],
        &[
            ("ANTHROPIC_API_KEY", ""),
            ("ANTHROPIC_BASE_URL", "https://gw"),
        ],
        &[("ANTHROPIC_API_KEY", "bad\nkey")],
    ];
    for file in &file_cases {
        for pairs in env_cases {
            for explicit in [false, true] {
                let env = env_of(pairs);
                let today = Settings::resolve(&env, file.as_ref());
                let book = EndpointBook::resolve(
                    None,
                    &NoSecrets,
                    &absent(&env, file.as_ref(), explicit),
                    None,
                );
                match (today, book) {
                    (Ok(settings), Ok(book)) => {
                        assert!(!book.configured);
                        assert_eq!(book.active.credential, settings.credential);
                        assert_eq!(book.active.base_url, settings.base_url);
                        assert_eq!(book.active.source, settings.source);
                        let mut expected = settings.log_context();
                        expected.credential_source = CredentialSource::Mode {
                            billing: wisp_protocol::Billing::Metered,
                            source: Box::new(expected.credential_source),
                        };
                        assert_eq!(book.start.as_str(), expected.endpoint_name);
                        assert_eq!(book.log_context(), expected);
                        assert_eq!(book.entries.len(), 2);
                        assert_eq!(
                            book.entries[&EndpointName::env()],
                            Availability::SameAs(book.start.clone())
                        );
                    }
                    (Err(today), Err(book)) => assert_eq!(today.to_string(), book.to_string()),
                    (today, book) => panic!("{pairs:?} {file:?}: {today:?} vs {book:?}"),
                }
            }
        }
    }
    let env = env_of(&[("ANTHROPIC_API_KEY", "k")]);
    let choice = |name| {
        Some(Choice {
            name,
            reason: StartReason::Variable,
        })
    };
    let book =
        EndpointBook::resolve(None, &NoSecrets, &absent(&env, None, false), choice("env")).unwrap();
    assert_eq!(book.start, name("claude-api"));
    assert_eq!(book.start_reason, StartReason::Variable);
    assert_eq!(
        EndpointBook::resolve(
            None,
            &NoSecrets,
            &absent(&env, None, false),
            choice("direct")
        )
        .unwrap_err()
        .to_string(),
        "no account \"direct\" (available: claude-api)"
    );
    let missing = EndpointBook::resolve(None, &NoSecrets, &absent(&env_of(&[]), None, false), None)
        .unwrap_err();
    assert!(matches!(
        missing,
        EndpointError::Anthropic(ConfigError::MissingCredentials { .. })
    ));
}

#[test]
fn provider_kinds_dispatch_through_one_table() {
    assert_eq!(
        ProviderKind::parse("anthropic"),
        Some(ProviderKind::Anthropic)
    );
    assert_eq!(ProviderKind::parse("Anthropic"), None);
    assert_eq!(ProviderKind::default(), ProviderKind::Anthropic);
    for kind in ProviderKind::ALL {
        assert!(ProviderKind::accepted().contains(kind.as_str()));
        assert!(http_url(kind.default_url()).is_some());
        assert!(!kind.implicit_variables().is_empty());
    }
    assert_eq!(
        ProviderKind::Anthropic.auth("api_key"),
        Some(AuthKind::ApiKey)
    );
    assert_eq!(ProviderKind::Anthropic.auth("cli"), None);
}

#[test]
fn a_named_endpoint_connects_with_its_own_variable_in_errors() {
    let endpoint = ResolvedEndpoint {
        client_version: None,
        name: name("gw"),
        provider: ProviderKind::Anthropic,
        credential: Credential::AuthToken("bad\nCANARY".into()),
        base_url: "https://gw.example.com".into(),
        source: Source::Environment,
        key_env: Some(var("GW_KEY")),
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let err = ProviderKind::Anthropic
        .connect(&endpoint, None)
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        err,
        "GW_KEY contains characters that can't be sent in an HTTP header"
    );
    let ok = ResolvedEndpoint {
        credential: Credential::AuthToken("tok".into()),
        ..endpoint
    };
    assert!(ProviderKind::Anthropic.connect(&ok, None).is_ok());
}

#[test]
fn plain_http_is_flagged_unless_loopback() {
    let at = |url: &str| ResolvedEndpoint {
        client_version: None,
        name: name("gw"),
        provider: ProviderKind::Anthropic,
        credential: Credential::AuthToken("t".into()),
        base_url: url.into(),
        source: Source::Environment,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    assert!(at("http://gw.example.com").plain_http());
    assert!(!at("http://127.0.0.1:8080").plain_http());
    assert!(!at("https://gw.example.com").plain_http());
    assert_eq!(
        at("https://GW.example.com:8443/x").host().as_deref(),
        Some("gw.example.com:8443")
    );
}

#[test]
fn every_secret_wisp_read_is_redacted_even_for_unavailable_endpoints() {
    let config = config(
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\nurl = \"https://gw.example.com\"\n[endpoints.half]\nauth = \"bearer\"\nkey_env = \"HALF\"\nurl_env = \"HALF_URL\"\n[endpoints.badurl]\nauth = \"bearer\"\nkey_env = \"BAD\"\nurl_env = \"BAD_URL\"\n[endpoints.badheader]\nauth = \"api_key\"\nkey_env = \"BADHEADER\"\n",
    );
    let env = env_of(&[
        ("BADHEADER", "bad\nCANARYHEADERKEY"),
        ("GW", "gw-token"),
        ("ANTHROPIC_API_KEY", "sk-ant-CANARYBOTHKEY"),
        ("ANTHROPIC_AUTH_TOKEN", "CANARYBOTHTOKEN"),
        ("ANTHROPIC_BASE_URL", "https://h/CANARYENVPATH"),
        ("HALF", "CANARYHALFKEY"),
        ("BAD", "CANARYBADKEY"),
        ("BAD_URL", "CANARYBADURL"),
    ]);
    let flag = Some(Choice {
        name: "gw",
        reason: StartReason::Flag,
    });
    let book =
        EndpointBook::resolve(Some(&config), &NoSecrets, &absent(&env, None, false), flag).unwrap();
    assert_eq!(book.start, name("gw"));
    assert!(reason(&book.entries, "env").contains("are both set"));
    assert!(reason(&book.entries, "half").contains("HALF_URL is not set"));
    assert!(reason(&book.entries, "badurl").contains("BAD_URL is not an http"));
    assert!(reason(&book.entries, "badheader").contains("can't be sent in an HTTP header"));
    let scrubbed = book.log_context().redactions.scrub(
        "sk-ant-CANARYBOTHKEY CANARYBOTHTOKEN /CANARYENVPATH CANARYHALFKEY CANARYBADKEY CANARYBADURL bad\nCANARYHEADERKEY",
    );
    assert!(!scrubbed.contains("CANARY"), "{scrubbed}");
    let shown = format!("{book:?}");
    assert!(!shown.contains("CANARY"), "{shown}");

    let file = env_file(&[
        ("ANTHROPIC_API_KEY", "sk-ant-CANARYFILEKEY"),
        ("ANTHROPIC_AUTH_TOKEN", "CANARYFILETOKEN"),
    ]);
    let book = EndpointBook::resolve(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[("GW", "gw-token")]), Some(&file), false),
        flag,
    )
    .unwrap();
    assert!(reason(&book.entries, "env").contains("are both set"));
    let scrubbed = book
        .log_context()
        .redactions
        .scrub("sk-ant-CANARYFILEKEY CANARYFILETOKEN");
    assert!(!scrubbed.contains("CANARY"), "{scrubbed}");
}

#[test]
fn row_five_starts_on_the_twin_of_a_hidden_env() {
    let config = config(
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\nurl = \"https://gw.example.com\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"D\"\n",
    );
    let file = env_file(&[
        ("ANTHROPIC_AUTH_TOKEN", "t"),
        ("ANTHROPIC_BASE_URL", "https://gw.example.com"),
        ("GW", "t"),
        ("D", "k"),
    ]);
    let book = EndpointBook::resolve(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[]), Some(&file), false),
        None,
    )
    .unwrap();
    assert_eq!(
        book.entries[&EndpointName::env()],
        Availability::SameAs(name("gw"))
    );
    assert_eq!(book.start, name("gw"));
    assert_eq!(book.start_reason, StartReason::EnvFile);
}

#[test]
fn a_default_of_env_starts_on_the_keys_file_account_or_is_unavailable() {
    let config = config("default = \"env\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\n");
    let keys = env_file(&[("ANTHROPIC_API_KEY", "sk-ant-CANARY-file")]);
    let gw = env_of(&[("GW", "t")]);
    let book = EndpointBook::resolve(
        Some(&config),
        &NoSecrets,
        &absent(&gw, Some(&keys), false),
        None,
    )
    .unwrap();
    assert_eq!(
        (book.start.as_str(), book.start_reason),
        ("claude-api", StartReason::Default)
    );
    for reason in [StartReason::Flag, StartReason::Variable] {
        let chosen = EndpointBook::resolve(
            Some(&config),
            &NoSecrets,
            &absent(&gw, Some(&keys), false),
            Some(Choice {
                name: "env",
                reason,
            }),
        )
        .unwrap();
        assert_eq!(
            (chosen.start.as_str(), chosen.start_reason),
            ("claude-api", reason)
        );
    }

    let err = EndpointBook::resolve(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[("GW", "t")]), None, false),
        None,
    )
    .unwrap_err();
    let missing = ConfigError::MissingCredentials {
        looked_in: "the environment".into(),
    }
    .to_string();
    assert!(
        matches!(err, EndpointError::DefaultUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        format!(
            "the keys-file account, where new chats start, is unavailable: {missing} (wisp account list shows your accounts)"
        )
    );
}

#[test]
fn env_with_a_bad_header_value_is_unavailable_under_a_config() {
    let config = config("[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\n");
    let env = env_of(&[("ANTHROPIC_API_KEY", "bad\nCANARYHEADER"), ("GW", "t")]);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    assert_eq!(
        reason(&entries, "env"),
        "ANTHROPIC_API_KEY contains characters that can't be sent in an HTTP header"
    );
    let book = EndpointBook::resolve(
        Some(&config),
        &NoSecrets,
        &absent(&env, None, false),
        Some(Choice {
            name: "gw",
            reason: StartReason::Flag,
        }),
    )
    .unwrap();
    assert!(
        !book
            .log_context()
            .redactions
            .scrub("bad\nCANARYHEADER")
            .contains("CANARY")
    );
}

#[test]
fn a_url_with_only_a_query_or_only_userinfo_holds_credentials() {
    for (url, holds) in [
        ("https://gw.example.com/x?k=v", true),
        ("https://user@gw.example.com/x", true),
        ("https://:pass@gw.example.com/x", true),
        ("https://gw.example.com/x", false),
    ] {
        let text = format!("[endpoints.m]\nauth = \"bearer\"\nkey_env = \"K\"\nurl = \"{url}\"\n");
        assert_eq!(
            config(&text).endpoints[&name("m")].url_has_credentials(),
            holds,
            "{url}"
        );
    }
}

#[test]
fn a_login_auth_kind_is_refused_for_now() {
    assert_eq!(
        error("[endpoints.m]\nauth = \"login\"\nkey_store = \"file\"\n"),
        at(
            2,
            "auth must be \"api_key\", \"bearer\" or \"anthropic_subscription\""
        )
    );
}

#[test]
fn a_cli_auth_kind_is_refused_for_now() {
    assert_eq!(
        error("[endpoints.m]\nauth = \"cli\"\nkey_env = \"K\"\n"),
        at(
            2,
            "auth must be \"api_key\", \"bearer\" or \"anthropic_subscription\""
        )
    );
}

#[test]
fn a_whole_file_never_shows_a_value_in_debug_output() {
    let text = "default = \"manifest\"\n[endpoints.manifest]\nprovider = \"anthropic\"\nauth = \"bearer\"\nkey_env = \"CANARY_VAR_A\"\nurl = \"https://CANARYUSER:CANARYPASS@gw.example.com/CANARYPATH?CANARYQUERY\"\npricing = \"none\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"CANARY_VAR_B\"\n";
    let config = config(text);
    let shown = format!("{config:?}");
    for canary in ["CANARYUSER", "CANARYPASS", "CANARYPATH", "CANARYQUERY"] {
        assert!(!shown.contains(canary), "{canary} in {shown}");
    }
    let env = env_of(&[
        ("CANARY_VAR_A", "CANARYTOKENA"),
        ("CANARY_VAR_B", "CANARYKEYB"),
    ]);
    let book =
        EndpointBook::resolve(Some(&config), &NoSecrets, &absent(&env, None, false), None).unwrap();
    let shown = format!("{book:?}");
    for canary in [
        "CANARYTOKENA",
        "CANARYKEYB",
        "CANARYUSER",
        "CANARYPASS",
        "CANARYPATH",
        "CANARYQUERY",
    ] {
        assert!(!shown.contains(canary), "{canary} in {shown}");
    }
}

const STORE_ID: &str = "5f0c2a9e41d7b388";

fn stored_config(body: &str) -> ConfigFile {
    config(&format!("store_id = \"{STORE_ID}\"\n{body}"))
}

fn records(pairs: &[(&str, &str, Option<&str>)]) -> crate::secrets::Prefetched {
    use crate::secrets::{Prefetched, Secret, Stored, StoredRecord};
    Prefetched::from_records(
        pairs
            .iter()
            .map(|(n, key, url)| {
                (
                    name(n),
                    Stored {
                        record: StoredRecord::Key {
                            key: Secret::new(*key),
                            url: url.map(Secret::new),
                        },
                        owner: None,
                    },
                )
            })
            .collect(),
    )
}

const CREDENTIALS_PATH: &str = "/home/u/.config/wisp/credentials.json";

#[test]
fn a_stored_endpoint_parses_with_its_store_and_store_id() {
    let config = stored_config(
        "default = \"manifest\"\n[endpoints.manifest]\nauth = \"bearer\"\nkey_store = \"file\"\nurl = \"https://gw.example.com\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK\"\n",
    );
    assert_eq!(config.store_id.as_ref().map(|s| s.as_str()), Some(STORE_ID));
    assert_eq!(config.shown, PathShown::Default);
    let manifest = &config.endpoints[&name("manifest")];
    assert_eq!(manifest.key, KeyFrom::Store(StoreKind::File));
    assert_eq!(manifest.key_env(), None);
    assert_eq!(manifest.store(), Some(StoreKind::File));
    assert_eq!(
        config.endpoints[&name("work")]
            .key_env()
            .map(VariableName::raw),
        Some("WORK")
    );
    assert!(config.stores(StoreKind::File));
    assert!(!config.stores(StoreKind::Keyring));
    let given =
        ConfigFile::parse(Path::new(PATH), PathShown::Given, "default = \"env\"\n").unwrap();
    assert_eq!(given.shown, PathShown::Given);
    assert_eq!(given.store_id, None);
}

#[test]
fn stored_endpoint_config_errors_have_exact_words() {
    let id = format!("store_id = \"{STORE_ID}\"\n");
    for (text, want) in [
        (
            "[endpoints.m]\nauth = \"api_key\"\nkey_store = \"keyring\"\n".to_owned(),
            at(
                3,
                "key_store needs store_id at the top of the file (16 lowercase hex characters)",
            ),
        ),
        (
            format!("{id}[endpoints.m]\nauth = \"api_key\"\nkey_store = \"CANARYSTORE\"\n"),
            at(4, "key_store must be \"file\" or \"keyring\""),
        ),
        (
            format!(
                "{id}[endpoints.m]\nauth = \"api_key\"\nkey_env = \"K\"\nkey_store = \"file\"\n"
            ),
            at(5, "[endpoints.m] has both key_store and key_env; keep one"),
        ),
        (
            format!(
                "{id}[endpoints.m]\nauth = \"api_key\"\nkey_store = \"file\"\nkey_env = \"K\"\n"
            ),
            at(5, "[endpoints.m] has both key_store and key_env; keep one"),
        ),
        (
            format!(
                "{id}[endpoints.m]\nauth = \"bearer\"\nkey_store = \"file\"\nurl_env = \"U\"\n"
            ),
            at(
                5,
                "[endpoints.m] keeps its key in a store, so its URL is url or in the store, not url_env",
            ),
        ),
        (
            "[endpoints.m]\nauth = \"api_key\"\nkey_store = \"file\"\n".to_owned(),
            at(
                3,
                "key_store needs store_id at the top of the file (16 lowercase hex characters)",
            ),
        ),
        (
            "store_id = \"CANARYNOTANID\"\n".to_owned(),
            at(1, "store_id must be 16 lowercase hex characters"),
        ),
        (
            "store_id = 1234567890123456\n".to_owned(),
            at(1, "store_id must be a string"),
        ),
        (
            format!("{id}[endpoints.m]\nauth = \"api_key\"\n"),
            format!("{PATH}: [endpoints.m] needs key_env or key_store"),
        ),
        (
            format!(
                "{id}[endpoints.m]\nauth = \"api_key\"\nkey_store = \"file\"\nkey = \"CANARYKEY\"\n"
            ),
            at(
                5,
                "secrets never go in config.toml: store the key with wisp account add, or name its variable with key_env",
            ),
        ),
    ] {
        let err = error(&text);
        assert_eq!(err, want, "{text}");
        assert!(!err.contains("CANARY"), "{err}");
        assert!(!format!("{:?}", parse(&text).unwrap_err()).contains("CANARY"));
    }
}

#[test]
fn a_given_config_carries_its_origin_into_every_error() {
    let err = ConfigFile::parse(
        Path::new("/tmp/CANARYDIR/config.toml"),
        PathShown::Given,
        "nope = 1\n",
    )
    .unwrap_err();
    let EndpointError::File(file) = &err else {
        panic!("{err:?}");
    };
    assert_eq!(file.shown, PathShown::Given);
    assert!(!format!("{err:?}").contains("CANARYDIR"));
}

#[test]
fn a_stored_endpoint_resolves_from_its_record() {
    let config = stored_config(
        "[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"file\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\n[endpoints.lit]\nauth = \"bearer\"\nkey_store = \"file\"\nurl = \"https://lit.example.com/p\"\n",
    );
    let secrets = records(&[
        ("direct", "sk-ant-direct", None),
        (
            "gw",
            "  gw-token\n",
            Some("https://gw.example.com/CANARYPATH"),
        ),
        ("lit", "lit-token", None),
    ]);
    let entries =
        EndpointBook::entries(Some(&config), &secrets, &absent(&env_of(&[]), None, false));
    let direct = ready(&entries, "direct");
    assert_eq!(
        direct.credential,
        Credential::ApiKey("sk-ant-direct".into())
    );
    assert_eq!(direct.base_url, ProviderKind::Anthropic.default_url());
    assert_eq!(direct.url_source, UrlSource::Official);
    assert_eq!(direct.key_env, None);
    assert_eq!(
        direct.source,
        Source::CredentialFile {
            path: CREDENTIALS_PATH.into(),
            shown: PathShown::Default
        }
    );
    assert_eq!(direct.source.to_string(), CREDENTIALS_PATH);
    let gw = ready(&entries, "gw");
    assert_eq!(gw.credential, Credential::AuthToken("gw-token".into()));
    assert_eq!(gw.base_url, "https://gw.example.com/CANARYPATH");
    assert_eq!(gw.url_source, UrlSource::Stored(StoreKind::File));
    let lit = ready(&entries, "lit");
    assert_eq!(lit.base_url, "https://lit.example.com/p");
    assert_eq!(
        lit.url_source,
        UrlSource::Config {
            path: PATH.into(),
            shown: PathShown::Default
        }
    );
    let book = EndpointBook::resolve(
        Some(&config),
        &secrets,
        &absent(&env_of(&[]), None, false),
        Some(Choice {
            name: "gw",
            reason: StartReason::Flag,
        }),
    )
    .unwrap();
    let context = book.log_context();
    assert_eq!(
        context.credential_source,
        CredentialSource::Mode {
            billing: wisp_protocol::Billing::Metered,
            source: Box::new(CredentialSource::CredentialFile),
        }
    );
    let scrubbed = context
        .redactions
        .scrub("sk-ant-direct gw-token lit-token /CANARYPATH");
    assert!(!scrubbed.contains("token"), "{scrubbed}");
    assert!(!scrubbed.contains("CANARY"), "{scrubbed}");
    assert!(!scrubbed.contains("sk-ant"), "{scrubbed}");
    let shown = format!("{book:?}");
    assert!(
        !shown.contains("token") && !shown.contains("CANARY"),
        "{shown}"
    );
}

#[test]
fn a_stored_endpoint_is_unavailable_with_its_reason() {
    let config = stored_config(
        "[endpoints.both]\nauth = \"bearer\"\nkey_store = \"file\"\nurl = \"https://a.example.com\"\n[endpoints.missing]\nauth = \"api_key\"\nkey_store = \"file\"\n[endpoints.spaced]\nauth = \"api_key\"\nkey_store = \"file\"\n[endpoints.badurl]\nauth = \"bearer\"\nkey_store = \"file\"\n",
    );
    let secrets = records(&[
        (
            "both",
            "CANARYBOTH",
            Some("https://b.example.com/CANARYPATH"),
        ),
        ("spaced", "sk-ant CANARYSPACE", None),
        ("badurl", "CANARYBAD", Some("ftp://CANARYURL")),
    ]);
    let entries =
        EndpointBook::entries(Some(&config), &secrets, &absent(&env_of(&[]), None, false));
    assert_eq!(
        reason(&entries, "both"),
        "the URL is set in config.toml and in the store; keep one"
    );
    assert_eq!(
        reason(&entries, "missing"),
        format!(
            "no key for missing in {CREDENTIALS_PATH}. If this config.toml was copied from another folder, its keys stayed in that folder's credentials.json: copy that file here too, or run wisp account edit missing --replace-key"
        )
    );
    assert_eq!(
        reason(&entries, "spaced"),
        "the key for spaced in credentials.json has a space, a control character or a non-ASCII character, so it can't be sent in a header"
    );
    assert_eq!(
        reason(&entries, "badurl"),
        "the URL in badurl's record in credentials.json is not an http or https URL"
    );
    let Availability::Unavailable(missing) = &entries[&name("missing")] else {
        panic!()
    };
    assert_eq!(missing.official, None);
    assert_eq!(missing.auth, Some(AuthKind::ApiKey));
    let book = EndpointBook::resolve(Some(&stored_config(
            "[endpoints.both]\nauth = \"bearer\"\nkey_store = \"file\"\nurl = \"https://a.example.com\"\n[endpoints.ok]\nauth = \"api_key\"\nkey_store = \"file\"\n[endpoints.spaced]\nauth = \"api_key\"\nkey_store = \"file\"\n",
        )), &records(&[
            ("both", "CANARYBOTH", Some("https://b.example.com/CANARYPATH")),
            ("ok", "k", None),
            ("spaced", "sk-ant CANARYSPACE", None),
        ]), &absent(&env_of(&[]), None, false), None)
    .unwrap();
    let scrubbed = book
        .log_context()
        .redactions
        .scrub("CANARYBOTH /CANARYPATH sk-ant CANARYSPACE");
    assert!(!scrubbed.contains("CANARY"), "{scrubbed}");
    let shown = format!("{entries:?}");
    assert!(!shown.contains("CANARY"), "{shown}");

    let given = ConfigFile::parse(
        Path::new("/tmp/CANARYDIR/config.toml"),
        PathShown::Given,
        &format!(
            "store_id = \"{STORE_ID}\"\n[endpoints.m]\nauth = \"api_key\"\nkey_store = \"file\"\n"
        ),
    )
    .unwrap();
    let entries = EndpointBook::entries(
        Some(&given),
        &records(&[]),
        &absent(&env_of(&[]), None, false),
    );
    let why = reason(&entries, "m");
    assert!(
        why.starts_with("no key for m in credentials.json beside the file given with --config. "),
        "{why}"
    );
    assert!(!why.contains("CANARYDIR"), "{why}");
}

#[test]
fn a_store_error_makes_every_stored_endpoint_unavailable_and_leaves_the_rest() {
    let config = stored_config(
        "[endpoints.a]\nauth = \"api_key\"\nkey_store = \"file\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK\"\n",
    );
    let secrets =
        crate::secrets::Prefetched::failing(crate::secrets::StoreError::Syntax { line: 3 });
    let book = EndpointBook::resolve(
        Some(&config),
        &secrets,
        &absent(&env_of(&[("WORK", "w")]), None, false),
        None,
    )
    .unwrap();
    assert_eq!(book.start, name("work"));
    assert_eq!(book.start_reason, StartReason::OnlyOne);
    assert_eq!(
        reason(&book.entries, "a"),
        "credentials.json line 3: not valid JSON"
    );
}

#[test]
fn stored_endpoints_follow_todays_precedence() {
    let config = stored_config(
        "default = \"direct\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"file\"\n[endpoints.other]\nauth = \"api_key\"\nkey_store = \"file\"\n",
    );
    let secrets = records(&[
        ("direct", "sk-ant-stored", None),
        ("other", "sk-ant-other", None),
    ]);
    let resolve = |env: &[(&str, &str)], file: Option<&EnvFile>, explicit: bool, choice| {
        EndpointBook::resolve(
            Some(&config),
            &secrets,
            &absent(&env_of(env), file, explicit),
            choice,
        )
    };
    let book = resolve(&[], None, false, None).unwrap();
    assert_eq!(book.start, name("direct"));
    assert_eq!(book.start_reason, StartReason::Default);
    assert!(book.overridden.is_none());

    let exported = resolve(
        &[("ANTHROPIC_API_KEY", "sk-ant-exported")],
        None,
        false,
        None,
    )
    .unwrap();
    assert_eq!(exported.start, EndpointName::keys_file(true));
    assert_eq!(exported.start_reason, StartReason::Exported);
    assert_eq!(
        exported.overridden.as_ref().map(Override::note).as_deref(),
        Some(
            "Using ANTHROPIC_API_KEY from the environment, not the default account \"direct\" (--account direct to use it)"
        )
    );

    let flag = resolve(
        &[("ANTHROPIC_API_KEY", "sk-ant-exported")],
        None,
        false,
        Some(Choice {
            name: "other",
            reason: StartReason::Flag,
        }),
    )
    .unwrap();
    assert_eq!(flag.start, name("other"));

    let twin = resolve(&[("ANTHROPIC_API_KEY", "sk-ant-stored")], None, false, None).unwrap();
    assert_eq!(
        twin.entries[&EndpointName::env()],
        Availability::SameAs(name("direct"))
    );
    assert_eq!(twin.start, name("direct"));
    assert!(twin.overridden.is_none());

    let quiet_file = env_file(&[("ANTHROPIC_API_KEY", "sk-ant-file")]);
    let default_file = resolve(&[], Some(&quiet_file), false, None).unwrap();
    assert_eq!(default_file.start, name("direct"));

    let no_default = stored_config("[endpoints.only]\nauth = \"api_key\"\nkey_store = \"file\"\n");
    let only = EndpointBook::resolve(
        Some(&no_default),
        &records(&[("only", "sk-ant-only", None)]),
        &absent(&env_of(&[]), None, false),
        None,
    )
    .unwrap();
    assert_eq!(only.start, name("only"));
    assert_eq!(only.start_reason, StartReason::OnlyOne);

    let unavailable_default = EndpointBook::resolve(
        Some(&config),
        &records(&[("other", "sk-ant-other", None)]),
        &absent(&env_of(&[]), None, false),
        None,
    )
    .unwrap_err();
    assert!(
        unavailable_default
            .to_string()
            .starts_with("the default account \"direct\" is unavailable: no key for direct in "),
        "{unavailable_default}"
    );
}

#[test]
fn a_key_shaped_variable_name_loads_but_is_shown_nowhere() {
    let canary = "canary0000000000000key1";
    let url_canary = "canary0000000000000url2";
    let text = format!(
        "[endpoints.unset]\nauth = \"api_key\"\nkey_env = \"{canary}\"\n[endpoints.badurl]\nauth = \"bearer\"\nkey_env = \"GOOD_KEY\"\nurl_env = \"{url_canary}\"\n[endpoints.elsewhere]\nauth = \"bearer\"\nkey_env = \"OTHER_KEY\"\nurl_env = \"{url_canary}\"\n[endpoints.header]\nauth = \"api_key\"\nkey_env = \"{canary}\"\n[endpoints.ready]\nauth = \"bearer\"\nkey_env = \"READY_KEY\"\nurl_env = \"{url_canary}\"\n"
    );
    let config = parse(&text).unwrap();
    let mut shown = format!("{config:?}");
    let entries = EndpointBook::entries(
        Some(&config),
        &NoSecrets,
        &absent(
            &env_of(&[
                ("GOOD_KEY", "k"),
                (url_canary, "ftp://x"),
                ("READY_KEY", "k"),
            ]),
            None,
            false,
        ),
    );
    let unset = &entries[&name("unset")];
    let Availability::Unavailable(why) = unset else {
        panic!("{unset:?}");
    };
    assert_eq!(
        why.reason,
        "(looks like a key, not shown) is not set in the environment"
    );
    let Availability::Unavailable(why) = &entries[&name("badurl")] else {
        panic!();
    };
    assert_eq!(
        why.reason,
        "(looks like a key, not shown) is not an http or https URL"
    );
    let file = env_file(&[("OTHER_KEY", "k")]);
    let elsewhere = EndpointBook::entries(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[]), Some(&file), false),
    );
    let Availability::Unavailable(why) = &elsewhere[&name("elsewhere")] else {
        panic!();
    };
    assert_eq!(
        why.reason,
        format!(
            "(looks like a key, not shown) is not set in {ENV_PATH}, where OTHER_KEY was found"
        )
    );
    let header = EndpointBook::entries(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[(canary, "sk-ant-\u{7f}")]), None, false),
    );
    let Availability::Unavailable(why) = &header[&name("header")] else {
        panic!();
    };
    assert_eq!(
        why.reason,
        "(looks like a key, not shown) contains characters that can't be sent in an HTTP header"
    );
    let ready = EndpointBook::entries(
        Some(&config),
        &NoSecrets,
        &absent(
            &env_of(&[("READY_KEY", "k"), (url_canary, "https://gw.example.com")]),
            None,
            false,
        ),
    );
    let Availability::Ready(endpoint) = &ready[&name("ready")] else {
        panic!();
    };
    assert_eq!(
        endpoint.url_source,
        UrlSource::Variable(var(url_canary), Source::Environment)
    );
    for entries in [&entries, &elsewhere, &header, &ready] {
        shown.push_str(&format!("{entries:?}"));
    }
    shown.push_str(&format!("{endpoint:?} {}", endpoint.key_var()));
    assert!(!shown.contains(canary), "{shown}");
    assert!(!shown.contains(url_canary), "{shown}");
    assert!(shown.contains("(looks like a key, not shown)"));
}

#[test]
fn a_connected_endpoint_digests_the_prefix_its_requests_send() {
    let endpoint = ResolvedEndpoint {
        client_version: None,
        name: name("gw"),
        provider: ProviderKind::Anthropic,
        credential: Credential::AuthToken("tok".into()),
        base_url: "https://gw.example.com".into(),
        source: Source::Environment,
        key_env: Some(var("GW_KEY")),
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let provider = ProviderKind::Anthropic.connect(&endpoint, None).unwrap();
    let session = crate::agent::default_session();
    let messages = [wisp_protocol::Message::user_text("hi")];
    let request = crate::provider::Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: crate::provider::Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let digest = provider.cache_prefix(&request);
    assert!(digest.is_some());
    assert_eq!(digest, crate::provider::anthropic::sent_prefix(&request));
}

mod keychain_first;

const ENV_TABLE: &str = "[endpoints.env]\nnative_web_search = false\npricing = \"none\"\nbackground_compaction = false\nbackground_tokens = 30000\n\n[endpoints.env.models.\"claude-haiku-5-5\"]\ncontext_window = 200000\n";

#[test]
fn the_env_table_takes_settings_and_adds_no_named_endpoint() {
    let config = config(ENV_TABLE);
    assert!(config.endpoints.is_empty());
    assert_eq!(config.default, None);
    let table = config.env.as_ref().unwrap();
    assert_eq!(table.pricing, PricingMode::None);
    assert_eq!(table.line, 1);
    let rows = &config.endpoint_models[&name("env")];
    let models = crate::models::ConfigModels::parse(ENV_TABLE).unwrap();
    assert_eq!(&models.endpoints["env"], rows);
    assert_eq!(models.native_web_search.get("env"), Some(&false));
    for keys_file in ["claude-api", "gateway"] {
        assert_eq!(models.table(keys_file), "env");
    }
    assert_eq!(models.table("work"), "work");
    let named = crate::models::ConfigModels::parse(&format!(
        "{ENV_TABLE}\n[endpoints.claude-api.models.\"claude-haiku-5-5\"]\ncontext_window = 150000\n"
    ))
    .unwrap();
    assert_eq!(named.table("claude-api"), "claude-api");
    assert_eq!(named.table("gateway"), "env");

    let env = env_of(&[("ANTHROPIC_API_KEY", "k")]);
    let book =
        EndpointBook::resolve(Some(&config), &NoSecrets, &absent(&env, None, false), None).unwrap();
    assert_eq!(
        book.entries.keys().collect::<Vec<_>>(),
        [&name("claude-api"), &name("env")]
    );
    assert_eq!(book.start, name("claude-api"));
    assert_eq!(book.active.pricing, PricingMode::None);
    let bare = config_from_nothing(&env);
    assert_eq!(bare.start, book.start);
    assert_eq!(bare.start_reason, book.start_reason);
    assert_eq!(bare.entries.len(), book.entries.len());

    let none = EndpointBook::entries(
        Some(&config),
        &NoSecrets,
        &absent(&env_of(&[]), None, false),
    );
    assert!(none.is_empty(), "{none:?}");
}

fn config_from_nothing(env: &(dyn Fn(&str) -> Option<String> + Sync)) -> EndpointBook {
    EndpointBook::resolve(
        Some(&config("")),
        &NoSecrets,
        &absent(env, None, false),
        None,
    )
    .unwrap()
}

#[test]
fn the_env_table_refuses_each_connection_field_on_its_line() {
    let values = [
        ("auth", "\"bearer\""),
        ("key_env", "\"K\""),
        ("key_store", "\"file\""),
        ("provider", "\"anthropic\""),
        ("url", "\"https://gw.example.com\""),
        ("url_env", "\"U\""),
    ];
    assert_eq!(
        values.map(|(k, _)| k),
        crate::endpoints::ENV_CONNECTION_FIELDS
    );
    for field in crate::endpoints::ENV_CONNECTION_FIELDS {
        let spec = find(field).unwrap();
        assert_eq!(spec.applies, Applies::Connection, "{field}");
    }
    for (field, value) in values {
        let text = format!("[endpoints.env]\nnative_web_search = false\n{field} = {value}\n");
        assert_eq!(
            error(&text),
            at(
                3,
                "the keys-file account in config.toml takes no connection fields: its connection comes from the ANTHROPIC_* variables; add a named account to configure one",
            ),
            "{field}"
        );
    }
}

#[test]
fn a_hand_set_client_version_reaches_its_account() {
    let table = "[endpoints.claude]
auth = 'anthropic_subscription'
key_env = 'LOCAL_KEY'
";
    let config = config(&format!("{table}client_version = '0.168.0'\n"));
    let env = env_of(&[("LOCAL_KEY", "sk-ant-oat-CANARYclientfixture")]);
    let entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    assert!(
        ready(&entries, "claude")
            .credential
            .subscription()
            .is_some()
    );
    assert_eq!(
        ready(&entries, "claude").client_version,
        ClientVersion::parse("0.168.0")
    );
    assert_eq!(
        error(&format!("{table}client_version = '0.168'\n")),
        at(
            4,
            r#"client_version must be three dot-separated numbers, such as "1.2.3""#
        )
    );
}

#[test]
fn max_in_flight_takes_a_whole_number_or_none_on_the_env_and_named_tables() {
    let file = config(
        "[endpoints.env]\nmax_in_flight = 4\n\n[endpoints.gw]\nurl = \"https://gw.example.com\"\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nmax_in_flight = \"none\"\n",
    );
    let spec = wisp_protocol::settings::find("max_in_flight").unwrap();
    let saved = |endpoint: &str| {
        file.settings
            .get(&SitePath::new(Place::Endpoint(endpoint.to_owned()), spec))
            .map(|saved| saved.value.clone())
    };
    for keys_file in ["env", "claude-api", "gateway"] {
        assert_eq!(saved(keys_file), Some(SettingValue::Int(4)), "{keys_file}");
    }
    assert_eq!(saved("gw"), Some(SettingValue::Text("none".into())));
    let named = config(
        "[endpoints.env]\nmax_in_flight = 4\n\n[endpoints.claude-api]\nauth = \"api_key\"\nkey_env = \"K\"\nmax_in_flight = 2\n",
    );
    let at_claude_api = SitePath::new(Place::Endpoint("claude-api".to_owned()), spec);
    assert_eq!(
        named
            .settings
            .get(&at_claude_api)
            .map(|saved| saved.value.clone()),
        Some(SettingValue::Int(2))
    );
    for value in ["0", "65", "\"many\"", "true"] {
        for table in [
            "[endpoints.env]\n".to_owned(),
            "[endpoints.gw]\nurl = \"https://gw.example.com\"\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\n".to_owned(),
        ] {
            let text = format!("{table}max_in_flight = {value}\n");
            let line = text.lines().count();
            assert_eq!(
                error(&text),
                at(
                    line,
                    "max_in_flight must be a whole number from 1 to 64, or \"none\""
                ),
                "{text}"
            );
        }
    }
}

#[test]
fn every_ready_endpoint_logs_the_billing_mode_its_provider_bills() {
    use crate::provider::credential::SubscriptionKind;
    let subscription = |kind| Credential::Subscription {
        kind,
        secret: "token".into(),
        profile: None,
    };
    let cases = [
        (
            ProviderKind::OpenAi,
            Credential::ApiKey("sk-test".into()),
            "https://api.openai.com/v1",
            wisp_protocol::Billing::Metered,
        ),
        (
            ProviderKind::OpenAi,
            subscription(SubscriptionKind::ChatGpt),
            crate::provider::openai::CHATGPT_BASE_URL,
            wisp_protocol::Billing::ChatGptPlan,
        ),
        (
            ProviderKind::Anthropic,
            Credential::ApiKey("sk-ant-test".into()),
            "https://api.anthropic.com",
            wisp_protocol::Billing::Metered,
        ),
        (
            ProviderKind::Anthropic,
            subscription(SubscriptionKind::Anthropic),
            "https://api.anthropic.com",
            wisp_protocol::Billing::AnthropicPlan,
        ),
    ];
    for (provider, credential, base_url, billing) in cases {
        let endpoint = ResolvedEndpoint {
            client_version: None,
            name: name("work"),
            provider,
            credential,
            base_url: base_url.into(),
            source: Source::Environment,
            key_env: Some(var("WORK_KEY")),
            url_source: UrlSource::Official,
            pricing: PricingMode::List,
        };
        let connected = provider.connect(&endpoint, None).unwrap();
        assert_eq!(connected.billing(), billing, "{billing:?}");
        assert_eq!(
            endpoint.log_context().credential_source,
            CredentialSource::Mode {
                billing,
                source: Box::new(CredentialSource::Environment),
            },
            "{billing:?}"
        );
    }
}

#[test]
fn background_tokens_on_an_endpoint_takes_a_number_or_a_short_form() {
    let file = config(
        "[endpoints.env]\nbackground_tokens = \"40k\"\n\n[endpoints.gw]\nurl = \"https://gw.example.com\"\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nbackground_tokens = 40000\n",
    );
    let spec = wisp_protocol::settings::find("context.background_tokens").unwrap();
    for endpoint in ["env", "gw"] {
        assert_eq!(
            file.settings
                .get(&SitePath::new(Place::Endpoint(endpoint.to_owned()), spec))
                .map(|saved| saved.value.clone()),
            Some(SettingValue::Tokens(40_000)),
            "{endpoint}"
        );
    }
    for value in ["0", "\"lots\"", "true"] {
        let text = format!("[endpoints.env]\nbackground_tokens = {value}\n");
        assert_eq!(
            error(&text),
            at(
                2,
                "background_tokens must be a token count, such as 8000 or \"8k\""
            ),
            "{text}"
        );
    }
}

#[test]
fn the_registry_lists_every_word_the_core_parsers_take() {
    let providers: Vec<&str> = ProviderKind::ALL.iter().map(|p| p.as_str()).collect();
    assert_eq!(providers, wisp_protocol::settings::PROVIDERS);
    let mut auths: Vec<&str> = ProviderKind::ALL
        .iter()
        .flat_map(|p| p.auths())
        .map(|a| a.as_str())
        .collect();
    auths.sort_unstable();
    auths.dedup();
    let mut listed = wisp_protocol::settings::AUTHS.to_vec();
    listed.sort_unstable();
    assert_eq!(auths, listed);
    let mut every: Vec<&str> = AuthKind::ALL.map(AuthKind::as_str).to_vec();
    every.sort_unstable();
    assert_eq!(every, listed);
    let pricings = PricingMode::ALL.map(PricingMode::as_str);
    assert_eq!(pricings, wisp_protocol::settings::PRICINGS);
    let stores = StoreKind::ALL.map(StoreKind::as_str);
    assert_eq!(stores, wisp_protocol::settings::KEY_STORES);
    let choices = wisp_protocol::endpoint::StoreChoice::ALL.map(|c| c.as_str());
    assert_eq!(choices[1..], stores);
    for kind in StoreKind::ALL {
        let wire = wisp_protocol::endpoint::StoreKind::from(kind);
        assert_eq!(serde_json::to_value(wire).unwrap(), kind.as_str());
    }
    for provider in ProviderKind::ALL {
        for auth in provider.auths() {
            assert_eq!(provider.auth(auth.as_str()), Some(*auth));
            let credential = auth.credential("placeholder".to_owned());
            assert_eq!(AuthKind::of(&credential), *auth);
            assert_eq!(credential.subscription(), auth.subscription());
        }
    }
    for (kind, auth, provider) in [
        (
            SubscriptionKind::ChatGpt,
            AuthKind::ChatGptSubscription,
            ProviderKind::OpenAi,
        ),
        (
            SubscriptionKind::Anthropic,
            AuthKind::AnthropicSubscription,
            ProviderKind::Anthropic,
        ),
        (
            SubscriptionKind::Antigravity,
            AuthKind::AntigravitySubscription,
            ProviderKind::Gemini,
        ),
    ] {
        assert_eq!(auth.subscription(), Some(kind));
        assert_eq!(AuthKind::from(kind), auth);
        let endpoint = crate::subscription_login::subscription_endpoint(kind, StoreKind::File);
        assert_eq!((endpoint.provider, endpoint.auth), (provider, auth));
        assert!(provider.auths().contains(&auth));
    }
    assert_eq!(AuthKind::ApiKey.subscription(), None);
    assert_eq!(AuthKind::Bearer.subscription(), None);
    assert_eq!(
        AuthKind::Bearer.credential("placeholder".to_owned()),
        Credential::AuthToken("placeholder".to_owned())
    );
    let keychain = find("keychain").unwrap();
    let Kind::Choice(words) = keychain.kind else {
        panic!("keychain is a choice");
    };
    assert_eq!(
        crate::secrets::detect::KeychainSetting::ALL.map(|k| k.as_str()),
        words
    );
}

fn samples(kind: Kind) -> Vec<String> {
    let quoted = |text: &str| format!("\"{text}\"");
    match kind {
        Kind::ClientVersion => ["1.2.3", "0.170.0", "1.2", "01.2.3", "1.2.3-alpha"]
            .map(quoted)
            .into_iter()
            .chain(["1".into()])
            .collect(),
        Kind::Bool => vec!["true".into(), "false".into(), quoted("on")],
        Kind::Int { min, max, none } => {
            let mut out = vec![min.to_string(), max.to_string()];
            if none {
                out.push(quoted("none"));
            }
            out.extend([(max + 1).to_string(), "true".into()]);
            out
        }
        Kind::Tokens { min, max } => {
            assert!((min..=max).contains(&1_000_000));
            let mut out = vec![
                "1000000".into(),
                quoted("1000000"),
                quoted("1000k"),
                quoted("1M"),
            ];
            if min > 0 {
                out.push((min - 1).to_string());
            }
            out.extend(["-1".into(), "true".into()]);
            out
        }
        Kind::TokensOrPercent => vec![
            "400000".into(),
            quoted("400k"),
            quoted("85%"),
            quoted("101%"),
            "-1".into(),
            "true".into(),
        ],
        Kind::List(words) => {
            let mut out: Vec<String> = words.iter().map(|w| format!("[{}]", quoted(w))).collect();
            out.extend([
                "[]".into(),
                quoted("none"),
                format!("[{}]", quoted("not_a_word")),
                quoted("not_a_word"),
                "1".into(),
            ]);
            out
        }
        Kind::ModelIds => vec![
            format!("[{}]", quoted("synthetic-alias")),
            "[]".into(),
            quoted("none"),
            format!("[{}]", quoted("not a model id")),
            "1".into(),
        ],
        Kind::Choice(words) => {
            let mut out: Vec<String> = words.iter().map(|w| quoted(w)).collect();
            out.extend([quoted("not_a_word"), "1".into()]);
            out
        }
        Kind::VarName => vec![
            quoted("SYNTHETIC_VAR"),
            quoted("1BAD"),
            quoted("canary0000000000000key1"),
            "1".into(),
        ],
        Kind::Usd { none } => {
            let mut out = vec!["0.5".into(), quoted("1.25"), "2".into()];
            if none {
                out.push(quoted("none"));
            }
            out.extend(["-1".into(), quoted("lots"), "true".into()]);
            out
        }
        Kind::Secret
        | Kind::ModelId
        | Kind::EndpointName
        | Kind::AgentName
        | Kind::VoiceName
        | Kind::Language
        | Kind::Path
        | Kind::PluginId
        | Kind::ToolNames
        | Kind::Names
        | Kind::Preload
        | Kind::SectionNames => Vec::new(),
    }
}

const READ_ON_PURPOSE: [(&str, &str); 2] = [
    ("key_env", "\"canary0000000000000key1\""),
    ("url_env", "\"canary0000000000000key1\""),
];

fn table_text(site: Site, leaf: &str, sample: &str) -> String {
    let endpoint = |extra: &str| format!("[endpoints.e]\nauth = \"api_key\"\n{extra}");
    match site {
        Site::Endpoint => {
            let key = match leaf {
                "key_env" | "key_store" => "",
                _ => "key_env = \"SYNTHETIC_KEY\"\n",
            };
            let root = if leaf == "key_store" {
                "store_id = \"0000000000000000\"\n\n"
            } else {
                ""
            };
            format!("{root}{}", endpoint(&format!("{key}{leaf} = {sample}\n")))
        }
        Site::Model => format!("[models.\"synthetic-model\"]\n{leaf} = {sample}\n"),
        _ => format!(
            "{}\n[endpoints.e.models.\"synthetic-model\"]\n{leaf} = {sample}\n",
            endpoint("key_env = \"SYNTHETIC_KEY\"\n")
        ),
    }
}

#[test]
fn every_endpoint_and_model_field_reads_as_its_kind() {
    let mut on_purpose = 0;
    for site in [Site::Endpoint, Site::Model, Site::EndpointModel] {
        let place = match site {
            Site::Endpoint => Place::Endpoint("e".to_owned()),
            Site::Model => Place::Model("synthetic-model".to_owned()),
            _ => Place::EndpointModel {
                endpoint: "e".to_owned(),
                model: "synthetic-model".to_owned(),
            },
        };
        for spec in SETTINGS.iter().filter(|s| {
            s.is_built()
                && s.has_site(site)
                && s.secrecy == Secrecy::Plain
                && !(site == Site::Endpoint && matches!(s.name, "auth" | "provider"))
        }) {
            let leaf = spec.leaf();
            let mut accepted = 0;
            for sample in samples(spec.kind) {
                let line = format!("v = {sample}");
                let holder = toml::de::DeTable::parse(&line).unwrap();
                let value = holder.get_ref().get("v").unwrap().get_ref();
                let wanted = crate::config::from_toml(spec, value);
                let read = parse(&table_text(site, leaf, &sample));
                let agrees = match (&wanted, &read) {
                    (Some(v), Ok(file)) => {
                        file.settings
                            .get(&SitePath::new(place.clone(), spec))
                            .map(|saved| &saved.value)
                            == Some(v)
                    }
                    (None, Err(_)) => true,
                    _ => false,
                };
                accepted += usize::from(wanted.is_some());
                if let Some(value) = wanted.as_ref().filter(|_| site != Site::Endpoint) {
                    let mut row = crate::models::ModelRow::new("synthetic-model");
                    crate::config::set_row_value(&mut row, leaf, value);
                    let back = crate::config::row_value(&row, leaf);
                    assert_eq!(back.as_ref(), Some(value), "--set {leaf} = {sample}");
                }
                if READ_ON_PURPOSE.contains(&(leaf, sample.as_str())) {
                    assert!(wanted.is_none() && read.is_ok(), "{leaf} = {sample}");
                    on_purpose += 1;
                } else {
                    assert!(
                        agrees,
                        "{} {leaf} = {sample}: registry {wanted:?}, parser {:?}",
                        site.words(),
                        read.as_ref().map(|_| ()).map_err(ToString::to_string)
                    );
                }
            }
            assert!(accepted > 0, "{} {leaf}", site.words());
        }
    }
    assert_eq!(on_purpose, READ_ON_PURPOSE.len());
    for (leaf, sample) in READ_ON_PURPOSE {
        let spec = find(leaf).unwrap();
        let canary = sample.trim_matches('"').to_owned();
        assert_eq!(
            spec.shown(&SettingValue::Text(canary)),
            wisp_protocol::NOT_SHOWN
        );
    }
}

#[test]
fn a_declared_row_loads_from_its_plugins_table() {
    use wisp_protocol::plugin::RowKind;
    let spec = crate::plugin::testing::declared(
        "saved-row",
        RowKind::Int {
            min: 1,
            max: 60,
            default: 10,
        },
    );
    let file = parse("[plugins.saved-row]\nminutes = 25\n").unwrap();
    assert_eq!(file.settings.iter().next().unwrap().0.spec(), spec);
    assert_eq!(
        file.settings.iter().next().unwrap().1.value,
        SettingValue::Int(25)
    );
    for value in ["0", "61", "true", "\"25\""] {
        assert!(
            parse(&format!("[plugins.saved-row]\nminutes = {value}\n")).is_err(),
            "{value}"
        );
    }
}

#[test]
fn an_unknown_key_is_refused_where_the_plugin_defines_settings_and_skipped_where_none_does() {
    use wisp_protocol::plugin::RowKind;
    crate::plugin::testing::declared("strict-row", RowKind::Bool { default: false });
    let known = parse("[plugins.strict-row]\nminutes = true\n").unwrap();
    assert_eq!(
        known.settings.iter().next().unwrap().1.value,
        SettingValue::Bool(true)
    );
    let near = parse("[plugins.strict-row]\nminuts = true\n").unwrap_err();
    assert!(near.to_string().contains("minutes"), "{near}");
    assert!(parse("[plugins.strict-row]\nformer = true\n").is_err());
    let leftover = "[plugins.missing-row]\nminutes = 25\nold = \"kept\"\n";
    assert!(parse(leftover).unwrap().settings.is_empty());
    assert!(parse("[plugins.missing-row]\nenabld = true\n").is_err());
    assert!(parse("[plugins.missing-row]\nminutes = {}\n").is_err());
}

#[test]
fn gemini_api_and_gateway_endpoints_resolve_with_expected_urls_and_headers() {
    let config = parse(
        "[endpoints.gemini-api]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"GEMINI_API_KEY\"\n[endpoints.gemini-gw]\nprovider = \"gemini\"\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"https://gemini-gw.example.com/v1beta\"\n",
    )
    .unwrap();
    let entries = EndpointBook::entries(
        Some(&config),
        &records(&[]),
        &absent(
            &env_of(&[
                ("GEMINI_API_KEY", "AIzaCANARY_gemini_key_with_underscre_1"),
                ("GW_TOKEN", "gw-bearer-token-1"),
            ]),
            None,
            false,
        ),
    );
    let direct = ready(&entries, "gemini-api");
    assert_eq!(direct.provider, ProviderKind::Gemini);
    assert_eq!(direct.base_url, crate::provider::gemini::DEFAULT_BASE_URL);
    assert!(direct.official());
    assert_eq!(
        ProviderKind::Gemini.auth_words(AuthKind::ApiKey),
        "x-goog-api-key header (a Gemini API key)"
    );
    let gw = ready(&entries, "gemini-gw");
    assert_eq!(gw.provider, ProviderKind::Gemini);
    assert!(!gw.official());
    assert_eq!(gw.gateway_host().as_deref(), Some("gemini-gw.example.com"));
}

#[test]
fn antigravity_subscription_rejects_static_keys_and_non_google_urls_but_allows_googleapis_override()
{
    let bad_url = parse(
        "store_id = \"0000000000000000\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\nurl = \"https://evil.example.com\"\n",
    );
    assert!(bad_url.is_err());

    let bad_url_env = parse(
        "[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_env = \"GEMINI_API_KEY\"\nurl_env = \"GEMINI_BASE_URL\"\n",
    );
    assert!(bad_url_env.is_err());

    let prod_override = parse(
        "store_id = \"0000000000000000\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\nurl = \"https://cloudcode-pa.googleapis.com\"\n",
    )
    .unwrap();
    assert_eq!(
        prod_override.endpoints[&name("antigravity")].url.as_deref(),
        Some("https://cloudcode-pa.googleapis.com")
    );
    assert_eq!(
        prod_override.endpoints[&name("antigravity")].official(),
        Some(true)
    );
    assert_eq!(
        prod_override.endpoints[&name("antigravity")].default_model(),
        crate::provider::gemini::antigravity_default_model()
    );

    let stored = stored_config(
        "[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\n",
    );
    let entries = EndpointBook::entries(
        Some(&stored),
        &records(&[("antigravity", "static-token-not-oauth", None)]),
        &absent(&env_of(&[]), None, false),
    );
    assert_eq!(
        reason(&entries, "antigravity"),
        "Antigravity subscription requires wisp login"
    );

    let variable = parse(
        "[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_env = \"GEMINI_API_KEY\"\n",
    )
    .unwrap();
    let var_entries = EndpointBook::entries(
        Some(&variable),
        &records(&[]),
        &absent(
            &env_of(&[("GEMINI_API_KEY", "AIzaCANARY_gemini_key_with_underscre_1")]),
            None,
            false,
        ),
    );
    assert_eq!(
        reason(&var_entries, "antigravity"),
        "Antigravity subscription requires wisp login"
    );
}

#[test]
fn custom_origin_treats_each_subscriptions_official_base_url_as_official() {
    for (provider, kind, expected_url) in [
        (
            ProviderKind::Anthropic,
            SubscriptionKind::Anthropic,
            crate::provider::anthropic::DEFAULT_BASE_URL,
        ),
        (
            ProviderKind::OpenAi,
            SubscriptionKind::ChatGpt,
            crate::provider::openai::CHATGPT_BASE_URL,
        ),
        (
            ProviderKind::Gemini,
            SubscriptionKind::Antigravity,
            crate::provider::gemini::antigravity_base_url(),
        ),
    ] {
        assert_eq!(kind.base_url(), expected_url);
        let endpoint = ResolvedEndpoint {
            client_version: None,
            name: name("sub"),
            provider,
            credential: Credential::Subscription {
                kind,
                secret: "tok".to_owned(),
                profile: None,
            },
            base_url: expected_url.to_owned(),
            source: Source::Keyring,
            key_env: None,
            url_source: UrlSource::Official,
            pricing: PricingMode::List,
        };
        assert!(endpoint.official(), "{kind:?}: {expected_url}");
        assert_eq!(endpoint.custom_origin(), None);
    }
}

#[test]
fn gemini_default_model_and_requires_oauth_match_provider_contracts() {
    assert_eq!(
        default_model(ProviderKind::Gemini, None),
        crate::provider::gemini::default_model()
    );
    assert_eq!(
        default_model(
            ProviderKind::Gemini,
            Some(AuthKind::AntigravitySubscription)
        ),
        crate::provider::gemini::antigravity_default_model()
    );
    assert!(AuthKind::AntigravitySubscription.requires_oauth());
    assert!(AuthKind::ChatGptSubscription.requires_oauth());
    assert!(!AuthKind::AnthropicSubscription.requires_oauth());
    assert!(!AuthKind::ApiKey.requires_oauth());
    assert!(!AuthKind::Bearer.requires_oauth());
}

#[test]
fn off_providers_keep_accounts_and_their_twins_unavailable_until_the_next_read() {
    let config = config(
        r#"[endpoints.g1]
provider = "gemini"
auth = "api_key"
key_env = "GEMINI_KEY"
"#,
    );
    let env = env_of(&[("GEMINI_KEY", "CANARY_GEMINI_KEY")]);
    let mut entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    entries.insert(name("twin"), Availability::SameAs(name("g1")));
    let before = entries.clone();
    let off = BTreeMap::from([("gemini".to_owned(), false)]);
    without_providers(&mut entries, &off);
    for alias in ["g1", "twin"] {
        assert_eq!(reason(&entries, alias), "gemini is off");
        let Availability::Unavailable(unready) = &entries[account(&entries, &name(alias))] else {
            panic!("unavailable");
        };
        assert_eq!(unready.cause, Cause::ProviderOff);
        assert_eq!(unready.auth, Some(AuthKind::ApiKey));
        assert_eq!(unready.official, Some(true));
    }
    let mut again = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    again.insert(name("twin"), Availability::SameAs(name("g1")));
    without_providers(&mut again, &BTreeMap::new());
    assert_eq!(again, before);
}

#[test]
fn an_off_account_keeps_every_redaction_and_an_existing_failure_keeps_its_reason() {
    let config = config(
        r#"[endpoints.g1]
provider = "gemini"
auth = "api_key"
key_env = "GEMINI_KEY"
url = "https://user:CANARY_PASSWORD@gateway.example.com/CANARY_PATH-long-account-path?CANARY_QUERY"
[endpoints.missing]
provider = "gemini"
auth = "api_key"
key_env = "UNSET_KEY"
"#,
    );
    let env = env_of(&[("GEMINI_KEY", "CANARY_GEMINI_KEY")]);
    let mut entries = EndpointBook::entries(Some(&config), &NoSecrets, &absent(&env, None, false));
    let scratch = crate::test_guard::ScratchDir::new("wisp-core-", "provider-off-redactions");
    let folder = scratch.root.join("profiles");
    crate::secrets::create_folder(&folder).unwrap();
    let profile = folder.join("synthetic-profile.json");
    let tokens = serde_json::json!({"version": 1, "kind": "antigravity", "access_token": "CANARY_SUBSCRIPTION_TOKEN", "refresh_token": "CANARY_REFRESH_TOKEN", "project_id": "CANARY_PROJECT_ID", "expires_at": 0});
    crate::secrets::write_atomic(&profile, tokens.to_string().as_bytes(), 0o600).unwrap();
    let mut subscription = ready(&entries, "g1");
    subscription.credential = Credential::Subscription {
        kind: SubscriptionKind::Antigravity,
        secret: "CANARY_SUBSCRIPTION_TOKEN".into(),
        profile: Some(profile),
    };
    entries.insert(name("g1"), Availability::Ready(subscription));
    let expected = redaction_items(
        &ready(&entries, "g1").credential,
        &ready(&entries, "g1").base_url,
    );
    let missing = entries[&name("missing")].clone();
    without_providers(&mut entries, &BTreeMap::from([("gemini".to_owned(), true)]));
    assert_eq!(entries[&name("missing")], missing);
    let Availability::Unavailable(unready) = &entries[&name("g1")] else {
        panic!("unavailable");
    };
    assert_eq!(unready.secrets(), &expected);
    assert_eq!(
        expected.scrub("CANARY_PROJECT_ID"),
        crate::session::REDACTED
    );
    assert_eq!(
        expected.scrub("CANARY_SUBSCRIPTION_TOKEN"),
        crate::session::REDACTED
    );
    assert_eq!(held_secrets(&entries), expected);
    assert_eq!(
        expected.scrub("/CANARY_PATH-long-account-path"),
        crate::session::REDACTED
    );
    assert_eq!(
        expected.mask("/CANARY_PATH-long-account-path"),
        "/CANARY_PATH-long-account-path"
    );
    assert_eq!(reason(&entries, "g1"), "gemini is removed");
    assert!(!format!("{entries:?}").contains("CANARY"));
}

#[test]
fn picking_among_accounts_skips_an_off_provider_and_keeps_a_defaults_reason() {
    let mut config = config(
        r#"[endpoints.g1]
provider = "gemini"
auth = "api_key"
key_env = "GEMINI_KEY"
[endpoints.other]
auth = "api_key"
key_env = "OTHER_KEY"
"#,
    );
    let env = env_of(&[
        ("GEMINI_KEY", "CANARY_GEMINI"),
        ("OTHER_KEY", "CANARY_OTHER"),
    ]);
    let keys = absent(&env, None, false);
    let mut entries = EndpointBook::entries(Some(&config), &NoSecrets, &keys);
    without_providers(
        &mut entries,
        &BTreeMap::from([("gemini".to_owned(), false)]),
    );
    let book = EndpointBook::pick_among(Some(&config), entries.clone(), &keys, None).unwrap();
    assert_eq!(book.start.as_str(), "other");
    assert_eq!(book.start_reason, StartReason::OnlyOne);
    config.default = Some(name("g1"));
    let error = EndpointBook::pick_among(Some(&config), entries, &keys, None).unwrap_err();
    assert!(
        matches!(&error, EndpointError::DefaultUnavailable { reason, .. } if reason == "gemini is off")
    );
}

#[test]
fn an_off_implicit_provider_stops_a_start_without_a_config_file() {
    let env = env_of(&[("ANTHROPIC_API_KEY", "sk-ant-CANARY_IMPLICIT")]);
    let keys = absent(&env, None, false);
    let book = EndpointBook::resolve(None, &NoSecrets, &keys, None).unwrap();
    let off = BTreeMap::from([(ProviderKind::IMPLICIT.as_str().to_owned(), false)]);
    let error = book.without_providers(&off).unwrap_err();
    assert!(
        matches!(&error, EndpointError::EndpointUnavailable { reason, .. } if reason == &crate::plugin::registry::off_words(ProviderKind::IMPLICIT.as_str(), false))
    );
}

#[test]
fn removed_providers_win_and_the_reason_and_fix_have_one_source() {
    use crate::plugin::registry::{off_hint, off_stop, off_words, provider_fix, providers_off};
    let switches = crate::plugin::PluginSwitches {
        off: ["gemini".to_owned(), "todos".to_owned()].into(),
        removed: ["gemini".to_owned()].into(),
    };
    assert_eq!(
        providers_off(&switches),
        BTreeMap::from([("gemini".to_owned(), true)])
    );
    for (removed, reason, fix, stop) in [
        (
            false,
            "gemini is off",
            "wisp plugin enable gemini",
            "turn it on",
        ),
        (
            true,
            "gemini is removed",
            "wisp plugin add gemini",
            "add it back",
        ),
    ] {
        assert_eq!(off_words("gemini", removed), reason);
        assert_eq!(provider_fix("gemini", removed), fix);
        assert_eq!(
            off_stop("gemini", removed),
            format!("{reason}; {stop} with {fix}")
        );
        assert_eq!(
            off_hint("gemini", removed),
            format!("{reason}; {stop} in /config")
        );
    }
}

#[test]
fn unavailable_words_match_the_error_for_named_and_keys_file_accounts() {
    for name in [name("g1"), EndpointName::env()] {
        let error = EndpointError::EndpointUnavailable {
            name: name.clone(),
            reason: "gemini is off".into(),
        };
        assert_eq!(unavailable_words(&name, "gemini is off"), error.to_string());
    }
}
