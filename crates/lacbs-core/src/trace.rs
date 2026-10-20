//! 阶段化（非平稳）负载：每条轨迹由若干 phase 拼成，phase 之间到达率/长度分布/类别混合会变。
//! 用途：M1 的固定 profile 扫描与逐阶段 oracle；后续也是控制器的评估母线。

use crate::model::{Class, Config, Request};
use crate::rng::Lcg;

#[derive(Debug, Clone, Copy)]
pub struct Phase {
    pub name: &'static str,
    pub duration_us: u64,
    /// 负载倍率：1.0 = 恰好当前配置的稳态容量。
    pub load: f64,
    pub interactive_pct: u32,
    pub i_prompt: (u32, u32),
    pub i_output: (u32, u32),
    pub b_prompt: (u32, u32),
    pub b_output: (u32, u32),
    pub i_ttft_us: u64,
    pub i_tpot_us: u64,
    pub b_ttft_us: u64,
    pub b_tpot_us: u64,
}

pub struct Trace {
    pub name: &'static str,
    pub phases: Vec<Phase>,
}

impl Trace {
    pub fn duration_us(&self) -> u64 {
        self.phases.iter().map(|p| p.duration_us).sum()
    }
}

const I_TTFT: u64 = 25_000; // 8ms 一步 ⇒ 首字地板约 16–19ms
const I_TPOT: u64 = 20_000;
const B_TTFT: u64 = 300_000;
const B_TPOT: u64 = 100_000;

fn phase(name: &'static str, secs: u64, load: f64, interactive_pct: u32) -> Phase {
    Phase {
        name,
        duration_us: secs * 1_000_000,
        load,
        interactive_pct,
        i_prompt: (64, 192),
        i_output: (16, 48),
        b_prompt: (128, 512),
        b_output: (64, 192),
        i_ttft_us: I_TTFT,
        i_tpot_us: I_TPOT,
        b_ttft_us: B_TTFT,
        b_tpot_us: B_TPOT,
    }
}

/// 批处理 TPOT 卡在 12ms（一步 8ms）：抢占重算会让它掉出 SLO ⇒ 真正的权衡场景。
fn tradeoff_phase(name: &'static str, load: f64, interactive_pct: u32) -> Phase {
    Phase {
        name,
        duration_us: 10_000_000,
        load,
        interactive_pct,
        i_prompt: (64, 192),
        i_output: (16, 48),
        b_prompt: (128, 256),
        b_output: (192, 256),
        i_ttft_us: I_TTFT,
        i_tpot_us: I_TPOT,
        b_ttft_us: B_TTFT,
        b_tpot_us: 10_000,
    }
}

pub fn traces() -> Vec<Trace> {
    vec![
        Trace {
            name: "calm-burst-calm",
            phases: vec![
                phase("calm", 10, 0.5, 33),
                phase("burst", 10, 1.15, 33),
                phase("calm2", 10, 0.5, 33),
            ],
        },
        Trace {
            name: "short-long",
            phases: vec![
                Phase {
                    b_prompt: (64, 128),
                    b_output: (32, 96),
                    ..phase("short", 10, 0.9, 33)
                },
                Phase {
                    b_prompt: (512, 1024),
                    b_output: (32, 96),
                    ..phase("long", 10, 0.9, 33)
                },
            ],
        },
        Trace {
            name: "class-shift",
            phases: vec![
                phase("mixed", 10, 0.95, 33),
                phase("interactive-heavy", 10, 0.95, 85),
            ],
        },
        Trace {
            name: "preempt-tradeoff",
            phases: vec![
                tradeoff_phase("batch-flood", 1.15, 5),
                tradeoff_phase("interactive-wave", 1.15, 50),
            ],
        },
        Trace {
            name: "oscillating",
            phases: vec![
                phase("low1", 10, 0.4, 33),
                phase("high1", 10, 1.15, 33),
                phase("low2", 10, 0.4, 33),
                phase("high2", 10, 1.15, 33),
            ],
        },
    ]
}

/// 该 phase 在当前配置下的稳态容量（请求/秒）：槽位 / (混合平均输出长度 × 单步耗时)。
/// 只看 decode，prefill 与真实 batch 效率未计入——它只把负载标到"同一条尺"上。
pub fn capacity_per_s(cfg: &Config, phase: &Phase) -> f64 {
    let mix = phase.interactive_pct as f64 / 100.0;
    let avg_out = mix * mid(phase.i_output) + (1.0 - mix) * mid(phase.b_output);
    let step_us = cfg.step_overhead_us as f64 + cfg.max_seqs as f64 * cfg.decode_us_per_seq as f64;
    cfg.max_seqs as f64 / (avg_out * step_us / 1e6)
}

fn mid(r: (u32, u32)) -> f64 {
    (r.0 as f64 + r.1 as f64) / 2.0
}

/// 生成整条轨迹的请求；`phases` 可只传一个 phase（oracle 的逐阶段测量）。
/// `capacity_per_s` 由调用方按成本模型算出，phase 的 `load` 是它的倍数。
pub fn generate(seed: u64, phases: &[Phase], cfg: &Config) -> Vec<Request> {
    let mut rng = Lcg::new(seed);
    let mut out = Vec::new();
    let mut id = 0u32;
    let mut start = 0u64;

    for p in phases {
        let rate = p.load * capacity_per_s(cfg, p);
        let mean_interval = (1_000_000.0 / rate) as u64;
        let mut t = start;
        loop {
            // 均匀落在 [0.5, 1.5] × 平均间隔，整数运算，跨平台逐位一致
            let interval =
                mean_interval / 2 + u64::from(rng.next_u32() % (mean_interval as u32 + 1));
            t += interval.max(1);
            if t >= start + p.duration_us {
                break;
            }
            out.push(make(&mut rng, id, t, p));
            id += 1;
        }
        start += p.duration_us;
    }
    out
}

fn make(rng: &mut Lcg, id: u32, arrival: u64, p: &Phase) -> Request {
    let interactive = rng.pct(p.interactive_pct);
    let (class, prompt, output) = if interactive {
        (Class::Interactive, p.i_prompt, p.i_output)
    } else {
        (Class::Batch, p.b_prompt, p.b_output)
    };
    Request {
        id,
        class,
        arrival_us: arrival,
        prompt_tokens: rng.range(prompt.0, prompt.1),
        output_tokens: rng.range(output.0, output.1),
        ttft_slo_us: if interactive {
            p.i_ttft_us
        } else {
            p.b_ttft_us
        },
        tpot_slo_us: if interactive {
            p.i_tpot_us
        } else {
            p.b_tpot_us
        },
    }
}
