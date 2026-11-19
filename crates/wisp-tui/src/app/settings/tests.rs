use wisp_protocol::settings::{CACHE_KEPT, CACHE_KEPT_EFFORT};
use wisp_protocol::{InputId, RejectKind, RequestId, TurnOutcome, Usage};
use wisp_render::chrome::account::AccountKind;

use super::*;
use crate::app::fixture::{ended, endpoint_facts, facts, s, with_levels};
use crate::events::{Changed, End, Ending, outcome, settings_changed, turn_ended};

const UNI: GlyphSet = GlyphSet::Unicode;
const ASKED: RequestId = RequestId(0);

fn answering(cache: &CacheEffect) -> Option<RequestId> {
    match cache {
        CacheEffect::Rebuilt {
            reason: RebuildReason::PerMessageEffortRejected | RebuildReason::NativeSearchRefused,
        } => None,
        _ => Some(ASKED),
    }
}

fn step(usage: Usage) -> AgentEvent {
    AgentEvent::StepEnded {
        step: 1,
        usage: Some(usage),
        cost_usd: None,
        lower_bound: false,
    }
}

fn first_request(one_hour: u64, five_minutes: u64) -> Usage {
    Usage {
        input_tokens: 10,
        cache_creation_input_tokens: one_hour + five_minutes,
        cache_creation_1h_input_tokens: Some(one_hour),
        cache_creation_5m_input_tokens: Some(five_minutes),
        ..Usage::default()
    }
}

fn later_request(prompt: u64) -> Usage {
    Usage {
        input_tokens: 100,
        cache_read_input_tokens: prompt - 100,
        ..Usage::default()
    }
}

fn changed(model: &str, effort: Effort, cache: CacheEffect) -> AgentEvent {
    settings_changed(Changed {
        answers: answering(&cache),
        endpoint: "claude-api".to_owned(),
        model: model.to_owned(),
        effort,
        cache,
        ..Changed::default()
    })
}

fn completed(committed: bool) -> AgentEvent {
    turn_ended(End {
        model: "claude-opus-5-5".to_owned(),
        committed,
        ..End::default()
    })
}

fn feed(t: &mut Tracker, f: &Facts, events: &[AgentEvent]) -> Vec<Answer> {
    events.iter().map(|e| t.apply(e, f, s(1), UNI)).collect()
}

fn note(answer: &Answer) -> String {
    match answer {
        Answer::Note(text) | Answer::Refused(text) => text.clone(),
        Answer::Notes(texts) => texts.first().cloned().unwrap_or_default(),
        Answer::Moved(moved, after) => Answer::moved_texts(moved, after).join(" | "),
        Answer::Hint(_) | Answer::Other => String::new(),
    }
}

#[test]
fn shift_tab_steps_up_and_wraps_on_the_official_endpoint() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let (level, hint) = t.cycle(&f, s(0), UNI).unwrap();
    assert_eq!(level, Effort::High);
    assert_eq!(
        hint,
        format!("effort medium \u{2192} high \u{b7} next turn \u{b7} {CACHE_KEPT}").as_str()
    );
    f.effort = "max".to_owned();
    let t = Tracker::new(&f);
    assert_eq!(t.cycle(&f, s(0), UNI).unwrap().0, Effort::Low);
    let ascii = t.cycle(&f, s(0), GlyphSet::Ascii).unwrap().1;
    assert_eq!(
        ascii,
        format!("effort max -> low - next turn - {CACHE_KEPT}").as_str()
    );
}

#[test]
fn shift_tab_waits_on_an_unknown_gateway_and_explains_the_cost() {
    let f = facts(Endpoint::Custom, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Unknown);
    assert_eq!(
        t.cycle(&f, s(0), UNI).unwrap_err(),
        "This gateway hasn't shown it passes per-message effort on, so a change may rebuild the cache (nothing cached yet). Use /effort to choose."
    );
    feed(
        &mut t,
        &f,
        &[step(first_request(10_000, 0)), step(later_request(12_000))],
    );
    let why = t.cycle(&f, s(2), UNI).unwrap_err();
    assert!(why.contains("(up to ~$0.096)"), "{why}");
}

fn state(model: &str, feature: Feature, state: FeatureState) -> AgentEvent {
    AgentEvent::FeatureStateChanged {
        endpoint: "claude-api".to_owned(),
        model: model.to_owned(),
        feature,
        state,
    }
}

