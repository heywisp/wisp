use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use wisp_plugin_api::host::Host;

use wisp_instructions::skills::{self, Parsed, Problem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Skill {
    pub folder: PathBuf,
    pub origin: &'static str,
    pub text: Parsed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Scan {
    pub skills: BTreeMap<String, Skill>,
    pub problems: Vec<Problem>,
}

pub(crate) fn load_all(host: &dyn Host) -> Scan {
    let mut found = Scan::default();
    let visits = skills::walk(
        skills::folders(
            &host.root(),
            host.home().as_deref(),
            host.config_dir().as_deref(),
        ),
        |root| {
            fs::read_dir(root)?
                .map(|entry| {
                    entry.map(|entry| skills::Entry {
                        path: entry.path(),
                        is_dir: entry.path().is_dir(),
                    })
                })
                .collect()
        },
        |file| fs::read_to_string(file),
    );
    for visit in visits {
        let visit = match visit {
            Ok(visit) => visit,
            Err(problem) => {
                found.problems.push(problem);
                continue;
            }
        };
        let file = visit.folder.join(skills::FILE_NAME);
        let text = match visit.text {
            Ok(text) => text,
            Err(message) => {
                found.problems.push(Problem {
                    path: file,
                    message,
                });
                continue;
            }
        };
        if let Some(message) = skills::name_problem(&visit.folder, &text) {
            found.problems.push(Problem {
                path: file.clone(),
                message,
            });
        }
        if found.skills.contains_key(&text.name) {
            found.problems.push(Problem {
                path: file,
                message: format!(
                    "`{}` is already supplied by an earlier folder; this copy is skipped",
                    text.name
                ),
            });
            continue;
        }
        found.skills.insert(
            text.name.clone(),
            Skill {
                folder: visit.folder,
                origin: visit.origin,
                text,
            },
        );
    }
    found
}

const MAX_FILE_NAMES: usize = 50;

pub(crate) fn file_names(folder: &Path) -> std::io::Result<Vec<String>> {
    let mut files = Vec::new();
    let mut pending = vec![folder.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if !skills::included(&name) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else if let Ok(relative) = entry.path().strip_prefix(folder) {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    if files.len() > MAX_FILE_NAMES {
        let remaining = files.len() - MAX_FILE_NAMES;
        files.truncate(MAX_FILE_NAMES);
        files.push(format!("and {remaining} more"));
    }
    Ok(files)
}
