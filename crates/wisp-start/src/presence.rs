use std::collections::BTreeSet;

use wisp_core::endpoints::{ConfigFile, EndpointName, KeyFrom, ProviderKind};
use wisp_core::secrets::VariableName;
use wisp_core::secrets::import::and_words;
use wisp_protocol::CONFIG_FILE;

use crate::keychain::{EnvScan, EnvText, KeychainInputs, Unscanned};

pub fn provider_keys() -> BTreeSet<VariableName> {
    ProviderKind::ALL
        .iter()
        .flat_map(|p| p.implicit_variables().iter().cloned())
        .collect()
}

pub fn provider_names() -> BTreeSet<VariableName> {
    let mut names = provider_keys();
    names.extend(ProviderKind::ALL.iter().map(|p| p.url_variable().clone()));
    names
}

pub fn asked(config: Option<&ConfigFile>) -> BTreeSet<VariableName> {
    let mut names = provider_names();
    for endpoint in config.iter().flat_map(|c| c.endpoints.values()) {
        if let KeyFrom::Variable(key_env) = &endpoint.key {
            names.insert(key_env.clone());
        }
        names.extend(endpoint.url_env.iter().cloned());
    }
    names
}

pub fn exported(names: &BTreeSet<VariableName>) -> BTreeSet<VariableName> {
    names
        .iter()
        .filter(|name| is_set(name.raw()))
        .cloned()
        .collect()
}

pub use wisp_core::secrets::is_set;

pub fn in_file(text: &EnvText, names: &BTreeSet<VariableName>) -> BTreeSet<VariableName> {
    wisp_core::secrets::env_file_names(text.as_str(), names)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Environment,
    File,
}

#[derive(Debug, Default)]
pub struct Presence {
    pub env: BTreeSet<VariableName>,
    pub file: BTreeSet<VariableName>,
    pub tables: Vec<(EndpointName, Option<Place>)>,
}

impl Presence {
    pub fn scan(inputs: &KeychainInputs, config: Option<&ConfigFile>) -> Self {
        Self::of(
            exported(&asked(config)),
            inputs
                .env_text()
                .map(|text| in_file(text, &asked(config)))
                .unwrap_or_default(),
            config,
        )
    }

    pub fn of(
        env: BTreeSet<VariableName>,
        file: BTreeSet<VariableName>,
        config: Option<&ConfigFile>,
    ) -> Self {
        let mut tables = Vec::new();
        for (name, endpoint) in config.iter().flat_map(|c| c.endpoints.iter()) {
            if let KeyFrom::Variable(key_env) = &endpoint.key {
                let place = if env.contains(key_env) {
                    Some(Place::Environment)
                } else if file.contains(key_env) {
                    Some(Place::File)
                } else {
                    None
                };
                tables.push((name.clone(), place));
            }
        }
        Self { env, file, tables }
    }

    fn provider_keys_in(&self, place: Place) -> Vec<VariableName> {
        let set = match place {
            Place::Environment => &self.env,
            Place::File => &self.file,
        };
        provider_keys()
            .into_iter()
            .filter(|name| set.contains(name))
            .collect()
    }

    fn provider_in(&self, place: Place) -> Vec<String> {
        let keys = self.provider_keys_in(place);
        if keys.is_empty() {
            return Vec::new();
        }
        let set = match place {
            Place::Environment => &self.env,
            Place::File => &self.file,
        };
        let mut names: Vec<String> = keys.iter().map(ToString::to_string).collect();
        for provider in ProviderKind::ALL {
            if set.contains(provider.url_variable()) {
                names.push(provider.url_variable().to_string());
            }
        }
        names
    }

    fn tables_in(&self, place: Place) -> Vec<&EndpointName> {
        self.tables
            .iter()
            .filter(|(_, p)| *p == Some(place))
            .map(|(name, _)| name)
            .collect()
    }

