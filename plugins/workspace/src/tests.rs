use futures::executor::block_on;
use wisp_plugin_api::manifest::{Manifest, Origin, SectionPlace};
use wisp_plugin_testkit::host::FakeHost;

use super::*;

fn facts(cwd: &str, project_root: Option<&str>, platform: &str) -> EnvironmentFacts {
    EnvironmentFacts {
        cwd: cwd.to_owned(),
        project_root: project_root.map(str::to_owned),
        platform: platform.to_owned(),
        resources: None,
        tool_lines: Vec::new(),
        agent_lines: Vec::new(),
    }
}

fn owned(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| (*l).to_owned()).collect()
}

#[test]
fn the_environment_block_lists_the_loop_lines_then_each_tool_line() {
    let shell = EnvironmentFacts {
        tool_lines: owned(&["Shell: x"]),
        ..facts("~/p", None, "Linux")
    };
    assert_eq!(
        environment_block(&shell),
        "<environment>\nWorking directory: ~/p\nPlatform: Linux\nShell: x\n</environment>\n"
    );
    assert_eq!(
        environment_block(&facts("~", None, "Linux")),
        "<environment>\nWorking directory: ~\nPlatform: Linux\n</environment>\n"
    );
    assert_eq!(
        environment_block(&facts("~/p/crates", Some("~/p"), "Linux")),
        "<environment>\nWorking directory: ~/p/crates\nProject root: ~/p\nPlatform: Linux\n</environment>\n"
    );
}

#[test]
fn the_resources_line_follows_the_platform() {
    let all = EnvironmentFacts {
        resources: Some("Resources: 1 CPU, 2 GiB memory".to_owned()),
        tool_lines: owned(&["Shell: x"]),
        ..facts("~/p", Some("~"), "Linux (container)")
    };
    assert_eq!(
        environment_block(&all),
        "<environment>\nWorking directory: ~/p\nProject root: ~\nPlatform: Linux (container)\nResources: 1 CPU, 2 GiB memory\nShell: x\n</environment>\n"
    );
}

#[test]
fn the_agent_lines_follow_the_tool_lines() {
    let child = EnvironmentFacts {
        tool_lines: owned(&["Shell: x"]),
        agent_lines: owned(&["Agent: /a", "Parent: main"]),
        ..facts("~/p", None, "Linux")
    };
    assert_eq!(
        environment_block(&child),
        "<environment>\nWorking directory: ~/p\nPlatform: Linux\nShell: x\nAgent: /a\nParent: main\n</environment>\n"
    );
}

#[test]
fn the_environment_renders_once_per_window_and_again_when_the_agent_lines_change() {
    let agent = EnvironmentFacts {
        agent_lines: owned(&["Agent: /a", "Parent: main"]),
        ..facts("~/p", None, "Linux")
    };
    let text = environment_block(&agent);
    let sent = |previous: &Prior| render(Some(agent.clone()), previous);
    assert_eq!(
        sent(&Prior::None),
        Rendered {
            snapshot: text.clone(),
            text: Some(text.clone())
        }
    );
    assert_eq!(sent(&Prior::Snapshot(text.clone())).text, None);
    assert_eq!(sent(&Prior::Snapshot(text.clone())).snapshot, text);
    assert_eq!(sent(&Prior::Legacy(Some(text.clone()))).text, None);
    assert_eq!(sent(&Prior::Legacy(None)).text, None);

    let edited = EnvironmentFacts {
        agent_lines: owned(&[
            "Agent: /a",
            "Parent: main",
            "Presets for spawn_agent: rev (Reviews)",
        ]),
        ..agent.clone()
    };
    let now = environment_block(&edited);
    let resent = |previous: &Prior| render(Some(edited.clone()), previous).text;
    assert_eq!(resent(&Prior::Snapshot(text.clone())), Some(now.clone()));
    assert_eq!(resent(&Prior::Legacy(Some(text))), Some(now.clone()));
    assert_eq!(resent(&Prior::Snapshot(now)), None);
}

#[test]
fn a_change_outside_the_agent_lines_sends_nothing_within_a_window() {
    let before = environment_block(&facts("~/p", None, "Linux"));
    let moved = facts("~/q", Some("~/p"), "Linux");
    assert_eq!(render(Some(moved), &Prior::Snapshot(before)).text, None);
}

