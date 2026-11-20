# Wisp.Core architecture

*Design doc for review (MR 3, revision 3, 2026-10-21). Written by the planner role. Milestone 1 (MR 1) is merged and milestone 2 (MR 4, turn logs) is in review; nothing else is built yet.*

Files cited as `docs/plans/<name>.md` are past design plans kept outside this repository.

## What you're approving

**Merging this doc approves the direction, not every detail.**

- Each milestone still gets its own plan for your approval, and any **Proposed** item can be revised in that plan.
- Proposals that won't matter for a while will be re-confirmed when their milestone comes up: the remote half of P4, P9, P19, P21–P24 and the proactivity defaults P44–P50.

**Reading time.** This page takes about 5 minutes. The core sections (§2–§6, §10, §12, §13) take about 55 minutes. The whole document, appendices included, takes about 2½ hours. You don't need the appendices to approve.

**What you already decided** (recorded as D1–D12 in §13.1):

- **D1. Three layers.** The core agent loop, an orchestrator that combines agents into flows, and Jarvis: the long-lived front door that works alone and starts projects for bigger work.
- **D2. Projects.** Projects are long-lived and isolated. Each has its own instructions, knowledge, memory, history, team and secrets. A coordinator without write tools leads coder, designer, researcher and reviewer workers.
- **D3. A message bus.** Agents talk over a durable kernel message bus, top to bottom, through the coordinator. Urgent messages interrupt a running turn. By default, siblings don't message each other; they share an artifact board.
- **D4. Orchestration is code, not prompts.**
  - Working solo comes first.
  - One writer per workspace.
  - Fixes go back to the same coder thread.
  - Tests are a code gate.
  - Delegation depth is at most 2.
  - Budgets are nested and pause when they hit their cap.
  - **Amended by `docs/plans/subagents.md` (decisions 2 and 3):** depth is the setting `agents.max_depth` (default 2), and there are no budgets: costs are shown per agent and in total, and nothing stops on cost. Profiles become optional presets, which may carry instructions (decision 5).
  - Approvals come only from you or a named policy.
- **D5–D7.** The existing cost and caching rules, LLM-agnostic providers, and the contribution rules.
- **D8. Tunable.** Wisp is tunable:
  - a headless mode that emits the event stream as JSONL;
  - profiles as file bundles (prompts, skills, scripts, hooks) that favour skills and scripts over long prompts;
  - safety pinned in code that no profile or optimizer can weaken;
  - optional self-tuning outside the kernel.
- **D9. Review loop.** Review repeats until the reviewer says APPROVED, with the same reviewer and coder each round and no round limit by default. If a round makes no progress, the run pauses for your decision.
- **D10. Proactive Jarvis.** Jarvis is proactive: it follows up, sets up routines and schedules, accumulates memories, and does research proactively, helping you from accumulated context and history. You named Grok Bot, Muse and dots as inspiration.
- **D11. TUI first.** The real terminal UI comes right after the turn-logs milestone, on the in-process core, without waiting for the daemon.
- **D12. A comment-free code mode.** Wisp's coding agents can be set to write no code comments at all. The setting lives in the profile's code style, the coder role's doctrine states it, and DiffGate enforces it. Wisp's own repository uses it.

**What gets built, in order** (§12):

1. M1: bootstrap (merged)
2. M2: turn logs and formatted turns (MR 4, in review); a stepping stone toward durable history
3. M3: the terminal UI
4. M4: durable history
5. M5: tools and solo work with a test gate
6. M6: profiles, headless mode, evals, and a second provider
7. M7: the daemon, with model-free reminders and notifications
8. M8: projects with a coordinator (`solo` and `change` flows)
9. M9: Jarvis and multiple projects
10. M10: proactive Jarvis (heartbeat, follow-ups, routines, research), with its own security review
11. M11: parallel fan-out
12. M12: remote access
13. M13: self-tuning

**Your calls.** One sentence each; the details are in §13.2 and §10.9.

- **P1. Review policy.** AI review runs by risk (big or risky diffs, or when someone asks), with a per-project "always" switch; tests always run.
- **P2. Build order.** The list above.
- **P3. Default effort.** Medium for Jarvis, coordinators and coders. High for reviewers and for coders in the `change` flow. Low to medium for researchers. Evals will tune these.
- **P4. Connections.** Local clients use a private Unix socket. Phones pair with a key, and remote access is off by default.
- **P6. Per-step history.** History is kept per step rather than per turn, so a cancelled turn keeps the tool actions that really happened. This amends a `CLAUDE.md` rule.
- **P8. Direct routing to you.** Approvals and stall decisions go straight to you instead of climbing the agent chain. Agents can block them but never approve them.
- **P10. Project approval.** New projects need your approval, except small read-only throwaway ones, which a built-in policy approves.
- **P31. Coordinator tools.** "No write tools" means the coordinator gets read-only file access, no shell, and the messaging and board tools.

**New calls for proactivity:**

- **P44. Heartbeat.** On by default, but most checks cost nothing: a deterministic check runs first, and one small triage call, on the session's model, happens only when something is new.
- **P46. Background budget.** Proactive work gets $5 a month. At the cap, model-free reminders keep working.
- **P47. Autonomy ceiling.** Proactive work may observe and suggest. Acting needs your approval, except for reversible actions you pre-approved for a routine, applied to typed reader or report data.
- **P48. Memories.** Things you tell Jarvis to remember are kept automatically unless they're sensitive. Everything else waits in a review inbox.
- **P51. Separate milestone.** Proactive Jarvis gets its own milestone and its own security review.

---

**Status markers.**

- **Decided**: you, or `CLAUDE.md`, already settled it.
- **Proposed (Pn)**: this doc's recommendation; needs approval.
- **Open (On)**: not settled; a direction is suggested.

**Research references:**

- [MAO] `multi-agent-orchestration.md`
- [HL] `harness-landscape.md`
- [PC] `prompt-caching.md`
- [H2] `google-harness2.md`
- [PS] `pstack.md`
- [TUI] `tui-libraries.md`
- [PRO] `proactive-agents.md`

These reports are kept outside this repository.
Citations name the section, for example [MAO §4.3].

**Appendices** (reference only):

- A: Rust types
- B: event and command catalogue
- C: flows as data
- D: research traceability
- E: `CLAUDE.md` changes
- F: headless contract and pairing
- G: full acceptance criteria for each milestone

---

## 1. Goals, non-goals and design principles

### 1.1 Goals

1. **A headless kernel with thin clients.** The kernel runs sessions; the independent `wispd` library serves their command and event channels. Clients use one small protocol [HL §1.1, §7.1 A].
2. **Durable work.** Runs survive crashes, restarts and approvals that wait for days. A phone that reconnects misses nothing [HL §7.1 B, §7.3].
3. **Minimal user cost.**
   - Cache-first requests (D5).
   - Solo by default.
   - Budgets that pause at their cap.
   - Cost shown per turn, stage, role, run and project [MAO §4.7].
