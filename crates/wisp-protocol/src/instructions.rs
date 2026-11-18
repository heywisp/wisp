use serde::{Deserialize, Serialize};

use crate::settings::{named, word_of};

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Project,
    Default,
    Preset,
    #[serde(untagged)]
    Other(String),
}

impl Scope {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Project => "project",
            Self::Default => "default",
            Self::Preset => "preset",
            Self::Other(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    NotAFile,
    NotUtf8,
    Unreadable,
    ImportMissing,
    ImportTooDeep,
    Large,
    #[serde(untagged)]
    Other(String),
}

impl DiagnosticKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotAFile => "not_a_file",
            Self::NotUtf8 => "not_utf8",
            Self::Unreadable => "unreadable",
            Self::ImportMissing => "import_missing",
            Self::ImportTooDeep => "import_too_deep",
            Self::Large => "large",
            Self::Other(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub kind: DiagnosticKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub scope: Scope,
    pub name: String,
    pub display_path: String,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_by: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceChange {
    pub display_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateInfo {
    pub scope: Scope,
    pub display_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionsSource {
    #[default]
    Auto,
    Default,
    None,
    #[serde(untagged)]
    Other(String),
}

impl InstructionsSource {
    pub const WORDS: [&str; Self::KNOWN.len()] = ["auto", "default", "none"];
    const KNOWN: [Self; 3] = [Self::Auto, Self::Default, Self::None];

    pub fn parse(text: &str) -> Option<Self> {
        named(&Self::WORDS, &Self::KNOWN, text)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Other(s) => s,
            known => word_of(&Self::WORDS, &Self::KNOWN, known).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionsSummary {
    pub chosen: Option<SourceInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<SourceInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<CandidateInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn open_enums_keep_unknown_values_as_text() {
        let scope: Scope = serde_json::from_value(json!("agent")).unwrap();
        assert_eq!(scope, Scope::Other("agent".into()));
        assert_eq!(serde_json::to_value(&scope).unwrap(), json!("agent"));
        assert_eq!(
            serde_json::from_value::<Scope>(json!("project")).unwrap(),
            Scope::Project
        );
        let kind: DiagnosticKind = serde_json::from_value(json!("too_hidden")).unwrap();
        assert_eq!(kind, DiagnosticKind::Other("too_hidden".into()));
        assert_eq!(kind.as_str(), "too_hidden");
        assert_eq!(
            serde_json::from_value::<DiagnosticKind>(json!("not_a_file")).unwrap(),
            DiagnosticKind::NotAFile
        );
        let source: InstructionsSource = serde_json::from_value(json!("agent")).unwrap();
        assert_eq!(source, InstructionsSource::Other("agent".into()));
        assert_eq!(
            serde_json::to_value(InstructionsSource::Default).unwrap(),
            json!("default")
        );
    }

    #[test]
    fn the_source_setting_parses_its_three_values_only() {
        for (value, source) in [
            ("auto", InstructionsSource::Auto),
            ("default", InstructionsSource::Default),
            ("none", InstructionsSource::None),
        ] {
            assert_eq!(InstructionsSource::parse(value), Some(source.clone()));
            assert_eq!(source.as_str(), value);
        }
        assert_eq!(InstructionsSource::parse("Auto"), None);
        assert_eq!(InstructionsSource::parse(""), None);
    }
}
