use wisp_protocol::settings::{AppliedChange, Consequence, Remembered, SwitchQuote, SwitchTarget};
use wisp_protocol::{AgentEvent, CompactionFailure, CompactionTrigger, SwitchCompactionCause};
use wisp_render::chrome::{
    Alive, CacheSize, Effect, Facts, Rebuild, Scope, SwitchCause, SwitchWork, model_line, one_line,
};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::copy;

use super::{Tracker, priced};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Quoted {
    #[default]
    Unknown,
    Nothing,
    Cause(SwitchCompactionCause, u64),
}

#[derive(Clone, Debug, Default)]
pub(super) struct Switching {
    quotes: Vec<SwitchQuote>,
    quoted: Quoted,
    failed: Option<String>,
    covered: Option<u64>,
    prepared: Option<u64>,
    turn: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Work {
    pub(super) work: SwitchWork,
    pub(super) head_tokens: u64,
}

fn compaction_of(consequence: Option<&Consequence>) -> Option<(u64, u64, &SwitchCompactionCause)> {
    match consequence? {
        Consequence::CompactsBeforeSwitch {
            estimate_micros,
            head_tokens,
            cause,
        } => Some((*estimate_micros, *head_tokens, cause)),
        _ => None,
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "an estimate in millionths of a dollar shown to the cent"
)]
fn usd_of(micros: u64) -> f64 {
    micros as f64 / 1_000_000.0
}

impl Tracker {
    fn quote(&self, target: &SwitchTarget) -> Option<&SwitchQuote> {
        self.switching.quotes.iter().find(|q| q.target == *target)
    }

    fn quote_for(&self, target: &SwitchTarget) -> Option<&Consequence> {
        self.quote(target).and_then(|q| q.consequence.as_ref())
    }

    pub fn remembered(&self, account: &str) -> Option<&Remembered> {
        self.quote(&SwitchTarget::Endpoint(account.to_owned()))
            .and_then(|q| q.remembered.as_ref())
    }

    fn compaction_priced(&self, facts: &Facts) -> bool {
        priced(facts, &self.session.endpoint)
            && facts
                .rates_on(&self.session.endpoint, &self.session.model)
                .is_some()
    }

    pub(super) fn work(
        &self,
        facts: &Facts,
        micros: u64,
        cause: &SwitchCompactionCause,
    ) -> SwitchWork {
        let priced = self.compaction_priced(facts);
        SwitchWork {
            usd: (priced && micros > 0).then(|| usd_of(micros)),
            cause: SwitchCause::of(cause),
            alone: priced && micros == 0 && *cause == SwitchCompactionCause::Convert,
        }
    }

    pub(super) fn head_rebuild(
        &self,
        facts: &Facts,
        endpoint: &str,
        model: &str,
        head_tokens: u64,
    ) -> Rebuild {
        let prefix = self.prefix.unwrap_or(0);
        let rates = facts.rates_on(endpoint, model);
        let rebuild = Rebuild::estimate(
            CacheSize {
                prefix,
                total: prefix.saturating_add(head_tokens),
            },
            Scope::Exact,
            Alive {
                prefix: true,
                tail: true,
            },
            rates,
            facts.writes_on(endpoint, model, self.next_ttl()),
        );
        if priced(facts, endpoint) {
            rebuild.billed(facts.billing_on(endpoint))
        } else {
            rebuild.unpriced()
        }
    }

    pub(super) fn quoted_switch(
        &self,
        facts: &Facts,
        target: &SwitchTarget,
        (endpoint, model): (&str, &str),
        rebuild: Rebuild,
    ) -> Effect {
        match compaction_of(self.quote_for(target)) {
            Some((micros, head_tokens, cause)) => Effect::Switch(
                self.head_rebuild(facts, endpoint, model, head_tokens),
                self.work(facts, micros, cause),
            ),
            None => Effect::Rebuild(rebuild),
        }
    }

