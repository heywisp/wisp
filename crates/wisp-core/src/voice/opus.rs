use ::opus::{Application, Channels, Decoder, Encoder, Result};
use wisp_protocol::voice::AudioFormat;

pub(super) const FORMAT: AudioFormat = AudioFormat::Pcm16Mono48k;
const MAX_PACKET: usize = 4000;
const MAX_SAMPLES: usize = 5760;

pub(super) struct Up(Encoder);

impl Up {
    pub(super) fn new() -> Result<Self> {
        Encoder::new(FORMAT.sample_rate(), Channels::Mono, Application::Voip).map(Self)
    }

    pub(super) fn encode(&mut self, samples: &[i16]) -> Option<Vec<u8>> {
        self.0.encode_vec(samples, MAX_PACKET).ok()
    }
}

pub(super) struct Down(Decoder);

impl Down {
    pub(super) fn new() -> Result<Self> {
        Decoder::new(FORMAT.sample_rate(), Channels::Mono).map(Self)
    }

    pub(super) fn decode(&mut self, packet: &[u8]) -> Vec<Vec<i16>> {
        let mut samples = vec![0; MAX_SAMPLES];
        let decoded = self.0.decode(packet, &mut samples, false).unwrap_or(0);
        samples[..decoded]
            .chunks(FORMAT.frame_samples())
            .map(<[i16]>::to_vec)
            .collect()
    }
}
