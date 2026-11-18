#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test on any file or process error"
)]

use crate::support;

use std::path::PathBuf;
use std::process::Stdio;

use support::{
    WISP,
    home::{Home, both, command_in},
};

impl Home {
    fn images(&self) -> PathBuf {
        self.0.join(".local/share/wisp/images")
    }

    fn attachments(&self) -> PathBuf {
        self.0.join(".local/share/wisp/attachments")
    }

    fn doctor(&self) -> String {
        self.section("images")
    }

    fn section(&self, title: &str) -> String {
        let output = command_in(&self.0, WISP)
            .arg("doctor")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let all = both(&output);
        all.split(&format!("\n{title}\n"))
            .nth(1)
            .and_then(|rest| rest.split("\n\n").next())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("no {title} section in\n{all}"))
    }
}

fn row(section: &str, key: &str) -> String {
    section
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(key))
        .map(|rest| rest.trim().to_owned())
        .unwrap_or_else(|| format!("no {key} row in\n{section}"))
}

#[test]
fn doctor_reports_a_missing_images_folder_without_creating_it() {
    let home = Home::bare("missing");
    let section = home.doctor();
    assert_eq!(row(&section, "folder"), "~/.local/share/wisp/images");
    assert_eq!(row(&section, "saved"), "none yet");
    assert!(!home.images().exists());
    assert!(!home.0.join(".local").exists());
}

#[test]
fn doctor_counts_the_saved_images() {
    let home = Home::new("saved");
    std::fs::create_dir_all(home.images().join("nested")).unwrap();
    std::fs::write(home.images().join("a.png"), vec![0u8; 1_500]).unwrap();
    std::fs::write(home.images().join("b.jpg"), vec![0u8; 500]).unwrap();
    let section = home.doctor();
    assert_eq!(row(&section, "saved"), "2 files, 2.0 kB");
    assert_eq!(std::fs::read_dir(home.images()).unwrap().count(), 3);
}

#[test]
fn doctor_names_a_symlinked_or_file_images_folder() {
    let home = Home::new("odd");
    let real = home.0.join("elsewhere");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(home.images().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, home.images()).unwrap();
    assert_eq!(row(&home.doctor(), "saved"), "error: is a symlink");
    std::fs::remove_file(home.images()).unwrap();
    std::fs::write(home.images(), "").unwrap();
    assert_eq!(row(&home.doctor(), "saved"), "error: not a folder");
}

#[test]
fn doctor_reports_the_attachments_folder_beside_the_old_images_one() {
    let home = Home::new("attachments");
    let section = home.section("attachments");
    assert_eq!(row(&section, "folder"), "~/.local/share/wisp/attachments");
    assert_eq!(row(&section, "saved"), "none yet");
    assert!(!home.attachments().exists());
    std::fs::create_dir_all(home.attachments()).unwrap();
    std::fs::write(home.attachments().join("a.pdf"), vec![0u8; 2_500]).unwrap();
    let section = home.section("attachments");
    assert_eq!(row(&section, "saved"), "1 file, 2.5 kB");
    assert_eq!(row(&home.doctor(), "saved"), "none yet");
}
