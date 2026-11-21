# OpenAI connections

Wisp runs OpenAI models through its own kernel and native Responses adapter. Named endpoints select either a ChatGPT subscription or an OpenAI Platform API key. Signing in never falls back to API billing, and Wisp does not launch another agent's CLI or read its credentials.

## ChatGPT subscription

```sh
wisp login chatgpt
wisp status chatgpt
wisp account models chatgpt
wisp --endpoint chatgpt --model gpt-6.1-sol --effort low
wisp logout chatgpt
```

Login shows the authorization link, tries the browser and copies the link, then waits up to ten minutes on an IPv4 loopback callback at `http://localhost:1455/auth/callback`. The browser that opens it must reach this machine's port 1455: in VS Code over Remote-SSH the page opens by itself through VS Code's `BROWSER` helper and VS Code forwards the port (if its Ports view shows another local port, free 1455 locally and sign in again); in other terminals, `ssh -L 1455:localhost:1455 HOST`, then open the link with the terminal's link click or paste the copied link into your browser. With no link click and no clipboard, quit Wisp and run `wisp login chatgpt`, which prints the link whole. The listener is running before the link is shown; a fresh independent state and S256 PKCE verifier protect each attempt. A process already using port 1455 must release it first. No developer API key or preconfigured client secret is required.

This connection intentionally follows Manifest's subscription integration: the shared public OAuth client, `https://auth.openai.com/oauth/authorize`, `/oauth/token` and `/oauth/revoke`; inference goes to `https://chatgpt.com/backend-api/codex/responses`. Requests use the reference's static originator/User-Agent profile, `store:false`, instructions, client functions, OpenAI's hosted web search and encrypted reasoning passthrough. They omit output-token caps, previous-response IDs and optional sampling controls, so a `max_output` you set has no effect on the subscription. `prompt_cache_key` is the SHA-256 of the stable model, reasoning, tools and instructions prefix: growing conversations and agents with the same prefix share it, without account identifiers. ChatGPT subscription inference also sends this same prefix hash as the `session-id` header for cache routing; API-key requests do not send that header. Wisp sends no email, name, account-ID header, identity hint or machine identity. This is distinct from OpenAI's direct Sign in with ChatGPT integration.

Inference, hosted web search and the verbatim replay of search items were checked live on the subscription on 2026-11-02 (gpt-6.1-sol, low effort); the API-key route, account eligibility, available models and limits have **not been tested live**. `wisp account models NAME` uses Manifest's model-list route; on a subscription it prints every usable entry of the subscription's model list, hidden ones included, in the list's order. A catalog entry alone does not establish model access. Service or eligibility errors remain errors; they do not change the endpoint's billing mode.

The system keychain is selected when detected. `--store file` explicitly selects protected local storage; `--store keyring` requires the keychain. There is no silent fallback after a keychain failure. Local profile files are versioned, owner-only, checked for unsafe permissions/ownership and symlinks, and written atomically under the credential-directory lock. Keychain profiles keep their actual token bundle in a separate typed keychain item; their local locator contains only an opaque random account key and provider metadata. Existing `credentials.json` records contain only the local `oauth:` profile handle. Beside a profile, `oauth-<id>.account` holds a SHA-256 of the `id_token`'s (else the access token's) ChatGPT user and workspace IDs, used only to refuse a second name for an account already signed in; the IDs themselves are never stored. A refresh the token endpoint refuses with 400 or 401 leaves an empty owner-only `oauth-<id>.refused` mark beside the profile, so the account reads `sign-in expired` until it signs in again; saving the profile, logout and a store move remove it. A refused sign-in's grant is discarded, not revoked: nothing of it is saved. When an explicit config lives in another Git repository, ignore its generated credential/profile files there; default configuration lives outside the project.

Access tokens refresh before expiry. A ChatGPT subscription inference request refused with HTTP 401 refreshes the refused token once and retries the identical request once; if authentication still fails or the refresh is refused, sign in again with `/account`. Concurrent refusals of the same token share the replacement rather than refreshing it again. Requests coalesce refreshes, reread under the cross-process directory lock, and publish a replacement access/refresh/expiry/generation together. Refresh continues to completion if its requesting turn is cancelled. A failed local save retains the already-rotated bundle in process and, for file storage only, in a protected sidecar beside the profile (the predecessor is represented only by its SHA-256). Keychain profiles retain failed rotations in memory only, never in a plaintext fallback. Wisp retries the save; newer persisted generations supersede stale pending writes. A transient proactive refresh failure can use the current access token only while it is still valid. Terminal refresh failures require login again. Reauthorization activates a new profile after sign-in and endpoint persistence succeed. Logout clears local credentials and reports whether remote revocation was confirmed.

