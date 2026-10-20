//! 确定性 RNG：同 seed 逐位复现，不引第三方库。

pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6364136223846793005).wrapping_add(1))
    }

    pub(crate) fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    pub(crate) fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + self.next_u32() % (hi - lo + 1)
    }

    pub(crate) fn pct(&mut self, p: u32) -> bool {
        self.next_u32() % 100 < p
    }

    /// 抖动：均值为 0 的微秒扰动。
    pub(crate) fn jitter(&mut self, span_us: u64) -> i64 {
        (self.next_u32() as i64 % (span_us as i64 * 2 + 1)) - span_us as i64
    }
}
