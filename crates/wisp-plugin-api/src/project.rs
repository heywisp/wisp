pub trait Project: Send + Sync {
    fn key(&self) -> Option<String>;
}

impl Project for () {
    fn key(&self) -> Option<String> {
        None
    }
}
