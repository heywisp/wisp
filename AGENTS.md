# Wisp

Wisp is an open, LLM-agnostic AI interface: one assistant ("Jarvis") across chat, projects, coding, desktop work, voice and bots. **Wisp.Core** is its headless agent kernel, written in Rust: a library plus the `wispd` daemon, with every frontend as a client.

Research stays in the main checkout's git-ignored `research/`, not in the repository. Read the relevant report, if present, before designing a subsystem.

## Contribution rules (strict)

Follow @CONTRIBUTING.md exactly. In short: Conventional Commit messages and pull request titles, squash-merged pull requests only, never push to `main`, branches named `type/short-desc`. Subagents never commit or push, except the single merge agent, which commits, rebases, pushes and merges approved pull requests only through the verify-and-merge tools.

**Privacy:** never put the user's personal data (email, name, account IDs) in any external request, header or payload, including User-Agent strings. The only personal identity permitted in a publication is the user's chosen commit name and GitHub no-reply address, solely in Git author, committer and tagger fields and as the identity on the signing key. An API field used solely to set the squash commit's Git author field counts as that field. Only the public signing key may be registered with the host. Do not place this pair in any other field, text, file content, header, report or log.

**Secrets:** never print, commit, log or send a credential's value anywhere, the test keys in `.env` included. Never read, `cat`, `grep` or copy `.env`; only source it inside the command that needs it. An app identifier that is the same for every install (an OAuth client ID, an installed-app client secret) is not a credential or secret under these rules: its value lives in one plain place in code, never split or encoded, and appears nowhere else in the repository (docs, research, tests, fixtures) or in logs.

## How work is run

Work on Wisp follows the projects flow in `flows/projects/`. When `flows/projects/preferences.md` exists, the main session follows `flows/projects/FLOW.md` and that file. It runs from the flow's `main-session` preset, which loads both (FLOW.md, "Main instructions"); started without it, it takes in both at the start of every session, after every compaction and after a change to either, read itself or quoted by its helper as the preferences say, in parts small enough for a report (a report over 16 KiB is cut). Every other role runs as a child agent whose preset loads its role file, and the flow with it.

This flow is also **Wisp.Core's own orchestration model**, moving into the kernel step by step. Built: a child is Wisp narrowed by its spawn and preset (tools, spawning, model, effort) plus the preset's instructions, with its own context and log; every step is in the event stream; costs are shown per agent and in total, never capped. Still to build, as kernel logic rather than prompt instructions: the flow as a state machine, gates enforced in code (e.g. "verify" runs the tests), and typed role profiles, output schemas and handoffs; until then a report is free text. The kernel will provide these primitives with this flow as the default policy, and a router will let simple requests skip it.

## Skills

A skill is a how-to guide in the Agent Skills format: `.agents/skills/<name>/SKILL.md`, a `name` and `description` header, then the body. Before a job, read the skills listed for it, using `use` when available or their paths.

| Job | Read first |
|---|---|
| Changing or reviewing Rust code (not plans) | `.agents/skills/rust-code/SKILL.md`, then ponytail and rust-best-practices (below) |
| Sizing a plan's MRs | `.agents/skills/rust-code/SKILL.md`, its Plan sizing section only |
| Reviewing an MR, after reading its diff | `.agents/skills/review-checklist/SKILL.md` |
| Anything that goes through `heavy` (Hard code rules) | `.agents/skills/machine-load/SKILL.md` |
| Reporting an MR that changes what the user sees (its smoke run); running `wisp` (any command, fake keys included) or a live test against a real model (it costs real money: keep runs short, `--effort low`); starting or resuming a Wisp session from another | `.agents/skills/live-testing/SKILL.md` |

Third-party skills are read at their clones and never modified (both MIT):

- **ponytail**, the smallest complete change: `~/Projects/ponytail/skills/ponytail/SKILL.md`, cloned from https://github.com/DietrichGebert/ponytail.
- **rust-best-practices**, idiomatic Rust: `~/Projects/BeautifulRust/skills/rust-best-practices/SKILL.md`, cloned from https://github.com/morganlinton/BeautifulRust; its `references/` may be read when needed.

If a clone is missing, say so rather than skipping it silently.

Wisp's hard rules win on conflict: this file and `CONTRIBUTING.md` over any skill, and Wisp's skills over third-party ones (`rust-code` lists the known conflicts). A new skill gets its own directory, a `name` equal to the directory's, and a row in this table.

## Architecture

Core libraries live in `crates/`, each app, a form factor, in `apps/<app>/` with the crates only it uses, harness types as data in `flows/`, plugins in `plugins/`; `wisp-cli` and `wisp-tui` are apps still in `crates/`, moving later.

