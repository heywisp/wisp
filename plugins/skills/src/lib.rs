mod scan;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use wisp_plugin_api::commands::{CommandLine, CommandReply, CommandRow};
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::index::{Entry, Kind};
use wisp_plugin_api::slots::{TextSpan, Tone};
use wisp_plugin_api::tools::{BuildContext, DoctorCell, DoctorRow, ToolDef};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::paths::shorten_path;

use scan::Scan;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");
pub use wisp_plugin_api::index::SEARCH_CAPABILITY;
pub const TURNED_OFF: &str = "turned_off";
const MIN_NUDGE_SCORE: f64 = 1.5;

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate: |host| {
            Arc::new(Skills {
                host,
                state: Mutex::default(),
            })
        },
    }
}

#[derive(Default)]
struct State {
    loaded: Option<Scan>,
    opened: BTreeSet<String>,
    search: bool,
}

struct Skills {
    host: Arc<dyn Host>,
    state: Mutex<State>,
}

impl Scan {
    fn entries(&self) -> Vec<Entry> {
        self.skills
            .values()
            .map(|skill| Entry {
                kind: Kind::Skill,
                name: skill.text.name.clone(),
                summary: wisp_index::first_line(&skill.text.description).to_owned(),
                text: skill.text.description.clone(),
            })
            .collect()
    }
}

impl Skills {
    fn turned_off(&self) -> BTreeSet<String> {
        self.host
            .get(TURNED_OFF)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn refresh(&self) -> bool {
        let loaded = scan::load_all(self.host.as_ref());
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let old = state.loaded.take();
        let changed = old.as_ref() != Some(&loaded);
        state.opened.retain(|name| {
            old.as_ref().and_then(|old| old.skills.get(name)) == loaded.skills.get(name)
        });
        state.loaded = Some(loaded);
        changed
    }

    fn ensure_loaded(&self) {
        let missing = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .loaded
            .is_none();
        if missing {
            self.refresh();
            self.host.index().changed();
        }
    }

    fn nudge(&self, typed: &str) -> Reply {
        let off = self.turned_off();
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.search {
            return Reply::None;
        }
        let entries = state
            .loaded
            .as_ref()
            .map_or_else(Vec::new, Scan::entries)
            .into_iter()
            .filter(|entry| !state.opened.contains(&entry.name) && !off.contains(&entry.name))
            .collect();
        let hits = wisp_index::Index::new(entries).search_above(
            typed,
            Some(Kind::Skill),
            Some(3),
            MIN_NUDGE_SCORE,
        );
        if hits.is_empty() {
            Reply::None
        } else {
            Reply::Append(vec![format!(
                "Skills that may fit: {}. Read one with use.",
                hits.iter()
                    .map(|hit| hit.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )])
        }
    }
}

impl Plugin for Skills {
    fn describe<'a>(&'a self, ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .search = ctx
            .capabilities
            .iter()
            .any(|capability| capability == SEARCH_CAPABILITY);
        Box::pin(async { Vec::new() })
    }

    fn entries(&self) -> BoxFuture<'_, Vec<Entry>> {
        self.refresh();
        let entries = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .loaded
            .as_ref()
            .map_or_else(Vec::new, Scan::entries);
        Box::pin(async move { entries })
    }

    fn open_entry<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, String>> {
        self.ensure_loaded();
        Box::pin(async move {
            let home = self.host.home();
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let skill = state
                .loaded
                .as_ref()
                .and_then(|loaded| loaded.skills.get(name))
                .ok_or_else(|| format!("no skill named `{name}` was found"))?;
            if state.opened.contains(name) {
                return Ok(wisp_tool_text::index::already_in_view(name));
            }
            let files = scan::file_names(&skill.folder).map_err(|error| error.to_string())?;
            let folder = shorten_path(&skill.folder, home.as_deref().map(Path::new));
            let result = format!(
                "Skill: {name}\nFolder: {folder}\nFiles: {}\n\n{}",
                files.join(", "),
                skill.text.body
            );
            state.opened.insert(name.to_owned());
            Ok(result)
        })
    }

