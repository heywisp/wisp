# Session log format

Every `wisp` session is recorded as an append-only log, for history and audit
now and for resume and observability later. Pass `--no-log` to turn it off.

## Where

`$XDG_DATA_HOME/wisp/sessions/<session-id>.jsonl`, by default
`~/.local/share/wisp/sessions/`. The directory is mode 700 and each file 600.
The welcome panel in transcript mode (ctrl-o; ctrl-o twice once anything can be opened, `docs/commands.md`) shows the file's path.

Files are created new and never through a symlink. Reopening an existing file
refuses a symlink or anything that isn't a regular file owned by the current
user, and tightens its mode to 600.

Unless chosen with `--session-id`, the session id is a UUIDv7, so those file
names sort by start time.

A running session holds an exclusive lock on its file (`flock` on Linux),
taken when the file is opened and released when the process ends, killed or
not. Wisp opens the file once and writes through the handle it locked, since
two opens of one file compete for the lock even within one process. A second
Wisp can't open a session that is open. Wisp unlocks the file before closing
it (when it switches sessions, ends, or only checked whether a session was
open), rather than leaving that to the close: an `flock` lock belongs to the
open file, and a child process started in the same instant (a bash call, say)
holds a copy of the handle until it runs its program, which would keep the
lock past the close. On a filesystem without file locks
(some network or FUSE mounts), a new session says so once at start and runs
unlocked.

A resumed session opens the existing file only, never creating one, so a
mistyped ID leaves nothing behind. It takes the lock before reading, so no
other Wisp can write between the read and the first append. A last line cut
off by a crash is dropped (the file is truncated to the last full line)
through the locked handle, on the first write, so a resume that is refused
leaves the log unchanged; `session_resumed.truncated_bytes` records how much
was dropped.

`ended/<root-session-id>.jsonl` beside the sessions folder keeps finished helpers' spend, row facts and links, with directory mode 700 and file mode 600. These facts are derived from the logs and safe to delete; a resume rebuilds them. A crash during replacement can leave one `.wisp-tmp-*` file in `ended/`.

## The spend record

Each request also appends one JSON object to `spend/YYYY-MM-DD.jsonl` beside
`sessions/` (by default `~/.local/share/wisp/spend/`). The folder is mode 700,
files 600; symlinks and non-regular files are refused. The date is the writer's
local day, or UTC when its zone is unknown. There is no import of old logs:
recording starts when each chat next starts or resumes after the update.

A line holds `at` (UTC timestamp), `session` (the writing agent), `account`,
`model`, `usage` (including subscription billing), `cost_usd`, `list_usd` and
`lower_bound`. These are the charge's original estimates, never repriced.
Each agent writes only its own requests, including failed charged attempts,
compaction, naming, plugin model calls, keep-warm and voice. Resuming writes
no charge. Account and model names pass through the log's secret redactions;
there are no prompts, replies, paths, credentials or provider account ids.
`--no-log` saves no spend either.

Writers briefly take an exclusive advisory lock on the open day file, not a
separate lock file. On local disks with working file locks, this keeps whole
records together even when a write is short. A busy lock is retried ten times
2 ms apart on the runtime worker, then the write fails rather than waiting
indefinitely. A batch and its individual lines are bounded to 64 KiB, with at
most one extra newline to end a fragment left by a crash or failed write.
A fragment is a bad line, not part of the following record. On a write error,
the writer rolls its batch back under the lock and keeps it for a retry; if
rollback fails it keeps only records whose JSON bytes did not wholly land.
A complete object missing only its newline is not retried. On filesystems
without locks, appends are best-effort and never roll back; network filesystem
locks may be per process, so the whole-record guarantee does not extend there.

A failed spend write never turns off the session log and shows no notice.
Up to 64 KiB of unwritten lines are kept, each on its original day, ahead of
new lines. The next charge and the turn's final sync retry them; a failing
day does not block other days. Later lines beyond the bound are dropped;
kept lines are lost if the agent ends before a retry succeeds. Retries reopen
after a failure and recreate a deleted folder or file (unlink detection on Unix).
Deleting files while a write is in progress can still discard that write.
A crash can leave an incomplete record; completed writes are with the OS,
and the day file is synced when the log is, without propagating sync errors.
A power cut can lose unsynced data. Nothing rotates or prunes these files.
At roughly 350 bytes per request, 1,000 requests a day is about 130 MB a year.

The read opens each of the last 30 day files read-only, without a lock or following links, and refuses non-regular files. It lists every day from today back to the oldest recorded day in that window, filling gaps with zero sums, and returns sums by account, by day and over the window. A shorter window includes its oldest recorded date as `since`.

Each non-blank line is judged by whether it parses as a whole spend record, with or without a final newline. A whole final record is counted; a cut-short or bad line is not counted and marks its day as a lower bound. A cut-short line's mark clears when a later read finds it complete. A failed open or read keeps the day's memoized sums, or zero sums if none were read, marked as lower bounds; a missing file drops that day. Any day on which this chat's main agent failed to write a spend line is also kept and marked, with or without a file. A marked day's accounts and total, and every window sum including it, are lower bounds.

The process remembers each day's sums and mark on the open file's length and modified time, taken before reading. An unchanged file is not parsed again, a changed one is parsed whole, and a file with no stamp is read every time. A read runs off the turn loop. `ListSpend` and `SpendListed` carry only display data: neither the request nor its answer is a log record or a model message.

## Attachments

Every attachment is saved beside the logs, in one store for every media type:
`$XDG_DATA_HOME/wisp/attachments/<sha256>[.<ext>]` (by default
`~/.local/share/wisp/attachments/`), for your records and so a log can be
rebuilt. That covers images sent in user messages and, from version 22, files
a tool attached to its result. The hash is the SHA-256 of the file's bytes,
which are exactly the bytes sent to the model: for a pasted image, the
normalised bytes (EXIF and every other kind of metadata stripped, see
`docs/media.md`); for a tool's file, its bytes as the tool returned them.
The extension is the media type's subtype when that is at most 8 ASCII
letters and digits (`image/png` gives `.png`, `image/jpeg` gives `.jpeg`,
`application/pdf` gives `.pdf`); any other type gets no extension.

- The folder is created only when the first attachment is saved. It is mode
  700 and each file 600. A symlink is refused, both for the folder and for a
  file, and so is a folder owned by another user.
- A file is written to a temporary name in the folder, fsynced, renamed into
  place, and then the folder is fsynced: a pasted image before the turn's
  `turn_started`, a tool's file before its call's `tool_call_finished`. An
  existing file whose hash verifies is reused (its mode is tightened to
  600); one that doesn't is replaced the same way.
- Before version 22, pasted images were saved in
  `$XDG_DATA_HOME/wisp/images/<sha256>.png` or `.jpg`. Reading falls back
  to that folder for a PNG or JPEG the attachments folder doesn't hold, so
  older logs still rebuild; nothing new is written there.
- `--no-log` saves nothing.
- Wisp never deletes them. A file that one session sent can be referenced by
  another session's log, since the names are content hashes.
- `wisp doctor` shows the `attachments` folder and, under `images`, the old
  one, each with how many files it holds and their total size (decimal
  units), or `none yet` when it doesn't exist. It only reads the folders and
  never creates them.
- They are written through `SessionStore::put_blob` and read back with
  `get_blob`, both given the hash and the media type, and `get_blob` checks
  the hash. M4's store will replace them with its encrypted per-entry
  storage (architecture P50), and must decide how forgetting a session
  treats a file another session also references.
- If an attachment can't be saved, logging turns off for the session, as for
  any other log write failure, and the attachment is still sent.

A sliced file still saves the whole file in the blob store. Starting in v43, a
stored `LoggedBlock::Media` in `message_committed.message.content` looks like this:

```json
{"type":"media","source":{"type":"blob","media_type":"application/pdf","sha256":"...","bytes":912004},"slice":{"type":"pages","start":14,"end":16}}
```

The call's `tool_call_finished.attachments` holds the same optional `slice` on
each `AttachmentRef`, beside `media_type`, `sha256`, `bytes` and `saved`:

```json
{"media_type":"application/pdf","sha256":"...","bytes":912004,"slice":{"type":"pages","start":14,"end":16}}
```

Other shapes are `{"type":"seconds","start":30,"end":90}` and
`{"type":"sheet","index":2}`. A single page uses equal bounds, e.g.
`{"type":"pages","start":5,"end":5}`. Both forms omit `slice` for a whole file.

## Shape

One JSON object per line. Each record is flushed as it is written, and the
file is fsynced after every `message_committed` and at the end of every turn.
Once the client has gone, the records written after that, `session_ended`
included, are flushed but not fsynced, so a quit never waits on the disk.

Besides the control characters JSON always escapes, DEL, the C1 controls
(U+0080 to U+009F) and the characters Wisp's terminal sanitiser shows as
`<U+XXXX>` or removes (U+200B, U+FEFF) are written as `\u` escapes, one
beyond U+FFFF as its surrogate pair, so the file itself holds none of them
raw. The escaping lives in `wisp_protocol::jsonl`, which `wisp --output jsonl`
uses too, so both write the same bytes. The strings decode back to exactly
what the model or server sent, though, so **anything that prints them (for
example `jq -r`) must sanitise them for the terminal first**.

Every record has the same envelope, with the record's own fields beside it:

| Field | Meaning |
|---|---|
| `schema_version` | Format version, currently `43` |
| `seq` | Per-session counter from 0, with no gaps |
| `at` | RFC 3339 UTC with milliseconds, e.g. `2026-10-21T19:15:02.481Z` |
| `type` | The record type, below |

```json
{"schema_version":29,"seq":0,"at":"2026-10-21T19:15:02.481Z","type":"session_started","session_id":"019a0f3c-5e2a-7b31-9c4d-2f6e8a1b3c5d","wisp_version":"0.1.0","model":"claude-opus-5-5","effort":"medium","cache_ttl":"one_hour","system_prompt":"You are Wisp, ...","system_prompt_sha256":"9f2c...","endpoint":"https://api.anthropic.com","credential_source":"environment","endpoint_name":"env","message_time":"local","context_note_enabled":true,"debug_cache":false,"refusal_fallback":true}
{"schema_version":29,"seq":1,"at":"2026-10-21T19:15:09.112Z","type":"turn_started","turn":1,"lead":[{"by":"time/sent","text":"Sent: Wednesday 2026-10-21 15:15:09 UTC-04:00\n"},{"by":"context/fill","text":"Context: 3k of 1M tokens (0%). Automatic compaction is off.\n"}],"text":"What is 2+2?","endpoint_name":"env","input_id":"tui-1","cache_ttl":"one_hour","debug_cache":false,"refusal_fallback":true}
{"schema_version":29,"seq":2,"at":"2026-10-21T19:15:11.240Z","type":"message_committed","turn":1,"message":{"role":"assistant","content":[{"type":"thinking","thinking":"Simple.","signature":"EqQB..."},{"type":"text","text":"4"}]},"model":"claude-opus-5-5"}
{"schema_version":29,"seq":3,"at":"2026-10-21T19:15:11.241Z","type":"turn_ended","turn":1,"outcome":{"status":"completed","stop_reason":"end_turn","model":"claude-opus-5-5","usage":{...},"cost":{...},"committed":true,"steps":1,"tool_calls":0},"stats":{"started_at":"2026-10-21T19:15:09.112Z","first_byte_ms":812,"duration_ms":2129,"retries":0,"previous_prompt_tokens":null}}
{"schema_version":29,"seq":4,"at":"2026-10-21T19:16:40.007Z","type":"session_ended","turns":1}
```

## Record types

A record that copies its event comes from the table in core's `session/mirror.rs`, which also gives back the event a resumed transcript shows; every event and record is placed there or in the replay, so a new one doesn't build until it is.

