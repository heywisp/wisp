use wisp_protocol::RequestId;
use wisp_protocol::settings::{
    FileState, Layer, Place, ResolvedSetting, SETTINGS, SavedValue, Secrecy, SettingName,
    SettingValue, SettingsSnapshot, Shadowed, SitePath, SnapshotContext, Status, find,
};

use super::*;
use crate::term::caps::{Caps, Env, Flags, SavedDisplay, Streams};

const MODEL: &str = "claude-opus-5-5";

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

fn setting(name: &str, value: Option<SettingValue>, layer: Layer, origin: &str) -> ResolvedSetting {
    ResolvedSetting {
        name: SettingName::of(find(name).unwrap()),
        value,
        layer,
        origin: origin.to_owned(),
        shadowed: Vec::new(),
        choices: Vec::new(),
        applies: true,
    }
}

fn global(name: &str) -> SitePath {
    SitePath::new(Place::Global, find(name).unwrap())
}

fn snapshot(
    settings: Vec<ResolvedSetting>,
    saved: Vec<SavedValue>,
    file: FileState,
) -> SettingsSnapshot {
    SettingsSnapshot {
        often: Vec::new(),
        answers: RequestId(1),
        context: SnapshotContext {
            endpoint: "manifest".to_owned(),
            model: MODEL.to_owned(),
            agent: None,
            max_tokens: 64_000,
            window: None,
            room: None,
        },
        settings,
        saved,
        endpoints: Vec::new(),
        models: Vec::new(),
        file,
        switches: Vec::new(),
        compaction: None,
        plugins: Vec::new(),
        placed: Vec::new(),
    }
}

fn ok() -> FileState {
    FileState::Ok {
        path_display: "~/.config/wisp/config.toml".to_owned(),
    }
}

fn effort_from_the_file(value: &str) -> ResolvedSetting {
    let mut effort = setting("effort", Some(text("high")), Layer::Flag, "--effort");
    effort.shadowed.push(Shadowed {
        layer: Layer::Config,
        value: text(value),
        origin: "effort".to_owned(),
    });
    effort
}

fn row_of<'a>(rows: &'a [Row], name: &str) -> &'a Row {
    rows.iter().find(|r| r.spec.name == name).unwrap()
}

#[test]
fn every_placed_name_is_a_registered_setting() {
    for name in ORDER {
        assert!(find(name).is_some(), "{name}");
    }
    for name in EVERYDAY {
        assert!(ORDER.contains(name), "{name}");
    }
    let fields: std::collections::BTreeSet<&str> =
        entries::ENDPOINT_FIELDS.iter().copied().collect();
    let endpoint_specs: std::collections::BTreeSet<&str> = SETTINGS
        .iter()
        .filter(|s| {
            s.is_built()
                && s.has_site(wisp_protocol::settings::Site::Endpoint)
                && !s.has_site(wisp_protocol::settings::Site::Global)
                && !OFF_SCREEN.contains(&s.name)
        })
        .map(|s| s.name)
        .collect();
    assert_eq!(fields, endpoint_specs);
}

#[test]
fn every_built_plain_setting_has_one_place_or_waits_for_an_entry_pane() {
    let placed: Vec<&str> = placed().map(|(_, spec)| spec.name).collect();
    let mut once = placed.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!(once.len(), placed.len(), "a name is placed twice");
    for spec in SETTINGS
        .iter()
        .filter(|s| s.status == Status::Built && !s.per_plugin())
    {
        let shown = placed.contains(&spec.name)
            || ENTRY_PANES.contains(&spec.name)
            || OFF_SCREEN.contains(&spec.name);
        match spec.secrecy {
            Secrecy::Plain => assert!(shown, "{} has no place", spec.name),
            Secrecy::Secret | Secrecy::Hidden => {
                assert!(!placed.contains(&spec.name), "{} is not shown", spec.name);
            }
        }
    }
    for name in ENTRY_PANES.iter().chain(OFF_SCREEN) {
        assert!(!placed.contains(name), "{name}");
    }
    let pages = entries::ENDPOINT_FIELDS
        .iter()
        .chain(ENTRY_PANES)
        .copied()
        .chain(entries::model_specs().map(|spec| spec.name));
    for name in pages {
        assert!(!OFF_SCREEN.contains(&name), "{name}");
    }
}

