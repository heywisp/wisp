use wisp_plugin_api::manifest::{self, Manifest, Origin};
use wisp_protocol::settings::{self, Form, PLUGIN_ID_RULE, Percent, find};

use super::*;
use crate::endpoints::ConfigFile;
use crate::test_guard::parse;

const CANARY: &str = "sk-ant-CANARY0123456789abcdef";

fn global(file: &ConfigFile, name: &str) -> Option<SettingValue> {
    file.settings.global(name).cloned()
}

#[test]
fn every_registered_global_key_parses() {
    let file = parse(
        "model = \"claude-sonnet-5-5\"\neffort = \"high\"\ncache_ttl = \"5m\"\ntime = \"utc\"\ndebug_cache = true\nrefusal_fallback = false\nlog = false\ntools = [\"bash\"]\npreload = [\"none\"]\ndefault = \"env\"\n\n[retry]\nbudget_usd = 0.5\n\n[instructions]\nsource = \"none\"\nwarn_tokens = \"8k\"\n\n[display]\ntheme = \"light\"\ncolor = \"256\"\nmotion = \"reduced\"\nambiguous = \"wide\"\nmouse = false\nscroll_speed = 3\nfull_repaint = true\ncopy_on_select = \"off\"\nverbose = true\n",
    )
    .unwrap();
    for (name, value) in [
        ("model", SettingValue::Text("claude-sonnet-5-5".into())),
        ("effort", SettingValue::Text("high".into())),
        ("cache_ttl", SettingValue::Text("5m".into())),
        ("time", SettingValue::Text("utc".into())),
        ("debug_cache", SettingValue::Bool(true)),
        ("refusal_fallback", SettingValue::Bool(false)),
        ("log", SettingValue::Bool(false)),
        ("tools", SettingValue::List(vec!["bash".into()])),
        ("preload", SettingValue::List(vec!["none".into()])),
        ("endpoint", SettingValue::Text("env".into())),
        ("retry.budget_usd", SettingValue::Usd { micros: 500_000 }),
        ("instructions.source", SettingValue::Text("none".into())),
        ("instructions.warn_tokens", SettingValue::Tokens(8_000)),
        ("display.theme", SettingValue::Text("light".into())),
        ("display.color", SettingValue::Text("256".into())),
        ("display.motion", SettingValue::Text("reduced".into())),
        ("display.ambiguous", SettingValue::Text("wide".into())),
        ("display.mouse", SettingValue::Bool(false)),
        ("display.scroll_speed", SettingValue::Int(3)),
        ("display.full_repaint", SettingValue::Bool(true)),
        ("display.copy_on_select", SettingValue::Text("off".into())),
        ("display.verbose", SettingValue::Bool(true)),
    ] {
        assert_eq!(global(&file, name), Some(value), "{name}");
    }
    let dotted = parse("display.theme = \"dark\"\nretry.budget_usd = \"none\"\n").unwrap();
    assert_eq!(
        global(&dotted, "display.theme"),
        Some(SettingValue::Text("dark".into()))
    );
    assert_eq!(
        global(&dotted, "retry.budget_usd"),
        Some(SettingValue::Text("none".into()))
    );
    assert_eq!(
        global(&parse("tools = []\n").unwrap(), "tools"),
        Some(SettingValue::List(vec![]))
    );
}