| `type` | When | Fields |
|---|---|---|
| `session_started` | First record | `session_id`, `wisp_version`, `model`, `effort`, `cache_ttl`, `system_prompt` and its `system_prompt_sha256`, `endpoint` (origin only), `credential_source` (`environment`, `env_file`, from version 11 `credential_file` for a key in `credentials.json` and `keyring` for one in the system keychain; a value a reader doesn't know reads as its text, so later sources need no bump), `endpoint_name` (the endpoint the session started on: `env` for the `ANTHROPIC_*` variables, or a name from `config.toml`; absent in older logs, where it means the session's only endpoint), `message_time` (`local`, `utc` or `off`; absent in older logs). `cwd` (the session root, with `~` for the home directory): from version 13 in every session that has a Workspace, with tools or not; before it, only with tools. With tools: `tools` (sorted names of the tools declared normally, never a deferred one, including `web_search` when the starting endpoint and model send the provider's server tool), `tools_sha256` (sha256 of the canonical tools JSON as sent in every request, client and server tools merged and sorted by name, deferred entries and their `defer_loading` flag included, for cache debugging) and `output_dir` (where full tool output is saved; local only, never sent), all three absent in a session without tools. From version 13: `instructions` (the project block sent as the second system block, byte for byte, and from version 30, in a child of a preset with instructions, the project block followed by the preset's section; absent when no instructions loaded), `instructions_sha256` (its sha256; absent with it) and `instructions_source` (`auto`, `default` or `none`, the resolved `--instructions` value; a value a reader doesn't know reads as its text; absent in older logs). Additive, with no version of its own: the block can also start with the built-in extra instructions (`docs/instructions.md`, "Extra instructions"), so `instructions` is the whole second system block, present with the extra text alone under `--instructions none`; and `instructions_extra`, the extra instructions' name as a string (`autonomous`), absent when there are none; a name a reader doesn't know reads as none, with a note. Also additive: `preset`, the name of the preset whose instructions the main agent loaded (`docs/agents.md`, "The main agent's preset"), absent in a child's log and when there is none; a resume reads it back. From version 14: `context_note_enabled` (the resolved `context.context_note` setting: whether turns carry the `Context:` line; absent in older logs). From version 16: `debug_cache` and `refusal_fallback` (the two request options the session started with; absent in older logs, where they mean `false` and `true`). From version 28, in a child agent's log only (`docs/agents.md`): `parent` (`session_id`: the parent's session; `agent`: this agent's path, such as `/review/planner`; `depth`: 1 for a child of the main agent; `config`: the spawn's choices, `preset` when one was used, `tools_off` when not empty, `spawn` only when `false`, and from version 30 `instructions`, the preset's instructions file `~/`-shortened when the preset named one (up to version 29 it held `report`, the kind owed, which a reader now ignores); additive with no version of its own, `preset_values`, the preset as it was at the spawn (`name`, `description` and the keys it set, `endpoint` among them when set), present only when a preset was used and absent in older logs; additive with no version of its own, `keep_warm: true` when the parent marked the child to be kept warm (`docs/agents.md`, "Keep-warm"), absent otherwise, and absent too when the spawn asked for it on an endpoint whose provider Wisp doesn't refresh; a resume compares the preset now against it, not against the agent's resolved choices, so a spawn that overrode a preset key isn't reported as a changed preset; additive with no version of its own, `choices`, the spawn's choices in order when its preset has a `fallback` (`[{"endpoint", "model", "effort"}]`, `model` and `effort` when set: the first choice as the spawn resolved it, then each fallback), which the child reads to move past a used-up limit (`docs/agents.md`, "Reports, and the parent's guarantee"), absent otherwise and in older logs; and, additive with no version of its own, `todos` when the spawn's `todos` became the child's starting checklist, `[{"text", "status"}]` with every status `pending`, absent otherwise). The rest of the child's configuration is in this record's own `model`, `effort`, `system_prompt` and `tools`. Additive: an older reader ignores it, and a child log replays as any session. Additive, with no version of its own: `plugins`, `[{"id", "version", "source"}]` sorted by id, every plugin that started for the session, whether or not one of its tools is on and whether or not the session has any tools (chat-only and jsonl sessions list them too), the legacy `shell` entry for bash and process included; a plugin left out when it started (its `describe` panicked or didn't match its manifest) is not listed (`docs/plugins.md`, "The registry"); `source` is `{"type": "bundled"}`, or `{"type": "wasm", "sha256"}` for a WASM plugin, with the SHA-256 of its component (`docs/plugins.md`, "WASM plugins"; additive, with no version of its own), and a type a reader doesn't know reads as `unknown`; absent when the session's tools were not built by the registry, as in kernel tests that build a `Toolbox` by hand. From version 29: `tool_definitions`, the session's client tool definitions exactly as the request's `tools` array carries them (`Toolbox::canonical_json`: sorted by name, each `{"description", "input_schema", "name"}` with its schema canonicalised), never a server tool such as `web_search`, which resume recomputes for its endpoint and model; absent when the session has no client tools. Additive, with no version of its own: `deferred_definitions`, the session's deferred set, chosen at its start (`docs/tools.md`, "Tools off at start: declared deferred"), in the same canonical form and order and without the flag; a request sends these entries, with `defer_loading`, only on a pair that defers, so the field can be present while the starting pair sends none of them. Absent, as in every older log, when the set is empty. And `tool_context` (`{"platform", "capabilities"}`): the build context the registry passed to every plugin's `describe` when the tools were built, the platform (`unix` or `windows`) and the capabilities the on tools' manifests provide, sorted; it is what the tools were described with, so it keeps the capability of a plugin left out at start; absent when the session's tools were not built by the registry. A child's record holds its own narrowed definitions and context. With `system_prompt` they are the session's whole prefix, so a later resume can rebuild it from the log and describe the tools again under the same context (`docs/plans/plugins.md` §2.11); only `system_prompt_sha256`, `system_base_sha256` and `tools_sha256` are exempt from redaction, never the whole copy. Additive, with no version of its own: `system_base_sha256`, the sha256 of the base instructions `system_prompt` starts with (before the tool lines and plugin sentences); absent in older logs and in a session with no system prompt. A resume reads it to tell whether this build's base differs from the logged one (`docs/tools.md`, "Resuming a session"). From version 35, `sections` (each `{"name", "snapshot"}`: the snapshot each `system` prompt section rendered at start, in resolved order; absent when none), the base a resume and a rewind read for those sections. Additive, with no version of its own: `headless` (`true`), written only by a root session that no person is typing into (`--output jsonl` and other scripted starts); absent in a TUI chat, in a child and in every older log. The resume picker leaves such a chat out |
| `turn_started` | A turn begins | `turn` (from 1), `text` (the user's message as sent: as typed, with each paste chip expanded to its full text, byte for byte), `lead` (from version 29: the text blocks sent ahead of the turn's inputs, in the order sent, each `{"by", "text"}`, `by` naming what wrote it and `text` the block byte for byte; absent when empty; the core writes `workspace/environment` (`core/environment` before version 35; the `<environment>` block; present on the turn whose user message first carried it, and on a later turn that sent it again because its agent lines changed, such as the presets line after a resume with edited presets, `docs/agents.md`, "Resume"; the rebuild places it on whichever turn carries it), `workspace/terminal` (the `Terminal: ...` line with the reply width and rows, `docs/plugins.md`; present on a turn whose size differs from the one the model last saw, absent otherwise and in older logs, with no version of its own), `time/sent` (the `Sent: ...` time stamp; absent when stamps are off), `context/fill` (the `Context: ...` line, `docs/context.md`; absent when the line is off), `idle/note` (the idle note, `docs/plugins.md`, "The bundled hooks"; present only on a main agent's turn the user didn't start, once they have been idle 10 minutes) and `shell/processes` (`core/processes` before version 35; the `<processes>` notice, below), in that order; later versions add entries written by plugins' `before-turn` hooks and prompt sections, under their names. From version 35 an entry may also carry `snapshot` (a prompt section's snapshot, `docs/plugins.md`, "Prompt sections": the base the section's next render compares against; absent on entries that aren't sections; the core's `core/preset`, a resumed main agent's preset update, carries one too) and `replay` (true for an entry a Retry sends again without calling its writer: the `<processes>` notice and the entries of hooks with `retry = "replay"`; absent when false). From version 42 an entry may carry `tail` (true for `core/data`, the one entry sent after the turn's inputs: on a model whose shipped row has `turn_form`, the `time/sent` and `context/fill` texts wrapped in the row's `data_open` and `data_close`, `docs/gemini.md`; a rewind doesn't count it among the entries ahead of the inputs, and the rebuild places it after the opening inputs and before any steer committed later; absent when false). A reader maps the old names once, where it reads the entry: `core/environment` to `workspace/environment` and `core/processes` to `shell/processes`, marked `replay`. Up to version 28 the same texts were the fields `context`, `stamp`, `context_note` (from version 14) and `processes` (from version 17), which a reader maps, in that order, to those four entries, the last marked `replay`), `model` and `effort` (the settings the turn runs with; absent before version 4), `effort_marker` (`{"level", "rendered"}`: the effort marker that follows the turn's user message; present only on a turn that changed effort), `images` (the turn's images in the order sent, each `{"sha256", "media_type", "bytes", "width", "height"}`: the file's hash, `image/png` or `image/jpeg`, its size in bytes and its pixel size; absent when the turn has none and before version 5), `endpoint_name` (the endpoint the turn ran on, every step of it; absent before version 6), `input_id` (the client's id for the message, on a turn started by one message; absent before version 8). From version 16, `cache_ttl`, `debug_cache` and `refusal_fallback`: the request settings outside the history that the turn's requests used (the conversation tail's cache lifetime and the two request options), on every turn; absent in older logs, where they mean the `session_started` values. A turn started by several messages at once (queued messages, or held ones sent with a new one) has no `text`, `images` or `input_id` of its own and instead lists them in `inputs`, in order, each `{"id", "text", "images", "note"}` with `images` as above and absent when empty. From version 38, `note` is the sent-while-working mark (`docs/architecture.md` §4.3), byte for byte, on the last of the user's messages written while a turn ran; absent otherwise. A turn started by one marked message lists it in `inputs` too. The ids in `input_id` and `inputs` are closed: those messages left the queue to start this turn, so a reader rebuilding the queue (resume) never offers them back, whatever the turn's outcome. From version 17: `origin` (`process_exit` on a turn Wisp started because background processes ended; absent for a turn the user started) and `processes` (the `<processes>` notice sent in that turn's user message, byte for byte, after the context note; absent when none; from version 29 the `core/processes` entry of `lead`). Both are written only while the `process` tool is on, which no release turns on yet; the rebuild places `processes` after the context note (the rebuild rule below). A turn with `origin` `process_exit` has no `text`, `images`, `inputs` or `input_id`: its user message is the context, stamp and context note by their usual rules, then the notice, whose first line inside the block is `The user didn't send this message; Wisp sent it because background processes ended.` From version 28, `origin` is also `mail`: a turn Wisp started because mail from another agent arrived (`docs/plans/subagents.md` §5), either a wake after the settle or mail from above at an idle agent. Such a turn lists the mail it carries in `inputs`, even a single one, each entry's `text` being the mail's rendered `<wisp:message from="…" kind="…">` block (inside its frame, except a `task`, on a model with `turn_form`, `docs/gemini.md`); any of your held messages it released come first, then mail from above, then other mail. Process ends due at the same moment ride in its `processes` notice without the wake line above. A turn you started also lists in `inputs` the mail it carried after your messages, with no `origin`. Mail entries are identified by their `input_accepted.from`; the rebuild treats them as any input. A retry turn that carries its failed turn's inputs again carries that turn's `processes` again too, byte for byte. Additive, with no version of its own: `ends` (the process ends that notice reported, each `{"id", "end"}` as in the `TurnStarted` event's `ends`, including the ends a resume's restart lines name, first and in the notice's order; written beside `processes` and carried again with it by a retry; absent when empty and in older logs). The rebuild doesn't read it; resume uses it so a Retry restored from the log, or offered after a crash, reports the same ends the live Retry would, and a log without it reports none. From version 20: `retry_of` (the failed turn this turn retries, started by the error's Retry or Switch endpoint choice; absent for every other turn). Additive, with no version of its own: `binding_controls` (whether the session sends the thinking binding controls, which it turns on when it starts with more than one endpoint or on a pair where they are usable, switches endpoint, or keeps thinking across a break (`docs/context.md`, "Broken thinking"); written on every turn, `true` or `false`, and absent in older logs, where resume guesses it from the endpoint records). The rebuild doesn't read it; resume restores it from the last turn, or turns it on when a `session_resumed` after that turn moved to another endpoint, so the request keeps its prefix whichever endpoints are configured when the session is resumed. A retry turn's `model` and `effort` are the failed turn's and its `endpoint_name` the one it runs on. When the failed turn had committed steps, the retry has no `text`, `images`, `inputs` or `input_id`: it continues that history and adds no opening user blocks (the rebuild rule below); when it had none, it carries the failed turn's inputs again, under new ids, with a fresh stamp and the environment block and context line recomputed in its `lead`. `kept_tools_sha256`: on a retry turn whose first step sent the failed request's tools although its endpoint has since been refused web search (a compaction after the failure met the organisation refusal, `docs/endpoints.md`), the sha256 of those tools, computed as `settings_changed.tools_sha256` is; absent on every other turn. They are sent for that step only, with no `settings_changed`: the one case where the last tools logged aren't the tools a request used. Additive, so no schema bump. The rebuild doesn't read it yet; it records which tools the thinking that step committed was built with From version 36, `tool_message`: the blocks of the turn's tool changes (`tool_removal`, `tool_addition` by `tool_reference` or `tool_definition`, and a text block of added tools' sentences), written only when not empty; the rebuild puts them, after the effort block if there is one, in the system message right after the user message, as the live turn did (`docs/tools.md`, "Switching tools in a running session"). A re-statement after an in-turn head is committed whole as a late system message, as the late effort marker is. Additive, with no version of its own: on a resumed session's first opening, and again wherever a compaction or rewind dropped it, `tool_message` can also hold one text block with the whole new base instructions, framed as replacing the earlier ones (`docs/tools.md`, "Resuming a session"); it has no display record, and the rebuild places it as any other block of the message |
| `input_accepted` | A message sent while a turn ran is waiting, or an edit or switch took a waiting message's place | `turn` (the running turn; null outside a turn), `id`, `mode` (`steer`: for the running turn's next step; `queue`: for the next turn), `text` (as sent, byte for byte), `images` (as in `turn_started`; absent when none), `replaces` (the id of the waiting message this one replaced; absent for a new message), `held` (true when it replaced a held message; absent when false). From version 28 also mail from another agent, logged as it arrives, mid-stream and mid-tool included, so nothing sent is lost on quit: `from` (`{"agent", "kind", "mail", "wake", "plugin"}`: the sender's path, the mail's kind (`task`, `message`, `report`, `notice`, and from version 40 `question` for an answer or stop note of a posted question, whose `mail` is the question's call id; one a reader doesn't know reads as `unknown`), its per-tree id, and `wake: false` for mail that waits for the next turn, `wake` being absent when true; optional `plugin` names the plugin whose line draws the notice), `mode` `steer`, no `images` and `text` the rendered `<wisp:message>` block (framed, except a `task`, on a model with `turn_form`; or plain user text for `question`). Mail is not your input: it never counts toward the 32 waiting messages, is never named by `input_held` (and only `question` letters are named by `input_withdrawn` with `by: rewound`), and leaves its own queue only by committing (named by an `input_committed`, or carried by a turn's `inputs` whose first reply committed). A turn that is stopped, fails, keeps nothing or ends with the session puts the mail it delivered back at the front of that queue; resume reads accepted, uncommitted mail as waiting again, in order, never as messages to return to your composer. An audit record: the rebuild doesn't read it |
| `input_held` | Queued messages kept after a turn was stopped or failed; they wait until the user's next message or a release. Also, with `by` `turn_failed`, a queued message that reached its turn while the chat's account was signed out in another window and the chat must choose where to go; `input_unheld` follows once the answer moved the chat | `turn` (null outside a turn), `ids`, `by` (`turn_stopped` or `turn_failed`) |
| `input_unheld` | Held messages became ordinary queued messages again (released during a running turn) | `turn` (null outside a turn), `ids` |
| `returned_taken` | A window restored or already holds returned messages; ignored ids write no record | `ids`; these messages are no longer handed back, even after a rewind or fall-back |
| `input_committed` | Steered messages joined history with the reply that answers them, or an internal `wisp-call-note-<turn>-<step>` retry note committed with a succeeding Gemini `MALFORMED_FUNCTION_CALL` retry (skipped on resume when drawing transcript bubbles or collecting user-said text). Written immediately before that reply's `message_committed` | `turn`, `step` (the step whose reply committed them), `inputs`: `[{"id", "text", "images", "note"}]` in delivery order, `note` as in `turn_started.inputs` (version 38), on the last of a batch's messages |
| `input_delivered` | Additive, with no version of its own. Steered messages went out with a step's request: written when the step starts, before its request is sent | `turn`, `step`, `ids` (the messages delivered into that step, in order; never the ones that started the turn). Resume reads it to know which messages a failed or interrupted turn's Retry sends again |
| `input_withdrawn` | Messages that will never join history: waiting ones removed or replaced by the user, ones returned by a stopped or failed turn, ones a completed turn didn't keep, or ones withdrawn when the session ended (resume hands back messages still waiting or not kept in the turn a stop cut, unless Retry resends them; it writes `session_ended` for those a crash left waiting; the kernel keeps them as `Agent::returned_inputs()`, and the full-screen TUI puts them back in the composer until a window takes them, with their images). Every delivered message either commits or is withdrawn | `turn` (null outside a turn), `ids`, `by` (`user`, `replaced`, `turn_stopped`, `turn_failed`, `not_kept` or `session_ended`; from version 29 also `{"hook": {"hook", "reason"}}`, an input a plugin's `user-input` hook blocked, which no release writes yet). `not_kept`: the turn completed with nothing to keep (`committed: false`), such as a reply cut off with nothing in it, so the messages that started it and any steers delivered to its last step went unanswered and were dropped with it |
| `settings_changed` | A settings change was accepted (any setting), a rejected per-message marker forced a rebuild, (from version 20) a Retry switched endpoint, a resume continued on another endpoint than the log's (after `session_resumed`, with an `endpoint_switch` rebuild), or a switch was withdrawn because the compaction it needed was cancelled (after that turn's `turn_ended`) or failed with a converted view that wouldn't fit (after the `compaction_failed`, before the turn's `turn_started`) | `model`, `effort` (what the next ordinary turn runs with, whether or not this change touched them; a Retry that switches endpoint writes the new endpoint with the pending model and effort, or the current ones when nothing is pending, never the retry's own, which are in its `turn_started`), `cache` (the effect of the next ordinary turn's whole pending state against the current one: an object whose `type` is `kept`, `kept_if_passed_on`, or `rebuilt` with a `reason`: `model_switch`, `effort_change`, `per_message_effort_rejected`, `endpoint_switch`, from version 10 `native_search_refused` (the endpoint refused the web search tool, its organisation having switched web search off or a gateway not passing the tool on, so the step was re-sent without it), and from version 16 `cache_ttl_change` and `request_option_change` (a pending cache lifetime, or a pending `debug_cache` or `refusal_fallback`); a reason a reader doesn't know reads as its text, so later reasons need no bump). From version 6 also `endpoint_name`, `endpoint` (its origin) and `credential_source`, naming the endpoint the next turn runs on. The optional `provider` uses the same name as `agent_spawned.provider` (additive, no schema bump); on an endpoint switch, `account_bound` (the models whose earlier thinking another account may drop; absent when empty). Also `tools` and `tools_sha256`, the tools the next request uses, written only when they differ from the last tools logged (the last `settings_changed` carrying `tools_sha256`, else `session_started`'s): a switch to a pair of the other web search form, or the organisation refusal. `tools_sha256` is the sha256 of the canonical merged tools JSON, client and server tools sorted by name, and is present whenever the tools changed, even when the new list is empty; `tools` itself is then omitted, so an absent `tools` beside a `tools_sha256` means no tools. Both are absent when the tools are unchanged; additive, so no schema bump. `changes` (absent when empty): the settings the change set, in order, each `{"target", "value", "layer", "when", "saved"}`, never a secret: `target` is the setting's name or its path (`models."claude-opus-5-5".context_window`), `layer` is `session` for a value set for this session, or the saved place's layer for a save to a path (`config`, `model`, `endpoint`, …), `when` is `now`, `next_turn`, `next_session` or `next_call` (a `voice.*` setting, read again when the next voice call starts; an older reader reads it as its text), and `saved` (`{"path_display", "line"}`, absent when nothing was written) says where it was written; for an unset, `value` is the value now in effect from whichever layer wins and `unset: true` is added. `by: "agent"` marks a change the agent made itself with `manage` (`docs/search-first.md`); it is absent otherwise and in older logs, additive within the version, and the resume fold reads it to know which `turned_off` names the agent may turn back on without asking. `withdrawn` names a withdrawn switch (`switch_compaction_cancelled`, `switch_compaction_failed`, or a later value read as its text). A `changes` entry whose target is the `tools` setting is a next-session value, unrelated to the record's own `tools` and `tools_sha256`, which name the tools the next request uses. No `turn`: a change takes effect from the next turn, or at once for a `now` setting From version 36 a `changes` target this build can't parse (a setting added later) is kept as its text and skipped by every reader, the fold, the transcript and the session layer, while the known targets on the same line apply |
| `per_message_effort` | Through a gateway, a model first accepted or refused the per-message effort beta, or (`fallen_back` only) an endpoint, the direct API included, later rejected it by shape or as an unexpected beta. The kernel also emits `AgentEvent::FeatureStateChanged` with the same `model` and `state` and `feature: per_message_effort`, so clients never infer it | `model`, `state` (`proven` or `fallen_back`), `endpoint_name` (the state belongs to the (endpoint, model) pair; absent before version 6, where it means the session's only endpoint) |
| `binding_controls` | Through a gateway, a model first accepted or refused the thinking binding controls beta, or (`fallen_back` only) an endpoint, the direct API included, later rejected it by shape or as an unexpected beta (also emitted as `FeatureStateChanged` with `feature: binding_controls`) | `model`, `state` (`proven` or `fallen_back`), `endpoint_name` (the state belongs to the (endpoint, model) pair; absent before version 6, where it means the session's only endpoint) |
| `retrying` | A failed request will be re-sent, byte for byte (except that after Gemini's `MALFORMED_FUNCTION_CALL` the retry appends the fixed retry note once), after `delay_ms` | `turn`, `attempt` (the retry's number within its step, from 1), `delay_ms`, `reason` (the failure's message, with its HTTP status when it had one). Additive fields: `step` (the step being retried; absent on a compaction's attempt in or before a turn, which still carries the turn's number, and `0` on one in logs written before automatic compaction), `kind` (the failure's error kind), `detail` (as in `turn_ended`), `usage` (the failed attempt's usage; for a compaction attempt, the sum of its `iterations`, when its `message_start` arrived), `partial_output` (the text the failed attempt had streamed, which was discarded and never committed), `model` (the model that served the failed attempt, when known), `cost_usd` (its priced usage, absent without usage or a price) and `lower_bound` (true when it may have cost more than `cost_usd`, absent when false), and from version 10 `resend` (`breakpoint_limit`: not a retry but a re-send after the API refused too many cache breakpoints mid-stream; it uses up no retry; absent for a retry), and `busy` (`waited_ms`, the step's busy waits before this one, and `window_ms`, the busy window, `0` when unbounded; older logs and finite policies can carry a nonzero window: present on a retry of a busy failure, which counts toward no limit, `docs/endpoints.md`, "Busy endpoints"; additive, absent otherwise and in older records). Older records hold only the first four. From version 19, `turn` and `step` are absent for a request sent between turns (a manual `/compact`) |
| `step_ended` | A step (one model request, however many attempts it took) ended | `turn`, `step`, `usage` (the attempt that ended the step; a retried attempt's usage is in its `retrying` record), `cost_usd`, `lower_bound`, `prompt_tokens` (input plus cache reads and writes, null without usage), `response_id` (the reply's id, null when nothing was committed), and, written when known, `window` (the context window in force), `compact_at` (the point Wisp would compact at) and `used` (the request's fill: the last iteration's prompt tokens when the response had iterations, else `prompt_tokens`). Absent before this record was added. The last three are additive, absent in older records, and take no schema version. Also additive, with no schema version: `first_pass` (the first iteration's prompt tokens), written only when the response had iterations (a server tool such as web search, or a refusal fallback), since without them it equals `prompt_tokens`. The next request's cache check compares its read with it, as that request can reuse only the prefix up to the first iteration's prompt. Resume reads it back; a record without it falls back to `used`, and one with neither skips the check for the next request rather than use the summed `prompt_tokens`. Written only for a step whose reply was committed, the request's entry in the request list (`docs/context.md`): `view_len` (how many messages the request sent), `started_at` (when it went out, RFC 3339 in `at`'s format), `ttl` (the tail cache TTL it was sent with, `five_minutes` or `one_hour`) and `prefix` (the SHA-256 digest of the prefix it sent, 64 hex characters, as the provider computed it when the request went out; absent when the provider gives none); additive like the three before, absent for any other step and in older records. From version 15, `accepted`: `false` when no request of the step got its first event (the provider refused it before answering, or the step was stopped first), written only when false and read as `true` when absent, so older records count as accepted. The rebuild reads `accepted` for provisional inputs; found-tool system messages are recorded by `message_committed` at acceptance, independently of whether the reply completes. Request metadata also restores cache baselines. Also additive, with no schema version: `queued_ms`, the milliseconds the step's requests waited for their turn on their endpoint (`max_in_flight`, [docs/endpoints.md](endpoints.md#one-limit-per-endpoint)), summed over its attempts and written only when not zero; `started_at` is taken after that wait From version 36, `betas`: the sorted `anthropic-beta` values the request sent, written only when not empty; from 36 an absent list on a committed step means none was sent, and before 36 it is unknown. A step whose request failed or whose reply wasn't committed logs the values that request sent too (additive, no version of its own; earlier version 36 logs wrote none there, so an absent list on such a step is unknown). Resume compares the list of the last step that was committed or logged a list, so a request sent after a background swap counts even when it failed, with the first resumed request's, leaving out `inline-tools-2026-09-15`, whose appearance keeps the cache ("Stability", below). Also additive, with no schema version, and not read by the rebuild: `thread`, how the step's last request used a message thread (`docs/context.md`, "Message threads"): `create`, `continue`, `replay` (a refused continue sent again as a new thread) or `dropped` (threads turned off on that account for the rest of the session after a refusal), absent when it used none; and `thread_error`, on `replay` and `dropped`, the refused request's error code, else its error type |
| `attachments_withdrawn` | From version 15. The API refused a request for its attachments, Wisp replaced them with notes and the re-send was accepted (`docs/media.md`, "A refused attachment"). Written at the re-send's first event, before the step's `input_committed` and `message_committed` | `turn`, `step`, `items`: `[{"ordinal", "sha256", "note"}]`, one per withdrawn block: its position among the provisional message's media blocks (from 0, in request order: an open tail of tool results first, then every pasted image of the turn and its steers, counting those an accepted request already carried, which are withdrawn only once a compaction's head has made them unsent again, and those an earlier withdrawal already replaced), the sha256 of its bytes and the text block that took its place |
| `fallback` | The provider switched models after a refusal | `turn`, `from`, `to` |
| `cache_miss` | The cache reused far less than expected: the request read less than half of the previous request's first-pass prompt (`step_ended.first_pass`) | `turn` (from version 19, absent for a request sent between turns), `previous_prompt_tokens` (the previous request's `prompt_tokens`, summed over its iterations), `cache_read_input_tokens`, `cache_creation_input_tokens`, `diagnostics` (the provider's own diagnosis, sent only with `debug_cache`; null otherwise; identity keys, `safety_identifier`, `user`, `user_id`, `account_id`, `email`, `metadata`, `plan_type`, `credits` and `promo`, are dropped at any depth). Additive, absent in older records, no schema bump: `cause`, Wisp's local diagnosis (`{"type", …}`; `docs/context.md`, "Cache misses"): `endpoint_changed`, `model_changed`, `effort_changed`, `thinking_changed`, `tools_changed`, `system_changed`, `betas_changed`, `message_changed` (`index`), `injected_thinking` and `provider_dropped_thinking` (`first_path`, `count`), `ttl_expired` (`gap_s`), `expected` (`reason`), `remote_loss` (nothing changed locally inside the TTL: a provider- or gateway-side loss) or `unexplained` (Wisp couldn't compare); a type a reader doesn't know reads as its text. The rebuild doesn't read it |
| `cache_miss_repeated` | The same cause, other than `expected` or `ttl_expired`, missed on two or more back-to-back requests; written after the second `cache_miss` and after each further one in the run. A new record type, so no schema bump: older readers skip it | `turn` (absent for a request sent between turns), `cause` (as in `cache_miss`, the latest miss's), `count` (misses in the run so far, from 2), `usd` (the run's estimated extra cost over full hits, `docs/pricing.md`; absent on an unpriced endpoint or model), optional `billing` (the estimate's billing provenance; omitted for `metered`, which is also the default when absent). Also emitted as `AgentEvent::CacheMissRepeated`, shown to the user as a notice and never added to the conversation. An audit record: the rebuild doesn't read it |
| `cache_refresh` | A keep-warm refresh of an idle agent's last request (`docs/agents.md`, "Keep-warm"): one per refresh sent or skipped, written when it ends or is skipped. A new record type, so no schema bump: older readers skip it | `turn` (the turn whose last request was refreshed), `outcome` (`read`: it read at least half its own prompt from the cache; `missed`: it read less, so it wrote the prefix again; `failed`: no usage came back; `stopped`: the session ended while it streamed; `skipped`: it wasn't sent; an outcome a reader doesn't know reads as its text), `detail` (text, absent when there is nothing to add: a read's or miss's stop reason other than `max_tokens`, or the error kind that cut its stream; a failure's error kind, such as `rate_limit`; a skip's `late` or `changed`), `started_at` (RFC 3339, when it was sent or skipped), `count` (its number in the idle spell, from 1), `usage` (the last the stream gave; absent with none), `cost_usd` (absent when unpriced or unknown) and `lower_bound` (only when `true`: the stream was cut, the cost is unknown, or usage was reported for a request that sent audio or video to a model that charges more for audio). Its cost is in the session's total, never in a turn's: `--resume` adds `cost_usd` to the session's spend and ORs `lower_bound`, even when a `rewound` masks the record's turn, and keeps the last unmasked `read`'s `started_at` as when that turn's last request was last used, for the resume note. The rebuild doesn't read it |
| `model_asked` | A plugin's one-shot model request ended; one record per ask, additive with no schema bump | `call_id`, `plugin`, optional `turn`, `detail` (error kind, unexpected stop name or `cancelled`, never provider text), `usage`, `cost_usd`, `list_usd`, `plan_usd`, `model` and `lower_bound`. The usage is the reply's own total, including search counts attributed to no iteration. Cost joins the session and open turn; resume adds it back even if the turn is masked. A sent request dropped before completion is possibly billed. No prompt or reply text is recorded; the rebuild ignores it |
| `session_named` | The chat got a name, or a try to name it ended. Written by the root of a full-screen session that logs, once its first three typed messages were sent (each the text of a `turn_started` without `origin` whose turn has an accepted `step_ended`; a multi-input turn has none), right after the third such turn ends, or after a resume of a chat that has them and no name. One try per run: a failed one is tried again on the next resume, never after a later turn; one that sent nothing (not admitted, or a local error) waits for the next turn instead. A `rewound` that drops one of those turns drops its message from the three, and keeps a name already made. A new record type, so no schema bump: older readers skip it | `name` (absent on a failure: the first non-blank line of the reply's text once quotes and backticks are dropped, spaces collapsed, without a heading's leading `#`, emphasis `*` runs or a final period, key-shaped words masked by `topic_mask` once, then cut to 200 characters), `detail` (absent with a name: the error kind that ended the call, such as `invalid_request`, `interrupted` when the session ended first, a stop reason other than `end_turn`, or `empty`; never the reply's or the provider's text), `usage`, `cost_usd`, `list_usd`, `model` and `lower_bound`, as on `cache_refresh`. A manual rename appends a record with only `name`, without usage, cost or model. A name request that lands after a name was set keeps its usage and cost, but has no name and has `detail` `renamed`. Its cost is in the session's total, never in a turn's, and `--resume` adds it as it does a refresh's. The list shows the newest `name` (`ChatRow.name`); the rebuild doesn't read it |
| `voice_call` | A voice call's connection ended (`docs/voice.md`): one per connection, so a call reconnected mid-way ("Reconnecting") has several with the same `call`, each priced on its own, and a total sums them all. Written by the root, which books the call's cost in the session's spend when the call's record reaches it, at its next idle moment or as the session ends. A new record type, so no schema bump: older readers skip it | `call` (the client's call id), `outcome` (`completed` with its `end`, or `failed` with its `kind` and `message`, as on the `ended` event), `seconds` (the connection's, from when it opened to when it closed on the kernel's clock; 0 for a call that never started), `lines` (the transcript lines no hand-off carried while that connection was up, and on the call's last record those after its last hand-off, which the next hand-off may still carry; absent when none), `unknown` (each control frame type the route couldn't name, with its count; absent when none), `endpoint_name` (the voice account, which its cost is charged to), `usage` (no tokens, only `billing`: `chat_gpt_plan` for a subscription's call), `cost_usd` (`pricing::voice_usd`; absent on an account set to `pricing = "none"`), `list_usd` (the estimate on such an account), `model` and `lower_bound` (only when `true`: the model has no voice price, or the account shows no price). A call that never started has no `endpoint_name`, `usage`, `cost_usd`, `list_usd` or `model`, and books nothing. Its cost is in the session's total, never in a turn's, and `--resume` adds it to the session's spend and to `endpoint_name`'s account. The rebuild doesn't read it |
| `unknown_provider_event` | The provider sent an event this version doesn't know | `turn` (from version 19, absent for a request sent between turns), `provider`, `event` (verbatim, except that identity keys, `safety_identifier`, `user`, `user_id`, `account_id`, `email`, `metadata`, `plan_type`, `credits` and `promo`, are dropped at any depth) |
| `input_transformations` | The provider reported what it changed in a request's input before the model saw it. Anthropic sends this list on `message_start` once the thinking binding controls beta is on, and again on the final `message_delta` after a mid-stream server-side model fallback; each copy is written as it arrives, or a moment later when it comes while the reply's text is held as a possible quiet end (`docs/architecture.md`, "Quiet end"). When a request has both, the `message_delta` copy is the one for the model that actually answered after the fallback. The TUI's details line summarises a list (its count by `type`, then the first `path`, at most 500 characters), or reads "same as the previous request" for a list equal to the one reported before it; the record keeps it whole. The list is empty when nothing changed, else one entry per thinking block it dropped or let through after a failed check. Also emitted as `AgentEvent::InputTransformations`, with no notice: the TUI lists a non-empty one in the turn's details. A list whose shape Wisp can't read is kept as `unknown_provider_event` (with `type: input_transformations`, as every version before this one wrote it). A new record type, so no schema bump: older readers skip it | `turn` (absent for a request sent between turns), `provider`, `transformations`: each entry as the provider sent it, byte for byte in meaning: `type` (text), `path` and `reason` (text, or `null`, each kept apart from an absent field) and any other fields. Wisp gives the values no meaning of its own; Anthropic's are `thinking_dropped` and `thinking_mismatch_allowed`, with the reasons `prefix_binding_mismatch`, `model_binding_mismatch` and `organization_binding_mismatch`, and `path` indexing the request's `messages` as sent |
| `rejected` | A command was refused | `turn` (null outside a turn), `reason`; from version 8, `id` (the message it concerns) and `kind` (`empty`, `image`, `too_many`, `duplicate`, `already_delivered`, `unknown_id`, `withdrawn`, `nothing_held` or `nothing_running`) when it concerns one message, both absent otherwise. From version 9, a refused answer to a tool's question has no `id` and the `kind` `no_question` (no question with that `call_id` is open) or `bad_answer` (the answers don't fit the open questions), so it is never taken for a refused settings change. From version 17, a refused process command has no `id` and one of the kinds `no_process`, `process_ended`, `not_movable`, `not_a_terminal`, `already_attached`, `not_attached`, `input_too_large` or `no_attach_request`, or `nothing_running` for a move to the background with no turn running (reason `nothing is running`); its `reason` names only the process number or the call id, never input typed into a process. From version 18, a refused endpoint save (a taken name, a lock that stayed busy, a keychain write that wasn't confirmed, or a keychain failure later in a run that had one) has the `kind` `endpoint`; its `reason` may name the endpoint but never its key, its URL or the keychain account and store id it would be kept under. From version 19, a refused `/compact` has no `id` and the `kind` `turn_running` (a turn is running), `nothing_to_compact` (nothing was added since the session began or since the last compaction) or `compacting` (a compaction is already running); a `/compact` refused because the model or endpoint had no native compaction, written only before Wisp's own engine compacted there, has no `kind`. A `kind` a reader doesn't know reads as its text. Refusals before the session starts (the first-run card, `docs/endpoints.md`) are never logged. From version 28, a `Withdraw` or an edit aimed at a mail id (an `input_accepted` with `from`) is refused with `kind` `not_yours`, a value older readers keep as its text. Additive, with no version of its own: a send whose user inputs a `user-input` hook all blocked has no `turn`, no `id` and the `kind` `input_blocked` (its `reason` is the first block's `Blocked by <hook>: <reason>`), and one stopped by Esc during that hook has the `kind` `input_cancelled`; older readers keep either as its text. Also additive: a send refused because the chat's account was signed out in another window and the chat must choose where to go has the `kind` `account_gone`. From version 20, a refused Retry has no `id`, the `kind` `not_retryable`, and its `reason` (for example `nothing to retry`, `turn 3 is no longer the last turn`, `a message was sent after the failure`, `the conversation was compacted after the failure`, `cancelled before it started`, `the session ended`, or `a thinking-binding failure isn't retried on the same account`). |
| `wake_held` | From version 28. Mail that would wake an idle agent is held, once per held episode (again only if the reason changes); the next `turn_started` closes the episode. Never written while a turn, a compaction or a queued settings command runs, nor for process ends alone | `turn` (the last turn started), `sources` (what is waiting: each `{"type": "mail", "mail", "from"}` for waking mail and `{"type": "process", "id"}` for process ends waiting beside it; a type a reader doesn't know reads as `unknown`), `reason` (`retry_offer`: a Retry offer stands; `held_messages`: your messages are held; `stopped`: the last turn was stopped or failed, or Esc fell in the settle window; `concurrency`: the agent's parent already runs its limit of children; a reason a reader doesn't know reads as `unknown`). An audit record: the rebuild doesn't read it |
| `agent_refused` | From version 28. The agent's spawn, send, stop, report or rename was refused, beside the tool error the model sees | `turn` (null outside a turn), `by` (the refused agent's path, `/` for the root), `action` (`spawn`, `send`, `stop`, `report`, `rename`; one a reader doesn't know reads as `unknown`), `reason`. An audit record: the rebuild doesn't read it |
| `agent_spawned` | From version 28. This agent started a child: through its `spawn_agent` tool, logged after the call's `tool_call_finished`, or through your `SpawnAgent` command | `turn` (null outside a turn), `agent` (the child's path), `session_id` (the child's own log), `depth`, `model`, `effort`, `cwd` (the child's working directory, `~`-shortened; absent when it has none) and `config` (as `session_started.parent.config`); additive with no version of its own, `endpoint` (the endpoint the child started on) and `provider` (its provider, such as `anthropic`), both absent in older logs: an absent `provider` is unrecorded, never read as `anthropic`. Resume uses the spawn's provider only if the child's log doesn't name the provider it last ran on; an unknown provider prevents resume only if it is the one selected (`docs/agents.md`, "Resume"). A fact about the session: a `rewound` that masks it changes nothing. The rebuild doesn't read it |
| `agent_ended` | From version 28 logs, without a bump of their own. This agent ended another: through `stop_agent` with `end: true`, logged after the call's `tool_call_finished`, or, in the main agent's log, through your `EndAgent` command; one record for the target and one for each agent below it (`docs/agents.md`, "Stopping and ending") | `turn` (null outside a turn), `agent` (the ended agent's path), `spent_usd` (its spend when it was ended; a stopped turn's last step may be priced after it, so the complete figure is in the ended agent's own log), and additively `plan_usd` (the subscription part of `spent_usd`: `0` when metered, absent when unknown; a record without it, such as one written before these fields, reads as an unknown split, never as a real charge) and `billing` (as on `agent_state_changed`; absent when `metered`). A fact about the session: a `rewound` that masks it changes nothing. The rebuild doesn't read it |
| `agent_renamed` | No bump: a reader that doesn't know it skips it. In the main agent's log, you renamed an agent with `RenameAgent` or main did with `rename_agent`, logged after the call's `tool_call_finished` (`docs/agents.md`, "Renaming"); one record for the target, the agents below it following by prefix | `turn` (null outside a turn), `agent` (its path before), `to` (its path after). A fact about the session: a `rewound` that masks it changes nothing, and a fallback carries it. The rebuild doesn't read it |
| `agent_reported` | From version 28, in a child agent's log. A turn ended and its parent was sent the report mail (`docs/agents.md`, "The parent's guarantee") | `turn`, `agent` (this agent's path), `outcome` (`reported`, `no_report` or `invalid_report`; one a reader doesn't know reads as `unknown`), `report` (from version 30 the report's whole text, a string, for `reported`; the last refused input for `invalid_report`; absent for `no_report`; up to version 29 a typed report object, whose `summary` a resume reads as its report line), `duration_ms` (the turn's). A fact about the session: a `rewound` that masks it changes nothing. The rebuild doesn't read it |
| `message_committed` | A message was appended to history | `turn`, `message`: the content blocks exactly as kept, thinking signatures included; `model` on assistant messages (the model that served the reply; absent before version 6), `stop_reason` (from version 10, `pause_turn` on an assistant message the provider paused, absent otherwise). One per committed message, in order: each assistant reply and each user message of tool results, so a turn with tools has several. From version 22, a file a tool attached (a `media` block, inside its `tool_result`) is logged by reference, never as base64: `{"type":"media","source":{"type":"blob","media_type":"application/pdf","sha256":"…","bytes":912004}}`, its bytes in the attachments folder. From v43 the block may also carry a [`slice`](#attachments). History in memory and every request keep base64. A quiet end (`docs/architecture.md`, "Quiet end"), Wisp's or one the model typed, is an assistant message holding `[no text]` alone—except signed Gemini quiet ends, which keep their Gemini native thinking and signature items before `[no text]` (older logs holding `[no text]` alone remain valid) |
| `tool_call_requested` | A committed reply asked for a tool call | `turn`, `step` (the request within the turn, from 1), `block` (the `tool_use` block's index in the reply), `call_id`, `tool`, `input` (verbatim), `display` (how clients show it; from version 27 it may carry a `view`, such as the `show_diff` diff, kept byte for byte). Additive, with no version of its own: `display.title` (the call's title from its own input, such as bash's `description`; absent when the tool set none and in older logs) the `process` view (`{"type":"process"}` with the call's `id`, `text`, `keys`, `signal`, `read` and `pattern`, each absent when not given; a refused call's view never has `text`), the `code` view (`{"type":"code"}` with `language`, absent when not given, and `code`), and `display.purpose`, the purpose the call's tool declared (`docs/tools.md`, "Adding a tool"): written only as `show`, for a tool that exists to show the user content; absent means `run`, so a `run` tool's record is byte for byte what it was, and a value a reader doesn't know reads as `run`. Resume takes each call's purpose from its tool as built now, and keeps the logged value for a tool that isn't. A reader that doesn't know a view type reads it as `unknown` |
| `tool_call_started` | A call is about to run | `turn`, `call_id`, `cwd` (shortened), and from version 17 `movable` (true when the user can move the running call to the background; absent when false and in older logs, and read as false). Additive, with no version of its own: `file`, the path of the call's output file, the same path `tool_call_finished.output.file` gives once the call ends (absent for a tool that saves no output, when the file couldn't be created, and in older logs). Like `output.file`, it is exempt from redaction. Written and synced before the tool starts |
| `tool_call_finished` | A call ended, ran or not | `turn`, `call_id`, `outcome` (`completed`, `timed_out`, `cancelled`, `not_run`, `invalid_input` or `failed`; a `not_run` outcome's `reason` is `reply_stopped` with its `stop_reason`, `turn_stopped`, from version 9 `after_question`: an earlier call of the same reply was a question, or from version 41 `after_refusal`: an earlier call of the same reply was refused and halted the reply), `output` (counts, the last lines, and the path of the file holding the full output; not the output itself; empty and inlined for a tool that saves no output), `facts`, `state` (a working-directory change), `duration_ms`, and from version 9 `result`: the tool's own result or a kernel replacement, such as an attachment refusal or an abandoned-call result (absent for ordinary captured output and in older logs). A newly masked call does not gain a `result`; when one already exists, it holds the final masked text without the held-key notice. The notice is kept in the model's committed text and in `notes`, not in the text the call's page draws. Additive, with no version of its own: `notes` (the lines Wisp added after the output, in order, such as `Working directory: ~/src` or `Working directory not tracked: ...`; `[]` when there were none, and absent in older logs), from which resume rebuilds a call's exact result when Wisp stopped before the results were committed. From version 17, `outcome` may be `backgrounded` (with a `reason`: `started`, `quiet` or `moved_by_user`, and from version 31 `still_running`, a foreground command handed back after 2 minutes): the call returned while its command kept running as a numbered process, and `process` is that number (absent for a call that gave no number and in older logs). From version 22, `attachments`: one `{"media_type", "sha256", "bytes", "saved"}` per file the call attached, in the order they follow its text in the `tool_result`, where `saved` is the file's path in the attachments folder (absent when it wasn't saved). From v43 each item may also carry a [`slice`](#attachments). The list is absent when the call attached nothing. A call that was refused for the media budget, cancelled or timed out attaches nothing. Additive, with no version of its own: `typed`, the verdict of a `process` call that wrote `input` or `keys` or sent a `signal`, as `{"echoed", "unsent", "signalled"}`: `echoed` is true only when every chunk written found the terminal in line mode with echo on, `unsent` that some of the input wasn't written, and `signalled` that the signal was really sent (`docs/tools.md`, `process`). Absent for calls that wrote nothing and sent no signal, and in older logs, which read as no verdict. For unmasked calls, `result` and `notes` remain the run's own text before `after-tool`; a hook's changes are in that call's `hook_applied`. For a newly masked call, `result`, if present, holds the finalized body after hooks and attachment checks, and `notes` ends with the held-key notice. Repair prefers that final body over an earlier hook change, or restores the notice at the end of the hooked or recovered text when `result` is absent. This repairs only uncommitted results; committed messages are never rewritten. A call blocked by a `before-tool` hook is `failed` with `before_start` and a `result` holding `Blocked by <hook>: <reason>` |
| `todos_updated` | From version 28 logs, without a bump of their own: readers skip record types they don't know, and it changes no rebuilt history. A completed `write_todos` call replaced the agent's checklist (`docs/tools.md`, "`write_todos`"). Written right after that call's `tool_call_finished` | `turn`, `call_id`, `items` (the whole checklist in kept order, unnamed items first then sections gathered in first-seen order: `{"text", "status"}` with optional `section` (additive, with no version of its own; omitted when absent) and `status` one of `pending`, `in_progress`, `completed` and `cancelled`). The checklist is the last unmasked one: a resume restores it and a rewind past it restores the one before (in a child, `session_started.parent.todos` when no update is kept; otherwise none); a masked `todos_updated` counts for nothing |
| `hook_ran` | From version 29 (`docs/plans/plugins.md` §2.8): one plugin hook ran at a point (`docs/plugins.md`, "Hooks"). Every turn whose opening runs the `shell` plugin's `before-turn` hook also logs a `shell/processes` run (`applied` with a `<processes>` notice, `none` when there is no news), unless that hook is switched off; a Retry that replays its notice logs none. Written as each awaited hook ends; a background hook's at the next drain moment; a run at an agent point (`agent-state`, say) like a background hook's, in the root's log only, with no `turn`, `step` or `call_id`; one cancelled by Esc, or a background hook aborted at quit (before `session_ended`), as `cancelled`; nested runs of a plugin point before the hook that fired them, which ends after them; one per hook a too-deep `points.fire` would have run, as `rejected` with `depth`. The bundled `time/sent` and `context/fill` are runs, so a turn that sends both lines logs two `applied` runs at `before-turn`. A turn's start points (`before-turn`, and the `settings-changed`, `model-switched` and deferred points drained there) are logged before its `turn_started` and after the seq its rewind point takes, so a rewind to before the turn masks them with it. Outside history: the rebuild doesn't read it, and a rewind that masks it drops it | `hook` (`<plugin>/<hook>`), `point` (the hook point, such as `before-turn`), `turn`, `step` and `call_id` (where it ran; each absent when it doesn't apply), `outcome` (`{"type": ...}`: `none`, `applied`, `blocked` with `reason`, `continued`, `failed` with `message`, `timed_out`, `rejected` with `rule`, or `cancelled`; at `context-threshold` a refused `compact` reply's `rule` is `request_failed`, `request_blocked` or `auto_paused`, each for the rest of its turn, `docs/context.md`, "Plugins and compaction"; a type a reader doesn't know reads as `unknown`), `ms` |
| `hook_applied` | From version 29, written from the release with the full tool hooks (no version of its own): the exact bytes of a change a hook made that no other record holds (a before-turn append is already in `turn_started.lead` under its hook's name). Logged whole, `before` and `after`, also when a redaction hook made it, and scrubbed like every record outside the conversation ("As a backstop", below). Outside history, like `hook_ran` | `hook`, `point`, `turn`, `step`, `call_id` (as in `hook_ran`), `change` (`{"type": ...}`: `input` with `before` and `after` (a tool call's input, as JSON), `result` with `before` and `after` (a tool result's text), `text` with `before` and `after` (an input's text), `continue` with `text`, or `blocked` with `reason`; a type a reader doesn't know reads as `unknown`); additive, with no version of its own: `input_id`, the input a `user-input` `text` change concerns, absent otherwise and in older logs). At `before-compact` a `text` change carries the summary instruction before and after the hook. Written only once the point's value is used: a change a later block or Esc threw away writes none, a `before-compact` change is written once the compaction goes ahead (a background one once its precompute has started), and an `after-tool` change is written after the call's attachments are settled, before its `tool_call_finished`. Resume reads it in two places only: a Retry resends a steer with its last `user-input` `text.after` rather than the typed text in `input_accepted`, and a call repaired after a kill takes its last `after-tool` `result.after`, unless a finalized masked `tool_call_finished.result` is present, which takes precedence with its held-key notice restored. Since every record is scrubbed, those may carry `[redacted]` where the live run sent one of Wisp's own credential values |
| `question_asked` | A running call asked the user questions, or a rewind or a resume reopened a posted question. Written and synced before the client is told, so a log can always show which questions were open | `turn` (the running turn, or the last turn started when reopened idle by a rewind or a resume), `call_id`, `questions`: in order, each `{"header", "question", "options", "multi_select"}`, with additive optional `source` (`{"asked_by": {"kind": "server", "name": "linear"}, "typed": true}`; false `typed` omitted), with `options` in order, each `{"label", "description"}` |
| `question_closed` | An open question ended (before `tool_call_finished` for a blocking card, or after it and possibly between turns for a posted question); each `question_asked` has at most one | `turn` (the running turn, else the last turn started), `call_id`, `how`: an object whose `type` is `answered` (with `answers`: one per question, in order, each `{"chosen", "own"}`, where `chosen` is the sorted option indexes and `own` the user's own words, absent when none; a skipped question is `{"chosen":[]}`), `withdrawn` (from version 40: withdrawn by the agent or dropped by a rewind), `stopped` (the user stopped the turn or closed a posted question, or a blocking call reached its time limit or returned while open), `message_instead` (written only by older versions: the user sent a message while it was open) or `interrupted` (reserved for resume, which closes a blocking question Wisp stopped during) |
| `turn_ended` | Every turn's last record | `turn`, `outcome` (status, stop reason, usage, cost with its lower-bound flags and optional `turn_billing`, `session_billing` and `agents_billing` provenance (each omitted for `metered`, the default when absent), and, in a main agent with children, the additive `agents_usd` and `agents_lower_bound` (every child's whole spend so far, absent when the session never had a child, and the flag absent when false; no version), `committed`, error kind and message, the error's `detail` described below, the retry fields below, the step counts below, and from version 20 `offer`, whose additive `retries_at`, with no version, is the Unix second at which a main agent retries a turn that stopped on a used-up limit by itself, a minute after the reset, and is absent otherwise), `stats` (start, first-byte and total times, `retries`: how many failed requests the turn re-sent), `partial_output` for a cancelled or failed turn (from its last attempt)  Optional Cost splits: `turn_plan_usd`, `session_plan_usd`, `agents_plan_usd`. |
| `session_ended` | Clean exit | `turns`. A log whose last run has none ended in a crash or kill. A resumed log holds one per run that ended cleanly. Additive, with no version of its own: `interrupted` (the turn the exit stopped while it was running, or a child's failed turn whose accepted Retry the exit dropped before it started, whether by hand or from a move, equal to `turns`; absent when the exit stopped no turn and dropped no such Retry, and in older logs, where a turn stopped by a quit reads as stopped). Resume treats the named turn as interrupted (`docs/agents.md`, "Resume"). In a child's log it ends a run, not the agent: a child ends only with the `agent_ended` its parent, an ancestor or you log, and a child that has only `session_ended` resumes with its tree |
| `session_resumed` | A run that resumed this log, in place of `session_started`, written before its first turn. A new record type that leaves the rebuilt history unchanged, so no bump: older readers skip it as an unknown type | `wisp_version`, `endpoint` (origin only) and `endpoint_name` (where this run continues), `system_prompt_sha256`, `tools` (sorted names, absent when none) and `tools_sha256` (the tools of this run's next request: those of a switch still waiting, else of the current pair), `instructions_sha256`, `cwd` (as in `session_started`), `gap_ms` (from the log's last record to the resume; absent when that time can't be read) and `truncated_bytes` (the cut last line dropped on reopening; absent when none). From version 32, `instructions_source` (the run's instructions source, written when it differs from the newest logged one: `session_started`'s, then each `session_resumed`'s) and `instructions` (the project block, byte for byte, written when this run's block differs from the newest logged `instructions_sha256` or the newest logged text couldn't be restored; absent when the block was restored, and when this run sends none, which `instructions_sha256`'s absence then says); and `instructions_extra` (the extra instructions' name, or `none`, written when this run's differ from the newest logged ones: `session_started`'s, then each `session_resumed`'s; absent means unchanged); all three are read back by the next resume From version 29, when its `system_prompt_sha256` or tools differ from the latest logged ones (the resume that turns binding controls on and rebuilds the cache, below, or one whose next request uses other tools without a rebuild, such as a pending endpoint switch or a logged endpoint that is gone), it also logs the new whole prefix: `system_prompt`, `tool_definitions` and `tool_context`, as in `session_started`, and `history_len` (the history length at the resume, before which the copy's tools apply; absent when 0, additive within version 36); a resume whose prefix is unchanged logs none of the three, so every prefix a session sends is logged whole exactly once. Additive, with no version of its own: `system_base_sha256` beside `system_prompt`, as in `session_started`, written only with the copy. Also additive: `tools_on`, the sorted names of the tools on when this run settled its tools (the copy's array can hold tools a note keeps off), written only with the copy and only in a session that switches tools; a deferred tool is in it only while on, so it never lists a tool only declared through `deferred_definitions`. A copy from `session_started` reads its array's names as the tools on; one from a `session_resumed` without `tools_on` reads none, and that resume seeds from the restored view instead. A resume reads the latest whole copy: `session_started`'s, or that of the latest `session_resumed` holding `system_prompt`; a record without `system_prompt` holds no copy, and neither does `settings_changed`; a `tools_changed` holding `system_prompt` (a rebuild at a tool change) is a whole copy too, and the latest of these wins. A full-screen root session that adopts the copy (`docs/tools.md`, "Resuming a session") sends its prefix unchanged and writes no copy, since its prefix is the logged one. The rebuild reads none of it: the history continues from the records before it. From version 35, `sections` (as in `session_started`: the start snapshots of the `system` prompt sections, present whenever this run sent the fresh project block rather than the logged one, even when empty), which becomes the base later resumes and rewinds read for those sections. Also additive: `returned`, the main agent's held message ids (including dropped turns, whose text comes from the dropped records), absent when empty; a window takes them with `ReturnedTaken` |
| `rewound` | From version 24. The user went back to an earlier point of the conversation (`docs/rewind.md`). Written after the rewind's `input_withdrawn` records and before its next turn. The rebuild masks every record after `to` and before this one: masked records add nothing to the history, except the few the masking rules below still read | `to` (the seq of the last record kept: the record before the point's `turn_started`), `point` (`{"type": "turn", "turn"}`: the turn whose message the conversation goes back to, which is dropped with every later one), `reason` (`user`; an unknown value is read as text), `messages` and `blocks` (the kept history's length and its last message's block count, which the rebuild checks: a mismatch fails the rebuild naming the turn), `dropped` (`{"turns", "messages"}`: the turns left and how many messages went with them), `cache` (the live `Rewound` event's cache effect: `{"type": "kept"}` when the point's cache was warm, so the next request expects reads from its baseline, otherwise `{"type": "rebuilt", "reason"}` with `model_switch`, `endpoint_switch`, `effort_change` or `rewind`; the resumed transcript replays it unchanged) |
| `window_lowered` | From version 25. The session kept the provider's stated context ceiling for that model and endpoint (`docs/context.md`, "A stated limit"). The window in use need not have fallen. Written with the live `WindowLowered` event, before its `context_changed`. A resume applies the last one for each endpoint and model, for that pair only, so the resumed session's point and `Context:` line match the live session's. A rewind's mask doesn't hide it: it is a fact about the endpoint | `endpoint` (the endpoint's name), `model` (as sent), `tokens` (the stated limit) |
| `resume_fell_back` | From version 26. A resume found damage it couldn't repair (`docs/plans/resilience.md` §7, 4.4b) and continued from the last turn that ended cleanly, or started fresh when no turn was kept. The first record that run appends, before its `session_resumed` (or, on a fresh start, its new `session_started`). Every later read drops the range it names, and ranges compose: one inside an earlier range still applies, and a damaged line inside a dropped range is not new damage | `kept_through` (the `seq` of the last kept record; null on a fresh start), `dropped_through` (the `seq` of the last dropped line, the one before this record; the dropped range runs from `kept_through + 1`, or 0, through it, counting damaged lines), `covered_through` (when the rebuild starts at a compaction head, the last `seq` the head covers, so damage below it is not read again; absent otherwise), `last_turn` (the last kept turn; null when none), `turns_seen` and `processes_seen` (the highest turn number and process id in any readable record, dropped ranges included, so numbering never repeats), `spent_usd` (a lower bound of what the dropped readable records show was spent), additively `plan_usd` and `billing` (the subscription part of `spent_usd` and the billing of the dropped records' usage, read from those records; `plan_usd` is `0` when they were metered and absent when the split is unknown; a record without it, such as one written before these fields, reads as an unknown split; `billing` absent when `metered`, and read back into the session's billing when `plan_usd` is present and `spent_usd` is above 0), `damage` (`[{"kind", "seq", "turn", "sha256"}]`, one per round of the search: `kind` is `unreadable`, `out_of_order`, `invalid_record`, `no_start`, `image`, `withdrawn_block` or `malformed_history`, a kind a reader doesn't know reads as `unknown`; `turn` when known; `sha256` only on `image`). No free text: a parse message could quote the record |
| `image_lost` | From version 26. An image or attached file a kept turn sent can no longer be read (its blob is missing, or its bytes or type don't match). The rebuild puts `note` where the image was, so later requests send the same bytes. Written by resume after its `session_resumed`; when the blob failed with an I/O error that may pass (permission denied, EIO), only just before the first request that sends the history, so quitting first leaves no record and the next resume tries the file again. At or after the active compaction head it turns thinking binding controls on, and on a pair whose controls fell back a `thinking_cut` from the image's message is written with it | `turn`, `sha256` (the lost image's), `note` (for example "[image no longer available: image/png, 1.2 MB]") |
| `native_compaction` | From version 19. A model first accepted or refused the native compaction beta on an endpoint, or (`fallen_back` only) an endpoint, the direct API included, later rejected it by shape or as an unexpected beta, a background compaction's request included (also emitted as `FeatureStateChanged` with `feature: native_compaction`), so a resumed session remembers a refusal and doesn't trial again | `model`, `state` (`proven` or `fallen_back`), `endpoint_name` |
| `inline_tools` | From version 36. A model first accepted or refused the inline tools beta (`inline-tools-2026-09-15`) on a gateway endpoint, or later rejected it by shape or as an unexpected beta; also records a listed official OpenAI API pair's first acceptance (`proven`) or refusal (`fallen_back`) of a sent `additional_tools` item, and a listed ChatGPT subscription pair's refusal (also emitted as `FeatureStateChanged` with `feature: inline_tools`), so a resumed session remembers the pair's state and doesn't trial again | `model`, `state` (`proven` or `fallen_back`), `endpoint_name` |
| `deferred_tools` | A new record type, so no schema bump: an older reader skips it. A request whose `tools` array declared tools with `defer_loading` was refused for it, so the step was resent without them and the pair stopped deferring for the session (also emitted as `FeatureStateChanged` with `feature: deferred_tools`, `docs/tools.md`, "Tools off at start: declared deferred"). Also written, with no event, where the pair is held from deferring: a gateway pair whose inline trial passed during a session with a deferred set, or a resumed pair whose logged array sent no deferred entries (`docs/tools.md`, "Resuming a session"). A resumed session remembers it | `model`, `state` (`fallen_back`), `endpoint_name` |
| `plugins_changed` | Plugins left, joined or were updated at the plugin moment, or the MCP service has child-change notes to show at an opening or inside a running turn. A new record type, so no schema bump: older readers skip it | `turn` (the running turn, or the turn the opening precedes), `added`, `updated`, `removed` and `kept` (each `{"id", "version", "source"}`, sorted by id; `updated` holds the new version, `removed` only plugins that were running, `kept` the version still running after an update that did not apply), `notes` (each `{"plugin", "message"}`, a plugin problem or an MCP service note); empty lists are omitted. Service notes keep their call order. Resume shows the record at the turn's edge, outside its content, so a rewind does not hide it; `turn` does not imply an opening. Sign-in `LinkNote`s are event-only and never appear in this record. |
| `tools_changed` | From version 36. The tools a running session offers changed at a turn's tool moment (`docs/tools.md`, "Switching tools in a running session"), after a search inside a turn, or when web search was dropped or restored for the pair | `turn`, `via` (`inline`, `turned_off` or `rebuild`), `added` and `removed` (tool names, each written only when not empty). History uses `turn_started.tool_message`, `message_committed` system messages and the turn's `core/tools` lead entry; resume also reads mid-turn inline changes as discovery marks. Resume reads its `added` and `removed` names, for `via` `inline` and `turned_off` only, to know which tools were on when the session closed: the tools on where the latest whole copy was written (`session_started`'s definitions, `session_resumed.tools_on`, or a rebuild's `tools_changed.tools_on`), adjusted by each such record after it, a record a rewind masked included, since a rewind keeps the tools applied (`docs/tools.md`, "Resuming a session"). A record a search refusal or a pair switch writes (`via: rebuild`) names only the tools. A rebuild at a tool change (`docs/tools.md`, "When a change can't go inline") also writes the whole new prefix, additive with no version of its own: `system_prompt`, `system_prompt_sha256` and `system_base_sha256`, `tools` (sorted names) and `tools_sha256` (the tools the next settings change would otherwise log), `tool_definitions`, `deferred_definitions` (the deferred set the rebuild keeps) and `tool_context` as in `session_resumed`, `history_len` (the history length at the rebuild, the point before which the copy's tools apply; absent when 0) and `tools_on` (the tools on after the rebuild); `added` and `removed` may then both be empty. A `web_search` flip that changes the pair's server tools is such a rebuild: it names `web_search` in `added` or `removed`, and its copy's `tools` follow it; a flip where the pair sends no search either way only sets the flag and writes nothing. Resume reads that copy as the latest whole copy, a record a rewind masked included, since the rebuilt prefix stays in use; a rewind to before `history_len` lowers it. Only the digests are exempt from redaction |
| `context_compacted` | From version 19. The context was compacted: the view the model sees became the head followed by the messages from `from` on (`docs/context.md`). Written once the head is in place: for a manual `/compact` when its reply has been read; for a compaction or Convert before a switch, before that turn's `turn_started`; for a Convert after a refusal in a turn, before that turn's next request (and, when the turn's own message wasn't kept yet, after its `turn_started`) | `turn` (absent for a manual `/compact`, which belongs to no turn; the turn's number for a compaction or Convert before or in a turn), `epoch` (from 1, one more than the last), `trigger` (`manual`, `auto`, `overflow`, `switch`, `convert` or `background`), `engine` (`native` or `wisp`), `focus` (the `/compact` text, absent without one), `before` and `after` (the fill before, and the head's estimate after), `partial` (true when turns after the old head were folded in without being summarised again; absent when false), `covers` (`{"first_seq", "last_seq", "first_turn", "last_turn"}`: the records and turns the summary covers; absent for a Convert), `from` (the history's length when the head was installed: every later message follows the head), `head` (the head user message exactly as sent: the native block first when there is one, then `<environment>` when the session has one, then `<compacted_context>`; never scrubbed, since its bytes are what a resume sends), `summary` (the text of a head in Wisp's form, including a `native` compaction that kept only the block's text) or `block` (the provider's compaction block verbatim, when the head holds it), `usage` and `cost_usd` (the compaction request's own, absent when no request was made), `duration_ms`, `switch` (on a `switch` compaction: `{"estimate_micros", "head_tokens", "cause"}`, the estimate it started with; absent otherwise). From version 23, for a background swap: `placement` (`leading`: `head` is an assistant message holding only the block, first in the view; absent means `head`), `cut` (the history index where the kept turns start; absent means `from`) and `additions` (`<environment>` and `<compacted_context>` exactly as sent: first in the first message from `from` on that isn't an effort marker, ahead of its own blocks, when message `from − 1` is a reply; when it is a user message (tool results, after a swap inside a turn or after a failed turn), inside it, right after the blocks it held when the swap was logged, so blocks joined to it later, a steer or the next turn's message, follow the additions; never in history, and never scrubbed). From version 35, `sections` (each `{"name", "snapshot"}`: the prompt sections the head carries, in resolved order, their texts placed after a native block and before `<compacted_context>`; absent when none), which become the sections' base in the new window; an older head's `<environment>` block is read as that section's legacy text, and an older head without one as seen with no text. Additive, with no version of its own: `instructions_by` (the `before-compact` hooks that changed the summary instruction, in run order; absent when none), `strategy` (the plugin whose compaction strategy laid out the head, its texts in place of the core's: `compaction` for `context.strategy = default`; absent when the core's minimal fallback laid it out), `instruction` (the summary instruction that strategy supplied, whole, before any `before-compact` hook changed it; absent when none) and `requested_by` (the `context-threshold` hook whose `compact` reply asked for this compaction, `<plugin>/<hook>`; absent for the core's own, a swap, `/compact` and every other trigger). A resumed transcript shows the `Compaction requested by` line again for a record that carries it. The texts `compaction-head` hooks appended are part of `head` (or of `additions` for a background swap), after `<compacted_context>`, so a resume or rewind rebuilds them without calling a hook |
| `compaction_prepared` | From version 23. A background compaction finished and its block waits for the swap (`docs/context.md`). Ignored on rebuild; an orphaned one, with no swap or discard after it, is never swapped | `turn` (the turn it overlapped; absent between turns), `epoch` (the epoch its swap would start), `covers`, `block` (the provider's compaction block verbatim), `usage`, `cost_usd`, `duration_ms` |
| `compaction_failed` | From version 19. A compaction ended without a head: it failed, was cancelled, or a queued `/compact` was cancelled or the session closed before it started | `turn` (as on `context_compacted`), `trigger`, `engine` (the engine whose leg failed: `wisp` when Wisp's engine failed after a native attempt fell back; absent when no engine was chosen: a queued `/compact` cancelled before it started), `kind` (`cancelled`, `{"discarded":{"reason"}}`, `{"no_summary":{"stop_reason"}}`, `check_failed`, `{"error":"<error kind>"}`, `blocked` (a `before-compact` hook vetoed it, the message naming the hook; additive, no version: an older reader reads it as its text), or a later kind read as its text), `message`, `usage` and `cost_usd` (the compaction's own, absent when no request was made; after a fallback, both legs' together: the native attempt and Wisp's), `switch` (as on `context_compacted`), and `lower_bound` (true when it may have cost more than `cost_usd`, such as a failure the endpoint may have billed without reporting usage; absent when false, and additive, so no version). Additive, with no version: `strategy` (the compaction strategy chosen for it, `docs/context.md`: `compaction` for `default`; absent when none was chosen), `instruction` (the instruction that strategy supplied, whole; absent when none) and `requested_by` (as on `context_compacted`: the hook whose request failed or was vetoed; a resumed transcript draws such a record's line again, and no other `compaction_failed`); a strategy's `accept` refusal is `check_failed` with its sentence whole in `message` |
| `rebuild_marked` | The request list's rebuild mark moved (`docs/context.md`, "The anchor and the explicit tail marker"): a change made the requests before it share no cached prefix with the next one. Written where the mark is set, before the next request is built: before the `turn_started` of the turn a switch or setting applies to, before the request after a fall-back, and after the `tail_closed`, `thinking_cut` or `rendering_form` that set it. A `search_history_unreadable` or `search_history_rendered` sets it with its record and writes it after the record and its `thinking_cut`, before the re-send; an `attachments_as_notes` sets it at the first event of the request that sent the notes and writes it after that record; one written by a refused compaction is written before the next request if the compaction fails, and not at all if it succeeds, since the compaction clears the list. A tail closed by a compaction writes none, since the compaction clears the list. A new record type that older readers skip, so it takes no schema version | `turn` (the turn it belongs to; the upcoming turn's number between turns), `reason` (informational text: `endpoint_switch`, `model_switch`, `per_message_effort_rejected`, `binding_controls_change`, `effort_change`, `tools_change`, `native_search_refused` (the server tools changed with no switch before it; the only such change today is the organisation refusing web search), `system_change`, `tail_closed`, `thinking_cut`, `rendering_form`, `search_history_unreadable`, `search_history_rendered` or `attachments_as_notes`; a later reason reads as its text) |
| `tail_closed` | From version 10. The kernel closed the server calls (web searches) a stopped, paused, failed or refused turn left without a result, or that a Convert in a turn left out of the view, so they are never run later. Written before anything is appended after the tail: before a stopped turn's `turn_ended`, or before the next turn's `turn_started`, a compaction's `context_compacted` or `session_ended` | `through` (the history position of the tail's last message), `calls` (the closed `server_tool_use` ids), `reason` (`stopped`, `paused`, `failed`, `refused` or `compacted`, parsed strictly) |
| `rendering_form` | From version 10. A plainer form for closed server calls was accepted after the API refused the previous one; session-wide, the last one holds | `form` (`text`, `without_thinking` or `flattened`) |
| `thinking_cut` | From version 10. A request changed bytes already sent, so it left out every thinking block from that point to the end of history; one record per cut, all applied in order. Written only where a cut is made: on a pair where thinking binding controls are usable, a deliberate change keeps every thinking block and writes none (`docs/context.md`, "Broken thinking"); an endpoint refusing earlier thinking is cut on any pair. A closure's cut is written straight after its `tail_closed`, and only when the closed range holds thinking | `from` (`{"message", "block"}`: the history position of the first changed byte, or `{"message":0,"block":0}` for a tools change and for the one cut a session makes when it moves off binding controls after it kept broken thinking, and for the cut after an endpoint refused earlier thinking, which covers every thinking block in history and sets binding controls back to the value its turn's `turn_started` logged), `through` (the last history message the cut covers), `tools_sha256` (on a cut caused by a tools change, the tools it moves to; absent otherwise) |
| `search_history_rendered` | From version 10. An endpoint refused web search blocks produced on another endpoint, so they are sent to it as text from then on; one per endpoint | `endpoint_name` |
| `attachments_as_notes` | From version 39. The first request on an endpoint and model that sent a note in place of each image and file was accepted (`docs/media.md`, "Attachments a model can't take"); one per endpoint and model | `endpoint_name`, `model` |
| `search_history_unreadable` | From version 33. An endpoint refused web search blocks it can no longer read (a 400 naming `encrypted_content` or `encrypted_index` with nothing foreign left to render), so every web search block (calls, results and cited text) in the history messages committed before this record is sent to it as text from then on, on every later request: steps, retries, both compaction engines, background compaction and the whole-context count. Written before the re-send. Never reverts; a later record on the same endpoint moves the boundary to its own position, and messages committed after it are sent unchanged. On resume the boundary is the number of rebuilt messages committed before the record, so a rewind past it clamps it as the live session does | `endpoint_name` |
| `native_web_search` | The endpoint refused the web search tool: a request carrying the server tool was refused, because the endpoint's organisation has web search switched off or a gateway doesn't pass the tool on, so it was re-sent without it, and every model on that endpoint sends no web search tool for the rest of the session. Written once per endpoint: before the re-send for the organisation's refusal, and for a gateway's suspected refusal only once the re-send's reply starts (a re-send that fails writes nothing) ([docs/endpoints.md](endpoints.md), "A gateway refusing the tool"). Also emitted as `FeatureStateChanged` with `feature: native_web_search` and the refused request's `model`. A new record type, so no schema bump: an older reader ignores it, and never sends the tool | `endpoint_name`, `state` (`fallen_back`) |
| `web_search_not_run` | A step's request carried the provider's search tool and the reply called a client tool of that name: the endpoint passed the tool to a model that can't run it, and Wisp answered the call with an error result. Written once per endpoint per session, at that step; also emitted as `AgentEvent::SearchNotRun` (`endpoint_name`, `model`). Resume doesn't read it, so a resumed session may write it, and show the notice, once more. A new record type, so no schema bump: an older reader ignores it | `turn`, `endpoint_name`, `model` (the model that served the reply) |
| `system_marker_dropped` | From version 10. An endpoint refused a request for too many cache breakpoints, so requests on it omit the base system block's 1h marker from then on | `endpoint_name` |
| `search_started` | From version 12. A web search call finished streaming, so its search began | `turn`, `step` (the step whose reply holds the call), `attempt` (the step's retries before it: `0` on its first request, so a reader can drop a discarded attempt's rows), `call_id` (the `server_tool_use` id), `query` (a page's URL when it opens one), `action` (absent for a search; `open_page` or `find_in_page`, and an unknown name reads as `other`; added without a version bump) |
| `search_ended` | From version 12. A web search ended: its result arrived, or the kernel closed it unfinished | `turn` (absent when the call was closed between turns: before the next turn's `turn_started`, or before `session_ended`), `step` and `attempt` (both `0` when closed between turns, since no step is running), `call_id`, `outcome` (`{"status":"done", ...}` with the result normalised as `{"type":"results","hits":[{"title","url","page_age"}]}` or `{"type":"failed","failure","code"}`; or `{"status":"unfinished","reason","possibly_billed"}`, with `reason` as in `tail_closed`, parsed strictly). An unfinished one is written just before its `tail_closed` |
| `citations` | From version 12. A reply's text block cited web search results | `turn`, `step`, `attempt`, `index` (the block's index within its reply), `sources` (one `{"url","title"}` per distinct cited URL, in order of first citation) |
| `instructions_loaded` | From version 13. Written once, right after `session_started`, by every logged session built with instructions (the CLI builds every session with a Workspace that way, with nothing chosen when there is no file); the `AgentEvent::InstructionsLoaded` event as logged (`docs/instructions.md`) | `turn` (always null: before any turn), `chosen` (the file that loaded, absent when none: `{"scope", "name", "display_path", "bytes", "sha256", "imported_by", "external"}`, with `imported_by` absent on the chosen file and `external` absent when false; `bytes` is the size on disk and `sha256` the hash of the normalised text), `imports` (the imported files, in block order, the same shape), `candidates` (the project and default paths looked at, each `{"scope", "display_path", "bytes", "mtime", "sha256"}` with the last three absent when the file doesn't exist; `mtime` in nanoseconds since the Unix epoch), `diagnostics` (`{"source", "kind", "message"}`, `kind` one of `not_a_file`, `not_utf8`, `unreadable`, `import_missing`, `import_too_deep` and `large`). The last three are absent when empty. `scope` (`project` or `default`) and `kind` are open: a value a reader doesn't know reads as its text, so a later scope or kind needs no bump |
| `instructions_changed` | Additive, no bump: an older reader skips it. A display record, written just before the `turn_started` of a new turn whose lead carries the `instructions/instructions` entry, or just before the `context_compacted` of a head that carries one; resume never reads it (the baseline is the section's snapshots), so it is scrubbed like any record | `turn` (null for a head between turns), `epoch` (a head's), `retry_of` (never written today), `ahead` (true for a head built to an unsaved turn's target; absent when false), `from` and `to` (the chosen file before and after, as in `instructions_loaded`), `changed` (`{"display_path", "old_sha256", "new_sha256"}`, a hash absent for a file added or removed), `chosen`, `imports`, `candidates` and `diagnostics` (the set after the change, as in `instructions_loaded`; a head built to a target has no diagnostics), `sent` (always true) and `text` (the `<instructions-update>` block as sent). The update itself is the lead entry `instructions/instructions` with its snapshot, which plays the part of the plan's `turn_started.context_delta` |
| `process_started` | From version 17. A command became a numbered process that keeps running after its call | `turn`, `id` (the process number, as the model and the user see it), `call_id` (the call that started or handed it back), `reason` (`started`, `quiet` or `moved_by_user`, and from version 31 `still_running`), `tty` (whether it runs in a terminal), `stdin` (whether it takes input), `size` (`{"cols", "rows"}` for a terminal, absent otherwise), `file` (where its whole output is saved; absent when none), and, for resume's cleanup only, `pid`, `sid`, `start_ticks` and `boot_id` (local values, never sent anywhere; each absent when it couldn't be read); `keeps_running: true` and `log` (the raw log's absolute path) for a process started with `keep_running`, both absent otherwise (added without a version bump) |
| `process_ended` | From version 17. A numbered process ended; each `process_started` has at most one. Written by the live session, or with `lost` by resume | `id`, `end` (`{"type":"exited","exit":...}` with `exit` as in a completed outcome, `{"type":"stopped","by":...}` with `by` one of `{"type":"model"}`, `{"type":"user"}`, `{"type":"session_ended"}` and `{"type":"time_limit","after_ms":...}`, `{"type":"lost","reason":...}` with `reason` `stopped_at_resume` or `gone`, or `{"type":"failed","message":...}`), `bytes` and `lines` (its whole output), `duration_ms` |
| `process_resized` | From version 17. A terminal process changed size | `id`, `size` |
| `process_attached` | From version 17. The user attached to a terminal process to type into it | `id`, `size` |
| `process_detached` | From version 17. The user's attachment ended | `id`, `size`, `reason` (`user`, `ended` or `disconnected`), `typed` (whether the user sent any input while attached). Additive, with no version of its own: `answered` (the detach answered the model's step-in request for this process, so the typing was told in that call's result; absent when false). A typed detach that didn't answer a request owes the model a `You typed into process N` line until a `process_heard.typed` names the process; resume offers it again in the first turn's notice. A `process_attached` with no `process_detached` after it counts as detached at the session's end |
| `process_heard` | From version 17. A just-committed block reported the ends of these processes to the model: a `<processes>` notice, a line after a `bash` or `process` result, a `process` answer or list that shows the end, or a background start's result for a command that ended within its start window. Written right after that block's `message_committed` (for a turn's own user blocks, the reply's, which places them; for a step's tool results, after the next reply is saved; if the turn ends first, no record is written and the ends are given back), at most one per `message_committed` | `turn`, `ids` (ascending, without repeats). Additive, with no version of its own: `typed` (ascending, without repeats; absent when empty): the processes whose `You typed into process N` line the block carried; a record may carry only `typed`, with `ids` empty. Outside history: the rebuild doesn't read it |
| `process_needs_input` | From version 17. A terminal process started reading input it doesn't show, almost always a password prompt | `id`, `kind` (`hidden_input`). No screen text |
| `process_needs_input_cleared` | From version 17. That state ended without the process ending | `id` |
| `endpoint_saved` | From version 18. An endpoint added mid-session (`/endpoint add`) was saved | `turn` (null outside a turn), `name`, `store` (`keyring` or `file`; a value a reader doesn't know reads as its text), `check` (`{"outcome", "reason"}`: `ok` or `skipped`, with `reason` absent when empty). Never the key, the URL or the event's notes. The save that sets up the first endpoint happens before the session starts and is never logged |
| `endpoint_not_saved` | From version 18. A mid-session endpoint save, edit or removal stopped before anything was written | `turn`, `name`, `why`: `{"check": {"outcome", "reason"}}` when the free check failed (`key_refused`, `forbidden`, `not_offered`, `unexpected` or `unreachable`), or `{"keychain_failed": "<which>"}` when the keychain didn't take the key. An unknown `why` or `outcome` reads as its value |
| `endpoint_edited` | From version 23 logs, without a bump of their own: readers skip record types they don't know, and it changes no rebuilt history. An endpoint's connection fields were changed mid-session | `turn`, `name`, `changed` (the fields set: `provider`, `auth`, `url`, `key`, `store`, `key_env`, `url_env`; a field a reader doesn't know reads as its text), `store` (where the key is kept afterwards; absent for an endpoint that reads its key from a variable), `check` (as on `endpoint_saved`). Never the key, the URL, a variable's value or the event's notes |
| `endpoint_removed` | From version 23 logs, without a bump of their own, as `endpoint_edited`. An endpoint was removed mid-session | `turn`, `name`, `kept_item` (its keychain item was left in place), `default_cleared` (it was the endpoint Wisp starts on, so `config.toml` names none now). Never the keychain account or the event's notes |
| `endpoints_changed` | From version 18. The session's endpoint list changed after a save, an edit or a removal | `turn`, `endpoints`: one `{"name", "provider", "credential", "official", "host", "unavailable", "priced", "default", "keys_file", "expired", "key_var", "mark", "models", "complete", "default_model", "voice_model"}` per endpoint, the optional fields absent when unknown or false. `keys_file` is true for the account Wisp's keys file defines; `expired` is true when the account's sign-in was refused; `key_var` is `{"name", "in_keys_file"}`, the variable the key is read from as Wisp shows it, with `in_keys_file` true when Wisp's keys file sets that variable rather than the shell (a value that looks like a key reads `(looks like a key, not shown)`), never the key; `mark` is the account's colour on screen, 1 to 6 (`docs/endpoints.md`, "What you see"), additive with no version of its own; `models` is the account's own model list (`{"id", "listed"}` per entry, `listed` the shown rank, absent for a hidden entry), absent when it has none, also additive; `complete` is true when that list is everything the account serves; `default_model` is the account's default model ID, absent when unknown; `voice_model` is the account's voice model, absent when its provider offers no voice, also additive. It changes no history and no request bytes: the current endpoint keeps its prefix |

`ErrorDetail.stream` is optional, additive stream-end metadata in the existing
`retrying.detail` and final error detail, with no schema bump or screen change.
Only Gemini fills it today, when the body ends without `finishReason`, with an
unfinished SSE event, or with an invalid finish (`MALFORMED_FUNCTION_CALL`,
`UNEXPECTED_TOOL_CALL`, or an empty `STOP`, where `stop_reason_seen` is `true`
and `partial_event` is `false`); it is absent on every success. `bytes` counts body
bytes and `events` counts complete SSE data events, not network reads.
`text_parts`, `thinking_parts` and `tool_use_parts` count received candidate
parts across events, including wrapped responses; a function call takes
precedence over a thought, then a string text part. `last_event_keys` merges
the last parsed JSON event's top-level keys with its `response` object's own
keys, when present. Only names of 1–64 ASCII bytes matching
`[A-Za-z_][A-Za-z0-9_]*` are kept; other keys are omitted, not truncated. Names
are sorted, deduplicated and then capped at 16 in total across both objects;
no deeper keys or values are included. `partial_event` tells a
mid-event end from a clean boundary; `stop_reason_seen` records whether a
finish was decoded. Optional `first_byte_ms` and `last_byte_ms` measure from
decoder creation just after accepting HTTP 200, not from request start; both
are absent when no body bytes arrived. No text, thought, arguments, signatures,
headers or URLs are added. Old records omit `stream`; older readers ignore it.

`ErrorDetail.malformed_call` is optional, additive label metadata in
`retrying.detail` and final error detail, with no schema bump or screen change.
Gemini fills it on `MALFORMED_FUNCTION_CALL` only, reading the candidate's
`finishMessage` against the sent request's declared tool names and stop
sequences: `form` (`absent`, `call`, `python`, `empty_call`, or `other`; unknown
values read as `other`), optional `function` (present only when the parsed name
is a declared tool, scrubbed by redactions), `undeclared` (true when a parsed
name was not declared), `chars` (`finishMessage` length in Unicode scalar
values), `balanced` (whether `{`, `[`, `(` and quoted `"`/`'` strings with
backslash escapes close in order; true for `absent`), `stop_string` (whether a
request stop sequence occurs in `finishMessage`), and `mail_tag` (whether
`<wisp:message` occurs in it). Absent `function` and false booleans are omitted
on the wire; no text from `finishMessage` or user content is recorded.

`session_started.wisp_version` and `session_resumed.wisp_version` carry
`0.1.0+<commit>` (the 12-character short `HEAD` hash captured by `wisp-start` at
build time) when built from a git checkout, or plain `0.1.0` when unset; the id
names `HEAD` even when the build's tree had uncommitted changes. User-Agent and
MCP headers keep the plain version.

`Cost.turn_plan_usd`, `Cost.session_plan_usd` and `Cost.agents_plan_usd` are optional subscription parts of the existing totals. Metered-only sessions omit `session_plan_usd`; `agents_plan_usd` is absent before the first child. Mixed totals display API spend and subscription estimates separately. Compaction records carry optional `plan_usd` to preserve their split across resume; `ContextReport.compaction_plan_usd` exposes the accumulated compaction split. A hook-requested compaction carries the same `usage.billing`, `cost_usd` and `plan_usd` as a core-requested one; `requested_by` identifies its requesting hook and does not change its billing. Plugin-owned instructions, validation and head layout use these same accounting paths. These additive schema-36 fields default to absent for old records.

Every record that prices model usage (`step_ended`, `retrying`, `cache_refresh`, `session_named`, `model_asked`, `voice_call`, `context_compacted`, `compaction_prepared` and `compaction_failed`) also carries two additive fields, with no schema bump: `model` (the model that served the request, absent when unknown; `retrying` always had it), so a step on a model Wisp has no price for can be priced later, and `list_usd` (the API-equivalent estimate on an endpoint set to `pricing = "none"`, written only when `cost_usd` is absent and Wisp has a price for the model). `list_usd` is for statistics only: resume, the turn and session totals and every display ignore it, so such an endpoint stays tokens-only on screen, and no event or client sees it. A statistic reads `cost_usd`, else `list_usd`, with `usage.billing` saying whether the amount was charged or is an estimate on a subscription; usage with neither amount is unpriced. To total a session, sum `step_ended`, `cache_refresh`, `session_named`, `model_asked`, `voice_call`, `context_compacted`, `compaction_prepared` and `compaction_failed`, and the `retrying` records that carry a `step` above `0`: a `retrying` record without a `step` (or with `step` `0`, in logs written before automatic compaction) is a compaction's attempt, and its usage and amounts are already inside that compaction's record, so it is skipped. A background compaction's summary is counted once, on `compaction_prepared`; the `context_compacted` that swaps it in carries no usage. The real and estimated amounts stay separate fields and are never summed. `Cost.*_usd` totals sum every request at API prices, `*_plan_usd` is their subscription part and the real spend is the difference; the per-request records, summed as above, are the source for statistics. `agent_ended` and `resume_fell_back` carry `plan_usd` and `billing` beside `spent_usd`, so the split survives a child log that can't be read: resume uses the `agent_ended` pair for such a child.

`context_compacted` and `compaction_prepared` also carry `lower_bound`, additive with no schema bump, like `compaction_failed`'s: `true` when the record's cost is a lower bound, absent otherwise, so an older log reads as before. Resume adds it to the session's, its account's and the turn's lower bound.

Billing provenance is additive and needs no schema bump. Its wire values are
`metered`, `chat_gpt_plan`, `anthropic_plan` and `mixed`; omitted fields read as
`metered`. `Usage.billing` accompanies usage wherever it is logged, including
steps, retries, cache refreshes, compactions and turn outcomes. In
`turn_ended.outcome.cost`, `turn_billing` describes `turn_usd`, `session_billing`
describes `session_usd`, and `agents_billing` independently describes
`agents_usd` (the child-tree total, not the root's billing mode). Plan figures
are API-equivalent estimates, not charges; `mixed` must not be interpreted as
an entirely metered or entirely subscription total. `cache_miss_repeated.billing`
is retained when that notice is replayed.

`ContextReport.billing` describes estimates for the current endpoint, while
`ContextReport.spent_billing` labels historical compaction spend using the
session's accumulated billing provenance. Both are optional on the wire,
omitted for `metered` and defaulting to it when absent. `ContextReport` is a
client event, not a persisted `LogEvent`, so it has no record row above.
Likewise, `AgentEntry.billing` and `AgentStateChanged.billing` are client/tree
fields rather than new log-record fields; see [Agents](agents.md).

Streamed text and thinking deltas, tool output, process output and
per-attempt events are not logged; `message_committed` carries the finished message and
`tool_call_finished` points at the output file.

A turn runs as steps, one request each. A step's reply is committed once it
has streamed in full, and its tool results once its calls have ended, with a
synthetic result for any call that was cut short or never ran. A cancelled or
failed turn keeps every committed step and loses only a reply that was still
streaming. The outcome counts them:

- `completed.steps` is the number of requests the turn sent and
  `completed.tool_calls` the number of tool calls its committed replies asked
  for (both absent before version 3, and read as 1 and 0);
- `cancelled.committed_steps` and `error.committed_steps` count the replies the
  turn kept (absent before version 3, read as 0).

A failed turn's `outcome.kind` is one of `auth`, `rate_limit`, `overloaded`,
`invalid_request`, `network`, `refusal`, `invalid_response`,
`thinking_binding`, `server`, `timeout`, `billing`, `interrupted`,
`context_overflow` or `other`. `other` is kept for an error type Wisp has never
seen. `context_overflow` is a request the context window can't hold: an error
whose type or code is `context_length_exceeded`, or an `invalid_request_error`
whose message says the prompt is too long (Anthropic's form), whether it came as
an HTTP 400, as an error object in a 200 or as a stream error event. A 400 whose
error has no type or code Wisp knows (a gateway's `"type": null`, with an HTTP
400 or a `"code": "400"`) and whose message says the prompt is too long is one
too. When the failure came from a model request, `outcome.detail` says what
Wisp knows about it (absent for failures Wisp raised itself, such as a refusal
or a malformed history, and before version 7):

- `phase`: `connect` (nothing was sent), `response` (the request was sent but
  no successful reply arrived, including an error object that is the whole
  body of a 200 reply) or `stream` (a 200 reply arrived and then failed);
- `source`: `local` (Wisp's own connection: a refused connect, a reset, a
  timeout, a cut or malformed stream) or `endpoint` (an error the server sent,
  whoever runs it);
- `status`: the HTTP status, or the status inside the error object (absent
  when no response arrived);
- `error_type`: the error's name when Wisp knows it (`api_error`,
  `overloaded_error`, `rate_limit_exceeded`, ...), else the body's own type,
  never an unknown raw code. The recorded name is scrubbed and capped at 64
  characters. A statusless `invalid_request` is
  recorded by that name when no stronger classification applies, even if
  supplied only as a code;
- `request_id`: from the `request-id` header, or else the error body; for
  OpenAI, only an `x-request-id` with the `req_` shape. Wisp keeps one line,
  capped at 64 characters after redacting registered secrets and known-prefix
  tokens. It skips the key-shape and account-ID checks so request IDs such as
  UUIDs stay useful when contacting support;
- `model`: the model the session was on when the server refused the request,
  as stored; additive with no schema bump, and absent for `local` failures and
  in older logs;
- `retry`: whether the same request could succeed later: `{"type":"never"}`,
  `{"type":"transient"}` or `{"type":"after","ms":...}` (from `retry-after-ms`
  or `retry-after` in seconds; an HTTP date counts as absent);
- `possibly_billed`: whether the failed request may have been charged.

A failed turn whose last failure came from a model request also carries
`outcome.attempts` (how many times that step was sent) and, when a policy
ended the retries, `outcome.gave_up`: `not_retryable` means the same request
would fail again, and `budget` means the next attempt would exceed the
opt-in `--retry-budget`. A typed subscription limit past its default wait
bound also stops with `retry_after_too_long`, and retryable
`invalid_response` failures stop with `step_limit` after 3 retries per step
(`MALFORMED_FUNCTION_CALL` included) even
under default or looser policies (see [Busy endpoints](endpoints.md#busy-endpoints)).
The finite-policy values are `step_limit`, `turn_limit`, `retry_after_too_long`
and, from version 34, `busy_window`. Their former defaults (5 step retries, 10 turn retries,
a 60 s Retry-After ceiling and a 10-minute busy window) remain the fallback
and native-compaction bounds, not the meaning of custom-policy values;
background compaction uses the counted bounds without busy waits.

Ordinary requests are otherwise unbounded by default: they stop for `never`,
`step_limit` on retryable `invalid_response` (3 retries per step), a budget,
or the user's stop; typed subscription limits use the bounds linked above. When an opt-in budget cannot be priced, the finite
fallback counts apply and the error says the budget could not be checked.
In the main agent, Esc during a model retry wait ends with the last failure and an offer;
`gave_up` is absent, since no policy limit ended it. Closing the command
channel or cancelling an in-flight request still ends cancelled, without
an offer. A stopped wait's failed attempt was already charged in its
`retrying` record; `step_ended` does not charge it again. These fields are
absent for other failures and in older logs. Ordinary retries change no committed
message, so they leave the rebuild untouched; after a Gemini
`MALFORMED_FUNCTION_CALL` whose retry succeeds, the fixed retry note is logged
in `input_committed` under an internal `wisp-call-note-<turn>-<step>` ID so
rebuild appends it to the same user message while resume skips drawing it or
recording it as user-said text.

Error objects are read in the Anthropic shape
(`{"type":"error","error":{"type","message"},"request_id"}`), the OpenAI shape
(`{"error":{"message","type","code","status"}}` or `{"error":"text"}`) and as a
typed event (`{"type":"error","code","message"}`), in an error response, a 200
JSON body or a 200 stream. A 200 reply that holds only text, with no error
object, is `invalid_response` and shows that text.

A turn whose reply stopped with tool calls still pending, or that was
cancelled while a call ran, ends with a user message of tool results: an
open tail. Every call has exactly one result; calls that never ran get a
`Not run: ...` result.

`tool_call_started` is synced to disk before the tool starts. A log with
`tool_call_started` and no matching `tool_call_finished` means Wisp stopped
while the call ran, and its effects are unknown. A call with a
`question_asked` record and no `question_closed` or `tool_call_finished` was
waiting for the user's answer when Wisp stopped, which has no effects; a posted
question whose `tool_call_finished` was logged keeps its logged result and
reopens on resume.

Resume closes a turn Wisp stopped in, appending only. When the last turn has
no `turn_ended`, the first session start after `session_resumed` writes:

- for the unanswered `tool_use` blocks of the last committed reply, in block
  order, one user `message_committed` of results: a call's logged `result`
  verbatim, followed by the files its `attachments` list, read back from the
  attachments folder as the live result carried them (a file whose saved
  copy is gone becomes the note `[{target} ({media type}) couldn't be
  processed: its saved copy was gone when Wisp resumed. ...]`, ending as a
  refused attachment's note does, `docs/media.md`, and the resume goes on); for a blocking question still open (no `tool_call_finished`), `Not answered:
  Wisp stopped while this question was open.` after a `question_closed` with
  `how: interrupted` (a posted question with `tool_call_finished` keeps its
  logged result and reopens); for
  a question closed but not finished, the text its ending gives; for a
  finished call with `notes`, the exact text the turn would have sent (or
  `Recovered after Wisp stopped: ...` when its output was cut to a tail), and
  for an older one without them, `Recovered after Wisp stopped: ...` with
  the outcome, the last lines of the output and its notes, citing the full
  output file only if it still exists; `Unknown: Wisp stopped while this
  call was running; its effects are unknown.` for a call that started and
  never finished (with ` It was running as process 4.` after it when its
  command had become a numbered process), and `Not run: Wisp stopped before this call ran.` for one
  that never started;
- a `tail_closed` for web searches left open (reason `stopped`);
- `input_withdrawn` with `by: session_ended` for messages still waiting;
  messages still waiting or not kept in the cut turn (unless Retry resends them) are handed back on resume
  until a window takes them, without a second withdrawal. Each main-agent
  `session_resumed` lists held message ids, including dropped turns whose text
  comes from the dropped records; `returned_taken` removes them.
  A client must not reuse an untaken returned message's id;
- `turn_ended` with an `interrupted` error, the steps and retries the log
  holds (an automatic compaction's retries count in its turn, as they do
  live), a cost that is a lower bound, and a Retry offer. The Retry sends
  what the live one would: the turn's inputs and, when its user message
  never committed, its logged `shell/processes` notice again. No Retry is
  offered when nothing is left to send again: the rebuilt history ends in
  a final reply, and no opening inputs, delivered steers or uncommitted
  notice are waiting (so a failed turn Wisp started for ended processes is
  offered again). A paused reply whose search resume just closed is
  offered, as a live failed turn is: the Retry sends the closed reply and
  then the closed-pause note (the rebuild rule below), since a request
  never ends in a closed paused reply. A
  turn whose final reply was committed before Wisp stopped ends with
  `Wisp stopped after the reply finished, before this turn ended.`; every
  other turn with `Wisp stopped before this turn ended.`. The same rule
  decides whether a later resume restores the offer.

Each record is also emitted as its event, before the first command is read,
so a client sees a `TurnEnded` for a turn this process never started. Right
after writing `session_resumed`, and before any of these, the kernel emits
`SessionResumed { turns }`: the number of the log's last turn. A client
takes its turn count from it, so the full screen's Retry on the closed turn
names that turn, and the next message's turn is `turns + 1`. The `jsonl`
loop waits for the `TurnStarted` of the message it sent before a `TurnEnded`
ends that message's turn.

When the last turn did end, a web search an error left open is closed as
`failed`, billed as that error said, and messages still waiting are
withdrawn the same way. A failed or interrupted last turn's Retry offer is
recomputed from the log: it is gone once a message was accepted or released
after the failure (`input_accepted`, `input_unheld`) or the conversation was
compacted, and Retry sends what the old process would have sent, including
the messages `input_delivered` lists.

When a call whose tool ends the reply (a question) has finished, however it
ended, every later call of the same reply is `not_run` with reason
`after_question`, so nothing runs before the model has read the answers.
Likewise, when a refused `stop_agent` with `end: true` halts the reply, every
later call of the same reply is `not_run` with reason `after_refusal`. A
stopped turn still gives `turn_stopped`.

## What is never logged

The live `InputAccepted.opening` flag is set only on the unlogged event for a
direct idle input in a hosted session, sent before `TurnStarted` so a new
window can replay it. The host's internal opt-in applies to new and resumed
children too; it appears in no prompt, request or log record.
It is false and omitted on other acceptance events. The input is still recorded
once in the ordinary `turn_started` record; no new acceptance record is added.

Credentials, request headers, env-file contents, and anything in the endpoint
URL beyond scheme, host and port (user info, path and query are dropped).
Error messages never include the request URL.

One in-memory process list holds credentials and log-only values; it is filled even without a log and is never itself logged or written to disk.
Before commit, tool results mask held credentials of 20 characters or more and keep that masked text, and its note, unchanged on later turns and resume.

As a backstop, content strings in records are scrubbed of registered values,
including credentials, their base64 forms when the credential has 20 characters or more,
held server-key values and the endpoint's password, path and query, which are replaced with `[redacted]`.
A value spelled with JSON's string escapes (`\uXXXX`, surrogate pairs and `\" \\ \/ \b \f \n \r \t`)
or Rust's `\u{…}` is caught too, nested up to four escape levels deep. A value needs at least three
non-whitespace Unicode characters to be masked; shorter values are left as-is,
including their debug-escaped forms. Everyday values of three or more characters
can still mask matching content. Record tags, enum values, object keys, hashes,
identifiers used for replay or pairing (such as call IDs and agent paths), and
tool, plugin, hook and hook-point names, versions and timestamps are never scrubbed.
Opaque JSON payloads keep their keys and structure, but their string values are
scrubbed. Existing logs are not rewritten.
Image files aren't scanned; they hold only the re-encoded
pixels, with no metadata. The conversation itself (`turn_started`,
`message_committed`, `input_accepted` and `input_committed`) is exempt, so that it stays
byte-identical: messages are stored as sent, and tool results as sent after the held-key
mask (`docs/tools.md`, "What running tools means today"), so a secret the user typed,
or one a tool printed that Wisp doesn't hold, is in the log. So are
the user's questions and answers and the results tools write themselves:
`question_asked.questions`, `question_closed.how` and
`tool_call_finished.result`, while the other fields of those records are
scrubbed as before, `notes` and the output's last lines included: a result
resume rebuilds from a scrubbed tail shows `[redacted]` where the live one
showed an unmasked value, which costs no cache, since no accepted request carried
it. A server's card remains exempt too, so resume can redraw it, but its question and answer never join `message_committed` or compaction's kept user messages; an unfinished call gets an unknown-outcome result rather than its answer.
An older build ignores `source` and treats a server's card as the user's on resume.
For other cards and tool results, the same text is already in `message_committed`. A code view's code,
`tool_call_requested.display.view.code`, is exempt when the call's input
holds it. That text is already in `message_committed`, so a resumed code
block shows and copies the exact code; otherwise it is scrubbed like the
rest of that record, `input` included. What a
resumed session reads back is exempt too, since a project whose path looks
like the endpoint's path must not have it rewritten: `session_started.cwd`,
`instructions`, `instructions_extra`, `instructions_sha256`,
`system_prompt_sha256` and `tools_sha256`, `tool_call_finished.state` (the working directory after a
`cd`), `process_started.file`, `tool_call_started.file` and
`tool_call_finished.output.file` (the output files `process` reads after a
resume; the rest of `output` is still scrubbed), and every field of `instructions_loaded` except each diagnostic's
`message`, which is still scrubbed. From version 30 the agent paths, names
and preset files a tree's resume reads back are exempt as well:
`session_started.preset`, `parent.agent` and `parent.config.instructions` (with
`parent.config.preset_values.instructions`), `agent_spawned.agent`, `cwd`,
`config.instructions` and `config.preset_values.instructions`,
`agent_ended.agent`, `agent_reported.agent`, `agent_renamed.agent` and `to`, and a `send_message` call's `input.to` in `tool_call_requested`, so a child named
`api-review` or a preset under `~/api-roles/` survives an endpoint whose path
is `/api`. From version 32 `session_resumed.instructions`,
`instructions_source` and `instructions_extra` are exempt too, beside its `cwd` and digests, so the
next resume restores the project block byte for byte; its whole prefix copy
(`system_prompt`, `tool_definitions`, `tool_context`) is still scrubbed.
From version 35 `session_started.sections`, `session_resumed.sections` and
`context_compacted.sections` are exempt as well, so rewind and resume restore
each prompt section's snapshot byte for byte and send no section again; a
lead entry's `snapshot` is in `turn_started`, which is never scrubbed.

A native item's replay identity is one of the hashes that are never scrubbed: an
unsalted SHA-256 over the account's whole URL, path included, a label, and its API key
or token, or a sign-in's profile path instead. It stays in the conversation records on
this machine and is never sent, shown or put in events. Whoever holds a copy of a log can
confirm a key they already have, or see that two logs used the same key, so a shared log
carries that fingerprint.

Wisp replaces registered secrets, bearer values, email addresses, key-shaped
or known-prefix tokens, and account IDs in provider error text with `[redacted]`
before applying character limits. This affects the screen too, as well as
`retrying.reason`, `turn_ended`'s message, `compaction_failed.message` and
`error_type`. The same key-shape check applies to messages and types, except
that exact known type names such as `ServiceUnavailableException` remain
readable in `error_type` unless they match a registered secret. The
`request_id` field uses only the registered-secret and known-prefix checks.
An account ID is a UUID, or a whole token with `org-`, `user-` or `proj_`
followed by 20 or more letters and digits.
Where Wisp keeps only the prose before an embedded payload, it masks registered
secrets before finding that boundary and scrubs the remaining prose before
capping it. On a Claude subscription the message and `error_type` are kept
only for a request-shaped refusal: a type that names an invalid request
(`invalid_request_error`, `not_found_error`, `request_too_large` and the like)
on a status that reads as a request error, so never a sign-in, billing,
rate-limit or server one. Every other failure there keeps the fixed sentence
and records no `error_type` (a thread refusal keeps its code), and the
`request_id` stays absent.

The `AccountsGone` event (accounts signed out in another window,
`docs/endpoints.md`) is never logged: each move it reports is that agent's
`settings_changed`, and a send it refuses is a `rejected` with `account_gone`.

`LeaveNeeds` (`docs/endpoints.md`) is never logged: the refusal after it is
a `rejected`, and the change that answers it a `settings_changed`.

Input the user types into a process (`ProcessInput`) is never logged: it
writes no record, and a refused one writes `rejected` with a `reason` naming
only the process number.

A secret the user enters for a tool (`AnswerSecret`) is never logged and goes to
no event: it writes no record, `SecretRequested` and `SecretRequestClosed` write
none either, and a refused answer writes `rejected` with the fixed words `no
secret request <call_id> is open` or `secret for <call_id> refused`, naming no
problem, since a problem is a fact about what was entered.

A client's `Terminal` size writes no record of its own; only the line it
produces reaches the log, as the `workspace/terminal` entry of a turn's `lead`.

Nothing of the sidebar is logged: a client's `SlotsShown`, the sections it
names, a section's content and its failures (`SlotContent`, `SlotFailed`)
write no record, and a section's mark writes none either. A session with a
busy sidebar logs exactly what the same session logs with none, record for
record.

Raw output files and output streamed live are not redacted. Before a tool
result joins the request and `message_committed`, eligible held credentials
are masked as described above; secrets outside that mask still reach both.

Tests run sessions with a canary key, URL password, path, query and env-file
value, including connection failures and a server that hangs up, and check
that none of them appear in the file.

## Stability

- Readers must ignore record types and fields they don't know. Adding either is
  not a breaking change and keeps `schema_version` the same, unless it changes
  the rebuilt history (below).
- Renaming or removing a field, or changing its meaning, bumps
  `schema_version`. A field that changes the rebuilt history counts as a
  change of meaning even when it is optional, since a reader that ignores it
  would rebuild different bytes and miss the prompt cache on resume. Older
  versions stay readable.
- Records with a `schema_version` newer than the reader's are skipped too.
- Bytes after the last newline are a write cut short by a crash or a full
  disk; readers skip them, even when the cut splits a UTF-8 character. A
  complete line that isn't UTF-8 or JSON, a record of a known type that
  doesn't parse, or a gap in `seq` is corruption and fails the read.
- History is the resume contract, rebuilt in order:
  - A turn's user blocks are a text block holding each entry of
    `turn_started.lead`, in the order logged (in a log up to version 28,
    the `context`, `stamp`, `context_note` and `processes` fields, read in
    that order as the entries `core/environment`, `time/sent`,
    `context/fill` and `shell/processes`), an image block for
    each entry of `turn_started.images`, in order, and a text block holding
    `turn_started.text`. A `turn_started` with `inputs` has, after the
    lead, each entry's image blocks, a text block
    holding its `text` and, when it has a `note`, a text block holding
    that, entry by entry in order. A `turn_started` with a
    replay entry (such as `shell/processes`) and no `text`, `images` or `inputs` (a turn Wisp started
    because processes ended, or a retry of one) has only the blocks before
    the input: no empty text block. An image-only message keeps its empty
    text block, notice or not. An image block is
    `{"type":"image","source":{"type":"base64","media_type":...,"data":...}}`,
    whose data is the saved file's bytes in standard, padded base64.
  - They become a new user message, unless history so far ends in a user
    message (an open tail of tool results), in which case they are appended
    to it, after its `tool_result` blocks. The first turn after a
    `context_compacted` (version 19) whose `from` equals the history's
    length always starts a new user message, so it lands after the head.
  - If `turn_started.effort_marker` is present, an effort marker follows
    them: a `system` message holding one `effort` block with the marker's
    `level` and `rendered`. A `per_message_effort` record with state
    `fallen_back` written during the turn sets `rendered` to false, since the
    endpoint refused the marker and the turn was re-sent without it.
  - Every `message_committed.message` of the turn follows, in order.
  - After a Convert in a turn (a `context_compacted` carrying the turn's
    number, version 19), the marker the new view needs is logged late, as
    its own `message_committed` holding the `system` effort message, when
    the turn's message joins the history (with its first kept reply) and
    before that step's `input_committed`. It is rebuilt like any committed
    message: the turn's pending user blocks become their message first and
    the marker follows it, so the steers in the `input_committed` become a
    new user message after the marker. An `attachments_withdrawn` written
    before the late marker whose ordinals pass the turn's own images names
    those steers' images: as for any withdrawal, those ordinals, less the
    turn's own images, belong to the next `input_committed`. An ordinal
    among the turn's own images that none of them took (one listed twice)
    fails the rebuild. A turn that
    ends before then logs no late marker, and the next turn's
    `turn_started.effort_marker` carries it instead.
  - After a Convert once the turn's message was kept (its first reply
    committed), the step is sent again with the continue note in its place:
    a user message holding one text block, `Wisp compacted the conversation
    in the middle of this turn; continue the task from where you left off.`
    When that step's reply commits, the note is logged as its own
    `message_committed`, then the marker the new view needs as its own
    `message_committed`, then the step's `input_committed`, then the reply.
    The note starts a new user message after the head. Without a marker the
    steers in the `input_committed` are appended to it; with one they become
    a new user message after the marker. A turn that ends before then logs
    no note.
  - A Retry whose history ends in a paused reply that was closed (a switch
    to another endpoint closes it, so does resume after Wisp stopped during
    it, and a later Retry finds it still closed) opens with the closed-pause
    note instead, a user message holding one text block, `Wisp closed the
    paused reply above because it couldn't be continued; continue the task
    from where you left off.`, logged the same way as the continue note: its own `message_committed` when the step's reply
    commits, then the marker, the `input_committed` and the reply. A request
    carries one note at most. When the Retry lands at the head, or a Convert
    runs on its first step, the continue note takes the closed-pause note's place
    and only the continue note is logged.
  - An `input_committed` adds, for each entry in order, its image blocks,
    a text block holding its `text` (no stamp) and its `note`'s text block
    if it has one, placed by the first rule
    that applies: if the turn's own user blocks are still pending (no
    `message_committed` since its `turn_started`, because the reply before
    the steer had nothing to keep or a step-1 Gemini `MALFORMED_FUNCTION_CALL`
    retry is committing `wisp-call-note-<turn>-1`), they are appended to those blocks when
    the turn has no effort marker, and become a new user message right after
    the marker when it has one, so the request already sent stays a prefix;
    if history ends in a user message (tool results),
    they are appended to it after its `tool_result` blocks; otherwise they
    become a new user message after the assistant's reply.
  - `input_accepted`, `input_held`, `input_unheld` and `input_withdrawn` add
    nothing to history, nor do `wake_held`, `agent_refused`, `agent_spawned`,
    `agent_ended` and `agent_reported` (version 28).
  - An `attachments_withdrawn` (version 15) replaces blocks of the newest
    user message with their notes. Its ordinals count first the media
    blocks (images and, from version 22, files, at the top level or inside a
    `tool_result`) of an open
    tail: the last `message_committed`, when it is a user message and no
    `step_ended` with `accepted` true follows it. The remaining ordinals,
    less that count, belong to the pasted images still to be placed: the
    turn's own user blocks when they are still pending, otherwise the next
    `input_committed`. They count every pasted image in request order,
    including those an accepted request already carried (withdrawn only
    once a compaction's head has made them unsent again) and those an
    earlier withdrawal already replaced, so they map straight onto the
    turn's images and steers whenever the steers' `input_committed` is
    written. A log written by an earlier Wisp, which numbered only the
    images still in the message, fails to rebuild when a turn withdrew
    twice. Each listed block must exist and hold the listed
    sha256 (for a pending image, the image record's hash; for a block in
    the tail, the hash of its decoded bytes), or the rebuild fails naming
    the turn; a withdrawn pasted image's file is never read. A withdrawal
    whose turn adds nothing to history is dropped with that turn.
  - Beside the messages, the rebuild returns four things a request needs
    to be rendered exactly as it was live (version 10; all empty in older
    logs): `paused`, the history positions of assistant messages whose
    `message_committed.stop_reason` is `pause_turn`; `closed`, every
    `tail_closed` record's `calls` and `through`; `endpoints`, one per
    history message, the `turn_started.endpoint_name` of the turn that
    added it (absent where the log has none, meaning the session's only
    endpoint); and `cuts`, every `thinking_cut` record's `from` and
    `through`, in log order. `rendering_form`, `search_history_rendered`,
    `search_history_unreadable`, `system_marker_dropped` and
    `attachments_as_notes` change what a
    resumed request sends but not
    the messages; resume reads them in log order. It also returns
    `search_refused`, the endpoints named by `native_web_search` records,
    on which a resumed session sends no web search tool. From version 15 it also
    returns `provisional`: the index of the last message when it is an open
    tail written by a version 15 or later writer, the user message no
    accepted request has carried; resume treats its media blocks as
    provisional, so an attachment the API refuses after a resume is
    withdrawn like any other. A tail from an older writer is never
    provisional.
  - A paused assistant message is followed by its continuation (another
    assistant message) or, once its tail is closed, by the next user
    message. A closed `server_tool_use` is sent as the text
    `[web search for "q": the turn ended before its result arrived]` in its
    place, and each cut leaves out every `thinking` and `redacted_thinking`
    block from its `from` position through message `through`.
  - A `turn_started` with `retry_of` (version 20) and no `text`, `images`,
    `inputs` or replay entry continues the failed turn's history: normally it
    adds no user blocks or effort marker. A cross-account continuation whose
    last rendered effort marker the landing model doesn't take appends a
    supported marker after the existing history, recorded as its own
    `message_committed` once the request is accepted. The continuation's
    `turn_started.effort_marker` names the same marker but does not place it;
    re-sent steers follow the marker as a new user message from their
    `input_committed`. Earlier markers are untouched. Otherwise its first
    `message_committed` follows history as it
    stands, and the steers it re-sent are placed by their `input_committed`
    like any steer. A `retry_of` turn that carries
    inputs rebuilds like any turn, images included, and one that carries
    only replay entries rebuilds like the turn Wisp started, with no empty text
    block.
  - Turns without a `message_committed` were never added to history and are
    left out, their user blocks, images and marker included. Their image
    files stay in the images folder as records, and the rebuild doesn't read
    them.
  - A kept turn's image whose file is missing, fails its hash, or doesn't
    match the record's `bytes` fails the whole rebuild with an error naming
    the turn and the hash. An image is never dropped silently. The file is
    looked up by its hash and the record's `media_type` (whose subtype is
    its extension), and the block takes its type from the record, so the
    store's answer has no type of its own to check.
  - From version 22, each `media` reference in a `message_committed` is
    turned back into a `media` block holding the file's bytes in standard,
    padded base64, read from the attachments folder and checked the same
    way: a missing or corrupt file, or one whose size doesn't match the
    reference, fails the rebuild naming the turn and the hash.

  - A `context_compacted` adds nothing to history and closes an open tail,
    since nothing before `from` is sent or withdrawn from again; the rebuild
    returns the last one's `epoch`, `from` and `head` beside the messages
    (version 19;
    absent in older logs and in a session never compacted). The view a
    request sends is that head followed by `history[from..]`, with each user
    message appended to the user message before it (the head included), so
    `wisp_protocol::log::compacted_view` and `Rebuilt::view` rebuild it byte
    for byte. Every earlier `message_committed` stays in the file, so the
    full conversation is always on disk.

  Done in order, this reproduces the history byte for byte.
  `wisp_protocol::log::rebuild_history` implements it, taking a function
  that returns each image's base64 (it can't hash, so it fails on a
  withdrawal from a tool-result tail); `rebuild_history_with` and
  `rebuild_log_with` take a `BlobSource` that also hashes. Only
  `rebuild_log_with` reads each record's version, so only it applies the
  provisional rule: the events-only functions don't know which writer wrote
  a tail and always return `provisional` as none.
  `wisp_core::session::rebuild` and `rebuild_checked` supply one that reads and
  checks the session store; resume uses `rebuild_checked`.
- Resume (`wisp --resume`) continues on the
  endpoint of the last logged turn (or of the last `session_resumed`, when no
  turn ran after it). If that endpoint is no longer configured or can't be
  used, resume continues on the endpoint this run starts on with the
  session's model and says so, naming both; if
  it now points to a different origin than the log recorded, resume
  continues there and the notice names both origins. Either way the cache is
  cold and binding controls go on. Pair states are restored per (endpoint
  name, model) only when the origin matches. A logged directory that no
  longer exists runs the tools and the instruction loader in the current
  one, with a note. The session's cost is the last `turn_ended`'s session
  total plus what each manual `/compact` after it spent: its record's
  `cost_usd`, which holds its retries, or, for one a crash cut short, its
  `retrying` records; any `lower_bound` among them makes the total a lower
  bound. A compaction
  in or before a turn (its `retrying` records carry the turn and no `step`,
  or `step` 0 in older logs) counts the same way, once: its record's cost
  holds its retries. When Wisp stopped in that
  turn, the closed turn's own cost includes the compaction's, as it did live,
  but not the retries of a compaction the crash cut short. A turn Wisp stopped in, messages left waiting and a web search left
  open are closed as described under tool calls above.
- Resume numbers new processes after the highest id in any `process_started`
  or `tool_call_finished.process`. For each `process_started` without a
  `process_ended`, it checks for leftovers (`docs/plans/interactive-exec.md`
  §2.8): when the record's `boot_id` is this boot's and the process's session
  (or group, without a session) is still Wisp's (its leader alive with the
  logged start time, or no process with the leader's pid at all), it stops
  what is left as the session's end does (TERM, then KILL after the grace)
  and writes `process_ended { lost { stopped_at_resume } }`,
  otherwise `lost { gone }`, with the output file's size and line count.
  These follow `session_resumed` and the records that close a turn Wisp
  stopped in (under tool calls above), so the old run's turn ends before
  this run reports the processes it left, as a live session's end writes
  its `stopped` ends after the last `turn_ended`. A repaired call's result
  that names a process number stays as the old run logged it. The first resumed turn's `<processes>` notice
  then names every process stopped by the session's end or lost whose number
  is in no `process_heard`: `Process 1 was still running when Wisp exited, so
  it has stopped; its output file remains if it still exists.` (or `Processes
  1, 2 and 3 were still running when Wisp exited, so they have stopped; their
  output files remain where the files still exist.`), ahead of any live ends.
  Its commit writes their `process_heard`, so a later resume names none of
  them again; a turn that fails first repeats it. A Retry sends its failed
  turn's notice, never this one, so after a Retry the notice waits for the
  next ordinary turn. Processes that ended before
  Wisp exited are not reported again. `process` still knows every earlier
  number as an ended entry: one from `process_started` takes its
  `process_ended` (or the lost end above), and one logged only on
  `tool_call_finished.process` takes its end from that call's `outcome`:
  `completed` with an exit gives `exited`, `timed_out` gives
  `stopped { time_limit }`, `failed` gives `failed`, and any other outcome
  `lost { gone }`. Wisp numbers a call that finished in the foreground only
  when it exited or hit its time limit, so its own logs never pair
  `cancelled` with a number; a log that does reads as `stopped { user }`.
- **A `keep_running` process** (`process_started.keeps_running`) is left out
  of the leftover stop. When its record's `boot_id` is this boot's and its
  leader is alive with the logged start ticks, it is *kept-live*: resume
  writes nothing for it, so the log keeps a `process_started` with no end and
  the next resume checks again; `ProcessRestored` carries no `end`, with
  `keeps_running`, the local `pid` and the `log`. The notice names it in its
  own sentence, after the stopped ones: `Process 3 keeps running from
  before the restart (started with keep_running); its output goes to
  ~/…/0003-bash.log.` (or `Processes 3 and 5 keep running from before the
  restart (started with keep_running); their output goes to their logs,
  listed by the process tool.`), the path shortened with home as `~`. A
  record whose leader is dead, or from another boot, takes the leftover path
  above, but the notice gives it the kept-gone sentence below, never "so it
  has stopped". The resume that first finds it gone tells the model even
  when an earlier notice already named it as kept (a `process_heard` for
  it); later resumes see the logged end and stay quiet once it was heard. One that can't be checked (no `boot_id` here or in the record, or no
  `start_ticks`, as on macOS) writes nothing, gets the `unchecked` sentence,
  is restored with `unchecked: true`, and `process` won't signal it, since
  the pid may now lead an unrelated group. A kept-live process is checked
  again on a `process` list, when `/ps` opens and at shutdown; once its
  leader and session are gone it gets `process_ended { lost { gone } }` and,
  at the next turn, `Process 3 (python3 app.py), kept running from before
  the restart, is no longer running.`, without a wake. `process` `signal`
  stops it (TERM, grace, KILL) and logs `process_ended { stopped { model } }`
  (`/ps` `x`: `user`); reads, searches and waits are refused with `Process 3
  was started before the restart, so Wisp can only stop it; read its log
  with tail or grep in bash: <log>`.
- Resume restores two values of each request setting. **Current** is the
  last `turn_started`'s model, effort, `cache_ttl`, `debug_cache` and
  `refusal_fallback`, and the `time`, `context.context_note` and
  `context.auto_compact` the last ordinary turn applied (the session's
  starting values, then each change entry before an ordinary
  `turn_started`; `context.auto_compact`'s starting value isn't logged, so
  a session that never changed it takes this run's own, which applies at
  once, a Retry included), so a Retry after resume sends
  what the old process would have sent. **Pending** is the last
  `settings_changed` after the last ordinary `turn_started` (one without
  `retry_of`) where its endpoint, model or effort differ from the current
  ones, and the last change entry for each of the six other settings after
  that turn where it differs; the next ordinary turn applies them. A pending endpoint that isn't configured now is
  dropped, and the notice says so ("Your switch to endpoint other wasn't
  restored: it can't be used now.").
- Resume replays every kernel setting's last `layer: session` change entry,
  in log order, onto the session layer (an `unset` entry removes it).
  A later saved-layer change to a per-model kernel setting ends every
  earlier chat value of that setting, at every place. Older saved changes
  logged with `layer: session` still replay until such a saved-layer change.
  For the replayed values:
  `time`, `context.context_note` and `context.auto_compact` apply from the
  next turn, `compact_at` (per model or for every model), per-model
  `context_window`, per-endpoint `pricing`,
  `retry.budget_usd`, `context.count_tokens` and `instructions.warn_tokens`
  apply at once. Client settings aren't restored. A setting given as a flag
  on the resume command wins over its restored value at every site (a
  global `compact_at` flag beats a restored per-model one) and applies from
  the next ordinary turn; a Retry still sends the current values.
- **Flags on resume.** Only the resume command's own flags and `--set`
  values count (a `--set` form behaves exactly as its flag); config,
  environment and saved defaults never override the session. `--model`,
  `--effort`, `--cache-ttl`, `--debug-cache`, `--no-fallback`, `--time` and
  `--context-note` become pending changes for the next ordinary turn, in
  place of any logged pending change for the same setting; `--endpoint`
  starts the run on that endpoint; `--instructions` replaces the project
  block (below). A flag equal to the value the next turn would use anyway (a
  logged pending value, else the session's) changes nothing: the requests
  are the bytes a plain resume sends and the notice doesn't name it. A flag
  that differs is named in the notice, so replacing a logged pending change
  is never silent. When the resume rebuilds the cache, one sentence names
  every cause, the flags that rebuild (a model or endpoint switch, effort on
  a pair without per-message markers, the project instructions) and a tools
  or prompt change, with one price for the next turn's model and cache TTL:
  "Your flags change the model to claude-sonnet-5-5 and the project
  instructions: the cache is rebuilt (~$0.41) and the model's earlier
  reasoning is dropped." When an image the session sent is no longer
  available too, that sentence, and the prompt or headers sentence in its
  place, adds ", and an image this session sent is no longer available"
  before the colon and says the earlier reasoning is dropped. Otherwise the
  flags are listed after the notice, each with what it does to the cache when
  the cache is still warm: "From your next message: effort low (cache kept),
  cache TTL 5m (may rebuild the cache)." (`--cache-ttl` and `--debug-cache`
  "may rebuild the cache"; `--no-fallback`, `--time` and `--context-note`
  keep it); after "The cache has likely expired" the list carries no class. A
  model is named only through the shared display rule, so a key-shaped
  `--model` is never echoed. A child agent never takes the root's flags: on a
  tree resume each child keeps its logged settings (`docs/agents.md`,
  "Resume").
  `--instructions-extra` (`instructions.extra`) follows the same rule as
  `--instructions`: without it the session keeps its newest logged extra
  instructions (`session_resumed.instructions_extra`, else
  `session_started.instructions_extra`), and a differing value rebuilds the
  block in the same single rewrite, named in the notice as "the extra
  instructions (autonomous)" or "(none)"; both flags in one resume still
  take one rewrite and one sentence.
- Resume restores the project block, never from the files, when it is the
  block the session last sent: the newest logged copy (the last
  `session_resumed.instructions`, else `session_started.instructions`; a
  `session_resumed` whose `instructions_sha256` changed without a copy
  clears it) when it matches the newest `instructions_sha256` (of
  `session_started` and every `session_resumed`), or no block when neither
  is logged. Its source is the newest logged `instructions_source` (the
  last `session_resumed`'s, else `session_started`'s). `InstructionsLoaded`
  is emitted once, with `turn: null`, from the log's `instructions_loaded`
  record, and no second record is written. A different `--instructions` (or
  `--set instructions.source`) on the resume command loads the project
  block from that source instead, with the session's extra instructions
  (this run's `--instructions-extra`, else the newest logged ones) ahead of
  the files as on a fresh load (so `--instructions none` on a session with
  extra instructions keeps them alone), and
  a block that differs from the newest logged one takes the "prompt or tools changed" path below: one rewrite,
  with binding controls on, never a mid-conversation message. A text that
  is missing or doesn't match its hash (a damaged log, or one an earlier
  resume replaced in a log before version 32) falls back to the current
  files, read with the newest logged source and extra instructions: a
  session started with
  `--instructions none` loads none, whatever this run's config says. When
  the block in use isn't the restored copy and differs from the newest
  logged one, or the newest copy couldn't be restored, `session_resumed`
  logs it in `instructions` and an `instructions_loaded` record follows, so
  the next resume restores it exactly.
- When Wisp's system prompt differs from the newest logged
  `system_prompt_sha256` (of `session_started` and every
  `session_resumed`), its tools differ from the last logged ones (the last
  `settings_changed` or `session_resumed` that carries `tools_sha256`, else
  `session_started`), computed for the pair they were logged for (a switch
  still waiting, else the current pair; not compared when that endpoint
  isn't configured now), or the fallback or an `--instructions` flag above
  changed the project block from the newest logged `instructions_sha256`,
  resume turns binding
  controls on with
  `prefix_mismatch_behavior: "drop_block"` and the notice says "Wisp's
  prompt or tools changed since this session: the cache is rebuilt (~$X)
  and the model's earlier reasoning is dropped; the conversation itself is
  kept." A `session_resumed` whose prompt, project block or tools differ
  from the ones logged before it keeps binding controls on for a later
  resume, even when no turn ran after it.
- `--resume` without an ID skips child agents' logs (those whose
  `session_started` has a `parent`), and `--resume` with a child's ID
  refuses, naming its root's ID: a child resumes with its tree
  (`docs/agents.md`, "Resume").
- `--session-id <ID>` starts a chat with that ID, or continues it if its log
  already exists. Use `--resume ID` to refuse when the chat is missing. Choose
  a random UUID, such as `uuidgen` prints, never one derived from a person's
  data: tool results can include the ID in output paths and agent lists.
  If `log = false` is set, pass `--set log=true`. If the session
  `is open in another Wisp`, wait for the other run to end and try again.
- In scripts (`--output jsonl`), `--resume` without an ID opens the newest
  session in this directory that has a turn, skipping child agents' logs, logs open in another Wisp and
  logs that can't be opened, whatever tools it ran with. It never refuses
  over tools: a root session always resumes with this run's tools (the
  config default or `--tools`), and a difference takes the "prompt or tools
  changed" path above, with one miss; the notice names the tools added and
  removed first ("This run's tools differ from the session's: added
  show_diff; removed ask_user."). The endpoint list the TUI shows takes
  each endpoint's `pricing` as resume restored it.
- **Picking a chat.** In the full-screen TUI, `wisp --resume` with no ID
  lists this directory's chats (`wisp_core::resume::list`, the same rule as
  above but without chats whose `session_started` is `headless`, newest first
  by the log's modified time; `wisp --resume ID` still opens those) and, with two or more that
  can be opened, opens on a picker titled `Resume` before any session starts;
  enter resumes the focused chat exactly as `wisp --resume ID`. A `kept open`
  chat always requires a deliberate pick, even when it is the only one. With
  none kept open, one chat that can be opened opens directly (with `The newest chat here is open in another
  window.` when a newer one is open elsewhere); none gives `wisp: nothing to
  resume in DIR; run wisp to start a new chat`, and every chat open elsewhere
  gives `wisp: every chat in DIR is open in another window; continue there or
  run wisp to start a new chat`. Scripts (`--output jsonl`) and `--resume ID`
  are unchanged. Each chat is one row: its name (`ChatRow.name`, the newest
  `session_named` with a name, stored masked and shown as stored), else its
  topic (the first typed message: the first
  `turn_started` without an `origin`, or `input_accepted` without `from`, its
  first line with spaces collapsed and key-shaped words masked by
  `topic_mask`), then the last account logged with its mark (none when this
  Wisp no longer has it), the last model and the chat's agents still live,
  then how long ago it was active (`12m ago`, `short_ago`). A hosted chat keeps
  these details and shows `kept open`; a held lock without a host shows only
  its name and `open elsewhere`. The columns are set over
  every chat listed: when the details would leave the names under 30 cells,
  every row drops the account's name, then the agents, then the model. The
  `Filter` row under the title shows what you type, which filters by name,
  topic, account and model, and by every message typed in the chat
  (`ChatRow.typed`, never shown); esc clears the filter, then quits. The list
  parses only the record types it reads (`LISTED`) and skips the rest unread,
  so damage in a skipped line is found when the chat opens, not in the list.
- **`/resume` inside the TUI** asks the kernel for the same list without this
  chat (`ClientCommand::ListChats { request }`, answered by
  `AgentEvent::ChatsListed { request, chats }`, one `ChatRow` per chat) and
  draws it in place of the composer. The list is built off the agent's loop,
  and a log unchanged since this run of Wisp last listed it isn't read
  again; its lock and host status are checked. Enter asks again and opens a
  picked chat that is free or hosted. A locked chat without a host reads
  `open elsewhere` and nothing changes. Switching leaves the current hosted
  chat running; a chat that lives in its window ends instead. The window then
  attaches to the picked host or starts it, carrying `RunEnd.resume` and
  only the flags a fresh `wisp --resume ID` would carry: the ID, `--config`,
  `--mcp-config`, `--env-file`, `--keychain`, `--verbose`, `--output` and the
  `--set` keys that aren't chat settings (`display.*`, per-account and keychain keys).
  Every other flag, `--no-log` included, is dropped, so the picked chat logs
  and sends what a fresh resume sends. With `log = false` from config, `--set`
  or the environment, a pick shows the `--resume` refusal and this chat stays.
  If another in-window process takes a stopped chat after the switch, the start picker
  opens with that row marked `open elsewhere`.
- **What the TUI shows.** The full-screen TUI rebuilds the transcript from
  the log by turning its records into the events the live session sent, so
  one renderer draws both (`wisp_core::resume::transcript`). Each turn shows
  its messages as they went out (the opening ones, then each steer at the
  step it joined, from `input_delivered`, or from `input_committed` for a
  log without it, with the live session's `queued`, `sent while working`,
  `retried` and `not kept` tags), its replies' text and thinking, its tool
  calls with their recorded output (each call's `display.purpose` taken from
  its tool as built now, or as logged for a tool that isn't), its web
  searches (only the attempt that committed, with citations after the text)
  and how it ended, with the notes the live turn showed where the log
  records them: each retry attempt, a model fallback, a cache miss, a
  compaction divider, a settings change (its model, effort and endpoint
  line, with "cache rebuild" but no estimate, since the log keeps no price
  for it, or "nothing cached yet" before the first turn) and the "Retrying
  your message" line on a retried turn, naming the endpoint, model and
  effort that turn's `turn_started` records (without the attempt count,
  which the log doesn't hold). A web search refusal replays as live showed
  it: its `native_web_search` record is replayed as the
  `FeatureStateChanged` live emitted (with its turn's logged `model`, else
  that of the last `session_started` or `settings_changed`), so the note
  names the refused endpoint and uses the gateway wording when that endpoint
  is a gateway; a refusal rebuild with no record names the endpoint it ran
  on. The refused endpoints also seed the settings tracker, so the welcome
  card and later switch notes show search off there, as they did live. Each
  `per_message_effort`, `binding_controls`, `native_compaction`,
  `inline_tools` and `deferred_tools` record likewise replays as the
  `FeatureStateChanged` live emitted, with the record's own `endpoint_name`
  and `model`; a record without `endpoint_name` (before version 6) replays
  nothing, since the name live used isn't logged. A `deferred_tools` hold,
  which live wrote with no event, is replayed by the core as refused, since
  the log can't tell it from a real refusal; the client then drops it unless
  the kernel restored the pair as refused (below). The
  `per_message_effort` states seed the settings tracker, so Shift-Tab and
  the /effort and /model rows keep a proven or fallen-back pair's cache
  wording after `--resume`. The client is sent only the states the kernel
  restored: a replayed pair state reaches it only when the resumed agent
  holds that same state for the pair, so a pair whose origin moved (the
  origin rule above) or, on a fresh start, a pair on another endpoint
  stays unknown, as it does in the kernel. The
  context meter starts from the last request's usage. Input ids are read per
  run: each `session_resumed` starts a new run whose ids may repeat earlier
  ones. The TUI numbers its next messages past the highest `tui-N`, and the
  jsonl client past the highest `jsonl-N`, among the ids the log's turns and
  accepted or committed inputs hold (an id only in a `rejected` record isn't
  counted). A large paste replays as its text, not as the paste chip,
  because the log doesn't record that it was pasted. Messages that never
  went out are not shown. A log from before inputs were logged shows a
  turn's `text` as its opening message. Only the last turn keeps a Retry
  choice, with the offer the kernel recomputed for this run's endpoints
  (`docs/plans/resilience.md` §7.6); an earlier failed turn shows none. A
  turn a crash left open is shown open, and the repair's events close it
  live, ending with the choice when it offers one. The welcome panel adds
  the resume line, "Resumed session 01a167d6-5e2a (18 turns, $5.15, last active 2
  h ago).", followed by the cache notice, and messages a stop or crash left
  waiting are put back in the composer until a window takes them, with their
  images and a note (and a count of images the log couldn't supply). The start notes (every start warning,
  among them `2 presets changed since agents started; see /agents`) follow
  the replayed history, before that note. The replay draws nothing itself:
  a draw renders only the cells on screen and the nearest one above them
  with rows. jsonl output prints the start warnings and then the resume
  line on stderr and replays nothing.
- Resume always recovers (`docs/plans/resilience.md` §7, 4.4b). It refuses
  only a session open in another Wisp or a line a newer Wisp wrote. Damage
  a crash can't explain (an unreadable or out-of-order line, a record that
  doesn't parse, no readable `session_started`, a withdrawn block whose
  hash changed, or a history the send check would refuse) makes it
  continue from the last turn that ended cleanly, or from a compaction head
  that covers the damage and keeps no turns (a background swap's head with
  `cut` before `from` needs the records under it, so damage there falls
  back to the last clean turn). It writes one `resume_fell_back` naming what it
  dropped, keeps turn and process numbers past the highest it saw, counts
  the dropped turns' cost as a lower bound, and returns the newest 3
  messages of the dropped turns to the composer (all of them go in a
  `{session}.dropped-{seq}.md` file beside the log). What Wisp learned
  about an endpoint survives a dropped range: the `per_message_effort`,
  `binding_controls`, `native_compaction`, `native_web_search` and
  `window_lowered` records in it still apply, so a window the provider
  stated in a dropped turn is still used. A fresh start applies them to its
  own run as well, so it agrees with a later resume of the same log: every
  lowered window and refused search, and the feature states of the
  endpoint it runs on (a record with no `endpoint_name` counts for that
  endpoint). A `binding_controls` record also turns binding on, as it does
  for a resume. A state for another endpoint is learned again, and logged,
  if the session switches to it. A garbled
  `session_started` starts a new conversation in the same file, and so does
  a log whose only readable records are carried ones (a crash between a
  fresh start's `resume_fell_back` and its `session_started`). Processes
  started in dropped turns go through the leftover check and get their
  `process_ended` with `lost`, but are never named to the model. A missing
  or changed image, or a missing file a tool attached, is not a fallback:
  `image_lost` puts a note in its place.
- The tools the newest thinking was built with are the tools logged before
  its turn's `turn_started`, or those of a `thinking_cut` with
  `tools_sha256` written after it or earlier in its turn. When the first
  resumed request's tools differ, it keeps the thinking where binding
  controls are usable, else takes the tools-change cut and writes its
  `thinking_cut`, as the live session would have.
- A break that kept broken thinking in the session's last turn has no
  `turn_started` after it, so resume reads the break itself: a
  `native_web_search` fallback, a `rendering_form`, a
  `search_history_rendered`, a `search_history_unreadable`, an
  `image_lost` after the head, a
  `session_resumed` whose system prompt, instructions or tools changed, and
  thinking committed under other tools than the newest thinking before it
  each mark the pair they happened on, and turn binding controls on only
  when no `turn_started` follows them. A
  `tail_closed` counts only when its pair has binding controls usable and
  the rebuilt history holds thinking after its first closed call; it turns
  them on only when it is in the last turn. A break followed directly by
  its own `thinking_cut` kept nothing and does not count. A `thinking_cut`
  from `{"message":0,"block":0}` clears the marks and returns binding
  controls to the last `turn_started`'s value. When a marked pair has
  binding controls usable, the resumed session knows it kept broken
  thinking, so a later move off binding controls cuts once, as the live
  session would.

### Versions

- `43` adds slices to logged media blocks and attachment references. Wisp reads v42 records as unsliced and appends v43 records without rewriting them. Pre-v43 readers skip v43 records for viewing and refuse to resume a log containing them, so they cannot send the whole file in place of a saved slice.
- `42` adds `tail` to a `turn_started.lead` entry and the `core/data` entry that carries it (`docs/gemini.md`, `turn_form`). The bump is a change of meaning: a version 41 reader would rebuild such a turn with the data part ahead of the inputs and send other bytes. A log up to version 41 has no tail entries and resumes as before.
- `41` adds the `after_refusal` reason of a `not_run` outcome in `tool_call_finished`. The variant is additive and changes no rebuilt history, but `tool_call_finished.outcome` is parsed strictly, so a version 40 reader must see it as newer rather than as corruption. Version 41 readers still load versions 1 to 40.
- `40` adds non-blocking posted questions (`docs/tools.md`, "`ask_user`"): `question_closed.how.type = withdrawn`, `input_accepted.from.kind = question`, `question_asked` outliving `tool_call_finished` and `turn_ended` (and re-logged when a rewind or a resume reopens it), and `question_closed` between turns. The bump is a change of meaning: a version 39 reader would misread a question open past its call and fail on `withdrawn`, so it must see the log as newer. Logs up to version 39 have none and resume as before (an open blocking question is still repaired `interrupted`).
- `39` adds `attachments_as_notes`, an endpoint and model that gets a note in place of each image and file (`docs/media.md`, "Attachments a model can't take"). The bump is a change of meaning: a version 38 reader would resume on that pair sending the bytes the model refused. A log up to version 38 has none and resumes as before.
- `38` adds `note` to `turn_started.inputs` and `input_committed.inputs`, the mark on a message the user sent while Wisp worked. The bump is a change of meaning: a version 37 reader would rebuild a marked turn without its mark and send other bytes. A log up to version 37 has no marks and resumes as before.
- `36` adds tool changes in a running session (`docs/tools.md`, "Switching tools in a running session"): `turn_started.tool_message`, whose blocks the rebuild puts in the turn's system message, the `core/tools` lead entry (`Tools turned off: …`), the `inline_tools` and `tools_changed` records, `step_ended.betas`, and the lenient read of a `settings_changed` target this build doesn't know. The bump is a change of meaning: a version 35 reader would rebuild a turn without its tool message and send other bytes, so it must see the log as newer. A log up to version 35 has none of these and resumes as before; its steps' betas are unknown, so resume compares none; the inline tools beta, which no earlier Wisp sent, keeps the cache when it first appears (`docs/pricing.md`). A rebuild's copy on `tools_changed`, with its `history_len`, is additive within version 36. Version 36 readers still load versions 1 to 35.
- `35` adds prompt sections (`docs/plugins.md`, "Prompt sections"):
  `turn_started.lead` entries' `snapshot` and `replay`,
  `session_started.sections`, `session_resumed.sections` and
  `context_compacted.sections`, and renames the core's lead entries
  `core/environment` and `core/processes` to `workspace/environment` and
  `shell/processes`. The bump is a change of meaning: a version 34 reader
  would read neither the snapshots nor the new names, so it would send the
  `<environment>` block again and drop a Retry's notice; it must see the log
  as newer. A log up to version 34 resumes as before: its old names are mapped
  once where an entry is read, a `core/processes` entry is marked `replay`,
  and an old head's `<environment>` block is that section's legacy text.
  Version 35 readers still load versions 1 to 34.
- `34` adds `busy_window` to `outcome.gave_up` (`docs/endpoints.md`, "Busy
  endpoints"). A reader that knows `gave_up` parses it strictly, so a version
  33 reader must see the log as newer. `retrying.busy` is additive. A log up
  to version 33 has no such value and resumes as before. Version 34 readers
  still load versions 1 to 33.
- `33` adds `search_history_unreadable` (`docs/endpoints.md`, "Unreadable
  search results"). The bump is a change of meaning: a version 32 reader
  would ignore the record and send the encrypted blocks the endpoint
  refused, so it must see the log as newer. A log up to version 32 has no
  such record and resumes as before. Version 33 readers still load
  versions 1 to 32.
- `32` adds `session_resumed.instructions_source`,
  `session_resumed.instructions_extra` and `session_resumed.instructions`: a
  resume whose `--instructions` or `--instructions-extra` changed the
  project block logs the new source, extra instructions and block, and
  resume reads the newest ones back (`docs/plans/cache-stability.md`, MR 4). The bump is a change of
  meaning: a version 31 reader would restore `session_started`'s block and
  source and send other bytes, so it must see the log as newer. A log up to
  version 31 has none of these fields and resumes as before. Version 32 readers
  still load versions 1 to 31.
- `31` adds `still_running` to `process_started.reason` and to
  `tool_call_finished.outcome.backgrounded.reason`: a foreground command still
  running after 2 minutes is handed back as a numbered process
  (`docs/tools.md`, "Timeouts, cancel and leftovers"). Both fields are parsed
  strictly, so a version 30 reader must see the log as newer rather than fail
  on the value. Version 31 readers still load versions 1 to 30.
- `30` makes reports free text and lets presets carry instructions
  (`docs/plans/subagents.md` §6, MR 12): `agent_reported.report` holds the
  report's text, a string, where 29 held a typed object;
  `parent.config` and `agent_spawned.config` drop `report` and gain
  `instructions`; `instructions_loaded` lists files with scope `preset`; and
  agent paths, names and preset files are exempt from the scrub (above). The
  bump is a change of meaning: a version 29 reader would read a text report
  as a typed one and lose its line, so it must see the log as newer. Reading
  a log up to version 29 keeps a typed report as it was and takes its
  `summary` as the report line; a logged `report` kind is ignored. Version 30
  readers still load versions 1 to 29.
- `29` adds the turn's lead (`docs/plans/plugins.md` §4, MR 7): `turn_started.lead` is written in place of
  `context`, `stamp`, `context_note` and `processes`, each block it held
  becoming an entry named by what wrote it (`ends` stays, beside the
  `core/processes` entry it describes). The bump is a change of meaning: a
  version 28 reader would ignore `lead` and rebuild a turn without its
  environment block, stamp, context line and notice. Reading a log up to
  version 28 maps the four fields, in that order, to `core/environment`,
  `time/sent`, `context/fill` and `core/processes`, so it rebuilds the same
  history. Also from 29, additive and outside the rebuild:
  `session_started.tool_definitions` and `tool_context`, and on a
  `session_resumed` that changed the prefix its whole copy
  (`system_prompt`, `tool_definitions`, `tool_context`); and, for the hook
  engine to come, the `hook_ran` and `hook_applied` records and the
  `input_withdrawn` cause `hook`, which is parsed strictly, so an older
  reader must see it as newer. Tool displays gain `verbs`, and their
  `action`, `output` and a call's `facts` read a type they don't know as
  `unknown`. Version 29 readers still load versions 1 to 28.
- `28` adds agent mail (`docs/plans/subagents.md` §5, §10): `turn_started.origin`
  `mail`, a turn with `origin` that may have `inputs`, `input_accepted.from`,
  and the `wake_held` and `agent_refused` records. The bump is a change of
  meaning: `turn_started.origin` is parsed strictly, so an older reader must
  see a mail turn as newer rather than as corruption. Mail rebuilds through
  the existing `turn_started.inputs` and `input_committed.inputs` texts,
  which hold the rendered blocks, so `from` is metadata only and the new
  record types are skipped by older readers. `wake_held` and `agent_refused`
  add nothing to history, and a `rewound` that masks them changes nothing.
  The agents service (`docs/agents.md`) then added, still within 28 and all
  additive: `session_started.parent` in a child's log, and the
  `agent_spawned`, `agent_ended` and `agent_reported` records, which older readers skip
  and the rebuild doesn't read; and the checklist's `todos_updated` record
  (`docs/tools.md`, "`write_todos`"), which the rebuild doesn't read either:
  resume and rewind take the checklist from the last unmasked one.
  Version 28 readers still load versions 1 to 27.
- `27` adds the `show_diff` tool (`docs/tools.md`): the `show` display
  action and `display.view` on `tool_call_requested`. The bump is a change
  of meaning: `display.action` is parsed strictly, so an older reader must
  see these records as newer rather than as corruption. `display.view`
  keeps a `type` it doesn't know as `unknown`, so later view types need no
  bump. Every record carries the writer's version and reopening refuses a
  log that holds newer records, so an older Wisp refuses any session
  written after this version, whether or not it holds a `show_diff` call.
  Version 27 readers still load versions 1 to 26.
- `26` adds resuming from the last turn that ended cleanly
  (`docs/plans/resilience.md` §7, 4.4b): the `resume_fell_back` and
  `image_lost` records. The bump is a change of meaning: a reader that
  ignored `resume_fell_back` would fail on the damaged lines or bring
  dropped turns back, and one that ignored `image_lost` would send the
  image instead of its note. A `rewound` that fails the rebuild is
  `malformed_history` damage at its own `seq`, so resume falls back to the
  last turn that ended cleanly before it. A fallback that keeps a `rewound`
  keeps its masking. One that would drop a `rewound` while keeping records
  after its `to` moves back to the last turn that ended cleanly at or before
  that `to`, so turns the user went back past never return. A masked
  `resume_fell_back` still drops its range and a masked `image_lost` still
  puts its note in a kept turn, and a masked `process_started` still counts
  toward `processes_seen`, so numbering never repeats. Version 26
  readers still load versions 1 to 25.
- `25` adds the `window_lowered` record: a window the provider stated now
  survives a resume. The bump is a change of meaning: an older reader would
  resume on the configured window, overflow again and send a different
  `Context:` line. Version 25 readers still load versions 1 to 24.
- `24` adds rewind (`docs/rewind.md`): the `rewound` record, the `rejected`
  kind `not_rewindable` and the `input_withdrawn` cause `rewound`. The bump
  is a change of meaning: the records a `rewound` masks would be rebuilt by
  an older reader as if they were still part of the conversation. The
  masking rules: a masked record adds nothing to the history, except that
  a masked `turn_started` still sets the model, endpoint and effort the
  next request is compared with (and, for `--resume`, the session's
  current settings, which a rewind keeps), a masked accepted `step_ended` still
  refreshes how long the kept points stay warm, a masked
  `attachments_withdrawn` still applies its items inside the kept part of
  an open tail, a masked `thinking_cut` from a tools change, or one from
  `{"message":0,"block":0}`, is kept, clamped to the kept history, a masked `native_web_search` fallback still
  marks the endpoint refused, and a masked `context_compacted` still
  advances the epoch. Facts about the session rather than the
  conversation stay when masked, for `--resume`: settings, learned endpoint
  and model states (`per_message_effort`, `binding_controls`,
  `native_compaction`, `rendering_form`, `search_history_rendered`,
  `search_history_unreadable`, `system_marker_dropped`, `attachments_as_notes`), `session_resumed`, `instructions_loaded`, the
  inputs' own records, and which tool calls ran (so saved outputs are never
  numbered twice). A masked reply's thinking doesn't count as thinking sent
  with the current tools.
  A `rewound` inside a later rewind's masked range is
  masked too, without its check; one whose `to` falls inside the turns an
  earlier rewind left fails the rebuild. A `rewound` that fails its check,
  keeps a record that isn't before it, or goes back inside the turns an
  earlier rewind left fails the rebuild with an error carrying the
  record's `seq` (`rewind_at`). Version 24 readers still load
  versions 1 to 23.
- `23` adds background compaction (`docs/context.md`): the
  `compaction_prepared` record, and `placement`, `cut` and `additions` on
  `context_compacted`. The bump is a change of meaning: a `leading` head is
  an assistant message followed by the kept turns from `cut`, with the
  additions placed in the view, which a version 22 reader would rebuild as
  a head user message followed by the messages from `from`. Version 23
  readers still load versions 1 to 22.
- `22` adds files that tools attach to their results: the `media` content
  block, logged in `message_committed` as a reference to the attachments
  folder rather than base64, and `tool_call_finished.attachments`. Attachments
  are saved under `attachments/`, with `images/` read as a fallback for older
  logs, and an `attachments_withdrawn` item may now name a file. The bump is
  a change of meaning: an older reader would take a `media` reference for a
  content block it doesn't know. Version 22 readers still load versions 1
  to 21.
- `20` adds Retry (`docs/plans/resilience.md` §6): `turn_started.retry_of`,
  `turn_ended.outcome.offer` (`{"retry", "switch", "focus", "suggest"}`:
  whether the failed turn may be retried where it failed, whether it may be
  retried on another endpoint, which of the two the client focuses first
  (`retry` or `switch`), and, after a thinking-binding failure, the endpoint
  the conversation last completed on; absent when nothing is offered), the
  `rejected` kind `not_retryable`, and `settings_changed` written by a Retry
  that switches endpoint. The bump is a change of meaning: a `turn_started`
  with `retry_of` and no inputs adds no user blocks, which a version 19
  reader would rebuild as an empty user message. Version 20 readers still
  load versions 1 to 19.
- `19` adds compaction (`docs/context.md`): the `context_compacted`,
  `compaction_failed` and `native_compaction` records, a `turn` that may be
  absent on `cache_miss`, `unknown_provider_event` and `retrying` (and a
  `step` on `retrying`), and the `turn_running`, `nothing_to_compact` and
  `compacting` rejected kinds. The bump is a change of meaning:
  `context_compacted` changes what a resumed request sends (the head and
  `from`), which a version 18 reader would silently replay as the whole
  history, and a record without a `turn` would fail its parse. Version 19
  readers still load versions 1 to 18.
- `18` adds endpoints saved mid-session: the `endpoint_saved`,
  `endpoint_not_saved` and `endpoints_changed` records and the `endpoint`
  kind of `rejected`. `rejected.kind` now keeps a value it doesn't know as its
  text. The records change no rebuilt history; the bump marks logs a version
  17 reader would otherwise read with an unknown `rejected.kind`, which it
  parses strictly. Version 18 readers still load versions 1 to 17.
- `17` adds background processes and terminals: the `process_started`,
  `process_ended`, `process_resized`, `process_attached`, `process_detached`,
  `process_heard`, `process_needs_input` and `process_needs_input_cleared`
  records, `turn_started.origin` and `processes`,
  `tool_call_started.movable`, `tool_call_finished.process`, the
  `backgrounded` outcome, the `display.action` types `input`, `wait`,
  `signal` (with a `kind`: `interrupt` or `kill`) and `attach` on
  `tool_call_requested`, the `process` fact (`{"type":"process","id":...}`)
  and the process kinds of `rejected`. The bump is a
  change of meaning: `tool_call_finished.outcome` is parsed strictly, so a
  version 16 reader must see `backgrounded` as newer rather than as
  corruption, and `turn_started.processes` adds a user block to the
  history once it is written. This version reserves the records and fields:
  no release writes them yet. The rebuild places `processes` after the
  context note (the rule above). Version 17 readers still load versions 1 to
  16.
- `16` adds the settings the kernel applies mid-session (`docs/settings.md`):
  `turn_started.cache_ttl`, `debug_cache` and `refusal_fallback`,
  `session_started.debug_cache` and `refusal_fallback`, the
  `cache_ttl_change` and `request_option_change` rebuild reasons with a
  catch-all for later ones, and `settings_changed.changes` and `withdrawn`.
  The bump is a change of meaning: the three `turn_started` fields are the
  request bytes a resumed session sends outside the history, and a reason a
  version 15 reader doesn't know would fail the record. Version 16 readers
  still load versions 1 to 15.
- `15` adds the one-step withdrawal of a refused attachment
  (`docs/media.md`): the `attachments_withdrawn` record, `step_ended.accepted`
  and the provisional rule. The bump is a change of meaning: a resumed
  session must rebuild withdrawn blocks as their notes, which a version 14
  reader would silently send as the refused attachments again. Version 15
  readers still load versions 1 to 14, where nothing is provisional.
- `14` adds `turn_started.context_note`, the `Context:` line sent after the
  stamp, and `session_started.context_note_enabled`. The system prompt now
  always ends with the `Sent:` and `Context:` sentences (`docs/context.md`).
  The bump is a change of meaning: a resumed session must rebuild each turn's
  line from `turn_started.context_note`, which a version 13 reader would
  silently drop. Version 14 readers still load versions 1 to 13.
- `13` adds project instructions (`docs/instructions.md`): the
  `instructions_loaded` record, and `session_started.instructions`,
  `instructions_sha256` and `instructions_source`. `session_started.cwd` is
  now written for every session with a Workspace, not only sessions with
  tools. The bump is a change of meaning: a resumed session must rebuild the
  second system block from `session_started.instructions`, which a version 12
  reader would silently drop. Version 13 readers still load versions 1 to 12.
- `12` adds the search rows: the `search_started`, `search_ended` and
  `citations` records, written from the first release that continues paused
  replies and closes stopped searches. A version 11 reader skips a version
  12 log entirely, as it does any newer version, since it would rebuild
  without the closures' `thinking_cut` cuts; the bump marks the logs whose
  `tail_closed` and `thinking_cut` records are actually written, and
  `search_ended.outcome.reason` is parsed strictly like `tail_closed.reason`.
  Version 12 readers still load versions 1 to 11.
- `11` adds stored keys: `keyring` and `credential_file` as values of
  `credential_source` (on `session_started` and `settings_changed`). The bump
  is a change of meaning: version 10 readers parse `credential_source`
  strictly, so they must see these logs as newer rather than as corrupt. From
  this version readers keep an unknown `credential_source` as its text (the
  `Other` catch-all), so a later source needs no bump. Version 11 readers
  still load versions 1 to 10.
- `10` adds native web search's records: `tail_closed`, `rendering_form`,
  `thinking_cut`, `search_history_rendered`, `system_marker_dropped`, and
  `message_committed.stop_reason`. The bump is a change of meaning: each of
  them changes the history or the request bytes a resumed session rebuilds
  (adjacent assistant messages after a pause, closed calls rendered as
  text, thinking left out, blocks rendered for another account, a dropped
  cache marker), so a reader that ignored them would send different bytes
  or a request the API refuses. `tail_closed.reason` is parsed strictly. The
  same bump covers the `native_search_refused` reason inside
  `settings_changed.cache`, which version 9 readers parse strictly. Additive
  in the same version: `retrying.resend`, `usage.web_search_requests` (the
  searches a request ran, absent when none) and `thinking_cut.tools_sha256`.
  Version 10 readers still load versions 1 to 9.
- `9` adds questions to the user: the `question_asked` and
  `question_closed` records, `tool_call_finished.result`, and the
  `after_question` reason of a `not_run` outcome. The records and the field
  are additive and change no rebuilt history, but `after_question` is a new
  value inside `tool_call_finished.outcome`, which readers parse strictly, so
  a version 8 reader must see it as newer rather than as corruption. The
  `no_question` and `bad_answer` kinds of `rejected` ride the same bump. Version
  9 readers still load versions 1 to 8.
- `8` adds messages sent while a turn runs (steering): the `input_accepted`,
  `input_held`, `input_unheld`, `input_committed` and `input_withdrawn`
  records, `turn_started.input_id` and `inputs`, and `rejected.id` and
  `kind`. The bump is a change of meaning: `input_committed` and
  `turn_started.inputs` add user blocks to the rebuilt history, so a reader
  that ignored them would rebuild different bytes. `turn_started.text` is
  now absent on a turn started by several messages. Version 8 readers still
  load versions 1 to 7.
- `7` adds precise failures: the `server`, `timeout`, `billing`,
  `interrupted` and `context_overflow` error kinds and `outcome.detail` on a
  failed `turn_ended`. The bump is a change of meaning: a version 6 reader
  would fail on a known record holding a new kind. Failures that were `other`
  before (an `api_error`, a 5xx, a malformed stream) now get their own kind.
  The rebuild rule is unchanged. Version 7 readers still load versions 1 to 6,
  whose failed turns have no `detail`.
- `6` adds switching endpoints mid-session: `turn_started.endpoint_name`,
  `message_committed.model`, the endpoint fields on every `settings_changed`,
  its `account_bound` list and `endpoint_switch` reason, and `endpoint_name`
  on `per_message_effort` and `binding_controls`. The bump is a change of
  meaning: those two records now describe an (endpoint, model) pair, so a
  reader that ignored the name would apply one gateway's fallback to every
  endpoint. History holds no endpoint data, so the rebuild rule is unchanged;
  rendering a resumed request uses each marker's `rendered` flag and the pair
  states read back per (endpoint, model). An absent `endpoint_name` means the
  session's only endpoint. Version 6 readers still load versions 1 to 5.
- `5` adds images: `turn_started.images` and the image blocks in the rule
  above, with the files in the images folder. Version 5 readers still load
  versions 1 to 4, which have no images. Version 4's `settings_changed`,
  `per_message_effort` and `binding_controls` records are now read back
  rather than skipped, so a rebuilt marker keeps its `rendered: false`.
- Within version 8, step retries added the `step_ended` record, the
  `retrying` record's `step`, `kind`, `detail`, `usage`, `partial_output`,
  `model`, `cost_usd` and `lower_bound`,
  and `outcome.attempts` and `outcome.gave_up` on a failed `turn_ended`. All
  are additive: no record or outcome is parsed with unknown fields denied, so a
  version 8 reader that predates them skips `step_ended` as an unknown type and
  ignores the new fields (`gave_up`'s values included, since it never parses
  that field). None holds a new value of a field it already parses, and none
  changes the rebuilt history, so the version stays 8. A reader that knows
  `gave_up` parses it strictly, so a new reason there will need a bump.
- The versions so far, in order: 3 (tools), 4 (model and effort changes),
  5 (images), 6 (endpoints), 7 (failures), 8 (steering), 9 (questions),
  10 (web search), 11 (stored keys). The number lives in one place,
  `wisp_protocol::log::LOG_SCHEMA_VERSION`.

- Within version 4, `session_started.endpoint_name` was added as an additive
  field when named endpoints arrived. It changes no meaning, since a session
  still uses one endpoint throughout; readers that ignore it are unaffected.
- `4` adds model and effort changes: `turn_started.model`, `effort` and
  `effort_marker`, the `settings_changed`, `per_message_effort` and
  `binding_controls` records, and the marker rule above. A marker with
  `rendered: true` is sent as `{"role":"system","content":[],"output_config":{"effort":...}}`
  under the per-message effort beta; one with `rendered: false` is kept in
  history but never sent, so a later return to a model that takes markers
  never renders it. Version 4 readers still load versions 1 to 3, which have
  no markers.
- `3` adds tools: `turn_started.context`, the tool fields of
  `session_started`, the `tool_call_*` records, several `message_committed`
  records per turn including user tool-result messages, the open-tail rule
  above, and the step counts in `turn_ended.outcome`. Version 3 readers still
  load versions 1 and 2, which have none of these.
- `2` adds `turn_started.stamp` and `session_started.message_time`. The stamp
  ends with a newline and is the user message's first block.
- `1` records have neither: their user message rebuilds as a single text block
  holding `text`. Version 2 readers still load them.

The format is written through the `SessionStore` trait in `wisp-core`, so the
backend can change (SQLite is planned) without changing these records.

Provider OAuth access and refresh tokens read or rotated during the process join the process-wide in-memory redaction list, including for sessions already open. So do the key and URL parts an unavailable account still holds, a keys-file key hidden by a saved account of the same name included, each time the process reads its accounts: at start, at every look of the accounts watch and after an account is saved or changed in the chat. The same list also holds ready accounts' keys and URL parts and key-variable values, with or without a session log, so child logs keep masking them after an account changes or disappears. A key typed in the chat to save or edit an account joins the set once its format check passes, whether or not the reload that follows works. MCP servers' configured stored key values also join before their connections start.
Named exported server-key values and exported values under names containing `KEY`, `TOKEN`, `SECRET` or `PASSWORD` join at start, and MCP sign-in access/refresh tokens and client secrets join on load or save, in both cases only when they meet the tool-result length floor. This uses the same field exemptions as configured redactions: committed conversation blocks and instruction snapshots remain byte-identical for replay.

### Subscription allowance privacy

Subscription allowance snapshots, lookup statuses and threshold warnings are
live display data, not session records. `PlanUsageChanged` and
`PlanUsageWarning`, including child-wrapped events, are excluded from JSONL
output. The agent's log mapping has no durable counterpart for them. Usage
commands never append conversation messages. The log schema remains unchanged.
Resume starts without a historical allowance cache; offline replay refuses
usage-display commands without fetching. A live client must register fresh
display interest after connecting.

The optional `PlanUsage.plan_tier` is part of that ephemeral snapshot only. Its
closed, provider-reported value travels through private local display events, never
session records, JSONL output, transcript export, model history or prompts. Root
and child-wrapped telemetry use the same exclusion. It is not part of
`ErrorDetail.limit_reached` or persisted authentication/configuration state. Adding
the display tier requires no log schema bump; resume and offline replay have no
historical tier to restore.

The OpenAI adapter owns every `codex.rate_limits` event, including malformed or
unsupported shapes, before generic unknown-event handling. It projects only
validated window data and safe typed errors. Account IDs, plan names, balances,
promo fields and credit headers are not retained as unknown events or logged.
Ordinary inference unknown events retain their existing behavior.

Version 37 adds optional `ErrorDetail.limit_reached`, a validated usage window
carrying only durable terminal refusal metadata, including the reset time and
source. Older records without it still read. The additive optional `ErrorDetail.model_limit`
field keeps at most one validated used-up model window confirmed on the sent account binding.
It is display-only and does not set the refusal's reason, scope or retry time. If a log
redaction matches a window's key, label or scope, its optional field is omitted from the
record; other retry fields remain, and the live hold is unchanged. Account-wide snapshots
and warning transitions remain ephemeral: they are not log records, model history, or prompts.

A by-call native family's runner is not loaded or held, so a logged prefix
copy's `tool_definitions` leave it out. Its `tools` and `tools_sha256` describe
the actual wire, with the runner added; resume adds it for the adoption check.
