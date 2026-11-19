use serde_json::Value;

use super::*;
use crate::provider::client_version::Version;

const ON_DEMAND: &str =
    "https://platform.claude.com/docs/en/build-with-claude/compaction-on-demand";
const WEB_SEARCH_PAGE: &str =
    "https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool";

fn basic_search() -> Option<Vec<String>> {
    Some(vec![wisp_protocol::settings::CLAUDE_WEB_SEARCH.to_owned()])
}

const LISTED: [&str; 14] = [
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "gpt-6.2-sol",
    "gpt-6.2-astra",
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6.1-terra",
    "gpt-6-luna",
    "gemini-4-carbon",
    "gemini-4-argon",
    "gemini-4-hydrogen",
    "gemini-3.8-flash",
    "gemini-2.5-pro",
];

#[test]
fn listed_models_rank_in_the_shipped_files_row_order() {
    let table = ModelTable::shipped().unwrap();
    let ranks: Vec<Option<u16>> = LISTED.iter().map(|id| table.listed_rank(id)).collect();
    assert_eq!(ranks, (0..14).map(Some).collect::<Vec<_>>());
    assert_eq!(table.listed_rank("claude-opus-4-8"), None);
    assert_eq!(table.listed_rank("not-a-model"), None);
}

#[test]
fn listed_belongs_to_the_shipped_file_only() {
    let problem = config_rows(Site::Models, "[models]\n\"m\" = { listed = true }\n").unwrap_err();
    assert!(matches!(problem.problem, RowProblem::ShippedOnly { .. }));
}

fn shipped_error(text: &str) -> RowProblem {
    ModelTable::parse_shipped(text).unwrap_err().problem
}

fn config_rows(site: Site, text: &str) -> Result<Vec<ModelRow>, RowError> {
    let root = DeTable::parse(text).unwrap();
    let (key, value) = root.get_ref().iter().next().unwrap();
    walk_models(site, key.span(), value.get_ref())
}

#[test]
fn the_shipped_file_parses() {
    let table = ModelTable::shipped().unwrap();
    assert_eq!(table.rows().len(), 57);
    assert!(table.find("gemini-4-carbon").unwrap().bill_tool_use_prompt);
    assert!(
        table
            .find("gemini-4-hydrogen")
            .unwrap()
            .bill_tool_use_prompt
    );
    assert!(!table.find("gemini-3.8-flash").unwrap().bill_tool_use_prompt);
    let root = DeTable::parse(SHIPPED).unwrap();
    for (top, fields) in [
        ("models", &SHIPPED_FIELDS[..]),
        ("providers", &PROVIDER_FIELDS),
    ] {
        let tables = root.get_ref()[top].get_ref().as_table().unwrap().values();
        let set: BTreeSet<&str> = tables
            .flat_map(|entry| entry.get_ref().as_table().unwrap().keys())
            .map(|key| &**key.get_ref())
            .collect();
        for (field, _) in fields {
            assert!(set.contains(field), "no {top} entry sets {field}");
        }
    }
    let models = root.get_ref()["models"].get_ref().as_table().unwrap();
    for (row, (_, entry)) in table.rows().iter().zip(in_order(models)) {
        let mut keys = entry.get_ref().as_table().unwrap().keys();
        let flags = keys.any(|key| CAPABILITY_FIELDS.contains(&&**key.get_ref()));
        assert_eq!(row.capabilities.is_some(), flags, "{}", row.id);
    }
    for id in ["gpt-6.2-sol", "gpt-6.2-astra"] {
        let row = table.find(id).unwrap();
        assert_eq!(row.context_window, Some(1_050_000), "{id}");
        assert_eq!(row.max_output, Some(128_000), "{id}");
        let caps = row.capabilities.unwrap();
        assert_eq!(caps.efforts, Effort::ALL, "{id}");
        assert_eq!(caps.default_effort, Some(Effort::Medium), "{id}");
        assert_eq!(caps.images.unwrap().per_request, 1500, "{id}");
        assert_eq!(row.web_search, Some(vec!["web_search".into()]), "{id}");
    }
    assert!(CAPABILITY_FIELDS.iter().all(|name| shipped_field(name)));
    let mut pairs = BTreeSet::new();
    let mut families = BTreeMap::new();
    for row in table
        .rows()
        .iter()
        .filter(|row| row.effort_variant.is_some())
    {
        let own = row.effort_variant.as_ref().unwrap();
        assert!(pairs.insert((&own.family, own.effort)), "{}", row.id);
        *families.entry(&own.family).or_insert(0) += 1;
        let caps = row.capabilities.unwrap();
        let expected_efforts: &[Effort] = if own.family == "gemini-4-hydrogen" {
            &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh]
        } else {
            &[Effort::Low, Effort::Medium, Effort::High]
        };
        assert_eq!(caps.efforts, expected_efforts, "{}", row.id);
        assert!(caps.effort_in_id);
        assert_eq!(caps.default_effort, None);
        assert_eq!(table.own_effort(&row.id, Effort::Max), own.effort);
        assert_eq!(table.sibling(&row.id, own.effort), Some(row.id.as_str()));
    }
    assert_eq!(pairs.len(), 13);
    assert!(families.iter().all(|(family, count)| {
        (*family == "gemini-4-hydrogen" && *count == 4)
            || (*family != "gemini-4-hydrogen" && *count == 3)
    }));
    let table = ModelTable::shared();
    let lists = &table
        .provider(ProviderKind::Anthropic)
        .unwrap()
        .weekly_limits;
    assert_eq!(lists.len(), 1);
    assert_eq!(
        lists["Fable"],
        ["claude-fable-5-2", "claude-fable-5-1", "claude-fable-5"]
    );
    for id in &lists["Fable"] {
        assert_eq!(
            table.find(id).unwrap().provider,
            Some(ProviderKind::Anthropic)
        );
    }
}

