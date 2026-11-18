#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "render's and tui's library tests, render's three binaries, tui's app binary and cli's unit tests each use part of the shared helpers, and a failed load fails the test"
)]

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ratatui_core::style::Color;
use wisp_protocol::{AccountSpend, Activity, AgentState, Billing, CacheTtl};
use wisp_render::chrome::AgentRow;
use wisp_render::highlight::Grammars;
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::stream::Stream;
use wisp_render::styled::StyledLine;
use wisp_render::text::{WidthMode, sanitize, spans_text, width};
use wisp_render::theme::Theme;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Config, Transcript};

#[path = "../tests/support/rng.rs"]
mod rng;

#[allow(
    unused_imports,
    reason = "only some of the crates that include this module use the generator"
)]
pub(crate) use rng::Rng;

pub(crate) fn grammars() -> Arc<Grammars> {
    static G: OnceLock<Arc<Grammars>> = OnceLock::new();
    G.get_or_init(|| Arc::new(Grammars::load().unwrap()))
        .clone()
}

pub(crate) fn line_text(line: &StyledLine) -> String {
    spans_text(&line.spans)
}

pub(crate) fn width_of(s: &str) -> usize {
    width(s, WidthMode::default())
}

pub(crate) fn text(lines: &[StyledLine]) -> String {
    texts(lines).join("\n")
}

pub(crate) fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(line_text).collect()
}

pub(crate) fn trimmed(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|l| line_text(l).trim_end().to_owned())
        .collect()
}

pub(crate) fn colour(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
        Color::Indexed(i) => i.to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

pub(crate) fn resolved(rows: &[StyledLine], theme: Theme) -> String {
    let mut out = String::new();
    for row in rows {
        for span in theme.line(row).spans {
            let style = span.style;
            let mut tags = vec![];
            if let Some(fg) = style.fg {
                tags.push(colour(fg));
            }
            if let Some(bg) = style.bg {
                tags.push(format!("on {}", colour(bg)));
            }
            if !style.add_modifier.is_empty() {
                tags.push(format!("{:?}", style.add_modifier).to_lowercase());
            }
            match tags.is_empty() {
                true => out.push_str(&span.content),
                false => out.push_str(&format!("[{}|{}]", tags.join(" "), span.content)),
            }
        }
        out.push('\n');
    }
    out
}

pub(crate) fn spend(usd: f64, billing: Billing) -> AccountSpend {
    AccountSpend::of(usd, None, billing, false)
}

pub(crate) fn child(name: &str, spent: f64, billing: Billing, priced: bool) -> AgentRow {
    AgentRow {
        entry: serde_json::from_value(serde_json::json!({
            "path": format!("/{name}"), "name": name, "model": "model",
            "effort": "high", "depth": 1, "state": AgentState::Idle { outcome: None },
            "activity": Activity::Idle, "spent_usd": spent,
            "billing": billing, "priced": priced, "endpoint": "endpoint", "provider": "provider"
        }))
        .unwrap(),
        effort: Some(wisp_protocol::Effort::High),
        last_outcome: None,
        last_mail: None,
    }
}

pub(crate) fn transcript() -> Transcript {
    Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: true,
    })
}

pub(crate) fn bad(c: char) -> bool {
    (c.is_control() && c != '\n')
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{2028}' | '\u{2029}' | '\u{200B}' | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}' | '\u{FFF9}'..='\u{FFFB}')
}

pub(crate) fn r(width: usize) -> Renderer<'static> {
    Renderer::new(Layout::new(width, GlyphSet::Unicode))
}

pub(crate) fn layout(w: usize) -> Layout {
    Layout::new(w, GlyphSet::Unicode)
}

pub(crate) fn millis(n: u64) -> Duration {
    Duration::from_millis(n)
}

pub(crate) fn expected(doc: &str, r: &Renderer<'_>) -> Vec<StyledLine> {
    r.markdown(&sanitize(&doc.replace("\r\n", "\n")))
}

pub(crate) fn feed(chunks: &[&[u8]], r: &Renderer<'_>, want: &[StyledLine]) -> Result<(), String> {
    let mut s = Stream::new();
    let mut committed: Vec<StyledLine> = vec![];
    let check = |committed: &[StyledLine], at: usize| -> Result<(), String> {
        if committed.len() > want.len() {
            return Err(format!(
                "chunk {at}: {} rows committed, {} in the full render",
                committed.len(),
                want.len()
            ));
        }
        match committed.iter().zip(want).position(|(g, w)| g != w) {
            Some(row) => Err(format!(
                "chunk {at}: row {row} changed: {:?} != {:?}",
                texts(&committed[row..=row]),
                texts(&want[row..=row])
            )),
            None => Ok(()),
        }
    };
    for (i, c) in chunks.iter().enumerate() {
        s.push_bytes(c);
        committed.extend(s.frame(r).commit);
        check(&committed, i)?;
    }
    committed.extend(s.finish(r).commit);
    check(&committed, chunks.len())?;
    if committed != want {
        return Err(format!("end: {:?} != {:?}", texts(&committed), texts(want)));
    }
    let rows = s.rows(r);
    if rows != committed {
        return Err(format!(
            "rows after finish: {:?} != {:?}",
            texts(&rows),
            texts(&committed)
        ));
    }
    Ok(())
}
