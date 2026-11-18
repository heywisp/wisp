# Testing

Every test except the ignored live tests (`live_*`, below) runs offline: a fake server on `127.0.0.1` answers any provider call, and those tests make no other network call. A test that calls `cargo metadata` or touches the registry must also pass offline in a cold cache, since neither of CI's two test jobs (each runs one hash-partitioned half) restores a dependency cache.

## Golden requests

`crates/wisp-cli/tests/it/golden_requests.rs` runs the real `wisp` binary against a fake local server and compares every request it sends, headers and body, with a file in `crates/wisp-cli/tests/golden/requests/`. The files prove that a change to the kernel, the settings or the CLI leaves the request bytes alone, which the cache rules in `AGENTS.md` depend on. Run it with `WISP_BLESS=1 heavy cargo nextest run -p wisp-cli -E 'binary(it) & test(/^golden_requests::/)'` to rewrite the files after a change that is meant to alter requests, and review the diff before committing it.

Each run uses a scratch `HOME`, a scratch `--config` where a case needs one, `TZ=IST-5:30` (a POSIX rule, so no time zone database is needed: UTC+05:30 all year) and an empty environment apart from `PATH`, `LANG`, `TMPDIR` and the case's own variables. The scratch directory must not be inside a repository, since the project's `AGENTS.md` would then join the system prompt; the test fails if any ancestor is a repository by the same rule the project root uses (`docs/instructions.md`, "The project root"), so an empty `.git` directory a sandbox briefly creates in `/tmp` doesn't trip it. The full-screen case needs `setsid`: without it the case is skipped with a note, except when `CI` is set, where it fails.

### Cases

| File | What it captures |
|---|---|
| `jsonl_default.http` | `--output jsonl`, no flags, no `config.toml`, the endpoint from `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL` |
| `jsonl_two_turns.http` | two turns in jsonl (`hello`, then `again`), both requests, pinning how history is sent |
| `debug_cache_two_turns.http` | the same two turns with `--debug-cache`: `previous_message_id` is `null`, then the first reply's id |
| `tui_default_tools.http` | the full screen in a pty with its default tools, no flags |
| `tui_agents.http` | the full screen with its default tools spawning one child: main's first request (the agent and checklist tools, no `report`), then the child's first (with `report`, without `ask_user`), with its `<environment>` agent lines and its brief |
| `tui_instructions.http` | the full screen started in `project/sub` of a repository whose `AGENTS.md` imports `notes.md` with `@`: the `<instructions>` block in the system prompt and the `Project root:` line |
| `model_sonnet`, `effort_high`, `effort_low`, `cache_ttl_5m`, `time_utc`, `time_off` | each flag whose default moves into the settings registry, given a value other than its default |
| `debug_cache`, `no_fallback` | the two one-way switches, given |
| `config_today.http` | a `config.toml` with only the keys from before the settings work: `default` and an endpoint with `provider`, `auth`, `key_env`, `url_env` and `pricing` |
| `config_stored_key.http` | `store_id` and a `key_store = "file"` endpoint whose key and URL come from `credentials.json` beside the `--config` file |
| `config_literal_url.http` | an endpoint with a literal `url`, `auth = "bearer"` and a `key_env` |

Each moved default (`--model`, `--effort`, `--cache-ttl`, `--time`) given explicitly at its default value must produce the same bytes as `jsonl_default.http`; that case has no file of its own.

`apps/learn/tests/it/flow_prefix.rs` pins the learning flow's cached prefix the same way: it starts `learn`, which reads `flows/interactive_education`'s `settings.toml`, `BRAIN.md` and `VOICE.md` in place, taps once, and compares the brain's first request's `system` and `tools` with `apps/learn/tests/golden/flow_prefix.json`, rewritten with `WISP_BLESS=1`.

### What the normaliser masks

Only values that differ by run or by machine are masked, and nothing else, so any other change fails:

- **The `Sent:` stamp's weekday, date and time**, which change every run. The masked time includes its seconds, and the mask doesn't depend on the time's width, so an older `HH:MM` stamp masks the same way. The offset is kept (`Sent: <masked> UTC+05:30`), so a change of the `--time` default from `local` to `utc` still fails.
- **The value after `Working directory: `**, which is the scratch directory of the run.
- **The value after `Platform: `**, which is `Linux (WSL2)` on one machine and `Linux` on another.
- **The value after `Resources: `**, the CPUs and memory of the machine or its cgroup.
- **The value after `Shell for the bash tool: `**, which carries the first line of `bash --version`.