#[test]
fn subscription_identity_data_renders_in_order_and_rejects_invalid_rows() {
    let root = DeTable::parse(SHIPPED).unwrap();
    let providers = root.get_ref()["providers"].get_ref().as_table().unwrap();
    let anthropic = providers["anthropic"].get_ref().as_table().unwrap();
    let row = anthropic["claude_code"].get_ref().as_table().unwrap();
    let original = ModelTable::shared()
        .provider(ProviderKind::Anthropic)
        .unwrap();
    let version = original.client.unwrap().version;
    let changed_version = Version::parse("1.2.3").unwrap();
    let template = original.claude_code.as_ref().unwrap();
    let identity = template.rendered(changed_version).unwrap();
    let original = template.rendered(version).unwrap();
    let version = version.to_string();
    let pairs = row["headers"].get_ref().as_array().unwrap();
    for ((name, value), pair) in identity.headers.iter().zip(pairs) {
        let pair = pair.get_ref().as_array().unwrap();
        assert_eq!(name.as_str(), pair[0].get_ref().as_str().unwrap());
        let before = original
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .unwrap()
            .1
            .to_str()
            .unwrap();
        assert_eq!(value.to_str().unwrap(), before.replace(&version, "1.2.3"));
    }
    assert_eq!(identity.headers.len(), pairs.len());
    for ((name, value), (_, before)) in identity.usage_headers.iter().zip(&original.usage_headers) {
        assert_eq!(
            value.to_str().unwrap(),
            before.to_str().unwrap().replace(&version, "1.2.3"),
            "{name}"
        );
        if name == "x-app" {
            assert!(identity.headers.contains(&(name.clone(), value.clone())));
        }
    }
    let marked: Vec<_> = row["betas"]
        .get_ref()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| {
            let entry = entry.get_ref().as_table().unwrap();
            entry
                .get("usage")?
                .get_ref()
                .as_bool()
                .unwrap()
                .then(|| entry["value"].get_ref().as_str().unwrap())
        })
        .collect();
    let usage_beta = identity
        .usage_headers
        .iter()
        .find(|(name, _)| name == "anthropic-beta")
        .unwrap();
    assert_eq!(usage_beta.1.to_str().unwrap(), marked.join(","));
    for (before, after) in [
        ("usage_user_agent =", "version ="),
        ("{version}", "{unknown}"),
        ("usage = true", "usage = 1"),
        (", usage = true", ""),
        ("\"x-app\"", "\"missing-app\""),
        ("\"user-agent\"", "\"missing-agent\""),
        ("\"user-agent\"", "\"bad header\""),
        ("\"x-app\"", "\"user-agent\""),
        ("{arch}", "bad\\nvalue"),
        ("headers = [", "headers = [ [\"broken\"],"),
    ] {
        assert_eq!(
            shipped_error(&SHIPPED.replacen(before, after, 1)),
            RowProblem::BadValue {
                field: "claude_code",
                allowed: &IDENTITY_ALLOWED,
            },
            "{before}"
        );
    }
}

const CAPABILITY_FIELDS: [&str; 10] = [
    "efforts",
    "effort_variant",
    "default_effort",
    "per_message_effort",
    "inline_tools",
    "tools_after_results",
    "deferred_tools",
    "adaptive_thinking",
    "thinking_budgets",
    "images",
];

#[test]
fn every_shipped_model_id_and_alias_is_shown_as_itself() {
    let table = ModelTable::shipped().unwrap();
    let mut names: Vec<String> = table
        .rows()
        .iter()
        .flat_map(|row| std::iter::once(&row.id).chain(&row.aliases))
        .cloned()
        .collect();
    names.extend(crate::pricing::model_ids().map(str::to_owned));
    for name in &names {
        assert!(wisp_protocol::check_model_id(name).is_ok(), "{name}");
        assert_eq!(wisp_protocol::shown_model(name), name.as_str());
    }
}

#[test]
fn every_shipped_row_names_its_pages() {
    let table = ModelTable::shipped().unwrap();
    for row in table.rows() {
        assert!(row.source.is_some(), "{}", row.id);
        let provider = row.provider.and_then(|kind| table.provider(kind));
        assert!(provider.is_some(), "{}", row.id);
        if row.native_compaction.is_some() {
            assert_eq!(
                row.source_of("native_compaction"),
                Some(ON_DEMAND),
                "{}",
                row.id
            );
        }
        assert_eq!(row.compact_at, None, "{}", row.id);
    }
}

#[test]
fn a_dated_id_resolves_to_its_row() {
    let table = ModelTable::shipped().unwrap();
    let row = table.find("claude-opus-4-5-20251101").unwrap();
    assert_eq!(row.id, "claude-opus-4-5");
    assert_eq!(row.context_window, Some(200_000));
    assert_eq!(
        table.find("claude-opus-5-5").unwrap().context_window,
        Some(1_000_000)
    );
    assert!(table.find("some-gateway-model").is_none());
}

#[test]
fn the_shipped_file_refuses_config_only_keys() {
    for key in [
        "compact_at = \"85%\"",
        "compact_at = 400000",
        "verbatim_user_tokens = 8000",
        "background_compaction = false",
        "background_tokens = 40000",
    ] {
        let text = format!(
            "[models.\"m\"]\ncontext_window = 1000000\nsource = \"https://example.invalid\"\n{key}\n"
        );
        let field = key.split(' ').next().unwrap().to_owned();
        assert_eq!(
            shipped_error(&text),
            RowProblem::ConfigOnly { field },
            "{key}"
        );
    }
}

#[test]
fn the_shipped_file_refuses_unknown_keys() {
    let text = "[models.\"m\"]\ncontext_window = 1\nsource = \"s\"\ntested = true\n";
    let error = ModelTable::parse_shipped(text).unwrap_err();
    assert_eq!(
        error.problem,
        RowProblem::UnknownField {
            site: Site::Shipped
        }
    );
    assert_eq!(error.line, Some(4));
    assert_eq!(
        error.to_string(),
        "the shipped model file, line 4: unknown field in a model table; allowed: context_window, native_compaction, web_search, aliases, listed, provider, price_input, price_output, price_cache_read, long_prompt, bill_tool_use_prompt, min_cacheable, max_output, efforts, effort_variant, default_effort, per_message_effort, inline_tools, tools_after_results, deferred_tools, adaptive_thinking, thinking_budgets, images, source, sources"
    );
    let error = ModelTable::parse_shipped("store_id = \"x\"\n").unwrap_err();
    assert_eq!(error.problem, RowProblem::UnknownTop);
    assert_eq!(
        error.problem.to_string(),
        "unknown field; allowed: providers, models"
    );
    for input in [
        "weekly_limits = 1",
        "weekly_limits = { Fable = ['bad id'] }",
        "weekly_limits = { 'bad name' = ['good'] }",
    ] {
        assert!(matches!(
            shipped_error(&format!("[providers.anthropic]\n{input}\n")),
            RowProblem::BadValue {
                field: "weekly_limits",
                ..
            }
        ));
    }
    let error = ModelTable::parse_shipped("[providers.openai]\nwrite_1m = 1.0\n").unwrap_err();
    assert_eq!(error.line, Some(2));
    assert_eq!(
        error.problem.to_string(),
        "unknown field in a provider table; allowed: client, claude_code, codex, write_5m, write_1h, write_30m, write_12h, web_search_price, message_threads, weekly_limits, account_bound_thinking, default_model, web_search_model, web_search_by_call, antigravity, subscription_additional_tools, api_additional_tools, subscription_tools_after_outputs, api_tools_after_outputs, sources"
    );
    assert_eq!(
        shipped_error("[providers.mistral]\nwrite_1h = 1.0\n").to_string(),
        "provider must be \"anthropic\", \"openai\" or \"gemini\""
    );
}

