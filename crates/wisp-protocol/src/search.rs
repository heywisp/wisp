use serde::{Deserialize, Serialize};

pub const SERVER_TOOL_USE: &str = "server_tool_use";
pub const SEARCH_RESULT: &str = "web_search_tool_result";
pub const SEARCH_CITATION: &str = "web_search_result_location";
pub const WEB_SEARCH: &str = "web_search";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_age: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchAction {
    #[default]
    Search,
    OpenPage,
    FindInPage,
    #[serde(other)]
    Other,
}

impl SearchAction {
    pub fn is_search(&self) -> bool {
        *self == Self::Search
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchOutcome {
    Results {
        hits: Vec<SearchHit>,
    },
    Failed {
        failure: SearchFailure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchFailure {
    RateLimited,
    InvalidQuery,
    Unavailable,
    Failed,
    #[serde(untagged)]
    Other(String),
}

impl SearchFailure {
    pub fn as_str(&self) -> &str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::InvalidQuery => "invalid_query",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
            Self::Other(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SearchEnd {
    Done(SearchOutcome),
    Unfinished {
        reason: UnfinishedReason,
        possibly_billed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnfinishedReason {
    Stopped,
    Paused,
    Failed,
    Refused,
    Compacted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitedSource {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoSearch {
    NotRequested,
    NotSupported,
    TurnedOff,
    NotStarted,
    Refused,
}

impl NoSearch {
    pub const fn why(self) -> Option<&'static str> {
        match self {
            Self::NotRequested => None,
            Self::NotSupported => Some("not on this model"),
            Self::TurnedOff => Some("off for this account"),
            Self::NotStarted => Some("not in this chat"),
            Self::Refused => Some("not on this account"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchForm {
    Native,
    None(NoSearch),
}

impl SearchForm {
    pub const fn is_native(self) -> bool {
        matches!(self, Self::Native)
    }

    pub fn kept(self, had: Option<bool>) -> Self {
        match (had, self) {
            (Some(false), Self::Native) => Self::None(NoSearch::NotStarted),
            (Some(true), Self::None(NoSearch::TurnedOff)) => Self::Native,
            _ => self,
        }
    }

    pub const fn word(self) -> &'static str {
        if self.is_native() { "on" } else { "off" }
    }

    pub fn words(self) -> String {
        match self {
            Self::None(why) => why.why().map_or_else(
                || self.word().to_owned(),
                |why| format!("{} ({why})", self.word()),
            ),
            Self::Native => self.word().to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resend {
    BreakpointLimit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderingForm {
    #[default]
    Text,
    WithoutThinking,
    Flattened,
}

impl RenderingForm {
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Text => Some(Self::WithoutThinking),
            Self::WithoutThinking => Some(Self::Flattened),
            Self::Flattened => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn outcomes_round_trip() {
        let results = SearchEnd::Done(SearchOutcome::Results {
            hits: vec![SearchHit {
                title: "Ripgrep".into(),
                url: "https://example.com/rg".into(),
                snippet: None,
                page_age: Some("2 days ago".into()),
            }],
        });
        let value = serde_json::to_value(&results).unwrap();
        assert_eq!(
            value,
            json!({"status": "done", "type": "results", "hits": [
                {"title": "Ripgrep", "url": "https://example.com/rg", "page_age": "2 days ago"}
            ]})
        );
        assert_eq!(serde_json::from_value::<SearchEnd>(value).unwrap(), results);
        let failed = SearchEnd::Done(SearchOutcome::Failed {
            failure: SearchFailure::RateLimited,
            code: Some("too_many_requests".into()),
        });
        let value = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            value,
            json!({"status": "done", "type": "failed", "failure": "rate_limited", "code": "too_many_requests"})
        );
        assert_eq!(serde_json::from_value::<SearchEnd>(value).unwrap(), failed);
        let unfinished = SearchEnd::Unfinished {
            reason: UnfinishedReason::Compacted,
            possibly_billed: false,
        };
        let value = serde_json::to_value(unfinished.clone()).unwrap();
        assert_eq!(
            value,
            json!({"status": "unfinished", "reason": "compacted", "possibly_billed": false})
        );
        assert_eq!(
            serde_json::from_value::<SearchEnd>(value).unwrap(),
            unfinished
        );
    }

    #[test]
    fn an_unknown_failure_kind_reads_as_other() {
        let kind: SearchFailure = serde_json::from_value(json!("quota_gone")).unwrap();
        assert_eq!(kind, SearchFailure::Other("quota_gone".into()));
        assert_eq!(serde_json::to_value(&kind).unwrap(), json!("quota_gone"));
        assert_eq!(kind.as_str(), "quota_gone");
        let known: SearchFailure = serde_json::from_value(json!("invalid_query")).unwrap();
        assert_eq!(known, SearchFailure::InvalidQuery);
    }

    #[test]
    fn unfinished_reasons_are_closed() {
        for (reason, name) in [
            (UnfinishedReason::Stopped, "stopped"),
            (UnfinishedReason::Paused, "paused"),
            (UnfinishedReason::Failed, "failed"),
            (UnfinishedReason::Refused, "refused"),
            (UnfinishedReason::Compacted, "compacted"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), json!(name));
        }
        assert!(serde_json::from_value::<UnfinishedReason>(json!("vanished")).is_err());
    }
}
