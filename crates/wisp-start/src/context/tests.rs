use super::*;
use wisp_core::endpoints::ProviderKind::Anthropic;

const ON: Run = Run {
    max_tokens: 64_000,
    auto_compact: true,
    cache_ttl: CacheTtl::OneHour,
};

#[test]
fn the_shipped_window_needs_no_config() {
    let loaded = load_text(None);
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert!(start_warnings(&loaded, "env", "claude-opus-5-5", &[]).is_empty());
    let rows = doctor_rows(&loaded, "env", "claude-opus-5-5", ON, &[]);
    assert_eq!(rows[1], ("window", "1M (shipped)".to_owned()));
    assert_eq!(
        rows[2].1,
        "compacts at 850,000 (window \u{2212} reserve); reserve 150,000"
    );
    let off = Run {
        auto_compact: false,
        ..ON
    };
    let rows = doctor_rows(&loaded, "env", "claude-opus-5-5", off, &[]);
    assert_eq!(
        rows[2].1,
        "would compact at 850,000 (window \u{2212} reserve); reserve 150,000; automatic compaction is off"
    );
    let capped = load_text(Some(
        "[models.\"claude-opus-5-5\"]\ncontext_window = 272000\ncompact_at = 400000\n",
    ));
    let rows = doctor_rows(&capped, "env", "claude-opus-5-5", ON, &[]);
    assert_eq!(
        rows[2].1,
        "compacts at 208,000 (config 400,000, capped at window − reserve); reserve 64,000"
    );
    let capped_reply = load_text(Some(
        "[models.\"claude-opus-5-5\"]\ncontext_window = 112000\nmax_output = 32000\ncompact_at = 70000\n",
    ));
    let rows = doctor_rows(&capped_reply, "env", "claude-opus-5-5", ON, &[]);
    assert_eq!(rows[2].1, "compacts at 70,000 (config); reserve 40,000");
    assert!(
        !rows
            .iter()
            .any(|(_, value)| value.starts_with("background compaction rarely runs"))
    );
}

#[test]
fn the_flag_point_shows_in_doctor_and_warns_below_the_floor() {
    let flags = crate::settings::flag_layer(Vec::new(), &["compact_at=10000".to_owned()]).unwrap();
    let loaded =
        load_text(Some("[models.\"claude-opus-5-5\"]\ncompact_at = 600000\n")).with_flags(&flags);
    let rows = doctor_rows(&loaded, "env", "claude-opus-5-5", ON, &[]);
    assert_eq!(rows[2].1, "compacts at 10,000 (flag); reserve 150,000");
    let warnings: Vec<&String> = rows
        .iter()
        .filter(|(key, _)| *key == "warning")
        .map(|(_, w)| w)
        .collect();
    assert_eq!(
        warnings,
        [
            "compact_at 10000 is below 40,000: once the head after a compaction is above it, every turn starts with a compaction, ~$0.123 a turn to compact and ~$0.12 to write the head again, at claude-opus-5-5's prices.",
            "background compaction rarely runs below about 60,000, and claude-opus-5-5 compacts at 10,000; compaction will usually wait at the point."
        ]
    );
    let five = point_warnings(
        &loaded,
        "env",
        "claude-opus-5-5",
        Run {
            cache_ttl: CacheTtl::FiveMinutes,
            ..ON
        },
        &[],
    );
    assert!(
        five[0].contains("and ~$0.075 to write the head again"),
        "{five:?}"
    );
    assert!(start_warnings(&loaded, "env", "claude-opus-5-5", &[]).is_empty());
}

#[test]
fn an_unknown_model_says_how_to_set_its_window() {
    let loaded = load_text(None);
    let warnings = start_warnings(&loaded, "env", "my-model", &[]);
    assert_eq!(
        warnings,
        [
            "Window unknown for my-model: set context_window under [models.\"my-model\"] in config.toml to enable automatic compaction."
        ]
    );
}

#[test]
fn a_limit_above_the_documented_window_warns_at_start_and_in_doctor() {
    let loaded = load_text(Some(
        "[models.\"claude-opus-5-5\"]\ncontext_window = 2000000\n",
    ));
    let warnings = start_warnings(&loaded, "env", "claude-opus-5-5", &[]);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with(
            "context_window 2000000 for claude-opus-5-5 is above the documented window of 1M (https://platform.claude.com/docs/en/build-with-claude/context-windows)"
        ),
        "{warnings:?}"
    );
    let rows = doctor_rows(&loaded, "env", "claude-opus-5-5", ON, &[]);
    assert_eq!(rows[1].1, "2M (config; documented 1M)");
    assert!(rows.iter().any(|(key, _)| *key == "warning"), "{rows:?}");
    let text = doctor_text(&rows);
    assert!(text.starts_with("\ncontext\n  model"), "{text}");
}