#[test]
fn only_the_kernels_feature_state_event_moves_a_pair() {
    let f = facts(Endpoint::Custom, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    let answers = feed(
        &mut t,
        &f,
        &[
            changed("claude-opus-5-5", Effort::High, CacheEffect::KeptIfPassedOn),
            AgentEvent::turn_started(1),
            completed(true),
            changed("claude-opus-5-5", Effort::Low, CacheEffect::Kept),
            AgentEvent::turn_started(1),
            completed(true),
        ],
    );
    assert_eq!(
        note(&answers[3]),
        format!("Effort \u{2192} low \u{b7} {CACHE_KEPT}").as_str()
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Unknown);
    feed(
        &mut t,
        &f,
        &[state(
            "claude-opus-5-5",
            Feature::BindingControls,
            FeatureState::Proven,
        )],
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Unknown);
    feed(
        &mut t,
        &f,
        &[state(
            "claude-opus-5-5",
            Feature::PerMessageEffort,
            FeatureState::Proven,
        )],
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Proven);
    assert_eq!(t.pair(&f, "claude-sonnet-5-5"), Pair::Unknown);
    assert!(t.cycle(&f, s(0), UNI).is_ok());
    feed(
        &mut t,
        &f,
        &[state(
            "claude-sonnet-5-5",
            Feature::PerMessageEffort,
            FeatureState::FallenBack,
        )],
    );
    assert_eq!(t.pair(&f, "claude-sonnet-5-5"), Pair::FallenBack);
}

#[test]
fn a_silent_fallback_followed_by_an_answered_turn_stays_unproven() {
    let f = facts(Endpoint::Custom, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    feed(
        &mut t,
        &f,
        &[
            AgentEvent::turn_started(1),
            state(
                "claude-opus-5-5",
                Feature::PerMessageEffort,
                FeatureState::FallenBack,
            ),
            step(first_request(10_000, 0)),
            completed(true),
            changed(
                "claude-opus-5-5",
                Effort::Low,
                CacheEffect::Rebuilt {
                    reason: RebuildReason::EffortChange,
                },
            ),
            AgentEvent::turn_started(1),
            completed(true),
        ],
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::FallenBack);
    assert!(matches!(
        t.effort_effect(&f, "claude-opus-5-5", Effort::High, s(2)),
        Effect::Rebuild(_)
    ));
    assert!(t.cycle(&f, s(2), UNI).is_err());
}

#[test]
fn a_kept_return_to_the_running_effort_proves_nothing() {
    let f = facts(Endpoint::Custom, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    feed(
        &mut t,
        &f,
        &[
            changed("claude-opus-5-5", Effort::High, CacheEffect::KeptIfPassedOn),
            changed("claude-opus-5-5", Effort::Medium, CacheEffect::Kept),
        ],
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Unknown);
}

#[test]
fn a_correction_falls_back_and_shift_tab_says_why() {
    let f = facts(Endpoint::Custom, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    let answers = feed(
        &mut t,
        &f,
        &[
            changed("claude-opus-5-5", Effort::High, CacheEffect::KeptIfPassedOn),
            AgentEvent::turn_started(1),
            changed(
                "claude-opus-5-5",
                Effort::High,
                CacheEffect::Rebuilt {
                    reason: RebuildReason::PerMessageEffortRejected,
                },
            ),
        ],
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Unknown);
    feed(
        &mut t,
        &f,
        &[
            state(
                "claude-opus-5-5",
                Feature::PerMessageEffort,
                FeatureState::FallenBack,
            ),
            completed(true),
        ],
    );
    assert_eq!(
        note(&answers[2]),
        "Effort \u{2192} high \u{b7} cache rebuilt: this gateway doesn't pass per-message effort on for opus-5-5"
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::FallenBack);
    let why = t.cycle(&f, s(0), UNI).unwrap_err();
    assert!(
        why.starts_with(
            "This gateway doesn't pass per-message effort on for opus-5-5, so a change rebuilds the cache"
        ),
        "{why}"
    );
}

#[test]
fn shift_tab_never_acts_on_a_model_without_per_message_effort() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    f.model = "claude-opus-4-8".to_owned();
    let warmed = |f: &Facts| {
        let mut t = Tracker::new(f);
        feed(
            &mut t,
            f,
            &[step(first_request(10_000, 0)), step(later_request(12_000))],
        );
        t
    };
    let t = warmed(&f);
    assert_eq!(
        t.cycle(&f, s(2), UNI).unwrap_err(),
        "Changing effort on opus-4-8 rebuilds the cache (up to ~$0.12). Use /effort to choose."
    );
    f.model = "gateway-model".to_owned();
    let t = Tracker::new(&f);
    let why = t.cycle(&f, s(0), UNI).unwrap_err();
    assert!(why.starts_with("Changing effort on gateway-model"), "{why}");

    let base = f.model_facts("claude-opus-4-8").unwrap().clone();
    for (effort, rate) in [(Effort::Medium, 5.0), (Effort::High, 2.0)] {
        let mut sibling = base.clone();
        sibling.id = format!("effort-variant-{}", effort.as_str());
        sibling.variant = Some(wisp_protocol::settings::EffortVariant {
            family: "effort-variant".into(),
            effort,
        });
        sibling.rates.as_mut().unwrap().input_per_mtok = rate;
        f.models.push(sibling);
    }
    f.model = "effort-variant-medium".into();
    let t = warmed(&f);
    assert_eq!(
        t.cycle(&f, s(2), UNI).unwrap_err(),
        "Changing effort on effort-variant-medium rebuilds the cache (up to ~$0.048). Use /effort to choose."
    );
}

#[test]
fn the_prefix_and_tail_are_priced_at_their_own_multipliers() {
    let one_hour = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&one_hour);
    feed(
        &mut t,
        &one_hour,
        &[step(first_request(10_000, 0)), step(later_request(12_100))],
    );
    assert_eq!(
        t.cache_size(),
        CacheSize {
            prefix: 10_000,
            total: 12_100
        }
    );
    let Rebuild::Usd(usd) = t.rebuild(&one_hour, "claude-opus-5-5", Scope::Exact, s(2)) else {
        panic!("not priced");
    };
    assert!((usd - 12_100.0 * 2.0 * 4.0 / 1e6).abs() < 1e-12, "{usd}");

    let five = facts(Endpoint::Official, CacheTtl::FiveMinutes);
    let mut t = Tracker::new(&five);
    feed(
        &mut t,
        &five,
        &[
            step(first_request(1_000, 9_000)),
            step(later_request(12_100)),
        ],
    );
    let Rebuild::Usd(usd) = t.rebuild(&five, "claude-opus-5-5", Scope::Exact, s(2)) else {
        panic!("not priced");
    };
    let expected = (1_000.0 * 2.0 + 11_100.0 * 1.25) * 4.0 / 1e6;
    assert!((usd - expected).abs() < 1e-12, "{usd}");
    assert_eq!(
        t.rebuild(&five, "claude-sonnet-5-5", Scope::Exact, s(2))
            .line(UNI),
        "cache rebuild ~$0.0318"
    );
}

#[test]
fn an_unsplit_first_write_counts_as_prefix_only_in_a_one_hour_session() {
    let unsplit = Usage {
        cache_creation_input_tokens: 5_000,
        ..Usage::default()
    };
    for (ttl, prefix) in [(CacheTtl::OneHour, 5_000), (CacheTtl::FiveMinutes, 0)] {
        let f = facts(Endpoint::Official, ttl);
        let mut t = Tracker::new(&f);
        feed(&mut t, &f, &[step(unsplit)]);
        assert_eq!(t.cache_size().prefix, prefix, "{ttl:?}");
    }
}

#[test]
fn the_rate_follows_the_whole_prompt_across_the_long_prompt_threshold() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    for (total, rate) in [(100_000, 0.10), (100_001, 0.50)] {
        let mut t = Tracker::new(&f);
        feed(&mut t, &f, &[step(later_request(total))]);
        let Rebuild::Usd(usd) = t.rebuild(&f, "claude-haiku-5-5", Scope::Exact, s(2)) else {
            panic!("not priced");
        };
        let expected = total as f64 * 2.0 * rate / 1e6;
        assert!((usd - expected).abs() < 1e-12, "{total}: {usd}");
    }
}

#[test]
fn an_expired_cache_costs_nothing_and_an_unpriced_model_shows_tokens() {
    let expired = "cache expired";
    let line = |t: &Tracker, f: &Facts, model: &str, scope: Scope, at: u64| {
        t.rebuild(f, model, scope, s(at)).line(UNI)
    };
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.apply(&step(first_request(8_000, 0)), &f, s(10), UNI);
    t.apply(&step(later_request(20_000)), &f, s(10), UNI);
    for scope in [Scope::Exact, Scope::UpTo] {
        assert_ne!(line(&t, &f, "claude-opus-5-5", scope, 3610), expired);
        assert_eq!(line(&t, &f, "claude-opus-5-5", scope, 3611), expired);
    }
    assert_eq!(
        line(&t, &f, "some-gateway-model", Scope::Exact, 11),
        "cache rebuild \u{2248}20k tokens"
    );
    assert_eq!(
        line(&t, &f, "some-gateway-model", Scope::UpTo, 11),
        "cache rebuild up to \u{2248}20k tokens"
    );

    let five = facts(Endpoint::Official, CacheTtl::FiveMinutes);
    let mut t = Tracker::new(&five);
    t.apply(&step(first_request(8_000, 0)), &five, s(10), UNI);
    t.apply(&step(later_request(20_000)), &five, s(10), UNI);
    assert_eq!(
        line(&t, &five, "claude-opus-5-5", Scope::UpTo, 310),
        "cache rebuild up to ~$0.124"
    );
    assert_eq!(
        line(&t, &five, "claude-opus-5-5", Scope::UpTo, 311),
        "cache rebuild up to ~$0.064"
    );
    let Rebuild::Usd(usd) = t.rebuild(&five, "claude-opus-5-5", Scope::Exact, s(311)) else {
        panic!("the 1h prefix outlives the 5m tail");
    };
    assert!((usd - 8_000.0 * 2.0 * 4.0 / 1e6).abs() < 1e-12, "{usd}");
    assert_eq!(
        line(&t, &five, "claude-opus-5-5", Scope::Exact, 3611),
        expired
    );
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    assert_eq!(
        Tracker::new(&f)
            .rebuild(&f, "claude-opus-5-5", Scope::Exact, s(0))
            .line(UNI),
        "nothing cached yet, so nothing to rebuild"
    );
}

#[test]
fn every_cache_effect_reads_as_its_own_note() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    feed(
        &mut t,
        &f,
        &[step(first_request(8_000, 0)), step(later_request(10_000))],
    );
    let rebuilt = |reason| CacheEffect::Rebuilt { reason };
    let answers = feed(
        &mut t,
        &f,
        &[
            changed("claude-opus-5-5", Effort::Low, CacheEffect::Kept),
            changed("claude-opus-5-5", Effort::High, CacheEffect::KeptIfPassedOn),
            changed(
                "claude-sonnet-5-5",
                Effort::High,
                rebuilt(RebuildReason::ModelSwitch),
            ),
            changed(
                "claude-opus-4-8",
                Effort::Low,
                rebuilt(RebuildReason::ModelSwitch),
            ),
            changed(
                "claude-opus-4-8",
                Effort::Max,
                rebuilt(RebuildReason::EffortChange),
            ),
            changed("claude-opus-4-8", Effort::Max, CacheEffect::Kept),
        ],
    );
    let notes: Vec<String> = answers.iter().map(note).collect();
    assert_eq!(
        notes,
        [
            format!("Effort \u{2192} low \u{b7} {CACHE_KEPT}").as_str(),
            format!("Effort \u{2192} high \u{b7} {CACHE_KEPT_EFFORT}").as_str(),
            "Switched to claude-api \u{b7} API \u{b7} sonnet-5-5 high \u{b7} cache rebuild ~$0.04",
            "Switched to claude-api \u{b7} API \u{b7} opus-4-8 low \u{b7} cache rebuild ~$0.10",
            "Effort \u{2192} max \u{b7} cache rebuild up to ~$0.10",
            format!("Settings unchanged: opus-4-8 \u{b7} max \u{b7} {CACHE_KEPT}").as_str(),
        ]
    );
}

