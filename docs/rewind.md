# Rewind

A rewind takes the conversation back to an earlier point: just before one
of your messages. The turns from that message on leave the conversation,
and the next request is what that turn's first request was, with your new
message in its place. Files are not restored: a rewind changes the
conversation only. The plan is `docs/plans/rewind.md`; this page describes
the kernel and the TUI as built.

## Points

- **A point** is `Turn(T)`: the moment just before turn T's message, after
  T's pre-turn work (a tail closure, a pre-turn compaction). The kernel
  marks it when it is about to write turn T's `turn_started`.
- **A turn that committed nothing** is not a point: its mark is dropped
  when the next turn starts, and it is never listed.
- **A point inside a message.** When turn T's message joined the results
  message of an open tail, the point is a length of history and a length
  of that message's content, so going back cuts the message to the blocks
  it had before T.
- **Wake and Retry turns** are points too. Their label reads
  `↻ a background process ended` or `↻ Retried turn N`; going back to one
  returns nothing to the composer.
- **A turn another agent's message opened**, such as a child's brief,
  reads `↻ a message from main` or `↻ a message from /lead`, the sender
  named as `shown_agent` names it.

Each point keeps, in memory: the history and last-message lengths, the
compaction head in force, the `<environment>` flag, the tools' working
directory, which process ends the model had heard, the search cuts, the
closed search tails and a failed search pause the turn continued, the
recent tool calls a compaction summary lists, the epoch's byte state, and
the cache baseline (below).
Nothing is copied: the inputs are read back from history when you rewind.

## Commands and events

- `RewindPoints { request }` answers `RewindPoints { answers, points }`,
  oldest first. Each `PointInfo` has the `point`, a `label` (the first
  non-blank line of the turn's first message, `[image]` for an image
  alone), `at`, `inputs_preview` (the first line of each input), a
  `quote`, a `compaction` note when a compaction ran after the point
  (`turn` names the turn it ran before, and `compacts_first` says the
  restored conversation is already past the compact point), and
  `processes`: the background processes started after the point that
  still run.
- `Rewind { to, request }` answers `Rewound { answers, point, reason,
  dropped, returned, cache }` or `Rejected { kind, request, reason }`.
  `returned` holds the turn's opening inputs, then any held messages, each
  `{ id, text, images, missing }`; `missing` lists each image the kernel
  could no longer read back from history as `{ ordinal, sha256 }`, where
  `ordinal` is its 1-based place among the input's images (the `#n` of
  `image #n`), and `images` keeps the others in order. `cache` is what the next
  request will do to the cache: `kept` for a warm point, otherwise
  `rebuilt` with the reason the quote gave.

