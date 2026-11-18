use std::collections::BTreeMap;

use wisp_protocol::plan_usage::{PlanUsageError, UsageState, UsageWindow};
use wisp_protocol::{AccountSpend, marked_amount};

use crate::chrome::account::{
    AccountKind, AccountState, billing_token, mark_line, mark_spans, marked,
};
use crate::chrome::{
    KEY_ROW, KeyHint, THIS_CHAT, WIDE_FROM,
    context::{MeterLevel, cell_bar},
    key_row, one_line, selected_row, selection_mark,
};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{spans_width, width, wrap};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spend {
    pub amount: AccountSpend,
    pub priced: bool,
}

pub(super) type Part = (Option<AccountState>, String);

impl Spend {
    fn lower(self) -> bool {
        self.amount.lower_bound || !self.priced
    }

    pub(super) fn parts(self, api: bool, subscription: bool) -> Vec<Part> {
        let usd = self.amount.usd;
        if !self.priced && usd == 0.0 {
            return vec![(Some(AccountState::Api), copy::NOT_PRICED.into())];
        }
        let Some((paid, plan)) = self.amount.api_usd().zip(self.amount.plan_usd) else {
            return vec![(
                None,
                marked_amount(&copy::usd(usd), true, self.lower()).to_string(),
            )];
        };
        let mut parts = vec![];
        if api || paid > 0.0 {
            parts.push((
                Some(AccountState::Api),
                marked_amount(&copy::usd(paid), false, self.lower()).to_string(),
            ));
        }
        if subscription || plan > 0.0 {
            parts.push((
                Some(AccountState::Subscription),
                marked_amount(&copy::usd(plan), true, self.lower()).to_string(),
            ));
        }
        parts
    }
}

fn is_api(kind: AccountKind) -> bool {
    kind.billing() == AccountState::Api
}

pub struct AccountView<'a> {
    pub alias: &'a str,
    pub mark: Option<u8>,
    pub kind: AccountKind,
    pub state: &'a UsageState,
    pub unavailable: Option<&'a str>,
    pub spend: Spend,
    pub active: bool,
    pub focused: bool,
    pub refused: Option<Refused>,
    pub saved_key: bool,
    pub off: bool,
}

pub struct Refused {
    pub fix: Option<String>,
}

impl AccountView<'_> {
    pub fn signs_in(&self) -> bool {
        !self.off
            && !is_api(self.kind)
            && (self.unavailable.is_some()
                || matches!(
                    self.state.error,
                    Some(PlanUsageError::AuthenticationRequired | PlanUsageError::ScopeRequired)
                ))
    }

    pub fn changes_key(&self) -> bool {
        is_api(self.kind) && self.saved_key
    }

    fn parts(&self) -> Vec<Part> {
        let api = is_api(self.kind);
        self.spend.parts(api, !api)
    }
}

pub struct AllChats {
    pub accounts: BTreeMap<String, [Spend; 2]>,
    pub today: Spend,
    pub window: Spend,
    pub label: String,
    pub days: Vec<(String, Spend)>,
}

pub struct UsagePage<'a> {
    pub accounts: &'a [AccountView<'a>],
    pub total: Spend,
    pub all: Option<&'a AllChats>,
    pub now: i64,
    pub scroll: usize,
}

