mod attach;
mod earlier;
mod opus;
mod service;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
mod webrtc;

use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::{Mutex, mpsc};
use wisp_protocol::voice::{AudioFrame, CallId};

use crate::provider::ProviderError;

pub use self::webrtc::{default_ip, legs};
pub use attach::{AudioLane, attach};
pub use earlier::Earlier;
pub use service::{CallRecord, QUIET, Voice};

pub struct LegAudio {
    pub call: CallId,
    pub up: Arc<Mutex<mpsc::Receiver<AudioFrame>>>,
    pub down: mpsc::Sender<AudioFrame>,
}

pub struct Control {
    pub out: mpsc::UnboundedSender<String>,
    pub frames: mpsc::Receiver<String>,
}

pub trait Leg: Send {
    fn offer(&mut self) -> BoxFuture<'_, Result<String, ProviderError>>;

    fn answer(
        &mut self,
        answer: String,
        audio: LegAudio,
    ) -> BoxFuture<'_, Result<Control, ProviderError>>;

    fn close(&mut self) -> BoxFuture<'_, ()>;
}

pub type Legs = Arc<dyn Fn() -> Box<dyn Leg> + Send + Sync>;
