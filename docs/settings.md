# Settings

This page is generated from Wisp's settings registry (`wisp_protocol::settings`); a test keeps it in step, and `WISP_BLESS=1` rewrites it.

Every setting can be changed without editing `config.toml`: `wisp config set NAME VALUE` saves it, `wisp config unset NAME` removes the saved value, and `wisp --set NAME=VALUE` (or a setting's own flag) sets it for one run. A setting saved per account or model takes its path, such as `wisp config set 'models."claude-opus-5-5".compact_at' 80%`, or `wisp --set 'models."claude-opus-5-5".compact_at=80%'` for one run, which applies at that place only and above the saved tables. `wisp config get NAME --explain` shows every place that sets a value and which one wins.

Where a value comes from, highest first: the session (the shortcuts `/model`, `/effort` and `/account`), flags and `--set`, the environment, the account×model table, the model table, the account table, the top level of `config.toml`, the shipped model facts, and the default. A `web_search` list that names only the other wire's versions is passed over (`docs/endpoints.md`, "Web search"). `/config` has one web search switch, the `Web search` row (the `web-search` plugin's own); it shows no row for `native_web_search` or a model's `web_search` list, which still act when saved: the row reads `off for this account` or `not on this model`, and `d` removes the saved value for new chats.

`/config` starts with Model, Effort, Account, Web search, Tools, Compact at and Theme. Plugins opens the plugin list; More settings opens the remaining groups. Account opens the accounts, with their fields behind the right arrow; Context window stays on the model's page and Pricing on the account's page. `/config NAME` opens the page a setting is on, and `/` finds rows across every page.

Often used adds up to three rows from More settings or a plugin's page on the first screen: the most changed, then the latest, without reordering while the screen is open. Only an applied save of one of those rows on the settings screen counts, including when it is already under Often used. Unsaved changes, plugin switches, sidebar folds and its switch, `wisp config set` and model changes do not.

`settings-used.json`, beside the usage and models folders, holds only registered setting names, change counts and last-used day numbers, with the credentials file's permissions. Names unused for 60 days drop out; a missing or broken file reads as empty and the next use replaces it. These records reach no model request.

A row shows what this chat uses. `this chat` marks a value that differs from what new chats use; the line under the row tells both values. Values set outside Wisp, by a mode or by a preset are named on that line instead, without a mark.

Saving Compact at, Context window, Background compaction or Background start on the settings screen removes that setting's value for this chat and ends its start flag in this chat, so the saved per-model numbers decide. In a number's box, Tab switches For between all models and this model; a third place appears only for a number already saved for this model on this account. Emptying the field and pressing Enter removes the number at the chosen place. On a `this chat` row, `d` returns the chat to what new chats use without changing what is saved. It is not offered when a flag would keep winning or when returning to the current model's number would override other models; Enter still saves.

`?` opens the details at every width: This chat, Saved and Wisp's own values, the flag or variable, the file and line, and Name (the setting's name in the file). Numbers align on their right edge. A value saved only for new chats keeps that mark when the screen is opened again. Tools and Plugins count the switches that are on; Compact at shows the point and the window the next turn uses, such as `850,000 of 1,000,000`.

On `--resume`, only the command line counts: a flag or `--set` value on the resume command applies to the resumed session from its next turn (`--account` from the start, `--instructions` and `--instructions-extra` as one rewrite of the project block), while config, environment and saved defaults never override what the session logged. A flag equal to the value the next turn would use anyway changes nothing; one that differs is named in the resume notice with what it does to the cache, and a key-shaped model is never shown there. Child agents keep their logged settings (`docs/session-log.md`, "Flags on resume").

A plugin can define on/off or whole-number settings under `[plugins.<id>]`. Where this Wisp has a plugin that defines settings, an unknown name is refused. Otherwise, a line that is not a table is kept and skipped; a near miss of `enabled` or `installed` is still refused.

