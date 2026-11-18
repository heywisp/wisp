use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "child" {
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    if mode == "exit" {
        return Ok(());
    }
    let key_name = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "WISP_TEST_TOKEN".into());
    if mode == "key"
        && std::env::var_os(&key_name).is_none_or(|value| {
            value.is_empty() || value.to_string_lossy().starts_with("CANARY-rejected")
        })
    {
        return Ok(());
    }
    std::fs::write("server.pid", std::process::id().to_string())?;
    let mut child = if mode == "descendant" {
        let child = std::process::Command::new(std::env::current_exe()?)
            .arg("child")
            .spawn()?;
        std::fs::write("child.pid", child.id().to_string())?;
        Some(child)
    } else {
        None
    };
    let mut output = std::io::stdout().lock();
    let mut pending = Vec::new();
    let mut summary = Value::Null;
    let mut echo = String::new();
    let mut secret_responses = 0;
    let mut asks = 0;

    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line?)?;
        let method = message["method"].as_str().unwrap_or_default();
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("methods")?;
        writeln!(log, "{method}")?;
        if method == "notifications/cancelled" {
            std::fs::write("cancelled", "yes")?;
            continue;
        }
        let Some(id) = message.get("id") else {
            continue;
        };
        if method == "server/discover" && matches!(mode.as_str(), "legacy" | "ask-legacy") {
            writeln!(
                output,
                "{}",
                json!({"jsonrpc":"2.0", "id": id, "error":{"code":-32601,"message":"not supported"}})
            )?;
            output.flush()?;
            continue;
        }
        if (method == "server/discover" && Path::new("hang-start").exists())
            || (method == "prompts/list" && Path::new("hang-prompts").exists())
            || (method == "resources/list" && Path::new("hang-resources").exists())
        {
            continue;
        }
        if mode.starts_with("ask") && method.is_empty() {
            let response = &message["result"];
            form_secret(response, &mut echo, &mut secret_responses)?;
            std::fs::write(
                "legacy-action",
                response["action"].as_str().unwrap_or_default(),
            )?;
            if Path::new("legacy-repeat").exists() && response["action"] == "accept" {
                asks += 1;
                legacy_form(&mut output, asks)?;
                continue;
            }
            for id in pending.drain(..) {
                writeln!(
                    output,
                    "{}",
                    json!({"jsonrpc":"2.0","id":id,"result":form_output(form_summary(response), &echo)})
                )?;
            }
            output.flush()?;
            continue;
        }
        if method == "initialize" || method == "server/discover" {
            std::fs::write(
                "client-capabilities",
                serde_json::to_vec(&message["params"])?,
            )?;
        }
        if mode.starts_with("ask") && method == "tools/call" {
            if mode == "ask-legacy" {
                pending.push(id.clone());
                if message["params"]["arguments"]["wait"] == true {
                    continue;
                }
                asks += 1;
                legacy_form(&mut output, asks)?;
                continue;
            }
            if let Some(responses) = message["params"]["inputResponses"].as_object() {
                let response = responses.values().next().unwrap_or(&Value::Null);
                form_secret(response, &mut echo, &mut secret_responses)?;
                summary = form_summary(response);
                if Path::new("expected.json").exists() {
                    let expected: Value = serde_json::from_slice(&std::fs::read("expected.json")?)?;
                    summary["structuredContent"]["matches_expected"] =
                        json!(response["content"] == expected);
                }
                std::fs::write("response-shape", serde_json::to_vec(&summary)?)?;
            }
            let state = message["params"]["requestState"]
                .as_str()
                .unwrap_or_default();
            let result = if summary.is_null() {
                let params: Value = serde_json::from_slice(&std::fs::read("form.json")?)?;
                json!({"resultType":"input_required","requestState":"answered","inputRequests":{"form":{"method":"elicitation/create","params":params}}})
            } else if state == "answered" && Path::new("poll").exists() {
                json!({"resultType":"input_required","requestState":"polled"})
            } else if state == "polled" && Path::new("echo-form").exists() {
                json!({"resultType":"input_required","requestState":"echoed","inputRequests":{"form":{"method":"elicitation/create","params":{"mode":"form","message":echo,"requestedSchema":{"type":"object","properties":{"again":{"type":"string"}}}}}}})
            } else {
                form_output(summary.clone(), &echo)
            };
            writeln!(
                output,
                "{}",
                json!({"jsonrpc":"2.0","id":id,"result":result})
            )?;
            output.flush()?;
            continue;
        }
        if mode == "ask-legacy"
            && method == "tools/list"
            && Path::new("ask-on-list").exists()
            && message["params"]["cursor"].is_null()
        {
            asks += 1;
            legacy_form(&mut output, asks)?;
        }
        let tools = || {
            if mode == "key" {
                return json!({"tools":[{"name":"has-key","description":"Whether the key reached this server","inputSchema":{"type":"object"}}],"resultType":"complete"});
            }
            if Path::new("gone").exists() {
                return json!({"tools":[],"resultType":"complete","ttlMs":5000,"cacheScope":"private"});
            }
            let description = if Path::new("large").exists() {
                "text".repeat(20_000)
            } else if Path::new("changed").exists() {
                "Changed".into()
            } else {
                "First".into()
            };
            let named = |name: &str| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":{"z":{"type":"string"},"a":{"type":"integer"}}}});
            if message["params"]["cursor"].as_str() == Some("second") {
                json!({"tools":[named(if mode == "collision" { "z_echo" } else if mode == "separator" { "echo" } else { "a-first" }),named("slow")],"ttlMs":5000,"cacheScope":"private","resultType":"complete"})
            } else {
                json!({"tools":[named(if mode == "separator" { "z__echo" } else { "z.echo" })],"nextCursor":"second","ttlMs":8000,"cacheScope":"private","resultType":"complete"})
            }
        };
        if (method == "prompts/list" && Path::new("fail-prompts").exists())
            || (method == "resources/list" && Path::new("fail-resources").exists())
            || (method == "tools/list" && mode == "metadata-only")
            || matches!(method, "prompts/get" | "resources/read")
                && Path::new("read-fails").exists()
        {
            writeln!(
                output,
                "{}",
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"CANARY server detail"}})
            )?;
            output.flush()?;
            continue;
        }
        if mode == "key"
            && method == "tools/list"
            && let Ok(path) = std::fs::read_to_string("block-key-store")
        {
            std::fs::create_dir_all(path.trim())?;
        }
        let capabilities = if mode == "tools-only" {
            json!({"tools":{}})
        } else if matches!(mode.as_str(), "metadata-only" | "undeclared-tools") {
            json!({"prompts":{},"resources":{}})
        } else {
            json!({"tools":{},"prompts":{},"resources":{}})
        };
        let text = |placeholder: &str| {
            if mode == "key" {
                std::env::var(&key_name).unwrap_or_default()
            } else {
                placeholder.to_owned()
            }
        };
        let result = match method {
            "server/discover" => {
                if Path::new("delay").exists() {
                    std::thread::sleep(Duration::from_millis(200));
                }
                json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":capabilities,"ttlMs":0,"cacheScope":"private"})
            }
            "initialize" => {
                json!({"protocolVersion":"2025-11-25","capabilities":capabilities,"serverInfo":{"name":"not-the-configured-name","version":"1"}})
            }
            "tools/list" => tools(),
            "prompts/list" => {
                if Path::new("gone-prompts").exists() {
                    json!({"prompts":[],"ttlMs":10000,"cacheScope":"private","resultType":"complete"})
                } else {
                    let second = message["params"]["cursor"].as_str() == Some("second");
                    let prompt = if second {
                        json!({"name":"review-required","description":"Required placeholder review","arguments":[{"name":"path","required":true,"description":"The placeholder file"}]})
                    } else {
                        json!({"name":"review.code","description":if Path::new("prompts-changed").exists() { "Changed prompt" } else { "Review placeholder code" },"arguments":[{"name":"language","description":"The placeholder language"}]})
                    };
                    let mut page = json!({"prompts":[prompt],"ttlMs":10000,"cacheScope":"private","resultType":"complete"});
                    if !second || Path::new("repeat-prompts").exists() {
                        page["nextCursor"] = json!("second");
                    }
                    page
                }
            }
            "resources/list" => {
                if Path::new("gone-resources").exists() {
                    json!({"resources":[],"ttlMs":10000,"cacheScope":"private","resultType":"complete"})
                } else {
                    let second = message["params"]["cursor"].as_str() == Some("second");
                    let resource = if second {
                        json!({"uri":"test:///blob","name":"binary","description":"Placeholder binary"})
                    } else {
                        json!({"uri":"test:///notes","name":"notes","title":"Notes","description":if Path::new("resources-changed").exists() { "Changed resource" } else { "Placeholder notes" }})
                    };
                    let mut page = json!({"resources":[resource],"ttlMs":10000,"cacheScope":"private","resultType":"complete"});
                    if !second || Path::new("repeat-resources").exists() {
                        page["nextCursor"] = json!("second");
                    }
                    page
                }
            }
            "prompts/get" => {
                if message["params"]["name"] != "review.code" {
                    return Err("unexpected original prompt name".into());
                }
                let second = if Path::new("prompt-binary").exists() {
                    json!({"type":"image","mimeType":"image/png","data":"eA=="})
                } else {
                    json!({"type":"resource","resource":{"uri":"test:///prompt","text":text("Placeholder assistant text.")}})
                };
                json!({"messages":[{"role":"user","content":{"type":"text","text":text("Placeholder prompt text.")}},{"role":"assistant","content":second}],"resultType":"complete"})
            }
            "resources/read" => {
                let uri = &message["params"]["uri"];
                if uri == "test:///blob" {
                    json!({"contents":[{"uri":uri,"blob":"eA=="}],"resultType":"complete","ttlMs":0,"cacheScope":"private"})
                } else {
                    if uri != "test:///notes" {
                        return Err("unexpected original resource URI".into());
                    }
                    let mut parts =
                        vec![json!({"uri":uri,"text":text("Placeholder resource text.")})];
                    if Path::new("resource-mixed").exists() {
                        parts.push(json!({"uri":uri,"blob":"eA=="}));
                    }
                    json!({"contents":parts,"resultType":"complete","ttlMs":0,"cacheScope":"private"})
                }
            }
            "tools/call" if message["params"]["arguments"]["exit"] == true => return Ok(()),
            "tools/call" if message["params"]["name"] == "slow" => {
                std::fs::write("slow-started", "yes")?;
                continue;
            }
            "tools/call" if message["params"]["arguments"]["invalid"] == true => {
                writeln!(
                    output,
                    "{}",
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"invalid arguments"}})
                )?;
                output.flush()?;
                continue;
            }
            "tools/call" if mode == "key" => {
                if message["params"]["arguments"]["echo-key"] == true {
                    let key = std::env::var(&key_name)?;
                    json!({"content":[
                        {"type":"text","text":format!("key {key}")},
                        {"type":"image","mimeType":"image/png","data":"eA=="}
                    ],"structuredContent":{"key":key,"nested":[key]},"isError":message["params"]["arguments"]["fail"] == true})
                } else {
                    json!({"content":[{"type":"text","text":std::env::var_os(&key_name).is_some_and(|value| !value.is_empty()).to_string()}],"isError":false})
                }
            }
            "tools/call" if mode == "text" => {
                json!({"content":[{"type":"text","text":"MCP result"}],"isError":false})
            }
            "tools/call" => json!({"content":[
                {"type":"text","text":message["params"]["name"]},
                {"type":"image","mimeType":"image/png","data":"eA=="},
                {"type":"audio","mimeType":"audio/wav","data":"eQ=="},
                {"type":"resource","resource":{"uri":"test:///text","text":"embedded"}},
                {"type":"resource","resource":{"uri":"test:///blob","mimeType":"application/octet-stream","blob":"eg=="}}
            ],"structuredContent":{"job":std::env::var("WISP_JOB")?,"cwd":std::env::current_dir()?.file_name().and_then(|name| name.to_str())},"isError":message["params"]["arguments"]["fail"] == true}),
            _ => json!({}),
        };
        writeln!(
            output,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"result":result})
        )?;
        output.flush()?;
    }
    if let Some(child) = child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

