use super::{AccountModel, EndpointSummary, ModelEntry};
use crate::Effort;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortVariant {
    pub family: String,
    pub effort: Effort,
}

pub fn siblings<'a>(
    models: impl IntoIterator<Item = (&'a str, &'a EffortVariant)>,
    of: &EffortVariant,
) -> Vec<(Effort, &'a str)> {
    let mut siblings: Vec<_> = models
        .into_iter()
        .filter(|(_, variant)| variant.family == of.family)
        .map(|(id, variant)| (variant.effort, id))
        .collect();
    siblings.sort_unstable();
    siblings
}

pub fn display_effort(
    own: Option<&EffortVariant>,
    levels: Option<&[Effort]>,
    effort: Option<Effort>,
) -> Option<Effort> {
    own.map(|variant| variant.effort)
        .or_else(|| effort.filter(|_| !takes_no_effort(levels)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer<'a> {
    pub id: &'a str,
    pub provider: Option<&'a str>,
    pub listed: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chat<'a> {
    pub provider: Option<&'a str>,
    pub model: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Landing<'a> {
    pub provider: &'a str,
    pub models: Option<&'a [AccountModel]>,
    pub complete: bool,
    pub default_model: Option<&'a str>,
}

impl<'a> Landing<'a> {
    pub fn new(
        provider: &'a str,
        models: &'a [AccountModel],
        complete: bool,
        default_model: Option<&'a str>,
    ) -> Self {
        Self {
            provider,
            models: (!models.is_empty()).then_some(models),
            complete,
            default_model,
        }
    }
}

impl EndpointSummary {
    pub fn landing(&self) -> Landing<'_> {
        Landing::new(
            &self.provider,
            &self.models,
            self.complete,
            self.default_model.as_deref(),
        )
    }
}

pub fn offers(entries: &[ModelEntry]) -> Vec<Offer<'_>> {
    entries
        .iter()
        .map(|m| Offer {
            id: &m.id,
            provider: m.provider.as_deref(),
            listed: m.listed,
        })
        .collect()
}

pub fn serves(a: Option<&str>, b: &str) -> bool {
    a.is_some_and(|a| a.eq_ignore_ascii_case(b))
}

pub fn offered(models: &[Offer<'_>], to: Landing<'_>, chat: Chat<'_>) -> Vec<String> {
    let mut listed: Vec<(u16, &str)> = match to.models {
        Some(own) => own
            .iter()
            .filter_map(|m| m.listed.map(|rank| (rank, m.id.as_str())))
            .collect(),
        None => models
            .iter()
            .filter(|m| serves(m.provider, to.provider))
            .filter_map(|m| m.listed.map(|rank| (rank, m.id)))
            .collect(),
    };
    listed.sort_unstable();
    listed.dedup();
    let mut out: Vec<String> = listed.into_iter().map(|(_, id)| id.to_owned()).collect();
    let owned = to
        .models
        .is_some_and(|own| own.iter().any(|m| m.id == chat.model));
    if (owned || (!to.complete && serves(chat.provider, to.provider)))
        && !out.iter().any(|id| id == chat.model)
    {
        out.push(chat.model.to_owned());
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub model: String,
    pub effort: Effort,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landed {
    pub model: String,
    pub effort: Option<Effort>,
    pub remembered: bool,
}

pub fn lands_on(
    models: &[Offer<'_>],
    to: Landing<'_>,
    same: bool,
    (chat, effort): (Chat<'_>, Option<Effort>),
    last: Option<&Remembered>,
) -> Landed {
    let (model, effort, remembered) = if same {
        (chat.model.to_owned(), effort, false)
    } else if let Some(last) = last {
        (last.model.clone(), Some(last.effort), true)
    } else {
        (runs_on(models, to, chat), effort, false)
    };
    Landed {
        model,
        effort,
        remembered,
    }
}

pub fn runs_on(models: &[Offer<'_>], to: Landing<'_>, chat: Chat<'_>) -> String {
    let listed = offered(models, to, chat);
    if listed.iter().any(|m| m == chat.model) {
        return chat.model.to_owned();
    }
    to.default_model
        .filter(|id| to.models.is_none_or(|own| own.iter().any(|m| m.id == *id)))
        .map(str::to_owned)
        .or_else(|| listed.into_iter().next())
        .unwrap_or_else(|| chat.model.to_owned())
}

pub fn fallback_effort(
    supported: &[crate::Effort],
    current: Option<crate::Effort>,
) -> Option<crate::Effort> {
    let below = supported
        .iter()
        .copied()
        .filter(|l| current.is_some_and(|c| *l < c))
        .max();
    below.or_else(|| supported.iter().copied().min())
}

pub fn takes_no_effort(supported: Option<&[crate::Effort]>) -> bool {
    supported.is_some_and(<[crate::Effort]>::is_empty)
}

pub fn effort_on(
    supported: &[crate::Effort],
    current: Option<crate::Effort>,
) -> Option<crate::Effort> {
    if takes_no_effort(Some(supported)) {
        None
    } else if current.is_some_and(|c| supported.contains(&c)) {
        current
    } else {
        fallback_effort(supported, current)
    }
}

pub fn every(models: &[Offer<'_>], to: Landing<'_>, chat: Chat<'_>) -> Vec<String> {
    let mut out = offered(models, to, chat);
    match to.models {
        Some(own) => add_missing(&mut out, own.iter().map(|m| m.id.as_str())),
        None => add_missing(&mut out, table_rows(models, to.provider)),
    }
    out
}

pub fn reachable(models: &[Offer<'_>], to: Landing<'_>, chat: Chat<'_>) -> Vec<String> {
    let mut out = every(models, to, chat);
    if !to.complete {
        add_missing(&mut out, table_rows(models, to.provider));
    }
    out
}

fn table_rows<'a>(models: &'a [Offer<'_>], account: &'a str) -> impl Iterator<Item = &'a str> {
    let rows = models.iter().filter(move |m| serves(m.provider, account));
    rows.map(|m| m.id)
}

fn add_missing<'a>(out: &mut Vec<String>, ids: impl Iterator<Item = &'a str>) {
    for id in ids {
        if !out.iter().any(|known| known == id) {
            out.push(id.to_owned());
        }
    }
}

#[cfg(test)]
mod tests;
