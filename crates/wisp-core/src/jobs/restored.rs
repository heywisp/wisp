use std::io::Read as _;
use std::path::Path;

use crate::call::{LineIndex, LineState, Ring};

pub fn file_state(path: &Path) -> std::io::Result<LineState> {
    let mut file = std::fs::File::open(path)?;
    let mut index = LineIndex::default();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut offset = 0u64;
    let mut last = None;
    loop {
        let read = file.read(&mut buffer)?;
        let Some(chunk) = buffer.get(..read).filter(|c| !c.is_empty()) else {
            break;
        };
        index.record(offset, chunk);
        offset += chunk.len() as u64;
        last = chunk.last().copied();
    }
    let complete = index.newlines() + u64::from(last.is_some_and(|b| b != b'\n'));
    Ok(LineState {
        complete,
        complete_bytes: offset,
        held: None,
        ring: Ring::default(),
        path: Some(path.to_path_buf()),
        file_lines: complete,
        index,
        finished: true,
        problem: None,
    })
}
