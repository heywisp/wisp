use std::fs;
use std::path::{Path, PathBuf};

#[path = "../../../tests/support/purity.rs"]
mod purity;

use purity::rust_source::{Kind, Token, attribute_end, is, scan};

fn comment_lines(source: &str) -> Result<Vec<usize>, String> {
    let scanned = scan(source);
    if let Some(error) = scanned.unterminated {
        return Err(error);
    }
    let tokens = scanned.tokens;
    let mut found = scanned.comment_lines;
    for (i, token) in tokens.iter().enumerate() {
        if !is(token, "#") {
            continue;
        }
        let bracket = i + 1 + usize::from(tokens.get(i + 1).is_some_and(|t| is(t, "!")));
        if tokens.get(bracket).is_some_and(|t| is(t, "["))
            && is_doc_attribute(&tokens[i..attribute_end(&tokens, i)])
        {
            found.push(token.line);
        }
    }
    found.sort_unstable();
    Ok(found)
}

fn is_doc_attribute(tokens: &[Token]) -> bool {
    let mut depth = 0;
    let mut words = Vec::new();
    let mut first_comma = None;
    for (i, token) in tokens.iter().enumerate().skip(1) {
        if token.kind == Kind::Ident {
            if is(token, "r") && tokens.get(i + 1).is_some_and(|t| is(t, "#")) {
                continue;
            }
            words.push(if is(&tokens[i - 1], "$") {
                ""
            } else {
                token.text.as_str()
            });
        } else if token.kind == Kind::Punct {
            match token.text.as_str() {
                "[" | "(" => depth += 1,
                "]" | ")" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                "," if depth == 2 && first_comma.is_none() => first_comma = Some(words.len()),
                _ => {}
            }
        }
    }
    match words.first().copied() {
        Some("doc") => true,
        Some("cfg_attr") => first_comma.is_some_and(|after| words[after..].contains(&"doc")),
        _ => false,
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    files_ending(dir, ".rs", out)
}

fn files_ending(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<PathBuf>>>()?;
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            let build_output = name == "target" || path.join("CACHEDIR.TAG").exists();
            if !build_output && !name.starts_with('.') {
                files_ending(&path, suffix, out)?;
            }
        } else if name.ends_with(suffix) {
            out.push(path);
        }
    }
    Ok(())
}

