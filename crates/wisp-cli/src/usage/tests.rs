use super::*;

const CANARY: &str = "sk-ant-CANARYUSAGE0001";

fn parse(args: &[&str]) -> Result<crate::Args, Failure> {
    let mut all = vec![OsString::from("wisp")];
    all.extend(args.iter().map(OsString::from));
    parse_from::<crate::Args, _>(all)
}

fn refused(args: &[&str]) -> Failure {
    match parse(args) {
        Ok(_) => panic!("{args:?} parsed"),
        Err(failure) => failure,
    }
}

fn assert_usage(failure: &Failure) {
    assert_eq!(failure.code, 2, "{}", failure.text);
    assert!(!failure.stdout);
    assert!(!failure.text.contains("CANARY"), "{}", failure.text);
}

#[test]
fn every_choice_list_round_trips() {
    for effort in Effort::ALL {
        assert_eq!(
            parse(&["--effort", effort.as_str()]).unwrap().effort,
            Some(effort)
        );
    }
    for ttl in CacheTtl::ALL {
        assert_eq!(
            parse(&["--cache-ttl", ttl.as_str()]).unwrap().cache_ttl,
            Some(ttl)
        );
    }
    for time in MessageTime::ALL {
        assert_eq!(parse(&["--time", time.as_str()]).unwrap().time, Some(time));
    }
}

#[test]
fn every_variant_is_in_its_all_list() {
    for effort in [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ] {
        match effort {
            Effort::Low => assert!(Effort::ALL.contains(&Effort::Low)),
            Effort::Medium => assert!(Effort::ALL.contains(&Effort::Medium)),
            Effort::High => assert!(Effort::ALL.contains(&Effort::High)),
            Effort::Xhigh => assert!(Effort::ALL.contains(&Effort::Xhigh)),
            Effort::Max => assert!(Effort::ALL.contains(&Effort::Max)),
        }
    }
    for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
        match ttl {
            CacheTtl::FiveMinutes => assert!(CacheTtl::ALL.contains(&CacheTtl::FiveMinutes)),
            CacheTtl::OneHour => assert!(CacheTtl::ALL.contains(&CacheTtl::OneHour)),
        }
    }
    for time in [MessageTime::Local, MessageTime::Utc, MessageTime::Off] {
        match time {
            MessageTime::Local => assert!(MessageTime::ALL.contains(&MessageTime::Local)),
            MessageTime::Utc => assert!(MessageTime::ALL.contains(&MessageTime::Utc)),
            MessageTime::Off => assert!(MessageTime::ALL.contains(&MessageTime::Off)),
        }
    }
}

#[test]
fn a_bad_value_is_never_quoted_and_lists_the_choices() {
    let failure = refused(&["--effort", CANARY]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp: --effort must be one of low, medium, high, xhigh, max (what was given isn't shown)"
    );
    let failure = refused(&["--time", CANARY]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp: --time must be one of local, utc, off (what was given isn't shown)"
    );
    let failure = refused(&["--cache-ttl", CANARY]);
    assert_usage(&failure);
    assert!(
        failure.text.contains("must be 5m or 1h"),
        "{}",
        failure.text
    );
    let failure = refused(&["--output", CANARY]);
    assert_usage(&failure);
    assert!(failure.text.starts_with("wisp: --output must be one of "));
    let failure = refused(&["--retry-budget", CANARY]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp: --retry-budget needs a number of dollars, such as 0.50, or none (what was given isn't shown)"
    );
}

#[test]
fn stray_arguments_and_commands_are_never_quoted() {
    let failure = refused(&[CANARY]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp: unknown command (not shown, in case it is a key); see wisp --help"
    );
    let failure = refused(&["--zzzzzzzz", CANARY]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp: unexpected argument (not shown, in case it is a key); see wisp --help"
    );
    let failure = refused(&["endpoint", "add", "a", CANARY]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp account add: unexpected argument (not shown, in case it is a key); see wisp account add --help"
    );
    let failure = refused(&["endpoint", CANARY]);
    assert_usage(&failure);
    assert!(
        failure.text.starts_with("wisp account: unknown command"),
        "{}",
        failure.text
    );
    let failure = refused(&["endpoint", "add", "--auth", CANARY]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp account add: --auth must be one of api_key, bearer, chatgpt_subscription, anthropic_subscription, antigravity_subscription (what was given isn't shown)"
    );
}

#[test]
fn tips_name_only_known_flags_and_values() {
    let failure = refused(&["--efort", "high"]);
    assert_usage(&failure);
    assert!(
        failure
            .text
            .ends_with("\n\n  tip: a similar argument exists: --effort"),
        "{}",
        failure.text
    );
    let failure = refused(&["--sk-ant-CANARYUSAGE0001"]);
    assert_usage(&failure);
    assert!(!failure.text.contains("tip:"), "{}", failure.text);
}

#[test]
fn missing_values_and_commands_are_named() {
    let failure = refused(&["endpoint", "remove"]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp account remove: missing NAME; see wisp account remove --help"
    );
}

#[test]
fn help_and_version_pass_through() {
    let failure = refused(&["--help"]);
    assert_eq!(failure.code, 0);
    assert!(failure.stdout);
    assert!(failure.text.contains("Usage: wisp"));
    let failure = refused(&["--version"]);
    assert_eq!(failure.code, 0);
    assert!(failure.stdout);
    let failure = refused(&["endpoint"]);
    assert!(
        failure.text.contains("Usage: wisp account"),
        "{}",
        failure.text
    );
    assert!(failure.text.contains("add"));
}

#[test]
fn the_command_path_skips_flag_values() {
    let failure = refused(&["--model", "endpoint", "--effort", CANARY]);
    assert_usage(&failure);
    assert!(
        failure.text.starts_with("wisp: --effort"),
        "{}",
        failure.text
    );
    let failure = refused(&[
        "--config",
        "endpoint",
        "endpoint",
        "--env-file",
        "add",
        "add",
        "--auth",
        CANARY,
    ]);
    assert_usage(&failure);
    assert!(
        failure.text.starts_with("wisp account add: --auth"),
        "{}",
        failure.text
    );
}

#[test]
fn a_repeated_key_flag_is_refused_as_a_key_and_other_repeats_say_so() {
    for args in [
        &["endpoint", "add", "a", "--key", CANARY, "--key", CANARY][..],
        &["endpoint", "add", "a", "--token"],
        &["endpoint", "add", "--key", CANARY, "--bogus"],
        &["endpoint", "add", "x", "--key", CANARY, "extra"],
        &["endpoint", "add", "x", "--api-key=sk-ant-CANARY", "extra"],
    ] {
        let failure = refused(args);
        assert_usage(&failure);
        assert_eq!(
            failure.text,
            format!("wisp: {}", crate::manage::KEY_ON_COMMAND_LINE),
            "{args:?}"
        );
    }
    let failure = refused(&[
        "endpoint", "add", "a", "--url", "http://x", "--url", "http://y",
    ]);
    assert_usage(&failure);
    assert_eq!(
        failure.text,
        "wisp account add: --url was given more than once"
    );
}
