use wisp_contain::is_contained;

use super::render::{cell_floor, share_columns};
use super::*;
use crate::styled::StyledSpan;
use crate::styled::{Look, StyledLine};
use crate::testing::{Rng, line_text, text};
use crate::text::{WidthMode, width};
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Ink, Token};

fn tags(look: &Look) -> String {
    let mut out = vec![];
    for (on, tag) in [
        (look.bold, "b"),
        (look.italic, "i"),
        (look.underline, "u"),
        (look.strike, "s"),
        (look.reverse, "r"),
        (look.diff_code, "code"),
    ] {
        if on {
            out.push(tag.to_owned());
        }
    }
    let ink = |i: Ink| match i {
        Ink::Token(t) => t.name().to_owned(),
        Ink::Mix { .. } => "mix".to_owned(),
    };
    if let Some(fg) = look.fg {
        out.push(ink(fg));
    }
    if let Some(bg) = look.bg {
        out.push(format!("on {}", ink(bg)));
    }
    out.join(" ")
}

fn show(lines: &[StyledLine]) -> String {
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            if span.look == Look::default() {
                out.push_str(&span.text);
            } else {
                out.push_str(&format!("[{}|{}]", tags(&span.look), span.text));
            }
        }
        if let Some((pad, look)) = line.band_pad() {
            out.push_str(&format!("{{{pad} {}}}", tags(&look)));
        }
        out.push('\n');
    }
    out
}

fn render(source: &str, width: usize) -> Vec<StyledLine> {
    Renderer::new(Layout::new(width, GlyphSet::Unicode)).markdown(source)
}

const WIDTHS: [usize; 5] = [120, 80, 60, 24, 10];

fn at_widths(source: &str, set: GlyphSet) -> String {
    let mut out = String::new();
    for w in WIDTHS {
        out.push_str(&format!("--- {w} ---\n"));
        out.push_str(&show(&Renderer::new(Layout::new(w, set)).markdown(source)));
    }
    out
}

const PARAGRAPH: &str = "Misses now say which part of the prefix changed: the system prompt, the tools or the messages. The prefix-stability test covers each case, and a long word like supercalifragilisticexpialidocious_and_then_some_more_to_overflow still wraps.\n\nA second paragraph\nwith a soft break and a hard  \nbreak.";

const HEADINGS: &str =
    "# One\n\nBody under one.\n\n## Two\n\n### Three\n\n#### Four with *emphasis*";

const LISTS: &str = "- first item that is long enough to wrap onto a second row when the measure is narrow\n- second\n  - nested one\n  - nested two\n    1. deep ordered\n    2. deep again\n- third\n\n8. eight\n9. nine\n10. ten\n\n- loose one\n\n- loose two\n\n  with a second paragraph\n\n- [ ] a task\n- [x] done task";

const QUOTES: &str = "> A quoted paragraph that runs long enough to wrap at the narrower widths we test.\n>\n> > A nested quote.\n>\n> - a list inside\n> - a quote";

const CODE: &str = "```rust\nfn main() {\n\tlet x = \"a string that is long enough to need splitting at the narrow widths, and more besides to pass a hundred columns\";\n}\n```\n\n    indented code\n\n```\n```\n\n```python\n```";

const DIFF: &str = "```diff\n--- a/cache.rs\n+++ b/cache.rs\n@@ -209,3 +209,3 @@\n fn key(req: &Request) -> Key {\n-    let key = hash(&req.system);\n+    let key = Fingerprint::of(req)?;\n     key\n```";

const TABLE: &str = "| Name | Kind | Size |\n|:-----|:----:|-----:|\n| `cache.rs` | file | 412 |\n| **src** | dir | 7 |\n| [docs](https://example.com/docs) | link | 1 |";

const WIDE_TABLE: &str = "| Model | Input price per million tokens | Output price per million tokens | Cache write | Cache read |\n|---|---|---|---|---|\n| claude-sonnet-5-5 | three dollars and no cents | fifteen dollars | three seventy five | thirty cents |\n| claude-haiku | one dollar | five dollars | | ten cents |";

const RULES: &str = "Above\n\n---\n\nBelow\n\n***";

const INLINE: &str = "Plain *emphasis* **strong** ***both*** `inline code` ~~struck~~ and **bold with `code` and *nested emphasis***.";

const LINKS: &str = "See [the docs](https://example.com/docs \"Title\"), <https://example.com/auto>, a bare https://example.org/path?q=1 and www.example.net, and ![an image](https://example.com/a.png). An email <me@example.com>.";

#[test]
fn snapshot_paragraphs() {
    insta::assert_snapshot!("paragraphs", at_widths(PARAGRAPH, GlyphSet::Unicode));
}

#[test]
fn snapshot_headings() {
    insta::assert_snapshot!("headings", at_widths(HEADINGS, GlyphSet::Unicode));
}

#[test]
fn snapshot_lists() {
    insta::assert_snapshot!("lists", at_widths(LISTS, GlyphSet::Unicode));
}

#[test]
fn snapshot_quotes() {
    insta::assert_snapshot!("quotes", at_widths(QUOTES, GlyphSet::Unicode));
}

#[test]
fn snapshot_code() {
    insta::assert_snapshot!("code", at_widths(CODE, GlyphSet::Unicode));
}

#[test]
fn snapshot_diff() {
    insta::assert_snapshot!("diff", at_widths(DIFF, GlyphSet::Unicode));
}

#[test]
fn snapshot_table() {
    insta::assert_snapshot!("table", at_widths(TABLE, GlyphSet::Unicode));
}

#[test]
fn snapshot_wide_table() {
    insta::assert_snapshot!("wide_table", at_widths(WIDE_TABLE, GlyphSet::Unicode));
}

#[test]
fn snapshot_rules() {
    insta::assert_snapshot!("rules", at_widths(RULES, GlyphSet::Unicode));
}

#[test]
fn snapshot_inline() {
    insta::assert_snapshot!("inline", at_widths(INLINE, GlyphSet::Unicode));
}

#[test]
fn snapshot_links() {
    insta::assert_snapshot!("links", at_widths(LINKS, GlyphSet::Unicode));
}

const WIDE_WIDTHS: [usize; 3] = [120, 160, 200];

fn at_wide_widths(source: &str) -> String {
    let mut out = String::new();
    for w in WIDE_WIDTHS {
        out.push_str(&format!("--- {w} ---\n"));
        out.push_str(&show(&render(source, w)));
    }
    out
}

const WRAPPING_TABLE: &str = "| Approach | Unit reversed | Example |\n|---|:---:|---:|\n| Reverse the characters | Each grapheme, so combined marks and emoji stay whole | `\"héllo\"` becomes `\"olléh\"` once the clusters are kept together |\n| Reverse the words | Each whitespace-separated word, keeping the letters of each word in their order | `\"one two three\"` becomes `\"three two one\"` |\n| Reverse the lines | Each line of the input, which suits logs read from the newest entry back to the oldest | the last line prints first |";

const STACKING_TABLE: &str = "| Alpha column | Bravo column | Charlie column | Delta column | Echo column | Foxtrot column | Golf column | Hotel column | India column | Juliett column | Kilo column | Lima column | Mike column | November column | Oscar column | Papa column |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n| first value here | second value here | third value here | fourth value here | fifth value here | sixth value here | seventh value here | eighth value here | ninth value here | tenth value here | eleventh value here | twelfth value here | thirteenth value | fourteenth value | fifteenth value | sixteenth value |";

#[test]
fn snapshot_wide_prose() {
    insta::assert_snapshot!("wide_prose", at_wide_widths(PARAGRAPH));
}

#[test]
fn snapshot_wide_code() {
    insta::assert_snapshot!("wide_code", at_wide_widths(CODE));
}

#[test]
fn snapshot_wide_grid() {
    insta::assert_snapshot!("wide_grid", at_wide_widths(WIDE_TABLE));
}

#[test]
fn snapshot_wide_wrapped_table() {
    insta::assert_snapshot!("wide_wrapped_table", at_wide_widths(WRAPPING_TABLE));
}

#[test]
fn snapshot_wide_stacked_table() {
    insta::assert_snapshot!("wide_stacked_table", at_wide_widths(STACKING_TABLE));
}

#[test]
fn a_wide_terminal_wraps_cells_where_a_narrow_one_stacks_records() {
    for w in [100, 120, 160, 200] {
        let rows = text(&render(WRAPPING_TABLE, w));
        assert!(rows.contains('\u{2500}'), "{w}: {rows}");
        assert!(!rows.contains("Approach:"), "{w}: {rows}");
        for word in ["grapheme", "whitespace-separated", "newest", "oldest"] {
            assert!(rows.contains(word), "{w} {word}: {rows}");
        }
    }
    for w in [60, 80] {
        let rows = text(&render(WRAPPING_TABLE, w));
        assert!(
            rows.contains("Approach: Reverse the characters"),
            "{w}: {rows}"
        );
        assert!(!rows.contains('\u{2500}'), "{w}: {rows}");
    }
    for w in WIDE_WIDTHS {
        let rows = text(&render(STACKING_TABLE, w));
        assert!(
            rows.contains("Alpha column: first value here"),
            "{w}: {rows}"
        );
    }
}

fn widths_at(source: &str, w: usize) -> Option<Vec<usize>> {
    let parsed = blocks(source);
    let Some(Block::Table(table)) = parsed.first() else {
        return None;
    };
    Renderer::new(Layout::new(w, GlyphSet::Unicode)).table_widths(table)
}

fn table_of(columns: &[(&str, &str)]) -> String {
    let head: Vec<&str> = columns.iter().map(|c| c.0).collect();
    let cells: Vec<&str> = columns.iter().map(|c| c.1).collect();
    format!(
        "| {} |\n|{}\n| {} |",
        head.join(" | "),
        "---|".repeat(columns.len()),
        cells.join(" | ")
    )
}

