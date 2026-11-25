use wisp_plugin_api::config::LimitWindow;

use super::*;

fn row(name: &str, value: &str, layer: &str) -> SettingRow {
    SettingRow {
        name: name.to_owned(),
        value: value.to_owned(),
        layer: layer.to_owned(),
        help: format!("What {name} does."),
        rule: format!("the rule for {name}"),
    }
}

#[test]
fn every_setting_is_one_line_with_its_value_and_layer() {
    let rows = [
        row("cache_ttl", "1h", "default"),
        row("compact_at", "400k", "config"),
        row("effort", "high", "session"),
    ];
    assert_eq!(
        listed(&rows, None),
        "3 settings, with where each value comes from:\n- cache_ttl: 1h (default)\n- compact_at: 400k (config)\n- effort: high (session)"
    );
}

#[test]
fn a_filtered_list_adds_what_each_setting_does_and_the_values_it_takes() {
    assert_eq!(
        listed(&[row("compact_at", "400k", "config")], Some("compact")),
        "1 setting, with where each value comes from:\n- compact_at: 400k (config)\n  What compact_at does.\n  the rule for compact_at"
    );
}

#[test]
fn a_filter_that_matches_nothing_says_how_to_list_them_all() {
    assert_eq!(
        listed(&[], Some("xyz")),
        "No setting matches `xyz`; leave filter out to list them all."
    );
    assert_eq!(
        listed(&[], None),
        "0 settings, with where each value comes from:"
    );
}

const NOW: i64 = 1_800_000_000;

fn window(label: &str, used: Option<f64>, resets_in: Option<i64>, age: i64) -> LimitWindow {
    LimitWindow {
        label: label.to_owned(),
        used_percent: used,
        resets_at: resets_in.map(|after| NOW + after),
        observed_at: NOW - age,
    }
}

fn account(name: &str, kind: &str) -> AccountRow {
    AccountRow {
        name: name.to_owned(),
        kind: Some(kind.to_owned()),
        models: vec!["opus-5-5".to_owned()],
        ..AccountRow::default()
    }
}

fn limits(windows: Vec<LimitWindow>, limited: bool, problem: Option<&str>) -> Option<Limits> {
    Some(Limits {
        windows,
        limited,
        problem: problem.map(str::to_owned),
    })
}

fn list(rows: Vec<AccountRow>) -> AccountList {
    AccountList {
        model: "opus-5-5".to_owned(),
        effort: "high".to_owned(),
        rows,
    }
}

#[test]
fn the_accounts_read_as_account_shows_them_with_models_and_limits() {
    let rows = vec![
        AccountRow {
            sign_in: Some(SignIn::Working),
            yours: true,
            start: true,
            models: vec!["opus-5-5".into(), "sonnet-5-5".into()],
            limits: limits(
                vec![
                    window("5-hour", Some(43.0), Some(7_800), 120),
                    window("Weekly", Some(12.5), Some(367_200), 60),
                    window("Fable weekly", Some(100.0), Some(367_200), 60),
                ],
                false,
                None,
            ),
            ..account("claude-work", "Claude subscription")
        },
        account("claude-api", "Claude API"),
        AccountRow {
            sign_in: Some(SignIn::Expired),
            models: vec!["gpt-6.1-sol".into()],
            limits: limits(
                Vec::new(),
                false,
                Some("sign in again to read subscription usage"),
            ),
            ..account("chatgpt", "ChatGPT subscription")
        },
        AccountRow {
            unavailable: true,
            ..account("corp", "Gateway, API")
        },
    ];
    assert_eq!(
        accounts(&list(rows), NOW),
        "You run on claude-work, model opus-5-5, effort high. New chats start on claude-work.\n\
         4 accounts:\n\
         - claude-work · Claude subscription · signed in\n  models: opus-5-5, sonnet-5-5\n  \
         limits used, read 2m00s ago: 5-hour 43%, resets in 2h10m · Weekly 12.5%, resets in 4d06h · Fable weekly 100%, resets in 4d06h\n\
         - claude-api · Claude API\n  models: opus-5-5\n\
         - chatgpt · ChatGPT subscription · sign-in expired\n  models: gpt-6.1-sol\n  \
         limits: sign in again to read subscription usage\n\
         - corp · Gateway, API · unavailable (/account says why)\n  models: opus-5-5\n\
         /account switches this chat's account."
    );
}

#[test]
fn each_sign_in_state_has_its_words() {
    for (state, words) in [
        (SignIn::Working, "signed in"),
        (SignIn::Refreshable, "signed in"),
        (SignIn::Expired, "sign-in expired"),
        (SignIn::Unavailable, "not signed in"),
        (SignIn::ExpiryUnknown, "signed in with a setup token"),
    ] {
        assert_eq!(sign_in_words(state), words, "{state:?}");
    }
}

#[test]
fn a_limits_line_says_what_the_cache_holds_and_nothing_it_does_not() {
    let line = |limits: Option<Limits>| {
        let row = AccountRow {
            limits,
            models: Vec::new(),
            ..account("a", "Claude subscription")
        };
        accounts(&list(vec![row]), NOW)
            .lines()
            .find_map(|l| l.strip_prefix("  "))
            .map(str::to_owned)
    };
    for (limits, expected) in [
        (
            limits(Vec::new(), false, None),
            Some("limits: not read yet"),
        ),
        (
            limits(Vec::new(), true, None),
            Some("limits: limit reached"),
        ),
        (
            limits(
                vec![
                    window("5-hour", None, Some(30), 5),
                    window("Weekly", Some(100.0), Some(0), 90_000),
                ],
                true,
                Some("subscription usage lookup is throttled"),
            ),
            Some(
                "limits used, read 25h00m ago: limit reached · 5-hour unknown, resets in 30s · Weekly 100% · subscription usage lookup is throttled",
            ),
        ),
    ] {
        assert_eq!(line(limits.clone()).as_deref(), expected, "{limits:?}");
    }
}

#[test]
fn a_reset_reads_as_list_agents_times_under_a_day_and_in_days_from_one() {
    for (seconds, words) in [
        (45, "45s"),
        (725, "12m05s"),
        (86_399, "23h59m"),
        (86_400, "1d00h"),
        (367_200, "4d06h"),
    ] {
        assert_eq!(resets_in(seconds), words, "{seconds}");
        assert_eq!(
            reset_time(Some(NOW + i64::try_from(seconds).unwrap()), NOW).as_deref(),
            Some(words),
            "{seconds}",
        );
    }
    for at in [None, Some(NOW - 1), Some(NOW), Some(i64::MIN)] {
        assert_eq!(reset_time(at, NOW), None, "{at:?}");
    }
}

#[test]
fn the_header_and_count_cover_no_account_of_yours_no_default_and_one_or_none() {
    let mut other = account("solo", "Claude API");
    other.models.clear();
    assert_eq!(
        accounts(&list(vec![other]), NOW),
        "You run on model opus-5-5, effort high.\n1 account:\n- solo · Claude API\n/account switches this chat's account."
    );
    assert_eq!(
        accounts(&list(Vec::new()), NOW),
        "You run on model opus-5-5, effort high.\nNo accounts.\n/account switches this chat's account."
    );
}
