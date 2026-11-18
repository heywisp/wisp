use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTransformation {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub path: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub reason: Option<Option<String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn present<'de, D: Deserializer<'de>>(from: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(from).map(Some)
}
