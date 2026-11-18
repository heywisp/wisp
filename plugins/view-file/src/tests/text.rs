use std::time::Instant;

use super::*;
use crate::look::{MAX_LINE_CHARS, MAX_TEXT_BYTES, SNIFF_BYTES, is_text};
use crate::words::{about_tokens, line_cut, next_span, size, text_limit};

fn numbered(lines: usize) -> String {
    (1..=lines).map(|i| format!("line {i}\n")).collect()
}

fn part_of(viewed: &Viewed) -> (u64, u64, bool) {
    match viewed.run.facts.as_slice() {
        [
            Fact::Part(Part {
                sent,
                total,
                more_than,
            }),
            Fact::Tokens { .. },
        ] => (*sent, *total, *more_than),
        other => panic!("{other:?}"),
    }
}

fn part(viewed: &Viewed) -> (u64, u64) {
    let (sent, total, more_than) = part_of(viewed);
    assert!(!more_than, "{:?}", viewed.run.facts);
    (sent, total)
}

fn more_than(viewed: &Viewed) -> (u64, u64) {
    let (sent, total, more_than) = part_of(viewed);
    assert!(more_than, "{:?}", viewed.run.facts);
    (sent, total)
}

#[tokio::test]
async fn a_short_file_comes_back_whole_and_goes_to_the_output() {
    let rig = Rig::new();
    rig.file("notes.txt", b"one\ntwo\n");
    let viewed = rig.view("notes.txt").await;
    assert_eq!(
        viewed.completed(),
        "~/work/notes.txt: text, 2 lines (8 bytes), all below (about 3 tokens).\none\ntwo"
    );
    assert_eq!(part(&viewed), (2, 2));
    assert!(viewed.run.attachments.is_empty());
    assert_eq!(viewed.captured.lines, 2);
    assert_eq!(viewed.captured.tail, "one\ntwo\n");
    let last = rig.file("last.txt", b"no newline");
    let viewed = rig.view(&last.display().to_string()).await;
    assert!(
        viewed
            .completed()
            .ends_with("all below (about 3 tokens).\nno newline")
    );
    assert_eq!(part(&viewed), (1, 1));
}

#[tokio::test]
async fn an_empty_file_says_so() {
    let rig = Rig::new();
    rig.file("empty.txt", b"");
    let viewed = rig.view("empty.txt").await;
    assert_eq!(
        viewed.completed(),
        "~/work/empty.txt: text, empty (0 bytes)."
    );
    assert_eq!(part(&viewed), (0, 0));
}

#[tokio::test]
async fn a_long_file_stops_at_2000_lines_with_the_command_for_the_rest() {
    let rig = Rig::new();
    let body = numbered(9_412);
    rig.file("logs/build log.txt", body.as_bytes());
    let viewed = rig.view("logs/build log.txt").await;
    let result = viewed.completed();
    let sent: String = numbered(2_000);
    let tokens = about_tokens(sent.len() as u64);
    let header = format!(
        "~/work/logs/build log.txt: text, 9,412 lines ({}); lines 1\u{2013}2,000 below (about {} \
tokens).",
        size(body.len() as u64),
        wisp_tool_text::output::group(tokens)
    );
    assert!(
        result.starts_with(&format!("{header}\nline 1\n")),
        "{result}"
    );
    assert!(
        result.ends_with(
            "line 2000\n[9,412 lines in all; read the next part with bash: sed -n \
'2001,3300p' ~/'work/logs/build log.txt']"
        ),
        "{result}"
    );
    assert_eq!(part(&viewed), (2_000, 9_412));
    assert_eq!(viewed.captured.lines, 2_000);
    assert!(viewed.captured.tail.ends_with("line 2000\n"));
}