#[test]
fn no_rust_file_in_the_workspace_has_a_comment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(root.join("Cargo.lock").is_file(), "{}", root.display());
    let mut files = Vec::new();
    rust_files(&root, &mut files).unwrap();
    assert!(
        files.len() >= 20,
        "only {} Rust files found under {}",
        files.len(),
        root.display()
    );
    assert!(
        files
            .iter()
            .any(|f| f.ends_with("wisp-protocol/tests/no_comments.rs"))
    );
    assert!(files.iter().any(|f| f.ends_with("wisp-core/src/agent.rs")));

    let mut failures = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).unwrap();
        let shown = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        match comment_lines(&source) {
            Ok(lines) => failures.extend(lines.into_iter().map(|line| format!("{shown}:{line}"))),
            Err(e) => failures.push(format!("{shown}: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "comments found:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_comment_kind_is_flagged() {
    let cases = [
        "// line",
        "/// outer doc",
        "//! inner doc",
        "/* block */",
        "/** outer block doc */",
        "/*! inner block doc */",
        "let x = 1; // trailing",
        "let x = /* inline */ 1;",
        "////",
        "//",
        "/**/",
        "#[doc = \"outer\"]",
        "#![doc = \"inner\"]",
        "# [ doc(hidden) ]",
        "#![doc(html_root_url = \"x\")]",
        "struct A; #[doc = include_str!(\"a.md\")] struct B;",
        "# ! [doc = \"spaced inner\"]",
        "#  [ doc ]",
        "#[r#doc = \"raw\"]",
        "#![r#doc(hidden)]",
        "#[cfg_attr(feature = \"x\", doc = \"y\")]",
        "#[cfg_attr(all(a, b), derive(Debug), doc(hidden))]",
        "#![cfg_attr(docsrs, r#doc = \"z\")]",
        "#[cfg_attr(doc, doc = \"both\")]",
        "#[cfg_attr(all(), rustfmt::skip(')'), doc = \"prose that is really a doc comment\")]",
        "#[cfg_attr(all(), rustfmt::skip(']'), doc = \"x\")]",
        "#[cfg_attr(any(), foo(']'), doc = \"x\")]",
        "#[cfg_attr(feature = r#\"a\"b\"#, doc = \"z\")]",
        "#[cfg_attr(feature = \"a)\", doc = \"z\")]",
        "#[cfg_attr(x = b']', y = br\")\", doc = \"z\")]",
        "#[cfg_attr(any(doc, test), derive(Debug), doc(hidden))]",
    ];
    for case in cases {
        assert_eq!(comment_lines(case), Ok(vec![1]), "{case}");
    }
}

#[test]
fn comments_are_reported_at_their_first_line() {
    let source = "fn a() {}\n// one\nfn b() {}\n/* two\n   lines */\nlet c = 3; // three\n";
    assert_eq!(comment_lines(source), Ok(vec![2, 4, 6]));
    let source = concat!(
        "let s = \"one\n// two\";\n",
        "#[doc = \"three\n four\"] /* five\n six */\n",
        "#![doc(hidden)] // seven",
    );
    assert_eq!(comment_lines(source), Ok(vec![3, 4, 6, 6]));
}

#[test]
fn nested_block_comments_end_at_their_matching_close() {
    assert_eq!(
        comment_lines("/* a /* b */ c */ let s = \"/*\";"),
        Ok(vec![1])
    );
    assert_eq!(comment_lines("/* a /* b */ c */\n// d"), Ok(vec![1, 2]));
    assert!(comment_lines("/* a /* b */ c").is_err());
}

#[test]
fn comment_markers_inside_literals_are_not_comments() {
    let cases = [
        r#"let url = "https://example.com/a";"#,
        r#"let s = "/* not a comment */";"#,
        r#"let s = "say \"// hi\" now";"#,
        r#"let s = "\\"; let t = "//";"#,
        r##"let s = r#"a "// b" c"#;"##,
        r###"let s = r##"a "# // "## ;"###,
        r#"let s = r"//";"#,
        r#"let s = b"//";"#,
        r#"let s = br"/*";"#,
        r##"let s = br#"/* "*/"#;"##,
        r#"let s = c"//";"#,
        r#"let s = cr"//";"#,
        "let c = '/'; let d = '*';",
        r"let c = b'/';",
        r"let c = '\'';",
        "let s = \"line one\n// line two\";",
        "event: ping\\ndata: {}",
        "let x = a / b / c; y /= 2; let z = *p;",
        "#[derive(Debug)] #[docs] #[document] #[doc_cfg] #![allow(dead_code)]",
        "#[cfg_attr(test, derive(Debug))] #[cfg_attr(docsrs, feature(x))]",
        "macro_rules! m { ($(#[$doc:meta])* $name:ident) => { $(#[$doc])* struct $name; } }",
        "#[$doc] #![$doc] # [ $doc ]",
        "#![cfg_attr(not(any(test, doc)), no_std)]",
        "#[cfg_attr(not(doc), repr(transparent))]",
        "#[cfg_attr(doc, allow(dead_code))]",
        "#[cfg_attr(all(doc, not(test)), derive(Debug), allow(unused))]",
        "#[cfg_attr(x, allow(dead_code, reason = \"doc = y\"))] #[r#docs]",
        "#[cfg(doc)] fn f() {}",
        r##"let s = "#[doc = \"x\"]"; let t = r"#![doc]";"##,
    ];
    for case in cases {
        assert_eq!(comment_lines(case), Ok(vec![]), "{case}");
    }
}

#[test]
fn literals_end_where_rust_ends_them() {
    let cases = [
        r#"let c = '"'; // after a quote char"#,
        r"let c = '\''; // after an escaped quote",
        r"let c = '\u{1F600}'; // after a unicode escape",
        "let c = 'é'; // after a non-ASCII char",
        "fn f<'a>(x: &'a str) {} // after lifetimes",
        "fn f(x: &'static str) {} // after 'static",
        "let r#match = 1; // after a raw identifier",
        r#"let s = "a\\"; // after an escaped backslash"#,
        r##"let s = r#"x"#; // after a raw string"##,
        "let s = br\"\\\"; // raw strings have no escapes",
    ];
    for case in cases {
        assert_eq!(comment_lines(case), Ok(vec![1]), "{case}");
    }
}

#[test]
fn unterminated_literals_are_errors_not_passes() {
    for case in [
        "let s = \"open // x",
        "let s = r#\"open\" // x",
        "let s = b\"open",
        "let s = c\"open",
        "let s = br##\"open\"#",
        "let s = cr\"open",
        "let c = '\\",
        "let c = b'open",
        "let c = b'\\",
        "/* open",
        "/* outer /* inner */",
    ] {
        assert!(comment_lines(case).is_err(), "{case}");
    }
}

#[test]
fn build_output_directories_are_skipped() {
    let root = std::env::temp_dir().join(format!("wisp-no-comments-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let out = root.join("build-elsewhere/debug/build/x/out");
    fs::create_dir_all(&out).unwrap();
    fs::write(
        root.join("build-elsewhere/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    fs::write(out.join("generated.rs"), "#[doc(hidden)] pub struct G;\n").unwrap();
    fs::create_dir_all(root.join("target/debug")).unwrap();
    fs::write(
        root.join("target/debug/t.rs"),
        "#[doc(hidden)] pub struct T;\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub struct S;\n").unwrap();

    let mut files = Vec::new();
    rust_files(&root, &mut files).unwrap();
    fs::remove_dir_all(&root).unwrap();
    assert_eq!(files, vec![root.join("src/lib.rs")]);
}

fn wit_comment_lines(source: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("//") || line.contains("/*"))
        .map(|(i, _)| i + 1)
        .collect()
}

#[test]
fn no_wit_file_in_the_workspace_has_a_comment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    files_ending(&root, ".wit", &mut files).unwrap();
    assert!(
        files.len() >= 9,
        "only {} WIT files found under {}",
        files.len(),
        root.display()
    );
    assert!(files.iter().any(|f| f.ends_with("wit/v0.1.0/types.wit")));
    assert!(files.iter().any(|f| f.ends_with("wit/v0.2.0/slots.wit")));
    assert!(files.iter().any(|f| f.ends_with("wit/v0.2.0/prompt.wit")));
    assert!(files.iter().any(|f| f.ends_with("wit/v0.2.0/world.wit")));
    assert!(files.iter().any(|f| f.ends_with("wit/v0.3.0/slots.wit")));
    assert!(files.iter().any(|f| f.ends_with("wit/v0.3.0/world.wit")));
    let mut failures = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).unwrap();
        let shown = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        failures.extend(
            wit_comment_lines(&source)
                .into_iter()
                .map(|line| format!("{shown}:{line}")),
        );
    }
    assert!(
        failures.is_empty(),
        "comments found:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_wit_comment_kind_is_flagged() {
    for case in [
        "// line",
        "/// outer doc",
        "/* block */",
        "/** block doc */",
        "  record a { b: u8 } // trailing",
        "record a { /* inline */ b: u8 }",
        "/*\n  spans lines\n*/",
    ] {
        assert_eq!(wit_comment_lines(case), vec![1], "{case}");
    }
    assert_eq!(
        wit_comment_lines("interface a {\n  f: func();\n  /// doc\n}\n"),
        vec![3]
    );
    assert!(wit_comment_lines("package a:b@0.1.0;\ninterface c { d: func() -> u8; }\n").is_empty());
}

#[test]
fn no_html_or_css_file_of_the_web_page_has_a_comment() {
    let page = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/learn/web/page");
    let mut files = Vec::new();
    files_ending(&page, ".html", &mut files).unwrap();
    files_ending(&page, ".css", &mut files).unwrap();
    assert!(files.len() >= 2, "only {} page files", files.len());
    let commented: Vec<String> = files
        .iter()
        .filter(|file| {
            let text = fs::read_to_string(file).unwrap();
            text.contains("<!--") || text.contains("/*")
        })
        .map(|file| file.display().to_string())
        .collect();
    assert!(commented.is_empty(), "comments found in {commented:?}");
}
