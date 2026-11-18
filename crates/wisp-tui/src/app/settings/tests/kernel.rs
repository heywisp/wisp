use wisp_protocol::settings::{
    AppliedChange, CACHE_KEPT, CACHE_KEPT_TOOLS, Layer, Overridden, OverrideScope, Place, Restored,
    Saved, SettingTarget, SettingValue, SitePath, When, Withdrawal, find,
};

use wisp_render::chrome::Wins;

use super::*;
use crate::events::{Changed, settings_changed};
use crate::shared::{change, named};

fn applied(
    name: &str,
    value: Option<SettingValue>,
    when: When,
    cache: Option<CacheEffect>,
) -> AppliedChange {
    AppliedChange {
        cache,
        ..change(named(name), value, Layer::Session, when)
    }
}

fn answer(request: RequestId, changes: Vec<AppliedChange>, cache: CacheEffect) -> AgentEvent {
    settings_changed(Changed {
        answers: Some(request),
        endpoint: "claude-api".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        effort: Effort::Medium,
        cache,
        changes,
        ..Changed::default()
    })
}

fn notes(answer: Answer) -> Vec<String> {
    match answer {
        Answer::Note(note) => vec![note],
        Answer::Notes(notes) => notes,
        Answer::Moved(moved, after) => Answer::moved_texts(&moved, &after),
        Answer::Refused(_) | Answer::Hint(_) | Answer::Other => Vec::new(),
    }
}

fn started() -> AgentEvent {
    AgentEvent::turn_started(1)
}

fn ttl_rebuild() -> CacheEffect {
    CacheEffect::Rebuilt {
        reason: RebuildReason::CacheTtlChange,
    }
}

#[test]
fn a_kernel_setting_gets_its_own_note_and_no_pair_note() {
    for (name, value, cache, words, effect) in [
        (
            "context.context_note",
            SettingValue::Bool(false),
            CacheEffect::Kept,
            "Context line \u{2192} off",
            CACHE_KEPT,
        ),
        (
            "tools",
            SettingValue::Text("none".into()),
            CacheEffect::Kept,
            "Tools \u{2192} none",
            CACHE_KEPT,
        ),
        (
            "tools",
            SettingValue::Text("none".into()),
            CacheEffect::KeptIfPassedOn,
            "Tools \u{2192} none",
            CACHE_KEPT_TOOLS,
        ),
    ] {
        let f = facts(Endpoint::Official, CacheTtl::OneHour);
        let mut t = Tracker::new(&f);
        t.asked_settings(RequestId(1), None, None, None);
        let change = applied(name, Some(value), When::NextTurn, Some(cache));
        let answer = t.apply(
            &answer(RequestId(1), vec![change], CacheEffect::Kept),
            &f,
            s(1),
            UNI,
        );
        assert_eq!(
            notes(answer),
            [format!("{words} \u{b7} next turn \u{b7} {effect}")]
        );
        assert!(t.wanted() == *t.next());
    }
}

#[test]
fn a_change_carrying_a_warning_ends_its_note_with_it() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_settings(RequestId(1), None, None, None);
    let mut change = applied(
        "context.background_tokens",
        Some(SettingValue::Tokens(50_000)),
        When::Now,
        None,
    );
    change.warnings = vec![
        "background compaction rarely runs below about 70,000, and claude-opus-5-5 compacts at 20,000; compaction will usually wait at the point."
            .into(),
    ];
    let answer = t.apply(
        &answer(RequestId(1), vec![change], CacheEffect::Kept),
        &f,
        s(1),
        UNI,
    );
    let notes = notes(answer);
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].ends_with(
            " \u{b7} background compaction rarely runs below about 70,000, and claude-opus-5-5 compacts at 20,000; compaction will usually wait at the point."
        ),
        "{notes:?}"
    );
}

