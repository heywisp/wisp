use wisp_protocol::settings::AccountModel;
use wisp_protocol::settings::{CACHE_KEPT, CACHE_KEPT_EFFORT};
use wisp_protocol::{
    AgentEvent, CacheEffect, CacheTtl, RebuildReason, TurnOutcome, TurnStats, valid_endpoint_name,
};
use wisp_render::chrome::Endpoint;

use super::*;
use crate::app::fixture::{endpoint_facts, facts, four_accounts, unglued, with_levels};
use crate::events::{Changed, End, Ending, outcome, settings_changed};
use crate::testing::line_text;

const UNI: GlyphSet = GlyphSet::Unicode;

fn key(code: KeyCode) -> Key {
    Key::plain(code)
}

fn typed(p: &mut Picker, text: &str, f: &Facts, t: &Tracker) {
    for c in text.chars() {
        assert_eq!(p.key(key(KeyCode::Char(c)), f, t), Step::Stay);
    }
}

fn labels(menu: &Menu) -> Vec<(String, bool)> {
    menu.choices
        .iter()
        .map(|c| (unglued(&c.label), c.enabled))
        .collect()
}

fn trimmed(menu: &Menu) -> Vec<(String, bool)> {
    labels(menu)
        .into_iter()
        .map(|(l, enabled)| (l.trim_start().to_owned(), enabled))
        .collect()
}

fn selected(menu: &Menu) -> String {
    menu.selected
        .and_then(|i| menu.choices.get(i))
        .map(|c| unglued(&c.label))
        .unwrap_or_default()
}

#[test]
fn the_model_picker_lists_every_model_and_dims_the_unsupported() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let menu = Picker::models().saving().menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        labels(&menu),
        [
            ("haiku-5-5".to_owned(), true),
            ("opus-4-5".to_owned(), false),
            ("opus-4-8".to_owned(), true),
            ("opus-5-5".to_owned(), true),
            ("sonnet-4-6".to_owned(), true),
            ("sonnet-5-5".to_owned(), true),
        ]
    );
    assert_eq!(selected(&menu), "opus-5-5");
    let opus = &menu.choices[3];
    assert!(opus.current);
    assert_eq!(opus.detail, "$4 \u{b7} $20");
    assert_eq!(opus.effect, account::NOW);
    assert_eq!(menu.choices[0].detail, "$0.10 \u{b7} $0.50");
    assert_eq!(
        menu.choices[1].effect,
        "not supported yet (no adaptive thinking)"
    );
    assert_eq!(
        menu.choices[5].effect,
        "nothing cached yet, so nothing to rebuild"
    );
    assert_eq!(
        menu.notes,
        [(
            "claude-\u{2026} (type any model ID, then enter)".to_owned(),
            Tone::Faint
        )]
    );
    assert_eq!(menu.titles[0], "Model");
}

#[test]
fn typing_filters_and_selection_skips_dimmed_rows() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let mut p = Picker::models().saving();
    typed(&mut p, "opus", &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        labels(&menu)
            .into_iter()
            .map(|(l, _)| l)
            .collect::<Vec<_>>(),
        ["opus-4-5", "opus-4-8", "opus-5-5"]
    );
    assert_eq!(menu.titles[0], "Model");
    assert_eq!(
        menu.notes,
        [(
            "matching \"opus\" (backspace edit, esc close)".to_owned(),
            Tone::Faint
        )]
    );
    p.key(key(KeyCode::Down), &f, &t);
    assert_eq!(selected(&p.menu(&f, &t, Duration::ZERO, UNI)), "opus-4-8");
    p.key(key(KeyCode::Up), &f, &t);
    assert_eq!(selected(&p.menu(&f, &t, Duration::ZERO, UNI)), "opus-5-5");
    p.key(key(KeyCode::Down), &f, &t);
    p.key(key(KeyCode::Down), &f, &t);
    assert_eq!(selected(&p.menu(&f, &t, Duration::ZERO, UNI)), "opus-5-5");
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: None,
            model: Some("claude-opus-5-5".to_owned()),
            effort: None,
        }),
        "the saving picker saves the current model too"
    );
}

#[test]
fn an_unlisted_id_is_offered_as_free_text_with_a_warning() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let mut p = Picker::models().saving();
    typed(&mut p, "my-gw/model@2", &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(labels(&menu), [("my-gw/model@2".to_owned(), true)]);
    assert_eq!(menu.choices[0].detail, "(any model ID, enter to use)");
    assert_eq!(
        menu.choices[0].effect,
        "nothing cached yet, so nothing to rebuild"
    );
    let mut cached = Tracker::new(&f);
    cached.apply(
        &AgentEvent::StepEnded {
            step: 1,
            usage: Some(wisp_protocol::Usage {
                input_tokens: 20_000,
                ..wisp_protocol::Usage::default()
            }),
            cost_usd: None,
            lower_bound: false,
        },
        &f,
        Duration::ZERO,
        UNI,
    );
    assert_eq!(
        p.menu(&f, &cached, Duration::ZERO, UNI).choices[0].effect,
        "cache rebuild \u{2248}20k tokens"
    );
    assert_eq!(
        menu.notes[0],
        (
            "No capability data for this ID: effort changes rebuild the cache, images are off, cost shows tokens.".to_owned(),
            Tone::Note
        )
    );
    let mut priced = f.clone();
    priced.on_accounts = vec![wisp_render::chrome::AccountFacts {
        account: cached.wanted().endpoint.clone(),
        model: "my-gw/model@2".to_owned(),
        rates: Some(wisp_render::chrome::ModelRates {
            writes: None,
            input_per_mtok: 1.2,
            output_per_mtok: 4.8,
            cache_read_per_mtok: 0.12,
            long_prompt: None,
        }),
        max_output: None,
    }];
    let menu = p.menu(&priced, &cached, Duration::ZERO, UNI);
    assert_eq!(menu.choices[0].effect, "cache rebuild ~$0.048");
    assert_eq!(
        menu.notes[0].0,
        "No capability data for this ID: effort changes rebuild the cache, images are off."
    );
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: None,
            model: Some("my-gw/model@2".to_owned()),
            effort: None,
        })
    );
}

const CANARY: &str = "sk-ant-api03-CANARYMODEL0000";

fn shows_nothing_of_the_canary(menu: &Menu) {
    let all = format!("{menu:?}");
    assert!(!all.contains("CANARY"), "{all}");
    assert!(!all.contains("sk-ant"), "{all}");
}

#[test]
fn a_key_shaped_id_typed_or_pasted_is_disabled_and_never_shown() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let mut typed_in = Picker::models();
    typed(&mut typed_in, CANARY, &f, &t);
    let mut pasted = Picker::models();
    pasted.paste(CANARY);
    for p in [&mut typed_in, &mut pasted] {
        let menu = p.menu(&f, &t, Duration::ZERO, UNI);
        assert_eq!(trimmed(&menu), [(NOT_SHOWN.to_owned(), false)]);
        assert_eq!(menu.choices[0].effect, copy::KEY_SHAPED_MODEL);
        assert_eq!(menu.selected, None);
        shows_nothing_of_the_canary(&menu);
        assert_eq!(p.key(key(KeyCode::Enter), &f, &t), Step::Stay);
        assert!(p.values(&f, &t).iter().all(|v| v.label == NOT_SHOWN));
    }
}

#[test]
fn a_typed_key_shows_as_the_placeholder_and_is_never_chosen() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let secret = "sk-ant-api03-CANARYMODEL0000";
    let mut p = Picker::models();
    typed(&mut p, secret, &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert!(!format!("{menu:?}").contains("CANARY"), "{menu:?}");
    assert_eq!(
        trimmed(&menu),
        [(wisp_protocol::NOT_SHOWN.to_owned(), false)]
    );
    assert!(
        menu.notes
            .iter()
            .any(|(note, _)| note.starts_with("matching \"(looks like a key, not shown)\"")),
        "{:?}",
        menu.notes
    );
    let values = p.values(&f, &t);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].label, wisp_protocol::NOT_SHOWN);
    assert!(!values[0].enabled);
    assert_eq!(p.key(key(KeyCode::Enter), &f, &t), Step::Stay);
}

#[test]
fn only_id_characters_reach_the_query_and_it_stays_short() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let mut p = Picker::models();
    p.paste("]0;pwn\u{7}\u{9b}[2J x\ny\u{202e}z");
    assert_eq!(p.query(), Some("0pwn2Jxyz"));
    p.paste(&"a".repeat(400));
    assert_eq!(p.query().map(|q| q.chars().count()), Some(MAX_MODEL_ID));
    assert!(valid_model_id(p.query().unwrap_or_default()));
    p.key(key(KeyCode::Backspace), &f, &t);
    assert_eq!(p.query().map(str::len), Some(MAX_MODEL_ID - 1));
}

#[test]
fn esc_and_ctrl_c_close_without_choosing() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    assert_eq!(Picker::models().key(key(KeyCode::Esc), &f, &t), Step::Close);
    assert_eq!(Picker::models().key(Key::ctrl('c'), &f, &t), Step::Close);
}

#[test]
fn a_model_without_the_current_effort_chains_the_effort_picker() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    f.effort = "xhigh".to_owned();
    let t = Tracker::new(&f);
    let mut p = Picker::models();
    typed(&mut p, "sonnet-4", &f, &t);
    let picked = p.key(key(KeyCode::Enter), &f, &t);
    assert_eq!(
        picked,
        Step::Done(Picked::Effort {
            endpoint: None,
            model: "claude-sonnet-4-6".to_owned(),
            preselect: Some(Effort::High),
        })
    );
    let mut chained = Picker::efforts("claude-sonnet-4-6".to_owned(), true, Some(Effort::High));
    let menu = chained.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        labels(&menu)
            .into_iter()
            .map(|(l, _)| l)
            .collect::<Vec<_>>(),
        ["low", "medium", "high", "max"]
    );
    assert_eq!(selected(&menu), "high");
    assert_eq!(menu.choices[2].detail, "default");
    assert!(menu.choices.iter().all(|c| !c.current));
    assert!(
        menu.choices
            .iter()
            .all(|c| c.effect == "with the model switch")
    );
    chained.key(key(KeyCode::Down), &f, &t);
    assert_eq!(
        chained.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: None,
            model: Some("claude-sonnet-4-6".to_owned()),
            effort: Some(Effort::Max),
        })
    );
    f.effort = "medium".to_owned();
    assert_eq!(
        switch_to("claude-sonnet-4-6", &f, Some(Effort::Medium), None),
        Picked::Settings {
            endpoint: None,
            model: Some("claude-sonnet-4-6".to_owned()),
            effort: None,
        }
    );
}

