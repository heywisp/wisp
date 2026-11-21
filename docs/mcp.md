# MCP

The kernel's `mcp` service connects to local and HTTP Model Context Protocol servers.
Connections belong to one session tree, shared by its main agent and children.
Each chat owns its connections, even when a daemon starts that chat.
The daemon has no MCP-specific transport or connection table.

The bundled MCP plugin exposes tools through search, use and `/mcp`. A server's prompts are found as skills and its resources as documents; use reads either.

## Configuration

A server is a named table in `config.toml`:

```toml
[mcp.servers.example]
command = "example-mcp-server"
args = ["--stdio"]
```

`command` is a nonempty executable name, path or HTTP(S) address. `args` is an optional array of
strings. Arguments are passed directly, not interpreted by a shell.
Unknown fields, invalid types and NUL characters are refused without echoing
command or argument values. The configured name identifies the server;
a server's self-reported name never decides its provider tool names.

Servers inherit the session's working folder and process environment with the
same stripped variables and defaults as shell jobs (`docs/tools.md`,
"Environment"); `keys` entries apply after those job defaults and before
`WISP_JOB=1`. Wisp does not export credentials loaded from its keys file or
another server's secret store entry. A key the user already exported is
inherited.
A table may name its keys with `keys = ["SENTRY_TOKEN"]`: names only, never values.
Wisp reads the named keys from its secret store and sets them on that server's command only.
It masks these stored values in that server's call text, structured results, resource and prompt text, form text and call errors, and in session-log side fields.
A named key inherited from Wisp's starting environment is masked in tool results when it is 20 characters or more.
Exported values whose names contain `KEY`, `TOKEN`, `SECRET` or `PASSWORD` (any case) are also held from start unless they look like paths (`docs/tools.md`), even for a server added later. Other inherited keys for a server added while Wisp runs are covered from the next start.
Configured keys alone do not withhold attachments.
Other tools' results mask those stored key values when they are 20 characters or more, from the chat's first turn, with the same held-key mask used for account credentials.
Shorter values and tokens not yet held by Wisp are outside that result mask.
Tool, prompt and resource listings are not masked.
Use `keys` for credentials; a stored value of at least three non-whitespace
characters is masked wherever it appears in that server's text output and in the
log's scrubbed fields. Shorter values are not masked (`docs/session-log.md`,
"What is never logged").
Do not put credentials in the command or its arguments.

## Connections and calls

Reading configuration and saved lists starts no process. A first connection
starts a local command in its own process group or reaches the HTTP address. The rmcp client tries stateless
`2026-07-28` discovery first and falls back to an older initialization handshake.
Initialization and listing together have their own 60-second deadline.
A call's separate deadline begins when the server is ready, not while it starts.
Cancellation interrupts startup waiting or sends an MCP cancellation for a call.

Lists include every tools page, as before, and every page of prompts and resources the server advertises. Metadata is sorted by its original name or URI and saved together.

A server whose prompts or resources can't be listed keeps its tools; those entries are missing until it lists them. Each optional family has a five-second limit inside startup's 60 seconds, so one that never answers doesn't prevent tools from starting. Incomplete lists aren't saved.

Definitions use the same canonical JSON representation as the plugin host.
Provider names are `mcp__<configured-server-name>__<tool-name>`, with characters
other than ASCII letters, digits, `_` and `-` replaced by `_`. Names that collide
or exceed the host's tool-name limit are refused, never routed ambiguously.
Text and structured results become result text; images, audio and embedded
binary resources become typed attachments, unless this call received a value in the secret box: then all attachments are withheld before media processing. Error results remain error results.
A text block that only repeats the structured content's JSON is left out, so the model reads that content once, masked like any structured content.
A call's arguments reach the server exactly as the model wrote them; the server checks them against its own schema.
An initial argument refusal before any form round triggers a new tools listing and returns the fresh parameter schema; a later refusal uses the fixed completion-failure sentence.
A stopped MCP call ends as cancelled; its call-level deadline ends as timed out, with or without a card.

## A server's questions

