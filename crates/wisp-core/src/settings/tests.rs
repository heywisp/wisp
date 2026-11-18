use std::{cell::Cell, path::Path};

use wisp_protocol::settings::{find, parse_target};

use super::*;
use crate::context::{CompactAt, ModelSettings};
use crate::endpoints::{ConfigFile, PathShown, ProviderKind};
use crate::models::ConfigModels;
use crate::search::SearchFacts;

thread_local! {
    pub(super) static SAVED_ALIAS_CALLS: Cell<usize> = const { Cell::new(0) };
}
fn file(text: &str) -> ConfigFile {
    ConfigFile::parse(Path::new("/h/config.toml"), PathShown::Default, text).unwrap()
}

fn flag(target: &str, value: &str, origin: &str) -> Given {
    let target = parse_target(target).unwrap();
    let value = target.spec().parse(value).unwrap();
    Given {
        target,
        value,
        origin: origin.to_owned(),
    }
}

fn current(endpoint: Option<&str>, model: &str) -> Current {
    Current {
        endpoint: endpoint.map(str::to_owned),
        model: model.to_owned(),
    }
}

fn layers_of(found: &Resolution) -> Vec<Layer> {
    found.found.iter().map(|f| f.layer).collect()
}

#[test]
fn every_layer_wins_over_the_next() {
    let config = file(
        "effort = \"low\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n[endpoints.gw.models.\"m\"]\ncompact_at = \"60%\"\n[models.\"m\"]\ncompact_at = 700000\n",
    );
    let layers = Layers {
        session: vec![flag("effort", "max", "/effort")],
        flags: vec![flag("effort", "high", "--effort")],
        env: vec![],
        saved: Some(&config.settings),
        shipped: None,
    };
    let here = current(Some("gw"), "m");
    let effort = resolve_one(find("effort").unwrap(), &layers, &here);
    assert_eq!(
        layers_of(&effort),
        [Layer::Session, Layer::Flag, Layer::Global, Layer::Default]
    );
    assert_eq!(effort.value().unwrap().text(), "max");
    assert_eq!(effort.shadowed().len(), 3);
    let point = resolve_one(find("compact_at").unwrap(), &layers, &here);
    assert_eq!(layers_of(&point), [Layer::EndpointModel, Layer::Model]);
    assert_eq!(point.value().unwrap().text(), "60%");
    assert_eq!(point.winner().unwrap().line, Some(6));
    let elsewhere = resolve_one(
        find("compact_at").unwrap(),
        &layers,
        &current(Some("other"), "m"),
    );
    assert_eq!(layers_of(&elsewhere), [Layer::Model]);
}

#[test]
fn a_path_flag_applies_to_its_site_only() {
    let layers = Layers {
        flags: vec![flag(
            "models.\"m\".compact_at",
            "50%",
            "--set models.\"m\".compact_at",
        )],
        ..Layers::default()
    };
    let spec = find("compact_at").unwrap();
    assert_eq!(
        resolve_one(spec, &layers, &current(None, "m"))
            .value()
            .map(SettingValue::text),
        Some("50%".to_owned())
    );
    assert_eq!(
        resolve_one(spec, &layers, &current(None, "other")).value(),
        None
    );
}

#[test]
fn shipped_facts_sit_below_saved_values() {
    let table = ModelTable::shipped().unwrap();
    let config = file("[models.\"claude-opus-5-5\"]\ncontext_window = 500000\n");
    let layers = Layers {
        saved: Some(&config.settings),
        shipped: Some(&table),
        ..Layers::default()
    };
    let window = resolve_one(
        find("context_window").unwrap(),
        &layers,
        &current(None, "claude-opus-5-5"),
    );
    assert_eq!(layers_of(&window), [Layer::Model, Layer::Shipped]);
    assert_eq!(window.value(), Some(&SettingValue::Tokens(500_000)));
}

