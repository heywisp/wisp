#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test on any file or pattern error"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use fancy_regex::Regex;
use wisp_core::instructions::{DEFAULT_WARN_TOKENS, Disk, Paths};
use wisp_instructions::{Diagnostic, LoadAll, Loaded, Options, Scope, Source};

const MONTHS: &str = "January|February|March|April|May|June|July|August|September|October|November|December|Sept|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sep|Oct|Nov|Dec";
const SIMPLE_STARTS: &str = "`([{\"'\u{201c}\u{2018}=:<,;|*";

struct Rules {
    year_first: Regex,
    year_last: Regex,
    api_version: Regex,
    month_first: Regex,
    day_first: Regex,
    paths: Vec<(&'static str, Regex)>,
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

static RULES: LazyLock<Rules> = LazyLock::new(|| {
    let joined = r"(?<![A-Za-z0-9])(?<![A-Za-z0-9][-_])";
    Rules {
        year_first: regex(&format!(r"{joined}\d{{4}}([-/])\d{{2}}\1\d{{2}}(?!\d)")),
        year_last: regex(&format!(
            r"{joined}\d{{1,2}}([/.-])\d{{1,2}}\1\d{{4}}(?!\d)"
        )),
        api_version: regex(r#"(?i)[-_]version\s*["']?\s*[:=]\s*["']?\s*$"#),
        month_first: regex(&format!(
            r"(?<![A-Za-z])(?:{MONTHS})(?![A-Za-z])\.?\s+(?:\d{{1,2}}(?:st|nd|rd|th)?|\d{{4}})(?![A-Za-z0-9])"
        )),
        day_first: regex(&format!(
            r"(?<![A-Za-z0-9.])\d{{1,2}}(?:st|nd|rd|th)?\s+(?:{MONTHS})(?![A-Za-z])"
        )),
        paths: vec![
            (
                "a path under a user or machine root",
                regex(r"^/(?:home|Users|root|mnt|media|Volumes|tmp|var|run|private|opt|srv)/\S"),
            ),
            ("a ~user path", regex(r"^~[A-Za-z_]")),
            ("a Windows drive path", regex(r"^[A-Za-z]:[\\/]")),
            (
                "a UNC path",
                regex(r"^\\\\[A-Za-z0-9._-]{2,}\\[A-Za-z0-9._$-]{2,}"),
            ),
        ],
    }
});

fn numeric_date(pattern: &Regex, line: &str) -> bool {
    pattern.find_iter(line).any(|found| {
        let found = found.unwrap();
        !RULES.api_version.is_match(&line[..found.start()]).unwrap()
    })
}

fn date_rule(line: &str) -> Option<&'static str> {
    let rules = &*RULES;
    if numeric_date(&rules.year_first, line) || numeric_date(&rules.year_last, line) {
        return Some("a numeric date");
    }
    if rules.month_first.is_match(line).unwrap() || rules.day_first.is_match(line).unwrap() {
        return Some("a month with a day or year");
    }
    None
}

fn token_starts(line: &str) -> Vec<usize> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut starts = Vec::new();
    for (k, &(at, _)) in chars.iter().enumerate() {
        let before = |n: usize| k.checked_sub(n).map(|j| chars[j].1);
        let start = match before(1) {
            None => true,
            Some(c) if c.is_whitespace() || SIMPLE_STARTS.contains(c) => true,
            Some('@') => before(2).is_none_or(char::is_whitespace),
            Some('>') => before(2)
                .is_none_or(|c| c.is_whitespace() || c.is_ascii_digit() || "&>`".contains(c)),
            Some(_) => line[..at].ends_with("file://"),
        };
        if start {
            starts.push(at);
        }
    }
    starts
}

fn path_rule(line: &str) -> Option<&'static str> {
    token_starts(line).into_iter().find_map(|at| {
        RULES
            .paths
            .iter()
            .find(|(_, rule)| rule.is_match(&line[at..]).unwrap())
            .map(|(name, _)| *name)
    })
}

fn line_rule(line: &str) -> Option<&'static str> {
    date_rule(line).or_else(|| path_rule(line))
}

