use std::sync::atomic::Ordering;

use std::sync::Arc;

use crate::agent::testing::{Caps, Fake, Knobs};
use crate::endpoints::generation::Generation;
use crate::endpoints::testing::AuthHost;
use crate::models::list::ModelList;
use crate::models::list::tests::{claude_entry, claude_list};
use crate::provider::anthropic::capabilities;

fn generation(host: &AuthHost, name: &str) -> Generation {
    let connection = host
        .catalog
        .view()
        .ready
        .into_iter()
        .find(|c| c.name == name);
    connection.and_then(|c| c.provider.generation()).unwrap()
}

#[tokio::test]
async fn an_unchanged_stamp_never_looks_and_a_change_here_never_hides_a_later_one_elsewhere() {
    let host = AuthHost::new();
    host.load("b");
    host.catalog.check().await;
    host.catalog.check().await;
    assert_eq!(host.elsewhere.looks.load(Ordering::SeqCst), 1);
    let off = std::collections::BTreeMap::from([("gemini".to_owned(), false)]);
    host.catalog.set_providers_off(off.clone());
    host.catalog.check().await;
    host.catalog.check().await;
    assert_eq!(host.elsewhere.looks.load(Ordering::SeqCst), 2);
    assert_eq!(*host.elsewhere.off.lock().unwrap(), off);
    host.catalog.set_providers_off(Default::default());
    host.catalog.check().await;
    assert!(host.elsewhere.off.lock().unwrap().is_empty());
    host.catalog
        .begin("b")
        .unwrap()
        .publish(host.catalog.view());
    host.elsewhere(&[], &[]);
    host.catalog.check().await;
    let b = generation(&host, "b");
    host.elsewhere(&[], &["b"]);
    host.catalog.check().await;
    assert!(b.admit().is_err());
    host.load("b");
    let b = generation(&host, "b");
    host.elsewhere(&["b"], &[]);
    host.catalog.check().await;
    assert_eq!(host.catalog.reader().take_removed(), ["b"]);
    assert!(b.admit().is_err());
    let (change, view) = host.loading("b");
    host.catalog.check().await;
    host.elsewhere(&[], &["b"]);
    change.publish(view);
    let b = generation(&host, "b");
    host.catalog.check().await;
    assert!(b.admit().is_ok(), "a look between this window's writes");
}

#[tokio::test]
async fn a_colour_change_elsewhere_reconnects_nothing_and_sends_the_new_marks() {
    let host = AuthHost::new();
    host.load("b");
    host.catalog.check().await;
    let b = generation(&host, "b");
    let revision = host.catalog.revision();
    let mut view = host.catalog.view();
    view.endpoints.iter_mut().for_each(|e| e.mark = Some(5));
    host.write(&view, &[]);
    host.catalog.check().await;
    assert!(b.admit().is_ok());
    assert_eq!(host.catalog.revision(), revision + 1);
    let marks: Vec<_> = host
        .catalog
        .view()
        .endpoints
        .iter()
        .map(|e| e.mark)
        .collect();
    assert_eq!(marks, [Some(5)]);
}

fn listed(ids: &[&str]) -> Arc<ModelList> {
    let entries: Vec<_> = ids
        .iter()
        .map(|id| claude_entry(id, "opus", 1_000_000))
        .collect();
    Arc::new(claude_list(&entries))
}

fn listing(old: &[&str], fresh: Option<&[&str]>) -> Arc<Fake> {
    let knobs = Knobs {
        caps: Some(Caps::Fn(capabilities::row)),
        list: Some(listed(old)),
        fresh: fresh.map(listed),
        ..Knobs::default()
    };
    Fake::with(knobs, [])
}