#[test]
fn a_shipped_row_needs_its_sources() {
    assert_eq!(
        shipped_error("[providers.anthropic.weekly_limits]\nFable = ['claude-fable-5']\n"),
        missing("providers", "anthropic", "weekly_limits", "weekly_limits")
    );
    assert_eq!(
        shipped_error("[models.\"m\"]\ncontext_window = 1\n"),
        RowProblem::MissingSource { model: "m".into() }
    );
    assert_eq!(
        shipped_error(
            "[models.\"m\"]\ncontext_window = 1\nnative_compaction = true\nsource = \"s\"\n"
        ),
        missing("models", "m", "native_compaction", "native_compaction")
    );
    let priced = "[models.\"m\"]\nprovider = \"openai\"\nprice_input = 1.0\nmin_cacheable = 1024\nsource = \"s\"\n";
    assert_eq!(
        shipped_error(priced),
        missing("models", "m", "price_input", "price")
    );
    let shared = format!("{priced}[providers.openai]\nsources = {{ price = \"p\" }}\n");
    let wanted = missing("models", "m", "min_cacheable", "min_cacheable");
    assert_eq!(shipped_error(&shared), wanted);
    let own = format!("{priced}sources = {{ price = \"p\", min_cacheable = \"c\" }}\n");
    let row = ModelTable::parse_shipped(&own).unwrap().rows()[0].clone();
    assert_eq!(
        (row.price_input, row.min_cacheable),
        (Some(1_000_000), Some(1024))
    );
    for (body, field, key) in [
        ("default_effort = \"high\"", "default_effort", "efforts"),
        (
            "images = { tier = \"high\", per_request = 20 }",
            "images",
            "images",
        ),
        ("deferred_tools = true", "deferred_tools", "deferred_tools"),
        (
            "tools_after_results = true",
            "tools_after_results",
            "tools_after_results",
        ),
    ] {
        let text = format!("[models.\"m\"]\nprovider = \"openai\"\n{body}\nsource = \"s\"\n");
        assert_eq!(
            shipped_error(&text),
            missing("models", "m", field, key),
            "{body}"
        );
    }
    let tools = "[models.\"m\"]\nprovider = \"anthropic\"\ninline_tools = true\nsource = \"s\"\n";
    let shared = format!("{tools}[providers.anthropic]\nsources = {{ inline_tools = \"t\" }}\n");
    assert!(ModelTable::parse_shipped(&shared).is_ok());
    let list = "[providers.anthropic]\nmessage_threads = [\"m\"]\n";
    let wanted = missing(
        "providers",
        "anthropic",
        "message_threads",
        "message_threads",
    );
    assert_eq!(shipped_error(list), wanted);
    for field in [
        "subscription_additional_tools",
        "api_additional_tools",
        "subscription_tools_after_outputs",
        "api_tools_after_outputs",
    ] {
        assert_eq!(
            shipped_error(&format!("[providers.openai]\n{field} = [\"m\"]\n")),
            missing("providers", "openai", field, field)
        );
    }
    let error = ModelTable::parse_shipped("[providers.openai]\nwrite_30m = 1.25\n").unwrap_err();
    assert_eq!(error.line, Some(1));
    assert_eq!(
        error.problem.to_string(),
        "[providers.\"openai\"] sets write_30m, so sources needs cache_writes"
    );
}

fn missing(table: &'static str, name: &str, field: &'static str, key: &'static str) -> RowProblem {
    RowProblem::MissingFieldSource {
        table,
        name: name.into(),
        field,
        key,
    }
}

#[test]
fn a_shipped_web_search_list_needs_its_own_source() {
    assert_eq!(
        shipped_error("[models.\"m\"]\nweb_search = [\"web_search_20250305\"]\nsource = \"s\"\n"),
        missing("models", "m", "web_search", "web_search")
    );
    let table = ModelTable::parse_shipped(
        "[models.\"m\"]\nweb_search = [\"web_search_20250305\"]\nsource = \"s\"\nsources = { web_search = \"w\" }\n",
    )
    .unwrap();
    let row = table.find("m").unwrap();
    assert_eq!(row.web_search, basic_search());
    assert_eq!(row.source_of("web_search"), Some("w"));
    assert_eq!(row.source_of("context_window"), Some("s"));
}

#[test]
fn every_shipped_search_row_names_its_web_search_page() {
    let table = ModelTable::shipped().unwrap();
    for row in table.rows() {
        let kind = crate::endpoints::goes::model_provider(&row.id).unwrap();
        if row.id == "gemini-3.1-flash-image" {
            assert_eq!(row.web_search, None);
            continue;
        }
        let version = crate::search::listed_version(kind).to_owned();
        assert_eq!(row.web_search, Some(vec![version]), "{}", row.id);
        let page = match kind {
            crate::endpoints::ProviderKind::Anthropic => WEB_SEARCH_PAGE,
            crate::endpoints::ProviderKind::Gemini => table
                .provider(kind)
                .unwrap()
                .sources
                .get("web_search_price")
                .unwrap()
                .as_str(),
            crate::endpoints::ProviderKind::OpenAi => continue,
        };
        assert_eq!(row.source_of("web_search"), Some(page), "{}", row.id);
    }
}

#[test]
fn a_web_search_version_wisp_does_not_know_is_refused() {
    for list in [
        "[\"web_search_20990101\"]",
        "[1]",
        "\"web_search_20250305\"",
    ] {
        let text = format!("[models.\"m\"]\nweb_search = {list}\n");
        assert_eq!(
            ConfigModels::parse(&text).unwrap_err().problem,
            RowProblem::BadValue {
                field: "web_search",
                allowed: &WEB_SEARCH_ALLOWED,
            },
            "{list}"
        );
    }
    let empty = ConfigModels::parse("[models.\"m\"]\nweb_search = []\n").unwrap();
    assert_eq!(empty.models[0].web_search, Some(Vec::new()));
}

#[test]
fn config_reads_web_search_at_both_sites_and_the_endpoint_opt_in() {
    let text = "[models.\"mine\"]\nweb_search = [\"web_search_20250305\", \"web_search_20250305\"]\n\n[endpoints.gw]\nnative_web_search = true\n\n[endpoints.gw.models.\"other\"]\nweb_search = []\n\n[endpoints.off]\nnative_web_search = false\n";
    let parsed = ConfigModels::parse(text).unwrap();
    assert_eq!(parsed.models[0].web_search, basic_search());
    assert_eq!(parsed.endpoints["gw"][0].web_search, Some(Vec::new()));
    assert_eq!(
        parsed.native_web_search,
        BTreeMap::from([("gw".to_owned(), true), ("off".to_owned(), false)])
    );
    let off = ConfigModels::parse("[endpoints.gw]\nnative_web_search = false\n").unwrap();
    assert_eq!(
        off.native_web_search,
        BTreeMap::from([("gw".to_owned(), false)])
    );
    let wrong = ConfigModels::parse("[endpoints.gw]\nnative_web_search = \"yes\"\n").unwrap_err();
    assert_eq!(
        wrong.problem,
        RowProblem::WrongType {
            field: "native_web_search",
            expected: "true or false"
        }
    );
}

