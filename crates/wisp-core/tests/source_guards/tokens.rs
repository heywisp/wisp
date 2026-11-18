use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::purity::sources;

pub(crate) use crate::purity::rust_source::{Kind, Token, is, tokens};
use crate::purity::rust_source::{
    attribute_end, attribute_text, is_attribute, test_ranges, visibility_end,
};

#[derive(Clone, Debug)]
pub(crate) struct Placed {
    pub(crate) token: Token,
    pub(crate) test: bool,
    pub(crate) func: Option<String>,
    pub(crate) imp: Option<String>,
}

pub(crate) struct File {
    pub(crate) path: String,
    pub(crate) tokens: Vec<Placed>,
    pub(crate) escapes: Vec<String>,
}

pub(crate) enum Frame {
    Fn(String),
    Impl(String),
    Mod(bool),
    Other,
}

pub(crate) struct Walked {
    pub(crate) placed: Vec<Placed>,
    pub(crate) modules: Vec<(String, Option<String>, Vec<String>, bool)>,
}

fn path_literals(attr: &str) -> Vec<String> {
    if !attr.starts_with("#[path=") && !attr.starts_with("#[cfg_attr(") {
        return Vec::new();
    }
    attr.match_indices("path=\"")
        .filter(|(at, _)| !attr[..*at].ends_with(|c: char| c.is_alphanumeric() || c == '_'))
        .filter_map(|(at, key)| {
            let (given, _) = attr[at + key.len()..].split_once('"')?;
            Some(given.to_owned())
        })
        .collect()
}

