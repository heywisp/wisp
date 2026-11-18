#![allow(
    dead_code,
    reason = "render's, tui's and cli's tests and diff's properties binary each use part of the generator"
)]

pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    pub(crate) fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }

    pub(crate) fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        let i = self.below(xs.len());
        xs.get(i).copied().unwrap_or_default()
    }
}
