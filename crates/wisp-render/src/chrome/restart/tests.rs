use super::*;
use crate::testing::trimmed;
use crate::theme::glyphs::GlyphSet;

const TITLES: [&str; 4] = [
    "Check example tool setup and login",
    "Example Brand Model X-100 Sample Radio",
    "Fetch an example index and compare it with the sample",
    "Find sample mentions in the example file",
];

fn row(what: Unfinished, title: Option<&str>, ms: u64) -> Unready {
    Unready {
        agent: wisp_protocol::AgentPath::root(),
        what,
        title: title.map(str::to_owned),
        target: String::new(),
        running_ms: ms,
        opening: false,
    }
}

fn chats() -> Vec<Chat> {
    TITLES
        .iter()
        .enumerate()
        .map(|(index, title)| Chat {
            kept: Kept {
                id: format!("019a7c55-7d1{index}"),
                title: (*title).to_owned(),
                window: false,
                older: false,
            },
            state: State::Restarted(Vec::new()),
        })
        .collect()
}

fn a1() -> Vec<Chat> {
    let mut chats = chats();
    chats[2].state = State::Finishing(vec![row(
        Unfinished::Process,
        Some("Run the workspace tests"),
        843_000,
    )]);
    chats[3].state = State::Finishing(vec![row(Unfinished::Reply, None, 6_000)]);
    chats
}

#[test]
fn the_approved_start_has_one_phrase_per_chat_and_one_time_column_at_each_width() {
    let expected = [
        (
            40,
            vec![
                "Restarting 4 chats · 2 still finishing",
                "stops them in 4m 57s",
                "",
                "  ✓ Check example tool setup and login",
                "  ✓ Example Brand Model X-100 Sample Ra…",
                "  ◌ Fetch an example index and compare…",
                "    finishing: Run the workspa…  14m 03s",
                "  ◌ Find sample mentions in the example…",
                "    finishing a reply                 6s",
                "",
                "enter restart now · ctrl-c cancel",
            ],
        ),
        (
            80,
            vec![
                "Restarting 4 chats · 2 still finishing · stops them in 4m 57s",
                "",
                "  ✓ Check example tool setup and login",
                "  ✓ Example Brand Model X-100 Sample Radio",
                "  ◌ Fetch an example index and com…  finishing: Run the workspace tests  14m 03s",
                "  ◌ Find sample mentions in the ex…  finishing a reply                        6s",
                "",
                "enter restart now · ctrl-c cancel",
            ],
        ),
        (
            120,
            vec![
                "Restarting 4 chats · 2 still finishing · stops them in 4m 57s",
                "",
                "  ✓ Check example tool setup and login",
                "  ✓ Example Brand Model X-100 Sample Radio",
                "  ◌ Fetch an example index and compare it with the sample  finishing: Run the workspace tests  14m 03s",
                "  ◌ Find sample mentions in the example file               finishing a reply                        6s",
                "",
                "enter restart now · ctrl-c cancel",
            ],
        ),
    ];
    for (width, expected) in expected {
        assert_eq!(
            trimmed(&live(
                &a1(),
                Some(297),
                24,
                Layout::new(width, GlyphSet::Unicode)
            )),
            expected,
            "{width}"
        );
    }
    let mut chats = a1().split_off(2);
    chats[0].state = State::Finishing(vec![
        row(Unfinished::ToolCall, None, 843_000),
        row(Unfinished::Reply, None, 6_000),
        row(Unfinished::Process, None, 5_000),
    ]);
    for (glyphs, mark, first, second) in [
        (
            GlyphSet::Unicode,
            "◌",
            "Fetch an example index and compare…",
            "Find sample mentions in the example…",
        ),
        (
            GlyphSet::Ascii,
            "o",
            "Fetch an example index and compar...",
            "Find sample mentions in the examp...",
        ),
    ] {
        let layout = Layout::new(40, glyphs);
        let lines = rows(&chats, layout);
        assert_eq!(
            trimmed(&lines),
            [
                format!("  {mark} {first}"),
                "    finishing a tool call +2 more".to_owned(),
                format!("{:>40}", "14m 03s"),
                format!("  {mark} {second}"),
                "    finishing a reply                 6s".to_owned(),
            ]
        );
        assert!(
            lines
                .iter()
                .all(|line| spans_width(&line.spans, layout.width_mode) <= 40)
        );
    }
}