fn load(start: &Path, home: &Path) -> Loaded {
    let paths = Paths {
        start: start.to_path_buf(),
        config_dir: Some(home.to_path_buf()),
        home: Some(home.to_path_buf()),
    };
    wisp_instructions::load(
        &paths,
        Options {
            source: Source::Auto,
            warn_tokens: DEFAULT_WARN_TOKENS,
        },
        &LoadAll,
        &Disk,
    )
}

fn lint(start: &Path, home: &Path) -> Vec<String> {
    let loaded = load(start, home);
    let mut problems = Vec::new();
    if loaded.summary.chosen.as_ref().map(|c| c.scope) != Some(Scope::Project) {
        problems.push("the project's AGENTS.md is not the chosen source".to_owned());
    }
    for diagnostic in &loaded.summary.diagnostics {
        if let Diagnostic::Problem(problem) = diagnostic {
            let kind = wisp_core::instructions::diagnostic(diagnostic.clone(), None).kind;
            problems.push(format!(
                "{}: {}: {}",
                problem.source.as_deref().unwrap_or("-"),
                kind.as_str(),
                problem.message
            ));
        }
    }
    for file in &loaded.files {
        problems.extend(line_problems(&file.name, &file.text));
    }
    problems
}

fn line_problems<'a>(name: &'a str, text: &'a str) -> impl Iterator<Item = String> + 'a {
    text.lines().enumerate().filter_map(move |(n, line)| {
        line_rule(line).map(|rule| format!("{name}:{}: {rule}: {line}", n + 1))
    })
}

const SKILLS_DIR: &str = ".agents/skills";
const SKILL_NAME_MAX: usize = 64;
const SKILL_DESCRIPTION_MAX: usize = 1_024;
const SKILL_DESCRIPTION_BAD_STARTS: &str = "!\"#%&'*,:>?@[]`{|}-";

static SKILL_NAME: LazyLock<Regex> = LazyLock::new(|| regex(r"^[a-z0-9]+(-[a-z0-9]+)*$"));
static SKILL_MENTION: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\.agents/skills/([a-z0-9-]+)/SKILL\.md"));

fn skill_dirs(root: &Path) -> Vec<(String, PathBuf)> {
    let mut dirs: Vec<(String, PathBuf)> = std::fs::read_dir(root.join(SKILLS_DIR))
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect();
    dirs.sort();
    dirs
}

fn skill_header_problems(dir: &str, header: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut names = Vec::new();
    let mut descriptions = Vec::new();
    for line in header {
        if let Some(name) = line.strip_prefix("name: ") {
            names.push(name);
        } else if let Some(description) = line.strip_prefix("description: ") {
            descriptions.push(description);
        } else {
            problems.push(format!(
                "a header line that is not one name or description: {line}"
            ));
        }
    }
    match names.as_slice() {
        [name] => {
            if *name != dir {
                problems.push(format!("name {name} differs from its directory"));
            }
            if name.len() > SKILL_NAME_MAX || !SKILL_NAME.is_match(name).unwrap() {
                problems.push(format!(
                    "name {name} is not kebab-case of at most 64 characters"
                ));
            }
        }
        _ => problems.push(format!("{} name lines, not one", names.len())),
    }
    match descriptions.as_slice() {
        [description] => {
            let length = description.chars().count();
            if length == 0 || length > SKILL_DESCRIPTION_MAX {
                problems.push(format!("a description of {length} characters"));
            }
            if description.contains(": ")
                || description.contains(" #")
                || description.ends_with(':')
                || description.starts_with(|c| SKILL_DESCRIPTION_BAD_STARTS.contains(c))
            {
                problems.push("a description that is not a plain scalar".to_owned());
            }
        }
        _ => problems.push(format!("{} description lines, not one", descriptions.len())),
    }
    problems
}

