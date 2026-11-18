use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::Deserializer;
use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, Visitor};
use serde_json::error::Category;
use wisp_protocol::settings::A_STRING;
use wisp_protocol::{Secret, account_name_error};

use super::keyring::ItemProblem;
use super::{Every, FILE_VERSION, StoreError, StoreId, Stored, StoredRecord};
use crate::endpoints::EndpointName;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordProblem {
    NotAnObject,
    UnknownTopField,
    NoVersion,
    BadVersion,
    NoStores,
    StoresNotObject,
    BadStoreId,
    StoreNotObject,
    BadName,
    RecordNotObject,
    UnknownRecordField,
    NoKey,
    KeyNotString,
    OwnerNotString,
    UrlNotString,
    Repeated,
}

impl fmt::Display for RecordProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotAnObject => "the file must be a JSON object with version and stores",
            Self::UnknownTopField => "unknown field; allowed: version, stores",
            Self::NoVersion => "the file needs a version",
            Self::BadVersion => "version must be a whole number from 1",
            Self::NoStores => "the file needs stores",
            Self::StoresNotObject => "stores must be an object of store IDs",
            Self::BadStoreId => return write!(f, "a store ID must be {}", StoreId::RULE),
            Self::StoreNotObject => "a store must be an object of account records",
            Self::BadName => return f.write_str(&account_name_error()),
            Self::RecordNotObject => "a record must be an object with key, owner and url",
            Self::UnknownRecordField => "unknown field in a record; allowed: key, owner, url",
            Self::NoKey => return fmt::Display::fmt(&ItemProblem::NoKey, f),
            Self::KeyNotString => return fmt::Display::fmt(&ItemProblem::KeyNotString, f),
            Self::OwnerNotString => return fmt::Display::fmt(&ItemProblem::OwnerNotString, f),
            Self::UrlNotString => return fmt::Display::fmt(&ItemProblem::UrlNotString, f),
            Self::Repeated => "a field, store ID or account name appears twice",
        })
    }
}

pub(super) enum Parsed {
    Records(Every),
    Newer,
}

type Problems = Cell<Option<RecordProblem>>;

fn refuse<T, E: de::Error>(cell: &Problems, problem: RecordProblem) -> Result<T, E> {
    if cell.get().is_none() {
        cell.set(Some(problem));
    }
    Err(E::custom("refused"))
}

macro_rules! refuse_scalars {
    () => {
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_i128<E: de::Error>(self, _: i128) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_u128<E: de::Error>(self, _: u128) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_char<E: de::Error>(self, _: char) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
        fn visit_some<D: Deserializer<'de>>(self, _: D) -> Result<Self::Value, D::Error> {
            refuse(self.cell, self.problem)
        }
        fn visit_newtype_struct<D: Deserializer<'de>>(self, _: D) -> Result<Self::Value, D::Error> {
            refuse(self.cell, self.problem)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
            refuse(self.cell, self.problem)
        }
        fn visit_enum<A: EnumAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
            refuse(self.cell, self.problem)
        }
    };
}

macro_rules! refuse_u64 {
    () => {
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
    };
}

macro_rules! refuse_str {
    () => {
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            refuse(self.cell, self.problem)
        }
    };
}

macro_rules! refuse_map {
    () => {
        fn visit_map<A: MapAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
            refuse(self.cell, self.problem)
        }
    };
}

pub(super) fn parse(text: &str) -> Result<Parsed, StoreError> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text)
        && value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|v| v > FILE_VERSION)
    {
        return Ok(Parsed::Newer);
    }
    let cell = Problems::new(None);
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let parsed = Top {
        cell: &cell,
        problem: RecordProblem::NotAnObject,
    }
    .deserialize(&mut deserializer)
    .and_then(|every| deserializer.end().map(|()| every));
    match parsed {
        Ok(every) => Ok(Parsed::Records(every)),
        Err(e) => Err(match (e.classify(), cell.get()) {
            (Category::Data, Some(problem)) => StoreError::Record {
                line: e.line(),
                problem,
            },
            _ => StoreError::Syntax { line: e.line() },
        }),
    }
}

