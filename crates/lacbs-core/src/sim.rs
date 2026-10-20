//! 连续批处理模拟器：每个 step 先保底 decode，再用剩余预算做 prefill。
//! 纯整数时间，同一输入逐位可复算。
//!
//! 两个可独立开关的过载机制（默认都关，便于归因）：
//! - `reject_hopeless`：到服务时已赶不上首字截止期的请求直接拒绝；
//! - `preempt_for_interactive`：交互式在等且槽位满时，挤掉批处理序列，KV 丢弃、恢复时重算。

use crate::controller::{Controller, WindowStats};
use crate::model::{Class, Config, Finished, Rejected, Report, Request};
use crate::policy::Policy;
use crate::rng::Lcg;

const STEP_BUDGET: u64 = 3_000_000;

struct Seq {
    req: Request,
    prefilled: u32,
    generated: u32,
    first_token_at: Option<u64>,
    last_token_at: u64,
    first_admit_at: Option<u64>,
    max_gap_us: u64,
    /// 被抢占后的冷却截止时间：在冷却期内不再被准入，防"挤出→抢回→再挤出"乒乓。
    cooldown_until: u64,
}

impl Seq {
    fn new(req: Request) -> Self {
        Self {
            req,
            prefilled: 0,
            generated: 0,
            first_token_at: None,
            last_token_at: req.arrival_us,
            first_admit_at: None,
            max_gap_us: 0,
            cooldown_until: 0,
        }
    }

    fn prefill_done(&self) -> bool {
        self.prefilled == self.req.prompt_tokens
    }
}

pub fn run(cfg: Config, policy: Policy, requests: Vec<Request>) -> Result<Report, String> {
    run_inner(cfg, Some(policy), requests, None)
}

/// 带窗口控制器的运行：每个窗口结束时问控制器要一个 profile，立刻生效。
pub fn run_with_controller(
    base: Config,
    requests: Vec<Request>,
    window_us: u64,
    ctl: &mut dyn Controller,
) -> Result<(Report, Vec<WindowStats>), String> {
    let arrivals: Vec<u64> = {
        let mut v: Vec<u64> = requests.iter().map(|r| r.arrival_us).collect();
        v.sort_unstable();
        v
    };
    let mut windows = Vec::new();
    let mut ctx = CtlCtx {
        window_us: window_us.max(1),
        index: 0,
        next_window: 0,
        win_start: 0,
        fin_cursor: 0,
        rej_cursor: 0,
        recompute_seen: 0,
        arrivals,
        arr_cursor: 0,
        ctl,
        windows: &mut windows,
    };
    let report = run_inner(base, None, requests, Some(&mut ctx))?;
    Ok((report, windows))
}

struct CtlCtx<'a> {
    window_us: u64,
    index: u64,
    next_window: u64,
    win_start: u64,
    fin_cursor: usize,
    rej_cursor: usize,
    recompute_seen: u64,
    arrivals: Vec<u64>,
    arr_cursor: usize,
    ctl: &'a mut dyn Controller,
    windows: &'a mut Vec<WindowStats>,
}

fn close_window(
    c: &mut CtlCtx<'_>,
    report: &Report,
    end: u64,
    queue_age_us: u64,
    queue_len: usize,
) -> WindowStats {
    let start = c.win_start;
    let from = c.arr_cursor;
    while c.arr_cursor < c.arrivals.len() && c.arrivals[c.arr_cursor] < end {
        c.arr_cursor += 1;
    }
    let arrivals = c.arr_cursor - from;
    let fin = &report.finished[c.fin_cursor..];
    let rej = &report.rejected[c.rej_cursor..];
    let mut met = 0usize;
    let mut max_wait = queue_age_us;
    for f in fin {
        if f.met_both() {
            met += 1;
        }
        max_wait = max_wait.max(f.wait_us);
    }
    let stats = WindowStats {
        index: c.index,
        t_start: start,
        t_end: end,
        arrivals,
        finished: fin.len(),
        met,
        rejected: rej.len(),
        recompute_delta: report.recomputed_tokens - c.recompute_seen,
        max_wait_us: max_wait,
        queue_len,
    };
    c.fin_cursor = report.finished.len();
    c.rej_cursor = report.rejected.len();
    c.recompute_seen = report.recomputed_tokens;
    stats
}

