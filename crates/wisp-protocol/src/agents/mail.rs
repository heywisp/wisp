use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::TextSpan;

pub const ROOT_PATH: &str = "/";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentPath(pub String);

impl AgentPath {
    pub fn root() -> Self {
        Self(ROOT_PATH.to_owned())
    }

    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0 == ROOT_PATH
    }

    pub fn parent(&self) -> Option<AgentPath> {
        if self.is_root() {
            return None;
        }
        match self.0.rsplit_once('/') {
            Some(("", _)) => Some(Self::root()),
            Some((parent, _)) => Some(Self(parent.to_owned())),
            None => None,
        }
    }

    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or_default()
    }

    pub fn join(&self, name: &str) -> AgentPath {
        if self.is_root() {
            Self(format!("/{name}"))
        } else {
            Self(format!("{}/{name}", self.0))
        }
    }

    pub fn depth(&self) -> u32 {
        if self.is_root() {
            return 0;
        }
        u32::try_from(self.0.matches('/').count()).unwrap_or(u32::MAX)
    }

    pub fn tree_cmp(&self, other: &AgentPath) -> Ordering {
        self.0.split('/').cmp(other.0.split('/'))
    }

    pub fn is_ancestor_of(&self, other: &AgentPath) -> bool {
        if self.is_root() {
            return !other.is_root() && other.0.starts_with('/');
        }
        other
            .0
            .strip_prefix(self.0.as_str())
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
    }

    pub fn moved(&self, from: &AgentPath, to: &AgentPath) -> Option<AgentPath> {
        if self == from {
            return Some(to.clone());
        }
        let rest = self.0.strip_prefix(from.0.as_str())?;
        (!from.is_root() && rest.starts_with('/')).then(|| Self(format!("{}{rest}", to.0)))
    }

    pub fn renamed(self, from: &AgentPath, to: &AgentPath) -> AgentPath {
        self.moved(from, to).unwrap_or(self)
    }

    pub fn followed(&self, renames: &[(AgentPath, AgentPath)]) -> AgentPath {
        renames
            .iter()
            .fold(self.clone(), |path, (from, to)| path.renamed(from, to))
    }
}

impl Default for AgentPath {
    fn default() -> Self {
        Self::root()
    }
}

impl fmt::Display for AgentPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MailId(pub String);

impl MailId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MailId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailKind {
    Task,
    Message,
    Report,
    Notice,
    Question,
    #[serde(other)]
    Unknown,
}

impl MailKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Message => "message",
            Self::Report => "report",
            Self::Notice => "notice",
            Self::Question => "question",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_kernel(self) -> bool {
        matches!(self, Self::Report | Self::Notice)
    }
}

impl fmt::Display for MailKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailFrom {
    pub agent: AgentPath,
    pub kind: MailKind,
    pub mail: MailId,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub wake: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
}

pub const MAIL_OPEN: &str = "<wisp:message";
const MAIL_CLOSE: &str = "</wisp:message>";

pub fn render_mail(from: &AgentPath, kind: MailKind, text: &str) -> String {
    render_mail_spans(from, kind, text, &[]).0
}

pub fn shift_spans(spans: &[TextSpan], offset: usize) -> Option<Vec<TextSpan>> {
    spans
        .iter()
        .map(|span| {
            Some(TextSpan {
                start: span.start.checked_add(offset)?,
                end: span.end.checked_add(offset)?,
            })
        })
        .collect()
}

pub fn render_mail_spans(
    from: &AgentPath,
    kind: MailKind,
    text: &str,
    spans: &[TextSpan],
) -> (String, Option<Vec<TextSpan>>) {
    let mut rendered = format!("{MAIL_OPEN} from=\"{from}\" kind=\"{kind}\">\n");
    let offset = rendered.len();
    rendered.push_str(text);
    rendered.push('\n');
    rendered.push_str(MAIL_CLOSE);
    (rendered, shift_spans(spans, offset))
}

pub fn mail_body(text: &str) -> &str {
    let (mut start, mut end, mut at) = (None, None, 0);
    for line in text.split_inclusive('\n') {
        let next = at + line.len();
        match start {
            None if line.starts_with(MAIL_OPEN) && line.ends_with('\n') => start = Some(next),
            Some(_) if line.trim() == MAIL_CLOSE => end = Some(at),
            _ => {}
        }
        at = next;
    }
    let body = match (start, end) {
        (Some(start), Some(end)) => text
            .get(start..end)
            .map(|body| body.strip_suffix('\n').unwrap_or(body)),
        (Some(start), None) => text.get(start..),
        (None, _) => None,
    };
    body.unwrap_or(text)
}

pub fn report_text(head: &str, text: &str) -> String {
    format!("{}\n{text}", head.replace('\n', " "))
}

pub fn report_parts(text: &str) -> (&str, &str) {
    text.split_once('\n').unwrap_or((text, ""))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WakeSource {
    Mail {
        mail: MailId,
        from: AgentPath,
    },
    Process {
        id: u32,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    Stopped,
    HeldMessages,
    RetryOffer,
    Concurrency,
    #[serde(other)]
    Unknown,
}

impl HoldReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::HeldMessages => "held_messages",
            Self::RetryOffer => "retry_offer",
            Self::Concurrency => "concurrency",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAction {
    Spawn,
    Send,
    Stop,
    Report,
    Rename,
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests;
