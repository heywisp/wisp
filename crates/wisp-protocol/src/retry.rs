use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryFocus {
    Retry,
    Switch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryOffer {
    pub retry: bool,
    pub switch: bool,
    pub focus: RetryFocus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries_at: Option<i64>,
}

impl RetryOffer {
    pub fn allows(&self, same_endpoint: bool) -> bool {
        if same_endpoint {
            self.retry
        } else {
            self.switch
        }
    }
}
