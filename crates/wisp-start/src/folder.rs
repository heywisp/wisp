use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use wisp_core::endpoints::watch::Stamp;
use wisp_core::plugin::{Installed, Problem, Source};
use wisp_wasm_host::{Dirs, Found};

pub type Read = fn(&Dirs, &Path) -> Result<Found, (String, String)>;

struct Folder {
    stamp: (Vec<String>, Option<Vec<u8>>, Stamp),
    good: Option<(String, Source)>,
    problem: Option<Problem>,
}

pub struct PluginFolder {
    dirs: Dirs,
    read: Read,
    folders: Mutex<Option<BTreeMap<String, Folder>>>,
}

impl PluginFolder {
    pub fn new(dirs: Dirs, read: Read) -> Self {
        Self {
            dirs,
            read,
            folders: Mutex::new(None),
        }
    }

    pub fn installed(&self, except: Option<&str>) -> (Vec<Source>, Vec<Problem>) {
        let mut folders = self.folders.lock().unwrap_or_else(PoisonError::into_inner);
        let folders = match &mut *folders {
            Some(folders) => folders,
            None => {
                let mut read = BTreeMap::new();
                self.scan(&mut read);
                folders.insert(read)
            }
        };
        let others = |id: &String| except != Some(id.as_str());
        let sources = folders
            .values()
            .filter_map(|f| f.good.as_ref())
            .filter(|(id, _)| others(id))
            .map(|(_, source)| source.clone())
            .collect();
        let problems = folders
            .values()
            .filter_map(|f| f.problem.clone())
            .filter(|p| others(&p.plugin))
            .collect();
        (sources, problems)
    }

    fn scan(&self, folders: &mut BTreeMap<String, Folder>) -> Option<Vec<Problem>> {
        let listed = wisp_wasm_host::folders(&self.dirs);
        let before = folders.len();
        folders.retain(|name, _| listed.iter().any(|(listed, _)| listed == name));
        let mut moved = folders.len() != before;
        let mut fresh = Vec::new();
        for (name, path) in listed {
            let stems = wisp_wasm_host::stems(&path).unwrap_or_default();
            let pair = match stems.as_slice() {
                [stem] => [".toml", ".wasm"].map(|end| Some(path.join(format!("{stem}{end}")))),
                _ => [None, None],
            };
            let manifest = pair[0].as_ref().and_then(|toml| std::fs::read(toml).ok());
            let files = Stamp::of(pair.iter().map(Option::as_deref));
            let stamp = (stems, manifest, files);
            if folders.get(&name).is_some_and(|f| f.stamp == stamp) {
                continue;
            }
            moved = true;
            let good = folders.remove(&name).and_then(|f| f.good);
            let folder = match good {
                Some(good) if stamp.0.len() > 1 => Folder {
                    stamp,
                    good: Some(good),
                    problem: None,
                },
                good => match (self.read)(&self.dirs, &path) {
                    Ok(found) => Folder {
                        stamp,
                        good: Some((
                            found.id,
                            Source::wasm(found.manifest, found.make, found.sha256),
                        )),
                        problem: None,
                    },
                    Err((plugin, message)) => {
                        let problem = Problem { plugin, message };
                        fresh.push(problem.clone());
                        Folder {
                            stamp,
                            good,
                            problem: Some(problem),
                        }
                    }
                },
            };
            folders.insert(name, folder);
        }
        moved.then_some(fresh)
    }
}