impl UsagePage<'_> {
    fn kinds(&self) -> (bool, bool) {
        let window = self.all.map(|all| all.window.amount);
        let api = self.accounts.iter().any(|a| is_api(a.kind))
            || window.is_some_and(|s| s.api_usd().is_none_or(|usd| usd > 0.0));
        let subscription = self.accounts.iter().any(|a| !is_api(a.kind))
            || window.is_some_and(|s| s.plan_usd.is_some_and(|usd| usd > 0.0));
        (api || !subscription, subscription || !api)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetTime {
    pub short: String,
    pub brief: String,
    pub long: String,
}

pub fn percent(value: wisp_protocol::plan_usage::Percent) -> String {
    let value = value.value();
    if value.fract() == 0.0 {
        format!("{value:.0}%")
    } else {
        format!("{value}%")
    }
}

pub fn severity(window: &UsageWindow) -> MeterLevel {
    match window.used_percent.map(|p| p.value()) {
        Some(p) if p >= 90.0 => MeterLevel::Over,
        Some(p) if p >= 75.0 => MeterLevel::Warning,
        _ => MeterLevel::Calm,
    }
}

pub fn gauge(
    window: &UsageWindow,
    cells: usize,
    look: Look,
    layout: Layout,
) -> Option<Vec<StyledSpan>> {
    let percent = window.used_percent?.value();
    let hundredths = ((percent * 100.0) as u64).max(u64::from(percent > 0.0));
    let glyphs = Glyphs::of(layout.glyphs);
    Some(cell_bar(hundredths, 10_000, cells, look, &glyphs))
}

pub fn is_stale(state: &UsageState, window: &UsageWindow, now: i64) -> bool {
    state.stale || window.stale(now)
}

fn passed(window: &UsageWindow, now: i64) -> bool {
    window.reset_passed(now)
}

fn lines(
    text: impl Into<String>,
    indent: usize,
    token: impl Into<crate::styled::Look>,
    layout: Layout,
) -> Vec<StyledLine> {
    let indent = indent.min(layout.width.saturating_sub(1));
    wrap(
        &[StyledSpan::new(
            Glyphs::of(layout.glyphs).fold(&one_line(&text.into())),
            token,
        )],
        layout.width.saturating_sub(indent + 1).max(1),
        layout.width_mode,
    )
    .into_iter()
    .map(|mut line| {
        line.spans.insert(0, StyledSpan::plain(" ".repeat(indent)));
        line
    })
    .collect()
}

fn paired(left: &str, right: &str, indent: usize, layout: Layout) -> Vec<StyledLine> {
    let room = layout.width.saturating_sub(indent + 1);
    let used = width(left, layout.width_mode) + width(right, layout.width_mode);
    if used < room {
        vec![StyledLine::new(vec![
            StyledSpan::plain(" ".repeat(indent)),
            StyledSpan::new(left, Token::Text),
            StyledSpan::plain(" ".repeat(room - used)),
            StyledSpan::new(right, Token::TextMuted),
        ])]
    } else {
        let mut out = lines(left, indent, Token::Text, layout);
        out.extend(lines(right, indent + 2, Token::TextMuted, layout));
        out
    }
}

const INDENT: usize = 4;
const API_EQUIVALENT: &str = "API equivalent";
const SPENT: &str = "Spent";

pub(super) struct Columns {
    label: usize,
    bar: usize,
    gaps: [usize; 3],
    percent: usize,
    amounts: usize,
}

impl Columns {
    pub(super) fn of(view: &UsagePage<'_>, layout: Layout) -> Self {
        let mode = layout.width_mode;
        let windows = || {
            view.accounts
                .iter()
                .filter_map(|a| a.state.snapshot.as_ref())
                .flat_map(|s| &s.windows)
        };
        let label = windows()
            .map(|w| width(&window_name(w), mode))
            .fold(6, usize::max);
        let percent = windows()
            .filter(|w| !passed(w, view.now))
            .map(|w| width(&measured(w), mode))
            .fold(4, usize::max);
        let (bar, gaps): (usize, _) = match layout.width {
            ..60 => (8, [1, 1, 2]),
            60..WIDE_FROM => (18, [3, 3, 3]),
            _ => (30, [3, 3, 3]),
        };
        let mut columns = Self {
            label,
            bar: bar.saturating_sub(percent.saturating_sub(4)),
            gaps,
            percent,
            amounts: 0,
        };
        let block = view
            .accounts
            .iter()
            .flat_map(|a| a.parts())
            .map(|part| INDENT + width(amount_label(&part), mode) + 2 + width(&part.1, mode));
        let total = view
            .total
            .parts(view.kinds().0, view.kinds().1)
            .into_iter()
            .map(|part| {
                2 + width(&copy::sentence_case(THIS_CHAT), mode) + 2 + width(&part.1, mode)
            });
        columns.amounts = block.chain(total).fold(columns.percents(), usize::max);
        if let Some(all) = view.all {
            let (api, subscription) = view.kinds();
            let mut amounts = vec![
                (
                    2,
                    copy::all_chats_label(&copy::TODAY.to_lowercase()),
                    all.today.parts(api, subscription),
                ),
                (
                    2,
                    copy::all_chats_label(&all.label),
                    all.window.parts(api, subscription),
                ),
            ];
            for account in view.accounts {
                if let Some(spend) = all.accounts.get(account.alias) {
                    let api = is_api(account.kind);
                    amounts.extend([
                        (
                            INDENT,
                            copy::account_chats_label(&copy::TODAY.to_lowercase()),
                            spend[0].parts(api, !api),
                        ),
                        (
                            INDENT,
                            copy::account_chats_label(&all.label),
                            spend[1].parts(api, !api),
                        ),
                    ]);
                }
            }
            amounts.extend(
                all.days
                    .iter()
                    .take(DAY_ROWS)
                    .map(|(label, spend)| (INDENT, label.clone(), spend.parts(api, subscription))),
            );
            for (indent, label, parts) in amounts {
                for part in parts {
                    columns.amounts = columns
                        .amounts
                        .max(indent + width(&label, mode) + 2 + width(&part.1, mode));
                }
            }
        }
        columns
    }

    fn percents(&self) -> usize {
        INDENT + self.label + self.gaps[0] + self.bar + self.gaps[1] + self.percent
    }

    fn actions(&self) -> usize {
        self.percents() + self.gaps[2]
    }
}

fn amount_label(part: &Part) -> &'static str {
    if part.0 == Some(AccountState::Subscription) {
        API_EQUIVALENT
    } else {
        SPENT
    }
}

