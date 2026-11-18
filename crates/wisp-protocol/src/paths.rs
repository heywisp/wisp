use std::path::Path;

const SHORT_ID: usize = 13;

pub const INVALID_SESSION_ID: &str = "invalid session id";

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

pub fn short_id(session: &str) -> &str {
    session.get(..SHORT_ID).unwrap_or(session)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids_hold_the_whole_millisecond_stamp() {
        assert_eq!(
            short_id("01a167d6-5e2a-7b31-9c4d-2f6e8a1b3c5d"),
            "01a167d6-5e2a"
        );
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn paths_under_home_are_shortened_by_whole_components() {
        let table: &[(&str, Option<&str>, &str)] = &[
            ("/home/al", Some("/home/al"), "~"),
            ("/home/al/", Some("/home/al"), "~"),
            (
                "/home/al/Projects/Wisp",
                Some("/home/al"),
                "~/Projects/Wisp",
            ),
            ("/home/al/x", Some("/home/al/"), "~/x"),
            ("/home/al/x", Some("/home//al"), "~/x"),
            ("/home/al/x", Some("/home/./al"), "~/x"),
            ("/home//al/x", Some("/home/al"), "~/x"),
            ("/home/alice/x", Some("/home/al"), "/home/alice/x"),
            ("/tmp/wisp-1", Some("/home/al"), "/tmp/wisp-1"),
            ("/home/al/x", None, "/home/al/x"),
            ("/x", Some("/"), "/x"),
            ("/home/al/x", Some("//"), "/home/al/x"),
            ("/home/al/x", Some(""), "/home/al/x"),
            ("rel/x", Some("rel"), "rel/x"),
        ];
        for &(path, home, shown) in table {
            assert_eq!(
                shorten_path(Path::new(path), home.map(Path::new)),
                shown,
                "{path} under home {home:?}"
            );
        }
    }
}
