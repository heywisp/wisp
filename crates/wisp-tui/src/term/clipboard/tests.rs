use std::path::Path;

use super::*;
use crate::scratch::{ScratchDir, alive};
use crate::term::caps::{Caps, Flags, Streams};

const TTY: Streams = Streams {
    stdin_tty: true,
    stdout_tty: true,
    stderr_tty: true,
};

fn env(pairs: &[(&str, &str)]) -> Env {
    Env::from_pairs(pairs.iter().copied())
}

fn host(pairs: &[(&str, &str)]) -> Host {
    Host::of(&env(pairs), None, false)
}

struct Fakes {
    dir: ScratchDir,
}

const RECORD: &str = r#"#!/bin/sh
d="${0%/*}"
n="${0##*/}"
if [ -f "$d/$n.hang" ]; then
  /bin/sleep 30 &
  echo $! > "$d/grandchild"
  echo $$ > "$d/leader"
  wait
fi
for a in "$@"; do printf '%s\n' "$a" >> "$d/$n.args"; done
printf -- '--\n' >> "$d/$n.args"
if [ -f "$d/$n.slow" ]; then /bin/rm "$d/$n.slow"; /bin/sleep 1; fi
if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then echo tty >> "$d/tty"; fi
/bin/cat >> "$d/$n.in"
if [ -f "$d/$n.fail" ]; then exit 3; fi
exit 0
"#;

const TMUX: &str = r#"#!/bin/sh
d="$(dirname "$0")"
for a in "$@"; do printf '%s\n' "$a" >> "$d/tmux.args"; done
printf -- '--\n' >> "$d/tmux.args"
case "$1" in
  show-options) /bin/cat "$d/setclip" ;;
  display-message) printf '$4\n' ;;
  list-clients) printf '100 /dev/pts/1\n300 /dev/pts/7\n200 /dev/pts/3\n' ;;
  load-buffer) /bin/cat >> "$d/tmux.in" ;;
esac
exit 0
"#;

const GATE: &str = r#"#!/bin/sh
d="$(dirname "$0")"
echo $$ > "$d/gated"
until [ -e "$d/go" ] || [ ! -d "$d" ]; do /bin/sleep 0.02; done
exec "$(/bin/cat "$d/setsid.real")" "$@"
"#;

impl Fakes {
    fn new(name: &str, tools: &[&str]) -> Fakes {
        let dir = ScratchDir::new(crate::SCRATCH, name);
        for tool in tools {
            fake_tool::install(
                &dir.root.join(tool),
                if *tool == "tmux" { TMUX } else { RECORD },
            );
        }
        std::fs::write(dir.root.join("setclip"), "external\n").ok();
        Fakes { dir }
    }

    fn tools(&self) -> Tools {
        Tools {
            path: Some(self.dir.root.clone().into_os_string()),
            powershell: vec![],
            setsid: system_setsid(),
            timeout: Duration::from_secs(5),
        }
    }

    fn gated(&self) -> Tools {
        let mut tools = self.tools();
        tools.setsid = tools.setsid.map(|real| {
            self.touch("setsid.real", &real.to_string_lossy());
            fake_tool::install(&self.dir.root.join("gated-setsid"), GATE);
            self.dir.root.join("gated-setsid")
        });
        tools
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.root.join(file)).unwrap_or_default()
    }

    fn touch(&self, file: &str, body: &str) {
        std::fs::write(self.dir.root.join(file), body).ok();
    }

    fn args(&self, tool: &str) -> Vec<Vec<String>> {
        let all = self.read(&format!("{tool}.args"));
        let mut calls: Vec<&str> = all.split("--\n").collect();
        calls.pop();
        calls
            .into_iter()
            .map(|c| c.lines().map(str::to_owned).collect())
            .collect()
    }
}

fn local(natives: &[Native], tmux: bool) -> Route {
    Route::Local {
        tmux,
        natives: natives.to_vec(),
    }
}

