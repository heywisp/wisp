use std::collections::BTreeSet;

use crate::setting_names::production;
use crate::tokens::{File, Kind, Placed, Token, is, tokens, walk};

struct List {
    line: usize,
    name: String,
    items: Vec<String>,
}

fn crate_of(path: &str) -> &str {
    path.split('/').next().unwrap_or_default()
}

fn variants(file: &File, name: &str) -> Option<Vec<String>> {
    let t = &file.tokens;
    let at = (1..t.len()).find(|&k| is(&t[k - 1].token, "enum") && is(&t[k].token, name))?;
    let open = at + t[at..].iter().position(|p| is(&p.token, "{"))?;
    let (mut found, mut depth, mut next) = (vec![], 0, true);
    for token in t[open + 1..].iter().map(|p| &p.token) {
        match token.text.as_str() {
            _ if token.kind == Kind::Str => {}
            "}" | ")" | "]" if depth == 0 => break,
            "{" | "(" | "[" => depth += 1,
            "}" | ")" | "]" => depth -= 1,
            "," if depth == 0 => next = true,
            word if depth == 0 && next && token.kind == Kind::Ident => {
                found.push(word.to_owned());
                next = false;
            }
            _ => {}
        }
    }
    Some(found)
}

fn path_crate(file: &File, name: &str) -> Option<String> {
    let t = &file.tokens;
    (0..t.len())
        .filter(|&k| is(&t[k].token, "use"))
        .find_map(|k| {
            let end = k + t[k..].iter().position(|p| is(&p.token, ";"))?;
            let named = t[k + 1..end].iter().any(|p| is(&p.token, name));
            named.then(|| t[k + 1].token.text.replace('_', "-"))
        })
}

fn resolve(files: &[File], list: &File, name: &str) -> Result<Vec<String>, String> {
    if let Some(found) = variants(list, name) {
        return Ok(found);
    }
    let named =
        path_crate(list, name).filter(|k| !matches!(k.as_str(), "crate" | "self" | "super"));
    let home = crate_of(&list.path).to_owned();
    for krate in [named, Some(home)].into_iter().flatten() {
        let mut found: Vec<Vec<String>> = files
            .iter()
            .filter(|f| crate_of(&f.path) == krate)
            .filter_map(|f| variants(f, name))
            .collect();
        if found.len() > 1 {
            return Err(format!("{} enums {name} in {krate}", found.len()));
        }
        if let Some(one) = found.pop() {
            return Ok(one);
        }
    }
    Err(format!("no enum {name}"))
}

fn lists(file: &File) -> Vec<List> {
    let t = &file.tokens;
    let starts = (1..t.len()).filter(|&k| is(&t[k - 1].token, "const") && is(&t[k].token, "ALL"));
    starts
        .filter_map(|k| {
            let eq = k + t[k..].iter().position(|p| is(&p.token, "="))?;
            let open = eq + 1 + usize::from(t.get(eq + 1).is_some_and(|p| is(&p.token, "&")));
            t.get(open).filter(|p| is(&p.token, "["))?;
            let close = open + t[open..].iter().position(|p| is(&p.token, "]"))?;
            let body: Vec<&Token> = t[open + 1..close].iter().map(|p| &p.token).collect();
            let (mut names, mut items) = (BTreeSet::new(), vec![]);
            for item in body.split(|token| is(token, ",")).filter(|i| !i.is_empty()) {
                let words: Vec<&str> = item
                    .iter()
                    .filter(|t| t.kind == Kind::Ident)
                    .map(|t| t.text.as_str())
                    .collect();
                if words.len() < 2 || item.iter().any(|t| t.kind != Kind::Ident && !is(t, ":")) {
                    return None;
                }
                names.insert(match words[words.len() - 2] {
                    "Self" => t[k].imp.clone()?,
                    name => name.to_owned(),
                });
                items.push(words[words.len() - 1].to_owned());
            }
            let name = names.pop_first()?;
            let line = t[k].token.line;
            names.is_empty().then_some(List { line, name, items })
        })
        .collect()
}

fn problems(files: &[File]) -> Vec<String> {
    let mut found = vec![];
    for file in files {
        for list in lists(file) {
            let at = format!("{}:{}: {}::ALL", file.path, list.line, list.name);
            let want = match resolve(files, file, &list.name) {
                Ok(want) => want,
                Err(why) => {
                    found.push(format!("{at}: {why}"));
                    continue;
                }
            };
            for variant in &want {
                match list.items.iter().filter(|i| *i == variant).count() {
                    1 => {}
                    0 => found.push(format!("{at} misses {variant}")),
                    n => found.push(format!("{at} names {variant} {n} times")),
                }
            }
            for item in list.items.iter().filter(|i| !want.contains(i)) {
                found.push(format!("{at} names {item}, not a variant"));
            }
        }
    }
    found
}

#[test]
fn every_all_list_names_each_variant_of_its_enum_once() {
    let files = production();
    let tied: BTreeSet<String> = files
        .iter()
        .flat_map(|f| {
            lists(f)
                .into_iter()
                .map(|l| format!("{} {}", f.path, l.name))
        })
        .collect();
    for known in [
        "wisp-protocol/src/lib.rs Effort",
        "wisp-core/src/plugin/imports.rs Import",
        "wisp-core/src/agent/tool_tests/questions.rs Ending",
        "wisp-tui/src/app/commands/tests.rs CommandId",
    ] {
        assert!(tied.contains(known), "{known}: {tied:?}");
    }
    assert_eq!(problems(&files), Vec::<String>::new());
}

