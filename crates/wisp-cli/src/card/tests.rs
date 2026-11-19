use wisp_core::endpoints::ProviderKind;
use wisp_tui::app::endpoint_card::endpoint_facts_of;

use super::{catalogue, setup, test_context};
use crate::endpoints::{self, Flags, Sources};
use crate::keychain::Keychain;
use crate::resume::tests::{connection, listed};

const CONFIG: &str = "default = \"direct\"\n\
[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"CARD_DIRECT_KEY\"\n\
[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"CARD_GW_KEY\"\nurl_env = \"CARD_GW_URL\"\n\
[endpoints.missing]\nauth = \"api_key\"\nkey_env = \"CARD_NOT_SET_KEY\"\n\
[endpoints.claude]\nauth = \"anthropic_subscription\"\nkey_env = \"CARD_CLAUDE_KEY\"\n\
[endpoints.chatgpt]\nprovider = \"openai\"\nauth = \"chatgpt_subscription\"\nkey_env = \"CARD_CHATGPT_KEY\"\n\
[endpoints.pasted]\nauth = \"api_key\"\nkey_env = \"CANARYpastedkey4wisp7card\"\n";

const ENV: &str = "CARD_DIRECT_KEY=sk-ant-CANARYCARDDIRECT1\n\
CANARYpastedkey4wisp7card=sk-ant-CANARYCARDPASTED1\n\
CARD_GW_KEY=CANARYCARDGWTOKEN\n\
CARD_GW_URL=https://gw.example.com/anthropic\n\
CARD_CLAUDE_KEY=sk-ant-oat01-CANARYCARDCLAUDE1\n\
CARD_CHATGPT_KEY=CANARYCARDCHATGPT\n";

#[test]
fn the_catalogue_lists_each_provider_with_its_auths_in_order() {
    let entries = catalogue();
    assert_eq!(entries.len(), ProviderKind::ALL.len());
    for (entry, provider) in entries.iter().zip(ProviderKind::ALL) {
        assert_eq!(entry.name, provider.as_str());
        assert_eq!(entry.default_url, provider.default_url());
        assert_eq!(entry.official_auth, provider.official_auth().as_str());
        let auths: Vec<&str> = entry.auths.iter().map(|a| a.name.as_str()).collect();
        let kinds: Vec<&str> = provider
            .auths()
            .iter()
            .filter(|auth| !auth.requires_oauth())
            .map(|a| a.as_str())
            .collect();
        assert_eq!(auths, kinds);
        assert!(provider.auth(&entry.official_auth).is_some());
    }
}

