#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

use dependencies::{built_without_defaults, depends_only_on};

const ALLOWED: [&str; 3] = ["similar", "unicode-segmentation", "unicode-width"];

#[test]
fn wisp_diff_depends_only_on_its_three_crates_and_no_workspace_crate() {
    depends_only_on(&ALLOWED, &[]);
}

#[test]
fn similar_is_built_without_std_or_any_feature() {
    assert_eq!(built_without_defaults("similar", &[])["req"], "^3.2.1");
}
