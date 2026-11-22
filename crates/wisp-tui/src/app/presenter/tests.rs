use wisp_protocol::ALREADY_COMPACTING;
use wisp_render::chrome::config_screen::TOO_SMALL;
use wisp_render::chrome::endpoint_card::NO_CARD;
use wisp_render::text::{WidthMode, width};
use wisp_render::theme::glyphs::UNICODE;
use wisp_render::transcript::copy;

use super::pager::page_clock;
use super::{AGAIN_TO_QUIT, AGENT_STOPPED, CONTEXT_COUNTS, CONTEXT_USAGE, NOTHING_TO_COPY};

const NOTICE_MAX: usize = 60;

#[test]
fn every_status_notice_fits_in_60_columns() {
    let fixed = [
        AGAIN_TO_QUIT,
        AGENT_STOPPED,
        CONTEXT_USAGE,
        CONTEXT_COUNTS,
        NOTHING_TO_COPY,
        ALREADY_COMPACTING,
        TOO_SMALL,
        NO_CARD,
        copy::PICKER_NO_ROOM,
        copy::NO_CLIPBOARD,
        copy::NO_CLIPBOARD_OVER_SSH,
        copy::NO_CLIPBOARD_IMAGE,
        copy::STILL_READING_CLIPBOARD,
        copy::CLIPBOARD_TOO_LARGE,
        copy::IMAGES_ELSEWHERE,
        copy::ALREADY_READ,
        copy::KEY_SHAPED_MODEL,
        copy::CARD_NO_PICKER,
        copy::COMPACT_BETWEEN_TURNS,
        copy::REWIND_BETWEEN_TURNS,
        copy::REWIND_AFTER_TURN,
        copy::REWIND_WHILE_COMPACTING,
    ];
    let built = [
        copy::too_late_edit(true),
        copy::too_late_edit(false),
        copy::unknown_command("hepl"),
        copy::unknown_effort("extreme"),
        copy::no_images_for("claude-sonnet-5-5"),
        copy::too_many_images(20),
        copy::resend_status(),
        copy::shutting_down(None, &UNICODE),
        copy::shutting_down(Some(copy::SAVING_SETTINGS), &UNICODE),
        copy::shutting_down(Some(copy::APPLYING_SETTINGS), &UNICODE),
        copy::shutting_down(Some(&copy::stopping_agents(13)), &UNICODE),
        copy::shutting_down(Some(&copy::stopping_processes(2)), &UNICODE),
    ];
    let all = fixed.iter().map(|s| (*s).to_owned()).chain(built);
    for notice in all {
        assert!(
            width(&notice, WidthMode::Normal) <= NOTICE_MAX,
            "{} columns: {notice}",
            width(&notice, WidthMode::Normal)
        );
    }
}

#[test]
fn a_page_clock_draws_its_scheduled_second_and_wakes_at_the_next_whole_second() {
    assert_eq!(
        page_clock(1_000_400, None, 1_000_400),
        (1_000_400, 1_001_000)
    );
    assert_eq!(
        page_clock(1_000_000, None, 1_000_000),
        (1_000_000, 1_001_000)
    );
    assert_eq!(
        page_clock(1_000_998, Some(1_001_000), 1_000_400),
        (1_001_000, 1_002_000)
    );
    assert_eq!(
        page_clock(1_001_003, Some(1_001_000), 1_000_400),
        (1_001_000, 1_002_000)
    );
    assert_eq!(
        page_clock(1_005_500, Some(1_001_000), 1_000_000),
        (1_005_000, 1_006_000)
    );
}

#[test]
fn a_frozen_clock_neither_moves_the_page_nor_reloads_it_faster_than_once_a_second() {
    for wall in [1_000_000u64, 1_000_400, 1_000_999] {
        let (mut drawn, mut next) = page_clock(wall, None, wall);
        let mut drawn_seen = vec![drawn];
        for _ in 0..6 {
            (drawn, next) = page_clock(wall, Some(next), drawn);
            drawn_seen.push(drawn);
            assert!(
                next - wall >= 500,
                "{wall}: reloads after {} ms",
                next - wall
            );
        }
        assert!(
            drawn_seen.windows(2).all(|w| w[0] <= w[1]),
            "{wall}: {drawn_seen:?}"
        );
        assert!(drawn <= (wall / 1000 + 1) * 1000, "{wall}: {drawn}");
    }
}

#[test]
fn hosted_shutdown_failures_keep_the_forced_code_and_waiting_save_note() {
    use super::{Exit, host_exit};
    use wisp_client::link::LinkEnd;
    for saving in [false, true] {
        for ended in [
            Some(LinkEnd::Ended { saved: false }),
            Some(LinkEnd::Lost),
            Some(LinkEnd::Cut),
            None,
        ] {
            assert_eq!(
                host_exit(ended, true, saving),
                Exit::Forced {
                    save_waiting: saving
                }
            );
        }
    }
    assert_eq!(
        host_exit(Some(LinkEnd::Ended { saved: true }), true, false),
        Exit::Quit
    );
    assert_eq!(host_exit(Some(LinkEnd::Lost), false, false), Exit::Lost);
    assert_eq!(host_exit(Some(LinkEnd::Cut), false, false), Exit::Cut);
    assert_eq!(
        host_exit(Some(LinkEnd::Restarting), false, false),
        Exit::Restarting
    );
}

#[test]
fn the_host_error_tail_keeps_only_the_last_five_clean_lines_in_order() {
    let scratch = crate::scratch::ScratchDir::new("wisp-tui-", "host-errors");
    let file = scratch.write("chat.err", "old one\nold two\n1\n2\n3\n4\n5\u{9b}31m\n");
    assert_eq!(super::host_errors(&file), ["1", "2", "3", "4", "5M-^[31m"]);
    assert!(super::host_errors(&scratch.root.join("missing")).is_empty());
}

#[test]
fn unsent_text_prints_pasted_text_and_image_labels() {
    let mut blank = super::Unsent::default();
    assert!(blank.texts().is_empty());
    blank.composer.insert("  \n\n\t");
    assert!(blank.texts().is_empty());
    let pasted = "error: placeholder failure\n  left: 0\n right: 1\n".repeat(4);
    for separator in ["", "\n", " "] {
        let mut unsent = super::Unsent::default();
        unsent.composer.insert("Why does this fail? ");
        unsent
            .composer
            .attach("image data".into(), |_| "[Image #1]".to_owned());
        unsent.composer.insert(separator);
        unsent.composer.paste(&pasted);
        let separator = if separator.is_empty() {
            "\n"
        } else {
            separator
        };
        unsent.children.push(super::Child {
            agent: wisp_protocol::AgentPath::new("/w"),
            unconfirmed: vec![(
                wisp_protocol::InputId("child-1".into()),
                super::Saved::from_text("sent to w"),
            )],
            texts: vec![super::Saved::from_text("draft for w")],
        });
        assert_eq!(
            unsent.texts(),
            [
                format!("Why does this fail? [Image #1]{separator}{pasted}"),
                "To /w:\nsent to w\n\ndraft for w".into()
            ]
        );
        unsent.composer = super::Composer::new();
        unsent.children[0].texts.clear();
        unsent.reopened(&["child-1".into()]);
        assert!(unsent.is_empty());
    }
}
