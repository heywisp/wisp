pub const UNAVAILABLE: &str = "renaming this chat is unavailable here";

pub trait Chat: Send + Sync {
    fn rename(&self, name: String) -> Result<String, String>;
}

impl Chat for () {
    fn rename(&self, _name: String) -> Result<String, String> {
        Err(UNAVAILABLE.to_owned())
    }
}
