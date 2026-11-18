use std::time::Duration;

use wisp_protocol::agents::{AgentPath, shown_agent};
use wisp_protocol::{ExitStatus, ProcessEnd, TermSize};

use super::band::{CHROME_EDGE, selected_row, selection_lead};
use super::menu::{key_row, menu_lead};
use super::question::{FormView, answer_options, form_rows};
use super::row::{KEY_ROW, KeyHint, clip, full};
use crate::markdown::Layout;
use crate::motion::{MotionCtx, comet, comet_next_change};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{spans_width, width, wrap};
use crate::theme::glyphs::Glyphs;
use crate::theme::{Token, screen_look};
use crate::transcript::copy::ProcessName;
use crate::transcript::tool::row::{Head, Pens, lay_out};
use crate::transcript::{CallCaption, Keeps, copy};

pub const PS_MIN_ROWS: usize = 4;
pub(super) const STATE_GAP: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRow {
    pub id: u32,
    pub command: String,
    pub terminal: Option<TermSize>,
    pub end: Option<ProcessEnd>,
    pub clock_ms: u64,
    pub caption: CallCaption,
    pub waiting: bool,
    pub restored: bool,
    pub kept: Keeps,
    pub pid: Option<u32>,
    pub log: Option<String>,
    pub agent: AgentPath,
    pub agent_ended: bool,
    pub opens: bool,
    pub stoppable: bool,
    pub attachable: bool,
}

impl ProcessRow {
    pub fn running(&self) -> bool {
        self.end.is_none()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PsView<'a> {
    pub rows: &'a [ProcessRow],
    pub focus: usize,
    pub confirm: bool,
    pub here: &'a AgentPath,
    pub t: Duration,
    pub cx: MotionCtx,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenMode {
    Watching,
    Typing {
        asked: Option<String>,
        warning: bool,
    },
}

pub struct ScreenView<'a> {
    pub name: ProcessName,
    pub command: &'a str,
    pub screen: &'a vt100::Screen,
    pub ended: bool,
    pub attachable: bool,
    pub mode: ScreenMode,
}

pub struct StepInView<'a> {
    pub id: u32,
    pub reason: &'a str,
    pub settling: bool,
    pub selected: Option<usize>,
    pub typing: bool,
}

pub const SCREEN_CHROME_ROWS: u16 = 2;

fn edge(layout: Layout) -> usize {
    layout.width.saturating_sub(1).min(CHROME_EDGE)
}

fn left_room(right_w: usize, edge: usize) -> Option<usize> {
    (right_w + STATE_GAP < edge).then(|| edge - right_w - STATE_GAP)
}

pub(super) fn split(left: Vec<StyledSpan>, right: Vec<StyledSpan>, layout: Layout) -> StyledLine {
    split_at(left, right, edge(layout), layout)
}

pub(super) fn split_at(
    left: Vec<StyledSpan>,
    right: Vec<StyledSpan>,
    edge: usize,
    layout: Layout,
) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let right_w = spans_width(&right, mode);
    let Some(room) = left_room(right_w, edge).filter(|_| !right.is_empty()) else {
        return full(left, layout);
    };
    let left = clip(left, room, glyphs.more, mode);
    let pad = edge - spans_width(&left, mode) - right_w;
    let mut spans = left;
    spans.push(StyledSpan::plain(" ".repeat(pad)));
    spans.extend(right);
    full(spans, layout)
}

pub(super) fn title_row(title: Vec<StyledSpan>, layout: Layout) -> StyledLine {
    let mut spans = vec![StyledSpan::plain(" ")];
    spans.extend(title);
    full(spans, layout)
}

pub(super) fn note_row(text: String, tone: Token, lead: &str, layout: Layout) -> StyledLine {
    full(
        vec![StyledSpan::plain(lead), StyledSpan::new(text, tone)],
        layout,
    )
}

pub(super) fn list_lead(layout: Layout) -> String {
    " ".repeat(menu_lead(layout))
}