#[test]
fn a_description_is_never_guessed_and_the_longest_row_and_other_count_are_shared() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    for (kind, description, expected) in [
        (
            Unfinished::Reply,
            Some("ignored description"),
            "finishing a reply",
        ),
        (Unfinished::ToolCall, None, "finishing a tool call"),
        (Unfinished::Process, None, "finishing a process"),
        (Unfinished::Other, Some("ignored"), "finishing something"),
        (
            Unfinished::Process,
            Some("Run the workspace tests"),
            "finishing: Run the workspace tests",
        ),
    ] {
        let open = [
            row(kind, description, 843_000),
            row(Unfinished::Reply, None, 6_000),
        ];
        let phrase = phrase(&open, Phase::Finishing, 80, layout);
        assert_eq!(
            crate::text::spans_text(&phrase.spans),
            format!("{expected} +1 more")
        );
        assert_eq!(phrase.time.as_deref(), Some("14m 03s"));
    }
    assert_eq!(
        crate::text::spans_text(&phrase(&[], Phase::Finishing, 80, layout).spans),
        "restarting"
    );
    let mut opening = row(Unfinished::Reply, None, 6_000);
    assert_eq!(
        phrase(std::slice::from_ref(&opening), Phase::Stopped, 80, layout).spans[0].look,
        Look::fg(Token::Text)
    );
    opening.opening = true;
    assert_eq!(
        phrase(&[opening], Phase::Stopped, 80, layout).spans[0].look,
        Look::fg(Token::Warn)
    );
}

#[test]
fn the_end_names_stopped_work_and_only_promises_to_carry_on_where_it_will() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut chats = chats();
    assert_eq!(
        trimmed(&end(&chats, &End::default(), layout)),
        ["Restarted 4 chats on the newest version."]
    );
    chats[2].state = State::Restarted(vec![row(
        Unfinished::Process,
        Some("Run the workspace tests"),
        852_000,
    )]);
    let mut opening = row(Unfinished::Reply, None, 18_000);
    opening.opening = true;
    chats[3].state = State::Restarted(vec![opening.clone()]);
    assert_eq!(
        trimmed(&end(&chats, &End::default(), layout)),
        [
            "Restarted 4 chats on the newest version.",
            "Stopped what 2 chats were finishing; 1 chat waits for you, the other carries on.",
            "",
            "  ✓ Fetch an example index and compa…  stopped: Run the workspace tests  14m 12s",
            "  ✓ Find sample mentions in the exam…  send your message again",
        ]
    );
    for (open, sentence) in [
        (
            vec![row(Unfinished::Reply, None, 18_000)],
            "Stopped what 1 chat was finishing; its agents carry on.",
        ),
        (
            vec![opening.clone()],
            "Stopped what 1 chat was finishing; it waits for you.",
        ),
    ] {
        chats[3].state = State::Restarted(Vec::new());
        chats[2].state = State::Restarted(open);
        assert_eq!(trimmed(&end(&chats, &End::default(), layout))[1], sentence);
    }
    chats[3].state = State::Restarted(vec![opening]);
    assert_eq!(
        trimmed(&end(&chats, &End::default(), layout))[1],
        "Stopped what 2 chats were finishing; they wait for you."
    );
    chats[2].state = State::Restarted(vec![row(Unfinished::Process, None, 1_140_000)]);
    chats[3].state = State::Restarted(vec![row(Unfinished::ToolCall, None, 290_000)]);
    assert_eq!(
        trimmed(&end(
            &chats,
            &End {
                waited: Some(300),
                ..End::default()
            },
            layout
        ))[1],
        "Waited 5m, then stopped what 2 chats were finishing; their agents carry on."
    );
}

#[test]
fn cancelled_and_failed_chats_stay_and_never_count_as_restarted() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut chats = chats();
    chats[2].state = State::Cancelled;
    chats[3].state = State::Cancelled;
    assert_eq!(
        &trimmed(&end(
            &chats,
            &End {
                cancelled: true,
                ..End::default()
            },
            layout
        ))[..2],
        [
            "Restarted 2 of 4 chats on the newest version.",
            "Cancelled: the other 2 carry on as they were."
        ]
    );
    chats[2].state = State::Failed("placeholder start failure\nsecond line".to_owned());
    chats[3].state = State::Restarted(Vec::new());
    let rendered = end(&chats, &End::default(), layout);
    assert!(
        rendered
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| { span.text == AGENTS_WAIT && span.look == Look::fg(Token::Warn) })
    );
    let failed = trimmed(&rendered).join("\n");
    assert!(failed.starts_with("Restarted 3 of 4 chats on the newest version.\n"));
    assert!(failed.contains("didn't start again"));
    assert!(failed.contains("placeholder start failure\n      second line"));
    assert!(failed.contains("Its agents wait for you. Open it again with: wisp --resume"));
    chats[2].state = State::NoAnswer;
    assert!(
        trimmed(&end(&chats, &End::default(), layout))
            .join("\n")
            .contains("It keeps running as it was; try wisp restart again later.")
    );
    chats[2].state = State::Restarted(Vec::new());
    chats[2].kept.older = true;
    assert!(
        trimmed(&end(&chats, &End::default(), layout))
            .join("\n")
            .contains("older version")
    );
    chats[2].kept.older = false;
    chats[2].state = State::Cancelled;
    let cancelled = trimmed(&end(
        &chats,
        &End {
            cancelled: true,
            ..End::default()
        },
        layout,
    ));
    assert!(
        cancelled
            .iter()
            .any(|line| line == "Cancelled: the other chat carries on as it was.")
    );
    for chat in &mut chats {
        chat.state = State::Cancelled;
    }
    assert_eq!(
        trimmed(&end(
            &chats,
            &End {
                cancelled: true,
                ..End::default()
            },
            layout
        )),
        ["Cancelled: no chat was restarted."]
    );
    chats[2].state = State::Failed("placeholder failure".into());
    let failed = trimmed(&end(
        &chats,
        &End {
            cancelled: true,
            ..End::default()
        },
        layout,
    ))
    .join("\n");
    assert!(failed.contains("didn't start again"));
    assert!(failed.contains("wisp --resume"));
}

