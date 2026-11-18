use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::*;
use crate::test_guard::{ScratchDir, protocol_fixtures};
use wisp_protocol::{ExitStatus, NotRunReason};
use wisp_tool_text::output::{
    HEAD_LIMIT, group, head_cut, newlines, output_section, tail_cut, time_limit_text,
};

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("wisp-output-test-", name)
}

fn sink_in(dir: &OutputDir, number: u32) -> OutputSink {
    let target: FileTarget = dir
        .create(number, "t")
        .map(|(p, f)| (p, Box::new(f) as Box<dyn Write + Send>))
        .map_err(|e| e.to_string());
    OutputSink::new("c1", None, target)
}

async fn capture(bytes: &[u8]) -> Captured {
    let mut sink = OutputSink::detached("c");
    sink.write(bytes).await;
    sink.finish().await
}

async fn cleaned(bytes: &[u8]) -> String {
    capture(bytes).await.head
}

fn numbered_lines(n: usize) -> String {
    (1..=n)
        .map(|i| format!("line {i}: kestrel-{i}\n"))
        .collect()
}

#[tokio::test]
async fn small_output_is_inlined_whole_and_the_file_keeps_raw_bytes() {
    let root = scratch("small");
    let dir = OutputDir::new(&root.root, "s1");
    let mut sink = sink_in(&dir, 7);
    sink.write(b"a\x1b[31mred\x1b[0m\r\nb\n").await;
    let captured = sink.finish().await;
    let path = dir.path().join("0007-t.out");
    assert_eq!(captured.path.as_deref(), Some(path.as_path()));
    assert_eq!(fs::read(&path).unwrap(), b"a\x1b[31mred\x1b[0m\r\nb\n");
    assert_eq!(captured.head, "ared\nb\n");
    assert!(captured.inlined());
    let text = result_text(
        &ToolOutcome::Completed {
            is_error: false,
            exit: Some(ExitStatus::Code { code: 0 }),
        },
        &captured,
        None,
        &[],
    );
    assert_eq!(text, "Exit code: 0\nOutput:\nared\nb");
    let view = output_view(&captured);
    assert!(view.inlined);
    assert_eq!((view.bytes, view.lines), (7, 2));
    assert_eq!(view.file.as_deref(), Some(path.to_str().unwrap()));
    assert_eq!(view.file_problem, None);
}

#[tokio::test]
async fn empty_output_says_none() {
    let captured = capture(b"").await;
    assert_eq!(output_section(&captured, None), "Output: (none)");
    assert_eq!(captured.lines, 0);
}

#[tokio::test]
async fn the_directory_is_700_and_files_are_600() {
    let root = scratch("modes");
    let dir = OutputDir::new(&root.root, "s1");
    let (path, _) = dir.create(1, "t").unwrap();
    let mode = |p: &Path| fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(dir.path()), 0o700);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(dir.path().file_name().unwrap(), "wisp-s1");
    assert_eq!(path.file_name().unwrap(), "0001-t.out");
}

#[tokio::test]
async fn large_output_gets_a_head_a_tail_and_the_path_and_the_figures_add_up() {
    let root = scratch("large");
    let dir = OutputDir::new(&root.root, "s1");
    let mut sink = sink_in(&dir, 1);
    let body = numbered_lines(2000);
    for chunk in body.as_bytes().chunks(997) {
        sink.write(chunk).await;
    }
    let captured = sink.finish().await;
    assert!(!captured.inlined());
    assert_eq!(
        fs::read_to_string(dir.path().join("0001-t.out")).unwrap(),
        body
    );
    assert_eq!(captured.bytes, body.len() as u64);
    assert_eq!(captured.lines, 2000);
    let text = output_section(&captured, None);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        format!(
            "Output: {} bytes, 2,000 lines, saved in full to {}",
            group(body.len() as u64),
            dir.path().join("0001-t.out").display()
        )
    );
    let (head, _) = head_cut(&captured.head);
    let (tail, _) = tail_cut(&captured.tail);
    assert!(head.len() <= HEAD_LIMIT && head.ends_with('\n'));
    assert!(tail.len() <= TAIL_LIMIT && tail.starts_with("line "));
    let head_lines = newlines(head);
    let tail_lines = newlines(tail);
    assert_eq!(lines[1], format!("Lines 1–{head_lines}:"));
    let tail_first = 2000 - tail_lines + 1;
    let omitted = body.len() - head.len() - tail.len();
    assert_eq!(
        lines[2 + head_lines as usize],
        format!(
            "[… lines {}–{} ({} bytes) not shown; read them from the file …]",
            head_lines + 1,
            group(tail_first - 1),
            group(omitted as u64)
        )
    );
    assert_eq!(
        lines[3 + head_lines as usize],
        format!("Lines {}–2,000:", group(tail_first))
    );
    assert_eq!(text.lines().last(), Some("line 2000: kestrel-2000"));
    assert_eq!(
        head_lines + (tail_first - 1 - head_lines) + tail_lines,
        2000
    );
    assert!(body.starts_with(head) && body.ends_with(tail));
}

