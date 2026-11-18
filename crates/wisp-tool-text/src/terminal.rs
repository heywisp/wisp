use wisp_plugin_api::host::Size;
use wisp_plugin_api::types::NamedKey;

pub const DEFAULT_SIZE: Size = Size {
    cols: 100,
    rows: 30,
};
pub const MIN_COLS: u16 = 20;
pub const MAX_COLS: u16 = 500;
pub const MIN_ROWS: u16 = 5;
pub const MAX_ROWS: u16 = 200;

pub fn parse_size(text: &str) -> Option<Size> {
    let (cols, rows) = text.trim().split_once(['x', 'X'])?;
    let cols = cols.trim().parse::<u16>().ok()?;
    let rows = rows.trim().parse::<u16>().ok()?;
    Some(Size { cols, rows })
}

pub fn size_allowed(size: Size) -> bool {
    (MIN_COLS..=MAX_COLS).contains(&size.cols) && (MIN_ROWS..=MAX_ROWS).contains(&size.rows)
}

pub fn size_text(size: Size) -> String {
    format!("{}x{}", size.cols, size.rows)
}

pub fn terminal_line(size: Size) -> String {
    format!(
        "Terminal: replies are {} columns wide (wider code blocks wrap), {} rows; don't mention this.\n",
        size.cols, size.rows
    )
}

pub fn key_names() -> String {
    let mut names: Vec<String> = NamedKey::ALL.iter().map(|k| k.name().to_owned()).collect();
    names.push("F1 to F12".to_owned());
    names.push("C-a to C-z".to_owned());
    names.push("EOF".to_owned());
    names.join(", ")
}

#[cfg(test)]
mod tests;
