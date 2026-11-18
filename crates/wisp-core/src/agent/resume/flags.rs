use wisp_protocol::settings::{CACHE_KEPT, CACHE_KEPT_EFFORT, SettingValue, names};
use wisp_protocol::{CacheEffect, Effort, InstructionsSource};

use super::super::Agent;
use super::RestoredBlock;
use crate::resume::{LaterOptions, LoggedSession};
use crate::settings::text;

const LABELS: [(&str, &str); 5] = [
    (names::CACHE_TTL, "cache TTL"),
    (names::DEBUG_CACHE, "cache debugging"),
    (names::REFUSAL_FALLBACK, "refusal fallback"),
    (names::TIME, "time stamps"),
    (names::CONTEXT_CONTEXT_NOTE, "context note"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Kept,
    KeptIfPassedOn,
    MayRebuild,
    Rebuilt,
}

impl Class {
    fn of(effect: &CacheEffect) -> Self {
        match effect {
            CacheEffect::Kept => Self::Kept,
            CacheEffect::KeptIfPassedOn => Self::KeptIfPassedOn,
            CacheEffect::Rebuilt { .. } => Self::Rebuilt,
        }
    }

    fn words(self) -> &'static str {
        match self {
            Self::Kept => CACHE_KEPT,
            Self::KeptIfPassedOn => CACHE_KEPT_EFFORT,
            Self::MayRebuild => "may rebuild the cache",
            Self::Rebuilt => "cache rebuilt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Flagged {
    pub(super) setting: String,
    pub(super) change: String,
    pub(super) class: Class,
}

pub(super) struct FlagNotes {
    pub(super) flagged: Vec<Flagged>,
    pub(super) reasoning: bool,
    pub(super) instructions_rebuilt: bool,
}

impl FlagNotes {
    pub(super) fn causes(&self) -> Vec<String> {
        self.flagged
            .iter()
            .filter(|f| f.class == Class::Rebuilt)
            .map(|f| f.change.clone())
            .collect()
    }

    pub(super) fn may_rebuild(&self) -> bool {
        self.flagged
            .iter()
            .any(|f| matches!(f.class, Class::MayRebuild | Class::Rebuilt))
    }

    pub(super) fn next_turn(&self, classed: bool) -> Option<String> {
        let listed: Vec<String> = self
            .flagged
            .iter()
            .filter(|f| f.class != Class::Rebuilt)
            .map(|f| {
                if classed {
                    format!("{} ({})", f.setting, f.class.words())
                } else {
                    f.setting.clone()
                }
            })
            .collect();
        (!listed.is_empty()).then(|| format!("From your next message: {}.", listed.join(", ")))
    }
}

impl Agent {
    pub(super) fn flagged_instructions(
        &self,
        logged: &LoggedSession,
    ) -> Option<InstructionsSource> {
        text(self.flag_value(names::INSTRUCTIONS_SOURCE))
            .and_then(InstructionsSource::parse)
            .filter(|source| logged.instructions_source.as_ref() != Some(source))
    }

    pub(super) fn flagged_extra(&self, logged: &LoggedSession) -> Option<String> {
        let logged_name = logged.instructions_extra.as_deref().unwrap_or("none");
        text(self.flag_value(names::INSTRUCTIONS_EXTRA))
            .filter(|name| *name != logged_name)
            .map(str::to_owned)
    }

    pub(super) fn flag_notes(
        &self,
        logged: &LoggedSession,
        block: &RestoredBlock,
        block_changed: bool,
    ) -> FlagNotes {
        let mut flagged = Vec::new();
        let pending = logged.pending.as_ref();
        let next = self.next_pair();
        let model_switch = next.model != self.session.model;
        if self.flag_value(names::ENDPOINT).is_some() {
            let plain = pending
                .and_then(|p| p.endpoint.as_deref())
                .or_else(|| logged.endpoint());
            let now = self.endpoint_name();
            if plain != Some(now) {
                let class = if logged.endpoint().is_some_and(|ran| ran != now) {
                    Class::Rebuilt
                } else {
                    Class::Kept
                };
                flagged.push(Flagged {
                    setting: crate::endpoints::goes::named(now),
                    change: format!("the account to {}", crate::endpoints::goes::shown(now)),
                    class,
                });
            }
        }
        if let Some(model) = self.flag_value(names::MODEL).map(SettingValue::text) {
            let plain = pending.map_or(self.session.model.as_str(), |p| p.model.as_str());
            if model != plain {
                let shown = wisp_protocol::shown_model(&next.model);
                flagged.push(Flagged {
                    setting: format!("model {shown}"),
                    change: format!("the model to {shown}"),
                    class: if model_switch {
                        Class::Rebuilt
                    } else {
                        Class::Kept
                    },
                });
            }
        }
        if let Some(effort) =
            text(self.flag_value(names::EFFORT)).and_then(|t| t.parse::<Effort>().ok())
        {
            let plain = pending.map_or(self.session.effort, |p| p.effort);
            if effort != plain {
                let class = if model_switch {
                    Class::Rebuilt
                } else {
                    Class::of(&self.cache_effect(self.endpoint, &next.model, next.effort))
                };
                flagged.push(Flagged {
                    setting: format!("effort {}", next.effort.as_str()),
                    change: format!("effort to {}", next.effort.as_str()),
                    class,
                });
            }
        }
        let block_class = if block_changed {
            Class::Rebuilt
        } else {
            Class::Kept
        };
        if let Some(source) = &block.flagged {
            flagged.push(Flagged {
                setting: format!("instructions {}", source.as_str()),
                change: "the project instructions".to_owned(),
                class: block_class,
            });
        }
        if let Some(name) = &block.extra_flagged {
            flagged.push(Flagged {
                setting: format!("extra instructions {name}"),
                change: format!("the extra instructions ({name})"),
                class: block_class,
            });
        }
        let plain = logged.later.clone().or(self.now_options());
        let values = self
            .with_flags(LaterOptions::default())
            .beyond(&plain)
            .values();
        for (name, label) in LABELS {
            if let Some((_, value)) = values.iter().find(|(option, _)| *option == name) {
                let setting = format!("{label} {}", value.text());
                flagged.push(Flagged {
                    change: setting.clone(),
                    setting,
                    class: if LaterOptions::reason(name).is_some() {
                        Class::MayRebuild
                    } else {
                        Class::Kept
                    },
                });
            }
        }
        let instructions_rebuilt =
            block_changed && (block.flagged.is_some() || block.extra_flagged.is_some());
        FlagNotes {
            flagged,
            reasoning: model_switch || instructions_rebuilt,
            instructions_rebuilt,
        }
    }
}
