use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};

const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', ')'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    pub imports: Vec<Import>,
    pub only_imports: bool,
}

fn code_ranges(text: &str) -> Vec<Range<usize>> {
    Parser::new_ext(text, Options::empty())
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Code(_) | Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => Some(range),
            _ => None,
        })
        .collect()
}

fn in_code(code: &[Range<usize>], at: usize) -> bool {
    code.iter().any(|r| r.contains(&at))
}

fn parse_at(line: &str, start: usize) -> Option<(String, usize)> {
    let rest = &line[start + 1..];
    if rest.starts_with(['"', '\'']) {
        return None;
    }
    let mut chars: Vec<(char, bool)> = Vec::new();
    let mut iter = rest.char_indices().peekable();
    let mut end = rest.len();
    while let Some((i, c)) = iter.next() {
        if c.is_whitespace() {
            end = i;
            break;
        }
        if c == '\\'
            && let Some(&(_, next)) = iter.peek()
        {
            chars.push((next, true));
            iter.next();
            continue;
        }
        chars.push((c, false));
    }
    while chars
        .last()
        .is_some_and(|(c, escaped)| !escaped && TRAILING.contains(c))
    {
        chars.pop();
    }
    if chars.is_empty() {
        return None;
    }
    Some((chars.into_iter().map(|(c, _)| c).collect(), start + 1 + end))
}

pub fn scan(text: &str) -> Scan {
    let code = code_ranges(text);
    let mut imports = Vec::new();
    let mut only_imports = true;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let mut covered: Vec<Range<usize>> = Vec::new();
        let mut previous: Option<char> = None;
        for (i, c) in body.char_indices() {
            let starts = previous.is_none_or(char::is_whitespace);
            previous = Some(c);
            if c != '@' || !starts || in_code(&code, offset + i) {
                continue;
            }
            if covered.iter().any(|r| r.contains(&i)) {
                continue;
            }
            if let Some((path, end)) = parse_at(body, i) {
                imports.push(Import { path });
                covered.push(i..end);
            }
        }
        let rest_blank = body
            .char_indices()
            .all(|(i, c)| c.is_whitespace() || covered.iter().any(|r| r.contains(&i)));
        if !rest_blank {
            only_imports = false;
        }
        offset += line.len();
    }
    Scan {
        imports,
        only_imports,
    }
}

#[cfg(test)]
mod tests;
