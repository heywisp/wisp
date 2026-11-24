# Model pricing

A text model's prices live in its row of the shipped model file, `crates/wisp-core/data/models.toml`: `price_input`, `price_output`, `price_cache_read` and optional `price_input_audio` in USD per million tokens, and `long_prompt`, an optional second tier for models priced by prompt length. Rows with `bill_tool_use_prompt = true` also count Gemini's `toolUsePromptTokenCount` as input, including it in the prompt length used for that tier; the flag defaults to false. Each row's `provider` names a `[providers.NAME]` table, which holds the cache-write multipliers of the input price (`write_5m`, `write_1h`, `write_30m`, `write_12h`) and the price of a web search (`web_search_price`). `wisp-core`'s `pricing` module is the only code that turns them into rates: `model_info` gives each priced ID and alias a `ModelInfo`. A row's `min_cacheable`, the minimum cacheable prompt length, is read by the cache-miss check. An `effort_variant` row inherits its family row's price fields as a whole when it sets none of `price_input`, `price_output`, `price_cache_read`, `price_input_audio` or `long_prompt`, and inherits `bill_tool_use_prompt`, `min_cacheable`, `reply_cap` and `turn_form` individually when unset.

A cache write costs the tier's input price times its TTL's multiplier from the provider's table; 5-minute and 1-hour writes are priced separately. When a response does not split its writes by TTL, they are priced at the session's TTL.

## Prompt-length tiers

A model with `long_prompt` is billed at the higher tier when the request's prompt is **over** `prompt_tokens_over` tokens. The prompt counts input tokens, cache reads and cache writes (`Usage::prompt_tokens`). A request at exactly the threshold uses the base tier. Above it, every token in the request, output included, is billed at the higher tier, even when part of the prompt was a cache hit. Each request is priced on its own: when a turn reports per-iteration usage, each iteration picks its own tier.

A cancelled turn's usage may be partial. Since a larger prompt never lowers the price, its estimate stays a lower bound.

One rule marks every request's cost a lower bound, a turn's step, a retry, a compaction (background ones included) and a keep-warm refresh alike: its usage has no price, or it failed possibly billed (with usage, or flagged by the provider) and its counts weren't reported complete. A cut background compaction with partial counts is a lower bound like a cut step. The flag is logged with the cost (`compaction_prepared`, `context_compacted` and `compaction_failed` included), so a resumed session's total stays marked as at least, and quitting while a side request is out, after it was sent, leaves it marked too.

A request that sends audio or video to a model with a higher audio input price also gets the lower-bound mark when usage is reported. Wisp prices those tokens at the text rates, cached input included. A request that only sends attachment notes or text does not get this mark.

## Tools

