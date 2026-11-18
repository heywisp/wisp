use std::path::Path;

use wisp_plugin_api::host::{self, JobEnd};
use wisp_plugin_api::types::ExitStatus;
use wisp_protocol::{LostReason, ProcessEnd, StoppedBy};

use super::job_end;

#[test]
fn every_process_end_maps_to_its_job_end() {
    let table = [
        (
            ProcessEnd::Exited {
                exit: wisp_protocol::ExitStatus::Code { code: 2 },
            },
            JobEnd::Exited(ExitStatus::Code(2)),
        ),
        (
            ProcessEnd::Exited {
                exit: wisp_protocol::ExitStatus::Signal { signal: 15 },
            },
            JobEnd::Exited(ExitStatus::Signal(15)),
        ),
        (
            ProcessEnd::Stopped {
                by: StoppedBy::Model,
            },
            JobEnd::Stopped(host::StoppedBy::Model),
        ),
        (
            ProcessEnd::Stopped {
                by: StoppedBy::User,
            },
            JobEnd::Stopped(host::StoppedBy::User),
        ),
        (
            ProcessEnd::Stopped {
                by: StoppedBy::SessionEnded,
            },
            JobEnd::Stopped(host::StoppedBy::SessionEnded),
        ),
        (
            ProcessEnd::Stopped {
                by: StoppedBy::TimeLimit { after_ms: 1500 },
            },
            JobEnd::Stopped(host::StoppedBy::TimeLimit(1500)),
        ),
        (
            ProcessEnd::Lost {
                reason: LostReason::StoppedAtResume,
            },
            JobEnd::Lost(host::LostReason::StoppedAtResume),
        ),
        (
            ProcessEnd::Lost {
                reason: LostReason::Gone,
            },
            JobEnd::Lost(host::LostReason::Gone),
        ),
        (
            ProcessEnd::Failed {
                message: "no exec".into(),
            },
            JobEnd::Failed("no exec".into()),
        ),
    ];
    for (end, mirrored) in table {
        assert_eq!(job_end(&end), mirrored, "{end:?}");
    }
}

#[test]
fn the_tool_text_copy_of_thousands_matches_the_protocols() {
    for n in [0, 7, 999, 1_000, 36_000, 999_999, 1_234_567, u64::MAX] {
        assert_eq!(
            wisp_tool_text::output::group(n),
            wisp_protocol::settings::thousands(n),
            "{n}"
        );
    }
}

#[test]
fn the_tool_text_copy_of_and_list_matches_the_protocols_listed() {
    let table: &[(&[&str], &str)] = &[
        (&[], ""),
        (&["a"], "a"),
        (&["a", "b"], "a and b"),
        (&["a", "b", "c"], "a, b and c"),
        (&["a", "b", "c", "d"], "a, b, c and d"),
    ];
    for &(items, shown) in table {
        let owned: Vec<String> = items.iter().map(|&item| item.to_owned()).collect();
        assert_eq!(wisp_tool_text::output::and_list(&owned), shown, "{items:?}");
        assert_eq!(
            wisp_protocol::settings::listed(items, "and"),
            shown,
            "{items:?}"
        );
    }
}

#[test]
fn the_tool_text_copy_of_shorten_path_matches_the_protocols() {
    let table: &[(&str, Option<&str>)] = &[
        ("/home/al", Some("/home/al")),
        ("/home/al/", Some("/home/al")),
        ("/home/al/Projects/Wisp", Some("/home/al")),
        ("/home/al/x", Some("/home/al/")),
        ("/home/al/x", Some("/home//al")),
        ("/home/al/x", Some("/home/./al")),
        ("/home//al/x", Some("/home/al")),
        ("/home/alice/x", Some("/home/al")),
        ("/tmp/wisp-1", Some("/home/al")),
        ("/home/al/x", None),
        ("/x", Some("/")),
        ("/home/al/x", Some("//")),
        ("/home/al/x", Some("")),
        ("rel/x", Some("rel")),
    ];
    for &(path, home) in table {
        let (path, home) = (Path::new(path), home.map(Path::new));
        assert_eq!(
            wisp_tool_text::paths::shorten_path(path, home),
            wisp_protocol::shorten_path(path, home),
            "{path:?} under {home:?}"
        );
    }
}