Secrets (an account's key and URL) never pass through a setting command; `wisp config set` points to `wisp account edit`. `--set` doesn't change how an account connects (its provider, auth, key store or variables) for one run either, and points there too.

A path never echoes a key typed into it by mistake. A part that looks like a key (20 or more characters with a lowercase letter, a digit and no `_`) is refused as `that isn't a setting name`, unless it is the account part and names an account in `config.toml`, such as `endpoints.prod-gateway-eu-west-2.pricing` (`--set` is read before `config.toml`, so it refuses that account part too); a model ID of that shape is written in quotes. A model ID that looks like a key, with a run of 10 or more letters and digits holding both (such as `sk-ant-api03-…`) or a run of 20 or more letters in both cases (such as Hugging Face's `hf_…` and `api_org_…`), is shown as `(looks like a key, not shown)` in `wisp config get` and `list` (with or without `--json`), `wisp doctor`, the start notices, the settings screen and the rest of the full screen, and is taken out of an error a provider sends back; Wisp still uses the value as given. A path under such a model, as saved in `config.toml`, reads `models."MODEL".…` there, in what `wisp config set` and `unset` echo and in the settings screen's `wisp config set` line, and a sentence that names the model says `this model`. The session log and `--output jsonl` keep the value as given.

MCP servers use named tables in `config.toml`, separate from registered settings:

```toml
[mcp.servers.example]
command = "example-mcp-server"
args = ["--stdio"]
```

The kernel reads these tables without starting servers. `command` can also be an HTTP(S) address, with no arguments. Command arguments are passed directly. The optional `keys = ["SENTRY_TOKEN"]` names the environment variables the server reads; never put their values in this table. Wisp asks for missing keys in its secret box when an agent adds or updates a server, and keeps them in its secret store. An older Wisp refuses a settings file containing `keys`. HTTP sign-ins use the existing secret store, never this table. See `docs/mcp.md`.

`preload` decides which permitted tools a new chat starts with: `["core"]` (the default), `["none"]` (only search and use), or a list of tool names. Followers come with their leader, search and use stay while on, and children keep report. Client tools left out are held and found by search; tools switched off cannot be found. `/config` calls the row **Start with** and offers core and none. A preset can name its own list; without one, a child uses the session's preload over its parent's permitted tools, including held ones.

## Chat

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `context.context_note` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `context.context_note` | `--context-note`, `--set context.context_note=…` | next turn · cache kept | Tell the model how full its context is, with a Context: line on each message. |
| `effort` | one of `low`, `medium`, `high`, `xhigh`, `max` (medium) | `effort` | `--effort`, `--set effort=…` | next turn · cache kept with per-message effort, else rebuilt | How hard the model thinks. |
| `endpoint` | an account name (the start rules) | `default` | `--account`, `--endpoint`, `WISP_ACCOUNT`, `WISP_ENDPOINT`, `--set endpoint=…` | next turn · cache rebuild | The account this chat uses. |
| `instructions.extra` | `none` or `autonomous` (none) | `instructions.extra` | `--instructions-extra`, `--set instructions.extra=…` | new chats | Built-in instructions that go before the instruction files: none or autonomous. |
| `instructions.source` | one of `auto`, `default`, `none` (auto) | `instructions.source` | `--instructions`, `--set instructions.source=…` | next turn · cache kept | Which instruction file goes in the system prompt: auto, default or none. |
| `instructions.warn_tokens` | tokens (`40000`, `40k`, `1M`) (8000) | `instructions.warn_tokens` | `--instructions-warn-tokens`, `--set instructions.warn_tokens=…` | now | Warn when the instruction files come to more than this many tokens. |
| `model` | a model ID (claude-opus-5-5) | `model` | `--model`, `--set model=…` | next turn · cache rebuild | The model to chat with. |
| `preload` | a list of tool names, core for Wisp's tools, or none for search and use (core) | `preload` | `--set preload=…` | new chats | core starts with all permitted Wisp tools; none starts with search and use. On resume, tools no longer held by this choice arrive at the first message. |
| `self_check.before_end` | `true` or `false` in `config.toml`, `on` or `off` on the command line (off) | `self_check.before_end` | `--self-check`, `--set self_check.before_end=…` | next turn · cache kept | Before a turn of the main agent that ran tools ends, ask the model once to check its work against the task. |
| `time` | one of `local`, `utc`, `off` (local) | `time` | `--time`, `--set time=…` | next turn · cache kept | Stamp each message with the time it was sent: local (offset only), utc or off. |
| `tools` | a list of tool names from the plugin catalog (the `--tools` values), or `none` (tools on by default) | `tools` | `--tools`, `--set tools=…` | next turn · may rebuild the cache | Which tools the model may use, or none. A saved list is exact: a tool added in a later version of Wisp isn't switched on for it. |
| `turned_off` | names of tools, skills and servers, or `none` (none) | `turned_off` | `--set turned_off=…` | next turn · may rebuild the cache | Tools, skills and servers that can't be found or used. Next message. |

## Compaction

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `compact_at` | tokens (`400k`) or a share of the window (`85%`) (window − reserve) | `compact_at`, `models."<m>".compact_at`, `endpoints.<e>.models."<m>".compact_at` | `--compact-at`, `--set compact_at=…` | next turn · cache kept | When the chat is shortened. |
| `context.auto_compact` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `context.auto_compact` | `--no-auto-compact`, `--set context.auto_compact=…` | next turn · cache kept | Compact the conversation before a request that would reach the compaction point. |
| `context.background_compaction` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on where the provider compacts) | `context.background_compaction`, `endpoints.<e>.background_compaction`, `models."<m>".background_compaction`, `endpoints.<e>.models."<m>".background_compaction` | `--set context.background_compaction=…` | now | Prepare a native compaction in the background before the compaction point, and swap it in on the next request. |
| `context.background_tokens` | tokens (`40000`, `40k`, `1M`) (40000) | `context.background_tokens`, `endpoints.<e>.background_tokens`, `models."<m>".background_tokens`, `endpoints.<e>.models."<m>".background_tokens` | `--set context.background_tokens=…` | now | How far below the compaction point a background compaction starts. |
| `context.compact_effort` | one of `low`, `medium`, `high`, `xhigh`, `max` (high) | `context.compact_effort` | `--set context.compact_effort=…` | now | The highest effort a compaction's summary runs at, where the server takes a per-message effort. |
| `context.strategy` | `default` or a plugin's id (default) | `context.strategy` | `--set context.strategy=…` | next turn · cache kept | Who lays out a compaction's summary and what it keeps: default for Wisp's own layout, or the id of an installed plugin that offers a strategy. |
| `context.verbatim_user_tokens` | tokens (`40000`, `40k`, `1M`) (8000) | `context.verbatim_user_tokens` | `--set context.verbatim_user_tokens=…` | now | How many tokens of your recent messages a compaction keeps word for word; a model table can set its own. |

## Agents

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `agents.max_agents` | 1 to 50, or `none` (50) | `agents.max_agents` | `--set agents.max_agents=…` | new chats | How many live (not ended) agents one session's tree may hold, the main agent included. |
| `agents.max_depth` | 1 to 5, or `none` (2) | `agents.max_depth` | `--set agents.max_depth=…` | new chats | How deep agents may spawn: the main agent is depth 0, its children 1, theirs 2. |
| `agents.max_running` | 1 to 16, or `none` (4) | `agents.max_running` | `--set agents.max_running=…` | new chats | How many of one agent's children may run a turn at once, not counting children whose preset sets worker = false; the others wait their turn. |
| `agents.max_tree_running` | 1 to 50, or `none` (none) | `agents.max_tree_running` | `--set agents.max_tree_running=…` | new chats | How many agents in one session's tree may run a turn at once, not counting the main agent or agents whose preset sets worker = false; the others wait their turn. |
| `agents.wake_when_stuck` | 0 to 1440 (0) | `agents.wake_when_stuck` | `--set agents.wake_when_stuck=…` | new chats | Minutes before Wisp wakes the main session when no agent has worked and its checklist has open items, or an agent's parent when that agent has waited that long to retry; 0 never wakes. |
| `agents.warm_refreshes` | 0 to 24 (8) | `agents.warm_refreshes` | `--set agents.warm_refreshes=…` | new chats | How many times per idle spell Wisp refreshes the prompt cache of an agent kept warm; 0 turns keep-warm off. |
| `keep_warm` | `true` or `false` in `config.toml`, `on` or `off` on the command line (off) | `keep_warm` | `--set keep_warm=…` | new chats | Refresh the main session's prompt cache while it idles, up to agents.warm_refreshes times per idle spell. |
| `preset` | an agent type's name (none) | `preset` | `--set preset=…` | new chats | The preset the main agent starts from: its instructions and tool limits, and its account, model and effort where no other setting chooses them. |

## Voice

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `voice.endpoint` | an account name (none) | `voice.endpoint` | `--set voice.endpoint=…` | next call | The account voice calls go through. Only accounts that offer voice are listed. |
| `voice.instructions` | a file, relative to config.toml's directory, under `~/` or absolute (none) | `voice.instructions` | `--set voice.instructions=…` | next call | The file of instructions the subscription's voice gets when a call opens; the API voice uses its own. A relative path starts at the config file's folder. |
| `voice.language` | a two-letter language code (ISO 639-1), such as en (detected from speech) | `voice.language` | `--set voice.language=…` | next call | The language spoken on calls, which helps the API voice hear it; unset, it is detected from speech. |
| `voice.voice` | a voice's name (the account's default voice) | `voice.voice` | `--set voice.voice=…` | next call | The voice that speaks on calls, from the voices the voice account offers. |