#[test]
fn tall_lists_fold_completed_chats_then_count_the_rows_left_out() {
    let mut chats = a1();
    if let State::Finishing(open) = &mut chats[2].state {
        open.extend([
            row(Unfinished::Reply, None, 1),
            row(Unfinished::ToolCall, None, 2),
        ]);
    }
    for (title, state) in [
        ("Plan the example garden bed", State::Restarted(Vec::new())),
        ("Compare three sample widgets", State::Restarting),
        ("~/Projects/example-app", State::Restarted(Vec::new())),
        (
            "Sort an example archive by day",
            State::Finishing(vec![row(Unfinished::Process, None, 161_000)]),
        ),
    ] {
        chats.push(Chat {
            kept: Kept {
                id: String::new(),
                title: title.to_owned(),
                window: false,
                older: false,
            },
            state,
        });
    }
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let layout = Layout::new(width, glyphs);
            let wide = trimmed(&live(&chats, Some(297), 24, layout));
            assert!(!wide.iter().any(|line| line.contains("4 restarted")));
            let folded = trimmed(&live(&chats, Some(297), 11, layout));
            assert!(folded.len() <= 11);
            assert!(folded.iter().any(|line| line.contains("4 restarted")));
            assert_eq!(
                folded.iter().any(|line| line.contains("2 more chats")),
                width == 40
            );
            for line in &folded {
                assert!(
                    crate::text::width(line, layout.width_mode) <= width,
                    "{line}"
                );
            }
        }
    }
}

#[test]
fn the_first_question_and_daemon_share_clean_titles_and_keep_the_ids_only_where_needed() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut kept = chats()
        .into_iter()
        .map(|chat| chat.kept)
        .collect::<Vec<_>>();
    kept[1].window = true;
    kept[2].window = true;
    kept[2].older = true;
    let mut single = vec![kept[0].clone()];
    single[0].window = true;
    let question = trimmed(&older_question(&single, layout)).join("\n");
    for text in [
        "This chat runs an older version of Wisp that can't wait for work to finish:",
        "Restarting it stops what it is doing; agents at work carry on afterwards.",
        "Restart it on the newest version?",
        "Its window closes; it prints the command that opens it again.",
    ] {
        assert!(question.contains(text), "{question}");
    }
    let mut group = vec![kept[0].clone(), kept[1].clone()];
    group[1].window = true;
    assert!(
        trimmed(&older_question(&group, layout))
            .join("\n")
            .contains("One window closes; it prints the command that opens it again.")
    );
    let question = trimmed(&older_question(&kept, layout));
    assert!(question.iter().any(|line| line
        == "A chat started with its own settings file, keys file or keychain comes back"));
    assert!(question.iter().any(|line| line == "without it."));
    assert!(question.iter().all(|line| !line.contains("019a7c55")));
    let styled = daemon(&kept, layout);
    assert!(
        styled[2]
            .spans
            .iter()
            .any(|span| span.text.contains(OLDER_VERSION) && span.look == Look::fg(Token::Text))
    );
    assert!(
        styled[2]
            .spans
            .iter()
            .any(|span| span.text.contains(&kept[2].id) && span.look == Look::fg(Token::TextMuted))
    );
    let listed = trimmed(&styled);
    assert_eq!(listed.len(), 4);
    assert!(listed[2].ends_with("in a window  older version"));
    kept[0].title = "a title\x1b[31m\nnext line".to_owned();
    for width in [40, 80, 120] {
        let layout = Layout::new(width, GlyphSet::Ascii);
        assert!(!trimmed(&daemon(&kept, layout)).join("\n").contains('\x1b'));
    }
}
