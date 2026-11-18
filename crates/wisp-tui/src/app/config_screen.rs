use std::collections::{BTreeMap, BTreeSet};

use wisp_protocol::endpoint::{EndpointEdit, NotSavedReason, StoreChoice};
use wisp_protocol::filter::Query;
use wisp_protocol::plugin::{HookDisabled, HookEntry};
use wisp_protocol::settings::{
    AppliedChange, ENABLED_KEY, FileState, Place, Section, SettingChange, SettingName,
    SettingTarget, SettingValue, SettingsSnapshot, Site, SitePath, When, names,
};
use wisp_protocol::{
    AgentEvent, CONFIG_FILE, CREDENTIALS, IN_KEYCHAIN, NOT_SHOWN, RequestId, model_hidden,
};
use wisp_render::chrome::account;
use wisp_render::chrome::config_screen::{AskView, Detail, Entry, LayerLine, ListRow};
use wisp_render::chrome::endpoint_card as card_words;
use wisp_render::chrome::{KeyHint, Told};
use wisp_render::text::sanitize;
use wisp_render::transcript::copy::{self, ENTER_KEY, ESC_KEY, NEXT_TURN, SEP};

use crate::app::list::{self, Picked};
use crate::term::{Key, KeyCode};
use crate::widgets::line_edit::LineEdit;

mod detail;
mod edit;
mod entries;
mod rows;
mod scrub;

pub use detail::{
    CHECK_KEYS, CHECK_LABEL, CHOICE_KEYS, CHOOSE_KEY, EDIT_KEY, FIXED_CHECK_KEYS, Keys,
    NOT_USED_HERE, Quote, ResetCost, TEXT_KEYS, View, changed_words, chat_line, chat_only, detail,
    help_title, hint, hints, new_chats, notes, reset_key, reset_value, set_command, toggle_key,
    when_words, winner_words,
};
pub use edit::{
    ALL_MODELS, Context, Edit, Field, NOT_SET, PRICING_PER_ENDPOINT, Pick, SiteChoice, THE_DEFAULT,
    Widget, after_reset, can_reset, check, found_words, key_shaped, locked, preview, sites,
    toggled, told, told_at, unnested, widget,
};
pub use entries::{
    ADD_ENDPOINT_HELP, ADD_KEY, ADD_MODEL, ADD_MODEL_HELP, ALL_ENDPOINTS, ASK_KEYS, Item,
    KEY_STAYS, MODEL_LEAD, NEW_CHATS, NO_FIELDS, NOT_REMOVABLE, NOT_RESETTABLE, NOTHING_FOR_MODEL,
    OPEN_KEY, OPEN_MODEL_KEY, PATH_HIDDEN, PLUGIN_LEAD, PROMPT_KEY, PROVIDER_URL, Page, REMOVE_KEY,
    REMOVE_MODEL_KEY, SEARCH_LEAD, SWITCH_FIRST, Spot, UNCHECKED_ASK, UNSET_KEY, URL_FROM_VARIABLE,
    URL_RESET_KEY, WHEN_YOU_SWITCH, endpoint_rows, heading, items, model_rows,
    remove_model_question, store_of, table_only_question,
};
pub use rows::{
    Changed, DEFAULT, DETECTED, Display, ENTRY_PANES, EVERYDAY, Found, OFF_SCREEN, ORDER, RULE,
    Resolved, Row, Set, display_set, forced_display, placed, rows, shown,
};

pub const BROKEN: &str = concat!(
    wisp_protocol::config_file!(),
    " has an error; saving is off until it's fixed"
);
pub const NOT_YET: &str = "This setting can't change here yet; use wisp config set.";
pub const NOTHING_TO_RESET: &str = "Nothing to reset: this setting has no saved or session value.";
pub const SAVING: &str = "saving\u{2026}";
pub const WHEN_YOU_USE: &str = "when you use this model";
const FILTER_GAPS_FROM: u16 = 40;
pub const FILTER_KEY: (&str, &str) = ("/", "filter");
pub const CHOOSE_KEYS: (&str, &str) = ("\u{2190} \u{2192}", copy::CHOOSE_WORD);
pub const MOVE_KEYS: (&str, &str) = ("\u{2190} \u{2192}", copy::MOVE_WORD);
pub const USE_KEY: (&str, &str) = (ENTER_KEY, "use it");
pub const TOGGLE_KEY: (&str, &str) = (copy::SPACE_KEY, copy::TOGGLE_WORD);
pub const SAVE_KEY: (&str, &str) = (ENTER_KEY, copy::SAVE_WORD);
pub const CANCEL_KEY: (&str, &str) = (ESC_KEY, copy::CANCEL_WORD);
pub const HELP_KEY: (&str, &str) = ("?", "details");
pub const BACK_KEY: (&str, &str) = (ESC_KEY, copy::BACK_WORD);
pub const DONE_KEY: (&str, &str) = (ENTER_KEY, "done");
pub const CLEAR_KEY: (&str, &str) = (ESC_KEY, copy::CLEAR_WORD);
pub const STOP_KEY: (&str, &str) = (copy::CTRL_C_KEY, copy::TO_STOP);

pub fn pending(when: &str) -> String {
    format!("pending{SEP}{when}")
}

pub fn no_setting_matches(word: &str) -> String {
    format!("no setting matches {word}")
}