fn caps(pairs: &[(&str, &str)], stdout_tty: bool, ascii: bool, saved: &SavedDisplay) -> Caps {
    let mut all = vec![("TERM", "xterm-256color"), ("LANG", "en_US.UTF-8")];
    all.extend_from_slice(pairs);
    Caps::resolve_saved(
        &Env::from_pairs(all),
        Streams {
            stdin_tty: true,
            stdout_tty,
            stderr_tty: stdout_tty,
        },
        Flags { ascii },
        saved,
        None,
    )
}

fn plain(pairs: &[(&str, &str)]) -> Caps {
    caps(pairs, true, false, &SavedDisplay::default())
}

#[test]
fn the_terminal_s_own_sources_name_the_layer_that_set_them() {
    let forced = Set::Layer(Layer::Forced);
    let flag = Set::Layer(Layer::Flag);
    let env = Set::Layer(Layer::Env);
    let saved_flags = SavedDisplay::default().with_flags([("display.motion", "calm")]);
    let saved_file = SavedDisplay::from_pairs([("display.theme", "light")]);
    let dumb = plain(&[("TERM", "dumb")]);
    let piped = caps(&[], false, false, &SavedDisplay::default());
    for (source, want) in [
        (plain(&[("NO_COLOR", "1")]).depth.source, forced.clone()),
        (dumb.glyphs.source, forced.clone()),
        (dumb.motion.source, forced.clone()),
        (piped.motion.source.clone(), forced.clone()),
        (piped.depth.source, forced.clone()),
        (plain(&[("LANG", "C")]).glyphs.source, forced),
        (
            caps(&[], true, true, &SavedDisplay::default())
                .glyphs
                .source,
            flag.clone(),
        ),
        (caps(&[], true, false, &saved_flags).motion.source, flag),
        (plain(&[("WISP_THEME", "light")]).mode.source, env.clone()),
        (plain(&[("WISP_MOUSE", "0")]).mouse.source, env.clone()),
        (plain(&[("FORCE_COLOR", "3")]).depth.source, env.clone()),
        (plain(&[("FORCE_COLOR", "1")]).depth.source, env.clone()),
        (
            caps(
                &[("CLICOLOR_FORCE", "1")],
                false,
                false,
                &SavedDisplay::default(),
            )
            .depth
            .source,
            env.clone(),
        ),
        (plain(&[("CLICOLOR", "0")]).depth.source, env),
        (
            caps(&[], true, false, &saved_file).mode.source,
            Set::Layer(Layer::Config),
        ),
        (plain(&[]).mode.source, Set::Layer(Layer::Default)),
        (plain(&[]).full_repaint.source, Set::Layer(Layer::Default)),
        (
            plain(&[("COLORTERM", "truecolor")]).depth.source,
            Set::Detected,
        ),
        (plain(&[("COLORFGBG", "15;0")]).mode.source, Set::Detected),
        (plain(&[]).depth.source, Set::Detected),
    ] {
        assert_eq!(display_set(&source), want, "{source}");
    }
}

#[test]
fn a_row_shows_the_winner_and_the_layers_under_it_without_the_default() {
    let mut effort = effort_from_the_file("medium");
    effort.shadowed.push(Shadowed {
        layer: Layer::Default,
        value: text("medium"),
        origin: "default".to_owned(),
    });
    let saved = vec![SavedValue::of(global("effort"), text("medium"), Some(3))];
    let rows = rows(&snapshot(vec![effort], saved, ok()), &Display::default());
    let effort = row_of(&rows, "effort");
    assert_eq!(
        (effort.value.as_str(), effort.source.as_str()),
        ("high", "flag")
    );
    assert_eq!(effort.shadowed.len(), 1);
    assert_eq!(effort.shadowed[0].word(), "config");
    assert_eq!(effort.shadowed[0].line, Some(3));
    assert_eq!(effort.changed, None);
}

