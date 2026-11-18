use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    Ident,
    Punct,
    Str,
    Other,
}

#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    pub(crate) text: String,
    pub(crate) line: usize,
    pub(crate) span: Range<usize>,
}

pub(crate) struct Scan {
    pub(crate) tokens: Vec<Token>,
    pub(crate) comment_lines: Vec<usize>,
    pub(crate) unterminated: Option<String>,
}

pub(crate) fn tokens(source: &str) -> Vec<Token> {
    scan(source).tokens
}

pub(crate) fn scan(source: &str) -> Scan {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    let offsets: Vec<usize> = source
        .char_indices()
        .map(|(i, _)| i)
        .chain([source.len()])
        .collect();
    let at = |i: usize| chars.get(i).copied();
    let token = |kind, text, line, start, end: usize| Token {
        kind,
        text,
        line,
        span: offsets[start]..offsets[end.min(n)],
    };
    let mut out = Vec::new();
    let mut comment_lines = Vec::new();
    let mut unterminated = None;
    let mut line = 1;
    let mut i = 0;
    let count_lines =
        |from: usize, to: usize| chars[from..to].iter().filter(|c| **c == '\n').count();
    while i < n {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == Some('/') {
            comment_lines.push(line);
            while i < n && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            comment_lines.push(line);
            let start = i;
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if chars[i] == '/' && at(i + 1) == Some('*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == Some('/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth > 0 {
                unterminated
                    .get_or_insert_with(|| format!("unterminated block comment at line {line}"));
            }
            line += count_lines(start, i);
        } else if c == '"' {
            let start = i;
            let (text, end, closed) = quoted(&chars, i + 1);
            if !closed {
                unterminated.get_or_insert_with(|| format!("unterminated string at line {line}"));
            }
            out.push(token(Kind::Str, text, line, start, end));
            line += count_lines(start, end);
            i = end;
        } else if c == '\'' || c == 'b' && at(i + 1) == Some('\'') {
            let start = i;
            let quote = if c == 'b' { i + 1 } else { i };
            let mut j = quote + 1;
            let mut closed = false;
            let text = if at(j) == Some('\\') || at(j + 1) == Some('\'') {
                while j < n && chars[j] != '\'' {
                    j += if chars[j] == '\\' { 2 } else { 1 };
                }
                closed = at(j) == Some('\'');
                j = (j + 1).min(n);
                "'c'".to_owned()
            } else {
                while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                chars[start..j].iter().collect()
            };
            if !closed && (c == 'b' || at(quote + 1) == Some('\\')) {
                let kind = if c == 'b' { "byte" } else { "char" };
                unterminated.get_or_insert_with(|| format!("unterminated {kind} at line {line}"));
            }
            out.push(token(Kind::Other, text, line, start, j));
            line += count_lines(start, j);
            i = j;
        } else if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let raw = matches!(word.as_str(), "r" | "br" | "cr");
            if raw || matches!(word.as_str(), "b" | "c") {
                let mut hashes = 0;
                while at(j + hashes) == Some('#') {
                    hashes += 1;
                }
                if at(j + hashes) == Some('"') && (raw || hashes == 0) {
                    let start = i;
                    let open = j + hashes + 1;
                    let (text, end, closed) = if raw {
                        raw_quoted(&chars, open, hashes)
                    } else {
                        quoted(&chars, open)
                    };
                    if !closed {
                        let kind = if raw { "raw string" } else { "string" };
                        unterminated
                            .get_or_insert_with(|| format!("unterminated {kind} at line {line}"));
                    }
                    out.push(token(Kind::Str, text, line, start, end));
                    line += count_lines(start, end);
                    i = end;
                    continue;
                }
            }
            out.push(token(Kind::Ident, word, line, i, j));
            i = j;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            out.push(token(Kind::Other, chars[i..j].iter().collect(), line, i, j));
            i = j;
        } else {
            out.push(token(Kind::Punct, c.to_string(), line, i, i + 1));
            i += 1;
        }
    }
    Scan {
        tokens: out,
        comment_lines,
        unterminated,
    }
}

