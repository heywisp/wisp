# Wisp in the terminal: brand, colour and motion

*Design spec for the terminal UI (milestone M3), 21 October 2026. Written by the designer role for the coder who
will build it in Ratatui. Accepted as M3's design source with the M3 plan (`docs/plans/m3-tui.md`), with the
deviations listed in that plan's §10. Amended 26 October 2026 by the whole-TUI review (`tui-review.md`): firm
lines and one selection recipe, the content edge, the composer as a rounded box, the folded thought, the error
headline, the three-group status row and docked pickers.*

**What this is.** Wisp's existing brand (the Stone palette, the Halo orb, the logo and the voice) translated into a
terminal, precisely enough to implement without guessing: every colour as hex, every glyph as a code point, every
animation as frames, characters, colours and milliseconds.

**See it.** `python3 docs/design/preview.py` plays every animation in your own terminal: about 80 seconds, where any
key skips and `q` quits. `--section NAME` plays one part (`--list` shows the names), and `--reduced-motion`,
`--theme light`, `--colors 256|16|none` and `--ascii` show the fallbacks. It uses the standard library only, and
it reads this document's token, glyph, frame, curve and sampling-grid tables; the rest is copied from its prose.
Where the two disagree, this document wins.

**The decisions in one screen**

| | Decision |
|---|---|
| Colour | VS Code's **Dark+** in dark and Wisp's **Stone** in light: blue `#569CD6` / `#1656C9` is the one accent. Body text and the background stay the terminal's own. **Gold means "needs you" and nothing else**; green means done. |
| Mark | **Halo in three braille cells**: `⢎⠶⡰`. An ink ring of dots, open at one o'clock like the logo, around a wisp-blue core. Next to `wisp` in bold it's the logo; by the composer it's the live orb. |
| Signature motion | **The orb's own states and tempos**: thinking turns its arcs every 500 ms, a streamed answer lights the core as each chunk lands, and "needs you" beats twice in gold every 1.6 s. With no work, the orb stays still; while waiting for working agents, a comet laps the ring and the core breathes. |
| Rules | Nothing moves in scrollback. Working loops are sampled every 100 ms; with no work, the idle screen writes no decorative frames. Reduced motion, `NO_COLOR`, ANSI-256, ANSI-16 and ASCII each have a defined fallback. |

---

## 1. Brand essence

### 1.1 What the existing brand says

From the plan (`wisp-plan.pdf`, 5 October), the two storyboards (light and dark, 13 flows each) and the design
artifacts of 21 October (orb motion, website story and website motion), which take precedence:

- **Name.** *Wisp*, set lowercase in the logo (`wisp`). Each user can also name their own Wisp (the storyboards
  use "Juniper"). The name shows in greetings and beside the mark, but "the logo doesn't change". In Wisp.Core the
  front-door agent is **Jarvis**. None of the sources say what the name means. The terminal keeps that unsaid, and
  uses the obvious reading only as a private design idea: a small light that goes ahead of you.
- **Promise.** "Your own AI. With a computer of its own." Wisp is "a personal AI assistant you talk to, with a
  computer of its own for real work", "open source, on the AI you choose".
- **Logo.** An ink ring, open at about one o'clock, around a wisp-blue dot, then `wisp` in Geist. It is the
  brand's face, and it **stays still**.
- **The orb: Halo.** The default orb, and the brand's face in motion. It has 48 ink dots in a ring around a
  15-light blue core. **"The ring is you, the core is Wisp."** It has eight presence states (Idle, Listening,
  You're speaking, Thinking, Speaking, Working, Needs you and Offline), plus a one-shot Done. "Same states, same
  meanings" on every surface. **"Needs you" is reserved**: only it is gold, gathered and beating.
- **Colour: the Stone palette.** Warm stone greys with near-black ink, one blue accent, gold for attention, green
  for done and red for the microphone and destructive actions. Light and dark are equal citizens (snapshots are dark only for now; see `docs/testing.md`, "Snapshot themes").
- **Type.** Geist and Geist Mono. Sentence case throughout. The signature heading is **two halves**, a light lead
  and a heavy finish ("First run: **sign up and go**"). Kickers are small capitals with wide tracking.
- **Voice.** "A capable friend: short, specific and honest, plain words first." Calm confidence, with no hype,
  no exclamation marks and no fear. Wisp says what it did in one plain sentence; the technical layer sits behind
  "Show details" or Advanced mode.
- **Motion.** Full, Calm or Reduced, and the operating system's reduced-motion setting wins. Only transform and
  opacity change: no blur. A state change morphs over 0.8 s on `cubic-bezier(.45, 0, .2, 1)`. Every orb pauses
  when it can't be seen.
- **Imagery.** None beyond the orb. The rest is plain cards, chips ("Needs you", "Done", "Suggested"), tables and
  sample content that is labelled as such.

### 1.2 How it translates to a terminal

1. **The terminal is the canvas.** Stone's warm grounds become the user's own background. Wisp paints only meaning:
   the accent, states, small bands and diff tints.
2. **Halo survives at three cells.** Braille gives a 2 × 4 grid of dots in each cell, and dots are exactly what
   Halo is made of. Three cells hold a ring of eight lights and a four-light core, each half in its own colour
   (§3).
3. **The states keep their meanings.** The same eight states, the same colours and the same tempos. Gold stays
   reserved, so a cache miss or a warning is never gold.
4. **Plain sentences first, details on request.** The Code tab's design in the storyboards (flow 11) already is a
   terminal UI: one-line tool rows ("Edited `path` +71 −0"), diffs, a status line with "64% context left · Esc to
   stop", and approvals as one plain sentence with Show details. The terminal follows it closely.
5. **Motion means work.** With nothing running, the terminal orb stays still. While the root waits for working
   agents, the comet and core breath continue (§6.3); root turns and working agents keep their existing motion.

### 1.3 Where the sources disagree, and what this spec chose

| Topic | The sources | Decision |
|---|---|---|
| Accent | The TUI research proposed cyan (`#6BD8DE` / `#007890`) and a `✧` mark | Brand wins: wisp blue, and Halo's core `⠶` as Wisp's glyph |
| Attention colour | The research used amber `warn` for needs you, approvals **and cache misses** | Gold for needs you only. A cache miss is set in plain `text` among muted facts (§7.1) |
| Orb motion | Today's orb is still in most states. The 21 October artifacts propose Subtle, Medium and Expressive and recommend **Medium**, which isn't in the app yet | The terminal uses the engine's and Medium's tempos: thinking arcs turning once every 4 s, a 1.6 s sparkle, a 4 s speaking breath, a 2 s working pulse and the 1.6 s beat |
| Idle | The app's idle has a comet every 6 s and a 6 s core breath | **Still with nothing running.** Keep the comet and core breath while waiting for working agents, with the chat mark beside "Waiting for reports" (§6.3) |
| Reduced motion | Today's reduced orb dims every light to half. The proposal keeps full brightness plus one 8 s brightness drift in the core | Full brightness and **no** drift: in a terminal any change is a redraw. The 1 Hz timer shows that Wisp is alive |
| Accent contrast | The plan says ≥ 3:1 (WCAG's graphics minimum). The orb engine says ≥ 4.5:1 as text | 4.5:1 for any token that colours text, 3:1 for glyph-only tokens (§2.2) |
| Light-theme gold | `#FFD21F` measures 1.21:1 on the light canvas, so the brand uses it only as a fill with ink text, and `#735600` for gold text | The same, plus `warn.glyph` `#947909` (3.52:1) for gold glyphs and rails on light backgrounds |
| Red | The app reserves red for real capture (the microphone pill) and destructive actions | A terminal has no microphone, so red means errors and destructive confirmations |
| Fonts | Geist and Geist Mono | A terminal can't choose its font. Nothing depends on Geist; every glyph was checked in Cascadia Mono (§5.3) |
| "You're speaking" | The brand's state for when the user is talking | Becomes **Listening**: the composer has text, and the ring closes around you |

---

## 2. Colour system

### 2.1 Principles

- **Leave the terminal's own colours alone.** Don't paint the background. Body text is the terminal's default
  foreground (SGR 39). `text` is VS Code Dark+'s `#D4D4D4` (Stone's `#161614` in light) for Wisp's own text: your
  message, queued rows and labels.
- **One accent**, wisp blue: Wisp's core and turn marker, links, focus, the selection and Jarvis.
- **Reserved meanings.** Gold means needs you, green means done or added, and red means error, deleted or
  destructive. They are used for nothing else. Code is the one exception: Dark+ paints comments and numbers
  green there, as VS Code does beside its green git and test marks; green in code never means done.
- **Warnings and notes are plain `text`, never gold.** A warning, a note or a confirmation (a paused
  compaction, a withdrawn attachment, an unknown context window) is set in plain `text`, which stands out
  from the muted facts around it. A destructive confirmation, such as "press x again" to stop a process, is
  `error` instead. A note attached to your message (a withdrawn attachment) sits below the message's band,
  off the `surface`, led by a `text.faint` bullet (`∙`, `-` in ASCII), so it never reads as text you typed. Gold (`warn`,
  `warn.glyph`, `warn.fill`, `warn.on`, `warn.tint`) is drawn only where something needs you, such as a
  process waiting for input; yellow program output becomes `hue.warm` (§7.2). The context meter is
  `text.muted` below 75% of its limit, bold `text` from 75% (bold keeps the step visible where `text.muted`
  and `text` look alike, as in ANSI-16), and bold `error` past the limit while automatic compaction is off.
- **Colour never works alone.** Every state also has a glyph and a word. `NO_COLOR` loses no meaning.
- **Tiers come from tone, not SGR dim.** Faint (SGR 2) renders differently in every terminal.

### 2.2 Tokens

Dark values are VS Code's Dark+ (`dark_plus.json`, `dark_vs.json` and the workbench defaults they rely on, mapped in
`research/tui-palette.md`); light values are Wisp's Stone, copied from the design artifacts. Where Dark+ has no
such colour (`thinking`, `user`, the orb's tones), the token keeps its recipe, given in each case. `mix(a, b, t)` is "t of a over b" in sRGB: CSS `color-mix(in srgb)`, which is how
the orb engine turns opacity into colour.

**Text and meaning** (foreground). "Min" is the contrast floor enforced at runtime (§2.7): 4.5 for text and 3.0
for glyph-only tokens. A dash means decorative, with no floor.

| Token | Dark | Light | Use | Min |
|---|---|---|---|---|
| `text` | `#D4D4D4` | `#161614` | Body text: drawn as the terminal's default foreground | 4.5 |
| `text.muted` | `#A6A6A6` | `#57564F` | Secondary text, tool arguments, metadata, the footer | 4.5 |
| `text.faint` | `#757575` | `#85837A` | Rails, rules, key hints that are repeated elsewhere. **Never on its own for meaning** | 3.0 |
| `text.disabled` | `#585858` | `#A3A29A` | The offline orb | — |
| `accent` | `#569CD6` | `#1656C9` | Wisp blue: the core, Wisp's glyph, links, focus, Jarvis | 4.5 |
| `thinking` | `#92A4B2` | `#47566E` | Thinking text: `mix(accent, text.muted, .25)`, the dimmed core's cool grey | 4.5 |
| `user` | `#828282` | `#767672` | The user's `›` marker: `mix(text, bg, .55)`, the ring at rest, because "the ring is you" | 3.0 |
| `ok` | `#89D185` | `#12703A` | Done, passed, added lines | 4.5 |
| `warn` | `#CCA700` | `#735600` | **Needs-you text only** | 4.5 |
| `warn.glyph` | `#CCA700` | `#947909` | Gold glyphs and rails: the needs-you orb, `◉` and the approval rail | 3.0 |
| `error` | `#F48771` | `#B90A11` | Errors, failures, deleted lines, destructive choices | 4.5 |
| `tool` / `tool.meta` | = `text` (bold) / = `text.muted` | | A tool's verb and target, and its right-hand facts | |
| `agent` | = `accent` | | Wisp and Jarvis | |
| `role.coordinator` | `#9CDCFE` | `#5B48CB` | Project rows (each led by a coordinator): light blue (dark), the brand's Violet `#6B5BE0`, corrected (light) | 4.5 |
| `role.coder` | `#4EC9B0` | `#0A6E6E` | The coder: the brand's Teal `#148A8A`, corrected | 4.5 |
| `role.reviewer` | `#DA70D6` | `#AD2061` | The reviewer: the brand's Rose `#D0457E`, corrected | 4.5 |
| `role.researcher`, `role.designer` | = `text` | | Neutral, the brand's "Ink" | |
| `syn.keyword` | = `accent` | | Syntax highlighting | 4.5 |
| `syn.string` | `#CE9178` | `#0A6E6E` | | 4.5 |
| `syn.number` | `#B5CEA8` | `#945206` | Numbers; light's orange is about 30° of hue away from gold | 4.5 |
| `syn.type` | `#4EC9B0` | `#5B48CB` | | 4.5 |
| `syn.comment` | `#6A9955` | `#57564F` | Comments, italic | 4.5 |
| `syn.function` | `#DCDCAA` | `#161614` | Function names and calls | 4.5 |
| `syn.variable` | `#9CDCFE` | `#161614` | Variables, parameters and fields | 4.5 |
| `syn.control` | `#C586C0` | `#1656C9` | Control flow such as `if`, `for` and `return` | 4.5 |
| `hue.cool` | `#B180D7` | `#0A6E6E` | Outside code: Tools in the context report and the Context box's tools part | 4.5 |
| `hue.warm` | `#F5F543` | `#945206` | Outside code: Instructions, the Context box's skills part and yellow program output | 4.5 |
| `mark.1` | `#9CDCFE` | `#2466A5` | An account's mark `●`, in blue | 3.0 |
| `mark.2` | `#DA70D6` | `#7E4B90` | An account's mark `●`, in magenta | 3.0 |
| `mark.3` | `#4EC9B0` | `#00786C` | An account's mark `●`, in cyan | 3.0 |
| `mark.4` | `#B180D7` | `#5C57A4` | An account's mark `●`, in violet | 3.0 |
| `mark.5` | `#DCDCAA` | `#94436D` | An account's mark `●`, in pale yellow (dark) or pink (light) | 3.0 |
| `mark.6` | `#29B8DB` | `#007290` | An account's mark `●`, in sky | 3.0 |
| `working` | `#9CDCFE` | `#2466A5` | A working agent's dot in the agents box, its picture and `/agents`, and in the sidebar's trees for a working node with no role: light blue | 3.0 |
| `gleam` | `#E4F4FF` | `#061633` | Main's core in the agents box's picture, drawn at 0.9 over `accent`: near-white pale blue (dark), deep navy that shows on the light background and on `accent` (light) | 3.0 |
| `mark.gone` | = `text.muted` | | A gone account's `○` | |
| `secret` | `#CE9178` | `#945206` | A secret's mask: its start, stars, last 4 and length | 4.5 |

In light, the role colours are the brand's own accent gallery (Violet, Teal and Rose; Blue stays the theme accent
and Ink is neutral), moved to about the accent's OKLCH lightness and fitted to 4.5:1. In dark they take Dark+'s
variable blue, type teal and bracket orchid. Hues near gold, green or red are excluded, exactly as the app refuses
user accents near its reserved colours.

The six marks tell accounts apart (§7.5), one colour per account, a seventh sharing one. Light takes six OKLCH hues
spaced through blue, violet, magenta and cyan at about 0.50 lightness, clear of gold, green and red by the same
rule. Dark takes Dark+'s blue, orchid, teal, purple, pale yellow and bright cyan, in the same order of families.
Roles and marks never sit in code or the context report, so sharing a value with a syntax colour is harmless.

`hue.cool` and `hue.warm` keep the context report and program output clear of green, gold and red, which Dark+'s
`syn.string` and `syn.number` sit close to. They take `syn.string`'s and `syn.number`'s light and 16-colour
values; in dark `hue.cool` takes `mark.4`'s values and `hue.warm` Dark+'s terminal bright yellow.

`working` takes `mark.1`'s values in every table: a working agent's dot is light blue, as account 1's mark is.

`secret` stays orange in both modes: `syn.string`'s Dark+ orange in dark, and `syn.number`'s light and 16-colour
values.

In light, `syn.function` and `syn.variable` take `text`'s explicit `#161614`, where plain code keeps the terminal's
own colour, and `syn.control` takes `accent`'s values.

**The orb's tones** (glyph colours: §6.3 animates between them)

| Token | Dark | Light | Recipe | Min |
|---|---|---|---|---|
| `orb.ring` | `#828282` | `#767672` | `mix(text, bg, .55)`: Halo's ring at rest | 3.0 |
| `orb.ring.listen` | `#B9B9B9` | `#363634` | `mix(text, bg, .85)` | 4.5 |
| `orb.ring.speak` | `#676767` | `#969692` | `mix(text, bg, .40)`: the ring steps back while Wisp answers | — |
| `orb.ring.dim` | `#555555` | `#ACABA7` | `mix(text, bg, .30)`: the comet's resting value in the splash | — |
| `orb.core` | = `accent` | | | |
| `orb.core.dim` | `#487CA8` | `#4174CF` | `mix(accent, bg, .75)` dark, `.80` light. The brand's 0.6 fails 3:1, so it is raised | 3.0 |
| `orb.off` | `#585858` | `#A3A29A` | = `text.disabled` | — |

**Surfaces** (backgrounds)

| Token | Dark | Light | Use |
|---|---|---|---|
| `bg` | `#1E1E1E` | `#ECEBE6` | Dark+'s editor background and Stone's canvas. **Never painted**: it is the reference for contrast and blending |
| `surface` | `#262626` | `#E2E2DD` | The user's message band, fullscreen panels. Fitted to the terminal's background (§2.7); none when the terminal doesn't report it |
| `sunken` | `#161616` | `#E2E1DA` | Code blocks and tool output (fullscreen) |
| `line` | `#585858` | `#ACABA7` | Structure only: rules, the composer box, the welcome card's frame, tree lines |
| `accent.tint` | `#04395E` | `#C1CDE0` | The selected row, everywhere (§7.8) |
| `warn.fill` | `#CCA700` | `#FFD21F` | The "Needs you" chip, with `warn.on` text |
| `warn.on` | `#1E1E1E` | `#161614` | Text on `warn.fill` |
| `warn.tint` | `#352A05` | `#EFE8CA` | Optional band behind a pending approval: light `mix(warn.fill, bg, .14)` |
| `diff.add` / `diff.add.word` | `#2D3125` / `#435026` | `#DEE9E0` / `#B5D1BF` | Added rows, and the changed words inside them |
| `diff.del` / `diff.del.word` | `#4B1818` / `#6F1313` | `#F2DDDB` / `#E7B3B3` | Deleted rows, and the changed words inside them |

The default `surface` is the band §2.7 fits over the default `bg`. In dark, `diff.add` and `diff.del` are Dark+'s
inserted and removed line tints (the added one at 12% rather than 20%, so syntax colours stay readable on it), and
the word tints its inserted and removed text over them. In light, `diff.add` and `diff.del` are Stone's `do-tint`
and `dont-tint`, and the word tints `mix(ok or error, tint, .20)`.

