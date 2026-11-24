# Commands

The full-screen terminal UI reads its slash commands from one registry, `wisp_tui::app::commands`. Dispatch reads the same table, and a test keeps this page in step with it: every name and alias in the registry must appear here.

In an agent's chat (`docs/agents.md`, "In the TUI") a command works as in the main chat, by its row's `chat` fact: those that act on the agent (`/model`, `/effort`, `/account`, `/context`, `/compact`, `/rewind`) and every plugin command run on that agent, for this session only, and their pickers, reports and notes are the agent's own, in its chat; `/account add`, a picker's `+ Sign in to another account…` and an account that needs a sign-in go back to the main chat and sign in there; `/help` and `/ps` open over the chat; Wisp's other commands (`/agents`, `/sidebar`, `/usage`, `/login`, `/logout`, `/config`, `/exit`, `/leave`, `/resume`) go back to the main chat and run there.

## The commands

In the order the registry lists them, most used first.

| Command | Arguments | What it does | While Wisp works |
|---|---|---|---|
| `/model` | `[id]`, a model ID | Switch the model. With no ID it opens the model picker with the chat's account open and every other account as one row (`docs/endpoints.md`, "Switching mid-session"). Antigravity's level-in-ID models share one picker row per family; an ID this account offers switches at once, without an effort step for a level-in-ID model; any other opens the picker filtered to it | Runs; applies from the next turn |
| `/effort` | `[level]`: `low`, `medium`, `high`, `xhigh` or `max` | Set how hard the model thinks. With no level it opens the effort picker; on Antigravity's level-in-ID models, a level switches to that level's ID | Runs; applies from the next turn |
| `/account` | `[name]`, an account name, or `add` | Switch accounts, or add one. With no name it opens the account picker; `add` opens the card that adds one (`docs/endpoints.md`). `/endpoint` is a hidden alias: it works the same, but help and suggestions show only `/account` | Runs; applies from the next turn; `add` saves through the kernel's settings queue |
| `/context` | `[all]` | Show what fills the context. `all` lists each item. `/context count` gives a hint: counting is automatic now (`context.count_tokens`) | Runs |
| `/compact` | `[focus]`, free text | Summarize the conversation to free up context (`docs/context.md`, "Compaction"). The focus steers the summary | Between turns only: refused with `/compact runs between turns. Send it after this one.`, and with `A compaction is already running.` while one runs |
| `/rewind`, `/undo` | none | Go back to an earlier message: opens the rewind picker, the same as Esc Esc on an empty composer, which in an agent's chat goes back instead (`docs/rewind.md`, "In the TUI") | Between turns only: refused with `/rewind runs between turns. Press esc to stop this one.` (in an agent's chat, `Send it after this one.`), and with `/rewind waits for the compaction. Send it after.` while one runs |
| `/config` | `[filter]`, a filter or a setting name | View and change settings on the settings screen. A word that is a setting's name opens the screen on it; any other word opens it filtered | Runs |
| `/ps` | none | Show the session's background processes and terminals, every agent's included, the same list in every chat: Running comes first, History second, both with a count even at zero. Within each group, agents' programs are listed under their names when any child has a program, main's first, then in `/agents`' order. Each program is one line: its description, then its command in softer grey, then its state; without a description the command stands alone. Enter opens one (a running terminal's live screen, otherwise the reader page of the call that started it, such as a pipe's or a number restored on resume, back in the main chat; nothing for a process with no call or for an agent's pipe, and `enter open` shows only on a row that opens), `a` takes over a running terminal, `x` twice stops a running program; `a` and `x` do nothing in History, esc closes; each key acts through the agent that started the program, which is told as main is. Outside its own agent's chat a program is named with its agent (`4 of /lead/coder`, `3 of main`). The focused program takes the selection band and stays selected when it finishes. A terminal waiting for a password reads `waiting for a password · a take over` (`docs/tools.md`, "Typing into a terminal yourself") | Runs |
| `/agents` | none | Show the Agents page, as Ctrl-T does: agent-viz's topology of main and every live agent, a picked dot, and two panel rows for it (name, model and effort, state and clock, cost, context and checklist as the width allows; its account and billing, a changed preset and the last mail it got). At 60 columns and up, a list on the left names every live agent under its parent and takes at most a third of the screen. ↑↓ walk the list when it shows; the other arrows move between dots. Enter or a click on a row or dot opens that agent's chat, where you talk to it as in the main chat and esc goes back to the main chat, and `r` releases your messages held at it; only its parent stops or ends it; on main this closes the page. With agent-viz off the list stays, and ↑↓ still step through main and the live agents. Esc closes it, ctrl-c closes it and never stops main's turn, ctrl-d does nothing (`docs/agents.md`, "In the TUI") | Runs |
| `/sidebar` | `[open]` or a section | Hide or show the sidebar that plugins' sections fill (`docs/design/tui-brand.md`, §7.8). It does the first of these that applies: before the session's plugins are known (until the first message starts the log), it says `The sidebar's sections aren't known yet`; with no section on, it says `No sidebar sections are on · /config` and changes nothing; narrower than 120 columns, it opens the overlay; when `WISP_SIDEBAR` or `--set display.sidebar` decides the setting, it opens the overlay instead and says which (for example `WISP_SIDEBAR=hidden sets it`), saving nothing; docked, it hides the sidebar, sets and saves `display.sidebar = hidden` and gives the transcript its width back; wide and not docked, it docks the sidebar at once, even before any section has something to show (`Nothing to show yet`), and sets and saves `display.sidebar = auto`. `/sidebar open` opens the overlay at any width and changes no setting (below). `/sidebar <section>`, such as `/sidebar context/window`, opens that section's page (below); `/sidebar agent-viz/topology` opens `/agents` instead | Runs |
| `/help` | none | Show commands and keys on the help screen | Runs |
| `/exit`, `/quit` | none | Quit at once, ending the chat and showing `Shutting down, please wait…` (with any saving/applying settings, stopping agents or stopping processes detail) until exit | Runs |
| `/leave` | none | Leave; the chat stays open. Prints the command that brings it back. If this chat lives in its window, says why and leaves it open here | Runs |
| `/resume` | `[filter]`, free text | Pick another chat in this directory to continue: opens the chat picker over the bottom, without this chat, filtered by the text, its list at most 12 rows tall. Enter leaves a hosted chat running and opens the picked one, as `wisp --resume ID` would; a chat that lives in its window ends when switched away from (`docs/session-log.md`, "Picking a chat") | Between turns only: refused with `/resume runs between turns.` |

Words after `/ps`, `/agents`, `/help`, `/rewind`, `/leave` or `/exit` are ignored. More than one word after `/sidebar` gives the hint `/sidebar takes open, a section, or nothing`.

### Plugin commands

Enabled plugins add their own commands (`docs/plugins.md`, "Plugin commands"). The TUI reads them from the catalog, once at the start and again on every `catalog_changed` and every settings snapshot (which carries the plugins too), keeping only the commands of plugins that are installed and enabled, so turning a plugin off removes its commands from the list, the help screen and parsing.

- **Names.** Every plugin command answers to `/<plugin>:<name>` and `/<plugin>:<alias>`, such as `/testkit:go`. It also answers to the short `/<name>` and `/<alias>` when the catalog offers its short form, which it does only when no built-in and no other plugin claims any of its names. A built-in name always wins: a plugin's `/model` is only `/<plugin>:model`. Whichever form you type, the TUI sends the kernel the full name, with the name or alias you typed: `/g fast` sends `testkit:g` with the argument `fast`.
- **While Wisp works.** A command that runs only between turns is refused while a turn runs or messages wait to start one, with its plugin's hint as the status hint, and stays in the composer, as `/compact` does. Any other plugin command runs at once, even mid-turn: it runs beside the turn and holds nothing up.
- **Stopping it.** While a plugin command you sent has no answer yet, esc and ctrl-c stop it, even between turns, as they stop a turn: the TUI sends `cancel`, which stops the running command and every waiting one. Nothing else changes while one runs: tab, ctrl-d and enter on an empty message do what they do between turns.
- **Its lines.** A command that has to wait shows one muted line, `/testkit:go queued until the call ends` (or `the hook finishes`, or `the command before it ends`); if it waits again for something else the same line changes. When it ends, that line becomes its answer while it is still the transcript's last line; once anything has been drawn below it, the waiting line goes and the answer is drawn at the end, as it is for a command that never waited. The answer shows at once, mid-turn too: the plugin's note as a note (`∙ 3 files changed`), `/testkit:go failed: …`, `/testkit:go timed out`, `/testkit:go cancelled`, or `/testkit:go: …` with the kernel's or the plugin's reason when it is refused. An answer with nothing to show (no reply, or a message sent) removes the waiting line.
- **An answer with rows** can keep a count at the right edge, use coloured states and bold text, and wrap longer sentences. Rows cut long left-hand text rather than moving their count; a wrapped sentence keeps its leading indentation on later rows. A blank line stays blank. `/skills` uses this answer for its title, count and two rows per skill; it has no note bullet. Unicode glyphs fold to ASCII when needed, including `—` to `-`.
- **A reply sent as a message** shows as a waiting message, placed where the kernel placed it: among the steers or the queued messages by when its command was sent, so after messages you sent before the command and before those you sent after it, never ahead of them all.
- **Text from a plugin** (names, aliases, summaries, argument names and values, hints, notes, answer spans and failures) is drawn with control and bidirectional characters made visible, as `<U+202E>` or `^[`; zero-width spaces (U+200B, U+FEFF) are removed.
- **A replayed session** (`--replay`) has no plugins, so no plugin command is listed, and a full name is an unknown command.

### The sidebar overlay

`/sidebar open`, or `/sidebar` on a terminal narrower than 120 columns, opens the sidebar as an overlay from the right edge, with the band's width (the whole width below 43 columns, where the boxes' left side is column 0 and the `│` rule shows only beside `Nothing to show yet`). It covers the transcript's rows on the surface colour, with the `│` rule (that colour, like a `/ps` screen's cell backgrounds, fills only where the terminal reports its background), and the transcript doesn't re-wrap; over a docked panel it takes the band's own columns, so the panel simply gains the keys. It draws what the docked panel draws, at the overlay's height and with the same scroll position, and takes the keys while open; the status row reads `↑↓ scroll · esc close`.