#[tokio::test]
async fn a_character_straddling_either_cut_is_never_split() {
    let mut text = "x".repeat(HEAD_LIMIT - 1);
    text.push('é');
    text.push_str(&"y".repeat(INLINE_LIMIT));
    text.push('€');
    text.push_str(&"z".repeat(TAIL_LIMIT - 2));
    let captured = capture(text.as_bytes()).await;
    let (head, aligned) = head_cut(&captured.head);
    assert!(!aligned);
    assert_eq!(head.len(), HEAD_LIMIT - 1);
    let (tail, aligned) = tail_cut(&captured.tail);
    assert!(!aligned);
    assert_eq!(tail, "z".repeat(TAIL_LIMIT - 2));
    let section = output_section(&captured, None);
    assert!(section.contains("The first 1,023 bytes of line 1:"));
    assert!(
        section.contains("The last 3,070 bytes of line 1:"),
        "{section}"
    );
    assert!(section.contains("head -c or tail -c"));
}

#[tokio::test]
async fn a_tail_starting_mid_line_names_the_line_and_its_bytes() {
    let mut text = numbered_lines(2000);
    text.push_str(&"w".repeat(5000));
    text.push('\n');
    let captured = capture(text.as_bytes()).await;
    let section = output_section(&captured, None);
    assert!(
        section.contains("The last 3,071 bytes of line 2,001:"),
        "{section}"
    );
    assert!(section.contains(", including lines "), "{section}");
    assert_eq!(captured.lines, 2001);
}

#[tokio::test]
async fn a_single_huge_line_streams_with_bounded_memory() {
    let root = scratch("huge");
    let dir = OutputDir::new(&root.root, "s1");
    let mut sink = sink_in(&dir, 1);
    let chunk = "ab€".repeat(64 * 1024 / 5);
    let mut total = 0u64;
    while total < 50 * 1024 * 1024 {
        sink.write(chunk.as_bytes()).await;
        total += chunk.len() as u64;
        assert!(sink.clean.line.len() <= LINE_HOLD + 4);
        assert!(sink.clean.out.tail.len() <= 4 * TAIL_LIMIT + LINE_HOLD);
        assert!(sink.clean.out.head.len() <= IN_MEMORY_HEAD);
    }
    let captured = sink.finish().await;
    assert_eq!(captured.bytes, total);
    assert_eq!(captured.lines, 1);
    assert_eq!(
        fs::metadata(dir.path().join("0001-t.out")).unwrap().len(),
        total
    );
    let (head, _) = head_cut(&captured.head);
    let (tail, _) = tail_cut(&captured.tail);
    assert!(head.len() <= HEAD_LIMIT && tail.len() <= TAIL_LIMIT);
}

#[tokio::test]
async fn the_cap_drains_but_does_not_write_and_says_what_is_missing() {
    let root = scratch("cap");
    let dir = OutputDir::new(&root.root, "s1");
    let mut sink = sink_in(&dir, 1).with_cap(100);
    let body = numbered_lines(2000);
    sink.write(body.as_bytes()).await;
    let captured = sink.finish().await;
    assert_eq!(
        fs::read(dir.path().join("0001-t.out")).unwrap(),
        &body.as_bytes()[..100]
    );
    assert_eq!(captured.bytes, body.len() as u64);
    let dropped = body.len() as u64 - 100;
    assert_eq!(
        captured.problem,
        Some(FileProblem::Capped { cap: 100, dropped })
    );
    let section = output_section(&captured, None);
    assert!(section.contains("Output file is incomplete"), "{section}");
    assert!(section.contains(&group(dropped)), "{section}");
    let view = output_view(&captured);
    assert!(view.file.is_some() && view.file_problem.is_some());
}

struct FailAfter(usize);

impl Write for FailAfter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.0 == 0 {
            return Err(io::Error::other("disk full"));
        }
        let n = buf.len().min(self.0);
        self.0 -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_write_failure_keeps_draining_and_reports_an_incomplete_file() {
    let target: FileTarget = Ok((PathBuf::from("/x/0001-t.out"), Box::new(FailAfter(10))));
    let mut sink = OutputSink::new("c", None, target);
    let body = numbered_lines(2000);
    for chunk in body.as_bytes().chunks(10) {
        sink.write(chunk).await;
    }
    let captured = sink.finish().await;
    assert_eq!(captured.bytes, body.len() as u64);
    assert_eq!(
        captured.problem,
        Some(FileProblem::WriteFailed {
            error: "disk full".into(),
            after: 10
        })
    );
    let section = output_section(&captured, None);
    assert!(
        section.contains("Output file is incomplete: disk full after 10 bytes"),
        "{section}"
    );
    assert!(section.ends_with("line 2000: kestrel-2000"));
}

#[tokio::test]
async fn an_unwritable_temp_dir_still_gives_the_preview() {
    let root = scratch("unwritable");
    let blocker = root.root.join("a-file");
    fs::write(&blocker, "not a directory").unwrap();
    let dir = OutputDir::new(&blocker, "s1");
    let error = dir.create(1, "t").unwrap_err().to_string();
    assert!(!dir.path().exists());
    let mut sink = OutputSink::new("c", None, Err(error));
    let body = numbered_lines(2000);
    sink.write(body.as_bytes()).await;
    let captured = sink.finish().await;
    let section = output_section(&captured, None);
    assert!(
        section.contains("Output file could not be written:")
            && section.contains("only this preview exists"),
        "{section}"
    );
    assert!(section.ends_with("line 2000: kestrel-2000"));
    assert_eq!(output_view(&captured).file, None);
}

