use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use wisp_core::endpoints::ProviderKind;

use crate::purity::TEST_MODULES;
use crate::setting_names::{production, roots};
use crate::tokens::{File, Kind, Placed, enclosing, is, scan, tokens, walk};

const PRESENCE_FORM: &str =
    "std::env::var_os(name).is_some_and(|v|v.to_str().is_some_and(|s|!s.trim().is_empty()))";

fn joined(tokens: &[Placed]) -> String {
    tokens
        .iter()
        .map(|p| match p.token.kind {
            Kind::Str => format!("\"{}\"", p.token.text),
            _ => p.token.text.clone(),
        })
        .collect()
}

fn provider_variables() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for provider in ProviderKind::ALL {
        names.extend(
            provider
                .implicit_variables()
                .iter()
                .map(|v| v.raw().to_owned()),
        );
        names.insert(provider.url_variable().raw().to_owned());
        names.insert(provider.bearer_variable().raw().to_owned());
    }
    names
}

fn callee_before(tokens: &[Placed], at: usize) -> String {
    let Some(open) = enclosing(tokens, at, ("(", ")")) else {
        return String::new();
    };
    let mut start = open;
    while start > 0 {
        let prev = &tokens[start - 1].token;
        if prev.kind == Kind::Ident || is(prev, ":") {
            start -= 1;
        } else {
            break;
        }
    }
    joined(&tokens[start..open])
}

const CALLERS: [&str; 3] = [
    "wisp-cli/src/manage.rs",
    "wisp-cli/src/manage/",
    "wisp-cli/src/first_run.rs",
];