#[test]
fn a_mixed_change_is_one_pop_one_pair_note_and_one_note_per_other_setting() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_settings(RequestId(1), None, None, Some(Effort::High));
    t.asked_settings(RequestId(2), None, None, None);
    let changes = vec![
        applied(
            "effort",
            Some(SettingValue::Text("high".into())),
            When::NextTurn,
            Some(CacheEffect::Kept),
        ),
        applied(
            "retry.budget_usd",
            Some(SettingValue::Usd { micros: 500_000 }),
            When::Now,
            None,
        ),
    ];
    let mut event = answer(RequestId(1), changes, CacheEffect::Kept);
    if let AgentEvent::SettingsChanged { effort, .. } = &mut event {
        *effort = Effort::High;
    }
    let all = notes(t.apply(&event, &f, s(1), UNI));
    assert_eq!(
        all,
        [
            format!("Effort \u{2192} high \u{b7} {CACHE_KEPT}").as_str(),
            "Retry budget \u{2192} $0.50 \u{b7} applies now",
        ]
    );
    assert_eq!(t.asked.len(), 1);
    assert_eq!(t.asked[0].request, RequestId(2));
}

#[test]
fn a_saved_overridden_change_names_where_it_went_and_what_still_wins() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_saved(RequestId(1), None);
    let mut change = applied(
        "context_window",
        Some(SettingValue::Tokens(200_000)),
        When::NextTurn,
        Some(CacheEffect::Kept),
    );
    change.target = SettingTarget::Path(SitePath::new(
        Place::Model("claude-opus-5-5".into()),
        find("context_window").unwrap(),
    ));
    change.saved = Some(Saved {
        path_display: "~/.config/wisp/config.toml".into(),
        line: Some(3),
        notes: Vec::new(),
    });
    change.overridden = Some(Overridden {
        by: Layer::Flag,
        scope: OverrideScope::ThisSession,
    });
    let label = find("context_window").unwrap().label;
    assert_eq!(
        notes(t.apply(
            &answer(RequestId(1), vec![change], CacheEffect::Kept),
            &f,
            s(1),
            UNI
        )),
        [format!(
            "{label} for claude-opus-5-5 \u{2192} 200,000 \u{b7} saved to ~/.config/wisp/config.toml \u{b7} the command line still wins this session"
        )]
    );
}

fn saved_to() -> Option<Saved> {
    Some(Saved {
        path_display: "~/.config/wisp/config.toml".into(),
        line: Some(4),
        notes: Vec::new(),
    })
}

fn screen_note(told: Told, change: AppliedChange) -> Vec<String> {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_saved(RequestId(7), Some(told));
    notes(t.apply(
        &answer(RequestId(7), vec![change], CacheEffect::Kept),
        &f,
        s(1),
        UNI,
    ))
}

#[test]
fn a_screen_change_names_the_value_it_replaced() {
    let mut change = applied(
        "retry.budget_usd",
        Some(SettingValue::Usd { micros: 500_000 }),
        When::Now,
        None,
    );
    change.saved = saved_to();
    let told = Told {
        was: "none".into(),
        saved: None,
        wins: None,
    };
    assert_eq!(
        screen_note(told, change.clone()),
        [
            "Retry budget \u{2192} $0.50 \u{b7} applies now \u{b7} saved to ~/.config/wisp/config.toml (was none)"
        ]
    );
    let told = Told {
        was: "$1.00".into(),
        saved: Some("$2.00".into()),
        wins: None,
    };
    assert_eq!(
        screen_note(told, change.clone()),
        [
            "Retry budget \u{2192} $0.50 \u{b7} applies now \u{b7} saved to ~/.config/wisp/config.toml (was $1.00 this session; $2.00 saved)"
        ]
    );
    let told = Told {
        was: "$1.00".into(),
        saved: Some("$1.00".into()),
        wins: None,
    };
    assert_eq!(
        screen_note(told, change),
        [
            "Retry budget \u{2192} $0.50 \u{b7} applies now \u{b7} saved to ~/.config/wisp/config.toml (was $1.00)"
        ]
    );
}

#[test]
fn kept_words_a_model_table_sets_never_read_as_applying_now() {
    let mut change = applied(
        "context.verbatim_user_tokens",
        Some(SettingValue::Tokens(4_000)),
        When::Now,
        None,
    );
    change.saved = saved_to();
    let told = Told {
        was: "8,000".into(),
        saved: None,
        wins: None,
    };
    for (by, words) in [
        (Layer::Model, "this model"),
        (Layer::EndpointModel, "this account and model"),
        (Layer::Endpoint, "this account"),
    ] {
        change.overridden = Some(Overridden {
            by,
            scope: OverrideScope::ThisSession,
        });
        let note = screen_note(told.clone(), change.clone());
        assert_eq!(
            note,
            [format!(
                "Your messages kept \u{2192} 4,000 \u{b7} saved to ~/.config/wisp/config.toml (was 8,000) \u{b7} {words} still wins"
            )]
        );
        assert!(!note[0].contains("applies now"));
    }
}

