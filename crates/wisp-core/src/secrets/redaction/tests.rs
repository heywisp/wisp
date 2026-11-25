use std::collections::BTreeMap;

use super::*;
use crate::endpoints::{EndpointBook, Keychain, Variables};
use crate::provider::credential::EnvFile;
use crate::secrets::NoSecrets;
use crate::session::REDACTED;
use crate::test_guard::config;

fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + Sync + use<> {
    let vars: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| vars.get(name).cloned()
}

fn file_of(text: &str) -> EnvFile {
    EnvFile {
        path: "/home/u/.config/wisp/.env".into(),
        vars: line_values(text),
    }
}

struct Case {
    config: Option<&'static str>,
    env: Vec<(&'static str, &'static str)>,
    file: &'static str,
}

fn contains(case: &Case) {
    let config = case.config.map(config);
    let env = lookup(&case.env);
    let file = file_of(case.file);
    let reader = variable_redactions(config.as_ref(), [], &env, Some(case.file));
    if case.config.is_none() {
        for (name, value) in &case.env {
            if name.ends_with("_BASE_URL") {
                let scrubbed = reader.scrub(value);
                assert!(!scrubbed.contains("CANARY"), "{name}");
                assert!(scrubbed.contains("implicit.example:8443"), "{name}");
            } else {
                assert_eq!(reader.scrub(value), REDACTED, "{name}");
            }
        }
    }
    let book = EndpointBook::resolve(
        config.as_ref(),
        &NoSecrets,
        &Keychain::Absent(Variables::new(&env, Some(&file), false)),
        None,
    )
    .unwrap();
    let context = book.log_context();
    assert_eq!(
        reader.union([&context.redactions]),
        reader,
        "{:?} {:?}",
        case.config,
        case.file
    );
}

#[test]
fn the_reader_holds_everything_the_log_context_would() {
    let cases = [
        Case {
            config: None,
            env: vec![
                ("ANTHROPIC_API_KEY", "sk-ant-CANARYA"),
                (
                    "ANTHROPIC_BASE_URL",
                    "https://u:CANARYURLPASS@implicit.example:8443/CANARYPATH?CANARYQUERY",
                ),
            ],
            file: "ANTHROPIC_API_KEY=sk-ant-CANARYB\n",
        },
        Case {
            config: Some(
                "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl = \"https://gw.example.com/CANARYTOKENPATH\"\n",
            ),
            env: vec![("GW_KEY", "CANARYGW")],
            file: "",
        },
        Case {
            config: Some(
                "default = \"ok\"\n[endpoints.ok]\nauth = \"api_key\"\nkey_env = \"OK_KEY\"\n[endpoints.bad]\nauth = \"bearer\"\nkey_env = \"BAD_KEY\"\nurl_env = \"BAD_URL\"\n",
            ),
            env: vec![
                ("OK_KEY", "sk-ant-ok"),
                ("BAD_KEY", "CANARYBAD"),
                ("BAD_URL", "ftp://u:CANARYPASS@host/CANARYPATH"),
            ],
            file: "",
        },
        Case {
            config: None,
            env: vec![],
            file: "ANTHROPIC_API_KEY=\"  sk-ant-CANARYPADDED  \"\n",
        },
        Case {
            config: Some(
                "[endpoints.work]\nauth = \"bearer\"\nkey_env = \"WORK_KEY\"\nurl_env = \"WORK_URL\"\n",
            ),
            env: vec![("ANTHROPIC_AUTH_TOKEN", "CANARYTOKEN")],
            file: "WORK_KEY=CANARYWORK\nWORK_URL=https://u:CANARYPASS@w.example.com/CANARYPATH?CANARYQUERY\n",
        },
    ];
    for case in &cases {
        contains(case);
    }
}