#[test]
fn bad_values_are_refused_with_their_line() {
    assert_eq!(
        shipped_error("[models.\"m\"]\nbill_tool_use_prompt = 1\n"),
        RowProblem::WrongType {
            field: "bill_tool_use_prompt",
            expected: "true or false"
        }
    );
    assert_eq!(
        shipped_error("[models.\"m\"]\nsource = 7\ncontext_window = 1\n"),
        RowProblem::WrongType {
            field: "source",
            expected: "a string"
        }
    );
    let error =
        ModelTable::parse_shipped("[models.\"m\"]\nsource = \"s\"\ncontext_window = \"big\"\n")
            .unwrap_err();
    assert_eq!(error.line, Some(3));
    assert_eq!(
        error.problem,
        RowProblem::WrongType {
            field: "context_window",
            expected: "a token count, such as 8000 or \"8k\""
        }
    );
    assert_eq!(
        shipped_error("[models.\"m\"]\nsource = \"s\"\ncontext_window = 0\n"),
        RowProblem::BadValue {
            field: "context_window",
            allowed: "a positive number of tokens"
        }
    );
    assert_eq!(
        shipped_error("[models.\"m\"]\nsource = \"s\"\ncontext_window = -5\n"),
        RowProblem::BadValue {
            field: "context_window",
            allowed: "a positive number of tokens"
        }
    );
    assert_eq!(shipped_error("[models.\"m\"\n"), RowProblem::Syntax);
    for (body, field, allowed) in [
        ("price_input = -1", "price_input", USD_WORDS),
        ("provider = \"x\"", "provider", &*PROVIDER_ALLOWED),
        (
            "long_prompt = { over = 1 }",
            "long_prompt",
            &*LONG_PROMPT_ALLOWED,
        ),
        ("efforts = [\"ultra\"]", "efforts", &*EFFORTS_ALLOWED),
        (
            "effort_variant = { family = \"m\" }",
            "effort_variant",
            EFFORT_VARIANT_ALLOWED,
        ),
        (
            "effort_variant = []",
            "effort_variant",
            EFFORT_VARIANT_ALLOWED,
        ),
        (
            "effort_variant = { family = \"bad id\", effort = \"low\" }",
            "effort_variant",
            EFFORT_VARIANT_ALLOWED,
        ),
        (
            "effort_variant = { family = \"m\", effort = \"ultra\" }",
            "effort_variant",
            EFFORT_VARIANT_ALLOWED,
        ),
        (
            "efforts = []\neffort_variant = { family = \"m\", effort = \"low\" }",
            "effort_variant",
            EFFORT_VARIANT_ALLOWED,
        ),
        (
            "effort_variant = { family = \"m\", effort = \"low\" }\nefforts = []",
            "effort_variant",
            EFFORT_VARIANT_ALLOWED,
        ),
        (
            "images = { tier = \"huge\", per_request = 20 }",
            "images",
            &*IMAGES_ALLOWED,
        ),
    ] {
        let problem = shipped_error(&format!("[models.\"m\"]\nsource = \"s\"\n{body}\n"));
        assert_eq!(problem, RowProblem::BadValue { field, allowed }, "{body}");
    }
    assert_eq!(
        *EFFORTS_ALLOWED,
        "a list of \"low\", \"medium\", \"high\", \"xhigh\" or \"max\""
    );
    assert_eq!(
        *IMAGES_ALLOWED,
        "a table of tier (\"standard\" or \"high\") and per_request"
    );
    for (body, refusal) in [
        (
            "price_input = \"cheap\"",
            "price_input must be a number of US dollars, such as 0.5",
        ),
        (
            "max_output = 0",
            "max_output must be a positive number of tokens",
        ),
        (
            "max_output = 5000000000",
            "max_output must be a positive number of tokens",
        ),
    ] {
        let text = format!("[models]\n\"m\" = {{ {body} }}\n");
        let problem = config_rows(Site::EndpointModels, &text)
            .unwrap_err()
            .problem;
        assert_eq!(problem.to_string(), refusal, "{body}");
    }
    let multiplier = shipped_error("[providers.openai]\nwrite_1h = \"twice\"\n");
    assert_eq!(
        multiplier.to_string(),
        "write_1h must be a number, such as 1.25"
    );
}

#[test]
fn an_id_may_name_only_one_row() {
    let text = "[models.\"a\"]\naliases = [\"b\"]\ncontext_window = 1\nsource = \"s\"\n\n[models.\"b\"]\ncontext_window = 1\nsource = \"s\"\n";
    assert_eq!(
        shipped_error(text),
        RowProblem::Duplicate { model: "b".into() }
    );
}

#[test]
fn config_tables_take_their_keys_at_both_sites() {
    let text = "[models]\n\"claude-opus-5-5\" = { context_window = 400000, compact_at = \"60%\", native_compaction = false }\n\"my-new-model\" = { context_window = 400_000, compact_at = 300000, aliases = [\"my-new-model-2026\"] }\n";
    let rows = config_rows(Site::Models, text).unwrap();
    assert_eq!(rows[0].context_window, Some(400_000));
    assert_eq!(
        rows[0].compact_at,
        Some(CompactAt::Share(
            wisp_protocol::settings::Percent::new(60).unwrap()
        ))
    );
    assert_eq!(rows[0].native_compaction, Some(false));
    assert_eq!(rows[1].context_window, Some(400_000));
    assert_eq!(rows[1].compact_at, Some(CompactAt::Tokens(300_000)));
    assert!(rows[1].answers_to("my-new-model-2026"));
    let text = "[models]\n\"claude-opus-5-5\" = { context_window = 400000, compact_at = \"60%\", native_compaction = true }\n";
    let rows = config_rows(Site::EndpointModels, text).unwrap();
    assert_eq!(rows[0].native_compaction, Some(true));
    for site in [Site::Models, Site::EndpointModels] {
        let text = "[models]\n\"m\" = { price_input = 1.2, price_output = \"4.80\", price_cache_read = 0, max_output = \"32k\" }\n";
        let row = &config_rows(site, text).unwrap()[0];
        let prices = (row.price_input, row.price_output, row.price_cache_read);
        assert_eq!(prices, (Some(1_200_000), Some(4_800_000), Some(0)));
        assert_eq!(row.max_output, Some(32_000));
        assert_eq!(row.capabilities, None);
    }
    for site in [Site::Models, Site::EndpointModels] {
        let text = "[models]\n\"m\" = { verbatim_user_tokens = 4000 }\n";
        let rows = config_rows(site, text).unwrap();
        assert_eq!(rows[0].verbatim_user_tokens, Some(4000));
        let text = "[models]\n\"m\" = { verbatim_user_tokens = \"lots\" }\n";
        assert_eq!(
            config_rows(site, text).unwrap_err().problem,
            RowProblem::WrongType {
                field: "verbatim_user_tokens",
                expected: "a token count, such as 8000 or \"8k\""
            }
        );
    }
}

