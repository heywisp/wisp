use wisp_protocol::{DeliveryMode, ImageType, WithdrawnBy};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::transcript::Detail;

use super::*;
use crate::app::fixture::{id, rendered, transcript};
use crate::events::{Changed, End, Ending, settings_changed, turn_ended};
use crate::testing::{line_text, text};

fn known() -> Facts {
    crate::app::fixture::endpoint_facts()
}

fn started(retry_of: Option<u64>) -> AgentEvent {
    AgentEvent::TurnStarted {
        turn: 1,
        origin: None,
        retry_of,
        answers: None,
        ends: Vec::new(),
    }
}

fn accepted(n: &str, text: &str, images: Vec<ImageRef>) -> AgentEvent {
    AgentEvent::InputAccepted {
        opening: false,
        id: id(n),
        mode: DeliveryMode::Steer,
        text: text.to_owned(),
        images,
        replaces: None,
        held: false,
        front: false,
        from: None,
        checklist_spans: Vec::new(),
    }
}

fn delivered(ids: &[&str], step: u32) -> AgentEvent {
    AgentEvent::InputDelivered {
        ids: ids.iter().map(|n| id(n)).collect(),
        step,
    }
}

fn played(events: &[AgentEvent]) -> (History, Transcript, Vec<bool>) {
    let mut history = History::new(GlyphSet::Unicode, None);
    let mut t = transcript();
    let mut taken = Vec::new();
    for event in events {
        let mine = history.apply(event, &known(), &mut t);
        if !mine {
            t.apply(event, std::time::Duration::ZERO);
        }
        taken.push(mine);
    }
    (history, t, taken)
}

#[test]
fn an_opening_message_shows_without_a_tag() {
    let legacy = vec![
        started(None),
        accepted("a", "hello there", Vec::new()),
        delivered(&["a"], 1),
    ];
    let mut opening = accepted("a", "hello there", Vec::new());
    if let AgentEvent::InputAccepted { opening, .. } = &mut opening {
        *opening = true;
    }
    for events in [legacy, vec![opening, started(None), delivered(&["a"], 1)]] {
        let (_, mut t, _) = played(&events);
        let text = rendered(&mut t);
        assert!(text.contains("hello there"), "{text}");
        assert!(!text.contains(copy::QUEUED_TAG), "{text}");
    }
}

#[test]
fn a_message_accepted_before_its_turn_is_tagged_queued() {
    let (_, mut t, _) = played(&[
        accepted("a", "later please", Vec::new()),
        started(None),
        delivered(&["a"], 1),
    ]);
    let text = rendered(&mut t);
    assert!(text.contains(copy::QUEUED_TAG), "{text}");
}

#[test]
fn a_steer_is_tagged_sent_while_working_and_a_retry_retried() {
    let (_, mut t, _) = played(&[
        started(None),
        accepted("a", "go", Vec::new()),
        delivered(&["a"], 1),
        accepted("b", "also this", Vec::new()),
        delivered(&["b"], 2),
    ]);
    let text = rendered(&mut t);
    assert!(text.contains(copy::SENT_WHILE_WORKING), "{text}");
    let (_, mut again, _) = played(&[
        started(Some(1)),
        accepted("a", "go", Vec::new()),
        delivered(&["a"], 1),
    ]);
    let text = rendered(&mut again);
    assert!(text.contains(copy::RETRIED_TAG), "{text}");
}

#[test]
fn a_withdrawn_delivered_message_is_marked_not_kept() {
    let (_, mut t, _) = played(&[
        started(None),
        accepted("a", "go", Vec::new()),
        delivered(&["a"], 1),
        AgentEvent::InputWithdrawn {
            ids: vec![id("a")],
            by: WithdrawnBy::NotKept,
        },
    ]);
    let text = rendered(&mut t);
    assert!(text.contains(copy::NOT_KEPT_TAG), "{text}");
}

