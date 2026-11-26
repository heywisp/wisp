use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use wisp_plugin_api::private_files::{
    ChangeAction, CreateCap, Entry, Expected, PrivateFileError as Error, PrivateFiles, Status,
    component, components, reserved, validate_change,
};

#[derive(Clone, Default)]
pub struct FakePrivateFiles(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    files: BTreeMap<String, File>,
    failed_change: Option<Error>,
}

enum File {
    Bytes(Vec<u8>),
    Dir,
    Refused,
}

impl FakePrivateFiles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seed(&self, path: &str, bytes: Vec<u8>) {
        self.insert(path, File::Bytes(bytes));
    }

    pub fn refuse(&self, path: &str) {
        self.insert(path, File::Refused);
    }

    pub fn fail_next_change(&self, error: Error) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failed_change = Some(error);
    }

    fn insert(&self, path: &str, file: File) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        make_dirs(&mut state.files, path);
        state.files.insert(path.to_owned(), file);
    }
}

fn make_dirs(files: &mut BTreeMap<String, File>, path: &str) {
    for (i, _) in path.match_indices('/') {
        files.entry(path[..i].to_owned()).or_insert(File::Dir);
    }
}

fn check_dirs(files: &BTreeMap<String, File>, path: &str) -> Result<(), Error> {
    for (i, _) in path.match_indices('/') {
        match files.get(&path[..i]) {
            Some(File::Dir) => {}
            Some(_) => return Err(Error::Refused),
            None => return Err(Error::NotFound),
        }
    }
    Ok(())
}

fn direct<'a>(path: &str, full: &'a str) -> Option<&'a str> {
    let name = if path.is_empty() {
        full
    } else {
        full.strip_prefix(path)?.strip_prefix('/')?
    };
    (!name.contains('/')).then_some(name)
}

impl PrivateFiles for FakePrivateFiles {
    fn list(&self, path: &str, max_entries: u32) -> Result<Vec<Entry>, Error> {
        let files = &self.0.lock().unwrap_or_else(PoisonError::into_inner).files;
        if !path.is_empty() {
            components(path)?;
            check_dirs(files, path)?;
            match files.get(path) {
                Some(File::Dir) => {}
                Some(_) => return Err(Error::Refused),
                None => return Err(Error::NotFound),
            }
        }
        let mut out = Vec::new();
        for (full, file) in files {
            let Some(name) = direct(path, full) else {
                continue;
            };
            if reserved(name) {
                continue;
            }
            if out.len() as u64 >= u64::from(max_entries) {
                return Err(Error::OverLimit);
            }
            if !component(name) {
                return Err(Error::Refused);
            }
            let status = match file {
                File::Bytes(_) => Status::Regular,
                File::Dir => Status::Directory,
                File::Refused => Status::Refused,
            };
            out.push(Entry {
                name: name.to_owned(),
                status,
            });
        }
        Ok(out)
    }

    fn read(&self, path: &str, max_bytes: u32) -> Result<Vec<u8>, Error> {
        components(path)?.last().ok_or(Error::InvalidPath)?;
        let files = &self.0.lock().unwrap_or_else(PoisonError::into_inner).files;
        check_dirs(files, path)?;
        match files.get(path) {
            Some(File::Bytes(bytes)) if bytes.len() as u64 <= u64::from(max_bytes) => {
                Ok(bytes.clone())
            }
            Some(File::Bytes(_)) => Err(Error::OverLimit),
            Some(_) => Err(Error::Refused),
            None => Err(Error::NotFound),
        }
    }

    fn change(
        &self,
        path: &str,
        before: Expected,
        action: ChangeAction,
        create_cap: Option<CreateCap>,
    ) -> Result<(), Error> {
        components(path)?.last().ok_or(Error::InvalidPath)?;
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        validate_change(name, &before, &action, create_cap.as_ref())?;
        let creating = matches!(before, Expected::Absent);
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(error) = state.failed_change.take() {
            return Err(error);
        }
        let files = &mut state.files;
        match check_dirs(files, path) {
            Err(Error::NotFound) if creating => make_dirs(files, path),
            Err(Error::NotFound) => return Err(Error::Changed),
            result => result?,
        }
        match (files.get(path), before) {
            (None, Expected::Absent) => {}
            (Some(File::Bytes(bytes)), Expected::Hash(expected)) => {
                if bytes.len() as u64 > u64::from(expected.max_bytes) {
                    return Err(Error::OverLimit);
                }
                if wisp_plugin_api::hex::encode(&hmac_sha256::Hash::hash(bytes)) != expected.sha256
                {
                    return Err(Error::Changed);
                }
            }
            (Some(File::Dir | File::Refused), _) => return Err(Error::Refused),
            _ => return Err(Error::Changed),
        }
        if let Some(cap) = create_cap {
            let count = files
                .iter()
                .filter(|(full, file)| {
                    matches!(file, File::Bytes(_))
                        && direct(parent, full)
                            .is_some_and(|name| !reserved(name) && name.ends_with(&cap.suffix))
                })
                .count();
            if count as u64 >= u64::from(cap.max_regular) {
                return Err(Error::CapReached);
            }
        }
        match action {
            ChangeAction::Write(bytes) => {
                files.insert(path.to_owned(), File::Bytes(bytes));
            }
            ChangeAction::Delete => {
                files.remove(path);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_validates_and_hides_case_aliases_and_bounds_compare() {
        let files = FakePrivateFiles::new();
        for name in [".WISP.LOCK", ".Wisp-TmP-fixture.dat"] {
            files.seed(name, vec![]);
            assert_eq!(files.read(name, 1), Err(Error::InvalidPath));
            assert_eq!(
                files.change(name, Expected::Absent, ChangeAction::Write(vec![]), None),
                Err(Error::InvalidPath)
            );
        }
        assert!(files.list("", 1).unwrap().is_empty());
        let bytes = vec![1, 2];
        assert_eq!(
            files.change(
                "a.dat",
                Expected::Absent,
                ChangeAction::Write(bytes.clone()),
                Some(CreateCap {
                    suffix: ".dat".into(),
                    max_regular: 1
                })
            ),
            Ok(())
        );
        let before = Expected::hash(
            wisp_plugin_api::hex::encode(&hmac_sha256::Hash::hash(&bytes)),
            1,
        );
        assert_eq!(
            files.change("a.dat", before, ChangeAction::Delete, None),
            Err(Error::OverLimit)
        );
        assert_eq!(files.read("a.dat", 2), Ok(bytes));
    }
}