#[test]
fn a_model_with_no_shipped_row_takes_any_limit_quietly() {
    let loaded = load_text(Some("[models.\"my-model\"]\ncontext_window = 120000\n"));
    assert!(start_warnings(&loaded, "env", "my-model", &[]).is_empty());
    let rows = doctor_rows(&loaded, "env", "my-model", ON, &[]);
    assert_eq!(rows[1].1, "120k (config)");
}

#[test]
fn a_limit_below_the_floor_is_refused_at_its_row() {
    let loaded = load_text(Some(
        "[models]\n\"a\" = { context_window = 400000 }\n\"my-model\" = { context_window = 100000 }\n",
    ));
    assert_eq!(
        loaded.refused.as_deref(),
        Some(
            "config.toml line 3: context_window 100000 for my-model puts the compaction point at 36,000 (window \u{2212} reserve 64,000); it must be at least 40,000. Set at least 104000."
        )
    );
    let accepted = load_text(Some("[models.\"my-model\"]\ncontext_window = 104000\n"));
    assert!(accepted.warnings.is_empty(), "{:?}", accepted.warnings);
    let rows = doctor_rows(&accepted, "env", "my-model", ON, &[]);
    assert!(rows[2].1.starts_with("compacts at 40,000"), "{rows:?}");
}

#[test]
fn the_endpoint_table_beats_the_model_table() {
    let text = "[models.\"claude-opus-5-5\"]\ncontext_window = 400000\n\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n\n[endpoints.gw.models.\"claude-opus-5-5\"]\ncontext_window = 300000\ncompact_at = \"60%\"\n";
    let loaded = load_text(Some(text));
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let rows = doctor_rows(&loaded, "gw", "claude-opus-5-5", ON, &[]);
    assert_eq!(rows[1].1, "300k (account; documented 1M)");
    assert!(
        rows[2].1.starts_with("compacts at 180,000 (account)"),
        "{rows:?}"
    );
    let rows = doctor_rows(&loaded, "env", "claude-opus-5-5", ON, &[]);
    assert_eq!(rows[1].1, "400k (config; documented 1M)");
}

#[test]
fn doctor_names_each_endpoints_search_form_and_its_sources() {
    let loaded = load_text(Some(
        "[endpoints.trusted]\nnative_web_search = true\n\n[endpoints.off]\nnative_web_search = false\n\n[endpoints.own.models.\"claude-haiku-5-5\"]\nweb_search = []\n\n[endpoints.stray.models.\"claude-haiku-5-5\"]\nweb_search = [\"web_search\"]\n",
    ));
    let endpoints = [
        ("direct".to_owned(), Anthropic),
        ("gateway".to_owned(), Anthropic),
        ("off".to_owned(), Anthropic),
        ("own".to_owned(), Anthropic),
        ("stray".to_owned(), Anthropic),
        ("trusted".to_owned(), Anthropic),
    ];
    let rows = search_rows(&loaded, &endpoints, "claude-haiku-5-5", true, &[]);
    assert_eq!(
        rows,
        [
            ("model".to_owned(), "claude-haiku-5-5".to_owned()),
            (
                "direct".to_owned(),
                "on (web_search: shipped; native_web_search: default)".to_owned()
            ),
            (
                "gateway".to_owned(),
                "on (web_search: shipped; native_web_search: default)".to_owned()
            ),
            (
                "off".to_owned(),
                "off (off for this account) (web_search: shipped; native_web_search: account)"
                    .to_owned()
            ),
            (
                "own".to_owned(),
                "off (not on this model) (web_search: config; native_web_search: default)"
                    .to_owned()
            ),
            (
                "stray".to_owned(),
                "on (web_search: shipped; native_web_search: default)".to_owned()
            ),
            (
                "trusted".to_owned(),
                "on (web_search: shipped; native_web_search: account)".to_owned()
            ),
        ]
    );
    let off = search_rows(&loaded, &endpoints[..1], "claude-haiku-5-5", false, &[]);
    assert_eq!(
        off[1].1,
        "off (web_search: shipped; native_web_search: default)"
    );
    assert!(search_text(&off).starts_with("\nweb search\n  model "));
}

