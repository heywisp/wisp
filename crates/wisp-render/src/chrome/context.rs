use wisp_protocol::context::{
    Category, CategoryEstimate, CompactPoint, ContextReport, EstimateReason, FigureSource, Fill,
    FillPart, ItemEstimate, PointSource, ReportBasis, Window, WindowSource, fill_percent,
    format_tokens, limit_of, unknown_window,
};

use super::row::clip;
use super::{model_line, one_line};
use crate::markdown::Layout;
use crate::num;
use crate::styled::{Look, StyledLine, StyledSpan, push_merged};
use crate::text::{WidthMode, fit_end, spans_width, width, wrap};
use crate::theme::glyphs::Glyphs;
use crate::theme::{Ink, Token};
use crate::transcript::copy;

pub const WARNING_FROM_PERCENT: u64 = 75;

pub const ESTIMATED: &str = "~";

pub const NOT_ESTIMATED: &str = "not estimated";

pub const TITLE: &str = "Usage by category";

pub const NO_REQUEST_YET: &str = "no request yet";

pub const NO_COUNTER: &str = "this server has no token counter";

pub const COUNTING_OFF: &str = "counting is off";

pub const ALL_HINT: &str = "/context all lists every item";

pub const INDENT: usize = 2;

pub const GAP: usize = 3;

pub const LEGEND_MARKER: usize = 2;

pub const LEGEND_LABEL: usize = 24;

pub const LEGEND_FIGURE: usize = 7;

pub const LEGEND_PERCENT: usize = 8;

pub const LEGEND_SOURCE: usize = 11;

pub const LEGEND_ROW: usize = LEGEND_MARKER + LEGEND_LABEL + LEGEND_FIGURE + LEGEND_PERCENT;

pub const LEGEND_WIDTH: usize = LEGEND_ROW + 1;

pub const LEGEND_WITH_SOURCE: usize = LEGEND_ROW + LEGEND_SOURCE;

pub const GRID_ROWS: usize = 10;

pub const NARROW_GRID: usize = 10;

pub const WIDE_GRID: usize = 20;

pub const WIDE_GRID_FROM: u64 = 1_000_000;

pub const NO_GRID_BELOW: usize = 22;

pub const TWO_SECTION_COLUMNS_FROM: usize = 100;

