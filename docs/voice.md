# Voice

A voice call is a live spoken conversation between a client and a provider's voice model, held by the kernel. The protocol has one contract for every client, a browser page and a native app alike: a few commands and events on the usual channels, and the call's audio on a lane of its own. The types live in `wisp_protocol::voice`.

## Commands and events

A client sends `ClientCommand::Voice { command }`, and the kernel answers with `AgentEvent::Voice { event }`.

| Command | What it does |
|---|---|
| `start { call, format }` | Opens a call. `call` is a `CallId` the client picks; `format` is the audio the client sends and plays. |
| `stop { call }` | Hangs up. |
| `note { call, text }` | A tap on the screen: it reaches the brain, and the voice knows it without saying it. |

| Event | When |
|---|---|
| `started { call }` | The call is up and the voice listens. |
| `ended { call, outcome, seconds, cost }` | The call is over. |

Every command and event names its call, so a second call or a second client needs no new type.

## The audio lane

Audio never rides the event stream, which is logged, mirrored and sent to every screen. It moves on a lane of two channels of `AudioFrame { call, seq, samples }`, one each way, beside the command and event channels, so a busy event stream can't stall the sound.

`AudioFormat` names the samples. Today it has one value, `pcm16_mono_48k`: signed 16-bit PCM, mono, at 48 kHz, in frames of 20 ms (`FRAME_MS`), which is 960 samples (`AudioFormat::frame_samples`). The kernel holds the only Opus codec, so a client plays and records plain PCM and nothing resamples. A compressed format for a client far from the kernel can be added as a new value without breaking the others.

## How a call ends

Every call ends in exactly one `ended`, whatever ends it. Its `outcome` says how:

- `completed` with an `end`: `hangup` (the client's `stop`, a new `start`, or the session closing), `idle` (two minutes of quiet), `client_gone` (the client closed its command channel) or `remote` (the provider closed the call);
- `failed` with an `ErrorKind` and a sentence for the user. After an error frame, a call that ends on its own (`idle` or `remote`) is `failed` with that error, while a `hangup` or `client_gone` stays `completed`.

The sentence is the voice layer's own when no signed-in account offers voice (`No signed-in account offers voice.`), when two or more do and none is set (`No voice account is set in Settings.`), when the one set is signed out, unavailable or offers no voice (`The voice account in Settings can't make calls.`), or, on a route that sends it (the subscription's), when the instructions file can't be read (`The voice instructions file can't be read.`); a route words its own failures, its refusal of the call and its error frames (the subscription's sentences are in `docs/openai.md`, "Voice on the subscription"; the API's, "Voice on the API", are its kind's sentence, then OpenAI's own message, identifiers redacted and cut, then its code in brackets), with its kind's sentence where it has none of its own; and every other failure gets its kind's sentence with the route's provider, `ErrorKind::sentence`, the same table the terminal shows for a failed turn (`The connection to ChatGPT failed.`, for example). A `stop` while the call's hand-off is being answered also cancels that turn (`cancel`), and a hand-off still queued at the end is withdrawn.

`seconds` is the call's length on the kernel's clock, from `started` to `ended`, since connected time is what is metered, and `cost` is its price as an `AccountSpend`: `usd`, with `plan_usd` the part a subscription covers. A call that never started ends with 0 seconds and no cost.

Before an account is set up, a `start` ends at once as `failed` with the set-up sentence, and `stop` and `note` are refused like any other command. A `stop` for a call that isn't live is ignored; a `note` reaches the brain whatever call it names. A session started without the voice layer, as every `wisp` session is, drops a voice command unanswered; no client of it sends one.

## Reconnecting

A call may outlive one connection to the provider. A route names the longest a connection may last with `limit()` (none by default; the API's is 60 minutes, OpenAI's limit for one Realtime session) and says with `busy()` when the voice is speaking or about to, or the learner is mid-speech. From a minute before the limit the layer reconnects at the first moment the route isn't busy, and at the latest 10 seconds before it. A connection that closes on its own (`remote`) after being up at least a minute is reconnected too; one that closes sooner ends the call as before. Reconnecting hangs the old connection up and dials a new one, with a fresh route and leg, for the same account, instructions, voice and language and the same call id, so the client sees nothing. What the brain says meanwhile is held and said on the new connection, after the note of the lesson's last lines a call opens with, and the lesson's state (the open hand-off, its lines and delegations, a hold) carries over. A dial that fails ends the call with its failure. Each connection is its own `voice_call` record (`docs/session-log.md`), priced on its own seconds and use: one replaced at the limit is `completed` with `hangup`, one that closed on its own its error, else `remote`, and a failed dial, or a call that ends while one dials, a record like a call that never started. The call's one `ended` has its whole length and the sum of the records' costs. `voice.language` (an ISO 639-1 code) goes to the API's transcriber as `audio.input.transcription.language`; unset, it detects the language, and the subscription's route doesn't take one.

## Where the voice layer sits

The voice layer is one task between the client and the root agent, `wisp_core::voice::attach(commands, events, voice)`. It wraps the command and event channels `start_root` returns and gives back new ones and the audio lane, so every session started with it (`learn`'s) has it and `start_root` doesn't change. It takes `ClientCommand::Voice` and passes every other command on unchanged, in order; it passes every event on unchanged, in order, reading the root agent's turns, inputs, text blocks and tool calls as they go by. Closing either side reaches the other: when the client drops its command channel the agent's closes, and the client's events end only when the agent's do.

A provider offers voice through `Provider::voice()`, a `VoiceRoute` that knows its provider's call address, sign-in, event names and voices; it defaults to none. The kernel holds the media leg (`voice::Leg`: offer, answer, the audio lane and the control frames), so a route only turns control frames into shared `LiveEvent`s and replies and notes (`LiveSay`) into frames. One frame can carry several events, so `read` gives a list: among them `Send` is a frame the route sends back at once (its next reply, a cancel), and `Spent` is what a reply or a transcript used, which the call keeps to price it (`docs/pricing.md`, "Voice calls"). A route whose voice only reads what it is given, and never speaks unasked, says so with `answers_alone()` (true by default). OpenAI offers two routes: the ChatGPT subscription's voice, which answers alone, and the API's, which only reads (`docs/openai.md`). Audio never passes through the voice layer's task, and the layer never waits on a leg: the frames it sends go into the leg's unbounded queue (`Control.out`), so a stalled leg can't hold up the events every screen gets. A call that ends while it is still opening is hung up once its opening finishes.

At each `start` the layer asks the kernel for its settings (`DescribeSettings` with its own request id, whose answer reaches no client) and takes `voice.endpoint`, `voice.voice`, `voice.language` and `voice.instructions` from the resolved values, every layer included, so a change, a voice pick saved mid-lesson included, applies at the next call and never to the live one. The kernel answers mid-turn, so a call that opens while the brain thinks isn't held up; it waits only behind a compaction queued ahead of it or a save in flight, which is rare. The voice account is looked up among the session's accounts, a saved voice the route doesn't offer gives way to the route's first, as no saved voice does, and the instructions file is read whole, only by a route that sends it.

## The media leg

`wisp_core::voice::legs()` gives the kernel's WebRTC leg (webrtc-rs 0.20.3, with libopus for the codec), and the start step (`wisp-start`) hands it to the voice layer. The route never sees audio: it only gets the leg's offer and gives back the provider's answer.

- **The offer** holds one send-and-receive Opus track at 48 kHz and a data channel named `oai-events`, and is returned once ICE gathering is complete, so nothing trickles. It uses no STUN or TURN server and binds two UDP sockets: one for the media, on the IPv4 address of the default interface (found by a route lookup that sends nothing), and the mDNS responder's on port 5353. The media socket's host candidate goes out as an mDNS name (`<random>.local`), so the offer holds no local address: the only address it names is `0.0.0.0`. The responder answers only a multicast query for that name, on the home network, with the default interface's address, and announces nothing unasked. The track's stream and track ids are random each call. The provider reaches the leg at the address its connectivity checks come from.
- **The answer** is applied, and the call starts once the channel opens; a channel that doesn't open within 15 seconds fails the call as `network`.
- **Audio up**: each lane frame of this call is encoded as one Opus packet (mono, voice mode) and sent on the track; frames of another call are dropped. The leg holds the lane's receiver from the answer until it closes, so the next call's leg takes it only then.
- **Audio down**: each RTP packet is decoded into frames of at most 20 ms, numbered from 1, and put on the lane without waiting: a frame the client has no room for is dropped, since late audio is no use.
- **Control frames**: each message on the channel is a frame for the route, and the route's frames are sent once the channel is open.
- **Hang-up** puts the route's hang-up frames on the channel (`LiveSay::HangUp`; a route with none sends nothing), waits up to 0.5 s for the channel to send them and the provider to acknowledge them, then stops the leg's tasks and closes the peer connection. A call whose channel is already gone skips the frames and the wait. When the connection is disconnected, fails or closes, or the channel closes, the frames end, which the layer reads as the provider's end (`remote`).

libopus is built from source with the crate, so building `wisp` needs `cmake` and a C compiler, and on Linux also `make`, for jemalloc.

## The hand-off

Every learner turn reaches the brain. When the voice hands a request over (a delegation), the learner taps or the learner speaks, the layer queues one message for the brain, `ClientCommand::queue` with an id of its own, holding a line for each thing said or tapped since the last hand-off, oldest first, then the requests:

| Label | What follows it |
|---|---|
| `Heard:` | the learner's words, as the voice heard them |
| `Said:` | the voice's words |
| `Tapped:` | the text a tap sent (`note`), cut to 200 characters; a tap within a second of the one before is dropped |
| `Cut off:` | the part of the brain's last answer the learner talked over |
| `Asked:` | each request the voice handed over, always last |

A hand-off carries at most the last 20 `Heard:`, `Said:` and `Cut off:` lines and the last 8 `Tapped:` lines; the call's record keeps the rest. Two waits gather a hand-off's lines:

- a delegation waits up to half a second for the learner's next line, which a route may send just after it, so the hand-off carries the question with its request;
- a learner line with no delegation waits up to 3 s (`REPLY_WAIT`, the one to tune) for the voice: its own line or a tap cuts the wait to half a second, and a delegation hands off at once with its `Asked:` line. The voice's own answer can't be held back on this route (Wisp sends no `ack_filler`, and the voice speaks only as its instructions say), so the brain reads it as a `Said:` line and builds on it;
- on a route that only reads, a learner line hands off at once, since no line of the voice's own will come.

A learner line that arrives while the open hand-off's delegation is still unanswered is that question's late transcript: it joins the next hand-off and starts none. One hand-off is open at a time: delegations, taps and learner lines that arrive meanwhile join the next, which goes once the turn has ended and no wait runs. The turn that takes the hand-off (`input_delivered`) answers it:

- the turn's first text block is said aloud sentence by sentence as it is written, each part as the answer to the newest delegation the hand-off carried; each older one is told quietly `The next answer covers this question too.` with the first part. A sentence ends at `.`, `!`, `?` or `…`, with any closing quote or bracket, followed by white space, or at a line break; the rest is said when the block closes. A retried step that discarded the block while it was being said has its new first text block said whole;
- when the learner speaks over the voice once the answer has started (`Interrupted`), the rest of the block being said is dropped, and each cut adds one `Cut off:` line to the next hand-off: the part the route was saying, or, when the route can't tell, the start of the text not yet sent while the block is still written, and none when nothing went unsaid. Before the first part the answer is still said whole, and only a part the route names gets the line;
- each successful tool call is told quietly as `<tool> finished`, and the first text block after it is said the same way, as later parts of the same answer, so the brain can lead with a sentence, draw, then explain with the board up; a cut drops the rest of the block being said, and the first block after a later successful call is still said. Other text in that turn, such as a block after a failed call, stays in the brain's history and reaches the voice nowhere (`NextSaid` in `voice/service.rs` holds this rule for the voice and for what the layer remembers);
- when the hand-off carries a delegation, or the route only reads, a turn that fails or is stopped has the voice say `I couldn't get an answer just now. Please ask me again.`; after parts were said, so does every turn that ends any other way than completing with `end_turn` (stopped, failed, or cut off by the output limit, such as after a lead sentence while the page is written), with or without a delegation, and a turn that completes with no text tells each delegation quietly `Nothing to say to this one. Stay quiet and keep listening.`

A hand-off the inbox holds after another turn failed is withdrawn, the voice says the same line when it carries a delegation or the route only reads, and its lines join the next hand-off. A tap is also told to the voice quietly as `Tapped: <text>`; a hand-off that only taps or learner lines started has no delegation, so its answer is said unasked, and, on a route that answers alone, it gets that line only after parts already said, when its turn stops mid-block. With no live call, each tap goes to the brain as its own queued message, `Tapped: <text>`.

The lines a call's record keeps because no hand-off of that call carried them (those after the last hand-off, and a withdrawn one's, not those past the limits) stay with the layer, at most the hand-off's limits of them: they go first in the next hand-off, of a later call or of a tap with no call, and start none themselves.

The layer remembers the lesson: the last 40 lines of the hand-offs the brain took (its own `voice-` and `tap-` ids, read from `input_accepted` once `input_delivered` names them, so a withdrawn one isn't remembered), then the brain's last answer, its turn's first text block and the first block after each successful tool call joined by spaces, as a `Said:` line when it came after them. A resumed session hands in the same from its log (`Voice.earlier`), and its `tap-` ids go on past the highest in the log. When a call opens, the voice is told them quietly as one note, `Earlier in this lesson:` and the lines (a commentary note with no delegation, cut into parts by the route); nothing is told when there is nothing yet.

Quiet is two minutes with no speech and no tap, counted from the last of the brain's turn end, a tap, and the route's latest sign of speech (a turn's end, or a transcript part while someone speaks); it never runs while a brain turn does.

## The call's record

Each connection of a call leaves a `CallRecord` ("Reconnecting"): its outcome and seconds, every line no hand-off carried while it was up (those past the limits and a withdrawn hand-off's, and on the call's last record those after the last hand-off, which the next hand-off may still carry), each unknown control frame type the route couldn't name, once with a count, and, once it started, its account, model, billing and price (`docs/pricing.md`, "Voice calls"). The service sends it to the root agent, which books it in the session's spend and writes it to the log as `voice_call`.

## Settings

| Setting | What it holds |
|---|---|
| `voice.endpoint` | The account that holds voice calls; its choices are the accounts whose provider offers voice. Unset, a call uses the only signed-in account that offers voice (`SettingsSnapshot::voice_account`, which learn's Settings shows too). |
| `voice.voice` | The voice that speaks; its choices come from the account's provider. |
| `voice.language` | The language the learner speaks, an ISO 639-1 code for the API voice's transcriber; unset, it is detected from speech. |
| `voice.instructions` | The file of instructions the subscription's voice gets when a call opens (the API voice uses its own), relative to the folder of the file that set it, as a preset's instructions are: `config.toml`'s, or the flow's folder for the flow's `settings.toml`. |

`docs/settings.md` lists their values and when a change applies.