Subscription requests report token usage and `chat_gpt_plan` billing provenance. Known models show the API-equivalent estimate marked `~` (`~$0.129`), not a charge. `pricing = "none"` keeps tokens only. Mixed sessions retain their billing provenance. `/usage` displays reported allowance windows separately from these cost estimates.

## Voice on the subscription

A ChatGPT subscription account offers voice calls (`docs/voice.md`); an API-key account offers its own (Voice on the API). Wisp holds the call itself and presents itself as Codex here, as on the text route: the same originator and User-Agent, Codex's two route markers and Codex's voice model, `gpt-live-1-codex`, the only one the route takes. This route is private to Codex and undocumented, so it can change or close without notice (`research/codex-voice.md`).

- **The call** is one `POST https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas` with the access token, `openai-alpha: quicksilver=v2`, a fresh random `session-id` and a JSON body: the offer as `sdp`, and a `session` with the instructions, the voice, client delegation and the model. It sends no account-ID header, and no name, email, path or local address: the offer names none (`docs/voice.md`, "The media leg"). The answer is the SDP as text, and the last part of `Location` is kept as the call's id.
- **Where the token goes:** still only `chatgpt.com`. Codex also opens an event socket on `api.openai.com`; Wisp doesn't, since the call's own `oai-events` channel carries everything, so `api.openai.com` never sees the token.
- **On the channel**, a turn's end (`turn.done`) gives the learner's or the voice's whole line; a learner turn with no words is skipped, and one from another role is kept as unknown. The transcript parts before it show that someone is speaking, which holds off quiet; `input_transcript.added` is also the learner talking over the voice, which drops the rest of an answer already started (`Interrupted`, `docs/voice.md`); `session.started`, `session.updated`, `session.usage.updated`, `turn.created`, `turn.delta`, `output_audio.delta` and `delegation.context.appended` are known and skipped. `delegation.created` is a hand-off with the request's text, and `error` fails the call if it then ends on its own. A reply goes back as `delegation.context.append` on the `speakable` channel, or, for a hand-off with no delegation, as `session.context.append` on `speakable`; a note as the same on `commentary`, except a tap, which always goes as `session.context.append` on `commentary`. Each is cut into parts of at most 500 bytes, on character boundaries.
- **Hanging up** sends `{"type":"session.close"}` on the channel before the leg closes, and there is no hang-up call. A live run showed why: after a hang-up that sent nothing, the provider kept sending audio for about 29 s, while `session.close` ended a call in about 2 s.
- **Billing:** the call uses the subscription's voice allowance, booked as subscription use with its API-equivalent estimate (`docs/pricing.md`, "Voice calls").
- **Voices:** the 25 Codex offers on this route (its `RealtimeVoicesList::builtin`, `v3`, at Codex `rust-v0.166.0`), `cove` first and by default. They and the model id live in the route until a data source holds them.

A refusal never shows the provider's text. It says `Voice on this ChatGPT subscription isn't open to Wisp.` when the route doesn't take Wisp's sign-in (`no_matching_rule`), `ChatGPT refused the call.` when a sign-in refusal names the account or workspace, `ChatGPT refused the voice model.` when it refuses `session.model`, `ChatGPT says this subscription has no voice time left.` for a usage cap, at the start or in an error frame, and otherwise its kind's sentence (`ChatGPT refused the credentials.`, for example). A 429 is read as the text route reads one (`usage_limit_reached`, or the `x-codex-rate-limit-reached-type` header), with that route's kind; `usage_not_included` is a cap too. A call never ends as too long for the model: that refusal is an invalid request.

The `#[ignore]` live test `live_call_on_the_subscription` makes two short calls on the account `WISP_LIVE_ACCOUNT` names, read through Wisp's config and sign-in. It sends: two `/wham/usage` reads, one before and one after, and a token refresh if the sign-in needs one; two call requests, each with fixed placeholder test instructions, the voice `cove` and no account ID; silence as the microphone; on the first call three fixed lines (a greeting with a test word and a question, both as `speakable` session appends, and a fixed answer to any hand-off), then a hang-up that sends nothing; on the second call only `session.close`. It checks that audio arrives and the greeting is said, and reports whether the question is handed off and answered (inconclusive when nothing is said after the voice reads it back), how long the provider keeps sending after Wisp's hang-up, and whether `session.close` ends a call. It prints only counts, booleans, times and the used percentages of usage buckets that changed. It needs the owner's approval to run.

