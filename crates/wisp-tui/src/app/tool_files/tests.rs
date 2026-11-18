use super::*;
use crate::events::{Finished, Requested, tool_finished, tool_requested};
use crate::scratch::ScratchDir;

fn temp(name: &str) -> ScratchDir {
    ScratchDir::new(crate::SCRATCH, name)
}

#[test]
fn a_small_file_is_read_whole() {
    let dir = temp("small");
    let path = dir.root.join("out");
    std::fs::write(&path, "one\ntwo\n").unwrap();
    assert_eq!(
        read_tail(&path, 1024),
        ToolFile::Text {
            text: "one\ntwo\n".to_owned(),
            skipped: 0
        }
    );
}

#[test]
fn a_long_file_is_read_from_its_tail_on_a_character_boundary() {
    let dir = temp("tail");
    let path = dir.root.join("out");
    let mut text = "a".repeat(9);
    text.push('\u{e9}');
    text.push_str("tail");
    std::fs::write(&path, &text).unwrap();
    let len = text.len() as u64;
    match read_tail(&path, 5) {
        ToolFile::Text { text: got, skipped } => {
            assert_eq!(got, "tail");
            assert_eq!(skipped, len - 4);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_deleted_file_or_a_directory_says_why() {
    let dir = temp("gone");
    assert_eq!(
        read_tail(&dir.root.join("missing"), 10),
        ToolFile::Gone(DELETED.to_owned())
    );
    assert_eq!(
        read_tail(&dir.root, 10),
        ToolFile::Gone(NOT_A_FILE.to_owned())
    );
}

#[test]
fn a_file_over_the_view_cap_opens_at_its_tail() {
    let dir = temp("big");
    let path = dir.root.join("out");
    let line = format!("{}\n", "x".repeat(99));
    let mut body = line
        .repeat(usize::try_from(FILE_TAIL).unwrap() / 100 + 10_000)
        .into_bytes();
    body.extend_from_slice(b"the very end\n");
    std::fs::write(&path, &body).unwrap();
    match read_tail(&path, FILE_TAIL) {
        ToolFile::Text { text, skipped } => {
            assert!(text.ends_with("the very end\n"));
            assert!(text.starts_with(&line));
            assert!(text.len() as u64 <= FILE_TAIL);
            assert!(text.len() as u64 > FILE_TAIL - 100);
            assert_eq!(skipped + text.len() as u64, body.len() as u64);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn loading_stops_at_the_budget_newest_first() {
    use wisp_protocol::{AgentEvent, ExitStatus, ToolOutcome, ToolOutputView};
    use wisp_render::transcript::{Config, Detail};
    let dir = temp("budget");
    let mut t = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: wisp_protocol::CacheTtl::OneHour,
        logging: false,
    });
    t.apply(&AgentEvent::turn_started(1), std::time::Duration::ZERO);
    for (i, word) in ["older", "newer"].into_iter().enumerate() {
        let path = dir.root.join(word);
        std::fs::write(&path, format!("{word} body\n").repeat(10)).unwrap();
        let id = format!("call-{i}");
        t.apply(
            &tool_requested(Requested {
                block: i,
                call_id: id.clone(),
                input: serde_json::json!({"command": word}),
                target: word.to_owned(),
                ..Requested::default()
            }),
            std::time::Duration::ZERO,
        );
        t.apply(
            &tool_finished(Finished {
                call_id: id,
                outcome: ToolOutcome::Completed {
                    is_error: false,
                    exit: Some(ExitStatus::Code { code: 0 }),
                },
                output: ToolOutputView {
                    tail: format!("{word} tail\n"),
                    bytes: 110,
                    lines: 10,
                    file: Some(path.to_string_lossy().into_owned()),
                    file_problem: None,
                    inlined: false,
                },
                duration_ms: 1,
                ..Finished::default()
            }),
            std::time::Duration::ZERO,
        );
    }
    let mut loader = Loader::new(150, std::sync::Arc::new(tokio::sync::Notify::new()));
    loader.request(&mut t);
    assert!(loader.receive(&mut t));
    loader.request(&mut t);
    assert!(loader.receive(&mut t));
    assert!(!loader.is_waiting());
    assert_eq!(loader.reads(), 2);
    loader.request(&mut t);
    assert!(!loader.receive(&mut t));
    assert_eq!(loader.reads(), 2);
    let r = wisp_render::markdown::Renderer::new(wisp_render::markdown::Layout::new(
        80,
        wisp_render::theme::glyphs::GlyphSet::Unicode,
    ));
    let rows: String = t
        .rows(&r, Detail::Full)
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
        .collect();
    assert_eq!(rows.matches("newer body").count(), 10, "{rows}");
    assert_eq!(rows.matches("older body").count(), 3, "{rows}");
    assert!(!rows.contains(" r body"), "{rows}");
}

#[test]
fn a_tail_starts_at_a_line_when_one_begins_nearby() {
    let dir = temp("snap");
    let path = dir.root.join("out");
    std::fs::write(&path, "first line\nsecond\nthird\n").unwrap();
    assert_eq!(
        read_tail(&path, 16),
        ToolFile::Text {
            text: "second\nthird\n".to_owned(),
            skipped: 11
        }
    );
}

#[test]
fn a_symlink_at_the_output_path_is_never_followed() {
    let dir = temp("link");
    let target = dir.root.join("secret");
    std::fs::write(&target, "secret text\n").unwrap();
    let path = dir.root.join("out");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(read_tail(&path, 1024), ToolFile::Gone(A_LINK.to_owned()));
}

#[test]
fn a_fifo_at_the_output_path_never_blocks() {
    let dir = temp("fifo");
    let path = dir.root.join("out");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &path,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let probe = path.clone();
    std::thread::spawn(move || {
        let _ = tx.send(read_tail(&probe, 1024));
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(5));
    if got.is_err() {
        let _ = std::fs::OpenOptions::new().write(true).open(&path);
    }
    assert_eq!(got, Ok(ToolFile::Gone(NOT_A_FILE.to_owned())));
}

#[test]
fn a_text_that_grows_when_decoded_is_still_held_to_the_cap() {
    let dir = temp("lossy");
    let path = dir.root.join("out");
    let mut body = vec![0xFFu8; 3000];
    body.extend_from_slice(b"end");
    std::fs::write(&path, &body).unwrap();
    match read_tail(&path, 1000) {
        ToolFile::Text { text, skipped } => {
            assert!(text.len() <= 1000, "{}", text.len());
            assert!(text.len() > 900, "{}", text.len());
            assert!(text.ends_with("end"));
            assert!(skipped > 0);
        }
        other => panic!("{other:?}"),
    }
}

fn one_call(dir: &std::path::Path) -> Transcript {
    use wisp_protocol::{AgentEvent, ExitStatus, ToolOutcome, ToolOutputView};
    use wisp_render::transcript::Config;
    let path = dir.join("out");
    std::fs::write(&path, "fresh body\n").unwrap();
    let mut t = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: wisp_protocol::CacheTtl::OneHour,
        logging: false,
    });
    t.apply(&AgentEvent::turn_started(1), std::time::Duration::ZERO);
    t.apply(
        &tool_requested(Requested {
            call_id: "c".to_owned(),
            input: serde_json::json!({"command": "gen"}),
            target: "gen".to_owned(),
            ..Requested::default()
        }),
        std::time::Duration::ZERO,
    );
    t.apply(
        &tool_finished(Finished {
            call_id: "c".to_owned(),
            outcome: ToolOutcome::Completed {
                is_error: false,
                exit: Some(ExitStatus::Code { code: 0 }),
            },
            output: ToolOutputView {
                tail: "tail\n".to_owned(),
                bytes: 11,
                lines: 1,
                file: Some(path.to_string_lossy().into_owned()),
                file_problem: None,
                inlined: false,
            },
            duration_ms: 1,
            ..Finished::default()
        }),
        std::time::Duration::ZERO,
    );
    t
}

fn full_text(t: &mut Transcript) -> String {
    let r = wisp_render::markdown::Renderer::new(wisp_render::markdown::Layout::new(
        80,
        wisp_render::theme::glyphs::GlyphSet::Unicode,
    ));
    t.rows(&r, wisp_render::transcript::Detail::Full)
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
        .collect()
}

#[test]
fn closing_drains_replies_and_a_late_reply_from_before_is_ignored() {
    let dir = temp("reopen");
    let mut t = one_call(&dir.root);
    let mut loader = Loader::new(1024, std::sync::Arc::new(tokio::sync::Notify::new()));
    loader.request(&mut t);
    loader.reset(&mut t);
    assert!(loader.replies.try_recv().is_err());
    loader.request(&mut t);
    let stale = Reply {
        generation: loader.generation - 1,
        index: 0,
        epoch: 0,
        cap: 1024,
        file: ToolFile::Text {
            text: "stale body\n".to_owned(),
            skipped: 0,
        },
    };
    loader.sender.send(stale).unwrap();
    assert!(loader.receive(&mut t));
    let shown = full_text(&mut t);
    assert!(shown.contains("fresh body"), "{shown}");
    assert!(!shown.contains("stale body"), "{shown}");
    loader.reset(&mut t);
    loader
        .sender
        .send(Reply {
            generation: loader.generation - 1,
            index: 0,
            epoch: 0,
            cap: 1024,
            file: ToolFile::Loading,
        })
        .unwrap();
    loader.discard();
    assert!(loader.replies.try_recv().is_err());
}

#[test]
fn the_skipped_count_is_in_raw_bytes_even_after_replacement() {
    let dir = temp("raw-skip");
    let path = dir.root.join("out");
    let mut body = vec![0xFFu8; 3000];
    body.extend_from_slice(b"end");
    std::fs::write(&path, &body).unwrap();
    match read_tail(&path, 1000) {
        ToolFile::Text { text, skipped } => {
            let replaced = text.matches(char::REPLACEMENT_CHARACTER).count() as u64;
            assert!(text.ends_with("end"));
            assert_eq!(skipped + replaced + 3, body.len() as u64, "{skipped}");
        }
        other => panic!("{other:?}"),
    }
    let mut mixed = "ok line\n".repeat(400).into_bytes();
    mixed.extend_from_slice(&[0xC3, 0x28, 0xE2, 0x82]);
    mixed.extend_from_slice("caf\u{e9}\n".repeat(300).as_bytes());
    std::fs::write(&path, &mixed).unwrap();
    match read_tail(&path, 900) {
        ToolFile::Text { text, skipped } => {
            assert!(text.is_char_boundary(0));
            let raw = mixed.len() as u64 - skipped;
            assert_eq!(
                &mixed[usize::try_from(skipped).unwrap()..],
                text.as_bytes(),
                "{raw}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_read_that_finishes_after_a_close_sends_nothing() {
    let dir = temp("late");
    let mut t = one_call(&dir.root);
    let mut loader = Loader::new(1024, std::sync::Arc::new(tokio::sync::Notify::new()));
    let path = dir.root.join("out").to_string_lossy().into_owned();
    let late = loader.job(0, (path.clone(), 0), 1024);
    loader.reset(&mut t);
    late();
    assert!(loader.replies.try_recv().is_err());
    assert_eq!(loader.reads(), 1);
    let fresh = loader.job(0, (path, 0), 1024);
    fresh();
    assert!(loader.replies.try_recv().is_ok());
}

#[test]
fn a_reply_that_slips_past_the_drain_is_dropped_while_closed() {
    let dir = temp("closed");
    let mut t = one_call(&dir.root);
    let mut loader = Loader::new(1024, std::sync::Arc::new(tokio::sync::Notify::new()));
    loader.request(&mut t);
    loader.reset(&mut t);
    let slipped = Reply {
        generation: loader.generation,
        index: 0,
        epoch: 0,
        cap: 1024,
        file: ToolFile::Text {
            text: "slipped body\n".to_owned(),
            skipped: 0,
        },
    };
    loader.sender.send(slipped).unwrap();
    assert!(!loader.receive(&mut t));
    assert!(loader.replies.try_recv().is_err());
    assert!(!full_text(&mut t).contains("slipped body"));
}

#[test]
fn a_reader_loads_only_its_own_cells_file() {
    use wisp_protocol::{AgentEvent, ExitStatus, ToolOutcome, ToolOutputView};
    use wisp_render::transcript::Config;
    let dir = temp("one-cell");
    let mut t = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: wisp_protocol::CacheTtl::OneHour,
        logging: false,
    });
    t.apply(&AgentEvent::turn_started(1), std::time::Duration::ZERO);
    for (i, word) in ["first", "second"].into_iter().enumerate() {
        let path = dir.root.join(word);
        std::fs::write(&path, format!("{word} body\n")).unwrap();
        let id = format!("call-{i}");
        t.apply(
            &tool_requested(Requested {
                block: i,
                call_id: id.clone(),
                input: serde_json::json!({"command": word}),
                target: word.to_owned(),
                ..Requested::default()
            }),
            std::time::Duration::ZERO,
        );
        t.apply(
            &tool_finished(Finished {
                call_id: id,
                outcome: ToolOutcome::Completed {
                    is_error: false,
                    exit: Some(ExitStatus::Code { code: 0 }),
                },
                output: ToolOutputView {
                    tail: format!("{word} tail\n"),
                    bytes: 11,
                    lines: 1,
                    file: Some(path.to_string_lossy().into_owned()),
                    file_problem: None,
                    inlined: false,
                },
                duration_ms: 1,
                ..Finished::default()
            }),
            std::time::Duration::ZERO,
        );
    }
    let wanted: Vec<usize> = t.tool_files().into_iter().map(|(i, _)| i).collect();
    assert_eq!(wanted.len(), 2);
    let first = *wanted.iter().min().unwrap();
    let mut loader = Loader::new(
        PAGER_BUDGET,
        std::sync::Arc::new(tokio::sync::Notify::new()),
    );
    loader.request_cell(&mut t, first);
    assert!(loader.receive(&mut t));
    assert_eq!(loader.reads(), 1);
    let left: Vec<usize> = t.tool_files().into_iter().map(|(i, _)| i).collect();
    assert_eq!(left, vec![*wanted.iter().max().unwrap()]);
    loader.request_cell(&mut t, first);
    assert_eq!(loader.reads(), 1);
}

#[test]
fn a_cut_character_at_the_end_of_a_file_still_being_written_is_left_out() {
    let dir = temp("cut-char");
    let path = dir.root.join("out");
    let mut bytes = b"progress caf".to_vec();
    bytes.extend_from_slice(&[0xC3]);
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        read_tail(&path, 1024),
        ToolFile::Text {
            text: "progress caf".to_owned(),
            skipped: 0
        }
    );
    std::fs::write(&path, "progress caf\u{e9}").unwrap();
    assert_eq!(
        read_tail(&path, 1024),
        ToolFile::Text {
            text: "progress caf\u{e9}".to_owned(),
            skipped: 0
        }
    );
    let mut bad = b"bad ".to_vec();
    bad.push(0xFF);
    std::fs::write(&path, &bad).unwrap();
    match read_tail(&path, 1024) {
        ToolFile::Text { text, .. } => assert!(text.ends_with('\u{fffd}'), "{text:?}"),
        other => panic!("{other:?}"),
    }
}

fn running_call(path: &std::path::Path) -> Transcript {
    use wisp_protocol::AgentEvent;
    use wisp_render::transcript::Config;
    let mut t = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: wisp_protocol::CacheTtl::OneHour,
        logging: false,
    });
    let zero = std::time::Duration::ZERO;
    t.apply(&AgentEvent::turn_started(1), zero);
    t.apply(
        &tool_requested(Requested {
            call_id: "c".to_owned(),
            input: serde_json::json!({"command": "watch"}),
            target: "watch".to_owned(),
            ..Requested::default()
        }),
        zero,
    );
    t.apply(
        &AgentEvent::ToolCallStarted {
            call_id: "c".to_owned(),
            cwd: "~/w".to_owned(),
            movable: false,
            file: Some(path.to_string_lossy().into_owned()),
        },
        zero,
    );
    t
}

#[test]
fn a_reload_keeps_the_old_text_until_its_read_lands_and_a_read_from_before_the_end_is_dropped() {
    use wisp_protocol::{ExitStatus, ToolOutcome, ToolOutputView};
    use wisp_render::transcript::copy;
    let dir = temp("reload");
    let path = dir.root.join("out");
    std::fs::write(&path, "first\n").unwrap();
    let mut t = running_call(&path);
    let mut loader = Loader::new(
        PAGER_BUDGET,
        std::sync::Arc::new(tokio::sync::Notify::new()),
    );
    assert!(loader.reload_cell(&mut t, 0));
    assert!(full_text(&mut t).contains(copy::LOADING_FILE));
    assert!(loader.receive(&mut t));
    assert!(full_text(&mut t).contains("first"));
    std::fs::write(&path, "first\nsecond\n").unwrap();
    assert!(loader.reload_cell(&mut t, 0));
    assert!(!loader.reload_cell(&mut t, 0));
    let shown = full_text(&mut t);
    assert!(shown.contains("first"), "{shown}");
    assert!(!shown.contains(copy::LOADING_FILE), "{shown}");
    assert!(!shown.contains("second"), "{shown}");
    assert!(loader.receive(&mut t));
    assert!(full_text(&mut t).contains("second"));
    std::fs::write(&path, "first\nsecond\nthird\n").unwrap();
    assert!(loader.reload_cell(&mut t, 0));
    t.apply(
        &tool_finished(Finished {
            call_id: "c".to_owned(),
            outcome: ToolOutcome::Completed {
                is_error: false,
                exit: Some(ExitStatus::Code { code: 0 }),
            },
            output: ToolOutputView {
                tail: "last\n".to_owned(),
                bytes: 4,
                lines: 1,
                file: Some(path.to_string_lossy().into_owned()),
                file_problem: None,
                inlined: false,
            },
            duration_ms: 1,
            ..Finished::default()
        }),
        std::time::Duration::ZERO,
    );
    std::fs::write(&path, "first\nsecond\nthird\nfourth\n").unwrap();
    assert!(loader.receive(&mut t));
    let kept = full_text(&mut t);
    assert!(kept.contains("second"), "{kept}");
    assert!(!kept.contains("third"), "{kept}");
    assert_eq!(t.tool_files().len(), 1);
    loader.request_cell(&mut t, 0);
    assert!(!full_text(&mut t).contains(copy::LOADING_FILE));
    assert!(loader.receive(&mut t));
    assert!(full_text(&mut t).contains("fourth"));
    std::fs::write(&path, "first\nsecond\nthird\nfourth\n").unwrap();
    assert!(loader.reload_cell(&mut t, 0));
    assert!(!loader.receive(&mut t));
    assert_eq!(loader.reads(), 5);
}