In the main chat, a tool's form opens the question card, with `Asked by linear, one of your servers`.
Pages follow field-name order. Choices end with `Not now`; text and number fields open on `Type your answer`.
Yes-or-no fields offer `Yes` and `No`; multiple-choice fields allow several ticks. A skipped field is omitted.
`Not now` on any page declines the whole form. Words typed on a choice decline too; only those words reach the agent.
Wisp keeps question-card answers in the local log, out of model requests, compaction's kept messages and unfinished-call repairs; a server's own output can quote a non-secret answer.
An unfinished call with a server's card resumes as an unknown outcome, since an earlier round may have acted.
Forms allow at most 10 fields, 100 choices per field, 1,000 message characters, 100 per name/title/choice label and 500 per description; larger forms are declined without opening a card.
A text or number field whose name or title contains `key`, `token`, `secret`, `password`, `passphrase` or `credential`, ignoring case and matching inside words, opens the secret box after the other questions.
The box names the asking server and its destination. Its answer goes there once and is held until the call ends to scrub later form text and result text if the server echoes it; it is not logged. Numeric fields are converted only for the server response; the scrub covers the typed text, its JSON form and its spellings with JSON's string escapes or Rust's `\u{…}` (`docs/session-log.md`). Cancelled typed-field drafts are not restored into the chat composer.
Wisp advertises form elicitation only when a client can answer. URL forms, sampling and roots are not supported; `prompts/get` and `resources/read` still refuse input requests.
A child is told which call its parent should make; without a client, a question fails with `the server asked for input Wisp can't show`.
The round trip and the older handshake's `elicitation/create` both support forms, with at most ten rounds. An older-handshake request needs exactly one call in flight on that server; otherwise it is cancelled.
The server has 120 seconds of its own time over all rounds; asking time is excluded. The MCP tool's overall limit includes startup and ten minutes for the card: `timed out · 13m 00s`.

## Saved lists and lifetime

Each server has a JSON file in Wisp's data folder under `mcp/`. Its filename is
a digest of the configured name, command and arguments, not the command itself.
The file holds sorted canonical tools, the server's prompts and resources sorted by name and URI, the time
observed and `ttlMs`. The cache is local and contains no connection credentials.
Writes are atomic under the same private-file lock used by usage and model lists.

Every new connection lists again. A changed definition list advances the service's
revision; timestamps and TTL changes alone do not. Callers can read saved tools
without a running process. Missing or broken files have no saved list.
Dropping the last session-tree service handle closes HTTP connections and kills
every local server's process group, including children that inherited its pipes. Failed or timed-out starts do too.
The service retains the child handle until group teardown, keeping its identifier
unreaped through the group signal; it never signals a recycled group number.

## Finding tools

The bundled MCP plugin adds `/mcp` and describes saved tools without starting
servers. With `search` on, the model finds them by their descriptions and calls
them through `use`. A server's tool has the provider name
`mcp__<server>__<tool>`; its row shows the original tool and your server name.
The tool's descriptions and results are untrusted text, never system instructions.

`tools` may name only `search`: servers' tools still start held and remain
searchable. A child with `search` has the same held tools and shared connections.
A preset with `tools_off = ["search"]` has neither. No switch for servers alone
is added by this change.

## Adding and removing

Type `/mcp add <name> <command…>` to start a local server once, count its tools,
save its list and add its named table to `config.toml`. Quote an argument that
contains spaces, as in a shell; this splits arguments but never runs a shell,
expands variables or expands `~`. Startup gets its own 60-second deadline and
the command allows 90 seconds so a first package download can finish.

Do not type a credential. A word with a known key prefix (`docs/endpoints.md`, "Secrets"),
a secret-named assignment or a value after a secret-named flag, an Authorization header or a Bearer token is
refused without repeating its value, before starting or saving anything. A
refused value is not saved to config or the session log; the echo masks opaque
values. Input recall and plugins listening at the `command` point receive the same
masked text as the echo, never the original secret value. Ask the agent to add the
server with its `keys` instead ("Server keys", below): Wisp asks you for each key
and sets it on that server alone. The terminal also refuses a key-shaped
plugin command before sending, echoing or remembering it. The add asks the
host's `exec` policy gate before starting a command, or `net` with an address;
installed plugins cannot add or remove servers.

`/mcp` lists each server, its tool count and `running`, `starts on use` or
`needs sign-in`, `needs a key` or `failed`. A failure includes its sentence below the row. While search is off,
the list and a successful add say where to turn it on.

`/mcp remove <name>` stops that server, removes its config table and saved list,
and removes its tools from the next turn's search. A tool already offered stays
declared; calling it fails with `<server> was removed`. A tool gone on reconnect
fails with `<server> no longer has <tool>`. A start failure says
`<server> couldn't start: <reason>`.

## Catalog changes

