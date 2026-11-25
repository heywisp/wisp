use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use wisp_protocol::VariableName;

use crate::endpoints::clean;
use crate::provider::credential::EnvFile;

pub use wisp_protocol::GIVEN_ENV_FILE;

pub fn missing_env_file() -> String {
    format!("{GIVEN_ENV_FILE} doesn't exist")
}

fn missing(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

pub struct EnvText(String);

impl EnvText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for EnvText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EnvText(<{} bytes>)", self.0.len())
    }
}

#[derive(Debug)]
pub enum Unscanned {
    GivenMissing,
    NotUtf8(EnvText),
    Unreadable(std::io::ErrorKind),
}

#[derive(Debug)]
pub enum EnvScan {
    Missing,
    Text(EnvText),
    Failed(Unscanned),
}

impl EnvScan {
    pub fn read(path: &Path, explicit: bool) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Self::Text(EnvText(text)),
                Err(e) => Self::Failed(Unscanned::NotUtf8(readable_lines(e.as_bytes()))),
            },
            Err(e) if missing(e.kind()) && explicit => Self::Failed(Unscanned::GivenMissing),
            Err(e) if missing(e.kind()) => Self::Missing,
            Err(e) => Self::Failed(Unscanned::Unreadable(e.kind())),
        }
    }

    pub fn readable_text(&self) -> Option<&EnvText> {
        match self {
            Self::Text(text) | Self::Failed(Unscanned::NotUtf8(text)) => Some(text),
            _ => None,
        }
    }

    pub fn parse(&self, path: &Path, explicit: bool) -> Result<Option<EnvFile>, String> {
        let shown = || {
            if explicit {
                GIVEN_ENV_FILE.to_owned()
            } else {
                crate::workspace::home_shown(path)
            }
        };
        let text = match self {
            Self::Text(text) => text,
            Self::Missing => return Ok(None),
            Self::Failed(Unscanned::GivenMissing) => return Err(missing_env_file()),
            Self::Failed(Unscanned::NotUtf8(_)) => {
                return Err(format!("{} is not valid UTF-8", shown()));
            }
            Self::Failed(Unscanned::Unreadable(kind)) => {
                return Err(format!("could not read {}: {kind}", shown()));
            }
        };
        let vars = parse_env(text.as_str()).map_err(|line| {
            format!(
                "{} line {line} is not KEY=VALUE; nothing was loaded",
                shown()
            )
        })?;
        Ok(Some(EnvFile {
            path: path.to_owned(),
            vars,
        }))
    }
}

fn readable_lines(bytes: &[u8]) -> EnvText {
    let lines: Vec<&str> = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .collect();
    EnvText(lines.join("\n"))
}

pub fn read_env_file(path: &Path, explicit: bool) -> Result<Option<EnvFile>, String> {
    EnvScan::read(path, explicit).parse(path, explicit)
}

pub fn env_file_names(text: &str, asked: &BTreeSet<VariableName>) -> BTreeSet<VariableName> {
    let values = line_values(text);
    asked
        .iter()
        .filter(|name| clean(values.get(name.raw()).cloned()).is_some())
        .cloned()
        .collect()
}

pub fn env_text_problem(text: &str) -> Option<usize> {
    parse_env(text).err()
}

pub(crate) fn line_values(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        if let Ok(vars) = parse_env(line) {
            values.extend(vars);
        }
    }
    values
}

fn parse_env(text: &str) -> Result<BTreeMap<String, String>, usize> {
    let mut vars = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let (key, value) = line.split_once('=').ok_or(index + 1)?;
        let key = key.trim();
        if !wisp_protocol::is_variable_name(key) {
            return Err(index + 1);
        }
        let value = match value.trim_start().chars().next() {
            Some(q @ ('"' | '\'')) => {
                let inner = &value.trim_start()[1..];
                let end = inner.find(q).ok_or(index + 1)?;
                let after = inner[end + 1..].trim_start();
                if !(after.is_empty() || after.starts_with('#')) {
                    return Err(index + 1);
                }
                &inner[..end]
            }
            _ => strip_comment(value).trim(),
        };
        vars.insert(key.to_owned(), value.to_owned());
    }
    Ok(vars)
}

