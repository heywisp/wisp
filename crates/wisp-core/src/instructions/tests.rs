use std::time::Duration;

use super::*;

fn within_a_second<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(1))
        .expect("finished within a second")
}

#[test]
fn the_extra_names_match_the_settings_choice() {
    let names: Vec<&str> = std::iter::once("none")
        .chain(Extra::ALL.map(Extra::name))
        .collect();
    assert_eq!(names, wisp_protocol::settings::INSTRUCTION_EXTRAS);
    assert_eq!(Extra::named("autonomous"), Some(Extra::Autonomous));
    assert_eq!(Extra::named("none"), None);
    assert_eq!(Extra::named("someday"), None);
    let block = Extra::Autonomous.block();
    assert!(block.starts_with(
        "<extra_instructions name=\"autonomous\">\nYou are working on a task on your own."
    ));
    assert!(block.ends_with("against what you built.\n</extra_instructions>"));
}

#[test]
fn large_instructions_are_priced_at_the_one_hour_write() {
    let priced = |id: &'static str| (id, *crate::pricing::model_info(id).unwrap());
    let (opus, gpt) = (priced("claude-opus-5-5"), priced("gpt-6.1-sol"));
    let opus = Some((opus.0, &opus.1));
    assert_eq!(
        large_message(20_000, opus).message,
        "instructions are large: about 20,000 tokens, ~$0.004 per request to read and ~$0.16 per cache write (claude-opus-5-5)"
    );
    assert_eq!(
        large_message(2_000, opus).message,
        "instructions are large: about 2,000 tokens, ~$0.0004 per request to read and ~$0.016 per cache write (claude-opus-5-5)"
    );
    assert_eq!(
        large_message(200, opus).message,
        "instructions are large: about 200 tokens, ~$0.00004 per request to read and ~$0.0016 per cache write (claude-opus-5-5)"
    );
    assert_eq!(
        large_message(20_000, Some((gpt.0, &gpt.1))).message,
        "instructions are large: about 20,000 tokens, ~$0.002 per request to read and ~$0.04 per cache write (gpt-6.1-sol)"
    );
    assert!(large_warning(32_000, 8_000, opus).is_none());
    assert!(large_warning(32_004, 8_000, opus).is_some());
    let large = large_warning(80_000, 100, None).unwrap();
    assert_eq!(large.kind, DiagnosticKind::Large);
    assert_eq!(large.source, None);
    assert_eq!(large.message, "instructions are large: about 20,000 tokens");
}

#[test]
fn tokens_parse_with_units() {
    assert_eq!(parse_tokens("8000"), Some(8_000));
    assert_eq!(parse_tokens("8k"), Some(8_000));
    assert_eq!(parse_tokens(" 1M "), Some(1_000_000));
    for bad in ["", "-1", "k", "1.5k", "8 k", "x"] {
        assert_eq!(parse_tokens(bad), None, "{bad}");
    }
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(thousands(999), "999");
}

#[test]
fn the_shared_crate_hashes_as_the_session_does() {
    for text in [
        "",
        "No comments.\n",
        "caf\u{e9} \u{2603}\n",
        &"x".repeat(100_000),
    ] {
        assert_eq!(
            wisp_instructions::sha256_hex(text),
            crate::session::sha256_hex(text)
        );
    }
}

#[cfg(unix)]
#[test]
fn the_disk_never_blocks_on_a_fifo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("AGENTS.md");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let read = within_a_second(move || Disk.read(&path).unwrap());
    assert_eq!(read, None);
    let file = dir.path().join("plain.md");
    std::fs::write(&file, "plain\n").unwrap();
    assert_eq!(Disk.read(&file).unwrap(), Some(b"plain\n".to_vec()));
    assert_eq!(Disk.metadata(&file).unwrap().kind, Kind::File);
    assert_eq!(Disk.metadata(dir.path()).unwrap().kind, Kind::Directory);
}