#[test]
fn the_effort_picker_marks_the_current_level_and_labels_the_default() {
    let f = facts(Endpoint::Custom, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let mut p = Picker::efforts("claude-opus-5-5".to_owned(), false, None);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(menu.titles[0], "Effort opus-5-5");
    assert_eq!(selected(&menu), "medium");
    let rows: Vec<(&str, bool, &str, &str)> = menu
        .choices
        .iter()
        .map(|c| {
            (
                c.label.as_str(),
                c.current,
                c.detail.as_str(),
                c.effect.as_str(),
            )
        })
        .collect();
    let maybe = CACHE_KEPT_EFFORT;
    assert_eq!(
        rows,
        [
            ("low", false, "", maybe),
            ("medium", true, "default", account::NOW),
            ("high", false, "", maybe),
            ("xhigh", false, "", maybe),
            ("max", false, "", maybe),
        ]
    );
    assert_eq!(p.key(key(KeyCode::Enter), &f, &t), Step::Close);
    p.key(key(KeyCode::Up), &f, &t);
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: None,
            model: None,
            effort: Some(Effort::Low),
        })
    );
}

#[test]
fn an_unknown_model_offers_every_level_with_the_rebuild_warning() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    f.model = "gateway-model".to_owned();
    let t = Tracker::new(&f);
    let menu =
        Picker::efforts("gateway-model".to_owned(), false, None).menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(menu.choices.len(), 5);
    assert_eq!(menu.notes[0].1, Tone::Note);
    assert_eq!(
        menu.choices[0].effect,
        "nothing cached yet, so nothing to rebuild"
    );
}

#[test]
fn a_chained_picker_for_the_running_model_shows_each_levels_own_effect() {
    let mut f = facts(Endpoint::Custom, CacheTtl::OneHour);
    f.model = "claude-sonnet-4-6".to_owned();
    f.effort = "high".to_owned();
    let mut t = Tracker::new(&f);
    t.apply(
        &settings_changed(Changed {
            endpoint: "claude-api".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            effort: Effort::Xhigh,
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            },
            ..Changed::default()
        }),
        &f,
        Duration::ZERO,
        UNI,
    );
    let mut p = Picker::models().saving();
    typed(&mut p, "sonnet-4", &f, &t);
    assert_eq!(
        p.menu(&f, &t, Duration::ZERO, UNI).choices[0].effect,
        CACHE_KEPT
    );
    let Step::Done(Picked::Effort {
        model, preselect, ..
    }) = p.key(key(KeyCode::Enter), &f, &t)
    else {
        panic!("sonnet-4-6 takes no xhigh, so the effort picker follows");
    };
    let chained = Picker::efforts(model, true, preselect);
    let effects: Vec<String> = chained
        .menu(&f, &t, Duration::ZERO, UNI)
        .choices
        .into_iter()
        .map(|c| c.effect)
        .collect();
    assert_eq!(
        effects,
        [
            "nothing cached yet, so nothing to rebuild",
            "nothing cached yet, so nothing to rebuild",
            CACHE_KEPT,
            "nothing cached yet, so nothing to rebuild",
        ]
    );
}

#[test]
fn the_endpoint_picker_lists_every_endpoint_with_its_credential_and_effect() {
    let f = endpoint_facts();
    let t = Tracker::new(&f);
    let menu = Picker::endpoints()
        .saving()
        .menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        menu.titles,
        [
            "Account".to_owned(),
            "credential".to_owned(),
            "next turn".to_owned()
        ]
    );
    assert_eq!(
        labels(&menu),
        [
            ("direct".to_owned(), true),
            ("gateway".to_owned(), true),
            ("manifest".to_owned(), true),
            ("work".to_owned(), false),
            ("+ Add an API key\u{2026}".to_owned(), true),
        ]
    );
    assert_eq!(selected(&menu), "manifest");
    let rows: Vec<(bool, &str, &str)> = menu
        .choices
        .iter()
        .map(|c| (c.current, c.detail.as_str(), c.effect.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            (
                false,
                AccountKind::ClaudeApi.label(),
                "nothing cached yet, so nothing to rebuild"
            ),
            (
                false,
                "Gateway \u{b7} gw2.example.com",
                "nothing cached yet, so nothing to rebuild"
            ),
            (true, "Gateway \u{b7} gw.example.com", account::NOW),
            (
                false,
                AccountKind::ClaudeApi.label(),
                "unavailable: WORK_KEY is not set"
            ),
            (false, "", ""),
        ]
    );
    assert_eq!(
        menu.notes,
        [(
            "list prices on manifest \u{b7} type to filter \u{b7} edit or remove with wisp account"
                .to_owned(),
            Tone::Faint
        )]
    );
}

#[test]
fn endpoint_keys_skip_unavailable_rows_filter_and_choose() {
    let mut f = endpoint_facts();
    f.endpoints[0].priced = false;
    let t = Tracker::new(&f);
    let mut p = Picker::endpoints().saving();
    p.key(key(KeyCode::Down), &f, &t);
    assert_eq!(
        selected(&p.menu(&f, &t, Duration::ZERO, UNI)),
        "+ Add an API key\u{2026}",
        "down skips the unavailable row"
    );
    assert_eq!(p.clone().key(key(KeyCode::Enter), &f, &t), Step::Add);
    p.key(key(KeyCode::Down), &f, &t);
    assert_eq!(
        selected(&p.menu(&f, &t, Duration::ZERO, UNI)),
        "direct",
        "and wraps"
    );
    assert_eq!(
        p.menu(&f, &t, Duration::ZERO, UNI).notes[0].0,
        "no prices on direct \u{b7} type to filter \u{b7} edit or remove with wisp account"
    );
    p.key(key(KeyCode::Up), &f, &t);
    p.key(key(KeyCode::Up), &f, &t);
    assert_eq!(selected(&p.menu(&f, &t, Duration::ZERO, UNI)), "manifest");
    p.key(key(KeyCode::Tab), &f, &t);
    p.key(key(KeyCode::Tab), &f, &t);
    assert_eq!(selected(&p.menu(&f, &t, Duration::ZERO, UNI)), "direct");
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: Some("direct".to_owned()),
            model: None,
            effort: None,
        })
    );
    let mut p = Picker::endpoints().saving();
    typed(&mut p, "WoR", &f, &t);
    assert_eq!(p.query(), Some("wor"));
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(labels(&menu), [("work".to_owned(), false)]);
    assert_eq!(
        menu.notes,
        [(
            "matching \"wor\" (backspace edit, esc close)".to_owned(),
            Tone::Faint
        )]
    );
    assert_eq!(p.key(key(KeyCode::Enter), &f, &t), Step::Stay);
    p.key(key(KeyCode::Backspace), &f, &t);
    p.key(key(KeyCode::Backspace), &f, &t);
    p.key(key(KeyCode::Backspace), &f, &t);
    typed(&mut p, "man", &f, &t);
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: Some("manifest".to_owned()),
            model: None,
            effort: None,
        }),
        "the saving picker saves the current endpoint too"
    );
    let mut p = Picker::endpoints().saving();
    p.paste("e/n v!.[31m");
    assert_eq!(p.query(), Some("env31m"));
    assert_eq!(p.key(key(KeyCode::Esc), &f, &t), Step::Close);
    assert_eq!(
        p.key(
            Key {
                ctrl: true,
                ..key(KeyCode::Char('c'))
            },
            &f,
            &t
        ),
        Step::Close
    );
}

#[test]
fn endpoint_names_follow_the_config_rule() {
    for good in ["direct", "a", "0-gw_2", &"a".repeat(32)] {
        assert!(valid_endpoint_name(good), "{good}");
    }
    for bad in [
        "",
        "-x",
        "_x",
        "Direct",
        "a b",
        "a.b",
        &"a".repeat(33),
        "a!b",
    ] {
        assert!(!valid_endpoint_name(bad), "{bad}");
    }
}

#[test]
fn a_model_switch_preselects_by_one_rule_whatever_the_api_default() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    with_levels(
        &mut f,
        "claude-opus-4-5",
        &[Effort::High, Effort::Low, Effort::Medium],
        Effort::Low,
    );
    with_levels(
        &mut f,
        "claude-sonnet-4-6",
        &[Effort::Low, Effort::Medium, Effort::High, Effort::Max],
        Effort::Max,
    );
    for model in ["claude-opus-4-5", "claude-sonnet-4-6"] {
        assert_eq!(
            switch_to(model, &f, Some(Effort::Xhigh), None),
            Picked::Effort {
                endpoint: None,
                model: model.to_owned(),
                preselect: Some(Effort::High),
            }
        );
    }
    assert_eq!(
        switch_to("claude-sonnet-4-6", &f, None, None),
        Picked::Effort {
            endpoint: None,
            model: "claude-sonnet-4-6".to_owned(),
            preselect: Some(Effort::Low),
        }
    );
}

#[test]
fn a_model_switch_with_no_lower_level_preselects_the_lowest_supported() {
    let mut f = facts(Endpoint::Official, CacheTtl::OneHour);
    with_levels(
        &mut f,
        "claude-sonnet-4-6",
        &[Effort::Max, Effort::High],
        Effort::Max,
    );
    assert_eq!(
        switch_to("claude-sonnet-4-6", &f, Some(Effort::Low), None),
        Picked::Effort {
            endpoint: None,
            model: "claude-sonnet-4-6".to_owned(),
            preselect: Some(Effort::High),
        }
    );
    let t = Tracker::new(&f);
    let menu = Picker::efforts("claude-sonnet-4-6".to_owned(), true, Some(Effort::High)).menu(
        &f,
        &t,
        Duration::ZERO,
        UNI,
    );
    assert_eq!(selected(&menu), "high");
}

