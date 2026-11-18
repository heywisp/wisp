use super::*;
use crate::filter::Query;

fn pct(value: u8) -> Percent {
    Percent::new(value).unwrap()
}

#[test]
fn the_smallest_window_is_the_first_whose_point_reaches_it() {
    for max_tokens in [0, 32_000, 64_000, 128_000] {
        for point in [
            1,
            40_000,
            208_000,
            400_000,
            741_200,
            184_467_440_738_095_516,
        ] {
            let window = smallest_window(point, max_tokens);
            assert!(default_point(window, max_tokens) >= point);
            assert!(default_point(window - 1, max_tokens) < point);
        }
    }
    assert_eq!(smallest_window(400_000, 64_000), 470_589);
    assert_eq!(smallest_window(400_000, 128_000), 528_000);
    assert_eq!(smallest_window(0, 64_000), 0);
    assert_eq!(smallest_window(u64::MAX, 64_000), u64::MAX);
}

#[test]
fn the_default_point_on_common_windows() {
    assert_eq!(default_point(1_000_000, 64_000), 850_000);
    assert_eq!(default_point(200_000, 64_000), 136_000);
    assert_eq!(reserve(1_000_000, 64_000), 150_000);
    assert_eq!(reserve(200_000, 64_000), 64_000);
}

#[test]
fn each_arm_of_the_reserve_wins_on_its_side() {
    assert_eq!(reserve(400_000, 32_000), 60_000);
    assert_eq!(reserve(200_000, 32_000), 40_000);
    assert_eq!(reserve(200_000, 50_000), 50_000);
    assert_eq!(reserve(266_666, 0), 40_000);
    assert_eq!(reserve(266_667, 0), 40_001);
    assert_eq!(reserve(400_000, 60_001), 60_001);
    assert_eq!(reserve(400_000, 59_999), 60_000);
}

#[test]
fn the_reserve_is_never_below_max_tokens() {
    for max_tokens in [0, 1, 40_000, 64_000, 128_000, u32::MAX] {
        for window in [0, 1, 100_000, 1_000_000, u64::MAX] {
            assert!(reserve(window, max_tokens) >= u64::from(max_tokens));
        }
    }
}

#[test]
fn the_share_of_the_reserve_rounds_up_and_the_point_down() {
    assert_eq!(reserve(333_333, 32_000), 50_000);
    assert_eq!(default_point(333_333, 32_000), 283_333);
    assert_eq!(share_point(333_333, pct(85)), 283_333);
}

#[test]
fn share_point_on_its_own() {
    assert_eq!(share_point(333_333, pct(85)), 283_333);
    assert_eq!(share_point(1_000_000, pct(100)), 1_000_000);
    assert_eq!(share_point(333_333, pct(100)), 333_333);
    assert_eq!(share_point(1_000_000, pct(0)), 0);
}

#[test]
fn both_points_are_exact_at_the_largest_window() {
    assert_eq!(
        share_point(u64::MAX, pct(85)),
        u64::MAX / 100 * 85 + u64::MAX % 100 * 85 / 100
    );
    assert_eq!(share_point(u64::MAX, pct(100)), u64::MAX);
    let share = u64::MAX / 100 * 15 + (u64::MAX % 100 * 15).div_ceil(100);
    assert_eq!(default_point(u64::MAX, 64_000), u64::MAX - share);
}

#[test]
fn a_window_below_the_reserve_gives_zero() {
    assert_eq!(default_point(30_000, 64_000), 0);
    assert_eq!(default_point(0, 0), 0);
}

#[test]
fn percent_refuses_more_than_a_hundred() {
    assert_eq!(Percent::new(101), Err(PercentTooLarge(101)));
    assert_eq!(Percent::new(255), Err(PercentTooLarge(255)));
    assert_eq!(pct(0).get(), 0);
    assert_eq!(pct(100).get(), 100);
    assert_eq!(pct(85).to_string(), "85%");
}

#[test]
fn percent_serialises_as_a_number_and_refuses_a_large_one() {
    assert_eq!(serde_json::to_string(&pct(85)).unwrap(), "85");
    assert_eq!(serde_json::from_str::<Percent>("100").unwrap(), pct(100));
    assert!(serde_json::from_str::<Percent>("101").is_err());
}

#[test]
fn effort_refusal_omits_empty_supported_list_for_fixed_tier_models() {
    assert_eq!(
        effort_refusal("gemini-4-carbon-medium", crate::Effort::High, true, &[]),
        "gemini-4-carbon-medium doesn't take effort high"
    );
    assert_eq!(
        effort_refusal(
            "gemini-4-carbon",
            crate::Effort::Max,
            true,
            &[
                crate::Effort::Low,
                crate::Effort::Medium,
                crate::Effort::High
            ],
        ),
        "gemini-4-carbon doesn't take effort max; it takes low, medium, high"
    );
}

#[test]
fn effort_selection_requires_an_offered_level() {
    use crate::Effort::{High, Low, Xhigh};

    assert!(takes_no_effort(Some(&[])));
    assert!(!takes_no_effort(None));
    assert!(!takes_no_effort(Some(&[Low])));

    for (levels, current, expected) in [
        (&[][..], Some(Xhigh), None),
        (&[][..], None, None),
        (&[Low, High][..], Some(High), Some(High)),
        (&[Low, High][..], Some(Xhigh), Some(High)),
        (&[Low, High][..], None, Some(Low)),
    ] {
        assert_eq!(effort_on(levels, current), expected);
    }
}

#[test]
fn compaction_cost_round_trips() {
    let cost = CompactionCost {
        estimate_micros: 290_000,
        head_write_micros: 120_000,
    };
    let text = serde_json::to_string(&cost).unwrap();
    assert_eq!(
        text,
        r#"{"estimate_micros":290000,"head_write_micros":120000}"#
    );
    assert_eq!(serde_json::from_str::<CompactionCost>(&text).unwrap(), cost);
}

