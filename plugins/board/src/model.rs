use serde::{Deserialize, Serialize};

pub(crate) const ROOT: &str = "/";

pub(crate) const DEFAULT_COLUMNS: [&str; 3] = ["todo", "doing", "done"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edit {
    Owner,
    Tree,
}

impl Edit {
    pub fn word(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Tree => "tree",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub at: String,
    pub column: String,
    pub author: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub log: Vec<LogLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session: Option<String>,
    pub edit: Edit,
    pub columns: Vec<String>,
    pub cards: Vec<Card>,
    pub updated: String,
    pub created: u64,
    #[serde(default)]
    pub deleted: bool,
}

impl Board {
    pub fn card(&self, id: &str) -> Option<&Card> {
        self.cards.iter().find(|card| card.id == id)
    }

    pub fn bytes(&self) -> Vec<u8> {
        let value = serde_json::to_value(self).unwrap_or_default();
        let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
        text.push('\n');
        text.into_bytes()
    }
}
