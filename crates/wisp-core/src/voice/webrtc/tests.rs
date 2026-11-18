use super::*;

const WAIT: Duration = Duration::from_secs(10);
const SERVER_SSRC: u32 = 7;

struct Server {
    connection: Arc<dyn PeerConnection>,
    track: Arc<TrackLocalStaticSample>,
    heard: mpsc::UnboundedReceiver<Vec<i16>>,
    said: mpsc::UnboundedReceiver<String>,
    channels: mpsc::UnboundedReceiver<Arc<dyn DataChannel>>,
    ended: CancellationToken,
}

struct ServerEvents {
    gathered: Arc<Notify>,
    heard: mpsc::UnboundedSender<Vec<i16>>,
    said: mpsc::UnboundedSender<String>,
    channels: mpsc::UnboundedSender<Arc<dyn DataChannel>>,
    ended: CancellationToken,
}

#[async_trait]
impl PeerConnectionEventHandler for ServerEvents {
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
        let heard = self.heard.clone();
        tokio::spawn(async move {
            let mut decoder = Down::new().unwrap();
            while let Some(event) = track.poll().await {
                if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                    decoder.decode(&packet.payload).into_iter().for_each(|s| {
                        let _ = heard.send(s);
                    });
                }
            }
        });
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let _ = self.channels.send(channel.clone());
        let said = self.said.clone();
        tokio::spawn(async move {
            while let Some(event) = channel.poll().await {
                if let DataChannelEvent::OnMessage(message) = event {
                    let _ = said.send(String::from_utf8(message.data.to_vec()).unwrap());
                }
            }
        });
    }
}

impl Server {
    async fn answer(offer: String) -> (Self, String) {
        let gathered = Arc::new(Notify::new());
        let (heard_to, heard) = mpsc::unbounded_channel();
        let (said_to, said) = mpsc::unbounded_channel();
        let (channels_to, channels) = mpsc::unbounded_channel();
        let ended = CancellationToken::new();
        let events = ServerEvents {
            gathered: gathered.clone(),
            heard: heard_to,
            said: said_to,
            channels: channels_to,
            ended: ended.clone(),
        };
        let mut settings = SettingEngine::default();
        settings.set_multicast_dns_mode(MulticastDnsMode::Disabled);
        let second = Some(Duration::from_secs(1));
        settings.set_ice_timeouts(second, second, Some(Duration::from_millis(200)));
        let connection = PeerConnectionBuilder::new()
            .with_media_engine(engine().unwrap())
            .with_setting_engine(settings)
            .with_handler(Arc::new(events))
            .with_udp_addrs(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))])
            .build()
            .await
            .unwrap();
        let connection: Arc<dyn PeerConnection> = Arc::new(connection);
        let track = voice_track(SERVER_SSRC).unwrap();
        connection.add_track(track.clone()).await.unwrap();
        let offer = RTCSessionDescription::offer(offer).unwrap();
        connection.set_remote_description(offer).await.unwrap();
        let answer = connection.create_answer(None).await.unwrap();
        connection.set_local_description(answer).await.unwrap();
        timeout(WAIT, gathered.notified()).await.unwrap();
        let answer = connection.local_description().await.unwrap().sdp;
        let server = Self {
            connection,
            track,
            heard,
            said,
            channels,
            ended,
        };
        (server, answer)
    }
}

fn loopback() -> Webrtc {
    Webrtc {
        ip: Some(Ipv4Addr::LOCALHOST.into()),
        ..Webrtc::default()
    }
}

type Lane = Arc<Mutex<mpsc::Receiver<AudioFrame>>>;

fn lane() -> (mpsc::Sender<AudioFrame>, Lane) {
    let (to, from) = mpsc::channel(64);
    (to, Arc::new(Mutex::new(from)))
}

async fn call(up: &Lane, id: &str) -> (Webrtc, Server, Control, mpsc::Receiver<AudioFrame>) {
    let mut leg = loopback();
    let offer = leg.offer().await.unwrap();
    let (server, answer) = Server::answer(offer).await;
    let (down, played) = mpsc::channel(64);
    let audio = LegAudio {
        call: CallId(id.to_owned()),
        up: up.clone(),
        down,
    };
    let control = leg.answer(answer, audio).await.unwrap();
    (leg, server, control, played)
}