#[test]
fn a_running_turn_keeps_its_settings_and_shows_the_next_ones_as_pending() {
    for takes_effort in [true, false] {
        let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
        if !takes_effort {
            with_levels(&mut f, "claude-sonnet-5-5", &[], Effort::Low);
        }
        let mut t = Tracker::new(&f);
        let switch = CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        };
        feed(
            &mut t,
            &f,
            &[
                AgentEvent::turn_started(1),
                changed("claude-sonnet-5-5", Effort::Low, switch),
            ],
        );
        assert!(!t.show(&mut f));
        assert_eq!(f.model, "claude-opus-5-5");
        assert_eq!(
            f.pending(&Glyphs::of(UNI)).as_deref(),
            Some(if takes_effort {
                "\u{2192} sonnet-5-5 \u{b7} low next turn"
            } else {
                "\u{2192} sonnet-5-5 next turn"
            })
        );
        feed(&mut t, &f, &[completed(true), AgentEvent::turn_started(1)]);
        assert!(t.show(&mut f));
        assert_eq!(
            (f.model.as_str(), f.effort.as_str()),
            ("claude-sonnet-5-5", if takes_effort { "low" } else { "" })
        );
        assert_eq!(f.next, None);
        assert_eq!(t.next().effort, Some(Effort::Low));
        t.asked_settings(ASKED, None, Some("claude-opus-5-5"), None);
        assert_eq!(t.wanted().effort, Some(Effort::Low));
        t.apply(
            &changed("claude-opus-5-5", Effort::Low, CacheEffect::Kept),
            &f,
            s(2),
            UNI,
        );
        t.apply(&completed(true), &f, s(3), UNI);
        t.show(&mut f);
        assert_eq!(f.effort, "low");
    }
}

#[test]
fn a_switch_made_during_a_turn_is_noted_at_once_and_not_again_at_its_end() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    let switch = CacheEffect::Rebuilt {
        reason: RebuildReason::ModelSwitch,
    };
    let answers = feed(
        &mut t,
        &f,
        &[
            AgentEvent::turn_started(1),
            changed("claude-sonnet-5-5", Effort::Medium, switch.clone()),
            changed("claude-haiku-5-5", Effort::Medium, switch),
            step(first_request(10_000, 0)),
            completed(true),
        ],
    );
    assert_eq!(
        note(&answers[1]),
        "Switching to claude-api \u{b7} API \u{b7} sonnet-5-5 medium \u{b7} nothing cached yet, so nothing to rebuild | From your next message."
    );
    assert!(note(&answers[2]).starts_with("Switching to claude-api \u{b7} API \u{b7} haiku-5-5"));
    assert_eq!(answers[4], Answer::Other);
}

#[test]
fn a_refused_change_is_told_apart_from_a_refused_message() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    let refused = AgentEvent::refused(
        ASKED,
        "claude-opus-4-8 doesn't take effort xhigh".to_owned(),
    );
    let message = AgentEvent::Rejected {
        request: None,
        reason: "32 messages are already waiting. Send this after the turn.".to_owned(),
        id: Some(InputId("tui-1".to_owned())),
        kind: Some(RejectKind::TooMany),
    };
    t.asked_settings(ASKED, None, None, Some(Effort::Xhigh));
    assert_eq!(t.wanted().effort, Some(Effort::Xhigh));
    let answers = feed(&mut t, &f, &[message, refused]);
    assert_eq!(answers[0], Answer::Other);
    assert_eq!(
        note(&answers[1]),
        "Settings not changed: claude-opus-4-8 doesn't take effort xhigh."
    );
    assert_eq!(t.wanted(), *t.next());
}

#[test]
fn a_refused_answer_is_never_paired_with_a_settings_request() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    let stale = AgentEvent::Rejected {
        request: None,
        reason: "no question toolu_1 is open".to_owned(),
        id: None,
        kind: Some(RejectKind::NoQuestion),
    };
    let bad = AgentEvent::Rejected {
        request: None,
        reason: "answer for toolu_1 refused: answer 1's own text is blank".to_owned(),
        id: None,
        kind: Some(RejectKind::BadAnswer),
    };
    let refused = AgentEvent::refused(
        ASKED,
        "claude-opus-4-8 doesn't take effort xhigh".to_owned(),
    );
    t.asked_settings(ASKED, None, None, Some(Effort::Xhigh));
    let answers = feed(&mut t, &f, &[stale, bad]);
    assert_eq!(answers, [Answer::Other, Answer::Other]);
    assert_eq!(
        t.wanted().effort,
        Some(Effort::Xhigh),
        "still waiting for the kernel"
    );
    let answers = feed(&mut t, &f, &[refused]);
    assert_eq!(
        note(&answers[0]),
        "Settings not changed: claude-opus-4-8 doesn't take effort xhigh."
    );
    assert_eq!(t.wanted(), *t.next());
}

#[test]
fn the_effort_rows_mirror_the_kernels_cache_rule() {
    let f = facts(Endpoint::Custom, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    feed(&mut t, &f, &[step(later_request(10_000))]);
    let effect = |t: &Tracker, model: &str, level| t.effort_effect(&f, model, level, s(2));
    assert_eq!(effect(&t, "claude-opus-5-5", Effort::Medium), Effect::Kept);
    assert_eq!(
        effect(&t, "claude-opus-5-5", Effort::High),
        Effect::KeptIfPassedOn
    );
    assert_eq!(
        effect(&t, "claude-sonnet-5-5", Effort::High),
        Effect::WithSwitch
    );
    let mut opus48 = f.clone();
    opus48.model = "claude-opus-4-8".to_owned();
    let t48 = Tracker::new(&opus48);
    assert!(matches!(
        t48.effort_effect(&opus48, "claude-opus-4-8", Effort::Low, s(0)),
        Effect::Rebuild(Rebuild::Empty)
    ));
}

#[test]
fn returning_to_the_running_model_shows_the_effect_of_the_effort_it_would_send() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    f.model = "claude-opus-4-8".to_owned();
    let mut t = Tracker::new(&f);
    feed(
        &mut t,
        &f,
        &[
            step(first_request(8_000, 0)),
            step(later_request(10_000)),
            changed(
                "claude-opus-4-8",
                Effort::High,
                CacheEffect::Rebuilt {
                    reason: RebuildReason::EffortChange,
                },
            ),
            changed(
                "claude-sonnet-5-5",
                Effort::High,
                CacheEffect::Rebuilt {
                    reason: RebuildReason::ModelSwitch,
                },
            ),
        ],
    );
    assert_eq!(
        t.model_effect(&f, "claude-opus-4-8", s(2)).text(UNI),
        "cache rebuild up to ~$0.10"
    );
    let mut gateway = facts(Endpoint::Custom, CacheTtl::OneHour);
    let mut t = Tracker::new(&gateway);
    feed(
        &mut t,
        &gateway,
        &[
            changed("claude-opus-5-5", Effort::High, CacheEffect::KeptIfPassedOn),
            changed(
                "claude-sonnet-5-5",
                Effort::High,
                CacheEffect::Rebuilt {
                    reason: RebuildReason::ModelSwitch,
                },
            ),
        ],
    );
    assert_eq!(
        t.model_effect(&gateway, "claude-opus-5-5", s(2)),
        Effect::KeptIfPassedOn
    );
    gateway.model = "claude-sonnet-4-6".to_owned();
    gateway.effort = "high".to_owned();
    let mut t = Tracker::new(&gateway);
    feed(
        &mut t,
        &gateway,
        &[changed(
            "claude-opus-5-5",
            Effort::Xhigh,
            CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            },
        )],
    );
    assert_eq!(
        t.model_effect(&gateway, "claude-sonnet-4-6", s(2)),
        Effect::Kept
    );
}

