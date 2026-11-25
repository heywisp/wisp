use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::executor::block_on;
use serde_json::json;
use wisp_plugin_api::host::FileKind;
use wisp_plugin_testkit::host::{CountFn, FakeCall, FakeHost, Flag};

use super::*;

mod media;
mod no_bash;
mod paths;
mod text;
mod worker;

const MEDIA_TYPES: [&str; 9] = [
    "application/pdf",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
];

struct Rig {
    _dir: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    edge: Option<u32>,
    count: Option<Arc<CountFn>>,
    cancel: Flag,
    bash: bool,
    media_types: &'static [&'static str],
    media_slices: Vec<SliceKind>,
}

struct Captured {
    bytes: u64,
    lines: u64,
    tail: String,
}

struct Viewed {
    run: ToolRun,
    captured: Captured,
}

impl Viewed {
    fn result(&self) -> &str {
        self.run.result.as_deref().unwrap_or_default()
    }

    fn refused(&self) -> &str {
        assert_eq!(
            self.run.outcome,
            Outcome::Completed(Completed {
                is_error: true,
                exit: None
            }),
            "{}",
            self.result()
        );
        assert!(self.run.attachments.is_empty());
        assert!(self.run.facts.is_empty(), "{:?}", self.run.facts);
        self.result()
    }

    fn completed(&self) -> &str {
        assert_eq!(
            self.run.outcome,
            Outcome::Completed(Completed {
                is_error: false,
                exit: None
            }),
            "{}",
            self.result()
        );
        self.result()
    }
}

pub(crate) fn kind_of(path: &str) -> Option<FileKind> {
    let kind = fs::metadata(path).ok()?.file_type();
    Some(if kind.is_file() {
        FileKind::Regular
    } else if kind.is_dir() {
        FileKind::Directory
    } else if kind.is_fifo() {
        FileKind::Fifo
    } else if kind.is_socket() {
        FileKind::Socket
    } else if kind.is_block_device() || kind.is_char_device() {
        FileKind::Device
    } else {
        FileKind::Other
    })
}

pub(crate) fn tokio_sleep(ms: u64) -> BoxFuture<'static, ()> {
    Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cwd = home.join("work");
        fs::create_dir_all(&cwd).unwrap();
        Self {
            _dir: dir,
            home,
            cwd,
            edge: Some(wisp_media::MAX_LONG_EDGE),
            count: None,
            cancel: Flag::default(),
            bash: true,
            media_types: &MEDIA_TYPES,
            media_slices: Vec::new(),
        }
    }

    fn host(&self) -> FakeHost {
        FakeHost {
            root: self.cwd.display().to_string(),
            home: Some(self.home.display().to_string()),
            capabilities: if self.bash {
                vec![SHELL.to_owned()]
            } else {
                Vec::new()
            },
            media_types: self
                .media_types
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            media_slices: self.media_slices.clone(),
            image_edge: self.edge,
            count: self.count.clone(),
            sleep: Some(Arc::new(tokio_sleep)),
            kind_of: Some(Arc::new(kind_of)),
            elapsed: None,
            settings: Vec::new(),
            ..FakeHost::default()
        }
    }

    fn tool(&self) -> ViewFile {
        let host = self.host();
        let context = BuildContext {
            platform: "unix".to_owned(),
            capabilities: host.capabilities.clone(),
        };
        let tool = ViewFile::new(Arc::new(host));
        let described = futures::executor::block_on(tool.describe(&context));
        assert_eq!(described, [def(self.bash)]);
        tool
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.cwd.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        path
    }

    fn sized(&self, name: &str, start: &[u8], len: u64) -> PathBuf {
        let path = self.file(name, start);
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(len)
            .unwrap();
        path
    }

    async fn view(&self, path: &str) -> Viewed {
        self.view_from(&self.cwd, path).await
    }

    async fn view_from(&self, cwd: &Path, path: &str) -> Viewed {
        self.view_with(cwd, json!({"path": path})).await
    }

    async fn view_with(&self, cwd: &Path, input: Value) -> Viewed {
        let tool = self.tool();
        let prepared = tool.prepare(NAME, &input.to_string()).await.unwrap();
        let call = FakeCall {
            id: "v".to_owned(),
            cwd: cwd.display().to_string(),
            cancel: self.cancel.clone(),
            ..FakeCall::default()
        };
        let run = tokio::time::timeout(Duration::from_secs(20), tool.run(prepared, &call))
            .await
            .unwrap();
        let written = call.written();
        let text = String::from_utf8_lossy(&written).into_owned();
        let captured = Captured {
            bytes: written.len() as u64,
            lines: written.iter().filter(|b| **b == b'\n').count() as u64,
            tail: text,
        };
        Viewed { run, captured }
    }
}

