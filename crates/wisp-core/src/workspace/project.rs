use std::path::{Path, PathBuf};

use hmac_sha256::HMAC;
use wisp_instructions::{Files, Kind};
use wisp_plugin_api::{hex, manifest::CORE_ID};

use crate::instructions::{Disk, GITFILE, is_git_dir, pointed};
use crate::plugin::host::private_files::read_or_create;
use crate::secrets::{random_bytes, work_tree};

const SECRET_FILE: &str = "project-secret";
const SECRET_BYTES: usize = 32;
const KEY_LEN: usize = 16;

pub fn project_key(root: &Path, data: &Path) -> Option<String> {
    let (_, top) = work_tree(root)?;
    let common = common_dir(&top)?;
    let secret = read_or_create(
        data.join(CORE_ID),
        SECRET_FILE,
        random_bytes::<SECRET_BYTES>,
    )?;
    let mac = HMAC::mac(common.as_os_str().as_encoded_bytes(), secret);
    Some(hex::encode(&mac[..KEY_LEN]))
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
