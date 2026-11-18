use std::fs;
use std::io::Write;
use std::time::{Duration, SystemTime};

use wisp_protocol::{Billing, Usage};

use super::*;
use crate::resume::tests::set_modified;
use crate::session::{JsonlStore, SessionStore};
use crate::test_guard;

pub(crate) fn append(store: &mut JsonlStore, day: Date, account: &str, spend: Spend) -> String {
    let line = line(&Record {
        at: format!("{day}T12:00:00Z"),
        session: "spend-test".into(),
        account: account.into(),
        model: "test-model".into(),
        spend,
    })
    .unwrap();
    store.append_spend(day, &line).unwrap();
    line
}

pub(crate) fn cost(usd: f64, billing: Billing) -> Spend {
    Spend {
        usd: Some(usd),
        usage: Some(Usage {
            billing,
            ..Usage::default()
        }),
        ..Spend::default()
    }
}

#[test]
fn days_sum_accounts_and_the_window_without_repricing() {
    let root = test_guard::dir();
    let sessions = root.path().join("sessions");
    let mut store = JsonlStore::open(&sessions).unwrap();
    let today: Date = "2026-11-14".parse().unwrap();
    let lost = BTreeSet::new();
    assert_eq!(
        days(&sessions, today, &lost),
        (Vec::new(), SpendWindow::default())
    );
    append(&mut store, today, "api", cost(1.0, Billing::Metered));
    append(&mut store, today, "api", cost(2.0, Billing::Metered));
    append(
        &mut store,
        today,
        "subscription",
        cost(3.0, Billing::AnthropicPlan),
    );
    append(&mut store, today, "mixed", cost(4.0, Billing::Mixed));
    append(
        &mut store,
        today,
        "unpriced",
        Spend {
            lower_bound: true,
            ..Spend::default()
        },
    );
    append(
        &mut store,
        today,
        "list-only",
        Spend {
            list_usd: Some(50.0),
            ..Spend::default()
        },
    );
    let oldest = today
        .checked_sub(jiff::Span::new().days(SPEND_WINDOW_DAYS - 1))
        .unwrap();
    append(&mut store, oldest, "api", cost(8.0, Billing::Metered));
    append(
        &mut store,
        oldest.yesterday().unwrap(),
        "excluded",
        cost(16.0, Billing::Metered),
    );
    append(
        &mut store,
        today.tomorrow().unwrap(),
        "excluded",
        cost(32.0, Billing::Metered),
    );
    let (found, window) = days(&sessions, today, &lost);
    assert_eq!(found.len(), SPEND_WINDOW_DAYS as usize);
    assert_eq!(found[0].day, today.to_string());
    assert_eq!(found[1].day, today.yesterday().unwrap().to_string());
    assert_eq!(
        found[0].accounts["api"],
        AccountSpend::charged(None, Some(3.0), false)
    );
    assert_eq!(found[0].accounts["subscription"].plan_usd, Some(3.0));
    assert_eq!(found[0].accounts["mixed"].plan_usd, None);
    assert!(found[0].accounts["unpriced"].lower_bound);
    assert!(!found[0].accounts.contains_key("list-only"));
    assert_eq!(
        found[0].total,
        AccountSpend {
            usd: 10.0,
            plan_usd: None,
            lower_bound: true
        }
    );
    assert!(found[1].accounts.is_empty());
    assert_eq!(found[1].total, AccountSpend::default());
    assert_eq!(window.since, None);
    assert_eq!(window.accounts["api"].usd, 11.0);
    assert!(!window.accounts["api"].lower_bound);
    assert_eq!(
        window.total,
        AccountSpend {
            usd: 18.0,
            plan_usd: None,
            lower_bound: true
        }
    );
    fs::remove_file(file(&sessions, oldest)).unwrap();
    let (found, window) = days(&sessions, today, &lost);
    assert_eq!(found.len(), 1);
    assert_eq!(window.since, Some(today.to_string()));
}

