use std::time::Duration;

use wisp_protocol::agents::AgentEntry;
use wisp_protocol::subscription_auth::{PresetGone, Way, WayOrigin};
use wisp_protocol::{Billing, QuestionOption};
use wisp_render::chrome::account::{
    self, AccountState, Asked, Moving, Named, SIGN_OUT_OPTIONS, THIS_CHAT, WHERE_SHOULD_IT_GO,
    WHERE_TITLE, WHICH_TITLE, logout_question, mark_line, marked, preset_line, sign_out_title,
};
use wisp_render::chrome::{
    CardHit, CardRows, Effect, Facts, FormView, KeyHint, Rebuild, Scope, answer_options, form_rows,
};
use wisp_render::markdown::Layout;
use wisp_render::styled::{Look, StyledLine, StyledSpan};
use wisp_render::text::width;
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::copy;

use crate::app::list::Pick;
use crate::app::picker;
use crate::app::settings::{Chosen, Tracker};
use crate::app::subscription_login::{cancel, columns};
use crate::term::{Key, KeyCode};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    pub mark: Option<u8>,
    pub details: Vec<String>,
}

const KIND_FROM: usize = 60;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WayRow {
    pub chat: String,
    pub mark: Option<u8>,
    pub follow: Vec<String>,
    pub kind: String,
    pub billing: String,
    pub model: String,
    pub price: String,
    pub rebuild: Option<Rebuild>,
    pub movers: Vec<(String, Option<Rebuild>)>,
    pub consequence: Vec<String>,
    pub fixed_notes: Vec<String>,
    pub live: bool,
    pub chat_moves: bool,
    pub accept_api: bool,
    pub charges: bool,
    pub again: bool,
}

impl WayRow {
    pub(crate) fn moving(&self, state: AccountState, chat: bool) -> Moving {
        let chat = chat.then(|| (THIS_CHAT.to_owned(), self.rebuild.as_ref().and_then(amount)));
        let agents = self.movers.iter();
        let agents =
            agents.map(|(name, rebuild)| (name.clone(), rebuild.as_ref().and_then(amount)));
        Moving {
            to: (self.chat.clone(), self.mark),
            state,
            rows: chat.into_iter().chain(agents).collect(),
            notes: self.fixed_notes.clone(),
        }
    }
}

fn amount(rebuild: &Rebuild) -> Option<String> {
    matches!(
        rebuild,
        Rebuild::Usd(_) | Rebuild::PlanUsd(..) | Rebuild::UpToUsd(_)
    )
    .then(|| rebuild.cell())
}

fn way_price(
    rebuild: Option<Rebuild>,
    glyphs: GlyphSet,
    follow: &[String],
    movers: &[(String, Option<Rebuild>)],
) -> String {
    let price = rebuild.map_or_else(String::new, |r| r.short(glyphs));
    let chat = rebuild.as_ref().and_then(amount);
    let amounts: Vec<(&String, Option<String>)> = movers
        .iter()
        .map(|(name, r)| (name, r.as_ref().and_then(amount)))
        .collect();
    let known = !follow.is_empty() && movers.len() == follow.len();
    let any = chat.is_some() || amounts.iter().any(|(_, a)| a.is_some());
    if !known || !any {
        return if price.is_empty() || follow.is_empty() {
            price
        } else {
            format!("{price} for {THIS_CHAT}")
        };
    }
    if let Some(chat) = &chat
        && amounts.iter().all(|(_, a)| a.is_some())
    {
        let all = std::iter::once(chat.clone()).chain(amounts.into_iter().filter_map(|(_, a)| a));
        return copy::cache_rebuild(&copy::listed(&all.collect::<Vec<_>>()));
    }
    let named = amounts
        .iter()
        .filter_map(|(name, a)| Some(format!("{} for {name}", a.as_ref()?)));
    let chat_named = chat.as_ref().map(|a| format!("{a} for {THIS_CHAT}"));
    let named: Vec<String> = chat_named.into_iter().chain(named).collect();
    let chat_rest =
        (chat.is_none() && !price.is_empty()).then(|| format!("{price} for {THIS_CHAT}"));
    let unpriced: Vec<String> = amounts
        .iter()
        .filter(|(_, a)| a.is_none())
        .map(|(name, _)| (*name).clone())
        .collect();
    let rest = (!unpriced.is_empty()).then(|| account::nothing_cached(&unpriced));
    let rest: Vec<String> = chat_rest.into_iter().chain(rest).collect();
    let rebuilds = copy::cache_rebuild(&copy::listed(&named));
    if rest.is_empty() {
        rebuilds
    } else {
        format!("{rebuilds}; {}.", rest.join("; "))
    }
}

