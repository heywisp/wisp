use wisp_protocol::{AttachmentKind, AttachmentRef, FileSlice, Mime, WithdrawnAttachment};

use super::*;
use crate::events::{Finished, Requested, tool_finished, tool_requested};
use crate::styled::Look;
use crate::theme::Token;

const SAVED: &str = "/data/wisp/attachments/3f2a9c.png";
const CANT_CARRY: &str = "song.mp3 is audio/mpeg, which this server can't carry (it takes \
application/pdf, image/gif, image/jpeg, image/png, image/webp). Convert it to one of \
those or to text, or inspect it another way (file, xxd, strings).";
const REASON: &str = "Could not process image";

fn mime(text: &str) -> Mime {
    text.parse().unwrap()
}

fn read(step: u32, block: usize, id: &str, path: &str) -> AgentEvent {
    tool_requested(Requested {
        step,
        block,
        call_id: id.to_owned(),
        tool: "look".to_owned(),
        input: json!({"path": path}),
        action: ToolAction::Read,
        target: path.to_owned(),
        output: OutputKind::None,
        ..Requested::default()
    })
}

fn attached(media_type: &str, bytes: u64, saved: Option<&str>) -> AttachmentRef {
    AttachmentRef {
        media_type: mime(media_type),
        sha256: "3f2a9c".to_owned(),
        bytes,
        saved: saved.map(str::to_owned),
        slice: None,
    }
}

fn viewed(id: &str, facts: Vec<Fact>, result: &str, attachments: Vec<AttachmentRef>) -> AgentEvent {
    tool_finished(Finished {
        call_id: id.to_owned(),
        outcome: ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        output: ToolOutputView {
            inlined: true,
            ..ToolOutputView::default()
        },
        facts,
        duration_ms: 40,
        result: Some(result.to_owned()),
        attachments,
        ..Finished::default()
    })
}

fn image(id: &str, path: &str, saved: Option<&str>) -> AgentEvent {
    viewed(
        id,
        vec![
            Fact::Media {
                media_type: mime("image/png"),
            },
            Fact::Bytes { n: 1_600_000 },
            Fact::Tokens { n: 2_900 },
        ],
        &format!(
            "~/p/{path}: image/png, 1.6 MB, attached as is (about 2,960 tokens, the server's \
estimate)."
        ),
        vec![attached("image/png", 1_600_000, saved)],
    )
}

fn pdf(id: &str, path: &str) -> AgentEvent {
    viewed(
        id,
        vec![
            Fact::Media {
                media_type: mime("application/pdf"),
            },
            Fact::Bytes { n: 912_000 },
        ],
        &format!("~/p/{path}: application/pdf, 912 KB, attached as is."),
        vec![attached("application/pdf", 912_000, None)],
    )
}

fn text_view(id: &str) -> AgentEvent {
    tool_finished(Finished {
        call_id: id.to_owned(),
        outcome: ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        output: ToolOutputView {
            tail: "line 1999\nline 2000\n".to_owned(),
            bytes: 51_000,
            lines: 2_000,
            file: Some("/tmp/wisp-s/0003-look.out".to_owned()),
            file_problem: None,
            inlined: false,
        },
        facts: vec![
            Fact::Part {
                sent: 2_000,
                total: 9_412,
                more_than: false,
            },
            Fact::Tokens { n: 17_000 },
        ],
        duration_ms: 120,
        result: Some(
            "~/p/build.log: text, 9,412 lines (1.2 MB); lines 1\u{2013}2,000 below (about \
16,800 tokens).\nline 1\n"
                .to_owned(),
        ),
        ..Finished::default()
    })
}

fn refused(id: &str) -> AgentEvent {
    tool_finished(Finished {
        call_id: id.to_owned(),
        outcome: ToolOutcome::Completed {
            is_error: true,
            exit: None,
        },
        output: ToolOutputView {
            inlined: true,
            ..ToolOutputView::default()
        },
        duration_ms: 3,
        result: Some(CANT_CARRY.to_owned()),
        ..Finished::default()
    })
}