fn problems(files: &[File]) -> Vec<String> {
    let providers = provider_variables();
    let mut found = Vec::new();
    for file in files {
        let path = file.path.as_str();
        found.extend(file.escapes.iter().map(|e| format!("{path}: {e}")));
        let defines_read_file = file
            .tokens
            .windows(2)
            .any(|w| is(&w[0].token, "fn") && is(&w[1].token, "read_file"));
        let live: Vec<Placed> = file.tokens.iter().filter(|p| !p.test).cloned().collect();
        let at = |k: usize| live.get(k).map(|p| &p.token);
        let mut in_use = vec![false; live.len()];
        for (k, p) in live.iter().enumerate() {
            if p.token.kind == Kind::Ident && p.token.text == "use" {
                let end = (k..live.len())
                    .find(|&j| is(&live[j].token, ";"))
                    .unwrap_or(live.len());
                for flag in &mut in_use[k..end] {
                    *flag = true;
                }
            }
        }
        let mut k = 0;
        while k < live.len() {
            let t = &live[k].token;
            let line = t.line;
            let place = format!("{path}:{line}");
            if t.kind == Kind::Ident
                && t.text == "env"
                && !in_use[k]
                && at(k + 1).is_some_and(|t| is(t, ":"))
                && at(k + 2).is_some_and(|t| is(t, ":"))
                && at(k + 3).is_some_and(|t| {
                    matches!(t.text.as_str(), "var" | "var_os" | "vars" | "vars_os")
                })
            {
                let full_path = k >= 3
                    && is(&live[k - 1].token, ":")
                    && is(&live[k - 2].token, ":")
                    && is(&live[k - 3].token, "std");
                let function = at(k + 3).map(|t| t.text.clone()).unwrap_or_default();
                let literal = at(k + 5).filter(|t| t.kind == Kind::Str);
                let literal_call = full_path
                    && matches!(function.as_str(), "var" | "var_os")
                    && at(k + 4).is_some_and(|t| is(t, "("))
                    && literal.is_some()
                    && at(k + 6).is_some_and(|t| is(t, ")"));
                let in_real_env = path == "wisp-start/src/paths.rs"
                    && matches!(live[k].func.as_deref(), Some("real_env" | "real_vars"));
                let caps = path == "wisp-tui/src/term/caps.rs";
                let presence = path == "wisp-core/src/secrets.rs"
                    && live[k].func.as_deref() == Some("is_set")
                    && full_path
                    && {
                        let tail = joined(&live[k - 3..live.len().min(k - 3 + 40)]);
                        tail.starts_with(PRESENCE_FORM)
                    };
                let job_mark = full_path
                    && function == "var_os"
                    && joined(&live[k + 4..live.len().min(k + 10)]) == "(wisp_protocol::WISP_JOB)";
                if literal_call {
                    if let Some(name) = literal
                        && providers.contains(&name.text)
                    {
                        found.push(format!("{place}: a provider variable read by name"));
                    }
                } else if !(in_real_env || caps || presence || job_mark) {
                    found.push(format!("{place}: env::{function} outside the literal form"));
                }
            }
            if t.kind == Kind::Ident && t.text == "include" && at(k + 1).is_some_and(|n| is(n, "!"))
            {
                found.push(format!("{place}: include! hides code from the guard"));
            }
            let import_reader = path.starts_with("wisp-cli/src/manage")
                || path == "wisp-cli/src/first_run.rs"
                || path == "wisp-core/src/secrets/import.rs";
            if t.kind == Kind::Ident
                && t.text == "read_file"
                && at(k + 1).is_some_and(|n| is(n, "("))
                && !(k > 0 && is(&live[k - 1].token, "fn"))
                && (!defines_read_file || (k >= 3 && is(&live[k - 3].token, "import")))
                && !import_reader
            {
                found.push(format!(
                    "{place}: secrets::import::read_file called outside the import commands"
                ));
            }
            if is(t, "use") && t.kind == Kind::Ident {
                let end = (k..live.len())
                    .find(|&j| is(&live[j].token, ";"))
                    .unwrap_or(live.len());
                let words: Vec<&str> = live[k + 1..end]
                    .iter()
                    .filter(|p| p.token.kind == Kind::Ident)
                    .map(|p| p.token.text.as_str())
                    .collect();
                if words.first() == Some(&"std") && words.contains(&"env") {
                    found.push(format!("{place}: std::env brought into scope"));
                }
                if words.contains(&"import") && words.contains(&"read_file") && !import_reader {
                    found.push(format!(
                        "{place}: secrets::import::read_file brought into scope"
                    ));
                }
                if words
                    .windows(2)
                    .any(|w| w[0] == "Variables" && w[1] == "as")
                {
                    found.push(format!("{place}: Variables renamed"));
                }
            }
            if is(t, "type") && t.kind == Kind::Ident {
                let end = (k..live.len())
                    .find(|&j| is(&live[j].token, ";"))
                    .unwrap_or(live.len());
                if live[k..end].iter().any(|p| is(&p.token, "Variables")) {
                    found.push(format!("{place}: a type alias names Variables"));
                }
            }
            if is(t, "Variables") && t.kind == Kind::Ident {
                let new_call = at(k + 1).is_some_and(|t| is(t, ":"))
                    && at(k + 2).is_some_and(|t| is(t, ":"))
                    && at(k + 3).is_some_and(|t| is(t, "new"));
                if new_call {
                    let allowed = (path == "wisp-start/src/keychain.rs"
                        && live[k].func.as_deref() == Some("keychain"))
                        || path == "wisp-core/src/secrets/import.rs";
                    if !allowed {
                        found.push(format!(
                            "{place}: Variables::new outside keychain() and secrets::import"
                        ));
                    }
                }
                if at(k + 1).is_some_and(|t| is(t, "{"))
                    && path != "wisp-core/src/endpoints/variables.rs"
                {
                    found.push(format!("{place}: a Variables literal"));
                }
            }
            if is(t, "real_env") && t.kind == Kind::Ident {
                let defined =
                    path == "wisp-start/src/paths.rs" && k > 0 && is(&live[k - 1].token, "fn");
                let helper = path == "wisp-start/src/keychain.rs"
                    && (live[k].func.as_deref() == Some("keychain")
                        || (live[k].func.as_deref() == Some("redactions")
                            && live[k].imp.as_deref() == Some("KeychainInputs")));
                let display = path == "wisp-start/src/run.rs"
                    && live[k].func.as_deref() == Some("display_env");
                let caller = CALLERS
                    .iter()
                    .any(|c| path == *c || (c.ends_with('/') && path.starts_with(c)))
                    && callee_before(&live, k).contains("secrets::import::");
                if !(defined || helper || display || caller) {
                    found.push(format!(
                        "{place}: real_env named outside its allowed places"
                    ));
                }
            }
            if is(t, "read_env_file")
                && t.kind == Kind::Ident
                && at(k + 1).is_some_and(|t| is(t, "("))
                && !(k > 0 && is(&live[k - 1].token, "fn"))
            {
                let allowed = (path == "wisp-start/src/keychain.rs"
                    && live[k].func.as_deref() == Some("load")
                    && live[k].imp.as_deref() == Some("KeychainInputs"))
                    || path == "wisp-core/src/secrets/import.rs";
                if !allowed {
                    found.push(format!(
                        "{place}: read_env_file called outside its two callers"
                    ));
                }
            }
            k += 1;
        }
    }
    found
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/variable_guard")
        .join(name)
}

