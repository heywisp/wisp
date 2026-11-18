use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::session::Rig;

const WAIT: Duration = Duration::from_secs(300);
pub(crate) const FIELDS: [&str; 5] = ["VmRSS", "RssAnon", "RssFile", "VmHWM", "Threads"];
pub(crate) const SAMPLED: [usize; 5] = [1, 30, 60, 90, 120];

pub(crate) const VARIANTS: [(&str, &str, &str); 5] = [
    ("default", "", ""),
    ("arena_max=1", "GLIBC_TUNABLES", "glibc.malloc.arena_max=1"),
    (
        "mmap_threshold=131072",
        "GLIBC_TUNABLES",
        "glibc.malloc.mmap_threshold=131072",
    ),
    ("TOKIO_WORKER_THREADS=2", "TOKIO_WORKER_THREADS", "2"),
    ("TOKIO_WORKER_THREADS=4", "TOKIO_WORKER_THREADS", "4"),
];

#[derive(Clone, Debug)]
pub(crate) struct Sample {
    pub(crate) at: String,
    pub(crate) values: [u64; 5],
}

pub(crate) fn status(pid: u32) -> [u64; 5] {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    FIELDS.map(|field| {
        text.lines()
            .find_map(|l| l.strip_prefix(field)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    })
}

struct Wisp {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
}

impl Wisp {
    fn start(rig: &Rig, variant: (&str, &str, &str), resume: Option<&str>) -> Wisp {
        let root = &rig.scratch.root;
        let mut command = Command::new(env!("CARGO_BIN_EXE_wisp"));
        command
            .current_dir(root.join("work"))
            .env_clear()
            .env("WISP_KEYCHAIN", "off")
            .env("WISP_TESTKIT", "inert")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root.join("home"))
            .env("TMPDIR", root.join("tmp"))
            .env("LANG", "C.UTF-8")
            .env("ANTHROPIC_API_KEY", "memory-made-up-key")
            .env("ANTHROPIC_BASE_URL", &rig.server.url)
            .args(["--output", "jsonl"])
            .args(["--compact-at", &super::script::P_COMPACT_AT.to_string()]);
        if !variant.1.is_empty() {
            command.env(variant.1, variant.2);
        }
        if let Some(id) = resume {
            command.args(["--resume", id]);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if send.send(line).is_err() {
                    return;
                }
            }
        });
        Wisp {
            child,
            stdin,
            lines,
        }
    }

    fn turn(&mut self, text: &str) -> usize {
        writeln!(self.stdin, "{text}").unwrap();
        self.stdin.flush().unwrap();
        let mut compactions = 0;
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .unwrap_or_else(|_| panic!("wisp gave no turn_ended within {WAIT:?}"));
            if line.contains(r#""type":"compacted""#) {
                compactions += 1;
            }
            if line.starts_with(r#"{"type":"turn_ended""#) {
                return compactions;
            }
        }
    }

    fn end(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "wisp exited with {status}");
    }
}

fn session_id(sessions: &Path) -> String {
    let mut ids: Vec<String> = std::fs::read_dir(sessions)
        .unwrap()
        .filter_map(|e| {
            let path = e.ok()?.path();
            (path.extension()? == "jsonl").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids.remove(0)
}

pub(crate) fn variant(rig: &Rig, variant: (&str, &str, &str), first: usize) -> Vec<Sample> {
    rig.script.reset();
    let mut samples = Vec::new();
    let mut wisp = Wisp::start(rig, variant, None);
    let pid = wisp.child.id();
    for n in 0..first {
        let compacted = wisp.turn(&rig.turns[n].text);
        let at = n + 1;
        if SAMPLED.contains(&at) {
            samples.push(Sample {
                at: format!("turn {at}"),
                values: status(pid),
            });
        }
        if compacted > 0 {
            samples.push(Sample {
                at: format!("after the compaction in turn {at}"),
                values: status(pid),
            });
        }
    }
    wisp.end();
    let sessions = rig.scratch.root.join("home/.local/share/wisp/sessions");
    let id = session_id(&sessions);
    let mut wisp = Wisp::start(rig, variant, Some(&id));
    let pid = wisp.child.id();
    for n in first..rig.turns.len() {
        wisp.turn(&rig.turns[n].text);
        if n == first {
            samples.push(Sample {
                at: format!("after the resume, turn {}", n + 1),
                values: status(pid),
            });
        }
    }
    samples.push(Sample {
        at: format!("after the resume, turn {}", rig.turns.len()),
        values: status(pid),
    });
    wisp.end();
    std::fs::remove_dir_all(&sessions).unwrap();
    samples
}

pub(crate) fn table(name: &str, samples: &[Sample]) -> String {
    let mut out = format!(
        "RSS {name} (kB; Threads a count)\n| point | {} |\n|---|{}\n",
        FIELDS.join(" | "),
        "---:|".repeat(FIELDS.len())
    );
    for sample in samples {
        let values: Vec<String> = sample.values.iter().map(u64::to_string).collect();
        let _ = writeln!(out, "| {} | {} |", sample.at, values.join(" | "));
    }
    out
}

pub(crate) fn anon_at(samples: &[Sample], at: &str) -> u64 {
    samples
        .iter()
        .find(|s| s.at.starts_with(at))
        .map_or(0, |s| s.values[1])
}
