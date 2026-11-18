use wisp_render::text::width;
use wisp_render::theme::glyphs::GlyphSet;

use wisp_render::chrome::ComposerLook;
use wisp_render::markdown::Layout;

use super::*;
use crate::testing::{Rng, line_text, trimmed};
use wisp_render::transcript::{UserPart, copy};

fn layout(width: usize) -> ComposerShape {
    ComposerShape::open(Layout::new(width, GlyphSet::Ascii))
}

fn typed(c: &mut Composer, text: &str) {
    for ch in text.chars() {
        c.edit(Key::plain(KeyCode::Char(ch)));
    }
}

fn text_of(view: &ComposerView) -> Vec<String> {
    trimmed(&view.rows)
}

#[test]
fn typing_and_newlines_build_the_text() {
    let mut c = Composer::new();
    assert!(c.is_empty());
    typed(&mut c, "hello");
    c.newline();
    typed(&mut c, "world");
    assert_eq!(c.text(), "hello\nworld");
    assert!(!c.on_first_line());
    assert!(c.on_last_line());
    assert_eq!(c.take().text, "hello\nworld");
    assert!(c.is_empty());
}

#[test]
fn pasted_text_is_sanitised_and_keeps_its_lines() {
    let mut c = Composer::new();
    c.insert("a\r\nb\u{7}c\u{202E}");
    assert_eq!(c.text(), "a\nb^Gc<U+202E>");
}

#[test]
fn history_walks_back_and_returns_to_the_draft() {
    let mut c = Composer::new();
    typed(&mut c, "one");
    c.take();
    typed(&mut c, "two");
    c.take();
    typed(&mut c, "draft");
    assert!(c.older());
    assert_eq!(c.text(), "two");
    assert!(c.older());
    assert_eq!(c.text(), "one");
    assert!(!c.older());
    assert!(c.newer());
    assert_eq!(c.text(), "two");
    assert!(c.newer());
    assert_eq!(c.text(), "draft");
    assert!(!c.newer());
}

#[test]
fn the_view_wraps_long_lines_and_places_the_cursor() {
    let mut c = Composer::new();
    typed(&mut c, "abcdefghij");
    let view = c.view(layout(8));
    assert_eq!(text_of(&view), [" > abcd", "   efgh", "   ij"]);
    assert_eq!(view.cursor, (2, 5));
    c.edit(Key::plain(KeyCode::Left));
    let view = c.view(layout(8));
    assert_eq!(view.cursor, (2, 4));
    let tiny = c.view(layout(6));
    assert_eq!(text_of(&tiny), ["> abc", "  def", "  ghi", "  j"]);
    let empty = Composer::new().view(layout(40));
    assert_eq!(text_of(&empty), [format!(" > {}", copy::PLACEHOLDER)]);
    assert_eq!(empty.cursor, (0, 3));
}

#[test]
fn the_rows_wear_the_composer_band() {
    let look = ComposerLook::new();
    let mut c = Composer::new();
    let empty = c.view(layout(40));
    assert_eq!(empty.rows[0].spans[1].look, look.prompt);
    assert_eq!(empty.rows[0].spans[3].look, look.placeholder);
    c.newline();
    let blank = c.view(layout(40));
    assert_eq!(text_of(&blank), [" >", ""]);
    typed(&mut c, "hi");
    let view = c.view(layout(40));
    assert_eq!(text_of(&view), [" >", "   hi"]);
    for row in &view.rows {
        assert_eq!(
            row.spans
                .iter()
                .map(|s| wisp_render::text::width(&s.text, WidthMode::Normal))
                .sum::<usize>(),
            40
        );
        assert!(row.spans.iter().all(|s| s.look.bg == look.band.bg));
    }
    assert_eq!(view.rows[1].spans[1].look, look.text);
}

#[test]
fn a_tall_input_shows_the_rows_around_the_cursor() {
    let mut c = Composer::new();
    for i in 0..20 {
        if i > 0 {
            c.newline();
        }
        typed(&mut c, &format!("line {i}"));
    }
    let view = c.view(layout(40));
    assert_eq!(view.rows.len(), MAX_ROWS);
    assert_eq!(view.cursor.0, MAX_ROWS - 1);
    assert!(text_of(&view).last().is_some_and(|r| r.contains("line 19")));
}

#[test]
fn the_cursor_inside_a_grapheme_cluster_sits_on_that_cluster() {
    let mut c = Composer::new();
    c.insert("ab\u{1F469}\u{200D}\u{1F4BB}cd");
    for _ in 0..3 {
        c.edit(Key::plain(KeyCode::Left));
    }
    let view = c.view(layout(40));
    assert_eq!(view.cursor, (0, 5));
}

