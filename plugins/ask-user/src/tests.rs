use std::sync::Arc;

use futures::executor::block_on;
use wisp_plugin_api::types::Completed;
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use super::*;

fn option(label: &str, description: &str) -> Value {
    json!({"label": label, "description": description})
}

fn two_options() -> Value {
    json!([option("Yes", "Go on."), option("No", "Stop here.")])
}

fn one(header: &str, question: &str) -> Value {
    json!({"header": header, "question": question, "options": two_options()})
}

fn plugin(host: FakeHost) -> AskUser {
    AskUser::new(Arc::new(host))
}

fn bare() -> AskUser {
    plugin(FakeHost::default())
}

fn refusal(input: &Value) -> Option<String> {
    block_on(bare().prepare(NAME, &input.to_string())).err()
}

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
        },
        Question {
            header: "Deploy".into(),
            question: "Where will it run?".into(),
            options: vec![opt("Fly.io", ""), opt("A VPS", "")],
            multi_select: false,
        },
        Question {
            header: "Name".into(),
            question: "What should the crate be called?".into(),
            options: vec![opt("wisp-store", ""), opt("wisp-db", "")],
            multi_select: false,
        },
    ]
}

#[test]
fn the_metadata_says_read_only_serial_unbounded_and_ends_the_reply() {
    let meta = meta();
    assert_eq!(meta.effect, Effect::ReadOnly);
    assert_eq!(meta.tags, Tags::default());
    assert!(!meta.parallel_safe);
    assert_eq!(meta.timeouts.max_ms, None);
    assert_eq!(block_on(bare().time_limit_ms(NAME, "{}")), None);
    assert_eq!(meta.writing.as_deref(), Some("a question"));
    assert_eq!(meta.purpose, Purpose::Run);
    assert!(!meta.saves_output);
    assert!(!meta.needs_workspace);
    assert!(meta.ends_reply);
}

#[test]
fn prepare_accepts_the_plan_example_and_keeps_its_text_verbatim() {
    let input = json!({"questions": [
        {"header": "Database", "question": "Which database should the service use?", "options": [
            option("PostgreSQL (Recommended)", "Relational; the billing service already runs it."),
            option("SQLite", "One file, no server; fine for a single writer."),
            option("DynamoDB", "Managed, but ties the service to AWS."),
        ]},
        {"header": "Features", "question": "Which features should ship first?", "multi_select": true, "options": [
            option("Auth", "Email and passkeys."),
            option("Search", "Full-text over notes."),
            option("Export", "CSV and JSON."),
        ]},
        {"header": "Deploy", "question": "Where will it run?", "multi_select": false, "options": [
            option("Fly.io", ""), option("A VPS", ""),
        ]},
        {"header": "Name", "question": "What should the crate be called?", "options": [
            option("wisp-store", ""), option("wisp-db", ""),
        ]},
    ]});
    let prepared = block_on(bare().prepare(NAME, &input.to_string())).unwrap();
    assert_eq!(
        prepared.display(),
        Display {
            action: Action::Other("ask_user".into()),
            verbs: None,
            target: "Database (+3)".into(),
            output: OutputKind::None,
            view: None,
            title: None,
        }
    );
    assert_eq!(
        prepared.into_any().downcast::<Call>().ok().map(|c| *c),
        Some(Call::Ask(plan_questions()))
    );
}

#[test]
fn prepare_accepts_the_withdraw_form_with_its_own_display() {
    let prepared = block_on(bare().prepare(NAME, &json!({"withdraw": true}).to_string())).unwrap();
    assert_eq!(
        prepared.display(),
        Display {
            action: Action::Other("ask_user".into()),
            verbs: Some(Verbs {
                doing: "Withdrawing".into(),
                done: "Withdrew".into(),
                failed: "couldn't withdraw".into(),
            }),
            target: "the question".into(),
            output: OutputKind::None,
            view: None,
            title: None,
        }
    );
    assert_eq!(
        prepared.into_any().downcast::<Call>().ok().map(|c| *c),
        Some(Call::Withdraw)
    );
}