#[test]
fn doctor_names_a_per_place_flag_as_the_source_of_search_facts() {
    let flags = crate::settings::flag_layer(
        Vec::new(),
        &[
            "endpoints.gateway.native_web_search=on".to_owned(),
            "models.\"claude-haiku-5-5\".web_search=web_search_20250305".to_owned(),
        ],
    )
    .unwrap();
    let loaded = load_text(Some("")).with_flags(&flags);
    let rows = search_rows(
        &loaded,
        &[("gateway".to_owned(), Anthropic)],
        "claude-haiku-5-5",
        true,
        &[],
    );
    assert_eq!(rows[1].1, "on (web_search: flag; native_web_search: flag)");
}

#[test]
fn the_tuis_search_forms_are_the_kernels_for_every_pair_it_can_name() {
    let loaded = load_text(Some(
        "[models.\"my-new-model\"]\nweb_search = [\"web_search_20250305\"]\n\n[endpoints.trusted]\nnative_web_search = true\n\n[endpoints.off]\nnative_web_search = false\n\n[endpoints.own.models.\"claude-haiku-5-5\"]\nweb_search = []\n\n[endpoints.own.models.\"their-model\"]\nweb_search = [\"web_search_20250305\"]\n",
    ));
    assert!(loaded.named.iter().any(|m| m == "my-new-model"));
    assert!(loaded.named.iter().any(|m| m == "their-model"));
    let endpoints = [
        ("direct".to_owned(), Anthropic),
        ("gateway".to_owned(), Anthropic),
        ("off".to_owned(), Anthropic),
        ("own".to_owned(), Anthropic),
        ("trusted".to_owned(), Anthropic),
    ];
    let priced = ["claude-haiku-5-5".to_owned(), "claude-opus-5-5".to_owned()];
    let forms = search_forms(&loaded, &endpoints, &priced, true, false, None, &[]);
    let mut ids: Vec<String> = loaded.named.iter().cloned().collect();
    ids.extend(priced.iter().cloned());
    ids.push("never-heard-of".to_owned());
    for (endpoint, wire) in &endpoints {
        for id in &ids {
            let kernel = SearchFacts::resolve(&loaded.settings, endpoint, id, *wire, None);
            assert_eq!(
                forms.form(endpoint, id),
                shown_form(kernel.form(true, false)),
                "{endpoint} {id}"
            );
        }
    }
    let native = Some(SearchForm::Native);
    assert_eq!(forms.form("direct", "my-new-model"), native);
    assert_eq!(forms.form("own", "their-model"), native);
    assert_eq!(forms.form("trusted", "claude-opus-5-5"), native);
    assert_eq!(forms.form("gateway", "claude-opus-5-5"), native);
    assert_eq!(
        forms.form("off", "claude-opus-5-5"),
        Some(SearchForm::None(NoSearch::TurnedOff))
    );
    let kept = search_forms(
        &loaded,
        &endpoints,
        &priced,
        true,
        false,
        Some(("gateway", "claude-opus-5-5", false)),
        &[],
    );
    assert_eq!(
        kept.form("gateway", "claude-opus-5-5"),
        Some(SearchForm::None(NoSearch::NotStarted))
    );
    assert_eq!(kept.form("gateway", "claude-haiku-5-5"), native);
    assert_eq!(kept.form("trusted", "claude-opus-5-5"), native);
    let kept = search_forms(
        &loaded,
        &endpoints,
        &priced,
        true,
        false,
        Some(("off", "claude-opus-5-5", true)),
        &[],
    );
    assert_eq!(kept.form("off", "claude-opus-5-5"), native);
    let off = search_forms(&loaded, &endpoints, &priced, false, true, None, &[]);
    assert_eq!(off.pairs, forms.pairs);
    assert_eq!(off.form("direct", "claude-opus-5-5"), None);
}

