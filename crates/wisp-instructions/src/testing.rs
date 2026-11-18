use std::io;
use std::path::{Path, PathBuf};

use super::{Files, Kind, Meta, mtime};

pub(crate) struct Disk;

impl Files for Disk {
    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        let meta = std::fs::metadata(path)?;
        let kind = if meta.is_file() {
            Kind::File
        } else if meta.is_dir() {
            Kind::Directory
        } else {
            Kind::Other
        };
        Ok(Meta {
            kind,
            len: meta.len(),
            mtime: meta.modified().ok().and_then(mtime),
        })
    }

    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        std::fs::read(path).map(Some)
    }
}
