pub fn megabytes_up(bytes: u64) -> String {
    let tenths = bytes.div_ceil(100_000);
    format!("{}.{} MB", tenths / 10, tenths % 10)
}

pub const NOT_IN_THIS_SESSION: &str = "it can't be read in this session";

pub const NO_DATA_FOLDER: &str = "this session has no data folder";

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

#[cfg(test)]
mod tests;