#[test]
fn the_tuis_search_forms_follow_the_switch_as_the_kernels_do() {
    let (registry, _) = crate::tools::registry();
    let all = registry.default_on();
    let without = all.iter().filter(|n| *n != "web_search").cloned().collect();
    let loaded = load_text(Some("[endpoints.off]\nnative_web_search = false\n"));
    let endpoints = [
        ("direct".to_owned(), Anthropic),
        ("off".to_owned(), Anthropic),
    ];
    let model = "claude-opus-5-5";
    let priced = [model.to_owned()];
    let logged = [
        ("direct", false, SearchForm::None(NoSearch::NotStarted)),
        ("off", true, SearchForm::Native),
    ];
    for (pair, had, before) in logged {
        let kernel = SearchFacts::resolve(&loaded.settings, pair, model, Anthropic, None);
        let kept = Some((pair, model, had));
        let mut forms = search_forms(&loaded, &endpoints, &priced, true, false, kept, &[]);
        assert_eq!(forms.form(pair, model), Some(before), "{pair}");
        forms.follow(&registry.catalog(&without, &[]).plugins);
        assert_eq!(forms.form(pair, model), None, "{pair}");
        forms.follow(&registry.catalog(&all, &[]).plugins);
        assert_eq!(
            forms.form(pair, model),
            shown_form(kernel.form(true, false)),
            "{pair}"
        );
    }
}

#[test]
fn a_left_out_search_shows_no_form() {
    assert_eq!(shown_form(SearchForm::None(NoSearch::NotRequested)), None);
    assert_eq!(
        shown_form(SearchForm::None(NoSearch::Refused)),
        Some(SearchForm::None(NoSearch::Refused))
    );
}

fn background_lines(text: &str, run: Run) -> Vec<String> {
    let loaded = load_text(Some(text));
    point_warnings(&loaded, "env", "claude-opus-5-5", run, &[])
        .into_iter()
        .filter(|w| w.starts_with("background compaction"))
        .collect()
}

#[test]
fn the_background_line_follows_the_limit_the_band_and_the_switches() {
    let limit = |tokens: u64| format!("[models.\"claude-opus-5-5\"]\ncontext_window = {tokens}\n");
    assert_eq!(
        background_lines(&limit(110_000), ON),
        [
            "background compaction rarely runs below about 60,000, and claude-opus-5-5 compacts at 46,000; compaction will usually wait at the point."
        ]
    );
    assert!(background_lines(&limit(130_000), ON).is_empty());
    let narrow = format!("[context]\nbackground_tokens = 20000\n\n{}", limit(110_000));
    assert!(background_lines(&narrow, ON).is_empty());
    let wide = format!("[context]\nbackground_tokens = 60000\n\n{}", limit(130_000));
    assert_eq!(
        background_lines(&wide, ON),
        [
            "background compaction rarely runs below about 80,000, and claude-opus-5-5 compacts at 66,000; compaction will usually wait at the point."
        ]
    );
    let off = format!(
        "[context]\nbackground_compaction = false\n\n{}",
        limit(110_000)
    );
    assert!(background_lines(&off, ON).is_empty());
    let model_off =
        "[models.\"claude-opus-5-5\"]\ncontext_window = 110000\nbackground_compaction = false\n";
    assert!(background_lines(model_off, ON).is_empty());
    let manual = Run {
        auto_compact: false,
        ..ON
    };
    assert!(background_lines(&limit(110_000), manual).is_empty());
    let no_native =
        "[models.\"claude-opus-5-5\"]\ncontext_window = 110000\nnative_compaction = false\n";
    assert!(background_lines(no_native, ON).is_empty());
}

fn flagged_background_lines(text: &str, sets: &[&str]) -> Vec<String> {
    let sets: Vec<String> = sets.iter().map(|s| (*s).to_owned()).collect();
    let flags = crate::settings::flag_layer(Vec::new(), &sets).unwrap();
    let loaded = load_text(Some(text)).with_flags(&flags);
    point_warnings(&loaded, "env", "claude-opus-5-5", ON, &[])
        .into_iter()
        .filter(|w| w.starts_with("background compaction"))
        .collect()
}

#[test]
fn the_background_line_follows_flags_over_the_file() {
    let limit = "[models.\"claude-opus-5-5\"]\ncontext_window = 110000\n";
    assert_eq!(flagged_background_lines(limit, &[]).len(), 1);
    assert!(
        flagged_background_lines(
            limit,
            &["models.\"claude-opus-5-5\".background_compaction=false"]
        )
        .is_empty()
    );
    assert!(flagged_background_lines(limit, &["context.background_tokens=20000"]).is_empty());
    assert!(
        flagged_background_lines(limit, &["models.\"claude-opus-5-5\".context_window=300000"])
            .is_empty()
    );
    assert_eq!(
        flagged_background_lines("", &["models.\"claude-opus-5-5\".context_window=110000"]),
        [
            "background compaction rarely runs below about 60,000, and claude-opus-5-5 compacts at 46,000; compaction will usually wait at the point."
        ]
    );
}

