use std::collections::BTreeMap;

use wisp_core::config::SavedSettings;
use wisp_core::settings::{Current, Found, Given, Layers, resolve_one};
use wisp_protocol::settings::{
    Applies, DEFAULT_MODEL, Endpoints, Place, Secrecy, SettingSpec, SettingValue, Site, SitePath,
    Status, Target, TargetError, find, names, parse_target_among,
};

use wisp_protocol::setting_looks_like_key;
use wisp_render::transcript::copy::KEY_SHAPED_MODEL;

pub const KEY_ON_COMMAND_LINE: &str = "A key on the command line is saved in your shell history and visible to other programs, so nothing was saved. Replace that key with your provider, then use --key-stdin or the prompt.";

pub const SET_NOT_CONNECTION: &str =
    "is how the account connects, so --set doesn't change it for one run; use wisp account edit";

pub struct Flag {
    pub flag: &'static str,
    pub name: &'static str,
    pub value: SettingValue,
}

impl Flag {
    pub fn new(flag: &'static str, name: &'static str, value: SettingValue) -> Self {
        Self { flag, name, value }
    }
}

fn reserved_note(spec: &SettingSpec) -> Option<String> {
    match spec.status {
        Status::Reserved { note } => Some(format!("{} is {note}.", spec.name)),
        _ => None,
    }
}

pub fn target(text: &str) -> Result<Target, String> {
    target_among(text, &[])
}

pub fn target_among(text: &str, endpoints: &[&str]) -> Result<Target, String> {
    let target = parse_target_among(text, Endpoints::Configured(endpoints))
        .map_err(|e: TargetError| e.to_string())?;
    match reserved_note(target.spec()) {
        Some(note) => Err(note),
        None => Ok(target),
    }
}

pub fn target_key(target: &Target) -> String {
    match target {
        Target::Name(spec) => spec.name.to_owned(),
        Target::Path(path) => path_text(path),
    }
}

pub fn model_of(path: &SitePath) -> Option<&str> {
    match &path.place {
        Place::Model(m) | Place::EndpointModel { model: m, .. } => Some(m.as_str()),
        _ => None,
    }
}

pub fn error_name(path: &SitePath) -> String {
    match model_of(path) {
        Some(model) if hidden_model(model) => path.spec().name.to_owned(),
        _ => path_text(path),
    }
}

pub fn hidden_model(model: &str) -> bool {
    wisp_protocol::model_hidden(model)
}

pub fn model_words(model: &str) -> &str {
    wisp_protocol::model_words(model)
}

pub fn path_text(path: &SitePath) -> String {
    match path.place {
        Place::Global => path.spec().name.to_owned(),
        _ => path.shown(),
    }
}

pub fn origin_text(found: &Found) -> String {
    wisp_protocol::hide_quoted_models(&found.origin)
}

fn example_path(spec: &'static SettingSpec) -> String {
    let place = if spec.has_site(Site::Model) {
        Place::Model(DEFAULT_MODEL.to_owned())
    } else if spec.has_site(Site::EndpointModel) {
        Place::EndpointModel {
            endpoint: "NAME".to_owned(),
            model: DEFAULT_MODEL.to_owned(),
        }
    } else {
        Place::Endpoint("NAME".to_owned())
    };
    SitePath::new(place, spec).shown()
}

fn endpoint_of(target: &Target) -> String {
    match target {
        Target::Path(SitePath {
            place: Place::Endpoint(e) | Place::EndpointModel { endpoint: e, .. },
            ..
        }) => e.clone(),
        _ => "NAME".to_owned(),
    }
}

pub fn secret_refusal(target: &Target, by: &str) -> String {
    format!(
        "{} is a secret, so {by} doesn't change it; use wisp account edit {}.",
        target_key(target),
        endpoint_of(target)
    )
}

