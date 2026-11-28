use super::*;

fn states() -> Vec<AgentState> {
    vec![
        AgentState::Queued {
            reason: QueueReason::Concurrency,
        },
        AgentState::Queued {
            reason: QueueReason::Stagger,
        },
        AgentState::Queued {
            reason: QueueReason::Endpoint,
        },
        AgentState::Queued {
            reason: QueueReason::Retry,
        },
        AgentState::Running,
        AgentState::Stopping,
        AgentState::Idle { outcome: None },
        AgentState::Idle {
            outcome: Some(IdleOutcome::Asked),
        },
        AgentState::Held {
            reason: HoldReason::Stopped,
        },
        AgentState::Ended,
    ]
}

fn changes() -> Vec<Change> {
    vec![
        Change::Spawned,
        Change::Queued(QueueReason::Concurrency),
        Change::Queued(QueueReason::Stagger),
        Change::Queued(QueueReason::Endpoint),
        Change::Queued(QueueReason::Retry),
        Change::Started,
        Change::Stopping,
        Change::Settled(None),
        Change::Settled(Some(IdleOutcome::Reported)),
        Change::Held(HoldReason::Concurrency),
        Change::Ended,
    ]
}

fn expected(state: AgentState, change: Change) -> AgentState {
    if state == AgentState::Ended || change == Change::Ended {
        return AgentState::Ended;
    }
    let stopping = state == AgentState::Stopping;
    match change {
        Change::Started
        | Change::Held(_)
        | Change::Queued(QueueReason::Stagger | QueueReason::Endpoint | QueueReason::Retry)
            if stopping =>
        {
            AgentState::Stopping
        }
        Change::Stopping if state == AgentState::Running => AgentState::Stopping,
        Change::Stopping => state,
        Change::Spawned | Change::Started => AgentState::Running,
        Change::Queued(reason) => AgentState::Queued { reason },
        Change::Held(_) if state == AgentState::Running => AgentState::Running,
        Change::Held(reason) => AgentState::Held { reason },
        Change::Settled(outcome) => match state {
            AgentState::Idle { outcome: before } => AgentState::Idle {
                outcome: outcome.or(before),
            },
            _ => AgentState::Idle { outcome },
        },
        Change::Ended => AgentState::Ended,
    }
}

#[test]
fn every_state_and_change_has_one_next_state() {
    for state in states() {
        for change in changes() {
            assert_eq!(
                next(state, change),
                expected(state, change),
                "{state:?} + {change:?}"
            );
        }
    }
}

#[test]
fn the_documented_paths_hold() {
    let spawned = next(AgentState::Idle { outcome: None }, Change::Spawned);
    assert_eq!(spawned, AgentState::Running);
    let idle = next(spawned, Change::Settled(Some(IdleOutcome::Reported)));
    assert_eq!(
        idle,
        AgentState::Idle {
            outcome: Some(IdleOutcome::Reported)
        }
    );
    let held = next(idle, Change::Held(HoldReason::Stopped));
    assert_eq!(
        held,
        AgentState::Held {
            reason: HoldReason::Stopped
        }
    );
    let woken = next(held, Change::Started);
    assert_eq!(woken, AgentState::Running);
    assert_eq!(next(woken, Change::Held(HoldReason::Stopped)), woken);
    assert_eq!(next(woken, Change::Ended), AgentState::Ended);
    assert_eq!(next(AgentState::Ended, Change::Started), AgentState::Ended);
    let quiet = next(idle, Change::Settled(None));
    assert_eq!(quiet, idle);
}

#[test]
fn a_stop_reaching_a_running_turn_reads_stopping_until_the_turn_settles() {
    let stopping = next(AgentState::Running, Change::Stopping);
    assert_eq!(stopping, AgentState::Stopping);
    assert_eq!(next(stopping, Change::Started), stopping);
    assert_eq!(next(stopping, Change::Held(HoldReason::Stopped)), stopping);
    assert_eq!(next(stopping, Change::Stopping), stopping);
    assert_eq!(
        next(stopping, Change::Queued(QueueReason::Stagger)),
        stopping
    );
    let stopped = Some(IdleOutcome::Stopped);
    assert_eq!(
        next(stopping, Change::Settled(stopped)),
        AgentState::Idle { outcome: stopped }
    );
    assert_eq!(next(stopping, Change::Ended), AgentState::Ended);
    let queued = AgentState::Queued {
        reason: QueueReason::Concurrency,
    };
    assert_eq!(next(queued, Change::Stopping), queued);
    assert_eq!(activity_of(stopping), None);
    assert!(stopping.is_running() && stopping.is_busy() && stopping.is_live());
}

