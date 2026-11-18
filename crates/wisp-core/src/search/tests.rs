use super::*;
use crate::endpoints::ProviderKind::{self, Anthropic, OpenAi};
use crate::test_guard::models;

fn form(config: &str, endpoint: &str, model: &str, wire: ProviderKind) -> SearchForm {
    SearchFacts::resolve(&models(config), endpoint, model, wire, None).form(true, false)
}

#[test]
fn the_providers_own_origin_uses_the_shipped_fact() {
    let facts = SearchFacts::resolve(&models(""), "direct", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(facts.versions, vec!["web_search_20250305".to_owned()]);
    assert_eq!(facts.versions_from, Layer::Shipped);
    assert_eq!(facts.form(true, false), SearchForm::Native);
    assert_eq!(
        facts.server_tools(facts.form(true, false), false),
        &[ServerToolSpec {
            name: "web_search".into(),
            kind: "web_search_20250305".into(),
        }]
    );
}

#[test]
fn another_origin_sends_search_by_default_and_can_turn_it_off() {
    assert_eq!(
        form("", "gw", "claude-haiku-5-5", Anthropic),
        SearchForm::Native
    );
    assert_eq!(
        form("", "env", "claude-haiku-5-5", Anthropic),
        SearchForm::Native
    );
    let off = "[endpoints.gw]\nnative_web_search = false\n";
    assert_eq!(
        form(off, "gw", "claude-haiku-5-5", Anthropic),
        SearchForm::None(NoSearch::TurnedOff)
    );
    assert_eq!(
        form(off, "other", "claude-haiku-5-5", Anthropic),
        SearchForm::Native
    );
    let facts = SearchFacts::resolve(&models(off), "gw", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(facts.opt_in_from().source(), "account");
    let env_off = "[endpoints.env]\nnative_web_search = false\n";
    assert_eq!(
        form(env_off, "env", "claude-haiku-5-5", Anthropic),
        SearchForm::None(NoSearch::TurnedOff)
    );
    let own =
        "[endpoints.gw.models.\"claude-haiku-5-5\"]\nweb_search = [\"web_search_20250305\"]\n";
    let facts = SearchFacts::resolve(&models(own), "gw", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(facts.versions_from, Layer::EndpointModel);
    assert_eq!(facts.form(true, false), SearchForm::Native);
}

#[test]
fn turning_it_off_works_on_the_providers_own_origin_too() {
    assert_eq!(
        form("", "direct", "claude-haiku-5-5", Anthropic),
        SearchForm::Native
    );
    assert_eq!(
        form(
            "[endpoints.direct]\nnative_web_search = false\n",
            "direct",
            "claude-haiku-5-5",
            Anthropic
        ),
        SearchForm::None(NoSearch::TurnedOff)
    );
}

#[test]
fn a_model_without_search_stays_without_it_whatever_the_setting() {
    let config = "[endpoints.gw]\nnative_web_search = true\n\n[models.\"claude-haiku-5-5\"]\nweb_search = []\n";
    assert_eq!(
        form(config, "gw", "claude-haiku-5-5", Anthropic),
        SearchForm::None(NoSearch::NotSupported)
    );
}

#[test]
fn the_logged_form_beats_the_setting_both_ways() {
    let native = SearchForm::Native;
    let off = SearchForm::None(NoSearch::TurnedOff);
    assert_eq!(native.kept(None), native);
    assert_eq!(native.kept(Some(true)), native);
    assert_eq!(
        native.kept(Some(false)),
        SearchForm::None(NoSearch::NotStarted)
    );
    assert_eq!(off.kept(Some(true)), native);
    assert_eq!(off.kept(Some(false)), off);
    let refused = SearchForm::None(NoSearch::Refused);
    assert_eq!(refused.kept(Some(true)), refused);
    let unsupported = SearchForm::None(NoSearch::NotSupported);
    assert_eq!(unsupported.kept(Some(true)), unsupported);
    assert!(had_search(&["bash".to_owned(), "web_search".to_owned()]));
    assert!(!had_search(&["bash".to_owned()]));
}

#[test]
fn the_override_order_is_endpoint_then_config_then_shipped() {
    let config = "[models.\"claude-haiku-5-5\"]\nweb_search = []\n\n[endpoints.direct.models.\"claude-haiku-5-5\"]\nweb_search = [\"web_search_20250305\"]\n";
    let direct = SearchFacts::resolve(
        &models(config),
        "direct",
        "claude-haiku-5-5",
        Anthropic,
        None,
    );
    assert_eq!(direct.form(true, false), SearchForm::Native);
    assert_eq!(direct.versions_from, Layer::EndpointModel);
    let other = SearchFacts::resolve(
        &models(config),
        "other",
        "claude-haiku-5-5",
        Anthropic,
        None,
    );
    assert_eq!(other.versions_from, Layer::Model);
    assert_eq!(
        other.form(true, false),
        SearchForm::None(NoSearch::NotSupported)
    );
    let stray = "[models.\"claude-haiku-5-5\"]\nweb_search = [\"web_search\"]\n";
    let haiku =
        |wire| SearchFacts::resolve(&models(stray), "direct", "claude-haiku-5-5", wire, None);
    let claude = haiku(Anthropic);
    assert_eq!(claude.versions_from, Layer::Shipped);
    assert_eq!(claude.form(true, false), SearchForm::Native);
    assert_eq!(
        claude.server_tools(SearchForm::Native, false),
        &[ServerToolSpec {
            name: "web_search".into(),
            kind: "web_search_20250305".into(),
        }]
    );
    assert_eq!(haiku(OpenAi).versions_from, Layer::Model);
    let gpt = SearchFacts::resolve(&models(""), "direct", "gpt-6.1-sol", Anthropic, None);
    assert_eq!(gpt.versions_from, Layer::Shipped);
    assert_eq!(
        gpt.form(true, false),
        SearchForm::None(NoSearch::NotSupported)
    );
}

#[test]
fn a_model_without_data_has_no_search() {
    assert_eq!(
        form("", "direct", "my-new-model", Anthropic),
        SearchForm::None(NoSearch::NotSupported)
    );
    assert_eq!(
        form(
            "[models.\"my-new-model\"]\nweb_search = [\"web_search_20250305\"]\n",
            "direct",
            "my-new-model",
            Anthropic
        ),
        SearchForm::Native
    );
}

#[test]
fn leaving_it_out_or_a_refusal_wins_over_the_data() {
    let facts = SearchFacts::resolve(&models(""), "direct", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(
        facts.form(false, false),
        SearchForm::None(NoSearch::NotRequested)
    );
    assert_eq!(facts.form(true, true), SearchForm::None(NoSearch::Refused));
    assert!(facts.server_tools(facts.form(true, true), false).is_empty());
}

#[test]
fn the_opt_in_names_where_it_came_from() {
    let on = SearchFacts::resolve(
        &models("[endpoints.gw]\nnative_web_search = true\n"),
        "gw",
        "claude-haiku-5-5",
        Anthropic,
        None,
    );
    assert_eq!(on.opt_in_from().source(), "account");
    assert_eq!(on.versions_from.source(), "shipped");
    let unset = SearchFacts::resolve(&models(""), "gw", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(unset.opt_in_from().source(), "default");
    assert_eq!(unset.versions_from.source(), "shipped");
}

#[test]
fn each_wire_sends_its_own_listed_type() {
    let both =
        "[models.\"my-new-model\"]\nweb_search = [\"web_search\", \"web_search_20250305\"]\n";
    let sent = |config: &str, model: &str, wire: ProviderKind| -> Vec<&'static str> {
        let facts = SearchFacts::resolve(&models(config), "direct", model, wire, None);
        let tools = facts.server_tools(facts.form(true, false), false);
        tools.iter().map(|spec| spec.kind.as_str()).collect()
    };
    assert_eq!(
        sent("", "claude-haiku-5-5", Anthropic),
        ["web_search_20250305"]
    );
    assert_eq!(sent("", "gpt-6.1-sol", OpenAi), ["web_search"]);
    assert_eq!(
        sent(both, "my-new-model", Anthropic),
        ["web_search_20250305"]
    );
    assert_eq!(sent(both, "my-new-model", OpenAi), ["web_search"]);
    let owner =
        "[models.\"claude-haiku-5-5\"]\nweb_search = [\"web_search\", \"web_search_20250305\"]\n";
    assert_eq!(
        sent(owner, "claude-haiku-5-5", Anthropic),
        ["web_search_20250305"]
    );
    let facts = SearchFacts::resolve(
        &models(owner),
        "direct",
        "claude-haiku-5-5",
        Anthropic,
        None,
    );
    assert_eq!(facts.versions_from, Layer::Model);
    let unsupported = SearchForm::None(NoSearch::NotSupported);
    assert_eq!(form("", "gw", "gpt-6.1-sol", Anthropic), unsupported);
    assert_eq!(form("", "gw", "claude-haiku-5-5", OpenAi), unsupported);
}

fn place_flag(target: &str, value: &str) -> crate::settings::Given {
    let target = wisp_protocol::settings::parse_target_among(
        target,
        wisp_protocol::settings::Endpoints::Any,
    )
    .unwrap();
    let value = target.spec().parse(value).unwrap();
    crate::settings::Given {
        target,
        value,
        origin: "--set".to_owned(),
    }
}

#[test]
fn flags_give_search_facts_for_one_run_and_say_so() {
    let trusted = models("[endpoints.gw]\nnative_web_search = true\n");
    let off = trusted
        .clone()
        .with_flags(&[place_flag("endpoints.gw.native_web_search", "off")]);
    let facts = SearchFacts::resolve(&off, "gw", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(
        facts.form(true, false),
        SearchForm::None(NoSearch::TurnedOff)
    );
    assert_eq!(facts.opt_in_from(), Layer::Flag);
    let on = models("[endpoints.gw]\nnative_web_search = false\n")
        .with_flags(&[place_flag("endpoints.gw.native_web_search", "on")]);
    let facts = SearchFacts::resolve(&on, "gw", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(facts.form(true, false), SearchForm::Native);
    assert_eq!(facts.opt_in_from(), Layer::Flag);
    assert_eq!(
        SearchFacts::resolve(&trusted, "gw", "claude-haiku-5-5", Anthropic, None).opt_in_from(),
        Layer::Endpoint
    );
    let none =
        models("").with_flags(&[place_flag("models.\"claude-haiku-5-5\".web_search", "none")]);
    let facts = SearchFacts::resolve(&none, "direct", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(facts.versions_from, Layer::Flag);
    assert_eq!(
        facts.form(true, false),
        SearchForm::None(NoSearch::NotSupported)
    );
    let at_gw = models("").with_flags(&[place_flag(
        "endpoints.gw.models.\"claude-haiku-5-5\".web_search",
        "web_search_20250305",
    )]);
    let facts = SearchFacts::resolve(&at_gw, "gw", "claude-haiku-5-5", Anthropic, None);
    assert_eq!(facts.versions_from, Layer::Flag);
    assert_eq!(facts.form(true, false), SearchForm::Native);
}

#[test]
fn gemini_wire_searches_by_default_even_with_a_saved_list_and_accepts_an_override() {
    let gemini = ProviderKind::Gemini;
    let shipped = SearchFacts::resolve(&models(""), "direct", "gemini-4-carbon", gemini, None);
    assert_eq!(shipped.form(true, false), SearchForm::Native);
    let list = crate::models::list::ModelList::from_gemini_ids(
        [("gemini-4-carbon".to_owned(), None, None)],
        None,
    )
    .unwrap();
    let row = ListedRow {
        row: list.find("gemini-4-carbon").unwrap().row(gemini),
        source: wisp_protocol::context::WindowSource::Subscription,
        max_window: None,
    };
    let saved = SearchFacts::resolve(&models(""), "direct", "gemini-4-carbon", gemini, Some(&row));
    assert_eq!(saved.form(true, false), SearchForm::Native);
    let configured = models("[models.\"gemini-4-carbon\"]\nweb_search = [\"google_search\"]\n");
    let facts = SearchFacts::resolve(&configured, "direct", "gemini-4-carbon", gemini, None);
    assert_eq!(facts.versions, vec!["google_search".to_owned()]);
    assert_eq!(facts.versions_from, Layer::Model);
    assert_eq!(facts.form(true, false), SearchForm::Native);
    assert_eq!(
        facts.server_tools(SearchForm::Native, false),
        &[ServerToolSpec {
            name: "web_search".into(),
            kind: "google_search".into(),
        }]
    );
    assert_eq!(
        SearchFacts::resolve(&configured, "gw", "gemini-4-carbon", Anthropic, None)
            .form(true, false),
        SearchForm::None(NoSearch::NotSupported)
    );
    assert_eq!(
        SearchFacts::resolve(&configured, "gw", "gemini-4-carbon", OpenAi, None).form(true, false),
        SearchForm::None(NoSearch::NotSupported)
    );
    assert_eq!(
        form("", "gw", "claude-haiku-5-5", gemini),
        SearchForm::None(NoSearch::NotSupported)
    );
    assert_eq!(
        form("", "gw", "gpt-6.1-sol", gemini),
        SearchForm::None(NoSearch::NotSupported)
    );
}

#[test]
fn listed_version_names_each_providers_native_search_tool() {
    for kind in ProviderKind::ALL {
        let version = listed_version(kind);
        assert!(speaks(kind, version), "{kind:?}: {version}");
        for other in ProviderKind::ALL {
            assert_eq!(speaks(other, version), other == kind);
        }
    }
}