    pub(super) fn changed_work(
        &mut self,
        facts: &Facts,
        changes: &[AppliedChange],
        switched: bool,
    ) -> Option<Work> {
        let found = changes
            .iter()
            .find_map(|c| compaction_of(c.consequence.as_ref()));
        if switched {
            self.switching.quoted = match found {
                Some((micros, _, cause)) => Quoted::Cause(cause.clone(), micros),
                None => Quoted::Nothing,
            };
        }
        let (micros, head_tokens, cause) = found?;
        Some(Work {
            work: self.work(facts, micros, cause),
            head_tokens,
        })
    }

    fn switch_target(&self) -> String {
        let model = (self.next.model != self.session.model).then(|| model_line(&self.next.model));
        let endpoint =
            (self.next.endpoint != self.session.endpoint).then(|| one_line(&self.next.endpoint));
        match (model, endpoint) {
            (Some(m), Some(e)) => copy::model_on(&m, &e),
            (Some(m), None) => m,
            (None, Some(e)) => e,
            (None, None) => model_line(&self.session.model),
        }
    }

    fn not_quoted(&self, facts: &Facts, cause: &SwitchCompactionCause) -> bool {
        match &self.switching.quoted {
            Quoted::Unknown => false,
            Quoted::Nothing => true,
            Quoted::Cause(quoted, micros) => {
                quoted != cause || (*micros == 0 && self.compaction_priced(facts))
            }
        }
    }

    pub(super) fn switch_event(
        &mut self,
        event: &AgentEvent,
        facts: &Facts,
        set: GlyphSet,
    ) -> Option<String> {
        let glyphs = Glyphs::of(set);
        match event {
            AgentEvent::SettingsSnapshot(snapshot) => {
                self.switching.quotes.clone_from(&snapshot.switches);
                None
            }
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                switch: Some(run),
                ..
            } => {
                let mut note = copy::compacting_before_switch(
                    &self.switch_target(),
                    self.not_quoted(facts, &run.cause),
                );
                if let Some(usd) = self.work(facts, run.estimate_micros, &run.cause).usd {
                    note = format!(
                        "{note} {} {}",
                        glyphs.separator,
                        wisp_protocol::marked_amount(&copy::usd(usd), true, false)
                    );
                }
                Some(note)
            }
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Switch,
                kind,
                ..
            } if *kind != CompactionFailure::Cancelled => {
                self.switching.failed = Some(self.switch_target());
                None
            }
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Convert,
                partial,
                ..
            } => {
                let target = self.switching.failed.take();
                let lost = self
                    .switching
                    .covered
                    .map(|c| (c.saturating_add(1), self.switching.turn))
                    .filter(|(first, last)| *partial && first <= last);
                self.covered_now();
                let mut note = copy::switch_converted(&target?);
                if let Some((first, last)) = lost {
                    let dash = if set == GlyphSet::Ascii {
                        "-"
                    } else {
                        "\u{2013}"
                    };
                    note = format!(
                        "{note} {} {}",
                        glyphs.separator,
                        copy::turns_kept_as_messages(first, last, dash)
                    );
                }
                Some(note)
            }
            AgentEvent::CompactionPrepared { covers, .. } => {
                self.switching.prepared = covers.as_ref().map(|c| c.last_turn);
                None
            }
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Background,
                ..
            } => {
                if let Some(last) = self.switching.prepared.take() {
                    self.switching.covered = Some(last);
                }
                None
            }
            AgentEvent::Compacted { .. } => {
                self.covered_now();
                None
            }
            AgentEvent::TurnStarted { turn, .. } => {
                self.switching.turn = *turn;
                self.switching.quoted = Quoted::Unknown;
                self.switching.failed = None;
                None
            }
            AgentEvent::SettingsChanged {
                withdrawn: Some(_), ..
            } => {
                self.switching.quoted = Quoted::Unknown;
                self.switching.failed = None;
                None
            }
            _ => None,
        }
    }

    fn covered_now(&mut self) {
        self.switching.covered = Some(self.switching.turn);
    }
}
