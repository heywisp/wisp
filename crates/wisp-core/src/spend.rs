use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};
use wisp_protocol::{AccountSpend, SPEND_WINDOW_DAYS, SpendDay, SpendWindow};

use crate::agent::Spend;
use crate::clock::Clock;
use crate::resume::charge_account;
use crate::session::{self, Stamp};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub at: String,
    pub session: String,
    pub account: String,
    pub model: String,
    #[serde(flatten)]
    pub(crate) spend: Spend,
}

pub fn folder(sessions: &Path) -> PathBuf {
    session::beside(sessions, "spend")
}

pub fn file(sessions: &Path, day: Date) -> PathBuf {
    folder(sessions).join(format!("{day}.jsonl"))
}

pub(crate) fn line(record: &Record) -> io::Result<String> {
    wisp_protocol::jsonl::line(record).map_err(io::Error::other)
}

pub fn day(at: Timestamp, clock: &dyn Clock) -> Date {
    at.to_zoned(clock.local_zone().unwrap_or(TimeZone::UTC))
        .date()
}

type Accounts = BTreeMap<String, AccountSpend>;
type Seen = BTreeMap<PathBuf, (Stamp, Accounts, bool)>;

static SEEN: Mutex<Seen> = Mutex::new(BTreeMap::new());

fn sum_lines(reader: impl BufRead) -> io::Result<(Accounts, bool)> {
    let mut accounts = Accounts::new();
    let mut marked = false;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(record) => charge_account(
                &mut accounts,
                &record.account,
                AccountSpend::charged(
                    record.spend.usage.as_ref(),
                    record.spend.usd,
                    record.spend.lower_bound,
                ),
            ),
            Err(_) => marked = true,
        }
    }
    Ok((accounts, marked))
}

fn read_day(path: &Path, seen: &mut Seen) -> Option<(Accounts, bool)> {
    let read: io::Result<_> = (|| {
        let file = session::open_regular(path, "the spend file")?;
        let stamp = file.metadata().ok().and_then(|m| Stamp::of(&m));
        if let Some((_, accounts, marked)) =
            seen.get(path).filter(|(known, _, _)| Some(*known) == stamp)
        {
            return Ok((accounts.clone(), *marked));
        }
        let (accounts, marked) = sum_lines(BufReader::new(file))?;
        seen.remove(path);
        if let Some(stamp) = stamp {
            seen.insert(path.to_owned(), (stamp, accounts.clone(), marked));
        }
        Ok((accounts, marked))
    })();
    match read {
        Ok(sums) => Some(sums),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            seen.remove(path);
            None
        }
        Err(_) => Some((
            seen.get(path)
                .map(|(_, a, _)| a.clone())
                .unwrap_or_default(),
            true,
        )),
    }
}

fn marked(accounts: &mut Accounts, total: &mut AccountSpend) {
    total.lower_bound = true;
    for account in accounts.values_mut() {
        account.lower_bound = true;
    }
}

pub fn days(sessions: &Path, today: Date, lost: &BTreeSet<Date>) -> (Vec<SpendDay>, SpendWindow) {
    let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
    let mut days = Vec::new();
    let mut window = SpendWindow::default();
    let mut oldest = None;
    let mut any_marked = false;
    for offset in 0..SPEND_WINDOW_DAYS {
        let Ok(date) = today.checked_sub(jiff::Span::new().days(offset)) else {
            break;
        };
        let sums = read_day(&file(sessions, date), &mut seen);
        let lost = lost.contains(&date);
        if sums.is_some() || lost {
            oldest = Some(days.len());
        }
        let (accounts, mark) = sums.unwrap_or_default();
        let mut day = SpendDay {
            day: date.to_string(),
            accounts,
            ..SpendDay::default()
        };
        for (name, spent) in &day.accounts {
            day.total.add(*spent);
            charge_account(&mut window.accounts, name, *spent);
        }
        if mark || lost {
            marked(&mut day.accounts, &mut day.total);
            any_marked = true;
        }
        window.total.add(day.total);
        days.push(day);
    }
    days.truncate(oldest.map_or(0, |index| index + 1));
    if let Some(last) = days
        .last()
        .filter(|_| days.len() < SPEND_WINDOW_DAYS as usize)
    {
        window.since = Some(last.day.clone());
    }
    if any_marked {
        marked(&mut window.accounts, &mut window.total);
    }
    (days, window)
}

#[cfg(test)]
pub(crate) mod tests;