#[test]
fn config_tables_refuse_what_belongs_elsewhere() {
    let refused = |site, body: &str| {
        config_rows(site, &format!("[models]\n\"m\" = {{ {body} }}\n"))
            .unwrap_err()
            .problem
    };
    assert_eq!(
        refused(Site::EndpointModels, "aliases = [\"m-1\"]"),
        RowProblem::NotAtEndpoint { field: "aliases" }
    );
    for site in [Site::Models, Site::EndpointModels] {
        for field in [
            "source",
            "provider",
            "min_cacheable",
            "long_prompt",
            "bill_tool_use_prompt",
            "efforts",
        ] {
            let problem = refused(site, &format!("{field} = 1"));
            let field = field.to_owned();
            assert_eq!(problem, RowProblem::ShippedOnly { field }, "{site:?}");
        }
    }
    assert_eq!(
        refused(Site::Models, "efforts = [\"low\"]").to_string(),
        "efforts belongs to the shipped model file, not config.toml"
    );
    for site in [Site::Models, Site::EndpointModels] {
        let text =
            "[models]\n\"m\" = { background_compaction = false, background_tokens = 20000 }\n";
        let rows = config_rows(site, text).unwrap();
        assert_eq!(rows[0].background_compaction, Some(false));
        assert_eq!(rows[0].background_tokens, Some(20_000));
        assert_eq!(
            refused(site, "compact_at = false"),
            RowProblem::WrongType {
                field: "compact_at",
                expected: "a number of tokens (400000) or a share of the window (\"85%\")"
            }
        );
    }
    assert_eq!(
        refused(Site::Models, "compact_at = \"101%\""),
        RowProblem::BadValue {
            field: "compact_at",
            allowed: "a number of tokens (400000) or a share of the window (\"85%\")"
        }
    );
    assert_eq!(
        refused(Site::Models, "native_compaction = \"yes\""),
        RowProblem::WrongType {
            field: "native_compaction",
            expected: "true or false"
        }
    );
}

#[test]
fn a_share_over_a_hundred_is_refused_at_load_with_its_line() {
    let text = "[models]\n\"m\" = { context_window = 400000 }\n\"n\" = { compact_at = \"101%\" }\n";
    for site in [Site::Models, Site::EndpointModels] {
        let error = config_rows(site, text).unwrap_err();
        assert_eq!(
            error.problem,
            RowProblem::BadValue {
                field: "compact_at",
                allowed: "a number of tokens (400000) or a share of the window (\"85%\")"
            },
            "{site:?}"
        );
        let span = error.span.clone().unwrap();
        assert_eq!(&text[span.clone()], "compact_at", "{site:?}");
        assert_eq!(error.line(text), Some(3), "{site:?}");
    }
    let shipped = "[models.\"m\"]\ncontext_window = 1\nsource = \"s\"\ncompact_at = \"101%\"\n";
    let error = ModelTable::parse_shipped(shipped).unwrap_err();
    assert_eq!(
        error.problem,
        RowProblem::ConfigOnly {
            field: "compact_at".into()
        }
    );
    assert_eq!(error.line, Some(4));
}

#[test]
fn config_tables_refuse_a_small_limit_at_its_row() {
    let text = "default = \"gw\"\n[models]\n\"a\" = { context_window = 400000 }\n\"b\" = { context_window = 100000 }\n\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n\n[endpoints.gw.models.\"c\"]\ncontext_window = 90000\n";
    let err = ConfigModels::parse(text).unwrap_err();
    assert_eq!(err.line, Some(4));
    assert!(matches!(err.problem, RowProblem::LimitTooSmall(ref l) if l.model == "b"));
    let text = "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"K\"\n\n[endpoints.gw.models.\"c\"]\ncontext_window = 90000\n";
    let err = ConfigModels::parse(text).unwrap_err();
    assert_eq!(err.line, Some(5));
    let fine = ConfigModels::parse(
        "[models.\"a\"]\ncontext_window = 400000\n[endpoints.gw.models.\"a\"]\ncompact_at = \"60%\"\n",
    )
    .unwrap();
    assert_eq!(fine.models[0].context_window, Some(400_000));
    assert_eq!(fine.endpoints["gw"][0].id, "a");
}