#[test]
fn variables_are_read_only_where_the_guard_allows() {
    let files = roots(|dir| dir != "plugins");
    assert!(files.iter().any(|f| f.path == "wisp-start/src/keychain.rs"));
    assert!(!files.iter().any(|f| f.path.starts_with("syntect")));
    let found = problems(&files);
    assert_eq!(found, Vec::<String>::new());
    let constructions: Vec<&str> = files
        .iter()
        .filter(|f| {
            f.tokens.iter().enumerate().any(|(k, p)| {
                !p.test
                    && is(&p.token, "Variables")
                    && f.tokens.get(k + 3).is_some_and(|n| is(&n.token, "new"))
            })
        })
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(
        constructions,
        [
            "wisp-core/src/secrets/import.rs",
            "wisp-start/src/keychain.rs"
        ]
    );
}

#[test]
fn test_code_is_found_by_the_module_tree() {
    let files = roots(|dir| dir != "plugins");
    let test_of = |path: &str| {
        files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.tokens.iter().all(|p| p.test))
    };
    assert_eq!(test_of("wisp-core/src/agent/settings/live.rs"), Some(true));
    assert_eq!(test_of("wisp-core/src/agent/tool_tests.rs"), Some(true));
    assert_eq!(
        test_of("wisp-core/src/agent/tool_tests/steering.rs"),
        Some(true)
    );
    assert_eq!(test_of("wisp-start/src/presence.rs"), Some(false));
}

#[test]
fn reused_walks_keep_reached_and_unreached_files_in_either_visit_order() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("crates/x/src");
    std::fs::create_dir_all(&src).unwrap();
    for (case, name) in [
        ("fail_reached_module", "reached"),
        ("fail_unreached_file", "stray"),
    ] {
        let source =
            std::fs::read_to_string(fixture(case).join(format!("crates/x/src/{name}.rs"))).unwrap();
        std::fs::write(
            src.join(format!("{name}.rs")),
            format!("impl Fixture {{ {source} #[cfg(test)] fn only_in_tests() {{}} }}"),
        )
        .unwrap();
    }
    let modules = [
        "mod reached;",
        "#[cfg(test)] #[path = \"reached.rs\"] mod test_reached;",
    ];
    let metadata = |p: Placed| {
        (
            p.token.kind,
            p.token.text,
            p.token.line,
            p.token.span,
            p.test,
            p.func,
            p.imp,
        )
    };
    for order in [modules, [modules[1], modules[0]]] {
        std::fs::write(src.join("lib.rs"), order.join("\n")).unwrap();
        let files = scan(root.path(), "crates");
        assert_eq!(
            files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            ["x/src/lib.rs", "x/src/reached.rs", "x/src/stray.rs"]
        );
        for file in files {
            let source =
                std::fs::read_to_string(root.path().join("crates").join(&file.path)).unwrap();
            assert_eq!(
                file.tokens.into_iter().map(metadata).collect::<Vec<_>>(),
                walk(&tokens(&source), false)
                    .placed
                    .into_iter()
                    .map(metadata)
                    .collect::<Vec<_>>(),
                "{}",
                file.path
            );
        }
    }
}

