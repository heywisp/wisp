use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;
use std::{ffi::OsString, path::PathBuf};

use regex::Regex;
use wisp_protocol::{TextSpan, TodoItem};

use crate::session::{self, REDACTED};

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::email_regression_cases;

pub(crate) fn contains_secret_or_identity(text: &str, home: Option<&Path>) -> bool {
    contains_secret_or_identity_with(text, home, &session::held())
}

pub(crate) fn contains_secret_or_identity_with(
    text: &str,
    home: Option<&Path>,
    held: &session::Redactions,
) -> bool {
    held.has_secret_check_match(text)
        || any_token(text, |token| is_key_token(token) || is_account_id(token))
        || wisp_protocol::has_url_password(text)
        || has_email(text)
        || has_home_path(text, home)
}

pub(crate) fn scrub_field(raw: &str, home: Option<&Path>, held: &session::Redactions) -> String {
    if raw == REDACTED || contains_secret_or_identity_with(raw, home, held) {
        return REDACTED.to_owned();
    }
    let scrubbed = held.scrub(raw);
    if scrubbed != raw
        && (contains_secret_or_identity_with(&scrubbed, home, held)
            || held.scrub(&scrubbed) != scrubbed)
    {
        REDACTED.to_owned()
    } else {
        scrubbed
    }
}

pub(crate) fn valid_checklist_spans(text: &str, spans: &[TextSpan]) -> bool {
    let mut cursor = 0;
    spans.iter().all(|span| {
        let valid = span.start >= cursor
            && span.start <= span.end
            && span.end <= text.len()
            && text.is_char_boundary(span.start)
            && text.is_char_boundary(span.end);
        cursor = span.end;
        valid
    })
}

pub(crate) fn suppress_checklist_brief(text: &mut String, spans: &mut Vec<TextSpan>) {
    *text = REDACTED.to_owned();
    *spans = vec![TextSpan {
        start: 0,
        end: REDACTED.len(),
    }];
}

pub(crate) fn scrub_checklist_spans(
    text: &mut String,
    spans: &mut Vec<TextSpan>,
    home: Option<&Path>,
    held: &session::Redactions,
) -> bool {
    if spans.is_empty() {
        return false;
    }
    if !valid_checklist_spans(text, spans) {
        suppress_checklist_brief(text, spans);
        return true;
    }
    let mut out = String::with_capacity(text.len());
    let mut updated = Vec::with_capacity(spans.len());
    let (mut cursor, mut changed) = (0, false);
    for span in spans.iter() {
        let (Some(prose), Some(raw)) =
            (text.get(cursor..span.start), text.get(span.start..span.end))
        else {
            suppress_checklist_brief(text, spans);
            return true;
        };
        out.push_str(prose);
        let start = out.len();
        let scrubbed = scrub_field(raw, home, held);
        changed |= scrubbed != raw;
        out.push_str(&scrubbed);
        updated.push(TextSpan {
            start,
            end: out.len(),
        });
        cursor = span.end;
    }
    let Some(tail) = text.get(cursor..) else {
        suppress_checklist_brief(text, spans);
        return true;
    };
    out.push_str(tail);
    if changed {
        *text = out;
        *spans = updated;
    }
    changed
}

pub(crate) fn scrub_todo_fields(
    text: &mut String,
    section: &mut Option<String>,
    home: Option<&Path>,
    held: &session::Redactions,
) {
    *text = scrub_field(text, home, held);
    if let Some(section) = section {
        *section = scrub_field(section, home, held);
    }
}

pub(crate) fn scrub_todos(items: &mut [TodoItem], home: Option<&Path>, held: &session::Redactions) {
    for item in items {
        scrub_todo_fields(&mut item.text, &mut item.section, home, held);
    }
}

pub(crate) fn fallback_detection_home(
    home: Option<PathBuf>,
    userprofile: Option<OsString>,
) -> Option<PathBuf> {
    home.filter(|path| home_path(path).is_some()).or_else(|| {
        userprofile
            .map(PathBuf::from)
            .filter(|path| home_path(path).is_some())
    })
}

pub(crate) fn os_detection_home() -> Option<PathBuf> {
    fallback_detection_home(crate::workspace::home(), std::env::var_os("USERPROFILE"))
}

pub(crate) fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_')
}

const EXTRA_KEY_PREFIXES: &[&str] = &["sk-", "hf_", "ops_", "api_org_"];

fn is_key_token(token: &str) -> bool {
    wisp_protocol::has_known_key_prefix(token)
        || EXTRA_KEY_PREFIXES.iter().any(|prefix| {
            token
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.len() >= wisp_protocol::KEY_SHAPED_MIN)
        })
}

