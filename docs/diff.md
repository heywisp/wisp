# `wisp-diff`

A pure crate for diffs: the line diff, the reader for unified diffs, the line classifier and changed-word emphasis. It does no I/O, reads no clock, and installs no panic hook (`tests/purity.rs`). The `show-diff` plugin uses it to compute and read diffs, and the renderer to classify diff lines and pick the words to emphasise, so both always agree. It depends on `similar` 3.2.1, `unicode-segmentation` and `unicode-width`, and on no workspace crate (`tests/dependencies.rs`). The design is `docs/plans/diff-rendering.md` §2.

## Data

| Item | What it is |
|---|---|
| `FileDiff { path, old_path, hunks, added, removed }` | One file. `old_path` is set only for a rename. `added` and `removed` count `+` and `-` lines. |
| `Hunk { old_start, new_start, lines }` | One hunk. The starts are `None` when the numbers are unknown (a bare `@@`). |
| `DiffLine` | `Context`, `Removed` or `Added`, holding the text after the prefix, or `NoNewline` for `\ No newline at end of file`. |
| `Repairs { recounted, unprefixed, split }` | What the reader fixed in one input: counts recounted, lines with no prefix shown as context, and the number of files a multi-file input was split into (0 for one file). |
| `FileDiff::text()` | The canonical hunk text: each hunk's `@@ -a,b +c,d @@`, or a bare `@@` when either start is unknown, then its lines with their prefixes. No file headers. `b` and `d` are always counted from the lines, and never count `\` lines. |

## The line diff

`diff_texts(before, after)`:

- `\r\n` becomes `\n` in both texts, then each is split after every `\n`. A line keeps its terminator while it is compared, so `x` and `x\n` differ, and a last line with no terminator is followed by `NoNewline` on its side.
- `similar::capture_diff_slices` with `Algorithm::Histogram`, then `group_diff_ops` with a radius of `CONTEXT_LINES` (3). Two changes with 6 or fewer equal lines between them share a hunk.
- The hunks are counted and numbered. A side with no lines in a hunk takes the line before as its start, as in `@@ -0,0 +1,2 @@` for a new file.
- `similar` is built with `default-features = false`: no `std`, so its clock-based deadlines are compiled out and no diff depends on time.

## The classifier

`Lines` decides what each line of a diff is, one line at a time. `Lines::full()` applies rules 1 to 4, and `Lines::hunks_only()` rule 5. `Lines::uncounted()` is the full classifier with every hunk read as uncounted, which the reader uses to recount. `push(line)` returns a `Step`: the line's `Kind`, and `previous` when the line revises the one before it.

