---
name: live-testing
description: How to keep a run of wisp from reaching a real provider, how to smoke-run a change the user sees against a local fake server, how to run wisp or the ignored live tests against a real model with this repository's test credentials or on the user's accounts without leaking them, how to keep the cost down, and which tools a session started from another session gets. Read before reporting an MR that changes what the user sees, before any run of wisp, fake keys included, or of a live test, and before starting or resuming a Wisp session from another.
---

# Live testing

This skill spells out how to run against a real model; AGENTS.md's rules, its Secrets rule first, win over it.

## Before any run

- Before any other `wisp` command, run `wisp doctor` with the same `HOME`, flags and environment as that command; it sends nothing (`--check` does). The command stays local only when every account doctor lists shows a local host (`127.0.0.1`, `localhost` or `[::1]`, with any port: a fake server or a closed port), and the command neither signs in nor saves an account whose URL isn't local.
- Any run that fails that check is a live run, fake keys included: start one only when the task asks for a live run, and follow the rest of this skill.

## Credentials

Test credentials live in `.env` at the repo root. The file is git-ignored and readable only by the owner (mode 600).
- `ANTHROPIC_API_KEY` is a direct Anthropic API key.
- For gateway tests, use the gateway API key and base URL named below, with the key as a Bearer token. The gateway buffers responses and reports its own errors as HTTP 200 OpenAI-style chunks.

## Running `wisp`

- `wisp` never loads `.env` from the current directory, since a cloned repo could use one to redirect your key. It reads `~/.config/wisp/.env` automatically, or the file given with `--env-file`; use `--env-file .env` for the repo copy.
- Credentials and endpoint always come from one source: if `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` is set in the real environment, the file's Anthropic variables are ignored. To go through the gateway, start on the named endpoint of the next line rather than exporting the gateway's token and address, which would reach everything the run starts.
- Named endpoints live in `~/.config/wisp/config.toml` (or `--config PATH`, never the current directory); start on one with `--endpoint NAME`, e.g. `wisp --env-file .env --config <scratch>/config.toml --endpoint manifest` with `key_env = "MANIFEST_API_KEY"` and `url_env = "MANIFEST_BASE_URL"` (`docs/endpoints.md`).
- Wisp reads keys from environment variables and env files only where no keychain is available. WSL counts as having none (`keychain = "auto"`); on a machine with a keychain, store the keys once in a scratch config with `wisp --config <scratch>/config.toml endpoint add NAME --env-file .env --key-var VAR --store file`, or pass `--keychain off` (or set `WISP_KEYCHAIN=off`) for live runs.

## Smoke run

An MR that changes what the user sees on screen gets a smoke run before each ready report: the worktree's own `wisp`, driven in a terminal against the fake server in this skill's folder, with the screen saved for the reviewer. It sends nothing off the machine, so it is not a live run.

- Build the program with `heavy cargo build -p wisp-cli`; it is `debug/wisp` in the worktree's target folder.
- Start `python3 .agents/skills/live-testing/scripts/fake-server.py` in the background; it prints its URL, then answers every model request with one short text reply.
- Run each command in the cleared environment the CLI test harness's `command_in` sets (docs/testing.md, CLI test harness, `home`), with a scratch home, plus `TERM=xterm-256color`, `WISP_JOB=1`, a made-up `ANTHROPIC_API_KEY` such as `smoke` and `ANTHROPIC_BASE_URL` set to the server's URL; run `wisp doctor` first, as Before any run says.
- Start `wisp` in a terminal once for each width and glyph set the approved frames show, or once at 80 columns with Unicode glyphs when there are none; for ASCII frames, set `LANG=C` in place of `LANG=C.UTF-8`. Press the keys that reach the change, and read the screen where it shows.
- Save the screen text to `<scratch>/smoke-<MR>.txt` in the scratch folder your brief names, one block per run headed by its width and glyph set, such as `80 columns, ascii`, and name the file in your report. End with `/quit`, since closing the terminal leaves the chat running, then stop the server by its process number and delete the scratch home.
- When the change can't be reached this way (it needs a real provider, a failure the server can't make, or another platform), say so in your report and name the test that covers it.

## Scripts and live tests

- In bash scripts, never `set -a; . ./.env`: it exports every key in the file to everything the script starts. Set only the variables a command needs, on that command, read in a subshell: `ANTHROPIC_API_KEY="$(. ./.env; printf '%s' "$ANTHROPIC_API_KEY")" heavy cargo nextest run …`. Never print, commit, log or send the values anywhere.
- The `#[ignore]` live tests read their keys from the environment, each by the names in its test file (`ANTHROPIC_API_KEY`, or `MANIFEST_API_KEY` and `MANIFEST_BASE_URL` for the gateway); set only those, as above.
- Live tests cost real money: keep them short and use `--effort low` unless testing effort itself.