impl Installed for PluginFolder {
    fn changed(&self) -> Option<(Vec<Source>, Vec<Problem>)> {
        let fresh = {
            let mut folders = self.folders.lock().unwrap_or_else(PoisonError::into_inner);
            self.scan(folders.get_or_insert_with(BTreeMap::new))?
        };
        Some((self.installed(None).0, fresh))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static READS: AtomicUsize = AtomicUsize::new(0);

    fn read(_: &Dirs, folder: &Path) -> Result<Found, (String, String)> {
        READS.fetch_add(1, Ordering::SeqCst);
        let id = folder.file_name().unwrap().to_string_lossy().into_owned();
        let [stem] = wisp_wasm_host::stems(folder).unwrap().try_into().unwrap();
        let manifest = std::fs::read_to_string(folder.join(format!("{stem}.toml"))).unwrap();
        if manifest == "broken" || folder.join("damaged").exists() {
            return Err((id, "it broke".to_owned()));
        }
        let make: wisp_wasm_host::Make = Arc::new(|_| panic!("not started"));
        Ok(Found {
            id,
            manifest,
            make,
            sha256: stem,
        })
    }

    struct Rig {
        scratch: crate::scratch::ScratchDir,
        folder: PluginFolder,
    }

    impl Rig {
        fn new(name: &str) -> Self {
            let scratch = crate::scratch_dir(&format!("folder-{name}"));
            let dirs = Dirs {
                data: scratch.dir("data"),
                cache: None,
                env: Vec::new(),
            };
            Self {
                scratch,
                folder: PluginFolder::new(dirs, read),
            }
        }

        fn place(&self, id: &str, stem: &str, manifest: &str) {
            self.scratch
                .write(&format!("data/plugins/{id}/{stem}.toml"), manifest);
            self.scratch
                .write(&format!("data/plugins/{id}/{stem}.wasm"), "");
        }

        fn rewrite_in_one_tick(&self, id: &str, stem: &str, manifest: &str) {
            let path = self
                .scratch
                .dir(&format!("data/plugins/{id}"))
                .join(format!("{stem}.toml"));
            let at = std::fs::metadata(&path).unwrap().modified().unwrap();
            assert_eq!(std::fs::read(&path).unwrap().len(), manifest.len());
            self.place(id, stem, manifest);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at)
                .unwrap();
        }

        fn reinstall_same_files(&self, id: &str, stem: &str) {
            let folder = self.scratch.dir(&format!("data/plugins/{id}"));
            for end in [".toml", ".wasm"] {
                let path = folder.join(format!("{stem}{end}"));
                let (bytes, at) = (
                    std::fs::read(&path).unwrap(),
                    std::fs::metadata(&path).unwrap().modified().unwrap(),
                );
                let fresh = folder.join("fresh");
                std::fs::write(&fresh, bytes).unwrap();
                std::fs::rename(&fresh, &path).unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(at)
                    .unwrap();
            }
        }

        fn manifests(sources: &[Source]) -> Vec<&str> {
            sources.iter().map(|s| s.manifest.as_str()).collect()
        }
    }

    #[test]
    fn only_changed_folders_are_read_again_and_a_deleted_one_drops_out() {
        let rig = Rig::new("changed");
        rig.place("aa", "1", "aa one");
        rig.place("bb", "1", "bb one");
        let (sources, _) = rig.folder.installed(None);
        assert_eq!(Rig::manifests(&sources), ["aa one", "bb one"]);
        assert_eq!(READS.load(Ordering::SeqCst), 2);
        assert!(rig.folder.changed().is_none());
        rig.place("bb", "1", "bb two!");
        let (sources, problems) = rig.folder.changed().unwrap();
        assert_eq!(Rig::manifests(&sources), ["aa one", "bb two!"]);
        assert!(problems.is_empty());
        assert_eq!(READS.load(Ordering::SeqCst), 3);
        std::fs::remove_dir_all(rig.scratch.dir("data/plugins/bb")).unwrap();
        let (sources, _) = rig.folder.changed().unwrap();
        assert_eq!(Rig::manifests(&sources), ["aa one"]);
        assert_eq!(READS.load(Ordering::SeqCst), 3);
        assert!(rig.folder.changed().is_none());
    }

    #[test]
    fn a_failed_read_keeps_the_last_good_with_a_note_and_a_second_pair_keeps_it_silently() {
        let rig = Rig::new("kept");
        rig.place("aa", "1", "aa one");
        rig.folder.installed(None);
        rig.rewrite_in_one_tick("aa", "1", "broken");
        let (sources, problems) = rig.folder.changed().unwrap();
        assert_eq!(Rig::manifests(&sources), ["aa one"]);
        assert_eq!(
            problems,
            [Problem {
                plugin: "aa".to_owned(),
                message: "it broke".to_owned(),
            }]
        );
        let reads = READS.load(Ordering::SeqCst);
        rig.place("aa", "2", "aa three");
        let (sources, problems) = rig.folder.changed().unwrap();
        assert_eq!(Rig::manifests(&sources), ["aa one"]);
        assert!(problems.is_empty());
        assert_eq!(READS.load(Ordering::SeqCst), reads);
        std::fs::remove_file(rig.scratch.dir("data/plugins/aa").join("1.toml")).unwrap();
        let (sources, _) = rig.folder.changed().unwrap();
        assert_eq!(Rig::manifests(&sources), ["aa three"]);
    }

    #[test]
    fn a_plugin_left_out_at_start_and_reinstalled_with_the_same_files_is_read_again() {
        let rig = Rig::new("reinstalled");
        rig.place("aa", "1", "aa one");
        rig.scratch.write("data/plugins/aa/damaged", "");
        let (sources, problems) = rig.folder.installed(None);
        assert!(sources.is_empty());
        assert_eq!(problems.len(), 1);
        std::fs::remove_file(rig.scratch.dir("data/plugins/aa").join("damaged")).unwrap();
        rig.reinstall_same_files("aa", "1");
        let (sources, problems) = rig.folder.changed().unwrap();
        assert_eq!(Rig::manifests(&sources), ["aa one"]);
        assert!(problems.is_empty());
    }
}
