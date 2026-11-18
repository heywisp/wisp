use std::collections::BTreeSet;
use std::path::Path;
use wisp_core::agent::{Connection, DEFAULT_MAX_TOKENS, listed_row};
use wisp_core::context::{ModelSettings, compact_at, group, listed_only_warning};

use crate::models::rates_of;
use wisp_core::config::SavedSettings;
use wisp_core::endpoints::{ConfigFile, PathShown, ProviderKind};
use wisp_core::models::{ConfigModels, ModelTable};
use wisp_core::search::{NoSearch, SearchFacts, SearchForm};
use wisp_core::settings::{Current, Given, Layers, background_band};
use wisp_protocol::context::unknown_window;
use wisp_protocol::settings::{self, names};
use wisp_protocol::settings::{Place, SettingValue, Target};
use wisp_protocol::{CONFIG_FILE, CacheTtl};
use wisp_render::chrome;
use wisp_render::chrome::context::{point_reason, window_text};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};

pub const SHIPPED_BROKEN: &str = "the shipped model file didn't load, so no window is known";

pub struct Loaded {
    pub settings: ModelSettings,
    pub saved: SavedSettings,
    pub warnings: Vec<String>,
    pub refused: Option<String>,
    pub named: BTreeSet<String>,
    pub flags: Vec<Given>,
}