fn measured(window: &UsageWindow) -> String {
    window
        .used_percent
        .map_or_else(|| "unknown".into(), percent)
}

fn window_name(window: &UsageWindow) -> String {
    super::window_names(window).map_or_else(|| one_line(&window.label), |(_, long)| long.into())
}

fn padded(text: &str, column: usize, layout: Layout) -> String {
    " ".repeat(column.saturating_sub(width(text, layout.width_mode)))
}

fn state_rows(
    state: &str,
    action: Option<&str>,
    columns: &Columns,
    layout: Layout,
) -> Vec<StyledLine> {
    let mut out = lines(state, INDENT, Token::Text, layout);
    let Some(action) = action else {
        return out;
    };
    let mode = layout.width_mode;
    if let [line] = out.as_mut_slice() {
        let used = spans_width(&line.spans, mode);
        let gap = if layout.width < 60 {
            format!(" {} ", Glyphs::of(layout.glyphs).separator)
        } else {
            " ".repeat(columns.actions().saturating_sub(used).max(3))
        };
        if used + width(&gap, mode) + width(action, mode) < layout.width {
            line.spans.push(StyledSpan::plain(gap));
            line.spans.push(StyledSpan::new(action, Token::TextMuted));
            return out;
        }
    }
    out.extend(lines(action, INDENT, Token::TextMuted, layout));
    out
}

pub const LIMIT_REACHED: &str = "limit reached";

fn sign_in_again() -> KeyHint {
    KeyHint::new(copy::ENTER_KEY, "sign in again")
}

const KEY_REFUSED: &str = "Key refused";

fn refused_line(refused: &Refused) -> String {
    let Some(fix) = &refused.fix else {
        return KEY_REFUSED.to_owned();
    };
    let fix = fix.strip_suffix('.').unwrap_or(fix);
    let mut chars = fix.chars();
    let first: String = chars
        .next()
        .into_iter()
        .flat_map(char::to_lowercase)
        .collect();
    format!("{KEY_REFUSED}: {first}{}", chars.as_str())
}

const RESET_SINCE: &str = "reset since";

