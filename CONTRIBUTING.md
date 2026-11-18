# Contributing to Wisp

These rules are strict. CI enforces what it can; reviewers enforce the rest.

## Commits and merge requests

- **Every change lands through a merge request.** Nobody pushes to `main`. The sole exception is the one-time creation of the previously absent GitHub `main` by the merge agent through the reviewed project cut tool, with the single approved signed root. This exception does not permit updating or recreating `main`.
- Ordinary GitHub merges require signed commits on `main`. If the branch's required-signatures check is missing or cannot be confirmed, do not merge.
- **MRs are squash-merged.** The MR title becomes the single commit on `main`, so the title must follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

  ```
  type(scope)!: description
  ```

  - `type`: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`
  - `scope` (optional): the area changed, usually a crate: `core`, `protocol`, `cli`, `wispd`
  - `!` marks a breaking change; explain it in a `BREAKING CHANGE:` footer in the MR description
  - `description`: imperative, lowercase, no trailing period, e.g. `feat(core): add anthropic provider`
- **Branch names** mirror the type: `feat/anthropic-provider`, `fix/sse-reconnect`, `docs/caching-research`.
- Commits inside a branch should also follow Conventional Commits. They disappear in the squash, but they keep review readable.

## Before opening an MR

Before an MR is ready to merge, these checks must pass locally on the change:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run -p <package>
```

The tests of the code a change touched are the package tests of every workspace package containing a changed file, plus every test named by the plan, approach note or a reviewer. Files outside every package add no package tests. No Rust file may hold a comment line; CI's `no_comments` test (`crates/wisp-protocol/tests/no_comments.rs`) also checks WIT files.

CI's pipeline is the one full run of the exact tree that merges, including `cargo nextest run --workspace`, except clippy and the tests on an MR whose every changed file is on the safe list in `.gitlab-ci.yml` (`.safe-list-skip`).

CI's clippy job also runs `cargo clippy --target wasm32-wasip2 -p wasm-fixture -p board -- -D warnings`, the lint of the WASM guests, and `cargo clippy -p wisp-cli --no-default-features -- -D warnings`, the CLI without its `wasm` feature.

Tests run under [cargo-nextest](https://nexte.st) 0.9.149 or newer (install it with `cargo install cargo-nextest --locked`), one process per test, with the settings in `.config/nextest.toml`. There are no doc tests, since code comments aren't allowed.

## Code rules

- The toolchain is pinned in `rust-toolchain.toml`; edition 2024.
- No `unwrap`, `expect`, `panic!` or `unsafe` outside tests (enforced by workspace lints).
- Add dependencies to `[workspace.dependencies]` in the root `Cargo.toml` and inherit them with `.workspace = true`.
- Clients consume only `wisp-protocol` commands and events. The CLI embeds `wisp-core` in-process for now, but still talks to it only through the protocol, so moving it behind `wispd` changes only the transport.
