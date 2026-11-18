use wisp_protocol::{
    AgentEvent, CacheEffect, DiffView, Dropped, Fact, RewindPoint, RewindReason, StopReason,
    ToolView,
};
use wisp_tui::app::find::{NO_MATCHES, search_key};
use wisp_tui::app::pager::return_keys;
use wisp_tui::term::{Input, Key, KeyCode};

use wisp_render::motion::MotionPolicy;
use wisp_render::theme::Depth;
use wisp_tui::app::highlight::{Start, default_feed};

use super::super::{
    Look, MODES, Screen, Stage, completed, cost, ended, facts, snapshot, staged, staged_tuned,
    tool_events as te,
};
use super::{
    DEPTHS, SETS, SIZES, hint, in_reader, long_thought, look, open, open_row, show_turn,
    think_more, tool_turn,
};
use crate::events::{show_finished, show_requested};
use crate::{Drive, eventually};

fn changed_events() -> [AgentEvent; 2] {
    let added: String = (1..=34)
        .map(|i| format!("+    let part_{i} = fingerprint.segment({i});\n"))
        .collect();
    let hunks = format!(
        "@@ -1,3 +1,38 @@\n pub fn ttl() -> Duration {{\n-    Duration::from_secs(300)\n+    Duration::from_secs(3600)\n }}\n+// 3600 seconds is one hour\n{added}"
    );
    let path = "crates/wisp-core/src/cache.rs";
    let file = DiffView {
        path: Some(path.to_owned()),
        old_path: None,
        language: None,
        hunks,
        added: 36,
        removed: 1,
    };
    let view = ToolView::Diff {
        title: None,
        files: vec![file],
    };
    let fact = Fact::Diffstat {
        added: 36,
        removed: 1,
    };
    [
        show_requested("d", path, None, view),
        show_finished(
            "d",
            vec![fact],
            "Shown to the user: crates/wisp-core/src/cache.rs, +36 -1.",
        ),
    ]
}

async fn changed_turn(s: &Stage) {
    show_turn(s, "Raise the TTL", changed_events()).await;
}

async fn press(s: &Stage, code: KeyCode) {
    s.key(code).wait(50).await;
}

pub(super) async fn typed(s: &Stage, text: &str) {
    for c in text.chars() {
        s.key(KeyCode::Char(c));
    }
    s.wait(100).await;
}

fn top_of(s: &Stage) -> usize {
    hint(s)
        .split(" \u{b7} ")
        .find_map(|p| p.split('-').next().and_then(|n| n.parse().ok()))
        .unwrap_or_else(|| panic!("no position: {:?}", hint(s)))
}

fn marked(s: &Stage) -> Vec<String> {
    let rows = s.screen.visible();
    s.screen
        .inverse_rows()
        .into_iter()
        .filter_map(|(row, _)| rows.get(usize::from(row)).cloned())
        .collect()
}

fn marks(s: &Stage) -> Vec<String> {
    s.screen
        .inverse_rows()
        .into_iter()
        .map(|(_, text)| text)
        .collect()
}

fn styles_of(s: &Stage, needle: &str) -> String {
    let shot = s.screen.styled();
    let mut lines = shot.lines();
    while let Some(line) = lines.next() {
        if line.contains(needle) {
            let next = lines.next().unwrap_or_default();
            return if next.starts_with("  ~") {
                next.to_owned()
            } else {
                String::new()
            };
        }
    }
    String::new()
}

fn emphasised(styles: &str) -> bool {
    let backs: std::collections::BTreeSet<&str> = styles
        .split("; ")
        .filter_map(|run| run.split(" bg ").nth(1))
        .map(|bg| bg.split(' ').next().unwrap_or_default())
        .collect();
    styles.contains("inverse") || backs.len() >= 2
}

async fn highlighted(s: &Stage) {
    let held = eventually(|| emphasised(&styles_of(s, " -     Duration::"))).await;
    assert!(
        held,
        "the changed word was never emphasised: {}",
        s.screen.styled()
    );
    s.wait(100).await;
}

async fn searched_diff(s: &Stage) {
    changed_turn(s).await;
    open(s, "more lines").await;
    highlighted(s).await;
    press(s, KeyCode::Char('/')).await;
    typed(s, "3600").await;
    s.wait(300).await;
}