#[test]
fn a_queried_endpoint_picker_lowercases_its_query_as_typing_does() {
    let f = endpoint_facts();
    let t = Tracker::new(&f);
    let queried = Picker::endpoints().saving().queried("MaN");
    assert_eq!(queried.query(), Some("man"));
    let mut typed = Picker::endpoints().saving();
    typed.paste("MaN");
    assert_eq!(queried, typed);
    let menu = queried.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(labels(&menu), [("manifest".to_owned(), true)]);
    let model = Picker::models().saving().queried("OPUS");
    assert_eq!(model.query(), Some("OPUS"));
    let menu = model.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(labels(&menu).len(), 3);
}

#[test]
fn every_row_has_the_value_it_inserts() {
    let f = endpoint_facts();
    let t = Tracker::new(&f);
    let values = |p: &Picker| -> Vec<(String, bool, bool)> {
        p.values(&f, &t)
            .into_iter()
            .map(|v| (v.text, v.enabled, v.matches))
            .collect()
    };
    assert_eq!(
        values(&Picker::models().saving().queried("opus")),
        [
            ("claude-opus-4-5".to_owned(), false, true),
            ("claude-opus-4-8".to_owned(), true, true),
            ("claude-opus-5-5".to_owned(), true, true),
        ]
    );
    assert_eq!(
        values(&Picker::models().saving().queried("claude-x9")),
        [("claude-x9".to_owned(), true, true)]
    );
    assert_eq!(
        values(&Picker::endpoints().saving().queried("w")),
        [
            ("gateway".to_owned(), true, true),
            ("work".to_owned(), false, true)
        ]
    );
    let t = Tracker::new(&f);
    let menu = Picker::models()
        .saving()
        .queried("opus")
        .menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(menu.choices.len(), 3);
    let opus = Picker::models().saving().queried("opus");
    assert_eq!(
        opus.values(&f, &t)
            .into_iter()
            .map(|v| v.label)
            .collect::<Vec<_>>(),
        menu.choices
            .iter()
            .map(|c| c.label.clone())
            .collect::<Vec<_>>()
    );
    let levels = Picker::efforts("claude-opus-5-5".to_owned(), false, None);
    assert!(levels.values(&f, &t).iter().all(|v| v.label == v.text));
    assert!(!Picker::models().is_closed());
    assert!(!Picker::endpoints().is_closed());
}

#[test]
fn an_effort_query_marks_prefix_matches_without_filtering_the_levels() {
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let t = Tracker::new(&f);
    let plain = Picker::efforts("claude-opus-5-5".to_owned(), false, None);
    let queried = plain.clone().queried("M");
    assert!(queried.is_closed());
    assert_eq!(
        queried
            .values(&f, &t)
            .into_iter()
            .map(|v| (v.text, v.matches))
            .collect::<Vec<_>>(),
        [
            ("low".to_owned(), false),
            ("medium".to_owned(), true),
            ("high".to_owned(), false),
            ("xhigh".to_owned(), false),
            ("max".to_owned(), true),
        ]
    );
    assert_eq!(
        queried.menu(&f, &t, Duration::ZERO, UNI),
        plain.menu(&f, &t, Duration::ZERO, UNI)
    );
    assert!(plain.values(&f, &t).iter().all(|v| v.matches && v.enabled));
}

fn effects(menu: &Menu) -> Vec<(String, String)> {
    menu.choices
        .iter()
        .map(|c| (unglued(&c.label), c.effect.clone()))
        .collect()
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

const EMPTY: &str = "nothing cached yet, so nothing to rebuild";
const NO_NATIVE: &str = "web search: off (not on this model)";
const TURNED_OFF: &str = "web search: off (off for this account)";
const SWITCHED_OFF: &str = "web search: off (not on this account)";

const DROPS: &str = "switching web search on or off drops earlier reasoning";
const ANY_ID: &str = "claude-\u{2026} (type any model ID, then enter)";

fn thought(outcome: TurnOutcome) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        AgentEvent::BlockStarted {
            index: 0,
            kind: wisp_protocol::BlockKind::Thinking,
        },
        AgentEvent::StepEnded {
            step: 1,
            usage: None,
            cost_usd: None,
            lower_bound: false,
        },
        AgentEvent::TurnEnded {
            outcome,
            stats: TurnStats::default(),
        },
    ]
}

fn completed() -> TurnOutcome {
    outcome(End {
        model: "claude-opus-5-5".to_owned(),
        ..End::default()
    })
}

fn notes_of(menu: &Menu) -> Vec<&str> {
    menu.notes.iter().map(|(text, _)| text.as_str()).collect()
}

#[test]
fn a_model_row_whose_search_form_differs_says_so_beside_its_rebuild() {
    let f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    let menu = Picker::models().saving().menu(&f, &t, Duration::ZERO, UNI);
    let shift = format!("{EMPTY} \u{b7} web search: on \u{2192} off");
    assert_eq!(
        effects(&menu),
        pairs(&[
            ("haiku-5-5", EMPTY),
            ("opus-4-5", "not supported yet (no adaptive thinking)"),
            ("opus-4-8", EMPTY),
            ("opus-5-5", account::NOW),
            ("sonnet-4-6", &shift),
            ("sonnet-5-5", EMPTY),
        ])
    );
    assert_eq!(notes_of(&menu), [ANY_ID]);
    for e in &thought(completed()) {
        t.apply(e, &f, Duration::ZERO, UNI);
    }
    let menu = Picker::models().saving().menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(menu.choices[4].effect, shift);
    assert_eq!(menu.notes[0], (DROPS.to_owned(), Tone::Faint));
    assert_eq!(notes_of(&menu), [DROPS, ANY_ID]);
    let mut p = Picker::models().saving();
    typed(&mut p, "my-model", &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, GlyphSet::Ascii);
    assert_eq!(
        menu.choices[0].effect,
        format!("{EMPTY} - web search: on -> off")
    );
    assert_eq!(notes_of(&menu)[0], DROPS);
}