4. **Quality enforced by code.** Gates run tests, check schemas, scope diffs and require review resolution. A failure is never reported as success, and "inconclusive" is never a pass [PS §8.2].
5. **LLM-agnostic in practice.** A provider trait and a documented capability table, with the same behaviour and defaults on every model. Profile variants per model are deferred by the owner's 2026-10-23 rule [HL §7.1 G]. Every role uses the session's model by default (§6.1).
6. **Safe by default for an always-on assistant.** OS sandbox, egress control, secret scoping and provenance. Authority only ever narrows [HL §7.1 F, §7.3 #5].
7. **Tunable (D8).** A versioned, hashable, validated profile surface that optimizers can improve against evals [H2 §9].
8. **Proactive but steerable (D10).** Jarvis follows up, runs routines and schedules, accumulates memories and does research proactively. Code decides whether anything runs or interrupts you (§10).

### 1.2 Non-goals for this design and its milestones

- A general workflow engine, or any dependency on Temporal, Restate or similar [MAO Q7].
- An LLM classifier as the default router [MAO §4.4].
- Peer-to-peer agent meshes and A2A. Sibling messaging is off by default; opt-in pairs are an Open item (O16).
- Model-written *executable* flows. Declarative model-proposed flows may come later (P22).
- Multiple users or tenants. There is one user with many devices.
- Building the voice, desktop and phone clients. The protocol supports them.
- An optimizer inside the kernel (D8).
- Proactivity the user can't steer: follow-ups inferred on every turn, engagement-tuned nudges, or an LLM approver for background actions [PRO §5.8].

### 1.3 Design principles

1. **Code holds control; prompts hold judgment.** Every rule that matters has a code path that can fail [PS §8.3 #1, MAO §4.5].
2. **Events are the source of truth.** State, status, budgets and timers are projections of the log (`CLAUDE.md`).
3. **Cache first.** Frozen prefixes, changes appended as deltas, deterministic bytes, and one prefix per role profile (D5) [PC §5].
4. **One writer per workspace.** Writers get their own worktrees. Readers fan out freely [MAO §4.1 #2].
5. **Authority only narrows.** Children are never wider than their parents. Agents may deny but never approve. Hooks and profiles may only restrict (P8, P25).
6. **Every ending is typed.** Each turn, step, task and run ends in exactly one terminal state.
7. **Status comes from reading the log, never from a model turn** [PS §8.2].
8. **A small protocol.** Tens of methods, not hundreds [HL §7.3 #7].
9. **The kernel must earn its place.** It enforces or records only what a prompt cannot [PS §8.1].
10. **Tuning and safety are separate surfaces.** Behaviour lives in profiles. Safety lives in code and user-owned policy (D8).

---

## 2. The three layers

**Decided (D1).** L1 is the agent loop, L2 is the orchestrator, and L3 is Jarvis.

### 2.1 What each layer is

| Layer | What it is | Implemented as | Depth |
|---|---|---|---|
| **L1: agent loop** | One agent: one provider, a frozen prefix (model, system blocks, tools, effort, thinking), append-only history, and a tool loop | `Thread`, which today's `Agent` grows into (§11). There is exactly one loop implementation | n/a |
| **L2: orchestrator** | Per project: a **coordinator thread** (L1 without write tools, P31) plus the kernel's **flow engine**. Together they turn assignments into runs of `solo`, `change` or `fanout` | The `orchestrate`, `bus` and `project` modules, plus the coordinator role profile | coordinator 1, workers 2 |
| **L3: Jarvis** | One long-lived, **proactive** thread (D10, §10) and the front door. It answers or works solo, and creates or uses **projects** for bigger work. In the background it runs follow-ups, routines and research | The `jarvis` module, plus the Jarvis role profile | 0 |

**Decided (D4).** Depth is capped at 2 in code: Jarvis (0) → coordinator (1) → workers (2).

- Workers cannot spawn.
- `MAX_DEPTH` is a constant that nothing can raise.
- A spawn beyond depth 2 fails with `SpawnRefused{reason: depth}`.

### 2.2 Diagram

```
 Clients:   wisp CLI/TUI   desktop   phone   voice   bots              eval / optimizer
               \             |         |       |      /                       |
                +------------+---------+-------+-----+                        | wisp run --headless
                        |  wisp-protocol (JSON-RPC, P19)                      | (embedded kernel,
                        |  UDS locally | WSS + device pairing remotely        |  JSONL events, D8)
 +----------------------v-----------------------------------------------------v-------------+
 | wispd  /  Kernel                                                                         |
 |   commands --> reducer(state) --> decide() --> steps      events --> store --> clients   |
 |                                                                                          |
 |   L3  Jarvis thread (depth 0): answer / solo / proactive (D10)                           |
 |          |   down: Assign  Steer  Pause/Resume/Cancel  Broadcast                         |
 |          |   up:   Report  Escalate  ApprovalRequest  Progress                           |
 |   L2  Project "webapp"                          Project "taxes"  ...                     |
 |         coordinator thread (depth 1, P31 tools)        coordinator thread                |
 |         + flow engine (kernel code)                     + flow engine                    |
 |            |              |              |                                               |
 |   L1    coder         reviewer       researcher      <- worker threads, depth 2,         |
 |        [worktree      [read-only +   [read-only]        cannot spawn                     |
 |         write lease]   scratch]                                                          |
 |                                                                                          |
 |   Kernel services: message bus | artifact board (one per project) | budget ledger        |
 |   approvals + decisions | timers + scheduler | notification router (§10)                 |
 |   capability host (tools, hooks, policy, sandbox, egress proxy, secrets)                 |
 |   providers + capability table + pricing | profile loader                                |
 |   Store: home.db + one project.db per project (events, blobs, snapshots)                 |
 +------------------------------------------------------------------------------------------+
```

### 2.3 How the layers interact (worked example)

```
user (phone): "add passkey login to the webapp"                      -> Jarvis (wakes)
Jarvis turn: project "webapp" exists -> tool assign(webapp, brief, cap $8)
  bus: Assign -> webapp/coordinator (wakes after debounce)
coordinator turn: tool start_run(flow=change)   -> RouteDecided{source: model}, RunStarted{change@1}
  Plan: coordinator publishes Plan v1 -> SchemaGate pass -> ApprovalGate(plan)
  kernel: ApprovalRequested -> user's clients (phone, desktop); coordinator and Jarvis get a non-waking notice (P8)
user (phone): approve Plan v1 (subject hash matches)                  -> ApprovalDecided{by: user}
  Build: kernel spawns coder (depth 2, worktree lease) with Handoff{plan verbatim, refs}
  coder ... submit -> CommandGate FAIL -> TestReport to the SAME coder thread -> ... -> PASS
  review trigger: 420 changed lines, touches auth/**  -> review required (P1)
  reviewer (fresh context, artifacts only) -> ChangesRequested{3 findings}
  round 1: same coder fixes 2, disputes 1 -> CommandGate PASS -> same reviewer re-checks
           -> 2 resolved (progress), the disputed one re-asserted (dispute deadlock, §5.5)
  kernel: only the deadlocked finding is left, so the run pauses -> Decision straight to the user (P8)
  user (desktop): sides with the coder (reason recorded) -> same reviewer re-checks -> APPROVED
  ApprovalGate(final): diff, test report, findings incl. the user's ruling, cost per round -> approved
  Integrate (kernel; policy "manual", P13): branch wisp/webapp/run-7 ready -> RunEnded{completed, verified}
coordinator turn: report -> Jarvis (outcome Done: queued, Jarvis NOT woken); kernel notifies clients
```

Every line above is an event in `webapp`'s log, except the Jarvis lines, which are in Jarvis's home log. Any client can watch live or replay from a cursor.

### 2.4 Where each tier runs

**Proposed (P10, P11).**

| Tier | Where it runs | Who writes |
|---|---|---|
| `answer` | Jarvis, or a coordinator answering from project knowledge | nobody |
| `solo` (Jarvis) | Jarvis's own thread, in a workspace **no project owns** | Jarvis, holding that workspace's write lease |
| `solo` (project) | The coordinator delegates to **exactly one coder thread**, which owns the task end to end | the coder |
| `change` | Projects only | one coder per worktree |
| `fanout` | Projects only (M11) | one coder per worktree |

- **P11 (reconciling solo with D2).** The research's `solo` tier has the coordinator plan and edit itself [MAO §4.3]. Here the coordinator has no write tools, so in-project solo costs **one handoff**.
  - We accept that cost because the coordinator lives for weeks and must keep its context small.
  - The user's request travels **verbatim** as an artifact, so nothing is lost to paraphrase [MAO Q2].
  - A solo run that escalates to `change` **carries its coder thread over** as the Build thread. Its effort is raised by a cache-preserving delta where the model supports one (P3).
- **P10 (delegation through projects).** Jarvis delegates **only through projects**. One-off work, such as a research question that needs parallel readers, becomes an **ephemeral project** that archives itself after reporting. Parallel readers arrive with `fanout` in M11. Proactive research runs in a built-in, read-only Research project (§10.8).

---

## 3. The project model

**Decided (D2).**

- **Ownership.** A project is long-lived. It owns:
  - its instructions;
  - its knowledge;
  - project-scoped memory;
  - an event log with its runs and artifacts;
  - a team: role profiles, flows and budgets;
  - a workspace, with policy and a **secret scope**.
- **Isolation.** Projects are isolated from each other by default, and get only a curated, read-only slice of the user's global profile.
- **Team.** Each project runs its own coordinator (no write tools) with coder, designer, researcher and reviewer workers.
- **Concurrency and access.** Projects run asynchronously and report to Jarvis with summaries plus artifact references. Clients can attach to a project directly.

### 3.1 What a project owns, and where it lives

**Proposed (P5, P29).** Project data lives in Wisp's data directory. Repositories are *workspaces* that the project references.

```
~/.local/share/wisp/
  home/                   home.db (Jarvis's log, project registry), memory/ (git), profile/ (global user profile)
  projects/<project-id>/
    project.toml          name, lifecycle, workspaces, verify commands, review policy, budget, secret scope, global slice
    AGENTS.md             project instructions (always-on, budgeted, §6)
    profile/              overlay on the base profile bundle: skills, role overrides, flows (§6)
    project.db            the project's event log
    memory/               project memory (git; provenance per entry)
    worktrees/            one git worktree per writer, on wisp/<project>/<run>[-<n>] branches
~/.config/wisp/           config.toml, policy.toml (the PINNED policy layer, §6.2), credentials.json, .env (read only where no keychain is available), devices/
```

| Owned item | Representation | Notes |
|---|---|---|
| Instructions | `AGENTS.md` plus the profile overlay | In the project system block. Edits reach live threads as a `ContextDelta` |
| Knowledge | `KnowledgeSource`s: repos, directories, URLs, documents | Read through tools. Their index goes in each thread's preamble. A repo's own `AGENTS.md` loads only after the user **trusts** that workspace [HL §7.1 #27] (M5; until then they load as `docs/instructions.md` describes) |
| Memory | Files in git with provenance | Writes sourced from untrusted content go to a review inbox [HL §7.1 J.39] |
| Event log | `project.db` | The source of truth |
| Team | Role profiles pinned by hash, flows, a budget account | Resolved from the base bundle plus the overlay (§6) |
| Workspace | Repos or directories; a git worktree per writer | Sandbox profile, network allowlist, write leases (§5.7) |
| Secret scope | Secret *names* only | Values never enter prompts, logs or the sandbox (§9.4) |

### 3.2 Isolation

**Decided (D2), enforced by code.**

- **Two enforcement layers.** Project threads reach only their own project's workspaces, knowledge, memory, board and secret scope.
  - The capability host checks this: paths resolve relative to project roots, and secrets resolve through the scope.
  - The sandbox also checks it: it mounts only that project's worktrees.
- **Global slice.** `project.toml` lists which global-profile entries the project may see, such as coding style and timezone. They are rendered read-only.
  - Projects can't write global memory.
  - They can *propose* updates in a Report, and those go to the user's review inbox.
- **Cross-project access is off.** Read-only links between projects are Open (O1).
- **Jarvis's view.** Jarvis reads Report summaries, the artifacts reports reference, and status projections. It never reads project transcripts.

### 3.3 Lifecycle

**Proposed.**

```
            approve (user, or the named policy below)
  Proposed ------------------------------------------> Active <-----> Paused
     | reject                                            |  ^            |
     v                                                   v  | unarchive  |
  (discarded)                                          Archived <--------+
                                                         | delete (user only)
                                                         v
                                                       Deleted
```

- **Creation (P10).** Users create projects from any client. Jarvis can call `propose_project(spec)`.
- **Approval cost.** A project grants a workspace, secrets and budget, so **every proposal from Jarvis costs one user approval**.
  - **Proposed default policy `ephemeral-readonly`.** It auto-approves a proposal when *all* of these hold:
    - no write workspace;
    - no secrets;
    - a budget of $1 or less;
    - it archives itself.
  - Everything else waits for the user.
  - The user can turn the policy off.
- **Provisioning.** Validate the profile overlay (§6), trust the workspaces, confirm the verify commands, then open `project.db`.
- **Active.** "Idle" and "running" are projections over the project's runs.
- **Paused.** No new steps start, in-flight model requests finish and are committed, and the pause cascades to every thread.
- **Archived.** Read-only, and can be resumed.

### 3.4 How Jarvis and clients interact with a project

- **Jarvis → project.** Jarvis sends `Assign`, `Steer`, `Pause`, `Resume`, `Cancel` and `Broadcast` over the bus (§4). It receives `Report`s: a capped summary, a typed outcome, artifact references and the cost. It never sees transcripts.
- **Clients → project.** A client attaches directly, with live events and replay from a cursor. It can:
  - message the coordinator, acting as the project's principal;
  - Steer, Pause or Cancel any thread;
  - decide approvals and decisions;
  - read artifacts.
- **Keeping Jarvis informed (Proposed).** When the user acts directly on a project, Jarvis gets a non-waking `Progress`.
- **Status.** Project status is a **projection** built from the log, plus a "since last seen" catch-up view [PS §8.2].

---

## 4. The message bus

**Decided (D3):**

- **Message types.**
  - Down: Assign, Steer, Pause/Resume/Cancel, Broadcast.
  - Up: Report, Escalate, ApprovalRequest, Progress.
- **Hub and spoke.** Escalation moves up one level at a time.
- **Durable, ordered, idempotent.** Messages are recorded in the event log.
- **Delivered between turns** as appended messages, so caches stay warm.
- **Only actionable messages wake a model.** Progress only updates state.
- **Urgent messages preempt** a running turn. Which messages are urgent is defined by P7.
- **Everything is visible to clients.**
- **No sibling messages by default.** Siblings coordinate through the coordinator and the project's **artifact board**, which everyone in the project reads and only an artifact's owner writes.

### 4.1 Message types

The Rust sketch is in Appendix A.1.

| Message | Direction | Carries |
|---|---|---|
| `Assign` | down | A task, a typed `Brief` (validated before delivery), a flow hint, a budget grant (§5.8), and `on_report: Wake \| Queue` |
| `Steer` | down | Guidance text and artifact references |
| `Control` | down | `Pause`, `Resume`, or `Cancel{reason}` |
| `Broadcast` | down | Text to all of the sender's children, optionally a new standing-orders version |
| `Report` | up | Typed outcome, capped summary, artifact references, structured output (schema-checked), cost, memory proposals |
| `Escalate` | up | Kind (`NeedsInfo`, `Stuck`, `BudgetExhausted`, `PermissionNeeded`, `ScopeConflict`, `ProductDecision`, `Failure`), question, options, default, evidence |
| `ApprovalRequest` | up | Either **approve a subject** (an artifact plus its hash) or **choose an option** (a typed Decision). Each has deciders, a deadline and a timeout action |
| `Progress` | up | Phase and note. Updates status only |
| `Trigger` (**Proposed**, P38) | kernel → thread | A fired timer or watch: fire id, slot, payload references, taint. Wakes the thread like `Assign`; **never counts as approval or consent** (§10.1) |

- **Envelope.** Every message carries a `MessageId` (its idempotency key), `from` and `to` addresses, `Provenance` (`User`, `Agent`, `Kernel` or `Policy`), and `task` and `causation` links.
- **Sending.** Agents send through **kernel tools** (`report`, `escalate`, `request_approval`, `progress`, `assign`, `steer`, `pause`, `resume`, `cancel`, `broadcast`), so every message is a recorded step.
- **Two checks.** Tool allowlists keep workers from sending downward, and the router validates every message regardless.

### 4.2 Routing rules

1. **Hub and spoke (Decided).** An agent's message is valid only on a parent ↔ child edge.
   - The router rejects everything else with `MessageRejected{reason}`, including sibling ↔ sibling and worker → Jarvis.
   - The router does consult a per-project list of **sibling grants**. It is empty by default, and whether to allow opt-in pairs is Open (O16).
   - **Amended by `docs/plans/subagents.md` (decision 1):** an agent messages its parent, its own descendants and its siblings; never its grandparent or a sibling's descendants.
2. **Up goes to the direct parent only (Decided).** A parent escalates further by sending a new `Escalate` with `causation` set.
   - **Proposed (P9):** an escalation still unhandled at its deadline (because the parent is paused, for example) is forwarded one level up as `EscalationForwarded`.
3. **Down goes to direct children only (Decided).** `Broadcast` reaches all of the sender's children.
4. **Proposed: the user may address any thread.** The user is the root authority, and messages from the user carry `Provenance::User`. Since everything is visible, the user can answer a worker directly.
5. **Proposed: the kernel speaks as the coordinator.** Kernel messages, such as a flow assigning a coder or returning a `TestReport`, are sent *as the run's coordinator address* with `Provenance::Kernel`. This keeps the hub-and-spoke shape and records who really acted.
6. **Proposed (P8): approvals and decisions go straight to deciders.** This is an explicit **exception** to hub and spoke.
   - Only the user or a named policy can decide (D4), so the kernel registers the request and sends it directly to the user's approve-capable clients and to any matching policy.
   - Each supervisor in the chain gets a **non-waking notice**.
   - Supervisors may **deny or withdraw** a request, because that narrows authority. They can never approve.
   - **Stall decisions (D9)** are kernel-originated, not an agent's `Escalate`. D9 itself sends them to the user, so they follow this same path. The coordinator may attach a non-binding note.
   - **The alternative is hop by hop:** worker → coordinator → Jarvis → user, with the decision coming back down the same way.
     - It is uniform with D3.
     - It adds latency and up to two model wakes per approval, and no hop in between can decide.
7. **Proposed (P38): the kernel → thread `Trigger`** goes only to long-lived threads: Jarvis and project coordinators, including the built-in Research project's coordinator. **Never to workers**, which get work only through `Assign`. It carries `Provenance::Kernel` and never authority.

### 4.3 Delivery semantics

- **Durable.** A send is a tool step. `MessageSent` commits in the sender's log in the same SQLite transaction as that step.
  - A dispatcher writes `MessageReceived` into the recipient's log, deduplicating on `MessageId`, then marks the outbox entry done.
  - Delivery across databases is at-least-once with an idempotent receive, which makes it effectively exactly once.
- **Ordered.** First-in, first-out per sender → recipient pair. The recipient's inbox sequence fixes the order its thread sees.
- **Idempotent.** Message ids are derived:
  - agents: `H(thread, step, call_index)`;
  - kernel: `H(run, step, kind)`;
  - users: a client-supplied id.
- **Appended, never inserted.** Delivered messages become **user-role content at the tail**: one deterministic `<wisp:message from kind>` block per mail (amended by `docs/plans/subagents.md` §5; this replaces one `<wisp:inbox>` block per delivery point). They never go into the system prompt and never edit history [PC §5.2.1]. There is one inbox format, the same for every model.
  - A user's own steer is plain text after the last `tool_result`, as Anthropic advises for mid-turn user input, then one text block marking it sent while Wisp worked (`STEER_NOTE`), after the last of the batch's messages and before any mail; a queued message written while a turn ran gets the same mark in the turn it opens (`docs/plans/steer-sent-note.md`). The framing of messages from agents and the kernel is set by `docs/plans/subagents.md` (`docs/plans/steering.md` §2, §8.5).

### 4.4 Delivery points, waking and preemption

**Proposed (P7, P15).**

- **Urgent messages (P7).** Only `Cancel` (and `stop_agent`) preempts. The rest arrive at a turn end or a step boundary.
  - **Amended by `docs/plans/subagents.md`:** agent mail never preempts; it waits for the step boundary, as a user's steer does (`docs/plans/steering.md`).
- **Step boundary.** The moment before the next model request, which includes between iterations of a tool loop. Both turn ends and step boundaries are append-only, so both keep the cache warm.

| Message | Delivered at | Wakes an idle recipient? | Preempts a running turn? |
|---|---|---|---|
| Assign | Turn end. A worker holds one task at a time; extra Assigns queue | **Yes** | No |
| Steer | Step boundary (amended by `docs/plans/subagents.md`) | **Yes** | No. Mail from above keeps a running turn going until answered |
| Pause | Step boundary | n/a | No. The in-flight request finishes and is committed, then nothing new starts. Cascades down |
| Resume | Immediately | Yes, if work is pending | n/a |
| Cancel | Immediately | n/a | **Hard.** Abort the request, kill tool process trees, record `EffectUnknown` for any non-idempotent effect already started (§7.3), and end the task `Cancelled`. Cascades down, in code |
| Broadcast | Step boundary for active threads; next wake for idle ones | No | No |
| Report (terminal) | Parent's turn end | **Yes** for coordinators. For Jarvis, only if `on_report = Wake` or the outcome needs attention; otherwise it queues | No |
| Escalate | Parent's turn end | **Yes** | No |
| ApprovalRequest | Straight to deciders (P8); supervisors get a notice | Supervisors: no | No |
| Progress | **Never enters model history.** It updates the status projection, which tools can read | No | No |
| Trigger (P38) | Turn end, as a `<wisp:trigger>` block | **Yes** | No |
| User message | Steer: the next step boundary of the running turn; it keeps the turn going until answered. Queue: the turn's end; after a stop or failure queued messages are held until the user's next send or `Release` | Yes | No. Neither mode preempts. Stop and failure return waiting steers to the sender |

- **Debounce (P15).** When a waking message reaches an idle thread, the kernel waits briefly, then starts **one** turn with everything pending.
  - Window: **one 2 s settle for every wake source**, mail and process ends alike (amended by `docs/plans/subagents.md`; this replaces per-role windows of 1 s, 3 s and 0.5 s).
  - Precedents: OpenClaw [HL §3.12] and pstack's "completions are queue events" [PS §8.2].
- **The kernel notifies; models wake only to decide.** A project's finished Report reaches your devices without a Jarvis model call.
- **Spawning never blocks a turn (P26).** `assign` returns immediately, and results arrive later as messages [MAO §4.10 #4].

### 4.5 Visibility

- **Clients.** Every message is an event.
  - A project subscription shows all of that project's messages and threads.
  - A `home` subscription shows Jarvis ↔ coordinator traffic.
- **Agents.** An agent sees only its own history, its inbox, the project board and the status its tools expose. It never sees another thread's transcript.

### 4.6 The artifact board

**Decided (D3): one board per project; the owner writes, everyone reads.**

- **Artifacts.** Each is a content-addressed blob plus typed metadata: id, a version chain, kind, owner, schema, a summary and a hash.
- **Kinds:** `Plan`, `Brief`, `Diff`, `TestReport`, `Findings`, `ReviewVerdict`, `Triage`, `ResearchReport`, `Design`, `Note`, `File`, `StallReport`, `Checkpoint`.
- **Owners.** An owner is a thread, the **kernel**, or the user.
  - Gate outputs such as `TestReport` are kernel-owned, so coders can't forge them.
  - A user edit, such as "approve with edits", makes a new version with user provenance.
  - Only the coordinator can transfer ownership.
- **Publishing.** `board_publish` checks ownership, then emits `ArtifactPublished`. Versions are superseded, never overwritten.
- **Notification.** Publishing never wakes anyone.
- **Transcripts are never artifacts.** That is how a reviewer stays independent of the coder's reasoning [MAO Q5].

---

## 5. Orchestration

**Decided (D4).** Orchestration is kernel logic, not prompt instructions:

- typed role profiles;
- flows as state machines;
- gates in code;
- typed handoffs;
- isolation;
- budgets;
- every step in the event stream.
- **Amended by `docs/plans/subagents.md` (decision 2):** no budgets: costs are shown per agent and in total, and nothing stops on cost.

D4 adapts the research:

- solo by default, with the team flow as an escalation tier;
- one writer per workspace;
- fixes go back to the same coder thread;
- tests as a code gate;
- depth ≤ 2 in code;
- nested budgets that pause at the cap;
- approvals only from the user or a named policy.

**Decided (D9).** The `change` review loop runs until the reviewer gives an explicit, typed **APPROVED**.

- The **same reviewer thread** and the **same coder thread** persist across rounds.
- There is **no round cap by default**. A cap is an optional setting on the flow or profile.
- A round that makes no progress pauses the run and asks the user to decide; then the run continues. The **ProgressGate** implements this. What counts as "no progress" is defined only in P28b (§5.5).
- Each round's cost is visible, and the run's budget still applies.

### 5.1 Tiers

| Tier | Run | Flow | Gates | When |
|---|---|---|---|---|
| `answer` | none | Plain turn, read-only tools | none | Questions with no side effects |
| `solo` | `Run(solo)` | One writer (§2.4) | CommandGate when files changed; DiffGate; BudgetGate; review only by risk (P1) | **Default for anything actionable** |
| `change` | `Run(change)` | Plan → approve → build ⇄ verify → review ⇄ fix → approve → integrate | All gate kinds | Explicit intent, a model proposal, or an objective signal |
| `fanout` (M11) | `Run(fanout)` | Decompose → pilot → map → barrier → merge → synthesize | Per-item gates; every item accounted for; merge gate | Many independent items |

### 5.2 The router

**Proposed, after [MAO §4.4].** There is a per-layer `Router` trait, and **no separate classifier call** by default.

1. **Explicit intent wins.** It comes from client commands (`/change`, `/fanout`, `/quick`, `/project <name>`), the `flow` hint on an Assign, or a saved flow. Logged as `RouteDecided{source: user}`.
2. **Default.** `answer` for questions, `solo` for actionable work.
3. **Model self-escalation** happens through tools inside the normal loop: `propose_project` (Jarvis), and `start_run` / `propose_flow` (coordinator).
   - Escalating to `change` needs no extra approval, because its first gate is the plan approval, which shows the estimate.
   - `quick` (skipping the plan approval) is only available on the user's explicit request.
4. **Objective signals offer, never force.**
   - Three CommandGate failures, or a ProgressGate trip → offer a re-plan, `change`, or a stronger model.
   - A large or risky diff → review (P1).
   - Too many items → `fanout`.
5. **Overrides are recorded** (escalations, de-escalations, cancellations) for offline tuning. Collapsing `change` to `solo` skips the remaining approvals, never the CommandGate.

### 5.3 Kernel primitives

These are Proposed; that the kernel has them is Decided (D4). The table and Rust sketches are in **Appendix A.3**.

| Primitive | What it is |
|---|---|
| `RoleProfile` | A resolved, hashed role |
| `Flow` | A state machine held as versioned data |
| `Run` | A pinned flow instance |
| `Step` | A durable unit of work |
| `Thread` | An L1 agent |
| `Artifact` | A board entry |
| `Handoff`/`Brief` | A typed brief; the kernel **refuses to spawn** if a field is missing [PS §8.2] |
| `Gate` | A check evaluated in code |
| `Approval`/`Decision` | Durable waits that only the user or a policy can resolve |
| `BudgetLedger` | Hierarchical accounts |
| `Limits` | Depth, concurrency and retry caps |
| `Workspace` | A worktree with a lease and path claims |

### 5.4 Gates and verdicts

**Proposed.**

- **What every gate does.** It emits `GateEvaluated`.
- **Results.** `Pass`, `Fail{reason}`, `Inconclusive{reason}`, `NotConfigured`, or `Stalled{signals}`.
  - `Inconclusive` never passes.
  - `NotConfigured` leads to an `Unverified` verdict.
- **No model calls.** Gate code never calls a model. ReviewGate reads a *typed verdict* produced by a reviewer thread.

The gates:

- **`CommandGate` ("verify").**
  - It runs the project's format, lint, build and test commands in the sandbox, with network off unless allowlisted.
  - It parses exit codes, plus structured output when available.
  - It stores a kernel-owned `TestReport`.
  - **It never asks a model whether tests passed** [MAO §4.2].
  - If no command is configured, the result is `NotConfigured`.
  - Commands come from user-owned config, or from detection plus a one-time user confirmation. They are never read silently from the repo.
- **`SchemaGate`.** It validates a role's output against its schema. On failure it retries with the validator's error (twice), then fails with a type.
  - **Amended by `docs/plans/subagents.md` (decision 6):** an agent's `report` is free text. Typed outputs and SchemaGate come back with the flow task (decision 4).
- **`DiffGate`.** It checks that the diff is non-empty when a change was expected and that changed paths stay within the plan's scope. **Edits to tests or acceptance criteria are flagged**, which forces a review and is shown at the final approval. When the profile's code style sets `comments = "none"` (D12), it also fails on any added comment, found with each language's comment syntax, and the failure goes back to the same coder thread.
- **`ReviewGate`.** It passes only on `ReviewVerdict::Approved`. The verdict is bound to the diff's patch-id hash, so a new diff voids it [PS §8.2].
- **`ProgressGate`.** Detects stalls from objective signals (§5.5). It returns `Stalled{signals}`.
- **`BudgetGate`.** It runs before each step and pauses at the cap (§5.8).
- **`ApprovalGate` / `DecisionGate`.** Durable waits with nothing in flight [MAO Q6].

**Verdicts on completion (P28).** A completed run carries `Verified{grade: TestsPassed | Reviewed}` or `Unverified`.

- `Unverified` means no check was configured. It is reported loudly and never shown as verified. Projects without checks can still finish.
- A failing gate never produces `Completed`. The run ends `Failed{kind: GateFailed}`, or stops at a decision.
- `change` requires a check. Without one, the review is forced and the final approval says "Unverified".

### 5.5 The review loop (D9) and stall decisions

**Decided (D9); the definitions are Proposed (P28b).**

- **Rounds.** A round is: coder triage (`Fixed` or `Disputed{reason}` per finding) → CommandGate → **the same reviewer** re-checks its open findings and the changed hunks → typed verdict → **ProgressGate**, which runs *after* the verdict so it can see it.
- **No progress** means at least one of:
  - **nothing resolved:** not one open blocking finding was closed this round;
  - **recurrence:** a finding resolved earlier reappears, matched by fingerprint;
  - **oscillation:** the diff returns to a previously seen hash, or a hunk reverts to an earlier version.
  
  A round that resolved anything without recurrence or oscillation *is* progress. Fixing 4 of 5 findings never pauses. The first verdict is never a stall.
- **Dispute deadlock is a separate signal.** The coder disputes a finding and the reviewer re-asserts it.
  - The kernel parks **that finding** as a typed user Decision ("side with the coder / side with the reviewer").
  - The loop **keeps going on the other open findings**. The run pauses only when the deadlocked findings are all that remain.
  - A finding the user dismisses is never re-raised as blocking. One the user upholds can't be disputed again.
- **When the ProgressGate trips**, the run pauses and the kernel sends a **Decision** with a `StallReport` straight to the user (P8). The options depend on where the stall happened:

| Option | Offered when | Then |
|---|---|---|
| Guide (with text) | Always | Back to the stalled loop's retry state (build_again, fix or plan), with the text delivered as a Steer |
| Side with the coder | Review stall or deadlock | The disputed findings are dismissed with the user's reason; the reviewer re-checks |
| Side with the reviewer | Review stall or deadlock | The disputed findings become mandatory; the coder can't dispute them again |
| Re-plan | Always | Back to Plan, and then to plan approval |
| Stop | Always | `RunEnded{cancelled}` |

- **Stalls in `solo`** also offer "escalate to `change`" (carrying the coder over, P11) and "retry on a stronger model".
- The full state machine is in Appendix C.

### 5.6 Default flows

**Amended by `docs/plans/subagents.md` (decision 4):** running the `change` flow as kernel data (Appendix C) is a later task.

**`solo`.** This flow runs inside a project. Jarvis's own solo work is the same flow with `thread = current`.

```
Assign --> route: solo --> Brief (coordinator turn; the user request is forwarded verbatim)
                             | Handoff validated (refuse-to-spawn on missing fields)
                             v
                        Build: ONE coder thread, own worktree, write lease
                             | submit
                             v
               +------- CommandGate + DiffGate --------+
               | Fail: TestReport -> SAME coder        | Pass or NotConfigured
               |       (2 retries)                     v
               |                          [review only if the risk policy says so (P1),
               | 3rd fail or Stalled                    then the D9 loop]
               v                                        |
        Decision to user: guide / change /              v
        stronger model / stop                    Report(Done{Verified | Unverified}) -> up
```

**`change`.** The escalation tier, with the D9 loop. The state machine is in Appendix C.

```
Intake --> [Explore]* --> Plan --> ApprovalGate(plan) --> Build <=> CommandGate + DiffGate + Progress
           (read-only      ^                                (carried or new coder; Fail -> Build; 3rd fail in a row -> Decision)
            researchers)   |                                           | Pass / NotConfigured (forces review)
                           |                                           v
                           |                               [review trigger (P1)]
                           |                                           |
                           |                     fresh reviewer --> verdict --> ProgressGate
                           |                                           |
                           |             ChangesRequested + progress   |   APPROVED
                           |           +-------------------------------+---------------+
                           |           v                                               v
                           |   Fix (SAME coder: fix or dispute)          ApprovalGate(final) --> Integrate
                           |           | CommandGate (3rd fail -> Decision) (diff, TestReport, findings,
                           |           v                                  rulings, cost per round)
                           |   SAME reviewer re-checks --> verdict --> ProgressGate --> (loop)
                           |
           re-plan <-------+---- Stalled anywhere -> run pauses -> user Decision (§5.5) -> continues
```

- **Plan.** The coordinator writes the `Plan` artifact for medium work; a planner thread does it for large work.
  - It holds checkable acceptance criteria, a test plan, a risk class and an estimate.
- **Build.** The coder carried over from `solo`, if there was one (P11); otherwise a new one.
  - It gets the plan **verbatim** and research **by reference**.
- **Integrate (P13).** A kernel step with an idempotency key and a write-ahead intent.

**`fanout` (M11).**

```
Assign --> Decompose (coordinator: Items artifact, disjoint path claims per writer)
       --> Pilot one item end to end --> Map: <= 4 concurrent fresh workers, own worktree if writing,
           per-item CommandGate --> Barrier: EVERY item done | failed | not covered
       --> Merge gate (claims disjoint, clean merge, CommandGate on the merged tree; conflict -> Escalate)
       --> Synthesize (coordinator) --> [ApprovalGate(final) if anything was written] --> Report
```

### 5.7 Workspaces, writers and integration

**Proposed.**

- **Leases.** Each workspace (worktree) has one write lease with a fencing token. Write tools and the sandbox's writable mount follow the lease.
  - Reviewers get a read-only view, plus a **throwaway scratch copy** for reproductions (P12).
- **Only coders write workspaces (P12).** Designers, researchers and reviewers publish artifacts. Design tools are Open (O5).
- **One writer run per repo per project (P27).** Others queue. Parallel writers are allowed in `fanout`, or by opt-in, but only with disjoint path claims and a merge gate.
- **Integration (P13).**
  - **Agents never run `git commit` or `git push`**; policy denies both, as D7 does for this repo.
  - The kernel commits on `wisp/<project>/<run>` branches.
  - Delivery is set per project: `manual` (the default; the Report carries the branch, diff and verdict), `local-merge`, or `mr`. Anything that pushes needs an approval.
  - Commit titles are Conventional Commits.

### 5.8 Budgets

**Amended by `docs/plans/subagents.md` (decision 2): no budgets.** Costs are shown per agent and in total, and nothing stops on cost. The design below is kept for reference only.

**Decided (D4): nested budgets that pause at the cap. Proposed (P17): the details.**

```
Global (user, per month)
 +-- Jarvis (its own turns, per month)
 +-- Jarvis.Background (heartbeat, routines, intents, research; P42, §10.7)
 |    +-- Research project (built in, §10.8): its runs' grants
 +-- Project (per month)
      +-- Run (cap set by the Assign's grant, drawn from the project account)
           +-- Thread (grant drawn from the run by the coordinator's Assign) --> Steps
```

- **Grants.** Every Assign carries a `BudgetGrant`, so no child ever starts with a fresh budget [MAO Q2].
  - Jarvis → project: the grant caps the new run and is drawn from the **project's** account.
  - Coordinator → worker: the grant is drawn from the **run's** account.
- **Units.** Integer micro-USD for the ledger (today's `f64` `Cost` stays for display), plus tokens, wall time and agent count.
  - Spend is folded from `StepCompleted{usage, cost}`. Cost is computed when the step is recorded, so replays are stable. Across chats, read the same charges from `spend/` (see `docs/session-log.md`, "The spend record"), not a second ledger file.
- **Checks.**
  - The BudgetGate runs before each model step.
  - **Overshoot is at most one response per concurrently running thread**, because threads already streaming finish their current response.
- **At the cap,** the account pauses its runs (`RunPaused{budget}`) and escalates `BudgetExhausted`.
  - Only the user or a named policy can raise the cap.
  - **A paused run becomes terminal `BudgetExhausted` only when:**
    - the user (or a policy) declines to raise the cap; or
    - the project's optional `budget_pause_deadline` passes. It is unset by default, so the run stays parked and reminders are sent.
- **Unpriced models** are capped on tokens only. A run that uses one must have a token cap, or it won't start.
- **Suggested starting caps:**
  - per run: solo $2, change $10, fanout $20;
  - per project: $50 a month;
  - globally: $100 a month.
  
  Hitting any cap pauses work rather than failing it.

### 5.9 Limits (defaults)

**Proposed (P18)**, except where marked Decided. These are tunable, but never past the pinned ceilings (§6.2).

| Limit | Default |
|---|---|
| Delegation depth | **2, the setting `agents.max_depth`** (amended by `docs/plans/subagents.md`, decision 3: a child is Wisp with its parent's capabilities, narrowed by its spawn; profiles become optional presets, which may carry instructions, decision 5) |
| Concurrent children per coordinator | 4 [MAO §4.8] |
| Agents per run | Warn at 10 or at a projected 1.5M tokens; hard cap 50 |
| Consecutive CommandGate failures before a Decision | 3, in the build loop and in each fix round |
| Review rounds | **No limit (Decided, D9).** `max_review_rounds: Option<u32> = None` |
| Schema retries | 2 |
| ProgressGate | Build loop: stall after 3 non-progressing attempts (the count decays on progress). Review loop: the no-progress rule and dispute deadlock (§5.5) |
| Wake debounce | One 2 s settle for every wake source (amended by `docs/plans/subagents.md`) |
| Child report summary cap | 2,000 tokens [MAO Q3] |
| Tool "ask" with no client attached | The run is parked (no compute) and the user is notified |

---

## 6. Profiles and tunability

**Decided (D8):**

1. A **headless mode** that emits the protocol event stream as JSONL.
2. **Profiles as file bundles** (prompts, skills, scripts, hooks) that favour skills and scripts over long prompts.
3. **Safety pinned in code-enforced policy** that no profile or optimizer can weaken.
4. **Self-tuning** as an optional job outside the kernel.

**Proposed (P30):** the bundle specifics in §6.1. These are typing, load-time validation, one always-on cap in config (instructions not capped; a size warning instead), hashes on every run and step, and the AGENTS.md / SKILL.md / hook conventions.

### 6.1 The profile bundle (the tunable surface)

**Proposed (P30).** A bundle is a directory with a typed manifest. It maps onto Harness²'s eight components [H2 §2.2].

```
my-profile/
  wisp-profile.toml        manifest: name, version, schema, components, permission REQUESTS
  AGENTS.md                base always-on instructions
  tool-guidance.md         always-on tool guidance
  roles/<role>/ROLE.md     role doctrine (frontmatter: model ref (optional; built-in roles name none), effort, tools, context mode, output schema)
  skills/<name>/SKILL.md   Agent Skills: name + description searchable, body read on demand; may ship scripts
  scripts/                 helper scripts, run through the sandboxed shell tool
  hooks/hooks.json         Claude Code hook schema (PreToolUse, PostToolUse, UserPromptSubmit, Stop, ...)
  memory/                  memory seeds
  flows/                   optional flow definitions (validated against the gate invariants)
```

- **No per-model variants.** The `variants/<model-family>/` layer (D6's per-model variants: prompt variants, edit-tool preset and effort) is deferred by the owner's 2026-10-23 rule. Profiles stay per role and are the same for every model; user overrides in `config.toml` stay allowed.
- **Every role uses the session's model by default** (owner, 2026-10-23). Every role, side job and one-shot call (triage, reader, consolidation, compaction, research scanners and synthesis) runs on the model of the session it serves; background work serves Jarvis's session. A model per role or agent type (coder, researcher, a writing role and so on), set by the user, is a later config option. The frontmatter's model ref and `RoleProfile.model` are its seam: a role that names no model gets the session's model. Built-in roles name no model.
- **Layering and hashing.**
  - Layer order: built-in defaults, then the user's base bundle, then the project overlay, then the role.
  - The resolved content is canonicalised and Merkle-hashed.
  - The **bundle hash** is recorded on `RunStarted`. The **role-profile hash** is recorded on `ThreadSpawned` and on every `StepScheduled` and `StepCompleted` [H2 §9.1 #1].
- **Validated at load.** Problems become `LoadDiagnostic` events, written so a model editing the profile can fix them from the message alone. Nothing is dropped or truncated silently [H2 §9.1 #3]. The checks:
  - frontmatter against its schema, with exact-name references;
  - every skill is reachable;
  - every referenced script exists;
  - every hook event is known;
  - no path escapes the bundle;
  - flows are valid;
  - caps are respected.
- **One always-on cap.**
  - The cap covers role doctrine, tool guidance and the skill index.
  - It is one value in config, defaulting to **8k tokens per role**, applied the same way on every model. A role's frontmatter or the user's `config.toml` can override it.
  - Going over is a load **error**, never a truncation. Codex silently truncates `AGENTS.md` at 32 KB [H2 §2.2].
  - **Instructions are not capped** and are never a load error: a repository's `AGENTS.md` and its imports, and the user's default, load whole, with a size warning above a threshold (`[instructions] warn_tokens`, 8k by default; `docs/instructions.md`).
- **Skills and scripts over prose.** Removing skills cost 5.3 points, scripts 3.7, and prompt text 1.5 [H2 §6, §9.1 #2].
  - Skill names and descriptions are found by search, not listed in the cached prefix.
  - Skill bodies arrive through `use` as appended results, so they don't break the cache; an instructions import pins one from the start.
- **Hooks are kernel middleware.**
  - They run sandboxed.
  - They may block, add context, or trim output.
  - A hook's "allow" can never override a pinned deny or ask (P25).

### 6.2 What is pinned (not tunable)

**Decided (D8):** safety lives in code-enforced policy that no optimizer or profile edit can weaken. This answers Harness² issue #94 [H2 §4, §9.2 #2].

| Pinned (code, plus the user-owned `~/.config/wisp/policy.toml`) | Tunable (profile bundle) |
|---|---|
| Permission rules, sandbox profiles, egress allowlists, secret scopes | Role doctrine, `AGENTS.md`, tool guidance |
| Gate semantics: CommandGate on file changes, typed APPROVED, approvals only from the user or a policy | Which skills, scripts and hooks exist (hooks can only narrow) |
| Limit ceilings (depth 2, the hard agent cap) and budget caps | Effort, context mode and output schema per role; a model per role later (the session's model until then, §6.1) |
| Protected paths: all of `.git`, Wisp's config and data, profile directories | Flows that pass the gate invariants |
| Provenance rules; untrusted content can't write memory | Memory seeds |
| Proactivity ceilings: the tier ceiling and act-allowlist, quiet hours, notification urgency caps and rate limits (§10) | Routine and triage prompts, triage thresholds |

- **Permission requests are intersected with the pinned policy.** A profile's permissions are only requests. The kernel intersects them with the pinned policy and records the effective set.
- **Agents can't edit profiles.** The sandbox mounts profile directories read-only.

### 6.3 Headless mode (a Harness² substrate)

**Decided (D8). Proposed details (P24).**

```
wisp run --headless --profile <dir> --workspace <dir> --model <provider/model> \
         --task <file|-> [--budget-usd N] [--max-turns N] [--state-dir <dir>] [--approvals <policy>|deny] --events jsonl
```

- **Embedded kernel.** No daemon is involved.
- **Output.** Durable `EventEnvelope`s are written to stdout as JSONL, with per-step tokens, cost and profile hash.
- **Termination.** Every run ends with exactly one `RunEnded`. Its outcome is one of:
  - `completed{verified | unverified}`;
  - `failed{kind}`;
  - `budget_exhausted`;
  - `cancelled`;
  - `rejected`;
  - `timed_out`;
  - `abandoned`;
  - `escalated{to}`.
  
  Each outcome has its own exit code (Appendix F) [H2 §9.1 #6].
- **No user present.** Approvals and decisions resolve through `--approvals <policy>` or are denied. A request nothing can resolve ends the run as `failed{kind: needs_user}`.
- **Isolation by default (P24).** This follows the Harness² #111 lesson [H2 §4]:
  - an ephemeral state directory;
  - no memory carried between runs (memory seeds only);
  - the sandbox on, with a scrubbed environment and a private `/tmp`;
  - no shared caches;
  - network off unless both the profile and the pinned policy allow it.
- **Substrate fit.** Harness²'s five calls map onto Wisp commands; Appendix F has the full mapping:
  - `surface` → `wisp profile surface`
  - `materialise` → `wisp profile validate`
  - `execute` → `wisp run --headless`
  - `read_trajectory` → the JSONL output, plus `wisp trajectory render`
  - `preflight` → `wisp preflight` (no model call)

### 6.4 Self-tuning

**Decided (D8): optional, outside the kernel, budgeted, approved by the user.**

- **What it is.** A separate client, `wisp-tune` (M13), that drives `wisp-eval` (M6).
- **How it decides.** It proposes profile diffs [H2 §2.3] and keeps one only if it wins **at equal budget** [MAO §4.9].
- **Adoption.** Changes go through the user's review inbox. Rollback is by hash. The tuner can never touch the pinned layer.
- **Proactivity.** It may tune triage prompts and thresholds against the usefulness eval, but never tier ceilings or quiet hours (§10).

---

## 7. Persistence and durability

**Proposed (P5, P6, P14), after [MAO §4.6] and [HL §7.1 B].**

### 7.1 An event-sourced reducer over SQLite

**Storage (P5).** One SQLite file per project, plus `home.db`. WAL mode, `rusqlite`, and one writer actor per database. Data stays on the Linux filesystem, never on `/mnt/c` under WSL2.

- **Why one file per project.**
  - A project is archived, exported or deleted as a directory.
  - Projects don't contend for one writer lock.
  - Isolation is physical, not a matter of query discipline.
  - The cost is a dispatcher for messages between files (§4.3).
- **The alternative: one file for everything.**
  - It gives atomic transactions across projects, no dispatcher, one backup, and simple global queries.
  - It also means one WAL writer for every concurrent project, isolation that depends on query discipline, and row-level deletion.

**Tables.** The real tables are `events`, `blobs` and `snapshots`. Timers, leases, the inbox and outbox, pending approvals and status are projections that can be rebuilt from events.

**The kernel loop:**

```
on input (client command | step result | timer | message delivery):
    txn { append events; apply(&mut state, &events) }        // apply = pure fold
    for cmd in decide(&state, now):                          // decide = pure function of state
        dispatch(cmd)  // start step, deliver message, evaluate gate, fire timer, pause run ...
```

- **Determinism.** `apply` and `decide` never read the clock or randomness. Time enters only as events or as `now` for due timers [MAO Q7].
- **Ownership.** A data-directory lock plus a fencing token per run mean two daemons never drive the same run.

### 7.2 Step journal and commit granularity

- **Step events.** `StepScheduled`, `StepCompleted{output_ref, usage, cost, profile_hash}`, `StepFailed{retryable, kind}`, and `AttemptRestarted{attempt, delay_ms, reason}`.
- **Model calls.** The full response is recorded inline in a durable `MessageCommitted` event. Replay never re-requests it. Token deltas are live-only [MAO §4.6 #3, #5].
- **One retry layer (P14). Partly delivered on the JSONL store (resilience MR 2, `docs/plans/resilience.md` §5).** Retry and backoff live in the kernel's step runner (`agent/retry.rs`); the adapter makes single attempts.
  - **Today.** The adapter classifies each failure (`ErrorDetail.retry`: `never`, `transient`, or `after` with the server's `retry-after`) and returns it. The step runner decides with a pure policy, `retry::decide`: an ordinary `transient` or `after` failure is re-sent without a count or waiting-time bound by default; `never` stops at once. Retryable `invalid_response` failures are capped at 3 retries per step (`RetryPolicy::for_kind`), even under looser explicit policies, while tighter bounds still win. Typed subscription-limit waits instead follow the bounds in [Busy endpoints](endpoints.md#busy-endpoints). The opt-in `--retry-budget USD` stops a possibly billed retry when the budget cannot cover the next attempt. The next attempt is estimated from the failed attempt's usage, else the last committed step's prompt read from the cache, else (on a session's first request) one token per byte of the serialised request with each image at the largest tier, priced at the 1h cache-write rate; a possibly billed attempt with unknown usage counts at that estimate. An unknown price cannot enforce a monetary budget, so a possibly billed failure under an opt-in budget uses the finite fallback counts and reports that the budget could not be checked. Waits double from 1 s to a 5-minute cap, less up to 20 % jitter, with the server's `retry-after` as a floor, however long for ordinary failures. A **busy** failure (a `rate_limit` or `overloaded` refusal that isn't `never`, carries no `refused` and can't have been billed, `retry::is_busy`) also tracks its waited time. `RetryPolicy` has optional step, turn, busy-window and retry-after bounds, all absent by default; one fallback policy fills absent bounds with 5 step retries, 10 turn retries, a 600 s busy window and a 60 s retry-after ceiling. Background compaction and foreground native compaction select it; native overloads stay counted, so they fall back to Wisp's engine after their quick tries. Wisp foreground compaction stays unbounded for ordinary failures. Keep-warm has its own deadline. In the main agent, Esc during a model retry wait ends with the already charged failure and a Retry offer; compaction waits and in-flight cancellations still end cancelled; a model, effort or endpoint change during it applies from the next turn. Messages sent during the wait are taken in as usual: a steer waits for the retried step and goes in at the next step boundary, never into the retry. A cancel, a closed channel or a turn that gives up settles waiting and delivered-but-uncommitted messages by the steering rules (`TurnStopped`, `SessionEnded` or `TurnFailed`).
  - **Same bytes.** The retry re-sends the failed attempt's messages unchanged and reads its settings and features again, which stay the same unless something between the attempts changed them, such as a break that turned binding controls on, so it reads the cache and nothing committed changes. The failed attempt's partial reply is never committed; its text goes to the `retrying` log record. The feature-trial fallback is a different request and is not counted as a retry.
  - **Reporting.** Each retry emits `AgentEvent::Retrying` (step, attempt and optional maximum, kind, detail, discarded blocks and characters, the failed attempt's usage, serving model, `cost_usd` and `lower_bound`), and its usage counts toward the turn's cost, flagged a lower bound when possibly billed. Clients add a retry's `cost_usd` to the running cost when it arrives; `StepEnded` carries only the attempt that ended the step, so nothing is counted twice. A step ends with one `StepEnded` however many attempts it took. `max_attempts: 0` means no maximum; an unbounded busy retry carries `busy { waited_ms, window_ms: 0 }`. The transcript updates one notice per step, while the details and log keep every retry. `TurnOutcome::Error` retains all `gave_up` variants for old logs and explicit finite policies; default turns stop for `not_retryable`, `step_limit` for retryable `invalid_response`, `budget`, `retry_after_too_long` for a typed subscription limit, or, for the main agent, an Esc-stopped wait (the last failure with no `gave_up`), while an unpriced opt-in budget uses finite fallback counts. Legacy count/window/long-wait give-up sentences are no longer shown.
  - **After a turn gives up** (planned, resilience MR 3, `docs/plans/resilience.md` §6). There is no `/retry` command. The kernel puts an `offer` on `TurnOutcome::Error`: **Switch endpoint** on every failure where it can work, whenever another endpoint is available (not after `ContextOverflow`, `Refusal`, or Wisp's own refusal to send a malformed history, `check_layout`'s `InvalidRequest`), and **Retry** only where the same bytes can succeed (not after `InvalidRequest`, `ContextOverflow`, `Refusal` or `ThinkingBinding`; after `Auth` it is offered for the client to sign in again or change the key first, and is never automatic, since automatic retries follow the error's `RetryClass`). A switch is judged when it is chosen, so an account added after the failure can take it. Either is one `ClientCommand::Retry { turn, endpoint }`, sent by the client's choice; the one Retry the kernel starts by itself is the main agent's Retry where it failed, a minute after a used-up limit's reset ([Switching mid-session](endpoints.md#switching-mid-session)). It starts a continuation turn that re-sends what the failed request carried and history doesn't hold (the steers delivered into it, and the turn's own inputs when nothing committed), in the same places, so its first request is the failed one byte for byte. After a restart, the resent steers carry one mark after their last user message. A switch persists like `/endpoint` and is labelled a cache rebuild. Retry keeps the failed turn's model where the chosen account serves it, else uses that account's default, as `/account` does. It keeps the failed turn's effort, refusing it when the landing model doesn't take it, as an account switch does. Never automatic: failover stays a keypress with the cost shown first.
  - **In M4.** `AttemptRestarted` **replaces** `AgentEvent::Retrying` with the event envelope, so there is a single retry event [HL §7.1 #32], and the step journal records attempts durably.
- **History is append-only per step (P6). This amends `CLAUDE.md` (D5).**
  - **Today** a whole turn commits or rolls back. With tools, a turn holds side effects that really happened, so history becomes append-only **per step**.
  - **In-flight responses.** A cancelled or failed one is never committed, exactly as today.
  - **Tool calls.** Every call that ran is recorded with its real result or a deterministic synthetic one (`interrupted`, `unknown`) [HL §7.1 #2, #7].
  - **Reporting a cancel.** `TurnOutcome::Cancelled` and `Error` gain `committed_steps: Vec<StepId>`. The client shows "cancelled after 3 steps; their changes remain". The next turn's history includes those steps.
  - **Chat without tools** behaves exactly as in M1.
  - **Quiet end.** A reply that stops with `end_turn` and has no content (no non-blank text, call or other block; thinking, markers and lossy blocks don't count) is kept as one assistant text block, `[no text]` (`provider::NO_TEXT`), with its thinking dropped, so the history shows where the turn ended. A reply whose only text is `[no text]`, the model's own (thinking and whitespace beside it allowed), is one too and is kept as Wisp's marker; live, the kernel holds a reply's text back while it could still become the marker, so no client ever sees it. It commits the turn's opening and mail, adds an epoch entry and keeps no message-thread head; it isn't a final reply, so `before-end` stays silent, and a child's last text, the agents tree and the resume transcript pass over it (`provider::quiet_end`). A reply cut off with nothing in it (`max_tokens`, `context_window_exceeded`) still keeps nothing.

### 7.3 Effect classes

Every tool declares an effect class [MAO §4.6 #4].

| Effect class | Examples | Recovery after a crash |
|---|---|---|
| `ReadOnly` | read, search, `git diff` | Re-run |
| `IdempotentKeyed` | Forge API calls with a key | Re-run with the key `run/step` |
| `LocalTxn` | Board publish, memory write | The effect and its record commit together: exactly once |
| `NonIdempotent` | Shell, file edits | A write-ahead `EffectIntended` comes first. If there is no completion, a reconcile probe checks file hashes and git refs. If that can't tell, `EffectUnknown` is surfaced. **Never re-run silently** |

### 7.4 Approvals, decisions and timers

- **Durable requests and decisions.** `ApprovalRequested` and `ApprovalDecided{decided_by: user{client, device} | policy{name}}` are events. Deadlines are entries in the single generic timer projection (P34, §10.1), which is rebuilt at boot.
- **Deadlines.** A passed deadline emits `ApprovalTimedOut`, then the configured action (remind, park or reject). Nothing is approved unless a named policy says so.
- **Superseding messages.** A user message to a thread with a pending tool-permission approval denies that approval [HL §7.1 #6].
- **Resuming after a long pause.** The CommandGate re-runs and the base commit and plan hash are checked before the final approval. The cost of a cold cache is shown first [MAO Q6].

### 7.5 Versioning

- **Event versions.** Every event carries `schema_version`. There is a migration chain and **golden JSON fixtures** for each kind [HL §7.1 #5].
- **Pinning at `RunStarted`.** The run records `flow@version+hash`, the bundle hash, every role-profile hash and the event schema version.
  - An unknown pinned flow is refused, never guessed.
  - A run migrates only while paused, recorded as `RunMigrated` [MAO §4.6 #7].

### 7.6 Replay

1. **Boot recovery.** Fold the snapshot plus the tail, then `decide`.
   - Model steps start a new attempt.
   - Tool steps follow their effect class.
   - Gates re-run.
   - Approvals stay pending.
2. **Fork at step N** with a different profile, prompt or model (`forked_from`). Used for debugging, evals and optimizers [MAO §4.6 #9, H2 §9.3].
3. **Cassette replay** of recorded provider streams, for deterministic evals and CI.

Large payloads live in blobs. Compaction is an overlay event, never a rewrite [HL §1.2].

---

## 8. Protocol, daemon and clients

### 8.1 Changes to `wisp-protocol`

**Proposed (P19).**

- **Frames.**
  - **Durable:** `EventEnvelope { stream, seq, at, thread, run, step, schema, event }`. `seq` is the resume cursor for its stream.
  - **Live:** `LiveEvent` carries token deltas. It is never stored.
- **`Event` families** (Appendix B):
  - `Agent`: today's `AgentEvent`, plus `MessageCommitted`, `ToolCallRequested` and `ToolCallFinished`;
  - `Bus`, `Thread`, `Run`, `Gate`, `Approval`, `Artifact`, `Budget`, `Effect`, `Project`, `Client`, `Diagnostic`;
  - for proactivity (§10): `Trigger`, `Routine`, `Intent`, `Notification`, `Memory`, `External`.
- **Which `AgentEvent`s are durable.**
  - **Durable:** `TurnStarted`, `MessageCommitted` (new: carries the assembled message **inline**, so replayed history and JSONL logs have the text), `ToolCallRequested` / `ToolCallFinished` (new: args and structured result), `ToolRefused` (new: a call refused by tier or taint, §10.4), `Fallback`, `CacheMiss` (gains `cause`), `Rejected`, `UnknownProviderEvent`, `InputTransformations`, `TurnEnded`.
  - **Live only:** `BlockStarted`, `TextDelta`, `ThinkingDelta`, `BlockStopped`, `CacheDiagnostics` (debug), and the `CacheStatus` projection.
  - **Removed:** `Retrying`, replaced by `AttemptRestarted`.
- **Stable ids for clients.**
  - Threads, runs and steps have ids.
  - A block is `(step, index)`.
  - A tool call is `call_id`.
  - Every approval has an id and a typed option set.
- **Commands** (13 methods; Appendix B). Each carries a `CommandId` for idempotency and gets an immediate `CommandAck` [HL §7.1 C.9]:
  - `Hello`
  - `Subscribe{scope, after_seq}`, `Unsubscribe`
  - `Send{to, text, mode: Queue | Steer}`, with a turn precondition (the turn the client saw running) and `replaces` (edit or switch a waiting message in place)
  - `Control{target, Pause | Resume | Cancel | Retry{endpoint}}`. `Target` gains `Input(InputId)`: `Control{Input(id), Cancel}` withdraws a waiting message, and `Control{Inbox, Resume}` releases held ones. `Control{Turn(n), Retry{endpoint}}` continues a failed turn, today's `ClientCommand::Retry` (`docs/plans/resilience.md` §6.4). Still 13 methods
  - `DecideApproval{id, subject_hash, decision}`
  - `CreateProject`, `SetProjectState`, `SetBudget`
  - `ReadArtifact`
  - `Query{Status | Approvals | Spend | SinceLastSeen | History{thread, before_seq, limit}}`
  - `Devices` (M12)
  - `Manage{object: Routine | Intent | Notification | Memory, op}` (§10)
- **Settings commands, as built.** One command queue in the kernel answers them in arrival order, each answer echoing the client's `request` (a number the client picks, one counter per connection shared by every command that carries one):
  - `ChangeSettings { request, changes, save }`: `changes` is a sorted, unique list of `{ target, value }`, where `target` is a setting name (`effort`) or, with `save`, a saved place (`endpoints.manifest.pricing`), and a missing `value` unsets. It is answered by `SettingsChanged { answers, changes, … }`, one `AppliedChange` per change (`layer`, `when`, `cache`, `saved`, `overridden`, and `warnings`, lines such as the background compaction warning that belong in the change's note), or by `Rejected { request, reason }`. A saved `endpoint` that moves the `default` flag sends `EndpointsChanged` before the answer. A save goes to the host (`SettingsHost`, the CLI's `config.toml` writer) off the kernel's loop; messages sent meanwhile wait at the front of the inbox (`front` on `InputAccepted` and `InputUnheld`) and start once every queued answer is out.
  - `DescribeSettings { request }`: answered by `SettingsSnapshot`, every plain setting with its value, layer, what it shadows and its choices, plus the saved values, the file's state, the endpoints and the models (each with its shipped `context_window`; `listed`, its rank among the shipped table's listed models, which `/model` shows under each account by default; `provider`, the provider that serves it, which groups it under each account in `/model` and learn's Settings).
  - `SaveEndpoint { request, endpoint, key, check }`, sent by the `Add an API key` card, is answered by `EndpointSaved { answers, name, store, check, notes }`, `EndpointNotSaved` or `Rejected { kind: endpoint }`. A save that reloads the endpoints announces the new set first, then answers: `EndpointsChanged`, then `EndpointSaved`. **Every endpoint command keeps this order**, as does a `ChangeSettings` that moves the default, so a client that acts on an answer (`/endpoint <name>` after `Saved`) already holds the new set.
  - `EditEndpoint { request, name, edit, store, check }` and `RemoveEndpoint { request, name, keep_key }` change an endpoint's connection fields (`provider`, `auth`, `url`, `key`, `key_env`, `url_env`; `store` moves the stored key) or remove its table, through the same edit and remove operations as `wisp endpoint edit` and `remove`, run by the host (`EndpointHost`) off the kernel's loop. An edit sets only the fields it carries; an empty one, a key-shaped variable name or a bad URL is refused at the head. `check: false` saves without the probe, which otherwise runs for a new provider, key or URL. They are answered by `EndpointEdited { answers, name, changed, store, check, notes }` or `EndpointRemoved { answers, name, kept_item, default_cleared, notes }`, preceded by `EndpointsChanged` with the new set; by `EndpointNotSaved` for a failed check or a keychain that didn't answer (a removal can then be sent again with `keep_key: true`); and by `Rejected { kind: endpoint }` for an invalid request. The current endpoint, the one the next turn uses and one a waiting `Retry` names are never removed. An edit of the current endpoint is saved and applies at the next start; an edit of another one is used from the next switch to it, and a removed one can't be chosen again this session.
  - **Every built kernel setting applies mid-session by its effect class**, and `when` says which:
    - **now** (`now`): nothing in any request changes. `retry.budget_usd`, `context.count_tokens` and `endpoints.<e>.pricing` (kept per endpoint, read only while that endpoint is current) are used from the next read on. `instructions.warn_tokens` re-checks the loaded instructions at once: when they are over the new threshold, the kernel emits `InstructionsWarning` with the `large` diagnostic after the answer, and at or below it nothing.
    - **next turn, appended** (`next_turn`, cache kept): the next turn's user message carries the change. `time` and `context.context_note` decide whether it carries its `Sent:` or `Context:` line (both sentences are always in the system prompt); `context_window` is kept per model and `compact_at` for every model (per model only when restored from an older session), and both change the `Context:` line's words. `effort` on a pair that takes per-message effort is appended as its marker.
    - **next turn, rebuild** (`next_turn`, `cache: rebuilt`): a request parameter outside the history. `model`, `endpoint`, top-level `effort`, `cache_ttl` (`cache_ttl_change`) and the request option `debug_cache` (`request_option_change`). The last two are expected to keep the cached prefix, so they don't set the rebuild mark that a compaction anchors on. The request option `refusal_fallback` is next turn, cache kept: neither the `fallbacks` field nor its beta keys the cache (`docs/pricing.md`).
    - **next session** (`next_session`): only saved, and refused without `save`. The tools, the log, the keychain, instruction sources and the rest; an endpoint's connection fields are changed with `EditEndpoint` or `wisp endpoint edit`, never through `ChangeSettings`.
    - **next call** (`next_call`): only saved, and refused without `save`, like next session. The `voice.*` settings, which the voice layer reads again when each call starts (`docs/voice.md`).
    - **display** settings (`now`, `layer: session`): the kernel stores nothing and only saves them; the client applies them when the answer arrives.
  - A name set only per model or per endpoint (`context_window`, `pricing`) binds when the command is queued to the model or endpoint the queue ahead will make current, and its answer names the full path.
  - `SettingsChanged`'s `endpoint`, `model` and `effort` are always the next ordinary turn's, and its `cache` is the effect of that turn's whole pending state: endpoint, model and effort first, then a pending `cache_ttl` or `debug_cache`.
  - **Request options.** `debug_cache` and `refusal_fallback` travel on each `Request` as `RequestOptions` beside `Features`, so changing one needs no reconnect. The fallback beta header is still sent while the history holds a `fallback` block, so turning the fallback off never makes a request carrying an old one fail.
  - **Under `--replay`** the harness answers `DescribeSettings` with the rows the CLI resolved once at start from the flags, the environment and one read of `config.toml`, each with its real layer; `model`, `effort` and `endpoint` read `session` once a replayed answer changed them, and it refuses every save, and an effort the target model doesn't take with the kernel's sentence. The account comes from `--account`, then `WISP_ACCOUNT` (or `WISP_ENDPOINT`), then the saved `default`, then `claude-api`, with no key, env file or keychain read; it is official by the same rule as the kernel's (`EndpointConfig::official`: a literal URL by its origin, a key variable with no URL official, a `url_env` or a stored key with no URL unknown, shown as a gateway). A broken file gives no saved values, and the snapshot names the break.
- **Client state, as built.** `Terminal { size }` tells the kernel the reply width (the columns a transcript line fills, narrower while the sidebar is docked) and the terminal's rows. The TUI sends it at start and whenever the value changes, checking at each settled resize, sidebar or pager change and message it sends, so a send that failed goes out at the next check; nothing answers it. The root agent keeps the last nonzero size, never logged, and the `workspace` plugin's `workspace/terminal` section adds a `Terminal:` line to the turn's message when it differs from what the model last saw. A child ignores it, and a `Child`-wrapped one is refused like any command that isn't routable. jsonl and `-p` send none.
- **Voice, as built.** `ClientCommand::Voice` and `AgentEvent::Voice` carry voice calls: `start`, `stop` and `note` in, `started` and exactly one `ended` with a typed outcome out, each naming its call. Audio rides a lane of its own, never the event stream. See `docs/voice.md`.
- **Determinism.**
  - No `HashMap` or `HashSet`, enforced by clippy `disallowed-types`.
  - JSON Schema is generated with `schemars`, committed, and diffed in CI [HL §7.1 C.11].
- **Stability.** The protocol stays unstable until M7 ends. After that it changes only through capability flags, and features are deprecated before they are removed.

### 8.2 Opening a running chat

Built: one host process per chat, one window at a time. `Hello` probes without
changing a window; `Open` takes it, and the previous window gets `Taken`.
An opening gives history through the last finished turn, current state, the
events since that cut and then live events. Replay writes no log record or
model request. A lost window leaves the chat working; a bound on waiting live
lines closes a window that falls too far behind. Multi-client observation, approval broadcast and
presence remain proposed. `docs/daemon.md` specifies the built link.

**Still proposed:**

- **`Hello`.** Declares the client, device, role (controller or observer) and capabilities (`can_approve`, `renders_diffs`, `audio`, `bot_channel`).
- **`Subscribe`.** Replays durable events after the cursor, then goes live with no gap. `Query{History}` pages back through older history for clients attaching mid-session.
- **Fan-out.** Each event is encoded once, into a bounded queue per connection. A slow client gets `Lagged{last_seq}` and resubscribes, so it never blocks the others.
- **Approvals.** They are broadcast to clients that can approve. The first valid decision wins, and `ApprovalResolved` tells the other clients. Pending approvals live in the log, so a reconnecting phone sees them.
- **Presence and catch-up.** Presence events drive pre-warming and notifications. Each client's last-seen sequence number drives its catch-up view.

### 8.3 Transport and authentication

Built: newline-delimited protocol JSON over Unix sockets in `<data>/run`, not
`XDG_RUNTIME_DIR`, with 0700 folders, 0600 sockets and peer-uid checks in both
directions. Chats refuse descendants and job-marked peers on Linux, or a second
window when that check cannot run. Their kernel runs in `wisp --host`, not in a
shared daemon.

An always-on daemon at `<data>/daemon/daemon.sock` only lists, starts and relays;
it reads no keys, logs or settings and hosts no kernel. Terminals stay direct.
A hosted chat starts it unless stopped by command or inside a Wisp job.
`wisp daemon stop` leaves chats open; an explicit start clears its stop mark.
The daemon checks the peer's job mark on Linux, refusing an unreadable environment.
These checks are seatbelts, not a sandbox.

**Still proposed (P4):** tool sandboxes that never mount the socket directory,
Wisp's config or data, apart from the leased worktree mounted at `/workspace`;
separate network namespaces; dedicated tool cgroups and rejection of peers in
those cgroups. A login service also remains proposed; it must preserve chat
processes when the daemon stops.

- **Remote (off by default; M12).**
  - WebSocket over TLS, with the certificate pinned at pairing.
  - Ed25519 device pairing, confirmed on a client that is already trusted.
  - A signed challenge on every connection, and only short-lived session tokens.
  - Scopes per device: `observe`, `chat`, `approve` and `admin`.
  - Revocation.
  - No endpoints taken from URL parameters (OpenClaw CVE-2026-25253) and no unauthenticated mode [HL §7.1 C.13].
  - The steps are in Appendix F.
- **Reachability off the LAN** is O4.

### 8.4 Clients and the TUI

**Proposed (P32, P33), following [TUI §10].**

- **`wisp-client`.** Connection, handshake, subscribe, reconnect from a cursor, and typed command helpers.
- **`wisp-render`** (new, **pure**: no I/O).
  - It turns the transcript model into styled lines.
  - It handles streaming Markdown (a newline gate, finalised blocks, held-back fences), syntax highlighting, diffs, width, wrapping, sanitisation and theme tokens.
  - Transcript export reuses it [TUI §10.3].
  - MR 4's `markdown` and `render` modules in `wisp-cli` move into it (M3).
- **`wisp-tui`** (new). Owns the terminal: capability probe, guard and restore, synchronized output, the full-screen presenter, widgets and the app loop.
  - It takes an event stream and a command sink, so it doesn't care about transport.
  - `wisp-cli` wires the transport and chooses the full screen or JSON output.
- **`wisp-media`** (new in M3.1, **pure**: no I/O, no clock). Image sniffing, the API's resize rule, and normalisation for pasted images, shared by the TUI (to size and refuse an image at attach time) and the kernel (to normalise what it sends). See `docs/media.md`.
- **`wisp-diff`** (**pure**: no I/O, no clock). The line diff for `before`/`after` text, the tolerant reader for unified diffs a model writes, the line classifier every diff view shares, and changed-word emphasis. The renderer and the `show-diff` plugin use it. See `docs/diff.md`.
- **`wisp-instructions`** (**pure**: file access goes through its `Files` trait, no clock). The instruction-file walker shared by the `instructions` plugin and the core's preset files: the choice, imports, the rendered block, the `large` token check and sha256 (`hmac-sha256`). Among workspace crates it depends only on `wisp-plugin-api`, whose `instructions` records it uses, and `wisp-tool-text`. Its pure `skills` module shares the SKILL.md parser, ordered folder walk and folder/name/file-inclusion rules between search and skills, with I/O supplied by the caller; `serde_json` reads JSON-quoted skill values, without I/O. Its `delta` module holds the instructions plugin's snapshot format and updates; the core reads that format through it (`delta::with_scope`, `block_scope`, `has_files`, `prior_entries`, `since`) for the window base and a head's record. See `docs/instructions.md`.
- **Dependencies.** Both crates depend on `wisp-protocol`; `wisp-render` also on `wisp-diff`, and `wisp-tui` also on `wisp-render` and `wisp-media`. `wisp-media` and `wisp-diff` depend on no workspace crate. A CI check enforces "no `wisp-core`".
- **`wisp-tool-text`** (**pure**: no I/O, no clock). The wording tools share: output sections and limits, `cut_chars`, result text over the `wisp-plugin-api` outcome mirror, the jobs sentences, the terminal size limits and copies of `shorten_path` and `and_list` (of `wisp-protocol`'s `shorten_path` and `listed`), each tied to its source by a `wisp-core` test. The kernel's `<processes>` notice, its synthetic results and the tools all use it, and so will plugins. It depends only on `wisp-plugin-api`; a test checks that.
- **Plugin registry and host.** `wisp-core`'s `plugin` module holds the registry (`plugin/registry.rs`: loads manifests, refuses clashes, starts plugins and builds the `Toolbox`), the kernel host (`plugin/host.rs`: `KernelHost` and the per-plugin `PluginHost`, which implement the mirror's `Host` over the session's workspace, clock, settings, context, media and the jobs service, with one `call-context` per running call), the API ↔ protocol conversions (`plugin/convert.rs`), the catalog (`Registry::catalog`, the protocol's `Catalog`), the task-local `Caller` that carries a call's cancel token, deadline and render kind to the host (`plugin/caller.rs`), world 0.2.0's `http` import (`plugin/http.rs`), the import table that says what each import does inside a render (`plugin/imports.rs`), and the slot lane (`plugin/slots.rs`, `plugin/slots/lane.rs`): a task of the root session's own, started by its first `SlotsShown`, that renders the shown sidebar sections when their marks fire and sends `SlotContent` and `SlotFailed`, with a rate limit and a breaker per view, so a render never runs in or waits on the turn loop, which only marks points and publishes the snapshot with plain calls. `wisp-contain` holds the containment flag every plugin call runs under, and `wisp-plugins` lists the bundled plugins. The kernel holds no tool code: `bash` and `process` come from the `shell` plugin, the provider's web search from the `web-search` plugin's native tool, and the kernel finds the shell tool through its `shell` capability (`provider_of`). The turn loop holds no turn-line code either: the `Sent:` stamp and the `Context:` line are the `before-turn` hooks of the `time` and `context` plugins, and their two system sentences are those plugins' `[system]` texts. The `<environment>` block is the `workspace` plugin's prompt section, rendered from the facts the kernel gives through `workspace-facts`, and the `<instructions>` block is the `instructions` plugin's `system` section, which reports its load through `instructions-report`. Compaction's summary instruction, its section check and the head's `<compacted_context>` layout are the `compaction` plugin's strategy, which `context.strategy = default` names; the core keeps the mechanics (when to compact, the summariser requests, the cut, install, the epoch and the log) and a minimal fallback head for when the plugin is removed or fails. See `docs/plugins.md`.
- **Kernel host services.** `wisp-core`'s `jobs` module is the jobs service (the process table, pipes, terminals, process groups, waits, reads and end reports per call), shaped as the WIT `jobs` interface, and `call` holds a call's output sink and user channel. Neither imports `crate::tool`; a source scan checks that.
- **One transcript model, one full-screen presenter (P32).**
  - M3 shipped an inline presenter: finished blocks went to the terminal's native scrollback, and only the live tail was redrawn.
  - M3.1 replaced it with a full-screen presenter by the owner's decision, ahead of the parity checklist [TUI §10.2]: the conversation scrolls inside Wisp on the alternate screen. Selection, copy and the `[` hand-off to native scrollback come in M3.1's MR 4; search comes later.
  - It lives in one crate, never two TUIs.
- **What the TUI needs from the protocol** [TUI §10.3]:
  - stable ids;
  - tool arguments and structured results;
  - approvals with option sets;
  - thread lifecycle (spawned, status, parent, role, budget, per-thread cost);
  - summaries of inter-agent messages;
  - paged history.
  
  §8.1 provides each of these. Events stay renderer-neutral: data, not styling.
- **Terminal-escape injection defence.** Model and tool output can carry escape sequences, such as a title-set injection or OSC 8 links.
  - `wisp-render` sanitises all content at ingestion, before it reaches the screen.
  - Every write that bypasses the screen buffer (MR 4's `[` hand-off to native scrollback) sanitises again.
  - Raw views show stripped control bytes visibly [TUI §7.5].
  - The one OSC 8 link Wisp writes is the sign-in card's link text. The painter wraps those cells in `ESC ] 8 ; id=wisp-login ; URL ESC \` only for an `https://` URL of printable ASCII, repaints in full whenever the span or URL changes, and the restore plan closes any open link first.
- **Model and effort pickers (M3.1 MR 3).** `/model` and `/effort` open a menu drawn over the bottom of the view, so the transcript never moves; `/model <id>` and `/effort <level>` apply directly, and Shift-Tab steps effort only where the cache is kept. When the chosen model lacks the current effort, the effort picker opens on the highest supported level below it, or on the lowest supported level if none is lower or there is no current level; Shift-Tab from an unsupported or absent level goes to the same level. The rule is the same for every model: the API's default effort is only shown as a label. `/endpoint` (endpoints MR 4) uses the same menu for the configured endpoints, priced as a whole-prompt rebuild at the target endpoint; the TUI's tracker keeps pair states per (endpoint, model) and an agreement test holds its predictions to the kernel's `CacheEffect` across switches.
  - `SessionFacts` carries the model list (sorted, from `pricing` and the capability table), each model's rates and capabilities, the endpoint kind and the prefix and tail write multipliers. `wisp-tui` never reads `wisp-core`.
  - Each row shows its cache effect before you choose: kept, kept if a gateway passes per-message effort on, or a rebuild estimate. The prefix is the first request's 1h writes plus its cache reads (a shared prefix another session wrote). A model switch rebuilds everything: the prefix at the 1h write multiplier and the rest of the last prompt at the session's. An effort change always rebuilds the messages and, on some models the docs don't list, the system and tools too, so its estimate prices the whole prompt the same way and reads `cache rebuild up to ~$X` (or `up to ≈N tokens`). Both use the input rate for the whole prompt, read tokens when the model has no price, and count each part only while it is still cached (the prefix for 1h even in a 5m session).
  - A switch to a model whose documented maximum output is below the session's `max_tokens` adds `output capped at N tokens` to its note.
  - Every `SettingsChanged` adds a note cell. A change made during a turn shows as pending in the status row and its note follows that turn's footer, priced against the cache the turn left. A mid-turn rejection of per-message effort adds a second, correcting note, and an expected miss is labelled `cache rebuilt: <reason>` in the footer instead of `cache miss`.
  - The kernel emits `FeatureStateChanged` whenever a (model, feature) pair is proven or falls back, silent fallbacks included. The TUI tracks per-message effort per model only from that event and the capability table, never by inference. Until a gateway pair is proven, Shift-Tab explains the cost instead of acting. A `wisp-cli` test runs the real `Agent` against a fake gateway and checks every row and Shift-Tab prediction against the kernel's `SettingsChanged.cache`.
- **Steering in the TUI (steering MR 2).** While a turn runs, or while queued messages are about to start the next one, Enter sends `Send { mode: Steer, turn }` and Tab (at the end of a non-blank last line, no chip before the cursor) sends `Send { mode: Queue, turn }`; idle sends use `mode: Steer, turn: None`, and Enter on an empty composer with held messages sends `Release`. Everything shown comes from events: a queued message appears as a waiting row on `InputAccepted`, becomes a user cell where `InputDelivered` puts it (idle sends included), turns `held` on `InputHeld`, and on `InputWithdrawn` either goes back into the composer ahead of the draft (a steer that was never delivered, with its paste chips renumbered after the draft's) or marks its cell `not kept` (one that was delivered but never answered). Refusals are matched by `Rejected.id`; the settings tracker only counts refusals without an id. The waiting rows take their own rows above the composer rather than covering the transcript, so the latest output stays visible for a whole turn. `wisp --replay` emulates the kernel's inbox (accepted, delivered at the next scripted `StepStarted`, held, withdrawn, queued turns and `Release`) so replays and tests exercise the same paths. A hosted session's replay-only `InputAccepted.opening` acknowledges a direct idle input before its turn: the window already tracking it stays unchanged, while a new window seeds it for delivery without a queued tag. The internal host opt-in is inherited by children and changes no request or log.
- **Visual design.** The TUI's design source is `docs/design/tui-brand.md`: a brand and animation spec built on Wisp's **orb** mark, covering palette, glyphs, motion and reduced-motion fallbacks. A designer is writing it, so **it is pending**. It supersedes the research's design-language proposal [TUI §10.4]. `wisp-render::theme` implements it as tokens, which makes late changes a token swap.
- **Stack (from the research).** `ratatui-core` and `ratatui-widgets`, our own terminal layer, a pinned `crossterm` behind a `term` module, `pulldown-cmark`, and `syntect` with `two-face` (O12: `onig`). `similar` and `diffy` arrive with diff rendering in M5, and a termina spike waits until native Windows is a target (M3 plan §10).

---

## 9. Providers, context, memory, tools, policy, sandbox, secrets, cost

These sections are brief; the detail is in the research.

### 9.1 Providers

**Decided (D6). Proposed details.**

- **`Provider` keeps `stream` and `echoed`.** `Request` gains system blocks, canonical sorted tools and a `CachePlan` (breakpoints and TTLs per request kind).
- **Capability table** (`provider::caps`). One entry per model, **taken from provider documentation and tested** [PC §5.3 #8]. It covers:
  - tools, thinking and effort levels;
  - mid-conversation system messages, per-message effort and inline tool changes;
  - `CacheSemantics` and minimum cacheable tokens (moved from the shipped model file's rows);
  - limits.
  
  Unknown models get no invented context window [HL §7.1 #30].
- **Effort capability table (today).** Each described row of `crates/wisp-core/data/models.toml`, one per model with its dated aliases, holds its capabilities: its effort levels (`efforts`, or `effort_variant` for the seventeen Antigravity level-in-ID rows: family and level, with the levels derived from the family’s rows; `docs/gemini.md`), default effort, whether it takes per-message effort, whether it supports adaptive thinking, whether it takes tool changes in the conversation (`inline_tools`, in the provider's wire form; Anthropic rows cite the tool-use docs' inline tool changes section), whether it takes tools declared with `defer_loading` (`deferred_tools`, from the tool search page's model list, https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool), and its maximum output (`max_output`); and its image columns (`images`: the vision tier and the images allowed per request, from the vision and context-window docs; `ImageCaps::new` adds the many-image rule of at most 2000 px a side past 20 images, the same for every model). `[providers.anthropic]` holds the two exact-ID lists, `message_threads` and `account_bound_thinking`, and `weekly_limits` ties a named weekly limit to documented exact model IDs; `[providers.openai]` holds `subscription_additional_tools` and `api_additional_tools`, the live-checked exact-ID lists of subscription and official API models that take Responses `additional_tools` items. Every value comes from the effort docs (https://platform.claude.com/docs/en/build-with-claude/effort, its "Change effort mid-conversation (beta)" section, and https://platform.claude.com/docs/en/about-claude/models/overview), never from a version number: each field's quote sits in the row's or the provider's `sources`, and the shipped file is refused when a field lacks it. `provider::anthropic::capabilities` and `provider::openai` read their provider's rows, and a test checks that every priced model is described. An unknown model ID gets no row and today's request shape, unless the account's model list names it (`LISTED_ONLY`; `docs/endpoints.md`, "Each account's model list").
- **Model and effort changes (today).** A change applies from the next turn. On a model with per-message effort it is an effort marker appended after the user message, so the cache is kept; otherwise it changes the top-level `output_config.effort` (or the model), and the cache is rebuilt. Through a gateway, each model's per-message effort and binding controls betas start untested: the first request carrying one is a trial, and a pre-event rejection that names the feature re-sends the step once without it, in the same step. A feature already established (proven through a gateway, or documented on the Anthropic direct API or the ChatGPT subscription for the capabilities its exact-ID lists name) falls back the same way, but only on a strong signal: rejected by shape (`output_config`, `block_binding` or `compaction` as extra inputs, an unexpected compaction block) or named as an unexpected beta; the looser wordings stay trial-only. The fall-back is the usual `fallen_back` record and lasts the session, a documented endpoint included (a fallen-back pair wins over documented). Each loop falls back a feature at most once per step, so a repeated rejection ends the turn with its own error. A capability later listed for the subscription is established at once and needs its own live check first; a 400 or 422 invalid request naming a sent `additional_tools` item is OpenAI's strong signal for inline tool changes.
- **Endpoints (today).** A session can hold several connections, one per endpoint in `config.toml` (`docs/endpoints.md`), each with its own provider, log context and pricing flag. A switch applies from the next turn, like a model or effort change, and always reports a cache rebuild with the reason `EndpointSwitch`. History holds no endpoint data, so the request after a switch carries the same messages, system prompt and tools the old endpoint would have received; only the auth header, the host, and anything the new pair has fallen back from (a beta or an effort marker) differ. The exception is web search: `tools` follows the new pair's search form (native or none, resolved per (endpoint, model) from the model file and the origin rule), so a switch between forms changes `tools`, cuts all earlier thinking and emits `ToolsChanged { via: rebuild }`; and an endpoint that refuses another account's search blocks gets them rendered as text (`docs/endpoints.md`, "Web search"). Pair states (per-message effort, binding controls) are kept per (endpoint, model). With more than one connection, thinking binding controls (`drop_block`) are on from the first request so the thinking config never changes mid-session, and a model listed as account-bound in the capability table is named in `SettingsChanged.account_bound` when its earlier thinking may be dropped. A refusal of earlier thinking blocks is `ErrorKind::ThinkingBinding`: Wisp leaves every earlier thinking block out and re-sends once (`docs/context.md`, "Broken thinking"), and a second refusal ends the turn. With no `config.toml` the single keys-file connection (`claude-api` or `gateway`) sends the same bytes as before endpoints existed.
- **No per-model edit preset.** When an edit tool arrives, every model gets the same one; per-model variants such as `str_replace` for Claude and `apply_patch` for GPT-family models [HL §7.1 #31] are deferred by the owner's 2026-10-23 rule.
- **A second provider (P23, M6).** An OpenAI-compatible adapter.
- **The provider contract for blocks another provider made (planned with the second adapter).**
  1. An adapter sends a provider-specific block (`Opaque`, `Thinking`, `RedactedThinking`, a signature, an encrypted payload) only if its own provider made it: its own `Opaque` blocks (the Anthropic adapter already drops others), and thinking only from messages its provider served, read from per-message provenance such as the search state's per-message endpoints. Text, tool calls, tool results and images always go.
  2. Everything else is rendered or left out, on the request only: history and the log keep every block byte for byte (the lossless round trip; nothing is silently dropped). Searches become rule R's lines from the provider-neutral `SearchOutcome`; signatures and encrypted payloads are left out; a file or container reference becomes a request-side text note, as a refused attachment's is (`docs/media.md`).
  3. The rendering is a pure function of history and logged records, so the prefix stays stable.
  4. The adapter maps its own provider's "can't read this block" refusals onto the kernel's recovery for that block kind: `Refused::CrossAccount` (search results), `ThinkingBinding` (thinking), `Refused::CompactionBlock` (a compaction block).
- **Adapter contract (today).** A provider makes **one attempt per `stream` call** and never re-sends; retrying is the kernel's (§7.2, P14). Every adapter runs its request through one driver (`provider/sse.rs`, `drive`): the adapter builds the request, the driver sends it, the adapter reads a refusal from the response, and the driver feeds the body to the adapter's decoder. The driver emits `Sending` after any token refresh and just before the send, because the agent counts the request as possibly billed from then on; a connection that never opened isn't billed. `stream` itself sends nothing: the request goes out only once the stream is polled past `Sending`, so the stagger can hold a built request (`docs/agents.md`, "The stagger"). Every failure carries an `ErrorDetail` (`provider/classify.rs`, `docs/plans/resilience.md` §4): its `retry` class says whether the same bytes could succeed later (`after` carries `retry-after-ms` or `retry-after`), and its `possibly_billed` follows the HTTP status, so a 408 or a 5xx other than 529 is possibly billed, while other 4xx and 529 are not, and a load refusal (a `rate_limit` or `overloaded` kind, `classify::load_refusal`) is never possibly billed; a failure after a 200 stream has started is always possibly billed, and carries status 200 and the response's request id. `error_type` records a scrubbed name capped at 64 characters; see `docs/session-log.md` for how Wisp selects the name. Redirects are never followed, since `x-api-key` would go along. The headers and the body's first chunk each get the provider's first-byte wait: 15 minutes on Anthropic, because buffering gateways send nothing until the reply is done, which at xhigh or max effort can take minutes, and 5 minutes on OpenAI. After the first chunk, a stream that sends nothing for 5 minutes ends. Both waits end as a transient `timeout`. An event in the stream may hold at most 16 MiB, counting the bytes still waiting for a line's end and the `data:` lines still waiting for their blank line (`provider/sse.rs`); a stream that passes it ends as an `invalid_response` that is possibly billed and never retried, for every provider.
- **Anthropic adapter notes (today).**
  - Claude Haiku 5.5 is priced in tiers by prompt length; `pricing` picks the tier per request from its usage (see [pricing.md](pricing.md)). An unknown model still gets no estimate rather than a wrong one.
  - The `FALLBACK_BETA` date gates the scalar form, `fallbacks: "default"`; the array form uses another date.
  - Opus 5.5 always thinks, so it is never sent `disabled` or a thinking budget.
  - The `fallback` marker block is echoed back: the API reference lists it as ignorable but also shows it echoed, and keeping it satisfies both readings.
- **Unchanged:** the model is sticky per thread, every fallback is visible, and authentication uses only documented means.

### 9.2 Context and memory

- **Assembly order** [PC §5.2.3]:
  1. tools;
  2. **role block** (1h breakpoint);
  3. **project block** (the chosen `AGENTS.md`, the project's or the user's default, with its imports; root-relative names and no absolute or `~` paths from Wisp; built once per session, changes as deltas; 1h breakpoint; `docs/instructions.md`);
  4. thread preamble (brief, memory and knowledge indexes, standing-orders version);
  5. history;
  6. deltas.
- **`ContextDelta`.** Covers date (by day), environment, mode, effort, tools, instructions and standing orders.
  - Each user message already carries a `Sent:` time stamp (`--time local|utc|off`), which covers the date; the date delta applies only when stamps are off.
  - Per-turn data, the date included, goes in the turn message, never the system prompt; agent type, mode and settings decide what is injected.
  - It is delivered by the cheapest cache-preserving means the model supports: a mid-conversation system message, a per-message effort setting, or a wrapped block.
  - It is never the last block [PC §5.2.1].
- **Local fingerprints** of each request name the cause of any `CacheMiss`. Expected rebuilds are counted separately [PC §5.2.6].
- **Long sessions.**
  - Tool output is bounded when recorded and spills to blobs.
  - Compaction is an overlay with a ledger of files and artifacts [HL §7.1 #15–16].
  - Jarvis and coordinators compact while idle, just before their cache would expire [PC §5.4].
- **Memory.** Scopes, kernel-written provenance and write rules are in §10.5 (P40).

### 9.3 Tools and hooks

- **`Tool` metadata.** Name, canonical schema, **effect class**, `read_only`, resource locks, capability tags (`reversible`, `irreversible`, `external_message` [PS §8.2], `reads_untrusted` [PRO §5.7]) and timeout.
- **Core tools (M5):** `read`, `edit`, `write`, `glob`, `grep`, `shell`.
- **Kernel tools:** bus, board, routing, `project_status` and `memory`. For proactivity: `intent_*`, `routine_propose` and `notify` (§10).
- **Proactivity roles:** `triage` (small and isolated) and `reader` (no tools, strict schema) (§10.3, §10.4). Both run on the session's model, like every role (§6.1).
- **Coordinator tool set (P31).** This is our reading of D2's "no write tools". Like Jarvis, the coordinator also gets no `reads_untrusted` tools (P39).
  - **Allowed:**
    - `read`, `glob`, `grep` on a read-only view;
    - board list, read and publish (its own Plan, Brief, Items and Note artifacts);
    - the down tools: `assign`, `steer`, `pause`, `resume`, `cancel`, `broadcast`;
    - the up tools: `report`, `escalate`, `request_approval`, `progress`;
    - `start_run`, `propose_flow`, `project_status`, and project `memory`.
  - **Not allowed:** `write`, `edit`, or `shell`. Proposed: no shell at all. The alternative is a sandboxed read-only shell for things like `git log`.
- **Execution.** Read-only calls run in parallel, and results are published in order [HL §7.1 #21].
- **Hooks** follow the Claude Code schema, run sandboxed, and only narrow.
- **Plugins.** Tools, slash commands and hooks come from plugins through one contract, the `wisp:plugin` WIT world in `wit/v0.1.0/`, mirrored by `wisp-plugin-api` with its manifest (`docs/plugins.md`, `docs/plans/plugins.md`). Plugin crates live under `plugins/` and may depend only on `wisp-plugin-api`, `wisp-tool-text`, `wisp-diff`, `wisp-media`, `wisp-instructions`, `wisp-index` and allow-listed pure crates.
- **MCP:** the kernel `mcp` service owns server connections and saved canonical tool lists, shared by a session tree. See `docs/mcp.md` for when connections start. The bundled plugin exposes saved tools through search/use and local server commands; HTTP and sign-in follow separately.
  Serving MCP and an ACP gateway come later (O11).

### 9.4 Policy, sandbox and secrets

- **Policy (pinned).**
  - Deterministic rules: deny > ask > allow.
  - Tiers: pinned user policy > project > profile request > default.
  - Shell matching is parser-aware [HL §7.1 #25].
  - "Ask" becomes an `ApprovalRequest`.
  - Background work gets narrower defaults [HL §7.1 #28].
  - No agent changes its own acceptance criteria, budget or permissions [PS §8.2].
- **Sandbox (M5, Linux first)** [HL §7.1 #23]. Each tool process runs under `bwrap` + seccomp, or Landlock.
  - Only the lease holder's worktree is writable.
  - **All of `.git` is read-only.** That covers config (`core.fsmonitor`, `core.hooksPath`, filters), hooks and refs. Agent git commands run with `GIT_OPTIONAL_LOCKS=0`.
  - Wisp's socket directory, config, and the rest of its data directory are **not mounted at all**. Only the leased worktree is bind-mounted, at a neutral path such as `/workspace` (§8.3).
  - The environment is scrubbed, `/tmp` is private, and there is no network by default.
- **The kernel's own git calls** run with `core.hooksPath=/dev/null` and `core.fsmonitor=false`. Agents can't write `.git/config`, so only user-written config can define commands that git runs.
- **`--dangerously-no-sandbox`:**
  - it needs an explicit user approval per run;
  - it is recorded on the run;
  - it is refused for headless or unattended runs unless the pinned policy allows it;
  - it is refused if the kernel can't put tool processes in the dedicated cgroup (§8.3).
  
  On WSL2 without user namespaces, shell is refused unless this flag is set.
- **Secrets (M9).**
  - Kept in the OS keyring or an `age` file, referenced by name.
  - Values leave only through an **egress proxy** that substitutes placeholders for allowlisted hosts [HL §7.1 #24], or a declared environment injection into one process.
  - Tool output is redacted, and provider credentials never reach agents.
  - A canary-secret test (our addition) checks every event, blob and request.
- **Provenance.** Every content block is tagged with its origin, and agent text never carries user authority. The taint rule and the quarantined reader are P39 (§10.4). O7 keeps only the representation details.

### 9.5 Caching and cost across agents

**Decided (D5); P16 per thread.**

- **Prefix identity.** Threads share a cache entry only when every part of the prefix matches:
  - provider and model;
  - the tools hash;
  - the role-block hash;
  - the project-block hash;
  - the **thinking configuration**;
  - **effort**, whenever it is sent as a request parameter rather than a per-message delta.
  
  So coders at different efforts don't share a prefix. Keep the number of distinct profiles small [MAO Q3 #5].
- **TTL (P16).** The system breakpoints always use the 1h TTL. The tail TTL depends on how long the thread is expected to sit idle:
  - **1h:** Jarvis, coordinators, and coders and reviewers in `change` or `fanout`;
  - **5m:** short-lived workers and solo coders, raised to 1h before a long tool run or an approval wait. Whether that switch still hits the cache is O8.
- **Siblings** are staggered or pre-warmed. Fix and review rounds resume their threads. One-shot calls send no cache markers [PC §5.2.4, §5.2.9].
- **Cost reporting.** Per turn, step, round, stage, role, run, project and month, including "saved by cache" and the cost of a cold restart [PC §5.2.7].

---

## 10. Proactivity

**Decided (D10).** Jarvis is proactive: it follows up, sets up routines and schedules, accumulates memories, and does research proactively, helping the user from accumulated context and history. The user named SpaceXSI's Grok Bot, Meta's Muse and OpenAI's dots as inspiration [PRO §2].

**What this section adds.** All three products enforce most of their proactivity rules in prompts [PRO §2.6]. Wisp puts those rules in the kernel instead, reusing machinery that already exists:

- timers over the event log (§7.4);
- kernel-originated bus messages (§4.2);
- budget accounts (§5.8);
- approvals (P8).

Everything below is **Proposed**. It maps the research's recommendations R1–R14 onto this architecture [PRO §5].

**Stance** (after [PRO §5.1]):

- **Code decides whether; the model decides what.** Code decides whether something runs and whether you hear about it; the model decides what to say. Models raise false alarms 50–65% of the time [PRO §1].
- **Silence is the default outcome,** and it is a typed outcome.
- **A trigger is never consent.** Authority for unattended work only narrows.
- **Background work proposes; you (or a named policy) accept.**
- **Every proactive message says why.** For example: "because: routine *Morning brief*, daily 08:00". It always offers Mute and Edit.

### 10.1 Triggers, timers and routines (P34, P35, P38)

- **One timer projection (P34)** [PRO §5.2 R1].
  - **Shared by every kind:** approval and budget-pause deadlines, schedule slots, intents, heartbeats, back-offs and expiries.
  - It is rebuilt from events at boot, and `decide(state, now)` stays pure.
  - **Fire identity.** Each fire's id is `H(trigger, slot)`, which makes firing idempotent.
  - **Catch-up, by timer kind:**

    | Timer kind | When overdue (for example, after downtime) |
    |---|---|
    | **Recurring** schedule slots and heartbeats | Missed occurrences **coalesce into one run** if the latest is within the catch-up window. Otherwise they are skipped and recorded. **Never backfilled** |
    | **One-shot** timers: approval and budget-pause deadlines (P8, §5.8), follow-up nudges (`IntentDue`), one-shot reminders (`At`), expiries, back-offs | **Always fire once**, late, with the delay recorded. A late reminder says how late it is |
  - **Uncertain delivery** is recorded as `DeliveryUnknown`, shown in the catch-up view, and **never retried**.
  - **Daylight saving.** A local time that doesn't exist fires at the next valid instant. An ambiguous one fires once, at its first occurrence.
  - The scheduler runs in `wispd` with no client attached.
- **Routines (P35)** [PRO §5.2 R2].
  - User-owned, versioned data in the D8 style: `routine.toml` plus a `SKILL.md` prompt, pinned by hash on every run.
  - A time trigger, a deterministic **watch**, or both.
  - A target: `Notify`, `Triage`, `Jarvis` or a project.
  - A tier, a budget and a delivery policy.
- **Model-free first (P35)** [PRO §5.2 R3].
  - "Remind me at 5 to call the plumber" compiles, while you're present, into a routine whose target is `Notify{text}`. Firing it costs nothing.
  - These reminders ship with the daemon (M7), before Jarvis.
- **The firing pipeline.** Everything up to dispatch is kernel code [PRO §5.2]:
  1. A pure pre-check: active window, quiet hours, budget, delivery route, and "nothing changed".
  2. Dispatch by target.
  3. `RoutineRunEnded`, with one outcome: `Silent`, `Digested`, `Notified`, `Proposed`, `Acted` or `Failed`.
- **`Trigger` message (P38)** [PRO §5.2 R6]. A new kernel → thread bus message, added alongside D3's types.
  - It wakes the thread the way `Assign` does.
  - It arrives at a turn end as a `<wisp:trigger>` block, so the model can tell a schedule from a person.
  - It carries `Provenance::Kernel` and **never counts as approval or consent**.
  - A routine that targets a project is a kernel `Assign` to the project's coordinator, with a grant from the project account.
- **External events.** Email and calendar arrive through MCP connectors (M9); local webhooks arrive on the Unix socket; file watches and project events are also sources.
  - Each event is recorded, deduplicated, and **marked untrusted**.
  - Mail is processed only from senders you allowed.

### 10.2 Follow-ups (P36)

Follow-ups are compiled into kernel-tracked **`Intent`s**, not into memory prose [PRO §5.2 R4].

- **Trigger.** An intent fires on a time, on an event (a watch), or on **a time paired with an event**.
  - Example: "if Alex hasn't replied by Friday, remind me" fires on Friday at 09:00, unless Alex's reply arrives first and cancels it.
- **Never nags.** It fires **one unanswered nudge** at most. After that the intent expires and is mentioned once in the next digest ("I stopped following up on X").
- **Expiry.** 30 days by default, editable.
- **Who creates intents.**
  - You.
  - Jarvis, but only in a turn **you started that isn't report-bearing** (P39).
  - A coordinator, within its own project.
  
  The kernel refuses `intent_create` in trigger, heartbeat or report-bearing turns, and in tainted short-lived runs. Instead it turns the request into a proposal in your inbox.
- **Cancelling.** Cancelling narrows authority, so agents may cancel. Each cancellation is recorded with its reason and listed in the digest.

### 10.3 The heartbeat (P37)

The heartbeat runs in three steps [PRO §5.2 R5]:

1. **Deterministic pre-check.** Every 30 minutes within active hours, it asks whether anything is due or new: intents, fired watches, queued Reports, aging approvals, upcoming calendar items, failure streaks. If nothing is, it records `TriggerSkipped{nothing_due}` and **calls no model**.
2. **Triage.** Otherwise it makes **one** small call on the `triage` role, on the session's model like every role (§6.1).
   - The role has its own small, stable prefix.
   - Its input is a structured status digest of about 2–5K tokens. It **never runs in Jarvis's main thread and never sees Jarvis's transcript**.
   - Its output is a schema-checked `TriageVerdict`: `none`, `digest`, `notify`, `wake`, `propose` or `research` (§10.8).
3. **Wake.** Jarvis is woken, through a `Trigger`, **only on `wake`**.

The heartbeat also has these safeguards and options:

- **Dead-man's switch.** If no tick is recorded for three intervals, the kernel raises an urgent notification.
- **Project heartbeats.** Coordinators can opt in to one. It reports to the coordinator, never to you.
- **Cost.** About 10K tokens a day, against about 4.8M for a heartbeat that resends Jarvis's full context [PRO §5.6].

### 10.4 Consent tiers and taint (P39)

These rules follow [PRO §5.4 R7].

| | Observe | Suggest (default ceiling) | Act |
|---|---|---|---|
| Read declared sources; update its own state | ✓ | ✓ | ✓ |
| Notify | Digest only | ✓, through the router | ✓ |
| Drafts, and approval requests that show the exact payload | – | ✓ | ✓ |
| Memory writes | Proposals only | Proposals only | Proposals only |
| External effects | ✗ | Only after you approve the payload | **Pre-granted** actions (allowed by **both** the routine and the pinned act-allowlist, reversible, with arguments bound to typed reader or report fields) run without approval. Any other external effect needs your approval of the payload. Never an `external_message` outside the allowlist, anything `irreversible`, payments, or permission changes |

- **Permissions are checked on every call (P39).** For each tool call the kernel evaluates one intersection:

  `spawn grant ∩ routine context ∩ taint state`

  - **The spawn grant** is the profile's tools ∩ the pinned policy. It is fixed at spawn, so the tool list and the cache prefix never change (D5).
  - **The routine context** comes from the routine or intent whose `Trigger` started the current turn: its tier and allowed actions.
    - The kernel records the context on that turn's first step (`TurnStarted{routine_ctx}`) and clears it when the turn ends.
    - A turn you started has no routine context; normal interactive policy applies.
    - So a routine that wakes Jarvis at `Observe` or `Suggest` really does narrow Jarvis for that turn. A call outside the context gets `ToolRefused{reason: tier}`.
  - **The taint state** applies to short-lived threads, as described below.
- **Act wins for pre-granted actions (P47).** Within an `Act` routine context, an action runs **without approval**, even in a turn that read reports, if it is:
  - in the routine's `allowed_actions` and the pinned act-allowlist;
  - reversible;
  - **bound to typed reader or report fields**.
  
  Each allowlisted action is declared in the routine bundle with its parameters bound to typed fields. The kernel checks that every call matches its binding.
  - **Bindings must be typed.** A parameter may bind only to an **id, enum, boolean, number or date** from a typed field, or to a **string checked against an allowlist** declared in the routine bundle.
  - **Untrusted free text never fills a pre-granted action's parameters.** That includes the reader's "bounded strings marked untrusted".
  - **The example complies.** In `email.label(id = reader.message_id, label = reader.category)`, `message_id` is an id and `category` is an enum. That's the point of the reader pattern: say, labelling email from typed reader output.
  - **Model-chosen external effects** need your approval of the exact payload (P8). That means any external effect outside the routine's grant, or not bound to typed fields, in any turn that read a report.
  - The same rule applies to run-scoped routine threads and to long-lived threads.
  - Bound fields must come from a reader or a report, so routine threads also read untrusted sources through the reader. A routine thread that read raw content is tainted (below), and taint blocks even pre-granted actions.
- **Long-lived threads never read raw untrusted content (P39; enforced in code, not just recommended).**
  - Jarvis and every coordinator, including the Research project's, get **no `reads_untrusted` tools**: no web fetch, no email bodies, no raw MCP output. The profile loader rejects such a grant for those roles.
  - Untrusted content reaches them only as **typed reports**:
    - **Amended by `docs/plans/subagents.md` (decision 6):** an agent's `report` is free text today; typed reports for taint come back with the security milestone. Until then a report is no less trusted than the old kinds' free-text `summary` and `findings` were.
    - from researcher workers;
    - from the tool-less `reader` role, through kernel tools like `read_web` or `read_mail`. These run the reader on the content and return only its typed output. That is a one-shot role call, not a spawned thread, so depth isn't affected.
  - Trigger payloads to long-lived threads are typed reader output too.
  - **The cost** is one reader hop, a single small call on the session's model, whenever a long-lived thread needs untrusted content.
  - **The benefit** is that injected text never sits in Jarvis's or a coordinator's history, so there is nothing to un-taint later.
  - **Report-bearing turns, defined by provenance.**
    - A turn is report-bearing **only if** it ingested reader output, a `ResearchReport`, or any report whose kernel-recorded provenance (P40) includes an **untrusted origin**.
    - Reports from workers whose inputs were all trusted don't count. So a coordinator reading ordinary worker `Report`s, or Jarvis reading a project's Report, can still auto-accept memories you stated (P48) and create the intents you ask for, such as "follow up Friday".
    - Report-bearing turns keep every tool. But in them:
    - model-chosen external effects need approval (above);
    - `intent_create` becomes a proposal in your inbox;
    - memory writes are proposals only.
    
    This blocks delayed-invocation tricks [PRO §4.4].
- **Taint rule for short-lived threads (Meta's Rule of Two)** [PRO §5.4]. This is the enforcement half of O7.
  - **What taints.** **Raw** untrusted content entering a worker or a routine run: a web page, an email body, MCP output, or a webhook payload.
  - **What it blocks.** Only **external effects**: tools that send, publish, pay, or act outside the thread's own workspace (tagged `external_message`, `irreversible`, or effects outside its lease). Editing files in its own leased worktree is not external, so a coder that read a web page keeps `edit` and `write` there.
  - **How it's enforced.** At **call time**, not by removing tools. A tainted external-effect call gets `ToolRefused{reason: tainted}`, and the cache stays intact [MAO Q3 #1].
  - **How long it lasts.** Until the run ends.
- **Quarantined reader.**
  - Untrusted sources are read by a `reader` role that has no tools.
  - Its output has a strict schema: enums, booleans, dates, and bounded strings marked untrusted.
  - Code consumes the typed fields.
- **Only you widen authority.**
  - Raising a tier or adding allowed actions needs a user command or a named policy.
  - Agents may lower a tier or remove actions.
  - A routine created at `Act` runs its first execution as a visible dry run.
- **Routines Jarvis proposes.**
  - Each goes through an approval. The approval shows a plain-language restatement, the tier, sources, actions, delivery, and the **projected monthly cost**.
  - An immediate visible test run follows the approval.
- **Suggestion pacing is enforced in code:**
  - at most 5 pending suggestions;
  - at most one new suggestion per conversation day;
  - a declined suggestion is never offered again.
- **The tier ceiling** lives in the pinned `policy.toml` (§6.2).

### 10.5 Memory (P40)

These scopes follow [PRO §5.3 R8–R9].

| Scope | Holds | Loaded |
|---|---|---|
| Global user profile (`home/profile/`) | Stable preferences and facts, plus a proactive-preferences file (topics wanted and unwanted) | As a snapshot in Jarvis's preamble; changes arrive as `ContextDelta` |
| Jarvis memory (`home/memory/`, git) | Facts, decisions, minimal notes on people | Small index plus search |
| Project memory (`projects/<id>/memory/`) | Project facts and conventions | Project preamble index plus search. Projects never see Jarvis memory |
| Prospective memory | Intents, routines, watches | **Kernel state**, not files |
| Episodic memory | What happened | **The event log itself**, searched with SQLite FTS5 |

- **Provenance is written by the kernel, not the model.**
  - Recorded fields: the origin (`user`, `agent`, `untrusted` or `kernel`; unknown never counts as `user`), source event references, validity dates and sensitivity.
  - Agents change memory only through the kernel `memory` tool. The sandbox never mounts memory directories.
- **Write paths.**
  - What you state in a turn that isn't report-bearing may be auto-accepted by the named policy `memory-user-stated` (P48). A turn is report-bearing only when it ingested untrusted-origin content (§10.4).
  - Background consolidation runs nightly on the session's model, like every role (§6.1), under the Background budget. It only **proposes**, into the review inbox. This closes O9.
  - **Trigger, heartbeat, routine and tainted runs, and report-bearing turns, never write memory directly; they can only propose.**
- **Sensitive topics are opt-in:** health, sex life, religion and politics, precise location, finances, and private details about third parties.
- **"Forget" is a real purge.**
  - It removes the entry, its git history, derived blobs, and everything provenance links to it.
  - It leaves a `MemoryForgotten` tombstone.
  - The event log is append-only and may still hold the original text, so the purge mechanism is the new **O18**. P50 proposes crypto-shredding as the default.
- **Incognito threads** neither read nor write memory, and create no intents.

### 10.6 Notifications (P41)

`Notification` is a durable event in `home.db`. It is routed by deterministic code, configured in the pinned policy [PRO §5.5 R10–R11].

- **Urgency is capped by policy.** Agents only propose urgency. `Urgent` survives only for:
  - approvals;
  - kernel alarms;
  - failures;
  - imminent deadlines from sources you allow to interrupt.
- **Quiet hours** (P45) let through only `Urgent` items from allowed sources.
- **Digests** at fixed times bundle everything else. Each ends with "that's all for now".
- **Delivery depends on presence.**
  - If a client is attached and active, the notification arrives there quietly.
  - Your phone is pushed only when no client is active.
  - Non-urgent items wait for a quiet moment.
- **Rate limits.** At most 3 `Normal` pushes a day, not counting approvals. Collapse keys remove duplicates.
- **Push carries no content.** The push itself says only "Wisp has an update". The client then fetches the content over the authenticated channel.
- **Usefulness is measured.**
  - "Acted", "dismissed" and "ignored" events feed a back-off for each topic.
  - A low-value routine is **auto-paused**, with an explanation in the digest.
  - User routines also pause after 14 deliveries in a row go unacknowledged.
- **Approvals use the same path,** with P8's semantics.

### 10.7 Background budget (P42)

These rules follow [PRO §5.6 R12–R13].

```
Global (per month)
 +-- Jarvis (interactive)
 +-- Jarvis.Background: heartbeat triage, routines, intents, consolidation   (P46)
 |      +-- Routine R (a cap per run and per month)
 |      +-- Research project (built in, §10.8): grants for research runs (caps in P52)
 +-- Project P (scheduled project runs draw from P)
```

- **Cost is visible up front.** The projected monthly cost is shown when a routine is created. The routine list shows each routine's actual monthly cost.
- **At a routine's cap,** the routine pauses and sends **one** notification.
- **At the Background cap,** the heartbeat drops to pre-checks only. **Model-free reminders and watches keep working**, so you never lose a reminder over money.
- **Unattended lifetime is bounded.**
  - Watches and loops that an agent created expire by default.
  - Every attempt gets a run record.

### 10.8 Proactive research (P52)

D10 includes doing research proactively. P52 proposes how. The research report cautions that the model-curated feed people couldn't steer (ChatGPT Pulse) was retired. What survives are briefings steered by the user's own stated goals [PRO §1, §5.8]. So research is triggered, run, delivered and paid for under the same rules as the rest of this section.

- **Triggers.** Research starts in one of two ways.
  - **Scheduled research routines** (P35). You create them, or Jarvis suggests them with your consent (P39's pacing and approval rules apply). For example, "every Monday, what's new in Rust async runtimes".
  - **Research prompted by events or context**, gated by the heartbeat's triage call (P37). Triage can return `research{topic, reason}`. P52 proposes that the kernel starts the run only for topics in three categories:
    - **what you care about:** your proactive-preferences "tell me about" list (§10.5);
    - **developments relevant to active projects:** topics a project publishes to Jarvis as research interests, in its `project.toml`;
    - **questions left open in earlier conversations:** recorded as **research intents**, under P36's rules (only in turns you started that aren't report-bearing).
  
  Anything else becomes a suggestion in your inbox, not a run.
- **Execution.**
  - P10 says Jarvis delegates only through projects, so research runs in a built-in, read-only **Research** project.
    - It has no write workspace and no secrets, and you approve it once, at setup.
    - **Its grants come from `Jarvis.Background`**, not from a normal project account (§10.7).
    - **It is isolated like any project (D2).** It has **no read access** to Jarvis memory or to other projects. Instead, the kernel puts **curated excerpts** into each researcher's brief: the topic, non-sensitive memory excerpts matched by topic key, titles and sources of earlier reports, and a project's published research interests.
  - Its coordinator assigns **read-only researcher workers**, using the existing researcher role and profiles, with **no write or send tools**. Depth stays at 2, and research never runs in Jarvis's main thread.
  - Web content is untrusted, so researchers are tainted from the start (P39) and can never trigger actions.
  - Each run returns a typed **`ResearchReport`**: claims with sources, confidence, and what is new since the last report on the topic.
  - Wide topics fan out to several researchers, once `fanout` exists (M11). Narrow ones use a single researcher.
- **Deduplication.**
  - Before a run, the kernel (not the Research project) checks what you already know: memory entries, plus earlier `ResearchReport`s on the same topic key, their sources, and their cooldown.
  - Those references go into the brief as excerpts, so the researcher looks only for what's new.
  - A run that finds nothing new ends `Silent`, at no further cost.
- **Delivery.** Through notification routing (P41).
  - By default, a finding is a **digest entry or a short brief**.
  - It becomes a push only if the pinned policy rates it urgent, for example a deadline you track.
  - Findings reach Jarvis only as typed `ResearchReport`s, never as raw web content (P39). In the turn that reads them:
    - model-chosen external effects need your approval of the exact payload;
    - actions an `Act` routine pre-granted, bound to typed report fields, run without approval (P47).
- **Memory.** Findings are **proposed** for memory, with provenance pointing to the report and its sources. Background runs **never write memory directly** (P40).
- **Cost.**
  - Charged to `Jarvis.Background` (P42), with research's own caps, separate from P46's $0.25-per-run routine cap.
  - **Proposed defaults:**
    - a research run's estimate and cap is $0.50 for a single researcher, and $2 for a wide fan-out run (M11);
    - each research routine has a $2-a-month cap.
    
    P46's $5 Background budget covers two weekly research routines at their $2 cap ($4) plus heartbeat triage only on a model up to about $3 per million input tokens: triage runs on the session's model by default and is about 300K tokens a month (§10.3), about $0.60 on Sonnet 5.5 and $1.20 on Opus 5.5 before output, and nightly consolidation also runs on the session's model by default. Raise the budget for more, or set a cheaper triage model once per-role models exist.
  - Triage, source scanning and synthesis all run on the session's model by default; a model per role is a later config option (§6.1). Synthesis uses the researcher profile's effort (P3: medium; low for fan-out scanners).
  - A routine shows its projected monthly cost when it's created.
- **Consent.** Research is observe- or suggest-tier work: it reads and reports. **Acting on findings** (sending, buying, changing anything) needs a pre-granted, field-bound `Act` action for that routine (P47), or your approval of the exact payload (P39).

### 10.9 Defaults to set

| ID | Question | Proposed default |
|---|---|---|
| P44 | Is the heartbeat on by default? | On, with pre-checks only. Triage switches on together with the Background budget |
| P45 | Quiet hours and digest times? | Quiet 22:00–07:00. Digests at 08:00, 13:00 and 18:00 |
| P46 | Background budget? | $5 a month. Per routine, $1 a month and $0.25 a run |
| P47 | Tier ceiling? | `Suggest`. Where you enable `Act` for a routine, its pre-granted actions (allowlisted, reversible, bound to typed reader or report fields) run without approval. Any other external effect still needs your approval of the payload |
| P48 | Auto-accept memories you stated? | Yes for non-sensitive ones, through `memory-user-stated`, in any turn that isn't report-bearing. Ordinary worker and project Reports don't make a turn report-bearing; only untrusted provenance does (§10.4) |
| P49 | Should Wisp act on external services under its own identity (its own mail alias and calendar)? | Yes for email and calendar actions. Connectors arrive read-only in M9; acting under Wisp's identity comes in M10. This makes actions attributable and revocable separately |
| P50 | How does "forget" purge content from an append-only log (O18)? | Crypto-shredding. User-content blobs are encrypted with per-entry keys, and forgetting deletes the key. This must be settled in M4's plan, because it shapes blob storage |
| P51 | Should the Jarvis milestone be split? | Yes: M9 is Jarvis and multiple projects; M10 is proactive Jarvis, with its own security review |

### 10.10 Where it lands (P43)

The research's mapping [PRO §5.7], renumbered to this doc's milestones:

| Milestone | Proactivity work |
|---|---|
| M4 durable history | The timer projection's shape and fire ids. The O18 decision, because it shapes blob storage |
| M5 tools | Capability tags, including `reads_untrusted`. Provenance and thread taint |
| M6 profiles and evals | `triage` and `reader` roles. Routine-bundle validation. A proactivity precision eval |
| M7 daemon | The scheduler. Model-free reminders and schedules. Notifications, the router, quiet hours, digests and presence. Daylight-saving tests |
| M8 projects | Routines that target projects. Run records. Project intents and the opt-in project heartbeat |
| M9 Jarvis | Memory scopes and provenance. `memory-user-stated`. Forget. The `Trigger` message. The Background account. **Read-only email and calendar connectors, on the MCP service** |
| M10 proactive Jarvis | Heartbeat. Intents. Routine proposals. Tiers enforced. The reader role. Usefulness tracking and auto-pause. Consolidation. **Proactive research** (§10.8) through the built-in Research project. Email and calendar watches, using M9's connectors. **Its own security review** |
| M11 fan-out | Fan-out for wide research topics |
| M3 TUI and later | Views ship with the milestone that produces their data: notification centre (M7); routines, intents and memory inbox (M10) |
| M12 remote | Content-free phone push |
| M13 self-tune | May tune triage prompts and thresholds. Never tiers or quiet hours |

**Not to build** [PRO §5.8]:

- follow-ups inferred by the model on every turn;
- a heartbeat in Jarvis's main thread;
- an LLM approver;
- memory written by background runs;
- follow-ups tuned for engagement;
- dossiers on third parties;
- connectors switched on by default for scheduled runs.

---

## 11. Crate and module layout

**Proposed (P20).** Start from today's code. Split a crate only once its boundary is proven.

```
crates/
  wisp-protocol/   conversation (today's types), config (ThreadConfig replaces SessionConfig), ids, command,
                   event (EventEnvelope, LiveEvent, families), bus, orchestration wire views, profile manifest;
                   schema/ (generated JSON Schema fixtures)
  wisp-core/       the kernel library
    thread/        <- agent.rs: step engine, tool loop, per-step commit, preemption, committable()
    provider/      trait, anthropic (single attempts; retries are the kernel's, P14), openai (M6), caps (capability table)
    pricing        prices only; micro-USD helpers
    context/       assembly, ContextDelta, fingerprints and miss causes, compaction
    tool/  hooks/  policy/  sandbox/  profile/
    store/         EventStore trait, reducer plumbing, in-memory store for tests
    bus/  orchestrate/  project/  jarvis/
    kernel.rs      command handling, scheduler, subscriptions, headless driver
    kit.rs         the Kit: builds an Agent from shared parts (connections from a per-path factory, model settings,
                   request options, retry, count-tokens, auto-compact, the tool builder, workspace, instructions,
                   the log store and redactions, start layers, settings and endpoint hosts, the clock); wisp-cli
                   builds its root through it, the agents service its children
    agents/        the agents service: the tree of named agents, spawns, Child routing, reports and the
                   turn-end guarantee, state and activity, quitting the tree (docs/agents.md)
  wisp-store/      SQLite EventStore: migrations, blobs, snapshots          (M4)
  wisp-client/     protocol client: started with the run helpers (channels, quit wait, interrupts, a turn);
                   UDS/WSS, handshake, reconnect                            (M7)
  wispd/           daemon: transport, auth, pairing, lifecycle              (M7, M12)
  wisp-cli/        binary: starts through wisp-start; wires transport; TUI or JSON output; run --headless
  wisp-start/      the start step shared by Wisp's programs: config, flags, keys and credentials into the Kit's
                   inputs, then the session opened or resumed and its root agent started; never depends on
                   wisp-tui or wisp-cli
  wisp-render/     pure rendering: markdown, highlight, diff, width, sanitise, theme   (M3)
  wisp-tui/        terminal layer, presenters, widgets, app loop                        (M3)
  wisp-media/      pure image sniffing, sizing and normalisation                        (M3.1)
  wisp-diff/       pure line, hunk and word diffs                                       (M3.1)
  wisp-index/      pure keyword ranking of tools, skills and documents                  (search-first)
  wisp-instructions/ pure instruction-file walker: choice, imports, render, large check (plugins)
  wisp-plugin-api/ the wisp:plugin contract: Rust mirror of wit/, manifest, Bundled     (plugins)
  wisp-tool-text/  pure tool wording: output, result text, jobs sentences, file and question sentences, terminal sizes, compaction tokens and instruction (plugins)
  wisp-contain/    the panic-containment flag for plugin calls, Markdown parses and the TUI's contained work, read by the TUI's panic hook (plugins)
  wisp-plugins/    bundled(): the bundled plugins, one Cargo feature each               (plugins)
  wisp-wasm-host/  WASM plugins on wasmtime: loading, the instance task, imports, cuts and restarts (plugins)
  wisp-wasm-guests/ builds the WASM guest crates for wasm32-wasip2, for tests          (plugins)
plugins/           the bundled plugin crates (docs/plugins.md, "Bundled plugins"); testkit (tests only); wasm-fixture, a WASM guest rather than a bundled plugin (tests only); board, the board plugin, a WASM guest installed with wisp plugin add
apps/              each app, a form factor, with the crates only it uses; wisp-cli, wisp-tui and wispd move here later
  learn/           the learning app: the flow's settings, the start through wisp-start, its lessons' folder (docs/learn.md)
  learn-web/       in apps/learn/web: the learning app's page and its host on this computer; never depends on wisp-core
  wisp-eval/       eval harness                                              (M6)
  wisp-tune/       optional self-tune job, a client                          (M13)
```

| Today | Becomes |
|---|---|
| `agent::Agent` (`run_turn`, `finish_turn`, `account`, `check_cache`) | `thread::Thread::step`. History becomes a projection; `account` moves to the ledger; `check_cache` moves to the fingerprint detector |
| `agent::spawn` (mpsc channels) | The kernel scheduler. Commands arrive through the inbox; cancellation uses a `CancellationToken` |
| `committable` (where `ToolUse` can't commit) | Kept. `ToolUse` becomes committable once the tool loop answers it |
| `SessionConfig` | `ThreadConfig`, built from a `RoleProfile`. `system` becomes `Vec<SystemBlock>` |
| `Request { session, messages, previous_response_id }` | Adds system blocks, tools, `CachePlan` and the request kind |
| `AgentEvent::Retrying`, and the adapter's retries | `AttemptRestarted`, emitted by the kernel step runner (P14) |
| `models::ModelRow::min_cacheable` | `provider::caps` |
| The `context`, `tool`, `policy` and `session` stubs | Filled in by M4 and M5. `session` is replaced by `store` and `thread` |

New dependencies, declared at workspace level: `rusqlite` (bundled), `tokio-util`, `blake3`, `schemars`, and a TOML parser. M3 adds the TUI stack (§8.4); M12 adds `ed25519-dalek` and `rustls`.

---

## 12. Milestones

**Proposed (P2), updated for D11 and P51.**

**Delivered so far:**

- **M1 (MR 1), bootstrap:** the Anthropic provider and terminal chat.
- **M2 (MR 4, in review), turn logs and formatted turns.** Every session is logged as JSONL behind a `SessionStore` trait (`docs/session-log.md`), and replies render as formatted Markdown. This is a stepping stone toward durable history:
  - M4 swaps the JSONL backend for the SQLite event store behind the same seam.
  - The JSONL format lives on as an export format and as the headless output style.
  - Its `message_committed` record already carries the message inline, as §8.1 requires.

**Process and checks.**

- **Every milestone follows the same steps (Decided, D7):** plan → your approval → code → review loop (D9) → fix → verify → MR → squash-merge.
- **Every milestone's checks:**
  - fmt, clippy and tests are green;
  - the prefix-stability test is extended;
  - every new event has a golden fixture;
  - live tests have stated cost caps.
- **Full acceptance criteria** for each milestone are in Appendix G.

| # | Milestone | Builds | Key acceptance | Main risk |
|---|---|---|---|---|
| M3 | TUI (D11) | `wisp-render` and `wisp-tui` on the in-process core: transcript model, inline presenter, streaming Markdown, highlighting, status line, composer, pager (plain and screen-reader modes were removed on 2026-10-24). Design from `docs/design/tui-brand.md` (orb mark). MR 4's formatting moves into `wisp-render` | Snapshot tests; escape-injection tests; terminal restored after a panic; no `wisp-core` dependency in either crate | Scope; terminal quirks on WSL |
| M4 | Durable threads | SQLite event store behind the `SessionStore` seam; per-step commit (P6); step journal; `AttemptRestarted` for the kernel retries (P14, delivered early on JSONL); resume; cache-miss causes; date delta; timer projection shape (P34); the O18 decision; port the TUI (M3) to the new event frames | A crash mid-stream resumes with byte-identical history; prefix test across resume; misses name their cause; timers rebuild from events under a fake clock | Event schema churn |
| M5 | Tools and `solo` | Tools with effect classes and capability tags (including `reads_untrusted`); provenance and thread taint (P39); policy; Linux sandbox; CommandGate and DiffGate, including the comment check (D12); a minimal `Run`; TUI views for tools, diffs and approvals | Gate failures return to the same thread; `EffectUnknown` after a crash; sandbox escape tests; a tainted thread's external-effect calls are refused at call time, while worktree edits still work; **security review** | Sandboxing on WSL2 |
| M6 | Profiles, headless, eval, second provider | Profile bundles (P30); `wisp run --headless`; `wisp-eval`; OpenAI-compatible adapter; `triage` and `reader` roles; routine-bundle validation; proactivity precision eval | Bundle diagnostics; stable hashes; Harness² conformance; first baseline sets P3; false-alarm baseline recorded | Eval cost |
| M7 | Daemon and reminders | `wispd` over UDS; cursor subscribe; multi-client attach; `wisp-client`; peer-cgroup check; **scheduler; model-free reminders and schedules; notifications, router, quiet hours, digests, presence**; TUI notification centre | Reconnecting clients get every event exactly once; schedules fire with no client attached; a restart past 3 slots fires once; daylight-saving tests; quiet hours hold a `Normal` item for the digest | Protocol churn |
| M8 | Projects: `solo` and `change` | Project v0; bus; board; flow engine; gates; D9 loop and decisions; ledger; worktrees; coordinator compaction; routines targeting projects; project intents | No agent path to approval; D9 loop and stall decisions; random-kill durability; Progress wakes nobody; scheduled project runs respect their grant | Size: about 5 MRs |
| M9 | Jarvis and multiple projects | Jarvis thread; memory scopes and provenance (P40); `memory-user-stated`; forget; `propose_project`; cross-database bus; secrets and egress proxy; the `Trigger` message; the Background account; read-only email and calendar connectors on the MCP service | Isolation; canary secret; Done reports wake no model; tainted and trigger turns can't write memory; forget leaves no trace; **security review** | Secrets; memory quality |
| M10 | Proactive Jarvis (P51) | Heartbeat (P37); intents (P36); routine proposals with cost and a test run; tiers enforced (P39); reader role; usefulness and auto-pause (P41); consolidation into the inbox; **proactive research** (P52); email and calendar watches (using M9's connectors); TUI views for routines, intents, findings and the memory inbox | 90%+ of heartbeat ticks in a recorded week make no model call; an injected calendar invite or web page can't cause a send, memory write or intent; one unanswered follow-up stops; a declined suggestion never returns; research never re-reports known findings; **its own security review** | Injection through triggers and web content; usefulness |
| M11 | Fan-out | `fanout` flow; pilot; barrier; parallel writers; merge gate; wide research topics | Every item accounted for; disjoint claims; conflicts escalate | Merge complexity |
| M12 | Remote access | WSS, pairing, scopes, revocation; content-free phone push; bot channels | Unpaired, revoked and replayed connections rejected; no content in push payloads; external security review | Attack surface |
| M13 | Self-tune | `wisp-tune`; also tunes triage prompts and thresholds, never tiers or quiet hours | Wins on a seeded suite; never touches the pinned layer | Overfitting |

- **Why this order.**
  1. **The TUI comes first (D11).** The in-process core is enough for chat, and the TUI depends only on the protocol.
  2. **Then durability**, because every later feature writes events.
  3. **Then a useful solo agent**, which is the baseline every flow must beat.
  4. **Then evals and profiles**, so flows are judged against solo at equal budget [MAO §4.9].
  5. **Then the daemon**, because projects run with no client attached. It also brings model-free reminders, so you get proactive value before Jarvis (M9) and proactive Jarvis (M10).
- **The TUI keeps growing.** Each later milestone ships the views for what it adds:
  - tools and approvals (M5);
  - notifications (M7);
  - the project tree, and the fullscreen presenter at parity (M8 onward);
  - routines, findings and the memory inbox (M10).

---

## 13. Decisions log

### 13.1 Decided

| ID | Decision (as the user decided it) | Source |
|---|---|---|
| D0 | Events are the source of truth, and every turn ends in exactly one typed terminal event. Providers sit behind a trait and round-trip their data losslessly. Authentication uses only documented means. Clients consume only the protocol | `CLAUDE.md` |
| D1 | Three layers: L1 the agent loop; L2 the orchestrator; L3 Jarvis, which is solo by default and spawns projects | User |
| D2 | Projects are long-lived, each with its own context. A project owns its instructions, knowledge, memory, event log (runs and artifacts), team and workspace (with policy and secret scope). It is isolated, with a curated read-only global slice. A coordinator with no write tools (interpreted in P31) leads coder, designer, researcher and reviewer workers. Projects run asynchronously, report summaries plus artifact references, and accept direct client attachment | User |
| D3 | A kernel message bus with typed messages down (Assign, Steer, Pause/Resume/Cancel, Broadcast) and up (Report, Escalate, ApprovalRequest, Progress). Hub and spoke; escalation one level at a time. Durable, ordered, idempotent. Delivered between turns as appended messages. Only actionable messages wake a model. **Urgent messages preempt** (P7 says which). Everything is visible. Siblings don't message each other directly **by default**. A per-project artifact board where owners write and everyone reads | User |
| D4 | Orchestration as kernel logic, adapted to the research: solo by default, with team flows as escalation; one writer per workspace; fixes to the same coder thread; tests as a code gate; depth ≤ 2 in code; nested budgets that pause at the cap; approvals only from the user or a named policy. Amended by `docs/plans/subagents.md` (decisions 2 and 3): depth is the setting `agents.max_depth` (default 2), a child is Wisp narrowed by its spawn with profiles as optional presets, and there are no budgets | User + [MAO] |
| D5 | Cost and caching rules from `CLAUDE.md`. **Amended by P6** (append-only history per step) and refined by P16 (tail TTL per thread kind) | `CLAUDE.md` |
| D6 | LLM-agnostic: a provider trait and a capability table per model, with the same behaviour and defaults on every model. Prompt and profile variants per model: deferred, owner rule 2026-10-23. Every role, side job and one-shot call uses the session's model by default; a model per role or agent type is a later user config option (owner, 2026-10-23: "every role uses the session's model by default. Later the user can configure a model per role/agent type (coder, research, writer, etc.)"; §6.1) | User |
| D7 | Conventional Commits, squash-merged MRs only, and agents never commit | User / `CONTRIBUTING.md` |
| D8 | Tunable: a headless mode that emits the protocol event stream as JSONL; profiles as file bundles (prompts, skills, scripts, hooks) that favour skills and scripts over long prompts; safety pinned in code-enforced policy that no profile or optimizer can weaken; self-tuning as an optional job outside the kernel. Everything else (typing, hashes, validation, caps, conventions) is P30 | User |
| D9 | The `change` review loop runs until an explicit APPROVED. The same reviewer and the same coder persist across rounds. There is no maximum number of rounds by default; a cap is optional. A round that makes no progress pauses the run and asks the user to decide. The definitions are in P28b | User |
| D10 | Jarvis is proactive: it follows up, sets up routines and schedules, accumulates memories, and does research proactively, helping the user from accumulated context and history. The user named Grok Bot (SpaceXSI), Meta Muse and OpenAI dots as inspiration | User |
| D11 | The real TUI is built right after the turn-logs milestone, on the in-process core, without waiting for the daemon | User |
| D12 | A comment-free code mode: the profile's code style can set `comments = "none"`; the coder role's doctrine states it and DiffGate rejects added comments. Off by default for users; Wisp's own repository uses it, and its Rust code carries no `//`, `///` or `//!` | User |

### 13.2 Your calls

| ID | Proposal | Why | Alternative |
|---|---|---|---|
| **P1** | **AI review by risk.** Each project sets `review = "risk"` (the default), `"always"` or `"never"`. Under `risk`, review runs when any trigger holds: ≥ 200 changed lines (lockfiles and generated files excluded), `risky_paths` globs touched (auth, crypto, migrations, CI, dependency manifests, policy and sandbox code), DiffGate flags on tests or acceptance criteria, a high-risk plan, or an explicit request. **Tests always run.** Once review runs, D9 applies. This repo would use `"always"` | In Claude Code Review, 31% of PRs under 50 lines still got a finding (0.5 on average), against 84% (7.5 on average) of PRs over 1,000 lines, at $15–25 per review [MAO §2b]. Value grows with size and risk. **The 200-line threshold is our inference, to be tuned by eval** | `always`: simplest and costliest |
| **P2** | **The build order in §12**, M3 through M13: the TUI first (D11), then durability, solo, evals, the daemon with reminders, projects, Jarvis, proactive Jarvis (P51), fan-out, remote access, self-tune | §12 "Why this order" | Daemon before the TUI |
| **P3** | **Effort per role until M6's evals:** Jarvis medium, coordinator medium, coder medium (high in `change`), reviewer high, researcher medium (low for fan-out explorers), designer medium, one-shot calls low. Every role runs on the session's model by default (D6, §6.1). Effort is set at spawn. To change a live thread's effort (for example, a solo coder carried into `change`, P11), use the **per-message effort delta** where the capability table says the model supports it, which preserves the cache per `CLAUDE.md`. Otherwise **keep the thread's effort**; never respawn just to change effort | Research effort curves are flat, while long coding work shows a real trade-off [MAO Q1]. A respawn throws away context and the warm cache | One effort for every role |
| **P4** | **Transport and authentication.** Local: a Unix socket (0700/0600), peer-uid check, sandboxed tools barred by cgroup (§8.3), no TCP. Remote: WSS (off by default), Ed25519 pairing, a signed challenge per connection, device scopes, revocation. Multi-client attach with resume from a cursor. JSON-RPC 2.0 | [HL §7.1 C.12–13]. Avoids the OpenCode and OpenClaw CVE classes. The remote half can be revisited in M12's plan | Localhost TCP with a token (rejected: a known CVE class) |
| **P6** | **History is append-only per step. This amends the `CLAUDE.md` rule** that "a cancelled or failed turn is rolled back completely". A cancelled or failed in-flight response is never committed; tool steps that ran stay, with real or synthetic results. `TurnOutcome::Cancelled` and `Error` report `committed_steps` | Side effects that happened can't be un-happened. History must match the world [HL §7.1 #2, #7] | Roll files back from a checkpoint on cancel. That can't undo external effects |
| **P8** | **Approvals and stall decisions go straight to the deciders** (the user's clients, or a named policy). This is an explicit exception to hub and spoke. Supervisors get non-waking notices and may deny or withdraw, never approve | Only the user or a policy can decide (D4). D9 sends stall decisions to the user. Hops in between would add latency and model wakes without any way to decide | Hop by hop through each level (uniform with D3; slower and costlier) |
| **P10** | **Projects are the only delegation unit from Jarvis.** Each new project costs one user approval, except that the built-in policy `ephemeral-readonly` (which the user can disable) auto-approves read-only, secret-free, ≤ $1, self-archiving projects. Jarvis's own solo work writes only to workspaces no project owns | One isolation and budget model. Friction stays low where the risk is low | Approve every project; or let Jarvis create projects freely |
| **P31** | **The coordinator's tools** ("no write tools", D2). Read-only `read`, `glob` and `grep`; board tools; bus tools; `start_run`, `propose_flow`, `project_status`; project `memory`. **No `write` or `edit`, and no shell** | It can't write even through a shell. It plans and briefs from what it reads | A sandboxed read-only shell (for `git log` and similar) |

Proactivity defaults P44–P51 are in §10.9.

### 13.3 Other proposals

| ID | Proposal | § |
|---|---|---|
| P5 | One SQLite file per project, plus `home.db`; `rusqlite`; WAL. The single-file alternative is in §7.1 | 7.1 |
| P7 | The only urgent message is `Cancel` (hard), with `stop_agent`. Everything else, agent mail and a user's steer included, arrives at a turn end or a step boundary (amended by `docs/plans/subagents.md`) | 4.4 |
| P9 | An escalation still unhandled at its deadline is forwarded one level up by the kernel | 4.2 |
| P11 | In-project `solo` delegates to exactly one coder. Escalating to `change` carries that coder thread over (effort per P3) | 2.4 |
| P12 | Only coders write workspaces. Others publish artifacts. Reviewers get a throwaway scratch copy | 5.7 |
| P13 | Integration is a kernel step. Policy denies `git commit/push` to agents. Default delivery is `manual`; pushing needs approval | 5.7 |
| P14 | A single retry layer, in the kernel. Partly delivered by resilience MR 2 (kernel step retries, `AgentEvent::Retrying` extended); `AttemptRestarted` replaces `AgentEvent::Retrying` in M4 | 7.2 |
| P15 | Wake and debounce defaults: one 2 s settle for every wake source (amended by `docs/plans/subagents.md`). Progress never enters history. Jarvis wakes for a Report only when asked to or when it needs attention | 4.4 |
| P16 | Tail TTL per thread kind (refines `CLAUDE.md`'s "5m for continuous loops"): loops that idle across gates or approvals use 1h | 9.5 |
| P17 | Integer micro-USD ledger. Grant sources as in §5.8. Overshoot at most one response per concurrent thread. A paused run ends `BudgetExhausted` only when a raise is declined or the optional pause deadline passes. Starting caps | 5.8 |
| P18 | Limits defaults, including piloting one item before a fan-out | 5.9 |
| P19 | JSON-RPC 2.0; generated schema fixtures; the protocol is unstable until M7, then changes only by capability flag | 8.1 |
| P20 | Crate layout | 11 |
| P21 | The reviewer uses the session's model by default, like every role (D6). A different reviewer model, including another family, comes with the later per-role model option; a seeded-bug reviewer eval can inform that choice | 9.1 |
| P22 | Model-proposed flows come later, as validated declarative data that the user approves | 1.2 |
| P23 | A second, OpenAI-compatible provider in M6 | 9.1 |
| P24 | Headless isolation defaults; approvals there resolve through a policy or are denied | 6.3 |
| P25 | Profiles and hooks may only narrow | 6.1 |
| P26 | Spawning never blocks a turn | 4.4 |
| P27 | One writer run per repo per project, except `fanout` or an opt-in with disjoint claims | 5.7 |
| P28 | Gate results include `NotConfigured`, which leads to `Unverified`. Completed verdicts are `Verified{grade}` or `Unverified`. `change` forces review when there is no check | 5.4 |
| P28b | Review-loop definitions: what counts as no progress, dispute deadlock as its own signal, the Decision options, and the ProgressGate placed after the verdict | 5.5 |
| P29 | Project data lives in Wisp's data directory. Repo instructions load only after the user trusts the workspace (M5; until then they load as `docs/instructions.md` describes) | 3.1 |
| P30 | Profile bundle specifics beyond D8: a typed manifest, load-time validation as events, one always-on cap in config for every model (8k per role by default, with role and user overrides; going over is an error; instructions are not capped and get a size warning instead), the hash recorded on every run and step, and the AGENTS.md / SKILL.md / Claude Code hook conventions | 6.1 |
| P32 | TUI: one transcript model. M3 shipped an inline presenter; M3.1 replaced it with full screen by the owner's decision, with selection and copy in its MR 4 and search later | 8.4 |
| P33 | Two client crates, `wisp-render` (pure) and `wisp-tui`, split now. Both depend only on the protocol. Protocol additions for stable ids, tool calls, history paging. Escape sanitisation at ingestion | 8.4 |
| P34 | One generic timer projection, shared with approval deadlines: fire ids; recurring slots coalesce into one run and are never backfilled; one-shot timers (deadlines, nudges, reminders) always fire once when overdue; uncertain deliveries are never re-sent; daylight-saving rules | 10.1 |
| P35 | Routines as versioned bundles; model-free reminders first (they ship in M7) | 10.1 |
| P36 | Follow-ups as kernel-tracked intents: time, event, or both paired; a reply cancels them; one unanswered nudge; expiry; none created in background, tainted or report-bearing turns (report-bearing is defined by untrusted provenance, §10.4) | 10.2 |
| P37 | Heartbeat: deterministic pre-check, then one small, isolated triage call on the session's model, never Jarvis's main thread; Jarvis woken only for action | 10.3 |
| P38 | A kernel → thread `Trigger` bus message that never counts as consent | 10.1 |
| P39 | Consent tiers enforced **per call**: spawn grant ∩ the triggering routine's tier and grant (attached to the turn it started) ∩ taint. Long-lived threads (Jarvis, coordinators) get no `reads_untrusted` tools and see untrusted content only as typed reports from readers or researchers; enforced in code. Taint for short-lived threads: raw untrusted content blocks external effects at call time until the run ends; worktree edits stay allowed. Suggestion pacing | 10.4 |
| P40 | Memory scopes with kernel-written provenance, which also defines report-bearing turns (§10.4); background runs never write memory; sensitive topics opt-in; forget as a real purge | 10.5 |
| P41 | Notification routing in code: urgency caps, quiet hours, digests, presence, rate limits, content-free push, per-topic back-off and auto-pause | 10.6 |
| P42 | `Jarvis.Background` budget: caps per routine, projected cost at creation, model-free fallback at the cap, bounded unattended lifetime | 10.7 |
| P43 | Where proactivity lands in the milestones | 10.10 |
| P44–P51 | Proactivity defaults: heartbeat, quiet hours, budget, tier ceiling, auto-accepting memories the user stated, Wisp's own identity, the purge mechanism, the M9/M10 split | 10.9 |
| P52 | Proactive research. Triggers: routines, or triage-gated topics in three categories (what you care about, project research interests, open questions as research intents). Runs: read-only, tainted researcher workers in a built-in Research project, funded from `Jarvis.Background`, with no access to memory or other projects (curated excerpts only). Output: typed `ResearchReport`s, deduplicated by the kernel, delivered as digest entries, with findings proposed for memory. Defaults: $0.50 per run ($2 when wide), $2 a month per routine. Observe or suggest tier | 10.8 |

### 13.4 Open

| ID | Question | Suggested direction |
|---|---|---|
| O1 | Read-only artifact links between projects | Later, as explicit grants the user approves |
| O2 | A fast path that skips the coordinator's turn when Jarvis's brief is already complete | Measure in M8 evals first |
| O3 | Voice client architecture [HL §7.5] | A realtime front end that delegates to Jarvis |
| O4 | Reaching the daemon off the LAN | Document VPN use first; a relay later |
| O5 | Designer role tools | Artifacts only, until a need appears |
| O6 | macOS and Windows sandbox parity | Seatbelt after M9 |
| O7 | How untrusted content is represented (the taint rule and reader role are now P39) | Provenance tags and taint in M5; tiers in M10 |
| O8 | Unverified caching behaviour: does a TTL switch keep hits, and does pre-warming work with thinking on? [PC §5.2.4, §5.2.9] | Test with diagnostics in M4 |
| O9 | Memory consolidation policy | **Closed by P40:** proposals go into the review inbox only |
| O10 | Retention for events and blobs | Keep events; collect blobs N days after archive |
| O11 | Serving Wisp's memory over MCP; an ACP gateway | After M10. The stdio MCP service is in the kernel (`docs/mcp.md`); the bundled plugin exposes its tools and local commands; HTTP support follows separately. **Read-only email and calendar connectors remain M9 work** |
| O12 | Accept syntect's `onig` C dependency (speed and the full grammar set) or use `fancy-regex` (pure Rust; loses four grammars and about half the speed)? [TUI §11] | **Closed by the M3 plan:** accept `onig`; CI already has a C toolchain |
| O13 | Plugin isolation: WASM or subprocess | Subprocess until a need appears |
| O14 | Write and upstream a Harness² substrate adapter? | Ship the CLI contract and a conformance test first |
| O15 | Jarvis as one thread: compaction thresholds, and two devices chatting at once | Queue per step boundary; tune in M9 |
| O16 | Opt-in direct messaging between specific sibling pairs (the router already supports a grant list) | Off until a flow shows a need; grants recorded and visible |
| O17 | TUI accent hue and motion | **Closed by `docs/design/tui-brand.md`:** wisp blue around the Halo orb, with the Medium motion tempos (M3 plan, call 1) |
| O18 | How "forget" purges content from the append-only event log | P50 proposes crypto-shredding: user-content blobs encrypted with per-entry keys, so deleting the key forgets. The alternative is redacting the matching blobs while keeping the audit record. **Settle this in M4's plan**, because it shapes blob storage [PRO §5.3] |

### 13.5 Changes to `CLAUDE.md`

These are made in a separate docs MR after this one merges (Appendix E).

- **Amends a rule:**
  - P6 replaces "a cancelled or failed turn is rolled back completely" (D5).
  - P16 refines "5m for continuous loops".
- **Additions** (no conflicts): the architecture table, the orchestration paragraph (`solo` is the default; the described flow is `change`; D9), a tunability section, a proactivity section (D10), and two prose rules turned into code checks.

---

## 14. Risks

| Risk | Mitigation |
|---|---|
| Multi-agent flows cost more and do worse than solo, since coding is a dependent chain [MAO §1, Q1] | Solo by default; `change` by escalation only; M8's equal-budget eval decides whether `change` stays opt-in |
| The unbounded review loop (D9) burns budget | ProgressGate after every verdict; the dispute-deadlock signal; budget pauses; cost per round visible; optional round cap |
| The coordinator's handoff cost in solo (P11) | Verbatim forwarding; measure it; the O2 fast path |
| Model behaviour drifts between versions [MAO Q1] | Caps in code; re-run evals on every model change. Per-model variants are deferred (owner rule, 2026-10-23) |
| Kernel and protocol bloat (Codex has about 250 methods) [HL §7.2] | 13 commands; "the kernel earns its place"; capability flags |
| Event schema lock-in | Versioning, migrations and golden fixtures from M4 |
| An always-on daemon's security (self-approval, secrets, injection) | Sandboxed tools can't see the socket and are barred by cgroup; all of `.git` protected; pinned policy; egress proxy; security reviews in M5, M9, M10 and M12 |
| Optimizers weakening safety (Harness² #94) | The pinned layer; profiles only narrow |
| Eval contamination (Harness² #111) | Isolated headless runs |
| TUI scope (Codex's TUI is about 461k lines) and terminal escape injection [TUI §10.6] | Inline first, one crate, sanitising at ingestion, snapshot tests |
| WSL2 specifics (user namespaces, `/mnt/c` locking, no systemd) | Detect and refuse with clear errors; the CLI starts the daemon |
| Unverified caching behaviour (O8) | Diagnostics mode; fingerprint miss causes |
| Long-lived Jarvis and coordinator context quality | Overlay compaction with pinned items; memory; idle compaction |
| Proactivity annoys or misfires: models raise false alarms 50–65% of the time [PRO §1] | Code gates whether anything runs or interrupts; digests and quiet hours; usefulness back-off and auto-pause; a precision eval (M6) |
| Injection through triggers and web content, such as a poisoned calendar invite or page [PRO §1] | Long-lived threads never read raw untrusted content; the taint rule for short-lived threads; the tool-less reader; triggers never count as consent; background runs never write memory; M10's own security review |
| Background cost creep | Background budget with caps per routine; pre-checks without a model; model-free fallback at the cap |

---

## Appendix A: Rust type sketches

These are sketches, not final APIs. Ids are opaque strings with a type prefix. Collections are `Vec` or `BTreeMap` only (D5).

### A.1 The bus (`wisp-protocol::bus`)

```rust
pub enum Address { User, Jarvis, Coordinator(ProjectId), Worker { project: ProjectId, thread: ThreadId } }

pub struct BusMessage {
    pub id: MessageId,                // idempotency key (derived for agents and kernel, client-supplied for users)
    pub from: Address,
    pub to: Address,
    pub provenance: Provenance,
    pub task: Option<TaskId>,
    pub causation: Option<MessageId>,
    pub body: Body,
}
pub enum Provenance {
    User { client: ClientId, device: DeviceId },
    Agent { thread: ThreadId, step: StepId },
    Kernel { run: Option<RunId> },
    Policy { name: String },
}
pub enum Body { Down(Down), Up(Up) }

pub enum Down {
    Assign(Assignment),
    Steer { text: String, refs: Vec<ArtifactRef> },
    Control(Control),                                    // Pause | Resume | Cancel { reason }
    Broadcast { text: String, standing_orders: Option<u32> },
}
pub struct Assignment {
    pub task: TaskId,
    pub brief: Brief,                 // validated before delivery (refuse-to-spawn)
    pub flow: Option<FlowRef>,        // a hint; the recipient's router decides
    pub budget: BudgetGrant,          // Jarvis->project: caps the run, drawn from the project account;
                                      // coordinator->worker: drawn from the run account (§5.8)
    pub on_report: ReportWake,        // Wake | Queue
    pub deadline: Option<Timestamp>,
}

pub enum Up { Report(Report), Escalate(Escalation), ApprovalRequest(ApprovalRequest), Progress(Progress) }

pub struct Report {
    pub task: TaskId,
    pub outcome: TaskOutcome,         // Done { verdict } | Failed { kind } | Cancelled | Rejected | BudgetExhausted | Escalated { to }
    pub summary: String,              // capped at 2,000 tokens by default
    pub artifacts: Vec<ArtifactRef>,
    pub structured: serde_json::Value,// checked against the brief's report schema
    pub cost: CostSummary,
    pub memory_proposals: Vec<MemoryProposal>,
}
pub struct Escalation {
    pub task: TaskId,
    pub kind: EscalationKind,         // NeedsInfo | Stuck | BudgetExhausted | PermissionNeeded | ScopeConflict | ProductDecision | Failure
    pub question: String,
    pub options: Vec<String>,
    pub default: Option<usize>,
    pub evidence: Vec<ArtifactRef>,
    pub deadline: Option<Timestamp>,
}
pub struct ApprovalRequest {
    pub approval: ApprovalId,
    pub kind: RequestKind,            // Approve { subject: ArtifactRef, subject_hash: BlobHash }
                                      // | Decide { subject: ArtifactRef, options: Vec<DecisionOption> }   (stall decisions, D9)
    pub question: String,
    pub deciders: Vec<Decider>,       // User | Policy(name). Never an agent
    pub on_timeout: TimeoutAction,    // Remind | Park | Reject. Never Approve unless a named policy decides
    pub deadline: Option<Timestamp>,
}
pub struct Progress { pub task: TaskId, pub phase: String, pub note: Option<String> }
```

### A.2 Protocol additions

```rust
pub struct EventEnvelope {
    pub stream: StreamRef,            // Home | Project(ProjectId)
    pub seq: u64,                     // monotonic per stream; the resume cursor
    pub at: Timestamp,                // for display only; never read by decide()
    pub thread: Option<ThreadId>,
    pub run: Option<RunId>,
    pub step: Option<StepId>,
    pub schema: u16,
    pub event: Event,
}
pub struct LiveEvent { pub stream: StreamRef, pub thread: ThreadId, pub step: StepId, pub after_seq: u64, pub event: LiveKind }

pub enum Event {
    Agent(AgentEvent),   // durable subset of today's enum (§8.1), with TurnStarted { routine_ctx: Option<RoutineCtx> }, plus:
                         //   ToolRefused { call_id, reason: RefusalReason }   // Tier | Tainted
                         //   MessageCommitted { message: Message, usage, cost }   // content inline, so JSONL logs and replay have it
                         //   ToolCallRequested { call_id, tool, args }
                         //   ToolCallFinished { call_id, result: ToolResultPayload }   // text, exit_code, patch, paths, truncated
                         //   CacheMiss { ..today's fields.., cause: MissCause }
    Bus(BusEvent),       // MessageSent, MessageReceived, MessageRejected, EscalationForwarded
    Thread(ThreadEvent), // ThreadSpawned { role, profile_hash, depth, parent, budget }, SpawnRefused, ThreadEnded
    Run(RunEvent),       // RouteDecided, RunStarted, RunPaused, RunResumed, RunMigrated, RunEnded,
                         // StepScheduled, StepCompleted, StepFailed, AttemptRestarted { attempt, delay_ms, reason }
    Gate(GateEvaluated), // including Stalled { signals } from the ProgressGate
    Approval(ApprovalEvent), // Requested, Decided, TimedOut, Resolved, Withdrawn
    Artifact(ArtifactEvent), // Published, OwnershipTransferred
    Budget(BudgetEvent),     // Granted, Warning, Paused, Raised
    Effect(EffectEvent),     // Intended, Completed, Unknown
    Project(ProjectEvent),   // Proposed, Created, StateChanged
    Client(ClientEvent),     // Attached, Detached, Lagged
    Diagnostic(LoadDiagnostic),
}

pub enum ClientCommand {
    Hello { protocol: u16, client: ClientInfo, caps: ClientCaps },
    Subscribe { scope: SubscribeScope, after_seq: Option<u64> },
    Unsubscribe { scope: SubscribeScope },
    Send { cmd: CommandId, to: Address, text: String, mode: DeliveryMode, turn: Option<u64>, replaces: Option<CommandId> },   // Queue | Steer
    Control { cmd: CommandId, target: Target, control: Control },
    DecideApproval { cmd: CommandId, approval: ApprovalId, subject_hash: BlobHash, decision: Decision },
    Answer { cmd: CommandId, call_id: String, answers: Vec<Answer> },   // a running tool's open question; the first valid answer wins
    CreateProject { cmd: CommandId, spec: ProjectSpec },
    SetProjectState { cmd: CommandId, project: ProjectId, state: ProjectState },
    SetBudget { cmd: CommandId, scope: BudgetScope, limit: BudgetLimit },
    ReadArtifact { cmd: CommandId, artifact: ArtifactRef },
    Query { cmd: CommandId, query: Query },  // Status | Approvals | Spend | SinceLastSeen | History { thread, before_seq, limit }
    Devices { cmd: CommandId, op: DeviceOp },// Pair | List | Revoke (M12)
}
pub enum Decision { Approve, ApproveWithEdits { edited: BlobHash }, Reject { reason: String }, Choose { option: String, text: Option<String> } }

pub enum RunOutcome {
    Completed { verdict: Verdict }, Failed { kind: FailKind }, BudgetExhausted,
    Cancelled, Rejected, TimedOut, Abandoned, Escalated { to: RunId },
}
pub enum Verdict { Verified { grade: VerifyGrade }, Unverified }  // a failed gate never reaches Completed
pub enum VerifyGrade { TestsPassed, Reviewed }                     // Reviewed implies TestsPassed
pub enum FailKind { GateFailed, SchemaInvalid, ProviderError(ErrorKind), PolicyDenied, EffectUnknown, NeedsUser, Crashed }
```

### A.3 Kernel primitives (`wisp-core::orchestrate`)

| Primitive | Essence | Key rules |
|---|---|---|
| `RoleProfile` | A role resolved from a bundle for one model: model (the session's model unless the user sets one for the role, a later option), effort, thinking, frozen system blocks, sorted tool allowlist, permission *requests*, workspace mode, context mode (`fresh`/`artifacts`/`fork`), output schema, budget ceiling, max turns, cache policy, `can_delegate` | Content-hashed. The hash, together with thinking and effort, identifies the cache prefix and is recorded on every step. Permissions are intersected with the pinned policy |
| `Flow` | A state machine as versioned data | Validated at load: everything reachable; every cycle bounded by `max_consecutive` or guarded by a progress gate; types match; file changes pass a CommandGate before `Completed` |
| `Run` | A flow instance: `flow@version+hash`, pinned profile hashes, state, a ledger account, leases | Exactly one `RunEnded`. M5 ships a minimal `Run` with a fixed solo sequence; M8 generalises it |
| `Step` | Durable work keyed `(run, thread, step_id, attempt)`: model call, tool call, child task, gate, approval or decision wait, timer | `step_id` comes from the triggering event's seq |
| `Thread` | One L1 agent: profile pin, parent, depth, status | Resumable; fix and review rounds continue the same threads |
| `Artifact` | A typed, versioned board entry | Single owner |
| `Brief` | Goal, scope, context refs, acceptance criteria, verify commands, report schema, standing orders, constraints | Refuse-to-spawn when incomplete |
| `Gate` | Evaluated by code | `Pass`, `Fail`, `Inconclusive`, `NotConfigured` or `Stalled` |
| `Approval` / `Decision` | Durable waits | First valid decision wins; stale hashes rejected; never decided by an agent |
| `BudgetLedger` | Hierarchical accounts folded from usage | Pause at the cap; overshoot at most one response per running thread |
| `Limits` | §5.9 | Pinned ceilings |
| `Workspace` | Worktree, read-only view, or scratch; leases with fencing; path claims | The sandbox mounts it writable only for the lease holder |

```rust
pub struct RoleProfile {
    pub role: RoleId, pub bundle: BundleHash, pub hash: ProfileHash,
    pub model: ModelRef, pub effort: Effort, pub thinking: ThinkingConfig,
    pub system: Vec<SystemBlock>, pub tools: Vec<ToolName>,
    pub permission_requests: Vec<PermissionRule>,
    pub workspace: WorkspaceMode, pub context: ContextMode, pub output: Option<SchemaRef>,
    pub budget_ceiling: BudgetLimit, pub max_turns: u32, pub cache: CachePolicy,
    pub can_delegate: bool, pub wake_debounce: Duration, pub inbox_format: InboxFormat,
}

pub struct Flow { pub id: FlowId, pub version: u32, pub hash: FlowHash, pub initial: StateId, pub states: Vec<State> }
pub struct State { pub id: StateId, pub kind: StateKind, pub retry: Option<StateId>, pub on: Vec<Transition> }
pub enum StateKind {
    Task { role: RoleId, thread: ThreadRef /* New | Same(StateId) | CarriedOrNew | Current */, inputs: Vec<ArtifactKind>, output: SchemaRef },
    Gate(Vec<GateSpec>),
    Approval(ApprovalSpec),
    Decision(DecisionSpec),           // options filtered by the origin state
    FanOut { role: RoleId, items: ArtifactKind, max_concurrency: u8, pilot: bool, per_item: Vec<GateSpec> },
    Barrier,
    Integrate(IntegrateSpec),
    Terminal(RunOutcomeKind),
}
pub struct Transition { pub when: Guard, pub to: Target /* State(id) | Origin | Retry */, pub max_consecutive: Option<u32>,
                        pub origins: Vec<StateId> /* only for Decision options */ }

pub enum GateSpec {
    Command { commands: Vec<CommandSpec> },
    Schema { schema: SchemaRef, retries: u8 },
    Diff { scope_from_plan: bool, flag_test_edits: bool, require_nonempty: bool },
    ReviewTrigger { policy: ReviewPolicy /* Risk | Always | Never */ },
    Review { max_rounds: Option<u32> /* D9: None */ },
    Progress { loop_kind: LoopKind /* Build | Review */ },
    Budget,
}
pub enum GateResult { Pass, Fail { reason: String }, Inconclusive { reason: String }, NotConfigured, Stalled { signals: Vec<StallSignal> } }
pub enum StallSignal { NothingResolved, Recurrence { finding: FindingId }, Oscillation, DisputeDeadlock { findings: Vec<FindingId> },
                       SameFailingTests, NoNewArtifact { steps: u16 } }

pub enum ReviewVerdict { Approved, ChangesRequested { findings: Vec<Finding> }, Inconclusive { reason: String } }
pub struct Finding { pub id: FindingId /* fingerprint: path + category + normalised claim */, pub severity: Severity,
                     pub location: Location, pub claim: String, pub evidence: Evidence, pub blocking: bool }
pub enum Triage { Fixed, Disputed { reason: String } }
pub enum Ruling { CoderUpheld { reason: String }, ReviewerUpheld }   // a user Decision on deadlocked findings

pub struct Spend { pub usd_micros: u64, pub tokens: u64, pub wall_ms: u64, pub agents: u32 }
pub struct Account { pub id: AccountId, pub scope: BudgetScope, pub parent: Option<AccountId>,
                     pub limit: BudgetLimit, pub granted: Spend, pub spent: Spend, pub pause_deadline: Option<Duration> }
pub struct Lease { pub holder: ThreadId, pub fencing: u64 }
pub const MAX_DEPTH: u8 = 2;          // D4: cannot be raised

pub trait Router { fn route(&self, input: &RouteInput) -> RouteDecision; }
pub trait Reducer { fn apply(&mut self, events: &[EventEnvelope]); fn decide(&self, now: Timestamp) -> Vec<KernelCommand>; }
```

### A.4 Proactivity (§10)

These types follow the research's sketch [PRO §5.2, §5.5], adjusted to this design.

```rust
pub enum TimerKind { ApprovalDeadline, BudgetPauseDeadline, ScheduleSlot, IntentDue, Heartbeat, Backoff, Expiry }
pub struct Timer { pub id: TimerId, pub kind: TimerKind, pub due: Instant, pub owner: TimerOwner }   // a projection, rebuilt at boot
// fire_id = H(trigger_id, slot_instant). Recurring slots: one TriggerFired{coalesced: n} within the window, else TriggerSkipped{stale}.
// One-shot timers (deadlines, IntentDue, At reminders, expiry, backoff) always fire once when overdue: TriggerFired{late_by}.

pub enum TimeSpec { Cron { expr: String, tz: Tz }, Every { period: Duration, anchor: Instant }, At(Instant) }
pub struct Watch {                       // deterministic event trigger; no model
    pub source: SourceRef,               // connector | local webhook | file | project event | bus
    pub filter: Filter,                  // sender allowlist, labels, path globs, event kinds
    pub dedup_key: KeyExpr,
    pub cooldown: Duration,
    pub condition: Option<ScriptRef>,    // sandboxed script, no model: { fire, message, small state }
}
pub enum Tier { Observe, Suggest, Act }
pub struct Routine {                     // user-owned, versioned bundle (routine.toml + SKILL.md), hash pinned per run
    pub id: RoutineId, pub version: u32, pub bundle_hash: BundleHash,
    pub time: Option<TimeSpec>, pub watch: Option<Watch>, pub window: ActiveWindow,
    pub target: RoutineTarget,           // Notify { text } | Triage | Jarvis | Project(ProjectId) | Research { topic: TopicKey }
    pub tier: Tier, pub allowed_actions: Vec<CapabilityTag>,
    pub budget: RoutineBudget,           // per run + per month
    pub delivery: DeliveryPolicy,
    pub expires_at: Option<Instant>, pub created_by: Principal,   // User | Policy(name)
    pub state: RoutineState,             // Active | Paused { reason } | Expired | Archived
}
pub struct Intent {                      // a follow-up (prospective memory)
    pub id: IntentId, pub scope: IntentScope /* Jarvis | Project(id) */, pub kind: IntentKind /* FollowUp | Research */,
    pub text: String, pub source: EventRef,                 // must be a user-started turn that isn't report-bearing
    pub trigger: IntentTrigger,          // Time(Instant) | Event(Watch) | Paired { at: Instant, cancel_on: Watch }
    pub max_unanswered: u8,              // 1
    pub expires_at: Instant,             // default 30 days
    pub state: IntentState,              // Armed | Fired | Answered | Done | Cancelled { by, reason } | Expired { why }
}
pub enum TriageVerdict { None, Digest(Vec<Item>), Notify(Item), Wake(Item), Propose(Proposal), Research { topic: TopicKey, reason: String } }
pub enum RoutineOutcome { Silent, Digested, Notified, Proposed, Acted, Failed { kind: FailKind } }

pub struct Notification {               // durable event in home.db
    pub id: NotificationId, pub source: SourceRef,
    pub urgency: Urgency,                // Silent | Digest | Normal | Urgent (capped by policy)
    pub collapse_key: Option<String>,
    pub title: String, pub body: String, // bounded; generated text labelled as such
    pub because: String,                 // "routine 'Morning brief' (daily 08:00)"
    pub actions: Vec<NotificationAction>,// Open | Snooze | Done | Dismiss | Mute(source) | Approve | Deny
    pub expires_at: Option<Instant>,
}
pub struct MemoryEntryMeta {            // written by the kernel, never by the model
    pub id: MemoryId, pub scope: MemoryScope, pub origin: Origin /* User | Agent | Untrusted | Kernel */,
    pub source: Vec<EventRef>, pub observed_at: Instant, pub valid_from: Instant,
    pub superseded_by: Option<MemoryId>, pub sensitivity: Sensitivity,
}
```

---

## Appendix B: Events and commands

| Family | Events | Durable? |
|---|---|---|
| Agent | `TurnStarted{turn, routine_ctx}`, `InputAccepted`, `InputHeld`, `InputUnheld`, `InputDelivered`, `InputWithdrawn`, `MessageCommitted`, `ToolCallRequested`, `QuestionAsked{call_id, questions}`, `QuestionClosed{call_id, how}`, `ToolCallFinished`, `SearchStarted{call_id, query, action}`, `SearchEnded{call_id, outcome}`, `Citations{index, sources}`, `ToolRefused{tier \| tainted}`, `Fallback`, `CacheMiss{cause}`, `Rejected`, `UnknownProviderEvent`, `InputTransformations`, `TurnEnded{…, committed_steps}` | Yes |
| Agent (live) | `BlockStarted`, `TextDelta`, `ThinkingDelta`, `BlockStopped`, `CacheDiagnostics`, `CacheStatus` | No, live only |
| Bus | `MessageSent`, `MessageReceived`, `MessageRejected{reason}`, `EscalationForwarded` | Yes |
| Thread | `ThreadSpawned`, `SpawnRefused{reason}`, `ThreadEnded{outcome}` | Yes |
| Run | `RouteDecided`, `RunStarted`, `RunPaused`, `RunResumed`, `RunMigrated`, `RunEnded`, `StepScheduled`, `StepCompleted`, `StepFailed`, `AttemptRestarted` | Yes |
| Gate | `GateEvaluated{gate, result, subject_hash, evidence}` (`result` includes `Stalled`; there is no separate stall event) | Yes |
| Approval | `ApprovalRequested` (approve or decide), `ApprovalDecided{decided_by}`, `ApprovalTimedOut`, `ApprovalResolved`, `ApprovalWithdrawn` | Yes |
| Artifact | `ArtifactPublished`, `OwnershipTransferred` | Yes |
| Budget | `BudgetGranted`, `BudgetWarning`, `BudgetPaused`, `BudgetRaised` | Yes |
| Effect | `EffectIntended`, `EffectCompleted`, `EffectUnknown` | Yes |
| Project | `ProjectProposed`, `ProjectCreated`, `ProjectStateChanged` | Yes |
| Client | `ClientAttached`, `ClientDetached`, `Lagged` | Yes (Lagged is sent to one connection only) |
| Trigger | `TriggerFired{fire_id, coalesced}`, `TriggerSkipped{reason}` | Yes |
| Routine | `RoutineCreated`, `RoutineRunStarted`, `RoutineRunEnded{outcome, cost}`, `RoutinePaused{reason}`, `RoutineExpired` | Yes |
| Intent | `IntentCreated`, `IntentFired`, `IntentAnswered`, `IntentCancelled{by, reason}`, `IntentExpired` | Yes |
| Notification | `NotificationCreated`, `NotificationDelivered{channel}`, `DeliveryUnknown`, `NotificationActed{action}`, `NotificationIgnored` | Yes |
| Memory | `MemoryProposed`, `MemoryAccepted{decided_by}`, `MemoryForgotten{by: user}` | Yes |
| External | `ExternalEventReceived{source, dedup_key, taint}` | Yes |
| Diagnostic | `LoadDiagnostic` | Yes |

**Commands (14).**

- `Hello`
- `Subscribe`, `Unsubscribe`
- `Send{mode: Queue | Steer}`. Steer is a mode of `Send`, not a separate command. It carries a turn precondition and `replaces`.
- `Control{Pause | Resume | Cancel}`. `Target::Input(id)` with `Cancel` withdraws a waiting message; `Target::Inbox` with `Resume` releases held ones
- `DecideApproval`
- `Answer{call_id, answers}`. Answers a running tool's open question (`docs/plans/ask.md`); it decides nothing for policy, so it is not `DecideApproval`. A bad answer, or one for a question that isn't open, is `Rejected` with kind `bad_answer` or `no_question`
- `CreateProject`, `SetProjectState`, `SetBudget`
- `ReadArtifact`
- `Query`
- `Devices`
- `Manage{object: Routine | Intent | Notification | Memory, op}`

---

## Appendix C: Built-in flows as data

Running this flow as kernel data is a later task (amended by `docs/plans/subagents.md`, decision 4).

The `change` flow below is valid TOML. Every transition target resolves to a state or to `@origin`/`@retry`. The schema is a sketch.

- **`@retry`** goes to the `retry` state that the stalled state declared.
- **`origins`** limits a Decision option to stalls that came from those states.
- **`max_consecutive`** counts consecutive uses of a transition and resets when the loop is left. When the limit is reached, the next failure takes `exhausted`. With 2, the 3rd consecutive CommandGate failure goes to a Decision, matching §5.9.

```toml
[flow]
id = "change"
version = 1
initial = "plan"

[[state]]
id = "plan"
kind = "task"
role = "coordinator"
thread = "current"
output = "Plan"                          # SchemaGate (2 retries) is implicit on every task output
retry = "plan"
on = [{ when = "pass", to = "approve_plan" }, { when = "fail", to = "stuck" }]

[[state]]
id = "approve_plan"
kind = "approval"
subject = "Plan"
deciders = ["user"]
on = [
  { when = "approved", to = "build" },
  { when = "edited", to = "build" },
  { when = "rejected", to = "end_rejected" },
]

[[state]]
id = "build"
kind = "task"
role = "coder"
thread = "carried_or_new"                # the solo coder carried over on escalation (P11), else a new one
inputs = ["Plan", "ResearchReport"]
output = "Submission"
on = [{ when = "pass", to = "verify" }]

[[state]]
id = "verify"
kind = "gate"
gates = ["command", "diff", "progress:build"]
retry = "build_again"
on = [
  { when = "pass", to = "review_needed" },
  { when = "not_configured", to = "review" },    # change forces review when there is no check (P28)
  { when = "fail", to = "build_again", max_consecutive = 2 },
  { when = "exhausted", to = "stuck" },          # the 3rd consecutive failure (§5.9)
  { when = "stalled", to = "stuck" },
]

[[state]]
id = "build_again"
kind = "task"
role = "coder"
thread = "same:build"                    # fixes go back to the same coder thread (D4)
inputs = ["TestReport"]
output = "Submission"
on = [{ when = "pass", to = "verify" }]

[[state]]
id = "review_needed"
kind = "gate"
gates = ["review_trigger"]               # P1; "fail" means no review is required
on = [{ when = "pass", to = "review" }, { when = "fail", to = "approve_final" }]

[[state]]
id = "review"
kind = "task"
role = "reviewer"
thread = "new"
context = "artifacts"                    # diff, plan, test report; never the coder's transcript
inputs = ["Diff", "Plan", "TestReport"]
output = "ReviewVerdict"
on = [{ when = "pass", to = "review_gate" }]

[[state]]
id = "review_gate"
kind = "gate"
gates = ["review", "progress:review"]    # runs AFTER each verdict, so it sees it (P28b)
retry = "fix"
on = [
  { when = "pass", to = "approve_final" },        # APPROVED
  { when = "fail", to = "fix" },                  # ChangesRequested and progress was made (or round 1)
  { when = "stalled", to = "stuck" },             # no progress, or only deadlocked findings remain
  { when = "inconclusive", to = "stuck" },
]

[[state]]
id = "fix"
kind = "task"
role = "coder"
thread = "same:build"
inputs = ["ReviewVerdict"]
output = "Triage"                        # per finding: Fixed, or Disputed with a reason
on = [{ when = "pass", to = "verify_fix" }]

[[state]]
id = "verify_fix"
kind = "gate"
gates = ["command", "diff"]
retry = "fix"
on = [
  { when = "pass", to = "re_review" },
  { when = "not_configured", to = "re_review" },
  { when = "fail", to = "fix", max_consecutive = 2 },     # the 3rd consecutive failure exhausts
  { when = "exhausted", to = "stuck" },
]

[[state]]
id = "re_review"
kind = "task"
role = "reviewer"
thread = "same:review"                   # D9: the same reviewer thread every round
inputs = ["Diff", "Triage", "Ruling"]
output = "ReviewVerdict"
on = [{ when = "pass", to = "review_gate" }]
# The fix -> verify_fix -> re_review -> review_gate cycle has no round cap (D9). The validator accepts it
# because "progress:review" guards it. A user-set max_review_rounds adds a bound on review_gate.

[[state]]
id = "stuck"
kind = "decision"                        # a typed user Decision, sent straight to the user (P8)
subject = "StallReport"
deciders = ["user"]
on = [
  { when = "guide", to = "@retry" },                                          # guidance arrives as a Steer
  { when = "side_with_coder", to = "re_review", origins = ["review_gate"] },  # disputed findings dismissed with the user's reason
  { when = "side_with_reviewer", to = "fix", origins = ["review_gate"] },     # disputed findings become mandatory
  { when = "re_plan", to = "plan" },
  { when = "stop", to = "end_stopped" },
]

[[state]]
id = "approve_final"
kind = "approval"
subject = "FinalPackage"                 # diff, TestReport, findings, rulings, cost per round, verdict
deciders = ["user"]
on = [{ when = "approved", to = "integrate" }, { when = "rejected", to = "end_rejected" }]

[[state]]
id = "integrate"
kind = "integrate"
policy = "project"                       # manual | local-merge | mr (P13)
retry = "integrate"
on = [{ when = "pass", to = "end_completed" }, { when = "fail", to = "stuck" }]

[[state]]
id = "end_completed"
kind = "terminal"
outcome = "completed"

[[state]]
id = "end_rejected"
kind = "terminal"
outcome = "rejected"

[[state]]
id = "end_stopped"
kind = "terminal"
outcome = "cancelled"
```

- **Dispute deadlock with other findings still open.** It doesn't route to `stuck`. The kernel raises a non-blocking Decision on the deadlocked findings, and `review_gate` keeps returning `fail` (→ `fix`) for the rest. The user's `Ruling` feeds into `re_review`.
- **`solo`** is `build → verify ⇄ build_again → review_needed → [the same review states] → report`, with no plan approval and no final approval. A stall there offers guide, escalate to `change`, a stronger model, or stop.
- **`fanout`** (M11) uses `kind = "fanout"` with `pilot = true` and a `barrier`.
- **Load-time validation** checks that:
  - every state is reachable;
  - every cycle is bounded or guarded by a progress gate;
  - every path that changes files passes a `command` gate before `end_completed`;
  - input and output types match.

---

## Appendix D: Traceability to the research

| Design element | Research basis |
|---|---|
| Solo default; `change` as escalation; equal-budget evals | [MAO §1, Q1, §4.1, §4.9] |
| Gates in code; CommandGate never asks a model | [MAO §4.2, Q4–Q5]; [PS §8.2–8.3] |
| Same coder thread for fixes; resume rather than respawn | [MAO §4.1 #2, Q3 #4] |
| Reviewer independence (artifacts only, evidence per finding) | [MAO Q5]; [PS §8.2] |
| Approvals only from the user or a policy; stale-hash rejection | [MAO Q2, Q6, §4.2]; [HL §7.1 C.12] |
| Hierarchical ledger pausing at the cap | [MAO Q2, §4.2] |
| Typed brief with refuse-to-spawn; report schemas | [PS §8.2]; [MAO Q2] |
| Event-sourced reducer, step journal, effect classes, version pinning | [MAO Q7, §4.6]; [HL §7.1 B] |
| Small protocol; multi-client attach; cursor resume; UDS and pairing | [HL §7.1 C, §7.3, §7.4] |
| Cache-first assembly; delta channel; fingerprints; TTL split; staggering | [PC §5.2]; [MAO Q3] |
| Profile bundles, validation, hashing, headless mode, pinned safety | [H2 §2.2, §2.8, §4, §9.1–9.3] |
| Capability table; per-model variants (deferred, owner rule 2026-10-23); second adapter | [HL §7.1 G]; [PC §5.3 #8] |
| Sandbox, egress proxy, secret substitution, provenance | [HL §7.1 F]; [H2 §9.2] |
| Memory as files with a review inbox | [HL §5.6, §7.1 J] |
| TUI crates, one transcript model, inline first, sanitisation | [TUI §7.5, §10.2–10.3, §10.6] |
| Proactivity: timers, routines, intents, heartbeat, tiers and taint, memory scopes, notification routing, background budget, research | [PRO §5] |

**Where the user's decisions override the research's defaults** [MAO §4.8]:

- **Depth.** The research suggests 1; the user decided 2 (Jarvis → coordinator → workers).
- **Review rounds.** The research suggests 2 plus a stop after two rounds without progress; the user decided unbounded rounds until APPROVED, guarded by the ProgressGate (D9).
- **Reviewer effort.** `CLAUDE.md`'s high-effort researchers and reviewers describe the human development process, not product defaults; P3 sets the product defaults.

---

## Appendix E: `CLAUDE.md` changes after this merges

1. **P6 (amends D5).** Replace "a cancelled or failed turn is rolled back completely" with: "History is append-only per step. A cancelled or failed in-flight model response is never committed; tool calls that ran are always recorded with their real or a synthetic result, and the turn's outcome lists the committed steps."
2. **P16 (refines D5).** "The tail TTL is chosen per thread kind: 1h for threads that idle across gates, approvals or user turns; 5m for short-lived workers."
3. **Architecture table.** Add `wisp-store`, `wisp-client`, `wisp-render`, `wisp-tui`, `wisp-eval` and `wisp-tune`, and the new `wisp-core` modules.
4. **Orchestration paragraph.** `solo` is the default tier. The described flow is `change`. Its review loop runs until APPROVED (D9).
5. **Tunability.** Profile bundles, the pinned layer, and headless mode (D8, P30).
6. **Proactivity (D10).** A short section: triggers never count as consent, background runs never write memory, and background work has its own budget.
7. **Prose rules become code.** A clippy `disallowed-types` rule for wire types (M4). A CI dependency check that clients don't depend on `wisp-core`: from M3 for `wisp-render` and `wisp-tui`, and from M7 for every client, once `wisp-cli` moves onto `wisp-client` (until then `wisp-cli` legitimately wires the in-process core).

---

## Appendix F: Headless contract and remote pairing

### F.1 Harness² `Substrate` mapping (D8)

| Substrate method [H2 §5] | Wisp |
|---|---|
| `surface()` | `wisp profile surface --json` returns the bundle's component paths, the always-on cap, frontmatter schemas and hook events (the equivalent of a `SurfaceSpec`) |
| `materialise()` | Write the bundle directory, then run `wisp profile validate <dir> --json`. It emits `LoadDiagnostic`s and the bundle hash |
| `execute()` | `wisp run --headless --profile <dir> --workspace <dir> --model <provider/model> --task <file> --events jsonl [--budget-usd N] [--max-turns N] [--state-dir <dir>] [--approvals <policy>\|deny] [--harvest <subpath>]` |
| `read_trajectory()` | The JSONL file of durable `EventEnvelope`s. `wisp trajectory render --format actions` produces the `[ASSISTANT] / [THINKING] / [ACTION] / [OBSERVATION]` grammar with explicit truncation markers |
| `preflight()` | `wisp preflight --profile <dir> --model <m>` checks credentials, the model's entry in the capability table, sandbox availability, profile validity and budget. **It never calls a model** |

The kernel also offers:

- isolated workspaces with copy-in and harvest-out;
- `wisp profile diff`;
- forking a recorded run at step N (§7.6);
- the per-run cost ledger, summarised in `RunEnded`.

**Exit codes**, one per outcome:

| Code | Outcome |
|---|---|
| 0 | `completed{verified}` |
| 10 | `completed{unverified}` |
| 20 | `failed{kind}` (including `needs_user`) |
| 30 | `budget_exhausted` |
| 40 | `cancelled` |
| 41 | `rejected` |
| 50 | `timed_out` |
| 60 | `abandoned` |
| 70 | `escalated{to}` (handed to a new run; only possible when a policy approves the next run's gates) |
| 2 | Usage or preflight error, before any run started |

### F.2 Remote device pairing (P4, M12)

1. **Start pairing.** On the host, `wisp devices pair` (or a trusted client) shows a one-time code or QR code. It contains the endpoint, the daemon's certificate fingerprint and a nonce.
2. **Connect.** The new device generates an Ed25519 keypair, connects over WSS, pins the fingerprint and presents the code.
3. **Confirm.** A trusted client asks "Pair device '<name>' with scopes […]?", and the user confirms.
4. **Store.** The daemon stores the device's public key, scopes and name.
5. **Every later connection.** The device signs `(nonce ‖ daemon fingerprint ‖ time)`. The daemon then issues a session token that lasts a few minutes, used only for reconnects. Handshakes are rate-limited.
6. **Revoke.** `wisp devices revoke <id>` invalidates the key and all live sessions at once.

---

## Appendix G: Acceptance criteria for each milestone

Every milestone also meets the common checks in §12. Live tests use low effort.

### M2: turn logs and formatted turns (MR 4, in review)

Delivered as a stepping stone toward M4.

- **Session logs.** Every session is recorded as append-only JSONL behind a `SessionStore` trait, with a `JsonlStore` backend for disk and a `MemoryStore` backend for tests. The format is in `docs/session-log.md`.
- **Records.** A schema version and gapless `seq`. `message_committed` carries the message inline. No credentials are ever logged, which a canary test checks.
- **Formatting.** Replies render as Markdown in the CLI.
- **Acceptance.** Defined in MR 4 itself.

### M3: TUI (D11)

- **Scope.**
  - **`wisp-render`:** a transcript model, streaming Markdown (newline gate, finalised blocks), highlighting, width and wrapping, sanitisation, and theme tokens. MR 4's `markdown` and `render` modules move here.
  - **`wisp-tui`:** the terminal layer and capability probe, guard and restore, synchronized output, the inline presenter, a status line, a composer, and a transcript pager.
  - **The visual design** comes from `docs/design/tui-brand.md`: the orb mark, palette, glyphs and animation, with reduced-motion fallbacks. The milestone plan is `docs/plans/m3-tui.md`.
  - **`wisp-cli`** wires the in-process core and chooses the full screen or JSON output; off a terminal it exits with an error unless `--output jsonl` is given. `learn` serves the learning app's page instead (docs/learn.md).
  - **Stable turn and block ids** are added to the protocol as the TUI needs them (§8.1).
  - Diff rendering moved to M5, the first milestone that produces diffs; M3 only tints ```` ```diff ```` fences.
- **Acceptance.**
  - `insta`/`vt100` snapshot tests at 80 and 120 columns, in the dark theme (the light theme's snapshots are dropped for now; `docs/testing.md`, "Snapshot themes").
  - Escape-injection tests. Title-set sequences, OSC 8 links, cursor moves and bidi characters in model output are all neutralised, on both the buffer path and the scrollback path.
  - The terminal is restored after a panic.
  - Cancelling with Ctrl-C works mid-stream.
  - The theme follows the brand spec's tokens. A clippy `disallowed-methods` rule bans raw colours outside `wisp-render::theme`.
  - CI shows no `wisp-core` dependency in `wisp-render` or `wisp-tui`.
- **Risks.**
  - Scope: the Codex TUI is about 461k lines, so phase hard.
  - Windows and WSL terminal quirks.
  - `crossterm` stewardship [TUI §10.6].
  - The brand spec arriving late. Mitigation: theme tokens make it a token swap.

### M4: durable threads and the event log

- **Scope.**
  - `wisp-store`: a SQLite event store behind the `SessionStore` seam from M2, with an in-memory version for tests.
  - `EventEnvelope` and `LiveEvent`.
  - History as a projection, committed per step (P6).
  - The step journal.
  - The kernel retry layer (P14); `AttemptRestarted` replaces `Retrying`.
  - `MessageCommitted`.
  - `wisp --resume`.
  - `CacheMiss{cause}` and `CacheStatus`.
  - The date `ContextDelta`, only for sessions with message stamps off (per-message stamps cover the date otherwise).
  - A skeleton capability table.
  - Clippy `disallowed-types`.
  - **The generic timer projection's shape, and fire ids (P34).**
  - **The O18 decision** (crypto-shredding or redaction), before the blob format is fixed.
  - **Port the TUI (M3)** from today's in-process `AgentEvent` stream to the new `EventEnvelope` and `LiveEvent` frames.
- **Acceptance.**
  - SIGKILL mid-stream, then restart: history equals the last committed step, and nothing commits twice.
  - Replay, or the JSONL output, reproduces the message text.
  - The prefix-stability test covers:
    - resume-from-disk byte equality, including tools held or off at startup;
    - cancelled-step rollback;
    - a day rollover.
  - Golden fixtures and a migration test.
  - The correct `CacheMiss.cause` in each scripted scenario.
  - The timer projection is rebuilt from events, and a property test shows `decide` is deterministic under a fake clock.
  - Live test (≤ $0.10): restart, then a cache read on the next turn.
- **Risks.**
  - Event schema churn.
  - SQLite on `/mnt/c`.

### M5: tools, policy, sandbox, and `solo` with the test gate

- **Scope.**
  - Tools with effect classes and capability tags: `reversible`, `irreversible`, `external_message` and `reads_untrusted`.
  - Read, edit, write, glob, grep and shell.
  - Diff rendering in `wisp-render` (`similar`, `diffy`, word emphasis, side by side), moved here from M3.
  - The tool loop.
  - `EffectIntended` and reconciliation.
  - Tool-call events.
  - **Content provenance and thread taint (P39).**
  - Pinned policy, with `git commit` and `git push` denied.
  - Durable approvals for "ask".
  - The Linux sandbox:
    - only the worktree is writable, mounted at `/workspace`;
    - all of `.git` is read-only;
    - Wisp's socket, data and config are hidden;
    - no network;
    - a scrubbed environment;
    - a tool cgroup.
  - Trust and leases.
  - **A minimal `Run`**: the fixed solo sequence, with `RunEnded` and verdicts.
  - A per-thread budget.
  - TUI views for tools, diffs and approvals.
- **Acceptance.**
  - A cancel during a tool run leaves a replayable history and reports `committed_steps`.
  - A crash between intent and completion yields `EffectUnknown`.
  - A break-then-fix cycle returns to the same thread and ends `Completed{Verified}`; with no check, the run ends `Unverified`.
  - No gate code calls a provider.
  - Sandbox tests show a shell cannot:
    - write outside `/workspace` or into `.git`;
    - reach the network;
    - see the socket, the config, or the data directory beyond its worktree;
    - connect from inside the tool cgroup.
  - Kernel git calls run with hooks and fsmonitor disabled.
  - **A thread tainted by a `reads_untrusted` tool gets `ToolRefused{tainted}` on any external-effect call, but can still edit inside its own worktree.** Its tool list (and so its cache prefix) is unchanged.
  - Live test (≤ $0.50): "fix the failing test".
  - **Security review before merge.**
- **Risks.**
  - User namespaces and cgroups on WSL2.
  - Edit-tool reliability.

### M6: profiles, headless mode, the eval harness, and a second provider

- **Scope** (four MRs, plus proactivity groundwork):
  - (a) Profile bundles (P30).
  - (b) `wisp run --headless`, emitting M5's `RunEnded`, plus the `profile` commands, `preflight` and `trajectory render`.
  - (c) `wisp-eval`.
  - (d) An OpenAI-compatible adapter.
  - Also: **the `triage` and `reader` role profiles; routine-bundle validation (`routine.toml` plus `SKILL.md`); a proactivity precision eval** [PRO §5.5 R11].
- **Acceptance.**
  - Actionable diagnostics; nothing is dropped silently.
  - Stable hashes.
  - Hooks can't override a pinned deny.
  - Harness² conformance.
  - Deterministic cassette evals.
  - The first live baseline is committed to `docs/evals/` (two providers, at most $15). It sets the P3 values.
  - **Invalid routine bundles produce diagnostics, and a false-alarm baseline is recorded.**
- **Risks.**
  - Eval cost.
  - Toy tasks.
  - Bundle churn.

### M7: the daemon and local chat link

- **Built.** Per-chat hosts, one window per chat, and an always-on daemon that
  only lists, starts and relays; `wisp-client` and permanent-frame fixtures;
  uid, ancestry and job checks; bounded replay and slow-client handling;
  mid-turn reopen without a changed provider prefix or new log record.
  Stopping or reinstalling the daemon leaves chats working. WSL's question and
  status notes explain the Windows setting needed for closed terminals.
  `docs/daemon.md` specifies the local link.
- **Still proposed.** Multiple live clients, presence, sandbox/cgroup admission,
  network pairing and login startup, plus:
  - **Proactivity:**
    - the scheduler, running headless (P34);
    - user-created **reminders and schedules with `Notify`**, which are model-free (P35);
    - `Notification` events and the router for attached clients: quiet hours, digests, presence, collapse keys and rate limits (P41);
    - daylight-saving rules;
    - `DeliveryUnknown` in the catch-up view;
    - the TUI notification centre.
- **Proposed acceptance for the remaining work.**
  - Approvals broadcast and resolve across clients.
  - **Schedules fire with no client attached.**
  - **A restart past 3 slots fires once, coalesced.**
  - **Spring-forward and fall-back tests pass.**
  - **Quiet hours hold a `Normal` notification until the next digest.**
  - **An uncertain delivery is never re-sent.**
- **Remaining risks.** Protocol expansion and daemon lifecycle on WSL2.

### M8: projects with `solo` and `change`

- **Scope** (about 5 MRs):
  - Project v0.
  - Default roles (coordinator with the P31 tools, coder, reviewer, researcher, a stub designer).
  - The bus.
  - The board.
  - The flow engine and validator, replacing M5's fixed sequence.
  - The `solo` and `change` flows.
  - All gates, the ProgressGate and stall Decisions.
  - The ledger.
  - Limits.
  - Worktrees.
  - Coordinator compaction.
  - CLI and TUI `attach` with a thread tree.
  - **Routines that target projects (a kernel `Assign` with a grant from the project), routine run records, project-scoped intents, and the opt-in project heartbeat.**
- **Acceptance.**
  - **Routing.** Spawn depth is enforced, and sibling messages are rejected.
  - **Solo.** A gate failure returns to the same thread, and escalation carries the coder over.
  - **Change.**
    - No agent path can satisfy an approval, and stale hashes are rejected.
    - The review loop runs until APPROVED with the same threads.
    - The P28b progress rules and dispute deadlock behave as documented.
    - Every Decision option leads to its documented transition.
    - The final approval lists rulings and cost per round.
  - **Budget.** At the cap the run pauses. A raise resumes it; declining ends it `BudgetExhausted`.
  - **Durability.** A random-kill property test passes.
  - **Waking.** Progress wakes nobody, and debounced Reports produce one coordinator turn.
  - **Preemption.** Cancel cascades.
  - **Compaction.** It keeps pinned items.
  - **Scheduled project runs** respect their grant, pause at the cap, and leave a run record for every attempt.
  - **Live test** (≤ $5): one `change` run.
  - **Eval.** `solo` vs `change` at equal budget.
- **Risks.**
  - Size.
  - Coordinator prompt quality.
  - Multi-agent overhead [MAO §1].

### M9: Jarvis and multiple projects

- **Scope.**
  - The Jarvis thread, with compaction.
  - `propose_project`, with `ephemeral-readonly`.
  - Bus delivery across databases.
  - Isolation and the global slice.
  - Secrets, the egress proxy and redaction.
  - `project_status` and the catch-up view.
  - **Memory per P40:**
    - scopes, with kernel-written provenance;
    - the `memory-user-stated` policy;
    - the review inbox;
    - sensitive topics opt-in;
    - incognito;
    - forget with lineage (per O18 and P50).
  - **The `Trigger` message (P38), and the `Jarvis.Background` account (P42).**
  - **Read-only email and calendar connectors, using the kernel's MCP service.** Their credentials live in the secret scope, they reach the network only through the egress proxy, and everything they return is recorded as untrusted.
- **Acceptance.**
  - **Isolation.** Isolation tests pass.
  - **Secrets.** The canary secret never leaks.
  - **Waking.** Done reports wake no model; failures wake Jarvis.
  - **Approvals.** Project approvals and policy auto-approval are recorded.
  - **Restart.** Three projects interrupted mid-run all resume.
  - **Memory.**
    - Tainted, trigger, background and report-bearing turns can't write memory directly.
    - A forget leaves no trace in memory files or blobs.
    - A trigger turn can't approve anything.
    - Jarvis's and coordinators' profiles can't be granted a `reads_untrusted` tool (load error). `read_web` and `read_mail` return only typed reader output.
  - **Connectors.**
    - Connector credentials never appear in events or prompts.
    - Connector output is recorded as untrusted. Only reader roles see it raw, and long-lived threads get typed reports.
  - **Security review** before merge.
- **Risks.**
  - Jarvis drift.
  - Memory quality.
  - Secrets.

### M10: proactive Jarvis (P51)

- **Scope.**
  - The heartbeat (P37): pre-check, isolated triage, wake only on `wake`, and the dead-man's switch.
  - Intents (P36): paired triggers, the one-unanswered rule, expiry, and refusal in tainted or report-bearing turns.
  - Routine proposals: a plain-language restatement, projected cost, and a visible test run.
  - Tiers enforced, with the tool grant computed (P39).
  - The quarantined reader.
  - Suggestion pacing.
  - Usefulness tracking, per-topic back-off and auto-pause (P41).
  - Nightly consolidation into the review inbox.
  - **Proactive research (P52):**
    - research routines and research intents;
    - triage-gated research runs in the built-in Research project;
    - deduplication against memory and earlier reports;
    - findings delivered as digest entries;
    - memory proposals with provenance.
  - Email and calendar watches, using M9's connectors.
  - TUI views for routines, intents, findings and the memory inbox.
- **Acceptance.**
  - **Heartbeat.** In a recorded week, at least 90% of heartbeat ticks make no model call.
  - **Injection.** An injected calendar invite, email or web page can't cause a send, a memory write or an intent.
  - **Follow-ups.** A follow-up that goes unanswered once stops.
  - **Suggestions.** A declined suggestion is never offered again.
  - **Routines.**
    - A routine created at `Act` runs its first execution dry.
    - An `Act` routine labels email from typed reader output without approval. A model-chosen send in the same turn needs approval.
    - A `Suggest` routine that wakes Jarvis gets out-of-tier calls refused (`ToolRefused{tier}`).
  - **Research.**
    - A research run that finds nothing new ends `Silent`.
    - A topic already covered by memory or by a recent report isn't researched again within its cooldown.
    - Findings are never written to memory without acceptance.
    - Research runs stay within their caps.
  - **Budget.** At the Background cap, model-free reminders still fire.
  - **Its own security review** before merge.
- **Risks.**
  - Injection through triggers and web content.
  - Usefulness and false alarms.
  - Cost creep.

### M11: fan-out and parallel writers

- **Scope.** The `fanout` flow:
  - decomposition;
  - a pilot item;
  - a rolling map;
  - a barrier;
  - per-item gates;
  - parallel writers with disjoint claims;
  - a merge gate;
  - synthesis;
  - fan-out for wide research topics.
- **Acceptance.**
  - Every item is accounted for.
  - Overlapping claims are refused.
  - Conflicts escalate.
  - Siblings are staggered.
- **Risks.**
  - Merge complexity.
  - The cost of wide runs.

### M12: remote access and device pairing

- **Scope.**
  - The WSS listener, off by default.
  - Pairing and signed challenges.
  - Scopes and revocation.
  - **Content-free phone push, bot channels, and notification actions from the phone.**
- **Acceptance.**
  - Unpaired, revoked and replayed connections are rejected.
  - Phone scopes can't reach admin methods.
  - There is no URL endpoint parameter.
  - **Push payloads carry no content.**
  - **A revoked device gets no notifications.**
  - An external security review is complete.
- **Risks.**
  - Attack surface.
  - Reachability (O4).

### M13: self-tune (optional, outside the kernel)

- **Scope.**
  - `wisp-tune` proposes profile diffs and evaluates them at equal budget.
  - The user adopts winners from the review inbox.
  - Rollback is by hash.
  - **It also tunes triage prompts and thresholds against the usefulness eval. It never tunes tier ceilings or quiet hours.**
- **Acceptance.**
  - It wins on a seeded suite.
  - It never edits the pinned layer.
  - It stays within its budget.
  - Every adoption needs a user decision.
- **Risks.**
  - Overfitting.
  - Eval noise.

## Subscription allowance observations

`wisp-protocol::plan_usage` is separate from inference `Usage` and `Cost`. Its
windows carry percent **used** (finite 0–100), optional duration/reset, scope,
observation time and provenance. Missing values mean unknown, not zero. Snapshots
are sorted by stable window key, reject duplicates and are limited to 64 windows.

Providers may implement `Provider::plan_usage`; its default is `Ok(None)` and it
never runs inference. Passive `ProviderEvent::PlanUsage` observations are consumed
before request acceptance and assembly. They cannot enter conversation history or
change cache prefixes. Malformed supported telemetry becomes a typed safe lookup
status; unrelated metered families are ignored without invalidating valid data.

The Kit shares `PlanUsageCache` with children. Local alias/provider generations
isolate cache entries; invalidation rejects late results. Only explicit reconnects
replace a generation. A child carrying an older provider cannot replace a newer
entry or subscribe to that newer credential generation. The cache coalesces
lookups, merges partial windows and replaces successful full inventories. Newer
passive observations win over older in-flight lookup windows with the same key;
other GET windows still update. The cache tracks the GET's window inventory
separately from each window's latest provenance, removing omitted GET-owned
windows while retaining independent passive-only windows. Lookup failures preserve
last good data but mark it stale. An authentication failure or access/scope refusal
in this process requires a new generation. A failed lookup waits 30 minutes, then an hour
for each further failure, or until a later Retry-After; a throttled status names only the
service's own future Retry-After. Neither affects inference retry policy.

Account switches share `remembered/<account>.json` in Wisp's data folder: one
model and effort per account, replaced atomically (folder 700, file 600).
`StartLayers.remembered` supplies the store; its default reads and writes nothing.

`PlanUsageCache::shared` keeps one record per subscription account (Claude, ChatGPT and
Antigravity alike), `~/.local/share/wisp/usage/<name>.json` (folder 700, file 600, replaced whole),
shared by every Wisp process: the last good reading and its time, the next allowed
automatic lookup, the failure count and the last failure (its `PlanUsageError` kind, with
the service's Retry-After time inside `throttled`, and its time), which is kept after later
successes. It holds no token or credential; read it with `jq . ~/.local/share/wisp/usage/<name>.json`.
Every refresh first adopts a record written after this process's last lookup or adoption
(passive readings don't count): its reading, its next lookup and failure count, and its
last failure when that is transient and there is no reading. A refusal that needs the user
is written for diagnosis with the next lookup allowed at once, but never adopted, so each
process finds it at its own next lookup. A lookup holds
`<name>.lock` (`File::try_lock`); when another process holds it the lookup ends quietly, and
when the record changed meanwhile it is adopted without a request. `Disabled` and accounts
without a usage lookup are never written. `Default` keeps everything in memory.

Hosts explicitly enable `plan_usage::Display` for interactive protocol clients;
it defaults to disabled on an Agent. `ClientCommand::PlanUsage` registers or
releases display interest, or explicitly refreshes it. Headless print/JSONL and
replay cannot register interest. A shown section or page of a plugin that reads usage
holds the cache for the session, letting every account refresh under the same gates
without a listener (see [the usage read](plugins.md), `usage.accounts`).
With no listeners and no hold, there are zero usage GETs, including for
unattended children. Interested endpoints refresh after completed provider steps,
not just turns, at most once per 10 minutes across processes (sooner once a window's
reset has passed, unless a failure's wait runs); opening the usage page and `wisp account`
follow that rule. An explicit refresh skips the spacing and is held only by the service's
own future Retry-After and a 10-second minimum.
There is no idle polling. Results are delivered independently of the
turn loop, including while tools run or later steps stream.

`PlanUsageChanged` and `PlanUsageWarning` are ephemeral. Warnings report only the
highest newly crossed 75/90/95/100 threshold for fresh relevant windows; reset
changes start a new interval, and unknown resets start a new interval on an
observed decrease. No allowance event stops inference or imposes a spend cap.
