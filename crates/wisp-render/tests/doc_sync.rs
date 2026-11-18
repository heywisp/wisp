#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test on a missing file or table"
)]

use std::collections::BTreeSet;

use wisp_render::theme::glyphs::{AMBIGUOUS_WIDE, ASCII, UNICODE};
use wisp_render::theme::{
    Ansi16, BAND_PAIR, BASE_TOKENS, Depth, FIT_STEPS, Mode, Rgb, Theme, Token, contrast, xterm,
};

const SPEC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/design/tui-brand.md"
);

const PREVIEW: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/design/preview.py");

fn spec() -> String {
    std::fs::read_to_string(SPEC).unwrap()
}

fn section<'a>(spec: &'a str, start: &str, end: &str) -> &'a str {
    let from = spec.find(start).unwrap();
    let to = from + spec[from..].find(end).unwrap();
    &spec[from..to]
}

struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let inner = line.strip_prefix('|').unwrap_or(line);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut out = vec![];
    let mut cell = String::new();
    let mut escaped = false;
    for c in inner.chars() {
        match c {
            '|' if !escaped => out.push(std::mem::take(&mut cell).trim().to_owned()),
            '|' => {
                cell.pop();
                cell.push(c);
            }
            _ => cell.push(c),
        }
        escaped = c == '\\';
    }
    out.push(cell.trim().to_owned());
    out
}

fn tables(text: &str) -> Vec<Table> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Table> = vec![];
    let mut i = 0;
    while i < lines.len() {
        let is_row = lines[i].trim_start().starts_with('|');
        let next_is_rule = lines.get(i + 1).is_some_and(|l| l.contains("|---"));
        if is_row && next_is_rule {
            let header = cells(lines[i]);
            let mut rows = vec![];
            i += 2;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                rows.push(cells(lines[i]));
                i += 1;
            }
            out.push(Table { header, rows });
        } else {
            i += 1;
        }
    }
    out
}

fn code_spans(cell: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = cell;
    while let Some(at) = rest.find('`') {
        let ticks = rest[at..].len() - rest[at..].trim_start_matches('`').len();
        let fence = &rest[at..at + ticks];
        let body = &rest[at + ticks..];
        let Some(end) = body.find(fence) else { break };
        let text = &body[..end];
        let padded = !text.trim().is_empty() && text.starts_with(' ') && text.ends_with(' ');
        out.push(
            if padded {
                &text[1..text.len() - 1]
            } else {
                text
            }
            .to_owned(),
        );
        rest = &body[end + ticks..];
    }
    out
}

fn tokens(cell: &str) -> Vec<Token> {
    let mut out = vec![];
    for name in code_spans(cell) {
        let found: Vec<Token> = match name.strip_suffix('*') {
            Some(prefix) => Token::ALL
                .into_iter()
                .filter(|t| t.name().starts_with(prefix))
                .collect(),
            None => Token::ALL
                .into_iter()
                .filter(|t| t.name() == name)
                .collect(),
        };
        assert!(
            !found.is_empty(),
            "the spec names an unknown token `{name}`"
        );
        out.extend(found);
    }
    out
}

fn groups(cell: &str) -> Vec<Vec<Token>> {
    cell.split(" / ").map(tokens).collect()
}

fn hexes(cell: &str) -> Vec<Rgb> {
    let mut out = vec![];
    for (i, _) in cell.match_indices('#') {
        let digits = cell.get(i + 1..i + 7).unwrap_or("");
        if digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
            out.push(Rgb::hex(u32::from_str_radix(digits, 16).unwrap()));
        }
    }
    out
}

