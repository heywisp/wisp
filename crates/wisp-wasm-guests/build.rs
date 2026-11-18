use std::path::{Path, PathBuf};
use std::process::{Command, exit};

const TARGET: &str = "wasm32-wasip2";

const GUESTS: [&str; 2] = ["wasm-fixture", "board"];

const CLEARED: [&str; 4] = [
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
];

fn fail(message: &str) -> ! {
    println!("cargo::error={message}");
    exit(1)
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fail(&format!("cargo didn't set {name}")))
}

fn constant(guest: &str) -> String {
    guest.replace('-', "_").to_uppercase()
}

fn main() {
    let root = Path::new(&var("CARGO_MANIFEST_DIR")).join("../..");
    let target_dir = PathBuf::from(var("OUT_DIR")).join("guests");
    println!("cargo::rerun-if-changed={}", root.join("wit").display());
    for guest in GUESTS {
        let folder = root.join("plugins").join(guest);
        for part in ["src", "Cargo.toml", "wisp-plugin.toml"] {
            println!("cargo::rerun-if-changed={}", folder.join(part).display());
        }
    }
    let mut build = Command::new(var("CARGO"));
    build
        .current_dir(&root)
        .args([
            "build",
            "--offline",
            "--locked",
            "--profile",
            "guest",
            "--target",
            TARGET,
        ])
        .arg("--target-dir")
        .arg(&target_dir);
    for guest in GUESTS {
        build.args(["-p", guest]);
    }
    for name in CLEARED {
        build.env_remove(name);
    }
    match build.status() {
        Ok(status) if status.success() => {}
        Ok(_) => fail(&format!(
            "the WASM guests didn't build; if the target is missing, run `rustup target add {TARGET}`"
        )),
        Err(e) => fail(&format!("cargo didn't start for the WASM guests: {e}")),
    }
    for guest in GUESTS {
        let wasm = target_dir
            .join(TARGET)
            .join("guest")
            .join(format!("{}.wasm", guest.replace('-', "_")));
        let manifest = root.join("plugins").join(guest).join("wisp-plugin.toml");
        let name = constant(guest);
        println!("cargo::rustc-env={name}={}", wasm.display());
        println!("cargo::rustc-env={name}_MANIFEST={}", manifest.display());
    }
}
