use wisp_protocol::settings::{
    AppliedChange, Consequence, Layer, SettingValue, SettingsSnapshot, SnapshotContext,
    SwitchQuote, SwitchTarget, When,
};
use wisp_protocol::{CompactionTrigger, SwitchCompaction, SwitchCompactionCause};

use super::*;
use crate::events::{Changed, settings_changed};
use crate::shared::{change, named};

const SONNET: &str = "claude-sonnet-5-5";

fn consequence(micros: u64, cause: SwitchCompactionCause) -> Consequence {
    Consequence::CompactsBeforeSwitch {
        estimate_micros: micros,
        head_tokens: 15_000,
        cause,
    }
}

fn snapshot(quote: Consequence) -> AgentEvent {
    snapshot_of(vec![SwitchQuote {
        target: SwitchTarget::Model(SONNET.to_owned()),
        consequence: Some(quote),
        retry_compaction: None,
        remembered: None,
    }])
}

pub(crate) fn snapshot_of(switches: Vec<SwitchQuote>) -> AgentEvent {
    AgentEvent::SettingsSnapshot(SettingsSnapshot {
        often: Vec::new(),
        answers: ASKED,
        context: SnapshotContext {
            endpoint: "manifest".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            agent: None,
            max_tokens: 64_000,
            window: None,
            room: None,
        },
        settings: Vec::new(),
        saved: Vec::new(),
        endpoints: Vec::new(),
        models: Vec::new(),
        file: wisp_protocol::settings::FileState::Missing,
        switches,
        compaction: None,
        plugins: Vec::new(),
        placed: Vec::new(),
    })
}

fn to_sonnet(quote: Consequence) -> AgentEvent {
    settings_changed(Changed {
        endpoint: "manifest".to_owned(),
        model: SONNET.to_owned(),
        effort: Effort::Medium,
        cache: CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
        changes: vec![AppliedChange {
            cache: Some(CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            }),
            consequence: Some(quote),
            ..change(
                named("model"),
                Some(SettingValue::Text(SONNET.to_owned())),
                Layer::Session,
                When::NextTurn,
            )
        }],
        ..Changed::default()
    })
}

fn started(micros: u64, cause: SwitchCompactionCause) -> AgentEvent {
    AgentEvent::CompactionStarted {
        trigger: CompactionTrigger::Switch,
        turn: Some(2),
        switch: Some(SwitchCompaction {
            estimate_micros: micros,
            head_tokens: 15_000,
            cause,
        }),
        requested_by: None,
    }
}

fn unpriced(f: &mut Facts) {
    for e in f.endpoints.iter_mut().filter(|e| e.name == "manifest") {
        e.priced = false;
    }
}