## Voice on the API

An API-key account at OpenAI's own address offers a voice that only reads: it says the brain's words and never answers on its own (`answers_alone()` is false, `docs/voice.md`). A gateway or other compatible address offers no voice. The route is `provider/openai/realtime.rs`; it shares the end of the call request, the refusal's reading and the error frame with the subscription's route, each worded with OpenAI's name, so no ChatGPT or subscription sentence reaches an API call.

- **The call** is one `POST <base>/v1/realtime/calls` (the account's address with the text route's `responses` tail replaced), with the key as a Bearer header and no other header of Wisp's, and a multipart form: the offer as `sdp`, and a `session` naming `gpt-realtime-2.1-mini`, the reader instructions, audio output only, `reasoning.effort: "minimal"`, the transcriber `gpt-realtime-whisper` (the one its price row names), with `voice.language` as its `language` when set, and the voice. It asks for far-field noise reduction (`audio.input.noise_reduction`), which filters the microphone before turn detection and the transcriber. Turn detection is `semantic_vad` at `eagerness: "high"` (it waits at most about 2 s for a learner who trails off), with `create_response: false`, so the model never answers the learner, and `interrupt_response: true`. No name, email, account id, path or local address goes out. The answer is the SDP, and the last part of `Location` is the call's id.
- **The reader instructions** are built in (`wisp_tool_text::voice::READER`), the wording a live test on the API showed reading 35 of 35 sentences without changing their meaning. The voice instructions file isn't sent or read on this route.
- **Saying:** each reply is one `response.create`, with the event id `wisp_create`, outside the conversation (`conversation: "none"`) whose input is only that text. One reply is made at a time: the next starts at the previous one's `response.done`, and a reply that comes while one is made, or while the learner speaks, waits. A note and the hang-up send nothing, since the reader keeps no context.
- **Which reply is playing:** the server plays the replies back to back from its own buffer, so the route keeps every reply made since the sound last stopped, each with its length from its audio tokens (20 a second, set at every `response.done`, so a failed reply with no sound never counts as playing), and the time the sound started (`output_audio_buffer.started`). The reply playing is the first whose summed lengths reach past the time since then, less the 0.3 s by which `speech_started` trails the learner; a reply still being made counts as playing once reached.
- **The learner speaks** (`input_audio_buffer.speech_started`): the sound stops at once, with `response.cancel` for a reply still being made and `output_audio_buffer.clear` for the buffered ones, and nothing new starts until that speech's transcript. With no words (a cough, a noise, or a failed transcription), the playing reply and those after it are said again from the start of the playing one, once a cancelled reply has ended. With words, the playing reply is named in `Interrupted`, the rest is dropped, and the words are `Heard`; words that come with no hold, such as during a re-read after a cough, first stop the sound the same way.
- **Reading:** a learner transcript (`conversation.item.input_audio_transcription.completed`) is `Heard` with its seconds as `Spent`. `response.done` gives `Spent` for its tokens, cached ones apart; a failed reply fails the call if it then ends on its own, drops the waiting replies and names itself in `Interrupted`; a cancelled one gives nothing more; any other is checked word for word against its text (lowercased, apostrophes dropped, runs of letters and digits) and, where they differ, its transcript is `Said`, so the brain reads what was heard. `output_audio_buffer.stopped` is the end of the sound (`TurnDone`). An error naming one of the route's cancels or clears is a race with a reply's end and is skipped; one naming `wisp_create` refused a reply, which ends that reply like a failed one, so the next can start; any other is reported. A reported error or failed reply is worded with its kind's sentence, then OpenAI's own message, with ids such as `proj_…` redacted and cut to 300 characters, then its code (else its type) in brackets, so the page and the `voice_call` record say what happened. A session lasts at most 60 minutes (`limit()`), so a call is reconnected before then (`docs/voice.md`, "Reconnecting"). The other frames the route knows are skipped, and the rest are counted as unknown.
- **Hanging up** is `POST /v1/realtime/calls/{call id}/hangup`.
- **Billing:** per use, to the key: tokens at the model's prices and the transcriber's seconds at its minute price (`docs/pricing.md`, "Voice calls"). Silence costs nothing.
- **Voices:** `marin` first, so a saved voice the route lacks gives `marin`, then `cedar`, `alloy`, `ash`, `ballad`, `coral`, `echo`, `sage`, `shimmer` and `verse`. They, the model, the transcriber, the turn detection and the noise reduction live in the route until a data source holds them.

