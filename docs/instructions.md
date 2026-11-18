# Project instructions

Wisp reads one instruction file per session, `AGENTS.md`, and puts it in the system prompt. The bundled `instructions` plugin (`plugins/instructions`) loads it through `wisp-instructions`, the walker shared with the core's preset files; the plan is `docs/plans/instructions.md`, and the move into a plugin is `docs/plans/plugin-moves.md` §2.3 ("Where it runs" below).

## The project root

The project root is the nearest ancestor of the working directory, itself included, that is a git repository, following git's own checks (`wisp_instructions::is_repository`): its `.git` is a directory holding `HEAD`, `objects` and `refs`, or a file of at most 1 MiB starting with `gitdir:`, as a linked worktree or submodule has. Anything else named `.git`, such as an empty directory a sandbox mounts in `/tmp`, is ignored. Outside a repository it is the working directory. Finding it reads only `.git` entries, never an instruction file. A session started in `~/Projects/Wisp/crates` reads `~/Projects/Wisp/AGENTS.md`, and each linked worktree reads its own checked-out copy.

The root is fixed at session start; the bash tool's `cd` doesn't move it.

## The two paths

| Source | Path | Scope |
|---|---|---|
| Project | `<project root>/AGENTS.md` | `project` |
| Default | `$XDG_CONFIG_HOME/wisp/AGENTS.md` when `XDG_CONFIG_HOME` is absolute, else `~/.config/wisp/AGENTS.md` | `default` |

A child agent's preset, or the main agent's from the `preset` setting, may add a third file, with scope `preset` ("Preset instructions" below).

- Only these two paths are looked for as the session's own instructions: no parent directories above the root, no subdirectories and no per-directory files. Other files load only when the chosen file imports them.
- The name is exact. `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.override.md` and other names are never looked for.
- `--config PATH` doesn't move the default: a scratch `config.toml` never drops your instructions.
- `config.toml` and `.env` still never come from the working directory. Only the instruction text does.

## The choice

1. If `<root>/AGENTS.md` exists as a directory entry, the project file is chosen and the default isn't looked at.
2. Otherwise, if the default exists, it is chosen.
3. Otherwise nothing is chosen, and the system prompt is exactly what it is without this feature.

The chosen file is then read:

- **It loads:** its text goes into the project block.
- **It holds only whitespace and import lines:** it gets no section of its own, but its imports load.
- **It is empty:** nothing loads, and the default is not used. An empty `AGENTS.md` is how a project opts out of your default.
- **It can't be loaded:** nothing loads, a diagnostic says why, and the default is not used. Wisp never silently switches to other instructions.

