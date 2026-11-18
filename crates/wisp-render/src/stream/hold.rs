use super::Work;
use crate::markdown::{Block, Inline, Renderer};

fn flanked(before: Option<char>, after: Option<char>) -> bool {
    let space = |c: Option<char>| c.is_none_or(char::is_whitespace);
    !(space(before) && space(after))
}

fn intraword(before: Option<char>, after: Option<char>) -> bool {
    before.is_some_and(char::is_alphanumeric) && after.is_some_and(char::is_alphanumeric)
}

pub(super) fn unclosed(inlines: &[Inline]) -> bool {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text: t, marks } if !marks.code => text.push_str(t),
            Inline::Text { .. } => text.push('x'),
            Inline::Target(_) => text.push(' '),
            Inline::Break => text.push('\n'),
        }
    }
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, &c)| {
        let before = i.checked_sub(1).and_then(|j| chars.get(j)).copied();
        let after = chars.get(i + 1).copied();
        match c {
            '`' | '[' | '<' => true,
            '_' => flanked(before, after) && !intraword(before, after),
            '*' | '~' => flanked(before, after),
            _ => false,
        }
    })
}

fn held(block: &Block) -> bool {
    match block {
        Block::Table(_) | Block::List { .. } | Block::Definitions(_) => true,
        Block::Paragraph(inlines) => unclosed(inlines),
        Block::Quote(children) => children.last().is_some_and(held),
        Block::Heading { .. } | Block::Code { .. } | Block::Rule | Block::Plain(_) => false,
    }
}

fn rows(block: &Block, r: &Renderer<'_>, work: &mut Work) -> usize {
    let n = r.block(block).len();
    work.renders += 1;
    work.rendered_rows += n;
    n
}

pub(super) fn cap(block: &Block, r: &Renderer<'_>, work: &mut Work) -> Option<usize> {
    match block {
        Block::Table(_) | Block::Definitions(_) => Some(0),
        Block::Paragraph(inlines) => unclosed(inlines).then_some(0),
        Block::List { start, items } => match items.split_last() {
            Some((_, settled)) if !settled.is_empty() => Some(rows(
                &Block::List {
                    start: *start,
                    items: settled.to_vec(),
                },
                r,
                work,
            )),
            _ => Some(0),
        },
        Block::Quote(children) => match children.split_last() {
            Some((last, settled)) if held(last) => {
                if settled.is_empty() {
                    Some(0)
                } else {
                    Some(rows(&Block::Quote(settled.to_vec()), r, work))
                }
            }
            _ => None,
        },
        Block::Heading { .. } | Block::Code { .. } | Block::Rule | Block::Plain(_) => None,
    }
}

pub(super) fn ends_with_table(list: &[Block]) -> bool {
    match list.last() {
        Some(Block::Table(_)) => true,
        Some(Block::List { items, .. }) => items.last().is_some_and(|item| {
            let children: Vec<Block> = item.blocks.iter().map(|c| c.block.clone()).collect();
            ends_with_table(&children)
        }),
        Some(Block::Quote(children)) => ends_with_table(children),
        _ => false,
    }
}