#[test]
fn a_const_default_is_no_winner_and_an_unset_rule_says_how_it_is_worked_out() {
    let settings = vec![
        setting("cache_ttl", Some(text("1h")), Layer::Default, "default"),
        setting(
            "compact_at",
            None,
            Layer::Default,
            "window \u{2212} reserve",
        ),
    ];
    let mut known = snapshot(settings, Vec::new(), ok());
    known.context.window = Some(200_000);
    let rows = rows(&known, &Display::default());
    let ttl = row_of(&rows, "cache_ttl");
    assert_eq!((ttl.value.as_str(), ttl.source.as_str()), ("1h", DEFAULT));
    assert!(ttl.winner.is_none());
    let compact = row_of(&rows, "compact_at");
    assert_eq!(compact.source, RULE);
    let point = wisp_protocol::settings::default_point(200_000, 64_000);
    assert!(
        compact
            .value
            .starts_with(&wisp_protocol::settings::thousands(point)),
        "{}",
        compact.value
    );
    let unknown = super::rows::rows(
        &snapshot(
            vec![setting("compact_at", None, Layer::Default, "default")],
            Vec::new(),
            ok(),
        ),
        &Display::default(),
    );
    assert_eq!(unknown[0].value, NOT_SET);
    assert!(unknown[0].default.is_empty());
}

#[test]
fn a_changed_file_shows_the_new_value_or_that_it_is_gone() {
    let changed = |file: FileState, saved: Vec<SavedValue>| {
        let rows = rows(
            &snapshot(vec![effort_from_the_file("medium")], saved, file),
            &Display::default(),
        );
        row_of(&rows, "effort").changed.clone()
    };
    let max = vec![SavedValue::of(global("effort"), text("max"), Some(3))];
    let same = vec![SavedValue::of(global("effort"), text("medium"), Some(3))];
    let elsewhere = vec![
        SavedValue::of(global("effort"), text("medium"), Some(3)),
        SavedValue::of(
            SitePath::new(
                Place::Model("claude-sonnet-5-5".to_owned()),
                find("effort").unwrap(),
            ),
            text("low"),
            Some(9),
        ),
    ];
    assert_eq!(
        changed(ok(), max.clone()),
        Some(Changed::Now("max".to_owned()))
    );
    assert_eq!(
        changed(FileState::Missing, max.clone()),
        Some(Changed::Now("max".to_owned()))
    );
    assert_eq!(changed(ok(), same), None);
    assert_eq!(changed(ok(), elsewhere), None);
    assert_eq!(changed(ok(), Vec::new()), Some(Changed::Removed));
    let broken = FileState::Broken {
        line: Some(12),
        reason: "expected a value".to_owned(),
    };
    assert_eq!(changed(broken, max), None);
}

#[test]
fn the_terminal_s_resolution_wins_and_the_kernel_winner_moves_under_it() {
    let mut theme = setting(
        "display.theme",
        Some(text("light")),
        Layer::Config,
        "display.theme",
    );
    theme.shadowed.push(Shadowed {
        layer: Layer::Default,
        value: text("dark"),
        origin: "default".to_owned(),
    });
    let motion = setting(
        "display.motion",
        Some(text("reduced")),
        Layer::Env,
        "WISP_MOTION",
    );
    let mut display = Display::default();
    display.set("display.theme", text("dark"), "NO_COLOR is set");
    display.set("display.motion", text("reduced"), "WISP_MOTION=reduced");
    let rows = rows(&snapshot(vec![theme, motion], Vec::new(), ok()), &display);
    let theme = row_of(&rows, "display.theme");
    assert_eq!(
        (theme.value.as_str(), theme.source.as_str()),
        ("dark", "forced")
    );
    let under: Vec<String> = theme.shadowed.iter().map(Found::word).collect();
    assert_eq!(under, ["config"]);
    let motion = row_of(&rows, "display.motion");
    assert_eq!(motion.source, "variable");
    assert!(motion.shadowed.is_empty(), "{:?}", motion.shadowed);
}