| Crate | Role |
|---|---|
| `wisp-protocol` | Wire types: messages, commands, events. The single contract between the kernel and every client. |
| `wisp-core` | The kernel: `agent` (turn loop), `kit` (builds agents from shared parts: connections, model settings, tools, workspace, instructions, log; every frontend builds its agents through it), `provider` (model adapters), `session`, `pricing`, `mcp` (session-tree MCP connections and saved server lists), `jobs` (the jobs host service: the process table, pipes, terminals and process groups), `call` (a call's output sink) and `plugin` (the plugin registry, the kernel host and the API ↔ protocol conversions). Planned: `context`, `tool`, `policy`. |
| `wispd` | The local session host: private sockets, connection admission and the JSON-lines link, serving command and event channels without the kernel. `serve` speaks the link on any admitted byte stream. The always-on daemon only lists, starts and relays chats. |
| `wisp-cli` | The `wisp` terminal client. Starts through `wisp-start`; full-screen chats run the kernel in separate `wisp --host` processes, and the client consumes only protocol events. |
| `wisp-start` | The start step shared by Wisp's programs: reads the user's config, flags, keys file, keychain and credentials into the Kit's inputs, opens or resumes the session and starts the root agent with its voice layer. Never depends on `wisp-tui` or `wisp-cli`. |
| `wisp-client` | The protocol client's run helpers and local session link: command and event channels, the quit wait, interrupts, a turn, dial, handshake and open. Depends on `wisp-protocol`, `tokio`, `serde` and `rustix` (the process uid). |
| `wisp-render` | Pure formatting for Wisp's clients: the transcript model, streaming Markdown, highlighting, wrapping, sanitisation, theme tokens and the agents picture's scene and raster (tiny-skia without `png-format`). No I/O. |
| `wisp-tui` | The full-screen terminal UI: owns the terminal, always restores it, and renders through `wisp-render`. Never depends on `wisp-core`. |
| `learn` | The learning app, in `apps/learn`: reads the flow's settings, starts its session through `wisp-start` on the user's Wisp config, keeps its lessons in its own folder and serves `learn-web`. |
| `learn-web` | The learning app's page and its host on this computer, in `apps/learn/web`: the page built into the binary, drawn from `wisp-render`'s theme and orb, and its socket, with the `Host`, `Origin` and peer checks and the one list each of what the page may send (`IN`) and the events it reads (`OUT`). Never depends on `wisp-core`. |
| `wisp-media` | Pure image handling shared by the kernel and the TUI: magic-byte sniffing, the API's resize rule and normalisation. No I/O. |
| `wisp-diff` | Pure diffs shared by the renderer and the `show-diff` plugin: the line diff, the tolerant unified-diff reader and its line classifier, and changed-word emphasis. No I/O, no clock. |
| `wisp-index` | Pure keyword ranking of tools, skills and documents using the plugin API's index entries: BM25 search, tool search text and first-line summaries. Depends only on `wisp-plugin-api` among workspace crates. No I/O, no clock. |
| `wisp-instructions` | Pure instruction files shared by the `instructions` plugin and the kernel's presets: the walker, imports, the rendered block, the `large` check and sha256, over a `Files` trait. Depends only on `wisp-plugin-api` and `wisp-tool-text` among workspace crates. No I/O, no clock. |
| `wisp-tool-text` | Pure tool wording shared by the kernel, its tools and plugins: output sections and limits, result text over the `wisp-plugin-api` outcome mirror, the jobs sentences, the file sentences (`files`), the question wording (`question`), the terminal size limits (`terminal`), the token estimate and the summary instruction's focus and last line (`compaction`), the checklist rules and wording (`todos`), the agent tools' wording (`agents`), the index hit, next-action, not-loaded and parameter-refusal wording (`index`), the code view checks (`code`), the chat rename wording (`chat`), shared search wording (`search`) and `shorten_path`. Depends only on `wisp-plugin-api`; no I/O. |
| `wisp-contain` | The panic-containment flag: `contain` for sync plugin calls, Markdown parses and the TUI's contained work, and `Contained` for plugin futures, read by the TUI's panic hook. No dependencies. |
| `wisp-plugins` | `bundled()`: the bundled plugins under `plugins/`, listed in `docs/plugins.md`'s bundled table, one Cargo feature each. |
| `wisp-plugin-api` | The plugin contract: the hand-written Rust mirror of the `wisp:plugin` WIT world (`wit/v0.1.0/`), the `wisp-plugin.toml` manifest and its validation, `Bundled`, `yield_now` and the internal `hex` support module. Depends only on `futures`, `serde` and `toml` (`docs/plugins.md`). |
| `wisp-wasm-host` | Hosts WASM plugins: `wisp:plugin` world 0.2.0 and 0.3.0 components on wasmtime, one instance per agent, behind the `wisp` binary's `wasm` feature (on by default; `--no-default-features` leaves it out). Returns `wisp-plugin-api` types only; never depends on `wisp-core`, `wisp-protocol`, `wisp-tui` or `wisp-render` (`docs/plugins.md`, "WASM plugins"). |
| `wisp-wasm-guests` | Tests only: builds the WASM guest crates (`plugins/wasm-fixture` and `plugins/board`, the board plugin, which is installed rather than bundled) for `wasm32-wasip2` and names their files. |

- **Events are the source of truth.** Every turn ends in exactly one terminal event (completed, cancelled or error, with a typed error kind). Never report a failure as success.
- **Providers are plugins.** Each provider comes from a plugin, switched by the same switches as any plugin (`docs/plugins.md`, "Provider plugins"). Its adapter stays in the kernel behind the `Provider` trait until a kernel contract lets the plugin hold it (planned). A provider plugin is never handed a key, a token, a sign-in route, an account's address or an origin, and a switch never touches a sign-in. Provider-specific data such as thinking signatures round-trips losslessly. Unknown blocks and events are recorded, never silently dropped.
- **Platform first.** Wisp is a platform, framework and engine. A user-facing feature lives in a plugin, on the plugin API (`docs/plugins.md`), not in the kernel or a client, and facts that change outside Wisp's releases (models with their limits and prices, endpoints, gateways) come from the provider, data files or settings, not code. The kernel keeps its primitives and what enforces an invariant (history, sending to providers, credentials and sign-in, caching, cost accounting, the session log), which plugins reach through host interfaces, and clients draw what plugins supply from protocol data. Where the API or a data source is missing, the plan (for an S task, the approach note) names it for the main session to queue, and that part stays hard-coded until it lands, as the voice prices in `pricing` do today.

## Cost rules (minimizing user cost is a core requirement)

- **Stable prefix, changes as deltas:** the system prompt and tool list are never edited in place. Wisp adds no absolute or `~` paths or IDs to the system prompt; file names are relative to the project root, to Wisp's config directory for the user's default, or to a preset's instructions file for a child of that preset. Instruction files (`AGENTS.md` and its imports) hold static instructions only: no dates or other per-turn data, which never go in the system prompt either, and only repository-relative, `~/` or placeholder paths (such as `<scratch>`), or system paths that are the same on every machine (such as `/dev/null`, `/usr/bin/env`), never user- or machine-specific absolute ones.
  - Per-turn data, such as the date, goes in the turn message. Agent type, mode and settings decide what is injected; today that is the `Sent:` stamp (`--time`).
  - Session changes (environment, mode, effort, tools) are appended to the conversation using the provider's cache-preserving features, such as mid-conversation system messages, per-message effort and inline tool add/remove.
  - Those features are enabled per model from a capability table taken from the provider docs, never guessed from version numbers.
- **Deterministic serialization:** no `HashMap` or `HashSet` in wire or request types. Tools and MCP schemas are sorted and canonicalised.
- **Append-only history, per step:** a block is committed once a request carrying it has been accepted by the provider, and is never changed after; until then a message holding a new attachment is provisional, and a refused attachment is replaced once by a text note (`docs/media.md`, "A refused attachment"). Committed blocks are never changed; new blocks are only appended, including after the last `tool_result` of an unanswered tail; a reply that was still streaming when the turn stopped is never committed; every requested call gets exactly one real or synthetic result.
- **Anthropic caching:**
  - The system/tools breakpoint always uses a 1h TTL.
  - The conversation tail uses the session's TTL: 1h for interactive use, 5m for continuous loops. 1h before 5m is a valid ordering.
  - One-shot calls (titles, classifiers, unshared compaction) send no cache markers.
  - Prefer the 1h TTL and pre-warming (`max_tokens: 0`) over keep-alive pings, with one exception, keep-warm (`docs/agents.md`, "Keep-warm"): an idle agent its parent marked at spawn, or the main session with `keep_warm` on, gets a one-token refresh of its last request before that request's cache entry expires, a bounded number of times per idle spell. Its prompt (tools, system, thinking, effort and messages) is byte-identical to that request's through its 1h breakpoint; it is never checked as a miss, commits nothing and is priced like any request. Nothing else is ever pinged.
- **Watch for cache breaks:**
  - Emit `CacheMiss` when reads collapse.
  - Planned: fingerprint each request's prefix locally so a miss names its cause (tools changed, system changed, and so on).
  - The prefix-stability test must keep passing and grows with every feature that touches requests (rollback, resume, compaction, forks, tool changes).
- **Prices live in one place** (text-model prices in the shipped model file, turned into rates only by `pricing`; voice prices in `pricing` until their data source lands). Normalise usage per provider, price 5m and 1h cache writes separately, and report estimated cost per turn and per session.
- **Shared prefixes:** forks and compaction reuse the parent's exact prefix. A role with its own system prompt, tools or model gets its own stable prefix, shared by every agent in that role. Fan out only after the first request starts streaming.

## Hard code rules

- **No comments in code**: no `//`, `///` or `//!`, in source or tests. Documentation lives in `docs/*.md`. Explain a lint exception with `#[allow(lint, reason = "...")]`, and express contracts through types, names and tests rather than prose.
- Workspace lints deny `unwrap`, `expect`, `panic!` and `unsafe` outside tests.
- Start every build, test run, clippy, `cargo install`, fuzzer, `docker run` of a build or test, and loop that reruns a test through `heavy`. If it gives up (exit 75), never bypass it; `machine-load` says what to do, and where `heavy` isn't installed, read `machine-load` first.
