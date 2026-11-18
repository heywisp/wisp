use serde_json::{Value, json};

use super::*;

#[test]
fn a_report_is_one_non_empty_text_field() {
    assert_eq!(
        check(&json!({"text": "Done: plan written.\nDetails."})),
        Ok("Done: plan written.\nDetails.".to_owned())
    );
    for refused in [
        json!({"text": "   \n"}),
        json!({}),
        json!({"text": "Done.", "kind": "summary"}),
        json!({"kind": "summary", "status": "done", "summary": "s"}),
        json!({"text": 3}),
        json!("Done."),
        Value::Null,
    ] {
        let e = check(&refused).unwrap_err();
        assert_eq!(e, ONE_FIELD, "{refused}");
        assert!(e.contains("text"), "{refused}");
    }
}

#[test]
fn the_plugins_schema_takes_exactly_what_the_checker_takes() {
    let schema = wisp_plugin_agents::report_schema();
    let properties: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
    assert_eq!(properties, ["text"]);
    assert_eq!(schema["required"], json!(["text"]));
}

#[test]
fn a_logged_report_line_reads_text_or_a_typed_reports_summary() {
    assert_eq!(
        logged_line(&json!("\nShipped it.\nmore")),
        Some("Shipped it.".to_owned())
    );
    assert_eq!(
        logged_line(&json!({"kind": "plan", "status": "done", "summary": "Planned.\nDetails"})),
        Some("Planned.".to_owned())
    );
    assert_eq!(logged_line(&json!({"kind": "plan"})), None);
    let long = "x".repeat(300);
    assert_eq!(
        logged_line(&json!({"summary": long})).map(|l| l.len()),
        Some(200)
    );
}

#[test]
fn the_spawn_schemas_efforts_are_the_protocols() {
    let schema = wisp_plugin_agents::spawn_schema();
    assert_eq!(
        schema["properties"]["effort"]["enum"],
        json!(wisp_protocol::Effort::WORDS)
    );
    assert!(schema["properties"].get("report").is_none());
}

#[test]
fn the_spawn_and_rename_name_rule_is_the_protocols() {
    assert_eq!(
        wisp_plugin_agents::NAME_RULE,
        wisp_protocol::agents::preset_name_rule()
    );
    let spawn = wisp_plugin_agents::spawn_schema();
    let rename = wisp_plugin_agents::rename_schema();
    for described in [
        &spawn["properties"]["name"],
        &rename["properties"]["new_name"],
    ] {
        let text = described["description"].as_str().unwrap();
        assert!(text.contains(wisp_plugin_agents::NAME_RULE), "{text}");
    }
}

#[test]
fn the_plugins_tool_names_are_the_protocols() {
    assert_eq!(
        wisp_plugin_agents::NAMES,
        wisp_protocol::agents::AGENT_TOOLS
    );
}
