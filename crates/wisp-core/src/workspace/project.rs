use std::io::{self, Read};
use std::path::{Path, PathBuf};

use hmac_sha256::HMAC;
use wisp_instructions::{Files, Kind};
use wisp_plugin_api::{hex, manifest::CORE_ID};

use crate::instructions::{Disk, GITFILE, is_git_dir, pointed};
use crate::secrets::{create_atomic, random_bytes, work_tree};
use crate::session::{
    check_regular_owned, open_read_nofollow, private_dir, refuse_link, tighten_file,
};

const SECRET_FILE: &str = "project-secret";
const SECRET_BYTES: usize = 32;
type Secret = [u8; SECRET_BYTES];
const KEY_LEN: usize = 16;

pub fn project_key(root: &Path, data: &Path) -> Option<String> {
    let (_, top) = work_tree(root)?;
    let common = common_dir(&top)?;
    let secret = install_secret(&data.join(CORE_ID))?;
    let mac = HMAC::mac(common.as_os_str().as_encoded_bytes(), secret);
    Some(hex::encode(&mac[..KEY_LEN]))
}

fn install_secret(dir: &Path) -> Option<Secret> {
    private_dir(dir, None, "the project key folder").ok()?;
    let path = dir.join(SECRET_FILE);
    match read_secret(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        found => return found.ok().flatten(),
    }
    let fresh: Secret = random_bytes();
    match create_atomic(&path, &fresh, 0o600) {
        Ok(()) => Some(fresh),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read_secret(&path).ok().flatten(),
        Err(_) => None,
    }
}

fn read_secret(path: &Path) -> io::Result<Option<Secret>> {
    refuse_link(path, None)?;
    let file = open_read_nofollow(path)?;
    check_regular_owned(&file, "the project key secret")?;
    let mut raw = Vec::new();
    (&file)
        .take(SECRET_BYTES as u64 + 1)
        .read_to_end(&mut raw)?;
    let Ok(secret) = Secret::try_from(raw.as_slice()) else {
        return Ok(None);
    };
    tighten_file(&file)?;
    Ok(Some(secret))
}

fn common_dir(top: &Path) -> Option<PathBuf> {
    let marker = top.join(".git");
    let git = match Disk.metadata(&marker).ok()?.kind {
        Kind::Directory => marker,
        Kind::File => {
            let pointer = pointed(&marker, GITFILE, &Disk)?;
            let path = line(pointer.strip_prefix(b" ")?)?;
            top.join(native(path)?)
        }
        Kind::Other => return None,
    };
    let link = git.join("commondir");
    let common = if Disk.exists(&link) {
        let pointer = pointed(&link, b"", &Disk)?;
        git.join(native(line(&pointer)?)?)
    } else {
        git
    };
    let common = Disk.canonicalize(&common).ok()?;
    is_git_dir(&common, &Disk).then_some(common)
}

fn line(bytes: &[u8]) -> Option<&[u8]> {
    let end = bytes.iter().rposition(|b| !matches!(b, b'\n' | b'\r'))?;
    Some(&bytes[..=end])
}

#[cfg(unix)]
fn native(bytes: &[u8]) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(OsStr::from_bytes(bytes)))
}

#[cfg(not(unix))]
fn native(bytes: &[u8]) -> Option<PathBuf> {
    Some(PathBuf::from(std::str::from_utf8(bytes).ok()?))
}

#[cfg(test)]
mod tests;