#[test]
fn columns_share_the_width_max_min_fairly() {
    let long_words = vec!["a".repeat(40); 5].join(" ");
    let short_words = vec!["word"; 44].join(" ");
    let sevens = table_of(&[
        ("A", &long_words),
        ("B", &short_words),
        ("c", "abc"),
        ("d", "abc"),
        ("e", "abc"),
        ("f", "abc"),
        ("g", "abc"),
    ]);
    assert_eq!(widths_at(&sevens, 120), Some(vec![45, 44, 3, 3, 3, 3, 3]));
    let thirty = format!("{} tail", "x".repeat(30));
    let wide = vec!["word"; 25].join(" ");
    let capped = table_of(&[
        ("A", &thirty),
        ("B", &wide),
        ("C", &wide),
        ("D", &wide),
        ("E", "abcde"),
    ]);
    assert_eq!(widths_at(&capped, 100), Some(vec![24, 20, 20, 19, 5]));
    assert_eq!(widths_at(&capped, 98), Some(vec![24, 19, 19, 19, 5]));
    assert_eq!(widths_at(&capped, 80), None);
    assert_eq!(widths_at(&empty_columns(50), 81), None);
    assert_eq!(widths_at(&empty_columns(50), 200), Some(vec![0; 50]));
    assert_eq!(cell_floor(5, 3), 5);
    assert_eq!(cell_floor(30, 3), 12);
    assert_eq!(cell_floor(30, 40), 24);
    assert_eq!(cell_floor(100, 18), 18);
    assert_eq!(share_columns(&[50, 50], &[30, 30], 59), None);
    assert_eq!(share_columns(&[50, 50], &[30, 30], 60), Some(vec![30, 30]));
    assert_eq!(
        share_columns(&[50, 50, 50], &[12, 12, 12], 100),
        Some(vec![34, 33, 33])
    );
    assert_eq!(share_columns(&[10, 90], &[10, 12], 60), Some(vec![10, 50]));
    assert_eq!(share_columns(&[90, 90], &[40, 12], 60), Some(vec![40, 20]));
}

#[test]
fn a_table_of_empty_columns_wider_than_the_screen_stacks() {
    let rows = text(&render(&empty_columns(50), 81));
    assert!(!rows.contains('\u{2500}'), "{rows}");
}

#[test]
fn wrapped_cells_keep_their_alignment_and_column() {
    let lines = render(WRAPPING_TABLE, 120);
    let rows: Vec<String> = lines.iter().map(line_text).collect();
    let rule = rows
        .iter()
        .find(|r| r.contains('\u{2500}'))
        .map(|r| width(r, WidthMode::Normal))
        .unwrap_or(0);
    assert!(rule <= GUTTER + 116, "{rule}");
    let starts: Vec<usize> = rows
        .iter()
        .filter(|r| r.contains("Reverse the"))
        .map(|r| r.find("Reverse").unwrap_or(0))
        .collect();
    assert!(starts.iter().all(|s| *s == GUTTER), "{rows:#?}");
    for row in &rows {
        assert_eq!(row.trim_end(), row);
        assert!(width(row, WidthMode::Normal) < 120, "{row}");
    }
    let right = rows
        .iter()
        .find(|r| r.contains("the last line prints first"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(width(&right, WidthMode::Normal), rule, "{rows:#?}");
}

const SHOWCASE: &str = "## Showcase\n\n- a bullet with *style*\n  1. ordered\n\n> quoted\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n```sh\necho hi\n```";

#[test]
fn snapshot_glyph_sets() {
    let mut out = String::new();
    for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
        out.push_str(&format!("=== {set:?} ===\n"));
        out.push_str(&show(
            &Renderer::new(Layout::new(40, set)).markdown(SHOWCASE),
        ));
    }
    insta::assert_snapshot!("glyph_sets", out);
}

fn corpus() -> Vec<String> {
    let mut out: Vec<String> = [
        PARAGRAPH, HEADINGS, LISTS, QUOTES, CODE, DIFF, TABLE, WIDE_TABLE, RULES, INLINE, LINKS,
        SHOWCASE,
    ]
    .map(str::to_owned)
    .to_vec();
    out.push(format!("{PARAGRAPH}\n\n{LISTS}\n\n{QUOTES}\n\n{TABLE}"));
    out.push("\u{4E2D}\u{6587}\u{5B57}\u{7B26} \u{1F469}\u{200D}\u{1F4BB} e\u{301} \u{B7}\u{2026} mixed text that wraps \u{4E2D}\u{6587}\u{5B57}\u{7B26}\u{4E32}\u{6D4B}\u{8BD5}".to_owned());
    out.push("| \u{4E2D}\u{6587} | b |\n|---|---|\n| \u{1F600} | \u{B7} |".to_owned());
    out.push(WRAPPING_TABLE.to_owned());
    out.push(STACKING_TABLE.to_owned());
    out.push(empty_columns(50));
    out
}

fn empty_columns(n: usize) -> String {
    format!(
        "{}|\n{}|\n{}|",
        "| ".repeat(n),
        "|-".repeat(n),
        "| ".repeat(n)
    )
}

#[test]
fn rows_are_copy_clean() {
    for source in corpus() {
        for w in [120, 80, 60, 40, 24, 10, 4, 1, 0] {
            for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
                for line in Renderer::new(Layout::new(w, set)).markdown(&source) {
                    let row = line_text(&line);
                    assert_eq!(row.trim_end(), row, "{w} {set:?} {row:?}");
                    assert!(!row.chars().any(char::is_control), "{row:?}");
                    assert!(line.spans.iter().all(|s| !s.text.is_empty()), "{line:?}");
                }
            }
        }
    }
}

#[test]
fn rows_fit_the_width() {
    for source in corpus() {
        for w in 8..=130 {
            for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
                let layout = Layout::new(w, set);
                let mode = layout.width_mode;
                let renderer = Renderer::new(layout);
                for block in blocks(&source) {
                    for line in renderer.block(&block) {
                        let cells = width(&line_text(&line), mode);
                        assert!(cells < w.max(GUTTER + 2), "{w} {set:?} {line:?}");
                        assert!(cells <= GUTTER + layout.wide(), "{w} {set:?} {line:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn prose_and_code_take_the_full_width() {
    let long = PARAGRAPH.repeat(4).replace('\n', " ");
    for w in [200, 250] {
        let widest = render(&long, w)
            .iter()
            .map(|l| width(&line_text(l), WidthMode::Normal))
            .max()
            .unwrap_or(0);
        assert!(widest < w, "{w} {widest}");
        assert!(widest > w - 1 - 15, "{w} {widest}");
    }
    let code = render(CODE, 120);
    let widest = code
        .iter()
        .map(|l| width(&line_text(l), WidthMode::Normal))
        .max()
        .unwrap_or(0);
    assert_eq!(widest, GUTTER + 116);
    for (w, wide) in [
        (60, 56),
        (80, 76),
        (84, 80),
        (120, 116),
        (160, 156),
        (250, 246),
    ] {
        let layout = Layout::new(w, GlyphSet::Unicode);
        assert_eq!(layout.wide(), wide, "{w}");
        assert_eq!(crate::chrome::edge(layout), w - 1, "{w}");
    }
}

#[test]
fn content_starts_at_column_three() {
    for line in render(PARAGRAPH, 80).iter().filter(|l| !l.spans.is_empty()) {
        let row = line_text(line);
        assert!(
            row.starts_with("   ") && !row[3..].starts_with(' '),
            "{row:?}"
        );
    }
    let lists = text(&render("- a\n  - b", 80));
    assert_eq!(lists, "   \u{2219} a\n     \u{2219} b");
}

#[test]
fn headings_use_weight_and_tone_never_marks_or_accent() {
    let lines = render(HEADINGS, 80);
    let joined = text(&lines);
    assert!(!joined.contains('#'));
    for line in &lines {
        for span in &line.spans {
            assert_ne!(span.look.fg, Some(Ink::Token(Token::Accent)));
        }
    }
    assert_eq!(
        lines[0].spans[1],
        StyledSpan::new("One", Look::default().bold())
    );
    assert_eq!(
        lines[6].spans[1],
        StyledSpan::new("Three", Look::fg(Token::TextMuted).bold())
    );
}

#[test]
fn inline_code_has_no_backticks() {
    let joined = text(&render(INLINE, 80));
    assert!(!joined.contains('`'));
    assert!(joined.contains("inline code"));
}

#[test]
fn links_show_text_and_url() {
    let lines = render(LINKS, 200);
    let joined = text(&lines).replace("\n   ", " ");
    assert!(
        joined.contains("the docs (https://example.com/docs)"),
        "{joined}"
    );
    assert_eq!(
        joined.matches("https://example.com/auto").count(),
        1,
        "{joined}"
    );
    assert!(
        joined.contains("an image (https://example.com/a.png)"),
        "{joined}"
    );
    let link = Look::fg(Token::Accent).underline();
    let bare = lines
        .iter()
        .flat_map(|l| &l.spans)
        .find(|s| s.text.starts_with("https://example.org"));
    assert_eq!(bare.map(|s| s.look), Some(link));
    assert_eq!(
        bare.map(|s| s.text.as_str()),
        Some("https://example.org/path?q=1")
    );
    assert!(!joined.contains("\u{1b}]8"));
}

#[test]
fn bare_urls_are_not_linked_inside_code() {
    let lines = render(
        "`https://example.com` and\n\n```\nhttps://example.com\n```",
        80,
    );
    for span in lines.iter().flat_map(|l| &l.spans) {
        assert!(!span.look.underline, "{span:?}");
    }
}

#[test]
fn tight_and_loose_lists() {
    assert_eq!(
        text(&render("- a\n- b", 80)),
        "   \u{2219} a\n   \u{2219} b"
    );
    assert_eq!(
        text(&render("- a\n\n- b", 80)),
        "   \u{2219} a\n\n   \u{2219} b"
    );
    assert_eq!(text(&render("9. a\n10. b", 80)), "   9. a\n   10. b");
}

#[test]
fn wide_tables_become_records() {
    let narrow = text(&render(WIDE_TABLE, 80));
    assert!(narrow.contains("Model: claude-sonnet-5-5"), "{narrow}");
    assert!(narrow.contains("Cache write:"), "{narrow}");
    assert!(!narrow.contains('\u{2500}'), "{narrow}");
    let wide = text(&render(TABLE, 80));
    assert!(wide.contains('\u{2500}'), "{wide}");
    assert!(!wide.contains('|'), "{wide}");
    let small = text(&render(TABLE, 20));
    assert!(small.contains("Name: cache.rs"), "{small}");
}

#[test]
fn diff_rows_are_tinted_and_signs_stay_ascii() {
    let lines = render(DIFF, 80);
    let added = lines.iter().find(|l| line_text(l).contains("+    let key"));
    let removed = lines.iter().find(|l| line_text(l).contains("-    let key"));
    assert_eq!(
        added.map(|l| l.spans[1].look.bg),
        Some(Some(Token::DiffAdd.into()))
    );
    assert_eq!(
        removed.map(|l| l.spans[1].look.bg),
        Some(Some(Token::DiffDel.into()))
    );
    assert!(!text(&lines).contains('\u{2212}'));
}

#[test]
fn code_has_no_borders_and_keeps_indentation() {
    let lines = render("```\n\tx\n  y\n```", 80);
    assert_eq!(text(&lines), "       x\n     y");
}

#[test]
fn the_language_label_sits_right_on_the_first_row() {
    let lines = render("```rust\nfn main() {}\n```", 40);
    let first = line_text(&lines[0]);
    assert!(first.starts_with("   fn main() {}"), "{first:?}");
    assert!(first.ends_with("rust"));
    assert_eq!(width(&first, WidthMode::Normal), GUTTER + 36);
    assert_eq!(
        lines[0].spans.last(),
        Some(&StyledSpan::new("rust", Token::TextFaint))
    );
}

#[test]
fn the_language_label_ends_at_the_content_edge_on_wide_screens() {
    for (screen, end) in [(80, 79), (120, 119), (200, 199), (250, 249)] {
        let lines = render("```rust\nfn main() {}\n```", screen);
        let first = line_text(&lines[0]);
        assert!(first.ends_with("rust"), "{first:?}");
        assert_eq!(width(&first, WidthMode::Normal), end, "{screen}");
    }
}

#[test]
fn the_content_edge_is_the_screen_edge_at_every_width() {
    for (screen, edge) in [
        (5, 5),
        (40, 39),
        (80, 79),
        (120, 119),
        (121, 120),
        (250, 249),
    ] {
        assert_eq!(
            Layout::new(screen, GlyphSet::Unicode).content_edge(),
            edge,
            "{screen}"
        );
    }
}

struct Shout;

struct ShoutLines {
    lang: String,
    count: usize,
}

impl CodeLines for ShoutLines {
    fn line(&mut self, line: &str) -> Vec<StyledSpan> {
        self.count += 1;
        vec![StyledSpan::new(
            format!("{}{}:{}", self.lang, self.count, line.to_uppercase()),
            Token::SynKeyword,
        )]
    }
}

impl CodeStyle for Shout {
    fn key(&self) -> CodeKey {
        CodeKey {
            name: "shout",
            generation: 1,
            params: [0; 3],
        }
    }

    fn start<'s>(&'s self, lang: Option<&str>, _at: FenceAt) -> Box<dyn CodeLines + 's> {
        Box::new(ShoutLines {
            lang: lang.unwrap_or("-").to_owned(),
            count: 0,
        })
    }
}

#[test]
fn a_code_style_plugs_in() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode)).with_code(&Shout);
    let lines = renderer.markdown("```js\nlet a\nlet b\n```\n\ntext");
    assert_eq!(
        lines[0].spans[1],
        StyledSpan::new("js1:LET A", Token::SynKeyword)
    );
    assert_eq!(
        lines[1].spans[1],
        StyledSpan::new("js2:LET B", Token::SynKeyword)
    );
    assert_eq!(line_text(&lines[3]), "   text");
    assert_ne!(
        renderer.key(),
        Renderer::new(Layout::new(80, GlyphSet::Unicode)).key()
    );
}

