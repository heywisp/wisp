use std::path::PathBuf;

use wisp_plugin_api::private_files::{
    ChangeAction, CreateCap, Entry, Expected, PrivateFileError, PrivateFiles,
};

use super::{Caller, Import, PluginHost};

impl PluginHost {
    fn private_base(&self, import: Import) -> Result<PathBuf, PrivateFileError> {
        if self.refuses(import) {
            return Err(PrivateFileError::Unavailable);
        }
        self.private_data_root()
            .ok_or(PrivateFileError::Unavailable)
    }
}

impl PrivateFiles for PluginHost {
    fn list(&self, path: &str, max_entries: u32) -> Result<Vec<Entry>, PrivateFileError> {
        disk::list(
            self.private_base(Import::PrivateFilesList)?,
            path,
            max_entries,
        )
    }

    fn read(&self, path: &str, max_bytes: u32) -> Result<Vec<u8>, PrivateFileError> {
        disk::read(
            self.private_base(Import::PrivateFilesRead)?,
            path,
            max_bytes,
        )
    }

    fn change(
        &self,
        path: &str,
        before: Expected,
        action: ChangeAction,
        create_cap: Option<CreateCap>,
    ) -> Result<(), PrivateFileError> {
        if !Caller::current().allows_private_write {
            return Err(PrivateFileError::Unavailable);
        }
        disk::change(
            self.private_base(Import::PrivateFilesChange)?,
            path,
            before,
            action,
            create_cap,
        )
    }
}