#[test]
fn a_set_naming_no_known_endpoint_or_model_is_warned_about_once() {
    let flags = crate::settings::flag_layer(
        Vec::new(),
        &[
            "endpoints.typo.pricing=none".to_owned(),
            "endpoints.main.pricing=none".to_owned(),
            "models.\"made-up\".context_window=300000".to_owned(),
            "models.\"mine\".context_window=300000".to_owned(),
            "models.\"claude-opus-5-5\".context_window=300000".to_owned(),
            "models.\"claude-opus-5-5\".aliases=nick".to_owned(),
            "models.\"nick\".compact_at=80%".to_owned(),
        ],
    )
    .unwrap();
    let loaded = load_text(Some("[models.\"mine\"]\ncontext_window = 200000\n")).with_flags(&flags);
    assert_eq!(
        unknown_places(&loaded, &["main"], "claude-sonnet-5-5"),
        [
            "--set endpoints.typo.pricing names no account Wisp knows",
            "--set models.\"made-up\".context_window names no model Wisp knows",
        ]
    );
}

#[test]
fn a_price_set_in_part_says_which_keys_are_missing_and_where() {
    let notes = |text: &str, sets: &[&str]| {
        let sets: Vec<String> = sets.iter().map(|s| (*s).to_owned()).collect();
        let flags = crate::settings::flag_layer(Vec::new(), &sets).unwrap();
        let loaded = load_text(Some(text)).with_flags(&flags);
        let price = |w: &&String| w.contains("shows no cost");
        loaded
            .warnings
            .iter()
            .filter(price)
            .cloned()
            .collect::<Vec<_>>()
    };
    let lead = "my-model has price_input but not price_output or price_cache_read, so Wisp shows no cost for it";
    let one = "[models.\"my-model\"]\nprice_input = 1.2\n";
    assert_eq!(notes(one, &[]), [lead]);
    let both = "\n[endpoints.gw.models.\"my-model\"]\nprice_input = 1\nprice_output = 2\nprice_cache_read = 0.1\n";
    assert_eq!(
        notes(&format!("{one}{both}"), &[]),
        [format!("{lead}, except on gw")]
    );
    let two = "[endpoints.gw.models.\"my-model\"]\nprice_input = 1.2\nprice_output = 4.8\n";
    assert_eq!(
        notes(two, &[]),
        [
            "my-model on gw has price_input and price_output but not price_cache_read, so Wisp shows no cost for it there"
        ]
    );
    let env = "[endpoints.env.models.\"my-model\"]\nprice_cache_read = 0.1\n";
    assert_eq!(
        notes(env, &[]),
        [
            "my-model on the keys-file account has price_cache_read but not price_input or price_output, so Wisp shows no cost for it there"
        ]
    );
    assert_eq!(notes("", &["models.\"my-model\".price_input=1.2"]), [lead]);
    let set = [
        "models.\"my-model\".price_output=4.8",
        "models.\"my-model\".price_cache_read=0.12",
    ];
    let full = [&set[..], &["models.\"my-model\".price_input=1"]].concat();
    assert!(notes("", &full).is_empty());
    assert!(notes(one, &full).is_empty());
}

#[test]
fn account_facts_follow_your_tables() {
    let text = "[endpoints.gw.models.\"my-model\"]\nprice_input = 1.2\nprice_output = 4.8\nprice_cache_read = 0.12\nmax_output = 32000\n\n[endpoints.own.models.\"my-model\"]\nprice_input = 1\n";
    let sets = ["endpoints.direct.models.\"my-model\".price_input=2".to_owned()];
    let flags = crate::settings::flag_layer(Vec::new(), &sets).unwrap();
    let loaded = load_text(Some(text)).with_flags(&flags);
    let endpoints = [
        ("direct".to_owned(), Anthropic),
        ("gateway".to_owned(), Anthropic),
        ("gw".to_owned(), Anthropic),
        ("own".to_owned(), Anthropic),
    ];
    let facts = account_facts(&loaded.settings, &endpoints);
    let on = |account: &str, model: &str| {
        let found = facts
            .iter()
            .find(|f| f.account == account && f.model == model);
        found.cloned().unwrap()
    };
    let gw = on("gw", "my-model");
    let rates = gw.rates.unwrap();
    let rates = (
        rates.input_per_mtok,
        rates.output_per_mtok,
        rates.cache_read_per_mtok,
    );
    assert_eq!(rates, (1.2, 4.8, 0.12));
    assert_eq!(
        gw.rates.and_then(|r| r.writes).map(|w| w.one_hour),
        Some(2.0)
    );
    assert_eq!(gw.max_output, Some(32_000));
    assert_eq!(on("gateway", "my-model").rates, None);
    assert_eq!(on("own", "my-model").rates, None);
    assert_eq!(on("direct", "my-model").rates, None);
    let shipped = wisp_core::pricing::model_info("claude-opus-5-5").map(crate::models::rates_of);
    assert_eq!(on("gateway", "claude-opus-5-5").rates, shipped);
    assert_eq!(on("gateway", "claude-opus-5-5").max_output, None);
    let sets = ["endpoints.gw.models.\"my-model\".price_input=2".to_owned()];
    let flags = crate::settings::flag_layer(Vec::new(), &sets).unwrap();
    let loaded = load_text(Some(text)).with_flags(&flags);
    let facts = account_facts(&loaded.settings, &endpoints);
    let gw = facts
        .iter()
        .find(|f| f.account == "gw" && f.model == "my-model");
    assert_eq!(gw.and_then(|f| f.rates.as_ref()), None);
}