#[test]
fn a_key_as_a_model_row_is_never_named_in_its_problem() {
    let key = || "sk-ant-api03-CANARYMODEL0000".to_owned();
    let texts: Vec<String> = [
        RowProblem::MissingSource { model: key() },
        missing("models", &key(), "web_search", "web_search"),
        RowProblem::Duplicate { model: key() },
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    for text in &texts {
        assert!(!text.contains("CANARY"), "{text}");
    }
    assert_eq!(
        texts[0],
        "[models.\"MODEL\"] needs source, the page its values come from"
    );
    assert_eq!(texts[2], "this model names more than one model row");
}

#[test]
fn openai_rows_list_openai_search_from_their_own_page() {
    let table = ModelTable::shipped().unwrap();
    let openai = crate::endpoints::ProviderKind::OpenAi;
    let rows: Vec<&ModelRow> = table
        .rows()
        .iter()
        .filter(|row| row.provider == Some(openai))
        .collect();
    assert!(!rows.is_empty());
    for row in rows {
        let (id, source) = (&row.id, row.source.as_deref());
        let page = source.and_then(|s| s.strip_prefix(OPENAI_MODEL_PAGES));
        assert!(page.is_some_and(|page| id.starts_with(page)), "{id}");
        assert_eq!(row.context_window, Some(1_050_000), "{id}");
        assert_eq!(row.native_compaction, None, "{id}");
        assert_eq!(row.web_search, Some(vec!["web_search".to_owned()]), "{id}");
        assert_eq!(
            row.sources.get("web_search").map(String::as_str),
            source,
            "{id}"
        );
    }
}

const OPENAI_MODEL_PAGES: &str = "https://developers.openai.com/api/docs/models/";

#[test]
fn token_fields_in_a_model_row_take_a_number_or_a_short_form() {
    for site in [Site::Models, Site::EndpointModels] {
        for (body, tokens) in [
            (
                "background_tokens = \"40k\", verbatim_user_tokens = \"8k\", context_window = \"1M\"",
                (40_000, 8_000, 1_000_000),
            ),
            (
                "background_tokens = 40000, verbatim_user_tokens = 8000, context_window = 1000000",
                (40_000, 8_000, 1_000_000),
            ),
        ] {
            let rows = config_rows(site, &format!("[models]\n\"m\" = {{ {body} }}\n")).unwrap();
            assert_eq!(rows[0].background_tokens, Some(tokens.0), "{body}");
            assert_eq!(rows[0].verbatim_user_tokens, Some(tokens.1), "{body}");
            assert_eq!(rows[0].context_window, Some(tokens.2), "{body}");
        }
        let refused = |body: &str| {
            config_rows(site, &format!("[models]\n\"m\" = {{ {body} }}\n"))
                .unwrap_err()
                .problem
        };
        assert_eq!(
            refused("background_tokens = \"lots\""),
            RowProblem::WrongType {
                field: "background_tokens",
                expected: "a token count, such as 8000 or \"8k\""
            }
        );
        assert_eq!(
            refused("verbatim_user_tokens = -8000"),
            RowProblem::BadValue {
                field: "verbatim_user_tokens",
                allowed: "a token count, such as 8000 or \"8k\""
            }
        );
        assert_eq!(
            refused("background_tokens = 0"),
            RowProblem::BadValue {
                field: "background_tokens",
                allowed: "a positive number of tokens"
            }
        );
        assert_eq!(
            refused("context_window = \"0k\""),
            RowProblem::BadValue {
                field: "context_window",
                allowed: "a positive number of tokens"
            }
        );
    }
}

const CATALOG_EXCEPTIONS: [&str; 1] = ["claude-mythos-preview"];
const OPENAI_DOCS: &str = include_str!("../../../../docs/openai.md");
const GEMINI_DOCS: &str = include_str!("../../../../docs/gemini.md");

#[test]
fn every_provider_prices_and_describes_exactly_its_shipped_models() {
    use crate::endpoints::{ProviderKind, goes::model_provider};
    use crate::provider::{anthropic::capabilities, gemini, shipped_capabilities};
    let table = ModelTable::shipped().unwrap();
    let mut shipped = BTreeSet::new();
    for row in table.rows() {
        for id in std::iter::once(&row.id).chain(&row.aliases) {
            shipped.insert(id.as_str());
            let described = !CATALOG_EXCEPTIONS.contains(&id.as_str());
            let priced = crate::pricing::model_info(id).is_some();
            assert_eq!(priced, described, "{id}: price");
            let kind = shipped_capabilities(id).map(|(kind, _)| kind);
            assert_eq!(kind, row.provider.filter(|_| described), "{id}");
            assert_eq!(model_provider(id), row.provider, "{id}");
            let prefixed = id.starts_with(capabilities::ID_PREFIX);
            assert_eq!(
                prefixed,
                row.provider == Some(ProviderKind::Anthropic),
                "{id}"
            );
            let gemini_prefixed = id.starts_with(gemini::ID_PREFIX);
            assert_eq!(
                gemini_prefixed,
                row.provider == Some(ProviderKind::Gemini),
                "{id}"
            );
        }
    }
    let priced: BTreeSet<&str> = crate::pricing::model_ids().collect();
    assert!(priced.is_subset(&shipped));
    for id in CATALOG_EXCEPTIONS {
        assert!(shipped.contains(id), "{id} is not in models.toml");
    }
}

#[test]
fn the_openai_docs_name_the_default_model() {
    let default = format!("defaults to `{}`", crate::provider::openai::DEFAULT_MODEL);
    assert!(OPENAI_DOCS.contains(&default), "{default}");
}

#[test]
fn the_gemini_docs_name_the_default_model() {
    let default = format!("defaults to `{}`", crate::provider::gemini::default_model());
    assert!(GEMINI_DOCS.contains(&default), "{default}");
    let antigravity = format!(
        "defaults to `{}`",
        crate::provider::gemini::antigravity_default_model()
    );
    assert!(GEMINI_DOCS.contains(&antigravity), "{antigravity}");
}

const ROWS_DIFFER_UNTIL_VERBATIM_SITES: [&str; 1] = ["verbatim_user_tokens"];

#[test]
fn the_row_field_lists_are_the_registrys() {
    use wisp_protocol::settings::{SETTINGS, Site as Sited};
    let fields = |site: Site| -> BTreeSet<&'static str> { site.allowed().split(", ").collect() };
    let shipped: Vec<&str> = fields(Site::Shipped)
        .into_iter()
        .filter(|field| shipped_only(field))
        .collect();
    let leaves = |sited: Sited| -> BTreeSet<&'static str> {
        SETTINGS
            .iter()
            .filter(|s| s.is_built() && s.has_site(sited))
            .map(|s| s.leaf())
            .collect()
    };
    for (site, sited) in [
        (Site::Shipped, Sited::Shipped),
        (Site::Models, Sited::Model),
        (Site::EndpointModels, Sited::EndpointModel),
    ] {
        let mut want = leaves(sited);
        let (extra, why): (&[&str], &str) = match site {
            Site::Shipped => (&shipped, "belongs to the shipped table only"),
            Site::Models | Site::EndpointModels => (
                &ROWS_DIFFER_UNTIL_VERBATIM_SITES,
                "the rows read verbatim_user_tokens per model until it gets its model sites; remove this entry then",
            ),
        };
        for leaf in extra {
            assert!(want.insert(leaf), "{leaf}: {why}");
        }
        assert_eq!(fields(site), want, "{site:?}");
    }
}

#[test]
fn each_default_model_names_a_listed_shipped_row_of_its_provider() {
    use crate::endpoints::ProviderKind;
    let table = ModelTable::shipped().unwrap();
    for kind in ProviderKind::ALL {
        let id = kind.default_model();
        let row = table.find(id).unwrap();
        assert_eq!(row.id, id);
        assert!(row.listed, "{id}");
        assert_eq!(row.provider, Some(kind), "{id}");
    }
    let search = crate::provider::gemini::web_search_model();
    let row = table.find(search).unwrap();
    assert_eq!(row.provider, Some(ProviderKind::Gemini));
    assert!(row.price_input.is_some());
    assert!(row.price_output.is_some());
    let ag = crate::provider::gemini::antigravity_default_model();
    let row = table.find(ag).unwrap();
    assert_eq!(row.id, ag);
    assert_eq!(row.provider, Some(ProviderKind::Gemini), "{ag}");
}

