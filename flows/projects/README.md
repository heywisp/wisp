# Projects flow

A Wisp flow for running a software project with agents. The session you talk to keeps a queue of tasks; each task gets a coordinator, which has it planned, reviewed, coded and checked, and a merge agent lands it.

- `FLOW.md`: the rules every agent follows.
- `roles/`: one file per role; each role's preset loads its file, which loads `FLOW.md` and your `preferences.md`.
- `presets.toml`: the presets for Wisp's `config.toml`.
- `questions.md`: what Wisp asks you at setup. Your answers become `preferences.md` and `privacy-terms.txt` here, both git-ignored and never committed; `git clean -fdx` deletes them, so keep a copy.
- `leaks.md`: the steps after a credential's value reached a model or left the machine (FLOW, Leaks).

## Using it today

Until Wisp Flows exists, one copy of this directory serves one project, the one whose main checkout's directory name is in its preferences' Project line, since the presets point at one copy. For a second project, use a second copy and a second config (`wisp --config …`).

1. Keep the copy where Wisp can read it: in a clone of its repository, or copied to `~/.config/wisp/flows/projects/`.
2. Tell the main session where the flow is: add this line to the project's `AGENTS.md` once everyone who works on the project uses the flow, else say it at the start of each session. "When `<flow>/preferences.md` exists, the main session follows `<flow>/FLOW.md` and that file. It runs from the flow's `main-session` preset, which loads both (FLOW.md, 'Main instructions'); started without it, it takes in both at the start of every session, after every compaction and after a change to either, read itself or quoted by its helper as the preferences say, in parts small enough for a report (a report over 16 KiB is cut)." Here `<flow>` is this directory's `~/` path.
3. Start Wisp in the project and ask it to set up the projects flow. It asks `questions.md`, has your answers written, and adds the presets, including `main-session`, to `config.toml` after your yes.
4. Quit Wisp, then start a new session in the project with `wisp --set preset=main-session`. A resume follows its logged preset, so it will not pick up this selection.

For a flow set up before this preset existed, add the `main-session` table from `presets.toml` to your Wisp `config.toml` beside the other flow presets, with `<flow>` replaced by this directory's `~/` path.

## Later, in Wisp Flows

- `wisp flow add` installs the flow, and its setup asks these questions with question cards, saving the answers per project.
- The Flow engine selects the main session's flow, so `AGENTS.md` needs no pointer and explicit preset setup is no longer needed.
- The steps, gates, slots and limits become kernel logic instead of instructions.
- A language flow (Rust, say) joins the coder and reviewer roles when the project uses that language.