#[test]
fn images_show_as_chips_with_their_logged_size() {
    let image = ImageRef {
        sha256: "00".to_owned(),
        media_type: ImageType::Png,
        bytes: 10,
        width: 640,
        height: 480,
    };
    let (_, mut t, _) = played(&[
        started(None),
        accepted("a", "look", vec![image]),
        delivered(&["a"], 1),
    ]);
    let text = rendered(&mut t);
    assert!(text.contains("[Image #1"), "{text}");
    assert!(text.contains("640"), "{text}");
    assert!(text.contains("look"), "{text}");
}

#[test]
fn the_note_counts_returned_messages_and_unreadable_images() {
    assert_eq!(returned_note(&[]), None);
    let one = Returned {
        id: None,
        text: "a".to_owned(),
        images: Vec::new(),
        missing: 0,
    };
    let note = returned_note(std::slice::from_ref(&one)).unwrap();
    assert!(
        note.starts_with("Your message from before Wisp stopped"),
        "{note}"
    );
    let lost = Returned {
        missing: 2,
        ..one.clone()
    };
    let note = returned_note(&[one, lost]).unwrap();
    assert!(note.starts_with("Your 2 messages"), "{note}");
    assert!(
        note.ends_with("2 of their images couldn't be read from the log."),
        "{note}"
    );
}

#[test]
fn text_after_an_image_chip_is_spaced_from_it() {
    let image = ImageRef {
        sha256: "00".to_owned(),
        media_type: ImageType::Png,
        bytes: 10,
        width: 640,
        height: 480,
    };
    let (_, mut t, _) = played(&[
        started(None),
        accepted("a", "look", vec![image]),
        delivered(&["a"], 1),
    ]);
    let text = rendered(&mut t);
    assert!(text.contains("] look"), "{text}");
}

#[test]
fn ids_reused_by_a_later_run_show_that_runs_text() {
    let (_, mut t, _) = played(&[
        started(None),
        accepted("tui-1", "first run", Vec::new()),
        delivered(&["tui-1"], 1),
        AgentEvent::SessionResumed {
            turns: 1,
            moved: Vec::new(),
            stay: Vec::new(),
        },
        AgentEvent::TurnStarted {
            turn: 2,
            origin: None,
            retry_of: None,
            answers: None,
            ends: Vec::new(),
        },
        accepted("tui-1", "second run", Vec::new()),
        delivered(&["tui-1"], 1),
    ]);
    let text = rendered(&mut t);
    let first = text.find("first run");
    let second = text.find("second run");
    assert!(
        first.is_some() && second.is_some() && first < second,
        "{text}"
    );
    assert_eq!(text.matches("first run").count(), 1, "{text}");
}

fn pair(model: &str) -> Pair {
    Pair {
        endpoint: "work".to_owned(),
        model: model.to_owned(),
        effort: Effort::Medium,
    }
}

#[test]
fn a_logged_model_change_shows_its_note() {
    let mut history = History::new(GlyphSet::Unicode, Some(pair("claude-opus-5-5")));
    let mut t = transcript();
    history.apply(&started(None), &known(), &mut t);
    let mine = history.apply(
        &settings_changed(Changed {
            endpoint: "work".to_owned(),
            model: "claude-sonnet-5".to_owned(),
            effort: Effort::Medium,
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            },
            ..Changed::default()
        }),
        &known(),
        &mut t,
    );
    assert!(!mine);
    let text = rendered(&mut t);
    assert!(text.contains("claude-sonnet-5"), "{text}");
    assert!(text.contains("cache rebuild"), "{text}");
}

