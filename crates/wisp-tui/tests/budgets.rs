#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a measurement harness that fails loudly and prints what it measured"
)]

use std::time::{Duration, Instant};

use crate::events::{Finished, Requested, tool_finished, tool_requested};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use wisp_protocol::plugin::{Catalog, PluginSource};
use wisp_protocol::slot::{
    NodeState, SlotBlock, SlotContent, SlotEntry, SlotNode, SlotPlace, SlotSpan, SlotTone, SlotView,
};
use wisp_protocol::{AgentEvent, CacheTtl, PluginEntry};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::motion::{MotionCtx, MotionPolicy};
use wisp_render::styled::StyledLine;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Theme};
use wisp_render::transcript::{Config, Transcript, UserPart};
use wisp_tui::app::pager::{PageRows, RowCounts};
use wisp_tui::app::replay;
use wisp_tui::app::sidebar::{Frame, RULE, Side, band, transcript_width};
use wisp_tui::app::view::{Paint, SliceClock, View};
use wisp_tui::term::{Painter, SidebarSetting};

#[path = "support/scratch.rs"]
mod scratch;

#[path = "../../wisp-render/tests/support/events.rs"]
mod events;

#[path = "../../wisp-render/tests/support/code.rs"]
mod code;

const REPLAY: &str = "WISP_BUDGET_REPLAY";
const SCRATCH: &str = "wisp-tui-";

fn turns(path: &str) -> Vec<Vec<AgentEvent>> {
    let text = std::fs::read_to_string(path).unwrap();
    let events = replay::parse(&text).unwrap();
    let mut turns = vec![];
    let mut turn = vec![];
    for event in events {
        let ended = matches!(event, AgentEvent::TurnEnded { .. });
        turn.push(event);
        if ended {
            turns.push(std::mem::take(&mut turn));
        }
    }
    turns
}

struct Bench {
    transcript: Transcript,
    view: View,
    painter: Painter,
    theme: Theme,
    width: u16,
    height: u16,
    banner: Vec<StyledLine>,
    clock: Duration,
    user: Option<String>,
}

impl Bench {
    fn new(width: u16, height: u16) -> Bench {
        Bench {
            transcript: Transcript::new(Config {
                provider: "anthropic".to_owned(),
                cache_ttl: CacheTtl::OneHour,
                logging: false,
            }),
            view: View::new(usize::from(width)),
            painter: Painter::new(false),
            theme: Theme::new(Mode::Dark, Depth::TrueColor),
            width,
            height,
            banner: vec![StyledLine::default(); 3],
            clock: Duration::ZERO,
            user: None,
        }
    }

    fn layout(&self) -> Layout {
        Layout::new(usize::from(self.width), GlyphSet::Unicode)
    }

    fn apply(&mut self, event: &AgentEvent) {
        self.clock += Duration::from_millis(1);
        self.transcript.apply(event, self.clock);
        if matches!(event, AgentEvent::TurnStarted { .. })
            && let Some(text) = self.user.take()
        {
            self.transcript.deliver(vec![UserPart::Text(text)], None);
        }
    }

    fn frame(&mut self) -> Duration {
        let started = Instant::now();
        self.clock += Duration::from_millis(33);
        let layout = self.layout();
        let _ = self.transcript.frame_at(&Renderer::new(layout), self.clock);
        self.view.sync(self.transcript.cells().len());
        let rows = self.height.saturating_sub(4);
        let mut paint = Paint {
            transcript: &mut self.transcript,
            layout,
            banner: &self.banner,
            banner_live: false,
            caret: None,
        };
        let shown = self.view.layout(usize::from(rows), &mut paint);
        let mut buffer = Buffer::empty(Rect::new(0, 0, self.width, self.height));
        for (y, line) in (0..rows).zip(shown.rows.iter()) {
            buffer.set_line(0, y, &self.theme.line(line), self.width);
        }
        if let Some(plan) = self.painter.plan(&buffer, None).unwrap() {
            self.painter.sent(plan, Duration::ZERO);
        }
        started.elapsed()
    }

    fn slice(&mut self) -> (bool, Duration) {
        let started = Instant::now();
        let layout = self.layout();
        let mut paint = Paint {
            transcript: &mut self.transcript,
            layout,
            banner: &self.banner,
            banner_live: false,
            caret: None,
        };
        let mut clock = SliceClock::new(Instant::now());
        let more = self
            .view
            .slice(&mut paint, || clock.out_of_time(Instant::now()));
        (more, started.elapsed())
    }

    fn resize(&mut self, width: u16) {
        self.width = width;
        self.view.set_width(usize::from(width));
        self.painter.invalidate();
    }

    fn play(&mut self, turns: &[Vec<AgentEvent>]) {
        for (n, turn) in turns.iter().enumerate() {
            self.user = Some(format!("question {n}"));
            for event in turn {
                self.apply(event);
            }
            self.frame();
        }
    }
}

fn percentile(samples: &mut [Duration], p: usize) -> Duration {
    samples.sort();
    let at = (samples.len() * p / 100).min(samples.len().saturating_sub(1));
    samples.get(at).copied().unwrap_or_default()
}

fn rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

#[test]
#[ignore = "a release-build measurement: WISP_BUDGET_REPLAY=FILE heavy cargo test --release --test budgets -- --ignored --nocapture"]
fn streaming_frame_at_120_by_40() {
    let Ok(path) = std::env::var(REPLAY) else {
        return;
    };
    streaming_p95(&path);
}