fn quoted(chars: &[char], mut i: usize) -> (String, usize, bool) {
    let mut text = String::new();
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                text.push(chars[i]);
                if let Some(next) = chars.get(i + 1) {
                    text.push(*next);
                }
                i += 2;
            }
            '"' => return (text, i + 1, true),
            c => {
                text.push(c);
                i += 1;
            }
        }
    }
    (text, i.min(chars.len()), false)
}

fn raw_quoted(chars: &[char], mut i: usize, hashes: usize) -> (String, usize, bool) {
    let mut text = String::new();
    while i < chars.len() {
        if chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#')) {
            return (text, i + 1 + hashes, true);
        }
        text.push(chars[i]);
        i += 1;
    }
    (text, i.min(chars.len()), false)
}

pub(crate) fn is(token: &Token, text: &str) -> bool {
    token.text == text && token.kind != Kind::Str
}

fn item_end(tokens: &[Token], start: usize) -> usize {
    let keyword = tokens.get(visibility_end(tokens, start)).is_some_and(|t| {
        t.kind == Kind::Ident
            && matches!(
                t.text.as_str(),
                "fn" | "async"
                    | "const"
                    | "unsafe"
                    | "static"
                    | "type"
                    | "struct"
                    | "enum"
                    | "union"
                    | "trait"
                    | "impl"
                    | "use"
                    | "mod"
                    | "extern"
                    | "macro_rules"
            )
    });
    let mut depth = 0i32;
    let mut i = start;
    while i < tokens.len() {
        let t = &tokens[i];
        if t.kind == Kind::Punct {
            match t.text.as_str() {
                "(" | "[" => depth += 1,
                ")" | "]" | "}" if depth == 0 => return i,
                ")" | "]" => depth -= 1,
                "{" if depth == 0 => {
                    let mut inner = 1;
                    let mut j = i + 1;
                    while j < tokens.len() && inner > 0 {
                        if is(&tokens[j], "{") {
                            inner += 1;
                        } else if is(&tokens[j], "}") {
                            inner -= 1;
                        }
                        j += 1;
                    }
                    return j;
                }
                "{" => depth += 1,
                "}" => depth -= 1,
                ";" if depth == 0 => return i + 1,
                "," if depth == 0 && !keyword => return i + 1,
                _ => {}
            }
        }
        i += 1;
    }
    i
}

pub(crate) fn is_attribute(tokens: &[Token], i: usize) -> bool {
    tokens
        .get(i..i + 2)
        .is_some_and(|t| is(&t[0], "#") && is(&t[1], "["))
}

pub(crate) fn visibility_end(tokens: &[Token], mut i: usize) -> usize {
    if tokens.get(i).is_some_and(|t| is(t, "pub")) {
        i += 1;
        if tokens.get(i).is_some_and(|t| is(t, "(")) {
            i = tokens[i..]
                .iter()
                .position(|t| is(t, ")"))
                .map_or(tokens.len(), |at| i + at + 1);
        }
    }
    i
}

pub(crate) fn attribute_end(tokens: &[Token], start: usize) -> usize {
    let mut depth = 0;
    let mut i = start + 1;
    while i < tokens.len() {
        if is(&tokens[i], "[") {
            depth += 1;
        } else if is(&tokens[i], "]") {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    i
}

pub(crate) fn attribute_text(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|t| match t.kind {
            Kind::Str => format!("\"{}\"", t.text),
            _ => t.text.clone(),
        })
        .collect()
}

pub(crate) fn test_ranges(tokens: &[Token], modules_only: bool) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let start = i;
        let mut test = false;
        while is_attribute(tokens, i) {
            let end = attribute_end(tokens, i);
            test |= attribute_text(&tokens[i..end]) == "#[cfg(test)]";
            i = end;
            if i == tokens.len() {
                break;
            }
        }
        if test {
            let end = item_end(tokens, i);
            let kind = visibility_end(tokens, i);
            if !modules_only || tokens.get(kind).is_some_and(|t| is(t, "mod")) {
                ranges.push(start..end);
                i = end.max(i + 1);
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    ranges
}

pub(crate) fn without_test_items(tokens: Vec<Token>) -> Vec<Token> {
    let mut keep = vec![true; tokens.len()];
    for range in test_ranges(&tokens, false) {
        keep[range].fill(false);
    }
    tokens
        .into_iter()
        .zip(keep)
        .filter_map(|(t, keep)| keep.then_some(t))
        .collect()
}
