use tokio::sync::{mpsc, oneshot};
use wisp_protocol::{
    AgentEvent, Answer, AttachRequestEnd, ImageData, Question, QuestionEnd, RejectKind, Secret,
    SecretRequestEnd, answer_refusal, check_answers, check_secret, report_refusal, secret_refusal,
};

use super::Agent;
use crate::call::user::{Goes, ScreenReport};
use crate::tool::{AskReply, AskRequest, SecretReply};

#[derive(Debug)]
pub(super) struct OpenQuestion {
    call_id: String,
    questions: Vec<Question>,
    reply: oneshot::Sender<AskReply>,
}

#[derive(Debug)]
pub(super) struct OpenReport {
    call_id: String,
    reply: oneshot::Sender<ScreenReport>,
}

#[derive(Debug)]
pub(super) struct OpenAttach {
    call_id: String,
    pub(super) id: u32,
    reply: oneshot::Sender<AskReply>,
}

#[derive(Debug)]
pub(super) struct OpenSecret {
    call_id: String,
    pub(super) id: Option<u32>,
    reply: oneshot::Sender<SecretReply>,
}

impl Agent {
    pub(super) async fn open_request(
        &mut self,
        call_id: &str,
        request: AskRequest,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        match request {
            AskRequest::Report { reply } => {
                let call_id = call_id.to_owned();
                self.screen_report = Some(OpenReport { call_id, reply });
            }
            AskRequest::Secret { reply, .. } if self.request_open() => {
                let _ = reply.send(SecretReply::Busy);
            }
            request if self.request_open() => request.unavailable(),
            AskRequest::Questions { questions, reply } => {
                self.open_question(call_id, questions, reply, events).await;
            }
            AskRequest::Attach { id, reason, reply } => {
                self.open_attach(call_id, id, reason, reply, events).await;
            }
            AskRequest::Secret {
                goes,
                asked_by,
                name,
                reason,
                show,
                reply,
            } => {
                let open = OpenSecret {
                    call_id: call_id.to_owned(),
                    id: match &goes {
                        Goes::Process(id) => Some(*id),
                        Goes::Server(_) => None,
                    },
                    reply,
                };
                let event = AgentEvent::SecretRequested {
                    call_id: open.call_id.clone(),
                    id: open.id,
                    asked_by,
                    destination: match goes {
                        Goes::Process(_) => None,
                        Goes::Server(destination) => Some(destination),
                    },
                    name,
                    reason,
                    show,
                };
                self.emit(events, event).await;
                let id = open.id;
                self.secret_request = Some(open);
                if let Some(id) = id
                    && !self.mark_asked(id)
                {
                    self.close_secret(SecretRequestEnd::ProcessEnded, None, events)
                        .await;
                }
            }
        }
    }

    pub(super) async fn screen_report(
        &mut self,
        call_id: String,
        report: serde_json::Value,
        images: Vec<ImageData>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        match self.screen_report.take_if(|open| open.call_id == call_id) {
            Some(open) => {
                let _ = open.reply.send(ScreenReport { report, images });
            }
            None => {
                let reason = report_refusal(&call_id);
                self.refuse(None, RejectKind::UnknownId, reason, events)
                    .await;
            }
        }
    }

    async fn open_attach(
        &mut self,
        call_id: &str,
        id: u32,
        reason: String,
        reply: oneshot::Sender<AskReply>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = AgentEvent::AttachRequested {
            call_id: call_id.to_owned(),
            id,
            reason,
        };
        self.emit(events, event).await;
        self.attach_request = Some(OpenAttach {
            call_id: call_id.to_owned(),
            id,
            reply,
        });
        if !self.mark_asked(id) {
            self.close_attach(AttachRequestEnd::ProcessEnded, false, events)
                .await;
        }
    }

    fn mark_asked(&self, id: u32) -> bool {
        let handle = self.processes.handle(id);
        if let Some(handle) = &handle {
            handle.state.set_asked(true);
        }
        handle.is_some_and(|h| h.state.is_running())
    }

    fn unmark_asked(&self, id: u32) {
        if let Some(handle) = self.processes.handle(id) {
            handle.state.set_asked(false);
        }
    }

    pub(super) fn attach_requested(&self) -> Option<u32> {
        self.attach_request.as_ref().map(|open| open.id)
    }

    pub(super) async fn close_attach(
        &mut self,
        how: AttachRequestEnd,
        typed: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(open) = self.attach_request.take() else {
            return;
        };
        self.unmark_asked(open.id);
        let reply = match how {
            AttachRequestEnd::Typed => Some(AskReply::Attached { typed: true }),
            AttachRequestEnd::AttachedOnly => Some(AskReply::Attached { typed: false }),
            AttachRequestEnd::Declined => Some(AskReply::Declined),
            AttachRequestEnd::ProcessEnded => Some(AskReply::ProcessEnded { typed }),
            AttachRequestEnd::Stopped => None,
        };
        let event = AgentEvent::AttachRequestClosed {
            call_id: open.call_id,
            how,
        };
        self.emit(events, event).await;
        if let Some(reply) = reply {
            let _ = open.reply.send(reply);
        }
    }