#[test]
fn model_tables_are_settings_with_their_lines() {
    let file = parse(
        "[models.\"claude-opus-5-5\"]\ncompact_at = \"60%\"\ncontext_window = 500000\naliases = [\"opus\"]\n\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\npricing = \"none\"\n\n[endpoints.gw.models.\"claude-opus-5-5\"]\ncompact_at = 400000\nnative_compaction = true\n",
    )
    .unwrap();
    let get = |path: &str| {
        let target = wisp_protocol::settings::parse_target(path).unwrap();
        let wisp_protocol::settings::Target::Path(path) = target else {
            panic!("{path}");
        };
        file.settings.get(&path).cloned()
    };
    let saved = get("models.\"claude-opus-5-5\".compact_at").unwrap();
    assert_eq!(
        saved.value,
        SettingValue::Percent(Percent::new(60).unwrap())
    );
    assert_eq!(saved.line, 2);
    assert_eq!(
        get("models.\"claude-opus-5-5\".aliases").unwrap().value,
        SettingValue::List(vec!["opus".into()])
    );
    assert_eq!(
        get("endpoints.gw.models.\"claude-opus-5-5\".compact_at")
            .unwrap()
            .value,
        SettingValue::Tokens(400_000)
    );
    assert_eq!(
        get("endpoints.gw.pricing").unwrap().value,
        SettingValue::Text("none".into())
    );
    assert_eq!(file.models.len(), 1);
    assert_eq!(file.endpoint_models.len(), 1);
    let at = |endpoint: &str| {
        let (endpoint, model) = (endpoint.to_owned(), "claude-opus-5-5".to_owned());
        let place = Place::EndpointModel { endpoint, model };
        SitePath::new(place, find("compact_at").unwrap())
    };
    let (shown, table) = (at(wisp_protocol::CLAUDE_API), at(crate::endpoints::ENV));
    let mut saved = file.settings.clone();
    let set = SettingValue::Tokens(300_000);
    saved.edit(&SiteEdit::Set(shown.clone(), set.clone()), None);
    assert_eq!(saved.get(&table).map(|s| &s.value), Some(&set));
    saved.edit(&SiteEdit::Unset(shown), None);
    assert!(saved.get(&table).is_none());
    assert_eq!(
        parse("[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n[endpoints.gw.models.\"m\"]\naliases = [\"x\"]\n")
            .unwrap_err(),
        "/h/config.toml line 5: aliases goes under [models.\"ID\"], not under an account's models"
    );
    assert_eq!(
        parse("[models.\"m\"]\naliases = [\"two words\"]\n").unwrap_err(),
        "/h/config.toml line 2: aliases must be a list of model IDs"
    );
    assert_eq!(
        parse("[models.\"m\"]\ncompact_at = \"101%\"\n").unwrap_err(),
        "/h/config.toml line 2: compact_at must be a number of tokens (400000) or a share of the window (\"85%\")"
    );
    assert_eq!(
        get_compact("[models.\"m\"]\ncompact_at = \"400k\"\n"),
        Some(crate::context::CompactAt::Tokens(400_000))
    );
}

fn get_compact(text: &str) -> Option<crate::context::CompactAt> {
    parse(text).unwrap().models[0].compact_at
}

#[test]
fn bad_values_name_the_setting_and_never_the_value() {
    for (text, message) in [
        (
            format!("effort = \"{CANARY}\"\n"),
            "/h/config.toml line 1: effort must be \"low\", \"medium\", \"high\", \"xhigh\" or \"max\"",
        ),
        (
            "effort = 3\n".to_owned(),
            "/h/config.toml line 1: effort must be a string",
        ),
        (
            "debug_cache = \"yes\"\n".to_owned(),
            "/h/config.toml line 1: debug_cache must be true or false",
        ),
        (
            "[display]\nscroll_speed = 21\n".to_owned(),
            "/h/config.toml line 2: display.scroll_speed must be a whole number from 1 to 20",
        ),
        (
            format!("[retry]\nbudget_usd = \"{CANARY}\"\n"),
            "/h/config.toml line 2: retry.budget_usd must be a number of US dollars, such as 0.5, or \"none\"",
        ),
        (
            format!("tools = [\"{CANARY}\"]\n"),
            "/h/config.toml line 1: tools must be a list of tool names, or [] for none",
        ),
        (
            "display = 1\n".to_owned(),
            "/h/config.toml line 1: display must be a table",
        ),
    ] {
        assert_eq!(parse(&text).unwrap_err(), message, "{text}");
    }
}

