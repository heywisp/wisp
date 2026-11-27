use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

use super::super::tests::{assert_unused, read_file, recorded_args, wslg_host};
use super::*;
use crate::scratch::ScratchDir;
use crate::term::caps::Env;
use crate::term::clipboard::fake_tool::install;

const HIGH: ImageTier = ImageTier {
    max_edge: 2576,
    max_tokens: 4784,
};

const RECORD: &str = r#"#!/bin/sh
d="$(dirname "$0")"
n="$(basename "$0")"
for a in "$@"; do printf '%s\n' "$a" >> "$d/$n.args"; done
printf -- '--\n' >> "$d/$n.args"
if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then echo tty >> "$d/tty"; fi
/bin/cat >> "$d/$n.in"
if [ -f "$d/$n.hang" ]; then /bin/sleep 30; fi
if [ "$1" = "--list-types" ] || [ "$4" = "TARGETS" ]; then
  if [ -f "$d/$n.types" ]; then /bin/cat "$d/$n.types"; exit 0; fi
  exit 1
fi
if [ -f "$d/$n.flood" ]; then /usr/bin/head -c "$(/bin/cat "$d/$n.flood")" /dev/zero; exit 0; fi
if [ -f "$d/$n.out" ]; then /bin/cat "$d/$n.out"; fi
if [ -f "$d/$n.fail" ]; then exit 3; fi
exit 0
"#;

struct Fakes {
    dir: ScratchDir,
}

impl Fakes {
    fn new(name: &str, tools: &[&str]) -> Fakes {
        let dir = ScratchDir::new(crate::SCRATCH, name);
        for tool in tools {
            install(&dir.root.join(tool), RECORD);
        }
        Fakes { dir }
    }

    fn setup(&self, natives: &[Native]) -> Setup {
        Setup {
            ssh: false,
            wsl: natives.contains(&Native::Windows),
            natives: natives.to_vec(),
            tools: Tools {
                path: Some(self.dir.root.clone().into_os_string()),
                powershell: vec![],
                setsid: super::super::system_setsid(),
                timeout: Duration::from_secs(10),
            },
            home: Some(self.dir.root.clone()),
        }
    }

    fn put(&self, file: &str, body: &[u8]) {
        std::fs::write(self.dir.root.join(file), body).unwrap();
    }

    fn read(&self, file: &str) -> String {
        read_file(&self.dir.root, file)
    }

    fn args(&self, tool: &str) -> Vec<Vec<String>> {
        recorded_args(&self.dir.root, tool)
    }
}