const ID_PREFIXES: &[&str] = &["org-", "user-", "proj_"];
const ID_MIN_CHARS: usize = 20;

pub(crate) fn is_account_id(token: &str) -> bool {
    ID_PREFIXES.iter().any(|prefix| {
        token.strip_prefix(prefix).is_some_and(|id| {
            id.len() >= ID_MIN_CHARS && id.bytes().all(|b| b.is_ascii_alphanumeric())
        })
    })
}

struct TokenSpan<'a> {
    start: usize,
    token: &'a str,
    candidate: &'a str,
}

impl TokenSpan<'_> {
    fn matching_start(&self, matches: impl Fn(&str) -> bool) -> Option<usize> {
        let stripped = self.token.len() - self.candidate.len();
        if matches(self.candidate) {
            Some(self.start + stripped)
        } else {
            (stripped != 0 && matches(self.token)).then_some(self.start)
        }
    }
}

fn escaped_boundary(before: &[u8], token: &[u8]) -> bool {
    before.ends_with(b"\\") && matches!(token.first(), Some(b'n' | b'r' | b't'))
}

fn token_spans(text: &str) -> impl Iterator<Item = TokenSpan<'_>> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let (before, tail) = rest.split_at(rest.find(is_token_char)?);
        let start = text.len() - tail.len();
        let end = tail.find(|c| !is_token_char(c)).unwrap_or(tail.len());
        let (token, after) = tail.split_at(end);
        rest = after;
        let escaped = escaped_boundary(before.as_bytes(), token.as_bytes());
        let candidate = token.get(usize::from(escaped)..)?;
        Some(TokenSpan {
            start,
            token,
            candidate,
        })
    })
}

pub(crate) fn redacting_tokens(text: &str, redacts: impl Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for span in token_spans(text) {
        let Some(start) = span.matching_start(&redacts) else {
            continue;
        };
        out.push_str(text.get(copied..start).unwrap_or_default());
        out.push_str(REDACTED);
        copied = span.start + span.token.len();
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    out
}

fn any_token(text: &str, matches: impl Fn(&str) -> bool) -> bool {
    token_spans(text).any(|span| span.matching_start(&matches).is_some())
}

struct EmailPatterns {
    delimiter: Regex,
    local_suffix: Regex,
    domain_prefix: Regex,
}

static EMAIL: LazyLock<Option<EmailPatterns>> = LazyLock::new(|| {
    Some(EmailPatterns {
        delimiter: Regex::new(r"@|%40").ok()?,
        local_suffix: Regex::new(r"[\w.!#$%&'*+/=?^`{|}~-]+\z").ok()?,
        domain_prefix: Regex::new(
            r"\A[\p{L}\p{N}]\p{M}*(?:[\p{L}\p{N}\p{M}-]*[\p{L}\p{N}]\p{M}*)?(?:(?:\.|%2[eE])[\p{L}\p{N}]\p{M}*(?:[\p{L}\p{N}\p{M}-]*[\p{L}\p{N}]\p{M}*)?)+",
        )
        .ok()?,
    })
});

impl EmailPatterns {
    fn candidates<'a>(
        &'a self,
        text: &'a str,
        encoded: bool,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        let mut delimiters = self
            .delimiter
            .find_iter(text)
            .filter(move |delimiter| encoded || delimiter.as_str() == "@")
            .peekable();
        let mut from = 0;
        let mut prior_domain_start = None;
        let mut in_url_path = false;
        std::iter::from_fn(move || {
            while let Some(delimiter) = delimiters.next() {
                let local_floor = prior_domain_start.take().unwrap_or(from);
                let before = text.get(local_floor..delimiter.start())?;
                from = delimiter.end();
                let Some(local) = self.local_suffix.find(before) else {
                    in_url_path &= before.is_empty();
                    continue;
                };
                let local_text = local.as_str();
                in_url_path = (in_url_path && local.start() == 0) || local_text.contains("//");
                if encoded && delimiter.as_str() == "@" {
                    continue;
                }
                let name = if in_url_path {
                    local_text.rsplit('/').next()?
                } else {
                    local_text
                };
                let name = name.trim_start_matches(['*', '`', '\'', '~', '.', '/']);
                if name.ends_with(['/', '.']) || !name.chars().any(char::is_alphanumeric) {
                    continue;
                }
                let limit = delimiters.peek().map_or(text.len(), |next| next.start());
                if let Some(domain) = self.domain_prefix.find(text.get(from..limit)?)
                    && let Some(end) = valid_domain_end(domain.as_str())
                {
                    prior_domain_start = Some(from);
                    from = from.checked_add(end)?;
                    return Some(delimiter.start().checked_sub(name.len())?..from);
                }
            }
            None
        })
    }

    fn ranges<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        let mut raw = self.candidates(text, false).peekable();
        let mut encoded = self.candidates(text, true).peekable();
        let mut ordered = std::iter::from_fn(move || {
            if let Some(first) = raw.peek()
                && encoded
                    .peek()
                    .is_none_or(|other| first.start <= other.start)
            {
                raw.next()
            } else {
                encoded.next()
            }
        })
        .peekable();
        std::iter::from_fn(move || {
            let mut range = ordered.next()?;
            while let Some(next) = ordered.next_if(|next| next.start <= range.end) {
                range.end = range.end.max(next.end);
            }
            Some(range)
        })
    }
}

