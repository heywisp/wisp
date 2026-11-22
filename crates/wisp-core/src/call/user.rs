use tokio::sync::{mpsc, oneshot};
use wisp_plugin_api::questions::Posted;
use wisp_plugin_api::types as api;
use wisp_protocol::{Answer, Ends, ImageData, Question, Secret, SecretDestination};

#[derive(Debug, Clone, Default)]
pub struct UserChannel {
    asks: Option<mpsc::Sender<AskRequest>>,
}

#[derive(Debug, Clone)]
pub enum Goes {
    Process(u32),
    Server(SecretDestination),
}

#[derive(Debug)]
pub enum AskRequest {
    Questions {
        questions: Vec<Question>,
        reply: oneshot::Sender<AskReply>,
    },
    Attach {
        id: u32,
        reason: String,
        reply: oneshot::Sender<AskReply>,
    },
    Secret {
        goes: Goes,
        asked_by: Option<wisp_protocol::AskedBy>,
        name: String,
        reason: String,
        show: Ends,
        reply: oneshot::Sender<SecretReply>,
    },
    Report {
        reply: oneshot::Sender<ScreenReport>,
    },
    Post {
        questions: Vec<Question>,
        reply: oneshot::Sender<Posted>,
    },
    Withdraw {
        reply: oneshot::Sender<bool>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenReport {
    pub report: serde_json::Value,
    pub images: Vec<ImageData>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SecretReply {
    Entered(Secret),
    ProcessEnded,
    Busy,
    Unavailable,
}

impl AskRequest {
    pub fn unavailable(self) {
        match self {
            Self::Questions { reply, .. } | Self::Attach { reply, .. } => {
                let _ = reply.send(AskReply::Unavailable);
            }
            Self::Secret { reply, .. } => {
                let _ = reply.send(SecretReply::Unavailable);
            }
            Self::Report { .. } => {}
            Self::Post { reply, .. } => {
                let _ = reply.send(Posted::Unavailable);
            }
            Self::Withdraw { reply } => {
                let _ = reply.send(false);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskReply {
    Answered { answers: Vec<Answer> },
    Attached { typed: bool },
    Declined,
    ProcessEnded { typed: bool },
    MessageInstead,
    Unavailable,
}

impl From<AskReply> for api::AskReply {
    fn from(reply: AskReply) -> Self {
        match reply {
            AskReply::Answered { answers } => Self::Answered(
                answers
                    .into_iter()
                    .map(|answer| api::Answer {
                        chosen: answer.chosen,
                        own: answer.own,
                    })
                    .collect(),
            ),
            AskReply::Attached { typed } => Self::Attached(typed),
            AskReply::Declined => Self::Declined,
            AskReply::ProcessEnded { typed } => Self::ProcessEnded(typed),
            AskReply::MessageInstead => Self::MessageInstead,
            AskReply::Unavailable => Self::Unavailable,
        }
    }
}

impl UserChannel {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn new(asks: mpsc::Sender<AskRequest>) -> Self {
        Self { asks: Some(asks) }
    }

    pub fn is_available(&self) -> bool {
        self.asks.is_some()
    }

    pub async fn ask(&self, questions: Vec<Question>) -> AskReply {
        self.request(
            |reply| AskRequest::Questions { questions, reply },
            AskReply::Unavailable,
        )
        .await
    }

    pub async fn post(&self, questions: Vec<Question>) -> Posted {
        self.request(
            |reply| AskRequest::Post { questions, reply },
            Posted::Unavailable,
        )
        .await
    }

    pub async fn withdraw(&self) -> bool {
        self.request(|reply| AskRequest::Withdraw { reply }, false)
            .await
    }

    pub async fn attach(&self, id: u32, reason: String) -> AskReply {
        self.request(
            |reply| AskRequest::Attach { id, reason, reply },
            AskReply::Unavailable,
        )
        .await
    }

    pub async fn secret(
        &self,
        goes: Goes,
        asked_by: Option<wisp_protocol::AskedBy>,
        name: String,
        reason: String,
        show: Ends,
    ) -> SecretReply {
        self.request(
            |reply| AskRequest::Secret {
                goes,
                asked_by,
                name,
                reason,
                show,
                reply,
            },
            SecretReply::Unavailable,
        )
        .await
    }

    pub async fn report(&self) -> Option<ScreenReport> {
        let asks = self.asks.as_ref()?;
        let (reply, report) = oneshot::channel();
        asks.send(AskRequest::Report { reply }).await.ok()?;
        report.await.ok()
    }

    async fn request<R>(
        &self,
        build: impl FnOnce(oneshot::Sender<R>) -> AskRequest,
        unavailable: R,
    ) -> R {
        let Some(asks) = &self.asks else {
            return unavailable;
        };
        let (reply, answer) = oneshot::channel();
        if asks.send(build(reply)).await.is_err() {
            return unavailable;
        }
        answer.await.unwrap_or(unavailable)
    }
}

#[cfg(test)]
mod tests;