#[test]
fn antigravity_shipped_metadata_has_no_machine_or_personal_data() {
    let table = ModelTable::shipped().unwrap();
    let gemini = table
        .provider(crate::endpoints::ProviderKind::Gemini)
        .unwrap();
    let identity = gemini.antigravity.as_ref().unwrap();
    assert_eq!(
        identity.base_url,
        "https://daily-cloudcode-pa.googleapis.com"
    );
    let shipped = gemini.client.unwrap().version;
    let ua = identity.user_agent(shipped).unwrap();
    let version = ua.strip_prefix("antigravity/cli/").unwrap();
    assert_eq!(version, shipped.to_string());
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(parts.len(), 3, "{ua}");
    assert!(
        parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        "{ua}"
    );
    assert!(!ua.chars().any(char::is_whitespace), "{ua}");
}

#[test]
fn antigravity_identity_data_renders_and_rejects_invalid_rows() {
    let changed = SHIPPED.replacen(
        "base_url = \"https://daily-cloudcode-pa.googleapis.com\"",
        "base_url = \"https://daily-cloudcode-pa.googleapis.com/\"",
        1,
    );
    let table = ModelTable::parse_shipped(&changed).unwrap();
    let identity = table
        .provider(ProviderKind::Gemini)
        .unwrap()
        .antigravity
        .as_ref()
        .unwrap();
    assert_eq!(
        identity.base_url,
        "https://daily-cloudcode-pa.googleapis.com"
    );
    let version = Version::parse("1.2.3").unwrap();
    assert_eq!(
        identity.user_agent(version).as_deref(),
        Some("antigravity/cli/1.2.3")
    );
    assert_eq!(identity.default_model, "gemini-4-carbon-medium");
    assert!(identity.session_id.is_some());
    let session_line = SHIPPED
        .lines()
        .find(|line| line.starts_with("session_id = "))
        .unwrap();
    for invalid in [
        "\"\"",
        "1",
        "\"with a space\"",
        "\"é\"",
        &format!("{:?}", "a".repeat(65)),
    ] {
        let broken = SHIPPED.replacen(session_line, &format!("session_id = {invalid}"), 1);
        assert!(ModelTable::parse_shipped(&broken).is_err(), "{invalid}");
    }
    let without = ModelTable::parse_shipped(&SHIPPED.replacen(session_line, "", 1)).unwrap();
    assert!(
        without
            .provider(ProviderKind::Gemini)
            .unwrap()
            .antigravity
            .as_ref()
            .unwrap()
            .session_id
            .is_none()
    );

    assert!(identity.cache_idle_s.is_some());
    let idle_line = SHIPPED
        .lines()
        .find(|line| line.starts_with("cache_idle_s = "))
        .unwrap();
    for invalid in ["0", "\"240\"", "240.5", "29", "86401"] {
        let broken = SHIPPED.replacen(idle_line, &format!("cache_idle_s = {invalid}"), 1);
        assert_eq!(
            shipped_error(&broken),
            RowProblem::BadValue {
                field: "providers.gemini.antigravity",
                allowed: &ANTIGRAVITY_IDENTITY_ALLOWED,
            },
            "{invalid}"
        );
    }
    let without = ModelTable::parse_shipped(&SHIPPED.replacen(idle_line, "", 1)).unwrap();
    assert!(
        without
            .provider(ProviderKind::Gemini)
            .unwrap()
            .antigravity
            .as_ref()
            .unwrap()
            .cache_idle_s
            .is_none()
    );

    let target = "[providers.gemini.antigravity]\nbase_url = \"https://daily-cloudcode-pa.googleapis.com\"\nuser_agent = \"antigravity/cli/{version}\"\ndefault_model = \"gemini-4-carbon-medium\"";
    assert!(SHIPPED.contains(target));
    for (label, replacement) in [
        (
            "old version field",
            format!("{target}\nversion = \"1.2.3\""),
        ),
        (
            "bad url scheme",
            target.replace(
                "https://daily-cloudcode-pa.googleapis.com",
                "ftp://daily-cloudcode-pa.googleapis.com",
            ),
        ),
        (
            "unparseable url",
            target.replace("https://daily-cloudcode-pa.googleapis.com", "not a url"),
        ),
        (
            "unexpanded placeholder",
            target.replace("{version}", "{version}-{other}"),
        ),
        (
            "missing version placeholder",
            target.replace("{version}", "1.5.5"),
        ),
        (
            "os token linux",
            target.replace("{version}", "{version}-Linux"),
        ),
        (
            "arch token arm64",
            target.replace("{version}", "{version}-ARM64"),
        ),
        (
            "whitespace in ua",
            target.replace("antigravity/cli/{version}", "antigravity/cli/{version} ext"),
        ),
        (
            "bad default_model",
            target.replace("gemini-4-carbon-medium", "bad model id"),
        ),
    ] {
        let broken = SHIPPED.replacen(target, &replacement, 1);
        assert_eq!(
            shipped_error(&broken),
            RowProblem::BadValue {
                field: "providers.gemini.antigravity",
                allowed: &ANTIGRAVITY_IDENTITY_ALLOWED,
            },
            "{label}"
        );
    }
}

#[test]
fn thinking_budgets_parse_and_refuse_bad_tables() {
    let table = ModelTable::shipped().unwrap();
    let pro = table.find("gemini-2.5-pro").unwrap().capabilities.unwrap();
    assert_eq!(
        pro.thinking_budgets,
        Some(ThinkingBudgets {
            low: Some(1024),
            medium: Some(8192),
            high: Some(24576),
            xhigh: None,
            max: Some(32768),
        })
    );
    assert_eq!(
        shipped_error(
            "[models.\"m\"]\nefforts = [\"low\"]\nthinking_budgets = {}\nsource = \"s\"\nsources = { efforts = \"e\" }\n"
        ),
        RowProblem::BadValue {
            field: "thinking_budgets",
            allowed: THINKING_BUDGETS_ALLOWED,
        }
    );
    assert_eq!(
        shipped_error(
            "[models.\"m\"]\nefforts = [\"low\"]\nthinking_budgets = { low = 0 }\nsource = \"s\"\nsources = { efforts = \"e\" }\n"
        ),
        RowProblem::BadValue {
            field: "thinking_budgets",
            allowed: THINKING_BUDGETS_ALLOWED,
        }
    );
}