fn skill_file_problems(dir: &str, file: &Path) -> Vec<String> {
    if !file.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
        return vec!["no regular SKILL.md".to_owned()];
    }
    let text = std::fs::read_to_string(file).unwrap();
    let Some(rest) = text.strip_prefix("---\n") else {
        return vec!["SKILL.md does not start with ---".to_owned()];
    };
    let lines: Vec<&str> = rest.lines().collect();
    let Some(end) = lines.iter().position(|line| *line == "---") else {
        return vec!["SKILL.md has no closing ---".to_owned()];
    };
    let mut problems = skill_header_problems(dir, &lines[..end]);
    if lines[end + 1..].iter().all(|line| line.trim().is_empty()) {
        problems.push("an empty body".to_owned());
    }
    for (n, line) in lines.iter().enumerate() {
        if let Some(rule) = line_rule(line) {
            problems.push(format!("SKILL.md:{}: {rule}: {line}", n + 2));
        }
    }
    problems
}

fn skill_problems(root: &Path) -> Vec<String> {
    skill_dirs(root)
        .into_iter()
        .flat_map(|(dir, path)| {
            skill_file_problems(&dir, &path.join("SKILL.md"))
                .into_iter()
                .map(move |problem| format!("{dir}: {problem}"))
        })
        .collect()
}

fn workspace_root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

#[test]
fn this_repositorys_instructions_hold_no_dates_or_machine_paths() {
    let home = tempfile::tempdir().unwrap();
    let mut problems = lint(&workspace_root(), home.path());
    for name in ["BRAIN.md", "VOICE.md"] {
        let file = workspace_root()
            .join("flows/interactive_education")
            .join(name);
        let text = std::fs::read_to_string(file).unwrap();
        problems.extend(line_problems(name, &text));
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn every_crate_has_a_row_in_agents_md_and_a_line_in_the_architecture_tree() {
    let root = workspace_root();
    let dirs = |under: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(under)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    };
    let package = |dir: &PathBuf| {
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
        let name = manifest.lines().find_map(|l| l.strip_prefix("name = \""))?;
        Some(name.trim_end_matches('"').to_owned())
    };
    let apps = dirs(&root.join("apps"));
    let webs = apps.iter().map(|app| app.join("web"));
    let mut crates: Vec<String> = dirs(&root.join("crates"))
        .iter()
        .chain(&apps)
        .cloned()
        .chain(webs)
        .filter_map(|dir| package(&dir))
        .collect();
    crates.sort_unstable();
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    let mut rows: Vec<&str> = agents
        .lines()
        .skip_while(|line| *line != "| Crate | Role |")
        .skip(2)
        .take_while(|line| line.starts_with('|'))
        .filter_map(|line| line.strip_prefix("| `")?.split_once('`'))
        .map(|(name, _)| name)
        .collect();
    rows.sort_unstable();
    assert_eq!(rows, crates, "AGENTS.md's crate table");
    let architecture = std::fs::read_to_string(root.join("docs/architecture.md")).unwrap();
    for name in &crates {
        let line = format!("  {name}/ ");
        assert!(
            architecture.lines().any(|l| l.starts_with(&line)),
            "docs/architecture.md: {name}"
        );
    }
}

#[test]
fn this_repositorys_skills_are_well_formed_and_portable() {
    let root = workspace_root();
    let skills = root.join(SKILLS_DIR);
    assert!(skills.is_dir(), "{} is missing", skills.display());
    let problems = skill_problems(&root);
    assert!(problems.is_empty(), "{problems:#?}");
    for (_, folder) in skill_dirs(&root) {
        let text = std::fs::read_to_string(folder.join("SKILL.md")).unwrap();
        let parsed = wisp_instructions::skills::parse(&text).unwrap();
        for (key, actual) in [
            ("name: ", parsed.name),
            ("description: ", parsed.description),
        ] {
            let expected = text
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap();
            assert_eq!(actual, expected);
        }
    }
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    let named: BTreeSet<String> = SKILL_MENTION
        .captures_iter(&agents)
        .map(|found| found.unwrap().get(1).unwrap().as_str().to_owned())
        .collect();
    let present: BTreeSet<String> = skill_dirs(&root)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        named, present,
        "AGENTS.md's skill index against .agents/skills"
    );
    let home = tempfile::tempdir().unwrap();
    let loaded = load(&root, home.path());
    assert!(
        loaded
            .files
            .iter()
            .any(|file| file.name == "CONTRIBUTING.md"),
        "AGENTS.md's @CONTRIBUTING.md import did not load"
    );
    let leaked: Vec<&str> = loaded
        .files
        .iter()
        .filter(|file| file.path.starts_with(&skills) || file.name.contains(SKILLS_DIR))
        .map(|file| file.name.as_str())
        .collect();
    assert!(
        leaked.is_empty(),
        "skills in the system prompt: {leaked:#?}"
    );
}