#[test]
fn a_kept_words_reset_names_the_model_table_not_the_default() {
    let mut change = applied("context.verbatim_user_tokens", None, When::Now, None);
    change.saved = saved_to();
    change.overridden = Some(Overridden {
        by: Layer::Model,
        scope: OverrideScope::ThisSession,
    });
    let told = Told {
        was: "4,000".into(),
        saved: Some("4,000".into()),
        wins: Some(Wins::ThisSession("the default".into())),
    };
    let note = screen_note(told, change);
    assert_eq!(
        note,
        [
            "Your messages kept reset \u{b7} saved value removed (was 4,000) \u{b7} this model still wins"
        ]
    );
}

#[test]
fn a_next_session_change_under_a_flag_still_says_when_it_applies() {
    let mut change = applied(
        "tools",
        Some(SettingValue::List(Vec::new())),
        When::NextSession,
        None,
    );
    change.saved = saved_to();
    change.overridden = Some(Overridden {
        by: Layer::Flag,
        scope: OverrideScope::ThisSession,
    });
    let told = Told {
        was: "every built tool".into(),
        saved: None,
        wins: None,
    };
    assert_eq!(
        screen_note(told, change),
        [
            "Tools \u{2192} none \u{b7} new chats \u{b7} saved to ~/.config/wisp/config.toml (was every built tool) \u{b7} the command line still wins this session"
        ]
    );
}

#[test]
fn a_next_session_change_under_a_variable_says_it_still_wins_in_new_sessions() {
    for (by, scope, words) in [
        (
            Layer::Env,
            OverrideScope::NewSessions,
            "your environment still wins in new sessions",
        ),
        (
            Layer::Flag,
            OverrideScope::ThisSession,
            "the command line still wins this session",
        ),
    ] {
        let mut change = applied(
            "keychain",
            Some(SettingValue::Text("on".into())),
            When::NextSession,
            None,
        );
        change.saved = saved_to();
        change.overridden = Some(Overridden { by, scope });
        let told = Told {
            was: "off".into(),
            saved: Some("off".into()),
            wins: None,
        };
        assert_eq!(
            screen_note(told, change),
            [format!(
                "Keychain \u{2192} on \u{b7} new chats \u{b7} saved to ~/.config/wisp/config.toml (was off) \u{b7} {words}"
            )]
        );
    }
}

#[test]
fn a_next_session_reset_under_a_variable_names_the_variable_that_now_wins() {
    let mut change = applied("keychain", None, When::NextSession, None);
    change.saved = saved_to();
    change.overridden = Some(Overridden {
        by: Layer::Env,
        scope: OverrideScope::NewSessions,
    });
    let told = Told {
        was: "off".into(),
        saved: Some("off".into()),
        wins: Some(Wins::NewSessions(
            "WISP_KEYCHAIN=off in your environment".into(),
        )),
    };
    assert_eq!(
        screen_note(told, change),
        [
            "Keychain reset \u{b7} new chats \u{b7} saved value removed (was off) \u{b7} WISP_KEYCHAIN=off in your environment wins in new sessions"
        ]
    );
}

