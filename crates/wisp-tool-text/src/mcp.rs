use wisp_plugin_api::index::Kind;
use wisp_plugin_api::mcp::{Prompt, PromptMessage, Resource};

use crate::code;

pub const UNFINISHED_QUOTE: &str = "the command has an unfinished quote";
pub const MISSING_COMMAND: &str = "use /mcp add <name> <command>";

pub const NAME_EXISTS: &str = "a server with that name already exists";

pub const INPUT_REQUIRED: &str = "the server asked for input Wisp can't show";

pub const MAX_ENTRY_BYTES: usize = 50 * 1024;

pub fn tool_count(count: u32) -> String {
    format!("{count} {}", if count == 1 { "tool" } else { "tools" })
}

pub fn signed_in(name: &str, count: u32) -> String {
    format!("{name} signed in · {}", tool_count(count))
}

pub fn not_signed_in(name: &str, reason: &str) -> String {
    format!("{name} not signed in: {reason}")
}

pub fn gone(server: &str, name: &str) -> String {
    format!("{server} no longer has {name}")
}

pub fn unavailable(name: &str) -> String {
    format!("{name} is no longer available")
}

pub fn removed(server: &str) -> String {
    format!("{server} was removed")
}

pub fn read_failed(server: &str, kind: &str) -> String {
    format!("{server} could not read the {kind}")
}

pub fn skill(prompt: &Prompt, messages: &[PromptMessage]) -> Result<String, String> {
    let mut result = format!("Skill: {}", prompt.name);
    if !prompt.description.is_empty() {
        result.push_str(&format!("\n{}", prompt.description));
    }
    if !prompt.arguments.is_empty() {
        result.push_str("\nArguments:");
        for arg in &prompt.arguments {
            let required = if arg.required { "required" } else { "optional" };
            result.push_str(&format!("\n- {} ({required})", arg.name));
            if !arg.description.is_empty() {
                result.push_str(&format!(": {}", arg.description));
            }
        }
    }
    if prompt.arguments.iter().any(|arg| arg.required) {
        result.push_str("\n\nuse can't pass these arguments, so the skill's text isn't shown.");
    } else {
        for message in messages {
            let text = message
                .text
                .as_deref()
                .ok_or_else(|| not_text(Kind::Skill))?;
            result.push_str(&format!("\n\n{}:\n{text}", message.role));
        }
    }
    bounded(Kind::Skill, result)
}

pub fn document(resource: &Resource, parts: &[Option<String>]) -> Result<String, String> {
    let parts: Option<Vec<_>> = parts.iter().map(Option::as_deref).collect();
    let text = parts.ok_or_else(|| not_text(Kind::Document))?.join("\n\n");
    bounded(
        Kind::Document,
        format!(
            "Document: {}\nURI: {}\n\n{text}",
            resource.title, resource.uri
        ),
    )
}

fn not_text(kind: Kind) -> String {
    format!(
        "this {} has parts that aren't text; use reads only text",
        kind.name()
    )
}

fn bounded(kind: Kind, text: String) -> Result<String, String> {
    if text.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "this {} is {}; the limit is {}",
            kind.name(),
            code::size(text.len()),
            code::size(MAX_ENTRY_BYTES)
        ));
    }
    Ok(text)
}

pub fn not_installed(command: &str) -> String {
    format!("{command} isn't installed")
}

pub fn remove_done(name: &str) -> String {
    format!("{name} removed")
}

pub const KEY_VARIABLE: &str = "keys must name environment variables, never key values";
pub const ADDRESS_KEYS: &str = "a server at an address signs in instead; leave keys out";

pub fn key_reason(server: &str) -> String {
    format!(
        "Your agent asks for the key of the server {server}. Wisp keeps it for that server and never shows it to your agent."
    )
}

pub fn child_key(server: &str) -> String {
    format!(
        "only the main chat can ask for a key; ask your parent to add {server} with manage (action add, kind server, the same name, from and keys)"
    )
}

pub fn child_rekey(server: &str, keys: &[String]) -> String {
    format!(
        "only the main chat can ask for a key; ask your parent to set {} for {server} with manage (action add, kind server, name {server}, keys)",
        keys.join(" and ")
    )
}

pub fn no_key_client(server: &str, keys: &[String]) -> String {
    format!(
        "{server} not added: no one can type its key in this session; set {} in the environment Wisp starts in",
        keys.join(" and ")
    )
}

pub fn no_rekey_client(server: &str, keys: &[String]) -> String {
    format!(
        "{server}'s key can't be typed in this session; set {} in the environment Wisp starts in",
        keys.join(" and ")
    )
}

pub fn rekey_failed(server: &str, keys: &[String], reason: &str) -> String {
    format!(
        "{server} didn't start with the new {}, so the old one stays: {reason}",
        keys.join(" and ")
    )
}

pub fn rekey_done(server: &str, keys: &[String]) -> String {
    format!(
        "{} is set for {server}, which restarted with it",
        keys.join(" and ")
    )
}

pub fn key_not_kept(server: &str) -> String {
    format!("{server} added; its key couldn't be kept, so it will need the key again")
}

pub fn needs_key(server: &str, keys: &[String]) -> String {
    format!(
        "{server} needs {}; set it with manage (action add, kind server, name {server}, keys)",
        keys.join(" and ")
    )
}

pub const YES: &str = "Yes";
pub const NO: &str = "No";

pub fn child_question(server: &str, tool: &str) -> String {
    format!(
        "{server} asked a question that only the main chat can show; ask your parent to call {tool} with the same input"
    )
}

pub fn declined_words(server: &str, words: &str) -> String {
    format!("{server} was told the user declined; the user wrote instead: {words}")
}

pub const MAX_FORM_FIELDS: usize = 10;
pub const MAX_FORM_CHOICES: usize = 100;
pub const MAX_FORM_MESSAGE_CHARS: usize = 1_000;
pub const MAX_FORM_LABEL_CHARS: usize = 100;
pub const MAX_FORM_DESCRIPTION_CHARS: usize = 500;
pub const CALL_CANCELLED: &str = "the MCP call was cancelled";

pub fn call_incomplete(server: &str) -> String {
    format!("{server} could not complete the call")
}

pub fn form_too_large(server: &str) -> String {
    format!("{server}'s form is larger than Wisp shows, so Wisp declined it")
}

pub fn withheld_attachments(server: &str) -> String {
    format!(
        "Wisp withheld {server}'s attachments, since this call received a value typed in the secret box."
    )
}

pub fn sign_in_help(name: &str) -> [String; 2] {
    [
        "If the browser ends on a page that can't load, copy that page's address and type:".into(),
        format!("/mcp sign-in {name} <address>"),
    ]
}
