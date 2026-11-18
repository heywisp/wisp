use std::ops::Range;

use crate::{has_known_key_prefix, masked, settings::secret_word};

fn key_shaped(value: &str) -> bool {
    has_known_key_prefix(value.trim_matches(['\'', '"']))
}

fn secret_name(name: &str) -> bool {
    let name = name.trim().trim_start_matches('-');
    secret_word(name).is_some()
        || name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("proxy-authorization")
}

fn secret_header(value: &str, is_header: bool) -> bool {
    let mut words = value.split_whitespace();
    words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("bearer"))
        && words.next().is_some()
        || is_header
            && value
                .split_once(':')
                .is_some_and(|(name, _)| secret_name(name))
}

fn secrets(word: &str, previous: Option<&str>) -> Vec<Range<usize>> {
    if word.is_empty() {
        return Vec::new();
    }
    let header = previous.is_some_and(|flag| matches!(flag, "-H" | "--header"));
    if key_shaped(word)
        || secret_header(word, header)
        || previous.is_some_and(|flag| flag.starts_with('-') && secret_name(flag))
        || word
            .strip_prefix("-H")
            .is_some_and(|value| secret_header(value, true))
    {
        return std::iter::once(0..word.len()).collect();
    }
    let mut ranges = Vec::new();
    if let Some(start) = word.find("://").map(|start| start + 3) {
        let authority = &word[start..];
        let end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
        if let Some(at) = authority[..end].rfind('@')
            && let Some(colon) = authority[..at].find(':')
            && colon + 1 < at
        {
            ranges.push(start + colon + 1..start + at);
        }
    }
    if let Some((head, query)) = word.split_once('?') {
        let query = query.split('#').next().unwrap_or_default();
        let mut offset = head.len() + 1;
        for pair in query.split('&') {
            if let Some((raw_name, value)) = pair.split_once('=')
                && !value.is_empty()
                && form_urlencoded::parse(pair.as_bytes())
                    .next()
                    .is_some_and(|(name, _)| name == "code" || secret_name(&name))
            {
                ranges.push(offset + raw_name.len() + 1..offset + pair.len());
            }
            offset += pair.len() + 1;
        }
    }
    if let Some((name, value)) = word.split_once('=')
        && !name.contains("://")
        && !value.is_empty()
        && (key_shaped(value) || secret_name(name) || secret_header(value, name == "--header"))
    {
        return std::iter::once(name.len() + 1..word.len()).collect();
    }
    ranges
}

pub fn command_secret<'a>(word: &'a str, previous: Option<&str>) -> Option<&'a str> {
    word.get(secrets(word, previous).into_iter().next()?)
}

pub fn masked_command(text: &str) -> Option<String> {
    if text
        .split_whitespace()
        .any(|word| command_secret(word.trim_matches(['\'', '"']), None).is_some_and(key_shaped))
    {
        return None;
    }
    let mut result = String::with_capacity(text.len());
    let mut previous = None;
    let mut copied = 0;
    for range in words(text) {
        let raw = &text[range.clone()];
        let (word, locations) = decoded(raw);
        if let Some(parsed) = shlex::split(raw)
            && !parsed.is_empty()
            && (parsed.len() != 1 || parsed[0] != word)
        {
            return None;
        }
        result.push_str(&text[copied..range.start]);
        let mut at = 0;
        for secret in secrets(&word, previous.as_deref()) {
            let value = &word[secret.clone()];
            if key_shaped(value) {
                return None;
            }
            let start = locations.get(secret.start)?.start;
            let end = locations.get(secret.end.checked_sub(1)?)?.end;
            result.push_str(&raw[at..start]);
            result.push_str(&masked(value, 20, ""));
            at = end;
        }
        result.push_str(&raw[at..]);
        copied = range.end;
        previous = Some(word);
    }
    result.push_str(&text[copied..]);
    Some(result)
}

