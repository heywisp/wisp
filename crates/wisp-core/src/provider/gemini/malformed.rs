use std::collections::BTreeSet;

use serde_json::Value;
use wisp_protocol::agents::MAIL_OPEN;
use wisp_protocol::{CallForm, MalformedCall};

const CALL_PREFIX: &str = "call:default_api:";
const PYTHON_PREFIX: &str = "default_api.";
const EMPTY_CALL: &str = "Function call is empty";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Sent {
    pub(super) tools: BTreeSet<String>,
    pub(super) stop_sequences: Vec<String>,
}

impl Sent {
    pub(super) fn from_body(body: &Value) -> Self {
        let tools = body
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.get("functionDeclarations").and_then(Value::as_array))
            .flatten()
            .filter_map(|decl| decl.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        let stop_sequences = body
            .pointer("/generationConfig/stopSequences")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|seq| !seq.is_empty())
            .map(str::to_owned)
            .collect();
        Self {
            tools,
            stop_sequences,
        }
    }
}

pub(super) fn labels(message: Option<&str>, sent: &Sent) -> MalformedCall {
    let Some(message) = message.filter(|m| !m.is_empty()) else {
        return MalformedCall {
            form: CallForm::Absent,
            balanced: true,
            ..MalformedCall::default()
        };
    };
    let (form, raw_name) = parse_form(message);
    let (function, undeclared) = match raw_name {
        Some(name) if sent.tools.contains(name) => (Some(name.to_owned()), false),
        Some(_) => (None, true),
        None => (None, false),
    };
    MalformedCall {
        form,
        function,
        undeclared,
        chars: u64::try_from(message.chars().count()).unwrap_or(u64::MAX),
        balanced: balanced(message),
        stop_string: sent
            .stop_sequences
            .iter()
            .any(|seq| message.contains(seq.as_str())),
        mail_tag: message.contains(MAIL_OPEN),
    }
}

fn parse_form(message: &str) -> (CallForm, Option<&str>) {
    let call = parse_prefixed(message, CALL_PREFIX, '{', CallForm::Call);
    let python = parse_prefixed(message, PYTHON_PREFIX, '(', CallForm::Python);
    let empty = message
        .find(EMPTY_CALL)
        .map(|start| (start, CallForm::EmptyCall, None));
    [call, python, empty]
        .into_iter()
        .flatten()
        .min_by_key(|(start, _, _)| *start)
        .map_or((CallForm::Other, None), |(_, form, name)| (form, name))
}

fn parse_prefixed<'a>(
    message: &'a str,
    prefix: &str,
    open: char,
    form: CallForm,
) -> Option<(usize, CallForm, Option<&'a str>)> {
    let start = message.find(prefix)?;
    let after = &message[start + prefix.len()..];
    let (raw, _) = after.split_once(open)?;
    let name = (!raw.is_empty()).then_some(raw);
    Some((start, form, name))
}

pub(super) fn balanced(message: &str) -> bool {
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in message.chars() {
        if let Some(open_quote) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == open_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '(' => stack.push(')'),
            '}' | ']' | ')' if stack.pop() != Some(ch) => return false,
            _ => {}
        }
    }
    quote.is_none() && stack.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_classify_forms_names_balance_stops_and_mail_without_leaking_text() {
        use CallForm::*;
        let stop = format!("\nuser\n{}", wisp_tool_text::SENT_PREFIX);
        let sent = Sent {
            tools: ["lookup", "fetch_page"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            stop_sequences: vec![stop.clone(), format!("\n{MAIL_OPEN} from=\"")],
        };
        let marker = "PRIVATE_MARKER_TEXT";
        let stop_msg = format!("call:default_api:lookup{{command:\"hi{stop}fake\"}}");
        let mail_msg = format!("call:default_api:lookup{{command:\"{MAIL_OPEN} from=x\"}}");
        let undeclared_msg =
            format!("Malformed function call: call:default_api:{marker}{{body:\"{marker}\"}}");
        let unicode_msg = "call:default_api:lookup{text:\"é🦀\"}";
        let ok = |form, function: Option<&str>| MalformedCall {
            form,
            function: function.map(str::to_owned),
            balanced: true,
            ..MalformedCall::default()
        };

        let cases = [
            (None, ok(Absent, None)),
            (Some(""), ok(Absent, None)),
            (
                Some("Malformed function call: call:default_api:lookup{command:\"ls\"}"),
                ok(Call, Some("lookup")),
            ),
            (
                Some("Malformed function call: print(default_api.fetch_page(to='..', text='ok'))"),
                ok(Python, Some("fetch_page")),
            ),
            (
                Some("Malformed function call: default_api.lookup(command='ls')"),
                ok(Python, Some("lookup")),
            ),
            (
                Some("Failed to parse function call: Function call is empty"),
                ok(EmptyCall, None),
            ),
            (
                Some("Malformed function call: unrecognised output"),
                ok(Other, None),
            ),
            (
                Some(undeclared_msg.as_str()),
                MalformedCall {
                    undeclared: true,
                    ..ok(Call, None)
                },
            ),
            (
                Some("default_api.unknown_tool(x=1)"),
                MalformedCall {
                    undeclared: true,
                    ..ok(Python, None)
                },
            ),
            (Some("call:default_api:{x:1}"), ok(Call, None)),
            (
                Some("call:default_api:lookup{command:\"ls\""),
                MalformedCall {
                    balanced: false,
                    ..ok(Call, Some("lookup"))
                },
            ),
            (
                Some("call:default_api:lookup{a:[1,2)}"),
                MalformedCall {
                    balanced: false,
                    ..ok(Call, Some("lookup"))
                },
            ),
            (
                Some("call:default_api:lookup{command:\"unclosed}"),
                MalformedCall {
                    balanced: false,
                    ..ok(Call, Some("lookup"))
                },
            ),
            (
                Some(r#"call:default_api:lookup{cmd:"a \"[(\\\" 'b \\\\"}"#),
                ok(Call, Some("lookup")),
            ),
            (
                Some(r#"call:default_api:lookup{cmd:"print(default_api.other(1))"}"#),
                ok(Call, Some("lookup")),
            ),
            (
                Some(r#"print(default_api.lookup(cmd="call:default_api:other{1}"))"#),
                ok(Python, Some("lookup")),
            ),
            (Some(unicode_msg), ok(Call, Some("lookup"))),
            (
                Some(stop_msg.as_str()),
                MalformedCall {
                    stop_string: true,
                    ..ok(Call, Some("lookup"))
                },
            ),
            (
                Some(mail_msg.as_str()),
                MalformedCall {
                    mail_tag: true,
                    ..ok(Call, Some("lookup"))
                },
            ),
        ];

        for (input, mut expected) in cases {
            expected.chars = input.map_or(0, |text| text.chars().count() as u64);
            let got = labels(input, &sent);
            assert_eq!(got, expected, "{input:?}");
            let serialized = serde_json::to_string(&got).unwrap();
            assert!(!serialized.contains(marker), "{serialized}");
        }
        assert!(unicode_msg.chars().count() < unicode_msg.len());
        assert!(!balanced("call:default_api:lookup{a:1}}"));
        assert!(!balanced(r#"call:default_api:lookup{a:"\""#));
        assert!(balanced(r#"default_api.lookup(cmd='hi "[{')"#));
    }
}
