use wisp_plugin_api::host::Host;
use wisp_plugin_api::index::{Entry, Kind};

use crate::note::{self, Scope};
use crate::store::{self, Inspected, NoteStatus, Store};

pub(super) fn entries(host: &dyn Host) -> Vec<Entry> {
    Store::reading(host)
        .notes()
        .filter_map(|file| entry(host, file))
        .collect()
}

fn entry(host: &dyn Host, file: Inspected) -> Option<Entry> {
    let note = file.note;
    let name = note.id.scope.key(&note.id.name);
    let text = content(&note.fact, &file.body);
    (store::safe(host, &name) && store::safe(host, &note.fact) && store::safe(host, &text))
        .then_some(Entry {
            kind: Kind::Document,
            name,
            summary: note.fact,
            text,
        })
}

fn content(fact: &str, body: &str) -> String {
    if body.is_empty() {
        fact.to_owned()
    } else {
        format!("{fact}\n\n{body}")
    }
}

pub(super) fn open(host: &dyn Host, name: &str) -> Result<String, String> {
    open_note(host, name).ok_or_else(|| store::NOT_FOUND.to_owned())
}

fn open_note(host: &dyn Host, key: &str) -> Option<String> {
    let (scope, name) = Scope::split_key(key)?;
    if (scope == Scope::You && !store::is_root(host)) || !note::slug(name) {
        return None;
    }
    let NoteStatus::Valid(file) = Store::reading(host).inspect(scope, name) else {
        return None;
    };
    let text = content(&file.note.fact, &file.body);
    (store::safe(host, key) && store::safe(host, &text)).then_some(text)
}