#[test]
fn a_screen_reset_says_the_saved_value_is_gone_and_what_now_wins() {
    let mut change = applied("time", None, When::NextTurn, Some(CacheEffect::Kept));
    change.saved = saved_to();
    change.overridden = Some(Overridden {
        by: Layer::Env,
        scope: OverrideScope::NewSessions,
    });
    let told = Told {
        was: "utc".into(),
        saved: Some("utc".into()),
        wins: Some(Wins::ThisSession("--time local on the command line".into())),
    };
    assert_eq!(
        screen_note(told, change.clone()),
        [
            format!("Message time reset \u{b7} next turn \u{b7} {CACHE_KEPT} \u{b7} saved value removed (was utc) \u{b7} --time local on the command line now wins this session").as_str()
        ]
    );
    let mut session = applied("compact_at", None, When::NextTurn, Some(CacheEffect::Kept));
    session.saved = saved_to();
    let told = Told {
        was: "500,000".into(),
        saved: None,
        wins: Some(Wins::ThisSession(
            "models.\"claude-opus-5-5\".compact_at = 400,000 in config.toml".into(),
        )),
    };
    assert_eq!(
        screen_note(told, session),
        [
            format!("Compact at reset \u{b7} next turn \u{b7} {CACHE_KEPT} \u{b7} session value removed (was 500,000) \u{b7} models.\"claude-opus-5-5\".compact_at = 400,000 in config.toml now wins this session").as_str()
        ]
    );
    let mut log = applied("log", None, When::NextSession, None);
    log.saved = saved_to();
    let told = Told {
        was: "off".into(),
        saved: Some("off".into()),
        wins: Some(Wins::NewSessions("the default".into())),
    };
    assert_eq!(
        screen_note(told, log),
        [
            "Session log reset \u{b7} new chats \u{b7} saved value removed (was off) \u{b7} the default wins in new sessions"
        ]
    );
}

#[test]
fn a_cache_lifetime_change_is_priced_at_its_own_lifetime_and_follows_at_the_next_turn() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    t.asked_settings(RequestId(1), None, None, None);
    let change = applied(
        "cache_ttl",
        Some(SettingValue::Text("5m".into())),
        When::NextTurn,
        Some(ttl_rebuild()),
    );
    let before = t.rebuild(&f, "claude-opus-5-5", Scope::UpTo, s(2));
    let all = notes(t.apply(
        &answer(RequestId(1), vec![change], ttl_rebuild()),
        &f,
        s(2),
        UNI,
    ));
    let after = t.rebuild(&f, "claude-opus-5-5", Scope::UpTo, s(2));
    assert_ne!(before, after);
    assert_eq!(all.len(), 1);
    assert!(
        all[0].starts_with("Cache lifetime \u{2192} 5m \u{b7} next turn \u{b7} cache rebuild"),
        "{all:?}"
    );
    assert!(all[0].contains(&after.line(UNI)), "{all:?}");
    assert_eq!(t.cache_ttl(), CacheTtl::OneHour);
    assert!(!t.show(&mut f));
    assert_eq!(f.cache_ttl, CacheTtl::OneHour);
    t.apply(&started(), &f, s(3), UNI);
    assert_eq!(t.cache_ttl(), CacheTtl::FiveMinutes);
    assert!(t.show(&mut f));
    assert_eq!(f.cache_ttl, CacheTtl::FiveMinutes);
    assert!((f.write_multipliers.tail - 1.25).abs() < f64::EPSILON);
    t.asked_settings(RequestId(2), None, None, None);
    let reset = applied("cache_ttl", None, When::NextTurn, Some(ttl_rebuild()));
    let all = notes(t.apply(
        &answer(RequestId(2), vec![reset], ttl_rebuild()),
        &f,
        s(4),
        UNI,
    ));
    assert!(
        all[0].starts_with("Cache lifetime reset \u{b7} next turn"),
        "{all:?}"
    );
    t.apply(&started(), &f, s(5), UNI);
    t.show(&mut f);
    assert_eq!(f.cache_ttl, CacheTtl::OneHour);
    assert!((f.write_multipliers.tail - 2.0).abs() < f64::EPSILON);
}

#[test]
fn a_waiting_save_says_saving_and_a_shortcut_says_applying() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    assert_eq!(t.quit_status(), None);
    assert!(!t.saving());
    t.asked_settings(RequestId(1), None, None, Some(Effort::High));
    assert_eq!(t.quit_status(), Some(copy::APPLYING_SETTINGS));
    t.asked_saved(RequestId(2), None);
    t.asked_described(RequestId(3));
    assert_eq!(t.quit_status(), Some(copy::SAVING_SETTINGS));
    assert!(t.saving());
    t.apply(&AgentEvent::refused(RequestId(2), "locked"), &f, s(1), UNI);
    assert_eq!(t.quit_status(), Some(copy::APPLYING_SETTINGS));
}

fn withdrawal(why: Withdrawal, effort: Effort, restored: Restored) -> AgentEvent {
    settings_changed(Changed {
        endpoint: "claude-api".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        effort,
        withdrawn: Some(why),
        restored: Some(restored),
        ..Changed::default()
    })
}

