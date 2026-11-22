# Endpoints

An endpoint is one way to reach a model: a provider, a base URL and a
credential. You name your endpoints once in `config.toml`, choose one when
Wisp starts, and switch mid-session with `/account` in the full-screen TUI.

Where no keychain is available (see [Keychain first](#keychain-first)),
nothing changes without a `config.toml`: Wisp reads `ANTHROPIC_API_KEY` or
`ANTHROPIC_AUTH_TOKEN`, and `ANTHROPIC_BASE_URL`, exactly as before, and names
that endpoint `claude-api`, or `gateway` when the URL isn't Anthropic's. On a machine with a keychain, Wisp reads keys only from
the keychain and `credentials.json`, never from variables or an env file.

## Where the file lives

- `$XDG_CONFIG_HOME/wisp/config.toml` when `XDG_CONFIG_HOME` is an absolute
  path, otherwise `~/.config/wisp/config.toml`.
- `--config PATH` reads another file instead. A missing file given with
  `--config` is an error (except for `wisp account add` and `wisp login`, which create it);
  a missing file at the default path just means no named endpoints.
- `--config` and `--env-file` are global: they work before or after a
  command (`wisp account --config work.toml list`). Wisp never prints a path
  given with `--config`, nor an `--env-file` path that doesn't exist, since it
  could be anything you typed; messages say `the file given with --config` (or
  `credentials.json beside the file given with --config`, or `the file given
  with --env-file doesn't exist`) instead. An `--env-file` that exists may be
  named, shortened to `~/…` under your home.
- Any other path Wisp prints, such as the default `config.toml`,
  `credentials.json` or `.env`, or the session folder, shows as `~/…` when it
  is under your home folder, compared by whole path components.
- **Never the current directory.** A cloned repository could otherwise ship a
  `config.toml` (or a `.env`) that sends your key to its own server.
- A syntax or validation error stops Wisp at start, and nothing is loaded. An
  error names the file and the line, never a value from the file.

## Schema

```toml
default = "manifest"                 # optional: the endpoint to start on

[endpoints.direct]
auth = "api_key"                     # x-api-key header
key_env = "WISP_DIRECT_KEY"          # the variable that holds the key
                                     # no url: the provider's own API
mark = 2                             # optional: this account's colour, 1 to 6

[endpoints.manifest]
provider = "anthropic"               # optional; "anthropic" is the default
auth = "bearer"                      # Authorization: Bearer header
key_env = "MANIFEST_API_KEY"
url_env = "MANIFEST_BASE_URL"        # or url = "https://gw.example.com/anthropic"
pricing = "list"                     # optional: "list" (the default) or "none"
native_web_search = false            # optional: on (the default) sends the provider's web search tool here
background_compaction = false        # optional: no background compaction on this endpoint
max_in_flight = 8                    # optional: requests the agent tree may have open here at once (default by account kind), or "none"

[endpoints.env]                      # optional: settings for the keys-file account, never its connection
native_web_search = false
max_in_flight = 4

[instructions]                       # optional: project instructions
source = "auto"                      # "auto" (the default), "default" or "none"
warn_tokens = "8k"                   # optional: the size warning's threshold

[context]                            # optional: what the model is told about its context
context_note = true                  # the Context: line on each message (the default)
auto_compact = true                  # compact before a request that reaches the point (the default)
verbatim_user_tokens = 8000          # your recent words a compaction keeps (the default)
compact_effort = "high"              # the effort Wisp's own summary asks for (the default)
background_compaction = true         # prepare native compaction in the background (the default)
background_tokens = 40000            # how far below the point it starts (the default)
```

| Field | Rules |
|---|---|
| `default` | Optional. Names an endpoint in the file, or `env` for the [keys-file endpoint](#the-keys-file-endpoint) |
| Account name | 1 to 32 of a-z, 0-9, _ and -, starting with a letter or digit. `env` names the [keys-file endpoint](#the-keys-file-endpoint): its table takes settings only |
| `provider` | Optional, default `anthropic`. Accepts `anthropic`, `openai` or `gemini` |
| `auth` | Required and explicit. `anthropic` accepts `api_key`, `bearer` or `anthropic_subscription`; `openai` accepts `api_key` or `chatgpt_subscription`; `gemini` accepts `api_key`, `bearer` or `antigravity_subscription`. The provider/auth table below describes their transports. Never guessed from the URL or a token prefix |
| `key_env` | The name of the variable that holds the secret (letters, digits and `_`). Each endpoint has `key_env` or `key_store`, never both. ChatGPT and Antigravity browser credentials are created with `wisp login` and require a store |
| `key_store` | `"file"`: the key (and a URL, if it has one) is in `credentials.json`, beside this file (see [Stored keys](#stored-keys)). `"keyring"`: they are in the system keychain (see [Keychain](#keychain)). Browser OAuth stores an opaque profile handle here; `login` manages its protected token bundle |
| `url`, `url_env` | At most one for API keys and ordinary bearer endpoints. Neither means the provider's API (`https://api.anthropic.com`, `https://api.openai.com` or `https://generativelanguage.googleapis.com`). Must be an `http` or `https` URL. `url_env` names a variable, so a URL with a token in its path can stay in the env file. A `key_store` endpoint takes `url` or a URL in its record, never `url_env`. Claude and ChatGPT subscription modes use fixed official endpoints and refuse URL overrides; `antigravity_subscription` defaults to the shipped Cloud Code PA host in `[providers.gemini.antigravity]` (`https://daily-cloudcode-pa.googleapis.com`) and accepts an explicit `url` override on `https://*.googleapis.com` (or loopback) in `config.toml` |
| `store_id` | Top level. 16 lowercase hex characters naming this file's records in `credentials.json`; required when any endpoint has `key_store` |
| `pricing` | `list` gives estimates at known model list prices (a gateway may charge differently); `none` shows tokens only, and the log still records the estimate (`list_usd`, `docs/session-log.md`). Subscription modes show the API-equivalent estimate marked `~`, not a charge; `none` keeps tokens only |
| `background_compaction`, `background_tokens` | Optional. This endpoint's background compaction setting and band, below its model tables and above `[context]` ([docs/context.md](context.md#background-compaction)) |
| `max_in_flight` | Optional, a whole number from 1 to 64, or `"none"` for no limit; by default 12 on a Claude subscription and 8 on every other account. How many requests the whole agent tree may have open on this account at once; the rest wait their turn ([Busy endpoints](#busy-endpoints)). Set it with `wisp config set endpoints.NAME.max_in_flight 4`, `--set endpoints.NAME.max_in_flight=4` or `/config`. Applies from the next session |
| `client_version` | Optional. A subscription's public client release number, such as `"0.168.0"`. By default Wisp uses the latest release it found. Applies at the next connection ([Client versions](#client-versions)) |
| `mark` | Optional, a whole number from 1 to 6: the account's colour on every screen ([What you see](#what-you-see)). Only you write it; any other value is ignored, the colour is picked, and `wisp doctor` warns. `[endpoints.env]`'s colours the keys-file account. A config holding `mark` doesn't load on a build from before marks, which refuses a field it doesn't know |
| `native_web_search` | Optional, `true` (the default) or `false`. Send the provider's own web search tool through this endpoint, to every model whose resolved `web_search` list has a version, whatever the origin; `false` sends it to none, on the provider's own origin too. Set it with `wisp config set endpoints.NAME.native_web_search off` or `/config`. Applies from the next session ([Web search](#web-search)) |
| `[instructions]` | Optional table for project instructions ([docs/instructions.md](instructions.md)), with three optional keys. `source`: `auto` loads the project's `AGENTS.md`, or your default when it has none; `default` only your default; `none` no instruction file. `--instructions` wins over it. `warn_tokens`: the size above which instructions get the `large` warning, an integer or a string such as `"8k"` or `"1M"` (default 8000); `--instructions-warn-tokens` wins over it. Nothing is ever cut. `extra`: `none` (the default) or `autonomous`, built-in instructions that go before the instruction files ([Extra instructions](instructions.md#extra-instructions)); `--instructions-extra` wins over it. Any other key in the table is an error |
| `[context]` | Optional table with optional keys. `context_note` (`true`, the default, or `false`): whether each message carries the `Context:` line ([docs/context.md](context.md#telling-the-model)). `--context-note on\|off` wins over it. `auto_compact` (`true`, the default, or `false`): whether Wisp compacts before a request that would reach the compaction point ([docs/context.md](context.md#automatic-compaction)); `--no-auto-compact` wins over it. `verbatim_user_tokens` (an integer or a string such as `"8k"`, default 8000): how many tokens of your recent messages a compaction keeps word for word ([docs/context.md](context.md#compaction)). `compact_effort` (`low`, `medium`, `high`, the default, `xhigh` or `max`): the effort Wisp's own engine asks for when it writes a summary, sent only where per-message effort is proven and only when the effort in force is higher ([docs/context.md](context.md#compaction)). `background_compaction` (`true`, the default, or `false`) and `background_tokens` (a positive token count, default 40000): whether native compaction is prepared in the background, and how far below the point it starts ([docs/context.md](context.md#background-compaction)). Any other key is refused |
| `[models."ID"]`, `[endpoints.NAME.models."ID"]` | Optional per-model tables: `context_window` (your window limit), `compact_at` (tokens or a share such as `"85%"`), `native_compaction`, `verbatim_user_tokens` (beats the `[context]` value for that model; the shipped table never sets it), `background_compaction` and `background_tokens` (beat the endpoint's and the `[context]` values; the shipped table never sets them) and `web_search` (the list of web search tool versions the model supports; `web_search_20250305` (Anthropic), `web_search` (OpenAI) and `google_search` (Gemini) are known, `[]` or `"none"` means none) in both, `max_output` (the longest reply this model may write, in tokens; it can only lower what Wisp asks for) and `price_input`, `price_output` and `price_cache_read` (US dollars per million tokens; set all three or none, [docs/pricing.md](pricing.md#your-own-prices)) in both, and `aliases` (dated IDs) under `[models."ID"]` only. The endpoint's table beats yours, and `--compact-at` beats both for one run; a top-level `compact_at` is the point for every model with none of its own. A window whose compaction point would fall under 40,000 tokens is refused at start ([docs/context.md](context.md)) |

The same file holds Wisp's other settings (`model`, `effort`, `[display]`,
per-model tables and more); [docs/settings.md](settings.md) lists every one,
and `wisp config set NAME VALUE` changes them without editing the file.

Anything else is an error. A field named `key`, `api_key`, `token` or
`secret` is refused with a pointer to `key_env` and to `credentials.json`:
**secrets never go in `config.toml`.**

### Providers

Every endpoint has a `provider`. Wisp keeps its own conversation, tools,
history and agent loop across all three providers:

| Provider | Auth | Transport and setup |
|---|---|---|
| `anthropic` | `api_key` | Native Messages API, `x-api-key`; `wisp account add` |
| `anthropic` | `bearer` | Native Messages API, `Authorization: Bearer`, including existing gateways |
| `anthropic` | `anthropic_subscription` | Fixed Anthropic Messages endpoint, Manifest-compatible Claude Code / Agent SDK request profile; browser `wisp login` or an explicitly supplied setup token |
| `openai` | `api_key` | Native Responses API, `Authorization: Bearer`; `wisp account add --provider openai` |
| `openai` | `chatgpt_subscription` | Fixed ChatGPT Codex Responses endpoint, Manifest-compatible Codex profile; browser `wisp login` |
| `gemini` | `api_key` | Native Gemini API, `x-goog-api-key`; `wisp account add --provider gemini` |
| `gemini` | `bearer` | Native Gemini API, `Authorization: Bearer`, including gateways |
| `gemini` | `antigravity_subscription` | Cloud Code PA endpoint (`daily-cloudcode-pa.googleapis.com` by default, `https://*.googleapis.com` `url` override allowed), Antigravity profile; browser `wisp login antigravity` |

These subscription profiles deliberately send the reference client's identity.
They use no official CLI subprocess. Live login acceptance, account entitlement,
cache reuse and subscription billing remain unverified by the offline tests.
See [OpenAI](openai.md), [Anthropic subscriptions](anthropic-subscriptions.md)
and [Gemini and Antigravity](gemini.md)
for their contracts and limitations. `auth = "cli"` remains unsupported.

Official provider endpoints reject model IDs known to belong to another
provider in the shipped model table or by `claude-*` or `gemini-*` prefix. Custom gateways
(an API-key or bearer endpoint whose `url` or `url_env` points to a non-official
host) accept any model ID so multi-provider routers can serve `claude-*`,
`gpt-*` or `gemini-*` models over whichever wire protocol the gateway speaks.

Agents may run on any configured endpoint, whatever its provider: a preset's or
a spawn's `endpoint` picks it, so a Claude coder and a ChatGPT reviewer can share
one session. A child's history can move to another provider just as the main chat's does
([Agents](agents.md), "Model, effort and endpoint" and "Resume").

The keys-file endpoint remains Anthropic-only. `OPENAI_API_KEY` and
`GEMINI_API_KEY` (with `OPENAI_BASE_URL` and `GEMINI_BASE_URL`) do not
create an implicit endpoint: add a named OpenAI or Gemini endpoint, import them
with `wisp account import`, or sign in.

### Subscription login and status

```sh
wisp login                 # asks: Claude, ChatGPT, Antigravity or an API key
wisp login claude          # a Claude Pro or Max subscription
wisp login chatgpt         # a ChatGPT Plus or Pro subscription
wisp login antigravity     # an Antigravity Gemini subscription
wisp login key             # an API key: Claude, OpenAI, Gemini or a gateway
wisp login claude-work     # sign in again to an account
wisp status                # every account and what needs you
wisp status chatgpt
wisp account models chatgpt
wisp --account chatgpt --model gpt-6.1-sol --effort low
wisp logout chatgpt
```

`wisp login WHAT` takes `claude`, `chatgpt`, `antigravity`, `key` or an account's name; a new
name adds an account with that name. Without a word it asks, on a terminal, with
one list; without a terminal it says what to give and changes nothing. A list is
chosen from by marking a row with ↑ and ↓ (stopping at the ends) or its number,
then Enter; Esc, Ctrl-C or Ctrl-D cancels. Either way the list, and the question
above it, clears, and the next step draws where it was. Every list in the CLI
works this way (`wisp login`, `wisp logout`, `wisp account add`, `edit` and
`import`), and so does every yes-or-no question (`wisp setup`, `wisp account
add`, `remove` and `import`, `wisp logout`): its answers are rows, such as Yes
and No, and Enter alone takes the row it opens on, which keeps things as they
are where an answer deletes or signs out. A terminal too short for the list, or
`TERM=dumb`, gets the numbered question instead (`Choose 1-3:`, or `Choose [2]:`
for a question that opens on a row), which takes a row's number, and in
`wisp login`'s and `wisp logout`'s lists a row's word too, and keeps its lines.
`--name NAME` names the new account for scripts. The hidden
`--provider anthropic|openai|gemini`, `--store auto|keyring|file` (where the sign-in is
kept) and `--default` (new chats start there) flags still work: `--provider`
behaves like the `claude`, `chatgpt` or `antigravity` word, so with accounts of that kind it
asks "Sign in again to NAME" or "Add another", and without a terminal it
refuses with the `--name` line instead of replacing an account.

A new account without a name gets one step, the same in `wisp login` and
`/login`: `Name for this account: claude`, with the suggestion (`claude`,
`chatgpt`, `antigravity`, `claude-api`, `openai-api` or `gemini-api`) already in the field. Backspace edits
it, Ctrl-U clears it and Enter keeps what is there; capitals become lowercase and
surrounding spaces are dropped. A taken suggestion leaves the field empty after
`claude is taken.`; names are never numbered. A taken or invalid name says
`claude-work is taken.` or
`Names are 1 to 32 of a-z, 0-9, _ and -, starting with a letter or digit.` and asks
again; `env` always says
`That name is reserved for the keys-file account; choose another.`, and the keys-file
endpoint's name ([The keys-file endpoint](#the-keys-file-endpoint)) is taken while it
exists.
`--name NAME`, or a new name as the word, skips the step; signing in again never
shows it. Without a terminal the suggestion is used when free, and otherwise a
line names `--name NAME`. A name another window takes while the browser is
open: `wisp login` asks again with the sign-in kept, so the code is not pasted
twice, and `/login` ends with `claude is taken.` Naming an existing account of
that kind signs in to it again.

Wisp opens the provider's sign-in page by itself: `$BROWSER` (its first `:`
entry, `%s` replaced by the link, `www-browser` skipped), then on WSL `wslview`
or `rundll32.exe url.dll,FileProtocolHandler`, then `open` on macOS, then
`xdg-open` when a display is set. Nothing opens under `CI`, or over SSH without a
display. Only an `https` link on the provider's own sign-in page is opened.
MCP sign-in uses the same opener from the chat window, with its own link check
(`docs/mcp.md`); it does not copy the link.
Every provider sign-in prints the link whole, tries the browser and says `tried your
browser` or `no browser here`; in a terminal it also copies the link and says
where it went, and under `TERM=dumb` the terminal route says `couldn't copy the
link`. Enter with nothing pasted says to paste the code first.

Claude shows a code to paste. The paste is checked before anything is sent:
an empty line, the code pasted twice, a code without its `#state` part, spaces
inside it, a far too long paste or a code from an earlier sign-in each get one
line and the question
again, on the same sign-in. The echo shows, in its own colour, a short start,
stars and the last 4 characters with the length
(`Code: DEMO…************…0000  87 characters`); a known key prefix such as
`sk-ant-api03-` is the start, and a value with fewer than 16 characters after
its prefix shows only stars. A bracketed paste is one answer,
even with a line break inside, and a new paste replaces the line. A code Claude
refuses ends the sign-in, and enter opens a new page. A token request that can't
reach the provider is retried once on the same sign-in with the same code
(`Couldn't reach Claude. enter tries again.`). ChatGPT returns through Wisp's
loopback callback, so there is nothing to paste; Antigravity uses a local
loopback callback when a browser can open here, and on a headless or remote
machine asks you to paste the `http://127.0.0.1:51121/oauth-callback?...` URL
instead ([Gemini and Antigravity](gemini.md)). Ctrl-C prints `Sign-in
cancelled; nothing was saved.`, except while the account is being saved, when
the saved account's result is shown. In the TUI a loopback sign-in that fails and
won't continue is cancelled at once, which frees its callback port. Login
expires after ten minutes.

The result is one line and one next step: `Signed in · Claude subscription ·
claude`, then `Run wisp to start a chat.` after the first account; signing in
again ends `Signed in again · …`. `wisp login key` asks which kind of key it is
(Claude API, OpenAI API, Gemini API, or a gateway at a URL you give; `--provider` skips the
question), then `Paste the API key: ` with the same masked echo, suggests
`claude-api`, `openai-api` or `gemini-api`, and ends the same way. Naming an account whose key
is already stored, with a word or `--name`, opens its key edit; a setup-token
account points to `wisp account edit NAME --replace-key`. `wisp endpoint` is
another name for `wisp account`.

`wisp status [NAME]`, `wisp account list [NAME]` and `wisp setup` print the
same panel, `Accounts`, in the resume picker's frame: subscriptions, then API
accounts, each sorted by name. A subscription row names its provider and
shows its 5-hour and weekly limits with `/usage`'s bars (or, for Antigravity,
its highest-used model window under `Model used`), each percentage
rounded to a whole number with its time to reset when available; a gateway
shows its host. The account new chats
start on reads `starts new chats`. An account that needs you reads
`sign-in expired` or `key missing` in the error colour with its fix, such as
`wisp login claude-work`; a locked store reads `can't be read now` and an unset
variable `VAR not set`. The host (except a gateway's), where the key is kept
and the masked key aren't shown; `wisp doctor` keeps the host and the key's
place. When its rows don't fit the terminal with a 10-column name, the panel
drops the provider column of subscriptions, and the default and each fix go on
the line under the name; a name too long for its
column takes its own line, with its cells under it. Piped, with `TERM=dumb` or
with `NO_COLOR` it prints the same text with no colour.

To show the limits, it asks each signed-in subscription's usage page once, as
`/usage` does, all at once with one 3-second limit, refreshing a lapsed sign-in
as a chat start does. A lookup that fails, is refused or runs past the limit
reads `usage unavailable`; with `claude_usage = false` no Claude account is
asked and its cells stay blank. It shares the usage record the chats keep
(`/usage`): a reading under 10 minutes old is shown without asking, a failed
lookup's wait (30 minutes, then an hour) holds it too, and a reading too old to
show as current reads `usage unavailable`. A refresh already sent when the limit passes is allowed to finish
and save its new tokens before the command exits (at most 60 seconds); one not
yet sent is dropped. `wisp login status` does the same as `wisp status`.

ChatGPT login refuses an
alias belonging to another provider: choose a different `--name`. Its endpoint
configuration is checked again under the persistence lock; a changed endpoint
is not overwritten using the earlier browser-login snapshot. A missing explicit
`--config` file is created, just as with `endpoint add`. New logins leave pricing
at its default (`list`); reauthorization preserves an existing pricing choice.
Replacement credentials are written beside the old namespace before config is
switched, so a failed config write leaves the old endpoint usable. `--default`
is part of that same config switch, not a second write. When replacing an existing stored credential, an unrelated missing stored key does not block the copy: it remains missing. An existing keyring item that must be copied still has to be readable; a failed save reports the underlying storage or config error and any available recovery action. If the endpoint changed while the browser was open, run login again.

ChatGPT, Claude and Antigravity CLI login, local browser-profile status and logout use a shared kernel workflow. Its pending login owns the OAuth
listener and deadline; cancellation drops it. The shared completion transaction
persists the new profile reference before retiring the previous profile. A failed
config save rolls back only the new profile; an unconfirmed rollback is reported
as such. If the save succeeds but old-profile cleanup fails, the new login remains
saved and the CLI warns about cleanup, asking you to remove the unused Wisp OAuth
profile from protected storage. The completion result contains no URL,
state, code, token or account data. This extraction adds no chat auth commands or
session-log records; live catalog activation is a separate step. Claude uses the same
OAuth client, scope and manual code#state flow as before, through the shared completion transaction.

`wisp account models NAME` queries any account's model list without an
inference request ("Each account's model list"). It does not guarantee that every returned
model is entitled to all features. Model IDs are sent as written; pick a model
listed for the connection or one supported by your account.

`wisp logout [NAME|all]` signs out of any account: a subscription, a stored API
key or a `key_env` table. A subscription, named or picked from the list, goes at
once; a stored key or `all` asks first, with Keep marked so Enter alone keeps
the account. `--yes` answers for a script; with no terminal the command says
to add it. The keys-file account is never edited: it is refused with the line
naming the variable to remove. Signing out of the start account (`default`)
moves the start to the chat's account, else the first subscription of the same
provider, else the first subscription, else the first account left; the result
says where new chats now start. A preset that names the account never blocks the sign-out: a later spawn
of it says the account is gone. The last account can go too; then `wisp login`
signs in again. Removal writes config first, then removes stored credentials and
the OAuth profile. A config-write failure preserves the login; cleanup failure
after the config change leaves the account removed and prints what may still be
saved. It never restores an account pointing at deleted credentials.
Remote revocation is reported separately. Reusing the name with `wisp login`
creates a fresh endpoint without a stale old-profile cleanup warning.

A separately running Wisp session keeps its in-memory connection until it next
reloads; the CLI does not inspect or interrupt other processes. Claude removal is
local; Wisp does not invent a remote revocation request. Static subscription-token
endpoints use the same config-first deletion path, without an OAuth-profile operation.

`status [NAME]` reads the local browser profile for the sign-in's state and asks
the usage page for its limits, as above; it doesn't prove live access. `logout [NAME]` follows the provider behavior above and
reports whether remote revocation was confirmed. Without `NAME`, these commands
use the configured default. Browser-profile `status` does not inspect static setup
tokens; `logout` can remove their configured endpoints and locally stored tokens.

Tokens refresh before a request when needed, under the shared credential lock.
An authentication failure ends that request; Wisp never substitutes an API key
or another endpoint. Subscription usage has its own billing provenance. With list pricing it shows
an API-equivalent estimate on the subscription, not a charge; `pricing = "none"`
keeps tokens only. It does not establish which quota or balance the provider used.

For a static Anthropic token, run `claude setup-token` separately, then use the
hidden prompt:

```sh
wisp account add claude-token --provider anthropic --auth anthropic_subscription
```

Add `--key-stdin` to provide it through a pipe. Wisp reads no other app's saved
credentials. Keep browser profiles under `login`/`logout` management instead of
editing their handle records by hand. `endpoint edit NAME --store file|keyring`
moves the browser token bundle (access, refresh and expiry), not merely its handle.
A new destination profile and handle are staged before config is switched; only
a successful config switch retires the source profile, under the profile lock;
there is no keychain fallback on failure. If a source record cannot be removed after a successful rename or move, the result reports a leftover in its actual store. A file-store failure explicitly warns that the old key remains in `credentials.json`, just as a keychain failure warns about the remaining keychain item.

## Secrets

Known key prefixes, which masks show as a key's start and which typed commands
refuse, are listed with their vendors in `crates/wisp-protocol/data/key-prefixes.txt`.

Where a keychain is available, secrets come only from stored keys (see
[Keychain first](#keychain-first)). Elsewhere, secrets come from variables (the real environment, or the env file:
`~/.config/wisp/.env`, or `--env-file PATH`) or from `credentials.json` (see
[Stored keys](#stored-keys)). `config.toml` holds only a variable's name or the
store's. A command such as `op read` is planned with the secret store;
until then `op run --env-file … -- wisp` works, though it exports each secret it lists,
so every command and server Wisp starts inherits them.

**One source per endpoint.** If the real environment sets an endpoint's
`key_env`, all its variables (`key_env` and `url_env`) are read from the
environment; otherwise all are read from the env file. A key is never paired
with a URL from somewhere else. A variable that isn't set makes the endpoint
*unavailable*, with a reason naming the variable, never its value. Values are
never expanded (`${HOME}` stays literal), and the env file is never exported to
tools.

No key, token, URL path, query or password appears in any message, the welcome panel,
label, log or `Debug` output. The session log redacts every available
endpoint's secret and URL parts.
`login` deliberately shows the provider's authorization URL, with its public
OAuth parameters, state and PKCE challenge; it never shows the verifier or tokens.

## Stored keys

An endpoint with `key_store = "file"` reads its key from `credentials.json`,
which sits beside the config path as given: `~/.config/wisp/credentials.json`
for the default config, or next to the file given with `--config` (never
beside a linked `config.toml`'s target). `wisp account add` writes both
files (see [Commands](#commands)); they can also be written by hand:

```toml
store_id = "5f0c2a9e41d7b388"

[endpoints.manifest]
auth = "bearer"
key_store = "file"
```

```json
{
  "version": 1,
  "stores": {
    "5f0c2a9e41d7b388": {
      "manifest": {
        "key": "…",
        "url": "https://gw.example.com/anthropic"
      }
    }
  }
}
```

- Records are grouped by `store_id`, so two configs in one folder share the
  file without colliding. Each record holds `key`, and optionally `url` (a
  gateway URL may carry a token, so it is kept with the key) and `owner`
  (the config file that wrote it). A URL in the record and a `url` in
  `config.toml` together make the endpoint unavailable.
- Keep the file readable only by you (`chmod 600`); Wisp warns at start and in
  `wisp doctor` when it isn't, and writes it `0600`. It must be a regular file:
  a symbolic link makes its endpoints unavailable.
- A stored key must be printable ASCII with no whitespace, at most 4096
  characters; otherwise the endpoint is unavailable with that reason.
- An error in the file (it isn't valid JSON, a field is wrong, a store ID or
  name breaks the rules) names its line and never a value, and makes every
  `key_store = "file"` endpoint unavailable; other endpoints still work. A file
  from a newer Wisp is refused whole.
- A missing record names the file and says what to do: copy the
  `credentials.json` of the folder the config came from, or give the endpoint
  a new key with `wisp account edit NAME --replace-key`.
- **Copied configs.** A copy of `config.toml` keeps its `store_id`, so a copy in
  the same folder reads the original's keys, and a copy in another folder
  doesn't (its own folder's `credentials.json` doesn't hold them). `wisp
  doctor` says when a record belongs to another config file. The copy's first
  change through `wisp account` gives it its own copy of its keys: a new
  `store_id`, with each record copied under it, so a later `remove` never
  deletes the original's key. If a key it needs isn't in its folder's
  `credentials.json`, the change is refused until you copy the original
  folder's `credentials.json` beside it (or `remove` the endpoint whose key is
  missing, or give it a new key with `edit NAME --replace-key`). Records written by hand, with no `owner`, are copied the same way.
  `remove` gives a copy its own `store_id` when the endpoints that remain, or
  the removed one, use a record it didn't write, and deletes the removed
  endpoint's key only when this config wrote it. The line about the copy names
  only where the copied keys came from, and is left out when nothing was
  copied.
- Wisp takes `.wisp.lock` in the folder of `credentials.json` (and of
  `config.toml`) while it writes, and unlocks it before closing it, as the
  session log does (`docs/session-log.md`). The lock file holds nothing and stays. When
  `credentials.json` or a lock file sits inside a git work tree (a dotfiles
  repository, for example, found by the project root's rule in
  `docs/instructions.md`), add `credentials.json` and `.wisp.lock` lines to
  its `.gitignore`; doctor warns until then.

Stored endpoints are ordinary named endpoints: an exported `ANTHROPIC_*`
variable still wins the start over a stored `default`, as described below.

Browser OAuth records are managed by `login`. Their `key` is an opaque local
profile handle. A protected `oauth/<config-identity>/oauth-<opaque>.json` under
the canonical config directory holds the token bundle for file storage, or a
locator for keychain storage. Each config has its own profile namespace: a
copied config needs its own login and cannot delete the original's profile.
The keychain
backend holds the complete bundle when selected; tokens never fall back to
plaintext after a keychain failure. Profile files and their containing folder
must belong to the current user, be private, and have no symbolic-link path.

## Keychain

An endpoint with `key_store = "keyring"` keeps its key (and URL) in the system
keychain: the Secret Service on Linux and the BSDs (GNOME Keyring, KeePassXC,
KWallet's Secret Service), the macOS Keychain, or the Windows Credential
Manager. Each item is under the service `wisp` and the account
`<store_id>/<name>`, and holds the same fields as a `credentials.json` record
plus a version, so a copied config reads it the way it reads the file.

The `keychain` setting says whether Wisp uses it:

| Value | Meaning |
|---|---|
| `auto` (the default) | Use the keychain where this machine has one: on macOS and Windows always; on Linux and the BSDs when a Secret Service owns (or can start) `org.freedesktop.secrets` on the session bus, asked with a 2-second limit; never under WSL, where Windows' keychain isn't reachable and a Linux one rarely runs. |
| `on` | Always use it, even under WSL. |
| `off` | Never touch the keychain: keys live in variables and `credentials.json`. |

It is set, highest first, by `--keychain auto|on|off`, the `WISP_KEYCHAIN`
variable, or `keychain = "…"` at the top of `config.toml` (`wisp config set
keychain off` writes it). `wisp doctor` shows it on its `keychain` row: the
backend and how many keys it holds, or `none (WSL)`, `none (no Secret Service
on the session bus)` or `off (WISP_KEYCHAIN)`, and `not running` or `didn't
answer` when it failed.

- **Where new keys go.** `add` and `import` with `--store auto` (the default)
  use the keychain when the setting finds one, and otherwise
  `credentials.json` with a one-line note saying why. `--store keyring` and
  `--store file` choose. If the keychain fails a write, a terminal asks
  whether to keep the key in `credentials.json` instead (`import` asks once
  for the run); a script stops with nothing saved and names `--store file`.
- **Waiting.** Wisp talks to the keychain on its own thread, over one
  connection, with one deadline for the whole run: 60 seconds on a terminal
  (after 2 seconds it says it is waiting, since the keychain may be asking to
  be unlocked) and 10 seconds without one. An endpoint other than the one
  Wisp starts on waits at most 2 seconds. After the first failure (no answer,
  locked, or not running) the keychain counts as down for the rest of the run,
  and its endpoints are unavailable with that reason; Wisp never falls back to
  a `credentials.json` record of the same name.
- **Items Wisp can't use.** An item from a newer Wisp (`the keychain item for
  direct is from a newer Wisp`) or one that isn't a valid record (`… isn't a
  valid Wisp record (a record needs a key)`) makes its endpoint unavailable.
  Wisp never rewrites or deletes it; replace it with your keychain app.
- **Under `off`**, `--store keyring` is refused, and so is any change that
  would read, rewrite, move or delete a keychain item of an existing endpoint
  (`remove` without `--keep-key`, `edit` with `--store file`, `--name`,
  `--replace-key`, `--key-stdin`, `--key-var`, `--url`, `--url-var`,
  `--no-url` or `--provider`): `direct's key is in the keychain, which is off
  (WISP_KEYCHAIN); run this with --keychain on`.
- **Copied configs.** A copy's first change copies each keychain item under
  its new `store_id`, like file records. While the keychain can't be read, a
  copy that must do this is refused, naming the keychain endpoints; nothing
  changes.

Platform notes:

- **Linux and the BSDs:** a desktop session usually runs GNOME Keyring or
  KWallet. Headless machines and containers usually don't: `auto` then uses
  `credentials.json`. To run one, start a session bus and an unlocked keyring
  (`dbus-run-session -- sh -c 'echo pw | gnome-keyring-daemon --unlock; …'`).
- **WSL:** `auto` never uses a keychain; `--keychain on` uses a Secret Service
  running inside WSL if you start one.
- **macOS:** the Keychain asks whether `wisp` may read each item, and it asks
  again after every new build of `wisp` (a rebuilt binary is a new program to
  the Keychain). Choose "Always Allow" to stop the questions for that build.
- **Windows:** items appear in Credential Manager under "Windows
  Credentials" with the target `<store_id>/<name>.wisp`.

**zbus stays on 5.** zbus 6 removes the blocking API that the session-bus check uses, and the Secret Service keyring stores (`zbus-secret-service-keyring-store`, `secret-service`) still require zbus 5. Move to zbus 6 once those stores do, porting the check off `zbus::blocking` at the same time.

## Keychain first

Where Wisp finds keys depends on whether this machine has a keychain, as the
`keychain` setting decides it (above):

| Machine | Keys come from |
|---|---|
| A keychain is available (`on`, or `auto` on macOS, Windows, or Linux and the BSDs with a Secret Service), even when it is locked or down | The keychain and `credentials.json` only |
| No keychain (`off`, or `auto` under WSL, in a container, on a headless server, in Linux CI) | Stored keys, plus variables: the real environment, `~/.config/wisp/.env`, `--env-file`, `key_env`/`url_env` and the keys-file endpoint |

On a machine with a keychain:

- No variable, env file or `--env-file` is read for a key or URL. The
  keys-file endpoint isn't built, so `--account env` (or `default = "env"`)
  stops with `the keys-file account is unavailable: ~/.config/wisp/.env and
  the ANTHROPIC_* variables aren't read on a machine with a keychain; wisp
  account import --from-environment stores them`.
- A table with `key_env` is unavailable: `its key is in the variable
  MANIFEST_API_KEY, which Wisp doesn't read on a machine with a keychain; wisp
  endpoint import stores it`.
- `--env-file` on a session is refused, not ignored: `--env-file supplies
  variables, which Wisp doesn't read on a machine with a keychain. Store its
  keys once with: wisp account import --env-file FILE`. The commands that
  import keys (`wisp account add --env-file … --key-var …`, `wisp account
  import --env-file …`) still read it, once.
- A key Wisp finds where it won't read it is never used silently. When no
  endpoint with `key_store` is configured and nothing can start, Wisp names
  where it found keys (names only, never values) and the one command that
  stores them, for example:

  ```
  wisp: no account is set up yet. ~/.config/wisp/.env holds ANTHROPIC_API_KEY, but this machine has a keychain, so Wisp doesn't read it. Store it once with: wisp account import
  For one run without the keychain: --keychain off
  ```

  The other forms say `ANTHROPIC_API_KEY is set … wisp account import
  --from-environment` for an exported key, and `config.toml's endpoints name
  variables … --from-environment` (or `--env-file FILE` when the variables are
  in neither place) for `key_env` tables. With nothing found, the message
  doesn't suggest variables: `no account is set up yet. Run "wisp setup" in a
  terminal, or store a key with:` followed by `printenv VAR | wisp account
  add NAME --key-stdin`. When `config.toml` has an endpoint with `key_store`,
  its own reason stands (`the default account "direct" is unavailable: the
  keychain didn't answer`), even with a key exported; an endpoint named by
  `--account` or `WISP_ACCOUNT` also keeps its own reason.
- A broken `~/.config/wisp/.env` doesn't stop a start, since it isn't read;
  `wisp doctor` warns about it.
- To find keys, Wisp looks only at variable names: whether each provider
  variable and each `key_env`/`url_env` is set to something non-blank in the
  environment or the env file. The session log still scrubs every value those
  variables hold, except values shorter than three non-whitespace characters
  (an env file that isn't valid UTF-8 is scrubbed line by line
  where it can be read, and doctor says it isn't), so a key a tool prints is
  replaced as before.

### Scripts and CI

Where no keychain is available (Linux CI, containers, headless servers, WSL by
default), scripts work as before: `ANTHROPIC_API_KEY=… wisp …`, `--env-file`,
`key_env` tables and `op run`. On a machine with a keychain (a macOS or
Windows runner, a Linux desktop), a job either passes `--keychain off` (or sets
`WISP_KEYCHAIN=off`), so the machine counts as having none, or stores its key
in a scratch config with `--store file`, so no keychain is touched:

```sh
dir=$(mktemp -d)
printenv MY_KEY | wisp --config "$dir/config.toml" endpoint add ci --key-stdin --store file --no-check
wisp --config "$dir/config.toml" --output jsonl …
rm -r "$dir"
```

Wisp's own tests that run the `wisp` binary set `WISP_KEYCHAIN=off`, so every
machine counts as having none and no test touches a developer's keychain;
the keychain-first tests point `WISP_KEYCHAIN=on` at a session bus that doesn't
exist, so the keychain counts as available but down.

## Commands

`wisp account` adds, edits, lists and removes endpoints, imports keys from
variables, and chooses the default. It
edits `config.toml` in place, keeping your comments, blank lines and key order,
and writes the key to the selected store; nothing secret ever goes in
`config.toml`.

```sh
wisp account add                    # asks for everything, the key hidden
wisp account add work --url https://gw.example.com --auth bearer
printenv WORK_KEY | wisp account add work --url https://gw.example.com --auth bearer --key-stdin
wisp --env-file work.env endpoint add work --key-var WORK_KEY --url-var WORK_URL --auth bearer
wisp account add work --auth bearer --key-env WORK_KEY --url-env WORK_URL
wisp account add openai-api --provider openai --auth api_key
wisp account edit work --replace-key
wisp account edit work --name main --pricing none
wisp account import                 # stores the keys in ~/.config/wisp/.env
wisp account import --from-environment
wisp account list
wisp account default work
wisp account remove work            # asks first (Remove or Keep); --yes for scripts
```

- **`add [NAME]`** asks for what its flags don't give: where it connects (the
  provider's API, or a gateway at a URL you type), how the gateway takes the
  key (`x-api-key` or `Authorization: Bearer`), the key (hidden as you type or
  paste; only its last four characters are shown back), and a name. Flags:
  `--provider anthropic|openai|gemini`, `--url`, `--auth` (the provider's choices above), `--store
  auto|keyring|file` (see [Keychain](#keychain)),
  `--pricing list|none`, `--default`, `--no-check` and `--key-stdin`.
  - `--key-stdin` reads one line from a pipe (never a terminal), for scripts.
    Without a terminal, `--url` needs `--auth`: it is never guessed.
  - A key is never taken as a flag: `--key`, `--api-key` and `--token` are
    refused, since the command line lands in your shell history and is visible
    to other programs.
  - The key is checked locally (printable ASCII, no spaces, at most 4096
    characters), then with the provider's check, as in `wisp doctor --check`:
    Anthropic token counting or an OpenAI or Gemini API model-list request. Subscription
    modes skip the remote check. A refused key (401) asks again, or stops a
    script with nothing saved; a 403 or an unavailable check saves with a note; any
    other failure offers to try again or save anyway, and stops a script
    (`--no-check` skips the request).
  - A plain `http://` URL to a host other than this machine asks before going
    on, since the key would be sent unencrypted.
  - `--key-var NAME` (and `--url-var NAME`) read the key (and URL) once, from
    the file given with `--env-file` or, with `--from-environment`, from the
    environment; one of the two is required, never both. The values are
    checked and stored like a typed key; the variables are not read again.
    A variable that isn't set, or a URL variable that isn't `http` or `https`,
    stops with nothing saved and names the flag, never the value.
  - `--key-env NAME` (and `--url-env NAME`) record only the variables' names,
    so the endpoint reads them at each start as before; nothing is read or
    checked now (`wisp doctor --check NAME` checks it). They can't be combined
    with `--store`, `--key-stdin`, `--key-var`, `--url-var`, `--url` (with
    `--url-env`) or `--from-environment`.
  - A variable name must be letters, digits and `_`. Something that looks like
    a key (20 or more characters with a lowercase letter, a digit and no `_`)
    is refused and never shown, and a hand-written `key_env` of that shape is
    shown everywhere as `(looks like a key, not shown)`.
- **`edit NAME`** changes an endpoint in place; with no flags on a terminal
  it lets you choose what to change. Flags: `--replace-key` (asks, hidden),
  `--key-stdin`, `--key-var`/`--url-var` (with `--env-file` or `--from-environment`, as for
  `add`), `--url`, `--no-url` (back to the provider's own URL), `--provider`,
  `--auth`, `--pricing`, `--name NEW` (renames it, and `default` with it),
  `--store keyring|file` (moves its key) and `--no-check`. It prints
  `Updated "NAME": …` with what changed.
  - `--store` writes the key to the new store, then `config.toml`, then
    deletes it from the old store. If the keychain can't delete the old item,
    Wisp says so, names its account, and exits 1.
  - A new key, URL or provider for a stored endpoint is checked like `add`
    (unless `--no-check`); a new auth is not. A new key or URL is written to
    the record, which keeps its URL unless you give another. A new URL for a stored endpoint whose `url`
    is still in `config.toml` moves it into the record and removes it from
    `config.toml`.
  - A stored endpoint whose record is missing can be given a new key; if the
    lost record held its URL, give `--url`, `--url-var` or `--no-url` (a
    terminal asks), which is settled before the key is read.
  - An endpoint that reads its key from a variable takes `--key-env` and
    `--url-env` to rename its variables, plus `--url`, `--no-url`, `--auth`,
    `--pricing` and `--name`; nothing is read or checked. Key flags are
    refused: `import` stores its key first. A stored endpoint refuses
    `--key-env`: remove it and add it again.
  - Changes through a copied config give it its own copy of its keys first, as
    for `add` and `remove`. A copy that can't is refused before the new key
    is read or checked. A rename writes the new record, then `config.toml`,
    then deletes the old record.
- **`import`** stores keys that today come from variables, reading only one
  source: `~/.config/wisp/.env` by default, the file given with `--env-file`,
  or the environment with `--from-environment`. It lists what it found:
  - each `key_env` table that would resolve from that source alone (its key,
    and its `url_env` as an `http` or `https` URL). The others are listed as
    skipped with the reason, plus `try --from-environment` (or `try
    --env-file …`) when the other source holds both variables. An imported
    table becomes `key_store = "file"`, keeping its name, comments and other
    fields, and its URL moves into the record;
  - the provider's own variables (`ANTHROPIC_API_KEY` or
    `ANTHROPIC_AUTH_TOKEN` with `ANTHROPIC_BASE_URL`, `OPENAI_API_KEY` with
    `OPENAI_BASE_URL`, or `GEMINI_API_KEY` with `GEMINI_BASE_URL`), unless they match an
    imported table, which become a new endpoint named after the provider (`anthropic`,
    `openai` or `gemini-api`; a terminal asks for the name).
  - A bearer token whose URL variable is set only in the other source gets a
    warning before any check, since it would be checked against the
    provider's own URL: a terminal asks; a script skips it unless `--no-url`.
  - Each key gets the local check and the `count_tokens` check (unless
    `--no-check`); a failure skips that one. A terminal confirms each import.
  - When there was no default, the first import becomes it. Then, for the
    default env file holding only imported variables, a terminal offers to
    delete it; otherwise Wisp names the lines it no longer needs. The env file
    is never edited.
  - The exit code is 1 when nothing was imported or an import that was tried
    failed; tables skipped because their variables aren't in the source don't
    count as failures. `--store auto|keyring|file` is as for `add`.
- **`list [NAME]`** prints `wisp status`'s panel (above): no host except a
  gateway's, no key place and no masked key.
- **`default NAME`** makes `NAME` the endpoint Wisp starts on.
- **`remove NAME`** removes the endpoint's table and deletes its stored key; an
  endpoint that reads its key from a variable leaves the variable alone. It
  clears `default` if that named it. It asks first, with Keep marked; `--yes`
  skips the question.
  `--keep-key`, for a keychain endpoint only, removes the table and always
  leaves its item, whether or not the keychain answers, naming its account:
  `Its key is still in the keychain (item <store_id>/direct); run wisp
  endpoint remove direct when the keychain answers to delete it`. Without it,
  a remove whose keychain item can't be reached is refused and names
  `--keep-key`. `remove NAME` for a name with no table deletes a leftover key
  this config wrote, in `credentials.json` or the keychain.
- Ctrl-C or Ctrl-D at any question cancels with nothing saved (exit 130).
  Answering No or Cancel to a question about the check or a plain `http://`
  URL declines: nothing is saved, and the exit code is 1.
- What the change did prints on stdout, including the line saying a copied
  config now keeps its own copy of its keys (a result: its `store_id`
  changed). Notes about the files, such as a hard link or a git work tree,
  print on stderr.
- `--env-file` is the source for `--key-var`, `--url-var` and `import`; the
  other commands, and `add` or `edit` without `--key-var` or `--url-var`, say
  it isn't used and carry on.
- Every write takes the `.wisp.lock` files only while it writes, never while it
  waits for you, so two commands at once both land; one that can't get the
  lock in time says another wisp is changing accounts. `add` writes the key
  before `config.toml`, and `remove` changes `config.toml` before deleting the
  key, so an interruption leaves at most an unused record, never an endpoint
  without its key.
- A new `config.toml` (and its folder) is created readable only by you. A
  linked `config.toml` is written through the link, keeping the target's mode,
  and its `credentials.json` stays beside the link; a target folder Wisp can't
  write changes nothing, and a link to a file that doesn't exist is refused
  before anything is written. A `config.toml` or `credentials.json` with
  other hard links gets a note: the other names still hold the old text,
  including any removed key.
- A `config.toml` that doesn't parse is never edited: fix it first.
- Usage errors (exit 2) never repeat what you typed, since a pasted key can
  land anywhere on a command line: they name the flag or the command and, for
  a choice, the values it takes.

## The keys-file endpoint

Where no keychain is available, today's `ANTHROPIC_*` variables form an
endpoint whenever `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` is set, in the
environment or the env file (with `ANTHROPIC_BASE_URL` from the same place).
It is named `claude-api`, or `gateway` when `ANTHROPIC_BASE_URL` points
anywhere but Anthropic's API; the name is real, so it shows on every screen,
in `wisp status`, in `wisp doctor` and in new session logs. With a
`config.toml` it is listed beside your named endpoints. If it is the same key
and URL as a named endpoint, it is hidden (`wisp doctor` shows
`gateway  same as manifest`), and choosing it starts on its named twin.
Both credentials set makes it unavailable, or stops Wisp if it is chosen.

While it exists, its name is taken, so `wisp account add` and `/account`
ask for another. If a named endpoint already holds the name, the named one
keeps it and the variables aren't used; the start, `wisp status` and
`wisp doctor` say so in one line:
`claude-api is a saved account, so the key in Wisp's keys file (~/.config/wisp/.env) isn't used. Rename one to use both.`
Choosing `env` then fails with that line as its reason.

`env` stays accepted wherever a name is typed or read: `--account env`,
`WISP_ACCOUNT=env`, `default = "env"`, a preset's `endpoint`, a
`--set endpoints.env.…` flag, `wisp config get endpoints.env.…` and older
session logs. It means this endpoint under its current name, so a config or
log naming `env` keeps working when the name changes between runs, and a
`default = "env"` shows as that name in `/config`, `wisp config` and
`wisp doctor`. A log
naming `claude-api` after the URL changed finds no such account, and resume
moves the chat to `gateway` with one line, as for any gone endpoint
([Resuming](#resuming)).

`[endpoints.env]` holds settings for it and nothing else: `native_web_search`,
`pricing`, `background_compaction`, `background_tokens`, `max_in_flight` and
`models`. They apply under either name, as do `--set endpoints.env.…` flags, unless a
named endpoint holds that name; a change saved from `/config` or `wisp config set` on this endpoint is
written there, and choosing it as the start endpoint writes
`default = "env"`. A
connection field there (`provider`, `auth`, `key_env`, `key_store`, `url`,
`url_env`) stops Wisp at start with its line:
`the keys-file account in config.toml takes no connection fields: its connection comes from the ANTHROPIC_* variables; add a named account to configure one`,
and `wisp config set` refuses one the same way. The table never makes a
named endpoint: it isn't listed unless the variables are set, doesn't count
toward binding controls' "more than one endpoint", and doesn't change which
endpoint Wisp starts on. `wisp account edit`, `rename`, `import` and `remove`
leave it as it is. When the endpoint is hidden as a named endpoint's twin, the
twin's table applies, and `wisp doctor` says the env table is unused; it says
so too on a machine with a keychain, where the variables aren't read.

## Choosing at start

Where no keychain is available, the first match wins:

1. `--account NAME` (`--endpoint NAME` and `WISP_ENDPOINT` still work).
2. `WISP_ACCOUNT=NAME`, from the real environment only (never the env file).
3. The keys-file endpoint, when the real environment sets `ANTHROPIC_API_KEY` or
   `ANTHROPIC_AUTH_TOKEN`, or the file given with `--env-file` does. If
   `config.toml` has a `default`, Wisp says so:
   `Using ANTHROPIC_API_KEY from the environment, not the default account "manifest" (--account manifest to use it)`.
   Pass `--account` or unset the variable to use the default.
4. `default` in `config.toml`. If it is unavailable, Wisp stops with the
   reason, followed by `(wisp account list shows your accounts)`; it never falls
   back to another endpoint.
5. The keys-file endpoint, from the default env file.
6. The only available named endpoint, when there is no `default`.
7. Otherwise Wisp stops: `no account chosen: use --account (available: direct, manifest)`.

With nothing at all (no endpoint, no `default` and no key in a variable),
Wisp sets one up first, as below.

On a machine with a keychain, the first match wins:

1. `--account NAME`.
2. `WISP_ACCOUNT=NAME`, from the real environment.
3. `default` in `config.toml`; if it is unavailable, Wisp stops with the
   reason (or, with no stored endpoint configured, with the
   [Keychain first](#keychain-first) message).
4. The only available endpoint, when there is no `default`.
5. Otherwise first-run setup, or the [Keychain first](#keychain-first)
   message.

## First run

`wisp` asks you to set up an endpoint when all of these hold:

- nothing is configured: `config.toml` is missing, or names no endpoint and no
  `default`, and no `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` is set in
  the environment or the env file (a `config.toml` that doesn't parse stops
  Wisp with its error instead);
- none of `--config`, `--env-file`, `--account` or `WISP_ACCOUNT` is
  given, since those mean scripted use;
- stdin, stdout and stderr are terminals and the output is the full screen.

On a machine with a keychain, setup first offers to store keys it found where
it won't read them (names and places only, never values):

```
Welcome to Wisp. No account is set up yet.

This machine has a keychain, so Wisp keeps keys there and doesn't read them
from environment variables or keys files. It found:
  ANTHROPIC_API_KEY in your environment
  manifest in config.toml, with its key in ~/.config/wisp/.env
Store these in the keychain? Each is checked first.
› 1  Yes
  2  No
enter choose · esc cancel
```

Yes, which Enter alone takes, runs `wisp account import`'s steps for the
environment and then the env file, each key checked first; No goes on to the
setup card. First-run setup
lists only the provider's variables; the `config.toml` lines appear under
`wisp setup` run by hand. The offer is asked on the normal screen,
before the full screen opens. When accounts are set up but none can be used,
the offer comes the same way, without the welcome line.

Then the full screen opens on the sign-in card instead of the composer:

```
 ◉ wisp

 ? Sign in to start

 › 1 Claude        Pro or Max subscription
   2 ChatGPT       Plus or Pro subscription
   3 Antigravity   Gemini subscription
   4 API key       Claude, OpenAI, Gemini or a gateway

   enter choose · esc quit
```

A subscription row signs in at once, with `/login`'s card (`docs/commands.md`),
on the provider's own name (`claude`, `chatgpt` or `antigravity`), with no name step; Esc on
it comes back to these rows. A sign-in that ends ready becomes the `default`,
the session starts on it, and the chat opens with `✓ Signed in · Claude
subscription · claude`. Before a session the link is copied only through the
terminal (OSC 52). Any sign-in made while no account and no `default` is
configured becomes the `default` the same way, also after the last sign-out.

`4 API key` opens the setup card; Esc there, with nothing saved or saving,
comes back to the rows:

```
 ◉ wisp

   Let's connect a model.

   Provider   Anthropic
   Connect    ● Anthropic's API · api.anthropic.com
              ○ A gateway or proxy
   Key        sk-ant-api03-…************…WXYZ  108 characters
 › Name       anthropic

   enter next · tab move · esc quit
```

The questions are those of `wisp account add`: where it connects, the URL and
how a gateway takes the key, the key and a name. Enter moves to the next field
and saves from the last; Tab and Shift-Tab move; Up, Down or a number picks a
choice. A typed key or sign-in code shows only stars. A paste that is not a
key says so at once and leaves the field unchanged. A pasted key shows as the
sign-in code's echo does: its known prefix (such
as `sk-ant-api03-`) or else, for a long key, its first 4 characters, then
stars, its last 4 characters and its length. A narrow card shows fewer stars
and no length. A key with fewer than 16 characters after its prefix shows only
stars, one per character, or as many as fit and its length when they don't
fit. The rest of the key is never shown, and it never reaches the composer,
its history, the transcript or the clipboard. Keys typed before the card
opened go to its first field. The card checks the name and the key before it
sends them; the save then runs the same free check, and each outcome asks the
same question as `wisp account add` in the card (`That key was refused.`,
`Save anyway?`, `Enter the URL again`). When the keychain doesn't take the
key, the card asks once per run `The keychain didn't answer. Keep this key in
~/.config/wisp/credentials.json instead?`; a keychain write that wasn't
confirmed only shows its reason, without the keychain account, and for the
rest of the run the card never offers `credentials.json`, since the key may
already be in the keychain. On a keychain machine where keys were found but
not imported, the card says so and names `wisp account import`. While the save
runs, the card shows `saving the endpoint…`; anything else you try before the
first save is refused with `set up an account first`. The card's keychain
write gets its own 60 seconds, counted from when the write starts however long
the card was open, so an unlock dialog can be answered; only when they pass
does the keychain count as down.

The assistant's secret box uses the start and end counts from its request
rather than the known-prefix table; a paste is followed by its length whenever
both fit, even when all its stars fit. Typed values show only stars. A value
shorter than 16 characters, with spaces or outside printable ASCII, shows only
stars; otherwise the shown start and end total at most a third of the value,
dropping the start first if needed. No form shows the whole secret. These
limits and the mask's drawing function are shared with the key and sign-in
fields; their existing paste forms and Tab keys stay unchanged.

The new endpoint becomes the `default`, and the session starts on it, with any
start-up warnings as notes only; they print on stderr, after the error, only
when the start fails or the full screen never draws them. The save's notes (for
example a folder inside a git work tree) show in the card and again on stderr
after the full screen closes.
Quitting the card:

| When | Exit | Line on stderr |
|---|---|---|
| Esc on the rows, or Ctrl-C or Ctrl-D, with no save or sign-in running | 130 | `Nothing was set up.` |
| A quit while a save runs, which then saves | 0 | `Saved "gw". wisp starts on it next time.` |
| A quit while a save runs, which then saves nothing | 130 | the cancel line |
| A second quit while a save runs, or a save that doesn't answer in 85 s | 130 | `wisp: an account save was still running; it may not be saved.` |
| A start that fails after the save | 1 | `wisp: <error>` (the endpoint stays saved) |

A sign-in waiting on its code or browser counts as a save here: a quit waits
for it, and one that then ends ready exits as a save does, never with `Nothing
was set up.` When the setup closes, a sign-in still saving finishes with its
account written; one not yet saving is cancelled.

A quit waits for the save rather than dropping it: a save cancelled before it
takes the locks writes nothing, and one already waiting on the locks finishes.
A second quit exits within about two seconds; every write is an atomic
replace, so no file is left half-written.

A configured endpoint that can't start never opens setup: its error stands,
and nothing overwrites your `default`.

Without a terminal, or with `--config`, `--env-file` or `--output jsonl`, nothing found
stops Wisp with the usual message and one more line:

```
wisp: no Anthropic credentials: set ANTHROPIC_API_KEY (an API key) or ANTHROPIC_AUTH_TOKEN (a bearer token for a gateway) in the environment
Or store a key: run "wisp setup" in a terminal, or printenv VAR | wisp account add NAME --key-stdin
```

`wisp setup` runs the same questions on demand and exits after saving. If
endpoints exist already, it lists them and asks whether to add another; the
new one becomes the `default` only when there is none. A file given with
`--config` that doesn't exist yet is created. It needs a terminal.

## Found tools on an account

A turn-start tool-change capability does not prove that the same account and
model accepts a change after tool outputs. Claude reads `tools_after_results`
from the model data; OpenAI reads separate exact-id lists for subscription and
API connections. All eight documentation-eligible Claude fields are false and
both OpenAI after-output lists are empty. No model loads a found tool after
results until a passing live run fills its row; until then every model runs
found tools through `use`, without rebuilding the prefix. The ignored tests
`live_tool_changes::live_found_tool_after_claude_results` and
`live_tool_changes::live_found_tool_after_openai_outputs` are connection-specific.
The Claude test may prove its exact model on the direct API. The OpenAI test
checks the project's gateway only: a pass fills no official API or subscription
row, which requires a passing run on the corresponding listed endpoint. An allowed trial uses the existing
inline-tools refusal handling once, then later searches keep the `use` wording.
Switching accounts does not rewrite a previously committed tool message.

## Switching mid-session

In the full-screen TUI, `/account` (or its hidden alias `/endpoint`) opens a
picker over the bottom of the view, titled `Account`: one row per configured
account (a hidden duplicate of a named account is left out), with `✓` (ASCII `+`) on the
chat's, its kind (`Claude subscription`, `Claude API`, `Gateway · API`), the
model and effort the chat would run there, and, from 60 columns, what the
switch costs next turn. Up and Down move, skipping unavailable rows; typing
filters by name; Enter switches; `→` opens the account under the cursor as
`/model`'s list and `←` folds it back, while `→` on an account that offers no
model or on the sign-in row does nothing; Esc or Ctrl-C closes. Its last row,
`+ Sign in to another account…`, starts the subscription sign-in. `/account NAME` switches
directly, or says why it can't (an unknown name lists the available ones; an
unavailable one gives its reason). `--output jsonl` has no `/account`; start
it with `--account`.

An account switch that names no model uses the model and effort you last chose
on that account, in any chat, when the account still serves that model. An effort
you name replaces the remembered effort; otherwise it adjusts to a level the model
takes. The account this chat is already on always keeps this chat's own pair.
Without a usable remembered pair, the switch keeps the chat's model when the target
serves it; otherwise it takes that account's default, then its first shown model
if its own list lacks its default. The picker, the line below it, `/config` and
the switch itself all use that same pair. A complete list admits only its own IDs;
without one, a model of the account's provider can stay.

Your accepted `/model`, `/effort` and `/account` picks remember the resulting pair
in `~/.local/share/wisp/remembered/<account>.json`, under Wisp's data folder.
A move also remembers the pair this chat was to run on the account it leaves,
only when that account has none yet. If writing fails, the change still applies:
`Could not remember the model and effort for this account.` New chats and resumes
start as before. Moving a running child agent reads the pair, but children never
write it; a new spawn inherits or uses its explicit choices as before.

`/model` opens with the chat's account open, one account at a time: a header
row (`name · kind`) and under it every model that account offers. The usual
rows come first: its provider's `listed` rows from the shipped model file
(`docs/context.md`), or on an account with its own model list ("Each account's
model list") that list's shown entries in its order, then the chat's model on
an account whose list names it, or of its provider without a complete list,
`✓` on it. The rest follow:
the account's own list in its order, or on an account with no list of its own
its provider's other rows. Every other account is one row, as in `/account`:
its name, kind, and the model and effort the chat would run there; an account
that offers no model has no row. Enter on it moves the chat there as
`/account` does, `→` opens it and closes the open one, `←` closes the open
account and leaves for `/account`'s list, and Esc closes. A model with no row
in the shipped model file reads `cost unknown`, and while it is selected the
line under the list adds that effort changes rebuild the cache. Typing filters
by model or account name, each typed word anywhere in either, and lists every
matching account's matching models, with the account's other model IDs and its
own list's hidden entries; a complete list excludes every ID it lacks, including the chat's model. A name no model matches gets a free-text row. The
picker a start opens, which has no `←` or `→`, keeps every account open with
its usual rows, and `/login status` opens `/model` as `/model` does.
Picking a model under another account sends one change with both the account
and the model, and the effort step that may follow keeps that account. The
line under the list names the move and its cost for that exact pair: `Moves
this chat to NAME · cache rebuild ~$0.04`, with the compaction a smaller
window needs and a note when the move changes the billing; the dock keeps the
height of its tallest line under the list, so it doesn't move as focus does.
Prices show from 100 columns, right-aligned under a `cache rebuild` column
title. An account whose sign-in has expired has its header or row in the error
colour, and picking under it signs in first, then switches. A press on the
account chip in the status line opens `/account`.

After a switch, the transcript says where the next message goes, as
`Switched to NAME · billing · model effort` with the rebuild's cost; a
switch made while a turn runs reads `Switching to …` with `From your next
message.` under it, and prints nothing more when it applies. Between turns
the status line and the welcome show the new account, model and effort at
once; while a turn runs they keep the running ones, with `→ NAME · billing`
beside them until the turn ends. `/account NAME` switches as the picker's row
does: to the model that row shows, signing in first when the sign-in has
expired.

A switch to a model the target account's loaded list doesn't name first fetches
that list again ("Each account's model list") before checking whether the model
can be used. A complete list can still refuse it; other accounts still switch. When the
fresh list still lacks the model, the first line under the switch line says so:
`NAME doesn't list MODEL, so the request may be refused.` An account with no
list of its own gets none.
A model that no priced shipped row and no account's list names reads as
stored, `claude-` included, in the banner, the status line and the switch line,
and so does a model Wisp ships no row for in `/config`'s Models row; every
other model keeps its short name.

Switching in the main chat moves only the main agent, and in an agent's chat
only that agent (`docs/agents.md`, "An agent's chat"). A child agent runs on the endpoint its
spawn or its preset names, or else on the one its parent uses when it spawns,
chosen from the endpoints the session started with (`docs/agents.md`, "What a
child gets"), until a client moves it (`ChangeSettings`
sent in `Child`, `docs/agents.md`, "Model, effort and endpoint"). Two endpoint names on one account are two endpoints to Wisp:
agents on them never wait on each other's stagger, and each has its own
`max_in_flight` limit.

`/account add` opens the same
card as first-run setup over the bottom of the view, titled `Add an API key`.
It saves through the kernel, with the same checks and questions; the new
endpoint is not made the `default`, and the picker lists it at once. Esc or
Ctrl-C closes the card and returns to the session; a save already sent still
gets its answer as a note in the transcript. Editing and removing endpoints
stay with `wisp account`.

After a switch, the transcript notes it (`Account → direct · cache rebuild
~$0.04`, naming a model change too), and adds `earlier Sonnet 5.5 reasoning may
not carry over` when the conversation holds thinking from a model whose
thinking is bound to the account that made it. That turn's footer reads
`cache rebuilt: account switched` when
less than half of the prompt was read from the cache; a switch back within the
TTL usually reads the old prefix, so the footer then shows `cached N%`.
With `pricing = "none"`, costs read `cache rebuild, no prices`, or
`cache rebuild up to the whole prompt, no prices` for an effort change that
may rebuild. On a subscription a rebuild reads `cache rebuild ~$0.41,
subscription`, and `~$0.41` where room is short. `/model`, `/account`,
the Retry-on list and `/config` take these words from one formatter.

A switch to a model that can't take images or files keeps the chat going: the model gets a note in place of each one, and the switch back sends them again (`docs/media.md`, "Attachments a model can't take").

A failed turn can also move: when it ends in an error the endpoint might not
repeat, its error cell offers `Retry` and, with another endpoint available,
`Switch account` (`docs/plans/resilience.md` §6). Switch account opens a
`Retry on` picker (the endpoint that just failed is left out). Each row shows
and prices the failed model where that account serves it, else the account's
default, without reading or changing remembered choices. Enter retries there from the last
committed step, keeping the failed turn's effort when the landing model takes
it, otherwise adjusting it to a supported level by the same rule as `/account`.
A model or effort the user names that doesn't fit is still refused. The switch
stays for later turns like any other. When
the failed request was the continuation of a paused reply (a long web
search), the switch closes that reply, since another endpoint can't continue
it, and the Retry's request ends with a fixed user message, `Wisp closed the
paused reply above because it couldn't be continued; continue the task from
where you left off.`, logged as its own `message_committed` so a resume sends the
same bytes; a further Retry of that turn, on any endpoint, ends the same way.
A Retry that lands at the head of a compaction sends only the compaction's
note, `Wisp compacted the conversation in the middle of this turn; continue
the task from where you left off.` A model
or effort you chose for the next turn stays pending. The transcript notes
`↻ Retrying step 3 on gw · cache rebuild ~$0.04`, with the same thinking note
as a switch when it applies. Retry where it failed sends exactly the failed
request again. A Retry to another model on the same account, as a child's move
past a used-up limit makes (`docs/agents.md`, "Reports, and the parent's
guarantee"), is treated as a move to another account: the model and effort
land, the failed reply closes, and later turns stay on that pair. There is no `/retry`; the choice lives on the error cell and
lapses once you send a message, stop, start another turn, or a compaction
completes (a failed or cancelled one leaves it standing).
When the chat's own agent stops on a used-up limit with a known reset or an
Anthropic subscription refusal with a long retry time, and Retry where it
failed is offered, the error cell adds `Wisp retries by itself when it resets.`
for the limit or `Wisp retries by itself then.` for the refusal. A minute after
the reset or retry time, Wisp sends the failed request again on the account
that failed, as Retry does, priced like any request.
A message, a Retry by hand on any account (picking one under `Switch account`
retries there at once), a compaction or a rewind cancels it, and a switch to
another account queued for your next message holds it until you switch back;
the line shows only while the retry is still to come. Esc only closes the
choice, and the line and the retry stay. An agent below the chat never
retries by itself, and a resumed chat does so only while the reset or retry time is ahead.

The kernel side: a switch applies from the next turn, and
the running turn, tool steps included, stays where it started. Every switch
is reported as a cache rebuild, since two endpoints may not share a cache, and
the conversation carries over unchanged, with two exceptions for web search:
`tools` follows the new pair's search form, and the earlier searches may be
sent as text ([Web search](#web-search)). Wisp asks for thinking binding
controls from a new session's first request when more than one endpoint is
configured, so that the request shape never changes on a switch, and on any
pair where they are usable: the model's `adaptive_thinking` capability and a
pair state that hasn't fallen back (documented on the provider's own origin,
a trial on a gateway). A session logged before that rule keeps them off on a
plain resume, so the resume sends the same bytes, until a break turns them on.
A session that kept broken thinking (`docs/context.md`, "Broken thinking")
and then runs on a pair where binding controls aren't usable (a refused
trial, a model without them) leaves out every thinking block once there, with
a `thinking_cut` from `{"message":0,"block":0}` and no `tools_sha256`, and the
cut stays on every later pair, rewinds included, so nothing is put back. Thinking blocks from another
account are dropped silently by the API, with no error. If an endpoint rejects
earlier thinking blocks as bound to a different conversation (for example
after the history passed through something that changed it, which can happen
without any switch), the turn ends with a `thinking binding` error; after a
switch, it names the endpoint to go back to.

## Each account's model list

A ChatGPT subscription signed in with `wisp login`, a Claude subscription signed in the same way, an Antigravity subscription signed in the same way, a Claude API key or bearer token on its account's URL (Anthropic's or a gateway's), and a Gemini API key or bearer token on Google's own URL read the account's own model list from that URL; a Claude setup token, an OpenAI API key and a Gemini gateway keep the shipped tables. For each model the list names, the list wins for every fact it carries and the tables fill the rest. Claude and ChatGPT use the tables for a model the list lacks. A loaded Gemini list, from an API key or a bearer token, is complete: a model it lacks is refused; Gemini uses the shipped row only before a list loads.

| Account | Request | Facts taken | Shown in `/model` |
|---|---|---|---|
| ChatGPT subscription | `/codex/models` | default window (`context_window`), maximum window (`max_context_window`), effort levels, default level, image input, search | the entries the list marks `list`, in its priority order; the rest follow in the open account's list |
| Claude subscription, API key or bearer token | `GET /v1/models?limit=1000` on the account's own URL, with the account's usual headers | window (`max_input_tokens`), max output (`max_tokens`), effort levels, adaptive thinking, image input, web search | of each model line, the newest active entry the tables know, ordered as the tables' listed rows, then every active entry the tables have no row for, in the list's order; the rest follow in the open account's list |
| Antigravity subscription | `POST /v1internal:fetchAvailableModels` on the account's Cloud Code PA URL | default model (`defaultAgentModelId`), search models (`webSearchModelIds`), window (`maxTokens`), max output (`maxOutputTokens`) | listed families and their effort variants in `models.toml` order (variants `low` upward; a listed `defaultAgentModelId` stays in its family's place), then an unlisted `defaultAgentModelId`, then unlisted shipped thinking rows, non-thinking shipped rows and unshipped rows newest version first (tie-broken by ID); saved lists are re-sorted on load |
| Gemini API key or bearer token | `GET /v1beta/models` on Google's URL | window (`inputTokenLimit`), max output (`outputTokenLimit`) | listed rows in `models.toml` order, then unlisted shipped thinking rows, non-thinking shipped rows and unshipped rows newest version first (tie-broken by ID); saved lists are re-sorted on load |

ChatGPT's list request sends the Codex client version as `client_version` (see Client versions below). The server can hide newer models from older client versions.

To check that the shipped fallback still supports the latest stable Codex catalog on GitHub, run:

```sh
cargo nextest run -p wisp-core --lib --run-ignored ignored-only models::tests::shipped_codex_version_supports_the_latest_catalog
```

To bump it, change only `version` in `[providers.openai.client]` in `crates/wisp-core/data/models.toml` to the newest stable Codex release's version (without `rust-v`). The check fails if any model in the latest stable catalog needs a newer version than the shipped fallback.

On a ChatGPT subscription, Wisp takes the listed default, lowered to the listed maximum if it is smaller. Values below 104,000 tokens are ignored. A `compact_at` in tokens that the default can't hold grows the window to the smallest one whose `window − reserve` reaches it, at most the listed maximum or a proved overflow (`docs/context.md`, "The point and the reserve"). Without a usable default, Wisp takes the smallest of the model's shipped window, the listed maximum (or the older saved `window` if the entry has neither new field), and the shipped fallback (`window` in `[providers.openai.codex]`). The fallback only lowers a known window: if there is neither a shipped model window nor a usable list bound, the window stays unknown. An older saved window remains only a bound until a connection loads a freshly fetched list.

A Gemini or Antigravity entry carries only its window and max output (with account-level `default_model` and `web_search_models` saved alongside the list on Antigravity); the shipped row supplies its other facts, so old saved copies of levels, default level, adaptive thinking, image input and search are ignored. For other providers the levels are exactly the known ones the list names, gaps included, and a list that names no default level keeps the tables' when its levels hold it. A model only a list names runs with what the list says and nothing only the tables know (per-message effort, inline tool changes, message threads, native compaction), its cost unknown until it is priced. On other accounts, a window too small for a compaction point is ignored and the tables' value applies. A max output below 32,000 tokens is ignored only where the tables have a value to use instead; a max output of 0 is always ignored. A list that is unreadable, misses a field Wisp needs, or shows no entry is a failed fetch. Wisp keeps only those fields, never display names, dates or other text, in `models/<key>.json` beside `sessions/` in its data folder (folder 700, file 600), one file per sign-in, named by a hash of a sign-in's request URL and profile path, or of an API key's or bearer token's account name and its URL's origin (scheme, host and port); neither the key nor a token in the URL's path or user info is hashed into it. A file an older build saved under the whole URL is left unused. Beside it, `models/<key>.lock` is the empty file the check below takes its claim on (file 600).

The list is fetched after a sign-in, and at start for each subscription with no saved attempt, waiting up to 5 seconds for the lists, plus up to 3 seconds only if a client version lookup starts. An interactive Wisp checks ready accounts that have a list and aren't signed out in the background at startup and roughly every 15 minutes. It refreshes a successful list after an hour and retries a failed or interrupted fetch at the first check at least about 15 minutes later. A failure keeps the last list and shows no message. A one-shot run skips these background checks. The check sends the list request in the table above, with the account's own address, credential and identity headers. A subscription may first look up its client version. If several Wisps check at once, only the one that claims the list's lock file fetches; it saves the attempt's time before sending the request.

Choosing a model absent from a loaded account list, through a startup model flag (even in a one-shot run), settings or an agent's explicit, preset or fallback choice (including a retry after a usage limit), fetches that list once more before the choice is checked or a warning is shown. At startup this happens only if no candidate account already offers the model (`docs/commands.md`, "`wisp --model NAME`"). Wisp waits up to 5 seconds for the fetch, plus up to 3 seconds only if a client version lookup starts. The fetch happens once per account and model name in the session, shared by startup, settings and spawns; simultaneous choices wait for the same fetch and deadline even if the first choice is cancelled. An effort choice fetches only if it changes the model ID, and a fallback fetches only when its choice is reached. This selection-triggered fetch does not wait for the next background check. Its result sets the next background refresh time in the same way.

A connection reads its account's list when it is made and takes from a newer one only entries for models it has no entry for and has sent no request for, so no byte a connection sends changes: a model the chat uses, or any name a request has already carried, keeps its facts until the connection is replaced (a new sign-in, an `/account` edit, or a change seen from another window) or the next start. A connection made with no list keeps none, and its account shows no rows from the list until the next start. What a connection adds reaches `/model` through the endpoints event. `/context` and `wisp doctor` name a listed window's source: `subscription` or `API`.

`wisp account models NAME` fetches the list live on any account and prints its IDs, shown entries first; when the answer isn't a full list (a gateway that sends only IDs, or a list with nothing shown), it prints each entry's `id`, sorted.
Add `--latest` to record the latest subscription client release and then list models with it (or with the version set by hand). This does not force a save of the model list.

## Client versions

An unpinned ChatGPT, Claude or Antigravity subscription can automatically check
for the latest stable release of Codex, Claude Code or Antigravity CLI on
GitHub. It checks when fetching the model list from its own origin, after the
list request's access token is ready. API keys, bearers, gateways and Antigravity
on another origin do not check. Neither inference nor `wisp doctor --check`
checks releases.

Each client has a record in `models/client-<provider>.json` with the last version
found and the last attempt time. The process holding the client's lock saves
the attempt before it makes the request. Other processes use the record without
waiting. Automatic attempts are at least an hour apart per client and shared
models folder, even after failure. An idle Wisp may send no list request at all.

Both automatic checks and `--latest` time out after three seconds. The request
goes directly to GitHub, without a proxy, redirect or credentials, and sends
Wisp's own user agent, not an account's identity. If an automatic check fails,
the list uses the last version found, even if older than the shipped fallback.
If none has been found, or there is no data folder, it uses the shipped version.

A list can use a newly found version immediately. Inference and other requests
keep the version read when their connection was made. Client versions do not
change prompt bytes or cache keys. If a provider refuses a version, Wisp reports
the request error as usual; it does not roll back automatically.

### Looking up now

`wisp account models NAME --latest` checks before it fetches the list request's
access token, even if the last attempt was less than an hour ago. It uses the
same lock and doesn't wait if another Wisp holds it. If the release can't be
looked up or saved, it doesn't fetch the list.

For an account with a list already loaded, a running Wisp can see newly
available models the next time it fetches that list: in a background refresh
when due, or when you pick a model by name that isn't in the loaded list. An
account without a loaded list waits until the next start. A new session reads
the release for its connection, uses its saved list while fresh, and reports
the release on its next list fetch. An open connection keeps its version for
other requests.

### Setting it by hand

Set `endpoints.NAME.client_version` to the public release number of the subscription's client, such as `0.168.0`. Use `wisp config set`, `--set endpoints.NAME.client_version=0.168.0` for one run, or **Client version** on the account's `/config` page. Clear it with `wisp config unset` or `d` there. Only the provider receives that version, never the release host. It takes effect at the next connection and skips automatic release lookups for that account. `--latest` still records a release for the other accounts of that provider.

## Busy endpoints

Recoverable failures keep retrying until the request succeeds or you stop
it. This applies to the main agent and its children, with no retry-count or
waiting-time limit, except retryable `invalid_response` replies below. The waits are 1, 2, 4, 8, 16, 32, 64, 128 and 256 s,
then 5 min, each less up to 20 % jitter. A `retry-after` header is a floor
under that backoff, even when it asks for a longer wait. A subscription's
used-up window retries by itself for up to 5 minutes on a child agent or
5 hours on the chat's own agent, the short usage window; a longer known
reset ends the turn so you can retry or switch accounts. This avoids
leaving a child waiting for hours or a chat waiting for days; without a
known reset it still stops.
Gemini subscription quota refusals use the body's retry delay or quota reset time,
or stop without a reset when the provider says `QUOTA_EXHAUSTED`. A short retry
stays a retry. The chat holds used-up limits until their reset, or for 15 minutes
without one: Gemini's applies to the request's model, OpenAI's to the whole account.
An unnamed Anthropic subscription 429 with a long retry delay does not identify a usage limit: it guards only the sent model until its retry time, unless a later request on that account and model completes successfully or a fresh, confirmed usage reading taken after the refusal shows room in every window covering that model; a renewed refusal sets the guard again. A fresh used-up reading blocks the models its window covers independently; Anthropic family windows apply only through shipped `weekly_limits`.
The refusal shows the retry time, then the model's own used-up reading and its separate reset when that account binding has confirmed one. Signing in again clears holds; other chats learn independently.
An opt-in `--retry-budget USD` still stops a possibly billed retry when
another attempt would exceed the budget. With an unknown price, the finite
fallback counts apply instead and the error says the budget could not be checked.

A `429` (`rate_limit`) or `529` (`overloaded`) refusal that couldn't have
been billed, and on OpenAI a `408` or `5xx` whose body names an overload or
a rate limit, also reports how long it has waited. The status line reads
`Retrying in 30 s (attempt 2): overloaded · waited 4 min`, or
`Retrying in 5 min (attempt 12): overloaded · waited 4 min`. The transcript
keeps one notice per step, updated in place even across discarded partial
replies; the details view keeps every attempt.

What still fails at once: authentication and permission errors, an invalid
request, a context overflow, billing (`402`, `billing_error`,
`insufficient_quota`) and anything else that can't succeed later. Anthropic's
spend-cap `429` (`error.details.error_code: "enforced_spend_limit_reached"`)
is billing too, so it fails on the first answer and its offer leads with
Switch. A gateway that drops that code and answers with its own `429` can't
be told apart from a busy one, so it keeps retrying until stopped.

Gemini's `MALFORMED_FUNCTION_CALL` and `UNEXPECTED_TOOL_CALL` finish reasons
always fail and discard the unfinished reply, even after a decoded tool call or
search, and so does a `STOP` reply with nothing in it (`docs/gemini.md`); Wisp
shows the plain cause and keeps known billed usage. `SAFETY`,
`MAX_TOKENS`, and other non-`STOP` Gemini finishes after a decoded call stop
the reply with their mapped stop reason instead of running the calls. Retryable
`invalid_response` failures retry at most 8 times per step for
`MALFORMED_FUNCTION_CALL` and 3 times per step for other causes—even under
looser explicit policies, while tighter bounds still win, sharing the step's
non-busy retry count—before stopping with `step_limit` and the Retry / Switch
offer. Raw
finish diagnostics and attempted arguments stay out. Other unreadable-reply
diagnostics keep a provider-named lead-in, never a bare parser fragment.

Native and background compaction keep finite counted bounds: 5 retries per
step (3 for non-malformed-call retryable `invalid_response`, while the tighter
5-retry bound still caps `MALFORMED_FUNCTION_CALL`), 10 total (shared with the
enclosing turn for in-turn compaction) and a 60 s `retry-after` ceiling.
Only a native compaction's busy rate limits take the 10 min busy window;
background compaction never busy-waits. A native compaction refused as
overloaded falls back to Wisp's own engine after its quick tries, whether or
not a gateway kept the `compaction_unavailable` code. Background compaction
yields to foreground compaction when its retries run out. Ordinary foreground
Wisp compaction uses the same defaults as a turn. Explicit finite policies
keep their bounds (still capped at 8 for `MALFORMED_FUNCTION_CALL` and 3 for
other retryable `invalid_response`).
Keep-warm keeps its separate 58-minute deadline.

In the main agent, Esc during a model retry wait stops automatic retries and leaves Retry available at once, with the same bytes; compaction waits still cancel compaction.
Closing the session stops any wait without a Retry offer. Retries re-send
the same bytes. On a session whose conversation tail is cached for
5 minutes, a stretch over 5 minutes without an accepted request lets that
entry expire, so the next success pays one tail rebuild (its cache miss
reads `ttl_expired`); a 1-hour cache lasts longer, but can also expire during
a long wait.

### One limit per endpoint

The agent tree shares one limit on the requests it has open on each endpoint,
`max_in_flight`. By default it is 12 on a Claude subscription, the most Wisp's
own logs held there for long with no throttle and no slowdown, and 8 on every
other account, below a gateway's usual 10, with room for a second process. Every
turn and compaction request counts from just before it is sent until its reply
stream ends, so a tool run or a retry wait never counts; background compaction
counts the same way. A plugin's `model.ask` also counts and waits at its agent's
depth, on the chat's account. Token counts don't. A request that finds the endpoint full
waits its turn: the main agent first, then its children, then theirs, and first
come, first served within a depth. A background compaction waits at its own
agent's depth, so the main agent's compaction never waits behind a deeper
agent's. The status line keeps its usual working line while the request waits,
and a child in a turn shows as running in `/agents`. A keep-warm refresh
([docs/agents.md](agents.md), "Keep-warm") waits behind every other request of
any depth, so it never delays a turn or a compaction, and it waits silently: no
status line and no state change. It gives up at 58 minutes after its anchor's
last use, logged as a `late` skip. A refresh refused as busy (a 429 or 529 with
no usage) is retried at the busy backoff above, counted from its own attempts,
while the next try still falls before those 58 minutes; its deadline stays
independent of a turn's retry policy, since the cache entry is gone at an hour.

The wait has no time limit, since it moves as requests end. It never counts
toward the reported busy wait, and the request sent after it is the same bytes it
would have been without waiting. Cancel (or `stop_agent`) leaves the queue at
once and lets the next request in. The step's `step_ended` record holds the
time it waited (`queued_ms`, [docs/session-log.md](session-log.md)).

Deeper agents can wait long only when the limit is small or `agents.max_running`
is raised: by default at most 5 agents at depth 1 or less run at once, plus
their occasional background compactions, so a limit of 8 or 12 leaves room below
them. The limit is per endpoint name: an agent that switches endpoints waits on
the new one's limit, and an endpoint added mid-session keeps 8 until the next
session, even after a switch to it. Two names pointing at one gateway get a
limit each (16 together by default), and two names on one Claude subscription 24
together, so lower both if the account's cap counts them together. Separate
`wisp` processes share nothing; together they can still pass a gateway's cap,
and the retries above cover that.

## Web search

Each (endpoint, model) pair has a web search form: `on`, where the request
carries its search tool, sorted with the other tools by name, or
`off`. It is on when `web_search` is in `--tools` (the TUI's default; the
tool is the bundled `web-search` plugin's, whose manifest asks for the
`web_search` native family, `docs/plugins.md`), the model's resolved
`web_search` list holds a version the endpoint's wire speaks, and the endpoint
doesn't set `native_web_search = false`. Anthropic's wire speaks the dated
versions (`web_search_` and eight digits). A Claude chat declares the bundled
plugin's fixed `web_search(query)` function; the plugin searches through one
uncached `model.ask` carrying the native tool and returns the findings and sources.
On the provider's own origin, the search-only model is the first ID from the
account's `webSearchModelIds` that is served and has native search, then the
provider row's `web_search_model` when served and with native search, else the
family's low ID on a level-in-ID chat when served, or the chat's model. Only the ask sends the native tool;
no server-added date enters the chat's prefix. A toolbox without the plugin's
runner keeps the in-reply native form. Gemini's speaks
`google_search` and declares `web_search(query)` as a function, answered by the
same plugin through `model.ask`, whose search-only request carries `{"googleSearch":{}}`; OpenAI's speaks the other
undated ones (`web_search`) and sends `{"type":"web_search"}` and nothing more (no location,
filters or context size), with `"web_search_call.action.sources"` added to
`include`. The first listed version the wire speaks is sent, so a model
reached over another provider's wire has no
search unless a version that wire speaks is configured. That holds on every origin: a gateway gets the
tool by default, as the provider's own origin does. The list is the first of
these that names a version the wire speaks, or is `none`: the account-and-model
flag, the model flag, the account-and-model table, the model's table, the
account's own model list, the shipped row. A list that names only the other
wire's versions is passed over, so OpenAI's name alone saved for a Claude model
leaves the shipped list in force; when no place names a version the wire speaks,
the first list stands and the pair has no search. Nothing is decided
from a model's or a host's name. The facts are read at start, so a pair keeps
its form, and its `tools` bytes, for the session.

- **One switch.** The `/config` row `Web search` is the only switch on the
  screen: it turns the `web-search` plugin on or off (`[plugins.web-search]
  enabled`, as the Plugins rows do), at the next turn, with one cache rebuild,
  and the Tools box lists the tool without a tick. Where a saved Tools list
  leaves the tool out, Enter puts it back in that list in the same save. The
  account's `native_web_search = false` and a model's own `web_search` list
  keep their effect when saved; the row then reads `off for this account` or
  `not on this model`, says which saved value does it, and `d` removes it for
  new chats. A resumed session that still sends search while the switch is
  saved off reads `on · this chat`. The reasons the row, the start banner, the
  line a switch of model or account prints and `wisp doctor` give are one
  list: `not on this model`, `not on this account` (the provider or a
  gateway refused it) and `off for this account`.

- **A resumed session keeps the tools it logged.** A Claude chat logged with the
  server tool moves to the fixed function at its next opening with one rebuild.
  For every other case, a session that had search keeps it after
  `native_web_search = false` (from `settings_changed.tools`, a rebuild's
  `tools_changed` copy, else `session_started.tools`, adjusted by inline
  `tools_changed`), and a session where search was switched off inside the chat
  resumes with it `off` (and toggleable back on). A session logged without
  search and with no in-chat switch-off picks search up by itself when today's
  settings and model have it (and the account hasn't refused it): an interactive
  or child session adopts the logged prefix, then adds `web_search` on the first
  turn after that resume — inline, keeping the cache, on a direct Claude API
  pair with `inline_tools` using the client function, or with one cache
  rebuild everywhere else — and never again. Only the pair the log ran on keeps
  its logged form: a resume onto another endpoint is a resume followed by a
  switch, and that endpoint takes today's form. Whether `web_search` is on at
  all follows this run's tools: a log with search resumed by a run started
  without it adopts the logged prefix, then turns search off at its first turn
  (inline on a direct Claude API pair with `inline_tools` using the client
  function, else with one rebuild; `docs/tools.md`, "Switching tools in a running session").
  Every new session takes today's settings.
- **A switch to a pair of the other form** changes `tools`: the request
  writes the cache again, keeps all earlier thinking for the API to drop (a
  thinking block's checked prefix includes the tools) where binding
  controls are usable, else leaves it out, and the kernel emits `ToolsChanged`
  (`via: rebuild`) when the switch applies. The session log's
  `settings_changed` record carries the new `tools` and `tools_sha256`.
- **OpenAI's search items.** Each search action OpenAI ran comes as its own
  `web_search_call` item and becomes a row when it is done: `search`,
  `open_page` (Opened, with the URL) or `find_in_page` (Searched page for).
  Its `url_citation` annotations become the reply's marks and Sources. A
  request carrying the tool may get `response.completed` with an empty
  `output`; it is accepted only when every announced item was finalized and
  none is still open. The search count is the response's
  `tool_usage.web_search.num_requests`, else the completed `search` actions.
  A reply holding a search item is sent back verbatim only when the request
  carries the tool and the connection is the ChatGPT subscription, where
  that replay was checked live. Otherwise that reply goes as text: each
  search item as one `[web search: "subject"]` line, its text and calls as
  usual, its reasoning left out. On API keys that drops the reasoning of
  every reply that searched.
- **The organisation refusal.** An administrator can switch web search off for
  an organisation; then a request carrying the tool fails with a 400
  `invalid_request_error` saying web search is not enabled. Wisp matches it
  narrowly (before any stream event, the message names web search or
  `web_search`, not just `web_search_call`, and says it isn't enabled or isn't
  supported, the request carried the tool; the same in a JSON error body or a
  gateway's 200 chunk; on OpenAI, the error object's own message, from a 400
  or 422 or a failed stream), re-sends the step once without the tool
  (not a retry), and sends no web search tool to any model on that endpoint
  for the rest of the session. It writes a `native_web_search` record and
  emits `FeatureStateChanged`, an unasked `SettingsChanged` with
  `Rebuilt { native_search_refused }` carrying your next turn's settings, and
  `ToolsChanged { removed: ["web_search"] }`. A search still open in the
  tail is closed as `Unfinished { Refused }`; after a pause there is nothing
  left to continue, so the turn ends as at the continuation cap. Any other
  error ends the turn as before, never dropping search silently.
  A refused search-only ask on a different side model is tried once on the
  chat's one-shot model (a level-in-ID chat's low ID when served, else the chat's
  model), with that model's own search version, for Claude and Gemini.
  Only a final refusal switches search off. Gemini marks a search-only ask's
  4xx other than 401 as refused, including 429; a 401 stays an authentication
  error. Other failures return `web search failed: <kind>` and the turn continues.

- **A gateway refusing the tool.** A gateway may not pass the provider's
  search tool on. OpenAI's adapter takes the same path for a 400 or 422 it
  reads as an invalid request. When a request on an endpoint whose origin isn't the
  provider's own carried the tool and no search history sent as text, and it
  fails before any stream event with a 400 or 422 the adapter reads as an
  invalid request (whatever the body's shape), or a 200 JSON error object of
  type `invalid_request_error`, and nothing else matched it (not an overflow,
  not a refused feature, not the organisation or cross-account refusals),
  Wisp suspects the tool. It re-sends the step once without it (not a retry),
  provided no server call is open in the tail, and records nothing yet. If
  that request's reply starts, the tool was the cause and the organisation
  refusal's path above follows unchanged: the same record, events and
  rebuild, with the note `gw refused the web search tool; continuing without
  web search. To stop sending it: wisp config set
  endpoints.gw.native_web_search off`. A resend that meets a rate limit, an
  overload or a network failure is retried under the usual retry rules, still
  marking nothing; each retry re-sends exactly the resend, without the tool
  and with any withdrawn attachment still out. If the resend fails, nothing
  is recorded, search stays on, and the log holds what a plain failure would
  (no `rebuild_marked`). Which error the turn ends with depends on the
  resend's: when it is also an invalid request, the turn ends with the first
  error, since the resend told Wisp nothing new; otherwise (an overload whose
  retries ran out, a network failure) it ends with the resend's own error,
  with its kind, detail, attempts and Retry offer. An overflow on the resend
  is a real overflow: it gets the same recovery as any overflow
  (`docs/context.md`), with the probe still open, so the compaction request
  and the step sent after it carry no search tool either. If that step then
  succeeds the refusal is recorded as above; if it fails, search stays on and
  the compaction stays, exactly as after a plain overflow. When the recovery
  can't compact, the turn ends with the recovery's own message. This
  happens at most once per endpoint per session. A gateway's error chunk in
  its own format is never suspected. When the step adds an attachment and the
  resend is refused for it, the probe ends: the attachment is withdrawn as
  usual and the next request carries the tool again, so a refused image never
  drops search; if that request is suspected too, it is probed again with the attachment still withdrawn, and if that fails too the turn ends as any failed resend does. When no
  resend can be made (a server call still open in the tail), the attachment
  withdrawal still runs. A mid-session resend also changes the tools under
  earlier thinking: kept for the API to drop where binding controls are
  usable, else left out (the tools-change cut), so there a 400 that a
  thinking block caused can pass on the resend and be taken for the tool's
  refusal; search is then off
  for that endpoint for the session, and the note's command turns it off for
  good if that is wanted. A compaction or background request meeting such an
  error treats it as any other error.
  - **After search history went as text.** A request carrying text-rendered
    search history is never suspected at once: its invalid request first
    climbs the rendering forms ("Web search", above). When the last form is
    refused too, the step carried the tool, no probe ran in it, and the error
    was a plain invalid request (not a cross-account refusal, whose wording
    names the history), Wisp starts the same resend without the tool, with
    the ladder's error as the one the turn ends with if it fails. The
    resend goes once more in the kept form, without climbing the forms
    again: at most 4 unbilled 400s before the turn fails.
  - **A Retry after a compaction refused for search.** When a turn has
    failed and a compaction before your Retry meets the refusal (and then
    fails or is cancelled, so the error choice still stands), a search the
    failed request left open stays open: the compaction's own request sends
    it as closed, but nothing is recorded. A Retry on the same endpoint then
    repeats the failed request with its tools, web search and the open
    search included, so the bytes match. That holds for the Retry's first step
    only, with its in-step retries: once that step commits a reply, the
    next request drops the tool, keeping earlier thinking where binding
    controls are usable, else leaving it out (the tools-change cut, with its
    `thinking_cut` record); a search the reply
    left open is first closed as `Unfinished { Refused }`, and after a pause
    the turn ends as at the continuation cap. If a request in that step is
    refused again, it is re-sent without the tool at once, with nothing new
    reported. No `settings_changed` is written for those tools; the retry
    turn's `turn_started` carries `kept_tools_sha256`. A Retry on another
    endpoint, or one after a resume, sends the tools in use now.
- **Across accounts.** If a request on an endpoint that hasn't rendered them
  yet is refused with a 400 naming `encrypted_content` or `encrypted_index`
  while history holds web search blocks produced on another endpoint, those
  blocks are sent to it as text from then on (`[web search: "q"]` and a
  numbered list of hits), the thinking after the first of them is kept where
  binding controls are usable and left out otherwise, a
  `search_history_rendered` record is written, and the step is re-sent once.
  Its own searches are sent unchanged. Searches made on the other endpoint
  later (A, B, A, B) are sent to it as text too, with no second refusal, and
  where binding controls aren't usable the first request carrying them
  leaves out the thinking from the first of them on (a `thinking_cut`
  record). If nothing foreign is left to render but
  the request already carried rendered text, the refusal is treated as a
  refused rendering and re-sent in the next plainer form, as above.
- **Unreadable search results.** The same refusal can come with nothing
  foreign in history: the account behind the endpoint changed, or old
  results went stale. If the request still sends a web search block
  natively (a call, a result or cited text), every such block in the
  messages committed so far is sent as the same text from then on, a
  `search_history_unreadable` record is written, the thinking after the
  first of them is kept where binding controls are usable and left out
  otherwise, and the step is re-sent once. A live paused or open tail is
  closed first, as when search is refused. Searches made after the record
  are sent unchanged, so the model keeps their page content; if they are
  refused later (back on the first account), a second record converts them
  too. Converted blocks never go back on that endpoint. Both compaction engines take the
  same step once when their request is refused this way; background
  compaction doesn't, and the next request converts. Nothing in history
  changes: the encrypted blocks stay committed and logged. A refusal with
  nothing left to convert goes down the rendering forms above, then ends
  the turn with the error of the first refused re-send.
- **Refused thinking.** An endpoint can also refuse earlier thinking: a
  signature it can't verify (``Invalid `signature` in `thinking` block``,
  with or without the prefix check's sentence after it) or redacted thinking
  it can't read (``Invalid `data` in `redacted_thinking` block``). After the
  rendering forms, when the request carried rendered text, every thinking
  block in history is left out from then on and the step is re-sent once
  (`docs/context.md`, "Broken thinking"): one `thinking_cut` record, one
  cache rebuild. A second refusal ends the turn with the thinking-binding
  message, which ends with the refusal's own text. Known limit: a gateway that replays cached thinking into tool
  turns that lack it (mnfst/llm-gateway does, for 30 minutes) can put the
  cut thinking back; on an account that enforces the prefix check, with no
  usable binding controls, that re-send is refused again and the turn ends
  with the message, and once the gateway's copy expires the next turn
  recovers.
- **Completed searches after a switch to a pair without the tool** are sent
  unchanged: the live check on 2026-10-25 (Haiku 5.5, low effort) sent a
  history with a completed search, no server tool and no thinking, and the API
  accepted it. Only the cross-account refusal, an unreadable result above and
  a Claude chat using the fixed function render them as text.
- **A search that didn't run.** An endpoint that routes a request to a model
  without native search may pass the tool on as a plain one, and that model
  may then call `web_search` as a client tool. When the step's request carried
  the search tool and the reply calls a client tool of that name, Wisp answers
  the call with an error result, `Invalid input: web search didn't run for
  this request; answer without it`, and the turn goes on. The tool stays in the request, so
  the cache isn't rebuilt. The first time on each endpoint in a session, the
  transcript says `web search didn't run: the server passed the search tool
  to a model that can't use it (it may have routed this request to another
  model)` (`AgentEvent::SearchNotRun`), and the log gets a
  `web_search_not_run` record. Resume doesn't read that record, so after a
  resume the notice may show once more. A call of that name on a step whose
  request didn't carry the search tool keeps the usual `there is no tool
  named` answer. The call and its result are sent as they are on later
  requests: the live check on 2026-10-26 (Haiku 5.5, low effort) sent a
  history holding a client `web_search` call and its result beside the server
  tool, and the API accepted it.
- **In the TUI.** `wisp` resolves every pair's form at start, with the same
  rules, for each endpoint and every model it can name (the priced models and
  every ID or alias in the model file and `config.toml`); any other model has
  no native search. A refusal then moves every model on its endpoint to
  `none`. With `web_search` left out of `--tools`, the TUI shows nothing about
  search.
  - **Pickers.** A `/model` or `/account` row whose form differs from the
    pair in use says so beside its cost, in short: `web search: on → off`
    or `web search: off → on`. When the session holds thinking such a
    switch would cut, one note under the rows says
    `switching web search on or off drops earlier reasoning`. A row that stays
    without search says why: `web search: off (not on this model)` (no
    data for the pair), `web search: off (off for this account)` or
    `web search: off (not on this account)` (the organisation's or a
    gateway's refusal); the row of the pair in use is no exception. When every row
    that stays without search would say the same, the picker says it once
    under the rows instead. Only thinking from a reply
    that was committed counts: a reply stopped or failed while thinking leaves
    none to drop. When a row is too wide for the terminal, the credential or
    price column is cut short (ending in `…`) first, and dropped only when not
    even its title fits.
  - **A switch that waits.** The note for a `/model` or `/account` switch,
    drawn at once or at the end of the running turn, ends with the full form
    when the switch changes it, for example `web search: on → off
    (off for this account)`, followed by
    `earlier reasoning dropped` when the session holds thinking the switch
    cuts.
  - **The welcome panel** adds the short row `web search off · ctrl-o for why`
    when the pair in use has no search (`web search off · chat only · ctrl-o
    for why` when that leaves no tools at all); transcript mode (ctrl-o, twice once anything can be opened) opens with
    the panel's full row:
    `web search: off` with one of the reasons above, with `· chat only`
    when web search was the only tool asked for (`--tools web_search`). The
    row follows the pair in use, so a switch or a refusal updates it.

The live test (`crates/wisp-cli/tests/it/live_web_search.rs`, ignored by default)
recorded on 2026-10-25, on Haiku 5.5 at low effort against the provider's own
origin: the search completed (one search, $0.0118 for the turn); the follow-up
turn with the native blocks in history was accepted; Plan A held (above); and
a reply with two searches on a request carrying three cache markers (two 1h
system blocks and the conversation tail) was answered, not refused, so the
API's own breakpoints after search results don't count toward the limit and
Wisp has no breakpoint fallback. Its `live_native_web_search_on_openai` runs
two turns with `OPENAI_API_KEY` and is skipped without it; it has not been
run yet. OpenAI's search on the ChatGPT subscription was checked live on
2026-11-02 (`docs/openai.md`).

## What you see

- The welcome panel's last row shows the working directory and the
  endpoint: its name when there is a choice, the credential kind and the host,
  for example `~/project · manifest · Gateway · gw.example.com`. When an
  exported `ANTHROPIC_*` variable wins over `default`, the panel in transcript
  mode adds the note
  `Using ANTHROPIC_API_KEY from the environment, not the default account "manifest" (--account manifest to use it)`.
  Where a credential comes from (the environment or the env file's path) is
  shown by `wisp doctor`, not at startup. When the pair in use has no web
  search, a short row under it says so, and transcript mode says why
  ([Web search](#web-search), "In the TUI").
- The status row and welcome panel name the endpoint when there is a choice:
  `manifest · Gateway · gw.example.com`, narrowing to `manifest`. With
  only the keys-file endpoint, the label is unchanged. The kind reads `Claude subscription`,
  `ChatGPT subscription`, `Antigravity subscription`, `Claude API`, `OpenAI API`,
  `Gemini API` or `Gateway`; only a gateway shows its host.
- Each account's name is led by `●` (ASCII `*`) in its colour, one of six, the
  same on every screen and in every window; a removed account's keeps a muted
  `○`, and piped output prints no mark. Accounts without a `mark` take, in the
  order of `config.toml`'s tables and the keys-file account last, their name's
  own colour or the next free one; a seventh shares. A sign-in adds its table
  last and moves only the keys-file account's colour, if any; a sign-out
  removes the table, its `mark` with it, and a sign-out then a sign-in again
  can swap two colours. Write `mark` to pin one.
- The session log's `session_started` records `endpoint_name`.
- When Wisp changes how it sends history to fit the backend, the transcript
  says so once, in a muted notice (`AgentEvent::HistoryAdapted`, never
  logged and not replayed on resume). `<endpoint>` is the endpoint's name,
  or "the endpoint" when the only endpoint is the keys-file one:
  - after [unreadable or foreign search results](#web-search) are converted:
    "Earlier web search results can't be read on <endpoint>; they're sent as
    titles and links from now on (the cache is rebuilt once).", adding "The
    thinking after them is left out." when that thinking was cut;
  - after [refused thinking](#web-search) is cut: "<endpoint> refused earlier
    thinking; it's left out from now on (the cache is rebuilt once)." A
    refusal the rendering forms recover shows none;
  - when the API's list of dropped thinking grows at blocks Wisp sent:
    "The API left out N earlier thinking blocks that no longer match the
    conversation; the model no longer sees that reasoning.", with "came from
    another model" or "came from another account" instead of "no longer match
    the conversation" when every new drop has that reason
    (`model_binding_mismatch`, `organization_binding_mismatch`). Drops at
    blocks Wisp didn't send are a gateway's and get the injected-thinking
    notice instead (`docs/context.md`, "Cache misses"), and the request right
    after a search conversion that kept its thinking shows none, since that
    notice covers its drops. A resume, a compaction and a rewind to a masked
    point keep no earlier drop list, so the notice may show once more after
    them.

## `wisp doctor`

The `endpoints` section shows the files, which endpoint Wisp would start on and
why, and for each endpoint which variable goes to which host
(`MANIFEST_API_KEY → gw.example.com`), or for a stored endpoint where its key
is (`key in credentials.json`, or `key and URL in credentials.json`), without
any secret. A `credentials` row counts this config's stored keys. It warns
about a config, env or credentials file readable by other users, a
`credentials.json` or `.wisp.lock` inside a git work tree, plain `http://` to a
non-loopback host, a literal `url` with user info or a query, an exported
`ANTHROPIC_*` variable overriding `default`, an unavailable `default`, a record
this config shares with or got from another config file, a record for an
endpoint that `config.toml` names but reads its key from a variable, and
records in `credentials.json` that no config here uses. Each of those lines
says what to do: a record from another config file, or written by hand, says
that this config's next change through `wisp account` gives it its own copy;
records no config uses say that `wisp account remove` can't reach them, so
delete them by hand or leave them. A linked default `config.toml` shows its
target in the `config` row. A `credentials.json`
inside a git work tree reached through a link says so. With `--config`, the
`credentials` row and the credentials and lock warnings show no path derived
from it (`credentials.json beside the file given with --config`). Pass the same
flags as a session (`wisp --account direct --env-file .env doctor`) to see what
that command line would do.

On a machine with a keychain, doctor builds nothing from a variable either,
and `--check` never probes one. It adds an `environment` row naming the
provider's own variables (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
`ANTHROPIC_BASE_URL`) that are set but not read (`ANTHROPIC_API_KEY is set,
not read: this machine has a keychain`), even when `config.toml` doesn't
parse; a `key_env` table's variable shows in that endpoint's row instead. It warns
when `~/.config/wisp/.env` holds keys it doesn't read there (or doesn't
parse, which is only a warning), shows an `--env-file` as `not read here`,
words the literal-`url` warning as `store the key and URL with wisp account
import`, and adds `a keychain is available: wisp account edit NAME --store
keyring moves it` to each endpoint kept in `credentials.json`. A broken `config.toml` makes doctor exit non-zero.
The `web search` section shows each endpoint's search form for the session's
model (`on`, or `off` with the reason: not on this model or off for this
account; `off` alone when the tool is left out), and where the model's
`web_search` list and the endpoint's `native_web_search` came from, in the
settings' source words (`shipped`, `config`, `account`, `default`). A refusal
by the organisation or a gateway is known only inside a session, so doctor
can't show it. It warns `the keys-file account in config.toml is unused: ~/.config/wisp/.env
and the ANTHROPIC_* variables aren't read on a machine with a keychain; wisp
account import --from-environment stores them`, or `the keys-file account in
config.toml is unused: the ANTHROPIC_* key is gw's, whose settings apply` when the keys-file endpoint is
hidden as a twin.
Without `--check`, doctor makes no network request.

### `wisp doctor --check`

`wisp doctor --check` checks available endpoints, or only the ones named
(`wisp doctor --check direct manifest`). Anthropic API-key and ordinary bearer
endpoints receive a `count_tokens` call with the `--model` value (or the shipped
Claude default model on Anthropic's own URL when `--model` belongs to another
provider) and message `ping`, sending the auth header, `anthropic-version`,
`content-type` and `accept: application/json`. Token counting is free on
Anthropic's side; a gateway may count it. OpenAI API endpoints receive
`GET /v1/models`, with their bearer API key, and must return a JSON `data`
array; Gemini API endpoints receive `GET /v1beta/models?pageSize=1` and must
return a JSON `models` array. Antigravity subscription endpoints receive
`POST /v1internal:fetchAvailableModels` with `{}` and must return a JSON
`models` object. This checks the connection without an inference request.

Claude and ChatGPT subscription modes report the check as not offered without sending a
provider request. The existing result wording mentions `count_tokens` and
404 even for this local skip; it is inconclusive, not an authentication
failure or an observed HTTP status. Use `wisp status NAME` for the sign-in's
state and limits and a deliberate model request to test live access. Checks
never run without `--check`; redirects are not followed and requests time out
after 10 seconds.

Each endpoint gets one Wisp-written result; a server's own text is never shown,
since it could hold a secret:

| Result | Meaning |
|---|---|
| `ok` | HTTP 200 with an Anthropic `input_tokens` count, an OpenAI `data` array, a Gemini API `models` array or an Antigravity `models` object |
| `key refused (401)` | The key or token was rejected |
| `forbidden (403)` | The endpoint refused permission for this check |
| `count_tokens not offered here (404): couldn't check this account` | The check is unavailable (common on gateways), or skipped locally for subscriptions; inconclusive |
| `unexpected answer (HTTP <status>)` | Anything else, with a reason: `the body is not JSON`, `no input_tokens count in the body`, `the body reports an error` (a gateway's HTTP 200 error), `a redirect, not followed`, `the body is too large` (over 64 KiB) or `the body could not be read` |
| `unreachable: timed out`, `could not connect`, `the request failed` | No answer |

An unavailable endpoint reads `not checked: <reason>`, and a hidden duplicate
keys-file endpoint reads `same as <twin>`; neither fails the run. A 404 is inconclusive and
doesn't fail the run either. Doctor exits non-zero when a checked endpoint
answers anything else that isn't `ok` (401, 403, another status, unreachable), or when an endpoint named on the command line is
unknown or unavailable. Naming a hidden keys-file endpoint (or `env`) checks its twin
(`same as manifest: ok`). With a broken config or env file, the check is
skipped (`skipped: the config or ~/.config/wisp/.env has errors`) and doctor exits
non-zero.

## Trust

`config.toml` is trusted as far as the env file: whoever can write it can send
any of your variables to any host, since an endpoint may pair any variable with
any URL. Copy a config from someone else only after reading it, and check
`wisp doctor`'s `<VAR> → <host>` lines.

## Resuming

`wisp --resume` continues a session on the endpoint it last used, or on the
one `--account` names. If that endpoint is gone or can't be used, one rule
decides where the chat goes (`endpoints::goes::chat_goes`): it continues by
itself, with one line, on the endpoint new chats start on when that is the
same provider and doesn't start charging, or else on the only same-provider
endpoint that doesn't. An endpoint whose provider can't be told from its
entry, its subscription or the logged model has no same provider, so the rule
asks. A move that doesn't start charging is API to
subscription, subscription to subscription or API to API. The line reads
`wisp: session 0f3a1b2c-5e2a ran on claude-work; continuing on
claude-subscription.`, and a session that ran on `env` says `ran on the keys-file
account` instead. The endpoint being left never counts as the
one new chats start on.

When the rule would ask (several fit and none is where new chats start, no
account of the same provider is left, or the only way on moves the chat from
a subscription to the API), a full-screen start opens on "Where should this
chat continue?" before the chat starts, titled with the account it ran on. Each row
is a usable account but the one being left, with its kind, model and effort,
the cache rebuild's estimate, and what changes: earlier reasoning that won't
carry over, a move from the subscription to the API, and the chat's agents that
move with it or stay behind. On this card and every other "Where" card, the
kind shows only from 60 columns and when every row's kind and model fit;
otherwise each row shows its billing word, such as `API`. A line every row
shares is said once, under the question. No row whose move starts charging is
chosen when the card opens: a digit or a click only chooses such a row, and
Enter then takes it.
Enter on any other row continues there at once. Esc quits with one line, or,
when the card opened from the resume picker, goes back to the list. The chosen
row's agents follow the chat (`docs/agents.md`, "Resume"); one whose move would
start charging follows only when its row named it. After the card, the chat's
first line says where it continued and what the move cost.

When the chat's own account is still set up and only its sign-in expired, the
card is titled `NAME's sign-in expired`, `This chat ran on it.`, and `Sign in
again to NAME` comes first: it opens `/login`'s card, and the chat continues
there once signed in. Scripts and `--account` keep continuing on that account.

Without a terminal, or with `--output jsonl`, one line names the choices and
`--account`. The same account still there but billing a subscription session to
the API now is refused with a rerun line.

A plain `wisp` whose start account can't be used asks the same way, with
"Where should this chat start?": a signed-out or expired one is titled
`NAME is signed out` or `NAME's sign-in expired`, with `Sign in again to NAME`
first. One that is gone moves where new chats start, as a sign-out does; one set
up but unusable (an unset variable, a missing or unreadable key) starts this
chat elsewhere with a line saying what's wrong, and new chats still start on it.
A keys-file account whose key left the file is named with the file's path:
`claude-api's key is missing from ~/.config/wisp/.env, so new chats now start
on NAME.`
Either asks first only when the move goes from a subscription to the API.
When no other account can be used, a full-screen start opens on the sign-in
card of a first start, with that line under its title (`claude-api's key is
missing from ~/.config/wisp/.env.`) and no keychain offer; a script, or a start
with `--config` or `--env-file`, still gets the start's error.

The resumed session records the move at its first turn as a live switch does:
a `settings_changed` with an `endpoint_switch` rebuild, naming the
account-bound models whose earlier thinking may be dropped, then one
`rebuild_marked`. Idle agents follow by the rule in `docs/agents.md`
("Resume"). If the endpoint now points to a different host, it continues
there and the notice names both hosts (`docs/session-log.md`, "Stability"). A name and a host
can't show that the key or account behind them changed; a changed account means
a cold cache, and possibly lost Sonnet 5.5, Haiku 5.5 and Fable 5.2 reasoning, with no
notice.

An `--account` on the resume command starts the run on that endpoint and
replaces a switch the log still had waiting. When it differs from where the
session last ran, the resume notice names it once, by its config name only,
as a cause of the one cache rebuild ("Your flags change the account to gw:
the cache is rebuilt (~$0.41)."); when it equals the endpoint the next turn
would use anyway, it changes nothing and the notice doesn't name it. The
same holds for the resume command's other flags and `--set` values; config,
environment and saved defaults never move a resumed session
(`docs/session-log.md`, "Flags on resume"). A notice that names a model
shows it through the shared display rule, so a key-shaped model ID is never
echoed, including when the session's endpoint is gone.

`wisp --resume` without an ID opens the newest session in this directory and
never refuses over a tool difference: the session resumes with this run's tools
(the config default or `--tools`). A difference costs one cache miss, priced in
the resume notice, which names the tools added and removed; earlier thinking
stays under binding controls where the pair supports them (`docs/context.md`,
"Broken thinking"). Child agents keep their logged tools for life.

## Examples

A direct key, with the key in `~/.config/wisp/.env` as
`WISP_DIRECT_KEY=sk-ant-…`:

```toml
[endpoints.direct]
auth = "api_key"
key_env = "WISP_DIRECT_KEY"
```

A gateway, with `MANIFEST_API_KEY` and `MANIFEST_BASE_URL` in the env file:

```toml
default = "manifest"

[endpoints.manifest]
auth = "bearer"
key_env = "MANIFEST_API_KEY"
url_env = "MANIFEST_BASE_URL"
```

Then `wisp` starts on `manifest`, and `wisp --account direct` on your key.

For a stored OpenAI API key, use the hidden prompt and then select its endpoint
and GPT model explicitly:

```sh
wisp account add openai-api --provider openai --auth api_key
wisp doctor --check openai-api
wisp --account openai-api --model gpt-6.1-sol --effort low
```

For ChatGPT subscription testing in Wisp, use the browser login commands under
[Subscription login and status](#subscription-login-and-status). Keep the API
and subscription endpoints separately named so their credential and billing
provenance remain explicit.

### Live subscription authentication

An open session shares one revisioned endpoint catalog with its children. A
successful subscription login is immediately selectable with `/account`, an
explicit spawn endpoint, or a preset loaded at startup that names the alias.
Definitions and instructions are not reloaded from disk. Known model-only
presets also remain available when their provider has not been configured yet.
Structural, unknown-model, tool and instruction errors remain errors; endpoint
availability and compatibility are checked against the current catalog when
spawning. Tool schemas and system prompts are not regenerated.

Queued inputs, pending retries and waiting mail do not block reauthorization:
they select their connection when delivered and use the newly published
generation. Only admitted or in-flight work on the affected connection blocks
replacement, including running turns/tools, spawn construction, background
compaction, keep-warm and allowance refresh; cancel that work first. Mutation
never interrupts it or waits for it to drain. Root active and accepted pending
endpoint selections hold binding guards, so logout cannot race a switch into
the alias being removed. A switch into a changing or retired connection is
refused before acceptance. Logout moves the chat and its idle agents off the
account first, then signs out. It refuses while the chat is answering on the
account or an agent on it is working; anything still bound or admitted when the
account goes refuses with the in-use line, deletes nothing, and keeps the moves
already made.

Children receive a read-only catalog handle, not the authentication host. An
idle child adopts reauthorization at its next safe boundary, and queued
input/mail can then run through the replacement provider. Catalog publication
retires the replaced connection and invalidates its allowance generation. Old
cloned connections cannot send requests, count tokens or fetch allowance data.
Unrelated aliases keep their existing connections. A reconnect failure after
saving is reported as “saved, not usable yet”, not as successful activation.
Retry loading that saved alias; a second browser login is unnecessary.
Authentication failures do not silently select another endpoint or API key.
Empty configuration and restart without a usable endpoint still require `wisp
login` outside chat; this does not add an unauthenticated startup UI.

Client protocol contract for the subscription wizard:

- Send `ClientCommand::SubscriptionAuth { command }`; responses arrive as
  `AgentEvent::SubscriptionAuth { event }`. Each `AuthCommand` and `AuthEvent`
  carries its request ID. These local operations are not model turns.
- `Start { request, provider, alias, reauthorize }` uses a registered
  provider. Omit the alias to choose its default name or the first free
  numbered suffix. An existing alias requires explicit reauthorization;
  another provider's alias is refused. Storage is selected automatically and
  the default is not changed.
- `Challenge` contains a `PrivateChallenge`: the Start request ID as `login`,
  provider, alias, browser/manual challenge kind, a protected authorization
  URL, and remaining lifetime in seconds. Present it only in the private auth
  card. For browser loopback, send `Finish` with no response; manual
  completion supplies a protected `Secret`. Finish and Cancel refer to the
  original `login` ID and have their own response request IDs. Stale and
  duplicate completions are refused.
- `Cancel` drops pending state/listeners. Timeout and session shutdown also
  release pending challenges. Once persistence has started, wait for the save
  outcome rather than pretending cancellation rolled it back.
- `Ready` and `SavedNotReady` contain safe endpoint/storage/local/default
  state and a previous-profile-cleanup flag. `Reload { request, alias }`
  retries only loading: it returns Ready/SavedNotReady/Failed and never starts
  OAuth. Reload of an already-ready alias is non-mutating.
- `Status { request, alias }` reads local configuration/profile validity
  without an account lookup; absent alias means all configured subscriptions.
  Local validity is not proof of remote acceptance.
- `Logout { request, alias, also, chat, confirmed, follow, gone }` signs out of
  `alias` and every name in `also`, always named; a client expands `all` into
  the names its question showed. An unconfirmed command answers
  `LogoutNeeds { deletes_keys_of, new_start, ways }` when it would delete a
  stored key, names more than one account, or leaves the chat a choice; nothing
  changes until the client resends it with `confirmed: true`. Each `Way` names
  where the chat would go (`chat`), the agents whose move the question shows
  (`follow`) and the agents that stay behind (`stay`). `follow` has one
  meaning, in the command and in a way: the agents the question showed moving.
  A move from a subscription to the API takes an agent only when `follow` names
  it, so resend `chat` and `follow` from the way the user chose; an agent the
  question didn't show stays, unbound, and reads `charges` in the result. A
  confirmed command with no `chat` while `ways` is not empty gets `LogoutNeeds`
  again, so the client asks where the chat goes. With nothing left, `chat` is
  absent and the command signs out of the last account.
- `LoggedOut` returns `alias`, `also`, `new_start`, the agents that moved
  (`moved`, with model, effort and rebuild estimate) and stayed (`stay`), the
  accounts whose cleanup didn't finish (`left_sign_ins`, `left_keys`, also
  summarised by `cleanup_pending`), and `stopped` when a failed write ended a
  list early. `remote_revoked` reports remote revocation separately.
  `Failed` carries a typed category and safe recovery wording.
- `AccountsGone { names, moved, stay, ways }` says that the accounts in
  `names` were signed out in another window, and where things are now: the
  agents that moved (`moved`), those that stayed (`stay`, with `busy` for one
  that was working; it moves once its turn ends and then sends its own
  `AccountsGone` naming only itself in `moved`) and, when the chat must
  choose, its `ways` (each with `charges` when that way starts charging). It
  is state, not a count: read each event as "these accounts are gone; these
  are the ways now", keyed by `names`, so a later event for the same `names`
  replaces the ways and never repeats a removal. While the chat must choose, a
  send is refused with the `kind` `account_gone`, just after an
  `AccountsGone` with the current `ways`; answer it with `Logout` naming those
  accounts, `gone: true` and the chosen way's `chat` and `follow`. A `gone`
  answer deletes nothing: it moves the chat and the agents, answers
  `LoggedOut`, and releases a queued message the refusal held. A name that
  came back in the meantime is dropped from it, and when the chat's own
  account came back nothing moves. Moving the chat to another account, as
  with `/account`, also ends the choice and releases what it held.

- `LeaveNeeds { request, ways }` comes just before the refusal of a
  `ChangeSettings` that turns off or removes the plugin of the provider this
  chat is on, when another account can be used. Its `ways` are where the chat
  can go, as in `LogoutNeeds`, each with the agents that stay behind and
  `charges`. The refusal still ends the request. To go ahead, send one unsaved
  `ChangeSettings` with the chosen way's `chat` as the account and the same
  switch: Wisp judges the two together, so the chat moves and the provider
  goes off, or nothing changes. To keep the switch for later chats, send it
  again with `save: true` once that change is answered.

### Accounts changed in another window

Each time an agent syncs its accounts (before a turn, before each command, and
when keep-warm is due), Wisp compares the modification stamps of `config.toml`, the credentials store and the env file
with the last ones it read. When they changed it reads them again, under the
config folder's lock, and compares a fingerprint of each account: its
provider, key variable, pricing, URL and key, or its sign-in profile. A token
refresh rewrites only the profile, so it is never a change. A file it can't
read or parse, or one being written, changes nothing: the last good view stays
and it reads again next time. An edited account or a sign-in again reconnects
with its one rebuild mark; a new one appears. A removed one is closed at once:
no new request or step starts on its key, and a request already in flight
finishes. The chat moves by itself when one account fits, as after `/logout`,
and otherwise asks where to go on its next send. A message sent while a turn
on the removed account still runs, a steer included, waits as a queued message;
when that turn ends it goes with the chat, or is held with the question. A
switch to another account, by `/account` or after a sign-in, runs the held
messages there, in order. Changes this window made are never reported as made elsewhere. `AccountsGone` is not written to the session
log: each move logs as that agent's `settings_changed`, and a refused send as
`rejected` with `account_gone`; JSONL output prints it.
- Auth commands, secret responses and challenges are excluded from durable
  logs, replay, model history and transcript rendering. Private challenge
  events are excluded from CLI JSONL output, including nested child events.
  Their serialization exists only for private client transport: never write
  whole auth commands or private challenge events to trace files. Tokens and
  account data are never protocol outcomes. No session-log schema change is
  introduced.

The provider implementation list is composed once by the CLI. ChatGPT, Claude
and Antigravity are always registered through the same contract, independently of `claude_usage`.
All three support in-chat sign-in and config-first endpoint deletion.

The terminal exposes this contract through `/login [claude|chatgpt|antigravity] [NAME]`,
`/login status [NAME]` and `/logout [NAME|all]` (see [commands](commands.md#subscription-sign-in)).
All three subscription implementations are always registered. The private card alone
holds the authorization URL and manual code#state; it never opens an editor,
exports a transcript or writes these values to the clipboard. The full URL uses
unindented rows, with explicit scrolling when it exceeds the card viewport.

## A provider that is off

An account of an off or removed provider has the cause `ProviderOff`, with the reason `gemini is off` or `gemini is removed`.
That reason wins over an expired sign-in, and Wisp offers no sign-in for it.
An account unavailable for its own reason keeps that reason. Its masked values stay masked.
A start or resume with no other account stops with the reason and the command to turn the provider on or add it back; no setup card opens.
An explicit `--account` keeps the unavailable-account line with the reason, without sign-in advice.
See [Provider plugins](plugins.md#provider-plugins) for switching, in-use refusals and idle sub-agents.