#[test]
fn a_replayed_retry_says_what_it_retried_on() {
    for takes_effort in [true, false] {
        let mut facts = known();
        if !takes_effort {
            crate::app::fixture::with_levels(&mut facts, "claude-opus-5-5", &[], Effort::High);
        }
        let mut history = History::new(GlyphSet::Unicode, Some(pair("claude-opus-5-5")));
        let mut t = transcript();
        history.apply(
            &turn_ended(End {
                ending: Ending::Error(wisp_protocol::ErrorKind::Overloaded),
                message: "busy".to_owned(),
                attempts: 1,
                ..End::default()
            }),
            &facts,
            &mut t,
        );
        history.apply(&started(Some(1)), &facts, &mut t);
        let note = history.take_retry_note().unwrap_or_default();
        assert!(note.contains("work"), "{note}");
        assert!(note.contains("claude-opus-5-5"), "{note}");
        assert_eq!(note.contains(" at "), takes_effort, "{note}");
        assert_eq!(history.take_retry_note(), None);
        history.apply(&started(None), &facts, &mut t);
        assert_eq!(history.take_retry_note(), None);
    }
}

#[test]
fn every_way_a_delivered_message_is_lost_marks_it_not_kept() {
    for by in [
        WithdrawnBy::NotKept,
        WithdrawnBy::TurnStopped,
        WithdrawnBy::TurnFailed,
        WithdrawnBy::SessionEnded,
    ] {
        let (_, mut t, _) = played(&[
            started(None),
            accepted("a", "go", Vec::new()),
            delivered(&["a"], 1),
            AgentEvent::InputWithdrawn {
                ids: vec![id("a")],
                by: by.clone(),
            },
        ]);
        let text = rendered(&mut t);
        assert!(text.contains(copy::NOT_KEPT_TAG), "{by:?}: {text}");
    }
}

#[test]
fn a_replayed_settings_note_never_shows_an_amount() {
    for takes_effort in [true, false] {
        let mut facts = known();
        if !takes_effort {
            crate::app::fixture::with_levels(&mut facts, "claude-sonnet-5-5", &[], Effort::High);
        }
        let (model, effort) = if takes_effort {
            ("claude-sonnet-5", Effort::Medium)
        } else {
            ("claude-sonnet-5-5", Effort::High)
        };
        let change = settings_changed(Changed {
            endpoint: "other".to_owned(),
            model: model.to_owned(),
            effort,
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch,
            },
            ..Changed::default()
        });
        let mut before = History::new(GlyphSet::Unicode, Some(pair("claude-opus-5-5")));
        let mut t = transcript();
        before.apply(&change, &facts, &mut t);
        let text = rendered(&mut t);
        assert!(text.contains("nothing cached yet"), "{text}");

        let mut after = History::new(GlyphSet::Unicode, Some(pair("claude-opus-5-5")));
        let mut t = transcript();
        after.apply(&started(None), &facts, &mut t);
        after.apply(&change, &facts, &mut t);
        let text = rendered(&mut t);
        assert!(text.contains("cache rebuild"), "{text}");
        assert!(!text.contains("nothing cached"), "{text}");
        for amount in ["$", "≈", "~", "tokens"] {
            assert!(!text.contains(amount), "{amount}: {text}");
        }
        if !takes_effort {
            assert!(!text.contains("high"), "{text}");
        }
    }
}

#[test]
fn a_replayed_retry_names_the_pair_it_ran_on() {
    let mut history = History::new(GlyphSet::Unicode, Some(pair("claude-opus-5-5"))).retried_on(
        [(
            2,
            Pair {
                endpoint: "spare".to_owned(),
                model: "claude-sonnet-5".to_owned(),
                effort: Effort::Low,
            },
        )]
        .into(),
    );
    let mut t = transcript();
    history.apply(
        &turn_ended(End {
            ending: Ending::Error(wisp_protocol::ErrorKind::Overloaded),
            message: "busy".to_owned(),
            attempts: 1,
            ..End::default()
        }),
        &known(),
        &mut t,
    );
    history.apply(
        &AgentEvent::TurnStarted {
            turn: 2,
            origin: None,
            retry_of: Some(1),
            answers: None,
            ends: Vec::new(),
        },
        &known(),
        &mut t,
    );
    let note = history.take_retry_note().unwrap_or_default();
    assert!(note.contains("spare"), "{note}");
    assert!(note.contains("claude-sonnet-5"), "{note}");
    assert!(note.contains("low"), "{note}");
}