#[test]
fn a_withdrawn_switch_restores_the_current_pair_and_names_the_target() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_settings(
        RequestId(1),
        None,
        Some("claude-sonnet-4-6"),
        Some(Effort::Max),
    );
    let mut switched = answer(
        RequestId(1),
        Vec::new(),
        CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
    );
    if let AgentEvent::SettingsChanged { model, effort, .. } = &mut switched {
        *model = "claude-sonnet-4-6".to_owned();
        *effort = Effort::Max;
    }
    t.apply(&switched, &f, s(1), UNI);
    t.apply(&started(), &f, s(2), UNI);
    assert_eq!(t.session().model, "claude-sonnet-4-6");
    let restored = Restored {
        effort: Effort::Medium,
        cache_ttl: CacheTtl::OneHour,
    };
    let event = withdrawal(
        Withdrawal::SwitchCompactionCancelled,
        Effort::Medium,
        restored,
    );
    let all = notes(t.apply(&event, &f, s(3), UNI));
    assert_eq!(
        all,
        [
            "Switch to claude-sonnet-4-6 withdrawn: the compaction it needed was cancelled \u{b7} choose it again with /model \u{b7} effort back to medium: opus-5-5 doesn't take max"
        ]
    );
    assert_eq!(t.session().model, "claude-opus-5-5");
    assert_eq!(t.session().effort, Some(Effort::Medium));
    assert_eq!(t.next_turn(), None);
    t.show(&mut f);
    assert_eq!(f.model, "claude-opus-5-5");
    assert_eq!(f.next, None);
    t.apply(&started(), &f, s(4), UNI);
    assert_eq!(t.session().model, "claude-opus-5-5");
    assert_eq!(t.session().effort, Some(Effort::Medium));
    t.asked_settings(
        RequestId(2),
        None,
        Some("claude-opus-5-5"),
        Some(Effort::Max),
    );
    let mut unnamed = withdrawal(
        Withdrawal::SwitchCompactionCancelled,
        Effort::Medium,
        Restored {
            effort: Effort::Medium,
            cache_ttl: CacheTtl::OneHour,
        },
    );
    if let AgentEvent::SettingsChanged { model, .. } = &mut unnamed {
        *model = "claude-zzz-9-9".to_owned();
    }
    let all = notes(t.apply(&unnamed, &f, s(5), UNI));
    assert!(
        all[0].ends_with("effort back to medium: claude-zzz-9-9 doesn't take max"),
        "{all:?}"
    );
}

#[test]
fn a_withdrawn_switch_to_a_key_as_the_model_never_names_it() {
    let key = "sk-ant-api03-CANARYMODEL0000";
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_settings(RequestId(1), None, Some(key), None);
    let mut switched = answer(
        RequestId(1),
        Vec::new(),
        CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
    );
    if let AgentEvent::SettingsChanged { model, .. } = &mut switched {
        *model = key.to_owned();
    }
    let mut all = notes(t.apply(&switched, &f, s(1), UNI));
    t.apply(&started(), &f, s(2), UNI);
    let restored = Restored {
        effort: Effort::Medium,
        cache_ttl: CacheTtl::OneHour,
    };
    let event = withdrawal(Withdrawal::SwitchCompactionFailed, Effort::Medium, restored);
    all.extend(notes(t.apply(&event, &f, s(3), UNI)));
    let text = all.join("\n");
    assert!(!text.contains("CANARY"), "{text}");
    assert!(
        text.contains(&format!(
            "Switch to {} withdrawn: the compaction it needed failed",
            wisp_protocol::NOT_SHOWN
        )),
        "{text}"
    );
    assert_eq!(t.session().model, "claude-opus-5-5");
}

#[test]
fn a_failed_switch_back_to_the_current_pair_names_no_target_and_keeps_the_pending_lifetime() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    let mut event = withdrawal(
        Withdrawal::SwitchCompactionFailed,
        Effort::Medium,
        Restored {
            effort: Effort::Medium,
            cache_ttl: CacheTtl::OneHour,
        },
    );
    if let AgentEvent::SettingsChanged { changes, .. } = &mut event {
        changes.push(applied(
            "cache_ttl",
            Some(SettingValue::Text("5m".into())),
            When::NextTurn,
            Some(ttl_rebuild()),
        ));
    }
    let all = notes(t.apply(&event, &f, s(1), UNI));
    assert_eq!(
        all,
        ["The switch the failed compaction was for is withdrawn."]
    );
    t.apply(&started(), &f, s(2), UNI);
    t.show(&mut f);
    assert_eq!(f.cache_ttl, CacheTtl::FiveMinutes);
}