#[tokio::test]
async fn a_file_of_long_lines_stops_at_50_kib_on_a_line_end() {
    let rig = Rig::new();
    let line = format!("{}\n", "w".repeat(99));
    rig.file("wide.txt", line.repeat(1_000).as_bytes());
    let viewed = rig.view("wide.txt").await;
    let sent = MAX_TEXT_BYTES / line.len();
    assert_eq!(part(&viewed), (sent as u64, 1_000));
    let result = viewed.completed();
    assert!(
        result.contains(&format!("lines 1\u{2013}{sent} below")),
        "{result}"
    );
    assert!(
        result.contains(&format!("sed -n '{},", sent + 1)),
        "{result}"
    );
    assert_eq!(viewed.captured.bytes, (sent * line.len()) as u64);
}

#[tokio::test]
async fn a_line_over_2000_characters_is_cut() {
    let rig = Rig::new();
    let long = format!("{}\nnext\n", "\u{e9}".repeat(3_000));
    rig.file("one.txt", long.as_bytes());
    let viewed = rig.view("one.txt").await;
    let result = viewed.completed();
    let cut = format!("{}{}\nnext", "\u{e9}".repeat(2_000), line_cut());
    assert!(result.ends_with(&cut), "{result}");
    assert!(
        result.contains("text, 2 lines (")
            && result.contains("); every line below, 1 cut at 2,000 characters (about "),
        "{result}"
    );
    assert!(!result.contains("all below"), "{result}");
    assert_eq!(part(&viewed), (2, 2));
}

#[tokio::test]
async fn a_single_huge_line_is_never_called_all_of_the_file() {
    let rig = Rig::new();
    rig.file("min.js", "x".repeat(40_000).as_bytes());
    let viewed = rig.view("min.js").await;
    let result = viewed.completed();
    assert!(
        result.contains("text, 1 line (40 KB); the line below, 1 cut at 2,000 characters (about "),
        "{}",
        &result[..120]
    );
    assert!(!result.contains("all below"), "{}", &result[..120]);
    assert!(result.ends_with(&format!("{}{}", "x".repeat(2_000), line_cut())));
    assert_eq!(part(&viewed), (1, 1));
}

#[tokio::test]
async fn a_last_line_with_no_newline_after_2000_lines_is_counted() {
    let rig = Rig::new();
    let body = format!("{}tail", numbered(2_000));
    rig.file("partial.txt", body.as_bytes());
    let viewed = rig.view("partial.txt").await;
    let result = viewed.completed();
    assert!(result.contains("text, 2,001 lines ("), "{}", &result[..120]);
    assert!(
        result.ends_with(
            "line 2000\n[2,001 lines in all; read the next part with bash: sed -n '2001,3300p' \
~/'work/partial.txt']"
        ),
        "{}",
        &result[result.len() - 200..]
    );
    assert_eq!(part(&viewed), (2_000, 2_001));
    let exact = format!("{}line 2000", numbered(1_999));
    rig.file("exact.txt", exact.as_bytes());
    let viewed = rig.view("exact.txt").await;
    assert!(
        viewed.completed().contains("text, 2,000 lines (")
            && viewed.completed().contains("all below"),
        "{}",
        &viewed.result()[..120]
    );
    assert_eq!(part(&viewed), (2_000, 2_000));
}

#[test]
fn the_line_count_stops_at_its_time_limit() {
    let rig = Rig::new();
    let path = rig.file("slow.txt", numbered(3_000).as_bytes());
    let host = FakeHost {
        elapsed: Some(Arc::new(|| 1_000)),
        ..rig.host()
    };
    let call = FakeCall::default();
    let job = |count_until_ms| look::Job {
        written: "slow.txt".to_owned(),
        resolved: path.clone(),
        shown: "slow.txt".to_owned(),
        media_types: MEDIA_TYPES.map(str::to_owned).to_vec(),
        host: &host,
        ctx: &call,
        count_until_ms,
        bash: true,
    };
    let Looked::Text(view) = futures::executor::block_on(look::look(&job(1_000))) else {
        panic!("not text");
    };
    assert_eq!(view.lines, 1);
    assert_eq!(view.total, look::Total::MoreThan(1));
    assert_eq!(view.text, "line 1\n");
    let Looked::Text(view) = futures::executor::block_on(look::look(&job(21_000))) else {
        panic!("not text");
    };
    assert_eq!(view.lines, 2_000);
    assert_eq!(view.total, look::Total::Exact(3_000));
}

