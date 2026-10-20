//! 确定性负载生成：同 seed 逐位复现，不引第三方 RNG。
//! 两类请求混跑——交互式（短输出、紧首字）与批处理（长输出、松预算），
//! 只有异质 SLO 存在时，"按截止期排序"才和 FIFO 有区别。

use crate::model::{Class, Request};

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6364136223846793005).wrapping_add(1))
    }

    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + self.next_u32() % (hi - lo + 1)
    }

    fn pct(&mut self, p: u32) -> bool {
        self.next_u32() % 100 < p
    }

    /// 抖动：均值为 0 的微秒扰动。
    fn jitter(&mut self, span_us: u64) -> i64 {
        (self.next_u32() as i64 % (span_us as i64 * 2 + 1)) - span_us as i64
    }
}

pub struct Spec {
    pub seed: u64,
    pub steady: u32,
    pub burst: u32,
    pub interval_us: u64,
    pub burst_at_us: u64,
    pub interactive_pct: u32,
    pub i_prompt: (u32, u32),
    pub i_output: (u32, u32),
    pub i_ttft_us: u64,
    pub i_tpot_us: u64,
    pub b_prompt: (u32, u32),
    pub b_output: (u32, u32),
    pub b_ttft_us: u64,
    pub b_tpot_us: u64,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            seed: 7,
            steady: 24,
            burst: 96,
            interval_us: 8_000,
            burst_at_us: 200_000,
            interactive_pct: 33,
            i_prompt: (64, 192),
            i_output: (16, 48),
            i_ttft_us: 15_000,
            i_tpot_us: 5_000,
            b_prompt: (128, 512),
            b_output: (64, 192),
            b_ttft_us: 300_000,
            b_tpot_us: 20_000,
        }
    }
}

/// 前一半按固定间隔平稳到达，后一半在同一时刻齐射。
pub fn generate(spec: &Spec) -> Vec<Request> {
    let mut rng = Lcg::new(spec.seed);
    let mut out = Vec::with_capacity((spec.steady + spec.burst) as usize);

    for i in 0..spec.steady {
        let arrival = i as u64 * spec.interval_us;
        out.push(make(&mut rng, i, arrival, spec));
    }
    for i in 0..spec.burst {
        let jitter = rng.jitter(1_000).max(0) as u64;
        out.push(make(
            &mut rng,
            spec.steady + i,
            spec.burst_at_us + jitter,
            spec,
        ));
    }
    out.sort_by_key(|r| (r.arrival_us, r.id));
    out
}

fn make(rng: &mut Lcg, id: u32, arrival: u64, spec: &Spec) -> Request {
    let class = if rng.pct(spec.interactive_pct) {
        Class::Interactive
    } else {
        Class::Batch
    };
    let (prompt, output, ttft, tpot) = match class {
        Class::Interactive => (spec.i_prompt, spec.i_output, spec.i_ttft_us, spec.i_tpot_us),
        Class::Batch => (spec.b_prompt, spec.b_output, spec.b_ttft_us, spec.b_tpot_us),
    };
    Request {
        id,
        class,
        arrival_us: arrival,
        prompt_tokens: rng.range(prompt.0, prompt.1),
        output_tokens: rng.range(output.0, output.1),
        ttft_slo_us: ttft,
        tpot_slo_us: tpot,
    }
}