#[test]
fn unknown_keys_are_quoted_only_when_safe_with_a_suggestion() {
    assert_eq!(
        parse("effrot = \"high\"\n").unwrap_err(),
        "/h/config.toml line 1: unknown field \"effrot\"; did you mean effort?"
    );
    assert_eq!(
        parse("[display]\ntheem = \"dark\"\n").unwrap_err(),
        "/h/config.toml line 2: unknown field \"theem\" in [display]; did you mean theme?"
    );
    assert_eq!(
        parse("[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\npricng = \"none\"\n")
            .unwrap_err(),
        "/h/config.toml line 4: unknown field \"pricng\" in [endpoints.gw]; did you mean pricing?"
    );
    for key in [
        "abcdefghij0123456789xyzw",
        "my_token",
        "pass_word_secret",
        "effrot-x",
    ] {
        let error = parse(&format!("{key} = 1\n")).unwrap_err();
        assert!(!error.contains(key), "{error}");
        assert!(error.contains("unknown field; allowed: "), "{error}");
    }
    let error = parse("[display]\nabcdefghij0123456789xyzw = 1\n").unwrap_err();
    assert!(!error.contains("abcdefghij"), "{error}");
    assert_eq!(
        parse("[agents.reviewer]\ntime = \"off\"\n").unwrap_err(),
        "/h/config.toml line 1: [agents.reviewer] is planned with agent types; a preset goes in [agents.presets.reviewer]"
    );
}

#[test]
fn the_allowed_lists_come_from_the_registry() {
    assert_eq!(
        top_fields(),
        "agents, cache_ttl, claude_usage, compact_at, context, debug_cache, default, display, effort, endpoints, instructions, keep_warm, keychain, log, mcp, message_threads, model, models, plugins, preload, preset, refusal_fallback, retry, self_check, store_id, time, tool_output, tools, turned_off, voice"
    );
    assert_eq!(
        endpoint_fields(),
        "auth, background_compaction, background_tokens, key_env, key_store, mark, max_in_flight, models, native_web_search, pricing, provider, url, url_env"
    );
    assert_eq!(group_fields("instructions"), "extra, source, warn_tokens");
    assert_eq!(
        group_fields("context"),
        "auto_compact, background_compaction, background_tokens, compact_effort, context_note, count_tokens, strategy, verbatim_user_tokens"
    );
    for key in top_keys() {
        assert!(
            TABLES.contains(&key)
                || key == "store_id"
                || global_spec(key).is_some()
                || is_group(key),
            "{key}"
        );
    }
}

#[test]
fn a_variable_name_that_looks_like_a_key_never_shows_in_debug() {
    let canary = "canary0000000000000key1";
    let file = parse(&format!(
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"{canary}\"\n"
    ))
    .unwrap();
    let shown = format!("{file:?}");
    assert!(!shown.contains(canary), "{shown}");
    let spec = find("key_env").unwrap();
    let (_, saved) = file.settings.of("key_env").next().unwrap();
    assert_eq!(saved.shown(spec), wisp_protocol::NOT_SHOWN);
}

