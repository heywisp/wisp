use std::time::Duration;

use wisp_protocol::filter::Query;
use wisp_protocol::settings::{Chat, Landing, Offer, effort_on, every, offered, reachable};
use wisp_protocol::{Effort, RequestId, starts_charging};
use wisp_render::chrome::account::{self, AccountKind, AccountState, Said, SaidWords, kind_words};
use wisp_render::chrome::{
    Choice, Dock, Effect, EndpointFacts, Facts, Head, KeyHint, Menu, ModelFacts, ModelRates, Shape,
    StartPick, Tone, Under, chat_of, endpoint_card, named_model, offers, short_model,
};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::copy;

use super::settings::Tracker;
use crate::app::list::next_enabled;
use crate::term::{Key, KeyCode};
use crate::widgets::line_edit::LineEdit;
use wisp_protocol::NOT_SHOWN;
use wisp_protocol::{MAX_MODEL_ID, model_id_char, model_looks_like_key, valid_model_id};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Picked {
    Settings {
        endpoint: Option<String>,
        model: Option<String>,
        effort: Option<Effort>,
    },
    Effort {
        endpoint: Option<String>,
        model: String,
        preselect: Option<Effort>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    Done(Picked),
    Add,
    SignIn { account: String, then: Picked },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Model {
        query: String,
        open: Option<String>,
    },
    Effort {
        endpoint: Option<String>,
        model: String,
        chained: bool,
        query: String,
    },
    Endpoint {
        query: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Account {
    pub(crate) name: String,
    pub(crate) provider: String,
    kind: Option<AccountKind>,
    state: AccountState,
    unavailable: Option<String>,
    mark: Option<u8>,
}

impl Account {
    fn usable(&self) -> bool {
        self.unavailable.is_none() || self.state == AccountState::Expired
    }

    pub(crate) fn head(&self, at: usize, glyphs: &Glyphs) -> (String, Head) {
        let (detail, short) = self.head_words(glyphs);
        (
            detail,
            Head {
                at,
                short,
                after: String::new(),
                tone: self.tone(),
            },
        )
    }

    pub(crate) fn folded(
        &self,
        facts: &Facts,
        tracker: &Tracker,
        at: usize,
        model: &str,
        effort: Option<Effort>,
        glyphs: &Glyphs,
    ) -> (Choice, Head, String) {
        let effort = shown_effort(facts, model, effort).map_or("", Effort::as_str);
        let (detail, head) = self.head(at, glyphs);
        let choice = Choice {
            current: self.name == tracker.wanted().endpoint,
            label: account::marked(&self.name, self.mark, glyphs),
            mark: self.mark,
            detail,
            effect: format!("{} {effort}", short_model(model))
                .trim_end()
                .to_owned(),
            enabled: self.usable(),
            ..Choice::default()
        };
        (choice, head, model.to_owned())
    }

    pub(crate) fn no_carry(&self, from: Option<&Account>) -> Option<String> {
        from.filter(|f| f.name != self.name && !f.provider.eq_ignore_ascii_case(&self.provider))
            .map(|_| account::no_carry(&self.name))
    }

    fn head_words(&self, glyphs: &Glyphs) -> (String, String) {
        let word = account::billing_word(self.state).to_owned();
        let kind = self
            .kind
            .map_or_else(|| word.clone(), |k| kind_words(k, glyphs));
        match (&self.unavailable, self.state) {
            (_, AccountState::Expired) => (format!("{kind}{}{word}", sep(glyphs)), word),
            (Some(reason), _) => (format!("{kind}{}{reason}", sep(glyphs)), reason.clone()),
            (None, _) => (kind, word),
        }
    }

    fn tone(&self) -> wisp_render::theme::Token {
        if self.unavailable.is_some() || self.state == AccountState::Expired {
            wisp_render::theme::Token::Error
        } else {
            account::billing_token(self.state)
        }
    }
}

fn sep(glyphs: &Glyphs) -> String {
    format!(" {} ", glyphs.separator)
}

pub(crate) fn chat_account(facts: &Facts, tracker: &Tracker) -> Account {
    let name = tracker.wanted().endpoint;
    Account {
        kind: facts
            .credential
            .map(|c| AccountKind::of(c, facts.endpoint, &facts.provider)),
        state: facts.account_state(&name),
        provider: facts.provider.clone(),
        mark: facts.mark_of(&name),
        name,
        unavailable: None,
    }
}

pub(crate) fn accounts(facts: &Facts, tracker: &Tracker) -> Vec<Account> {
    if facts.endpoints.is_empty() {
        return vec![chat_account(facts, tracker)];
    }
    facts
        .endpoints
        .iter()
        .map(|e| Account {
            name: e.name.clone(),
            provider: e.provider.clone(),
            kind: e.account_kind(),
            state: facts.account_state(&e.name),
            unavailable: e.unavailable.clone(),
            mark: e.mark,
        })
        .collect()
}

type Listing = fn(&[Offer<'_>], Landing<'_>, Chat<'_>) -> Vec<String>;

fn family_on<'a>(facts: &'a Facts, account: &str, id: &str) -> Option<&'a str> {
    facts.landing(account).models?;
    Some(&facts.model_facts(id)?.variant.as_ref()?.family)
}

fn models_on(facts: &Facts, account: &Account, chat: &str, listing: Listing) -> Vec<String> {
    listing(
        &offers(&facts.models),
        facts.landing(&account.name),
        chat_of(&facts.models, chat),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub text: String,
    pub label: String,
    pub enabled: bool,
    pub matches: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Model(String),
    Listed { account: String, id: String },
    Header(String),
    Free(String),
    Level(Effort),
    Endpoint(String),
    Add,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picker {
    kind: Kind,
    selected: Option<usize>,
    preselect: Option<Effort>,
    checking: Option<RequestId>,
    saves: bool,
    start: Option<Start>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Start {
    head: Vec<Said>,
    on: String,
    mark: Option<u8>,
}

fn money(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("${v:.0}")
    } else {
        format!("${v:.2}")
    }
}

fn price(rates: Option<&ModelRates>, sep: &str) -> String {
    rates.map_or_else(
        || copy::COST_UNKNOWN.to_owned(),
        |r| {
            format!(
                "{}{sep}{}",
                money(r.input_per_mtok),
                money(r.output_per_mtok)
            )
        },
    )
}

fn endpoint_detail(endpoint: &EndpointFacts, sep: &str) -> String {
    endpoint
        .kind()
        .map_or_else(String::new, |label| match &label.host {
            Some(host) => format!("{}{sep}{host}", label.name),
            None => label.name.to_owned(),
        })
}

fn free_row(rows: &mut Vec<(Row, bool)>, query: &str) {
    if !query.is_empty() && rows.is_empty() {
        let usable = valid_model_id(query) && !model_looks_like_key(query);
        rows.push((Row::Free(query.to_owned()), usable));
    }
}

fn query_notes(rows: &[(Row, bool)], query: &str, priced: bool) -> Vec<(String, Tone)> {
    let mut notes = Vec::new();
    if rows.iter().any(|(r, _)| matches!(r, Row::Free(_))) {
        notes.push((copy::no_capability_data(priced), Tone::Note));
    }
    notes.push((copy::matching(&typed_label(query)), Tone::Faint));
    notes
}

fn typed_label(id: &str) -> String {
    wisp_protocol::shown_text(id).to_owned()
}

impl Picker {
    pub fn models() -> Picker {
        Picker {
            kind: Kind::Model {
                query: String::new(),
                open: None,
            },
            selected: None,
            preselect: None,
            checking: None,
            saves: false,
            start: None,
        }
    }

    pub fn endpoints() -> Picker {
        Picker {
            kind: Kind::Endpoint {
                query: String::new(),
            },
            selected: None,
            preselect: None,
            checking: None,
            saves: false,
            start: None,
        }
    }

    pub fn starting(pick: StartPick, on: &str, mark: Option<u8>) -> Picker {
        Picker {
            start: Some(Start {
                head: pick.head,
                on: on.to_owned(),
                mark,
            }),
            ..Picker::models().queried(&pick.query)
        }
    }

    #[must_use]
    pub fn opened(mut self, account: String) -> Picker {
        if let Kind::Model { open, .. } = &mut self.kind {
            *open = Some(account);
        }
        self.selected = None;
        self
    }

    #[must_use]
    pub fn saving(mut self) -> Picker {
        self.saves = true;
        self
    }

    pub fn saves(&self) -> bool {
        self.saves
    }

    #[must_use]
    pub fn asking(mut self, request: RequestId) -> Picker {
        self.checking = Some(request);
        self
    }

    pub fn answered(&mut self, request: RequestId) {
        if self.checking == Some(request) {
            self.checking = None;
        }
    }

    fn quoted(&self, effect: String, glyphs: &Glyphs) -> String {
        match self.checking {
            Some(_) => format!(
                "{effect} {} {}",
                glyphs.separator,
                copy::ongoing(copy::CHECKING_COMPACTION, glyphs)
            ),
            None => effect,
        }
    }

    pub fn efforts(model: String, chained: bool, preselect: Option<Effort>) -> Picker {
        Picker::efforts_on(None, model, chained, preselect)
    }

    pub fn efforts_on(
        endpoint: Option<String>,
        model: String,
        chained: bool,
        preselect: Option<Effort>,
    ) -> Picker {
        Picker {
            kind: Kind::Effort {
                endpoint,
                model,
                chained,
                query: String::new(),
            },
            selected: None,
            preselect,
            checking: None,
            saves: false,
            start: None,
        }
    }

    pub fn query(&self) -> Option<&str> {
        match &self.kind {
            Kind::Model { query, .. } | Kind::Endpoint { query } => Some(query),
            Kind::Effort { .. } => None,
        }
    }

    pub fn queried(mut self, text: &str) -> Picker {
        match &mut self.kind {
            Kind::Endpoint { query } => *query = text.to_lowercase(),
            Kind::Model { query, .. } | Kind::Effort { query, .. } => *query = text.to_owned(),
        }
        self.selected = None;
        self
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.kind, Kind::Effort { .. })
    }

    fn grouped(&self) -> bool {
        matches!(self.kind, Kind::Model { .. }) && !self.saves
    }

    pub fn values(&self, facts: &Facts, tracker: &Tracker) -> Vec<Value> {
        let level_query = match &self.kind {
            Kind::Effort { query, .. } => query.to_ascii_lowercase(),
            Kind::Model { .. } | Kind::Endpoint { .. } => String::new(),
        };
        self.rows(facts, tracker)
            .into_iter()
            .map(|(row, enabled)| match row {
                Row::Listed { account, id } => Value {
                    label: family_on(facts, &account, &id)
                        .map_or_else(|| short_model(&id), str::to_owned),
                    text: id,
                    enabled,
                    matches: true,
                },
                Row::Model(text) => Value {
                    label: short_model(&text),
                    text,
                    enabled,
                    matches: true,
                },
                Row::Header(name) => Value {
                    text: String::new(),
                    label: name,
                    enabled: false,
                    matches: true,
                },
                Row::Free(text) => Value {
                    label: typed_label(&text),
                    text,
                    enabled,
                    matches: true,
                },
                Row::Endpoint(text) => Value {
                    label: text.clone(),
                    text,
                    enabled,
                    matches: true,
                },
                Row::Add => Value {
                    text: copy::ADD_WORD.to_owned(),
                    label: if self.saves {
                        endpoint_card::add_row(GlyphSet::Ascii)
                    } else {
                        account::sign_in_row(&Glyphs::of(GlyphSet::Ascii))
                    },
                    enabled: enabled && !matches!(self.kind, Kind::Model { .. }),
                    matches: true,
                },
                Row::Level(level) => Value {
                    text: level.as_str().to_owned(),
                    label: level.as_str().to_owned(),
                    enabled,
                    matches: level.as_str().starts_with(&level_query),
                },
            })
            .collect()
    }

    pub fn is_chained(&self) -> bool {
        matches!(self.kind, Kind::Effort { chained: true, .. })
    }

    fn flat_models(facts: &Facts, query: &str) -> Vec<(Row, bool)> {
        let filter = Query::new(query);
        let mut rows: Vec<(Row, bool)> = facts
            .models
            .iter()
            .filter(|m| m.shipped || m.listed.is_some())
            .filter(|m| filter.matches(&[&m.id]))
            .map(|m| {
                (
                    Row::Model(m.id.clone()),
                    facts.model_selectable(
                        &m.id,
                        facts
                            .next
                            .as_ref()
                            .and_then(|next| next.endpoint.as_deref()),
                    ),
                )
            })
            .collect();
        free_row(&mut rows, query);
        rows
    }

    fn grouped_models(
        facts: &Facts,
        tracker: &Tracker,
        query: &str,
        open: Option<&str>,
    ) -> Vec<(Row, bool)> {
        let wanted = tracker.wanted();
        let filter = Query::new(query);
        let open = open.filter(|_| filter.is_empty());
        let mut rows: Vec<(Row, bool)> = Vec::new();
        for a in accounts(facts, tracker) {
            let ids: Vec<String> = match open {
                Some(open) if open == a.name => models_on(facts, &a, &wanted.model, every),
                Some(_) => models_on(facts, &a, &wanted.model, offered),
                None if filter.matches(&[&a.name]) => models_on(facts, &a, &wanted.model, offered),
                None => models_on(facts, &a, &wanted.model, reachable)
                    .into_iter()
                    .filter(|id| filter.matches(&[&a.name, id]))
                    .collect(),
            };
            if ids.is_empty() {
                continue;
            }
            if open.is_some_and(|open| open != a.name) {
                rows.push((Row::Endpoint(a.name.clone()), a.usable()));
                continue;
            }
            rows.push((Row::Header(a.name.clone()), false));
            let mut families = Vec::new();
            for id in &ids {
                let id = if let Some(family) = family_on(facts, &a.name, id) {
                    if families.contains(&family) {
                        continue;
                    }
                    families.push(family);
                    let own = facts.model_facts(id).and_then(|m| m.variant.as_ref());
                    let survivors = ids.iter().filter_map(|id| {
                        Some((id.as_str(), facts.model_facts(id)?.variant.as_ref()?))
                    });
                    let siblings = own
                        .map(|own| wisp_protocol::settings::siblings(survivors, own))
                        .unwrap_or_default();
                    let levels: Vec<_> = siblings.iter().map(|(level, _)| *level).collect();
                    let level = effort_on(&levels, wanted.effort);
                    if a.name == wanted.endpoint
                        && siblings.iter().any(|(_, id)| *id == wanted.model)
                    {
                        wanted.model.as_str()
                    } else {
                        siblings
                            .into_iter()
                            .find(|(effort, _)| Some(*effort) == level)
                            .map_or(id.as_str(), |(_, id)| id)
                    }
                } else {
                    id.as_str()
                };
                let enabled = a.usable() && facts.model_selectable(id, Some(&a.name));
                rows.push((
                    Row::Listed {
                        account: a.name.clone(),
                        id: id.to_owned(),
                    },
                    enabled,
                ));
            }
        }
        free_row(&mut rows, query);
        if filter.is_empty() {
            rows.push((Row::Add, true));
        }
        rows
    }

    fn rows(&self, facts: &Facts, tracker: &Tracker) -> Vec<(Row, bool)> {
        match &self.kind {
            Kind::Model { query, .. } if self.saves => Picker::flat_models(facts, query),
            Kind::Model { query, open } => {
                let mut rows = Picker::grouped_models(facts, tracker, query, open.as_deref());
                rows.retain(|(row, _)| self.start.is_none() || !matches!(row, Row::Free(_)));
                rows
            }
            Kind::Effort { model, .. } => {
                let caps = facts.model_facts(model).and_then(|m| m.caps.as_ref());
                let levels: Vec<Effort> = caps.map_or(Effort::ALL.to_vec(), |c| c.efforts.clone());
                levels.into_iter().map(|l| (Row::Level(l), true)).collect()
            }
            Kind::Endpoint { query } => {
                let filter = Query::new(query);
                let mut rows: Vec<(Row, bool)> = accounts(facts, tracker)
                    .into_iter()
                    .filter(|a| filter.matches(&[&a.name]))
                    .map(|a| {
                        let usable = a.usable();
                        (Row::Endpoint(a.name), usable)
                    })
                    .collect();
                if copy::ADD_WORD.starts_with(query.as_str()) {
                    rows.push((Row::Add, true));
                }
                rows
            }
        }
    }

    fn initial(&self, rows: &[(Row, bool)], facts: &Facts, tracker: &Tracker) -> Option<usize> {
        let wanted = tracker.wanted();
        let hit = rows.iter().position(|(row, _)| match row {
            Row::Model(id) => *id == wanted.model,
            Row::Listed { account, id } => *account == wanted.endpoint && *id == wanted.model,
            Row::Free(_) | Row::Header(_) | Row::Add => false,
            Row::Level(l) => Some(*l) == self.preselect.or(wanted.effort),
            Row::Endpoint(name) => *name == wanted.endpoint,
        });
        if self.start.is_none() {
            let listed = rows
                .iter()
                .position(|(row, enabled)| *enabled && matches!(row, Row::Listed { .. }));
            return hit
                .or(listed)
                .or_else(|| rows.iter().position(|(_, enabled)| *enabled));
        }
        let from = facts.billing_on(&wanted.endpoint);
        let named = self
            .query()
            .and_then(|query| named_model(&facts.models, query))
            .map(|m| m.id.as_str());
        let free = |model: Option<&str>| {
            rows.iter().position(|(row, enabled)| match row {
                Row::Listed { account, id } => {
                    *enabled
                        && model.is_none_or(|m| m == id)
                        && !starts_charging(Some(from), facts.billing_on(account))
                }
                _ => false,
            })
        };
        hit.or_else(|| named.and_then(|m| free(Some(m))))
            .or_else(|| free(None))
    }

    fn selection(&self, rows: &[(Row, bool)], facts: &Facts, tracker: &Tracker) -> Option<usize> {
        match self.selected {
            Some(i) if i < rows.len() && rows[i].1 => Some(i),
            _ => self.initial(rows, facts, tracker),
        }
    }

    fn search_notes(
        &self,
        rows: &[(Row, bool)],
        facts: &Facts,
        tracker: &Tracker,
        set: GlyphSet,
    ) -> (Vec<Option<String>>, Vec<String>) {
        let wanted = tracker.wanted();
        let pairs: Vec<Option<(&str, String)>> = rows
            .iter()
            .map(|(row, enabled)| match row {
                _ if !*enabled => None,
                Row::Model(id) | Row::Free(id) => Some((wanted.endpoint.as_str(), id.clone())),
                Row::Listed { account, id } => Some((account.as_str(), id.clone())),
                Row::Endpoint(name) => {
                    Some((name.as_str(), shown_pair(facts, tracker, name).model))
                }
                Row::Level(_) | Row::Add | Row::Header(_) => None,
            })
            .collect();
        let shifts: Vec<bool> = pairs
            .iter()
            .map(|pair| {
                pair.as_ref()
                    .is_some_and(|(e, m)| tracker.changes_search(facts, e, m))
            })
            .collect();
        let notes: Vec<Option<String>> = pairs
            .iter()
            .map(|pair| {
                pair.as_ref()
                    .and_then(|(e, m)| tracker.search_note(facts, e, m, set))
            })
            .collect();
        let drops = pairs
            .iter()
            .flatten()
            .any(|(e, m)| tracker.drops_reasoning(facts, e, m));
        let mut staying = notes
            .iter()
            .zip(&shifts)
            .filter(|(_, shift)| !**shift)
            .filter_map(|(note, _)| note.as_ref());
        let first = staying.next().cloned();
        let shared = first.filter(|note| staying.all(|other| other == note));
        let (notes, mut below) = match shared {
            Some(note) => (
                notes
                    .into_iter()
                    .zip(&shifts)
                    .map(|(n, shift)| n.filter(|_| *shift))
                    .collect(),
                vec![note],
            ),
            None => (notes, vec![]),
        };
        if drops {
            below.push(copy::SEARCH_CHANGE_DROPS_REASONING.to_owned());
        }
        (notes, below)
    }

    pub fn menu(&self, facts: &Facts, tracker: &Tracker, at: Duration, set: GlyphSet) -> Menu {
        self.menu_with(facts, tracker, at, set, true)
    }

    pub fn docked(&self, facts: &Facts, tracker: &Tracker, at: Duration, set: GlyphSet) -> Menu {
        self.menu_with(facts, tracker, at, set, false)
    }

    pub fn dock(&self, menu: &Menu, set: GlyphSet) -> Dock {
        let glyphs = Glyphs::of(set);
        if let Some(start) = &self.start {
            let first = match menu.selected {
                None => copy::arrows(set),
                Some(_) => KeyHint::new(copy::ENTER_KEY, account::START_WORD),
            };
            let on = account::marked(&start.on, start.mark, &glyphs);
            return Dock {
                name: copy::MODEL_TITLE.to_owned(),
                keys: vec![first, copy::esc(&account::stay_on(&on))],
                head: start
                    .head
                    .iter()
                    .map(|said| said.marked(&glyphs))
                    .collect::<Vec<_>>()
                    .join(&sep(&glyphs)),
                exit_short: account::STAY_WORD.to_owned(),
                mark: start.mark,
                ..Dock::default()
            };
        }
        let filtered = vec![
            KeyHint::new(copy::ENTER_KEY, copy::USE_WORD),
            KeyHint::new(copy::KEY_TYPE, copy::TO_FILTER),
            copy::esc(copy::CLOSE_WORD),
        ];
        let on_account = menu
            .selected
            .is_some_and(|at| menu.shape.heads.iter().any(|head| head.at == at));
        let switching = |fold: Option<(&str, &str)>| {
            let fold = fold.map(|(key, word)| KeyHint::new(key, word));
            let enter = KeyHint::new(copy::ENTER_KEY, account::SWITCH_WORD);
            let rest = [
                KeyHint::new(copy::KEY_TYPE, copy::TO_FILTER),
                copy::esc(copy::CLOSE_WORD),
            ];
            [enter].into_iter().chain(fold).chain(rest).collect()
        };
        let (name, column, keys) = match &self.kind {
            Kind::Model { .. } if self.saves => (copy::MODEL_TITLE, copy::NAME_COLUMN, filtered),
            Kind::Model { .. } => {
                let fold = if on_account {
                    (glyphs.arrow, account::MODELS_WORD)
                } else {
                    (glyphs.back, account::ACCOUNTS_WORD)
                };
                (copy::MODEL_TITLE, "", switching(Some(fold)))
            }
            Kind::Endpoint { .. } => (
                account::ACCOUNT_TITLE,
                "",
                switching(on_account.then_some((glyphs.arrow, account::MODELS_WORD))),
            ),
            Kind::Effort { endpoint, .. } => (
                copy::EFFORT_TITLE,
                if endpoint.is_some() {
                    ""
                } else {
                    copy::LEVEL_COLUMN
                },
                vec![
                    KeyHint::new(copy::ENTER_KEY, copy::USE_WORD),
                    copy::esc(copy::CLOSE_WORD),
                ],
            ),
        };
        Dock {
            name: name.to_owned(),
            column: column.to_owned(),
            keys,
            ..Dock::default()
        }
    }

    fn under(
        &self,
        row: Option<&Row>,
        facts: &Facts,
        tracker: &Tracker,
        at: Duration,
        set: GlyphSet,
    ) -> Option<Under> {
        let wanted = tracker.wanted();
        let glyphs = Glyphs::of(set);
        let target = match row? {
            Row::Add => {
                return Some(Under {
                    first: vec![account::ADDS_AN_ACCOUNT.to_owned()],
                    ..Under::default()
                });
            }
            Row::Listed { account, id } => (account.clone(), id.clone()),
            Row::Endpoint(name) => (name.clone(), shown_pair(facts, tracker, name).model),
            Row::Level(_) => match &self.kind {
                Kind::Effort {
                    endpoint: Some(endpoint),
                    model,
                    ..
                } => (endpoint.clone(), model.clone()),
                _ => return None,
            },
            Row::Free(id) if model_looks_like_key(id) => return None,
            Row::Free(id) => (wanted.endpoint.clone(), id.clone()),
            Row::Model(_) | Row::Header(_) => return None,
        };
        let (name, model) = target;
        let all = accounts(facts, tracker);
        let to = all.iter().find(|a| a.name == name)?;
        if to.state == AccountState::Expired {
            return Some(Under {
                mark: None,
                first: vec![account::sign_in_expired(&name)],
                parts: vec![(
                    account::SIGNS_IN_THEN_SWITCHES.to_owned(),
                    account::SIGNS_IN_THEN_SWITCHES.to_owned(),
                )],
                after: Vec::new(),
            });
        }
        if self.start.is_some() {
            let named = account::marked(&name, to.mark, &glyphs);
            return Some(Under {
                mark: to.mark,
                parts: account::starts_on(&named, to.state),
                ..Under::default()
            });
        }
        if name == wanted.endpoint && model == wanted.model {
            return Some(Under {
                first: vec![account::THIS_CHAT_NOW.to_owned()],
                ..Under::default()
            });
        }
        let from = all.iter().find(|a| a.name == wanted.endpoint);
        let named = name != wanted.endpoint
            && account::billing_move(from.map_or(to.state, |f| f.state), to.state).is_some();
        let effect = tracker.pair_effect(facts, &name, &model, at);
        let mut parts: Vec<(String, String)> = Vec::new();
        match &effect {
            Effect::Rebuild(rebuild) => parts.push(rebuild.parts(set, named)),
            Effect::Switch(rebuild, work) => {
                parts.push(rebuild.parts(set, named));
                let text = work.text(set);
                parts.push((text.clone(), text));
            }
            Effect::Current => {}
            other => {
                let text = other.text(set);
                parts.push((text.clone(), text));
            }
        }
        if let Some(note) = tracker.search_note(facts, &name, &model, set) {
            parts.push((note.clone(), note));
        }
        if let Some(checking) = self
            .checking
            .map(|_| copy::ongoing(copy::CHECKING_COMPACTION, &glyphs))
        {
            parts.push((checking.clone(), checking));
        }
        let first = if name == wanted.endpoint {
            account::ONLY_THE_MODEL.to_owned()
        } else {
            let named = account::marked(&name, to.mark, &glyphs);
            account::moves_to(&named, from.map_or(to.state, |f| f.state), to.state)
        };
        let mut after: Vec<String> = to.no_carry(from).into_iter().collect();
        if tracker.drops_reasoning(facts, &name, &model) {
            after.push(copy::SEARCH_CHANGE_DROPS_REASONING.to_owned());
        }
        let no_caps = |m: &ModelFacts| {
            !m.shipped
                && m.image_tier.is_none()
                && m.caps
                    .as_ref()
                    .is_none_or(|c| c.efforts.is_empty() && !c.adaptive_thinking)
        };
        let no_data = |m: &ModelFacts| !m.shipped && !m.per_message();
        if matches!(row, Some(Row::Listed { .. })) {
            let gemini = to
                .provider
                .eq_ignore_ascii_case(wisp_protocol::settings::PROVIDER_GEMINI);
            if gemini && facts.model_facts(&model).is_none_or(no_caps) {
                after.push(copy::NO_CAPABILITY_DATA_FOR_MODEL.to_owned());
            } else if facts.model_facts(&model).is_none_or(no_data) {
                after.push(copy::NO_DATA_FOR_MODEL.to_owned());
            }
        }
        Some(Under {
            mark: to.mark,
            first: vec![first],
            parts,
            after,
        })
    }

    fn menu_with(
        &self,
        facts: &Facts,
        tracker: &Tracker,
        at: Duration,
        set: GlyphSet,
        filter_note: bool,
    ) -> Menu {
        if !self.saves && !matches!(self.kind, Kind::Effort { endpoint: None, .. }) {
            return self.switch_menu(facts, tracker, at, set);
        }
        let glyphs = Glyphs::of(set);
        let sep = format!(" {} ", glyphs.separator);
        let rows = self.rows(facts, tracker);
        let selected = self.selection(&rows, facts, tracker);
        let wanted = tracker.wanted();
        let (searches, shared) = self.search_notes(&rows, facts, tracker, set);
        let with_search = |effect: String, i: usize| match searches.get(i).and_then(Option::as_ref)
        {
            Some(note) => format!("{effect}{sep}{note}"),
            None => effect,
        };
        let choices = rows
            .iter()
            .enumerate()
            .map(|(i, (row, enabled))| match row {
                Row::Model(id) | Row::Listed { id, .. } => {
                    let account = match row {
                        Row::Listed { account, .. } => account,
                        _ => &wanted.endpoint,
                    };
                    Choice {
                        mark: None,
                        current: *id == wanted.model,
                        label: short_model(id),
                        detail: price(facts.rates_on(account, id), &sep),
                        effect: with_search(
                            self.quoted(tracker.model_effect(facts, id, at).text(set), &glyphs),
                            i,
                        ),
                        enabled: *enabled,
                        line: None,
                    }
                }
                Row::Free(id) if model_looks_like_key(id) => Choice {
                    mark: None,
                    current: false,
                    label: NOT_SHOWN.to_owned(),
                    detail: String::new(),
                    effect: copy::KEY_SHAPED_MODEL.to_owned(),
                    enabled: false,
                    line: None,
                },
                Row::Free(id) => Choice {
                    mark: None,
                    current: false,
                    label: typed_label(id),
                    detail: copy::FREE_TEXT.to_owned(),
                    effect: with_search(
                        self.quoted(tracker.model_effect(facts, id, at).text(set), &glyphs),
                        i,
                    ),
                    enabled: *enabled,
                    line: None,
                },
                Row::Endpoint(name) => {
                    let endpoint = facts.endpoint_facts(name);
                    let effect = match endpoint.and_then(|e| e.unavailable.as_deref()) {
                        Some(reason) => copy::unavailable_because(reason),
                        None => {
                            self.quoted(tracker.endpoint_effect(facts, name, at).text(set), &glyphs)
                        }
                    };
                    Choice {
                        mark: facts.mark_of(name),
                        current: *name == wanted.endpoint,
                        label: account::marked(name, facts.mark_of(name), &glyphs),
                        detail: endpoint.map_or_else(String::new, |e| endpoint_detail(e, &sep)),
                        effect: with_search(effect, i),
                        enabled: *enabled,
                        line: None,
                    }
                }
                Row::Add => Choice {
                    mark: None,
                    current: false,
                    label: endpoint_card::add_row(set),
                    detail: String::new(),
                    effect: String::new(),
                    enabled: *enabled,
                    line: None,
                },
                Row::Header(name) => Choice {
                    label: name.clone(),
                    ..Choice::default()
                },
                Row::Level(level) => self.level_choice(*level, facts, tracker, at, set),
            })
            .collect();
        let mut notes: Vec<(String, Tone)> = shared.into_iter().map(|n| (n, Tone::Faint)).collect();
        let titles = match &self.kind {
            Kind::Model { query, .. } => {
                if query.is_empty() {
                    notes.push((
                        format!("claude-{} {}", glyphs.more, copy::ANY_MODEL_ID),
                        Tone::Faint,
                    ));
                } else {
                    let priced = facts.rates_on(&wanted.endpoint, query).is_some();
                    notes.extend(query_notes(&rows, query, priced));
                }
                [
                    copy::MODEL_TITLE.to_owned(),
                    format!("{}{sep}out", copy::PRICE_TITLE),
                    copy::NEXT_TURN_TITLE.to_owned(),
                ]
            }
            Kind::Endpoint { query } => {
                if query.is_empty() {
                    let chosen =
                        selected
                            .and_then(|i| rows.get(i))
                            .and_then(|(row, _)| match row {
                                Row::Endpoint(name) => facts.endpoint_facts(name),
                                _ => None,
                            });
                    let mut parts: Vec<String> = chosen
                        .map(|e| copy::endpoint_prices(e.priced, &e.name))
                        .into_iter()
                        .collect();
                    if filter_note {
                        parts.push(copy::type_to_filter());
                    }
                    parts.push(copy::EDIT_WITH_CLI.to_owned());
                    notes.push((parts.join(&sep), Tone::Faint));
                } else {
                    notes.push((copy::matching(query), Tone::Faint));
                }
                [
                    account::ACCOUNT_TITLE.to_owned(),
                    copy::CREDENTIAL_TITLE.to_owned(),
                    copy::NEXT_TURN_TITLE.to_owned(),
                ]
            }
            Kind::Effort {
                model, endpoint, ..
            } => {
                if facts.model_facts(model).is_none() {
                    let account = endpoint.as_ref().unwrap_or(&wanted.endpoint);
                    let priced = facts.rates_on(account, model).is_some();
                    notes.push((copy::no_capability_data(priced), Tone::Note));
                }
                [
                    format!("{} {}", copy::EFFORT_TITLE, short_model(model)),
                    String::new(),
                    copy::NEXT_TURN_TITLE.to_owned(),
                ]
            }
        };
        Menu {
            titles,
            choices,
            selected,
            notes,
            shape: Shape::default(),
        }
    }

    fn level_choice(
        &self,
        level: Effort,
        facts: &Facts,
        tracker: &Tracker,
        at: Duration,
        set: GlyphSet,
    ) -> Choice {
        let Kind::Effort {
            model,
            chained,
            endpoint,
            ..
        } = &self.kind
        else {
            return Choice::default();
        };
        let wanted = tracker.wanted();
        let default = facts
            .model_facts(model)
            .and_then(|m| m.caps.as_ref())
            .and_then(|c| c.default_effort)
            == Some(level);
        let current = !chained && Some(level) == wanted.effort;
        let effect = if endpoint.is_some() {
            String::new()
        } else if current {
            Effect::Current.text(set)
        } else {
            tracker.effort_effect(facts, model, level, at).text(set)
        };
        Choice {
            mark: None,
            current,
            label: level.as_str().to_owned(),
            detail: if default {
                copy::DEFAULT.to_owned()
            } else {
                String::new()
            },
            effect,
            enabled: true,
            line: None,
        }
    }

    fn switch_menu(&self, facts: &Facts, tracker: &Tracker, at: Duration, set: GlyphSet) -> Menu {
        let glyphs = Glyphs::of(set);
        let rows = self.rows(facts, tracker);
        let selected = self.selection(&rows, facts, tracker);
        let wanted = tracker.wanted();
        let all = accounts(facts, tracker);
        let mut heads: Vec<Head> = Vec::new();
        let choices: Vec<Choice> = rows
            .iter()
            .enumerate()
            .map(|(i, (row, enabled))| match row {
                Row::Endpoint(name) => {
                    let Some(a) = all.iter().find(|a| a.name == *name) else {
                        return Choice::default();
                    };
                    let pair = shown_pair(facts, tracker, &a.name);
                    let (mut choice, mut head, _) =
                        a.folded(facts, tracker, i, &pair.model, pair.effort, &glyphs);
                    if matches!(self.kind, Kind::Model { .. }) {
                        head.after = std::mem::take(&mut choice.effect);
                    }
                    heads.push(head);
                    Choice {
                        enabled: *enabled,
                        ..choice
                    }
                }
                Row::Header(name) => {
                    let Some(a) = all.iter().find(|a| a.name == *name) else {
                        return Choice::default();
                    };
                    let (detail, head) = a.head(i, &glyphs);
                    heads.push(head);
                    Choice {
                        label: account::marked(name, a.mark, &glyphs),
                        mark: a.mark,
                        detail,
                        enabled: *enabled,
                        ..Choice::default()
                    }
                }
                Row::Listed { account, id } => {
                    let current = *account == wanted.endpoint && *id == wanted.model;
                    let price = if self.start.is_some() {
                        String::new()
                    } else if current {
                        account::NOW.to_owned()
                    } else {
                        match tracker.pair_effect(facts, account, id, at) {
                            Effect::Rebuild(r) | Effect::Switch(r, _) => r.cell(),
                            _ => String::new(),
                        }
                    };
                    Choice {
                        current,
                        label: format!(
                            "  {}",
                            family_on(facts, account, id)
                                .map_or_else(|| short_model(id), str::to_owned)
                        ),
                        detail: if current {
                            display_effort(facts, id, wanted.effort)
                                .map_or_else(String::new, |e| e.as_str().to_owned())
                        } else if facts.rates_on(account, id).is_none() {
                            copy::COST_UNKNOWN.to_owned()
                        } else {
                            String::new()
                        },
                        effect: price,
                        enabled: *enabled,
                        ..Choice::default()
                    }
                }
                Row::Free(id) if model_looks_like_key(id) => Choice {
                    label: format!("  {NOT_SHOWN}"),
                    effect: copy::KEY_SHAPED_MODEL.to_owned(),
                    ..Choice::default()
                },
                Row::Free(id) => Choice {
                    label: format!("  {}", typed_label(id)),
                    detail: copy::FREE_TEXT.to_owned(),
                    enabled: *enabled,
                    ..Choice::default()
                },
                Row::Add => Choice {
                    label: account::sign_in_row(&glyphs),
                    enabled: *enabled,
                    ..Choice::default()
                },
                Row::Level(level) => self.level_choice(*level, facts, tracker, at, set),
                Row::Model(id) => Choice {
                    label: short_model(id),
                    enabled: *enabled,
                    ..Choice::default()
                },
            })
            .collect();
        let row = selected.and_then(|i| rows.get(i)).map(|(row, _)| row);
        let reserve = rows
            .iter()
            .filter(|(_, enabled)| *enabled)
            .filter_map(|(r, _)| self.under(Some(r), facts, tracker, at, set))
            .collect();
        let mut notes: Vec<(String, Tone)> = Vec::new();
        if let Kind::Effort { model, .. } = &self.kind
            && let Some(effort) = wanted.effort
            && !rows.iter().any(|(r, _)| *r == Row::Level(effort))
        {
            notes.push((
                copy::no_level_offered(&short_model(model), effort.as_str()),
                Tone::Faint,
            ));
        }
        match self.query().filter(|q| !q.is_empty()) {
            Some(query) if self.start.is_some() => {
                notes.push((copy::typed_filter(&typed_label(query)), Tone::Faint));
            }
            Some(query) => {
                let priced = facts.rates_on(&wanted.endpoint, query).is_some();
                notes.extend(query_notes(&rows, query, priced));
            }
            None => {}
        }
        let effect_from = match &self.kind {
            Kind::Model { .. } => GROUPED_PRICES_FROM,
            Kind::Endpoint { .. } => FOLDED_MODELS_FROM,
            Kind::Effort { .. } => usize::MAX,
        };
        let titles = match &self.kind {
            Kind::Model { .. } if self.start.is_none() => [
                String::new(),
                String::new(),
                account::REBUILD_TITLE.to_owned(),
            ],
            Kind::Model { .. } | Kind::Effort { .. } | Kind::Endpoint { .. } => Default::default(),
        };
        Menu {
            titles,
            choices,
            selected,
            notes,
            shape: Shape {
                heads,
                under: self.under(row, facts, tracker, at, set),
                reserve,
                effect_from,
            },
        }
    }

    fn step_selection(
        &mut self,
        rows: &[(Row, bool)],
        facts: &Facts,
        tracker: &Tracker,
        down: bool,
    ) {
        let enabled: Vec<bool> = rows.iter().map(|row| row.1).collect();
        self.selected = next_enabled(self.selection(rows, facts, tracker), &enabled, down);
    }

    fn edit(&mut self, f: impl FnOnce(&mut String)) {
        if let Kind::Model { query, .. } | Kind::Endpoint { query } = &mut self.kind {
            f(query);
            self.selected = None;
        }
    }

    pub fn paste(&mut self, text: &str) {
        let line = match self.kind {
            Kind::Endpoint { .. } => LineEdit::ENDPOINT_NAME,
            _ => MODEL_ID,
        };
        self.edit(|q| {
            line.paste(q, text);
        });
    }

    pub fn key(&mut self, key: Key, facts: &Facts, tracker: &Tracker) -> Step {
        let rows = self.rows(facts, tracker);
        match key.code {
            KeyCode::Esc => Step::Close,
            KeyCode::Char('c' | 'C') if key.ctrl => Step::Close,
            KeyCode::Up | KeyCode::BackTab => {
                self.step_selection(&rows, facts, tracker, false);
                Step::Stay
            }
            KeyCode::Down | KeyCode::Tab => {
                self.step_selection(&rows, facts, tracker, true);
                Step::Stay
            }
            KeyCode::Left if self.grouped() && self.start.is_none() => {
                let to = Kind::Endpoint {
                    query: String::new(),
                };
                self.refold(&rows, facts, tracker, to);
                Step::Stay
            }
            KeyCode::Right if !self.saves && self.start.is_none() => {
                let folded = self
                    .selection(&rows, facts, tracker)
                    .and_then(|i| rows.get(i))
                    .and_then(|(row, _)| match row {
                        Row::Endpoint(name) => Some(name.clone()),
                        _ => None,
                    })
                    .filter(|name| Picker::offers_models(facts, tracker, name));
                if let Some(name) = folded {
                    let to = Kind::Model {
                        query: String::new(),
                        open: Some(name),
                    };
                    self.refold(&rows, facts, tracker, to);
                }
                Step::Stay
            }
            KeyCode::Backspace => {
                self.edit(|q| {
                    q.pop();
                });
                Step::Stay
            }
            KeyCode::Char(c) if !key.ctrl && !key.alt => {
                let mut buf = [0u8; 4];
                self.paste(c.encode_utf8(&mut buf));
                Step::Stay
            }
            KeyCode::Enter => match self
                .selection(&rows, facts, tracker)
                .and_then(|i| rows.get(i))
            {
                Some((row, true)) => self.choose(row, facts, tracker),
                _ => Step::Stay,
            },
            _ => Step::Stay,
        }
    }

    fn refold(&mut self, rows: &[(Row, bool)], facts: &Facts, tracker: &Tracker, to: Kind) {
        let account_of = |row: &Row| match row {
            Row::Listed { account, .. } | Row::Endpoint(account) => Some(account.clone()),
            _ => None,
        };
        let account = self
            .selection(rows, facts, tracker)
            .and_then(|i| rows.get(i))
            .and_then(|(row, _)| account_of(row));
        self.kind = to;
        let shown = self.rows(facts, tracker);
        self.selected = account.and_then(|name| {
            shown
                .iter()
                .position(|(row, enabled)| *enabled && account_of(row).as_ref() == Some(&name))
        });
    }

    fn offers_models(facts: &Facts, tracker: &Tracker, name: &str) -> bool {
        let wanted = tracker.wanted();
        accounts(facts, tracker)
            .iter()
            .find(|a| a.name == name)
            .is_some_and(|a| !models_on(facts, a, &wanted.model, offered).is_empty())
    }

    fn signing_in(facts: &Facts, account: &str, then: Picked) -> Step {
        if facts.account_state(account) == AccountState::Expired {
            Step::SignIn {
                account: account.to_owned(),
                then,
            }
        } else {
            Step::Done(then)
        }
    }

    pub fn account_step(name: &str, facts: &Facts, tracker: &Tracker) -> Step {
        let wanted = tracker.wanted();
        if name == wanted.endpoint {
            return Step::Close;
        }
        let pair = shown_pair(facts, tracker, name);
        let model = pair.model;
        let then = if pair.remembered || model == wanted.model {
            Picked::Settings {
                endpoint: Some(name.to_owned()),
                model: None,
                effort: None,
            }
        } else {
            switch_to(&model, facts, wanted.effort, Some(name))
        };
        Picker::signing_in(facts, name, then)
    }

    fn choose(&self, row: &Row, facts: &Facts, tracker: &Tracker) -> Step {
        let wanted = tracker.wanted();
        match (row, &self.kind) {
            (Row::Add, _) => Step::Add,
            (Row::Header(_), _) => Step::Stay,
            (Row::Endpoint(name), _) if self.saves => Step::Done(Picked::Settings {
                endpoint: Some(name.clone()),
                model: None,
                effort: None,
            }),
            (Row::Endpoint(name), _) => Picker::account_step(name, facts, tracker),
            (Row::Listed { account, id }, _) if *account != wanted.endpoint => {
                let then = switch_to(id, facts, wanted.effort, Some(account));
                Picker::signing_in(facts, account, then)
            }
            (Row::Model(id) | Row::Free(id) | Row::Listed { id, .. }, _) => {
                if *id == wanted.model && !self.saves {
                    return Step::Close;
                }
                Step::Done(switch_to(id, facts, wanted.effort, None))
            }
            (
                Row::Level(level),
                Kind::Effort {
                    model,
                    chained,
                    endpoint,
                    ..
                },
            ) => {
                if *chained {
                    Step::Done(Picked::Settings {
                        endpoint: endpoint.clone(),
                        model: Some(model.clone()),
                        effort: Some(*level),
                    })
                } else if Some(*level) == wanted.effort && !self.saves {
                    Step::Close
                } else {
                    Step::Done(Picked::Settings {
                        endpoint: None,
                        model: None,
                        effort: Some(*level),
                    })
                }
            }
            (Row::Level(_), Kind::Model { .. } | Kind::Endpoint { .. }) => Step::Close,
        }
    }
}

const MODEL_ID: LineEdit = LineEdit {
    max: MAX_MODEL_ID,
    filter: |c| model_id_char(c).then_some(c),
};
const GROUPED_PRICES_FROM: usize = 100;
pub(crate) const FOLDED_MODELS_FROM: usize = 60;

pub(crate) fn shown_pair(
    facts: &Facts,
    tracker: &Tracker,
    name: &str,
) -> wisp_protocol::settings::Landed {
    let wanted = tracker.wanted();
    wisp_protocol::settings::lands_on(
        &offers(&facts.models),
        facts.landing(name),
        name == wanted.endpoint,
        (chat_of(&facts.models, &wanted.model), wanted.effort),
        tracker.remembered(name),
    )
}

pub(crate) fn model_on(facts: &Facts, from: &str, name: &str, chat: &str) -> String {
    wisp_protocol::settings::lands_on(
        &offers(&facts.models),
        facts.landing(name),
        name == from,
        (chat_of(&facts.models, chat), None),
        None,
    )
    .model
}

pub(crate) fn effort_levels<'a>(facts: &'a Facts, model: &str) -> Option<&'a [Effort]> {
    facts
        .model_facts(model)
        .and_then(|m| m.caps.as_ref())
        .map(|c| c.efforts.as_slice())
}

pub(crate) fn display_effort(facts: &Facts, model: &str, effort: Option<Effort>) -> Option<Effort> {
    wisp_protocol::settings::display_effort(
        facts.model_facts(model).and_then(|m| m.variant.as_ref()),
        effort_levels(facts, model),
        effort,
    )
}

pub(crate) fn shown_effort(facts: &Facts, model: &str, effort: Option<Effort>) -> Option<Effort> {
    display_effort(
        facts,
        model,
        effort_levels(facts, model).map_or(effort, |levels| effort_on(levels, effort)),
    )
}

pub fn switch_to(
    id: &str,
    facts: &Facts,
    effort: Option<Effort>,
    endpoint: Option<&str>,
) -> Picked {
    let preselect = shown_effort(facts, id, effort);
    if facts.model_facts(id).is_some_and(|m| m.variant.is_some())
        || preselect.is_none()
        || preselect == effort
    {
        Picked::Settings {
            endpoint: endpoint.map(str::to_owned),
            model: Some(id.to_owned()),
            effort: None,
        }
    } else {
        Picked::Effort {
            endpoint: endpoint.map(str::to_owned),
            model: id.to_owned(),
            preselect,
        }
    }
}

pub fn offers_typed(facts: &Facts, tracker: &Tracker, id: &str) -> bool {
    let wanted = tracker.wanted();
    accounts(facts, tracker)
        .iter()
        .find(|a| a.name == wanted.endpoint)
        .is_some_and(|a| models_on(facts, a, id, offered).iter().any(|m| m == id))
}

#[cfg(test)]
mod tests;
