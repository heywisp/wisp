use std::time::Duration;

use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::manifest::{OBSERVE_POINTS, PointKind};
use wisp_tool_text::output::INLINE_LIMIT;

pub use wisp_plugin_api::manifest::{
    AFTER_COMPACT, AFTER_RESPONSE, AFTER_TOOL, AFTER_TURN, AGENT_ENDED, AGENT_MAIL, AGENT_POINTS,
    AGENT_REPORTED, AGENT_SPAWNED, AGENT_STATE, AGENT_STOPPED, BEFORE_COMPACT, BEFORE_END,
    BEFORE_REQUEST, BEFORE_TOOL, BEFORE_TURN, COMMAND, COMPACTION_HEAD, CONTEXT_THRESHOLD,
    MODEL_SWITCHED, REWOUND, SESSION_END, SESSION_START, SETTINGS_CHANGED, TOOLS_CHANGED,
    USAGE_CHANGED, USER_INPUT,
};

pub const OUTSIDE_BUDGET: Duration = Duration::from_secs(2);

pub const MAX_DEPTH: u8 = 4;

pub const MAX_APPEND_CHARS: usize = 10_000;

pub const MAX_CONTINUES: u32 = 8;

pub use wisp_tool_text::compaction::MAX_INSTRUCTION_CHARS;

pub const MAX_HEAD_CHARS: usize = 20_000;

pub const MAX_FOCUS_CHARS: usize = 2_000;

const SHORT: Duration = Duration::from_secs(2);

const LONG: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takes {
    None,
    Append,
    Rewrite,
    Block,
    Continue,
    Compact,
}

impl Takes {
    pub fn of(reply: &Reply) -> Self {
        match reply {
            Reply::None => Self::None,
            Reply::Append(_) => Self::Append,
            Reply::Rewrite(_) => Self::Rewrite,
            Reply::Block(_) => Self::Block,
            Reply::Continue(_) => Self::Continue,
            Reply::Compact(_) => Self::Compact,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub takes: &'static [Takes],
    pub timeout: Duration,
    pub gate: bool,
}

const OBSERVE: Point = Point {
    takes: &[],
    timeout: SHORT,
    gate: false,
};

pub fn core(point: &str) -> Option<Point> {
    let (takes, timeout, gate): (&'static [Takes], Duration, bool) = match point {
        p if OBSERVE_POINTS.contains(&p) || AGENT_POINTS.contains(&p) || p == USAGE_CHANGED => {
            return Some(OBSERVE);
        }
        USER_INPUT | BEFORE_TOOL => (&[Takes::Rewrite, Takes::Block], SHORT, true),
        BEFORE_TURN | COMPACTION_HEAD => (&[Takes::Append], SHORT, false),
        BEFORE_END => (&[Takes::Continue], LONG, false),
        AFTER_TOOL => (&[Takes::Rewrite, Takes::Append], LONG, false),
        CONTEXT_THRESHOLD => (&[Takes::Compact], SHORT, false),
        BEFORE_COMPACT => (&[Takes::Block, Takes::Append, Takes::Rewrite], SHORT, true),
        _ => return None,
    };
    Some(Point {
        takes,
        timeout,
        gate,
    })
}

pub fn custom(kind: PointKind) -> Point {
    let takes: &'static [Takes] = match kind {
        PointKind::Observe => &[],
        PointKind::Contribute => &[Takes::Append],
        PointKind::Transform => &[Takes::Rewrite],
    };
    Point {
        takes,
        timeout: SHORT,
        gate: false,
    }
}

pub fn texts(texts: &[String], uncapped: bool) -> Result<(), &'static str> {
    if texts.iter().any(|t| t.trim().is_empty()) {
        return Err("blank");
    }
    if uncapped {
        return Ok(());
    }
    let total = texts
        .iter()
        .map(|t| t.chars().count())
        .fold(0, usize::saturating_add);
    if total > MAX_APPEND_CHARS {
        return Err("too_large");
    }
    Ok(())
}

pub fn input_object(text: &str) -> Option<String> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value @ serde_json::Value::Object(_)) => serde_json::to_string(&value).ok(),
        _ => None,
    }
}

fn rewrite(event: Option<&Event>, text: &str) -> Result<(), &'static str> {
    match event {
        Some(Event::UserInput(input)) => {
            if input.images == 0 && text.trim().is_empty() {
                return Err("blank");
            }
            let cap = input.text.chars().count().saturating_add(MAX_APPEND_CHARS);
            if text.chars().count() > cap {
                return Err("too_large");
            }
            Ok(())
        }
        Some(Event::BeforeTool(_)) if input_object(text).is_none() => Err("not_json"),
        Some(Event::AfterTool(_)) if text.len() > INLINE_LIMIT => Err("too_large"),
        Some(Event::BeforeCompact(_)) if text.trim().is_empty() => Err("blank"),
        Some(Event::BeforeCompact(_)) if text.chars().count() > MAX_INSTRUCTION_CHARS => {
            Err("too_large")
        }
        _ => Ok(()),
    }
}

fn continued(event: Option<&Event>, text: &str) -> Result<(), &'static str> {
    if text.trim().is_empty() {
        return Err("blank");
    }
    if text.chars().count() > MAX_APPEND_CHARS {
        return Err("too_large");
    }
    match event {
        Some(Event::BeforeEnd(end)) if end.continues >= MAX_CONTINUES => Err("cap_reached"),
        _ => Ok(()),
    }
}

pub fn check(
    point: &Point,
    reply: &Reply,
    event: Option<&Event>,
    uncapped: bool,
) -> Result<(), &'static str> {
    let takes = Takes::of(reply);
    if takes == Takes::None {
        return Ok(());
    }
    if !point.takes.contains(&takes) {
        return Err("not_allowed");
    }
    match reply {
        Reply::Append(appended) => texts(appended, uncapped),
        Reply::Rewrite(text) => rewrite(event, text),
        Reply::Continue(text) => continued(event, text),
        Reply::Compact(Some(focus)) if focus.chars().count() > MAX_FOCUS_CHARS => Err("too_large"),
        _ => Ok(()),
    }
}