Wisp keeps a server's tools, prompts and resources in four places: the saved
list on disk, written by each complete listing and when an added or re-keyed
server is committed; the chat tree's copy, read from the saved list at start and
replaced by every listing; each agent's definitions, where held ones follow the
tree's copy before the agent's next tool call and loaded ones change only at the
next message, by the rule below; and the search index, gathered again with the
held definitions, whose MCP entries the MCP plugin gives. A server is listed by
the main chat's background hook at chat start, if the server is on; at each new
connection (its first call, skill or document read in this run, a sign-in, a new
key or a reconnect); and after an argument refusal or a list-change notice.
Search hits for its tools, skills and documents end their summary with
`(saved list; may be out of date)` until the first listing in this run and again
after a notice until the new list arrives, and a held tool's hit also while the
tree's copy of that tool differs from its definition or is missing. A child
follows only the servers present when it starts or resumes.

To send notices, declare `listChanged: true` in the `tools`, `prompts` or `resources` capability and return the current items from `tools/list`, `prompts/list` or `resources/list`.
On 2026-07-28, acknowledge Wisp's one `subscriptions/listen` stream per connection and send `notifications/tools/list_changed` (or the matching notice for prompts or resources) on it with its subscription id.
On 2025-11-25 and older, send the notice on the session: stdout for a local server or the GET stream for HTTP.

Each opening compares the canonical definitions; an unchanged catalog
appends nothing to the request. A changed loaded definition is
sent by value only on an established inline-tools connection; otherwise the
loaded version stays for this session. Earlier request bytes never change.
The session-start check appears in the expanded hook history, including on
resume; it adds no line to the normal transcript or provider input.
A skill's full search name is `mcp__<mapped-server>__prompt:<original-name>`. A document's is `mcp__<mapped-server>__resource:<URI>`. Original prompt names and resource URIs are kept unchanged after the prefix.

Opening a skill shows its description and argument list. A prompt with no required arguments also returns its text, with each message's role. When required arguments exist, use lists them and explains that the skill's text isn't shown. use can't pass a prompt's arguments yet.

A skill already in view returns one line instead of repeating its text. Documents are read again on every use. Non-text parts and oversized results are refused without a partial result. All server content remains a tool result, never system instructions. `/skills` continues to list only the skills plugin's folders.

## HTTP and sign-in

`/mcp add linear https://mcp.example.invalid/mcp` saves an address with no
credentials, user part or query. A server requiring an account is added as
`needs sign-in`; `/mcp` shows the command to sign in.

`/mcp sign-in linear` shows a sign-in link and the chat window tries to open it
in the user's browser. Repeating the command within ten minutes tries the same
pending link again. A browser on Wisp's machine returns to a loopback listener
and finishes automatically. Elsewhere,
copy the browser's final address and type `/mcp sign-in linear <address>`.
The shared account redirect reader checks its path, state and deadline.
A typed address's `code=` (including percent-encoded parameter names) is masked in the local echo, recall and command hooks;
the original travels only in the private command argument, not log records.
The link and its client ID travel only in an event-only link note, never in the
command's answer or a log. The window opens only literal `https://` links or
`http://` links to this computer, with no user part and only ASCII letters,
digits, `-._~:/?#[]@!&*+,;=` or percent-encoded bytes (`%` and two hex digits).
The check is written for wslview's default PowerShell engine; wslu 4's opt-in cmd engines read `&`, `,`, `;`, `=` and `%NAME%` differently.
A refused link is still shown and reads `no browser here`. As with provider
sign-in, the link is an argument of the browser opener process.

Sign-in requires HTTPS or loopback HTTP for the server and every resolved issuer,
authorization, token and registration endpoint before sending credentials.
rmcp may fetch public discovery metadata over HTTP; these requests carry no credentials.
rmcp discovers the sign-in service, registers Wisp with its fixed name and the
loopback redirect, exchanges the code and refreshes tokens. Its credential
adapter uses the existing account OAuth profile store, scoped by config and
server name, checking the stored address and issuer before each use. Changing the
address clears its old sign-in at the next connection. A sign-in an earlier Wisp saved under the
address is deleted, never reused, so that server asks for sign-in once more.
Access/refresh tokens and the returned client registration, including any client secret, use
the existing keychain-or-0600-file choice and Credentials lock. Refresh holds
that lock across load, exchange and save; removing a server deletes its profile
and any keychain item and cancels its pending loopback listener.
Access/refresh tokens and a client secret of 20 characters or more are masked in tool results from the chat's first turn, and new ones once received.
The chat starts once they are read; a locked keychain may ask to be unlocked first.
Tokens go only to their exact server address, over HTTPS or loopback HTTP;
redirects are disabled. Completing sign-in lists and saves tools and advances
the existing catalog revision. The chat shows `plugin mcp: NAME signed in · N tools`
(`1 tool` for one), or `NAME not signed in: REASON` after a decline or a failed
exchange. If the exchange succeeds but the server cannot start, its whole failure
sentence shows instead, `NAME couldn't start: …` (or `NAME needs sign-in`). A pasted
finish shows the same line. If `/mcp sign-in NAME <address>` succeeds in a helper
agent's chat, it answers nothing there; the line shows only in the main chat.
A timeout, removing the server or exiting shows nothing.