**Firm lines** (`tui-review.md` §1). `line` is Dark+'s tree guide `#585858` dark and `mix(text, bg, .30)` light,
2.34:1 and 1.93:1 on `bg`: visible as structure but quieter than any text tone. `accent.tint` is Dark+'s list
selection dark and `mix(accent, bg, .20)` light, 1.39:1 and 1.35:1. On the tint `text` is 8.09:1 and 11.28:1,
`text.muted` 4.92:1 and 4.59:1, and `accent` 4.07:1 and 4.08:1, so `accent` marks glyphs only on a selected row.

### 2.3 Contrast, computed

Computed with the WCAG 2.2 relative-luminance formula against each mode's grounds (§2.2). The script that produced these
numbers is the same code as `preview.py` (`contrast()`), and its palette section prints them live.

| Token | Dark | on `bg` | on `surface` | on `sunken` | Light | on `bg` | on `surface` | on `sunken` | Min |
|---|---|---|---|---|---|---|---|---|---|
| `text` | `#D4D4D4` | 11.25 | 10.21 | 12.21 | `#161614` | 15.18 | 13.94 | 13.82 | 4.5 |
| `text.muted` | `#A6A6A6` | 6.85 | 6.22 | 7.43 | `#57564F` | 6.17 | 5.67 | 5.62 | 4.5 |
| `text.faint` | `#757575` | 3.62 | 3.28 | 3.93 | `#85837A` | 3.18 | **2.92** | **2.90** | 3.0 |
| `thinking` | `#92A4B2` | 6.49 | 5.89 | 7.04 | `#47566E` | 6.23 | 5.72 | 5.67 | 4.5 |
| `user` | `#828282` | 4.34 | 3.94 | 4.71 | `#767672` | 3.82 | 3.51 | 3.48 | 3.0 |
| `accent` | `#569CD6` | 5.65 | 5.13 | 6.14 | `#1656C9` | 5.48 | 5.04 | 4.99 | 4.5 |
| `ok` | `#89D185` | 9.15 | 8.30 | 9.93 | `#12703A` | 5.17 | 4.75 | 4.70 | 4.5 |
| `warn` | `#CCA700` | 7.22 | 6.55 | 7.84 | `#735600` | 5.75 | 5.28 | 5.23 | 4.5 |
| `warn.glyph` | `#CCA700` | 7.22 | 6.55 | 7.84 | `#947909` | 3.52 | 3.23 | 3.20 | 3.0 |
| `error` | `#F48771` | 6.79 | 6.16 | 7.37 | `#B90A11` | 5.65 | 5.19 | 5.14 | 4.5 |
| `role.coordinator` | `#9CDCFE` | 11.18 | 10.15 | 12.13 | `#5B48CB` | 5.44 | 5.00 | 4.95 | 4.5 |
| `role.coder` | `#4EC9B0` | 8.18 | 7.43 | 8.88 | `#0A6E6E` | 5.07 | 4.66 | 4.62 | 4.5 |
| `role.reviewer` | `#DA70D6` | 5.77 | 5.24 | 6.26 | `#AD2061` | 5.57 | 5.12 | 5.07 | 4.5 |
| `syn.number` | `#B5CEA8` | 9.81 | 8.91 | 10.65 | `#945206` | 5.07 | 4.66 | 4.62 | 4.5 |
| `syn.function` | `#DCDCAA` | 11.80 | 10.71 | 12.81 | `#161614` | 15.18 | 13.94 | 13.82 | 4.5 |
| `syn.variable` | `#9CDCFE` | 11.18 | 10.15 | 12.13 | `#161614` | 15.18 | 13.94 | 13.82 | 4.5 |
| `syn.control` | `#C586C0` | 5.99 | 5.44 | 6.50 | `#1656C9` | 5.48 | 5.04 | 4.99 | 4.5 |
| `orb.ring` | `#828282` | 4.34 | 3.94 | 4.71 | `#767672` | 3.82 | 3.51 | 3.48 | 3.0 |
| `orb.ring.listen` | `#B9B9B9` | 8.50 | 7.71 | 9.22 | `#363634` | 10.15 | 9.32 | 9.24 | 4.5 |
| `orb.ring.speak` | `#676767` | 2.95 | 2.68 | 3.20 | `#969692` | 2.49 | 2.28 | 2.26 | — |
| `orb.ring.dim` | `#555555` | 2.24 | 2.03 | 2.43 | `#ACABA7` | 1.93 | 1.77 | 1.75 | — |
| `orb.core.dim` | `#487CA8` | 3.75 | 3.40 | 4.07 | `#4174CF` | 3.80 | 3.49 | 3.46 | 3.0 |
| `orb.off` | `#585858` | 2.34 | 2.13 | 2.54 | `#A3A29A` | 2.15 | 1.97 | 1.96 | — |

What the table forces:

- **`text.faint` is never text that carries meaning.** It passes 3:1 as a glyph on every ground except light
  `sunken` (2.90) and the light band (2.92), where the sidebar overlay draws it. So line numbers in code and diffs
  use `text.muted` (5.91 on light `diff.add`, 5.47 on dark).
- **Decorative tones** (`orb.ring.speak`, `orb.ring.dim`, `orb.off`) only ever dim the orb, and the orb's state is
  also spelled out beside it in words.
- **The same tokens on common terminal backgrounds.** All the text tokens hold up, so a near-match background
  needs no correction:

| Token | `#000000` | `#0C0C0C` (Campbell) | `#1E1E1E` | `#282C34` | `#FFFFFF` | `#FDF6E3` | `#F5F5F5` |
|---|---|---|---|---|---|---|---|
| `text` | 14.17 | 13.20 | 11.25 | 9.44 | 18.12 | 16.80 | 16.62 |
| `text.muted` | 8.63 | 8.04 | 6.85 | 5.75 | 7.37 | 6.83 | 6.76 |
| `text.faint` | 4.56 | 4.25 | 3.62 | 3.04 | 3.80 | 3.52 | 3.49 |
| `thinking` | 8.18 | 7.62 | 6.49 | 5.45 | 7.44 | 6.89 | 6.82 |
| `accent` | 7.12 | 6.63 | 5.65 | 4.75 | 6.54 | 6.07 | 6.00 |
| `ok` | 11.52 | 10.73 | 9.15 | 7.68 | 6.17 | 5.72 | 5.66 |
| `warn` | 9.09 | 8.47 | 7.22 | 6.06 | 6.86 | 6.36 | 6.29 |
| `warn.glyph` | 9.09 | 8.47 | 7.22 | 6.06 | 4.20 | 3.90 | 3.85 |
| `error` | 8.55 | 7.96 | 6.79 | 5.70 | 6.74 | 6.25 | 6.18 |
| `role.coordinator` | 14.08 | 13.12 | 11.18 | 9.39 | 6.49 | 6.02 | 5.96 |
| `role.coder` | 10.31 | 9.60 | 8.18 | 6.87 | 6.05 | 5.61 | 5.55 |
| `role.reviewer` | 7.27 | 6.77 | 5.77 | 4.85 | 6.65 | 6.17 | 6.10 |
| `orb.ring` | 5.46 | 5.09 | 4.34 | 3.64 | 4.56 | 4.23 | 4.18 |

  None fails; the closest is `text.faint` on `#282C34` (3.04). On lighter dark backgrounds some Dark+ colours fall
  under their floors on the band (`research/tui-palette.md`), which the runtime correction in §2.7 is for.

- **Text on fills:**

| Pair | Dark | Light |
|---|---|---|
| `text` on `surface` / `sunken` | 10.21 / 12.21 | 13.94 / 13.82 |
| `text` / `text.muted` on `accent.tint` | 8.09 / 4.92 | 11.28 / 4.59 |
| `accent` on `accent.tint` (glyphs only) | 4.07 | 4.08 |
| `warn.on` on `warn.fill` (the chip) | 7.22 | 12.50 |
| `warn` on `warn.tint` | 6.13 | 5.58 |
| `text` on `diff.add` / `diff.add.word` | 8.98 / 5.87 | 14.53 / 11.08 |
| `text` on `diff.del` / `diff.del.word` | 9.82 / 7.97 | 13.93 / 9.92 |
| `ok` on `diff.add`, `error` on `diff.del` (the signs) | 7.30, 5.93 | 4.95, 5.18 |
| `accent` (keywords) on `diff.add` | 4.51 | 5.25 |
| `syn.string` / `syn.number` on `diff.del` | 5.51 / 8.56 | 4.65 / 4.65 |
| `syn.comment` on `diff.add` / `diff.del` | 3.99 / 4.37 | 5.91 / 5.66 |

  **Rule:** inside a word-emphasis cell (`diff.*.word`), the foreground is always `text`. Syntax colours drop as
  low as 2.6:1 on the stronger tints. On the row tints themselves every `syn.*` colour and `text.muted` keep at
  least 4.5:1 in both modes except dark comments, so code on a diff row keeps its colours, and a foreground under
  4.5:1 on its row paints as `text` (§2.4), which in truecolour catches only dark comments.

### 2.4 ANSI-256 fallback

Used when the terminal has 256 colours but not truecolour. Indices come from 16–255 only, because 0–15 follow the
user's theme. Most are the nearest by OKLab distance. A few were picked by hand: to keep their hue (`thinking`
110 and 60, light `warn` 94, rather than plain greys and olive; light `mark.5` 132, since its nearest, 96, is `mark.2`'s),
to keep the diff tints distinct from each other (dark 22, 22 and 88; light 194, 151 and 217), or to meet its floor
(light `role.coordinator` and `syn.type` 61). `surface` is the band's nearest index: the default band's here, and on a
terminal that reports its background the fitted band's (§2.7), stored with the band.
Each was checked against its token's floor, and **none falls below it**. Ratios are against each mode's `bg`.

| Token | Dark | Light |
|---|---|---|
| `text` | 188 `#D7D7D7` 11.58 | 233 `#121212` 15.69 |
| `text.muted` | 248 `#A8A8A8` 7.01 | 240 `#585858` 5.96 |
| `text.faint` | 243 `#767676` 3.67 | 244 `#808080` 3.31 |
| `text.disabled`, `orb.off` | 240 `#585858` 2.34 | 247 `#9E9E9E` 2.24 |
| `accent` | 74 `#5FAFD7` 6.83 | 26 `#005FD7` 4.86 |
| `thinking` | 110 `#87AFD7` 7.26 | 60 `#5F5F87` 5.07 |
| `user`, `orb.ring` | 244 `#808080` 4.22 | 243 `#767676` 3.81 |
| `ok` | 114 `#87D787` 9.61 | 22 `#005F00` 6.67 |
| `warn`, `warn.glyph` | 178 `#D7AF00` 7.96 | 94 `#875F00` 4.80 |
| `warn.fill` / `warn.on` | 178 / 234 | 220 / 233 |
| `error` | 209 `#FF875F` 7.05 | 124 `#AF0000` 6.23 |
| `role.coordinator` | 153 `#AFD7FF` 11.10 | 61 `#5F5FAF` 4.70 |
| `role.coder` | 43 `#00D7AF` 9.00 | 23 `#005F5F` 6.28 |
| `role.reviewer` | 170 `#D75FD7` 5.17 | 125 `#AF005F` 5.89 |
| `syn.string` | 173 `#D7875F` 5.97 | 23 `#005F5F` 6.28 |
| `syn.number` | 151 `#AFD7AF` 10.45 | 94 `#875F00` 4.80 |
| `syn.type` | 43 `#00D7AF` 9.00 | 61 `#5F5FAF` 4.70 |
| `syn.comment` | 71 `#5FAF5F` 6.18 | 240 `#585858` 5.96 |
| `syn.function` | 187 `#D7D7AF` 11.30 | 233 `#121212` 15.69 |
| `syn.variable` | 153 `#AFD7FF` 11.10 | 233 `#121212` 15.69 |
| `syn.control` | 175 `#D787AF` 6.33 | 26 `#005FD7` 4.86 |
| `hue.cool` | 140 `#AF87D7` 5.77 | 23 `#005F5F` 6.28 |
| `hue.warm` | 227 `#FFFF5F` 15.66 | 94 `#875F00` 4.80 |
| `secret` | 173 `#D7875F` 5.97 | 94 `#875F00` 4.80 |
| `mark.1` | 153 `#AFD7FF` 11.10 | 25 `#005FAF` 5.40 |
| `mark.2` | 170 `#D75FD7` 5.17 | 96 `#875F87` 4.38 |
| `mark.3` | 43 `#00D7AF` 9.00 | 30 `#008787` 3.65 |
| `mark.4` | 140 `#AF87D7` 5.77 | 61 `#5F5FAF` 4.70 |
| `mark.5` | 187 `#D7D7AF` 11.30 | 132 `#AF5F87` 3.66 |
| `mark.6` | 38 `#00AFD7` 6.44 | 24 `#005F87` 5.89 |
| `working` | 153 `#AFD7FF` 11.10 | 25 `#005FAF` 5.40 |
| `gleam` | 255 `#EEEEEE` 14.37 | 234 `#1C1C1C` 14.28 |
| `orb.ring.listen` | 250 | 237 |
| `orb.ring.speak` | 242 | 246 |
| `orb.ring.dim` | 240 | 248 |
| `orb.core.dim` | 31 `#0087AF` 4.04 | 32 `#0087D7` 3.23 |
| `surface` / `sunken` / `line` | 235 / 233 / 240 | 254 / 254 / 248 |
| `accent.tint` / `warn.tint` | 237 / 236 | 252 / 230 |
| `diff.add` / `diff.add.word` | 22 / 22 **plus bold** | 194 / 151 |
| `diff.del` / `diff.del.word` | 52 / 88 | 224 / 217 |

In 256 colours, `text` on 22, 52 and 88 measures 5.53, 9.82 and 7.19; on 194, 151, 224 and 217 it measures
17.11, 11.74, 14.23 and 10.71. Dark `diff.add.word` reuses 22 and adds bold, because the nearest stronger green
(28) gives only 3.27.

**Code in diff rows.** Code inside a `+` or `-` row keeps its syntax colours only where they stay readable on the
row: any foreground under 4.5:1 on its row, as painted, shows as `text`, in truecolour and 256 colours alike. In 256
colours, on dark 22 that is `syn.keyword`, `syn.string`, `syn.type`, `syn.comment` and `syn.control` (2.85–4.30),
while numbers keep their green (4.99), function and variable names keep theirs (5.40 and 5.30), and `text.muted` and
most marks; on dark 52 `mark.2`; on light 224 `accent`, `syn.control`, `syn.number`, `syn.type` and `mark.2` to
`mark.5` (3.31–4.41), and on light 194 `mark.3` and `mark.5`. In truecolour only dark
comments fall under (§2.3). The theme computes it from the contrast formula. Context rows have no tint and keep the
usual colours.

Animated in-between colours (§6) map to the nearest index 16–255 frame by frame. Steps through the grey ramp
(232–255) are smooth enough.

### 2.5 ANSI-16 fallback

The 16 colours belong to the user's theme, so they map **by meaning** rather than by nearest colour. Codes are SGR
foreground codes; "default" is 39.

