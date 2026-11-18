use super::line_edit::LineEdit;
use crate::term::Key;
use wisp_protocol::{Ends, KeyProblem, Secret, check_key, check_secret};
use wisp_render::chrome::FormValue;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rule {
    #[default]
    Key,
    Code,
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretField {
    value: Secret,
    rule: Rule,
    asked: Option<Ends>,
    pasted: bool,
}

impl Default for SecretField {
    fn default() -> Self {
        Self::new(Rule::Key)
    }
}

impl SecretField {
    pub fn new(rule: Rule) -> Self {
        Self {
            value: Secret::new(""),
            rule,
            asked: None,
            pasted: false,
        }
    }
    pub fn ask(&mut self, ends: Ends) {
        self.clear();
        self.asked = Some(ends);
    }
    pub fn value(&self) -> &Secret {
        &self.value
    }
    pub fn filled(&self) -> bool {
        !self.value.expose().is_empty()
    }
    pub fn clear(&mut self) {
        self.value = Secret::new("");
        self.pasted = false;
    }
    pub fn take(&mut self) -> Secret {
        self.pasted = false;
        std::mem::replace(&mut self.value, Secret::new(""))
    }
    pub fn pasted(&self) -> bool {
        self.pasted
    }
    pub fn ends(&self) -> Option<Ends> {
        if self.pasted {
            self.asked
        } else {
            Some(Ends::default())
        }
    }
    pub fn form_value(&self) -> FormValue<'_> {
        match self.ends() {
            Some(ends) => FormValue::Ends {
                value: &self.value,
                ends,
                counted: false,
            },
            None => FormValue::Secret(&self.value),
        }
    }
    fn check<'a>(&self, text: &'a str) -> Result<&'a str, KeyProblem> {
        match self.rule {
            Rule::Key | Rule::Code => check_key(text),
            Rule::Any => check_secret(text),
        }
    }
    pub fn paste(&mut self, text: &str) -> Result<(), KeyProblem> {
        self.value = Secret::new(self.check(text)?);
        self.pasted = true;
        Ok(())
    }
    pub fn key(&mut self, key: Key) -> Result<bool, KeyProblem> {
        let mut next = self.value.expose().to_owned();
        let before = next.len();
        if !LineEdit::ANY.key(&mut next, key) {
            return Ok(false);
        }
        self.pasted = false;
        if next.len() > before {
            let checked = match self.rule {
                Rule::Key => check_secret(&next),
                Rule::Code | Rule::Any => self.check(&next),
            }?;
            if self.rule != Rule::Key && checked != next {
                return Err(match self.rule {
                    Rule::Key | Rule::Code => KeyProblem::NotPrintable,
                    Rule::Any => KeyProblem::Control,
                });
            }
        }
        self.value = Secret::new(next);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::KeyCode;
    #[test]
    fn a_paste_replaces_typing_appends_and_both_stay_bounded_and_redacted() {
        let mut field = SecretField::new(Rule::Code);
        assert!(field.paste(" synthetic-code#synthetic-state\n").is_ok());
        assert!(!format!("{field:?}").contains("synthetic"));
        assert!(field.filled());
        assert_eq!(field.paste("second\nline"), Err(KeyProblem::NotPrintable));
        assert!(
            field
                .paste(&"x".repeat(wisp_protocol::KEY_MAX + 1))
                .is_err()
        );
        assert!(field.paste("fresh-code#fresh-state").is_ok());
        assert_eq!(field.key(Key::plain(KeyCode::Char('x'))), Ok(true));
        assert_eq!(field.value().expose(), "fresh-code#fresh-statex");
        assert!(!field.pasted());
        assert_eq!(field.ends(), Some(Ends::default()));
        assert!(matches!(field.form_value(), FormValue::Ends { .. }));
        assert!(field.paste("fresh-code#fresh-state").is_ok());
        assert!(field.pasted());
        assert_eq!(field.ends(), None);
        assert!(matches!(field.form_value(), FormValue::Secret(_)));
        field.ask(Ends { start: 8, end: 4 });
        assert!(field.paste("fresh-code#fresh-state").is_ok());
        assert_eq!(field.ends(), Some(Ends { start: 8, end: 4 }));
        assert_eq!(field.key(Key::plain(KeyCode::Backspace)), Ok(true));
        assert_eq!(field.ends(), Some(Ends::default()));
        assert_eq!(
            field.key(Key::plain(KeyCode::Char(' '))),
            Err(KeyProblem::NotPrintable)
        );
        let mut any = SecretField::new(Rule::Any);
        assert_eq!(any.paste("second\nline"), Err(KeyProblem::ManyLines));
        assert_eq!(any.paste(" spaced 非ASCII \r\n"), Ok(()));
        assert_eq!(any.key(Key::plain(KeyCode::Char(' '))), Ok(true));
        assert_eq!(any.take().expose(), " spaced 非ASCII  ");
        assert!(!any.filled());
        let mut key = SecretField::default();
        for c in "key with space 非ASCII".chars() {
            assert_eq!(key.key(Key::plain(KeyCode::Char(c))), Ok(true));
        }
        assert_eq!(key.value().expose(), "key with space 非ASCII");
        assert_eq!(
            check_key(key.value().expose()),
            Err(KeyProblem::NotPrintable)
        );
        assert_eq!(key.paste("two\nlines"), Err(KeyProblem::NotPrintable));
        assert_eq!(key.value().expose(), "key with space 非ASCII");
        assert!(key.paste(&"x".repeat(wisp_protocol::KEY_MAX)).is_ok());
        assert_eq!(
            key.key(Key::plain(KeyCode::Char('x'))),
            Err(KeyProblem::TooLong)
        );
        field.clear();
        assert!(!field.filled());
    }
}