## Display

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `display.ambiguous` | one of `auto`, `wide`, `narrow` (auto (detected)) | `display.ambiguous` | `WISP_AMBIGUOUS`, `--set display.ambiguous=…` | now | How wide the terminal draws ambiguous-width characters such as ·. |
| `display.color` | one of `auto`, `truecolor`, `256`, `16`, `none` (auto (detected)) | `display.color` | `WISP_COLOR`, `--set display.color=…` | now | How many colours to use. |
| `display.copy_on_select` | one of `auto`, `on`, `off` (auto (detected)) | `display.copy_on_select` | `WISP_COPY_ON_SELECT`, `--set display.copy_on_select=…` | now | Copy text to the clipboard as soon as it is selected. |
| `display.full_repaint` | `true` or `false` in `config.toml`, `on` or `off` on the command line and in variables (off) | `display.full_repaint` | `WISP_FULL_REPAINT`, `--set display.full_repaint=…` | now | Redraw the whole screen each frame instead of only the changed cells, and hold every animation still, as reduced motion does. |
| `display.motion` | one of `auto`, `full`, `calm`, `reduced` (auto) | `display.motion` | `WISP_MOTION`, `--set display.motion=…` | now | How much the screen animates. |
| `display.mouse` | `true` or `false` in `config.toml`, `on` or `off` on the command line and in variables (on) | `display.mouse` | `WISP_MOUSE`, `--set display.mouse=…` | now | Use the mouse to scroll, select and see names under the pointer on `/agents`. |
| `display.pictures` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `display.pictures` | `--set display.pictures=…` | now | Draw the agents and Context boxes as pictures where the terminal can show images. |
| `display.scroll_speed` | 1 to 20 (1) | `display.scroll_speed` | `WISP_SCROLL_SPEED`, `--set display.scroll_speed=…` | now | Lines scrolled per mouse-wheel step. |
| `display.sidebar` | `auto` or `hidden` (auto) | `display.sidebar` | `WISP_SIDEBAR`, `--set display.sidebar=…` | now | Auto docks the sidebar beside the transcript on a wide terminal once a section has something to show; hidden never docks it. |
| `display.sidebar_folded` | a list of sidebar sections (`<plugin>/<name>`), or `none` (none) | `display.sidebar_folded` | `--set display.sidebar_folded=…` | now | Sidebar sections drawn as their title row only. A click on a section's title folds it or opens it again. |
| `display.sidebar_pinned` | a list of sidebar sections (`<plugin>/<name>`), or `none` (none) | `display.sidebar_pinned` | `--set display.sidebar_pinned=…` | now | Sidebar sections kept at the top while the rest scroll under them. |
| `display.theme` | one of `auto`, `dark`, `light` (auto (detected)) | `display.theme` | `WISP_THEME`, `--set display.theme=…` | now | Dark or light colours; auto follows the terminal's background. |
| `display.verbose` | `true` or `false` in `config.toml`, `on` or `off` on the command line (off) | `display.verbose` | `--verbose`, `--set display.verbose=…` | now | Show the full token and cache line after each turn, all of the thinking, and a line for every hook run. |