#[test]
fn with_no_file_and_no_flags_the_defaults_are_todays_values() {
    let resolved = resolve(&Layers::default(), &current(None, "claude-opus-5-5"));
    let text = |name: &str| resolved[name].value().map(SettingValue::text);
    assert_eq!(
        text("model").as_deref(),
        Some(wisp_protocol::settings::DEFAULT_MODEL)
    );
    assert_eq!(text("effort").as_deref(), Some("medium"));
    assert_eq!(text("cache_ttl").as_deref(), Some("1h"));
    assert_eq!(text("time").as_deref(), Some("local"));
    assert_eq!(text("debug_cache").as_deref(), Some("off"));
    assert_eq!(text("refusal_fallback").as_deref(), Some("on"));
    assert_eq!(text("retry.budget_usd").as_deref(), Some("none"));
    assert_eq!(text("log").as_deref(), Some("on"));
    assert_eq!(text("instructions.source").as_deref(), Some("auto"));
    assert_eq!(text("instructions.warn_tokens").as_deref(), Some("8000"));
    assert_eq!(text("tools"), None);
    assert!(!resolved.contains_key("agent"));
    assert_eq!(resolved["effort"].source(), "default");
}

#[test]
fn the_context_note_flag_beats_the_context_table_which_beats_the_default() {
    let note = find("context.context_note").unwrap();
    let here = current(None, "claude-opus-5-5");
    let none = resolve_one(note, &Layers::default(), &here);
    assert_eq!(none.value(), Some(&SettingValue::Bool(true)));
    assert_eq!(layers_of(&none), [Layer::Default]);
    let config = file("[context]\ncontext_note = false\n");
    let saved = Layers {
        saved: Some(&config.settings),
        ..Layers::default()
    };
    let off = resolve_one(note, &saved, &here);
    assert_eq!(off.value(), Some(&SettingValue::Bool(false)));
    assert_eq!(layers_of(&off), [Layer::Global, Layer::Default]);
    let flagged = Layers {
        flags: vec![flag("context.context_note", "on", "--context-note")],
        saved: Some(&config.settings),
        ..Layers::default()
    };
    let on = resolve_one(note, &flagged, &here);
    assert_eq!(on.value(), Some(&SettingValue::Bool(true)));
    assert_eq!(layers_of(&on), [Layer::Flag, Layer::Global, Layer::Default]);
}

#[test]
fn the_summary_effort_is_high_unless_the_context_table_sets_it() {
    let effort = find("context.compact_effort").unwrap();
    let here = current(None, "claude-opus-5-5");
    let none = resolve_one(effort, &Layers::default(), &here);
    assert_eq!(none.value(), Some(&SettingValue::Text("high".into())));
    let config = file("[context]\ncompact_effort = \"low\"\n");
    let saved = Layers {
        saved: Some(&config.settings),
        ..Layers::default()
    };
    let low = resolve_one(effort, &saved, &here);
    assert_eq!(low.value(), Some(&SettingValue::Text("low".into())));
    assert_eq!(layers_of(&low), [Layer::Global, Layer::Default]);
    let refused = ConfigFile::parse(
        Path::new("/h/config.toml"),
        PathShown::Default,
        "[context]\ncompact_effort = \"turbo\"\n",
    );
    assert!(refused.is_err());
}

#[test]
fn the_most_specific_flag_wins_whatever_order_the_flags_came_in() {
    let spec = find("context_window").unwrap();
    let model = flag(
        "models.\"claude-opus-5-5\".context_window",
        "300000",
        "--set m",
    );
    let pair = flag(
        "endpoints.gw.models.\"claude-opus-5-5\".context_window",
        "200000",
        "--set em",
    );
    for flags in [
        vec![model.clone(), pair.clone()],
        vec![pair.clone(), model.clone()],
    ] {
        let layers = Layers {
            flags,
            ..Layers::default()
        };
        let at_gw = resolve_one(spec, &layers, &current(Some("gw"), "claude-opus-5-5"));
        assert_eq!(at_gw.winner().unwrap().origin, "--set em");
        assert_eq!(at_gw.source(), "flag");
        let elsewhere = resolve_one(spec, &layers, &current(Some("env"), "claude-opus-5-5"));
        assert_eq!(elsewhere.winner().unwrap().origin, "--set m");
        let other = resolve_one(spec, &layers, &current(Some("gw"), "claude-haiku-5-5"));
        assert!(other.winner().is_none_or(|f| f.layer != Layer::Flag));
    }
}

