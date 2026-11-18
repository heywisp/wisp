# Whole-TUI design review

*Design decisions, 26 October 2026, ready to build. Full screen only (the plain and screen-reader modes are frozen).
It builds on `tui-brand.md`'s tokens, the idle orb and the welcome card as built, and changes the look only. The
lasting rules are folded into `tui-brand.md` (§2.2, §2.7, §4.2, §4.3, §5.1, §7.1, §7.2, §7.5, §7.8, §8.5); this
document keeps the diagnosis, the full specification of each decision and the build plan.*

## Decisions

| Area | Chosen | Decided by | Not chosen |
|---|---|---|---|
| 1. Foundations | **1A** Firm lines, one selection | recommended, accepted 2026-10-26 | 1B bands instead of lines; 1C weight only |
| 2a. Your messages | **2a-1** Your message on a band | recommended, accepted 2026-10-26 | 2a-2 bold first line; 2a-3 a rule between turns |
| 2b. Finished thinking | **2b-1** Fold it to one row when it ends | owner, 2026-10-26 | 2b-2 three quieter lines; 2b-3 as today |
| 2c. Tool rows | **2c-1** Tight rows, the state in the facts | recommended, accepted 2026-10-26 | 2c-2 an elbow under the row; 2c-3 output on a band |
| 2d. Errors and Retry cards | **2d-1** Red on the `⨯` and the first clause only, then a card | owner, 2026-10-26 | 2d-2 a red rail; 2d-3 a tinted headline |
| 3. The composer | **3B** A rounded box | owner, 2026-10-26 | 3A a `surface` band; 3C today's rules, raised |
| 4. The status row | **4A** Three groups | recommended, accepted 2026-10-26 | 4B hints on their own row; 4C fewer facts |
| 5. Pickers and `/config` | **5A** A docked panel | recommended, accepted 2026-10-26 | 5B a boxed panel; 5C a centred dialog |

Routine choices were settled from Claude Code, Codex, OpenCode and Aider practice. The owner decided the three
places where those tools disagree in a way you would notice every day: the composer, finished thinking and errors.

Screenshots below are cut from the insta snapshots in `crates/wisp-tui/tests/snapshots/chrome/` (dark,
truecolor), as the screen looked before these changes.

---

## 1. Foundations: colour, weight, lines and spacing

### What read badly

1. **The lines were almost invisible.** Rules, the welcome frame, the composer's two rules and the table rule are
   all `line`: `#2D2C28` on `#0E0E0C` is 1.38:1 dark, and `#D8D6CE` on `#ECEBE6` is 1.22:1 light.
2. **Selection had no consistent look, and was often invisible.** Pickers marked the current row with a `surface`
   band (1.11:1 dark, 1.14:1 light), `/config` the selected row with `accent.tint` (1.33:1 dark, 1.05:1 light), the
   queued-message list with `surface`, and the ask and Retry cards with a `›` alone.
3. **Everything was grey.** Five greys at once, and `Ready` fainter than the facts beside it.
4. **SGR faint was in use.** While an ask card had the keys, the composer was drawn with SGR 2, which §4.1 forbids.
5. **Spacing within a tool group was uneven.** A blank row followed every tool that printed output.
6. **Facts drifted far from what they describe.** At 200 columns `exit 0 · 12s` sat 150 columns from its command.
7. **Errors were the loudest thing on screen.** The whole headline was `error` red.

```
| ✓ Ran       cargo test -p wisp-core                                                       exit 0 · 12s
|     test 213 ... ok
|     … 212 more lines · ctrl-o
|
| ⨯ Ran       cargo clippy --workspace                                                     exit 101 · 2s
|     error 4 ... ok
```

### Decision: 1A, firm lines and one selection

- **Two tokens rise; none are added.**

  | Token | Dark | Light | ANSI-256 | ANSI-16 |
  |---|---|---|---|---|
  | `line` | `mix(text, bg, .24)` = `#454441`, 1.98:1 on `bg`, 1.79:1 on `surface` | `mix(text, bg, .30)` = `#ACABA7`, 1.93:1 on `bg`, 2.20:1 on `surface` | 238 dark, 248 light | 90 (was the default foreground) |
  | `accent.tint` | `mix(accent, bg, .22)` = `#222D41`, 1.40:1 | `mix(accent, bg, .20)` = `#C1CDE0`, 1.35:1 | 236 dark, 252 light | reverse video, as now |

  On the new tint, `text` is 12.20:1 dark and 11.28:1 light, `text.muted` 5.79:1 and 4.59:1, `accent` 5.15:1 and
  4.08:1, and `text.faint` 2.91:1 and 2.37:1. So on a selected row the text is `text`, facts step up from
  `text.faint` to `text.muted`, and `accent` is used only for glyphs (`›`, `●`), whose floor is 3:1. §2.7's
  correction recipe moves with them: `line = mix(fg, bg, .24)` dark, `.30` light; `accent.tint` is the accent at
  `.22` dark, `.20` light.