#[test]
fn one_of_every_event_samples_every_log_event() {
    let files = production();
    let file = |path: &str| files.iter().find(|f| f.path == path).unwrap();
    let every = variants(file("wisp-protocol/src/log.rs"), "LogEvent").unwrap();
    let samples = &file("wisp-core/src/session/tests.rs").tokens;
    let inside = |p: &Placed| p.func.as_deref() == Some("one_of_every_event");
    let called: BTreeSet<&str> = samples
        .windows(3)
        .filter(|w| inside(&w[1]) && w[1].token.kind == Kind::Ident && is(&w[2].token, "("))
        .filter(|w| !is(&w[0].token, ".") && !is(&w[0].token, ":"))
        .map(|w| w[1].token.text.as_str())
        .chain(["one_of_every_event"])
        .collect();
    let sampled: BTreeSet<&str> = samples
        .windows(4)
        .filter(|w| w[0].func.as_deref().is_some_and(|f| called.contains(f)))
        .filter(|w| is(&w[0].token, "LogEvent") && is(&w[1].token, ":") && is(&w[2].token, ":"))
        .map(|w| w[3].token.text.as_str())
        .collect();
    let missing: Vec<&String> = every
        .iter()
        .filter(|v| *v != "Unknown" && !sampled.contains(v.as_str()))
        .collect();
    assert!(every.len() > 50, "{every:?}");
    assert_eq!(missing, Vec::<&String>::new());
}

fn planted(sample: &str) -> Vec<String> {
    let sample = sample
        .replace("THREE", "pub enum Mode { A, #[default] B, C }")
        .replace("TWO", "pub enum Mode { A, B }");
    let files: Vec<File> = sample
        .split('|')
        .filter_map(|file| file.trim().split_once(' '))
        .map(|(path, source)| File {
            path: path.to_owned(),
            tokens: walk(&tokens(source), false).placed,
            escapes: vec![],
        })
        .collect();
    problems(&files)
}

#[test]
fn each_planted_list_fails_and_each_tied_form_passes() {
    for (sample, want) in [
        (
            "a/src/lib.rs THREE impl Mode { const ALL: L = [Self::A, Self::B]; }",
            "a/src/lib.rs:1: Mode::ALL misses C",
        ),
        (
            "a/src/lib.rs THREE impl Mode { const ALL: L = [Self::A, Self::B, Self::C, Self::C]; }",
            "a/src/lib.rs:1: Mode::ALL names C 2 times",
        ),
        (
            "a/src/lib.rs THREE impl Mode { const ALL: L = [Self::B]; }",
            "a/src/lib.rs:1: Mode::ALL misses A; a/src/lib.rs:1: Mode::ALL misses C",
        ),
        (
            "a/src/lib.rs THREE impl Mode { const ALL: L = [Self::A, Self::B, Self::C, Self::D]; }",
            "a/src/lib.rs:1: Mode::ALL names D, not a variant",
        ),
        (
            "a/src/lib.rs THREE | a/src/list.rs const ALL: L = [Mode::A, Mode::B];",
            "a/src/list.rs:1: Mode::ALL misses C",
        ),
        (
            "a/src/lib.rs THREE | b/src/lib.rs TWO | b/src/list.rs const ALL: L = [Mode::A, Mode::B, Mode::C];",
            "b/src/list.rs:1: Mode::ALL names C, not a variant",
        ),
        (
            "a/src/one.rs THREE | a/src/two.rs THREE | a/src/list.rs const ALL: L = [Mode::A, Mode::B, Mode::C];",
            "a/src/list.rs:1: Mode::ALL: 2 enums Mode in a",
        ),
        (
            "a/src/lib.rs THREE | b/src/lib.rs TWO | c/src/list.rs use b::Mode; const ALL: L = [Mode::A, Mode::B, Mode::C];",
            "c/src/list.rs:1: Mode::ALL names C, not a variant",
        ),
        (
            "a/src/lib.rs TWO | b/src/lib.rs THREE | a/src/list.rs use b::Mode; const ALL: L = [Mode::A, Mode::B];",
            "a/src/list.rs:1: Mode::ALL misses C",
        ),
    ] {
        assert_eq!(planted(sample).join("; "), want, "{sample}");
    }
    for sample in [
        "a/src/lib.rs THREE impl Mode { const ALL: L = [Self::C, Self::A, Self::B]; }",
        "a/src/lib.rs THREE | a/src/list.rs const ALL: L = [Mode::A, Mode::B, Mode::C];",
        "a/src/lib.rs THREE | b/src/lib.rs TWO | b/src/list.rs const ALL: L = [Mode::A, Mode::B];",
        "b/src/lib.rs TWO | a/src/lib.rs THREE | c/src/list.rs use a::Mode; const ALL: L = [Mode::A, Mode::B, Mode::C];",
        r#"a/src/lib.rs THREE const SOME: L = [Mode::A]; const ALL: &str = "all"; const ALL: u8 = 0xFF; const ALL: &[&str] = &["a"];"#,
    ] {
        assert_eq!(planted(sample), Vec::<String>::new(), "{sample}");
    }
}
