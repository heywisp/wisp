use base64::Engine;

use super::*;
use crate::session::Redactions;

#[test]
fn extra_key_prefixes_require_twenty_tail_characters_and_a_token_boundary() {
    let held = Redactions::default();
    let tail = "CANARY_1234567890-ab";
    assert_eq!(tail.len(), 20);
    for prefix in EXTRA_KEY_PREFIXES {
        let key = format!("{prefix}{tail}");
        for detected in [key.clone(), format!(r"Line\n{key}"), format!("({key})")] {
            assert!(contains_secret_or_identity_with(&detected, None, &held));
        }
        for safe in [format!("{prefix}{}", &tail[..19]), format!("x{key}")] {
            assert!(!contains_secret_or_identity_with(&safe, None, &held));
        }
    }
}

#[test]
fn checklist_scrubbing_is_idempotent_when_a_log_only_match_overlaps_the_marker() {
    let held = Redactions::log_only(["redact".to_owned()]);
    let safe = scrub_field("Use the redact command", None, &held);
    assert_eq!(safe, REDACTED);
    assert_eq!(scrub_field(&safe, None, &held), safe);
}

#[test]
fn scrubbing_a_todo_refuses_a_key_exposed_by_a_log_only_match() {
    let log_only = Redactions::log_only(["CANARY-prefix".into()]);
    let key = "sk-ant-api03-CANARY-todo-token-boundary-fixture";
    let text = format!("Record CANARY-prefix{key}");
    assert!(!contains_secret_or_identity_with(&text, None, &log_only));
    assert_eq!(log_only.scrub(&text), format!("Record {REDACTED}{key}"));
    assert_eq!(scrub_field(&text, None, &log_only), REDACTED);
}

#[test]
fn held_credentials_refuse_short_long_escaped_base64_and_project_id_but_allow_log_only_paths() {
    let long = "CANARYheldlongcredentialmaterial";
    let project_id = "CANARYprojectid12345";
    let short = "CANARYpin";
    let log = "/v1/CANARY-endpoint-path";
    session::register_secrets([
        long.to_owned(),
        short.to_owned(),
        "tag:CANARYpin".to_owned(),
        "test".to_owned(),
        "ollama".to_owned(),
        "ab".to_owned(),
    ]);
    let project = crate::oauth::project_redactions(Some(project_id));
    session::register(&project);
    let log_only = Redactions::log_only([log.to_owned()]);
    session::register(&log_only);
    assert_eq!(project_id.len(), wisp_protocol::KEY_SHAPED_MIN);
    assert_eq!(project.mask(project_id), project_id);
    for prefix in ["", ":", "u:"] {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{prefix}{project_id}:suffix"));
        let embedded = format!("pre{encoded}post");
        assert!(
            project.has_secret_check_match(&embedded),
            "alignment {prefix:?}"
        );
        assert!(
            contains_secret_or_identity(&embedded, None),
            "alignment {prefix:?}"
        );
        assert_eq!(project.mask(&embedded), embedded);
    }
    let wrapped_long = format!("pre{long}post");
    let wrapped_project = format!("pre{project_id}post");
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("u:{long}:suffix"));
    for text in [
        short,
        r"key\u003dCANARYpin",
        r"key\\u003dCANARYpin",
        r"key\u0020CANARYpin",
        r#"key\"CANARYpin\""#,
        "notag:CANARYpin",
        &wrapped_long,
        &wrapped_project,
        &encoded,
        r"CANARY\u0068eldlongcredentialmaterial",
        "test",
        "KEY=test",
        r"a\ntest",
        r"t\u0065st",
    ] {
        assert!(contains_secret_or_identity(text, None), "{text}");
    }
    for (value, redactions) in [
        (short, Redactions::new([short.to_owned()])),
        (log, log_only),
    ] {
        let encoded = base64::engine::general_purpose::STANDARD.encode(value);
        assert!(!contains_secret_or_identity(&encoded, None));
        assert_eq!(redactions.scrub(&encoded), encoded);
    }
    for text in [
        log,
        "prefixCANARYpin",
        "CANARYpin_old",
        "latest",
        "tests",
        "_ollama_",
        "ab",
    ] {
        assert!(!contains_secret_or_identity(text, None), "{text}");
    }
}