pub(crate) fn read_or_create<const N: usize>(
    root: PathBuf,
    name: &str,
    fresh: impl FnOnce() -> [u8; N],
) -> Option<[u8; N]> {
    disk::read_or_create(root, name, fresh).ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod disk {
    use super::{ChangeAction, CreateCap, Entry, Expected, PathBuf, PrivateFileError as Error};

    pub(super) fn read_or_create<const N: usize>(
        _: PathBuf,
        _: &str,
        _: impl FnOnce() -> [u8; N],
    ) -> Result<[u8; N], Error> {
        Err(Error::Unavailable)
    }

    pub(super) fn list(_: PathBuf, _: &str, _: u32) -> Result<Vec<Entry>, Error> {
        Err(Error::Unavailable)
    }

    pub(super) fn read(_: PathBuf, _: &str, _: u32) -> Result<Vec<u8>, Error> {
        Err(Error::Unavailable)
    }

    pub(super) fn change(
        _: PathBuf,
        _: &str,
        _: Expected,
        _: ChangeAction,
        _: Option<CreateCap>,
    ) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod disk {
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path, PathBuf};
    use std::time::Instant;

    use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, Stat};
    use rustix::io::Errno;
    use wisp_plugin_api::private_files::{
        ChangeAction, CreateCap, Entry, Expected, LOCK_FILE, PrivateFileError as Error, Status,
        TEMP_PREFIX, component, components, reserved, validate_change,
    };

    use crate::secrets::{LOCK_WAIT, lock_file, sync_staged};
    use crate::session::{check_owned, check_regular_owned, read_nofollow_flags, tighten_file};

    const ANCESTOR_DIR_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::CLOEXEC);
    const DIR_FLAGS: OFlags = ANCESTOR_DIR_FLAGS.union(OFlags::NOFOLLOW);
    const DIR_MODE: Mode = Mode::RWXU;
    const FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

    fn error(e: impl Into<io::Error>) -> Error {
        let e = e.into();
        if e.raw_os_error() == Some(Errno::LOOP.raw_os_error()) {
            return Error::Refused;
        }
        match e.kind() {
            io::ErrorKind::NotFound => Error::NotFound,
            io::ErrorKind::NotADirectory | io::ErrorKind::PermissionDenied => Error::Refused,
            io::ErrorKind::AlreadyExists => Error::Changed,
            _ => Error::Io,
        }
    }

    fn descend(parent: &File, name: &Path, flags: OFlags, create: bool) -> Result<File, Error> {
        match fs::openat(parent, name, flags, Mode::empty()) {
            Err(Errno::NOENT) if create => {
                let made = match fs::mkdirat(parent, name, DIR_MODE) {
                    Ok(()) => true,
                    Err(Errno::EXIST) => false,
                    Err(e) => return Err(error(e)),
                };
                let dir =
                    File::from(fs::openat(parent, name, flags, Mode::empty()).map_err(error)?);
                if made {
                    parent.sync_all().map_err(error)?;
                }
                Ok(dir)
            }
            result => result.map(File::from).map_err(error),
        }
    }

    fn private(dir: &File) -> Result<(), Error> {
        let meta = dir.metadata().map_err(error)?;
        check_owned(meta.uid(), "private directory").map_err(error)?;
        if meta.mode() & 0o077 != 0 {
            return Err(Error::Refused);
        }
        Ok(())
    }

    fn directory(root: PathBuf, parts: &[&str], create: bool) -> Result<File, Error> {
        let absolute = std::path::absolute(root).map_err(error)?;
        let mut names = absolute.components().peekable();
        if names.next() != Some(Component::RootDir) {
            return Err(Error::InvalidPath);
        }
        let mut dir = File::from(fs::open("/", DIR_FLAGS, Mode::empty()).map_err(error)?);
        while let Some(name) = names.next() {
            let Component::Normal(name) = name else {
                return Err(Error::InvalidPath);
            };
            let private_part = names.clone().nth(1).is_none();
            let flags = if private_part {
                DIR_FLAGS
            } else {
                ANCESTOR_DIR_FLAGS
            };
            dir = descend(&dir, Path::new(name), flags, create)?;
            if private_part {
                private(&dir)?;
            }
        }
        for part in parts {
            dir = descend(&dir, Path::new(part), DIR_FLAGS, create)?;
            private(&dir)?;
        }
        Ok(dir)
    }

    fn entries(
        dir: &File,
    ) -> Result<impl Iterator<Item = Result<(String, Option<Stat>), Error>> + '_, Error> {
        Ok(Dir::read_from(dir)
            .map_err(error)?
            .filter_map(move |entry| {
                let get = || {
                    let entry = entry.map_err(error)?;
                    let name = entry.file_name().to_str().map_err(|_| Error::Refused)?;
                    if matches!(name, "." | "..") || reserved(name) {
                        return Ok(None);
                    }
                    let stat = match fs::statat(dir, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(stat) => Some(stat),
                        Err(Errno::NOENT) => None,
                        Err(e) => return Err(error(e)),
                    };
                    Ok(Some((name.to_owned(), stat)))
                };
                get().transpose()
            }))
    }

    fn status(stat: Option<Stat>) -> Status {
        let Some(stat) = stat else {
            return Status::Refused;
        };
        if check_owned(stat.st_uid, "private entry").is_err() {
            return Status::Refused;
        }
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile if stat.st_nlink == 1 => Status::Regular,
            FileType::Directory if stat.st_mode & 0o077 == 0 => Status::Directory,
            _ => Status::Refused,
        }
    }

    pub(super) fn list(root: PathBuf, path: &str, max_entries: u32) -> Result<Vec<Entry>, Error> {
        let dir = directory(root, &components(path)?, false)?;
        let mut out = Vec::new();
        for entry in entries(&dir)? {
            if out.len() as u64 == u64::from(max_entries) {
                return Err(Error::OverLimit);
            }
            let (name, stat) = entry?;
            if !component(&name) {
                return Err(Error::Refused);
            }
            out.push(Entry {
                name,
                status: status(stat),
            });
        }
        out.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn file(dir: &File, name: &str, flags: OFlags) -> Result<File, Error> {
        let f = File::from(
            fs::openat(
                dir,
                name,
                flags | read_nofollow_flags() | OFlags::CLOEXEC,
                FILE_MODE,
            )
            .map_err(error)?,
        );
        check_regular_owned(&f, "private file").map_err(|_| Error::Refused)?;
        if f.metadata().map_err(error)?.nlink() != 1 {
            return Err(Error::Refused);
        }
        Ok(f)
    }

    pub(super) fn read(root: PathBuf, path: &str, max_bytes: u32) -> Result<Vec<u8>, Error> {
        let parts = components(path)?;
        let (name, parents) = parts.split_last().ok_or(Error::InvalidPath)?;
        let f = file(&directory(root, parents, false)?, name, OFlags::RDONLY)?;
        read_capped(&f, u64::from(max_bytes))
    }

    fn read_capped(file: &File, max_bytes: u64) -> Result<Vec<u8>, Error> {
        let mut content = Vec::new();
        if file.metadata().map_err(error)?.len() > max_bytes {
            return Err(Error::OverLimit);
        }
        file.take(max_bytes + 1)
            .read_to_end(&mut content)
            .map_err(error)?;
        if content.len() as u64 > max_bytes {
            return Err(Error::OverLimit);
        }
        Ok(content)
    }

    struct Temp<'a> {
        dir: &'a File,
        name: String,
        staged: bool,
    }

    impl Temp<'_> {
        fn publish(mut self, name: &str, creating: bool) -> Result<(), Error> {
            if creating {
                fs::renameat_with(
                    self.dir,
                    self.name.as_str(),
                    self.dir,
                    name,
                    RenameFlags::NOREPLACE,
                )
                .map_err(error)?;
            } else {
                fs::renameat(self.dir, self.name.as_str(), self.dir, name).map_err(error)?;
            }
            self.staged = false;
            self.dir.sync_all().map_err(error)
        }
    }

    pub(super) fn read_or_create<const N: usize>(
        root: PathBuf,
        name: &str,
        fresh: impl FnOnce() -> [u8; N],
    ) -> Result<[u8; N], Error> {
        if !component(name) {
            return Err(Error::InvalidPath);
        }
        let dir = directory(root, &[], true)?;
        let read = || {
            let f = file(&dir, name, OFlags::RDONLY)?;
            let bytes = read_capped(&f, N as u64)?;
            let bytes = bytes.try_into().map_err(|_| Error::Refused)?;
            tighten_file(&f).map_err(error)?;
            Ok(bytes)
        };
        match read() {
            Err(Error::NotFound) => {}
            found => return found,
        }
        let fresh = fresh();
        match stage(&dir, &fresh)?.publish(name, true) {
            Ok(()) => Ok(fresh),
            Err(Error::Changed) => read(),
            Err(e) => Err(e),
        }
    }

    impl Drop for Temp<'_> {
        fn drop(&mut self) {
            if self.staged {
                let _ = fs::unlinkat(self.dir, self.name.as_str(), AtFlags::empty());
            }
        }
    }

    fn stage<'a>(dir: &'a File, bytes: &[u8]) -> Result<Temp<'a>, Error> {
        let name = format!("{TEMP_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let mut file = File::from(
            fs::openat(
                dir,
                name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                FILE_MODE,
            )
            .map_err(error)?,
        );
        let temp = Temp {
            dir,
            name,
            staged: true,
        };
        sync_staged(&mut file, bytes, 0o600).map_err(error)?;
        Ok(temp)
    }

    fn compare(dir: &File, name: &str, before: &Expected) -> Result<(), Error> {
        match (file(dir, name, OFlags::RDONLY), before) {
            (Err(Error::NotFound), Expected::Absent) => Ok(()),
            (Ok(f), Expected::Hash(expected)) => {
                let limit = u64::from(expected.max_bytes);
                if f.metadata().map_err(error)?.len() > limit {
                    return Err(Error::OverLimit);
                }
                let mut f = f.take(limit + 1);
                let mut hasher = hmac_sha256::Hash::new();
                let mut buf = [0; 8192];
                loop {
                    let read = f.read(&mut buf).map_err(error)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buf[..read]);
                }
                if f.limit() == 0 {
                    return Err(Error::OverLimit);
                }
                if wisp_plugin_api::hex::encode(&hasher.finalize()) == expected.sha256 {
                    Ok(())
                } else {
                    Err(Error::Changed)
                }
            }
            (Ok(_), Expected::Absent) | (Err(Error::NotFound), Expected::Hash(_)) => {
                Err(Error::Changed)
            }
            (Err(e), _) => Err(e),
        }
    }

    fn check_cap(dir: &File, cap: &CreateCap) -> Result<(), Error> {
        let mut remaining = cap.max_regular;
        for entry in entries(dir)? {
            if remaining == 0 {
                break;
            }
            let (name, stat) = entry?;
            if name.ends_with(&cap.suffix)
                && stat.is_some_and(|s| FileType::from_raw_mode(s.st_mode) == FileType::RegularFile)
            {
                remaining -= 1;
            }
        }
        if remaining == 0 {
            Err(Error::CapReached)
        } else {
            Ok(())
        }
    }

    pub(super) fn change(
        root: PathBuf,
        path: &str,
        before: Expected,
        action: ChangeAction,
        create_cap: Option<CreateCap>,
    ) -> Result<(), Error> {
        let parts = components(path)?;
        let (name, parents) = parts.split_last().ok_or(Error::InvalidPath)?;
        validate_change(name, &before, &action, create_cap.as_ref())?;
        let creating = matches!(before, Expected::Absent);
        let dir = match directory(root, parents, creating) {
            Err(Error::NotFound) => return Err(Error::Changed),
            result => result?,
        };
        let lock = file(&dir, LOCK_FILE, OFlags::WRONLY | OFlags::CREATE)?;
        lock_file(&lock, Instant::now() + LOCK_WAIT).map_err(|_| Error::Io)?;
        match action {
            ChangeAction::Write(bytes) => {
                let temp = stage(&dir, &bytes)?;
                compare(&dir, name, &before)?;
                if let Some(cap) = create_cap {
                    check_cap(&dir, &cap)?;
                }
                temp.publish(name, creating)
            }
            ChangeAction::Delete => {
                compare(&dir, name, &before)?;
                fs::unlinkat(&dir, *name, AtFlags::empty()).map_err(error)?;
                dir.sync_all().map_err(error)
            }
        }
    }
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tests;