#[test]
fn a_first_request_that_reads_the_shared_prefix_counts_it_as_prefix() {
    let five = facts(Endpoint::Official, CacheTtl::FiveMinutes);
    let mut t = Tracker::new(&five);
    let first = Usage {
        input_tokens: 10,
        cache_read_input_tokens: 8_000,
        cache_creation_input_tokens: 2_000,
        cache_creation_1h_input_tokens: Some(0),
        cache_creation_5m_input_tokens: Some(2_000),
        ..Usage::default()
    };
    feed(&mut t, &five, &[step(first), step(later_request(12_000))]);
    assert_eq!(
        t.cache_size(),
        CacheSize {
            prefix: 8_000,
            total: 12_000
        }
    );
    let Rebuild::Usd(usd) = t.rebuild(&five, "claude-opus-5-5", Scope::Exact, s(2)) else {
        panic!("not priced");
    };
    let expected = (8_000.0 * 2.0 + 4_000.0 * 1.25) * 4.0 / 1e6;
    assert!((usd - expected).abs() < 1e-12, "{usd}");
}

#[test]
fn a_switch_to_a_model_with_a_lower_output_cap_says_so() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    for m in &mut f.models {
        if m.id == "claude-sonnet-5-5"
            && let Some(caps) = m.caps.as_mut()
        {
            caps.max_output = Some(32_000);
        }
        if m.id == "claude-opus-4-8"
            && let Some(caps) = m.caps.as_mut()
        {
            caps.max_output = None;
        }
    }
    let mut t = Tracker::new(&f);
    let switch = CacheEffect::Rebuilt {
        reason: RebuildReason::ModelSwitch,
    };
    let answers = feed(
        &mut t,
        &f,
        &[
            changed("claude-sonnet-5-5", Effort::Medium, switch.clone()),
            changed("claude-sonnet-5-5", Effort::Low, CacheEffect::Kept),
            changed("claude-opus-4-8", Effort::Low, switch.clone()),
            changed("claude-haiku-5-5", Effort::Low, switch),
        ],
    );
    let notes: Vec<String> = answers.iter().map(note).collect();
    assert_eq!(
        notes,
        [
            "Switched to claude-api \u{b7} API \u{b7} sonnet-5-5 medium \u{b7} nothing cached yet, so nothing to rebuild | output capped at 32k tokens",
            format!("Effort \u{2192} low \u{b7} {CACHE_KEPT}").as_str(),
            "Switched to claude-api \u{b7} API \u{b7} opus-4-8 low \u{b7} nothing cached yet, so nothing to rebuild",
            "Switched to claude-api \u{b7} API \u{b7} haiku-5-5 low \u{b7} nothing cached yet, so nothing to rebuild",
        ]
    );
    let mut t = Tracker::new(&f);
    t.picks_start();
    let switch = CacheEffect::Rebuilt {
        reason: RebuildReason::ModelSwitch,
    };
    let answers = feed(
        &mut t,
        &f,
        &[
            changed("claude-opus-4-8", Effort::Low, switch.clone()),
            changed("claude-haiku-5-5", Effort::Low, switch),
        ],
    );
    let notes: Vec<String> = answers.iter().map(note).collect();
    assert_eq!(
        notes,
        [
            "Started on claude-api \u{b7} API \u{b7} opus-4-8 low",
            "Switched to claude-api \u{b7} API \u{b7} haiku-5-5 low \u{b7} nothing cached yet, so nothing to rebuild",
        ]
    );
}

#[test]
fn an_effort_change_in_a_one_hour_session_is_bounded_by_the_whole_prompt() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    f.model = "claude-opus-4-8".to_owned();
    let mut t = Tracker::new(&f);
    feed(
        &mut t,
        &f,
        &[
            AgentEvent::turn_started(1),
            step(first_request(5_000, 0)),
            completed(true),
        ],
    );
    let effect = t.effort_effect(&f, "claude-opus-4-8", Effort::High, s(2));
    let Effect::Rebuild(Rebuild::UpToUsd(usd)) = effect else {
        panic!("{effect:?}");
    };
    assert!((usd - 5_010.0 * 2.0 * 5.0 / 1e6).abs() < 1e-12, "{usd}");
    assert_eq!(effect.text(UNI), "cache rebuild up to ~$0.0501");
    assert_eq!(
        t.cycle(&f, s(2), UNI).unwrap_err(),
        "Changing effort on opus-4-8 rebuilds the cache (up to ~$0.0501). Use /effort to choose."
    );
    let answers = feed(
        &mut t,
        &f,
        &[changed(
            "claude-opus-4-8",
            Effort::High,
            CacheEffect::Rebuilt {
                reason: RebuildReason::EffortChange,
            },
        )],
    );
    assert_eq!(
        note(&answers[0]),
        "Effort \u{2192} high \u{b7} cache rebuild up to ~$0.0501"
    );
}

fn switched(endpoint: &str, model: &str, effort: Effort, bound: &[&str]) -> AgentEvent {
    settings_changed(Changed {
        answers: Some(ASKED),
        endpoint: endpoint.to_owned(),
        model: model.to_owned(),
        effort,
        cache: CacheEffect::Rebuilt {
            reason: RebuildReason::EndpointSwitch,
        },
        account_bound: bound.iter().map(|m| (*m).to_owned()).collect(),
        ..Changed::default()
    })
}

fn warm(t: &mut Tracker, f: &Facts) {
    feed(
        t,
        f,
        &[
            AgentEvent::turn_started(1),
            step(first_request(10_000, 0)),
            step(later_request(12_000)),
            completed(true),
        ],
    );
}

#[test]
fn an_endpoint_switch_is_priced_as_a_whole_prompt_at_the_target_endpoint() {
    let mut f = endpoint_facts();
    let mut t = Tracker::new(&f);
    assert_eq!(t.session().endpoint, "manifest");
    warm(&mut t, &f);
    assert_eq!(t.endpoint_effect(&f, "manifest", s(1)), Effect::Current);
    assert_eq!(t.endpoint_effect(&f, "work", s(1)), Effect::Unavailable);
    let Effect::Rebuild(rebuild) = t.endpoint_effect(&f, "direct", s(1)) else {
        panic!("a switch rebuilds");
    };
    assert_eq!(rebuild.line(UNI), "cache rebuild ~$0.096");
    f.endpoints[0].priced = false;
    assert_eq!(
        t.endpoint_effect(&f, "direct", s(1)).text(UNI),
        "cache rebuild, no prices"
    );
    assert_eq!(
        t.endpoint_effect(&f, "direct", s(7200)).text(UNI),
        "cache expired"
    );
    let direct = f.endpoints.iter_mut().find(|e| e.name == "direct").unwrap();
    direct.models = vec![wisp_protocol::settings::AccountModel {
        id: "claude-sonnet-5-5".into(),
        listed: Some(0),
    }];
    direct.default_model = Some("claude-sonnet-5-5".into());
    direct.complete = true;
    assert_eq!(
        t.endpoint_effect(&f, "direct", s(1)),
        t.pair_effect(&f, "direct", "claude-sonnet-5-5", s(1))
    );
    t.apply(
        &switched("direct", "claude-sonnet-5-5", Effort::Medium, &[]),
        &f,
        s(2),
        UNI,
    );
    let manifest = f
        .endpoints
        .iter_mut()
        .find(|e| e.name == "manifest")
        .unwrap();
    manifest.models = vec![wisp_protocol::settings::AccountModel {
        id: "claude-haiku-5-5".into(),
        listed: Some(0),
    }];
    manifest.default_model = Some("claude-haiku-5-5".into());
    manifest.complete = true;
    assert_eq!(
        t.endpoint_effect(&f, "manifest", s(2)),
        t.pair_effect(&f, "manifest", "claude-haiku-5-5", s(2))
    );
}

