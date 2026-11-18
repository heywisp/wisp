use wisp_protocol::settings::{
    Kind, Place, SavedEntry, SettingValue, SettingsSnapshot, SwitchTarget,
};
use wisp_protocol::{NOT_A_MODEL_ID, NOT_SHOWN, hide_models, hide_quoted_models, model_hidden};

fn model_values<'a>(kind: Kind, value: &'a SettingValue, out: &mut Vec<&'a str>) {
    match (kind, value) {
        (Kind::ModelId, SettingValue::Text(id)) => out.push(id),
        (Kind::ModelIds, SettingValue::List(ids)) => out.extend(ids.iter().map(String::as_str)),
        _ => {}
    }
}

fn hidden_models(snapshot: &SettingsSnapshot) -> Vec<String> {
    let mut found: Vec<&str> = vec![snapshot.context.model.as_str()];
    found.extend(snapshot.models.iter().map(|m| m.id.as_str()));
    for quote in &snapshot.switches {
        if let SwitchTarget::Model(id) | SwitchTarget::Pair { model: id, .. } = &quote.target {
            found.push(id);
        }
        found.extend(quote.remembered.as_ref().map(|p| p.model.as_str()));
    }
    for setting in &snapshot.settings {
        let kind = setting.name.spec().kind;
        if let Some(value) = &setting.value {
            model_values(kind, value, &mut found);
        }
        for under in &setting.shadowed {
            model_values(kind, &under.value, &mut found);
        }
        if kind == Kind::ModelId {
            found.extend(setting.choices.iter().map(String::as_str));
        }
    }
    for saved in &snapshot.saved {
        if let Place::Model(model) | Place::EndpointModel { model, .. } = &saved.path.place {
            found.push(model);
        }
        if let SavedEntry::Value { value } = &saved.entry {
            model_values(saved.path.spec().kind, value, &mut found);
        }
    }
    let mut out: Vec<String> = found
        .into_iter()
        .filter(|id| placeholder_free(id) && model_hidden(id))
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn placeholder_free(id: &str) -> bool {
    !id.is_empty() && !id.contains(NOT_SHOWN) && !id.contains(NOT_A_MODEL_ID)
}

struct Scrub {
    hidden: Vec<String>,
}

impl Scrub {
    fn text(&self, text: &str) -> String {
        hide_models(text, self.hidden.iter().map(String::as_str))
    }

    fn origin(&self, origin: &str) -> String {
        self.text(&hide_quoted_models(origin))
    }

    fn value(&self, value: &SettingValue) -> SettingValue {
        match value {
            SettingValue::Text(text) => SettingValue::Text(self.text(text)),
            SettingValue::List(items) => {
                SettingValue::List(items.iter().map(|i| self.text(i)).collect())
            }
            SettingValue::Other { kind, text } => SettingValue::Other {
                kind: kind.clone(),
                text: self.text(text),
            },
            other => other.clone(),
        }
    }

    fn place(&self, place: &Place) -> Place {
        match place {
            Place::Model(model) => Place::Model(self.text(model)),
            Place::EndpointModel { endpoint, model } => Place::EndpointModel {
                endpoint: endpoint.clone(),
                model: self.text(model),
            },
            other => other.clone(),
        }
    }
}

pub(super) fn scrubbed(snapshot: &SettingsSnapshot) -> SettingsSnapshot {
    let hidden = hidden_models(snapshot);
    if hidden.is_empty() {
        return snapshot.clone();
    }
    let scrub = Scrub { hidden };
    let mut out = snapshot.clone();
    out.saved.retain(|saved| match &saved.path.place {
        Place::Model(model) | Place::EndpointModel { model, .. } if model_hidden(model) => {
            *model == snapshot.context.model
        }
        _ => true,
    });
    out.context.model = scrub.text(&out.context.model);
    for setting in &mut out.settings {
        setting.value = setting.value.as_ref().map(|v| scrub.value(v));
        setting.origin = scrub.origin(&setting.origin);
        for choice in &mut setting.choices {
            *choice = scrub.text(choice);
        }
        for under in &mut setting.shadowed {
            under.value = scrub.value(&under.value);
            under.origin = scrub.origin(&under.origin);
        }
    }
    for saved in &mut out.saved {
        saved.path.place = scrub.place(&saved.path.place);
        if let SavedEntry::Value { value } = &mut saved.entry {
            *value = scrub.value(value);
        }
    }
    for model in &mut out.models {
        model.id = scrub.text(&model.id);
    }
    for quote in &mut out.switches {
        if let SwitchTarget::Model(id) | SwitchTarget::Pair { model: id, .. } = &mut quote.target {
            *id = scrub.text(id);
        }
        if let Some(pair) = &mut quote.remembered {
            pair.model = scrub.text(&pair.model);
        }
    }
    for endpoint in &mut out.endpoints {
        endpoint.unavailable = endpoint.unavailable.as_ref().map(|u| scrub.text(u));
    }
    out
}

#[cfg(test)]
mod tests;
