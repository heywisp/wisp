# Vendored syntect

`vendor/syntect` is a guarded fork of [syntect](https://github.com/trishume/syntect), used only by `wisp-render` to tokenise code blocks. The fork exists because upstream syntect can hang or run for minutes on some grammar and input pairs (see `research/code-highlighting.md` §6 and the M3 step 5 review), and a terminal UI can't afford either.

## Upstream base

| | |
|---|---|
| Crate | `syntect` 5.3.0 (crates.io) |
| Commit | `e4670846ecf16d8832db6c43d531bec466214e27` |
| Licence | MIT, kept unchanged in `vendor/syntect/LICENSE.txt` |
| Regex engine | `fancy-regex` 0.20.0 (upstream pins 0.16.2) |

The crate keeps the name `syntect` and version `5.3.0`, so the workspace refers to it as `syntect = { path = "vendor/syntect" }`.

## What is kept

Only what parsing a pre-built dump needs:

- `dumps`: `from_uncompressed_data` (bincode) and the crate-private `from_compressed_data`, which inflates each syntax's lazily loaded contexts from a byte slice. Upstream's reader takes any `BufRead`; the fork takes `&[u8]`, so the crate names no `std::io` item.
- `parsing::scope`: `Scope`, `ScopeStack`, `ScopeStackOp`, `ClearAmount` and the global atom repository.
- `parsing::regex`: the `Regex` and `Region` wrapper over fancy-regex.
- `parsing::syntax_definition`: the deserialisable grammar types (`Context`, `Pattern`, `MatchPattern`, `ContextReference`, `MatchOperation`, `ContextId`).
- `parsing::syntax_set`: `SyntaxSet` and `SyntaxReference`, loading only.
- `parsing::parser`: `ParseState` and the line parser.

## What is removed

- Modules: `html`, `easy`, `escape`, `util`, `utils`, `highlighting` (themes, styles and the highlighter), `parsing::metadata`, `parsing::yaml_load`, the `SyntaxSetBuilder`, linking, and every dump writer.
- Loading from YAML (`.sublime-syntax`) and plist (`.tmTheme`, `.tmPreferences`) files, and anything that touches the filesystem (`walkdir`, `load_from_folder`, `find_syntax_for_file`).
- Lookups by token, extension, path or first line. Wisp resolves languages through its own allow-list (`wisp_render::highlight::LANGS`).
- `Serialize` derives, `MatchPower` and scope selectors, `ScopeStack::debug_print`, `bottom_n` and `apply_with_hook`.
- Cargo features. The fork has none: fancy-regex is the only engine, and Oniguruma (C code) is gone.
- `once_cell`, replaced by `std::sync::OnceLock` and `LazyLock`.

## Changes to the code that is kept

The rules below come from the workspace. The fork is a workspace member with `[lints] workspace = true`, and the no-comments test scans it with no exemption.

1. **No comments.** All upstream comments and doc comments are removed, including the crate-level `#![doc(html_root_url)]`.
2. **No `unwrap`, `expect`, `panic!`, indexing panics or `unsafe`.** Every unwrap, `[i]` index and recursion in the parser, `MatchIter`, regex wrapper, scope code and backref substitution is rewritten to use `get`, `last()`, `?` or a fallback. The fork's public API differs from upstream's in the following ways:
   - `ParseState::new` returns `Result<ParseState, ParsingError>`.
   - `SyntaxSet::get_context` returns `ParsingError::MissingContext` when a dump is missing or corrupt, rather than panicking.
   - `Scope::atom_at` returns 0 when the index is out of range.
3. **Bad regexes never panic.** A pattern that fancy-regex can't compile (for example, Elixir's `\o{17}`) is cached as "never matches", and searching with it returns no match. `Regex::compiles()` reports which patterns failed.
4. **Backtrack limit.** Every regex is built with `RegexBuilder::oniguruma_mode(true).backtrack_limit(BACKTRACK_LIMIT)`, with `BACKTRACK_LIMIT = 30_000`. A search that hits the limit counts as no match. The limit trades colour fidelity against the worst line's cost. Measured against upstream (no limit) on 919 real files (132k lines), and on the slowest 2 KiB hostile line:

   | Limit | Real lines whose colours differ from upstream | Worst 2 KiB line |
   |---|---|---|
   | 10k | 13 (12 Makefile lines with `$(...)` in a target or `$(if ...)`, 1 Scala `=>` after a long call chain) | 70 ms |
   | **30k** | **1 distinct line**: libsodium's 1,829-byte `DEFS = ...` Makefile line (three copies in the corpus) | 83 ms |
   | 100k | the same as 30k | 125 ms |
   | 1M | 0 | about 1 s |

   At 30k, three R lines also differ in their scopes but not in their colours; every Makefile difference is a copy of the `DEFS` line. Only the 1M limit matched upstream on the whole corpus. `crates/wisp-render/tests/highlight_fidelity/` holds real lines, including the 10k regressions, with the colours an unlimited backtracker gives them; a test checks them at the shipped limit.