#[test]
fn rows_keep_their_reasoning_cost_off_the_row_and_name_it_once_below() {
    let f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    for e in &thought(completed()) {
        t.apply(e, &f, Duration::ZERO, UNI);
    }
    let menu = Picker::endpoints()
        .saving()
        .menu(&f, &t, Duration::ZERO, UNI);
    let off = format!("{EMPTY} \u{b7} web search: on \u{2192} off");
    assert_eq!(
        effects(&menu),
        pairs(&[
            ("direct", account::NOW),
            ("gateway", &off),
            ("manifest", &off),
            ("work", "unavailable: WORK_KEY is not set"),
            ("+ Add an API key\u{2026}", ""),
        ])
    );
    assert_eq!(
        notes_of(&menu),
        [
            DROPS,
            "list prices on direct \u{b7} type to filter \u{b7} edit or remove with wisp account"
        ]
    );
    assert!(menu.choices.iter().all(|c| !c.effect.contains("reasoning")));
    let mut p = Picker::endpoints().saving();
    typed(&mut p, "dir", &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert!(!notes_of(&menu).contains(&DROPS), "{menu:?}");
}

#[test]
fn a_stopped_or_failed_reply_leaves_no_reasoning_to_drop() {
    let stopped = outcome(End {
        ending: Ending::Cancelled,
        ..End::default()
    });
    let failed = outcome(End {
        ending: Ending::Error(wisp_protocol::ErrorKind::Overloaded),
        message: "overloaded".to_owned(),
        ..End::default()
    });
    let uncommitted = outcome(End {
        model: "claude-opus-5-5".to_owned(),
        committed: false,
        ..End::default()
    });
    for outcome in [stopped, failed, uncommitted] {
        let f = crate::app::fixture::search_facts("direct");
        let mut t = Tracker::new(&f);
        for e in &thought(outcome.clone()) {
            t.apply(e, &f, Duration::ZERO, UNI);
        }
        let menu = Picker::endpoints().menu(&f, &t, Duration::ZERO, UNI);
        assert!(!notes_of(&menu).contains(&DROPS), "{outcome:?}: {menu:?}");
    }
}

#[test]
fn rows_that_all_share_one_search_note_state_it_once_below() {
    let mut f = crate::app::fixture::search_facts("manifest");
    f.search.pairs.retain(|p| p.endpoint != "manifest");
    let t = Tracker::new(&f);
    let menu = Picker::models().saving().menu(&f, &t, Duration::ZERO, UNI);
    assert!(
        menu.choices
            .iter()
            .all(|c| !c.effect.contains("web search")),
        "{menu:?}"
    );
    assert_eq!(
        menu.notes,
        [
            (NO_NATIVE.to_owned(), Tone::Faint),
            (
                "claude-\u{2026} (type any model ID, then enter)".to_owned(),
                Tone::Faint
            ),
        ]
    );
    let menu = Picker::efforts("claude-opus-5-5".to_owned(), false, None).menu(
        &f,
        &t,
        Duration::ZERO,
        UNI,
    );
    assert!(menu.notes.is_empty(), "{:?}", menu.notes);
    assert!(
        menu.choices
            .iter()
            .all(|c| !c.effect.contains("web search")),
        "{menu:?}"
    );
}

#[test]
fn each_endpoint_row_says_whether_the_model_would_search_there() {
    let f = crate::app::fixture::search_facts("manifest");
    let t = Tracker::new(&f);
    let menu = Picker::endpoints()
        .saving()
        .menu(&f, &t, Duration::ZERO, UNI);
    let to_native = format!("{EMPTY} \u{b7} web search: off \u{2192} on");
    assert_eq!(
        effects(&menu),
        pairs(&[
            ("direct", &to_native),
            ("gateway", EMPTY),
            ("manifest", account::NOW),
            ("work", "unavailable: WORK_KEY is not set"),
            ("+ Add an API key\u{2026}", ""),
        ])
    );
    assert_eq!(
        notes_of(&menu),
        [
            TURNED_OFF,
            "list prices on manifest \u{b7} type to filter \u{b7} edit or remove with wisp account"
        ]
    );
    let mut p = Picker::endpoints().saving();
    typed(&mut p, "ga", &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(effects(&menu), pairs(&[("gateway", EMPTY)]));
    assert_eq!(notes_of(&menu)[0], TURNED_OFF);
    let mut p = Picker::endpoints().saving();
    typed(&mut p, "d", &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(effects(&menu), pairs(&[("direct", &to_native)]));
    assert!(!notes_of(&menu).contains(&TURNED_OFF), "{menu:?}");
}

#[test]
fn a_refusal_mid_session_updates_both_pickers() {
    let mut f = crate::app::fixture::search_facts("direct");
    let mut t = Tracker::new(&f);
    let before = Picker::endpoints()
        .saving()
        .menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(before.choices[0].effect, account::NOW);
    let events = [
        AgentEvent::turn_started(1),
        AgentEvent::FeatureStateChanged {
            endpoint: "direct".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            feature: wisp_protocol::Feature::NativeWebSearch,
            state: wisp_protocol::log::FeatureState::FallenBack,
        },
        settings_changed(Changed {
            endpoint: "direct".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            effort: wisp_protocol::Effort::Medium,
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::NativeSearchRefused,
            },
            ..Changed::default()
        }),
    ];
    for e in &events {
        t.apply(e, &f, Duration::ZERO, UNI);
        t.show(&mut f);
    }
    let menu = Picker::models().saving().menu(&f, &t, Duration::ZERO, UNI);
    assert!(
        menu.choices
            .iter()
            .all(|c| !c.effect.contains("web search")),
        "{menu:?}"
    );
    assert_eq!(menu.notes[0], (SWITCHED_OFF.to_owned(), Tone::Faint));
    let menu = Picker::endpoints()
        .saving()
        .menu(&f, &t, Duration::ZERO, UNI);
    let current = format!("{} \u{b7} {SWITCHED_OFF}", account::NOW);
    let gateway = format!("{EMPTY} \u{b7} {TURNED_OFF}");
    assert_eq!(
        effects(&menu),
        pairs(&[
            ("direct", &current),
            ("gateway", &gateway),
            ("manifest", &gateway),
            ("work", "unavailable: WORK_KEY is not set"),
            ("+ Add an API key\u{2026}", ""),
        ])
    );
}

#[test]
fn an_explicitly_supported_reasoning_model_is_selectable_without_adaptive_thinking() {
    let mut facts = facts(Endpoint::Official, CacheTtl::OneHour);
    facts.endpoint_name = Some("openai".into());
    let mut model = facts.models[0].clone();
    model.id = "gpt-6.1-sol".into();
    let caps = model.caps.as_mut().unwrap();
    caps.supported = true;
    caps.adaptive_thinking = false;
    caps.per_message_effort = false;
    facts.models.push(model);
    let tracker = Tracker::new(&facts);
    let mut picker = Picker::models().saving();
    typed(&mut picker, "gpt-6.1-sol", &facts, &tracker);
    let menu = picker.menu(&facts, &tracker, Duration::ZERO, UNI);
    assert_eq!(labels(&menu), [("gpt-6.1-sol".to_owned(), true)]);
    assert!(
        matches!(picker.key(key(KeyCode::Enter),&facts,&tracker),Step::Done(Picked::Settings {model:Some(model),..}) if model=="gpt-6.1-sol")
    );
}

#[test]
fn the_saving_pick_lists_a_list_only_model_a_list_shows_and_types_a_hidden_one_as_free_text() {
    let mut f = four_accounts();
    let hidden = f.model_facts("gpt-reserve").unwrap().clone();
    f.models.push(wisp_render::chrome::ModelFacts {
        id: "gpt-shown".into(),
        listed: Some(8),
        ..hidden.clone()
    });
    f.models.push(wisp_render::chrome::ModelFacts {
        provider: Some("gemini".to_owned()),
        id: "gemini-9-future-exp".into(),
        ..hidden
    });
    let t = Tracker::new(&f);
    let free = "(any model ID, enter to use)";
    for (id, listed) in [
        ("gpt-shown", true),
        ("gpt-reserve", false),
        ("gemini-9-future-exp", false),
    ] {
        let mut p = Picker::models().saving();
        typed(&mut p, id, &f, &t);
        let menu = p.menu(&f, &t, Duration::ZERO, UNI);
        assert_eq!(labels(&menu), [(id.to_owned(), true)]);
        assert_eq!(menu.choices[0].detail != free, listed, "{id}");
        assert_eq!(menu.choices[0].detail == copy::COST_UNKNOWN, listed, "{id}");
    }
}

fn warmed(f: &Facts) -> Tracker {
    let mut t = Tracker::new(f);
    t.apply(
        &AgentEvent::StepEnded {
            step: 1,
            usage: Some(wisp_protocol::Usage {
                input_tokens: 20_000,
                ..wisp_protocol::Usage::default()
            }),
            cost_usd: None,
            lower_bound: false,
        },
        f,
        Duration::ZERO,
        UNI,
    );
    t
}

fn shown(menu: &Menu) -> Vec<String> {
    menu.choices.iter().map(|c| unglued(&c.label)).collect()
}

fn on_chat(t: &Tracker) -> Picker {
    Picker::models().opened(t.wanted().endpoint)
}

fn select(p: &mut Picker, f: &Facts, t: &Tracker, label: &str, under: &str) {
    *p = p.clone().opened(under.to_owned());
    for _ in 0..40 {
        let menu = p.menu(f, t, Duration::ZERO, UNI);
        let at = menu.selected.unwrap_or_default();
        let head = menu
            .shape
            .heads
            .iter()
            .rev()
            .find(|h| h.at < at)
            .map(|h| h.at);
        let account = head
            .map(|h| unglued(&menu.choices[h].label))
            .unwrap_or_default();
        if unglued(&menu.choices[at].label)
            .trim_start()
            .ends_with(label)
            && account == under
        {
            return;
        }
        p.key(key(KeyCode::Down), f, t);
    }
    panic!("{label} under {under} never selected");
}

#[test]
fn the_model_picker_opens_the_chats_account_and_folds_the_others_into_one_row_each() {
    let f = four_accounts();
    let t = Tracker::new(&f);
    let menu = on_chat(&t).menu(&f, &t, Duration::ZERO, UNI);
    let want = [
        "claude-subscription",
        "  opus-5-5",
        "  sonnet-5-5",
        "  haiku-5-5",
        "  opus-4-5",
        "  opus-4-8",
        "  sonnet-4-6",
        "claude-work",
        "claude-api",
        "chatgpt",
        "+ Sign in to another account\u{2026}",
    ];
    assert_eq!(shown(&menu), want);
    let heads: Vec<usize> = menu.shape.heads.iter().map(|h| h.at).collect();
    assert_eq!(heads, [0, 7, 8, 9]);
    let after: Vec<&str> = menu.shape.heads.iter().map(|h| h.after.as_str()).collect();
    assert_eq!(
        after,
        [
            "",
            "opus-5-5 medium",
            "opus-5-5 medium",
            "gpt-6.1-sol medium"
        ]
    );
    assert_eq!(menu.choices[0].detail, "Claude subscription");
    assert_eq!(menu.choices[9].detail, "ChatGPT subscription");
    assert!(
        menu.choices[7..10]
            .iter()
            .all(|c| c.effect.is_empty() && c.enabled)
    );
    assert_eq!(selected(&menu), "  opus-5-5");
    let current: Vec<usize> = (0..menu.choices.len())
        .filter(|&i| menu.choices[i].current)
        .collect();
    assert_eq!(current, [1]);
    let mut unlisted = four_accounts();
    unlisted.endpoint_name = Some("claude-work".to_owned());
    unlisted.model = "claude-fable-5-2".to_owned();
    let t = Tracker::new(&unlisted);
    let menu = on_chat(&t).menu(&unlisted, &t, Duration::ZERO, UNI);
    assert_eq!(selected(&menu), "  opus-5-5");
}

#[test]
fn focus_skips_the_open_header_and_wraps() {
    let f = four_accounts();
    let t = Tracker::new(&f);
    let mut p = on_chat(&t);
    let last = p.menu(&f, &t, Duration::ZERO, UNI).choices.len() - 1;
    let mut seen = Vec::new();
    for _ in 0..20 {
        p.key(key(KeyCode::Down), &f, &t);
        let at = p.menu(&f, &t, Duration::ZERO, UNI).selected;
        assert!(at.is_some_and(|at| at != 0), "{at:?}");
        seen.extend(at);
    }
    assert!(seen.contains(&last) && seen.contains(&1), "{seen:?}");
    assert!(seen.contains(&7), "{seen:?}");
}

#[test]
fn a_filter_keeps_the_headers_and_an_account_name_keeps_its_group() {
    let f = four_accounts();
    let t = Tracker::new(&f);
    let menu = Picker::models()
        .queried("haiku")
        .menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        shown(&menu),
        [
            "claude-subscription",
            "  haiku-5-5",
            "claude-work",
            "  haiku-5-5",
            "claude-api",
            "  haiku-5-5",
        ]
    );
    let menu = Picker::models()
        .queried("work")
        .menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        shown(&menu),
        ["claude-work", "  opus-5-5", "  sonnet-5-5", "  haiku-5-5"]
    );
    let menu = Picker::models()
        .queried("opus-4")
        .menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        shown(&menu),
        [
            "claude-subscription",
            "  opus-4-5",
            "  opus-4-8",
            "claude-work",
            "  opus-4-5",
            "  opus-4-8",
            "claude-api",
            "  opus-4-5",
            "  opus-4-8",
        ]
    );
}