fn glyph(row: &ProcessRow, view: &PsView<'_>, glyphs: &Glyphs) -> (&'static str, Token) {
    match &row.end {
        None => (comet(view.t, view.cx, glyphs), Token::Accent),
        Some(ProcessEnd::Exited {
            exit: ExitStatus::Code { code: 0 },
        }) => (glyphs.done, Token::Ok),
        Some(ProcessEnd::Exited { .. } | ProcessEnd::Failed { .. }) => {
            (glyphs.failed, Token::Error)
        }
        Some(ProcessEnd::Stopped { .. } | ProcessEnd::Lost { .. }) => {
            (glyphs.stopped, Token::TextMuted)
        }
    }
}

fn entry(
    row: &ProcessRow,
    focused: bool,
    number_w: usize,
    view: &PsView<'_>,
    layout: Layout,
) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let (mark, tone) = glyph(row, view, &glyphs);
    let id = row.id.to_string();
    let pad = number_w.saturating_sub(id.len());
    let mut left = selection_lead(focused, layout);
    left.extend([
        StyledSpan::new(mark, tone),
        StyledSpan::plain(format!(" {}", " ".repeat(pad))),
        StyledSpan::new(id, Token::Text),
        StyledSpan::plain("  "),
    ]);
    let kept = if row.running() { row.kept } else { Keeps::No };
    let state = if row.waiting && row.running() {
        StyledSpan::new(copy::ps_waiting(glyphs.separator), Token::Warn)
    } else if kept == Keeps::Unchecked {
        StyledSpan::new(
            format!(
                "{} {} {}",
                copy::may_still_run(row.pid),
                glyphs.separator,
                copy::BEFORE_RESTART
            ),
            Token::TextMuted,
        )
    } else if kept == Keeps::Running && row.restored {
        StyledSpan::new(copy::KEPT_RESTORED_STATE, Token::TextMuted)
    } else if kept == Keeps::Running {
        StyledSpan::new(
            format!(
                "{} {} {}",
                copy::KEPT_STATE,
                glyphs.separator,
                copy::clock(row.clock_ms / 1000)
            ),
            Token::TextMuted,
        )
    } else if row.restored {
        StyledSpan::new(
            format!(
                "{} {} {}",
                copy::process_state(row.end.as_ref(), row.terminal),
                glyphs.separator,
                copy::BEFORE_RESTART
            ),
            Token::TextMuted,
        )
    } else {
        StyledSpan::new(
            format!(
                "{} {} {}",
                copy::process_state(row.end.as_ref(), row.terminal),
                glyphs.separator,
                copy::clock(row.clock_ms / 1000)
            ),
            Token::TextMuted,
        )
    };
    let edge = edge(layout);
    let caption_edge = left_room(width(&state.text, layout.width_mode), edge).unwrap_or(edge);
    let caption = lay_out(
        Head {
            glyph: String::new(),
            glyph_look: Look::default(),
            title: vec![StyledSpan::new(
                row.caption.title.as_ref().unwrap_or(&row.command),
                Token::Text,
            )],
            command: row.caption.command.clone(),
            extra: row.caption.extra.clone(),
            facts: vec![],
        },
        left,
        &Pens {
            edge: caption_edge,
            mode: layout.width_mode,
            sep: glyphs.separator,
            more: glyphs.more,
        },
    );
    let out = split(caption.spans, vec![state], layout);
    if focused {
        selected_row(out.spans, layout)
    } else {
        out
    }
}