#[test]
fn a_model_row_value_is_checked_once_and_never_dropped() {
    let error = parse("[models.\"m\"]\ncontext_window = 1000\n").unwrap_err();
    assert!(
        error.starts_with("/h/config.toml line 1: context_window 1000 for m"),
        "{error}"
    );
    let error = parse(
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n[endpoints.gw.models.\"m\"]\ncontext_window = 50000\n",
    )
    .unwrap_err();
    assert!(error.contains("context_window 50000 for m"), "{error}");
    let file = parse("[models.\"m\"]\ncontext_window = 200000\n").unwrap();
    assert_eq!(file.models[0].context_window, Some(200_000));
    assert_eq!(file.settings.len(), 1);
}

#[test]
fn a_saved_tool_name_is_read_for_its_spelling_only() {
    let file = parse("tools = [\"telepathy\", \"bash\"]\n").unwrap();
    let path = wisp_protocol::settings::SitePath::global(find("tools").unwrap());
    assert_eq!(
        file.settings.get(&path).map(|s| s.value.clone()),
        Some(wisp_protocol::settings::SettingValue::List(vec![
            "bash".to_owned(),
            "telepathy".to_owned()
        ]))
    );
    assert!(parse("tools = [\"two words\"]\n").is_err());
}

#[test]
fn every_setting_describes_its_kind_for_config_toml() {
    let rows: Vec<String> = SETTINGS
        .iter()
        .map(|s| format!("{}: {}", s.name, s.describe(Form::File)))
        .collect();
    insta::assert_snapshot!(rows.join("\n"));
}

#[test]
fn the_plugin_id_rule_is_the_manifest_rule() {
    let text = "id = \"1x\"\nversion = \"0.1.0\"\nworld = \"0.1.0\"\nsummary = \"Acme\"\n";
    let Err(problem) = Manifest::parse(text, Origin::External) else {
        panic!("a bad id parsed");
    };
    assert_eq!(problem.message, format!("id `1x` must be {PLUGIN_ID_RULE}"));
}

#[test]
fn the_kebab_checks_are_the_manifest_checks() {
    let alphabet = ['a', 'z', '0', '9', '-', 'A', '_', '.', '/', ' ', 'é'];
    let mut texts = vec![String::new()];
    for len in 0..3 {
        let longer: Vec<String> = texts
            .iter()
            .filter(|t| t.chars().count() == len)
            .flat_map(|t| alphabet.iter().map(move |c| format!("{t}{c}")))
            .collect();
        texts.extend(longer);
    }
    texts.extend([1, 2, 31, 32, 33].map(|n| "a".repeat(n)));
    for text in &texts {
        assert_eq!(settings::plugin_id(text), manifest::is_id(text), "{text:?}");
        assert_eq!(
            settings::hook_name(text),
            manifest::is_name(text),
            "{text:?}"
        );
        assert_eq!(
            settings::section_name(text),
            manifest::is_id(text),
            "{text:?}"
        );
        for name in &texts {
            let text = format!("{text}/{name}");
            assert_eq!(
                settings::section_ref(&text),
                manifest::is_section(&text),
                "{text:?}"
            );
        }
    }
}

#[test]
fn mcp_servers_parse_commands_and_argument_vectors_without_becoming_settings() {
    let file = parse("model = \"claude-opus-5-5\"\n[mcp.servers.local]\ncommand = \"server\"\nargs = [\"--stdio\", \"an argument\"]\n[mcp.servers.\"user.choice\"]\ncommand = \"other\"\n").unwrap();
    assert_eq!(
        file.mcp_servers["local"],
        McpServer {
            keys: Vec::new(),
            command: "server".into(),
            args: vec!["--stdio".into(), "an argument".into()],
        }
    );
    assert!(file.mcp_servers["user.choice"].args.is_empty());
    assert_eq!(file.settings.len(), 1);
    assert!(parse("[mcp]\n").unwrap().mcp_servers.is_empty());
    assert!(top_keys().contains(&"mcp"));
    let file = parse(
        "[mcp.servers.sentry]\ncommand = \"server\"\nkeys = [\"SENTRY_TOKEN\", \"SENTRY_KEY\"]\n",
    )
    .unwrap();
    assert_eq!(
        file.mcp_servers["sentry"].keys,
        ["SENTRY_TOKEN", "SENTRY_KEY"]
    );
}

#[test]
fn invalid_mcp_tables_fail_without_repeating_their_values() {
    for input in [
        "[mcp.servers.local]\ncommand = \"server\"\nkeys = [1]",
        "[mcp.servers.local]\ncommand = \"server\"\nkeys = [\"1BAD\"]",
        "[mcp.servers.local]\ncommand = \"server\"\nkeys = [\"lin_api_CANARYa12345678901234567890\"]",
        "mcp = 1",
        "[mcp]\nunknown = {}",
        "[mcp]\nservers = []",
        "[mcp.servers]\nlocal = 1",
        "[mcp.servers.local]\nargs = []",
        "[mcp.servers.local]\ncommand = \"\"",
        "[mcp.servers.local]\ncommand = 1",
        "[mcp.servers.local]\ncommand = \"private-command\"\nargs = [1]",
        "[mcp.servers.local]\ncommand = \"private-command\"\nunknown = \"private-value\"",
        "[mcp.servers.\"\"]\ncommand = \"private-command\"",
    ] {
        let error = parse(input).unwrap_err().to_string();
        assert!(error.contains("mcp.servers"), "{input}: {error}");
        assert!(!error.contains("private"), "{error}");
    }
}