fn window_rows(
    window: &UsageWindow,
    account: &AccountView<'_>,
    columns: &Columns,
    now: i64,
    layout: Layout,
    time: &impl Fn(i64) -> ResetTime,
) -> Vec<StyledLine> {
    let stale = is_stale(account.state, window, now);
    let quiet = stale || account.state.loading;
    let look = if quiet {
        Token::TextMuted.into()
    } else {
        severity(window).look()
    };
    let name = window_name(window);
    let [before, between, after] = columns.gaps.map(|gap| StyledSpan::plain(" ".repeat(gap)));
    let label = format!("{name}{}", padded(&name, columns.label, layout));
    if passed(window, now) {
        return vec![StyledLine::new(vec![
            StyledSpan::plain(" ".repeat(INDENT)),
            StyledSpan::new(label, Token::TextMuted),
            before,
            StyledSpan::new(RESET_SINCE, Token::TextMuted),
        ])];
    }
    let bar = gauge(window, columns.bar, look, layout)
        .unwrap_or_else(|| vec![StyledSpan::plain(" ".repeat(columns.bar))]);
    let reached = window.used_percent.is_some_and(|p| p.value() >= 100.0);
    let reset = if stale {
        copy::ago(u64::try_from(now.saturating_sub(window.observed_at)).unwrap_or(0))
    } else {
        window.resets_at.map_or_else(String::new, |at| {
            let time = time(at);
            let when = if layout.width >= WIDE_FROM {
                time.long
            } else {
                time.brief
            };
            if reached && layout.width >= 60 {
                format!("{LIMIT_REACHED}, resets {when}")
            } else {
                format!("resets {when}")
            }
        })
    };
    let mut spans = vec![
        StyledSpan::plain(" ".repeat(INDENT)),
        StyledSpan::new(label, if quiet { look } else { Token::Text.into() }),
        before,
    ];
    spans.extend(bar);
    spans.extend([
        between,
        StyledSpan::new(format!("{:>1$}", measured(window), columns.percent), look),
    ]);
    if spans_width(&spans, layout.width_mode) + after.text.len() + width(&reset, layout.width_mode)
        < layout.width
    {
        spans.extend([after, StyledSpan::new(reset, look)]);
        return vec![StyledLine::new(spans)];
    }
    let mut out = vec![StyledLine::new(spans)];
    out.extend(lines(reset, INDENT, look, layout));
    out
}

fn usage_rows(
    account: &AccountView<'_>,
    columns: &Columns,
    now: i64,
    layout: Layout,
    time: &impl Fn(i64) -> ResetTime,
) -> Vec<StyledLine> {
    let state = account.state;
    match state.error {
        Some(PlanUsageError::Disabled) => return vec![],
        Some(PlanUsageError::AuthenticationRequired) => {
            return state_rows(
                crate::chrome::account::billing_word(AccountState::Expired),
                Some(&sign_in_again().text()),
                columns,
                layout,
            );
        }
        _ => {}
    }
    let mut out = vec![];
    if state.loading {
        out.extend(lines(
            format!("checking{}", Glyphs::of(layout.glyphs).more),
            INDENT,
            Token::TextMuted,
            layout,
        ));
    }
    if let Some(snapshot) = &state.snapshot {
        let mut windows: Vec<_> = snapshot.windows.iter().collect();
        windows.sort_by_key(|w| (w.duration_seconds, &w.key));
        for window in windows {
            out.extend(window_rows(window, account, columns, now, layout, time));
        }
        if snapshot.windows.is_empty() {
            out.extend(lines(
                "No allowance windows reported",
                INDENT,
                Token::TextMuted,
                layout,
            ));
        }
    }
    match state.error.filter(|_| !state.loading) {
        Some(
            PlanUsageError::Throttled { .. }
            | PlanUsageError::Network
            | PlanUsageError::InvalidResponse,
        ) if state.snapshot.is_some() => {}
        Some(PlanUsageError::Throttled { retry_at: Some(at) }) if at > now => {
            out.extend(state_rows(
                &format!("usage unavailable until {}", time(at).brief),
                None,
                columns,
                layout,
            ))
        }
        Some(error) if error.needs_user() => out.extend(state_rows(
            "usage unavailable",
            account
                .signs_in()
                .then(|| sign_in_again().text())
                .as_deref(),
            columns,
            layout,
        )),
        error if error.is_some() || out.is_empty() => out.extend(state_rows(
            if layout.width < 60 {
                "unavailable now"
            } else {
                "usage unavailable right now"
            },
            Some("r try again"),
            columns,
            layout,
        )),
        _ => {}
    }
    out
}

