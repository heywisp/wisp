use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use wisp_client::{Channels, QuitBound};
use wisp_core::agent::{self, Agent, Connection};
use wisp_core::agents::{CheckedPresets, TreeSetup};
use wisp_core::endpoints::EndpointName;
use wisp_core::endpoints::catalog::LiveCatalog;
use wisp_core::endpoints::host::EndpointHost;
use wisp_core::endpoints::{EndpointBook, StartReason};
use wisp_core::kit::{self, BuildTools, Kit};
use wisp_core::plugin::{KernelHost, Prestarted, Problem, Tools};
use wisp_core::session::JsonlStore;
use wisp_core::settings::host::StartLayers;
use wisp_core::settings::{Given, START_ORIGIN};
use wisp_protocol::agents::Preset;
use wisp_protocol::opening::StartPick;
use wisp_protocol::settings::{SettingValue, names};
use wisp_protocol::subscription_auth::Unusable;
use wisp_protocol::{CacheTtl, Effort, Opening, SessionView};
use wisp_render::chrome::account::Said;
use wisp_render::chrome::{ModelFacts, named_model};

use crate::keychain::Keychain;
use crate::paths::{home_dir, models_folder, usage_cache, xdg_config_home, xdg_data_home};
use crate::resume::{self, Left, Question};
use crate::run::{
    Run, Setup, env_given, keychain_variable, named_preset, preset_place, saved_layers,
    start_layers, start_presets, start_summaries, tree_setup,
};
use crate::tools::{self, ToolRule};
use crate::watch::SourcesWatch;
use crate::{context, endpoints, instructions, models, settings};

pub struct Reopened {
    pub unlocked: Option<String>,
    pub path: PathBuf,
    pub recovered: wisp_core::resume::Recovered,
    pub logged: Option<wisp_core::resume::LoggedSession>,
    pub warnings: Vec<String>,
}

pub struct StartAt {
    pub account: String,
    pub follow: Vec<String>,
}

pub struct Inputs<'a> {
    pub config: Option<&'a Path>,
    pub mcp_config: Option<&'a Path>,
    pub env_file: Option<&'a Path>,
    pub keychain: &'a Arc<Keychain>,
    pub setup: &'a Setup,
    pub resume: Option<resume::Request<'a>>,
    pub session_id: Option<&'a str>,
    pub at: Option<&'a StartAt>,
    pub asks: bool,
    pub tool_rule: ToolRule,
    pub full_screen: bool,
    pub restarted: bool,
    pub forced: Vec<Given>,
    pub voice: bool,
    pub sessions: Option<PathBuf>,
    pub presets: wisp_core::agents::Presets,
    pub picked: Picked,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Picked {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

pub struct Opened<'a> {
    pub inputs: Inputs<'a>,
    pub setup: Setup,
    pub sources: endpoints::Sources,
    pub lists: Option<PathBuf>,
    pub listed: Vec<String>,
    pub new_chats: Option<String>,
    pub start_line: Option<String>,
    pub start_pick: Option<StartPick>,
    pub resolved: Run,
    pub presets: wisp_core::agents::CheckedPresets,
    pub preset: Option<Preset>,
    pub refused: bool,
    pub book: EndpointBook,
    pub models: context::Loaded,
    pub resumed: Option<resume::Resumed>,
    pub moved: Option<(String, Left)>,
    pub gone: Option<(String, Left)>,
    pub said: Option<Said>,
    pub client_warnings: Vec<String>,
    pub unpicked: Option<String>,
    catalog: Option<LiveCatalog>,
}

pub enum Stop {
    Failed(String),
    Ask(Box<Asking>),
}

impl From<String> for Stop {
    fn from(text: String) -> Self {
        Self::Failed(text)
    }
}

pub struct Asking {
    pub question: Question,
    pub book: EndpointBook,
    pub model: String,
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
    pub size: Option<u64>,
    pub on_accounts: Vec<wisp_render::chrome::AccountFacts>,
}

pub type Host = Box<dyn FnOnce(LiveCatalog) -> Arc<dyn EndpointHost> + Send>;

pub struct Accounts {
    pub unusable: Vec<Unusable>,
    pub reaches: Box<dyn Fn(&str) -> bool + Send>,
}

pub struct Built {
    pub kit: Kit,
    pub tree: TreeSetup,
    pub agent: Agent,
    pub sources: endpoints::Sources,
    pub setup: Setup,
    pub resolved: Run,
    pub book: EndpointBook,
    pub reopened: Option<Reopened>,
    pub continuing: Option<Left>,
    pub opening: Opening,
    pub warnings: Vec<String>,
    pub view: Option<SessionView>,
    pub voice: bool,
    pub unlocked: Option<String>,
    fresh_log: Option<wisp_core::session::FreshLog>,
}

impl Built {
    pub fn discard_unstarted_log(&mut self) {
        if let Some(fresh) = self.fresh_log.take() {
            fresh.discard_empty();
        }
    }
}

pub struct Started {
    pub channels: Channels,
    pub lane: Option<wisp_core::voice::AudioLane>,
    pub task: agent::AgentTask,
    pub view: Option<SessionView>,
    pub opening: Opening,
}

pub const SAVE_CUT: &str = concat!(
    "a settings save was still waiting for ",
    wisp_protocol::config_file!(),
    "; it may not be saved."
);

pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);
pub const FORCED_EXIT: u8 = 130;
pub const SIGNAL_LIMIT: Duration = wisp_core::jobs::SHUTDOWN_LIMIT
    .saturating_add(agent::QUIT_MARGIN)
    .saturating_add(SHUTDOWN_WAIT);

