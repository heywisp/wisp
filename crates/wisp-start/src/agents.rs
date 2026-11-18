use wisp_core::agents::endpoints::{EndpointView, shown_endpoint};
use wisp_core::agents::{
    CheckedPresets, EndpointNote, Limit, Limits, PresetInstructions, PresetPlace, Presets,
};
use wisp_core::endpoints::ConfigFile;
use wisp_core::settings::{Current, Given, Layers};
use wisp_protocol::agents::Preset;
use wisp_protocol::settings::DEFAULT_MODEL;
use wisp_protocol::{CONFIG_FILE, shown_model, shown_text};
use wisp_render::transcript::copy::SEP;

pub const NOT_LOADED: &str = concat!("not read: ", wisp_protocol::config_file!(), " didn't load");

fn limit_row(limit: &Limit) -> (String, String) {
    let leaf = limit
        .name
        .split_once('.')
        .map_or(limit.name, |(_, leaf)| leaf);
    let source = match (limit.source, limit.line) {
        ("config", Some(line)) => format!("{CONFIG_FILE} line {line}"),
        (source, _) => source.to_owned(),
    };
    let value = limit
        .value
        .map_or_else(|| "none".to_owned(), |n| n.to_string());
    (leaf.to_owned(), format!("{value} ({source})"))
}

fn instructions_words(found: &PresetInstructions, home: Option<&std::path::Path>) -> String {
    let file = wisp_protocol::shorten_path(&found.path, home);
    let file = shown_text(&file);
    let files = if found.files == 1 { "file" } else { "files" };
    format!(
        "instructions {file} ({} {files}, about {} tokens)",
        found.files,
        wisp_core::context::group(wisp_core::instructions::estimate_tokens(found.bytes))
    )
}

fn account_words(endpoint: &str, note: Option<&EndpointNote>) -> String {
    let name = shown_endpoint(endpoint).unwrap_or(wisp_protocol::NOT_SHOWN);
    let account = wisp_core::endpoints::goes::named(name);
    match note {
        Some(EndpointNote::Unavailable(reason)) => format!("{account} (unavailable: {reason})"),
        Some(EndpointNote::NotChecked) => format!("{account} (not checked)"),
        Some(EndpointNote::NotSetUp) => format!("{account} isn't set up"),
        None => account,
    }
}

fn preset_words(
    preset: &Preset,
    instructions: Option<&PresetInstructions>,
    note: Option<&EndpointNote>,
    fallback_notes: &[Option<EndpointNote>],
    home: Option<&std::path::Path>,
) -> String {
    let mut parts = vec![shown_text(&preset.description).to_owned()];
    if let Some(endpoint) = &preset.endpoint {
        parts.push(account_words(endpoint, note));
    }
    if let Some(model) = &preset.model {
        let unchecked = if note.is_some() { " (not checked)" } else { "" };
        parts.push(format!("model {}{unchecked}", shown_model(model)));
    }
    if let Some(effort) = preset.effort {
        parts.push(format!("effort {}", effort.as_str()));
    }
    for (index, choice) in preset.fallback.iter().enumerate() {
        let note = fallback_notes.get(index).and_then(Option::as_ref);
        let mut words = vec![account_words(&choice.endpoint, note)];
        if let Some(model) = &choice.model {
            words.push(format!("model {}", shown_model(model)));
        }
        if let Some(effort) = choice.effort {
            words.push(format!("effort {}", effort.as_str()));
        }
        parts.push(format!("fallback {}: {}", index + 1, words.join(", ")));
    }
    if !preset.tools_off.is_empty() {
        parts.push(format!("tools off {}", preset.tools_off.join(", ")));
    }
    if preset.spawn == Some(false) {
        parts.push("no spawning".to_owned());
    }
    if preset.worker == Some(false) {
        parts.push("not a worker".to_owned());
    }
    if let Some(found) = instructions {
        parts.push(instructions_words(found, home));
    }
    parts.join(SEP)
}

pub fn tools_off_names() -> Vec<String> {
    let (registry, _) = crate::tools::registry();
    wisp_protocol::agents::tools_off_names(&crate::tools::catalog(&registry))
}

pub fn tools_off_followers() -> Vec<(String, String)> {
    let (registry, _) = crate::tools::registry();
    wisp_protocol::agents::tools_off_followers(&crate::tools::catalog(&registry))
}

pub fn checked(
    presets: Presets,
    view: Option<&EndpointView>,
    place: &PresetPlace,
) -> CheckedPresets {
    presets.checked(view, &tools_off_names(), place)
}

#[derive(Debug, Clone, Copy)]
pub enum Config<'a> {
    Broken,
    Missing,
    Read(&'a ConfigFile),
}

pub fn doctor_rows(
    config: Config<'_>,
    flags: &[Given],
    place: &PresetPlace,
    view: Option<&EndpointView>,
) -> Vec<(String, String)> {
    let layers = Layers {
        flags: flags.to_vec(),
        saved: match config {
            Config::Read(file) => Some(&file.settings),
            Config::Broken | Config::Missing => None,
        },
        ..Layers::default()
    };
    let current = Current {
        endpoint: None,
        model: DEFAULT_MODEL.to_owned(),
    };
    let mut rows: Vec<(String, String)> = Limits::explained(&layers, &current)
        .iter()
        .map(limit_row)
        .collect();
    let presets = match config {
        Config::Broken => {
            rows.push(("presets".to_owned(), NOT_LOADED.to_owned()));
            return rows;
        }
        Config::Missing => CheckedPresets::default(),
        Config::Read(file) => checked(file.presets.clone(), view, place),
    };
    if presets.is_empty() {
        rows.push(("presets".to_owned(), "none".to_owned()));
        return rows;
    }
    let home = place.home.as_deref();
    for preset in presets.usable() {
        let found = presets.instructions(&preset.name);
        rows.push((
            format!("preset {}", preset.name),
            preset_words(
                preset,
                found,
                presets.note(&preset.name),
                presets.fallback_notes(&preset.name),
                home,
            ),
        ));
        if let Some(large) = found.and_then(|f| f.large.as_ref()) {
            rows.push((
                format!("preset {}", preset.name),
                format!("warning: {large}"),
            ));
        }
    }
    for off in presets.off() {
        rows.push((
            format!("preset {}", off.shown_name()),
            format!("off ({CONFIG_FILE} line {}): {}", off.line, off.problem),
        ));
    }
    rows
}

pub fn doctor_text(rows: &[(String, String)]) -> String {
    let mut text = String::from("\nagents\n");
    for (key, value) in rows {
        let key = wisp_render::text::clean(key);
        let value = wisp_render::text::clean(value);
        text.push_str(&format!("  {key:<18} {value}\n"));
    }
    text
}

#[cfg(test)]
mod tests;
