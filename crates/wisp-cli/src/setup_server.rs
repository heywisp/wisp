use std::collections::VecDeque;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use tokio::sync::{mpsc, oneshot, watch};
use wisp_core::endpoints::host::{EndpointHost, SaveOutcome};
use wisp_protocol::endpoint::SET_UP_FIRST;
use wisp_protocol::subscription_auth::AuthEvent;
use wisp_protocol::voice::{VoiceCommand, VoiceEvent};
use wisp_protocol::{AgentEvent, ClientCommand, ErrorKind, RejectKind};
use wisp_tui::app::setup::SessionStart;

pub(crate) fn refusal(command: &ClientCommand) -> AgentEvent {
    if let ClientCommand::Voice {
        command: VoiceCommand::Start { call, .. },
    } = command
    {
        let event = VoiceEvent::failed(call.clone(), ErrorKind::InvalidRequest, SET_UP_FIRST);
        return AgentEvent::Voice { event };
    }
    let (id, request) = match command {
        ClientCommand::SubscriptionAuth { command } => (None, Some(command.request())),
        ClientCommand::Send { id, .. } => (Some(id.clone()), None),
        ClientCommand::PlanUsage { request, .. }
        | ClientCommand::ChangeSettings { request, .. }
        | ClientCommand::AnswerProviderSwitch { request, .. }
        | ClientCommand::DescribeSettings { request }
        | ClientCommand::ContextReport { request, .. }
        | ClientCommand::SaveEndpoint { request, .. }
        | ClientCommand::EditEndpoint { request, .. }
        | ClientCommand::RemoveEndpoint { request, .. }
        | ClientCommand::Compact { request, .. }
        | ClientCommand::Retry { request, .. }
        | ClientCommand::Rewind { request, .. }
        | ClientCommand::RewindPoints { request }
        | ClientCommand::SpawnAgent { request, .. }
        | ClientCommand::RenameAgent { request, .. }
        | ClientCommand::ListAgents { request }
        | ClientCommand::DescribeSession { request }
        | ClientCommand::ListChats { request }
        | ClientCommand::ListSpend { request }
        | ClientCommand::StartOn { request, .. }
        | ClientCommand::RunPluginCommand { request, .. } => (None, Some(*request)),
        ClientCommand::Child { .. }
        | ClientCommand::EndAgent { .. }
        | ClientCommand::WatchAgent { .. }
        | ClientCommand::GetReady { .. }
        | ClientCommand::WindowGone => (None, None),
        ClientCommand::SettingsUsed { .. }
        | ClientCommand::Withdraw { .. }
        | ClientCommand::Release
        | ClientCommand::Cancel
        | ClientCommand::Answer { .. }
        | ClientCommand::ScreenReport { .. }
        | ClientCommand::StopProcess { .. }
        | ClientCommand::CheckProcesses
        | ClientCommand::Background { .. }
        | ClientCommand::AttachProcess { .. }
        | ClientCommand::ProcessInput { .. }
        | ClientCommand::DetachProcess { .. }
        | ClientCommand::DeclineAttach { .. }
        | ClientCommand::AnswerSecret { .. }
        | ClientCommand::ReturnedTaken { .. }
        | ClientCommand::SlotsShown { .. }
        | ClientCommand::Terminal { .. }
        | ClientCommand::Voice { .. } => (None, None),
    };
    AgentEvent::Rejected {
        reason: SET_UP_FIRST.to_owned(),
        id,
        kind: Some(RejectKind::Endpoint),
        request,
    }
}

type SignIns<'a> = FuturesUnordered<BoxFuture<'a, AuthEvent>>;

async fn signed(events: &mpsc::Sender<AgentEvent>, event: AuthEvent) {
    drop(events.send(AgentEvent::SubscriptionAuth { event }).await);
}

async fn sign_ins_end(
    host: &dyn EndpointHost,
    signing: &mut SignIns<'_>,
    events: &mpsc::Sender<AgentEvent>,
) {
    host.cancel_auth();
    while let Some(event) = signing.next().await {
        signed(events, event).await;
    }
}

pub(crate) async fn serve<T>(
    host: &dyn EndpointHost,
    mut commands: mpsc::Receiver<ClientCommand>,
    events: mpsc::Sender<AgentEvent>,
    started: oneshot::Sender<Result<SessionStart, String>>,
    start: impl AsyncFnOnce(String, Vec<String>) -> Result<(SessionStart, T), String>,
) -> Option<T> {
    let (cancel, _) = watch::channel(false);
    let mut waiting: VecDeque<ClientCommand> = VecDeque::new();
    let mut signing = SignIns::new();
    loop {
        let command = match waiting.pop_front() {
            Some(command) => command,
            None => tokio::select! {
                next = commands.recv() => match next {
                    Some(command) => command,
                    None => {
                        sign_ins_end(host, &mut signing, &events).await;
                        return None;
                    }
                },
                Some(event) = signing.next(), if !signing.is_empty() => {
                    signed(&events, event).await;
                    continue;
                }
            },
        };
        match command {
            ClientCommand::SlotsShown { .. } | ClientCommand::Terminal { .. } => continue,
            ClientCommand::SubscriptionAuth { command } => {
                signing.push(host.auth(command));
                continue;
            }
            _ => {}
        }
        if let ClientCommand::StartOn {
            account, follow, ..
        } = command
        {
            sign_ins_end(host, &mut signing, &events).await;
            if commands.is_closed() {
                return None;
            }
            return match start(account, follow).await {
                Ok((session, task)) => {
                    drop(started.send(Ok(session)));
                    Some(task)
                }
                Err(e) => {
                    drop(started.send(Err(e)));
                    None
                }
            };
        }
        let ClientCommand::SaveEndpoint {
            request,
            endpoint,
            key,
            check,
        } = command
        else {
            if events.send(refusal(&command)).await.is_err() {
                return None;
            }
            continue;
        };
        let mut save = host.save(endpoint, key, check, cancel.subscribe());
        let mut open = true;
        let outcome = loop {
            tokio::select! {
                outcome = &mut save => break outcome,
                Some(event) = signing.next(), if !signing.is_empty() => signed(&events, event).await,
                next = commands.recv(), if open => match next {
                    Some(ClientCommand::SlotsShown { .. } | ClientCommand::Terminal { .. }) => {}
                    Some(ClientCommand::SubscriptionAuth { command }) => {
                        signing.push(host.auth(command));
                    }
                    Some(other) => waiting.push_back(other),
                    None => {
                        open = false;
                        cancel.send_replace(true);
                        host.cancel_auth();
                    }
                },
            }
        };
        let answer = match outcome {
            Ok(SaveOutcome::Saved(saved)) => Some(AgentEvent::EndpointSaved {
                answers: request,
                name: saved.name,
                store: saved.store,
                check: saved.check,
                notes: saved.notes,
            }),
            Ok(SaveOutcome::NotSaved(not)) => Some(AgentEvent::EndpointNotSaved {
                answers: request,
                name: not.name,
                why: not.why,
            }),
            Ok(SaveOutcome::Cancelled) => None,
            Err(e) => Some(AgentEvent::endpoint_refused(Some(request), e.to_string())),
        };
        if let Some(answer) = answer {
            drop(events.send(answer).await);
        }
        if !open {
            for other in waiting.drain(..) {
                drop(events.send(refusal(&other)).await);
            }
            sign_ins_end(host, &mut signing, &events).await;
            return None;
        }
    }
}

#[cfg(test)]
mod tests;
