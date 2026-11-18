#![allow(clippy::unwrap_used, reason = "tests fail on a missing value")]

use super::*;

fn streams(stdin_tty: bool, stdout_tty: bool) -> Streams {
    Streams {
        stdin_tty,
        stdout_tty,
        stderr_tty: stdout_tty,
    }
}

const REQUESTS: [Requested; 3] = [Requested::Auto, Requested::Tui, Requested::Jsonl];

fn expected(requested: Requested, stdin: bool, stdout: bool, dumb: bool) -> Option<Presenter> {
    let terminal = stdin && stdout;
    Some(match requested {
        Requested::Jsonl => Presenter::Jsonl,
        Requested::Auto if terminal && !dumb => Presenter::Tui,
        Requested::Auto => return None,
        Requested::Tui if terminal => Presenter::Tui,
        Requested::Tui => return None,
    })
}

#[test]
fn every_combination_resolves_as_the_plan_says() {
    let mut checked = 0;
    for requested in REQUESTS {
        for stdin in [false, true] {
            for stdout in [false, true] {
                for dumb in [false, true] {
                    let env =
                        Env::from_pairs([("TERM", if dumb { "dumb" } else { "xterm-256color" })]);
                    let got = resolve(requested, &env, streams(stdin, stdout));
                    let want = expected(requested, stdin, stdout, dumb);
                    let case = (requested, stdin, stdout, dumb);
                    assert_eq!(got.as_ref().ok().map(|c| c.presenter), want, "{case:?}");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 24);
}

#[test]
fn auto_refuses_anything_but_a_real_terminal() {
    let term = Env::from_pairs([("TERM", "xterm-256color")]);
    let dumb = Env::from_pairs([("TERM", "dumb")]);
    let auto = |env: &Env, s: Streams| resolve(Requested::Auto, env, s);
    assert_eq!(
        auto(&term, streams(true, true)).unwrap().describe(),
        "tui (stdin and stdout are terminals)"
    );
    assert_eq!(
        auto(&term, streams(true, false)),
        Err(NEEDS_TERMINAL.to_owned())
    );
    assert_eq!(
        auto(&term, streams(false, true)),
        Err(NEEDS_TERMINAL.to_owned())
    );
    assert_eq!(
        auto(&dumb, streams(false, false)),
        Err(NEEDS_TERMINAL.to_owned())
    );
    assert_eq!(
        auto(&dumb, streams(true, true)),
        Err(DUMB_TERMINAL.to_owned())
    );
}

#[test]
fn an_explicit_tui_forces_a_dumb_terminal_but_still_needs_one() {
    let dumb = Env::from_pairs([("TERM", "dumb")]);
    let forced = resolve(Requested::Tui, &dumb, streams(true, true)).unwrap();
    assert_eq!(forced.describe(), "tui (--output tui, forced on TERM=dumb)");
    let err = resolve(Requested::Tui, &dumb, streams(true, false)).unwrap_err();
    assert_eq!(
        err,
        "--output tui needs a terminal, but stdout is not a terminal; use --output jsonl for scripts and pipes"
    );
}

#[test]
fn jsonl_works_anywhere() {
    let dumb = Env::from_pairs([("TERM", "dumb")]);
    for (stdin, stdout) in [(false, false), (true, false), (false, true), (true, true)] {
        let chosen = resolve(Requested::Jsonl, &dumb, streams(stdin, stdout)).unwrap();
        assert_eq!(chosen.describe(), "jsonl (--output jsonl)");
    }
}