fn require_pin_log(inputs: &Inputs<'_>, log: bool) -> Result<(), Stop> {
    if !log && inputs.session_id.is_some() {
        let message = wisp_render::transcript::copy::SESSION_ID_NEEDS_LOG;
        return Err(format!("wisp: {message}").into());
    }
    Ok(())
}

pub async fn open(inputs: Inputs<'_>) -> Result<Opened<'_>, Stop> {
    wisp_core::version::set_commit(env!("WISP_COMMIT"));
    let setup = inputs.setup;
    require_pin_log(
        &inputs,
        !matches!(setup.value(names::LOG), Some(SettingValue::Bool(false))),
    )?;
    let at = inputs.at;
    let asks = inputs.asks;
    let on_tui = inputs.full_screen;
    let models = models::models(&[]);
    let typed = setup.value(names::MODEL).map(SettingValue::text);
    let named = typed
        .as_deref()
        .map(|typed| named_model(&models, typed).map_or(typed, |m| m.id.as_str()));
    let named = setup.with_model(named);
    let setup = &named;
    let mut sources = endpoints::Sources::load_with(
        &endpoints::Flags {
            env_file: inputs.env_file,
            config: inputs.config,
            endpoint: setup.endpoint.as_deref(),
            keychain: inputs.keychain,
            set: &setup.flags,
        },
        xdg_config_home(),
        home_dir(),
        at.is_some(),
    );
    if let Some(path) = inputs.mcp_config {
        sources
            .add_servers(path)
            .map_err(|err| format!("wisp: {err}"))?;
    }
    let unpicked = inputs.picked.account.clone().filter(|name| {
        let entries = sources.entries().unwrap_or_default();
        let config = sources.config.as_mut().ok().and_then(Option::as_mut);
        let named = EndpointName::parse(name).filter(|name| {
            config
                .as_ref()
                .is_some_and(|c| c.endpoints.contains_key(name))
                || (name.names_keys_file() && entries.contains_key(name))
        });
        match (named, config) {
            (Some(name), Some(config)) => config.default = Some(name),
            _ => return true,
        }
        false
    });
    require_mcp_search(&sources, || {
        let config = sources.config.as_ref().ok().and_then(Option::as_ref);
        let resolved = Run::resolve_with(setup, config, None, None, None, None);
        let name = resolved
            .preset
            .as_deref()
            .filter(|_| inputs.resume.is_none());
        let presets = config
            .map(|c| c.presets.clone())
            .unwrap_or_default()
            .joined(inputs.presets.clone());
        let preset = name.and_then(|name| {
            presets
                .usable()
                .into_iter()
                .find(|preset| preset.name == name)
        });
        selected_tools(&inputs, setup, &sources, &resolved, preset)
            .tools
            .is_enabled("search")
    })?;
    let lists = models_folder();
    let (missing, listed) = match (lists.as_deref(), sources.entries()) {
        (Some(folder), Ok(entries)) => {
            let due = endpoints::due(&entries, folder, jiff::Timestamp::now().as_second());
            (endpoints::fetches(due.missing, folder), due.listed)
        }
        _ => Default::default(),
    };
    let deferred_lists = if inputs.resume.is_some() && !sources.run_servers.is_empty() {
        Some(missing)
    } else {
        endpoints::fetch_due(missing, endpoints::LIST_WAIT).await;
        None
    };
    let entries = sources.entries().unwrap_or_default();
    let (mut start_ready, start_unavailable) = endpoints::connections(&entries, lists.as_deref());
    let mut models = models::models(&start_ready);
    let chose = sources.choice.is_some() || at.is_some();
    let gone = resume::start_gone(&sources, (asks || at.is_some()) && inputs.resume.is_none());
    let mut new_chats = sources.book().ok().map(|b| b.start.to_string());
    let at_name = at.map(|at| at.account.as_str());
    if let Some(name) = at_name {
        resume::move_to(&mut sources, name);
    }
    let mut start_line = None;
    let mut said = None;
    let mut asked = None;
    let mut gone_left = None;
    let start_failed = match gone {
        Some(selected) if inputs.resume.is_some() => {
            new_chats = selected.first().map(str::to_owned);
            if let Some(name) = new_chats.as_deref().filter(|_| at.is_none()) {
                resume::move_to(&mut sources, name);
            }
            true
        }
        Some(selected) => {
            match selected.settle(at_name, asks)? {
                resume::Settled::Go { name, left } => {
                    resume::move_to(&mut sources, &name);
                    if let Some(left) = left {
                        start_line = (!on_tui).then(|| left.continuing(&name));
                        gone_left = Some((name, left));
                    }
                }
                resume::Settled::Ask(question) => {
                    if let Some(first) = question.choices.first() {
                        resume::move_to(&mut sources, first);
                    }
                    asked = Some(question);
                }
            }
            false
        }
        None => false,
    };
    let explicit = typed.as_deref().filter(|_| {
        inputs.resume.is_none()
            && at.is_none()
            && asked.is_none()
            && !inputs.restarted
            && inputs.picked == Picked::default()
            && setup.flags.iter().any(|flag| {
                flag.origin != START_ORIGIN
                    && settings::flag_value(std::slice::from_ref(flag), names::MODEL).is_some()
            })
    });
    let catalog = match explicit.zip(lists.as_deref()) {
        Some((typed, folder)) => {
            refresh_start_model(
                &sources,
                chose,
                typed,
                &models,
                &start_ready,
                &start_unavailable,
                folder,
            )
            .await
        }
        None => None,
    };
    if let Some(catalog) = &catalog {
        start_ready = catalog.view().ready;
        models = models::models(&start_ready);
    }
    let canonical;
    let setup = if let Some(model) = explicit.and_then(|typed| named_model(&models, typed)) {
        canonical = setup.with_model(Some(&model.id));
        &canonical
    } else {
        setup
    };
    let mut start_pick = None;
    let moved;
    let model = setup.value(names::MODEL).map(SettingValue::text);
    let setup = match (model.as_deref(), at) {
        (Some(model), Some(at)) => {
            moved = setup.with_model(
                resume::model_on(&sources, &at.account, model, &models, &start_ready).as_deref(),
            );
            &moved
        }
        (Some(_), None) if !chose && asked.is_none() && inputs.resume.is_none() => {
            match typed
                .as_deref()
                .and_then(|typed| resume::start_model(&sources, typed, &models, &start_ready))
            {
                Some(resume::StartModel::Continue { name, left, note }) => {
                    resume::move_to(&mut sources, &name);
                    if !on_tui {
                        start_line = Some(
                            gone_left
                                .as_ref()
                                .map_or(&left, |(_, gone)| gone)
                                .continuing(&name),
                        );
                    }
                    said = on_tui.then_some(note);
                    setup
                }
                Some(resume::StartModel::Pick { pick, .. }) if asks => {
                    start_pick = Some(pick);
                    moved = setup.with_model(None);
                    &moved
                }
                Some(resume::StartModel::Pick { line, .. }) => return Err(line.into()),
                None => setup,
            }
        }
        _ => setup,
    };
    let mut start = sources.book().ok().map(|b| b.start.to_string());
    let start_view = endpoints::view(&entries, &start_ready, &start_unavailable);
    let with_picks = picks(
        setup,
        &inputs.picked,
        &sources,
        start.as_deref(),
        &start_view,
    );
    let setup = &with_picks;
    let resolved = Run::resolve(setup, &sources, start.as_deref());
    if let Some(question) = asked {
        let book = sources.resolve().map_err(|e| format!("wisp: {e}"))?;
        return Err(Stop::Ask(Box::new(Asking {
            question,
            book,
            model: resolved.model,
            effort: resolved.effort,
            cache_ttl: resolved.cache_ttl,
            size: None,
            on_accounts: Vec::new(),
        })));
    }
    let mut presets = start_presets(
        &sources,
        &start_view,
        resolved.instructions.warn_tokens,
        inputs.presets.clone(),
    );
    let mut preset = match inputs.resume {
        None => named_preset(&presets, resolved.preset.as_deref())?.cloned(),
        Some(_) => None,
    };
    if let Some(name) = preset.as_ref().and_then(|p| p.endpoint.as_deref())
        && sources
            .book()
            .ok()
            .is_none_or(|b| matches!(b.start_reason, StartReason::OnlyOne | StartReason::EnvFile))
    {
        resume::move_to(&mut sources, name);
        start = sources.book().ok().map(|b| b.start.to_string());
    }
    if let Some(preset) = preset.as_mut() {
        preset.model = preset
            .model
            .take()
            .filter(|model| resume::start_model(&sources, model, &models, &start_ready).is_none());
    }
    let resolved = Run::resolve_with(
        setup,
        sources.config.as_ref().ok().and_then(Option::as_ref),
        start.as_deref(),
        preset.as_ref(),
        start.as_deref().and_then(|name| start_view.ready(name)),
        None,
    );
    require_pin_log(&inputs, resolved.log)?;
    let refused = !relaunch_logs(setup, &sources, start.as_deref());
    let mut book = sources.resolve().map_err(|e| {
        let error = sources.start_failure(&e);
        if sources.sign_in_helps(&e) {
            endpoints::startup_recovery(error)
        } else {
            error
        }
    })?;
    let models = context::load(sources.config_path.as_deref()).with_flags(&resolved.flags);
    if let Some(problem) = &models.refused {
        return Err(format!("wisp: {problem}; nothing was loaded").into());
    }
    let resumed = match &inputs.resume {
        Some(request) if resolved.log => {
            let (ready, _) = endpoints::connections(&book.entries, lists.as_deref());
            let dir = inputs.sessions.clone();
            let start = book.start.as_str();
            let providers = |name: Option<&str>| {
                let wanted = name.unwrap_or(start);
                ready
                    .iter()
                    .find(|c| c.name == wanted)
                    .or_else(|| ready.iter().find(|c| c.name == start))
                    .map(|c| c.provider.clone())
            };
            let found = resume::open(dir, request, resume::here().as_deref(), &providers);
            Some(found.map_err(|e| format!("wisp: {e}"))?)
        }
        Some(_) => {
            return Err(
                format!("wisp: {}", wisp_render::transcript::copy::RESUME_NEEDS_LOG).into(),
            );
        }
        None => None,
    };
    if let Some(missing) = deferred_lists {
        require_mcp_search(&sources, || {
            let preset = session_preset(resumed.as_ref(), &presets, None);
            selected_tools(&inputs, setup, &sources, &resolved, preset)
                .tools
                .is_enabled("search")
        })?;
        if !missing.is_empty() {
            endpoints::fetch_due(missing, endpoints::LIST_WAIT).await;
            let (ready, unavailable) = endpoints::connections(&book.entries, lists.as_deref());
            let view = endpoints::view(&book.entries, &ready, &unavailable);
            presets = start_presets(
                &sources,
                &view,
                resolved.instructions.warn_tokens,
                inputs.presets.clone(),
            );
        }
    }
    let mut moved_warning = None;
    if let Some(saved) = resumed.as_ref()
        && let Some(logged) = saved.logged.as_ref()
        && setup.endpoint.is_none()
        && let Some(name) = logged.tools_pair().0
    {
        let start = (!start_failed).then(|| book.start.to_string());
        let selected = resume::select_endpoint(
            &saved.recovered.records,
            logged,
            name,
            &book.entries,
            start.as_deref(),
            asks,
        );
        let (name, left) = match selected.settle(at_name, asks)? {
            resume::Settled::Go { name, left } => (name, left),
            resume::Settled::Ask(question) => {
                let step = logged.last_step.as_ref();
                let on_accounts =
                    context::account_facts(&models.settings, &search_endpoints(&book));
                return Err(Stop::Ask(Box::new(Asking {
                    question,
                    book,
                    model: logged.current.model.clone(),
                    effort: logged.current.effort,
                    cache_ttl: step.map_or(logged.current.cache_ttl, |s| s.ttl),
                    size: step.map(|s| s.prompt_tokens + s.output_tokens),
                    on_accounts,
                })));
            }
        };
        if name != book.start.as_str() {
            resume::move_to(&mut sources, &name);
            book = sources.resolve().map_err(|e| format!("wisp: {e}"))?;
        }
        moved_warning = left.map(|left| (left.continuing(&name), left));
    }
    Ok(Opened {
        setup: setup.clone(),
        inputs,
        sources,
        lists,
        listed,
        new_chats,
        start_line,
        start_pick,
        resolved,
        presets,
        preset,
        refused,
        book,
        models,
        resumed,
        moved: moved_warning,
        gone: gone_left,
        said,
        client_warnings: Vec::new(),
        unpicked,
        catalog,
    })
}

async fn refresh_start_model(
    sources: &endpoints::Sources,
    pinned: bool,
    typed: &str,
    models: &[ModelFacts],
    ready: &[Connection],
    unavailable: &[(String, String)],
    folder: &Path,
) -> Option<LiveCatalog> {
    let book = sources.book().ok()?;
    let active = ready.iter().find(|c| c.name == book.start.as_str())?;
    let candidates = || ready.iter().filter(|c| !pinned || c.name == active.name);
    let offered = resume::model_offered(typed, models, ready);
    if candidates().any(|c| offered(&c.name, c.provider.kind())) {
        return None;
    }
    let model = named_model(models, typed).map_or(typed, |m| m.id.as_str());
    let catalog = LiveCatalog::new(kit::Connected {
        provider: active.provider.clone(),
        ready: ready.to_vec(),
        unavailable: unavailable.to_vec(),
        active: active.name.clone(),
        env_account: wisp_core::endpoints::env_account(&book.entries).map(ToString::to_string),
    })
    .with_lists(Some(folder.to_owned()));
    let reader = catalog.reader();
    let attempts = candidates().map(|c| reader.refetch_unlisted(&c.name, model));
    futures::future::join_all(attempts)
        .await
        .into_iter()
        .any(|attempted| attempted)
        .then_some(catalog)
}

fn picks(
    setup: &Setup,
    picked: &Picked,
    sources: &endpoints::Sources,
    start: Option<&str>,
    view: &wisp_core::agents::endpoints::EndpointView,
) -> Setup {
    let account = start.filter(|start| picked.account.as_deref() == Some(*start));
    let account = account.and_then(|start| view.ready(start));
    let mut flags = setup.flags.clone();
    let model = picked
        .model
        .clone()
        .filter(|m| account.is_some_and(|a| a.serves(m)));
    let given =
        |name, value: &str| env_given(name, SettingValue::Text(value.into()), START_ORIGIN.into());
    flags.extend(
        model
            .as_deref()
            .and_then(|model| given(names::MODEL, model)),
    );
    let model = model.unwrap_or_else(|| Run::resolve(setup, sources, start).model);
    let ready = start.and_then(|start| view.ready(start));
    let takes = |effort: &Effort| {
        ready
            .and_then(|a| a.row(&model))
            .is_none_or(|row| row.takes_effort(*effort))
    };
    let effort = picked.effort.filter(takes);
    flags.extend(effort.and_then(|effort| given(names::EFFORT, effort.as_str())));
    Setup {
        flags,
        endpoint: setup.endpoint.clone(),
    }
}

fn logged_preset(resumed: &resume::Resumed) -> Option<&str> {
    resumed.logged.as_ref()?.started.preset.as_deref()
}

fn session_preset<'a>(
    resumed: Option<&resume::Resumed>,
    presets: &'a CheckedPresets,
    fresh: Option<&'a Preset>,
) -> Option<&'a Preset> {
    match resumed {
        Some(resumed) => presets.find(logged_preset(resumed)?),
        None => fresh,
    }
}

