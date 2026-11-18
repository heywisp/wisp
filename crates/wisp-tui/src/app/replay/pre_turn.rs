use wisp_protocol::settings::{
    AppliedChange, Layer, Restored, SettingName, SettingTarget, SettingValue, When, Withdrawal,
    find, names,
};
use wisp_protocol::{
    AgentEvent, CacheEffect, CompactionFailure, CompactionTrigger, Effort, InputId, RequestId,
};

use super::{Agent, COMPACTION_STOPPED, Pending, Settings};

pub(super) fn trails_turn(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::SettingsChanged {
            answers: None,
            withdrawn: Some(_),
            ..
        } | AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Background,
            turn: None,
            ..
        } | AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Background,
            turn: None,
            ..
        } | AgentEvent::CompactionPrepared { turn: None, .. }
    )
}

#[derive(Debug, Default)]
pub(super) struct PreTurn {
    pub(super) open: Option<CompactionTrigger>,
    pub(super) holding: Option<CompactionTrigger>,
    pub(super) cancelled: bool,
    pub(super) converting: bool,
    pub(super) stop_due: bool,
}

impl PreTurn {
    pub(super) fn saw(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Convert,
                ..
            } => self.converting = true,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Convert,
                ..
            }
            | AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Convert,
                ..
            } => self.converting = false,
            AgentEvent::CompactionStarted {
                trigger,
                turn: Some(_),
                ..
            } => {
                self.open = Some(*trigger);
                self.holding = Some(*trigger);
            }
            AgentEvent::CompactionFailed {
                trigger,
                kind: CompactionFailure::Cancelled,
                turn: Some(_),
                ..
            } => {
                self.open = None;
                self.cancelled |= *trigger == CompactionTrigger::Switch;
            }
            AgentEvent::Compacted { turn: Some(_), .. }
            | AgentEvent::CompactionFailed { turn: Some(_), .. } => self.open = None,
            _ => {}
        }
    }
}

impl Settings {
    pub(super) fn scripted(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::SettingsChanged {
                answers: None,
                withdrawn: Some(_),
                endpoint,
                model,
                effort,
                restored,
                ..
            } => self.withdraw(endpoint, model, *effort, restored.as_ref()),
            AgentEvent::SettingsChanged {
                withdrawn: None,
                endpoint,
                model,
                effort,
                ..
            } => {
                let unchanged =
                    *endpoint == self.endpoint && *model == self.model && *effort == self.effort;
                self.pending = (!unchanged).then(|| Pending {
                    endpoint: endpoint.clone(),
                    model: model.clone(),
                    effort: *effort,
                });
            }
            _ => {}
        }
    }

    fn withdraw(&mut self, endpoint: &str, model: &str, next: Effort, restored: Option<&Restored>) {
        endpoint.clone_into(&mut self.endpoint);
        model.clone_into(&mut self.model);
        if let Some(restored) = restored {
            self.effort = restored.effort;
        }
        self.pending = (next != self.effort).then(|| Pending {
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            effort: next,
        });
    }

    pub(super) fn withdrawal(&mut self) -> AgentEvent {
        let wanted = self.pending.as_ref().map_or(self.effort, |p| p.effort);
        let takes = self
            .efforts
            .get(&self.model)
            .is_none_or(|efforts| efforts.contains(&wanted));
        let effort = if takes { wanted } else { self.effort };
        let (endpoint, model) = (self.endpoint.clone(), self.model.clone());
        let mut restated = vec![
            (names::MODEL, model.clone()),
            (names::ENDPOINT, endpoint.clone()),
        ];
        if !takes {
            restated.push((names::EFFORT, self.effort.as_str().to_owned()));
        }
        let changes = restated
            .into_iter()
            .filter_map(|(name, value)| {
                let spec = find(name)?;
                Some(AppliedChange {
                    target: SettingTarget::Name(SettingName::of(spec)),
                    value: Some(SettingValue::Text(value)),
                    layer: Layer::Session,
                    when: When::NextTurn,
                    cache: Some(CacheEffect::Kept),
                    consequence: None,
                    saved: None,
                    overridden: None,
                    warnings: Vec::new(),
                })
            })
            .collect();
        let restored = Restored {
            effort: self.effort,
            cache_ttl: self.cache_ttl,
        };
        self.withdraw(&endpoint, &model, effort, Some(&restored));
        AgentEvent::SettingsChanged {
            endpoint,
            model,
            effort,
            cache: CacheEffect::Kept,
            account_bound: Vec::new(),
            answers: None,
            changes,
            withdrawn: Some(Withdrawal::SwitchCompactionCancelled),
            restored: Some(restored),
        }
    }
}

pub(super) type Unstarted<'a> = (Option<(u64, RequestId)>, &'a [(InputId, u32)]);

pub(super) type StopAt = (Option<CompactionTrigger>, bool, Option<(u64, RequestId)>);

pub(super) type Stop<'a> = (
    CompactionTrigger,
    Option<(u64, RequestId)>,
    &'a [(InputId, u32)],
);

impl Agent {
    fn opened(&self, retry: Option<(u64, RequestId)>) -> AgentEvent {
        let (origin, ends) = self.recorded.clone();
        AgentEvent::TurnStarted {
            turn: self.turn,
            origin,
            retry_of: retry.map(|(of, _)| of),
            answers: retry.map(|(_, request)| request),
            ends,
        }
    }

    pub(super) async fn stop_at(
        &mut self,
        (open, unstarted, retry): StopAt,
        delivered: &[(InputId, u32)],
        committed: u32,
    ) -> Option<bool> {
        let stopped = match open {
            Some(trigger) => self.stop_pre_turn((trigger, retry, delivered)).await,
            None if unstarted => self.stop_unstarted((retry, delivered)).await,
            None => self.stop(delivered, committed).await,
        };
        stopped.then_some(false)
    }

    pub(super) async fn stop_unstarted(&mut self, (retry, delivered): Unstarted<'_>) -> bool {
        if retry.is_some() {
            self.settings.start_retry();
        } else {
            self.settings.start_turn();
        }
        let started = self.opened(retry);
        self.emit(started).await && self.stop(delivered, 0).await
    }

    pub(super) async fn stop_pre_turn(&mut self, (trigger, retry, delivered): Stop<'_>) -> bool {
        let failed = AgentEvent::CompactionFailed {
            trigger,
            engine: None,
            kind: CompactionFailure::Cancelled,
            message: COMPACTION_STOPPED.to_owned(),
            usage: None,
            cost: None,
            answers: None,
            turn: Some(self.turn),
            paused: false,
            requested_by: None,
        };
        if trigger != CompactionTrigger::Switch {
            return self.emit(failed).await && self.stop_unstarted((retry, delivered)).await;
        }
        let started = self.opened(retry);
        if !(self.emit(failed).await && self.emit(started).await) {
            return false;
        }
        if !self.stop(delivered, 0).await {
            return false;
        }
        if retry.is_some() {
            return true;
        }
        let withdrawn = self.settings.withdrawal();
        self.emit(withdrawn).await
    }
}