Each of these four values ends at the line's `\n` or the JSON string's closing quote, whichever comes first; a value that runs to the end of the body fails the test rather than masking the rest.
- **The key in the auth header** (`x-api-key`, or the token after `Bearer ` in `authorization`), so no key is ever written to a file; the scheme is kept.
- **Dropped headers:** `host` (the fake server's port), `content-length`, `connection` and `accept-encoding`, which the HTTP client sets and which carry no Wisp choice. Every other header is kept and compared.

The environment lines themselves are kept, so a line added, dropped or reordered still fails. The session id is in no request, so it is not masked. The test masks rather than pins because it adds no seam to Wisp's code; a later change that adds one (a fixed clock, say) may drop the matching mask in the same change. A unit test, `the_normaliser_masks_only_what_varies`, checks that each rule masks only its own value.

## Request goldens

`crates/wisp-core/src/agent/tests/golden_session.rs` and its modules run scripted kernel sessions and keep the body of every request they send, byte for byte, in `crates/wisp-core/tests/fixtures/requests/session/<case>/`. Where the CLI's golden requests pin what the binary sends for each flag and config, these pin what the kernel sends across a whole session: the opening lead (environment block, `Sent:` stamp, `Context:` line, `<processes>` notice), tool results, steers, effort markers, compaction, rewind, resume and Retry. They were recorded on the code before the plugin work (`docs/plans/plugins.md`, MR 1), so each later change is checked against bytes the old code produced.

Each case drives a real `AnthropicProvider` (on the official origin) against a fake server on `127.0.0.1`, which answers from a script of streamed replies and records what it is sent. A reply can carry client commands that the server sends to the agent before it answers, which is how a steer, a queued message or a typed-in process lands mid-turn at a fixed point.

- **Files:** `NN.json` is the body of the session's `NN`th Messages request, in the order sent. `count-NN.json` is the body of a `count_tokens` request; these are sorted by content, since the counts of one `/context` report run at once. The server answers a count with a figure computed from the body, so the answers are fixed too.
- **What the harness pins:** the clock (a fixed clock on `2026-10-25 09:41:27 UTC` with a `UTC+05:30` local zone, so stamps carry non-zero seconds; the rewind, resume and Retry cases use a step clock that the harness moves forward one minute before each message or Retry it sends, and that never moves on its own, so a stamp that is made again rather than replayed shows as different bytes, while a change in how often Wisp reads the clock changes nothing), the platform (`Linux`) and no `Resources:` line (`with_resources(None)`, as every agent test pins it, so none sends the host's limits), the bash description (`GNU bash 5.2`, from a test-only `KernelHost::with_capture` that answers `bash --version` at once, with the start window through `Shell::with_timing`), and the working directory (a scratch folder whose parent is the session's home, so it shows as `~/work`, or `~/work/sub` for the cases started in a subdirectory). The instruction cases load `AGENTS.md` through `instructions::load` and `with_instructions`, with the config directory in the scratch folder, so no file of the machine's is read. The pointer cases log to a store with a location (`~/sessions/s1.jsonl`), which the in-memory store lacks. The background cases set `context.background_tokens` through start layers, and a scripted reply can be gated, so the server holds its answer until the test releases it. Nothing is masked.
- **Cases:** the `Sent:` modes (`local`, `utc`, `off`) with the `Context:` line on and off; the default TUI tools through one turn (the environment block, a bash call, a `show_diff` call, a `view_file` call, an answered `ask_user` call, a refused call and a mid-turn steer), then a second turn; a turn of several queued messages and a turn with an image; an effort change between turns; a `/context` report with the token counter, in a chat session and after the default tools turn (the Tools layer's count); a `/context` report after a resume that follows a manual compaction, where there is no measured fill, so the whole conversation is counted; a background job ending into a wake turn; text typed into a process, told by a `process` result and, after a resume, by the first turn's notice; an automatic compaction with Wisp's engine and with the native one; an estimate exactly at the compaction point (it compacts) and one token under it (it doesn't); a rewind, a resume and a Retry; and a native web search pair. Added before the plugin moves (`docs/plans/plugin-moves.md`, MR 1), so each move is checked against them:
  - **Instructions:** `instructions_project` (a project `AGENTS.md` with one `@` import, Wisp started in a subdirectory of the repository, so `Project root:` shows; one tools turn), `instructions_default` (the user's default file only, in the same subdirectory: no `Project root:`), `instructions_empty` (an empty project file beside a default file: the empty file opts out of the default, so no block and no `Project root:`), `instructions_root` (`instructions_project`'s files with Wisp started at the repository root: the block, but no `Project root:`) and `instructions_resume` (`instructions_project` resumed, then compacted with Wisp's engine, so the head's `<environment>` still holds `Project root:`).
  - **Processes:** `process_rewound` (a rewind past a background job's start, then the next turn's rewound news) and `retry_notice` (a wake turn opened with `<processes>` whose first request fails, then Retry, which sends the kept notice again, then a user turn with no notice, since the Retry heard it).
  - **Tools paths:** `retry_first_turn_tools` (the session's first request fails, then Retry and one more turn, so `<environment>` is sent once) `rewind_to_start_tools` (a rewind to turn 1, so the next message carries `<environment>` again) and `rewind_after_resume_tools` (the same rewind after a resume, from the rewind point rebuilt from the log).
  - **Heads:** `compact_head_tools` (Wisp's engine with tools on: the head holds `<environment>` with `Project root:`, a ledger with an attachment, a verbatim message cut with its record pointer where a multi-byte character straddles the byte limit, so the cut is by bytes, and a running line; the next turn sends no `<environment>` of its own), `compact_overflow_cut_off` (with tools on and a job running: a reply cut off by the window, then a compaction refused as too long, so the fallback head holds `<cut_off_reply>` between the ledger and the running line and pointers), `compact_swap_between_turns` and `compact_swap_in_turn` (the native engine with tools on and a kept turn: a background summary swapped in before a turn, and at a step that reaches the compaction point while the summary is still running, each followed by one more turn) and `compact_pointers` and `compact_pointers_chat` (the head's log and output pointers, with tools on and in a chat, where the log is named in Wisp's sessions folder).
- **History:** the rewind, resume and Retry cases (and every case added before the plugin moves that resumes, rewinds or retries) also check that the history rebuilt from the session log serialises to the same JSON as the live one, and the resume case that a resumed agent starts with the same history the live one ended with.
- **Unix only:** the cases that run tools are `#[cfg(unix)]`. The typed-input case needs `setsid --ctty` and is skipped with a note without it, like the other terminal tests.

`CASES` lists every case name, and `the_fixture_folders_are_exactly_the_cases` checks that the folders under the fixtures directory are exactly those names, so a renamed or removed case can't leave stale files behind; a case whose name isn't in `CASES` fails at its start.

Run the cases with `heavy cargo nextest run -p wisp-core golden_session`. A failure names the case and file and shows the bytes around the first difference; a different number of requests fails before any body is compared.

**Blessing.** `WISP_BLESS=1 heavy cargo nextest run -p wisp-core golden_session` rewrites every case's folder from scratch, dropping files the case no longer produces. Only an MR that is meant to change what Wisp sends may re-bless, and it says so in its description; the reviewer reads every changed fixture file, not only the count. An MR that is meant to leave requests alone (a refactor, a move into a plugin) must pass with the files unchanged.

## Keychain tests

No test touches this machine's keychain. Kernel tests use `MemoryKeyring`, an in-memory backend that can work, fail with any error kind, refuse to open or never answer, and fake detection inputs (a fake session bus and a fake `/proc` under a temporary folder). Binary tests set `WISP_KEYCHAIN=off` in every command builder (`command_in` sets it for the builders on it), since a cleared environment could otherwise let the bus check reach a real session bus; the tests of a keychain that isn't running set `WISP_KEYCHAIN=on` with `DBUS_SESSION_BUS_ADDRESS` pointing at a socket that doesn't exist.

One test, `a_real_secret_service_round_trips_an_item`, talks to a real Secret Service. It is ignored, and does nothing unless `WISP_CONTAINER_KEYCHAIN` is set, so it never runs against a desktop keychain by accident. Run it in a throwaway container with its own session bus and keyring:

```sh
heavy docker run --rm --cpus=12 --memory=16g --memory-swap=16g -e CARGO_BUILD_JOBS=12 -v "$PWD":/src:ro -w /src rust:1.99.1 bash -c '
  apt-get update -qq && apt-get install -y -qq cmake gnome-keyring dbus >/dev/null
  CARGO_TARGET_DIR=/tmp/target dbus-run-session -- sh -c "
    printf throwaway | gnome-keyring-daemon --unlock --components=secrets >/dev/null
    WISP_CONTAINER_KEYCHAIN=1 cargo test -p wisp-core --features os-keyring --lib a_real_secret_service -- --ignored"'
```

## CLI test harness

`wisp-cli`'s integration tests, apart from `memory.rs` ("Memory"), are one test binary, `it`. `crates/wisp-cli/tests/it/main.rs` declares the shared modules once, `support` (`tests/it/support/`), `runner` (`tests/it/endpoint_commands/runner.rs`, which `endpoint_commands.rs` and `endpoint_edit_import.rs` share), `scratch`, `live` and `pick`, then one `mod` line per test file; a new test file goes in `tests/it/` with its `mod` line. A file keeps its own launchers and command builders, with its exact arguments, environment and terminal size, as a few lines over the harness, and adds its own methods to `Home`, `Pty` or `Server` in its own `impl` block under names no other file uses, since every `impl Home` in the crate adds to the one type. One file's tests run with `heavy cargo nextest run -p wisp-cli -E 'binary(it) & test(/^tools::/)'`.

- **`home`**: `Home` is a scratch home directory, a `ScratchDir` with the prefix `wisp-cli-` (below), removed on drop. Build one with `Home::new` or `Home::configured`, which also makes `.config/wisp`. `write` makes the parent folders and leaves the file at mode 0o600, so Wisp's "readable by other users" notes stay out; `env_file` writes an empty `empty.env` with the umask's mode. `command_in` gives a command run in a home with a cleared environment: `WISP_KEYCHAIN=off`, `PATH`, `HOME`, `TMPDIR` (the home), `LANG=C.UTF-8` and `HTTPS_PROXY=http://[::1]:9`, a closed port, so a model-list fetch from an account on an official URL fails at once and never leaves the machine (no test sets `NO_PROXY`); a later `.env` or `.env_remove` call changes one of them. `fed` starts a command with piped stdio and writes its stdin, and `run` then waits for its output, draining both pipes. `CONFIG_DIR`, `CONFIG`, `CREDENTIALS` and `KEYS_FILE` are Wisp's config folder and its config, credentials and keys files under a home; `assert_clean` fails when an output shows `CANARY` or a given canary, and `mode` reads a file's permission bits.
- **`server`**: `Server::start` serves `127.0.0.1` on a free port and answers each request with what its closure returns for the request it read. It records every request (`Seen`: head and body) before it writes the answer, so a test may read the record as soon as Wisp exits. `sse`, `json` and `counted` frame whole responses, and `text_reply` (on `message_start`) is the fake Anthropic text answer for a given id, model, text and usage. `src/tui/agreement.rs`'s gateway reads its requests with `read_request`, included by `#[path]` from `src/main.rs`. A request that names a chat (`is_naming`: its body holds `naming::INSTRUCTION`) never reaches the closure: it gets an immediate `400 invalid_request` and is recorded apart (`named()`), so naming changes no test's requests or screens; `Server::naming` answers it with a given reply instead, and `Server::replying` answers every other request with one fixed reply. `tests/it/openai.rs`'s own server refuses it the same way and accepts again without spending a reply.
- **`pty`**: `Pty::start` runs a command on a new terminal of the given size, with stdout on the terminal or piped, and reads the terminal into `screen` and one screen parser as bytes arrive. Waits, `shown`, `screen()` and `saw` see only complete DEC 2026 synchronized frames, with output outside frames visible at once; failures of `Pty`'s waits print every raw byte, including an unfinished frame. `Pty::command` is `command_in` running Wisp under `setsid --ctty --wait` with `TERM=xterm-256color`. `READY` is the TUI's empty-composer placeholder, which a test waits for to know the TUI is ready. `usable` says whether `setsid` runs: a test that needs it returns early with a note when it doesn't, and fails instead when `CI` is set. `send` pauses for `pause` after each write, zero unless the launcher sets it. `wait_idle` waits for two idle polls in a row. `run` runs one turn on a 35×110 terminal: it types the text once `READY` shows, waits for the expected text and then for the turn's end, and returns the screen read after the end, failing when Wisp exits unsuccessfully or the screen shows the given key. `wait_per_step` waits like `wait_until` with `PATIENCE`, but each model request the fake server records restarts the deadline, so a test that runs a tree of agents waits per step rather than for the whole tree. `finish` waits for the exit, failing with the screen when Wisp hasn't exited in 30 s, then returns the piped stdout, or nothing when stdout is the terminal. Dropping a `Pty` kills a child still running.
- **`screen`**: shared with the TUI PTY tests from `crates/wisp-tui/tests/support/screen.rs`. `process` feeds bytes into the parser, `contents` reads the completed visible text and `complete` selects the completed raw prefix. `shown` renders bytes as a terminal would, and `idle` says whether the last status line shows the turn ended.
- **`prefix`**: `assert_prefix_stable(turns, rebuilt)` checks that each request sends the same `tools` and `system` as the one before and that its `messages` start with the earlier ones, whole; a `Rebuilt` names the one request where a test expects a rebuild, where the tools array must change, the system prompt changes as it says, and thinking blocks are left out of the comparison. `tools.rs`, `golden_requests.rs` and `wasm.rs` use it.
- **`pty`** also has `switch_tool`, which turns one tool on or off through `/config tools`, pressing once per step of the checklist row's `k of n` and waiting for each step to be drawn, so it needs a list longer than the row.

## WASM guests

`plugins/wasm-fixture` is a WASM guest, not a bundled plugin: a `wisp:plugin` 0.3.0 component that the host's tests (`crates/wisp-wasm-host/tests/`) and `wisp-cli`'s `tests/it/wasm.rs` load. `plugins/board`, the board plugin (`docs/plugins.md`, "The board"), is the 0.2.0 guest, loaded beside the fixture to guard older-world compatibility: its logic is tested natively (`cargo nextest run -p board`), and its component by the host's `tests/board.rs` and `wisp-cli`'s `tests/it/wasm.rs`. `crates/wisp-wasm-guests` builds both: its build script runs a nested `cargo build --offline --locked --profile guest --target wasm32-wasip2 -p wasm-fixture -p board` into its own `OUT_DIR`, and its library names each built `.wasm` and guest manifest (`WASM_FIXTURE`, `WASM_FIXTURE_MANIFEST`, `BOARD`, `BOARD_MANIFEST`). The target comes with the toolchain (`rust-toolchain.toml`); a build without it fails with the `rustup target add wasm32-wasip2` line. The nested build clears the outer build's flags and target settings and inherits its jobserver, and it runs inside the outer build, so it goes through `heavy` with it. The measurement gate is an ignored test, run optimised: `heavy cargo nextest run --cargo-profile guest -p wisp-wasm-host --run-ignored only gate --no-capture`. Each test crate keeps one compile cache per target dir, so a component compiles once rather than in every test: `wasm.rs` sets `XDG_CACHE_HOME` to `<target>/tmp/wasm-cli` and runs each `wisp plugin add` of a manifest and component while it holds a lock on that folder, so concurrent cold compiles queue, and the host's `Rig` uses `<target>/tmp/wasm-host`. A wisp killed mid-write leaves a `<hash>.wip-atomic-write-mod` there, and that component then compiles cold until a later wasmtime cleanup, run at most once an hour, removes it once it is 30 minutes old.

## Hung tests

`.config/nextest.toml` marks a test slow after 60 s and terminates it after three periods (`slow-timeout = { period = "60s", terminate-after = 3 }`), so a hung test fails at about 3 minutes instead of holding a `heavy` slot or the CI job until an outer timeout. The `ci` profile sets no `slow-timeout` of its own and inherits this one and the overrides below from `default`. The frame-burst picture test takes about 70 s in CI, so the 180 s limit is about two and a half times that.

Three overrides give ignored tests longer, since they only run on request:

- **Soak tests** (`test(/_soak$/)`, every one of them in `wisp-render` today) get 10 minutes. They rerun exhaustive sweeps by design, and `cell_rows_concatenated_equal_the_whole_transcript_at_every_step_soak` has taken 169 s at a load of about 10, too close to 3 minutes.
- **Live tests** (the `live_` tests in `wisp-cli`'s `live_*` modules, `wisp-core`'s `agent::settings::live` tests, `plugin::shell_tests::bash::live_*` and `provider::openai::tests::voice::live_*`) get 15 minutes, in case they are run through nextest rather than `cargo test`: they wait on a real model, whose speed Wisp doesn't control.
- **Memory profiles** (`memory_profile`, `memory_profile_resume` and `memory_rss` in `wisp-cli`'s `memory` binary) get 30 minutes: a heap profile unwinds the whole stack on every allocation, and the RSS run plays four 130-turn sessions on the real binary (see "Memory").

Any other test that legitimately needs longer gets its own override with a reason, not a higher limit for everyone.

Terminating a test kills its process group only. A child that a test started under `setsid` (the bash tool's commands, the PTY tests' children) is in its own session and outlives a terminated test; find it by its PID and kill that PID, never by pattern.

A test's program that should run until the test stops it waits through `hold()` or `wait_for(file)` (`crates/wisp-core/src/test_guard.rs`), which end once the test process is gone; never a `sleep` longer than a minute or a loop without that check.

## Retries and test groups

The suite's two slowest tests are among `wisp-tui`'s picture tests, so a priority override in `.config/nextest.toml` starts that module's tests first.

The `retries = 1` override in `.config/nextest.toml` is the project's list of known flaky tests: each gets one more try in the same run, locally and in CI. A pass on the second try prints a `FLAKY` line and counts as passed; a failure on both fails the run. The list changes only in that file; FLOW step 8 (`flows/projects/FLOW.md`) says how a test joins or leaves it. To measure a listed test's own pass rate, run it with `--retries 0`, which overrides the file.

The `terminal` group runs wisp-core's terminal tests one at a time. The `cli-and-pty` group runs at most 10 of wisp-cli's integration tests and wisp-tui's `pty` and `multiplexers` tests at once; the cap goes up when the group's summed time divided by the cap passes half a run.

## Test agents

Production agents get the bundled `workspace`, `time`, `context`, `shell` and `compaction` plugins from the Kit. Under `cfg(test)` only (never behind `test-support`, which the CLI's own build turns on), `Agent::new` in `wisp-core` starts with those five plugins' section, hooks, sentences and strategy (the `shell` plugin's tools stay with the test's own toolbox), so every kernel test agent sends the `<environment>` block (when it has the facts for one), the `Sent:` and `Context:` lines, the `<processes>` notice and the two system sentences as a production agent does, and logs four `hook_ran` records per turn (`workspace/environment`, `time/sent`, `context/fill`, `shell/processes`), plus a `section-head` record for `workspace/environment` at each compaction head and the `compaction/strategy` records (`strategy-instruction`, `strategy-accept` on Wisp's engine, `strategy-head`) at each compaction, so the compaction goldens go through the plugin as production does; `compact_fallback` leaves it out of the registry to pin the core's minimal head. A test that hands `with_hooks` its own registry adds them with `plugin::testing::turn_line_sources()`; a registry built from `wisp_plugins::bundled()` has them already. `wisp-cli`'s tests that build an `Agent` directly wrap it in `with_turn_lines` from `crates/wisp-cli/tests/it/support/turn_lines.rs`, which starts the bundled registry with no tools on and hands its hooks to `Agent::with_hooks`. A test that compares a whole event list or log either filters those runs or lists them. A test build turns on `wisp-start`'s `test-support` feature for every binary, since Cargo unifies features across the workspace, so a binary built on `wisp-start` calls `wisp_start::paths::use_process_env()` first in its `main`; without it, its tests read no provider key, `WISP_ACCOUNT` or `WISP_ENDPOINT` from the environment.

The bundled `idle` plugin isn't among them: the kernel's bundled-registry test helpers (the golden `registry()` and `tool::testkit::catalog()`) leave it out through `plugin::testing::bundled_but_idle`, so a test agent's hooks stay those five plugins', and only the `idle_wake` golden case takes the real plugin.

## The fake provider

Most of `wisp-core`'s kernel tests talk to one fake provider, `Fake` in `crates/wisp-core/src/agent/testing.rs` (`cfg(test)` only), instead of a fake per suite. The fakes that wrap the real adapter, fake the plan-usage API or compute each reply from the request keep theirs. A test builds it from steps, `Fake::new(steps)` or `Fake::with(knobs, steps)`, usually through its suite's constructor (`scripted`, `documented`, `warm`, `searching`, `planned`); `view(change)` gives a second face over the same steps and records with other knobs, for a connection that states other provider facts.

Each `stream` call records the request as a `Sent` (an owned copy of every `Request` field, the time and a process-wide order), checks that `anthropic::build_body` encodes it and that the body holds no system message with content before a user message, the order Anthropic refuses (`docs/tools.md`, "Switching tools in a running session"), and plays the next step. The golden sessions' server answers that order with the API's 400. A step plays in this order:

1. `Sending`, when built with `Step::hang()` or `.gated(&gate)`;
2. its wait, if it has one: `Wait::Gate` takes a permit and returns it, `Wait::Notified` waits for a notify, `Wait::Sleep` sleeps, `Wait::Take` takes a permit and keeps it (`.gated` and `.held(wait)`);
3. its script's events, up to a trailing `Err`;
4. its actions, in the order they were added: `.then(command)` sends a command to the sender set with `send_to`, `.mail(mail)` posts to the `mail_to` sender, `.run(f)` calls `f`;
5. the trailing `Err`, if any;
6. then pending forever, with `.hanging()` or `Step::hang()`.

With no step left the fake stops the test with the reason, or pends with `hang_when_out`; so does a request whose body doesn't encode, and a step's command or mail with no `send_to` or `mail_to` sender. Stopping prints the reason and aborts the test process, which nextest, running one process per test, reports as that test's failure. `body()`, `text()` (the exact bytes), `beta()` and `count_body()` on a `Sent` rebuild from its parts, so they are the bytes the Anthropic adapter sends for that request.

`Knobs` holds one field per provider fact a test varies: the cache prefix (`Prefix::None`, `Sent` or `Fixed`), `caps` (a fixed row, a function or the shipped row for listed models only), `documented`, `bound`, `keeps_warm`, `cache_life`, `opt_in` (`configure_plan_usage`'s answer, flipped by it), `media`, `search`, `wire_print`, `prefix_bytes` (the real ones, or `Prints::Shared`, the same bytes for every request), `count` (the tokens `count_tokens` answers, recording the request in `counted()`), `kind`, `billing` and `wire`. A knob left at `None` or `false` answers what the `Provider` trait's default does, through `Bare`, a private provider that implements only the two required methods, so no trait default is copied into the kit and a view drops a fact by leaving its knob unset. `echoed` is always `anthropic::echoed_blocks`, and `plan_usage` counts its calls (`plan_usage_calls()`) and answers the trait's `Ok(None)`.

`wire` joins fakes into one wire, a `Wire` and the fake's name on it, for tests that watch requests across agents: each request adds a `Moment::Sent` to the wire's timeline when first polled and a `Moment::Streamed` after its wait, which on such a fake plays before `Sending`, and when the fake has prefix bytes its usage reports `PREFIX_TOKENS` as cache reads if an earlier request with the same prefix bytes had passed its wait when this one was first polled, else as cache writes.

The tests that drive a real adapter, a plan-usage lookup or the plugin `http` host use one local server, `test_guard::http` in `wisp-core` (`cfg(test)` only), apart from the tests of the transport itself (a connection that stalls, never answers or is never made), which hold their own socket. `serve(replies)` answers one connection per reply, in order: `Reply::Send` writes its bytes and closes, `Reply::Hang` holds the connection until the client closes it and `Reply::HeadThenHang` writes a head first; each request is kept in `requests` and sent on `heard`, which `next()` reads. `gateway(answer)` answers every connection as it comes, from a function of the request, for a server that answers by path or holds one reply while others pass. `response`, `json`, `sse` and `sse_with` (an SSE reply with extra headers) build replies, and `header_lines` and `body` read a request.

## The testkit in the binary

The testkit plugin (`plugins/testkit`, test tools and scripted hooks) is compiled into `wisp` only through `wisp-plugins`' `testkit` feature, which only `wisp-cli`'s dev-dependencies turn on, so the binary `cargo build` and `cargo install` make never holds it; `crates/wisp-cli/tests/it/testkit_gate.rs` checks that over `cargo metadata`. In a test build `wisp-plugins::bundled()` reads `WISP_TESTKIT`:

- unset: no testkit, so `wisp --help`, `wisp doctor` and `session_started.plugins` are as shipped;
- `inert`: the testkit with its tools, each `default = "off"`, and no hooks and no `[system]`, so a session sends the shipped bytes until a tool is named in `--tools`;
- `block-input`: the same plus one `user-input` hook, `testkit/block-input`, that blocks an input whose text starts with `block` (`the message starts with block`), for the CLI's end-to-end hook tests;
- `sidebar`: the same as `inert` plus one sidebar section, `testkit/busy` (`default = "off"`, marked at `after-turn`, `before-request` and `after-response`), which renders the kernel tests' busy content with its `SIDEBAR-CANARY` strings, for the end-to-end sidebar test (turn it on with `--set plugins.testkit.slots.busy=on`);
- `sections`: the same as `inert` plus one `lead` prompt section, `testkit/notes`, whose snapshot is always `testkit notes` and whose text is `<notes>\ntestkit notes\n</notes>\n` when `previous` differs, for the end-to-end prompt-section test;
- any other value: no testkit.

## Source scans

`crates/wisp-core/tests/source_guards/` is one test binary of source scans. Most share the tokenizer in `tests/support/rust_source.rs`; `tokens.rs` adds the non-test module walk, using the shared `sources` (below):

- the variable guard (`variable_guard.rs`, formerly `crates/wisp-core/tests/variable_guard.rs`), with the test-module check: every `src` file named after a test module (`TEST_MODULES`: `tests` or `testing`), or under a dir of that name, is reached through `#[cfg(test)]`. It also fails a `#[path]` module that leads outside the scanned root (`crates/`, `apps/`), whatever its visibility or other attributes and in any `cfg_attr`, and a declared module it finds no file for;
- the setting-name scan (`setting_names.rs`), over every crate under `crates/` and `plugins/`;
- the variant-list guard (`variant_lists.rs`), over the same crates: each `const ALL` whose items are one enum's variants names every variant exactly once, test code included, and `one_of_every_event` (wisp-core `session/tests.rs`) samples every `LogEvent` variant but `Unknown`. It finds the enum in the list's own file, then in the crate a `use` path names, then in the list's crate. A deliberate subset gets another name;
- the tool-path scan (`tool_paths.rs`, formerly in `src/jobs/tests.rs`): the jobs and call services and the process notice never reach `crate::tool`;
- the start-gate scan (`start_gate.rs`, formerly in `src/plugin/shell_tests/bash.rs`): outside `wisp-start`'s `tools.rs`, no client's non-test code builds the registry or turns bash or `ask_user` on.

A `src` test module can't include `tests/support/` by `#[path]`, so these two scans live here.

The setting-name scan checks dotted literals starting with a registry group; names passed to `find`, `get`, `on`, `saved_global`, `touched`, `.global` or `SettingChange::named`; `flag_value` on any receiver; `Flag::new`'s name argument; and comparisons or match arms on `spec.name`, including guarded arms. It checks direct list elements when another direct element is a `names::` constant, or the list initializes a `const`, `static` or `let` identifier containing `setting` (case-insensitive), including `vec!` lists. All-literal, unanchored lists stay unchecked. Each checked name must be a registry setting. Group-first non-setting text uses `format!` with its group const, such as `format!("{PLUGINS_GROUP}.{id}")`. Allowed non-settings are derived from the presets group and host import names, never kept by hand. Under `crates/`, checked names use their `wisp_protocol::settings::names` consts; the scan names the const to use and checks each `names.rs` const against the registry. Crates under `plugins/` may not depend on wisp-protocol, so their literals keep the registry-name check; a drift assertion ties the time plugin's `SETTING` to `names::TIME`.

## Shared test support

The purity guards (`tests/purity.rs` in `wisp-diff`, `wisp-index`, `wisp-instructions`, `wisp-media`, `wisp-render` and `wisp-tool-text`) and the dependency guards (`tests/dependencies.rs` in those six, `wisp-plugin-api` and `wisp-tui`) share their helpers through three files under `tests/support/`, included by `#[path]`: `purity.rs` (the source walk, module-only test skipper and impurity scanner), `dependencies.rs` (the one `cargo metadata` call and dependency checks), and `rust_source.rs` (the tokenizer and test-item ranges, included by `purity.rs` and shared by its skipper, `plugin_crates.rs`, `source_guards` and wisp-protocol's `no_comments.rs`). The test-module names (`TEST_MODULES`) live in `purity.rs` too. The source walk takes a file extension, so wisp-tui's `tests/scans.rs`, wisp-render's `tests/tool_neutral.rs` and `tests/gold_reserved.rs`, wisp-cli's `tests/it/variable_guard.rs` and wisp-core's `tests/source_guards/` walk with it too, and wisp-plugin-api's `tests/plugin_crates.rs` and wisp-cli's `tests/it/testkit_gate.rs` read `cargo metadata` through the shared call. Each crate keeps its own lists (its I/O modules, impure words, allowed lines, allowed dependencies and pinned features), its planted samples and every test under its own name, so `cargo nextest run -p <crate>` still runs its whole guard.

Scratch directories come from one file, `crates/wisp-tui/tests/support/scratch.rs`: `ScratchDir::new(prefix, name)` makes `{prefix}{pid}-{name}-{random}` in the system temp folder and removes it on drop, and `alive` tells whether a process still runs. Wisp-core's `test_guard`, wisp-tui's library tests, wisp-tui's `app`, `pty`, `budgets` and `multiplexers` binaries, wisp-cli's `tests/it/main.rs`, wisp-cli's `src` unit tests (from `src/main.rs`) and wisp-start's library tests include it with `#[path]`, so it must name no `wisp_*` crate and use only std, rustix and tempfile. A prefix is always `wisp-<word>-`, never bare `wisp-`, since Wisp's own sweep reads `wisp-` plus a session id; wisp-tui's is `wisp-tui-`, wisp-cli's `wisp-cli-`, wisp-start's `wisp-start-` and wisp-cli's live tests' `wisp-live-`. Each `new` first sweeps the directories under its prefix whose process has died, so a killed or failed run's scratch is removed by the next run. `pick!(items, Pattern => value)`, which collects the value of each item that matches the pattern into a `Vec`, comes the same way from `crates/wisp-tui/tests/support/pick.rs`, included by wisp-core's `test_guard` and `session_locks` binary, wisp-tui's library tests and `app` binary, and wisp-cli's `src` unit tests and `it` binary; it uses only std.

Within `wisp-protocol`, `wisp-render` and `wisp-tui`, and from them into `wisp-cli` and `wisp-diff`, test helpers that several test files use are shared rather than copied. `wisp-protocol`'s integration binaries share `crates/wisp-protocol/tests/support/mod.rs`, which each declares with `mod support;`. Its `fixture.rs` keeps the current log version out of the fixtures: a record written at `LOG_SCHEMA_VERSION` holds `"schema_version": "LOG_SCHEMA_VERSION"`, which `golden` writes when blessing and every reader (core's `test_guard::protocol_fixture` too) turns back into the number, so a version bump re-blesses only the records whose format changed; a record a test pins at an older version keeps its number. `wisp-render`'s library tests share the `cfg(test)` module `src/testing.rs`, which names render types as `wisp_render::…` (through a `cfg(test)` `extern crate self as wisp_render;`) and uses only its public API, so the `stream_fuzz`, `sanitising` and `transcript_safety` binaries, wisp-tui's library tests and `app` binary and wisp-cli's `src` unit tests include it by `#[path]` too. Its line text (`line_text`, `text`, `texts` and `trimmed`) comes from `wisp_render::text::spans_text`, and its `grammars()` loads the bundled grammars once per binary. `wisp-tui`'s library tests and its `app` binary share `crates/wisp-tui/tests/support/shared.rs`, declared like `scratch.rs`; the `app` binary's own helpers live in `tests/app.rs` or in one scene file that its siblings import. The `app` and `pty` binaries share their screen searches in `crates/wisp-tui/tests/support/spots.rs`: `rows_of` (a `vt100` screen's trimmed rows), `row_spot` (where a needle starts) and `fold_spot` (the collapsed fold row holding a needle). Event builders and other helpers that both `wisp-render` and `wisp-tui` tests need live in `crates/wisp-render/tests/support/events.rs`: one builder per event kind, each taking an options struct whose `Default` holds the common values (`turn_ended(End)`, `outcome(End)`, `settings_changed(Changed)`, `tool_requested(Requested)`, `tool_finished(Finished)`), plus `cost`, `usage` and the small shared builders. Render's library tests and its three binaries above, wisp-tui's library tests and its `app`, `budgets`, `interrupt`, `pty` and `multiplexers` binaries include it by `#[path]`, so it uses only std, `wisp_protocol` and `serde_json`. A test states only the fields that differ from those defaults. The code pump every test that highlights uses lives in `crates/wisp-render/tests/support/code.rs`: `Code` keeps a worker per stream over one `Arc<Grammars>`, and its `feed`, `keep_live` and `pump` restart, retire and drop workers as the TUI's worker thread and `feed_code` do. Render's and wisp-tui's library tests, wisp-tui's `budgets` binary and wisp-cli's `memory` binary include it by `#[path]`, so it uses only std and `wisp_render`. The seeded xorshift generator `Rng` (`next`, `below`, `chance` and `pick`) lives beside them in `crates/wisp-render/tests/support/rng.rs` and uses only std: `src/testing.rs` includes it, so every includer of `testing.rs` has it, and wisp-diff's `tests/properties.rs` includes it by `#[path]` directly.

Most of wisp-tui's `app` tests run on paused Tokio time (`#[tokio::test(start_paused = true)]`): the clock moves only when the app and the test both wait, so `settle` and `drain` return after every frame, search slice and debounce due in their window, however loaded the machine. A test whose state comes from a real thread (the highlight worker, the image reader) or from the wall clock stays on real time. Paused and real-time tests alike poll with `eventually` in app.rs, and a paused test holds a nothing-happens window with `real_wait`, both stepping with `tick`: 20 ms of real sleep, then 20 ms of Tokio sleep, so the two clocks move together; a real-time test holds one with a single `sleep`. The `pty` binary polls with `Pty::pump_until`, and wisp-cli's `it` binary shares `wait_until`.

## Snapshot themes

The `insta` snapshots of `wisp-render` and `wisp-tui` cover the dark theme only. The light theme and its token table stay in the code and stay selectable (`display.theme`, `WISP_THEME`), but are unmaintained until a later light-theme task brings its snapshots back; adding `Mode::Light` to `MODES` in `crates/wisp-tui/tests/app/scenes.rs` restores most of the scene matrix. Tests that check light behaviour without a snapshot still run: the token contrast floors and the table resolving at every depth (`crates/wisp-render/src/theme/tests.rs`), the brand-doc sync (`crates/wisp-render/tests/doc_sync.rs`), theme detection and the theme setting.

A reduced-motion look whose shots equal its full look's is not stored: `Twins` in `crates/wisp-tui/tests/app/scenes.rs` snapshots the full look and asserts the reduced one equals it in memory, and fails on a reduced look checked before its full look.

## Test stack

`.cargo/config.toml` sets `RUST_MIN_STACK` to 16 MiB through Cargo's `[env]`, so every test thread and Tokio worker gets 16 MiB instead of libtest's 2 MiB; a binary a test starts gets it for the threads it spawns, while its main thread keeps the ulimit stack. Debug builds keep large async frames, so the deepest `wisp-core` tests use well over 1 MiB and an unrelated change can push them toward 2 MiB; the stack alarm's tests below check that margin themselves. 16 MiB is the pinned toolchain's own compiler-thread stack for rustc and clippy, which use `RUST_MIN_STACK` instead when it is set, so the compiler thread runs as it would without the setting; when `rust-toolchain.toml` moves, the value follows rustc's default. Cargo and nextest find the file from the working directory, so run them from inside the repository. An exported `RUST_MIN_STACK` overrides it for every thread without an explicit size (not the runtime's threads below), to go higher; `a_test_thread_has_room_for_six_mib_of_frames` in `crates/wisp-protocol/tests/test_stack.rs` fails at 6 MiB or less.

The stack alarm's tests, the deepest `wisp-core` tests (a spawn during a hook or a compaction, a root stopped mid-stream, the hook depth cap), don't use that 16 MiB: each runs on a thread of its own with `STACK_ALARM_BYTES` (1.5 MiB, `wisp_core::stack_alarm`, test-only) and a current-thread runtime, through `on_alarm_stack`. An explicit stack size wins over `RUST_MIN_STACK`, so every ordinary test run checks the alarm, and a change that eats the margin aborts its own run with `has overflowed its stack` while 512 KiB still remain below 2 MiB; passing at 1.5 MiB implies passing at 2 MiB. A new test that spawns a child from deep in the turn loop belongs on it too. To measure a test's headroom, bisect `STACK_ALARM_BYTES` (each step is a rebuild) to the smallest size it passes at, to 1 KiB or 4 KiB. When an alarm trips, shrink the largest by-value state on the path: box a cold Agent field or borrow the Agent instead of moving it. Never raise the alarm limit.

The `wisp` binary gives its Tokio workers, blocking threads and the root thread that runs a session the same 16 MiB, from `wisp_core::runtime::THREAD_STACK_BYTES`, so a test that overflows on those threads means a real overflow; the plain threads Wisp spawns itself (TUI input, highlighting, clipboard and signals, jobs readers, the keyring, output) keep std's 2 MiB in the binary, so a green test doesn't vouch for them. An explicit size wins over `RUST_MIN_STACK`, so exporting it doesn't change the runtime's threads; `runtime::tests::the_thread_stack_matches_the_test_stack_in_cargo_config` keeps the constant equal to the config, and `tests::stack::the_root_workers_and_blocking_threads_get_the_thread_stack` in `wisp-cli` checks all three at a 2 MiB default.

## Debug info

Dev and test builds keep line tables for Wisp's crates and none for dependencies, so a backtrace has file and line in Wisp's frames and only names in a dependency's.

## Memory

`crates/wisp-cli/tests/memory.rs` measures where Wisp's memory goes and pins a heap budget, so the memory a session holds can't grow without a test failing. It runs the kernel and the client in one process and makes the calls the CLI makes: a `Kit` as `wisp-start`'s `build` step builds it, over an Anthropic provider on the official origin pointed at a loopback fake server (a made-up key, never `.env`), `kit.build` and `start_root` for a fresh session, and for a resume `resume::open`, `kit.build` with `Start::Resume`, `load_tree` and the shared kernel's `Agent::view`. The client keeps one `Transcript` for the root and one per child, paints one frame at 120×40 after each turn, and runs the code highlighting in-process and single-threaded, so the figures repeat, through the shared `Code` pump, which restarts, retires and drops workers as the TUI does. B's fixed costs hide a record's size, so wisp-render's `a_cell_and_a_tool_call_hold_only_what_every_kind_needs` caps the sizes of a transcript `Cell` and a `ToolCell`. Each scenario has its own scratch directory in the system temp folder, outside any repository, with a made-up 20 KB `AGENTS.md`; the session log and its attachments live there and are removed at the end.

The heap figures come from [dhat](https://docs.rs/dhat), a dev-dependency of `wisp-cli` only, as the test binary's global allocator (no `unsafe`). dhat allows one profiler per process at a time, which nextest's process per test gives; under plain `cargo test` the binary needs `--test-threads 1`.

| Test | Runs | Command |
|---|---|---|
| `memory_budget` (B) | every run, 14–27 s in a full parallel run | `heavy cargo nextest run -p wisp-cli --test memory -E 'test(=memory_budget)' --no-capture` |
| `memory_budget_resume` (B-resume) | every run, 10–24 s in a full parallel run | `heavy cargo nextest run -p wisp-cli --test memory -E 'test(=memory_budget_resume)' --no-capture` |
| `memory_profile` (P) | ignored, about 8 minutes | `heavy cargo nextest run -p wisp-cli --test memory --run-ignored only -E 'test(=memory_profile)' --no-capture` |
| `memory_profile_resume` (P-resume) | ignored, about 4 minutes | `heavy cargo nextest run -p wisp-cli --test memory --run-ignored only -E 'test(=memory_profile_resume)' --no-capture` |
| `memory_rss` (S, Linux) | ignored, a few minutes | `heavy cargo nextest run -p wisp-cli --test memory --run-ignored only -E 'test(=memory_rss)' --no-capture` |

Run the ignored ones one at a time; `.config/nextest.toml` gives them 30 minutes (see "Hung tests").

### The budget tests

B plays 16 turns of two steps each (thinking, text and a testkit `echo` of 48 KiB or a `snap` attachment of about 100 KB, then Markdown with a 40-line Rust fence), with a noise PNG in turns 4 and 12 and one automatic compaction on Wisp's engine after turn 10, background compaction off so no spawned task makes the counts vary. It takes dhat's `HeapStats` after turns 2, 9, 11 and 16 (each after a `RewindPoints` round trip, so the kernel is idle), after dropping the client (CLIENT) and after closing the root and awaiting its task (END), against START, before the `Kit` is built. B-resume writes B's log with no profiler running, then profiles the resume sequence and one more turn.

| Figure | Meaning |
|---|---|
| R | retained heap per content byte: (T9 − T2) / content committed in turns 3–9, and the same for turns 12–16; content is the log's growth plus the base64 size of every media reference in the new records, once per reference |
| K | churn per request byte: growth of all bytes allocated over turns 3–16 / the request body bytes the server saw over them |
| H | peak per request byte: (peak − T16) / the largest request body |
| client, kernel | T16 − CLIENT and CLIENT − END: the client's and the kernel's share of the live heap |
| L | leak: END − START, at most 1 MiB |
| Q | the resume's peak / the content of the whole log |

The tests assert R (the larger of its two), K, H and Q against constants in `memory.rs`, each set from a measured run plus 15%, and L ≤ 1 MiB. On failure they print every figure and the command that re-measures. The ratchet: a constant moves up only in an MR that says why, and an MR that cuts memory moves it down to its new figure plus 15%. H can rise after a good cut in retained memory, since it subtracts a smaller T16; read it beside R. Ratios to content and request bytes keep the gate stable when the scenario's sizes change.

### The profiles

P is B scaled to 160 turns on the default compaction path (native compaction, background compaction on), with images on 8 turns, a `starter` job printing 200 KB on 8 turns, a 300-line `show_diff` every 10 turns, and one turn in which the root spawns 3 children that each run 5 steps and report. P-resume writes P's log with no profiler and profiles its resume, children included. Both write dhat's JSON (full backtraces) into the scratch directory, drop the profiler while the agent and the client are still alive, so its end column is the live set, and print three owner tables: the top 25 owners by bytes live at the end, at the peak and allocated in total, then a sum per crate. An allocation's owner is the first frame, from the allocation outward, whose demangled name starts with `wisp_`. Trait-impl frames (`<A as B>::f`, such as `<wisp_protocol::Message as Clone>::clone`) are skipped, so a clone or a serialisation is credited to the function that asked for it; an inherent method, which the symbol names `<wisp_core::agent::Agent>::serve`, counts as `wisp_core::agent::Agent::serve`; a box made by an inlined `Box::new` is credited to the caller of the function that made it; an async body, whose inlined frame is only `{async_fn#0}` or `{async_block#0}`, is named by the future in the `poll` frame after it, without its `{impl#N}` segment (`wisp_core::agent::run_steps_unboxed`); generic arguments and closure segments (`::{closure#0}`, `::{{closure}}`) are dropped. Allocations with no such frame (the HTTP client's buffers, the harness itself) form the `<no wisp frame>` row, always printed. Labels are function paths only. dhat reads the symbols from the test binary when the profiler drops, so nothing may rebuild that binary while a profile runs (another `cargo nextest run` of `wisp-cli` after an edit, say): the tables then come out as one `<no wisp frame>` row. The JSON holds this machine's source paths, so it is deleted with the scratch directory and is never committed, uploaded or quoted; the printed tables are the record.

### The RSS run

S runs the real `wisp` binary with `--output jsonl` in an isolated home (cleared environment, `HOME` and `TMPDIR` in the scratch directory, `WISP_KEYCHAIN=off`, `WISP_TESTKIT=inert`, a made-up key and the loopback URL in `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL`), so it never touches the owner's config, keychain or sessions. It plays 120 text turns one line at a time with `--compact-at 200000` (P's point, since the binary reads no models file here; every 40th turn's usage crosses it, since the gateway runs no background compaction), then `--resume` for 10 more, and reads `VmRSS`, `RssAnon`, `RssFile`, `VmHWM` and `Threads` from `/proc/<pid>/status` at turns 1, 30, 60, 90 and 120, after each compaction and after the resume, in five variants: default, `GLIBC_TUNABLES=glibc.malloc.arena_max=1`, `GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072`, `TOKIO_WORKER_THREADS=2` and `TOKIO_WORKER_THREADS=4`, set on the child only. The arena share is 1 − RssAnon(arena_max=1) / RssAnon(default) at turn 120 and after the resume; the large-buffer share is the same with the `mmap_threshold` variant. S is headless and limited: jsonl has no tools, so large text replies stand in for tool output and there are no images, jobs or children; a loopback base URL makes the endpoint a gateway, so features run on trial; and JSONL takes only the shared view's input IDs, skipping its history and the TUI's threads. On jemalloc ("The allocator") the two glibc variants change only what C libraries allocate, so the arena share is about 0; a share back above about 0.3 means the binary is on glibc's malloc again. The client's share comes from B and P.

### Why not on the alarm stack

The harness runs the kernel on a current-thread runtime on a thread of `THREAD_STACK_BYTES`, for repeatable counts, not on `on_alarm_stack` (see "Test stack"): that helper is test code private to `wisp-core`, dhat unwinds the stack on every allocation, which the alarm stack isn't sized for, and the harness adds no code path. The compaction and spawn depth stay guarded by the alarm tests.

### The allocator

On Linux with glibc, `wisp` allocates through jemalloc with background purging (`tikv-jemallocator`), since glibc's per-thread arenas keep the memory the parallel resume frees: the largest measured session held 732 MB after a resume on glibc and 154 MB on jemalloc (713 and 108 MB headless), while a typical session reopened with nothing sent holds at most 2 MB more, and test S's session in use about 11 MB more at turn 120 and 15 MB after the resume. The declaration is in `crates/wisp-cli/src/main.rs` and never in a library, since a program has one global allocator and a test binary such as `memory.rs` declares its own (dhat); the binary's unit tests run on jemalloc, its integration tests on their own. `the_binary_allocates_through_jemalloc` (`tests/it/allocator.rs`) runs `wisp --version` with `_RJEM_MALLOC_CONF=stats_print:true` and checks jemalloc's statistics and `opt.background_thread: true` on stderr; the variable carries the `_RJEM_` prefix because jemalloc's symbols are prefixed `_rjem_` on Linux.

The C libraries (`aws-lc-sys`, `zstd-sys`, `opusic-sys`) still use glibc's malloc, and heap profilers that hook `malloc` through `LD_PRELOAD` no longer see the Rust heap, so heap profiles come from dhat (the memory tests) or `_RJEM_MALLOC_CONF`. Three places carry the scope `cfg(all(target_os = "linux", target_env = "gnu"))`, and a scope change edits all three: the target table in `crates/wisp-cli/Cargo.toml`, the declaration in `main.rs` and the test's `#![cfg]`.

## Slow property tests

CI's test step can't finish before its slowest test, so property tests keep a short default run and leave long runs to an ignored test.

- **Cell rows equal the whole transcript** (`wisp-render`, `stream_fuzz`). The default run, `cell_rows_concatenated_equal_the_whole_transcript_at_every_step`, plays 16 random three-turn transcripts. It checks the property after every event that isn't a text or thinking delta. A delta that holds a line break or a backtick is always checked, since those are where a block or a code fence opens and closes; any other delta is checked 4% of the time, from a fixed seed. Each check renders the whole transcript, so checking every delta made the run grow with the square of a turn's length. The soak run, `cell_rows_concatenated_equal_the_whole_transcript_at_every_step_soak`, plays 48 transcripts and checks after every event:

  ```sh
  heavy cargo nextest run -p wisp-render --run-ignored only -E 'test(cell_rows_concatenated_equal_the_whole_transcript_at_every_step_soak)'
  ```

- **The TUI's image estimate equals what the kernel sends** (`wisp-cli`, `tui::agreement`). Every capability row sends one small probe (1600×200) through the kernel. The probe is cut to the edge in the standard tier and passes unchanged in the high one, so a row that resolves to the wrong tier fails. The three large photos, which make the token cap bind, run once per distinct tier rather than once per row.

### Streaming fuzz tests

The slowest streaming fuzz tests in `wisp-render` come in two variants. The Quick variant runs in CI and on every `cargo nextest run`; the soak variant is `#[ignore]`d and reruns the old exhaustive sweep with the same seeds.

| Quick test | What the soak variant adds |
|---|---|
| `random_documents_commit_a_prefix_of_the_full_render` | Fine chunks across the whole document, not only in one 256-byte burst at a random place with chunks four times coarser elsewhere. |
| `rich_inline_text_replays_at_most_the_last_rows_on_resize` | The old chunking and resizes for all 300 documents. The Quick test replays them exactly for four pinned documents (22, 25, 69 and 276), whose rapid resizes mid-paragraph exposed reanchoring and rebasing bugs only the full chunking reaches, and samples the rest like `random_documents`. |
| `a_forced_cut_keeps_the_structure_of_the_whole_document` | Chunks of up to 90 bytes through the whole document, not only in a 1 KiB window centred on the held block's line break past `HOLD_LINES`, where it is cut (the generator's lead line and blank line come first), with chunks of up to 720 bytes elsewhere. |
| `three_way_splits_commit_a_prefix_of_the_full_render` | Every seventh second cut, not only the cut just after the first, the three cuts around the next line break and one random cut. |
| `a_resize_parses_a_held_block_at_the_cap_at_most_twice` | 97-byte chunks from the start, not only over the last 4 KiB. |
| `diff_fences_streamed_in_chunks_keep_the_spans_of_one_pass` | Fine chunks across every diff document, not only in one 256-byte burst. Both compare each fully streamed diff fence's `KeptCode` spans, word emphasis included, with the fence fed to a `CodeWorker` in one delta. |
| `deep_lines_after_any_block_start_reparse_and_stream_identically` | 10,000 inputs per seed with uniform depths, not 2,500 where two in three depths sit on either side of the nesting limit, the absolute and 512-byte prefix limits, or the top of the range. |

The sweeps check a forced cut's structure, not where it falls. Unit tests pin the hold limits themselves, streaming a held block one line at a time:

- **Lines:** `a_held_table_is_cut_on_the_first_line_past_the_hold_limit` and `a_held_list_item_is_cut_on_the_first_line_past_the_hold_limit` check that a held table and a held list item commit nothing through line `HOLD_LINES` (200) and are cut on the next line.
- **Bytes:** `a_held_list_item_is_cut_on_the_first_line_past_the_byte_limit` checks that a list item of long lines commits nothing while it holds exactly `HOLD_BYTES` (64 KiB) and is cut on the next line, well before the line limit.
- **Pending rows:** `a_block_past_the_line_limit_is_cut_only_with_more_than_hold_rows_pending` checks that a block past the line limit with exactly `HOLD_ROWS` (64) rows pending stays held, and one with a row more is cut.

Each soak test is named after its Quick test with `_soak` appended. Run them all, with the cell rows soak run, after changing the stream, ingest, hold or block parser code:

```sh
heavy cargo nextest run -p wisp-render --run-ignored only -E 'test(/_soak$/)'
```

A bug the soak tests catch that the Quick tests miss is a gap in the Quick sampling; fix it by moving the sampled window or cuts to that boundary, or by pinning the soak input that found it, not by making the Quick test exhaustive again.

#### Pinned documents

`RICH_PINNED` in `crates/wisp-render/tests/stream_fuzz.rs` lists each pinned document by its index in the soak run, with a fingerprint: an FNV-1a hash of the document and its chunk and resize plan. A change to the generator, the rng or the plan makes the fingerprint check fail with "re-pin". To re-pin:

1. Plant each mutant the pins guard (see below), run the soak test and note which documents fail.
2. Pin one failing document per mutant with a fingerprint of `0`, run the Quick test once and copy the fingerprint each failing check prints.
3. Confirm the Quick test fails under each mutant.

The pins guard four bugs, each also covered by a small literal regression test that a mutant of its line fails:

| Bug | Regression test |
|---|---|
| Reanchoring tries one row fewer than `REANCHOR_BACK` | `a_resize_reanchors_up_to_three_committed_rows_back` |
| The frontier search only matches whole committed rows, not a row cut mid-line | `a_resize_finds_the_frontier_inside_a_committed_line` |
| A rebase skips rows after rows were emitted again | `a_rebase_skips_rows_only_before_any_are_emitted_again` |
| A failed reanchor keeps the rows it could not place | `a_failed_reanchor_forgets_the_rows_it_could_not_place` |

Subscription usage has offline parser, cache, rendering, and refusal tests.
The type-only ChatGPT `usage_limit_reached` response is distinct from API billing
errors; a synthetic long Retry-After represents 75 hours without exhausting a real
account. Claude usage is on by default; with `claude_usage = false` it must
perform no lookup or credential refresh. Live Claude usage checks require explicit opt-in and are not
implied by passing offline tests.

## Subscription login UI and deletion

The subscription login wizard is a local state machine with a transport adapter;
its reusable forms use the question-card renderer. Synthetic tests cover provider
and name shortcuts, collisions, protected code#state paste, expiry, cancellation,
stale replies and saved-but-not-ready reloads. Private code input never uses the
composer or renders its value. Browser challenge URLs belong only to the private
card, not transcript/history cells. Auth-envelope integration tests accompany the
in-chat command wiring.

Claude uses the existing OAuth client/scope and shared persistence/deletion path.
Offline tests construct the authorization URL and reject a synthetic wrong state
before any token request. CLI tests remove synthetic setup-token endpoints and
exercise default/preset blockers. Config-write failures use non-empty-directory
obstructions, not permission bits, so they also work as root. Run those tests with
`heavy unshare -r` where user namespaces are available; no live OAuth is part of
these checks.

The in-chat adapter is exercised with the real request-correlated protocol, and CLI
integration tests connect the actual TUI to the kernel and CLI subscription host
with synthetic OAuth implementations. These cover all three providers, activation,
endpoint selection, reauthorization, logout bindings, cancellation and saved-only
Reload without enabling Claude allowance fetching. Existing kernel catalog tests
also cover explicit-endpoint and preloaded-preset children after login. No live
OAuth or model request is made by these tests.

URL rows are tested at 40/80/120/160/200 columns, including the chrome band and
short viewports. Long URLs remain complete and scrollable. Private auth events are
excluded both when parsing replay JSONL and when building replay scripts directly;
input, transcript, history, clipboard, Debug and kernel-log canaries complement
the existing output JSONL and protocol persistence tests.

The subscription usage page has a 40/80/120-column × dark × Unicode/ASCII
render and TUI matrix, with additional monochrome and ambiguous-wide render cases.
Coverage includes selection bands, atomic footer/status hints, truthful fractional
percentages, tier privacy, stale/reset-due values, unknown prices/splits, root/child
spend and removed accounts' spend. Controller tests use synthetic protocol events
and explicit clocks: focus and scrolling never request usage, one-shot refresh
interests are released, and closing the page does not export its data to transcript
history. No live account or quota exhaustion is needed for these checks.

## Hosted chat tests

Full-screen CLI tests with a log run through a separate host. `Home` ends every
host and daemon under its scratch root on drop; the dead-run sweep does the same before
removing a killed test's files. The golden request launcher waits for the
composer and dials a real session socket, so an in-window fallback cannot pass
as hosted coverage. Picker and account-question tests launch without that wait
and make the same assertion after their choice. Tests with logging off assert
the in-window note instead. A long socket path is a test-folder problem, never
a reason to bless a request golden again.

`command_in` sets `WISP_WSL_ROOT` to scratch. The WSL helper builds its release,
`/proc/stat`, placeholder Windows user folder and `PATH` there, removing every
inherited `/mnt/` entry. It never reads or changes a real Windows settings file.
A live test must not start a full-screen `wisp` with its log on unless it ends
that chat's host itself; the existing ignored live tests run the kernel inside
the test process instead.

`Home` leaves the daemon stop mark by default. Tests start a daemon explicitly
in scratch only; the same socket sweep ends one left by a killed test. Daemon
client tests run in an environment-cleared subprocess so the real peer job check
is tested without depending on the test runner's own job mark. A live test never
starts a daemon in the real data folder.
