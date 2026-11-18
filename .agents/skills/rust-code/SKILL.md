---
name: rust-code
description: Wisp's Rust conventions and checks beyond the hard rules in AGENTS.md, and where the ponytail and rust-best-practices skills give way to them. Read before changing or reviewing Rust code in this repository, and before sizing a plan's MRs, for its Plan sizing section.
---

# Rust code

AGENTS.md's code rules come first: no comments, no `unwrap`, `expect`, `panic!` or `unsafe` outside tests, no `HashMap` or `HashSet` in wire or request types, and CONTRIBUTING.md's checks. This skill adds the rest.

## Toolchain and dependencies

- Rust edition 2024, toolchain pinned in `rust-toolchain.toml`.
- Dependencies are declared in `[workspace.dependencies]` in the root `Cargo.toml` and inherited with `.workspace = true`.

## Checks

- Follow CONTRIBUTING.md's "Before opening a pull request" checks, through `heavy` (the `machine-load` skill).
- For a named test, specify the package, test target and filter: `heavy cargo nextest run -p wisp-core --lib pricing::tests::unsplit_writes_use_session_ttl` for in-crate tests; `heavy cargo nextest run -p wisp-core --test instruction_files this_repositorys_skills_are_well_formed_and_portable` for an integration-test file.
- Each worktree builds into its own target dir; never point two worktrees at one. Cargo rebuilds by file modification times, so a shared dir can reuse another tree's crates and a gate passes on the wrong code; a target dir that the flow's build-output tool moved in is not shared, since the tool checks that every file of the worktree is newer than it.
- Two tests guard the rules no lint covers: `crates/wisp-protocol/tests/no_comments.rs` fails on any comment in a Rust or WIT file, and `crates/wisp-core/tests/instruction_files.rs` fails on a date or machine path in AGENTS.md, its imports or a skill. A change to the files these tests guard names them among its tests. Goldens, blessing and slow tests are covered in `docs/testing.md`.
- A fake key in a test or fixture holds `CANARY`, as most existing ones do: the push scan stops on a key-shaped string without it unless the main branch already holds it (`flows/projects/FLOW.md`, Privacy).

## Third-party skills

- ponytail and rust-best-practices (AGENTS.md, "Skills") apply to every Rust change and review, and their `references/` may be read when needed.
- The plan's tests are still required.
- A code review judges against them as well as AGENTS.md. A finding that is only their style is a hint and never blocks.
- Where they disagree, Wisp wins:

| They say | Wisp does |
|---|---|
| ponytail: comment the why in one line; mark a shortcut with a `shortcut:` comment | No comments. The why goes into names, types and tests, or `docs/*.md`; a shortcut's limit goes in the plan or the MR description |
| ponytail: trivial changes need no test | The plan's tests are always written |
| ponytail: end the reply with one or two lines on what was skipped | When a brief fixes the reply's last line (a verdict or report line), that line stays last; ponytail's lines go just before it |
| rust-best-practices, "Before you hand back code": `cargo fmt`, `cargo clippy --all-features`, `cargo test`, `cargo doc` | CONTRIBUTING.md's checks, through `heavy`; no `cargo doc`, no doc tests |
| Same section: prefer `#[expect]`, `#[allow]` only in generated code | A lint exception carries `reason = "..."` in its attribute, never a comment: `#[allow(lint, reason = "...")]`, or `#[expect(lint, reason = "...")]` as the code also does (`is_on` in `crates/wisp-protocol/src/lib.rs`) |
| "Errors and panics" and "Final checklist": `expect("why")` or `panic!` for a broken invariant; `.lock().expect("lock poisoned")` | No `unwrap`, `expect` or `panic!` outside tests: return a typed error or make the state unrepresentable; a lock is taken with `.lock().unwrap_or_else(PoisonError::into_inner)`, as the code does (`WarmthTable::table` in `crates/wisp-core/src/warmth.rs`) |
| "Unsafe and FFI": `unsafe` with `// SAFETY:` | `unsafe` is denied workspace-wide |
| "Documentation and comments": `///` on public items, `//!` module docs, `// TODO(issue)` | No doc comments; documentation is `docs/*.md` |
| "Tests": doc examples are tests | No doc tests |
| "Performance": a faster hasher for `HashMap` | No `HashMap` or `HashSet` in wire or request types (AGENTS.md, deterministic serialization) |

The rows cite section names, not numbers, so an upstream renumbering doesn't break them.

## Plan sizing

Unit costs, in formatted lines, for the flow's Size from the code rule. Measured over the workspace's code: functions and tests from `fn` to the closing brace, new files' `use` headers, serde field declarations. Check a plan's total against the merged MR's `git show --numstat`, summed over its files:
- a new function: about 15 lines; the median is 9, so price a group of small functions one by one, not at the mean;
- a test function: about 30 lines;
- a new file: about 10 lines for its header (its `use` lines), plus the blocks it holds and its parent module's `mod` line;
- an optional serde field: 2 lines for its declaration, plus 1 at each site that builds or destructures its type.

The planner states which unit it used. These are a guide, not a cap.
