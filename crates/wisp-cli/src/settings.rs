use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use serde_json::{Value, json};
use wisp_core::agent::DEFAULT_MAX_TOKENS;
use wisp_core::config::{SavedSettings, shown_value};
use wisp_core::config_file::{SaveError, SaveOutcome, SiteEdit, save};
use wisp_core::context::{
    CompactAt, background_point_warning, compact_at, group, low_point_warning,
};
use wisp_core::endpoints::{ConfigFile, ENV, GIVEN_CONFIG, PathShown, ProviderKind};
use wisp_core::models::ModelTable;
use wisp_core::secrets::LOCK_WAIT;
use wisp_core::settings::{
    Current, Found, Given, Layer, Layers, Resolution, Resolved, resolve, resolve_one,
};
use wisp_protocol::settings::{
    Applies, CredentialKind, DEFAULT_MODEL, Kind, Place, Secrecy, Section, SettingSpec,
    SettingValue, Site, SitePath, Target, all, find, names,
};

use wisp_protocol::{CONFIG_FILE, CacheTtl};
use wisp_render::chrome::context::window_text;

use crate::endpoints::{self, note_words};
use crate::manage::{self, KEY_ON_COMMAND_LINE};
use crate::usage::USAGE_EXIT;
pub(crate) use wisp_start::settings::*;

pub(crate) const APPLIES_NEXT: &str =
    "applies to new sessions; a running Wisp keeps its settings (use /config there)";
pub(crate) const CREATED_GIVEN: &str = concat!("created ", wisp_protocol::given_config!());

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
    #[command(about = "Every setting with its value and where it comes from")]
    List {
        #[arg(
            value_name = "FILTER",
            help = "Only settings whose name holds this text"
        )]
        filter: Option<String>,
        #[arg(long, help = "Print JSON")]
        json: bool,
    },
    #[command(about = "One setting's value")]
    Get {
        #[arg(
            value_name = "NAME",
            help = "A setting's name, or its path for one account or model"
        )]
        name: String,
        #[arg(long, help = "Show every place that sets it and which one wins")]
        explain: bool,
        #[arg(long, help = "Print JSON")]
        json: bool,
    },
    #[command(about = concat!("Save a setting to ", wisp_protocol::config_file!()))]
    Set {
        #[arg(
            value_name = "NAME",
            help = "A setting's name, or its path for one account or model"
        )]
        name: String,
        #[arg(
            value_name = "VALUE",
            allow_hyphen_values = true,
            help = "The new value"
        )]
        value: Option<String>,
    },
    #[command(about = concat!("Remove a saved setting from ", wisp_protocol::config_file!()))]
    Unset {
        #[arg(
            value_name = "NAME",
            help = "A setting's name, or its path for one account or model"
        )]
        name: String,
    },
    #[command(about = concat!("Show where ", wisp_protocol::config_file!(), " is"))]
    Path,
}

pub(crate) type SecretStates = BTreeMap<String, (bool, bool)>;

pub(crate) struct Command<'a> {
    pub(crate) manage: &'a manage::Context<'a>,
    pub(crate) flags: &'a [Given],
    pub(crate) env: Vec<Given>,
    pub(crate) secrets: SecretStates,
    pub(crate) keys_file: Option<String>,
    pub(crate) endpoint: Option<String>,
    pub(crate) start: Option<String>,
    pub(crate) model: String,
    pub(crate) endpoints: Vec<String>,
    pub(crate) kinds: BTreeMap<String, CredentialKind>,
    pub(crate) wires: BTreeMap<String, ProviderKind>,
}

impl Command<'_> {
    fn target(&self, text: &str) -> Result<Target, String> {
        let endpoints: Vec<&str> = self.endpoints.iter().map(String::as_str).collect();
        target_among(text, &endpoints)
    }
}

fn usage_fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("wisp: {message}");
    ExitCode::from(USAGE_EXIT)
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("wisp: {message}");
    ExitCode::FAILURE
}

