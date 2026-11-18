use wisp_protocol::settings::{
    AppliedChange, CACHE_KEPT, CACHE_KEPT_EFFORT, Layer, Overridden, OverrideScope, Place,
    SettingTarget, When,
};
use wisp_protocol::{CacheEffect, Effort, RebuildReason, SwitchCompactionCause, marked_amount};

use super::account::{AccountState, NONE, NOW, billing_word};
use super::facts::{ModelRates, WriteMultipliers, short_model};
use super::{model_line, one_line};
use crate::num;
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::transcript::copy;

pub fn approx(set: GlyphSet) -> &'static str {
    match set {
        GlyphSet::Unicode => "\u{2248}",
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => "~",
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheSize {
    pub prefix: u64,
    pub total: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Exact,
    UpTo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alive {
    pub prefix: bool,
    pub tail: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rebuild {
    Usd(f64),
    PlanUsd(f64, Scope),
    Tokens(u64),
    UpToUsd(f64),
    UpToTokens(u64),
    Expired,
    Empty,
    Unpriced(Scope),
    Unknown,
}

impl Rebuild {
    pub fn estimate(
        size: CacheSize,
        scope: Scope,
        alive: Alive,
        rates: Option<&ModelRates>,
        multipliers: WriteMultipliers,
    ) -> Rebuild {
        let prefix = size.prefix.min(size.total);
        let tail = size.total - size.prefix.min(size.total);
        if prefix + tail == 0 {
            return Rebuild::Empty;
        }
        let prefix = if alive.prefix { prefix } else { 0 };
        let tail = if alive.tail { tail } else { 0 };
        if prefix + tail == 0 {
            return Rebuild::Expired;
        }
        let Some(rates) = rates else {
            return match scope {
                Scope::Exact => Rebuild::Tokens(prefix + tail),
                Scope::UpTo => Rebuild::UpToTokens(prefix + tail),
            };
        };
        let rate = rates.input_for(size.total);
        let weighted = prefix as f64 * multipliers.prefix + tail as f64 * multipliers.tail;
        let usd = weighted * rate / 1_000_000.0;
        match scope {
            Scope::Exact => Rebuild::Usd(usd),
            Scope::UpTo => Rebuild::UpToUsd(usd),
        }
    }

    pub fn billed(self, billing: wisp_protocol::Billing) -> Self {
        if billing.is_metered() {
            return self;
        }
        match self {
            Self::Usd(usd) => Self::PlanUsd(usd, Scope::Exact),
            Self::UpToUsd(usd) => Self::PlanUsd(usd, Scope::UpTo),
            other => other,
        }
    }

    pub fn amount(&self, set: GlyphSet) -> String {
        self.amount_in(set, false)
    }

    fn amount_in(&self, set: GlyphSet, tight: bool) -> String {
        let approx = approx(set);
        let estimate = |usd: f64| marked_amount(&copy::usd(usd), true, false).to_string();
        let up_to = |scope: &Scope| match scope {
            Scope::Exact => String::new(),
            Scope::UpTo => format!("{} ", copy::UP_TO),
        };
        match self {
            Rebuild::PlanUsd(usd, scope) if tight => format!("{}{}", up_to(scope), estimate(*usd)),
            Rebuild::PlanUsd(usd, scope) => format!(
                "{}{}, {}",
                up_to(scope),
                estimate(*usd),
                billing_word(AccountState::Subscription)
            ),
            Rebuild::Usd(usd) => estimate(*usd),
            Rebuild::UpToUsd(usd) => format!("{}{}", up_to(&Scope::UpTo), estimate(*usd)),
            Rebuild::Tokens(n) => format!("{approx}{} {}", num(*n), copy::TOKENS),
            Rebuild::UpToTokens(n) => {
                format!(
                    "{}{approx}{} {}",
                    up_to(&Scope::UpTo),
                    num(*n),
                    copy::TOKENS
                )
            }
            Rebuild::Expired => copy::CACHE_EXPIRED_SHORT.to_owned(),
            Rebuild::Empty => copy::NOTHING_CACHED_SHORT.to_owned(),
            Rebuild::Unpriced(Scope::Exact) => copy::NO_PRICES.to_owned(),
            Rebuild::Unpriced(Scope::UpTo) => copy::up_to_unpriced(),
            Rebuild::Unknown => String::new(),
        }
    }

    #[must_use]
    pub fn unpriced(self) -> Rebuild {
        match self {
            Rebuild::PlanUsd(_, scope) => Rebuild::Unpriced(scope),
            Rebuild::Usd(_) | Rebuild::Tokens(_) => Rebuild::Unpriced(Scope::Exact),
            Rebuild::UpToUsd(_) | Rebuild::UpToTokens(_) => Rebuild::Unpriced(Scope::UpTo),
            other => other,
        }
    }

    pub fn line(&self, set: GlyphSet) -> String {
        self.sentence(set, false)
    }

    pub fn short(&self, set: GlyphSet) -> String {
        self.sentence(set, true)
    }

    fn sentence(&self, set: GlyphSet, tight: bool) -> String {
        match self {
            Rebuild::Empty if !tight => {
                format!("{}, so nothing to rebuild", copy::NOTHING_CACHED_SHORT)
            }
            Rebuild::Empty | Rebuild::Expired => self.amount_in(set, tight),
            Rebuild::Unpriced(Scope::Exact) => {
                format!(
                    "{}, {}",
                    copy::cache_rebuild("").trim_end(),
                    copy::NO_PRICES
                )
            }
            _ => copy::cache_rebuild(&self.amount_in(set, tight))
                .trim_end()
                .to_owned(),
        }
    }

    pub fn parts(&self, set: GlyphSet, named: bool) -> (String, String) {
        match self {
            Rebuild::PlanUsd(..) if named => (self.short(set), self.short(set)),
            Rebuild::PlanUsd(..) => (self.line(set), self.line(set)),
            _ => (self.line(set), self.short(set)),
        }
    }

    pub fn cell(&self) -> String {
        match self {
            Rebuild::PlanUsd(usd, _) | Rebuild::Usd(usd) | Rebuild::UpToUsd(usd) => {
                marked_amount(&copy::usd(*usd), true, false).to_string()
            }
            Rebuild::Empty | Rebuild::Expired => NONE.to_owned(),
            Rebuild::Tokens(_)
            | Rebuild::UpToTokens(_)
            | Rebuild::Unpriced(_)
            | Rebuild::Unknown => String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchCause {
    Convert,
    SmallerWindow,
    KeptTurns,
    Other,
}

impl SwitchCause {
    pub fn of(cause: &SwitchCompactionCause) -> SwitchCause {
        match cause {
            SwitchCompactionCause::Convert => SwitchCause::Convert,
            SwitchCompactionCause::SmallerWindow => SwitchCause::SmallerWindow,
            SwitchCompactionCause::KeptTurns => SwitchCause::KeptTurns,
            SwitchCompactionCause::Other(_) => SwitchCause::Other,
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            SwitchCause::Convert => copy::CAUSE_CONVERT,
            SwitchCause::SmallerWindow => copy::CAUSE_SMALLER_WINDOW,
            SwitchCause::KeptTurns => copy::CAUSE_KEPT_TURNS,
            SwitchCause::Other => copy::CAUSE_OTHER,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwitchWork {
    pub usd: Option<f64>,
    pub cause: SwitchCause,
    pub alone: bool,
}

impl SwitchWork {
    pub fn text(&self, set: GlyphSet) -> String {
        if self.alone {
            return copy::CONVERTS_BEFORE_SWITCHING.to_owned();
        }
        let detail = match self.usd {
            Some(usd) => format!(
                "{} {} {}",
                marked_amount(&copy::usd(usd), true, false),
                Glyphs::of(set).separator,
                self.cause.words()
            ),
            None => self.cause.words().to_owned(),
        };
        copy::compacts_then_switches(&detail)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Current,
    Kept,
    KeptIfPassedOn,
    Rebuild(Rebuild),
    Switch(Rebuild, SwitchWork),
    WithSwitch,
    WithEndpointSwitch,
    Unsupported,
    Unavailable,
}

impl Effect {
    pub fn text(&self, set: GlyphSet) -> String {
        match self {
            Effect::Current => NOW.to_owned(),
            Effect::Kept => CACHE_KEPT.to_owned(),
            Effect::KeptIfPassedOn => CACHE_KEPT_EFFORT.to_owned(),
            Effect::Rebuild(r) => r.line(set),
            Effect::Switch(r, work) => {
                format!(
                    "{} {} {}",
                    r.line(set),
                    Glyphs::of(set).separator,
                    work.text(set)
                )
            }
            Effect::WithSwitch => copy::WITH_THE_SWITCH.to_owned(),
            Effect::WithEndpointSwitch => copy::WITH_THE_ENDPOINT_SWITCH.to_owned(),
            Effect::Unsupported => copy::NO_ADAPTIVE_THINKING.to_owned(),
            Effect::Unavailable => copy::UNAVAILABLE.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change<'a> {
    pub endpoint: Option<&'a str>,
    pub account_bound: &'a [String],
    pub model: &'a str,
    pub effort: Option<Effort>,
    pub cache: &'a CacheEffect,
    pub model_changed: bool,
    pub effort_changed: bool,
    pub rebuild: Rebuild,
    pub search_refused_on: Option<&'a str>,
    pub search_refused_gateway: bool,
    pub search: Option<&'a str>,
    pub work: Option<SwitchWork>,
}

pub fn change_note(change: &Change<'_>, glyphs: &Glyphs) -> String {
    let sep = format!(" {} ", glyphs.separator);
    let model = model_line(change.model);
    let effort = change.effort.map(Effort::as_str);
    let effect = match change.cache {
        CacheEffect::Kept => CACHE_KEPT.to_owned(),
        CacheEffect::KeptIfPassedOn => CACHE_KEPT_EFFORT.to_owned(),
        CacheEffect::Rebuilt {
            reason: RebuildReason::PerMessageEffortRejected,
        } => {
            let tail = copy::gateway_rejected(&short_model(&model));
            return match effort {
                Some(effort) => {
                    format!("{}{sep}{tail}", copy::effort_changed(glyphs.arrow, effort))
                }
                None => tail,
            };
        }
        CacheEffect::Rebuilt {
            reason: RebuildReason::NativeSearchRefused,
        } => {
            let endpoint = one_line(change.search_refused_on.or(change.endpoint).unwrap_or("?"));
            let head = if change.search_refused_gateway {
                copy::gateway_search_refused(&endpoint)
            } else {
                copy::native_search_refused(&endpoint)
            };
            return format!("{head}{sep}{}", change.rebuild.line(glyphs.set));
        }
        CacheEffect::Rebuilt { .. } => change.rebuild.line(glyphs.set),
    };
    let head = match change.endpoint {
        Some(endpoint) => {
            let mut head = copy::endpoint_changed(glyphs.arrow, &one_line(endpoint));
            if change.model_changed {
                head = format!("{head}, {}", copy::model_also(glyphs.arrow, &model));
            }
            if let Some(effort) = effort.filter(|_| change.effort_changed) {
                head = format!("{head}, {}", copy::effort_also(glyphs.arrow, effort));
            }
            head
        }
        None => match (
            change.model_changed,
            effort.filter(|_| change.effort_changed),
        ) {
            (true, Some(effort)) => format!(
                "{}{sep}{}",
                copy::model_changed(glyphs.arrow, &model),
                copy::effort_with(effort)
            ),
            (true, None) => copy::model_changed(glyphs.arrow, &model),
            (false, Some(effort)) => copy::effort_changed(glyphs.arrow, effort),
            (false, None) => copy::unchanged(&short_model(&model), effort, &sep),
        },
    };
    let mut note = format!("{head}{sep}{effect}");
    if let Some(work) = change.work {
        note = format!("{note}{sep}{}", work.text(glyphs.set));
    }
    if let Some(search) = change.search {
        note = format!("{note}{sep}{}", one_line(search));
    }
    let bound: Vec<String> = change
        .account_bound
        .iter()
        .map(|id| model_name(&one_line(id)))
        .collect();
    if !bound.is_empty() {
        note = format!("{note}{sep}{}", copy::reasoning_may_not_carry(&bound));
    }
    note
}

pub fn setting_label(target: &SettingTarget) -> String {
    let label = target.spec().label;
    match target {
        SettingTarget::Name(_) => label.to_owned(),
        SettingTarget::Path(path) => match &path.place {
            Place::Global => label.to_owned(),
            Place::Endpoint(endpoint) => copy::label_on(label, &one_line(endpoint)),
            Place::Model(model) => copy::label_for(label, &model_line(model)),
            Place::EndpointModel { endpoint, model } => copy::label_on(
                &copy::label_for(label, &model_line(model)),
                &one_line(endpoint),
            ),
            Place::Agent(agent) => copy::label_for(label, &one_line(agent)),
            Place::PluginHook { plugin, hook } => {
                copy::label_for(label, &one_line(&format!("{plugin}/{hook}")))
            }
            Place::PluginSlot { plugin, section } => {
                copy::label_for(label, &one_line(&format!("{plugin}/{section}")))
            }
            Place::Plugin(plugin) => copy::label_for(label, &one_line(plugin)),
        },
    }
}

pub fn when_words(when: &When) -> String {
    match when {
        When::Now => copy::APPLIES_NOW.to_owned(),
        When::NextTurn => copy::NEXT_TURN.to_owned(),
        When::NextSession => wisp_protocol::settings::NEW_CHATS.to_owned(),
        When::NextCall => copy::APPLIES_NEXT_CALL.to_owned(),
        When::Other(other) => one_line(&other.replace('_', " ")),
    }
}

fn layer_words(layer: &Layer) -> String {
    let words = match layer {
        Layer::Forced => copy::LAYER_FORCED,
        Layer::Session => copy::LAYER_SESSION,
        Layer::Flag => copy::LAYER_FLAG,
        Layer::Env => copy::LAYER_ENV,
        Layer::Mode => copy::LAYER_MODE,
        Layer::Agent => copy::LAYER_AGENT,
        Layer::EndpointModel => copy::LAYER_ENDPOINT_MODEL,
        Layer::Model => copy::LAYER_MODEL,
        Layer::Endpoint => copy::LAYER_ENDPOINT,
        Layer::Config => wisp_protocol::CONFIG_FILE,
        Layer::Shipped => copy::LAYER_SHIPPED,
        Layer::Default => copy::LAYER_DEFAULT,
        Layer::Other(other) => return one_line(&other.replace('_', " ")),
    };
    words.to_owned()
}

fn bound_model(target: &SettingTarget) -> Option<&str> {
    match target {
        SettingTarget::Path(path) => match &path.place {
            Place::Model(model) | Place::EndpointModel { model, .. } => Some(model.as_str()),
            _ => None,
        },
        SettingTarget::Name(_) => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Told {
    pub was: String,
    pub saved: Option<String>,
    pub wins: Option<Wins>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wins {
    ThisSession(String),
    NewSessions(String),
}

impl Told {
    fn was_words(&self) -> String {
        match &self.saved {
            Some(saved) if *saved != self.was => {
                copy::was_saved(&one_line(&self.was), &one_line(saved))
            }
            _ => copy::was(&one_line(&self.was)),
        }
    }
}

pub fn setting_note(
    change: &AppliedChange,
    cost: Option<String>,
    current_model: &str,
    glyphs: &Glyphs,
    told: Option<&Told>,
) -> String {
    let sep = format!(" {} ", glyphs.separator);
    let label = setting_label(&change.target);
    let value = change
        .value
        .as_ref()
        .map(|value| format!("{label} {} {}", glyphs.arrow, one_line(&value.shown())));
    let mut parts = vec![value.clone().unwrap_or_else(|| copy::setting_reset(&label))];
    let elsewhere = change.when == When::NextTurn
        && bound_model(&change.target).is_some_and(|model| model != current_model);
    let beaten = change.when != When::NextSession
        && change
            .overridden
            .as_ref()
            .is_some_and(|o| o.scope == OverrideScope::ThisSession);
    if elsewhere {
        parts.push(copy::APPLIES_WHEN_CURRENT.to_owned());
    } else if !beaten {
        parts.push(when_words(&change.when));
        parts.extend(cost);
    }
    parts.extend(saved_parts(change, told));
    parts.extend(change.warnings.iter().map(|w| one_line(w)));
    parts.join(&sep)
}

pub fn saved_parts(change: &AppliedChange, told: Option<&Told>) -> Vec<String> {
    let mut parts = vec![];
    let was = told.map(Told::was_words);
    if let Some(saved) = &change.saved {
        let done = match &change.value {
            Some(_) => copy::setting_saved_to(&one_line(&saved.path_display)),
            None if told.is_some_and(|t| t.saved.is_some()) => copy::SAVED_VALUE_REMOVED.to_owned(),
            None if told.is_some() => copy::SESSION_VALUE_REMOVED.to_owned(),
            None => copy::setting_saved_to(&one_line(&saved.path_display)),
        };
        parts.push(match &was {
            Some(was) => format!("{done} {was}"),
            None => done,
        });
        parts.extend(saved.notes.iter().map(|n| one_line(n)));
    } else if let Some(was) = was {
        parts.push(was);
    }
    let wins = told.and_then(|t| t.wins.as_ref());
    match &change.overridden {
        Some(overridden) if overridden.scope == OverrideScope::ThisSession || wins.is_none() => {
            parts.push(still_wins(overridden));
        }
        _ => parts.extend(wins.map(|wins| match wins {
            Wins::ThisSession(by) => copy::now_wins_now(&one_line(by)),
            Wins::NewSessions(by) => copy::now_wins_later(&one_line(by)),
        })),
    }
    parts
}

fn still_wins(overridden: &Overridden) -> String {
    let by = layer_words(&overridden.by);
    let saved = matches!(
        overridden.by,
        Layer::Model | Layer::EndpointModel | Layer::Endpoint
    );
    match &overridden.scope {
        OverrideScope::ThisSession if saved => copy::still_wins(&by),
        OverrideScope::ThisSession => copy::still_wins_now(&by),
        OverrideScope::NewSessions => copy::still_wins_later(&by),
        OverrideScope::Other(scope) => copy::still_wins_other(&by, &one_line(scope)),
    }
}

pub fn model_name(id: &str) -> String {
    let short = short_model(id);
    let mut words = short.split('-');
    let family = words.next().unwrap_or_default();
    let version: Vec<&str> = words.collect();
    let numeric = !version.is_empty()
        && version
            .iter()
            .all(|v| !v.is_empty() && v.len() <= 2 && v.chars().all(|c| c.is_ascii_digit()));
    let mut chars = family.chars();
    match (chars.next(), numeric) {
        (Some(first), true) if family.chars().all(|c| c.is_ascii_lowercase()) => format!(
            "{}{} {}",
            first.to_ascii_uppercase(),
            chars.as_str(),
            version.join(".")
        ),
        _ => short,
    }
}
