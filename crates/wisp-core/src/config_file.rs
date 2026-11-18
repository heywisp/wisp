use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use std::time::Duration;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};
use wisp_protocol::settings::{Place, Secrecy, SettingValue, SitePath, names};
use wisp_protocol::{CONFIG_FILE, check_model_id, model_looks_like_key, setting_looks_like_key};

use crate::endpoints::{
    AuthKind, ConfigFile, ENV, EndpointError, EndpointName, FileError, GIVEN_CONFIG, PathShown,
    PricingMode, Problem, ProviderKind, keys_file_table,
};
use wisp_protocol::VariableName;

use crate::secrets::{
    LockKind, PathNote, PathNoteKind, StoreError, StoreId, StoreKind, create_folder, lock_noting,
    write_atomic,
};

pub const NEW_MODE: u32 = 0o600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteProblem {
    Unreadable(io::ErrorKind),
    NotUtf8,
    Broken(Box<FileError>),
    Invalid(Box<FileError>),
    TargetNotWritable(Box<PathNote>),
    DanglingLink,
    Write(io::ErrorKind),
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConfigWriteError {
    pub path: PathBuf,
    pub shown: PathShown,
    pub problem: WriteProblem,
}

impl fmt::Debug for ConfigWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigWriteError")
            .field("path", &self.shown.path(&self.path))
            .field("shown", &self.shown)
            .field("problem", &self.problem)
            .finish()
    }
}

impl ConfigWriteError {
    fn place(&self) -> String {
        match self.shown {
            PathShown::Default => crate::workspace::home_shown(&self.path),
            PathShown::Given => GIVEN_CONFIG.to_owned(),
        }
    }
}

impl fmt::Display for ConfigWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let place = self.place();
        match &self.problem {
            WriteProblem::Unreadable(kind) => write!(f, "could not read {place}: {kind}"),
            WriteProblem::NotUtf8 => write!(f, "{place} is not valid UTF-8"),
            WriteProblem::Broken(e) => write!(f, "{e}; nothing was changed"),
            WriteProblem::Invalid(e) => write!(
                f,
                "the change would leave {place} invalid ({e}); nothing was changed"
            ),
            WriteProblem::TargetNotWritable(_) => match self.shown {
                PathShown::Default => write!(f, "{place} points into a folder Wisp can't write"),
                PathShown::Given => {
                    write!(f, "{place} (a link) points into a folder Wisp can't write")
                }
            },
            WriteProblem::DanglingLink => write!(
                f,
                "could not write {place}: it is a link to a file that doesn't exist; create that file or replace the link. Nothing was changed"
            ),
            WriteProblem::Write(kind) => write!(f, "could not write {place} ({kind})"),
        }
    }
}

impl std::error::Error for ConfigWriteError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewEndpoint {
    pub provider: ProviderKind,
    pub auth: AuthKind,
    pub store: StoreKind,
    pub pricing: PricingMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTable {
    pub provider: ProviderKind,
    pub auth: AuthKind,
    pub key_env: VariableName,
    pub url: Option<String>,
    pub url_env: Option<VariableName>,
    pub pricing: PricingMode,
}

pub struct ConfigText {
    path: PathBuf,
    shown: PathShown,
    doc: DocumentMut,
    existed: bool,
    leading: usize,
}

impl fmt::Debug for ConfigText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigText")
            .field("path", &self.shown.path(&self.path))
            .field("shown", &self.shown)
            .field("existed", &self.existed)
            .finish()
    }
}

pub fn target_of(path: &Path) -> io::Result<(PathBuf, bool)> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok((std::fs::canonicalize(path)?, true)),
        Ok(_) => Ok((path.to_owned(), false)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((path.to_owned(), false)),
        Err(e) => Err(e),
    }
}

pub fn lock_folder(path: &Path) -> PathBuf {
    let target = target_of(path).map_or_else(|_| path.to_owned(), |(target, _)| target);
    match target.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(folder) => folder.to_owned(),
        None => PathBuf::from("."),
    }
}

pub fn canonical_config(path: &Path) -> Option<PathBuf> {
    if let Ok(real) = std::fs::canonicalize(path) {
        return Some(real);
    }
    let name = path.file_name()?;
    let folder = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(folder) => folder.to_owned(),
        None => PathBuf::from("."),
    };
    Some(std::fs::canonicalize(folder).ok()?.join(name))
}

fn string_value(text: &str, keep: Option<&Value>) -> Value {
    let mut value = Value::from(text);
    if let Some(old) = keep {
        *value.decor_mut() = old.decor().clone();
    }
    value
}