fn paste_of(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("line {i} with some words in it"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_big_paste_is_one_insert_and_frames_wrap_only_the_rows_shown() {
    let mut small = Composer::new();
    small.insert(&paste_of(400));
    small.view(layout(40));
    let mut big = Composer::new();
    let started = std::time::Instant::now();
    big.insert(&paste_of(20_000));
    let pasted = started.elapsed();
    assert_eq!(big.text().lines().count(), 20_000);
    assert!(pasted < std::time::Duration::from_secs(10), "{pasted:?}");
    big.view(layout(40));
    assert!(
        big.wrapped() <= 2 * small.wrapped(),
        "{} vs {}",
        big.wrapped(),
        small.wrapped()
    );
    let once = big.wrapped();
    for _ in 0..10 {
        big.view(layout(40));
    }
    assert_eq!(big.wrapped(), once, "an unchanged composer re-wrapped");
    big.edit(Key::plain(KeyCode::Up));
    big.view(layout(40));
    assert!(big.wrapped() - once < 4 * once);
}

fn lines_of(n: usize, sep: &str) -> String {
    (0..n)
        .map(|i| format!("pasted line {i}"))
        .collect::<Vec<_>>()
        .join(sep)
}

fn chip(n: u32) -> char {
    char::from_u32(0xE000 + n).unwrap()
}

fn press(c: &mut Composer, code: KeyCode) -> bool {
    c.edit(Key::plain(code))
}

fn ctrl(c: &mut Composer, ch: char) -> bool {
    c.edit(Key::ctrl(ch))
}

#[test]
fn a_paste_collapses_past_800_characters_or_8_lines() {
    let mut c = Composer::new();
    c.paste(&"x".repeat(800));
    assert_eq!(c.text(), "x".repeat(800));
    assert!(c.chips().is_empty());
    let mut c = Composer::new();
    c.paste(&"x".repeat(801));
    assert_eq!(c.text(), chip(1).to_string());
    assert_eq!(c.chips()[0].label, "[Pasted text #1 +801 characters]");
    let mut c = Composer::new();
    c.paste(&lines_of(8, "\n"));
    assert_eq!(c.text(), lines_of(8, "\n"));
    let mut c = Composer::new();
    c.paste(&lines_of(9, "\n"));
    assert_eq!(c.text(), chip(1).to_string());
    assert_eq!(c.chips()[0].label, "[Pasted text #1 +9 lines]");
    let mut c = Composer::new();
    c.paste(&format!("{}\n", lines_of(8, "\n")));
    assert!(c.chips().is_empty());
}

#[test]
fn crlf_counts_once_and_is_sent_as_lf() {
    let mut c = Composer::new();
    c.paste(&lines_of(8, "\r\n"));
    assert_eq!(c.text(), lines_of(8, "\n"));
    assert!(c.chips().is_empty());
    let mut c = Composer::new();
    c.paste(&format!("{}\r\n{}", "x".repeat(399), "y".repeat(400)));
    assert!(
        c.chips().is_empty(),
        "801 bytes with CRLF are 800 characters"
    );
    let mut c = Composer::new();
    c.paste(&lines_of(9, "\r\n"));
    assert_eq!(c.chips().len(), 1);
    assert_eq!(c.take().text, lines_of(9, "\n"));
}

#[test]
fn long_counts_are_grouped_in_labels() {
    let mut c = Composer::new();
    c.paste(&"y".repeat(4210));
    c.paste(&lines_of(1200, "\n"));
    let labels: Vec<&str> = c.chips().iter().map(|ch| ch.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "[Pasted text #1 +4,210 characters]",
            "[Pasted text #2 +1,200 lines]"
        ]
    );
}

#[test]
fn the_sent_text_is_the_full_expansion() {
    let big = lines_of(40, "\n");
    let mut c = Composer::new();
    typed(&mut c, "Review ");
    c.paste(&big);
    typed(&mut c, " please");
    let message = c.take();
    assert_eq!(message.text, format!("Review {big} please"));
    assert_eq!(
        message.parts,
        [
            UserPart::Text("Review ".to_owned()),
            UserPart::Pasted {
                label: "[Pasted text #1 +40 lines]".to_owned(),
                text: big.clone().into(),
            },
            UserPart::Text(" please".to_owned()),
        ]
    );
    assert!(c.is_empty());
    assert!(c.chips().is_empty());
}

#[test]
fn backspace_and_delete_remove_a_chip_whole_and_undo_brings_it_back() {
    let big = lines_of(20, "\n");
    let mut c = Composer::new();
    typed(&mut c, "a");
    c.paste(&big);
    typed(&mut c, "b");
    press(&mut c, KeyCode::Left);
    assert!(press(&mut c, KeyCode::Backspace));
    assert_eq!(c.text(), "ab");
    assert!(ctrl(&mut c, 'u'));
    assert_eq!(c.text(), format!("a{}b", chip(1)));
    assert_eq!(c.message().text, format!("a{big}b"));
    assert!(ctrl(&mut c, 'r'));
    assert_eq!(c.text(), "ab");
    assert!(ctrl(&mut c, 'u'));
    press(&mut c, KeyCode::Left);
    assert!(press(&mut c, KeyCode::Delete));
    assert_eq!(c.text(), "ab");
    assert!(ctrl(&mut c, 'u'));
    assert_eq!(c.message().text, format!("a{big}b"));
}

#[test]
fn the_cursor_steps_over_a_chip_as_one_character() {
    let mut c = Composer::new();
    typed(&mut c, "a");
    c.paste(&lines_of(20, "\n"));
    typed(&mut c, "b");
    press(&mut c, KeyCode::Left);
    press(&mut c, KeyCode::Left);
    typed(&mut c, "|");
    assert_eq!(c.text(), format!("a|{}b", chip(1)));
}

#[test]
fn a_yanked_chip_comes_back_intact() {
    let big = lines_of(20, "\n");
    let mut c = Composer::new();
    typed(&mut c, "a");
    c.paste(&big);
    typed(&mut c, "b");
    press(&mut c, KeyCode::Home);
    assert!(ctrl(&mut c, 'k'));
    assert!(c.is_empty());
    assert!(ctrl(&mut c, 'y'));
    assert!(ctrl(&mut c, 'y'));
    assert_eq!(c.message().text, format!("a{big}ba{big}b"));
}

#[test]
fn tab_after_a_chip_expands_it_as_one_undoable_edit() {
    let big = lines_of(12, "\n");
    let mut c = Composer::new();
    typed(&mut c, "x");
    c.paste(&big);
    assert!(press(&mut c, KeyCode::Tab));
    assert_eq!(c.text(), format!("x{big}"));
    assert_eq!(c.message().text, format!("x{big}"));
    assert!(ctrl(&mut c, 'u'));
    assert_eq!(c.text(), format!("x{}", chip(1)));
    assert!(ctrl(&mut c, 'u'));
    assert_eq!(c.text(), "x");
    assert!(ctrl(&mut c, 'r'));
    assert!(ctrl(&mut c, 'r'));
    assert_eq!(c.text(), format!("x{big}"));
    assert!(ctrl(&mut c, 'u'));
    assert_eq!(c.text(), format!("x{}", chip(1)));
    press(&mut c, KeyCode::Home);
    assert!(press(&mut c, KeyCode::Tab));
    assert_eq!(c.text(), format!("    x{}", chip(1)));
}

#[test]
fn the_undo_groups_survive_the_depth_limit() {
    let big = lines_of(12, "\n");
    let mut c = Composer::new();
    c.paste(&big);
    assert!(press(&mut c, KeyCode::Tab));
    for _ in 0..(UNDO_DEPTH - 1) {
        typed(&mut c, "z");
    }
    for _ in 0..(UNDO_DEPTH - 1) {
        assert!(ctrl(&mut c, 'u'));
    }
    assert_eq!(c.text(), big);
    assert!(ctrl(&mut c, 'u'));
    assert_eq!(c.text(), "");
    assert!(!ctrl(&mut c, 'u'));
    for _ in 0..UNDO_DEPTH {
        assert!(ctrl(&mut c, 'r'));
    }
    assert_eq!(c.text(), format!("{big}{}", "z".repeat(UNDO_DEPTH - 1)));
    assert!(!ctrl(&mut c, 'r'));
}

#[test]
fn private_use_characters_typed_or_pasted_become_replacement_characters() {
    let mut c = Composer::new();
    typed(&mut c, "\u{E001}");
    c.paste("\u{E000}\u{F8FF}\u{F0001}\u{10FFFD}");
    assert_eq!(
        c.text(),
        format!("{}\u{F0001}\u{10FFFD}", "\u{FFFD}".repeat(3))
    );
    assert!(c.chips().is_empty());
    let mut big = lines_of(10, "\n");
    big.push('\u{E001}');
    c.paste(&big);
    let ChipBody::Text(stored) = &c.chips()[0].body else {
        panic!("not a text chip");
    };
    assert!(stored.ends_with('\u{FFFD}'));
    assert!(press(&mut c, KeyCode::Tab));
    assert_eq!(c.chips().len(), 1);
    assert!(!c.message().text.chars().any(is_private_use));
}

#[test]
fn the_same_paste_reuses_its_number_and_numbers_restart_after_a_send() {
    let one = lines_of(10, "\n");
    let two = lines_of(11, "\n");
    let mut c = Composer::new();
    c.paste(&one);
    c.paste(&two);
    c.paste(&one);
    assert_eq!(c.text(), format!("{}{}{}", chip(1), chip(2), chip(1)));
    assert_eq!(c.chips().len(), 2);
    assert_eq!(c.take().text, format!("{one}{two}{one}"));
    c.paste(&two);
    assert_eq!(c.text(), chip(1).to_string());
    assert_eq!(c.chips()[0].label, "[Pasted text #1 +11 lines]");
}

#[test]
fn history_recalls_entries_with_their_chips() {
    let one = lines_of(10, "\n");
    let two = lines_of(30, "\n");
    let mut c = Composer::new();
    typed(&mut c, "first ");
    c.paste(&one);
    c.take();
    typed(&mut c, "draft ");
    c.paste(&two);
    assert!(c.older());
    assert_eq!(c.message().text, format!("first {one}"));
    assert_eq!(c.chips()[0].label, "[Pasted text #1 +10 lines]");
    assert!(c.newer());
    assert_eq!(c.message().text, format!("draft {two}"));
    assert_eq!(c.chips()[0].label, "[Pasted text #1 +30 lines]");
    assert!(c.older());
    c.paste(&two);
    assert_eq!(c.chips()[1].label, "[Pasted text #2 +30 lines]");
    assert_eq!(c.take().text, format!("first {one}{two}"));
}

#[test]
fn a_chip_renders_as_its_label_in_the_chip_look_and_wraps_whole() {
    let look = ComposerLook::new();
    let mut c = Composer::new();
    typed(&mut c, "Review ");
    c.paste(&lines_of(312, "\n"));
    let view = c.view(layout(80));
    assert_eq!(text_of(&view), [" > Review [Pasted text #1 +312 lines]"]);
    let span = view.rows[0]
        .spans
        .iter()
        .find(|s| s.text == "[Pasted text #1 +312 lines]")
        .unwrap();
    assert_eq!(span.look, look.chip);
    assert_eq!(view.cursor, (0, 3 + 7 + 27));
    let narrow = c.view(layout(30));
    assert_eq!(
        text_of(&narrow),
        [" > Review", "   [Pasted text #1 +312 li..."]
    );
    assert_eq!(narrow.cursor, (1, 3 + 26));
}

#[test]
fn escapes_in_a_paste_never_reach_the_screen_or_the_label() {
    let evil = format!(
        "]0;pwn\u{7}\u{9b}2J\u{90}[31m{}\u{202e}",
        lines_of(20, "\n")
    );
    let mut c = Composer::new();
    c.paste(&evil);
    let view = c.view(layout(80));
    for row in &view.rows {
        for span in &row.spans {
            assert!(!span.text.chars().any(|ch| ch.is_control()), "{span:?}");
        }
    }
    let message = c.take();
    assert!(!message.text.chars().any(|ch| ch.is_control() && ch != '\n'));
    assert!(!message.text.contains('\u{202e}'));
    assert!(message.text.starts_with("]0;pwn^GM-^[2JM-^P[31m"));
    c.paste(&evil);
    press(&mut c, KeyCode::Tab);
    let expanded = c.view(layout(80));
    for row in &expanded.rows {
        for span in &row.spans {
            assert!(!span.text.chars().any(|ch| ch.is_control()), "{span:?}");
        }
    }
}

fn typing_work(chip_lines: usize) -> (usize, usize) {
    let mut c = Composer::new();
    typed(&mut c, "look at ");
    c.paste(&paste_of(chip_lines));
    c.view(layout(60));
    let before = c.wrapped();
    for ch in "this and tell me what you think".chars() {
        c.edit(Key::plain(KeyCode::Char(ch)));
        c.view(layout(60));
    }
    c.edit(Key::plain(KeyCode::Backspace));
    c.view(layout(60));
    c.edit(Key::plain(KeyCode::Left));
    c.view(layout(60));
    (c.wrapped() - before, c.text().len())
}

#[test]
fn the_cost_of_a_keystroke_does_not_grow_with_the_chip() {
    let (small, small_len) = typing_work(20);
    let (huge, huge_len) = typing_work(200_000);
    assert_eq!(small, huge);
    assert_eq!(small_len, huge_len);
}

#[test]
fn undo_stays_in_step_after_undone_edits_meet_the_depth_limit() {
    let big = lines_of(12, "\n");
    let mut c = Composer::new();
    for _ in 0..UNDO_DEPTH {
        typed(&mut c, "a");
    }
    for _ in 0..3 {
        assert!(ctrl(&mut c, 'u'));
    }
    c.paste(&big);
    assert!(press(&mut c, KeyCode::Tab));
    typed(&mut c, "b");
    assert!(ctrl(&mut c, 'u'));
    assert!(ctrl(&mut c, 'u'));
    let kept = UNDO_DEPTH - 3;
    assert_eq!(c.text(), format!("{}{}", "a".repeat(kept), chip(1)));
    assert!(ctrl(&mut c, 'u'));
    assert_eq!(c.text(), "a".repeat(kept));
    for _ in 1..kept {
        assert!(ctrl(&mut c, 'u'));
    }
    assert_eq!(c.text(), "a");
    assert!(!ctrl(&mut c, 'u'));
}

#[derive(Clone, Debug)]
struct ModelEdit {
    step: u64,
    before: String,
    after: String,
}

#[derive(Default)]
struct Model {
    edits: std::collections::VecDeque<ModelEdit>,
    index: usize,
    step: u64,
}

impl Model {
    fn push(&mut self, before: String, after: String) {
        if self.edits.len() == UNDO_DEPTH {
            self.edits.pop_front();
            self.index = self.index.saturating_sub(1);
        }
        self.edits.truncate(self.index);
        self.edits.push_back(ModelEdit {
            step: self.step,
            before,
            after,
        });
        self.index += 1;
    }
    fn step(&mut self) {
        self.step += 1;
    }
    fn undo(&mut self) -> Option<String> {
        if self.index == 0 {
            return None;
        }
        let step = self.edits[self.index - 1].step;
        let mut text = None;
        while self.index > 0 && self.edits[self.index - 1].step == step {
            self.index -= 1;
            text = Some(self.edits[self.index].before.clone());
        }
        text
    }
    fn redo(&mut self) -> Option<String> {
        if self.index == self.edits.len() {
            return None;
        }
        let step = self.edits[self.index].step;
        let mut text = None;
        while self.index < self.edits.len() && self.edits[self.index].step == step {
            text = Some(self.edits[self.index].after.clone());
            self.index += 1;
        }
        text
    }
}

fn expand(c: &Composer) -> String {
    let mut out = String::new();
    for ch in c.text().chars() {
        let code = u32::from(ch);
        if (0xE001..=0xF8FF).contains(&code) {
            let ChipBody::Text(t) = &c.chips()[(code - 0xE001) as usize].body else {
                panic!("not a text chip");
            };
            out.push_str(t);
        } else {
            out.push(ch);
        }
    }
    out
}

fn fuzz_run(seed: u64, actions: usize) {
    let bigs: Vec<String> = (0..3).map(|i| lines_of(9 + i, "\n")).collect();
    let mut r = Rng(seed);
    let mut c = Composer::new();
    let mut m = Model::default();
    let mut sent: Vec<(String, String)> = vec![];
    for n in 0..actions {
        let before = c.text();
        let a = r.below(22);
        let mut expanded = false;
        let mut mid = String::new();
        let mut undo_redo = None;
        match a {
            0..=3 => {
                let ch = ['a', 'b', ' ', 'é'][r.below(4)];
                c.edit(Key::plain(KeyCode::Char(ch)));
            }
            4 => c.newline(),
            5 => c.paste(&bigs[r.below(3)]),
            6 => c.paste(
                [
                    "xy",
                    "p\nq",
                    "\u{E001}",
                    "z\u{E002}\nw",
                    "\n\n\n\n\n\n\n\n\n\n",
                    "  \t ",
                ][r.below(6)],
            ),
            7 => {
                c.edit(Key::plain(KeyCode::Backspace));
            }
            8 => {
                c.edit(Key::plain(KeyCode::Delete));
            }
            9 => {
                c.edit(Key::plain(
                    [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End][r.below(4)],
                ));
            }
            10 => {
                c.edit(Key::plain([KeyCode::Up, KeyCode::Down][r.below(2)]));
            }
            11 => {
                c.edit(Key::ctrl(
                    [
                        'k', 'j', 'w', 'y', 'y', 'x', 'c', 'h', 'd', 'e', 'a', 'b', 'f', 'm',
                    ][r.below(14)],
                ));
            }
            12 => {
                c.edit(Key::alt(
                    [
                        KeyCode::Char('d'),
                        KeyCode::Backspace,
                        KeyCode::Char('f'),
                        KeyCode::Char('b'),
                        KeyCode::Delete,
                        KeyCode::Char('h'),
                    ][r.below(6)],
                ));
            }
            13 => {
                let DataCursor(row, col) = c.area.cursor();
                let had_chip = col > 0
                    && c.area.lines()[row]
                        .chars()
                        .nth(col - 1)
                        .is_some_and(|ch| (0xE001..=0xF8FF).contains(&u32::from(ch)));
                if had_chip {
                    let mut lines: Vec<String> = c.area.lines().to_vec();
                    let l: Vec<char> = lines[row].chars().collect();
                    lines[row] = l[..col - 1].iter().chain(l[col..].iter()).collect();
                    mid = lines.join("\n");
                }
                c.edit(Key::plain(KeyCode::Tab));
                expanded = had_chip;
            }
            14..=16 => {
                let ok = c.undo();
                undo_redo = Some((ok, m.undo()));
            }
            17..=18 => {
                let ok = c.redo();
                undo_redo = Some((ok, m.redo()));
            }
            19 => {
                c.edit(Key {
                    code: [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::End][r.below(4)],
                    ctrl: false,
                    alt: false,
                    shift: true,
                });
            }
            20 => {
                if r.below(10) == 0 {
                    let want = expand(&c);
                    let msg = c.take();
                    assert_eq!(msg.text, want, "seed {seed} action {n}");
                    sent.push((want, msg.text));
                    m = Model::default();
                    continue;
                }
            }
            _ => {
                let moved = if r.below(2) == 0 {
                    c.older()
                } else {
                    c.newer()
                };
                if moved {
                    m = Model::default();
                }
                assert_eq!(c.message().text, expand(&c));
                continue;
            }
        }
        let after = c.text();
        match undo_redo {
            Some((ok, want)) => {
                assert_eq!(
                    ok,
                    want.is_some(),
                    "seed {seed} action {n}: undo/redo availability"
                );
                if let Some(want) = want {
                    assert_eq!(after, want, "seed {seed} action {n}: undo/redo text");
                }
            }
            None => {
                if before != after {
                    m.step();
                    if expanded {
                        m.push(before.clone(), mid.clone());
                        m.push(mid.clone(), after.clone());
                    } else {
                        m.push(before, after.clone());
                    }
                }
            }
        }
        assert_eq!(c.message().text, expand(&c), "seed {seed} action {n}");
        assert!(!c.message().text.chars().any(is_private_use));
        assert!(
            c.text()
                .chars()
                .filter(|ch| is_private_use(*ch))
                .all(|ch| c.chips.get(ch).is_some())
        );
        let _ = c.view(layout(1 + (n % 50)));
    }
}

#[test]
fn undo_and_redo_agree_with_a_model_of_the_textarea_history() {
    for seed in 1..=40u64 {
        fuzz_run(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1, 400);
    }
}

#[test]
fn a_paste_with_undone_edits_at_the_depth_limit_keeps_undo_in_step() {
    let mut c = Composer::new();
    for _ in 0..UNDO_DEPTH {
        typed(&mut c, "a");
    }
    for _ in 0..3 {
        assert!(ctrl(&mut c, 'u'));
    }
    c.paste(&lines_of(12, "\n"));
    let mut n = 0;
    while ctrl(&mut c, 'u') {
        n += 1;
        assert!(n <= UNDO_DEPTH);
    }
    assert_eq!(n, UNDO_DEPTH - 3);
    assert_eq!(c.text(), "a");
}

#[test]
fn a_whitespace_only_paste_stays_inline_and_blank() {
    for paste in ["\n".repeat(9), " ".repeat(801), " \t\n".repeat(400)] {
        let mut c = Composer::new();
        c.paste(&paste);
        assert!(c.chips().is_empty(), "{paste:?}");
        assert!(c.is_blank(), "{paste:?}");
    }
}

#[test]
fn the_view_follows_undo_and_redo() {
    let mut c = Composer::new();
    typed(&mut c, "ab");
    c.paste(&lines_of(12, "\n"));
    let pasted = text_of(&c.view(layout(60)));
    assert!(ctrl(&mut c, 'u'));
    let undone = text_of(&c.view(layout(60)));
    assert_eq!(undone, [" > ab"]);
    assert!(ctrl(&mut c, 'r'));
    assert_eq!(text_of(&c.view(layout(60))), pasted);
}

#[test]
fn a_combining_mark_after_a_chip_keeps_the_label() {
    let mut c = Composer::new();
    c.paste(&lines_of(12, "\n"));
    c.paste("\u{301}");
    assert_eq!(c.text(), format!("{}\u{301}", chip(1)));
    let view = text_of(&c.view(layout(60)));
    assert_eq!(view.len(), 1);
    assert!(
        view[0].starts_with(" > [Pasted text #1 +12 lines]"),
        "{view:?}"
    );
    assert!(c.message().text.ends_with("pasted line 11\u{301}"));
}

#[test]
fn history_shares_chip_bodies_and_drops_deleted_ones() {
    let kept = lines_of(12, "\n");
    let gone = lines_of(13, "\n");
    let mut c = Composer::new();
    c.paste(&gone);
    press(&mut c, KeyCode::Backspace);
    c.paste(&kept);
    let message = c.take();
    let UserPart::Pasted { text: sent, .. } = &message.parts[0] else {
        panic!("{message:?}");
    };
    assert!(c.older());
    assert_eq!(c.chips().len(), 2);
    assert_eq!(c.chips()[0].body, ChipBody::Text("".into()));
    assert_eq!(c.chips()[0].label, "[Pasted text #1 +13 lines]");
    let ChipBody::Text(recalled) = &c.chips()[1].body else {
        panic!("not a text chip");
    };
    assert!(Arc::ptr_eq(recalled, sent));
    assert_eq!(c.message().text, kept);
    c.paste(&gone);
    assert_eq!(c.chips()[2].label, "[Pasted text #3 +13 lines]");
}

fn image_label(n: usize) -> String {
    format!("[Image #{n}]")
}

#[test]
fn image_chips_send_their_data_in_chip_order_and_leave_the_text_alone() {
    let mut c = Composer::new();
    assert!(c.attach(Arc::from("AAAA"), image_label));
    typed(&mut c, " compare with ");
    assert!(c.attach(Arc::from("BBBB"), image_label));
    c.paste(&lines_of(12, "\n"));
    assert_eq!(c.image_count(), 2);
    assert_eq!(c.chips()[0].label, "[Image #1]");
    assert_eq!(c.chips()[1].label, "[Image #2]");
    assert_eq!(c.chips()[2].label, "[Pasted text #1 +12 lines]");
    let message = c.take();
    assert_eq!(
        message.images,
        vec![Arc::<str>::from("AAAA"), Arc::<str>::from("BBBB")]
    );
    assert_eq!(
        message.text,
        format!(" compare with {}", lines_of(12, "\n"))
    );
    assert_eq!(
        message.parts[..3],
        [
            UserPart::Image {
                label: "[Image #1]".to_owned()
            },
            UserPart::Text(" compare with ".to_owned()),
            UserPart::Image {
                label: "[Image #2]".to_owned()
            },
        ]
    );
    assert_eq!(c.image_count(), 0);
}

#[test]
fn moving_a_chip_changes_the_order_and_a_copy_is_sent_once() {
    let mut c = Composer::new();
    c.attach(Arc::from("first"), image_label);
    c.attach(Arc::from("second"), image_label);
    press(&mut c, KeyCode::Left);
    press(&mut c, KeyCode::Left);
    press(&mut c, KeyCode::Delete);
    press(&mut c, KeyCode::End);
    ctrl(&mut c, 'u');
    ctrl(&mut c, 'r');
    let text = c.text();
    assert_eq!(text, format!("{}", chip(2)));
    c.insert(&format!("{}", chip(1)));
    let message = c.message();
    assert_eq!(
        message.images,
        vec![Arc::<str>::from("second")],
        "a typed private-use character is not a chip"
    );
    let mut c = Composer::new();
    c.attach(Arc::from("one"), image_label);
    press(&mut c, KeyCode::Home);
    ctrl(&mut c, 'k');
    ctrl(&mut c, 'y');
    ctrl(&mut c, 'y');
    assert_eq!(c.text(), format!("{}{}", chip(1), chip(1)));
    assert_eq!(c.message().images, vec![Arc::<str>::from("one")]);
    assert_eq!(c.image_count(), 1);
}

#[test]
fn an_image_chip_alone_is_a_message_and_is_removed_whole() {
    let mut c = Composer::new();
    c.attach(Arc::from("data"), image_label);
    assert!(!c.is_blank());
    assert!(!c.is_empty());
    let view = c.view(layout(40));
    assert_eq!(text_of(&view)[0], " > [Image #1]");
    assert!(press(&mut c, KeyCode::Backspace));
    assert!(c.is_empty());
    assert_eq!(c.image_count(), 0);
    assert!(c.undo());
    assert_eq!(c.image_count(), 1);
}

#[test]
fn history_recalls_image_chips_and_forgets_unused_ones() {
    let mut c = Composer::new();
    c.attach(Arc::from("kept"), image_label);
    c.attach(Arc::from("dropped"), image_label);
    press(&mut c, KeyCode::Backspace);
    let sent = c.take();
    assert_eq!(sent.images, vec![Arc::<str>::from("kept")]);
    assert!(c.older());
    assert_eq!(c.message().images, vec![Arc::<str>::from("kept")]);
    assert_eq!(c.chips()[1].body, ChipBody::Image(Arc::from("")));
    c.attach(Arc::from("new"), image_label);
    assert_eq!(c.chips()[2].label, "[Image #3]");
}

#[test]
fn tab_queues_only_at_the_end_of_text_with_no_chip_before_the_cursor() {
    let mut c = Composer::new();
    assert!(!c.tab_queues());
    typed(&mut c, "steer me");
    assert!(c.tab_queues());
    press(&mut c, KeyCode::Left);
    assert!(!c.tab_queues(), "mid-text");
    press(&mut c, KeyCode::End);
    c.newline();
    assert!(!c.tab_queues(), "a blank last line");
    typed(&mut c, "  ");
    assert!(!c.tab_queues(), "a whitespace-only last line");
    c.clear();
    typed(&mut c, "x");
    c.paste(&lines_of(10, "\n"));
    assert!(!c.tab_queues(), "a chip just before the cursor");
    typed(&mut c, "y");
    assert!(c.tab_queues());
    c.newline();
    typed(&mut c, "z");
    press(&mut c, KeyCode::Up);
    assert!(!c.tab_queues(), "not on the last line");
}

#[test]
fn returned_messages_go_ahead_of_the_draft_with_their_chips_renumbered() {
    let mut c = Composer::new();
    c.paste(&lines_of(10, "\n"));
    typed(&mut c, " first");
    let (_, one) = c.take_saved();
    c.paste(&lines_of(20, "\n"));
    let (_, two) = c.take_saved();
    c.paste(&lines_of(30, "\n"));
    typed(&mut c, " draft");
    assert_eq!(c.chips()[0].label, "[Pasted text #1 +30 lines]");
    c.restore_ahead(&[one, two]);
    assert_eq!(
        c.text(),
        format!("{} first\n\n{}\n\n{} draft", chip(2), chip(3), chip(1))
    );
    let labels: Vec<&str> = c.chips().iter().map(|ch| ch.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "[Pasted text #1 +30 lines]",
            "[Pasted text #2 +10 lines]",
            "[Pasted text #3 +20 lines]",
        ]
    );
    let sent = c.take().text;
    assert_eq!(
        sent,
        format!(
            "{} first\n\n{}\n\n{} draft",
            lines_of(10, "\n"),
            lines_of(20, "\n"),
            lines_of(30, "\n")
        )
    );
}