mod registry {
    use super::super::*;

    const CANARY: &str = "sk-ant-CANARY0123456789abcdef";

    #[test]
    fn names_are_sorted_and_unique_and_flags_and_variables_are_unique() {
        let names: Vec<&str> = SETTINGS.iter().map(|s| s.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted);
        let mut flags: Vec<&str> = SETTINGS
            .iter()
            .flat_map(|s| s.flags.iter().copied())
            .collect();
        let count = flags.len();
        flags.sort_unstable();
        flags.dedup();
        assert_eq!(flags.len(), count);
        let mut env: Vec<&str> = SETTINGS
            .iter()
            .flat_map(|s| s.env.iter().copied())
            .collect();
        let count = env.len();
        env.sort_unstable();
        env.dedup();
        assert_eq!(env.len(), count);
        let mut paths: Vec<String> = SETTINGS
            .iter()
            .filter(|s| s.has_site(Site::Global))
            .map(|s| s.file_key().to_owned())
            .collect();
        let count = paths.len();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), count);
    }

    #[test]
    fn only_facts_are_shipped() {
        let shipped: Vec<&str> = SETTINGS
            .iter()
            .filter(|s| s.has_site(Site::Shipped))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            shipped,
            [
                "aliases",
                "context_window",
                "max_output",
                "native_compaction",
                "price_cache_read",
                "price_input",
                "price_output",
                "web_search"
            ]
        );
        assert!(!find("compact_at").unwrap().has_site(Site::Shipped));
    }

    #[test]
    fn secrets_and_hidden_settings_have_no_session_value_flag_or_variable() {
        for spec in SETTINGS.iter().filter(|s| s.secrecy != Secrecy::Plain) {
            assert!(!spec.mid_session, "{}", spec.name);
            assert!(spec.flags.is_empty(), "{}", spec.name);
            assert!(spec.env.is_empty(), "{}", spec.name);
        }
    }

    #[test]
    fn every_built_setting_has_a_place_or_a_named_reason() {
        let without: Vec<&str> = SETTINGS
            .iter()
            .filter(|s| !s.is_built() || s.secrecy == Secrecy::Hidden)
            .map(|s| s.name)
            .collect();
        assert_eq!(without, ["agent", "store_id"]);
        for spec in SETTINGS.iter().filter(|s| s.is_built()) {
            assert!(
                !spec.label.is_empty() && !spec.help.is_empty(),
                "{}",
                spec.name
            );
            assert!(Section::ALL.contains(&spec.section));
        }
    }

    #[test]
    fn every_built_setting_can_be_changed_without_editing_the_file() {
        for spec in SETTINGS.iter().filter(|s| s.is_built()) {
            let settable = match spec.secrecy {
                Secrecy::Plain => !spec.sites.iter().all(|s| *s == Site::Shipped),
                Secrecy::Secret | Secrecy::Hidden => true,
            };
            assert!(settable, "{}", spec.name);
        }
    }

    #[test]
    fn the_model_setting_defaults_to_the_default_model() {
        assert_eq!(find("model").unwrap().default_text(), Some(DEFAULT_MODEL));
    }

    #[test]
    fn defaults_parse_by_their_kind() {
        for spec in SETTINGS {
            if let Default::Const(text) = spec.default {
                assert!(spec.parse(text).is_ok(), "{}", spec.name);
            }
            let subscription = CredentialKind::Other(AUTH_ANTHROPIC_SUBSCRIPTION.to_owned());
            for kind in [None, Some(&CredentialKind::ApiKey), Some(&subscription)] {
                if let Some(value) = spec.default_for(kind) {
                    assert!(spec.parse(&value.shown()).is_ok(), "{}", spec.name);
                    assert!(matches!(spec.default, Default::Rule(_)), "{}", spec.name);
                }
            }
        }
    }

    #[test]
    fn values_parse_by_kind_and_errors_never_hold_the_input() {
        let ok = |name: &str, text: &str| find(name).unwrap().parse(text).unwrap();
        assert_eq!(ok("debug_cache", "yes"), SettingValue::Bool(true));
        assert_eq!(ok("debug_cache", "off"), SettingValue::Bool(false));
        assert_eq!(ok("effort", "xhigh"), SettingValue::Text("xhigh".into()));
        assert_eq!(ok("display.scroll_speed", "20"), SettingValue::Int(20));
        assert_eq!(
            ok("instructions.warn_tokens", "40k"),
            SettingValue::Tokens(40_000)
        );
        assert_eq!(ok("context_window", "1M"), SettingValue::Tokens(1_000_000));
        assert_eq!(ok("compact_at", "400k"), SettingValue::Tokens(400_000));
        assert_eq!(
            ok("compact_at", "60%"),
            SettingValue::Percent(Percent::new(60).unwrap())
        );
        assert_eq!(
            ok("retry.budget_usd", "$0.50"),
            SettingValue::Usd { micros: 500_000 }
        );
        assert_eq!(
            ok("retry.budget_usd", "none"),
            SettingValue::Text("none".into())
        );
        assert_eq!(
            ok("model", "claude-sonnet-5.5"),
            SettingValue::Text("claude-sonnet-5.5".into())
        );
        assert_eq!(ok("endpoint", "gw-2"), SettingValue::Text("gw-2".into()));
        assert_eq!(ok("key_env", "GW_KEY"), SettingValue::Text("GW_KEY".into()));
        assert_eq!(
            ok("tools", "bash, bash"),
            SettingValue::List(vec!["bash".into()])
        );
        assert_eq!(ok("tools", "none"), SettingValue::List(vec![]));
        assert_eq!(
            ok("aliases", "a-1,b-2"),
            SettingValue::List(vec!["a-1".into(), "b-2".into()])
        );
        assert_eq!(ok("voice.voice", "cove"), SettingValue::Text("cove".into()));
        assert_eq!(
            ok("voice.language", " en "),
            SettingValue::Text("en".into())
        );
        assert_eq!(
            ok("voice.instructions", " flows/VOICE.md "),
            SettingValue::Text("flows/VOICE.md".into())
        );
        for name in [
            "voice.endpoint",
            "voice.voice",
            "voice.language",
            "voice.instructions",
        ] {
            assert_eq!(find(name).unwrap().section, Section::Voice, "{name}");
        }
        let instructions = find("voice.instructions").unwrap();
        assert_eq!(
            instructions.shown(&ok("voice.instructions", CANARY)),
            crate::NOT_SHOWN
        );
        for (name, bad) in [
            ("debug_cache", CANARY),
            ("effort", CANARY),
            ("display.scroll_speed", "21"),
            ("display.scroll_speed", "0"),
            ("display.scroll_speed", "none"),
            ("instructions.warn_tokens", CANARY),
            ("compact_at", "101%"),
            ("compact_at", CANARY),
            ("retry.budget_usd", CANARY),
            ("retry.budget_usd", "0.1234567"),
            ("model", "sk ant CANARY"),
            ("endpoint", CANARY),
            ("key_env", "canary0123456789abcdefgh"),
            ("tools", CANARY),
            ("tools", "none,bash"),
            ("tools", "bash,,none"),
            ("tools", "two words"),
            ("tools", "none,ghp_CANARY0123456789abcdef"),
            ("aliases", "a b"),
            ("key", CANARY),
            ("voice.endpoint", CANARY),
            ("voice.voice", "Two Words"),
            ("voice.language", "EN"),
            ("voice.language", "eng"),
            ("voice.instructions", ""),
            ("voice.instructions", "two\nlines"),
        ] {
            let spec = find(name).unwrap();
            let error = spec.parse(bad).unwrap_err();
            assert!(
                bad.len() < 4 || !error.rule.contains(bad),
                "{name}: {}",
                error.rule
            );
            assert!(!error.rule.contains("CANARY"), "{name}");
        }
        assert_eq!(
            find("effort").unwrap().describe(Form::Rule),
            "must be one of low, medium, high, xhigh, max"
        );
        assert_eq!(
            find("key_env").unwrap().describe(Form::Rule),
            "needs a variable name of letters, digits and _"
        );
    }

    #[test]
    fn the_strategy_setting_takes_default_or_a_plugin_id() {
        let spec = find("context.strategy").unwrap();
        for good in ["default", "notes", "my-notes", " notes "] {
            assert_eq!(
                spec.parse(good).unwrap(),
                SettingValue::Text(good.trim().to_owned()),
                "{good}"
            );
        }
        for bad in ["", "Notes", "a", "9lives", "two words", &"x".repeat(33)] {
            let error = spec.parse(bad).unwrap_err();
            assert_eq!(
                error.rule,
                "needs default or a plugin id of 2 to 32 characters of a-z, 0-9 and -, starting with a letter",
                "{bad}"
            );
        }
    }

    #[test]
    fn values_round_trip_through_their_canonical_text() {
        for (name, text) in [
            ("debug_cache", "on"),
            ("effort", "high"),
            ("display.scroll_speed", "3"),
            ("instructions.warn_tokens", "8000"),
            ("compact_at", "85%"),
            ("compact_at", "400000"),
            ("retry.budget_usd", "1.25"),
            ("tools", "bash"),
        ] {
            let spec = find(name).unwrap();
            let value = spec.parse(text).unwrap();
            assert_eq!(value.text(), text, "{name}");
            assert_eq!(spec.parse(&value.text()).unwrap(), value, "{name}");
            let json = serde_json::to_string(&value).unwrap();
            let back: SettingValue = serde_json::from_str(&json).unwrap();
            assert_eq!(back, value, "{name}");
        }
        assert_eq!(
            serde_json::to_string(&SettingValue::Bool(true)).unwrap(),
            r#"{"kind":"bool","text":"on"}"#
        );
        assert_eq!(
            serde_json::to_string(&SettingValue::Usd { micros: 500_000 }).unwrap(),
            r#"{"kind":"usd","text":"0.5"}"#
        );
        assert_eq!(SettingValue::Usd { micros: 500_000 }.shown(), "$0.50");
        assert_eq!(SettingValue::Tokens(40_000).shown(), "40,000");
    }

    #[test]
    fn an_unknown_value_kind_reads_back_and_reserialises_unchanged() {
        for json in [
            r#"{"kind":"term_size","text":"100x30"}"#,
            r#"{"kind":"bool","text":"maybe"}"#,
            r#"{"kind":"tokens","text":"40k"}"#,
        ] {
            let value: SettingValue = serde_json::from_str(json).unwrap();
            assert!(matches!(value, SettingValue::Other { .. }), "{json}");
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
        }
    }

    #[test]
    fn tokens_or_percent_is_one_parser() {
        assert_eq!(
            TokensOrPercent::parse("400k"),
            Some(TokensOrPercent::Tokens(400_000))
        );
        assert_eq!(
            TokensOrPercent::parse("0%"),
            Some(TokensOrPercent::Share(Percent::new(0).unwrap()))
        );
        for bad in ["101%", "-5", "85.5%", "%", "", "1e6", "+85%"] {
            assert_eq!(TokensOrPercent::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn paths_parse_and_print_in_one_canonical_form() {
        for (text, canonical) in [
            ("effort", "effort"),
            ("default", "default"),
            ("display.theme", "display.theme"),
            ("endpoints.manifest.pricing", "endpoints.manifest.pricing"),
            (
                "models.\"claude-opus-5-5\".compact_at",
                "models.\"claude-opus-5-5\".compact_at",
            ),
            (
                "models.claude-opus-5-5.compact_at",
                "models.\"claude-opus-5-5\".compact_at",
            ),
            (
                "models.'claude-sonnet-5.5'.context_window",
                "models.\"claude-sonnet-5.5\".context_window",
            ),
            ("models.\"a/b:c@d\".aliases", "models.\"a/b:c@d\".aliases"),
            (
                "models.\"claude-sonnet-4-5-20250929\".compact_at",
                "models.\"claude-sonnet-4-5-20250929\".compact_at",
            ),
            (
                "endpoints.gw.models.\"anthropic/claude-sonnet-4.5\".compact_at",
                "endpoints.gw.models.\"anthropic/claude-sonnet-4.5\".compact_at",
            ),
            (
                "endpoints.manifest.models.\"claude-opus-5-5\".compact_at",
                "endpoints.manifest.models.\"claude-opus-5-5\".compact_at",
            ),
        ] {
            let target = parse_target(text).unwrap();
            let shown = match &target {
                Target::Name(spec) => spec.file_key().to_owned(),
                Target::Path(path) => path.to_string(),
            };
            assert_eq!(shown, canonical, "{text}");
            if let Target::Path(path) = target {
                assert_eq!(parse_target(&path.to_string()).unwrap(), Target::Path(path));
            }
        }
    }

    #[test]
    fn bad_paths_and_names_are_never_quoted() {
        for text in [
            CANARY,
            "abcdefghij0123456789xyzw",
            "endpoints.Bad.pricing",
            "endpoints.-x.pricing",
            "agents.Bad.time",
            "models.\"a b\".compact_at",
            "models.abcdefghij0123456789xyzw.compact_at",
            "ask_secret",
            "my_token",
            "api_password",
            "Effort",
        ] {
            let error = parse_target(text).unwrap_err();
            assert_eq!(error, TargetError::NotAName, "{text}");
            assert_eq!(error.to_string(), NOT_A_NAME);
        }
        assert_eq!(
            parse_target("contxt_window").unwrap_err().to_string(),
            "unknown setting contxt_window; did you mean context_window?"
        );
        assert_eq!(
            parse_target("agents.reviewer.time")
                .unwrap_err()
                .to_string(),
            "agents.reviewer.time is planned with agent types and can't be set yet; a preset goes in [agents.presets.reviewer] in config.toml"
        );
        assert_eq!(
            parse_target("agent").unwrap_err().to_string(),
            "agent is planned with agent types and can't be set yet."
        );
        assert!(matches!(
            parse_target("endpoints.gw.effort"),
            Err(TargetError::WrongSite { .. })
        ));
        assert!(matches!(
            parse_target("endpoints.gw.models.\"m\".aliases"),
            Err(TargetError::WrongSite { .. })
        ));
    }

    #[test]
    fn a_configured_endpoint_name_that_looks_like_a_key_takes_paths() {
        let name = "prod-gateway-eu-west-2";
        assert!(crate::looks_like_key(name));
        let configured = [name, "main"];
        let among = Endpoints::Configured(&configured);
        for (text, place) in [
            (
                format!("endpoints.{name}.pricing"),
                Place::Endpoint(name.to_owned()),
            ),
            (
                format!("endpoints.{name}.models.\"claude-opus-5-5\".compact_at"),
                Place::EndpointModel {
                    endpoint: name.to_owned(),
                    model: "claude-opus-5-5".to_owned(),
                },
            ),
        ] {
            let Ok(Target::Path(path)) = parse_target_among(&text, among) else {
                panic!("{text} was refused");
            };
            assert_eq!(path.place, place, "{text}");
            assert_eq!(path.to_string(), text);
            assert_eq!(parse_target(&text).unwrap_err(), TargetError::NotAName);
            let wire = serde_json::to_string(&path).unwrap();
            assert_eq!(serde_json::from_str::<SitePath>(&wire).unwrap(), path);
        }
        for text in [
            "endpoints.prod-gateway-eu-west-9.pricing".to_owned(),
            format!("endpoints.\"{name}\".pricing"),
            format!("endpoints.main.models.{name}.compact_at"),
            format!("models.{name}.compact_at"),
            format!("agents.{name}.time"),
            format!("{name}.pricing"),
            format!("endpoints.main.{name}"),
            format!("endpoints.{CANARY}.pricing"),
        ] {
            let error = parse_target_among(&text, among).unwrap_err();
            assert_eq!(error, TargetError::NotAName, "{text}");
            assert_eq!(error.to_string(), NOT_A_NAME);
        }
        let error =
            parse_target_among(&format!("endpoints.{CANARY}.pricing"), Endpoints::Any).unwrap_err();
        assert_eq!(error.to_string(), NOT_A_NAME);
        assert!(
            serde_json::from_str::<SitePath>(&format!("\"endpoints.{CANARY}.pricing\"")).is_err()
        );
    }

    #[test]
    fn a_typo_after_a_configured_endpoint_names_the_setting_as_unknown() {
        let name = "prod-gateway-eu-west-2";
        let configured = [name, "main"];
        let among = Endpoints::Configured(&configured);
        for endpoint in configured {
            let text = format!("endpoints.{endpoint}.prcing");
            let error = parse_target_among(&text, among).unwrap_err();
            assert!(
                matches!(&error, TargetError::Unknown { given, .. } if *given == text),
                "{error:?}"
            );
            assert!(
                error
                    .to_string()
                    .starts_with(&format!("unknown setting {text}")),
                "{error}"
            );
        }
        for text in [
            format!("endpoints.{name}.{CANARY}"),
            format!("endpoints.{name}.models.\"claude-opus-5-5\".prcing"),
            "endpoints.prod-gateway-eu-west-9.prcing".to_owned(),
        ] {
            let error = parse_target_among(&text, among).unwrap_err();
            assert_eq!(error, TargetError::NotAName, "{text}");
        }
    }

    #[test]
    fn a_path_under_a_hidden_model_is_shown_with_a_placeholder() {
        let window = find("context_window").unwrap();
        let compact = find("compact_at").unwrap();
        let key = "sk-ant-api03-CANARYMODEL0000";
        let model = SitePath::new(Place::Model(key.to_owned()), window);
        assert_eq!(model.shown(), "models.\"MODEL\".context_window");
        assert!(model.to_string().contains(key));
        let pair = SitePath::new(
            Place::EndpointModel {
                endpoint: "gw".to_owned(),
                model: key.to_owned(),
            },
            compact,
        );
        assert_eq!(pair.shown(), "endpoints.gw.models.\"MODEL\".compact_at");
        let real = SitePath::new(Place::Model("claude-opus-5-5".to_owned()), window);
        assert_eq!(real.shown(), real.to_string());
        let endpoint = SitePath::new(Place::Endpoint("gw".to_owned()), find("pricing").unwrap());
        assert_eq!(endpoint.shown(), "endpoints.gw.pricing");
    }

    #[test]
    fn model_ids_in_a_list_hide_each_key() {
        let aliases = find("aliases").unwrap();
        let shown = aliases.shown(&SettingValue::List(vec![
            "claude-opus-5-5".to_owned(),
            "sk-ant-api03-CANARYMODEL0000".to_owned(),
            "hf_CANARYGhIjKlMnOpQrStUvWx".to_owned(),
        ]));
        assert!(shown.contains("claude-opus-5-5"), "{shown}");
        assert!(!shown.contains("CANARY"), "{shown}");
        assert_eq!(shown.matches(crate::NOT_SHOWN).count(), 2, "{shown}");
    }

    #[test]
    fn every_saved_place_on_the_settings_page_parses_back_to_its_setting() {
        let page = render_docs();
        for spec in SETTINGS.iter().filter(|s| s.is_built()) {
            for site in spec.sites {
                let (shown, typed) = match site {
                    Site::Endpoint => (
                        format!("`endpoints.<e>.{}`", spec.leaf()),
                        format!("endpoints.gw.{}", spec.leaf()),
                    ),
                    Site::Model => (
                        format!("`models.\"<m>\".{}`", spec.leaf()),
                        format!("models.\"m\".{}", spec.leaf()),
                    ),
                    Site::EndpointModel => (
                        format!("`endpoints.<e>.models.\"<m>\".{}`", spec.leaf()),
                        format!("endpoints.gw.models.\"m\".{}", spec.leaf()),
                    ),
                    Site::PluginHook => (
                        "`[plugins.<id>.hooks]` `<hook>`".to_owned(),
                        "plugins.acme.hooks.fill".to_owned(),
                    ),
                    Site::PluginSlot => (
                        "`[plugins.<id>.slots]` `<name>`".to_owned(),
                        "plugins.acme.slots.usage".to_owned(),
                    ),
                    Site::Plugin => {
                        let key = spec.name.rsplit('.').next().unwrap();
                        (
                            format!("`[plugins.<id>]` `{key}`"),
                            format!("plugins.acme.{key}"),
                        )
                    }
                    Site::Global | Site::Shipped | Site::Agent => continue,
                };
                assert!(page.contains(&shown), "{shown}");
                let parsed = parse_target(&typed);
                assert!(
                    matches!(&parsed, Ok(Target::Path(path)) if path.spec().name == spec.name),
                    "{typed}: {parsed:?}"
                );
            }
        }
    }

    #[test]
    fn a_section_switch_parses_to_its_plugin_and_section_and_bad_names_are_refused() {
        let Ok(Target::Path(path)) = parse_target("plugins.agent-viz.slots.topology") else {
            panic!("the section switch was refused");
        };
        assert_eq!(
            path.place,
            Place::PluginSlot {
                plugin: "agent-viz".to_owned(),
                section: "topology".to_owned(),
            }
        );
        assert_eq!(path.place.section().as_deref(), Some("agent-viz/topology"));
        assert_eq!(path.spec().name, PLUGIN_SLOT);
        assert_eq!(path.to_string(), "plugins.agent-viz.slots.topology");
        for bad in [
            "plugins.Agent!.slots.tree",
            "plugins.a.slots.tree",
            "plugins.agent-viz.slots.t",
            "plugins.agent-viz.slots.Tree",
            "plugins.agent-viz.slots",
            "plugins.agent-viz.slots.topology.more",
        ] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_plugin_switch_parses_to_its_plugin_and_key_and_bad_names_are_refused() {
        for (typed, name) in [
            ("plugins.time.enabled", PLUGIN_ENABLED),
            ("plugins.self-check.installed", PLUGIN_INSTALLED),
        ] {
            let Ok(Target::Path(path)) = parse_target(typed) else {
                panic!("{typed} was refused");
            };
            assert_eq!(path.spec().name, name);
            assert_eq!(path.to_string(), typed);
            assert_eq!(path.segments().join("."), typed);
            assert_eq!(path.place.site(), Site::Plugin);
        }
        assert_eq!(
            SitePath::plugin_switch("time", "enabled").unwrap().place,
            Place::Plugin("time".to_owned())
        );
        assert!(SitePath::plugin_switch("time", "hooks").is_none());
        for bad in [
            "plugins.T.enabled",
            "plugins.a.enabled",
            "plugins.time.\"enabled\"",
            "plugins.time.removed",
            "plugins.time.enabled.more",
        ] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_agent_limits_are_top_level_names_beside_the_reserved_agent_types() {
        for (name, default, max) in [
            ("agents.max_running", SettingValue::Int(4), MAX_RUNNING_MAX),
            (
                "agents.max_tree_running",
                SettingValue::Text("none".into()),
                MAX_AGENTS_MAX,
            ),
            ("agents.max_agents", SettingValue::Int(50), MAX_AGENTS_MAX),
            ("agents.max_depth", SettingValue::Int(2), MAX_DEPTH_MAX),
        ] {
            let Ok(Target::Name(spec)) = parse_target(name) else {
                panic!("{name} was refused");
            };
            assert_eq!(spec.name, name);
            assert_eq!(spec.section, Section::Agents);
            assert_eq!(spec.default_value(), Some(default));
            assert_eq!(
                spec.kind,
                Kind::Int {
                    min: 1,
                    max,
                    none: true
                }
            );
            assert!(!spec.mid_session, "{name}");
            assert_eq!(spec.applies, Applies::NextSession);
            let rule = format!("must be a whole number from 1 to {max}, or none");
            for bad in [
                "0".to_owned(),
                (max + 1).to_string(),
                "four".to_owned(),
                "nothing".to_owned(),
            ] {
                assert_eq!(spec.parse(&bad).unwrap_err().rule, rule, "{name} {bad}");
            }
            assert_eq!(spec.parse(&max.to_string()), Ok(SettingValue::Int(max)));
            assert_eq!(
                spec.parse(" none "),
                Ok(SettingValue::Text("none".to_owned()))
            );
        }
        assert_eq!(
            parse_target("agents.max_running.time")
                .unwrap_err()
                .to_string(),
            "unknown setting agents.max_running.time; see wisp config list"
        );
        assert_eq!(
            parse_target("agents.reviewer.time")
                .unwrap_err()
                .to_string(),
            "agents.reviewer.time is planned with agent types and can't be set yet; a preset goes in [agents.presets.reviewer] in config.toml"
        );
    }

    #[test]
    fn the_agents_names_come_from_one_place() {
        assert_eq!(PRESETS_GROUP, format!("{AGENTS_GROUP}.{PRESETS_TABLE}"));
        assert_eq!(
            agents_group_keys(),
            [
                "max_agents",
                "max_depth",
                "max_running",
                "max_tree_running",
                "presets",
                "wake_when_stuck",
                "warm_refreshes"
            ]
        );
    }

    #[test]
    fn keep_warm_is_off_by_default_with_eight_refreshes_a_spell() {
        let Ok(Target::Name(limit)) = parse_target("agents.warm_refreshes") else {
            panic!("agents.warm_refreshes was refused");
        };
        assert_eq!(limit.section, Section::Agents);
        assert_eq!(limit.default_value(), Some(SettingValue::Int(8)));
        assert_eq!(
            limit.kind,
            Kind::Int {
                min: 0,
                max: 24,
                none: false
            }
        );
        assert!(!limit.mid_session);
        assert_eq!(limit.applies, Applies::NextSession);
        assert_eq!(limit.parse("0"), Ok(SettingValue::Int(0)));
        assert_eq!(limit.parse("24"), Ok(SettingValue::Int(24)));
        for bad in ["25", "-1", "none", "eight"] {
            assert!(limit.parse(bad).is_err(), "{bad}");
        }
        let Ok(Target::Name(main)) = parse_target("keep_warm") else {
            panic!("keep_warm was refused");
        };
        assert_eq!(main.kind, Kind::Bool);
        assert_eq!(main.default_value(), Some(SettingValue::Bool(false)));
        assert!(!main.mid_session);
        assert_eq!(main.applies, Applies::NextSession);
        assert_eq!(main.parse("on"), Ok(SettingValue::Bool(true)));
    }

    #[test]
    fn a_preset_path_points_to_the_file() {
        for (text, name) in [
            ("agents.presets.reviewer.effort", Some("reviewer")),
            ("agents.presets.deep-research.model", Some("deep-research")),
            ("agents.presets.reviewer", Some("reviewer")),
            ("agents.presets", None),
            ("agents.presets.2x.effort", None),
        ] {
            let error = parse_target(text).unwrap_err();
            assert_eq!(
                error,
                TargetError::Preset {
                    name: name.map(str::to_owned)
                },
                "{text}"
            );
        }
        assert_eq!(
            parse_target("agents.presets.reviewer.effort")
                .unwrap_err()
                .to_string(),
            "presets live only in config.toml; edit [agents.presets.reviewer] there"
        );
        assert_eq!(
            parse_target("agents.presets").unwrap_err().to_string(),
            "presets live only in config.toml; edit [agents.presets.NAME] there"
        );
        for text in [
            "agents.presets.sk-ant-CANARY0123456789abcdef.effort",
            "agents.presets.\"x\".effort",
            "agents.presets.x.Effort",
            "agents.presets.Bad.effort",
        ] {
            assert_eq!(
                parse_target(text).unwrap_err(),
                TargetError::NotAName,
                "{text}"
            );
        }
    }

    #[test]
    fn every_bool_row_says_config_toml_takes_true_or_false() {
        let page = render_docs();
        for spec in SETTINGS
            .iter()
            .filter(|s| s.is_built() && s.kind == Kind::Bool)
        {
            let variables = if spec.env.is_empty() {
                ""
            } else {
                " and in variables"
            };
            let row = format!(
                "| `{}` | `true` or `false` in `config.toml`, `on` or `off` on the command line{variables} (",
                spec.name
            );
            assert!(page.contains(&row), "{}", spec.name);
        }
        assert!(page.contains("| `keep_warm` | `true` or `false` in `config.toml`, `on` or `off` on the command line (off)"));
        assert!(page.contains("| `display.mouse` | `true` or `false` in `config.toml`, `on` or `off` on the command line and in variables (on)"));
    }

    #[test]
    fn the_settings_page_is_the_rendered_registry() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/settings.md");
        let rendered = render_docs();
        if std::env::var_os("WISP_BLESS").is_some() {
            std::fs::write(&path, &rendered).unwrap();
        }
        let saved = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            saved, rendered,
            "run with WISP_BLESS=1 to regenerate docs/settings.md"
        );
    }
}

#[test]
fn a_tools_list_hides_a_name_that_could_be_a_pasted_key() {
    let tools = find("tools").unwrap();
    assert_eq!(
        tools.shown(&SettingValue::List(vec![
            "bash".into(),
            "ask_secret".into(),
            "ghp_canary012345CANARYcdef".to_ascii_lowercase(),
            "telepathy".into(),
        ])),
        format!("bash, ask_secret, {}, telepathy", crate::NOT_SHOWN)
    );
}

#[test]
fn a_section_list_reads_back_sorted_and_deduplicated_and_refuses_a_broken_name() {
    for name in ["display.sidebar_folded", "display.sidebar_pinned"] {
        let spec = find(name).unwrap();
        assert_eq!(spec.owner, Owner::Client, "{name}");
        assert_eq!(spec.dynamic, Some(Dynamic::Sections), "{name}");
        assert_eq!(spec.default_value(), Some(SettingValue::List(vec![])));
        let value = spec
            .parse(" testkit/busy, context/usage,testkit/busy,context/window ")
            .unwrap();
        assert_eq!(
            value,
            SettingValue::List(vec!["context/window".into(), "testkit/busy".into()])
        );
        assert_eq!(spec.parse(&value.text()), Ok(value.clone()));
        assert_eq!(spec.parse_list(&[]), Ok(SettingValue::List(vec![])));
        assert_eq!(spec.parse("none"), Ok(SettingValue::List(vec![])));
        assert_eq!(
            spec.parse_list(&["gone/section".into()]),
            Ok(SettingValue::List(vec!["gone/section".into()]))
        );
        for bad in [
            "",
            "testkit",
            "testkit/",
            "/busy",
            "Testkit/busy",
            "testkit/b",
            "testkit/busy/x",
            "t/busy",
            "testkit/busy,none",
        ] {
            let refused = spec.parse(bad).unwrap_err();
            assert_eq!(
                refused.rule, "must be none or a list of sidebar sections, each <plugin>/<name>",
                "{bad:?}"
            );
        }
    }
    let Ok(Target::Path(old)) = parse_target("plugins.context.slots.usage") else {
        panic!("the old section path is refused");
    };
    assert_eq!(old.to_string(), "plugins.context.slots.window");
    assert_eq!(current_slot("context", "usage"), "window");
    assert_eq!(current_slot("agent-viz", "usage"), "usage");
    assert_eq!(current_slot("context", "window"), "window");
    assert!(former_slots("context", "window").eq(["usage"]));
    assert!(former_slots("context", "usage").eq([] as [&str; 0]));
}

#[test]
fn each_layer_has_one_word_and_the_two_config_files_share_one() {
    let words: Vec<(Layer, &str)> = vec![
        (Layer::Forced, "forced"),
        (Layer::Session, "session"),
        (Layer::Flag, "flag"),
        (Layer::Env, "variable"),
        (Layer::Mode, "mode"),
        (Layer::Agent, "agent"),
        (Layer::EndpointModel, "config"),
        (Layer::Model, "model"),
        (Layer::Endpoint, "account"),
        (Layer::Config, "config"),
        (Layer::Shipped, "shipped"),
        (Layer::Default, "default"),
        (Layer::Other("detected".into()), "detected"),
    ];
    for (layer, word) in words {
        assert_eq!(layer.word(), word, "{layer:?}");
    }
}

#[test]
fn a_filter_matches_a_label_name_or_help_in_any_case_and_a_blank_one_matches_all() {
    let time = find("time").unwrap();
    let matches = |spec: &SettingSpec, filter: &str| spec.matches(&Query::new(filter));
    for filter in ["stamp", "Message time", "TIME", "  time  ", "", "   "] {
        assert!(matches(time, filter), "{filter:?}");
    }
    assert!(matches(time, "each STAMP"));
    assert!(matches(find("compact_at").unwrap(), "compact"));
    assert!(matches(find("display.theme").unwrap(), "theme"));
    for filter in ["xyz", "stamps each"] {
        assert!(!matches(time, filter), "{filter:?}");
    }
}

const NAMED_PLAIN: [&str; 7] = [
    "key_env",
    "key_store",
    "keychain",
    "context.background_tokens",
    "context.count_tokens",
    "context.verbatim_user_tokens",
    "instructions.warn_tokens",
];

fn ends_in_secret_word(name: &str) -> bool {
    (0..name.len()).any(|at| {
        holds_secret_word(&name[at..])
            && !holds_secret_word(&name[at + 1..])
            && !holds_secret_word(&name[at..name.len() - 1])
    })
}

#[test]
fn every_name_with_a_secret_word_is_secret_or_named_plain() {
    use crate::SECRET_FIELDS;
    for field in SECRET_FIELDS {
        assert!(holds_secret_word(field), "{field}");
    }
    for spec in SETTINGS
        .iter()
        .filter(|s| holds_secret_word(s.name) && !NAMED_PLAIN.contains(&s.name))
    {
        assert!(SECRET_FIELDS.contains(&spec.leaf()), "{}", spec.name);
        assert_ne!(spec.secrecy, Secrecy::Plain, "{}", spec.name);
    }
    for spec in SETTINGS.iter().filter(|s| s.kind == Kind::Secret) {
        assert_eq!(spec.secrecy, Secrecy::Secret, "{}", spec.name);
    }
    for name in NAMED_PLAIN {
        let spec = find(name).unwrap();
        assert!(holds_secret_word(name), "{name}");
        assert_eq!(spec.secrecy, Secrecy::Plain, "{name}");
        assert!(!SECRET_FIELDS.contains(&spec.leaf()), "{name}");
        assert!(!ends_in_secret_word(spec.leaf()), "{name}");
    }
    assert!(ends_in_secret_word("auth_token"));
    assert!(ends_in_secret_word("token"));
    assert!(!ends_in_secret_word("warn_tokens"));
}

#[test]
fn every_setting_describes_its_kind_for_the_docs_and_errors() {
    let rows: Vec<String> = SETTINGS
        .iter()
        .map(|s| {
            format!(
                "{}\n  docs: {}\n  rule: {}",
                s.name,
                s.describe(Form::Docs),
                s.describe(Form::Rule)
            )
        })
        .collect();
    insta::assert_snapshot!(rows.join("\n"));
}

fn declared_row(name: &str) -> crate::plugin::SettingRow {
    crate::plugin::SettingRow {
        name: name.into(),
        label: "Minutes".into(),
        help: "Minutes before away.".into(),
        kind: crate::plugin::RowKind::Int {
            min: 1,
            max: 60,
            default: 10,
        },
    }
}

#[test]
fn a_declared_row_is_found_parsed_and_listed_after_the_compiled_rows() {
    let spec = declare("declared", &declared_row("minutes")).unwrap();
    assert!(std::ptr::eq(find(spec.name).unwrap(), spec));
    assert_eq!(spec.name, "plugins.declared.minutes");
    assert_eq!(spec.default_value(), Some(SettingValue::Int(10)));
    assert_eq!(spec.parse("60"), Ok(SettingValue::Int(60)));
    assert!(spec.parse("61").is_err());
    assert!(spec.parse("off").is_err());
    assert_eq!(spec.plugin, Some("declared"));
    assert!(spec.mid_session && spec.is_declared());
    assert_eq!(spec.owner, Owner::Kernel);
    assert_eq!(spec.applies, Applies::NextTurnMayRebuild);
    let rows: Vec<_> = all().map(|s| s.name).collect();
    assert_eq!(
        &rows[..SETTINGS.len()],
        SETTINGS.iter().map(|s| s.name).collect::<Vec<_>>()
    );
    assert_eq!(rows.last(), Some(&spec.name));
    let row = crate::plugin::SettingRow {
        kind: crate::plugin::RowKind::Bool { default: true },
        ..declared_row("quiet")
    };
    let quiet = declare("declared", &row).unwrap();
    assert_eq!(quiet.default_value(), Some(SettingValue::Bool(true)));
    assert_eq!(quiet.parse("off"), Ok(SettingValue::Bool(false)));
}

#[test]
fn declaring_a_name_again_keeps_the_first_spec() {
    let first = declare("first-spec", &declared_row("minutes")).unwrap();
    let mut changed = declared_row("minutes");
    changed.label = "Changed".into();
    changed.kind = crate::plugin::RowKind::Bool { default: false };
    assert!(std::ptr::eq(
        first,
        declare("first-spec", &changed).unwrap()
    ));
    assert_eq!(first.label, "Minutes");
    assert_eq!(first.default_value(), Some(SettingValue::Int(10)));
}

#[test]
fn a_rows_name_follows_one_rule() {
    for (name, good) in [
        ("a".to_owned(), true),
        ("a_1".to_owned(), true),
        ("a".repeat(32), true),
        ("a1".repeat(16), true),
        ("a".repeat(33), false),
        ("".into(), false),
        ("A".into(), false),
        ("a-b".into(), false),
        ("1a".into(), false),
        (ENABLED_KEY.into(), false),
        (INSTALLED_KEY.into(), false),
        (HOOKS_TABLE.into(), false),
        (SLOTS_TABLE.into(), false),
        ("api_key".into(), false),
        ("max_tokens".into(), false),
    ] {
        assert_eq!(row_name("hotkeys", &name).is_ok(), good, "{name}");
        assert_eq!(
            declare("hotkeys", &declared_row(&name)).is_some(),
            good,
            "{name}"
        );
        assert_eq!(
            matches!(
                parse_target(&format!("plugins.hotkeys.{name}")),
                Ok(Target::Name(_))
            ),
            good,
            "{name}"
        );
    }
}

#[test]
fn a_declared_name_is_a_target_on_the_wire_and_an_unknown_one_stays_unknown_in_the_log() {
    let spec = declare("wire-row", &declared_row("minutes")).unwrap();
    let target = SettingTarget::Name(SettingName::of(spec));
    let encoded = serde_json::to_string(&target).unwrap();
    assert_eq!(encoded, r#"{"name":"plugins.wire-row.minutes"}"#);
    assert_eq!(
        serde_json::from_str::<SettingTarget>(&encoded).unwrap(),
        target
    );
    assert!(
        matches!(parse_target(spec.name), Ok(Target::Name(found)) if std::ptr::eq(spec, found))
    );
    let unknown = r#"{"name":"plugins.missing-row.minutes"}"#;
    assert!(serde_json::from_str::<SettingTarget>(unknown).is_err());
    let logged: LoggedTarget = serde_json::from_str(unknown).unwrap();
    assert!(
        matches!(&logged, LoggedTarget::Unknown { kind: TargetKind::Name, text } if text == "plugins.missing-row.minutes")
    );
    assert_eq!(serde_json::to_string(&logged).unwrap(), unknown);
}