fn title_rows(account: &AccountView<'_>, layout: Layout) -> Vec<StyledLine> {
    let mode = layout.width_mode;
    let tier = (account.kind == AccountKind::ChatGpt)
        .then(|| account.state.snapshot.as_ref().and_then(|s| s.plan_tier))
        .flatten();
    let kind = tier.map_or_else(
        || account.kind.label().to_owned(),
        |tier| format!("{} {}", account.kind.provider_word(), tier.label()),
    );
    let glyphs = Glyphs::of(layout.glyphs);
    let name = marked(&one_line(account.alias), account.mark, &glyphs);
    let whole = format!("{kind} {} {name}", glyphs.separator);
    let chat = if account.active {
        3 + width(THIS_CHAT, mode)
    } else {
        0
    };
    if 2 + width(&whole, mode) + chat < layout.width {
        let mut spans = vec![StyledSpan::plain("  ")];
        spans.extend(mark_line(&whole, Token::Text, account.mark, &glyphs).spans);
        if account.active {
            spans.push(StyledSpan::plain("   "));
            spans.push(StyledSpan::new(THIS_CHAT, Token::TextMuted));
        }
        return vec![StyledLine::new(spans)];
    }
    let mut out = lines(kind, 2, Token::Text, layout);
    let named = if account.active {
        paired(&name, THIS_CHAT, 2, layout)
    } else {
        lines(name, 2, Token::Text, layout)
    };
    out.extend(
        named
            .into_iter()
            .map(|line| mark_spans(line, account.mark, &glyphs)),
    );
    out
}

fn amount_rows(
    indent: usize,
    label: &str,
    part: &Part,
    word: &str,
    columns: &Columns,
    layout: Layout,
) -> Vec<StyledLine> {
    let mode = layout.width_mode;
    let token = part.0.map_or(Token::TextMuted, billing_token);
    let lead = format!("{}{label}", " ".repeat(indent));
    let used = width(&lead, mode) + width(&part.1, mode);
    let aligned = columns.amounts.saturating_sub(used).max(2);
    let gap = layout
        .width
        .checked_sub(used + width(word, mode))
        .filter(|room| *room >= 2)
        .map_or(aligned, |room| aligned.min(room));
    let mut spans = vec![
        StyledSpan::new(lead, Token::Text),
        StyledSpan::plain(" ".repeat(gap)),
        StyledSpan::new(part.1.clone(), token),
    ];
    if spans_width(&spans, mode) + width(word, mode) <= layout.width {
        spans.push(StyledSpan::new(word, token));
        return vec![StyledLine::new(spans)];
    }
    let mut out = vec![StyledLine::new(spans)];
    out.extend(lines(
        word.trim_start_matches([' ', '·', '-']),
        columns.amounts.saturating_sub(width(&part.1, mode)),
        token,
        layout,
    ));
    out
}

fn mixed_note(layout: Layout) -> String {
    let sep = format!(" {} ", Glyphs::of(layout.glyphs).separator);
    format!("{sep}{}", copy::mixed_note_parts().join(&sep))
}

fn amount_note(part: &Part, layout: Layout) -> String {
    if part.0.is_none() {
        mixed_note(layout)
    } else {
        String::new()
    }
}

pub(super) fn account_rows(
    account: &AccountView<'_>,
    all: Option<&AllChats>,
    columns: &Columns,
    now: i64,
    layout: Layout,
    time: &impl Fn(i64) -> ResetTime,
) -> Vec<StyledLine> {
    let mut out = title_rows(account, layout);
    if let Some(reason) = account.unavailable {
        out.extend(state_rows(
            &copy::unavailable_because(reason),
            account
                .signs_in()
                .then(|| sign_in_again().text())
                .as_deref(),
            columns,
            layout,
        ));
    } else if !is_api(account.kind) {
        out.extend(usage_rows(account, columns, now, layout, time));
    } else if let Some(refused) = &account.refused {
        out.extend(lines(refused_line(refused), INDENT, Token::Error, layout));
    }
    for part in account.parts() {
        let word = amount_note(&part, layout);
        out.extend(amount_rows(
            INDENT,
            amount_label(&part),
            &part,
            &word,
            columns,
            layout,
        ));
    }
    if let Some(all) = all
        && let Some(amounts) = all.accounts.get(account.alias)
    {
        for (label, spend) in [
            (
                copy::account_chats_label(&copy::TODAY.to_lowercase()),
                amounts[0],
            ),
            (copy::account_chats_label(&all.label), amounts[1]),
        ] {
            let api = is_api(account.kind);
            for (index, part) in spend.parts(api, !api).iter().enumerate() {
                let word = amount_note(part, layout);
                out.extend(amount_rows(
                    INDENT,
                    if index == 0 { &label } else { "" },
                    part,
                    &word,
                    columns,
                    layout,
                ));
            }
        }
    }
    if account.focused {
        if let Some(line) = out.first_mut() {
            line.spans
                .splice(0..1, [selection_mark(layout), StyledSpan::plain(" ")]);
        }
        out = out
            .into_iter()
            .map(|line| selected_row(line.spans, layout))
            .collect();
    }
    out
}