#[test]
fn subscription_versions_come_from_client_tables_and_reject_invalid_rows() {
    let table = ModelTable::shipped().unwrap();
    for kind in [
        ProviderKind::OpenAi,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
    ] {
        let name = kind.as_str();
        let version = table
            .provider(kind)
            .unwrap()
            .client
            .unwrap()
            .version
            .to_string();
        let prefix = format!("[providers.{name}.client]");
        let original = format!("{prefix}\nversion = {version:?}");
        assert!(SHIPPED.contains(&original));
        let changed = SHIPPED.replace(&original, &format!("{prefix}\nversion = '999999999.0.3'"));
        let changed = ModelTable::parse_shipped(&changed).unwrap();
        assert_eq!(
            changed
                .provider(kind)
                .unwrap()
                .client
                .unwrap()
                .version
                .to_string(),
            "999999999.0.3"
        );
        for entry in [
            "",
            "version = 1",
            "version = ''",
            "version = '1.2'",
            "version = '1..3'",
            "version = '1.2.3.4'",
            "version = '01.2.3'",
            "version = '1.2.3-alpha'",
            "version = '+1.2.3'",
            "version = '1. 2.3'",
            "version = '1.5_5'",
            "version = '1.2.1000000000'",
            "version = '1.2.3&extra=1'",
            "version = '1.2.3'\nother = 1",
        ] {
            let broken = SHIPPED.replace(&original, &format!("{prefix}\n{entry}"));
            assert!(
                ModelTable::parse_shipped(&broken).is_err(),
                "{name}: {entry}"
            );
        }
        if kind != ProviderKind::OpenAi {
            let without_client = SHIPPED.replace(&original, "");
            assert!(
                matches!(
                    shipped_error(&without_client),
                    RowProblem::BadValue {
                        field: "client",
                        ..
                    }
                ),
                "{name}"
            );
        }
    }

    let window = table
        .provider(ProviderKind::OpenAi)
        .unwrap()
        .codex_window
        .unwrap();
    let original = format!("[providers.openai.codex]\nwindow = {window}");
    assert!(SHIPPED.contains(&original));
    let changed = SHIPPED.replace(&original, "[providers.openai.codex]\nwindow = 160000");
    let changed = ModelTable::parse_shipped(&changed).unwrap();
    assert_eq!(
        changed.provider(ProviderKind::OpenAi).unwrap().codex_window,
        Some(160_000)
    );
    let version_only = SHIPPED.replace(&original, "[providers.openai.codex]\nversion = '1.2.3'");
    assert!(ModelTable::parse_shipped(&version_only).is_err());
    let too_small = SHIPPED.replace(&original, "[providers.openai.codex]\nwindow = 103999");
    assert!(matches!(
        shipped_error(&too_small),
        RowProblem::LimitTooSmall(_)
    ));
}

fn models_requiring_newer_codex(
    json: &str,
    shipped_version: &str,
) -> Result<Vec<(String, String)>, String> {
    let shipped = Version::parse(shipped_version)
        .ok_or_else(|| format!("invalid shipped Codex version: {shipped_version:?}"))?;
    let catalog: Value = serde_json::from_str(json).map_err(|err| err.to_string())?;
    let models = catalog
        .get("models")
        .and_then(Value::as_array)
        .ok_or("expected a models array")?;
    let mut incompatible = Vec::new();
    for model in models {
        let slug = model
            .get("slug")
            .and_then(Value::as_str)
            .ok_or("expected a model slug")?;
        let version = model
            .get("minimal_client_version")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{slug}: expected a minimal_client_version string"))?;
        let minimum = Version::parse(version)
            .ok_or_else(|| format!("{slug}: invalid minimal_client_version {version:?}"))?;
        if minimum > shipped {
            incompatible.push((slug.to_owned(), version.to_owned()));
        }
    }
    Ok(incompatible)
}

#[test]
fn codex_catalog_comparison_selects_only_models_above_the_shipped_version() {
    let catalog = r#"{"models": [
        {"slug": "above", "minimal_client_version": "0.99.1"},
        {"slug": "equal", "minimal_client_version": "0.99.0"},
        {"slug": "below", "minimal_client_version": "0.98.0"},
        {"slug": "numeric", "minimal_client_version": "0.100.0"}
    ]}"#;
    let incompatible = models_requiring_newer_codex(catalog, "0.99.0").unwrap();
    assert_eq!(
        incompatible,
        [
            ("above".to_owned(), "0.99.1".to_owned()),
            ("numeric".to_owned(), "0.100.0".to_owned())
        ]
    );
}

#[test]
fn codex_catalog_comparison_rejects_unexpected_shapes() {
    for catalog in [
        r#"{}"#,
        r#"{"models": {}}"#,
        r#"{"models": [["not-an-object", "0.1.0"]]}"#,
        r#"{"models": [{"slug": "missing-version"}]}"#,
        r#"{"models": [{"minimal_client_version": "0.1.0"}]}"#,
        r#"{"models": [{"slug": "short-version", "minimal_client_version": "0.1"}]}"#,
        r#"{"models": [{"slug": "long-version", "minimal_client_version": "0.1.0.0"}]}"#,
        r#"{"models": [{"slug": "not-numeric", "minimal_client_version": "0.1.+0"}]}"#,
    ] {
        assert!(
            models_requiring_newer_codex(catalog, "0.99.0").is_err(),
            "{catalog}"
        );
    }
}

async fn fetch_codex_check_input(client: &reqwest::Client, url: &str) -> String {
    let response = client
        .get(url)
        .send()
        .await
        .unwrap_or_else(|err| panic!("could not reach {url}: {err}"));
    assert!(
        response.status().is_success(),
        "{url} returned {}",
        response.status()
    );
    response
        .text()
        .await
        .unwrap_or_else(|err| panic!("could not read {url}: {err}"))
}

#[tokio::test]
#[ignore = "network: checks the latest public Codex release on GitHub"]
async fn shipped_codex_version_supports_the_latest_catalog() {
    let client = reqwest::Client::builder()
        .user_agent("Wisp-Codex-Version-Check")
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("build GitHub client");
    let release = fetch_codex_check_input(
        &client,
        "https://api.github.com/repos/openai/codex/releases/latest",
    )
    .await;
    let release: Value =
        serde_json::from_str(&release).expect("invalid JSON for GitHub's latest Codex release");
    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .expect("expected tag_name for GitHub's latest Codex release");
    assert!(
        tag.strip_prefix("rust-v")
            .and_then(Version::parse)
            .is_some(),
        "expected a Codex release tag like rust-vX.Y.Z, got {tag:?}"
    );
    let url = format!(
        "https://raw.githubusercontent.com/openai/codex/{tag}/codex-rs/models-manager/models.json"
    );
    let catalog = fetch_codex_check_input(&client, &url).await;
    let shipped = ModelTable::shared()
        .provider(ProviderKind::OpenAi)
        .and_then(|row| row.client)
        .expect("shipped OpenAI Codex version")
        .version;
    let incompatible = models_requiring_newer_codex(&catalog, &shipped.to_string())
        .unwrap_or_else(|err| panic!("could not check the Codex catalog at {tag}: {err}"));
    assert!(
        incompatible.is_empty(),
        "Codex {tag} models above shipped client version {shipped}: {}",
        incompatible
            .iter()
            .map(|(slug, version)| format!("{slug} ({version})"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    eprintln!("Codex {tag}: all models support shipped client version {shipped}");
}