An agent rewinds the same way: `RewindPoints` and `Rewind` sent in
`Child { agent, command }` reach it (`docs/agents.md`, "Commands and
events"), and its answers come back wrapped.

Refusals:

| Kind | When | Reason |
|---|---|---|
| `turn_running` | A turn is running | `/rewind works between turns. Stop the turn first.` |
| `compacting` | A compaction is running when the rewind is judged | `/rewind waits for the compaction. Send it again when it ends.` |
| `not_rewindable` | No point yet | `there is nothing earlier to go back to` |
| `not_rewindable` | The turn isn't a point | `turn N isn't a message you can go back to` |
| `not_rewindable` | The turn was left by an earlier rewind | `turn N was rewound away` |
| `not_rewindable` | A message was sent, or held messages released, before the verdict | `a message was sent before the rewind` |
| `not_rewindable` | Esc while the rewind waited | `the rewind was cancelled before it ran` |
| `not_rewindable` | The session ended first | `the session ended` |

## When a rewind runs

`RewindPoints` and `Rewind` go through the command queue, so the points
are quoted with any settings change sent before them applied. A `Rewind`
waits the same way, so a settings change sent before it applies first: it waits behind a settings save still in
flight, and a message sent or released, or Esc, while it waits cancels it.
At the head it is judged at once: refused while a turn or a compaction
runs, and run otherwise.

## What goes back and what stays

- **Goes back:** the history (truncated to the point's lengths), the
  compaction view in force at the point (a compaction after the point is
  undone; epoch numbers are never reused), whether the `<environment>`
  block was sent (so a rewind to turn 1 sends it again), the working
  directory (a `cd` in the turns you left is forgotten), the search cuts
  (a tools-change cut made after the point stays, clamped to the kept
  history), the closed search tails, and which process ends the model
  heard. A Retry turn that continued a failed search pause gets that pause
  back, so the next message closes it as one sent after the failure would.
  The recent tool calls a compaction summary lists go back to the point's,
  including calls the list's cap had dropped since.
- **Stays:** your settings (model, effort, endpoint, cache lifetime, and
  any pending change), the endpoints' learned states (including the
  rendering form for closed server calls), the turn counter
  (the next turn after a rewind in an 18-turn session is turn 19), the
  session's cost, and running processes.
- **Held messages** are withdrawn with `input_withdrawn { by: rewound }`
  before the record, and returned after the point's own inputs.
- **Steers** committed inside the restored turn are not returned.
- **A Retry offer lapses:** the failed request is no longer the
  conversation's.
- **Background processes** started in the turns you left keep running. The
  next turn's `<processes>` block tells the model about each one still
  running (`it was started in turns the user rewound`), and an end it
  heard only in the turns you left is reported again.

## The cache

- **The baseline** of `Turn(T)` is the last accepted request before the
  point, so a rewind compares the next request's reads with what that
  request cached. A refused request is never a baseline; pre-turn work
  that changed bytes before T's message (a compaction, a closure's cut)
  leaves the point without one.
- **Warmth,** in order: no baseline is `prefix_changed`; then the settings
  the next turn runs with are compared with the baseline's through the
  same rule a settings change uses (`other_endpoint`, `other_model`,
  `other_effort`, or `may_have_expired` when per-message effort isn't yet
  proven); then any later change to the bytes at or before the point (a
  cut inside it, another rendering form, other system or tools bytes) is
  `prefix_changed`; then a point is `warm` until the latest request that
  sent its prefix unchanged plus that request's cache lifetime, and
  `may_have_expired` after.
- **The quote** is `prefix_tokens` (the baseline's prompt tokens) priced
  at the next turn's model as a cache read (`read_micros`) and as a write
  at the session's cache lifetime (`rewrite_micros`), both absent when the
  endpoint or model has no prices.
- **After the rewind,** a warm point's baseline becomes the cache check's
  last request; any other point expects a rebuild (the `rewound` record's
  `cache` says which, and why, so `--resume` agrees), and the next turn's
  footer names it (`cache rebuilt: conversation rewound`, or the settings
  reason). Until the next accepted request or a compaction, the next point
  takes the rewound point's baseline, warm or not, so its quote compares
  with what that point cached; `--resume` rebuilds this from the log.

## The log

A rewind appends one `rewound` record (`docs/session-log.md`); records are
never removed or edited. The rebuild masks the records the rewind left and
checks the kept history against the record's `messages` and `blocks`.
Masking needs record numbers, so only the record-based rebuild
(`rebuild_checked`, used by `--resume`) honours rewinds; the event-only
`rebuild` fails on a log that has one (`a rewind needs record numbers`).
A damaged `rewound` record (one failing its check, keeping a record that
isn't before it, or overlapping an earlier rewind's range) fails the
rebuild with `rewind_at`, the record's `seq`. `--resume` reads that failure
as `malformed_history` damage at the record and falls back to the last turn
that ended cleanly before the rewind's point (`docs/session-log.md`,
`resume_fell_back`). With `--no-log` nothing is written and
the marks are enough.

## Resume

`--resume` rebuilds the conversation as it stands after the last rewind,
rebuilds the points of the kept turns with their baselines, and remembers
the turns earlier rewinds left, so a rewind to one of them is refused as
in the live session. The first request after a resume is the one the live
session would have sent.

When the log is damaged, resume falls back to the last turn that ended
cleanly (`docs/session-log.md`, `resume_fell_back`), and the fallback
respects rewinds. A kept `rewound` keeps its masking. A fallback point after a
rewind's `to` whose `rewound` would be dropped moves back to that `to`, so
rewound turns never return. A damaged `rewound` is damage at its own record.
A restart from a compaction head never uses a head a rewind masked. Messages
of rewound turns are never put back in the composer, and the notice counts
only the dropped turns that were live.

The resumed transcript replays each `rewound` record as the same `Rewound`
event the live session sent, so a client draws the same cut and divider. A
replayed rewind answers no request and returns no input: the composer is
never refilled from the log. Its cache effect is the record's `cache`,
the one the live event carried, so the next turn's `cache rebuilt:` note
names the same reason as live.

## In the TUI

- **Opening.** Esc on an empty composer while nothing runs shows the key
  hint `esc again to rewind` in the status row for one second, between
  the status sentence and the facts (`tui-brand.md` §7.5); a second Esc in
  that second opens the picker. Any other key cancels it. Esc while a turn
  runs stops the turn and never primes, and Esc while scrolled or with a
  selection keeps its own meaning. `/rewind` (or `/undo`) opens the same
  picker; while a turn or a compaction runs it is refused with `/rewind
  runs between turns. Press esc to stop this one.` or `/rewind waits for
  the compaction. Send it after.` (60 columns at most, like every status
  notice), and the command stays in the composer.
- **The picker** docks above the composer like every other picker
  (`tui-review.md` §E): a rule, the title row `Rewind`, the column
  header `turn  message  cache`, the
  rows, and its key row `enter choose · esc close`; the transcript ends
  whole at the rule. It asks the kernel for the points (`rewind_points`)
  and shows `loading…`, with the key row `esc close` alone, until they
  arrive. A list taller than the dock
  scrolls inside it with `↑ N more` and `↓ N more`. Each row is the turn, the time its message was sent
  (in the local time zone, with the date when it isn't today, such as
  `Oct 25 14:02`), the message's first line, and the
  quote: what the cache holds and the estimated cost of the next message,
  for example `cache warm ~$0.04`. The newest point is selected. A narrow
  terminal first drops the time when the time costs room, then shortens the
  quote to its figure. The selected point's marks show below the rows:
  `before the compaction at turn 30` and `2 processes started after this
  point`. Up and Down move, Home and End jump, Enter opens the confirm
  panel, Esc closes. A click selects a row, and a click on the selected row
  confirms it.
- **The confirm panel** takes the dock's place, under the same rule, and
  names the point, its message and `files stay as they
  are`, then what the next message reads or writes and its cost, the
  compaction note, the processes that keep running, and the key row
  `enter rewind · esc back`. On a short terminal it keeps its first row
  and the key row. Enter sends `rewind`, and the status row says `rewinding…` until the answer; Esc goes
  back to the picker.
- **After a rewind** the transcript drops every cell from the point on and
  draws a divider: `↶ rewound to before turn 11 · turns 11–18 set aside ·
  files unchanged`. A resumed view replays the log's turns with their
  numbers, so a rewind after `--resume` cuts the same cells as live, and
  each `Rewound` the log replays draws the same cut and divider. A replayed
  rewind never refills the composer, shows no image line and leaves Esc
  Esc unarmed. A point with no cell of its own cuts from the first later
  turn the TUI has. The
  composer gets the rewound turn's messages, then any
  held ones, ahead of a draft. Paste chips and images come back as chips when
  this TUI still holds the message's parts (up to the last 200 messages);
  otherwise the text comes back as the kernel returns it, with images as
  image chips. An image the provider refused (`attachments_withdrawn`)
  doesn't come back, from either copy, and a line above the composer, with
  the waiting messages and hidden while a picker docks there, names it by
  its number in the message: `image #2 wasn't restored: it couldn't be
  processed`, or `images #1, #2 and #3 weren't restored: they couldn't be
  processed`. The line shows even when the image was the whole message and
  the composer comes back empty, and stays until you send, clear with
  ctrl-c, or rewind again.
- **A refusal** (another message was sent first, the point is gone) clears
  `rewinding…` and adds a note with the kernel's reason, `Not rewound: …`;
  the transcript is unchanged. For `turn_running` the note gives the TUI's
  own text instead: `/rewind runs between turns. Press esc to stop this one.`
- **In an agent's chat** (`docs/agents.md`, "In the TUI") `/rewind` takes
  that agent back: the picker lists its own turns, priced on its cache,
  and a turn another agent's message opened, such as its brief, reads
  `↻ a message from /lead` (`↻ a message from main` for the main agent's). The
  panel, the cut and the divider are drawn in its chat, and the rewound
  turn's messages come back into its box; the main chat's box is
  untouched. Mail never comes back, so a rewind to before its brief leaves
  the box empty, and the agent waits for your message, with any report it
  owes still due. Esc Esc doesn't open the picker there, since esc goes
  back. While the agent works `/rewind` stays in the box and the status
  row says `/rewind runs between turns. Send it after this one.`, the text
  a `turn_running` refusal's note gives there too. An image that isn't
  restored is named by a note in the agent's transcript.
- **Replay.** A `rewind_points` or `rewound` event outside a turn in a
  replayed log answers the TUI's next request of that kind, so a recorded
  session with a rewind plays back with its picker and divider.
