use wisp_protocol::CONFIG_FILE;
use wisp_protocol::settings::{
    Applies, Form, Kind, Layer, NEW_CHATS, Place, Section, SettingValue, TokensOrPercent, names,
};
use wisp_render::chrome::KeyHint;
use wisp_render::chrome::config_screen::{
    Check, Detail, EditView, LayerLine, SAVED, THIS_CHAT, WISPS_OWN,
};

use super::edit::{
    CheckItem, Context, Edit, Field, Widget, after_reset, can_reset, check, mark_of, widget,
};
use super::entries::CHILD_AGENTS_ONLY;
use super::rows::{Changed, Found, Row, Set};
use super::{CANCEL_KEY, CHOOSE_KEYS, MOVE_KEYS, SAVE_KEY, SEP, TOGGLE_KEY};
use wisp_render::chrome::config_screen::CHOOSE_LABEL;
use wisp_render::transcript::copy::{CHOOSE_WORD, ENTER_KEY, sentence_case};

pub const NOT_USED_HERE: &str = "not used by this output";
pub type Keys = &'static [(&'static str, &'static str)];
pub const CHOICE_KEYS: Keys = &[CANCEL_KEY, CHOOSE_KEYS, SAVE_KEY];
pub const TEXT_KEYS: Keys = &[CANCEL_KEY, SAVE_KEY];
pub const CHECK_KEYS: Keys = &[CANCEL_KEY, MOVE_KEYS, TOGGLE_KEY, SAVE_KEY];
pub const FIXED_CHECK_KEYS: Keys = &[CANCEL_KEY, MOVE_KEYS, SAVE_KEY];
const KEPT_NOTE: &str = "no plugin provides it; kept";
const KEPT_UNSHOWN: &str = "a saved value that is no tool";
pub const CHOOSE_KEY: (&str, &str) = (ENTER_KEY, CHOOSE_WORD);
pub const EDIT_KEY: (&str, &str) = (ENTER_KEY, "edit");
pub const CHECK_LABEL: &str = "Use";
const CHECKED: &str = "[x]";
const UNCHECKED: &str = "[ ]";

fn check_line(item: &CheckItem, on: bool) -> String {
    if item.by_row {
        return item.name.clone();
    }
    if item.kept {
        let shown = if wisp_protocol::plugin::shows_tool_name(&item.name)
            || wisp_protocol::settings::section_ref(&item.name)
        {
            item.name.as_str()
        } else {
            KEPT_UNSHOWN
        };
        return match &item.follows {
            Some(_) if item.name == wisp_protocol::agents::REPORT => {
                format!("    {shown}{SEP}{CHILD_AGENTS_ONLY}; kept")
            }
            Some(leader) => format!("    {shown}{SEP}comes with {leader}; kept"),
            None => format!("    {shown}{SEP}{KEPT_NOTE}"),
        };
    }
    if let (Some(leader), false) = (&item.follows, item.default_on) {
        return match &item.note {
            Some(note) => format!("    {}{SEP}{note}", item.name),
            None => format!("    {} (follows {leader})", item.name),
        };
    }
    let mark = if on { CHECKED } else { UNCHECKED };
    let line = match &item.follows {
        Some(leader) => format!("    {mark} {} (comes with {leader})", item.name),
        None => format!("{mark} {}", item.name),
    };
    match &item.note {
        Some(note) => format!("{line}{SEP}{note}"),
        None => line,
    }
}
const CHOICES_SHOWN: usize = 6;

fn title_word(set: &Set) -> String {
    match set {
        Set::Detected => "Detected".to_owned(),
        Set::Layer(layer) => match layer {
            Layer::Forced => "Forced".to_owned(),
            layer if chat_layer(layer) => sentence_case(THIS_CHAT),
            Layer::Env => "Environment".to_owned(),
            Layer::Mode => "Mode".to_owned(),
            Layer::Agent => "Agent".to_owned(),
            Layer::EndpointModel | Layer::Model | Layer::Endpoint | Layer::Config => {
                SAVED.to_owned()
            }
            Layer::Shipped => WISPS_OWN.to_owned(),
            Layer::Default => WISPS_OWN.to_owned(),
            other => sentence_case(other.word()),
        },
    }
}

