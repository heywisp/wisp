use std::borrow::Cow;
use std::fmt;

pub const KEY_SHAPED_MIN: usize = 20;
pub const NOT_SHOWN: &str = "(looks like a key, not shown)";
pub const VAR_NAME_CHARS: &str = "letters, digits and _";

pub fn is_variable_name(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn looks_like_key(name: &str) -> bool {
    name.chars().count() >= KEY_SHAPED_MIN
        && (!crate::secret::known_prefix(name).is_empty()
            || (name.chars().any(|c| c.is_ascii_lowercase())
                && name.chars().any(|c| c.is_ascii_digit())
                && !name.contains('_')))
}

pub fn plain_variable(name: &str) -> bool {
    is_variable_name(name) && !looks_like_key(name) && !looks_like_key(&name.replace('_', ""))
}

pub fn shown_variable(name: &str) -> &str {
    if plain_variable(name) {
        name
    } else {
        NOT_SHOWN
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VariableName(Cow<'static, str>);

impl VariableName {
    pub const fn from_static(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }

    pub fn parse(text: &str) -> Option<Self> {
        is_variable_name(text).then(|| Self(Cow::Owned(text.to_owned())))
    }

    pub fn raw(&self) -> &str {
        &self.0
    }

    pub fn shown(&self) -> &str {
        shown_variable(&self.0)
    }

    pub fn looks_like_key(&self) -> bool {
        looks_like_key(&self.0)
    }
}

impl fmt::Display for VariableName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.shown())
    }
}

impl serde::Serialize for VariableName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for VariableName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "not a variable name ({VAR_NAME_CHARS}); what was given isn't shown"
            ))
        })
    }
}

impl fmt::Debug for VariableName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VariableName").field(&self.shown()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variable_name_is_shown_and_a_key_shaped_one_is_not() {
        assert_eq!(shown_variable("WORK_API_KEY"), "WORK_API_KEY");
        assert_eq!(shown_variable("_x1"), "_x1");
        assert_eq!(shown_variable("abcdefghij0123456789"), NOT_SHOWN);
        assert_eq!(shown_variable("sk-ant-CANARY0001"), NOT_SHOWN);
        assert_eq!(shown_variable(""), NOT_SHOWN);
        assert_eq!(shown_variable("1ABC"), NOT_SHOWN);
    }

    #[test]
    fn token_shaped_variable_names_are_never_plain() {
        for prefix in ["ghp_", "github_pat_", "sk_live_", "lin_api_", "ntn_"] {
            let name = format!("{prefix}CANARYa1{}", "x".repeat(40));
            assert!(is_variable_name(&name));
            assert!(!plain_variable(&name));
            assert_eq!(shown_variable(&name), NOT_SHOWN);
        }
        assert!(plain_variable("SENTRY_TOKEN"));
    }

    #[test]
    fn looks_like_key_has_one_boundary() {
        assert!(!looks_like_key("abcdefghij012345678"));
        assert!(looks_like_key("abcdefghij0123456789"));
        assert!(!looks_like_key("abcdefghij012345678_"));
        assert!(!looks_like_key("abcdefghijklmnopqrst"));
        assert!(!looks_like_key("ABCDEFGHIJ0123456789"));
        assert!(looks_like_key("AIzaCANARY_KEY_WITH_UNDERSCORE"));
        assert_eq!(shown_variable("AIzaCANARY_KEY_WITH_UNDERSCORE"), NOT_SHOWN);
    }

    #[test]
    fn a_variable_name_never_formats_a_key_shaped_value() {
        let canary = "canary0000000000000key1";
        let name = VariableName::parse(canary).unwrap_or_else(|| VariableName::from_static("X"));
        assert_eq!(name.raw(), canary);
        for text in [format!("{name}"), format!("{name:?}"), format!("{name:#?}")] {
            assert!(!text.contains(canary), "{text}");
            assert!(text.contains(NOT_SHOWN), "{text}");
        }
        let plain = VariableName::from_static("ANTHROPIC_API_KEY");
        assert_eq!(plain.to_string(), "ANTHROPIC_API_KEY");
        assert_eq!(format!("{plain:?}"), "VariableName(\"ANTHROPIC_API_KEY\")");
        assert_eq!(VariableName::parse("1BAD"), None);
        assert_eq!(VariableName::parse("A-B"), None);
    }

    #[test]
    fn aiza_keys_with_underscores_are_caught_as_key_shaped_variables() {
        let gemini = "AIzaCANARY_gemini_key_with_underscre_1";
        assert!(is_variable_name(gemini));
        assert!(looks_like_key(gemini));
        assert_eq!(shown_variable(gemini), NOT_SHOWN);
        let parsed = VariableName::parse(gemini).unwrap_or_else(|| VariableName::from_static("X"));
        assert!(parsed.looks_like_key());
        assert_eq!(parsed.shown(), NOT_SHOWN);
        assert!(!format!("{parsed}").contains("CANARY"));
        assert!(!format!("{parsed:?}").contains("CANARY"));
        assert_eq!(shown_variable("GEMINI_API_KEY"), "GEMINI_API_KEY");
    }
}