#[tokio::test]
async fn invalid_utf8_after_the_sniff_is_replaced_and_counted() {
    let rig = Rig::new();
    let mut bytes = numbered(1_500).into_bytes();
    assert!(bytes.len() > SNIFF_BYTES);
    bytes.extend_from_slice(b"bad \xff\xfe end\n");
    rig.file("mixed.log", &bytes);
    let viewed = rig.view("mixed.log").await;
    let result = viewed.completed();
    assert!(
        result.contains(", 2 invalid UTF-8 sequences replaced with U+FFFD), all below"),
        "{}",
        &result[..200]
    );
    assert!(result.ends_with("bad \u{fffd}\u{fffd} end"), "{result}");
}

#[tokio::test]
async fn a_100_mb_file_is_counted_within_the_limit() {
    let rig = Rig::new();
    let start = format!("{}{}", "line\n".repeat(1_000), "a".repeat(5_000));
    rig.sized("huge.txt", start.as_bytes(), 100_000_000);
    let began = Instant::now();
    let viewed = rig.view("huge.txt").await;
    assert!(
        began.elapsed() < Duration::from_secs(15),
        "{:?}",
        began.elapsed()
    );
    let result = viewed.completed();
    assert!(
        result.contains("text, more than 1,000 lines (100 MB); lines 1\u{2013}1,001 below"),
        "{}",
        &result[..120]
    );
    assert!(
        result.contains(
            "[more than 1,000 lines in all; read the next part with bash: sed -n '1002,1002p'"
        ),
        "{}",
        &result[result.len() - 200..]
    );
    assert!(result.contains(&format!("{}{}\n[", "a".repeat(2_000), line_cut())));
    assert!(!result.contains('\u{0}'));
    assert_eq!(more_than(&viewed), (1_001, 1_000));
    let lined = "line\n".repeat(2_000);
    rig.sized("lined.txt", lined.as_bytes(), 100_000_000);
    let viewed = rig.view("lined.txt").await;
    assert!(
        viewed
            .completed()
            .contains("text, more than 2,000 lines (100 MB); lines 1\u{2013}2,000 below"),
        "{}",
        &viewed.result()[..120]
    );
    assert_eq!(more_than(&viewed), (2_000, 2_000));
}

#[tokio::test]
async fn text_that_starts_like_a_bitmap_or_a_script_is_text() {
    let rig = Rig::new();
    rig.file("cars.txt", "BMW and BMX\n".repeat(10).as_bytes());
    rig.file("run.sh", b"#!/bin/sh\necho hi\n");
    for name in ["cars.txt", "run.sh"] {
        let viewed = rig.view(name).await;
        assert!(
            viewed.completed().contains(": text, "),
            "{}",
            viewed.result()
        );
    }
    let bom = rig.file("bom.txt", b"\xEF\xBB\xBFhello\n");
    let viewed = rig.view(&bom.display().to_string()).await;
    assert!(
        viewed.completed().ends_with("\nhello"),
        "{}",
        viewed.result()
    );
}

#[tokio::test]
async fn utf16_is_refused_as_a_type_the_endpoint_can_t_carry() {
    let rig = Rig::new();
    rig.file("wide.txt", b"\xFF\xFEh\x00i\x00\n\x00");
    let viewed = rig.view("wide.txt").await;
    assert!(
        viewed
            .refused()
            .starts_with("wide.txt is application/octet-stream, which this server can't carry"),
        "{}",
        viewed.result()
    );
}