#[test]
fn the_endpoints_the_tui_is_told_about_equal_what_a_start_shows() {
    let root = &crate::scratch_dir("card-endpoints").root;
    let config = root.join("config.toml");
    let env = root.join("test.env");
    std::fs::write(&config, CONFIG).unwrap();
    std::fs::write(&env, ENV).unwrap();
    let keychain = Keychain::off();
    let flags = Flags {
        env_file: Some(&env),
        config: Some(&config),
        endpoint: None,
        keychain: &keychain,
        set: &[],
    };
    let sources = Sources::load(&flags, None, Some(root.clone()));
    let book = sources.book().unwrap();
    let (_, refused) = endpoints::connections(&book.entries, None);
    let list = listed(&[
        ("gpt-6.1-sol", "list", "low"),
        ("gpt-reserve", "hide", "medium"),
    ]);
    let ready = [connection("chatgpt", Some(list.clone()))];
    let shown = endpoints::endpoint_facts(&book.entries, &book.marks, &refused, &ready);
    let summaries =
        endpoints::summaries(&book.entries, &book.marks, &refused, Some("direct"), &ready);
    let told: Vec<_> = summaries.iter().map(endpoint_facts_of).collect();
    assert_eq!(told, shown);
    let own = |name: &str| told.iter().find(|e| e.name == name).unwrap().models.clone();
    assert_eq!(own("chatgpt"), list.offers());
    assert!(own("direct").is_empty());
    let chosen = crate::tui::Chosen {
        model: "gpt-6.1-sol",
        effort: "medium",
        cache_ttl: wisp_protocol::CacheTtl::OneHour,
        endpoint: wisp_render::chrome::Endpoint::Official,
        max_tokens: 64000,
        claude_usage: false,
    };
    let facts = wisp_start::models::session_facts(&book, chosen, summaries.clone(), &ready);
    assert!(facts.models.iter().any(|m| m.id == "gpt-reserve"));
    let var = |name: &str| {
        told.iter()
            .find(|e| e.name == name)
            .and_then(|e| e.key_var.clone())
            .map(|v| (v.name, v.in_keys_file))
    };
    assert_eq!(var("direct"), Some(("CARD_DIRECT_KEY".to_owned(), true)));
    assert_eq!(
        var("pasted"),
        Some((wisp_protocol::NOT_SHOWN.to_owned(), true))
    );
    assert!(told.iter().all(|e| !e.expired));
    let record = serde_json::to_string(&summaries).unwrap();
    assert!(record.contains("CARD_DIRECT_KEY"));
    assert!(!record.contains("CANARY"), "{record}");
    assert!(
        told.iter()
            .any(|e| e.name == "missing" && e.unavailable.is_some())
    );
    assert!(
        told.iter()
            .any(|e| e.host.as_deref() == Some("gw.example.com"))
    );
    for credential in [
        wisp_tui::app::Credential::AnthropicPlan,
        wisp_tui::app::Credential::ChatGptPlan,
    ] {
        assert!(told.iter().any(|e| e.credential == Some(credential)));
    }
    assert!(!format!("{told:?}").contains("CANARY"));
}

#[test]
fn the_card_names_the_given_configs_credentials_without_a_path() {
    let root = &crate::scratch_dir("card-given-config").root;
    let config = root.join("config.toml");
    std::fs::write(&config, "").unwrap();
    let keychain = Keychain::off();
    let flags = Flags {
        env_file: None,
        config: Some(&config),
        endpoint: None,
        keychain: &keychain,
        set: &[],
    };
    let sources = Sources::load(&flags, None, Some(root.clone()));
    let ctx = test_context(root, &config, &keychain);
    let card = setup(&sources, &ctx);
    assert_eq!(
        card.credentials_shown,
        "credentials.json beside the file given with --config"
    );
    assert!(card.presence.is_empty());
    assert!(!card.credentials_shown.contains(&root.display().to_string()));
}

#[test]
fn session_facts_use_the_named_openai_provider_label() {
    let directory = crate::scratch_dir("card-openai");
    let config = directory.root.join("config.toml");
    let variables = directory.root.join("test.env");
    std::fs::write(&config,"default = \"openai\"\n[endpoints.openai]\nprovider = \"openai\"\nauth = \"api_key\"\nkey_env = \"CARD_OPENAI_KEY\"\n").unwrap();
    std::fs::write(&variables, "CARD_OPENAI_KEY=synthetic-card-key\n").unwrap();
    let keychain = Keychain::off();
    let sources = Sources::load(
        &Flags {
            env_file: Some(&variables),
            config: Some(&config),
            endpoint: Some("openai"),
            keychain: &keychain,
            set: &[],
        },
        None,
        Some(directory.root.clone()),
    );
    let book = sources.book().unwrap();
    let facts = wisp_start::models::session_facts(
        &book,
        crate::tui::Chosen {
            model: "gpt-6.1-sol",
            effort: "medium",
            cache_ttl: wisp_protocol::CacheTtl::OneHour,
            endpoint: wisp_render::chrome::Endpoint::Official,
            max_tokens: 64000,
            claude_usage: false,
        },
        endpoints::summaries(&book.entries, &book.marks, &[], None, &[]),
        &[],
    );
    assert_eq!(facts.provider, "OpenAI");
}
