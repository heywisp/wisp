use serde_json::Value;

pub const ONE_FIELD: &str = "report takes one field, text, holding the whole report";

pub fn check(input: &Value) -> Result<String, String> {
    let Some(fields) = input.as_object() else {
        return Err(ONE_FIELD.to_owned());
    };
    if fields.len() != 1 {
        return Err(ONE_FIELD.to_owned());
    }
    match fields.get("text").and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_owned()),
        _ => Err(ONE_FIELD.to_owned()),
    }
}

pub fn logged_line(report: &Value) -> Option<String> {
    let text = match report {
        Value::String(text) => text.as_str(),
        typed => typed.get("summary").and_then(Value::as_str)?,
    };
    wisp_tool_text::agents::report_line(text)
}

#[cfg(test)]
mod tests;