fn require_mcp_search(
    sources: &endpoints::Sources,
    search_enabled: impl FnOnce() -> bool,
) -> Result<(), String> {
    if !sources.run_servers.is_empty() && !search_enabled() {
        return Err("wisp: --mcp-config needs the search tool, which finds a server's tools; add search to --tools".to_owned());
    }
    Ok(())
}

struct SelectedTools {
    tools: Tools,
    skipped_tools: Vec<String>,
    plugin_problems: Vec<Problem>,
    preset_off: BTreeSet<String>,
}

fn selected_tools(
    inputs: &Inputs<'_>,
    setup: &Setup,
    sources: &endpoints::Sources,
    resolved: &Run,
    main_preset: Option<&Preset>,
) -> SelectedTools {
    let saved = sources
        .config
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .map(|config| config.settings.clone())
        .unwrap_or_default();
    let switches = wisp_core::settings::start_switches(&setup.flags, &saved);
    let (registry, plugin_problems) = tools::registry_with(switches);
    let preset_off = main_preset
        .filter(|_| inputs.full_screen)
        .map(tools::preset_off)
        .unwrap_or_default();
    let configured = if preset_off.is_empty() {
        resolved.tools.clone()
    } else {
        Some(Vec::from_iter(
            &tools::names(resolved.tools.as_deref(), &registry) - &preset_off,
        ))
    };
    let flagged = match setup.value(names::TOOLS) {
        Some(SettingValue::List(names)) => Some(names.as_slice()),
        _ => None,
    };
    let listed = tools::Listed {
        configured: configured.as_deref(),
        flagged,
    };
    let (tools, skipped_tools) = tools::tools(listed, inputs.tool_rule, registry);
    let tools = tools.without_tools(resolved.turned_off.iter().cloned().collect());
    SelectedTools {
        tools,
        skipped_tools,
        plugin_problems,
        preset_off,
    }
}

