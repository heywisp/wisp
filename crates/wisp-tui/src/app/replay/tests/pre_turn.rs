use wisp_protocol::settings::{Restored, Withdrawal};
use wisp_protocol::{SwitchCompaction, SwitchCompactionCause};

use super::*;
use crate::events::{Changed, settings_changed};

const OLD: &str = "claude-sonnet-4-6";
const NEW: &str = "claude-opus-5-5";

fn switch_started(turn: u64) -> AgentEvent {
    AgentEvent::CompactionStarted {
        trigger: CompactionTrigger::Switch,
        turn: Some(turn),
        switch: Some(SwitchCompaction {
            estimate_micros: 190_000,
            head_tokens: 15_000,
            cause: SwitchCompactionCause::Convert,
        }),
        requested_by: None,
    }
}

fn switch_failed(turn: u64, kind: CompactionFailure) -> AgentEvent {
    AgentEvent::CompactionFailed {
        trigger: CompactionTrigger::Switch,
        engine: Some(CompactionEngine::Native),
        kind,
        message: "it stopped".to_owned(),
        usage: None,
        cost: None,
        answers: None,
        turn: Some(turn),
        paused: false,
        requested_by: None,
    }
}

fn withdrawn(why: Withdrawal, effort: Effort) -> AgentEvent {
    settings_changed(Changed {
        endpoint: "claude-api".to_owned(),
        model: OLD.to_owned(),
        effort,
        withdrawn: Some(why),
        restored: Some(Restored {
            effort: Effort::High,
            cache_ttl: CacheTtl::OneHour,
        }),
        ..Changed::default()
    })
}

fn effort_change() -> AgentEvent {
    settings_changed(Changed {
        endpoint: "claude-api".to_owned(),
        model: OLD.to_owned(),
        effort: Effort::High,
        ..Changed::default()
    })
}

fn cancelled_turn() -> Vec<AgentEvent> {
    vec![
        switch_started(1),
        switch_failed(1, CompactionFailure::Cancelled),
        AgentEvent::turn_started(1),
        cancelled(),
    ]
}

fn on_old_pair() -> Settings {
    Settings {
        endpoint: "claude-api".to_owned(),
        model: OLD.to_owned(),
        effort: Effort::High,
        pending: Some(Pending {
            endpoint: "claude-api".to_owned(),
            model: NEW.to_owned(),
            effort: Effort::Low,
        }),
        ..Settings::default()
    }
}

fn effort_only(request: u64) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: ClientCommand::shortcut_changes(None, None, Some(Effort::Medium)),
        save: false,
    }
}

fn pair_of(event: &AgentEvent) -> Option<(String, Effort)> {
    match event {
        AgentEvent::SettingsChanged {
            model,
            effort,
            answers: Some(_),
            ..
        } => Some((model.clone(), *effort)),
        _ => None,
    }
}

#[test]
fn a_cancels_withdrawal_ends_its_turns_script_and_an_earlier_change_stays_in_the_preamble() {
    let withdrawal = withdrawn(Withdrawal::SwitchCompactionCancelled, Effort::Low);
    let mut events = vec![effort_change()];
    events.extend(cancelled_turn());
    events.push(withdrawal.clone());
    events.extend(stepped("two"));
    let script = Script::new(events);
    assert_eq!(script.preamble, [effort_change()]);
    let mut first = cancelled_turn();
    first.push(withdrawal);
    assert_eq!(script.turns, [first, stepped("two")]);
}

#[test]
fn a_failures_withdrawal_moves_with_the_pre_turn_run() {
    let withdrawal = withdrawn(Withdrawal::SwitchCompactionFailed, Effort::Low);
    let mut turn = vec![
        switch_started(1),
        switch_failed(1, CompactionFailure::Other("overloaded".to_owned())),
        withdrawal,
    ];
    turn.extend(stepped("one"));
    let mut events = vec![effort_change()];
    events.extend(turn.clone());
    let script = Script::new(events);
    assert_eq!(script.preamble, [effort_change()]);
    assert_eq!(script.turns, [turn]);
}