#[tokio::test(start_paused = true)]
async fn a_refetch_that_adds_a_model_bumps_the_revision_once_and_one_that_adds_nothing_bumps_nothing()
 {
    let host = AuthHost::new();
    let known = ["claude-opus-5-5"];
    let fetching = listing(&known, Some(&["claude-opus-5-5", "claude-opus-6"]));
    let unasked = listing(&known, None);
    *host.next_provider.lock().unwrap() = Some(fetching.clone());
    host.load("b");
    *host.next_provider.lock().unwrap() = Some(unasked.clone());
    host.load("c");
    host.catalog.check().await;
    let folder = tempfile::tempdir().unwrap();
    let catalog = host.catalog.clone().with_lists(Some(folder.path().into()));
    let b = generation(&host, "b");
    let mut changes = catalog.changes();
    let revision = catalog.revision();
    let models = |name: &str| {
        let view = catalog.view();
        let summary = view.endpoints.into_iter().find(|e| e.name == name);
        summary
            .unwrap()
            .models
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>()
    };
    assert!(models("b").is_empty());
    catalog.refetch("b", false).await;
    assert_eq!(catalog.revision(), revision + 1);
    assert!(changes.has_changed().unwrap());
    assert_eq!(models("b"), ["claude-opus-5-5", "claude-opus-6"]);
    assert!(b.admit().is_ok());
    changes.borrow_and_update();
    catalog.refetch("b", false).await;
    assert_eq!(fetching.refreshes(), 2);
    assert_eq!(catalog.revision(), revision + 1);
    assert!(!changes.has_changed().unwrap());
    let reader = catalog.reader();
    fetching.knobs(|knobs| knobs.stall_list = true);
    let started = tokio::time::Instant::now();
    let mut first = Box::pin(reader.refetch_unlisted("b", "claude-opus-7"));
    assert!(futures::poll!(&mut first).is_pending());
    tokio::task::yield_now().await;
    assert_eq!(fetching.refreshes(), 3);
    let wait = crate::models::list::LIST_WAIT;
    tokio::time::advance(wait - std::time::Duration::from_secs(1)).await;
    let mut second = Box::pin(reader.refetch_unlisted("b", "claude-opus-7"));
    assert!(futures::poll!(&mut second).is_pending());
    drop(first);
    tokio::time::timeout(wait, second).await.unwrap();
    assert_eq!(started.elapsed(), wait);
    assert!(futures::poll!(Box::pin(reader.refetch_unlisted("b", "claude-opus-7"))).is_ready());
    reader.refetch_unlisted("b", "claude-opus-6").await;
    assert_eq!(fetching.refreshes(), 3);
    assert_eq!(catalog.revision(), revision + 1);
    assert!(!changes.has_changed().unwrap());
    catalog.refetch("c", false).await;
    catalog.refetch("gone", true).await;
    assert_eq!(unasked.refreshes(), 0);
    let refused = listing(&known, Some(&known));
    *host.next_provider.lock().unwrap() = Some(refused.clone());
    let (change, mut view) = host.loading("d");
    view.endpoints
        .iter_mut()
        .for_each(|e| e.expired = e.name == "d");
    change.publish(view);
    catalog.refetch("d", false).await;
    assert_eq!(refused.refreshes(), 0);
    catalog.refetch("d", true).await;
    assert_eq!(refused.refreshes(), 1);
    host.elsewhere(&["b"], &[]);
    catalog.check().await;
    catalog.refetch("b", true).await;
    assert_eq!(fetching.refreshes(), 3);
    let without = host.catalog.clone();
    without.refetch("c", true).await;
    assert_eq!(unasked.refreshes(), 0);
}

#[tokio::test]
async fn off_or_removed_providers_stop_new_admissions_and_refresh_account_reasons() {
    for initially_removed in [false, true] {
        let provider = Fake::with(
            Knobs {
                kind: Some(crate::endpoints::ProviderKind::Gemini),
                ..Knobs::default()
            },
            [],
        );
        let host = AuthHost::new();
        *host.next_provider.lock().unwrap() = Some(provider);
        for name in ["g1", "g2"] {
            host.load(name);
        }
        host.catalog.check().await;
        let (change, view) = host.loading("g2");
        host.catalog.check().await;
        change.publish(view);
        host.catalog
            .begin("g1")
            .unwrap()
            .publish(host.catalog.view());
        let generations = ["g1", "g2"].map(|name| generation(&host, name));
        let admitted: Vec<_> = generations.iter().map(|g| g.admit().unwrap()).collect();
        for removed in [false, true]
            .into_iter()
            .skip(usize::from(initially_removed))
        {
            let reason = if removed {
                "gemini is removed"
            } else {
                "gemini is off"
            };
            host.catalog
                .set_providers_off(std::collections::BTreeMap::from([(
                    "gemini".to_owned(),
                    removed,
                )]));
            host.catalog.check().await;
            let view = host.catalog.view();
            for (name, generation) in ["g1", "g2"].into_iter().zip(&generations) {
                assert!(matches!(
                    generation.admit(),
                    Err(crate::endpoints::generation::Unavailable::Closing)
                ));
                assert!(!view.ready.iter().any(|c| c.name == name));
                assert!(
                    view.unavailable
                        .iter()
                        .any(|(alias, why)| alias == name && why == reason)
                );
            }
            assert!(generation(&host, "env").admit().is_ok());
        }
        drop(admitted);
        for generation in generations {
            assert_eq!(
                generation.available(),
                Err(crate::endpoints::generation::Unavailable::Retired)
            );
        }
    }
    let entries = |reason: &str| {
        std::collections::BTreeMap::from([(
            crate::test_guard::name("g1"),
            crate::endpoints::Availability::Unavailable(
                crate::endpoints::Unready::new(
                    reason.into(),
                    crate::endpoints::ProviderKind::Gemini,
                )
                .because(crate::endpoints::Cause::ProviderOff),
            ),
        )])
    };
    assert_ne!(
        super::prints(&entries("gemini is off")),
        super::prints(&entries("gemini is removed"))
    );
}
