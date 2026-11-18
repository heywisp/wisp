use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use wisp_protocol::IN_KEYCHAIN;
use wisp_protocol::log::CredentialSource;
use wisp_protocol::subscription_auth::SubscriptionProvider;

use crate::endpoints::{PathShown, ProviderKind};

pub use wisp_protocol::GIVEN_CREDENTIALS;

#[derive(Clone, PartialEq, Eq)]
pub enum Source {
    Environment,
    File(PathBuf),
    Keyring,
    CredentialFile { path: PathBuf, shown: PathShown },
}

impl Source {
    pub fn credential_source(&self) -> CredentialSource {
        match self {
            Self::Environment => CredentialSource::Environment,
            Self::File(_) => CredentialSource::EnvFile,
            Self::Keyring => CredentialSource::Keyring,
            Self::CredentialFile { .. } => CredentialSource::CredentialFile,
        }
    }
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment => f.write_str("Environment"),
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
            Self::Keyring => f.write_str("Keyring"),
            Self::CredentialFile { path, shown } => f
                .debug_struct("CredentialFile")
                .field("path", &shown.path(path))
                .field("shown", shown)
                .finish(),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment => f.write_str("the environment"),
            Self::File(path) => f.write_str(&crate::workspace::home_shown(path)),
            Self::Keyring => f.write_str(IN_KEYCHAIN),
            Self::CredentialFile {
                path,
                shown: PathShown::Default,
            } => f.write_str(&crate::workspace::home_shown(path)),
            Self::CredentialFile {
                shown: PathShown::Given,
                ..
            } => f.write_str(GIVEN_CREDENTIALS),
        }
    }
}

#[derive(Clone, Default)]
pub struct EnvFile {
    pub path: PathBuf,
    pub vars: BTreeMap<String, String>,
}

impl fmt::Debug for EnvFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvFile")
            .field("path", &self.path)
            .field("vars", &self.vars.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionKind {
    ChatGpt,
    Anthropic,
    Antigravity,
}

impl SubscriptionKind {
    pub const ALL: [Self; 3] = [Self::ChatGpt, Self::Anthropic, Self::Antigravity];

    pub fn billing(self) -> wisp_protocol::Billing {
        SubscriptionProvider::from(self).billing()
    }

    pub fn provider(self) -> ProviderKind {
        match self {
            Self::ChatGpt => ProviderKind::OpenAi,
            Self::Anthropic => ProviderKind::Anthropic,
            Self::Antigravity => ProviderKind::Gemini,
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Self::ChatGpt => super::openai::CHATGPT_BASE_URL,
            Self::Anthropic => super::anthropic::DEFAULT_BASE_URL,
            Self::Antigravity => super::gemini::antigravity_base_url(),
        }
    }

    pub const fn requires_oauth(self) -> bool {
        matches!(self, Self::ChatGpt | Self::Antigravity)
    }
}

impl From<SubscriptionKind> for SubscriptionProvider {
    fn from(kind: SubscriptionKind) -> Self {
        match kind {
            SubscriptionKind::ChatGpt => Self::ChatGpt,
            SubscriptionKind::Anthropic => Self::Claude,
            SubscriptionKind::Antigravity => Self::Antigravity,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    ApiKey(String),
    AuthToken(String),
    Subscription {
        kind: SubscriptionKind,
        secret: String,
        profile: Option<PathBuf>,
    },
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("ApiKey(<redacted>)"),
            Self::AuthToken(_) => f.write_str("AuthToken(<redacted>)"),
            Self::Subscription { kind, .. } => f
                .debug_struct("Subscription")
                .field("kind", kind)
                .finish_non_exhaustive(),
        }
    }
}

impl Credential {
    pub fn billing(&self) -> wisp_protocol::Billing {
        self.subscription()
            .map_or(wisp_protocol::Billing::Metered, SubscriptionKind::billing)
    }

    pub async fn access_token(&self) -> Result<String, super::ProviderError> {
        match self {
            Self::Subscription {
                kind,
                profile: Some(path),
                ..
            } => crate::oauth::access_token(path, *kind).await,
            _ => Ok(self.secret().to_owned()),
        }
    }

    pub(crate) async fn refresh_refused(
        &self,
        refused: &str,
    ) -> Result<String, super::ProviderError> {
        match self {
            Self::Subscription {
                kind,
                profile: Some(path),
                ..
            } => crate::oauth::refresh_refused(path, *kind, refused).await,
            _ => Err(super::ProviderError::new(
                wisp_protocol::ErrorKind::Auth,
                "subscription sign-in cannot be refreshed",
            )),
        }
    }

    pub fn subscription(&self) -> Option<SubscriptionKind> {
        match self {
            Self::Subscription { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    pub(crate) fn secret(&self) -> &str {
        match self {
            Self::ApiKey(v) | Self::AuthToken(v) | Self::Subscription { secret: v, .. } => v,
        }
    }
}

pub(crate) fn subscription_authentication_failed(provider: &str) -> String {
    format!("{provider} subscription authentication failed; sign in again with /account")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_sources_name_paths_under_home_with_a_tilde() {
        let Some(home) = crate::workspace::home().filter(|h| h.parent().is_some()) else {
            return;
        };
        let credentials = home.join(".config/wisp/credentials.json");
        assert_eq!(
            Source::File(home.join("keys.env")).to_string(),
            "~/keys.env"
        );
        assert_eq!(
            Source::File(PathBuf::from("/nonexistent-wisp-root/keys.env")).to_string(),
            "/nonexistent-wisp-root/keys.env"
        );
        assert_eq!(
            Source::CredentialFile {
                path: credentials.clone(),
                shown: PathShown::Default,
            }
            .to_string(),
            "~/.config/wisp/credentials.json"
        );
        assert_eq!(
            Source::CredentialFile {
                path: credentials,
                shown: PathShown::Given,
            }
            .to_string(),
            GIVEN_CREDENTIALS
        );
    }
}
