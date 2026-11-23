use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let commit = git(&["rev-parse", "--short=12", "HEAD"])
        .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_default();
    if !commit.is_empty() {
        let watch = git(&["rev-parse", "--git-path", "logs/HEAD"])
            .filter(|p| !p.contains('\n') && Path::new(p).exists())
            .or_else(|| {
                git(&["rev-parse", "--git-path", "HEAD"])
                    .filter(|p| !p.contains('\n') && Path::new(p).exists())
            });
        if let Some(path) = watch {
            println!("cargo::rerun-if-changed={path}");
        }
    }
    println!("cargo::rustc-env=WISP_COMMIT={commit}");
}
