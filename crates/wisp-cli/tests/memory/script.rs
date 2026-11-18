use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::server::{Script, Step};

pub(crate) const SUMMARY: &str = "## Goal and standing instructions\nKeep the measured session going.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: the earlier turns.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

const WORDS: [&str; 16] = [
    "kernel", "history", "request", "budget", "session", "client", "stream", "render", "turn",
    "agent", "cache", "prefix", "image", "block", "view", "record",
];

pub(crate) enum Part {
    Think(String),
    Text(String),
    Call(String, &'static str, Value),
    Compaction(&'static str),
}

pub(crate) enum Tool {
    Echo(usize),
    Snap(u64),
    Starter(usize),
    Diff(usize),
    Spawn(Vec<(String, String)>),
    Report,
}

pub(crate) struct Turn {
    pub(crate) text: String,
    pub(crate) image: Option<u64>,
    pub(crate) last: String,
}

pub(crate) struct Scenario {
    pub(crate) turns: Vec<Turn>,
    pub(crate) script: Script,
    pub(crate) models: String,
    pub(crate) background: bool,
    pub(crate) children: Vec<String>,
}

pub(crate) fn filler(bytes: usize, seed: u64) -> String {
    let mut out = String::with_capacity(bytes + 16);
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    while out.len() < bytes {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let word = WORDS[usize::try_from(state >> 60).unwrap()];
        out.push_str(word);
        out.push(if state & 0x3f == 0 { '\n' } else { ' ' });
    }
    out.truncate(bytes);
    out
}

pub(crate) fn markdown(seed: u64) -> String {
    let mut out = format!("## Step {seed}\n\n");
    out.push_str(&filler(500, seed));
    out.push_str("\n\n- one point\n- another point\n\n```rust\n");
    for line in 0..40 {
        out.push_str(&format!(
            "    let value_{line} = compute({line}, &settings_{seed})?;\n"
        ));
    }
    out.push_str("```\n\n");
    let rest = 1_536usize.saturating_sub(out.len());
    out.push_str(&filler(rest.max(100), seed + 7));
    out
}

pub(crate) fn sse(id: &str, input: u64, parts: &[Part], stop: &str) -> String {
    let mut body = event(&json!({"type": "message_start", "message": {
        "id": id, "type": "message", "role": "assistant", "model": wisp_core::agent::DEFAULT_MODEL,
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": input, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 1}
    }}));
    for (index, part) in parts.iter().enumerate() {
        let start = match part {
            Part::Think(_) => json!({"type": "thinking", "thinking": "", "signature": ""}),
            Part::Text(_) => json!({"type": "text", "text": ""}),
            Part::Call(id, name, _) => {
                json!({"type": "tool_use", "id": id, "name": name, "input": {}})
            }
            Part::Compaction(text) => json!({"type": "compaction", "content": text}),
        };
        body.push_str(&event(
            &json!({"type": "content_block_start", "index": index, "content_block": start}),
        ));
        let deltas = match part {
            Part::Think(text) => vec![
                json!({"type": "thinking_delta", "thinking": text}),
                json!({"type": "signature_delta", "signature": format!("c2lnbmF0dXJl{index}{id}==")}),
            ],
            Part::Text(text) => vec![json!({"type": "text_delta", "text": text})],
            Part::Call(_, _, input) => {
                vec![json!({"type": "input_json_delta", "partial_json": input.to_string()})]
            }
            Part::Compaction(_) => vec![],
        };
        for delta in deltas {
            body.push_str(&event(
                &json!({"type": "content_block_delta", "index": index, "delta": delta}),
            ));
        }
        body.push_str(&event(
            &json!({"type": "content_block_stop", "index": index}),
        ));
    }
    body.push_str(&event(&json!({"type": "message_delta",
        "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": 400}})));
    body.push_str(&event(&json!({"type": "message_stop"})));
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn event(value: &Value) -> String {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().unwrap()
    )
}

fn call(id: &str, tool: &Tool, seed: u64) -> Vec<Part> {
    let one = |name, input| vec![Part::Call(id.to_owned(), name, input)];
    match tool {
        Tool::Echo(bytes) => one("echo", json!({"text": filler(*bytes, seed)})),
        Tool::Snap(size) => one("snap", json!({"size": size})),
        Tool::Starter(bytes) => one(
            "starter",
            json!({"command": format!("head -c {bytes} /dev/zero | tr '\\0' x; echo")}),
        ),
        Tool::Diff(lines) => {
            let before: String = (0..*lines).map(|l| format!("let a_{l} = {l};\n")).collect();
            let after: String = (0..*lines)
                .map(|l| format!("let a_{l} = {} + {seed};\n", l * 2))
                .collect();
            one(
                "show_diff",
                json!({"title": "A change", "files": [{"path": "src/lib.rs", "before": before, "after": after}]}),
            )
        }
        Tool::Spawn(children) => children
            .iter()
            .enumerate()
            .map(|(n, (name, task))| {
                Part::Call(
                    format!("{id}_{n}"),
                    "spawn_agent",
                    json!({"name": name, "task": task}),
                )
            })
            .collect(),
        Tool::Report => one("report", json!({"text": filler(600, seed)})),
    }
}

pub(crate) struct Builder {
    steps: BTreeMap<String, Step>,
    pub(crate) turns: Vec<Turn>,
}

impl Builder {
    pub(crate) fn new() -> Builder {
        Builder {
            steps: BTreeMap::new(),
            turns: Vec::new(),
        }
    }