async fn staged_highlighting<D, F>(look: Look, drive: D) -> Vec<(String, String)>
where
    D: FnOnce(Stage) -> F,
    F: std::future::Future<Output = ()>,
{
    staged_tuned(
        Screen::new(look.rows, look.width),
        look,
        facts(),
        |options| options.code = Start::Loaded(crate::grammars(), default_feed()),
        drive,
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn the_reader_search_at_every_size_theme_glyph_set_and_depth() {
    for mode in MODES {
        for depth in DEPTHS {
            for glyphs in SETS {
                for (width, rows) in SIZES {
                    let look = Look {
                        mode,
                        depth,
                        glyphs,
                        width,
                        rows,
                        motion: MotionPolicy::Full,
                    };
                    let shots = staged_highlighting(look, |s| async move {
                        searched_diff(&s).await;
                        s.shot("reader");
                    })
                    .await;
                    snapshot("reader-search", look, &shots);
                }
            }
        }
    }
}

async fn long_thought_turn(s: &Stage, lines: usize) {
    s.send("Why do misses lose their cause?").await;
    s.event(te::step(1)).await;
    s.think(0, &long_thought(lines)).await;
    s.wait(2_000).await.stop(0).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
        2_500,
    ))
    .await;
    s.wait(3000).await;
}