5. **Line budget.** `ParseState::parse_line_limited(line, set, &LineLimits)` bounds the work done on each line, independent of any clock:
   - Iterations: at most `8 × (bytes + 1) + 64` parser steps.
   - Searches: at most `32 × (bytes + 1) + 256` real regex searches. Cache hits are free.
   - Going over either budget returns `ParsingError::BudgetExceeded`.
   - `parse_line` is `parse_line_limited` with `LineLimits::DEFAULT`.
6. **Loop detection.** When `match_start` hasn't moved for more than `stall = 64` tokens, the parser steps one character forward and carries on. This ends the push and pop cycles that hung upstream (JavaScript `a /*`, Scala `:z@@@`, Svelte `<script>`).
7. **Cancellation.** `LineLimits::with_cancel(&AtomicBool)` makes the parser check the flag before every token and every regex search, and return `ParsingError::Cancelled` once it is set.
8. **Errors.** `ParsingError` gains `BudgetExceeded` and `Cancelled`. After any error the `ParseState` is unusable, and callers treat the rest of the block as plain text, which is what `wisp_render::highlight::CodeCursor` does.
9. **Reachability.** `SyntaxSet::reachable(&[usize])` returns the closure of syntaxes that can be entered from the given roots, through prototypes, includes, pushes, sets and `with_prototype`. Wisp's tests use it to prove that the allow-list reaches exactly 49 grammars: the 33 grammars behind the research's 34 languages (Diff needs none) and the 16 grammars they embed, four of which (XML, GraphQL, Sass, Graphviz) also have fence names.
10. **Lint fixes.** Clippy findings are fixed in place: `sort_by` becomes `sort_by_key`, collapsible `if`s become let-chains, and two `#[allow(..., reason = "...")]` cover upstream's eight-argument `parse_next_token` and the `HashMap` keyed by `MatchPattern`. A third keeps `Pattern::Match` unboxed, as upstream has it, now that fancy-regex 0.20's larger `Regex` trips `large_enum_variant`.

With the backtrack limit raised to 1M, the fork's scope output is byte-identical to upstream's on the 919-file corpus, so the strip and the rewrites change nothing else. At the shipped 30k limit the differences are the ones in the table above. The search cache, the `non_consuming_push_at` pop-loop check and `MAX_STACK_DEPTH = 100` are all upstream logic, now with named constants.

## Grammar dump

`crates/wisp-render/assets/grammars.bin` is the uncompressed, newline-mode syntax dump from two-face 0.5.2+bat-0.26.1 (`syntaxes-onig-newlines.bin`, 983,684 bytes, sha256 `7b567f54cf1b4b33abb7b64258647c256f99043c4bee6804b38f9ef612d0110a`). The licences of its grammars are in `crates/wisp-render/assets/grammars-licenses.md`, and two-face's own MIT licence is in `two-face-LICENSE-MIT`.

### Licences of the reachable grammars

`grammars-licenses.md` starts with one row per reachable grammar (source file, licence) and the full licence texts, then appends two-face's own acknowledgement listing for the rest of the dump. A test checks that every reachable grammar has a row whose licence text is present. F# is deliberately not reachable: its grammar (hoest/sublimetext-fsharp) ships no licence of its own.

To regenerate it for a new two-face release:

1. Clone bat at the tag two-face names (`git clone --depth 1 --branch vX.Y.Z https://github.com/sharkdp/bat`) and run `git submodule update --init --depth 1` on `assets/syntaxes/01_Packages` and on each `assets/syntaxes/02_Extra/*` submodule a reachable grammar comes from.
2. For each grammar `Grammars::reachable()` returns, find the `.sublime-syntax` file whose `name:` matches (bat keeps patched copies of some, such as `02_Extra/Kotlin.sublime-syntax`, next to the submodule they come from).
3. Take the licence from that file's submodule: its `LICENSE*` or `NOTICE` files. If bat patches the grammar (`assets/patches/<file>.patch`) or the file is bat's own converted copy (`assets/syntaxes/02_Extra/*.sublime-syntax`), also cite bat's licence (`LICENSE-MIT`, `LICENSE-APACHE`). For `01_Packages` (sublimehq/Packages) the root `LICENSE` applies unless the package has its own licence file (Rust) or the syntax file carries its own notice (C#, YAML), as that `LICENSE` says; when a notice only names a licence, include that licence's text.
4. Rewrite the table and texts, keep two-face's listing (`two_face::acknowledgement::listing()`) at the end, and run the tests.

Wisp loads the dump bytes directly rather than depending on two-face. two-face's only route to that dump is its `syntect-onig` feature, which turns on `syntect/regex-onig` and so needs the C engine, and its theme module needs syntect's `highlighting` and `dump-create` features. Neither can be satisfied by the stripped fork through `[patch.crates-io]`. The dump format is unchanged (bincode 1, with zlib-compressed lazy contexts), so the same bytes load in upstream syntect and in the fork. The fork reads it with bincode 2's `legacy()` configuration, which is bincode 1's encoding.

The dump was built for Oniguruma. fancy-regex in `oniguruma_mode` accepts nearly all of it. The few patterns it rejects never match, and the fuzz test checks that every reachable grammar still parses.

## Dependencies

| Crate | Version | Why |
|---|---|---|
| fancy-regex | 0.20.0 | Latest. Supports backtrack limits and `oniguruma_mode`. |
| bincode | 2.0.1 | Latest usable: 3.0.0 is a `compile_error!` tombstone (the project is unmaintained). Only the `std` and `serde` features. 2.x's default encoding differs from 1.x, so `dumps` decodes with `config::legacy()`, which matches 1.x; every syntax and context in the dump decodes identically to bincode 1.3.3. RUSTSEC-2025-0141 marks every bincode version unmaintained (informational, no vulnerability), and its maintainers call 1.3.3 complete; 2.0.1 is kept under the latest-release rule. Replacing bincode with a small in-tree reader of the legacy format is a possible follow-up. |
| flate2 | 1.1.10 | Latest. Uses the pure Rust `miniz_oxide` backend. |
| fnv | 1.0.7 | Hash for the search cache, as upstream. |
| regex-syntax | 0.8.11 | `escape` for backreference substitution. |
| serde, thiserror | workspace | Derives. |

All dependencies are declared in the root `[workspace.dependencies]`. There is no C code in the tree.

## Merging a new upstream release

1. Download the new release with `cargo download syntect@X.Y.Z`, or take it from `~/.cargo/registry/src/*/syntect-X.Y.Z` after adding it to a scratch crate. Record its commit from `.cargo_vcs_info.json`.
2. Diff the new release against 5.3.0, limited to the kept files: `dumps.rs`, `parsing/{parser,regex,scope,syntax_definition,syntax_set}.rs`. Ignore the removed modules.
3. Apply each upstream change by hand to the matching fork file, keeping the rules above: no comments, no panicking calls, budget and cancel checks before every regex search and every token, and `?` instead of unwrap.
4. If the serialised types change (field order or enum variants in `SyntaxSet`, `SyntaxReference`, `Context`, `Pattern`, `MatchPattern`, `ContextReference`, `MatchOperation`, `Scope`, `ClearAmount`), regenerate `grammars.bin` from the matching two-face release and update the size and hash above. The order of fields and variants must match the dump exactly.
5. Update the version, the commit and this document.
6. Run `cargo test -p syntect` (the guard tests), then CONTRIBUTING.md's checks. The highlight fuzz test in `wisp-render` must still cover every reachable grammar.