`instructions.source` picks the behaviour: `auto` (the default) as above, `default` for your default only (for a repository you don't trust) and `none` for no instruction file. It resolves from `auto`, then `[instructions] source` in `config.toml` (`docs/endpoints.md`), then `--instructions`, the flag winning. It is logged as `session_started.instructions_source`.

## Reading

- Only regular files load. Symlinks are followed, then the target must be a regular file; a directory, FIFO, socket or device gets `not_a_file` and is never read. The plugin reads a file only after `workspace.kind-of` calls it regular, so a FIFO is never opened; the core reads a preset's files itself, checking the path before opening and again on the open handle, with a non-blocking open, so nothing can hang either.
- The text must be valid UTF-8 (`not_utf8` otherwise; there is no lossy decoding). A leading byte-order mark is stripped, line endings become `\n` and a trailing newline is ensured.
- A symlink loop, a permissions error or a dangling symlink is `unreadable`, never a failed session.
- There is no size limit (below).

Every problem is a diagnostic, `{ source, kind, message }`, never a failure. The kinds are `not_a_file`, `not_utf8`, `unreadable`, `import_missing`, `import_too_deep` and `large`. A missing project file is not a diagnostic.

## Imports

- `@path` imports a file. The `@` must start a line or follow whitespace, so `maintainer@example.com` is not an import. Imports inside code spans and fenced code blocks are ignored; write `` `@README` `` to keep the text literal.
- Sentence punctuation at the end (`.`, `,`, `;`, `:`, `!`, `?`, `)`) is not part of the path, so "Follow @docs/style.md." imports `docs/style.md`. A backslash keeps the next character, including a space. Unescaped whitespace ends the path, and a quoted path is not an import.
- Relative paths resolve against the importing file's directory as its name shows it (not a symlink target). `~/` expands to the home directory, and absolute paths are allowed.
- Imports nest at most 4 hops from the chosen file (`import_too_deep` beyond). A file reached twice, by a cycle or a repeat, loads once.
- A missing import is `import_missing`; the rest still loads.
- An imported file is rendered as its own section right after the file that imports it, depth-first, and takes that file's scope. The `@path` text stays where it was written.
- A project file's import that resolves outside the project root (symlinks resolved) is marked `external`, and doctor names it. Your default's imports are never marked.

## Pinning a skill

An import such as `@.agents/skills/guide/SKILL.md` pins a skill in these instructions
from the start. Project, role, preset and default files use the same import and
portable naming rules; no pin setting is added. See `docs/skills.md`.

## In the system prompt

The system prompt has two blocks, each with a 1h cache breakpoint:

1. **The base block:** Wisp's own prompt, byte-identical in every project.
2. **The project block:** present only when instructions loaded, or when extra instructions are on (below).

```
<instructions>
Instructions for this project follow, from its AGENTS.md. File names are relative to the project root. The user's messages take precedence over them.
<file scope="project" name="AGENTS.md">
...
</file>
<file scope="project" name="docs/style.md" imported_by="AGENTS.md">
...
</file>
</instructions>
```

With your default, the opening line reads "The user's default instructions follow. File names are relative to the user's Wisp config directory. …" and the scope is `default`.

- **Names are portable.** A file under its scope's root (the project root, or Wisp's config directory for the default) is named relative to it. Any other file is named from the imports as written, joined and normalised lexically; `~` and a leading `/` are anchors that `..` never removes. Wisp adds no absolute or `~` path of its own, so two clones of a repository produce the same bytes.
- **Where the model finds the files:** when a project file loaded and the root isn't the working directory, the `<environment>` block gains `Project root: ~/…`.
- **No file, no block:** the request is byte-identical to one without this feature.
- **Built once, at session start.** It is never edited in place: an edit made during a session reaches the model as an update in a later user message ("Changes mid-session"), and the system prompt only in the next session.

## Changes mid-session

When an instruction file changes during a session, the next user message carries an update, after `<environment>` and before the `Sent:` stamp:

```
<instructions-update>
These files changed since they were loaded. Each version below replaces the earlier one.
<file scope="project" name="AGENTS.md" path="~/Projects/Wisp/AGENTS.md">
...the whole new text...
</file>
<removed scope="project" path="~/Projects/Wisp/docs/old.md"/>
</instructions-update>
```