#[test]
fn a_late_copy_is_dropped_only_while_it_is_what_was_put_back() {
    for (pasted, browsing) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut c = Composer::new();
        c.insert("waiting message");
        if pasted {
            c.insert("\n\n");
            c.paste(&lines_of(10, "\n"));
        }
        let (_, saved) = c.take_saved();
        if pasted {
            c.paste(&lines_of(9, "\n"));
            c.insert(" ");
        }
        c.insert("draft 🌿 with waiting message later");
        let mut draft = c.text();
        let piece = c.restore_ahead(&[saved]).remove(0);
        if pasted {
            c.restore_ahead(&[Saved::from_text("returned first")]);
            draft = format!("returned first\n\n{draft}");
        }
        let restored = c.text();
        if !browsing {
            c.edit(Key::plain(KeyCode::Left));
        }
        let cursor = c.cursor();
        if browsing {
            assert!(c.older());
            assert!(c.is_browsing());
        }
        assert!(c.drop_ahead(&piece));
        assert!(!c.is_browsing());
        assert_eq!(c.text(), draft);
        assert_eq!(c.cursor(), (cursor.0 - piece.lines().count() - 1, cursor.1));
        assert!(!c.drop_ahead(&piece));
        assert!(c.undo());
        assert_eq!(c.text(), restored);
        c.area.move_cursor(CursorMove::Top);
        c.area.move_cursor(CursorMove::Head);
        if pasted {
            c.area.move_cursor(CursorMove::Down);
            c.area.move_cursor(CursorMove::Down);
        }
        c.insert("edited ");
        let edited = c.text();
        assert!(!c.drop_ahead(&piece));
        assert_eq!(c.text(), edited);
    }
    let mut c = Composer::new();
    c.insert("same words later in the draft");
    assert!(!c.drop_ahead("same words"));
    assert_eq!(c.text(), "same words later in the draft");
}