pub fn broken_words(line: Option<u64>) -> String {
    match line {
        Some(line) => {
            format!("{CONFIG_FILE} has an error at line {line}; saving is off until it's fixed")
        }
        None => BROKEN.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Save {
    pub name: &'static str,
    pub change: SettingChange,
    pub told: Told,
    pub save: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointOp {
    Edit {
        name: String,
        edit: EndpointEdit,
        store: Option<StoreChoice>,
        check: bool,
    },
    Remove {
        name: String,
        keep_key: bool,
    },
}

pub(super) type Mark = (Option<Spot>, Spot);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub op: EndpointOp,
    mark: Mark,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Many {
    pub changes: Vec<SettingChange>,
    mark: Mark,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Stay,
    Close,
    Hint(&'static str),
    Note(String),
    Save(Box<Save>),
    Pick(Pick),
    Interrupt,
    Repaint,
    Suspend,
    Endpoint(Box<Sent>),
    SaveMany(Box<Many>),
    UseEndpoint(String),
    Prompt { name: String, url: bool },
    AddEndpoint,
    AddModel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub question: String,
    options: [&'static str; 2],
    pick: list::Pick,
    yes: Act,
}

impl Ask {
    fn new(question: String, options: [&'static str; 2], held: bool, yes: Act) -> Self {
        let pick = list::Pick {
            rows: options.len(),
            held: vec![held, false],
            focus: Some(1),
        };
        Self {
            question,
            options,
            pick,
            yes,
        }
    }

    fn remove(question: String, yes: Act) -> Self {
        Self::new(question, copy::REMOVE_KEEP, true, yes)
    }

    fn yes_no(question: String, yes: Act) -> Self {
        Self::new(question, copy::YES_NO, false, yes)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    current: Page,
    filter: String,
    selected: Option<Spot>,
    edit: Option<Edit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Opened {
    spot: Spot,
    rows: Vec<Row>,
    items: Vec<Item>,
    selected: Option<Spot>,
    top: usize,
    filter: String,
}

#[derive(Clone, Debug, Default)]
pub struct Screen {
    asked: Option<RequestId>,
    snapshot: Option<SettingsSnapshot>,
    often: Vec<String>,
    rows: Vec<Row>,
    items: Vec<Item>,
    filter: String,
    editing: bool,
    selected: Option<Spot>,
    top: usize,
    page: usize,
    current: Page,
    expanded: bool,
    spaced: bool,
    live: Option<String>,
    edit: Option<Edit>,
    saving: Vec<(RequestId, Mark, bool)>,
    pending: BTreeSet<Mark>,
    pub(super) later: BTreeSet<Mark>,
    refused: BTreeMap<Mark, String>,
    opened: Option<Box<Opened>>,
    ask: Option<Ask>,
    ops: Vec<(RequestId, EndpointOp)>,
    credentials: Option<String>,
    keys_file: String,
    display: Display,
}

fn names(changes: &[AppliedChange]) -> impl Iterator<Item = (&'static str, &When)> {
    changes.iter().map(|c| (c.target.spec().name, &c.when))
}

pub fn named_settings() -> impl Iterator<Item = &'static wisp_protocol::settings::SettingSpec> {
    placed().map(|(_, spec)| spec).chain(
        [names::CONTEXT_WINDOW, names::PRICING]
            .into_iter()
            .filter_map(wisp_protocol::settings::find),
    )
}

pub fn named(word: &str) -> Option<&'static str> {
    named_settings()
        .find(|spec| spec.name == word)
        .map(|spec| spec.name)
}

pub(crate) fn not_saved_words(why: &NotSavedReason) -> String {
    match why {
        NotSavedReason::Check(check) => check
            .reason
            .as_deref()
            .map_or_else(|| card_words::NOT_SAVED.to_owned(), sanitize),
        NotSavedReason::KeychainFailed(which) => card_words::keychain_stopped(&sanitize(which)),
        NotSavedReason::Other(_) => card_words::NOT_SAVED.to_owned(),
    }
}

impl Screen {
    pub fn opening(request: RequestId, words: &str) -> Screen {
        let words = words.trim();
        let mut screen = Screen {
            asked: Some(request),
            ..Screen::default()
        };
        match named(words) {
            Some(name) => {
                screen.current = Page::setting(name);
                screen.selected = Some(Spot::Setting(name));
            }
            None => screen.filter = words.to_owned(),
        }
        screen
    }

    pub fn restored(request: RequestId, draft: Draft) -> Screen {
        Screen {
            asked: Some(request),
            current: draft.current,
            filter: draft.filter,
            selected: draft.selected,
            edit: draft.edit,
            ..Screen::default()
        }
    }

    pub fn draft(&self) -> Draft {
        let (filter, selected, edit) = match &self.opened {
            Some(opened) => (opened.filter.clone(), opened.selected.clone(), None),
            None => (
                self.filter.clone(),
                self.selected.clone(),
                self.edit.clone().filter(Edit::keeps),
            ),
        };
        let filter = if key_shaped(filter.trim()) {
            String::new()
        } else {
            filter
        };
        Draft {
            current: self.current.clone(),
            filter,
            selected,
            edit,
        }
    }

    pub fn credentials_shown(&mut self, shown: Option<String>) {
        self.credentials = shown;
    }

    pub fn set_keys_file(&mut self, file: String) {
        self.keys_file = file;
    }

    fn keys_file_only(&self, ending: &str) -> String {
        entries::keys_file_only(&self.keys_file, ending)
    }

    pub fn edit(&self) -> Option<&Edit> {
        self.edit.as_ref()
    }

    pub fn asking(&self) -> Option<&Ask> {
        self.ask.as_ref()
    }

    pub fn opened(&self) -> Option<&Spot> {
        self.opened.as_ref().map(|o| &o.spot)
    }

    fn context(&self) -> Option<Spot> {
        self.opened.as_ref().map(|o| o.spot.clone())
    }

    fn mark(&self, spot: Spot) -> Mark {
        (self.context(), spot)
    }

    fn mark_sent(&mut self, request: RequestId, mark: Mark, count: bool) {
        self.refused.remove(&mark);
        self.pending.remove(&mark);
        self.later.remove(&mark);
        self.saving.push((request, mark, count));
    }

    pub fn sent(&mut self, request: RequestId, names: &[&'static str], save: bool) {
        for name in names {
            let mark = self.mark(Spot::Setting(name));
            self.mark_sent(request, mark, save && Page::counts(name));
        }
    }

    pub fn sent_spot(&mut self, request: RequestId, spot: Spot) {
        let mark = self.mark(spot);
        self.mark_sent(request, mark, false);
    }

    pub fn sent_op(&mut self, request: RequestId, sent: Sent) {
        self.ops.push((request, sent.op));
        self.mark_sent(request, sent.mark, false);
    }

    pub fn sent_many(&mut self, request: RequestId, many: &Many) {
        self.mark_sent(request, many.mark.clone(), false);
    }

    pub fn is_saving(&self, name: &str) -> bool {
        let context = self.context();
        self.saving
            .iter()
            .any(|(_, (c, spot), _)| *c == context && spot.setting() == Some(name))
    }

    fn spot_saving(&self, spot: &Spot) -> bool {
        let mark = self.mark(spot.clone());
        self.saving.iter().any(|(_, m, _)| *m == mark)
    }

    fn answered_save(&mut self, request: RequestId) -> Vec<Mark> {
        let (answered, rest) = std::mem::take(&mut self.saving)
            .into_iter()
            .partition(|(r, _, _)| *r == request);
        self.saving = rest;
        answered.into_iter().map(|(_, mark, _)| mark).collect()
    }

    fn take_op(&mut self, request: RequestId) -> Option<EndpointOp> {
        let at = self.ops.iter().position(|(r, _)| *r == request)?;
        Some(self.ops.remove(at).1)
    }

    fn not_saved(&mut self, request: RequestId, why: &NotSavedReason) -> bool {
        let marks = self.answered_save(request);
        let op = self.take_op(request);
        let Some(mark) = marks.first().cloned() else {
            return false;
        };
        let file = self
            .credentials
            .clone()
            .unwrap_or_else(|| CREDENTIALS.to_owned());
        let ask = match (why, op) {
            (
                NotSavedReason::KeychainFailed(which),
                Some(EndpointOp::Remove {
                    name,
                    keep_key: false,
                }),
            ) => Some(Ask::remove(
                table_only_question(&sanitize(which)),
                Act::Endpoint(Box::new(Sent {
                    op: EndpointOp::Remove {
                        name,
                        keep_key: true,
                    },
                    mark,
                })),
            )),
            (NotSavedReason::KeychainFailed(which), Some(EndpointOp::Edit { name, edit, .. }))
                if !edit.is_empty() =>
            {
                Some(Ask::yes_no(
                    card_words::keychain_question(&sanitize(which), &file, false),
                    Act::Endpoint(Box::new(Sent {
                        op: EndpointOp::Edit {
                            name,
                            edit,
                            store: Some(StoreChoice::File),
                            check: false,
                        },
                        mark,
                    })),
                ))
            }
            (
                NotSavedReason::Check(_),
                Some(EndpointOp::Edit {
                    name,
                    edit,
                    store,
                    check: true,
                }),
            ) => Some(Ask::yes_no(
                format!(
                    "{} {UNCHECKED_ASK}",
                    card_words::check_failed(not_saved_words(why).trim_end_matches('.'))
                ),
                Act::Endpoint(Box::new(Sent {
                    op: EndpointOp::Edit {
                        name,
                        edit,
                        store,
                        check: false,
                    },
                    mark,
                })),
            )),
            _ => None,
        };
        match ask {
            Some(ask) => self.ask = Some(ask),
            None => {
                let words = not_saved_words(why);
                for mark in marks {
                    self.refused.insert(mark, words.clone());
                }
            }
        }
        true
    }

    pub fn used(&self, event: &AgentEvent) -> Vec<String> {
        let AgentEvent::SettingsChanged {
            answers: Some(request),
            changes,
            ..
        } = event
        else {
            return Vec::new();
        };
        self.saving
            .iter()
            .filter(|(r, _, count)| r == request && *count)
            .filter_map(|(_, (_, spot), _)| spot.setting())
            .filter(|name| names(changes).any(|(n, _)| n == *name))
            .map(str::to_owned)
            .collect()
    }

    pub fn saved(&mut self, event: &AgentEvent, busy: bool) -> bool {
        match event {
            AgentEvent::SettingsChanged {
                answers: Some(request),
                changes,
                ..
            } => {
                let answered = self.answered_save(*request);
                if answered.is_empty() {
                    return false;
                }
                for mark in answered {
                    if mark.1 == Spot::Search {
                        if changes.iter().any(|c| c.when == When::NextSession) {
                            self.later.insert(mark);
                        }
                        continue;
                    }
                    let Some(name) = mark.1.setting() else {
                        continue;
                    };
                    match names(changes).find(|(n, _)| *n == name).map(|(_, w)| w) {
                        Some(When::NextTurn) if busy => {
                            self.pending.insert(mark);
                        }
                        Some(When::NextSession) => {
                            self.later.insert(mark);
                        }
                        _ => {}
                    }
                }
                true
            }
            AgentEvent::Rejected {
                request: Some(request),
                reason,
                ..
            } => {
                self.take_op(*request);
                let marks = self.answered_save(*request);
                for mark in &marks {
                    self.refused.insert(mark.clone(), sanitize(reason));
                }
                !marks.is_empty()
            }
            AgentEvent::EndpointEdited { answers, .. }
            | AgentEvent::EndpointRemoved { answers, .. } => {
                self.take_op(*answers);
                !self.answered_save(*answers).is_empty()
            }
            AgentEvent::EndpointNotSaved { answers, why, .. } => self.not_saved(*answers, why),
            _ => false,
        }
    }

    pub fn turn_started(&mut self) -> bool {
        let had = !self.pending.is_empty();
        self.pending.clear();
        had
    }

    pub fn asked(&self) -> Option<RequestId> {
        self.asked
    }

    pub fn ask(&mut self, request: RequestId) {
        self.asked = Some(request);
    }

    pub fn loading(&self) -> bool {
        self.snapshot.is_none()
    }

    pub fn snapshot(&self) -> Option<&SettingsSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn editing(&self) -> bool {
        self.editing
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }

    pub fn spaced(&mut self, height: u16, room: usize) {
        self.spaced = false;
        let (entries, _) = self.entries();
        let gaps = if self.opened.is_none() && !self.filter.is_empty() && height >= FILTER_GAPS_FROM
        {
            entries
                .iter()
                .filter(|e| matches!(e, Entry::Heading(_)))
                .count()
                .saturating_sub(1)
        } else if self.current == Page::Home && self.opened.is_none() && self.filter.is_empty() {
            if self.often_spots().next().is_some() {
                2
            } else {
                1
            }
        } else if self.on_plugin_page() && self.opened.is_none() {
            1
        } else {
            return;
        };
        self.spaced = entries.len() + gaps <= room;
    }

    pub fn live_endpoint(&mut self, name: Option<String>) {
        self.live = name;
    }

    pub fn top(&self) -> usize {
        self.top
    }

    fn entry_rows(snapshot: &SettingsSnapshot, spot: &Spot) -> Option<Vec<Row>> {
        match spot {
            Spot::Endpoint(name) => (entries::summary_of(snapshot, name).is_some()
                || snapshot.context.endpoint == *name)
                .then(|| {
                    let mut rows = endpoint_rows(snapshot, name);
                    if entries::keys_file(snapshot, name) {
                        rows.retain(|row| row.spec.name == names::PRICING);
                    }
                    rows
                }),
            Spot::Model(id) => Some(model_rows(snapshot, id)),
            Spot::Open(_)
            | Spot::Setting(_)
            | Spot::AddEndpoint
            | Spot::AddModel
            | Spot::Plugin(_)
            | Spot::Section(_)
            | Spot::Hook(_)
            | Spot::Search => None,
        }
    }

    pub fn answered(&mut self, snapshot: &SettingsSnapshot, display: &Display) -> bool {
        if self.asked != Some(snapshot.answers) {
            return false;
        }
        self.asked = None;
        let snapshot = scrub::scrubbed(snapshot);
        if self.snapshot.is_none() {
            self.often.clone_from(&snapshot.often);
        }
        let mut main = rows::scrubbed_rows(&snapshot, display);
        main.retain(|row| !entries::removed_plugin_of(&snapshot, row.spec));
        let found = items(&snapshot, display);
        self.display = display.clone();
        let fields = self
            .opened
            .as_ref()
            .and_then(|o| Self::entry_rows(&snapshot, &o.spot));
        match (&mut self.opened, fields) {
            (Some(opened), Some(fields)) => {
                opened.rows = main;
                opened.items = found;
                self.rows = fields;
            }
            (Some(_), None) => {
                self.leave_entry();
                self.rows = main;
                self.items = found;
            }
            (None, _) => {
                self.rows = main;
                self.items = found;
            }
        }
        self.snapshot = Some(snapshot);
        if self.opened.is_none()
            && let Some(name) = self
                .selected
                .as_ref()
                .and_then(Spot::setting)
                .filter(|name| ENTRY_PANES.contains(name) && named(name).is_some())
            && let Some(snapshot) = &self.snapshot
        {
            let spot = if self.current == Page::Accounts {
                Spot::Endpoint(snapshot.context.endpoint.clone())
            } else {
                Spot::Model(snapshot.context.model.clone())
            };
            self.selected = Some(spot.clone());
            self.enter_entry(spot);
            self.selected = Some(Spot::Setting(name));
        }
        self.keep_selection();
        true
    }

    pub fn catalog_changed(&mut self, catalog: &wisp_protocol::Catalog) -> bool {
        let Some(snapshot) = &mut self.snapshot else {
            return false;
        };
        snapshot.plugins.clone_from(&catalog.plugins);
        let found = items(snapshot, &self.display);
        match &mut self.opened {
            Some(opened) => opened.items = found,
            None => self.items = found,
        }
        self.keep_selection();
        true
    }

    fn enter_entry(&mut self, spot: Spot) -> Act {
        let Some(fields) = self
            .snapshot
            .as_ref()
            .and_then(|s| Self::entry_rows(s, &spot))
        else {
            return Act::Stay;
        };
        let opened = Opened {
            spot,
            rows: std::mem::replace(&mut self.rows, fields),
            items: std::mem::take(&mut self.items),
            selected: self.selected.take(),
            top: self.top,
            filter: std::mem::take(&mut self.filter),
        };
        self.opened = Some(Box::new(opened));
        self.editing = false;
        self.edit = None;
        self.top = 0;
        self.keep_selection();
        Act::Stay
    }

    fn leave_entry(&mut self) {
        let Some(opened) = self.opened.take() else {
            return;
        };
        let Opened {
            rows,
            items,
            selected,
            top,
            filter,
            ..
        } = *opened;
        self.rows = rows;
        self.items = items;
        self.selected = selected;
        self.top = top;
        self.filter = filter;
        self.editing = false;
        self.edit = None;
        self.keep_selection();
    }

    pub fn open_model(&mut self, id: &str) -> Act {
        if self.opened.is_some() {
            self.leave_entry();
        }
        if model_hidden(id) {
            return Act::Hint(edit::HIDDEN_MODEL_LOCK);
        }
        let spot = Spot::Model(id.to_owned());
        self.selected = Some(spot.clone());
        self.enter_entry(spot)
    }

    pub fn told_for(&self, name: &str) -> Option<Told> {
        let rows = match &self.opened {
            Some(opened) => &opened.rows,
            None => &self.rows,
        };
        let row = rows.iter().find(|r| r.spec.name == name)?;
        Some(Told {
            was: unnested(&row.value),
            saved: row.saved_here.clone(),
            wins: None,
        })
    }

    fn place(&self, row: &Row) -> SitePath {
        if let Some(site) = self.edit.as_ref().and_then(Edit::site) {
            return site.path.clone();
        }
        if self.opened.is_none() && row.spec.has_site(Site::Global) && self.has_sites(row) {
            return SitePath::global(row.spec);
        }
        row.path.clone()
    }

    fn sites_for(&self, row: &Row) -> Vec<SiteChoice> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut found = sites(row, snapshot);
        match self.opened() {
            None => {}
            Some(Spot::Model(_)) => found
                .retain(|s| matches!(s.path.place, Place::Model(_) | Place::EndpointModel { .. })),
            Some(_) => found.clear(),
        }
        found
    }

    pub fn has_sites(&self, row: &Row) -> bool {
        self.sites_for(row).len() > 1
    }

    fn keys_file(&self, name: &str) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| entries::keys_file(snapshot, name))
    }

    pub fn can_change(&self) -> bool {
        if self.loading() || self.broken().is_some() {
            return false;
        }
        match self.selected_spot() {
            Some(Spot::Setting(_)) => self.selected().is_some_and(|row| {
                (widget(row.spec).is_some() || entries::prompts(row.spec))
                    && self.locked(row).is_none()
            }),
            Some(Spot::Endpoint(name)) => self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.context.endpoint != *name
                    || entries::summary_of(snapshot, name).is_some_and(|e| !e.default)
            }),
            Some(Spot::Plugin(_) | Spot::Search) | None => false,
            Some(_) => true,
        }
    }

    fn locked(&self, row: &Row) -> Option<&'static str> {
        let hidden = edit::names_hidden_model(&row.path.place).then_some(edit::HIDDEN_MODEL_LOCK);
        let endpoint = match self.opened() {
            Some(Spot::Endpoint(name)) => name,
            _ => &self.snapshot.as_ref()?.context.endpoint,
        };
        locked(row.spec, self.keys_file(endpoint)).or(hidden)
    }

    pub fn broken(&self) -> Option<String> {
        match &self.snapshot.as_ref()?.file {
            FileState::Broken { line, .. } => Some(broken_words(*line)),
            FileState::Ok { .. } | FileState::Missing | FileState::Other(_) => None,
        }
    }

    fn row_named(&self, name: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.spec.name == name)
    }

    fn item_of(&self, spot: &Spot) -> Option<&Item> {
        self.items.iter().find(|i| i.spot == *spot)
    }

    fn section_of(&self, spot: &Spot) -> Option<Section> {
        match spot {
            Spot::Setting(name) => self.row_named(name).map(|r| r.section),
            _ => self.item_of(spot).map(|i| i.section),
        }
    }

    fn often_spots(&self) -> impl Iterator<Item = Spot> + '_ {
        self.often
            .iter()
            .filter_map(|name| named(name))
            .filter(|name| Page::counts(name) && self.row_named(name).is_some())
            .take(3)
            .map(Spot::Setting)
    }

    fn page_spots(&self, page: &Page) -> Vec<Spot> {
        match page {
            Page::Home => EVERYDAY
                .iter()
                .flat_map(|name| {
                    let spot = Spot::Setting(name);
                    std::iter::once(spot).chain((*name == names::ENDPOINT).then_some(Spot::Search))
                })
                .chain(self.often_spots())
                .chain([Spot::Open(Page::Plugins), Spot::Open(Page::More)])
                .filter(|spot| {
                    matches!(spot, Spot::Open(_))
                        || spot.setting().map_or_else(
                            || self.item_of(spot).is_some(),
                            |n| self.row_named(n).is_some(),
                        )
                })
                .collect(),
            Page::Group(section) => self
                .rows
                .iter()
                .filter(|row| row.section == *section && !EVERYDAY.contains(&row.spec.name))
                .map(|row| Spot::Setting(row.spec.name))
                .chain(
                    self.items
                        .iter()
                        .filter(|i| {
                            i.section == *section
                                && matches!(i.spot, Spot::Model(_) | Spot::AddModel)
                        })
                        .map(|i| i.spot.clone()),
                )
                .collect(),
            Page::More => Section::ALL
                .into_iter()
                .filter(|section| *section != Section::Plugins)
                .filter_map(|section| {
                    let page = Page::Group(section);
                    (!self.page_spots(&page).is_empty()).then_some(Spot::Open(page))
                })
                .collect(),
            _ => self
                .rows
                .iter()
                .filter(
                    |row| matches!(page, Page::Plugin(id) if row.spec.plugin == Some(id.as_str())),
                )
                .map(|row| Spot::Setting(row.spec.name))
                .chain(
                    self.items
                        .iter()
                        .filter(|item| match (page, &item.spot) {
                            (Page::Accounts, Spot::Endpoint(_) | Spot::AddEndpoint)
                            | (Page::Plugins, Spot::Plugin(_)) => true,
                            (Page::Plugin(id), Spot::Section(name)) => {
                                self.snapshot.as_ref().is_some_and(|s| {
                                    s.plugins.iter().any(|p| {
                                        p.id == *id && p.slots.iter().any(|slot| slot.name == *name)
                                    })
                                })
                            }
                            (Page::Plugin(id), Spot::Hook(name)) => {
                                name.split_once('/').is_some_and(|(plugin, _)| plugin == id)
                            }
                            _ => false,
                        })
                        .map(|item| item.spot.clone()),
                )
                .collect(),
        }
    }

    fn shown(&self) -> Vec<Spot> {
        if self.filter.is_empty() && self.opened.is_none() {
            return self.page_spots(&self.current);
        }
        let query = Query::new(&self.filter);
        let mut out = vec![];
        for section in Section::ALL {
            out.extend(
                self.rows
                    .iter()
                    .filter(|row| row.section == section && row.spec.matches(&query))
                    .map(|row| Spot::Setting(row.spec.name)),
            );
            out.extend(
                self.items
                    .iter()
                    .filter(|item| {
                        item.section == section
                            && query.matches(&[
                                &item.label,
                                &item.value,
                                match item.spot {
                                    Spot::Hook(_) => entries::HOOK_LEAD,
                                    Spot::Section(_) => entries::SECTION_LEAD,
                                    _ => "",
                                },
                            ])
                    })
                    .map(|item| item.spot.clone()),
            );
        }
        out
    }

    fn marked(&self, spot: &Spot, value: &str) -> String {
        let mark = self.mark(spot.clone());
        if self.saving.iter().any(|(_, m, _)| *m == mark) {
            format!("{value}{SEP}{SAVING}")
        } else if self.pending.contains(&mark) {
            format!("{value}{SEP}{}", pending(NEXT_TURN))
        } else if self.later.contains(&mark) {
            format!("{value}{SEP}{NEW_CHATS}")
        } else {
            value.to_owned()
        }
    }

    fn on_plugin_page(&self) -> bool {
        matches!(self.current, Page::Plugin(_)) && self.filter.is_empty()
    }

    fn part_name(&self, name: &str) -> String {
        let name = if self.on_plugin_page() {
            name.split_once('/').map_or(name, |(_, part)| part)
        } else {
            name
        };
        sanitize(name)
    }

    pub fn entries(&self) -> (Vec<Entry>, Vec<Option<Spot>>) {
        let mut entries = vec![];
        let mut map = vec![];
        let mut section = None;
        if let Some(opened) = &self.opened {
            entries.push(Entry::Heading(heading(&opened.spot)));
            map.push(None);
        } else if self.filter.is_empty() && self.current != Page::Home {
            entries.push(Entry::Heading(sanitize(self.current.title())));
            map.push(None);
        }
        let first_often =
            (self.current == Page::Home && self.filter.is_empty() && self.opened.is_none())
                .then(|| self.often_spots().next())
                .flatten();
        for spot in self.shown() {
            if first_often.as_ref() == Some(&spot) {
                if self.spaced {
                    entries.push(Entry::Gap);
                    map.push(None);
                }
                entries.push(Entry::Heading(
                    wisp_render::chrome::config_screen::OFTEN_USED.to_owned(),
                ));
                map.push(None);
            }
            if self.opened.is_none() && !self.filter.is_empty() {
                let Some(this) = self.section_of(&spot) else {
                    continue;
                };
                if section != Some(this) {
                    if self.spaced && section.is_some() {
                        entries.push(Entry::Gap);
                        map.push(None);
                    }
                    section = Some(this);
                    entries.push(Entry::Heading(this.title().to_owned()));
                    map.push(None);
                }
            }
            if self.spaced
                && (spot == Spot::Open(Page::Plugins)
                    || (self.on_plugin_page()
                        && spot.setting().is_none()
                        && matches!(map.last(), Some(Some(Spot::Setting(_))))))
            {
                entries.push(Entry::Gap);
                map.push(None);
            }
            let (row, aside) = match &spot {
                Spot::Open(page) => (
                    ListRow {
                        label: if *page == Page::More {
                            format!("{}\u{2026}", page.title())
                        } else {
                            page.title().to_owned()
                        },
                        value: if *page == Page::Plugins {
                            self.snapshot
                                .as_ref()
                                .map_or_else(String::new, entries::plugin_count)
                        } else {
                            String::new()
                        },
                        marked: false,
                        numeric: false,
                    },
                    None,
                ),
                Spot::Setting(name) => {
                    let Some(row) = self.row_named(name) else {
                        continue;
                    };
                    (
                        ListRow {
                            label: row.spec.label.to_owned(),
                            value: self.marked(&spot, &row.value),
                            marked: chat_only(row),
                            numeric: matches!(
                                row.spec.kind,
                                wisp_protocol::settings::Kind::Int { .. }
                                    | wisp_protocol::settings::Kind::Tokens { .. }
                                    | wisp_protocol::settings::Kind::TokensOrPercent
                                    | wisp_protocol::settings::Kind::Usd { .. }
                            ),
                        },
                        None,
                    )
                }
                _ => {
                    let Some(item) = self.item_of(&spot) else {
                        continue;
                    };
                    (
                        ListRow {
                            label: self.part_name(&item.label),
                            value: self.marked(&spot, &item.value),
                            marked: spot == Spot::Search
                                && self
                                    .snapshot
                                    .as_ref()
                                    .and_then(entries::SearchRow::of)
                                    .is_some_and(|r| {
                                        matches!(
                                            r.state,
                                            entries::SearchState::Now { chat: true, .. }
                                        )
                                    }),
                            numeric: false,
                        },
                        item.aside.clone(),
                    )
                }
            };
            entries.push(Entry::Row(row));
            map.push(Some(spot.clone()));
            if let Some(aside) = aside {
                entries.push(Entry::Aside(aside));
                map.push(None);
            }
            if let Some(reason) = self.refused.get(&self.mark(spot)) {
                entries.push(Entry::Note(reason.clone()));
                map.push(None);
            }
        }
        (entries, map)
    }

    fn snapshot_context(&self) -> Context {
        self.snapshot.as_ref().map_or(
            Context {
                window: None,
                room: None,
                max_tokens: 0,
            },
            Context::of,
        )
    }

    fn connection_keys(&self, row: &Row) -> Vec<KeyHint> {
        let first = if entries::prompts(row.spec) {
            PROMPT_KEY
        } else if widget(row.spec) == Some(Widget::Text) {
            EDIT_KEY
        } else {
            CHOOSE_KEY
        };
        let reset = match row.spec.name {
            names::URL if entries::can_reset_connection(row.spec, row) => Some(URL_RESET_KEY),
            names::KEY_ENV | names::URL_ENV if entries::can_reset_connection(row.spec, row) => {
                Some(UNSET_KEY)
            }
            _ => None,
        };
        std::iter::once(first).chain(reset).map(hint).collect()
    }

    fn field_detail(&self, row: &Row, quote: &Quote) -> Detail {
        let mut shown = detail(
            row,
            View {
                quote,
                edit: self.edit.as_ref(),
                context: self.snapshot_context(),
                locked: self.locked(row),
                sites: self.has_sites(row),
            },
        );
        let Some(snapshot) = self.snapshot.as_ref() else {
            return shown;
        };
        match self.opened() {
            Some(Spot::Endpoint(name)) => {
                let connection = entries::is_connection(row.spec);
                if *name != snapshot.context.endpoint {
                    shown.applies = WHEN_YOU_SWITCH.to_owned();
                } else if connection {
                    shown.applies = NEW_CHATS.to_owned();
                }
                if entries::prompts(row.spec) {
                    shown.default = String::new();
                    shown.layers = vec![LayerLine {
                        word: row.spec.label.to_owned(),
                        value: row.value.clone(),
                        origin: String::new(),
                        winner: true,
                        note: None,
                    }];
                }
                if connection && self.edit.is_none() {
                    shown.keys = self.connection_keys(row);
                }
            }
            Some(Spot::Model(id)) if *id != snapshot.context.model => {
                shown.applies = WHEN_YOU_USE.to_owned();
            }
            _ => {}
        }
        shown
    }

    fn endpoint_detail(&self, name: &str, quote: &Quote) -> Option<Detail> {
        let snapshot = self.snapshot.as_ref()?;
        let summary = entries::summary_of(snapshot, name)?;
        let current = snapshot.context.endpoint == name;
        let reserved = summary.keys_file;
        let layers = if reserved {
            vec![]
        } else {
            endpoint_rows(snapshot, name)
                .iter()
                .map(|row| LayerLine {
                    word: row.spec.label.to_owned(),
                    value: row.value.clone(),
                    origin: String::new(),
                    winner: true,
                    note: None,
                })
                .collect()
        };
        let mut keys = vec![];
        if !current || !summary.default {
            keys.push(hint(USE_KEY));
            if !current {
                keys.extend(notes(quote.endpoint.as_deref().unwrap_or_default()));
            }
        }
        if !reserved {
            keys.push(hint(OPEN_KEY));
        }
        if !current && !reserved {
            keys.push(hint(REMOVE_KEY));
        }
        let mut notes = vec![];
        if reserved {
            notes.push(self.keys_file_only(NO_FIELDS));
        }
        if let Some(why) = &summary.unavailable {
            notes.push(sanitize(why));
        }
        Some(Detail {
            lead: account::ACCOUNT_TITLE.to_owned(),
            title: sanitize(name),
            layers,
            applies: if current {
                NEW_CHATS.to_owned()
            } else {
                WHEN_YOU_SWITCH.to_owned()
            },
            line: notes.first().cloned().unwrap_or_default(),
            notes,
            keys,
            ..Detail::default()
        })
    }

    fn model_detail(&self, id: &str) -> Option<Detail> {
        let snapshot = self.snapshot.as_ref()?;
        let item = self.item_of(&Spot::Model(id.to_owned()))?;
        let layers = model_rows(snapshot, id)
            .iter()
            .map(|row| LayerLine {
                word: row.spec.label.to_owned(),
                value: row.value.clone(),
                origin: row.source.clone(),
                winner: true,
                note: None,
            })
            .collect();
        let mut keys = vec![hint(OPEN_MODEL_KEY)];
        if entries::configured(snapshot, id) {
            keys.push(hint(REMOVE_MODEL_KEY));
        }
        Some(Detail {
            lead: MODEL_LEAD.to_owned(),
            title: item.label.clone(),
            layers,
            applies: if id == snapshot.context.model {
                String::new()
            } else {
                WHEN_YOU_USE.to_owned()
            },
            keys,
            ..Detail::default()
        })
    }

    fn plugin_detail(&self, id: &str, quote: &Quote) -> Option<Detail> {
        let snapshot = self.snapshot.as_ref()?;
        let plugin = snapshot.plugins.iter().find(|p| p.id == id)?;
        let row = entries::PluginRow::of(plugin, snapshot);
        let tools = !plugin.tools.is_empty() && !row.waits(plugin);
        let mut keys = row.keys();
        keys.insert(1, hint(OPEN_KEY));
        if tools {
            keys.splice(1..1, self::notes(&quote.tools));
        }
        let state = LayerLine {
            word: entries::PLUGIN_STATE_WORD.to_owned(),
            value: row.state(plugin),
            origin: String::new(),
            winner: true,
            note: None,
        };
        let layers = std::iter::once(state)
            .chain(plugin.tools.iter().map(|tool| LayerLine {
                word: sanitize(&tool.name),
                value: entries::tool_state(tool, plugin),
                origin: String::new(),
                winner: true,
                note: None,
            }))
            .collect();
        Some(Detail {
            lead: sanitize(id),
            title: help_title(&sanitize(&plugin.summary)),
            layers,
            fallback: if plugin.tools.is_empty() {
                String::new()
            } else {
                format!(
                    "Tools: {}",
                    plugin
                        .tools
                        .iter()
                        .map(|t| sanitize(&t.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            notes: entries::plugin_notes(plugin),
            keys,
            applies: if tools {
                quote.tools.clone()
            } else {
                String::new()
            },
            ..Detail::default()
        })
    }

    fn spot_save(&self, spot: Spot, changes: Vec<SettingChange>) -> Act {
        if self.broken().is_some() {
            return Act::Hint(BROKEN);
        }
        if self.spot_saving(&spot) {
            return Act::Stay;
        }
        Act::SaveMany(Box::new(Many {
            changes,
            mark: self.mark(spot),
        }))
    }

    fn plugin_change(&self, id: &str, key: &str, on: bool) -> Act {
        self.switch_change(Spot::Plugin(id.to_owned()), id, key, on, Vec::new())
    }

    fn switch_change(
        &self,
        spot: Spot,
        id: &str,
        key: &str,
        on: bool,
        more: Vec<SettingChange>,
    ) -> Act {
        let Some(path) = SitePath::plugin_switch(id, key) else {
            return Act::Stay;
        };
        let switch = SettingChange {
            target: SettingTarget::Path(path),
            value: Some(SettingValue::Bool(on)),
        };
        self.spot_save(spot, std::iter::once(switch).chain(more).collect())
    }

    fn toggle_plugin(&self, id: &str) -> Act {
        let Some((plugin, snapshot)) = self
            .snapshot
            .as_ref()
            .and_then(|s| Some((s.plugins.iter().find(|p| p.id == id)?, s)))
        else {
            return Act::Stay;
        };
        let row = entries::PluginRow::of(plugin, snapshot);
        if !row.installed {
            return self.plugin_change(id, wisp_protocol::settings::INSTALLED_KEY, true);
        }
        self.plugin_change(id, wisp_protocol::settings::ENABLED_KEY, !row.enabled)
    }

    fn remove_plugin(&mut self, id: String) -> Act {
        let installed = self.snapshot.as_ref().is_some_and(|s| {
            s.plugins.iter().any(|p| p.id == id)
                && s.plugin_on(&id, wisp_protocol::settings::INSTALLED_KEY)
        });
        if !installed {
            return Act::Stay;
        }
        let yes = self.plugin_change(&id, wisp_protocol::settings::INSTALLED_KEY, false);
        if !matches!(yes, Act::SaveMany(_)) {
            return yes;
        }
        self.ask = Some(Ask::remove(
            entries::remove_plugin_question(&sanitize(&id)),
            yes,
        ));
        Act::Stay
    }

    fn section_detail(&self, name: &str) -> Option<Detail> {
        let snapshot = self.snapshot.as_ref()?;
        let (plugin, slot) = snapshot.plugins.iter().find_map(|p| {
            p.slots
                .iter()
                .find(|s| s.name == name)
                .map(|slot| (p, slot))
        })?;
        let (on, source) = entries::section_on(slot, &self.display);
        let path = crate::app::sidebar::switch_path(name).unwrap_or_default();
        let mut notes = vec![sanitize(&slot.title)];
        if !plugin.enabled {
            notes.push(entries::PLUGIN_OFF.to_owned());
        }
        if let Some(wisp_protocol::slot::SlotDisabled::Cycle { cycle }) = &slot.disabled {
            let names: Vec<String> = cycle.iter().map(|n| sanitize(n)).collect();
            notes.push(format!("in a cycle: {}", names.join(" \u{2192} ")));
        }
        Some(Detail {
            lead: self.part_name(name),
            title: format!("the sidebar section \"{}\"", sanitize(&slot.title)),
            layers: vec![LayerLine {
                word: source,
                value: if on { "on" } else { "off" }.to_owned(),
                origin: sanitize(&path),
                winner: true,
                note: None,
            }],
            default: if slot.default_on { "on" } else { "off" }.to_owned(),
            notes,
            keys: vec![hint(if on {
                entries::PLUGIN_KEY_OFF
            } else {
                entries::PLUGIN_KEY_ON
            })],
            ..Detail::default()
        })
    }

    fn toggle_section(&self, name: &str) -> Act {
        if self.broken().is_some() {
            return Act::Hint(BROKEN);
        }
        let spot = Spot::Section(name.to_owned());
        if self.spot_saving(&spot) {
            return Act::Stay;
        }
        let Some(slot) = self.snapshot.as_ref().and_then(|s| {
            s.plugins
                .iter()
                .flat_map(|p| &p.slots)
                .find(|s| s.name == name)
        }) else {
            return Act::Stay;
        };
        let (on, _) = entries::section_on(slot, &self.display);
        let Some((plugin, section)) = name.split_once('/') else {
            return Act::Stay;
        };
        let Some(spec) = wisp_protocol::settings::find(wisp_protocol::settings::PLUGIN_SLOT) else {
            return Act::Stay;
        };
        let place = Place::PluginSlot {
            plugin: plugin.to_owned(),
            section: section.to_owned(),
        };
        Act::SaveMany(Box::new(Many {
            changes: vec![SettingChange {
                target: SettingTarget::Path(SitePath::new(place, spec)),
                value: Some(SettingValue::Bool(!on)),
            }],
            mark: self.mark(spot),
        }))
    }

    fn hook_entry(&self, name: &str) -> Option<(&HookEntry, &SettingsSnapshot)> {
        let snapshot = self.snapshot.as_ref()?;
        let entry = snapshot
            .plugins
            .iter()
            .flat_map(|p| &p.hooks)
            .find(|h| h.hook == name)?;
        Some((entry, snapshot))
    }

    fn hook_detail(&self, name: &str) -> Option<Detail> {
        let (entry, snapshot) = self.hook_entry(name)?;
        let row = entries::HookRow::of(entry, snapshot);
        let state = LayerLine {
            word: entries::PLUGIN_STATE_WORD.to_owned(),
            value: row.state(entry),
            origin: String::new(),
            winner: true,
            note: None,
        };
        let placed = entries::hook_placed(entry, snapshot).map(|p| LayerLine {
            word: p.layer.word().to_owned(),
            value: if row.value == Some(false) {
                "off"
            } else {
                "on"
            }
            .to_owned(),
            origin: sanitize(&p.path.shown()),
            winner: true,
            note: None,
        });
        let count = snapshot
            .plugins
            .iter()
            .flat_map(|p| &p.hooks)
            .filter(|h| h.point == entry.point)
            .count();
        let mut notes = vec![format!(
            "{}{SEP}{} of {count}",
            sanitize(&entry.point),
            u64::from(entry.order) + 1
        )];
        let names =
            |names: &[String]| -> Vec<String> { names.iter().map(|n| sanitize(n)).collect() };
        match &entry.disabled {
            Some(HookDisabled::Cycle { cycle }) => {
                notes.push(format!("in a cycle: {}", names(cycle).join(" \u{2192} ")));
            }
            Some(HookDisabled::Missing { requires }) => {
                notes.push(format!(
                    "requires {}, which doesn't run",
                    names(requires).join(", ")
                ));
            }
            _ => {}
        }
        Some(Detail {
            lead: self.part_name(name),
            title: format!("the hook \"{}\"", sanitize(&entry.point)),
            layers: std::iter::once(state).chain(placed).collect(),
            line: notes.get(1).cloned().unwrap_or_default(),
            fallback: notes.first().cloned().unwrap_or_default(),
            notes,
            keys: row.keys(entry),
            ..Detail::default()
        })
    }

    fn toggle_hook(&mut self, name: &str) -> Act {
        let Some((entry, snapshot)) = self.hook_entry(name) else {
            return Act::Stay;
        };
        if entry.disabled == Some(HookDisabled::LeftOut) {
            return Act::Stay;
        }
        if let Some(switch) = &entry.switch {
            let Some(spec) = wisp_protocol::settings::find(switch) else {
                return Act::Stay;
            };
            if entries::removed_plugin_of(snapshot, spec) {
                let id = spec.plugin.unwrap_or_default();
                return Act::Note(format!("add {} back in Plugins", sanitize(id)));
            }
            let name = spec.name;
            let spot = Spot::Setting(name);
            if !self.page_spots(&self.current).contains(&spot) {
                self.open_page(Page::setting(name));
            }
            self.filter.clear();
            self.selected = Some(spot);
            return Act::Stay;
        }
        if self.broken().is_some() {
            return Act::Hint(BROKEN);
        }
        let on = entries::HookRow::of(entry, snapshot).value != Some(false);
        let spot = Spot::Hook(name.to_owned());
        if self.spot_saving(&spot) {
            return Act::Stay;
        }
        let Some(path) = name
            .split_once('/')
            .and_then(|(plugin, hook)| SitePath::plugin_hook(plugin, hook))
        else {
            return Act::Stay;
        };
        Act::SaveMany(Box::new(Many {
            changes: vec![SettingChange {
                target: SettingTarget::Path(path),
                value: Some(SettingValue::Bool(!on)),
            }],
            mark: self.mark(spot),
        }))
    }

    fn search_detail(&self, quote: &Quote) -> Option<Detail> {
        let snapshot = self.snapshot.as_ref()?;
        let row = entries::SearchRow::of(snapshot)?;
        let mut lines: Vec<String> = row.chat_note().into_iter().collect();
        let mut keys = vec![];
        if let Some((_, saved)) = row.saved_switch(snapshot) {
            lines.push(saved);
            keys.extend([
                KeyHint::new(
                    "d",
                    format!("back to {}", wisp_render::chrome::config_screen::WISPS_OWN),
                ),
                KeyHint::note(NEW_CHATS),
            ]);
        }
        if let Some(on) = row.turns_on() {
            keys.push(hint(if on {
                entries::PLUGIN_KEY_ON
            } else {
                entries::PLUGIN_KEY_OFF
            }));
            if matches!(row.state, entries::SearchState::Now { .. }) {
                if quote.drops {
                    lines.push(copy::SEARCH_CHANGE_DROPS_REASONING.to_owned());
                } else {
                    keys.extend(self::notes(&quote.tools));
                }
            }
        }
        Some(Detail {
            lead: SEARCH_LEAD.to_owned(),
            title: help_title(entries::SEARCH_HELP),
            line: lines.first().cloned().unwrap_or_default(),
            notes: lines,
            applies: quote.tools.clone(),
            keys,
            ..Detail::default()
        })
    }

    fn toggle_search(&self, clear: bool) -> Act {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Act::Stay;
        };
        let Some(row) = entries::SearchRow::of(snapshot) else {
            return Act::Stay;
        };
        if clear {
            return match row.saved_switch(snapshot) {
                Some((path, _)) => self.spot_save(
                    Spot::Search,
                    vec![SettingChange {
                        target: SettingTarget::Path(path),
                        value: None,
                    }],
                ),
                None => Act::Stay,
            };
        }
        let Some(on) = row.turns_on() else {
            return Act::Stay;
        };
        let put_back = row
            .leaves_out()
            .filter(|_| on)
            .and_then(|left| {
                let spec = wisp_protocol::settings::find(names::TOOLS)?;
                let list = spec
                    .parse_list(&[left, std::slice::from_ref(&row.tool)].concat())
                    .ok()?;
                Some(SettingChange {
                    target: SettingTarget::Name(SettingName::of(spec)),
                    value: Some(list),
                })
            })
            .into_iter()
            .collect();
        self.switch_change(Spot::Search, &row.plugin, ENABLED_KEY, on, put_back)
    }

    fn add_detail(lead: &str, help: &str) -> Detail {
        Detail {
            lead: lead.to_owned(),
            title: help_title(help),
            keys: vec![hint(ADD_KEY)],
            ..Detail::default()
        }
    }

    fn page_detail(&self, page: &Page) -> Detail {
        let title = if *page == Page::Plugins {
            entries::PLUGINS_HELP.to_owned()
        } else {
            self.page_spots(page)
                .iter()
                .filter_map(|spot| match spot {
                    Spot::Open(page) => Some(page.title().to_owned()),
                    Spot::Setting(name) => {
                        self.row_named(name).map(|row| row.spec.label.to_owned())
                    }
                    _ => self.item_of(spot).map(|item| item.label.clone()),
                })
                .collect::<Vec<_>>()
                .join(SEP)
        };
        Detail {
            lead: page.title().to_owned(),
            title,
            keys: vec![hint(entries::OPEN_PAGE_KEY)],
            ..Detail::default()
        }
    }

    pub fn detail(&self, quote: &Quote) -> Option<Detail> {
        let spot = self.selected_spot()?;
        let label_width = self.label_width(spot);
        let mut shown = match spot {
            Spot::Open(page) => self.page_detail(page),
            Spot::Setting(_) => {
                let row = self.selected()?;
                self.field_detail(row, quote)
            }
            Spot::Endpoint(name) => self.endpoint_detail(name, quote)?,
            Spot::Model(id) => self.model_detail(id)?,
            Spot::AddEndpoint => Self::add_detail(account::ACCOUNT_TITLE, ADD_ENDPOINT_HELP),
            Spot::AddModel => Self::add_detail(MODEL_LEAD, ADD_MODEL_HELP),
            Spot::Plugin(id) => self.plugin_detail(id, quote)?,
            Spot::Section(name) => self.section_detail(name)?,
            Spot::Hook(name) => self.hook_detail(name)?,
            Spot::Search => self.search_detail(quote)?,
        };
        if let Some(ask) = &self.ask {
            shown.ask = Some(AskView {
                question: ask.question.clone(),
                options: ask.options,
                selected: ask.pick.focus,
            });
            shown.keys = hints(ASK_KEYS);
        }
        if self.editing {
            shown.keys.clear();
        }
        if let Some(reason) = self.refused.get(&self.mark(spot.clone())) {
            shown.line = reason.clone();
            shown.notes.insert(0, reason.clone());
        }
        shown.label_width = label_width;
        Some(shown)
    }

    fn label_width(&self, spot: &Spot) -> usize {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return 0;
        };
        let rows = match spot {
            Spot::Endpoint(_) => snapshot
                .endpoints
                .iter()
                .filter(|e| !e.keys_file)
                .flat_map(|e| endpoint_rows(snapshot, &e.name))
                .collect(),
            Spot::Model(id) => model_rows(snapshot, id),
            _ => Vec::new(),
        };
        rows.iter()
            .map(|row| row.spec.label.chars().count())
            .max()
            .unwrap_or(0)
    }

    pub fn selected_spot(&self) -> Option<&Spot> {
        let spot = self.selected.as_ref()?;
        self.shown().contains(spot).then_some(spot)
    }

    pub fn selected(&self) -> Option<&Row> {
        let name = self.selected.as_ref()?.setting()?;
        let query = Query::new(&self.filter);
        self.rows
            .iter()
            .filter(|row| row.spec.matches(&query))
            .find(|row| row.spec.name == name)
    }

    pub fn selected_endpoint(&self) -> Option<&str> {
        match self.selected_spot()? {
            Spot::Endpoint(name) => Some(name),
            _ => None,
        }
    }

    pub fn selected_entry(&self) -> Option<usize> {
        let spot = self.selected.as_ref()?;
        let (_, map) = self.entries();
        map.iter().position(|s| s.as_ref() == Some(spot))
    }

    fn keep_selection(&mut self) {
        if self.selected_spot().is_none() {
            self.selected = self.shown().first().cloned();
            self.top = 0;
        }
    }

    fn select_at(&mut self, position: usize) {
        let shown = self.shown();
        if let Some(spot) = shown.get(position.min(shown.len().saturating_sub(1))) {
            self.selected = Some(spot.clone());
        }
    }

    fn position(&self) -> Option<usize> {
        let spot = self.selected.as_ref()?;
        self.shown().iter().position(|s| s == spot)
    }

    pub fn move_by(&mut self, delta: isize) {
        let to = self
            .position()
            .map_or(0, |at| at.saturating_add_signed(delta));
        self.select_at(to);
    }

    pub fn first(&mut self) {
        self.select_at(0);
    }

    pub fn last(&mut self) {
        self.select_at(usize::MAX);
    }

    fn section_starts(&self) -> Vec<usize> {
        if self.filter.is_empty() || self.opened.is_some() {
            return Vec::new();
        }
        let shown = self.shown();
        let mut starts = vec![];
        let mut section = None;
        for (at, spot) in shown.iter().enumerate() {
            let Some(this) = self.section_of(spot) else {
                continue;
            };
            if section != Some(this) {
                section = Some(this);
                starts.push(at);
            }
        }
        starts
    }

    pub fn next_section(&mut self) {
        let at = self.position().unwrap_or(0);
        let starts = self.section_starts();
        if let Some(next) = starts.iter().find(|&&s| s > at) {
            self.select_at(*next);
        }
    }

    pub fn previous_section(&mut self) {
        let at = self.position().unwrap_or(0);
        let starts = self.section_starts();
        let current = starts.iter().rev().find(|&&s| s <= at).copied();
        let before = current.and_then(|c| starts.iter().rev().find(|&&s| s < c).copied());
        if let Some(to) = before.or(current) {
            self.select_at(to);
        }
    }

    fn page_rows(&self) -> isize {
        isize::try_from(self.page.saturating_sub(1).max(1)).unwrap_or(1)
    }

    pub fn fit(&mut self, rows: usize) {
        self.page = rows;
        let (entries, map) = self.entries();
        let total = entries.len();
        if rows == 0 {
            self.top = 0;
            return;
        }
        self.top = self.top.min(total.saturating_sub(rows));
        if let Some(at) = self.selected_entry() {
            let heading = at
                .checked_sub(1)
                .is_some_and(|before| map.get(before).is_some_and(Option::is_none));
            let want = if heading && rows > 1 { at - 1 } else { at };
            if want < self.top {
                self.top = want;
            }
            if at >= self.top + rows {
                self.top = at + 1 - rows;
            }
        }
    }

    pub fn scroll(&mut self, delta: isize) {
        if self.edit.is_some() || self.ask.is_some() {
            return;
        }
        let (entries, map) = self.entries();
        let page = self.page.max(1);
        let most = entries.len().saturating_sub(page);
        self.top = self.top.saturating_add_signed(delta).min(most);
        let Some(at) = self.selected_entry() else {
            return;
        };
        let visible = self.top..self.top + page;
        if visible.contains(&at) {
            return;
        }
        let inside = |e: &usize| visible.contains(e) && map.get(*e).is_some_and(Option::is_some);
        let pick = if at < self.top {
            (self.top..self.top + page).find(inside)
        } else {
            (self.top..self.top + page).rev().find(inside)
        };
        if let Some(spot) = pick.and_then(|e| map.get(e).cloned().flatten()) {
            self.selected = Some(spot);
        }
    }

    pub fn click(&mut self, entry: usize) {
        if self.edit.is_some() || self.ask.is_some() {
            return;
        }
        let (_, map) = self.entries();
        if let Some(spot) = map.get(entry).cloned().flatten() {
            self.selected = Some(spot);
        }
    }

    pub fn clear_filter(&mut self) -> bool {
        let had = !self.filter.is_empty() || self.editing;
        self.filter.clear();
        self.editing = false;
        self.keep_selection();
        had
    }

    pub fn paste(&mut self, text: &str) {
        if self.ask.is_some() {
            return;
        }
        if let Some(edit) = &mut self.edit {
            edit.paste(text);
        } else if self.editing {
            LineEdit::TEXT.paste(&mut self.filter, text);
            self.keep_selection();
        }
    }

    fn save(&self, row: &Row, value: Option<SettingValue>) -> Act {
        let reset = value.is_none();
        let place = if reset && self.edit.is_none() {
            row.path.clone()
        } else {
            self.place(row)
        };
        let by_path = self.opened.is_some() || self.has_sites(row);
        let (target, told) = match self.snapshot.as_ref() {
            Some(snapshot) if by_path => (
                SettingTarget::Path(place.clone()),
                if place == row.path && self.opened.is_none() {
                    told(row, reset)
                } else {
                    told_at(row, &place, snapshot)
                },
            ),
            _ => (
                SettingTarget::Name(SettingName::of(row.spec)),
                told(row, reset),
            ),
        };
        Act::Save(Box::new(Save {
            name: row.spec.name,
            change: SettingChange { target, value },
            told,
            save: true,
        }))
    }

    fn commit(&self, row: &Row, value: Option<SettingValue>) -> Act {
        let Some(Spot::Endpoint(name)) = self.opened() else {
            return self.save(row, value);
        };
        if !entries::is_connection(row.spec) {
            return self.save(row, value);
        }
        if value.is_some() && value == row.current {
            return Act::Stay;
        }
        match entries::connection_edit(row.spec, value.as_ref()) {
            Some((edit, store)) => Act::Endpoint(Box::new(Sent {
                op: EndpointOp::Edit {
                    name: name.clone(),
                    edit,
                    store,
                    check: true,
                },
                mark: self.mark(Spot::Setting(row.spec.name)),
            })),
            None => Act::Hint(NOT_RESETTABLE),
        }
    }

    fn open_widget(&mut self, name: &'static str) -> Act {
        if name == names::ENDPOINT && self.opened.is_none() {
            return self.open_page(Page::Accounts);
        }
        let Some(row) = self.row_named(name) else {
            return Act::Stay;
        };
        let sites = self.sites_for(row);
        let place = if self.opened.is_some() {
            sites
                .iter()
                .rfind(|s| s.saved)
                .map_or(&row.path, |s| &s.path)
        } else {
            &row.path
        };
        let saved = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| edit::held(row, place, snapshot));
        let current = if *place == row.path {
            row.current.clone()
        } else {
            saved.clone()
        };
        match widget(row.spec) {
            None => Act::Hint(NOT_YET),
            Some(Widget::Picker(pick)) => Act::Pick(pick),
            Some(Widget::Toggle) => self.commit(row, Some(toggled(current.as_ref()))),
            Some(Widget::Choice | Widget::Text | Widget::Checklist) => {
                match Edit::open(row, current.as_ref(), saved.as_ref()) {
                    Some(opened) => {
                        let opened = if opened.is_text() && sites.len() > 1 {
                            opened.for_sites(sites)
                        } else {
                            opened
                        };
                        let mark = self.mark(Spot::Setting(name));
                        self.refused.remove(&mark);
                        self.edit = Some(opened);
                        Act::Stay
                    }
                    None => Act::Hint(NOT_YET),
                }
            }
        }
    }

    fn change_connection(&mut self, row: &Row, reset: bool) -> Act {
        let Some(Spot::Endpoint(name)) = self.opened().cloned() else {
            return Act::Stay;
        };
        if reset {
            if entries::can_reset_connection(row.spec, row) {
                return self.commit(row, None);
            }
            return Act::Hint(if row.spec.name == names::KEY {
                KEY_STAYS
            } else {
                NOT_RESETTABLE
            });
        }
        if entries::prompts(row.spec) {
            return Act::Prompt {
                name,
                url: row.spec.name == names::URL,
            };
        }
        self.open_widget(row.spec.name)
    }

    fn change(&mut self, reset: bool) -> Act {
        if self.broken().is_some() {
            return Act::Hint(BROKEN);
        }
        let Some(row) = self.selected().cloned() else {
            return Act::Stay;
        };
        if self.is_saving(row.spec.name) {
            return Act::Stay;
        }
        if let Some(why) = self.locked(&row) {
            return Act::Hint(why);
        }
        if self.opened.is_some() && entries::is_connection(row.spec) {
            return self.change_connection(&row, reset);
        }
        if widget(row.spec).is_none() {
            return Act::Hint(NOT_YET);
        }
        if reset {
            if chat_only(&row) {
                let Some(value) = reset_value(&row) else {
                    return Act::Stay;
                };
                return Act::Save(Box::new(Save {
                    name: row.spec.name,
                    change: SettingChange {
                        target: SettingTarget::Name(SettingName::of(row.spec)),
                        value,
                    },
                    told: told(&row, false),
                    save: false,
                }));
            }
            if !can_reset(&row) {
                return Act::Hint(NOTHING_TO_RESET);
            }
            return self.commit(&row, None);
        }
        self.open_widget(row.spec.name)
    }

    fn save_edit(&mut self) -> Act {
        if self.broken().is_some() {
            return Act::Hint(BROKEN);
        }
        let Some(edit) = &self.edit else {
            return Act::Stay;
        };
        let Some(row) = self.row_named(edit.name) else {
            self.edit = None;
            return Act::Stay;
        };
        let value = edit.value(row.spec);
        let remove = matches!(&edit.field, Field::Text { text, .. } if text.trim().is_empty())
            && edit.site().is_some_and(|s| s.saved);
        if value.is_none() && !remove {
            return Act::Stay;
        }
        let act = self.commit(row, value);
        self.edit = None;
        act
    }

    fn widget_key(&mut self, key: Key) -> Act {
        let spec = self
            .edit
            .as_ref()
            .and_then(|e| self.row_named(e.name))
            .map(|r| r.spec);
        let Some(spec) = spec else {
            self.edit = None;
            return Act::Stay;
        };
        let Some(edit) = self.edit.as_mut() else {
            return Act::Stay;
        };
        if key.alt {
            return Act::Stay;
        }
        let text = edit.is_text();
        match key.code {
            KeyCode::Char('c' | 'C') if key.ctrl => {
                if edit.is_empty() {
                    self.edit = None;
                } else {
                    edit.clear();
                }
            }
            KeyCode::Char('u' | 'U') if key.ctrl => edit.clear(),
            KeyCode::Char('a' | 'A') if key.ctrl => edit.home(),
            KeyCode::Char('e' | 'E') if key.ctrl => edit.end(),
            KeyCode::Char('l' | 'L') if key.ctrl => return Act::Repaint,
            KeyCode::Char('z' | 'Z') if key.ctrl => return Act::Suspend,
            _ if key.ctrl => {}
            KeyCode::Esc => self.edit = None,
            KeyCode::Char('q') if !text => self.edit = None,
            KeyCode::Enter => return self.save_edit(),
            KeyCode::Tab if key.shift => edit.next_site(-1),
            KeyCode::Tab => edit.next_site(1),
            KeyCode::BackTab => edit.next_site(-1),
            KeyCode::Char(' ') if edit.is_checklist() => edit.toggle(),
            KeyCode::Left if !text => edit.shift(-1, spec),
            KeyCode::Right if !text => edit.shift(1, spec),
            KeyCode::Char('h') if !text => edit.shift(-1, spec),
            KeyCode::Char('l') if !text => edit.shift(1, spec),
            KeyCode::Left => edit.left(),
            KeyCode::Right => edit.right(),
            KeyCode::Home => edit.home(),
            KeyCode::End => edit.end(),
            KeyCode::Backspace => edit.backspace(),
            KeyCode::Delete => edit.delete(),
            KeyCode::Char(c) if text => edit.insert(c),
            _ => {}
        }
        Act::Stay
    }

    fn edit_key(&mut self, key: Key) -> Act {
        if key.alt {
            return Act::Stay;
        }
        match key.code {
            KeyCode::Char('c' | 'C') if key.ctrl => {
                if self.filter.is_empty() {
                    self.editing = false;
                } else {
                    self.filter.clear();
                    self.keep_selection();
                }
            }
            KeyCode::Backspace if key.ctrl => {}
            _ if LineEdit::TEXT.key(&mut self.filter, key) => self.keep_selection(),
            _ if key.ctrl => {}
            KeyCode::Esc => {
                self.clear_filter();
            }
            KeyCode::Enter | KeyCode::Tab => self.editing = false,
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            _ => {}
        }
        Act::Stay
    }

    fn ask_key(&mut self, key: Key) -> Act {
        if key.alt {
            return Act::Stay;
        }
        match key.code {
            KeyCode::Char('c' | 'C') if key.ctrl => Act::Interrupt,
            KeyCode::Char('l' | 'L') if key.ctrl => Act::Repaint,
            KeyCode::Char('z' | 'Z') if key.ctrl => Act::Suspend,
            _ if key.ctrl => Act::Stay,
            KeyCode::Char('q') | KeyCode::Esc => {
                self.ask = None;
                Act::Stay
            }
            _ => match self.ask.as_mut().map(|ask| ask.pick.key(key)) {
                Some(Picked::Took(row)) => match self.ask.take() {
                    Some(ask) if row == 0 => self.still_wanted(ask.yes),
                    _ => Act::Stay,
                },
                _ => Act::Stay,
            },
        }
    }

    fn snapshot_endpoint(&self) -> Option<&str> {
        self.snapshot.as_ref().map(|s| s.context.endpoint.as_str())
    }

    fn still_wanted(&self, yes: Act) -> Act {
        if let Act::Endpoint(sent) = &yes
            && let EndpointOp::Remove { name, .. } = &sent.op
            && let Some(refused) = self.removal_refused(name)
        {
            return refused;
        }
        yes
    }

    fn removal_refused(&self, name: &str) -> Option<Act> {
        if self.broken().is_some() {
            return Some(Act::Hint(BROKEN));
        }
        if self.keys_file(name) {
            return Some(Act::Note(self.keys_file_only(NOT_REMOVABLE)));
        }
        if self.snapshot_endpoint() == Some(name) || self.live.as_deref() == Some(name) {
            return Some(Act::Hint(SWITCH_FIRST));
        }
        if self.spot_saving(&Spot::Endpoint(name.to_owned())) {
            return Some(Act::Stay);
        }
        None
    }

    fn enter_spot(&mut self) -> Act {
        match self.selected_spot().cloned() {
            Some(Spot::Open(page)) => self.open_page(page),
            Some(Spot::Setting(_)) => self.change(false),
            Some(Spot::Endpoint(name)) => self.use_endpoint(name),
            Some(Spot::Model(id)) if model_hidden(&id) || id.contains(NOT_SHOWN) => {
                Act::Hint(edit::HIDDEN_MODEL_LOCK)
            }
            Some(spot @ Spot::Model(_)) => self.enter_entry(spot),
            Some(Spot::AddEndpoint) => Act::AddEndpoint,
            Some(Spot::AddModel) => Act::AddModel,
            Some(Spot::Section(name)) => self.toggle_section(&name),
            Some(Spot::Plugin(id)) => self.toggle_plugin(&id),
            Some(Spot::Hook(name)) => self.toggle_hook(&name),
            Some(Spot::Search) => self.toggle_search(false),
            None => Act::Stay,
        }
    }

    fn remove_endpoint(&mut self, name: String) -> Act {
        if let Some(refused) = self.removal_refused(&name) {
            return refused;
        }
        let spot = Spot::Endpoint(name.clone());
        let store = self
            .snapshot
            .as_ref()
            .and_then(|s| entries::store_of(s, &name));
        let place = match store.as_deref().and_then(StoreChoice::parse) {
            Some(StoreChoice::Keyring) => Some(IN_KEYCHAIN),
            Some(StoreChoice::File) => Some(CREDENTIALS),
            _ => None,
        };
        self.ask = Some(Ask::remove(
            card_words::remove_question(&sanitize(&name), place),
            Act::Endpoint(Box::new(Sent {
                op: EndpointOp::Remove {
                    name,
                    keep_key: false,
                },
                mark: self.mark(spot),
            })),
        ));
        Act::Stay
    }

    fn remove_model(&mut self, id: String) -> Act {
        if self.broken().is_some() {
            return Act::Hint(BROKEN);
        }
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Act::Stay;
        };
        let paths = entries::model_saved_paths(snapshot, &id);
        if paths.is_empty() {
            return Act::Hint(NOTHING_FOR_MODEL);
        }
        let spot = Spot::Model(id.clone());
        if self.spot_saving(&spot) {
            return Act::Stay;
        }
        let changes = paths
            .into_iter()
            .map(|path| SettingChange {
                target: SettingTarget::Path(path),
                value: None,
            })
            .collect();
        let label = self
            .item_of(&spot)
            .map_or_else(|| id.clone(), |i| i.label.clone());
        self.ask = Some(Ask::remove(
            remove_model_question(&label),
            Act::SaveMany(Box::new(Many {
                changes,
                mark: self.mark(spot),
            })),
        ));
        Act::Stay
    }

    fn d_spot(&mut self) -> Act {
        match self.selected_spot().cloned() {
            Some(Spot::Setting(_)) => self.change(true),
            Some(Spot::Endpoint(name)) => self.remove_endpoint(name),
            Some(Spot::Model(id)) => self.remove_model(id),
            Some(Spot::Plugin(id)) => self.remove_plugin(id),
            Some(Spot::Search) => self.toggle_search(true),
            Some(
                Spot::Open(_)
                | Spot::AddEndpoint
                | Spot::AddModel
                | Spot::Section(_)
                | Spot::Hook(_),
            )
            | None => Act::Stay,
        }
    }

    fn use_endpoint(&self, name: String) -> Act {
        if self.broken().is_some() {
            return Act::Hint(BROKEN);
        }
        if self.spot_saving(&Spot::Endpoint(name.clone()))
            || self.is_saving(names::ENDPOINT)
            || self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.context.endpoint == name
                    && entries::summary_of(snapshot, &name).is_some_and(|e| e.default)
            })
        {
            return Act::Stay;
        }
        Act::UseEndpoint(name)
    }

    fn open_page(&mut self, page: Page) -> Act {
        self.current = page;
        self.filter.clear();
        self.editing = false;
        self.selected = None;
        self.top = 0;
        self.keep_selection();
        Act::Stay
    }

    fn into(&mut self) -> Act {
        match self.selected_spot().cloned() {
            Some(Spot::Plugin(id)) => self.open_page(Page::Plugin(id)),
            Some(Spot::Endpoint(name)) if self.keys_file(&name) => {
                Act::Note(self.keys_file_only(NO_FIELDS))
            }
            Some(spot @ Spot::Endpoint(_)) => self.enter_entry(spot),
            Some(Spot::Model(_)) => self.enter_spot(),
            Some(Spot::Open(page)) => self.open_page(page),
            _ => Act::Stay,
        }
    }

    fn back(&mut self) -> Act {
        if self.opened.is_some() {
            self.leave_entry();
        } else if let Some(parent) = self.current.parent() {
            let selected = match &self.current {
                Page::Accounts => Spot::Setting(names::ENDPOINT),
                Page::Plugin(id) => Spot::Plugin(id.clone()),
                page => Spot::Open(page.clone()),
            };
            self.open_page(parent);
            self.selected = Some(selected);
        } else {
            return Act::Close;
        }
        Act::Stay
    }

    pub fn key(&mut self, key: Key) -> Act {
        if self.ask.is_some() && !self.loading() {
            return self.ask_key(key);
        }
        if self.edit.is_some() && !self.loading() {
            return self.widget_key(key);
        }
        if self.editing {
            return self.edit_key(key);
        }
        if key.alt {
            return Act::Stay;
        }
        match key.code {
            KeyCode::Char('c' | 'C') if key.ctrl => return Act::Interrupt,
            KeyCode::Char('l' | 'L') if key.ctrl => return Act::Repaint,
            KeyCode::Char('z' | 'Z') if key.ctrl => return Act::Suspend,
            _ if key.ctrl => {}
            KeyCode::Esc if self.expanded => self.expanded = false,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.clear_filter();
            }
            KeyCode::Esc | KeyCode::Char('q') => return self.back(),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-self.page_rows()),
            KeyCode::PageDown => self.move_by(self.page_rows()),
            KeyCode::Home | KeyCode::Char('g') => self.first(),
            KeyCode::End | KeyCode::Char('G') => self.last(),
            KeyCode::Tab if key.shift => self.previous_section(),
            KeyCode::Tab => self.next_section(),
            KeyCode::BackTab => self.previous_section(),
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Char('?') => self.expanded = !self.expanded,
            KeyCode::Enter if !self.loading() => return self.enter_spot(),
            KeyCode::Char('d') if !self.loading() => return self.d_spot(),
            KeyCode::Right if !self.loading() => return self.into(),
            KeyCode::Left
                if !self.loading()
                    && (self.opened.is_some() || self.current.parent().is_some()) =>
            {
                return self.back();
            }
            _ => {}
        }
        Act::Stay
    }
}

#[cfg(test)]
mod tests;
