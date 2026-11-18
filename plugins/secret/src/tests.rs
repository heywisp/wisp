use futures::executor::block_on;
use wisp_plugin_api::manifest::{Capability, Manifest, Origin, Platform};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};
use wisp_tool_text::question::SECRET_CAPABILITY;

use super::*;

fn input() -> Value {
    json!({
        "process": 4,
        "name": "OpenAI API key",
        "reason": "To add it to Wisp as the account work.",
        "end_input": true,
        "show_start": 8,
        "show_end": 4
    })
}

fn plugin(end: SecretEnd) -> (AskSecret, wisp_plugin_testkit::host::SecretCalls) {
    let host = FakeHost {
        secret_end: Some(end),
        ..FakeHost::default()
    };
    let asked = host.secret_asked.clone();
    (AskSecret::new(Arc::new(host)), asked)
}

fn refusal(input: &Value) -> Option<String> {
    block_on(plugin(SecretEnd::Typed).0.prepare(NAME, &input.to_string())).err()
}

#[test]
fn the_manifest_names_match_the_shared_constants() {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, "secret");
    assert_eq!(manifest.world, "0.3.0");
    let [tool] = manifest.tools.as_slice() else {
        panic!("{:?}", manifest.tools)
    };
    assert_eq!(tool.name, SECRET_TOOL);
    assert_eq!(tool.provides, [SECRET_CAPABILITY]);
    assert_eq!(tool.platforms, [Platform::Unix]);
    assert_eq!(manifest.capabilities.requested, [Capability::Ask]);
}

#[test]
fn the_metadata_asks_the_user_ends_the_reply_and_has_no_time_limit() {
    let meta = meta();
    assert!(meta.asks_user && meta.ends_reply);
    assert!(!meta.parallel_safe && !meta.saves_output && !meta.reports_jobs);
    assert_eq!(meta.timeouts.max_ms, None);
    assert_eq!(
        block_on(plugin(SecretEnd::Typed).0.time_limit_ms(NAME, "{}")),
        None
    );
    assert_eq!(meta.writing.as_deref(), Some("a secret request"));
}