fn withdrawal(step: u32, items: &[(&str, &str)]) -> AgentEvent {
    AgentEvent::AttachmentsWithdrawn {
        step,
        items: items
            .iter()
            .enumerate()
            .map(|(ordinal, (id, label))| WithdrawnAttachment {
                call_id: Some((*id).to_owned()),
                ordinal,
                kind: AttachmentKind::File,
                label: (*label).to_owned(),
                reason: REASON.to_owned(),
                note: format!("[{label} couldn't be processed]"),
            })
            .collect(),
    }
}

fn files_turn() -> Transcript {
    files_turn_in(transcript())
}

fn files_turn_in(mut t: Transcript) -> Transcript {
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            read(1, 0, "a", "screenshots/home.png"),
            read(1, 1, "b", "docs/spec.pdf"),
            read(1, 2, "c", "build.log"),
            read(1, 3, "d", "song.mp3"),
            step_ended(1, 0.01),
            AgentEvent::call_started("a", "~/p"),
            image("a", "screenshots/home.png", Some(SAVED)),
            AgentEvent::call_started("b", "~/p"),
            pdf("b", "docs/spec.pdf"),
            AgentEvent::call_started("c", "~/p"),
            text_view("c"),
            AgentEvent::call_started("d", "~/p"),
            refused("d"),
            AgentEvent::StepStarted { step: 2 },
            read(2, 0, "e", "shots/a.png"),
            read(2, 1, "f", "shots/b.png"),
            step_ended(2, 0.01),
            AgentEvent::call_started("e", "~/p"),
            image("e", "shots/a.png", None),
            AgentEvent::call_started("f", "~/p"),
            image("f", "shots/b.png", None),
            AgentEvent::StepStarted { step: 3 },
            read(3, 0, "g", "docs/old.pdf"),
            step_ended(3, 0.01),
            withdrawal(3, &[("e", "shots/a.png"), ("f", "shots/b.png")]),
            AgentEvent::call_started("g", "~/p"),
            pdf("g", "docs/old.pdf"),
            AgentEvent::StepStarted { step: 4 },
            step_ended(4, 0.01),
            withdrawal(4, &[("g", "docs/old.pdf")]),
            ended(completed(StopReason::EndTurn, 4, 7)),
        ],
    );
    t
}

fn line_of<'a>(all: &'a str, target: &str) -> &'a str {
    all.lines()
        .find(|l| l.contains(target))
        .unwrap_or_else(|| panic!("{target}:\n{all}"))
}

#[test]
fn a_file_row_shows_its_type_size_and_tokens_without_a_clock() {
    let t = &mut files_turn();
    let all = text(&t.rows(&r(120), Detail::Scrollback));
    let sep = " \u{b7} ";
    let image = line_of(&all, "screenshots/home.png");
    assert!(image.starts_with(" \u{2713} Read "), "{image}");
    assert!(
        image.ends_with(&format!("image/png{sep}1.6 MB{sep}\u{2248}3k tokens")),
        "{image}"
    );
    let pdf = line_of(&all, "docs/spec.pdf");
    assert!(
        pdf.ends_with(&format!("application/pdf{sep}912 KB")),
        "{pdf}"
    );
    let log = line_of(&all, "build.log");
    assert!(
        log.ends_with(&format!(
            "text{sep}2,000 of 9,412 lines{sep}\u{2248}17k tokens"
        )),
        "{log}"
    );
    assert!(!all.contains("line 2000"), "{all}");
    let full = text(&t.rows(&r(120), Detail::Full));
    assert!(full.contains("line 2000"), "{full}");
}