#[test]
fn input_is_sanitised() {
    let source = "evil \u{1b}]0;title\u{7} \u{1b}[2J \u{202E}rtl \u{9b}31m\r FAKE PROMPT>\u{200B}\n\n```\n\u{1b}[31mred\t\n```\n\n[x](https://e.com/\u{1b}[0m)";
    let joined = text(&render(source, 80));
    assert!(
        !joined.chars().any(|c| c.is_control() && c != '\n'),
        "{joined:?}"
    );
    assert!(joined.contains("^[]0;title^G"), "{joined}");
    assert!(joined.contains("<U+202E>rtl"), "{joined}");
    assert!(joined.contains("M-^[31m"), "{joined}");
    assert!(joined.contains("^[[31mred"), "{joined}");
    assert!(!joined.contains('\u{200B}'));
}

#[test]
fn blocks_render_independently_and_join_with_one_blank_row() {
    let source = format!("{PARAGRAPH}\n\n{LISTS}\n\n{TABLE}\n\n{CODE}");
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let parsed = parse(&source);
    let mut joined: Vec<StyledLine> = vec![];
    let mut last = None;
    for p in &parsed {
        join(&mut joined, &mut last, &p.block, renderer.block(&p.block));
    }
    assert_eq!(joined, renderer.markdown(&source));
}

#[test]
fn each_top_level_block_reparses_from_its_own_source() {
    let source = format!(
        "{REFERENCES}\n\n{HEADINGS}\n\n{LISTS}\n\n{QUOTES}\n\n{CODE}\n\n{DIFF}\n\n{TABLE}\n\n{RULES}\n\n{LINKS}\n\n{REFERENCES}"
    );
    let parsed = parse(&source);
    assert!(parsed.len() > 15, "{}", parsed.len());
    for p in &parsed {
        let again = blocks(&source[p.source.clone()]);
        assert_eq!(
            again,
            vec![p.block.clone()],
            "{:?}",
            &source[p.source.clone()]
        );
    }
    let mut last = 0;
    for p in &parsed {
        assert!(p.source.start >= last);
        last = p.source.end;
    }
}

#[test]
fn rendering_depends_only_on_block_and_layout() {
    use std::collections::HashMap;
    let parsed = blocks(&format!("{LISTS}\n\n{LISTS}"));
    let mut cache: HashMap<(Block, RenderKey), Vec<StyledLine>> = HashMap::new();
    let layout = Layout::new(60, GlyphSet::Unicode);
    let renderer = Renderer::new(layout);
    for block in &parsed {
        let rows = renderer.block(block);
        let cached = cache
            .entry((block.clone(), renderer.key()))
            .or_insert_with(|| rows.clone());
        assert_eq!(*cached, rows);
    }
    assert!(cache.len() < parsed.len());
}

fn depth(block: &Block) -> usize {
    match block {
        Block::Quote(blocks) => 1 + blocks.iter().map(depth).max().unwrap_or(0),
        Block::List { items, .. } => {
            1 + items
                .iter()
                .flat_map(|i| &i.blocks)
                .map(|c| depth(&c.block))
                .max()
                .unwrap_or(0)
        }
        _ => 0,
    }
}

#[test]
fn deep_nesting_is_capped_and_never_panics() {
    let quotes = format!("{} deep", ">".repeat(20_000));
    let lists: String = (0..3_000)
        .map(|i| format!("{}- level {i}\n", "  ".repeat(i)))
        .collect();
    let mixed: String = (0..2_000)
        .map(|i| if i % 2 == 0 { "> " } else { "- " })
        .collect::<String>()
        + "x";
    let defined = format!("{}[a]: /u\n", ">".repeat(20_000));
    for source in [quotes, lists, mixed, defined] {
        let parsed = blocks(&source);
        assert!(parsed.iter().map(depth).max().unwrap_or(0) <= MAX_DEPTH);
        for w in [120, 80, 24, 5, 0] {
            let lines = Renderer::new(Layout::new(w, GlyphSet::Unicode)).markdown(&source);
            assert!(!lines.is_empty());
            if w >= 8 {
                for line in &lines {
                    assert!(width(&line_text(line), WidthMode::Normal) < w);
                }
            }
        }
    }
}

#[test]
fn huge_tables_and_long_words_never_panic() {
    let columns = 300;
    let header: String = (0..columns).map(|c| format!("| h{c} ")).collect::<String>() + "|\n";
    let rule: String = "|---".repeat(columns) + "|\n";
    let row: String = (0..columns).map(|c| format!("| v{c} ")).collect::<String>() + "|\n";
    let table = format!("{header}{rule}{}", row.repeat(200));
    let word = "x".repeat(50_000);
    let wide_cell = format!("| a | b |\n|---|---|\n| {word} | y |");
    let bombs = [
        "*".repeat(10_000),
        "[".repeat(10_000),
        "`".repeat(10_000),
        "_a".repeat(5_000),
        "\u{1F469}\u{200D}".repeat(5_000),
        "e\u{301}".repeat(10_000),
    ];
    let mut sources = vec![table, word, wide_cell];
    sources.extend(bombs);
    for source in &sources {
        for w in [120, 60, 9, 1] {
            let lines = Renderer::new(Layout::new(w, GlyphSet::Unicode)).markdown(source);
            if w >= 9 {
                for line in &lines {
                    assert!(width(&line_text(line), WidthMode::Normal) < w, "{w}");
                }
            }
        }
    }
}

#[test]
fn ascii_glyphs_render_ascii_chrome() {
    let lines = Renderer::new(Layout::new(40, GlyphSet::Ascii)).markdown(SHOWCASE);
    let chrome: String = lines
        .iter()
        .flat_map(|l| &l.spans)
        .filter(|s| {
            s.look.fg == Some(Token::TextFaint.into())
                || s.look.fg == Some(Token::Line.into())
                || s.look.fg == Some(Token::TextMuted.into())
        })
        .map(|s| s.text.as_str())
        .collect();
    assert!(chrome.is_ascii(), "{chrome:?}");
}

