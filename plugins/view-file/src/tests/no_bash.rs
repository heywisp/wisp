use super::*;

fn without_bash() -> Rig {
    let mut rig = Rig::new();
    rig.bash = false;
    rig
}

#[test]
fn the_description_names_bash_only_when_bash_is_on() {
    let described = |capabilities: Vec<String>| {
        let tool = ViewFile::new(Arc::new(FakeHost::default()));
        let context = BuildContext {
            platform: "unix".to_owned(),
            capabilities,
        };
        futures::executor::block_on(tool.describe(&context))
    };
    let alone = described(Vec::new());
    assert_eq!(alone, [def(false)]);
    let alone = description(false);
    assert!(!alone.contains("bash"), "{alone}");
    assert!(alone.contains("only the start of the file"));
    assert_eq!(described(vec![SHELL.to_owned()]), [def(true)]);
    assert_eq!(described(vec!["other".to_owned()]), [def(false)]);
    for absent in ["/home", "claude", "image/png", "2026"] {
        assert!(!alone.contains(absent), "{absent}");
    }
}

#[tokio::test]
async fn a_long_file_says_only_this_part_can_be_viewed() {
    let rig = without_bash();
    let body: String = (1..=2_500).map(|n| format!("line {n}\n")).collect();
    rig.file("big.txt", body.as_bytes());
    let viewed = rig.view("big.txt").await;
    let result = viewed.completed();
    assert!(
        result.ends_with(
            "line 2000\n[2,500 lines in all; only this part can be viewed in this session]"
        ),
        "{result}"
    );
    assert!(!result.contains("bash"));
}

#[tokio::test]
async fn refusals_name_no_bash() {
    let mut rig = without_bash();
    fs::create_dir_all(rig.cwd.join("logs")).unwrap();
    assert_eq!(
        rig.view("logs").await.refused(),
        "logs (resolved to ~/work/logs) is a directory, not a file."
    );
    rig.file("wide.png", &png(2_001, 1));
    assert_eq!(
        rig.view("wide.png").await.refused(),
        "wide.png is 2001\u{d7}1; images over 2,000 px a side can't be attached, so it can't be \
viewed in this session."
    );
    let mut start = png(2, 2);
    start.truncate(64);
    rig.sized("huge.png", &start, 8_000_000);
    assert_eq!(
        rig.view("huge.png").await.refused(),
        "huge.png is 8.0 MB; files over 7.5 MB can't be attached, so it can't be read in this \
session."
    );
    rig.file("a.zip", b"PK\x03\x04\x14\x00\x00\x00\x08\x00");
    let zip = rig.view("a.zip").await;
    assert_eq!(
        zip.refused(),
        wisp_tool_text::files::uncarried("a.zip", "application/zip", &MEDIA_TYPES, None)
    );
    assert!(
        zip.refused()
            .ends_with("so it can't be read in this session.")
    );
    rig.file(
        "spec.pdf",
        b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n",
    );
    rig.count = counting(TokenCount::Counted(182_000)).0;
    assert_eq!(
        rig.view("spec.pdf").await.refused(),
        "spec.pdf would add about 182,000 tokens (the server's estimate), more than one view \
may add, so it can't be viewed in this session."
    );
}