struct Top<'a> {
    cell: &'a Problems,
    problem: RecordProblem,
}

impl<'de> DeserializeSeed<'de> for Top<'_> {
    type Value = Every;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Every, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Top<'_> {
    type Value = Every;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a credentials file")
    }

    refuse_scalars!();
    refuse_u64!();
    refuse_str!();

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Every, A::Error> {
        let mut version = None;
        let mut stores = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "version" if version.is_none() => {
                    version = Some(map.next_value_seed(Version {
                        cell: self.cell,
                        problem: RecordProblem::BadVersion,
                    })?);
                }
                "stores" if stores.is_none() => {
                    stores = Some(map.next_value_seed(Stores {
                        cell: self.cell,
                        problem: RecordProblem::StoresNotObject,
                    })?);
                }
                "version" | "stores" => return refuse(self.cell, RecordProblem::Repeated),
                _ => return refuse(self.cell, RecordProblem::UnknownTopField),
            }
        }
        match version {
            None => return refuse(self.cell, RecordProblem::NoVersion),
            Some(FILE_VERSION) => {}
            Some(_) => return refuse(self.cell, RecordProblem::BadVersion),
        }
        match stores {
            Some(stores) => Ok(stores),
            None => refuse(self.cell, RecordProblem::NoStores),
        }
    }
}

struct Version<'a> {
    cell: &'a Problems,
    problem: RecordProblem,
}

impl<'de> DeserializeSeed<'de> for Version<'_> {
    type Value = u64;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<u64, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Version<'_> {
    type Value = u64;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a version")
    }

    refuse_scalars!();
    refuse_str!();
    refuse_map!();

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> {
        Ok(v)
    }
}

struct Stores<'a> {
    cell: &'a Problems,
    problem: RecordProblem,
}

impl<'de> DeserializeSeed<'de> for Stores<'_> {
    type Value = Every;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Every, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Stores<'_> {
    type Value = Every;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stores")
    }

    refuse_scalars!();
    refuse_u64!();
    refuse_str!();

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Every, A::Error> {
        let mut every = BTreeMap::new();
        while let Some(id) = map.next_key_seed(Id {
            cell: self.cell,
            problem: RecordProblem::BadStoreId,
        })? {
            if every.contains_key(&id) {
                return refuse(self.cell, RecordProblem::Repeated);
            }
            let store = map.next_value_seed(Store {
                cell: self.cell,
                problem: RecordProblem::StoreNotObject,
            })?;
            every.insert(id, store);
        }
        Ok(every)
    }
}

struct Id<'a> {
    cell: &'a Problems,
    problem: RecordProblem,
}

impl<'de> DeserializeSeed<'de> for Id<'_> {
    type Value = StoreId;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<StoreId, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Id<'_> {
    type Value = StoreId;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a store ID")
    }

    refuse_scalars!();
    refuse_u64!();
    refuse_map!();

    fn visit_str<E: de::Error>(self, v: &str) -> Result<StoreId, E> {
        match StoreId::parse(v) {
            Some(id) => Ok(id),
            None => refuse(self.cell, RecordProblem::BadStoreId),
        }
    }
}

struct Store<'a> {
    cell: &'a Problems,
    problem: RecordProblem,
}

impl<'de> DeserializeSeed<'de> for Store<'_> {
    type Value = BTreeMap<EndpointName, Stored>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Store<'_> {
    type Value = BTreeMap<EndpointName, Stored>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a store")
    }

    refuse_scalars!();
    refuse_u64!();
    refuse_str!();

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut store = BTreeMap::new();
        while let Some(name) = map.next_key_seed(Name {
            cell: self.cell,
            problem: RecordProblem::BadName,
        })? {
            if store.contains_key(&name) {
                return refuse(self.cell, RecordProblem::Repeated);
            }
            let record = map.next_value_seed(Record {
                cell: self.cell,
                problem: RecordProblem::RecordNotObject,
            })?;
            store.insert(name, record);
        }
        Ok(store)
    }
}