#[test]
fn an_older_current_model_sits_after_the_usual_rows_of_its_provider_accounts_only() {
    let mut f = four_accounts();
    f.model = "claude-opus-4-8".to_owned();
    let t = Tracker::new(&f);
    let opus = |l: &String| l.ends_with("opus-4-8");
    for account in [
        "claude-subscription",
        "claude-work",
        "claude-api",
        "chatgpt",
    ] {
        let menu = Picker::models()
            .opened(account.to_owned())
            .menu(&f, &t, Duration::ZERO, UNI);
        let labels = shown(&menu);
        let at = labels.iter().position(opus);
        if account == "chatgpt" {
            assert_eq!(at, None, "{labels:?}");
            continue;
        }
        let head = labels.iter().position(|l| l == account).unwrap_or_default();
        assert_eq!(at, Some(head + 4), "{account}: {labels:?}");
        assert_eq!(
            menu.choices[head + 4].current,
            account == "claude-subscription"
        );
    }
    let menu = on_chat(&t).menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(selected(&menu), "  opus-4-8");
}

#[test]
fn a_pick_under_another_account_sends_the_account_and_the_model_together() {
    let f = four_accounts();
    let t = Tracker::new(&f);
    let mut p = Picker::models();
    select(&mut p, &f, &t, "sonnet-5-5", "claude-work");
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: Some("claude-work".to_owned()),
            model: Some("claude-sonnet-5-5".to_owned()),
            effort: None,
        })
    );
    let mut p = Picker::models();
    select(&mut p, &f, &t, "gpt-6.1-sol", "chatgpt");
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: Some("chatgpt".to_owned()),
            model: Some("gpt-6.1-sol".to_owned()),
            effort: None,
        })
    );
}

#[test]
fn the_effort_step_after_a_pick_under_another_account_keeps_that_account() {
    let mut f = four_accounts();
    f.effort = "xhigh".to_owned();
    with_levels(
        &mut f,
        "claude-sonnet-5-5",
        &[Effort::Low, Effort::High],
        Effort::High,
    );
    let t = Tracker::new(&f);
    let mut p = Picker::models();
    select(&mut p, &f, &t, "sonnet-5-5", "claude-work");
    let Step::Done(Picked::Effort {
        endpoint,
        model,
        preselect,
    }) = p.key(key(KeyCode::Enter), &f, &t)
    else {
        panic!("sonnet-5-5 takes no xhigh here");
    };
    assert_eq!(endpoint.as_deref(), Some("claude-work"));
    let mut levels = Picker::efforts_on(endpoint, model, true, preselect);
    let menu = levels.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(shown(&menu), ["low", "high"]);
    assert_eq!(levels.dock(&Menu::default(), UNI).name, "Effort");
    assert!(
        menu.notes
            .iter()
            .any(|(n, _)| n == "sonnet-5-5 doesn't offer xhigh"),
        "{:?}",
        menu.notes
    );
    let under = menu.shape.under.clone().unwrap_or_default();
    assert!(
        under.first[0].starts_with("Moves this chat to \u{25cf}\u{a0}claude-work"),
        "{under:?}"
    );
    assert_eq!(
        levels.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: Some("claude-work".to_owned()),
            model: Some("claude-sonnet-5-5".to_owned()),
            effort: Some(Effort::High),
        })
    );
}

#[test]
fn an_account_whose_sign_in_expired_is_marked_and_signs_in_before_switching() {
    let mut f = four_accounts();
    f.endpoints[1].expired = true;
    let work = wisp_protocol::settings::EndpointSummary {
        models: Vec::new(),
        mark: Some(2),
        name: "claude-work".into(),
        provider: "anthropic".into(),
        credential: Some(wisp_protocol::settings::CredentialKind::Other(
            "anthropic_subscription".into(),
        )),
        official: Some(true),
        host: None,
        unavailable: None,
        priced: true,
        default: false,
        keys_file: false,
        expired: true,
        key_var: None,
        voice_model: None,
        complete: false,
        default_model: None,
    };
    assert_eq!(
        crate::app::endpoint_card::endpoint_facts_of(&work),
        f.endpoints[1]
    );
    let t = Tracker::new(&f);
    let mut p = on_chat(&t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    let work = menu.shape.heads.iter().find(|h| h.at == 7);
    assert_eq!(work.map(|h| h.tone), Some(wisp_render::theme::Token::Error));
    assert_eq!(work.map(|h| h.after.as_str()), Some("opus-5-5 medium"));
    select(&mut p, &f, &t, "sonnet-5-5", "claude-work");
    let under = p
        .menu(&f, &t, Duration::ZERO, UNI)
        .shape
        .under
        .unwrap_or_default();
    assert_eq!(under.first, ["claude-work's sign-in expired"]);
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::SignIn {
            account: "claude-work".to_owned(),
            then: Picked::Settings {
                endpoint: Some("claude-work".to_owned()),
                model: Some("claude-sonnet-5-5".to_owned()),
                effort: None,
            },
        }
    );
}

#[test]
fn each_line_under_the_list_names_its_move() {
    let f = four_accounts();
    let t = warmed(&f);
    let under = |label: &str, account: &str| {
        let mut p = Picker::models();
        select(&mut p, &f, &t, label, account);
        p.menu(&f, &t, Duration::ZERO, UNI)
            .shape
            .under
            .unwrap_or_default()
    };
    assert_eq!(
        under("opus-5-5", "claude-subscription").first,
        [account::THIS_CHAT_NOW]
    );
    let model = under("haiku-5-5", "claude-subscription");
    assert_eq!(model.first, [account::ONLY_THE_MODEL]);
    assert!(model.parts[0].0.ends_with(", subscription"), "{model:?}");
    let work = under("opus-5-5", "claude-work");
    assert_eq!(work.first, ["Moves this chat to \u{25cf}\u{a0}claude-work"]);
    assert!(work.parts[0].0.ends_with(", subscription"), "{work:?}");
    let api = under("opus-5-5", "claude-api");
    assert_eq!(
        api.first,
        ["Moves this chat to \u{25cf}\u{a0}claude-api, from your subscription to the API"]
    );
    assert!(!api.parts[0].0.contains("subscription"), "{api:?}");
    let chatgpt = under("gpt-6.1-sol", "chatgpt");
    assert_eq!(chatgpt.first, ["Moves this chat to \u{25cf}\u{a0}chatgpt"]);
    assert_eq!(chatgpt.after, [account::no_carry("chatgpt")]);
    let mut p = on_chat(&t);
    for _ in 0..20 {
        let menu = p.menu(&f, &t, Duration::ZERO, UNI);
        if menu.selected == Some(menu.choices.len() - 1) {
            break;
        }
        p.key(key(KeyCode::Down), &f, &t);
    }
    let add = p
        .menu(&f, &t, Duration::ZERO, UNI)
        .shape
        .under
        .unwrap_or_default();
    assert_eq!(add.first, [account::ADDS_AN_ACCOUNT]);
}

#[test]
fn each_pair_is_priced_for_its_own_account_and_model() {
    let f = four_accounts();
    let t = warmed(&f);
    let price = |account: &str, model: &str| match t.pair_effect(&f, account, model, Duration::ZERO)
    {
        Effect::Rebuild(r) | Effect::Switch(r, _) => r.cell(),
        other => panic!("{other:?}"),
    };
    let haiku = price("claude-work", "claude-haiku-5-5");
    let opus = price("claude-work", "claude-opus-5-5");
    let sol = price("chatgpt", "gpt-6.1-sol");
    assert_ne!(haiku, opus);
    assert_ne!(sol, opus);
    let row = |account: &str, at: usize| {
        let menu = Picker::models()
            .opened(account.to_owned())
            .menu(&f, &t, Duration::ZERO, UNI);
        menu.choices[at].effect.clone()
    };
    assert_eq!(row("claude-work", 4), haiku);
    assert_eq!(row("claude-work", 2), opus);
    assert_eq!(row("chatgpt", 4), sol);
    assert_eq!(row("claude-subscription", 1), account::NOW);
}

#[test]
fn account_folds_one_row_per_account_and_opens_one_at_a_time() {
    let f = four_accounts();
    let t = Tracker::new(&f);
    let mut p = Picker::endpoints();
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        shown(&menu),
        [
            "claude-subscription",
            "claude-work",
            "claude-api",
            "chatgpt",
            "+ Sign in to another account\u{2026}",
        ]
    );
    assert!(menu.choices[0].current && !menu.choices[1].current);
    let effects: Vec<&str> = menu.choices.iter().map(|c| c.effect.as_str()).collect();
    assert_eq!(
        effects,
        [
            "opus-5-5 medium",
            "opus-5-5 medium",
            "opus-5-5 medium",
            "gpt-6.1-sol medium",
            ""
        ]
    );
    assert_eq!(menu.choices[2].detail, AccountKind::ClaudeApi.label());
    assert_eq!(p.dock(&Menu::default(), UNI).name, "Account");
    p.key(key(KeyCode::Down), &f, &t);
    p.key(key(KeyCode::Right), &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    let work = [
        "claude-subscription",
        "claude-work",
        "  opus-5-5",
        "  sonnet-5-5",
        "  haiku-5-5",
        "  opus-4-5",
        "  opus-4-8",
        "  sonnet-4-6",
        "claude-api",
        "chatgpt",
        "+ Sign in to another account\u{2026}",
    ];
    assert_eq!(shown(&menu), work);
    assert_eq!(selected(&menu), "  opus-5-5");
    assert_eq!(menu.selected, Some(2));
    for _ in 0..5 {
        p.key(key(KeyCode::Down), &f, &t);
    }
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(selected(&menu), "claude-api");
    let keys = p.dock(&menu, UNI).keys;
    let arrow = Glyphs::of(UNI).arrow;
    assert!(
        keys.contains(&KeyHint::new(arrow, account::MODELS_WORD)),
        "{keys:?}"
    );
    assert_eq!(
        p.clone().key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: Some("claude-api".to_owned()),
            model: None,
            effort: None,
        })
    );
    p.key(key(KeyCode::Right), &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        &shown(&menu)[..3],
        ["claude-subscription", "claude-work", "claude-api"]
    );
    assert_eq!(shown(&menu)[3], "  opus-5-5");
    assert_eq!(menu.shape.heads.len(), 4, "{:?}", shown(&menu));
    assert_eq!(selected(&menu), "  opus-5-5");
    p.key(key(KeyCode::Left), &f, &t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(selected(&menu), "claude-api");
    assert_eq!(p.dock(&menu, UNI).name, "Account");
    p.key(key(KeyCode::Down), &f, &t);
    p.key(key(KeyCode::Down), &f, &t);
    let sign_in = "+ Sign in to another account\u{2026}";
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(selected(&menu), sign_in);
    assert!(
        !p.dock(&menu, UNI)
            .keys
            .contains(&KeyHint::new(arrow, account::MODELS_WORD))
    );
    let before = p.clone();
    p.key(key(KeyCode::Right), &f, &t);
    assert_eq!(p, before);
}