#[test]
fn bad_lines_read_errors_and_lost_writes_mark_every_affected_sum() {
    let root = test_guard::dir();
    let sessions = root.path().join("sessions");
    let mut store = JsonlStore::open(&sessions).unwrap();
    let today: Date = "2026-11-14".parse().unwrap();
    let yesterday = today.yesterday().unwrap();
    let before = yesterday.yesterday().unwrap();
    let empty = BTreeSet::new();
    let line = append(&mut store, today, "api", cost(1.0, Billing::Metered));
    append(
        &mut store,
        yesterday,
        "subscription",
        cost(2.0, Billing::ChatGptPlan),
    );
    let path = file(&sessions, today);
    let whole = format!("{line}\n \t\n{}", line.trim_end());
    fs::write(&path, &whole).unwrap();
    let (found, window) = days(&sessions, today, &empty);
    assert_eq!(found[0].total.usd, 2.0);
    assert!(!window.total.lower_bound);
    fs::write(&path, &whole[..whole.len() - 1]).unwrap();
    let (found, window) = days(&sessions, today, &empty);
    assert_eq!(found[0].total.usd, 1.0);
    assert!(found[0].accounts["api"].lower_bound);
    assert!(window.accounts["subscription"].lower_bound);
    assert!(window.total.lower_bound);
    fs::write(&path, &whole).unwrap();
    assert!(!days(&sessions, today, &empty).1.total.lower_bound);
    store.append_spend(today, "not a record\n").unwrap();
    let (found, window) = days(&sessions, today, &empty);
    assert_eq!(found[0].total.usd, 2.0);
    assert!(found[0].total.lower_bound);
    assert!(window.accounts.values().all(|a| a.lower_bound));
    let lost = BTreeSet::from([yesterday, before]);
    let (found, window) = days(&sessions, today, &lost);
    assert_eq!(found.len(), 3);
    assert!(found[1].accounts["subscription"].lower_bound);
    assert!(found[2].total.lower_bound);
    assert_eq!(window.since, Some(before.to_string()));
    let unreadable = before.yesterday().unwrap();
    fs::create_dir(file(&sessions, unreadable)).unwrap();
    let (found, window) = days(&sessions, today, &empty);
    assert_eq!(found.len(), 4);
    assert_eq!(found[3].total.usd, 0.0);
    assert!(found[3].total.lower_bound);
    assert!(window.total.lower_bound);
    fs::write(file(&sessions, before), "\n \t\n").unwrap();
    assert_eq!(
        days(&sessions, today, &empty).0[2].total,
        AccountSpend::default()
    );
}

#[test]
fn an_unchanged_day_is_read_once_and_a_changed_one_again() {
    let root = test_guard::dir();
    let sessions = root.path().join("sessions");
    let mut store = JsonlStore::open(&sessions).unwrap();
    let today: Date = "2026-11-14".parse().unwrap();
    let lost = BTreeSet::new();
    let line = append(&mut store, today, "api", cost(1.0, Billing::Metered));
    let read = || days(&sessions, today, &lost).1.total;
    assert_eq!(read().usd, 1.0);
    let path = file(&sessions, today);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let changed = line.replace("\"cost_usd\":1.0", "\"cost_usd\":2.0");
    assert_ne!(line, changed);
    assert_eq!(line.len(), changed.len());
    fs::write(&path, &changed).unwrap();
    set_modified(&path, modified);
    assert_eq!(read().usd, 1.0);
    let later = modified + Duration::from_secs(60);
    set_modified(&path, later);
    assert_eq!(read().usd, 2.0);
    fs::File::options()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    set_modified(&path, later);
    assert_eq!(read().usd, 3.0);
    fs::write(&path, &line).unwrap();
    set_modified(&path, later);
    assert_eq!(read().usd, 1.0);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(read(), AccountSpend::charged(None, Some(1.0), true));
    fs::remove_dir(&path).unwrap();
    assert!(days(&sessions, today, &lost).0.is_empty());
    fs::write(&path, &line).unwrap();
    let pre_epoch = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
    set_modified(&path, pre_epoch);
    assert_eq!(read().usd, 1.0);
    fs::write(&path, &changed).unwrap();
    set_modified(&path, pre_epoch);
    assert_eq!(read().usd, 2.0);
}