fn leading_number(text: &str) -> Option<u8> {
    let digits: String = text
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn floor(cell: &str) -> Option<Option<f64>> {
    match cell.trim() {
        "" => None,
        "—" => Some(None),
        v => Some(Some(v.parse().unwrap())),
    }
}

fn ratio(cell: &str) -> String {
    cell.replace('*', "")
}

const MODES: [(Mode, usize); 2] = [(Mode::Dark, 1), (Mode::Light, 2)];

#[test]
fn every_hex_value_matches_the_token_tables() {
    let spec = spec();
    let mut seen = BTreeSet::new();
    let mut checked = 0;
    for table in tables(section(&spec, "### 2.2 Tokens", "### 2.3")) {
        let min_col = table.header.iter().position(|h| h == "Min");
        for row in &table.rows {
            let names = groups(&row[0]);
            seen.extend(names.iter().flatten().copied());
            for (mode, col) in MODES {
                let theme = Theme::new(mode, Depth::TrueColor);
                let cell = &row[col];
                if let Some(alias) = cell.strip_prefix('=') {
                    let targets: Vec<Vec<Token>> = alias.split(" / ").map(tokens).collect();
                    for (group, target) in names.iter().zip(&targets) {
                        for token in group {
                            assert_eq!(token.base(), target[0].base(), "{}", token.name());
                        }
                    }
                    if alias.contains("(bold)") {
                        assert!(
                            theme
                                .fg(names[0][0])
                                .add_modifier
                                .contains(ratatui_core::style::Modifier::BOLD)
                        );
                    }
                    if alias.contains("italic") {
                        let t = names[0][0];
                        assert!(
                            theme
                                .fg(t)
                                .add_modifier
                                .contains(ratatui_core::style::Modifier::ITALIC)
                        );
                    }
                    continue;
                }
                let values = hexes(cell);
                if values.is_empty() {
                    continue;
                }
                assert_eq!(values.len(), names.len(), "row {row:?}");
                for (group, want) in names.iter().zip(values) {
                    for token in group {
                        assert_eq!(theme.rgb(*token), want, "{} ({mode:?})", token.name());
                        checked += 1;
                    }
                }
            }
            if let Some(col) = min_col
                && let Some(want) = floor(&row[col])
            {
                for token in names.iter().flatten() {
                    assert_eq!(token.floor(), want, "the floor of {}", token.name());
                }
            }
        }
    }
    for token in Token::ALL {
        assert!(
            seen.contains(&token),
            "{} is missing from spec 2.2",
            token.name()
        );
    }
    assert!(
        checked >= 2 * BASE_TOKENS - 2,
        "only {checked} hex values checked"
    );
}

#[test]
fn the_contrast_tables_match_the_formula() {
    let spec = spec();
    let section = section(&spec, "### 2.3 Contrast", "### 2.4");
    let all = tables(section);
    let grounds = all
        .iter()
        .find(|t| t.header.iter().any(|h| h == "on `sunken`"))
        .unwrap();
    let ground_tokens = [Token::Bg, Token::Surface, Token::Sunken];
    assert!(
        grounds.rows.len() >= 20,
        "grounds: {} rows",
        grounds.rows.len()
    );
    for row in &grounds.rows {
        let token = tokens(&row[0])[0];
        for (mode, at) in [(Mode::Dark, 1), (Mode::Light, 5)] {
            let theme = Theme::new(mode, Depth::TrueColor);
            assert_eq!(
                hexes(&row[at]),
                [theme.rgb(token)],
                "{} ({mode:?})",
                token.name()
            );
            for (i, ground) in ground_tokens.iter().enumerate() {
                let got = contrast(theme.rgb(token), theme.rgb(*ground));
                assert_eq!(
                    format!("{got:.2}"),
                    ratio(&row[at + 1 + i]),
                    "{} on {}",
                    token.name(),
                    ground.name()
                );
            }
        }
        if let Some(want) = floor(&row[9]) {
            assert_eq!(token.floor(), want, "the floor of {}", token.name());
        }
    }
    let common = all
        .iter()
        .find(|t| t.header.iter().any(|h| h.contains("Campbell")))
        .unwrap();
    assert!(
        common.rows.len() >= 13,
        "common: {} rows",
        common.rows.len()
    );
    for row in &common.rows {
        let token = tokens(&row[0])[0];
        for (i, head) in common.header.iter().enumerate().skip(1) {
            let ground = hexes(head)[0];
            let dark = wisp_render::theme::luminance(ground) < 0.5;
            let mode = if dark { Mode::Dark } else { Mode::Light };
            let got = contrast(Theme::new(mode, Depth::TrueColor).rgb(token), ground);
            assert_eq!(
                format!("{got:.2}"),
                ratio(&row[i]),
                "{} on {head}",
                token.name()
            );
        }
    }
}

#[test]
fn every_ansi256_index_matches_the_spec() {
    let spec = spec();
    let table = tables(section(&spec, "### 2.4 ANSI-256", "### 2.5"))
        .into_iter()
        .next()
        .unwrap();
    let mut seen = BTreeSet::new();
    for row in &table.rows {
        let names = groups(&row[0]);
        for (mode, col) in MODES {
            let theme = Theme::new(mode, Depth::Ansi256);
            let parts: Vec<&str> = row[col].split(" / ").collect();
            assert_eq!(parts.len(), names.len(), "row {row:?}");
            for (group, part) in names.iter().zip(&parts) {
                let index = leading_number(part).unwrap();
                for token in group {
                    seen.insert(*token);
                    assert_eq!(
                        theme.ansi256(*token),
                        Some(index),
                        "{} ({mode:?})",
                        token.name()
                    );
                }
                if let Some(hex) = hexes(part).first() {
                    assert_eq!(xterm(index), *hex, "index {index}");
                    let shown = part.split_whitespace().last().unwrap();
                    let got = contrast(
                        xterm(index),
                        Theme::new(mode, Depth::TrueColor).rgb(Token::Bg),
                    );
                    assert_eq!(format!("{got:.2}"), shown, "{index} on bg ({mode:?})");
                }
                if part.contains("plus bold") {
                    let style = theme.bg(group[0]);
                    assert!(
                        style
                            .add_modifier
                            .contains(ratatui_core::style::Modifier::BOLD)
                    );
                }
            }
        }
    }
    for token in &Token::ALL[..BASE_TOKENS] {
        if *token != Token::Bg {
            assert!(
                seen.contains(token),
                "{} is missing from spec 2.4",
                token.name()
            );
        }
    }
}

#[test]
fn every_ansi16_code_matches_the_spec() {
    let spec = spec();
    let table = tables(section(&spec, "### 2.5 ANSI-16", "### 2.6"))
        .into_iter()
        .next()
        .unwrap();
    let mut checked = 0;
    for row in &table.rows {
        if code_spans(&row[0]).is_empty() {
            continue;
        }
        let names = groups(&row[0]);
        for (mode, col) in MODES {
            let theme = Theme::new(mode, Depth::Ansi16);
            let parts: Vec<&str> = row[col].split(" / ").collect();
            assert_eq!(parts.len(), names.len(), "row {row:?}");
            for (group, part) in names.iter().zip(&parts) {
                let want = Ansi16 {
                    code: if part.trim_start().starts_with("default") {
                        None
                    } else {
                        Some(leading_number(part).unwrap())
                    },
                    bold: part.contains("bold"),
                    italic: part.contains("italic"),
                };
                for token in group {
                    assert_eq!(theme.ansi16(*token), want, "{} ({mode:?})", token.name());
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 40, "only {checked} codes checked");
}

#[test]
fn the_context_report_colours_match_the_spec() {
    use wisp_protocol::context::Category;
    use wisp_render::chrome::context::{SPACE_TOKEN, category_label, category_token};
    let spec = spec();
    let table = tables(section(&spec, "### 7.9 The context report", "## 8."))
        .into_iter()
        .next()
        .unwrap();
    let mut rows = 0;
    for row in &table.rows {
        let token = match Category::ORDER.iter().find(|c| category_label(c) == row[0]) {
            Some(category) => category_token(category),
            None => {
                assert!(["Free", "Reserve"].contains(&row[0].as_str()), "{row:?}");
                SPACE_TOKEN
            }
        };
        let named = code_spans(&row[1]);
        assert_eq!(
            named.first().map(String::as_str),
            Some(token.name()),
            "{row:?}"
        );
        for (mode, hex, on_bg) in [(Mode::Dark, 2, 3), (Mode::Light, 4, 5)] {
            let theme = Theme::new(mode, Depth::TrueColor);
            assert_eq!(hexes(&row[hex]), vec![theme.rgb(token)], "{row:?}");
            let got = contrast(theme.rgb(token), theme.rgb(Token::Bg));
            assert_eq!(format!("{got:.2}"), row[on_bg], "{row:?} ({mode:?})");
        }
        let indices: Vec<&str> = row[6].split(" / ").collect();
        let codes: Vec<&str> = row[7].split(" / ").collect();
        for (i, mode) in [Mode::Dark, Mode::Light].into_iter().enumerate() {
            let index = indices.get(i).copied().unwrap_or(indices[0]);
            assert_eq!(
                Theme::new(mode, Depth::Ansi256).ansi256(token),
                leading_number(index),
                "{row:?} ({mode:?})"
            );
            let code = codes.get(i).copied().unwrap_or(codes[0]);
            let want = if code.trim() == "default" {
                None
            } else {
                leading_number(code)
            };
            assert_eq!(
                Theme::new(mode, Depth::Ansi16).ansi16(token).code,
                want,
                "{row:?} ({mode:?})"
            );
        }
        rows += 1;
    }
    assert_eq!(rows, Category::ORDER.len() + 2);
}

#[test]
fn the_context_panel_parts_match_the_spec() {
    use wisp_protocol::context::FillPart;
    use wisp_render::chrome::context::{free_look, part_look};
    let spec = spec();
    let table = tables(section(&spec, "### 7.9 The context report", "## 8."))
        .into_iter()
        .nth(1)
        .unwrap();
    let mut rows = 0;
    for row in &table.rows {
        let (_, token) = FillPart::ORDER
            .iter()
            .map(|&part| part_look(part))
            .chain([free_look()])
            .find(|(word, _)| *word == row[0])
            .unwrap();
        assert_eq!(code_spans(&row[1]), [token.name()], "{row:?}");
        assert_eq!(
            Theme::new(Mode::Dark, Depth::Ansi16).ansi16(token).code,
            leading_number(&row[2]),
            "{row:?}"
        );
        rows += 1;
    }
    assert_eq!(rows, FillPart::ORDER.len() + 1);
}

#[test]
fn preview_py_band_matches_the_theme() {
    let preview = std::fs::read_to_string(PREVIEW).unwrap();
    let (over, under) = BAND_PAIR;
    for line in [
        format!("BAND_PAIR = ('{over}', '{under}')"),
        format!("FIT_STEPS = {FIT_STEPS}"),
    ] {
        assert!(
            preview.lines().any(|l| l == line),
            "preview.py lacks {line}"
        );
    }
}

#[test]
fn every_glyph_matches_the_glyph_table() {
    let spec = spec();
    let table = tables(section(&spec, "### 5.1 Glyphs", "### 5.2"))
        .into_iter()
        .next()
        .unwrap();
    let mut keys = 0;
    for row in &table.rows {
        let unicode = code_spans(&row[1]);
        let wide = if row[4] == "same" {
            unicode.clone()
        } else {
            code_spans(&row[4])
        };
        let ascii = if row[5].starts_with("same") {
            wide.clone()
        } else {
            code_spans(&row[5])
        };
        for (n, key) in code_spans(&row[7]).iter().enumerate() {
            for (set, column) in [
                (UNICODE, &unicode),
                (AMBIGUOUS_WIDE, &wide),
                (ASCII, &ascii),
            ] {
                let drawn: String = set
                    .all()
                    .into_iter()
                    .filter(|(name, _)| *name == key.as_str())
                    .map(|(_, glyph)| glyph)
                    .collect();
                let unicode_spark = key == "spark" && set.spark.is_none();
                assert!(
                    unicode_spark || drawn == column[n],
                    "{key} in {:?}: {drawn:?}, the spec says {:?}",
                    set.set,
                    column[n]
                );
            }
            keys += 1;
        }
    }
    assert_eq!(keys, 33);
}