const REFUSED_ON_MANIFEST: &str = "manifest refused the web search tool; continuing without web search. To stop sending it: wisp config set endpoints.manifest.native_web_search off";

fn search_refused_on(endpoint: &str) -> AgentEvent {
    AgentEvent::FeatureStateChanged {
        endpoint: endpoint.to_owned(),
        model: "claude-opus-5-5".to_owned(),
        feature: Feature::NativeWebSearch,
        state: FeatureState::FallenBack,
    }
}

fn changed(endpoint: &str, reason: RebuildReason) -> AgentEvent {
    settings_changed(Changed {
        endpoint: endpoint.to_owned(),
        model: "claude-opus-5-5".to_owned(),
        effort: Effort::Medium,
        cache: CacheEffect::Rebuilt { reason },
        ..Changed::default()
    })
}

fn on(endpoint: &str) -> Pair {
    Pair {
        endpoint: endpoint.to_owned(),
        model: "claude-opus-5-5".to_owned(),
        effort: Effort::Medium,
    }
}

fn session_on(endpoint: &str) -> Facts {
    let mut f = known();
    f.endpoint = f.reach_of(endpoint);
    f.endpoint_name = Some(endpoint.to_owned());
    f
}

fn live_notes(f: &Facts, events: &[AgentEvent]) -> Vec<String> {
    let mut tracker = crate::app::settings::Tracker::new(f);
    let mut notes = Vec::new();
    for event in events {
        match tracker.apply(event, f, std::time::Duration::ZERO, GlyphSet::Unicode) {
            crate::app::settings::Answer::Note(text)
            | crate::app::settings::Answer::Refused(text) => notes.push(text),
            crate::app::settings::Answer::Notes(texts) => notes.extend(texts),
            crate::app::settings::Answer::Moved(moved, after) => {
                notes.extend(crate::app::settings::Answer::moved_texts(&moved, &after));
            }
            crate::app::settings::Answer::Hint(_) | crate::app::settings::Answer::Other => {}
        }
    }
    notes
}

fn replayed_notes(start: &str, events: &[AgentEvent]) -> Vec<String> {
    let f = session_on(start);
    let mut history = History::new(GlyphSet::Unicode, Some(on(start)));
    let mut t = transcript();
    for event in events {
        if !history.apply(event, &f, &mut t) {
            t.apply(event, std::time::Duration::ZERO);
        }
    }
    let ended = turn_ended(End {
        model: "claude-opus-5-5".to_owned(),
        ..End::default()
    });
    t.apply(&ended, std::time::Duration::ZERO);
    let r = Renderer::new(Layout::new(1000, GlyphSet::Unicode));
    let mut notes: Vec<String> = Vec::new();
    let mut open = false;
    for row in t.rows(&r, Detail::Scrollback) {
        let text = line_text(&row);
        if let Some(note) = text.strip_prefix(" \u{2219} ") {
            notes.push(note.trim_end().to_owned());
            open = true;
        } else if open && text.starts_with("   ") && !text.trim().is_empty() {
            if let Some(last) = notes.last_mut() {
                last.push(' ');
                last.push_str(text.trim());
            }
        } else {
            open = false;
        }
    }
    notes
        .into_iter()
        .filter(|n| n.contains("web search") || n.contains("Account"))
        .collect()
}

fn head(note: &str) -> &str {
    note.split(" \u{b7} ").next().unwrap_or(note)
}

fn heads(notes: &[String]) -> Vec<&str> {
    notes.iter().map(|n| head(n)).collect()
}

#[test]
fn a_replayed_gateway_refusal_reads_as_live_did() {
    let events = [
        started(None),
        search_refused_on("manifest"),
        changed("manifest", RebuildReason::NativeSearchRefused),
    ];
    let live = live_notes(&session_on("manifest"), &events);
    let replayed = replayed_notes("manifest", &events);
    assert_eq!(heads(&live), [REFUSED_ON_MANIFEST]);
    assert_eq!(heads(&replayed), heads(&live), "{replayed:?}");
    assert!(
        replayed[0].ends_with(" \u{b7} cache rebuild"),
        "{replayed:?}"
    );
}