pub fn mcp_service(
    sources: &endpoints::Sources,
    cwd: PathBuf,
    keychain: &Keychain,
    user_input: bool,
) -> Result<wisp_core::mcp::Service, String> {
    use wisp_core::mcp::Service;
    let servers = sources.mcp_servers();
    let folder =
        JsonlStore::default_dir(xdg_data_home(), home_dir()).map(|dir| dir.with_file_name("mcp"));
    let service = match sources.run_config.as_deref() {
        Some(run) => Service::new_with_run_config(
            servers,
            cwd,
            folder,
            run,
            &sources.run_servers,
            sources.config_path.as_deref(),
        )?,
        None => Service::new(servers, cwd, folder),
    }
    .with_forms(user_input);
    Ok(match sources.config_path.clone() {
        Some(path) => service
            .with_config(path, sources.config_shown)
            .with_store(keychain.detected().auto_store()),
        None => service,
    })
}

pub async fn build(
    opened: Opened<'_>,
    host: Option<Host>,
    accounts: Option<Accounts>,
) -> Result<Built, String> {
    let Opened {
        inputs,
        setup: opened_setup,
        sources,
        lists,
        listed: list_names,
        new_chats,
        start_line,
        start_pick,
        resolved,
        presets,
        preset,
        refused,
        book,
        models,
        mut resumed,
        moved: moved_warning,
        client_warnings,
        catalog,
        ..
    } = opened;
    let setup = &opened_setup;
    let on_tui = inputs.full_screen;
    let main_preset = session_preset(resumed.as_ref(), &presets, preset.as_ref()).cloned();
    let own_preset = main_preset.as_ref().map(|p| p.name.clone());
    let SelectedTools {
        tools,
        skipped_tools,
        plugin_problems,
        preset_off,
    } = selected_tools(&inputs, setup, &sources, &resolved, main_preset.as_ref());
    let preload = wisp_protocol::settings::resolve_preload(
        main_preset
            .as_ref()
            .and_then(|preset| preset.preload.as_deref()),
        Some(&resolved.preload),
    );
    let tools = tools.with_preload(preload);
    let workspace = tools::workspace();
    let mut log_context = book.log_context();
    log_context.redactions = sources.held_redactions(Some(&log_context.redactions));
    wisp_core::session::register(&log_context.redactions);
    let mcp = mcp_service(
        &sources,
        workspace
            .as_ref()
            .map(|workspace| workspace.root.clone())
            .unwrap_or_default(),
        inputs.keychain,
        on_tui,
    )
    .map_err(|err| format!("wisp: {err}"))?;
    mcp.refresh_keys().await;
    let kernel_host = Arc::new(KernelHost::new());
    kernel_host.set_mcp(mcp.clone());
    let started = tools.start(kit::ROOT, kernel_host).await;
    let toolbox = &started.toolbox;
    let spawn_agent = wisp_protocol::agents::SPAWN_AGENT;
    let can_spawn = toolbox.is_enabled(spawn_agent);
    if inputs.tool_rule == ToolRule::Flagged {
        let mut refused = toolbox.asking();
        if can_spawn {
            refused.push(spawn_agent.to_owned());
        }
        if !refused.is_empty() {
            refused.sort_unstable();
            refused.dedup();
            return Err(format!("wisp: {}", tools::tui_only(&refused)));
        }
    }
    require_mcp_search(&sources, || toolbox.is_enabled("search"))?;
    let spawns = inputs.tool_rule == ToolRule::Configured || can_spawn;
    let refreshed = catalog.as_ref().map(LiveCatalog::view);
    let provider = match refreshed
        .as_ref()
        .and_then(|v| v.ready.iter().find(|c| c.name == book.start.as_str()))
    {
        Some(active) => active.provider.clone(),
        None => book
            .active
            .provider
            .connect(&book.active, lists.as_deref())
            .map_err(|e| format!("wisp: {e}"))?,
    };
    let mut reopened = None;
    let start = match resumed.take() {
        Some(resume::Resumed {
            log,
            path,
            recovered,
            truncated_bytes,
            logged,
            rebuilt,
            warnings,
            unlocked,
        }) => {
            let kept = reopened.insert(Reopened {
                unlocked,
                path,
                recovered,
                logged,
                warnings,
            });
            kit::Start::Resume(kit::Resume {
                log,
                recovered: &kept.recovered,
                truncated_bytes,
                logged: kept.logged.as_ref(),
                rebuilt,
            })
        }
        None => kit::Start::Fresh,
    };
    let spec = kit::Spec {
        path: kit::ROOT.to_owned(),
        choices: kit::Choices {
            model: resolved.model.clone(),
            effort: resolved.effort,
            cache_ttl: resolved.cache_ttl,
            message_time: resolved.time,
            context_note: resolved.context_note,
        },
        instructions: kit::InstructionsChoice {
            source: resolved.instructions.source.clone(),
            warn_tokens: resolved.instructions.warn_tokens,
        },
        instructions_flag: instructions_flag(&setup.flags),
        extra: resolved.instructions_extra,
        extra_flag: extra_flag(&setup.flags),
        preset: main_preset.and_then(|p| {
            let found = presets.instructions(&p.name)?;
            Some(kit::PresetSource {
                path: found.path.clone(),
                preset: p.name,
            })
        }),
        start,
    };
    let session = spec.session();
    if spec.logged().is_none() {
        wisp_core::agents::endpoints::check_model(
            provider.as_ref(),
            book.start.as_str(),
            &session.model,
        )
        .map_err(|e| format!("wisp: {e}"))?;
        if let Some(row) = provider.capabilities(&session.model) {
            row.check_selectable(provider.kind(), &session.model)
                .map_err(|e| format!("wisp: {e}"))?;
        }
    }

    let picked = resolved.chosen(&session, endpoints::reach(&book.active));
    let (ready, unavailable) = refreshed.map_or_else(
        || endpoints::connections(&book.entries, lists.as_deref()),
        |v| (v.ready, v.unavailable),
    );
    let summaries = start_summaries(&sources, &book, &unavailable, &ready);
    let mut facts = models::session_facts(&book, picked, summaries.clone(), &ready);
    facts.resume_refused = refused;
    facts.start_pick = start_pick;
    let mut log_path = None;
    let mut warnings: Vec<String> = start_line
        .into_iter()
        .chain(
            skipped_tools
                .iter()
                .map(|warning| format!("wisp: {warning}")),
        )
        .collect();
    warnings.extend(sources.shadowed().map(|line| format!("wisp: {line}")));
    let known: Vec<&str> = book
        .entries
        .keys()
        .map(|n| n.as_str())
        .chain(std::iter::once(book.start.as_str()))
        .collect();
    warnings.extend(
        context::start_warnings(&models, book.start.as_str(), &session.model, &ready)
            .into_iter()
            .chain(context::unknown_places(&models, &known, &session.model))
            .chain(context::point_warnings(
                &models,
                book.start.as_str(),
                &session.model,
                context::Run {
                    max_tokens: agent::DEFAULT_MAX_TOKENS,
                    auto_compact: resolved.auto_compact,
                    cache_ttl: session.cache_ttl,
                },
                &ready,
            ))
            .map(|w| format!("wisp: {w}")),
    );
    let ids: Vec<String> = facts.models.iter().map(|m| m.id.clone()).collect();
    facts.on_accounts = context::account_facts(&models.settings, &search_endpoints(&book));
    let search_plugin = wisp_core::resume::search_plugin_id(&started.catalog);
    let logged = spec.logged();
    let web_search = toolbox.web_search()
        && !logged.is_some_and(|logged| logged.search_switched_off(search_plugin));
    facts.search = context::search_forms(
        &models,
        &search_endpoints(&book),
        &ids,
        web_search,
        toolbox.is_empty(),
        logged.and_then(|logged| {
            logged.search_on().then(|| {
                let (endpoint, model) = logged.tools_pair();
                (endpoint.unwrap_or(book.start.as_str()), model, true)
            })
        }),
        &ready,
    );
    let mut mode_warnings = Vec::new();
    if book.configured {
        let home = std::env::var("HOME").ok();
        let files = [
            (sources.config_path.as_deref(), sources.config_shown),
            (
                sources.inputs.env_path.as_deref().filter(|p| p.exists()),
                wisp_core::endpoints::PathShown::Default,
            ),
        ];
        mode_warnings.extend(
            endpoints::mode_warnings(&files, home.as_deref())
                .into_iter()
                .chain(endpoints::credential_warnings(&sources, home.as_deref()))
                .map(|w| format!("wisp: {w}")),
        );
    }
    let home = std::env::var("HOME").ok();
    mode_warnings.extend(client_warnings);
    let lanes = wisp_core::lanes::Lanes::resolve(
        &saved_layers(setup, &sources),
        summaries
            .iter()
            .map(|s| (s.name.as_str(), s.credential.as_ref())),
        &session.model,
    );
    let start_layers = StartLayers {
        remembered: crate::paths::remembered(),
        forced: inputs.forced,
        ..start_layers(
            setup,
            &sources,
            &book,
            inputs.keychain,
            keychain_variable().as_deref(),
            summaries.clone(),
            on_tui,
        )
    };
    let plan_usage = usage_cache();
    let catalog = match catalog {
        Some(catalog) => catalog.with_start(book.start.to_string(), &provider),
        None => LiveCatalog::new(kit::Connected {
            provider,
            ready,
            unavailable,
            active: book.start.to_string(),
            env_account: wisp_core::endpoints::env_account(&book.entries).map(ToString::to_string),
        }),
    };
    catalog.set_providers_off(sources.off.clone());
    let watch = SourcesWatch {
        env_file: inputs.env_file.map(Path::to_path_buf),
        config: inputs.config.map(Path::to_path_buf),
        endpoint: setup.endpoint.clone(),
        xdg_config_home: xdg_config_home(),
        home: home_dir(),
        keychain: Arc::clone(inputs.keychain),
        set: setup.flags.clone(),
        models: lists.clone(),
    };
    let catalog = catalog
        .with_usage(plan_usage.clone())
        .with_summaries(summaries)
        .with_lists(lists.clone())
        .with_watch(
            Arc::new(watch),
            wisp_core::endpoints::watch::prints(&book.entries),
        );
    let endpoint_host = host.map(|host| host(catalog.clone()));
    let rounds = on_tui.then(|| catalog.clone());
    let kit = Kit {
        connect: Arc::new(catalog),
        tools: Arc::new(Prestarted::new(started, tools)),
        logs: Arc::new(kit::JsonlLogs {
            on: resolved.log,
            dir: inputs.sessions.clone(),
            root_id: inputs.session_id.map(str::to_owned),
        }),
        log_context,
        models: models.settings,
        options: resolved.options,
        retry_budget: resolved.retry_budget,
        count_tokens: resolved.count_tokens,
        auto_compact: resolved.auto_compact,
        user_input: on_tui,
        naming: on_tui,
        workspace,
        instructions: instructions::dirs(xdg_config_home(), home_dir()),
        plugin_data: JsonlStore::default_plugin_data_dir(xdg_data_home(), home_dir()),
        start_layers,
        settings_host: sources.config_path.clone().map(|path| {
            Arc::new(settings::host::FileHost {
                used_path: crate::paths::used_settings(),
                path,
                shown: sources.config_shown,
                home: home.clone(),
            }) as Arc<dyn wisp_core::settings::host::SettingsHost>
        }),
        endpoint_host,
        clock: Arc::new(wisp_core::clock::SystemClock),
        plan_usage,
        usage_display: if on_tui {
            wisp_core::plan_usage::Display::Enabled
        } else {
            wisp_core::plan_usage::Display::Disabled
        },
        warmth: wisp_core::warmth::WarmthTable::default(),
        lanes,
        mcp,
    };
    let mut tree = tree_setup(
        setup,
        &sources,
        &session.model,
        spec.instructions(),
        presets,
        inputs.restarted,
    );
    warnings.extend(
        tree.presets
            .warnings(spawns, own_preset.as_deref())
            .into_iter()
            .map(|w| format!("wisp: {w}")),
    );
    let built = kit.build(spec).await.map_err(|e| {
        let unshown = plugin_problems
            .iter()
            .map(|problem| format!("wisp: {problem}"))
            .chain(warnings.iter().cloned())
            .chain(mode_warnings.iter().cloned())
            .chain(reopened.iter().flat_map(|r| r.warnings.iter().cloned()))
            .chain(moved_warning.iter().map(|(line, _)| line.clone()));
        with_warnings(format!("wisp: {e}"), unshown)
    })?;
    if inputs.session_id.is_some()
        && let kit::LogOutcome::Failed(e) = &built.log
    {
        return Err(format!("wisp: {e}"));
    }
    if let Some(catalog) = rounds {
        endpoints::keep_lists_fresh(catalog, list_names);
    }
    let mut warnings = plugin_warnings(&plugin_problems, &built.plugin_problems, warnings);
    let instructions_note = match &built.workspace {
        Some(kit::WorkspaceNote::Missing(e)) => {
            warnings.push(format!("wisp: {e}; no instructions were loaded"));
            None
        }
        Some(kit::WorkspaceNote::Gone(cwd)) => {
            warnings.push(format!(
                "wisp: this session ran in {cwd}, which no longer exists; its tools run in the current directory"
            ));
            Some(resume::GONE_NOTE)
        }
        Some(kit::WorkspaceNote::NoDirectory) => Some(resume::NO_DIRECTORY_NOTE),
        None => None,
    };
    if let Some(loaded) = &built.instructions {
        facts.instructions = instructions::facts(loaded);
        facts.instructions.note = instructions_note.map(str::to_owned);
    }
    warnings.extend(mode_warnings);
    let mut agent = built.agent.with_preset_off(preset_off);
    let mut view = None;
    let (moved_line, continuing) = moved_warning.unzip();
    if let Some(r) = reopened.as_mut() {
        log_path = Some(r.path.display().to_string());
        warnings.extend(std::mem::take(&mut r.warnings));
        warnings.extend(moved_line.filter(|_| !on_tui));
        if let Some(logged) = &r.logged {
            let flagged = settings::flag_value(&setup.flags, names::INSTRUCTIONS_EXTRA).is_some();
            warnings.extend(resume::extra_kept(logged, flagged));
        }
        warnings.extend(instructions_note.map(|note| format!("wisp: {note}")));
        let follow = inputs.at.map_or(&[][..], |at| at.follow.as_slice());
        let lock = resume::Request {
            id: None,
            unlocked: inputs.resume.as_ref().is_some_and(|r| r.unlocked),
        }
        .lock();
        tree.start = new_chats;
        let (unusable, reaches) = accounts.map(|a| (a.unusable, a.reaches)).unzip();
        tree.unusable = unusable.unwrap_or_default();
        tree.loaded =
            wisp_core::agents::load_tree(&kit, &tree, &agent, &r.recovered.records, lock, follow)
                .await;
        let placed = std::mem::take(&mut tree.loaded.placed);
        warnings.extend(
            std::mem::take(&mut tree.loaded.warnings)
                .into_iter()
                .filter(|w| !on_tui || !placed.contains(w))
                .map(|w| format!("wisp: {w}")),
        );
        if !on_tui {
            use wisp_render::chrome::account::{Surface, login_again};
            warnings.extend(tree.loaded.stay.iter().filter_map(|stayed| {
                let name = &stayed.account;
                let short = reaches.as_ref().is_some_and(|reaches| reaches(name));
                let command = login_again(Surface::Terminal, name, short);
                let line = wisp_render::chrome::account::not_resumed_line(stayed, &command)?;
                Some(format!("wisp: {line}"))
            }));
        }
        agent.carry_agents(
            std::mem::take(&mut tree.loaded.moved),
            std::mem::take(&mut tree.loaded.stay),
        );
        let notice = agent.resume_notice_text().map(str::to_owned);
        let changed = &tree.loaded.preset_changes;
        let records = &r.recovered.records;
        view = Some(if on_tui || inputs.voice {
            agent.view(records)
        } else {
            SessionView {
                input_ids: agent::view::input_ids(records),
                ..SessionView::default()
            }
        });
        if on_tui {
            facts.resumed = notice;
            if !changed.is_empty() {
                let line = wisp_render::transcript::copy::presets_changed(changed.len());
                warnings.push(format!("wisp: {line}"));
            }
        } else {
            warnings.extend(changed.iter().map(|c| format!("wisp: {}", c.warning())));
            warnings.extend(notice);
        }
        resume::refresh_priced(
            facts
                .endpoints
                .iter_mut()
                .map(|endpoint| (endpoint.name.as_str(), &mut endpoint.priced)),
            &agent,
        );
        facts.next = agent.next_turn();
    } else {
        match &built.log {
            kit::LogOutcome::Opened { path, unlocked, .. } => {
                log_path = Some(path.display().to_string());
                warnings.extend(unlocked.as_deref().map(unlocked_warning));
            }
            kit::LogOutcome::Failed(e) => {
                warnings.push(format!("wisp: not logging this session: {e}"));
            }
            kit::LogOutcome::Off | kit::LogOutcome::Resumed => {}
        }
    }
    let (unlocked, fresh_log) = match built.log {
        kit::LogOutcome::Opened {
            unlocked, fresh, ..
        } => (unlocked, fresh),
        _ => (reopened.as_ref().and_then(|r| r.unlocked.clone()), None),
    };
    facts.log = log_path;
    Ok(Built {
        unlocked,
        fresh_log,
        kit,
        tree,
        agent,
        sources,
        setup: opened_setup,
        resolved,
        book,
        reopened,
        continuing,
        opening: facts,
        warnings,
        view,
        voice: inputs.voice,
    })
}

