pub const SEARCH_CAPABILITY: &str = "search";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Tool,
    Skill,
    Document,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Skill => "skill",
            Self::Document => "document",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [Self::Tool, Self::Skill, Self::Document]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub summary: String,
    pub text: String,
}

pub trait Index: Send + Sync {
    fn entries(&self) -> Vec<Entry> {
        Vec::new()
    }

    fn open<'a>(&'a self, name: &'a str) -> crate::BoxFuture<'a, Result<String, String>> {
        Box::pin(std::future::ready(Err(format!(
            "nothing named `{name}` was found"
        ))))
    }

    fn changed(&self) {}
}

impl Index for () {}
