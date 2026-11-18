use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[allow(
    clippy::expect_used,
    reason = "the test fails if the piped stdin is missing"
)]
pub(crate) fn install(path: &Path, script: &str) {
    let mut writer = Command::new("/bin/sh")
        .args([
            "-c",
            "/bin/cat > \"$1\" && /bin/chmod 755 \"$1\"",
            "install",
        ])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = writer.stdin.take().expect("stdin is piped");
    stdin.write_all(script.as_bytes()).unwrap();
    drop(stdin);
    assert!(writer.wait().unwrap().success(), "{}", path.display());
}
