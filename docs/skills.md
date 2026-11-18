# Skills

A skill is a how-to text the model reads when it needs it. It is an Agent Skills
folder with a `SKILL.md`, and may include scripts, references and other files.
The bundled `skills` plugin finds and reads them through `search` and `use`.

## Folders

Wisp looks in this order:

- `.agents/skills/` in the project;
- `~/.agents/skills/`, shared with other harnesses;
- `skills/` in Wisp's config folder.

Each child folder is one skill. A symlinked skill folder is followed, so a clone
elsewhere can supply a skill without copying it. The first skill of a name wins;
a project skill overrides a user skill. `wisp doctor` names the skipped copy.

The file starts with frontmatter between `---` lines. `name` and `description`
are required. Values may be plain, quoted, folded (`>`) or literal (`|`) text.
A name different from its folder warns but still loads. Missing descriptions,
unreadable files and broken frontmatter skip that skill and appear in doctor.
The repository's own skills keep their stricter house checks.

## Finding and reading

No list of skill names, descriptions or bodies is added to the system prompt.
`search` finds the descriptions with the same keyword ranking as tools. `use`
opens a skill by the full name that search returned.

An opened skill supplies its body without frontmatter, preserving its indentation,
its folder and up to 50 sorted file names, with an `and N more` tail. Dot entries
and `node_modules` are skipped. Other files' contents are not read. A home folder is written as `~/…`.
The model can read a named reference separately when the body calls for it.

A repeated open while the skill is still in view answers in one line, without
sending the body again. After a successful compaction or a rewind it can be
read again. A changed skill can be read again after the next folder scan.

The folders are scanned at every turn's opening. An unchanged turn keeps the
existing index and scans once. When skills changed or another source marked the
index dirty, gathering entries scans the folders again; there can be two scans
at that opening. Every later dirty gather also re-reads the folders.

A successful skill add or remove through `manage` marks the index dirty, so the
next tool call in the same turn sees the change, including in child agents.
Manual folder changes are found at the next turn's opening or a later dirty gather.

## Adding and removing

An agent adds a prepared local folder with `manage`: `action` is `add`, `kind`
is `skill`, `name` is the skill's name and `from` is its folder. A relative folder
is read from the project; `~/` starts at the home folder. The agent can fetch or
write the folder with its existing tools first.

Add checks `SKILL.md` with the same parser as the folder scan. Its name must match
both the source folder and `name`, and its description must be present. An
existing skill name is refused, not replaced or shadowed. The folder is copied
into `skills/` in Wisp's config folder, for every project. Only a complete copy
becomes available; an interrupted copy cannot remove a later installation. Scripts keep their
permissions; dot entries and `node_modules` are skipped. Nested symlinks and
special files are refused; a symlinked source folder itself is followed.

With `action` `remove`, `kind` `skill` and `name`, `manage` removes only a skill
from Wisp's config skills folder. It refuses a skill supplied by the project or
`~/.agents/skills/`; turn that skill off instead. A linked config skills root is
refused; removing a linked skill folder removes the link, never its target.
Neither action adds a question or changes the asking rules for on/off.

## `/skills`

The command lists only these folders' skills, with a count, their source
(`project` or `yours`), state and description. A server's prompt is still found
and read through search/use, but belongs to that server rather than this list.
Broken files appear in doctor instead. With no skills the answer names the
folders; while search is off it says how to turn it on in `/config`, Tools.
A skill named in `turned_off` reads `off` (muted), and while one is off a last
line says `turn on: /config, Turned off`.

## The nudge

The `before-turn` nudge compares the typed message with skill descriptions.
A clear keyword match adds at most three names to that turn's message:

```text
Skills that may fit: rust-code. Read one with use.
```

A common word alone adds nothing. Already-open and turned-off skills are left out, and the line
is absent while search/use are off.
It reads the existing current capability snapshot each turn, rather than keeping
only the build-time value. The hook's own switch is `plugins.skills.hooks.nudge`;
turning that hook off does not stop the folder scan. No nudge enters the system
prompt, and a skill search or open never rebuilds the cached prefix.

## Pins

To keep a skill always present, import its file from an instructions file:

```text
@.agents/skills/guide/SKILL.md
```

A project's `AGENTS.md` pins it for that project; a role or preset instructions
file pins it for that role or preset; the user's default instructions pin it
for every chat. Imports use the existing scope-relative naming and home-path
rules in `docs/instructions.md`. There is no new pin setting. A pinned skill is
part of the frozen system prompt from the start, unlike a body opened by `use`.