type Asked = Arc<Mutex<Vec<(String, Vec<u8>, Option<FileSlice>)>>>;

fn counting(answer: TokenCount) -> (Option<Arc<CountFn>>, Asked) {
    let asked: Asked = Arc::default();
    let seen = asked.clone();
    let count: Arc<CountFn> = Arc::new(move |media_type, bytes, slice| {
        seen.lock().unwrap().push((media_type, bytes, slice));
        Box::pin(std::future::ready(answer.clone()))
    });
    (Some(count), asked)
}

fn png(width: u32, height: u32) -> Vec<u8> {
    encoded(width, height, image::ImageFormat::Png)
}

fn encoded(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
    let picture = image::RgbImage::from_pixel(width, height, image::Rgb([10, 120, 200]));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(picture)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn the_spec_requires_only_path_and_describes_optional_slices() {
    let def = def(false);
    assert_eq!(def.name, "view_file");
    let ToolKind::Client(client) = def.kind else {
        panic!("view_file is a client tool");
    };
    let schema: Value = serde_json::from_str(&client.input_schema).unwrap();
    assert_eq!(schema["required"], json!(["path"]));
    assert_eq!(schema["additionalProperties"], false);
    let properties = schema["properties"].as_object().unwrap();
    assert_eq!(properties["path"]["type"], "string");
    assert_eq!(
        properties
            .keys()
            .map(String::as_str)
            .filter(|&key| key != "path")
            .collect::<BTreeSet<_>>(),
        SliceKind::ALL.iter().map(SliceKind::name).collect()
    );
    for kind in [SliceKind::Pages, SliceKind::Seconds] {
        assert_eq!(properties[kind.name()]["type"], "string", "{kind:?}");
    }
    let sheet = &properties[SliceKind::Sheet.name()];
    assert_eq!(sheet["type"], "integer");
    assert_eq!(sheet["minimum"], 1);
    assert!(!client.description.contains("bash"));
    assert!(!client.input_schema.contains("bash"));
}

#[test]
fn the_meta_saves_output_and_reads_in_parallel() {
    let meta = meta();
    assert!(meta.saves_output);
    assert!(meta.parallel_safe);
    assert_eq!(meta.effect, Effect::ReadOnly);
    assert!(meta.tags.reads_untrusted);
    assert_eq!(meta.purpose, Purpose::Run);
    assert_eq!(meta.writing.as_deref(), Some("a file path"));
}

#[test]
fn input_requires_a_path_and_shows_it_as_written() {
    let tool = ViewFile::new(Arc::new(FakeHost::default()));
    let error = |input: Value| {
        block_on(tool.prepare(NAME, &input.to_string()))
            .err()
            .unwrap()
    };
    assert_eq!(error(json!({})), "`path` must be a string");
    assert_eq!(error(json!({"path": 3})), "`path` must be a string");
    assert_eq!(error(json!({"path": "  "})), "`path` is empty");
    assert_eq!(
        error(json!({"path": "a\u{0}b"})),
        "`path` contains a NUL byte"
    );
    assert_eq!(
        error(json!({"path": "x".repeat(MAX_PATH_BYTES + 1)})),
        "`path` is 4,097 bytes; the limit is 4,096 bytes"
    );
    assert!(
        block_on(tool.prepare(
            NAME,
            &json!({"path": "x".repeat(MAX_PATH_BYTES)}).to_string()
        ))
        .is_ok()
    );
    assert_eq!(error(json!("a.txt")), "input must be an object");
    let prepared =
        block_on(tool.prepare(NAME, &json!({"path": "~/shots/a.png"}).to_string())).unwrap();
    assert_eq!(
        prepared.display(),
        Display {
            action: Action::Read,
            verbs: None,
            target: "~/shots/a.png".to_owned(),
            output: OutputKind::None,
            view: None,
            title: None,
        }
    );
}

#[tokio::test]
async fn a_stopped_view_attaches_nothing() {
    let rig = Rig::new();
    rig.file("a.png", &png(4, 4));
    rig.cancel.set();
    let viewed = rig.view("a.png").await;
    assert_eq!(viewed.run.outcome, Outcome::Cancelled);
    assert_eq!(viewed.result(), STOPPED);
    assert!(viewed.run.attachments.is_empty());
}