#[test]
fn unshipped_gemini_model_warns_at_start_and_resolves_antigravity_default_model() {
    use std::sync::Arc;
    use wisp_core::endpoints::{EndpointName, PathShown, PricingMode, ResolvedEndpoint, UrlSource};
    use wisp_core::provider::credential::{Credential, Source, SubscriptionKind};
    use wisp_core::provider::gemini::GeminiProvider;

    let ep = ResolvedEndpoint {
        name: EndpointName::parse("antigravity").unwrap(),
        provider: ProviderKind::Gemini,
        credential: Credential::Subscription {
            kind: SubscriptionKind::Antigravity,
            secret: "ya29.CANARY_TOK".into(),
            profile: None,
        },
        base_url: wisp_core::provider::gemini::antigravity_base_url().to_owned(),
        source: Source::Keyring,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let provider = Arc::new(GeminiProvider::from_endpoint(&ep).unwrap());
    let ready = [Connection {
        name: "antigravity".into(),
        provider,
        context: ep.log_context(),
        priced: true,
    }];
    let loaded = load_text(None);
    let unlisted_warnings = start_warnings(&loaded, "antigravity", "gemini-9-future-exp", &ready);
    assert_eq!(unlisted_warnings, [unknown_window("gemini-9-future-exp")]);
    let configured_loaded = load_text(Some(
        "[models.\"gemini-9-future-exp\"]\ncontext_window = 1048576\n",
    ));
    let configured_unlisted = start_warnings(
        &configured_loaded,
        "antigravity",
        "gemini-9-future-exp",
        &ready,
    );
    assert!(configured_unlisted.is_empty(), "{configured_unlisted:?}");
    let shipped_warnings = start_warnings(
        &loaded,
        "antigravity",
        wisp_core::provider::gemini::antigravity_default_model(),
        &ready,
    );
    assert!(shipped_warnings.is_empty(), "{shipped_warnings:?}");

    let cfg_text = "store_id = \"0123456789abcdef\"\ndefault = \"antigravity\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"keyring\"\n[endpoints.gemini-api]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"GEMINI_API_KEY\"\n";
    let cfg = ConfigFile::parse(Path::new("config.toml"), PathShown::Default, cfg_text).unwrap();
    let setup = crate::run::Setup {
        flags: Vec::new(),
        endpoint: None,
    };
    let resolved_ag = crate::run::Run::resolve_on(&setup, Some(&cfg), Some("antigravity"), None);
    assert_eq!(
        resolved_ag.model,
        wisp_core::provider::gemini::antigravity_default_model()
    );
    let resolved_api = crate::run::Run::resolve_on(&setup, Some(&cfg), Some("gemini-api"), None);
    assert_eq!(
        resolved_api.model,
        wisp_core::provider::gemini::default_model()
    );

    let dir = tempfile::tempdir().unwrap();
    let wisp_dir = dir.path().join("wisp");
    std::fs::create_dir_all(&wisp_dir).unwrap();
    let cfg_path = wisp_dir.join("config.toml");
    std::fs::write(
        &cfg_path,
        "store_id = \"0123456789abcdef\"\ndefault = \"antigravity\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\n",
    )
    .unwrap();
    let handle = "oauth:00000000000000000000000000000001";
    wisp_core::secrets::write_atomic(
        &wisp_dir.join("credentials.json"),
        format!(
            "{{\"version\":1,\"stores\":{{\"0123456789abcdef\":{{\"antigravity\":{{\"key\":\"{handle}\"}}}}}}}}"
        )
        .as_bytes(),
        0o600,
    )
    .unwrap();
    let profile = wisp_core::oauth::profile_path(&cfg_path, handle).unwrap();
    let oauth_dir = profile.parent().unwrap();
    std::fs::create_dir_all(oauth_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(oauth_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    wisp_core::secrets::write_atomic(
        &profile,
        br#"{"version":1,"kind":"antigravity","access_token":"ya29.CANARY_AG","refresh_token":"1//CANARY_REF","expires_at":4102444800,"project_id":"proj-ag-resume"}"#,
        0o600,
    )
    .unwrap();
    let resume_ep = ResolvedEndpoint {
        name: EndpointName::parse("antigravity").unwrap(),
        provider: ProviderKind::Gemini,
        credential: Credential::Subscription {
            kind: SubscriptionKind::Antigravity,
            secret: "ya29.CANARY_AG".into(),
            profile: Some(profile.clone()),
        },
        base_url: wisp_core::provider::gemini::antigravity_base_url().to_owned(),
        source: Source::File(profile),
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let models_dir = wisp_dir.join("models");
    let key =
        wisp_core::models::list::key(&resume_ep.base_url, &resume_ep.credential, "antigravity");
    let saved: wisp_core::models::list::SavedList = serde_json::from_value(serde_json::json!({
        "attempted": 1_800_000_000,
        "default_model": "gemini-4-carbon-high",
        "list": [
            {
                "id": "gemini-3.1-flash-image",
                "shown": true,
                "window": 65_536,
                "levels": [],
                "images": false,
                "search": false
            },
            {
                "id": "gemini-4-carbon-high",
                "shown": true,
                "window": 1_048_576,
                "levels": [],
                "adaptive": true,
                "images": false,
                "search": false
            }
        ]
    }))
    .unwrap();
    saved.save(&models_dir, key.as_deref().unwrap()).unwrap();
    let resume_provider = Arc::new(
        GeminiProvider::from_endpoint(&resume_ep)
            .unwrap()
            .with_saved_list(Some(&models_dir)),
    );
    let resume_ready = [Connection {
        name: "antigravity".into(),
        provider: resume_provider,
        context: resume_ep.log_context(),
        priced: true,
    }];
    let keychain = crate::keychain::Keychain::load(
        Some((
            wisp_core::secrets::detect::KeychainSetting::Off,
            "test".to_owned(),
        )),
        None,
        crate::keychain::ConfigSetting::Read(None),
        false,
        Box::new(|| {}),
    )
    .unwrap();
    let flags = crate::endpoints::Flags {
        env_file: None,
        config: Some(&cfg_path),
        endpoint: None,
        keychain: &keychain,
        set: &[],
    };
    let sources = crate::endpoints::Sources::load(
        &flags,
        Some(dir.path().to_path_buf()),
        Some(dir.path().to_path_buf()),
    );
    let facts_models = crate::models::models(&resume_ready);
    assert_eq!(
        crate::resume::model_on(
            &sources,
            "antigravity",
            "claude-sonnet-5-5",
            &facts_models,
            &resume_ready,
        ),
        Some("gemini-4-carbon-high".to_owned())
    );
}

#[tokio::test]
async fn start_warnings_and_model_switch_share_one_listed_only_warning() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use wisp_core::agent::Agent;
    use wisp_core::endpoints::{
        Availability, EndpointName, PricingMode, ResolvedEndpoint, UrlSource,
    };
    use wisp_core::provider::credential::{Credential, Source, SubscriptionKind};
    use wisp_core::provider::gemini::GeminiProvider;
    use wisp_core::settings::host::StartLayers;
    use wisp_protocol::{AgentEvent, Effort, MessageTime, SessionConfig};

    let dir = tempfile::tempdir().unwrap();
    let ep = ResolvedEndpoint {
        name: EndpointName::parse("antigravity").unwrap(),
        provider: ProviderKind::Gemini,
        credential: Credential::Subscription {
            kind: SubscriptionKind::Antigravity,
            secret: "ya29.CANARY_LISTED".into(),
            profile: Some(dir.path().join("oauth-listed.json")),
        },
        base_url: wisp_core::provider::gemini::antigravity_base_url().to_owned(),
        source: Source::Keyring,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let key = wisp_core::models::list::key(&ep.base_url, &ep.credential, "antigravity").unwrap();
    let saved: wisp_core::models::list::SavedList = serde_json::from_value(serde_json::json!({
        "attempted": 1_800_000_000,
        "default_model": "gemini-4-carbon-medium",
        "list": [
            {
                "id": "gemini-4-carbon-medium",
                "shown": true,
                "window": 1_048_576,
                "levels": [],
                "adaptive": true,
                "images": true,
                "search": false
            },
            {
                "id": "gemini-9-future-exp",
                "shown": true,
                "window": 1_048_576,
                "levels": [],
                "images": false,
                "search": false
            },
            {
                "id": "gemini-10-priced-exp",
                "shown": true,
                "window": 1_048_576,
                "levels": [],
                "images": false,
                "search": false
            }
        ]
    }))
    .unwrap();
    saved.save(dir.path(), &key).unwrap();

    let provider: Arc<dyn wisp_core::provider::Provider> = Arc::new(
        GeminiProvider::from_endpoint(&ep)
            .unwrap()
            .with_saved_list(Some(dir.path())),
    );
    let ready = [Connection {
        name: "antigravity".into(),
        provider: provider.clone(),
        context: ep.log_context(),
        priced: true,
    }];
    let loaded = load_text(Some(
        "[models.\"gemini-10-priced-exp\"]\ncontext_window = 1048576\n",
    ));
    let entries = BTreeMap::from([(ep.name.clone(), Availability::Ready(ep))]);
    let summaries =
        crate::endpoints::summaries(&entries, &BTreeMap::new(), &[], Some("antigravity"), &ready);
    assert!(summaries[0].complete);
    assert_eq!(
        summaries[0].default_model.as_deref(),
        Some("gemini-4-carbon-medium")
    );
    let session = SessionConfig {
        model: "gemini-4-carbon-medium".into(),
        system: None,
        effort: Effort::Medium,
        cache_ttl: CacheTtl::OneHour,
        max_tokens: 8_192,
        message_time: MessageTime::Off,
        instructions: None,
        context_note: false,
    };
    let mut agent = Agent::new(provider, session)
        .with_connections(ready.to_vec(), Vec::new(), "antigravity")
        .with_model_settings(loaded.settings.clone())
        .with_start_layers(StartLayers {
            endpoints: summaries,
            saved: loaded.saved.clone(),
            ..StartLayers::default()
        });

    let mut switch_warnings = async |model: &str| -> Vec<String> {
        let (ev_tx, mut ev_rx) = mpsc::channel(16);
        agent
            .change_settings(None, Some(model.into()), None, &ev_tx)
            .await;
        drop(ev_tx);
        let mut out = Vec::new();
        while let Some(ev) = ev_rx.recv().await {
            if let AgentEvent::SettingsChanged { changes, .. } = ev {
                out.extend(changes.into_iter().flat_map(|c| c.warnings));
            }
        }
        out
    };

    let unconfigured_expected = "No data for gemini-9-future-exp yet: thinking, effort and images are off; add [models.\"gemini-9-future-exp\"] in config.toml to set price or context window.";
    assert_eq!(
        start_warnings(&loaded, "antigravity", "gemini-9-future-exp", &ready),
        [unconfigured_expected]
    );
    assert_eq!(
        switch_warnings("gemini-9-future-exp").await,
        [unconfigured_expected]
    );

    let configured_expected =
        "No capability data for gemini-10-priced-exp yet: thinking, effort and images are off.";
    assert_eq!(
        start_warnings(&loaded, "antigravity", "gemini-10-priced-exp", &ready),
        [configured_expected]
    );
    assert_eq!(
        switch_warnings("gemini-10-priced-exp").await,
        [configured_expected]
    );

    assert!(start_warnings(&loaded, "antigravity", "gemini-4-carbon-medium", &ready).is_empty());
    assert!(switch_warnings("gemini-4-carbon-medium").await.is_empty());

    assert_eq!(
        start_warnings(&loaded, "antigravity", "gemini-9-unlisted", &ready),
        [unknown_window("gemini-9-unlisted")]
    );
    let unlisted_switch = switch_warnings("gemini-9-unlisted").await;
    assert!(
        unlisted_switch
            .iter()
            .all(|w| !w.contains("thinking, effort and images are off")),
        "{unlisted_switch:?}"
    );
}
