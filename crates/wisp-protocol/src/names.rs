use std::fmt;
use std::sync::LazyLock;

use crate::settings::{Kind, SettingValue};
use crate::variable::NOT_SHOWN;

pub const MAX_ENDPOINT_NAME: usize = 32;

pub const MAX_MODEL_ID: usize = 128;

pub const KEY_RUN: usize = 10;

pub const KEY_LETTER_RUN: usize = 20;

pub const MODEL_PLACEHOLDER: &str = "MODEL";

pub const THIS_MODEL: &str = "this model";

pub const NOT_A_MODEL_ID: &str = "(not a model ID, not shown)";

pub const MODEL_IDS_RULE: &str = "a list of model IDs";

pub fn endpoint_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-')
}

pub fn valid_endpoint_name(name: &str) -> bool {
    (1..=MAX_ENDPOINT_NAME).contains(&name.chars().count())
        && name.chars().all(endpoint_char)
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

pub const ACCOUNT_NAME_LABEL: &str = "Name for this account";

pub static ACCOUNT_NAME_RULE: LazyLock<String> =
    LazyLock::new(|| format!("Names are {}.", endpoint_name_rule()));

pub const KEYS_FILE_ALIAS: &str = "env";

pub const KEYS_FILE_ACCOUNT: &str = "the keys-file account";

pub const ACCOUNT_FLAG: &str = "--account";

#[macro_export]
macro_rules! config_file {
    () => {
        "config.toml"
    };
}

#[macro_export]
macro_rules! credentials_file {
    () => {
        "credentials.json"
    };
}

#[macro_export]
macro_rules! keys_file {
    () => {
        ".env"
    };
}

#[macro_export]
macro_rules! lock_file {
    () => {
        ".wisp.lock"
    };
}

#[macro_export]
macro_rules! config_dir {
    () => {
        "~/.config/wisp/"
    };
}

#[macro_export]
macro_rules! given_config {
    () => {
        "the file given with --config"
    };
}

#[macro_export]
macro_rules! given_env_file {
    () => {
        "the file given with --env-file"
    };
}

#[macro_export]
macro_rules! credentials_beside_config {
    () => {
        concat!($crate::credentials_file!(), " beside the config")
    };
}

pub const CONFIG_FILE: &str = config_file!();

pub const CREDENTIALS: &str = credentials_file!();

pub const IN_KEYCHAIN: &str = "the keychain";

pub const KEYS_FILE: &str = keys_file!();

pub const LOCK_FILE: &str = lock_file!();

pub const DEFAULT_CONFIG_FILE: &str = concat!(config_dir!(), config_file!());

pub const DEFAULT_KEYS_FILE: &str = concat!(config_dir!(), keys_file!());

pub const GIVEN_CONFIG: &str = given_config!();

pub const GIVEN_ENV_FILE: &str = given_env_file!();

pub const GIVEN_CREDENTIALS: &str = concat!(credentials_file!(), " beside ", given_config!());

pub fn account_name_reserved() -> String {
    format!("That name is reserved for {KEYS_FILE_ACCOUNT}; choose another.")
}

pub const CLAUDE_API: &str = "claude-api";

pub const OPENAI_API: &str = "openai-api";

pub const GEMINI_API: &str = "gemini-api";

pub const GATEWAY: &str = "gateway";

pub const KEYS_FILE_NAMES: [&str; 2] = [CLAUDE_API, GATEWAY];

pub fn keys_file_name(official: bool) -> &'static str {
    if official { CLAUDE_API } else { GATEWAY }
}

pub fn default_api_account_name(provider: &str) -> &str {
    if provider.eq_ignore_ascii_case(crate::settings::PROVIDER_GEMINI) {
        GEMINI_API
    } else {
        provider
    }
}

pub fn account_name_taken(name: &str) -> String {
    format!("{name} is taken.")
}

pub fn account_name(typed: &str) -> Option<String> {
    let name = typed.trim().to_ascii_lowercase();
    (name != KEYS_FILE_ALIAS && valid_endpoint_name(&name)).then_some(name)
}

pub fn account_name_problem(typed: &str) -> String {
    if typed.trim().eq_ignore_ascii_case(KEYS_FILE_ALIAS) {
        account_name_reserved()
    } else {
        ACCOUNT_NAME_RULE.to_owned()
    }
}

pub fn endpoint_name_rule() -> String {
    format!("1 to {MAX_ENDPOINT_NAME} of a-z, 0-9, _ and -, starting with a letter or digit")
}

pub fn account_name_error() -> String {
    format!("an account name must be {}", endpoint_name_rule())
}

pub fn bad_account_name() -> String {
    format!("that is not an account name ({})", endpoint_name_rule())
}

pub const MODEL_ID_CHARS: &str = "letters, digits and . _ : @ / -";

pub fn model_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '@' | '/' | '-')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelIdProblem {
    Empty,
    TooLong,
    BadCharacter,
}

impl fmt::Display for ModelIdProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the model ID is empty"),
            Self::TooLong => write!(f, "the model ID is longer than {MAX_MODEL_ID} characters"),
            Self::BadCharacter => {
                write!(f, "the model ID may only hold {MODEL_ID_CHARS}")
            }
        }
    }
}