    pub(super) async fn decline_attach(
        &mut self,
        call_id: String,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self
            .attach_request
            .as_ref()
            .is_some_and(|open| open.call_id == call_id)
        {
            self.close_attach(AttachRequestEnd::Declined, false, events)
                .await;
            return;
        }
        let kind = RejectKind::NoAttachRequest;
        let text = super::inbox::reason(&kind, &call_id);
        self.refuse(None, kind, text, events).await;
    }

    pub(super) fn secret_requested(&self) -> Option<u32> {
        self.secret_request.as_ref().and_then(|open| open.id)
    }

    pub(super) async fn close_secret(
        &mut self,
        how: SecretRequestEnd,
        secret: Option<Secret>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(open) = self.secret_request.take() else {
            return;
        };
        if let Some(id) = open.id {
            self.unmark_asked(id);
        }
        let reply = match how {
            SecretRequestEnd::Entered => secret.map(SecretReply::Entered),
            SecretRequestEnd::ProcessEnded => Some(SecretReply::ProcessEnded),
            SecretRequestEnd::Stopped => None,
        };
        let event = AgentEvent::SecretRequestClosed {
            call_id: open.call_id,
            how,
        };
        self.emit(events, event).await;
        if let Some(reply) = reply {
            let _ = open.reply.send(reply);
        }
    }

    pub(super) async fn secret_command(
        &mut self,
        call_id: String,
        value: Secret,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let line = match &self.secret_request {
            Some(open) if open.call_id == call_id => check_secret(value.expose())
                .map(Secret::new)
                .map_err(|_| RejectKind::BadAnswer),
            _ => Err(RejectKind::UnknownId),
        };
        match line {
            Ok(line) => {
                self.close_secret(SecretRequestEnd::Entered, Some(line), events)
                    .await;
            }
            Err(kind) => {
                let reason = secret_refusal(&kind, &call_id);
                self.refuse(None, kind, reason, events).await;
            }
        }
    }

    pub(super) fn request_open(&self) -> bool {
        self.question.is_some() || self.attach_request.is_some() || self.secret_request.is_some()
    }

    pub(super) async fn stop_requests(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.screen_report = None;
        self.close_question(QuestionEnd::Stopped, events).await;
        self.close_attach(AttachRequestEnd::Stopped, false, events)
            .await;
        self.close_secret(SecretRequestEnd::Stopped, None, events)
            .await;
    }

    async fn open_question(
        &mut self,
        call_id: &str,
        questions: Vec<Question>,
        reply: oneshot::Sender<AskReply>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let turn = self.track().index;
        let event = AgentEvent::QuestionAsked {
            call_id: call_id.to_owned(),
            questions: questions.clone(),
        };
        let seq = self.logged_seq();
        self.log_event(events, &event, Some(turn)).await;
        self.sync_log(events).await;
        self.note_event(&event, seq);
        super::emit(events, event).await;
        self.question = Some(OpenQuestion {
            call_id: call_id.to_owned(),
            questions,
            reply,
        });
    }

    pub(super) async fn answer_command(
        &mut self,
        call_id: String,
        answers: Vec<Answer>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let problem = match &self.question {
            Some(open) if open.call_id == call_id => check_answers(&open.questions, &answers)
                .err()
                .map(|why| (RejectKind::BadAnswer, Some(why))),
            _ => Some((RejectKind::NoQuestion, None)),
        };
        if let Some((kind, why)) = problem {
            let event = AgentEvent::Rejected {
                reason: answer_refusal(&kind, &call_id, why.as_deref()),
                id: None,
                kind: Some(kind),
                request: None,
            };
            self.emit(events, event).await;
            return;
        }
        self.close_question(QuestionEnd::Answered { answers }, events)
            .await;
    }

    pub(super) async fn close_question(
        &mut self,
        how: QuestionEnd,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(open) = self.question.take() else {
            return;
        };
        let reply = match &how {
            QuestionEnd::Answered { answers } => Some(AskReply::Answered {
                answers: answers.clone(),
            }),
            QuestionEnd::MessageInstead => Some(AskReply::MessageInstead),
            QuestionEnd::Stopped | QuestionEnd::Interrupted | QuestionEnd::Withdrawn => None,
        };
        let event = AgentEvent::QuestionClosed {
            call_id: open.call_id,
            how,
        };
        self.emit(events, event).await;
        if let Some(reply) = reply {
            let _ = open.reply.send(reply);
        }
    }
}
