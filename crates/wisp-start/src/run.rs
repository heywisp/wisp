use std::path::Path;

use wisp_core::agent;
use wisp_core::agents::CheckedPresets;
use wisp_core::endpoints::{ConfigFile, EndpointBook, EndpointName, StartReason};
use wisp_core::kit;
use wisp_core::provider::RequestOptions;
use wisp_core::settings::Given;
use wisp_core::settings::host::StartLayers;
use wisp_protocol::agents::Preset;
use wisp_protocol::settings::{SettingValue, names};
use wisp_protocol::{CacheTtl, Effort, MessageTime};

use crate::paths::{home_dir, models_folder, xdg_config_home};
use crate::{agents, endpoints, instructions, keychain, settings};

#[derive(Clone)]
pub struct Setup {
    pub flags: Vec<Given>,
    pub endpoint: Option<String>,
}

impl Setup {
    pub fn value(&self, name: &str) -> Option<&SettingValue> {
        settings::flag_value(&self.flags, name)
    }

    pub fn with_model(&self, model: Option<&str>) -> Setup {
        let mut flags = self.flags.clone();
        flags.retain_mut(|given| {
            let flagged = settings::flag_value(std::slice::from_ref(given), names::MODEL).is_some();
            if let (true, Some(model)) = (flagged, model) {
                given.value = SettingValue::Text(model.to_owned());
            }
            !flagged || model.is_some()
        });
        Setup {
            flags,
            endpoint: self.endpoint.clone(),
        }
    }
}

pub fn load_keychain(config: Option<&Path>, setup: &Setup) -> Result<keychain::Keychain, String> {
    use std::io::IsTerminal;
    load_keychain_on(
        config,
        setup,
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        Box::new(|| eprintln!("{}", keychain::WAITING)),
    )
}

pub fn load_keychain_on(
    config: Option<&Path>,
    setup: &Setup,
    terminal: bool,
    notice: wisp_core::secrets::keyring::Notice,
) -> Result<keychain::Keychain, String> {
    let flag = setup.flags.iter().find_map(|given| {
        (given.target.spec().name == names::KEYCHAIN)
            .then(|| keychain::flag_setting(&given.value).map(|s| (s, given.origin.clone())))
            .flatten()
    });
    let variable = keychain_variable();
    let config = match endpoints::config_path(
        wisp_protocol::CONFIG_FILE,
        config,
        xdg_config_home(),
        home_dir(),
    ) {
        Some(path) => endpoints::read_config(&path, config.is_some()),
        None => Ok(None),
    };
    let setting = keychain::config_setting(config.as_ref().map(Option::as_ref).map_err(|_| ()));
    keychain::Keychain::load(flag, variable.as_deref(), setting, terminal, notice)
}

pub fn env_given(name: &str, value: SettingValue, origin: String) -> Option<Given> {
    let spec = wisp_protocol::settings::find(name)?;
    Some(Given {
        target: wisp_protocol::settings::Target::Name(spec),
        value,
        origin,
    })
}

pub fn display_env() -> Vec<Given> {
    let mut out = Vec::new();
    for spec in wisp_protocol::settings::SETTINGS
        .iter()
        .filter(|s| s.group() == Some("display"))
    {
        for var in spec.env {
            let Some(raw) = crate::paths::real_env(var) else {
                continue;
            };
            let lower = raw.trim().to_ascii_lowercase();
            let text = match lower.as_str() {
                "1" | "true" | "yes" => "on",
                "0" | "false" | "no" => "off",
                other => wisp_protocol::settings::color_word(other),
            };
            if let Ok(value) = spec.parse(text)
                && let Some(given) = env_given(spec.name, value, (*var).to_owned())
            {
                out.push(given);
            }
        }
    }
    out
}

pub const KEY_EXPORTED: &str = "a provider key is set in the environment";
pub const NOT_AN_ENDPOINT: &str = "(not an account name, not shown)";