const REFERENCES: &str = "[docs]: https://example.com/docs\n\nSee [the docs][docs], [docs][] and [docs], or [later][after] and [missing].\n\n> [inner]: https://example.com/inner\n>\n> Quoted [inner].\n\n- item [after]\n\n[after]: https://example.com/after \"Title\"";

#[test]
fn reference_links_render_literally_and_definitions_show_their_urls() {
    let joined = text(&render(REFERENCES, 120)).replace("\n   ", " ");
    assert!(
        joined.contains("[docs]: https://example.com/docs"),
        "{joined}"
    );
    assert!(
        joined.contains("[after]: https://example.com/after"),
        "{joined}"
    );
    assert!(
        joined.contains("[inner]: https://example.com/inner"),
        "{joined}"
    );
    assert!(
        joined.contains(
            "See [the docs][docs], [docs][] and [docs], or [later][after] and [missing]."
        ),
        "{joined}"
    );
    assert!(joined.contains("Quoted [inner]."), "{joined}");
    let twice = text(&render(&format!("{REFERENCES}\n\n{REFERENCES}"), 120));
    assert_eq!(twice.matches("[docs]: https://example.com/docs").count(), 2);
    assert_eq!(
        twice.matches("[inner]: https://example.com/inner").count(),
        2
    );
    let parsed = parse("[a]: http://a.example\n\n[a] used\n");
    assert_eq!(parsed.len(), 2);
    for p in &parsed {
        assert_eq!(
            blocks(parsed_source("[a]: http://a.example\n\n[a] used\n", p)),
            vec![p.block.clone()]
        );
    }
}

fn parsed_source<'a>(source: &'a str, p: &Parsed) -> &'a str {
    &source[p.source.clone()]
}

fn join(out: &mut Vec<StyledLine>, last: &mut Option<Block>, block: &Block, rows: Vec<StyledLine>) {
    if rows.is_empty() {
        return;
    }
    if last.is_some() {
        out.push(StyledLine::default());
    }
    *last = Some(block.clone());
    out.extend(rows);
}

fn stream(source: &str, renderer: &Renderer<'_>) -> Vec<StyledLine> {
    let mut finished: Vec<StyledLine> = vec![];
    let mut last: Option<Block> = None;
    let mut open_at = 0;
    let mut open: Option<Block> = None;
    for (end, _) in source.match_indices('\n').chain([(source.len(), "")]) {
        let parsed = parse(&source[open_at..end.max(open_at)]);
        let Some((tail, done)) = parsed.split_last() else {
            continue;
        };
        for p in done {
            join(&mut finished, &mut last, &p.block, renderer.block(&p.block));
        }
        open_at += tail.source.start;
        open = Some(tail.block.clone());
    }
    if let Some(block) = open {
        let rows = renderer.block(&block);
        join(&mut finished, &mut last, &block, rows);
    }
    finished
}

const TRICKY: &[&str] = &[
    "- a\n  - b\n\n    [x]: http://nested.example\n\n    [x] used\n",
    "10. a\n\n    [y]: http://wide-marker.example\n",
    "1. a\n   1. b\n\n      [w]: http://deep.example\n",
    "[a]: http://one.example\n[a]: http://two.example\n",
    "[1]: http://bold.example\n\n[**bold** text][1] and [`code` \\*x\\*][1] and ![alt *i*][1]\n\n> [multi\n> line][1] and [1][] and [1]\n",
    "Sources:\n\n- [1]: https://a.example\n- [2]: https://b.example\n",
    "- x\n- [d]: https://d.example\n- y\n",
    "1. [a]: https://oa.example\n2. [b]: https://ob.example\n",
    "> - [q]: https://q.example\n",
    "- [e]: https://e.example\n  text in item\n",
    "[a\\_b]: https://escaped.example\n\n[x][a\\_b] and [y][a&amp;b]\n",
    "[\u{1}]: https://control.example\n\n[z][\u{1}]\n",
];

const TRICKY_URLS: &[&str] = &[
    "http://nested.example",
    "http://wide-marker.example",
    "http://deep.example",
    "http://one.example",
    "http://two.example",
    "http://bold.example",
    "https://a.example",
    "https://b.example",
    "https://d.example",
    "https://oa.example",
    "https://ob.example",
    "https://q.example",
    "https://e.example",
    "https://escaped.example",
    "https://control.example",
];

fn committed_rows_never_change(source: &str, renderer: &Renderer<'_>) {
    let full = renderer.markdown(source);
    for (end, _) in source.match_indices('\n') {
        let parsed = parse(&source[..end + 1]);
        let Some((_, done)) = parsed.split_last() else {
            continue;
        };
        let committed = renderer.blocks(&done.iter().map(|p| p.block.clone()).collect::<Vec<_>>());
        assert!(
            full.len() >= committed.len() && full[..committed.len()] == committed[..],
            "{:?} then {source:?}",
            &source[..end + 1]
        );
    }
}

#[test]
fn definitions_survive_indentation_and_duplicates() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let urls = TRICKY_URLS;
    for source in TRICKY {
        committed_rows_never_change(source, &renderer);
        let full = renderer.markdown(source);
        assert_eq!(stream(source, &renderer), full, "{source:?}");
        let joined = text(&full);
        for url in urls.iter().filter(|u| source.contains(*u)) {
            assert!(joined.contains(url), "{url} in {joined}");
        }
        for p in parse(source) {
            assert_eq!(
                blocks(&source[p.source.clone()]),
                vec![p.block.clone()],
                "{source:?}"
            );
        }
    }
    let duplicates = text(&render(TRICKY[3], 80));
    assert_eq!(
        duplicates,
        "   [a]: http://one.example\n   [a]: http://two.example"
    );
}

#[test]
fn reference_links_style_their_text_as_when_undefined() {
    let lines = render(TRICKY[4], 120);
    let joined = text(&lines);
    assert!(
        joined.contains("[bold text][1] and [code *x*][1] and ![alt i][1]"),
        "{joined}"
    );
    assert!(
        joined.contains("[multi line][1] and [1][] and [1]"),
        "{joined}"
    );
    let bold = lines
        .iter()
        .flat_map(|l| &l.spans)
        .find(|s| s.text == "bold");
    assert_eq!(bold.map(|s| s.look), Some(Look::default().bold()));
}

#[test]
fn streaming_block_by_block_matches_a_full_render() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    for source in [
        REFERENCES.to_owned(),
        TRICKY.join("\n"),
        format!(
            "{REFERENCES}\n\n{PARAGRAPH}\n\n{LISTS}\n\n{QUOTES}\n\n{TABLE}\n\n{CODE}\n\n{RULES}\n\n{LINKS}"
        ),
    ] {
        let streamed = stream(&source, &renderer);
        assert_eq!(streamed, renderer.markdown(&source));
        let joined = text(&streamed);
        for url in ["https://example.com/docs", "https://example.com/after"]
            .iter()
            .chain(TRICKY_URLS)
        {
            assert_eq!(
                joined.contains(url),
                source.contains(url),
                "{url} in {joined}"
            );
        }
    }
}

#[test]
fn earlier_list_rows_never_move() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let full = "8. eight\n9. nine\n10. ten\n11. eleven\n- a\n- b\n\n- c\n- d";
    let all = renderer.markdown(full);
    let mut cut = 0;
    for (end, _) in full.match_indices('\n') {
        let part = renderer.markdown(&full[..end]);
        let settled = part.len().saturating_sub(1);
        assert_eq!(part[..settled], all[..settled], "{:?}", &full[..end]);
        cut = end;
    }
    assert!(cut > 0);
    assert_eq!(
        text(&render("- a\n- b\n\n- c\n- d", 80)),
        "   \u{2219} a\n   \u{2219} b\n\n   \u{2219} c\n   \u{2219} d"
    );
    assert_eq!(
        text(&render("- a\n  - x\n\n  more", 80)),
        "   \u{2219} a\n     \u{2219} x\n\n     more"
    );
}

#[test]
fn no_row_exceeds_tiny_widths() {
    let mut sources = corpus();
    sources.push("123456789012. big start\n123456789013. next\n\n- a\n  1000000. deep".to_owned());
    sources.push("> - 99999999. quoted big\n>   - nested".to_owned());
    for source in &sources {
        for w in 1..=12 {
            for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
                let layout = Layout::new(w, set);
                for line in Renderer::new(layout).markdown(source) {
                    let cells = width(&line_text(&line), layout.width_mode);
                    assert!(cells <= w, "{w} {set:?} {line:?}");
                }
            }
        }
    }
}

#[test]
fn bare_urls_split_by_the_parser_are_linked_whole() {
    for url in [
        "https://en.wikipedia.org/wiki/Foo_(bar)",
        "https://example.com/a*b",
        "https://example.com/a[1]",
        "https://example.com/?a=1&b=2",
        "https://example.com/a_b_c",
    ] {
        let source = match url {
            "https://example.com/?a=1&b=2" => "see https://example.com/?a=1&amp;b=2 ok".to_owned(),
            "https://example.com/a_b_c" => "see https://example.com/a\\_b\\_c ok".to_owned(),
            _ => format!("see {url} ok"),
        };
        let lines = render(&source, 120);
        let linked: Vec<&str> = lines
            .iter()
            .flat_map(|l| &l.spans)
            .filter(|s| s.look.underline)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(linked, [url], "{source}");
    }
}

fn generated(seed: u64) -> String {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut next = |n: u64| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % n
    };
    let pieces = [
        "- a",
        "  - b",
        "10. c",
        "1. d",
        "   1. e",
        "> q",
        "> > qq",
        "text *open",
        "close* text",
        "",
        "",
        "    indented",
        "| a | b |\n|---|---|\n| 1 | 2 |",
        "```\ncode\n```",
        "a \\* b \\_ c",
        "- a \\* b",
        "- x 1\\. y",
        "> p \\# q",
        "1. a \\| b",
        "```\nf() -> list[int]:\n    \n```",
        "    ",
        "\t",
        ">      ",
    ];
    let blanks = ["\n", "\n    \n", "\n\t\n", "\n>      \n", "\n      \n"];
    let indents = [
        "", "  ", "   ", "    ", "      ", "> ", ">   ", "- ", "1. ", "> - ", "  - ", "2. ",
    ];
    let mut out = String::new();
    for i in 0..(4 + next(10)) {
        if next(3) == 0 {
            let indent = indents[next(indents.len() as u64) as usize];
            out.push_str(&format!(
                "{indent}[l{}]: http://u{seed}-{i}.example\n{}",
                next(3),
                blanks[next(blanks.len() as u64) as usize]
            ));
        } else {
            out.push_str(pieces[next(pieces.len() as u64) as usize]);
            out.push('\n');
        }
    }
    out
}

