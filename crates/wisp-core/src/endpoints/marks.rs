use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::{ConfigFile, EndpointName};

pub const COLOURS: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Unset,
    Set(u8),
    Bad,
}

impl Mark {
    pub fn parse(value: Option<i128>) -> Mark {
        match value.and_then(|n| u8::try_from(n).ok()) {
            Some(n @ 1..=COLOURS) => Mark::Set(n),
            _ => Mark::Bad,
        }
    }

    fn colour(self) -> Option<u8> {
        match self {
            Mark::Set(n) => Some(n),
            Mark::Unset | Mark::Bad => None,
        }
    }
}

pub fn home(name: &EndpointName) -> u8 {
    Sha256::digest(name.as_str().as_bytes())[0] % COLOURS + 1
}

pub fn pick(
    config: Option<&ConfigFile>,
    keys_file: Option<&EndpointName>,
) -> BTreeMap<EndpointName, u8> {
    let tables = config.map_or(&[][..], |c| c.marks.as_slice());
    let written = |name: &EndpointName| tables.iter().find(|(n, _)| n == name).map(|(_, m)| *m);
    let mut order: Vec<(&EndpointName, Option<u8>)> = tables
        .iter()
        .filter(|(name, _)| !name.is_env())
        .map(|(name, mark)| (name, mark.colour()))
        .collect();
    let env_mark = written(&EndpointName::env()).and_then(Mark::colour);
    if let Some(account) = keys_file.filter(|a| written(a).is_none()) {
        order.push((account, env_mark));
    }
    let mut taken = [false; COLOURS as usize];
    if let Some(n) = env_mark {
        taken[usize::from(n - 1)] = true;
    }
    let mut marks = BTreeMap::new();
    for (name, mark) in &order {
        if let Some(n) = mark {
            taken[usize::from(n - 1)] = true;
            marks.insert((*name).clone(), *n);
        }
    }
    for (name, _) in order.iter().filter(|(_, mark)| mark.is_none()) {
        let home = home(name);
        let free = (0..COLOURS)
            .map(|step| (home - 1 + step) % COLOURS + 1)
            .find(|n| !taken[usize::from(n - 1)])
            .unwrap_or(home);
        taken[usize::from(free - 1)] = true;
        marks.insert((*name).clone(), free);
    }
    marks
}