#[test]
fn a_withdrawal_not_after_a_turn_stays_where_it_was() {
    let withdrawal = withdrawn(Withdrawal::SwitchCompactionCancelled, Effort::Low);
    let script = Script::new(vec![withdrawal.clone()]);
    assert_eq!(script.preamble, [withdrawal]);
    assert!(script.turns.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_replayed_cancel_plays_its_withdrawal_and_later_answers_start_from_the_old_pair() {
    let withdrawal = withdrawn(Withdrawal::SwitchCompactionCancelled, Effort::Low);
    let mut events_in = cancelled_turn();
    events_in.push(withdrawal.clone());
    let script = Script::new(events_in).with_settings(on_old_pair());
    let (commands, mut events) = spawn(script, PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    let played = until(&mut events, |e| *e == withdrawal).await;
    assert_eq!(played.last(), Some(&withdrawal), "{played:?}");
    commands.send(effort_only(9)).await.unwrap();
    let answered = until(&mut events, |e| pair_of(e).is_some()).await;
    assert_eq!(
        answered.iter().find_map(pair_of),
        Some((OLD.to_owned(), Effort::Medium))
    );
}

#[tokio::test(start_paused = true)]
async fn a_replayed_failures_withdrawal_runs_the_turn_on_the_old_pair_with_the_pending_effort() {
    let mut events_in = vec![
        switch_started(1),
        switch_failed(1, CompactionFailure::Other("overloaded".to_owned())),
        withdrawn(Withdrawal::SwitchCompactionFailed, Effort::Low),
    ];
    events_in.extend(answer(400));
    let script = Script::new(events_in).with_settings(on_old_pair());
    let (commands, mut events) = spawn(script, PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TurnStarted { .. })).await;
    commands.send(effort_only(9)).await.unwrap();
    let answered = until(&mut events, |e| pair_of(e).is_some()).await;
    let Some(AgentEvent::SettingsChanged { model, cache, .. }) =
        answered.iter().find(|e| pair_of(e).is_some())
    else {
        panic!("{answered:?}");
    };
    assert_eq!(model, OLD);
    assert_eq!(*cache, CacheEffect::Kept);
}

#[tokio::test(start_paused = true)]
async fn esc_during_a_replayed_switch_compaction_ends_the_turn_and_withdraws_the_switch() {
    let mut events_in = vec![
        switch_started(1),
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Switch,
            engine: CompactionEngine::Native,
            before: 600_000,
            after: 15_000,
            partial: false,
            converted: false,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: None,
            cost: None,
            answers: None,
            turn: Some(1),
            summary: None,
            paused: false,
        },
    ];
    events_in.extend(stepped("one"));
    let script = Script::new(events_in).with_settings(on_old_pair());
    let (commands, mut events) = spawn(script, PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| {
        matches!(e, AgentEvent::CompactionStarted { .. })
    })
    .await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    let played = until(&mut events, |e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                withdrawn: Some(_),
                ..
            }
        )
    })
    .await;
    let kinds: Vec<&str> = played
        .iter()
        .map(|e| match e {
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                turn: Some(1),
                ..
            } => "failed",
            AgentEvent::TurnStarted { turn: 1, .. } => "started",
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            } => "ended",
            AgentEvent::SettingsChanged { .. } => "withdrawn",
            _ => "other",
        })
        .filter(|k| *k != "other")
        .collect();
    assert_eq!(kinds, ["failed", "started", "ended", "withdrawn"]);
    let Some(AgentEvent::SettingsChanged {
        model,
        effort,
        withdrawn: Some(Withdrawal::SwitchCompactionCancelled),
        restored: Some(restored),
        ..
    }) = played.last()
    else {
        panic!("{played:?}");
    };
    assert_eq!((model.as_str(), *effort), (OLD, Effort::Low));
    assert_eq!(restored.effort, Effort::High);
    assert!(
        !played
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{played:?}"
    );
}

#[test]
fn a_background_compaction_between_turns_ends_the_turn_before_it() {
    let started = AgentEvent::CompactionStarted {
        trigger: CompactionTrigger::Background,
        turn: None,
        switch: None,
        requested_by: None,
    };
    let prepared = AgentEvent::CompactionPrepared {
        covers: None,
        usage: None,
        cost: None,
        turn: None,
    };
    let mut events = vec![effort_change()];
    events.extend(stepped("one"));
    events.push(started.clone());
    events.push(prepared.clone());
    events.extend(stepped("two"));
    let script = Script::new(events);
    assert_eq!(script.preamble, [effort_change()]);
    let mut first = stepped("one");
    first.extend([started, prepared]);
    assert_eq!(script.turns, [first, stepped("two")]);
}