#[test]
fn a_saved_switch_that_is_withdrawn_says_it_is_still_saved() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_saved(RequestId(1), None);
    let mut switched = answer(
        RequestId(1),
        Vec::new(),
        CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
    );
    if let AgentEvent::SettingsChanged { model, .. } = &mut switched {
        *model = "claude-sonnet-5-5".to_owned();
    }
    t.apply(&switched, &f, s(1), UNI);
    let event = withdrawal(
        Withdrawal::SwitchCompactionCancelled,
        Effort::Medium,
        Restored {
            effort: Effort::Medium,
            cache_ttl: CacheTtl::OneHour,
        },
    );
    let all = notes(t.apply(&event, &f, s(2), UNI));
    assert!(
        all[0].ends_with(
            "choose it again with /model \u{b7} still saved as the model Wisp starts with"
        ),
        "{all:?}"
    );
}

#[test]
fn a_snapshot_pops_its_describe_request() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_described(RequestId(4));
    let snapshot = wisp_protocol::settings::SettingsSnapshot {
        often: Vec::new(),
        answers: RequestId(4),
        context: wisp_protocol::settings::SnapshotContext {
            endpoint: "claude-api".into(),
            model: "claude-opus-5-5".into(),
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
        switches: Vec::new(),
        compaction: None,
        plugins: Vec::new(),
        placed: Vec::new(),
    };
    t.apply(&AgentEvent::SettingsSnapshot(snapshot), &f, s(1), UNI);
    assert!(t.asked.is_empty());
}

#[test]
fn a_withdrawal_restores_the_lifetime_its_switch_turn_changed() {
    for takes_effort in [true, false] {
        let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
        if !takes_effort {
            with_levels(&mut f, "claude-opus-5-5", &[], Effort::Medium);
        }
        let mut t = Tracker::new(&f);
        t.asked_settings(RequestId(1), None, Some("claude-sonnet-4-6"), None);
        let ttl = applied(
            "cache_ttl",
            Some(SettingValue::Text("5m".into())),
            When::NextTurn,
            Some(ttl_rebuild()),
        );
        let mut switched = answer(
            RequestId(1),
            vec![ttl.clone()],
            CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            },
        );
        if let AgentEvent::SettingsChanged { model, .. } = &mut switched {
            *model = "claude-sonnet-4-6".to_owned();
        }
        t.apply(&switched, &f, s(1), UNI);
        t.apply(&started(), &f, s(2), UNI);
        assert_eq!(t.cache_ttl(), CacheTtl::FiveMinutes);
        let mut event = withdrawal(
            Withdrawal::SwitchCompactionCancelled,
            Effort::Medium,
            Restored {
                effort: Effort::Medium,
                cache_ttl: CacheTtl::OneHour,
            },
        );
        if let AgentEvent::SettingsChanged { changes, .. } = &mut event {
            changes.push(ttl);
        }
        t.apply(&event, &f, s(3), UNI);
        assert_eq!(t.cache_ttl(), CacheTtl::OneHour);
        t.show(&mut f);
        assert_eq!(f.cache_ttl, CacheTtl::OneHour);
        assert_eq!(f.effort, if takes_effort { "medium" } else { "" });
        assert_eq!(t.session().effort, Some(Effort::Medium));
        assert!((f.write_multipliers.tail - 2.0).abs() < f64::EPSILON);
        t.apply(&started(), &f, s(4), UNI);
        assert_eq!(t.cache_ttl(), CacheTtl::FiveMinutes);
    }
}

