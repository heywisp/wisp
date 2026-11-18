use wisp_protocol::{ErrorKind, IdleOutcome, ReportOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Completed,
    Cancelled,
    Failed(ErrorKind),
    InvalidReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    User,
    WithParent,
    ByParent,
    ByAncestor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndFacts {
    pub ended: Ended,
    pub owes: bool,
    pub reported: bool,
    pub waiting: Option<IdleOutcome>,
    pub stop: Option<StopCause>,
    pub closing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToParent {
    Report(ReportOutcome),
    Failed(ErrorKind),
    Stopped(StopCause),
}

impl ToParent {
    pub fn wakes(self) -> bool {
        !matches!(self, Self::Stopped(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndDecision {
    pub mail: Option<ToParent>,
    pub outcome: Option<IdleOutcome>,
    pub owes: bool,
}

pub fn decide(facts: &EndFacts) -> EndDecision {
    let keep = |mail, outcome| EndDecision {
        mail,
        outcome,
        owes: facts.owes,
    };
    let settle = |outcome: ReportOutcome| EndDecision {
        mail: Some(ToParent::Report(outcome)),
        outcome: Some(outcome.idle()),
        owes: false,
    };
    if facts.closing {
        return keep(None, None);
    }
    if facts.reported {
        return settle(ReportOutcome::Reported);
    }
    match facts.ended {
        Ended::InvalidReport => settle(ReportOutcome::InvalidReport),
        Ended::Cancelled => {
            let cause = facts.stop.unwrap_or(StopCause::User);
            let mail = (cause != StopCause::ByParent).then_some(ToParent::Stopped(cause));
            keep(mail, Some(IdleOutcome::Stopped))
        }
        Ended::Failed(kind) => keep(
            facts.owes.then_some(ToParent::Failed(kind)),
            Some(IdleOutcome::Failed),
        ),
        Ended::Completed if !facts.owes => keep(None, None),
        Ended::Completed => match facts.waiting {
            Some(waiting) => keep(None, Some(waiting)),
            None => settle(ReportOutcome::NoReport),
        },
    }
}

#[cfg(test)]
mod tests;