#[test]
fn a_returned_message_into_an_empty_composer_is_the_whole_text_with_the_cursor_at_its_end() {
    let mut c = Composer::new();
    typed(&mut c, "unsent steer");
    let (_, saved) = c.take_saved();
    assert_eq!(shown(&saved.preview()), "unsent steer");
    c.restore_ahead(&[saved]);
    assert_eq!(c.text(), "unsent steer");
    assert!(c.tab_queues());
    c.restore_ahead(&[]);
    assert_eq!(c.text(), "unsent steer");
}

#[test]
fn a_preview_is_the_first_non_blank_line_with_chips_as_their_labels() {
    let mut c = Composer::new();
    c.newline();
    typed(&mut c, "  see ");
    c.paste(&lines_of(10, "\n"));
    typed(&mut c, " here");
    c.newline();
    typed(&mut c, "second line");
    let (_, saved) = c.take_saved();
    assert_eq!(
        shown(&saved.preview()),
        "see [Pasted text #1 +10 lines] here"
    );
    assert!(!shown(&Saved::from_text("\n\nother \u{9b}line").preview()).contains('\u{9b}'));
}

#[test]
fn a_returned_message_while_browsing_history_keeps_the_real_draft() {
    let mut c = Composer::new();
    typed(&mut c, "an old message");
    let _ = c.take_saved();
    typed(&mut c, "the steer");
    let (_, steer) = c.take_saved();
    typed(&mut c, "my draft");
    assert!(c.older());
    assert_eq!(c.text(), "the steer");
    assert!(c.older());
    assert_eq!(c.text(), "an old message");
    c.restore_ahead(&[steer]);
    assert_eq!(c.text(), "the steer\n\nmy draft");
    assert!(!c.text().contains("an old message"));
    assert!(!c.newer(), "browsing ended");
    assert!(c.older());
    assert_eq!(c.text(), "the steer");
    assert!(c.newer());
    assert_eq!(c.text(), "the steer\n\nmy draft");
}