fn streaming_p95(path: &str) -> Duration {
    let turns = turns(path);
    let (last, done) = turns.split_last().unwrap();
    let mut bench = Bench::new(120, 40);
    bench.play(done);
    bench.user = Some("the last question".to_owned());
    let mut frames = vec![];
    for event in last {
        if let AgentEvent::TextDelta { index, text } = event {
            let mut cut = 0;
            while cut < text.len() {
                let mut end = (cut + 40).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                bench.apply(&AgentEvent::TextDelta {
                    index: *index,
                    text: text[cut..end].to_owned(),
                });
                frames.push(bench.frame());
                cut = end;
            }
        } else {
            bench.apply(event);
            frames.push(bench.frame());
        }
    }
    let worst = frames.iter().max().copied().unwrap_or_default();
    let p95 = percentile(&mut frames, 95);
    eprintln!(
        "BUDGET streaming frame 120x40 on {path}: {} frames, p95 {p95:?}, max {worst:?}, rss {} MB",
        frames.len(),
        rss_kb() / 1024
    );
    p95
}

fn resumed_fences(turns: usize, fences: usize, lines: usize) -> Vec<AgentEvent> {
    let body: String = (0..lines)
        .map(|i| format!("    let value_{i} = compute({i}, &settings)?;\n"))
        .collect();
    let answer: String = (0..fences)
        .map(|f| format!("Step {f}:\n\n```rust\nfn step_{f}() -> Result<()> {{\n{body}}}\n```\n\n"))
        .collect();
    let mut out = String::new();
    for turn in 1..=turns {
        let lines = [
            serde_json::json!({"type": "turn_started", "turn": turn}),
            serde_json::json!({"type": "step_started", "step": 1}),
            serde_json::json!({"type": "block_started", "index": 0, "kind": {"type": "text"}}),
            serde_json::json!({"type": "text_delta", "index": 0, "text": answer}),
            serde_json::json!({"type": "block_stopped", "index": 0}),
        ];
        for l in lines {
            out.push_str(&format!("{l}\n"));
        }
        out.push_str(&format!(
            "{}\n",
            serde_json::json!({"type": "turn_ended", "outcome": {"status": "completed", "stop_reason": "end_turn", "model": "m", "usage": {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}, "cost": null, "committed": true, "steps": 1, "tool_calls": 0}, "stats": {"started_at": "2026-10-27T09:00:00Z", "first_byte_ms": 700, "duration_ms": 2100, "retries": 0, "previous_prompt_tokens": null}})
        ));
    }
    replay::parse(&out).unwrap()
}

const LATE_SLICE_BUDGET: Duration = Duration::from_millis(4);

#[derive(Clone, Copy, Debug)]
enum Ready {
    Before,
    During,
    After,
}

