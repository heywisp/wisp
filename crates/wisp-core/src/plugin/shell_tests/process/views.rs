use std::fs;
use std::sync::atomic::AtomicBool;

use regex::Regex;

use super::{ScratchDir, scratch};
use crate::call::{FileTarget, LineState, OutputDir, OutputSink, RING_BYTES};
use crate::jobs::state::Cursor;

mod view {
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use regex::Regex;
    use wisp_plugin_shell::views;

    use crate::call::LineState;
    use crate::jobs::read;
    pub(super) use crate::jobs::read::{Item, LINE_READ, visit};
    use crate::jobs::screen::{ScreenView, screen_record};
    use crate::jobs::search::{Query, find};
    use crate::jobs::state::{Cursor, Ended};

    pub(super) struct Shown {
        pub(super) text: Option<String>,
        pub(super) cursor: Option<Cursor>,
    }

    pub(super) struct Search<'a> {
        pub(super) regex: &'a Regex,
        pub(super) pattern: &'a str,
        pub(super) from: u64,
        pub(super) context: u64,
        pub(super) max: u64,
    }

    pub(super) fn read_new(
        lines: &LineState,
        cursor: Cursor,
        label: &str,
        home: Option<&Path>,
    ) -> Shown {
        Shown {
            text: views::new_text(&read::unread(lines, cursor), label, home),
            cursor: Some(read::end_cursor(lines)),
        }
    }

    pub(super) fn no_new_output(lines: &LineState) -> String {
        views::no_new_text(lines.held.as_deref())
    }

    pub(super) fn read_tail(lines: &LineState, n: u64, home: Option<&Path>) -> Shown {
        Shown {
            text: Some(views::tail_text(&read::tail(lines, n), home)),
            cursor: Some(read::end_cursor(lines)),
        }
    }

    pub(super) fn read_lines(lines: &LineState, from: u64, n: u64) -> String {
        views::rows_text(&read::rows(lines, from, n), from)
    }

    pub(super) fn search(lines: &LineState, s: &Search<'_>, stop: &AtomicBool) -> Option<String> {
        let query = Query {
            regex: s.regex,
            from: s.from,
            context: s.context,
            max: s.max,
        };
        let found = find(lines, &query, stop)?;
        Some(views::found_text(&found, s.pattern, s.from, s.context))
    }

    pub(super) fn screen_lines(view: &ScreenView) -> Vec<String> {
        views::screen_rows_text(&screen_record(view, true).rows)
    }

    pub(super) fn ended_sentence(id: u32, ended: &Ended) -> String {
        wisp_tool_text::jobs::ended_sentence(id, &ended.job_end())
    }

    pub(super) fn state_words(end: Option<&Ended>) -> String {
        views::state_words(end.map(Ended::job_end).as_ref())
    }

    pub(super) fn state_sentence(id: u32, end: Option<&Ended>) -> String {
        views::state_sentence(id, end.map(Ended::job_end).as_ref())
    }
}

use view::Search;

struct Fed {
    _scratch: ScratchDir,
    sink: OutputSink,
}

impl Fed {
    async fn new(name: &str, bytes: &[u8]) -> Self {
        let scratch = scratch(name);
        let out = OutputDir::new(&scratch.root.join("tmp"), "s1");
        let target: FileTarget = out
            .create(1, "bash")
            .map(|(p, f)| (p, Box::new(f) as Box<dyn std::io::Write + Send>))
            .map_err(|e| e.to_string());
        let mut sink = OutputSink::new("c", None, target);
        sink.keep_lines();
        sink.write(bytes).await;
        Self {
            _scratch: scratch,
            sink,
        }
    }

    fn lines(&self) -> LineState {
        self.sink.line_state()
    }

    fn path(&self) -> String {
        self.sink.file_path().unwrap().display().to_string()
    }
}

fn numbered(from: u64, to: u64) -> String {
    (from..=to).map(|n| format!("{n}\n")).collect()
}