| Key | What it does |
|---|---|
| ↑, ↓, pgup, pgdn, home, end, the wheel | Scroll it |
| esc, q, ctrl-c | Close it |
| ctrl-t | Close it and open `/agents` |

`/sidebar` can't be typed, and nothing can be pasted, while the overlay has the keys. A question card, a step-in card or a Retry choice that appears closes it too, and widening the terminal to 120 columns or more closes an overlay opened below 120 (the panel then docks if the docking rule holds). A turn keeps streaming underneath. A click or drag inside its columns selects nothing and opens nothing, except a click on a section's title or body (below).

### Folding a sidebar section

A click on a section's title, in the docked panel or the overlay, folds the section to its title row (`▸` after the title) or opens it again; a click that slips by one column between the press and the release still counts. The click saves `display.sidebar_folded` at once and quietly: it adds no note to the transcript, and if the change is refused, the section goes back as it was and the reason shows as a hint. A click that refocuses the terminal, the first click while a transcript selection shows, a drag of two or more columns, and a press while a picker, card or suggestion list is open fold nothing. From the keyboard, `/config` folds sections (`display.sidebar_folded`) and pins them to the top of the sidebar (`display.sidebar_pinned`).

### A section's page

Any sidebar section expands to a full-screen page, except agent-viz's Agents box (`agent-viz/topology`), which opens `/agents` instead, closing the overlay. Two things open it:

- **A click on a section's body**, in the docked panel or the overlay. The body is the rows its content draws, its `couldn't update` row and a pinned section's `… n more`. The `↑ n more` and `↓ n more` rows, the boxes' edges and `… k more sections` belong to no section, and a click on them opens nothing. A click on a title still only folds. The click follows the fold click's rules: a click that refocuses the terminal, the first click while a selection shows, a drag of two or more columns, and a press while a picker, card or suggestion list is open open nothing.
- **`/sidebar <section>`**, with the section's full name. The list after `/sidebar ` offers `open` and every section that is on, by name, with its title. A section that is off says `<name> is off · /config`; a name that isn't a section says `No sidebar section named <name>` when it has the `<plugin>/<name>` form, and `No sidebar section by that name` otherwise, so a pasted secret is never echoed. Nothing opens then.

The page is a pager page like the reader: the section's title, with its note on the right, stays on the top row above a rule, and the key row reads `/ search`, the position, the waiting count, `ctrl-c to stop` while a turn runs, and `q esc back`. It scrolls, searches, selects and copies with the reader's keys (the table under "The reader"); ctrl-o, `s` and `[` do nothing here, and `G` and end jump to the last row but never follow, since a page has no output to follow. New content and the clock keep the row you are reading at the top. `q` or esc closes it, back to the docked panel, or to the overlay when it was opened from there; after a resize across 120 columns it closes to what the sidebar's rules give at the new width. It closes on its own when its section is switched off or its plugin is disabled or removed. Opening, scrolling and closing a page change no setting and add nothing to the log.

## What counts as a command

A line is a command only when all of these hold:

- it starts with `/`, with nothing before it;
- its first word is `/` and a name: an ASCII letter, then letters, digits and `-`, or a plugin's name, `:` and a name with nothing between them, such as `/testkit:go`. Names match in any case;
- it is one line;
- it is typed text, with no image and no paste chip.

Everything after the first word, trimmed, is the command's argument.

Any other line is a message. So `  /help` (with a leading space), `/--`, `/2x`, `/etc/hosts is wrong`, `/a:b:c`, `/:b`, `/todo: buy milk` and a message whose second line follows `/model x` are all sent to the model as typed.

A line whose first word is a name that isn't a command, such as `/hepl`, `/tmp is full` or `/nosuch:cmd`, is not sent. It stays in the composer with a hint: `/hepl isn't a command. Start with a space to send it.`

A command that runs is never sent to the model or logged; it enters the composer's history, so Up recalls it. A refused line, unknown or not runnable while Wisp works, stays in the composer and enters no history.

## Suggestions as you type

Typing `/` as the first character of a message opens a list of commands above the composer, over the bottom of the transcript, so the transcript never moves. It lists up to 8 commands, each with its arguments and what it does, and filters as you type: a command shows when its name or an alias starts with what follows the `/`, in any case. An exact name comes first; the rest keep the registry's order, so the same letters always pick the same command. The letters that matched are bold. When nothing matches, one row says so: `no command matches /fo`.

The list follows the same rules as a command (above): it shows only while the message is one typed line that starts with `/` and a name, with no image or paste chip, and the cursor at the end. It opens when you edit the message, not when Up recalls an earlier one, and not while a picker, a question, the help screen or transcript mode holds the keys. A question that opens hides it; your next edit after the question closes brings it back. After a command's name and a space it lists that command's values instead (below). After an unknown name and a space it keeps saying `no command matches /hepl`, and tab does nothing.

While the list shows:

| Key | What it does |
|---|---|
| up, down, ctrl-p, ctrl-n | Move the highlight, wrapping |
| tab | Complete the highlighted command, adding a space when it takes arguments. With no match it does nothing |
| enter | Run the highlighted command, exactly as if its name had been typed: `/mo` then enter opens the model picker. With no match, enter acts on the line as typed |
| esc | Close the list until the word changes. It doesn't stop a running turn; a second esc does |
| a click on a row | Complete it, as tab does. It never runs a command |

Every other key goes to the composer as usual. The first row is highlighted whenever something matches, so `/` then enter runs `/model`. Completion only edits the message, and enter then runs it through the same path as a typed command: the same command, history entry and hint.

The list works while Wisp works, and its keys come first: tab completes rather than saving the message for the next turn, esc closes the list rather than stopping, and up moves the highlight rather than going to the waiting messages, which hide while the list shows. A command that runs only between turns is dimmed, labelled `between turns`, and refused with its hint.

Plugin commands are listed with the built-ins: exact matches first, then prefix matches, and within each, the built-ins before the plugin commands, which keep catalog order. A row shows the short forms when the catalog offers them, and the full `/<plugin>:<name>` forms when it doesn't or when what you typed matches a full form, so `/testkit:` lists each of testkit's commands by its full name. Tab and enter complete the command's name in the form the row shows. After a plugin command's name and a space, its first argument's values are listed as a closed list (as `/context`'s words are), or, when it has no fixed values, one row names it, as for `/compact`. The status row shows `esc closes · tab completes · enter runs` while the list is open.

### Argument values

After `/model `, `/effort `, `/account `, `/context `, `/sidebar ` or `/config `, the list shows values for the word you are typing. Only the first word after the command is suggested; a second word closes the list.

| Command | Rows | What a row inserts |
|---|---|---|
| `/model` | The model picker's rows, grouped by account, with the word as its filter on model and account names: price, cache effect and dimmed unsupported models. A word that matches no model gets the picker's free-text row | The model ID the row stands for, including when its label is an Antigravity family |
| `/account` | The account picker's rows, with the word as its filter: the current one marked, its kind, the model and effort the chat would run there, and unavailable ones dimmed | The name |
| `/effort` | The effort picker's rows for the current model, every level in order: the current one marked, the default labelled, the cache effect of each. On a model with no effort levels, one row says so | The level |
| `/context` | Each word the registry lists for `/context`, with what it does | The word |
| `/sidebar` | Each word the registry lists for `/sidebar`, with what it does, then every section that is on, by its full name, with its title | The word or the name |
| `/config` | The name of each setting on the settings screen, in its order, with its label. A name shows when it starts with the word or its label contains it, in any case | The name |

After `/compact `, one row reads `focus (optional) · enter run`: the focus is free text, so nothing is suggested.

Models, endpoints and setting names are open lists: they match anywhere in the name, in any case, as their pickers filter. Effort levels and words are closed lists: every value shows, and a value matches when it starts with the word, in any case. The rows come from the pickers' own code, so a price or a cache effect never differs between the picker and the list. Up to 12 rows show, with the picker's header row.

A value is highlighted only when you chose it (up, down, tab or a click), when the word is exactly that value or exactly the name its row shows (so `sonnet-5-5` takes the row `claude-sonnet-5-5`, unless two rows show that name), or, in a closed list, when it is the only value that matches. Exact means the same case: `/model CLAUDE-OPUS-5-5` is listed but runs as typed. Nothing else is guessed:

| Key | What it does |
|---|---|
| up, down, ctrl-p, ctrl-n | Move the highlight, wrapping and skipping dimmed rows. With nothing highlighted, down starts at the first match and up at the last, or at the first and last rows when nothing matches |
| tab | Replace the word with the highlighted value, or with the first value that matches when nothing is highlighted. When nothing matches, it does nothing |
| enter | With a value highlighted, replace the word with it and run the line. Otherwise run the line exactly as typed |
| esc | Close the list until the line changes |
| a click on a row | Replace the word with it, as tab does. It never runs the command |

So `/effort x` then enter sets `xhigh`, the only level that starts with `x`. `/effort m` (medium or max) then enter runs as typed and gives the ``Use low, medium, high, xhigh or max, not `m`.`` hint. `/model sonnet` then enter switches to the ID `sonnet` as typed, never to the first model whose name contains it; down, then enter, takes the highlighted model. `/model ` or `/context ` with no word runs as typed, which opens the picker or shows the summary. The status row shows `esc close · enter run` while values are listed.

### Mouse and small terminals

A click or drag elsewhere in the transcript closes the list and then selects as usual, or opens a `▸` row or a tool row in the reader ("Folded rows, the ctrl-o picker and the reader"). A click in the composer moves the cursor, which closes the list unless the cursor stays at the end. The wheel scrolls the transcript.

The list needs 2 rows above the composer; with fewer it doesn't show, and the typed line still works. Between 2 and 8 rows it shows that many and scrolls to keep the highlight in view. Below 60 columns the descriptions drop, and at 30 columns or fewer the arguments drop too.

## The help screen

`/help`, or `?` on an empty message, opens a full-screen page in the same frame as transcript mode (ctrl-o, then ctrl-o again once anything can be opened). `/help` opens it at the top; `?` opens it at the first keys section. To start a message with `?`, type a space first. `?` while editing a waiting message, or with text, an image or a paste chip in the composer, is typed as a character.