#[test]
fn both_sources_are_read_for_every_name() {
    let config = config(
        "[mcp.servers.local]\ncommand = \"test-server\"\nkeys = [\"SERVER_KEY\", \"SHORT_KEY\"]\n[endpoints.work]\nauth = \"bearer\"\nkey_env = \"WORK_KEY\"\nurl_env = \"WORK_URL\"\n[endpoints.odd]\nauth = \"bearer\"\nkey_env = \"ODD_KEY\"\nurl_env = \"ODD_URL\"\n",
    );
    let run_servers = crate::config::mcp_servers_in(
        "[mcp.servers.run]\ncommand = 'test-server'\nkeys = ['FLAG_MCP_AUTH']\n",
    )
    .unwrap();
    const FLAG_VALUE: &str = "CANARY-exported-flag-server-value";
    let env = lookup(&[
        ("FLAG_MCP_AUTH", FLAG_VALUE),
        ("ANTHROPIC_API_KEY", "sk-ant-CANARYA-credential"),
        (
            "WORK_URL",
            "https://u:CANARYPASS-long-password@w.example.com/CANARYPATH-long-account-path?CANARYQUERY-long-query",
        ),
        ("ODD_URL", "not a url CANARYODDURL"),
        ("SERVER_KEY", "CANARY-exported-mcp-key-credential"),
        ("SHORT_KEY", "CANARY-short-secret"),
    ]);
    let text = "ANTHROPIC_API_KEY=sk-ant-CANARYB-credential\nthis line has CANARYSKIPPED no equals\nWORK_KEY=  CANARYWORK  \nANTHROPIC_BASE_URL=https://gw.example.com/CANARYBASE\n";
    let reader = variable_redactions(Some(&config), run_servers.values(), &env, Some(text));
    let scrubbed = reader.scrub(
        "sk-ant-CANARYA-credential sk-ant-CANARYB-credential CANARYWORK CANARYPASS-long-password /CANARYPATH-long-account-path CANARYQUERY-long-query not a url CANARYODDURL /CANARYBASE",
    );
    assert!(!scrubbed.contains("CANARY"), "{scrubbed}");
    assert!(
        reader
            .scrub("this line has CANARYSKIPPED no equals")
            .contains("CANARYSKIPPED")
    );
    let text = "sk-ant-CANARYA-credential sk-ant-CANARYB-credential CANARYPASS-long-password /CANARYPATH-long-account-path CANARYQUERY-long-query not a url CANARYODDURL /CANARYBASE";
    assert_eq!(
        reader.mask(text),
        format!(
            "{REDACTED} {REDACTED} {REDACTED} /CANARYPATH-long-account-path CANARYQUERY-long-query not a url CANARYODDURL /CANARYBASE"
        )
    );
    assert_eq!(reader.mask(FLAG_VALUE), REDACTED);
    assert_eq!(reader.scrub(FLAG_VALUE), REDACTED);
    assert_eq!(reader.mask("CANARY-exported-mcp-key-credential"), REDACTED);
    assert_eq!(reader.scrub("CANARY-exported-mcp-key-credential"), REDACTED);
    assert_eq!(reader.mask("CANARY-short-secret"), "CANARY-short-secret");
    assert_eq!(reader.scrub("CANARY-short-secret"), "CANARY-short-secret");
    let shown = format!("{reader:?}");
    assert!(!shown.contains("CANARY"), "{shown}");
}

#[test]
fn exported_key_names_hold_only_long_non_path_values() {
    let fake = "CANARY-exported-unknown-credential";
    let vars = [
        ("UNKNOWN_TOKEN", fake),
        ("mixed_Key", fake),
        ("prefixSECRETpostfix", fake),
        ("service_password", fake),
        ("PASSWORD_FILE", "/CANARY-password-file-path"),
        ("PASSWORD_HOME", "~/CANARY-password-file-path"),
        ("PASSWORD_RELATIVE", "./CANARY-password-file-path"),
        ("SHORT_TOKEN", "CANARY-short-secret"),
        ("KEYTIMEOUT", "1"),
        ("LONG_ID", "CANARY-unrelated-long-identifier"),
    ];
    for (name, value) in vars {
        let redactions = named_key_values([(name.to_owned(), value.to_owned())]);
        let expected = if value == fake { REDACTED } else { value };
        assert_eq!(redactions.mask(value), expected, "{name}");
        assert_eq!(redactions.scrub(value), expected, "{name}");
    }
}

