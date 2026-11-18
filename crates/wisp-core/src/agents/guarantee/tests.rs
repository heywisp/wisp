use super::*;

fn facts(ended: Ended) -> EndFacts {
    EndFacts {
        ended,
        owes: true,
        reported: false,
        waiting: None,
        stop: None,
        closing: false,
    }
}

fn mail(f: EndFacts) -> Option<ToParent> {
    decide(&f).mail
}

#[test]
fn each_row_of_the_guarantee_table_sends_its_mail() {
    let reported = EndFacts {
        reported: true,
        ..facts(Ended::Completed)
    };
    assert_eq!(
        decide(&reported),
        EndDecision {
            mail: Some(ToParent::Report(ReportOutcome::Reported)),
            outcome: Some(IdleOutcome::Reported),
            owes: false,
        }
    );
    assert_eq!(
        decide(&facts(Ended::Completed)),
        EndDecision {
            mail: Some(ToParent::Report(ReportOutcome::NoReport)),
            outcome: Some(IdleOutcome::NoReport),
            owes: false,
        }
    );
    assert_eq!(
        mail(facts(Ended::InvalidReport)),
        Some(ToParent::Report(ReportOutcome::InvalidReport))
    );
    assert_eq!(
        decide(&facts(Ended::Failed(ErrorKind::Overloaded))),
        EndDecision {
            mail: Some(ToParent::Failed(ErrorKind::Overloaded)),
            outcome: Some(IdleOutcome::Failed),
            owes: true,
        }
    );
    assert_eq!(
        mail(facts(Ended::Cancelled)),
        Some(ToParent::Stopped(StopCause::User))
    );
    let cascade = EndFacts {
        stop: Some(StopCause::WithParent),
        ..facts(Ended::Cancelled)
    };
    assert_eq!(
        mail(cascade),
        Some(ToParent::Stopped(StopCause::WithParent))
    );
    let by_parent = EndFacts {
        stop: Some(StopCause::ByParent),
        ..facts(Ended::Cancelled)
    };
    assert_eq!(decide(&by_parent).mail, None);
    assert_eq!(decide(&by_parent).outcome, Some(IdleOutcome::Stopped));
}

#[test]
fn a_waiting_turn_sends_nothing_and_keeps_owing() {
    for waiting in [IdleOutcome::Asked, IdleOutcome::Waiting] {
        let f = EndFacts {
            waiting: Some(waiting),
            ..facts(Ended::Completed)
        };
        assert_eq!(
            decide(&f),
            EndDecision {
                mail: None,
                outcome: Some(waiting),
                owes: true,
            }
        );
    }
}

#[test]
fn a_turn_that_owes_nothing_or_ends_with_the_tree_sends_nothing() {
    let free = EndFacts {
        owes: false,
        ..facts(Ended::Completed)
    };
    assert_eq!(decide(&free).mail, None);
    let failed_free = EndFacts {
        owes: false,
        ..facts(Ended::Failed(ErrorKind::Network))
    };
    assert_eq!(decide(&failed_free).mail, None);
    for ended in [
        Ended::Completed,
        Ended::Cancelled,
        Ended::Failed(ErrorKind::Server),
        Ended::InvalidReport,
    ] {
        let closing = EndFacts {
            closing: true,
            reported: true,
            ..facts(ended)
        };
        assert_eq!(decide(&closing).mail, None, "{ended:?}");
    }
}

#[test]
fn only_stop_notices_leave_the_parent_asleep() {
    assert!(ToParent::Report(ReportOutcome::NoReport).wakes());
    assert!(ToParent::Failed(ErrorKind::Other).wakes());
    assert!(!ToParent::Stopped(StopCause::User).wakes());
}

#[test]
fn every_ending_turn_that_owes_sends_at_most_one_mail() {
    let endings = [
        Ended::Completed,
        Ended::Cancelled,
        Ended::Failed(ErrorKind::Server),
        Ended::InvalidReport,
    ];
    let waits = [None, Some(IdleOutcome::Asked), Some(IdleOutcome::Waiting)];
    let stops = [
        None,
        Some(StopCause::User),
        Some(StopCause::WithParent),
        Some(StopCause::ByParent),
    ];
    for ended in endings {
        for reported in [false, true] {
            for waiting in waits {
                for stop in stops {
                    let f = EndFacts {
                        ended,
                        owes: true,
                        reported,
                        waiting,
                        stop,
                        closing: false,
                    };
                    let d = decide(&f);
                    let silent = waiting.is_some() && ended == Ended::Completed && !reported
                        || stop == Some(StopCause::ByParent)
                            && ended == Ended::Cancelled
                            && !reported;
                    assert_eq!(d.mail.is_none(), silent, "{f:?} -> {d:?}");
                }
            }
        }
    }
}
