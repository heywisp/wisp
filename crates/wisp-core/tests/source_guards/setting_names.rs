use std::collections::BTreeSet;

use wisp_core::plugin::imports;
use wisp_protocol::settings::{PRESETS_GROUP, SETTINGS, find, names, parse_target};

use crate::tokens::{File, Kind, Placed, enclosing, is, repo, scan, tokens, walk};

const PLAIN_CALLS: [&str; 5] = ["find", "get", "on", "saved_global", "touched"];
const NAMES_FILE: &str = "wisp-protocol/src/settings/names.rs";

fn name_grammar(text: &str) -> bool {
    text.split('.').all(|segment| {
        !segment.is_empty()
            && segment.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '<' | '>')
            })
    })
}

fn groups() -> BTreeSet<&'static str> {
    SETTINGS.iter().filter_map(|spec| spec.group()).collect()
}

fn allowed() -> BTreeSet<&'static str> {
    imports::ALL
        .iter()
        .map(|import| import.wit())
        .chain([PRESETS_GROUP])
        .collect()
}

fn text_is(placed: &[Placed], at: Option<usize>, text: &str) -> bool {
    at.and_then(|k| placed.get(k))
        .is_some_and(|p| is(&p.token, text))
}

fn call_argument(placed: &[Placed], at: usize) -> bool {
    let Some(open) = enclosing(placed, at, ("(", ")")) else {
        return false;
    };
    let before = |back: usize| open.checked_sub(back);
    let callee = before(1).and_then(|k| placed.get(k));
    let Some(callee) = callee.filter(|p| p.token.kind == Kind::Ident) else {
        return false;
    };
    let method = text_is(placed, before(2), ".");
    match callee.token.text.as_str() {
        "global" => method,
        "flag_value" => true,
        "new" => {
            let commas = (open + 1..at)
                .filter(|&k| is(&placed[k].token, ",") && list_element(placed, k, open))
                .count();
            text_is(placed, before(2), ":")
                && text_is(placed, before(3), ":")
                && text_is(placed, before(4), "Flag")
                && commas == 1
        }
        "named" => {
            text_is(placed, before(2), ":")
                && text_is(placed, before(3), ":")
                && text_is(placed, before(4), "SettingChange")
        }
        name => !method && PLAIN_CALLS.contains(&name),
    }
}

fn spec_name_ends(placed: &[Placed], at: Option<usize>) -> bool {
    let back = |n: usize| at.and_then(|at| at.checked_sub(n));
    text_is(placed, at, "name")
        && text_is(placed, back(1), ".")
        && (text_is(placed, back(2), "spec")
            || (text_is(placed, back(2), ")")
                && text_is(placed, back(3), "(")
                && text_is(placed, back(4), "spec")))
}

fn compared_to_spec_name(placed: &[Placed], at: usize) -> bool {
    let back = |n: usize| at.checked_sub(n);
    text_is(placed, back(1), "=")
        && (text_is(placed, back(2), "=") || text_is(placed, back(2), "!"))
        && spec_name_ends(placed, back(3))
}

fn arm_of_spec_match(placed: &[Placed], at: usize) -> bool {
    let next = |n: usize| Some(at + n);
    let arm = (text_is(placed, next(1), "=") && text_is(placed, next(2), ">"))
        || text_is(placed, next(1), "|")
        || text_is(placed, next(1), "if");
    arm && enclosing(placed, at, ("{", "}"))
        .is_some_and(|open| spec_name_ends(placed, open.checked_sub(1)))
}

fn setting_initializer(placed: &[Placed], open: usize) -> bool {
    let mut equal = open;
    if text_is(placed, equal.checked_sub(1), "!") && text_is(placed, equal.checked_sub(2), "vec") {
        equal -= 2;
    }
    while equal > 0
        && (text_is(placed, Some(equal - 1), "&") || text_is(placed, Some(equal - 1), "mut"))
    {
        equal -= 1;
    }
    let Some(equal) = equal
        .checked_sub(1)
        .filter(|&k| text_is(placed, Some(k), "="))
    else {
        return false;
    };
    let mut depth = 0;
    for k in (0..equal).rev() {
        let t = &placed[k].token;
        if is(t, "]") {
            depth += 1;
        } else if is(t, "[") {
            depth -= 1;
        } else if depth == 0 {
            if ["let", "const", "static"].iter().any(|s| is(t, s)) {
                let name = k + 1 + usize::from(text_is(placed, Some(k + 1), "mut"));
                return placed.get(name).is_some_and(|p| {
                    p.token.kind == Kind::Ident
                        && p.token.text.to_ascii_lowercase().contains("setting")
                });
            }
            if [";", "=", "{", "}"].iter().any(|s| is(t, s)) {
                break;
            }
        }
    }
    false
}

fn list_element(placed: &[Placed], at: usize, open: usize) -> bool {
    [("(", ")"), ("{", "}"), ("[", "]")]
        .into_iter()
        .filter_map(|pair| enclosing(placed, at, pair))
        .max()
        == Some(open)
}