#[test]
fn a_refused_file_reads_couldn_t_with_the_reason() {
    for width in [80, 120, 200] {
        let all = text(&files_turn().rows(&r(width), Detail::Scrollback));
        let lines: Vec<&str> = all.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("couldn't read"))
            .unwrap_or_else(|| panic!("{all}"));
        assert!(lines[at].starts_with(" \u{2a2f} Read song.mp3 "), "{all}");
        assert!(lines[at].trim_end().ends_with("  couldn't read"), "{all}");
        assert!(lines[at + 1].contains("couldn't attach"), "{all}");
        assert!(!all.contains("which this server can't carry"), "{all}");
    }
    let full = text(&files_turn().rows(&r(200), Detail::Full));
    assert!(
        full.contains("song.mp3 is audio/mpeg, which this server can't carry"),
        "{full}"
    );
    assert_eq!(copy::could_not(Some(&ToolAction::Run)), "couldn't run");
    assert_eq!(copy::could_not(None), "couldn't use");
}

#[test]
fn a_withdrawn_file_turns_its_row_into_couldn_t_attach() {
    let t = &mut files_turn();
    let all = text(&t.rows(&r(120), Detail::Scrollback));
    let lines: Vec<&str> = all.lines().collect();
    let row = |path: &str| {
        lines
            .iter()
            .find(|l| l.contains(path))
            .copied()
            .unwrap_or_else(|| panic!("{path}:\n{all}"))
    };
    for path in ["shots/a.png", "shots/b.png", "docs/old.pdf"] {
        let row = row(path);
        assert!(row.starts_with(" \u{2a2f} Read "), "{row}");
        assert!(row.ends_with("  couldn't attach"), "{row}");
    }
    assert!(!all.contains("withdrawn with 1 other attachment"), "{all}");
    assert!(!all.contains(REASON), "{all}");
    let full = text(&t.rows(&r(120), Detail::Full));
    assert!(
        full.contains("Withdrawn with 1 other attachment when the API refused the step"),
        "{full}"
    );
    assert!(full.contains(REASON), "{full}");
    assert!(
        !all.contains("from call"),
        "drawn in place, not as a notice:\n{all}"
    );
    assert_eq!(
        copy::withdrawn_with(2),
        "withdrawn with 2 other attachments"
    );
}

#[test]
fn ctrl_o_names_the_saved_copy_and_output_file_under_home_with_a_tilde() {
    let t = &mut files_turn_in(transcript().with_home(Some("/data".to_owned())));
    let full = text(&t.rows(&r(120), Detail::Full));
    assert!(
        full.contains("Saved copy: ~/wisp/attachments/3f2a9c.png"),
        "{full}"
    );
    assert!(!full.contains(SAVED), "{full}");
    assert!(
        full.contains("Output saved to /tmp/wisp-s/0003-look.out"),
        "{full}"
    );
    let t = &mut files_turn_in(transcript().with_home(Some("/tmp".to_owned())));
    let full = text(&t.rows(&r(120), Detail::Full));
    assert!(
        full.contains("Output saved to ~/wisp-s/0003-look.out"),
        "{full}"
    );
    assert!(full.contains(&format!("Saved copy: {SAVED}")), "{full}");
    assert!(
        t.tool_files()
            .iter()
            .any(|(_, path)| path == "/tmp/wisp-s/0003-look.out"),
        "the pager still opens the exact file: {:?}",
        t.tool_files()
    );
}

#[test]
fn ctrl_o_adds_the_source_line_the_saved_copy_and_the_withdrawal() {
    let t = &mut files_turn();
    let full = text(&t.rows(&r(120), Detail::Full));
    assert!(
        full.contains("~/p/screenshots/home.png: image/png, 1.6 MB, attached as is"),
        "{full}"
    );
    assert!(full.contains(&format!("Saved copy: {SAVED}")), "{full}");
    assert!(
        full.contains("~/p/build.log: text, 9,412 lines (1.2 MB)"),
        "{full}"
    );
    assert!(
        !full.contains("\n     line 1\n"),
        "only the result's first line:\n{full}"
    );
    for part in [
        "song.mp3 is audio/mpeg, which this server can't carry",
        "Convert it to one of those",
    ] {
        assert!(full.contains(part), "{full}");
    }
    assert!(
        full.contains(&format!(
            "Withdrawn with 1 other attachment when the API refused the step: {REASON}"
        )),
        "{full}"
    );
    assert!(
        full.contains(&format!("Couldn't attach: {REASON}")),
        "{full}"
    );
    assert_eq!(full.matches("Saved copy: ").count(), 1, "{full}");
}