const DAY_ROWS: usize = 7;

fn day_rows(view: &UsagePage<'_>, columns: &Columns, layout: Layout) -> Vec<StyledLine> {
    let Some(all) = view.all else {
        return vec![];
    };
    let (api, subscription) = view.kinds();
    let mut out = vec![];
    for (kind, show) in [
        (AccountState::Api, api),
        (AccountState::Subscription, subscription),
    ] {
        if !show {
            continue;
        }
        if !out.is_empty() {
            out.push(StyledLine::default());
        }
        let part = (Some(kind), String::new());
        out.extend(lines(
            format!(
                "{} {} {}",
                copy::ALL_CHATS,
                Glyphs::of(layout.glyphs).separator,
                amount_label(&part)
            ),
            2,
            Token::Text,
            layout,
        ));
        for (label, spend) in all.days.iter().take(DAY_ROWS) {
            if let Some(part) = spend
                .parts(api, subscription)
                .into_iter()
                .find(|p| p.0 == Some(kind) || (p.0.is_none() && kind == AccountState::Api))
            {
                let word = amount_note(&part, layout);
                out.extend(amount_rows(INDENT, label, &part, &word, columns, layout));
            }
        }
    }
    out
}

fn total_word(part: &Part, layout: Layout) -> String {
    match part.0 {
        Some(AccountState::Subscription) => format!(" {}", wisp_protocol::SUBSCRIPTIONS_WORD),
        Some(_) => " API".to_owned(),
        None => mixed_note(layout),
    }
}

fn total_rows(
    label: &str,
    parts: &[Part],
    second: usize,
    split: bool,
    columns: &Columns,
    layout: Layout,
) -> Vec<StyledLine> {
    if let [part] = parts
        && part.0.is_none()
    {
        return amount_rows(2, label, part, &mixed_note(layout), columns, layout);
    }
    let sep = format!(" {} ", Glyphs::of(layout.glyphs).separator);
    let mut out: Vec<StyledLine> = vec![];
    for (index, part) in parts.iter().enumerate() {
        let word = total_word(part, layout);
        let tail = format!("{}{}{word}", padded(&part.1, second, layout), part.1);
        if !split
            && let [line] = out.as_mut_slice()
            && spans_width(&line.spans, layout.width_mode)
                + width(&sep, layout.width_mode)
                + width(&tail, layout.width_mode)
                <= layout.width
        {
            line.spans
                .push(StyledSpan::new(sep.clone(), Token::TextFaint));
            line.spans.push(StyledSpan::new(
                tail,
                part.0.map_or(Token::TextMuted, billing_token),
            ));
            continue;
        }
        if split && parts.len() == 1 {
            out.extend(lines(label, 2, Token::Text, layout));
        }
        if split && (index > 0 || parts.len() == 1) {
            let tail = format!("{}{word}", part.1);
            if width(&tail, layout.width_mode) <= layout.width {
                out.push(StyledLine::new(vec![
                    StyledSpan::plain(" ".repeat(layout.width - width(&tail, layout.width_mode))),
                    StyledSpan::new(tail, part.0.map_or(Token::TextMuted, billing_token)),
                ]));
            } else {
                out.extend(amount_rows(2, "", part, &word, columns, layout));
            }
        } else {
            out.extend(amount_rows(
                2,
                if index == 0 { label } else { "" },
                part,
                &word,
                columns,
                layout,
            ));
        }
    }
    out
}