fn valid_domain_end(domain: &str) -> Option<usize> {
    let mut separators = domain.match_indices(['.', '%']).peekable();
    let mut valid_end = None;
    while let Some((at, separator)) = separators.next() {
        let start = at.checked_add(if separator == "." { 1 } else { "%2e".len() })?;
        let end = separators.peek().map_or(domain.len(), |&(next, _)| next);
        if domain.get(start..end).is_some_and(valid_tld_label) {
            valid_end = Some(end);
        }
    }
    valid_end
}

fn valid_tld_label(tld: &str) -> bool {
    static BASE_LETTER: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"[\p{Alphabetic}--\p{M}]").ok());
    if let Some(payload) = strip_prefix_ignore_ascii_case(tld, "xn--") {
        return payload.len() >= 2
            && payload
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    }
    !tld.contains(|c: char| c.is_numeric() || c == '-')
        && BASE_LETTER
            .as_ref()
            .is_none_or(|letter| letter.find_iter(tld).nth(1).is_some())
}

fn has_email(text: &str) -> bool {
    EMAIL
        .as_ref()
        .is_none_or(|email| email.ranges(text).next().is_some())
}

pub(crate) fn redact_emails(text: &str) -> String {
    let Some(email) = EMAIL.as_ref() else {
        return REDACTED.to_owned();
    };
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for range in email.ranges(text) {
        out.push_str(text.get(copied..range.start).unwrap_or_default());
        out.push_str(REDACTED);
        copied = range.end;
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    out
}

fn is_path_start(text: &[u8], at: usize) -> bool {
    let Some(before) = text.get(..at) else {
        return false;
    };
    if path_delimiter(before) {
        return true;
    }
    let start = before
        .iter()
        .rposition(|&byte| !is_token_char(char::from(byte)))
        .map_or(0, |at| at + 1);
    let start = start + usize::from(escaped_boundary(&before[..start], &before[start..]));
    let (prefix, token) = before.split_at(start);
    path_delimiter(prefix)
        && token
            .strip_prefix(b"--")
            .or_else(|| token.strip_prefix(b"-"))
            .is_some_and(|body| body.first().is_some_and(u8::is_ascii_alphabetic))
}

fn path_delimiter(before: &[u8]) -> bool {
    let Some((&previous, prefix)) = before.split_last() else {
        return true;
    };
    previous.is_ascii_whitespace()
        || b"`([{ \"'=:<,;|*>".contains(&previous)
        || (previous == b'@' && prefix.last().is_none_or(u8::is_ascii_whitespace))
        || escaped_boundary(prefix, &[previous])
        || ["“", "‘", "file://", "file:///"]
            .iter()
            .any(|prefix| before.ends_with(prefix.as_bytes()))
}

fn starts_with_user_segment(bytes: &[u8]) -> bool {
    bytes
        .first()
        .is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_home_boundary(after: &str) -> bool {
    let identifier = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-');
    if let Some(suffix) = after.strip_prefix('.') {
        suffix.chars().next().is_none_or(|c| !identifier(c))
    } else {
        after.chars().next().is_none_or(|c| !identifier(c))
    }
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())?
        .eq_ignore_ascii_case(prefix)
        .then_some(text.get(prefix.len()..)?)
}

fn matches_wsl_unc_home(rest: &str, unix_home: Option<&str>) -> bool {
    let Some(share) = rest.strip_prefix(r"\\") else {
        return false;
    };
    let (share, sep) = share
        .strip_prefix(r"\\")
        .map_or((share, r"\"), |tail| (tail, r"\\"));
    let Some(tail) = ["wsl.localhost", "wsl$"]
        .iter()
        .find_map(|server| strip_prefix_ignore_ascii_case(share, server)?.strip_prefix(sep))
    else {
        return false;
    };
    let distro_len = tail
        .bytes()
        .take_while(|&b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        .count();
    let Some(rest) = tail.get(distro_len..).filter(|_| distro_len > 0) else {
        return false;
    };
    if rest
        .strip_prefix(sep)
        .and_then(|rest| rest.strip_prefix("home"))
        .and_then(|rest| rest.strip_prefix(sep))
        .is_some_and(|user| starts_with_user_segment(user.as_bytes()))
    {
        return true;
    }
    unix_home
        .and_then(|home| home.strip_prefix('/'))
        .and_then(|home| {
            home.split('/')
                .filter(|segment| !segment.is_empty())
                .try_fold(rest, |rest, segment| {
                    rest.strip_prefix(sep)?.strip_prefix(segment)
                })
        })
        .is_some_and(is_home_boundary)
}

fn drive_tail(rest: &str) -> Option<&str> {
    match rest.as_bytes() {
        [drive, b':', ..] if drive.is_ascii_alphabetic() => rest.get(2..),
        _ => None,
    }
}

fn matches_standard_home(rest: &str) -> bool {
    let has_user = |user: &str| starts_with_user_segment(user.as_bytes());
    if let Some(user) = rest
        .strip_prefix("/home/")
        .or_else(|| rest.strip_prefix("/Users/"))
    {
        return has_user(user);
    }
    let users_path = |tail, sep| {
        strip_prefix_ignore_ascii_case(tail, "Users")
            .and_then(|tail| tail.strip_prefix(sep))
            .is_some_and(has_user)
    };
    if let Some(mount) = rest.strip_prefix("/mnt/")
        && let [drive, b'/', ..] = mount.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        return mount.get(2..).is_some_and(|tail| users_path(tail, "/"));
    }
    if let Some(tail) = drive_tail(rest) {
        return [r"\\", r"\", "/"].iter().any(|&sep| {
            tail.strip_prefix(sep)
                .is_some_and(|tail| users_path(tail, sep))
        });
    }
    let Some(user) = rest.strip_prefix('~') else {
        return false;
    };
    user.as_bytes()
        .first()
        .is_some_and(|&b| b.is_ascii_alphabetic() || b == b'_')
        && user
            .bytes()
            .find(|&b| !b.is_ascii_alphanumeric() && !b"._-".contains(&b))
            == Some(b'/')
}

fn home_path(path: &Path) -> Option<&str> {
    let path = path.to_str()?;
    if drive_tail(path).is_some_and(|tail| tail.starts_with(['/', '\\'])) || path.starts_with(r"\\")
    {
        let trimmed = path.trim_end_matches(['/', '\\']);
        return (!trimmed.is_empty() && drive_tail(trimmed) != Some("")).then_some(trimmed);
    }
    let trimmed = path.trim_end_matches('/');
    (trimmed.starts_with('/')).then_some(trimmed)
}

#[derive(Default)]
struct HomeNeedles<'a> {
    exact: Option<&'a str>,
    windows: Option<[String; 3]>,
}

fn runtime_home_needles(home: Option<&Path>) -> HomeNeedles<'_> {
    let Some(path) = home.and_then(home_path) else {
        return HomeNeedles::default();
    };
    let windows = (!path.starts_with('/')).then(|| {
        let backslash = path.replace('/', r"\");
        let slash = path.replace('\\', "/");
        let escaped = backslash.replace('\\', r"\\");
        [backslash, slash, escaped]
    });
    HomeNeedles {
        exact: Some(path),
        windows,
    }
}

fn matches_runtime_home(rest: &str, needles: &HomeNeedles<'_>) -> bool {
    let Some(exact) = needles.exact else {
        return false;
    };
    let ignore_case = needles.windows.is_some();
    let matches = |needle| {
        (if ignore_case {
            strip_prefix_ignore_ascii_case(rest, needle)
        } else {
            rest.strip_prefix(needle)
        })
        .is_some_and(is_home_boundary)
    };
    matches(exact)
        || needles
            .windows
            .iter()
            .flatten()
            .any(|needle| matches(needle.as_str()))
}

fn has_home_path(text: &str, home: Option<&Path>) -> bool {
    let needles = runtime_home_needles(home);
    let unix_home = needles.exact.filter(|path| path.starts_with('/'));
    text.char_indices().any(|(at, _)| {
        text.get(at..).is_some_and(|rest| {
            matches!(rest.as_bytes(), [b'/' | b'\\' | b'~', ..] | [_, b':', ..])
                && is_path_start(text.as_bytes(), at)
                && (matches_standard_home(rest)
                    || matches_wsl_unc_home(rest, unix_home)
                    || matches_runtime_home(rest, &needles))
        })
    })
}