A `wisp` run as a job ends with that job. Prefix every run with `WISP_JOB=1`,
including `doctor`, for a chat started before jobs carried the mark.
A job-marked run opens only the chat it started: a second window on a chat that
is still running is refused and shows only `wisp: the chat connection closed`.
Never start a daemon in a live test: its data folder is the real one.

## Running on the user's accounts

- Start a run on the user's accounts only on the main session's word for the task, and only while the test credentials cannot do the job or the behaviour depends on a signed-in account.
- Use a Claude subscription account chosen as `flows/projects/preferences.md`'s Accounts line says, or `claude-api`, on the cheapest model that shows the behaviour (never `claude-fable-5-2`), with `--effort low` unless testing effort itself, and keep the run to a handful of requests.
- Keep `HOME` and the config home (`XDG_CONFIG_HOME` left as the user's) so Wisp loads `~/.config/wisp/{config.toml,credentials.json,oauth/}` (and any keychain) in-process; moving them or copying `config.toml` breaks sign-in.
- Create `<scratch>/{data,cache,tmp,cwd}` at a short absolute `<scratch>` path before the first command (Wisp ignores a relative `XDG_DATA_HOME` or `XDG_CACHE_HOME` and falls back to `~/.local/share` and `~/.cache`, does not create `TMPDIR`, and needs the host socket path to fit). Prefix every command with `WISP_JOB=1 XDG_DATA_HOME=<scratch>/data XDG_CACHE_HOME=<scratch>/cache TMPDIR=<scratch>/tmp`, run in `<scratch>/cwd`, and pass `--account <account> --model <model> --effort low --instructions none --set plugins.mcp.enabled=false --set keep_warm=false --set claude_usage=false` (drop `plugins.mcp.enabled=false` only when testing MCP), plus `--tools` with only the tools the check needs, or `--tools none` when it needs none (use a terminal when tools are needed, since `--output jsonl` refuses tools; `--account` also keeps the default untouched).
- Run `wisp … doctor` first with the exact same environment and flags; it sends nothing, confirms `attachments` and `images` point inside `<scratch>/data`, and lists `<account>`'s non-local host so the run that follows is a live run under Before any run.
- Resume a scratch chat only by starting a new run with the same absolute `<scratch>` environment and flags plus `--resume <id>` (from `<scratch>/data/wisp/sessions/`), never `/resume` or `/restart` inside the chat. End with `/quit`, stop any process you started by its process number, and delete `<scratch>`.
- For `#[ignore]` `wisp-core` tests that use `signed_in()`, run `WISP_LIVE_ACCOUNT=<account> heavy cargo nextest run -p wisp-core --run-ignored only <test>` (touches only that single account and in-memory or temporary stores).
- Every file of the run goes under `<scratch>` and is removed afterwards. A job-marked (`WISP_JOB=1`) chat or `wisp doctor` run with `--account <account>` (or `WISP_ACCOUNT=<account>`) reaches only that account, so the only write outside `<scratch>` is Wisp's own refresh of that account's expired sign-in token in `~/.config/wisp/oauth/` (or the keychain).
- Never read, copy, export or print a key, `~/.config/wisp/.env`, `credentials.json`, `~/.config/wisp/oauth/` or the keychain, and never ask the model for the config folder or those files; never put a key into an environment (so raw probes that expect a key in the environment cannot run this way); under the live check's prefix, run only chats and `wisp doctor`, never `wisp account`, `wisp status`, `wisp setup` or any command that saves settings, accounts, MCP servers or sign-ins (such as `/config` save, `/mcp`, `/logout`, `wisp config set` or sign-in); never run without an absolute scratch `XDG_DATA_HOME`; and never resume, open or write to the user's own chats or read their session logs.
- Output and the scratch session log of a signed-in run may hold an email address, an account ID, a conversation ID or a path under the user's home; none of it goes into the repository, a fixture, a report or a note — record only the numbers and fields asked for.

## Sessions started as processes

A Wisp session that another session starts or resumes as a process (running `wisp`, in a terminal) instead of spawning it, the fallback when child agents can't be used, gets `--tools bash,list_accounts,list_settings,show_diff,view_file,web_search,write_todos`, leaving out `ask_user` and `spawn_agent`, unless it is started and driven only to try `ask_user`: it puts its questions in its report, and only the session the user talks to asks with `ask_user`. A child needs no such list: it has its parent's tools without `ask_user`, narrowed by its spawn and preset.