#[test]
fn activity_follows_the_childs_events() {
    let mut watch = Watch::default();
    assert_eq!(
        watch.activity(&AgentEvent::StepStarted { step: 1 }),
        Some(Activity::Streaming)
    );
    assert_eq!(
        watch.activity(&AgentEvent::TextDelta {
            index: 0,
            text: "x".into()
        }),
        None
    );
    let requested = requested("c1", None, "");
    assert_eq!(watch.activity(&requested), None);
    let started = AgentEvent::ToolCallStarted {
        call_id: "c1".into(),
        cwd: String::new(),
        movable: false,
        file: None,
    };
    assert_eq!(
        watch.activity(&started),
        Some(Activity::Tool {
            name: "bash".into()
        })
    );
    assert_eq!(activity_of(AgentState::Running), None);
    assert_eq!(
        activity_of(AgentState::Held {
            reason: HoldReason::RetryOffer
        }),
        Some(Activity::Held {
            reason: HoldReason::RetryOffer
        })
    );
}

fn requested(id: &str, title: Option<&str>, target: &str) -> AgentEvent {
    AgentEvent::ToolCallRequested {
        step: 1,
        block: 0,
        call_id: id.into(),
        tool: "bash".into(),
        input: serde_json::json!({}),
        display: wisp_protocol::ToolDisplay {
            action: wisp_protocol::ToolAction::Other {
                name: "bash".into(),
            },
            verbs: None,
            target: target.into(),
            output: wisp_protocol::OutputKind::Text,
            view: None,
            title: title.map(str::to_owned),
            purpose: wisp_protocol::ToolPurpose::Run,
        },
        refused: false,
    }
}

#[tokio::test(start_paused = true)]
async fn unfinished_calls_keep_their_own_description_target_and_start() {
    let mut watch = Watch::default();
    watch.activity(&AgentEvent::turn_started(1));
    watch.activity(&requested("first", Some("Run the checks"), "cargo check"));
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    watch.activity(&requested("second", None, "file.txt"));
    let rows = watch.unfinished();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].title.as_deref(), Some("Run the checks"));
    assert_eq!(rows[0].target, "cargo check");
    assert_eq!(rows[1].target, "file.txt");
    assert!(rows[0].started < rows[1].started);
    assert!(
        rows.iter()
            .all(|r| r.what == Unfinished::ToolCall && !r.opening)
    );
    watch.activity(&AgentEvent::call_finished(
        "first",
        wisp_protocol::ToolOutcome::Cancelled,
    ));
    assert_eq!(watch.unfinished(), rows[1..]);
}

#[test]
fn questions_and_parks_are_ready_and_only_a_users_first_reply_is_opening() {
    let mut watch = Watch::default();
    watch.activity(&AgentEvent::turn_started(1));
    assert!(watch.unfinished()[0].opening);
    watch.activity(&AgentEvent::QuestionAsked {
        call_id: "ask".into(),
        questions: Vec::new(),
    });
    assert!(watch.unfinished().is_empty());
    watch.activity(&AgentEvent::QuestionClosed {
        call_id: "ask".into(),
        how: wisp_protocol::QuestionEnd::Answered {
            answers: Vec::new(),
        },
    });
    assert!(watch.unfinished()[0].opening);
    watch.activity(&AgentEvent::StepEnded {
        step: 1,
        usage: None,
        cost_usd: None,
        lower_bound: false,
    });
    watch.activity(&AgentEvent::ReadyToRestart);
    assert!(watch.unfinished().is_empty());
    watch.activity(&AgentEvent::StepStarted { step: 2 });
    assert!(!watch.unfinished()[0].opening);
    let mut mail = AgentEvent::turn_started(2);
    if let AgentEvent::TurnStarted { origin, .. } = &mut mail {
        *origin = Some(wisp_protocol::TurnOrigin::Mail);
    }
    watch.activity(&mail);
    assert!(!watch.unfinished()[0].opening);
}