A refused call, like an error frame and a failed reply, says its kind's sentence with OpenAI's name, then OpenAI's own message (identifiers redacted, cut to 300 characters), then its code in brackets. The `#[ignore]` live test `live_call_on_the_api` makes one short call with the key in `WISP_LIVE_OPENAI_KEY`, which never leaves the Bearer header: fixed test sentences as replies, its own recorded replies as the microphone, then the hang-up. It prints only counts, times, booleans, the transcripts of the fixed sentences, the session's turn detection, reasoning and transcriber, the unknown frame names and the priced cost. It costs about $0.10 and needs the owner's approval to run.

## OpenAI Platform API key

Store a key through the hidden prompt:

```sh
wisp account add openai --provider openai --auth api_key --default
wisp account models openai
wisp --endpoint openai --model gpt-6.1-sol --effort low
```

For automation, pipe the key through stdin with `--key-stdin`; never put its value on the command line. The existing `--store` choices apply. A named configuration may instead reference `key_env = "OPENAI_API_KEY"` on machines configured to read variables. `OPENAI_BASE_URL` can be selected explicitly through `url_env`; API endpoints can use a custom compatible HTTP(S) base URL. Subscription endpoints are pinned to the official route.

API requests use Bearer authentication and `/v1/responses` (`/responses` when the configured base already ends in `/v1`). Endpoint checks list models without generating a completion. Redirects are disabled. API token costs use the central pricing table, with cached reads, separate 30-minute/12-hour writes and reasoning usage counted without duplicating output tokens. Unknown usage fields remain in response diagnostics.

On the API and subscription routes, a failed or incomplete response stream can include the provider's reason in the turn error, retry line, compaction failure, or session log. Wisp redacts recognized credentials, email addresses and account identifiers and keeps at most 240 characters of the reason. The request ID is shown when the server supplies a valid `req_` ID in `x-request-id`.
Only the prose before embedded data is kept.

## Models and history

A new OpenAI endpoint defaults to `gpt-6.1-sol` when no model was explicitly configured. The OpenAI models Wisp knows, with their windows, effort levels, default level, image input and max output, are the `provider = "openai"` rows of `crates/wisp-core/data/models.toml`. Availability is account-specific.

On a ChatGPT subscription, each model the subscription's model list names takes its effort levels, default level, image input and search support from that list, as `docs/endpoints.md` ("Each account's model list") describes. Levels Wisp doesn't know, such as `ultra`, are skipped; an entry naming none of the known ones keeps the shipped row's levels, default level and image input. A usable listed default is the window before any [token-point growth](context.md#the-point-and-the-reserve). The listed maximum caps that growth; it does not replace the default unless it is smaller. Without a usable default, the shipped fallback also bounds the window, even if the model is not listed. Max output and prices still come from the shipped row, and per-message effort and tool deferral never come from the list. A static-token subscription saves none.

Interactive Wisp refreshes a ChatGPT subscription list hourly after success, or at the first background check at least about 15 minutes after failure.