The page lists the commands, then, when any enabled plugin has commands, a "Plugins" group with each one's names (the short forms when offered, the full ones otherwise), arguments and summary, then the keys in six sections: writing, while Wisp works, waiting messages, reading, transcript, and anywhere. From 120 columns the sections flow into two columns, split between sections so the columns are as even as they can be; below that they are one column, and below 60 columns each row takes two lines. The rows match the terminal: the newline keys it sends, the selection key for mouse capture, the right-click row, and arrows for the glyph set.

The commands come from the registry. The keys come from one table, `wisp_tui::app::help::KEYS`, whose rows name the keymap actions they document; a test fails when an action has no row. Adding a key is one row in that table.

On the help screen:

| Key | What it does |
|---|---|
| up, down, `j`, `k` | Scroll a line |
| pgup, pgdn, space, `b`, `f` | Scroll a page |
| `u`, `d` | Scroll half a page |
| home, end, `g`, `G` | Jump to the top or the bottom |
| `/`, `n`, `N` | Search ("Search in the reader, transcript mode and help") |
| the wheel | Scroll |
| drag, then ctrl-c | Select, then copy |
| right-click | Copy the selection |
| esc, `q`, `?`, ctrl-o | Back to where you were, with the scroll position and the composer kept |
| ctrl-c | Stop a running turn; when idle, back |

`[` does nothing here, and `?` closes the page rather than searching back. The key row starts with `/ search` and ends `q esc ctrl-o back`. With nothing selected, right-click does nothing here. A turn keeps running behind the page, and its events wait in the transcript. A question that waits on you closes the page. Nothing is added to the transcript.

## Folded rows, the ctrl-o picker and the reader

A row that hides something ends in `▸` (`>` with ASCII glyphs, in `text.faint`), and no cell names a key:

- **A live thought** streams whole under `╎ Thinking` (with its note facts, and no `▸`, since nothing is hidden); the view follows the bottom as usual.
- **A finished thought** folds when its block ends, or when the turn ends mid-thought: its label `╎ Thought for 4 s ▸` (`╎ Thought ▸` when replayed from a resumed log, which has no duration), then the last 3 text rows of the thought, wrapped as they streamed. Paragraph-break rows are skipped and never counted, so the fold never shows a bare `╎` row. The rows above fold away, so the label and what came before it move down into view once. Two thoughts in one turn fold separately.
- **A thought that fits** (3 text rows or fewer) shows whole with no `▸`, and a thought with no text stays `╎ Thought for 4 s` with no `▸`.
- **A note from an agent** (`docs/agents.md`, "Mail rows") is one grey row, `◇ note from /x · its first line`, cut with `…` and ending in `▸` when it hides something, whole with no `▸` when its one line fits; the whole row opens it. A long sender path is cut instead when needed to show the first four columns of the words, including `API:`.
- **Tool output** no longer folds: a tool call is one row with nothing under it, and the whole row opens (below). Only a content cell's view keeps a fold: a diff longer than 30 lines ends in `… N more lines ▸` (`docs/tools.md`, "The diff cell").

The `▸` keeps its place at any width; the label shortens with `…` before it. It sits outside the row's copy range, so a copy of the row never contains it. Each folded cell has exactly one such row.

**Opening.** A click on a `▸` row opens that cell alone in the reader. The press records the row's cell, and the release opens it unless the pointer moved two or more columns from the press, or off its row, or the wheel turned in between; a click that slips by one column still opens it, even when streaming moved the row while the button was down. A drag that starts there selects as usual, a click on the cell's other rows never opens it, and a right-click on it copies or pastes as it does everywhere. Opening clears the click history, so the second press of a double-click lands in the reader as a fresh click. With a picker open, a click outside it only closes it. Shift-drag (Option-drag on macOS) still gives the terminal's own selection under mouse capture; with capture off (`display.mouse = off`, `WISP_MOUSE=0`) the rows are inert and the ctrl-o picker reaches every cell.

**The ctrl-o picker.** ctrl-o in the main view docks a list of everything you can open above the composer, like `/model`'s:

```
────────────────────────────────────────────────────────────────────────────────
   Open
   Full transcript    every cell in full
 › ✓ Showed crates/wisp-core/src/cache.rs                                 +40 −0
   ⨯ Ran cargo build --release                                     exit 101 · 3s
   ✓ Ran cargo test -p wisp-core --all-features                      exit 0 · 1s
   ╎ Thought for 4 s  Placeholder 9.
   enter open · type to filter · ctrl-o transcript · esc close
```

- **Entries:** `Full transcript` first, then every cell the reader can show, newest first: a thought with text, and every tool call in every state, from `Writing a command` to refused. A thought is its mark and label, then its last line, aligned with `Full transcript`'s text and cut with `…` at the edge.
- **A tool call's entry** is its main-view row (`docs/tools.md`, "Tool rows in the TUI"), built by the same function at the dock's width after the marker, so its facts end at the dock's right edge and a short entry drops counts, then the command, then cuts the title, keeping the state as the row does. A content cell's entry is its row alone, without its diff. A running or writing call's entry ticks with its clock. A call moved to the background is one entry at its call's place, `⇗ … process 27 · running`, then its process's outcome, or `before the restart` for one restored on resume; its process gets no entry of its own. Entries come from cell facts, never from bodies, and every entry is rebuilt on every frame.
- **A tool call's filter** holds the title, the whole command (every line of a multi-line one), the uncut program a process call names, the facts (`exit 101`, `queued`, `process 27 · running`), `failed` for a failed call, and a content cell's file paths. Typed text enters only through a title that shows it (`docs/tools.md`, "Tool rows in the TUI"), so hidden typed text, a refused call's values and an old log's JSON values never match.
- **The selection** starts on the lowest openable cell on screen, or on `Full transcript` when none is or when the welcome card on screen shows `ctrl-o for why`. Each move scrolls the chat the least that shows the selected cell's first row above the dock; `Full transcript` shows the chat where it was. New cells join at the top while it's open and the selection stays on its cell; if that cell goes (a rewind, or a retry that discards it), the next older entry is selected.
- **Keys:** typing and backspace edit the filter, and a text paste goes into it (an image or right-click paste is ignored). Every typed word must appear in the entry, in any case; the dock says `matching "…"`, and `nothing matches` when nothing does. up and down move and stop at the ends, pgup and pgdn move a page of the list, home and end go to the first and last entry. enter opens the entry in the reader, or transcript mode for `Full transcript` (at the top while the welcome card is on screen, otherwise where the chat is); enter on `nothing matches` does nothing. ctrl-o opens transcript mode at the selected cell, so ctrl-o twice is the quick way to every detail. esc and ctrl-c close it and put the chat back where it was, unless you scrolled it with the wheel meanwhile.
- **Mouse:** a click on an entry opens it; a click anywhere else closes the list and puts the chat back.
- **When it doesn't open:** with nothing to open, in a terminal too short for a dock, or with a question card showing, ctrl-o opens transcript mode at once, as before; a fresh session's `ctrl-o for why` still takes one key.
- **While it's open** the turn keeps streaming. The question card, the Retry card and a step-in request wait until it closes, so typed keys only filter and enter never sends a retry; the status row still says a process waits for a password. A terminal that shrinks below a dock closes it.