#[test]
fn hosts_are_read_from_the_environment_and_the_kernel_release() {
    let wsl = Host::of(
        &env(&[]),
        Some("6.18.33.2-microsoft-standard-WSL2\n"),
        false,
    );
    assert!(wsl.wsl);
    assert!(host(&[("WSL_DISTRO_NAME", "Ubuntu")]).wsl);
    assert!(!host(&[("WSL_DISTRO_NAME", "")]).wsl);
    assert!(host(&[("SSH_TTY", "/dev/pts/1")]).ssh);
    assert!(host(&[("SSH_CONNECTION", "1 2 3 4")]).ssh);
    assert!(!host(&[("SSH_CLIENT", "1 2 3")]).ssh);
    let h = host(&[
        ("WAYLAND_DISPLAY", "wayland-0"),
        ("DISPLAY", ":0"),
        ("TERM_PROGRAM", "vscode"),
        ("WT_SESSION", "x"),
        ("TMUX", "/tmp/s,1,0"),
        ("ZELLIJ", "0"),
    ]);
    assert!(h.wayland && h.x11 && h.vscode && h.windows_terminal && h.tmux && h.zellij);
}

#[test]
fn copy_on_release_follows_the_terminal_and_its_override() {
    let mut warnings = vec![];
    let mut on = |pairs: &[(&str, &str)], wsl: bool| {
        let e = env(pairs);
        let h = Host {
            wsl,
            ..Host::of(&e, None, false)
        };
        let caps = Caps::resolve(&e, TTY, Flags::default(), None);
        warnings.extend(caps.warnings);
        copy_on_release(caps.copy_on_select, &h).value
    };
    assert!(on(&[], false));
    assert!(!on(&[("WT_SESSION", "x")], true));
    assert!(!on(&[("TERM_PROGRAM", "vscode")], true));
    assert!(!on(
        &[("TERM_PROGRAM", "vscode"), ("WT_SESSION", "x")],
        true
    ));
    assert!(on(
        &[("TERM_PROGRAM", "vscode"), ("WT_SESSION", "x")],
        false
    ));
    assert!(on(&[("WT_SESSION", "x"), ("TMUX", "/tmp/s,1,0")], true));
    assert!(on(&[("WT_SESSION", "x"), ("ZELLIJ", "0")], true));
    assert!(on(
        &[("TERM_PROGRAM", "vscode"), ("TMUX", "/tmp/s,1,0")],
        true
    ));
    assert!(on(
        &[("WT_SESSION", "x"), ("WISP_COPY_ON_SELECT", "1")],
        true
    ));
    assert!(!on(&[("WISP_COPY_ON_SELECT", "0")], false));
    assert!(!on(
        &[("TMUX", "/tmp/s,1,0"), ("WISP_COPY_ON_SELECT", "off")],
        false
    ));
    assert!(on(&[("WISP_COPY_ON_SELECT", "maybe")], false));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

#[test]
fn right_click_pastes_only_where_the_terminal_would_have() {
    let on = |pairs: &[(&str, &str)], wsl: bool| right_click_pastes(&Host { wsl, ..host(pairs) });
    assert!(on(&[("WT_SESSION", "x")], true));
    assert!(on(&[("WT_SESSION", "x")], false));
    assert!(!on(&[("TERM_PROGRAM", "vscode")], true));
    assert!(!on(&[("TERM_PROGRAM", "vscode")], false));
    assert!(!on(
        &[("TERM_PROGRAM", "vscode"), ("WT_SESSION", "x")],
        true
    ));
    assert!(!on(&[], true));
    assert!(!on(&[("TERM_PROGRAM", "iTerm.app")], false));
    assert!(!on(&[("WT_SESSION", "x"), ("SSH_TTY", "/dev/pts/1")], true));
    assert!(!on(
        &[("TERM_PROGRAM", "vscode"), ("SSH_CONNECTION", "a b c d")],
        true
    ));
}

#[test]
fn only_vs_code_pastes_its_own_clipboard_on_right_click() {
    let on = |pairs: &[(&str, &str)], wsl: bool| {
        terminal_pastes_on_right_click(&Host { wsl, ..host(pairs) })
    };
    assert!(on(&[("TERM_PROGRAM", "vscode")], true));
    assert!(on(&[("TERM_PROGRAM", "vscode")], false));
    assert!(on(
        &[("TERM_PROGRAM", "vscode"), ("SSH_CONNECTION", "a b c d")],
        false
    ));
    assert!(!on(&[("WT_SESSION", "x")], true));
    assert!(!on(&[("TERM_PROGRAM", "iTerm.app")], false));
    assert!(!on(&[], false));
}

#[test]
fn ssh_outside_tmux_sends_to_the_terminal_and_everything_else_stays_local() {
    assert_eq!(route(&host(&[("SSH_TTY", "/dev/pts/1")])), Route::Terminal);
    assert_eq!(
        route(&host(&[
            ("SSH_TTY", "/dev/pts/1"),
            ("DISPLAY", ":0"),
            ("TMUX", "t")
        ])),
        local(&[], true)
    );
    let wsl = Host {
        wsl: true,
        ..host(&[("WAYLAND_DISPLAY", "w"), ("DISPLAY", ":0")])
    };
    assert_eq!(
        route(&wsl),
        local(&[Native::Windows, Native::Wayland, Native::X11], false)
    );
    let mac = Host {
        macos: true,
        ..host(&[])
    };
    assert_eq!(route(&mac), local(&[Native::Mac], false));
    assert_eq!(route(&host(&[])), local(&[], false));
    for version in ["1.124.2", "1.125.0", "1.146.0"] {
        let h = host(&[
            ("WSL_DISTRO_NAME", "Ubuntu"),
            ("TERM_PROGRAM", "vscode"),
            ("TERM_PROGRAM_VERSION", version),
        ]);
        assert_eq!(route(&h), local(&[Native::Windows], false), "{version}");
    }
    for (key, value) in [
        ("TERM_PROGRAM", "vscode"),
        ("TERM_PROGRAM", "unknown"),
        ("WT_SESSION", "x"),
    ] {
        assert_eq!(
            route(&host(&[("WSL_DISTRO_NAME", "Ubuntu"), (key, value)])),
            local(&[Native::Windows], false)
        );
    }
    for (key, value) in [("TMUX", "t"), ("ZELLIJ", "0"), ("STY", "s")] {
        let h = host(&[
            ("WSL_DISTRO_NAME", "Ubuntu"),
            ("TERM_PROGRAM", "vscode"),
            ("TERM_PROGRAM_VERSION", "1.146.0"),
            (key, value),
        ]);
        assert_eq!(route(&h), local(&[Native::Windows], h.tmux), "{key}");
    }
}

#[test]
fn base64_matches_the_standard_alphabet_and_padding() {
    for (input, want) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foobar", "Zm9vYmFy"),
        ("é\n", "w6kK"),
    ] {
        assert_eq!(
            wisp_media::encode_base64(input.as_bytes()),
            want,
            "{input:?}"
        );
    }
    assert_eq!(wisp_media::encode_base64(&utf16le(WSL_SCRIPT)), WSL_ENCODED);
}

