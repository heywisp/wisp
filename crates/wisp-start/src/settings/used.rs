use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wisp_core::secrets::write_atomic;
use wisp_protocol::settings::find;

const DAYS_KEPT: u64 = 60;

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Used(BTreeMap<String, Entry>);

#[derive(Deserialize, Serialize)]
struct Entry {
    count: u64,
    last_day: u64,
}

impl Used {
    pub(super) fn load(path: &Path, today: u64) -> Self {
        let mut used: Self = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        used.0.retain(|name, entry| {
            find(name).is_some() && today.saturating_sub(entry.last_day) < DAYS_KEPT
        });
        used
    }

    pub(super) fn bump(path: &Path, names: &[String], today: u64) {
        let mut used = Self::load(path, today);
        for name in names.iter().filter(|name| find(name).is_some()) {
            let entry = used.0.entry(name.clone()).or_insert(Entry {
                count: 0,
                last_day: today,
            });
            entry.count = entry.count.saturating_add(1);
            entry.last_day = today;
        }
        if let Some(folder) = path.parent()
            && wisp_core::session::create_private_dir(folder).is_ok()
            && let Ok(bytes) = serde_json::to_vec(&used)
        {
            let _ = write_atomic(path, &bytes, 0o600);
        }
    }

    pub(super) fn ranked(&self) -> Vec<String> {
        let mut entries: Vec<_> = self.0.iter().collect();
        entries.sort_by_key(|(_, entry)| std::cmp::Reverse((entry.count, entry.last_day)));
        entries.into_iter().map(|(name, _)| name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_protocol::settings::names;

    #[test]
    fn counts_rank_before_recency_and_sixty_days_without_a_change_drops_a_name() {
        let folder = crate::scratch_dir("used-ranked");
        let path = folder.root.join("data/settings-used.json");
        Used::bump(
            &path,
            &[names::TIME.into(), names::DISPLAY_MOUSE.into()],
            10,
        );
        Used::bump(&path, &[names::TIME.into()], 11);
        Used::bump(&path, &[names::DEBUG_CACHE.into()], 12);
        let used = Used::load(&path, 69);
        assert_eq!(
            used.ranked(),
            [names::TIME, names::DEBUG_CACHE, names::DISPLAY_MOUSE]
        );
        assert_eq!(used.0[names::TIME].count, 2);
        assert_eq!(
            Used::load(&path, 70).ranked(),
            [names::TIME, names::DEBUG_CACHE]
        );
        Used::bump(&path, &[names::DEBUG_CACHE.into()], 71);
        assert_eq!(Used::load(&path, 71).ranked(), [names::DEBUG_CACHE]);
        assert_eq!(Used::load(&path, 71).0[names::DEBUG_CACHE].count, 2);
    }

    #[test]
    fn missing_or_broken_files_are_empty_and_the_next_use_replaces_them() {
        let folder = crate::scratch_dir("used-broken");
        let path = folder.root.join("data/settings-used.json");
        for contents in [
            None,
            Some("{"),
            Some(r#"{"unregistered":{"count":2,"last_day":5}}"#),
        ] {
            if let Some(contents) = contents {
                std::fs::write(&path, contents).unwrap();
            }
            assert!(Used::load(&path, 5).ranked().is_empty());
            Used::bump(&path, &[names::TIME.into()], 5);
            assert_eq!(Used::load(&path, 5).ranked(), [names::TIME]);
            assert_eq!(Used::load(&path, 5).0.len(), 1);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let data = std::fs::metadata(path.parent().unwrap()).unwrap();
                assert_eq!(data.permissions().mode() & 0o777, 0o700);
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        let blocked = folder.root.join("blocked");
        std::fs::write(&blocked, "not a folder").unwrap();
        Used::bump(
            &blocked.join("settings-used.json"),
            &[names::TIME.into()],
            5,
        );
    }
}
