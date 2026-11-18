use std::io;
use std::path::{Path, PathBuf};

use crate::secrets::detect::wsl2_release;
pub use wisp_protocol::shorten_path;

mod resources;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub root: PathBuf,
    pub home: Option<PathBuf>,
    pub temp: PathBuf,
}

impl Workspace {
    pub fn detect() -> io::Result<Self> {
        Ok(Self {
            root: std::env::current_dir()?,
            home: home(),
            temp: std::env::temp_dir(),
        })
    }

    pub fn shorten(&self, path: &Path) -> String {
        shorten_path(path, self.home.as_deref())
    }
}

pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
}

pub fn home_shown(path: &Path) -> String {
    shorten_path(path, home().as_deref())
}

pub fn platform() -> String {
    match std::env::consts::OS {
        "linux" => linux_platform(
            &std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default(),
            |marker| Path::new(marker).exists(),
        ),
        "macos" => "macOS".to_owned(),
        "windows" => "Windows".to_owned(),
        other => other.to_owned(),
    }
}

const CONTAINER_MARKERS: [&str; 2] = ["/.dockerenv", "/run/.containerenv"];

fn linux_platform(osrelease: &str, exists: impl Fn(&str) -> bool) -> String {
    let release = osrelease.to_ascii_lowercase();
    if CONTAINER_MARKERS.into_iter().any(exists) {
        "Linux (container)".to_owned()
    } else if wsl2_release(osrelease) {
        "Linux (WSL2)".to_owned()
    } else if release.contains("microsoft") {
        "Linux (WSL)".to_owned()
    } else {
        "Linux".to_owned()
    }
}

pub fn resources() -> Option<String> {
    let facts = match std::env::consts::OS {
        "linux" => resources::read(&resources::Host, None),
        _ => resources::Resources {
            cpus_visible: std::thread::available_parallelism()
                .ok()
                .and_then(|n| u32::try_from(n.get()).ok()),
            ..resources::Resources::default()
        },
    };
    resources::line(&facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_is_named() {
        let none = |_: &str| false;
        assert_eq!(
            linux_platform("6.18.33.2-microsoft-standard-WSL2\n", none),
            "Linux (WSL2)"
        );
        assert_eq!(
            linux_platform("4.4.0-19041-Microsoft\n", none),
            "Linux (WSL)"
        );
        assert_eq!(linux_platform("6.9.0-arch1-1\n", none), "Linux");
    }

    #[test]
    fn each_container_marker_overrides_the_kernel_string() {
        for marker in ["/.dockerenv", "/run/.containerenv"] {
            assert_eq!(
                linux_platform("6.18.33.2-microsoft-standard-WSL2\n", |p| p == marker),
                "Linux (container)",
                "{marker}"
            );
            assert_eq!(
                linux_platform("6.9.0-arch1-1\n", |p| p == marker),
                "Linux (container)"
            );
        }
        assert_eq!(
            linux_platform("6.9.0-arch1-1\n", |p| p == "/.containerenv"),
            "Linux"
        );
    }
}
