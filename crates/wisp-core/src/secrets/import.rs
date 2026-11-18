use std::fmt;

use wisp_protocol::settings::listed;
use wisp_protocol::{Secret, VariableName};

use crate::endpoints::{
    AuthKind, ConfigFile, EndpointName, KeyFrom, ProviderKind, UrlSource, Variables, clean,
    http_url,
};
use crate::provider::credential::{Credential, EnvFile};

pub type Lookup<'a> = &'a (dyn Fn(&str) -> Option<String> + Sync);

pub fn read_file(path: &std::path::Path, explicit: bool) -> Result<Option<EnvFile>, String> {
    super::read_env_file(path, explicit)
}

#[derive(Clone, Copy)]
pub enum Place<'a> {
    Environment(Lookup<'a>),
    File(&'a EnvFile),
}

impl fmt::Debug for Place<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment(_) => f.write_str("Environment"),
            Self::File(file) => f.debug_tuple("File").field(file).finish(),
        }
    }
}

pub fn read_var(from: Place<'_>, name: &VariableName) -> Option<String> {
    match from {
        Place::Environment(lookup) => clean(lookup(name.raw())),
        Place::File(file) => clean(file.vars.get(name.raw()).cloned()),
    }
}

pub struct Source<'a> {
    pub from: Place<'a>,
    pub words: String,
}

pub struct Other<'a> {
    pub has: &'a dyn Fn(&VariableName) -> bool,
    pub words: String,
    pub hint: String,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Origin {
    Provider,
    Table(EndpointName),
}

#[derive(Clone, PartialEq, Eq)]
pub struct Found {
    pub origin: Origin,
    pub provider: ProviderKind,
    pub auth: AuthKind,
    pub key: Secret,
    pub url: Option<Secret>,
    pub config_url: Option<String>,
    pub variables: Vec<VariableName>,
    pub warning: Option<String>,
}

impl fmt::Debug for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let origin = match &self.origin {
            Origin::Provider => "Provider".to_owned(),
            Origin::Table(name) => format!("Table({name})"),
        };
        f.debug_struct("Found")
            .field("origin", &origin)
            .field("provider", &self.provider)
            .field("auth", &self.auth)
            .field("key", &self.key)
            .field("url", &self.url)
            .field(
                "config_url",
                &self.config_url.as_deref().map(crate::endpoints::origin),
            )
            .field("variables", &self.variables)
            .field("warning", &self.warning)
            .finish()
    }
}

pub fn default_alias(provider: ProviderKind) -> &'static str {
    wisp_protocol::default_api_account_name(provider.as_str())
}