    fn step(&mut self, marker: String, response: String) {
        let step = Step {
            order: self.steps.len(),
            response,
            served: AtomicBool::new(false),
        };
        assert!(self.steps.insert(marker, step).is_none());
    }

    pub(crate) fn two_steps(
        &mut self,
        label: &str,
        seed: u64,
        tool: &Tool,
        usage: [u64; 2],
    ) -> (String, String) {
        let first = format!("MQ-{label}a");
        let second = format!("MQ-{label}b");
        let mut parts = vec![
            Part::Think(filler(2_048, seed)),
            Part::Text(filler(600, seed + 1)),
        ];
        parts.extend(call(&format!("toolu_{second}"), tool, seed));
        self.step(
            first.clone(),
            sse(&format!("msg_{label}a"), usage[0], &parts, "tool_use"),
        );
        self.step(
            second.clone(),
            sse(
                &format!("msg_{label}b"),
                usage[1],
                &[Part::Text(markdown(seed))],
                "end_turn",
            ),
        );
        (first, second)
    }

    pub(crate) fn child(&mut self, label: &str, seed: u64, steps: usize) -> (String, String) {
        let mut keys: Vec<String> = (0..steps).map(|s| format!("MQ-{label}s{s}")).collect();
        keys.push(format!("MQ-{label}end"));
        for s in 0..steps {
            let tool = if s + 1 == steps {
                Tool::Report
            } else {
                Tool::Echo(8_192)
            };
            let mut parts = vec![
                Part::Think(filler(1_024, seed + s as u64)),
                Part::Text(filler(300, seed + s as u64)),
            ];
            parts.extend(call(&format!("toolu_{}", keys[s + 1]), &tool, seed));
            self.step(
                keys[s].clone(),
                sse(&format!("msg_{label}{s}"), 3_000, &parts, "tool_use"),
            );
        }
        self.step(
            keys[steps].clone(),
            sse(
                &format!("msg_{label}end"),
                3_000,
                &[Part::Text("Reported.".to_owned())],
                "end_turn",
            ),
        );
        (keys[0].clone(), keys[steps - 1].clone())
    }

    pub(crate) fn turn(&mut self, n: usize, (first, last): (String, String), image: Option<u64>) {
        self.turns.push(Turn {
            text: format!("{first} Turn {n}: carry on with the work."),
            image,
            last,
        });
    }