#[test]
fn an_idle_switch_shows_its_account_in_the_status_row_at_once_and_notes_the_rebuild() {
    let mut f = endpoint_facts();
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    t.asked_settings(ASKED, Some("direct"), None, None);
    assert_eq!(t.wanted().endpoint, "direct");
    let answer = t.apply(
        &switched("direct", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(1),
        UNI,
    );
    assert_eq!(
        note(&answer),
        "Switched to direct \u{b7} API \u{b7} opus-5-5 medium \u{b7} cache rebuild ~$0.096"
    );
    t.show(&mut f);
    assert_eq!(f.next, None);
    assert_eq!(f.endpoint_name.as_deref(), Some("direct"));
    assert_eq!(
        f.credential_label(
            usize::MAX,
            &Glyphs::of(UNI),
            wisp_render::text::WidthMode::Normal
        ),
        format!("direct \u{b7} {}", AccountKind::ClaudeApi.label())
    );
    let answer = t.apply(
        &switched(
            "direct",
            "claude-sonnet-5-5",
            Effort::Medium,
            &["claude-sonnet-5-5"],
        ),
        &f,
        s(1),
        UNI,
    );
    assert_eq!(
        note(&answer),
        "Switched to direct \u{b7} API \u{b7} sonnet-5-5 medium \u{b7} cache rebuild ~$0.048 | earlier Sonnet 5.5 reasoning may not carry over"
    );
    assert!(t.show(&mut f));
    assert_eq!(f.next, None);
    assert_eq!(f.model, "claude-sonnet-5-5");
}

#[test]
fn notes_name_every_account_bound_model() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    let answer = t.apply(
        &switched(
            "gateway",
            "claude-opus-5-5",
            Effort::Medium,
            &["claude-haiku-5-5", "claude-sonnet-5-5"],
        ),
        &f,
        s(1),
        UNI,
    );
    assert_eq!(
        note(&answer),
        "Switched to gateway \u{b7} API \u{b7} opus-5-5 medium \u{b7} nothing cached yet, so nothing to rebuild | earlier Haiku 5.5 and Sonnet 5.5 reasoning may not carry over"
    );
}

#[test]
fn a_switch_while_a_turn_runs_is_noted_at_once() {
    let mut f = endpoint_facts();
    let mut t = Tracker::new(&f);
    t.apply(&AgentEvent::turn_started(1), &f, s(0), UNI);
    let answer = t.apply(
        &switched("direct", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(1),
        UNI,
    );
    assert!(
        note(&answer).starts_with("Switching to direct"),
        "{answer:?}"
    );
    assert!(
        note(&answer).ends_with("From your next message."),
        "{answer:?}"
    );
    t.show(&mut f);
    assert_eq!(
        f.pending_chip().map(|chip| chip.name).as_deref(),
        Some("direct")
    );
    assert_eq!(f.endpoint_name.as_deref(), Some("manifest"));
    let answer = t.apply(&completed(true), &f, s(2), UNI);
    assert_eq!(answer, Answer::Other);
    t.show(&mut f);
    assert!(f.pending_chip().is_none());
    assert_eq!(f.endpoint_name.as_deref(), Some("direct"));
}

#[test]
fn pair_states_are_kept_per_endpoint() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    let proven = AgentEvent::FeatureStateChanged {
        endpoint: "manifest".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        feature: Feature::PerMessageEffort,
        state: FeatureState::Proven,
    };
    t.apply(&proven, &f, s(0), UNI);
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Proven);
    assert_eq!(t.pair_on(&f, "gateway", "claude-opus-5-5"), Pair::Unknown);
    assert_eq!(t.pair_on(&f, "direct", "claude-opus-5-5"), Pair::Documented);
    t.apply(
        &switched("gateway", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(0),
        UNI,
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Unknown);
    assert_eq!(
        t.effort_effect(&f, "claude-opus-5-5", Effort::High, s(0)),
        Effect::WithEndpointSwitch
    );
    assert_eq!(
        t.model_effect(&f, "claude-sonnet-5-5", s(0)),
        Effect::WithEndpointSwitch
    );
    assert!(t.cycle(&f, s(0), UNI).is_err());
    t.apply(&AgentEvent::turn_started(1), &f, s(0), UNI);
    t.apply(&completed(true), &f, s(0), UNI);
    t.apply(
        &switched("manifest", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(0),
        UNI,
    );
    assert_eq!(t.pair(&f, "claude-opus-5-5"), Pair::Proven);
}

#[test]
fn switching_back_to_the_running_endpoint_costs_only_the_pending_change() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    t.asked_settings(ASKED, Some("direct"), None, None);
    assert_eq!(t.endpoint_effect(&f, "manifest", s(0)), Effect::Kept);
    t.asked_settings(ASKED, None, Some("claude-sonnet-5-5"), None);
    assert!(matches!(
        t.endpoint_effect(&f, "manifest", s(0)),
        Effect::Rebuild(_)
    ));
    for effort in [Effort::Medium, Effort::High] {
        let mut t = Tracker::new(&f);
        warm(&mut t, &f);
        t.apply(
            &AgentEvent::FeatureStateChanged {
                endpoint: "manifest".into(),
                model: f.model.clone(),
                feature: Feature::PerMessageEffort,
                state: FeatureState::FallenBack,
            },
            &f,
            s(0),
            UNI,
        );
        t.apply(
            &switched("direct", &f.model, Effort::Medium, &[]),
            &f,
            s(0),
            UNI,
        );
        let snapshot = switch::snapshot_of(vec![wisp_protocol::settings::SwitchQuote {
            target: SwitchTarget::Endpoint("manifest".into()),
            remembered: Some(wisp_protocol::settings::Remembered {
                model: f.model.clone(),
                effort,
            }),
            consequence: None,
            retry_compaction: None,
        }]);
        t.apply(&snapshot, &f, s(0), UNI);
        assert_eq!(
            super::super::picker::shown_pair(&f, &t, "manifest").effort,
            Some(effort)
        );
        let effect = t.endpoint_effect(&f, "manifest", s(0));
        if effort == Effort::Medium {
            assert_eq!(effect, Effect::Kept);
        } else {
            assert!(matches!(effect, Effect::Rebuild(_)), "{effect:?}");
        }
    }
}

#[test]
fn an_effort_row_on_an_unpriced_endpoint_keeps_up_to() {
    let mut f = endpoint_facts();
    f.endpoints[2].priced = false;
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    t.apply(
        &AgentEvent::FeatureStateChanged {
            endpoint: "manifest".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            feature: Feature::PerMessageEffort,
            state: FeatureState::FallenBack,
        },
        &f,
        s(1),
        UNI,
    );
    assert_eq!(
        t.effort_effect(&f, "claude-opus-5-5", Effort::High, s(1))
            .text(UNI),
        "cache rebuild up to the whole prompt, no prices"
    );
    assert_eq!(
        t.endpoint_effect(&f, "direct", s(1)).text(UNI),
        "cache rebuild ~$0.096"
    );
}