fn in_name_list(placed: &[Placed], at: usize) -> bool {
    let Some(open) = enclosing(placed, at, ("[", "]")) else {
        return false;
    };
    if !list_element(placed, at, open) {
        return false;
    }
    if setting_initializer(placed, open) {
        return true;
    }
    let end = (at + 1..placed.len())
        .find(|&k| is(&placed[k].token, "]") && enclosing(placed, k, ("[", "]")) == Some(open))
        .unwrap_or(placed.len());
    placed[open + 1..end].windows(4).enumerate().any(|(k, w)| {
        is(&w[0].token, "names")
            && is(&w[1].token, ":")
            && is(&w[2].token, ":")
            && w[3].token.kind == Kind::Ident
            && list_element(placed, open + 1 + k, open)
    })
}

struct Checked {
    line: usize,
    name: String,
    called: bool,
}

fn checked(placed: &[Placed]) -> Vec<Checked> {
    let groups = groups();
    placed
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.test && p.token.kind == Kind::Str && name_grammar(&p.token.text))
        .filter_map(|(k, p)| {
            let grouped = p
                .token
                .text
                .split_once('.')
                .is_some_and(|(group, _)| groups.contains(group));
            let called = call_argument(placed, k)
                || compared_to_spec_name(placed, k)
                || arm_of_spec_match(placed, k);
            (grouped || called || in_name_list(placed, k)).then(|| Checked {
                line: p.token.line,
                name: p.token.text.clone(),
                called,
            })
        })
        .collect()
}

fn known(checked: &Checked, allowed: &BTreeSet<&str>) -> bool {
    let name = checked.name.as_str();
    allowed.contains(name)
        || find(name).is_some()
        || (!checked.called && parse_target(name).is_ok())
}

fn typed_name(name: &str) -> Option<String> {
    find(name)
        .filter(|spec| spec.name == name && !name.contains('<'))
        .map(|_| name.to_uppercase().replace('.', "_"))
}

fn untied_consts(placed: &[Placed]) -> Vec<String> {
    placed
        .windows(6)
        .filter(|w| w[5].token.kind == Kind::Str)
        .filter(|w| typed_name(&w[5].token.text).as_deref() != Some(w[0].token.text.as_str()))
        .map(|w| w[0].token.text.clone())
        .collect()
}

fn problems(path: &str, placed: &[Placed], typed: bool) -> Vec<String> {
    let allowed = allowed();
    checked(placed)
        .into_iter()
        .filter_map(|c| match typed_name(&c.name).filter(|_| typed) {
            Some(constant) => Some(format!(
                "{path}:{}: \"{}\" is a setting name: use names::{constant}",
                c.line, c.name
            )),
            None => (!known(&c, &allowed)).then(|| {
                format!(
                    "{path}:{}: \"{}\" is not a registry setting name",
                    c.line, c.name
                )
            }),
        })
        .collect()
}

pub(crate) fn production() -> Vec<File> {
    roots(|_| true)
}

pub(crate) fn roots(wanted: impl Fn(&str) -> bool) -> Vec<File> {
    let root = repo();
    let apps = std::fs::read_dir(root.join("apps"))
        .unwrap()
        .map(|entry| format!("apps/{}", entry.unwrap().file_name().to_string_lossy()));
    ["crates", "apps"]
        .map(str::to_owned)
        .into_iter()
        .chain(apps)
        .chain(["plugins".to_owned()])
        .filter(|dir| wanted(dir))
        .flat_map(|dir| scan(&root, &dir))
        .collect()
}

#[test]
fn production_setting_names_are_registry_names() {
    assert_eq!(wisp_plugin_time::SETTING, names::TIME);
    let crates = roots(|dir| dir != "plugins");
    let plugins = roots(|dir| dir == "plugins");
    let found: Vec<String> = crates
        .iter()
        .map(|f| (f, f.path != NAMES_FILE))
        .chain(plugins.iter().map(|f| (f, false)))
        .flat_map(|(f, typed)| problems(&f.path, &f.tokens, typed))
        .collect();
    assert_eq!(found, Vec::<String>::new());
    let names = crates.iter().find(|f| f.path == NAMES_FILE).unwrap();
    assert_eq!(untied_consts(&names.tokens), Vec::<String>::new());
    let consts = names.tokens.iter().filter(|p| p.token.kind == Kind::Str);
    let typed = SETTINGS.iter().filter(|s| !s.name.contains('<'));
    assert_eq!(consts.count(), typed.count());
    let instructions = plugins
        .iter()
        .find(|f| f.path == "instructions/src/lib.rs")
        .unwrap();
    assert!(
        checked(&instructions.tokens)
            .iter()
            .any(|c| c.name == "instructions.warn_tokens")
    );
}

fn found_in(body: &str, typed: bool) -> Vec<String> {
    let source = format!("fn planted() {{ {body}; }}");
    problems("planted.rs", &walk(&tokens(&source), false).placed, typed)
}