#[test]
fn right_on_an_account_that_offers_no_model_does_nothing() {
    let mut f = four_accounts();
    let mut gateway = f.endpoints[0].clone();
    gateway.name = "gateway".to_owned();
    gateway.provider = "gateway".to_owned();
    f.endpoints.push(gateway);
    let t = Tracker::new(&f);
    let mut p = Picker::endpoints();
    for _ in 0..4 {
        p.key(key(KeyCode::Down), &f, &t);
    }
    assert_eq!(selected(&p.menu(&f, &t, Duration::ZERO, UNI)), "gateway");
    let before = p.clone();
    p.key(key(KeyCode::Right), &f, &t);
    assert_eq!(p, before);
    let menu = on_chat(&t).menu(&f, &t, Duration::ZERO, UNI);
    assert!(!shown(&menu).contains(&"gateway".to_owned()));
}

fn live_lists() -> Facts {
    let mut f = four_accounts();
    let base = f.model_facts("claude-opus-5-5").unwrap().clone();
    let ids = [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-fable-5-1",
        "claude-nova-7",
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-sonnet-4-6",
        "claude-opus-4-6",
        "claude-opus-4-5-20251101",
        "claude-haiku-4-5-20251001",
    ];
    let own = |id: &str, at: usize| AccountModel {
        id: id.to_owned(),
        listed: u16::try_from(at).ok().filter(|at| *at < 5),
    };
    let mut every: Vec<AccountModel> = ids.iter().enumerate().map(|(i, id)| own(id, i)).collect();
    let new: Vec<&str> = ids
        .into_iter()
        .filter(|id| f.model_facts(id).is_none())
        .collect();
    for id in new {
        let mut facts = wisp_render::chrome::ModelFacts {
            id: id.to_owned(),
            ..base.clone()
        };
        if id == "claude-nova-7" {
            facts.shipped = false;
            facts.rates = None;
            facts
                .caps
                .iter_mut()
                .for_each(|caps| caps.per_message_effort = false);
        }
        f.models.push(facts);
    }
    for endpoint in &mut f.endpoints[..2] {
        endpoint.models = every.clone();
    }
    every.retain(|m| m.id != "claude-nova-7");
    f.endpoints[2].models = every;
    f
}

#[test]
fn a_model_without_data_reads_cost_unknown_and_says_so_under_the_list() {
    let f = live_lists();
    let t = warmed(&f);
    let mut p = on_chat(&t);
    select(&mut p, &f, &t, "nova-7", "claude-subscription");
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    let want = [
        "claude-subscription",
        "  opus-5-5",
        "  sonnet-5-5",
        "  haiku-5-5",
        "  fable-5-1",
        "  nova-7",
        "  opus-5",
    ];
    assert_eq!(shown(&menu)[..7], want);
    assert_eq!(menu.selected, Some(5));
    assert!(f.rates_on("claude-subscription", "claude-nova-7").is_none());
    assert_eq!(menu.choices[5].detail, copy::COST_UNKNOWN);
    assert_eq!(menu.choices[5].effect, "");
    assert_eq!(menu.choices[4].detail, "");
    let under = menu.shape.under.unwrap_or_default();
    assert_eq!(under.first, [account::ONLY_THE_MODEL]);
    assert_eq!(under.after, [copy::NO_DATA_FOR_MODEL]);
    select(&mut p, &f, &t, "sonnet-5-5", "claude-subscription");
    let under = p.menu(&f, &t, Duration::ZERO, UNI).shape.under;
    assert_eq!(under.unwrap_or_default().after, Vec::<String>::new());
    let mut on_nova = live_lists();
    on_nova.model = "claude-nova-7".to_owned();
    let t = warmed(&on_nova);
    let menu = on_chat(&t).menu(&on_nova, &t, Duration::ZERO, UNI);
    assert_eq!(selected(&menu), "  nova-7");
    assert_eq!(menu.choices[5].detail, "medium");
    let under = menu.shape.under.unwrap_or_default();
    assert_eq!(
        (under.first, under.after),
        (vec![account::THIS_CHAT_NOW.to_owned()], vec![])
    );
    let t = Tracker::new(&f);
    let menu = Picker::models()
        .queried("nova-7")
        .menu(&f, &t, Duration::ZERO, UNI);
    let want = [
        "claude-subscription",
        "  nova-7",
        "claude-work",
        "  nova-7",
        "claude-api",
        "  nova-7",
    ];
    assert_eq!(shown(&menu), want);
    assert!(menu.choices.iter().all(|c| c.detail != copy::FREE_TEXT));
}

#[test]
fn a_rowless_model_at_120_columns_unicode_and_40_ascii() {
    let f = live_lists();
    let t = warmed(&f);
    let mut p = on_chat(&t);
    select(&mut p, &f, &t, "nova-7", "claude-subscription");
    for (set, width) in [(GlyphSet::Unicode, 120), (GlyphSet::Ascii, 40)] {
        let text = painted(&p, &f, &t, set, width).join("\n");
        insta::assert_snapshot!(format!("rowless_{width}_{set:?}"), text);
    }
}

#[test]
fn the_sign_in_row_starts_the_sign_in() {
    let f = four_accounts();
    let t = Tracker::new(&f);
    let mut p = Picker::endpoints();
    for _ in 0..4 {
        p.key(key(KeyCode::Down), &f, &t);
    }
    assert_eq!(p.key(key(KeyCode::Enter), &f, &t), Step::Add);
}

fn painted(p: &Picker, f: &Facts, t: &Tracker, set: GlyphSet, width: usize) -> Vec<String> {
    let menu = p.menu(f, t, Duration::ZERO, set);
    let layout = wisp_render::markdown::Layout::new(width, set);
    crate::testing::trimmed(
        &wisp_render::chrome::docked_lines(&menu, &p.dock(&menu, set), 24, layout).lines,
    )
}

#[test]
fn the_grouped_and_folded_docks_over_four_accounts_keep_their_height_at_40_80_and_120() {
    let f = four_accounts();
    let t = warmed(&f);
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let paint = |p: &Picker| painted(p, &f, &t, set, width);
            let mut shots: Vec<(String, Vec<String>)> = Vec::new();
            let mut models = on_chat(&t);
            shots.push(("whole".to_owned(), paint(&models)));
            select(&mut models, &f, &t, "opus-5-5", "claude-work");
            shots.push(("claude-work".to_owned(), paint(&models)));
            select(&mut models, &f, &t, "gpt-6.1-sol", "chatgpt");
            shots.push(("chatgpt".to_owned(), paint(&models)));
            let mut reserve = Picker::models();
            typed(&mut reserve, "reserve", &f, &t);
            shots.push(("typed reserve".to_owned(), paint(&reserve)));
            let mut filtered = Picker::models();
            typed(&mut filtered, "opus", &f, &t);
            shots.push(("filtered opus".to_owned(), paint(&filtered)));
            let effort = Picker::efforts_on(
                Some("chatgpt".to_owned()),
                "gpt-6.1-sol".to_owned(),
                true,
                None,
            );
            shots.push(("effort on chatgpt".to_owned(), paint(&effort)));
            let mut folded = Picker::endpoints();
            let mut heights = Vec::new();
            for step in 0..4 {
                let lines = paint(&folded);
                heights.push(lines.len());
                shots.push((format!("account {step}"), lines));
                folded.key(key(KeyCode::Down), &f, &t);
            }
            assert!(heights.windows(2).all(|h| h[0] == h[1]), "{heights:?}");
            assert_eq!(shots[0].1.len(), shots[1].1.len());
            assert_eq!(shots[1].1.len(), shots[2].1.len());
            let text: Vec<String> = shots
                .into_iter()
                .map(|(name, lines)| format!("== {name}\n{}", lines.join("\n")))
                .collect();
            insta::assert_snapshot!(format!("four_accounts_{width}_{set:?}"), text.join("\n"));
        }
    }
}

fn on_chatgpt(mut f: Facts) -> Facts {
    f.endpoint_name = Some("chatgpt".to_owned());
    f.credential = Some(wisp_render::chrome::Credential::ChatGptPlan);
    f.model = "gpt-6.1-sol".to_owned();
    f
}

fn starting(f: &Facts, query: &str, on: &str) -> Picker {
    use wisp_render::chrome::account::Who;
    let mark = f.mark_of(on);
    let head = match query {
        "" => vec![
            Said::Text("No account offers opus-55".to_owned()),
            Said::Text("choose a model".to_owned()),
        ],
        _ => vec![Said::Lacks(
            Who::Account(on.to_owned(), mark),
            query.to_owned(),
        )],
    };
    let query = query.to_owned();
    Picker::starting(StartPick { query, head }, on, mark)
}