#[test]
fn osc_52_is_base64_under_the_cap_and_refused_over_it() {
    let seq = Osc52::new("hi\u{1b}there").unwrap_or_else(|_| Osc52 { bytes: vec![] });
    assert_eq!(seq.bytes(), b"\x1b]52;c;aGkbdGhlcmU=\x07");
    let text = "你好 🦀\r\nline\nlast";
    let osc = Osc52::new(text).unwrap();
    let encoded = &osc.bytes()[7..osc.bytes().len() - 1];
    assert_eq!(
        wisp_media::decode_base64(std::str::from_utf8(encoded).unwrap()).unwrap(),
        text.as_bytes()
    );
    let boundary = "é".repeat(OSC52_MAX / 2);
    assert!(Osc52::new(&boundary).is_ok());
    assert_eq!(Osc52::new(&format!("{boundary}x")), Err(TooLarge));
}

#[test]
fn toasts_name_the_count_and_where_the_text_went() {
    let done = |chars, outcome| Done {
        chars,
        outcome,
        purpose: Purpose::Selection,
    };
    assert_eq!(
        toast(
            &done(
                1204,
                Outcome::Copied(vec![Destination::Native(Native::Windows)])
            ),
            "\u{b7}"
        ),
        "Copied 1,204 characters \u{b7} Windows clipboard"
    );
    for (to, shown) in [
        (
            vec![Destination::Tmux { forwards: true }],
            "tmux buffer and clipboard",
        ),
        (
            vec![
                Destination::Tmux { forwards: true },
                Destination::Native(Native::Wayland),
            ],
            "tmux buffer and clipboard",
        ),
        (
            vec![
                Destination::Tmux { forwards: false },
                Destination::Native(Native::Windows),
            ],
            "tmux buffer and Windows clipboard",
        ),
    ] {
        assert_eq!(
            toast(&done(1, Outcome::Copied(to)), "\u{b7}"),
            format!("Copied 1 character \u{b7} {shown}")
        );
    }
    assert_eq!(
        toast(&done(1204, Outcome::Sent), "\u{b7}"),
        "Sent 1,204 characters to your terminal (OSC 52)"
    );
    assert_eq!(
        toast(&done(200_000, Outcome::TooLarge), "\u{b7}"),
        "Too large to send to your terminal (over 100,000 bytes)."
    );
    assert_eq!(
        toast(
            &done(3, Outcome::Failed("install wl-clipboard".to_owned())),
            "\u{b7}"
        ),
        "Couldn't copy: install wl-clipboard"
    );
}