#[test]
fn definition_urls_reach_the_output_and_stream_identically() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut checked = 0;
    for seed in 0..1500 {
        let source = generated(seed);
        let full = renderer.markdown(&source);
        assert_eq!(stream(&source, &renderer), full, "{source:?}");
        committed_rows_never_change(&source, &renderer);
        let joined = text(&full);
        let mut shown = 0;
        for def in definitions_in(&source) {
            assert!(
                joined.contains(&def),
                "{def} missing from {source:?}\n{joined}"
            );
            shown += 1;
        }
        checked += shown;
        for p in parse(&source) {
            assert_eq!(
                blocks(&source[p.source.clone()]),
                vec![p.block.clone()],
                "{source:?}"
            );
        }
    }
    assert!(checked > 500, "{checked}");
}

fn definitions_in(source: &str) -> Vec<String> {
    let mut parser = pulldown_cmark::Parser::new_ext(source, pulldown_cmark::Options::empty());
    for _ in parser.by_ref() {}
    let mut urls: Vec<String> = parser
        .reference_definitions()
        .iter()
        .map(|(_, d)| d.dest.to_string())
        .collect();
    urls.sort();
    urls
}

#[test]
fn reference_labels_render_as_when_undefined() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let defined = text(&renderer.markdown("[a\\_b]: https://x.example\n\n[x][a\\_b] [y][a&amp;b]"));
    let undefined = text(&renderer.markdown("[x][a\\_b] [y][a&amp;b]"));
    let line = |t: &str| t.lines().last().unwrap_or_default().to_owned();
    assert_eq!(line(&defined), line(&undefined));
    assert_eq!(line(&undefined), "   [x][a_b] [y][a&b]");
    let control = text(&renderer.markdown("[\u{1}]: https://control.example\n\n[z][\u{1}]"));
    assert!(control.contains("https://control.example"), "{control}");
    assert!(control.contains("[z][^A]"), "{control}");
}

fn definitions_document(n: usize) -> String {
    let mut out = String::with_capacity(n * 40);
    for i in 0..n {
        out.push_str(&format!("[d{i}]: https://example.com/{i}\n"));
    }
    out.push('\n');
    for i in 0..n / 10 {
        out.push_str(&format!("Text with [d{i}] and more words.\n\n"));
    }
    out
}

#[test]
fn definition_parsing_scales_linearly() {
    let sizes = [10_000, 20_000, 40_000];
    let rows: usize = parse(&definitions_document(40_000))
        .iter()
        .map(|p| match &p.block {
            Block::Definitions(rows) => rows.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(rows, 40_000);
    for n in sizes {
        let source = definitions_document(n);
        let scanned = super::parse::parse_measured(&source).1.scanned;
        assert!(
            scanned <= 3 * source.len(),
            "{n}: {scanned} of {}",
            source.len()
        );
    }
}

const ESCAPES: &[&str] = &[
    "- a \\* b\n",
    "- a \\_ b\n- c 1\\. d\n",
    "1. x \\| y \\# z\n",
    "- loose \\* one\n\n- loose \\_ two\n",
    "> quoted \\* text\n",
    "> - quoted \\* item\n",
    "plain \\* paragraph \\_ here\n",
];

const COLON_SOURCES: &[&str] = &[
    "```python\ndef f() -> list[int]:\n```\n\n",
    "`[key: string]: T`\n\n",
    "Note[^1]: here\n\n",
];

#[test]
fn escapes_never_become_definition_rows() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    for colon in COLON_SOURCES {
        for escape in ESCAPES {
            let source = format!("{colon}{escape}");
            let with = text(&renderer.markdown(&source));
            let alone = text(&renderer.markdown(escape));
            assert!(with.ends_with(&alone), "{source:?}\n{with}\n---\n{alone}");
            assert!(!with.lines().any(|l| l.trim() == "\\"), "{with}");
            assert_eq!(stream(&source, &renderer), renderer.markdown(&source));
            for p in parse(&source) {
                assert_eq!(blocks(&source[p.source.clone()]), vec![p.block.clone()]);
            }
        }
    }
}

const PANICS: &[&str] = &[
    "> - [1]: https://a.example\n> - [2]: https://b.example\n    \n",
    "Intro\n\n> - [a]: https://a.example\n\t\n\nMore text.\n",
    "- [a]: https://c.example\n      \n- b\n",
    "> - [a]: https://d.example\n>       \n> - b\n",
    "- [a]: https://e.example\n  \"t\"\n      \n- b\n",
    "> - [a]: /u\n\t\n",
    "> - [a]: /u\n \t\n",
    "> - [a]: /u\n    \n",
    "> - [a]: /u\n     \n",
    "> * [a]: /u\n\t\n",
    "> 1. [a]: /u\n\t\n",
    "> - [a]: /u\n\t\nmore\n",
    "> > - [a]: /u\n\t\n",
    "- > [a]: /u\n\t\n",
    "- - [a]: /u\n\t\n",
    "> - [a]: /u\n> \t\n",
    "> - [a]: /u\n>\t\n",
    "- [a]: /u\n\t\n",
    "> - [a]: /u\n- b\n\t\n",
    "> - x\n> - [a]: /u\n\t\n",
    "> - [a]: /u\n   \n",
    "> - [a]: /u\n  \n",
    "> 1. [a]: /u\n     \nnext\n",
    "> - [a]: /u\n- [b]: /v\n\t\n",
    "- [o]:u\n    \t",
    "> - [o]:u\n>     \t",
    "> - [o]:u\n> \u{c}",
    "+\t[\0]:d\n\u{b}",
    "- [a]:[\n\u{b}",
    "- [a]:[\n\u{c}",
    "- [o]:u\r\n    \t\r\n",
    "1. [o]:u\n\t\t",
];

#[test]
fn former_parser_panics_parse_through_the_offset_iterator() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    for source in PANICS {
        assert!(
            !parse(source)
                .iter()
                .any(|p| matches!(p.block, Block::Plain(_))),
            "{source:?}"
        );
        let full = renderer.markdown(source);
        let joined = text(&full);
        for url in [
            "https://a.example",
            "https://b.example",
            "https://c.example",
        ] {
            if source.contains(url) {
                assert!(joined.contains(url), "{url} in {joined}");
            }
        }
        assert_eq!(stream(source, &renderer), full, "{source:?}");
        committed_rows_never_change(source, &renderer);
        for p in parse(source) {
            assert_eq!(blocks(&source[p.source.clone()]), vec![p.block.clone()]);
        }
    }
    assert_eq!(
        text(&render(PANICS[1], 80)),
        "   Intro\n\n   ╎ ∙ [a]: https://a.example\n\n   More text."
    );
}

