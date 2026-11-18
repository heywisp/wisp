pub mod app;
pub mod present;
pub mod term;
pub mod widgets;

#[cfg(test)]
#[path = "../tests/support/pick.rs"]
mod pick;

#[cfg(test)]
#[path = "../tests/support/scratch.rs"]
mod scratch;

#[cfg(test)]
#[path = "../tests/support/shared.rs"]
mod shared;

#[cfg(test)]
#[path = "../../wisp-render/tests/support/events.rs"]
mod events;

#[cfg(test)]
#[path = "../../wisp-render/tests/support/code.rs"]
mod code;

#[cfg(test)]
#[path = "../../wisp-render/src/testing.rs"]
mod testing;

#[cfg(test)]
const SCRATCH: &str = "wisp-tui-";