## Advanced

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `cache_ttl` | `5m` or `1h` (1h) | `cache_ttl` | `--cache-ttl`, `--set cache_ttl=…` | next turn · may rebuild the cache | How long the conversation's prompt-cache entry lives: 1h for interactive use, 5m for continuous loops. |
| `claude_usage` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `claude_usage` | `--set claude_usage=…` | now | Read Claude subscription allowance usage when displayed. On by default; claude_usage = false turns it off. |
| `context.count_tokens` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `context.count_tokens` | `--set context.count_tokens=…` | now | Let /context ask the server's token counter for the fixed part of the request, where it has one. |
| `debug_cache` | `true` or `false` in `config.toml`, `on` or `off` on the command line (off) | `debug_cache` | `--debug-cache`, `--set debug_cache=…` | next turn · may rebuild the cache | Ask the API to diagnose prompt-cache misses. |
| `keychain` | one of `auto`, `on`, `off` (auto) | `keychain` | `--keychain`, `WISP_KEYCHAIN`, `--set keychain=…` | new chats | Whether Wisp uses the system keychain: auto (when this machine has one; not under WSL), on or off (variables and credentials.json only). |
| `log` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `log` | `--no-log`, `--set log=…` | new chats | Write a session log. |
| `message_threads` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `message_threads` | `--set message_threads=…` | new chats | Send Claude only each request's new messages, on the API and a subscription. Anthropic then holds the conversation, system prompt and tools in memory for up to an hour; not eligible for ZDR or HIPAA. |
| `refusal_fallback` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `refusal_fallback` | `--no-fallback`, `--set refusal_fallback=…` | next turn · cache kept | Re-run refused requests on Anthropic's fallback model. |
| `retry.budget_usd` | US dollars (`0.50`) or `none` (none) | `retry.budget_usd` | `--retry-budget`, `--set retry.budget_usd=…` | now | Cap what retries of failed requests may cost per turn, in US dollars. |
| `tool_output.max_age_days` | 1 to 3650, or `none` (7) | `tool_output.max_age_days` | `--set tool_output.max_age_days=…` | new chats | How many days other sessions' saved tool outputs are kept; Wisp removes older ones when it starts, and none keeps them. |