    pub fn found(&self, config: Option<&ConfigFile>) -> Vec<VariableName> {
        let mut names: BTreeSet<VariableName> = BTreeSet::new();
        for place in [Place::Environment, Place::File] {
            names.extend(self.provider_keys_in(place));
        }
        for (_, endpoint) in config.iter().flat_map(|c| c.endpoints.iter()) {
            if let KeyFrom::Variable(key_env) = &endpoint.key
                && (self.env.contains(key_env) || self.file.contains(key_env))
            {
                names.insert(key_env.clone());
            }
        }
        names.into_iter().collect()
    }

    pub fn importable(&self, place: Place) -> bool {
        !self.provider_keys_in(place).is_empty() || !self.tables_in(place).is_empty()
    }

    pub fn any_importable(&self) -> bool {
        self.importable(Place::Environment) || self.importable(Place::File)
    }

    pub fn offer_lines(&self, file_words: &str) -> Vec<String> {
        let mut lines = Vec::new();
        for (place, words) in [
            (Place::Environment, "your environment"),
            (Place::File, file_words),
        ] {
            let names = self.provider_in(place);
            if !names.is_empty() {
                lines.push(format!("{} in {words}", and_words(&names)));
            }
        }
        for (place, words) in [
            (Place::Environment, "your environment"),
            (Place::File, file_words),
        ] {
            for name in self.tables_in(place) {
                lines.push(format!("{name} in {CONFIG_FILE}, with its key in {words}"));
            }
        }
        lines
    }

    pub fn unread_message(&self, file_words: &str) -> Option<String> {
        let lead = "no account is set up yet.";
        let file_keys = self.provider_keys_in(Place::File);
        let file_tables = !self.tables_in(Place::File).is_empty();
        let env_keys = self.provider_keys_in(Place::Environment);
        let message = if !file_keys.is_empty() || file_tables {
            let names: Vec<String> = file_keys.iter().map(ToString::to_string).collect();
            let what = match (names.is_empty(), file_tables) {
                (false, false) => and_words(&names),
                (false, true) => format!("{} and the keys {CONFIG_FILE} names", and_words(&names)),
                _ => format!("the keys {CONFIG_FILE} names"),
            };
            let it = if names.len() == 1 && !file_tables {
                "it"
            } else {
                "them"
            };
            format!(
                "{lead} {file_words} holds {what}, but this machine has a keychain, so Wisp doesn't read {it}. Store {it} once with: wisp account import"
            )
        } else if !env_keys.is_empty() {
            let names: Vec<String> = env_keys.iter().map(ToString::to_string).collect();
            let (verb, it) = if names.len() == 1 {
                ("is", "it")
            } else {
                ("are", "them")
            };
            format!(
                "{lead} {} {verb} set, but this machine has a keychain, so Wisp doesn't read {it}. Store {it} once with: wisp account import --from-environment",
                and_words(&names)
            )
        } else if !self.tables.is_empty() {
            let command = if self.tables_in(Place::Environment).is_empty() {
                "wisp account import --env-file FILE"
            } else {
                "wisp account import --from-environment"
            };
            format!(
                "{lead} {CONFIG_FILE}'s accounts name variables, which Wisp doesn't read on a machine with a keychain. Store their keys once with: {command}"
            )
        } else {
            return None;
        };
        Some(format!(
            "{message}\nFor one run without the keychain: --keychain off"
        ))
    }
}

pub fn scan_problem(inputs: &KeychainInputs, file_words: &str) -> Option<String> {
    let unread = "it isn't read on this machine";
    match inputs.scan()? {
        EnvScan::Missing => None,
        EnvScan::Text(text) => wisp_core::secrets::env_text_problem(text.as_str())
            .map(|line| format!("{file_words} doesn't parse (line {line}); {unread}")),
        EnvScan::Failed(Unscanned::NotUtf8(_)) => {
            Some(format!("{file_words} is not valid UTF-8; {unread}"))
        }
        EnvScan::Failed(Unscanned::GivenMissing) => Some(wisp_core::secrets::missing_env_file()),
        EnvScan::Failed(Unscanned::Unreadable(kind)) => {
            Some(format!("could not read {file_words}: {kind}"))
        }
    }
}

#[cfg(test)]
mod tests;
