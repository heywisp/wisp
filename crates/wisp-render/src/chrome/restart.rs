use wisp_protocol::{Unfinished, Unready, short_id};

use super::one_line;
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{fit_end, padded, spans_width, width, wrap_plain};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy::{ABOUT_TO_RESTART, RESUME_COMMAND, duration};

const IN_WINDOW: &str = "in a window";
const OLDER_VERSION: &str = "older version";
const AGENTS_WAIT: &str = "Its agents wait for you.";
const SEND_MESSAGE_AGAIN: Token = Token::Warn;
const AGENTS_WAITING: Token = Token::Warn;
const OPEN_AGAIN: &str = "Open it again with:";

struct Singular {
    older_intro: &'static str,
    older_stop: &'static str,
    older_question: &'static str,
    cancelled: &'static str,
    its_window: &'static str,
    one_window: &'static str,
}

const SINGULAR: Singular = Singular {
    older_intro: "This chat runs an older version of Wisp that can't wait for work to finish:",
    older_stop: "Restarting it stops what it is doing; agents at work carry on afterwards.",
    older_question: "Restart it on the newest version?",
    cancelled: "Cancelled: the other chat carries on as it was.",
    its_window: "Its window closes; it prints the command that opens it again.",
    one_window: "One window closes; it prints the command that opens it again.",
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub id: String,
    pub title: String,
    pub window: bool,
    pub older: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Finishing(Vec<Unready>),
    Restarting,
    Restarted(Vec<Unready>),
    NotAnswering,
    NoAnswer,
    Cancelled,
    InAnotherRestart,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chat {
    pub kept: Kept,
    pub state: State,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Finishing,
    Stopped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Phrase {
    pub spans: Vec<StyledSpan>,
    pub time: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct End {
    pub cancelled: bool,
    pub waited: Option<u64>,
    pub older: Vec<String>,
    pub daemon: bool,
}

fn cut(text: &str, room: usize, layout: Layout) -> String {
    let glyphs = Glyphs::of(layout.glyphs);
    let text = glyphs.fold(&one_line(text));
    let clipped = fit_end(&text, room, glyphs.more, layout.width_mode);
    if width(&text, layout.width_mode) > room {
        format!(
            "{}{}",
            clipped
                .strip_suffix(glyphs.more)
                .unwrap_or_default()
                .trim_end(),
            if clipped.is_empty() { "" } else { glyphs.more }
        )
    } else {
        clipped
    }
}

pub fn phrase(open: &[Unready], phase: Phase, room: usize, layout: Layout) -> Phrase {
    let stopped = phase == Phase::Stopped;
    if stopped && open.iter().any(|row| row.opening) {
        return words("send your message again", SEND_MESSAGE_AGAIN);
    }
    let Some(row) = open.iter().max_by_key(|row| row.running_ms) else {
        return words("restarting", Token::Accent);
    };
    let kind = match row.what {
        Unfinished::Reply => "a reply",
        Unfinished::ToolCall => "a tool call",
        Unfinished::Process => "a process",
        Unfinished::Other => "something",
    };
    let description = row
        .title
        .as_deref()
        .filter(|_| matches!(row.what, Unfinished::ToolCall | Unfinished::Process))
        .map(one_line)
        .filter(|title| !title.is_empty());
    let more = if open.len() > 1 {
        format!(" +{} more", open.len() - 1)
    } else {
        String::new()
    };
    let lead = if stopped {
        "stopped: "
    } else if description.is_some() {
        "finishing: "
    } else {
        "finishing "
    };
    let label = description.as_ref().map_or_else(
        || kind.to_owned(),
        |description| {
            cut(
                description,
                room.saturating_sub(
                    width(lead, layout.width_mode) + width(&more, layout.width_mode),
                ),
                layout,
            )
        },
    );
    let mut spans = vec![StyledSpan::new(format!("{lead}{label}"), Token::Text)];
    if !more.is_empty() {
        spans.push(StyledSpan::new(more, Token::TextMuted));
    }
    Phrase {
        spans,
        time: Some(duration(row.running_ms / 1000 * 1000)),
    }
}

pub fn window_phrase(open: &[Unready], room: usize, layout: Layout) -> Vec<StyledSpan> {
    let glyphs = Glyphs::of(layout.glyphs);
    let lead = format!("{ABOUT_TO_RESTART} {} ", glyphs.separator);
    for with_time in [true, false] {
        let mut detail = phrase(open, Phase::Finishing, room, layout);
        if !with_time {
            detail.time = None;
        }
        let mut spans = vec![StyledSpan::new(lead.clone(), Token::Text)];
        spans.extend(detail.spans);
        if let Some(time) = detail.time {
            spans.push(StyledSpan::new(
                format!(" {} {time}", glyphs.separator),
                Token::TextMuted,
            ));
        }
        if spans_width(&spans, layout.width_mode) <= room {
            return spans;
        }
    }
    vec![StyledSpan::new(ABOUT_TO_RESTART, Token::Text)]
}

fn words(text: &str, token: Token) -> Phrase {
    Phrase {
        spans: vec![StyledSpan::new(text, token)],
        time: None,
    }
}

fn detail(chat: &Chat, room: usize, layout: Layout) -> Phrase {
    match &chat.state {
        State::Finishing(open) => phrase(open, Phase::Finishing, room, layout),
        State::Restarting => phrase(&[], Phase::Finishing, room, layout),
        State::Restarted(_) if chat.kept.older => words(OLDER_VERSION, Token::Text),
        State::Restarted(open) if !open.is_empty() => phrase(open, Phase::Stopped, room, layout),
        State::NotAnswering => words("not answering", Token::Text),
        State::NoAnswer => words("no answer, not restarted", Token::Text),
        State::InAnotherRestart => words("in another restart", Token::Text),
        State::Failed(_) => words("didn't start again", Token::Error),
        State::Restarted(_) | State::Cancelled => Phrase::default(),
    }
}

fn mark(chat: &Chat, glyphs: Glyphs) -> (&str, Token) {
    match chat.state {
        State::Restarted(_) if !chat.kept.older => (glyphs.done, Token::Ok),
        State::Failed(_) => (glyphs.failed, Token::Error),
        State::Finishing(_) | State::Restarting | State::NotAnswering => {
            (glyphs.running, Token::Accent)
        }
        _ => (glyphs.minus, Token::TextMuted),
    }
}

fn line(text: impl Into<String>) -> StyledLine {
    let text: String = text.into();
    StyledLine::new(vec![StyledSpan::plain(text.replace('\u{a0}', " "))])
}

fn toned(mut lines: Vec<StyledLine>, token: Token) -> Vec<StyledLine> {
    for line in &mut lines {
        for span in &mut line.spans {
            span.look = token.into();
        }
    }
    lines
}

fn name_token(title: &str) -> Token {
    if title.starts_with(['~', '/']) {
        Token::TextMuted
    } else {
        Token::Text
    }
}

fn wrapped(text: &str, lead: &str, hang: &str, layout: Layout) -> Vec<StyledLine> {
    wrap_plain(
        &Glyphs::of(layout.glyphs).fold(&one_line(text)),
        layout.width,
        lead,
        hang,
        &Glyphs::of(layout.glyphs),
    )
    .into_iter()
    .map(line)
    .collect()
}

pub fn rows(chats: &[Chat], layout: Layout) -> Vec<StyledLine> {
    let mode = layout.width_mode;
    let glyphs = Glyphs::of(layout.glyphs);
    let busy = chats
        .iter()
        .filter(|chat| !detail(chat, usize::MAX, layout).spans.is_empty())
        .collect::<Vec<_>>();
    let time = busy
        .iter()
        .filter_map(|chat| {
            detail(chat, usize::MAX, layout)
                .time
                .map(|time| width(&time, mode))
        })
        .max()
        .unwrap_or(0);
    let right = if time == 0 { 0 } else { 2 + time };
    let longest = busy
        .iter()
        .map(|chat| width(&glyphs.fold(&one_line(&chat.kept.title)), mode))
        .max()
        .unwrap_or(0);
    let phrase_w = busy
        .iter()
        .map(|chat| spans_width(&detail(chat, usize::MAX, layout).spans, mode))
        .max()
        .unwrap_or(0);
    let title_w = longest.min(layout.width.saturating_sub(6 + phrase_w + right));
    let title_w = if title_w < longest {
        title_w.max(30.min(longest))
    } else {
        title_w
    };
    let phrase_w = phrase_w.min(layout.width.saturating_sub(6 + title_w + right));
    let mut out = Vec::new();
    for chat in chats {
        let (mark, token) = mark(chat, glyphs);
        let mut spans = vec![StyledSpan::new(format!("  {mark} "), token)];
        let full = detail(chat, usize::MAX, layout);
        let below = layout.width < 80;
        let has_phrase = !full.spans.is_empty();
        let title_room = if below || !has_phrase {
            layout.width.saturating_sub(4)
        } else {
            title_w
        };
        let title = cut(&chat.kept.title, title_room, layout);
        let look = if matches!(
            chat.state,
            State::Finishing(_) | State::Restarting | State::NotAnswering
        ) {
            Look::fg(name_token(&chat.kept.title)).bold()
        } else {
            Look::fg(Token::TextMuted)
        };
        spans.push(StyledSpan::new(
            if !below && has_phrase {
                padded(&title, title_room, mode)
            } else {
                title
            },
            look,
        ));
        if has_phrase {
            let room = if below {
                layout
                    .width
                    .saturating_sub(4 + if full.time.is_some() { 9 } else { 0 })
            } else {
                phrase_w
            };
            let phrase = detail(chat, room, layout);
            let used = spans_width(&phrase.spans, mode);
            if below {
                out.push(StyledLine::new(spans));
                spans = vec![StyledSpan::plain("    ")];
            } else {
                spans.push(StyledSpan::plain("  "));
            }
            spans.extend(phrase.spans);
            if let Some(text) = phrase.time {
                let time_w = if below { 7 } else { time };
                let gap = if below && used > room {
                    out.push(StyledLine::new(std::mem::take(&mut spans)));
                    layout.width.saturating_sub(time_w)
                } else {
                    room.saturating_sub(used) + 2
                };
                spans.push(StyledSpan::new(
                    format!(
                        "{}{}{text}",
                        " ".repeat(gap),
                        " ".repeat(time_w.saturating_sub(width(&text, mode)))
                    ),
                    Token::TextMuted,
                ));
            }
        }
        out.push(StyledLine::new(spans));
        match &chat.state {
            State::Failed(reason) => {
                for text in reason.lines().filter(|line| !line.is_empty()) {
                    out.extend(wrapped(
                        text,
                        if below { "    " } else { "      " },
                        "      ",
                        layout,
                    ));
                }
                if below {
                    out.extend(toned(
                        wrapped(AGENTS_WAIT, "    ", "    ", layout),
                        AGENTS_WAITING,
                    ));
                    out.extend(wrapped(OPEN_AGAIN, "    ", "    ", layout));
                    out.push(line(format!(
                        "      {RESUME_COMMAND} {}",
                        short_id(&chat.kept.id)
                    )));
                } else {
                    out.push(StyledLine::new(vec![
                        StyledSpan::plain("      "),
                        StyledSpan::new(AGENTS_WAIT, AGENTS_WAITING),
                        StyledSpan::plain(format!(
                            " {OPEN_AGAIN} {RESUME_COMMAND} {}",
                            short_id(&chat.kept.id)
                        )),
                    ]));
                }
            }
            State::NoAnswer => out.extend(wrapped(
                "It keeps running as it was; try wisp\u{a0}restart again later.",
                if below { "    " } else { "      " },
                "      ",
                layout,
            )),
            _ => {}
        }
    }
    out
}

fn parts(items: &[String], layout: Layout) -> Vec<StyledLine> {
    let mut out = Vec::new();
    let mut text = String::new();
    let sep = format!(" {} ", Glyphs::of(layout.glyphs).separator);
    for item in items {
        let next = if text.is_empty() {
            item.clone()
        } else {
            format!("{text}{sep}{item}")
        };
        if width(&next, layout.width_mode) > layout.width && !text.is_empty() {
            out.push(line(std::mem::take(&mut text)));
            text = item.clone();
        } else {
            text = next;
        }
    }
    if !text.is_empty() {
        out.push(line(text));
    }
    out
}

pub fn live(chats: &[Chat], left: Option<u64>, height: usize, layout: Layout) -> Vec<StyledLine> {
    let finishing = chats
        .iter()
        .filter(|chat| matches!(chat.state, State::Finishing(_)))
        .count();
    let silent = chats
        .iter()
        .filter(|chat| matches!(chat.state, State::NotAnswering))
        .count();
    let mut top = vec![format!(
        "Restarting {} {}",
        chats.len(),
        if chats.len() == 1 { "chat" } else { "chats" }
    )];
    if silent > 0 {
        top.push(format!("{silent} not answering"));
    }
    if finishing > 0 {
        top.push(format!("{finishing} still finishing"));
    }
    if let Some(left) = left {
        if finishing > 0 {
            top.push(format!(
                "stops {} in {}",
                if finishing == 1 { "it" } else { "them" },
                duration(left * 1000)
            ));
        } else if silent > 0 {
            top.push(format!("waits {} more", duration(left * 1000)));
        }
    }
    let mut out = parts(&top, layout);
    out.push(StyledLine::default());
    let keys = if finishing > 0 && left.is_some() {
        let mut keys = vec![StyledLine::default()];
        keys.extend(toned(
            parts(
                &["enter restart now".to_owned(), "ctrl-c cancel".to_owned()],
                layout,
            ),
            Token::TextMuted,
        ));
        keys
    } else {
        Vec::new()
    };
    let room = height.saturating_sub(out.len() + keys.len());
    let mut listed = chats.to_vec();
    let mut omitted = 0;
    if rows(&listed, layout).len() > room {
        let done = listed
            .iter()
            .filter(|chat| {
                matches!(&chat.state, State::Restarted(open) if open.is_empty()) && !chat.kept.older
            })
            .count();
        if done > 0 {
            listed.retain(|chat| {
                !matches!(&chat.state, State::Restarted(open) if open.is_empty()) || chat.kept.older
            });
            listed.insert(
                0,
                Chat {
                    kept: Kept {
                        id: String::new(),
                        title: format!("{done} restarted"),
                        window: false,
                        older: false,
                    },
                    state: State::Restarted(Vec::new()),
                },
            );
        }
        while !listed.is_empty() && rows(&listed, layout).len() + usize::from(omitted > 0) > room {
            listed.pop();
            omitted += 1;
        }
    }
    out.extend(rows(&listed, layout));
    if omitted > 0 {
        out.push(line(format!(
            "  {} {omitted} more chats",
            Glyphs::of(layout.glyphs).running
        )));
    }
    out.extend(keys);
    out
}

pub fn end(chats: &[Chat], end: &End, layout: Layout) -> Vec<StyledLine> {
    let restarted = chats
        .iter()
        .filter(|chat| matches!(chat.state, State::Restarted(_)) && !chat.kept.older)
        .count();
    let stopped = chats
        .iter()
        .filter(|chat| !end.older.contains(&chat.kept.id))
        .filter_map(|chat| {
            if let State::Restarted(open) = &chat.state {
                (!open.is_empty()).then_some(open)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let waits = stopped
        .iter()
        .filter(|open| open.iter().any(|row| row.opening))
        .count();
    let mut out = Vec::new();
    if chats.is_empty() {
        out.extend(wrapped("No chat is kept open.", "", "", layout));
        if end.daemon {
            out.extend(wrapped(
                "Restarted Wisp's daemon on the newest version.",
                "",
                "",
                layout,
            ));
        }
    } else if end.cancelled && restarted == 0 {
        out.extend(wrapped("Cancelled: no chat was restarted.", "", "", layout));
    } else {
        let count = if restarted == chats.len() {
            format!("{restarted}")
        } else {
            format!("{restarted} of {}", chats.len())
        };
        out.extend(wrapped(
            &format!(
                "Restarted {count} {} on the newest version.",
                if chats.len() == 1 { "chat" } else { "chats" }
            ),
            "",
            "",
            layout,
        ));
    }
    if end.cancelled && restarted > 0 {
        let remaining = chats
            .iter()
            .filter(|chat| matches!(chat.state, State::Cancelled))
            .count();
        if remaining == 1 {
            out.extend(wrapped(SINGULAR.cancelled, "", "", layout));
        } else if remaining > 1 {
            out.extend(wrapped(
                &format!("Cancelled: the other {remaining} carry on as they were."),
                "",
                "",
                layout,
            ));
        }
    }
    if !stopped.is_empty() {
        let n = stopped.len();
        let tail = if waits == 0 {
            format!("{} agents carry on.", if n == 1 { "its" } else { "their" })
        } else if waits == n {
            format!("{} for you.", if n == 1 { "it waits" } else { "they wait" })
        } else {
            format!(
                "{waits} {} for you, {}.",
                if waits == 1 {
                    "chat waits"
                } else {
                    "chats wait"
                },
                if n - waits == 1 {
                    "the other carries on"
                } else {
                    "the others carry on"
                }
            )
        };
        out.extend(toned(
            wrapped(
                &format!(
                    "{} what {n} {} finishing; {tail}",
                    end.waited.map_or_else(
                        || "Stopped".to_owned(),
                        |seconds| {
                            let time = duration(seconds * 1000);
                            format!(
                                "Waited {}, then stopped",
                                time.strip_suffix(" 00s").unwrap_or(&time)
                            )
                        }
                    ),
                    if n == 1 { "chat was" } else { "chats were" }
                ),
                "",
                "",
                layout,
            ),
            Token::Text,
        ));
    }
    if !end.older.is_empty() {
        let older = chats
            .iter()
            .filter(|chat| end.older.contains(&chat.kept.id))
            .collect::<Vec<_>>();
        let waits = older
            .iter()
            .filter(|chat| {
                matches!(&chat.state, State::Restarted(open) if open.iter().any(|row| row.opening))
                    || matches!(chat.state, State::Failed(_))
            })
            .count();
        let windows = older
            .iter()
            .filter(|chat| {
                chat.kept.window && matches!(chat.state, State::Restarted(_) | State::Failed(_))
            })
            .count();
        out.extend(wrapped(
            &if waits > 0 {
                format!(
                    "Agents that were at work carry on, but {waits} {} for you.",
                    if waits == 1 {
                        "chat waits"
                    } else {
                        "chats wait"
                    }
                )
            } else {
                "Agents that were at work carry on.".to_owned()
            },
            "",
            "",
            layout,
        ));
        if windows > 0 {
            out.extend(wrapped(
                &format!(
                    "{} {} closed; each printed the command that opens its chat again.",
                    windows,
                    if windows == 1 { "window" } else { "windows" }
                ),
                "",
                "",
                layout,
            ));
        }
    }
    let kept = chats
        .iter()
        .filter(|chat| {
            !matches!(&chat.state, State::Restarted(open) if open.is_empty() && !chat.kept.older)
                && !(end.cancelled && restarted == 0 && matches!(chat.state, State::Cancelled))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !kept.is_empty() {
        out.push(StyledLine::default());
        out.extend(rows(&kept, layout));
    }

    out
}

pub const DECLINED: &str = "No chat was restarted.";

pub fn in_a_job(layout: Layout) -> Vec<StyledLine> {
    toned(
        wrapped(
            "wisp: run by an agent, wisp\u{a0}restart restarts nothing",
            "",
            "",
            layout,
        ),
        Token::Error,
    )
}

pub fn older_question(chats: &[Kept], layout: Layout) -> Vec<StyledLine> {
    let one = chats.len() == 1;
    let intro = if one {
        SINGULAR.older_intro.to_owned()
    } else {
        format!(
            "These {} chats run an older version of Wisp that can't wait for work to finish:",
            chats.len()
        )
    };
    let mut out = wrapped(&intro, "", "", layout);
    out.push(StyledLine::default());
    let title_w = chats
        .iter()
        .filter(|chat| chat.window)
        .map(|chat| width(&one_line(&chat.title), layout.width_mode))
        .max()
        .unwrap_or(0)
        .min(layout.width.saturating_sub(15));
    for chat in chats {
        if layout.width >= 80 && chat.window {
            out.push(StyledLine::new(vec![
                StyledSpan::new(
                    format!(
                        "  {}",
                        padded(
                            &cut(&chat.title, title_w, layout),
                            title_w,
                            layout.width_mode
                        )
                    ),
                    name_token(&chat.title),
                ),
                StyledSpan::new(format!("  {IN_WINDOW}"), Token::TextMuted),
            ]));
        } else {
            out.push(StyledLine::new(vec![StyledSpan::new(
                format!(
                    "  {}",
                    cut(&chat.title, layout.width.saturating_sub(2), layout)
                ),
                name_token(&chat.title),
            )]));
            if chat.window {
                out.push(StyledLine::new(vec![StyledSpan::new(
                    format!("    {IN_WINDOW}"),
                    Token::TextMuted,
                )]));
            }
        }
    }
    out.push(StyledLine::default());
    out.extend(wrapped(
        if one {
            SINGULAR.older_stop
        } else {
            "Restarting them stops what they are doing; agents at work carry on afterwards."
        },
        "",
        "",
        layout,
    ));
    let windows = chats.iter().filter(|chat| chat.window).count();
    if windows == 1 {
        out.extend(wrapped(
            if one {
                SINGULAR.its_window
            } else {
                SINGULAR.one_window
            },
            "",
            "",
            layout,
        ));
    } else if windows > 1 {
        out.extend(wrapped(
            &format!("Their {windows} windows close; each prints the command that opens it again."),
            "",
            "",
            layout,
        ));
    }
    out.extend(wrapped(
        "A chat started with its own settings file, keys file or keychain comes back without it.",
        "",
        "",
        layout,
    ));
    out.extend(wrapped(
        if one {
            SINGULAR.older_question
        } else {
            "Restart them on the newest version?"
        },
        "",
        "",
        layout,
    ));
    out
}

pub fn daemon(chats: &[Kept], layout: Layout) -> Vec<StyledLine> {
    let title_w = chats
        .iter()
        .map(|chat| width(&one_line(&chat.title), layout.width_mode))
        .max()
        .unwrap_or(0)
        .min(layout.width.saturating_sub(45));
    let mut out = Vec::new();
    for chat in chats {
        let id = short_id(&chat.id);
        if chat.title.is_empty() {
            out.push(StyledLine::new(vec![StyledSpan::new(
                format!("  {id}"),
                Token::TextMuted,
            )]));
        } else if layout.width >= 80 {
            let title = padded(
                &cut(&chat.title, title_w, layout),
                title_w,
                layout.width_mode,
            );
            let mut row = vec![
                StyledSpan::new(format!("  {title}  "), name_token(&chat.title)),
                StyledSpan::new(id, Token::TextMuted),
                StyledSpan::new(
                    format!("  {:11}", if chat.window { IN_WINDOW } else { "" }),
                    Token::TextMuted,
                ),
            ];
            if chat.older {
                row.push(StyledSpan::new(format!("  {OLDER_VERSION}"), Token::Text));
            }
            out.push(StyledLine::new(row));
        } else {
            out.push(StyledLine::new(vec![StyledSpan::new(
                format!(
                    "  {}",
                    cut(&chat.title, layout.width.saturating_sub(2), layout)
                ),
                name_token(&chat.title),
            )]));
            let mut row = vec![StyledSpan::new(format!("    {id}"), Token::TextMuted)];
            let mut used = 4 + width(id, layout.width_mode);
            for (mark, token) in [
                chat.window.then_some((IN_WINDOW, Token::TextMuted)),
                chat.older.then_some((OLDER_VERSION, Token::Text)),
            ]
            .into_iter()
            .flatten()
            {
                if used + 2 + mark.len() > layout.width {
                    out.push(StyledLine::new(std::mem::take(&mut row)));
                    row.push(StyledSpan::plain("    "));
                    used = 4;
                } else {
                    row.push(StyledSpan::plain("  "));
                    used += 2;
                }
                row.push(StyledSpan::new(mark, token));
                used += mark.len();
            }
            out.push(StyledLine::new(row));
        }
    }
    out
}

#[cfg(test)]
mod tests;