## Accounts

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `auth` | one of `api_key`, `bearer`, `anthropic_subscription`, `chatgpt_subscription`, `antigravity_subscription` (none) | `endpoints.<e>.auth` | `wisp account edit` | new chats for the current account; at the next switch for another | How the account takes its key: api_key, bearer or anthropic_subscription for Anthropic, api_key or chatgpt_subscription for OpenAI, api_key, bearer or antigravity_subscription for Gemini. |
| `client_version` | a version such as 1.2.3 (latest release) | `endpoints.<e>.client_version` | `--set PATH=…` | new chats | The version to report. |
| `key` | a secret, never typed as a value (none) | `endpoints.<e>.key` | `wisp account edit` | new chats for the current account; at the next switch for another | The account's key, kept in credentials.json; change it with wisp account edit. |
| `key_env` | a variable name (none) | `endpoints.<e>.key_env` | `wisp account edit` | new chats for the current account; at the next switch for another | The variable that holds the account's key, where no keychain is available. |
| `key_store` | `file` or `keyring` (none) | `endpoints.<e>.key_store` | `wisp account edit` | new chats for the current account; at the next switch for another | Where the account's key is kept: file (credentials.json) or keyring. |
| `max_in_flight` | 1 to 64, or `none` (by account kind) | `endpoints.<e>.max_in_flight` | `--set PATH=…` | new chats | How many requests the whole agent tree may have open on this account at once; the rest wait their turn, the main agent first. |
| `native_web_search` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `endpoints.<e>.native_web_search` | `--set PATH=…` | new chats | Send the provider's own web search tool through this account. |
| `pricing` | `list` or `none` (list) | `endpoints.<e>.pricing` | `--set PATH=…` | now | Estimate costs at list prices, or show tokens only. |
| `provider` | one of `anthropic`, `openai`, `gemini` (anthropic) | `endpoints.<e>.provider` | `wisp account edit` | new chats for the current account; at the next switch for another | Which provider the account speaks to. |
| `store_id` | written by Wisp (none) | `store_id` | written by Wisp | new chats | Which records in credentials.json belong to this config; written by Wisp. |
| `url` | a secret, never typed as a value (none) | `endpoints.<e>.url` | `wisp account edit` | new chats for the current account; at the next switch for another | The account's URL; change it with wisp account edit. |
| `url_env` | a variable name (none) | `endpoints.<e>.url_env` | `wisp account edit` | new chats for the current account; at the next switch for another | The variable that holds the account's URL, where no keychain is available. |

