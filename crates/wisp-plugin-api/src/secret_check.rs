#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Safe,
    Refused,
}

pub trait SecretCheck: Send + Sync {
    fn check(&self, text: &str) -> Verdict;
}

impl SecretCheck for () {
    fn check(&self, _text: &str) -> Verdict {
        Verdict::Refused
    }
}