fn bmp(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let row = (width * 3).div_ceil(4) * 4;
    let pixels = row * height;
    let mut out = b"BM".to_vec();
    out.extend((54 + pixels).to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend(width.to_le_bytes());
    out.extend(height.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(24u16.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(pixels.to_le_bytes());
    out.extend(2835u32.to_le_bytes());
    out.extend(2835u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    for y in 0..height {
        for x in 0..row {
            out.push(seed.wrapping_add(u8::try_from((x + y) % 251).unwrap()));
        }
    }
    out
}

fn ran(setup: &Setup) -> Result<Found, Miss> {
    read_clipboard(setup, &Running::default())
}

#[test]
fn wayland_lists_types_then_reads_the_best_image_type() {
    let fakes = Fakes::new("wayland", &["wl-paste"]);
    fakes.put("wl-paste.types", b"text/plain\nimage/bmp\nimage/png\n");
    let image = bmp(3, 2, 7);
    fakes.put("wl-paste.out", &image);
    assert_eq!(
        ran(&fakes.setup(&[Native::Wayland])),
        Ok(Found::Image(image))
    );
    assert_eq!(
        fakes.args("wl-paste"),
        vec![
            vec!["--list-types".to_owned()],
            vec!["--type".to_owned(), "image/png".to_owned()],
        ]
    );
    assert!(fakes.read("tty").is_empty(), "a reader saw the tty");
    assert!(fakes.read("wl-paste.in").is_empty());
}

#[test]
fn x11_asks_xclip_for_targets_then_the_image() {
    let fakes = Fakes::new("x11", &["xclip"]);
    fakes.put("xclip.types", b"TARGETS\nimage/jpeg\nUTF8_STRING\n");
    fakes.put("xclip.out", b"jpeg bytes");
    assert_eq!(
        ran(&fakes.setup(&[Native::X11])),
        Ok(Found::Image(b"jpeg bytes".to_vec()))
    );
    let strs = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        fakes.args("xclip"),
        vec![
            strs(&["-selection", "clipboard", "-t", "TARGETS", "-o"]),
            strs(&["-selection", "clipboard", "-t", "image/jpeg", "-o"]),
        ]
    );
}

#[test]
fn wsl_runs_a_fixed_encoded_script_and_decodes_its_base64() {
    let fakes = Fakes::new("wsl", &["powershell.exe"]);
    let image = bmp(2, 2, 1);
    let mut out = b"\xEF\xBB\xBFimage:".to_vec();
    out.extend(wisp_media::encode_base64(&image).as_bytes());
    out.extend(b"\r\n");
    fakes.put("powershell.exe.out", &out);
    assert_eq!(
        ran(&fakes.setup(&[Native::Windows])),
        Ok(Found::Image(image))
    );
    let args = fakes.args("powershell.exe");
    assert_eq!(args.len(), 1);
    assert_eq!(
        args[0],
        vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Sta".to_owned(),
            "-EncodedCommand".to_owned(),
            encoded_read_script(),
        ]
    );
    let decoded = wisp_media::decode_base64(&encoded_read_script()).unwrap();
    let units: Vec<u16> = decoded
        .chunks(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    assert_eq!(String::from_utf16(&units).unwrap(), WSL_READ_SCRIPT);
    assert!(WSL_READ_SCRIPT.contains("ContainsImage()"));
    assert!(WSL_READ_SCRIPT.contains("ContainsFileDropList()"));
}

#[test]
fn a_wsl_file_drop_list_goes_through_wslpath() {
    let fakes = Fakes::new("wsl-files", &["powershell.exe", "wslpath"]);
    fakes.put(
        "powershell.exe.out",
        "file:C:\\Users\\me\\shot one.png\r\nfile:D:\\x.jpg\n".as_bytes(),
    );
    fakes.put("wslpath.out", b"/mnt/c/Users/me/shot one.png\n");
    assert_eq!(
        ran(&fakes.setup(&[Native::Windows])),
        Ok(Found::Files(vec![
            PathBuf::from("/mnt/c/Users/me/shot one.png"),
            PathBuf::from("/mnt/c/Users/me/shot one.png"),
        ]))
    );
    assert_eq!(
        fakes.args("wslpath"),
        vec![
            vec!["-u".to_owned(), "C:\\Users\\me\\shot one.png".to_owned()],
            vec!["-u".to_owned(), "D:\\x.jpg".to_owned()],
        ]
    );
}

#[test]
fn no_image_gives_its_sentence_after_every_platform_is_asked() {
    let fakes = Fakes::new("none", &["wl-paste", "xclip"]);
    fakes.put("wl-paste.types", b"text/plain\n");
    let got = ran(&fakes.setup(&[Native::Wayland, Native::X11]));
    assert_eq!(got, Err(Miss::NoImage));
    assert_eq!(got.unwrap_err().sentence(), "No image on the clipboard.");
    assert_eq!(fakes.args("wl-paste").len(), 1);
    assert_eq!(fakes.args("xclip").len(), 1);
}

#[test]
fn wslg_does_not_read_display_clipboards_when_powershell_is_missing() {
    let display_readers = [WAYLAND_READ.program, X11_READ.program];
    let fakes = Fakes::new("wslg-missing", &display_readers);
    let setup = Setup {
        tools: fakes.setup(&[]).tools,
        ..Setup::current(&wslg_host(&[]))
    };
    assert_eq!(
        ran(&setup),
        Err(Miss::Missing(vec!["powershell.exe wasn't found"]))
    );
    assert_eq!(text_of(&setup), Text::Empty);
    assert_unused(&fakes.dir.root, &display_readers, "wslg-missing");
}

#[test]
fn a_missing_wayland_reader_falls_back_to_x11() {
    let fakes = Fakes::new("fallback", &["xclip"]);
    fakes.put("xclip.types", b"image/bmp\n");
    fakes.put("xclip.out", b"BM...");
    assert_eq!(
        ran(&fakes.setup(&[Native::Wayland, Native::X11])),
        Ok(Found::Image(b"BM...".to_vec()))
    );
}

#[test]
fn a_missing_tool_names_what_to_install() {
    let fakes = Fakes::new("missing", &[]);
    for (natives, sentence) in [
        (
            vec![Native::Wayland],
            "Couldn't read the clipboard: install wl-clipboard.",
        ),
        (
            vec![Native::X11],
            "Couldn't read the clipboard: install xclip.",
        ),
        (
            vec![Native::Mac],
            "Couldn't read the clipboard: install pngpaste.",
        ),
        (
            vec![Native::Windows],
            "Couldn't read the clipboard: powershell.exe wasn't found.",
        ),
        (
            vec![Native::Wayland, Native::X11],
            "Couldn't read the clipboard: install wl-clipboard; install xclip.",
        ),
    ] {
        let got = ran(&fakes.setup(&natives)).unwrap_err();
        assert_eq!(got.sentence(), sentence, "{natives:?}");
    }
    assert_eq!(
        ran(&fakes.setup(&[])).unwrap_err().sentence(),
        copy::NO_CLIPBOARD
    );
}

#[test]
fn over_ssh_nothing_is_run_and_the_hint_names_paths() {
    let fakes = Fakes::new("ssh", &["wl-paste"]);
    let mut setup = fakes.setup(&[Native::Wayland]);
    setup.ssh = true;
    let got = ran(&setup).unwrap_err();
    assert_eq!(got, Miss::OverSsh);
    assert_eq!(
        got.sentence(),
        "Paste the image's path: no clipboard over SSH."
    );
    assert!(fakes.args("wl-paste").is_empty());
    let env = Env::from_pairs([("SSH_CONNECTION", "1 2 3 4"), ("WAYLAND_DISPLAY", "w")]);
    let host = Host::of(&env, Some("6.18-microsoft-standard-WSL2"), false);
    assert!(host.ssh && host.wsl);
    assert!(native_routes(&host).is_empty());
    assert!(Setup::current(&host).ssh);
}

#[test]
fn a_hanging_reader_is_killed_at_the_timeout_with_a_sentence() {
    let fakes = Fakes::new("hang", &["wl-paste"]);
    fakes.put("wl-paste.hang", b"");
    let mut setup = fakes.setup(&[Native::Wayland]);
    setup.tools.timeout = Duration::from_secs(1);
    let started = Instant::now();
    let got = ran(&setup).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        got.sentence(),
        "Couldn't read the clipboard: wl-paste took longer than 1s."
    );
}

#[test]
fn output_over_the_cap_is_refused_without_waiting_for_the_timeout() {
    let fakes = Fakes::new("flood", &["wl-paste"]);
    fakes.put("wl-paste.types", b"image/png\n");
    let mut setup = fakes.setup(&[Native::Wayland]);
    setup.tools.timeout = Duration::from_secs(60);
    fakes.put("wl-paste.flood", b"41000000");
    let started = Instant::now();
    let got = ran(&setup);
    assert_eq!(got, Err(Miss::TooLarge));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        Miss::TooLarge.sentence(),
        "The clipboard image is larger than 40 MB."
    );
}

#[test]
fn garbage_from_the_clipboard_is_refused_with_the_media_sentence() {
    let fakes = Fakes::new("garbage", &["wl-paste"]);
    fakes.put("wl-paste.types", b"image/png\n");
    fakes.put("wl-paste.out", b"definitely not a png");
    let setup = fakes.setup(&[Native::Wayland]);
    let got = perform(Job::Clipboard, HIGH, &setup, &Running::default());
    assert_eq!(
        got,
        Got::Failed(
            "Couldn't attach the clipboard image. This isn't a PNG, JPEG, GIF, WebP or BMP image."
                .to_owned()
        )
    );
}

#[test]
fn a_clipboard_image_is_prepared_with_the_media_crates_size() {
    let fakes = Fakes::new("prepared", &["wl-paste"]);
    fakes.put("wl-paste.types", b"image/bmp\n");
    let image = bmp(56, 28, 3);
    fakes.put("wl-paste.out", &image);
    let setup = fakes.setup(&[Native::Wayland]);
    let Got::Images { images, .. } = perform(Job::Clipboard, HIGH, &setup, &Running::default())
    else {
        panic!("no image");
    };
    assert_eq!(images.len(), 1);
    assert_eq!(
        (images[0].width, images[0].height, images[0].tokens),
        (56, 28, 2)
    );
    assert_eq!(&*images[0].data, wisp_media::encode_base64(&image));
}

#[test]
fn mac_reads_osascript_hex_then_pngpaste() {
    assert_eq!(
        mac_hex("\u{00AB}data PNGf89504E47\u{00BB}\n"),
        Some(vec![0x89, 0x50, 0x4E, 0x47])
    );
    assert_eq!(mac_hex("\u{00AB}data PNGf8\u{00BB}"), None);
    assert_eq!(mac_hex("\u{00AB}data TIFF00\u{00BB}"), None);
    assert_eq!(mac_hex("\u{00AB}data PNGfZZ\u{00BB}"), None);
    let fakes = Fakes::new("mac", &["osascript", "pngpaste"]);
    fakes.put("osascript.out", "\u{00AB}data PNGf0102\u{00BB}".as_bytes());
    assert_eq!(
        ran(&fakes.setup(&[Native::Mac])),
        Ok(Found::Image(vec![1, 2]))
    );
    assert_eq!(
        fakes.args("osascript"),
        vec![vec!["-e".to_owned(), MAC_SCRIPT.to_owned()]]
    );
    assert!(fakes.args("pngpaste").is_empty());
    fakes.put("osascript.fail", b"");
    fakes.put("osascript.out", b"");
    fakes.put("pngpaste.out", b"png");
    assert_eq!(
        ran(&fakes.setup(&[Native::Mac])),
        Ok(Found::Image(b"png".to_vec()))
    );
    assert_eq!(fakes.args("pngpaste"), vec![vec!["-".to_owned()]]);
}

#[test]
fn osascript_timeouts_and_floods_are_named_and_never_read_as_no_image() {
    let fakes = Fakes::new("mac-fail", &["osascript", "pngpaste"]);
    let mut setup = fakes.setup(&[Native::Mac]);
    setup.tools.timeout = Duration::from_secs(1);
    fakes.put("osascript.hang", b"");
    let started = Instant::now();
    let got = ran(&setup).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(
        got.sentence(),
        "Couldn't read the clipboard: osascript took longer than 1s."
    );
    std::fs::remove_file(fakes.dir.root.join("osascript.hang")).unwrap();
    setup.tools.timeout = Duration::from_secs(60);
    fakes.put("osascript.flood", (HEX_CAP + 1).to_string().as_bytes());
    let started = Instant::now();
    assert_eq!(ran(&setup), Err(Miss::TooLarge));
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(fakes.args("pngpaste").is_empty());
    std::fs::remove_file(fakes.dir.root.join("osascript.flood")).unwrap();
    fakes.put("osascript.fail", b"");
    assert_eq!(ran(&setup), Err(Miss::NoImage));
    assert_eq!(fakes.args("pngpaste"), vec![vec!["-".to_owned()]]);
}

fn unix(path: &str) -> Candidate {
    Candidate::Unix(PathBuf::from(path))
}

#[test]
fn whole_pastes_of_image_paths_are_recognised_in_every_form() {
    let home = Path::new("/home/me");
    let paths = |text: &str, wsl: bool| image_paths(text, Some(home), wsl);
    assert_eq!(paths("/tmp/a.png", false), Some(vec![unix("/tmp/a.png")]));
    assert_eq!(
        paths("  /tmp/a.PNG\n", false),
        Some(vec![unix("/tmp/a.PNG")])
    );
    assert_eq!(
        paths("'/tmp/my shot.jpg' \"/tmp/two.webp\"", false),
        Some(vec![unix("/tmp/my shot.jpg"), unix("/tmp/two.webp")])
    );
    assert_eq!(
        paths("/tmp/my\\ shot\\ \\(1\\).jpeg", false),
        Some(vec![unix("/tmp/my shot (1).jpeg")])
    );
    assert_eq!(
        paths(
            "file:///tmp/my%20shot.gif file://localhost/tmp/b.bmp",
            false
        ),
        Some(vec![unix("/tmp/my shot.gif"), unix("/tmp/b.bmp")])
    );
    assert_eq!(
        paths("~/Pictures/x.png", false),
        Some(vec![unix("/home/me/Pictures/x.png")])
    );
    assert_eq!(
        paths("C:\\Users\\me\\x.png", true),
        Some(vec![Candidate::Windows("C:\\Users\\me\\x.png".to_owned())])
    );
    assert_eq!(
        paths("\"C:\\Users\\me\\my shot.png\"", true),
        Some(vec![Candidate::Windows(
            "C:\\Users\\me\\my shot.png".to_owned()
        )])
    );
    assert_eq!(
        paths("file:///C:/Users/me/x.png", true),
        Some(vec![Candidate::Windows("C:/Users/me/x.png".to_owned())])
    );
    assert_eq!(paths("C:\\Users\\me\\x.png", false), None);
    assert_eq!(paths("look at /tmp/a.png", false), None);
    assert_eq!(paths("/tmp/a.png /tmp/notes.txt", false), None);
    assert_eq!(paths("a.png", false), None);
    assert_eq!(paths("/tmp/.png", false), None);
    assert_eq!(paths("/tmp/png", false), None);
    assert_eq!(paths("'/tmp/a.png", false), None);
    assert_eq!(paths("", false), None);
    assert_eq!(paths("   \n", false), None);
    assert_eq!(image_paths("~/x.png", None, false), None);
    assert_eq!(paths("file:///tmp/%zz.png", false), None);
    assert_eq!(paths("file:///tmp/a%00.png", false), None);
    assert_eq!(paths("file:///tmp/a%1b%5b2J.png", false), None);
    assert_eq!(paths("file:///tmp/a%0a.png", false), None);
    assert_eq!(paths("file:///tmp/a%c2%9b.png", false), None);
}

#[test]
fn only_existing_regular_files_attach_and_others_paste_as_text() {
    let fakes = Fakes::new("files", &[]);
    let setup = fakes.setup(&[]);
    let image = bmp(4, 4, 9);
    fakes.put("one.bmp", &image);
    std::fs::create_dir_all(fakes.dir.root.join("dir.png")).unwrap();
    let fifo = fakes.dir.root.join("fifo.png");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let job = |paths: Vec<Candidate>| Job::Paths {
        text: "pasted".to_owned(),
        paths,
    };
    let path = |name: &str| Candidate::Unix(fakes.dir.root.join(name));
    let running = Running::default();
    let Got::Images { images, .. } = perform(job(vec![path("one.bmp")]), HIGH, &setup, &running)
    else {
        panic!("not attached");
    };
    assert_eq!(&*images[0].data, wisp_media::encode_base64(&image));
    for missing in ["gone.png", "dir.png", "fifo.png"] {
        let started = Instant::now();
        assert_eq!(
            perform(
                job(vec![path("one.bmp"), path(missing)]),
                HIGH,
                &setup,
                &running
            ),
            Got::Text("pasted".to_owned()),
            "{missing}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{missing}");
    }
    let big = std::fs::File::create(fakes.dir.root.join("big.png")).unwrap();
    big.set_len(READ_CAP + 1).unwrap();
    assert_eq!(
        perform(
            job(vec![path("one.bmp"), path("big.png")]),
            HIGH,
            &setup,
            &running
        ),
        Got::Refused {
            text: "pasted".to_owned(),
            why: "Couldn't attach big.png. The image file is larger than 40 MB.".to_owned()
        }
    );
}

#[test]
fn a_path_that_exists_but_cannot_attach_gives_back_its_text_with_the_reason() {
    let fakes = Fakes::new("refused", &[]);
    let setup = fakes.setup(&[]);
    fakes.put("one.bmp", &bmp(4, 4, 9));
    fakes.put("notes.png", b"plain text, not a picture");
    fakes.put("locked.png", &bmp(4, 4, 1));
    let locked = fakes.dir.root.join("locked.png");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::File::open(&locked).is_ok();
    let path = |name: &str| Candidate::Unix(fakes.dir.root.join(name));
    let refused = |name: &str| {
        perform(
            Job::Paths {
                text: format!("'/x/one.bmp' '/x/{name}'"),
                paths: vec![path("one.bmp"), path(name)],
            },
            HIGH,
            &setup,
            &Running::default(),
        )
    };
    assert_eq!(
        refused("notes.png"),
        Got::Refused {
            text: "'/x/one.bmp' '/x/notes.png'".to_owned(),
            why: "Couldn't attach notes.png. This isn't a PNG, JPEG, GIF, WebP or BMP image."
                .to_owned()
        }
    );
    if !readable {
        let got = refused("locked.png");
        let Got::Refused { text, why } = &got else {
            panic!("{got:?}");
        };
        assert_eq!(text, "'/x/one.bmp' '/x/locked.png'");
        assert!(why.starts_with("Couldn't attach locked.png. "), "{why}");
        assert!(why.contains("ermission denied"), "{why}");
    }
}

#[test]
fn escape_sequences_in_file_names_never_reach_a_sentence() {
    let fakes = Fakes::new("names", &[]);
    let setup = fakes.setup(&[]);
    let name = "\u{1b}]52;c;aGk=\u{7}\u{1b}[2J\u{9b}31mx.png";
    fakes.put(name, b"not an image at all");
    let got = perform(
        Job::Paths {
            text: "t".to_owned(),
            paths: vec![Candidate::Unix(fakes.dir.root.join(name))],
        },
        HIGH,
        &setup,
        &Running::default(),
    );
    let Got::Refused {
        text,
        why: sentence,
    } = got
    else {
        panic!("{got:?}");
    };
    assert_eq!(text, "t");
    assert!(
        !sentence.chars().any(|c| c.is_control() || c == '\u{9b}'),
        "{sentence:?}"
    );
    assert!(sentence.contains("x.png"), "{sentence:?}");
    assert!(sentence.ends_with("This isn't a PNG, JPEG, GIF, WebP or BMP image."));
    let miss = Miss::Failed(format!("{name} broke"));
    assert!(!miss.sentence().chars().any(char::is_control));
    assert!(wslpath(name, &setup, &Running::default()).is_err());
}

#[test]
fn the_reader_never_blocks_its_caller_and_reports_through_the_wake() {
    let fakes = Fakes::new("worker", &["wl-paste"]);
    fakes.put("wl-paste.hang", b"");
    let mut setup = fakes.setup(&[Native::Wayland]);
    setup.tools.timeout = Duration::from_secs(1);
    let wake = Arc::new(Notify::new());
    let mut reader = Reader::new(setup, Arc::clone(&wake));
    let started = Instant::now();
    assert_eq!(reader.start(Job::Clipboard, HIGH), Ok(()));
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(reader.busy());
    assert_eq!(
        reader.start(Job::Clipboard, HIGH),
        Err(copy::STILL_READING_CLIPBOARD.to_owned())
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let got = loop {
        if let Some(got) = reader.take().into_iter().next() {
            break got;
        }
        assert!(Instant::now() < deadline, "no result");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        matches!(got, Got::Failed(ref why) if why.contains("longer than 1s")),
        "{got:?}"
    );
    assert!(!reader.busy());
}

fn text_of(setup: &Setup) -> Text {
    read_text(setup, &Running::default())
}

#[test]
fn each_platform_reads_text_with_its_fixed_argv() {
    let fakes = Fakes::new(
        "text-argv",
        &["powershell.exe", "pbpaste", "wl-paste", "xclip"],
    );
    for tool in ["pbpaste", "wl-paste", "xclip"] {
        fakes.put(&format!("{tool}.out"), b"one\ntwo\n");
    }
    fakes.put("powershell.exe.out", b"\xEF\xBB\xBFtext:one\r\ntwo\r\n");
    for native in [Native::Mac, Native::Wayland, Native::X11] {
        assert_eq!(
            text_of(&fakes.setup(&[native])),
            Text::Found("one\ntwo\n".to_owned()),
            "{native:?}"
        );
    }
    assert_eq!(
        text_of(&fakes.setup(&[Native::Windows])),
        Text::Found("one\r\ntwo\r\n".to_owned())
    );
    let owned = |args: &[&str]| -> Vec<String> { args.iter().map(|a| (*a).to_owned()).collect() };
    assert_eq!(fakes.args("pbpaste"), vec![Vec::<String>::new()]);
    assert_eq!(
        fakes.args("wl-paste"),
        vec![owned(&["--no-newline", "--type", "text"])]
    );
    assert_eq!(
        fakes.args("xclip"),
        vec![owned(&[
            "-selection",
            "clipboard",
            "-t",
            "UTF8_STRING",
            "-o"
        ])]
    );
    let script = encoded_text_script();
    assert_eq!(
        fakes.args("powershell.exe"),
        vec![owned(&[
            "-NoProfile",
            "-NonInteractive",
            "-Sta",
            "-EncodedCommand",
            &script
        ])]
    );
    let decoded = wisp_media::decode_base64(&script).unwrap();
    let units: Vec<u16> = decoded
        .chunks(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    assert_eq!(String::from_utf16(&units).unwrap(), WSL_TEXT_SCRIPT);
    assert!(WSL_TEXT_SCRIPT.contains("Get-Clipboard -Raw"));
}

#[test]
fn no_text_is_empty_and_never_an_error() {
    let fakes = Fakes::new("text-none", &["powershell.exe", "wl-paste", "xclip"]);
    fakes.put("powershell.exe.out", b"none");
    assert_eq!(text_of(&fakes.setup(&[Native::Windows])), Text::Empty);
    fakes.put("powershell.exe.out", b"text:");
    assert_eq!(text_of(&fakes.setup(&[Native::Windows])), Text::Empty);
    fakes.put("wl-paste.fail", b"");
    assert_eq!(text_of(&fakes.setup(&[Native::Wayland])), Text::Empty);
    assert_eq!(text_of(&fakes.setup(&[Native::X11])), Text::Empty);
    assert_eq!(text_of(&fakes.setup(&[])), Text::Empty);
    let mut ssh = fakes.setup(&[Native::X11]);
    ssh.ssh = true;
    fakes.put("xclip.out", b"secret");
    assert_eq!(text_of(&ssh), Text::Empty);
    assert_eq!(fakes.args("xclip").len(), 1);
}

#[test]
fn the_first_platform_with_a_tool_answers() {
    let fakes = Fakes::new("text-first", &["wl-paste", "xclip"]);
    fakes.put("wl-paste.fail", b"");
    fakes.put("xclip.out", b"from x11");
    assert_eq!(
        text_of(&fakes.setup(&[Native::Wayland, Native::X11])),
        Text::Empty
    );
    assert!(fakes.args("xclip").is_empty());
    std::fs::remove_file(fakes.dir.root.join("wl-paste")).unwrap();
    assert_eq!(
        text_of(&fakes.setup(&[Native::Wayland, Native::X11])),
        Text::Found("from x11".to_owned())
    );
}

#[test]
fn slow_and_flooding_text_readers_are_named() {
    let fakes = Fakes::new("text-bad", &["wl-paste"]);
    let mut setup = fakes.setup(&[Native::Wayland]);
    setup.tools.timeout = Duration::from_secs(60);
    fakes.put("wl-paste.flood", b"41000000");
    assert_eq!(
        text_of(&setup),
        Text::Failed("The clipboard text is larger than 40 MB.".to_owned())
    );
    std::fs::remove_file(fakes.dir.root.join("wl-paste.flood")).unwrap();
    fakes.put("wl-paste.hang", b"");
    setup.tools.timeout = Duration::from_secs(1);
    assert_eq!(
        text_of(&setup),
        Text::Failed("Couldn't read the clipboard: wl-paste took longer than 1s.".to_owned())
    );
}

#[test]
fn a_text_read_reports_through_the_worker() {
    let fakes = Fakes::new("text-worker", &["pbpaste"]);
    let wake = Arc::new(Notify::new());
    let mut reader = Reader::new(fakes.setup(&[Native::Mac]), Arc::clone(&wake));
    let next = |reader: &mut Reader| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(got) = reader.take().into_iter().next() {
                return got;
            }
            assert!(Instant::now() < deadline, "no result");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    fakes.put("pbpaste.out", b"hello");
    assert_eq!(reader.start_text(), Ok(()));
    assert!(reader.busy());
    assert_eq!(
        reader.start_text(),
        Err(copy::STILL_READING_CLIPBOARD.to_owned())
    );
    assert_eq!(next(&mut reader), Got::Pasted("hello".to_owned()));
    std::fs::remove_file(fakes.dir.root.join("pbpaste.out")).unwrap();
    assert_eq!(reader.start_text(), Ok(()));
    assert_eq!(next(&mut reader), Got::NoText);
}

#[test]
fn clipboard_text_that_looks_like_the_scripts_markers_comes_through_intact() {
    let fakes = Fakes::new("text-markers", &["powershell.exe", "wl-paste"]);
    let windows = fakes.setup(&[Native::Windows]);
    for (out, want) in [
        ("text:text:abc", "text:abc"),
        ("text:none", "none"),
        ("\u{FEFF}text:none\r\n", "none\r\n"),
        ("text: spaced ", " spaced "),
    ] {
        fakes.put("powershell.exe.out", out.as_bytes());
        assert_eq!(text_of(&windows), Text::Found(want.to_owned()), "{out:?}");
    }
    let wayland = fakes.setup(&[Native::Wayland]);
    for text in ["none", "text:abc", "image:abc"] {
        fakes.put("wl-paste.out", text.as_bytes());
        assert_eq!(text_of(&wayland), Text::Found(text.to_owned()), "{text:?}");
    }
}
