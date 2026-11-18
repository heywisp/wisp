use wisp_protocol::ChatRow;
use wisp_protocol::filter::Query;

use super::composer::{Edge, border, edge, frame_inner, frame_row};
use super::config_screen::filter_spans;
use super::facts::short_model;
use super::menu::{MENU_MAX_ROWS, key_row, more_span, name_spans, window};
use super::one_line;
use super::row::{KEY_ROW, KeyHint, clip, cut_at_word};
use super::summary::{down_arrow, up_arrow};
use super::welcome::frame_for;
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{spans_width, width, wrap_words};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatsPlace {
    Start,
    InChat(Joins),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Joins {
    pub above: bool,
    pub below: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatsView<'a> {
    pub chats: Vec<&'a ChatRow>,
    pub listed: &'a [ChatRow],
    pub focus: Option<usize>,
    pub query: &'a str,
    pub loading: bool,
    pub below: Vec<String>,
    pub place: ChatsPlace,
}

const GAP: usize = 2;
const FIXED_ROWS: usize = 4;
const NAME_MIN: usize = 30;
const NAME_MAX: usize = 40;
const DROPS: usize = 3;

#[derive(Clone, Copy, Debug)]
struct Columns {
    name: usize,
    time: usize,
    drops: usize,
}

fn marked(text: &str, query: &Query, look: Look) -> Vec<StyledSpan> {
    name_spans(text, &query.found(text), look)
}

fn tone(chat: &ChatRow, token: Token) -> Look {
    Look::fg(if !chat.can_open() {
        Token::TextFaint
    } else {
        token
    })
}

fn ends(
    left: Vec<StyledSpan>,
    right: Vec<StyledSpan>,
    room: usize,
    layout: Layout,
) -> Vec<StyledSpan> {
    let mode = layout.width_mode;
    let right_w = spans_width(&right, mode);
    let gap = if right.is_empty() { 0 } else { GAP };
    let more = Glyphs::of(layout.glyphs).more;
    let left = clip(left, room.saturating_sub(right_w + gap), more, mode);
    let pad = room.saturating_sub(spans_width(&left, mode) + right_w);
    let mut out = left;
    out.push(StyledSpan::plain(" ".repeat(pad)));
    out.extend(right);
    out
}

pub fn title_of(chat: &ChatRow) -> String {
    one_line(chat.name.as_deref().unwrap_or(&chat.topic))
}

fn active_at(chat: &ChatRow) -> String {
    if chat.hosted {
        copy::KEPT_OPEN.to_owned()
    } else {
        one_line(&chat.active_at)
    }
}

fn columns(listed: &[ChatRow], room: usize, layout: Layout) -> Columns {
    let mode = layout.width_mode;
    let longest = |cells: &dyn Fn(&ChatRow) -> usize| listed.iter().map(cells).max().unwrap_or(0);
    let time = longest(&|chat| width(&active_at(chat), mode));
    let named = longest(&|chat| width(&title_of(chat), mode)).min(NAME_MAX);
    let all = Query::new("");
    let wide = |drops: usize| {
        longest(&|chat| {
            if !chat.can_open() {
                return 0;
            }
            spans_width(&details(chat, &all, drops, layout), mode)
        })
    };
    let spare = |details: usize| room.saturating_sub(GAP + details + GAP + time);
    let drops = (0..DROPS)
        .find(|&drops| spare(wide(drops)) >= named.min(NAME_MIN))
        .unwrap_or(DROPS);
    Columns {
        name: spare(wide(drops)).min(named),
        time,
        drops,
    }
}

fn details(chat: &ChatRow, query: &Query, drops: usize, layout: Layout) -> Vec<StyledSpan> {
    let glyphs = Glyphs::of(layout.glyphs);
    let muted = Look::fg(Token::TextMuted);
    let account = chat
        .account
        .as_deref()
        .filter(|_| drops < 1)
        .map(|name| marked(&one_line(name), query, muted));
    let agents = (drops < 2 && chat.agents > 0).then(|| {
        vec![StyledSpan::new(
            copy::agents_count(usize::try_from(chat.agents).unwrap_or(usize::MAX)),
            muted,
        )]
    });
    let model = chat
        .model
        .as_deref()
        .filter(|_| drops < 3)
        .map(|model| marked(&short_model(model), query, muted));
    let mut spans = vec![match chat.mark {
        Some(_) => super::account::mark(chat.mark, &glyphs),
        None => super::account::mark_gap(),
    }];
    for (n, item) in [account, model, agents].into_iter().flatten().enumerate() {
        if n > 0 {
            let sep = format!(" {} ", glyphs.separator);
            spans.push(StyledSpan::new(sep, Token::TextFaint));
        }
        spans.extend(item);
    }
    if let [lead] = spans.as_mut_slice() {
        lead.text = lead.text.trim_end().to_owned();
    }
    spans
}

fn row(
    chat: &ChatRow,
    query: &Query,
    cols: Columns,
    room: usize,
    layout: Layout,
) -> Vec<StyledSpan> {
    let mode = layout.width_mode;
    let name = marked(&title_of(chat), query, tone(chat, Token::Text));
    if !chat.can_open() {
        let right = StyledSpan::new(copy::OPEN_ELSEWHERE, tone(chat, Token::TextMuted));
        let fits = room.saturating_sub(width(&right.text, mode) + GAP);
        let name = cut_at_word(name, fits.min(cols.name), layout);
        return ends(name, vec![right], room, layout);
    }
    let mut left = cut_at_word(name, cols.name, layout);
    let pad = cols.name.saturating_sub(spans_width(&left, mode)) + GAP;
    left.push(StyledSpan::plain(" ".repeat(pad)));
    left.extend(details(chat, query, cols.drops, layout));
    let time = active_at(chat);
    let lead = " ".repeat(cols.time.saturating_sub(width(&time, mode)));
    let time = vec![StyledSpan::new(
        lead + &time,
        if chat.hosted {
            Token::Accent
        } else {
            Token::TextMuted
        },
    )];
    ends(left, time, room, layout)
}

pub(super) fn framed(body: Vec<StyledSpan>, focused: bool, layout: Layout) -> StyledLine {
    let frame = frame_for(layout.glyphs);
    let glyphs = Glyphs::of(layout.glyphs);
    let side = StyledSpan::new(frame.down, Token::Line);
    let mark = if focused {
        StyledSpan::new(glyphs.band, Token::Accent)
    } else {
        StyledSpan::plain(" ")
    };
    let pad = inner_room(layout).saturating_sub(spans_width(&body, layout.width_mode));
    let mut inner = vec![StyledSpan::plain(" "), mark, StyledSpan::plain(" ")];
    inner.extend(body);
    inner.push(StyledSpan::plain(" ".repeat(pad + 1)));
    if focused {
        for span in &mut inner {
            span.look = super::band::selected_look(span.look, &span.text);
        }
    }
    let mut spans = vec![side.clone()];
    spans.extend(inner);
    spans.push(side);
    spans.retain(|s| !s.text.is_empty());
    StyledLine::new(spans)
}

pub(super) fn inner_room(layout: Layout) -> usize {
    let side = width(frame_for(layout.glyphs).down, layout.width_mode);
    layout.width.saturating_sub(2 * side + 4)
}

pub(super) fn top(title: Vec<StyledSpan>, joined: bool, layout: Layout) -> StyledLine {
    let at = if joined { Edge::Divider } else { Edge::Top };
    border(at, title, None, layout).0
}

pub(super) fn rule(layout: Layout) -> StyledLine {
    let frame = frame_for(layout.glyphs);
    let mode = layout.width_mode;
    let used = width(frame.rule_left, mode) + width(frame.rule_right, mode);
    StyledLine::new(vec![
        StyledSpan::new(frame.rule_left, Token::Line),
        StyledSpan::new(edge("", frame.dotted, "", used, layout), Token::TextFaint),
        StyledSpan::new(frame.rule_right, Token::Line),
    ])
}

pub(super) fn bottom(joined: bool, layout: Layout) -> StyledLine {
    let at = if joined { Edge::Divider } else { Edge::Bottom };
    border(at, vec![], None, layout).0
}

fn keys(view: &ChatsView<'_>) -> Vec<KeyHint> {
    let openable = view
        .focus
        .and_then(|i| view.chats.get(i))
        .is_some_and(|chat| !chat.open_elsewhere);
    let esc = match view.place {
        _ if !view.query.is_empty() => copy::CLEAR_WORD,
        ChatsPlace::Start => copy::QUIT_WORD,
        ChatsPlace::InChat(_) => copy::CLOSE_WORD,
    };
    [
        openable.then(|| KeyHint::new(copy::ENTER_KEY, copy::RESUME_WORD)),
        Some(copy::esc(esc)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn fits(room: usize) -> usize {
    room.saturating_sub(FIXED_ROWS)
}

fn slots(len: usize, lines: usize) -> (usize, bool) {
    if len <= lines {
        return (len, false);
    }
    match lines.saturating_sub(2) {
        0 => (lines, false),
        steady => (steady, true),
    }
}

pub fn chats_fit(room: usize) -> bool {
    fits(room) > 0
}

pub fn chats_whole(room: usize, below: &[String], joins: Joins, layout: Layout) -> bool {
    chats_fit(room.saturating_sub(notes(below, joins, layout).iter().map(Vec::len).sum()))
}

fn tail(joins: Joins, layout: Layout) -> Layout {
    if joins.below {
        Layout {
            width: frame_inner(layout).width.saturating_sub(2),
            ..layout
        }
    } else {
        layout
    }
}

fn in_tail(line: StyledLine, joins: Joins, layout: Layout) -> StyledLine {
    if !joins.below {
        return line;
    }
    let mut spans = vec![StyledSpan::plain(" ")];
    spans.extend(line.spans);
    frame_row(StyledLine::new(spans), layout)
}

fn notes(below: &[String], joins: Joins, layout: Layout) -> Vec<Vec<StyledLine>> {
    let tail = tail(joins, layout);
    let note = |text: &str| {
        let spans = [StyledSpan::new(text, Token::TextMuted)];
        wrap_words(&spans, tail.width.saturating_sub(GAP), tail.width_mode)
            .into_iter()
            .map(|line| {
                let mut spans = vec![StyledSpan::plain(" ".repeat(GAP))];
                spans.extend(line.spans);
                in_tail(StyledLine::new(spans), joins, layout)
            })
            .collect()
    };
    below.iter().map(|text| note(text)).collect()
}

pub fn chats_lines(view: &ChatsView<'_>, room: usize, layout: Layout) -> Vec<StyledLine> {
    if !chats_fit(room) {
        let no_room = StyledLine::new(vec![StyledSpan::new(
            copy::PICKER_NO_ROOM,
            Token::TextMuted,
        )]);
        return std::iter::once(no_room).take(room).collect();
    }
    let room_w = inner_room(layout);
    let joins = match view.place {
        ChatsPlace::Start => Joins::default(),
        ChatsPlace::InChat(joins) => joins,
    };
    let below = notes(&view.below, joins, layout);
    let title = vec![StyledSpan::new(copy::RESUME_TITLE, Token::Text)];
    let placeholder = copy::type_to_filter();
    let editing = !view.query.is_empty();
    let (filter, _) = filter_spans(view.query, editing, &placeholder, frame_inner(layout));
    let mut out = vec![
        top(title, joins.above, layout),
        frame_row(StyledLine::new(filter), layout),
    ];
    if view.chats.is_empty() && !view.loading {
        out.push(framed(
            vec![StyledSpan::new(copy::NOTHING_MATCHES, Token::TextMuted)],
            false,
            layout,
        ));
    }
    let glyphs = Glyphs::of(layout.glyphs);
    let noted = below.iter().map(Vec::len).sum();
    let list_room = room.saturating_sub(noted).max(FIXED_ROWS + 1);
    let lines = match view.place {
        ChatsPlace::Start => fits(list_room),
        ChatsPlace::InChat(_) => fits(list_room).min(MENU_MAX_ROWS),
    };
    let (rows, steady) = slots(view.chats.len(), lines);
    let shown = window(view.chats.len(), rows, view.focus.unwrap_or(0), steady);
    let more = |arrow: &str, n: usize| match n {
        0 => framed(Vec::new(), false, layout),
        n => framed(vec![more_span(arrow, n)], false, layout),
    };
    if steady {
        out.push(more(up_arrow(&glyphs), shown.above));
    }
    let query = Query::new(view.query);
    let cols = columns(view.listed, room_w, layout);
    for (n, chat) in view
        .chats
        .iter()
        .enumerate()
        .skip(shown.first)
        .take(shown.shown)
    {
        let body = row(chat, &query, cols, room_w, layout);
        out.push(framed(body, view.focus == Some(n), layout));
    }
    if steady {
        out.push(more(down_arrow(&glyphs), shown.below));
    }
    out.push(bottom(joins.below, layout));
    let mut kept = room.saturating_sub(out.len() + 1);
    for note in below {
        if note.len() > kept {
            break;
        }
        kept -= note.len();
        out.extend(note);
    }
    out.push(in_tail(
        key_row(
            &keys(view),
            "",
            None,
            KEY_ROW,
            &" ".repeat(GAP),
            tail(joins, layout),
        ),
        joins,
        layout,
    ));
    out.truncate(room);
    out
}

#[cfg(any(test, feature = "testing"))]
pub fn mockup_chat(id: &str, name: &str, account: &str, model: &str, active_at: &str) -> ChatRow {
    ChatRow {
        id: id.to_owned(),
        name: Some(name.to_owned()),
        topic: name.to_owned(),
        typed: String::new(),
        active_at: active_at.to_owned(),
        account: Some(account.to_owned()),
        mark: super::account::mockup_mark(account),
        model: Some(model.to_owned()),
        agents: 0,
        hosted: false,
        open_elsewhere: false,
    }
}

#[cfg(any(test, feature = "testing"))]
pub fn mockup_chats() -> Vec<ChatRow> {
    let (opus, sonnet) = ("claude-opus-5-5", "claude-sonnet-5-5");
    let about = |topic: &str, chat: ChatRow| ChatRow {
        topic: topic.to_owned(),
        ..chat
    };
    let usage = "Make /usage show each Claude account's own limits";
    let review = "Review the sign-in MR and fix what the reviewers find";
    let cache = "Why does the cache miss after compacting a resumed chat?";
    let notes = "Draft the release notes for 0.1.0";
    let trim = "Trim the usage page so every row fits at 40 columns";
    let model = "Work out how /model groups models under their account, and where the effort step goes when a model doesn't offer one";
    let subscription = "claude-subscription";
    vec![
        ChatRow {
            hosted: false,
            open_elsewhere: true,
            ..about(
                usage,
                mockup_chat("a", "Per-account limits", subscription, opus, "12m ago"),
            )
        },
        ChatRow {
            agents: 2,
            ..about(
                review,
                mockup_chat("b", "Sign-in MR review", subscription, opus, "2h ago"),
            )
        },
        ChatRow {
            typed: format!("{cache}\nCheck whether a retry writes the cache again."),
            ..about(
                cache,
                mockup_chat("c", "Cache miss on resume", "claude-api", sonnet, "5h ago"),
            )
        },
        about(
            notes,
            mockup_chat(
                "d",
                "Release notes 0.1.0",
                "chatgpt",
                "gpt-6.1-sol",
                "16h ago",
            ),
        ),
        ChatRow {
            account: None,
            mark: None,
            ..about(
                trim,
                mockup_chat("e", "Narrow usage page", "", opus, "1d ago"),
            )
        },
        ChatRow {
            name: None,
            ..about(model, mockup_chat("f", "", "claude-work", opus, "1d ago"))
        },
    ]
}

#[cfg(any(test, feature = "testing"))]
pub fn mockup_older_chats() -> Vec<ChatRow> {
    let (opus, sonnet, work, api) = (
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-work",
        "claude-api",
    );
    vec![
        ChatRow {
            agents: 1,
            typed: "The reconnect test fails now and then.\nAdd a retry with a short backoff."
                .to_owned(),
            ..mockup_chat("g", "Flaky reconnect test", work, opus, "2d ago")
        },
        mockup_chat("h", "Diff theme tokens", api, sonnet, "2d ago"),
        mockup_chat("i", "Sidebar width at 120", work, opus, "3d ago"),
        mockup_chat(
            "j",
            "Retry budget wording",
            "chatgpt",
            "gpt-6.1-sol",
            "3d ago",
        ),
        mockup_chat(
            "k",
            "Compaction cost line",
            "claude-subscription",
            sonnet,
            "4d ago",
        ),
        mockup_chat("l", "Login card copy", work, opus, "5d ago"),
        mockup_chat("m", "Manifest validation", api, sonnet, "6d ago"),
        mockup_chat("n", "Theme doc sync", work, opus, "6d ago"),
        mockup_chat("o", "Keychain fallback", api, sonnet, "7d ago"),
        mockup_chat("p", "Doctor output", work, opus, "7d ago"),
    ]
}