- **One selection recipe everywhere.** The selected row gets an `accent.tint` band from column 0 to the content
  edge, a `›` in `accent` at column 1, and its text in `text`. Pickers, `/config`, the ask card, the Retry card, the
  queued-message list and the command suggestions all use it. **Current** (the model or endpoint in use) is a `●`
  in `accent` with no band, so current and selected never look alike; a current row that is also selected takes
  the band and keeps its `●`.
- **Four tones, used by role.** `text` for what you read; `text.muted` for every status sentence (`Ready` too),
  facts and details; `text.faint` only for keys, hints and rails; `line` only for structure.
- **No SGR faint anywhere.** Anything that loses the keys steps down by tone (§3 below). The renderer never sets
  SGR 2.
- **A content edge for facts.** Right-hand facts in the transcript, pickers and cards end at
  `min(width − 1, 119)`: at the screen edge up to 120 columns, then no further out. Selection bands and the user's
  band end at the same edge. The status row and the composer box stay full width, because they are chrome.
  *Since changed: the transcript now uses its whole area, with facts and bands at `width − 1`; only chrome keeps the
  119 edge (`tui-brand.md` §4.3).*
- **Tight groups.** No blank rows inside a tool group, with or without output. One blank row between a turn's
  blocks, as now.
- **Errors say it once.** The `⨯` and the headline's first sentence (what happened) are `error`; the rest of the
  headline (what still works, §8.5) is `text`.

### How 1A degrades

| | 1A |
|---|---|
| ANSI-256 | `line` 238 / 248, `accent.tint` 236 / 252 |
| ANSI-16 | lines and frames in 90; the selected row in reverse video (§2.5), `›` included |
| `NO_COLOR` | lines and frames plain; the selected row in reverse video |
| ASCII and ambiguous-wide | `-` rules, `+-+` and `\|` frames; `>` marks the selection, `*` current |
| Reduced motion | nothing here moves |
| 40 columns | the content edge is the screen edge; facts drop first (§4.3) |
| 200 columns | facts and bands stop at column 119; prose at the 100-column measure (since changed: the transcript uses the whole width, `tui-brand.md` §4.3) |

**Not chosen.** 1B (structure from `surface` fills instead of rules): a 1.11:1 fill disappears on any background
that isn't Stone's. 1C (no token changes, structure from bold and blank rows): it leaves light-mode selection
invisible.

---

## 2. The transcript

### 2a. Your messages: on a band (2a-1)

**What read badly.** Your message was a grey `›` and a line of `text`, the same weight as Wisp's answer, so a long
session read as one stream.

```
| › Question 3
|
| ⠶ An answer.
|
|   2.4s · cut off at the output limit · $0.03 · cache 92% read
|
| › Question 4
```

**Decision.** Your message sits on a `surface` band from column 0 to the content edge, with one band row above and
one below your text inside it (since changed: only behind its text rows, `tui-brand.md` §7.1). `›` in `user` at
column 1, the text in `text` from column 3, wrapped at the prose measure. Claude Code, Codex and OpenCode all set the
user's message apart with a fill or a bar. Wisp's prose keeps `⠶` in `accent` on its first block, and the turn footer
stays `text.faint`. A message with images or held chips keeps them inside the band. The band is drawn with explicit
spaces in `surface` (§4.4), and copying a message copies its text only.

| Truecolor, 256 | ANSI-16 | `NO_COLOR` | ASCII | 40 → 200 columns |
|---|---|---|---|---|
| `surface` (234 / 255) | no band: the `›` and a blank row above and below, as before (since changed: only behind its text rows, `tui-brand.md` §7.1) | no band | `>` | the band spans column 0 to the content edge, at most 120 cells (since changed: the whole width, `tui-brand.md` §4.3) |

**Not chosen.** 2a-2 (a bold first line): bold is reserved for verbs and headings, and long messages still blur.
2a-3 (a rule before each of your messages): a row per turn.

### 2b. Thinking: fold it when it ends (2b-1, owner)

*Superseded by `docs/plans/thinking-cells.md` and later changes: a live thought now streams whole, a finished thought keeps its last 3 text rows under `╎ Thought for 4 s ▸`, no cell names `ctrl-o`, and a click on a `▸` row opens the cell in a reader (`docs/commands.md`, "Folded rows, the ctrl-o picker and the reader"). The decision below is the history.*