pub fn winner_words(set: &Set) -> String {
    match set {
        Set::Detected => "detection".to_owned(),
        Set::Layer(layer) => match layer {
            Layer::Forced => "the forced value".to_owned(),
            Layer::Session => "the session value".to_owned(),
            Layer::Flag => "the flag".to_owned(),
            Layer::Env => "the variable".to_owned(),
            Layer::Mode => "the mode".to_owned(),
            Layer::Agent => "the agent type".to_owned(),
            Layer::EndpointModel | Layer::Model | Layer::Endpoint | Layer::Config => {
                "the saved value".to_owned()
            }
            Layer::Shipped => "the shipped value".to_owned(),
            Layer::Default => "the default".to_owned(),
            Layer::Other(word) => format!("the {word} value"),
        },
    }
}

fn place(found: &Found, row: &Row, several: bool) -> String {
    let origin = wisp_protocol::hide_quoted_models(&found.origin);
    let Some(words) = found.set.site() else {
        return origin;
    };
    let file = match found.line {
        Some(line) => format!("{CONFIG_FILE} line {line}"),
        None => CONFIG_FILE.to_owned(),
    };
    if several {
        format!("{words}{SEP}{file}")
    } else if found.origin == row.spec.file_key() {
        file
    } else {
        format!("{origin}{SEP}{file}")
    }
}