#[test]
fn the_effort_fallback_is_the_nearest_level_below_or_else_the_lowest() {
    let mixed = [Effort::Max, Effort::Low, Effort::High];
    assert_eq!(
        fallback_effort(&mixed, Some(Effort::Xhigh)),
        Some(Effort::High)
    );
    assert_eq!(
        fallback_effort(&mixed, Some(Effort::Medium)),
        Some(Effort::Low)
    );
    assert_eq!(
        fallback_effort(&mixed, Some(Effort::Max)),
        Some(Effort::High)
    );
    assert_eq!(
        fallback_effort(&mixed, Some(Effort::Low)),
        Some(Effort::Low)
    );
    assert_eq!(fallback_effort(&mixed, None), Some(Effort::Low));
    let upper = [Effort::Max, Effort::High];
    assert_eq!(
        fallback_effort(&upper, Some(Effort::Low)),
        Some(Effort::High)
    );
    assert_eq!(fallback_effort(&upper, None), Some(Effort::High));
    assert_eq!(fallback_effort(&[], Some(Effort::High)), None);
}

#[test]
fn shift_tab_from_an_unsupported_level_follows_one_rule_whatever_the_api_default() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    with_levels(
        &mut f,
        "claude-haiku-5-5",
        &[Effort::Max, Effort::High, Effort::Low],
        Effort::Max,
    );
    with_levels(
        &mut f,
        "claude-opus-5-5",
        &[Effort::High, Effort::Medium, Effort::Low],
        Effort::Low,
    );
    f.effort = "xhigh".to_owned();
    for model in ["claude-haiku-5-5", "claude-opus-5-5"] {
        f.model = model.to_owned();
        let t = Tracker::new(&f);
        let (level, hint) = t.cycle(&f, s(0), UNI).unwrap();
        assert_eq!(level, Effort::High, "{model}");
        assert!(hint.starts_with("effort xhigh \u{2192} high"), "{hint}");
    }
}

#[test]
fn shift_tab_with_no_lower_level_lands_on_the_lowest_supported() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    with_levels(
        &mut f,
        "claude-opus-5-5",
        &[Effort::Max, Effort::High, Effort::Medium],
        Effort::High,
    );
    f.effort = "low".to_owned();
    let t = Tracker::new(&f);
    assert_eq!(t.cycle(&f, s(0), UNI).unwrap().0, Effort::Medium);
}

#[test]
fn shift_tab_with_no_current_level_lands_on_the_lowest_supported() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    with_levels(
        &mut f,
        "claude-haiku-5-5",
        &[Effort::Max, Effort::High, Effort::Medium],
        Effort::Max,
    );
    with_levels(
        &mut f,
        "claude-opus-5-5",
        &[Effort::High, Effort::Medium, Effort::Max],
        Effort::High,
    );
    f.effort = String::new();
    for model in ["claude-haiku-5-5", "claude-opus-5-5"] {
        f.model = model.to_owned();
        let t = Tracker::new(&f);
        let (level, hint) = t.cycle(&f, s(0), UNI).unwrap();
        assert_eq!(level, Effort::Medium, "{model}");
        assert!(hint.starts_with("effort ? \u{2192} medium"), "{hint}");
    }
}

fn search_refused_on(endpoint: &str, model: &str) -> AgentEvent {
    AgentEvent::FeatureStateChanged {
        endpoint: endpoint.to_owned(),
        model: model.to_owned(),
        feature: Feature::NativeWebSearch,
        state: FeatureState::FallenBack,
    }
}

fn refusal(endpoint: &str, model: &str) -> AgentEvent {
    settings_changed(Changed {
        endpoint: endpoint.to_owned(),
        model: model.to_owned(),
        effort: Effort::Medium,
        cache: CacheEffect::Rebuilt {
            reason: RebuildReason::NativeSearchRefused,
        },
        ..Changed::default()
    })
}

const REFUSED_ON_MANIFEST: &str = "manifest refused the web search tool; continuing without web search. To stop sending it: wisp config set endpoints.manifest.native_web_search off";

#[test]
fn a_mid_turn_search_refusal_notes_at_once_and_leaves_a_pending_answer_paired() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    t.asked_settings(ASKED, None, Some("claude-sonnet-5-5"), None);
    let answers = feed(
        &mut t,
        &f,
        &[
            AgentEvent::turn_started(1),
            search_refused_on("manifest", "claude-opus-5-5"),
            refusal("manifest", "claude-opus-5-5"),
        ],
    );
    assert_eq!(answers[1], Answer::Other);
    let refused = note(&answers[2]);
    assert!(refused.starts_with(REFUSED_ON_MANIFEST), "{refused}");
    assert_eq!(t.wanted().model, "claude-sonnet-5-5");
    let answers = feed(
        &mut t,
        &f,
        &[
            changed_on("manifest", "claude-sonnet-5-5", RebuildReason::ModelSwitch),
            completed(true),
        ],
    );
    assert!(
        note(&answers[0]).starts_with("Switching to manifest \u{b7} API \u{b7} sonnet-5-5"),
        "{answers:?}"
    );
    assert_eq!(answers[1], Answer::Other);
    assert_eq!(t.next().model, "claude-sonnet-5-5");
    assert_eq!(t.wanted().model, "claude-sonnet-5-5");
}

fn changed_on(endpoint: &str, model: &str, reason: RebuildReason) -> AgentEvent {
    settings_changed(Changed {
        answers: answering(&CacheEffect::Rebuilt {
            reason: reason.clone(),
        }),
        endpoint: endpoint.to_owned(),
        model: model.to_owned(),
        effort: Effort::Medium,
        cache: CacheEffect::Rebuilt { reason },
        ..Changed::default()
    })
}

#[test]
fn a_search_refusal_with_an_endpoint_switch_pending_names_the_refused_endpoint() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    let answers = feed(
        &mut t,
        &f,
        &[
            AgentEvent::turn_started(1),
            changed_on("direct", "claude-opus-5-5", RebuildReason::EndpointSwitch),
            search_refused_on("manifest", "claude-opus-5-5"),
            refusal("direct", "claude-opus-5-5"),
        ],
    );
    assert!(
        note(&answers[1]).starts_with("Switching to direct"),
        "{answers:?}"
    );
    let refused = note(&answers[3]);
    assert!(refused.starts_with(REFUSED_ON_MANIFEST), "{refused}");
    assert_eq!(t.next().endpoint, "direct");
    let answers = feed(&mut t, &f, &[completed(true)]);
    assert_eq!(answers[0], Answer::Other);
}

#[test]
fn a_search_refusal_between_turns_is_noted_at_once() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    let answers = feed(
        &mut t,
        &f,
        &[
            search_refused_on("manifest", "claude-opus-5-5"),
            refusal("manifest", "claude-opus-5-5"),
        ],
    );
    let refused = note(&answers[1]);
    assert!(refused.starts_with(REFUSED_ON_MANIFEST), "{refused}");
    assert_eq!(t.next().endpoint, "manifest");
}

#[test]
fn a_search_refusal_is_priced_on_the_refused_endpoint_not_the_pending_one() {
    let mut f = endpoint_facts();
    f.endpoints[0].priced = false;
    let mut t = Tracker::new(&f);
    warm(&mut t, &f);
    let answers = feed(
        &mut t,
        &f,
        &[
            AgentEvent::turn_started(2),
            changed_on("direct", "claude-opus-5-5", RebuildReason::EndpointSwitch),
            search_refused_on("manifest", "claude-opus-5-5"),
            refusal("direct", "claude-opus-5-5"),
        ],
    );
    let refused = note(&answers[3]);
    assert!(refused.starts_with(REFUSED_ON_MANIFEST), "{refused}");
    assert!(
        refused.ends_with("cache rebuild up to ~$0.096"),
        "{refused}"
    );
    assert!(!refused.contains("no prices"), "{refused}");
    assert!(note(&answers[1]).contains("no prices"), "{answers:?}");
}