## Models

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `aliases` | a list of model IDs (none) | shipped, `models."<m>".aliases` | `--set PATH=…` | new chats | Other model IDs that name this model's row. A save made mid-session reaches the window and the point at the next change of either. |
| `context_window` | tokens (`40000`, `40k`, `1M`) (the documented window) | shipped, `models."<m>".context_window`, `endpoints.<e>.models."<m>".context_window` | `--set PATH=…` | next turn · cache kept | Your limit on the model's context window; the documented window is the default. |
| `max_output` | tokens (`40000`, `40k`, `1M`) (documented maximum) | shipped, `models."<m>".max_output`, `endpoints.<e>.models."<m>".max_output` | `--set PATH=…` | new chats | The longest reply this model may write, in tokens; it can only lower what Wisp asks for. |
| `native_compaction` | `true` or `false` in `config.toml`, `on` or `off` on the command line (the shipped fact, else off) | shipped, `models."<m>".native_compaction`, `endpoints.<e>.models."<m>".native_compaction` | `--set PATH=…` | new chats | Whether the provider can compact this model's conversations itself. |
| `price_cache_read` | US dollars (`0.50`) (shipped price or none) | shipped, `models."<m>".price_cache_read`, `endpoints.<e>.models."<m>".price_cache_read` | `--set PATH=…` | new chats | US dollars per million tokens read from the cache; set all three prices or none. |
| `price_input` | US dollars (`0.50`) (shipped price or none) | shipped, `models."<m>".price_input`, `endpoints.<e>.models."<m>".price_input` | `--set PATH=…` | new chats | US dollars per million input tokens; set all three prices or none. |
| `price_output` | US dollars (`0.50`) (shipped price or none) | shipped, `models."<m>".price_output`, `endpoints.<e>.models."<m>".price_output` | `--set PATH=…` | new chats | US dollars per million output tokens; set all three prices or none. |
| `web_search` | a list of `web_search_20250305`, `web_search`, `google_search`, or `none` (the shipped fact, else none) | shipped, `models."<m>".web_search`, `endpoints.<e>.models."<m>".web_search` | `--set PATH=…` | new chats | Which versions of the provider's web search tool this model supports; none means no native search. |