#[test]
fn existing_directories_are_accepted_only_when_private_and_ours() {
    let root = scratch("existing");
    let dir = OutputDir::new(&root.root, "s1");
    fs::create_dir(dir.path()).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let error = dir.ensure().unwrap_err().to_string();
    assert!(error.contains("mode is 755"), "{error}");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir.ensure().unwrap();
    assert_eq!(
        fs::metadata(dir.path()).unwrap().uid(),
        rustix::process::geteuid().as_raw()
    );

    let elsewhere = scratch("elsewhere");
    fs::set_permissions(&elsewhere.root, fs::Permissions::from_mode(0o700)).unwrap();
    let linked = OutputDir::new(&root.root, "s2");
    std::os::unix::fs::symlink(&elsewhere.root, linked.path()).unwrap();
    let error = linked.ensure().unwrap_err().to_string();
    assert!(error.contains("symlink"), "{error}");

    let file = OutputDir::new(&root.root, "s3");
    fs::write(file.path(), "x").unwrap();
    let error = file.ensure().unwrap_err().to_string();
    assert!(error.contains("not a directory"), "{error}");
}

#[test]
fn files_are_never_opened_over_something_already_there() {
    let root = scratch("files");
    let dir = OutputDir::new(&root.root, "s1");
    dir.ensure().unwrap();
    fs::write(dir.path().join("0001-t.out"), "planted").unwrap();
    assert_eq!(
        dir.create(1, "t").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    let target = root.root.join("target");
    fs::write(&target, "keep").unwrap();
    std::os::unix::fs::symlink(&target, dir.path().join("0002-t.out")).unwrap();
    assert_eq!(
        dir.create(2, "t").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
    assert_eq!(
        fs::read_to_string(dir.path().join("0001-t.out")).unwrap(),
        "planted"
    );
}

#[test]
fn a_vanished_directory_is_made_again() {
    let root = scratch("vanish");
    let dir = OutputDir::new(&root.root, "s1");
    dir.create(1, "t").unwrap();
    fs::remove_dir_all(dir.path()).unwrap();
    dir.create(2, "t").unwrap();
    assert!(dir.path().join("0002-t.out").is_file());
}

#[tokio::test]
async fn a_path_under_home_is_shown_with_a_tilde() {
    let home = scratch("home");
    let temp = home.root.join("tmp");
    fs::create_dir(&temp).unwrap();
    let dir = OutputDir::new(&temp, "s1");
    let mut sink = sink_in(&dir, 1);
    sink.write(numbered_lines(2000).as_bytes()).await;
    let captured = sink.finish().await;
    let section = output_section(&captured, Some(&home.root));
    assert!(
        section.contains("saved in full to ~/tmp/wisp-s1/0001-t.out"),
        "{section}"
    );
}

#[tokio::test]
async fn cleaning_collapses_progress_and_removes_escapes_but_keeps_newlines() {
    assert_eq!(cleaned(b"10%\r50%\r100%\ndone\n").await, "100%\ndone\n");
    assert_eq!(cleaned(b"a\r\nb\r\n").await, "a\nb\n");
    assert_eq!(cleaned(b"50%\r\x1b[K60%\n").await, "60%\n");
    assert_eq!(
        cleaned(b"\x1b]0;title\x07ok \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\\n").await,
        "ok link\n"
    );
    assert_eq!(cleaned(b"\x1b(Bplain\x07\x08\n").await, "plain\n");
    assert_eq!(cleaned("a\u{9b}31mb\n".as_bytes()).await, "ab\n");
    assert_eq!(
        cleaned(b"bad \xff\xfe end\n").await,
        "bad \u{fffd}\u{fffd} end\n"
    );
    assert_eq!(cleaned(b"tab\there\n").await, "tab\there\n");
    assert_eq!(cleaned(b"trailing\r").await, "");
    assert_eq!(
        cleaned(b"HTTP/1.1 200 OK\r\r\nServer: x\r\r\n\r\r\nbody\r\n").await,
        "HTTP/1.1 200 OK\nServer: x\n\nbody\n"
    );
    assert_eq!(
        cleaned(b"HTTP/1.1 200 OK\r\nServer: x\r\n\r\nbody\n").await,
        "HTTP/1.1 200 OK\nServer: x\n\nbody\n"
    );
    assert_eq!(cleaned(b"old\r\rnew\n").await, "new\n");
    let text = b"x\n\x1b[1my\n\r\n\rz";
    assert_eq!(
        capture(text).await.lines,
        newlines(std::str::from_utf8(text).unwrap()) + 1
    );
}

#[tokio::test]
async fn a_character_split_across_writes_is_decoded_whole() {
    let mut sink = OutputSink::detached("c");
    let bytes = "é€😀".as_bytes();
    for b in bytes {
        sink.write(std::slice::from_ref(b)).await;
    }
    assert_eq!(sink.finish().await.head, "é€😀");
    let mut sink = OutputSink::detached("c");
    sink.write(&bytes[..1]).await;
    assert_eq!(sink.finish().await.head, "\u{fffd}");
}

#[tokio::test]
async fn split_keys_are_masked_before_the_file_events_and_capture() {
    fn oracle(bytes: &[u8], values: &[&str], hidden: usize) -> Vec<u8> {
        let mut covered = vec![false; bytes.len()];
        covered[..hidden].fill(true);
        for value in values
            .iter()
            .filter(|value| value.len() >= crate::session::MIN_REDACTION_CHARS)
        {
            for at in 0..bytes.len() {
                if bytes[at..].starts_with(value.as_bytes()) {
                    covered[at..at + value.len()].fill(true);
                }
            }
        }
        let mut out = Vec::new();
        let mut hiding = false;
        for (at, &byte) in bytes.iter().enumerate() {
            if covered[at] {
                if !hiding {
                    out.extend_from_slice(crate::session::REDACTED.as_bytes());
                }
            } else {
                out.push(byte);
            }
            hiding = covered[at];
        }
        out
    }

    fn collapsed(bytes: &[u8]) -> Vec<u8> {
        let marker = crate::session::REDACTED.as_bytes();
        let pair = [marker, marker].concat();
        let mut out = bytes.to_vec();
        while let Some(at) = out.windows(pair.len()).position(|part| part == pair) {
            drop(out.drain(at..at + marker.len()));
        }
        out
    }

    fn feed(
        list: &crate::session::Redactions,
        bytes: &[u8],
        pending: &mut crate::session::MaskTail,
        capped: &mut bool,
        bound: usize,
    ) -> Vec<u8> {
        let out = list.mask_chunk(bytes, pending).0;
        *capped |= pending.covered > 0;
        assert!(pending.bytes.len() <= bound);
        assert!(pending.covered <= pending.bytes.len());
        out
    }

    async fn check(
        mut sink: OutputSink,
        mut rx: mpsc::Receiver<AgentEvent>,
        expected: &str,
        masked: bool,
    ) {
        let captured = sink.finish().await;
        assert_eq!(captured.head, expected);
        assert_eq!(sink.masked(), masked);
        assert_eq!(
            fs::read(captured.path.unwrap()).unwrap(),
            expected.as_bytes()
        );
        drop(sink);
        let mut joined = String::new();
        while let Some(event) = rx.recv().await {
            if let AgentEvent::ToolOutput { text, .. } = event {
                joined.push_str(&text);
            }
        }
        assert_eq!(joined, expected);
    }

    let root = scratch("split-held-key");
    let dir = OutputDir::new(&root.root, "s1");
    let fake = "CANARY-output-stream-credential";
    let long = "CANARY-long-credential-END";
    let inside = "CANARY-abcdefghijklmnop";
    let outside = "yCANARY-abcdefghijklmnop-more";
    let crossing = "CANARY-crossing-credential-END";
    let base64 = "CANARY-base64-overlap-credential";
    let repeated = "CANARYCANARYCANARYCANARY";
    let private = "sk-CANARY-private-key-credential";
    crate::session::register_secrets(
        [
            fake, long, inside, outside, crossing, base64, repeated, private,
        ]
        .map(str::to_owned),
    );
    let held = crate::session::held();
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, base64);
    let bodies: [(String, &[&str]); 13] = [
        (format!("before {fake} after\n"), &[]),
        ("no held value é€😀\n".to_owned(), &[]),
        ("last CANARY-output".to_owned(), &[]),
        (long.to_owned(), &["long"]),
        (outside.to_owned(), &[]),
        ("CANARY-long".to_owned(), &["long"]),
        (format!("x{crossing}"), &["xCANARY"]),
        ("xCANARY-cro!".to_owned(), &["xCANARY"]),
        (encoded.clone(), &[&encoded[8..12]]),
        ("ababababab".to_owned(), &["aba", "bab"]),
        (repeated.repeat(2), &[]),
        ("AAAaaa".to_owned(), &["]aaa", "AAA", "]aa"]),
        (format!("key={private}"), &["=sk"]),
    ];
    let mut number = 0;
    for (body, typed) in bodies {
        let redactions = held.union([&crate::session::Redactions::typed(
            typed.iter().map(|value| (*value).to_owned()),
        )]);
        let expected = redactions.mask(&body);
        for split in 0..=body.len() {
            number += 1;
            let (tx, rx) = mpsc::channel(16);
            let mut sink = sink_in(&dir, number);
            sink.events = Some(tx);
            sink.hold(typed.iter().map(|value| (*value).to_owned()));
            sink.write(&body.as_bytes()[..split]).await;
            sink.write_now(&body.as_bytes()[split..]);
            check(sink, rx, &expected, body != expected).await;
        }
        let mut masker = Masker {
            typed: crate::session::Redactions::typed(typed.iter().map(|value| (*value).to_owned())),
            ..Masker::default()
        };
        let mut joined = Vec::new();
        for byte in body.as_bytes() {
            joined.extend(masker.feed(std::slice::from_ref(byte)));
        }
        joined.extend(masker.release());
        assert_eq!(joined, expected.as_bytes(), "bytewise {body:?}");
    }
    let old64 = "CANARY-b64-old-credential";
    let new64 = "CANARY-b64-new-password-longer";
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, new64);
    type Growth<'a> = (&'a [&'a str], &'a [&'a str], &'a str, &'a str, bool);
    let growth: [Growth<'_>; 6] = [
        (
            &["CANARY-old"],
            &["CANARY-new-long"],
            "CANARY-",
            "new-long",
            false,
        ),
        (
            &["CANARY-old-password"],
            &["CANARY-new-password-longer"],
            "CANARY-",
            "new-password-longer",
            false,
        ),
        (&["AAA", "]aa"], &["]aaa"], "AAA", "aaa", false),
        (
            &["CANARY-shared-old-password"],
            &["CANARY-shared-new-password-longer"],
            "CANARY-shared-",
            "new-password-longer",
            true,
        ),
        (
            &["CANARY-shared-finish-key-more"],
            &["CANARY-shared-finish-key"],
            "CANARY-shared-finish-key",
            "",
            true,
        ),
        (&[old64], &[new64], &encoded[..12], &encoded[12..], true),
    ];
    for (before, added, prefix, rest, shared) in growth {
        number += 1;
        let (tx, rx) = mpsc::channel(16);
        let mut sink = sink_in(&dir, number);
        sink.events = Some(tx);
        if shared {
            crate::session::register_secrets(before.iter().map(|value| (*value).to_owned()));
        } else {
            sink.hold(before.iter().map(|value| (*value).to_owned()));
        }
        sink.write(prefix.as_bytes()).await;
        if shared {
            crate::session::register_secrets(added.iter().map(|value| (*value).to_owned()));
        } else {
            sink.hold(added.iter().map(|value| (*value).to_owned()));
        }
        let redactions = crate::session::held().union([&sink.mask.typed]);
        let expected = redactions.mask(&format!("{prefix}{rest}"));
        sink.write_now(rest.as_bytes());
        check(sink, rx, &expected, true).await;
    }
    let sets: [&[&str]; 10] = [
        &["aaa"],
        &["aba"],
        &["aba", "bab"],
        &["ab", "aab"],
        &["aab", "baab"],
        &["aba", "ababa"],
        &["aaa", "baaa"],
        &["aba", "babab"],
        &["aaaa", "aaab", "aaba"],
        &["aab", "aba", "baa"],
    ];
    for values in sets {
        let redactions =
            crate::session::Redactions::typed(values.iter().map(|value| (*value).to_owned()));
        let before = crate::session::Redactions::typed([values[0].to_owned()]);
        let bound = 2 * values.iter().map(|value| value.len()).max().unwrap_or(0);
        for length in 0..=8 {
            for bits in 0..1usize << length {
                let body: String = (0..length)
                    .map(|bit| if bits & (1 << bit) == 0 { 'a' } else { 'b' })
                    .collect();
                let expected = oracle(body.as_bytes(), values, 0);
                assert_eq!(redactions.mask(&body).as_bytes(), expected);
                assert_eq!(redactions.scrub(&body).as_bytes(), expected);
                for first in 0..=length {
                    for second in first..=length {
                        let chunks = [
                            &body.as_bytes()[..first],
                            &body.as_bytes()[first..second],
                            &body.as_bytes()[second..],
                        ];
                        let mut pending = crate::session::MaskTail::default();
                        let mut joined = Vec::new();
                        let mut capped = false;
                        for chunk in chunks {
                            joined.extend(feed(
                                &redactions,
                                chunk,
                                &mut pending,
                                &mut capped,
                                bound,
                            ));
                        }
                        joined.extend(redactions.release(&mut pending).0);
                        assert_eq!(
                            collapsed(&joined),
                            expected,
                            "{values:?}, {body:?}, cuts {first}/{second}"
                        );
                        if !capped {
                            assert_eq!(joined, expected);
                        }
                        for change in [1, 2] {
                            let mut pending = crate::session::MaskTail::default();
                            let mut joined = Vec::new();
                            let mut expected = Vec::new();
                            let mut capped = false;
                            let mut fed = 0;
                            for (index, chunk) in chunks.into_iter().enumerate() {
                                if index == change {
                                    expected = joined.clone();
                                    let window = [&pending.bytes, &body.as_bytes()[fed..]].concat();
                                    expected.extend(oracle(&window, values, pending.covered));
                                }
                                let list = if index < change { &before } else { &redactions };
                                joined.extend(feed(list, chunk, &mut pending, &mut capped, bound));
                                fed += chunk.len();
                            }
                            joined.extend(redactions.release(&mut pending).0);
                            assert_eq!(
                                collapsed(&joined),
                                collapsed(&expected),
                                "growth {values:?}, {body:?}, cuts {first}/{second}, change {change}"
                            );
                            if !capped {
                                assert_eq!(joined, expected);
                            }
                        }
                    }
                }
            }
        }
        let mut pending = crate::session::MaskTail::default();
        assert_eq!(redactions.mask_chunk(b"!", &mut pending).0, b"!");
        assert!(pending.bytes.is_empty());
    }
    let redactions = crate::session::Redactions::typed(["AAA".to_owned(), "][r".to_owned()]);
    let mut pending = crate::session::MaskTail::default();
    let mut joined = Vec::new();
    let mut capped = false;
    for _ in 0..64 {
        joined.extend(feed(&redactions, b"AAA", &mut pending, &mut capped, 6));
    }
    joined.extend(redactions.release(&mut pending).0);
    assert!(capped);
    assert_eq!(
        collapsed(&joined),
        oracle(&"AAA".repeat(64).into_bytes(), &["AAA", "][r"], 0)
    );
}

#[tokio::test]
async fn output_events_are_coalesced_and_lossless() {
    let (tx, mut rx) = mpsc::channel(1024);
    let mut sink = OutputSink::new("c9", Some(tx), Err("none".into()));
    let body = numbered_lines(3000);
    for chunk in body.as_bytes().chunks(100) {
        sink.write(chunk).await;
    }
    sink.finish().await;
    drop(sink);
    let mut joined = String::new();
    let mut count = 0;
    while let Some(event) = rx.recv().await {
        let AgentEvent::ToolOutput { call_id, text } = event else {
            panic!("{event:?}")
        };
        assert_eq!(call_id, "c9");
        assert!(text.len() <= EVENT_BYTES + 100);
        joined.push_str(&text);
        count += 1;
    }
    assert_eq!(joined, body);
    assert!(count < body.len() / 100, "{count} events");
}

#[tokio::test]
async fn every_outcome_gives_the_same_result_text_through_the_mirror() {
    let partial = capture(b"partial\n").await;
    let empty = capture(b"").await;
    let notes = ["Working directory: ~/p".to_owned()];
    let plain = ResultStyle::default();
    let limit = ResultStyle {
        time_limit: true,
        process: Some(4),
        terminal: false,
    };
    let screen = ResultStyle {
        terminal: true,
        ..limit
    };
    let process_line = "Use the process tool with id 4 to read or search this output.";
    let screen_line =
        "Use the process tool with id 4 to read or search this output or show its final screen.";
    let table: Vec<(ToolOutcome, &Captured, ResultStyle, String)> = vec![
        (
            ToolOutcome::Completed {
                is_error: false,
                exit: Some(ExitStatus::Code { code: 3 }),
            },
            &partial,
            plain,
            "Exit code: 3\nOutput:\npartial\nWorking directory: ~/p".to_owned(),
        ),
        (
            ToolOutcome::Completed {
                is_error: true,
                exit: Some(ExitStatus::Signal { signal: 9 }),
            },
            &partial,
            limit,
            format!("Exit signal: 9\nOutput:\npartial\n{process_line}\nWorking directory: ~/p"),
        ),
        (
            ToolOutcome::Completed {
                is_error: false,
                exit: None,
            },
            &empty,
            plain,
            "Output: (none)\nWorking directory: ~/p".to_owned(),
        ),
        (
            ToolOutcome::TimedOut { after_ms: 120_000 },
            &partial,
            plain,
            "Output:\npartial\nTimed out after 120 s; the call was stopped.\nWorking directory: ~/p"
                .to_owned(),
        ),
        (
            ToolOutcome::TimedOut { after_ms: 1500 },
            &partial,
            screen,
            format!(
                "Output:\npartial\n{}\n{screen_line}\nWorking directory: ~/p",
                time_limit_text(1500)
            ),
        ),
        (
            ToolOutcome::Cancelled,
            &partial,
            plain,
            "Output:\npartial\n[stopped by the user]\nWorking directory: ~/p".to_owned(),
        ),
        (
            ToolOutcome::NotRun {
                reason: NotRunReason::TurnStopped,
            },
            &partial,
            limit,
            format!("Output:\npartial\n{process_line}\nWorking directory: ~/p"),
        ),
        (
            ToolOutcome::InvalidInput {
                message: "bad".into(),
            },
            &empty,
            plain,
            "Output: (none)\nWorking directory: ~/p".to_owned(),
        ),
        (
            ToolOutcome::Failed {
                message: "it broke".into(),
                before_start: false,
            },
            &partial,
            plain,
            "Failed: it broke\nOutput:\npartial\nWorking directory: ~/p".to_owned(),
        ),
        (
            ToolOutcome::Failed {
                message: "spawn failed".into(),
                before_start: true,
            },
            &empty,
            limit,
            format!("Failed: spawn failed\n{process_line}\nWorking directory: ~/p"),
        ),
        (
            ToolOutcome::Backgrounded {
                reason: wisp_protocol::BackgroundReason::MovedByUser,
            },
            &partial,
            plain,
            "Output:\npartial\nWorking directory: ~/p".to_owned(),
        ),
    ];
    for (outcome, captured, style, want) in table {
        assert_eq!(
            result_text_with(&outcome, captured, None, &notes, style),
            want,
            "{outcome:?}"
        );
        if let Some(mirrored) = outcome_of(&outcome) {
            assert_eq!(
                wisp_tool_text::output::result_text_with(&mirrored, captured, None, &notes, style),
                want,
                "{outcome:?}"
            );
        } else {
            assert!(matches!(outcome, ToolOutcome::NotRun { .. }));
        }
    }
}

#[tokio::test]
async fn results_for_each_outcome_are_fixed_text() {
    let captured = capture(b"partial\n").await;
    let text =
        |outcome: ToolOutcome, notes: &[String]| result_text(&outcome, &captured, None, notes);
    assert_eq!(
        text(ToolOutcome::Cancelled, &[]),
        "Output:\npartial\n[stopped by the user]"
    );
    assert_eq!(
        text(ToolOutcome::TimedOut { after_ms: 120_000 }, &[]),
        "Output:\npartial\nTimed out after 120 s; the call was stopped."
    );
    assert_eq!(
        text(
            ToolOutcome::Completed {
                is_error: false,
                exit: Some(ExitStatus::Signal { signal: 9 })
            },
            &["Working directory: ~/p".to_owned()]
        ),
        "Exit signal: 9\nOutput:\npartial\nWorking directory: ~/p"
    );
    let empty = capture(b"").await;
    assert_eq!(
        result_text(
            &ToolOutcome::Failed {
                message: "spawn failed".into(),
                before_start: false,
            },
            &empty,
            None,
            &[]
        ),
        "Failed: spawn failed"
    );
}

#[test]
fn a_setgid_temp_parent_still_gets_output_files() {
    let root = scratch("setgid");
    fs::set_permissions(&root.root, fs::Permissions::from_mode(0o2755)).unwrap();
    let dir = OutputDir::new(&root.root, "sid");
    let first = dir.create(1, "t");
    assert!(first.is_ok(), "{:?}", first.err());
    let mode = fs::metadata(dir.path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o2000, 0o2000, "the new directory inherits setgid");
    let second = dir.create(2, "t");
    assert!(second.is_ok(), "{:?}", second.err());
}

#[tokio::test]
async fn escapes_split_across_writes_are_still_removed() {
    let mut sink = OutputSink::detached("c");
    for part in [
        &b"a\x1b[3"[..],
        b"1mred\x1b",
        b"[0m b\x1b(",
        b"Bc\x1b]0;ti",
        b"tle\x07d\n",
    ] {
        sink.write(part).await;
    }
    assert_eq!(sink.finish().await.head, "ared bcd\n");
}

#[tokio::test]
async fn a_designator_escape_ends_on_its_final_byte() {
    assert_eq!(cleaned(b"\x1b(0abc\n").await, "abc\n");
    assert_eq!(cleaned(b"\x1b(Babc\n").await, "abc\n");
    assert_eq!(cleaned(b"\x1b%Gabc\n").await, "abc\n");
    assert_eq!(cleaned(b"\x1b#8abc\n").await, "abc\n");
}

#[tokio::test(start_paused = true)]
async fn buffered_output_is_sent_when_the_tool_goes_quiet() {
    let (tx, mut rx) = mpsc::channel(16);
    let mut sink = OutputSink::new("c1", Some(tx), Err("none".into()));
    let flush = sink.flusher();
    sink.write(b"first\n").await;
    sink.write(b"second\n").await;
    let mut texts = Vec::new();
    while let Ok(AgentEvent::ToolOutput { text, .. }) = rx.try_recv() {
        texts.push(text);
    }
    assert_eq!(texts, ["first\n"]);
    flush.flush().await;
    assert!(rx.try_recv().is_err(), "nothing is sent before 50 ms");
    tokio::time::sleep(EVENT_INTERVAL).await;
    flush.flush().await;
    let Ok(AgentEvent::ToolOutput { text, .. }) = rx.try_recv() else {
        panic!("the quiet tail was never sent")
    };
    assert_eq!(text, "second\n");
    flush.flush().await;
    assert!(rx.try_recv().is_err());
    drop(sink);
}

fn kernel_clean(text: &str) -> String {
    let mut cleaner = Cleaner::default();
    cleaner.feed(text);
    cleaner.finish();
    cleaner.out.head
}

fn without_controls(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

#[test]
fn the_cleaner_matches_the_shared_clean_corpus() {
    let corpus = std::fs::read_to_string(protocol_fixtures().join("clean-corpus.jsonl")).unwrap();
    let mut differ = vec![];
    let mut seen = 0;
    for line in corpus.lines() {
        let (input, cleaned): (String, String) = serde_json::from_str(line).unwrap();
        seen += 1;
        if input.contains('\r') {
            continue;
        }
        let want = without_controls(&cleaned);
        let have = kernel_clean(&input);
        if have != want {
            differ.push((input, have, want));
        }
    }
    assert!(seen > 2_500, "{seen}");
    assert!(
        differ.is_empty(),
        "{} differ, first {:?}",
        differ.len(),
        differ.first()
    );
}

#[test]
fn can_and_sub_end_a_string_sequence_in_the_kernel() {
    assert_eq!(kernel_clean("a\u{1b}]0;t\u{18}b\n"), "ab\n");
    assert_eq!(kernel_clean("a\u{1b}Pq\u{1a}b\n"), "ab\n");
}

fn texts(rx: &mut mpsc::Receiver<AgentEvent>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::ToolOutput { text, .. } = event {
            out.push(text);
        }
    }
    out
}

#[tokio::test]
async fn finishing_twice_returns_the_same_capture_and_sends_nothing_more() {
    let dir = scratch("finish-twice");
    let out = OutputDir::new(&dir.root, "s");
    let target: FileTarget = out
        .create(1, "t")
        .map(|(p, f)| (p, Box::new(f) as Box<dyn Write + Send>))
        .map_err(|e| e.to_string());
    let (tx, mut rx) = mpsc::channel(64);
    let mut sink = OutputSink::new("c1", Some(tx), target);
    sink.write(b"one\ntwo\r\nthree \xe2\x82").await;
    let first = sink.finish().await;
    let sent = texts(&mut rx).concat();
    assert_eq!(sent, "one\ntwo\r\nthree \u{fffd}");
    let second = sink.finish().await;
    assert_eq!(first, second);
    assert!(texts(&mut rx).is_empty());
    assert_eq!(first.head, "one\ntwo\nthree \u{fffd}");
}

#[tokio::test]
async fn taking_a_sink_leaves_a_detached_placeholder_with_the_same_call() {
    let (tx, mut rx) = mpsc::channel(64);
    let mut sink = OutputSink::new("c7", Some(tx), Err("no file".into()));
    let mut moved = sink.take();
    assert_eq!(sink.call_id(), "c7");
    assert_eq!(moved.call_id(), "c7");
    sink.write(b"lost\n").await;
    assert_eq!(sink.finish().await.bytes, 5);
    assert!(texts(&mut rx).is_empty());
    moved.write(b"kept\n").await;
    assert_eq!(moved.finish().await.head, "kept\n");
    assert_eq!(texts(&mut rx).concat(), "kept\n");
}

#[tokio::test]
async fn stopping_events_sends_the_pending_text_last_and_then_nothing() {
    let (tx, mut rx) = mpsc::channel(64);
    let mut sink = OutputSink::new("c1", Some(tx), Err("no file".into()));
    sink.write(b"first\n").await;
    sink.write(b"second\n").await;
    sink.stop_events().await;
    assert_eq!(texts(&mut rx).concat(), "first\nsecond\n");
    sink.write(b"third\n").await;
    let captured = sink.finish().await;
    assert!(texts(&mut rx).is_empty());
    assert_eq!(captured.head, "first\nsecond\nthird\n");
}

#[tokio::test]
async fn a_snapshot_summarises_the_output_so_far_and_changes_nothing() {
    let mut sink = OutputSink::detached("c");
    sink.write(numbered_lines(3000).as_bytes()).await;
    sink.write(b"10%\r50%").await;
    let snapshot = sink.snapshot();
    assert_eq!(snapshot.lines, 3001);
    assert!(
        snapshot.tail.ends_with("kestrel-3000\n50%"),
        "{}",
        snapshot.tail
    );
    assert_eq!(sink.snapshot(), snapshot);
    sink.write(b"\rdone\n").await;
    let finished = sink.finish().await;
    assert_eq!(finished.lines, 3001);
    assert!(finished.tail.ends_with("kestrel-3000\ndone\n"));
}

#[tokio::test]
async fn a_sink_keeps_line_numbers_only_when_asked() {
    let mut plain = OutputSink::detached("c");
    plain.write(b"a\nb\n").await;
    assert!(!plain.keeps_lines());
    assert_eq!(plain.complete_lines(), 0);
    assert_eq!(plain.line_state().complete, 0);
    let mut kept = OutputSink::detached("c");
    kept.keep_lines();
    kept.write(b"a\nb\n").await;
    assert!(kept.keeps_lines());
    assert_eq!(kept.complete_lines(), 2);
    assert_eq!(kept.line_state().ring.line(2), Some("b"));
}

#[test]
fn the_ring_keeps_the_newest_lines_within_its_bytes() {
    use super::{RING_BYTES, Ring};
    let mut ring = Ring::default();
    ring.push("one\ntwo\nthr");
    assert_eq!((ring.complete(), ring.first()), (2, 1));
    assert_eq!(ring.line(1), Some("one"));
    assert_eq!(ring.line(2), Some("two"));
    assert_eq!(ring.line(3), None);
    assert_eq!(ring.partial(), "thr");
    assert_eq!(ring.complete_bytes(), 8);
    let line = "x".repeat(999);
    for _ in 0..200 {
        ring.push(&line);
        ring.push("\n");
    }
    assert_eq!(ring.complete(), 202);
    assert!(ring.first() > 3);
    assert!(!ring.holds(ring.first() - 1));
    assert!(ring.holds(202));
    assert_eq!(ring.line(202).map(str::len), Some(999));
    assert_eq!(ring.line(3).map(str::len), None);
    let kept = (ring.first()..=202).filter_map(|n| ring.line(n)).count() as u64;
    assert_eq!(kept, 202 - ring.first() + 1);
    assert!(kept * 1000 <= RING_BYTES as u64);
    ring.push(&"y".repeat(RING_BYTES + 10));
    ring.push("\nlast");
    assert_eq!(ring.complete(), 203);
    assert_eq!(ring.first(), 204);
    assert_eq!(ring.line(203), None);
    ring.finish();
    assert!(ring.unterminated());
    assert_eq!(ring.complete(), 204);
    assert_eq!(ring.line(204), Some("last"));
}

#[test]
fn the_ring_keeps_a_newest_line_that_fits_and_matches_a_plain_model() {
    use super::{RING_BYTES, Ring};
    let mut ring = Ring::default();
    let big = "b".repeat(RING_BYTES - 3);
    ring.push("small\n");
    ring.push(&big);
    ring.push("\n");
    assert_eq!(ring.first(), 2);
    assert_eq!(ring.line(2), Some(big.as_str()));
    let mut model: Vec<String> = Vec::new();
    let mut ring = Ring::default();
    let mut drops = 0;
    for n in 0..5000u64 {
        let before = ring.first();
        let line = "x".repeat(usize::try_from(n * 7 % 300).unwrap());
        ring.push(&line);
        ring.push("\n");
        model.push(line);
        let first = ring.first();
        let kept: usize = (first..=ring.complete())
            .map(|k| ring.line(k).unwrap().len() + 1)
            .sum();
        assert!(kept <= RING_BYTES);
        assert!(kept + 301 > RING_BYTES / 4 * 3 || first == 1);
        if first > before {
            drops += 1;
            assert!(kept <= RING_BYTES / 4 * 3, "{kept} kept after a drop");
        }
        for k in [first, ring.complete()] {
            assert_eq!(
                ring.line(k),
                Some(model[usize::try_from(k - 1).unwrap()].as_str())
            );
        }
    }
    assert!(drops > 1, "{drops}");
}