**What read badly.** A finished thought kept a label, six italic lines and a `… N more lines` row: 8 rows a turn
in a colour of their own, competing with the answer.

```
| ╎ Thinking · 4s
| ╎ Placeholder thought line one, wrapped narrowly.
| ╎ Placeholder thought line two, longer than the first.
| ╎ Placeholder thought, 3rd.
| ╎ Placeholder thought line 4, middling.
| ╎ Placeholder thought line five, medium.
| ╎ Placeholder thought line six, rather long.
| ╎ … 3 more lines · ctrl-o
```

**Decision.**

- **While it streams:** unchanged. `╎ Thinking · 4 s`, then the last 3 lines of the body in italic `thinking`.
- **When it ends:** it folds to one row, `╎ Thought for 4 s · ctrl-o`. The rail is `text.faint`, `Thought for 4 s`
  is `thinking` (upright: it is a label), and ` · ctrl-o` is `text.faint`. The duration uses §4.3's finished form
  (`4 s`, `1 min 12 s`); under a second it reads `Thought for 1 s`.
- **Ctrl-O** (transcript mode) shows every thought in full, as now.
- **A thought with no text** (redacted, or a provider that sends only a signature) folds to `╎ Thought for 4 s`,
  with no key, because there is nothing to show.
- **A turn stopped mid-thought** folds the same way when the turn ends.

Claude Code folds to "Thought for 4s", and Codex keeps one bold summary line. Aider and OpenCode show reasoning in
full, which is why this was the owner's call.

| Truecolor, 256 | ANSI-16 | `NO_COLOR` | ASCII | Reduced motion | Width |
|---|---|---|---|---|---|
| as above | `thinking` is 37 / 90 | rail and words | rail `:` | the live 3 lines jump rather than scroll | one row at any width; ` · ctrl-o` drops when the whole row doesn't fit (below 27 columns for `Thought for 4 s`) |

**Not chosen.** 2b-2 (three quieter lines in `text.muted`): still 5 rows a turn. 2b-3: as before.

### 2c. Tool rows: tight, with the state in the facts (2c-1)

**What read badly.** Blank rows inside a group, facts far out, and states wider than the verb column
(`Couldn't run`, `Timed out`) that pushed their targets out of line.

```
| ◦ Timed out ./slow-server                                                                 after 2m 00s
|     listening on :8080
|
| ⨯ Couldn't run {"cmd": 3}
|     `command` must be a string
```

**Decision.**

- **The verb stays the tool's verb** (`Ran`, `Read`, `Searched`); what happened moves to the glyph and the facts:

  ```
  ✓ Ran       cargo test -p wisp-core                                   41 passed · 12 s
  ⨯ Ran       cargo clippy --workspace                                    exit 101 · 2 s
  ◦ Ran       ./slow-server                                      timed out after 2m 00s
  ⨯ Ran       {"cmd": 3}                                                  couldn't start
      `command` must be a string
  ```

- **Facts:** `exit 101` and `couldn't start` in `error`, the rest in `text.muted`. `timed out after 2m 00s` and
  `stopped` are `text.muted` beside a `◦`.
- **Output** stays at column 5 in `text.muted`, and `… N more lines · ctrl-o` in `text.faint`. A reason a tool
  couldn't start is its first output row, in `text.muted`.
- **No blank rows inside a group.** Facts end at the content edge (1A).
- **When the row is short,** durations and counts drop first; a state (`exit 101`, `timed out`, `couldn't start`)
  stays, and the target shortens from the middle instead.

| Truecolor, 256 | ANSI-16 | `NO_COLOR` | ASCII | Reduced motion |
|---|---|---|---|---|
| as above | glyphs and states in 92 / 91 / 90 dark, 32 / 31 / 90 light | glyph shape and the words carry it | `+ x .` | the running comet rests (§6.6) |

**Superseded (tool-rows, `docs/plans/tool-rows.md`).** The verb column and the output lines above are gone. Each
call is one line: the tool's title (or a made title such as `Ran cargo fmt --all --check`, with no padded verb
column), the command muted beside it, and the facts, `timed out · 2m 00s` among them. Output and a failure's reason
live in the reader, which a click on the row opens; only a content tool's view (show_diff's diff) stays under its
row. `docs/tools.md`, "Tool rows in the TUI", has the current rules.

**Not chosen.** 2c-2 (a `└` elbow before the output): a box character beside text people copy (§4.4). 2c-3
(output on a `sunken` band): invisible at 1.03:1 unless raised, and then heavier than the row.

### 2d. Errors and Retry cards: headline, details, card (2d-1, owner)

**What read badly.** The headline was all red, the detail lines changed tone line by line, and the Retry options
sat at the detail indent with only a `›` for the selection.