pub fn start(built: Built) -> Started {
    let Built {
        kit,
        tree,
        agent,
        sources,
        mut opening,
        warnings,
        continuing,
        book,
        view,
        voice,
        ..
    } = built;
    let start = opening.start.get_or_insert_with(Default::default);
    start.warnings = warnings;
    start.continued = continuing.as_ref().map(|left| {
        let marks = |name: &str| crate::run::marked_in(&book.marks, name);
        let mut continued = left.continued(&marks);
        continued.quote = agent.resume_quote();
        continued
    });
    let voice = voice.then(|| wisp_core::voice::Voice {
        accounts: kit.connect.clone(),
        place: preset_place(&sources, 0),
        legs: Some(wisp_core::voice::legs()),
        records: agent.voice_sender(),
        earlier: view.as_ref().map_or_else(Default::default, |view| {
            wisp_core::voice::Earlier::of(&view.events, &view.input_ids)
        }),
    });
    let (commands, events, task) = wisp_core::agents::start_root(kit, tree, agent);
    let (commands, events, lane) = match voice {
        Some(voice) => {
            let (commands, events, lane) = wisp_core::voice::attach(commands, events, voice);
            (commands, events, Some(lane))
        }
        None => (commands, events, None),
    };
    let quit = QuitBound {
        worst: task.worst.clone(),
        margin: agent::QUIT_MARGIN,
    };
    Started {
        channels: Channels {
            commands,
            events,
            quit: Some(quit),
        },
        lane,
        task,
        view,
        opening,
    }
}