#[test]
fn under_a_forced_layer_the_session_and_saved_values_are_both_not_used() {
    let theme = setting(
        "display.theme",
        Some(text("light")),
        Layer::Config,
        "display.theme",
    );
    let mut display = Display::default();
    display.set("display.theme", text("dark"), "NO_COLOR is set");
    display.set_session("display.theme", "light");
    let first = rows(&snapshot(vec![theme.clone()], Vec::new(), ok()), &display);
    let row = row_of(&first, "display.theme");
    let under: Vec<(String, String)> = row
        .shadowed
        .iter()
        .map(|f| (f.word(), f.value.clone()))
        .collect();
    assert_eq!(
        under,
        [
            ("session".to_owned(), "light".to_owned()),
            ("config".to_owned(), "light".to_owned())
        ]
    );
    let mut stale = theme;
    stale.layer = Layer::Session;
    stale.origin = crate::term::caps::SESSION_SOURCE.to_owned();
    stale.value = Some(text("dark"));
    let second = rows(&snapshot(vec![stale], Vec::new(), ok()), &display);
    let row = row_of(&second, "display.theme");
    let under: Vec<String> = row.shadowed.iter().map(|f| f.value.clone()).collect();
    assert_eq!(
        under,
        ["light"],
        "the TUI's own session value replaces a stale one"
    );
}

#[test]
fn a_display_value_left_at_its_default_reads_default() {
    let theme = setting(
        "display.theme",
        Some(text("light")),
        Layer::Config,
        "display.theme",
    );
    let mut display = Display::default();
    display.set("display.theme", text("dark"), "default");
    let rows = rows(&snapshot(vec![theme], Vec::new(), ok()), &display);
    let theme = row_of(&rows, "display.theme");
    assert_eq!(
        (theme.value.as_str(), theme.source.as_str()),
        ("dark", DEFAULT)
    );
    assert!(theme.winner.is_none());
    assert_eq!(theme.shadowed[0].word(), "config");
}

#[test]
fn the_filter_matches_label_name_or_help_in_any_case() {
    let rows = rows(
        &snapshot(
            vec![
                setting("cache_ttl", Some(text("1h")), Layer::Default, "default"),
                setting(
                    "display.verbose",
                    Some(SettingValue::Bool(false)),
                    Layer::Default,
                    "default",
                ),
                setting("effort", Some(text("high")), Layer::Flag, "--effort"),
            ],
            Vec::new(),
            ok(),
        ),
        &Display::default(),
    );
    let kept = |filter: &str| -> Vec<&str> {
        rows.iter()
            .filter(|r| r.spec.matches(&Query::new(filter)))
            .map(|r| r.spec.name)
            .collect()
    };
    assert_eq!(kept("LIFETIME"), ["cache_ttl"]);
    assert_eq!(kept("cache_ttl"), ["cache_ttl"]);
    assert_eq!(kept(" cache "), ["cache_ttl", "display.verbose"]);
    assert_eq!(kept(""), ["effort", "cache_ttl", "display.verbose"]);
    assert!(kept("zzz").is_empty());
}

#[test]
fn opening_on_a_name_selects_it_and_any_other_word_filters() {
    let on = Screen::opening(RequestId(4), " effort ");
    assert_eq!(on.asked(), Some(RequestId(4)));
    assert_eq!(on.filter(), "");
    assert!(on.loading());
    let mut behind = Screen::opening(RequestId(1), "time");
    assert_eq!(behind.current, Page::Group(Section::Chat));
    assert!(behind.answered(
        &snapshot(
            vec![setting(
                "time",
                Some(text("local")),
                Layer::Default,
                "default"
            )],
            Vec::new(),
            ok()
        ),
        &Display::default()
    ));
    assert_eq!(behind.selected_spot(), Some(&Spot::Setting("time")));
    let mut entry = Screen::opening(RequestId(1), names::CONTEXT_WINDOW);
    assert!(entry.answered(&snapshot(Vec::new(), Vec::new(), ok()), &Display::default()));
    assert_eq!(entry.opened(), Some(&Spot::Model(MODEL.to_owned())));
    assert_eq!(entry.selected().unwrap().spec.name, names::CONTEXT_WINDOW);
    assert_eq!(
        Screen::opening(RequestId(1), names::PRICING).current,
        Page::Accounts
    );
    let filtered = Screen::opening(RequestId(5), "cache");
    assert_eq!(filtered.filter(), "cache");
    assert_eq!(Screen::opening(RequestId(6), "").filter(), "");
    assert_eq!(named("key"), None);
    assert_eq!(named("display.theme"), Some("display.theme"));
}

