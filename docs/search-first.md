# Search-first

`wisp-index` is the pure keyword search used to find tools, skills and documents.
It does no I/O, reads no clock and makes no model request. The `search` plugin
uses it and is bundled with the skills source. `search` is on by default and
`use` follows it. Ordinary requests gain their two definitions and one fixed
instruction; permitted tools held by preload are found without changing the prefix.

## Entries

World 0.3.0 defines `index.kind` and `index.entry` in
`wit/v0.3.0/index.wit`. Their Rust mirrors live in
`wisp_plugin_api::index`; the ranking crate defines no second entry type.

An entry holds:

- `kind`: `tool`, `skill` or `document`, named and parsed by `Kind`;
- `name`: the full name, including a server prefix when there is one;
- `summary`: one line for the result;
- `text`: the full search text, including the name once.

`tool_entry(name, description, schema)` builds an entry from a tool definition's
fields. Its text contains the name, description, argument names and argument
descriptions, including nested arguments and schema alternatives. Its summary
comes from `first_line`: the first nonblank line, trimmed at both ends.
The schema is already parsed JSON; no schema is read or changed by the index.

## Ranking

`Index::new(entries)` builds an in-memory index. `Index::search(query, kind,
limit)` returns entries in descending score order. `kind` optionally restricts
the results; `limit` defaults to five, and zero returns nothing.

Ranking uses BM25 with k1 1.2 and b 0.75. Words are lower-cased; a trailing `s`
is folded. Compound names are indexed whole and in parts: `snake_case`,
`camelCase` and `mcp__server__tool` all keep their whole word as well as the
parts. Thus `create_issue` finds `mcp__github__create_issue`.

The name is counted once more than its occurrence in the full search text,
so a name match counts double. Document frequency and average weighted length
come from the whole index, including when a search filters by kind. Repeated
query words count once. Equal scores sort by name; hits below 30% of the best
score are dropped. An empty query or a query with no matching words returns no
results. There is no stemming beyond the trailing `s`, no stop-word list and
no synonym matching.

The kernel may build tool entries, but ranking belongs to plugins. A plugin
may depend on `wisp-index`, and uses the same entry types as every source.
The tests reuse the real tool-definition fixtures rather than keeping a copy.

## Search and use

The bundled `search` plugin ranks the host's snapshot with this crate.
`search` is on by default and `use` follows it. `search` accepts a query,
optional kind and optional limit, and returns one line per hit plus a `hits` fact.
The transcript formats the fact's three counts; an empty result says `nothing found`.

For tool hits, `index.open(name)` supplies the parameter schema as JSON text and
the result says to run the tool with `use`. For skills and documents it says to
read them with `use`; that tool asks the live owner for the entry's text.
A `use` of a tool is unwrapped by the kernel before preparation and runs with
that tool's row, timeout, hooks and gate, while history keeps the original call.

The kernel gathers entries at start, then at a turn opening or after a completed,
uncancelled tool call when a source marks them changed. Gathering refreshes the
searchable index, not provider declarations or the cached system prefix. Off tools are excluded. Duplicate
names keep the first kind (tool, skill, document), then the first source in stable
plugin-name order, and the other entry produces a problem. A source that panics
while listing is left out of that gathering with a problem. No ranking runs in
the kernel, and no new tool definition is put into the request by search yet.

## Turning tools and skills off and on

`manage`, the search plugin's third tool, turns a tool or skill off in this chat,
or back on, from the next message. It is on by default and held: the manifest key
`held = true` keeps it out of the start array and the deferred set under every
preload, while it stays enabled (the state a preload leaves), so the model finds it
with `search` and runs it like any found tool or through `use`. A tools list that
leaves it out leaves it off.

Its input is `action` (`on`, `off`, `add` or `remove`), `kind` (`tool`, `skill` or `server`),
`name` and optional `from`. For a server, `add` and `remove` use the same service
as `/mcp`; `from` is the command line or address, omitted for a saved server
waiting for sign-in. For a skill, `add` validates the local folder in `from` and
copies it into Wisp's config skills folder; `remove` only removes a skill there,
never a project or shared skill. Existing names are refused. Skill changes are
searchable before the next tool call in the same turn, for a main or child agent,
without a new question. For a tool, add/remove explain the file or shell operation.
A follower is acted on as its leader, and the answer names the leader. For
switches and servers, the plugin calls the world 0.4.0 import `manage.manage`,
which only the bundled `search` plugin may use inside its own call. For a switch,
the kernel decides from the facts it writes at each turn opening:

- only the main chat of a full-screen session switches; a child or a `-p` run is refused;
- a provider's own tool (`web_search`) is refused, since switching it rebuilds the cache;
- an `on` for what is on, or an `off` for what is off, changes nothing and completes with `NAME is already on` (or `off`);
- an `on` that would rebuild (the chat's requests can't carry the tool back) is refused before anything is asked;
- turning off `search`, `use` or `manage`, or turning on what the person's settings have off (the Turned off list or the Tools list), asks him on today's question card (`Approve`, `Not now`), within a 10-minute limit; where no one can be asked it is refused with where to change it;
- anything else is done at once: an `off` adds the name to this chat's `turned_off`, an `on` removes a name the agent itself added.

A switch is queued as a session settings change when the call completes; a call
cut by its limit or a stop applies nothing. An on/off change goes through the tool
moment as any `tools` change does: a removal or addition by name where inline
changes are established, a note where they aren't. An agent's own change is
logged with `"by": "agent"` (`docs/session-log.md`); an approved one isn't, and
only the agent's own names come back on without asking, after a resume too.

`turned_off` (Turned off in /config) lists tools, skills and servers that can't be
found or used. A listed name is left out of the start set, the search index and
the nudge; `use` of it answers that it is in Turned off.

Held server tools follow a listing before the next step and stay undeclared.
Changed loaded definitions wait for the next opening. A child follows only the
servers present when it starts or resumes, keeps its fixed declared tools, and leaves ordered
server-change notes for the main chat.

A no-shell main chat asks before changing saved servers; a no-shell child names
the call its parent should make. Sign-in starts in Wisp, and its link is an
event-only `LinkNote`, never a tool result or logged `PluginNote`. The chat window
tries to open it; repeating add with the server's name tries again. Replay and
JSON-lines sinks clear these links while keeping ordinary notes.