```
| ⨯ Anthropic had a server error. 1 step was kept: the commands that ran and their output.
|   Gave up after 6 attempts at this step.
|   HTTP 200 · api_error · request req_011CTurn18 · mid-stream
|   Upstream provider stream was interrupted.
|   › Retry              on direct · cache rebuild ≈$0.04
|     Switch endpoint    pick one, then retry · cache rebuild
|   ↑↓ choose · enter · esc dismisses
```

**Decision.**

```
 ⨯ Anthropic had a server error. 1 step was kept: the commands that ran and their output.
   Gave up after 6 attempts at this step.
   HTTP 200 · api_error · request req_011CTurn18 · mid-stream
   Upstream provider stream was interrupted.

 › Retry              on direct · cache rebuild ≈$0.04
   Switch endpoint    pick one, then retry · cache rebuild
   ↑↓ choose · enter · esc dismisses
```

- **The headline.** `⨯` and the first sentence (what happened) in `error`; the rest of the headline (what still
  works, §8.5) in `text`. A headline of one sentence is red throughout, `⨯` included.
- **The details** in `text.muted`, as one block at column 3.
- **The card,** after one blank row: each option a row with the 1A selection (band from column 0, `›` at column 1),
  its name at column 3 in `text` and its detail in `text.muted`, aligned in a second column. The key row follows:
  keys in `text.muted`, words in `text.faint`.
- **Notices** (`∙ Account → direct`) keep a `text.faint` bullet and `text.muted` words.
- **Once used,** the card shrinks to its one-line record as now (`Retried on direct`, §8.5).

| Truecolor, 256 | ANSI-16 | `NO_COLOR` | ASCII | Width |
|---|---|---|---|---|
| band 236 / 252 | the selected option in reverse video; the clause in 91 / 31 | reverse video; the `⨯` and words carry the error | `x`, `>` | below 60 columns each option's detail wraps under its name |

**Not chosen.** 2d-2 (a `┃` rail in `error` down the headline and card): heavier than most errors deserve, and it
competes with the gold approval rail. 2d-3 (the headline on a `diff.del` band): loud in a new way, and gone in
ANSI-16.

---

## 3. The composer: a rounded box (3B, owner)

### What read badly

- Two full-width rules in `line` (1.38:1) framed one row. On most screens they didn't show, so the prompt floated.
- The caret was a reverse-video cell in `text.faint` over the placeholder's first letter, and read as a stray block.
- When an ask card had the keys, the whole composer was SGR faint.

```
|──────────────────────────────────────────────────────────────────────────────────────────────────────────
| › Ask anything, or type / for commands
|──────────────────────────────────────────────────────────────────────────────────────────────────────────
| ⢎⠶⡰  Ready                                         sonnet-5-5 · low · Anthropic API key · $0.014 · cache 92%
```

### Decision

```
╭──────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│ › Ask anything, or type / for commands                                                                   │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────╯
 ⢎⠶⡰  Ready                                         sonnet-5-5   low · Anthropic API key · $0.014 · cache 92%
```

Claude Code and OpenCode draw the composer this way. "Never boxes around messages" (§7.8) is about the transcript;
the composer is chrome.

- **Glyphs.** The welcome card's frame (`chrome::welcome::frame_for`): Unicode `╭` U+256D, `╮` U+256E,
  `╰` U+2570, `╯` U+256F, `─` U+2500 and `│` U+2502. These are East Asian ambiguous width, so the ambiguous-wide
  and ASCII sets both use `+`, `-` and `|`.
- **Geometry.** The box spans the full width, column 0 to `width − 1`, like the status row. The top edge takes the
  old top rule's row and the bottom edge the old bottom rule's, so the box costs no rows. Inside each row: `│` at
  column 0, a space, the prompt `›` at column 2, a space, the input from column 4 to `width − 3`, a space, and `│`
  at `width − 1`. The input is `width − 6` columns wide. Continuation rows start at column 4, under the first
  character.
- **Growth.** One input row, growing with wrapped lines and newlines to at most 8 input rows (10 rows with the
  edges), as now. The box grows upward and the transcript view gives up the rows. Past 8 rows the input scrolls
  inside the box, keeping the caret's row in view, as now.
- **Wrapping.** Input wraps at word boundaries, as Claude Code and Codex do: a word that doesn't fit moves whole to
  the next row, and only a word wider than the input is broken, filling the row it starts on. Whitespace is a break
  point wherever it sits, leading indentation included; the no-break spaces (U+00A0, U+2007, U+202F) are not.
  Spaces and tabs at the row's edge all hang in the padding column, drawn as at most one space, so the next row
  never starts with a space after text; a caret after them sits at the start of the next row. Wide characters (CJK,
  IME text) may break before or after any of them, never inside one. A paste or image chip moves like a word. A
  click right of a wrapped row's end lands on its last character, not on the next row. The caret, clicks, drags and
  selection map through the same rows, so a selection across a wrap includes the space at the break.