**Tool rows.** A tool row is a click target as a whole, with no `▸`. A single click opens the call in the reader once the double-click time (400 ms) has passed since the press, or at the release if that is later, timed by the view's next wake as the drag edge is; nothing is painted before it opens. Only a release that ends a single click, on the press's screen cell or one column either side of it on the same row, opens it, and anything else first drops the pending open: another press (which then selects a word or a line), a key, a paste, the wheel, a resize, focus lost or gained, anything that takes the keys (a card, a picker, `/ps`, the settings screen), or a rewind or retry that removes the cell. A click while a selection is painted in the view clears the selection and opens nothing; a selection scrolled out of view doesn't stop it. A press within 150 ms of the terminal reporting focus gained never opens, so the click that brings the window forward only focuses it. A drag from a tool row selects and copies it as text, and a click on a content cell's view lines (a diff's code) opens nothing, while its `▸` row opens at once. It opens the call's exact log, below.

**The reader** is a full-screen, read-only page in the same frame as transcript mode:

```
 ✓ Run the wisp-core tests  cargo test -p wisp-core                  exit 0 · 12s
────────────────────────────────────────────────────────────────────────────────
     $ cargo test -p wisp-core
     in ~/Projects/Wisp · exit 0 · 12s
        Compiling wisp-core v0.1.0
     …
   / search · 1-21/43 · ctrl-o every detail · q esc back
```

- **The title row** is the cell's first main-view row without its `▸`: `╎ Thought for 4 s` (`╎ Thinking` while live), or the tool row with its facts. It never opens anything. A `─` rule in `line` follows it. Neither is selectable.
- **The body** is the cell's transcript-mode rows minus that header: the whole thought; a tool call's exact log (below); or a content tool's whole view, such as a diff. Wrapping and widths are transcript mode's, and a resize rewraps after the same debounce. A wrapped line continues under its own text: an output line at its own indent, a command after its `$ `, a question's option under its number; a line indented past half the row starts again at the left.
- **A tool call's page** reads, in order: the input (`$ ` and the whole command; for a process call the typed text, only when the terminal echoed it, its keys and a search pattern; `Writing a command · 14s` until the call is complete); one muted facts line, `in {cwd}`, the outcome in words (`exit 101`, `timed out after 2m 00s`, `couldn't start: {reason}`, a refusal's first line, `moved to the background after 10s as process 27`) and the duration; the description when it isn't the title; a view the call carries, unfolded; the result's notes; `Output saved to {path}`, from the moment the call starts, since `ToolCallStarted.file` names the file (a call that wrote nothing loses it at the end, when the kernel deletes its empty file); the output; any file problem; and for a background call a muted `process 27 · running`, then its outcome or `before the restart`.
- **The output** is the call's output file, read off the render thread for this call only, at most its last 2 MiB; past that the output starts with `… earlier output is in {path}`. With no file it is the call's last lines. `ToolCallStarted.file` names the file from the start, so a running call has it too, and a background process keeps writing the same file. An unfinished last line shows only the text after its last `\r`, and a character cut by the read is left out until the next one.
- **While the call or its process runs,** the page reads the file again at most once a second, and only while it is at the bottom, following. A read keeps the old text on screen until the new one lands, so nothing blanks. Scrolled up, the page holds still and the key row adds `G follows`; `G` (or end) follows again. The title row's clock and comet still tick once a second, without reading the file. A read that brings nothing new changes nothing on screen. The end of the call (`ToolCallFinished`) or of its process (`ProcessEnded`) marks a file read while it ran as stale, so one last read gives the page its final state, here and in transcript mode; until it lands the old text stays, with no `Loading the output file…`, and a page scrolled up keeps its place: a top inside the output keeps its offset from the output's first row, so a facts line that wraps or failure notes that appear at the end move nothing on screen. A following page rewraps the whole file on each read, about 12 ms for 2 MiB in a release build (`crates/wisp-tui/tests/budgets.rs`, `a_following_page_reload_at_the_file_tail`).
- **A running terminal's** file holds its output with held keys masked, including terminal control sequences, which reads poorly for a program that redraws its screen, so the key row adds `s screen`: `s` closes the reader and opens `/ps` on that terminal's live screen without attaching; esc goes to `/ps`'s list, then back to the main view.
- **Search on a live page** (`/`, `?`, `n`, `N`, below) runs again over the new rows each time a read lands, so its count and highlights follow the file. Jumping to a match above the bottom stops following, as scrolling up does, until `G`.
- **The key row** reads `/ search`, the position, the waiting count, `ctrl-c to stop` while a turn runs, `G follow` on a live tool page scrolled up, `s screen` for a running terminal, `c copy` for a code block, `ctrl-o every detail`, then the way back, `q esc back` (`q esc ctrl-o back` in transcript mode, after `[ to scrollback`). A narrow terminal drops whole hints from the one before the way back.
- **Small terminals:** below 8 rows the title and rule drop; below 2 rows the key row drops too.
- **Where it opens:** a finished cell at its top; a live one at its bottom, following it until you scroll up. A background call whose process still runs counts as live. Once scrolled up the text stays put while the cell grows, and an output file landing never moves it. Only scrolling to the bottom follows again.
- **It closes** when its cell goes: a rewind that cuts it, or a retry that discards it. A question arriving behaves as it does in transcript mode.

| Key | What it does |
|---|---|
| up, down, `j`, `k` | Scroll a line |
| pgup, pgdn, space, `b`, `f` | Scroll a page |
| `u`, `d` | Scroll half a page |
| home, end, `g`, `G` | Jump to the top or the bottom |
| `/`, `?`, `n`, `N` | Search forward or back, then step (below) |
| the wheel | Scroll |
| drag, then ctrl-c | Select, then copy; a drag onto the title row scrolls up |
| right-click | Copy the selection |
| ctrl-o | Transcript mode at this cell |
| `s` | A running terminal's live screen in `/ps` |
| `c` | Copy the code block's code, whatever is selected, and clear the selection |
| `q`, esc | Back to the main view as you left it, with the composer kept |
| ctrl-c | Copy the selection; else stop a running turn; else back |

`[` (the hand-off to the scrollback) stays in transcript mode only. ctrl-b, ctrl-f and ctrl-d do nothing on any of these pages: in the main view ctrl-d quits and ctrl-b backgrounds a command, so no page teaches them. Transcript mode takes the same `b`, `f`, `d`, `u` and search keys. Transcript mode now keeps the cell and the offset it shows when a file lands or a cell above it grows, and opening it at a cell no longer follows the bottom; only opening at the bottom or scrolling there does. Transcript mode keeps a row count per cell and draws only the rows on and near the screen, so opening it, scrolling and jumping draw a few screens; the counts are kept while it is closed, and only the first open after a resume, a resize or a display change that rewraps counts every cell.

### Hook runs in transcript mode

Every hook run gets one muted line in transcript mode, in event order: `hook <name> at <point> <outcome> · <ms> ms`, where the outcome is `ran`, `applied`, `blocked: …`, `continued` or `cancelled`; a failed, timed-out or refused run keeps its notice (`hook <name> at <point> failed: …`, `timed out` or `gave a reply Wisp refused (…)`) as its one line instead. A change to a call with no row of its own shows its field names only. A change the run applied joins its line (the last run of that hook at that point and call): an input's changed fields, a result's or a message's new first line, cut, a continuation's text, or a block's reason. Normal scrollback keeps only today's lines (a failed, timed-out or refused run, `rewritten by …` on a tool row), with no row and no blank line for the others. The lines show in every full view: transcript mode, the main view with `display.verbose` on, and an agent's chat (`docs/agents.md`). Verbose scrollback writes a run's line once, as it was when the run finished, so a change that comes after the line has gone to scrollback shows only in transcript mode. A resumed session replays `hook_ran` and `hook_applied`, so it draws the same lines, and a message a `user-input` hook rewrote is drawn once, as it was live. For a `process` call the typed text follows the echoed rule (`docs/tools.md`): a hook's rewrite of its input shows the changed field names only, unless the call's verdict says the terminal echoed it, and then the value is quoted and cut as the row's typed text is.

### Search in the reader, transcript mode and help

- **`/`** searches forward and **`?`** back (on the help screen `?` closes it instead). The key row becomes a prompt: `/` (or `?`) faint, the query, and `2 of 5` or `no matches` at the right. Each typed character moves to the first match at or after the top row (at or before it, searching back), scrolling only when the match is off the page. Starting a search clears a selection; a paste goes into the query, newlines as spaces.
- **enter** closes the prompt and keeps the match marked; enter on an empty query keeps the previous search. **esc** (or ctrl-c, or backspace on an empty query) cancels: the page goes back where it was and the previous search, if any, comes back.
- **`n`** steps to the next match in the search's direction and **`N`** the other way, wrapping at either end.
- **Matching** is literal, and ignores case unless the query has a capital. It runs over logical lines as copy joins them, so a match across a soft wrap is found once and marked on both rows, and the gutter and padding copy skips are never matched.
- **Only the current match is marked,** in reverse video; on a cell that is already reversed (changed diff words at 16 colours and none) it is also bold and underlined, so it always stands out.
- **Long pages** are searched in slices of about 4 ms, so a key never waits; the count reads `2 of 5+` until the scan ends, which it does with no key pressed. In transcript mode the scan draws each cell once, as it reaches it, so a slice that reaches a long cell draws it first and lasts that draw.
- **A refresh keeps the place:** a rewrap or a live cell growing re-runs the query and keeps the current match (held as its logical line and offset), without scrolling to it.

## Right-click

With the mouse captured, most terminals never see a right-click, so Wisp does what the terminal would have done, following Windows Terminal's default. VS Code is the exception (below):

- **With a selection** (in the transcript, transcript mode, the help screen or anywhere else selection works), right-click copies it exactly as ctrl-c does: to the native clipboard or by OSC 52, with the same toast and the same `Too large to send` refusal. Then it clears the selection, as Windows Terminal does. It does this whatever `display.copy_on_select` is, and never sends anything to the kernel.
- **With nothing selected**, in Windows Terminal (`WT_SESSION`), right-click pastes: Wisp reads the clipboard's text on its clipboard worker (`docs/media.md`, "Right-click with nothing selected") and puts it in the composer exactly as a bracketed paste of that text, with the same paste chips, sanitisation and dropped-path images. A clipboard with no text but an image attaches the image as Alt-V does. A right-click paste is ignored wherever an empty paste is (a picker, a question card while it has the keys, transcript mode and the help screen, the settings screen and the `Add an API key` card, `/ps`, `/agents`, a step-in card, the sidebar overlay, the retry choice's endpoint picker, a process you're typing into, a hand-off to the scrollback, while quitting); the retry choice itself passes the paste to the composer, as it does typing. A right-press during a left-button drag does nothing. Losing the terminal's focus ends a drag, so a release the terminal never sent (after Alt-Tab mid-drag, say) can't leave right-click dead; with button-event tracking (`?1002h`) the terminal reports no motion without a button held, so focus is the only sign of a lost release. Over SSH and in any other terminal, whose right-click doesn't paste, right-click with nothing selected does nothing.
- **In VS Code** (`TERM_PROGRAM=vscode`), VS Code acts on a right-click itself even with the mouse captured: with `terminal.integrated.rightClickBehavior` at `copyPaste`, its default on Windows, it sends its own clipboard as a bracketed paste just after the press, since it sees no selection of its own. So Wisp never pastes on a right-click there, and VS Code's paste goes wherever a paste goes. When a right-click copies a selection, Wisp drops the first bracketed paste that arrives within 0.5 s of the press, which is VS Code's paste of the old clipboard; any later paste lands as usual. To have right-click only copy, set `terminal.integrated.rightClickBehavior` to `nothing` in VS Code; right-click with nothing selected then does nothing.

Wisp acts on the press, as Windows Terminal does, and ignores the release. The help screen's row reads `Copy the selection; paste when nothing is selected` where Wisp pastes on a right-click and `Copy the selection` elsewhere, VS Code included; without mouse capture it isn't listed, since the terminal's own right-click works.

## The settings screen

`/config` replaces the transcript and the composer with the settings screen, as transcript mode does, and nothing on it moves the transcript. A turn keeps running behind it: the orb and the status sentence in the footer stay live. Esc goes back one page and returns to the transcript where you left it from the first screen. The `Add an API key` card and the settings screen each keep every key while open, so `/config` typed into the card and `/account add` typed on the screen are only text there. The card opened from the composer closes the screen; opened from the screen (`Add an API key…`, or an endpoint's Key or URL row) it is drawn over the screen, which takes the keys back when the card closes. The screen needs 40 columns and 12 rows; in a smaller terminal `/config` says `Make the terminal 40x12 for settings, or use wisp config.`, and a terminal that shrinks below that while the screen is open shows the same sentence until it grows again.

The screen asks the kernel for its settings when it opens (`DescribeSettings`) and shows `Loading settings…` until the answer arrives. It asks again whenever a `SettingsChanged` or an `EndpointsChanged` (an endpoint saved from the `Add an API key` card, or a new default endpoint saved from the screen) arrives, and when a turn ends, keeping the old rows on screen until the new answer. A refused question (before the first endpoint exists, `set up an account first`) closes the screen and leaves the reason in the transcript. An answer that arrives after the screen closed is dropped.

At every width, the list is followed by three lines: the selected row's title, an explanation when it has one, and its keys. An open box keeps four rows and puts its keys only in the footer. `?` opens the full details at every width. A question takes the rows it needs, never fewer than four; the list gives them up, keeping three rows where there is room. A short question keeps its answers and their keys visible. From 60 columns a row can carry `this chat`; below that width the line under it carries the comparison alone. Numbers right-align, text does not. Values keep their room while names keep a readable minimum; a value that still does not fit shortens at its end. The header shows the model, effort and account; the footer shows the status sentence and the screen's keys.

### Rows

The first screen is Model, Effort, Account, Web search, Tools, Compact at and Theme, then up to three rows under `Often used`, then Plugins and `More settings…`. Often used is absent when empty; its rows also stay in their groups, or on the plugin's page for a setting the plugin adds. Account opens the accounts; Plugins opens the plugins, with a count of those on. More settings opens Chat, Compaction, Agents, Voice, Display, Advanced and Models. Each group leaves out the seven fixed first-screen rows. Context window stays on a model's page; Pricing stays on an account's page. A page's bold first line is its name. Where rows don't fit, a mark on the rule below or above the list shows that some are hidden.

A setting's group comes from `SettingSpec.section` in the protocol registry. The TUI keeps only the order (`ORDER`) and the first screen's settings (`EVERYDAY`); a test fails if a built setting has no place on a page or in the account and model fields. `/config NAME` opens the page a setting is on, with it selected; `/` searches settings, accounts and plugins across every page, with results under their groups and no leading rows. Enter edits a match in place, keeping the filter and its other matches.

A row shows what this chat uses. `this chat` marks a flag or chat value that differs from what new chats use; the line under it says `This chat: high. New chats: medium.` A shell variable, a forced terminal value, a mode or a preset is named there instead, with `Saved: …` only when a value is saved, otherwise `Wisp's own: …`. A saved-for-new-chats value keeps `· new chats` after it when the screen is opened again. A plugin's row shows its switch state, with its summary in the title and `Tools: …` below when it has tools; Enter switches it and the right arrow opens its page: settings first, then sidebar sections and hooks.
On its page each tool reads `on` when the model can use it, loaded or found through search, and `off` otherwise. Turning off or removing the plugin of the provider this chat is on asks `Where should this chat go?` with the card `/logout` has: Enter on an account moves the chat there and turns the provider off; Esc changes nothing. A hook controlled by a setting offers `enter its setting`; Enter selects that setting on this page if it is listed here, otherwise opens the setting's page.

Behind `?`, the details list the layers under the one that won, as `wisp config get NAME --explain` does: under `--account`, a set `WISP_ACCOUNT` or `WISP_ENDPOINT` (listed even when it names no account, as the layer a start would use and fail on) and a provider key from the environment or a given `--env-file` (named with its file); under `--keychain`, a set `WISP_KEYCHAIN`. A key only in the default env file is one of the start rules, not the environment, so it shows no layer. A value given in the wrong place may be a key, so it is never shown. An `--endpoint` or `WISP_ENDPOINT` that names a configured endpoint always shows in full, whatever its shape; any other key-shaped value reads `(looks like a key, not shown)`, and one that is no endpoint name reads `(not an account name, not shown)`, on the screen, under `--replay`, in `wisp config get` and `list` with or without `--json`, and in the start error for an endpoint that doesn't exist. The kernel keeps the flag's real value, so clearing a session endpoint falls back to it. A model ID that looks like a key, saved in `config.toml`, reads `(looks like a key, not shown)` in its row, its layers and the screen's header, as everywhere else (`docs/settings.md`); the session still uses the value as given. Given with `--model` or `--set`, it is refused before the start ("`wisp --model NAME`"). The terminal's variables, in the colour, theme and glyph sources, their warnings and `wisp doctor`'s environment table, show a value only when it is short and plain (at most 16 of letters, digits, `.`, `_`, `+`, `;` and `-`; for `LANG`, `LC_*` and `WSL_DISTRO_NAME`, at most 32 and `@` too), or for `TERM` when it is shaped like a terminfo name (lowercase letters, digits, `.`, `+` and `-`, no part between dashes longer than 12); anything else reads `(not shown)`.

The display rows show what this terminal uses: the TUI replaces the kernel's display values with its own resolution, which adds `forced` (`NO_COLOR`, `TERM=dumb`, output that isn't a terminal) and `detected`. The kernel's saved and shadowed values stay listed.

The details show the label and help, the layers This chat, Saved and Wisp's own (the winner in `text`, the others muted), the choices, Name (the setting's name in the file), and the keys. A saved value names its file and line, such as `config.toml line 3`; with several places it also names `this model` or `all models`. Flag and variable names appear only here. In the list, a change's cost follows its key or moves to the line above when it will not fit. When `config.toml` changed after Wisp started, the row says `config.toml now has max · new chats`. A broken file is named under the header with `config.toml has an error at line 12; saving is off until it's fixed`.

### Changing a setting

Every change on the screen is saved to `config.toml`, except `d` on a `this chat` row, which changes only this chat. Saving Compact at, Context window, Background compaction or Background start clears that setting's chat values at every place and ends its start flag in this chat. The saved numbers then decide, just as for a new chat. Model, Effort, Account, single-place and display settings keep the chat value that equals what was saved. A setting that can change mid-session takes effect by its class: now, or at the next turn as an appended delta. A setting that only applies to new chats keeps `new chats` on its row. Pricing is set per named account, so on a keys-file account its row says to add one with `wisp account add`. Moving, filtering, opening a box and esc never write anything.

- **Web search** is one row on the first screen, from the catalog's search tool: `on`, `off`, or why not (`not on this model`, `not on this account`, `off for this account`). Enter turns the `web-search` plugin on or off for the next turn, quoting the cache rebuild beside the key (or `switching web search on or off drops earlier reasoning` when thinking is held) and `pending · next turn` until then; where a saved Tools list leaves the tool out, the same save puts it back in that list. A row that reads a reason has no Enter. Where an account's saved `native_web_search = false` or the model's saved empty `web_search` list is the reason, the line under says `Saved for this account: off.` or `Saved for this model: off.` and `d` removes it for new chats (`off for this account · new chats` stays until then). A resumed chat that still sends search while the switch is saved off reads `on · this chat`, with `This chat: on. New chats: off.` under it and no Enter.
- **A switch** (Cache diagnostics, Refusal fallback, Context line, Count with the server, Session log, Mouse, Full repaint, Pictures, Verbose): enter turns it on or off and saves it.
- **A short choice** (Cache lifetime, Pricing, Message time, Instructions, Summary effort, Keychain, Theme, Colour, Motion, Ambiguous width, Copy on select, in screen order): enter opens a choice row below the list; left and right (or `h` and `l`) move, enter saves, esc or `q` closes it unchanged.
- **A number** (Retry budget, Instructions warning, Compact at, Your messages kept, Context window, Scroll speed): enter opens a one-line box holding the current value, with its unit below (`tokens, 0 or more`). It reads `8k`, `1M`, `60%` and `$0.50`, and previews the parsed value as you type (`= 8,000 tokens`). Compact at shares its point-and-window format between the row and box: `600,000 of 1,000,000`. A saved point above the model's cap shows the capped point on the row (`136,000 of 200,000`) and the saved value below (`Saved: 400,000.`). The box's full hint, `tokens, or a share such as 60%`, appears only where it fits; details say `of this model's 1,000,000 tokens`. Where the window is unknown, no Wisp's own point is claimed. An entry that does not parse shows the rule in `error`, and enter does nothing until it is valid.
- **A pick** (Model or Effort): Enter opens the `/model` or `/effort` picker over the bottom of the screen, with each row's cost as in the composer. Enter saves the choice as the global default and switches this session to it, the current value included. Saving an effort on an Antigravity level-in-ID model also saves that level's model ID, only when this chat is on the account new chats use and the saved model is unset or is the ID before the effort change; otherwise only the effort is saved; a model whose effort levels differ asks for the effort next, as `/model` does. Esc closes it unchanged. The note names what was saved: `… · saved to ~/.config/wisp/config.toml (was claude-opus-5-5)`, and after a model and its effort picked together, both old values: `(was claude-opus-5-5, high)`. A typed or pasted ID that looks like a key is never shown: its row reads `(looks like a key, not shown)`, is dimmed with the reason `this looks like a key, not a model ID, so it can't be used`, and enter does nothing. `/model` typed in the composer with such an ID refuses it the same way: nothing switches, the status line gives that reason, and the line is not kept for recall. The `--model` flag refuses one ("`wisp --model NAME`"). Saving refuses it too: no route writes a model ID, or a model table name, that looks like a key to `config.toml` (`config.toml wasn't changed: that model ID looks like a key, so it isn't saved`). A table already in `config.toml` under such a model can still be changed, as `wisp config set 'models."<id>".compact_at' 99%` does, since nothing new is written; creating one, or a path whose model part isn't a model ID (`config.toml wasn't changed: that path's model part isn't a model ID`), is refused. When the session's model looks like a key, the screen offers no place under that model, and a row saved under it reads `This model's ID looks like a key, so its settings can't be saved here; use wisp config set with the real ID, or edit config.toml.` for Enter and `d`. The Account row's `d` on a saved Account row quotes no cost when the start rules choose the account, since that choice is known only at the next start; under a start flag with no known default, no `d` is offered. A saved endpoint becomes the default in the endpoint list, so the next `wisp` starts on it.
- **A checklist** (Tools): enter opens the choices below the list, each `[x]` or `[ ]`. Left and right (or `h` and `l`) move, space checks or unchecks, enter saves the checked ones (none is allowed), esc or `q` closes it unchanged. Tools lists the catalog's tools: each leader checkable, its followers indented under it as `(comes with LEADER)` and marked as it is, except a follower that doesn't come with its leader, a note row never checked (`report · child agents only`), an unavailable tool with its reason, and each name saved at the place being written that isn't a leader as a kept line: `NAME · no plugin provides it; kept` (`a saved value that is no tool` when the name could be a pasted key), or `NAME · comes with LEADER; kept` for a saved follower whose leader the list doesn't name. A saved follower beside its leader gets no line and is dropped when the checklist saves. A kept line isn't checkable and is saved again unchanged; resetting the row is how to drop one. The search tool is listed without a tick and says `Switched by the Web search row.` while the cursor is on it; saving leaves it in or out as the saved list had it. On a line that isn't checkable the footer drops `space toggle`. With no saved list, Tools starts with the tools on by default checked. Saving exactly those removes the saved list instead of writing one, so tools added to Wisp later come on too; under a flag, the environment or a forced value for `tools` the list is saved as given (`docs/tools.md`, beside the `--tools` table). Tools changes from the next turn; the key names any cache rebuild cost.
- **`d` on a `this chat` row** returns only this chat to what new chats use; it never removes the saved value. It is not offered when a flag would keep winning or when returning to the current model's number would override other models. Enter can still save a choice, ending a per-model flag. On an ordinary saved row, `d` removes that saved value and names what wins afterward.

- **For** in a number's box chooses where it saves. Tab and shift-tab cycle all models and this model; the first choice is this model where it already has a number, otherwise all models. The account-and-model place appears only where it already holds a number. Empty the field and press Enter to remove the number at the chosen place; the preview names the removal. The choice ends with the box, though a set-aside open box keeps it. At a narrow width the caption reads `For (tab)` and shows one choice with its position. A model's page offers only its model-wide path, plus this account's path where a number is already saved there. A number saved on another account is reachable from that account's chats. Background compaction's Enter switch saves for all models; its model-wide switch stays on the model's page.
- **`d`** resets the setting: it removes the value saved at its place and this session's value, so the next layer wins. The keys show the destination first, such as `d back to medium`; for Model, Effort and Account the cost is the switch's, as the picker shows it. On/off rows have no reset key. With nothing saved there and no session value, `d` says so.

A display change applies to the running screen when the kernel confirms its save, never before and never after a refusal. The TUI keeps what it read at start (the environment, the streams, the flags and the terminal's replies) and resolves the display again with the new value as this session's, so `auto` means what it meant at start, and `NO_COLOR` and `TERM=dumb` still win. Picking `auto` falls through to the `WISP_*` variables and `--set` flags, as a fresh start does. The next frame is the one a fresh start with that value would draw: a new theme or colour depth repaints in full, a new ambiguous width or Verbose re-wraps the transcript, Motion changes the animation policy, Scroll speed the wheel step, Full repaint the painter and whether anything moves, Pictures the agents and Context boxes, and Copy on select is worked out again for this terminal. Mouse goes through the terminal guard, so a restore after a signal, a suspend and a hand-off to the scrollback all return to the new setting; changed while handed off, it waits for the take-back. A Mouse change also re-wraps the transcript and rewords the drag hint in `/help` (`drag` with the mouse off, `shift-drag` with it on). Text already handed off to the terminal's scrollback keeps the look it was written with. A display row's source then reads `session` unless a forced layer (`NO_COLOR`, `TERM=dumb`) still wins, where the session and saved values both show as not used; choosing `auto` drops the session value, so the row reads whichever layer then wins.

The pickers' switch badges for a compaction a switch would make due come with the kernel's quotes (context MR 4); until then a row shows only the switch's cache rebuild.

While a save is in flight its row keeps its value followed by `saving…`, and Enter and `d` on it do nothing until the answer comes. A change that applies at the next turn, made while a turn or a compaction runs, reads `pending · next turn` until the next turn starts. The answer adds a note to the transcript that names the value it replaced, so you can set it back: `Retry budget → $0.50 · applies now · saved to ~/.config/wisp/config.toml (was none)`, `(was low this session; medium saved)` when the saved value differed, and for a reset `Retry budget reset · applies now · saved value removed (was $0.50) · the default now wins this session`, or `--retry-budget $1.00 on the command line now wins this session` when a flag is next; a setting that applies next session says `wins in new sessions`; a reset that removed only a value set this session says `session value removed (was 500,000)` in place of `saved value removed`. When something still wins this session, the note names it instead of when the change applies, and a change that applies next session keeps `new chats`: `Tools → none · new chats · saved to … · the command line still wins this session` under `--tools`. A variable lasts beyond this session, so one that wins over a saved value says `still wins in new sessions`, and for a setting that applies next session it is named even when a flag also sets it, since only the variable reaches new sessions: `Keychain → on · new chats · saved to … (was off) · your environment still wins in new sessions` under `WISP_KEYCHAIN=off`, while `--keychain` alone keeps `the command line still wins this session`. A reset names the variable that now wins: `Keychain reset · new chats · saved value removed (was off) · WISP_KEYCHAIN=off in your environment wins in new sessions`. For a per-model setting, a save ends its flag and chat values before the override note is worked out. For the other settings that apply in this chat, a saved value that isn't `auto` under a flag reports no override, since its chat value wins over the flag; a setting that applies next session still names the flag, as `Tools` does under `--tools`. A display setting forced for this run says so: with `NO_COLOR` set, `Colour → 16 · saved to … · a forced value still wins this session`, and the same for Motion under `TERM=dumb` or output that isn't a terminal, and for Ambiguous width under `TERM=dumb` or a locale that isn't UTF-8. A row counts as forced only when it would still win over a value set this session; `TERM=dumb` and output that isn't a terminal fall back to no colour, but a Colour you set wins over them, so the note says it applies now. The kernel learns the forced rows at start from the TUI's own resolution, in a full-screen session only. Picking `auto` under a `--set display.…` flag says `the command line still wins this session`, and under a `WISP_*` variable `your environment still wins this session`, since `auto` falls through to them now. A reset keeps its usual note. A saved value that a more specific saved place beats, such as a number saved for all models while the model has its own, names that place in the Settings screen's words and leaves out when the change applies, since nothing changes in this chat: `Compact at → 300,000 · saved to ~/.config/wisp/config.toml (was not set) · this model still wins`, or `this account and model`, or `this account`; a save made on another model's page keeps `applies when it is the current model`. A model table that sets its own `verbatim_user_tokens` is saved in `config.toml`, so it wins in new sessions too, and both a change and a reset of Your messages kept say so: `Your messages kept → 4,000 · saved to ~/.config/wisp/config.toml (was 8,000) · this model still wins`. A refused change shows its reason under its row in `error` and in the transcript; the row keeps its value.

While `config.toml` is broken, the screen changes nothing: Enter and `d` say `config.toml has an error; saving is off until it's fixed`.

### Accounts and models

The Accounts page has one row per account, sorted by name: its name, kind and host, with an unavailable account's reason muted under it. `✓` (`+` in ASCII) marks the account the chat uses; `new chats` follows a different saved default. `Add an API key…` opens the card. The Models group behind More settings has one row per model the shipped table or `config.toml` knows, by its short name when Wisp ships its row and by its ID otherwise, with its window and its compaction point when one is saved, sourced `config` when `config.toml` has a table for it and `shipped` otherwise. `Add a model…` opens the model picker and then that model's settings. A model ID that looks like a key reads `(looks like a key, not shown)` and opens nothing. Every name, host and note from the kernel is drawn sanitised.

The right arrow on an account or Enter on a model lists its fields under `Account NAME` or `Model ID`; Esc goes back to the row you came from. Enter on an account uses it and saves it for new chats. Its fields are Provider, Auth, Pricing, Requests in flight, Key, URL, and Key store for a stored key or Key variable and URL variable for one read from variables. Key reads `set · keychain`, `set · credentials.json`, `set · variable` or `not set`, never the key; URL reads the host with `(path hidden)` or `the provider's URL`.

- **Pricing** is an ordinary setting at the endpoint's path and saves through `ChangeSettings`, as any row does.
- **Provider, Auth, Key store and the variables** are the connection fields: they are sent as one `EditEndpoint`, so the credentials code's edit runs with its probe and write order, and apply to that endpoint from the next switch to it (from the next session for the current one). The answer adds a note naming what changed. A failed check asks `Save it anyway, without the check?`, Yes sending it again unchecked; a keychain that refuses the write says why, and for a move offers the file instead.
- **Key and URL** open the hidden prompt card over the screen: nothing typed there is drawn or kept. `d` on URL goes back to the provider's URL; a key can't be removed on its own.
- **Enter** on an account saves it as the default and switches this chat to it from the next turn: one `ChangeSettings { endpoint, save: true }`. The keys put the cost beside Enter. `✓` marks the account the chat is on; when another account is saved for new chats, that one reads `new chats`. Enter on the current account saves it for new chats if it differs, and otherwise does nothing. The right arrow opens its fields; `Add an API key…` opens the card.
- **`d`** on an endpoint asks `Remove "work" and delete its key from the keychain?` (or `credentials.json`), Remove removing it and Keep or esc keeping it. The current endpoint and the keys-file endpoint (`docs/endpoints.md`) can't be removed, and say why. When the keychain can't delete the key, the screen asks whether to remove only the table and leave the keychain item. A removed endpoint leaves the list and can't be switched to for the rest of the session.
- **A model's fields** are its built per-model settings (Context window, Compact at and the others saved under a model), each showing Wisp's own value when nothing is saved. A number's box has a For choice only where this account already holds a number for that model; an empty value removes it at the chosen place. `d` on a model asks `Remove your settings for opus-5-5? Wisp's own numbers stay.` and Remove sends one `ChangeSettings` that removes every value saved for it.

### Keys

| Key | What it does |
|---|---|
| up, down, `j`, `k` | Move between rows, skipping headings; with a question open, up and down move its mark |
| pgup, pgdn | Move a page |
| home, end, `g`, `G` | The first and the last row |
| tab, shift-tab | In a number's box, the next or previous For choice; otherwise the next or previous group heading while filtering, or the question's marked answer |
| `/` | Filter: type to keep the rows whose label, name or help holds every typed word, in any case and any order. Sections with no match collapse. Enter keeps the filter, esc clears it, ctrl-c clears the text and then stops filtering |
| `?` | Show the full details at every width in place of the list; `?` or esc goes back |
| enter | Open a leading row; otherwise change the row: turn a switch on or off, or open its choice row, checklist, editor or picker; in one of those, save. An empty number removes it from the place For names. On an account, use it and save it for new chats; on a model, list its fields; on an add row, open the card or the picker |
| space | In a checklist, check or uncheck the selected choice; with a question open, take the marked answer |
| `d` | On a this-chat row, return to what new chats use without changing what is saved, when that reset is available; otherwise remove the row's saved value; on an account, remove it; on a model, remove its saved settings |
| right arrow | Open a leading row, an account’s fields or a plugin’s page |
| left arrow | Go back one page, to the row that led there; on the first screen, do nothing |
| `1`, `2`, enter | Answer the screen's question (remove a plugin, an endpoint or a model's settings, save without the check) from its numbered answers: a digit takes its answer, enter takes the marked one, and a Remove answer is never marked when the question opens, so a digit on it only moves the mark |
| esc | Close an open editor, picker or list unchanged, else go back from the full details, else clear the filter if there is one, else go back one page, to the row that led there, or close from the first screen |
| `q` | Close an open question, choice row or checklist, else go back one page and close from the first screen |
| ctrl-c | In an editor, clear its text, then close it. Otherwise stop a running turn; when idle, clear the filter if there is one, else the composer's rule: a first ctrl-c says `Press ctrl-c again to quit.` and a second quits |
| ctrl-l | Repaint |
| a click | Select the row |
| the wheel | Scroll the list |

The terminal cursor, hidden, sits at the first column of the selected row, or in the filter or the editor while you type there, so screen readers and magnifiers that follow it follow the selection. A question that waits on you, the error choice after a failed turn, and the `Add an API key` card opened from the composer close the screen when they open, and hold the keys as usual. What you were doing is kept, unsaved: the next `/config` with no word opens on the same row and filter with the editor as you left it. Esc or `q` closing the screen discards it, and so does `/config` with a word. Text that looks like a key is never kept.

## `/usage [NAME]`

`/usage` opens a live, scrollable page under the title `Usage`, with one block per
account: this chat's first, marked `this chat`, then the other subscriptions, then
the API accounts. From an agent's chat it opens in the main chat, so `this chat` is
main's. A block's title is `Kind · name` (a ChatGPT block names its tier,
such as `ChatGPT Pro`, when the service reports it); at narrow widths it breaks
where its `·` was. A subscription's windows show a bar, the percent used and the
local reset (`resets 14:20` or `resets Mon`; at 120 columns `resets 14:20, in 2 h 10 min`
or `resets Mon 09:00`). The percents end in one column as wide as the page's widest
reading (`74.99%`, `unknown`); the bars give up the extra columns. One window for a
model, such as `Fable weekly`, widens the label column for the whole page. A bar fills
whole squares; its used squares turn to the warning look at 75% and the error colour
at 90%, its free squares stay dim; a reached limit reads
`limit reached, resets …`. A window whose reset
time has passed reads `reset since` (muted) in place of its bar and percent. Where the
bars go, a block can instead read `checking…` (earlier bars stay, muted), an old
reading's age (`2 h ago`, muted, from 15 minutes old or after a failed lookup); with no
reading at all, `usage unavailable right now` with `r try again` after a network
error, an answer Wisp can't read, a throttled lookup or one that ended without a reading
(such as while another Wisp chat was checking that account), or `usage unavailable until 14:05`
while the service asks Wisp to wait; `usage unavailable` when the lookup is refused or
not supported (with `enter sign in again` when it needs a new sign-in),
`sign-in expired` with `enter sign in again`, or an unavailable account's own reason
(with `enter sign in again` on a subscription). An API account whose key was refused
in this chat reads `Key refused` in the error colour, with the fix the retry card gives
for a key from a variable (`Key refused: change ANTHROPIC_API_KEY in ~/.config/wisp/.env`,
or `… in your shell`); the line goes once a step on that account succeeds or its key is
changed.

Every Wisp chat and `wisp account` share one usage record per account
(`~/.local/share/wisp/usage/<name>.json`): a reading another chat took shows here with
its age. Usage is looked up at most every 10 minutes across all of them, after a model
step or when the page opens, and a failed lookup waits 30 minutes, then an hour.

Under the bars, `API equivalent ~$41.20` is what a subscription covered and `Spent $2.86`
what the API charged; a block holding the other billing's spend (a name listed again as
another kind of account) shows both. An API account Wisp can't price reads
`Spent not priced`, and spend with no known split reads `Spent` with the mixed-billing
note. The amounts right-align in one column.
The fixed bottom lines are `This chat`, `All chats, today` and `All chats, 30 days`.
Each shows the API part and the subscriptions part separately, never added together;
a kind shows when an account of that kind is listed or the last 30 days hold its spend.
Under each account, `Today, all chats` and `30 days, all chats` count that account alone.
At the end of the page, each kind has a list of up to seven days, newest first. Until
30 days are recorded, the window names its first day instead (`All chats, since 12 Nov`
and `Since 12 Nov, all chats`). With nothing recorded only `This chat` shows.

The cursor starts on the first account that must sign in again, else at the top;
`/usage NAME` starts on NAME. Up/Down, Home/End move it; PageUp/PageDown and the mouse
wheel scroll. The keys row reads `r refresh · esc close`, with `enter sign in again`
first while the focused account must sign in again: Enter then opens the same sign-in
as `/login NAME`, and the page comes back as it was when the sign-in ends. On an API
account whose key Wisp saved, the keys row reads `enter change key` first: Enter opens
the form that changes the key, and the page comes back when it is saved or closed. Enter
does nothing on any other account. `r` looks up every account at once, skipping the
spacing, unless the service asked Wisp to wait. Other letters are ignored,
and text pasted while the page is open is dropped. Esc returns to the unchanged draft
and transcript position. While a turn runs Ctrl-C stops it; when idle Ctrl-C closes
the page. The keys row drops whole hints at narrow widths.

Opening `/usage` or `/usage NAME` asks for eligible accounts' usage, as does an account
signed in, signed in again or changed while the page is open; a lookup runs only when one
is due under the spacing above, and `r` forces one.
Resize, scrolling, focus and clock redraws never fetch. Non-active interests are
correlated and released after completion/error or close. Active background monitoring
retains the existing spacing/backoff/single-flight rules. API-key, expired, unavailable
and lookup-off accounts are never fetched. Claude lookup is on by default;
`claude_usage = false` turns it off, and those Claude blocks then show only this chat's
spend.

Each block counts this chat's requests, the root's and every child's, that were sent on
that account, as the kernel splits each agent's spend. Spend with no account named (older
logs) or on an account not listed counts in the total only. An account whose split is
unknown shows one amount. An account signed out or removed leaves the page, and its
block shows this chat's spend on it again once its name is listed again.
Across chats, Wisp records each request in `~/.local/share/wisp/spend/`, by the writer's
local date, from each chat's next start after the update. Chats not restarted since the
update and `--no-log` chats leave their spend out. Removed accounts count in the bottom
lines and day lists only. The figures refresh on opening, `r` and this chat's spend
changes while the page is open. `+` marks an amount that may be short, including a day
whose file could not be read completely. Nothing is inferred or priced again. Deleting
`spend/` starts the record over; concurrent writes are intended for local disks.
This page and its updates are local display only, never transcript history, model
messages or log records.

The status line keeps the activity/retry sentence and stop controls ahead of usage
details. Reset and freshness deadlines update idle displays without a keypress or
network polling. Fresh relevant windows warn at 75/90/95/100 percent used, only the
highest newly crossed threshold. Anthropic family warnings and compact hints apply only to the models documented for that named weekly limit; other family readings stay visible on the full usage page. Session spend never changes these percentages.

## `learn`

`learn [--new] [--account NAME] [--port N]` is the learning app, its own program beside `wisp`: it serves a lesson to a browser on this computer, or over HTTPS to the home network, continuing the last lesson unless `--new` is given (`docs/learn.md`).

## `wisp --model NAME`

`--model` and `--set model=…` take a model's ID or the short name `/model` shows for it, so `--model opus-5-5` and `--model claude-opus-5-5` both start `claude-opus-5-5`. At chat start, a name found in the catalog `/model` lists becomes its ID, so the start's cards, `/config` and the first request all carry the ID; there is no other alias and no guess from a prefix. With `--account NAME`, a name still outside the catalog starts as typed, which is how a gateway's own model is used.

On a fresh start, if none of the candidate accounts already offers the name given with `--model` or `--set model=…`, Wisp refreshes each eligible loaded list that lacks it once, without waiting for the next background refresh. These fetches run together. Wisp waits up to 5 seconds for them, plus up to 3 seconds only if a client version lookup starts. If `--account`, `WISP_ACCOUNT` or `WISP_ENDPOINT` names an account, only that account is a candidate.

A fresh chat on a Gemini account with a loaded list refuses a model the list still lacks after any refresh, including with `--account` or a configured model: `wisp: model gemini-4-carbon isn't one account antigravity serves`, exit 1, and no chat request is sent. A resumed chat still opens so it can switch to a listed model; its other settings still change without moving its model.

A fresh chat also refuses a known Anthropic model without adaptive thinking, whether chosen by `--model`, the config default or a main preset: `wisp: claude-opus-4-5 doesn't support adaptive thinking, which Wisp needs`, exit 1, and nothing is sent. A resumed chat still opens so `/model` can switch it to a supported model.

When no account is named (no `--account`, `WISP_ACCOUNT` or `WISP_ENDPOINT`) and no chat is resumed, the start checks that the account where new chats start offers the model, by `/model`'s own list for that account, after any refresh (a ChatGPT subscription's hidden entries count, so `--model gpt-reserve` from an API key moves to it):

