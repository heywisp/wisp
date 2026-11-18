use wisp_protocol::agents::{preset_name_rule, valid_preset_name};

pub const RESERVED_NAMES: [&str; 4] = ["main", "parent", "user", "wisp"];

pub fn check_name(name: &str) -> Result<(), String> {
    if !valid_preset_name(name) {
        let shown = if name.chars().count() <= 40 && !name.chars().any(char::is_control) {
            format!("{name:?}")
        } else {
            "this name".to_owned()
        };
        return Err(format!(
            "{shown} isn't a valid agent name: use {}",
            preset_name_rule()
        ));
    }
    if RESERVED_NAMES.contains(&name) {
        return Err(format!(
            "{name} is reserved; reserved names are {}",
            RESERVED_NAMES.join(", ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_preset_rule_and_skip_the_reserved_ones() {
        assert!(check_name("review").is_ok());
        assert!(check_name("a").is_ok());
        assert!(check_name("code-2_b").is_ok());
        assert!(check_name(&"a".repeat(32)).is_ok());
        assert!(check_name(&"a".repeat(33)).is_err());
        assert!(check_name("Review").is_err());
        assert!(check_name("2nd").is_err());
        assert!(check_name("").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name("..").is_err());
        for reserved in RESERVED_NAMES {
            let err = check_name(reserved).unwrap_err();
            assert!(err.contains("reserved"), "{err}");
        }
        assert_eq!(
            check_name("Bad").unwrap_err(),
            "\"Bad\" isn't a valid agent name: use a lowercase letter, then up to 31 of a-z, 0-9, _ and -"
        );
    }
}
