# Contributing to Wisp

These rules are strict. CI enforces what it can; reviewers enforce the rest.

## Commits and pull requests

- **Every change lands through a pull request.** Nobody pushes to `main`.
- Ordinary GitHub merges require signed commits on `main`. If the branch's required-signatures check is missing or cannot be confirmed, do not merge.
- **Pull requests are squash-merged.** The pull request title becomes the single commit on `main`, so the title must follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

  ```
  type(scope)!: description
  ```

  - `type`: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`
  - `scope` (optional): the area changed, usually a crate: `core`, `protocol`, `cli`, `wispd`
  - `!` marks a breaking change; explain it in a `BREAKING CHANGE:` footer in the pull request description
  - `description`: imperative, lowercase, no trailing period, e.g. `feat(core): add anthropic provider`
- **Branch names** mirror the type: `feat/anthropic-provider`, `fix/sse-reconnect`, `docs/caching-research`.
- Commits inside a branch should also follow Conventional Commits. They disappear in the squash, but they keep review readable.

## Before opening a pull request

Before a pull request is ready to merge, these checks must pass locally on the change:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

No Rust file may hold a comment line. When guarded files change, the source guards (`wisp-core`'s `source_guards` in `crates/wisp-core/tests/source_guards/` and `wisp-protocol`'s `no_comments` in `crates/wisp-protocol/tests/no_comments.rs`, which also checks WIT files) also run locally.

Routine test suites (`cargo nextest run --workspace`) run on the pull request in CI (`.github/workflows/ci.yml`), the one full run of the exact tree that merges, except clippy and the tests on a pull request whose every changed file is on the safe list in its `changes` job; a merge needs its `ci-ok` check and the title check of `.github/workflows/title.yml`, which reads the types above. Targeted local runs (`cargo nextest run -p <package> <filter>`) remain optional, for example for a test named by a plan, approach note or reviewer, or a live or smoke run CI cannot do.

CI's clippy job also runs `cargo clippy --target wasm32-wasip2 -p wasm-fixture -p board -- -D warnings`, the lint of the WASM guests, and `cargo clippy -p wisp-cli --no-default-features -- -D warnings`, the CLI without its `wasm` feature.

Tests run under [cargo-nextest](https://nexte.st) 0.9.149 or newer (install it with `cargo install cargo-nextest --locked`), one process per test, with the settings in `.config/nextest.toml`. There are no doc tests, since code comments aren't allowed.

## Code rules

- The toolchain is pinned in `rust-toolchain.toml`; edition 2024.
- No `unwrap`, `expect`, `panic!` or `unsafe` outside tests (enforced by workspace lints).
- Add dependencies to `[workspace.dependencies]` in the root `Cargo.toml` and inherit them with `.workspace = true`.
- Clients consume only `wisp-protocol` commands and events. The CLI embeds `wisp-core` in-process for now, but still talks to it only through the protocol, so moving it behind `wispd` changes only the transport.