fn shadowed_save(value: &str, cache: CacheEffect, by: Layer) -> AppliedChange {
    let mut change = applied(
        "cache_ttl",
        Some(SettingValue::Text(value.into())),
        When::NextTurn,
        Some(cache),
    );
    change.target = SettingTarget::Path(SitePath::global(find("cache_ttl").unwrap()));
    change.layer = Layer::Config;
    change.saved = Some(Saved {
        path_display: "~/.config/wisp/config.toml".into(),
        line: Some(2),
        notes: Vec::new(),
    });
    change.overridden = Some(Overridden {
        by,
        scope: OverrideScope::ThisSession,
    });
    change
}

#[test]
fn a_saved_lifetime_the_session_still_overrides_never_shows_as_pending() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_saved(RequestId(1), None);
    let save = shadowed_save("5m", CacheEffect::Kept, Layer::Flag);
    t.apply(
        &answer(RequestId(1), vec![save], CacheEffect::Kept),
        &f,
        s(1),
        UNI,
    );
    t.apply(&started(), &f, s(2), UNI);
    assert_eq!(t.cache_ttl(), CacheTtl::OneHour);
    assert!(!t.show(&mut f));
    assert_eq!(f.cache_ttl, CacheTtl::OneHour);

    let mut t = Tracker::new(&f);
    t.asked_settings(RequestId(1), None, None, None);
    let session = applied(
        "cache_ttl",
        Some(SettingValue::Text("5m".into())),
        When::NextTurn,
        Some(ttl_rebuild()),
    );
    t.apply(
        &answer(RequestId(1), vec![session], ttl_rebuild()),
        &f,
        s(1),
        UNI,
    );
    t.asked_saved(RequestId(2), None);
    let save = shadowed_save("1h", ttl_rebuild(), Layer::Session);
    t.apply(
        &answer(RequestId(2), vec![save], ttl_rebuild()),
        &f,
        s(2),
        UNI,
    );
    t.apply(&started(), &f, s(3), UNI);
    assert_eq!(t.cache_ttl(), CacheTtl::FiveMinutes);
}

#[test]
fn a_window_bound_to_a_queued_switch_applies_when_that_model_is_current() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_settings(RequestId(1), None, Some("claude-sonnet-4-6"), None);
    let mut switched = answer(
        RequestId(1),
        Vec::new(),
        CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
    );
    if let AgentEvent::SettingsChanged { model, .. } = &mut switched {
        *model = "claude-sonnet-4-6".to_owned();
    }
    t.apply(&switched, &f, s(1), UNI);
    t.asked_settings(RequestId(2), None, None, None);
    let window = |model: &str| {
        let mut change = applied(
            "context_window",
            Some(SettingValue::Tokens(300_000)),
            When::NextTurn,
            Some(CacheEffect::Kept),
        );
        change.target = SettingTarget::Path(SitePath::new(
            Place::Model(model.into()),
            find("context_window").unwrap(),
        ));
        change
    };
    let mut bound = answer(
        RequestId(2),
        vec![window("claude-sonnet-4-6")],
        CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
    );
    if let AgentEvent::SettingsChanged { model, .. } = &mut bound {
        *model = "claude-sonnet-4-6".to_owned();
    }
    let label = find("context_window").unwrap().label;
    assert_eq!(
        notes(t.apply(&bound, &f, s(2), UNI)),
        [format!(
            "{label} for claude-sonnet-4-6 \u{2192} 300,000 \u{b7} applies when it is the current model"
        )]
    );
    t.apply(&started(), &f, s(3), UNI);
    t.asked_settings(RequestId(3), None, None, None);
    let mut here = answer(
        RequestId(3),
        vec![window("claude-sonnet-4-6")],
        CacheEffect::Kept,
    );
    if let AgentEvent::SettingsChanged { model, .. } = &mut here {
        *model = "claude-sonnet-4-6".to_owned();
    }
    assert_eq!(
        notes(t.apply(&here, &f, s(4), UNI)),
        [format!(
            "{label} for claude-sonnet-4-6 \u{2192} 300,000 \u{b7} next turn \u{b7} {CACHE_KEPT}"
        )]
    );
}