impl Loaded {
    pub fn with_flags(mut self, flags: &[Given]) -> Self {
        self.settings = self.settings.with_flags(flags);
        self.warnings.extend(self.settings.flag_warnings());
        self.flags = flags.to_vec();
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub max_tokens: u32,
    pub auto_compact: bool,
    pub cache_ttl: CacheTtl,
}

pub fn load(path: Option<&Path>) -> Loaded {
    let text = path.and_then(|p| std::fs::read_to_string(p).ok());
    load_text(text.as_deref())
}

pub fn load_text(config: Option<&str>) -> Loaded {
    let mut warnings = vec![];
    let mut refused = None;
    let shipped = ModelTable::shipped().unwrap_or_else(|e| {
        warnings.push(format!("{SHIPPED_BROKEN}: {e}"));
        ModelTable::default()
    });
    let models = match config.map(ConfigModels::parse) {
        None => ConfigModels::default(),
        Some(Ok(models)) => models,
        Some(Err(e)) => {
            refused = Some(match e.line {
                Some(line) => format!("{CONFIG_FILE} line {line}: {}", e.problem),
                None => format!("{CONFIG_FILE}: {}", e.problem),
            });
            ConfigModels::default()
        }
    };
    let settings = ModelSettings::new(shipped, models);
    let named = settings.named_models();
    warnings.extend(settings.warnings());
    let saved = config
        .and_then(|text| ConfigFile::parse(Path::new(CONFIG_FILE), PathShown::Default, text).ok())
        .map(|file| file.settings)
        .unwrap_or_default();
    Loaded {
        settings,
        saved,
        warnings,
        refused,
        named,
        flags: Vec::new(),
    }
}

pub fn unknown_places(loaded: &Loaded, endpoints: &[&str], model: &str) -> Vec<String> {
    let known_model = |id: &str| {
        id == model
            || loaded.named.iter().any(|n| n == id)
            || wisp_core::models::ModelTable::shared().find(id).is_some()
            || loaded.flags.iter().any(|g| {
                matches!(
                    (&g.target, &g.value),
                    (Target::Path(p), SettingValue::List(aliases))
                        if p.spec().name == names::ALIASES && aliases.iter().any(|a| a == id)
                )
            })
    };
    let mut out: Vec<String> = Vec::new();
    for given in &loaded.flags {
        let Target::Path(path) = &given.target else {
            continue;
        };
        let missing = match &path.place {
            Place::Endpoint(e) | Place::EndpointModel { endpoint: e, .. }
                if !endpoints.contains(&e.as_str()) =>
            {
                Some("account")
            }
            Place::Model(m) | Place::EndpointModel { model: m, .. } if !known_model(m) => {
                Some("model")
            }
            _ => None,
        };
        if let Some(what) = missing {
            let line = format!("{} names no {what} Wisp knows", given.origin);
            if !out.contains(&line) {
                out.push(line);
            }
        }
    }
    out
}

pub fn start_warnings(
    loaded: &Loaded,
    endpoint: &str,
    model: &str,
    ready: &[Connection],
) -> Vec<String> {
    let mut out = loaded.warnings.clone();
    let listed = listed_row(ready, endpoint, model);
    let resolved = loaded
        .settings
        .resolve(endpoint, model, listed.as_ref(), DEFAULT_MAX_TOKENS);
    if resolved.window.is_none() {
        out.push(unknown_window(model));
    }
    if let Some(conn) = ready.iter().find(|c| c.name == endpoint)
        && let Some(note) = listed_only_warning(
            conn.provider.as_ref(),
            model,
            loaded.settings.named_models().contains(model),
        )
    {
        out.push(note);
    }
    out
}

fn reply_limit(
    loaded: &Loaded,
    ready: &[Connection],
    endpoint: &str,
    model: &str,
    max_tokens: u32,
) -> u32 {
    let documented = ready
        .iter()
        .find(|conn| conn.name == endpoint)
        .and_then(|conn| conn.provider.capabilities(model))
        .and_then(|caps| caps.max_output);
    loaded
        .settings
        .reply_limit(endpoint, model, documented, max_tokens)
}

pub fn point_warnings(
    loaded: &Loaded,
    endpoint: &str,
    model: &str,
    run: Run,
    ready: &[Connection],
) -> Vec<String> {
    let layers = Layers {
        session: Vec::new(),
        flags: loaded.flags.clone(),
        env: Vec::new(),
        saved: Some(&loaded.saved),
        shipped: None,
    };
    let band = |endpoint: &str, model: &str| {
        let current = Current {
            endpoint: Some(endpoint.to_owned()),
            model: model.to_owned(),
        };
        run.auto_compact
            .then(|| background_band(&layers, &current))
            .flatten()
    };
    let listed = |endpoint: &str, model: &str| listed_row(ready, endpoint, model);
    let reply =
        |endpoint: &str, model: &str| reply_limit(loaded, ready, endpoint, model, run.max_tokens);
    loaded
        .settings
        .low_points(endpoint, model, &reply, run.cache_ttl, &band, &listed)
}

pub fn doctor_rows(
    loaded: &Loaded,
    endpoint: &str,
    model: &str,
    run: Run,
    ready: &[Connection],
) -> Vec<(&'static str, String)> {
    let max_tokens = reply_limit(loaded, ready, endpoint, model, run.max_tokens);
    let listed = listed_row(ready, endpoint, model);
    let resolved = loaded
        .settings
        .resolve(endpoint, model, listed.as_ref(), max_tokens);
    let mut rows = vec![("model", wisp_protocol::shown_model(model).to_owned())];
    match &resolved.window {
        Some(window) => {
            rows.push(("window", window_text(window)));
            let point = compact_at(window.tokens, max_tokens, resolved.compact_at);
            let (verb, off) = if run.auto_compact {
                ("compacts at", "")
            } else {
                ("would compact at", "; automatic compaction is off")
            };
            rows.push((
                "compaction",
                format!(
                    "{verb} {} ({}); reserve {}{off}",
                    group(point.tokens),
                    point_reason(&point, &Glyphs::of(GlyphSet::Unicode), group),
                    group(settings::reserve(window.tokens, max_tokens))
                ),
            ));
        }
        None => rows.push(("window", unknown_window(model))),
    }
    rows.extend(loaded.refused.iter().map(|e| ("error", e.clone())));
    rows.extend(loaded.warnings.iter().map(|w| ("warning", w.clone())));
    let low = point_warnings(loaded, endpoint, model, run, ready);
    rows.extend(low.into_iter().map(|w| ("warning", w)));
    rows
}

pub fn doctor_text(rows: &[(&'static str, String)]) -> String {
    let mut text = String::from("\ncontext\n");
    for (key, value) in rows {
        let value = wisp_render::text::clean(value);
        text.push_str(&format!("  {key:<18} {value}\n"));
    }
    text
}

pub fn search_rows(
    loaded: &Loaded,
    endpoints: &[(String, ProviderKind)],
    model: &str,
    requested: bool,
    ready: &[Connection],
) -> Vec<(String, String)> {
    let mut rows = vec![(
        "model".to_owned(),
        wisp_protocol::shown_model(model).to_owned(),
    )];
    for (name, wire) in endpoints {
        let listed = listed_row(ready, name, model);
        let facts = SearchFacts::resolve(&loaded.settings, name, model, *wire, listed.as_ref());
        rows.push((
            name.clone(),
            format!(
                "{} (web_search: {}; native_web_search: {})",
                facts.form(requested, false).words(),
                facts.versions_from.source(),
                facts.opt_in_from().source()
            ),
        ));
    }
    rows
}

pub fn shown_form(form: SearchForm) -> Option<SearchForm> {
    (form != SearchForm::None(NoSearch::NotRequested)).then_some(form)
}

pub fn search_forms(
    loaded: &Loaded,
    endpoints: &[(String, ProviderKind)],
    models: &[String],
    requested: bool,
    chat_only: bool,
    kept: Option<(&str, &str, bool)>,
    ready: &[Connection],
) -> chrome::SearchForms {
    let mut ids: Vec<&String> = models.iter().chain(&loaded.named).collect();
    ids.sort();
    ids.dedup();
    let mut pairs = Vec::new();
    for (endpoint, wire) in endpoints {
        for model in &ids {
            let listed = listed_row(ready, endpoint, model);
            let facts =
                SearchFacts::resolve(&loaded.settings, endpoint, model, *wire, listed.as_ref());
            if let Some(form) = shown_form(facts.form(true, false)) {
                pairs.push(chrome::SearchPair {
                    endpoint: endpoint.clone(),
                    model: (*model).clone(),
                    form,
                });
            }
        }
    }
    chrome::SearchForms {
        requested,
        chat_only,
        pairs,
        refused: Vec::new(),
        kept: kept.map(|(endpoint, model, had)| (endpoint.to_owned(), model.to_owned(), had)),
    }
}

pub fn account_facts(
    settings: &ModelSettings,
    accounts: &[(String, ProviderKind)],
) -> Vec<chrome::AccountFacts> {
    let models = settings.named_models();
    let pairs = accounts
        .iter()
        .flat_map(|account| models.iter().map(move |m| (account, m)));
    pairs
        .map(|((account, kind), model)| chrome::AccountFacts {
            account: account.clone(),
            model: model.clone(),
            rates: settings.price(account, model, *kind).as_ref().map(rates_of),
            max_output: settings.max_output(account, model, None),
        })
        .collect()
}

pub fn search_text(rows: &[(String, String)]) -> String {
    let mut text = String::from("\nweb search\n");
    for (key, value) in rows {
        let key = wisp_render::text::clean(key);
        let value = wisp_render::text::clean(value);
        text.push_str(&format!("  {key:<18} {value}\n"));
    }
    text
}

#[cfg(test)]
mod tests;
