#[path = "../../../tests/support/purity.rs"]
mod purity;

use std::path::{Path, PathBuf};

use purity::{
    SRC, TEST_MODULES, assert_test_modules_are_files, impurities, non_test_code, relative, sources,
};

const IO_MODULES: [&str; 6] = ["fs", "env", "process", "io", "net", "thread"];

const IMPURE_WORDS: [&str; 17] = [
    "Instant::now",
    "SystemTime",
    "UNIX_EPOCH",
    "print!",
    "println!",
    "eprint!",
    "eprintln!",
    "dbg!",
    "open(",
    "File::",
    "set_hook",
    "take_hook",
    ".save(",
    "save_buffer",
    "save_buffer_with_format",
    "save_with_format",
    "ImageReader::open",
];

const ALLOWED: [(&str, &str, &str); 1] = [(
    "normalise.rs",
    "use std::io::Cursor;",
    "the decoders read from an in-memory cursor over the caller's bytes",
)];

fn checked_code(path: &Path) -> String {
    let name = relative(path);
    let mut code = non_test_code(path);
    for (file, line, _) in ALLOWED {
        if file == name {
            assert_eq!(code.matches(line).count(), 1, "{name}: {line}");
            code = code.replace(line, "");
        }
    }
    code
}

fn all_sources() -> Vec<PathBuf> {
    sources(Path::new(SRC), "rs", &TEST_MODULES, 6)
}

#[test]
fn the_scanner_catches_what_it_looks_for() {
    for sample in [
        "use std::fs;",
        "let x = std::env::var(\"A\");",
        "use std::{fmt, process};",
        "use std::io::Read;",
        "std::thread::spawn(f);",
        "let t = SystemTime::now();",
        "let gone = start.elapsed();",
        "eprintln!(\"x\");",
        "let f = File::open(p);",
        "std::panic::set_hook(h);",
        "image.save(\"out.png\")?;",
        "image::save_buffer(p, &raw, 1, 1, colour)?;",
        "save_buffer_with_format(p, &raw, 1, 1, colour, format)?;",
        "image.save_with_format(p, ImageFormat::Png)?;",
        "let reader = ImageReader::open(p)?;",
        "let reader = image::ImageReader::open(p)?;",
    ] {
        assert!(
            !impurities(sample, &IO_MODULES, &IMPURE_WORDS).is_empty(),
            "missed: {sample}"
        );
    }
    for clean in [
        "use std::cell::Cell;",
        "use std::panic::{AssertUnwindSafe, catch_unwind};",
        "let environment = 1; let threads = 3; let ratio = 2;",
    ] {
        assert!(
            impurities(clean, &IO_MODULES, &IMPURE_WORDS).is_empty(),
            "{clean}"
        );
    }
}

#[test]
fn wisp_media_does_no_io_reads_no_clock_and_installs_no_hook() {
    for path in all_sources() {
        let found = impurities(&checked_code(&path), &IO_MODULES, &IMPURE_WORDS);
        assert!(found.is_empty(), "{}: {found:?}", relative(&path));
    }
}

#[test]
fn the_allowed_lines_still_exist() {
    for (file, line, reason) in ALLOWED {
        let text = std::fs::read_to_string(Path::new(SRC).join(file)).unwrap();
        assert!(text.contains(line), "{file}: {line} ({reason}) is gone");
    }
}

#[test]
fn every_test_module_is_a_separate_file() {
    assert_test_modules_are_files(&TEST_MODULES, &all_sources());
}
