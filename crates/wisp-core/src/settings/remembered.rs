use std::io;
use std::path::PathBuf;

use wisp_protocol::model_hidden;
use wisp_protocol::settings::Remembered;

use crate::plan_usage::record;

#[derive(Clone, Debug, Default)]
pub struct RememberedStore {
    folder: Option<PathBuf>,
}

impl RememberedStore {
    pub fn shared(folder: PathBuf) -> Self {
        Self {
            folder: Some(folder),
        }
    }

    pub fn of(&self, account: &str) -> Option<Remembered> {
        let pair: Remembered = record::read(self.folder.as_deref()?, account)?;
        (!model_hidden(&pair.model)).then_some(pair)
    }

    pub fn keep(&self, account: &str, pair: &Remembered) -> io::Result<()> {
        let Some(folder) = self
            .folder
            .as_deref()
            .filter(|_| !model_hidden(&pair.model))
        else {
            return Ok(());
        };
        crate::secrets::create_folder(folder)?;
        record::write(folder, account, pair)
    }
}
