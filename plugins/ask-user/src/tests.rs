use std::sync::Arc;
use std::sync::Mutex;

use futures::executor::block_on;
use wisp_plugin_api::types::{Answer, Completed};
use wisp_plugin_testkit::host::FakeCall;

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

fn refusal(input: &Value) -> Option<String> {
    block_on(AskUser.prepare(NAME, &input.to_string())).err()
}

fn opt(label: &str, description: &str) -> QuestionOption {
    QuestionOption {
        label: label.into(),
        description: description.into(),
    }
}

fn choose(index: u32) -> Answer {
    Answer {
        chosen: vec![index],
        own: None,
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
    assert_eq!(block_on(AskUser.time_limit_ms(NAME, "{}")), None);
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
    let prepared = block_on(AskUser.prepare(NAME, &input.to_string())).unwrap();
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
        prepared
            .into_any()
            .downcast::<Vec<Question>>()
            .ok()
            .map(|q| *q),
        Some(plan_questions())
    );
}

#[test]
fn prepare_refuses_every_rule_of_the_input_table() {
    let cases: Vec<(Value, String)> = vec![
        (json!("hi"), "input must be an object".into()),
        (json!({}), "`questions` must be an array".into()),
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
    let prepared = block_on(AskUser.prepare(NAME, &input.to_string())).unwrap();
    assert_eq!(prepared.display().target, "Deployment … (+1)");
    let questions = *prepared.into_any().downcast::<Vec<Question>>().unwrap();
    assert_eq!(questions[0].header, "Deployment target");
    let single = block_on(AskUser.prepare(
        NAME,
        &json!({"questions": [one("Database", "Q?")]}).to_string(),
    ))
    .unwrap();
    assert_eq!(single.display().target, "Database");
}

fn prepared() -> Box<dyn Prepared> {
    block_on(AskUser.prepare(
        NAME,
        &json!({"questions": [one("Go", "Go on?")]}).to_string(),
    ))
    .unwrap()
}

type Seen = Arc<Mutex<Vec<Vec<Question>>>>;

fn answering(reply: AskReply) -> (FakeCall, Seen) {
    let seen: Seen = Arc::default();
    let kept = seen.clone();
    let call = FakeCall {
        ask: Some(Arc::new(move |questions| {
            kept.lock().unwrap().push(questions);
            Box::pin(std::future::ready(reply.clone()))
        })),
        ..FakeCall::default()
    };
    (call, seen)
}

fn ok() -> Outcome {
    Outcome::Completed(Completed {
        is_error: false,
        exit: None,
    })
}

#[test]
fn without_a_channel_the_question_is_not_asked() {
    let call = FakeCall::default();
    let run = block_on(AskUser.run(prepared(), &call));
    assert_eq!(run.result.as_deref(), Some(UNAVAILABLE));
    assert_eq!(
        run.outcome,
        Outcome::Failed(Failure {
            message: UNAVAILABLE_MESSAGE.into(),
            before_start: false,
        })
    );
}

#[test]
fn an_answer_comes_back_as_the_result_text() {
    let (call, asked) = answering(AskReply::Answered(vec![choose(1)]));
    let run = block_on(AskUser.run(prepared(), &call));
    assert_eq!(
        run.result.as_deref(),
        Some("The user answered:\n\n1. [Go] Go on?\n   No")
    );
    assert_eq!(run.outcome, ok());
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0][0].question, "Go on?");
}

#[test]
fn a_message_instead_is_completed_and_says_so() {
    let (call, _) = answering(AskReply::MessageInstead);
    let run = block_on(AskUser.run(prepared(), &call));
    assert_eq!(run.result.as_deref(), Some(MESSAGE_INSTEAD));
    assert_eq!(run.outcome, ok());
}

#[test]
fn a_stop_wins_over_a_reply_that_is_ready_at_the_same_moment() {
    let (call, _) = answering(AskReply::Answered(vec![choose(0)]));
    call.cancel.set();
    let run = block_on(AskUser.run(prepared(), &call));
    assert_eq!(run.result.as_deref(), Some(STOPPED));
    assert_eq!(run.outcome, Outcome::Cancelled);
}

#[test]
fn a_stop_ends_a_question_nobody_answers() {
    let call = FakeCall {
        ask: Some(Arc::new(|_| Box::pin(std::future::pending()))),
        ..FakeCall::default()
    };
    let stopper = call.cancel.clone();
    let stopping = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        stopper.set();
    });
    let run = block_on(AskUser.run(prepared(), &call));
    stopping.join().unwrap();
    assert_eq!(run.result.as_deref(), Some(STOPPED));
    assert_eq!(run.outcome, Outcome::Cancelled);
}

#[test]
fn the_definition_is_canonical_and_holds_no_dates_paths_or_ids() {
    let canonical =
        json!({"description": DESCRIPTION, "input_schema": schema(), "name": NAME}).to_string();
    assert_eq!(canonical.len(), 2129);
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