fn strip_comment(value: &str) -> &str {
    let mut prev_space = false;
    for (i, c) in value.char_indices() {
        if c == '#' && prev_space {
            return &value[..i];
        }
        prev_space = c.is_whitespace();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::anthropic::{Credential, Settings};

    #[test]
    fn env_file_is_parsed_without_exporting() {
        let dir =
            std::env::temp_dir().join(format!("wisp-core-env-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "WISP_TEST_ONLY_VAR=secret-value\n").unwrap();
        let file = read_env_file(&path, true).unwrap().unwrap();
        assert_eq!(file.vars["WISP_TEST_ONLY_VAR"], "secret-value");
        assert!(std::env::var_os("WISP_TEST_ONLY_VAR").is_none());

        std::fs::write(&path, "GOOD=1\nBAD LINE secret-value\n").unwrap();
        let err = read_env_file(&path, true).unwrap_err();
        assert!(!err.contains("secret-value"), "{err}");
        assert!(err.contains("line 2"), "{err}");

        let missing = dir.join("missing.env");
        assert!(read_env_file(&missing, false).unwrap().is_none());
        assert!(read_env_file(&missing, true).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_file_values_are_never_expanded() {
        assert!(std::env::var_os("HOME").is_some());
        let vars =
            parse_env("ANTHROPIC_AUTH_TOKEN=${HOME}\nA=$HOME\nB=\"${HOME}\"\nC='$HOME'\n").unwrap();
        assert_eq!(vars["ANTHROPIC_AUTH_TOKEN"], "${HOME}");
        assert_eq!(vars["A"], "$HOME");
        assert_eq!(vars["B"], "${HOME}");
        assert_eq!(vars["C"], "$HOME");
        let file = EnvFile {
            path: "/x/.env".into(),
            vars,
        };
        let s = Settings::resolve(|_| None, Some(&file)).unwrap();
        assert_eq!(s.credential, Credential::AuthToken("${HOME}".into()));
    }

    #[test]
    fn env_file_syntax() {
        let vars = parse_env(
            "# comment\n\n  export KEY_1 = plain value \nQ=\"a 'b' #c\"\nS='x\\ny'\nE=\nDUP=first\nDUP=second\n",
        )
        .unwrap();
        assert_eq!(vars["KEY_1"], "plain value");
        assert_eq!(vars["Q"], "a 'b' #c");
        assert_eq!(vars["S"], "x\\ny");
        assert_eq!(vars["E"], "");
        assert_eq!(vars["DUP"], "second");
        assert_eq!(vars.len(), 5);
        for (bad, line) in [
            ("OK=1\nno equals sign\n", 2),
            ("=value\n", 1),
            ("1KEY=v\n", 1),
            ("A B=v\n", 1),
            ("\n\nK=\"unterminated\n", 3),
            ("K='mismatched\"\n", 1),
            ("K=\"\n", 1),
        ] {
            assert_eq!(parse_env(bad), Err(line), "{bad:?}");
        }
    }

    #[test]
    fn env_file_names_returns_only_asked_names_that_are_set() {
        let name = |n: &str| VariableName::parse(n).unwrap();
        let asked: BTreeSet<VariableName> = [
            "EXPORTED", "SPACED", "EMPTY", "BLANK", "LAST", "QUOTED", "ABSENT",
        ]
        .into_iter()
        .map(name)
        .collect();
        let text = "export EXPORTED=sk-ant-CANARYONE\nSPACED = CANARYTWO\nEMPTY=\nBLANK=\"  \"\nLAST=CANARYTHREE\nLAST=\nsk-ant-CANARYPASTEDALONE0000\nQUOTED='CANARYFOUR' # note\nNOT_ASKED=CANARYFIVE\n";
        let found = env_file_names(text, &asked);
        assert_eq!(
            found,
            ["EXPORTED", "QUOTED", "SPACED"]
                .into_iter()
                .map(name)
                .collect::<BTreeSet<_>>()
        );
        let shown = format!("{found:?}");
        assert!(!shown.contains("CANARY"), "{shown}");
        assert_eq!(env_text_problem(text), Some(7));
        assert_eq!(env_text_problem("A=1\n"), None);
    }

    #[test]
    fn env_inline_comments() {
        let vars = parse_env(
            "A=value # comment\nB=value#not-a-comment\nC=\"quoted # kept\" # dropped\nD='x #y'\nE= # only a comment\nF=a\t# tab\n",
        )
        .unwrap();
        assert_eq!(vars["A"], "value");
        assert_eq!(vars["B"], "value#not-a-comment");
        assert_eq!(vars["C"], "quoted # kept");
        assert_eq!(vars["D"], "x #y");
        assert_eq!(vars["E"], "");
        assert_eq!(vars["F"], "a");
        assert_eq!(parse_env("K=\"v\" trailing\n"), Err(1));
    }
}