#[test]
fn the_start_picker_at_40_80_and_120_columns() {
    let several = on_chatgpt(four_accounts());
    let mut only_api = several.clone();
    only_api
        .endpoints
        .retain(|e| e.name == "claude-api" || e.name == "chatgpt");
    let none = four_accounts();
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let paint = |name: &str, p: &Picker, f: &Facts| -> String {
                let lines = painted(p, f, &Tracker::new(f), set, width);
                format!("== {name}\n{}", lines.join("\n"))
            };
            let mut shots = Vec::new();
            let mut p = starting(&several, "opus-5-5", "chatgpt");
            shots.push(paint("several offer it", &p, &several));
            let t = Tracker::new(&several);
            p.key(key(KeyCode::Down), &several, &t);
            p.key(key(KeyCode::Down), &several, &t);
            shots.push(paint("several, on claude-api", &p, &several));
            let mut p = starting(&only_api, "opus-5-5", "chatgpt");
            shots.push(paint("only the API", &p, &only_api));
            p.key(key(KeyCode::Down), &only_api, &Tracker::new(&only_api));
            shots.push(paint("only the API, after down", &p, &only_api));
            let p = starting(&none, "", "claude-subscription");
            shots.push(paint("no account offers it", &p, &none));
            insta::assert_snapshot!(format!("start_picker_{width}_{set:?}"), shots.join("\n"));
        }
    }
}

#[test]
fn the_start_picker_focuses_the_named_model_where_it_starts_no_charge() {
    let mut f = on_chatgpt(four_accounts());
    let mut opus_5 = f
        .models
        .iter()
        .find(|m| m.id == "claude-opus-5-5")
        .unwrap()
        .clone();
    opus_5.id = "claude-opus-5".to_owned();
    opus_5.listed = None;
    f.models.push(opus_5);
    let t = Tracker::new(&f);
    let p = starting(&f, "opus-5", "chatgpt");
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(selected(&menu), "  opus-5");
    assert_eq!(menu.choices[1].label, "  opus-5-5");
    let mut nothing = p.clone();
    typed(&mut nothing, "zzz", &f, &t);
    assert!(nothing.menu(&f, &t, Duration::ZERO, UNI).choices.is_empty());

    let mut api = f.clone();
    api.endpoints
        .retain(|e| e.name == "claude-api" || e.name == "chatgpt");
    let t = Tracker::new(&api);
    let mut p = starting(&api, "opus-5-5", "chatgpt");
    assert_eq!(p.key(key(KeyCode::Enter), &api, &t), Step::Stay);
    assert_eq!(p.key(key(KeyCode::Left), &api, &t), Step::Stay);
    assert_eq!(p.query(), Some("opus-5-5"));
    p.key(key(KeyCode::Up), &api, &t);
    let menu = p.menu(&api, &t, Duration::ZERO, UNI);
    assert_eq!(selected(&menu), "  opus-5-5");
    let layout = wisp_render::markdown::Layout::new(80, UNI);
    let lines = wisp_render::chrome::docked_lines(&menu, &p.dock(&menu, UNI), 24, layout).lines;
    let glyphs = Glyphs::of(UNI);
    assert_ne!(api.mark_of("chatgpt"), api.mark_of("claude-api"));
    for (has, on) in [
        ("doesn't offer", "chatgpt"),
        ("esc stay on", "chatgpt"),
        ("Starts this chat on", "claude-api"),
    ] {
        let line = lines.iter().find(|l| line_text(l).contains(has));
        let glyph = line.and_then(|l| l.spans.iter().find(|s| s.text == "\u{25cf} "));
        assert_eq!(
            glyph,
            Some(&account::mark(api.mark_of(on), &glyphs)),
            "{has}"
        );
    }
    assert!(matches!(
        p.clone().key(key(KeyCode::Enter), &api, &t),
        Step::Done(_)
    ));
    assert_eq!(p.key(key(KeyCode::Esc), &api, &t), Step::Close);

    let f = four_accounts();
    let t = Tracker::new(&f);
    let mut p = starting(&f, "", "claude-subscription");
    assert_eq!(p.key(key(KeyCode::Enter), &f, &t), Step::Close);
}