pub(crate) fn walk(tokens: &[Token], file_test: bool) -> Walked {
    let mut placed: Vec<Placed> = tokens
        .iter()
        .map(|t| Placed {
            token: t.clone(),
            test: file_test,
            func: None,
            imp: None,
        })
        .collect();
    for range in test_ranges(tokens, false) {
        for placed in &mut placed[range] {
            placed.test = true;
        }
    }
    let mut modules = Vec::new();
    let mut i = 0;
    let mut attrs: Vec<String> = Vec::new();
    while i < tokens.len() {
        if is_attribute(tokens, i) {
            let end = attribute_end(tokens, i);
            attrs.push(attribute_text(&tokens[i..end]));
            i = end;
            continue;
        }
        let visible = visibility_end(tokens, i);
        if visible != i {
            i = visible;
            continue;
        }
        let mut paths: Vec<Option<String>> = attrs
            .iter()
            .flat_map(|a| path_literals(a))
            .map(Some)
            .collect();
        if paths.is_empty() {
            paths.push(None);
        }
        attrs.clear();
        if is(&tokens[i], "mod")
            && tokens.get(i + 1).is_some_and(|t| t.kind == Kind::Ident)
            && tokens.get(i + 2).is_some_and(|t| is(t, ";"))
        {
            let inline = inline_path(tokens, i);
            let test = placed[i].test;
            for path_attr in paths {
                modules.push((tokens[i + 1].text.clone(), path_attr, inline.clone(), test));
            }
        }
        i += 1;
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut pending: Option<Frame> = None;
    let mut paren = 0i32;
    for (k, t) in tokens.iter().enumerate() {
        if t.kind == Kind::Ident && t.text == "fn" {
            if let Some(name) = tokens.get(k + 1).filter(|n| n.kind == Kind::Ident) {
                pending = Some(Frame::Fn(name.text.clone()));
            }
        } else if t.kind == Kind::Ident && t.text == "impl" && pending.is_none() {
            pending = Some(Frame::Impl(String::new()));
        } else if t.kind == Kind::Ident && t.text == "mod" {
            pending = Some(Frame::Mod(placed[k].test));
        } else if let Some(Frame::Impl(name)) = &mut pending
            && t.kind == Kind::Ident
            && t.text != "for"
            && t.text != "where"
        {
            *name = t.text.clone();
        }
        if is(t, "(") {
            paren += 1;
        } else if is(t, ")") {
            paren -= 1;
        } else if is(t, ";") && paren == 0 {
            pending = None;
        } else if is(t, "{") {
            stack.push(pending.take().unwrap_or(Frame::Other));
        } else if is(t, "}") {
            stack.pop();
        }
        placed[k].func = stack.iter().rev().find_map(|f| match f {
            Frame::Fn(name) => Some(name.clone()),
            _ => None,
        });
        placed[k].imp = stack.iter().rev().find_map(|f| match f {
            Frame::Impl(name) => Some(name.clone()),
            _ => None,
        });
        if stack.iter().any(|f| matches!(f, Frame::Mod(true))) {
            placed[k].test = true;
        }
    }
    Walked { placed, modules }
}

pub(crate) fn inline_path(tokens: &[Token], at: usize) -> Vec<String> {
    let mut path = Vec::new();
    let mut depth = 0i32;
    let mut k = at;
    while k > 0 {
        k -= 1;
        if is(&tokens[k], "}") {
            depth += 1;
        } else if is(&tokens[k], "{") {
            if depth == 0 {
                if k >= 2 && is(&tokens[k - 2], "mod") && tokens[k - 1].kind == Kind::Ident {
                    path.push(tokens[k - 1].text.clone());
                }
            } else {
                depth -= 1;
            }
        }
    }
    path.reverse();
    path
}

pub(crate) fn module_file(
    dir: &Path,
    inline: &[String],
    name: &str,
    path_attr: Option<&str>,
    parent: &Path,
) -> Option<PathBuf> {
    if let Some(given) = path_attr {
        let target = normalized(&parent.parent()?.join(given));
        return target.exists().then_some(target);
    }
    let mut base = dir.to_owned();
    for part in inline {
        base = base.join(part);
    }
    [
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

pub(crate) fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn scan(root: &Path, dir: &str) -> Vec<File> {
    let crates = normalized(&root.join(dir));
    let mut files = Vec::new();
    let mut crate_dirs: Vec<PathBuf> = std::fs::read_dir(&crates)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("src").is_dir())
        .collect();
    crate_dirs.sort();
    for crate_dir in crate_dirs {
        let src = crate_dir.join("src");
        let mut roots: Vec<PathBuf> = ["lib.rs", "main.rs"]
            .iter()
            .map(|n| src.join(n))
            .filter(|p| p.exists())
            .collect();
        let bin = src.join("bin");
        let bins = if bin.is_dir() {
            sources(&bin, "rs", &[], 0)
        } else {
            vec![]
        };
        roots.extend(
            bins.into_iter()
                .filter(|p| p.parent() == Some(bin.as_path())),
        );
        let mut seen: BTreeMap<PathBuf, (bool, Walked)> = BTreeMap::new();
        let mut escapes: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        let mut queue: Vec<(PathBuf, bool)> = roots.into_iter().map(|p| (p, false)).collect();
        while let Some((path, test)) = queue.pop() {
            if seen.get(&path).is_some_and(|(t, _)| !*t || test) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let walked = walk(&tokens(&text), test);
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let rooted = matches!(name, "mod.rs" | "lib.rs" | "main.rs")
                || path.parent() == Some(src.join("bin").as_path());
            let child_dir = if rooted {
                path.parent().unwrap().to_owned()
            } else {
                path.with_extension("")
            };
            for (module, path_attr, inline, module_test) in &walked.modules {
                if let Some(given) = path_attr
                    && !normalized(&path.parent().unwrap().join(given)).starts_with(&crates)
                {
                    escapes
                        .entry(path.clone())
                        .or_default()
                        .push(format!("#[path] module {module} leads outside {dir}/"));
                    continue;
                }
                match module_file(&child_dir, inline, module, path_attr.as_deref(), &path) {
                    Some(found) => queue.push((found, test || *module_test)),
                    None => escapes
                        .entry(path.clone())
                        .or_default()
                        .push(format!("module {module} has no file")),
                }
            }
            seen.insert(path, (test, walked));
        }
        let mut all = sources(&src, "rs", &[], 0);
        all.extend(seen.keys().filter(|p| !p.starts_with(&src)).cloned());
        for path in all {
            let walked = match seen.remove(&path) {
                Some((_, walked)) => walked,
                None => {
                    let text = std::fs::read_to_string(&path).unwrap();
                    walk(&tokens(&text), false)
                }
            };
            let shown = path
                .strip_prefix(&crates)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            files.push(File {
                path: shown,
                tokens: walked.placed,
                escapes: escapes.remove(&path).unwrap_or_default(),
            });
        }
    }
    files
}

pub(crate) fn enclosing(
    tokens: &[Placed],
    at: usize,
    (open, close): (&str, &str),
) -> Option<usize> {
    let mut depth = 0i32;
    for k in (0..at).rev() {
        let t = &tokens[k].token;
        if is(t, close) {
            depth += 1;
        } else if is(t, open) {
            if depth == 0 {
                return Some(k);
            }
            depth -= 1;
        }
    }
    None
}

pub(crate) fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