1. **`@@` at column 0** is always a hunk header (`Kind::Hunk`), with its counts when they parse (`@@ -a[,b] +c[,d] @@`, a missing length meaning 1).
2. **A counted hunk with lines left:** the first character alone decides (space or an empty line is context, then `-`, `+` and `\`), so `--- x` and `----` stay removed lines. The hunk ends when both counts reach 0. A line with any other first character ends it early and is classified by rule 4.
3. **An uncounted hunk:** the first character decides, except that `diff --git ` ends the hunk as a header, and a `+++ ` line directly after a `--- ` line makes the pair a file header: the `+++ ` step returns `previous: Some(Header(Old))`. A removed `-- x` directly followed by an added `++ y` therefore reads as a header pair; that is the one ambiguity left.
4. **Outside a hunk:** `diff --git `, `index `, the mode lines, `rename from`/`to`, `copy from`/`to`, `similarity index`, `dissimilarity index`, `--- ` and `+++ ` are headers. Any other line goes by its first character, and a line with none of the prefixes is `Kind::Other`. A non-empty body line (a space, `-` or `+` first) starts an uncounted hunk, read by rule 3 until the next header or `@@`, so a body with no `@@` keeps its `--- ` and `+++ ` lines unless they form a header pair. An empty line, or a line with no prefix, does not.
5. **Hunks-only mode:** rule 1, then the first character alone for every other line, with no headers at all. Counts only number the lines.

## Reading a unified diff

`parse_hunks(text)` reads `FileDiff::text()` back in hunks-only mode, and `parse_hunks(file.text())` gives `file` back (the fixed-point property). A line with no prefix becomes a context line holding the whole line. `\r\n` is read as a line end, so a line whose text ends in `\r` loses that `\r` on the way back; `diff_texts` produces one only from a text ending in a lone `\r` (`diff_texts("a\n", "foo\r")`) or holding `\r\r\n`, and such a file is not a fixed point.

`parse_diff(text)` reads a diff a model wrote, tolerantly, into `Parsed { files, repairs }`:

- **Clean-up:** `\r\n` becomes `\n`, empty lines at both ends are dropped, and a ```` ``` ```` fence around the diff is stripped. A last context line that was blank and written without its space is one of those empty lines, so its hunk falls one line short and is recounted without it.
- **Counts are trusted only when they hold.** The whole input is read with `Lines::full()`. Each counted hunk must take exactly its counts and end at a real boundary: after any `\` lines and empty lines, the next line is an `@@`, a `diff --git ` line, a `--- ` line directly followed by a `+++ ` line, or the end of the text. No other header proves a boundary, so counts that stop one line early before a removed `--- x` fail the check. A blank line between files or hunks therefore keeps the counts trusted, and becomes an implicit hunk with no changes, which is dropped. Prose after a counted hunk that holds is not a boundary: the input is recounted and the prose becomes unprefixed context at the end of the hunk.
- **Otherwise the input is read again with `Lines::uncounted()`,** keeping the hunks' starts, and `repairs.recounted` is set. The reread covers the whole input, not one file: a wrong count can move a file boundary, so the files of the first reading can't be trusted either. A hunk whose counts held is recounted to the same lines, so the result differs only where a rule 3 ambiguity applies.
- **Files:** a file starts at `diff --git `, and at a `--- ` or `+++ ` header once the current file has hunks or has already had that header. Paths come from `+++ ` (or `---` when `+++` is `/dev/null`) and from `diff --git a/x b/y`, cut at a tab, trimmed at the end and with the `a/`/`b/` prefix dropped, `/dev/null` kept as it is (`header_path(text, prefix)`, exported so every reader of header paths uses the same rule); `rename from`/`rename to` set the old path. `old_path` is kept only when it differs from the path. Other git metadata is dropped. `repairs.split` is the number of files when there are several.
- **Lines outside any `@@`** form an implicit hunk with unknown numbers, so a body with no `@@` is one hunk. An implicit hunk with no `+` or `-` line (a sentence before the diff, say) is dropped, and so is a file with no path whose hunks are all empty (a bare `@@` directly before a `--- `/`+++ ` pair).
- **No prefix:** an empty line is empty context. A non-empty line is shown as context and counted in `repairs.unprefixed`; in a counted hunk it also ends the hunk early, so the input is recounted.
- A file with no `+` or `-` line is still returned; `FileDiff::has_changes()` tells the caller.
- **Known limits:** a diff whose very first line is a removed `-- x` (written `--- x`), with no headers and no `@@`, reads that line as a file header, since nothing before it shows a body has started. A Markdown bullet before a diff (`- renamed`) reads as a removed line, so it lands in an unnamed first file of its own.

## Word emphasis

A `Run` holds one change run: `push_minus` for the `-` lines, then `push_plus` for the `+` lines. `pairs(cancel)` recomputes the pairing and returns `Ok(Some(pairs))` when it changed since the last call, `Ok(None)` when it didn't, and `Err(Cancelled)` when `cancel()` said stop. `current()` is the last pairing returned. After `Err(Cancelled)` the caller must call `pairs` again, before pushing the next line, until it returns `Ok`; otherwise the pairings returned depend on how the lines were chunked between calls. A `Pair` holds the two line indices and, for each side, the byte ranges to emphasise: sorted, disjoint and on grapheme boundaries.

1. **Tokens** (`tokens(line)`): a run of graphemes starting with a Unicode alphanumeric or `_` is one word, a run of whitespace graphemes is one token, and every other grapheme is a token of its own. A line over 512 bytes (`MAX_LINE_BYTES`) or 256 tokens (`MAX_LINE_TOKENS`) is never paired.
2. **Pairing:** greedy and forward, as delta does. Each `-` line tries the `+` lines from the cursor on and takes the first within distance 0.6; the cursor moves past it, so the `+` lines passed over stay unpaired. A `-` line with no match leaves the cursor where it was.
3. **Distance:** tokens are aligned with Myers (`similar`), and with `w` the display width of a token's trimmed text, distance = (deleted + inserted) ÷ (2 × equal + deleted + inserted), 0 when that is 0. It is compared in integers: `5 × changed ≤ 3 × (2 × equal + changed)` (`distance_within`).
4. **Ranges:** the deleted tokens on the `-` side and the inserted tokens on the `+` side, with an equal whitespace token between two changed tokens joined to them. A side whose every non-whitespace token changed gets no ranges. Under the 0.6 rule that can only happen when every changed token has no width (a zero-width space, say), since such a side has no equal token with any width.
5. **The size cap:** with more than 64 lines on either side (`MAX_RUN_LINES`), the run gets no emphasis. The call that finds the cap crossed returns one empty pairing if the last pairing returned was non-empty, whether or not the run was frozen, and drops its cache; the run is then inert and returns `Ok(None)` for good.
6. **The cache:** each `(minus, plus)` alignment is computed once and kept. Each call walks the pairing again from the cache, aligning only pairs it hasn't seen. The walk asks `cancel()` every 64 steps (`WALK_CANCEL_STEPS`) and before each alignment.
7. **The budget:** each new alignment spends the two lines' token counts from 16,384 units per run (`RUN_BUDGET`), and cache hits are free. When an alignment the walk needs doesn't fit, the run freezes: it keeps the pairing last returned, and every later call returns `Ok(None)` until the cap clears it. The run owns its budget, so it can't be shared between runs by mistake.

Emphasis is defined by this incremental procedure, pushing a run's lines in order and calling `pairs` after each `+` line. For a run that never froze, the final pairing equals one call over the whole run (a property test), and for every run it is deterministic: the budget counts tokens, not time.

## Tests

- Unit tests beside each module: tokens, distance, pairing, ranges, caps, budget, cache and cancel (`words`); context and merging, new and deleted files, the newline marker and CRLF (`line_diff`); every classifier rule (`lines`); every row of the reader's table and the count check (`parse`).
- `tests/properties.rs`, seeded: applying the computed hunks to `before` gives `after`; `parse_hunks(text())` is a fixed point; corrupting every count reads back the same hunks with `recounted`, except where the rule 3 ambiguity splits the file; incremental pairing equals one pass within the budget, with the cap applied to both; emphasis ranges are sorted, disjoint and on grapheme boundaries.
- `tests/purity.rs` and `tests/dependencies.rs`, as for `wisp-media`.

## Rendering diffs (`wisp-render`)

The renderer draws a code fence as a diff when the first word of its info string is `diff`, `patch` or `diff-hunks`. For those three the parser keeps the whole info string (`diff src/x.rs`), the label shows all of it, and the rest after the first word is the hint. `diff` and `patch` classify with `Lines::full()`, `diff-hunks` with `Lines::hunks_only()`. The design is `docs/plans/diff-rendering.md` §3.

### Two cursors

- **Structure** (`CodeCursor` without grammars: `PlainCode`, the fallback inside `CachedLines` and `KeptLines`, the TUI worker's plain lines). Each line is classified by `Lines`. Headers, `@@` and `\` lines are `text.muted`; a `+` or `-` row is its sign in `ok` or `error` on `diff.add` or `diff.del`, then its code on the same tint; a context row is its leading space, then its code; any other line is plain. It never revises a line.
- **Full** (`CodeCursor` with grammars, which in the TUI only the highlight worker builds). The same structure, plus syntax colours and changed words.
  - **Language:** a `diff --git a/x b/y` line gives `y`, and a `+++` line its path, or the `---` path when it is `/dev/null`, with the `a/` or `b/` prefix and anything after a tab dropped. A path resolves through `LANGS` as a whole, then by its lowercased extension, then by its file name (`Makefile`). When the path resolves to nothing, or before any header, the hint is used the same way, one word at a time, and the first word that resolves wins (`diff-hunks klingon src/x.rs` is Rust). Nothing found means no syntax colours.
  - **Two sides:** the old side reads context and `-` lines, the new side context and `+` lines, each with its own `ParseState` and `ScopeStack`. A context row is painted from the new side. Both sides restart at every `@@` and file header.
  - **Limits:** the `Limits` and `Stop`s end syntax colouring for the rest of the fence, as for any fence; structure and word emphasis carry on.
  - **Words:** a change run (its `-` lines, then its `+` lines, with any `\` line inside it) feeds a `Run`, and `pairs` runs after each `+` line. A changed word is `text` on `diff.add.word` or `diff.del.word`.
  - **After a cancel:** when `pairs` returns `Err(Cancelled)`, the run asks again before its next line is pushed, and when a later line ends the run. The cache and the spent budget have moved on but the last pairing hasn't, so pushing on without the retry could freeze the run at a different point than an uncancelled pass. This follows the contract above, except that a run whose retry is cancelled too stops changing its emphasis rather than asking again: under a flag that stays set (the worker's `GOVERNOR` stop, a dropped stream) asking until `Ok` would never end.
  - **A capped run:** once either side has more than `MAX_RUN_LINES` lines, or `Run::is_capped` reports the cap, the run stops keeping its lines and stops pairing, so a long run holds no per-line state. Its later lines keep their syntax colours and get no emphasis.

Every code span of a diff line, context included, carries `Look.diff_code`, and `+`/`-` rows are banded to the content edge.

### Revisions

`CodeLines::revise` returns a `Revision { from, lines }` after a line when earlier lines changed their spans: a `+` line that changes a run's pairing, or the `+++` line of a `---`/`+++` pair in an uncounted hunk (rule 3), which turns the provisional removed row into a header. `from` is the first line whose spans changed and `lines` runs from it through the line just returned, so applying it replaces a suffix and never changes the count. A consumer calls `revise` after every line:

- `Renderer::code` applies it to the lines it has collected.
- `HighlightedCode::push` applies it and keeps the lowest `from` until `take_revised`; `rewind` replays the kept lines and marks the first one whose spans differ from before, since a dropped `+` line can take emphasis away. A rewind whose replay is cancelled (the worker's 1 s governor) keeps the spans already shown and drops the open run, so it never resends earlier lines with fewer colours than they had. `feed` returns its range from that line.
- `CodeWorker::apply_with_cancel` returns its `CodeUpdate` from `min(delta.from, revised)`, which `SpanCache::apply` and `KeptCode::apply` accept.
- The TUI worker batches one-line updates: an update that starts after the batch's first line truncates the batch there and extends it, and one that starts at or before it replaces the batch, since every update runs to the fence's last line.

The cursors without grammars never revise, so a rule 3 `---` line shows as a removed row until the worker's spans arrive.

### Late spans

A fence is highlighted live while its reply streams: the stream sends its lines to the worker, holds unhighlighted rows for up to `CODE_WAIT` (2 s), and drops a fence's queued work once the fence ends. A fence that ends before its spans arrive, because the grammars were still loading, the worker was busy past the wait, Esc stopped the turn, or the reply was rebuilt from a resumed log or `--replay`, is filled in once its reply has finished:

- **Replayed cells never wait:** a prose cell built by `Transcript::replay` is never held for spans (`Stream::replayed`), whenever the grammars arrive. It is sealed during the replay, finished without drawing, and goes through the late path, so a resume renders only the cells the screen shows and the nearest one above them with rows, rather than a held cell per frame with every later cell rebuilt behind it.
- **Joining:** when a finished prose cell passes the transcript's `next`, it joins `Transcript::views` with diff cells, if the transcript has a code key and the grammars haven't failed. Plain and screen-reader output have no code key, so nothing changes there.
- **What is sent:** `Stream::missing_code` rebuilds each fence's lines from the finished blocks and keeps the fences whose highlighted lines stop short of their last line, each as one delta from line 0. A cell with none leaves the set at once, so a reply highlighted live costs one parse of its blocks.
- **Esc and the governor:** after Esc the worker still answers a stream's queued lines, plain, so the live rows settle at once. It marks those updates with `CodeUpdate::cancelled`, the first line made after the cancel; `KeptCode` keeps the mark per fence, and `KeptCode::highlighted` counts only the lines before it, so the fence is sent again once the reply has finished. Lines the 1 s governor stopped carry no mark: they count as highlighted and stay plain. The worker also keeps, per stream, the fences the governor or a panic gave up on, across a restart, so a fence sent again for any reason comes back plain at once instead of with a fresh second. Such updates, and a panicked fence's plain lines, carry `CodeUpdate::given_up`: `KeptCode::apply` then keeps the highlighted lines it already holds wherever their text is unchanged, and takes only the rest plain, so a fence whose coloured head arrived before its plain tail was lost never loses that head. A fence cut by Esc before the governor tripped is not given up, and is highlighted again with a fresh budget.
- **Slices:** `take_code` feeds at most `LATE_CODE_LINES` (400) lines a call and works out at most `LATE_CODE_SCANS` (4) cells' missing fences a call, newest cell first. The worker also serves the newest stream first, so after a resume the bottom of the transcript, the part on screen, is coloured first.
- **A fresh start:** a cell's first late feed has `restart` set. `Highlights` then cancels the stream's earlier work, forgets its retire mark, gives it a new cancel flag and a new generation (unique across streams), and the worker drops the stream's old state apart from the fences it gave up on. Replies from an earlier generation are discarded, so a cancelled live request can't land after the restart. `Highlights::finish` forgets the stream's generation.
- **Live work first:** the worker takes the newest stream with work, and between two fences of a request it checks for work on a newer stream. If there is some, it puts the rest of the request back, merging anything queued since for that stream into it with `FenceDelta::merge` as `send` does, so a fence never has two queued deltas (unless a restart or drop replaced the request, when the rest is dropped) and serves the newer stream, so a live reply waits for at most one late fence.
- **Applying:** a reply for a finished cell is applied only after the cell's first late feed went out, so nothing from the live request lands between joining and that feed. `Stream::apply_late_code` writes the update into the cell's `KeptCode`. Spans change only colours, so rows never move. The cell leaves the set once every fence it sent has its highlighted lines up to its last line.
- **Limits:** `Transcript::code_failed` empties the set and stops later cells joining, and a rewind's `cut` drops the late cells after the cut; compaction removes no cells, so it drops none.

### Colour depths

`Theme::resolve` reads `diff_code` with the background:

| Depth | Code in a diff | Changed words |
|---|---|---|
| Truecolour | A foreground under 4.5:1 on its row paints as `text`, computed from the contrast formula (only dark comments) | `text` on the word tints |
| 256 | The same rule on the row's index | `text` on the word indices; dark `diff.add.word` adds bold |
| 16 | No foreground, context rows included | Reverse video; inside a selection, which reverses the whole row, a changed word loses its mark (an accepted cost, plan Q1) |
| `NO_COLOR` | Nothing | Reverse video, with the same cost inside a selection |

### Rendering tests

- `highlight/diff/tests.rs`: the per-side state, the reset at `@@`, the language from headers, hint and nothing, a second file switching language, `diff-hunks` against `diff`, removed `--` lines in a counted hunk, the word looks, revisions (their `from`, a pairing taken over, the rule 3 header pair), the last run of a fence, worker updates starting at the revision and after a rewind, the retry after a cancel, span texts always joining to the line, and the review's six: `a_no_newline_marker_does_not_end_a_change_run`, `context_lines_are_painted_from_the_new_side`, `a_header_with_an_unknown_language_keeps_the_hint`, `a_run_over_the_cap_stops_keeping_its_lines`, `a_rewind_under_cancel_keeps_the_lines_already_shown` and `diff_rows_are_banded_to_the_content_edge_on_a_wide_screen`; and the snapshots at every depth and mode (`diff-<mode>-<depth>`, widths 120, 80 and 24, plus a run over the budget at 120).
- `theme/tests.rs`: the floor at truecolour and 256, every diff-code foreground readable or `text`, and the 16-colour and `NO_COLOR` rules.
- `stream_fuzz`: `diff_fences_streamed_in_chunks_keep_the_spans_of_one_pass` and its soak variant.
- `wisp-tui`'s `app/highlight/tests.rs`: a revision inside a worker batch.
