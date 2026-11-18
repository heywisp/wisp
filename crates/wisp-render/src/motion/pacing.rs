use std::time::Duration;

use super::{FRAME_MIN, MotionCtx};

pub const MAX_FPS: u32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pacing {
    cx: MotionCtx,
    unfocused: bool,
}

impl Pacing {
    pub const fn new(cx: MotionCtx) -> Pacing {
        Pacing {
            cx,
            unfocused: false,
        }
    }

    pub const fn ctx(&self) -> MotionCtx {
        self.cx
    }

    pub const fn set_ctx(&mut self, cx: MotionCtx) {
        self.cx = cx;
    }

    pub const fn focus(&mut self, focused: bool) {
        self.unfocused = !focused;
    }

    pub const fn unfocused(&self) -> bool {
        self.unfocused
    }
}

pub fn next_frame(want: Duration, last_frame: Option<Duration>) -> Duration {
    match last_frame {
        Some(last) => want.max(last.saturating_add(FRAME_MIN)),
        None => want,
    }
}