#[test]
fn the_latest_client_wins_and_odd_names_are_refused() {
    assert_eq!(
        latest_client("100 /dev/pts/1\n300 /dev/pts/7\n200 /dev/pts/3\n").as_deref(),
        Some("/dev/pts/7")
    );
    assert_eq!(
        latest_client("900 -t\n1 /dev/pts/2").as_deref(),
        Some("/dev/pts/2")
    );
    assert_eq!(latest_client("x y\n"), None);
    assert_eq!(latest_client(""), None);
}

#[test]
fn tools_are_found_on_the_given_path_and_powershell_at_its_fallback() {
    let fakes = Fakes::new("find", &["pbcopy"]);
    let tools = fakes.tools();
    assert_eq!(tools.find("pbcopy"), Some(fakes.dir.root.join("pbcopy")));
    assert_eq!(tools.find("wl-copy"), None);
    assert_eq!(tools.find(POWERSHELL), None);
    std::fs::write(fakes.dir.root.join("plain"), "not executable").ok();
    assert_eq!(tools.find("plain"), None);
    let fallback = Fakes::new("fallback", &["powershell.exe"]);
    let tools = Tools {
        powershell: vec![fallback.dir.root.join(POWERSHELL)],
        ..fakes.tools()
    };
    assert_eq!(
        tools.find(POWERSHELL),
        Some(fallback.dir.root.join(POWERSHELL))
    );
    let none = Tools {
        path: Some("relative/dir".into()),
        ..tools
    };
    assert_eq!(none.find("pbcopy"), None);
}

