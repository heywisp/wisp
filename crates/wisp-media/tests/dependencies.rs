#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

use dependencies::{built_without_defaults, depends_only_on, normal_dependencies};

const ALLOWED: [&str; 4] = ["base64", "image", "image-webp", "thiserror"];

const IMAGE_FEATURES: [&str; 5] = ["bmp", "gif", "jpeg", "png", "webp"];

#[test]
fn wisp_media_depends_only_on_its_image_crates_and_never_on_the_kernel() {
    depends_only_on(&ALLOWED, &[]);
}

#[test]
fn image_is_built_with_only_the_five_formats() {
    built_without_defaults("image", &IMAGE_FEATURES);
}

#[test]
fn wisp_tui_reaches_wisp_media() {
    let tui = normal_dependencies("wisp-tui");
    let names: Vec<&str> = tui.iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"wisp-media"), "{names:?}");
}