#[test]
fn one_undo_takes_a_return_back_out_and_redo_puts_it_back() {
    let mut c = Composer::new();
    c.paste(&lines_of(10, "\n"));
    let (_, saved) = c.take_saved();
    typed(&mut c, "draft");
    c.restore_ahead(&[saved]);
    let restored = c.text();
    assert_eq!(restored, format!("{}\n\ndraft", chip(1)));
    assert_eq!(c.area.cursor(), DataCursor(2, 5));
    assert!(c.undo());
    assert_eq!(c.text(), "draft");
    assert!(c.undo());
    assert_eq!(c.text(), "draf");
    assert!(c.redo());
    assert!(c.redo());
    assert_eq!(c.text(), restored);
    assert_eq!(c.take().text, format!("{}\n\ndraft", lines_of(10, "\n")));
}

#[test]
fn returned_image_chips_come_back_in_send_order_renumbered_after_the_drafts_own() {
    let mut c = Composer::new();
    c.attach(Arc::from("A"), image_label);
    typed(&mut c, " one ");
    c.attach(Arc::from("B"), image_label);
    let (_, first) = c.take_saved();
    c.paste(&lines_of(10, "\n"));
    c.attach(Arc::from("C"), image_label);
    let (_, second) = c.take_saved();
    c.attach(Arc::from("D"), image_label);
    typed(&mut c, " draft");
    c.restore_ahead(&[first, second]);
    assert_eq!(
        c.text(),
        format!(
            "{} one {}\n\n{}{}\n\n{} draft",
            chip(2),
            chip(3),
            chip(4),
            chip(5),
            chip(1)
        )
    );
    let labels: Vec<&str> = c.chips().iter().map(|ch| ch.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "[Image #1]",
            "[Image #2]",
            "[Image #3]",
            "[Pasted text #1 +10 lines]",
            "[Image #4]",
        ]
    );
    assert_eq!(c.image_count(), 4);
    let sent = c.take();
    let order: Vec<&str> = sent.images.iter().map(|d| &**d).collect();
    assert_eq!(order, ["A", "B", "C", "D"]);
}

#[test]
fn a_chip_used_twice_comes_back_as_one_chip_and_a_real_label_keeps_its_details() {
    let mut c = Composer::new();
    let real = |n: usize| format!("[Image #{n} \u{b7} 56\u{d7}28 \u{b7} \u{2248}2 tokens]");
    c.attach(Arc::from("A"), real);
    press(&mut c, KeyCode::Home);
    ctrl(&mut c, 'k');
    ctrl(&mut c, 'y');
    ctrl(&mut c, 'y');
    let (_, saved) = c.take_saved();
    c.attach(Arc::from("draft"), real);
    c.restore_ahead(&[saved]);
    assert_eq!(c.text(), format!("{}{}\n\n{}", chip(2), chip(2), chip(1)));
    assert_eq!(
        c.chips()[1].label,
        "[Image #2 \u{b7} 56\u{d7}28 \u{b7} \u{2248}2 tokens]"
    );
    assert_eq!(c.message().images.len(), 2);
}