#[test]
fn each_writer_gets_its_fixed_argv_and_the_text_on_stdin() {
    let text = "héllo\nworld $(rm -rf /) `x` ; | &";
    let cases: [(Native, &str, Vec<Vec<&str>>, &str); 4] = [
        (
            Native::Windows,
            "powershell.exe",
            vec![vec![
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                WSL_ENCODED,
            ]],
            "Windows clipboard",
        ),
        (Native::Mac, "pbcopy", vec![vec![]], "clipboard"),
        (
            Native::Wayland,
            "wl-copy",
            vec![vec![], vec!["--primary"]],
            "clipboard",
        ),
        (
            Native::X11,
            "xclip",
            vec![
                vec!["-selection", "clipboard", "-i"],
                vec!["-selection", "primary", "-i"],
            ],
            "clipboard",
        ),
    ];
    for (native, tool, argv, dest) in cases {
        let fakes = Fakes::new(tool, &[tool]);
        let outcome = copy_local(text, false, &[native], &fakes.tools());
        assert_eq!(
            outcome,
            Outcome::Copied(vec![Destination::Native(native)]),
            "{tool}"
        );
        assert_eq!(Destination::Native(native).label(), dest, "{tool}");
        let want: Vec<Vec<String>> = argv
            .iter()
            .map(|a| a.iter().map(|s| (*s).to_owned()).collect())
            .collect();
        assert_eq!(fakes.args(tool), want, "{tool}");
        assert_eq!(
            fakes.read(&format!("{tool}.in")),
            text.repeat(argv.len()),
            "{tool}"
        );
        assert_eq!(fakes.read("tty"), "", "{tool}");
    }
}

#[test]
fn xsel_is_used_when_xclip_is_missing() {
    let fakes = Fakes::new("xsel", &["xsel"]);
    let outcome = copy_local("x", false, &[Native::X11], &fakes.tools());
    assert_eq!(
        outcome,
        Outcome::Copied(vec![Destination::Native(Native::X11)])
    );
    assert_eq!(
        fakes.args("xsel"),
        vec![
            vec!["--clipboard".to_owned(), "--input".to_owned()],
            vec!["--primary".to_owned(), "--input".to_owned()],
        ]
    );
}

#[test]
fn a_missing_tool_names_what_to_install() {
    let fakes = Fakes::new("missing", &[]);
    for (native, why) in [
        (Native::Wayland, "install wl-clipboard"),
        (Native::X11, "install xclip or xsel"),
        (Native::Mac, "pbcopy wasn't found"),
        (Native::Windows, "powershell.exe wasn't found"),
    ] {
        assert_eq!(
            copy_local("x", false, &[native], &fakes.tools()),
            Outcome::Failed(why.to_owned())
        );
    }
    assert_eq!(
        copy_local("x", false, &[], &fakes.tools()),
        Outcome::Failed("no clipboard was found for this session".to_owned())
    );
}

#[test]
fn a_failing_writer_gives_a_sentence_and_the_next_platform_is_tried() {
    let fakes = Fakes::new("failing", &["wl-copy", "xclip"]);
    fakes.touch("wl-copy.fail", "");
    assert_eq!(
        copy_local("x", false, &[Native::Wayland], &fakes.tools()),
        Outcome::Failed("wl-copy failed (exit 3)".to_owned())
    );
    assert_eq!(
        copy_local("x", false, &[Native::Wayland, Native::X11], &fakes.tools()),
        Outcome::Copied(vec![Destination::Native(Native::X11)])
    );
}

#[test]
fn a_hanging_writer_and_everything_it_started_are_killed_at_the_timeout() {
    for setsid in [system_setsid(), None] {
        let fakes = Fakes::new("hanging", &["pbcopy"]);
        fakes.touch("pbcopy.hang", "");
        let tools = Tools {
            timeout: Duration::from_secs(2),
            setsid: setsid.clone(),
            ..fakes.tools()
        };
        let (leader, grandchild) = (0..3)
            .find_map(|_| {
                let started = Instant::now();
                let outcome = copy_local("x", false, &[Native::Mac], &tools);
                assert!(
                    started.elapsed() < Duration::from_secs(15),
                    "{:?}",
                    started.elapsed()
                );
                assert_eq!(
                    outcome,
                    Outcome::Failed("pbcopy took longer than 2s".to_owned()),
                    "{setsid:?}"
                );
                let leader: Option<i32> = fakes.read("leader").trim().parse().ok();
                let grandchild: Option<i32> = fakes.read("grandchild").trim().parse().ok();
                leader.zip(grandchild)
            })
            .unzip();
        assert!(leader.is_some() && grandchild.is_some(), "{setsid:?}");
        reaped([leader, grandchild]);
        assert!(
            !leader.is_some_and(alive),
            "{setsid:?}: the writer outlived its timeout"
        );
        assert!(
            !grandchild.is_some_and(alive),
            "{setsid:?}: the writer's child outlived it"
        );
    }
}