pub fn check_model_id(id: &str) -> Result<(), ModelIdProblem> {
    if id.is_empty() {
        return Err(ModelIdProblem::Empty);
    }
    if id.chars().count() > MAX_MODEL_ID {
        return Err(ModelIdProblem::TooLong);
    }
    if !id.chars().all(model_id_char) {
        return Err(ModelIdProblem::BadCharacter);
    }
    Ok(())
}

pub fn valid_model_id(id: &str) -> bool {
    check_model_id(id).is_ok()
}

fn key_run(run: &str) -> bool {
    let has = |test: fn(&char) -> bool| run.chars().any(|c| test(&c));
    let mixed_digits =
        run.len() >= KEY_RUN && has(char::is_ascii_alphabetic) && has(char::is_ascii_digit);
    let mixed_case = run.len() >= KEY_LETTER_RUN
        && has(char::is_ascii_uppercase)
        && has(char::is_ascii_lowercase);
    mixed_digits || mixed_case
}

pub fn model_looks_like_key(id: &str) -> bool {
    id.split(|c: char| !c.is_ascii_alphanumeric()).any(key_run)
}

pub fn setting_looks_like_key(kind: Kind, value: &SettingValue) -> bool {
    match (kind, value) {
        (Kind::ModelId, SettingValue::Text(id)) => model_looks_like_key(id),
        (Kind::ModelIds, SettingValue::List(ids)) => ids.iter().any(|id| model_looks_like_key(id)),
        _ => false,
    }
}

pub fn model_hidden(id: &str) -> bool {
    shown_model(id) != id
}

pub fn model_words(id: &str) -> &str {
    if model_hidden(id) { THIS_MODEL } else { id }
}

pub fn model_or_placeholder(id: &str) -> &str {
    if model_hidden(id) {
        MODEL_PLACEHOLDER
    } else {
        id
    }
}

fn replace_any_case(text: &str, model: &str, shown: &str) -> String {
    let lowered = text.to_ascii_lowercase();
    let needle = model.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for (at, found) in lowered.match_indices(needle.as_str()) {
        out.push_str(text.get(from..at).unwrap_or_default());
        out.push_str(shown);
        from = at + found.len();
    }
    out.push_str(text.get(from..).unwrap_or_default());
    out
}

pub fn hide_models<'a>(text: &str, models: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = text.to_owned();
    for model in models {
        if !model.is_empty() && model_hidden(model) {
            out = replace_any_case(&out, model, shown_model(model));
        }
    }
    out
}