#[test]
fn the_tool_has_no_field_for_the_value_and_the_show_limit_is_one_constant() {
    let schema = schema();
    let properties = schema["properties"].as_object().unwrap();
    let mut names: Vec<&str> = properties.keys().map(String::as_str).collect();
    names.sort_unstable();
    let mut fields = FIELDS.to_vec();
    fields.sort_unstable();
    assert_eq!(names, fields);
    assert_eq!(schema["required"], json!(["name", "process", "reason"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    for field in ["show_start", "show_end"] {
        assert_eq!(properties[field]["maximum"], json!(SHOW_MAX));
        assert_eq!(properties[field]["minimum"], json!(0));
    }
    assert_eq!(SHOW_MAX, 16);
    let canonical = def().kind;
    let ToolKind::Client(client) = canonical else {
        panic!("not a client tool")
    };
    assert!(!client.description.contains('/') && !client.description.contains("20"));
}

#[test]
fn a_call_is_parsed_into_the_ask_and_the_defaults_are_zero_and_false() {
    assert_eq!(
        parse(&input()),
        Ok(SecretAsk {
            process: 4,
            name: "OpenAI API key".into(),
            reason: "To add it to Wisp as the account work.".into(),
            end_input: true,
            show_start: 8,
            show_end: 4,
        })
    );
    let bare = json!({"process": 1, "name": "sudo password", "reason": "To run apt."});
    let parsed = parse(&bare).unwrap();
    assert_eq!(
        (parsed.end_input, parsed.show_start, parsed.show_end),
        (false, 0, 0)
    );
    let edge = json!({"process": 1, "name": "n", "reason": "r", "show_start": 16, "show_end": 0});
    assert_eq!(parse(&edge).map(|ask| ask.show_start), Ok(16));
}

#[test]
fn a_bad_field_refuses_the_call_before_anyone_is_asked() {
    for field in ["show_start", "show_end"] {
        let sentence = format!("`{field}` must be a whole number from 0 to 16");
        for bad in [
            json!(4.5),
            json!(-1),
            json!(17),
            json!(256),
            json!("4"),
            json!(null),
            json!(true),
        ] {
            let mut call = input();
            call[field] = bad.clone();
            assert_eq!(refusal(&call), Some(sentence.clone()), "{field} {bad}");
        }
    }
    for (field, most) in [("name", NAME_CHARS), ("reason", REASON_CHARS)] {
        let sentence =
            format!("`{field}` must be one line of text, not empty, at most {most} characters");
        for bad in [
            json!("x".repeat(most + 1)),
            json!("two\nlines"),
            json!("bell\u{7}"),
            json!("  "),
            json!(""),
            json!(7),
        ] {
            let mut call = input();
            call[field] = bad.clone();
            assert_eq!(refusal(&call), Some(sentence.clone()), "{field} {bad}");
        }
    }
    for (field, bad) in [
        ("process", json!(0)),
        ("process", json!(-3)),
        ("process", json!("4")),
        ("process", json!(4_294_967_296_u64)),
        ("end_input", json!("yes")),
        ("secret", json!("CANARY-secret-0001")),
    ] {
        let mut call = input();
        call[field] = bad.clone();
        let said = refusal(&call).unwrap_or_default();
        assert!(!said.is_empty(), "{field} {bad}");
        assert!(!said.contains("CANARY"), "{said}");
    }
    let mut missing = input();
    missing.as_object_mut().unwrap().remove("reason");
    assert!(refusal(&missing).is_some());
    assert!(refusal(&json!([1])).is_some());
    assert_eq!(
        block_on(plugin(SecretEnd::Typed).0.prepare(NAME, "{")).err(),
        Some("input is not JSON: EOF while parsing an object at line 1 column 1".to_owned())
    );
    let full_name = "n".repeat(NAME_CHARS);
    let mut longest = input();
    longest["name"] = json!(full_name);
    assert_eq!(refusal(&longest), None);
}

#[test]
fn the_row_asks_for_the_name_and_reads_entered_when_it_is_done() {
    let prepared = block_on(
        plugin(SecretEnd::Typed)
            .0
            .prepare(NAME, &input().to_string()),
    );
    assert_eq!(
        prepared.unwrap().display(),
        Display {
            action: Action::Other("ask_secret".into()),
            verbs: Some(Verbs {
                doing: "Asking for".into(),
                done: "Entered".into(),
                failed: "couldn't ask".into(),
            }),
            target: "OpenAI API key".into(),
            output: OutputKind::None,
            view: None,
            title: None,
        }
    );
}

fn result_of(end: SecretEnd, end_input: bool) -> ToolRun {
    let (plugin, _) = plugin(end);
    let mut call = input();
    call["end_input"] = json!(end_input);
    call["process"] = json!(7);
    let prepared = block_on(plugin.prepare(NAME, &call.to_string())).unwrap();
    block_on(plugin.run(prepared, &FakeCall::default()))
}

#[test]
fn each_end_has_its_own_sentence_and_outcome() {
    let failed = |sentence: &str| {
        (
            Outcome::Failed(Failure {
                message: sentence.to_owned(),
                before_start: false,
            }),
            sentence.to_owned(),
        )
    };
    let cases = [
        (
            SecretEnd::ProcessEnded,
            failed("Process 7 ended before the user entered the secret, so nothing was typed."),
        ),
        (
            SecretEnd::Unavailable,
            failed(
                "Not asked: no one can enter a secret in this session. Don't ask for it in the chat or from another agent; say what the process needs.",
            ),
        ),
        (
            SecretEnd::Refused(SecretRefusal::NoProcess),
            failed(&format!("Not asked: {}.", jobs::no_process(7))),
        ),
        (
            SecretEnd::Refused(SecretRefusal::Ended),
            failed("Not asked: process 7 has ended."),
        ),
        (
            SecretEnd::Refused(SecretRefusal::ShowsInput),
            failed(
                "Not typed: process 7's terminal would show what is typed, so the secret would land in its output. If it hasn't reached its prompt yet, wait for the prompt and call again; otherwise start the command again without tty so that it reads the secret from standard input.",
            ),
        ),
        (
            SecretEnd::Refused(SecretRefusal::NoInput),
            failed(
                "Not asked: process 7 takes no input. Start the command with run_in_background, which keeps its standard input open, and call again.",
            ),
        ),
        (
            SecretEnd::Refused(SecretRefusal::Attached),
            failed("Not asked: the user is typing into process 7 themselves."),
        ),
        (
            SecretEnd::Refused(SecretRefusal::TooLong),
            failed(
                "Not typed: the secret is longer than one terminal line can take. Start the command again without tty so that it reads standard input.",
            ),
        ),
        (
            SecretEnd::Failed,
            failed(
                "The secret could not be typed into process 7: its input closed or the write stopped part-way. Check the process, and ask again if it still waits.",
            ),
        ),
    ];
    for (end, (outcome, sentence)) in cases {
        let run = result_of(end, true);
        assert_eq!(run.outcome, outcome, "{end:?}");
        assert_eq!(run.result.as_deref(), Some(sentence.as_str()), "{end:?}");
        assert!(run.facts.is_empty(), "{end:?}");
    }
    let stopped = result_of(SecretEnd::Stopped, true);
    assert_eq!(stopped.outcome, Outcome::Cancelled);
    assert_eq!(
        stopped.result.as_deref(),
        Some("Not entered: the user stopped the turn while the secret box was open.")
    );
    assert!(stopped.facts.is_empty());
}

#[test]
fn a_typed_secret_says_how_it_ended_and_the_process_is_the_rows_fact() {
    let enter = result_of(SecretEnd::Typed, false);
    assert_eq!(
        enter.result.as_deref(),
        Some(
            "The user entered the secret and Wisp typed it into process 7, then pressed Enter. You were not shown it; check the process to see whether it was accepted."
        )
    );
    let closed = result_of(SecretEnd::Typed, true);
    assert_eq!(
        closed.result.as_deref(),
        Some(
            "The user entered the secret and Wisp typed it into process 7 and closed its input. You were not shown it; check the process to see whether it was accepted."
        )
    );
    for run in [enter, closed] {
        assert_eq!(run.facts, [Fact::Process(7)]);
        assert_eq!(
            run.outcome,
            Outcome::Completed(wisp_plugin_api::types::Completed {
                is_error: false,
                exit: None,
            })
        );
    }
}

#[test]
fn the_host_is_asked_once_with_the_ask_as_the_model_gave_it() {
    let (plugin, asked) = plugin(SecretEnd::Typed);
    let prepared = block_on(plugin.prepare(NAME, &input().to_string())).unwrap();
    let call = FakeCall {
        id: "toolu_07".into(),
        ..FakeCall::default()
    };
    block_on(plugin.run(prepared, &call));
    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        [(
            "toolu_07".to_owned(),
            SecretAsk {
                process: 4,
                name: "OpenAI API key".into(),
                reason: "To add it to Wisp as the account work.".into(),
                end_input: true,
                show_start: 8,
                show_end: 4,
            }
        )]
    );
}
