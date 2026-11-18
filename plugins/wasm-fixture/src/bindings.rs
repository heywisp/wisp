#![allow(
    unsafe_code,
    unreachable_pub,
    reason = "wit-bindgen's generated exports are unsafe extern functions"
)]

wit_bindgen::generate!({
    world: "wisp:plugin/plugin@0.3.0",
    path: ["../../wit/v0.1.0", "../../wit/v0.2.0", "../../wit/v0.3.0"],
    generate_all,
});

use crate::glue::Fixture;

export!(Fixture);