pub fn key_set_in(variable: impl std::fmt::Display, source: impl std::fmt::Display) -> String {
    format!("{variable} is set in {source}")
}

pub fn exported_origin(sources: &endpoints::Sources) -> Option<String> {
    let keychain = keychain::keychain(&sources.inputs).ok()?;
    let variables = keychain.variables()?;
    if !wisp_core::endpoints::exported(variables) {
        return None;
    }
    Some(match wisp_core::endpoints::exported_key(variables) {
        Some((variable, source)) => key_set_in(variable, source),
        None => KEY_EXPORTED.to_owned(),
    })
}

pub fn shown_choice(name: &str, known: &[&str]) -> String {
    if known.contains(&name) {
        name.to_owned()
    } else if wisp_protocol::looks_like_key(name) {
        wisp_protocol::NOT_SHOWN.to_owned()
    } else if EndpointName::parse(name).is_none() {
        NOT_AN_ENDPOINT.to_owned()
    } else {
        name.to_owned()
    }
}

pub fn configured(config: Option<&ConfigFile>) -> Vec<&str> {
    config
        .into_iter()
        .flat_map(|c| c.endpoints.keys().map(EndpointName::as_str))
        .collect()
}

pub fn shown_flags(flags: &[Given], known: &[&str]) -> Vec<Given> {
    flags
        .iter()
        .cloned()
        .map(|mut given| {
            if given.target.spec().name == names::ENDPOINT {
                given.value = SettingValue::Text(shown_choice(&given.value.text(), known));
            }
            given
        })
        .collect()
}

pub fn keychain_variable() -> Option<String> {
    std::env::var("WISP_KEYCHAIN").ok()
}

pub fn start_env(
    sources: &endpoints::Sources,
    book: Option<&EndpointBook>,
    keychain: &keychain::Keychain,
    keychain_variable: Option<&str>,
) -> Vec<Given> {
    use wisp_core::secrets::detect::{KeychainSetting, VARIABLE};
    let mut env = display_env();
    let choice = keychain.choice();
    let from_variable = if choice.origin.as_deref() == Some(VARIABLE) {
        Some(choice.setting)
    } else {
        keychain_variable.and_then(KeychainSetting::parse)
    };
    if let Some(setting) = from_variable
        && let Some(given) = env_given(
            names::KEYCHAIN,
            SettingValue::Text(setting.as_str().to_owned()),
            VARIABLE.to_owned(),
        )
    {
        env.push(given);
    }
    let mut endpoint = |name: String, origin: String| {
        if let Some(given) = env_given(names::ENDPOINT, SettingValue::Text(name), origin) {
            env.push(given);
        }
    };
    let Some(book) = book else {
        if let Some(chosen) = &sources.variable {
            let known = configured(sources.config.as_ref().ok().and_then(Option::as_ref));
            endpoint(
                shown_choice(&chosen.name, &known),
                chosen.variable.to_owned(),
            );
        }
        return env;
    };
    match (book.start_reason, &book.overridden, &sources.variable) {
        (StartReason::Variable, _, chosen) => {
            let origin = chosen
                .as_ref()
                .map_or(endpoints::CHOICE_VAR, |c| c.variable);
            endpoint(book.start.to_string(), origin.to_owned());
        }
        (StartReason::Flag, _, Some(chosen)) => {
            let known: Vec<&str> = book.entries.keys().map(EndpointName::as_str).collect();
            endpoint(
                shown_choice(&chosen.name, &known),
                chosen.variable.to_owned(),
            );
        }
        (StartReason::Exported, Some(over), _) => endpoint(
            book.start.to_string(),
            key_set_in(&over.variable, &over.source),
        ),
        (StartReason::Exported, None, _) => endpoint(
            book.start.to_string(),
            exported_origin(sources).unwrap_or_else(|| KEY_EXPORTED.to_owned()),
        ),
        _ => {}
    }
    if matches!(book.start_reason, StartReason::Flag | StartReason::Variable)
        && let Some(origin) = exported_origin(sources)
    {
        let env = wisp_core::endpoints::EndpointName::env();
        endpoint(
            wisp_core::endpoints::account(&book.entries, &env).to_string(),
            origin,
        );
    }
    env
}