#[test]
fn no_facts_render_no_text() {
    for previous in [Prior::None, Prior::Snapshot("x".to_owned())] {
        assert_eq!(render(None, &previous), Rendered::default());
    }
}

#[test]
fn the_section_reads_one_environment_call_from_the_host() {
    let host = FakeHost {
        environment: Some(facts("~/p", None, "Linux")),
        ..FakeHost::default()
    };
    let plugin = instantiate(Arc::new(host));
    let rendered = block_on(plugin.render_section(SECTION, At::Turn, Prior::None, None));
    assert_eq!(
        rendered.text.as_deref(),
        Some("<environment>\nWorking directory: ~/p\nPlatform: Linux\n</environment>\n")
    );
    let other = block_on(plugin.render_section("other", At::Turn, Prior::None, None));
    assert_eq!(other, Rendered::default());
    let bare = instantiate(Arc::new(FakeHost::default()));
    let none = block_on(bare.render_section(SECTION, At::Head, Prior::None, None));
    assert_eq!(none, Rendered::default());
}

#[test]
fn the_manifest_continues_the_core_environment_in_the_state_set() {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, "workspace");
    assert!(manifest.system.is_none());
    let [section, terminal] = manifest.prompt_sections.as_slice() else {
        panic!("{:?}", manifest.prompt_sections);
    };
    assert_eq!(section.name, SECTION);
    assert_eq!(section.place, SectionPlace::Lead);
    assert_eq!(section.sets, ["core/state"]);
    assert_eq!(section.continues.as_deref(), Some("core/environment"));
    assert_eq!(terminal.name, TERMINAL);
    assert_eq!(terminal.place, SectionPlace::Lead);
    assert_eq!(terminal.sets, ["core/state"]);
    assert_eq!(terminal.after, ["workspace/environment"]);
    assert_eq!(terminal.continues, None);
}

const WIDE: Size = Size {
    cols: 121,
    rows: 40,
};

#[test]
fn the_terminal_line_names_the_reply_width_and_the_rows() {
    assert_eq!(
        terminal_line(WIDE),
        "Terminal: replies are 121 columns wide (wider code blocks wrap), 40 rows; don't mention this.\n"
    );
}

#[test]
fn the_terminal_renders_when_unseen_or_changed_and_never_when_the_same() {
    let line = terminal_line(WIDE);
    let seen = |previous: &Prior| render_terminal(Some(WIDE), previous);
    assert_eq!(
        seen(&Prior::None),
        Rendered {
            snapshot: "121x40".to_owned(),
            text: Some(line.clone())
        }
    );
    assert_eq!(
        seen(&Prior::Snapshot("121x40".to_owned())),
        Rendered {
            snapshot: "121x40".to_owned(),
            text: None
        }
    );
    assert_eq!(seen(&Prior::Snapshot("80x24".to_owned())).text, Some(line));
    let resized = Size { cols: 80, rows: 24 };
    assert_eq!(
        render_terminal(Some(resized), &Prior::Snapshot("121x40".to_owned())),
        Rendered {
            snapshot: "80x24".to_owned(),
            text: Some(terminal_line(resized))
        }
    );
}

#[test]
fn no_size_sends_nothing_and_keeps_what_was_seen() {
    assert_eq!(
        render_terminal(None, &Prior::Snapshot("121x40".to_owned())),
        Rendered {
            snapshot: "121x40".to_owned(),
            text: None
        }
    );
    assert_eq!(render_terminal(None, &Prior::None), Rendered::default());
}

#[test]
fn a_head_carries_the_terminal_line_once() {
    let host = FakeHost {
        terminal: Some(WIDE),
        ..FakeHost::default()
    };
    let plugin = instantiate(Arc::new(host));
    let head = block_on(plugin.render_section(TERMINAL, At::Head, Prior::None, None));
    assert_eq!(head.text, Some(terminal_line(WIDE)));
    let next =
        block_on(plugin.render_section(TERMINAL, At::Turn, Prior::Snapshot(head.snapshot), None));
    assert_eq!(next.text, None);
    let bare = instantiate(Arc::new(FakeHost::default()));
    let none = block_on(bare.render_section(TERMINAL, At::Turn, Prior::None, None));
    assert_eq!(none, Rendered::default());
}
