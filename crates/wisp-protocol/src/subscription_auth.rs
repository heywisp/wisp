use serde::{Deserialize, Serialize};

use crate::endpoint::StoreKind;
use crate::{CodeProblem, RequestId, Secret};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionProvider {
    ChatGpt,
    Claude,
    Antigravity,
}

impl SubscriptionProvider {
    pub const ALL: [Self; 3] = [Self::Claude, Self::ChatGpt, Self::Antigravity];
    pub const API_KEY_WORDS: &str = "Claude, OpenAI, Gemini or a gateway";

    pub fn from_name(word: &str) -> Option<Self> {
        match word {
            "claude" => Some(Self::Claude),
            "chatgpt" => Some(Self::ChatGpt),
            "antigravity" => Some(Self::Antigravity),
            _ => None,
        }
    }

    pub fn default_name(self) -> &'static str {
        match self {
            Self::ChatGpt => "chatgpt",
            Self::Claude => "claude",
            Self::Antigravity => "antigravity",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT",
            Self::Claude => "Claude",
            Self::Antigravity => "Antigravity",
        }
    }

    pub const fn subscription_words(self) -> &'static str {
        match self {
            Self::Claude => "Pro or Max subscription",
            Self::ChatGpt => "Plus or Pro subscription",
            Self::Antigravity => "Gemini subscription",
        }
    }

    pub fn billing(self) -> crate::Billing {
        match self {
            Self::ChatGpt => crate::Billing::ChatGptPlan,
            Self::Claude => crate::Billing::AnthropicPlan,
            Self::Antigravity => crate::Billing::AntigravityPlan,
        }
    }

    pub fn login_hint(self) -> String {
        format!(
            "{} subscription sign-in uses wisp login {}",
            self.title(),
            self.default_name()
        )
    }

    pub fn names() -> [&'static str; Self::ALL.len()] {
        Self::ALL.map(Self::default_name)
    }

    pub fn names_or() -> String {
        crate::settings::listed(&Self::names(), "or")
    }

    pub fn names_pipe() -> String {
        Self::names().join("|")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum AuthCommand {
    Start {
        request: RequestId,
        provider: SubscriptionProvider,
        alias: Option<String>,
        #[serde(default)]
        reauthorize: bool,
    },
    Cancel {
        request: RequestId,
        login: RequestId,
    },
    Finish {
        request: RequestId,
        login: RequestId,
        response: Option<Secret>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
    },
    Open {
        request: RequestId,
        login: RequestId,
    },
    Status {
        request: RequestId,
        alias: Option<String>,
    },
    Reload {
        request: RequestId,
        alias: String,
    },
    Logout {
        request: RequestId,
        alias: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        also: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        confirmed: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        follow: Vec<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        gone: bool,
    },
}

impl AuthCommand {
    pub fn logout(
        request: RequestId,
        names: &[String],
        confirmed: bool,
        chat: Option<String>,
        follow: Vec<String>,
    ) -> Self {
        Self::Logout {
            request,
            alias: names.first().cloned().unwrap_or_default(),
            also: names.iter().skip(1).cloned().collect(),
            chat,
            confirmed,
            follow,
            gone: false,
        }
    }

    pub fn request(&self) -> RequestId {
        match self {
            Self::Start { request, .. }
            | Self::Cancel { request, .. }
            | Self::Finish { request, .. }
            | Self::Open { request, .. }
            | Self::Status { request, .. }
            | Self::Logout { request, .. }
            | Self::Reload { request, .. } => *request,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeKind {
    BrowserLoopback,
    ManualCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivateChallenge {
    pub login: RequestId,
    pub provider: SubscriptionProvider,
    pub alias: String,
    pub kind: ChallengeKind,
    pub authorization_url: Secret,
    pub expires_in_secs: u64,
    #[serde(default)]
    pub opened: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalState {
    Working,
    Expired,
    Refreshable,
    Unavailable,
    SetupTokenExpiryUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionEndpoint {
    pub alias: String,
    pub provider: SubscriptionProvider,
    pub storage: Option<StoreKind>,
    pub is_default: bool,
    pub state: LocalState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthFailureKind {
    UnsupportedProvider,
    InvalidAlias,
    AliasConflict,
    Busy,
    ActiveEndpoint,
    DefaultEndpoint,
    PresetReference,
    ChildReference,
    StaleRequest,
    Expired,
    Declined,
    Callback,
    CodeRejected,
    Unreachable,
    Storage,
    Unavailable,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Way {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stay: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub charges: bool,
}

impl Way {
    pub fn follows(agent: Option<&str>, to: Option<&str>) -> bool {
        to.is_some() && agent == to
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Followed {
    pub agent: String,
    pub model: String,
    pub effort: crate::Effort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Unusable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stayed {
    pub agent: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub charges: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Unusable>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetGone {
    pub preset: String,
    pub account: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub runs: bool,
}

pub const CANT_BE_READ: &str = "can't be read now";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unusable {
    pub account: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sign_in: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AuthOutcome {
    Challenge {
        challenge: PrivateChallenge,
    },
    Ready {
        endpoint: SubscriptionEndpoint,
        previous_cleanup_pending: bool,
    },
    SavedNotReady {
        endpoint: SubscriptionEndpoint,
        previous_cleanup_pending: bool,
    },
    Status {
        endpoints: Vec<SubscriptionEndpoint>,
    },
    LoggedOut {
        alias: String,
        remote_revoked: bool,
        cleanup_pending: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        also: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        left_sign_ins: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        left_keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_start: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        moved: Vec<Followed>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stay: Vec<Stayed>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stopped: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        presets: Vec<PresetGone>,
    },
    LogoutNeeds {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deletes_keys_of: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_start: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ways: Vec<Way>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        presets: Vec<PresetGone>,
    },
    Cancelled {
        login: RequestId,
    },
    Failed {
        kind: AuthFailureKind,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<CodeProblem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        same_account: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthEvent {
    pub request: RequestId,
    pub outcome: AuthOutcome,
}

impl AuthEvent {
    pub fn is_private(&self) -> bool {
        matches!(self.outcome, AuthOutcome::Challenge { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_follows_only_onto_its_own_provider() {
        assert!(Way::follows(Some("anthropic"), Some("anthropic")));
        assert!(!Way::follows(Some("anthropic"), Some("openai")));
        assert!(!Way::follows(None, None));
    }

    #[test]
    fn auth_wire_is_request_correlated_and_secrets_only_appear_on_private_transport() {
        let command = AuthCommand::Finish {
            request: RequestId(2),
            login: RequestId(1),
            response: Some(Secret::new("CODE_STATE_CANARY")),
            alias: None,
        };
        assert_eq!(
            serde_json::from_str::<AuthCommand>(&serde_json::to_string(&command).unwrap()).unwrap(),
            command
        );
        assert_eq!(command.request(), RequestId(2));
        assert!(!format!("{command:?}").contains("CODE_STATE_CANARY"));
        assert!(
            serde_json::from_str::<AuthCommand>(
                r#"{"operation":"start","request":1,"provider":"unknown","alias":null}"#
            )
            .is_err()
        );
        let outcome = AuthOutcome::SavedNotReady {
            endpoint: SubscriptionEndpoint {
                alias: "plan".to_owned(),
                provider: SubscriptionProvider::ChatGpt,
                storage: Some(StoreKind::File),
                is_default: false,
                state: LocalState::Unavailable,
            },
            previous_cleanup_pending: false,
        };
        let value = serde_json::to_value(outcome).unwrap();
        assert_eq!(value["outcome"], "saved_not_ready");
        for field in [
            "token",
            "authorization_url",
            "state_token",
            "code",
            "account_id",
            "email",
            "account_uuid",
            "organization",
            "account_hash",
        ] {
            assert!(value.get(field).is_none());
        }
        let reload = AuthCommand::Reload {
            request: RequestId(3),
            alias: "plan".to_owned(),
        };
        assert_eq!(reload.request(), RequestId(3));
    }

    #[test]
    fn older_auth_wire_still_reads_and_new_fields_default() {
        let finish: AuthCommand =
            serde_json::from_str(r#"{"operation":"finish","request":2,"login":1,"response":null}"#)
                .unwrap();
        assert_eq!(
            finish,
            AuthCommand::Finish {
                request: RequestId(2),
                login: RequestId(1),
                response: None,
                alias: None,
            }
        );
        let challenge: PrivateChallenge = serde_json::from_str(
            r#"{"login":1,"provider":"claude","alias":"claude","kind":"manual_code","authorization_url":"https://claude.ai/oauth/authorize","expires_in_secs":600}"#,
        )
        .unwrap();
        assert!(!challenge.opened);
        let failed: AuthOutcome =
            serde_json::from_str(r#"{"outcome":"failed","kind":"code_rejected","message":"m"}"#)
                .unwrap();
        assert_eq!(
            failed,
            AuthOutcome::Failed {
                kind: AuthFailureKind::CodeRejected,
                message: "m".to_owned(),
                problem: None,
                same_account: None,
            }
        );
        let rejected = AuthOutcome::Failed {
            kind: AuthFailureKind::CodeRejected,
            message: "m".to_owned(),
            problem: Some(CodeProblem::Repeated),
            same_account: None,
        };
        let written = serde_json::to_value(&rejected).unwrap();
        assert_eq!(written["problem"], "repeated");
        assert!(written.get("same_account").is_none());
        let open = AuthCommand::Open {
            request: RequestId(4),
            login: RequestId(1),
        };
        assert_eq!(open.request(), RequestId(4));
        assert_eq!(serde_json::to_value(&open).unwrap()["operation"], "open");
    }

    #[test]
    fn each_provider_suggests_one_plain_name() {
        assert_eq!(
            SubscriptionProvider::ALL,
            [
                SubscriptionProvider::Claude,
                SubscriptionProvider::ChatGpt,
                SubscriptionProvider::Antigravity,
            ]
        );
        assert_eq!(SubscriptionProvider::Claude.default_name(), "claude");
        assert_eq!(SubscriptionProvider::ChatGpt.default_name(), "chatgpt");
        assert_eq!(
            SubscriptionProvider::Antigravity.default_name(),
            "antigravity"
        );
        assert_eq!(SubscriptionProvider::Claude.title(), "Claude");
        assert_eq!(SubscriptionProvider::ChatGpt.title(), "ChatGPT");
        assert_eq!(SubscriptionProvider::Antigravity.title(), "Antigravity");
        assert_eq!(
            SubscriptionProvider::Claude.subscription_words(),
            "Pro or Max subscription"
        );
        assert_eq!(
            SubscriptionProvider::ChatGpt.subscription_words(),
            "Plus or Pro subscription"
        );
        assert_eq!(
            SubscriptionProvider::Antigravity.subscription_words(),
            "Gemini subscription"
        );
        assert_eq!(
            SubscriptionProvider::Antigravity.billing(),
            crate::Billing::AntigravityPlan
        );
        for p in SubscriptionProvider::ALL {
            assert_eq!(SubscriptionProvider::from_name(p.default_name()), Some(p));
        }
        assert_eq!(
            SubscriptionProvider::Antigravity.login_hint(),
            "Antigravity subscription sign-in uses wisp login antigravity"
        );
        assert_eq!(
            SubscriptionProvider::names_or(),
            "claude, chatgpt or antigravity"
        );
        assert_eq!(
            SubscriptionProvider::names_pipe(),
            "claude|chatgpt|antigravity"
        );
        assert_eq!(SubscriptionProvider::from_name("gemini"), None);
    }
}