pub fn start_layers(
    setup: &Setup,
    sources: &endpoints::Sources,
    book: &EndpointBook,
    keychain: &keychain::Keychain,
    keychain_variable: Option<&str>,
    endpoints: Vec<wisp_protocol::settings::EndpointSummary>,
    full_screen: bool,
) -> StartLayers {
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    StartLayers {
        remembered: Default::default(),
        forced: Vec::new(),
        flags: setup.flags.clone(),
        env: start_env(sources, Some(book), keychain, keychain_variable),
        saved: config.map(|c| c.settings.clone()).unwrap_or_default(),
        endpoints,
        models: Vec::new(),
        full_screen,
    }
}

pub fn start_summaries(
    sources: &endpoints::Sources,
    book: &EndpointBook,
    refused: &[(String, String)],
    ready: &[agent::Connection],
) -> Vec<wisp_protocol::settings::EndpointSummary> {
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    let default = config.and_then(|c| c.default.as_ref()).map(|d| d.as_str());
    endpoints::summaries(&book.entries, &book.marks, refused, default, ready)
}

pub struct StartPreset<'a> {
    pub preset: &'a Preset,
    pub account: Option<&'a wisp_core::agents::endpoints::Ready>,
}

pub struct Run {
    pub model: String,
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
    pub time: MessageTime,
    pub context_note: bool,
    pub auto_compact: bool,
    pub claude_usage: bool,
    pub count_tokens: bool,
    pub options: RequestOptions,
    pub retry_budget: Option<f64>,
    pub log: bool,
    pub verbose: bool,
    pub tools: Option<Vec<String>>,
    pub turned_off: Vec<String>,
    pub preload: Vec<String>,
    pub preset: Option<String>,
    pub instructions: instructions::Settings,
    pub instructions_extra: Option<wisp_core::instructions::Extra>,
    pub flags: Vec<Given>,
}

pub fn choice<T: Copy>(
    value: Option<&SettingValue>,
    all: &[T],
    name: fn(T) -> &'static str,
    fallback: T,
) -> T {
    value
        .map(SettingValue::text)
        .and_then(|text| all.iter().copied().find(|v| name(*v) == text))
        .unwrap_or(fallback)
}