    pub(crate) fn finish(
        self,
        models: String,
        background: bool,
        children: Vec<String>,
    ) -> Scenario {
        Scenario {
            turns: self.turns,
            script: Script {
                steps: self.steps,
                native: sse(
                    "msg_native",
                    ASK_INPUT,
                    &[Part::Compaction("So far: the measured session.")],
                    "compaction",
                ),
                summary: sse(
                    "msg_summary",
                    ASK_INPUT,
                    &[Part::Text(SUMMARY.to_owned())],
                    "end_turn",
                ),
                fallback: sse(
                    "msg_fallback",
                    5_000,
                    &[Part::Text("Noted.".to_owned())],
                    "end_turn",
                ),
            },
            models,
            background,
            children,
        }
    }
}

const ASK_INPUT: u64 = 150_000;

pub(crate) const B_COMPACT_AT: u64 = 100_000;

pub(crate) fn b(turns: usize) -> Scenario {
    let mut builder = Builder::new();
    for n in 1..=turns {
        let seed = n as u64;
        let tool = if n % 2 == 1 {
            Tool::Echo(48 * 1024)
        } else {
            Tool::Snap(100_000 + seed * 1_013)
        };
        let after = if n == 10 {
            B_COMPACT_AT + 20_000
        } else {
            4_000 + seed * 500
        };
        let markers = builder.two_steps(
            &format!("r{n:03}"),
            seed,
            &tool,
            [3_000 + seed * 500, after],
        );
        let image = matches!(n, 4 | 12).then_some(seed);
        builder.turn(n, markers, image);
    }
    builder.finish(models(B_COMPACT_AT, false), false, Vec::new())
}

pub(crate) fn models(compact_at: u64, native: bool) -> String {
    let mut text = format!(
        "[models.\"{}\"]\ncompact_at = {compact_at}\n",
        wisp_core::agent::DEFAULT_MODEL
    );
    if !native {
        text.push_str("native_compaction = false\n");
    }
    text
}

pub(crate) const P_TURNS: usize = 160;
pub(crate) const P_COMPACT_AT: u64 = 200_000;
pub(crate) const P_LEG: usize = 50;
const P_EPOCH: usize = 40;

pub(crate) fn p(turns: usize, with_tools: bool) -> Scenario {
    let mut builder = Builder::new();
    let mut children = Vec::new();
    for n in 1..=turns {
        let seed = n as u64;
        let tool = if !with_tools {
            None
        } else if n == P_LEG {
            let tasks = (1..=3)
                .map(|c| {
                    let (first, end) = builder.child(&format!("c{c}"), seed * 10 + c, 5);
                    children.push(end);
                    (
                        format!("child-{c}"),
                        format!("{first} Child {c}: check one part and report."),
                    )
                })
                .collect();
            Some(Tool::Spawn(tasks))
        } else if n % 20 == 5 {
            Some(Tool::Starter(200_000))
        } else if n % 10 == 0 {
            Some(Tool::Diff(300))
        } else if n % 2 == 1 {
            Some(Tool::Echo(48 * 1024))
        } else {
            Some(Tool::Snap(100_000 + seed * 1_013))
        };
        let ramp = if n % P_EPOCH == 0 && with_tools {
            P_COMPACT_AT - 30_000
        } else if n % P_EPOCH == 0 {
            P_COMPACT_AT + 10_000
        } else {
            20_000 + (seed % P_EPOCH as u64) * 3_000
        };
        let label = format!("r{n:03}");
        let markers = match tool {
            Some(tool) => builder.two_steps(&label, seed, &tool, [ramp, ramp + 2_000]),
            None => builder.text_turn(&label, seed, ramp),
        };
        let image = (with_tools && n % 20 == 3).then_some(seed);
        builder.turn(n, markers, image);
    }
    builder.finish(models(P_COMPACT_AT, true), true, children)
}

impl Builder {
    pub(crate) fn text_turn(&mut self, label: &str, seed: u64, usage: u64) -> (String, String) {
        let first = format!("MQ-{label}a");
        let parts = [
            Part::Think(filler(2_048, seed)),
            Part::Text(filler(48 * 1024, seed + 1)),
            Part::Text(markdown(seed)),
        ];
        self.step(
            first.clone(),
            sse(&format!("msg_{label}a"), usage, &parts, "end_turn"),
        );
        (first.clone(), first)
    }
}
