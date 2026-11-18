use std::time::Duration;

use wisp_protocol::filter::Query;
use wisp_protocol::settings::takes_no_effort;
use wisp_render::chrome::{Facts, Menu, Suggestion, Suggestions, short_model};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::copy;

use super::commands::{
    COMMANDS, Command, Entry, PluginCommand, Source, accepted, is_word, matched, matching_entries,
    names, synopsis,
};
use super::picker::{Picker, Value, effort_levels};
use super::settings::Tracker;
use crate::app::list::ListCursor;
use crate::term::{Key, KeyCode};

pub fn command_word(text: &str, attachments: bool, at_end: bool) -> Option<&str> {
    let word = typed_line(text, attachments, at_end)?;
    (word.is_empty() || is_word(word)).then_some(word)
}

pub fn typed_line(text: &str, attachments: bool, at_end: bool) -> Option<&str> {
    if attachments || !at_end || text.contains(['\n', '\r']) {
        return None;
    }
    text.strip_prefix('/')
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage<'a> {
    Commands(&'a str),
    Unknown(&'a str),
    Values { command: Entry, word: &'a str },
}

pub fn stage<'a>(
    registry: &'static [Command],
    plugins: &[PluginCommand],
    line: &'a str,
) -> Option<Stage<'a>> {
    let Some((name, word)) = line.split_once(char::is_whitespace) else {
        return (line.is_empty() || is_word(line)).then_some(Stage::Commands(line));
    };
    if !is_word(name) {
        return None;
    }
    let built_in = registry
        .iter()
        .find(|c| accepted(c).any(|n| n.eq_ignore_ascii_case(name)))
        .map(Entry::BuiltIn);
    let found = built_in.or_else(|| {
        plugins
            .iter()
            .find(|c| c.base_of(name).is_some())
            .cloned()
            .map(Entry::Plugin)
    });
    let Some(command) = found else {
        return Some(Stage::Unknown(name));
    };
    if !command.takes_args() || word.contains(char::is_whitespace) {
        return None;
    }
    Some(Stage::Values { command, word })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub word: String,
    pub commands: Vec<Entry>,
    pub selected: Option<usize>,
}

impl Listing {
    pub fn highlighted(&self) -> Option<&Entry> {
        self.selected.and_then(|i| self.commands.get(i))
    }

    pub fn view(&self, busy: bool) -> Suggestions {
        Suggestions {
            rows: self
                .commands
                .iter()
                .map(|command| suggestion(command, &self.word, busy))
                .collect(),
            selected: self.selected,
            empty: copy::no_command_matches(&self.word),
        }
    }
}

fn suggestion(entry: &Entry, word: &str, busy: bool) -> Suggestion {
    let hit = if word.is_empty() {
        None
    } else {
        matched(entry, word)
    };
    let (shown, args, summary) = match entry {
        Entry::BuiltIn(command) => (
            names(command).map(str::to_owned).collect(),
            synopsis(command)
                .strip_prefix(&format!("/{}", command.name))
                .unwrap_or_default()
                .to_owned(),
            command.summary.to_owned(),
        ),
        Entry::Plugin(command) => (
            command.shown(hit.as_ref().is_some_and(|hit| hit.contains(':'))),
            command.synopsis_args(),
            command.summary.clone(),
        ),
    };
    let mut name = String::new();
    let mut bold = 0..0;
    for (i, each) in shown.iter().enumerate() {
        if i > 0 {
            name.push_str(", ");
        }
        name.push('/');
        if hit.as_ref() == Some(each) {
            bold = name.len()..name.len() + word.len();
        }
        name.push_str(each);
    }
    let refused = busy && entry.between_turns();
    Suggestion {
        name,
        bold,
        args,
        summary: if refused {
            copy::BETWEEN_TURNS.to_owned()
        } else {
            summary
        },
        enabled: !refused,
    }
}

fn typed_in_full(entry: &Entry, word: &str) -> bool {
    entry
        .names()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(word))
}

