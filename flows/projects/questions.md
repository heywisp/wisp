# Questions

The main session asks these at setup. It first offers to take every recommendation at once; otherwise it asks one question at a time, recommended option first. Asked again later ("change the flow's preferences"), it rewrites only the lines it asked.

`preferences.md`, beside this file, gets a `# Preferences` heading, a `- Project: <name>.` line with the main checkout's directory name (written without asking), one line per answer in question order, and a `## Rules` heading for rules the user adds later. Agents load it as instructions, so it holds no dates, absolute paths, names or secrets. Question 1 goes into the presets instead; question 10 also writes `privacy-terms.txt`. Both files are git-ignored and never committed.

## Team

1. **Models and effort.** Which model and effort does each role get?
   - Recommended: every role on the main session's model at the efforts in `presets.toml`, except the helper, which only reads and writes notes, on a smaller, cheaper model of the same endpoint (a `model` line).
   - Cheaper: as recommended, plus the planner and plan reviewers at high, the code reviewers at medium, and the merge agent on the smaller model too.
   - Your own, per role, which may name another endpoint for a role, such as reviewers on a second provider.

   Setup lowers any effort a role's model doesn't take to the highest one it does. Writes the presets, not `preferences.md`.
2. **Slots.** How many tasks may run at once?
   - 1: one at a time, the cheapest and slowest.
   - 2 (recommended).
   - 4, for a user who reviews a lot of work in parallel.

   Writes `- Slots: 2.`
3. **Agents.** How many agents may work at once?
   - Recommended: 10 across the tree; a coordinator 3, or 5 for the top-priority task.
   - Lighter: 6 across the tree; a coordinator 2, or 3 for the top-priority task.

   Writes `- Agents: 10 across the tree; a coordinator 3, or 5 for the top-priority task.`

4. **Main reads files.** Does the main session read files and run short commands itself?
   - Recommended: yes, small ones, as a coordinator does; its helper does the big reads.
   - No: it uses only Wisp's agent and question tools, and its helper does every read and check. This suits an expensive main model, at the cost of slower turns.

   Writes `- Main reads files: yes.`

## Approvals

5. **Approvals.** Beyond the asking rule's cases (a) to (c), what do you approve yourself?
   - Recommended: L plans and the mockups of visible UI changes.
   - Nothing more: the main session approves clean plans and mockups itself and lists them under "Decided, not yet seen".
   - Every M and L plan, every mockup and every merge.

   Writes `- You approve: L plans and the mockups of visible UI changes.`
6. **Away.** When you say you're away, what happens to what you approve?
   - Recommended: the main session approves it once its reviews are clean and lists it under "Decided, not yet seen"; only the asking rule's cases (a) to (c) wait for you.
   - It waits for you, and other work goes on.

   Writes `- Away: the main session approves what you approve once its reviews are clean; only the asking rule's cases (a) to (c) wait.`

## Project

7. **Checks.** Which commands must pass before an MR merges?
   - Recommended: the ones the project's `AGENTS.md` or contribution rules name.
   - These commands (the user lists them).

   Writes `- Checks: as the project's AGENTS.md says.`
8. **Merging.** How does an approved MR land, and what runs after it?
   - Recommended: the merge agent pushes the branch, opens the MR and merges it once CI passes, as the project's contribution rules say; nothing runs after.
   - Local only: the merge agent merges into the main branch locally, with no remote.
   - You merge: the merge agent pushes, opens the MR and reports it opened; the main session asks you, or checks the host, before the clean-up step.
   - Any of these, with a command to run after each merge (reinstall, deploy). A command that needs a credential loads it itself; the line never holds one.

   Writes `- Merging: push, open the MR, merge once CI passes; nothing runs after.`
9. **Commit identity.** Whose name and email go on commits?
   - Recommended: whatever the repository's git config gives. It is public on every commit; the privacy scan skips its author and sign-off lines, though not other text that holds it.
   - A separate identity for this project, which you set yourself with `git config --local user.name` and `git config --local user.email`; Wisp never writes your name or email anywhere.

   Writes `- Commit identity: the repository's git config.` or `- Commit identity: the repository's local git config only; stop if it isn't set.`
10. **Privacy terms.** The privacy scan matches each line of `privacy-terms.txt` beside this file as a whole word, ignoring case. Setup's helper appends the machine's username, home directory and host name by a command that never reads or prints them, skips blank values and ones shorter than 4 characters, and says only which kind it skipped. Terms you add are kept as you typed them, short ones included.
   - Recommended: you also add your names and other private words yourself, in an editor.
   - Nothing beyond the machine's terms.

   Writes `privacy-terms.txt` only; and `- Key prefixes: <prefixes>.` if the keys and tokens your project uses (its providers', its code host's, its password manager's) start with prefixes other than `sk-`, and `- Canary marker: <marker>.` if your tests' fake keys use a marker other than `CANARY`.
11. **Mockups.** Does the project have a UI that needs mockups?
    - Recommended: none, for a project without a UI.
    - A terminal UI: text mockups at 40, 80 and 120 columns.
    - Another UI: mockups at the sizes you name.

    Writes `- Mockups: none.` (or the kind and sizes).
12. **Notes.** Where do the board, the detail files, the plans, the decisions log, the estimates and the reviews live?
    - Recommended: `notes/` in the main checkout, with a `notes/.gitignore` holding `*`, so nothing in it is committed.
    - Another directory under your home, written as a `~/` path, outside every worktree.

    Writes `- Notes: notes/ in the main checkout.`
13. **New rules.** How does a rule you give later get written down?
    - Recommended: the helper drafts it in the flow (by a docs MR, batched) or in `preferences.md`, one reviewer checks it for duplicates, contradictions and its place, and it takes effect once approved; a rule you call urgent applies at once and is reviewed after.
    - Lighter: the helper adds it under "Rules" in `preferences.md`, with no review and no MR.

    Writes `- New rules: reviewed for duplicates, contradictions and place before they apply; flow changes by docs MR, batched.`

## After the answers

- No preset loads before `preferences.md` exists, so the main session spawns a helper with `spawn: false` and the path of `roles/helper.md` in the brief to write setup's files.
- The main session shows `presets.toml`, including `main-session`, with `<flow>` replaced by this directory's `~/` path and question 1's answer applied, plus `[agents] max_running` set to at least the slots plus 2 and at least a coordinator's limit for the top-priority task (Wisp's default is 4), as a diff of Wisp's `config.toml`, and has it written after your yes, since it is outside the project. The default `agents.max_depth`, 2, is the flow's depth.
- Quit Wisp, then start a new session in the project with `wisp --set preset=main-session` (a resume follows its logged preset), then run `wisp doctor`: each preset should list its role file.