#[test]
fn a_numbered_process_keeps_its_starting_calls_description_until_it_ends() {
    let mut watch = Watch::default();
    watch.activity(&requested("start", Some("Run the checks"), "cargo check"));
    let mut finished = AgentEvent::call_finished("start", wisp_protocol::ToolOutcome::Cancelled);
    if let AgentEvent::ToolCallFinished { process, .. } = &mut finished {
        *process = Some(7);
    }
    watch.activity(&finished);
    watch.activity(&requested("poll", Some("Check the output"), "7"));
    let mut poll = AgentEvent::call_finished("poll", wisp_protocol::ToolOutcome::Cancelled);
    if let AgentEvent::ToolCallFinished { process, .. } = &mut poll {
        *process = Some(7);
    }
    watch.activity(&poll);
    assert_eq!(watch.process_title(7).as_deref(), Some("Run the checks"));
    assert!(watch.unfinished().is_empty());
    watch.activity(&AgentEvent::ProcessEnded {
        id: 7,
        end: wisp_protocol::ProcessEnd::Stopped {
            by: wisp_protocol::StoppedBy::User,
        },
        bytes: 0,
        lines: 0,
        duration_ms: 0,
    });
    assert_eq!(watch.process_title(7), None);
}

#[test]
fn preparation_ignores_old_lifecycle_events_and_a_park_acknowledges_before_resuming() {
    let mut watch = Watch::default();
    watch.preparing(2, None);
    watch.activity(&AgentEvent::turn_started(1));
    watch.activity(&AgentEvent::StepEnded {
        step: 9,
        usage: None,
        cost_usd: None,
        lower_bound: false,
    });
    assert!(watch.unfinished()[0].opening);
    watch.activity(&AgentEvent::turn_started(2));
    let mut parked = watch.parking();
    watch.activity(&AgentEvent::ReadyToRestart);
    assert_eq!(parked.try_recv(), Ok(()));
    assert!(watch.unfinished().is_empty());
    watch.requesting();
    assert!(watch.unfinished()[0].opening);
    watch.preparing(3, Some(TurnOrigin::Mail));
    assert!(!watch.unfinished()[0].opening);
    watch.activity(&AgentEvent::QuestionAsked {
        call_id: "preparing".into(),
        questions: Vec::new(),
    });
    assert!(watch.unfinished().is_empty());
    watch.activity(&AgentEvent::QuestionClosed {
        call_id: "preparing".into(),
        how: wisp_protocol::QuestionEnd::Stopped,
    });
    watch.prepared(3);
    assert!(watch.unfinished().is_empty());
}

#[test]
fn foreground_compaction_is_a_reply_but_background_preparation_changes_no_reply_row() {
    let mut watch = Watch::default();
    let started = |trigger| AgentEvent::CompactionStarted {
        trigger,
        turn: None,
        switch: None,
        requested_by: None,
    };
    watch.activity(&started(CompactionTrigger::Background));
    assert!(watch.unfinished().is_empty());
    watch.activity(&started(CompactionTrigger::Manual));
    let reply = watch.unfinished();
    assert_eq!(reply.len(), 1);
    assert_eq!(reply[0].what, Unfinished::Reply);
    assert!(!reply[0].opening);
    watch.activity(&started(CompactionTrigger::Background));
    watch.activity(&AgentEvent::CompactionPrepared {
        covers: None,
        usage: None,
        cost: None,
        turn: None,
    });
    assert_eq!(watch.unfinished(), reply);
    let mut failed = AgentEvent::CompactionFailed {
        trigger: CompactionTrigger::Manual,
        engine: None,
        kind: wisp_protocol::CompactionFailure::Cancelled,
        message: "stopped".into(),
        usage: None,
        cost: None,
        answers: None,
        turn: None,
        paused: false,
        requested_by: None,
    };
    watch.activity(&failed);
    assert!(watch.unfinished().is_empty());
    watch.requesting();
    if let AgentEvent::CompactionFailed { turn, .. } = &mut failed {
        *turn = Some(1);
    }
    watch.activity(&failed);
    assert_eq!(
        watch.unfinished().len(),
        1,
        "an in-turn resend still has a reply in flight"
    );
}

#[test]
fn a_finished_calls_question_stops_hiding_unfinished_work() {
    let mut watch = Watch::default();
    watch.activity(&AgentEvent::turn_started(1));
    watch.activity(&requested("q", None, "Plan"));
    watch.activity(&AgentEvent::QuestionAsked {
        call_id: "q".into(),
        questions: Vec::new(),
    });
    assert!(watch.unfinished().is_empty());
    watch.activity(&AgentEvent::ToolCallFinished {
        call_id: "q".into(),
        outcome: wisp_protocol::ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        output: wisp_protocol::ToolOutputView::default(),
        facts: Vec::new(),
        state: None,
        duration_ms: 1,
        result: None,
        process: None,
        attachments: Vec::new(),
        notes: Vec::new(),
        typed: None,
    });
    watch.activity(&AgentEvent::StepStarted { step: 2 });
    assert_eq!(watch.unfinished().len(), 1);
    assert_eq!(watch.unfinished()[0].what, Unfinished::Reply);
}
