use wisp_plugin_api::manifest::{BUNDLED_IDS, Manifest, Origin};

use super::*;

#[test]
fn every_bundled_manifest_parses_and_ids_are_unique() {
    let mut ids: Vec<String> = bundled()
        .iter()
        .map(|b| Manifest::parse(b.manifest, Origin::Bundled).unwrap().id)
        .collect();
    let count = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), count);
    let mut bundled_ids = BUNDLED_IDS.to_vec();
    bundled_ids.sort_unstable();
    assert_eq!(ids, bundled_ids);
    let table = include_str!("../../../docs/plugins.md");
    let mut rows: Vec<&str> = table
        .lines()
        .filter_map(|l| {
            let (id, rest) = l.strip_prefix("| `")?.split_once("` | `plugins/")?;
            rest.starts_with(&format!("{id}` |")).then_some(id)
        })
        .collect();
    rows.sort_unstable();
    assert_eq!(rows, ids, "docs/plugins.md, \"Bundled plugins\"");
}

#[test]
fn exactly_one_bundled_hook_words_the_job_news_and_it_is_the_processes_notice() {
    let news: Vec<String> = bundled()
        .iter()
        .map(|b| Manifest::parse(b.manifest, Origin::Bundled).unwrap())
        .flat_map(|m| {
            m.hooks
                .iter()
                .filter(|h| h.job_news && h.point == "before-turn")
                .map(|h| format!("{}/{}", m.id, h.name))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(news, ["shell/processes"]);
}

#[test]
fn memory_id_stays_reserved_without_the_bundle_feature() {
    let reports_memory = bundled().iter().any(|bundle| {
        Manifest::parse(bundle.manifest, Origin::Bundled)
            .unwrap()
            .id
            == "memory"
    });
    assert_eq!(reports_memory, cfg!(feature = "memory"));
    let outside = "id = 'memory'\nversion = '0.1.0'\nworld = '0.3.0'\nsummary = 'Fixture'\n";
    let refused = Manifest::parse(outside, Origin::External).unwrap_err();
    assert!(refused.to_string().contains("bundled"));
}