impl Found {
    pub fn default_alias(&self) -> &'static str {
        default_alias(self.provider)
    }

    pub fn base_url(&self) -> String {
        match (&self.url, &self.config_url) {
            (Some(url), _) => url.expose().to_owned(),
            (None, Some(url)) => url.clone(),
            (None, None) => self.provider.default_url().to_owned(),
        }
    }

    pub fn names(&self) -> String {
        let names: Vec<String> = self.variables.iter().map(ToString::to_string).collect();
        and_words(&names)
    }

    fn same_as(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.key == other.key
            && self.base_url().trim_end_matches('/') == other.base_url().trim_end_matches('/')
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub what: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Findings {
    pub found: Vec<Found>,
    pub skipped: Vec<Skipped>,
}

pub fn and_words(items: &[impl AsRef<str>]) -> String {
    listed(items, "and")
}

fn provider_found(
    provider: ProviderKind,
    source: &Source<'_>,
    other: Option<&Other<'_>>,
) -> Result<Option<Found>, Skipped> {
    let empty = |_: &str| None;
    let variables = match source.from {
        Place::Environment(lookup) => Variables::new(lookup, None, false),
        Place::File(file) => Variables::new(&empty, Some(file), false),
    };
    let resolved = provider.implicit(&variables);
    let endpoint = match resolved {
        Ok(Some(endpoint)) => endpoint,
        Ok(None) => return Ok(None),
        Err(e) => {
            return Err(Skipped {
                what: provider.as_str().to_owned(),
                reason: e.to_string(),
            });
        }
    };
    let url_var = provider.url_variable();
    let key_var = endpoint.key_var().clone();
    let mut variables = vec![key_var.clone()];
    let url = match endpoint.url_source {
        UrlSource::Variable(..) => {
            variables.push(url_var.clone());
            if http_url(&endpoint.base_url).is_none() {
                return Err(Skipped {
                    what: provider.as_str().to_owned(),
                    reason: format!("{url_var} is not an http or https URL"),
                });
            }
            Some(Secret::new(endpoint.base_url.clone()))
        }
        _ => None,
    };
    let bearer = matches!(endpoint.credential, Credential::AuthToken(_));
    let warning = match other {
        Some(other) if bearer && url.is_none() && (other.has)(url_var) => Some(format!(
            "{url_var} is set in {}, not in {}; this token will be checked against the provider's own URL",
            other.words, source.words
        )),
        _ => None,
    };
    let auth = if bearer {
        AuthKind::Bearer
    } else {
        AuthKind::ApiKey
    };
    Ok(Some(Found {
        origin: Origin::Provider,
        provider,
        auth,
        key: Secret::new(endpoint.credential.secret().to_owned()),
        url,
        config_url: None,
        variables,
        warning,
    }))
}

fn table_found(
    name: &EndpointName,
    config: &crate::endpoints::EndpointConfig,
    key_env: &VariableName,
    source: &Source<'_>,
    other: Option<&Other<'_>>,
) -> Result<Found, Skipped> {
    let skip = |reason: String| {
        let both = other.is_some_and(|o| {
            (o.has)(key_env) && config.url_env.as_ref().is_none_or(|u| (o.has)(u))
        });
        let reason = match other {
            Some(o) if both => format!("{reason}; try {}", o.hint),
            _ => reason,
        };
        Skipped {
            what: name.to_string(),
            reason,
        }
    };
    let Some(key) = read_var(source.from, key_env) else {
        return Err(skip(format!("{key_env} is not set in {}", source.words)));
    };
    let mut variables = vec![key_env.clone()];
    let url = match &config.url_env {
        None => None,
        Some(var) => {
            variables.push(var.clone());
            match read_var(source.from, var) {
                Some(url) if http_url(&url).is_some() => Some(Secret::new(url)),
                Some(_) => return Err(skip(format!("{var} is not an http or https URL"))),
                None => {
                    return Err(skip(format!(
                        "{var} is not set in {}, where {key_env} was found",
                        source.words
                    )));
                }
            }
        }
    };
    Ok(Found {
        origin: Origin::Table(name.clone()),
        provider: config.provider,
        auth: config.auth,
        key: Secret::new(key),
        url,
        config_url: config.url.clone(),
        variables,
        warning: None,
    })
}

pub fn find(
    config: Option<&ConfigFile>,
    source: &Source<'_>,
    other: Option<&Other<'_>>,
) -> Findings {
    let mut findings = Findings::default();
    if let Some(config) = config {
        for (name, endpoint) in &config.endpoints {
            let KeyFrom::Variable(key_env) = &endpoint.key else {
                continue;
            };
            match table_found(name, endpoint, key_env, source, other) {
                Ok(found) => findings.found.push(found),
                Err(skipped) => findings.skipped.push(skipped),
            }
        }
    }
    for provider in ProviderKind::ALL {
        match provider_found(provider, source, other) {
            Ok(Some(found)) if findings.found.iter().any(|f| f.same_as(&found)) => {}
            Ok(Some(found)) => findings.found.push(found),
            Ok(None) => {}
            Err(skipped) => findings.skipped.push(skipped),
        }
    }
    findings
}

#[cfg(test)]
mod tests;
