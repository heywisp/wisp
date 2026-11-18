#[path = "../../../tests/support/purity.rs"]
mod purity;

use std::path::{Path, PathBuf};

use purity::{
    SRC, TEST_MODULES, assert_test_modules_are_files, impurities, non_test_code, relative, sources,
    word_count,
};

const IO_MODULES: [&str; 6] = ["fs", "env", "process", "net", "thread", "os"];

const IMPURE_WORDS: [&str; 15] = [
    "Instant",
    "SystemTime",
    "UNIX_EPOCH",
    "std::time",
    "rustix",
    "libc",
    "print!",
    "println!",
    "eprint!",
    "eprintln!",
    "dbg!",
    "open(",
    "File::",
    "set_hook",
    "take_hook",
];

const MTIME: &str = "pub fn mtime(modified: std::time::SystemTime) -> Option<i64> {\n    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;\n    i64::try_from(since.as_nanos()).ok()\n}\n";

fn checked_code(path: &Path) -> String {
    non_test_code(path).replace(MTIME, "")
}

fn all_sources() -> Vec<PathBuf> {
    sources(Path::new(SRC), "rs", &TEST_MODULES, 3)
}

#[test]
fn the_scanner_catches_what_it_looks_for() {
    for sample in [
        "use std::fs;",
        "let x = std::env::var(\"A\");",
        "use std::{fmt, process};",
        "use std::net::TcpStream;",
        "std::thread::spawn(f);",
        "let t = SystemTime::now();",
        "let gone = start.elapsed();",
        "eprintln!(\"x\");",
        "let f = File::open(p);",
        "std::panic::set_hook(h);",
        "let start = Instant::now();",
        "use std::os::unix::fs::MetadataExt;",
        "let t = std::time::Duration::ZERO;",
        "rustix::fs::open(p);",
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
        "fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>>;",
        "use std::io;",
    ] {
        assert!(
            impurities(clean, &IO_MODULES, &IMPURE_WORDS).is_empty(),
            "{clean}"
        );
    }
}

#[test]
fn wisp_instructions_does_no_io_reads_no_clock_and_installs_no_hook() {
    for path in all_sources() {
        let found = impurities(&checked_code(&path), &IO_MODULES, &IMPURE_WORDS);
        assert!(found.is_empty(), "{}: {found:?}", relative(&path));
    }
}

#[test]
fn the_only_time_type_is_the_one_mtime_conversion() {
    let lib = std::fs::read_to_string(Path::new(SRC).join("lib.rs")).unwrap();
    assert_eq!(lib.matches(MTIME).count(), 1);
    let elsewhere: usize = all_sources()
        .iter()
        .map(|p| word_count(&checked_code(p), "time"))
        .sum();
    assert_eq!(elsewhere, 0);
}

#[test]
fn every_test_module_is_a_separate_file() {
    assert_test_modules_are_files(&TEST_MODULES, &all_sources());
}