fn note(t: &mut Tracker, f: &Facts, event: &AgentEvent) -> String {
    match t.apply(event, f, s(1), UNI) {
        Answer::Note(note) => note,
        Answer::Notes(notes) => notes.join(" | "),
        Answer::Moved(moved, after) => Answer::moved_texts(&moved, &after).join(" | "),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_quoted_compaction_on_an_unpriced_endpoint_leaves_its_figure_out() {
    let mut f = endpoint_facts();
    unpriced(&mut f);
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    t.apply(
        &snapshot(consequence(0, SwitchCompactionCause::Convert)),
        &f,
        s(1),
        UNI,
    );
    let row = t.model_effect(&f, SONNET, s(1)).text(UNI);
    assert_eq!(
        row,
        "cache rebuild, no prices \u{b7} compacts before your next message, then switches (converts the summary)"
    );
    let changed = note(
        &mut t,
        &f,
        &to_sonnet(consequence(0, SwitchCompactionCause::Convert)),
    );
    assert!(!changed.contains('$'), "{changed}");
    assert!(
        changed.ends_with("then switches (converts the summary)"),
        "{changed}"
    );
    let running = note(&mut t, &f, &started(0, SwitchCompactionCause::Convert));
    assert_eq!(running, format!("Compacting before switching to {SONNET}"));
    t.apply(
        &snapshot(consequence(190_000, SwitchCompactionCause::Convert)),
        &f,
        s(1),
        UNI,
    );
    let figured = t.model_effect(&f, SONNET, s(1)).text(UNI);
    assert!(!figured.contains('$'), "{figured}");
    let grown = note(
        &mut t,
        &f,
        &started(190_000, SwitchCompactionCause::Convert),
    );
    assert!(!grown.contains('$'), "{grown}");
}

#[test]
fn a_convert_alone_shows_only_the_rebuild_of_the_head() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    t.apply(
        &snapshot(consequence(0, SwitchCompactionCause::Convert)),
        &f,
        s(1),
        UNI,
    );
    let Effect::Switch(rebuild, work) = t.model_effect(&f, SONNET, s(7200)) else {
        panic!("a quoted switch");
    };
    assert!(work.alone);
    assert!(matches!(rebuild, Rebuild::Usd(_)), "{rebuild:?}");
    assert!(
        t.model_effect(&f, SONNET, s(7200))
            .text(UNI)
            .ends_with("\u{b7} converts the summary before switching")
    );
    assert!(matches!(
        t.model_effect(&f, "claude-haiku-5-5", s(1)),
        Effect::Rebuild(_)
    ));
}

#[test]
fn a_run_that_differs_from_its_quote_says_it_was_not_quoted() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    t.apply(
        &to_sonnet(consequence(0, SwitchCompactionCause::Convert)),
        &f,
        s(1),
        UNI,
    );
    let grown = note(
        &mut t,
        &f,
        &started(190_000, SwitchCompactionCause::Convert),
    );
    assert_eq!(
        grown,
        format!(
            "Compacting before switching to {SONNET} (not quoted when you switched) \u{b7} ~$0.19"
        )
    );
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    t.apply(
        &to_sonnet(consequence(190_000, SwitchCompactionCause::Convert)),
        &f,
        s(1),
        UNI,
    );
    let same = note(
        &mut t,
        &f,
        &started(210_000, SwitchCompactionCause::Convert),
    );
    assert_eq!(
        same,
        format!("Compacting before switching to {SONNET} \u{b7} ~$0.21")
    );
    let other = note(
        &mut t,
        &f,
        &started(210_000, SwitchCompactionCause::SmallerWindow),
    );
    assert!(other.contains("(not quoted when you switched)"), "{other}");
    let mut resumed = Tracker::new(&f);
    let quiet = note(
        &mut resumed,
        &f,
        &started(210_000, SwitchCompactionCause::SmallerWindow),
    );
    assert!(!quiet.contains("not quoted"), "{quiet}");
}

#[test]
fn a_switch_quoted_with_no_compaction_says_so_when_one_runs() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    let mut plain = to_sonnet(consequence(0, SwitchCompactionCause::Convert));
    if let AgentEvent::SettingsChanged { changes, .. } = &mut plain {
        for c in changes.iter_mut() {
            c.consequence = None;
        }
    }
    t.apply(&plain, &f, s(1), UNI);
    let running = note(
        &mut t,
        &f,
        &started(190_000, SwitchCompactionCause::KeptTurns),
    );
    assert_eq!(
        running,
        format!(
            "Compacting before switching to {SONNET} (not quoted when you switched) \u{b7} ~$0.19"
        )
    );
}

fn compacted_by(trigger: CompactionTrigger, turn: Option<u64>, partial: bool) -> AgentEvent {
    AgentEvent::Compacted {
        trigger,
        engine: wisp_protocol::CompactionEngine::Native,
        before: 600_000,
        after: 15_000,
        partial,
        converted: trigger == CompactionTrigger::Convert,
        cache_effect: CacheEffect::Rebuilt {
            reason: RebuildReason::Compaction,
        },
        usage: None,
        cost: None,
        answers: None,
        turn,
        summary: None,
        paused: false,
    }
}

fn prepared_through(last_turn: u64) -> AgentEvent {
    AgentEvent::CompactionPrepared {
        covers: Some(wisp_protocol::Covers {
            first_seq: 1,
            last_seq: 40,
            first_turn: 1,
            last_turn,
        }),
        usage: None,
        cost: None,
        turn: None,
    }
}