#[test]
fn a_refused_or_withdrawn_file_lines_up_with_the_files_that_were_read() {
    for width in [40, 80, 120] {
        let all = text(&files_turn().rows(&r(width), Detail::Scrollback));
        let columns: Vec<usize> = all
            .lines()
            .filter(|l| l.contains(" Read "))
            .map(|l| {
                let after = l.find(" Read ").map_or(0, |at| at + " Read ".len());
                let target = after + l[after..].len() - l[after..].trim_start().len();
                l[..target].chars().count()
            })
            .collect();
        assert_eq!(columns.len(), 7, "{width}:\n{all}");
        assert!(
            columns.iter().all(|c| *c == columns[0]),
            "{width}: {columns:?}\n{all}"
        );
        for state in ["couldn't read", "couldn't attach"] {
            assert!(
                all.lines()
                    .any(|l| l.contains(" Read ") && l.ends_with(state)),
                "{width} {state}:\n{all}"
            );
        }
    }
}

#[test]
fn file_rows_in_every_glyph_set_at_80_and_120_columns() {
    let mut out = String::new();
    for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
        for width in [80, 120] {
            let r = Renderer::new(Layout::new(width, set));
            let rows = text(&files_turn().rows(&r, Detail::Scrollback));
            if set == GlyphSet::Ascii {
                assert!(rows.is_ascii(), "{rows}");
            }
            for line in rows.lines() {
                assert!(
                    crate::text::width(line, r.layout().width_mode) <= width,
                    "{set:?} {width}: {line}"
                );
            }
            out.push_str(&format!("=== {set:?} {width} ===\n{rows}\n"));
        }
    }
    insta::assert_snapshot!("file_rows", out);
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    insta::assert_snapshot!("file_rows_full", text(&files_turn().rows(&r, Detail::Full)));
}

fn sliced_files() -> Transcript {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
        ],
    );
    const PDF: &str = "application/pdf";
    const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
    let pages = FileSlice::pages(14, 16).unwrap();
    let sheet = FileSlice::sheet(2).unwrap();
    let clip = FileSlice::seconds(30, 90).unwrap();
    for (block, (path, media_type, n, size, slice, tokens)) in [
        ("docs/manual.pdf", PDF, 912_000, "912 KB", pages, false),
        ("docs/manual.pdf", PDF, 912_000, "912 KB", pages, true),
        ("budget.xlsx", XLSX, 48_000, "48 KB", sheet, false),
        ("clip.wav", "audio/wav", 1_400_000, "1.4 MB", clip, false),
    ]
    .into_iter()
    .enumerate()
    {
        let id = block.to_string();
        let mut facts = vec![
            Fact::Media {
                media_type: mime(media_type),
            },
            Fact::Bytes { n },
        ];
        let estimate = if tokens {
            facts.push(Fact::Tokens { n: 1680 });
            "; about 1,680 tokens, the server's estimate"
        } else {
            ""
        };
        let result = format!(
            "~/p/{path}: {media_type}, {size}, attached whole (read only {slice}{estimate})."
        );
        let attachment = AttachmentRef {
            slice: Some(slice),
            ..attached(media_type, n, None)
        };
        apply(
            &mut t,
            &[
                read(1, block, &id, path),
                AgentEvent::call_started(&id, "~/p"),
                viewed(&id, facts, &result, vec![attachment]),
            ],
        );
    }
    t
}