pub const ITEM_NAME: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeterView {
    pub used: u64,
    pub used_estimated: bool,
    pub window: Option<u64>,
    pub limit: Option<u64>,
    pub auto_compact: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterLevel {
    Calm,
    Warning,
    Over,
}

impl MeterLevel {
    pub fn token(self) -> Token {
        match self {
            MeterLevel::Calm => Token::TextMuted,
            MeterLevel::Warning => Token::Text,
            MeterLevel::Over => Token::Error,
        }
    }

    pub fn look(self) -> Look {
        let look = Look::fg(self.token());
        if self == MeterLevel::Warning {
            look.bold()
        } else {
            look
        }
    }

    pub fn fill_look(self) -> Look {
        if self == MeterLevel::Over {
            self.look().bold()
        } else {
            self.look()
        }
    }
}

pub fn meter_base(limit: Option<u64>, window: Option<u64>) -> Option<u64> {
    limit
        .filter(|&limit| limit > 0)
        .or(window)
        .filter(|&base| base > 0)
}

pub fn meter_level(used: u64, limit: Option<u64>, auto_compact: bool) -> MeterLevel {
    let Some(limit) = limit.filter(|&limit| limit > 0) else {
        return MeterLevel::Calm;
    };
    let used = u128::from(used);
    let limit = u128::from(limit);
    if used > limit && !auto_compact {
        MeterLevel::Over
    } else if used * 100 >= limit * u128::from(WARNING_FROM_PERCENT) {
        MeterLevel::Warning
    } else {
        MeterLevel::Calm
    }
}

pub fn meter_text(view: &MeterView) -> String {
    let mark = if view.used_estimated { ESTIMATED } else { "" };
    match meter_base(view.limit, view.window) {
        Some(base) => format!("{mark}{}/{}", format_tokens(view.used), format_tokens(base)),
        None => format!("{mark}{}", format_tokens(view.used)),
    }
}

pub fn meter(view: &MeterView) -> StyledSpan {
    let level = meter_level(view.used, view.limit, view.auto_compact);
    StyledSpan::new(meter_text(view), level.fill_look())
}

#[derive(Clone, Copy, Debug)]
pub struct CellView<'a> {
    pub model: &'a str,
    pub all: bool,
    pub layout: Layout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis<'a> {
    Counted,
    Counting,
    PartlyEstimated(&'a str),
    Failed(&'a str),
    Estimated(Option<&'a str>),
}

pub fn basis(report: &ContextReport) -> Basis<'_> {
    match &report.basis {
        ReportBasis::Counted => Basis::Counted,
        ReportBasis::Counting => Basis::Counting,
        ReportBasis::PartlyEstimated { reason } => Basis::PartlyEstimated(reason),
        ReportBasis::Estimated { reason } => match reason {
            EstimateReason::NoCounter => Basis::Estimated(Some(NO_COUNTER)),
            EstimateReason::Off => Basis::Estimated(Some(COUNTING_OFF)),
            EstimateReason::Failed(reason) => Basis::Failed(reason),
            EstimateReason::NothingYet => Basis::Estimated(Some(NO_REQUEST_YET)),
            EstimateReason::Other(reason) => Basis::Estimated(Some(reason)),
        },
        ReportBasis::Other(_) => Basis::Estimated(None),
    }
}

pub fn title_mode(basis: &Basis<'_>, source_column: bool, glyphs: &Glyphs) -> String {
    match basis {
        Basis::Counted if source_column => "counted by the server".to_owned(),
        Basis::Counted => "counted".to_owned(),
        Basis::Counting => format!("counting{}", glyphs.more),
        Basis::PartlyEstimated(_) => format!("partly estimated ({ESTIMATED})"),
        Basis::Failed(_) | Basis::Estimated(_) => format!("estimated ({ESTIMATED})"),
    }
}

pub fn reason_text(basis: &Basis<'_>) -> Option<String> {
    match basis {
        Basis::PartlyEstimated(reason) | Basis::Failed(reason) => {
            Some(format!("{ESTIMATED} count failed: {}", one_line(reason)))
        }
        Basis::Estimated(Some(reason)) => Some(format!("{ESTIMATED} {}", one_line(reason))),
        Basis::Counted | Basis::Counting | Basis::Estimated(None) => None,
    }
}

pub fn category_label(category: &Category) -> String {
    match category {
        Category::SystemPrompt => "System prompt".to_owned(),
        Category::Tools => "Tools".to_owned(),
        Category::Instructions => "Instructions".to_owned(),
        Category::Environment => "Environment".to_owned(),
        Category::Summary => "Summary".to_owned(),
        Category::UserMessages => "Your messages".to_owned(),
        Category::Replies => "Replies and thinking".to_owned(),
        Category::ToolResults => "Tool results".to_owned(),
        Category::Attachments => "Attachments".to_owned(),
        Category::Other(name) => one_line(name),
    }
}

pub fn category_token(category: &Category) -> Token {
    match category {
        Category::SystemPrompt => Token::Thinking,
        Category::Tools => Token::HueCool,
        Category::Instructions => Token::HueWarm,
        Category::Environment => Token::User,
        Category::Summary => Token::SynType,
        Category::UserMessages => Token::Text,
        Category::Replies => Token::Accent,
        Category::ToolResults => Token::RoleReviewer,
        Category::Attachments => Token::TextMuted,
        Category::Other(_) => Token::TextMuted,
    }
}

pub fn part_look(part: FillPart) -> (&'static str, Token) {
    let (word, category) = match part {
        FillPart::System => ("system", Category::SystemPrompt),
        FillPart::Tools => ("tools", Category::Tools),
        FillPart::Chat => ("chat", Category::Replies),
        FillPart::Skills => ("skills", Category::Instructions),
        FillPart::Unknown => ("", Category::Other(String::new())),
    };
    (word, category_token(&category))
}

pub fn free_look() -> (&'static str, Token) {
    ("free", SPACE_TOKEN)
}

pub fn free_square() -> Ink {
    Ink::mix(Token::Line, Token::Bg, 0.5, Token::Line)
}

pub const CACHED: &str = "cached";

pub const SPACE_TOKEN: Token = Token::TextFaint;

pub fn source_word(source: &FigureSource) -> String {
    match source {
        FigureSource::Counted => "counted".to_owned(),
        FigureSource::Measured => "measured".to_owned(),
        FigureSource::Split => "split".to_owned(),
        FigureSource::Estimated => "estimated".to_owned(),
        FigureSource::Other(word) => one_line(word),
    }
}

fn files(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

fn row_label(estimate: &CategoryEstimate, epoch: u64) -> String {
    let base = category_label(&estimate.category);
    let count = estimate.items.len();
    match &estimate.category {
        Category::Tools | Category::Attachments if count > 0 => format!("{base} ({count})"),
        Category::Instructions if count > 0 => format!("{base} ({})", files(count)),
        Category::Summary => format!("{base} (epoch {epoch})"),
        _ => base,
    }
}

pub fn window_source(source: &WindowSource) -> String {
    match source {
        WindowSource::Session => "session".to_owned(),
        WindowSource::Flag => "flag".to_owned(),
        WindowSource::Endpoint => "account".to_owned(),
        WindowSource::Config => "config".to_owned(),
        WindowSource::Shipped => "shipped".to_owned(),
        WindowSource::Overflow => "overflow".to_owned(),
        WindowSource::Subscription => "subscription".to_owned(),
        WindowSource::Api => "API".to_owned(),
        WindowSource::Other(text) => one_line(text),
    }
}

pub fn point_reason(point: &CompactPoint, glyphs: &Glyphs, shown: fn(u64) -> String) -> String {
    let limit = || format!("window {} reserve", glyphs.minus);
    let source = match &point.set_by {
        PointSource::Default => limit(),
        PointSource::Session => "session".to_owned(),
        PointSource::Flag => "flag".to_owned(),
        PointSource::Endpoint => "account".to_owned(),
        PointSource::Config => "config".to_owned(),
        PointSource::Other(text) => one_line(text),
    };
    match point.capped_from {
        Some(tokens) => format!("{source} {}, capped at {}", shown(tokens), limit()),
        None => source,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Beside { columns: usize, source: bool },
    Below,
    NoGrid,
}

pub const fn grid_width(columns: usize) -> usize {
    columns.saturating_mul(2).saturating_sub(1)
}

pub const fn legend_width(source: bool) -> usize {
    if source {
        LEGEND_WITH_SOURCE
    } else {
        LEGEND_WIDTH
    }
}

pub fn form(width: usize, window: Option<u64>) -> Form {
    let Some(window) = window.filter(|w| *w > 0) else {
        return Form::NoGrid;
    };
    let edge = width.saturating_sub(1);
    let forms = [
        (WIDE_GRID, true),
        (WIDE_GRID, false),
        (NARROW_GRID, true),
        (NARROW_GRID, false),
    ];
    for (columns, source) in forms {
        if columns == WIDE_GRID && window < WIDE_GRID_FROM {
            continue;
        }
        if INDENT + grid_width(columns) + GAP + legend_width(source) <= edge {
            return Form::Beside { columns, source };
        }
    }
    if width >= NO_GRID_BELOW {
        Form::Below
    } else {
        Form::NoGrid
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Used { row: usize },
    Free,
    Reserve,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget<'a> {
    pub rows: &'a [u64],
    pub free: u64,
    pub reserve: u64,
    pub scale: u64,
    pub cells: usize,
}

pub fn rounded(value: u64, max: u64, steps: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    let whole = u128::from(max);
    let scaled = (u128::from(value) * u128::from(steps) * 2 + whole) / (2 * whole);
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

fn largest(counts: &[usize]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, count) in counts.iter().enumerate() {
        if *count > 1 && best.is_none_or(|b| counts[b] < *count) {
            best = Some(i);
        }
    }
    best
}

pub fn allocate(budget: &Budget<'_>) -> Vec<Cell> {
    let cells = budget.cells;
    if budget.scale == 0 || cells == 0 {
        return vec![Cell::Free; cells];
    }
    let whole = u128::from(budget.scale);
    let share = |row: usize| u128::from(budget.rows[row]) * cells as u128;
    let whole_rows: Vec<usize> = (0..budget.rows.len())
        .filter(|&row| share(row) >= whole)
        .collect();
    let mut counts: Vec<usize> = (0..budget.rows.len())
        .map(|row| match share(row) {
            0 => 0,
            s if s < whole => 1,
            s => usize::try_from(s / whole).unwrap_or(cells),
        })
        .collect();
    let target = whole_rows.iter().map(|&row| share(row)).sum::<u128>() / whole;
    let held: u128 = whole_rows.iter().map(|&row| counts[row] as u128).sum();
    for _ in held..target {
        let Some(row) = whole_rows.iter().copied().max_by_key(|&row| {
            let leftover = share(row).saturating_sub(counts[row] as u128 * whole);
            (leftover, budget.rows[row], usize::MAX - row)
        }) else {
            break;
        };
        counts[row] += 1;
    }
    let mut reserved =
        usize::try_from(rounded(budget.reserve, budget.scale, cells as u64)).unwrap_or(cells);
    let mut over = (counts.iter().sum::<usize>() + reserved).saturating_sub(cells);
    let taken = over.min(reserved);
    reserved -= taken;
    over -= taken;
    while over > 0 {
        let Some(i) = largest(&counts) else {
            break;
        };
        counts[i] -= 1;
        over -= 1;
    }
    let left = cells.saturating_sub(counts.iter().sum::<usize>() + reserved);
    if budget.free == 0 && left > 0 {
        if budget.reserve > 0 {
            reserved += left;
        } else if let Some(i) = (0..counts.len()).max_by_key(|&i| (counts[i], usize::MAX - i)) {
            counts[i] += left;
        }
    }
    let mut out = Vec::with_capacity(cells);
    for (row, &count) in counts.iter().enumerate() {
        out.extend(std::iter::repeat_n(Cell::Used { row }, count));
    }
    let free = cells.saturating_sub(out.len() + reserved);
    out.extend(std::iter::repeat_n(Cell::Free, free));
    out.extend(std::iter::repeat_n(Cell::Reserve, reserved));
    out.truncate(cells);
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Space {
    pub free: u64,
    pub reserve: u64,
    pub over: u64,
    pub scale: u64,
}

pub fn space(report: &ContextReport) -> Option<Space> {
    let window = report.window.as_ref().filter(|w| w.tokens > 0)?.tokens;
    let point = report
        .compact_at
        .as_ref()
        .map_or(window, |p| p.tokens.min(window));
    let next = report.next;
    Some(Space {
        free: point.saturating_sub(next),
        reserve: window.saturating_sub(next.max(point)),
        over: next.saturating_sub(window),
        scale: window.max(next),
    })
}

pub fn share(part: u64, whole: u64) -> String {
    if whole == 0 {
        return String::new();
    }
    if part == 0 {
        return "0.0%".to_owned();
    }
    let tenths = rounded(part, whole, 1_000);
    if tenths == 0 {
        return "<0.1%".to_owned();
    }
    format!("{}.{}%", tenths / 10, tenths % 10)
}

pub fn fill_tokens(report: &ContextReport) -> u64 {
    match report.fill {
        Fill::Measured { used, .. } => used,
        Fill::EstimatedAfterCompaction { tokens, .. } => tokens,
        Fill::None | Fill::Other(_) => report.next,
    }
}

fn clipped(text: &str, room: usize, view: &CellView<'_>) -> String {
    let more = Glyphs::of(view.layout.glyphs).more;
    fit_end(text, room, more, view.layout.width_mode)
}

fn padded(text: &str, room: usize, view: &CellView<'_>) -> String {
    crate::text::padded(&clipped(text, room, view), room, view.layout.width_mode)
}

fn wrapped(text: &str, look: Look, room: usize, view: &CellView<'_>) -> Vec<Vec<StyledSpan>> {
    wrap(
        &[StyledSpan::new(text, look)],
        room.max(1),
        view.layout.width_mode,
    )
    .into_iter()
    .map(|line| line.spans)
    .collect()
}

fn one(text: String, look: impl Into<Look>) -> Vec<StyledSpan> {
    vec![StyledSpan::new(text, look)]
}

fn mark(source: &FigureSource) -> &'static str {
    if source.approximate() { ESTIMATED } else { "" }
}

fn meter_limit(report: &ContextReport) -> Option<u64> {
    let ceiling = report
        .window
        .as_ref()
        .zip(report.reserve)
        .map(|(window, reserve)| window.tokens.saturating_sub(reserve));
    limit_of(
        report.compact_at.as_ref().map(|p| p.tokens),
        ceiling,
        report.auto_compact,
    )
}

fn header(report: &ContextReport, room: usize, view: &CellView<'_>) -> Vec<Vec<StyledSpan>> {
    let glyphs = Glyphs::of(view.layout.glyphs);
    let model = model_line(view.model);
    let endpoint = one_line(&report.endpoint);
    let who = if endpoint.is_empty() {
        model
    } else {
        format!("{model} {} {endpoint}", glyphs.separator)
    };
    let mut lines = vec![one(clipped(&who, room, view), Look::default())];
    let counted_total = matches!(report.fill, Fill::None) && report.basis == ReportBasis::Counted;
    let fill_mark = match report.fill {
        Fill::Measured { .. } => "",
        _ if counted_total => "",
        _ => ESTIMATED,
    };
    let next = num(report.next);
    let limit = meter_limit(report);
    let size = match meter_base(limit, report.window.as_ref().map(|w| w.tokens)) {
        Some(base) => format!(
            "{fill_mark}{next} / {} tokens ({}%)",
            format_tokens(base),
            fill_percent(report.next, base)
        ),
        None => format!("{fill_mark}{next} tokens"),
    };
    let level = meter_level(report.next, limit, report.auto_compact);
    lines.push(one(clipped(&size, room, view), level.fill_look()));
    let sep = glyphs.separator;
    let source = match report.fill {
        Fill::Measured { used, turn, step } => {
            let since = num(report.next.saturating_sub(used));
            let long = format!(
                "{} measured at turn {turn}, step {step} {sep} {since} since",
                num(used)
            );
            if width(&long, view.layout.width_mode) <= room {
                long
            } else {
                format!("{} measured {sep} {since} since", num(used))
            }
        }
        Fill::EstimatedAfterCompaction { .. } => "estimated: compacted, no request yet".to_owned(),
        Fill::None if counted_total => "no request yet: counted".to_owned(),
        Fill::None if report.basis == ReportBasis::Counting => {
            format!("no request yet: counting{}", glyphs.more)
        }
        Fill::None | Fill::Other(_) => "no request yet: estimated".to_owned(),
    };
    lines.push(one(clipped(&source, room, view), Token::TextMuted));
    lines
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Columns {
    label: usize,
    percent: bool,
    source: bool,
}

impl Columns {
    fn row(self) -> usize {
        LEGEND_MARKER
            + self.label
            + LEGEND_FIGURE
            + if self.percent { LEGEND_PERCENT } else { 0 }
            + if self.source { LEGEND_SOURCE } else { 0 }
    }
}

fn columns(room: usize, source: bool, percent: bool) -> Columns {
    let fixed = LEGEND_MARKER + LEGEND_FIGURE + if source { LEGEND_SOURCE } else { 0 };
    let with_percent = room.saturating_sub(fixed + LEGEND_PERCENT);
    if !percent {
        Columns {
            label: room.saturating_sub(fixed).clamp(1, LEGEND_LABEL),
            percent,
            source,
        }
    } else if with_percent >= LEGEND_LABEL / 3 {
        Columns {
            label: with_percent.min(LEGEND_LABEL),
            percent,
            source,
        }
    } else {
        Columns {
            label: room.saturating_sub(fixed).clamp(1, LEGEND_LABEL),
            percent: false,
            source,
        }
    }
}

struct Entry {
    marker: StyledSpan,
    label: String,
    figure: String,
    tokens: u64,
    source: Option<String>,
}

fn entry(
    entry: &Entry,
    cols: Columns,
    window: Option<u64>,
    room: usize,
    view: &CellView<'_>,
) -> Vec<StyledSpan> {
    let mut spans = vec![
        entry.marker.clone(),
        StyledSpan::plain(" "),
        StyledSpan::plain(padded(&entry.label, cols.label, view)),
        StyledSpan::plain(format!("{:>LEGEND_FIGURE$}", entry.figure)),
    ];
    if cols.percent {
        let percent = window.map(|w| share(entry.tokens, w)).unwrap_or_default();
        spans.push(StyledSpan::plain(format!("{percent:>LEGEND_PERCENT$}")));
    }
    if cols.source
        && let Some(source) = &entry.source
    {
        let room = LEGEND_SOURCE - 2;
        spans.push(StyledSpan::new(
            format!("  {}", clipped(source, room, view)),
            Token::TextMuted,
        ));
    }
    let more = Glyphs::of(view.layout.glyphs).more;
    clip(spans, room, more, view.layout.width_mode)
}

fn title_line(mode: &str, room: usize, view: &CellView<'_>) -> Vec<Vec<StyledSpan>> {
    let used = width(TITLE, view.layout.width_mode) + width(mode, view.layout.width_mode);
    if used + 2 <= room {
        vec![vec![
            StyledSpan::new(TITLE, Look::default().bold()),
            StyledSpan::plain(" ".repeat(room - used)),
            StyledSpan::new(mode, Token::TextMuted),
        ]]
    } else {
        vec![
            one(clipped(TITLE, room, view), Look::default().bold()),
            one(clipped(mode, room, view), Token::TextMuted),
        ]
    }
}

fn point_lines(report: &ContextReport, glyphs: &Glyphs) -> Vec<String> {
    let mut lines = vec![];
    if let Some(point) = &report.compact_at {
        let lead = if report.auto_compact {
            "Compacts at"
        } else {
            "Would compact at"
        };
        lines.push(format!(
            "{lead} {} ({})",
            num(point.tokens),
            point_reason(point, glyphs, num)
        ));
    }
    if !report.auto_compact {
        lines.push("automatic compaction is off".to_owned());
    }
    if let Some(window) = &report.window {
        lines.push(format!("window {}", window_text(window)));
    }
    lines
}

pub fn window_text(window: &Window) -> String {
    let source = window_source(&window.source);
    match window.documented {
        Some(documented) => format!(
            "{} ({source}; documented {})",
            format_tokens(window.tokens),
            format_tokens(documented)
        ),
        None => format!("{} ({source})", format_tokens(window.tokens)),
    }
}

fn legend(
    report: &ContextReport,
    basis: &Basis<'_>,
    cols: Columns,
    room: usize,
    view: &CellView<'_>,
) -> Vec<Vec<StyledSpan>> {
    let glyphs = Glyphs::of(view.layout.glyphs);
    let window = report
        .window
        .as_ref()
        .filter(|w| w.tokens > 0)
        .map(|w| w.tokens);
    let title_room = cols.row().min(room);
    let mut lines = title_line(&title_mode(basis, cols.source, &glyphs), title_room, view);
    for estimate in &report.categories {
        let row = Entry {
            marker: StyledSpan::new(
                glyphs.cell_full,
                Look::fg(category_token(&estimate.category)),
            ),
            label: row_label(estimate, report.epoch),
            figure: format!("{}{}", mark(&estimate.source), num(estimate.tokens)),
            tokens: estimate.tokens,
            source: Some(source_word(&estimate.source)),
        };
        lines.push(entry(&row, cols, window, room, view));
    }
    let text_room = room.min(cols.row().max(LEGEND_WIDTH));
    if let Some(space) = space(report) {
        let (free_label, reserve_label) = if report.auto_compact {
            ("Free before compaction", "Compaction reserve")
        } else {
            ("Free before the point", "Reserve past the point")
        };
        let past = report
            .compact_at
            .as_ref()
            .is_some_and(|p| report.next > p.tokens);
        let rows = [
            Entry {
                marker: StyledSpan::new(glyphs.cell_free, SPACE_TOKEN),
                label: free_label.to_owned(),
                figure: format!("{ESTIMATED}{}", num(space.free)),
                tokens: space.free,
                source: None,
            },
            Entry {
                marker: StyledSpan::new(glyphs.cell_reserve, SPACE_TOKEN),
                label: reserve_label.to_owned(),
                figure: format!(
                    "{}{}",
                    if past { ESTIMATED } else { "" },
                    num(space.reserve)
                ),
                tokens: space.reserve,
                source: None,
            },
        ];
        for row in &rows {
            lines.push(entry(row, cols, window, room, view));
        }
        for line in point_lines(report, &glyphs) {
            for mut spans in wrapped(
                &line,
                Token::TextMuted.into(),
                text_room.saturating_sub(2).max(1),
                view,
            ) {
                spans.insert(0, StyledSpan::plain("  "));
                lines.push(spans);
            }
        }
        if space.over > 0 {
            let over = format!("over the window by {}", num(space.over));
            lines.extend(wrapped(&over, Token::Error.into(), text_room, view));
        }
    } else {
        let unknown = unknown_window(&one_line(view.model));
        lines.extend(wrapped(&unknown, Token::Text.into(), room, view));
    }
    if let Some(reason) = reason_text(basis) {
        lines.extend(wrapped(&reason, Token::TextMuted.into(), text_room, view));
    }
    lines
}

fn grid_rows(report: &ContextReport, columns: usize, glyphs: &Glyphs) -> Vec<Vec<StyledSpan>> {
    let Some(space) = space(report) else {
        return vec![];
    };
    let rows: Vec<u64> = report.categories.iter().map(|c| c.tokens).collect();
    let cells = allocate(&Budget {
        rows: &rows,
        free: space.free,
        reserve: space.reserve,
        scale: space.scale,
        cells: columns * GRID_ROWS,
    });
    let used = |row: usize| {
        let category = report.categories.get(row);
        Look::fg(category.map_or(Token::TextMuted, |c| category_token(&c.category)))
    };
    cell_rows(&cells, columns, used, glyphs)
}

pub fn cell_span(cell: Cell, used: impl Fn(usize) -> Look, glyphs: &Glyphs) -> StyledSpan {
    match cell {
        Cell::Used { row } => StyledSpan::new(glyphs.cell_full, used(row)),
        Cell::Free => StyledSpan::new(glyphs.cell_free, SPACE_TOKEN),
        Cell::Reserve => StyledSpan::new(glyphs.cell_reserve, SPACE_TOKEN),
    }
}

pub fn cell_rows(
    cells: &[Cell],
    columns: usize,
    used: impl Fn(usize) -> Look,
    glyphs: &Glyphs,
) -> Vec<Vec<StyledSpan>> {
    cells
        .chunks(columns.max(1))
        .map(|chunk| {
            let mut spans = vec![];
            for (i, &cell) in chunk.iter().enumerate() {
                if i > 0 {
                    spans.push(StyledSpan::plain(" "));
                }
                spans.push(cell_span(cell, &used, glyphs));
            }
            spans
        })
        .collect()
}

pub fn cell_bar(
    value: u64,
    max: u64,
    cells: usize,
    look: Look,
    glyphs: &Glyphs,
) -> Vec<StyledSpan> {
    let budget = Budget {
        rows: &[value],
        free: max.saturating_sub(value),
        reserve: 0,
        scale: max.max(value),
        cells,
    };
    cell_run(allocate(&budget), |_| look, glyphs)
}

pub fn cell_run(
    cells: impl IntoIterator<Item = Cell>,
    used: impl Fn(usize) -> Look,
    glyphs: &Glyphs,
) -> Vec<StyledSpan> {
    let mut spans = vec![];
    for cell in cells {
        push_merged(&mut spans, cell_span(cell, &used, glyphs));
    }
    spans
}

pub fn beside(
    mut left: Vec<Vec<StyledSpan>>,
    mut right: Vec<Vec<StyledSpan>>,
    left_width: usize,
    mode: WidthMode,
) -> Vec<Vec<StyledSpan>> {
    let height = left.len().max(right.len());
    left.resize(height, vec![]);
    right.resize(height, vec![]);
    left.into_iter()
        .zip(right)
        .map(|(mut spans, right)| {
            if !right.is_empty() {
                let used = spans_width(&spans, mode);
                spans.push(StyledSpan::plain(
                    " ".repeat(left_width.saturating_sub(used)),
                ));
                spans.extend(right);
            }
            spans
        })
        .collect()
}

pub fn cell_tokens(scale: u64, cells: usize) -> u64 {
    if cells == 0 {
        return 0;
    }
    let cells = cells as u128;
    let each = (u128::from(scale) + cells / 2) / cells;
    u64::try_from(each).unwrap_or(u64::MAX)
}

fn caption(report: &ContextReport, columns: usize) -> String {
    let scale = space(report).map_or(0, |s| s.scale);
    format!("1 cell = {}", num(cell_tokens(scale, columns * GRID_ROWS)))
}

fn line(spans: Vec<StyledSpan>) -> StyledLine {
    let mut all = vec![StyledSpan::plain(" ".repeat(INDENT))];
    all.extend(spans);
    StyledLine::new(all)
}

fn block(report: &ContextReport, basis: &Basis<'_>, view: &CellView<'_>) -> Vec<StyledLine> {
    let glyphs = Glyphs::of(view.layout.glyphs);
    let full = view.layout.width.saturating_sub(1 + INDENT).max(1);
    let window = report.window.as_ref().filter(|w| w.tokens > 0);
    match form(view.layout.width, window.map(|w| w.tokens)) {
        Form::Beside { columns, source } => {
            let left_width = grid_width(columns) + GAP;
            let room = full.saturating_sub(left_width).max(1);
            let mut left = grid_rows(report, columns, &glyphs);
            left.push(vec![]);
            left.push(one(caption(report, columns), Token::TextMuted));
            let mut right = header(report, room.min(legend_width(source)), view);
            right.push(vec![]);
            let cols = Columns {
                label: LEGEND_LABEL,
                percent: true,
                source,
            };
            right.extend(legend(report, basis, cols, room, view));
            beside(left, right, left_width, view.layout.width_mode)
                .into_iter()
                .map(line)
                .collect()
        }
        form => {
            let mut lines: Vec<StyledLine> = header(report, full.min(LEGEND_WIDTH), view)
                .into_iter()
                .map(line)
                .collect();
            lines.push(StyledLine::default());
            if form == Form::Below {
                lines.extend(
                    grid_rows(report, NARROW_GRID, &glyphs)
                        .into_iter()
                        .map(line),
                );
                lines.push(line(one(caption(report, NARROW_GRID), Token::TextMuted)));
                lines.push(StyledLine::default());
            }
            let cols = columns(full, false, window.is_some());
            lines.extend(
                legend(report, basis, cols, full, view)
                    .into_iter()
                    .map(line),
            );
            lines
        }
    }
}

pub const SECTION_ORDER: [Category; 4] = [
    Category::Instructions,
    Category::Tools,
    Category::ToolResults,
    Category::Attachments,
];

struct Section {
    title: String,
    items: Vec<String>,
    rows: Vec<String>,
}

fn section(estimate: &CategoryEstimate, view: &CellView<'_>) -> Option<Section> {
    if estimate.items.is_empty() {
        return None;
    }
    let glyphs = Glyphs::of(view.layout.glyphs);
    let sep = glyphs.separator;
    let figure = format!("{}{}", mark(&estimate.source), num(estimate.tokens));
    let count = estimate.items.len();
    let title = match &estimate.category {
        Category::Instructions => format!("Instructions {sep} {} {sep} {figure}", files(count)),
        Category::ToolResults => {
            format!("Tool results {sep} {figure} {sep} largest {count}")
        }
        other => format!("{} {sep} {count} {sep} {figure}", category_label(other)),
    };
    let items = estimate
        .items
        .iter()
        .map(|item| {
            let turn = item
                .turn
                .map(|t| format!(" (turn {t})"))
                .unwrap_or_default();
            format!("{} {}{turn}", one_line(&item.name), item_figure(item))
        })
        .collect();
    let rows = estimate
        .items
        .iter()
        .map(|item| {
            let turn = item.turn.map(|t| format!("  turn {t}")).unwrap_or_default();
            let name = padded(&one_line(&item.name), ITEM_NAME, view);
            format!("  {name} {:>LEGEND_FIGURE$}{turn}", item_figure(item))
        })
        .collect();
    Some(Section { title, items, rows })
}

fn item_figure(item: &ItemEstimate) -> String {
    if item.unestimated {
        NOT_ESTIMATED.to_owned()
    } else {
        format!("{ESTIMATED}{}", num(item.tokens))
    }
}

fn section_lines(section: &Section, room: usize, all: bool, view: &CellView<'_>) -> Vec<String> {
    let glyphs = Glyphs::of(view.layout.glyphs);
    let mut lines = vec![clipped(&section.title, room, view)];
    if all {
        lines.extend(section.rows.iter().map(|row| clipped(row, room, view)));
    } else {
        let joined = section.items.join(&format!(" {} ", glyphs.separator));
        lines.push(clipped(&format!("{} {joined}", glyphs.elbow), room, view));
    }
    lines
}

fn sections(report: &ContextReport, view: &CellView<'_>) -> Vec<StyledLine> {
    let mut ordered: Vec<&CategoryEstimate> = report.categories.iter().collect();
    ordered.sort_by_key(|c| {
        SECTION_ORDER
            .iter()
            .position(|o| *o == c.category)
            .unwrap_or(SECTION_ORDER.len())
    });
    let found: Vec<Section> = ordered
        .into_iter()
        .filter_map(|c| section(c, view))
        .collect();
    let full = view.layout.width.saturating_sub(1 + INDENT).max(1);
    let body = |text: String| line(one(text, Look::default()));
    if view.all || view.layout.width < TWO_SECTION_COLUMNS_FROM {
        return found
            .iter()
            .flat_map(|s| section_lines(s, full, view.all, view))
            .map(body)
            .collect();
    }
    let half = full.saturating_sub(GAP) / 2;
    let mut out = vec![];
    for pair in found.chunks(2) {
        let left = pair
            .first()
            .map(|s| section_lines(s, half, false, view))
            .unwrap_or_default();
        let right = pair
            .get(1)
            .map(|s| section_lines(s, half, false, view))
            .unwrap_or_default();
        for i in 0..left.len().max(right.len()) {
            let l = left.get(i).cloned().unwrap_or_default();
            let text = match right.get(i) {
                Some(r) => format!("{}{r}", padded(&l, half + GAP, view)),
                None => l,
            };
            out.push(body(text));
        }
    }
    out
}

fn cost_line(report: &ContextReport, glyphs: &Glyphs) -> String {
    let quote =
        |usd: f64| wisp_protocol::marked_amount(&wisp_protocol::format_usd(usd), true, false);
    let fill = num(fill_tokens(report));
    let sep = format!(" {} ", glyphs.separator);
    let mut parts = vec![];
    match report.reread_cost_usd {
        Some(cost) => parts.push(format!(
            "Each step re-reads {fill}: {}",
            copy::Fact::joined(&copy::Fact::billed_cost(cost, false, report.billing), &sep)
        )),
        None => parts.push(format!("Each step re-reads {fill}")),
    }
    match (
        report.compact_now_cost_usd,
        report.compact_now_head_write_usd,
    ) {
        (Some(now), Some(head)) => parts.push(format!(
            "compacting now {}, then {} to write the new head",
            quote(now),
            quote(head)
        )),
        (Some(now), None) => parts.push(format!("compacting now {}", quote(now))),
        _ => {}
    }
    if report.reread_cost_usd.is_none() && report.compact_now_cost_usd.is_none() {
        parts.push(copy::NO_PRICES.to_owned());
    }
    let spent = report.compaction_cost_usd.is_some_and(|usd| usd > 0.0);
    if report.compactions > 0 || spent {
        parts.push(copy::compactions_so_far(
            report.compactions,
            report
                .compaction_cost_usd
                .map(|cost| {
                    let billing = report.spent_billing;
                    let spend = wisp_protocol::AccountSpend::of(
                        cost,
                        report.compaction_plan_usd,
                        billing,
                        false,
                    );
                    copy::Fact::joined(&copy::total_facts(spend, billing), &sep)
                })
                .as_deref(),
        ));
    }
    parts.join(&sep)
}

pub fn context_cell(report: &ContextReport, view: &CellView<'_>) -> Vec<StyledLine> {
    context_cell_as(report, &basis(report), view)
}

pub fn context_cell_as(
    report: &ContextReport,
    basis: &Basis<'_>,
    view: &CellView<'_>,
) -> Vec<StyledLine> {
    let glyphs = Glyphs::of(view.layout.glyphs);
    let full = view.layout.width.saturating_sub(1 + INDENT).max(1);
    let mut lines = block(report, basis, view);
    let found = sections(report, view);
    if !found.is_empty() {
        lines.push(StyledLine::default());
        lines.extend(found);
    }
    lines.push(StyledLine::default());
    let cost = cost_line(report, &glyphs);
    lines.extend(
        wrapped(&cost, Look::default(), full, view)
            .into_iter()
            .map(line),
    );
    if !view.all && report.categories.iter().any(|c| !c.items.is_empty()) {
        lines.extend(
            wrapped(ALL_HINT, Token::TextMuted.into(), full, view)
                .into_iter()
                .map(line),
        );
    }
    lines
}

#[cfg(test)]
mod tests;