#[test]
fn the_text_check_allows_a_sequence_cut_at_the_sniff_edge_only() {
    let mut head = vec![b'a'; SNIFF_BYTES - 1];
    head.push(0xC3);
    assert!(is_text(&head));
    assert!(!is_text(&head[1..]));
    assert!(!is_text(b"a\x00b"));
    assert!(is_text(b"\xEF\xBB\xBFplain"));
    assert!(!is_text(b"\xff plain"));
}

#[test]
fn estimates_and_sizes_read_like_the_plan() {
    assert_eq!(about_tokens(50_000), 16_800);
    assert_eq!(about_tokens(13_000), 4_400);
    assert_eq!(about_tokens(8), 3);
    assert_eq!(size(1), "1 byte");
    assert_eq!(size(912_000), "912 KB");
    assert_eq!(size(1_600_000), "1.6 MB");
    assert_eq!(size(12_000_000), "12 MB");
    assert_eq!(size(999_949_999), "999.9 MB");
    assert_eq!(size(999_950_000), "1 GB");
    assert_eq!(size(1_260_000_000), "1.3 GB");
    assert_eq!(size(12_345_000_000_000), "12,345 GB");
}

#[tokio::test]
async fn the_next_part_fits_in_one_inline_bash_result() {
    let rig = Rig::new();
    let line = format!("{}\n", "x".repeat(24));
    rig.file("log.txt", line.repeat(3_000).as_bytes());
    let viewed = rig.view("log.txt").await;
    let result = viewed.completed();
    assert!(
        result.ends_with("[3,000 lines in all; read the next part with bash: sed -n '2001,2491p' ~/'work/log.txt']"),
        "{}",
        &result[result.len() - 200..]
    );
    const { assert!(25 * 491 <= wisp_tool_text::output::INLINE_LIMIT) };
}

#[tokio::test]
async fn lines_over_the_cut_size_the_next_part_from_the_bytes_read() {
    let rig = Rig::new();
    let line = format!("{}\n", "a".repeat(3_000));
    rig.file("wide.txt", line.repeat(100).as_bytes());
    let viewed = rig.view("wide.txt").await;
    let result = viewed.completed();
    assert!(
        result.contains("lines 1\u{2013}25 below, 25 cut at 2,000 characters"),
        "{}",
        &result[..160]
    );
    assert!(
        result.ends_with("sed -n '26,29p' ~/'work/wide.txt']"),
        "{}",
        &result[result.len() - 120..]
    );
    assert!(4 * line.len() <= wisp_tool_text::output::INLINE_LIMIT);
    let huge = format!("{}\n", "b".repeat(100_000));
    rig.file("huge.txt", huge.repeat(30).as_bytes());
    let viewed = rig.view("huge.txt").await;
    let result = viewed.completed();
    assert!(
        result.ends_with("sed -n '26,26p' ~/'work/huge.txt']"),
        "{}",
        &result[result.len() - 120..]
    );
}

#[test]
fn the_next_part_span_stays_between_one_line_and_2000() {
    assert_eq!(next_span(25 * 2_000, 2_000), 491);
    assert_eq!(next_span(2 * 2_000, 2_000), 2_000);
    assert_eq!(next_span(20_000, 1), 1);
    assert_eq!(next_span(12_288, 1), 1);
    assert_eq!(next_span(12_287, 1), 1);
    assert_eq!(next_span(6_144, 1), 2);
    assert_eq!(next_span(0, 0), 2_000);
}

#[test]
fn the_tools_doc_states_the_text_limit() {
    let docs = include_str!("../../../../docs/tools.md");
    let chars = group(MAX_LINE_CHARS as u64);
    let limit = format!(
        "**Text:** at most {}, cut at a line end; a line over {chars} characters ends `{}`",
        text_limit(),
        line_cut()
    );
    assert!(docs.contains(&limit), "{limit}");
}