#[test]
fn file_slice_survives_count_facts_at_all_mockup_widths() {
    let t = &mut sliced_files();
    for (width, unicode, ascii) in [
        (
            40,
            r#" ✓ Read docs/ma…  read only pages 14–16
 ✓ Read docs/ma…  read only pages 14–16
 ✓ Read budget.xlsx   read only sheet 2
 ✓ Read clip.…  read only seconds 30–90"#,
            r#" + Read docs/...  read only pages 14-16
 + Read docs/...  read only pages 14-16
 + Read budget.xlsx   read only sheet 2
 + Read cli...  read only seconds 30-90"#,
        ),
        (
            80,
            r#" ✓ Read docs/manual.pdf        application/pdf · read only pages 14–16 · 912 KB
 ✓ Read docs/manual.pdf                                   read only pages 14–16
 ✓ Read budget.xlsx                                           read only sheet 2
 ✓ Read clip.wav                   audio/wav · read only seconds 30–90 · 1.4 MB"#,
            r#" + Read docs/manual.pdf        application/pdf - read only pages 14-16 - 912 KB
 + Read docs/manual.pdf                                   read only pages 14-16
 + Read budget.xlsx                                           read only sheet 2
 + Read clip.wav                   audio/wav - read only seconds 30-90 - 1.4 MB"#,
        ),
        (
            120,
            r#" ✓ Read docs/manual.pdf                                                application/pdf · read only pages 14–16 · 912 KB
 ✓ Read docs/manual.pdf                                   application/pdf · read only pages 14–16 · 912 KB · ≈2k tokens
 ✓ Read budget.xlsx       application/vnd.openxmlformats-officedocument.spreadsheetml.sheet · read only sheet 2 · 48 KB
 ✓ Read clip.wav                                                           audio/wav · read only seconds 30–90 · 1.4 MB"#,
            r#" + Read docs/manual.pdf                                                application/pdf - read only pages 14-16 - 912 KB
 + Read docs/manual.pdf                                   application/pdf - read only pages 14-16 - 912 KB - ~2k tokens
 + Read budget.xlsx       application/vnd.openxmlformats-officedocument.spreadsheetml.sheet - read only sheet 2 - 48 KB
 + Read clip.wav                                                           audio/wav - read only seconds 30-90 - 1.4 MB"#,
        ),
    ] {
        let wide = ascii.replace(" + Read ", " ✓ Read ").replace(" - ", " ∙ ");
        for (set, expected) in [
            (GlyphSet::Unicode, unicode),
            (GlyphSet::AmbiguousWide, wide.as_str()),
            (GlyphSet::Ascii, ascii),
        ] {
            let r = Renderer::new(Layout::new(width, set));
            let rows = t.rows(&r, Detail::Scrollback);
            let all = text(&rows);
            let read = all
                .lines()
                .filter(|line| line.contains(" Read "))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(read, expected, "{set:?} {width}");
            let slices: Vec<_> = rows
                .iter()
                .flat_map(|row| &row.spans)
                .filter(|span| span.text.starts_with("read only "))
                .collect();
            assert_eq!(slices.len(), 4, "{set:?} {width}");
            assert!(
                slices
                    .iter()
                    .all(|span| span.look == Look::fg(Token::TextMuted))
            );
            let full = text(&t.rows(&r, Detail::Full));
            let words = full.split_whitespace().collect::<Vec<_>>().join(" ");
            for result in [
                "attached whole (read only pages 14-16).",
                "attached whole (read only pages 14-16; about 1,680 tokens, the server's estimate).",
            ] {
                assert!(words.contains(result), "{result}:\n{full}");
            }
        }
    }
}