#[test]
fn a_preview_leaves_images_to_their_labels() {
    let mut c = Composer::new();
    c.attach(Arc::from("A"), image_label);
    c.newline();
    typed(&mut c, "look ");
    c.attach(Arc::from("B"), image_label);
    typed(&mut c, " here");
    let (_, saved) = c.take_saved();
    assert_eq!(saved.image_labels(), ["[Image #1]", "[Image #2]"]);
    assert_eq!(shown(&saved.preview()), "look  here");
}

#[test]
fn an_edit_sets_the_draft_aside_whole_and_puts_it_back_unchanged() {
    let mut c = Composer::new();
    c.paste(&lines_of(10, "\n"));
    typed(&mut c, " waiting");
    let (_, waiting) = c.take_saved();
    c.paste(&lines_of(20, "\n"));
    typed(&mut c, " draft");
    press(&mut c, KeyCode::Left);
    let before = (c.text(), c.chips().to_vec());
    let draft = c.set_aside(&waiting);
    assert_eq!(c.text(), format!("{} waiting", chip(1)));
    assert_eq!(c.chips()[0].label, "[Pasted text #1 +10 lines]");
    assert!(c.on_last_line());
    typed(&mut c, " more");
    assert_eq!(
        c.message().text,
        format!("{} waiting more", lines_of(10, "\n"))
    );
    c.put_back(draft);
    assert_eq!((c.text(), c.chips().to_vec()), before);
    assert_eq!(c.take().text, format!("{} draft", lines_of(20, "\n")));
}

#[test]
fn setting_aside_while_browsing_keeps_the_real_draft_not_the_shown_entry() {
    let mut c = Composer::new();
    typed(&mut c, "older");
    let (_, older) = c.take_saved();
    typed(&mut c, "my draft");
    assert!(c.older());
    assert!(c.is_browsing());
    let draft = c.set_aside(&older);
    assert!(!c.is_browsing());
    assert_eq!(draft.text(), "my draft");
}

#[test]
fn an_ended_edit_keeps_its_text_and_cursor_and_the_draft_follows_renumbered() {
    let mut c = Composer::new();
    c.paste(&lines_of(10, "\n"));
    typed(&mut c, " edited");
    let (_, edit) = c.take_saved();
    c.paste(&lines_of(20, "\n"));
    typed(&mut c, " draft");
    let draft = c.set_aside(&edit);
    press(&mut c, KeyCode::Left);
    let cursor = c.area.cursor();
    c.append_behind(&draft);
    assert_eq!(c.text(), format!("{} edited\n\n{} draft", chip(1), chip(2)));
    assert_eq!(c.area.cursor(), cursor);
    let labels: Vec<&str> = c.chips().iter().map(|ch| ch.label.as_str()).collect();
    assert_eq!(
        labels,
        ["[Pasted text #1 +10 lines]", "[Pasted text #2 +20 lines]"]
    );
    assert_eq!(
        c.take().text,
        format!(
            "{} edited\n\n{} draft",
            lines_of(10, "\n"),
            lines_of(20, "\n")
        )
    );
    let mut browsing = Composer::new();
    browsing.insert("sent before");
    browsing.take();
    browsing.insert("unsent draft");
    assert!(browsing.older());
    browsing.append_behind(&Saved::from_text("edit draft"));
    assert!(!browsing.is_browsing());
    assert_eq!(browsing.text(), "unsent draft\n\nedit draft");
    let mut empty = Composer::new();
    typed(&mut empty, "alone");
    empty.append_behind(&Saved::default());
    assert_eq!(empty.text(), "alone");
    let mut blank = Composer::new();
    blank.append_behind(&Saved::from_text("draft"));
    assert_eq!(blank.text(), "draft");
}

#[test]
fn a_saved_entry_rebuilds_its_message_and_takes_returns_ahead() {
    let mut c = Composer::new();
    c.attach(Arc::from("A"), image_label);
    typed(&mut c, " look ");
    c.paste(&lines_of(10, "\n"));
    let (message, saved) = c.take_saved();
    assert_eq!(saved.message(), message);
    assert_eq!(saved.image_count(), 1);
    c.paste(&lines_of(20, "\n"));
    let (_, returned) = c.take_saved();
    typed(&mut c, "x");
    let (_, draft) = c.take_saved();
    let joined = draft.with_ahead(&[saved, returned]);
    let mut shown = Composer::new();
    shown.put_back(joined);
    assert_eq!(
        shown.text(),
        format!("{} look {}\n\n{}\n\nx", chip(1), chip(2), chip(3))
    );
    let labels: Vec<&str> = shown.chips().iter().map(|ch| ch.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "[Image #1]",
            "[Pasted text #1 +10 lines]",
            "[Pasted text #2 +20 lines]"
        ]
    );
    assert_eq!(
        Saved::from_text("same").with_ahead(&[]),
        Saved::from_text("same")
    );
}

#[test]
fn a_removed_message_becomes_the_newest_history_entry_once() {
    let mut c = Composer::new();
    typed(&mut c, "removed");
    let (_, removed) = c.take_saved();
    typed(&mut c, "later");
    let _ = c.take_saved();
    c.remember(&removed);
    assert!(c.older());
    assert_eq!(c.text(), "removed");
    assert!(c.older());
    assert_eq!(c.text(), "later");
    assert!(!c.older(), "the removed message is in history once");
    c.remember(&Saved::from_text("  "));
    assert!(c.newer());
    assert!(c.newer());
    assert!(!c.newer());
}

#[test]
fn a_draft_without_its_second_image_keeps_the_others_and_its_pastes() {
    let mut c = Composer::new();
    c.attach(Arc::from("A"), image_label);
    c.attach(Arc::from("B"), image_label);
    c.attach(Arc::from("C"), image_label);
    c.paste(&lines_of(10, "\n"));
    typed(&mut c, " compare");
    let (_, saved) = c.take_saved();
    c.restore_ahead(&[saved.without_images(&[2])]);
    let labels: Vec<&str> = c.chips().iter().map(|ch| ch.label.as_str()).collect();
    assert_eq!(
        labels,
        ["[Image #1]", "[Image #2]", "[Pasted text #1 +10 lines]"]
    );
    assert_eq!(
        c.text(),
        format!("{}{}{} compare", chip(1), chip(2), chip(3))
    );
    let sent = c.take();
    let order: Vec<&str> = sent.images.iter().map(|d| &**d).collect();
    assert_eq!(order, ["A", "C"]);
    assert_eq!(sent.text, format!("{} compare", lines_of(10, "\n")));
}

#[test]
fn a_draft_of_only_refused_images_comes_back_empty_and_no_ordinals_change_nothing() {
    let mut c = Composer::new();
    c.attach(Arc::from("A"), image_label);
    c.attach(Arc::from("B"), image_label);
    let (_, saved) = c.take_saved();
    assert_eq!(saved.clone().without_images(&[]), saved);
    assert!(saved.without_images(&[1, 2]).is_empty());
}

fn wrapped_rows(line: &str, room: usize, chips: &Chips) -> Vec<WrappedRow> {
    let fit = Fit {
        room,
        mode: WidthMode::Normal,
        more: "...",
        chips,
    };
    let mut work = 0;
    wrap_line(line, None, &fit, &mut work).0
}

fn row_text(row: &WrappedRow) -> String {
    row.text.iter().map(|p| p.text.as_str()).collect()
}

fn breaks_inside_a_word(before: &str, after: &str) -> bool {
    let last = before.chars().last();
    let next = after.chars().next();
    let wide = |c: Option<char>| c.is_some_and(|c| width(&c.to_string(), WidthMode::Normal) > 1);
    !last.is_some_and(char::is_whitespace) && !wide(last) && !wide(next)
}