Wisp installs no tracing subscriber; adding tracing output must first filter
rmcp's auth logs, which can contain codes, client IDs and tokens.

## Agents manage servers

Tell the agent to add a server, remove one, or turn its tools off. It finds
`manage` through `search`: `action` is `add`, `remove`, `on` or `off`, `kind` is
`server`, and `name` is the configured server name. For an add, `from` is the
command line or address after the name in `/mcp add`; quoted arguments work the
same way. Neither `name` nor `from` may carry a key. Rows, notes and results never
repeat `from` or a saved server table.

An add starts and lists the server before saving its table and list, as `/mcp add`
does. The main agent can find and use its new tools in the same reply; changed
loaded definitions wait until the next message. A remove stops it at once and
removes its settings and list. Both write the user's settings for every chat.

An agent with a shell, loaded or held, may add or remove. A main chat without one
asks first: the approval card says the change affects every chat. An add's card
shows a new server's command line after checking for keys. Retrying a waiting
sign-in validates any supplied `from` but never shows or runs it; the card names
the sign-in instead. A remove's card names the server only.
A child without a shell is refused with the call its parent should make. A child
that adds a server keeps its fixed tools; agents started or resumed after it have the server.
Its additions and removals are shown once, in call order, in the main chat.

`off` and `on` take effect at the next message. An off server is off throughout
the tree: no agent can call it, search leaves out its tools, prompts and resources,
and `/mcp` shows `off`. This changes this chat only, never the saved server table.

When an agent's add needs sign-in, Wisp starts it and shows `plugin mcp: sign in
to <name> at <host>`, its link and `tried your browser` or `no browser here` as
one block. The chat window tries to open the page; under `no browser here` it
also shows how to finish with the browser's final address. The agent gets no
link and cannot find it in a log, file or tool result. The main agent that asked
gets the outcome as a notice and carries on after the usual two-second settle,
searching for the server's tools after success. If busy, it takes the notice when
its turn ends; if stopped, failed or held, the notice waits for your next message.
A helper agent's add, a run with no window and a sign-in started by the user do
not set a wait: without a waiting main agent, only the line shows. The wait lasts
until the notice, removal or exit; it does not survive a restart. An add that
finds the server already signed in answers `NAME added` and waits for nothing.
`add` with the same name and no `from` shows the pending link and tries again;
`/mcp sign-in <name>` does the same by hand. A run with no window tells the agent
to ask the user to type that command in a full-screen Wisp chat.
A child's sign-in is started and shown at the main chat's next message.
Sign-in links aren't in the session log, JSON-lines output or replay stores.

## Server keys

An agent's add names the server's environment variables in `keys`, never their values.
For each missing key, Wisp opens its secret box in the main chat after any approval card.
The box names the server and the add's command; when setting a key anew it names only the server.
A child asks its parent to make that call; a session with no client must use an inherited key,
which every command and server Wisp starts inherits too.
Wisp starts and lists before writing the table and keeping the key, using the sign-in's secret store.
Once listing succeeds, the writes run to their end even if the call stops or times out.
An unsuccessful keep leaves the add counted and the server running in this chat; the result says it will need the key again.
Ask the agent to set a key anew with `manage` (`action add`, `kind server`, its `name` and `keys`); `from` is ignored.
Wisp restarts with the new key and keeps it only after listing. A failed start leaves the old key stored and the row failed until the next start.
`needs a key` means neither the store nor Wisp's inherited environment holds a named key; `/mcp` says how to set it.
Stored keys are bound to the server's name, command and arguments. Editing the command makes them unavailable; remove clears them by name.
A typed `/mcp add` cannot name keys. An older Wisp refuses a settings file containing `keys`.
Servers at an address sign in instead and refuse `keys`.
