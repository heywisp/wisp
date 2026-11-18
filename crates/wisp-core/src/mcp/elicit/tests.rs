use serde_json::json;

use super::*;

fn schema(properties: Value) -> rmcp::model::ElicitationSchema {
    serde_json::from_value(json!({"type":"object","properties":properties})).unwrap()
}

fn flight() -> Registered {
    Flight::register(
        Arc::default(),
        "linear".into(),
        UserChannel::none(),
        CancellationToken::new(),
        Redactions::new(["CANARY-0001".into()]),
    )
}

#[test]
fn fields_to_questions() {
    for word in [
        "apiKey",
        "auth_token",
        "Password",
        "passphrase",
        "credentials",
        "Issue key",
        "Keywords",
    ] {
        assert!(secret_word(word), "{word}");
    }
    for word in ["Assignee", "Release notes"] {
        assert!(!secret_word(word), "{word}");
    }
    let registered = flight();
    let flight = &registered.0;
    let fields = fields(&schema(json!({
        "a":{"type":"string","title":"Title CANARY-0001","description":"Description"},
        "b":{"type":"number"}, "c":{"type":"integer","title":"Count"},
        "d":{"type":"boolean"},
        "e":{"type":"string","oneOf":[{"const":"one","title":"One"}]},
        "f":{"type":"array","items":{"anyOf":[{"const":"two","title":"Two"}]}},
        "g":{"type":"string","enum":["x"],"enumNames":["X"]},
        "h":{"type":"string","title":"Admin token"}
    })))
    .unwrap();
    let questions = questions(&fields, "Message.", flight);
    assert_eq!(questions.len(), 7);
    assert_eq!(questions[0].question, "Message. Description");
    assert_eq!(questions[0].header, "Title [redacted]");
    assert!(!flight.received_secret.load(Ordering::Acquire));
    assert_eq!(questions[1].question, "b");
    assert_eq!(questions[2].question, "Count");
    assert_eq!(
        questions[3]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["Yes", "No", "Not now"]
    );
    assert_eq!(questions[4].options[0].label, "One");
    assert!(questions[5].multi_select);
    assert_eq!(questions[6].options[0].label, "X");
    assert!(questions.iter().all(Question::kept_from_model));
    let one = super::questions(&fields[..1], "Only.", flight);
    assert_eq!(one[0].header, "Needs you");
    for (kind, name, title) in [
        ("integer", "one_time_password", "Value"),
        ("number", "auth_token", "Value"),
        ("integer", "value", "Password"),
        ("number", "value", "Authentication token"),
    ] {
        let secret_fields =
            super::fields(&schema(json!({name:{"type":kind,"title":title}}))).unwrap();
        assert!(secret_fields[0].secret);
        assert!(super::questions(&secret_fields, "Message", flight).is_empty());
        assert_eq!(
            typed_value(&secret_fields[0].kind, "1.25e2".into()).as_f64(),
            Some(125.0)
        );
        assert_eq!(
            typed_value(&secret_fields[0].kind, "CANARY-not-a-number".into()),
            json!("CANARY-not-a-number")
        );
    }
    for field in [
        json!({"type":"boolean"}),
        json!({"type":"string","enum":["A","B"]}),
    ] {
        let choice = super::fields(&schema(json!({"auth_token":field}))).unwrap();
        assert!(!choice[0].secret);
        assert!(!super::questions(&choice, "Message", flight)[0].is_typed());
    }
    for properties in [
        json!({"x":{"type":"string","title":"x".repeat(text::MAX_FORM_LABEL_CHARS + 1)}}),
        json!({"x":{"type":"string","description":"x".repeat(text::MAX_FORM_DESCRIPTION_CHARS + 1)}}),
        json!({"x":{"type":"string","enum":["x".repeat(text::MAX_FORM_LABEL_CHARS + 1)]}}),
        json!({"x":{"type":"string","enum":vec!["x"; text::MAX_FORM_CHOICES + 1]}}),
        json!({"x".repeat(text::MAX_FORM_LABEL_CHARS + 1):{"type":"string"}}),
        Value::Object(
            (0..=text::MAX_FORM_FIELDS)
                .map(|i| (i.to_string(), json!({"type":"string"})))
                .collect(),
        ),
    ] {
        assert!(super::fields(&schema(properties)).is_none());
    }
    let boundary = json!({"x".repeat(text::MAX_FORM_LABEL_CHARS):{"type":"string","title":"x".repeat(text::MAX_FORM_LABEL_CHARS),"description":"x".repeat(text::MAX_FORM_DESCRIPTION_CHARS),"enum":vec!["x".repeat(text::MAX_FORM_LABEL_CHARS); text::MAX_FORM_CHOICES]}});
    assert!(super::fields(&schema(boundary)).is_some());
}

#[test]
fn answers_to_result() {
    let fields = fields(&schema(json!({"a":{"type":"string"},"b":{"type":"number"},"c":{"type":"boolean"},"d":{"type":"array","items":{"type":"string","enum":["one","two"]}},"e":{"type":"string"}}))).unwrap();
    let mut answers = vec![
        Answer::own("text"),
        Answer::own("2.5"),
        Answer::choose(0),
        Answer {
            chosen: vec![0, 1],
            own: None,
        },
        Answer::skipped(),
    ];
    assert_eq!(
        values(&fields, &answers),
        Ok(json!({"a":"text","b":2.5,"c":true,"d":["one","two"]})
            .as_object()
            .unwrap()
            .clone())
    );
    answers[1] = Answer::own("not a number");
    assert_eq!(values(&fields, &answers).unwrap()["b"], "not a number");
    answers[2] = Answer::choose(1);
    assert_eq!(values(&fields, &answers).unwrap()["c"], false);
    answers[2] = Answer::own("do something else");
    assert_eq!(
        values(&fields, &answers),
        Err(Some("do something else".into()))
    );
    answers[2] = Answer::choose(2);
    assert_eq!(values(&fields, &answers), Err(None));
    answers[2] = Answer::choose(0);
    answers[3].chosen.push(2);
    assert_eq!(values(&fields, &answers), Err(None));
    answers[3] = Answer::skipped();
    assert!(!values(&fields, &answers).unwrap().contains_key("d"));
}