pub fn help_title(help: &str) -> String {
    let help = help.trim().trim_end_matches('.');
    let mut chars = help.chars();
    let (Some(first), second) = (chars.next(), chars.next()) else {
        return help.to_owned();
    };
    if second.is_some_and(char::is_uppercase) {
        return help.to_owned();
    }
    let rest: String = help.chars().skip(1).collect();
    format!("{}{rest}", first.to_lowercase())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Quote {
    pub rebuild: String,
    pub reset: ResetCost,
    pub endpoint: Option<String>,
    pub tools: String,
    pub drops: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ResetCost {
    #[default]
    Applies,
    Quoted(String),
    Unknown,
}

pub fn when_words(applies: Applies, quote: &Quote) -> String {
    use wisp_render::transcript::copy;
    match applies {
        Applies::Now | Applies::NextTurnKept | Applies::NextTurnEffort | Applies::Reserved => {
            String::new()
        }
        Applies::NextSession | Applies::Connection => NEW_CHATS.to_owned(),
        Applies::NextTurnMayRebuild if !quote.rebuild.is_empty() => {
            copy::cache_rebuild(&quote.rebuild)
        }
        Applies::NextTurnMayRebuild => copy::MAY_REBUILD.to_owned(),
        Applies::NextTurnRebuild => copy::CACHE_REBUILD.to_owned(),
        Applies::NextCall => applies.words().to_owned(),
    }
}

pub(super) fn chat_layer(layer: &Layer) -> bool {
    matches!(layer, Layer::Session | Layer::Flag)
}

pub fn reset_value(row: &Row) -> Option<Option<SettingValue>> {
    if row
        .winner
        .iter()
        .chain(&row.shadowed)
        .any(|f| f.set == Set::Layer(Layer::Flag))
    {
        if !row.spec.mid_session
            || row.spec.settles_on_save()
            || row.spec.defers(row.new_chat.as_ref())
        {
            return None;
        }
        row.new_chat.clone().map(Some)
    } else {
        Some(None)
    }
}

pub fn new_chats(row: &Row) -> String {
    row.new_chat
        .as_ref()
        .map_or_else(|| row.default.clone(), |v| super::rows::shown(row.spec, v))
}

pub fn chat_only(row: &Row) -> bool {
    row.winner.as_ref().is_some_and(|w| {
        matches!(&w.set, Set::Layer(layer) if chat_layer(layer)) && w.value != new_chats(row)
    })
}

pub(super) fn chat_values(now: &str, saved: &str) -> String {
    format!(
        "{}: {now}. {}: {saved}.",
        sentence_case(THIS_CHAT),
        sentence_case(NEW_CHATS)
    )
}

fn saved(row: &Row) -> Option<&Found> {
    row.winner
        .iter()
        .chain(&row.shadowed)
        .find(|f| f.set.site().is_some())
}

pub fn chat_line(row: &Row) -> String {
    let Some(winner) = &row.winner else {
        return String::new();
    };
    let by = match winner.set {
        Set::Layer(ref layer) if chat_layer(layer) && chat_only(row) => {
            if new_chats(row).is_empty() {
                return format!("{}: {}.", sentence_case(THIS_CHAT), winner.value);
            }
            return chat_values(&winner.value, &new_chats(row));
        }
        Set::Layer(Layer::Env | Layer::Forced) => "Set outside Wisp",
        Set::Layer(Layer::Mode) => "Set by the mode",
        Set::Layer(Layer::Agent) => "Set by the preset",
        _ => return String::new(),
    };
    match saved(row) {
        Some(saved) => format!("{by}. {SAVED}: {}.", saved.value),
        None if !row.default.is_empty() => format!("{by}. {WISPS_OWN}: {}.", row.default),
        None => format!("{by}."),
    }
}

pub fn hint((key, does): (&str, &str)) -> KeyHint {
    KeyHint::new(key, does)
}

pub fn notes(text: &str) -> impl Iterator<Item = KeyHint> {
    text.split(SEP)
        .filter(|part| !part.is_empty())
        .map(KeyHint::note)
}

pub fn hints(keys: &[(&str, &str)]) -> Vec<KeyHint> {
    keys.iter().copied().map(hint).collect()
}

pub fn toggle_key(row: &Row) -> KeyHint {
    let on = matches!(row.current, Some(SettingValue::Bool(true)));
    KeyHint::new(ENTER_KEY, format!("turn {}", if on { "off" } else { "on" }))
}

pub fn reset_key(row: &Row, quote: &Quote) -> Option<Vec<KeyHint>> {
    if !can_reset(row)
        || matches!(row.spec.kind, Kind::Bool)
        || row.winner.as_ref().is_some_and(|w| {
            matches!(
                w.set,
                Set::Layer(Layer::Env | Layer::Forced | Layer::Mode | Layer::Agent)
            )
        })
    {
        return None;
    }
    let to = if chat_only(row) {
        new_chats(row)
    } else {
        after_reset(row).map_or_else(|| row.default.clone(), |f| f.value.clone())
    };
    let to = if to.is_empty() || to == super::edit::NOT_SET {
        WISPS_OWN
    } else {
        &to
    };
    let reset = KeyHint::new("d", format!("back to {to}"));
    let cost = match &quote.reset {
        ResetCost::Applies => when_words(row.spec.applies, quote),
        ResetCost::Quoted(cost) => cost.clone(),
        ResetCost::Unknown => return Some(vec![reset]),
    };
    Some(std::iter::once(reset).chain(notes(&cost)).collect())
}

fn keys(row: &Row, edit: Option<&Edit>, view: &View<'_>) -> Vec<KeyHint> {
    let (quote, locked) = (view.quote, view.locked);
    match edit.map(|e| &e.field) {
        Some(Field::Choice { .. }) => return hints(CHOICE_KEYS),
        Some(Field::Text { .. }) => return hints(TEXT_KEYS),
        Some(Field::Checklist { .. }) if edit.is_some_and(|e| !e.toggles()) => {
            return hints(FIXED_CHECK_KEYS);
        }
        Some(Field::Checklist { .. }) => return hints(CHECK_KEYS),
        None => {}
    }
    if let Some(why) = locked {
        return vec![KeyHint::note(why)];
    }
    let first = match widget(row.spec) {
        None => {
            return vec![KeyHint::note(format!(
                "Change it with {}",
                set_command(row)
            ))];
        }
        Some(Widget::Toggle) => toggle_key(row),
        Some(Widget::Choice | Widget::Checklist | Widget::Picker(_)) => hint(CHOOSE_KEY),
        Some(Widget::Text) => hint(EDIT_KEY),
    };
    let mut out = vec![first];
    out.extend(notes(&when_words(row.spec.applies, quote)));
    out.extend(reset_key(row, quote).unwrap_or_default());
    out
}

fn edit_view(row: &Row, edit: &Edit, context: Context) -> EditView {
    match &edit.field {
        Field::Choice { at } => EditView::Choices {
            label: CHOOSE_LABEL.to_owned(),
            choices: row
                .spec
                .kind
                .choices()
                .iter()
                .map(|c| {
                    row.spec.parse(c).map_or_else(
                        |_| (*c).to_owned(),
                        |value| super::rows::shown(row.spec, &value),
                    )
                })
                .collect(),
            at: *at,
        },
        Field::Checklist { items, marks, at } => EditView::Choices {
            label: CHECK_LABEL.to_owned(),
            choices: items
                .iter()
                .enumerate()
                .map(|(i, item)| check_line(item, mark_of(items, marks, i)))
                .collect(),
            at: *at,
        },
        Field::Text { text, cursor } => {
            let removal = edit.site().filter(|s| s.saved && text.trim().is_empty());
            EditView::Text {
                text: text.clone(),
                cursor: *cursor,
                unit: if removal.is_some() {
                    String::new()
                } else {
                    row.spec.describe(Form::Unit)
                },
                check: removal.map_or_else(
                    || check(row.spec, text, context),
                    |site| Check::Preview(format!("remove it from {}", site.label)),
                ),
                sites: edit.sites.iter().map(|s| s.label.clone()).collect(),
                site: edit.site,
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    pub quote: &'a Quote,
    pub edit: Option<&'a Edit>,
    pub context: Context,
    pub locked: Option<&'static str>,
    pub sites: bool,
}

pub fn set_command(row: &Row) -> String {
    let path = match row.path.place {
        Place::Global => row.spec.name.to_owned(),
        _ => row.path.shown(),
    };
    let path = if path.contains('"') {
        format!("'{path}'")
    } else {
        path
    };
    format!("wisp config set {path} VALUE")
}

pub fn changed_words(changed: &Changed) -> String {
    match changed {
        Changed::Now(value) => format!("{CONFIG_FILE} now has {value}{SEP}{NEW_CHATS}"),
        Changed::Removed => format!("{CONFIG_FILE} no longer sets it{SEP}{NEW_CHATS}"),
    }
}

pub fn detail(row: &Row, view: View<'_>) -> Detail {
    let edit = view.edit.filter(|e| e.name == row.spec.name);
    let places = view.sites && row.section != Section::Models;
    let mut layers = vec![];
    if let Some(winner) = &row.winner {
        layers.push(LayerLine {
            word: title_word(&winner.set),
            value: winner.value.clone(),
            origin: place(winner, row, places),
            winner: true,
            note: None,
        });
    }
    for found in &row.shadowed {
        layers.push(LayerLine {
            word: title_word(&found.set),
            value: found.value.clone(),
            origin: place(found, row, places),
            winner: false,
            note: None,
        });
    }
    let mut notes = vec![];
    let item = match edit.map(|e| &e.field) {
        Some(Field::Checklist { items, at, .. }) => items.get(*at),
        _ => None,
    };
    let by_row = item.is_some_and(|i| i.by_row);
    if let Some(changed) = &row.changed {
        notes.push(changed_words(changed));
    }
    if !row.applies {
        notes.push(NOT_USED_HERE.to_owned());
    }
    if let Some(locked) = item.and_then(|i| i.locked.clone()) {
        notes.push(locked);
        if by_row {
            notes.rotate_right(1);
        }
    }
    let capped = (row.spec.name == names::COMPACT_AT)
        .then(|| {
            saved(row)
                .filter(|saved| {
                    let configured = row.current.as_ref().and_then(TokensOrPercent::of);
                    row.value != saved.value
                        && view
                            .context
                            .point(configured)
                            .is_some_and(|point| point.capped_from.is_some())
                })
                .map(|saved| format!("{SAVED}: {}.", saved.value))
        })
        .flatten();
    Detail {
        lead: row.spec.label.to_owned(),
        title: help_title(row.spec.help),
        name: row.spec.file_key().to_owned(),
        line: notes.first().cloned().unwrap_or_else(|| {
            let chat = chat_line(row);
            if chat.is_empty() {
                capped.unwrap_or_default()
            } else {
                chat
            }
        }),
        fallback: if !row.default.is_empty()
            && row.winner.as_ref().is_some_and(|w| w.value != row.default)
        {
            format!("{WISPS_OWN}: {}", row.default)
        } else {
            String::new()
        },
        layers,
        default: row.default.clone(),
        default_wins: row.winner.is_none(),
        default_origin: if row.spec.name == names::COMPACT_AT {
            view.context.base().map_or_else(String::new, |w| {
                format!(
                    "of this model's {} tokens",
                    wisp_protocol::settings::thousands(w)
                )
            })
        } else {
            String::new()
        },
        choices: if row.choices.len() > CHOICES_SHOWN {
            vec![format!("{} choices", row.choices.len())]
        } else {
            row.choices.clone()
        },
        applies: if by_row {
            String::new()
        } else {
            when_words(row.spec.applies, view.quote)
        },
        notes,
        keys: keys(row, edit, &view),
        edit: edit.map(|e| edit_view(row, e, view.context)),
        ask: None,
        label_width: 0,
    }
}