fn named_rebuild(name: &str, rebuild: Rebuild, glyphs: GlyphSet) -> String {
    match rebuild {
        Rebuild::Unknown => format!("no estimate for {name}"),
        Rebuild::Empty => account::nothing_cached(&[name.to_owned()]),
        Rebuild::Unpriced(Scope::Exact) => format!("no prices for {name}"),
        other => format!(
            "{} for {name}",
            amount(&other).unwrap_or_else(|| other.amount(glyphs))
        ),
    }
}

fn live_price(movers: &[(&str, Rebuild)], glyphs: GlyphSet) -> String {
    if movers.is_empty() {
        return String::new();
    }
    if movers
        .iter()
        .all(|(_, r)| *r == Rebuild::Unpriced(Scope::Exact))
    {
        let names: Vec<_> = movers.iter().map(|(name, _)| (*name).to_owned()).collect();
        return format!("cache rebuild, no prices for {}.", copy::listed(&names));
    }
    let mut dollars = Vec::new();
    let mut other = Vec::new();
    for &(name, rebuild) in movers {
        if let Some(value) = amount(&rebuild) {
            dollars.push((name, value));
        } else {
            other.push(named_rebuild(name, rebuild, glyphs));
        }
    }
    if other.is_empty() {
        let amounts: Vec<_> = dollars.into_iter().map(|(_, value)| value).collect();
        return copy::cache_rebuild(&copy::listed(&amounts));
    }
    let reasons = other.join("; ");
    if dollars.is_empty() {
        format!("cache rebuild, {reasons}.")
    } else {
        let named: Vec<_> = dollars
            .into_iter()
            .map(|(name, value)| format!("{value} for {name}"))
            .collect();
        let amounts = copy::cache_rebuild(&copy::listed(&named));
        format!("{amounts}; {reasons}.")
    }
}

fn landed(
    facts: &Facts,
    tracker: &Tracker,
    dest: &str,
    origin: &WayOrigin,
) -> wisp_protocol::settings::Landed {
    picker::shown_pair_from(
        facts,
        tracker,
        dest,
        &Chosen {
            endpoint: origin.account.clone(),
            model: origin.model.clone(),
            effort: Some(origin.effort),
        },
    )
}

fn live_quote(
    facts: &Facts,
    tracker: &Tracker,
    to: &str,
    model: &str,
    known: bool,
    at: Duration,
) -> Rebuild {
    if known {
        tracker.rebuild_on(facts, to, model, Scope::Exact, at)
    } else {
        Rebuild::Unknown
    }
}

fn agent_rebuild(agent: &Mover, to: &str, at: Duration) -> Option<Rebuild> {
    agent.entry.context.as_ref()?;
    let model = picker::shown_pair(&agent.facts, agent.tracker, to).model;
    rebuild_to(&agent.facts, agent.tracker, to, &model, at)
}

fn rebuild_to(
    facts: &Facts,
    tracker: &Tracker,
    to: &str,
    model: &str,
    at: Duration,
) -> Option<Rebuild> {
    match tracker.pair_effect(facts, to, model, at) {
        Effect::Rebuild(r) | Effect::Switch(r, _) => Some(r),
        _ => None,
    }
}

pub(crate) struct Mover<'a> {
    pub(crate) entry: &'a AgentEntry,
    pub(crate) facts: Facts,
    pub(crate) tracker: &'a Tracker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Leaving {
    Chat(Billing),
    Start(Billing),
}