fn resume_budget(turns: usize, fences: usize, lines: usize, ready: Ready) {
    use wisp_render::highlight::{Grammars, Highlighter};
    use wisp_render::markdown::CodeStyle;
    let mut code = code::Code::new(std::sync::Arc::new(Grammars::load().unwrap()));
    let mut bench = Bench::new(120, 40);
    bench.transcript = Transcript::new(Config {
        provider: "anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    })
    .with_code(Highlighter::pending().key());
    let events = resumed_fences(turns, fences, lines);
    let half = events.len() / 2;
    let started = Instant::now();
    for (n, event) in events.iter().enumerate() {
        let ready_now = match ready {
            Ready::Before => n == 0,
            Ready::During => n == half,
            Ready::After => false,
        };
        if ready_now {
            bench.transcript.wait_for_code(true);
        }
        bench.transcript.replay(event, Duration::ZERO);
    }
    let first = bench.frame();
    let resumed = started.elapsed();
    if matches!(ready, Ready::After) {
        bench.transcript.wait_for_code(true);
    }
    let mut pending = vec![];
    let mut slices = vec![];
    let mut fed = 0usize;
    for _ in 0..100_000 {
        let slice = Instant::now();
        for (stream, update) in pending.drain(..) {
            bench.transcript.apply_code(stream, update);
        }
        let feeds = bench.transcript.take_code();
        bench.frame();
        slices.push(slice.elapsed());
        if feeds.is_empty() && bench.transcript.code_streams().is_empty() {
            break;
        }
        for (stream, feed) in feeds {
            fed += feed.deltas.len();
            for update in code.feed(stream, &feed) {
                pending.push((stream, update));
            }
        }
        code.keep_live(&bench.transcript);
    }
    assert!(bench.transcript.code_streams().is_empty());
    let layout = bench.layout();
    let mut counts = RowCounts::default();
    let pages = [(); 2].map(|()| {
        let started = Instant::now();
        let _ = counts.count(&mut bench.transcript, layout);
        let total = counts.total();
        let mut page = PageRows::new(&mut bench.transcript, &counts, &[], layout, None);
        let _ = page.range(total.saturating_sub(200)..total);
        started.elapsed()
    });
    let over = slices.iter().filter(|s| **s > LATE_SLICE_BUDGET).count();
    let worst = slices.iter().max().copied().unwrap_or_default();
    let total: Duration = slices.iter().sum();
    let p95 = percentile(&mut slices, 95);
    eprintln!(
        "BUDGET resume {turns}x{fences} fences of {lines} lines, grammars {ready:?}, 120x40: replay and first frame {resumed:?} (frame {first:?}); {fed} fences fed in {} slices, p95 {p95:?}, max {worst:?}, slices total {total:?}, {over} over {LATE_SLICE_BUDGET:?}; ctrl-o page {:?} then {:?}; rss {} MB",
        slices.len(),
        pages[0],
        pages[1],
        rss_kb() / 1024
    );
    assert_eq!(fed, turns * fences);
    assert!(
        pages[1] <= FRAME_BUDGET,
        "ctrl-o reopened in {:?}",
        pages[1]
    );
    if turns <= 200 {
        assert_eq!(over, 0, "{over} slices ran over {LATE_SLICE_BUDGET:?}");
        assert!(
            pages[0] <= FULL_LAYOUT_BUDGET,
            "ctrl-o counted in {pages:?}"
        );
    }
}

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release --test budgets -- --ignored --nocapture"]
fn a_resumed_transcripts_fences_highlight_in_slices_whenever_the_grammars_arrive() {
    for ready in [Ready::Before, Ready::During, Ready::After] {
        resume_budget(20, 10, 20, ready);
        resume_budget(200, 3, 40, ready);
    }
    resume_budget(660, 3, 40, Ready::Before);
}

const CAP_LINES: usize = 26_000;

fn cap_sized_show_diff() -> Vec<AgentEvent> {
    let line = "    let value = compute(input, &settings)?;";
    let mut hunks = format!("@@ -1,{CAP_LINES} +1,{CAP_LINES} @@\n");
    for i in 0..CAP_LINES {
        let sign = match i % 4 {
            0 => '-',
            1 => '+',
            _ => ' ',
        };
        hunks.push_str(&format!("{sign}{line} {i}\n"));
    }
    let added = (CAP_LINES / 4) as u64;
    let fence =
        "```diff\n@@ -1,3 +1,3 @@\n fn key() {\n-    hash(a)\n+    fingerprint(a)\n }\n```\n\n";
    let text = format!("Here it is.\n\n{}", fence.repeat(10));
    let display = serde_json::json!({
        "action": {"type": "show"},
        "target": "big.rs",
        "output": {"type": "none"},
        "view": {"type": "diff", "files": [{"path": "big.rs", "hunks": hunks, "added": added, "removed": added}]},
        "purpose": "show"
    });
    let lines = [
        serde_json::json!({"type": "turn_started", "turn": 1}),
        serde_json::json!({"type": "step_started", "step": 1}),
        serde_json::json!({"type": "block_started", "index": 0, "kind": {"type": "text"}}),
        serde_json::json!({"type": "text_delta", "index": 0, "text": text}),
        serde_json::json!({"type": "block_stopped", "index": 0}),
        serde_json::json!({"type": "tool_call_requested", "step": 1, "block": 1, "call_id": "d1", "tool": "show_diff", "input": {}, "display": display}),
        serde_json::json!({"type": "tool_call_started", "call_id": "d1", "cwd": "~"}),
        serde_json::json!({"type": "tool_call_finished", "call_id": "d1", "outcome": {"status": "completed", "is_error": false, "exit": null}, "output": {"tail": "", "bytes": 0, "lines": 0, "file": null, "file_problem": null, "inlined": true}, "facts": [{"type": "diffstat", "added": added, "removed": added}], "state": null, "duration_ms": 9}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    replay::parse(&text).unwrap()
}

const FRAME_BUDGET: Duration = Duration::from_millis(8);
const SLICE_BUDGET: Duration = Duration::from_millis(4);
const APPLY_BUDGET: Duration = Duration::from_millis(15);
const FULL_LAYOUT_BUDGET: Duration = Duration::from_millis(250);

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release --test budgets -- --ignored --nocapture"]
fn a_cap_sized_show_diff_at_120_by_40() {
    let events = cap_sized_show_diff();
    let mut bench = Bench::new(120, 40);
    let mut frames = vec![];
    let mut applied = Duration::ZERO;
    for event in &events {
        let started = Instant::now();
        bench.apply(event);
        let took = started.elapsed();
        if matches!(event, AgentEvent::ToolCallRequested { .. }) {
            applied = took;
        }
        frames.push(bench.frame());
    }
    for _ in 0..200 {
        frames.push(bench.frame());
    }
    let (mut slices, mut over) = (0usize, 0usize);
    loop {
        let (more, took) = bench.slice();
        slices += 1;
        if took > SLICE_BUDGET {
            over += 1;
        }
        if !more {
            break;
        }
    }
    let opened = Instant::now();
    let full = bench.transcript.rows(
        &Renderer::new(bench.layout()),
        wisp_render::transcript::Detail::Full,
    );
    let opened = opened.elapsed();
    let worst = frames.iter().max().copied().unwrap_or_default();
    let p95 = percentile(&mut frames, 95);
    eprintln!(
        "BUDGET cap-sized show_diff 120x40: apply {applied:?}; {} frames, p95 {p95:?}, max {worst:?}; {slices} slices, {over} over {SLICE_BUDGET:?}; ctrl-o layout {opened:?} for {} rows; rss {} MB",
        frames.len(),
        full.len(),
        rss_kb() / 1024
    );
    assert!(full.len() > CAP_LINES);
    assert!(applied <= APPLY_BUDGET, "apply took {applied:?}");
    assert!(worst <= FRAME_BUDGET, "the slowest frame took {worst:?}");
    assert_eq!(
        over, 0,
        "{over} of {slices} slices ran over {SLICE_BUDGET:?}"
    );
    assert!(
        opened <= FULL_LAYOUT_BUDGET,
        "ctrl-o laid out in {opened:?}"
    );
}

#[test]
#[ignore = "a release-build measurement: WISP_BUDGET_REPLAY=FILE heavy cargo test --release --test budgets -- --ignored --nocapture"]
fn a_ten_second_drag() {
    let Ok(path) = std::env::var(REPLAY) else {
        return;
    };
    let turns = turns(&path);
    let mut bench = Bench::new(120, 40);
    bench.play(&turns);
    let played = rss_kb();
    let mut first = vec![];
    let mut keys = vec![];
    let mut slices = vec![];
    let widths = [100u16, 80, 60, 70, 90, 110, 120, 105, 85, 65];
    for (step, width) in widths.into_iter().cycle().take(40).enumerate() {
        bench.resize(width);
        first.push(bench.frame());
        let window = Instant::now();
        let mut sliced = true;
        let mut key = 0;
        while window.elapsed() < Duration::from_millis(250) {
            if sliced {
                let (more, took) = bench.slice();
                slices.push(took);
                sliced = more;
            }
            if key < 5 {
                let wait = slices
                    .last()
                    .copied()
                    .filter(|_| sliced)
                    .unwrap_or_default();
                keys.push(wait + bench.frame());
                key += 1;
            }
            if !sliced && key >= 5 {
                break;
            }
        }
        if step == 0 {
            eprintln!("BUDGET first sweep done in the first window: {}", !sliced);
        }
    }
    let first_max = first.iter().max().copied().unwrap_or_default();
    let first_p95 = percentile(&mut first, 95);
    let key_p95 = percentile(&mut keys, 95);
    let key_max = keys.iter().max().copied().unwrap_or_default();
    let slice_max = slices.iter().max().copied().unwrap_or_default();
    let over = slices
        .iter()
        .filter(|s| **s > Duration::from_millis(4))
        .count();
    eprintln!("BUDGET slices {} over 4 ms: {over}", slices.len());
    eprintln!(
        "BUDGET drag on {path}: first frame at a new width p95 {first_p95:?} max {first_max:?}; key-to-frame p95 {key_p95:?} max {key_max:?}; slice max {slice_max:?}; rss after replay {} MB, after drag {} MB",
        played / 1024,
        rss_kb() / 1024
    );
}

fn tool_transcript(
    dir: &scratch::ScratchDir,
    files: usize,
    file_bytes: usize,
    line: &str,
) -> Transcript {
    use wisp_protocol::{ExitStatus, ToolOutcome, ToolOutputView};
    let mut t = Transcript::new(Config {
        provider: "anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    for i in 0..files {
        let path = dir.root.join(format!("{i}.out"));
        let body = line.repeat(file_bytes / line.len() + 1);
        std::fs::write(&path, &body).unwrap();
        let id = format!("c{i}");
        t.apply(
            &tool_requested(Requested {
                block: i,
                call_id: id.clone(),
                input: serde_json::json!({"command": "cargo test"}),
                target: "cargo test".to_owned(),
                ..Requested::default()
            }),
            Duration::ZERO,
        );
        t.apply(
            &tool_finished(Finished {
                call_id: id,
                outcome: ToolOutcome::Completed {
                    is_error: false,
                    exit: Some(ExitStatus::Code { code: 0 }),
                },
                output: ToolOutputView {
                    tail: line.repeat(20),
                    bytes: body.len() as u64,
                    lines: (body.len() / line.len()) as u64,
                    file: Some(path.to_string_lossy().into_owned()),
                    file_problem: None,
                    inlined: false,
                },
                duration_ms: 1,
                ..Finished::default()
            }),
            Duration::ZERO,
        );
    }
    t
}

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release --test budgets -- --ignored --nocapture transcript_mode"]
fn transcript_mode_with_a_full_file_budget() {
    use wisp_render::transcript::FILE_TAIL;
    use wisp_tui::app::pager::{PageRows, RowCounts};
    use wisp_tui::app::tool_files::{Loader, PAGER_BUDGET};
    let kinds = [
        (
            "plain",
            "test cache::fingerprint::names_the_segment_that_changed ... ok (0.01s) 12345\n",
        ),
        (
            "coloured",
            "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m wisp-core v0.1.0 (/home/user/Projects/Wisp/crates)\n",
        ),
        (
            "unicode",
            "r\u{e9}sum\u{e9} \u{2713} caf\u{e9} \u{4f60}\u{597d} \u{2014} wide and accented text for the pager\n",
        ),
    ];
    let long_ascii = format!(
        "{}\n",
        "error[E0308]: mismatched types in a long wrapped line ".repeat(8)
    );
    let long_cjk = format!(
        "{}\n",
        "\u{4f60}\u{597d}\u{4e16}\u{754c}\u{3001}".repeat(60)
    );
    let kinds = [
        kinds[0],
        kinds[1],
        kinds[2],
        ("long ascii", long_ascii.as_str()),
        ("long cjk", long_cjk.as_str()),
    ];
    let limit = |name: &str| {
        if name.starts_with("long") {
            Duration::from_millis(250)
        } else {
            Duration::from_millis(100)
        }
    };
    let mut slow = vec![];
    for (name, line) in kinds {
        let dir = scratch::ScratchDir::new(SCRATCH, "pager");
        let files = usize::try_from(PAGER_BUDGET / FILE_TAIL).unwrap() + 2;
        let mut t = tool_transcript(&dir, files, usize::try_from(FILE_TAIL).unwrap() * 2, line);
        let mut loader = Loader::new(
            PAGER_BUDGET,
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );
        while {
            loader.request(&mut t);
            loader.receive(&mut t)
        } {}
        let loaded = t.tool_file_bytes();
        let mut counts = RowCounts::default();
        let mut open = |width: usize| {
            let started = Instant::now();
            let layout = Layout::new(width, GlyphSet::Unicode);
            let _ = counts.count(&mut t, layout);
            let total = counts.total();
            let mut page = PageRows::new(&mut t, &counts, &[], layout, None);
            let _ = page.range(total.saturating_sub(200)..total);
            (started.elapsed(), total)
        };
        let (first, rows) = open(100);
        let (resize, _) = open(120);
        let (refresh, _) = open(120);
        eprintln!(
            "transcript mode, {name}: {loaded} bytes, {rows} rows; first {first:?}, resize {resize:?}, refresh {refresh:?}"
        );
        assert!(loaded <= PAGER_BUDGET);
        slow.extend(
            [(name, "first", first), (name, "resize", resize)]
                .into_iter()
                .filter(|(name, _, d)| *d >= limit(name)),
        );
    }
    assert!(slow.is_empty(), "{slow:?}");
}

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release --test budgets -- --ignored --nocapture breath"]
fn a_breath_frame_at_120_by_40() {
    use ratatui_core::buffer::Cell;
    use ratatui_core::layout::{Position, Size};
    use wisp_render::chrome::Status;
    use wisp_render::motion::{MotionCtx, MotionPolicy, Pacing, idle, idle_next_change};
    use wisp_render::styled::Look;
    use wisp_render::theme::Token;
    let (width, height) = (120u16, 40u16);
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor);
    let pacing = Pacing::new(cx);
    let status = Status::new();
    let mut view = Buffer::empty(Rect::new(0, 0, width, height));
    for y in 0..height {
        let text = format!("{y:>3} the quick brown fox jumps over the lazy dog ").repeat(4);
        let style = theme.style(&Look::fg(Token::Text));
        view.set_stringn(0, y, &text, usize::from(width), style);
    }
    let at = Position {
        x: 2,
        y: height - 1,
    };
    let size = Size { width, height };
    let mut painter = Painter::new(false);
    let plan = painter.plan(&view, None).unwrap().unwrap();
    painter.sent(plan, Duration::ZERO);
    let mut patched = vec![];
    let mut written = 0usize;
    let mut t = Duration::from_secs(4);
    for _ in 0..20_000 {
        let started = Instant::now();
        let local = status.resting(t, pacing.ctx()).unwrap();
        let frame = idle(local, cx);
        let core = frame.cells[1];
        let mut cell = Cell::default();
        cell.set_char(core.ch);
        cell.set_style(theme.style(&Look::fg(core.ink)));
        let next = idle_next_change(local, cx).unwrap();
        if let Some(plan) = painter.patch(size, at, cell).unwrap().unwrap() {
            written = written.max(plan.bytes().len());
            painter.sent(plan, Duration::ZERO);
        }
        patched.push(started.elapsed());
        t = if next > Duration::from_secs(500) {
            Duration::from_secs(4)
        } else {
            next
        };
    }
    let mut full = vec![];
    let mut t = Duration::from_secs(4);
    for _ in 0..2_000 {
        let started = Instant::now();
        let frame = idle(t, cx);
        let mut next_view = view.clone();
        let core = frame.cells[1];
        next_view[at].set_char(core.ch);
        next_view[at].set_style(theme.style(&Look::fg(core.ink)));
        if let Some(plan) = painter.plan(&next_view, None).unwrap() {
            painter.sent(plan, Duration::ZERO);
        }
        full.push(started.elapsed());
        t += Duration::from_millis(250);
    }
    let worst = patched.iter().max().copied().unwrap_or_default();
    let p50 = percentile(&mut patched, 50);
    let p99 = percentile(&mut patched, 99);
    let full_p50 = percentile(&mut full, 50);
    eprintln!(
        "BUDGET breath frame 120x40: one-cell patch p50 {p50:?}, p99 {p99:?}, max {worst:?}, largest body {written} bytes; a full diff of the same change p50 {full_p50:?}"
    );
    assert!(p99 < Duration::from_micros(300), "{p99:?}");
}

fn lit_box() -> AgentEvent {
    use wisp_protocol::slot::{SlotFlow, SlotNoun, SlotTopologyNode};
    let node = |key: &str, state| SlotTopologyNode {
        key: key.to_owned(),
        parent: key.rsplit_once('/').map(|(parent, _)| parent.to_owned()),
        state,
    };
    let (w, d, o) = (NodeState::Working, NodeState::Done, NodeState::Waiting);
    let nodes = [
        ("task-a", w),
        ("task-a/coder", w),
        ("task-a/coder/helper", d),
        ("task-a/helper", d),
        ("task-a/planner", w),
        ("task-a/review", o),
        ("task-b", d),
        ("task-c", o),
    ];
    let flow = |from: &str, to: Option<&str>, at_ms| SlotFlow {
        from: Some(from.to_owned()),
        to: to.map(str::to_owned),
        at_ms,
    };
    let topology = SlotBlock::Topology {
        root: Some("main".to_owned()),
        nodes: nodes.iter().map(|&(key, state)| node(key, state)).collect(),
        flows: vec![
            flow("task-a/review", Some("task-a"), 1_000),
            flow("task-c", None, 1_100),
            flow("task-a/coder/helper", Some("task-a/coder"), 1_300),
        ],
        counted: Some(SlotNoun::Agents),
    };
    AgentEvent::SlotContent {
        section: "agent-viz/topology".to_owned(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![],
            blocks: vec![topology],
        },
    }
}

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release --test budgets -- --ignored --nocapture braille_light"]
fn the_largest_braille_light_frame_at_120_by_40() {
    let (width, height) = (120u16, 40u16);
    let edge = u16::try_from(transcript_width(usize::from(width), true)).unwrap();
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor);
    let mut side = Side::new(SidebarSetting::Auto, usize::from(width));
    side.event(&panel_catalog());
    side.event(&lit_box());
    side.settle(usize::from(width), &side.on(&|_| None));
    assert!(side.docked());
    let on = side.on(&|_| None);
    let mut buffer = |ms: u64| {
        let panel = side.draw(
            &on,
            Frame {
                height: usize::from(height),
                at: Duration::from_millis(ms),
                now_ms: ms,
                cx,
                layout: Layout::new(band(usize::from(width)) + RULE, GlyphSet::Unicode),
                whole: false,
                pictures: false,
            },
        );
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        for (y, line) in (0..height).zip(panel.lines.iter()) {
            buffer.set_line(edge, y, &theme.line(line), width - edge);
        }
        buffer
    };
    let (mut largest, mut at, mut times) = (0, 0, vec![]);
    for ms in 1_000..2_400 {
        let mut painter = Painter::new(false);
        let plan = painter.plan(&buffer(ms), None).unwrap().unwrap();
        painter.sent(plan, Duration::ZERO);
        let started = Instant::now();
        let next = buffer(ms + 33);
        let bytes = painter
            .plan(&next, None)
            .unwrap()
            .map_or(0, |p| p.bytes().len());
        times.push(started.elapsed());
        if bytes > largest {
            (largest, at) = (bytes, ms);
        }
    }
    let p99 = percentile(&mut times, 99);
    eprintln!(
        "BUDGET braille light frame 120x40: largest {largest} bytes, the frame from {at} ms to {} ms; drawing and diffing a frame p99 {p99:?}",
        at + 33
    );
    assert!(p99 <= FRAME_BUDGET, "{p99:?}");
}

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release -p wisp-tui --test budgets the_agents_page_at_200_by_60_with_30_agents -- --ignored --nocapture"]
fn the_agents_page_at_200_by_60_with_30_agents() {
    use ratatui_core::layout::Position;
    use std::cell::Cell;
    use wisp_protocol::slot::{SlotFlow, SlotTopologyNode};
    use wisp_render::chrome::topology::{self, Topology};
    use wisp_render::picture;
    use wisp_tui::term::picture::{Encoder, Tile, Tiles};

    let node = |key: String| SlotTopologyNode {
        parent: key.rsplit_once('/').map(|(parent, _)| parent.to_owned()),
        key,
        state: NodeState::Working,
    };
    let mut nodes: Vec<_> = ["helper", "merge"].map(|key| node(key.to_owned())).into();
    for coordinator in 0..4 {
        let key = format!("task-{coordinator}");
        nodes.push(node(key.clone()));
        nodes.extend((0..6).map(|worker| node(format!("{key}/worker-{worker}"))));
    }
    let flows: Vec<_> = [
        ("task-0/worker-0", Some("task-0")),
        ("task-1/worker-1", None),
        ("task-2/worker-2", Some("task-3/worker-3")),
    ]
    .into_iter()
    .map(|(from, to)| SlotFlow {
        from: Some(from.to_owned()),
        to: to.map(str::to_owned),
        at_ms: 5_000,
    })
    .collect();
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let cell = (10, 20);
    let mut painter = Painter::new(false);
    let mut frame = |ms, flows: &[SlotFlow]| {
        let started = Instant::now();
        let drawing = topology::page(
            &Topology {
                root: Some("main"),
                nodes: &nodes,
                flows,
                picked: None,
                label: None,
                layout: Layout::new(200, GlyphSet::Unicode),
                at: Duration::from_millis(ms),
                now_ms: ms,
                cx: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
                pictures: true,
            },
            60,
        );
        let scene = &drawing.scenes[0];
        let rects = picture::tiles(scene, cell);
        assert!(!rects.is_empty(), "frame {ms} planned no tiles");
        let list = rects
            .iter()
            .map(|&(r, hash)| Tile {
                at: Position {
                    x: r.x.try_into().unwrap(),
                    y: r.y.try_into().unwrap(),
                },
                cells: r.cells.try_into().unwrap(),
                hash,
            })
            .collect();
        let sent = Cell::new(0);
        let pixels = |i: usize| {
            sent.set(sent.get() + 1);
            Some(picture::raster(scene, &theme, cell, rects[i].0, None))
        };
        let tiles = Tiles {
            list,
            cell,
            encoder: Encoder::Kitty,
            pixels: &pixels,
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 200, 60));
        for (y, line) in (0..60).zip(&drawing.lines) {
            buffer.set_line(0, y, &theme.line(line), 200);
        }
        if let Some(plan) = painter.plan_tiles(&buffer, None, &tiles).unwrap() {
            painter.sent(plan, Duration::ZERO);
        }
        (started.elapsed(), sent.get())
    };
    let (opening, opened) = frame(2_000, &[]);
    let mut breathing: Vec<_> = (1..=30)
        .map(|i| {
            let (took, sent) = frame(2_000 + i * 100, &[]);
            assert!(
                sent < opened,
                "breathing frame {i} re-sent {sent} of {opened} opening tiles"
            );
            took
        })
        .collect();
    let mut mail: Vec<_> = (1..=60).map(|i| frame(5_000 + i * 33, &flows).0).collect();
    eprintln!(
        "BUDGET agents page 200x60, 30 agents, kitty 10x20: open {opening:?} ({opened} tiles); breathing median {:?}, p95 {:?}; mail median {:?}, p95 {:?}",
        percentile(&mut breathing, 50),
        percentile(&mut breathing, 95),
        percentile(&mut mail, 50),
        percentile(&mut mail, 95),
    );
}

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release --test budgets -- --ignored --nocapture following_page"]
fn a_following_page_reload_at_the_file_tail() {
    use wisp_render::transcript::FILE_TAIL;
    use wisp_tui::app::pager::render_page;
    use wisp_tui::app::tool_files::{Loader, PAGER_BUDGET};
    let line = "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m wisp-core v0.1.0 (/home/user/Projects/Wisp/crates)\n";
    let dir = scratch::ScratchDir::new(SCRATCH, "follow");
    let mut t = tool_transcript(&dir, 1, usize::try_from(FILE_TAIL).unwrap() * 2, line);
    let mut loader = Loader::new(
        PAGER_BUDGET,
        std::sync::Arc::new(tokio::sync::Notify::new()),
    );
    let mut samples = vec![];
    for _ in 0..5 {
        let read = Instant::now();
        assert!(loader.reload_cell(&mut t, 0));
        assert!(loader.receive(&mut t));
        let read = read.elapsed();
        let wrap = Instant::now();
        let page = render_page(&mut t, 0, Layout::new(100, GlyphSet::Unicode)).unwrap();
        samples.push(wrap.elapsed());
        eprintln!(
            "following page: {} bytes, {} rows; read {read:?}, rewrap {:?}",
            t.tool_file_bytes(),
            page.body.len(),
            samples.last().unwrap()
        );
    }
    let p95 = percentile(&mut samples, 95);
    assert!(p95 < Duration::from_millis(100), "{p95:?}");
}

fn panel_tree(step: u64) -> AgentEvent {
    let node = |depth: u8, state: NodeState, label: &str| SlotNode {
        depth,
        state,
        role: Some("coder".into()),
        label: label.to_owned(),
        facts: vec![
            SlotSpan::Elapsed {
                since_ms: 0,
                tone: SlotTone::Muted,
            },
            SlotSpan::Usd {
                micros: step * 1_000,
                at_least: false,
                tone: SlotTone::Muted,
            },
        ],
    };
    let mut nodes = vec![node(0, NodeState::Working, "main")];
    for i in 0..12 {
        let state = if i % 3 == 0 {
            NodeState::Working
        } else {
            NodeState::Done
        };
        nodes.push(node(1 + (i % 3), state, &format!("agent {i}")));
    }
    AgentEvent::SlotContent {
        section: "agent-viz/topology".to_owned(),
        view: SlotView::Panel,
        content: SlotContent {
            note: vec![],
            blocks: vec![SlotBlock::Tree { nodes }],
        },
    }
}

fn panel_catalog() -> AgentEvent {
    AgentEvent::CatalogChanged {
        catalog: Catalog {
            plugins: vec![PluginEntry {
                prompt_sections: Vec::new(),
                id: "agent-viz".to_owned(),
                version: "0.1.0".to_owned(),
                summary: String::new(),
                source: PluginSource::Bundled,
                installed: true,
                enabled: true,
                tools: vec![],
                commands: vec![],
                hooks: vec![],
                slots: vec![SlotEntry {
                    name: "agent-viz/topology".to_owned(),
                    slot: SlotPlace::Sidebar,
                    title: "Agents".to_owned(),
                    default_on: true,
                    order: 0,
                    disabled: None,
                }],
                settings: vec![],
                problems: vec![],
            }],
        },
    }
}

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release --test budgets -- --ignored --nocapture"]
fn a_pulsing_docked_panel_beside_streaming_at_120_by_40() {
    let (width, height) = (120u16, 40u16);
    let rule = transcript_width(usize::from(width), true);
    let band = band(usize::from(width));
    let mut bench = Bench::new(u16::try_from(rule).unwrap(), height);
    let mut side = Side::new(SidebarSetting::Auto, usize::from(width));
    side.event(&panel_catalog());
    side.event(&panel_tree(0));
    side.settle(usize::from(width), &side.on(&|_| None));
    assert!(side.docked());
    bench.user = Some("stream beside the panel".to_owned());
    bench.apply(&AgentEvent::turn_started(1));
    bench.apply(&AgentEvent::BlockStarted {
        index: 0,
        kind: wisp_protocol::BlockKind::Text,
    });
    let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor);
    let mut frames = vec![];
    let mut panels = vec![];
    for n in 0..2_000u64 {
        let started = Instant::now();
        if n % 8 == 0 {
            side.event(&panel_tree(n));
        }
        bench.apply(&AgentEvent::TextDelta {
            index: 0,
            text: if n % 10 == 9 {
                format!("word {n} ends a paragraph.\n\n")
            } else {
                format!("word {n} of a streamed answer, ")
            },
        });
        bench.clock += Duration::from_millis(33);
        let layout = bench.layout();
        let _ = bench
            .transcript
            .frame_at(&Renderer::new(layout), bench.clock);
        bench.view.sync(bench.transcript.cells().len());
        let rows = height.saturating_sub(4);
        let mut paint = Paint {
            transcript: &mut bench.transcript,
            layout,
            banner: &bench.banner,
            banner_live: false,
            caret: None,
        };
        let shown = bench.view.layout(usize::from(rows), &mut paint);
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        let edge = u16::try_from(rule).unwrap();
        for (y, line) in (0..rows).zip(shown.rows.iter()) {
            buffer.set_line(0, y, &bench.theme.line(line), edge);
        }
        let panel_started = Instant::now();
        let on = side.on(&|_| None);
        let panel = side.draw(
            &on,
            Frame {
                height: usize::from(rows),
                at: bench.clock,
                now_ms: 1_000 + n * 33,
                cx,
                layout: Layout::new(band + RULE, GlyphSet::Unicode),
                whole: false,
                pictures: false,
            },
        );
        for (y, line) in (0..rows).zip(panel.lines.iter()) {
            buffer.set_line(edge, y, &bench.theme.line(line), width - edge);
        }
        panels.push(panel_started.elapsed());
        if let Some(plan) = bench.painter.plan(&buffer, None).unwrap() {
            bench.painter.sent(plan, Duration::ZERO);
        }
        frames.push(started.elapsed());
    }
    let worst = frames.iter().max().copied().unwrap_or_default();
    let p95 = percentile(&mut frames, 95);
    let panel_worst = panels.iter().max().copied().unwrap_or_default();
    let panel_p95 = percentile(&mut panels, 95);
    eprintln!(
        "BUDGET docked panel 120x40: {} frames, p95 {p95:?}, max {worst:?}; the panel's part p95 {panel_p95:?}, max {panel_worst:?}; rss {} MB",
        frames.len(),
        rss_kb() / 1024
    );
    assert!(side.ticks().pulse);
    assert!(worst <= FRAME_BUDGET, "the slowest frame took {worst:?}");
}

fn page_blocks(round: u64) -> Vec<SlotBlock> {
    let since = |n: u64| SlotSpan::Elapsed {
        since_ms: n * 1_000,
        tone: SlotTone::Muted,
    };
    let words = |n: u64| SlotSpan::Text {
        text: format!("item {n} of the page, with a few words to wrap at the column's width"),
        tone: SlotTone::Text,
        bold: false,
    };
    vec![
        SlotBlock::Line {
            spans: vec![words(round), since(round)],
        },
        SlotBlock::Rows {
            rows: vec![wisp_protocol::slot::SlotRow {
                key: format!("key {round}"),
                value: vec![since(round)],
            }],
        },
        SlotBlock::List {
            items: vec![vec![words(round), since(round)]],
        },
        SlotBlock::Gauge {
            label: format!("gauge {round}"),
            value: round % 10,
            max: 10,
            tone: SlotTone::Accent,
            note: vec![since(round)],
        },
        SlotBlock::Spark {
            label: format!("spark {round}"),
            values: (0..120).map(|v| (v + round) % 9).collect(),
            max: 9,
            tone: SlotTone::Ok,
            note: vec![since(round)],
        },
        SlotBlock::Tree {
            nodes: (0..4u8)
                .map(|depth| SlotNode {
                    depth,
                    state: NodeState::Working,
                    role: Some("coder".into()),
                    label: format!("agent {round}.{depth}"),
                    facts: vec![
                        since(round),
                        SlotSpan::Usd {
                            micros: round * 1_000,
                            at_least: false,
                            tone: SlotTone::Muted,
                        },
                    ],
                })
                .collect(),
        },
        SlotBlock::NextColumn,
    ]
}

fn full_page() -> SlotContent {
    let mut content = SlotContent {
        note: vec![SlotSpan::Elapsed {
            since_ms: 0,
            tone: SlotTone::Muted,
        }],
        blocks: vec![],
    };
    for round in 0.. {
        let before = content.blocks.len();
        content.blocks.extend(page_blocks(round));
        if wisp_protocol::slot::check(&content, SlotView::Page).is_err() {
            content.blocks.truncate(before);
            break;
        }
    }
    content
}

#[test]
#[ignore = "a release-build measurement: heavy cargo test --release --test budgets -- --ignored --nocapture"]
fn an_open_64_kib_page_reloading_each_second_at_120_by_40() {
    use ratatui_core::layout::Size;
    use wisp_tui::app::pager::Pager;
    let (width, height) = (120u16, 40u16);
    let size = Size::new(width, height);
    let section = "agent-viz/topology";
    let mut side = Side::new(SidebarSetting::Auto, usize::from(width));
    side.event(&panel_catalog());
    let content = full_page();
    let bytes = serde_json::to_vec(&content).unwrap().len();
    assert!(bytes > 60_000, "{bytes}");
    side.event(&AgentEvent::SlotContent {
        section: section.to_owned(),
        view: SlotView::Page,
        content,
    });
    assert!(side.page_clock(section));
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let mut painter = Painter::new(false);
    let mut pager = Pager::section(section.to_owned());
    let mut frames = vec![];
    for second in 0..120u64 {
        let started = Instant::now();
        let now_ms = 1_000_000 + second * 1_000;
        pager.refresh();
        let mut title = None;
        pager.fit(size, |w| {
            let page = side
                .page(
                    section,
                    now_ms,
                    Layout::new(usize::from(w), GlyphSet::Unicode),
                )
                .unwrap();
            title = Some(page.title);
            page.rows
        });
        pager.set_title(title);
        let buffer = pager.view(&theme, GlyphSet::Unicode, size);
        if let Some(plan) = painter.plan(&buffer, None).unwrap() {
            painter.sent(plan, Duration::ZERO);
        }
        frames.push(started.elapsed());
    }
    let worst = frames.iter().max().copied().unwrap_or_default();
    let p95 = percentile(&mut frames, 95);
    eprintln!(
        "BUDGET open page 120x40, {bytes} bytes: {} reloads, p95 {p95:?}, max {worst:?}; rss {} MB",
        frames.len(),
        rss_kb() / 1024
    );
    assert!(pager.rows().len() > 100, "{}", pager.rows().len());
    assert!(worst <= FRAME_BUDGET, "the slowest reload took {worst:?}");
}
