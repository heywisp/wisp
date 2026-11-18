use wisp_protocol::{SignalKind, ToolAction, ToolView, Typed};

use super::{clip, one_line};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{WidthMode, fit_end_or_cut, spans_width};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy::{self, ProcessVerb};
use crate::transcript::facts_at_edge;

pub const TYPED_COLUMNS: usize = 40;
pub const PATTERN_COLUMNS: usize = 40;
pub const MIN_TITLE: usize = 10;
pub const MIN_COMMAND: usize = 10;
pub const COMMAND_GAP: usize = 2;
pub const FACTS_GAP: usize = 2;

pub fn echoed(typed: Option<&Typed>) -> bool {
    typed.is_some_and(|t| t.echoed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Weight {
    State,
    Count,
    Time,
}

#[derive(Clone, Debug)]
pub(crate) struct Said {
    spans: Vec<StyledSpan>,
    weight: Weight,
}

impl Said {
    pub(super) fn new(text: impl Into<String>, tone: Token, weight: Weight) -> Said {
        Said {
            spans: vec![StyledSpan::new(text.into(), tone)],
            weight,
        }
    }

    pub(super) fn spans(spans: Vec<StyledSpan>, weight: Weight) -> Said {
        Said { spans, weight }
    }

    pub(super) fn state(text: impl Into<String>) -> Said {
        Said::new(text, Token::TextMuted, Weight::State)
    }

    pub(super) fn count(text: impl Into<String>) -> Said {
        Said::new(text, Token::TextMuted, Weight::Count)
    }

    pub(super) fn time(text: impl Into<String>) -> Said {
        Said::new(text, Token::TextMuted, Weight::Time)
    }

    pub(super) fn error(text: impl Into<String>) -> Said {
        Said::new(text, Token::Error, Weight::State)
    }
}

pub(crate) struct Head {
    pub(crate) glyph: String,
    pub(crate) glyph_look: Look,
    pub(crate) title: Vec<StyledSpan>,
    pub(crate) command: Option<String>,
    pub(crate) extra: Option<String>,
    pub(crate) facts: Vec<Said>,
}

pub(super) fn title_spans(verb: &str, rest: &str, look: Look) -> Vec<StyledSpan> {
    let verb_look = look.bold();
    match (verb.is_empty(), rest.is_empty()) {
        (true, _) => vec![StyledSpan::new(rest, look)],
        (false, true) => vec![StyledSpan::new(verb, verb_look)],
        (false, false) => vec![
            StyledSpan::new(verb, verb_look),
            StyledSpan::new(format!(" {rest}"), look),
        ],
    }
}

fn facts_of(facts: &[Said], keep: impl Fn(Weight) -> bool, sep: &str) -> Vec<StyledSpan> {
    let mut out = vec![];
    for said in facts.iter().filter(|s| keep(s.weight)) {
        if !out.is_empty() {
            out.push(StyledSpan::new(sep.to_owned(), Token::TextMuted));
        }
        out.extend(said.spans.iter().cloned());
    }
    out
}

pub(super) fn entry_facts(facts: &[Said], sep: &str) -> (Vec<StyledSpan>, Vec<StyledSpan>) {
    (
        facts_of(facts, |_| true, sep),
        facts_of(facts, |w| w == Weight::State, sep),
    )
}

pub(crate) struct Pens<'a> {
    pub(crate) edge: usize,
    pub(crate) mode: WidthMode,
    pub(crate) sep: &'a str,
    pub(crate) more: &'a str,
}

struct Group {
    gap: usize,
    text: String,
}

fn group_of(head: &Head) -> Option<Group> {
    match (&head.command, &head.extra) {
        (Some(command), Some(extra)) => Some(Group {
            gap: COMMAND_GAP,
            text: format!("{command} {extra}"),
        }),
        (Some(command), None) => Some(Group {
            gap: COMMAND_GAP,
            text: command.clone(),
        }),
        (None, Some(extra)) => Some(Group {
            gap: 1,
            text: extra.clone(),
        }),
        (None, None) => None,
    }
    .filter(|g| !g.text.is_empty())
}

pub(crate) fn lay_out(head: Head, lead: Vec<StyledSpan>, pens: &Pens<'_>) -> StyledLine {
    let mode = pens.mode;
    let width_of = |spans: &[StyledSpan]| spans_width(spans, mode);
    let room = pens.edge.saturating_sub(width_of(&lead));
    let title_w = width_of(&head.title);
    let all = facts_of(&head.facts, |_| true, pens.sep);
    let timed = facts_of(&head.facts, |w| w != Weight::Count, pens.sep);
    let states = facts_of(&head.facts, |w| w == Weight::State, pens.sep);
    let room_for = |facts: &[StyledSpan]| match width_of(facts) {
        0 => 0,
        w => w + FACTS_GAP,
    };
    let group = group_of(&head);
    let group_w = group.as_ref().map_or(0, |g| {
        g.gap + width_of(&[StyledSpan::plain(g.text.clone())])
    });
    let build = |title: Vec<StyledSpan>, group: Option<(usize, String)>, facts: Vec<StyledSpan>| {
        let mut spans = lead.clone();
        spans.extend(title);
        if let Some((gap, text)) = group {
            spans.push(StyledSpan::plain(" ".repeat(gap)));
            spans.push(StyledSpan::new(text, Token::TextMuted));
        }
        facts_at_edge(&mut spans, facts, pens.edge, mode);
        spans.retain(|s| !s.text.is_empty());
        clip(StyledLine::new(spans), pens.edge, mode, pens.more)
    };
    let whole_group = group.as_ref().map(|g| (g.gap, g.text.clone()));
    if title_w + group_w + room_for(&all) <= room {
        return build(head.title, whole_group, all);
    }
    if title_w + group_w + room_for(&timed) <= room {
        return build(head.title, whole_group, timed);
    }
    if let Some(g) = group.as_ref().filter(|g| g.gap == COMMAND_GAP) {
        let left = room.saturating_sub(title_w + room_for(&timed) + g.gap);
        if left >= MIN_COMMAND {
            let cut = fit_end_or_cut(&g.text, left, pens.more, mode);
            return build(head.title, Some((g.gap, cut)), timed);
        }
    }
    if title_w + room_for(&timed) <= room {
        return build(head.title, None, timed);
    }
    if room >= room_for(&timed) + MIN_TITLE {
        let cut = clip(
            StyledLine::new(head.title),
            room - room_for(&timed),
            mode,
            pens.more,
        );
        return build(cut.spans, None, timed);
    }
    let title_room = room.saturating_sub(room_for(&states));
    let cut = if title_room == 0 {
        vec![]
    } else {
        clip(StyledLine::new(head.title), title_room, mode, pens.more).spans
    };
    build(cut, None, states)
}

pub(super) struct Typing {
    text: Option<String>,
}

impl Typing {
    pub(super) fn wrote(&self) -> bool {
        self.text.is_some()
    }

    pub(super) fn shown(&self, typed: Option<&Typed>) -> Option<&str> {
        self.text.as_deref().filter(|_| echoed(typed))
    }
}

pub(super) struct ProcessCall {
    pub(super) id: Option<u32>,
    typing: Typing,
    keys: Vec<String>,
    signal: Option<SignalKind>,
    read: Option<String>,
    pattern: Option<String>,
}

impl ProcessCall {
    pub(super) fn of(view: &ToolView) -> Option<ProcessCall> {
        match view {
            ToolView::Process {
                id,
                text,
                keys,
                signal,
                read,
                pattern,
            } => Some(ProcessCall {
                id: *id,
                typing: Typing {
                    text: text.clone().filter(|t| !t.is_empty()),
                },
                keys: keys.iter().map(|k| one_line(k)).collect(),
                signal: *signal,
                read: read.as_deref().map(one_line),
                pattern: pattern.as_deref().map(one_line),
            }),
            _ => None,
        }
    }

    pub(super) fn of_action(action: &ToolAction, target: &str) -> Option<ProcessCall> {
        let id = target
            .split_whitespace()
            .next()
            .and_then(|word| word.parse::<u32>().ok());
        let call = |signal: Option<SignalKind>| ProcessCall {
            id,
            typing: Typing { text: None },
            keys: vec![],
            signal,
            read: None,
            pattern: None,
        };
        match action {
            ToolAction::Input | ToolAction::Wait | ToolAction::Attach => Some(call(None)),
            ToolAction::Signal { kind } => Some(call(Some(*kind))),
            _ => None,
        }
    }

    pub(super) fn writes(&self) -> bool {
        self.typing.wrote() || !self.keys.is_empty()
    }

    pub(super) fn input(
        &self,
        typed: Option<&Typed>,
        (glyphs, mode): (&Glyphs, WidthMode),
    ) -> Vec<String> {
        let mut out = vec![];
        if self.writes() {
            let mut line = String::new();
            if let Some(text) = self.typing.shown(typed) {
                line.push_str(&quoted(text, glyphs, mode));
            }
            if !self.keys.is_empty() {
                if self.typing.wrote() {
                    line.push_str(if line.is_empty() { "+ " } else { " + " });
                }
                line.push_str(&self.keys.join(" + "));
            }
            if !line.is_empty() {
                out.push(line);
            }
        }
        if let Some(pattern) = &self.pattern {
            out.push(copy::quoted(pattern));
        }
        out
    }

    pub(super) fn facts(&self, typed: Option<&Typed>) -> Vec<Said> {
        let mut out = vec![];
        let Some(typed) = typed else {
            return out;
        };
        if typed.unsent {
            out.push(Said::state(copy::PARTLY_SENT));
        }
        if let Some(kind) = self.signal.filter(|_| self.writes() && !typed.signalled) {
            out.push(Said::state(copy::signal_not_sent(kind)));
        }
        out
    }

    pub(super) fn words(
        &self,
        action: Option<&ToolAction>,
        name: Option<&str>,
        typed: Option<&Typed>,
        running: bool,
        (glyphs, mode): (&Glyphs, WidthMode),
    ) -> Option<(String, String)> {
        let verb = |v: ProcessVerb| match name {
            Some(_) => copy::process_verb(v, running).to_owned(),
            None => copy::process_act(v, running).to_owned(),
        };
        let named = name.unwrap_or_default().to_owned();
        if self.id.is_none() && matches!(action, Some(ToolAction::Other { .. }) | None) {
            name?;
            return Some((
                verb(ProcessVerb::Listed),
                copy::BACKGROUND_PROCESSES.to_owned(),
            ));
        }
        if matches!(action, Some(ToolAction::Attach)) {
            return Some((verb(ProcessVerb::AskedToTakeOver), named));
        }
        if self.writes() {
            return Some(self.written(name, typed, running, (glyphs, mode)));
        }
        if let Some(kind) = self.signal {
            let v = match kind {
                SignalKind::Interrupt => ProcessVerb::Interrupted,
                SignalKind::Kill => ProcessVerb::Stopped,
            };
            return Some((verb(v), named));
        }
        Some(match action {
            Some(ToolAction::Wait) => (verb(ProcessVerb::WaitedFor), named),
            Some(ToolAction::Input) => (
                verb(ProcessVerb::Typed),
                name.map(copy::typed_into_name).unwrap_or_default(),
            ),
            Some(ToolAction::Search) => {
                let pattern = self
                    .pattern
                    .as_deref()
                    .map(|p| fit_end_or_cut(p, PATTERN_COLUMNS, glyphs.more, mode))
                    .unwrap_or_default();
                (
                    verb(ProcessVerb::Searched),
                    copy::searched_for(name, &pattern),
                )
            }
            _ if self.read.as_deref() == Some("screen") => {
                (verb(ProcessVerb::Read), copy::screen_of(name))
            }
            _ => (verb(ProcessVerb::Read), copy::output_of(name)),
        })
    }

    fn written(
        &self,
        name: Option<&str>,
        typed: Option<&Typed>,
        running: bool,
        (glyphs, mode): (&Glyphs, WidthMode),
    ) -> (String, String) {
        let keys = self.keys.join(" + ");
        let signalled = self.signal.filter(|_| typed.is_some_and(|t| t.signalled));
        let shown = self.typing.shown(typed).map(|t| quoted(t, glyphs, mode));
        let mut parts: Vec<String> = vec![];
        let v = if self.typing.wrote() {
            match name {
                Some(name) => parts.push(copy::typed_into_name(name)),
                None => parts.extend(shown.clone()),
            }
            if !keys.is_empty() {
                parts.push(copy::and_pressed(&keys, running));
            }
            ProcessVerb::Typed
        } else {
            parts.push(match name {
                Some(name) => copy::pressed_in(&keys, name),
                None => keys,
            });
            ProcessVerb::Pressed
        };
        let mut rest = parts.join(" ");
        match name {
            Some(_) => {
                if let Some(kind) = signalled {
                    rest.push_str(copy::then_signalled(kind));
                }
                if let Some(shown) = shown {
                    rest.push_str(": ");
                    rest.push_str(&shown);
                }
                (copy::process_verb(v, running).to_owned(), rest)
            }
            None => {
                if let Some(kind) = signalled {
                    rest.push_str(copy::then_signalled_act(kind));
                }
                (copy::process_act(v, running).to_owned(), rest)
            }
        }
    }
}

pub(super) fn quoted(text: &str, glyphs: &Glyphs, mode: WidthMode) -> String {
    let (first, rest) = match text.split_once('\n') {
        Some((first, rest)) => (first, Some(rest)),
        None => (text, None),
    };
    let more = rest.map_or(0, |r| r.lines().count());
    let first = one_line(first.strip_suffix('\r').unwrap_or(first));
    let shown = match rest {
        Some(_) => format!("{first}{}", glyphs.newline),
        None => first,
    };
    let cut = fit_end_or_cut(&shown, TYPED_COLUMNS, glyphs.more, mode);
    let mut out = format!("\"{cut}\"");
    if more > 0 {
        out.push(' ');
        out.push_str(&copy::more_lines(more));
    }
    out
}
