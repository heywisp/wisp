use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::settings::EndpointSummary;
use crate::{AgentEvent, CacheTtl, Effort, InputId};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub events: Vec<AgentEvent>,
    pub returned: Vec<Returned>,
    pub input_ids: Vec<String>,
    pub started: Option<Pair>,
    pub retried_on: BTreeMap<u64, Pair>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    pub endpoint: String,
    pub model: String,
    pub effort: Effort,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Returned {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<InputId>,
    pub text: String,
    pub images: Vec<ReturnedImage>,
    pub missing: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturnedImage {
    pub data: Arc<str>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Now {
    pub endpoint: String,
    pub model: String,
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
    pub next: Option<NextTurn>,
    pub endpoints: Vec<EndpointSummary>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextTurn {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub cache_ttl: Option<CacheTtl>,
}