pub fn ps_rows(view: &PsView<'_>, room: usize, layout: Layout) -> Vec<StyledLine> {
    if room < PS_MIN_ROWS {
        return vec![];
    }
    let header = title_row(
        vec![StyledSpan::new(
            copy::PROCESSES_TITLE,
            Look::fg(Token::Text).bold(),
        )],
        layout,
    );
    let number_w = view
        .rows
        .iter()
        .map(|r| r.id.to_string().len())
        .max()
        .unwrap_or(1);
    let focus = view.focus.min(view.rows.len().saturating_sub(1));
    let grouped = view.rows.iter().any(|r| !r.agent.is_root());
    let mut body: Vec<StyledLine> = vec![];
    let (mut focus_line, mut pins) = (0, vec![]);
    for (running, label) in [(true, copy::PS_RUNNING), (false, copy::PS_HISTORY)] {
        let section = body.len();
        let rows = view
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.running() == running);
        body.push(group_heading(label, rows.clone().count(), layout));
        let (mut previous, mut heading) = (None, section);
        for (i, row) in rows {
            if grouped && previous != Some(&row.agent) {
                heading = body.len();
                body.push(agent_heading(row, layout));
            }
            previous = Some(&row.agent);
            if i == focus {
                focus_line = body.len();
                pins = vec![section];
                if grouped {
                    pins.push(heading);
                }
            }
            body.push(entry(row, i == focus, number_w, view, layout));
        }
    }
    let focused = view.rows.get(focus);
    let hint = match focused.filter(|r| view.confirm && r.stoppable) {
        Some(row) => {
            let lead = list_lead(layout);
            let named = copy::press_x_again(ProcessName::of(row.id, &row.agent, view.here));
            let fits = width(&lead, layout.width_mode) + width(&named, layout.width_mode);
            let note = if fits <= layout.width {
                named
            } else {
                copy::press_x_again(row.id)
            };
            note_row(note, Token::Error, &lead, layout)
        }
        None => {
            let stop = focused.is_some_and(|r| r.stoppable);
            let attach = focused.is_some_and(|r| r.attachable);
            let several = view.rows.len() > 1;
            let open = focused.is_some_and(|r| r.opens);
            let keys = copy::ps_keys_with(layout.glyphs, open, stop, several, attach);
            key_row(&keys, "", None, KEY_ROW, &list_lead(layout), layout)
        }
    };
    framed(header, body, Some(hint), focus_line, &pins, room)
}

fn group_heading(label: &str, count: usize, layout: Layout) -> StyledLine {
    split(
        vec![
            StyledSpan::plain(" "),
            StyledSpan::new(label, Look::fg(Token::TextMuted).bold()),
        ],
        vec![StyledSpan::new(count.to_string(), Token::TextMuted)],
        layout,
    )
}

fn agent_heading(row: &ProcessRow, layout: Layout) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let mut text = shown_agent(&row.agent).to_owned();
    if row.agent_ended {
        text = format!("{text} {} {}", glyphs.separator, copy::SCREEN_ENDED);
    }
    note_row(text, Token::TextMuted, &list_lead(layout), layout)
}

pub(super) fn framed(
    header: StyledLine,
    body: Vec<StyledLine>,
    hint: Option<StyledLine>,
    focus: usize,
    pins: &[usize],
    room: usize,
) -> Vec<StyledLine> {
    let window = room.saturating_sub(1 + usize::from(hint.is_some()));
    let window = window.min(body.len());
    let start = (focus + 1).saturating_sub(window);
    let mut pinned = vec![];
    let spare = window - 1;
    for pin in pins.iter().skip(pins.len().saturating_sub(spare)) {
        if *pin < start + pinned.len() {
            pinned.extend(body.get(*pin).cloned());
        }
    }
    let skip = pinned.len();
    let mut out = vec![header];
    out.extend(pinned);
    out.extend(body.into_iter().skip(start + skip).take(window - skip));
    out.extend(hint);
    out
}

pub fn ps_next_change(view: &PsView<'_>) -> Option<Duration> {
    let running = view.rows.iter().any(ProcessRow::running);
    let comet = running
        .then(|| comet_next_change(view.t, view.cx))
        .flatten();
    let clock = running.then(|| Duration::from_secs(view.t.as_secs().saturating_add(1)));
    comet.into_iter().chain(clock).min()
}

fn screen_line(
    screen: &vt100::Screen,
    row: u16,
    cols: u16,
    cursor: Option<u16>,
) -> Vec<StyledSpan> {
    let mut spans: Vec<StyledSpan> = vec![];
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let mut look = screen_look(cell);
        if cursor == Some(col) {
            look = Look {
                reverse: !look.reverse,
                ..look
            };
        }
        let text: String = if cell.has_contents() {
            cell.contents()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect()
        } else {
            " ".to_owned()
        };
        match spans.last_mut() {
            Some(last) if last.look == look => last.text.push_str(&text),
            _ => spans.push(StyledSpan::new(text, look)),
        }
    }
    spans
}

pub fn screen_inner(view_rows: u16, width: u16) -> TermSize {
    TermSize {
        cols: width.saturating_sub(1),
        rows: view_rows.saturating_sub(SCREEN_CHROME_ROWS),
    }
}