#[test]
fn every_protocol_key_prefix_and_url_password_is_refused_without_blocking_normal_text() {
    for prefix in wisp_protocol::KEY_PREFIXES.iter() {
        let token = format!("{prefix}CANARYabcdefghijklmnopqrst");
        for text in [
            format!("key={token}"),
            format!(r"log\n{token}"),
            format!(r"log\r{token}"),
            format!(r"log\t{token}"),
        ] {
            assert!(contains_secret_or_identity(&text, None), "{text}");
        }
        assert!(!contains_secret_or_identity(&format!("x{token}"), None));
    }
    for (prefix, password, expected_password) in [
        ("http://", "CANARY[part]", true),
        ("http://", "CANARY{part}", true),
        ("http://", "CANARY<part>", true),
        ("http://", "CANARY\"part\"", true),
        ("http://", "CANARY`part`", true),
        ("http://", "CANARY,part", true),
        ("http://", "CANARY;part", true),
        ("http://", "CANARY(1)", true),
        ("http://", "CANARY'part", true),
        ("postgres://", r"CANARY\x", true),
        ("postgres://", r"CANARY\n", true),
        ("postgres://", r"CANARY\r", true),
        ("postgres://", r"CANARY\t", true),
        ("https:///", "CANARYpass", true),
        (r"https://\", "CANARYpass", true),
        (r"https:///\", "CANARYpass", true),
        ("file:///", "CANARYpass", false),
    ] {
        let url = format!("{prefix}u:{password}@localhost/db");
        let parsed = reqwest::Url::parse(&url).unwrap();
        assert_eq!(
            parsed.password().is_some_and(|p| !p.is_empty()),
            expected_password,
            "{url}"
        );
        assert_eq!(
            contains_secret_or_identity(&url, None),
            expected_password,
            "{url}"
        );
        assert_eq!(
            wisp_protocol::has_url_password(&url),
            expected_password,
            "{url}"
        );
        assert_eq!(
            wisp_protocol::command_secret(&url, None),
            expected_password.then_some(password)
        );
    }
    assert!(contains_secret_or_identity(
        "read [a](https://example.invalid:443)[b](http://u:p@[::1]:8080)",
        None
    ));
    for text in [
        "https://example.invalid",
        "https://example.invalid:443 @scope/pkg",
        r"https://example.invalid:443\n@scope/pkg",
        "postgres://example.invalid:5432\n@scope/pkg",
        "https://example.invalid:443,https://other.invalid",
    ] {
        assert!(!contains_secret_or_identity(text, None), "{text}");
    }
}

pub(crate) fn email_regression_cases() -> impl Iterator<Item = (String, String)> {
    let url = "https://example.invalid/u";
    let first = ["CANARYalpha", "example.invalid"].join("%40");
    let second = ["CANARYbeta", "other.invalid"].join("%40");
    let raw = ["CANARYbeta", "other.invalid"].join("@");
    let version = ["pkg", "1.2.3"].join("@");
    let handle = ["", "CANARY.handle"].join("@");
    [
        (format!("{url}/{first}"), format!("{url}/{REDACTED}")),
        (
            format!("{url}/{first}/{version}"),
            format!("{url}/{REDACTED}/{version}"),
        ),
        (
            format!("{url}/{first}/{handle}"),
            format!("{url}/{REDACTED}/{handle}"),
        ),
        (
            format!("{url}/{first}/sub/{second}"),
            format!("{url}/{REDACTED}/sub/{REDACTED}"),
        ),
        (
            format!("{url}/{first}/sub/{raw}"),
            format!("{url}/{REDACTED}/sub/{REDACTED}"),
        ),
        (
            format!("{url}/{version}/{first}"),
            format!("{url}/{version}/{REDACTED}"),
        ),
    ]
    .into_iter()
    .chain(
        [
            ["CANARY%40", "example.invalid"].join("@"),
            ["CANARY%40%40", "example.invalid"].join("@"),
            ["CANARY%40tag", "example.invalid"].join("@"),
            [first.as_str(), "other.invalid"].join("@"),
            ["CANARYalpha", "example.invalid", "other.invalid"].join("@"),
            ["CANARY", "example.invalid.bak"].join("@"),
            [
                "CANARYalpha",
                &["example.invalid", "other.invalid"].join("%40"),
            ]
            .join("@"),
        ]
        .into_iter()
        .map(|text| (text, REDACTED.to_owned())),
    )
    .chain(
        [
            (["CANARY", "example.invalid"].join("@"), "."),
            (["CANARY", "example.invalid"].join("%40"), "."),
            (["CANARY", "example%2einvalid"].join("%40"), "%2e"),
            (["CANARY", "example%2Einvalid"].join("%40"), "%2E"),
            (["CANARY", "sub.1.example.invalid"].join("@"), "."),
        ]
        .into_iter()
        .flat_map(|(mailbox, sep)| {
            ["1", "mp4"].map(|ext| {
                (
                    format!("{mailbox}{sep}{ext}"),
                    format!("{REDACTED}{sep}{ext}"),
                )
            })
        }),
    )
    .chain(
        [
            ["CANARY", "example.1"].join("@"),
            ["CANARY", "a.b.1"].join("@"),
        ]
        .into_iter()
        .map(|text| (text.clone(), text)),
    )
}

#[test]
fn email_and_account_id_checks_cover_unicode_wrappers_and_allow_versions_and_handles() {
    let plain = ["CANARY", "example.invalid"].join("@");
    for text in [
        ["CANARY+tag", "müller.invalid"].join("@"),
        ["CANARY", "mu\u{0308}ller.invalid"].join("@"),
        ["CANARY", "cafe\u{0301}.invalid"].join("@"),
        ["CANARYcafe\u{0301}", "example.invalid"].join("@"),
        format!("_{}_,", plain),
        format!("**{plain}**"),
        format!("‘{plain}’"),
        format!("`{plain}`"),
        format!("<mailto:{plain}>"),
        plain.replace('@', "%40").replace('.', "%2E"),
    ] {
        assert!(has_email(&text), "{text}");
        assert!(contains_secret_or_identity(&text, None), "{text}");
    }
    for (text, scrubbed) in email_regression_cases() {
        assert_eq!(has_email(&text), text != scrubbed, "{text}");
        assert_eq!(
            contains_secret_or_identity(&text, None),
            text != scrubbed,
            "{text}"
        );
        assert_eq!(redact_emails(&text), scrubbed);
    }
    for label in ["café", "cafe\u{0301}", "भारत", "XN--AA"] {
        assert!(valid_tld_label(label), "{label}");
    }
    for label in ["a", "a\u{0301}", "क\u{093f}", "XN--A", "ab1", "ab-c"] {
        assert!(!valid_tld_label(label), "{label}");
    }
    for prefix in ID_PREFIXES {
        let id = format!("{prefix}CANARY0123456789abcd");
        assert!(contains_secret_or_identity(&id, None));
        assert!(!contains_secret_or_identity(&format!("{id}_suffix"), None));
    }
    for text in [
        ["wisp:plugin", "0.4.0"].join("@"),
        ["CANARY", "127.0.0.1"].join("@"),
        ["lodash", "4.17.21"].join("@"),
        ["CANARY", "1.2.3-beta.1"].join("@"),
        ["CANARY", "1.2.3-beta"].join("@"),
        ["https://social.invalid/", "CANARY.handle"].join("@"),
        ["https://cdn.example.invalid/", "pkg.js"].join("@"),
        "@scope/pkg user-provided org-wide proj_name".to_owned(),
        "00000000-0000-0000-0000-000000000000".to_owned(),
    ] {
        assert!(!contains_secret_or_identity(&text, None), "{text}");
    }
}

#[test]
fn attached_path_flags_require_a_delimiter_and_find_home_paths_across_platforms() {
    let home = Path::new("/srv/CANARY-checklist-home");
    let held = Redactions::default();
    let detected = |text| contains_secret_or_identity_with(text, Some(home), &held);
    for text in [
        "-I/home/CANARY/inc",
        "compile -F/Users/CANARY/framework",
        "-iframework/Users/CANARY/Frameworks",
        "--sysroot/home/CANARY/sdk",
        r"-LC:\Users\CANARY\lib",
        "-C~CANARY-project/src",
        "-I/srv/CANARY-checklist-home/inc",
        "(-isystem/srv/CANARY-checklist-home/inc)",
        r"-isysrootD:\Users\CANARY\sdk",
        r"line\n-I/Users/CANARY/inc",
        r"line\t-isystem/srv/CANARY-checklist-home/inc",
        r"line\r--sysroot/srv/CANARY-checklist-home/sdk",
        r"line\n-iframework/Users/CANARY/Frameworks",
    ] {
        assert!(detected(text), "{text}");
    }
    for text in [
        "https://example.invalid/home/CANARY",
        "https://a-b/home/CANARY",
        "https://example.invalid/-x/home/CANARY",
        "https://example.invalid/-isystem/home/CANARY",
        "https://example.invalid/-iframework/Users/CANARY/Frameworks",
        "foo-I/home/CANARY",
        "foo-iframework/Users/CANARY/Frameworks",
        "foo-isystem/srv/CANARY-checklist-home/inc",
        "--/home/CANARY",
        "-1/home/CANARY",
        r"line\nfoo-I/Users/CANARY/inc",
        r"line\n--/home/CANARY",
        r"line\n-1/Users/CANARY/inc",
        r"line\nhttps://example.invalid/-iframework/Users/CANARY/Frameworks",
    ] {
        assert!(!detected(text), "{text}");
    }
}

#[test]
fn home_paths_are_refused_across_unix_macos_windows_wsl_unc_tilde_and_runtime_homes() {
    for text in [
        "/home/CANARY/work",
        "see '/Users/Canary Fixture'",
        "see “/home/CANARY/path”",
        "see ‘/Users/CANARY’",
        "/mnt/c/uSeRs/CANARY/work",
        r"C:\uSeRs\CANARY\work",
        r"C:\\Users\\CANARY\\work",
        "d:/Users/CANARY/work",
        r"\\WSL.LocalHost\Fixture-1\home\CANARY\work",
        r"\\wsl$\Fixture_1\home\CANARY\work",
        r"\\\\wsl.localhost\\Fixture.1\\home\\CANARY\\work",
        r"\\\\wsl$\\Fixture.1\\home\\CANARY\\work",
        "~CANARY-user/work",
        "~_CANARY/work",
        "use @/home/CANARY/path",
        "@/Users/CANARY/path",
        "file:///home/CANARY/path",
        "file:////home/CANARY/path",
        r"output\n/home/CANARY/path",
        r"output\r/Users/CANARY/path",
        r"output\tD:\Users\CANARY\work",
    ] {
        assert!(contains_secret_or_identity(text, None), "{text}");
    }
    for (home, text) in [
        ("/CANARY-profile", "/CANARY-profile/sub"),
        ("/CANARY-profile/", "My home is /CANARY-profile."),
        ("/u/CANARY", "My home is /u/CANARY."),
        ("/u/CANARY", "See /u/CANARY..."),
        ("/u/CANARY", r"\\wsl.localhost\Fixture\u\CANARY\work"),
        ("/u/CANARY", r"\\\\wsl$\\Fixture\\u\\CANARY\\work"),
        ("/u//CANARY", r"\\wsl$\Fixture\u\CANARY\work"),
        ("/CANARY-profile", r"\\wsl$\Fixture\CANARY-profile."),
        (r"C:\Profiles\CANARY", r"c:\profiles\canary\work"),
        (r"C:\Profiles\CANARY", "c:/profiles/canary/work"),
        (r"C:\Profiles\CANARY", r"c:\\profiles\\canary\\work"),
        (
            r"\\fixture\profiles\CANARY",
            r"\\fixture\profiles\CANARY\work",
        ),
        (
            r"\\Fixture\Profiles\CANARY",
            r"\\fixture\profiles\canary\work",
        ),
        (
            r"\\fixture\profiles\CANARY",
            r"\\\\fixture\\profiles\\CANARY\\work",
        ),
    ] {
        assert!(
            contains_secret_or_identity(text, Some(Path::new(home))),
            "{text}"
        );
    }
    let profile = OsString::from(r"C:\Profiles\CANARY");
    let windows = Some(PathBuf::from(&profile));
    assert_eq!(
        fallback_detection_home(None, Some(profile.clone())),
        windows
    );
    assert_eq!(
        fallback_detection_home(Some("/".into()), Some(profile.clone())),
        windows
    );
    assert_eq!(
        fallback_detection_home(Some(r"D:\".into()), Some(profile.clone())),
        windows
    );
    assert_eq!(
        fallback_detection_home(Some("/CANARY-profile".into()), Some(profile)),
        Some("/CANARY-profile".into())
    );
    assert_eq!(
        fallback_detection_home(
            Some("relative/path".into()),
            Some("relative/profile".into())
        ),
        None
    );
}

#[test]
fn portable_paths_prose_dates_model_ids_and_placeholders_stay_safe() {
    for text in [
        "~ ~/ ~/Projects/Wisp ~10 minutes ~mask ~~strike~~",
        "GET /users/me /home/ /home/. /Users/ /home/<user> /home/$USER /Users/${USER}",
        r"C:\Users\%USERNAME% C:\Users\<user> C:\Users\",
        r"\\wsl.localhost\\home\CANARY \\wsl$\Fixture\Home\CANARY",
        r"\\wsl$\Fixture\home\<user> \\wsl$\Fixture\home\.",
        "https://example.invalid/home/CANARY file://example.invalid/home/CANARY",
        "http://example.invalid/Users/CANARY /mnt/cc/Users/CANARY",
        "git status; notes/2026-04-02.md, ghcr.io/example/image:latest",
        "claude-sonnet-4-5-20250929 ServiceUnavailableException",
        "1234567890abcdef1234567890abcdef12345678",
        "mail is available after 4.12.0, filename.ts and ERROR_CODE here",
        "prefix@/home/CANARY /work/Users/CANARY",
    ] {
        assert!(!contains_secret_or_identity(text, None), "{text}");
    }
    for text in [
        "/u/CANARY.backup",
        "/u/CANARY2",
        "/u/CANARY_old",
        r"\\wsl$\Fixture\u\CANARY.backup",
    ] {
        assert!(
            !contains_secret_or_identity(text, Some(Path::new("/u/CANARY"))),
            "{text}"
        );
    }
    for (root, text) in [
        ("/", "/usr/share"),
        (r"C:\", r"C:\ProgramData"),
        ("D:/", "D:/ProgramData"),
    ] {
        assert!(
            !contains_secret_or_identity(text, Some(Path::new(root))),
            "{text}"
        );
    }
}

#[test]
fn multibyte_boundaries_and_large_adversarial_inputs_never_panic_or_stall() {
    let email = ["CANARY", "mu\u{0308}ller.invalid"].join("@");
    for text in [
        format!("界🙂‘{email}’界"),
        "界🙂“/home/CANARY/🍂".to_owned(),
        "界🙂(D:/Users/CANARY/🍂)".to_owned(),
        "界🙂([a](https://example.invalid:443)[b](http://u:p@localhost))".to_owned(),
    ] {
        assert!(contains_secret_or_identity(&text, None), "{text}");
    }
    for text in [
        "🙂:/h",
        "🙂:C:/Usérs/",
        "字(é:",
        "界“/u/CANARYé",
        "🍂à://",
        "🙂:\\wsl$",
    ] {
        assert!(
            !contains_secret_or_identity(text, Some(Path::new("/u/CANARY"))),
            "{text}"
        );
    }
    let mut large = "[a](https://example.invalid:443)[b](custom://other.invalid), @scope/pkg \\n /home/<user> 界 ".repeat(12_000);
    assert!(large.len() >= 1_000_000);
    assert!(!contains_secret_or_identity(
        &large,
        Some(Path::new("/CANARY-profile"))
    ));
    large.push_str(&email);
    assert!(contains_secret_or_identity(&large, None));

    let mixed = "@%40".repeat(125_000);
    let percents = "%40".repeat(175_000);
    let prefix = format!("{mixed}\nCANARY{percents}");
    assert!(!contains_secret_or_identity(&prefix, None));
    let mailbox = [prefix.as_str(), "example.invalid"].join("@");
    assert!(contains_secret_or_identity(&mailbox, None));
    assert!(redact_emails(&mailbox) == format!("{mixed}\n{REDACTED}"));
}

#[test]
fn scrub_checklist_spans_preserves_prose_handles_multiline_empty_and_fails_closed_on_corrupt_spans()
{
    let prose = "café\nChecklist:\n- [ ] sk-CANARYfakekeyintaskprose000\n";
    let first = "café: use log-only-CANARY here";
    let second = "second\n- [ ] credential-CANARY";
    let mut text = format!("{prose}- [ ] {first}\n- [ ] \n- [ ] {second}");
    let first_start = text.find(first).unwrap();
    let second_start = text.find(second).unwrap();
    let mut spans = vec![
        TextSpan {
            start: first_start,
            end: first_start + first.len(),
        },
        TextSpan {
            start: second_start - "\n- [ ] ".len(),
            end: second_start - "\n- [ ] ".len(),
        },
        TextSpan {
            start: second_start,
            end: text.len(),
        },
    ];
    assert!(valid_checklist_spans(&text, &spans));
    assert!(scrub_checklist_spans(
        &mut text,
        &mut spans,
        None,
        &Redactions::log_only(["log-only-CANARY".to_owned()])
    ));
    assert!(text.starts_with(prose));
    assert_eq!(
        &text[spans[0].start..spans[0].end],
        "café: use [redacted] here"
    );
    assert_eq!(&text[spans[1].start..spans[1].end], "");
    assert_eq!(&text[spans[2].start..spans[2].end], second);
    let credentials = Redactions::new(["credential-CANARY".to_owned()]);
    assert!(scrub_checklist_spans(
        &mut text,
        &mut spans,
        None,
        &credentials
    ));
    assert_eq!(&text[spans[2].start..spans[2].end], REDACTED);
    assert!(!scrub_checklist_spans(
        &mut text,
        &mut spans,
        None,
        &credentials
    ));

    for mut corrupt in [
        vec![TextSpan {
            start: 0,
            end: usize::MAX,
        }],
        vec![TextSpan { start: 4, end: 3 }],
        vec![TextSpan { start: 0, end: 5 }, TextSpan { start: 2, end: 8 }],
        vec![TextSpan { start: 1, end: 2 }],
    ] {
        let mut raw = "é opaque-CANARY".to_owned();
        assert!(scrub_checklist_spans(
            &mut raw,
            &mut corrupt,
            None,
            &credentials
        ));
        assert_eq!(raw, REDACTED);
        assert_eq!(
            corrupt,
            [TextSpan {
                start: 0,
                end: REDACTED.len()
            }]
        );
        assert!(valid_checklist_spans(&raw, &corrupt));
        assert!(!scrub_checklist_spans(
            &mut raw,
            &mut corrupt,
            None,
            &credentials
        ));
    }
}