#[tokio::test(start_paused = true)]
async fn a_replayed_compaction_before_a_turn_holds_like_a_manual_one() {
    let mut events_in = vec![
        switch_started(1),
        switch_failed(1, CompactionFailure::Other("overloaded".to_owned())),
    ];
    events_in.extend(stepped("one"));
    let (commands, mut events) = spawn(Script::new(events_in), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| {
        matches!(e, AgentEvent::CompactionStarted { .. })
    })
    .await;
    let from = tokio::time::Instant::now();
    until(&mut events, |e| {
        matches!(e, AgentEvent::CompactionFailed { .. })
    })
    .await;
    assert!(from.elapsed() >= PACE.saturating_mul(COMPACT_HOLD));
}

fn compacted_by(trigger: CompactionTrigger) -> AgentEvent {
    AgentEvent::Compacted {
        trigger,
        engine: CompactionEngine::Wisp,
        before: 600_000,
        after: 15_000,
        partial: true,
        converted: trigger == CompactionTrigger::Convert,
        cache_effect: CacheEffect::Rebuilt {
            reason: RebuildReason::Compaction,
        },
        usage: None,
        cost: None,
        answers: None,
        turn: Some(1),
        summary: None,
        paused: false,
    }
}

fn shape(played: &[AgentEvent]) -> Vec<String> {
    played
        .iter()
        .filter_map(|e| match e {
            AgentEvent::CompactionStarted { trigger, .. } => Some(format!("started {trigger:?}")),
            AgentEvent::CompactionFailed { trigger, kind, .. } => {
                Some(format!("failed {trigger:?} {kind:?}"))
            }
            AgentEvent::Compacted { trigger, .. } => Some(format!("compacted {trigger:?}")),
            AgentEvent::TurnStarted { .. } => Some("turn started".to_owned()),
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            } => Some("cancelled".to_owned()),
            AgentEvent::TurnEnded { .. } => Some("ended".to_owned()),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn esc_during_a_replayed_convert_waits_for_it_and_then_stops_the_turn() {
    let mut events_in = vec![
        switch_started(1),
        switch_failed(1, CompactionFailure::Other("overloaded".to_owned())),
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Convert,
            turn: Some(1),
            switch: None,
            requested_by: None,
        },
        compacted_by(CompactionTrigger::Convert),
    ];
    events_in.extend(stepped("one"));
    let script = Script::new(events_in).with_settings(on_old_pair());
    let (commands, mut events) = spawn(script, PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    let mut played = until(&mut events, |e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Convert,
                ..
            }
        )
    })
    .await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    played.extend(until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await);
    assert_eq!(
        shape(&played),
        [
            "started Switch",
            "failed Switch Other(\"overloaded\")",
            "started Convert",
            "compacted Convert",
            "turn started",
            "cancelled",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn esc_after_a_replayed_compaction_ends_starts_the_turn_before_stopping_it() {
    let mut events_in = vec![switch_started(1), compacted_by(CompactionTrigger::Switch)];
    events_in.extend(stepped("one"));
    let script = Script::new(events_in).with_settings(on_old_pair());
    let (commands, mut events) = spawn(script, PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    let mut played = until(&mut events, |e| matches!(e, AgentEvent::Compacted { .. })).await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    played.extend(until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await);
    assert_eq!(
        shape(&played),
        [
            "started Switch",
            "compacted Switch",
            "turn started",
            "cancelled"
        ]
    );
}

async fn model_after(events_in: Vec<AgentEvent>, cancel_on_start: bool) -> Option<String> {
    let script = Script::new(events_in).with_settings(on_old_pair());
    let (commands, mut events) = spawn(script, PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    if cancel_on_start {
        until(&mut events, |e| {
            matches!(e, AgentEvent::CompactionStarted { .. })
        })
        .await;
        commands.send(ClientCommand::Cancel).await.unwrap();
    }
    until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    commands
        .send(ClientCommand::ContextReport {
            detail: wisp_protocol::context::ReportDetail::Summary,
            request: RequestId(5),
        })
        .await
        .unwrap();
    let got = until(&mut events, |e| matches!(e, AgentEvent::ContextReport(_))).await;
    got.iter().find_map(|e| match e {
        AgentEvent::ContextReport(r) => Some(r.model.clone()),
        _ => None,
    })
}

fn auto_started() -> AgentEvent {
    AgentEvent::CompactionStarted {
        trigger: CompactionTrigger::Auto,
        turn: Some(1),
        switch: None,
        requested_by: None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_recorded_cancelled_auto_compaction_keeps_the_pending_switch() {
    let auto_cancelled = AgentEvent::CompactionFailed {
        trigger: CompactionTrigger::Auto,
        engine: Some(CompactionEngine::Native),
        kind: CompactionFailure::Cancelled,
        message: "stopped".to_owned(),
        usage: None,
        cost: None,
        answers: None,
        turn: Some(1),
        paused: false,
        requested_by: None,
    };
    let events_in = vec![
        auto_started(),
        auto_cancelled,
        AgentEvent::turn_started(1),
        cancelled(),
    ];
    assert_eq!(model_after(events_in, false).await.as_deref(), Some(NEW));
}

#[tokio::test(start_paused = true)]
async fn esc_during_a_replayed_auto_compaction_still_starts_the_pending_switch() {
    let mut events_in = vec![auto_started(), compacted_by(CompactionTrigger::Auto)];
    events_in.extend(stepped("one"));
    assert_eq!(model_after(events_in, true).await.as_deref(), Some(NEW));
}

#[tokio::test(start_paused = true)]
async fn esc_during_a_replayed_switch_compaction_keeps_the_old_pair() {
    let mut events_in = vec![switch_started(1), compacted_by(CompactionTrigger::Switch)];
    events_in.extend(stepped("one"));
    assert_eq!(model_after(events_in, true).await.as_deref(), Some(OLD));
}

#[test]
fn a_recorded_withdrawal_restores_the_effort_and_keeps_the_next_one_pending() {
    let mut settings = Settings {
        endpoint: "claude-api".to_owned(),
        model: NEW.to_owned(),
        effort: Effort::Low,
        ..Settings::default()
    };
    let mut withdrawal = withdrawn(Withdrawal::SwitchCompactionCancelled, Effort::Max);
    if let AgentEvent::SettingsChanged { restored, .. } = &mut withdrawal {
        *restored = Some(Restored {
            effort: Effort::Medium,
            cache_ttl: CacheTtl::OneHour,
        });
    }
    settings.scripted(&withdrawal);
    assert_eq!(settings.model, OLD);
    assert_eq!(settings.effort, Effort::Medium);
    assert_eq!(
        settings.pending,
        Some(Pending {
            endpoint: "claude-api".to_owned(),
            model: OLD.to_owned(),
            effort: Effort::Max,
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_recorded_cancelled_switch_compaction_keeps_the_old_pair() {
    let events_in = vec![
        switch_started(1),
        switch_failed(1, CompactionFailure::Cancelled),
        AgentEvent::turn_started(1),
        cancelled(),
    ];
    assert_eq!(model_after(events_in, false).await.as_deref(), Some(OLD));
}

fn ended_process() -> Vec<wisp_protocol::ReportedEnd> {
    vec![wisp_protocol::ReportedEnd {
        id: 3,
        end: wisp_protocol::ProcessEnd::Exited {
            exit: wisp_protocol::ExitStatus::Code { code: 0 },
        },
    }]
}

async fn reported_after_esc(compaction: AgentEvent) -> Vec<Vec<wisp_protocol::ReportedEnd>> {
    let mut turn = stepped("one");
    turn[0] = AgentEvent::turn_started(1).reporting(ended_process());
    let mut events_in = vec![compaction];
    events_in.extend(turn);
    let script = Script::new(events_in).with_settings(on_old_pair());
    let (commands, mut events) = spawn(script, PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| {
        matches!(e, AgentEvent::CompactionStarted { .. })
    })
    .await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    let played = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    pick!(played, AgentEvent::TurnStarted { ends, .. } => ends)
}

#[tokio::test(start_paused = true)]
async fn esc_before_a_replayed_turn_starts_still_reports_the_processes_that_ended() {
    assert_eq!(reported_after_esc(auto_started()).await, [ended_process()]);
    assert_eq!(
        reported_after_esc(switch_started(1)).await,
        [ended_process()]
    );
}
