use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::BoxFuture;
use rtc::ice::mdns::MulticastDnsMode;
use rtc::interceptor::Registry;
use rtc::media::Sample;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_OPUS;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::error::Result;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::media_stream::{MediaStreamTrack, Track};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceGatheringState, RTCPeerConnectionState, RTCSessionDescription,
    SettingEngine,
};
use webrtc::rtp_transceiver::RtpSender;
use wisp_protocol::ErrorKind;
use wisp_protocol::voice::{AudioFrame, CallId};

use super::opus::{Down, FORMAT, Up};
use super::{Control, Leg, LegAudio, Legs};
use crate::provider::ProviderError;

#[cfg(test)]
mod tests;

const CHANNEL: &str = "oai-events";
const OPUS_PAYLOAD: u8 = 111;
const WAIT: Duration = Duration::from_secs(15);
const CONTROL_FRAMES: usize = 1024;
const FLUSH: Duration = Duration::from_millis(500);
const FLUSH_STEP: Duration = Duration::from_millis(10);

pub fn legs() -> Legs {
    Arc::new(|| Box::<Webrtc>::default())
}

#[derive(Default)]
pub(super) struct Webrtc {
    ip: Option<IpAddr>,
    connection: Option<Arc<dyn PeerConnection>>,
    media: Option<Media>,
    drained: Option<CancellationToken>,
    tasks: JoinSet<()>,
}

struct Media {
    sender: Arc<dyn RtpSender>,
    track: Arc<TrackLocalStaticSample>,
    channel: Arc<dyn DataChannel>,
    tracks: mpsc::UnboundedReceiver<Arc<dyn TrackRemote>>,
    ended: CancellationToken,
}

struct Events {
    gathered: Arc<Notify>,
    tracks: mpsc::UnboundedSender<Arc<dyn TrackRemote>>,
    ended: CancellationToken,
}

#[async_trait]
impl PeerConnectionEventHandler for Events {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if ended(state) {
            self.ended.cancel();
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let _ = self.tracks.send(track);
    }
}

impl Webrtc {
    async fn open(&mut self) -> Result<String> {
        let ip = match self.ip {
            Some(ip) => ip,
            None => default_ip()?,
        };
        let gathered = Arc::new(Notify::new());
        let (tracks_to, tracks) = mpsc::unbounded_channel();
        let ended = CancellationToken::new();
        let events = Events {
            gathered: gathered.clone(),
            tracks: tracks_to,
            ended: ended.clone(),
        };
        let mut engine = engine()?;
        let registry = register_default_interceptors(Registry::new(), &mut engine)?;
        let mut settings = SettingEngine::default();
        settings.set_multicast_dns_mode(MulticastDnsMode::QueryAndGather);
        settings.set_multicast_dns_local_ip(Some(ip));
        let connection = PeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().build())
            .with_media_engine(engine)
            .with_interceptor_registry(registry)
            .with_setting_engine(settings)
            .with_handler(Arc::new(events))
            .with_udp_addrs(vec![SocketAddr::new(ip, 0)])
            .build()
            .await?;
        let connection: Arc<dyn PeerConnection> = Arc::new(connection);
        self.connection = Some(connection.clone());
        let track = voice_track(uuid::Uuid::new_v4().as_fields().0)?;
        let sender = connection.add_track(track.clone()).await?;
        let channel = connection.create_data_channel(CHANNEL, None).await?;
        let offer = connection.create_offer(None).await?;
        connection.set_local_description(offer).await?;
        timeout(WAIT, gathered.notified())
            .await
            .map_err(io::Error::other)?;
        let offer = connection
            .local_description()
            .await
            .ok_or_else(|| io::Error::other("no local description"))?;
        self.media = Some(Media {
            sender,
            track,
            channel,
            tracks,
            ended,
        });
        Ok(offer.sdp)
    }

    async fn connect(&mut self, answer: String, audio: LegAudio) -> Result<Control> {
        let (Some(connection), Some(media)) = (&self.connection, self.media.take()) else {
            return Err(io::Error::other("no offer").into());
        };
        connection
            .set_remote_description(RTCSessionDescription::answer(answer)?)
            .await?;
        let payload = media
            .sender
            .get_parameters()
            .await?
            .rtp_parameters
            .codecs
            .first()
            .map_or(OPUS_PAYLOAD, |codec| codec.payload_type);
        let (out, says) = mpsc::unbounded_channel();
        let (heard, frames) = mpsc::channel(CONTROL_FRAMES);
        timeout(WAIT, opened(&*media.channel))
            .await
            .map_err(io::Error::other)??;
        let drained = CancellationToken::new();
        self.drained = Some(drained.clone());
        let control_frames = control(media.channel, says, heard, media.ended, drained);
        self.tasks.spawn(control_frames);
        self.tasks
            .spawn(up(media.track, payload, audio.up, audio.call.clone()));
        self.tasks.spawn(down(media.tracks, audio.down, audio.call));
        Ok(Control { out, frames })
    }
}

