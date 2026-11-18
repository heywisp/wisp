use std::path::Path;

use crate::dirs::Dirs;
use crate::load::{Found, Loader, pair};

pub enum Install<T> {
    Added(T),
    Unchanged(T),
}

fn refused(id: Option<&str>, message: &str) -> String {
    match id {
        Some(id) => format!("{id}: {message}; nothing was added"),
        None => format!("{message}; nothing was added"),
    }
}

fn holds(folder: &Path, stem: &str, text: &str, bytes: &[u8]) -> bool {
    pair(folder).is_ok_and(|found| found == stem)
        && std::fs::read_to_string(folder.join(format!("{stem}.toml"))).is_ok_and(|t| t == text)
        && std::fs::read(folder.join(format!("{stem}.wasm"))).is_ok_and(|b| b == bytes)
}

fn remove_others(folder: &Path, keep: [&str; 2]) -> std::io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let paired = name.ends_with(".toml") || name.ends_with(".wasm");
        if paired && !keep.contains(&name.as_ref()) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub fn install<T>(
    manifest: &Path,
    wasm: &Path,
    dirs: &Dirs,
    check: impl FnOnce(Found) -> Result<T, String>,
    write: impl Fn(&Path, &[u8]) -> Result<(), String>,
) -> Result<Install<T>, String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| refused(None, &format!("{} can't be read: {e}", manifest.display())))?;
    let (found, bytes) = Loader::new(dirs)
        .check(text, wasm, None)
        .map_err(|(id, message)| refused(id.as_deref(), &message))?;
    let id = found.id.clone();
    let stem = found.sha256.clone();
    let text = found.manifest.clone();
    let checked = check(found).map_err(|message| refused(Some(&id), &message))?;
    let folder = dirs.plugins().join(&id);
    let names = [format!("{stem}.wasm"), format!("{stem}.toml")];
    let unchanged = holds(&folder, &stem, &text, &bytes);
    if !unchanged {
        write(&folder.join(&names[0]), &bytes)
            .and_then(|()| write(&folder.join(&names[1]), text.as_bytes()))
            .map_err(|e| refused(Some(&id), &format!("its files can't be written: {e}")))?;
    }
    remove_others(&folder, [&names[0], &names[1]]).map_err(|e| {
        format!("{id}: the older version can't be removed: {e}; run wisp plugin add again")
    })?;
    Ok(if unchanged {
        Install::Unchanged(checked)
    } else {
        Install::Added(checked)
    })
}