fn prefix_of(key: &toml_edit::Key) -> String {
    key.leaf_decor()
        .prefix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn swap_entries(
    entries: Vec<(toml_edit::Key, Item)>,
    gone: &[&str],
    key: &str,
    value: &str,
) -> Vec<(toml_edit::Key, Item)> {
    let mut kept: Vec<(toml_edit::Key, Item)> = Vec::with_capacity(entries.len());
    let mut placed: Option<usize> = None;
    for (old_key, item) in entries {
        if old_key.get() != key && !gone.contains(&old_key.get()) {
            kept.push((old_key, item));
            continue;
        }
        match placed {
            None => {
                let decor = old_key.leaf_decor().clone();
                let new_value = string_value(value, item.as_value());
                placed = Some(kept.len());
                kept.push((
                    toml_edit::Key::new(key).with_leaf_decor(decor),
                    Item::Value(new_value),
                ));
            }
            Some(at) => {
                let comment = prefix_of(&old_key);
                if let Some((first, _)) = kept.get_mut(at)
                    && !comment.trim().is_empty()
                {
                    let joined = format!("{}{comment}", prefix_of(first));
                    first.leaf_decor_mut().set_prefix(joined);
                }
            }
        }
    }
    kept
}

impl ConfigText {
    pub fn read(path: &Path, shown: PathShown) -> Result<Self, ConfigWriteError> {
        let fail = |problem| ConfigWriteError {
            path: path.to_owned(),
            shown,
            problem,
        };
        let (text, existed) = match std::fs::read(path) {
            Ok(bytes) => (
                String::from_utf8(bytes).map_err(|_| fail(WriteProblem::NotUtf8))?,
                true,
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let dangling =
                    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
                if dangling {
                    return Err(fail(WriteProblem::DanglingLink));
                }
                (String::new(), false)
            }
            Err(e) => return Err(fail(WriteProblem::Unreadable(e.kind()))),
        };
        ConfigFile::parse(path, shown, &text)
            .map_err(|e| fail(WriteProblem::Broken(Box::new(file_error(e, path, shown)))))?;
        let doc = DocumentMut::from_str(&text).map_err(|_| {
            fail(WriteProblem::Broken(Box::new(FileError {
                path: path.to_owned(),
                shown,
                line: None,
                problem: Problem::Syntax,
            })))
        })?;
        let leading = text.len() - text.trim_start_matches('\n').len();
        Ok(Self {
            path: path.to_owned(),
            shown,
            doc,
            existed,
            leading,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn shown(&self) -> PathShown {
        self.shown
    }

    pub fn existed(&self) -> bool {
        self.existed
    }

    pub fn text(&self) -> String {
        let text = self.doc.to_string();
        let trimmed = text.trim_start_matches('\n');
        let kept = self.leading.min(text.len() - trimmed.len());
        format!("{}{trimmed}", "\n".repeat(kept))
    }

    pub fn config(&self) -> Result<ConfigFile, EndpointError> {
        ConfigFile::parse(&self.path, self.shown, &self.text())
    }

    pub fn current(&self) -> Result<ConfigFile, ConfigWriteError> {
        self.config().map_err(|e| ConfigWriteError {
            path: self.path.clone(),
            shown: self.shown,
            problem: WriteProblem::Broken(Box::new(file_error(e, &self.path, self.shown))),
        })
    }

    fn take_leading(&mut self) -> String {
        if !self.doc.as_table().is_empty() {
            return String::new();
        }
        let trailing = self.doc.trailing().as_str().unwrap_or_default().to_owned();
        self.doc.set_trailing("");
        trailing
    }

    fn set_top(&mut self, key: &str, text: &str) {
        if let Some(value) = self
            .doc
            .as_table_mut()
            .get_mut(key)
            .and_then(Item::as_value_mut)
        {
            let kept = value.clone();
            *value = string_value(text, Some(&kept));
            return;
        }
        let leading = self.take_leading();
        let root = self.doc.as_table_mut();
        root.insert(key, Item::Value(string_value(text, None)));
        if !leading.is_empty()
            && let Some(mut key) = root.key_mut(key)
        {
            key.leaf_decor_mut().set_prefix(leading);
        }
    }

    pub fn set_store_id(&mut self, id: &StoreId) {
        self.set_top("store_id", id.as_str());
    }

    fn names_table(&self, name: &str) -> bool {
        self.doc
            .get("endpoints")
            .and_then(|endpoints| endpoints.get(name))
            .is_some()
    }

    pub fn set_default(&mut self, name: Option<&EndpointName>) {
        match name {
            Some(name) if keys_file_table(name.as_str(), |n| self.names_table(n)) => {
                self.set_top("default", ENV);
            }
            Some(name) => self.set_top("default", name.as_str()),
            None => {
                let root = self.doc.as_table_mut();
                let prefix = root
                    .key("default")
                    .and_then(|key| key.leaf_decor().prefix())
                    .and_then(|raw| raw.as_str())
                    .map(str::to_owned)
                    .unwrap_or_default();
                if root.remove("default").is_none() || prefix.trim().is_empty() {
                    return;
                }
                let next_value =
                    root.iter_mut()
                        .find(|(_, item)| item.is_value())
                        .map(|(mut key, _)| {
                            let own = key
                                .leaf_decor()
                                .prefix()
                                .and_then(|raw| raw.as_str())
                                .unwrap_or_default()
                                .to_owned();
                            key.leaf_decor_mut().set_prefix(format!("{prefix}{own}"));
                        });
                if next_value.is_none() {
                    let current = self.doc.trailing().as_str().unwrap_or_default().to_owned();
                    let start = prefix.trim_end().to_owned();
                    self.doc.set_trailing(format!("{start}\n{current}"));
                }
            }
        }
    }

    pub fn add_endpoint(&mut self, name: &EndpointName, endpoint: &NewEndpoint) {
        let fields: Vec<(&str, String)> = endpoint_fields(endpoint)
            .into_iter()
            .map(|(key, text)| (key, text.to_owned()))
            .collect();
        self.add_fields(name, &fields);
    }

    pub fn add_table(&mut self, name: &EndpointName, table: &NewTable) {
        self.add_fields(name, &table_fields(table));
    }

    fn add_fields(&mut self, name: &EndpointName, fields: &[(&str, String)]) {
        let leading = self.take_leading();
        let blank_line = !self.text().trim().is_empty() || !leading.is_empty();
        let root = self.doc.as_table_mut();
        if !root.contains_key("endpoints") {
            let mut table = Table::new();
            table.set_implicit(true);
            root.insert("endpoints", Item::Table(table));
        }
        let Some(endpoints) = root.get_mut("endpoints") else {
            return;
        };
        if let Some(inline) = endpoints.as_inline_table_mut() {
            let mut entry = InlineTable::new();
            for (key, text) in fields {
                entry.insert(*key, Value::from(text.as_str()));
            }
            inline.insert(name.as_str(), Value::InlineTable(entry));
            return;
        }
        if let Some(table) = endpoints.as_table_mut() {
            let mut entry = Table::new();
            for (key, text) in fields {
                entry.insert(key, Item::Value(Value::from(text.as_str())));
            }
            if blank_line {
                entry.decor_mut().set_prefix(format!("{leading}\n"));
            }
            table.insert(name.as_str(), Item::Table(entry));
        }
    }

    pub fn set_mcp_server(&mut self, name: &str, server: &crate::config::McpServer) -> bool {
        let parents = vec!["mcp".to_owned(), "servers".to_owned(), name.to_owned()];
        let had_text = !self.text().trim().is_empty();
        let Some((table, _)) = ensure_table(self.doc.as_table_mut(), &parents, had_text) else {
            return false;
        };
        table.insert("command", toml_edit::value(&server.command));
        if server.args.is_empty() {
            table.remove("args");
        } else {
            table.insert(
                "args",
                toml_edit::value(server.args.iter().collect::<Array>()),
            );
        }
        if server.keys.is_empty() {
            table.remove("keys");
        } else {
            table.insert(
                "keys",
                toml_edit::value(server.keys.iter().collect::<Array>()),
            );
        }
        true
    }

    pub fn add_mcp_keys(&mut self, name: &str, keys: &[String]) -> bool {
        let Some(table) = self
            .doc
            .get_mut("mcp")
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(|servers| servers.get_mut(name))
            .and_then(Item::as_table_like_mut)
        else {
            return false;
        };
        if table.get("keys").is_none() {
            table.insert("keys", toml_edit::value(Array::new()));
        }
        let Some(names) = table.get_mut("keys").and_then(Item::as_array_mut) else {
            return false;
        };
        for key in keys {
            if !names
                .iter()
                .any(|value| value.as_str() == Some(key.as_str()))
            {
                names.push(key.as_str());
            }
        }
        true
    }

    pub fn remove_mcp_server(&mut self, name: &str) -> bool {
        self.doc
            .get_mut("mcp")
            .and_then(Item::as_table_like_mut)
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(Item::as_table_like_mut)
            .is_some_and(|servers| servers.remove(name).is_some())
    }

    pub fn remove_endpoint(&mut self, name: &EndpointName) -> bool {
        self.doc
            .as_table_mut()
            .get_mut("endpoints")
            .and_then(Item::as_table_like_mut)
            .and_then(|t| t.remove(name.as_str()))
            .is_some()
    }

    fn endpoint_mut(&mut self, name: &EndpointName) -> Option<&mut dyn TableLike> {
        self.doc
            .as_table_mut()
            .get_mut("endpoints")
            .and_then(Item::as_table_like_mut)
            .and_then(|t| t.get_mut(name.as_str()))
            .and_then(Item::as_table_like_mut)
    }

    pub fn set_field(&mut self, name: &EndpointName, key: &str, value: Option<&str>) -> bool {
        let Some(entry) = self.endpoint_mut(name) else {
            return false;
        };
        match value {
            None => {
                entry.remove(key);
            }
            Some(text) => match entry.get_mut(key).and_then(Item::as_value_mut) {
                Some(old) => {
                    let kept = old.clone();
                    *old = string_value(text, Some(&kept));
                }
                None => {
                    entry.insert(key, Item::Value(Value::from(text)));
                }
            },
        }
        true
    }

    pub fn replace_fields(
        &mut self,
        name: &EndpointName,
        gone: &[&str],
        key: &str,
        value: &str,
    ) -> bool {
        let Some(endpoints) = self
            .doc
            .as_table_mut()
            .get_mut("endpoints")
            .and_then(Item::as_table_like_mut)
        else {
            return false;
        };
        let Some(item) = endpoints.get_mut(name.as_str()) else {
            return false;
        };
        let found = |names: Vec<String>| {
            names
                .into_iter()
                .filter(|k| k == key || gone.contains(&k.as_str()))
                .count()
        };
        match item {
            Item::Table(table) => {
                let names: Vec<String> = table.iter().map(|(k, _)| k.to_owned()).collect();
                if found(names.clone()) == 0 {
                    return self.set_field(name, key, Some(value));
                }
                let entries: Vec<(toml_edit::Key, Item)> =
                    names.iter().filter_map(|k| table.remove_entry(k)).collect();
                for (k, item) in swap_entries(entries, gone, key, value) {
                    table.insert_formatted(&k, item);
                }
                true
            }
            Item::Value(Value::InlineTable(table)) => {
                let names: Vec<String> = table.iter().map(|(k, _)| k.to_owned()).collect();
                if found(names.clone()) == 0 {
                    return self.set_field(name, key, Some(value));
                }
                let entries: Vec<(toml_edit::Key, Item)> = names
                    .iter()
                    .filter_map(|k| table.remove_entry(k))
                    .map(|(k, v)| (k, Item::Value(v)))
                    .collect();
                for (k, item) in swap_entries(entries, gone, key, value) {
                    if let Item::Value(v) = item {
                        table.insert_formatted(&k, v);
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub fn rename_endpoint(&mut self, old: &EndpointName, new: &EndpointName) -> bool {
        let Some(endpoints) = self
            .doc
            .as_table_mut()
            .get_mut("endpoints")
            .and_then(Item::as_table_like_mut)
        else {
            return false;
        };
        let Some(item) = endpoints.remove(old.as_str()) else {
            return false;
        };
        endpoints.insert(new.as_str(), item);
        true
    }

    pub fn write(&self) -> Result<Vec<PathNote>, ConfigWriteError> {
        let fail = |problem| ConfigWriteError {
            path: self.path.clone(),
            shown: self.shown,
            problem,
        };
        let text = self.text();
        ConfigFile::parse(&self.path, self.shown, &text).map_err(|e| {
            fail(WriteProblem::Invalid(Box::new(file_error(
                e, &self.path, self.shown,
            ))))
        })?;
        let (target, linked) =
            target_of(&self.path).map_err(|e| fail(WriteProblem::Unreadable(e.kind())))?;
        let (mode, links) = match std::fs::metadata(&target) {
            Ok(meta) => (file_mode(&meta), link_count(&meta)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (NEW_MODE, 1),
            Err(e) => return Err(fail(WriteProblem::Unreadable(e.kind()))),
        };
        if let Some(folder) = target.parent().filter(|p| !p.as_os_str().is_empty())
            && !folder.exists()
        {
            create_folder(folder).map_err(|e| fail(WriteProblem::Write(e.kind())))?;
        }
        write_atomic(&target, text.as_bytes(), mode).map_err(|e| {
            let unwritable = matches!(
                e.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            );
            if linked && unwritable {
                fail(WriteProblem::TargetNotWritable(Box::new(PathNote {
                    kind: PathNoteKind::TargetNotWritable,
                    path: target.clone(),
                    repo: None,
                    shown: self.shown,
                })))
            } else {
                fail(WriteProblem::Write(e.kind()))
            }
        })?;
        let mut notes = Vec::new();
        if links > 1 {
            notes.push(PathNote {
                kind: PathNoteKind::HardLinks,
                path: target,
                repo: None,
                shown: self.shown,
            });
        }
        Ok(notes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiteEdit {
    Set(SitePath, SettingValue),
    Unset(SitePath),
}

impl SiteEdit {
    pub fn path(&self) -> &SitePath {
        match self {
            Self::Set(path, _) | Self::Unset(path) => path,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditRefused {
    NotWritable(SitePath),
    NotATable(SitePath),
    KeyShapedModel,
    NotAModelId,
}

fn place_model(place: &Place) -> Option<&str> {
    match place {
        Place::Model(model) | Place::EndpointModel { model, .. } => Some(model),
        Place::Global
        | Place::Endpoint(_)
        | Place::Agent(_)
        | Place::PluginHook { .. }
        | Place::PluginSlot { .. }
        | Place::Plugin(_) => None,
    }
}

fn model_table(place: &Place) -> Vec<&str> {
    match place {
        Place::Model(model) => vec!["models", model],
        Place::EndpointModel { endpoint, model } => vec!["endpoints", endpoint, "models", model],
        Place::Global
        | Place::Endpoint(_)
        | Place::Agent(_)
        | Place::PluginHook { .. }
        | Place::PluginSlot { .. }
        | Place::Plugin(_) => Vec::new(),
    }
}

fn key_shaped_value(edit: &SiteEdit) -> bool {
    matches!(edit, SiteEdit::Set(path, value) if setting_looks_like_key(path.spec().kind, value))
}

fn toml_value(value: &SettingValue) -> Value {
    match value {
        SettingValue::Bool(on) => Value::from(*on),
        SettingValue::Int(n) => Value::from(*n),
        SettingValue::Tokens(n) => match i64::try_from(*n) {
            Ok(n) => Value::from(n),
            Err(_) => Value::from(n.to_string()),
        },
        SettingValue::Usd { micros } => {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a dollar amount written to config.toml, read back exactly by its text"
            )]
            let dollars = *micros as f64 / 1e6;
            Value::from(dollars)
        }
        SettingValue::List(items) => {
            let mut array = Array::new();
            for item in items {
                array.push(item.as_str());
            }
            Value::Array(array)
        }
        SettingValue::Percent(_) | SettingValue::Text(_) | SettingValue::Other { .. } => {
            Value::from(value.text())
        }
    }
}

fn ensure_table<'a>(
    mut current: &'a mut dyn TableLike,
    parents: &[String],
    had_text: bool,
) -> Option<(&'a mut dyn TableLike, bool)> {
    let mut inline = false;
    for (i, segment) in parents.iter().enumerate() {
        if !current.contains_key(segment) {
            let item = if inline {
                let mut table = InlineTable::new();
                let suffix = make_room(current);
                table.decor_mut().set_prefix(" ");
                table.decor_mut().set_suffix(suffix);
                Item::Value(Value::InlineTable(table))
            } else {
                let mut table = Table::new();
                let holds_values = i + 1 == parents.len();
                table.set_implicit(!holds_values);
                if holds_values && had_text {
                    table.decor_mut().set_prefix(
                        "
",
                    );
                }
                Item::Table(table)
            };
            current.insert(segment, item);
        }
        let item = current.get_mut(segment)?;
        if i + 1 == parents.len()
            && let Some(table) = item.as_table_mut()
            && table.is_implicit()
        {
            give_header(table);
        }
        inline |= item.is_inline_table();
        current = item.as_table_like_mut()?;
    }
    Some((current, inline))
}

fn table_at<'a>(table: &'a mut dyn TableLike, path: &[String]) -> Option<&'a mut dyn TableLike> {
    match path.split_first() {
        None => Some(table),
        Some((first, rest)) => table_at(table.get_mut(first)?.as_table_like_mut()?, rest),
    }
}

fn first_header(table: &toml_edit::Table) -> Option<isize> {
    table
        .iter()
        .filter_map(|(_, item)| item.as_table())
        .filter_map(|child| {
            let own = (!child.is_implicit()).then(|| child.position()).flatten();
            match (own, first_header(child)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        })
        .min()
}

fn header_at(table: &mut toml_edit::Table, at: isize) -> Option<&mut toml_edit::Table> {
    for (_, item) in table.iter_mut() {
        let Some(child) = item.as_table_mut() else {
            continue;
        };
        if !child.is_implicit() && child.position() == Some(at) {
            return Some(child);
        }
        if let Some(found) = header_at(child, at) {
            return Some(found);
        }
    }
    None
}

fn prefix_of_table(table: &toml_edit::Table) -> String {
    table
        .decor()
        .prefix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn give_header(table: &mut toml_edit::Table) {
    table.set_implicit(false);
    let Some(at) = first_header(table) else {
        return;
    };
    let Some(child) = header_at(table, at) else {
        return;
    };
    let own = prefix_of_table(child);
    let rest = own.trim_start_matches('\n');
    let blank = "\n".repeat(own.len() - rest.len());
    child.decor_mut().set_prefix(format!("\n{rest}"));
    table.decor_mut().set_prefix(blank);
    table.set_position(Some(at));
}

fn drop_header(table: &mut toml_edit::Table) {
    let holds_values = table.iter().any(|(_, item)| !item.is_table());
    if table.is_implicit() || holds_values || table.is_empty() {
        return;
    }
    let blank = prefix_of_table(table);
    table.set_implicit(true);
    table.decor_mut().set_prefix("");
    let Some(at) = first_header(table) else {
        return;
    };
    let Some(child) = header_at(table, at) else {
        return;
    };
    let own = prefix_of_table(child);
    let rest = own.strip_prefix('\n').unwrap_or(&own).to_owned();
    child.decor_mut().set_prefix(format!("{blank}{rest}"));
}

fn has_comment(text: Option<&toml_edit::RawString>) -> bool {
    text.and_then(toml_edit::RawString::as_str)
        .is_some_and(|t| t.contains('#'))
}

fn commented(item: &Item) -> bool {
    match item {
        Item::Table(table) => {
            has_comment(table.decor().prefix()) || has_comment(table.decor().suffix())
        }
        Item::Value(value) => {
            has_comment(value.decor().prefix()) || has_comment(value.decor().suffix())
        }
        _ => false,
    }
}

fn empty(item: &Item) -> bool {
    item.as_table_like().is_some_and(TableLike::is_empty)
}

fn inline_at(table: &dyn TableLike, path: &[String]) -> bool {
    match path.split_first() {
        None => false,
        Some((first, rest)) => match table.get(first) {
            Some(item) if item.is_inline_table() => true,
            Some(item) => item
                .as_table_like()
                .is_some_and(|child| inline_at(child, rest)),
            None => false,
        },
    }
}

fn remove_key(table: &mut dyn TableLike, key: &str, inline: bool) -> Option<String> {
    let comment = table
        .key(key)
        .and_then(|k| k.leaf_decor().prefix())
        .and_then(toml_edit::RawString::as_str)
        .filter(|p| p.contains('#'))
        .map(str::to_owned);
    let was_last = table.iter().last().is_some_and(|(k, _)| k == key);
    let closing = table
        .get(key)
        .and_then(Item::as_value)
        .and_then(|v| v.decor().suffix())
        .and_then(toml_edit::RawString::as_str)
        .filter(|s| !s.is_empty() && s.trim().is_empty())
        .map(str::to_owned);
    table.remove(key)?;
    if inline
        && was_last
        && let Some(closing) = closing
        && let Some(last) = table
            .iter_mut()
            .last()
            .and_then(|(_, item)| item.as_value_mut())
        && !last
            .decor()
            .suffix()
            .and_then(toml_edit::RawString::as_str)
            .is_some_and(|s| s.contains('#'))
    {
        last.decor_mut().set_suffix(closing);
    }
    place_comment(table, comment?)
}

fn make_room(table: &mut dyn TableLike) -> String {
    let Some(last) = table
        .iter_mut()
        .last()
        .and_then(|(_, item)| item.as_value_mut())
    else {
        return " ".to_owned();
    };
    let suffix = last
        .decor()
        .suffix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or(" ")
        .to_owned();
    last.decor_mut().set_suffix("");
    suffix
}

fn place_comment(table: &mut dyn TableLike, comment: String) -> Option<String> {
    let moved = table
        .iter_mut()
        .find(|(_, item)| item.is_value())
        .map(|(mut next, _)| {
            let own = next
                .leaf_decor()
                .prefix()
                .and_then(toml_edit::RawString::as_str)
                .unwrap_or_default()
                .to_owned();
            next.leaf_decor_mut().set_prefix(format!("{comment}{own}"));
        });
    match moved {
        Some(()) => None,
        None => Some(comment),
    }
}

impl ConfigText {
    fn has_table(&self, keys: &[&str]) -> bool {
        let mut current: &dyn TableLike = self.doc.as_table();
        for key in keys {
            match current.get(key).and_then(Item::as_table_like) {
                Some(table) => current = table,
                None => return false,
            }
        }
        !keys.is_empty()
    }

    fn refused(&self, edit: &SiteEdit) -> Option<EditRefused> {
        if key_shaped_value(edit) {
            return Some(EditRefused::KeyShapedModel);
        }
        let place = &edit.path().place;
        let model = place_model(place)?;
        if check_model_id(model).is_err() {
            return Some(EditRefused::NotAModelId);
        }
        let creates = matches!(edit, SiteEdit::Set(..)) && !self.has_table(&model_table(place));
        (model_looks_like_key(model) && creates).then_some(EditRefused::KeyShapedModel)
    }

    pub fn apply(&mut self, edits: &[SiteEdit]) -> Result<(), EditRefused> {
        if let Some(refused) = edits.iter().find_map(|edit| self.refused(edit)) {
            return Err(refused);
        }
        for edit in edits {
            let path = edit.path();
            let spec = path.spec();
            if spec.secrecy != Secrecy::Plain {
                return Err(EditRefused::NotWritable(path.clone()));
            }
            if spec.name == names::ENDPOINT && path.place == Place::Global {
                match edit {
                    SiteEdit::Set(_, value) => {
                        let name = EndpointName::parse(&value.text())
                            .ok_or_else(|| EditRefused::NotWritable(path.clone()))?;
                        self.set_default(Some(&name));
                    }
                    SiteEdit::Unset(_) => self.set_default(None),
                }
                continue;
            }
            let table = crate::config::table_path(path, |n| self.names_table(n));
            let written = table.as_ref().unwrap_or(path);
            match edit {
                SiteEdit::Set(_, value) => self.set_path(written, value)?,
                SiteEdit::Unset(_) => self.unset_path(written),
            }
            if let Place::PluginSlot { plugin, section } = &path.place {
                for former in wisp_protocol::settings::former_slots(plugin, section) {
                    let place = Place::PluginSlot {
                        plugin: plugin.clone(),
                        section: former.to_owned(),
                    };
                    self.unset_path(&SitePath::new(place, spec));
                }
            }
        }
        Ok(())
    }

    fn set_path(&mut self, path: &SitePath, value: &SettingValue) -> Result<(), EditRefused> {
        let segments = path.segments();
        let Some((leaf, parents)) = segments.split_last() else {
            return Err(EditRefused::NotATable(path.clone()));
        };
        let had_text = !self.text().trim().is_empty();
        let leading = if parents.is_empty() {
            self.take_leading()
        } else {
            String::new()
        };
        let (current, inline) = ensure_table(self.doc.as_table_mut(), parents, had_text)
            .ok_or_else(|| EditRefused::NotATable(path.clone()))?;
        let mut new = toml_value(value);
        match current.get_mut(leaf).and_then(Item::as_value_mut) {
            Some(old) => {
                *new.decor_mut() = old.decor().clone();
                *old = new;
            }
            None => {
                if inline {
                    let suffix = make_room(current);
                    new.decor_mut().set_suffix(suffix);
                    new.decor_mut().set_prefix(" ");
                }
                current.insert(leaf, Item::Value(new));
                if !leading.is_empty()
                    && let Some(mut key) = current.key_mut(leaf)
                {
                    key.leaf_decor_mut().set_prefix(leading);
                }
            }
        }
        Ok(())
    }

    fn unset_path(&mut self, path: &SitePath) {
        let segments = path.segments();
        let Some((leaf, parents)) = segments.split_last() else {
            return;
        };
        let parents_inline = inline_at(self.doc.as_table(), parents);
        let mut orphan = table_at(self.doc.as_table_mut(), parents)
            .and_then(|table| remove_key(table, leaf, parents_inline));
        if let Some((last, owner)) = parents.split_last()
            && let Some(Item::Table(table)) =
                table_at(self.doc.as_table_mut(), owner).and_then(|t| t.get_mut(last))
        {
            drop_header(table);
        }
        let mut level = parents.len();
        while level > 0 {
            let (owner, child) = (&parents[..level - 1], &parents[level - 1]);
            let inline = inline_at(self.doc.as_table(), owner);
            let Some(table) = table_at(self.doc.as_table_mut(), owner) else {
                break;
            };
            let removable = table
                .get(child)
                .is_some_and(|item| empty(item) && !commented(item));
            if !removable {
                break;
            }
            let above = remove_key(table, child, inline);
            orphan = match (orphan, above) {
                (Some(below), Some(above)) => Some(format!("{above}{below}")),
                (below, above) => below.or(above),
            };
            orphan = orphan.and_then(|comment| place_comment(table, comment));
            level -= 1;
        }
        if let Some(comment) = orphan {
            self.keep_comment(&parents[..level], &comment);
        }
    }

    fn keep_comment(&mut self, parents: &[String], comment: &str) {
        if parents.is_empty() {
            let first = first_header(self.doc.as_table());
            if let Some(table) = first.and_then(|at| header_at(self.doc.as_table_mut(), at)) {
                let own = table
                    .decor()
                    .prefix()
                    .and_then(toml_edit::RawString::as_str)
                    .unwrap_or_default()
                    .to_owned();
                table.decor_mut().set_prefix(format!("{comment}{own}"));
                return;
            }
            let current = self.doc.trailing().as_str().unwrap_or_default().to_owned();
            let start = comment.trim_end().to_owned();
            self.doc.set_trailing(format!("{start}\n{current}"));
            return;
        }
        let Some((last, owner)) = parents.split_last() else {
            return;
        };
        let Some(current) = table_at(self.doc.as_table_mut(), owner) else {
            return;
        };
        if let Some(Item::Table(table)) = current.get_mut(last) {
            let own = table
                .decor()
                .suffix()
                .and_then(toml_edit::RawString::as_str)
                .unwrap_or_default()
                .to_owned();
            table
                .decor_mut()
                .set_suffix(format!("{own}\n{}", comment.trim_end()));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveOutcome {
    pub notes: Vec<PathNote>,
    pub created: bool,
}

pub const NOT_A_MODEL_ID_PLACE: &str = concat!(
    wisp_protocol::config_file!(),
    " wasn't changed: that path's model part isn't a model ID"
);

pub const KEY_SHAPED_MODEL: &str = concat!(
    wisp_protocol::config_file!(),
    " wasn't changed: that model ID looks like a key, so it isn't saved"
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveError {
    Write(ConfigWriteError),
    Refused(EditRefused),
    NoFolder,
    Busy(Vec<PathNote>),
    Lock(io::ErrorKind, Vec<PathNote>),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(e) => write!(f, "{e}"),
            Self::Refused(EditRefused::NotWritable(path)) => {
                write!(
                    f,
                    "{CONFIG_FILE} wasn't changed: {} isn't written by a setting",
                    path.shown()
                )
            }
            Self::Refused(EditRefused::NotATable(path)) => write!(
                f,
                "{CONFIG_FILE} wasn't changed: a key on the way to {} isn't a table",
                path.shown()
            ),
            Self::Refused(EditRefused::KeyShapedModel) => f.write_str(KEY_SHAPED_MODEL),
            Self::Refused(EditRefused::NotAModelId) => f.write_str(NOT_A_MODEL_ID_PLACE),
            Self::NoFolder => write!(f, "the folder of {GIVEN_CONFIG} doesn't exist"),
            Self::Busy(_) => write!(
                f,
                "{CONFIG_FILE} wasn't changed: another wisp is changing it; try again."
            ),
            Self::Lock(kind, _) => write!(
                f,
                "{CONFIG_FILE} wasn't changed: its lock couldn't be taken ({kind})."
            ),
        }
    }
}

impl std::error::Error for SaveError {}

pub fn save(
    path: &Path,
    shown: PathShown,
    edits: &[SiteEdit],
    wait: Duration,
) -> Result<SaveOutcome, SaveError> {
    unwritable_target(path, shown).map_err(SaveError::Write)?;
    if shown == PathShown::Given
        && path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .is_some_and(|folder| !folder.is_dir())
    {
        return Err(SaveError::NoFolder);
    }
    let folder = lock_folder(path);
    let locks = lock_noting(&[(folder.as_path(), LockKind::Config)], shown, wait).map_err(
        |(error, notes)| match error {
            StoreError::Busy => SaveError::Busy(notes),
            StoreError::Lock(kind) => SaveError::Lock(kind, notes),
            _ => SaveError::Lock(io::ErrorKind::Other, notes),
        },
    )?;
    let mut text = ConfigText::read(path, shown).map_err(SaveError::Write)?;
    text.apply(edits).map_err(SaveError::Refused)?;
    let mut notes = locks.notes().to_vec();
    notes.extend(text.write().map_err(SaveError::Write)?);
    drop(locks);
    Ok(SaveOutcome {
        notes,
        created: !text.existed(),
    })
}

#[cfg(unix)]
fn folder_writable(folder: &Path) -> bool {
    match rustix::fs::access(folder, rustix::fs::Access::WRITE_OK) {
        Ok(()) => true,
        Err(e) => e != rustix::io::Errno::ACCESS && e != rustix::io::Errno::ROFS,
    }
}

#[cfg(not(unix))]
fn folder_writable(_folder: &Path) -> bool {
    true
}

pub(crate) fn unwritable_target(
    config_path: &Path,
    shown: PathShown,
) -> Result<(), ConfigWriteError> {
    let Ok((target, true)) = target_of(config_path) else {
        return Ok(());
    };
    if folder_writable(&lock_folder(config_path)) {
        return Ok(());
    }
    Err(ConfigWriteError {
        path: config_path.to_owned(),
        shown,
        problem: WriteProblem::TargetNotWritable(Box::new(PathNote {
            kind: PathNoteKind::TargetNotWritable,
            path: target,
            repo: None,
            shown,
        })),
    })
}

fn file_error(error: EndpointError, path: &Path, shown: PathShown) -> FileError {
    match error {
        EndpointError::File(e) => e,
        _ => FileError {
            path: path.to_owned(),
            shown,
            line: None,
            problem: Problem::Syntax,
        },
    }
}

fn endpoint_fields(endpoint: &NewEndpoint) -> Vec<(&'static str, &'static str)> {
    let mut fields = Vec::new();
    if endpoint.provider != ProviderKind::default() {
        fields.push(("provider", endpoint.provider.as_str()));
    }
    fields.push(("auth", endpoint.auth.as_str()));
    fields.push(("key_store", endpoint.store.as_str()));
    if endpoint.pricing == PricingMode::None {
        fields.push(("pricing", PricingMode::None.as_str()));
    }
    fields
}

fn table_fields(table: &NewTable) -> Vec<(&'static str, String)> {
    let mut fields = Vec::new();
    if table.provider != ProviderKind::default() {
        fields.push(("provider", table.provider.as_str().to_owned()));
    }
    fields.push(("auth", table.auth.as_str().to_owned()));
    fields.push(("key_env", table.key_env.raw().to_owned()));
    if let Some(url) = &table.url {
        fields.push(("url", url.clone()));
    }
    if let Some(var) = &table.url_env {
        fields.push(("url_env", var.raw().to_owned()));
    }
    if table.pricing == PricingMode::None {
        fields.push(("pricing", PricingMode::None.as_str().to_owned()));
    }
    fields
}

#[cfg(unix)]
pub fn file_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
pub fn file_mode(_meta: &std::fs::Metadata) -> u32 {
    NEW_MODE
}

#[cfg(unix)]
pub(crate) fn link_count(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.nlink()
}

#[cfg(not(unix))]
pub(crate) fn link_count(_meta: &std::fs::Metadata) -> u64 {
    1
}

#[cfg(test)]
mod tests;
