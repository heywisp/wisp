use std::path::Path;

pub fn shorten_path(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home.filter(|h| h.is_absolute() && h.parent().is_some()) else {
        return path.display().to_string();
    };
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}
