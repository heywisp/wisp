use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "SKILL.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    pub root: PathBuf,
    pub origin: &'static str,
    pub folder: PathBuf,
    pub text: Result<Parsed, String>,
}

pub fn read(
    folder: &Path,
    mut read_text: impl FnMut(&Path) -> io::Result<String>,
) -> Result<Parsed, String> {
    read_text(&folder.join(FILE_NAME))
        .map_err(|error| error.to_string())
        .and_then(|text| parse(&text))
}

pub fn walk(
    folders: impl IntoIterator<Item = (PathBuf, &'static str)>,
    mut list: impl FnMut(&Path) -> io::Result<Vec<Entry>>,
    mut read_text: impl FnMut(&Path) -> io::Result<String>,
) -> Vec<Result<Visit, Problem>> {
    let mut visits = Vec::new();
    for (root, origin) in folders {
        let mut entries = match list(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                visits.push(Err(Problem {
                    path: root,
                    message: error.to_string(),
                }));
                continue;
            }
        };
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        for entry in entries {
            if !entry.is_dir || entry.path.file_name().is_none_or(|name| !visible(name)) {
                continue;
            }
            let text = read(&entry.path, &mut read_text);
            visits.push(Ok(Visit {
                root: root.clone(),
                origin,
                folder: entry.path,
                text,
            }));
        }
    }
    visits
}

pub fn config_folder(config: &str) -> PathBuf {
    Path::new(config).join("skills")
}

pub fn folders(
    root: &str,
    home: Option<&str>,
    config: Option<&str>,
) -> Vec<(PathBuf, &'static str)> {
    let mut folders = Vec::new();
    if !root.is_empty() {
        folders.push((Path::new(root).join(".agents/skills"), "project"));
    }
    if let Some(home) = home {
        folders.push((Path::new(home).join(".agents/skills"), "yours"));
    }
    if let Some(config) = config {
        folders.push((config_folder(config), "yours"));
    }
    folders
}

pub fn name_problem(folder: &Path, text: &Parsed) -> Option<String> {
    (folder.file_name() != Some(OsStr::new(&text.name)))
        .then(|| format!("name `{}` differs from its folder", text.name))
}

pub fn visible(name: &OsStr) -> bool {
    !name.to_string_lossy().starts_with('.')
}

pub fn included(name: &OsStr) -> bool {
    visible(name) && name != "node_modules"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub name: String,
    pub description: String,
    pub body: String,
}

pub fn parse(text: &str) -> Result<Parsed, String> {
    parse_header(text, false).map(|(parsed, _)| parsed)
}

pub fn parse_with_saved(text: &str) -> Result<(Parsed, Option<String>), String> {
    parse_header(text, true)
}

fn parse_header(text: &str, with_saved: bool) -> Result<(Parsed, Option<String>), String> {
    let mut lines = text.trim_start_matches('\u{feff}').split_inclusive('\n');
    if lines.next().is_none_or(|line| line.trim() != "---") {
        return Err("SKILL.md needs frontmatter between --- lines".to_owned());
    }
    let mut header = Vec::new();
    loop {
        match lines.next() {
            Some(line) if line.trim() == "---" => break,
            Some(line) => header.push(line.trim_end_matches(['\r', '\n'])),
            None => return Err("the frontmatter has no closing --- line".to_owned()),
        }
    }
    let mut name = None;
    let mut description = None;
    let mut saved = None;
    let mut fields = header.into_iter().peekable();
    while let Some(line) = fields.next() {
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with(char::is_whitespace)
        {
            continue;
        }
        let (key, text) = line
            .split_once(':')
            .ok_or_else(|| "frontmatter needs key: value fields".to_owned())?;
        let target = match key.trim() {
            "name" => &mut name,
            "description" => &mut description,
            "saved" if with_saved => &mut saved,
            _ => continue,
        };
        let text = text.trim();
        let value = if matches!(text, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
            let mut block = Vec::new();
            while let Some(line) = fields
                .peek()
                .copied()
                .filter(|line| line.trim().is_empty() || line.starts_with(char::is_whitespace))
            {
                block.push(line.trim());
                fields.next();
            }
            block.join(if text.starts_with('>') { " " } else { "\n" })
        } else {
            scalar(text)?
        };
        *target = Some(value);
    }
    let required = |value: Option<String>, field: &str| {
        value
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("the frontmatter has no {field}"))
    };
    Ok((
        Parsed {
            name: required(name, "name")?,
            description: required(description, "description")?,
            body: lines.collect(),
        },
        saved,
    ))
}

fn scalar(text: &str) -> Result<String, String> {
    match text.chars().next() {
        Some(quote @ ('\'' | '"')) => {
            let end = text
                .rfind(quote)
                .filter(|end| *end > 0)
                .ok_or_else(|| "a frontmatter quote is not closed".to_owned())?;
            let tail = text[end + 1..].trim();
            if !tail.is_empty() && !tail.starts_with('#') {
                return Err("unexpected text after a frontmatter quote".to_owned());
            }
            if quote == '\'' {
                Ok(text[1..end].replace("''", "'"))
            } else {
                serde_json::from_str(&text[..=end])
                    .map_err(|_| "invalid quoted frontmatter text".to_owned())
            }
        }
        Some('[' | '{') => Err("name and description must be text".to_owned()),
        _ => Ok(text
            .split_once(" #")
            .map_or(text, |(value, _)| value)
            .trim()
            .to_owned()),
    }
}