#[test]
fn a_module_path_is_read_behind_any_visibility_attribute_order_or_cfg_attr() {
    let cases: [(&str, &[Option<&str>]); 14] = [
        ("#[path = \"x.rs\"] mod m;", &[Some("x.rs")]),
        ("#[path = \"x.rs\"] pub mod m;", &[Some("x.rs")]),
        ("#[path = \"x.rs\"] pub(crate) mod m;", &[Some("x.rs")]),
        ("#[path = \"x.rs\"] pub(super) mod m;", &[Some("x.rs")]),
        (
            "#[path = \"x.rs\"] pub(in crate::a) mod m;",
            &[Some("x.rs")],
        ),
        (
            "#[allow(dead_code)] #[path = \"x.rs\"] pub(crate) mod m;",
            &[Some("x.rs")],
        ),
        (
            "#[path = \"x.rs\"] #[allow(dead_code)] pub(crate) mod m;",
            &[Some("x.rs")],
        ),
        (
            "#[path = \"x.rs\"] #[cfg(test)] pub(crate) mod m;",
            &[Some("x.rs")],
        ),
        (
            "#[cfg(test)] #[path = \"x.rs\"] pub mod m;",
            &[Some("x.rs")],
        ),
        (
            "#[cfg_attr(test, path = \"x.rs\")] pub(crate) mod m;",
            &[Some("x.rs")],
        ),
        (
            "#[cfg_attr(test, path = \"a.rs\")] #[path = \"b.rs\"] pub(crate) mod m;",
            &[Some("a.rs"), Some("b.rs")],
        ),
        (
            "#[path = \"b.rs\"] #[cfg_attr(test, path = \"a.rs\")] pub mod m;",
            &[Some("b.rs"), Some("a.rs")],
        ),
        (
            "#[cfg_attr(test, foo(some_path = \"a.rs\"))] #[path = \"b.rs\"] mod m;",
            &[Some("b.rs")],
        ),
        ("#[path = \"x.rs\"] pub fn f() {} mod m;", &[None]),
    ];
    for (source, given) in cases {
        let test = source.contains("#[cfg(test)]");
        let expected: Vec<_> = given
            .iter()
            .map(|path| ("m".to_owned(), path.map(str::to_owned), vec![], test))
            .collect();
        assert_eq!(walk(&tokens(source), false).modules, expected, "{source}");
    }
}

#[test]
fn every_module_named_tests_is_test_code() {
    let files = production();
    let named = |path: &str| {
        path.split_once("/src/").is_some_and(|(_, rest)| {
            rest.split('/')
                .any(|p| TEST_MODULES.contains(&p.strip_suffix(".rs").unwrap_or(p)))
        })
    };
    let checked: Vec<&File> = files.iter().filter(|f| named(&f.path)).collect();
    for path in [
        "wisp-core/src/jobs/tests.rs",
        "wisp-core/src/agent/testing.rs",
    ] {
        assert!(checked.iter().any(|f| f.path == path), "{path}");
    }
    let live: Vec<&str> = checked
        .iter()
        .filter(|f| !f.tokens.iter().all(|p| p.test))
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(live, Vec::<&str>::new());
}

const FAILING: [&str; 32] = [
    "fail_other_constant_read",
    "fail_read_file_glob",
    "fail_read_file_module_alias",
    "fail_read_file_core_elsewhere",
    "fail_path_outside_crates",
    "fail_path_outside_src",
    "fail_module_without_file",
    "fail_include",
    "fail_read_file_elsewhere",
    "fail_read_file_use",
    "fail_read_file_other_crate",
    "fail_third_variables_new",
    "fail_variables_literal",
    "fail_vars",
    "fail_vars_os",
    "fail_map_var",
    "fail_use_rename",
    "fail_variables_rename",
    "fail_reached_module",
    "fail_inline_module",
    "fail_unreached_file",
    "fail_provider_literal",
    "fail_real_env_elsewhere",
    "fail_real_env_other_caller",
    "fail_presence_map",
    "fail_presence_is_some",
    "fail_presence_lossy",
    "fail_presence_var",
    "fail_type_alias",
    "fail_read_env_file",
    "fail_use_env",
    "fail_read_env_file_other_load",
];

const PASSING: [&str; 7] = [
    "pass_job_mark",
    "pass_own_read_file",
    "pass_vendor",
    "pass_match_arm",
    "pass_constant",
    "pass_named_callers",
    "pass_nested_test_module",
];

#[test]
fn each_planted_violation_fails_the_guard() {
    for name in FAILING {
        let found = problems(&scan(&fixture(name), "crates"));
        assert_eq!(found.len(), 1, "{name}: {found:?}");
    }
}

#[test]
fn each_allowed_form_passes_the_guard() {
    for name in PASSING {
        let found = problems(&scan(&fixture(name), "crates"));
        assert_eq!(found, Vec::<String>::new(), "{name}");
    }
}
