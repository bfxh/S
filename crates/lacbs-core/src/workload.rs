//! 确定性负载生成：同 seed 逐位复现，不引第三方 RNG。
//! 两类请求混跑——交互式（短输出、紧首字）与批处理（长输出、松预算），
//! 只有异质 SLO 存在时，"按截止期排序"才和 FIFO 有区别。

use crate::model::{Class, Request};
use crate::rng::Lcg;

pub struct Spec {
    pub seed: u64,
    pub steady: u32,
    pub burst: u32,
    pub interval_us: u64,
    pub burst_at_us: u64,
    /// 齐射之后再单独到的交互式：用来制造"槽位已被批处理占满"的抢占场景。
    pub late_interactive: u32,
    /// 强制平稳到达者的类别（None = 按比例随机）。
    pub steady_class: Option<Class>,
    /// 强制齐射者的类别。
    pub burst_class: Option<Class>,
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
            late_interactive: 0,
            steady_class: None,
            burst_class: None,
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
    let mut out = Vec::with_capacity((spec.steady + spec.burst + spec.late_interactive) as usize);

    for i in 0..spec.steady {
        let arrival = i as u64 * spec.interval_us;
        out.push(make(&mut rng, i, arrival, spec, spec.steady_class));
    }
    for i in 0..spec.burst {
        let jitter = rng.jitter(1_000).max(0) as u64;
        out.push(make(
            &mut rng,
            spec.steady + i,
            spec.burst_at_us + jitter,
            spec,
            spec.burst_class,
        ));
    }
    // 槽位已被批处理占满之后才到的交互式：这才是抢占的用武之地
    for i in 0..spec.late_interactive {
        let arrival = spec.burst_at_us + 50_000 + u64::from(i) * 2_000;
        out.push(make(
            &mut rng,
            spec.steady + spec.burst + i,
            arrival,
            spec,
            Some(Class::Interactive),
        ));
    }
    out.sort_by_key(|r| (r.arrival_us, r.id));
    out
}

fn make(rng: &mut Lcg, id: u32, arrival: u64, spec: &Spec, forced: Option<Class>) -> Request {
    let class = forced.unwrap_or(if rng.pct(spec.interactive_pct) {
        Class::Interactive
    } else {
        Class::Batch
    });
    let (prompt, output, ttft, tpot) = match class {
        Class::Interactive => (spec.i_prompt, spec.i_output, spec.i_ttft_us, spec.i_tpot_us),
        Class::Batch => (spec.b_prompt, spec.b_output, spec.b_ttft_us, spec.b_tpot_us),
    };
    Request {
        id,
        attempt: 0,
        class,
        arrival_us: arrival,
        prompt_tokens: rng.range(prompt.0, prompt.1),
        output_tokens: rng.range(output.0, output.1),
        ttft_slo_us: ttft,
        tpot_slo_us: tpot,
    }
}
