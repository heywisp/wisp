use futures::executor::block_on;
use wisp_plugin_api::host::TodoStatus;
use wisp_plugin_api::manifest::{Manifest, Origin, Switch};
use wisp_plugin_api::types::Completed;
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use super::*;

fn item(text: &str, status: &str) -> Value {
    json!({"text": text, "status": status})
}

fn todo(text: &str, status: TodoStatus) -> Todo {
    Todo {
        text: text.to_owned(),
        status,
        section: None,
    }
}

fn plugin(host: &FakeHost) -> Arc<dyn Plugin> {
    instantiate(Arc::new(host.clone()))
}

fn refusal(input: &Value) -> Option<String> {
    block_on(plugin(&FakeHost::default()).prepare(NAME, &input.to_string())).err()
}

#[test]
fn the_validation_table() {
    let fifty: Vec<Value> = (1..=50)
        .map(|i| item(&format!("step {i}"), "pending"))
        .collect();
    let fifty_one: Vec<Value> = (1..=51)
        .map(|i| item(&format!("step {i}"), "pending"))
        .collect();
    let cases: Vec<(Value, Option<&str>)> = vec![
        (json!({"todos": [item("Read", "pending")]}), None),
        (
            json!({"todos": [{"text": "Read", "status": "pending", "section": "Next"}]}),
            None,
        ),
        (
            json!({"todos": [{"text": "Read", "status": "pending", "section": null}]}),
            None,
        ),
        (
            json!({"todos": [item("Read", "pending"), {"text": "Write", "status": "pending", "section": 1}]}),
            Some("item 2's `section` must be a string"),
        ),
        (
            json!({"todos": [{"text": "Read", "status": "pending", "section": "x".repeat(61)}]}),
            Some("item 1's `section` is 61 characters; the limit is 60"),
        ),
        (
            json!({"todos": [{"text": "Read", "status": "pending", "section": "Next\nLater"}]}),
            Some("item 1's `section` must be one line with no control characters"),
        ),
        (
            json!({"todos": [{"text": " ", "status": "pending", "section": "x".repeat(61)}]}),
            Some("item 1's `text` is blank"),
        ),
        (
            json!({"todos": [{"text": "Read", "status": "pending", "section": "x".repeat(61)}, item(" ", "pending")]}),
            Some("item 1's `section` is 61 characters; the limit is 60"),
        ),
        (json!({"todos": fifty}), None),
        (
            json!({"todos": [item(&"x".repeat(500), "in_progress"), item("b", "completed"), item("c", "cancelled")]}),
            None,
        ),
        (
            json!({"todos": []}),
            Some("`todos` has 0 items; give 1 to 50"),
        ),
        (
            json!({"todos": fifty_one}),
            Some("`todos` has 51 items; give 1 to 50"),
        ),
        (json!({"todos": "a"}), Some("`todos` must be an array")),
        (json!({}), Some("`todos` must be an array")),
        (json!([]), Some("input must be an object")),
        (
            json!({"todos": [item("a", "pending")], "plan": 1}),
            Some("unknown field `plan`"),
        ),
        (json!({"todos": ["a"]}), Some("item 1 must be an object")),
        (
            json!({"todos": [{"text": "a", "status": "pending", "id": 1}]}),
            Some("unknown field `id` in item 1"),
        ),
        (
            json!({"todos": [item("a", "pending"), {"status": "pending"}]}),
            Some("item 2's `text` must be a string"),
        ),
        (
            json!({"todos": [item(" \n", "pending")]}),
            Some("item 1's `text` is blank"),
        ),
        (
            json!({"todos": [item(&"x".repeat(501), "pending")]}),
            Some("item 1's `text` is 501 characters; the limit is 500"),
        ),
        (
            json!({"todos": [item("a", "blocked")]}),
            Some("item 1's `status` must be pending, in_progress, completed or cancelled"),
        ),
        (
            json!({"todos": [{"text": "a"}]}),
            Some("item 1's `status` must be pending, in_progress, completed or cancelled"),
        ),
        (
            json!({"todos": [item("a", "in_progress"), item("b", "in_progress")]}),
            Some(
                "items 1 and 2 are both in_progress; at most one item may be in_progress at a time",
            ),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(refusal(&input).as_deref(), expected, "{input}");
    }
    assert_eq!(
        block_on(plugin(&FakeHost::default()).prepare(NAME, "{"))
            .err()
            .map(|e| e.starts_with("input is not JSON")),
        Some(true)
    );
}

#[test]
fn text_is_trimmed_and_the_display_counts_completed_items() {
    let items = parse(&json!({"todos": [
        {"text": "Write the plan", "status": "in_progress", "section": " Next "},
        item("  Read the plan ", "completed"),
        {"text": "Review", "status": "pending", "section": "Next"},
    ]}))
    .unwrap();
    assert_eq!(
        items,
        [
            todo("Read the plan", TodoStatus::Completed),
            Todo {
                section: Some("Next".to_owned()),
                ..todo("Write the plan", TodoStatus::InProgress)
            },
            Todo {
                section: Some("Next".to_owned()),
                ..todo("Review", TodoStatus::Pending)
            },
        ]
    );
    let shown = display(&items);
    assert_eq!(shown.action, Action::Other("write_todos".to_owned()));
    assert_eq!(shown.target, "1 of 3 completed");
    assert_eq!(shown.output, OutputKind::None);
}

#[test]
fn a_run_hands_the_list_to_the_host_and_answers_with_one_line() {
    let host = FakeHost::default();
    let plugin = plugin(&host);
    let input = json!({"todos": [
        item("Read the plan", "completed"),
        {"text": "Write the plan", "status": "in_progress", "section": "Next"},
        {"text": "Review it", "status": "pending", "section": "Next"},
        {"text": "Drop the draft", "status": "cancelled", "section": "Next"},
        {"text": "Merge", "status": "completed", "section": "Next"},
    ]});
    let call = block_on(plugin.prepare(NAME, &input.to_string())).unwrap();
    let ctx = FakeCall {
        id: "toolu_1".to_owned(),
        ..FakeCall::default()
    };
    let run = block_on(plugin.run(call, &ctx));
    assert_eq!(
        run.outcome,
        Outcome::Completed(Completed {
            is_error: false,
            exit: None
        })
    );
    assert_eq!(
        run.result.as_deref(),
        Some("Todos updated: 2 of 5 completed. In progress: \"Write the plan\".")
    );
    let replaced = host.todos.lock().unwrap().clone();
    assert_eq!(replaced.len(), 1);
    assert_eq!(replaced[0].0, "toolu_1");
    assert_eq!(replaced[0].1.len(), 5);
    assert_eq!(
        replaced[0].1[1],
        Todo {
            section: Some("Next".to_owned()),
            ..todo("Write the plan", TodoStatus::InProgress)
        }
    );
}

#[test]
fn a_list_the_host_refuses_fails_with_its_reason() {
    let host = FakeHost {
        refuse_todos: Some("no call named toolu_9 is running".to_owned()),
        ..FakeHost::default()
    };
    let plugin = plugin(&host);
    let call =
        block_on(plugin.prepare(NAME, &json!({"todos": [item("a", "pending")]}).to_string()))
            .unwrap();
    let run = block_on(plugin.run(call, &FakeCall::default()));
    assert_eq!(
        run.outcome,
        Outcome::Failed(Failure {
            message: "no call named toolu_9 is running".to_owned(),
            before_start: false,
        })
    );
    assert_eq!(
        run.result.as_deref(),
        Some("no call named toolu_9 is running")
    );
    assert!(host.todos.lock().unwrap().is_empty());
}

#[test]
fn the_definition_is_static_and_on_by_default() {
    let context = BuildContext {
        platform: "unix".to_owned(),
        capabilities: vec!["shell".to_owned()],
    };
    let bare = BuildContext {
        platform: "windows".to_owned(),
        capabilities: Vec::new(),
    };
    let one = block_on(plugin(&FakeHost::default()).describe(&context));
    let two = block_on(plugin(&FakeHost::default()).describe(&bare));
    assert_eq!(one, two);
    assert_eq!(one, vec![def()]);
    let ToolKind::Client(client) = &one[0].kind else {
        panic!("{:?}", one[0].kind);
    };
    assert!(client.description.contains("multi-step work"));
    assert!(!client.description.chars().any(|c| c == '/' || c == '\\'));
    let schema: Value = serde_json::from_str(&client.input_schema).unwrap();
    assert_eq!(schema, super::schema());
    assert!(!client.meta.needs_workspace);
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, "todos");
    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].name, NAME);
    assert_eq!(manifest.tools[0].default, Switch::On);
    assert!(manifest.system.is_none());
}