fn search(lines: &LineState, pattern: &str, from: u64, context: u64, max: u64) -> String {
    let regex = Regex::new(pattern).unwrap();
    view::search(
        lines,
        &Search {
            regex: &regex,
            pattern,
            from,
            context,
            max,
        },
        &AtomicBool::new(false),
    )
    .unwrap()
}

fn sed(path: &str, n: usize) -> String {
    let raw = fs::read(path).unwrap();
    let line = raw.split(|b| *b == b'\n').nth(n - 1).unwrap();
    crate::call::Cleaner::line_text(line)
}

#[tokio::test]
async fn new_output_moves_past_complete_lines_and_shows_the_held_one() {
    let fed = Fed::new("new", b"a\nb\n>>> ").await;
    let lines = fed.lines();
    let shown = view::read_new(&lines, Cursor::default(), "New output", None);
    assert_eq!(
        shown.text.as_deref(),
        Some("New output (lines 1–2):\na\nb\nLine 3 (incomplete): >>> ")
    );
    let cursor = shown.cursor.unwrap();
    assert_eq!(cursor, Cursor { line: 2, bytes: 4 });
    let again = view::read_new(&lines, cursor, "New output", None);
    assert_eq!(again.text, None);
    assert_eq!(
        view::no_new_output(&lines),
        "No new output; the last line is still incomplete: >>> "
    );
}

#[tokio::test]
async fn a_progress_bar_shows_its_latest_state_and_then_one_line() {
    let mut fed = Fed::new("progress", b"10%\r").await;
    assert_eq!(
        view::no_new_output(&fed.lines()),
        "No new output; the last line is still incomplete: 10%"
    );
    fed.sink.write(b"50%\r").await;
    assert_eq!(
        view::no_new_output(&fed.lines()),
        "No new output; the last line is still incomplete: 50%"
    );
    fed.sink.write(b"100%\n").await;
    let lines = fed.lines();
    let shown = view::read_new(&lines, Cursor::default(), "New output", None);
    assert_eq!(shown.text.as_deref(), Some("New output (line 1):\n100%"));
    assert_eq!(view::read_lines(&lines, 1, 5), "Line 1:\n100%");
    assert_eq!(sed(&fed.path(), 1), "100%");
}

#[tokio::test]
async fn tail_and_lines_match_the_file() {
    let fed = Fed::new("tail", numbered(1, 10).as_bytes()).await;
    let lines = fed.lines();
    let tail = view::read_tail(&lines, 3, None);
    assert_eq!(
        tail.text.as_deref(),
        Some("Last lines (lines 8–10):\n8\n9\n10")
    );
    assert_eq!(
        tail.cursor,
        Some(Cursor {
            line: 10,
            bytes: 21
        })
    );
    assert_eq!(view::read_lines(&lines, 4, 2), "Lines 4–5:\n4\n5");
    assert_eq!(view::read_lines(&lines, 9, 100), "Lines 9–10:\n9\n10");
    assert_eq!(
        view::read_lines(&lines, 99, 3),
        "There is no line 99: the output has 10 lines so far."
    );
    for n in [1, 5, 10] {
        assert_eq!(sed(&fed.path(), n), n.to_string());
    }
    let empty = Fed::new("tail-empty", b"").await;
    assert_eq!(
        view::read_tail(&empty.lines(), 50, None).text.as_deref(),
        Some("Output: (none)")
    );
}

#[tokio::test]
async fn lines_stop_at_sixteen_kib_and_say_where_to_continue() {
    let line = "x".repeat(999);
    let text: String = (0..40).map(|_| format!("{line}\n")).collect();
    let fed = Fed::new("cap", text.as_bytes()).await;
    let read = view::read_lines(&fed.lines(), 1, 100);
    assert!(read.starts_with("Lines 1–16:\n"), "{read}");
    assert!(
        read.ends_with("[… stopped at 16,384 bytes; continue with from 17 …]"),
        "{read}"
    );
}