#[test]
fn only_the_answer_to_the_last_question_fills_the_screen() {
    let mut screen = Screen::opening(RequestId(2), "effort");
    let mut stale = snapshot(vec![effort_from_the_file("medium")], Vec::new(), ok());
    stale.answers = RequestId(1);
    assert!(!screen.answered(&stale, &Display::default()));
    assert!(screen.loading());
    let mut fresh = stale.clone();
    fresh.answers = RequestId(2);
    assert!(screen.answered(&fresh, &Display::default()));
    assert!(!screen.loading());
    assert_eq!(screen.asked(), None);
    assert_eq!(screen.selected().map(|r| r.spec.name), Some("effort"));
}

#[test]
fn the_forced_display_rows_are_what_no_color_and_a_dumb_terminal_force() {
    let tty = Streams {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
    };
    let names = |pairs: &[(&str, &str)]| -> Vec<(&'static str, String)> {
        forced_display(
            &Env::from_pairs(pairs.iter().copied()),
            tty,
            Flags::default(),
        )
        .into_iter()
        .map(|(name, row)| (name, row.source))
        .collect()
    };
    let utf8 = ("LANG", "en_US.UTF-8");
    assert_eq!(names(&[utf8, ("TERM", "xterm-256color")]), []);
    assert_eq!(
        names(&[utf8, ("TERM", "xterm-256color"), ("NO_COLOR", "1")]),
        [("display.color", "NO_COLOR is set".to_owned())]
    );
    assert_eq!(
        names(&[utf8, ("TERM", "dumb")]),
        [
            ("display.ambiguous", "TERM=dumb".to_owned()),
            ("display.motion", "TERM=dumb".to_owned())
        ]
    );
    let latin = names(&[("LANG", "en_US.ISO-8859-1"), ("TERM", "xterm-256color")]);
    assert_eq!(
        latin.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["display.ambiguous"],
        "{latin:?}"
    );
    let piped = forced_display(
        &Env::from_pairs([utf8, ("TERM", "xterm-256color")]),
        Streams {
            stdin_tty: true,
            stdout_tty: false,
            stderr_tty: false,
        },
        Flags::default(),
    );
    assert_eq!(
        piped
            .iter()
            .map(|(name, row)| (*name, row.source.as_str()))
            .collect::<Vec<_>>(),
        [("display.motion", "stdout is not a terminal")]
    );
}

mod edit;

#[test]
fn every_display_choice_reads_back_as_its_word() {
    use wisp_protocol::settings::{Kind, Owner};

    let mut seen = 0;
    for spec in SETTINGS.iter().filter(|s| s.owner == Owner::Client) {
        let Kind::Choice(words) = spec.kind else {
            continue;
        };
        for word in words.iter().filter(|w| **w != "auto") {
            let saved = SavedDisplay::default().with_flags([(spec.name, *word)]);
            let caps = caps(&[], true, false, &saved);
            let copy = caps
                .copy_on_select
                .clone()
                .unwrap_or_else(|| crate::term::caps::Choice::new(false, "default"));
            let display = Display::from_caps(&caps, &copy);
            let row = display.get(spec.name).unwrap();
            assert_eq!(spec.shown(&row.value), *word, "{}", spec.name);
            seen += 1;
        }
    }
    assert!(seen > 0);
    for word in wisp_protocol::settings::KEY_STORES {
        let spec = find("key_store").unwrap();
        let edit = entries::connection_edit(spec, Some(&text(word)));
        assert!(edit.is_some_and(|(_, store)| store.is_some()), "{word}");
    }
}
