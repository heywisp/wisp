use std::path::PathBuf;

use clap::ValueEnum;
use wisp_core::agent::InstructionsFacts as Loaded;
use wisp_core::endpoints::InstructionsConfig;
use wisp_core::instructions::{self, DEFAULT_WARN_TOKENS, Paths};
use wisp_core::kit::InstructionsDirs;
use wisp_protocol::InstructionsSource;
use wisp_protocol::settings::TOKEN_COUNT;
use wisp_render::chrome::InstructionsFacts;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum InstructionsArg {
    #[value(help = "The project's AGENTS.md, or your default when the project has none")]
    Auto,
    #[value(help = "Only your default, ~/.config/wisp/AGENTS.md")]
    Default,
    #[value(help = "No instruction files")]
    None,
}

impl InstructionsArg {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Default => "default",
            Self::None => "none",
        }
    }

    pub fn named(text: &str) -> Option<Self> {
        [Self::Auto, Self::Default, Self::None]
            .into_iter()
            .find(|v| v.name() == text)
    }

    pub fn source(self) -> InstructionsSource {
        match self {
            Self::Auto => InstructionsSource::Auto,
            Self::Default => InstructionsSource::Default,
            Self::None => InstructionsSource::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ExtraArg {
    #[value(help = "No built-in instructions")]
    None,
    #[value(help = "For a task done on its own, with nobody to answer questions")]
    Autonomous,
}

impl ExtraArg {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Autonomous => "autonomous",
        }
    }
}

pub fn parse_warn_tokens(text: &str) -> Result<u64, String> {
    instructions::parse_tokens(text).ok_or_else(|| format!("not {}", TOKEN_COUNT.replace('"', "")))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub source: InstructionsSource,
    pub warn_tokens: u64,
}

pub fn settings(
    flag: Option<InstructionsArg>,
    warn_flag: Option<u64>,
    file: Option<&InstructionsConfig>,
) -> Settings {
    let source = flag
        .map(InstructionsArg::source)
        .or_else(|| file.and_then(|f| f.source.clone()))
        .unwrap_or_default();
    let warn_tokens = warn_flag
        .or_else(|| file.and_then(|f| f.warn_tokens))
        .unwrap_or(DEFAULT_WARN_TOKENS);
    Settings {
        source,
        warn_tokens,
    }
}

pub fn paths(start: PathBuf, xdg_config_home: Option<PathBuf>, home: Option<PathBuf>) -> Paths {
    let home = home.filter(|h| h.is_absolute());
    Paths {
        start,
        config_dir: Paths::config_dir_from(xdg_config_home, home.as_deref()),
        home,
    }
}

pub fn dirs(xdg_config_home: Option<PathBuf>, home: Option<PathBuf>) -> InstructionsDirs {
    let home = home.filter(|h| h.is_absolute());
    InstructionsDirs {
        config_dir: Paths::config_dir_from(xdg_config_home, home.as_deref()),
        home,
    }
}

pub fn facts(loaded: &Loaded) -> InstructionsFacts {
    InstructionsFacts {
        chosen: loaded.chosen.clone(),
        summary: loaded.summary.clone(),
        note: None,
    }
}

#[cfg(test)]
mod tests;