#[tokio::test]
async fn long_new_output_is_previewed_with_offset_line_numbers() {
    let fed = Fed::new("preview", numbered(1, 9000).as_bytes()).await;
    let lines = fed.lines();
    let shown = view::read_new(
        &lines,
        Cursor {
            line: 1000,
            bytes: 3893,
        },
        "New output",
        None,
    );
    let text = shown.text.unwrap();
    let mut rows = text.lines();
    assert_eq!(
        rows.next(),
        Some(
            format!(
                "New output: 40,000 bytes, lines 1,001–9,000, in full in {}",
                fed.path()
            )
            .as_str()
        )
    );
    assert!(rows.next().unwrap().starts_with("Lines 1,001–"));
    assert_eq!(rows.next(), Some("1001"));
    assert!(text.contains("not shown; read or search them with the process tool …]"));
    assert!(text.ends_with("\n9000"));
}

#[tokio::test]
async fn search_numbers_merges_context_and_caps() {
    let mut text = String::new();
    for n in 1..=30u64 {
        let line = match n {
            3 | 5 | 20 => format!("error {n}"),
            12 => "Error twelve".to_owned(),
            _ => format!("ok {n}"),
        };
        text.push_str(&line);
        text.push('\n');
    }
    let fed = Fed::new("search", text.as_bytes()).await;
    let lines = fed.lines();
    assert_eq!(
        search(&lines, "error", 1, 0, 50),
        "Searched lines 1–30 for /error/: 3 matching lines.\n3:error 3\n5:error 5\n20:error 20"
    );
    assert_eq!(
        search(&lines, "error", 1, 1, 50),
        "Searched lines 1–30 for /error/ with 1 line of context: 3 matching lines.\n2-ok 2\n3:error 3\n4-ok 4\n5:error 5\n6-ok 6\n--\n19-ok 19\n20:error 20\n21-ok 21"
    );
    assert_eq!(
        search(&lines, "(?i)^error", 1, 0, 2),
        "Searched lines 1–30 for /(?i)^error/: 4 matching lines, the first 2 shown.\n3:error 3\n5:error 5\n[… 2 more matching lines after line 5; search again with from 6 …]"
    );
    assert_eq!(
        search(&lines, "(?i)^error", 6, 0, 2),
        "Searched lines 6–30 for /(?i)^error/: 2 matching lines.\n12:Error twelve\n20:error 20"
    );
    assert_eq!(
        search(&lines, "nothing", 1, 0, 50),
        "Searched lines 1–30 for /nothing/: no matching lines."
    );
    assert_eq!(
        search(&lines, "x", 31, 0, 50),
        "There is nothing to search from line 31: the output has 30 lines so far."
    );
    assert_eq!(
        search(&lines, "error", 1, 1, 50),
        search(&lines, "error", 1, 1, 50)
    );
}

#[tokio::test]
async fn search_stops_at_sixteen_kib_of_matches() {
    let line = "match ".repeat(150);
    let text: String = (0..60).map(|_| format!("{line}\n")).collect();
    let fed = Fed::new("search-cap", text.as_bytes()).await;
    let found = search(&fed.lines(), "match", 1, 0, 500);
    let last = found.lines().last().unwrap();
    assert!(
        last.starts_with("[… ") && last.contains("more matching lines after line"),
        "{last}"
    );
    assert!(found.len() < 17 * 1024, "{}", found.len());
    let shown = found.lines().filter(|l| l.contains(":match")).count();
    let continued = search(&fed.lines(), "match", shown as u64 + 1, 0, 500);
    assert!(
        continued.starts_with(&format!("Searched lines {}–60", shown + 1)),
        "{continued}"
    );
}