## Plugins

| Setting | Values (default) | Saved at | Command line | Applies | |
|---|---|---|---|---|---|
| `plugins.<id>.enabled` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `[plugins.<id>]` `enabled` | `--set PATH=…` | next turn · may rebuild the cache | Turn a plugin on or off: off skips its hooks, sidebar sections and prompt sections and refuses its commands from the next turn. A provider's plugin goes off at once, and its accounts can't be used while it is off. |
| `plugins.<id>.hooks.<hook>` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `[plugins.<id>.hooks]` `<hook>` | `--set PATH=…` | next turn · cache kept | Turn one plugin hook on or off; a hook with its own switch setting follows that setting instead. |
| `plugins.<id>.installed` | `true` or `false` in `config.toml`, `on` or `off` on the command line (on) | `[plugins.<id>]` `installed` | `--set PATH=…` | next turn · may rebuild the cache | Remove a plugin, or add it back, from the next turn; a provider's plugin at once. Removing a provider deletes no account, key or sign-in. |
| `plugins.<id>.slots.<name>` | `true` or `false` in `config.toml`, `on` or `off` on the command line (the section's manifest default) | `[plugins.<id>.slots]` `<name>` | `--set PATH=…` | now | Show one plugin's sidebar section, or leave it out of the sidebar. |

## Agent presets

Optional `[agents.presets.NAME]` tables in `config.toml` name the choices a spawn may make; `docs/agents.md` describes them. They live only in `config.toml`: `wisp config set` points there. A name is a lowercase letter, then up to 31 of a-z, 0-9, _ and -. An unknown key, tool, model or effort, or instructions that don't load, turn off only that preset, and `wisp doctor` says why. An account that isn't set up or is unavailable stays in the preset for the spawn to resolve.

| Key | Values (default) | |
|---|---|---|
| `description` | text (required) | What the preset is for; agents that may spawn see it beside the preset's name. |
| `effort` | one of `low`, `medium`, `high`, `xhigh`, `max` (the parent's) | The child's effort; with `model` set, one that model takes. |
| `endpoint` | the name of one of the session's accounts (the parent's) | The account the child runs on; with `model` set, one that serves it. |
| `fallback` | a list of `{ endpoint, model?, effort? }` (none) | Where a spawn runs, in order, when the account before has no room; an agent whose limit is used up moves to the first one with room. |
| `instructions` | a file, or a directory holding `AGENTS.md`, relative to config.toml's directory, under `~/` or absolute (none) | Instructions for the child's role, loaded at each spawn and placed after the project's; `@` imports work as in `AGENTS.md`. |
| `model` | a model ID its account serves (the parent's) | The child's model; without `endpoint`, one some account of the session serves. |
| `preload` | a list of tool names, core, or none (the session's preload) | Which tools the child starts with; search and use stay while on, and report stays. |
| `spawn` | `true` or `false` (the parent's) | Whether the child may spawn agents of its own; false removes spawn_agent and stop_agent. |
| `tools_off` | a list of tool names (none) | Tools the child doesn't get: any `--tools` value from the plugin catalog, or stop_agent. |
| `worker` | `true` or `false` (true) | Whether the child is a worker: workers wait for a place under agents.max_running and agents.max_tree_running; false, for an agent that directs or serves others, runs at once and takes no place. |

## Reserved

These names are kept for later; every command and the parser refuse them.

- `agents.<name>.*`, for any name other than `max_agents`, `max_depth`, `max_running`, `max_tree_running`, `presets`, `wake_when_stuck` and `warm_refreshes`: planned with agent types; a preset goes in `[agents.presets.<name>]`.
- `agent`: planned with agent types and can't be set yet.
