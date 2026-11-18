use tokio::sync::mpsc;
use wisp_protocol::AgentEvent;
use wisp_protocol::log::LogEvent;

use super::{Agent, Spend};
use crate::voice::CallRecord;

pub(super) type Calls = (
    mpsc::UnboundedSender<CallRecord>,
    mpsc::UnboundedReceiver<CallRecord>,
);

pub(super) async fn ended(calls: &mut mpsc::UnboundedReceiver<CallRecord>) -> CallRecord {
    match calls.recv().await {
        Some(call) => call,
        None => std::future::pending().await,
    }
}

impl Agent {
    pub fn voice_sender(&self) -> mpsc::UnboundedSender<CallRecord> {
        self.voice_calls.0.clone()
    }

    pub(super) async fn book_voice(&mut self, call: CallRecord, events: &mpsc::Sender<AgentEvent>) {
        let lower_bound = call.unpriced();
        let spend = Spend {
            usage: call.usage,
            usd: call.cost_usd,
            list_usd: call.list_usd,
            lower_bound,
        };
        if let Some(account) = &call.endpoint_name {
            self.add_session_to(spend, account, call.model.as_deref().unwrap_or_default());
        }
        let record = LogEvent::VoiceCall {
            call: call.call,
            outcome: call.outcome,
            seconds: call.seconds,
            lines: call.lines,
            unknown: call.unknown,
            endpoint_name: call.endpoint_name,
            usage: call.usage,
            cost_usd: call.cost_usd,
            list_usd: call.list_usd,
            model: call.model,
            lower_bound,
        };
        self.log(events, record).await;
    }

    pub(super) async fn book_voice_left(&mut self, events: &mpsc::Sender<AgentEvent>) {
        while let Ok(call) = self.voice_calls.1.try_recv() {
            self.book_voice(call, events).await;
        }
    }
}

#[cfg(test)]
mod tests;