#[test]
fn only_one_attachment_and_one_media_fact_describe_a_slice() {
    let mut t = sliced_files();
    let pdf = Fact::Media {
        media_type: mime("application/pdf"),
    };
    let sliced = AttachmentRef {
        slice: Some(FileSlice::pages(14, 16).unwrap()),
        ..attached("application/pdf", 912_000, None)
    };
    let plain = attached("application/pdf", 912_000, None);
    for (block, path, facts, attachments) in [
        (4, "one.pdf", vec![pdf.clone()], vec![plain.clone()]),
        (5, "two.pdf", vec![pdf.clone()], vec![sliced.clone(), plain]),
        (6, "two-facts.pdf", vec![pdf.clone(), pdf], vec![sliced]),
    ] {
        apply(
            &mut t,
            &[
                read(1, block, path, path),
                AgentEvent::call_started(path, "~/p"),
                viewed(path, facts, "attached as is.", attachments),
            ],
        );
    }
    let all = text(&t.rows(&r(120), Detail::Scrollback));
    for path in ["one.pdf", "two.pdf", "two-facts.pdf"] {
        assert!(!line_of(&all, path).contains("read only"), "{all}");
    }
}

#[test]
fn expanded_slice_refusals_keep_the_reason_and_withdrawals_keep_the_requested_slice() {
    let mut t = sliced_files();
    let reason = "No file was sent: call.mp3 can't be sliced with `seconds` on this model (it takes pages, sheet).";
    apply(
        &mut t,
        &[
            read(1, 4, "d", "call.mp3"),
            AgentEvent::call_started("d", "~/p"),
            with_result(
                "d",
                ToolOutcome::Completed {
                    is_error: true,
                    exit: None,
                },
                reason,
                Vec::new(),
            ),
            withdrawal(2, &[("0", "docs/manual.pdf")]),
        ],
    );
    let collapsed = text(&t.rows(&r(120), Detail::Scrollback));
    assert!(
        line_of(&collapsed, "docs/manual.pdf").ends_with("couldn't attach"),
        "{collapsed}"
    );
    assert!(
        line_of(&collapsed, "call.mp3").ends_with("couldn't read"),
        "{collapsed}"
    );
    assert!(!collapsed.contains(reason), "{collapsed}");
    let full = text(&t.rows(&r(120), Detail::Full));
    assert!(full.contains(reason), "{full}");
    assert!(
        full.contains("attached whole (read only pages 14-16)."),
        "{full}"
    );
    assert!(
        full.contains(&format!("Couldn't attach: {REASON}")),
        "{full}"
    );
}

fn with_result(
    id: &str,
    outcome: ToolOutcome,
    result: &str,
    attachments: Vec<AttachmentRef>,
) -> AgentEvent {
    tool_finished(Finished {
        call_id: id.to_owned(),
        outcome,
        output: view("out 1\nout 2\n", 2),
        duration_ms: 1_000,
        result: Some(result.to_owned()),
        attachments,
        ..Finished::default()
    })
}

#[test]
fn a_withdrawal_moves_the_tool_revision_so_ctrl_o_refreshes() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            read(1, 0, "a", "x.png"),
            step_ended(1, 0.01),
            AgentEvent::call_started("a", "~/p"),
            image("a", "x.png", None),
        ],
    );
    let before = t.tools_revision();
    t.apply(&withdrawal(2, &[("a", "x.png")]), ms(500));
    assert!(t.tools_revision() > before);
    let after = t.tools_revision();
    t.apply(&withdrawal(3, &[("gone", "y.png")]), ms(600));
    assert_eq!(t.tools_revision(), after, "a notice redraws no row");
}

#[test]
fn a_command_s_result_stays_out_of_ctrl_o() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "make"),
            started("a"),
            with_result("a", ok(0), "Exit code: 0\nOutput:\nRESULT-ONLY", vec![]),
        ],
    );
    let full = text(&t.rows(&r(120), Detail::Full));
    assert!(full.contains("out 2"), "{full}");
    assert!(!full.contains("RESULT-ONLY"), "{full}");
    assert!(!full.contains("Exit code: 0"), "{full}");
}