fn check_wrap(line: &str, room: usize) -> Vec<WrappedRow> {
    let chips = Chips::default();
    let rows = wrapped_rows(line, room, &chips);
    let solid = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let joined: String = rows.iter().map(row_text).collect();
    assert_eq!(solid(&joined), solid(line), "room {room}");
    let mut chars = 0;
    for (n, row) in rows.iter().enumerate() {
        let text = row_text(row);
        assert!(row.width <= room + 1, "room {room}: {text:?}");
        if row.width > room {
            assert!(text.ends_with(' '), "room {room}: {text:?}");
        }
        assert_eq!(row.cells.first().map(|c| c.1), Some(chars), "room {room}");
        assert!(
            row.cells
                .windows(2)
                .all(|w| w[0].1 < w[1].1 && w[0].0 <= w[1].0)
        );
        assert!(
            row.cells.iter().all(|c| c.0 <= room),
            "room {room}: {text:?}"
        );
        chars = row.end;
        if let Some(next) = rows.get(n + 1) {
            let after = row_text(next);
            if text.chars().any(|c| !c.is_whitespace()) {
                assert!(!after.starts_with(' '), "room {room}: {text:?} / {after:?}");
            }
            if breaks_inside_a_word(&text, &after) {
                let word: String = text
                    .rsplit(' ')
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .chain(after.chars().take_while(|c| *c != ' '))
                    .collect();
                assert!(
                    width(&word, WidthMode::Normal) > room,
                    "room {room}: {word:?} was broken but fits: {text:?} / {after:?}"
                );
            } else if !after.starts_with(' ') {
                let first: String = after.chars().take_while(|c| *c != ' ').collect();
                let first_w = width(&first, WidthMode::Normal);
                let wide_start = after
                    .chars()
                    .next()
                    .is_some_and(|c| width(&c.to_string(), WidthMode::Normal) > 1);
                if first_w <= room && !wide_start {
                    assert!(
                        row.width + first_w > room,
                        "room {room}: {first:?} would have fit after {text:?}"
                    );
                }
            }
        }
    }
    assert_eq!(chars, line.chars().count(), "room {room}");
    rows
}

fn texts_at(line: &str, room: usize) -> Vec<String> {
    check_wrap(line, room).iter().map(row_text).collect()
}

#[test]
fn latin_after_wide_text_moves_whole_and_wide_text_breaks_before_itself() {
    assert_eq!(
        texts_at("ok\u{6F22}\u{5B57}latin", 7),
        ["ok\u{6F22}\u{5B57}", "latin"]
    );
    assert_eq!(texts_at("ab cd\u{6F22}", 5), ["ab cd", "\u{6F22}"]);
    assert_eq!(
        texts_at("aaa bb\u{6F22}\u{5B57}\u{304B}\u{306A}", 5)[..2],
        ["aaa ", "bb\u{6F22}"]
    );
}

#[test]
fn leading_whitespace_is_a_break_point_and_never_hangs() {
    assert_eq!(texts_at("      abcdefgh", 10), ["      ", "abcdefgh"]);
    assert_eq!(texts_at("        ab", 4), ["    ", "    ", "ab"]);
    for room in 2..=30 {
        check_wrap("    indented words go here\tand a tab\tin the middle", room);
    }
}

#[test]
fn every_space_at_the_edge_hangs_in_one_column() {
    let rows = check_wrap("aaaa  bbbb", 4);
    assert_eq!(
        rows.iter().map(row_text).collect::<Vec<_>>(),
        ["aaaa ", "bbbb"]
    );
    assert_eq!(rows[0].width, 5);
    assert_eq!(rows[0].end, 6);
    assert_eq!(texts_at("aaaa\tbbbb cc", 6), ["aaaa   ", "bbbb ", "cc"]);
    for room in 3..=8 {
        let rows = check_wrap("abcd\u{3000}\u{3000}ef gh\t\tij", room);
        assert!(rows.iter().all(|r| r.width <= room + 1));
    }
}

#[test]
fn no_break_spaces_hold_their_words_together() {
    for nb in ['\u{a0}', '\u{2007}', '\u{202f}'] {
        let line = format!("x aa{nb}bb");
        assert_eq!(texts_at(&line, 6), ["x ".to_owned(), format!("aa{nb}bb")]);
    }
}

#[test]
fn a_click_right_of_a_short_wrapped_row_stays_on_that_row() {
    let mut c = Composer::new();
    typed(&mut c, "alpha beta gamma delta epsilon zeta");
    let Some(shape) = (20..60).map(layout).find(|s| s.room() == 26) else {
        panic!("no width gives room 26");
    };
    for col in [26, 27, 40] {
        let view = c.view(shape);
        assert!(c.jump(&view, 0, view.lead + col));
        let after = c.view(shape);
        assert_eq!(after.cursor.0, 0, "col {col}");
        assert_eq!(c.cursor(), (0, 22), "col {col}");
    }
    let view = c.view(shape);
    assert!(c.jump(&view, 1, view.lead + 40));
    assert_eq!(c.cursor(), (0, 35));
}

const PROSE: &str = "Rename the cache fingerprint module so its name says what it compares, then update every caller and see https://example.com/a/very/long/path/that/never/ends/in/time?query=1&more=2 before 漢字かな交じりの文章も折り返す and finish";

#[test]
fn words_wrap_whole_at_every_width_from_20_to_200() {
    for width in 20..=200 {
        let room = layout(width).room();
        check_wrap(PROSE, room);
    }
}

#[test]
fn a_long_url_fills_the_row_it_starts_on_and_breaks_only_itself() {
    let line = "see https://example.com/a/very/long/path/that/never/ends";
    let rows = check_wrap(line, 20);
    let texts: Vec<String> = rows.iter().map(row_text).collect();
    assert_eq!(
        texts,
        [
            "see https://example.",
            "com/a/very/long/path",
            "/that/never/ends"
        ]
    );
}

#[test]
fn ime_text_breaks_between_wide_characters_and_never_inside_one() {
    let line =
        "ok \u{6F22}\u{5B57}\u{304B}\u{306A}\u{4EA4}\u{3058}\u{308A}\u{306E}\u{6587}\u{7AE0}";
    let rows = check_wrap(line, 9);
    let texts: Vec<String> = rows.iter().map(row_text).collect();
    assert_eq!(
        texts,
        [
            "ok \u{6F22}\u{5B57}\u{304B}",
            "\u{306A}\u{4EA4}\u{3058}\u{308A}",
            "\u{306E}\u{6587}\u{7AE0}"
        ]
    );
    for room in 2..=12 {
        check_wrap(line, room);
    }
}

#[test]
fn a_chip_moves_whole_to_the_next_row_like_a_word() {
    let mut c = Composer::new();
    typed(&mut c, "look at ");
    c.paste(&lines_of(12, "\n"));
    typed(&mut c, " please");
    let view = c.view(layout(30));
    let rows = text_of(&view);
    assert!(rows[0].ends_with("look at"), "{rows:?}");
    assert!(rows[1].trim_start().starts_with("[Pasted"), "{rows:?}");
}

#[test]
fn a_space_at_the_edge_hangs_and_the_caret_after_it_starts_the_next_row() {
    let mut c = Composer::new();
    let room = layout(20).room();
    let word = "a".repeat(room);
    typed(&mut c, &word);
    typed(&mut c, " ");
    let view = c.view(layout(20));
    assert_eq!(text_of(&view).len(), 2, "{:?}", text_of(&view));
    assert_eq!(view.cursor, (1, view.lead));
    let boxed = ComposerShape {
        boxed: true,
        ..layout(24)
    };
    for shape in [layout(20), boxed] {
        let mut d = Composer::new();
        typed(&mut d, &"a".repeat(shape.room()));
        typed(&mut d, " ");
        let view = d.view(shape);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.cursor, (1, view.lead));
        for line in &view.rows {
            let drawn = line_text(line);
            assert_eq!(
                width(&drawn, WidthMode::Normal),
                shape.layout.width,
                "{drawn:?}"
            );
        }
    }
    typed(&mut c, "b");
    let view = c.view(layout(20));
    let rows = text_of(&view);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].ends_with(&word), "{rows:?}");
    assert_eq!(rows[1].trim_start(), "b", "{rows:?}");
    assert_eq!(view.cursor, (1, view.lead + 1));
    assert_eq!(c.text(), format!("{word} b"));
}