fn run_inner(
    base: Config,
    policy: Option<Policy>,
    mut requests: Vec<Request>,
    mut ctl: Option<&mut CtlCtx<'_>>,
) -> Result<Report, String> {
    let mut cfg = base;
    let mut policy = policy.unwrap_or(Policy::Deadline);
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
    if let Some(c) = ctl.as_mut() {
        c.win_start = time;
        c.next_window = time + c.window_us;
    }
    let mut rng = Lcg::new(cfg.retry_seed);
    let mut next_id = waiting.iter().map(|s| s.req.id).max().unwrap_or(0) + 1;

    loop {
        if report.steps > STEP_BUDGET {
            return Err(format!(
                "步数超过预算 {STEP_BUDGET}（时间 {} µs，剩余 {} 个请求）",
                time,
                running.len() + waiting.len()
            ));
        }
        if running.is_empty() && waiting.is_empty() {
            break;
        }

        if let Some(c) = ctl.as_mut() {
            while time >= c.next_window {
                let age = waiting
                    .first()
                    .map_or(0, |s| time.saturating_sub(s.req.arrival_us));
                let stats = close_window(
                    c,
                    &report,
                    c.next_window,
                    age,
                    waiting.len() + running.len(),
                );
                let pid = c.ctl.choose(&stats);
                let (p, next) = pid.apply(base);
                policy = p;
                cfg = next;
                c.windows.push(stats);
                c.win_start = c.next_window;
                c.next_window += c.window_us;
                c.index += 1;
            }
        }

        let available_end = waiting.partition_point(|s| s.req.arrival_us <= time);
        if running.is_empty() && available_end == 0 {
            time = waiting[0].req.arrival_us;
            continue;
        }
        let mut pool: Vec<Seq> = waiting.drain(..available_end).collect();

        // A0. 队列上限：超出的新到请求直接拒绝（先到先留）
        if let Some(cap) = cfg.max_queue {
            while pool.len() > cap {
                let s = pool.pop().expect("pool 非空");
                report.rejected.push(Rejected {
                    id: s.req.id,
                    class: s.req.class,
                    arrival_us: s.req.arrival_us,
                    rejected_at_us: time,
                });
                report.queue_full_rejects += 1;
                if maybe_retry(&s.req, time, &cfg, &mut rng, &mut next_id, &mut waiting) {
                    report.retries += 1;
                }
            }
        }

        // A. 拒绝：装不下的（prompt+output 超过 KV 上限）永远拒绝；无望的按开关拒绝
        {
            let mut i = 0;
            while i < pool.len() {
                let infeasible = cfg
                    .kv_capacity_tokens
                    .is_some_and(|cap| pool[i].req.prompt_tokens + pool[i].req.output_tokens > cap);
                if infeasible || (cfg.reject_hopeless && hopeless(&pool[i], time, &cfg)) {
                    let s = pool.remove(i);
                    report.rejected.push(Rejected {
                        id: s.req.id,
                        class: s.req.class,
                        arrival_us: s.req.arrival_us,
                        rejected_at_us: time,
                    });
                    if maybe_retry(&s.req, time, &cfg, &mut rng, &mut next_id, &mut waiting) {
                        report.retries += 1;
                    }
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
                v.cooldown_until = time + cooldown_span(&cfg);
                cooled.push(v);
                need -= 1;
            }
        }

        // B2. KV 驻留压力：超上限时挤掉占 KV 最大的批处理序列
        if let Some(cap) = cfg.kv_capacity_tokens {
            loop {
                let resident = resident_tokens(&running);
                let decode_need = running.iter().filter(|s| s.prefill_done()).count() as u32;
                if resident + decode_need <= cap {
                    break;
                }
                let Some(vi) = pick_kv_victim(&running) else {
                    break;
                };
                let mut v = running.remove(vi);
                report.preemptions += 1;
                report.recomputed_tokens += u64::from(v.req.prompt_tokens) + u64::from(v.generated);
                v.prefilled = 0;
                v.last_token_at = time;
                v.cooldown_until = time + cooldown_span(&cfg);
                cooled.push(v);
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
        // 准入时为"下一步的 decode 增长"预留 max_seqs 个 token，避免把 KV 填到顶后每步都触发抢占
        let mut kv_left = match cfg.kv_capacity_tokens {
            Some(cap) => cap
                .saturating_sub(resident_tokens(&running))
                .saturating_sub(decode_now)
                .saturating_sub(cfg.max_seqs as u32),
            None => u32::MAX,
        };

        // D. 续跑上次没切完的 prefill（在途优先）
        for &i in &partial {
            let need = running[i].req.prompt_tokens - running[i].prefilled;
            let take = chunk_for(policy, &cfg, need).min(budget).min(kv_left);
            running[i].prefilled += take;
            budget -= take;
            kv_left -= take;
            prefill_tokens += take;
        }

        // E. 新准入（不再抢占，避免同一步反复腾挪）
        let mut admitted: Vec<Seq> = Vec::new();
        while budget > 0 && running.len() + admitted.len() < cfg.max_seqs {
            let Some(pos) = pick(policy, &pool, time, &cfg) else {
                break;
            };
            let mut job = pool.remove(pos);
            let need = job.req.prompt_tokens - job.prefilled;
            let take = chunk_for(policy, &cfg, need).min(budget).min(kv_left);
            if take == 0 {
                pool.insert(pos, job);
                break;
            }
            budget -= take;
            kv_left -= take;
            prefill_tokens += take;
            job.prefilled += take;
            if job.first_admit_at.is_none() {
                job.first_admit_at = Some(time);
            }
            admitted.push(job);
        }

        if std::env::var_os("LACBS_DEBUG_STEP").is_some() && report.steps < 8 {
            eprintln!(
                "step={} t={}us run={} pool={} wait={} dec={} pf={} admitted={} rej={}",
                report.steps,
                time,
                running.len(),
                pool.len(),
                waiting.len(),
                decode_now,
                prefill_tokens,
                admitted.len(),
                report.rejected.len()
            );
        }

        // F. 时间前进
        time += cfg.step_overhead_us
            + u64::from(prefill_tokens) * cfg.prefill_us_per_token
            + u64::from(decode_now) * cfg.decode_us_per_seq;
        report.steps += 1;

        let step_tokens = prefill_tokens + decode_now;
        report.max_step_tokens_seen = report.max_step_tokens_seen.max(step_tokens);
        report.total_step_tokens += u64::from(step_tokens);
        report.total_decode_tokens += u64::from(decode_now);
        report.total_prefill_tokens += u64::from(prefill_tokens);

        // G. 本步开始时已在 decode 的序列出词
        for &i in &decode_set {
            let seq = &mut running[i];
            seq.max_gap_us = seq.max_gap_us.max(time - seq.last_token_at);
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

        if let Some(cap) = cfg.kv_capacity_tokens {
            let resident = resident_tokens(&running);
            report.max_resident_tokens = report.max_resident_tokens.max(resident);
            if resident > cap {
                report.kv_overshoot_steps += 1;
            }
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

        // J. 剩下的回等待队列（被抢占者本步不参与准入；按到达时间回插，不整体排序）
        for v in cooled {
            let at = pool
                .partition_point(|s| (s.req.arrival_us, s.req.id) < (v.req.arrival_us, v.req.id));
            pool.insert(at, v);
        }
        waiting.splice(0..0, pool);
    }

    if let Some(c) = ctl.as_mut()
        && time > c.win_start
    {
        let stats = close_window(c, &report, time, 0, waiting.len() + running.len());
        c.windows.push(stats);
    }

    report.makespan_us = time;
    report.finished.sort_by_key(|f| f.id);
    report.rejected.sort_by_key(|r| r.id);
    Ok(report)
}

fn resident_tokens(running: &[Seq]) -> u32 {
    running.iter().map(|s| s.prefilled + s.generated).sum()
}

/// KV 压力下优先挤出占地最大的批处理序列；没有批处理时退而挤最大的任意序列（防活锁）。
fn pick_kv_victim(running: &[Seq]) -> Option<usize> {
    let batch = running
        .iter()
        .enumerate()
        .filter(|(_, s)| s.req.class == Class::Batch)
        .max_by_key(|(_, s)| (s.prefilled + s.generated, s.req.id));
    batch
        .or_else(|| {
            running
                .iter()
                .enumerate()
                .max_by_key(|(_, s)| (s.prefilled + s.generated, s.req.id))
        })
        .map(|(i, _)| i)
}

fn maybe_retry(
    req: &Request,
    now: u64,
    cfg: &Config,
    rng: &mut Lcg,
    next_id: &mut u32,
    waiting: &mut Vec<Seq>,
) -> bool {
    let cap = cfg.max_attempts.max(1);
    if cfg.retry_prob_pct == 0 || req.attempt.saturating_add(1) >= cap {
        return false;
    }
    if !rng.pct(cfg.retry_prob_pct) {
        return false;
    }
    let mut r = *req;
    r.attempt += 1;
    r.id = *next_id;
    *next_id += 1;
    let extra = if cfg.retry_delay_us > 1 {
        u64::from(rng.next_u32() % cfg.retry_delay_us as u32)
    } else {
        0
    };
    r.arrival_us = now + cfg.retry_delay_us + extra;
    let at = waiting.partition_point(|s| (s.req.arrival_us, s.req.id) < (r.arrival_us, r.id));
    waiting.insert(at, Seq::new(r));
    true
}

/// 先在候选里做老化插队（等待超过 factor×自身首字 SLO 的最老者优先），否则走策略本身。
fn pick(policy: Policy, pool: &[Seq], now: u64, cfg: &Config) -> Option<usize> {
    let ready = |s: &Seq| s.cooldown_until <= now;
    if let Some(factor) = cfg.aging_factor {
        let mut aged: Option<(u64, usize)> = None;
        for (i, s) in pool.iter().enumerate() {
            if !ready(s) {
                continue;
            }
            let waited = now.saturating_sub(s.req.arrival_us);
            if waited > factor.saturating_mul(s.req.ttft_slo_us) {
                match aged {
                    Some((w, _)) if w >= waited => {}
                    _ => aged = Some((waited, i)),
                }
            }
        }
        if let Some((_, i)) = aged {
            return Some(i);
        }
    }
    // 与 Policy::pick 同语义，但不在热路径上分配 Vec（等待队列可能上千条）
    match policy {
        Policy::Fifo => pool.iter().position(ready),
        Policy::Deadline => pool
            .iter()
            .enumerate()
            .filter(|(_, s)| ready(s))
            .min_by_key(|(_, s)| (s.req.ttft_deadline(), s.req.arrival_us, s.req.id))
            .map(|(i, _)| i),
    }
}

fn cooldown_span(cfg: &Config) -> u64 {
    cfg.step_overhead_us + cfg.decode_us_per_seq * cfg.max_seqs as u64
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
        max_gap_us: seq.max_gap_us,
        wait_us: seq
            .first_admit_at
            .unwrap_or(seq.req.arrival_us)
            .saturating_sub(seq.req.arrival_us),
        met_ttft: ttft_us <= seq.req.ttft_slo_us,
        met_tpot: tpot_us <= seq.req.tpot_slo_us && seq.max_gap_us <= 2 * seq.req.tpot_slo_us,
    }
}
