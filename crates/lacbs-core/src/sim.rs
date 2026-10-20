//! 连续批处理模拟器：每个 step 先保底 decode，再用剩余预算做 prefill。
//! 纯整数时间，同一输入逐位可复算。

use crate::model::{Config, Finished, Report, Request};
use crate::policy::Policy;

struct Seq {
    req: Request,
    prefilled: u32,
    generated: u32,
    first_token_at: Option<u64>,
    last_token_at: u64,
}

impl Seq {
    fn prefill_done(&self) -> bool {
        self.prefilled == self.req.prompt_tokens
    }
}

pub fn run(cfg: Config, policy: Policy, mut requests: Vec<Request>) -> Result<Report, String> {
    cfg.validate()?;
    for r in &requests {
        if r.prompt_tokens == 0 || r.output_tokens == 0 {
            return Err(format!("请求 {} 的 prompt/output 必须 ≥ 1", r.id));
        }
    }
    requests.sort_by_key(|r| (r.arrival_us, r.id));

    let mut waiting = requests;
    let mut running: Vec<Seq> = Vec::new();
    let mut report = Report::default();
    let mut time = waiting.first().map_or(0, |r| r.arrival_us);

    loop {
        if running.is_empty() && waiting.is_empty() {
            break;
        }

        let available_end = waiting.partition_point(|r| r.arrival_us <= time);
        if running.is_empty() && available_end == 0 {
            time = waiting[0].arrival_us;
            continue;
        }

        // 1) 预算：先保底已有序列的 decode，再给 prefill
        let decode_set: Vec<usize> = (0..running.len())
            .filter(|&i| running[i].prefill_done())
            .collect();
        let partial: Vec<usize> = (0..running.len())
            .filter(|&i| !running[i].prefill_done())
            .collect();
        let decode_now = decode_set.len() as u32;
        let mut budget = cfg.max_step_tokens - decode_now;
        let mut prefill_tokens = 0u32;

        // 2) 续跑上次没切完的 prefill（在途优先）
        for &i in &partial {
            let need = running[i].req.prompt_tokens - running[i].prefilled;
            let take = chunk_for(policy, &cfg, need).min(budget);
            running[i].prefilled += take;
            budget -= take;
            prefill_tokens += take;
        }

        // 3) 新准入
        let mut admitted: Vec<(usize, u32)> = Vec::new();
        loop {
            if running.len() + admitted.len() >= cfg.max_seqs || budget == 0 {
                break;
            }
            let free: Vec<usize> = (0..available_end)
                .filter(|i| !admitted.iter().any(|(j, _)| j == i))
                .collect();
            let cand: Vec<&Request> = free.iter().map(|&i| &waiting[i]).collect();
            let Some(pos) = policy.pick(&cand) else { break };
            let wi = free[pos];
            let need = waiting[wi].prompt_tokens;
            let take = chunk_for(policy, &cfg, need).min(budget);
            if take == 0 {
                break;
            }
            budget -= take;
            prefill_tokens += take;
            admitted.push((wi, take));
        }

        // 4) 时间前进
        time += cfg.step_overhead_us
            + u64::from(prefill_tokens) * cfg.prefill_us_per_token
            + u64::from(decode_now) * cfg.decode_us_per_seq;
        report.steps += 1;

        let step_tokens = prefill_tokens + decode_now;
        report.max_step_tokens_seen = report.max_step_tokens_seen.max(step_tokens);
        report.total_step_tokens += u64::from(step_tokens);

        // 5) 本步开始时已在 decode 的序列出词
        for &i in &decode_set {
            let seq = &mut running[i];
            seq.generated += 1;
            seq.last_token_at = time;
        }

        // 6) 本步完成 prefill 的给出首字
        for &i in &partial {
            if running[i].prefill_done() {
                finish_prefill(&mut running[i], time);
            }
        }

        // 7) 新准入的序列
        if !admitted.is_empty() {
            let mut keep = vec![true; waiting.len()];
            for &(wi, take) in &admitted {
                keep[wi] = false;
                let req = waiting[wi];
                let mut seq = Seq {
                    req,
                    prefilled: take,
                    generated: 0,
                    first_token_at: None,
                    last_token_at: time,
                };
                if seq.prefill_done() {
                    finish_prefill(&mut seq, time);
                }
                running.push(seq);
            }
            let mut idx = 0;
            waiting.retain(|_| {
                let k = keep[idx];
                idx += 1;
                k
            });
        }

        // 8) 收尾
        let mut i = 0;
        while i < running.len() {
            if running[i].generated >= running[i].req.output_tokens {
                let seq = running.swap_remove(i);
                report.finished.push(convert(&seq));
            } else {
                i += 1;
            }
        }
    }

    report.makespan_us = time;
    report.finished.sort_by_key(|f| f.id);
    Ok(report)
}

fn chunk_for(policy: Policy, cfg: &Config, need: u32) -> u32 {
    match policy.chunk_limit(cfg) {
        0 => need,
        c => c.min(need),
    }
}

fn finish_prefill(seq: &mut Seq, time: u64) {
    seq.first_token_at = Some(time);
    seq.generated = 1;
    seq.last_token_at = time;
}

fn convert(seq: &Seq) -> Finished {
    let first = seq.first_token_at.unwrap_or(seq.req.arrival_us);
    let ttft_us = first.saturating_sub(seq.req.arrival_us);
    let tpot_us = if seq.generated > 1 {
        (seq.last_token_at - first) / u64::from(seq.generated - 1)
    } else {
        0
    };
    Finished {
        id: seq.req.id,
        class: seq.req.class,
        arrival_us: seq.req.arrival_us,
        ttft_us,
        finish_us: seq.last_token_at,
        tpot_us,
        met_ttft: ttft_us <= seq.req.ttft_slo_us,
        met_tpot: tpot_us <= seq.req.tpot_slo_us,
    }
}
