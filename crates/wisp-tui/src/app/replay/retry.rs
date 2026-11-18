use wisp_protocol::{AgentEvent, RejectKind, RequestId, TurnOutcome};

use super::{Agent, NO_MORE_TURNS, Settings};

pub(super) const NOTHING_TO_RETRY: &str = "nothing to retry";

impl Settings {
    pub(super) fn start_retry(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.endpoint.clone_from(&pending.endpoint);
        let kept = pending.model != self.model || pending.effort != self.effort;
        self.pending = kept.then_some(pending);
    }

    fn next_endpoint(&self) -> &str {
        self.pending
            .as_ref()
            .map_or(self.endpoint.as_str(), |p| p.endpoint.as_str())
    }
}

pub(super) fn offered(outcome: &TurnOutcome) -> bool {
    matches!(outcome, TurnOutcome::Error { offer: Some(_), .. })
}

impl Agent {
    async fn refuse_retry(&mut self, request: RequestId, reason: String) -> bool {
        self.emit(AgentEvent::Rejected {
            reason,
            id: None,
            kind: Some(RejectKind::NotRetryable),
            request: Some(request),
        })
        .await
    }

    pub(super) async fn refuse_running_retry(&mut self, turn: u64, request: RequestId) -> bool {
        let reason = if turn == self.turn {
            NOTHING_TO_RETRY.to_owned()
        } else {
            format!("turn {turn} is no longer the last turn")
        };
        self.refuse_retry(request, reason).await
    }

    pub(super) async fn retry(
        &mut self,
        turn: u64,
        endpoint: Option<String>,
        request: RequestId,
        out_of_turns: bool,
    ) -> Option<bool> {
        let refusal = if turn != self.turn {
            Some(format!("turn {turn} is no longer the last turn"))
        } else if self.offered != Some(turn) {
            Some(NOTHING_TO_RETRY.to_owned())
        } else if out_of_turns {
            Some(NO_MORE_TURNS.to_owned())
        } else {
            None
        };
        if let Some(reason) = refusal {
            return self.refuse_retry(request, reason).await.then_some(false);
        }
        self.offered = None;
        if let Some(name) = endpoint.filter(|e| e != self.settings.next_endpoint()) {
            let switched = self.settings.answer(Some(name), None, None);
            if !self.emit(switched).await {
                return None;
            }
        }
        let ids = self.inbox.unhold();
        if !ids.is_empty()
            && !self
                .emit(AgentEvent::InputUnheld { ids, front: false })
                .await
        {
            return None;
        }
        self.retry = Some((turn, request));
        Some(true)
    }
}