fn reaped(pids: [Option<i32>; 2]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.into_iter().any(|pid| pid.is_some_and(alive)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn dropping_the_copier_kills_a_running_writer_and_its_group() {
    for gated in [false, true] {
        let fakes = Fakes::new(if gated { "dropped-gated" } else { "dropped" }, &["pbcopy"]);
        fakes.touch("pbcopy.hang", "");
        let tools = if gated { fakes.gated() } else { fakes.tools() };
        assert!(tools.setsid.is_some(), "no setsid to test with");
        let mut copier = Copier::new(false, vec![Native::Mac], tools, Arc::new(Notify::new()));
        assert_eq!(copier.start("x".to_owned(), Purpose::Selection), None);
        let started = if gated { "gated" } else { "leader" };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fakes.read(started).ends_with('\n') && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let leader: Option<i32> = fakes.read(started).trim().parse().ok();
        let grandchild: Option<i32> = fakes.read("grandchild").trim().parse().ok();
        assert!(leader.is_some() && (gated || grandchild.is_some()));
        let dropped = Instant::now();
        drop(copier);
        assert!(dropped.elapsed() < Duration::from_millis(100));
        fakes.touch("go", "");
        reaped([leader, grandchild]);
        assert!(
            !leader.is_some_and(alive),
            "gated {gated}: the writer outlived the copier"
        );
        assert!(
            !grandchild.is_some_and(alive),
            "gated {gated}: the writer's child outlived the copier"
        );
    }
}

#[test]
fn timeouts_read_in_milliseconds_below_a_second() {
    assert_eq!(span(Duration::from_millis(300)), "300ms");
    assert_eq!(span(Duration::from_secs(5)), "5s");
    assert_eq!(span(Duration::from_millis(1500)), "2s");
    assert_eq!(span(Duration::from_secs(65)), "1m 05s");
}

#[test]
fn tmux_forwards_to_the_most_recently_active_client_of_this_session() {
    let fakes = Fakes::new("tmux-on", &["tmux", "wl-copy"]);
    let outcome = copy_local("copied", true, &[Native::Wayland], &fakes.tools());
    assert_eq!(
        outcome,
        Outcome::Copied(vec![
            Destination::Tmux { forwards: true },
            Destination::Native(Native::Wayland)
        ])
    );
    let calls = fakes.args("tmux");
    let strs = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![
            strs(&["show-options", "-gv", "set-clipboard"]),
            strs(&["display-message", "-p", "#{session_id}"]),
            strs(&[
                "list-clients",
                "-t",
                "$4",
                "-F",
                "#{client_activity} #{client_name}"
            ]),
            strs(&["load-buffer", "-w", "-t", "/dev/pts/7", "-"]),
        ]
    );
    assert_eq!(fakes.read("tmux.in"), "copied");
    assert_eq!(fakes.read("wl-copy.in"), "copiedcopied");
}

#[test]
fn tmux_with_set_clipboard_off_writes_only_the_buffer() {
    let fakes = Fakes::new("tmux-off", &["tmux"]);
    fakes.touch("setclip", "off\n");
    let outcome = copy_local("copied", true, &[], &fakes.tools());
    assert_eq!(
        outcome,
        Outcome::Copied(vec![Destination::Tmux { forwards: false }])
    );
    let calls = fakes.args("tmux");
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[1], vec!["load-buffer".to_owned(), "-".to_owned()]);
    assert_eq!(fakes.read("tmux.in"), "copied");
}