#[test]
fn the_caret_lands_on_its_character_at_every_position_and_width() {
    let line = "the quick brown fox jumps over \u{6F22}\u{5B57} lazy dogs";
    let mut c = Composer::new();
    typed(&mut c, line);
    let total = line.chars().count();
    for width in [20, 24, 33, 47, 80] {
        let shape = layout(width);
        let chips = Chips::default();
        let rows = wrapped_rows(line, shape.room(), &chips);
        for _ in 0..total {
            press(&mut c, KeyCode::Left);
        }
        for at in 0..=total {
            let view = c.view(shape);
            let (row, col) = view.cursor;
            let mark = &view.marks[row];
            if at < total {
                let cell = mark
                    .cells
                    .iter()
                    .find(|(_, ch)| *ch == at)
                    .unwrap_or_else(|| panic!("{width}: char {at} not on row {row}"));
                assert_eq!(cell.0 + view.lead, col, "{width}: char {at}");
            } else {
                assert!(rows.len() >= row, "{width}");
            }
            press(&mut c, KeyCode::Right);
        }
    }
}

fn shape_with_room(room: usize) -> ComposerShape {
    let Some(shape) = (10..200).map(layout).find(|s| s.room() == room) else {
        panic!("no width gives room {room}");
    };
    shape
}

#[test]
fn up_and_down_walk_the_rows_of_a_wrapped_line_before_leaving_it() {
    let shape = shape_with_room(26);
    let mut c = Composer::new();
    typed(&mut c, "alpha beta gamma delta epsilon zeta");
    assert_eq!(text_of(&c.view(shape)).len(), 2);
    assert!(!c.on_first_row(shape));
    assert!(c.on_last_row(shape));
    assert!(c.on_first_line());
    assert!(c.vertical(true, shape));
    assert_eq!(c.cursor(), (0, 12));
    assert!(c.on_first_row(shape));
    assert!(!c.on_last_row(shape));
    assert!(!c.vertical(true, shape));
    assert_eq!(c.cursor(), (0, 12));
    assert!(c.vertical(false, shape));
    assert_eq!(c.cursor(), (0, 35));
    assert!(!c.vertical(false, shape));
}

#[test]
fn the_goal_column_survives_a_short_row_and_resets_after_another_move() {
    let shape = shape_with_room(40);
    let mut c = Composer::new();
    typed(&mut c, "abcdefghij");
    c.newline();
    typed(&mut c, "ab");
    c.newline();
    typed(&mut c, "abcdefghij");
    for _ in 0..12 {
        press(&mut c, KeyCode::Up);
    }
    let view = c.view(shape);
    assert!(c.jump(&view, 0, view.lead + 8));
    assert_eq!(c.cursor(), (0, 8));
    assert!(c.vertical(false, shape));
    assert_eq!(c.cursor(), (1, 2));
    assert!(c.vertical(false, shape));
    assert_eq!(c.cursor(), (2, 8));
    press(&mut c, KeyCode::Left);
    assert!(c.vertical(true, shape));
    assert_eq!(c.cursor(), (1, 2));
    assert!(c.vertical(true, shape));
    assert_eq!(c.cursor(), (0, 7));
}

#[test]
fn a_column_inside_a_wide_character_lands_on_that_character() {
    let shape = shape_with_room(40);
    let mut c = Composer::new();
    typed(&mut c, "abcdef");
    c.newline();
    typed(&mut c, "\u{6F22}\u{5B57}\u{304B}\u{306A}");
    let view = c.view(shape);
    assert!(c.jump(&view, 0, view.lead + 3));
    assert!(c.vertical(false, shape));
    assert_eq!(c.cursor(), (1, 1));
    assert!(c.vertical(true, shape));
    assert_eq!(c.cursor(), (0, 3));
}

#[test]
fn a_chip_under_the_column_is_landed_on_whole() {
    let shape = shape_with_room(60);
    let mut c = Composer::new();
    typed(&mut c, "ab");
    c.paste(&lines_of(12, "\n"));
    typed(&mut c, "cd");
    c.newline();
    typed(&mut c, &"x".repeat(4));
    assert!(c.vertical(true, shape));
    assert_eq!(c.cursor(), (0, 2));
    assert!(c.vertical(false, shape));
    assert_eq!(c.cursor(), (1, 4));
}

#[test]
fn the_row_after_a_hung_space_is_the_last_row() {
    let shape = shape_with_room(20);
    let mut c = Composer::new();
    typed(&mut c, &"a".repeat(20));
    typed(&mut c, " ");
    assert!(c.on_last_row(shape));
    assert!(!c.on_first_row(shape));
    assert!(c.vertical(true, shape));
    assert_eq!(c.cursor(), (0, 0));
    assert!(c.vertical(false, shape));
    assert_eq!(c.cursor(), (0, 21));
    assert!(c.on_last_row(shape));
    c.newline();
    typed(&mut c, "b");
    assert!(c.vertical(true, shape));
    assert_eq!(c.cursor(), (0, 21));
    assert!(c.vertical(true, shape));
    assert_eq!(c.cursor(), (0, 1));
}

#[test]
fn a_typed_character_wraps_the_caret_line_once() {
    let shape = layout(60);
    let line = "word ".repeat(2_000);
    let run = |edges: bool| {
        let mut c = Composer::new();
        c.insert(&line);
        c.view(shape);
        let before = c.wrapped();
        for ch in "abc".chars() {
            let key = Key::plain(KeyCode::Char(ch));
            if edges {
                assert_eq!(c.row_edges(key, shape), (false, false));
            }
            c.edit(key);
            c.view(shape);
        }
        c.wrapped() - before
    };
    let plain = run(false);
    assert!(plain > 0);
    assert_eq!(run(true), plain);
    let mut c = Composer::new();
    c.insert(&line);
    let before = c.wrapped();
    assert_eq!(c.row_edges(Key::plain(KeyCode::Up), shape), (false, false));
    let once = c.wrapped() - before;
    assert!(once > 0 && once <= line.len(), "{once}");
    assert_eq!(c.row_edges(Key::plain(KeyCode::Down), shape), (false, true));
    assert_eq!(c.wrapped() - before, 2 * once);
}

#[test]
fn an_edit_that_ends_where_it_began_forgets_the_goal_column() {
    let shape = shape_with_room(40);
    let probes: [&[KeyCode]; 2] = [
        &[KeyCode::Char('x'), KeyCode::Backspace],
        &[KeyCode::Left, KeyCode::Right],
    ];
    for keys in probes {
        let mut c = Composer::new();
        typed(&mut c, "abcdefghij");
        c.newline();
        typed(&mut c, "ab");
        c.newline();
        typed(&mut c, "abcdefghij");
        let view = c.view(shape);
        assert!(c.jump(&view, 0, view.lead + 8));
        assert!(c.vertical(false, shape));
        assert_eq!(c.cursor(), (1, 2));
        for &key in keys {
            press(&mut c, key);
        }
        assert_eq!(c.cursor(), (1, 2));
        assert!(c.vertical(false, shape));
        assert_eq!(c.cursor(), (2, 2), "{keys:?}");
    }
    let mut c = Composer::new();
    typed(&mut c, "abcdefghij");
    c.newline();
    typed(&mut c, "ab");
    c.newline();
    typed(&mut c, "abcdefghij");
    let view = c.view(shape);
    assert!(c.jump(&view, 0, view.lead + 8));
    assert!(c.vertical(false, shape));
    let view = c.view(shape);
    assert!(c.jump(&view, 1, view.lead + 2));
    assert_eq!(c.cursor(), (1, 2));
    assert!(c.vertical(false, shape));
    assert_eq!(c.cursor(), (2, 2), "a click sets the column afresh");
}

#[test]
fn a_new_width_starts_a_fresh_goal_column() {
    let mut c = Composer::new();
    typed(&mut c, "abcdefghij");
    c.newline();
    typed(&mut c, "ab");
    c.newline();
    typed(&mut c, "abcdefghij");
    let wide = shape_with_room(40);
    let view = c.view(wide);
    assert!(c.jump(&view, 0, view.lead + 8));
    assert!(c.vertical(false, wide));
    assert_eq!(c.cursor(), (1, 2));
    assert!(c.vertical(false, shape_with_room(30)));
    assert_eq!(c.cursor(), (2, 2));
}