impl Run {
    pub fn chosen<'a>(
        &self,
        session: &'a wisp_protocol::SessionConfig,
        endpoint: wisp_render::chrome::Endpoint,
    ) -> crate::models::Chosen<'a> {
        crate::models::Chosen {
            model: &session.model,
            effort: session.effort.as_str(),
            cache_ttl: session.cache_ttl,
            endpoint,
            max_tokens: session.max_tokens,
            claude_usage: self.claude_usage,
        }
    }

    pub fn resolve(setup: &Setup, sources: &endpoints::Sources, endpoint: Option<&str>) -> Self {
        let config = sources.config.as_ref().ok().and_then(Option::as_ref);
        let live_default = endpoint.and_then(|name| {
            let entries = sources.entries().ok()?;
            let (_, wisp_core::endpoints::Availability::Ready(resolved)) =
                entries.iter().find(|(k, _)| k.as_str() == name)?
            else {
                return None;
            };
            (resolved.provider != wisp_core::endpoints::ProviderKind::Anthropic)
                .then(|| resolved.default_model(models_folder().as_deref()))
        });
        Self::resolve_with(setup, config, endpoint, None, None, live_default)
    }

    pub fn resolve_on(
        setup: &Setup,
        config: Option<&wisp_core::endpoints::ConfigFile>,
        endpoint: Option<&str>,
        start: Option<StartPreset<'_>>,
    ) -> Self {
        let preset = start.as_ref().map(|s| s.preset);
        let account = start.as_ref().and_then(|s| s.account);
        Self::resolve_with(setup, config, endpoint, preset, account, None)
    }

    pub fn resolve_with(
        setup: &Setup,
        config: Option<&wisp_core::endpoints::ConfigFile>,
        endpoint: Option<&str>,
        preset: Option<&Preset>,
        account: Option<&wisp_core::agents::endpoints::Ready>,
        live_default: Option<String>,
    ) -> Self {
        let saved = config.map(|c| &c.settings);
        let configured_model = settings::resolved_model(&setup.flags, saved);
        let explicit = settings::flag_value(&setup.flags, names::MODEL).is_some()
            || saved.is_some_and(|saved| {
                saved
                    .iter()
                    .any(|(path, _)| path.spec().name == names::MODEL)
            });
        let account_default = account
            .filter(|a| a.kind() != wisp_core::endpoints::ProviderKind::Anthropic)
            .map(|a| a.provider.default_model())
            .or(live_default)
            .or_else(|| {
                let entry = endpoint.and_then(|name| {
                    config?
                        .endpoints
                        .iter()
                        .find(|(key, _)| key.as_str() == name)
                        .map(|(_, value)| value)
                })?;
                (entry.provider != wisp_core::endpoints::ProviderKind::Anthropic)
                    .then(|| entry.default_model().to_owned())
            });
        let model = match preset.and_then(|p| p.model.clone()) {
            Some(model) if !explicit => model,
            _ if !explicit && let Some(default) = account_default => default,
            _ => configured_model,
        };
        let layers = wisp_core::settings::Layers {
            flags: setup.flags.clone(),
            saved,
            ..Default::default()
        };
        let current = wisp_core::settings::Current {
            endpoint: endpoint.map(str::to_owned),
            model: model.clone(),
        };
        let resolved = wisp_core::settings::resolve(&layers, &current);
        let get = |name: &str| resolved.get(name).and_then(|r| r.value());
        let on = |name: &str, fallback: bool| match get(name) {
            Some(SettingValue::Bool(b)) => *b,
            _ => fallback,
        };
        let retry_budget = match get(names::RETRY_BUDGET_USD) {
            Some(SettingValue::Usd { micros }) => Some(micros_to_usd(*micros)),
            _ => None,
        };
        let tools = match get(names::TOOLS) {
            Some(SettingValue::List(names)) => Some(names.clone()),
            _ => None,
        };
        let source = get(names::INSTRUCTIONS_SOURCE)
            .map(SettingValue::text)
            .and_then(|t| instructions::InstructionsArg::named(&t));
        let warn = match get(names::INSTRUCTIONS_WARN_TOKENS) {
            Some(SettingValue::Tokens(n)) => Some(*n),
            _ => None,
        };
        let defaulted = |name: &str| {
            resolved
                .get(name)
                .and_then(|r| r.winner())
                .is_none_or(|f| f.layer == wisp_core::settings::Layer::Default)
        };
        let takes = |effort: Effort| {
            account
                .and_then(|a| a.row(&model))
                .is_none_or(|row| row.takes_effort(effort))
        };
        Self {
            effort: match preset.and_then(|p| p.effort) {
                Some(effort) if defaulted(names::EFFORT) && takes(effort) => effort,
                _ => choice(
                    get(names::EFFORT),
                    &Effort::ALL,
                    Effort::as_str,
                    Effort::Medium,
                ),
            },
            cache_ttl: choice(
                get(names::CACHE_TTL),
                &CacheTtl::ALL,
                CacheTtl::as_str,
                CacheTtl::OneHour,
            ),
            time: choice(
                get(names::TIME),
                &MessageTime::ALL,
                MessageTime::as_str,
                MessageTime::Local,
            ),
            context_note: on(names::CONTEXT_CONTEXT_NOTE, true),
            auto_compact: on(names::CONTEXT_AUTO_COMPACT, true),
            claude_usage: on(names::CLAUDE_USAGE, false),
            flags: setup.flags.clone(),
            count_tokens: on(names::CONTEXT_COUNT_TOKENS, true),
            options: RequestOptions {
                refusal_fallback: on(names::REFUSAL_FALLBACK, true),
                debug_cache: on(names::DEBUG_CACHE, false),
                message_threads: on(names::MESSAGE_THREADS, true),
            },
            retry_budget,
            log: on(names::LOG, true),
            verbose: on(names::DISPLAY_VERBOSE, false),
            tools,
            turned_off: match get(names::TURNED_OFF) {
                Some(SettingValue::List(names)) => names.clone(),
                _ => Vec::new(),
            },
            preload: wisp_protocol::settings::resolve_preload(
                preset.and_then(|preset| preset.preload.as_deref()),
                match get(names::PRELOAD) {
                    Some(SettingValue::List(names)) => Some(names),
                    _ => None,
                },
            ),
            preset: get(names::PRESET).map(SettingValue::text),
            instructions: instructions::settings(source, warn, None),
            instructions_extra: get(names::INSTRUCTIONS_EXTRA)
                .map(SettingValue::text)
                .and_then(|t| wisp_core::instructions::Extra::named(&t)),
            model,
        }
    }
}