pub(crate) fn run(command: &ConfigCommand, cx: &Command<'_>) -> ExitCode {
    match command {
        ConfigCommand::List { filter, json } => list(filter.as_deref(), *json, cx),
        ConfigCommand::Get {
            name,
            explain,
            json,
        } => get(name, *explain, *json, cx),
        ConfigCommand::Set { name, value } => set(name, value.as_deref(), cx),
        ConfigCommand::Unset { name } => unset(name, cx),
        ConfigCommand::Path => path(cx),
    }
}

pub(crate) fn read(cx: &Command<'_>) -> Result<(PathBuf, PathShown, Option<ConfigFile>), String> {
    let (path, shown) = cx.manage.target(true)?;
    let config = endpoints::read_config(&path, shown == PathShown::Given)?;
    Ok((path, shown, config))
}

fn current(cx: &Command<'_>) -> Current {
    Current {
        endpoint: cx.endpoint.clone(),
        model: cx.model.clone(),
    }
}

fn layers<'a>(
    cx: &Command<'_>,
    saved: Option<&'a SavedSettings>,
    shipped: Option<&'a ModelTable>,
) -> Layers<'a> {
    Layers {
        flags: cx.flags.to_vec(),
        env: cx.env.clone(),
        saved,
        shipped,
        ..Layers::default()
    }
}

fn on_wire(resolution: Resolution, here: &Current, cx: &Command<'_>) -> Resolution {
    let wire = here.endpoint.as_ref().and_then(|name| cx.wires.get(name));
    resolution.on_wire(wire.copied())
}

fn shown(spec: &SettingSpec, value: &SettingValue) -> String {
    shown_value(spec, value)
}

fn found_words(spec: &SettingSpec, found: &Found) -> String {
    let value = shown(spec, &found.value);
    let origin = origin_text(found);
    match found.layer {
        Layer::Flag if origin.starts_with("--set ") => format!("{origin}={value}"),
        Layer::Environment if origin.contains(' ') => format!("{value}: {origin}"),
        Layer::Environment => format!("{origin}={value}"),
        Layer::Flag | Layer::Session => match found.value {
            SettingValue::Bool(_) if !origin.starts_with("--set") => origin,
            _ => format!("{origin} {value}"),
        },
        Layer::Shipped => format!("{value} (shipped, {origin})"),
        Layer::Default => value,
        _ => format!("{origin} = {value}"),
    }
}

fn source_words(found: &Found) -> String {
    match (found.layer, found.line) {
        (_, Some(line)) => format!("{CONFIG_FILE} line {line}"),
        (Layer::Environment, _) if !found.origin.contains(' ') => found.origin.clone(),
        (Layer::Flag, _) => "flag".to_owned(),
        (Layer::Shipped, _) => "shipped".to_owned(),
        (layer, _) => layer.source().to_owned(),
    }
}

fn listed(spec: &SettingSpec) -> bool {
    spec.is_built() && spec.secrecy != Secrecy::Hidden && !spec.per_plugin()
}