- **Up and Down.** They move by visual row, as in Claude Code: Up on a wrapped line's later row goes to the row
  above in the same line, and only Up on the draft's first visual row recalls history or focuses the waiting
  rows; only Down on its last visual row goes to newer history. The caret keeps its display column across a run
  of Up and Down, through shorter rows, and any other edit or move sets it afresh. A column inside a wide
  character or a chip lands on it, a column past a row's end lands on its last character (or the line's end on
  its last row), and the empty row after a hung space counts as a row.
- **Tokens.**
  - Frame: `line` (the raised value, §1), in both states below. The frame is structure; it never changes colour.
  - The input: `text`, drawn as the terminal's default foreground like the transcript's body text
    (`tui-brand.md`: `text` is the default foreground). The placeholder: `text.faint`.
  - Paste and image chips: `text` on `accent.tint` (§1: `accent` marks glyphs only on the tint).
  - The caret: a block in `text` (the cell drawn `bg` on `text`), steady, not blinking. Over the placeholder it sits
    on the first cell, so it reads as a cursor, not a stray block.
  - No padding rows and no fill: the inside of the box is the terminal's background.
- **With the keys** (the normal state): `›` in `accent`, the caret shown.
- **Without the keys** (an ask card, approval, Retry card, picker or `/config` has them): `›` in `text.faint`, the
  input in `text.muted`, the placeholder in `text.faint`, no caret; the frame is unchanged (amended by
  `docs/plans/question-vs-chat-input.md`: an empty composer's row isn't drawn). Nothing is SGR faint.
  The draft is kept and comes back in full tone when the keys return.
- **A mouse selection** inside the box takes the `accent.tint` band over the selected input cells only, never the
  frame, the padding or the prompt. Copying from the composer never includes `│`, `›` or padding.
  A press in the input starts the selection there and moves the caret; a click without a drag only moves the caret.
  A selection stays in the area where it started: a drag from the transcript into the box stops at the
  transcript's last row (and scrolls it, as at any edge), and a drag from the box into the transcript stops at the
  box's nearest input row. Copy on release and right-click copy work as for the transcript.
- **Clicks** on the frame or padding place the caret at the nearest input cell.
- **What docks against it.** Pickers (§5), the ask card and approvals sit above the bottom frame, with its top row
  between them and the box; the queued-message rows sit inside the frame, under the top row. The status row sits
  under the box, below a divider.
- **Too small for a box.** Below 21 columns, or when fewer than 5 rows are free (the thresholds at which the rules
  dropped before, `RULES_FROM` and the height rule), the frame goes and the composer is today's borderless row:
  `›` at column 0 or 1 and the input after it.
- **A short placeholder.** When the full placeholder doesn't fit the input width, it reads `Ask anything`; when even
  that doesn't fit, it ends in `…`.

### How 3B degrades

| | 3B |
|---|---|
| Truecolor | as above |
| ANSI-256 | frame 238 / 248 |
| ANSI-16 | frame in 90; `›` in 94 / 34 with the keys, 90 without; the caret in reverse video |
| `NO_COLOR` | the frame plain; the caret in reverse video; without the keys, the `›` and text lose nothing but the caret (no faint) |
| ASCII | `+-----+` edges, `\|` sides, `>` prompt |
| Ambiguous-wide | as ASCII for the frame; `›` stays |
| Reduced motion | the caret is steady in every mode, so nothing changes |
| 40 columns | a 34-column input; the placeholder shortens to `Ask anything` |
| 200 columns | full width, like the status row |
| Below 21 columns or 5 rows | no frame (above) |

