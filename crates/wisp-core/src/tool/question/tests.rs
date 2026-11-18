use std::path::PathBuf;

use wisp_protocol::{Answer, Question, QuestionEnd, QuestionOption};

use super::*;

fn opt(label: &str, description: &str) -> QuestionOption {
    QuestionOption {
        label: label.into(),
        description: description.into(),
    }
}

fn plan_questions() -> Vec<Question> {
    vec![
        Question {
            header: "Database".into(),
            question: "Which database should the service use?".into(),
            options: vec![
                opt(
                    "PostgreSQL (Recommended)",
                    "Relational; the billing service already runs it.",
                ),
                opt("SQLite", "One file, no server; fine for a single writer."),
                opt("DynamoDB", "Managed, but ties the service to AWS."),
            ],
            multi_select: false,
            source: None,
        },
        Question {
            header: "Features".into(),
            question: "Which features should ship first?".into(),
            options: vec![
                opt("Auth", "Email and passkeys."),
                opt("Search", "Full-text over notes."),
                opt("Export", "CSV and JSON."),
            ],
            multi_select: true,
            source: None,
        },
        Question {
            header: "Deploy".into(),
            question: "Where will it run?".into(),
            options: vec![opt("Fly.io", ""), opt("A VPS", "")],
            multi_select: false,
            source: None,
        },
        Question {
            header: "Name".into(),
            question: "What should the crate be called?".into(),
            options: vec![opt("wisp-store", ""), opt("wisp-db", "")],
            multi_select: false,
            source: None,
        },
    ]
}

fn plan_answers() -> Vec<Answer> {
    vec![
        Answer::choose(0),
        Answer {
            chosen: vec![0, 1],
            own: None,
        },
        Answer::own("Fly.io, but keep a Dockerfile in the repo"),
        Answer::skipped(),
    ]
}

#[test]
fn the_plan_example_reads_exactly_as_the_plan_shows_it() {
    let expected = "The user answered:

1. [Database] Which database should the service use?
   PostgreSQL (Recommended)

2. [Features] Which features should ship first?
   - Auth
   - Search

3. [Deploy] Where will it run?
   In their own words: Fly.io, but keep a Dockerfile in the repo

4. [Name] What should the crate be called?
   Skipped. Decide yourself and say what you assumed.

Read the answers in the user's own words carefully: they may ask for something else, or ask you not to go on. Follow what they say.";
    assert_eq!(answered_text(&plan_questions(), &plan_answers()), expected);
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ask_user")
}

fn cases() -> Vec<(&'static str, String)> {
    let questions = plan_questions();
    let single = &questions[..1];
    let multi = &questions[1..2];
    let how = |answers: Vec<Answer>| QuestionEnd::Answered { answers };
    vec![
        ("plan", result_text(&questions, &how(plan_answers()))),
        ("single", result_text(single, &how(vec![Answer::choose(1)]))),
        (
            "multi",
            result_text(
                multi,
                &how(vec![Answer {
                    chosen: vec![0, 2],
                    own: None,
                }]),
            ),
        ),
        (
            "multi_one",
            result_text(multi, &how(vec![Answer::choose(1)])),
        ),
        (
            "multi_with_own",
            result_text(
                multi,
                &how(vec![Answer {
                    chosen: vec![0, 2],
                    own: Some("Webhooks for new notes".into()),
                }]),
            ),
        ),
        (
            "own",
            result_text(single, &how(vec![Answer::own("MySQL, it's what ops know")])),
        ),
        (
            "own_multi_line",
            result_text(
                single,
                &how(vec![Answer::own(
                    "None of these.\n\nUse the existing store,\n  and add a cache.",
                )]),
            ),
        ),
        (
            "skipped",
            result_text(single, &how(vec![Answer::skipped()])),
        ),
        ("stopped", result_text(single, &QuestionEnd::Stopped)),
        (
            "message_instead",
            result_text(single, &QuestionEnd::MessageInstead),
        ),
        (
            "interrupted",
            result_text(single, &QuestionEnd::Interrupted),
        ),
        ("unavailable", UNAVAILABLE.to_owned()),
    ]
}

#[test]
fn every_result_text_matches_its_fixture_byte_for_byte() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    for (name, text) in cases() {
        let path = fixture_dir().join(format!("{name}.txt"));
        let bytes = format!("{text}\n");
        if bless {
            std::fs::create_dir_all(fixture_dir()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bytes,
            "{}",
            path.display()
        );
    }
}

#[test]
fn result_texts_hold_no_time_and_do_not_depend_on_when_they_are_made() {
    let first = cases();
    assert_eq!(first, cases());
    for (name, text) in first {
        for line in text.lines() {
            let body = line
                .split_once(". [")
                .filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
                .map_or(line, |(_, rest)| rest);
            assert!(!body.chars().any(|c| c.is_ascii_digit()), "{name}: {line}");
        }
    }
}

#[test]
fn a_multi_line_own_answer_keeps_every_line_indented_and_every_byte() {
    let own = "line one\r\n\nline three\n";
    let text = answered_text(&plan_questions()[..1], &[Answer::own(own)]);
    assert!(
        text.contains("   In their own words: line one\r\n\n   line three\n\n\nRead the answers")
    );
}

#[test]
fn a_multi_line_label_is_indented_the_same_way_for_single_and_multi_select() {
    let question = |multi_select: bool| Question {
        header: "H".into(),
        question: "Q?".into(),
        options: vec![opt("A\nB", ""), opt("C", "")],
        multi_select,
        source: None,
    };
    assert_eq!(
        answered_text(&[question(false)], &[Answer::choose(0)]),
        "The user answered:\n\n1. [H] Q?\n   A\n   B"
    );
    assert_eq!(
        answered_text(
            &[question(true)],
            &[Answer {
                chosen: vec![0, 1],
                own: None,
            }]
        ),
        "The user answered:\n\n1. [H] Q?\n   - A\n   B\n   - C"
    );
}

#[test]
fn the_caution_is_added_only_when_an_answer_is_in_the_users_own_words() {
    let questions = &plan_questions()[..1];
    assert!(!answered_text(questions, &[Answer::choose(0)]).contains(OWN_WORDS_CAUTION));
    assert!(!answered_text(questions, &[Answer::skipped()]).contains(OWN_WORDS_CAUTION));
    assert!(answered_text(questions, &[Answer::own("x")]).ends_with(OWN_WORDS_CAUTION));
}