fn convert_note_after(t: &mut Tracker, f: &Facts, from: u64, partial: bool) -> String {
    for n in from..=12 {
        t.apply(&AgentEvent::turn_started(n), f, s(1), UNI);
    }
    t.apply(
        &to_sonnet(consequence(190_000, SwitchCompactionCause::KeptTurns)),
        f,
        s(1),
        UNI,
    );
    t.apply(
        &started(190_000, SwitchCompactionCause::KeptTurns),
        f,
        s(1),
        UNI,
    );
    let failed = AgentEvent::CompactionFailed {
        trigger: CompactionTrigger::Switch,
        engine: None,
        kind: wisp_protocol::CompactionFailure::Other("x".to_owned()),
        message: "x".to_owned(),
        usage: None,
        cost: None,
        answers: None,
        turn: Some(13),
        paused: false,
        requested_by: None,
    };
    t.apply(&failed, f, s(1), UNI);
    note(
        t,
        f,
        &compacted_by(CompactionTrigger::Convert, Some(13), partial),
    )
}

#[test]
fn the_convert_note_counts_kept_turns_from_a_background_head() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    for n in 1..=3 {
        t.apply(&AgentEvent::turn_started(n), &f, s(1), UNI);
    }
    t.apply(
        &compacted_by(CompactionTrigger::Manual, None, false),
        &f,
        s(1),
        UNI,
    );
    for n in 4..=10 {
        t.apply(&AgentEvent::turn_started(n), &f, s(1), UNI);
    }
    t.apply(&prepared_through(7), &f, s(1), UNI);
    t.apply(
        &compacted_by(CompactionTrigger::Background, Some(10), false),
        &f,
        s(1),
        UNI,
    );
    let note = convert_note_after(&mut t, &f, 11, true);
    assert!(
        note.ends_with("turns 8\u{2013}12 kept only as your messages and the ledger"),
        "{note}"
    );
}

#[test]
fn the_convert_note_names_kept_turns_when_only_a_background_head_ran() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    for n in 1..=10 {
        t.apply(&AgentEvent::turn_started(n), &f, s(1), UNI);
    }
    t.apply(&prepared_through(9), &f, s(1), UNI);
    t.apply(
        &compacted_by(CompactionTrigger::Background, Some(10), false),
        &f,
        s(1),
        UNI,
    );
    let note = convert_note_after(&mut t, &f, 11, true);
    assert!(
        note.ends_with("turns 10\u{2013}12 kept only as your messages and the ledger"),
        "{note}"
    );
}

#[test]
fn a_whole_convert_names_no_kept_turns() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    for n in 1..=3 {
        t.apply(&AgentEvent::turn_started(n), &f, s(1), UNI);
    }
    t.apply(
        &compacted_by(CompactionTrigger::Manual, None, false),
        &f,
        s(1),
        UNI,
    );
    let note = convert_note_after(&mut t, &f, 4, false);
    assert!(!note.contains("kept only"), "{note}");
    assert!(
        note.starts_with("Couldn't compact before switching"),
        "{note}"
    );
}

fn withdrawn_switch() -> AgentEvent {
    settings_changed(Changed {
        endpoint: "manifest".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        effort: Effort::Medium,
        withdrawn: Some(wisp_protocol::settings::Withdrawal::SwitchCompactionCancelled),
        ..Changed::default()
    })
}

#[test]
fn a_quote_lasts_only_until_its_turn_starts_or_the_switch_is_withdrawn() {
    let f = endpoint_facts();
    for reset in [AgentEvent::turn_started(2), withdrawn_switch()] {
        let mut t = Tracker::new(&f);
        warm(&mut t, &f);
        t.apply(
            &to_sonnet(consequence(190_000, SwitchCompactionCause::Convert)),
            &f,
            s(1),
            UNI,
        );
        t.apply(&reset, &f, s(1), UNI);
        let running = note(
            &mut t,
            &f,
            &started(210_000, SwitchCompactionCause::SmallerWindow),
        );
        assert!(!running.contains("not quoted"), "{reset:?}: {running}");
    }
}