struct Name<'a> {
    cell: &'a Problems,
    problem: RecordProblem,
}

impl<'de> DeserializeSeed<'de> for Name<'_> {
    type Value = EndpointName;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<EndpointName, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Name<'_> {
    type Value = EndpointName;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an account name")
    }

    refuse_scalars!();
    refuse_u64!();
    refuse_map!();

    fn visit_str<E: de::Error>(self, v: &str) -> Result<EndpointName, E> {
        match EndpointName::parse(v).filter(|n| !n.is_env()) {
            Some(name) => Ok(name),
            None => refuse(self.cell, RecordProblem::BadName),
        }
    }
}

struct Record<'a> {
    cell: &'a Problems,
    problem: RecordProblem,
}

impl<'de> DeserializeSeed<'de> for Record<'_> {
    type Value = Stored;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Stored, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Record<'_> {
    type Value = Stored;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a record")
    }

    refuse_scalars!();
    refuse_u64!();
    refuse_str!();

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Stored, A::Error> {
        let mut key = None;
        let mut owner = None;
        let mut url = None;
        while let Some(field) = map.next_key::<String>()? {
            let (slot, problem) = match field.as_str() {
                "key" => (&mut key, RecordProblem::KeyNotString),
                "owner" => (&mut owner, RecordProblem::OwnerNotString),
                "url" => (&mut url, RecordProblem::UrlNotString),
                _ => return refuse(self.cell, RecordProblem::UnknownRecordField),
            };
            if slot.is_some() {
                return refuse(self.cell, RecordProblem::Repeated);
            }
            *slot = Some(map.next_value_seed(Text {
                cell: self.cell,
                problem,
            })?);
        }
        let Some(key) = key else {
            return refuse(self.cell, RecordProblem::NoKey);
        };
        Ok(Stored {
            record: StoredRecord::Key {
                key: Secret::new(key),
                url: url.map(Secret::new),
            },
            owner: owner.map(PathBuf::from),
        })
    }
}

struct Text<'a> {
    cell: &'a Problems,
    problem: RecordProblem,
}

impl<'de> DeserializeSeed<'de> for Text<'_> {
    type Value = String;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Text<'_> {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(A_STRING)
    }

    refuse_scalars!();
    refuse_u64!();
    refuse_map!();

    fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
        Ok(v.to_owned())
    }
}

pub(super) fn render(every: &Every) -> Result<String, StoreError> {
    let stores: serde_json::Map<String, serde_json::Value> = every
        .iter()
        .map(|(id, store)| {
            let records: serde_json::Map<String, serde_json::Value> = store
                .iter()
                .map(|(name, stored)| {
                    let StoredRecord::Key { key, url } = &stored.record;
                    let mut record = serde_json::Map::new();
                    record.insert("key".into(), key.expose().into());
                    if let Some(owner) = &stored.owner {
                        record.insert("owner".into(), owner.to_string_lossy().into());
                    }
                    if let Some(url) = url {
                        record.insert("url".into(), url.expose().into());
                    }
                    (name.as_str().to_owned(), serde_json::Value::Object(record))
                })
                .collect();
            (id.as_str().to_owned(), serde_json::Value::Object(records))
        })
        .collect();
    let mut top = serde_json::Map::new();
    top.insert("stores".into(), serde_json::Value::Object(stores));
    top.insert("version".into(), FILE_VERSION.into());
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(top))
        .map_err(|_| StoreError::Write(std::io::ErrorKind::InvalidData))?;
    Ok(format!("{text}\n"))
}