fn tone(id: &str, seq: u64) -> AudioFrame {
    let samples = (0..FORMAT.frame_samples())
        .map(|i| ((i as f32 / 20.0).sin() * 8000.0) as i16)
        .collect();
    AudioFrame {
        call: CallId(id.to_owned()),
        seq,
        samples,
    }
}

async fn speak_until_heard(mic: &mpsc::Sender<AudioFrame>, id: &str, server: &mut Server) {
    let heard = async {
        for seq in 0.. {
            mic.send(tone(id, seq)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            if let Ok(samples) = server.heard.try_recv()
                && samples.iter().any(|s| *s != 0)
            {
                return;
            }
        }
    };
    timeout(WAIT, heard).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_flows_both_ways_through_opus() {
    let (mic, up) = lane();
    let (mut leg, mut server, _control, mut played) = call(&up, "call-1").await;

    speak_until_heard(&mic, "call-1", &mut server).await;

    let mut encoder = Up::new().unwrap();
    let played = async {
        loop {
            let samples = tone("call-1", 0).samples;
            say(
                &server.track,
                SERVER_SSRC,
                OPUS_PAYLOAD,
                &mut encoder,
                &samples,
            )
            .await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            if let Ok(frame) = played.try_recv() {
                return frame;
            }
        }
    };
    let frame = timeout(WAIT, played).await.unwrap();
    assert_eq!(frame.call, CallId("call-1".into()));
    assert_eq!(frame.samples.len(), FORMAT.frame_samples());
    leg.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_frames_flow_both_ways_on_the_events_channel() {
    let (_mic, up) = lane();
    let (mut leg, mut server, mut control, _played) = call(&up, "call-1").await;

    control.out.send("{\"type\":\"up\"}".into()).unwrap();
    let said = timeout(WAIT, server.said.recv()).await.unwrap();
    assert_eq!(said.as_deref(), Some("{\"type\":\"up\"}"));

    let channel = timeout(WAIT, server.channels.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(channel.label().await.unwrap(), "oai-events");
    channel.send_text("{\"type\":\"down\"}").await.unwrap();
    let heard = timeout(WAIT, control.frames.recv()).await.unwrap();
    assert_eq!(heard.as_deref(), Some("{\"type\":\"down\"}"));
    control.out.send("{\"type\":\"bye\"}".into()).unwrap();
    drop(control.out);
    leg.close().await;
    let said = timeout(WAIT, server.said.recv()).await.unwrap();
    assert_eq!(said.as_deref(), Some("{\"type\":\"bye\"}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_call_frees_the_microphone_and_a_remote_close_ends_the_frames() {
    let (mic, up) = lane();
    let (mut first, first_server, _control, _played) = call(&up, "call-1").await;
    first.close().await;
    timeout(WAIT, first_server.ended.cancelled()).await.unwrap();

    let (mut second, mut server, mut control, _played) = call(&up, "call-2").await;
    speak_until_heard(&mic, "call-2", &mut server).await;

    server.connection.close().await.unwrap();
    let frames = timeout(WAIT, control.frames.recv()).await.unwrap();
    assert_eq!(frames, None);
    second.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_offer_names_no_address_but_0_0_0_0() {
    let (mut leg, hidden) = match default_ip() {
        Ok(ip) => (Webrtc::default(), ip),
        Err(_) => {
            assert!(Webrtc::default().offer().await.is_err());
            (loopback(), Ipv4Addr::LOCALHOST.into())
        }
    };
    let offer = leg.offer().await.unwrap();
    leg.close().await;

    assert!(!offer.contains(&hidden.to_string()), "{offer}");

    let address = regex::Regex::new(r"\d+\.\d+\.\d+\.\d+").unwrap();
    let found: Vec<&str> = address.find_iter(&offer).map(|m| m.as_str()).collect();
    assert!(found.iter().all(|a| *a == "0.0.0.0"), "{found:?}\n{offer}");
    let candidates: Vec<&str> = offer
        .lines()
        .filter(|l| l.starts_with("a=candidate"))
        .collect();
    assert!(!candidates.is_empty(), "{offer}");
    assert!(candidates.iter().all(|c| c.contains(".local ")), "{offer}");
    assert!(offer.contains("a=sendrecv"), "{offer}");
    assert!(offer.contains("opus/48000/2"), "{offer}");
}