fn secret_state(cx: &Command<'_>, endpoint: Option<&str>, name: &str) -> &'static str {
    let set = endpoint
        .and_then(|e| {
            if e == ENV {
                cx.keys_file.as_deref()
            } else {
                Some(e)
            }
        })
        .and_then(|e| cx.secrets.get(e))
        .is_some_and(|(key, url)| if name == names::KEY { *key } else { *url });
    if set { "set" } else { "not set" }
}

fn secret_rows(cx: &Command<'_>, spec: &SettingSpec) -> Vec<(String, String)> {
    cx.secrets
        .keys()
        .map(|e| {
            (
                format!("endpoints.{e}.{}", spec.name),
                secret_state(cx, Some(e), spec.name).to_owned(),
            )
        })
        .collect()
}

fn started_on<'a>(resolution: &Resolution, cx: &'a Command<'_>) -> Option<&'a str> {
    (resolution.spec.name == names::ENDPOINT && resolution.winner().is_none())
        .then_some(cx.endpoint.as_deref())
        .flatten()
}

fn default_words(spec: &SettingSpec, here: &Current, cx: &Command<'_>) -> String {
    match &here.endpoint {
        None => spec.default_words(),
        Some(name) => spec
            .default_for(cx.kinds.get(name))
            .map_or_else(|| spec.default_words(), |value| shown(spec, &value)),
    }
}

fn value_and_source(resolution: &Resolution, here: &Current, cx: &Command<'_>) -> (String, String) {
    let source = Layer::Default.source().to_owned();
    match (resolution.winner(), started_on(resolution, cx)) {
        (Some(found), _) => (shown(resolution.spec, &found.value), source_words(found)),
        (None, Some(start)) => (start.to_owned(), source),
        (None, None) => (default_words(resolution.spec, here, cx), source),
    }
}

fn list(filter: Option<&str>, json: bool, cx: &Command<'_>) -> ExitCode {
    let (_, _, config) = match read(cx) {
        Ok(read) => read,
        Err(e) => return fail(e),
    };
    let saved = config.as_ref().map(|c| &c.settings);
    let shipped = ModelTable::shipped().ok();
    let here = current(cx);
    let resolved: Resolved = resolve(&layers(cx, saved, shipped.as_ref()), &here)
        .into_iter()
        .map(|(name, resolution)| {
            let resolution = resolution.shown_for_keys_file(cx.keys_file.as_deref());
            (name, on_wire(resolution, &here, cx))
        })
        .collect();
    let keep = |spec: &SettingSpec| listed(spec) && filter.is_none_or(|f| spec.name.contains(f));
    let row = |spec: &SettingSpec| -> (String, String) {
        if spec.secrecy == Secrecy::Secret {
            return match cx.endpoint.as_deref() {
                Some(e) => (
                    secret_state(cx, Some(e), spec.name).to_owned(),
                    format!("account {e}"),
                ),
                None => ("per account".to_owned(), "account".to_owned()),
            };
        }
        resolved.get(spec.name).map_or_else(
            || {
                (
                    default_words(spec, &here, cx),
                    Layer::Default.source().to_owned(),
                )
            },
            |resolution| value_and_source(resolution, &here, cx),
        )
    };
    let others = |spec: &SettingSpec| -> Vec<(String, String, usize)> {
        saved
            .into_iter()
            .flat_map(|s| s.of(spec.name))
            .filter(|(path, _)| path.place != Place::Global)
            .map(|(path, entry)| (path.shown(), entry.shown(spec), entry.line))
            .collect()
    };
    if json {
        let mut rows = Vec::new();
        for spec in all().filter(|s| keep(s)) {
            let (value, source) = row(spec);
            let mut saved: Vec<Value> = others(spec)
                .into_iter()
                .map(|(path, value, line)| json!({"path": path, "value": value, "line": line}))
                .collect();
            if spec.secrecy == Secrecy::Secret {
                saved.extend(
                    secret_rows(cx, spec)
                        .into_iter()
                        .map(|(path, value)| json!({"path": path, "value": value})),
                );
            }
            rows.push(json!({"name": spec.name, "value": value, "source": source, "saved": saved}));
        }
        println!("{}", Value::Array(rows));
        return ExitCode::SUCCESS;
    }
    let mut text = String::new();
    for section in Section::ALL {
        let specs: Vec<&SettingSpec> = all().filter(|s| s.section == section && keep(s)).collect();
        if specs.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(section.title());
        text.push('\n');
        for spec in specs {
            let (value, source) = row(spec);
            text.push_str(&format!("  {:<26} {value}  ({source})\n", spec.name));
            if spec.secrecy == Secrecy::Secret {
                for (path, value) in secret_rows(cx, spec) {
                    text.push_str(&format!("    {path} = {value}\n"));
                }
            }
            for (path, value, line) in others(spec) {
                text.push_str(&format!(
                    "    {path} = {value}  ({CONFIG_FILE} line {line})\n"
                ));
            }
        }
    }
    if text.is_empty() {
        println!("no setting matches; see wisp config list");
    } else {
        print!("{text}");
    }
    ExitCode::SUCCESS
}

fn current_for(target: &Target, cx: &Command<'_>) -> Current {
    let mut here = current(cx);
    if let Target::Path(path) = target {
        match &path.place {
            Place::Endpoint(e) => here.endpoint = Some(e.clone()),
            Place::Model(m) => here.model = m.clone(),
            Place::EndpointModel { endpoint, model } => {
                here.endpoint = Some(endpoint.clone());
                here.model = model.clone();
            }
            Place::Global
            | Place::Agent(_)
            | Place::PluginHook { .. }
            | Place::PluginSlot { .. }
            | Place::Plugin(_) => {}
        }
    }
    here
}

fn header(spec: &SettingSpec, here: &Current) -> String {
    let mut out = spec.name.to_owned();
    if spec.has_site(Site::Model) || spec.has_site(Site::EndpointModel) {
        out.push_str(&format!(" for {}", wisp_protocol::shown_model(&here.model)));
    }
    if (spec.has_site(Site::Endpoint) || spec.has_site(Site::EndpointModel))
        && let Some(endpoint) = &here.endpoint
    {
        out.push_str(&format!(" on {endpoint}"));
    }
    out
}

fn get(name: &str, explain: bool, json: bool, cx: &Command<'_>) -> ExitCode {
    let target = match cx.target(name) {
        Ok(target) => target,
        Err(e) => return usage_fail(e),
    };
    let (_, _, config) = match read(cx) {
        Ok(read) => read,
        Err(e) => return fail(e),
    };
    let spec = target.spec();
    let here = current_for(&target, cx);
    if spec.secrecy == Secrecy::Secret {
        let endpoint = here.endpoint.as_deref();
        let state = secret_state(cx, endpoint, spec.name);
        let key = match endpoint {
            Some(e) => format!("endpoints.{e}.{}", spec.name),
            None => spec.name.to_owned(),
        };
        if json {
            println!(
                "{}",
                json!({"name": key, "value": state, "source": "account"})
            );
        } else if explain {
            println!(
                "{key} = {state}\n  a secret; wisp account edit {} changes it",
                endpoint.unwrap_or("NAME")
            );
        } else {
            println!("{state}");
        }
        return ExitCode::SUCCESS;
    }
    let shipped = ModelTable::shipped().ok();
    let saved = config.as_ref().map(|c| &c.settings);
    let resolved = resolve_one(spec, &layers(cx, saved, shipped.as_ref()), &here)
        .shown_for_keys_file(cx.keys_file.as_deref());
    let mut resolution = on_wire(resolved, &here, cx);
    if let Target::Path(path) = &target {
        let origin = path.to_string();
        resolution
            .found
            .retain(|f| f.line.is_none() || f.origin == origin);
    }
    let (value, source) = value_and_source(&resolution, &here, cx);
    if json {
        let layers: Vec<Value> = resolution
            .found
            .iter()
            .map(|f| {
                json!({
                    "layer": f.layer.source(),
                    "origin": origin_text(f),
                    "value": shown(spec, &f.value),
                    "line": f.line,
                })
            })
            .collect();
        println!(
            "{}",
            json!({"name": target_key(&target), "value": value, "source": source, "layers": layers})
        );
        return ExitCode::SUCCESS;
    }
    if !explain {
        println!("{value}");
        return ExitCode::SUCCESS;
    }
    let mut text = format!("{} = {value}\n", header(spec, &here));
    let mut first = true;
    let mut default_shown = false;
    for found in &resolution.found {
        let label = match found.layer {
            Layer::Default => {
                default_shown = true;
                Layer::Default.source()
            }
            _ if first => "wins",
            _ => "shadowed",
        };
        first = false;
        text.push_str(&format!("  {label:<9} {}\n", found_words(spec, found)));
        if let Some(line) = found.line {
            text.push_str(&format!("            {CONFIG_FILE} line {line}\n"));
        }
    }
    if !default_shown {
        let words = match (started_on(&resolution, cx), cx.start.as_deref()) {
            (Some(endpoint), Some(why)) => format!("{endpoint} ({why})"),
            _ => default_words(spec, &here, cx),
        };
        text.push_str(&format!("  {:<9} {words}\n", Layer::Default.source()));
    }
    print!("{text}");
    ExitCode::SUCCESS
}

fn example_model(cx: &Command<'_>) -> String {
    wisp_protocol::model_or_placeholder(&cx.model).to_owned()
}

fn per_site(target: &Target, cx: &Command<'_>) -> Result<SitePath, String> {
    match target {
        Target::Path(path) => Ok(path.clone()),
        Target::Name(spec) if spec.has_site(Site::Global) => Ok(SitePath::global(spec)),
        Target::Name(spec) if spec.has_site(Site::Model) => Err(format!(
            "{} is set per model; give its path, such as '{}'",
            spec.name,
            SitePath::new(Place::Model(example_model(cx)), spec)
        )),
        Target::Name(spec) => Err(format!(
            "{} is set per account; give its path, such as '{}'",
            spec.name,
            SitePath::new(
                Place::Endpoint(cx.endpoint.clone().unwrap_or_else(|| "NAME".to_owned())),
                spec
            )
        )),
    }
}

enum Refusal {
    Usage(String),
    Edit(String, String, String),
}

fn plain_path(target: &Target, cx: &Command<'_>) -> Result<SitePath, Refusal> {
    let spec = target.spec();
    match spec.secrecy {
        Secrecy::Secret => return Err(Refusal::Usage(secret_refusal(target, "wisp config"))),
        Secrecy::Hidden => {
            return Err(Refusal::Usage(format!(
                "{} is written by Wisp, so wisp config doesn't change it",
                spec.name
            )));
        }
        Secrecy::Plain => {}
    }
    let path = per_site(target, cx).map_err(Refusal::Usage)?;
    if let Place::Endpoint(endpoint) = &path.place {
        if endpoint == wisp_core::endpoints::ENV
            && wisp_core::endpoints::ENV_CONNECTION_FIELDS.contains(&spec.name)
        {
            return Err(Refusal::Usage(wisp_core::endpoints::env_connection()));
        }
        if spec.name == names::KEY_STORE {
            return Err(Refusal::Usage(format!(
                "{} moves with the key, so wisp config doesn't change it; remove {endpoint} and add it again with --store",
                path
            )));
        }
        if spec.applies == Applies::Connection {
            return Err(Refusal::Edit(
                endpoint.clone(),
                spec.name.to_owned(),
                path.to_string(),
            ));
        }
    }
    Ok(path)
}

fn save_failure(error: &SaveError, home: Option<&str>) -> ExitCode {
    match error {
        SaveError::Busy(notes) | SaveError::Lock(_, notes) => {
            for note in notes {
                eprintln!("note: {}", note_words(note, home));
            }
        }
        _ => {}
    }
    fail(error)
}

fn print_notes(outcome: &SaveOutcome, file: &Path, shown: PathShown, home: Option<&str>) {
    if outcome.created {
        println!("{}", created_words(file, shown, home));
    }
    for note in &outcome.notes {
        eprintln!("note: {}", note_words(note, home));
    }
}

fn old_value(config: Option<&ConfigFile>, path: &SitePath) -> String {
    if path.spec().name == names::ENDPOINT && path.place == Place::Global {
        return config
            .and_then(|c| c.default.as_ref())
            .map_or_else(|| "not set".to_owned(), ToString::to_string);
    }
    config
        .and_then(|c| c.settings.get(path))
        .map_or_else(|| "not set".to_owned(), |s| s.shown(path.spec()))
}

pub(crate) fn set_quietly(name: &str, value: &str, cx: &Command<'_>) -> ExitCode {
    save_one(name, Some(value), cx, false)
}

fn set(name: &str, value: Option<&str>, cx: &Command<'_>) -> ExitCode {
    save_one(name, value, cx, true)
}

fn save_one(name: &str, value: Option<&str>, cx: &Command<'_>, says_when: bool) -> ExitCode {
    let target = match cx.target(name) {
        Ok(target) => target,
        Err(e) => return usage_fail(e),
    };
    let path = match plain_path(&target, cx) {
        Ok(path) => path,
        Err(Refusal::Usage(e)) => {
            if value.is_some() && target.spec().secrecy == Secrecy::Secret {
                eprintln!("wisp: {e}\n{KEY_ON_COMMAND_LINE}");
                return ExitCode::from(USAGE_EXIT);
            }
            return usage_fail(e);
        }
        Err(Refusal::Edit(endpoint, field, shown_path)) => {
            let Some(value) = value else {
                return usage_fail("wisp config set: missing VALUE; see wisp config set --help");
            };
            return match target.spec().parse(value) {
                Ok(_) => manage::edit::field(&endpoint, &field, value, cx.manage),
                Err(e) => usage_fail(format!(
                    "{shown_path} {} (what was given isn't shown)",
                    e.rule
                )),
            };
        }
    };
    let spec = path.spec();
    let Some(text) = value else {
        return usage_fail("wisp config set: missing VALUE; see wisp config set --help");
    };
    let shown_path = path_text(&path);
    let named = error_name(&path);
    let parsed = match spec.parse(text) {
        Ok(parsed) => parsed,
        Err(e) => {
            return usage_fail(format!("{named} {} (what was given isn't shown)", e.rule));
        }
    };
    if let Err(e) = crate::tools::checked(spec, &parsed) {
        return usage_fail(format!("{named}: {e}"));
    }
    let warnings = match crate::tools::switch_checked(&path, &parsed, &saved_removed(cx)) {
        Ok(warnings) => warnings,
        Err(e) => return usage_fail(format!("{named}: {e}")),
    };
    if let (Some(model), SettingValue::Tokens(window)) = (model_of(&path), &parsed)
        && let Err(e) =
            wisp_core::context::check_limit(model_words(model), *window, DEFAULT_MAX_TOKENS)
        && spec.name == names::CONTEXT_WINDOW
    {
        return usage_fail(e);
    }
    let (file, shown_as) = match cx.manage.target(false) {
        Ok(target) => target,
        Err(e) => return fail(e),
    };
    let before = if std::fs::symlink_metadata(&file).is_ok() {
        match endpoints::read_config(&file, shown_as == PathShown::Given) {
            Ok(config) => config,
            Err(e) => return fail(e),
        }
    } else {
        None
    };
    let home = cx.manage.home.as_ref().map(|h| h.display().to_string());
    if crate::tools::saves_as_default(&path, &parsed, before.as_ref()) {
        let was = old_value(before.as_ref(), &path);
        if before
            .as_ref()
            .is_some_and(|c| c.settings.get(&path).is_some())
        {
            match save(&file, shown_as, &[SiteEdit::Unset(path)], LOCK_WAIT) {
                Ok(outcome) => print_notes(&outcome, &file, shown_as, home.as_deref()),
                Err(e) => return save_failure(&e, home.as_deref()),
            }
        }
        println!("Saved {shown_path} as the default (was {was}), so new tools come on too.");
        if says_when {
            println!("{APPLIES_NEXT}");
        }
        return ExitCode::SUCCESS;
    }
    let outcome = match save(
        &file,
        shown_as,
        &[SiteEdit::Set(path.clone(), parsed.clone())],
        LOCK_WAIT,
    ) {
        Ok(outcome) => outcome,
        Err(e) => return save_failure(&e, home.as_deref()),
    };
    println!(
        "Saved {shown_path} = {} (was {}).",
        shown(spec, &parsed),
        old_value(before.as_ref(), &path)
    );
    print_notes(&outcome, &file, shown_as, home.as_deref());
    if says_when {
        println!("{APPLIES_NEXT}");
    }
    for note in compaction_notes(&path, &file).into_iter().flatten() {
        eprintln!("wisp: {note}");
    }
    if let Some(note) = removed_plugin_note(spec, before.as_ref()) {
        eprintln!("wisp: {note}");
    }
    for warning in warnings {
        eprintln!("wisp: {warning}");
    }
    ExitCode::SUCCESS
}

fn saved_removed(cx: &Command<'_>) -> std::collections::BTreeSet<String> {
    read(cx)
        .ok()
        .and_then(|(_, _, config)| config)
        .map(|config| wisp_core::settings::start_switches(&[], &config.settings).removed)
        .unwrap_or_default()
}

fn removed_plugin_note(spec: &SettingSpec, before: Option<&ConfigFile>) -> Option<String> {
    let id = spec.plugin?;
    let path = SitePath::plugin_switch(id, wisp_protocol::settings::INSTALLED_KEY)?;
    let removed = before
        .and_then(|config| config.settings.get(&path))
        .is_some_and(|saved| wisp_core::settings::is_off(&saved.value));
    removed.then(|| {
        format!(
            "{} belongs to the plugin {id}, which is removed; it applies once wisp plugin add {id} brings it back",
            spec.name
        )
    })
}

fn unset(name: &str, cx: &Command<'_>) -> ExitCode {
    let target = match cx.target(name) {
        Ok(target) => target,
        Err(e) => return usage_fail(e),
    };
    let path = match plain_path(&target, cx) {
        Ok(path) => path,
        Err(Refusal::Usage(e)) => return usage_fail(e),
        Err(Refusal::Edit(endpoint, _, shown_path)) => {
            return usage_fail(format!(
                "{shown_path} is part of how Wisp reaches {endpoint}; change it with wisp account edit {endpoint}"
            ));
        }
    };
    let (file, shown_as, config) = match read(cx) {
        Ok(read) => read,
        Err(e) => return fail(e),
    };
    let shown_path = path_text(&path);
    let was = old_value(config.as_ref(), &path);
    if config.is_none() || was == "not set" {
        println!("{shown_path} isn't saved; nothing to remove.");
        return ExitCode::SUCCESS;
    }
    let home = cx.manage.home.as_ref().map(|h| h.display().to_string());
    match save(&file, shown_as, &[SiteEdit::Unset(path)], LOCK_WAIT) {
        Ok(outcome) => {
            println!("Removed {shown_path} (was {was}).");
            print_notes(&outcome, &file, shown_as, home.as_deref());
            println!("{APPLIES_NEXT}");
            ExitCode::SUCCESS
        }
        Err(e) => save_failure(&e, home.as_deref()),
    }
}

fn path(cx: &Command<'_>) -> ExitCode {
    let (file, shown_as) = match cx.manage.target(true) {
        Ok(target) => target,
        Err(e) => return fail(e),
    };
    if shown_as == PathShown::Given {
        println!("{GIVEN_CONFIG}");
        return ExitCode::SUCCESS;
    }
    let home = cx.manage.home.as_ref().map(|h| h.display().to_string());
    println!("{}", path_words(&file, home.as_deref()));
    ExitCode::SUCCESS
}

fn created_words(file: &Path, shown: PathShown, home: Option<&str>) -> String {
    match shown {
        PathShown::Given => CREATED_GIVEN.to_owned(),
        PathShown::Default => format!("created {}", endpoints::short(file, home)),
    }
}

pub(crate) fn path_words(file: &Path, home: Option<&str>) -> String {
    let short = |p: &Path| endpoints::short(p, home);
    let linked = std::fs::symlink_metadata(file).is_ok_and(|m| m.file_type().is_symlink());
    match (linked, std::fs::canonicalize(file)) {
        (true, Ok(target)) => format!("{} \u{2192} {}", short(file), short(&target)),
        _ => short(file),
    }
}

fn saved_global(name: &str, saved: &SavedSettings) -> Option<SettingValue> {
    let spec = find(name)?;
    let layers = Layers {
        saved: Some(saved),
        ..Layers::default()
    };
    let here = Current {
        endpoint: None,
        model: DEFAULT_MODEL.to_owned(),
    };
    resolve_one(spec, &layers, &here).value().cloned()
}

fn auto_compact_on(saved: &SavedSettings) -> bool {
    !matches!(
        saved_global(names::CONTEXT_AUTO_COMPACT, saved),
        Some(SettingValue::Bool(false))
    )
}

fn saved_ttl(saved: &SavedSettings) -> CacheTtl {
    saved_global(names::CACHE_TTL, saved)
        .and_then(|value| value.text().parse().ok())
        .unwrap_or(CacheTtl::OneHour)
}

fn compaction_notes(path: &SitePath, file: &Path) -> Option<Vec<String>> {
    if !path.spec().settles_on_save() {
        return None;
    }
    let text = std::fs::read_to_string(file).ok()?;
    let loaded = crate::context::load_text(Some(&text));
    let settings = loaded.settings;
    let saved = &loaded.saved;
    let default_endpoint = || {
        saved
            .global(names::ENDPOINT)
            .map_or_else(|| ENV.to_owned(), SettingValue::text)
    };
    let default_model = || resolved_model(&[], Some(saved));
    let (endpoint, model) = match &path.place {
        Place::Endpoint(endpoint) => (endpoint.clone(), default_model()),
        Place::Model(model) => (default_endpoint(), model.clone()),
        Place::EndpointModel { endpoint, model } => (endpoint.clone(), model.clone()),
        _ => (default_endpoint(), default_model()),
    };
    let (resolved, origin) =
        settings.resolve_with_point_origin(&endpoint, &model, None, DEFAULT_MAX_TOKENS);
    let point_saved = path.spec().name == names::COMPACT_AT;
    if point_saved && origin.is_none_or(|origin| origin != path.to_string()) {
        return None;
    }
    let configured = resolved.compact_at.as_ref().map(|(at, _)| *at);
    let point = resolved
        .window
        .as_ref()
        .map(|window| compact_at(window.tokens, DEFAULT_MAX_TOKENS, resolved.compact_at));
    let tokens = match (&point, configured) {
        (Some(point), _) => point.tokens,
        (None, Some(CompactAt::Tokens(tokens))) => tokens,
        (None, _) => return None,
    };
    let mut notes = Vec::new();
    let shown_model = model_words(&model);
    if point_saved && let Some(at) = configured {
        if let Some(window) = resolved.window.as_ref()
            && let Some(requested) = point.and_then(|point| point.capped_from)
        {
            let size = window_text(window);
            let verb = if auto_compact_on(saved) {
                "compacts at"
            } else {
                "would compact at"
            };
            let requested = group(requested);
            let lead = match at {
                CompactAt::Share(percent) => {
                    format!("{percent} is {requested} tokens on {shown_model}'s window {size}")
                }
                CompactAt::Tokens(_) => {
                    format!("{requested} is past the reserve of {shown_model}'s window {size}")
                }
            };
            notes.push(format!(
                "compact_at {lead}; with that window Wisp {verb} {} (window \u{2212} reserve).",
                group(tokens)
            ));
        }
        let prices = settings.price(&endpoint, &model, ProviderKind::IMPLICIT);
        notes.extend(low_point_warning(
            at,
            tokens,
            (shown_model, prices.as_ref()),
            saved_ttl(saved),
        ));
    }
    if settings.native_compaction(&endpoint, &model) {
        let layers = Layers {
            saved: Some(saved),
            ..Layers::default()
        };
        let current = Current {
            endpoint: Some(endpoint),
            model: model.clone(),
        };
        let band = wisp_core::settings::background_band(&layers, &current);
        notes.extend(background_point_warning(shown_model, tokens, band));
    }
    Some(notes)
}

pub(crate) fn doctor_rows(settings: &SavedSettings, keys_file: Option<&str>) -> Vec<String> {
    settings
        .iter()
        .map(|(path, saved)| {
            let mut value = saved.value.clone();
            if path.place == Place::Global && path.spec().kind == Kind::EndpointName {
                let shown = wisp_core::config::keys_file_shown(keys_file);
                wisp_core::config::shown_endpoint(&mut value, Some(shown));
            }
            format!("{} = {}", path_text(path), shown(path.spec(), &value))
        })
        .collect()
}
