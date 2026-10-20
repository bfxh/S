//! 连续批处理模拟器：每个 step 先保底 decode，再用剩余预算做 prefill。
//! 纯整数时间，同一输入逐位可复算。
//!
//! 两个可独立开关的过载机制（默认都关，便于归因）：
//! - `reject_hopeless`：到服务时已赶不上首字截止期的请求直接拒绝；
//! - `preempt_for_interactive`：交互式在等且槽位满时，挤掉批处理序列，KV 丢弃、恢复时重算。

use crate::model::{Class, Config, Finished, Rejected, Report, Request};
use crate::policy::Policy;

struct Seq {
    req: Request,
    prefilled: u32,
    generated: u32,
    first_token_at: Option<u64>,
    last_token_at: u64,
}

impl Seq {
    fn new(req: Request) -> Self {
        Self {
            req,
            prefilled: 0,
            generated: 0,
            first_token_at: None,
            last_token_at: req.arrival_us,
        }
    }

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

    let mut waiting: Vec<Seq> = requests.into_iter().map(Seq::new).collect();
    let mut running: Vec<Seq> = Vec::new();
    let mut report = Report::default();
    let mut time = waiting.first().map_or(0, |s| s.req.arrival_us);

    loop {
        if running.is_empty() && waiting.is_empty() {
            break;
        }

        let available_end = waiting.partition_point(|s| s.req.arrival_us <= time);
        if running.is_empty() && available_end == 0 {
            time = waiting[0].req.arrival_us;
            continue;
        }
        let mut pool: Vec<Seq> = waiting.drain(..available_end).collect();

        // A. 拒绝：已经无望的请求不再占算力
        if cfg.reject_hopeless {
            let mut i = 0;
            while i < pool.len() {
                if hopeless(&pool[i], time, &cfg) {
                    let s = pool.remove(i);
                    report.rejected.push(Rejected {
                        id: s.req.id,
                        class: s.req.class,
                        arrival_us: s.req.arrival_us,
                        rejected_at_us: time,
                    });
                } else {
                    i += 1;
                }
            }
        }

        // B. 抢占：一次把所有在等的交互式都腾出槽位（否则它们要排队等好几个 step）
        let mut cooled: Vec<Seq> = Vec::new();
        if cfg.preempt_for_interactive {
            let waiting_interactive = pool
                .iter()
                .filter(|s| s.req.class == Class::Interactive)
                .count();
            let free = cfg.max_seqs.saturating_sub(running.len());
            let mut need = waiting_interactive.saturating_sub(free);
            while need > 0 {
                let Some(vi) = pick_victim(&running) else {
                    break;
                };
                let mut v = running.remove(vi);
                report.preemptions += 1;
                report.recomputed_tokens += u64::from(v.req.prompt_tokens) + u64::from(v.generated);
                v.prefilled = 0;
                v.last_token_at = time;
                cooled.push(v);
                need -= 1;
            }
        }

        // C. 预算：先保底已有序列的 decode，再给 prefill
        let decode_set: Vec<usize> = (0..running.len())
            .filter(|&i| running[i].prefill_done())
            .collect();
        let partial: Vec<usize> = (0..running.len())
            .filter(|&i| !running[i].prefill_done())
            .collect();
        let decode_now = decode_set.len() as u32;
        let mut budget = cfg.max_step_tokens - decode_now;
        let mut prefill_tokens = 0u32;

        // D. 续跑上次没切完的 prefill（在途优先）
        for &i in &partial {
            let need = running[i].req.prompt_tokens - running[i].prefilled;
            let take = chunk_for(policy, &cfg, need).min(budget);
            running[i].prefilled += take;
            budget -= take;
            prefill_tokens += take;
        }

        // E. 新准入（不再抢占，避免同一步反复腾挪）
        let mut admitted: Vec<Seq> = Vec::new();
        while budget > 0 && running.len() + admitted.len() < cfg.max_seqs {
            let reqs: Vec<&Request> = pool.iter().map(|s| &s.req).collect();
            let Some(pos) = policy.pick(&reqs) else { break };
            let mut job = pool.remove(pos);
            let need = job.req.prompt_tokens - job.prefilled;
            let take = chunk_for(policy, &cfg, need).min(budget);
            if take == 0 {
                pool.insert(pos, job);
                break;
            }
            budget -= take;
            prefill_tokens += take;
            job.prefilled += take;
            admitted.push(job);
        }

        // F. 时间前进
        time += cfg.step_overhead_us
            + u64::from(prefill_tokens) * cfg.prefill_us_per_token
            + u64::from(decode_now) * cfg.decode_us_per_seq;
        report.steps += 1;

        let step_tokens = prefill_tokens + decode_now;
        report.max_step_tokens_seen = report.max_step_tokens_seen.max(step_tokens);
        report.total_step_tokens += u64::from(step_tokens);

        // G. 本步开始时已在 decode 的序列出词
        for &i in &decode_set {
            let seq = &mut running[i];
            seq.generated += 1;
            seq.last_token_at = time;
        }

        // H. 本步完成 prefill 的（含被抢占后重算完的）给首字
        for &i in &partial {
            if running[i].prefill_done() {
                finish_prefill(&mut running[i], time);
            }
        }
        for mut job in admitted {
            if job.prefill_done() {
                finish_prefill(&mut job, time);
            }
            running.push(job);
        }

        // I. 收尾
        let mut i = 0;
        while i < running.len() {
            if running[i].generated >= running[i].req.output_tokens {
                let seq = running.swap_remove(i);
                report.finished.push(convert(&seq));
            } else {
                i += 1;
            }
        }

        // J. 剩下的回等待队列（被抢占者本步不参与准入）
        pool.extend(cooled);
        pool.sort_by_key(|s| (s.req.arrival_us, s.req.id));
        waiting.splice(0..0, pool);
    }

    report.makespan_us = time;
    report.finished.sort_by_key(|f| f.id);
    report.rejected.sort_by_key(|r| r.id);
    Ok(report)
}

fn hopeless(seq: &Seq, now: u64, cfg: &Config) -> bool {
    let need = seq.req.prompt_tokens - seq.prefilled;
    let earliest = now
        + cfg.step_overhead_us
        + u64::from(need) * cfg.prefill_us_per_token
        + cfg.decode_us_per_seq;
    earliest > seq.req.arrival_us + seq.req.ttft_slo_us
}

/// 只允许低优先级给高优先级让路；挑剩余输出最多的批处理序列。
fn pick_victim(running: &[Seq]) -> Option<usize> {
    running
        .iter()
        .enumerate()
        .filter(|(_, s)| s.req.class == Class::Batch && s.generated < s.req.output_tokens)
        .max_by_key(|(_, s)| (s.req.output_tokens - s.generated, s.req.id))
        .map(|(i, _)| i)
}

fn chunk_for(policy: Policy, cfg: &Config, need: u32) -> u32 {
    match policy.chunk_limit(cfg) {
        0 => need,
        c => c.min(need),
    }
}

fn finish_prefill(seq: &mut Seq, time: u64) {
    if seq.first_token_at.is_none() {
        seq.first_token_at = Some(time);
        seq.generated = 1;
    }
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