#[test]
fn nothing_set_holds_nothing() {
    let reader = variable_redactions(None, [], &|_| None, None);
    assert!(reader.is_empty());
    let blank = variable_redactions(
        None,
        [],
        &|_| Some("   ".to_owned()),
        Some("ANTHROPIC_API_KEY=\n"),
    );
    assert!(blank.is_empty());
}

#[test]
fn url_parts_redact_the_bare_host() {
    let secrets = Redactions::new(url_parts("https://user:pw@gw.example.com:8443/v1?k=1"));
    assert_eq!(
        secrets.scrub("gw.example.com and gw.example.com:8443 and user:pw@gw.example.com:8443"),
        "[redacted] and [redacted] and [redacted]"
    );
    assert_eq!(url_parts("http://[::1]:8080")[2], "::1");
}

#[test]
fn live_redactions_hide_each_provider_and_gateway_value_and_the_bare_host() {
    let names = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "GEMINI_API_KEY",
        "GEMINI_BASE_URL",
        "MANIFEST_API_KEY",
        "MANIFEST_BASE_URL",
    ];
    let canary = |name: &str| {
        Some(if name.ends_with("_BASE_URL") {
            format!("https://user:CANARYPW@canary-gw.example:8443/{name}")
        } else {
            format!("CANARY-{name}")
        })
    };
    let secrets = live_redactions(&canary);
    for name in names {
        assert_eq!(secrets.scrub(&canary(name).unwrap()), REDACTED, "{name}");
    }
    assert_eq!(
        secrets.scrub("at canary-gw.example"),
        format!("at {REDACTED}")
    );
    let provider_only = live_redactions(&|name| {
        (name == "OPENAI_BASE_URL").then(|| "http://localhost:11434/v1".to_owned())
    });
    assert_eq!(
        provider_only.scrub("at localhost"),
        format!("at {REDACTED}")
    );
}

#[test]
fn antigravity_profile_project_id_is_redacted_in_endpoint_log_context() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let config = dir.path().join("config.toml");
    let handle = "oauth:0123456789abcdef0123456789abcdef";
    let profile = crate::oauth::profile_path(&config, handle).unwrap();
    crate::oauth::prepare(&config).unwrap();
    let payload = serde_json::json!({
        "version": 1,
        "kind": "antigravity",
        "access_token": "ya29.CANARY_ACCESS_TOKEN",
        "refresh_token": "1//CANARY_REFRESH",
        "expires_at": 4_102_444_800_u64,
        "generation": 0,
        "project_id": "CANARY-gcp-project-42"
    });
    crate::secrets::write_atomic(&profile, &serde_json::to_vec(&payload).unwrap(), 0o600).unwrap();
    let credential = crate::oauth::credential(
        &config,
        handle,
        crate::provider::credential::SubscriptionKind::Antigravity,
    )
    .unwrap();
    let items = crate::endpoints::redaction_items(
        &credential,
        crate::provider::gemini::antigravity_base_url(),
    );
    let scrubbed = items.scrub("project CANARY-gcp-project-42 with ya29.CANARY_ACCESS_TOKEN");
    assert_eq!(scrubbed, format!("project {REDACTED} with {REDACTED}"));

    assert_eq!(
        items.mask("project CANARY-gcp-project-42 with ya29.CANARY_ACCESS_TOKEN"),
        format!("project CANARY-gcp-project-42 with {REDACTED}")
    );
    assert_eq!(
        crate::session::held().mask("CANARY-gcp-project-42"),
        "CANARY-gcp-project-42"
    );

    let non_ag = crate::provider::credential::Credential::Subscription {
        kind: crate::provider::credential::SubscriptionKind::Anthropic,
        secret: "sk-ant-oat01-CANARY".to_owned(),
        profile: Some(profile),
    };
    let non_ag_items =
        crate::endpoints::redaction_items(&non_ag, crate::provider::anthropic::DEFAULT_BASE_URL);
    assert_eq!(
        non_ag_items.scrub("CANARY-gcp-project-42"),
        "CANARY-gcp-project-42"
    );
}