- **It does:** the chat starts there, as without the flag.
- **One other account offers it**, and moving there doesn't start charging: the chat starts there, under `Started on chatgpt · subscription · gpt-6.1-sol high` with the reason `claude-subscription doesn't offer gpt-6.1-sol.`; a script gets `wisp: claude-subscription, where new chats start, doesn't offer gpt-6.1-sol; this chat starts on chatgpt.` on stderr.
- **Several offer it, or the only one starts charging:** the chat opens on the start account with its own model, and the `/model` picker opens over it filtered to the name, under `chatgpt doesn't offer opus-5-5`. No chat request is sent until a row is taken; the focus starts on a row of the named model that doesn't start charging, else on none (`↑↓ choose`), and a row on the API is taken only after moving to it. Esc keeps the chat as it is. A script stops with exit 1: `wisp: chatgpt doesn't offer opus-5-5; add --account claude-subscription, claude-work or claude-api to choose one.`, or `… add --account claude-api to use it.` for one account.
- **None offers it, or the name names no model:** the picker lists every model under `No account offers opus-55 · choose a model`, focused on the chat's own row; the typed name is never tried. A script stops with exit 1: `wisp: no account offers opus-55. Available models: opus-5-5, sonnet-5-5, gpt-6.1-sol.`

The start picker shows no prices, since nothing is cached before the first message, and no reasoning line. Each row's line is `Starts this chat on NAME · covered by your subscription` or `Starts this chat on NAME, on the API` (`Starts on NAME` where that doesn't fit), its keys `enter start · esc stay on NAME` (`esc stay` where that doesn't fit), and `←` doesn't open the accounts view. A row taken prints `Started on NAME · …` with no price and no `Moved from …` line.

A value that looks like a key is refused at once, for every command: `wisp: --model: this looks like a key, not a model ID, so it can't be used` (or `--set model:`, and the same for a model's `aliases`), exit 2, and nothing is sent.