#[test]
fn prepare_refuses_every_rule_of_the_input_table() {
    let cases: Vec<(Value, String)> = vec![
        (json!("hi"), "input must be an object".into()),
        (json!({}), "`questions` must be an array".into()),
        (
            json!({"questions": [one("H", "Q?")], "withdraw": true}),
            "give either questions or withdraw, not both".into(),
        ),
        (
            json!({"withdraw": false}),
            "`withdraw` must be true; to ask, give `questions` instead".into(),
        ),
        (
            json!({"withdraw": "yes"}),
            "`withdraw` must be true; to ask, give `questions` instead".into(),
        ),
        (
            json!({"questions": one("H", "Q?")}),
            "`questions` must be an array".into(),
        ),
        (
            json!({"questions": [one("H", "Q?")], "why": "x"}),
            "unknown field `why`".into(),
        ),
        (
            json!({"questions": []}),
            "ask one to four questions in one call, not 0".into(),
        ),
        (
            json!({"questions": [one("A", "1?"), one("B", "2?"), one("C", "3?"), one("D", "4?"), one("E", "5?")]}),
            "ask one to four questions in one call, not 5".into(),
        ),
        (
            json!({"questions": ["Q?"]}),
            "question 1 must be an object".into(),
        ),
        (
            json!({"questions": [one("H", "Q?"), {"header": "H", "question": "R?", "options": two_options(), "preview": "x"}]}),
            "unknown field `preview` in question 2".into(),
        ),
        (
            json!({"questions": [{"question": "Q?", "options": two_options()}]}),
            "question 1's `header` must be a string".into(),
        ),
        (
            json!({"questions": [one(" ", "Q?")]}),
            "question 1's `header` is blank".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": 3, "options": two_options()}]}),
            "question 1's `question` must be a string".into(),
        ),
        (
            json!({"questions": [one("H", "")]}),
            "question 1's `question` is blank".into(),
        ),
        (
            json!({"questions": [one("A", "Same?"), one("B", "Same?")]}),
            "questions 1 and 2 are the same; each question must differ".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": two_options(), "multi_select": "yes"}]}),
            "question 1's `multi_select` must be true or false".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?"}]}),
            "question 1's `options` must be an array".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": []}]}),
            TOO_FEW_OPTIONS.into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [option("Only", "")]}]}),
            TOO_FEW_OPTIONS.into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [
                option("A", ""), option("B", ""), option("C", ""), option("D", ""), option("E", "")
            ]}]}),
            "question 1 has 5 options; give two to four".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [option("A", ""), "B"]}]}),
            "option 2 of question 1 must be an object".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [option("A", ""), {"label": "B", "description": "", "preview": "x"}]}]}),
            "unknown field `preview` in option 2 of question 1".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [option("A", ""), option("", "x")]}]}),
            "option 2 of question 1's `label` is blank".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [option("A", ""), {"label": "B"}]}]}),
            "option 2 of question 1's `description` must be a string".into(),
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [option("A", ""), option("B", ""), option("A", "again")]}]}),
            "options 1 and 3 of question 1 have the same label; labels must differ".into(),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(refusal(&input), Some(expected), "{input}");
    }
}

#[test]
fn optional_and_lenient_fields_are_accepted() {
    let empty_description = json!({"questions": [{"header": "H", "question": "Q?", "options": [option("A", ""), option("B", "")]}]});
    assert_eq!(refusal(&empty_description), None);
    let same_label_elsewhere = json!({"questions": [
        {"header": "H", "question": "Q?", "options": [option("Yes", ""), option("No", "")]},
        {"header": "H", "question": "R?", "options": [option("Yes", ""), option("No", "")]},
    ]});
    assert_eq!(refusal(&same_label_elsewhere), None);
    let four =
        json!({"questions": [one("A", "1?"), one("B", "2?"), one("C", "3?"), one("D", "4?")]});
    assert_eq!(refusal(&four), None);
}

#[test]
fn a_long_header_is_shown_cut_but_kept_whole_in_the_question() {
    assert_eq!(cut_header("Database"), "Database");
    assert_eq!(cut_header("Twelve chars"), "Twelve chars");
    assert_eq!(cut_header("Thirteen char"), "Thirteen ch…");
    assert_eq!(cut_header("Ünïcödé héädér"), "Ünïcödé héä…");
    assert_eq!(cut_header("Thirteen char").chars().count(), HEADER_CHARS);
    let input = json!({"questions": [one("Deployment target", "Q?"), one("B", "R?")]});
    let prepared = block_on(bare().prepare(NAME, &input.to_string())).unwrap();
    assert_eq!(prepared.display().target, "Deployment … (+1)");
    let Call::Ask(questions) = *prepared.into_any().downcast::<Call>().unwrap() else {
        panic!("expected ask");
    };
    assert_eq!(questions[0].header, "Deployment target");
    let single = block_on(bare().prepare(
        NAME,
        &json!({"questions": [one("Database", "Q?")]}).to_string(),
    ))
    .unwrap();
    assert_eq!(single.display().target, "Database");
}