pub async fn end(
    app: &str,
    task: agent::AgentTask,
    forced: bool,
    by: tokio::time::Instant,
    done: ExitCode,
) -> ExitCode {
    if forced {
        drop(tokio::time::timeout_at(by, task.handle).await);
        return done;
    }
    let saving = task.save_waiting();
    if task.join().await {
        return done;
    }
    if saving {
        drop(writeln!(std::io::stderr(), "{app}: {SAVE_CUT}"));
    }
    ExitCode::from(FORCED_EXIT)
}

pub fn unlocked_warning(reason: &str) -> String {
    format!("wisp: {}", wisp_core::resume::unlocked_warning(reason))
}

pub fn search_endpoints(book: &EndpointBook) -> Vec<(String, wisp_core::endpoints::ProviderKind)> {
    use wisp_core::endpoints::Availability;
    wisp_core::endpoints::listed(&book.entries)
        .filter_map(|(name, entry)| {
            let wire = match entry {
                Availability::Ready(endpoint) => endpoint.provider,
                Availability::Unavailable(unready) => unready.provider,
                Availability::SameAs(_) => return None,
            };
            Some((name.as_str().to_owned(), wire))
        })
        .collect()
}

pub fn relaunch_logs(setup: &Setup, sources: &endpoints::Sources, start: Option<&str>) -> bool {
    let kept = Setup {
        flags: setup
            .flags
            .iter()
            .filter(|given| given.origin != "--no-log")
            .cloned()
            .collect(),
        endpoint: setup.endpoint.clone(),
    };
    Run::resolve(&kept, sources, start).log
}

pub fn plugin_warnings(
    loaded: &[wisp_core::plugin::Problem],
    started: &[wisp_core::plugin::Problem],
    rest: Vec<String>,
) -> Vec<String> {
    loaded
        .iter()
        .chain(started)
        .map(|problem| format!("wisp: {problem}"))
        .chain(rest)
        .collect()
}

pub fn with_warnings(error: String, warnings: impl IntoIterator<Item = String>) -> String {
    warnings.into_iter().fold(error, |text, warning| {
        format!("{text}\n{}", wisp_render::text::sanitize(&warning))
    })
}

pub fn instructions_flag(
    flags: &[wisp_core::settings::Given],
) -> Option<wisp_protocol::InstructionsSource> {
    settings::flag_value(flags, names::INSTRUCTIONS_SOURCE)
        .and_then(|v| wisp_protocol::InstructionsSource::parse(&v.text()))
}

pub fn extra_flag(
    flags: &[wisp_core::settings::Given],
) -> Option<Option<wisp_core::instructions::Extra>> {
    settings::flag_value(flags, names::INSTRUCTIONS_EXTRA)
        .map(|v| wisp_core::instructions::Extra::named(&v.text()))
}