#[test]
fn listed_only_gemini_model_says_thinking_effort_and_images_are_off() {
    let mut f = live_lists();
    let mut carbon = f.model_facts("claude-opus-5-5").unwrap().clone();
    carbon.provider = Some("gemini".to_owned());
    carbon.id = "gemini-4-carbon-medium".to_owned();
    carbon.caps = Some(wisp_render::chrome::ModelCaps {
        supported: true,
        efforts: Vec::new(),
        default_effort: None,
        per_message_effort: false,
        adaptive_thinking: true,
        max_output: Some(65_536),
    });
    let mut flash_image = carbon.clone();
    flash_image.id = "gemini-3.1-flash-image".to_owned();
    flash_image.caps = Some(wisp_render::chrome::ModelCaps {
        supported: false,
        efforts: Vec::new(),
        default_effort: None,
        per_message_effort: false,
        adaptive_thinking: false,
        max_output: Some(32_768),
    });
    let future = wisp_render::chrome::ModelFacts {
        provider: Some("gemini".to_owned()),
        shipped: false,
        id: "gemini-9-future-exp".to_owned(),
        listed: None,
        rates: None,
        caps: Some(wisp_render::chrome::ModelCaps {
            supported: true,
            efforts: Vec::new(),
            default_effort: None,
            per_message_effort: false,
            adaptive_thinking: false,
            max_output: None,
        }),
        variant: None,
        image_tier: None,
    };
    let mut priced_future = future.clone();
    priced_future.id = "gemini-9-priced-exp".to_owned();
    priced_future.rates = carbon.rates;
    let mut base = carbon.clone();
    base.id = "gemini-4-carbon".into();
    f.models
        .extend([carbon, flash_image, future, priced_future, base]);
    f.endpoints.push(wisp_render::chrome::EndpointFacts {
        name: "antigravity".to_owned(),
        provider: "gemini".to_owned(),
        credential: Some(wisp_render::chrome::Credential::AntigravityPlan),
        reach: Some(Endpoint::Official),
        host: None,
        unavailable: None,
        priced: true,
        expired: false,
        key_var: None,
        mark: None,
        models: vec![
            AccountModel {
                id: "gemini-3.1-flash-image".to_owned(),
                listed: Some(0),
            },
            AccountModel {
                id: "gemini-4-carbon-medium".to_owned(),
                listed: Some(1),
            },
            AccountModel {
                id: "gemini-9-future-exp".to_owned(),
                listed: Some(2),
            },
            AccountModel {
                id: "gemini-9-priced-exp".to_owned(),
                listed: Some(3),
            },
            AccountModel {
                id: "gemini-10-live-exp".to_owned(),
                listed: Some(4),
            },
        ],
        complete: true,
        default_model: Some("gemini-4-carbon-medium".to_owned()),
    });
    let t = warmed(&f);
    assert_eq!(
        super::model_on(&f, &t.wanted().endpoint, "antigravity", "claude-sonnet-5-5"),
        "gemini-4-carbon-medium"
    );
    assert_eq!(
        Picker::account_step("antigravity", &f, &t),
        Step::Done(Picked::Settings {
            endpoint: Some("antigravity".to_owned()),
            model: Some("gemini-4-carbon-medium".to_owned()),
            effort: None,
        })
    );
    {
        let mut f = f.clone();
        let ag_index = f.endpoints.len() - 1;
        let mut api = f.endpoints[ag_index].clone();
        api.name = "gemini".into();
        api.credential = Some(wisp_render::chrome::Credential::ApiKey);
        api.models = vec![AccountModel {
            id: "gemini-4-carbon".into(),
            listed: Some(0),
        }];
        api.default_model = Some("gemini-4-carbon".into());
        f.endpoints.push(api);
        f.endpoint_name = Some("gemini".into());
        f.model = "gemini-4-carbon".into();
        let t = warmed(&f);
        assert_eq!(
            super::model_on(&f, &t.wanted().endpoint, "antigravity", &f.model),
            "gemini-4-carbon-medium"
        );
        assert!(
            matches!(Picker::account_step("antigravity", &f, &t), Step::Done(Picked::Settings { model: Some(model), .. }) if model == "gemini-4-carbon-medium")
        );
        let target = accounts(&f, &t)
            .into_iter()
            .find(|a| a.name == "antigravity")
            .unwrap();
        let model = super::model_on(&f, &t.wanted().endpoint, &target.name, &f.model);
        let (choice, _, _) = target.folded(&f, &t, 0, &model, None, &Glyphs::of(UNI));
        assert_eq!(choice.effect, "gemini-4-carbon-medium");
        assert!(!models_on(&f, &target, &f.model, reachable).contains(&f.model));
        f.endpoints[ag_index].complete = false;
        assert_eq!(
            super::model_on(&f, &t.wanted().endpoint, "antigravity", &f.model),
            f.model
        );
        f.endpoints[ag_index].complete = true;
        assert_eq!(
            super::model_on(&f, &t.wanted().endpoint, "gemini", &f.model),
            f.model
        );
    }
    for needle in ["9-future-exp", "9-priced-exp", "10-live-exp"] {
        let mut p = on_chat(&t);
        select(&mut p, &f, &t, needle, "antigravity");
        let menu = p.menu(&f, &t, Duration::ZERO, UNI);
        let under = menu.shape.under.unwrap_or_default();
        assert_eq!(
            under.after,
            [
                "On antigravity, earlier reasoning won't carry over.",
                copy::NO_CAPABILITY_DATA_FOR_MODEL,
            ],
            "{needle}"
        );
    }
    f.endpoint_name = Some("antigravity".to_owned());
    f.model = "gemini-4-carbon-medium".to_owned();
    f.effort = "xhigh".to_owned();
    let t = Tracker::new(&f);
    let p = on_chat(&t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    let current = menu.choices.iter().find(|c| c.current).unwrap();
    assert!(current.label.contains("gemini-4-carbon-medium"));
    assert_eq!(current.detail, "");
    assert_eq!(
        switch_to(&f.model, &f, Some(Effort::Xhigh), None),
        Picked::Settings {
            endpoint: None,
            model: Some(f.model.clone()),
            effort: None,
        }
    );
    assert_eq!(
        shown_effort(&f, "gemini-10-live-exp", Some(Effort::Xhigh)),
        Some(Effort::Xhigh)
    );
    let (set, width) = (GlyphSet::Unicode, 120);
    insta::assert_snapshot!(
        format!("no_effort_current_{width}_{set:?}"),
        painted(&p, &f, &t, set, width).join("\n")
    );
    with_levels(
        &mut f,
        "claude-opus-5-5",
        &[Effort::Low, Effort::High],
        Effort::High,
    );
    for effort in [None, Some(Effort::Xhigh)] {
        assert_eq!(display_effort(&f, "claude-opus-5-5", effort), effort);
        assert_eq!(display_effort(&f, "gemini-10-live-exp", effort), effort);
        assert_eq!(display_effort(&f, &f.model, effort), None);
    }
    assert_eq!(
        shown_effort(&f, "claude-opus-5-5", Some(Effort::Xhigh)),
        Some(Effort::High)
    );
    f.models
        .iter_mut()
        .find(|m| m.id == f.model)
        .unwrap()
        .variant = Some(wisp_protocol::settings::EffortVariant {
        family: "gemini-4-carbon".into(),
        effort: Effort::Medium,
    });
    assert_eq!(
        display_effort(&f, &f.model, Some(Effort::Xhigh)),
        Some(Effort::Medium)
    );
}

#[test]
fn a_listed_effort_family_folds_only_matching_ids_and_quotes_sibling_switches() {
    use wisp_protocol::settings::EffortVariant;
    let mut f = live_lists();
    let base = f.model_facts("claude-opus-5-5").unwrap().clone();
    f.models.clear();
    let mut account = f.endpoints[0].clone();
    account.name = "antigravity".into();
    account.provider = "gemini".into();
    account.credential = Some(wisp_render::chrome::Credential::AntigravityPlan);
    account.models.clear();
    for family in ["gemini-4-carbon", "gemini-4-argon", "gemini-3.8-flash"] {
        for effort in [Effort::Medium, Effort::High, Effort::Low] {
            let id = format!("{family}-{}", effort.as_str());
            let mut row = base.clone();
            row.provider = Some("gemini".into());
            row.id = id.clone();
            row.variant = Some(EffortVariant {
                family: family.into(),
                effort,
            });
            let caps = row.caps.as_mut().unwrap();
            caps.efforts = vec![Effort::Low, Effort::Medium, Effort::High];
            caps.default_effort = None;
            caps.per_message_effort = false;
            account.models.push(AccountModel {
                id,
                listed: Some(u16::try_from(account.models.len()).unwrap()),
            });
            f.models.push(row);
        }
    }
    let mut tiered = f.models.last().unwrap().clone();
    tiered.id = "gemini-3.8-flash-tiered".into();
    tiered.variant = None;
    tiered.caps.as_mut().unwrap().efforts.clear();
    account.models.push(AccountModel {
        id: tiered.id.clone(),
        listed: Some(9),
    });
    f.models.push(tiered);
    f.endpoints = vec![account];
    f.endpoint_name = Some("antigravity".into());
    f.provider = "gemini".into();
    f.credential = Some(wisp_render::chrome::Credential::AntigravityPlan);
    f.model = "gemini-4-carbon-medium".into();
    f.effort = "medium".into();
    let mut t = warmed(&f);
    let p = on_chat(&t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        shown(&menu),
        [
            "antigravity",
            "  gemini-4-carbon",
            "  gemini-4-argon",
            "  gemini-3.8-flash",
            "  gemini-3.8-flash-tiered",
            "+ Sign in to another account…"
        ]
    );
    assert_eq!(menu.choices[1].detail, "medium");
    for (width, set) in [(120, UNI), (40, GlyphSet::Ascii)] {
        insta::assert_snapshot!(
            format!("gemini_families_{width}_{set:?}"),
            painted(&p, &f, &t, set, width).join("\n")
        );
    }
    let mut filtered = p.clone().queried("high");
    let values = filtered.values(&f, &t);
    assert_eq!(
        values
            .iter()
            .filter(|v| v.enabled)
            .map(|v| v.text.as_str())
            .collect::<Vec<_>>(),
        [
            "gemini-4-carbon-high",
            "gemini-4-argon-high",
            "gemini-3.8-flash-high"
        ]
    );
    assert_eq!(values[1].label, "gemini-4-carbon");
    assert_eq!(
        filtered.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: None,
            model: Some("gemini-4-carbon-high".into()),
            effort: None
        })
    );
    assert_eq!(
        switch_to("gemini-4-argon-low", &f, Some(Effort::Xhigh), None),
        Picked::Settings {
            endpoint: None,
            model: Some("gemini-4-argon-low".into()),
            effort: None
        }
    );
    assert_eq!(
        t.effort_effect(&f, &f.model, Effort::High, Duration::ZERO),
        t.model_effect(&f, "gemini-4-carbon-high", Duration::ZERO)
    );
    assert!(matches!(
        t.effort_effect(&f, &f.model, Effort::High, Duration::ZERO),
        Effect::Rebuild(_) | Effect::Switch(_, _)
    ));
    let effort = Picker::efforts(f.model.clone(), false, Some(Effort::Medium)).menu(
        &f,
        &t,
        Duration::ZERO,
        UNI,
    );
    assert_eq!(shown(&effort), ["low", "medium", "high"]);
    assert!(effort.choices[2].effect.contains("cache rebuild"));
    t.apply(
        &settings_changed(Changed {
            endpoint: "antigravity".into(),
            model: "gemini-4-carbon-high".into(),
            effort: Effort::High,
            ..Changed::default()
        }),
        &f,
        Duration::ZERO,
        UNI,
    );
    assert_eq!(
        t.effort_effect(&f, "gemini-4-carbon-high", Effort::Medium, Duration::ZERO),
        Effect::Kept
    );
    f.endpoints[0].models.clear();
    let ids: Vec<_> = on_chat(&t)
        .values(&f, &t)
        .into_iter()
        .filter(|v| v.enabled && v.text.starts_with("gemini-"))
        .collect();
    assert_eq!(ids.len(), 10);
    assert!(ids.iter().all(|v| v.label == v.text));

    let hydrogen_efforts = [Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh];
    for effort in hydrogen_efforts {
        let id = format!("gemini-4-hydrogen-{}", effort.as_str());
        let mut row = base.clone();
        row.provider = Some("gemini".into());
        row.id = id.clone();
        row.variant = Some(EffortVariant {
            family: "gemini-4-hydrogen".into(),
            effort,
        });
        let caps = row.caps.as_mut().unwrap();
        caps.efforts = hydrogen_efforts.to_vec();
        caps.default_effort = None;
        caps.per_message_effort = false;
        f.models.push(row);
    }
    f.endpoints[0].models = f
        .models
        .iter()
        .enumerate()
        .map(|(i, m)| AccountModel {
            id: m.id.clone(),
            listed: Some(u16::try_from(i).unwrap()),
        })
        .collect();
    f.model = "gemini-4-hydrogen-high".into();
    f.effort = "high".into();
    let t = warmed(&f);
    let p = on_chat(&t);
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert!(shown(&menu).contains(&"  gemini-4-hydrogen".to_owned()));
    assert_eq!(
        shown(
            &Picker::efforts(f.model.clone(), false, Some(Effort::High)).menu(
                &f,
                &t,
                Duration::ZERO,
                UNI
            )
        ),
        ["low", "medium", "high", "xhigh"]
    );
    let xhigh_values = p.clone().queried("xhigh").values(&f, &t);
    assert_eq!(
        xhigh_values
            .iter()
            .filter(|v| v.enabled)
            .map(|v| (v.text.as_str(), v.label.as_str()))
            .collect::<Vec<_>>(),
        [("gemini-4-hydrogen-xhigh", "gemini-4-hydrogen")]
    );
    for effort in hydrogen_efforts {
        let sibling = format!("gemini-4-hydrogen-{}", effort.as_str());
        assert_eq!(
            switch_to(&sibling, &f, Some(Effort::Max), None),
            Picked::Settings {
                endpoint: None,
                model: Some(sibling.clone()),
                effort: None
            }
        );
        if effort != Effort::High {
            assert_eq!(
                t.effort_effect(&f, &f.model, effort, Duration::ZERO),
                t.model_effect(&f, &sibling, Duration::ZERO)
            );
        }
    }
}

#[test]
fn an_account_row_shows_prices_and_sends_its_remembered_pair() {
    use wisp_protocol::settings::{Remembered, SwitchQuote, SwitchTarget};
    let f = endpoint_facts();
    let mut t = warmed(&f);
    let last = Remembered {
        model: "claude-sonnet-5-5".into(),
        effort: Effort::High,
    };
    let snapshot = crate::app::settings::tests::switch::snapshot_of(vec![
        SwitchQuote {
            target: SwitchTarget::Endpoint("direct".into()),
            remembered: Some(last.clone()),
            consequence: None,
            retry_compaction: None,
        },
        SwitchQuote {
            target: SwitchTarget::Endpoint("manifest".into()),
            remembered: Some(last.clone()),
            consequence: None,
            retry_compaction: None,
        },
    ]);
    t.apply(&snapshot, &f, Duration::ZERO, UNI);
    let mut p = Picker::endpoints().queried("direct");
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(menu.choices[0].effect, "sonnet-5-5 high");
    let under = menu.shape.under.unwrap();
    assert!(
        under.first.iter().any(|line| line.contains("direct")),
        "{under:?}"
    );
    assert!(
        under
            .parts
            .iter()
            .any(|(text, _)| text == &t.endpoint_effect(&f, "direct", Duration::ZERO).text(UNI)),
        "{under:?}"
    );
    assert_eq!(
        p.key(key(KeyCode::Enter), &f, &t),
        Step::Done(Picked::Settings {
            endpoint: Some("direct".into()),
            model: None,
            effort: None
        })
    );
    let pair = shown_pair(&f, &t, "manifest");
    assert_eq!(
        (pair.model, pair.effort, pair.remembered),
        (f.model.clone(), f.effort_level(), false)
    );
    let mut p = Picker::endpoints().queried("manifest");
    let menu = p.menu(&f, &t, Duration::ZERO, UNI);
    assert_eq!(
        menu.shape.under.unwrap().first,
        [account::THIS_CHAT_NOW.to_owned()]
    );
    assert_eq!(p.key(key(KeyCode::Enter), &f, &t), Step::Close);
}
