use std::collections::BTreeMap;

use wisp_protocol::VariableName;

use crate::config::McpServer;
use crate::endpoints::{ConfigFile, KeyFrom, ProviderKind, clean, url_redactions};
use crate::session::Redactions;

use super::env_file::line_values;

pub(crate) struct Values<'a> {
    env: &'a dyn Fn(&str) -> Option<String>,
    file: BTreeMap<String, String>,
}

impl<'a> Values<'a> {
    pub(crate) fn new(
        env: &'a dyn Fn(&str) -> Option<String>,
        file: BTreeMap<String, String>,
    ) -> Self {
        Self { env, file }
    }

    pub(crate) fn provider(&self, provider: ProviderKind) -> Redactions {
        provider
            .implicit_variables()
            .iter()
            .fold(self.url(provider.url_variable()), |items, name| {
                items.union([&self.whole(name)])
            })
    }

    fn each(&self, name: &VariableName) -> Vec<(String, String)> {
        [(self.env)(name.raw()), self.file.get(name.raw()).cloned()]
            .into_iter()
            .flatten()
            .filter_map(|raw| clean(Some(raw.clone())).map(|cleaned| (raw, cleaned)))
            .collect()
    }

    fn whole(&self, name: &VariableName) -> Redactions {
        Redactions::new(
            self.each(name)
                .into_iter()
                .flat_map(|(raw, cleaned)| [cleaned, raw]),
        )
    }

    fn url(&self, name: &VariableName) -> Redactions {
        self.each(name)
            .into_iter()
            .flat_map(|(raw, cleaned)| [cleaned, raw])
            .fold(Redactions::default(), |items, value| {
                items.union([&url_redactions(&value)])
            })
    }
}

const KEY_NAME_WORDS: [&str; 4] = ["KEY", "TOKEN", "SECRET", "PASSWORD"];

pub fn named_key_values(vars: impl IntoIterator<Item = (String, String)>) -> Redactions {
    Redactions::new(vars.into_iter().filter_map(|(name, value)| {
        let name = name.to_ascii_uppercase();
        (KEY_NAME_WORDS.iter().any(|word| name.contains(word))
            && crate::session::maskable(&value)
            && !value.starts_with(['/', '~', '.']))
        .then_some(value)
    }))
}

pub fn variable_redactions<'a>(
    config: Option<&'a ConfigFile>,
    run_servers: impl IntoIterator<Item = &'a McpServer>,
    env: &dyn Fn(&str) -> Option<String>,
    env_file_text: Option<&str>,
) -> Redactions {
    let values = Values::new(env, env_file_text.map(line_values).unwrap_or_default());
    let mut items = Redactions::default();
    for provider in ProviderKind::ALL {
        items = items.union([&values.provider(provider)]);
    }
    for endpoint in config.iter().flat_map(|c| c.endpoints.values()) {
        if let KeyFrom::Variable(key_env) = &endpoint.key {
            items = items.union([&values.whole(key_env)]);
        }
        if let Some(url_env) = &endpoint.url_env {
            items = items.union([&values.url(url_env)]);
        }
        if let Some(url) = &endpoint.url {
            items = items.union([&url_redactions(url)]);
        }
    }
    let keys = config
        .into_iter()
        .flat_map(|c| c.mcp_servers.values())
        .chain(run_servers)
        .flat_map(|server| &server.keys)
        .filter_map(|name| env(name))
        .filter(|value| crate::session::maskable(value));
    items.union([&Redactions::new(keys)])
}

#[cfg(any(test, feature = "test-support"))]
pub fn live_redactions(env: &dyn Fn(&str) -> Option<String>) -> Redactions {
    let values: Vec<String> = ProviderKind::ALL
        .into_iter()
        .map(|provider| provider.url_variable().raw())
        .chain(["MANIFEST_API_KEY", "MANIFEST_BASE_URL"])
        .filter_map(env)
        .collect();
    let hosts: Vec<String> = values.iter().flat_map(|v| url_parts(v)).collect();
    variable_redactions(None, [], env, None)
        .union([&Redactions::new(values.into_iter().chain(hosts))])
}

#[cfg(any(test, feature = "test-support"))]
fn url_parts(url: &str) -> Vec<String> {
    let Some((_, rest)) = url.split_once("://") else {
        return Vec::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, after)| after);
    let host = if host_port.starts_with('[') {
        host_port
            .split_once(']')
            .map_or(host_port, |(h, _)| h.trim_start_matches('['))
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    [authority, host_port, host]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests;