fn prepared_ask() -> Box<dyn Prepared> {
    block_on(bare().prepare(
        NAME,
        &json!({"questions": [one("Go", "Go on?")]}).to_string(),
    ))
    .unwrap()
}

fn prepared_withdraw() -> Box<dyn Prepared> {
    block_on(bare().prepare(NAME, &json!({"withdraw": true}).to_string())).unwrap()
}

fn ok() -> Outcome {
    Outcome::Completed(Completed {
        is_error: false,
        exit: None,
    })
}

#[test]
fn posting_results_match_open_busy_and_unavailable() {
    let call = FakeCall {
        id: "call-1".into(),
        ..FakeCall::default()
    };

    let host = FakeHost {
        post_reply: Some(Posted::Open),
        ..FakeHost::default()
    };
    let seen = host.posted_questions.clone();
    let run = block_on(plugin(host).run(prepared_ask(), &call));
    assert_eq!(run.outcome, ok());
    assert_eq!(run.result, Some(posted()));
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "call-1");
    assert_eq!(seen[0].1[0].question, "Go on?");

    let host = FakeHost {
        post_reply: Some(Posted::Busy),
        ..FakeHost::default()
    };
    let run = block_on(plugin(host).run(prepared_ask(), &call));
    assert_eq!(
        run.outcome,
        Outcome::Failed(Failure {
            message: BUSY_MESSAGE.into(),
            before_start: false,
        })
    );
    assert_eq!(run.result, Some(busy()));

    let run = block_on(bare().run(prepared_ask(), &call));
    assert_eq!(
        run.outcome,
        Outcome::Failed(Failure {
            message: UNAVAILABLE_MESSAGE.into(),
            before_start: false,
        })
    );
    assert_eq!(run.result.as_deref(), Some(UNAVAILABLE));
}

#[test]
fn withdraw_results_match_open_and_none_open() {
    let call = FakeCall {
        id: "call-w".into(),
        ..FakeCall::default()
    };

    let host = FakeHost {
        withdraw_reply: true,
        ..FakeHost::default()
    };
    let seen = host.withdraw_calls.clone();
    let run = block_on(plugin(host).run(prepared_withdraw(), &call));
    assert_eq!(run.outcome, ok());
    assert_eq!(run.result.as_deref(), Some(WITHDRAWN));
    assert_eq!(seen.lock().unwrap().as_slice(), ["call-w"]);

    let host = FakeHost::default();
    let seen = host.withdraw_calls.clone();
    let run = block_on(plugin(host).run(prepared_withdraw(), &call));
    assert_eq!(
        run.outcome,
        Outcome::Failed(Failure {
            message: NOTHING_OPEN_MESSAGE.into(),
            before_start: false,
        })
    );
    assert_eq!(run.result.as_deref(), Some(NOTHING_OPEN));
    assert_eq!(seen.lock().unwrap().as_slice(), ["call-w"]);
}

#[test]
fn the_definition_is_canonical_and_holds_no_dates_paths_or_ids() {
    let canonical =
        json!({"description": description(), "input_schema": schema(), "name": NAME}).to_string();
    assert_eq!(canonical.len(), 2695);
    assert!(canonical.starts_with(r#"{"description":"Ask the user"#));
    assert!(canonical.ends_with(r#""type":"object"},"name":"ask_user"}"#));
    assert!(!canonical.contains('/'));
    assert!(!canonical.contains("20"));
}

#[test]
fn a_model_cannot_claim_a_server_as_the_source_of_ask_user() {
    let mut question = one("Team", "Which team?");
    question["source"] = json!({"asked_by":{"kind":"server","name":"linear"},"typed":true});
    assert_eq!(
        refusal(&json!({"questions":[question]})),
        Some("unknown field `source` in question 1".into())
    );
}