fn form_summary(response: &Value) -> Value {
    let fields = response["content"]
        .as_object()
        .map(|fields| {
            fields
                .iter()
                .map(|(name, value)| {
                    let kind = match value {
                        Value::String(_) => "string",
                        Value::Number(_) => "number",
                        Value::Bool(_) => "boolean",
                        Value::Array(_) => "array",
                        _ => "other",
                    };
                    (name.clone(), json!(kind))
                })
                .collect::<serde_json::Map<_, _>>()
        })
        .unwrap_or_default();
    json!({"content":[],"structuredContent":{"action":response["action"],"fields":fields}})
}

fn legacy_form(output: &mut impl Write, id: usize) -> Result<(), Box<dyn std::error::Error>> {
    let params: Value = serde_json::from_slice(&std::fs::read("form.json")?)?;
    writeln!(
        output,
        "{}",
        json!({"jsonrpc":"2.0","id":format!("ask-{id}"),"method":"elicitation/create","params":params})
    )?;
    output.flush()?;
    Ok(())
}

fn form_secret(
    response: &Value,
    echo: &mut String,
    count: &mut usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if response["content"]["admin_token"].as_str().is_some() {
        *count += 1;
        *echo = response["content"].to_string();
        std::fs::write("secret-shape", serde_json::to_vec(&form_summary(response))?)?;
    }
    std::fs::write("secret-response-count", count.to_string())?;
    Ok(())
}

fn form_output(mut summary: Value, echo: &str) -> Value {
    if !echo.is_empty() {
        summary["structuredContent"]["echo"] = json!(echo);
        summary["content"] = json!([{"type":"text","text":echo},{"type":"resource","resource":{"uri":"test:///echo","text":echo}}]);
        if let Ok(kind) = std::fs::read_to_string("echo-attachment") {
            let token = serde_json::from_str::<Value>(echo)
                .ok()
                .and_then(|value| value["admin_token"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let mime = if kind == "mime" {
                format!("application/x-{token}")
            } else {
                "application/pdf".into()
            };
            let blob = base64::engine::general_purpose::STANDARD.encode(format!(
                "%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\n%{echo}\n%%EOF\n"
            ));
            summary["content"] = json!([{"type":"text","text":echo},{"type":"resource","resource":{"uri":"test:///echo","text":echo}},{"type":"resource","resource":{"uri":"test:///receipt","mimeType":mime,"blob":blob}}]);
        }
    }
    summary
}