    fn run_command<'a>(&'a self, name: &'a str, _args: &'a str) -> BoxFuture<'a, CommandReply> {
        if name != "skills" {
            return Box::pin(async { CommandReply::None });
        }
        self.ensure_loaded();
        let off = self.turned_off();
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let skills: Vec<_> = state
            .loaded
            .iter()
            .flat_map(|scan| scan.skills.values())
            .collect();
        let name_width = skills
            .iter()
            .map(|skill| skill.text.name.chars().count())
            .max()
            .unwrap_or(0);
        let source_width = skills
            .iter()
            .map(|skill| skill.origin.len())
            .max()
            .unwrap_or(0);
        let mut title = span("Skills", Tone::Text);
        title.bold = true;
        let mut lines = vec![CommandLine::Row(CommandRow {
            left: vec![title],
            right: vec![span(skills.len().to_string(), Tone::Text)],
        })];
        for skill in &skills {
            let word = if off.contains(&skill.text.name) {
                span("off", Tone::Muted)
            } else {
                span("on", Tone::Ok)
            };
            lines.push(CommandLine::Row(CommandRow {
                left: vec![
                    span(format!("{:<name_width$}  ", skill.text.name), Tone::Text),
                    span(format!("{:<source_width$}   ", skill.origin), Tone::Muted),
                    word,
                ],
                right: Vec::new(),
            }));
            lines.push(CommandLine::Row(CommandRow {
                left: vec![span(format!("  {}", skill.text.description), Tone::Muted)],
                right: Vec::new(),
            }));
        }
        if skills.is_empty() {
            lines.push(CommandLine::Text(vec![span("none yet: put a skill's folder in .agents/skills/ here, or in ~/.agents/skills/ for every project", Tone::Text)]));
        }
        if skills.iter().any(|skill| off.contains(&skill.text.name)) {
            lines.push(CommandLine::Text(Vec::new()));
            lines.push(CommandLine::Text(vec![span(
                wisp_tool_text::manage::TURN_ON_WHERE,
                Tone::Muted,
            )]));
        }
        if !state.search {
            lines.push(CommandLine::Text(Vec::new()));
            lines.push(CommandLine::Text(vec![span(
                wisp_tool_text::index::SEARCH_OFF,
                Tone::Text,
            )]));
        }
        Box::pin(async move { CommandReply::Lines(lines) })
    }

    fn on_hook<'a>(&'a self, hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        let reply = match (hook, event) {
            ("scan", Event::BeforeTurn(_)) => {
                if self.refresh() {
                    self.host.index().changed();
                }
                let search = self
                    .host
                    .current()
                    .capabilities
                    .iter()
                    .any(|capability| capability == SEARCH_CAPABILITY);
                self.state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .search = search;
                Reply::None
            }
            ("nudge", Event::BeforeTurn(info)) => self.nudge(&info.typed),
            ("compacted", Event::AfterCompact(info)) if info.failed.is_none() => {
                self.state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .opened
                    .clear();
                Reply::None
            }
            ("rewound", Event::Rewound(_)) => {
                self.state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .opened
                    .clear();
                Reply::None
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    }

    fn doctor<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
        let loaded = scan::load_all(self.host.as_ref());
        let home = self.host.home();
        let home = home.as_deref().map(Path::new);
        let mut rows: Vec<_> = loaded
            .skills
            .values()
            .map(|skill| DoctorRow {
                key: format!("skill {}", skill.text.name),
                cells: vec![
                    DoctorCell::Text(skill.origin.to_owned()),
                    DoctorCell::Text(shorten_path(&skill.folder, home)),
                    DoctorCell::Text("on".to_owned()),
                ],
            })
            .collect();
        rows.extend(loaded.problems.into_iter().map(|problem| DoctorRow {
            key: "skill".to_owned(),
            cells: vec![
                DoctorCell::Text(shorten_path(&problem.path, home)),
                DoctorCell::Text(problem.message),
            ],
        }));
        Box::pin(async move { rows })
    }
}

fn span(text: impl Into<String>, tone: Tone) -> TextSpan {
    TextSpan {
        text: text.into(),
        tone,
        bold: false,
    }
}

#[cfg(test)]
mod tests;