fn decoded(raw: &str) -> (String, Vec<Range<usize>>) {
    let mut word = String::new();
    let mut locations = Vec::new();
    let mut quote = None;
    let mut chars = raw.char_indices().peekable();
    while let Some((start, mut ch)) = chars.next() {
        let mut end = start + ch.len_utf8();
        if ch == '\\' && quote != Some('\'') {
            if let Some(&(next, value)) = chars.peek()
                && (quote != Some('"') || matches!(value, '$' | '`' | '"' | '\\' | '\n'))
            {
                chars.next();
                if value == '\n' {
                    continue;
                }
                ch = value;
                end = next + ch.len_utf8();
            }
        } else if quote == Some(ch) {
            quote = None;
            continue;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
            continue;
        }
        word.push(ch);
        locations.extend(std::iter::repeat_n(start..end, ch.len_utf8()));
    }
    (word, locations)
}

fn words(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut words = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if matches!(bytes[at], b' ' | b'\t' | b'\n') {
            at += 1;
            continue;
        }
        let start = at;
        let mut quote = None;
        while at < bytes.len() {
            let byte = bytes[at];
            if byte == b'\\' && quote != Some(b'\'') {
                at = (at + 2).min(bytes.len());
                continue;
            }
            if quote == Some(byte) {
                quote = None;
            } else if quote.is_none() {
                if matches!(byte, b' ' | b'\t' | b'\n') {
                    break;
                }
                if matches!(byte, b'\'' | b'"') {
                    quote = Some(byte);
                }
            }
            at += 1;
        }
        words.push(start..at);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_flags_assignments_and_known_prefixes_share_one_guard() {
        for (word, previous) in [
            ("CANARY_OPAQUE_VALUE_0011", Some("--token")),
            ("API_KEY=CANARY_OPAQUE_VALUE_0011", None),
            ("sk-ant-api03-CANARY_VALUE_0011", None),
            ("ARG=sk-ant-api03-CANARY_VALUE_0011", None),
        ] {
            assert!(command_secret(word, previous).is_some());
        }
        for prefix in crate::KEY_PREFIXES.iter() {
            let value = format!("{prefix}CANARY{}", "x".repeat(20));
            assert!(command_secret(&value, None).is_some());
            assert!(command_secret(&format!("ARG={value}"), None).is_some());
        }
        assert!(command_secret("--stdio", None).is_none());
        assert!(command_secret("~/My Documents", Some("--folder")).is_none());
    }

    #[test]
    fn masking_keeps_nonsecret_quotes_and_masks_a_quoted_secret_as_one_value() {
        let secret = "CANARY opaque value 0011";
        let line = format!("/mcp add notes server --folder \"~/My Documents\" --token '{secret}'");
        let shown = masked_command(&line).unwrap();
        assert_eq!(
            shown,
            format!(
                "/mcp add notes server --folder \"~/My Documents\" --token '{}'",
                masked(secret, 20, "")
            )
        );
        assert!(!shown.contains(secret));
    }

    #[test]
    fn an_assignment_masks_only_its_value_and_unfinished_quotes_stay_safe() {
        let value = "CANARY_OPAQUE_VALUE_0011";
        assert_eq!(
            masked_command(&format!("/mcp add notes server API_KEY={value}")),
            Some(format!(
                "/mcp add notes server API_KEY={}",
                masked(value, 20, "")
            ))
        );
        let incomplete =
            masked_command(&format!("/mcp add notes server --token '{value}")).unwrap();
        assert!(!incomplete.contains(value));
        assert_eq!(
            masked_command("/mcp add files server \"~/My Documents\""),
            Some("/mcp add files server \"~/My Documents\"".into())
        );
    }

    #[test]
    fn a_known_key_has_no_echo_even_when_quoted_escaped_or_in_a_comment() {
        for value in [
            "sk-ant-api03-CANARY_VALUE_0011",
            "'sk-ant-api03-CANARY_VALUE_0011'",
            "ARG=sk-ant-api03-CANARY_VALUE_0011",
            "ARG='sk-ant-api03-CANARY_VALUE_0011",
            "sk\\-ant-api03-CANARY_VALUE_0011",
            "# sk-ant-api03-CANARY_VALUE_0011",
        ] {
            assert!(masked_command(&format!("/mcp add notes server {value}")).is_none());
        }
    }
    #[test]
    fn authorization_bearer_and_secret_named_headers_are_refused_and_masked_together() {
        let secret = "CANARY_HEADER_VALUE_0011";
        for (flag, value) in [
            ("--header", format!("Authorization: Bearer {secret}")),
            ("-H", format!("x-api-key: {secret}")),
            ("--header", format!("Proxy-Authorization: Basic {secret}")),
            ("", format!("Bearer {secret}")),
            ("", format!("-HAuthorization: Bearer {secret}")),
            ("", format!("--header=Authorization: Bearer {secret}")),
            ("--authorization", secret.to_owned()),
        ] {
            assert!(command_secret(&value, Some(flag)).is_some());
            let shown = masked_command(&format!("/mcp add local server {flag} '{value}'")).unwrap();
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(command_secret("Content-Type: application/json", Some("-H")).is_none());
    }
    #[test]
    fn a_redirect_masks_each_decoded_secret_name_preserves_quotes_and_covers_comments() {
        let code = "CANARY-code-0011";
        for name in [
            "code",
            "%63ode",
            "co%64e",
            "api_key",
            "api%5fkey",
            "authorization",
            "proxy-authorization",
        ] {
            let address = format!(
                "http://127.0.0.1:8123/oauth-callback?state=CANARY-state&{name}={code}&scope=read&{name}={code}#_=_"
            );
            assert_eq!(command_secret(&address, None), Some(code));
            for raw in [
                address.clone(),
                format!("'{address}'"),
                format!("\"{address}\""),
                format!("# {address}"),
                format!("# '{address}'"),
            ] {
                let typed = format!("/mcp sign-in linear {raw}");
                let shown = masked_command(&typed).unwrap();
                assert_eq!(shown, typed.replace(code, &masked(code, 20, "")));
                assert!(!shown.contains(code));
            }
        }
        let password = "CANARY-password-0011";
        let key = "CANARY-key-0011";
        for (user, path, query) in [
            ("user", "mcp", format!("profile=work&api_key={key}")),
            ("user", "mcp", format!("api_key={key}")),
            ("user", "keys/mcp", "profile=work".into()),
            ("x-access-token", "mcp", "profile=work".into()),
            (
                "x-access-token",
                "keys/mcp",
                format!("api%5fkey={key}&%63ode={code}"),
            ),
        ] {
            let address =
                format!("https://{user}:{password}@server.example.invalid/{path}?{query}");
            for raw in [
                address.clone(),
                format!("'{address}'"),
                format!("# {address}"),
            ] {
                let typed = format!("/mcp add local {raw}");
                let shown = masked_command(&typed).unwrap();
                let expected = typed
                    .replace(password, &masked(password, 20, ""))
                    .replace(key, &masked(key, 20, ""))
                    .replace(code, &masked(code, 20, ""));
                assert_eq!(shown, expected);
                for value in [password, key, code] {
                    assert!(!shown.contains(value));
                }
            }
        }
        let value = "https://server.example.invalid/mcp";
        assert_eq!(
            masked_command(&format!("API_KEY={value}")),
            Some(format!("API_KEY={}", masked(value, 20, "")))
        );
        for (word, previous) in [
            ("ghcr.io/acme/keycloak-mcp:latest", None),
            ("~/secrets:/data", Some("-v")),
            ("key: value", None),
        ] {
            assert!(command_secret(word, previous).is_none());
        }
        for typed in [
            "/mcp add files server \"~/My Documents\"",
            "/mcp add files server '--folder' '路径 with spaces'",
            "/mcp add files server path\\ with\\ spaces",
        ] {
            assert_eq!(masked_command(typed).as_deref(), Some(typed));
        }
    }
}