pub fn micros_to_usd(micros: u64) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a dollar budget far below 2^53 micros"
    )]
    let value = micros as f64;
    value / 1e6
}

pub fn marked_in(marks: &std::collections::BTreeMap<EndpointName, u8>, name: &str) -> Option<u8> {
    EndpointName::parse(name).and_then(|name| marks.get(&name).copied())
}

pub fn saved_layers<'a>(
    setup: &Setup,
    sources: &'a endpoints::Sources,
) -> wisp_core::settings::Layers<'a> {
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    wisp_core::settings::Layers {
        flags: setup.flags.clone(),
        saved: config.map(|c| &c.settings),
        ..Default::default()
    }
}

pub fn start_presets(
    sources: &endpoints::Sources,
    view: &wisp_core::agents::endpoints::EndpointView,
    warn_tokens: u64,
    extra: wisp_core::agents::Presets,
) -> CheckedPresets {
    let place = preset_place(sources, warn_tokens);
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    let presets = config.map(|c| c.presets.clone()).unwrap_or_default();
    agents::checked(presets.joined(extra), Some(view), &place)
}

pub fn named_preset<'a>(
    presets: &'a CheckedPresets,
    name: Option<&str>,
) -> Result<Option<&'a Preset>, String> {
    name.map(|name| presets.named(name).map_err(|e| format!("wisp: {e}")))
        .transpose()
}

pub fn preset_place(
    sources: &endpoints::Sources,
    warn_tokens: u64,
) -> wisp_core::agents::PresetPlace {
    wisp_core::agents::PresetPlace::of_config(
        sources.config_path.as_deref(),
        home_dir().filter(|h| h.is_absolute()),
        warn_tokens,
    )
}

pub fn tree_setup(
    setup: &Setup,
    sources: &endpoints::Sources,
    model: &str,
    instructions: kit::InstructionsChoice,
    presets: CheckedPresets,
    restarted: bool,
) -> wisp_core::agents::TreeSetup {
    let layers = saved_layers(setup, sources);
    let current = wisp_core::settings::Current {
        endpoint: setup.endpoint.clone(),
        model: model.to_owned(),
    };
    wisp_core::agents::TreeSetup {
        presets,
        tools_off: agents::tools_off_names(),
        followers: agents::tools_off_followers(),
        limits: wisp_core::agents::Limits::resolve(&layers, &current),
        instructions,
        loaded: wisp_core::agents::LoadedTree::default(),
        keep_warm_main: wisp_core::agents::keep_warm_main(&layers, &current),
        restarted,
        start: None,
        unusable: Vec::new(),
    }
}