Tool definitions and the tool-use system prompt are billed as ordinary input tokens and are already included in each response's `usage`, so nothing is added by hand. Every tool that's on sends its definition in the cached prefix, which is written once and then read from the cache, and real costs always come from the usage the API reports. With tools on, the tool-use system prompt adds 286 input tokens for `tool_choice: auto` on Opus 5.5 and Sonnet 5.5 ([tool use overview, pricing](https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview#pricing)).

A turn that runs tools sends one request per step. Each request is priced on its own, with its own prompt-length tier, and reported in a `StepEnded` event. The turn's cost is the sum of its steps' costs in step order. A step whose usage is unknown or only partial (a cancel or a failed request that may still have been billed) marks the turn's cost as a lower bound.

### Starting tools

`preload = ["core"]` starts all permitted Wisp tools; `["none"]` starts only
`search` and `use`. A child also keeps `report`. Omitted client tools are found
and run with `use`, without a cache rebuild. The two search definitions and
search's fixed instruction are part of the stable prefix from the first request.

The ignored `live_preload_*` tests in `crates/wisp-cli/tests/it/live_tool_changes.rs`
measure provider-reported prompt tokens and the cost of N deferred declarations.
Token counts are not yet measured; run the ignored tests with the project's test
credentials to collect them. Local serialized client-tool arrays, without native search:

| Provider | core tools | none tools | core bytes | none bytes |
|---|---:|---:|---:|---:|
| Claude | 17 | 2 | 27,500 | 990 |
| OpenAI | 17 | 2 | 28,027 | 1,052 |
| Gemini | 17 | 2 | 27,663 | 1,033 |

These are bytes, not token estimates (`ask_user`'s definition is 2,695 bytes after adding `withdraw` and non-blocking wording, +566 bytes over its earlier 2,129 bytes). Preload-held tools are not declared deferred:
the earlier single-tool measurement below found 93 extra prefix tokens for one
deferred declaration. Tools switched off retain their existing deferral behavior.
The N-tool live check is not yet measured.

### Tools found during a turn

A native search hit adds its definition at the conversation tail, not in the
cached tool array. Its cost is the appended definition (or a small reference
when already deferred), plus any provider-side tail rewrite on the next step.
Finding the same loaded tool again adds no second definition. Compaction
restates it only if the compacted view no longer offers it; keep-warm and
summary forks copy their actual accepted request without another addition.
A model without after-results proof gets parameters in the search result and
runs the tool through `use`, with no cache rebuild. No after-results token or
cache-read measurements are claimed until the bounded ignored live probes
succeed. Preload-held tools keep their existing no-deferred-declaration policy.

### Tool changes in a running session

A tool turned off or on in a running session (`docs/tools.md`, "Switching tools in a running session") keeps the `tools` array and the system prompt, so the cached prefix is read as before. On a model with `inline_tools` the change adds a few input tokens to the turn: Anthropic sends a removal or an addition by name (tens of tokens) or a whole definition by value (the definition's size); listed ChatGPT subscription and official OpenAI API pairs send definitions in an `additional_tools` item and off/on developer notes. It is written at its own request's tail, and the next request may write some of it again (see "After a tool message at the tail" below); on other models the `core/tools` note line in the user message costs the same way. A tool kept off still sends its definition in the array, read from the cache. A child agent's prefix follows its own preload over its parent's permitted tools when it was spawned; without a preset override it uses the session's preload. Each start set has its own child prefix, written once.

A tool turned on where the pair can't take tool-change blocks (a model without `inline_tools`, or an endpoint that refused the beta or item) rebuilds the prefix once at the turn's tool moment: a new sorted `tools` array and system prompt, written whole at the turn's first request, priced as any cache rebuild at that request's TTLs (`docs/tools.md`, "When a change can't go inline"). Changes that wait for the same opening share that one write. A trial of the beta refused mid-request costs two writes: the resend without blocks, then the rebuild at the next opening that brings back the by-value tools the resend took off the wire. The same resend then rebuild applies when a listed ChatGPT subscription or official OpenAI API pair refuses a sent `additional_tools` item. Turning `web_search` off or on, where it changes the pair's server tools, is one such rebuild's write; where the pair sends no search either way it costs nothing.

A resume after such a change, or after a tool or plugin was switched in the saved config while Wisp was closed, keeps the cache too: a full-screen root session resumes on its logged system prompt and `tools` array and sends the difference as the same blocks or note at its first turn (`docs/tools.md`, "Resuming a session"). An upgraded tool definition goes as one by-value addition, and a changed base prompt as one text block of its size. A difference a pair can't take as a delta (a tool to add on a model without `inline_tools`) pays one rebuild at the first opening, as a live change does; a copy that fails its digest check, a jsonl or `--tools none` session, or a child that can't adopt its copy (a fallback endpoint whose server tools differ) pays the one rebuild it paid before.

Measured with the live test (`crates/wisp-cli/tests/it/live_tool_changes.rs`, `docs/tools.md`) on 2026-10-30, at `--effort low`, for a few cents in all:

- **Step 0**, the beta's first appearance: two requests over one warm prefix, the second with the `inline-tools-2026-09-15` beta. On `claude-haiku-5-5` the first request (prompt 10,521) read 10,517, and the second (prompt 10,521) read 10,517. On `claude-sonnet-5-5` the first (prompt 10,521) was cold, read 0, and the second (prompt 10,521) read 10,517. Adding the beta kept the cache on both models, so neither a mid-session switch nor resume treats it as a change.
- **Steps 1–5**, one session on `claude-haiku-5-5`, one change per turn: step 1 removes `view_file` by name, step 2 adds `show_diff` by value, step 3 removes it by name, step 4 adds it by value again, step 5 adds `view_file` back by name. "Short" is how far a request's read fell below what the request before it read and wrote. Three runs gave 0/27/0/0/0, then 27/665/28/741 for steps 2–5, then 0/27/665/28/0 tokens short, the last with tool messages of 25, 663, 26, 642 and 339 tokens. Step 1, the session's first change, read exactly what was cached where it was measured (11,468 of 1,095 read and 10,373 written, in the first run). A request after a removal or a by-name change fell 0–28 short, and one after a by-value addition 0 or about 665–741 short: about the size of that addition's tool message.
- **After a tool message at the tail.** Wisp's bytes are a strict append there (the system prompt, tools, top-level `cache_control`, betas and every earlier message are unchanged, checked by `each_request_after_a_tool_system_message_strictly_appends_to_the_one_before`), but after a request that ends in a tool-change system message, the next request may write again up to that request's own new tail, on the API's side. Every shortfall above stayed within the previous request's write, and history read before was never lost: each request read at least what the one before it read. In the tail test the explicit breakpoint on the user message's last block read exactly (0 short) at nearly the same cost, $0.001823 against $0.001649–0.001833 for the automatic one. Wisp keeps automatic caching, so there is one request shape; a later change could revisit explicit tail markers if tool changes become frequent.
- **Step 6**, an addition by value of a tool already in the array under the same name with another description, on `claude-haiku-5-5`: the request read 10,517 of the 10,517 cached before it, so the cache was kept, and the next request, which reads through that system message, read 10,530 (at least the 10,517 the addition's request read).
- **A tool declared deferred** (`docs/tools.md`, "Tools off at start: declared deferred"), measured by `live_a_deferred_tool_costs_no_more_than_declaring_it_and_surfaces_from_the_cache` on `claude-haiku-5-5` on 2026-10-30, for $0.0049 in all: three sessions over one padded user message, each a cold request then a warm one, with `view_file` declared and `show_diff` left out (A), declared normally (B) or declared with `defer_loading` (C). The warm requests had prompts of 10,777 (A), 11,377 (B) and 10,870 (C) tokens, every one 4 uncached, 13 written and the rest read ($0.000113, $0.000119 and $0.000114). So the deferred declaration adds no uncached input and costs less per request than declaring the tool: it adds 93 tokens to the cached prefix, against 600 for the whole definition. C's cold request read nothing of A's entry, so the deferred entry is part of the prefix's identity, which is why the set is chosen once and never changes, except that a rebuild, which writes the prefix anew anyway, drops from it the tools its array now declares normally. Surfacing it by name read all 10,866 tokens cached before it (2 uncached, 644 written: the definition, expanded where the addition sits), so the cache was kept. A same-name addition by value of a deferred tool, whose schema adds a required parameter, was accepted and used: `live_a_same_name_addition_by_value_replaces_a_deferred_declaration` (one uncached request, $0.000143) got a call carrying the new parameter, which is why an upgraded deferred tool goes by value (`docs/tools.md`, "Resuming a session").

Message threads (`docs/context.md`, "Message threads") were checked live on 2026-11-03 on the API and a subscription: the `message-threads-2026-08-12` beta doesn't key the cache, and a plain request reads what a continue wrote, so a continue costs the same as the full request it stands for. A later round of the same check, on `claude-haiku-5-5`, showed that the `fallbacks` field and the `server-side-fallback-2026-07-01` beta don't key the cache: a request with neither read the whole prefix one with both had written. That model has no server-side fallback, so the check shows only that the field and beta are accepted and leave the cache key alone, not what a request served by a fallback model reads.

## Web search

Anthropic's native web search costs `[providers.anthropic]`'s `web_search_price` per search, on top of tokens ([pricing, web search](https://platform.claude.com/docs/en/about-claude/pricing); [web search tool](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool)). The search results count as ordinary input tokens, in the request that searched and in every later one. A failed search isn't billed. For a Claude chat the native tool runs only in
its search-only `model.ask`, not in the cached chat request. The ask's tokens,
automatic 5-minute writes and searches go into `model_asked`; only its report
and numbered source list join later chat requests as an ordinary tool result.

OpenAI's hosted web search costs `[providers.openai]`'s `web_search_price` per call on the API, for every model, on top of tokens: only `search` actions are billed, not opened pages or searches within a page, and the search content is billed as input tokens at the model's rates ([pricing, built-in tools](https://developers.openai.com/api/docs/pricing#built-in-tools); [web search guide](https://developers.openai.com/api/docs/guides/tools-web-search)). The adapter takes the count from the response's `tool_usage.web_search.num_requests`, else counts the completed `search` actions; on the subscription it stays a `~` estimate.

Gemini's search-only `model.ask` is logged and priced in `model_asked`, at its own model's token rates, plus `[providers.gemini].web_search_price` for each non-empty `webSearchQueries` query Google reports, not per function call ([Google Search pricing](https://ai.google.dev/gemini-api/docs/google-search)). Subscription accounts show the same list-price estimate; use of their model allowance is not measured. The free monthly queries are not modelled. A fallback to Gemini 2.5 is estimated per query although Google bills it per prompt.

If a search-only reply breaks off, queries reported before the failure and their fees stay in `model_asked`. Its tokens follow the rule for any ask that breaks off, and its cost is shown as a lower bound. Each refused ask is a separate `model_asked` record, priced only from any usage it reports; a refusal before any usage has no usage or estimated price. A refused side model is tried once on the chat's one-shot model (a level-in-ID chat's low ID when served, else the chat's model) before search is switched off.

- **Usage.** The final `message_delta`'s `usage.server_tool_use.web_search_requests` becomes `Usage::web_search_requests` (written in logs and events only when non-zero). `message_start`'s usage carries no search count.
- **Price.** `pricing::estimate_usd` adds `pricing::web_search_usd` (the provider's `web_search_price` per search) to a request's token price, so every caller that prices a `Usage` includes it. When a reply reports per-iteration usage, each iteration is priced with the searches it reports, and any searches only the top-level usage counts are added once.
- **`pricing = "none"`.** No price is shown, as for tokens; the count is still in the usage.
- **Cache writes across search iterations.** In a reply that searches, the final usage's `cache_creation_input_tokens` covers every iteration, but its `cache_creation` breakdown (`ephemeral_5m_input_tokens`, `ephemeral_1h_input_tokens`) stays at `message_start`'s figures: the streamed `message_delta` carries no breakdown, measured direct and through a gateway. The live run on 2026-10-25 (Haiku 5.5) reported 11,635 written against a breakdown of 158 + 2,898, and 17,891 against 165 + 14 for a reply with two searches. The writes the breakdown doesn't name come from the automatic breakpoint the API puts on each server tool result, which always uses the 5-minute TTL ([tool use with prompt caching](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-use-with-prompt-caching)), so the Anthropic adapter counts them at 5m whatever the session's `cache_ttl`:
  - **Without iterations**, the remainder of the total over the breakdown counts at 5m.
  - **With iterations** (each carrying its own usage), the first iteration's remainder counts at the request's tail TTL, since its writes are the request's own breakpoints, and a later iteration's at 5m. A later iteration that wrote tokens but names neither figure counts them all at 5m. The top-level 5m and 1h figures become the sums of the iterations' figures. If the first iteration wrote tokens and names neither figure, summing would undercount, so the top level keeps `message_start`'s breakdown (the first pass's) and counts its remainder, the later iterations' writes, at 5m; an iteration that wrote nothing doesn't count here.
  - **Otherwise** (no server tool, or a reply with a `fallback_message` iteration, whose top level covers only the last attempt and so is never replaced by a sum), any remainder counts at the tail TTL, since those writes land after the last explicit breakpoint.

  Every consumer sees the normalised usage: the per-step, per-turn and per-session cost, the log and `/context`. A reply with iterations is priced from its iterations, so the summed top level changes only the logged split. `cache_read_input_tokens` has no breakdown and is taken from the final usage as reported, so it needs no correction.
- **Retries.** A failed attempt usually reports no final usage, so Wisp counts its searches itself: the `SearchStarted` events of `search` actions in that attempt (not opened pages or searches within a page) plus the open calls its request carried (a results request after a reply with a search, or the continuation of a paused reply), less the searches the attempt's own usage already reports (a stream that fails after OpenAI's `response.completed`). A search that started and then failed counts too, though a failed search isn't billed; the attempt's price is still a lower bound, since its tokens go unreported. When the priced endpoint may have billed the attempt, the provider's search price per counted search is added to the turn's retry spend, so it counts against the retry budget, and it is always added to the next attempt's estimate. A closed call is marked `possibly_billed` when a request that carried it completed, or failed or was stopped after it may have been billed; a call no request carried never is.

## Rejected requests

When a gateway refuses a beta feature (per-message effort or thinking binding controls) before the reply starts, Wisp re-sends the step once without it. The refused request is treated as unbilled, and the step is priced from the re-send alone. Any surcharge a gateway adds for refused requests is outside Wisp's view and not in the estimate.

Live results (2026-10-23, `claude-sonnet-5-5`, effort `low`):

- **Checks 1 and 2 (per-message effort keeps the cache; a model switch rebuilds it): pass.**
  - First run: turn 1 wrote 3593 tokens to the 1h cache (4 uncached input tokens after the last breakpoint). After the change to `medium`, turn 2 read 3593 from cache and wrote 12. Cost $0.0149: the cache was cold, and a bug in the test's assertion made the run fail.
  - Re-run on the warm cache: turn 2 read 3605 from cache and wrote 0. The switch to `claude-haiku-5-5` succeeded and wrote 3615 to cache, a fresh write as expected. Cost $0.0015.
- **Check 3 (a rendered marker, one turn on `claude-sonnet-5`, then back): pass.** `input_transformations` was `[]` on Sonnet 5 and `[]` on the return, so R24 is closed and the `drop_block` fallback stays off. Cost about $0.016 (estimated; the output wasn't kept), plus an accidental second run of about $0.002.
- **Check 4 (gateway pair states): pass.** Ran `claude-sonnet-5-5` through a gateway on 2026-10-23.
  - An effort change on the Unknown pair reported `KeptIfPassedOn`.
  - The marker turn completed in one request, with no correction and no re-send, and the pair became Proven for per-message effort.
  - The binding-controls beta also became Proven in one request, with `input_transformations` `[]`.
  - The gateway passes both betas through. Cost $0.00028.
  - An earlier run cost $0.0145 without recording the pair state, because the padded 3620-token prompt was written to the 1h cache through the gateway. The check now sends a tiny prompt below the cacheable minimum.

## Gateways

With `pricing = "list"` (the default) an endpoint's turns are estimated at the
model provider's list prices, or at the prices you set ([Your own prices](#your-own-prices)),
which a gateway may not charge: it may add a markup or a discount, bill or
reject the 1h cache TTL, and meter `count_tokens`. None of that is visible to
Wisp. With `pricing = "none"` in `config.toml`, that endpoint's turns show
tokens only, and the session total becomes a lower bound, as for a model with
no price. The log still records the estimate: every priced record carries the
served `model` and, on such an endpoint, `list_usd`, the list-price estimate
that would have been `cost_usd`, for statistics only. Resume, the totals and
every display ignore it (`docs/session-log.md`).

Every endpoint switch is priced as a cache rebuild at the target model's
rates, even when the model stays the same, since Wisp can't know whether two
endpoints share a cache. A switch that reaches a warm cache simply costs less
than shown.

## Repeated cache misses

The repeat notice's figure (`CacheMissRepeated.usd`, `docs/context.md`, "Cache misses") is the run's extra cost over full hits: for each miss in the run, the tokens it should have read (the previous request's first-pass prompt less what it read) priced as a cache write minus a cache read at the served model's rates and the session's tail TTL (`pricing::prefix_quote`), summed. It is absent on an endpoint with `pricing = "none"`, for a model with no price, or once any miss in the run had none.

## Keep-warm refreshes

A keep-warm refresh (`docs/agents.md`, "Keep-warm") is priced from the usage it got, like any request: mostly a cache read of the prompt, one output token and a few uncached input tokens; one that missed is priced as the write it made. Its cost goes into `session_usd` (and, for a child, its spend and the tree's `agents_usd`), never into a turn's total, and a stream cut before its end makes it a lower bound. `CacheRefreshed.cost` carries it, with `turn_usd` holding the refresh's own price, and the strip's total (`docs/agents.md`, "The strip") moves with it while idle. An idle spell costs at most `agents.warm_refreshes` refreshes; one avoided cache rewrite on Opus 5.5 pays for about 39 of them.

Naming a chat (`session_named`, `docs/session-log.md`) is priced and counted the same way: its cost goes into `session_usd` and the account in use, never a turn, `SessionNamed.cost` carries it, and a call cut after it was sent is a lower bound.

## Plugin model requests

A plugin's `model.ask` is priced as a chat step: the reply's total usage and its
iterations, including searches counted only in the total. Its `model_asked`
record and `ModelAsked.cost` add to the session and open turn, without changing
the turn's served model. Resume restores the cost and subscription split. A sent
request dropped before completion is a lower bound; prompt and reply text are not logged.

## Voice calls

A voice call (`docs/voice.md`) is priced by its connected seconds, or, for a model priced by tokens, by what it used (below): `pricing::voice_usd` looks up the call's model in `VOICE`; the voice prices stay in `pricing` until voice models have rows of their own. `gpt-live-1` costs **$0.05 a minute**, billed by the second, with 15 seconds charged when the call is created ([OpenAI, WebRTC](https://developers.openai.com/api/docs/guides/voice-webrtc)), so a call shorter than 15 seconds costs $0.0125. `gpt-live-1-codex`, the subscription's voice model, is priced the same as its API equivalent; its billing is the subscription's, so it shows as subscription use (`~`), never a charge. A model with no row leaves the call unpriced, a lower bound, and so does a voice account set to `pricing = "none"`, whose log keeps the estimate as `list_usd`, as a text step's does. Wisp books a call only once it started: one that ended while it opened, its session already created, books nothing, though the API still charges that session's 15 seconds; booking them is left to the API's voice route.

A model priced by tokens, `gpt-realtime-2.1-mini`, is priced from what the call used instead, and its connected seconds cost nothing. The route reports each reply's and each transcript's use as a `Spent`: text and audio input, cached input and output tokens (an input count leaves out its cached tokens), and the seconds its transcriber heard. `voice_usd` prices each at the row's two `Rates`, text ($0.60 in, $0.06 cached, $2.40 out per million tokens) and audio ($10, $0.30, $20), and the transcriber's minute price ($0.017 for `gpt-realtime-whisper`), with no floor. The row names its transcriber (`voice_transcriber`), so the route runs the one it is priced for.

The service prices each connection of a call as it closes (`docs/voice.md`, "Reconnecting"), each with its own 15-second charge on a per-minute model, and puts the call's total in its `ended` event; the root then books it in `session_usd` and the voice account's spend, never a turn's, and writes `voice_call` (`docs/session-log.md`), which `--resume` counts again.

## Plugins

World 0.2.0's `pricing` import (`wit/v0.2.0/pricing.wit`, `docs/plugins.md`) gives plugins this table without copying it:

- `estimate(usage)` prices a usage for the session's model with `estimate_usd`, prompt-length tiers and web searches included, and returns micro-dollars rounded half-up from the f64 estimate (`pricing::micros`), so an estimate whose f64 value is 0.5 µ$ becomes 1; a true half that f64 lands just under rounds down. Rewind quotes and compaction costs round with the same rule. The usage splits cache writes into `cache-write-5m` and `cache-write-1h`, so each is priced at its own multiplier. It returns `none` for a model with no row or an endpoint with `pricing = "none"`.
- `spend()` returns this agent's turn and session cost so far in micro-dollars, from the same tally as `StepEnded` and `Cost` (child agents' `Cost.agents_usd` is not included), with `turn-at-least` and `session-at-least` set where the figure is a lower bound. A figure is `none` on an unpriced endpoint and while nothing has been priced yet, so an unknown model never reads as $0.
- There is no rates call, and no plugin sees the endpoint, its URL or a key. To price a cached prefix, a plugin estimates a read and a write of its tokens, putting the write under `cache-write-1h` for the system and tools prefix and under the session's `cache_ttl` (readable with `settings.get` when the manifest lists it, though it may differ in a child agent or until a change applies) for the conversation.

## Output tokens while streaming

The TUI's working line shows an estimate of the reply text so far, `↓ ≈N tokens`: ⌈characters / 4⌉ over the text deltas streamed in the turn; thinking deltas, a summary written for display, aren't counted (`docs/context.md`, "The status row"). The exact count comes from the turn's usage and appears on the `Done` line.

Live result (2026-10-23, `claude-sonnet-5-5`, effort `low`, direct API key; `crates/wisp-cli/tests/it/live_working_line.rs`; cost $0.0138):

- **`message_delta` events: one per message, at the end**, just before `message_stop`. No mid-stream output count arrives, so the line keeps the estimate and its `≈`, and there is no `AgentEvent::OutputTokens`.
- **The estimate runs low:** 994 estimated against 1,300 exact output tokens, about 24% under.
- A short turn read `Kindling… (1s · esc to stop)`, then `Done · 1.6s · ↓ 4 tokens · $0.000336 · cached 0%`.
- The long turn hit that run's `max_tokens` of 1300 and ended with `Cut off at the output limit`, never `Done`. The check now gives the long turn a 2,400-token limit and asks for about 600 words; a reply cut at the limit now ends its turn with an error (`docs/architecture.md`, "Reply cut at the limit"), which fails the check.

## Images

An image costs ⌈width / 28⌉ × ⌈height / 28⌉ visual tokens, billed as input tokens, after the API's resize to the model's tier: 1,568 px and 1,568 tokens on the standard tier, 2,576 px and 4,784 tokens on the high-resolution tier (Claude 4.7 and later) ([vision](https://platform.claude.com/docs/en/build-with-claude/vision)). Wisp normalises each image to that size before sending (`docs/media.md`), so `wisp_media::tokens` of the saved size is the estimate. An 800 × 800 PNG is 29 × 29 = 841 tokens. Images are part of the cached prefix like any other block; the prompt-caching docs say adding or removing images anywhere in the prompt affects the message blocks.

The measurement of what an image turn does to the cache is `crates/wisp-cli/tests/it/live_images.rs` (`claude-sonnet-5-5`, effort `low`, at most $0.04, about $0.02): a text turn over a cacheable system prompt, a turn with an 800 × 800 PNG, then a text turn. It prints each turn's cache reads and writes, its prompt size and the image estimate, then a verdict. Run it with credentials in the environment:

```sh
heavy cargo test -p wisp-cli --test it -- live_images:: --ignored --nocapture
```

Live result (2026-10-23, `claude-sonnet-5-5`, effort `low`, direct API key; total $0.0098 of the $0.04 cap):

| Turn | Input | Cache read | Cache write | Output | Prompt | Image estimate | Cost |
|---|---|---|---|---|---|---|---|
| 1 (text) | 4 | 0 | 1711 (5m 39, 1h 1672) | 4 | 1715 | 0 | $0.00683 |
| 2 (800×800 PNG) | 4 | 1711 | 891 (5m 891) | 14 | 2606 | 841 | $0.00255 |
| 3 (text) | 4 | 2602 | 57 (5m 57) | 5 | 2663 | 0 | $0.00046 |

- **Adding an image doesn't rebuild the cache.** Turn 2 read all 1711 cached tokens before the image, and turn 3 read the image turn's prefix (2602 tokens).
- **The image turn added 891 prompt tokens:** the image estimate of 841, plus the stamp, the text and turn 1's reply.
- **So Wisp shows no re-cache warning** when an image is attached, and the footer has no `image added` miss label (plan §10.3). The `≈` token estimate before sending stays.

## Current table

The prices are the rows of `crates/wisp-core/data/models.toml`; a row with no `price_input`, such as `claude-mythos-preview`, has no price. Retired models are not served on the Claude API. Fast mode, batch, and `inference_geo: "us"` (1.1×) prices are not modelled.

## Your own prices

A `config.toml` model table can set `price_input`, `price_output` and `price_cache_read`, in US dollars per million tokens, under `[models."ID"]` for every account or `[endpoints.NAME.models."ID"]` for one, or through `--set` and `wisp config set`. They resolve like a context window: `--set` for the account, `--set` for the model, the account's table, your table, then the shipped row. The most specific place that sets any of the three gives the whole price, flat, with no prompt-length tier; the cache-write multipliers and the search price stay the shipped row's provider's, else the account's provider's. A place that sets only some of the three gives no price: the model shows no cost there, and a start note and `wisp doctor` name the missing keys. Every cost Wisp shows and logs uses the price that applies on the account it runs on; `pricing = "none"` still turns them off.

## Adding or changing a model

1. Take every number from the sources below, never from a version number or a neighbouring model. If one is missing, leave the model out: an unknown model shows no cost rather than a wrong one.
2. Set the row's prices, and its `long_prompt` when the price depends on prompt length; a price needs its `price` page in the row's or its provider's `sources`, which the file is refused without.
3. Add a test for any new pricing rule, and update the provider's `price` source and its date.

## Sources

The price, multiplier and cache-minimum pages are each provider's `sources` in the data file, the Anthropic price page with its checked date. Checked 2026-10-22:

- Current model IDs: https://platform.claude.com/docs/en/about-claude/models/overview.md
- Dated IDs and aliases before the 4.6 generation: https://platform.claude.com/docs/en/about-claude/models/model-ids-and-versions.md
- Model status: https://platform.claude.com/docs/en/about-claude/model-deprecations.md

## Subscription equivalents

ChatGPT, Anthropic and Antigravity subscription usage uses the same price tables, with billing provenance retained separately from the estimate. Turn costs, session totals, quit summaries and agent lists mark the API-equivalent amount with `~` (`~$0.129`), an estimate rather than a charge, from one formatter (`wisp_protocol::marked_amount`); a lower bound adds a trailing `+` (`~$0.129+`), and real API spend, a gateway's included, stays bare. Mixed billing is shown as separate metered and subscription amounts (for example, `$12.30 · ~$4.10 subscriptions`), rather than claiming the entire total is charged or covered. The one exception is the strip's total (`docs/agents.md`, "The strip"), which sums them. The status line shows no cost. `pricing = "none"` remains tokens-only. Plan usage limits are outside this accounting.

OpenAI cache-rebuild estimates use ordinary input pricing, without Anthropic's cache-write premium.

Provider-specific cache-write multipliers live in each `[providers.NAME]` table of the data file. OpenAI cache-rebuild, compaction-head and picker estimates use ordinary input pricing rather than Anthropic's 5m/1h write premiums; explicit OpenAI 30m/12h writes retain their own rates. The CLI passes these model rates to the pure renderer, so endpoint switches use the target model's rates.

Subscription estimates are marked in rebuild/switch/retry dialogs and context report as well as turn/session summaries. Context reports distinguish the current endpoint's estimate provenance from historical spend. `pricing = "none"` suppresses context dollar figures, including historical compaction amounts on that endpoint. Generic plugin-authored dollar spans describe plugin-defined amounts, not necessarily model inference, and are not relabelled as subscription usage.

## Subscription allowance display

`/usage` and the status line show account-wide percent used, independently of
session token totals or API-equivalent costs. These are not charges or a budget.
Child report mail and notices mark subscription estimates as `~$X subscription`; mixed totals separate metered spend from subscription estimates, except the
strip's one `total`.
No exhausted meter alone stops inference, purchases credits, or switches accounts.

The live `/usage` page counts each request, the root's and every child's, in the block of the
account it was sent on; spend with no account named or on an account not listed counts in
the total only (`docs/commands.md`, "`/usage [NAME]`"). Its total keeps subscription estimates
and API spend separate, as every total but the strip's does; missing prices and mixed-billing
splits never become exact charges. It does not change pricing, durable cost events or child
reports.
