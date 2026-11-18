use std::fmt;
use std::path::PathBuf;

#[derive(Clone)]
pub struct Dirs {
    pub data: PathBuf,
    pub cache: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl Dirs {
    pub(crate) fn plugins(&self) -> PathBuf {
        self.data.join("plugins")
    }

    pub(crate) fn compiled(&self) -> Option<PathBuf> {
        self.cache.as_ref().map(|cache| cache.join("wasmtime"))
    }
}

impl fmt::Debug for Dirs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.env.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("Dirs")
            .field("data", &self.data)
            .field("cache", &self.cache)
            .field("env", &names)
            .finish()
    }
}
