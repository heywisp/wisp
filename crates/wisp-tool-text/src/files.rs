use wisp_plugin_api::types::{FileSlice, PageSlice, SecondSlice, SliceKind};

pub fn megabytes_up(bytes: u64) -> String {
    let tenths = bytes.div_ceil(100_000);
    format!("{}.{} MB", tenths / 10, tenths % 10)
}

pub const NOT_IN_THIS_SESSION: &str = "it can't be read in this session";

pub const NO_DATA_FOLDER: &str = "this session has no data folder";

pub fn carries(carried: &[&str], media_type: &str) -> bool {
    carried.iter().any(|entry| match entry.strip_suffix('*') {
        Some(prefix) => media_type.starts_with(prefix),
        None => *entry == media_type,
    })
}

pub fn uncarried(target: &str, media_type: &str, carried: &[&str], shell: Option<&str>) -> String {
    let (takes, to) = if carried.is_empty() {
        ("it takes no files".to_owned(), "to")
    } else {
        (
            format!("it takes {}", carried.join(", ")),
            "to one of those or to",
        )
    };
    let Some(shell) = shell else {
        return format!(
            "{target} is {media_type}, which this server can't carry ({takes}), so \
{NOT_IN_THIS_SESSION}."
        );
    };
    format!(
        "{target} is {media_type}, which this server can't carry ({takes}). Convert it with \
{shell} {to} text, or inspect it another way (file, xxd, strings)."
    )
}

pub fn too_large(target: &str, bytes: u64, limit: u64, then: &str, shell: Option<&str>) -> String {
    let Some(shell) = shell else {
        return format!(
            "{target} is {}; files over {} can't be attached, so {NOT_IN_THIS_SESSION}.",
            megabytes_up(bytes),
            megabytes_up(limit)
        );
    };
    format!(
        "{target} is {}; files over {} can't be attached. Make a smaller copy with {shell}, or \
extract the part you need, and {then}.",
        megabytes_up(bytes),
        megabytes_up(limit)
    )
}

fn parse_u32_digits(value: &str) -> Option<u32> {
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse().ok()
    } else {
        None
    }
}

fn parse_range(value: &str) -> Option<(u32, u32)> {
    let (start, end) = value.split_once('-')?;
    Some((parse_u32_digits(start)?, parse_u32_digits(end)?))
}

pub fn parse_pages(value: &str) -> Result<FileSlice, &'static str> {
    let bounds = parse_range(value).or_else(|| parse_u32_digits(value).map(|n| (n, n)));
    match bounds {
        Some((start, end)) if start >= 1 && start <= end => {
            Ok(FileSlice::Pages(PageSlice { start, end }))
        }
        _ => Err("`pages` must be a 1-based page or range like `5` or `14-16`"),
    }
}

pub fn parse_seconds(value: &str) -> Result<FileSlice, &'static str> {
    match parse_range(value) {
        Some((start, end)) if start < end => Ok(FileSlice::Seconds(SecondSlice { start, end })),
        _ => Err(
            "`seconds` must be a range in whole seconds like `0-30` or `120-210` with end greater than start",
        ),
    }
}

pub fn parse_sheet(value: &str) -> Result<FileSlice, &'static str> {
    match parse_u32_digits(value) {
        Some(index) if index >= 1 => Ok(FileSlice::Sheet(index)),
        _ => Err("`sheet` must be a 1-based worksheet number (1 or greater)"),
    }
}

pub fn read_only(slice: &FileSlice) -> String {
    match slice {
        FileSlice::Pages(PageSlice { start, end }) if start == end => {
            format!("read only page {start}")
        }
        FileSlice::Pages(PageSlice { start, end }) => format!("read only pages {start}-{end}"),
        FileSlice::Seconds(SecondSlice { start, end }) => {
            format!("read only seconds {start}-{end}")
        }
        FileSlice::Sheet(index) => format!("read only sheet {index}"),
    }
}

pub fn slice_kind_for(media_type: &str) -> Option<SliceKind> {
    match media_type {
        "application/pdf"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.template"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/vnd.oasis.opendocument.text"
        | "application/vnd.oasis.opendocument.presentation" => Some(SliceKind::Pages),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.oasis.opendocument.spreadsheet" => Some(SliceKind::Sheet),
        _ => match media_type.split_once('/') {
            Some(("audio" | "video", subtype)) if !subtype.is_empty() => Some(SliceKind::Seconds),
            _ => None,
        },
    }
}

pub fn slice_supported(
    media_type: &str,
    slice: SliceKind,
    carried: &[&str],
    supported: &[SliceKind],
) -> bool {
    carries(carried, media_type)
        && slice_kind_for(media_type) == Some(slice)
        && supported.contains(&slice)
}

pub fn slice_refusal(
    target: &str,
    media_type: &str,
    slice: SliceKind,
    supported: &[SliceKind],
    shell: Option<&str>,
) -> Option<String> {
    let expected = slice_kind_for(media_type);
    if expected != Some(slice) {
        return Some(wrong_slice_kind(target, media_type, slice, expected, shell));
    }
    (!supported.contains(&slice)).then(|| unsupported_slice(target, slice, supported, shell))
}

pub fn text_with_slice(target: &str, slice: SliceKind, shell: Option<&str>) -> String {
    let kind = slice.name();
    let hint = shell.map(|shell| format!(", or read a line range with {shell} (sed -n)"));
    format!(
        "No file was sent: {target} is a text file, so `{kind}` does not apply. Omit `{kind}` to read the start{}.",
        hint.as_deref().unwrap_or_default()
    )
}

fn wrong_slice_kind(
    target: &str,
    media_type: &str,
    slice: SliceKind,
    expected: Option<SliceKind>,
    shell: Option<&str>,
) -> String {
    let slice = slice.name();
    if let Some(expected) = expected {
        let expected = expected.name();
        return format!(
            "No file was sent: {target} is {media_type}, which takes `{expected}`, not `{slice}`."
        );
    }
    let retry = if let Some(shell) = shell {
        cut_with_shell(slice, shell)
    } else {
        format!("Omit `{slice}` to send and store the whole file.")
    };
    format!(
        "No file was sent: {target} is {media_type}, which can't be sliced with `{slice}`. {retry}"
    )
}

fn unsupported_slice(
    target: &str,
    slice: SliceKind,
    supported: &[SliceKind],
    shell: Option<&str>,
) -> String {
    let slice = slice.name();
    let takes = if supported.is_empty() {
        "this model does not slice files".to_owned()
    } else {
        let names: Vec<_> = supported.iter().map(SliceKind::name).collect();
        format!("it takes {}", names.join(", "))
    };
    let retry = if let Some(shell) = shell {
        cut_with_shell(slice, shell)
    } else {
        format!(
            "Omit `{slice}` to read the whole file, or switch to a model that supports `{slice}`; either retry sends and stores the whole file."
        )
    };
    format!(
        "No file was sent: {target} can't be sliced with `{slice}` on this model ({takes}). {retry}"
    )
}

fn cut_with_shell(slice: &str, shell: &str) -> String {
    format!(
        "Cut out only the part you need with {shell} and view that copy, or omit `{slice}` to send and store the whole file."
    )
}

#[cfg(test)]
mod tests;