#[test]
fn a_switch_note_prices_the_head_it_rebuilds_not_the_whole_prompt() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    let shown = |r: &Rebuild| r.line(UNI);
    let head = shown(&t.head_rebuild(&f, "manifest", SONNET, 15_000));
    let whole = shown(&t.rebuild_on(&f, "manifest", SONNET, Scope::Exact, s(1)));
    assert_ne!(head, whole);
    let changed = note(
        &mut t,
        &f,
        &to_sonnet(consequence(190_000, SwitchCompactionCause::KeptTurns)),
    );
    assert!(changed.contains(&head), "{changed} lacks {head}");
}

#[test]
fn a_switchs_head_rebuild_is_priced_at_the_next_turns_lifetime() {
    for (ttl, multiplier) in [(CacheTtl::OneHour, 2.0), (CacheTtl::FiveMinutes, 1.25)] {
        let f = facts(Endpoint::Official, ttl);
        let t = Tracker::new(&f);
        let Rebuild::Usd(usd) = t.head_rebuild(&f, "claude-api", "claude-opus-5-5", 15_000) else {
            panic!("not priced");
        };
        let expected = 15_000.0 * multiplier * 4.0 / 1e6;
        assert!((usd - expected).abs() < 1e-12, "{ttl:?}: {usd}");
    }
}

#[test]
fn a_compaction_on_a_model_with_no_prices_leaves_its_figure_out() {
    let mut f = endpoint_facts();
    f.model = "some-gateway-model".to_owned();
    let mut t = Tracker::new(&f);
    assert_eq!(t.session().model, "some-gateway-model");
    let running = note(
        &mut t,
        &f,
        &started(190_000, SwitchCompactionCause::KeptTurns),
    );
    assert!(!running.contains('$'), "{running}");
}

#[test]
fn a_switch_to_a_model_nothing_names_shows_its_warning_first_and_its_full_name() {
    let f = crate::app::fixture::search_facts("manifest");
    let mut t = Tracker::new(&f);
    let warning = "direct doesn't list claude-zzz-9-9, so the request may be refused.";
    let moved = settings_changed(Changed {
        answers: Some(ASKED),
        endpoint: "direct".to_owned(),
        model: "claude-zzz-9-9".to_owned(),
        effort: Effort::Medium,
        cache: CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
        changes: vec![AppliedChange {
            warnings: vec![warning.to_owned()],
            ..change(
                named("model"),
                Some(SettingValue::Text("claude-zzz-9-9".to_owned())),
                Layer::Session,
                When::NextTurn,
            )
        }],
        ..Changed::default()
    });
    assert_eq!(
        note(&mut t, &f, &moved),
        format!(
            "Switched to direct \u{b7} API \u{b7} claude-zzz-9-9 medium \u{b7} nothing cached yet, so nothing to rebuild | {warning}"
        )
    );
}

#[test]
fn an_effort_only_change_shows_its_warning_even_when_deferred() {
    let f = endpoint_facts();
    for running in [false, true] {
        let mut t = Tracker::new(&f);
        t.running = running;
        let warning = "Could not remember the model and effort for this account.";
        let event = settings_changed(Changed {
            answers: Some(ASKED),
            endpoint: "manifest".into(),
            model: f.model.clone(),
            effort: Effort::High,
            changes: vec![AppliedChange {
                warnings: vec![warning.into()],
                ..change(
                    named("effort"),
                    Some(SettingValue::Text("high".into())),
                    Layer::Session,
                    When::NextTurn,
                )
            }],
            ..Changed::default()
        });
        let answer = t.apply(&event, &f, s(1), UNI);
        let answer = if running {
            assert!(matches!(answer, Answer::Other));
            t.apply(&completed(true), &f, s(2), UNI)
        } else {
            answer
        };
        assert!(
            matches!(answer, Answer::Notes(notes) if notes.last().is_some_and(|n| n == warning))
        );
    }
}
