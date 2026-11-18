use std::collections::BTreeMap;

use wisp_media::tokens;
use wisp_protocol::log::{FeatureState, ImageRef};
use wisp_protocol::{
    AgentEvent, Billing, CacheEffect, Continued, Effort, Feature, HookChange, InputId,
    RebuildReason, TurnOutcome,
};
use wisp_render::chrome::account::{AccountState, Said};
use wisp_render::chrome::{Change, Endpoint, Facts, Rebuild, change_note};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::{Transcript, UserPart, copy};

use super::inputs::{unkept, user_tag};
use super::mail::MailBook;
use super::picker::display_effort;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resumed {
    pub view: wisp_protocol::SessionView,
    pub continuing: Option<Continuing>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Continuing {
    pub reason: Option<Said>,
    pub from: Option<AccountState>,
    pub rebuild: Option<Rebuild>,
}

impl Continuing {
    pub fn of(continued: Continued, billing: Billing) -> Self {
        Self {
            reason: continued.reason,
            from: continued.from.map(AccountState::of),
            rebuild: continued
                .quote
                .map(|usd| wisp_render::chrome::account::quoted(usd, AccountState::of(billing))),
        }
    }
}

pub use wisp_protocol::opening::StartNote;

pub use wisp_protocol::{Pair, Returned, ReturnedImage};

pub fn returned_note(returned: &[Returned]) -> Option<String> {
    let missing: usize = returned.iter().map(|r| r.missing).sum();
    copy::returned_after_resume(returned.len(), missing)
}

pub fn question_interrupted(view: &wisp_protocol::SessionView) -> bool {
    let Some(end) = view
        .events
        .iter()
        .rposition(|event| matches!(event, AgentEvent::TurnEnded { .. }))
    else {
        return false;
    };
    view.events[..=end]
        .iter()
        .rev()
        .take_while(|event| !matches!(event, AgentEvent::TurnStarted { .. }))
        .any(|event| {
            matches!(
                event,
                AgentEvent::QuestionClosed {
                    how: wisp_protocol::QuestionEnd::Interrupted,
                    ..
                }
            )
        })
}

struct Accepted {
    text: String,
    images: Vec<(u32, u32)>,
    turns: u64,
}

struct Rewrite {
    hook: String,
    before: String,
    after: String,
}

pub struct History {
    glyphs: GlyphSet,
    accepted: BTreeMap<InputId, Accepted>,
    mail: MailBook,
    running: bool,
    cells: BTreeMap<InputId, usize>,
    rewrites: BTreeMap<InputId, Rewrite>,
    turns: u64,
    retrying: bool,
    pair: Option<Pair>,
    retried_on: BTreeMap<u64, Pair>,
    failed_steps: Option<u32>,
    retry_note: Option<String>,
    search_refused_on: Option<String>,
}

fn parts(accepted: &Accepted, glyphs: GlyphSet) -> Vec<UserPart> {
    let mut parts: Vec<UserPart> = accepted
        .images
        .iter()
        .enumerate()
        .map(|(i, &(w, h))| UserPart::Image {
            label: copy::image_chip(i + 1, (w, h), tokens(w, h), glyphs),
        })
        .collect();
    let spaced = !parts.is_empty() && !accepted.text.starts_with(char::is_whitespace);
    if !accepted.text.is_empty() && spaced {
        parts.push(UserPart::Text(format!(" {}", accepted.text)));
    } else if !accepted.text.is_empty() || parts.is_empty() {
        parts.push(UserPart::Text(accepted.text.clone()));
    }
    parts
}

fn sizes(images: &[ImageRef]) -> Vec<(u32, u32)> {
    images.iter().map(|i| (i.width, i.height)).collect()
}

impl History {
    pub fn new(glyphs: GlyphSet, started: Option<Pair>) -> History {
        History {
            glyphs,
            accepted: BTreeMap::new(),
            mail: MailBook::default(),
            running: false,
            cells: BTreeMap::new(),
            rewrites: BTreeMap::new(),
            turns: 0,
            retrying: false,
            pair: started,
            retried_on: BTreeMap::new(),
            failed_steps: None,
            retry_note: None,
            search_refused_on: None,
        }
    }

    #[must_use]
    pub fn retried_on(mut self, turns: BTreeMap<u64, Pair>) -> History {
        self.retried_on = turns;
        self
    }

    pub fn mail_drawn(&self) -> impl Iterator<Item = InputId> + '_ {
        self.mail.drawn()
    }

    pub fn turn_edge(&mut self, event: &AgentEvent, transcript: &mut Transcript) {
        match event {
            AgentEvent::TurnStarted { .. } => self.running = true,
            AgentEvent::TurnEnded { outcome, .. } => {
                self.running = false;
                self.mail.ended(outcome, transcript);
            }
            _ => {}
        }
    }

    pub fn take_retry_note(&mut self) -> Option<String> {
        self.retry_note.take()
    }

    fn sep(&self) -> String {
        format!(" {} ", Glyphs::of(self.glyphs).separator)
    }

    fn retry_line(&self, turn: u64, facts: &Facts) -> Option<String> {
        let pair = self.retried_on.get(&turn).or(self.pair.as_ref())?;
        let failed = self.failed_steps?;
        let step = (failed > 0).then(|| failed.saturating_add(1));
        let effort = display_effort(facts, &pair.model, Some(pair.effort));
        Some(
            copy::retrying_on(
                step,
                &pair.endpoint,
                &pair.model,
                effort.map(Effort::as_str),
            )
            .join(&self.sep()),
        )
    }

    fn settings_note(
        &mut self,
        facts: &Facts,
        endpoint: &str,
        model: &str,
        effort: Effort,
        cache: &CacheEffect,
        account_bound: &[String],
    ) -> String {
        let before = self.pair.replace(Pair {
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            effort,
        });
        let endpoint_changed = before.as_ref().is_none_or(|p| p.endpoint != endpoint)
            || *cache
                == CacheEffect::Rebuilt {
                    reason: RebuildReason::EndpointSwitch,
                };
        let search_refused_on = match cache {
            CacheEffect::Rebuilt {
                reason: RebuildReason::NativeSearchRefused,
            } => self.search_refused_on.take(),
            _ => None,
        };
        let rebuilt_on = search_refused_on.as_deref().unwrap_or(endpoint);
        let change = Change {
            endpoint: (endpoint_changed && !endpoint.is_empty()).then_some(endpoint),
            account_bound,
            model,
            effort: display_effort(facts, model, Some(effort)),
            cache,
            model_changed: before.as_ref().is_none_or(|p| p.model != model),
            effort_changed: before.as_ref().is_none_or(|p| p.effort != effort),
            rebuild: if self.turns == 0 {
                Rebuild::Empty
            } else {
                Rebuild::Unknown
            },
            search_refused_on: Some(rebuilt_on),
            search_refused_gateway: facts.reach_of(rebuilt_on) == Endpoint::Custom,
            search: None,
            work: None,
        };
        change_note(&change, &Glyphs::of(self.glyphs))
    }

    pub fn apply(
        &mut self,
        event: &AgentEvent,
        facts: &Facts,
        transcript: &mut Transcript,
    ) -> bool {
        match event {
            AgentEvent::FeatureStateChanged {
                endpoint,
                feature: Feature::NativeWebSearch,
                state: FeatureState::FallenBack,
                ..
            } => {
                self.search_refused_on = Some(endpoint.clone());
                false
            }
            AgentEvent::SessionResumed { .. } => {
                self.accepted.clear();
                self.mail.clear();
                self.running = false;
                self.cells.clear();
                true
            }
            AgentEvent::TurnStarted { turn, retry_of, .. } => {
                self.turns = self.turns.saturating_add(1);
                self.retrying = retry_of.is_some();
                self.retry_note = retry_of.and_then(|_| self.retry_line(*turn, facts));
                false
            }
            AgentEvent::TurnEnded { outcome, .. } => {
                self.failed_steps = match outcome {
                    TurnOutcome::Error {
                        committed_steps, ..
                    } => Some(*committed_steps),
                    _ => None,
                };
                false
            }
            AgentEvent::SettingsChanged {
                endpoint,
                model,
                effort,
                cache,
                account_bound,
                withdrawn: None,
                ..
            } => {
                let note =
                    self.settings_note(facts, endpoint, model, *effort, cache, account_bound);
                transcript.note(&note);
                false
            }
            AgentEvent::InputAccepted {
                id,
                text,
                images,
                opening,
                from: None,
                ..
            } => {
                let accepted = Accepted {
                    text: text.clone(),
                    images: sizes(images),
                    turns: self.turns + u64::from(*opening),
                };
                self.accepted.insert(id.clone(), accepted);
                true
            }
            AgentEvent::InputAccepted {
                id,
                text,
                from: Some(from),
                ..
            } => {
                self.mail
                    .accepted(id, from, text, !self.running, transcript);
                true
            }
            AgentEvent::InputDelivered { ids, step } => {
                let queued = ids
                    .iter()
                    .filter_map(|id| self.accepted.get(id))
                    .any(|a| a.turns < self.turns);
                let mut first = true;
                for id in ids {
                    let Some(accepted) = self.accepted.get(id) else {
                        continue;
                    };
                    let tag = user_tag(self.retrying, *step, queued && first);
                    first = false;
                    let rewrite = self.rewrites.remove(id);
                    let cell = match &rewrite {
                        Some(r) if r.after == accepted.text => {
                            let typed = Accepted {
                                text: r.before.clone(),
                                images: accepted.images.clone(),
                                turns: accepted.turns,
                            };
                            transcript.deliver(parts(&typed, self.glyphs), tag)
                        }
                        _ => transcript.deliver(parts(accepted, self.glyphs), tag),
                    };
                    if let Some(r) = rewrite {
                        transcript.rewritten(cell, &r.hook, &r.after);
                    }
                    self.cells.insert(id.clone(), cell);
                }
                self.mail.delivered(ids, transcript);
                true
            }
            AgentEvent::HookApplied {
                hook,
                input_id: Some(id),
                change: HookChange::Text { before, after },
                ..
            } => {
                match self.cells.get(id) {
                    Some(cell) => transcript.rewritten(*cell, hook, after),
                    None => {
                        let rewrite = Rewrite {
                            hook: hook.clone(),
                            before: before.clone(),
                            after: after.clone(),
                        };
                        self.rewrites.insert(id.clone(), rewrite);
                    }
                }
                false
            }
            AgentEvent::InputWithdrawn { ids, by } => {
                if unkept(by) {
                    for cell in ids.iter().filter_map(|id| self.cells.get(id)) {
                        transcript.not_kept(*cell);
                    }
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