#[test]
fn the_copier_works_off_the_calling_thread_and_wakes_the_app() {
    let fakes = Fakes::new("copier", &["pbcopy"]);
    fakes.touch("pbcopy.hang", "");
    let wake = Arc::new(Notify::new());
    let mut copier = Copier::new(
        false,
        vec![Native::Mac],
        Tools {
            timeout: Duration::from_millis(500),
            ..fakes.tools()
        },
        Arc::clone(&wake),
    );
    let started = Instant::now();
    assert_eq!(copier.start("text".to_owned(), Purpose::Selection), None);
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(copier.take().is_empty());
    let deadline = Instant::now() + Duration::from_secs(10);
    let done = loop {
        let got = copier.take();
        if let Some(done) = got.into_iter().next() {
            break done;
        }
        assert!(Instant::now() < deadline, "no result");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(done.chars, 4);
    assert!(matches!(done.outcome, Outcome::Failed(ref why) if why.contains("longer")));
    assert_eq!(copier.started(), 1);
}

fn results(copier: &Copier, count: usize) -> Vec<Done> {
    let mut all = vec![];
    let deadline = Instant::now() + Duration::from_secs(10);
    while all.len() < count {
        all.extend(copier.take());
        assert!(Instant::now() < deadline, "only {all:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    all.extend(copier.take());
    all
}

#[test]
fn the_latest_copy_wins_after_a_slow_one_and_a_pending_one_is_dropped() {
    let fakes = Fakes::new("latest", &["pbcopy"]);
    fakes.touch("pbcopy.slow", "");
    let mut copier = Copier::new(
        false,
        vec![Native::Mac],
        fakes.tools(),
        Arc::new(Notify::new()),
    );
    assert_eq!(copier.start("first".to_owned(), Purpose::Selection), None);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(copier.start("second".to_owned(), Purpose::Selection), None);
    assert_eq!(copier.start("link".to_owned(), Purpose::Link), None);
    assert_eq!(
        copier.start("third".to_owned(), Purpose::Selection),
        Some(Done {
            chars: 4,
            outcome: Outcome::Failed(REPLACED.to_owned()),
            purpose: Purpose::Link,
        })
    );
    let done = results(&copier, 2);
    assert_eq!(
        done.iter().map(|d| d.chars).collect::<Vec<_>>(),
        vec![5, 5],
        "{done:?}"
    );
    assert_eq!(fakes.read("pbcopy.in"), "firstthird");
    assert_eq!(copier.started(), 4);
}

#[test]
fn inside_gnu_screen_over_ssh_osc_52_is_not_used() {
    let blocked = Route::Blocked(SCREEN_BLOCKS);
    assert_eq!(
        route(&host(&[
            ("SSH_TTY", "/dev/pts/1"),
            ("STY", "123.pts-1.box")
        ])),
        blocked
    );
    assert_eq!(
        route(&host(&[
            ("SSH_TTY", "/dev/pts/1"),
            ("TERM", "screen.xterm-256color")
        ])),
        blocked
    );
    assert_eq!(
        route(&host(&[
            ("SSH_TTY", "/dev/pts/1"),
            ("TERM", "screen-256color"),
            ("TMUX", "/tmp/s,1,0")
        ])),
        local(&[], true)
    );
    assert!(host(&[("STY", "1.x")]).screen);
    assert!(!host(&[("TERM", "screen"), ("TMUX", "t")]).screen);
    assert_eq!(
        route(&host(&[("STY", "1.x"), ("DISPLAY", ":0")])),
        local(&[Native::X11], false)
    );
    assert_eq!(
        Fakes::new("blocked", &[]).tools().writer(&blocked, None),
        format!("none: {SCREEN_BLOCKS}")
    );
}

#[test]
fn the_writer_summary_names_found_tools_or_what_is_missing() {
    let fakes = Fakes::new("summary", &[POWERSHELL, "wl-copy"]);
    let tools = fakes.tools();
    let shown = tools.writer(&local(&[Native::Wayland], false), None);
    assert!(shown.ends_with("wl-copy"), "{shown}");
    assert_eq!(
        tools.writer(&local(&[Native::X11], false), None),
        "none: install xclip or xsel"
    );
    assert_eq!(
        tools.writer(&Route::Terminal, None),
        "OSC 52, sent to your terminal"
    );
    let windows = route(&host(&[
        ("WSL_DISTRO_NAME", "Ubuntu"),
        ("TERM_PROGRAM", "vscode"),
        ("TERM_PROGRAM_VERSION", "1.146.0"),
        ("WAYLAND_DISPLAY", "w"),
    ]));
    assert_eq!(
        tools.writer(&windows, Some(&fakes.dir.root)),
        "~/powershell.exe, ~/wl-copy"
    );
    assert!(Path::new(POWERSHELL_FALLBACK).is_absolute());
    assert_eq!(
        tools.writer(&local(&[Native::Wayland], false), Some(&fakes.dir.root)),
        "~/wl-copy"
    );
    assert_eq!(
        tools.writer(&local(&[Native::Wayland], false), fakes.dir.root.parent()),
        format!(
            "~/{}/wl-copy",
            fakes
                .dir
                .root
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    );
    let tmux = Fakes::new("summary-tmux", &["tmux"]);
    assert_eq!(
        tmux.tools().writer(&local(&[], true), Some(&tmux.dir.root)),
        "~/tmux (load-buffer)"
    );
}

#[test]
fn copy_once_follows_the_route_and_stops_with_its_running() {
    let link = "https://claude.ai/oauth/authorize?state=LINK_CANARY";
    let running = Running::default();
    let mut written = Vec::new();
    let terminal = Clipboard {
        route: Route::Terminal,
        ..Clipboard::none()
    };
    assert_eq!(
        copy_once(&terminal, link, &mut written, &running),
        Outcome::Sent
    );
    assert_eq!(written, Osc52::new(link).unwrap().bytes());
    let long = "x".repeat(OSC52_MAX + 1);
    written.clear();
    assert_eq!(
        copy_once(&terminal, &long, &mut written, &running),
        Outcome::TooLarge
    );
    let blocked = Clipboard {
        route: Route::Blocked(SCREEN_BLOCKS),
        ..Clipboard::none()
    };
    let failed = Outcome::Failed(SCREEN_BLOCKS.to_owned());
    assert_eq!(copy_once(&blocked, link, &mut written, &running), failed);
    let nowhere = copy_once(&Clipboard::none(), link, &mut written, &running);
    assert_eq!(link_words(&nowhere), LINK_NOT_COPIED);
    let fakes = Fakes::new("copy-once", &["wl-copy"]);
    let local = Clipboard {
        route: local(&[Native::Wayland], false),
        tools: fakes.tools(),
    };
    let copied = Outcome::Copied(vec![Destination::Native(Native::Wayland)]);
    assert_eq!(copy_once(&local, link, &mut written, &running), copied);
    assert_eq!(fakes.read("wl-copy.in"), link.repeat(2));
    assert!(written.is_empty());
    let windows = Fakes::new("copy-once-wsl", &["powershell.exe"]);
    let wsl = Clipboard {
        route: route(&host(&[
            ("WSL_DISTRO_NAME", "Ubuntu"),
            ("TERM_PROGRAM", "vscode"),
            ("TERM_PROGRAM_VERSION", "1.146.0"),
        ])),
        tools: windows.tools(),
    };
    let code = "你好 🦀\r\nline\nlast";
    let boundary = "é".repeat(OSC52_MAX / 2);
    let copied = Outcome::Copied(vec![Destination::Native(Native::Windows)]);
    for text in [code, &boundary, &long] {
        assert_eq!(copy_once(&wsl, text, &mut written, &running), copied);
        assert!(written.is_empty());
    }
    assert_eq!(windows.args(POWERSHELL).len(), 3);
    assert_eq!(
        windows.read("powershell.exe.in"),
        format!("{code}{boundary}{long}")
    );
    running.stop();
    let stopped = copy_once(&local, link, &mut written, &running);
    assert!(matches!(stopped, Outcome::Failed(_)), "{stopped:?}");
    assert_eq!(fakes.read("wl-copy.in"), link.repeat(2));
}
