---
name: review-checklist
description: Checks drawn from defects that passed review and reached the main branch, each with a trigger read from an MR's diff. Read when reviewing an MR, after reading its diff.
---

# Review checklist

Read the diff first. Then, for each check whose trigger the diff matches, do the check and list it under what you checked in your review file, with what you found. A check that fails is a finding, labelled as the flow says. The commits after each check brought in the defects behind it.

- **Screens against their frames.** Trigger: the diff changes what a screen, card, list or command draws. Check: compare each block of the smoke run's screen text (the file your brief names; ask your parent if it names none) with the approved frame of the same width and glyph set, line by line: every line a frame draws, a typed command line included; blank lines; each label against its column at the widest label; a box one row short of fitting. Behind it: c759373b3, 373db3103, 00f5fcf32.
- **Tests that wait or depend on the machine.** Trigger: a new or changed test waits for screen text, an idle or quiet window, a sleep, a log record or a process's exit, or depends on the user, file permissions, timing or load. Check: it waits for something only the awaited step produces, never text the typed input already shows or a fixed time, and it holds when run as root, under the `ci` profile in `.config/nextest.toml` and under load. Behind it: df609bbf0, a12493c2d, a9a85edd8, 866d0e27f, 9dac55363.
- **Finished before every write.** Trigger: the diff adds or changes a step that reports work finished: a barrier, a finish, a shutdown, an ended or settled record. Check: every write it covers has landed before it returns, and a test fails if one lands late. Behind it: dbb6f81ee, 47807c3e9.
- **Time limits.** Trigger: the diff adds or changes a time limit or timeout. Check: a test shows the limit applies, and that an optional step can't use up a required step's limit. Behind it: fbcc7d7eb, 11ec4a1c2.
- **Every kind of input.** Trigger: the diff adds or changes a rule that sorts inputs into kinds, such as HTTP statuses and error shapes, or settings sources. Check: its test table has a row for each kind the input can take, with the documented outcome: for errors, a typed, a coded and a bare 400, 401, 403, 429, other 4xx and 5xx; for settings, each source. Behind it: f2bffdc6d, f8d96a72f.
- **Billed but cut short.** Trigger: the diff touches cost, usage or a lower-bound mark, or a request that can end early: cancelled, failed or cut. Check: on each such path the cost shows as exact only with complete usage, else as a lower bound, with a test for each path. Behind it: ad285bd26, 0c846040c.
- **Mid-turn state across a resume.** Trigger: the diff changes what a session saves, restores, replays or describes: resume, restart, reopen, a window attaching. Check: state held mid-turn or for this session only (a queued message, a running turn's IDs, a switch for this session) survives it, with a test that resumes or reopens mid-turn. Behind it: 0e518789a, b24b08e87, abc7d9122.