- **What counts as a change:** an edit to the chosen file or an import, an import added or removed, the project file created (it replaces your default) or deleted (your default comes back), and a change of `instructions.source`. An edit to your default while the project file is in use is no change. Only the files of the block count: an empty file, or one that holds only imports, is in no update by itself.
- **What goes in:** each changed file whole, never a diff, with its `~/…` path; a file no longer loaded is `<removed scope="…" path="…"/>`; one that can no longer be read gets the line "This file can no longer be read; its earlier instructions no longer apply." after it, and the chosen file stays chosen (your default isn't used instead). When nothing applies any more, the update ends with "No instructions apply from now on."
- **The check.** The plugin's `turn` render compares the current files with the section's committed snapshot. It runs `stat` on each file; only a file whose size or time changed, or whose time is within 2 seconds of its last read (git's racy-clean rule, against `clock.now`), is read and hashed again. The first check after a start or a resume reads each file once, since a start render has no clock.
- **When it counts.** The update's snapshot becomes the committed one when its message is saved. If the first request fails or is cancelled first, the next turn renders again from the old snapshot, so a later edit is folded into one update. A Retry resends the failed turn's update byte for byte, unless a head was built since, which already carries the files.
- **Heads.** A head renders the section too: from the project block's version to the target, the snapshot of the turn whose message is still unsaved (so a file edited after that turn's check waits for the next turn), or to the current files when there is none. A target logged by an older Wisp as the block itself, not a snapshot, is read as the block's project and default files. Its texts come from the plugin's store of the versions it has read, which each turn's check prunes to the committed snapshot, the new one, and those of earlier checks whose message isn't saved yet; a version it lacks, as after a resume, is read again and used if its hash matches, and otherwise the current file, or `<removed …/>`, goes in.
- **`instructions.source`** is a next-turn setting: a change applies at the next new turn (not a Retry), through an update; the system prompt keeps its block.
- **Resume.** The logged block is sent while its digest holds, and the window base stays the logged snapshot, so a file edited while Wisp was closed arrives as an update on the first new turn. A config change of `instructions.source` doesn't count: the session keeps its logged source.
- **Turned off,** the plugin sends no updates and the block stays as it was; turned back on, it sends what changed since the committed snapshot.
- **The record.** Each new update the model gets is logged as `instructions_changed` with its text, and `InstructionsChanged` drives the full screen's note `instructions updated · <path>[, …]`, or `instructions switched · <path>` when the source switched. A Retry's resend, and a head built to a target, show none. A head carries every change since the project block, but its record's changes, and so its note, are only those since the committed snapshot: a change already sent shows once.

## Preset instructions

A child of an agent preset whose `instructions` names a file, or a directory holding `AGENTS.md` (`docs/agents.md`, "Presets"), gets that file too, with scope `preset`; so does the main agent started from the `preset` setting (`docs/agents.md`, "The main agent's preset"), whose opening ends before the precedence sentence. The file is resolved against `config.toml`'s directory, `~/` or as an absolute path, checked when the session starts, and loaded again at each spawn through the same walker, imports included, so an edit reaches the next child. Its section follows the project block after a newline, with its own opening line, and its names are relative to the preset file's directory:

```
<instructions>
Instructions for your preset follow, from the file named by Preset instructions in <environment>. File names are relative to that file's directory. Your parent's messages take precedence over them.
<file scope="preset" name="AGENTS.md">
...
</file>
</instructions>
```

The child's `<environment>` names the file (`Preset instructions: ~/…`), `--instructions none` or `default` changes only the project's section, and the child's `instructions_loaded` lists the preset's files after the project's. A problem is worded for the preset (`preset planner's instructions are large: …`, `instructions ~/roles/x.md doesn't exist`); a preset's large instructions show at start only for the preset the session runs as, and `wisp doctor` lists every preset's; a file that stops loading turns the preset off at the next session start and refuses a spawn before it. A resumed child keeps its logged block.

A resumed main agent keeps its logged block too, but an edit to its preset's files since then reaches it as an update: its next new turn's message carries `<instructions-update>` with each changed file whole (`scope="preset"`, named from the preset file's directory, with its `~/…` path), after the instructions plugin's update and before the `Sent:` stamp, logged as the `core/preset` lead entry with its snapshot and shown as `instructions updated · <path>`. A head states it again, from the logged block's version, and a Retry resends it. The files are the ones loaded when Wisp started, so an edit made while it runs arrives at the next start.

Sessions without tools (jsonl output, `--tools none`) load instructions too: the CLI always detects the working directory, and the agent gets it through `Agent::with_workspace`. A session without tools still sends no `<environment>` block, unless its main agent started from a preset, whose block names the file there.

The agent emits `InstructionsLoaded` once, before any turn, and logs `instructions_loaded` (`docs/session-log.md`). jsonl prints the event; `wisp doctor` has an `instructions` section listing each candidate, import, its size and status, the total and any warning.

## Where it runs

- **The plugin.** `instructions` has one `system` prompt section, `instructions/instructions` (`in = ["core/state"]`, after `workspace/environment`). Its start render is the `<instructions>` block, read from `workspace.root()` and, for your default, `workspace-facts.config-dir()`; without a workspace nothing loads. Its snapshot lists `<sha256> <scope> <name>` for each file in the block, one per line, and is logged in `session_started.sections` with the others. A line of the older `<sha256> <name>` form takes its scope from the logged block (project when the block doesn't say), and a file of such a line that still loads unchanged is no removal. Its `turn` and `head` renders send updates ("Changes mid-session"). Reading uses `std::fs` after `workspace.kind-of`, with no `rustix` and no `std::env`, as the plugin-crate guard checks.
- **What it reports.** The host interface `instructions-report` (`wit/v0.2.0/prompt.wit`, granted only to the bundled `instructions` plugin, and unavailable in a sidebar render) takes `loaded`, the summary, the chosen scope and the project root, `warning`, a problem or `large` with its token count, and `changed`, an update's record with the text it describes. The core turns them into `InstructionsLoaded` and words `large` with the session's prices, so the text above is the core's. It relays the project root to `workspace-facts`, for the `Project root:` line, only when a project file loaded and the root isn't the working directory.
- **What the core keeps.** The extra text, placed before the plugin's block, a child's preset files, placed after it, and `instructions.extra`; and the main agent's preset section, `core/preset`, whose snapshot of the preset's files is logged in `session_started.sections` with the others (a log written before it takes the base from the logged block's `scope="preset"` files). Other `system` sections follow, as before.
- **Settings.** The plugin owns `instructions.source` and `instructions.warn_tokens` (its manifest's `settings`). It reads them through `settings.get`, which answers from the session: the logged source on a resume unless a flag changes it, and a child's own source from its spawn.
- **Resume.** A resume runs the start render once. The summary and project root are the fresh ones; while the logged block passes its digest check, the logged block is sent and the logged summary replaces the fresh one. A damaged digest, or a resume flag that changes `instructions.source` or `instructions.extra`, sends the fresh block and summary. When the logged start (`session_started.sections`, or `session_resumed.sections` after a fresh block) has no `instructions/instructions` snapshot, as in a log written before the plugin, the section's window base is `legacy` holding the latest logged block, read from the latest record that carries one, but only when that block holds a project or default file; otherwise the base is `none`. The plugin reads only the block's `scope="project"` and `scope="default"` files from it.
- **Doctor.** `wisp doctor`'s `instructions` rows come from the plugin's `doctor` export; the core adds the `large` warning row last.
- **Without the plugin** (removed from the registry) there is no files block and no `Project root:`; the extra text still comes alone.

## Extra instructions

`instructions.extra` (`--instructions-extra`, `--set instructions.extra=…`, `[instructions] extra` in `config.toml`; `none` or `autonomous`, default `none`; applies to the next session; `docs/settings.md`) adds a built-in text, compiled into Wisp, ahead of any instruction file. `autonomous` is for a task the model does on its own, with nobody to answer questions: meet each requirement literally, write the deliverable early, test beyond the examples, leave deliverable directories clean, size parallel work to the `Resources:` line, keep the turn open until every command whose result it needs has ended (waiting with `process`), and check the requirements before ending. Its text is `crates/wisp-core/src/instructions/autonomous.md`; it names Wisp's own features, so it changes with them, and holds no path, date, id, digit or machine value (`system_prompts_hold_no_dates_paths_or_ids` checks it).

It goes first in the second system block, followed by the project block after a newline when files load, and alone with `--instructions none` or no file:

```
<extra_instructions name="autonomous">
You are working on a task on your own. …
</extra_instructions>
<instructions>
…
</instructions>
```

- **Off means unchanged:** with `none`, the system prompt is byte-identical to one without this feature. The base block never changes, and the prompt keeps its two blocks and two 1h breakpoints.
- **Logged and restored with the block:** the text is part of `session_started.instructions` and its sha256, and `session_started.instructions_extra` names it (`docs/session-log.md`). A resume restores the logged block, so a later edit of the text never changes a resumed prefix; a differing `--instructions-extra` or `--set instructions.extra=…` on the resume command applies as one rewrite of the block, logged in `session_resumed.instructions_extra`, and the next resume keeps it; config never overrides the logged extra instructions, and a value equal to them changes nothing. Without a flag, a logged name this Wisp doesn't know gets a note: the logged instructions are kept, and the name would read as `none` only if a damaged log made the block load again. A different `--instructions` (or `--set instructions.source=…`) on the resume command does apply: it rebuilds the project block from that source with the session's extra text ahead of it, as a fresh load would, in one rewrite, and the next resume restores that block from `session_resumed.instructions` (`docs/session-log.md`, "Flags on resume"); so `--instructions none` on a session started with `autonomous` keeps the extra text alone. A flag equal to the logged source changes nothing.
- **The main agent only:** a child agent never gets it; its instructions are its task, its project's files and its preset's file.

## Size

Everything loads; no file is cut and no import skipped. When the loaded files come to more than 8,000 tokens (bytes ÷ 4, about 32 KiB), doctor and the event carry a `large` warning:

`instructions are large: about 20,000 tokens, ~$0.004 per request to read and ~$0.16 per cache write (claude-opus-5-5)`

The prices come from `pricing` for the session's model. When the model isn't priced, or the endpoint has `pricing = "none"`, the cost is left out. `[instructions] warn_tokens` in `config.toml` or `--instructions-warn-tokens N` moves the threshold (`8000`, `8k` and `1M` forms). Changed mid-session, it re-checks the loaded instructions at once and shows the warning when they are over the new threshold. Instructions too big for the model's window make every request fail with the provider's typed context-length error; `--instructions default` or `none` starts a session anyway.

## Trust

Security is deferred. The loader passes each file it is about to read through a `SourcePolicy`; the only one, `LoadAll`, admits everything. A later milestone adds skipping and asking, with a trusted-workspace list. Until then, `--instructions default` is the manual switch for a repository you don't trust.

The risks, stated plainly:

1. **A cloned repository's `AGENTS.md` is instructions from a stranger.** With no approvals, it can tell the model to run any command, and the bash tool will run it as you.
2. **Imports from outside the repository** let a repository's `AGENTS.md` pull any file you can read, such as `@~/.ssh/id_ed25519`, into the request sent to the provider and into the session log. Doctor names every such import, including one reached through a symlink.
3. **System-role authority.** In the system prompt, a repository's text carries more weight with the model than a user message would.
4. **Hidden text.** Zero-width or bidirectional Unicode can hide instructions from a human reader. Stripping it belongs to the later trust work.

## This repository's files

`crates/wisp-core/tests/instruction_files.rs` loads this repository's `AGENTS.md` and its imports through `wisp_instructions::load`, with an empty home, and fails on any diagnostic other than `large`, on any date (numeric, or a month with a day or year), and on any user- or machine-specific absolute path (`/home/…`, `/tmp/…`, `~user/…`, a Windows drive or a UNC path). Repository-relative, `~/` and placeholder paths, and system paths such as `/dev/null`, pass.

The same test checks the repository's skills, `.agents/skills/<name>/SKILL.md`, which never load into the system prompt. It checks each skill's one-line `name` (equal to its directory) and `description` (a plain scalar of at most 1,024 characters), that `AGENTS.md` names every skill and that every skill it names exists, and that every line passes the same date and path rules.