#[test]
fn a_resumed_session_starts_from_the_restored_pending_values() {
    for takes_effort in [true, false] {
        let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
        f.next = Some(NextTurn {
            endpoint: None,
            model: None,
            effort: Some(Effort::Low),
            cache_ttl: Some(CacheTtl::FiveMinutes),
        });
        if !takes_effort {
            let model = f.model.clone();
            with_levels(&mut f, &model, &[], Effort::Low);
        }
        let mut t = Tracker::new(&f);
        assert_eq!(t.next().effort, Some(Effort::Low));
        assert_eq!(t.cache_ttl(), CacheTtl::OneHour);
        assert_eq!(
            t.next_turn().and_then(|n| n.cache_ttl),
            Some(CacheTtl::FiveMinutes)
        );
        t.show(&mut f);
        assert_eq!(f.next, None);
        assert_eq!(f.effort, if takes_effort { "low" } else { "" });
        t.apply(&started(), &f, s(1), UNI);
        assert_eq!(t.cache_ttl(), CacheTtl::FiveMinutes);
        assert_eq!(t.session().effort, Some(Effort::Low));
        assert_eq!(t.next_turn(), None);
    }
}

#[test]
fn a_display_change_under_a_forced_or_deferring_layer_says_what_wins_now() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let cases = [
        ("display.color", "16", Layer::Forced, "a forced value"),
        ("display.motion", "full", Layer::Forced, "a forced value"),
        ("display.theme", "auto", Layer::Env, "your environment"),
    ];
    for (n, (name, value, by, words)) in cases.into_iter().enumerate() {
        let request = RequestId(n as u64 + 1);
        let mut t = Tracker::new(&f);
        t.asked_saved(request, None);
        let mut change = applied(
            name,
            Some(SettingValue::Text(value.into())),
            When::Now,
            None,
        );
        change.saved = saved_to();
        change.overridden = Some(Overridden {
            by,
            scope: OverrideScope::ThisSession,
        });
        let label = find(name).unwrap().label;
        assert_eq!(
            notes(t.apply(
                &answer(request, vec![change], CacheEffect::Kept),
                &f,
                s(1),
                UNI
            )),
            [format!(
                "{label} \u{2192} {value} \u{b7} saved to ~/.config/wisp/config.toml \u{b7} {words} still wins this session"
            )]
        );
    }
}

fn withdrawn_after(saved: bool, endpoint: Option<&str>, model: Option<&str>) -> Vec<String> {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    if saved {
        t.asked_saved(RequestId(1), None);
    } else {
        t.asked_settings(RequestId(1), endpoint, model, None);
    }
    let mut switched = answer(
        RequestId(1),
        Vec::new(),
        CacheEffect::Rebuilt {
            reason: RebuildReason::EndpointSwitch,
        },
    );
    if let AgentEvent::SettingsChanged {
        model: to_model,
        endpoint: to_endpoint,
        ..
    } = &mut switched
    {
        if let Some(name) = endpoint {
            *to_endpoint = name.to_owned();
        }
        if let Some(id) = model {
            *to_model = id.to_owned();
        }
    }
    t.apply(&switched, &f, s(1), UNI);
    let event = withdrawal(
        Withdrawal::SwitchCompactionCancelled,
        Effort::Medium,
        Restored {
            effort: Effort::Medium,
            cache_ttl: CacheTtl::OneHour,
        },
    );
    notes(t.apply(&event, &f, s(2), UNI))
}

#[test]
fn a_withdrawn_endpoint_switch_points_at_endpoint_and_its_saved_default() {
    let saved = withdrawn_after(true, Some("manifest"), None);
    assert_eq!(
        saved,
        [
            "Switch to manifest withdrawn: the compaction it needed was cancelled \u{b7} choose it again with /account \u{b7} still saved as the account Wisp starts on"
        ]
    );
    let shortcut = withdrawn_after(false, Some("manifest"), None);
    assert_eq!(
        shortcut,
        [
            "Switch to manifest withdrawn: the compaction it needed was cancelled \u{b7} choose it again with /account"
        ]
    );
}

#[test]
fn a_withdrawn_mixed_switch_names_both_targets_and_both_saved_defaults_model_first() {
    let saved = withdrawn_after(true, Some("manifest"), Some("claude-sonnet-5-5"));
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert!(
        saved[0].ends_with(
            "choose them again with /model and /account \u{b7} still saved as the model Wisp starts with \u{b7} still saved as the account Wisp starts on"
        ),
        "{saved:?}"
    );
    assert!(
        saved[0].starts_with("Switch to claude-sonnet-5-5 on manifest withdrawn"),
        "{saved:?}"
    );
}
