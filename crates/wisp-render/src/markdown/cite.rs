use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::ir::{Block, Child, Inline, Inlines, Item, Parsed};
use super::parse::{guarded, options, parse};

pub(crate) fn parse_contained(source: &str) -> Option<Vec<Parsed>> {
    guarded(|| parse(source))
}

pub(crate) fn block_takes_marks(block: &str, marks: &[(usize, String)]) -> bool {
    guarded(|| takes_marks(block, marks)).unwrap_or(false)
}

pub(crate) fn list_items(list: &str) -> Option<Vec<usize>> {
    guarded(|| {
        let mut depth = 0usize;
        let mut starts = vec![];
        for (event, range) in Parser::new_ext(list, options()).into_offset_iter() {
            match event {
                Event::Start(Tag::List(_)) => depth += 1,
                Event::End(TagEnd::List(_)) => depth = depth.saturating_sub(1),
                Event::Start(Tag::Item) if depth == 1 => starts.push(range.start),
                _ => {}
            }
        }
        starts
    })
}

fn stand_ins(block: &str, count: usize) -> Vec<String> {
    let longest = block
        .split(|c: char| !c.is_ascii_digit())
        .map(str::len)
        .max()
        .unwrap_or(0);
    let width = (longest + 1).max(count.to_string().len());
    (0..count).map(|k| format!("{k:0>width$}")).collect()
}

fn takes_marks(block: &str, marks: &[(usize, String)]) -> bool {
    let numbers = stand_ins(block, marks.len());
    let mut with = String::with_capacity(block.len() + marks.len() * 16);
    let mut from = 0;
    for ((at, _), number) in marks.iter().zip(&numbers) {
        let Some(piece) = block.get(from..*at) else {
            return false;
        };
        with.push_str(piece);
        with.push_str(&format!("\\[{number}\\]"));
        from = *at;
    }
    with.push_str(block.get(from..).unwrap_or_default());
    let plains: Vec<String> = numbers.iter().map(|n| format!("[{n}]")).collect();
    let before = parse(block);
    let after = parse(&with);
    let ([before], [after]) = (before.as_slice(), after.as_slice()) else {
        return false;
    };
    let mut next = 0;
    let stripped = strip_block(after.block.clone(), &plains, &mut next);
    next == plains.len() && merged_block(stripped) == merged_block(before.block.clone())
}

fn strip_block(block: Block, plains: &[String], next: &mut usize) -> Block {
    match block {
        Block::Paragraph(inlines) => Block::Paragraph(strip_inlines(inlines, plains, next)),
        Block::List { start, items } => Block::List {
            start,
            items: items
                .into_iter()
                .map(|item| Item {
                    spaced: item.spaced,
                    blocks: item
                        .blocks
                        .into_iter()
                        .map(|child| Child {
                            spaced: child.spaced,
                            block: strip_block(child.block, plains, next),
                        })
                        .collect(),
                })
                .collect(),
        },
        other => other,
    }
}

fn strip_inlines(inlines: Inlines, plains: &[String], next: &mut usize) -> Inlines {
    let mut out = Inlines::new();
    for inline in inlines {
        match inline {
            Inline::Text { mut text, marks } if !marks.code && !marks.link => {
                let mut cursor = 0;
                while let Some(plain) = plains.get(*next) {
                    let Some(found) = text.get(cursor..).and_then(|t| t.find(plain.as_str()))
                    else {
                        break;
                    };
                    let at = cursor + found;
                    text.replace_range(at..at + plain.len(), "");
                    cursor = at;
                    *next += 1;
                }
                if !text.is_empty() {
                    out.push(Inline::Text { text, marks });
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn merged_inlines(inlines: Inlines) -> Inlines {
    let mut out = Inlines::new();
    for inline in inlines {
        match (out.last_mut(), inline) {
            (
                Some(Inline::Text { text, marks }),
                Inline::Text {
                    text: more,
                    marks: next,
                },
            ) if *marks == next => text.push_str(&more),
            (_, inline) => out.push(inline),
        }
    }
    out
}

fn merged_block(block: Block) -> Block {
    match block {
        Block::Paragraph(inlines) => Block::Paragraph(merged_inlines(inlines)),
        Block::List { start, items } => Block::List {
            start,
            items: items
                .into_iter()
                .map(|item| Item {
                    spaced: item.spaced,
                    blocks: item
                        .blocks
                        .into_iter()
                        .map(|child| Child {
                            spaced: child.spaced,
                            block: merged_block(child.block),
                        })
                        .collect(),
                })
                .collect(),
        },
        other => other,
    }
}