#[test]
fn a_saved_path_with_control_characters_is_sanitised() {
    let mut t = transcript();
    let evil = format!("/data/{ESC}[31mred\u{7}\nnext.png");
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            read(1, 0, "a", "x.png"),
            started("a"),
            viewed(
                "a",
                vec![Fact::Media {
                    media_type: mime("image/png"),
                }],
                "~/p/x.png: image/png, 12 bytes, attached as is.",
                vec![attached("image/png", 12, Some(&evil))],
            ),
        ],
    );
    let full = text(&t.rows(&r(120), Detail::Full));
    let saved = line_of(&full, "Saved copy: /data/");
    assert!(!saved.chars().any(char::is_control), "{saved:?}");
    assert!(
        saved.contains("red") && saved.contains("next.png"),
        "{saved:?}"
    );
    assert!(!full.contains(ESC) && !full.contains('\u{7}'), "{full:?}");
}

#[test]
fn an_error_with_an_exit_code_is_not_drawn_as_a_refusal() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "make check"),
            started("a"),
            with_result(
                "a",
                ToolOutcome::Completed {
                    is_error: true,
                    exit: Some(ExitStatus::Code { code: 2 }),
                },
                "make: *** [check] Error 2",
                vec![],
            ),
        ],
    );
    let all = text(&t.rows(&r(120), Detail::Scrollback));
    let row = line_of(&all, "make check");
    assert!(row.starts_with(" \u{2a2f} Ran "), "{row}");
    assert!(!all.contains("Couldn't"), "{all}");
    assert!(!all.contains("Error 2"), "{all}");
}

#[test]
fn a_text_view_reads_text_then_its_lines() {
    let g = crate::theme::glyphs::Glyphs::of(GlyphSet::Ascii);
    let part = |sent, total| {
        copy::fact(
            &Fact::Part {
                sent,
                total,
                more_than: false,
            },
            &g,
        )
    };
    assert_eq!(part(2_000, 9_412), "2,000 of 9,412 lines");
    assert_eq!(part(9_412, 9_412), "9,412 lines");
    assert_eq!(part(1, 1), "1 line");
    assert_eq!(part(0, 0), "0 lines");
    let more = |sent, total| {
        copy::fact(
            &Fact::Part {
                sent,
                total,
                more_than: true,
            },
            &g,
        )
    };
    assert_eq!(more(2_000, 2_000), "first 2,000 lines; the file has more");
    assert_eq!(more(1, 0), "first 1 line; the file has more");
    assert_eq!(more(1, 1), "first 1 line; the file has more");
    assert_eq!(more(1_001, 1_000), "first 1,001 lines; the file has more");
    assert_eq!(copy::size(999_949_999), "999.9 MB");
    assert_eq!(copy::size(999_950_000), "1 GB");
    assert_eq!(copy::size(1_260_000_000), "1.3 GB");
    assert_eq!(
        copy::fact_kind(&Fact::Part {
            sent: 1,
            total: 1,
            more_than: false
        }),
        Some("text")
    );
    assert_eq!(
        copy::fact_kind(&Fact::Media {
            media_type: mime("image/png")
        }),
        None
    );
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            read(1, 0, "a", "notes.txt"),
            started("a"),
            viewed(
                "a",
                vec![
                    Fact::Part {
                        sent: 412,
                        total: 412,
                        more_than: false,
                    },
                    Fact::Tokens { n: 4_400 },
                ],
                "~/p/notes.txt: text, 412 lines (13.0 KB), all below (about 4,400 tokens).",
                vec![],
            ),
        ],
    );
    let all = text(&t.rows(&r(120), Detail::Scrollback));
    let row = line_of(&all, "notes.txt");
    assert!(
        row.trim_end()
            .ends_with("text \u{b7} 412 lines \u{b7} \u{2248}4k tokens"),
        "{row}"
    );
    assert!(!row.contains("text/plain"), "{row}");
}