#[test]
fn a_flag_under_an_alias_applies_to_the_model_it_names() {
    let spec = find("context_window").unwrap();
    let opus = "claude-opus-4-5";
    let dated = "claude-opus-4-5-20251101";
    for (given, current_model) in [(dated, opus), (opus, dated)] {
        let layers = Layers {
            flags: vec![flag(
                &format!("models.\"{given}\".context_window"),
                "300000",
                "--set alias",
            )],
            ..Layers::default()
        };
        let found = resolve_one(spec, &layers, &current(Some("env"), current_model));
        assert_eq!(found.source(), "flag", "{given} at {current_model}");
    }
    let layers = Layers {
        flags: vec![
            flag("models.\"my-model\".aliases", "my-alias", "--set aliases"),
            flag(
                "endpoints.gw.models.\"my-model\".context_window",
                "300000",
                "--set window",
            ),
        ],
        ..Layers::default()
    };
    let found = resolve_one(spec, &layers, &current(Some("gw"), "my-alias"));
    assert_eq!(found.winner().unwrap().origin, "--set window");
    let other = resolve_one(spec, &layers, &current(Some("gw"), "claude-haiku-5-5"));
    assert!(other.winner().is_none_or(|f| f.origin != "--set window"));
}

fn kernel_point(models: &ModelSettings, endpoint: &str, model: &str) -> Option<CompactAt> {
    models
        .resolve(endpoint, model, None, 64_000)
        .compact_at
        .map(|(at, _)| at)
}

fn layered_point(layers: &Layers<'_>, endpoint: &str, model: &str) -> Option<CompactAt> {
    let spec = find("compact_at").unwrap();
    let found = resolve_one(spec, layers, &current(Some(endpoint), model));
    found.value().and_then(CompactAt::of)
}

#[test]
fn a_top_level_compact_at_is_read_the_same_by_resolve_one_and_model_settings() {
    let text = "compact_at = 300000\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n[endpoints.gw.models.\"claude-opus-5-5\"]\ncompact_at = \"60%\"\n[models.\"claude-sonnet-5-5\"]\ncompact_at = 700000\n";
    let config = file(text);
    let layers = Layers {
        saved: Some(&config.settings),
        ..Layers::default()
    };
    let models = ModelSettings::new(
        ModelTable::shipped().unwrap(),
        ConfigModels::parse(text).unwrap(),
    );
    let pairs = [
        ("gw", "claude-opus-5-5"),
        ("env", "claude-opus-5-5"),
        ("gw", "claude-sonnet-5-5"),
        ("env", "my-model"),
    ];
    for (endpoint, model) in pairs {
        let at = layered_point(&layers, endpoint, model);
        assert_eq!(
            at,
            kernel_point(&models, endpoint, model),
            "{endpoint} {model}"
        );
    }
    assert_eq!(
        layered_point(&layers, "env", "my-model"),
        Some(CompactAt::Tokens(300_000))
    );
    for name in ["compact_at", "context_window"] {
        let spec = find(name).unwrap();
        assert!(spec.env.is_empty(), "{name}");
        assert!(spec.default_value().is_none(), "{name}");
    }
}

#[test]
fn a_flag_and_the_saved_row_meet_through_an_alias_saved_in_the_file() {
    let config = file(concat!(
        "[models.\"my-model\"]\naliases = [\"my-alias\"]\ncontext_window = 500000\n",
        "[models.\"other-model\"]\ncontext_window = 200000\ncompact_at = 150000\n",
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n",
        "[endpoints.gw.models.\"my-model\"]\nmax_output = 16000\ncompact_at = 300000\n",
        "[endpoints.gw.models.\"endpoint-only\"]\ncontext_window = 200000\ncompact_at = 160000\n",
    ));
    let spec = find("context_window").unwrap();
    for given in ["my-model", "my-alias"] {
        let layers = Layers {
            flags: vec![flag(
                &format!("models.\"{given}\".context_window"),
                "300000",
                "--set",
            )],
            saved: Some(&config.settings),
            ..Layers::default()
        };
        let found = resolve_one(spec, &layers, &current(Some("gw"), "my-alias"));
        assert_eq!(layers_of(&found), [Layer::Flag, Layer::Model], "{given}");
        let found = resolve_one(spec, &layers, &current(Some("gw"), "my-model"));
        assert_eq!(found.source(), "flag", "{given}");
    }
    for (model, expected, judged) in [
        ("my-alias", vec!["my-alias", "my-model"], 3),
        ("my-model", vec!["my-model"], 2),
        ("missing", vec!["missing"], 3),
    ] {
        SAVED_ALIAS_CALLS.set(0);
        assert_eq!(model_names(&config.settings, &[], model), expected);
        assert_eq!(SAVED_ALIAS_CALLS.get(), judged * 2, "{model}");
    }
}

