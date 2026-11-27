use tokio::sync::{mpsc, oneshot};
use wisp_plugin_api::questions as api_questions;
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::{
    AgentEvent, Answer, AttachRequestEnd, ImageData, InputId, MailFrom, MailId, MailKind, Question,
    QuestionEnd, RejectKind, Secret, SecretRequestEnd, answer_refusal, check_answers, check_secret,
    report_refusal, secret_refusal,
};

use super::Agent;
use crate::call::user::{Goes, ScreenReport};
use crate::tool::{AskReply, AskRequest, SecretReply};

const QUESTION_LETTER_PREFIX: &str = "wisp-question-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PostedState {
    Open,
    Ending(Option<InputId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Posted {
    pub(crate) call_id: String,
    pub(crate) questions: Vec<Question>,
    pub(crate) state: PostedState,
}

impl Posted {
    pub(crate) fn is_open(&self) -> bool {
        self.state == PostedState::Open
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PostedFold {
    asking: Option<(String, Vec<Question>)>,
    posted: Option<Posted>,
    opening: Option<(u64, InputId)>,
}

impl PostedFold {
    pub(crate) fn see(&mut self, event: &LogEvent) {
        match event {
            LogEvent::QuestionAsked {
                call_id, questions, ..
            } => {
                if let Some(posted) = &mut self.posted
                    && posted.call_id == *call_id
                {
                    posted.questions.clone_from(questions);
                    posted.state = PostedState::Open;
                    self.opening = None;
                } else {
                    self.asking = Some((call_id.clone(), questions.clone()));
                }
            }
            LogEvent::ToolCallFinished { call_id, .. } => {
                if let Some((id, questions)) = self.asking.take_if(|(id, _)| id == call_id) {
                    self.posted = Some(Posted {
                        call_id: id,
                        questions,
                        state: PostedState::Open,
                    });
                    self.opening = None;
                }
            }
            LogEvent::QuestionClosed { call_id, how, .. } => {
                if self.asking.take_if(|(id, _)| id == call_id).is_some() {
                    return;
                }
                if let Some(posted) = &mut self.posted
                    && posted.call_id == *call_id
                {
                    self.opening = None;
                    match how {
                        QuestionEnd::Answered { .. } | QuestionEnd::Stopped => {
                            posted.state = PostedState::Ending(None);
                        }
                        QuestionEnd::Withdrawn
                        | QuestionEnd::MessageInstead
                        | QuestionEnd::Interrupted => {
                            self.posted = None;
                        }
                    }
                }
            }
            LogEvent::InputAccepted {
                id,
                from: Some(from),
                ..
            } if from.kind == MailKind::Question => {
                if let Some(posted) = &mut self.posted
                    && posted.state == PostedState::Ending(None)
                {
                    posted.state = PostedState::Ending(Some(id.clone()));
                }
            }
            LogEvent::TurnStarted { turn, inputs, .. } => {
                self.opening = self.posted.as_ref().and_then(|posted| {
                    let PostedState::Ending(Some(letter)) = &posted.state else {
                        return None;
                    };
                    inputs
                        .iter()
                        .any(|i| &i.id == letter)
                        .then(|| (*turn, letter.clone()))
                });
            }
            LogEvent::MessageCommitted { turn, .. } => {
                if let Some((_, letter)) = self.opening.take_if(|(t, _)| t == turn)
                    && self.posted.as_ref().is_some_and(
                        |p| matches!(&p.state, PostedState::Ending(Some(id)) if *id == letter),
                    )
                {
                    self.posted = None;
                }
            }
            LogEvent::TurnEnded { .. } => {
                self.opening = None;
            }
            LogEvent::InputCommitted { inputs, .. } => {
                if let Some(posted) = &self.posted
                    && let PostedState::Ending(Some(letter)) = &posted.state
                    && inputs.iter().any(|i| &i.id == letter)
                {
                    self.posted = None;
                    self.opening = None;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn posted(&self) -> Option<Posted> {
        self.posted.clone()
    }
}

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
            AskRequest::Post { questions, reply } => {
                self.open_posted(call_id, questions, reply, events).await;
            }
            AskRequest::Withdraw { reply } => {
                self.withdraw_posted(reply, events).await;
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

    pub(super) async fn ask_logged(
        &mut self,
        turn: u64,
        call_id: &str,
        questions: Vec<Question>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = AgentEvent::QuestionAsked {
            call_id: call_id.to_owned(),
            questions,
        };
        let seq = self.logged_seq();
        self.log_event(events, &event, Some(turn)).await;
        self.sync_log(events).await;
        self.note_event(&event, seq);
        super::emit(events, event).await;
    }

    async fn open_question(
        &mut self,
        call_id: &str,
        questions: Vec<Question>,
        reply: oneshot::Sender<AskReply>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let turn = self.running_turn().unwrap_or(self.turns);
        self.ask_logged(turn, call_id, questions.clone(), events)
            .await;
        self.question = Some(OpenQuestion {
            call_id: call_id.to_owned(),
            questions,
            reply,
        });
    }

    async fn open_posted(
        &mut self,
        call_id: &str,
        questions: Vec<Question>,
        reply: oneshot::Sender<api_questions::Posted>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.posted.is_some() {
            let _ = reply.send(api_questions::Posted::Busy);
            return;
        }
        let turn = self.running_turn().unwrap_or(self.turns);
        self.ask_logged(turn, call_id, questions.clone(), events)
            .await;
        self.posted = Some(Posted {
            call_id: call_id.to_owned(),
            questions,
            state: PostedState::Open,
        });
        let _ = reply.send(api_questions::Posted::Open);
    }

    async fn withdraw_posted(
        &mut self,
        reply: oneshot::Sender<bool>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if !self.posted.as_ref().is_some_and(Posted::is_open) {
            let _ = reply.send(false);
            return;
        }
        self.close_posted(QuestionEnd::Withdrawn, events).await;
        let _ = reply.send(true);
    }

    pub(super) async fn answer_command(
        &mut self,
        call_id: String,
        answers: Vec<Answer>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let checked = if let Some(open) = self.question.as_ref().filter(|q| q.call_id == call_id) {
            check_answers(&open.questions, &answers)
                .map(|()| true)
                .map_err(|why| (RejectKind::BadAnswer, Some(why)))
        } else if let Some(posted) = self
            .posted
            .as_ref()
            .filter(|p| p.is_open() && p.call_id == call_id)
        {
            check_answers(&posted.questions, &answers)
                .map(|()| false)
                .map_err(|why| (RejectKind::BadAnswer, Some(why)))
        } else {
            Err((RejectKind::NoQuestion, None))
        };
        match checked {
            Err((kind, why)) => {
                let event = AgentEvent::Rejected {
                    reason: answer_refusal(&kind, &call_id, why.as_deref()),
                    id: None,
                    kind: Some(kind),
                    request: None,
                };
                self.emit(events, event).await;
            }
            Ok(true) => {
                self.close_question(QuestionEnd::Answered { answers }, events)
                    .await;
            }
            Ok(false) => {
                self.close_posted(QuestionEnd::Answered { answers }, events)
                    .await;
            }
        }
    }

    pub(super) async fn close_posted(
        &mut self,
        how: QuestionEnd,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.close_posted_unsynced(how, events).await {
            self.sync_log(events).await;
        }
    }

    async fn close_posted_unsynced(
        &mut self,
        how: QuestionEnd,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let Some(posted) = self.posted.as_ref().filter(|p| p.is_open()).cloned() else {
            return false;
        };
        let turn = self.running_turn().unwrap_or(self.turns);
        let event = AgentEvent::QuestionClosed {
            call_id: posted.call_id.clone(),
            how: how.clone(),
        };
        self.emit_unboxed(events, event, Some(turn)).await;
        let letter = match &how {
            QuestionEnd::Answered { answers } => Some((
                crate::tool::question::answered_text(&posted.questions, answers),
                true,
            )),
            QuestionEnd::Stopped => Some((crate::tool::question::CLOSED_NOTE.to_owned(), false)),
            QuestionEnd::Withdrawn | QuestionEnd::MessageInstead | QuestionEnd::Interrupted => None,
        };
        if let Some((text, wake)) = letter {
            let id = self
                .question_letter(&posted.call_id, text, wake, events)
                .await;
            self.posted = Some(Posted {
                state: PostedState::Ending(Some(id)),
                ..posted
            });
        } else {
            self.posted = None;
        }
        true
    }

    async fn question_letter(
        &mut self,
        call_id: &str,
        text: String,
        wake: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) -> InputId {
        let id = self.next_question_letter();
        let from = MailFrom {
            agent: self.path(),
            kind: MailKind::Question,
            mail: MailId(call_id.to_owned()),
            wake,
            plugin: None,
        };
        self.push_letter(id.clone(), from, text, Vec::new(), events)
            .await;
        id
    }

    pub(super) fn seed_question_letters(&mut self, records: &[LogRecord]) {
        let accepted = records.iter().filter_map(|r| match &r.event {
            LogEvent::InputAccepted { id, .. } => Some(id.as_str()),
            _ => None,
        });
        self.question_letters = self
            .question_letters
            .max(InputId::highest_numbered(QUESTION_LETTER_PREFIX, accepted));
    }

    fn next_question_letter(&mut self) -> InputId {
        self.question_letters += 1;
        InputId(format!("{QUESTION_LETTER_PREFIX}{}", self.question_letters))
    }

    pub(super) async fn stop_posted(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.close_posted(QuestionEnd::Stopped, events).await;
    }

    pub(super) fn posted_committed(&mut self, id: &InputId) {
        if self
            .posted
            .as_ref()
            .is_some_and(|p| matches!(&p.state, PostedState::Ending(Some(letter)) if letter == id))
        {
            self.posted = None;
        }
    }

    pub(super) fn posted_event(&self) -> Option<AgentEvent> {
        let posted = self.posted.as_ref().filter(|p| p.is_open())?;
        Some(AgentEvent::QuestionAsked {
            call_id: posted.call_id.clone(),
            questions: posted.questions.clone(),
        })
    }

    pub(super) async fn announce_posted(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(posted) = self.posted.clone() else {
            return;
        };
        match posted.state {
            PostedState::Open => {
                super::emit(
                    events,
                    AgentEvent::QuestionAsked {
                        call_id: posted.call_id,
                        questions: posted.questions,
                    },
                )
                .await;
            }
            PostedState::Ending(None) => {
                let turn = self.turns;
                self.ask_logged(turn, &posted.call_id, posted.questions.clone(), events)
                    .await;
                self.posted = Some(Posted {
                    state: PostedState::Open,
                    ..posted
                });
            }
            PostedState::Ending(Some(_)) => {}
        }
    }

    pub(super) async fn withdraw_dropped_posted(
        &mut self,
        target: Option<&Posted>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let same_open = matches!(
            (&self.posted, target),
            (Some(now), Some(want)) if now.is_open() && now.call_id == want.call_id
        );
        if same_open {
            return;
        }
        if self.posted.as_ref().is_some_and(Posted::is_open) {
            self.close_posted_unsynced(QuestionEnd::Withdrawn, events)
                .await;
        } else {
            self.posted = None;
        }
    }

    pub(super) async fn reopen_posted(
        &mut self,
        target: Option<Posted>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(want) = target else {
            return;
        };
        if self
            .posted
            .as_ref()
            .is_some_and(|now| now.is_open() && now.call_id == want.call_id)
        {
            return;
        }
        let turn = self.turns;
        self.ask_logged(turn, &want.call_id, want.questions.clone(), events)
            .await;
        self.posted = Some(Posted {
            state: PostedState::Open,
            ..want
        });
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