fn titled(title: StyledSpan, detail: StyledSpan, room: usize, layout: Layout) -> Vec<StyledSpan> {
    let more = width(Glyphs::of(layout.glyphs).more, layout.width_mode);
    let named = width(&title.text, layout.width_mode) + 2 + more;
    let mut spans = vec![title];
    if named <= room {
        spans.extend([StyledSpan::plain("  "), detail]);
    }
    spans
}

pub fn screen_rows(view: &ScreenView<'_>, room: usize, layout: Layout) -> Vec<StyledLine> {
    if room < 3 {
        return vec![];
    }
    let glyphs = Glyphs::of(layout.glyphs);
    let sep = glyphs.separator;
    let (header, mut footer) = match &view.mode {
        ScreenMode::Typing { asked, warning } => {
            let (detail, tone) = match asked {
                Some(reason) => (reason.clone(), Token::Text),
                None => (view.command.to_owned(), Token::TextMuted),
            };
            let accent = Look::fg(Token::Accent).bold();
            let back = copy::go_back_hint().text();
            let room = left_room(width(&back, layout.width_mode), edge(layout))
                .unwrap_or(0)
                .saturating_sub(1);
            let mut left = vec![StyledSpan::plain(" ")];
            left.extend(titled(
                StyledSpan::new(copy::screen_title(&view.name), accent),
                StyledSpan::new(detail, tone),
                room,
                layout,
            ));
            let title = split(left, vec![StyledSpan::new(back, accent)], layout);
            let footer = if *warning {
                let warning = [StyledSpan::new(copy::attach_note(), Token::Text)];
                wrap(&warning, layout.width.saturating_sub(2), layout.width_mode)
                    .into_iter()
                    .map(|line| {
                        let mut spans = vec![StyledSpan::plain(" ")];
                        spans.extend(line.spans);
                        full(spans, layout)
                    })
                    .collect()
            } else {
                vec![full(vec![], layout)]
            };
            (title, footer)
        }
        ScreenMode::Watching => {
            let mut title = titled(
                StyledSpan::new(copy::screen_title(&view.name), Look::fg(Token::Text).bold()),
                StyledSpan::new(view.command.to_owned(), Token::TextMuted),
                layout.width.saturating_sub(1),
                layout,
            );
            if view.ended {
                title.push(StyledSpan::new(
                    format!(" {sep} {}", copy::SCREEN_ENDED),
                    Token::TextMuted,
                ));
            }
            let keys = copy::screen_keys(view.attachable);
            (
                title_row(title, layout),
                vec![key_row(&keys, "", None, KEY_ROW, " ", layout)],
            )
        }
    };
    let mut out = vec![header];
    let (rows, cols) = view.screen.size();
    let (rows, cols) = (rows.get(), cols.get());
    let (cursor_row, cursor_col) = view.screen.cursor_position();
    footer.truncate(room - 2);
    let shown = room - 1 - footer.len();
    let last = usize::from(rows).max(1);
    let end = (usize::from(cursor_row) + 1).max(shown).min(last);
    let start = end.saturating_sub(shown);
    let caret = !view.screen.hide_cursor() && !view.ended;
    for row in start..end {
        let Ok(r) = u16::try_from(row) else {
            break;
        };
        let cursor = (caret && r == cursor_row).then_some(cursor_col);
        let mut spans = vec![StyledSpan::plain(" ")];
        spans.extend(screen_line(view.screen, r, cols, cursor));
        out.push(full(spans, layout));
    }
    out.extend(footer);
    out
}

pub fn step_in_rows(view: &StepInView<'_>, room: usize, layout: Layout) -> Vec<StyledLine> {
    let title = copy::step_in_title(view.id);
    let options = answer_options(&copy::YES_NO);
    let footer = if view.settling {
        vec![KeyHint::note(copy::CARD_SETTLING)]
    } else if view.typing {
        vec![copy::card_pick(options.len(), layout.glyphs)]
    } else {
        let take = copy::enter_choose();
        let held = view.selected.map(|_| false);
        let mut keys = copy::pick_keys(held, take.clone(), take, None, layout.glyphs);
        if view.selected.is_none() {
            keys.insert(0, copy::card_pick(options.len(), layout.glyphs));
        }
        keys
    };
    let form = FormView {
        title: &title,
        prompt: view.reason,
        options: &options,
        selected: view.selected,
        footer: &footer,
        ..FormView::default()
    };
    form_rows(&form, room, layout).lines
}

#[cfg(test)]
mod tests;
