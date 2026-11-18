use crate::MediaError;

const HEADER: usize = 12;

const CHUNK_HEADER: u64 = 8;

const FRAME_HEADER: u64 = 16;

const ANMF: &[u8] = b"ANMF";

fn le_u32(bytes: &[u8], at: u64) -> Option<u64> {
    let at = usize::try_from(at).ok()?;
    let field: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u64::from(u32::from_le_bytes(field)))
}

pub(crate) fn check_chunks(bytes: &[u8]) -> Result<(), MediaError> {
    let length = u64::try_from(bytes.len()).map_err(|_| MediaError::Malformed)?;
    if bytes.len() < HEADER {
        return Err(MediaError::Malformed);
    }
    let declared = le_u32(bytes, 4).ok_or(MediaError::Malformed)?;
    let end = CHUNK_HEADER + declared;
    if end > length {
        return Err(MediaError::Malformed);
    }
    let start = u64::try_from(HEADER).map_err(|_| MediaError::Malformed)?;
    walk(bytes, start, end, true)
}

fn walk(bytes: &[u8], start: u64, end: u64, top_level: bool) -> Result<(), MediaError> {
    let mut at = start;
    while at < end {
        if at + CHUNK_HEADER > end {
            return Err(MediaError::Malformed);
        }
        let size = le_u32(bytes, at + 4).ok_or(MediaError::Malformed)?;
        let chunk_end = at + CHUNK_HEADER + size;
        if chunk_end > end {
            return Err(MediaError::Malformed);
        }
        if top_level && fourcc(bytes, at) == Some(ANMF) {
            if size < FRAME_HEADER {
                return Err(MediaError::Malformed);
            }
            walk(bytes, at + CHUNK_HEADER + FRAME_HEADER, chunk_end, false)?;
        }
        at = chunk_end + (size & 1);
    }
    Ok(())
}

fn fourcc(bytes: &[u8], at: u64) -> Option<&[u8]> {
    let at = usize::try_from(at).ok()?;
    bytes.get(at..at.checked_add(4)?)
}

#[cfg(test)]
mod tests;