**Not chosen.** 3A (a borderless `surface` band, Codex's and the brand spec's earlier composer): the owner chose
the box. 3C (today's two rules, raised): the smallest change, but the prompt still floats between them.

---

## 4. The status row: three groups (4A)

### What read badly

- The facts were one grey run, so nothing said which fact mattered.
- Key hints joined the same run in the same tone, so a hint looked like a fact and long hints pushed the facts off.
- `Ready` was in `text.faint` while every other sentence was `text.muted`.

```
| ⢎⠶⡰  Kindling…                         enter edits · del removes · tab switches · sonnet-5-5 · low · Anthropic
| ⢎⠶⠐  Ready                                sonnet-5-5 · low · Anthropic API key
| ⠢⡠⠊  Done · 18s · ↓ 475 tokens · $0.014 · cache 92%           sonnet-5-5 · low
```

### Decision

```
 ⢎⠶⡰  Kindling…      enter edits · del removes · tab switches      sonnet-5-5   low · Anthropic API key
```

- **Left:** the orb, two spaces, the status sentence in `text.muted`, `Ready` included. `Needs you` in `warn` and
  `Done` in `ok`, as now.
- **Middle:** hints, keys in `text.muted` and words in `text.faint`, joined by ` · `.
- **Right:** facts, right-aligned at `width − 1`: the model in `text`, then three spaces, then the rest in
  `text.muted` joined by ` · `.
- **Groups are separated by three spaces,** never by ` · `, so a hint never reads as a fact.
- **When the row is short,** facts drop from the right in today's order (§7.5): cache, cost, requests, credential,
  pending, effort, model. The hints stay, because a hint is what you need at that moment; only after every fact has
  gone do hints drop from their right. While something is running, the row always offers esc at every width:
  `esc to stop`, or a hint that already binds esc (`esc stops`, `esc cancels`, `esc closes`) in its place, and
  the working line then leaves `esc to stop` out of its bracket. The esc hint comes first among the hints, after
  a state note when there is one (`editing a next-turn message · esc cancels · enter saves`); when the row is
  short the hints right of it drop first, then the note, then the esc hint. Below 60 columns a busy row shows
  its sentence and the esc hint only. A notice is never cut: the esc hint steps aside until it expires, and
  every notice is at most 60 columns, action first.

| ANSI-256 | ANSI-16 | `NO_COLOR` | ASCII | Reduced motion | 40 columns | 200 columns |
|---|---|---|---|---|---|---|
| model 255 / 233, rest 248 / 240 | model in the default foreground, rest 37 / 90 | groups by spacing; the model bold | orb `(*)` | orb at rest; `Kindling…` without the shimmer | sentence, then the first hint, `esc to stop` or the model | the facts at the far edge, as chrome |

**Not chosen.** 4B (hints on their own row above the status row): clearer, but it costs a row while a hint shows.
4C (model and effort only): session cost stops being visible between turns.

---

## 5. Pickers and `/config`: a docked panel (5A)

### What read badly

- Pickers drew over the last transcript rows with no edge and cut a sentence mid-line, with no title.
- "Current" and "selected" looked alike, and both were faint.
- `/config` was close to right; only its selection vanished in light mode, and its rules were the faint `line`.

```
| ⠶ Misses now say which part of the prefix changed: the system prompt, the
|   Endpoint  credential                        next turn
|   direct    Anthropic API key                 cache rebuild ≈$0.04
|   env       Custom Gateway · gw2.example.com  cache rebuild ≈$0.04
| ● manifest  Custom Gateway · gw.example.com   current
|   work      Anthropic API key                 unavailable: WORK_KEY is not set
|   + Add an API key…
|   list prices on manifest · type to filter · edit or remove with wisp account
```

### Decision

```
 ⠶ Misses now say which part of the prefix changed: the system prompt, the tools or the messages.
────────────────────────────────────────────────────────────────────────────────────────────────────
   Endpoint  next turn rebuilds the cache
   name      credential                          next turn
   direct    Anthropic API key                   cache rebuild ≈$0.04
 › env       Custom Gateway · gw2.example.com    cache rebuild ≈$0.04
 ● manifest  Custom Gateway · gw.example.com     current
   work      Anthropic API key                   unavailable: WORK_KEY is not set
   + Add an API key…
   enter use · type to filter · esc closes
╭──────────────────────────────────────────────────────────────────────────────────────────────────╮
│ › Ask anything, or type / for commands                                                           │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
```

- **Where.** The picker docks directly above the composer, under a `line` rule across the full width. The
  transcript view ends at the rule, so no sentence is cut: the view shrinks by the panel's height and stays
  anchored at its bottom.
- **The title row** holds the picker's name (`Account`, `Model`, `Effort`, `Retry on`) in bold `text`, and after
  it only the column headers or, when the list is cut, its count.
- **Column headers** in `text.faint`; **rows** with the 1A selection and a `●` in `accent` for current; facts end
  at the content edge. **The key row** last: keys in `text.muted`, words in `text.faint`.
- **Height.** Today's picker height plus the rule and the title row. When that would leave the transcript fewer
  than 3 rows, the list scrolls inside the panel, with `↑ N more` and `↓ N more` in `text.faint` on its first and
  last rows. When space is short the list gets its rows first, up to 3 so the markers can show, then the column
  headers, then the key row, then the notes, and any rows left go back to the list. When the list still has fewer
  than 3 rows and hides choices, the title row shows the selected row's place, `1 of 8`, in `text.faint`, so a
  hidden choice always has a cue.
- **`/config`** keeps its own screen and layout, and takes the 1A selection and the raised `line`. From 40 rows
  tall, each section title but the first gets one blank row above it, but only when the spaced list fits the
  screen; otherwise the list stays unspaced, so no section is pushed off. At 120 columns the whole list fits spaced
  from 50 rows.
- **Command suggestions** (`/`) keep their place above the composer and take the 1A selection.

| ANSI-256 | ANSI-16 | `NO_COLOR` | ASCII | Reduced motion | 40 columns | 200 columns |
|---|---|---|---|---|---|---|
| rule 238 / 248, tint 236 / 252 | rule 90; the selection in reverse video | the selection in reverse video | `-` rule, `>` selected, `*` current | opens at once | columns become key/value records, as now | facts stop at the content edge |

**Not chosen.** 5B (the same panel in a rounded box): 2 more rows and 4 columns for no new information. 5C (a
centred dialog over the transcript): nothing covers the transcript, and at 40 columns it has nowhere to go.

---

## Build

Each step is one MR, in dependency order. Every MR is full-screen only, re-blesses the snapshots it names after
checking each diff is only its own change (the review checks the same), and keeps the existing contrast, glyph,
doc-sync and frame-budget tests green. Snapshot families are the file-name prefixes in
`crates/wisp-tui/tests/snapshots/chrome/` plus the `wisp-render` snapshot directories named.

**Order and parallelism.**

```
A1 tokens ──┬── A2 selection, no faint ──┬── B2 tool states, error card
            │                            ├── C  composer box ── E docked pickers
            ├── A3 edge, groups, headline┘
            ├── B1 user band, thinking fold
            └── D  status row
```

A1 goes first, because it changes colour spans in nearly every snapshot. After it, A2, A3, B1 and D can be built in
parallel. B2 needs A2 (the card's selection) and A3 (the headline). C needs A2 (no faint). E needs C, because both
change the row layout (`presenter::rows_for`). MRs built in parallel will conflict only in snapshot files; the
merge agent resolves those by re-blessing on the rebased tree, as for the idle orb.

### A1. Tokens

- **Scope.** `line` and `accent.tint` in `wisp-render/src/theme/tables.rs` (truecolor, ANSI-256), `line` → 90 in
  both ANSI-16 tables, and §2.7's correction recipe in `preview.py` and the recipe test (`theme/tests.rs`). The
  same MR updates `tui-brand.md` §2.2–§2.5's tables (doc-sync checks them against the code) and removes §2.2's
  "pending" note.
- **Re-blesses.** Colour spans only, in nearly every chrome family with a rule, frame or selection (`showcase`,
  `status`, `settings-*`, `endpoints`, `endpoint-*`, `ask-*`, `pickers`, `suggest*`, `tools-*`, `retry*`,
  `editing`, `compact`, `waiting`, `help`, `rules`, `table`, and the welcome card wherever a scene shows it) and
  the `wisp-render` `chrome`, `markdown` and `transcript` snapshots with table rules.
- **Proves it.** `doc_sync` (§2.2–§2.5 match the code); the contrast tests (every floor, both themes, the four
  common backgrounds); a new theme test that `text`, `text.muted` and `accent` on `accent.tint` meet 4.5, 4.5 and
  3.0 in both themes; a re-bless check that every changed snapshot line is a span line (`  ~ …`), never a text row.

### A2. One selection, no faint

- **Scope.** The selection recipe (band from column 0 to the content edge, `›` in `accent` at column 1, text in
  `text`, faint facts stepping up to `text.muted`) in pickers, `/config`, the ask card, the Retry card, the
  queued-message rows and command suggestions; `●` for current without a band. Remove `dim_rows` and every
  SGR 2: a composer, card or row without the keys steps down by tone (with today's rules until C lands).
- **Re-blesses.** `pickers`, `endpoints`, `endpoint-*`, `settings-*`, `ask-*`, `retry*`, `suggest*`, `editing`,
  `delivered`, `question` scenes, `showcase`.
- **Proves it.** A scan test over every scene that no cell carries `Modifier::DIM`; one test per component that the
  selected row's band covers columns 0 to the content edge with `›` at column 1 in `accent`; a test that a current
  row shows `●` with no band and a current-and-selected row shows both; light-theme snapshots in which the
  selected row is visible (`tint` 1.35:1 or more against `bg`).

### A3. Content edge, tight groups, error headline

- **Scope.** A `CONTENT_EDGE` of 120 columns for right-hand facts in the transcript, pickers and cards (`facts.rs`,
  `transcript/tool.rs`, the code block's language label); no blank rows inside a tool group; the error headline
  coloured by sentence (§2d); `Ready` and every status sentence in `text.muted`.
- **Re-blesses.** `tools-done`, `tools-live`, `showcase`, `retry`, `retry-variants`, `status`, `code`; adds
  200-column `showcase` and `tools-done` scenes.
- **Proves it.** At 200 columns, facts end at column 119 and the status row's facts at 199; at 80 and 120 columns,
  nothing moves; a tool group with output has no blank row between its rows; the headline's `error` cells are
  exactly the `⨯` and the first sentence. *Since changed: transcript facts now end at column 199 at 200 columns
  (`tui-brand.md` §4.3).*

### B1. Your message on a band, thinking folded

- **Scope.** The user's `surface` band (2a); the thinking fold, `Thought for N s · ctrl-o`, for finished, redacted
  and stopped thoughts (2b). Transcript mode keeps the full thought.
- **Re-blesses.** `showcase`, `footers`, `turn`, `thinking`, `delivered`, `editing`, `search*`, `compact`,
  `web-search-turn`, `native-search-*`.
- **Proves it.** The band spans column 0 to the content edge with one band row above and below the text (since
  changed: only behind its text rows, `tui-brand.md` §7.1), and is absent in ANSI-16 and `NO_COLOR`; copying a user
  message yields its text only; a finished thought is one row, a live one shows its last 3 lines, Ctrl-O shows it
  whole, and a thought without text has no `ctrl-o`.

### B2. Tool states in the facts, the error card

- **Scope.** The verb stays the tool's verb; `couldn't start`, `timed out after …`, `stopped` and `exit N` move to
  the facts, with states kept when the row is short (2c). The error details as one `text.muted` block, a blank row,
  and the Retry options as a card with the A2 selection and the key row (2d).
- **Re-blesses.** `tools-done`, `tools-live`, `retry`, `retry-variants`, `showcase`, `compact`.
- **Proves it.** Verbs line up for every state; at 40 columns a failed row keeps `exit 101` and shortens its
  target from the middle; the Retry card's selected row carries the band; the key row's keys are `text.muted` and
  words `text.faint`.

### C. The composer box

- **Scope.** §3: the frame from `frame_for`, the geometry (prompt at column 2, input from column 4, `width − 6`
  wide), growth to 8 input rows, with- and without-keys tones, the caret, mouse selection and clicks inside the
  frame, the no-frame fallback below 21 columns or 5 free rows, and the short placeholder.
- **Re-blesses.** Every chrome family that shows the composer (all but full-screen `/config` and `help`), and the
  `wisp-render` composer snapshots.
- **Proves it.** Each glyph set draws its frame (`╭╮╰╯─│`, or `+-|`); 1 to 8 input rows give 3 to 10 composer
  rows and the ninth line scrolls; without the keys the frame is unchanged (amended by `docs/plans/question-vs-chat-input.md`), `›` is `text.faint` and there is no
  caret; a mouse selection and a copy from the composer contain no frame glyph; a click on the frame lands on the
  nearest input cell; at 20 columns and at 4 free rows there is no frame; at 40 columns the placeholder reads
  `Ask anything`.

### D. The status row in three groups

- **Scope.** §4: sentence, hints and facts as three groups separated by three spaces; the model in `text`; hint
  keys in `text.muted` and words in `text.faint`; facts drop before hints; below 60 columns a busy row shows
  `esc to stop` only.
- **Re-blesses.** `status`, `editing`, `showcase`, `suggest*`, and the status row in every other family.
- **Proves it.** No ` · ` ever joins a hint to a fact; with a long hint the facts drop from the right in §7.5's
  order before any hint does; the model cell is `text` and the rest `text.muted`.

### E. Docked pickers

- **Scope.** §5: the rule, title row, headers, rows and key row above the composer; the view shrinking so the
  transcript ends at the rule; the scrolling list when space is short; `/config`'s section spacing from 40 rows.
- **Re-blesses.** `pickers`, `endpoints`, `endpoint-*`, `settings-*`, `retry` (the `Retry on` picker).
- **Proves it.** No transcript row is drawn between the rule and the composer; the last transcript row above the
  rule is a whole line; on a 10-row terminal the list scrolls and the transcript keeps 3 rows; every picker's title
  row has a bold name and nothing after it but its column headers or its count.

---

## Feature ideas (not part of this work)

- A theme picker in `/config` with a live preview of these tokens.
- `enter` on a tool row to expand it in place, not only ctrl-o for the latest. (Superseded by `docs/plans/thinking-cells.md`: a click on a `▸` row opens the cell in a reader, and nothing expands in place.)
- A sticky row at the top of the transcript naming the turn you've scrolled into.
- A `/status` screen for the facts that don't fit the status row.
- A one-key toggle for a dense transcript (no blank rows between blocks).
- Turn times on hover, when mouse capture is on.