#[tokio::test(start_paused = true)]
async fn the_changed_diff_word_is_marked_bold_and_underlined_where_it_is_already_reversed() {
    for depth in [Depth::Ansi16, Depth::None] {
        let look = Look {
            depth,
            ..look(80, 24)
        };
        staged_highlighting(look, |s| async move {
            changed_turn(&s).await;
            open(&s, "more lines").await;
            highlighted(&s).await;
            let before = styles_of(&s, "from_secs(3600)");
            assert!(before.contains("inverse"), "{before}");
            assert!(!before.contains("bold underline inverse"), "{before}");
            press(&s, KeyCode::Char('/')).await;
            typed(&s, "3600").await;
            assert!(hint(&s).contains("1 of 2"), "{:?}", hint(&s));
            let after = styles_of(&s, "from_secs(3600)");
            assert!(after.contains("bold underline inverse"), "{after}");
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn b_f_d_and_u_move_a_page_or_half_and_their_ctrl_forms_do_nothing() {
    staged(look(80, 24), |s| async move {
        tool_turn(&s).await;
        open_row(&s, "cargo test -p wisp-core").await;
        assert_eq!(top_of(&s), 1, "{:?}", hint(&s));
        press(&s, KeyCode::Char('f')).await;
        let page = top_of(&s) - 1;
        assert!(page > 10, "{:?}", hint(&s));
        press(&s, KeyCode::Char('b')).await;
        assert_eq!(top_of(&s), 1);
        press(&s, KeyCode::Char('d')).await;
        assert_eq!(top_of(&s) - 1, page / 2, "{:?}", hint(&s));
        press(&s, KeyCode::Char('u')).await;
        assert_eq!(top_of(&s), 1);
        for c in ['f', 'd', 'b'] {
            s.input(Input::Key(Key::ctrl(c))).wait(50).await;
            assert!(in_reader(&s), "ctrl-{c}: {:#?}", s.screen.visible());
            assert_eq!(top_of(&s), 1, "ctrl-{c}");
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn search_moves_as_you_type_enter_keeps_the_mark_and_esc_puts_the_page_back() {
    staged(look(80, 24), |s| async move {
        long_thought_turn(&s, 60).await;
        open(&s, "Thought for").await;
        assert_eq!(top_of(&s), 1);
        assert!(
            hint(&s).trim_start().starts_with(&search_key().text()),
            "{:?}",
            hint(&s)
        );
        press(&s, KeyCode::Char('/')).await;
        assert!(hint(&s).trim_start().starts_with('/'), "{:?}", hint(&s));
        typed(&s, "line 5").await;
        assert!(hint(&s).contains("1 of 11"), "{:?}", hint(&s));
        assert!(
            marked(&s)[0].contains("Thought line 5 "),
            "{:#?}",
            marked(&s)
        );
        typed(&s, "5").await;
        assert!(hint(&s).contains("1 of 1"), "{:?}", hint(&s));
        assert_eq!(marks(&s), vec!["line 55".to_owned()]);
        assert!(
            marked(&s)[0].contains("Thought line 55 "),
            "{:#?}",
            marked(&s)
        );
        press(&s, KeyCode::Esc).await;
        assert!(in_reader(&s), "{:?}", hint(&s));
        assert_eq!(top_of(&s), 1);
        assert!(marks(&s).is_empty());
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "line 55").await;
        press(&s, KeyCode::Enter).await;
        assert!(in_reader(&s), "{:?}", hint(&s));
        assert!(top_of(&s) > 1);
        assert!(
            marked(&s)[0].contains("Thought line 55 "),
            "{:#?}",
            marked(&s)
        );
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "nowhere").await;
        assert!(hint(&s).contains(NO_MATCHES), "{:?}", hint(&s));
        press(&s, KeyCode::Esc).await;
        assert!(
            marked(&s)[0].contains("Thought line 55 "),
            "{:#?}",
            marked(&s)
        );
        press(&s, KeyCode::Char('q')).await;
        assert!(!in_reader(&s));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn n_and_shift_n_step_through_the_matches_and_wrap_at_either_end() {
    staged(look(80, 24), |s| async move {
        long_thought_turn(&s, 60).await;
        open(&s, "Thought for").await;
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "weighs").await;
        assert!(hint(&s).contains("1 of 60"), "{:?}", hint(&s));
        press(&s, KeyCode::Enter).await;
        assert!(
            marked(&s)[0].contains("Thought line 1 "),
            "{:#?}",
            marked(&s)
        );
        press(&s, KeyCode::Char('n')).await;
        assert!(
            marked(&s)[0].contains("Thought line 2 "),
            "{:#?}",
            marked(&s)
        );
        press(&s, KeyCode::Char('N')).await;
        press(&s, KeyCode::Char('N')).await;
        assert!(
            marked(&s)[0].contains("Thought line 60 "),
            "{:#?}",
            marked(&s)
        );
        assert!(top_of(&s) > 1);
        press(&s, KeyCode::Char('n')).await;
        assert!(
            marked(&s)[0].contains("Thought line 1 "),
            "{:#?}",
            marked(&s)
        );
        assert_eq!(top_of(&s), 1);
        press(&s, KeyCode::Char('?')).await;
        typed(&s, "line 3").await;
        press(&s, KeyCode::Enter).await;
        assert!(
            marked(&s)[0].contains("Thought line 39 "),
            "{:#?}",
            marked(&s)
        );
        press(&s, KeyCode::Char('n')).await;
        assert!(
            marked(&s)[0].contains("Thought line 38 "),
            "{:#?}",
            marked(&s)
        );
        press(&s, KeyCode::Char('N')).await;
        assert!(
            marked(&s)[0].contains("Thought line 39 "),
            "{:#?}",
            marked(&s)
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_rewrap_and_a_live_refresh_keep_the_current_match() {
    staged(look(80, 24), |s| async move {
        long_thought_turn(&s, 60).await;
        open(&s, "Thought for").await;
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "line 55").await;
        press(&s, KeyCode::Enter).await;
        let size = s.screen.resize(24, 60);
        s.input(Input::Resize(size)).wait(1_000).await;
        assert!(
            marked(&s)[0].contains("Thought line 55 "),
            "{:#?}",
            s.screen.visible()
        );
        press(&s, KeyCode::Char('n')).await;
        assert!(
            marked(&s)[0].contains("Thought line 55 "),
            "{:#?}",
            s.screen.visible()
        );
    })
    .await;
    staged(look(80, 24), |s| async move {
        s.send("Why?").await;
        s.event(te::step(1)).await;
        s.think(0, &long_thought(30)).await;
        s.wait(500).await;
        super::open_live(&s).await;
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "weighs").await;
        press(&s, KeyCode::Enter).await;
        press(&s, KeyCode::Char('n')).await;
        let first = marked(&s)[0].clone();
        let at = first.find("Thought line ").unwrap_or(0) + 13;
        let number: usize = first[at..]
            .split(' ')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{first}"));
        for i in 31..=40 {
            think_more(
                &s,
                &format!("Thought line {i} weighs one more part of the cache miss.\n"),
            )
            .await;
        }
        s.wait(200).await;
        press(&s, KeyCode::Char('n')).await;
        let want = format!("Thought line {} ", number + 1);
        assert!(
            s.screen
                .visible()
                .iter()
                .zip(0..)
                .any(|(row, y)| row.contains(&want)
                    && s.screen.inverse_rows().iter().any(|(r, _)| *r == y)),
            "{want}: {:#?}",
            s.screen.visible()
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn transcript_mode_and_help_search_too_and_keep_their_way_back_first() {
    staged(look(80, 24), |s| async move {
        long_thought_turn(&s, 20).await;
        s.open_transcript(100).await.wait(100).await;
        assert!(hint(&s).contains(&return_keys().text()), "{:?}", hint(&s));
        assert!(
            hint(&s).trim_start().starts_with(&search_key().text()),
            "{:?}",
            hint(&s)
        );
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "line 17").await;
        assert!(hint(&s).contains("1 of 1"), "{:?}", hint(&s));
        assert_eq!(marks(&s), vec!["line 17".to_owned()]);
        press(&s, KeyCode::Esc).await;
        press(&s, KeyCode::Char('q')).await;
        s.say("/help");
        s.wait(200).await;
        assert!(hint(&s).contains(&return_keys().text()), "{:?}", hint(&s));
        assert!(
            hint(&s).trim_start().starts_with(&search_key().text()),
            "{:?}",
            hint(&s)
        );
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "Search forward").await;
        assert!(hint(&s).contains("1 of 1"), "{:?}", hint(&s));
        assert_eq!(marks(&s), vec!["Search forward".to_owned()]);
        press(&s, KeyCode::Enter).await;
        press(&s, KeyCode::Char('?')).await;
        assert!(
            !hint(&s).contains(&return_keys().text()),
            "{:#?}",
            s.screen.visible()
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_long_transcript_scan_finishes_with_no_key_pressed() {
    staged(look(80, 24), |s| async move {
        long_thought_turn(&s, 400).await;
        open(&s, "Thought for").await;
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "weighs").await;
        s.wait(500).await;
        assert!(hint(&s).contains("1 of 400"), "{:?}", hint(&s));
        assert!(!hint(&s).contains('+'), "{:?}", hint(&s));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_page_closed_under_the_prompt_drops_the_rest_of_the_query_and_its_enter() {
    staged(look(80, 24), |s| async move {
        long_thought_turn(&s, 4).await;
        s.send("And then?").await;
        s.event(te::step(1)).await;
        s.think(0, &long_thought(10)).await;
        s.wait(2_000).await.stop(0).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            2_500,
        ))
        .await;
        s.wait(3000).await;
        s.paste("a draft");
        s.wait(100).await;
        s.input(Input::Key(Key::ctrl('o'))).wait(100).await;
        press(&s, KeyCode::Enter).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
        press(&s, KeyCode::Char('/')).await;
        typed(&s, "Tho").await;
        s.event(AgentEvent::Rewound {
            answers: None,
            point: RewindPoint::Turn { turn: 2 },
            reason: RewindReason::User,
            dropped: Dropped {
                turns: vec![2],
                messages: 2,
            },
            returned: Vec::new(),
            cache: CacheEffect::Kept,
        })
        .await;
        s.wait(50).await;
        assert!(!in_reader(&s), "{:#?}", s.screen.visible());
        for c in "ught".chars() {
            s.key(KeyCode::Char(c)).wait(100).await;
        }
        press(&s, KeyCode::Backspace).await;
        press(&s, KeyCode::Enter).await;
        let rows = s.screen.visible();
        assert!(rows.iter().any(|r| r.contains("a draft")), "{rows:#?}");
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("draftu") || r.contains("draftThoug")),
            "{rows:#?}"
        );
        s.wait(600).await;
        s.key(KeyCode::Char('x')).wait(100).await;
        let rows = s.screen.visible();
        assert!(rows.iter().any(|r| r.contains("a draftx")), "{rows:#?}");
    })
    .await;
}