fn as_blank_lines(source: &str) -> String {
    source
        .split('\n')
        .map(|line| {
            let blank = line.bytes().all(|b| matches!(b, b' ' | b'\t' | b'>'));
            let quotes = line.bytes().filter(|b| *b == b'>').count();
            if blank && line.replace('\t', "    ").len() >= 4 + quotes {
                ">".repeat(quotes)
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn whitespace_lines_of_any_width_are_blank_lines() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut guarded = 0;
    for seed in 0..1500 {
        let source = generated(seed);
        let blanked = as_blank_lines(&source);
        if blanked != source {
            guarded += 1;
        }
        assert_eq!(
            text(&renderer.markdown(&source)),
            text(&renderer.markdown(&blanked)),
            "{source:?}"
        );
    }
    assert!(guarded > 300, "{guarded}");
}

#[test]
fn tab_indented_definitions_stay_in_their_item() {
    let renderer = Renderer::new(Layout::new(60, GlyphSet::Unicode));
    let nested = "- a\n\n\t[d]: http://d.example\n";
    assert_eq!(
        text(&renderer.markdown(nested)),
        "   ∙ a\n\n     [d]: http://d.example"
    );
    let empty = "\n-\n\t[l]: http://u.example\n  - [m]: http://m.example\n";
    for source in [nested, empty] {
        committed_rows_never_change(source, &renderer);
        assert_eq!(stream(source, &renderer), renderer.markdown(source));
        for p in parse(source) {
            assert_eq!(blocks(&source[p.source.clone()]), vec![p.block.clone()]);
        }
    }
}

#[test]
fn definition_rows_keep_continuation_lines_verbatim() {
    assert_eq!(
        text(&render("[foo\n2. bar]: /u\n", 80)),
        "   [foo\n   2. bar]: /u"
    );
    assert_eq!(
        text(&render("- [a]: /u\n  * [ ] t\n", 80)),
        text(&render("- [a]: /u\n  * [ ] t\n", 80))
    );
    assert_eq!(
        text(&render("[a]: http://a.example\n\t\n<details>\n", 80)),
        "   [a]: http://a.example\n\n   <details>"
    );
}

#[test]
fn a_lone_carriage_return_ends_a_line() {
    assert_eq!(
        text(&render("[a]: /u\r[b]: /v\r\rtext\r", 80)),
        "   [a]: /u\n   [b]: /v\n\n   text"
    );
}

#[test]
fn a_parser_panic_renders_the_source_as_plain_text() {
    let source = "# Title\n\nsome *text* \u{202E}here\n";
    let parsed = super::parse::contain(source, |_| {
        assert!(is_contained());
        panic!("parser bug")
    });
    assert_eq!(
        parsed,
        vec![Parsed {
            block: Block::Plain(vec![
                "# Title".to_owned(),
                String::new(),
                "some *text* <U+202E>here".to_owned(),
            ]),
            source: 0..source.len(),
        }]
    );
    assert!(!is_contained());
    let rows = Renderer::new(Layout::new(12, GlyphSet::Unicode)).blocks(&blocks_of(&parsed));
    assert!(rows.len() > 3);
    for row in &rows {
        assert!(width(&line_text(row), WidthMode::Normal) < 12);
        for span in &row.spans {
            if !span.text.trim().is_empty() {
                assert_eq!(span.look.fg, Some(Ink::Token(Token::TextMuted)));
            }
        }
    }
    assert!(super::parse::contain("  \n\n", |_| panic!("again")).is_empty());
}

fn blocks_of(parsed: &[Parsed]) -> Vec<Block> {
    parsed.iter().map(|p| p.block.clone()).collect()
}

#[test]
fn the_containment_flag_is_set_only_inside_a_parse() {
    assert!(!is_contained());
    let parsed = super::parse::contain("x", |s| {
        assert!(is_contained());
        parse(s)
    });
    assert_eq!(blocks_of(&parsed), blocks("x"));
    assert!(!is_contained());
}

fn deep_then_lazy(marker: &str, depth: usize, lines: usize) -> String {
    marker.repeat(depth) + &"x\n".repeat(lines)
}

#[test]
fn nesting_past_the_cap_renders_as_plain_text_with_bounded_work() {
    for marker in ["- ", "> ", "1. "] {
        let at_cap = parse(&deep_then_lazy(marker, MAX_NESTING, 3));
        assert!(!matches!(at_cap[0].block, Block::Plain(_)), "{marker:?}");
        let past = parse(&deep_then_lazy(marker, MAX_NESTING + 1, 3));
        assert!(matches!(past[0].block, Block::Plain(_)), "{marker:?}");
    }
    let wide = format!("{}- x\n", " ".repeat(MAX_PREFIX));
    assert!(matches!(parse(&wide)[0].block, Block::Plain(_)));
    for marker in ["- ", "> ", "1. "] {
        for (depth, lines) in [(10_000, 2_000), (40_000, 8_000), (MAX_NESTING, 8_000)] {
            let source = deep_then_lazy(marker, depth, lines);
            let (_, work) = super::parse::parse_measured(&source);
            assert!(work.nesting <= MAX_NESTING, "{marker:?} {depth}: {work:?}");
            assert!(
                work.scanned <= 3 * source.len(),
                "{marker:?} {depth}: {work:?}"
            );
        }
    }
}

const RAW_DEEP: &[&str] = &[
    "```bf\n>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>+.\n```\n",
    "~~~yaml\nroot:\n{indent}- leaf\n~~~\n",
    "```\n{tabs}* x\n```\n",
    "<pre>\n> > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > > >\n</pre>\n",
    "> ```\n> {indent}- leaf\n> ```\n",
    ">{indent}aligned\n",
    "{dashes}\n",
];

fn raw_deep(template: &str) -> String {
    template
        .replace("{indent}", &" ".repeat(MAX_PREFIX + 1))
        .replace("{tabs}", &"\t".repeat(MAX_NESTING + 1))
        .replace("{dashes}", &"- ".repeat(MAX_NESTING + 1))
}

#[test]
fn code_html_rules_and_indentation_never_hit_the_cap() {
    for template in RAW_DEEP {
        let source = raw_deep(template);
        assert!(
            !parse(&source)
                .iter()
                .any(|p| matches!(p.block, Block::Plain(_))),
            "{source:?}"
        );
    }
}

#[test]
fn only_the_block_that_crosses_the_cap_turns_plain() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let before = "# Title\n\nSome **bold** text.\n\n- a\n- b\n";
    let after = "\nAfter *the* deep block.\n";
    let deep = format!("{}deep\nlazy\n", "- ".repeat(MAX_NESTING + 1));
    let source = format!("{before}{deep}{after}");
    let parsed = parse(&source);
    let kinds: Vec<bool> = parsed
        .iter()
        .map(|p| matches!(p.block, Block::Plain(_)))
        .collect();
    assert_eq!(kinds, vec![false, false, false, true, false], "{parsed:?}");
    assert_eq!(blocks_of(&parsed[..3]), blocks(before));
    assert_eq!(blocks_of(&parsed[4..]), blocks(after));
    for p in &parsed {
        assert_eq!(blocks(&source[p.source.clone()]), vec![p.block.clone()]);
    }
    assert_eq!(stream(&source, &renderer), renderer.markdown(&source));
    committed_rows_never_change(&source, &renderer);
    let growing: String = (1..=MAX_NESTING + 3)
        .map(|n| format!("{}x\n", "> ".repeat(n)))
        .collect();
    let growing = format!("{before}\n{growing}{after}");
    assert_eq!(stream(&growing, &renderer), renderer.markdown(&growing));
    committed_rows_never_change(&growing, &renderer);
}

const ITEM_ESCAPES: &[&str] = &[
    "```python\nif a[0]:\n    pass\n```\n\n- \\*args and \\*\\*kwargs\n- \\_private names\n",
    "[1]: https://a.example\n\n1. \\# not a heading\n2. ok\n",
    "> - \\*args\n\n[1]: https://a.example\n",
    "- [1]: https://a.example\n\\[2] lazy line\n",
    "- \\*\n\n]:",
    "- [ref][]\n\n[ref]: https://e.com",
];

#[test]
fn escapes_opening_an_item_stay_in_the_item() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    for source in ITEM_ESCAPES {
        let full = renderer.markdown(source);
        let shown = text(&full);
        assert!(
            !shown.lines().any(|l| l.trim() == "\\" || l.trim() == "[]"),
            "{source:?}\n{shown}"
        );
        assert_eq!(stream(source, &renderer), full, "{source:?}");
        committed_rows_never_change(source, &renderer);
        for p in parse(source) {
            assert_eq!(
                blocks(&source[p.source.clone()]),
                vec![p.block.clone()],
                "{source:?}"
            );
        }
    }
    assert_eq!(
        text(&render(ITEM_ESCAPES[0], 80))
            .lines()
            .skip(3)
            .collect::<Vec<_>>(),
        vec!["   ∙ *args and **kwargs", "   ∙ _private names"]
    );
    assert_eq!(
        text(&render(ITEM_ESCAPES[1], 80))
            .lines()
            .skip(2)
            .collect::<Vec<_>>(),
        vec!["   1. # not a heading", "   2. ok"]
    );
}

#[test]
fn a_trailing_definition_never_changes_what_comes_before() {
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let bodies = ESCAPES
        .iter()
        .chain(ITEM_ESCAPES)
        .chain(TRICKY)
        .chain(PANICS)
        .map(|s| (*s).to_owned())
        .chain((0..1500).map(generated));
    for body in bodies {
        let body = body.trim_end().to_owned();
        let alone = renderer.markdown(&body);
        let with = renderer.markdown(&format!("{body}\n\n[zz]: https://z.example"));
        assert!(
            with.len() >= alone.len() && with[..alone.len()] == alone[..],
            "{body:?}\n{}\n---\n{}",
            text(&alone),
            text(&with)
        );
    }
}

const CAP_BYPASSES: &[&str] = &[
    "    ```\n",
    "-     ```\n\n",
    "- ```\n\n",
    "text\n<b>\n",
    "text\n</pre>\n",
];

#[test]
fn raw_block_lookalikes_never_lift_the_cap() {
    for prefix in CAP_BYPASSES {
        for (depth, lines) in [(10_000, 2_000), (40_000, 8_000)] {
            let source = format!("{prefix}{}{}", "> ".repeat(depth), "x\n".repeat(lines));
            let (parsed, work) = super::parse::parse_measured(&source);
            assert!(work.nesting <= MAX_NESTING, "{prefix:?} {depth}: {work:?}");
            assert!(
                work.scanned <= 3 * source.len(),
                "{prefix:?} {depth}: {work:?}"
            );
            assert!(parsed.iter().any(|p| matches!(p.block, Block::Plain(_))));
        }
    }
    let fenced = format!(
        "```\n{}x\n{}```\n",
        "> ".repeat(10_000),
        "x\n".repeat(2_000)
    );
    let (_, work) = super::parse::parse_measured(&fenced);
    assert!(work.nesting <= MAX_ABSOLUTE, "{work:?}");
    let brainfuck = format!("```\n{}+.\n```\n", ">".repeat(MAX_ABSOLUTE));
    assert!(
        !parse(&brainfuck)
            .iter()
            .any(|p| matches!(p.block, Block::Plain(_)))
    );
}

#[test]
fn text_outside_a_deep_block_never_changes_its_rendering() {
    let deep = format!("{}x", "> ".repeat(MAX_NESTING + 1));
    let renderer = Renderer::new(Layout::new(60, GlyphSet::Unicode));
    for source in [
        format!("text\n<b>\n{deep}\n\nNormal **text**.\n"),
        format!("{deep}\n\n    ```\n\nNormal **text**.\n"),
        format!("{deep}\n\n-     ```\n\nNormal **text**.\n"),
        format!("{deep}\n\n- <!-- note\n\nNormal **text**.\n"),
        format!(
            "para line\n{}x\n\nNormal **text**.\n",
            "2. ".repeat(MAX_NESTING + 1)
        ),
    ] {
        let parsed = parse(&source);
        for p in &parsed {
            assert_eq!(
                blocks(&source[p.source.clone()]),
                vec![p.block.clone()],
                "{source:?}"
            );
        }
        assert!(
            text(&renderer.markdown(&source)).ends_with("Normal text."),
            "{source:?}"
        );
        assert_eq!(
            stream(&source, &renderer),
            renderer.markdown(&source),
            "{source:?}"
        );
    }
}

fn definition_glued(source: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    lines
        .windows(2)
        .any(|w| w[0].contains("]:") && !w[1].trim().is_empty() && !w[1].contains("]:"))
}

#[test]
fn deep_spans_reparse_and_stream_identically() {
    let lines: &[&str] = &[
        "> ```sh",
        "> ls",
        "> ```",
        "> - ```",
        "para text",
        "more **bold**",
        "",
        "",
        "- item",
        "  - nested",
        "1. one",
        "2. two",
        "> quote",
        ">",
        "> - qi",
        "```",
        "~~~",
        "code",
        "    indented",
        "\tindented",
        "<div>",
        "</div>",
        "<!-- c",
        "-->",
        "<pre>",
        "</pre>",
        "Title",
        "===",
        "---",
        "| a | b |",
        "|---|---|",
        "[d]: http://d.example",
        "  continuation",
        "lazy",
        "- - -",
        "* * *",
        "<b>",
        "text <i>x</i>",
        "    ```",
        "-     ```",
        "- ```",
    ];
    let deeps = [
        format!("{}x", "> ".repeat(65)),
        format!("{}x", "- ".repeat(65)),
        format!("{}y", ">".repeat(70)),
        format!("  {}x", "1. ".repeat(66)),
        format!("{}- x", " ".repeat(260)),
        format!("{}x", "> ".repeat(64)),
    ];
    let mut rng = Rng(424_242);
    let mut checked = 0;
    for _ in 0..3000 {
        let mut source = String::new();
        let mut deep = false;
        for _ in 0..2 + rng.below(9) {
            if rng.below(4) == 0 {
                source.push_str(&deeps[rng.below(deeps.len())]);
                deep = true;
            } else {
                source.push_str(lines[rng.below(lines.len())]);
            }
            source.push('\n');
        }
        if !deep || definition_glued(&source) {
            continue;
        }
        checked += 1;
        for p in parse(&source) {
            assert_eq!(
                blocks(&source[p.source.clone()]),
                vec![p.block.clone()],
                "{source:?}"
            );
        }
        let mut finished: Vec<Block> = vec![];
        let mut open = 0;
        for (end, _) in source.match_indices('\n') {
            let cut = end + 1;
            let tail = parse(&source[open..cut]);
            if let Some((last, done)) = tail.split_last() {
                finished.extend(done.iter().map(|p| p.block.clone()));
                open += last.source.start;
            }
            let mut joined = finished.clone();
            joined.extend(blocks(&source[open..cut]));
            assert_eq!(joined, blocks(&source[..cut]), "{source:?} at {cut}");
        }
    }
    assert!(checked > 1000, "{checked}");
}

#[test]
fn fences_close_only_inside_their_container() {
    let deep = format!("{}x", "> ".repeat(MAX_NESTING + 1));
    for source in [
        format!("   ```\ncode\n{deep}\n```\n"),
        format!("> ```\n>code\n> {deep}\n> ```\n"),
        format!("- ```\n  code\n  {deep}\n  ```\n"),
    ] {
        assert!(
            !parse(&source)
                .iter()
                .any(|p| matches!(p.block, Block::Plain(_))),
            "{source:?}"
        );
    }
    for source in [
        format!("- ```\n\n{deep}\n"),
        format!("```\n    ```\n{deep}\n"),
    ] {
        let parsed = parse(&source);
        let plain = parsed.iter().any(|p| matches!(p.block, Block::Plain(_)));
        assert_eq!(plain, !source.starts_with("```"), "{source:?}");
    }
}

#[test]
fn a_blank_line_ends_a_quoted_fence() {
    let renderer = Renderer::new(Layout::new(60, GlyphSet::Unicode));
    let source = format!(
        "Intro\n\n> ```sh\n> ls\n\n{}x\n",
        ">".repeat(MAX_NESTING + 1)
    );
    let parsed = parse(&source);
    assert!(
        matches!(parsed.last().map(|p| &p.block), Some(Block::Plain(_))),
        "{parsed:?}"
    );
    for p in &parsed {
        assert_eq!(blocks(&source[p.source.clone()]), vec![p.block.clone()]);
    }
    assert_eq!(stream(&source, &renderer), renderer.markdown(&source));
    committed_rows_never_change(&source, &renderer);
}

#[test]
fn plain_rows_keep_relative_indentation() {
    let renderer = Renderer::new(Layout::new(60, GlyphSet::Unicode));
    let source = format!(
        "- a\n  {}x\n\n      def f():\n          return 1\n\nafter\n",
        "- ".repeat(MAX_NESTING + 1)
    );
    let shown = text(&renderer.markdown(&source));
    let def = shown
        .lines()
        .find(|l| l.contains("def f():"))
        .unwrap_or_default();
    let ret = shown
        .lines()
        .find(|l| l.contains("return 1"))
        .unwrap_or_default();
    let lead = |l: &str| l.len() - l.trim_start().len();
    assert_eq!(lead(ret), lead(def) + 4, "{shown}");
    assert_eq!(stream(&source, &renderer), renderer.markdown(&source));
    committed_rows_never_change(&source, &renderer);
    let narrow = Renderer::new(Layout::new(12, GlyphSet::Unicode));
    for row in narrow.markdown(&source) {
        assert!(width(&line_text(&row), WidthMode::Normal) < 12);
    }
}

#[test]
fn huge_plain_indentation_stays_bounded() {
    let source = format!(
        "{}x\n{}y\n{}z\n",
        "> ".repeat(MAX_NESTING + 1),
        " ".repeat(1 << 20),
        "\t".repeat(1 << 20)
    );
    let parsed = parse(&source);
    let Some(Block::Plain(rows)) = parsed.last().map(|p| &p.block) else {
        panic!("{:?}", parsed.len());
    };
    let leads: Vec<usize> = rows
        .iter()
        .map(|r| r.len() - r.trim_start().len())
        .collect();
    assert!(leads.iter().all(|l| *l <= PLAIN_INDENT), "{leads:?}");
    assert_eq!(leads.get(1..), Some(&[PLAIN_INDENT, PLAIN_INDENT][..]));
    let rendered = Renderer::new(Layout::new(40, GlyphSet::Unicode)).markdown(&source);
    assert!(rendered.len() <= 12, "{}", rendered.len());
    let (_, work) = super::parse::parse_measured(&source);
    assert!(work.scanned <= 3 * source.len(), "{work:?}");
}

fn reparse_or_stream_differs(source: &str) -> bool {
    if parse(source)
        .iter()
        .any(|p| blocks(&source[p.source.clone()]) != vec![p.block.clone()])
    {
        return true;
    }
    let mut cuts: Vec<usize> = source.match_indices('\n').map(|(i, _)| i + 1).collect();
    cuts.push(source.len());
    let mut finished: Vec<Block> = vec![];
    let mut open = 0;
    for cut in cuts {
        let tail = parse(&source[open..cut]);
        if let Some((last, done)) = tail.split_last() {
            finished.extend(done.iter().map(|p| p.block.clone()));
            open += last.source.start;
        }
        let mut joined = finished.clone();
        joined.extend(blocks(&source[open..cut]));
        if joined != blocks(&source[..cut]) {
            return true;
        }
    }
    false
}

const TRACKER_LEADS: &[&str] = &[
    "- foo\n===\n",
    "para\n--\n",
    "para\n===\n",
    "> foo\n===\n",
    "> - foo\n===\n",
    "> foo\n--\n",
    "para\n*\n",
    "para\n+\n",
    "para\n1.\n",
    "para\n1)\n",
    "para\n2) x\n",
    "para\n2. x\n",
    "#tag\n",
    "para\n#tag\n",
    "####### x\n",
    "<!-- c -->\n",
    "<?x y ?>\n",
    "<!DOCTYPE html>\n",
    "<![CDATA[x]]>\n",
    "<pre>x</pre>\n",
    "<script>x</script>\n",
    "para\n<!-- c -->\n",
    "- a\n  - b\n===\n",
    "para\n- \n",
    "> para\n> -\n",
    "para\n    -\n",
    "| a |\n|---|\n===\n",
    "| a |\n|---|\n--\n",
    "| a |\n|---|\n| 1 |\n===\n",
    "| a |\n|---|\n*\n",
    "| a | b |\n|---|---|\n#tag\n",
    "a\n|---|\nb\n",
    "para\n| a |\n|---|\n===\n",
    "para\n| a |\n|---|\n*\n",
    "Here is a table:\n| a | b |\n|---|---|\n| 1 | 2 |\n",
    "| a |\n    |---|\n===\n",
    "| a |\n:--\n===\n",
    "> para\n> | a |\n> |---|\n> ===\n",
];

#[test]
fn the_tracker_follows_commonmark_block_starts() {
    let renderer = Renderer::new(Layout::new(60, GlyphSet::Unicode));
    let deep = format!("{}x", ">".repeat(MAX_NESTING + 1));
    for lead in TRACKER_LEADS {
        for tag in ["<b>", "</pre>", "<span>x</span>"] {
            let source = format!("{lead}{tag}\n{deep}\n\nafter\n");
            assert!(!reparse_or_stream_differs(&source), "{source:?}");
            committed_rows_never_change(&source, &renderer);
        }
    }
}

#[test]
fn list_content_columns_stay_bounded_by_the_absolute_cap() {
    let source = format!("  - x\n\n    code\n</pre>\n{}x\n", "> ".repeat(120));
    let (_, work) = super::parse::parse_measured(&source);
    assert!(work.nesting <= MAX_ABSOLUTE, "{work:?}");
    let renderer = Renderer::new(Layout::new(60, GlyphSet::Unicode));
    committed_rows_never_change(&source, &renderer);
    for depth in [MAX_ABSOLUTE + 1, 4 * MAX_ABSOLUTE] {
        let source = format!("  - x\n\n    code\n</pre>\n{}x\n", "> ".repeat(depth));
        let (_, work) = super::parse::parse_measured(&source);
        assert!(work.nesting <= MAX_ABSOLUTE, "{depth}: {work:?}");
    }
}

fn indentation(line: &str) -> (usize, &str) {
    let rest = line.trim_start_matches([' ', '\t']);
    let column = line
        .bytes()
        .take(line.len() - rest.len())
        .fold(0, |c, b| if b == b'\t' { c + 4 - c % 4 } else { c + 1 });
    (column, rest)
}

fn item_content_column(indent: usize, rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let width = match bytes.first() {
        Some(b'-' | b'*' | b'+') => 1,
        Some(b'0'..=b'9') => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            matches!(bytes.get(digits), Some(b'.' | b')')).then_some(digits + 1)?
        }
        _ => return None,
    };
    let after = rest.get(width..)?;
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let (spaces, text) = indentation(after);
    let gap = if text.is_empty() || !(1..=4).contains(&spaces) {
        1
    } else {
        spaces
    };
    Some(indent + width + gap)
}