#[test]
fn each_answer_pops_only_the_request_id_it_names() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_settings(RequestId(1), None, None, Some(Effort::High));
    t.asked_settings(RequestId(2), None, None, Some(Effort::Low));
    let refused = AgentEvent::refused(RequestId(2), "not now");
    assert_eq!(
        note(&t.apply(&refused, &f, s(1), UNI)),
        "Settings not changed: not now."
    );
    assert_eq!(t.wanted().effort, Some(Effort::High));
    let mut answered = changed("claude-opus-5-5", Effort::High, CacheEffect::KeptIfPassedOn);
    if let AgentEvent::SettingsChanged { answers, .. } = &mut answered {
        *answers = Some(RequestId(1));
    }
    t.apply(&answered, &f, s(2), UNI);
    assert_eq!(t.wanted(), *t.next());
    assert_eq!(t.next().effort, Some(Effort::High));
}

#[test]
fn an_unknown_request_id_pops_nothing() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut t = Tracker::new(&f);
    t.asked_settings(RequestId(1), None, None, Some(Effort::High));
    let stray = AgentEvent::refused(RequestId(9), "not now");
    assert_eq!(t.apply(&stray, &f, s(1), UNI), Answer::Other);
    assert_eq!(t.wanted().effort, Some(Effort::High));
}

fn thought() -> Vec<AgentEvent> {
    vec![
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        step(later_request(12_000)),
    ]
}

fn tools_changed(removed: &[&str]) -> AgentEvent {
    AgentEvent::ToolsChanged {
        added: Vec::new(),
        removed: removed.iter().map(|n| (*n).to_owned()).collect(),
        via: wisp_protocol::ToolChangeVia::Rebuild,
        cache: None,
    }
}

const TO_NATIVE: &str = "web search: off \u{2192} on";
const SWITCHED_OFF: &str = "web search: off (not on this account)";
const TURNED_OFF: &str = "web search: off (off for this account)";