## Subscription sign-in

`/login [claude|chatgpt|antigravity|NAME]` or `/login --name NAME` signs in between turns, in
one card that changes in place. This card and the `/logout` card stay open while
a turn runs (after a child's report, say), and the sign-in waits for you.
`/login` asks Claude, ChatGPT, Antigravity or API key; `/login claude`, `/login chatgpt`
and `/login antigravity` start one step in. When that provider already has accounts, the card offers
`Sign in again to NAME` for each, then `Add another … account`. The name step
suggests `claude`, `chatgpt` or `antigravity`; when it is taken, it asks for one. `/login NAME` signs
in again to that account, and provider keywords win: `/login --name claude`
signs in again to an account named `claude`. API key opens the key form, titled
`Add an API key`, with its name filled in.

The card shows the sign-in page as one link, its host and path only, then whether
Wisp tried the browser and where the copied link went: `tried your browser` or
`no browser here`, then `link copied`, `link copied to tmux`, `link sent to your
terminal` or `couldn't copy the link`. Wisp opens the page and copies the link once
per sign-in. The link text is an OSC 8 link to the whole URL, so the terminal's own
link click (ctrl-click in Windows Terminal and VS Code, cmd-click on a Mac,
ctrl-shift-click in kitty, shift-click in WezTerm and Alacritty) opens it on the
computer the terminal runs on. A plain click on the link row opens the page again,
at most once a second, or copies the link again when no browser opened; a click
with ctrl, alt or shift does nothing in Wisp. `ctrl-o` opens and `ctrl-l` copies
when the card has an input field (Claude's `Code`, or headless Antigravity's
`Callback URL`); enter opens and `c` copies when waiting on a local loopback
listener (ChatGPT, or Antigravity with a local browser). When no browser opened and the
copy fails, the card says `quit, then run wisp login claude` (or `chatgpt` / `antigravity`), which
prints the link whole in the shell. The copied link stays in clipboards and tmux's
buffers after Wisp exits, which is harmless: its `state` is single-use and expires
with the sign-in.

Over SSH, VS Code's Remote-SSH terminal opens the page by itself through VS Code's
`BROWSER` helper and forwards port 1455; if its Ports view shows another local
port, free 1455 locally and sign in again. In other terminals, run `ssh -L
1455:localhost:1455 HOST` for ChatGPT, then open the link with the terminal's link
click or paste the copied link into your browser. A Claude sign-in needs no port
(its page shows the code to paste), and headless Antigravity prompts for the
browser's `http://127.0.0.1:51121/oauth-callback?...` redirect URL (`Callback URL`).

Paste Claude's code into the `Code` field, never the message composer: a paste
replaces what's there, typing adds to it, and it stays masked, with at most its first
and last 4 characters and its length shown. A mistake shows one line above the keys,
the same line `wisp login` prints. No network keeps the sign-in, and enter sends the
code again. A refused code or a time-out ends the sign-in: the field takes nothing,
and enter, the link or `ctrl-o` opens a new page. Escape cancels and clears the
card's credentials. A saved-but-not-ready result offers enter to retry loading
without signing in again.

When it's done, the card reads `✓ Signed in · Claude subscription · NAME` and, when
this chat runs on another account, offers `Use NAME for this chat`. Signing in again
closes at once and leaves one line. `/login status` is a hidden alias that opens
`/model`.

A sign-in that brings back an account another name already holds is refused, in
`/login` and `wisp login` alike: nothing is saved and both names stay as they
were. In place of the done card, the card, and then the transcript, show `Not
signed in: this is the same Claude account as claude-work.` and `To sign it in
again: /login claude-work`; enter or escape closes it. `wisp login` prints the
same two lines on stderr, with `wisp login claude-work`, and exits with status 1.
Signing in again to the same name works as before. The comparison uses a SHA-256
of the account's IDs from the provider's token answer, kept beside the profile as
`oauth-<id>.account`; the IDs themselves are never stored, logged or sent. One
Claude login in two organizations, or one ChatGPT user in two workspaces, counts
as two accounts. Profiles saved before this check have no such file until their
next refresh, so they are not compared.

`/logout [NAME|all]` signs out of an account, config first, and deletes its
stored credentials. With no name it lists the accounts with no row chosen, and
Enter does nothing until ↑↓ chooses one. Enter or Space on the chosen row, a
digit (only while the list has nine rows or fewer) or a click then does what
naming that row does, so a subscription signs out at once.
A stored key and `all` ask before anything changes, with two answers, Sign out
and Keep, and the card opens on Keep: ↑ or `1` chooses Sign out, and only Enter
or Space then signs out, while `2` or Enter on Keep leaves everything as it was.
When the chat's account goes and the way on isn't clear, "Where should this chat
go?" opens with no row chosen; a digit, an arrow or a click chooses a row, and
Enter or Space signs out. Idle agents on the account follow the
chat; one whose move would start charging the API follows only when the card
showed it. Every row prices each agent that moves with it, `cache rebuild ~$0.41
and ~$0.08`, or names each amount when one has nothing cached yet; the line an
agent prints after the move shows the amount its row showed. A question about
two or more accounts that says more than its title counts them, `Sign out of 4
accounts?`, over one line naming what goes, `(key deleted)` after a key Wisp
saved, then `Moving to NAME · API` with one row per mover and its price
right-aligned under `cache rebuild` (`—` for one with nothing cached), and
`Staying behind` with each agent that can't move and why. A key's question
gains the `Moving to` section when agents on the key move. A preset without
`fallback` whose account goes gets a line: `Preset coder used claude-work; it now runs on its
parent's account.`, or, when an agent that may spawn it would be on the API,
`…; new agents from it won't start on the API.`, followed after the sign-out by
`Sign in again with /login claude-work.` The keys-file account is never
edited. Credential cleanup failure leaves the account removed and says what may
still be saved. After the last account the sign-in card opens; when it doesn't
open at once, `Run /login to sign in again.` says how, and the next message
opens it. First login or recovery
when no endpoint is usable still requires `wisp login` outside chat, whose lists are chosen from
with the arrow keys and Enter (`docs/endpoints.md`).

## `wisp status`, `wisp account list` and `wisp setup`

All three print one panel of your accounts, grouped under `Subscriptions` and
`API`, with each subscription's 5-hour and weekly used percentages and time
until each available reset (`42%, resets in 2h10m`), `starts new chats` on
the account new chats start on, and the fix for an account that needs you
(`sign-in expired · wisp login NAME`). With a `NAME`, `wisp status` and `wisp
account list` show that one row; `wisp setup` prints the panel before `Add
another?`. Model limits the provider reports, such as `Fable weekly`, appear
as rows of their own under the subscription. The limits come from one lookup
per subscription with a 3-second limit, so each of these commands may ask the
provider's usage page; one that can't be read shows `usage unavailable`
(`docs/endpoints.md`, "Subscription login and status"). `wisp endpoint` is a
hidden alias of `wisp account`.

## `wisp plugin add MANIFEST WASM`

Installs a WASM plugin from its manifest and its component, from the next
session on; with one argument, `add` takes a plugin id and adds a removed
plugin back. What it checks, writes and prints is in `docs/plugins.md`,
"Installing a WASM plugin".


## `/rename`

`/rename Strada` renames the saved chat you are in and says `Renamed this chat to Strada`.
The name is cleaned with the automatic naming rule: quotes and emphasis are removed,
key-shaped words are masked and the first line is kept, up to 200 characters.
The confirmation shows the name as kept. The automatic name never replaces it.
The bundled `chat` plugin supplies the command; asking the agent to rename this chat uses its held `rename_chat` tool.

## `/skills`

The standalone `skills` plugin supplies this command when registered; it is not
yet in the standard bundle. It lists the skills in the project's and user's
folders, each skill's source (`project` or `yours`), state (`on`) and description.
Skills another plugin offers are found by search, not listed here. Broken files are shown by `wisp doctor`
instead. With no skills it names the folders; while search is off it says how to
turn it on in `/config`, Tools. See `docs/skills.md`.

## Restarting the chats kept open

`wisp restart` restarts the chats kept open on the newest version.
`wisp restart <id>...` restarts only those chats and leaves the others alone; an
id's start is enough, from any folder. Each finishes what it is doing first,
then comes back by itself; idle chats restart at once.
Enter stops what remains, and the command does so after five minutes at most.
Ctrl-C before that leaves the chats still finishing as they were. Agents whose
turns were stopped carry on; a stopped first reply's message is back in the composer.
The first restart asks before stopping chats that cannot finish first. Inside
a Wisp job nothing restarts. Open windows reconnect, and messages accepted while
finishing go out after the restart. A window keeps unsent text; `/leave`, then
`wisp --resume` updates that older window once the text is sent.
See `docs/daemon.md`, “Restarting”.

## Quitting a chat from the terminal

`wisp quit <id>...` quits chats without opening them. An id's prefix is enough, from any folder.
A chat open in a window is left alone: quit it there. Inside a Wisp job this command quits nothing.

### `/mcp`

Lists MCP servers on this machine or at an address, their state and tool count.
`/mcp add <name> <command or address>` starts one server, saves its list and
writes its configuration; quote arguments with spaces. Never type a key: ask your
agent to add the server with its `keys`, and Wisp asks you for each one
(`docs/mcp.md`, "Server keys"); a key exported before Wisp starts reaches every
command and server it starts. `/mcp sign-in <name>` shows the sign-in link and
tries to open it in the user's browser; elsewhere, finish with `/mcp sign-in <name> <address>`.
`/mcp remove <name>` stops and removes that server. The model finds server tools
through `search` and runs them through `use`; `/mcp` says when search is off.

## Wisp's daemon

`wisp daemon` shows the daemon's state and every chat kept open by its title and
short id. A chat with no title is named by its folder.
A chat a window has is marked `in a window`. Open one with `wisp --resume <id>`;
quit one without opening it with `wisp quit <id>`.

`wisp daemon start` starts it, or says it is already running. `wisp daemon stop`
leaves every chat open and prevents another chat start from bringing it back;
start it explicitly to undo that. Under WSL the status first shows any Windows
setting or restart still needed. See `docs/daemon.md` for the local interface.