pub(crate) fn way_rows(
    facts: &Facts,
    tracker: &Tracker,
    set: GlyphSet,
    ways: &[Way],
    leaving: Leaving,
    agents: Option<&[Mover]>,
    at: Duration,
) -> Vec<WayRow> {
    let glyphs = Glyphs::of(set);
    let wanted = tracker.wanted();
    let rows = picker::accounts(facts, tracker);
    let row = |name: &str| rows.iter().find(|a| a.name == name);
    let chat = picker::chat_account(facts, tracker);
    let (ran, billing) = match leaving {
        Leaving::Chat(billing) => (Some(row(&wanted.endpoint).unwrap_or(&chat)), billing),
        Leaving::Start(billing) => (None, billing),
    };
    ways.iter()
        .filter_map(|way| {
            let name = way.chat.clone()?;
            let to = row(&name)?;
            let live = !way.origins.is_empty();
            let root = way.origins.get("/");
            let pair = root.map_or_else(
                || picker::shown_pair(facts, tracker, &name),
                |origin| landed(facts, tracker, &name, origin),
            );
            let (choice, head, model) =
                to.folded(facts, tracker, 0, &pair.model, pair.effort, &glyphs);
            let rebuild = if live {
                root.map(|_| {
                    live_quote(
                        facts,
                        tracker,
                        &name,
                        &model,
                        tracker.has_cache_measurement(),
                        at,
                    )
                })
            } else {
                rebuild_to(facts, tracker, &name, &model, at)
            };
            let movers: Vec<_> = way
                .follow
                .iter()
                .filter_map(|path| {
                    let entry = agents.and_then(|agents| {
                        agents
                            .iter()
                            .find(|e| e.entry.path.as_str().trim_start_matches('/') == path)
                    });
                    let rebuild = if live {
                        Some(entry.zip(way.origins.get(path)).map_or(
                            Rebuild::Unknown,
                            |(e, origin)| {
                                let pair = landed(&e.facts, e.tracker, &name, origin);
                                live_quote(
                                    &e.facts,
                                    e.tracker,
                                    &name,
                                    &pair.model,
                                    e.entry.context.is_some(),
                                    at,
                                )
                            },
                        ))
                    } else {
                        agents?;
                        entry.and_then(|e| agent_rebuild(e, &name, at))
                    };
                    Some((path.clone(), rebuild))
                })
                .collect();
            let state = facts.account_state(&name);
            let origins = root
                .map(|origin| ("/", origin))
                .into_iter()
                .chain(
                    way.follow
                        .iter()
                        .filter_map(|path| Some((path.as_str(), way.origins.get(path)?))),
                )
                .collect::<Vec<_>>();
            let quotes: Vec<_> = rebuild
                .map(|r| (THIS_CHAT, r))
                .into_iter()
                .chain(
                    movers
                        .iter()
                        .map(|(name, r)| (name.as_str(), r.unwrap_or(Rebuild::Unknown))),
                )
                .collect();
            let provider = facts
                .endpoint_facts(&name)
                .map_or("", |facts| facts.provider.as_str());
            let mut fixed_notes: Vec<_> = if live {
                quotes
                    .iter()
                    .filter(|(_, r)| amount(r).is_none())
                    .map(|(name, r)| {
                        let note = named_rebuild(name, *r, set);
                        let mut chars = note.chars();
                        chars.next().map_or_else(String::new, |c| {
                            format!("{}{}.", c.to_uppercase(), chars.as_str())
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if live {
                fixed_notes.extend(account::live_consequences(&origins, provider, state, true));
            }
            Some(WayRow {
                mark: facts.mark_of(&name),
                kind: choice.detail,
                billing: head.short,
                model: choice.effect,
                price: if live {
                    live_price(&quotes, set)
                } else {
                    way_price(rebuild, set, &way.follow, &movers)
                },
                rebuild,
                movers,
                consequence: if live {
                    account::live_consequences(&origins, provider, state, false)
                } else {
                    account::consequence(
                        &way.follow,
                        &way.stay,
                        to.no_carry(ran).is_none(),
                        AccountState::of(billing),
                        state,
                    )
                },
                follow: way.follow.clone(),
                charges: way.charges || live && state == AccountState::Api,
                fixed_notes,
                live,
                chat_moves: root.is_some(),
                accept_api: state == AccountState::Api,
                again: false,
                chat: name,
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
enum Stage {
    Which(Vec<Choice>),
    Yes,
    Where,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    Close,
    Send {
        names: Vec<String>,
        confirmed: bool,
        chat: Option<String>,
        follow: Vec<String>,
        gone: bool,
        accept_api: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignOut {
    stage: Stage,
    asked: Asked,
    ways: Vec<WayRow>,
    way: Option<usize>,
    typed: bool,
    pick: Pick,
    heading: Option<(String, String, Option<u8>)>,
    action: KeyHint,
    esc: KeyHint,
    pub drawn: bool,
}

type Drawn<'a> = (
    Vec<StyledSpan>,
    Vec<StyledLine>,
    Vec<QuestionOption>,
    Vec<Option<u8>>,
);

impl SignOut {
    pub fn which(choices: Vec<Choice>) -> Self {
        Self {
            pick: Pick {
                rows: choices.len(),
                held: Vec::new(),
                focus: None,
            },
            ..Self::yes(Asked::default(), Vec::new(), None)
        }
        .at(Stage::Which(choices))
    }

    pub fn yes(asked: Asked, ways: Vec<WayRow>, way: Option<usize>) -> Self {
        Self {
            stage: Stage::Yes,
            asked,
            ways,
            way,
            typed: false,
            pick: Pick {
                rows: SIGN_OUT_OPTIONS.len(),
                held: vec![true, false],
                focus: Some(1),
            },
            heading: None,
            action: KeyHint::new(copy::ENTER_KEY, SIGN_OUT_OPTIONS[0].to_lowercase()),
            esc: cancel(),
            drawn: false,
        }
    }

    pub fn place(names: Vec<Named>, ways: Vec<WayRow>, typed: bool) -> Self {
        let asked = Asked {
            names,
            ..Asked::default()
        };
        let mut card = Self::yes(asked, ways, None).at(Stage::Where);
        card.typed = typed;
        card
    }

    pub fn gone(
        names: Vec<String>,
        heading: (String, String, Option<u8>),
        ways: Vec<WayRow>,
        esc: KeyHint,
    ) -> Self {
        let names = names.into_iter().map(|name| (name, None)).collect();
        let mut card = Self::place(names, ways, false);
        card.pick.held = card.ways.iter().map(|w| w.charges).collect();
        card.pick.focus = card.ways.iter().position(|w| !w.charges);
        card.heading = Some(heading);
        card.action = copy::use_the_api();
        card.esc = esc;
        card
    }

    pub fn presets(mut self, presets: Vec<PresetGone>) -> Self {
        self.asked.presets = presets;
        self
    }

    fn at(mut self, stage: Stage) -> Self {
        if stage == Stage::Where {
            self.pick = Pick {
                rows: self.ways.len(),
                held: vec![true; self.ways.len()],
                focus: None,
            };
        }
        self.stage = stage;
        self.drawn = false;
        self
    }

    pub fn rebuild_of(&self, chat: &str) -> Option<Rebuild> {
        self.ways.iter().find(|w| w.chat == chat)?.rebuild
    }

    pub fn movers_of(&self, chat: Option<&str>) -> Vec<(String, Rebuild)> {
        let way = self.ways.iter().find(|w| Some(w.chat.as_str()) == chat);
        let movers = way.into_iter().flat_map(|w| &w.movers);
        movers
            .filter_map(|(agent, rebuild)| Some((agent.clone(), (*rebuild)?)))
            .collect()
    }

    pub fn nothing_left(&self) -> bool {
        self.asked.nothing_left
    }

    fn send(&self, way: Option<usize>) -> Intent {
        let way = way.and_then(|at| self.ways.get(at));
        Intent::Send {
            names: self
                .asked
                .names
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
            confirmed: true,
            chat: way.map(|w| w.chat.clone()),
            follow: way.map(|w| w.follow.clone()).unwrap_or_default(),
            gone: self.heading.is_some(),
            accept_api: way.is_some_and(|w| w.accept_api),
        }
    }

    fn chose(&self, row: usize) -> Option<Intent> {
        let Stage::Which(choices) = &self.stage else {
            return None;
        };
        Some(Intent::Send {
            names: vec![choices.get(row)?.name.clone()],
            confirmed: false,
            chat: None,
            follow: Vec::new(),
            gone: false,
            accept_api: false,
        })
    }

    fn answered(&mut self, row: usize) -> Option<Intent> {
        match self.stage {
            Stage::Which(_) => self.chose(row),
            Stage::Yes if row == 0 => self.said_yes(),
            Stage::Yes => Some(Intent::Close),
            Stage::Where => Some(self.send(Some(row))),
        }
    }

    fn said_yes(&mut self) -> Option<Intent> {
        if self.way.is_none() && !self.ways.is_empty() {
            *self = self.clone().at(Stage::Where);
            return None;
        }
        Some(self.send(self.way))
    }

    pub fn key(&mut self, key: Key) -> Option<Intent> {
        if key.code == KeyCode::Esc || (key.ctrl && matches!(key.code, KeyCode::Char('c' | 'C'))) {
            return Some(Intent::Close);
        }
        if !self.drawn {
            return None;
        }
        let row = self.pick.key(key).took()?;
        self.answered(row)
    }

    pub fn click(&mut self, hit: CardHit) -> Option<Intent> {
        let CardHit::Option(row) = hit else {
            return None;
        };
        if !self.drawn {
            return None;
        }
        let row = self.pick.click(row).took()?;
        self.answered(row)
    }

    pub fn render(&self, room: usize, layout: Layout) -> CardRows {
        let glyphs = Glyphs::of(layout.glyphs);
        let bold = Look::fg(Token::Text).bold();
        let lines = |text: &str, account: Option<u8>| -> Vec<StyledLine> {
            text.lines()
                .map(|line| mark_line(line, Token::Text, account, &glyphs))
                .collect()
        };
        let (title, prompt, options, marks): Drawn<'_> = match &self.stage {
            Stage::Which(choices) => {
                let rows = choices
                    .iter()
                    .map(|c| (marked(&c.name, c.mark, &glyphs), c.details.clone()));
                let options = columns(rows.collect(), layout);
                (
                    vec![StyledSpan::new(WHICH_TITLE, bold)],
                    Vec::new(),
                    options,
                    choices.iter().map(|c| c.mark).collect(),
                )
            }
            Stage::Yes => {
                let room = layout.width.saturating_sub(4);
                let (title, asked) = self.asked.titled(room, &glyphs);
                let lines = logout_question(&asked, room, &glyphs);
                let options = answer_options(&SIGN_OUT_OPTIONS);
                (title, lines, options, Vec::new())
            }
            Stage::Where => {
                let (title, mut prompt) = match &self.heading {
                    Some((title, prompt, mark)) => (
                        mark_line(title, bold, *mark, &glyphs).spans,
                        lines(prompt, *mark),
                    ),
                    None if self.typed => (
                        sign_out_title(&self.asked.names, usize::MAX, &glyphs).0,
                        lines(WHERE_SHOULD_IT_GO, None),
                    ),
                    None => (vec![StyledSpan::new(WHERE_TITLE, bold)], Vec::new()),
                };
                let shared = self.shared();
                prompt.extend(shared.iter().flat_map(|line| lines(line, None)));
                let presets = self.asked.presets.iter();
                prompt.extend(presets.flat_map(|gone| lines(&preset_line(gone, None), None)));
                let marks = self.ways.iter().map(|w| w.mark).collect();
                (
                    title,
                    prompt,
                    self.way_options(&shared, layout, &glyphs),
                    marks,
                )
            }
        };
        let footer = copy::pick_keys(
            self.pick.focus.map(|row| self.pick.holds(row)),
            self.action.clone(),
            copy::enter_choose(),
            Some(self.esc.clone()),
            layout.glyphs,
        );
        let form = FormView {
            title: "",
            title_line: Some(&title),
            note: "",
            prompt: "",
            prompt_lines: &prompt,
            error: "",
            options: &options,
            option_marks: &marks,
            selected: self.pick.focus,
            field: None,
            footer: &footer,
            account: None,
            done: false,
            link: None,
            refused: None,
        };
        form_rows(&form, room, layout)
    }

    fn shared(&self) -> Vec<String> {
        match self.ways.as_slice() {
            [first, rest @ ..] if !rest.is_empty() => first
                .consequence
                .iter()
                .filter(|line| rest.iter().all(|w| w.consequence.contains(line)))
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn way_options(
        &self,
        shared: &[String],
        layout: Layout,
        glyphs: &Glyphs,
    ) -> Vec<QuestionOption> {
        let sep = format!(" {} ", glyphs.separator);
        let room = layout.width.saturating_sub(6);
        let kind = layout.width >= KIND_FROM
            && self
                .ways
                .iter()
                .all(|w| width(&format!("{}{sep}{}", w.kind, w.model), layout.width_mode) <= room);
        self.ways
            .iter()
            .map(|w| {
                if w.again {
                    let long = account::sign_in_again_to(&marked(&w.chat, w.mark, glyphs));
                    let fits = width(&long, layout.width_mode) <= room;
                    return QuestionOption {
                        label: if fits {
                            long
                        } else {
                            account::SIGN_IN_AGAIN.to_owned()
                        },
                        description: String::new(),
                    };
                }
                let word = if kind { &w.kind } else { &w.billing };
                let mut lines = vec![format!("{word}{sep}{}", w.model)];
                lines.extend(Some(&w.price).filter(|p| !p.is_empty()).cloned());
                let own = w.consequence.iter().filter(|line| !shared.contains(line));
                lines.extend(own.cloned());
                QuestionOption {
                    label: marked(&w.chat, w.mark, glyphs),
                    description: lines.join("\n"),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