fn starts_block(rest: &str) -> bool {
    let hashes = rest.bytes().take_while(|b| *b == b'#').count();
    let atx = (1..=6).contains(&hashes)
        && rest
            .as_bytes()
            .get(hashes)
            .is_none_or(|b| matches!(b, b' ' | b'\t'));
    let fence = ["```", "~~~"].iter().any(|f| rest.starts_with(f));
    atx || fence || rest.starts_with('>')
}

fn reaches_list_content(source: &str) -> bool {
    let mut column: Option<usize> = None;
    let mut after_blank = false;
    for line in source.lines() {
        let (indent, rest) = indentation(line);
        if rest.is_empty() {
            after_blank = true;
            continue;
        }
        if column.is_some_and(|c| indent >= c) {
            return true;
        }
        if indent < 4
            && let Some(c) = item_content_column(indent, rest)
        {
            column = Some(c);
        } else if after_blank || starts_block(rest) {
            column = None;
        }
        after_blank = false;
    }
    false
}

const FUZZ_INPUTS: usize = 2_500;
const SOAK_INPUTS: usize = 10_000;
const EDGE_REPEATS: [usize; 8] = [65, 66, 127, 128, 129, 130, 253, 254];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sweep {
    Quick,
    Soak,
}

impl Sweep {
    fn inputs(self) -> usize {
        match self {
            Sweep::Quick => FUZZ_INPUTS,
            Sweep::Soak => SOAK_INPUTS,
        }
    }
}