#[test]
fn a_replayed_direct_refusal_reads_as_live_did() {
    let events = [
        started(None),
        search_refused_on("direct"),
        changed("direct", RebuildReason::NativeSearchRefused),
    ];
    let live = live_notes(&session_on("direct"), &events);
    let replayed = replayed_notes("direct", &events);
    assert_eq!(heads(&live).len(), 1, "{live:?}");
    assert!(heads(&live)[0].contains(" on direct;"), "{live:?}");
    assert!(!heads(&live)[0].contains("refused the web search tool"));
    assert_eq!(heads(&replayed), heads(&live), "{replayed:?}");
    assert!(!replayed[0].contains('?'), "{replayed:?}");
}

#[test]
fn a_replayed_refusal_after_an_endpoint_switch_names_the_refused_endpoint() {
    let events = [
        started(None),
        changed("direct", RebuildReason::EndpointSwitch),
        search_refused_on("manifest"),
        changed("direct", RebuildReason::NativeSearchRefused),
    ];
    let replayed = replayed_notes("manifest", &events);
    assert_eq!(
        heads(&replayed),
        ["Account \u{2192} direct", REFUSED_ON_MANIFEST],
        "{replayed:?}"
    );
    let live = live_notes(&session_on("manifest"), &events);
    assert!(
        live.iter().any(|n| head(n) == REFUSED_ON_MANIFEST),
        "{live:?}"
    );
}

#[test]
fn a_refusal_without_its_record_names_the_endpoint_it_ran_on() {
    let events = [
        started(None),
        search_refused_on("manifest"),
        changed("manifest", RebuildReason::NativeSearchRefused),
        changed("direct", RebuildReason::EndpointSwitch),
        changed("direct", RebuildReason::NativeSearchRefused),
    ];
    let replayed = replayed_notes("manifest", &events);
    assert_eq!(heads(&replayed)[0], REFUSED_ON_MANIFEST, "{replayed:?}");
    assert!(heads(&replayed)[2].contains(" on direct;"), "{replayed:?}");
}

fn full(t: &mut Transcript) -> String {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    text(&t.rows(&r, Detail::Full))
}

#[test]
fn a_replayed_rewritten_message_is_drawn_once_as_it_was_live() {
    let applied = AgentEvent::HookApplied {
        hook: "aa/u".to_owned(),
        point: "user-input".to_owned(),
        turn: None,
        step: None,
        call_id: None,
        input_id: Some(id("m")),
        change: HookChange::Text {
            before: "hi".to_owned(),
            after: "hi [sent]".to_owned(),
        },
    };
    let ran = AgentEvent::HookRan {
        hook: "aa/u".to_owned(),
        point: "user-input".to_owned(),
        turn: None,
        step: None,
        call_id: None,
        outcome: wisp_protocol::HookOutcome::Applied,
        ms: 3,
    };
    let (_, mut replayed, taken) = played(&[
        ran.clone(),
        applied.clone(),
        started(None),
        accepted("m", "hi [sent]", Vec::new()),
        delivered(&["m"], 1),
    ]);
    assert_eq!(taken, [false, false, false, true, true]);
    let mut live = transcript();
    live.apply(&ran, std::time::Duration::ZERO);
    live.apply(&applied, std::time::Duration::ZERO);
    live.apply(&started(None), std::time::Duration::ZERO);
    let cell = live.deliver(vec![UserPart::Text("hi".to_owned())], None);
    live.rewritten(cell, "aa/u", "hi [sent]");
    let shown = full(&mut replayed);
    assert_eq!(shown, full(&mut live));
    assert!(
        shown.contains("hook aa/u at user-input applied \u{b7} 3 ms \u{b7} hi [sent]"),
        "{shown}"
    );
    assert_eq!(shown.matches("hi [sent]").count(), 2, "{shown}");
    assert!(shown.contains("rewritten by aa/u"), "{shown}");
}