const DATES_FAIL: &[&str] = &[
    "2026-10-23",
    "(2026-10-23)",
    "Sent: Friday 2026-10-23 14:05",
    "Sent: Friday 2026-10-23 14:05:09",
    "released 2023-06-01",
    "23 Oct 2026",
    "On 23 Oct 2026, the load",
    "October 2026",
    "Oct 23",
    "October 23, 2026",
    "Sept. 4",
    "October 23rd, 2026",
    "23rd October",
    "2026/10/23",
    "_2026-10-23_",
    "*2026-10-23*",
    "10/23/2026",
    "23.10.2026",
    "23-10-2026",
    "Last conversion: 2026-10-23",
    "Docs version: 2026-10-23, see notes",
    "version: 2026-10-23",
];

const DATES_PASS: &[&str] = &[
    "0.19.2",
    "1h",
    "5m",
    "128",
    "mid-conversation-output-config-2026-07-01",
    "v2026-10-23",
    "anthropic-version: 2023-06-01",
    "may 2",
    "october 2026",
    "MARCH 2026",
    "Mayday 2026",
    "Octopus 12",
    "May 5m",
    "1.2.3 Mar",
    "config_2026-10-23",
    "x2026-10-23",
    "1.10.0",
    "v10.23.2026",
    r#""anthropic-version": "2023-06-01""#,
    r#"anthropic_version = "2023-06-01""#,
    "X-Api-Version: 2023-06-01",
];

const PATHS_FAIL: &[&str] = &[
    "/home/alice/notes.md",
    "`/tmp/wisp/out`",
    "(/home/alice/x)",
    "--config=/var/lib/wisp/config.toml",
    "2>/tmp/wisp.log",
    "PATH=$PATH:/home/alice/bin",
    "file:///home/alice/x",
    "/Users/alice/x",
    "/mnt/c/Users/alice",
    "/root/.cargo/bin",
    "sort </tmp/in.txt",
    "a,/home/alice/x",
    "cmd >/tmp/x",
    "&>/tmp/x",
    "x;/home/a/b",
    "a|/tmp/x",
    "{/home/a/b}",
    "\u{201c}/home/a/b\u{201d}",
    "\u{2018}/tmp/a/b\u{2019}",
    r"C:\Users\alice",
    "D:/work",
    r"--out=C:\x",
    r"\\server\share",
    r#""\\host\c$""#,
    "@/home/alice/rules.md",
    ">/tmp/x",
    "*/home/a/b*",
    "[/home/a/b]",
    r#""/home/a/b""#,
    "'/tmp/a/b'",
    "cmd >>/tmp/x",
    "`>/tmp/x`",
    "~alice/notes.md",
    "(~bob/x)",
    "file:///tmp/a/b",
    "~_build/x",
];

const PATHS_PASS: &[&str] = &[
    "docs/endpoints.md",
    "./.env",
    "~/.config/wisp/.env",
    "$HOME/x",
    "--config <scratch>/config.toml",
    "<scratch>/tmp/x",
    "https://example.com/a/b",
    "https://example.com/home/alice",
    "https://x.y/a:b/c",
    "POST /v1/messages",
    "/dev/null",
    "`/dev/null`",
    "2>/dev/null",
    "/usr/bin/env",
    "/etc/hosts",
    "/tmp",
    "/home",
    "</file>",
    "<br/>",
    "//",
    "///",
    "//!",
    "/context",
    "and/or",
    "1/2",
    "**/*.rs",
    r"\\n",
    r"\\",
    r#""\\n\\t""#,
    r#""\\x1b\\[0m""#,
    r"a\\server\share",
    r#""\\n\t""#,
    "@docs/style.md",
    "user@/home/x",
    "maintainer@example.com",
    "~",
    "~/",
    "a~b",
    "~10 minutes",
    "file://example.com/home/a",
];