pub fn flag_layer(typed: Vec<Flag>, sets: &[String]) -> Result<Vec<Given>, String> {
    let mut given: Vec<Given> = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut add = |given: &mut Vec<Given>, item: Given| -> Result<(), String> {
        let key = target_key(&item.target);
        if let Some(before) = seen.get(&key) {
            return Err(if *before == item.origin {
                format!("{before} was given more than once for {key}")
            } else {
                format!("{before} and {} both set {key}; give one", item.origin)
            });
        }
        seen.insert(key, item.origin.clone());
        given.push(item);
        Ok(())
    };
    for flag in typed {
        let spec = find(flag.name).ok_or_else(|| flag.name.to_owned())?;
        if setting_looks_like_key(spec.kind, &flag.value) {
            return Err(format!("{}: {KEY_SHAPED_MODEL}", flag.flag));
        }
        add(
            &mut given,
            Given {
                target: Target::Name(spec),
                value: flag.value,
                origin: flag.flag.to_owned(),
            },
        )?;
    }
    for set in sets {
        let Some((name, value)) = set.split_once('=') else {
            return Err("--set needs NAME=VALUE (what was given isn't shown)".to_owned());
        };
        let target = target(name)?;
        let spec = target.spec();
        match spec.secrecy {
            Secrecy::Secret => {
                return Err(format!(
                    "{}\n{KEY_ON_COMMAND_LINE}",
                    secret_refusal(&target, "--set")
                ));
            }
            Secrecy::Hidden => {
                return Err(format!(
                    "{} is written by Wisp; --set doesn't take it",
                    spec.name
                ));
            }
            Secrecy::Plain => {}
        }
        let key = target_key(&target);
        if spec.applies == Applies::Connection {
            return Err(format!(
                "{key} {SET_NOT_CONNECTION} {}",
                endpoint_of(&target)
            ));
        }
        if let Target::Name(spec) = &target
            && !spec.has_site(Site::Global)
            && spec.flags.is_empty()
        {
            return Err(format!(
                "{} is set per account or model; give its path, such as {}",
                spec.name,
                example_path(spec)
            ));
        }
        let value = spec
            .parse(value)
            .map_err(|e| format!("--set {key} {} (what was given isn't shown)", e.rule))?;
        if setting_looks_like_key(spec.kind, &value) {
            return Err(format!("--set {key}: {KEY_SHAPED_MODEL}"));
        }
        crate::tools::checked(spec, &value).map_err(|e| format!("--set {key}: {e}"))?;
        if let Target::Path(path) = &target {
            crate::tools::switch_checked(path, &value, &std::collections::BTreeSet::new())
                .map_err(|e| format!("--set {key}: {e}"))?;
        }
        add(
            &mut given,
            Given {
                target,
                value,
                origin: format!("--set {key}"),
            },
        )?;
    }
    alias_clash(&given, &[])?;
    Ok(given)
}

pub fn alias_clash(flags: &[Given], saved: &[wisp_core::models::ModelRow]) -> Result<(), String> {
    let mut rows: BTreeMap<String, Vec<String>> = saved
        .iter()
        .map(|row| (row.id.clone(), row.aliases.clone()))
        .collect();
    for given in flags {
        if let (Target::Path(path), SettingValue::List(aliases)) = (&given.target, &given.value)
            && path.spec().name == names::ALIASES
            && let Place::Model(id) = &path.place
        {
            rows.insert(id.clone(), aliases.clone());
        }
    }
    let mut seen: Vec<&String> = Vec::new();
    for (id, aliases) in &rows {
        for name in std::iter::once(id).chain(aliases) {
            if seen.contains(&name) {
                let problem = wisp_core::models::RowProblem::Duplicate {
                    model: name.clone(),
                };
                return Err(format!("{problem}; nothing was changed"));
            }
            seen.push(name);
        }
    }
    Ok(())
}

pub fn resolved_model(flags: &[Given], saved: Option<&SavedSettings>) -> String {
    let layers = Layers {
        flags: flags.to_vec(),
        saved,
        ..Layers::default()
    };
    let current = Current {
        endpoint: None,
        model: DEFAULT_MODEL.to_owned(),
    };
    find(names::MODEL)
        .map(|spec| resolve_one(spec, &layers, &current))
        .and_then(|r| r.value().map(SettingValue::text))
        .unwrap_or_else(|| DEFAULT_MODEL.to_owned())
}

pub fn flag_value<'a>(flags: &'a [Given], name: &str) -> Option<&'a SettingValue> {
    flags
        .iter()
        .find(|g| matches!(&g.target, Target::Name(s) if s.name == name)
            || matches!(&g.target, Target::Path(p) if p.place == Place::Global && p.spec().name == name))
        .map(|g| &g.value)
}

pub mod host;
#[cfg(test)]
mod tests;
mod used;