pub fn completion(entry: &Entry, word: &str) -> String {
    let name = entry.word_for(word);
    if entry.takes_args() {
        format!("/{name} ")
    } else {
        format!("/{name}")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ValuesView {
    Menu(Box<Menu>),
    List(Suggestions),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub line: String,
    pub word: String,
    pub values: Vec<Value>,
    pub closed: bool,
    pub selected: Option<usize>,
    pub view: ValuesView,
}

impl Values {
    pub fn highlighted(&self) -> Option<&Value> {
        self.selected.and_then(|i| self.values.get(i))
    }

    pub fn with(&self, value: &str) -> String {
        let head = self
            .line
            .strip_suffix(self.word.as_str())
            .unwrap_or(&self.line);
        format!("/{head}{value}")
    }

    pub fn enabled(&self, text: &str) -> Option<&Value> {
        self.values.iter().find(|v| v.enabled && v.text == text)
    }

    fn first_enabled_match(&self) -> Option<usize> {
        (0..self.values.len()).find(|&i| self.values[i].enabled && self.values[i].matches)
    }

    fn first_match(&self, down: bool) -> Option<usize> {
        let enabled = || (0..self.values.len()).filter(|&i| self.values[i].enabled);
        let matching = || enabled().filter(|&i| self.values[i].matches);
        if down {
            matching().next().or_else(|| enabled().next())
        } else {
            matching().next_back().or_else(|| enabled().next_back())
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Open {
    Commands(Listing),
    Values(Values),
}

pub struct Context<'a> {
    pub facts: &'a Facts,
    pub tracker: &'a Tracker,
    pub at: Duration,
    pub glyphs: GlyphSet,
    pub sections: Vec<(String, String)>,
}

fn word_rows(words: &[(&str, &str)], word: &str) -> (Vec<Value>, Suggestions) {
    let typed = word.to_ascii_lowercase();
    let values = words
        .iter()
        .map(|(text, _)| Value {
            text: (*text).to_owned(),
            label: (*text).to_owned(),
            enabled: true,
            matches: text.starts_with(&typed),
        })
        .collect();
    let rows = words
        .iter()
        .map(|(text, summary)| Suggestion {
            name: (*text).to_owned(),
            bold: if !typed.is_empty() && text.starts_with(&typed) {
                0..typed.len()
            } else {
                0..0
            },
            args: String::new(),
            summary: (*summary).to_owned(),
            enabled: true,
        })
        .collect();
    (
        values,
        Suggestions {
            rows,
            selected: None,
            empty: String::new(),
        },
    )
}

fn setting_rows(word: &str) -> (Vec<Value>, Suggestions) {
    let typed = word.to_lowercase();
    let query = Query::new(word);
    let listed: Vec<(&'static str, &'static str)> = super::config_screen::named_settings()
        .map(|spec| (spec.name, spec.label))
        .filter(|(name, label)| name.starts_with(&typed) || query.matches(&[label]))
        .collect();
    let values = listed
        .iter()
        .map(|(name, _)| Value {
            text: (*name).to_owned(),
            label: (*name).to_owned(),
            enabled: true,
            matches: true,
        })
        .collect();
    let rows = listed
        .iter()
        .map(|(name, label)| Suggestion {
            name: (*name).to_owned(),
            bold: if !typed.is_empty() && name.starts_with(&typed) {
                0..typed.len()
            } else {
                0..0
            },
            args: String::new(),
            summary: (*label).to_owned(),
            enabled: true,
        })
        .collect();
    (
        values,
        Suggestions {
            rows,
            selected: None,
            empty: super::config_screen::no_setting_matches(word),
        },
    )
}

fn note(text: String) -> ValuesView {
    ValuesView::List(Suggestions {
        rows: vec![],
        selected: None,
        empty: text,
    })
}

fn picked(picker: &Picker, cx: &Context) -> (Vec<Value>, bool, ValuesView) {
    let menu = picker.menu(cx.facts, cx.tracker, cx.at, cx.glyphs);
    (
        picker.values(cx.facts, cx.tracker),
        picker.is_closed(),
        ValuesView::Menu(Box::new(menu)),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Pass,
    Stay,
    Close,
    Complete(String),
    Run(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggest {
    registry: &'static [Command],
    plugins: Vec<PluginCommand>,
    live: bool,
    line: Option<String>,
    dismissed: Option<String>,
    chosen: Option<String>,
}

impl Default for Suggest {
    fn default() -> Suggest {
        Suggest::new(COMMANDS)
    }
}

impl Suggest {
    pub fn new(registry: &'static [Command]) -> Suggest {
        Suggest {
            registry,
            plugins: Vec::new(),
            live: false,
            line: None,
            dismissed: None,
            chosen: None,
        }
    }

    pub fn stage<'a>(&self, line: &'a str) -> Option<Stage<'a>> {
        stage(self.registry, &self.plugins, line)
    }

    pub fn plugins(&self) -> &[PluginCommand] {
        &self.plugins
    }

    pub fn set_plugins(&mut self, plugins: Vec<PluginCommand>) {
        self.plugins = plugins;
    }

    pub fn edited(&mut self, line: Option<&str>) {
        self.live = true;
        if self.line.as_deref() != line {
            self.line = line.map(str::to_owned);
            self.chosen = None;
        }
        if self.dismissed.as_deref() != line {
            self.dismissed = None;
        }
    }

    pub fn reset(&mut self) {
        self.live = false;
        self.line = None;
        self.dismissed = None;
        self.chosen = None;
    }

    pub fn quiet(&mut self) {
        self.live = false;
    }

    pub fn dismiss(&mut self, line: &str) {
        self.dismissed = Some(line.to_owned());
    }

    pub fn shows(&self, line: &str) -> bool {
        self.live && self.dismissed.as_deref() != Some(line)
    }

    fn chosen_for(&self, line: &str) -> Option<&str> {
        self.chosen
            .as_deref()
            .filter(|_| self.line.as_deref() == Some(line))
    }

    pub fn open(&self, line: &str, cx: &Context) -> Option<Open> {
        match self.stage(line)? {
            Stage::Commands(word) => Some(Open::Commands(self.listing(word))),
            Stage::Unknown(word) => Some(Open::Commands(Listing {
                word: word.to_owned(),
                commands: vec![],
                selected: None,
            })),
            Stage::Values { command, word } => {
                self.values(line, &command, word, cx).map(Open::Values)
            }
        }
    }

    pub fn values(&self, line: &str, entry: &Entry, word: &str, cx: &Context) -> Option<Values> {
        let command = match entry {
            Entry::BuiltIn(command) => command,
            Entry::Plugin(command) => return self.plugin_values(line, command, word, cx),
        };
        let arg = command.args.first()?;
        let (values, closed, view) = match arg.source {
            Source::Models => picked(&Picker::models().queried(word), cx),
            Source::Endpoints => picked(&Picker::endpoints().queried(word), cx),
            Source::Efforts => {
                let model = cx.tracker.wanted().model;
                if takes_no_effort(effort_levels(cx.facts, &model)) {
                    let text = copy::no_effort_levels(&short_model(&model));
                    (vec![], true, note(text))
                } else {
                    picked(&Picker::efforts(model, false, None).queried(word), cx)
                }
            }
            Source::Words(words) => {
                let (values, list) = word_rows(words, word);
                (values, true, ValuesView::List(list))
            }
            Source::Sidebar(words) => {
                let listed: Vec<(&str, &str)> = words
                    .iter()
                    .copied()
                    .chain(cx.sections.iter().map(|(n, t)| (n.as_str(), t.as_str())))
                    .collect();
                let (values, list) = word_rows(&listed, word);
                (values, true, ValuesView::List(list))
            }
            Source::Free => {
                let separator = Glyphs::of(cx.glyphs).separator;
                let text = copy::optional_argument(arg.name, separator);
                (vec![], false, note(text))
            }
            Source::Settings => {
                let (values, list) = setting_rows(word);
                (values, false, ValuesView::List(list))
            }
        };
        Some(self.finish_values(line, word, values, closed, view))
    }

    fn plugin_values(
        &self,
        line: &str,
        command: &PluginCommand,
        word: &str,
        cx: &Context,
    ) -> Option<Values> {
        let arg = command.args.first()?;
        let (values, closed, view) = if arg.values.is_empty() {
            let separator = Glyphs::of(cx.glyphs).separator;
            let text = copy::optional_argument(&arg.name, separator);
            (vec![], false, note(text))
        } else {
            let words: Vec<(&str, &str)> = arg.values.iter().map(|v| (v.as_str(), "")).collect();
            let (values, list) = word_rows(&words, word);
            (values, true, ValuesView::List(list))
        };
        Some(self.finish_values(line, word, values, closed, view))
    }

    fn finish_values(
        &self,
        line: &str,
        word: &str,
        values: Vec<Value>,
        closed: bool,
        view: ValuesView,
    ) -> Values {
        let selected = self.highlight(line, word, &values, closed);
        let view = match view {
            ValuesView::Menu(menu) => ValuesView::Menu(Box::new(Menu { selected, ..*menu })),
            ValuesView::List(list) => ValuesView::List(Suggestions { selected, ..list }),
        };
        Values {
            line: line.to_owned(),
            word: word.to_owned(),
            values,
            closed,
            selected,
            view,
        }
    }

    fn highlight(&self, line: &str, word: &str, values: &[Value], closed: bool) -> Option<usize> {
        let chosen = self
            .chosen_for(line)
            .and_then(|text| values.iter().position(|v| v.enabled && v.text == text));
        let exact = || values.iter().position(|v| v.enabled && v.text == word);
        let labelled = || {
            let mut hits = values
                .iter()
                .enumerate()
                .filter(|(_, v)| v.enabled && v.label == word);
            match (hits.next(), hits.next()) {
                (Some((i, _)), None) if !word.is_empty() => Some(i),
                _ => None,
            }
        };
        let only = || {
            if !closed || word.is_empty() {
                return None;
            }
            let mut hits = values
                .iter()
                .enumerate()
                .filter(|(_, v)| v.enabled && v.matches);
            match (hits.next(), hits.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        };
        chosen.or_else(exact).or_else(labelled).or_else(only)
    }

    pub fn listing(&self, word: &str) -> Listing {
        let commands = matching_entries(self.registry, &self.plugins, word);
        let chosen = self
            .chosen_for(word)
            .and_then(|key| commands.iter().position(|c| c.key() == key));
        let selected = chosen.or_else(|| (!commands.is_empty()).then_some(0));
        Listing {
            word: word.to_owned(),
            commands,
            selected,
        }
    }

    pub fn choose(&mut self, line: &str, text: &str) {
        self.line = Some(line.to_owned());
        self.chosen = Some(text.to_owned());
    }

    fn moved(&mut self, listing: &Listing, down: bool) -> Step {
        let count = listing.commands.len();
        if count == 0 {
            return Step::Stay;
        }
        let at = listing.selected.unwrap_or(0);
        if let Some(command) = listing.commands.get(ListCursor::WRAP.step(at, count, down)) {
            self.choose(&listing.word, &command.key());
        }
        Step::Stay
    }

    pub fn key(&mut self, key: Key, listing: &Listing) -> Step {
        let Key {
            code,
            ctrl,
            alt,
            shift,
        } = key;
        let plain = !ctrl && !alt;
        match code {
            _ if let Some(down) = moves(key) => self.moved(listing, down),
            KeyCode::Tab if plain && !shift => match listing.highlighted() {
                Some(command) => Step::Complete(completion(command, &listing.word)),
                None => Step::Stay,
            },
            KeyCode::Enter if plain && !shift => match listing.highlighted() {
                Some(command) if !typed_in_full(command, &listing.word) => {
                    Step::Run(format!("/{}", command.word_for(&listing.word)))
                }
                Some(_) | None => Step::Pass,
            },
            KeyCode::Esc if plain => Step::Close,
            _ => Step::Pass,
        }
    }

    fn moved_value(&mut self, values: &Values, down: bool) -> Step {
        let enabled: Vec<usize> = (0..values.values.len())
            .filter(|&i| values.values[i].enabled)
            .collect();
        let at = values
            .selected
            .and_then(|at| enabled.iter().position(|&i| i == at));
        let next = match at {
            Some(at) => enabled
                .get(ListCursor::WRAP.step(at, enabled.len(), down))
                .copied(),
            None => values.first_match(down),
        };
        if let Some(value) = next.and_then(|i| values.values.get(i)) {
            self.choose(&values.line, &value.text);
        }
        Step::Stay
    }

    pub fn value_key(&mut self, key: Key, values: &Values) -> Step {
        let Key {
            code,
            ctrl,
            alt,
            shift,
        } = key;
        let plain = !ctrl && !alt;
        match code {
            _ if let Some(down) = moves(key) => self.moved_value(values, down),
            KeyCode::Tab if plain && !shift => {
                let target = values.selected.or_else(|| values.first_enabled_match());
                match target.and_then(|i| values.values.get(i)) {
                    Some(value) => Step::Complete(values.with(&value.text)),
                    None => Step::Stay,
                }
            }
            KeyCode::Enter if plain && !shift => match values.highlighted() {
                Some(value) => Step::Run(values.with(&value.text)),
                None => Step::Pass,
            },
            KeyCode::Esc if plain => Step::Close,
            _ => Step::Pass,
        }
    }
}

fn moves(key: Key) -> Option<bool> {
    let plain = !key.ctrl && !key.alt;
    let ctrl = key.ctrl && !key.alt;
    match key.code {
        KeyCode::Up if plain => Some(false),
        KeyCode::Down if plain => Some(true),
        KeyCode::Char('p' | 'P') if ctrl => Some(false),
        KeyCode::Char('n' | 'N') if ctrl => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