Final Responses items, including exact argument strings, item/call IDs, phase, namespaces, encrypted reasoning and future fields, are stored beside their semantic display/tool projections. Replay sends each native item once. Disk resume retains these records; a different credential/profile uses portable semantic projections and does not reuse private reasoning (the replay identity: `docs/session-log.md`, "What is never logged"). Resume checks the saved billing mode before a request, including sessions with no usage yet. An unavailable or removed subscription profile never selects an API connection automatically. Older logs without plan-billing evidence read as metered for both root and child resume; no explicit `--endpoint` is needed to continue on a metered endpoint. A metered session whose endpoint is gone falls back only to a default that is also metered; otherwise it asks for an explicit `--endpoint NAME`. A refresh preserves the same profile and prompt bytes. Thinking-cut overlays remove reasoning without deleting earlier user text or tool pairs. Wisp's ordinary client-summary compaction remains available; OpenAI native compaction and Anthropic keep-warm/cache diagnostics are unavailable on this adapter. Text and PNG/JPEG images are supported, pasted or attached by a tool; the adapter sends a tool's images inside its `function_call_output` as `input_image` items, as the [Responses reference](https://developers.openai.com/api/reference/resources/responses/methods/create) documents. A tool can also attach PDF, docx, xlsx, pptx and odt files, the types of the [file inputs guide](https://developers.openai.com/api/docs/guides/file-inputs) that `view_file` can name; each goes as an `input_file` item with a data URL and a fixed name from its type (`file.pdf`, `file.docx` …), never its path. The subscription's backend documents no file support, so it takes only the types a live check showed reaching the model, PDF and docx; xlsx, pptx and odt are API-key only (`research/openai-files.md`). A file of a type the connection doesn't carry, which only a history from another connection can hold, goes as a short request-side note instead. An attachment OpenAI refuses before the response starts is withdrawn and the step sent again, as `docs/media.md` describes.

Offline fixtures cover token storage/rotation, synthetic OAuth HTTP, split SSE, native replay, tool execution, terminal failures and CLI resume. A later short acceptance check can run the interactive `wisp --endpoint chatgpt --model gpt-6.1-sol --effort low` command above and send a single short greeting after the user completes login.

The [official Responses guide](https://developers.openai.com/api/docs/guides/reasoning), [prompt-cache usage reference](https://developers.openai.com/api/docs/guides/prompt-caching), [model specifications](https://developers.openai.com/api/docs/models/gpt-6.1-sol) and [pricing](https://developers.openai.com/api/docs/pricing) describe the native API behavior. The checked Manifest source establishes the separate subscription mechanics.

## Web search

GPT models get OpenAI's hosted web search through the same `web_search` tool and setting as Claude models (`docs/endpoints.md`, "Web search"): the request carries `{"type":"web_search"}` and asks for each search's sources. While search is on, every request carries the hosted tool's own prompt, about 4,400 tokens, whether it searches or not: cached input after the first request with an API key, allowance on the subscription. Turning web search off (`/tools`, or `native_web_search = false` on the account) removes it.

Each search, opened page and search within a page is a row. A cited link shows its text and its `[n]` mark, and its URL, the one a terminal can open, is under Sources: Wisp writes no clickable links in reply text. An address too long for a source row is shortened in the ordinary Sources list; Ctrl+O shows it whole, and selecting and copying it there preserves the full address.

On API keys, and whenever the tool isn't sent, a reply that searched goes back with one line per search and without its reasoning, since replaying search items was checked only on the subscription. A search item never holds the pages it read, so on API keys a later step of the same turn, such as one after a tool call, no longer has what the search read unless the reply wrote it out.

## Failure safety and portable history

Login replacement uses a new credential namespace before switching config; a failed switch does not overwrite the working credential. New endpoints use default list pricing, while reauthorization preserves the selected pricing. Explicit new config files are allowed. OAuth store moves stage a fresh profile and handle, switch config, then retire the source; failure before the switch leaves the source untouched; ignore the whole `oauth/` folder in repositories.

Switching providers renders foreign cited text, citation sources and server-tool results as deterministic text without editing committed history. Native wrappers with existing projections do not duplicate those projections. A search item renders as one line naming its query (`[web search: "query"]`), never as its results. Foreign reasoning is not included in the text rendering, including opaque thinking/redacted-thinking blocks produced by unknown deltas and native reasoning wrappers. Nested thinking/data fields, encrypted citation indices, encrypted content, signatures and account identity are stripped; public answer text and citation URLs remain.

Non-streaming model lists, probes and provider error bodies are capped (1 MiB, with a five-second read deadline; Anthropic probes retain their stricter existing cap). OAuth exchanges already enforce the same 1 MiB size cap. Oversized replies are rejected rather than read unboundedly.

HTTP and stream errors follow the adapter contract (`docs/architecture.md`): kind, retry-after and possibly-billed come from `provider/classify.rs`, a known OpenAI error code still names the kind, and only a subscription 429 keeps its plan-limit handling.

An OpenAI response start without token counts is explicitly unavailable usage, not a zero-token metered request. A mid-stream failure with a complete reported usage object keeps the endpoint's billing mode and is priced normally; a failure without complete counts remains a lower bound. Partial Anthropic counts also remain lower bounds. This applies to retries, final failures, compaction and refresh accounting. A plan-only retry never introduces metered usage merely because the stream stopped early.

## Subscription allowance lookup

ChatGPT subscription connections can read account-wide allowance with a bodyless
GET to the fixed `https://chatgpt.com/backend-api/wham/usage` route. This is optional
telemetry, separate from session token totals and API-equivalent cost. API-key
connections make no usage request. A connected usage-display subscriber is
required; print, JSONL, replay and unattended child agents do not initiate GETs.

The request uses the existing model-list identity subset (`originator` and its
static non-personal User-Agent), bearer authentication and JSON Accept. It sends
no account-ID, cookie, profile, FedRAMP or inference session headers. Redirects are
refused, requests have a ten-second timeout and responses use the bounded body
reader. Refusal never triggers an identity fallback or raw-response diagnostic.
These internal routes are not a supported public API contract for Wisp, and
successful access would not establish provider permission.

The allowlist mapper handles primary/secondary windows in seconds from the GET,
minutes from verified response headers and `codex.rate_limits` events, and UTC
Unix reset seconds. Additional GET windows require a validated
`normal_model_slug`; unsupported or malformed optional buckets are skipped without
discarding valid default/model windows. Unrelated metered-event families are
ignored, not treated as refresh failures. Unsupported-only responses are
unavailable, never zero usage.
Passive response-header observations are captured on success and refusal.
The successful usage response's root `plan_type` may supply an optional local
`PlanUsage.plan_tier`. This is a closed `PlanTier` vocabulary with static display
labels, not arbitrary provider text. Missing, null, non-string and unknown values
mean absent; they do not invalidate otherwise valid allowance windows. A tier-only
response cannot create an allowance snapshot. Inference headers, errors and stream
events do not supply a tier. Account identifiers, arbitrary plan names, credit
balances and promotional data are still dropped.

The reported tier is ephemeral display data: it is never logged, placed in prompts
or sent in provider/tool requests. It requires no additional lookup. Partial window
observations preserve the last full snapshot's tier; a successful full refresh
replaces it, including clearing an absent or unknown tier. Credential-generation
invalidation clears the cached value. Labels are never inferred from aliases,
models or quota size.
Claude allowance fetching is not enabled by this work. Live account acceptance
and comparison with the official client require a separate authorized check.

The adapter accepts an injected receipt clock for GET, response-header and both
SSE observation envelopes. Token-refresh transport failures remain retryable
lookup errors; only actual authentication failures require a new sign-in.

Absent allowance buckets are not zero-usage meters. Empty window/header values
and zero-duration buckets with zero usage and no reset are omitted across GET,
header and event mapping. Header buckets are validated independently: a malformed
secondary bucket cannot discard a valid weekly primary bucket (and vice versa).
An entirely absent passive update is ignored rather than invalidating the cache.
The header allowlist remains percent, duration in minutes and absolute reset-at;
reset-after-seconds headers and account/credit/safety/turn-state metadata are not
read or forwarded. GET relative resets retain their existing seconds conversion.

### Reauthorization in an open session

Replacing a ChatGPT alias creates a new protected profile and provider identity.
At a safe, non-busy boundary the shared catalog replaces the connection and retires
all old handles; allowance observations are rebound to a fresh generation. Wisp
keeps committed history unchanged. Account-bound encrypted reasoning is not
replayed into the replacement account, and ordinary assistant output uses the
existing text projection instead of the old native item. The resulting request
prefix changes once, so expect a full-prefix cache miss for that replacement;
subsequent requests use the stable new projection. This is an intentional account
replacement boundary, not a promise of byte-identical prompts across accounts.