pub fn hide_quoted_models(text: &str) -> String {
    text.split('"')
        .enumerate()
        .map(|(at, part)| {
            if at % 2 == 1 {
                model_or_placeholder(part)
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("\"")
}

pub fn shown_text(text: &str) -> &str {
    if model_looks_like_key(text) {
        NOT_SHOWN
    } else {
        text
    }
}

pub fn shown_model(id: &str) -> &str {
    let shown = shown_text(id);
    if shown == id && !id.is_empty() && check_model_id(id).is_err() {
        NOT_A_MODEL_ID
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_names_follow_one_rule() {
        for good in ["a", "manifest", "gw-2", "0x", "a_b", &"a".repeat(32)] {
            assert!(valid_endpoint_name(good), "{good}");
        }
        for bad in ["", "-a", "_a", "A", "a.b", "a b", &"a".repeat(33)] {
            assert!(!valid_endpoint_name(bad), "{bad}");
        }
        assert_eq!(
            endpoint_name_rule(),
            "1 to 32 of a-z, 0-9, _ and -, starting with a letter or digit"
        );
        assert_eq!(
            account_name_error(),
            format!("an account name must be {}", endpoint_name_rule())
        );
        assert_eq!(
            bad_account_name(),
            format!("that is not an account name ({})", endpoint_name_rule())
        );
        let docs = include_str!("../../../docs/endpoints.md");
        assert!(docs.contains(&endpoint_name_rule()));
        assert!(docs.contains(&format!("`{}`", ACCOUNT_NAME_RULE.as_str())));
    }

    #[test]
    fn an_account_name_is_trimmed_and_lowercased_and_never_env() {
        assert_eq!(default_api_account_name("gemini"), GEMINI_API);
        assert_eq!(default_api_account_name("GEMINI"), GEMINI_API);
        assert_eq!(default_api_account_name("openai"), "openai");
        assert_eq!(account_name("  Claude1 "), Some("claude1".to_owned()));
        assert_eq!(account_name("claude"), Some("claude".to_owned()));
        for bad in ["", "env", "ENV", "a b", "-a", "\u{e9}", &"a".repeat(33)] {
            assert_eq!(account_name(bad), None, "{bad}");
        }
        assert_eq!(
            ACCOUNT_NAME_RULE.as_str(),
            format!("Names are {}.", endpoint_name_rule())
        );
        for env in ["env", " ENV "] {
            assert_eq!(
                account_name_problem(env),
                "That name is reserved for the keys-file account; choose another."
            );
        }
        for bad in ["", "a b", "-a", &"a".repeat(33)] {
            assert_eq!(
                account_name_problem(bad),
                ACCOUNT_NAME_RULE.as_str(),
                "{bad}"
            );
        }
    }

    #[test]
    fn model_ids_follow_one_rule() {
        assert_eq!(check_model_id("claude-sonnet-5.5"), Ok(()));
        assert_eq!(check_model_id("a/b:c@d_e"), Ok(()));
        assert_eq!(check_model_id(""), Err(ModelIdProblem::Empty));
        assert_eq!(
            check_model_id(&"m".repeat(MAX_MODEL_ID + 1)),
            Err(ModelIdProblem::TooLong)
        );
        assert_eq!(check_model_id("a b"), Err(ModelIdProblem::BadCharacter));
        assert_eq!(
            ModelIdProblem::TooLong.to_string(),
            "the model ID is longer than 128 characters"
        );
    }

    #[test]
    fn a_model_id_shows_unless_it_looks_like_a_key() {
        for real in [
            "claude-opus-5-5",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-5@20251101",
            "us.anthropic.claude-3-7-sonnet-20250219-v1:0",
            "gpt-4o-mini-2024-07-18",
            "ft:gpt-4o-mini-2024-07-18:my-org::A1b2C3d4",
            "meta-llama/Meta-Llama-3.1-405B-Instruct-Turbo",
            "Qwen/Qwen2.5-Coder-32B-Instruct",
            "accounts/fireworks/models/llama-v3p1-405b-instruct",
            "mixtral-8x7b-32768",
            "gpt-4o-transcribe",
            "text-moderation-latest",
            "sentence-transformers/all-MiniLM-L6-v2",
            "NousResearch/Hermes-3-Llama-3.1-8B",
            "deepseek-ai/DeepSeek-Coder-V2-Instruct",
            "arn:aws:bedrock:us-east-1:123456789012:provisioned-model/abc123xyz",
            "projects/1234567890/endpoints/9876543210123",
            "abcdefghijklmnopqrstuvwxyz",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "aBcDeFgHiJkLmNoPqRs",
            "m",
            "",
        ] {
            assert_eq!(shown_model(real), real, "{real}");
            assert_eq!(model_words(real), real, "{real}");
            assert_eq!(model_or_placeholder(real), real, "{real}");
        }
        for key in [
            "sk-ant-api03-CANARYMODEL0000",
            "sk-ant-api03-canCANARYer01",
            "sk-ant-CANARY0001",
            "sk-ant-api03-Ab_cD3fGh1jKlMn0pQrCANARYx2yZ",
            "abcdefghij0123456789",
            "hf_CANARYGhIjKlMnOpQrStUvWx",
            "api_org_CANARYGhIjKlMnOpQrStUvWx",
            "aBcDeFgHiJkLmNoPqRsT",
        ] {
            assert_eq!(model_words(key), THIS_MODEL, "{key}");
            assert_eq!(model_or_placeholder(key), MODEL_PLACEHOLDER, "{key}");
            assert!(model_looks_like_key(key), "{key}");
            assert_eq!(shown_model(key), NOT_SHOWN, "{key}");
        }
        for odd in ["a b", "claude\u{1b}[2J", "caf\u{e9}"] {
            assert_eq!(shown_model(odd), NOT_A_MODEL_ID, "{odd:?}");
            assert_eq!(model_words(odd), THIS_MODEL, "{odd:?}");
        }
    }

    #[test]
    fn hidden_models_are_taken_out_of_text() {
        let key = "sk-ant-api03-CANARYMODEL0000";
        assert_eq!(
            hide_models(
                &format!("not_found_error: model: {key}"),
                [key, "claude-opus-5-5"]
            ),
            format!("not_found_error: model: {NOT_SHOWN}")
        );
        assert_eq!(
            hide_models(
                &format!(
                    "model {} or {} not found: {key}",
                    key.to_ascii_lowercase(),
                    key.to_ascii_uppercase()
                ),
                [key]
            ),
            format!("model {NOT_SHOWN} or {NOT_SHOWN} not found: {NOT_SHOWN}")
        );
        assert_eq!(
            hide_models("caf\u{e9} \u{1b}[2J SK-ANT-API03-canarymodel0000!", [key]),
            format!("caf\u{e9} \u{1b}[2J {NOT_SHOWN}!")
        );
        assert_eq!(
            hide_models("model: claude-opus-5-5", ["claude-opus-5-5", ""]),
            "model: claude-opus-5-5"
        );
        assert_eq!(
            hide_models("model: CLAUDE-OPUS-5-5", ["claude-opus-5-5"]),
            "model: CLAUDE-OPUS-5-5"
        );
        assert_eq!(
            hide_quoted_models(&format!("endpoints.gw.models.\"{key}\".compact_at")),
            "endpoints.gw.models.\"MODEL\".compact_at"
        );
        assert_eq!(
            hide_quoted_models("models.\"claude-opus-5-5\".context_window"),
            "models.\"claude-opus-5-5\".context_window"
        );
        assert_eq!(
            hide_quoted_models("config.toml line 3"),
            "config.toml line 3"
        );
    }
}
