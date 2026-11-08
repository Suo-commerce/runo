//! `SplitMix64`: a seeded, portable random sequence, so a seed replays exactly on any machine.

/// The generator.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Seeded.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n` (`0` when `n` is `0`).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// One of `n` small indices (lanes, replicas).
    pub fn pick(&mut self, n: u8) -> u8 {
        u8::try_from(self.below(u64::from(n))).unwrap_or(0)
    }

    /// True with probability `percent` in a hundred.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// Shuffle in place (Fisher–Yates).
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = usize::try_from(self.below(i as u64 + 1)).unwrap_or(0);
            v.swap(i, j);
        }
    }
}