#[test]
fn the_snapshot_rows_leave_out_the_plugin_switch_paths() {
    let endpoint = EndpointRow {
        value: "env".to_owned(),
        layer: wisp_protocol::settings::Layer::Default,
        origin: String::new(),
        keys_file: None,
        effort: wisp_protocol::Effort::Medium,
        credential: None,
        wire: None,
    };
    let rows = snapshot_rows(
        &Layers::default(),
        &current(None, "claude-opus-5-5"),
        true,
        &endpoint,
        &|_| Vec::new(),
    );
    let names: Vec<&str> = rows.iter().map(|r| r.name.spec().name).collect();
    assert!(names.contains(&"display.sidebar"), "{names:?}");
    for path in [
        wisp_protocol::settings::PLUGIN_SLOT,
        wisp_protocol::settings::PLUGIN_HOOK,
    ] {
        assert!(!names.contains(&path), "{path} in {names:?}");
    }
}

#[test]
fn the_request_and_the_settings_resolver_pick_the_same_search_list() {
    const HAIKU: &str = "claude-haiku-5-5";
    let account = format!("endpoints.gw.models.\"{HAIKU}\"");
    let model = format!("models.\"{HAIKU}\"");
    let saved = |at: &str, list: &str| format!("[{at}]\nweb_search = {list}\n");
    let flagged = |at: &str, list: &str| vec![flag(&format!("{at}.web_search"), list, "--set")];
    let one = "[\"web_search\"]";
    let both = "[\"web_search\", \"web_search_20250305\"]";
    let cases = [
        (HAIKU, String::new(), flagged(&account, "web_search")),
        (HAIKU, String::new(), flagged(&model, "web_search")),
        (HAIKU, saved(&account, one), vec![]),
        (HAIKU, saved(&model, one), vec![]),
        (HAIKU, saved(&account, one) + &saved(&model, "[]"), vec![]),
        (
            HAIKU,
            saved(&account, "[\"web_search_20250305\"]"),
            flagged(&model, "none"),
        ),
        (HAIKU, saved(&model, both), vec![]),
        (HAIKU, String::new(), vec![]),
        ("my-new-model", String::new(), vec![]),
        (
            "my-new-model",
            saved("models.\"my-new-model\"", one),
            vec![],
        ),
    ];
    let spec = find("web_search").unwrap();
    let shipped = ModelTable::shipped().unwrap();
    for (case, (model, text, flags)) in cases.into_iter().enumerate() {
        let text = format!("[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n{text}");
        let config = file(&text);
        let layers = Layers {
            flags: flags.clone(),
            saved: Some(&config.settings),
            shipped: Some(&shipped),
            ..Layers::default()
        };
        let models = crate::test_guard::models(&text).with_flags(&flags);
        for wire in ProviderKind::ALL {
            let facts = SearchFacts::resolve(&models, "gw", model, wire, None);
            let kernel = (facts.versions_from != Layer::Default)
                .then_some((SettingValue::List(facts.versions), facts.versions_from));
            let resolved = resolve_one(spec, &layers, &current(Some("gw"), model));
            let settings = resolved
                .on_wire(Some(wire))
                .winner()
                .map(|f| (f.value.clone(), f.layer));
            assert_eq!(kernel, settings, "case {case} {model} {wire:?}");
        }
    }
}