#[tokio::test]
async fn reads_beyond_the_ring_come_from_the_file_or_say_they_are_gone() {
    let line = "y".repeat(99);
    let mut text = String::from("first needle\n");
    for _ in 0..(2 * RING_BYTES / 100) {
        text.push_str(&line);
        text.push('\n');
    }
    let fed = Fed::new("ring", text.as_bytes()).await;
    let lines = fed.lines();
    assert!(lines.ring.first() > 1);
    assert_eq!(view::read_lines(&lines, 1, 1), "Line 1:\nfirst needle");
    assert!(search(&lines, "needle", 1, 0, 50).contains("\n1:first needle"));
    fs::remove_file(fed.path()).unwrap();
    let first = lines.ring.first();
    let gone = view::read_lines(&lines, 1, 1);
    assert_eq!(
        gone,
        "Line 1:\n[… line 1 is no longer kept …]".to_owned(),
        "first kept line is {first}"
    );
    let found = search(&lines, "needle", 1, 0, 50);
    assert!(
        found.contains(&format!(
            "[… lines 1–{} are no longer kept and weren't searched …]",
            wisp_tool_text::output::group(first - 1)
        )),
        "{found}"
    );
}

#[test]
fn end_sentences_name_each_kind_of_end() {
    use wisp_protocol::{ExitStatus, ProcessEnd, StoppedBy};
    let ended = crate::jobs::state::Ended::of;
    let cases = [
        (
            ProcessEnd::Exited {
                exit: ExitStatus::Code { code: 1 },
            },
            "Process 4 exited with code 1.",
            "exited with code 1",
        ),
        (
            ProcessEnd::Exited {
                exit: ExitStatus::Signal { signal: 9 },
            },
            "Process 4 was ended by signal 9.",
            "ended by signal 9",
        ),
        (
            ProcessEnd::Stopped {
                by: StoppedBy::TimeLimit { after_ms: 600_000 },
            },
            "Process 4 was stopped at its 600 s time limit.",
            "stopped at its 600 s limit",
        ),
        (
            ProcessEnd::Stopped {
                by: StoppedBy::User,
            },
            "Process 4 was stopped by the user.",
            "stopped by the user",
        ),
        (
            ProcessEnd::Stopped {
                by: StoppedBy::Model,
            },
            "Process 4 was stopped.",
            "stopped",
        ),
    ];
    for (end, sentence, words) in cases {
        let ended = ended(end);
        assert_eq!(view::ended_sentence(4, &ended), sentence);
        assert_eq!(view::state_words(Some(&ended)), words);
    }
    assert_eq!(view::state_sentence(4, None), "Process 4 is running.");
}

#[tokio::test]
async fn a_giant_line_is_read_in_bounded_pieces() {
    let mut text = "z".repeat(5 * view::LINE_READ);
    text.push_str("\nafter\n");
    let mut fed = Fed::new("giant", text.as_bytes()).await;
    fed.sink.write(&"y".repeat(10).into_bytes()).await;
    let lines = fed.lines();
    let mut seen = Vec::new();
    view::visit(&lines, 1, 2, |item| {
        if let view::Item::Line(n, text) = item {
            seen.push((n, text.len()));
        }
        std::ops::ControlFlow::Continue(())
    });
    assert_eq!(seen, [(1, view::LINE_READ), (2, 5)]);
}

#[test]
fn a_screen_read_stays_under_the_inline_limit() {
    use crate::jobs::screen::ScreenView;
    use wisp_protocol::TermSize;
    let full = ScreenView {
        rows: (0..200).map(|n| format!("{n:0>500}")).collect(),
        cursor: (0, 0),
        size: TermSize {
            cols: 500,
            rows: 200,
        },
        alternate: true,
    };
    let lines = view::screen_lines(&full);
    let text = lines.join("\n");
    assert!(
        text.len() <= wisp_tool_text::output::INLINE_LIMIT + 200,
        "{}",
        text.len()
    );
    assert_eq!(lines.len(), 1 + 32 + 1);
    assert_eq!(
        lines.last().unwrap(),
        "[… rows 33–200 not shown: a screen read shows at most 16 KiB; resize the terminal smaller, or read tail …]"
    );
    let small = ScreenView {
        rows: vec!["one".into(), "two".into()],
        ..full
    };
    assert_eq!(view::screen_lines(&small), ["Screen:", "one", "two"]);
}