fn tracker_fuzz(leads: &[&str], seed: u64, sweep: Sweep) -> (Vec<String>, usize) {
    let inputs = sweep.inputs();
    let mut rng = Rng(seed);
    let (mut differs, mut excluded) = (vec![], 0);
    for _ in 0..inputs {
        let mut source = String::new();
        for _ in 0..1 + rng.below(6) {
            source.push_str(leads[rng.below(leads.len())]);
            source.push('\n');
        }
        let unit = ["> ", ">", "- ", "> - "][rng.below(4)];
        let repeats = match sweep {
            Sweep::Quick if rng.below(3) > 0 => EDGE_REPEATS[rng.below(EDGE_REPEATS.len())],
            _ => 65 + rng.below(190),
        };
        source.push_str(&unit.repeat(repeats));
        source.push_str("x\nlazy\n");
        if !reparse_or_stream_differs(&source) {
            continue;
        }
        if reaches_list_content(&source) {
            excluded += 1;
        } else {
            differs.push(source);
        }
    }
    (differs, excluded)
}

#[test]
fn deep_lines_after_any_block_start_reparse_and_stream_identically() {
    deep_lines(Sweep::Quick);
}

#[test]
#[ignore = "soak run: 10,000 uniform depths per seed; see docs/testing.md"]
fn deep_lines_after_any_block_start_reparse_and_stream_identically_soak() {
    deep_lines(Sweep::Soak);
}

fn deep_lines(sweep: Sweep) {
    let mut leads = vec![
        "```",
        "    ```",
        "-     ```",
        "- ```",
        "> ```",
        "~~~",
        "<pre>",
        "</pre>",
        "<div>",
        "</div>",
        "<b>",
        "text",
        "<!--",
        "-->",
        "",
        "",
        "  - x",
        "> q",
        "<?x",
        "?>",
        "<![CDATA[",
        "]]>",
        "<!X",
        "<script>",
        "</script>",
        "    code",
        "\t```",
        "1. ```",
        "> - ```",
        "para <b>",
        "<span>x</span>",
        "<details>",
        "</details>",
    ];
    for seed in [99_991, 31_337] {
        let (differs, excluded) = tracker_fuzz(&leads, seed, sweep);
        assert!(differs.is_empty(), "{differs:?}");
        assert!(excluded * 100 < sweep.inputs(), "{excluded}");
    }
    leads.extend([
        "===",
        "--",
        "---",
        "*",
        "1.",
        "#tag",
        "# h",
        "<!-- c -->",
        "[a]: /u\n",
        "- foo",
        "> foo",
        "| a |",
        "|---|",
        "- - -",
        "| a | b |",
        "|---|---|",
        "| 1 |",
        "a | b",
        "-|-",
        "| 1 | 2 |",
    ]);
    for seed in [77_777, 4_242] {
        let (differs, excluded) = tracker_fuzz(&leads, seed, sweep);
        assert!(differs.is_empty(), "{differs:?}");
        assert!(excluded * 100 < sweep.inputs(), "{excluded}");
    }
}

#[test]
fn text_under_a_definition_is_pulldowns_documented_limit() {
    let starts = |source: &str| -> Vec<String> {
        pulldown_cmark::Parser::new(source)
            .filter_map(|event| match event {
                pulldown_cmark::Event::Start(tag) => Some(format!("{tag:?}")),
                _ => None,
            })
            .collect()
    };
    assert_eq!(starts("[a]: /u\n<b>\n"), vec!["Paragraph"]);
    assert_eq!(starts("<b>\n"), vec!["HtmlBlock"]);
    let source = format!("[a]: /u\n<b>\n{}x\n", ">".repeat(MAX_NESTING + 1));
    assert!(reparse_or_stream_differs(&source));
    let (_, work) = super::parse::parse_measured(&source);
    assert!(work.nesting <= MAX_ABSOLUTE);
    committed_rows_never_change(&source, &Renderer::new(Layout::new(60, GlyphSet::Unicode)));
}

#[test]
fn a_table_may_interrupt_a_paragraph() {
    let parsed = blocks("Here is a table:\n| a | b |\n|---|---|\n| 1 | 2 |\n");
    assert!(
        matches!(parsed.as_slice(), [Block::Paragraph(_), Block::Table(_)]),
        "{parsed:?}"
    );
    let deep = format!("{}x", ">".repeat(MAX_NESTING + 1));
    for lead in ["para\n| a |\n|---|\n===\n", "para\n| a |\n|---|\n*\n"] {
        let source = format!("{lead}<b>\n{deep}\n\nafter\n");
        assert!(!reparse_or_stream_differs(&source), "{source:?}");
    }
}

#[test]
fn a_panicking_mark_judgement_reads_as_no_and_restores_containment() {
    assert!(!is_contained());
    assert_eq!(guarded(|| -> bool { panic!("a parser bug") }), None);
    assert!(!is_contained());
    assert_eq!(guarded(|| true), Some(true));
    assert_eq!(guarded(is_contained), Some(true));
}

#[test]
fn a_block_takes_a_mark_only_in_plain_paragraph_or_item_text() {
    let block_takes_mark =
        |block: &str, at: usize, mark: &str| block_takes_marks(block, &[(at, mark.to_owned())]);
    let end = |text: &str| block_takes_mark(text, text.len(), "\\[1\\]");
    for text in [
        "Plain prose",
        "- an item",
        "use `rg`",
        "**bold**",
        "1. first\n2. second",
    ] {
        assert!(end(text), "{text:?}");
    }
    for text in [
        "```sh\nrg --files\n```",
        "    indented code",
        "| a | b |\n|---|---|\n| x | y |",
        "Title\n---",
        "---",
        "## Head ##",
        "<div>\nhtml content\n</div>",
        "Read https://docs.example/page",
        "",
    ] {
        assert!(!end(text), "{text:?}");
    }
    assert!(block_takes_mark("ripgrep is fast. More.", 15, "\\[1\\]"));
    assert!(!block_takes_mark(
        "Run `cargo build --release` now.",
        16,
        "\\[1\\]"
    ));
    assert!(!block_takes_mark(
        "See [the docs](https://x.example)",
        13,
        "\\[1\\]"
    ));
}

#[test]
fn a_block_takes_all_its_marks_at_once_in_order() {
    let block = "One claim. Two claim. Three.";
    let marks = vec![(9, "\\[1\\]".to_owned()), (20, "\\[2\\]".to_owned())];
    assert!(block_takes_marks(block, &marks));
    let code = "Run `cargo build --release` now.";
    assert!(!block_takes_marks(
        code,
        &[(16, "\\[1\\]".to_owned()), (31, "\\[2\\]".to_owned())]
    ));
}

#[test]
fn list_items_are_the_top_level_items_only() {
    let list = "- one\n  - nested\n- two\n\n- three\n";
    assert_eq!(list_items(list), Some(vec![0, 17, 24]));
}

#[test]
fn diff_fences_keep_their_whole_info_string() {
    let lang = |source: &str| match blocks(source).first() {
        Some(Block::Code { lang, .. }) => lang.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        lang("```diff  src/x.rs\n+a\n```").as_deref(),
        Some("diff src/x.rs")
    );
    assert_eq!(
        lang("```patch rust\n+a\n```").as_deref(),
        Some("patch rust")
    );
    assert_eq!(
        lang("```diff-hunks py\n+a\n```").as_deref(),
        Some("diff-hunks py")
    );
    assert_eq!(
        lang("```rust ignore\nfn a() {}\n```").as_deref(),
        Some("rust")
    );
    assert_eq!(diff_hint(Some("diff src/x.rs")), Some("src/x.rs"));
    assert_eq!(diff_hint(Some("diff")), None);
    assert_eq!(diff_kind(Some("DIFF rust")), Some(DiffKind::Full));
    assert_eq!(diff_kind(Some("diff-hunks")), Some(DiffKind::Hunks));
    assert_eq!(diff_kind(Some("diffs")), None);
    let rows = render("```diff rust\n+let a = 1;\n```", 80);
    assert!(
        line_text(&rows[0]).ends_with("diff rust"),
        "{:?}",
        line_text(&rows[0])
    );
}