| Token | Dark background | Light background |
|---|---|---|
| `text` | default | default |
| `text.muted` | 37 (white, which is grey in most themes) | 90 (bright black) |
| `text.faint`, `user`, `orb.ring*`, `orb.off` | 90 | 90 |
| `line` (rules, frames, tree lines) | 90 | 90 |
| `accent`, `orb.core*`, `syn.keyword` | 94 (bright blue: plain blue is 2.4:1 on Campbell black) | 34 |
| `thinking` | 37 | 90 |
| `ok` | 92 | 32 |
| `warn` | 93 | default + **bold** (yellow is 1.7:1 on white in xterm's palette) |
| `warn.glyph` | 93 | 33 |
| `error` | 91 | 31 |
| `syn.string` / `syn.number` / `syn.type` | 96 / 95 / 95 | 36 / 35 / 35 |
| `syn.comment` | 37 | 90 |
| `syn.function` / `syn.variable` / `syn.control` | default / default / 94 | default / default / 34 |
| `hue.cool` / `hue.warm` | 96 / 95 | 36 / 35 |
| `secret` | 95 | 35 |
| `mark.1` / `mark.2` / `mark.3` | 94 / 95 / 96 | 34 / 35 / 36 |
| `mark.4` / `mark.5` / `mark.6` | 34 / 35 / 36 | 94 / 95 / 96 |
| `working` / `gleam` | 94 / 97 | 34 / default |
| `role.*` | default (the name already says the role) | default |
| The "Needs you" chip | 30 on 43 (black on yellow) | 30 on 43 |
| The selected row | reverse video (7) | reverse video (7) |
| Diffs | no backgrounds: the `+` and `-` signs in 32/92 and 31/91, the code in default, context rows included (no syntax colours inside a diff) | same |
| Changed words in a diff | reverse video (7) | reverse video (7) |
| Every other surface | none | none |

The `thinking` token itself carries no italic at any depth, so the status word `Thinking` stays upright (§4.1:
italic is never for UI labels). The renderer sets italic on the thinking body at every depth, `NO_COLOR` included.

In 16 colours, **brightness animations become static** (there is no in-between colour). Shape animations still
play (§6.10). When the probe can read the palette (`OSC 4`, §9.5), each mapped colour is checked against the real
background: anything under 3:1 becomes `default`, plus bold if it was `warn` or `error`.

### 2.6 `NO_COLOR`

A non-empty `NO_COLOR` turns off **every** colour code: 30–37, 38, 39, 40–47, 48, 49 and 90–107. A command-line
flag or a config-file key can override it ([no-color.org](https://no-color.org/)). Other environment variables
can't: `NO_COLOR` beats `WISP_COLOR`, `FORCE_COLOR` and `CLICOLOR_FORCE`. What still carries meaning:

| Need | Without colour |
|---|---|
| Hierarchy | Bold (1) for structure, the tool verb and the wordmark; normal for the rest. Never faint (2) |
| Thinking | Italic (3) and the `╎` rail |
| Links | Underline (4) |
| Selection, the "Needs you" chip, the chosen approval option | Reverse video (7) |
| Done plan items | Strikethrough (9) and `✓` |
| States | The glyphs alone: `✓ ⨯ ◦ ◉ ◌`, the orb's shapes and the words |
| Diffs | `+` and `-` columns (always present); changed words in reverse video (7) |

Motion is unaffected by `NO_COLOR`: the shapes still animate. Brightness-only loops become still, as in ANSI-16.

### 2.7 Adapting to the real terminal

1. **Light or dark.** In order: the user's setting (`WISP_THEME=dark|light`), then the background from `OSC 11`
   (dark when OKLab L < 0.60), then `COLORFGBG` (background index 0–6 or 8 means dark), then dark. Re-query
   after `CSI ? 997 ; 1|2 n` (theme-change notifications, mode 2031) and on focus-in.
2. **The band.** When the probed background is on the theme's side, `bg` is that colour and `surface` is the
   band fitted to it: the smallest k in 1..=10,000 for which `mix(text, bg, k / 10,000)` stands at least
   `contrast(#252526, #1E1E1E)` (1.0886:1) off `bg`. `#1F1F1F` gets `#262626`, Ubuntu's `#300A24` gets `#38142C`
   and `#FFFFFF` gets `#F5F5F5`. Without a reply, or with a theme forced to the other side, there is no band.
3. **Corrected when it doesn't match.** When the probed background is more than ΔE_OK 0.03 from the theme's `bg`,
   for every token with a floor, keep its OKLCH hue and chroma and move its lightness
   away from the background in steps of 0.004 until it meets the floor. Then re-derive the blends from the real
   foreground (`OSC 10`, or the theme's `text`) and the real background:
   `sunken = mix(#000, bg, .35)` (dark) or `mix(fg, bg, .05)` (light);
   `line = mix(fg, bg, .24)` dark and `.30` light; `accent.tint` is the accent at `.22` dark and `.20` light;
   `warn.tint`, `diff.add` and `diff.del` are their colour at `.14` over bg; the word tints are `.26`;
   `orb.ring` is `fit(mix(fg, bg, .55), 3.0)`; and the other orb tones use the recipes in §2.2.
4. **The user's accent** (the app's accent picker): `accent = theme | ink | violet | blue | teal | rose | #RRGGBB`
   colours **the orb's core and the streaming caret only**, as in the app ("the accent colours the orb only;
   themes stay with design"). It is corrected to 4.5:1. A hue within 25° (OKLCH) of gold (93°), green (152°) or
   red (25°) is refused, and Wisp says why.

`preview.py` implements steps 1 and 3 (`resolve_tokens`), and step 2's fit through its port of `fitted_band`,
whenever the background is known, on either side; it keeps the table's 256 index for the band. On a white terminal it reports "tokens corrected for it".

---

## 3. Wordmark and logo

### 3.1 The one-line mark

```
⢎⠶⡰ wisp
```

| Cell | Char | Code point | Dots | Colour |
|---|---|---|---|---|
| 1 | `⢎` | U+288E | the ring's left half: dots 2 3 4 8 | `text` in the logo, `orb.ring` in the live orb |
| 2 | `⠶` | U+2836 | the core: dots 2 3 5 6 | `accent` (always) |
| 3 | `⡰` | U+2870 | the ring's right half, **without its top dot**: dots 5 6 7 | as cell 1 |
| 4 | space | | | |
| 5–8 | `wisp` | | | `text`, **bold** |

- **The gap is the logo's.** The full ring's right half is `⡱` (U+2871, dots 1 5 6 7). Leaving out dot 1 opens
  the ring at about one o'clock, where the logo and Halo's resting ring are open.
- **Logo or orb.** The same three cells are the **logo** where they stand for the brand rather than for Wisp's
  state (the fullscreen header, this document, an icon or a plain-text print), drawn in `text` and never
  animated, by the brand's rule. Everywhere else they are the orb at its smallest: the **live orb** at the bottom
  left (§6.3), and the three cells beside `wisp` in the narrow welcome card and the one-line banner. They stay
  still with no work; while the root waits for working agents, they share its comet (§6.3). Reduced motion holds
  them still.
- **Width:** eight cells, all East Asian width N, all single cell.
- **ASCII fallback:** `(*) wisp`, with `(` and `)` in `text` and `*` in `accent`.
- **An empty half** (during the startup reveal and the welcome opening) is a space (U+0020), **never U+2800**,
  so copied text stays clean.
- **Never:** recolour the core away from accent (except for the user's own accent in the live orb, §2.7), use the
  mark in gold except as the needs-you orb, add emoji, or set `wisp` in caps.

The banner line, printed once when Wisp starts:

```
 ⢎⠶⡰ wisp 0.1.0 · Jarvis · ~/Projects/Wisp
```

### 3.2 The splash (optional, fullscreen empty state)

Halo's "small cut": 18 ring lights 20° apart (two of them hidden in the gap), around a 12-dot core, on an
8 × 3-cell braille grid (16 × 12 dots). Ring centre (7.5, 5.5), radii 6.3 × 5.6 dots; the core fills the middle
row, so no seam runs through it.

```
╭──────────────────────────────────────────────────────────────────────────────╮
│                                                                              │
│    ⡐⠈ ⠁⠁     wisp                                                     0.1.0  │
│   ⠰  ⢾⡷  ⠆   Your own AI. With a computer of its own.                        │
│    ⠡⢀ ⡀⡀⠌    opus-5-5 · medium effort                                        │
│              ~/Projects/Wisp · Custom Gateway                                │
│                                                                              │
╰──────────────────────────────────────────────────────────────────────────────╯
  enter send · ctrl-j new line · /model · /help
```

| Row | Cells (columns 0–7) | Code points |
|---|---|---|
| 0 | ` ⡐⠈ ⠁⠁  ` | U+2850 U+2808, U+2801 U+2801 |
| 1 | `⠰  ⢾⡷  ⠆` | U+2830, core U+28BE U+2877, U+2806 |
| 2 | ` ⠡⢀ ⡀⡀⠌ ` | U+2821 U+2880, U+2840 U+2840 U+280C |

- **Colours:** ring cells `orb.ring`, core cells `accent`.
- **The calm card (the welcome panel).** The splash sits in a rounded `line` frame with one blank row of air above
  and below, and four lines three cells after the orb, in three tiers: `wisp` in bold `text` with the version
  right-aligned in `text.faint`; the brand's two halves, "Your own AI." in `text.muted` and "With a computer of
  its own." in bold `text`; what you talk to, model and effort in `text` with the word "effort" in `text.muted`;
  and where, the project path and the endpoint in `text.muted`. The tips leave the box: one key line under it,
  keys in `text`, meanings and dots in `text.muted` (`enter send · ctrl-j new line · /model · /help`; the longer
  list lives in `/help`). When the line is too narrow, whole keys drop from the one before the switch command backwards
  (`/help`, then the new-line key, then `enter`), so the switch command is always the last key left.
- **Detail stays one switch away.** The cache TTL, the log path, the web-search hint and the endpoint note are not
  in the default card; transcript mode (Ctrl-O) opens with the same card plus those rows in `text.muted`
  (`cache 1h · logging to ~/.local/…`, the web-search line, the endpoint note). A web-search hint that needs
  action stays in the default card as one short line, `web search off · ctrl-o for why`, or `web search off ·
  chat only · ctrl-o for why` when that leaves the session with no tools.
- **Widths:** the card with the splash from 60 columns, the same card with the three-cell orb from 44, the
  one-line banner (§3.1) below that, which keeps model, effort and endpoint and moves the same detail behind
  Ctrl-O. The card is at most 100 columns wide.
- **The comet** (§6.7) needs to know which lights each cell holds, with light k at 20k° clockwise from twelve
  o'clock:

  | Cell (col,row) | Lights | Cell | Lights |
  |---|---|---|---|
  | (4,0) | 0 | (5,0) | 1 |
  | (7,1) | 4, 5 | (6,2) | 6, 7 |
  | (5,2) | 8 | (4,2) | 9 |
  | (2,2) | 10 | (1,2) | 11, 12 |
  | (0,1) | 13, 14 | (1,0) | 15, 16 |
  | (2,0) | 17 | gap | 2, 3 (not drawn) |

- **The bit for each light** (to build partial cells): 0 (4,0) `0x01`, 1 (5,0) `0x01`, 4 (7,1) `0x02`,
  5 (7,1) `0x04`, 6 (6,2) `0x04`, 7 (6,2) `0x08`, 8 (5,2) `0x40`, 9 (4,2) `0x40`, 10 (2,2) `0x80`, 11 (1,2) `0x20`,
  12 (1,2) `0x01`, 13 (0,1) `0x20`, 14 (0,1) `0x10`, 15 (1,0) `0x40`, 16 (1,0) `0x10`, 17 (2,0) `0x08`. The
  character is U+2800 plus the OR of its bits.
- **ASCII splash:**

  ```
   .-''  .   wisp
  :  **  :   Your own AI. With a computer of its own.
   '-..-'    opus-5-5 - medium effort
  ```

### 3.3 Width check

Every character in the mark, the splash, the orb's frames and the glyph set (§5) was checked for its Unicode East
Asian width and its coverage in Cascadia Mono (Windows Terminal's default font), Consolas and DejaVu Sans Mono.
The preview's `widths` section measures the real advance in whatever terminal you run it in.

- **All braille (U+2800–28FF), `›` `−` `∙` `╎` `▸` `▾` `◉` `◌` `◦` `✓` `⨯`:** width N, one cell everywhere. All
  are in Cascadia Mono. Consolas lacks braille, `◉`, `✓` and `⨯`, and DejaVu Sans Mono lacks braille; terminals
  then use a fallback font, which still draws one cell.
- **Ambiguous (A): `·` `…` `→` `─` `│` `┃` `├` `└`.** One cell, except in terminals set to draw ambiguous
  characters wide (some CJK setups). In that case the ambiguous-wide set in §5.1 replaces them.
- **No emoji, no Nerd Font code points, and no emoji-presentation characters.** Specifically, never use `⏺` `⏸`
  `⏹` `✔` `✳` `▪` `▫` `◻` `◼` `↪` `▶` `◀` `‼` `⁉` `⚪` `⚫` `⭕`, which some fonts or terminals draw as colour
  emoji, two cells wide.
- **To measure it in a real terminal,** run `preview.py --section widths`. It writes each of the 52 glyphs and
  reads back the cursor column (`CSI 6 n`). Only a real terminal's answer counts: the preview's own test pty
  answers by counting code points, so it proves the code path, not a font.

---

## 4. Typography in a monospace grid

### 4.1 Weights and styles

| Style | SGR | Used for | Never for |
|---|---|---|---|
| Regular | — | Body text | |
| **Bold** | 1 | The wordmark, tool verbs, headings, the key phrase in an approval, the chosen option, the heavy half of a two-part heading | Whole paragraphs |
| *Italic* | 3 | Thinking text, Markdown emphasis, code comments | UI labels |
| Underline | 4 | Links only (with OSC 8 when supported) | Emphasis |
| Reverse | 7 | Selection and chips when there is no colour | Decoration |
| Strikethrough | 9 | Finished plan items (with `✓`) | Deleted code (use the `-` column) |
| Faint | 2 | **Never.** Every terminal draws it differently; use the tone tiers. Anything without the keys (the composer holding a draft, a card) steps down by tone (§7.5) | |

Terminals that can't draw italics (legacy conhost) keep the thinking colour and the rail, so nothing is lost.

### 4.2 Tiers and headings

- **Four tones, not more, each with its role:** `text` for what you read; `text.muted` for every status sentence
  (`Ready` too), facts and details; `text.faint` only for keys, hints and rails; `line` only for structure.
  `orb.off` is the offline orb only.
- **Two halves, the brand's signature,** for chrome titles: overlay and pane titles, and the splash.
  `Projects: **1 needs you**`, `Activity: **every request, tokens and cost**`. The lead half is `text.muted`, the
  heavy half bold `text`.
- **Markdown headings** in answers: H1 bold `text` with a blank line after it; H2 bold `text`; H3 bold
  `text.muted`. No `#` marks, no underlines and **no accent colour**: blue marks Wisp and things you can act on.
- **Sentence case** everywhere, including the brand's labels ("Show details", "Not now").

### 4.3 The grid

Columns are numbered from 0.

| Column | Holds |
|---|---|
| 0 | A margin space (in fullscreen, the selection band starts here) |
| 1 | The state glyph: `›` `⠶` `✓` `⨯` `◦` `◉` `╎` `┃`, or the first cell of the orb |
| 2 | A space |
| 3 onwards | Content: prose, tool verbs, notices |
| 5 onwards | Tool output and diff bodies, hanging under the verb (line numbers right-aligned in columns 5–8 for diffs) |

- **The transcript uses its whole area.** `width` here is the transcript's area: the terminal's width today, and
  less when the planned sidebar docks beside it (`Layout.width`, from `presenter.rs` `layout()`). Prose, thoughts,
  notes, code, diffs, tables and tool output all wrap at `width − 4` columns of content (columns 3 to `width − 2`),
  with no cap. A tool row's head (verb, target and facts) stops at the content edge; its output lines keep the full
  width.
  - **Why there was a cap.** Prose used to stop at a 100-column measure and facts and bands at column 119, for
    readability: lines near 100 characters are easier to follow back to the next line's start, and facts stayed
    near their subject. On a wide terminal that left the text stopping near the middle of the screen, so the owner
    asked for the whole width; how wide to read is the terminal's (and later the sidebar's) to decide.
- **The content edge.** In the transcript, right-hand facts (a code block's language, elapsed time, diffstat, a
  tool's state) are right-aligned at `width − 1`, and the user's band, a focused choice's selection band and diff
  row tints end there. Pickers, `ps`, question cards and `/config` are chrome and keep their own edge,
  `min(width − 1, 119)` (`CHROME_EDGE`, from `WIDE_FROM`): the screen edge up to 120 columns, then no further out, and their
  selection bands end at the same column. `WIDE_FROM` (120) also docks the sidebar, splits `/help` into two columns
  and widens `/config` and `/usage`, so moving one moves all. The bottom frame uses the full width: its sides sit at
  columns 0 and `width − 1`, and the text inside ends at `width − 3`.
- **Tables** render as a grid whenever they fit:
  - **Natural fit:** a grid at each column's natural width when the widest cells and the 2-column gaps fit the
    full width.
  - **Wrapped cells (above 80 columns only):** otherwise each column gets a floor of its longest word, clamped to
    12–24 columns (never more than its natural width). If the floors and gaps fit, the space is shared max-min
    fairly by a level `L`: each column gets `max(floor, min(natural, L))`, with `L` the largest level whose total
    fits. Any columns left over go one each, leftmost first, to the columns still growing at that level (floor
    `≤ L <` natural). Words wrap inside each cell, a word longer than its column breaks, and every wrapped line
    keeps the column's alignment marker (left, centre, right). Wrapped lines are separate rows with no joins and no
    trailing spaces, so copying stays clean. The rule spans the grid.
  - **Records:** when even the floors can't fit, when the gaps alone fill the width, or at 80 columns and below,
    the table becomes key/value records.
- **Tool verbs** pad to the longest verb in their group plus two spaces, capped at 10 columns (`Searched` makes
  10), so targets line up.
- **When something must give,** in this order: drop the right-hand facts to their shortest form (`0:12`, then
  nothing); shorten a path from the middle and keep the file name (`crates/…/cache.rs`); wrap prose; never wrap a
  tool row.
- **Vertical rhythm:** one blank line between the turn's blocks (the user message, thinking, prose, a tool group,
  the turn footer) and between turns. **No** blank lines inside a tool group, with or without output, or inside an
  approval.
- **Numbers:** running timers as `m:ss` (`0:07`, `12:30`); finished durations in words (`18 s`, `1 min 52 s`);
  cost to three significant figures (`$0.014`, `$0.21`, `$1.20`); percentages whole (`92%`); counts with their
  noun (`7 matches in 3 files`, `41 passed`). The separator is ` · ` (space, middle dot, space).

### 4.4 Rendering Markdown and code

| Element | Style |
|---|---|
| Paragraph | `text`, wrapped at the content width (`width − 4`); continuation lines indent to column 3 |
| List | `∙` or `1.` at column 3, the text at column 5, nested lists two columns further in |
| Quote | a `╎` rail in `text.faint` at column 3, the text in `text.muted` |
| Inline code | `text` on `surface` (fullscreen) or plain `text` (inline), never backticks |
| Code block | `sunken` band (fullscreen) or none (inline), syntax colours, the language in `text.faint` right-aligned on the first row, **no side borders**, so copied code is clean |
| Link | `accent`, underlined, OSC 8 when supported; otherwise the URL is shown |
| Table | header bold, a single `─` rule below it in `line`, no vertical borders; words wrap inside cells when the natural width is too wide (§4.3); key/value records when even wrapped columns can't fit, or at 80 columns and below |
| Rule | `─` in `line`, across the content width (`width − 4`) |

**Code colours.** In shell, command names and `[[`/`]]` show as `syn.function` and options such as `-l` as
`syn.variable`; in Rust, `self` in `&self` and attribute names (`derive`) are `syn.variable` while `self.` in a body
stays `syn.keyword`; format placeholders inside strings (`{}`, `%s`), custom Python decorators (`@cache_it`) and
`ALL_CAPS` names show as `syn.variable`, while builtin decorators such as `@property` show as `syn.function`.
Dockerfile instructions (`FROM`, `RUN`, `AS`) stay `syn.keyword`, and a stage name is `syn.variable`. In shell's `$f`
and `${f}` only the name is `syn.variable`; `$`, `${` and `}` keep the terminal's colour. Rust path calls such as
`Duration::from_secs` and `Vec::new` keep the terminal's colour, since the grammar gives them no function scope.

**Copy-clean.** Trailing spaces are trimmed. No box characters sit beside text that someone would copy; the composer
box's frame, prompt and padding are never part of a selection or a copy, except a space that hangs in the padding
column at a wrapped row's edge, which is input and is selected, tinted and copied like the rest of the draft. A user
cell's tags are left out of a copy and of transcript search. The run of spaces that right-aligns the welcome card's
version copies as one space (`wisp 0.0.0`). The streaming caret is never written to scrollback. In inline mode, bands
are drawn with explicit spaces in the band colour, never with "erase to end of line" (terminals differ on whether
erase uses the current background).

---

## 5. The glyph set

### 5.1 Glyphs

| Meaning | Unicode | Code point | Width | Ambiguous-wide set | ASCII | Colour | Key |
|---|---|---|---|---|---|---|---|
| Composer prompt | `›` | U+203A | N | `›` | `>` | `accent`; `text.faint` without the keys | `prompt` |
| Composer box, welcome card, sidebar sections | `╭` `╮` `╰` `╯` `─` `│` `├` `┤` | U+256D, U+256E, U+2570, U+256F, U+2500, U+2502, U+251C, U+2524 | **A** | `+` `+` `+` `+` `-` `\|` `+` `+` | same as ambiguous-wide | `line` | |
| Current (model, endpoint), after the name | `✓` | U+2713 | N | `✓` | `+` | `accent` | |
| An account's mark; a gone account's | `●`; `○` | U+25CF; U+25CB | **A** | `*`; `o` | `*`; `o` | `mark.1` to `mark.6`; `mark.gone` | `mark`; `hollow` |
| User turn | `›` | U+203A | N | `›` | `>` | `user` | `user` |
| Wisp turn, Jarvis, an agent at work (the core) | `⠶` | U+2836 | N | `⠶` | `*` | `accent` or the role colour | `wisp` |
| Streaming caret | `⠶` | U+2836 | N | `⠶` | `_` | the orb's core colour (§6.5) | `caret` |
| Tool running (comet) | `⠃⠉⠘⠰⢠⣀⡄⠆` | U+2803… | N | same | `-\\|/` | `accent` | `comet` |
| Running, reduced motion | `◌` | U+25CC | N | `◌` | `o` | `accent` | `running` |
| Done | `✓` | U+2713 | N | `✓` | `+` | `ok` | `done` |
| Failed | `⨯` | U+2A2F | N | `⨯` | `x` | `error` | `failed` |
| Waiting or queued | `◦` | U+25E6 | N | `◦` | `.` | `text.faint` | `waiting` |
| Stopped | `◦` | U+25E6 | N | `◦` | `-` | `text.muted`, with the word "Stopped" | `stopped` |
| Needs you | `◉` | U+25C9 | N | `◉` | `!` | `warn.glyph` | `needs_you` |
| Moved to the background (a tool row whose command keeps running as a process) | `⇗` | U+21D7 | N | `⇗` | `^` | `accent` | `moved` |
| A turn Wisp started because background processes ended (the wake-turn row) | `↻` | U+21BB | N | `↻` | `~` | `text.muted` | `woke` |
| Thinking rail, quote rail | `╎` | U+254E | N | `╎` | `:` | `text.faint` | `rail` |
| Approval rail | `┃` | U+2503 | **A** | `\|` | `\|` | `warn.glyph` | `approval_rail` |
| Collapsed, expanded (fullscreen); a folded sidebar section, `▸` after its title | `▸` `▾` | U+25B8, U+25BE | N | same | `>` `v` | `text.faint` | `collapsed` `expanded` |
| A newline in typed text (a process row's quoted input) | `⏎` | U+23CE | N | `⏎` | `\n` | inherits | `newline` |
| Separator | `·` | U+00B7 | **A** | `∙` U+2219 | `-` | inherits | `separator` |
| List bullet | `∙` | U+2219 | N | `∙` | `-` | `text.muted` | `bullet` |
| More, truncated | `…` | U+2026 | **A** | `...` | `...` | `text.faint` | `more` |
| Message between agents | `→` | U+2192 | **A** | `->` | `->` | `text.faint` | `arrow` |
| Minus in a diffstat | `−` | U+2212 | N | `−` | `-` | `error` | `minus` |
| Tree | `├─` `└─` `│ ` | U+251C, U+2514, U+2500, U+2502 | **A** | `\|-` `` `- `` `\| ` | same as ambiguous-wide | `line` | `tee` `elbow` `pipe` |
| Spark (a sidebar `spark` block), two values per cell | `⢀` or `⣀`…`⣿` | U+2880, U+28C0… | N | same | `.:-=+*#`, one value per cell | the block's tone | `spark` |
| Bar and grid cell: used, free | `⣿` `⣀` | U+28FF, U+28C0 | N | same | `#` `.` | the bar's look, or the category's or part's colour; free `text.faint` (§7.9) | `cell_full` `cell_free` |
| Context reserve (hatched) | `⡪` | U+286A | N | `⡪` | `/` | `text.faint` (§7.9) | `cell_reserve` |
| No figure (a Context panel row with nothing to count) | `—` | U+2014 | **A** | `-` | `-` | `text.muted` | `absent` |

- **Diff signs** in diff bodies are always ASCII `+` and `-`, so a copied patch applies. The display-only diffstat
  uses `−`.
- **Separator.** Cascadia draws `∙` as a heavy bullet, so `·` is the default and `∙` is kept for the
  ambiguous-wide set.
- **The context report's reserve** `⡪` is the one glyph added for it: hatched, so it differs from free space
  by shape.
- **The spark** draws a sidebar `spark` block as braille columns, two values per cell with the newest on the
  right: the left column takes dots 7, 3, 2 and 1 from the bottom, the right column dots 8, 6, 5 and 4, and a
  value v of max m fills `1 + round(3 × v / m)` of them, so a zero still shows its bottom dot. For example, 0, 3,
  6 and 9 of 9 are `⣠⣾`. With an odd count the oldest value takes the first cell's right column alone, so
  0, 9 and 9 of 9 are `⢀⣿`. The ASCII set draws one value per cell as `spark[round(6 × v / m)]`.
- **Bars** (a sidebar gauge, `/usage`, the status bar's usage, the account list), the Context box and
  `/context`'s grid draw the same whole cells, `⣿` used and `⣀` free: a value v of max m over n cells fills
  `⌊v × n / m⌋` cells, one when that is 0 and v is above 0, and all n only at the max. For example, over 8
  cells, 94% is `⣿⣿⣿⣿⣿⣿⣿⣀` and 64% is `⣿⣿⣿⣿⣿⣀⣀⣀`. A sidebar gauge's % is cut down to a whole number, as
  the Context box's is (2 of 3 reads 66%). Used cells take the bar's look; free cells are `text.faint`.

### 5.2 The frames

| Animation | Frames | Code points | Timing |
|---|---|---|---|
| Comet (tool running) | `⠃ ⠉ ⠘ ⠰ ⢠ ⣀ ⡄ ⠆` | U+2803 2809 2818 2830 28A0 28C0 2844 2806 | 150 ms each, a 1.2 s lap, clockwise |
| Comet, ASCII | `- \ \| /` | | 150 ms each |
| Orb thinking (§6.3) | `⢆⠶⠱` `⠎⠶⡰` `⢊⠶⡡` `⢌⠶⡑` | | 500 ms each |
| Orb done | `⠢⡠⠊` | U+2822 2860 280A | one shot (§6.3) |
| Orb at rest / closed | `⢎⠶⡰` / `⢎⠶⡱` | | |
| Orb, ASCII | `(*)`; thinking `(-)` `(\)` `(\|)` `(/)`; needs you `(!)`; done `(+)` | | as the Unicode orb |

The comet is one light and its tail moving around the eight dots of one braille cell: Halo's idle comet, made
small. Two dots are lit in each frame, the head and the one it just left.

### 5.3 Fonts

Glyph shapes were checked in Cascadia Mono at 18–44 px (rendered in `preview.py`'s development, not shipped).
Braille dots tile cleanly across a row. Across rows, terminals with a line height above 1.0 leave a seam, which is
why the splash keeps its core inside one row. Wisp doesn't tell anyone which font to use: every glyph falls back
safely.

---

## 6. Motion

### 6.1 Principles

1. **Still without work.** When no turn or agent is working, the status orb, welcome orb and splash hold
   their still poses, with no decorative timer writes. While the root waits for working agents, their existing
   comet and core breath continue (§6.3), including the chat mark beside "Waiting for reports" (§6.5).
2. **Purposeful.** Motion means work is happening, or something just changed. It always comes with words.
3. **One thing moves at a time, while working.** During a turn at most one element changes *shape* at once: the
   comet, the thinking arcs, the reveal or a message pulse. Everything else may only change *brightness*. While a
   tool row's comet runs, the orb only pulses. While waiting for working agents, the status orb and welcome orb
   run the same comet on the same clock, so they read as one light. During a turn and while agents work, the
   chat mark (§6.5) is the status orb's frame; while only agents keep Wisp waiting it says "Waiting for reports".
4. **Never in scrollback.** Only the live region (inline) or the visible screen (fullscreen) animates. Whatever
   is committed to scrollback is its final frame.
5. **Never blocking.** Animations never delay input or output. A key press always lands; the startup reveal and
   the splash give way to the first key.
6. **The brand's tempos.** Durations and curves come from the orb engine and the Medium proposal, sampled on a
   grid that suits a terminal (§6.2).
7. **The same whether seen or not.** Focus never changes motion: the waiting-for-agents comet and every working
   animation run whether or not the window is focused. Only the motion setting and real terminal limits keep
   the orb still.
   Focus reports (mode 1004) are used only to ring the bell for a question while the window is unfocused.

**Motion settings** (the brand's three, with the same names):

| Setting | Loops | Transitions | Selected by |
|---|---|---|---|
| **Full** (default) | as specified | as specified | |
| **Calm** | twice as slow and half as deep; the beat keeps its rhythm but plays once | as specified | `WISP_MOTION=calm`, or `motion = "calm"` |
| **Reduced** | none: each state shows its still pose (§6.3) | instant | `WISP_MOTION=reduced`; the OS setting (macOS Reduce motion, Windows "Show animations" off, GNOME `enable-animations=false`); `TERM=dumb`; output that isn't a terminal; full repaint (`WISP_FULL_REPAINT=1`) |

The OS setting always wins over Full and Calm, as in the app.

### 6.2 Clock, sampling, curves

- **One clock.** Every animation is a pure function of `(state, t_ms since the state began)`, plus a shared
  app-wide clock for things that must stay in phase (the tree's pulse, the beat).
- **Sampling grids.** An animation's colour or frame may change only on its grid. Between grid points the output
  is identical, so nothing is written.

  | Animation | Grid |
  |---|---|
  | Brightness loops: sparkle, breath, pulse, splash | 100 ms |
  | Thinking arcs | 500 ms |
  | Comet | 150 ms |
  | Needs-you beat | 40 ms (only during the 768 ms swell of each 1.6 s cycle) |
  | Startup reveal (banner, narrow card) | 40 ms |
  | Welcome opening (the comet's lap) | 100 ms (200 ms in Calm) |
  | Notification swell | 50 ms |
  | Message pulse | 70 ms |
  | Offline breath | 250 ms |
  | Waiting-for-agents comet and core breath | 100 ms (200 ms in Calm) |
  | Timers | 1000 ms |

- **The frame cap is 30 a second** for the whole app (at least 33 ms between writes), and changes are coalesced.
  Measured in `preview.py` (averages over each demo, holds included), in frames a second:

  | Demo | Rate |
  |---|---|
  | The orb's states | 7.2 |
  | The banner reveal | 5.0 |
  | The splash | 10.1 |
  | The welcome opening (B2) | 10 for its 1.2 s, then still unless agents are working |
  | Waiting, then streaming | 7.0 |
  | Tool rows | 10.2 |
  | Notifications | 8.5 |
  | The tree, while a message pulse overlaps its 2 s pulse | 17.4 |
  | Waiting for working agents | at most 10 (the orb's cells only) |
  | Idle with no work | 0 |

- **Curves** (CSS `cubic-bezier`):

  | Name | Curve | Used for |
  |---|---|---|
  | `ease-in-out` | (.42, 0, .58, 1) | Loops and sparkles |
  | `ease-out` | (0, 0, .58, 1) | Decays |
  | `morph` | (.45, 0, .2, 1) | State changes: the orb engine's own |
  | `beat` | (.33, 0, .3, 1) | The needs-you beat, between its keyframes |
  | `wave(t, P)` | 0.5 − 0.5·cos(2π·t/P) | Breaths and pulses: 0, then 1 at P/2, then 0 |

- **Colour.** An animated colour is always `mix(a, b, u)` between two tokens (§2.2), computed in sRGB from the
  *resolved* tokens, so runtime correction carries through. In ANSI-256 the result maps to the nearest index; in
  ANSI-16 and `NO_COLOR` the animation holds its still pose instead.
- **The state morph.** When the orb changes state, its shape switches at once (braille can't interpolate), and
  its colours blend from the old pose to the new one in three 80 ms steps: `u` = 0.401, 0.919 and 1.0 (the
  `morph` curve at 1/3, 2/3 and 1). Each cell is `mix(new, old, u)`. In Reduced the change is instant.

### 6.3 The presence orb (the signature animation)

**Where it lives.** At columns 2–4 of the status row inside the bottom frame (columns 1–3 when the status is a
bare row), in the footer of the live region (inline) or the screen
(fullscreen). It is the docked orb from the storyboards' Code tab. There is exactly one; the chat mark (§6.5) draws its frame again.

**Geometry.** A 6 × 4 dot grid over three cells. The ring's eight lights, R0–R7, run clockwise from the top left;
the core is the middle cell.

| Light | Dot (x, y) | Cell | Bit | Light | Dot (x, y) | Cell | Bit |
|---|---|---|---|---|---|---|---|
| R0 | (1,0) | left | 0x08 | R4 | (4,3) | right | 0x40 |
| R1 | (4,0) | right | 0x01 | R5 | (1,3) | left | 0x80 |
| R2 | (5,1) | right | 0x10 | R6 | (0,2) | left | 0x04 |
| R3 | (5,2) | right | 0x20 | R7 | (0,1) | left | 0x02 |

The core is U+2836 `⠶` (bits 0x36) in the middle cell. A ring half is U+2800 plus the OR of the bits of its lit
lights, or a space when none is lit. The rest pose leaves out R1 (the logo's gap).

**Poses** (L, M and R are the three cells; the colours are §2.2 tokens):

| State | Brand state | Cells | Ring colour | Core colour | Moves |
|---|---|---|---|---|---|
| **Ready** | Idle | `⢎⠶⡰` | `orb.ring` | `accent` | Still with no work; comet and core breath while waiting for working agents |
| **Listening** | Listening, You're speaking | `⢎⠶⡱` (ring closed) | `orb.ring.listen` | `orb.core.dim` | Nothing. Shown while the composer has text |
| **Thinking** | Thinking | `⢆⠶⠱` `⠎⠶⡰` `⢊⠶⡡` `⢌⠶⡑` | `text` | sparkle | Two arcs turn clockwise; the core sparkles |
| **Answering** | Speaking | `⢎⠶⡰` | `orb.ring.speak` | chunk light, then breath | The core |
| **Working** | Working | `⢎⠶⡰`, or a progress fill | pulse | `accent` | The ring's brightness |
| **Needs you** | Needs you | `⢎⠶⡱`, all three cells gold | beat | beat | Brightness: the beat |
| **Done** | Done (one shot) | `⠢⡠⠊` (the ring redrawn as a check) | `ok` with a glint | — | The glint, once |
| **Offline** | Offline | `⢎⠶⡰`, all cells grey | breath | breath | Brightness |
| **Stopped** | — | `⢎⠶⡰` | `orb.ring` | `orb.core.dim` for 300 ms | Once |

**Waiting for working agents.** When the root rests in Ready but at least one child is busy, the comet and
core breath start after the 240 ms morph and continue for as long as that work lasts, focused or not. With no
working agent, Ready stays on its still pose and schedules no decorative updates:

- **The comet.** A light laps the ring clockwise every 6000 ms (Calm 12 000 ms), starting at R2, the end of the
  gap: R2 R3 R4 R5 R6 R7 R0, then R1, the gap itself, where it passes out of sight. It spends 750 ms on each light.
  A braille cell has one colour, so the comet is drawn by **shape and colour together**: the half that holds the
  head draws only the head and the light behind it, brightly, and the other half keeps its rest dots in
  `orb.ring`. One lap, sampled every 100 ms (the shape changes on the first sample of each light):

  | Time (ms) | Head | Cells |
  |---|---|---|
  | 0 | R2 | `⢎⠶⠐` |
  | 800 | R3 | `⢎⠶⠰` |
  | 1500 | R4 | `⢎⠶⡠` |
  | 2300 | R5 | `⢀⠶⡰` |
  | 3000 | R6 | `⢄⠶⡰` |
  | 3800 | R7 | `⠆⠶⡰` |
  | 4500 | R0 | `⠊⠶⡰` |
  | 5300 | R1 (the gap) | `⢎⠶⡰` |

- **Its glow.** Each light's opacity follows the brand's comet keyframes (§6.7): 1.0 when the head reaches it, .8
  after 6% of the tail and .28 from 18% on, with the tail as long as the app's 18-light comet (so the head
  stays bright for its whole 750 ms). A cell takes the highest glow `g = (op − .28) / .72` of the lights it
  draws, as `mix(text, orb.ring, g × amp)`. Without colour blends (ANSI-16, `NO_COLOR`) a cell is `text` when
  `g ≥ .5` and `orb.ring` otherwise, so the shape still turns.
- **The core breath.** `mix(orb.core.dim, accent, amp × wave(t, 6000))`: full depth, from `accent` down to
  `orb.core.dim` at 3 s and back, in phase with the comet. Calm doubles the period and halves the depth. No
  breath without colour blends.
- **The welcome orb** runs the same clock while the welcome panel is on screen: the splash's 16 lights take the
  §6.7 comet (`d_k = ((k − 4) mod 18) / 18 × lap`) with the same glow, and its two core cells breathe with the
  status orb. In a narrow welcome or banner, the three cells beside `wisp` are the orb at its smallest and run
  the status orb's frames; reduced motion holds them still.
- **The chat mark** (§6.5) runs the status orb's frames while agents work, seated beside it and patched with it;
  its words "Waiting for reports" are drawn once and never rewritten.
- **Writes.** Only the orbs' own cells (the status orb, the welcome orb, the chat mark) are rewritten, without
  re-rendering the screen: at most 10 frames a second (5 in Calm), each a few cells. A frame is written only when a
  sampled cell changes, so in ANSI-256, where neighbouring blends share an index, fewer frames go out.
- **Stays still** in Reduced (the rest pose), with ASCII glyphs (`(*)`: an ASCII spin would read as Thinking),
  with full repaints (`WISP_FULL_REPAINT=1`, since each step would rewrite the whole screen), and when the status
  row has no room for the orb. It also stays still when no child is busy. It pauses while the composer has text,
  a hint or toast shows, a picker or transcript mode is open, a selection is in progress, or Wisp is suspended.
  The welcome orb never moves once it has scrolled out of view.

The frame tables follow. Full motion unless noted. Calm doubles every period and halves every depth (`× amp`,
amp = 0.5). "Still pose" is what Reduced, ANSI-16 and `NO_COLOR` show.

**Thinking**
- **Ring.** The frame is `F[floor(t / 500) mod 4]`: 0–499 ms `⢆⠶⠱` (gaps at R0 and R4), 500–999 `⠎⠶⡰` (R1, R5),
  1000–1499 `⢊⠶⡡` (R2, R6), 1500–1999 `⢌⠶⡑` (R3, R7). The ring is `text`. This is "the ring splits into turning
  arcs": two arcs, because eight lights can't make the brand's three, turning a half revolution every 2 s.
- **Core sparkle.** Period 1600 ms, sampled every 100 ms: `core = mix(accent, orb.core.dim, s × amp)`, where s at
  0, 100, 200 … 800 ms is 0, .129, .5, .871, **1.0**, .871, .5, .129, 0, and stays 0 until 1600.
- **Still pose:** `⢆⠶⠱`, ring `text`, core `orb.core.dim`. ASCII: `(-)` `(\)` `(|)` `(/)` at the same 500 ms
  steps, with the core character turning: "churning, inward".

**Answering**
- **Ring.** `⢎⠶⡰` in `orb.ring.speak`.
- **Core.** It follows the stream. Each time a chunk of text is drawn, the core lights fully and then decays:
  `core = mix(accent, orb.core.dim, s)` with s = 1.0, .622, .315, .093 and 0 at 0, 100, 200, 300 and 400 ms after
  the chunk (ease-out). If no chunk arrives for 600 ms, a breath fills the pause: `s = wave(t − 600, 4000) × amp`,
  rising from 0 to 1 at 2 s and back by 4 s. "Voice leads; a slow breath fills the pauses."
- **Before the first chunk.** Answering can begin (on `BlockStarted{Text}`) before any text is drawn. Until the
  first chunk, the core stays at `orb.core.dim` (s = 0), and if none arrives within 600 ms of the state's start,
  the same breath begins: `s = wave(t − 600, 4000) × amp`. The first chunk then lights the core as above.
- **Still pose:** core `accent`.

**Working**
- **Unknown progress.** `⢎⠶⡰`, with `ring = mix(text, orb.ring, wave(t, 2000) × amp)`, sampled every 100 ms: 0,
  .024, .095, .206, .345, .5, .655, .794, .905, .976, then 1.0 at 1000 ms, then back. The core stays `accent`.
- **Known progress** p (0–1), when a tool reports it: lights R0 … R(n−1) with `n = max(1, ceil(8p))`, drawn in
  `text`, the rest unlit; no pulse. "The ring fills with real progress", clockwise from the top.
- **Still pose:** `⢎⠶⡰`, ring `orb.ring`.

**Needs you** (the reserved signal)
- **Cells.** `⢎⠶⡱`, all three in `mix(warn.glyph, bg, op)`.
- **The beat.** Keyframes (ms → op): 0 → .5, 176 → 1.0, 352 → .6, 528 → .95, 768 → .5, 1600 → .5, eased with
  `beat` between them. Sampled every 40 ms, the swell is: .5, .606, .835, .956, **.997**, .972, .798, .664, .61,
  .602, .708, .858, .928, **.949**, .92, .785, .64, .555, .514, .5. Then it rests at .5 until 1600 ms.
- **Repeats.** Full plays 3 cycles (4.8 s), then holds at op 1.0. Calm plays 1 cycle. Reduced holds at 1.0 from
  the start. It plays again when another item starts needing you. **Gold
  never appears in any other state.**

**Done** (one shot, when a turn completes)

| Time (ms) | Left | Middle | Right |
|---|---|---|---|
| 0–119 | `⠢` `mix(ok, bg, .5)` | `⡠` same | `⠊` same |
| 120–399 | `ok` | `ok` | `ok` |
| 400–599 | **glint**: `mix(ok, text, .45)` | `ok` | `ok` |
| 600–799 | `ok` | **glint** | `ok` |
| 800–999 | `ok` | `ok` | **glint** |
| 1000–2199 | `ok` | `ok` | `ok` |
| 2200 | morph to Ready | | |

In Calm every time after the first 120 ms doubles (4.4 s in all). Reduced shows `⠢⡠⠊` in `ok` for 1200 ms, then
Ready, with no glint. ASCII: `(+)` in `ok`.

**Offline**
- **Cells.** `⢎⠶⡰`, all in `mix(orb.off, bg, op)`, with `op = 1 − 0.5 × wave(t, 5000) × amp`, sampled every
  250 ms (1.0, .988, .952, .897, .827, .75, .673, .603, .548, .512, then .5 at 2500 ms, then back). "Asleep,
  reconnecting, not frozen."
- **Still pose:** op 1.0.

**Stopped** (the user cancelled): 0–299 ms `⢎⠶⡰` with ring `orb.ring` and core `orb.core.dim` ("the core
yields"), then a morph to Ready at 300 ms (Calm too: Stopped is a transition, not a loop). Reduced goes straight
to Ready.

**Settling into Ready.** Done and Stopped end the same way: at their settle time (Done 2200 ms, Calm 4400 ms,
Reduced 1200 ms; Stopped 300 ms, Reduced 0) the shape switches to Ready at once and the colours morph from the
last pose to Ready's in the §6.2 three 80 ms steps. Reduced, ANSI-16 and `NO_COLOR` cut instead of morphing.

**Which event drives which state** (today's `AgentEvent`s, then the ones M5–M8 will add):

| Event | Orb |
|---|---|
| The composer becomes non-empty, or empty | Listening, or Ready |
| `TurnStarted`, `Retrying`, `BlockStarted{Thinking}`, `ThinkingDelta` | Thinking |
| `BlockStarted{Text}`, `TextDelta` (each drawn chunk relights the core) | Answering |
| `BlockStarted{ToolUse}`, until its result | Working |
| An approval or stall decision waiting for the user (P8), or a needs-you notification | Needs you, which **outranks** every state but Offline |
| `TurnEnded{Completed}` | Done, then Ready |
| `TurnEnded{Cancelled}` | Stopped, then Ready |
| `TurnEnded{Error}` | Ready. The error is in words, not the orb (§8.5) |
| The connection to `wispd` is lost | Offline |

### 6.4 Waiting for the first token

From the moment the user presses Enter:

| Time | Orb | Status (after the orb) | Transcript |
|---|---|---|---|
| 0 | Thinking (morph 240 ms) | empty | Nothing until Wisp starts on the message |
| 300 ms | | `Thinking` in `thinking` | |
| 2000 ms | | `Thinking · 0:02` (the timer in `text.muted`, every 1000 ms) | |
| `Retrying` | | `Anthropic didn't answer. Retrying in 8 s` | |
| Thinking text arrives | | | The whole thinking block streams (§7.1) above the chat mark |
| First answer token | Answering | `Answering · esc to stop` | The answer takes the chat mark's place and streams with the caret (§6.5) |
| First tool call | Working | `Running cargo test · 0:03` | A tool row with the comet (§6.6) |
| `TurnEnded` | Done (2.2 s), then Ready | `Done · 18 s · cache 92%`, then `Ready` | The turn footer is committed |

Nothing changes in the first 300 ms other than the orb, so a fast answer doesn't flash a label; during a turn the
chat mark is the orb, not a label, and lands with the message. The status row never jumps: it is always one row.

### 6.5 The streaming caret

- **Glyph.** `⠶` (ASCII `_`): **the core itself**, leading the words. It sits in the cell after the last
  character drawn, with one space before it unless the text already ends in a space.
- **Colour.** Always the orb's current core colour (§6.3, Answering), so the caret and the orb light and breathe
  together as one element.
- **Lifetime.** It exists only in the live tail. It is removed in the same frame as the block's text is finalised
  or committed, so it **never reaches scrollback** or a copy.
- **Still pose:** the caret in `accent`, without the breath.

**The chat mark.** While a turn runs or agents work, the status orb's frame is drawn a second time at the bottom
of the chat.
- **Its place:** a blank row and the mark's row, the shape of a new message; the mark's cells are columns 0 to 2, so
  its core is in the column of the reply's `⠶`. New rows land above it.
- **The rule:** the place opens only in a frame where a row lands while a turn runs, an agent works or mail that
  wakes Wisp waits, and leaves only when a row takes it over: the reply's first row, a question's card, or a row
  with its own blank, such as the turn's footer. So the chat moves only when a row lands, and never down for the
  mark.
- **What it shows:** the status orb's frame at the same instant, whatever its pose; nothing while the caret or a
  question shows. Main chat only, with a gutter (8 columns and up) and where the status row is drawn.
- **Its words:** while no turn runs and no waking mail waits, so only agents keep Wisp waiting, `ORB_GAP` cells
  and "Waiting for reports" in `text.muted` follow the frame, cut with `…` like the status row's words where the
  chat is too narrow. During a turn, and from the moment a report that wakes Wisp lands, the mark has no words; the
  status row says what Wisp is doing. Background processes never show the mark.
- **Never stored:** never in the transcript, a copy, the reader or the log.

### 6.6 The tool spinner, and success, failure and cancel

A tool row while it runs, and the moment it ends:

```
 ⠘ Writing a command                                                     14s
 ⠘ Run the wisp-core tests  cargo test -p wisp-core                        7s
 ✓ Run the wisp-core tests  cargo test -p wisp-core              exit 0 · 12s
 ⨯ Lint the workspace  cargo clippy --workspace                 exit 101 · 2s
 ◦ Build the release binary  cargo build --release               stopped · 1s
```

| Phase | Glyph | Colour | Verb | Right-hand facts |
|---|---|---|---|---|
| Writing (the model is still writing the call) | the comet, as running | `text.muted` | `Writing a command` (the tool's phrase) | elapsed, `text.muted`, every 1 s |
| Running | the comet, frame `floor(t / 150) mod 8` (Calm 300 ms) | `accent` | present: `Running`, `Reading` | elapsed `m:ss`, `text.muted`, every 1 s |
| Succeeded | `✓` | `mix(ok, text, .5)` for 0–119 ms (a glint), then `ok` | past: `Ran`, `Read` | the result: `41 passed · 0:12` |
| Failed | `⨯` | `error` at once | past | the state in `error`, then the result: `exit 101 · 2s`, `couldn't start`, `failed · 2s` |
| Stopped | `◦` | `text.muted` at once | past | the state in `text.muted`: `stopped · 1s`, `timed out after 2m 00s` |
| Interrupted (replayed from a resumed log: Wisp stopped while the call ran, so it has no end) | `◦` | `text.muted` | past | the state in `text.muted`, then why: `interrupted · Wisp stopped while it ran`, with no clock |

- **Output.** Nothing is drawn under a tool row, running or done: its output is in the reader, which a single
  click on the row opens (`docs/commands.md`, "Folded rows, the ctrl-o picker and the reader"). The verb column above is a made
  title's verb; a call the tool titled shows its title instead, with no verb (§8.3).
- **Reduced motion:** the running glyph is a still `◌`. ASCII: `-\|/` at 150 ms; reduced `o`.
- **Not used:** shaking, red flashes, or confetti for success. A failure is just a different still glyph and a
  sentence.

### 6.7 The startup reveal

**Inline** (the banner line, §3.1). The text appears at once; the orb assembles in 280 ms:

| Time (ms) | Left | Middle (core) | Right |
|---|---|---|---|
| 0 | ` ` | `⠶` `orb.core.dim` | ` ` |
| 40 | ` ` | `⠶` `mix(accent, orb.core.dim, 40/280)` | `⠐` (R2) |
| 80 | ` ` | … | `⠰` (+R3) |
| 120 | ` ` | … | `⡰` (+R4) |
| 160 | `⢀` (R5) | … | `⡰` |
| 200 | `⢄` (+R6) | … | `⡰` |
| 240 | `⢆` (+R7) | … | `⡰` |
| 280 | `⢎` (+R0) | `⠶` `accent` | `⡰` |

The ring is `text` (this is the logo). The lights arrive clockwise from the end of the gap. The core's colour at
each 40 ms step is `mix(accent, orb.core.dim, t/280)`. Then the line is committed to scrollback as its final
frame. Any key jumps straight to that final frame, and **typing works from frame 0**. Reduced: the final frame
only.

**Fullscreen** (the splash, §3.2, shown while the transcript is empty). The opening is **B2, the comet draws
the ring**: one fast lap in which the comet's head runs round the ring and every light it passes stays lit, so
the orb is drawn by the same light that marks working agents when the root waits.

1. **The lap, 0–1199 ms (Calm 0–2399).** Sampled every 100 ms (200 in Calm), so at most 10 frames a second. With
   `u = t / 1200` and each light's place `p_k = (k − 4) mod 18` (light 4, at the end of the gap, is place 0), the
   head runs exactly one lap of 18 places, `h(u) = h₁ − 18 + 32.4·u − 14.4·u²`, where `h₁ = 18 × 1200 / 6000 = 3.6`
   is the waiting comet's head at 1200 ms. The curve starts fast and slows to that comet's own speed
   (3.6 places per 1200 ms) at the hand-off. A light is drawn once the head has passed it,
   `(p_k − (h₁ − 18)) mod 18 ≤ h(u) − (h₁ − 18)`, and stays drawn. A drawn light's glow is the waiting comet's
   (§6.3), `comet_glow((h(u) − p_k) / 18)`, so the head is `text` and the tail fades behind it into `orb.ring`.
   Undrawn halves are spaces. The lap starts just after light 7, low on the right, rather than at the gap's end,
   because that is the start that ends one lap exactly on the waiting comet's head without moving the clock
   shared with the status orb.
2. **The core** fades in under it: `mix(core, bg, (t + 100) / 1300)`, where `core` follows the waiting core breath
   at that moment (§6.3), so at 1200 ms it matches that core exactly.
3. **The words** fade in under the lap: text row `i` of the card starts at `min(300 + 100·i, 900)` ms and takes
   three 100 ms steps, `mix(tone, bg, 1/3, 2/3, 1)`. The frame and the key line appear at once.
4. **The hand-off, 1200 ms (Calm 2400).** While agents work, the frame equals the waiting comet's frame at
   1200 ms: the same lights, glow and core carry on without a jump. With no work, the splash holds its rest frame.
5. **The waiting-for-agents comet, 6000 ms a lap (Calm 12000), only while agents are working** (§6.3).
   With no work, the splash stays on its still frame. For each ring light k, with its delay
   `d_k = ((k − 4) mod 18) / 18 × lap`, its opacity is the brand's comet keyframes (linear):
   `τ = ((t − d_k) mod lap) / lap`; op = 1.0 at τ = 0, .8 at .06, .28 from .18 onward. A cell shows the highest op
   of its lights, as `mix(text, orb.ring, g)` with `g = (op − .28) / .72`, sampled every 100 ms. The core
   breathes with the status orb (§6.3).
6. **Rest.** With no work, or while the root works, the splash rests: ring `orb.ring`, core `accent`. It scrolls
   away with the transcript, and never moves once it is out of view: a panel that leaves the screen during the
   lap skips to its final frame and writes nothing more.

Any key or click skips to the final frame, and typing works from the first frame. Reduced motion shows the final
frame at once. ASCII shows the ASCII splash, still. ANSI-16 and `NO_COLOR` keep the shapes: the head draws the
ring in `text` and the rest in `orb.ring`, with no fades on the ring, the core or the words. The narrower
fallbacks keep the three-cell reveal above: the card with the three-cell orb (44–59 columns) and the one-line
banner (below 44).

The opening is one named choice in `wisp-render` (`chrome::OPENING`, today `Opening::Comet`), which draws the art
and says when it next changes, so a different opening can replace it without touching the card's layout.

### 6.8 The notification pulse (proactive messages)

Jarvis speaks up on its own: routine results, follow-ups and reminders.

- **Informational** (a routine ran, a follow-up is ready). The notice is printed (§7.7), and the notice's `⠶` and
  the orb's core make **one swell**, sampled every 50 ms. With `glint = mix(accent, text, .5)`:
  - 0–199 ms: `mix(glint, accent, ease-in-out(t/200))`, rising;
  - 200–799 ms: `mix(accent, glint, ease-out((t − 200)/600))`, back to `accent`.
  That's all: no repeat and no sound.
- **Needs you** (a decision is waiting). The orb goes to Needs you: the beat, three times, then still gold. The
  notice's `◉` beats in phase, and the footer shows `1 needs you · ctrl-y`.
- **When the terminal is unfocused**, send one desktop notification (`OSC 9`, `OSC 777` or `OSC 99`, whichever the
  terminal supports, otherwise BEL) saying only `Wisp: 1 thing needs you` or `Wisp: a routine finished`. Never
  the contents, as the brand's pushes don't carry them. Never send one while the terminal is focused.
- **Reduced:** no swell and no beat; still `accent` or gold.

### 6.9 The agent tree

- **Working agents.** `⠶` in their role colour. All working rows share **one** 2 s pulse on the app clock:
  `mix(role, bg, 1 − 0.4 × wave(t, 2000) × amp)`, sampled every 100 ms, so the tree breathes as one element
  rather than flickering row by row.
- **Needs you.** `◉` with the beat (§6.3), in phase with the orb.
- **Waiting, done, failed.** Still: `◦` `text.faint`, `✓` `ok`, `⨯` `error`.
- **A message between agents** travels the tree's own lines as a small light:
  - **Path.** The sender's glyph, then down (or up) the parent's rail column, then across the branch to the
    recipient's glyph. Messages run top to bottom through the coordinator (D3), so the path is always
    parent ↔ child.
  - **Head and tail.** The head moves one cell every 70 ms (Calm 140 ms) and is `accent`. The two cells behind it
    are `mix(accent, line, .5)` and `mix(accent, line, .25)`. Every other cell keeps its usual colour.
  - **Arrival.** When the head passes the recipient's glyph, that glyph shows `mix(role, text, .5)` for 200 ms.
  - **Summary.** A row appears under the recipient: `coder → coordinator  "patch ready, 3 files"`, the names in
    `text.faint` and the summary in `text.muted`. It stays for 8 s, then folds into the agent's last-message
    column.
  - **Queueing.** One pulse at a time. Others queue; past three queued, they merge into
    `→ 4 messages · ctrl-o`.
- **Reduced:** no travel and no swell; the summary row appears at once.

### 6.10 How each animation degrades

| Animation | Reduced | Calm | ANSI-256 | ANSI-16 or `NO_COLOR` | ASCII |
|---|---|---|---|---|---|
| Orb: Thinking | Still `⢆⠶⠱` | 1000 ms steps, sparkle 3.2 s at half depth | as Full | Arcs turn; no sparkle | `(-)(\)(\|)(/)` |
| Orb: Answering | Core `accent` | Breath 8 s at half depth | as Full | Still | `(*)` |
| Orb: Working | Still | Pulse 4 s at half depth | as Full | Still | `(*)` |
| Orb: Needs you | Gold, no beat | 1 cycle | as Full | Still gold, or reverse-video chip | `(!)` |
| Orb: Done | Check 1.2 s | 4.4 s | as Full | Check, no glint | `(+)` |
| Orb: Offline | Still grey | Breath 10 s at half depth | as Full | Still | `(*)` dim |
| Orb: Ready | Still `⢎⠶⡰` | Still without work; waiting comet below | as Full | as Full | `(*)` |
| Comet | Still `◌` | 300 ms frames | as Full | Frames, in default colour | `-\\|/` |
| Caret | Still | as Full | as Full | Still | `_` |
| Chat mark | as the status orb | as the status orb | as Full | as the status orb | as the status orb |
| Startup reveal | Final frame | as Full | as Full | Frames, no fade | `(*) wisp` |
| Welcome opening (B2) | Final frame | Twice as slow (2.4 s), half depth | as Full | The head draws the ring, no fades; words at once | ASCII splash, still |
| Waiting-for-agents comet (status orb) | Rest pose | 12 s laps, half depth | as Full | Shape turns; head `text`, no breath | `(*)`, still |
| Waiting-for-agents comet (splash) | Rest | 12 s laps, half depth | as Full | ANSI-16: brightest cells in `text`, no breath. `NO_COLOR`: still, since the splash only changes colour | ASCII splash, still |
| Notification swell | None | as Full | as Full | None | — |
| Tree pulse | None | 4 s at half depth | as Full | None | — |
| Message pulse | Summary only | 140 ms a cell | as Full | Head as bold, no tail | `->` |

Focus changes none of these. When the window is unfocused, the notification swell is sent as OSC 9 or BEL.

---

## 7. Layouts

Mock-ups use real widths: inline at 100 columns, fullscreen at 120. Colours are described below each mock-up;
`preview.py --section chat` shows them in colour.

### 7.1 Inline: a chat turn

```
 › Make cache misses name their cause

 ╎ Thought for 4 s ▸
 ╎ Placeholder thought, third row from the end.
 ╎ Placeholder thought, second row from the end.
 ╎ Placeholder thought, last row.

 ⠶ I'll start with how request prefixes are built.

 ✓ Read crates/wisp-core/src/context.rs                                                     412 lines
 ✓ Searched "fingerprint"                                                            7 matches in 3 files
 ✓ Name the miss's cause                                                                         +38 −12
     crates/wisp-core/src/cache.rs                                                               +38 −12
      209       fn key(req: &Request) -> Key {
      210 -         let key = hash(&req.system);
      210 +         let key = Fingerprint::of(req)?;
      211           key
     … 34 more lines ▸
 ✓ Run the wisp-core tests  cargo test -p wisp-core                                         exit 0 · 12s

 ⠶ Misses now say which part of the prefix changed: the system prompt,
   the tools or the messages. The prefix-stability test covers each case.

   18 s · 4 tools · $0.014 · cache 92% read
```

- **The user's message** is a `surface` band from column 0 to the content edge (§4.3), only behind its text rows, `›`
  in `user` and the text in `text`. Chips (`[Image #1 …]`, `[Pasted text #1 …]`) are `text.muted` and never split:
  one that doesn't fit moves to the next row, and one wider than the row is cut with `…`. ANSI-16 and `NO_COLOR` draw
  no band, nor does a terminal that doesn't report its background or a theme forced to its other side (§2.7).
- **Thinking.** The rail in `text.faint`. While it streams, the whole thought streams under the label `Thinking`
  in `thinking` (its note facts in `text.muted`, and no `▸`, since nothing is hidden), the body in italic
  `thinking`. When its block ends, or the turn ends mid-thought, it folds to its label and the last 3 text rows of
  the thought, wrapped as they streamed: `╎ Thought for 4 s ▸`, `Thought for 4 s` in upright `thinking`, ` ▸`
  (`collapsed`, §5.1) in `text.faint`, outside the copy range. Paragraph-break rows are skipped and never counted,
  so the fold never shows a bare `╎` row. A click on the `▸` row opens the thought in the reader
  (`docs/commands.md`, "Folded rows, the ctrl-o picker and the reader"); no row names a key. A thought of 3 text
  rows or fewer shows whole with no `▸`, and a thought with no text stays `╎ Thought for 4 s`. Two thoughts in
  one turn fold separately.
  A thought replayed from a resumed session's log has no known duration, so it reads `╎ Thought ▸`. A tool call
  the log shows still running when Wisp stopped keeps its title and replays as `◦ Ran cargo build
  interrupted · Wisp stopped while it ran`, in `text.muted` with no clock, never as `stopped` (§6.6); a call you
  stop live keeps `stopped · 1s`.
- **Wisp's prose.** `⠶` in `accent` at the start of each prose block. Continuation lines and later paragraphs of
  the same block have no marker.
- **Tool rows.** One line each: the glyph, then the title in `text` (a made title's verb in bold), two spaces
  and the command in `text.muted`, then the facts in `text.muted` ending at the content edge: the state, then
  the time. A diffstat is `+38` in `ok` and `−12` in `error`. What happened is the glyph and a fact: `exit 101`
  and `couldn't start` in `error`, `timed out` and `stopped` in `text.muted`. When the row is short, counts drop
  first, then the command, then the title is cut at its end; the state stays. Only a content tool's view (today
  show_diff's diff) is drawn under its row.
- **The turn footer**, in `text.faint`: duration, tool count, cost and cache read. When the cache missed it reads
  `cache miss: tools changed` in `text`: noticeable, but not gold. A reply that wasn't kept adds `not kept in
  history`, and the status row says so in place of `Done`, only when its last step showed text, a tool call or
  another block; an empty or thinking-only reply ends like a kept one (the `Said` tracker beside `clean_outcome`).

### 7.2 A tool call and its result

One line in the transcript, running or done, and opened in the reader by a single click on the row; nothing
expands in place. The reader shows the one cell under its title row and a `─` rule in `line`, with the pager's
key row:

```
 ✓ Run the wisp-core tests  cargo test -p wisp-core                                     exit 0 · 12s
────────────────────────────────────────────────────────────────────────────────────────────────────
     $ cargo test -p wisp-core
        Compiling wisp-core v0.1.0
         Running unittests src/lib.rs
     …
   / search · 1-21/43 · ctrl-o every detail · q esc back
```

- **Expanded output** is a `sunken` band from column 5 to the content edge, in `text`. The command line is in
  `text.muted`, with its `$` in `text.faint`.
- **Exit status:** a failing exit adds `exit 101` in `error` to the facts; a tool that couldn't start reads
  `couldn't start` there, with its reason in the reader.
- **ANSI colour** in tool output is remapped into the theme: red becomes `error`, green `ok`, yellow `hue.warm`
  (never gold), blue `accent`. Only SGR passes through; every other escape is stripped (§9.7).

### 7.3 A diff

A content tool keeps its content: a call whose tool declares `Show` (today show_diff) draws its diff under its
one-line row, the only thing ever drawn under a tool row. Inline (above) shows a diff of up to 30 lines whole,
and a longer one's first 20 rows, then `… N more lines ▸`. A click on that row opens the full diff in the reader at
once, as does a click on the row above it after 400 ms (§7.2); the layout below is its fullscreen form:

```
 crates/wisp-core/src/cache.rs                                                         +38 −12   1 of 3
 ──────────────────────────────────────────────────────────────────────────────────────────────────────
  205    impl Cache {
  … 3 unchanged lines
  209        fn key(req: &Request) -> Key {
  210 -          let key = hash(&req.system);
  210 +          let key = Fingerprint::of(req)?;
  211            key
  212        }
```

- **Columns.** Line numbers in `text.muted`, right-aligned. The sign column is `+` in `ok` or `-` in `error`, then
  the code in syntax colours.
- **Rows.** A whole row takes `diff.add` or `diff.del`, to the content edge. Changed words inside a paired row take
  `diff.add.word` or `diff.del.word`, with the foreground forced to `text`. Rows are paired when they are at least
  40% similar (delta's 0.6 distance rule). In 16 colours and under `NO_COLOR` changed words are in reverse video.
  Inside a selection, which reverses the whole row, a changed word looks like the rest of the row and loses its
  mark: an accepted cost of using reverse video (plan Q1).
- **Syntax.** The language comes from the file headers, else from a fence hint (`diff rust`, `diff src/x.rs`). The
  old and new sides are highlighted separately, so a removed `/*` never turns the added lines into a comment; both
  reset at every `@@` and file header.
- **Unchanged runs** of more than 6 lines collapse to `… N unchanged lines` in `text.faint`.
- **Side by side** when each side gets at least 70 columns (140 or wider). Otherwise unified.
- **Header.** The path in bold `text`, the diffstat right-aligned, and `1 of 3` files. A `─` rule in `line`.

### 7.4 An approval

```
 ┃  Needs you   Send
 ┃ Push 3 commits and open a pull request on GitHub?
 ┃   Repository  example-org/website-redesign
 ┃   Branch      feat/update-layout into main
 ┃   Commits     3 · 4 files · +167 −1
 ┃   Runs        git push · gh pr create
 ┃
 ┃ › Approve                   y
 ┃   Approve for this task     t
 ┃   Edit                      e
 ┃   Not now                   n  esc
 ┃ Pushing needs a keypress here. "Yes" typed in the chat doesn't count.
```

- **The rail.** `┃` in `warn.glyph`: the card's gold border, translated. It's the only saturated colour on screen.
- **The chip.** `Needs you` in bold `warn.on` on `warn.fill`, with a space of padding each side. The action
  class (`Read`, `Write`, `Send`, `Admin` or `Spend`, the plan's classes) follows in `text.muted`.
- **The question.** One sentence from a template: the key phrase in bold `text`, the rest in `text`.
- **Details** are key/value rows, the keys in `text.muted` and padded to 12 columns. Show details (`d`) reveals
  the exact command or request; Hide details folds it again.
- **Options** use the brand's words: Approve, Approve for this task, Always allow `git push` here (where the
  policy offers it), Edit, Not now. The selected option has a `›` in `warn.glyph` and bold text; keys are in
  `text.faint`. ↑ and ↓ move, Enter confirms, and the letter keys act at once.
- **The last line** says what can't approve it, in `text.muted`.
- **Where it sits.** In fullscreen, the approval is pinned above the composer: never a modal box, never covering
  the transcript. In inline mode it is part of the live region. Once answered, it is committed as one line:
  `✓ Approved: push 3 commits and open a pull request` (`ok`), or `◦ Not now: push 3 commits` (`text.muted`).

### 7.5 The status line and composer (the live region)

Inline, at the bottom, these are the only rows that redraw:

```
 ⠘ Run the wisp-core tests  cargo test -p wisp-core                                              7s

╭──────────────────────────────────────────────────────────────────────────────────────────────────╮
│ ⧉ 2 agents · 1 working                                                              total ~$0.21 │
├──────────────────────────────────────────────────────────────────────────────────────────────────┤
│ › Ask for a change, or type / for commands                                                       │
├──────────────────────────────────────────────────────────────────────────────────────────────────┤
│ ⢎⠶⡰  Run the wisp-core tests · 7s   esc to stop        sonnet-5.5   1 needs you · Custom Gateway │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
```

- **The frame.** The bottom of the screen is one rounded frame in `line` across the full width, in three
  sections divided by `├─┤` (`+-+` in the ambiguous-wide and ASCII sets), and every row in it is `│ `, the text,
  then ` │`. The top row holds the agent strip and `total` (`docs/agents.md`, "The strip"), and is an empty framed
  row until there is something to show, so the composer never moves when the first reply lands; the waiting
  messages, rewind notes and mail line follow it while they have something to show and no overlay is open. Then
  come the composer and the status row. Pickers, cards and the other docks open above the frame, which keeps its top
  row and status row under them; while one has the keys and the chat is empty, the composer's row and its edge aren't
  drawn, as under `/chats`; `/resume` takes the composer's section, its title on the divider. The sidebar keeps its rail
  beside the transcript and the frame runs under it. On a short terminal things give way in this order: the
  strip rows after the first, keeping 2 transcript rows; below 9 rows the top row and its divider; below 7 the
  status divider, leaving the status as a bare row under the composer's box; below 5 rows or 21 columns the box,
  with the waiting messages, rewind notes and mail line as bare rows from 9 rows; below 2 rows the status row. A
  picker, card, `/agents`, `/ps` or `/resume` that needs more rows than are left takes them in the same order, the
  top row first, then the status divider, before it is refused or cut; over an empty chat the composer's row and
  its edge have already given way.
- **The composer** is the frame's middle section (`tui-review.md` §3): `│` at column 0, the
  prompt `›` at column 2, the input from column 4 to `width − 3`, and `│` at `width − 1`. Spaces at a wrapped row's
  edge hang in the padding column at `width − 2`, drawn as at most one space, so the frame never moves. The box's edges take the rows
  the rules used to, and the frame's top row and status section take 3 more; it grows to at most 8 input rows. The prompt is `›` in `accent`, the
  input `text`, the placeholder `text.faint`, and the caret a steady `text` block. **Without the keys** (a card,
  picker or another prompt has them) an empty composer's row isn't drawn; one holding a draft stays, its `›`
  `text.faint`, the input `text.muted` and no caret. Nothing is SGR faint. The ambiguous-wide and ASCII sets draw
  `+-+` and `|`. Below 21 columns, or with fewer than 5 rows free, there is no frame. Newline is shift-enter, with
  ctrl-j and alt-enter as fallbacks.
- **The footer row.**
  - **Three groups,** separated by three spaces, never by ` · `, so a hint never reads as a fact
    (`tui-review.md` §4).
  - **Left:** the orb, two spaces, then the status sentence (§8.2) in `text.muted`, `Ready` included. `Needs you`
    is in `warn`, and `Done` in `ok`.
  - **Middle:** key hints, keys in `text.muted` and words in `text.faint`. When the row is short, facts drop
    first and hints stay; hints drop from their right only once every fact has gone.
  - **Right:** right-aligned facts, the model in `text` and three spaces after it, the rest in `text.muted`, most
    important first: `1 needs you` (in `warn`) and
    `3 working`; the model and effort; the credential in use (`Anthropic API key`, `Anthropic auth token`,
    or `Custom Gateway · <host>` for a custom endpoint, never a credential value); `64% context left`; the
    session cost `$0.21`; and the cache read rate `cache 94%`.
  - **When it's narrow,** the gateway host shrinks first, with a middle `…`, down to 12 columns, then drops,
    leaving `Custom Gateway`. Only then do facts drop from the right of the row as built today (model,
    effort, pending change, credential, request count, cost, cache): cache, cost, requests, credential,
    pending, effort, model.
  - **The esc hint.** While something is running, the row always offers esc, at every width. It is
    `esc to stop`, or, when a hint already binds esc (`esc to stop` with an edit waiting, `esc cancel` while
    editing, `esc close` with the suggestions open), that hint in its place; the working line then leaves
    `esc to stop` out of its bracket. The esc hint comes first among the hints, after a state note when there
    is one (`editing a next-turn message · esc cancel · enter save`, `1 waiting · esc close`). When the
    row is short, the hints right of the esc hint drop first, then the note, and the esc hint last; the
    sentence makes room for it by dropping its detail whole (`Writing a web_search call` reads `Writing`),
    and a head still too long is cut at its last whole word with `…` (`Run the…`). Below 60 columns a busy row
    shows only its sentence and the esc hint (`tui-review.md` §4). None of this happens when the sentence already names the stop key
    (the selection hint `ctrl-c copy · esc to stop`): the stop hint is shown once. While you type into a
    process, esc goes to the process, so its exit hint `f12 to go back` takes the esc hint's place in all
    of this (`typing into 3 · f12 to go back`), and the row never offers `esc to stop`.
  - **Notices** (a status sentence that reports or refuses, such as `Paste the image's path: no clipboard
    over SSH.`) are never cut for the esc hint: it steps aside until the notice expires, and esc keeps
    working throughout. Every notice is at most 60 columns with the action first, so it is whole at 80
    columns.

### 7.6 The project and agent tree

The bundled agent-viz draws something simpler than this sketch: its section `agent-viz/topology` is the agents minimap, a textless `topology` of state dots and lines (`docs/plugins.md`, "Bundled plugins", the `agent-viz` row), and a click on it, or `/sidebar agent-viz/topology`, opens `/agents`. Its title row counts the live agents on the right, in the agent strip's words and colours (`5 agents · 1 working`, `6 · 2 working · 1 failed`), narrowing as the strip does and kept when the box is folded.

The agent-viz plugin's section of the sidebar (§7.8): docked beside the transcript at 120 columns and wider, or in the `/sidebar open` overlay at any width:

```
 Projects   1 needs you · 3 working · 1 done
 ⠶ Jarvis                         two projects                      $0.42 today
 ├─⠶ cache-fingerprints  project  drafting the plan            2:14       $0.31
 │   coder → coordinator  "patch ready, 3 files"
 │ ├─⠶ researcher                 reading prompt-caching.md    0:41       $0.04
 │ ├─◉ coder                      needs you: run cargo test               $0.12
 │ └─◦ reviewer                   waiting for coder
 └─✓ tui-research        project  report ready                            $1.20
```

- **The title** is the two halves: `Projects` in bold `text`, then the counts (`1 needs you` in bold `warn`, the
  rest in `text.muted`).
- **One row per agent.** Glyph, then the name in its role colour (Jarvis in `accent` and bold), then `project` in
  `text.faint` for project rows, then the activity in `text.muted` (in `warn` when it needs you), then the elapsed
  time and the cost right-aligned.
- **The tree lines** are in `line`. A message pulse lights them (§6.9).
- **Order.** Siblings sort by need: needs you, then working, then waiting, then done. Finished projects fold after
  10 minutes.
- **Who draws it.** Agent-viz decides which rows exist, their order and their facts, and sends them as a tree
  block; Wisp draws the glyphs, the tree lines, the role colours and the right-aligned facts, and ticks elapsed
  times and the working pulse itself, so the plugin is asked again only when something changes.
- **Keys.** The tree's rows take no keys. Ctrl-T opens `/agents`, the topology page, where Enter on a dot opens
  that agent's chat, whose rows 0 and 1 are its header and a `─` rule in `line`, and `r` releases its held messages; it
  works with or without the sidebar.
- **Folding and pinning.** A click on the section's title folds it to that row, ending with `▸` in `text.faint`,
  or opens it again, and the choice is saved (`display.sidebar_folded`). Pinned in `/config`
  (`display.sidebar_pinned`), the tree stays at the top of the sidebar while the other sections scroll under it
  (§7.8).
- **The board.** A click on the tree's rows, or `/sidebar agent-viz/tree`, opens the section's page (§7.8): the
  whole tree with a role fact on each node, and beside it, in a second column, a spend spark and the counts. Its
  working nodes are drawn still, at every motion level, and its rows take no keys either. At 120 columns:

  ```
   Agents                                                                           3 working · 2 waiting · 5 done · $1.24
  ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
   ⠶ main                                main  12m 03s  $0.42   spend  ⣀⣠⣤⣤⣶⣶⣶⣶⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿  $1.24
   ├─⠶ cache-fingerprints         coordinator   2m 14s  $0.31   working                                                  3
   │ ├─⠶ researcher                researcher      41s  $0.04   waiting                                                  2
   │ ├─◦ coder                          coder     held  $0.12   done                                                     5
   │ └─◦ reviewer                    reviewer   queued
   └─✓ tui-research               coordinator     done  $0.20
     ├─✓ researcher                researcher     done  $0.03
     ├─✓ designer                    designer     done  $0.05
     ├─✓ coder                          coder     done  $0.05
     └─✓ reviewer                    reviewer     done  $0.02

     / search · 1-10/10 · q esc back
  ```
- **Inline mode** has no tree; the footer carries the counts (`1 needs you · 3 working`).

### 7.7 A proactive notification

Inline, committed above the live region:

```
 ⠶ Jarvis · routine · 7:30 AM
   The price check ran. Bloom Mini is down $12, to $349.
   ctrl-o show details

 ◉ Jarvis needs you · follow-up
   Send this morning's summary to home.example? It's ready.
    ctrl-y  review · ctrl-n not now
```

- **The header line.** The glyph (`⠶` `accent`, or `◉` `warn.glyph`), the agent's name in bold, then the kind and
  time in `text.muted`. For a needs-you notice, `Jarvis needs you` is in bold `warn`.
- **The body** is one or two plain sentences in `text`.
- **Keys** are in `text.faint`. The primary key is a `warn.fill` chip when it's needed.
- **In fullscreen** the same notice appears in the transcript, and the header shows a toast on the right:
  `◉ 1 needs you · ctrl-y` (it beats with the orb), or `⠶ routine finished · ctrl-o` for 8 s.

### 7.8 Fullscreen, everything together

```
 ⢎⠶⡰ wisp   Jarvis › cache-fingerprints                                           ◉ 1 needs you · ctrl-y    ⠶ 3 working
────────────────────────────────────────────────────────────────────────────────┬───────────────────────────────────────
 › Make cache misses name their cause                                           │ Projects      1 needs you · 3 working
                                                                                │ ⠶ Jarvis                        $0.42
 ╎ Thinking                                                                     │ ├─⠶ cache-fingerprints           2:14
                                                                                │ │ ├─⠶ researcher                 0:41
 ⠶ I'll start with how request prefixes are built.                              │ │ ├─◉ coder                 needs you
                                                                                │ │ └─◦ reviewer                waiting
 ✓ Read crates/wisp-core/src/context.rs                               412 lines │ │     → reviewer             12 s ago
 ✓ Searched "fingerprint"                                  7 matches in 3 files │ │       "please review the patch"
 ✓ Edited crates/wisp-core/src/cache.rs                               +38 −12   │ └─✓ tui-research                 done
      209       fn key(req: &Request) -> Key {                                  │
      210 -         let key = hash(&req.system);                                │ Activity
      210 +         let key = Fingerprint::of(req)?;                            │ cache             ⣿⣿⣿⣿⣿⣿⣿⣀  94% read
      211           key                                                         │ context           ⣿⣿⣿⣿⣿⣀⣀⣀  64% left
     … 34 more lines ▸                                                          │ today                           $0.42
 ⠘ Run the wisp-core tests  cargo test -p wisp-core                        7s   │
────────────────────────────────────────────────────────────────────────────────┴───────────────────────────────────────
╭──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│ › Ask for a change, or type / for commands                                                                           │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
 ⢎⠶⡰  Run the wisp-core tests · 7s                 sonnet-5.5   your Claude plan · 64% context left · cache 94%
```

- **The header.** The static logo, then the breadcrumb (the agent you're attached to) in `text.muted` with the
  current node in `text`, then the toast or counts on the right.
- **The sidebar** is a band of `clamp(width / 4, 38, 48)` columns right of a one-column `│` rule in `line`; the
  transcript takes the rest (81 columns at 120, 119 at 160, 201 at 250). Each section is a box standing on the
  rule: the rule is its left side and the terminal's last column its right. The first box tops with `╭` `╮`, a box
  below spare pinned rows with `├` `╮`, and the others share the edge above them; the last box's foot is `├` `╯`
  where the rule runs on. Its title, its `couldn't update` row and each group of its content are ruled off by
  `├─┤`. The band's last row is the rule's, so a box never touches the bottom frame. The header, the composer, the footer,
  pickers, cards, the agents strip and `/agents` keep the full width, drawn over the band where they reach it.
- **Docking.** The sidebar docks when the terminal is 120 columns or wider, `display.sidebar` is `auto` (the
  default) and some section is on, once a section has shown content or `/sidebar` asked for it. It docks and
  undocks once per settled resize, never mid-drag, and the transcript re-wraps once each time. A section that
  empties later leaves it docked, saying `Nothing to show now` in `text.muted` (`Nothing to show yet` before any
  content). It undocks when the terminal narrows below 120, when the last section is switched off (its plugin
  disabled, removed or caught in a cycle) or when `display.sidebar` becomes `hidden`; `/sidebar` toggles between
  docked and hidden and saves the choice.
- **Scrolling.** Sections stack top to bottom, two boxes sharing one edge. When they don't fit, the wheel over
  the band scrolls it (never the transcript), and the cut ends show `↑ n more` and `↓ n more` in `text.faint` on
  the first and last rows inside the boxes' sides, counting the hidden rows that aren't edges; a box has no edge
  where it is cut. Rows sit one space inside each side, so their numbers end in the frame's facts column.
- **Pinned sections** (`display.sidebar_pinned`, set in `/config`) come first and never scroll; their last
  box shares its foot with the first box below, as the top edge of that box. They get the rows they ask for, but no
  more than leaves the other sections the rows those ask for, and never less than half the panel; with nothing
  below, they may take it all and end in their foot. When they need more they share those rows, and a section that is
  cut ends with `… n more`, or `… k more sections` on the rule for those left out, with any spare rows. Since the
  rows below count, an unpinned section that grows or shrinks can move the shared edge and change a pinned
  section's `… n more`.
- **Folding.** A click on a section's title (a press and release on one cell) folds it to its title row, or opens
  it again, and saves `display.sidebar_folded` quietly: no transcript note, and a refusal shows as a hint. A folded
  title keeps its note and ends with `▸` in `text.faint`; a failing folded section keeps its failure row. This is
  the one row that opens in place. An open title shows no glyph, so no row shows `▾`. A click that refocuses the
  terminal, the first click while a transcript selection shows, and a drag fold nothing, and a title under a card
  or picker is never hit. Folding, pinning and scrolling never change the band, so the transcript never re-wraps
  for them.
- **The overlay.** Below 120 columns, or with `/sidebar open` at any width, the same panel is drawn over the
  transcript's right side on a `surface` ground (the whole width under 43 columns, where the boxes' sides are the frame's and the foot is `╰` `╯`; no ground where there is no
  band, §2.7), with `↑↓ scroll · esc close`
  in the status line. It never re-wraps the transcript; esc closes it and keeps the scroll position, a question
  card, a step-in card or a Retry choice closes it, and Ctrl-T closes it and opens `/agents`. Widening past 120 closes an overlay opened narrow
  and docks the panel.
- **The page.** A click on a section's body (its content rows, its `couldn't update` row or a pinned cut's
  `… n more`) or `/sidebar <section>` opens the section full screen, as the reader is: the title in bold `text`
  with its note on the right on the top row, a `line` rule, the content, and the reader's key row without its
  detail, screen and follow keys. `↑ n more`, `↓ n more`, the boxes' edges and `… k more sections`
  open nothing, and a title click still only folds. `next-column` sets the content in columns side by side at
  equal widths with a 3-column gutter when each gets at least 40 columns, and stacks them in order, a blank row
  apart, otherwise; a column is at most 120 columns wide and spare width stays empty. A page has no folds, pins or
  scrolling part: it scrolls as a whole. A tree keeps every fact column, working nodes are still at every motion
  level, and a gauge's meter stops at 40 cells. A failure shows its whole reason, wrapped. Until the page's own
  content arrives it draws the panel's (`Nothing to show yet` when there is none), so nothing flashes. Its
  `elapsed` clocks step on the same whole wall-clock seconds as the panel's. esc returns to the panel or the
  overlay it came from.
- **Selection** stops at the rule: a drag in the transcript never reaches the band, and a click or drag in the
  band selects nothing.
- **Rules** are `─` and `│` in `line`. With the composer box, the welcome card's frame and the sidebar sections'
  boxes, they are the only box drawing in the app, and all of it is chrome.
- **One selection recipe.** The selected row, wherever it is (pickers, `/config`, the ask and Retry cards, the
  queued-message rows, command suggestions, transcript rows, `/agents`, `/ps` and the API key card), gets an
  `accent.tint` band from column 0 to the transcript's content edge, or the chrome edge for pickers, `/config` and
  the ask card (§4.3), a `›` in `accent` at column 1, and its text in `text`; `text.faint`, `text.disabled` and
  `user` on it step up to `text.muted`. Every row of a selected entry is banded (an agent and its detail rows, a
  field and its choices), and unfocused rows keep three blank columns where ` › `
  would be. Any other ink on the band keeps its colour where it reads there, in the theme painted: a span with
  more than one grapheme and a digit or two letters in a row is text and needs 4.5:1, anything else (`›`, `✓`,
  `●`, a radio, `[x]`, a bar, ASCII's one-letter marks) is a glyph and needs 3.0:1; an ink under its floor shows
  as `text`. ANSI-16 and `NO_COLOR` check nothing, since the band is reverse video. Under a mouse selection (a
  drag in the transcript or the composer) every other colour keeps its own colour on `accent.tint`, code included,
  unless it is under 3.0:1 there as painted, where it shows as `text`. The selection sees only painted colours, so
  in dark `line` shows as `text.muted`, as `text.disabled` does. The choice in use is a `✓`, `accent`, one space
  after its name, focused or not, and `now` in its effect cell. ANSI-16 and `NO_COLOR` draw the selected row in
  reverse video. A row that hides something ends with `▸` in `text.faint` after its text, and a click on it opens
  the cell in the reader; nothing else expands in place (a folded sidebar title, above, is the one row that does),
  so no row shows `▾`.
- **Pickers dock above the composer,** under a `line` rule, with a title row (the name in bold `text`, what
  choosing does in `text.muted`), column headers in `text.faint`, the rows and a key row. The transcript ends at
  the rule, so no sentence is cut (`tui-review.md` §5).
- **Never boxes around messages.** The panel and the composer are the only containers.

### 7.9 The context report

`/context` draws a grid of cells, each one 1% of the window (10×10), or 0.5% on a 1M window when the
screen is wide enough (20×10). Cells are coloured by category in the legend's order, then come the free
space and the reserve past the compaction point. The header and the legend sit beside the grid, or under it
when the screen is narrow (`docs/context.md`).

```
  ⣿ ⣿ ⣿ ⣿ ⣿ ⣿ ⣿ ⣿ ⣿ ⣿   claude-opus-5-5 · direct
  ⣿ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀   63.1k / 850k tokens (7%)
  ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀   62.4k measured at turn 9, step 4 · 700 since
  ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀
  ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀   Usage by category                      estimated (~)
  ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀   ⣿ System prompt              ~312   <0.1%  estimated
  ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀   ⣿ Tools (2)                 ~1.6k    0.2%  estimated
  ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀ ⣀   ⣿ Instructions (2 files)    ~3.4k    0.3%  estimated
  ⣀ ⣀ ⣀ ⣀ ⣀ ⡪ ⡪ ⡪ ⡪ ⡪   ⣿ Environment                ~120   <0.1%  split
  ⡪ ⡪ ⡪ ⡪ ⡪ ⡪ ⡪ ⡪ ⡪ ⡪   ⣿ Your messages             ~5.5k    0.6%  split
                        ⣿ Replies and thinking      19.3k    1.9%  measured
  1 cell = 10k          ⣿ Tool results             ~30.7k    3.1%  split
                        ⣿ Attachments (2)           ~2.2k    0.2%  split
                        ⣀ Free before the point     ~787k   78.7%
                        ⡪ Reserve past the point     150k   15.0%
                          Would compact at 850k (window − reserve)
                          automatic compaction is off
                          window 1M (shipped)
```

Only the grid cells and the legend's markers carry a category's colour; the legend's words stay in body
text. So the colours are glyph-only and need 3.0:1. Contrast is against each mode's `bg`.

| Row | Token | Dark | on `bg` | Light | on `bg` | 256 (dark / light) | 16 (dark / light) |
|---|---|---|---|---|---|---|---|
| System prompt | `thinking` | `#92A4B2` | 6.49 | `#47566E` | 6.23 | 110 / 60 | 37 / 90 |
| Tools | `hue.cool` | `#B180D7` | 5.51 | `#0A6E6E` | 5.07 | 140 / 23 | 96 / 36 |
| Instructions | `hue.warm` | `#F5F543` | 14.31 | `#945206` | 5.07 | 227 / 94 | 95 / 35 |
| Environment | `user` | `#828282` | 4.34 | `#767672` | 3.82 | 244 / 243 | 90 / 90 |
| Summary | `syn.type` | `#4EC9B0` | 8.18 | `#5B48CB` | 5.44 | 43 / 61 | 95 / 35 |
| Your messages | `text` | `#D4D4D4` | 11.25 | `#161614` | 15.18 | 188 / 233 | default |
| Replies and thinking | `accent` | `#569CD6` | 5.65 | `#1656C9` | 5.48 | 74 / 26 | 94 / 34 |
| Tool results | `role.reviewer` | `#DA70D6` | 5.77 | `#AD2061` | 5.57 | 170 / 125 | default |
| Attachments | `text.muted` | `#A6A6A6` | 6.85 | `#57564F` | 6.17 | 248 / 240 | 37 / 90 |
| Free | `text.faint`, glyph `⣀` | `#757575` | 3.62 | `#85837A` | 3.18 | 243 / 244 | 90 / 90 |
| Reserve | `text.faint`, glyph `⡪` | `#757575` | 3.62 | `#85837A` | 3.18 | 243 / 244 | 90 / 90 |

- **Why these.** Gold, green and red mean needs-you, done and error (§2.1), so no category uses them. The
  five other hues go to the rows that are usually large; the three greys go to the rows that are usually
  small. Replies take Wisp's blue, since `agent` is `accent`. In dark, Tools is violet (`hue.cool`),
  Instructions yellow (`hue.warm`, clear of gold) and Summary teal; in light, teal, orange and violet, as
  `hue.cool` and `hue.warm` take `syn.string`'s and `syn.number`'s light values. These keep a colour in
  16 colours, where the role tokens fall to the default. In dark the greys sit close (System prompt and
  Attachments, Environment and Free); their sizes and the legend's words tell them apart.
- **Free and reserve** share `text.faint` and differ by shape, since faint never carries meaning alone
  (§2.3).
- **16 colours** leave six usable classes, so rows that mean similar things share one: Instructions and
  Summary are magenta, Your messages and Tool results the default colour, and System prompt, Environment
  and Attachments grey. The legend's words tell them apart.
- **`NO_COLOR`** keeps the shapes only: `⣿` used, `⣀` free, `⡪` reserve. The legend's figures carry
  the categories, and the title is bold, as headings are.
- **Every category with tokens** shows at least one cell, as in Claude Code; the legend's figures and
  the caption give the real sizes.

The sidebar's Context section (`docs/context.md`, "The sidebar section") draws the same fill as two rows of
25 cells, each 2% of the box's 100%, filled column by column and split into four parts: `⣿` in the colour of
the category each part is named after, then `⣀` free; the parts' figures give the sizes. Where the terminal shows
pictures, the rows are 100 rounded squares, four to a column, each 1%: free squares are `line` mixed halfway into
`bg`, told apart from used ones by the fill's edge and the %, not by a shape; the list's part marks become squares,
and `free` keeps its `⣀` in `text.faint`.

| Part | Token | 16 (dark) |
|---|---|---|
| system | `thinking` | 37 |
| tools | `hue.cool` | 96 |
| chat | `accent` | 94 |
| skills | `hue.warm` | 95 |
| free | `text.faint` | 90 |

- **Why these.** System takes the System prompt's colour, tools the Tools', chat the Replies' (usually its
  largest share) and skills the Instructions', since skills are instruction files. So the four differ,
  also in 16 colours: grey, cyan, blue and magenta.
- **Without colour** the used parts look alike, as on `/context`; used and free still differ by shape, and
  the legend's figures name the parts.

---

## 8. Voice and microcopy

### 8.1 How Wisp talks in a terminal

The brand's rules, applied:

- **Plain words first.** Say what happened, in the reader's words. Technical detail goes behind
  `ctrl-o show details`, as in the app's "Show details".
- **Specific beats impressive.** `41 passed · 0:12`, not `Tests look good!`
- **Honest about now and later.** Say "Retrying in 8 s", never "Working on it…" with no end. Always name the
  provider that saw something.
- **Calm confidence.** No exclamation marks, no "Oops", no blame, no fear.
- **House style.** US English, sentence case, `·` between facts. The product's own names: Jarvis, Show details,
  Needs you, Not now, Approve, Activity.
- **Words we don't use:** agentic, LLM (in UI copy), seamless, revolutionary, magic, supercharge, unleash,
  effortless, "100%", "never" (unless the code enforces it), successfully, oops, sorry (for things that aren't
  Wisp's fault).

### 8.2 Status words

The sentence after the orb:

| State | Sentence | Notes |
|---|---|---|
| Ready | `Ready` (in `text.muted`, like every status sentence) | Or `1 needs you · ctrl-y` when something waits |
| Thinking | `Thinking` · `0:04` | The timer appears from 2 s |
| Retrying | `Anthropic didn't answer. Retrying in 8 s` | It counts down; the provider is always named |
| Answering | `Answering · esc to stop` | |
| Working | `Running cargo test · 0:07`, `Reading context.rs`, `Editing cache.rs` | The tool's present-tense verb and target |
| Needs you | `Needs you: approve git push` | In `warn` |
| Done | `Done · 18 s · cache 92%` | `Done` in `ok`. It holds for 2.2 s, then `Ready` |
| Stopped | `Stopped. The 2 steps that finished are kept.` | Per P6, finished steps stay in history |
| Offline | `Offline. Reconnecting · 0:12` | |
| Budget cap | `Paused at this project's $5.00 budget · ctrl-y` | A needs-you decision |

### 8.3 Tool verbs

A tool row's title is the tool's own when it gives one (bash's `description`, show_diff's `title`), with the
command muted beside it; the verbs below make the title when it doesn't, bold and beside the target
(`Ran cargo fmt --all --check`), in the present tense until the call ends. A process call the model described
shows its description with what it did muted beside it (`Stop the dev server  interrupted  process 27`).
Otherwise it reads as a sentence naming the process by number, never by its command, with the description of the
call that started it muted beside it: `Typed into process 31 and pressed Enter: "select 1;"`, `Typed into process
31` when the terminal didn't echo the text, `Pressed Enter in process 31`, `Waited for process 27  Start the dev
server`, `Stopped process 27`.

| Tool | Running | Done | Facts when done |
|---|---|---|---|
| read | Reading | Read | `412 lines` |
| search, grep | Searching | Searched | `7 matches in 3 files` |
| list, glob | Listing | Listed | `24 files` |
| edit, patch | Editing | Edited | `+38 −12` |
| write | Writing | Wrote | `new · 120 lines` |
| shell | Running | Ran | `41 passed · 0:12`, or `exit 101 · 0:03` |
| fetch, web | Fetching | Fetched | `example.com · 14 KB` |
| message an agent | Asking | Asked | `coder · "tests first"` |
| plan | Planning | Planned | `5 steps` |

### 8.4 Approvals

- **Templates.** One sentence and a reason. For example: `Run cargo test --workspace?`;
  `Push 3 commits and open a pull request on GitHub?`; `Send today's summary to home.example?`;
  `Install gnuplot on this computer? It needs admin rights.`
- **Options, in this order:** `Approve` (y), `Approve for this task` (t), `Always allow "cargo test" here` (a,
  only where the policy offers it), `Edit` (e), `Not now` (n, esc), `Show details` (d).
- **The last line:** `Sending needs a keypress here. "Yes" typed in the chat doesn't count.` Swap in the class's
  verb: Pushing, Sending, Spending, Installing.
- **Editing anything** voids the approval and asks again, and says so: `Editing this asks again.`

### 8.5 Errors

Each error is a sentence saying what happened, then a sentence saying what still works or what Wisp is doing,
then at most one action. Only the `⨯` and the first sentence are `error`, and only when something actually
failed; the rest of the headline is `text` and the details below it one `text.muted` block. An action is a card
after one blank row: the options with the selection recipe (§7.8), each name in `text` and its detail in
`text.muted`, then the key row, keys in `text.muted` and words in `text.faint` (`tui-review.md` §2d). The
technical detail sits behind `ctrl-o`.

| Situation | Message |
|---|---|
| A provider is busy | `Anthropic is busy. Retrying in 8 s.` (no red: nothing failed yet) |
| A provider fails after retries | `Anthropic didn't answer after 5 tries. Your message is kept; press enter to try again.` |
| A bad key | `Anthropic refused this API key. Check it with /keys. Nothing was sent.` |
| A test failure | `cargo test failed: 2 of 41 tests. Wisp is looking at why.` |
| A budget cap | `This project reached its $5.00 budget and paused. Raise it with /budget, or stop here.` |
| A message the kernel refused (`Rejected`, matched by its id) | `This message wasn't sent: <reason>.` The message goes back into the composer, ahead of the draft |
| The 33rd waiting message | `This message wasn't sent: 32 messages are already waiting. Send this after the turn.` |
| `/compact` while Wisp works | `/compact runs between turns. Send it after this one.` (a hint; the text stays in the composer) |
| After a stop with messages waiting | `Stopped. N queued messages are held; enter sends them.` then, if steers were waiting, `Your unsent steer is back in the composer.` (a note after the turn's footer) |
| After a failure with messages waiting | `The turn failed. N queued messages are held; enter sends them when you're ready.` and the same steer sentence |
| An edit or removal that came too late (`already_delivered`) | `Wisp already read that message, so it stays sent.` For an edit, `Already sent: your edit is back, ahead of your draft.` (or `Already sent: your edit is back in the composer.` when there was no draft). A hint, not a note |
| Tab on a held row while nothing runs | `nothing is running to steer` while the focus is in the rows (their key hint follows), else `nothing is running to steer; enter sends held messages` (a hint; nothing is sent) |
| Editing or switching another client's message whose images Wisp doesn't hold | `Can't change this message: its images aren't held here.` (a hint) |
| A failure Wisp can offer to retry (`docs/plans/resilience.md` §6.7) | The error cell ends with the choice: `› Retry` then `Switch endpoint`, only the offered rows, each with a detail in `text.muted`. Retry reads `from step 19 · cache warm ≈$0.06`, or `sends your message again`; with a pending endpoint `on direct · cache rebuild ≈$1.52`; after the cache lifetime `cache may have expired ≈$1.52`; after a long wait `asked to wait 5m 00s`; `checking…` until the prices arrive. Switch endpoint reads `pick one, then retry · cache rebuild`. Then the key hint |
| The `Retry on` picker | Titled `Retry on`, one row per endpoint priced for the failed turn's model: `cache warm ≈$0.06` (or `cache expired · rewrite ≈$1.52`) where it failed, `cache rebuild ≈$1.52` elsewhere, `this endpoint failed` when Retry isn't offered there, `unavailable: <reason>` |
| A Retry used | The choice shrinks to `Retried` or `Retried on direct` (`text.muted`), then `↻ Retrying step 19 on direct · cache rebuild ≈$1.52 · earlier Sonnet 5.5 reasoning may not carry over` (ASCII drops the `↻`); a re-sent message's cell is tagged `retried` |
| A Retry the kernel refused | `Couldn't retry: <reason>.` and the choice goes |
| Logging stopped (`LogDisabled`) | `Session logging stopped: the disk is full. The chat carries on.` |
| A provider event this version doesn't know | `Anthropic sent an event this version doesn't know. It's kept in the log.` (`text.muted`) |
| A fallback model | `Anthropic declined, so this answer came from claude-sonnet-5.5.` (`text.muted`) |
| A cache miss | `cache miss: tools changed` in the turn footer, in `text` |

**Not:** `Error: ECONNRESET` on its own, `Oops! Something went wrong`, stack traces in the transcript, or capitals
for emphasis.

### 8.6 Empty states, hints and figures

- **Empty transcript:** the calm card (§3.2) and its key line, `enter send · ctrl-j new line · /model · /help`
  (`shift-enter` in place of `ctrl-j` where the terminal reports it), then the composer's placeholder.
- **The composer placeholder:** `Ask for a change, or type / for commands`. On the first run:
  `Ask Jarvis anything`.
- **Key hints** are lowercase, the key then a bare verb: `enter send`, `esc close`; `to` only in `esc to stop`,
  `ctrl-c to stop`, `type to filter`, `esc again to rewind` and `[ to scrollback`. They belong to chrome (the status
  row, cards, pickers, the pager's key row), never to a transcript cell: a cell that hides rows ends in `▸` instead
  (§7.1). Each key and shared word lives once in `copy`.
- **Key rows** are in priority order: the main action first, then the other actions, the arrows next to last
  (`↑↓ move`, ASCII `up/down move`) and the exit hint last; the exit mark (`as_exit`) means kept last. Too wide,
  the exit first takes its short form where it has one (`esc stay`), then the hint before it drops, again and
  again; a row without an exit drops from its end; only a lone hint is cut with `…`. A row starts where its
  view's rows start. The pager's row reads `/ search`, the position, the waiting count, the other keys and the way
  back last. A card shown while the turn runs has no stop hint: the status row under it shows `esc to stop`.
- **The retry choice's hint** sits under its rows: `enter choose · ↑↓ move · esc dismiss` (`↑↓ move` left out with
  one row). The choice has the keys only while the composer is empty and nothing else holds them; once you type,
  it stays visible without its hint or selection band, its rows stepped down to `text.muted` (never SGR faint),
  until the composer is empty again. There is no `/retry`.
- **Cost:** `$0.014` per turn, in the footer too; plain-language summaries say `about 1¢, included`
  when a subscription covers it.
- **Cache:** `cache 92% read` per turn; `cache 94%` in the footer.
- **Messages sent while Wisp works** (`docs/plans/steering.md` §6). Enter steers (the message joins the running
  turn at its next step) and Tab queues (it waits for the next turn). Tab queues only when the cursor is at the end
  of the message, its last line has text and no paste chip sits just before the cursor; anywhere else it keeps
  its meaning.
  - **Waiting rows** sit on their own lines between the transcript and the composer, in the order they will be sent
    (`next step`, then `next turn`, then `held`; oldest first within each kind), one per message: ` ◦ ` in
    `text.faint` (ASCII `.`), the label in `text.muted` padded to one width, then the message in `text`, chips in
    `text.muted`: its images first as their chip labels (`[Image #1 · 56×28 · ≈2 tokens]`), then its first non-blank
    line with paste chips as their labels, cut with `…`. The labels are words: `next step` (a steer), `next turn` (a
    queued message) and `held` (a queued message kept after a stop or a failure). At most 5 rows, then `+ N more` in
    `text.faint`. While a picker is open the rows are hidden and the status row's facts start with `N waiting`; in
    transcript mode `N waiting` follows the position in the pager's hint row.
  - **Key hints** lead the status row's facts, each piece its own fact so the last pieces drop first on a
    narrow row, as the other facts do. In order of precedence: `editing a next-turn message · esc cancel · enter
    save` (or `next-step`, or `held`) while a waiting message is open for editing; `enter edit · del remove ·
    tab switch` while the focus is in the rows; `tab queue for the next turn` while Wisp works and the cursor
    is where Tab would queue; `esc to stop · ↑ edit waiting` (ASCII `up edit waiting`) while Wisp works and
    messages wait; and `enter send held · ↑ edit waiting` while idle with held messages.
  - **Working with the rows.** Up on the composer's first visual row moves the focus onto the bottom row (unless the
    composer is showing a history entry, where Up keeps walking history). The focused row swaps ` ◦ ` for the
    prompt glyph (`›`, ASCII `>`) in `accent` and sits on `accent.tint` across the width; the composer's cursor is
    hidden meanwhile. With more than 5 rows the window scrolls to keep the focused row in view, and `+ N more`
    sits above the rows when every hidden one is above, below them otherwise. In the rows, Up and Down move,
    Down past the bottom row returns to the composer and Up past the top row returns to it with the previous
    history entry; Enter or `e` edits, Delete, Backspace or `x` removes, Tab switches between `next turn` and
    `next step`, Esc returns to the composer and Ctrl-C stops the turn as everywhere. Any other key, or a click
    in the composer, returns to the composer and acts there. A row whose edit, switch or removal is still on
    its way to the kernel takes no further edit, switch or removal until the kernel answers. When you remove the
    focused row the focus moves to its neighbour; when it goes for any other reason (delivered, returned by a
    stop) the focus returns to the composer.
  - **Editing** sets the draft aside and opens the message in the composer, its chips and images restored; the
    row stays marked as focused. Enter saves it in place and Esc cancels; either way the draft comes back.
    While editing, Tab, Up and Down act inside the message. If the turn stops or fails while a `next step`
    message is open, the edit becomes ordinary composer text with the draft after it, separated by a blank
    line. Steers returned while a `next turn` or `held` message is open go ahead of the set-aside draft, and
    their `Your unsent steer is back in the composer.` note appears when the edit ends. A removed message
    becomes the newest history entry.
  - **Cell tags** end the last row of a user cell in `text.muted`, at the content edge and at least two spaces
    after the text, joined by ` · `; with no room there they take a row of their own, cut with `…`: `sent while
    working` (a steer, at the step that read it), `queued` (the first message of a queued turn) and `not kept` (a
    message whose turn stopped, failed or kept nothing, so the model never answered it).

---

## 9. Implementation notes for Ratatui

These fit the crate layout proposed in `research/tui-libraries.md` §10.3: pure rendering in `wisp-render`, and the
terminal and its state in `wisp-tui`.

### 9.1 Tokens to `Style`s

```rust
// wisp-render/src/theme/mod.rs: the only module allowed to name a colour (clippy disallowed-methods elsewhere).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    Text, TextMuted, TextFaint, TextDisabled, Accent, Thinking, User, Ok, Warn, WarnGlyph, WarnFill, WarnOn,
    Error, RoleCoordinator, RoleCoder, RoleReviewer, SynString, SynNumber, SynType, OrbRing, OrbRingListen,
    OrbRingSpeak, OrbRingDim, OrbCoreDim, OrbOff, Surface, Sunken, Line, AccentTint, WarnTint, DiffAdd,
    DiffAddWord, DiffDel, DiffDelWord,
}
pub enum Depth { TrueColor, Ansi256, Ansi16, None }
pub enum Mode { Dark, Light }

pub struct Theme { depth: Depth, mode: Mode, rgb: [Rgb; TOKEN_COUNT] /* resolved, §2.7 */ }

impl Theme {
    pub fn resolve(depth: Depth, mode: Mode, probe: &Probe) -> Theme;   // Stone, or corrected (§2.7)
    pub fn fg(&self, t: Token) -> Style;                                // depth-mapped: §2.4, §2.5, §2.6
    pub fn bg(&self, t: Token) -> Style;
    pub fn mix(&self, a: Token, b: Token, u: f32) -> Color;             // animated in-betweens, §6.2
    pub fn body(&self) -> Style { Style::default() }                    // `text` is the terminal's default fg
}
```

- **Two const tables, `DARK_PLUS` and `STONE_LIGHT`,** hold the §2.2 hex values; `ANSI256` and `ANSI16` hold
  §2.4 and §2.5. Generate them from one source, and test that they match this document.
- **`Depth::None` returns `Style`s with no colour,** only `Modifier::{BOLD, ITALIC, UNDERLINED, REVERSED,
  CROSSED_OUT}`. Never `DIM`, in any depth.
- **Lint the palette.** In `clippy.toml`, ban `ratatui::style::Color::Rgb`, `Color::Indexed` and the `Stylize`
  colour methods (`.red()` and the rest) outside `theme`, as Codex does. Give `theme` an `#[allow]`.
- **Cache mixes.** Memoise `Theme::mix` per `(a, b, u quantised to 1/64)`. Animations ask for the same few dozen
  colours over and over.
- **Animated colours in ANSI-16 and `None`** must return the token's own style. That's what makes brightness
  loops still there for free.

### 9.2 Glyphs

`wisp-render/src/theme/glyphs.rs` has `struct Glyphs { prompt: &'static str, … }`, with three constants:
`UNICODE`, `AMBIGUOUS_WIDE` and `ASCII` (§5.1). Pick one at startup: ASCII when `TERM=dumb`, under a `--ascii`
setting, or when the locale isn't UTF-8; ambiguous-wide under the setting, or when the startup width probe
(§9.5) measures `·` as 2. The orb's frames come from a function rather than strings, so the three sets can't
drift:

```rust
pub fn orb(state: Presence, t: Duration, cx: &OrbCtx) -> [(char, Color); 3];   // pure, §6.3
pub fn ring_cells(on: u8 /* bitmask of R0..R7 */) -> (char, char);           // §6.3 table; empty half = ' '
```

**Unit-test the frames against this document**, for example:

```rust
assert_eq!(cells(orb(Thinking, ms(1250))), "⢊⠶⡡");
assert_eq!(cells(orb(Ready, ms(0))), "⢎⠶⡰");
assert_eq!(beat_opacity(ms(160)), 0.997);   // to three places
```

### 9.3 One ticker drives all animation

```rust
pub trait Anim {
    /// The pose to draw at `now` (pure; quantised to this animation's grid, §6.2).
    fn frame(&self, now: Instant, theme: &Theme) -> Frame;
    /// When the output can next change, or None when it is still.
    fn next_change(&self, now: Instant) -> Option<Instant>;
}
```

- **The event loop** is `tokio::select!` over input events, protocol events and
  `sleep_until(min(next_change of every live Anim))`. An idle app has no live `Anim`, so it sleeps until an event
  arrives: **zero frames when idle**.
- **A frame scheduler** in the style of Codex's `FrameRequester` coalesces "dirty" signals into at most one draw
  every 33 ms. Draw only if a widget's output actually changed (compare the frame, then let ratatui's buffer diff
  handle cells).
- **Time comes from the scheduler, never `Instant::now()` inside widgets,** so tests can drive time (paused tokio
  time) and render frame-exact snapshots.
- **Motion policy** is resolved once, and again when it changes: `Full | Calm | Reduced` (§6.1). Reduced makes
  every `Anim` return its still pose and `next_change = None`.
- **Focus.** Enable focus reports (`crossterm::event::EnableFocusChange`) to know when a question should ring the
  bell. Focus never changes the motion policy.
- **Backpressure.** If a write takes more than 50 ms (SSH, ConPTY), halve every animation's rate until writes
  recover.
- **The comet and the thinking arcs share one "mover" slot.** Starting one stops the other (§6.1, rule 3).

### 9.4 Writing frames

- **Wrap every frame** in synchronized output: `crossterm::terminal::BeginSynchronizedUpdate` and
  `EndSynchronizedUpdate` (DEC 2026). This includes resize replays and scrollback inserts. Probe with
  `CSI ? 2026 $ p` to learn whether the terminal really holds frames (§9.5). Emitting it unconditionally is
  harmless, but don't send the *query* to Apple Terminal, which prints a stray `p`.
- **One `write` per frame**, with the cursor hidden. The full-screen presenter writes only the cells that changed,
  and repaints every row after a resize, a resume, a recovered panic, Ctrl-L or with `WISP_FULL_REPAINT=1`.
- **Paint bands with explicit spaces** in the band colour, not "erase line" (§4.4).

### 9.5 Detecting the terminal

1. **Environment, without waiting:** `NO_COLOR`, `FORCE_COLOR`, `CLICOLOR(_FORCE)`, `COLORTERM`, `TERM`,
   `TERM_PROGRAM`, `WT_SESSION`, `TMUX`, `WISP_COLOR`, `WISP_THEME`, `WISP_MOTION`, `WISP_MOUSE`,
   `WISP_SCROLL_SPEED`, `WISP_FULL_REPAINT`, `WISP_COPY_ON_SELECT`. Truecolour when `COLORTERM`
   is `truecolor` or `24bit`, or `WT_SESSION` is set; 256 when `TERM` contains `256`; otherwise 16.
   `WISP_COLOR=truecolor|256|16|none` overrides.
2. **One batched probe, DA1 last:** `OSC 10 ?`, `OSC 11 ?`, `CSI ? 2026 $ p` and `CSI ? 1007 $ p` (both
   skipped under Terminal.app), `CSI ? u` (kitty keyboard), `CSI 6 n` (the cursor), the width probe below, the
   kitty graphics query `APC G i=31,s=1,v=1,a=q,t=d,f=24;AAAA ST` and `CSI 16 t` (the cell size in pixels; both
   skipped under Terminal.app and inside a multiplexer), then `CSI c`. The palette (`OSC 4`) isn't asked yet.
   Wait at most 250 ms locally (600 ms over SSH); a reply that comes later is swallowed. Paint the first frame
   with conservative defaults, and upgrade when the replies land. Replies may end in BEL or ST. `preview.py`'s
   `probe()` is a working reference.
3. **A width probe for the ambiguous glyphs** (optional, at most once per terminal, cached): write `·`, query
   `CSI 6 n`, read the column. If it comes back as 2, switch to the ambiguous-wide set. `preview.py --section
   widths` does this for every glyph.
4. **Cache what you learn** per `TERM_PROGRAM` and `TERM` (not per host), and re-probe after `SIGCONT`.

### 9.6 Restoring the terminal

A guard (`Drop`) plus a panic hook. Undo everything, in reverse order, and keep going even when a step fails:
end synchronized update; SGR reset; show the cursor; re-enable autowrap; disable mouse reports (1000, 1002, 1003, 1006);
put alternate scroll (1007) back when the probe or `WT_SESSION` says it was on; disable focus reports, bracketed
paste and 2031; pop the keyboard flags; leave the alternate screen; restore the termios state. Do the same on
`SIGTERM` and `SIGHUP`. A restore from the background writes the mode resets but no cursor moves or erases. The preview does the subset it uses, and passes Ctrl-C (exit 130), `q` and normal
exit in a pty test.

### 9.7 Pitfalls

- **Ambiguous widths.** `· … → ─ │ ┃ ├ └` are one cell, except in "ambiguous = wide" terminals. Never put an
  ambiguous glyph where a width mistake shifts the rest of the row, unless it's covered by the switch in §9.2.
  Braille and the other glyphs in §5 are always narrow.
- **Emoji presentation.** Characters with the Emoji property (`⏺ ✔ ▪ ↪ ▶` and others) become two-cell emoji in
  some fonts or terminals, and VS16 makes it worse. Test for them (§9.8).
- **Fonts without braille** (Consolas, DejaVu Sans Mono) fall back to another font. That works, but the dots can
  be smaller. That's acceptable; never substitute other glyphs automatically.
- **Bold as bright.** Some terminals brighten bold ANSI-16 text. Don't rely on bold for hierarchy in ANSI-16
  beyond what §2.5 says.
- **Theme detection gaps.** Windows Terminal, Alacritty, WezTerm, Konsole, VS Code and Apple Terminal send no
  2031 notifications, so re-query `OSC 11` on focus-in. GNU screen, mosh and the Linux console answer nothing:
  fall back to `COLORFGBG`, then dark.
- **tmux.**
  - Truecolour needs `terminal-features ',xterm*:RGB'`. Without it, use 256.
  - DEC 2026 passes through from 3.7.
  - Any redraw in a background window raises its activity flag, which is one more reason idle must write nothing.
  - `OSC 11` answers with tmux's idea of the client's colours.
- **Windows Terminal and WSL.** Cascadia Mono has every glyph here. There's no 2031, synchronized output needs
  1.24 or later, and redraw fully after a resize. Conhost has no italics.
- **Escape injection.** Model and tool output can carry cursor moves, `OSC 52` clipboard writes, titles and bidi
  controls. Strip them at ingestion. Only SGR survives, remapped to theme tokens (§7.2), on every path including
  scrollback inserts.
- **The caret and the orb never reach scrollback.** Commit a block only after its caret is removed. Test it: the
  committed bytes contain no U+2836 at the end of a line.
- **Resize** mid-animation: a frame must never be written for an old width. Drop the in-flight frame and redraw.

### 9.8 Tests that keep the brand intact

- **A contrast test** that resolves both themes and asserts every token's floor (§2.2) against its grounds. It
  also runs on the bands fitted to `#000000`, `#1E1E1E`, `#FFFFFF` and `#FDF6E3` (§2.7 step 2); step 3's
  correction is not built yet.
- **A glyph test:** every glyph in all three sets has East Asian width N or A (no W or F), no emoji property, and
  every A glyph has an ambiguous-wide replacement.
- **Frame tests:** the orb's frames, the comet, the reveal and the beat values at the times listed in §6, to three
  decimal places.
- **A snapshot matrix:** dark (light is unmaintained for now and has no snapshots; `docs/testing.md`, "Snapshot themes") × truecolour, 256, 16 and none × Unicode, ambiguous-wide and ASCII × 80
  and 120 columns. Use `insta` on the `TestBackend` buffer, with styles.
- **A frame-budget test:** simulate 10 s of each state with paused time and assert the number of draws (Ready 0;
  Thinking at most 100; the beat at most 70 in 4.8 s).

---

## 10. Open questions for the owner

1. **Medium isn't shipped yet.** The terminal follows the Medium proposal's tempos. If the app settles on Subtle
   or Expressive, change only §6.3's periods; the structure stays.
2. **A still idle** is a deliberate departure from the app's Halo: with nothing running, nothing moves.
   The comet still marks working agents while the root waits (§6.1, §6.3).
3. **The user's accent** colours only the orb's core (§2.7), as in the app. Should agent role colours follow the
   user's accent picker too? This spec says no: roles are identity, not decoration.
4. **The splash's tagline** is the website's hero line. Wisp.Core is the open kernel rather than the hosted
   product, so it may want its own line.
