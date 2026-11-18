#![deny(clippy::print_stdout, clippy::print_stderr)]

mod canvas;
pub mod chrome;
pub mod doctor;
mod footer;
pub mod highlight;
pub mod markdown;
pub mod motion;
pub mod picture;
pub mod plan_usage;
pub mod stream;
pub mod styled;
pub mod text;
pub mod theme;
pub mod transcript;

#[cfg(test)]
extern crate self as wisp_render;

#[cfg(test)]
mod testing;

#[cfg(test)]
#[path = "../tests/support/events.rs"]
mod events;

#[cfg(test)]
#[path = "../tests/support/code.rs"]
mod code;

pub use footer::{footer, num};
pub use text::strip_ansi;