#[test]
fn a_switch_to_a_pair_of_the_other_search_form_says_so_in_its_note() {
    let f = crate::app::fixture::search_facts("manifest");
    let mut t = Tracker::new(&f);
    let answer = t.apply(
        &switched("direct", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(1),
        UNI,
    );
    assert_eq!(
        note(&answer),
        format!(
            "Switched to direct \u{b7} API \u{b7} opus-5-5 medium \u{b7} nothing cached yet, so nothing to rebuild | {TO_NATIVE}"
        )
    );
    let answer = t.apply(
        &switched("gateway", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(1),
        UNI,
    );
    assert!(!note(&answer).contains("web search"), "{answer:?}");
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(thought());
    events.push(completed(true));
    feed(&mut t, &f, &events);
    let answer = t.apply(
        &switched("direct", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(1),
        GlyphSet::Ascii,
    );
    assert!(
        note(&answer).ends_with("web search: off -> on - earlier reasoning dropped"),
        "{answer:?}"
    );
}

#[test]
fn a_switch_while_a_turn_runs_names_the_search_change_at_once() {
    let f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    feed(&mut t, &f, &[AgentEvent::turn_started(1)]);
    let answer = t.apply(
        &changed_on("direct", "claude-sonnet-4-6", RebuildReason::ModelSwitch),
        &f,
        s(1),
        UNI,
    );
    assert!(
        note(&answer).ends_with("| web search: on \u{2192} off (not on this model)"),
        "{answer:?}"
    );
    let mut events = thought();
    events.push(completed(true));
    let answers = feed(&mut t, &f, &events);
    assert_eq!(answers[2], Answer::Other);
}

const OFF_HERE: &str = "web search: on \u{2192} off (off for this account)";

#[test]
fn a_switch_to_a_gateway_with_search_turned_off_gives_the_reason_in_its_note() {
    let f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    let answer = t.apply(
        &switched("manifest", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(1),
        UNI,
    );
    assert_eq!(
        note(&answer),
        format!(
            "Switched to manifest \u{b7} API \u{b7} opus-5-5 medium \u{b7} nothing cached yet, so nothing to rebuild | {OFF_HERE}"
        )
    );
    assert_eq!(
        t.search_note(&f, "manifest", "claude-opus-5-5", UNI)
            .as_deref(),
        Some("web search: on \u{2192} off")
    );
}

#[test]
fn a_tools_change_leaves_no_earlier_reasoning_to_drop() {
    let f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    let drops = |t: &Tracker| t.drops_reasoning(&f, "manifest", "claude-opus-5-5");
    assert!(!drops(&t));
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(thought());
    events.push(completed(true));
    feed(&mut t, &f, &events);
    assert!(drops(&t));
    assert!(!t.drops_reasoning(&f, "direct", "claude-haiku-5-5"));
    assert_eq!(
        t.search_note(&f, "manifest", "claude-opus-5-5", UNI)
            .as_deref(),
        Some("web search: on \u{2192} off")
    );
    feed(&mut t, &f, &[tools_changed(&["one"])]);
    assert!(!drops(&t));
    feed(
        &mut t,
        &f,
        &[
            AgentEvent::turn_started(2),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::RedactedThinking,
            },
            completed(false),
        ],
    );
    assert!(!drops(&t));
}

#[test]
fn an_inline_or_noted_tools_change_keeps_the_earlier_reasoning() {
    let f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(thought());
    events.push(completed(true));
    feed(&mut t, &f, &events);
    assert!(t.drops_reasoning(&f, "manifest", "claude-opus-5-5"));
    for via in [
        wisp_protocol::ToolChangeVia::Inline,
        wisp_protocol::ToolChangeVia::TurnedOff,
    ] {
        let kept = AgentEvent::ToolsChanged {
            added: Vec::new(),
            removed: vec!["one".into()],
            via,
            cache: Some(wisp_protocol::CacheEffect::Kept),
        };
        feed(&mut t, &f, &[kept]);
        assert!(
            t.drops_reasoning(&f, "manifest", "claude-opus-5-5"),
            "{via:?}"
        );
    }
}

fn stopped(committed_steps: u32) -> AgentEvent {
    ended(TurnOutcome::Cancelled {
        usage: None,
        cost: None,
        committed_steps,
    })
}

fn failed(committed_steps: u32) -> AgentEvent {
    ended(outcome(End {
        ending: Ending::Error(wisp_protocol::ErrorKind::Network),
        message: "connection reset".to_owned(),
        committed_steps,
        ..End::default()
    }))
}

#[test]
fn only_a_committed_steps_thinking_counts_as_reasoning_to_drop() {
    let f = crate::app::fixture::search_facts("direct");
    let drops = |t: &Tracker| t.drops_reasoning(&f, "manifest", "claude-opus-5-5");
    for end in [stopped(0), failed(0)] {
        let mut t = Tracker::new(&f);
        let mut events = vec![AgentEvent::turn_started(1)];
        events.extend(thought());
        feed(&mut t, &f, &events);
        assert!(drops(&t), "a running turn's thinking is expected to stay");
        feed(&mut t, &f, std::slice::from_ref(&end));
        assert!(!drops(&t), "{end:?}");
    }
    let mut t = Tracker::new(&f);
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(thought());
    events.push(stopped(1));
    feed(&mut t, &f, &events);
    assert!(drops(&t), "a stop after the reply was committed keeps it");
    for end in [stopped(1), failed(1)] {
        let mut t = Tracker::new(&f);
        let mut events = vec![
            AgentEvent::turn_started(1),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
            step(later_request(12_000)),
        ];
        events.extend(thought());
        events.push(end.clone());
        feed(&mut t, &f, &events);
        assert!(!drops(&t), "{end:?}");
    }
    let mut t = Tracker::new(&f);
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(thought());
    events.extend(thought());
    events.push(stopped(1));
    feed(&mut t, &f, &events);
    assert!(drops(&t), "the first step was committed before the stop");
    let mut later = vec![AgentEvent::turn_started(2)];
    later.extend(thought());
    later.push(stopped(0));
    feed(&mut t, &f, &later);
    assert!(drops(&t), "an earlier turn's thinking stays");
}

#[test]
fn a_tools_change_mid_turn_keeps_counting_steps_from_the_turns_start() {
    let f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    let text = AgentEvent::BlockStarted {
        index: 0,
        kind: BlockKind::Text,
    };
    let mut events = vec![
        AgentEvent::turn_started(1),
        text.clone(),
        step(later_request(12_000)),
        tools_changed(&["two"]),
        text,
        step(later_request(12_000)),
    ];
    events.extend(thought());
    events.push(stopped(2));
    feed(&mut t, &f, &events);
    assert!(!t.drops_reasoning(&f, "manifest", "claude-opus-5-5"));
}

#[test]
fn a_refusal_mid_session_moves_every_model_on_its_endpoint_to_no_search() {
    let mut f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    assert_eq!(t.search_note(&f, "direct", "claude-haiku-5-5", UNI), None);
    assert_eq!(f.search_line(&Glyphs::of(UNI)), None);
    let answers = feed(
        &mut t,
        &f,
        &[
            AgentEvent::turn_started(1),
            search_refused_on("direct", "claude-opus-5-5"),
            refusal("direct", "claude-opus-5-5"),
            tools_changed(&["two"]),
        ],
    );
    assert!(!note(&answers[2]).contains("web search:"), "{answers:?}");
    for model in ["claude-haiku-5-5", "claude-opus-5-5", "claude-sonnet-4-6"] {
        assert_eq!(
            t.search_note(&f, "direct", model, UNI).as_deref(),
            Some(SWITCHED_OFF),
            "{model}"
        );
    }
    assert_eq!(
        t.search_note(&f, "manifest", "claude-opus-5-5", UNI)
            .as_deref(),
        Some(TURNED_OFF)
    );
    assert!(t.show(&mut f));
    assert_eq!(f.search.refused, ["direct"]);
    assert_eq!(
        f.search_line(&Glyphs::of(UNI)).as_deref(),
        Some(SWITCHED_OFF)
    );
    assert!(!t.show(&mut f));
    let answer = t.apply(
        &switched("manifest", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(1),
        UNI,
    );
    assert!(
        note(&answer).starts_with("Switching to manifest"),
        "{answer:?}"
    );
    assert!(!note(&answer).contains("web search"), "{answer:?}");
}

#[test]
fn search_left_out_of_the_tools_adds_nothing_anywhere() {
    let mut f = crate::app::fixture::search_facts("direct");
    f.search.requested = false;
    let mut t = Tracker::new(&f);
    assert_eq!(t.search_note(&f, "manifest", "claude-opus-5-5", UNI), None);
    let answer = t.apply(
        &switched("manifest", "claude-opus-5-5", Effort::Medium, &[]),
        &f,
        s(1),
        UNI,
    );
    assert!(!note(&answer).contains("web search"), "{answer:?}");
    feed(
        &mut t,
        &f,
        &[search_refused_on("direct", "claude-opus-5-5")],
    );
    t.show(&mut f);
    assert_eq!(f.search_line(&Glyphs::of(UNI)), None);
}

mod kernel;
pub(crate) mod switch;

#[test]
fn a_search_refusal_without_its_feature_event_names_the_endpoint_it_ran_on() {
    let f = endpoint_facts();
    let mut t = Tracker::new(&f);
    let answers = feed(&mut t, &f, &[refusal("manifest", "claude-opus-5-5")]);
    let refused = note(&answers[0]);
    assert!(refused.starts_with(REFUSED_ON_MANIFEST), "{refused}");
    assert!(!refused.contains(" ? "), "{refused}");
}

#[test]
fn a_refusal_seeded_on_resume_reads_as_the_live_refusal_did() {
    let mut f_live = crate::app::fixture::search_facts("direct");
    let mut live = Tracker::new(&f_live);
    feed(
        &mut live,
        &f_live,
        &[
            AgentEvent::turn_started(1),
            search_refused_on("direct", "claude-opus-5-5"),
            refusal("direct", "claude-opus-5-5"),
            completed(true),
        ],
    );
    let mut f_resumed = crate::app::fixture::search_facts("direct");
    let mut resumed = Tracker::new(&f_resumed);
    resumed.search_refused_before("direct");
    resumed.show(&mut f_resumed);
    live.show(&mut f_live);
    assert_eq!(f_resumed.search.refused, ["direct"]);
    assert_eq!(f_resumed.search.refused, f_live.search.refused);
    assert_eq!(
        f_resumed.search_line(&Glyphs::of(UNI)),
        f_live.search_line(&Glyphs::of(UNI))
    );
    for endpoint in ["direct", "gateway", "manifest", "work"] {
        for model in ["claude-haiku-5-5", "claude-opus-5-5"] {
            assert_eq!(
                resumed.search_note(&f_resumed, endpoint, model, UNI),
                live.search_note(&f_live, endpoint, model, UNI),
                "{endpoint} {model}"
            );
        }
    }
    assert_eq!(
        resumed
            .search_note(&f_resumed, "direct", "claude-opus-5-5", UNI)
            .as_deref(),
        Some(SWITCHED_OFF)
    );
    let mut f_unseeded = crate::app::fixture::search_facts("manifest");
    let unseeded = Tracker::new(&f_unseeded);
    unseeded.show(&mut f_unseeded);
    assert_eq!(
        unseeded
            .search_note(&f_unseeded, "direct", "claude-opus-5-5", UNI)
            .as_deref(),
        Some(TO_NATIVE)
    );
}

#[test]
fn endpoint_switch_updates_the_current_provider_label() {
    let mut facts = endpoint_facts();
    facts
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.name == "direct")
        .unwrap()
        .provider = "openai".into();
    let mut tracker = Tracker::new(&facts);
    tracker.apply(
        &switched("direct", "gpt-6.1-sol", Effort::Medium, &[]),
        &facts,
        Duration::ZERO,
        UNI,
    );
    tracker.apply(&AgentEvent::turn_started(1), &facts, Duration::ZERO, UNI);
    assert!(tracker.show(&mut facts));
    assert_eq!(facts.provider, "OpenAI");
    tracker.apply(
        &switched("manifest", "claude-opus-5-5", Effort::Medium, &[]),
        &facts,
        Duration::ZERO,
        UNI,
    );
    tracker.apply(&AgentEvent::turn_started(2), &facts, Duration::ZERO, UNI);
    assert!(tracker.show(&mut facts));
    assert_eq!(facts.provider, "Anthropic");
}

#[test]
fn rebuild_uses_model_write_rates_and_endpoint_billing() {
    let mut facts = facts(Endpoint::Official, CacheTtl::OneHour);
    facts.credential = Some(wisp_render::chrome::Credential::ChatGptPlan);
    let rates = facts
        .models
        .iter_mut()
        .find(|m| m.id == "claude-opus-5-5")
        .unwrap()
        .rates
        .as_mut()
        .unwrap();
    rates.writes = Some(wisp_render::chrome::WriteMultipliers {
        prefix: 1.0,
        tail: 1.0,
        five_minutes: 1.0,
        one_hour: 1.0,
    });
    let mut tracker = Tracker::new(&facts);
    feed(
        &mut tracker,
        &facts,
        &[step(first_request(10_000, 0)), step(later_request(12_100))],
    );
    let quote = tracker.rebuild(&facts, "claude-opus-5-5", Scope::Exact, s(2));
    let Rebuild::PlanUsd(usd, _) = quote else {
        panic!("{quote:?}");
    };
    assert!((usd - 12_100.0 * 4.0 / 1e6).abs() < 1e-12);
    assert!(quote.line(UNI).ends_with(", subscription"));
}
