use serde_json::{Value, json};
use wisp_plugin_api::types::{Action, Display, View};

use crate::process::input::parse;
use crate::process::{display, rejected};

fn shown(input: Value) -> Display {
    display(&parse(&input).unwrap())
}

fn titled(input: Value) -> Option<String> {
    shown(input).title
}

#[test]
fn the_title_is_the_description_on_every_kind_of_call() {
    let words = Some("Ask the reviewer for its findings".to_owned());
    for input in [
        json!({"description": "Ask the reviewer for its findings"}),
        json!({"id": 3, "input": "go on", "keys": ["Enter"], "description": "Ask the reviewer for its findings"}),
        json!({"id": 3, "signal": "kill", "description": "Ask the reviewer for its findings"}),
        json!({"id": 3, "ask_user_to_attach": "log in", "description": "  Ask the reviewer for its findings "}),
    ] {
        assert_eq!(titled(input.clone()), words, "{input}");
    }
    assert_eq!(titled(json!({"id": 3, "input": "go on"})), None);
    assert_eq!(titled(json!({"id": 3, "description": " "})), None);
}

#[test]
fn a_rejected_call_takes_its_title_and_never_its_input() {
    let refused = rejected(&json!({"id": 99, "input": "go on", "description": "Ask the reviewer"}));
    assert_eq!(refused.title.as_deref(), Some("Ask the reviewer"));
    assert_eq!(refused.action, Action::Input);
    let Some(View::Process(view)) = refused.view else {
        panic!("a process view");
    };
    assert_eq!(view.text, None);
    assert_eq!(rejected(&json!({"id": 99, "input": "go on"})).title, None);
    assert_eq!(rejected(&json!({"id": 99, "description": 4})).title, None);
    assert_eq!(rejected(&Value::Null).title, None);
}

#[test]
fn a_description_repeating_typed_text_is_left_off_the_row() {
    assert_eq!(
        titled(
            json!({"id": 3, "input": "admin\rhunter2\r", "description": "Log in as admin with hunter2"})
        ),
        None
    );
    assert_eq!(
        titled(
            json!({"id": 3, "input": "make test", "keys": ["Enter"], "description": "Run make test again"})
        ),
        None
    );
    assert_eq!(
        titled(
            json!({"id": 3, "input": "y", "keys": ["Enter"], "description": "Say yes to the prompt"})
        ),
        Some("Say yes to the prompt".to_owned())
    );
    assert_eq!(
        titled(json!({"id": 3, "input": "y\nno\n", "description": "Answer y and no"})),
        Some("Answer y and no".to_owned())
    );
    assert_eq!(
        titled(
            json!({"id": 3, "input": "y\nsecret-word\n", "description": "Answer with secret-word"})
        ),
        None
    );
    assert_eq!(
        titled(json!({"id": 3, "input": "  pass  \n", "description": "Send the pass"})),
        None
    );
    assert_eq!(
        titled(json!({"id": 3, "input": "ab\tcd\r", "description": "Type ab\tcd"})),
        None
    );
    assert_eq!(
        titled(
            json!({"id": 3, "input": "admin\thunter2\r", "description": "Log in as admin with hunter2"})
        ),
        None
    );
    assert_eq!(
        titled(
            json!({"id": 3, "input": "hunter2\u{1b}:wq", "description": "Save hunter2 in the editor"})
        ),
        None
    );
    assert_eq!(
        titled(json!({"id": 3, "input": "\u{1b}[A\r", "description": "Repeat the last command"})),
        Some("Repeat the last command".to_owned())
    );
    let refused = rejected(
        &json!({"id": 99, "input": "admin\rhunter2\r", "description": "Log in with hunter2"}),
    );
    assert_eq!(refused.title, None);
    let Some(View::Process(view)) = refused.view else {
        panic!("a process view");
    };
    assert_eq!(view.text, None);
}
