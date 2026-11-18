pub const PATCH: u32 = 28;

pub const MAX_LONG_EDGE: u32 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tier {
    pub max_edge: u32,
    pub max_tokens: u32,
}

impl Tier {
    pub const STANDARD: Self = Self {
        max_edge: 1568,
        max_tokens: 1568,
    };

    pub const HIGH: Self = Self {
        max_edge: 2576,
        max_tokens: 4784,
    };

    pub const NAMED: [(&'static str, Self); 2] =
        [("standard", Self::STANDARD), ("high", Self::HIGH)];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
    pub tokens: u64,
}

impl ImageSize {
    fn of(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            tokens: tokens(width, height),
        }
    }
}

pub fn tokens(width: u32, height: u32) -> u64 {
    u64::from(width.div_ceil(PATCH)) * u64::from(height.div_ceil(PATCH))
}

fn padded(edge: u32) -> u64 {
    u64::from(edge.div_ceil(PATCH)) * u64::from(PATCH)
}

fn fits(width: u32, height: u32, tier: Tier) -> bool {
    let edge = u64::from(tier.max_edge);
    padded(width) <= edge
        && padded(height) <= edge
        && tokens(width, height) <= u64::from(tier.max_tokens)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the short edge is at most the long edge, a u32, and is clamped to at least 1"
)]
fn short_edge(long: u32, aspect: f64) -> u32 {
    let short = (f64::from(long) / aspect).round_ties_even();
    if short < 1.0 {
        1
    } else if short >= f64::from(long) {
        long
    } else {
        short as u32
    }
}

fn shrink_wide(width: u32, height: u32, accept: impl Fn(u32, u32) -> bool) -> (u32, u32) {
    let aspect = f64::from(width) / f64::from(height);
    let (mut lo, mut hi) = (1, width);
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if accept(mid, short_edge(mid, aspect)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo, short_edge(lo, aspect))
}

fn shrink(width: u32, height: u32, accept: impl Fn(u32, u32) -> bool) -> (u32, u32) {
    if height > width {
        let (h, w) = shrink_wide(height, width, |long, short| accept(short, long));
        (w, h)
    } else {
        shrink_wide(width, height, accept)
    }
}

pub fn api_size(width: u32, height: u32, tier: Tier) -> ImageSize {
    if width == 0 || height == 0 || fits(width, height, tier) {
        return ImageSize::of(width, height);
    }
    let (w, h) = shrink(width, height, |w, h| fits(w, h, tier));
    ImageSize::of(w, h)
}

pub fn target(width: u32, height: u32, tier: Tier) -> ImageSize {
    let sent = api_size(width, height, tier);
    if sent.width.max(sent.height) <= MAX_LONG_EDGE {
        return sent;
    }
    capped(width, height, MAX_LONG_EDGE)
}

pub(crate) fn capped(width: u32, height: u32, long_edge: u32) -> ImageSize {
    if width == 0 || height == 0 || width.max(height) <= long_edge {
        return ImageSize::of(width, height);
    }
    let (w, h) = shrink(width, height, |w, h| w.max(h) <= long_edge);
    ImageSize::of(w, h)
}

#[cfg(test)]
mod tests;