#[test]
fn the_date_rule_catches_dates_and_lets_names_and_versions_pass() {
    let missed: Vec<&str> = DATES_FAIL
        .iter()
        .copied()
        .filter(|line| date_rule(line).is_none())
        .collect();
    assert!(missed.is_empty(), "not caught: {missed:#?}");
    let caught: Vec<(&str, &str)> = DATES_PASS
        .iter()
        .filter_map(|line| line_rule(line).map(|rule| (*line, rule)))
        .collect();
    assert!(caught.is_empty(), "caught wrongly: {caught:#?}");
}

#[test]
fn the_path_rule_catches_machine_paths_and_lets_portable_ones_pass() {
    let missed: Vec<&str> = PATHS_FAIL
        .iter()
        .copied()
        .filter(|line| path_rule(line).is_none())
        .collect();
    assert!(missed.is_empty(), "not caught: {missed:#?}");
    let caught: Vec<(&str, &str)> = PATHS_PASS
        .iter()
        .filter_map(|line| line_rule(line).map(|rule| (*line, rule)))
        .collect();
    assert!(caught.is_empty(), "caught wrongly: {caught:#?}");
}

struct Repo {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Repo {
    fn new(agents: &str) -> Repo {
        let dir = tempfile::tempdir().unwrap();
        wisp_core::instructions::make_repository(dir.path()).unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), agents).unwrap();
        Repo {
            dir,
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    fn skill_problems(&self) -> Vec<String> {
        skill_problems(self.dir.path())
    }

    fn lint(&self) -> Vec<String> {
        lint(self.dir.path(), self.home.path())
    }
}

#[test]
fn an_import_cannot_carry_a_machine_path_past_the_lint() {
    let repo = Repo::new("Rules.\n@notes.md\n");
    repo.write("notes.md", "keep\nsee /home/alice/x\n");
    let problems = repo.lint();
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].starts_with("notes.md:2: "), "{problems:#?}");
}

#[test]
fn a_missing_import_fails_the_lint() {
    let problems = Repo::new("@gone.md\n").lint();
    assert!(
        problems.iter().any(|p| p.contains("import_missing")),
        "{problems:#?}"
    );
}

#[test]
fn a_large_file_only_warns() {
    let repo = Repo::new(&"Keep the rules short.\n".repeat(2_000));
    assert!(repo.lint().is_empty());
}

#[test]
fn a_home_import_is_missing_under_the_empty_home() {
    let problems = Repo::new("@~/notes.md\n").lint();
    assert!(
        problems.iter().any(|p| p.contains("import_missing")),
        "{problems:#?}"
    );
}

const SKILL_FIXTURE: &str =
    "---\nname: x\ndescription: Does x. Read before x.\n---\n\n# X\n\nBody.\n";

fn skill_repo(skill: &str) -> Repo {
    let repo = Repo::new("Read `.agents/skills/x/SKILL.md`.\n");
    repo.write(".agents/skills/x/SKILL.md", skill);
    repo
}

#[test]
fn a_skill_whose_name_differs_from_its_directory_fails() {
    let problems = skill_repo(&SKILL_FIXTURE.replace("name: x", "name: y")).skill_problems();
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(
        problems[0].contains("differs from its directory"),
        "{problems:#?}"
    );
}

#[test]
fn a_skill_description_that_is_not_a_plain_scalar_fails() {
    let problems = skill_repo(&SKILL_FIXTURE.replace("Does x.", "Does x: y.")).skill_problems();
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].contains("not a plain scalar"), "{problems:#?}");
}

#[test]
fn a_machine_path_in_a_skill_fails() {
    let problems =
        skill_repo(&SKILL_FIXTURE.replace("Body.", "See /home/alice/x.")).skill_problems();
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].starts_with("x: SKILL.md:8: "), "{problems:#?}");
}