impl Leg for Webrtc {
    fn offer(&mut self) -> BoxFuture<'_, std::result::Result<String, ProviderError>> {
        async move { self.open().await.map_err(broken) }.boxed()
    }

    fn answer(
        &mut self,
        answer: String,
        audio: LegAudio,
    ) -> BoxFuture<'_, std::result::Result<Control, ProviderError>> {
        async move { self.connect(answer, audio).await.map_err(broken) }.boxed()
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        async move {
            if let Some(drained) = self.drained.take() {
                let _ = timeout(FLUSH, drained.cancelled()).await;
            }
            self.tasks.shutdown().await;
            self.media = None;
            if let Some(connection) = self.connection.take() {
                let _ = connection.close().await;
            }
        }
        .boxed()
    }
}

async fn control(
    channel: Arc<dyn DataChannel>,
    mut says: mpsc::UnboundedReceiver<String>,
    heard: mpsc::Sender<String>,
    ended: CancellationToken,
    drained: CancellationToken,
) {
    let _drained = drained.drop_guard();
    loop {
        tokio::select! {
            event = channel.poll() => match event {
                Some(DataChannelEvent::OnMessage(message)) => {
                    let text = String::from_utf8_lossy(&message.data).into_owned();
                    if heard.send(text).await.is_err() {
                        return;
                    }
                }
                Some(DataChannelEvent::OnClose) | None => return,
                Some(_) => {}
            },
            say = says.recv() => match say {
                Some(say) => {
                    if channel.send_text(&say).await.is_err() {
                        return;
                    }
                }
                None => return sent(&*channel).await,
            },
            () = ended.cancelled() => return,
        }
    }
}

async fn sent(channel: &dyn DataChannel) {
    while channel
        .outstanding_bytes()
        .await
        .is_ok_and(|bytes| bytes > 0)
    {
        tokio::time::sleep(FLUSH_STEP).await;
    }
}

async fn opened(channel: &dyn DataChannel) -> Result<()> {
    loop {
        match channel.poll().await {
            Some(DataChannelEvent::OnOpen) => return Ok(()),
            Some(DataChannelEvent::OnClose) | None => {
                return Err(io::Error::other("the channel closed").into());
            }
            Some(_) => {}
        }
    }
}

async fn up(
    track: Arc<TrackLocalStaticSample>,
    payload: u8,
    lane: Arc<Mutex<mpsc::Receiver<AudioFrame>>>,
    call: CallId,
) {
    let (Ok(mut encoder), Some(&ssrc)) = (Up::new(), track.ssrcs().await.first()) else {
        return;
    };
    let mut lane = lane.lock().await;
    while let Some(frame) = lane.recv().await {
        if frame.call == call {
            say(&track, ssrc, payload, &mut encoder, &frame.samples).await;
        }
    }
}

async fn say(
    track: &TrackLocalStaticSample,
    ssrc: u32,
    payload: u8,
    encoder: &mut Up,
    samples: &[i16],
) {
    let Some(packet) = encoder.encode(samples) else {
        return;
    };
    let per_ms = FORMAT.sample_rate() as usize / 1000;
    let sample = Sample {
        data: packet.into(),
        duration: Duration::from_millis((samples.len() / per_ms) as u64),
        ..Default::default()
    };
    let _ = track.write_sample(ssrc, payload, &sample, &[]).await;
}

async fn down(
    mut tracks: mpsc::UnboundedReceiver<Arc<dyn TrackRemote>>,
    lane: mpsc::Sender<AudioFrame>,
    call: CallId,
) {
    let (Some(track), Ok(mut decoder)) = (tracks.recv().await, Down::new()) else {
        return;
    };
    let mut seq = 0;
    while let Some(event) = track.poll().await {
        let TrackRemoteEvent::OnRtpPacket(packet) = event else {
            continue;
        };
        for samples in decoder.decode(&packet.payload) {
            seq += 1;
            let frame = AudioFrame {
                call: call.clone(),
                seq,
                samples,
            };
            if let Err(mpsc::error::TrySendError::Closed(_)) = lane.try_send(frame) {
                return;
            }
        }
    }
}

fn ended(state: RTCPeerConnectionState) -> bool {
    matches!(
        state,
        RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Closed
    )
}

fn engine() -> Result<MediaEngine> {
    let mut engine = MediaEngine::default();
    let codec = RTCRtpCodecParameters {
        rtp_codec: opus(),
        payload_type: OPUS_PAYLOAD,
    };
    engine.register_codec(codec, RtpCodecKind::Audio)?;
    Ok(engine)
}

fn voice_track(ssrc: u32) -> Result<Arc<TrackLocalStaticSample>> {
    let encoding = RTCRtpEncodingParameters {
        rtp_coding_parameters: RTCRtpCodingParameters {
            ssrc: Some(ssrc),
            ..Default::default()
        },
        codec: opus(),
        ..Default::default()
    };
    let id = || uuid::Uuid::new_v4().to_string();
    let stream = MediaStreamTrack::new(id(), id(), id(), RtpCodecKind::Audio, vec![encoding]);
    Ok(Arc::new(TrackLocalStaticSample::new(stream)?))
}

fn opus() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: FORMAT.sample_rate(),
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: Vec::new(),
    }
}

pub fn default_ip() -> io::Result<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9))?;
    Ok(socket.local_addr()?.ip())
}

fn broken(error: webrtc::error::Error) -> ProviderError {
    ProviderError::new(ErrorKind::Network, error.to_string())
}