fn bottom_rows(view: &UsagePage<'_>, columns: &Columns, layout: Layout) -> Vec<StyledLine> {
    let (api, subscription) = view.kinds();
    let mut sums = vec![(
        copy::sentence_case(THIS_CHAT),
        view.total.parts(api, subscription),
    )];
    if let Some(all) = view.all {
        sums.extend([
            (
                copy::all_chats_label(&copy::TODAY.to_lowercase()),
                all.today.parts(api, subscription),
            ),
            (
                copy::all_chats_label(&all.label),
                all.window.parts(api, subscription),
            ),
        ]);
    }
    let second = sums
        .iter()
        .filter_map(|(_, parts)| parts.get(1))
        .map(|p| width(&p.1, layout.width_mode))
        .max()
        .unwrap_or(0);
    let split = view.all.is_some()
        && sums.iter().any(|(label, parts)| {
            let rows = total_rows(
                label,
                parts.get(..1).unwrap_or(parts),
                0,
                false,
                columns,
                layout,
            );
            let first = rows
                .first()
                .map_or(0, |row| spans_width(&row.spans, layout.width_mode));
            rows.len() > 1
                || first
                    + parts.get(1).map_or(0, |part| {
                        3 + second + width(&total_word(part, layout), layout.width_mode)
                    })
                    > layout.width
        });
    sums.into_iter()
        .flat_map(|(label, parts)| total_rows(&label, &parts, second, split, columns, layout))
        .collect()
}

fn keys_row(view: &UsagePage<'_>, layout: Layout) -> StyledLine {
    let mut keys = vec![];
    if view.accounts.iter().any(|a| a.focused && a.signs_in()) {
        keys.push(sign_in_again());
    }
    if view.accounts.iter().any(|a| a.focused && a.changes_key()) {
        keys.push(KeyHint::new(copy::ENTER_KEY, "change key"));
    }
    keys.push(KeyHint::new("r", "refresh"));
    keys.push(copy::esc(copy::CLOSE_WORD));
    key_row(&keys, "", None, KEY_ROW, " ", layout)
}

pub struct PageRows {
    pub rows: Vec<StyledLine>,
    pub scroll: usize,
    pub max_scroll: usize,
    pub focus: usize,
}

pub fn page_rows(
    view: &UsagePage<'_>,
    room: usize,
    layout: Layout,
    time: impl Fn(i64) -> ResetTime,
) -> PageRows {
    let mut out = PageRows {
        rows: vec![],
        scroll: 0,
        max_scroll: 0,
        focus: 0,
    };
    if room == 0 || layout.width == 0 {
        return out;
    }
    let glyphs = Glyphs::of(layout.glyphs);
    let rule = || {
        StyledLine::new(vec![StyledSpan::new(
            glyphs.rule.repeat(layout.width),
            Token::Line,
        )])
    };
    let columns = Columns::of(view, layout);
    let mut header = lines("Usage", 1, Token::Text, layout);
    header.push(rule());
    let mut footer = vec![rule()];
    footer.extend(bottom_rows(view, &columns, layout));
    footer.push(keys_row(view, layout));
    let mut body = vec![];
    for account in view.accounts {
        if !body.is_empty() {
            body.push(StyledLine::default());
        }
        if account.focused {
            out.focus = body.len();
        }
        body.extend(account_rows(
            account, view.all, &columns, view.now, layout, &time,
        ));
    }
    if view.accounts.is_empty() {
        body.extend(lines("No configured accounts", 2, Token::TextMuted, layout));
    }
    if view.all.is_some() {
        body.push(StyledLine::default());
        body.extend(day_rows(view, &columns, layout));
    }
    if header.len() + footer.len() > room {
        out.rows = lines("Usage", 1, Token::Text, layout);
        out.rows.extend(bottom_rows(view, &columns, layout));
        out.rows.truncate(room.saturating_sub(1));
        out.rows.push(keys_row(view, layout));
        return out;
    }
    let available = room - header.len() - footer.len();
    out.max_scroll = body.len().saturating_sub(available);
    out.scroll = view.scroll.min(out.max_scroll);
    header.extend(body.into_iter().skip(out.scroll).take(available));
    header.extend(footer);
    out.rows = header;
    out
}