#[test]
fn each_planted_name_fails_and_each_allowed_form_passes() {
    let failing = [
        r#""display.thme""#,
        r#""plugins.time.enabld""#,
        r#"find("cache_tl")"#,
        r#"get("efort")"#,
        r#"on("debug_cach", false)"#,
        r#"saved_global("cache_tl", c)"#,
        r#"saved.global("efort")"#,
        r#"flag_value(&flags, "modle")"#,
        r#"self.flag_value("modle")"#,
        r#"Flag::new("--model", "modle", "value")"#,
        r#"settings::Flag::new(flag("a", "b"), "modle", v)"#,
        r#"Flag::new("model", "model", "effort")"#,
        r#"touched("efort")"#,
        r#"SettingChange::named("tme", v)"#,
        r#"spec.name == "keychian""#,
        r#"path.spec().name != "pricng""#,
        r#"match spec.name { "cache_tl" => 1, _ => 0 }"#,
        r#"match path.spec().name { "debug_cach" | names::TIME => 1, _ => 0 }"#,
        r#""display.theme""#,
        r#"find("cache_ttl")"#,
        r#"match spec.name { "cache_ttl" => 1, _ => 0 }"#,
        r#"match spec.name { "alaises" if !row.aliases.is_empty() => 1, _ => 0 }"#,
        r#"match path.spec().name { names::TIME | "efort" if enabled => 1, _ => 0 }"#,
        r#"[names::MODEL, "efort"]"#,
        r#"&["modle", names::EFFORT]"#,
        r#"[names::MODEL, "effort"]"#,
        r#"let settings = ["modle"]"#,
        r#"let settings = vec!["modle"]"#,
        r#"const KEPT_SETTINGS: [&str; 1] = ["modle"]"#,
        r#"static SETTINGS: &[&str] = &["efort"]"#,
        r#"let mut setting_names: &[&str] = &["modle"]"#,
        r#"let settings = ["modle"]; let fields = ["unrelated"]"#,
    ];
    for body in failing {
        assert_eq!(found_in(body, true).len(), 1, "{body}");
    }
    assert_eq!(
        found_in(r#""display.theme""#, true),
        ["planted.rs:1: \"display.theme\" is a setting name: use names::DISPLAY_THEME"]
    );
    let passing = [
        r#"find(names::CACHE_TTL)"#,
        r#"flag_value(&flags, names::MODEL); self.flag_value(names::MODEL)"#,
        r#"Flag::new("model", names::MODEL, "effort")"#,
        r#"Flag::new(flag("a", "b"), names::MODEL, text("value"))"#,
        r#"touched(names::EFFORT); self.touched()"#,
        r#"match spec.name { names::ALIASES if !row.aliases.is_empty() => 1, _ => 0 }"#,
        r#"[names::MODEL, names::EFFORT]"#,
        r#"let settings = vec![names::MODEL]"#,
        r#"let fields = vec!["modle"]"#,
        r#"const KEPT_SETTINGS: [&str; 1] = [names::MODEL]"#,
        r#"let settings = [names::MODEL]; let fields = ["unrelated"]"#,
        r#"["model", "reasoning", "tools", "instructions"]"#,
        r#"["modle"]"#,
        r#"let settings = [names::MODEL]; fields = ["unrelated"]"#,
        r#"[names::MODEL, "plugins.time.enabled"]"#,
        r#"[SettingSpec::new(names::MODEL, "value")]"#,
        r#"let settings = [SettingSpec { name: names::MODEL, default: "value" }]"#,
        r#"[SettingSpec::new(names::MODEL, "value"), "unrelated"]"#,
        r#"[[names::MODEL], "unrelated"]"#,
        r#"match spec.name { names::CACHE_TTL => 1, _ => 0 }"#,
        r#""plugins.<id>.enabled""#,
        r#""plugins.time.enabled""#,
        r#""agents.presets""#,
        r#""agents.report""#,
        r#""context.current-fill""#,
        r#""agents.{a}.{leaf}""#,
        r#"find("\u{1b}[c")"#,
        r#"tools.find("tmux")"#,
        r#"match name.as_str() { "system" => 1, "lead" => 2, _ => 0 }"#,
    ];
    for body in passing {
        assert_eq!(found_in(body, true), Vec::<String>::new(), "{body}");
    }
    assert_eq!(found_in(r#""display.theme""#, false), Vec::<String>::new());
    assert_eq!(found_in(r#""display.thme""#, false).len(), 1);
    let untied = |source: &str| untied_consts(&walk(&tokens(source), false).placed);
    assert_eq!(
        untied(r#"pub const PRICING: &str = "pricing";"#),
        Vec::<String>::new()
    );
    assert_eq!(
        untied(r#"pub const PRICNG: &str = "pricing"; pub const GONE: &str = "gone";"#),
        ["PRICNG", "GONE"]
    );
    let mixed = "#[allow(dead_code)] #[cfg (test)] pub(crate) fn t() { find(\"nope\"); }\nfind(\"cache_tl\");";
    assert_eq!(
        problems("planted.rs", &walk(&tokens(mixed), false).placed, true),
        ["planted.rs:2: \"cache_tl\" is not a registry setting name"]
    );
    let in_tests = r#"#[cfg(test)] mod tests { fn t() { find("nope"); } }"#;
    assert_eq!(
        problems("planted.rs", &walk(&tokens(in_tests), false).placed, true),
        Vec::<String>::new()
    );
}
