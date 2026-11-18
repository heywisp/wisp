use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;
use crate::test_guard::config;

fn file(vars: &[(&str, &str)]) -> EnvFile {
    EnvFile {
        path: PathBuf::from("/home/u/.config/wisp/.env"),
        vars: vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

fn lookup(vars: &[(&str, &str)]) -> BTreeMap<String, String> {
    vars.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

const GATEWAY: &str = "[endpoints.gateway]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"DIRECT_KEY\"\nurl = \"https://direct.example.com\"\n";

fn none(_: &VariableName) -> bool {
    false
}

#[test]
fn a_file_source_finds_tables_and_provider_variables_without_values_in_debug() {
    let env_file = file(&[
        ("GW_KEY", "CANARYGWKEY0001"),
        ("GW_URL", "https://gw.example.com/CANARYPATH"),
        ("DIRECT_KEY", "sk-ant-CANARYDIRECT"),
        ("ANTHROPIC_API_KEY", "sk-ant-CANARYPROVIDER"),
    ]);
    let source = Source {
        from: Place::File(&env_file),
        words: "~/.config/wisp/.env".to_owned(),
    };
    let findings = find(Some(&config(GATEWAY)), &source, None);
    assert!(findings.skipped.is_empty(), "{findings:?}");
    let origins: Vec<String> = findings
        .found
        .iter()
        .map(|f| match &f.origin {
            Origin::Provider => "provider".to_owned(),
            Origin::Table(name) => name.to_string(),
        })
        .collect();
    assert_eq!(origins, ["direct", "gateway", "provider"]);
    let gateway = &findings.found[1];
    assert_eq!(gateway.names(), "GW_KEY and GW_URL");
    assert_eq!(gateway.key.expose(), "CANARYGWKEY0001");
    assert_eq!(
        gateway.url.as_ref().map(Secret::expose),
        Some("https://gw.example.com/CANARYPATH")
    );
    let direct = &findings.found[0];
    assert_eq!(direct.url, None);
    assert_eq!(
        direct.config_url.as_deref(),
        Some("https://direct.example.com")
    );
    let provider = &findings.found[2];
    assert_eq!(provider.names(), "ANTHROPIC_API_KEY");
    assert_eq!(provider.auth, AuthKind::ApiKey);
    let shown = format!("{findings:?}");
    assert!(!shown.contains("CANARY"), "{shown}");
}

#[test]
fn only_the_chosen_source_is_read() {
    let env_file = file(&[
        ("GW_KEY", "file-key"),
        ("GW_URL", "https://file.example.com"),
    ]);
    let real = lookup(&[
        ("GW_KEY", "env-key"),
        ("GW_URL", "https://env.example.com"),
        ("ANTHROPIC_API_KEY", "env-provider"),
    ]);
    let env = |name: &str| real.get(name).cloned();
    let from_file = find(
        Some(&config(GATEWAY)),
        &Source {
            from: Place::File(&env_file),
            words: "the file".to_owned(),
        },
        None,
    );
    assert_eq!(from_file.found.len(), 1);
    assert_eq!(from_file.found[0].key.expose(), "file-key");
    let from_env = find(
        Some(&config(GATEWAY)),
        &Source {
            from: Place::Environment(&env),
            words: "the environment".to_owned(),
        },
        None,
    );
    let keys: Vec<&str> = from_env.found.iter().map(|f| f.key.expose()).collect();
    assert_eq!(keys, ["env-key", "env-provider"]);
    assert_eq!(
        read_var(
            Place::Environment(&env),
            &VariableName::from_static("GW_URL")
        )
        .as_deref(),
        Some("https://env.example.com")
    );
    assert_eq!(
        read_var(
            Place::File(&env_file),
            &VariableName::from_static("MISSING")
        ),
        None
    );
}

#[test]
fn a_gateway_table_whose_url_is_not_in_the_chosen_source_is_skipped_with_todays_reason() {
    let only_key = file(&[("GW_KEY", "k")]);
    let real = lookup(&[("GW_KEY", "k"), ("GW_URL", "https://gw.example.com")]);
    let has = |v: &VariableName| real.contains_key(v.raw());
    let other = Other {
        has: &has,
        words: "the environment".to_owned(),
        hint: "--from-environment".to_owned(),
    };
    let text =
        "[endpoints.gateway]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n";
    let source = Source {
        from: Place::File(&only_key),
        words: "~/.config/wisp/.env".to_owned(),
    };
    let findings = find(Some(&config(text)), &source, Some(&other));
    assert!(findings.found.is_empty());
    assert_eq!(
        findings.skipped,
        [Skipped {
            what: "gateway".to_owned(),
            reason: "GW_URL is not set in ~/.config/wisp/.env, where GW_KEY was found; try --from-environment".to_owned(),
        }]
    );
    let bad = file(&[("GW_KEY", "k"), ("GW_URL", "ftp://gw.example.com")]);
    let findings = find(
        Some(&config(text)),
        &Source {
            from: Place::File(&bad),
            words: "the file".to_owned(),
        },
        None,
    );
    assert_eq!(
        findings.skipped[0].reason,
        "GW_URL is not an http or https URL"
    );
    let unset = file(&[]);
    let findings = find(
        Some(&config(text)),
        &Source {
            from: Place::File(&unset),
            words: "the file".to_owned(),
        },
        None,
    );
    assert_eq!(findings.skipped[0].reason, "GW_KEY is not set in the file");
}

#[test]
fn a_bearer_token_whose_url_variable_is_only_in_the_other_source_is_warned_about() {
    let env_file = file(&[("ANTHROPIC_AUTH_TOKEN", "tok")]);
    let real = lookup(&[("ANTHROPIC_BASE_URL", "https://gw.example.com")]);
    let has = |v: &VariableName| real.contains_key(v.raw());
    let other = Other {
        has: &has,
        words: "the environment".to_owned(),
        hint: "--from-environment".to_owned(),
    };
    let source = Source {
        from: Place::File(&env_file),
        words: "~/.config/wisp/.env".to_owned(),
    };
    let findings = find(None, &source, Some(&other));
    assert_eq!(findings.found.len(), 1);
    assert_eq!(findings.found[0].auth, AuthKind::Bearer);
    assert_eq!(
        findings.found[0].warning.as_deref(),
        Some(
            "ANTHROPIC_BASE_URL is set in the environment, not in ~/.config/wisp/.env; this token will be checked against the provider's own URL"
        )
    );
    let quiet = find(
        None,
        &source,
        Some(&Other {
            has: &none,
            ..other
        }),
    );
    assert_eq!(quiet.found[0].warning, None);
    let key_file = file(&[("ANTHROPIC_API_KEY", "k")]);
    let api = find(
        None,
        &Source {
            from: Place::File(&key_file),
            words: "f".to_owned(),
        },
        Some(&Other {
            has: &has,
            words: "the environment".to_owned(),
            hint: String::new(),
        }),
    );
    assert_eq!(api.found[0].warning, None, "only a bearer token is guarded");
}

#[test]
fn a_provider_endpoint_equal_to_a_table_is_listed_once() {
    let env_file = file(&[
        ("GW_KEY", "same"),
        ("GW_URL", "https://gw.example.com"),
        ("ANTHROPIC_AUTH_TOKEN", "same"),
        ("ANTHROPIC_BASE_URL", "https://gw.example.com/"),
    ]);
    let source = Source {
        from: Place::File(&env_file),
        words: "f".to_owned(),
    };
    let text =
        "[endpoints.gateway]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n";
    let findings = find(Some(&config(text)), &source, None);
    assert_eq!(findings.found.len(), 1);
    assert!(matches!(findings.found[0].origin, Origin::Table(_)));
}

#[test]
fn conflicting_provider_variables_are_skipped_and_a_bad_url_too() {
    let both = file(&[("ANTHROPIC_API_KEY", "a"), ("ANTHROPIC_AUTH_TOKEN", "b")]);
    let findings = find(
        None,
        &Source {
            from: Place::File(&both),
            words: "f".to_owned(),
        },
        None,
    );
    assert!(findings.found.is_empty());
    assert_eq!(findings.skipped[0].what, "anthropic");
    assert!(findings.skipped[0].reason.contains("are both set"));
    let bad = file(&[
        ("ANTHROPIC_API_KEY", "a"),
        ("ANTHROPIC_BASE_URL", "not a url"),
    ]);
    let findings = find(
        None,
        &Source {
            from: Place::File(&bad),
            words: "f".to_owned(),
        },
        None,
    );
    assert_eq!(
        findings.skipped[0].reason,
        "ANTHROPIC_BASE_URL is not an http or https URL"
    );
}

#[test]
fn a_key_shaped_variable_name_is_never_shown() {
    let canary = "canary0000000000000key1";
    let text = format!("[endpoints.gw]\nauth = \"api_key\"\nkey_env = \"{canary}\"\n");
    let env_file = file(&[]);
    let findings = find(
        Some(&config(&text)),
        &Source {
            from: Place::File(&env_file),
            words: "f".to_owned(),
        },
        None,
    );
    let shown = format!("{findings:?} {}", findings.skipped[0].reason);
    assert!(!shown.contains(canary), "{shown}");
    assert!(
        shown.contains("(looks like a key, not shown) is not set in f"),
        "{shown}"
    );
}

#[test]
fn gemini_api_key_variable_imports_with_gemini_api_default_alias() {
    let env_file = file(&[("GEMINI_API_KEY", "AIzaCANARY_gemini_key_with_underscre_1")]);
    let findings = find(
        None,
        &Source {
            from: Place::File(&env_file),
            words: "~/.config/wisp/.env".to_owned(),
        },
        None,
    );
    assert!(findings.skipped.is_empty(), "{findings:?}");
    assert_eq!(findings.found.len(), 1);
    let found = &findings.found[0];
    assert_eq!(found.provider, ProviderKind::Gemini);
    assert_eq!(found.auth, AuthKind::ApiKey);
    assert_eq!(found.default_alias(), wisp_protocol::GEMINI_API);
    assert_eq!(found.names(), "GEMINI_API_KEY");
    assert_eq!(found.url, None);
    assert_eq!(default_alias(ProviderKind::Anthropic), "anthropic");
    assert_eq!(default_alias(ProviderKind::OpenAi), "openai");
}

#[test]
fn gemini_api_key_with_custom_base_url_imports_both_variables() {
    let env_file = file(&[
        ("GEMINI_API_KEY", "AIzaCANARY_gemini_key_with_underscre_1"),
        ("GEMINI_BASE_URL", "https://gemini-gw.example.com/proxy"),
    ]);
    let findings = find(
        None,
        &Source {
            from: Place::File(&env_file),
            words: "~/.config/wisp/.env".to_owned(),
        },
        None,
    );
    assert!(findings.skipped.is_empty(), "{findings:?}");
    assert_eq!(findings.found.len(), 1);
    let found = &findings.found[0];
    assert_eq!(found.names(), "GEMINI_API_KEY and GEMINI_BASE_URL");
    assert_eq!(
        found.url.as_ref().map(Secret::expose),
        Some("https://gemini-gw.example.com/proxy")
    );
}
