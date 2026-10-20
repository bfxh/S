//! 扫描：固定 profile 跑完整轨迹，按"到达阶段"归属，逐阶段取最优得到 oracle。
//! 支持两类输入：合成阶段化轨迹，以及从真实 trace 抽出的 CSV（`replay_csv`）。

use lacbs_core::model::{Class, Config, Request};
use lacbs_core::profile::ProfileId;
use lacbs_core::sim;
use lacbs_core::trace::{self, Trace};

struct Row {
    profile: ProfileId,
    met_by_phase: Vec<usize>,
    rejected_by_phase: Vec<i64>,
    met: usize,
    rejected: usize,
    retries: u64,
    recompute: u64,
    kv_peak: u32,
    kv_over: u64,
    max_wait_us: u64,
}

pub fn run(seed: u64, base: Config, reject_penalty: f64) -> anyhow::Result<()> {
    for trace in trace::traces() {
        let cfg = Config {
            decode_us_per_seq: 500,
            ..base
        };
        one(&trace, seed, cfg, reject_penalty)?;
    }
    Ok(())
}

fn one(trace: &Trace, seed: u64, cfg: Config, reject_penalty: f64) -> anyhow::Result<()> {
    let reqs = trace::generate(seed, &trace.phases, &cfg);
    let starts = phase_starts(trace);
    let names: Vec<String> = trace.phases.iter().map(|p| p.name.to_string()).collect();
    one_requests(trace.name, &names, &starts, reqs, cfg, reject_penalty)
}

/// 真实 trace 回放：CSV 列 = arrival_s,prompt_tokens,output_tokens,class。
/// 时间按 `target_load × 当前配置容量` 缩放，保持原 trace 的突发形状。
pub fn replay_csv(
    path: &str,
    cfg: Config,
    reject_penalty: f64,
    phases: usize,
    target_load: f64,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut rows: Vec<(f64, u32, u32, Class)> = Vec::new();
    let mut skipped = 0usize;
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() {
            continue;
        }
        let mut it = line.split(',');
        let t: f64 = it.next().unwrap_or("").trim().parse()?;
        let p: u32 = it.next().unwrap_or("").trim().parse()?;
        let o: u32 = it.next().unwrap_or("").trim().parse()?;
        let class = match it.next().unwrap_or("batch").trim() {
            "interactive" => Class::Interactive,
            _ => Class::Batch,
        };
        if p == 0 || o == 0 {
            skipped += 1;
            continue;
        }
        rows.push((t, p, o, class));
    }
    if skipped > 0 {
        eprintln!("replay {path}: 跳过 {skipped} 条空 prompt/output 记录");
    }
    if rows.is_empty() {
        anyhow::bail!("{path} 没有数据行");
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("时间非 NaN"));

    let n = rows.len() as f64;
    let span_s = rows.iter().map(|r| r.0).fold(0.0f64, f64::max).max(1.0);
    let avg_out = rows.iter().map(|r| f64::from(r.2)).sum::<f64>() / n;
    let avg_prompt = rows.iter().map(|r| f64::from(r.1)).sum::<f64>() / n;
    let cap = capacity_per_s(&cfg, avg_out, avg_prompt + avg_out);
    let observed = n / span_s;
    if cap <= 0.0 {
        anyhow::bail!("容量为 0，检查 max_seqs / decode_us_per_seq");
    }
    let k = observed / (target_load * cap);
    eprintln!(
        "replay {path}: rows={n} span={span_s:.0}s rate={observed:.2}/s cap={cap:.2}/s \
         avg_prompt={avg_prompt:.0} avg_out={avg_out:.0} scale={k:.4} (sim_span={:.0}s)",
        span_s * k
    );

    let mut reqs: Vec<Request> = rows
        .iter()
        .enumerate()
        .map(|(i, (t, p, o, class))| {
            let arrival_us = (t * 1e6 * k).round() as u64;
            let (ttft, tpot) = trace::slo_for(*class);
            Request {
                id: i as u32,
                attempt: 0,
                class: *class,
                arrival_us,
                prompt_tokens: *p,
                output_tokens: *o,
                ttft_slo_us: ttft,
                tpot_slo_us: tpot,
            }
        })
        .collect();
    reqs.sort_by_key(|r| (r.arrival_us, r.id));

    let total_us = (span_s * 1e6 * k).round() as u64;
    let phases = phases.max(1);
    let starts: Vec<u64> = (0..phases)
        .map(|i| total_us * i as u64 / phases as u64)
        .collect();
    let names: Vec<String> = (0..phases).map(|i| format!("p{}", i + 1)).collect();
    let label = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("csv");

    one_requests(label, &names, &starts, reqs, cfg, reject_penalty)
}

/// 稳态容量：受槽位与 KV 上限双重约束；decode 步成本按有效并发算。
fn capacity_per_s(cfg: &Config, avg_out: f64, avg_footprint: f64) -> f64 {
    let seqs = match cfg.kv_capacity_tokens {
        Some(cap) => (f64::from(cap) / avg_footprint.max(1.0))
            .floor()
            .clamp(1.0, cfg.max_seqs as f64),
        None => cfg.max_seqs as f64,
    };
    let step_us = cfg.step_overhead_us as f64 + seqs * cfg.decode_us_per_seq as f64;
    seqs / (avg_out * step_us / 1e6)
}

fn one_requests(
    label: &str,
    phase_names: &[String],
    starts: &[u64],
    reqs: Vec<Request>,
    cfg: Config,
    reject_penalty: f64,
) -> anyhow::Result<()> {
    let secs = starts.last().copied().unwrap_or(0) as f64 / 1e6;
    let mut rows = Vec::new();
    for pid in ProfileId::ALL {
        let (policy, c) = pid.apply(cfg);
        let r = match sim::run(c, policy, reqs.clone()) {
            Ok(r) => r,
            Err(e) => {
                // 抖动/颠簸到步数预算耗尽也是一种结果：记 0 达标，不中断整轮扫描
                println!("ROW trace={} profile={} error={}", label, pid.as_str(), e);
                rows.push(Row {
                    profile: pid,
                    met_by_phase: vec![0; starts.len()],
                    rejected_by_phase: vec![0; starts.len()],
                    met: 0,
                    rejected: 0,
                    retries: 0,
                    recompute: 0,
                    kv_peak: 0,
                    kv_over: 0,
                    max_wait_us: 0,
                });
                continue;
            }
        };
        let mut met_by_phase = vec![0usize; starts.len()];
        for f in &r.finished {
            if f.met_both() {
                met_by_phase[phase_of(f.arrival_us, starts)] += 1;
            }
        }
        let mut rejected_by_phase = vec![0i64; starts.len()];
        for x in &r.rejected {
            rejected_by_phase[phase_of(x.arrival_us, starts)] += 1;
        }
        let row = Row {
            profile: pid,
            met_by_phase,
            rejected_by_phase,
            met: r.met_both(),
            rejected: r.rejected.len(),
            retries: r.retries,
            recompute: r.recomputed_tokens,
            kv_peak: r.max_resident_tokens,
            kv_over: r.kv_overshoot_steps,
            max_wait_us: r.max_wait_us(),
        };
        println!(
            "ROW trace={} profile={} met={} goodput={:.2}/s rejected={} retries={} recompute={} kv_peak={} kv_over={} qfull={} steps={} dec_per_step={:.1} pf_per_step={:.0} span_s={:.0} max_wait_ms={:.1} by_phase={}",
            label,
            pid.as_str(),
            row.met,
            if secs > 0.0 {
                row.met as f64 / secs
            } else {
                0.0
            },
            row.rejected,
            row.retries,
            row.recompute,
            row.kv_peak,
            row.kv_over,
            r.queue_full_rejects,
            r.steps,
            r.total_decode_tokens as f64 / r.steps.max(1) as f64,
            r.total_prefill_tokens as f64 / r.steps.max(1) as f64,
            r.makespan_us as f64 / 1e6,
            row.max_wait_us as f64 / 1000.0,
            row.met_by_phase
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("/")
        );
        rows.push(row);
    }

    // 目标：达标数 − penalty × 拒绝数（penalty=0 即纯达标口径）
    let score = |r: &Row, pi: usize| -> f64 {
        r.met_by_phase[pi] as f64 - reject_penalty * r.rejected_by_phase[pi] as f64
    };
    let total = |r: &Row| -> f64 { (0..starts.len()).map(|pi| score(r, pi)).sum() };

    let best = rows
        .iter()
        .max_by(|a, b| total(a).partial_cmp(&total(b)).expect("分数非 NaN"))
        .expect("profile 列表非空");

    let mut per_phase = Vec::new();
    let mut oracle = 0.0f64;
    for (pi, name) in phase_names.iter().enumerate().take(starts.len()) {
        let (pid, sc) = rows
            .iter()
            .map(|r| (r.profile, score(r, pi)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).expect("分数非 NaN"))
            .expect("profile 列表非空");
        oracle += sc;
        per_phase.push(format!("{}={}:{:.0}", name, pid.as_str(), sc));
    }
    let best_sc = total(best);
    let gap_pct = if oracle.abs() < 1e-9 {
        0.0
    } else {
        (oracle - best_sc) / oracle.abs() * 100.0
    };
    println!(
        "SUMMARY trace={} penalty={} best_fixed={} best_score={:.0} best_met={} oracle_score={:.0} gap_pct={:.1} per_phase={}",
        label,
        reject_penalty,
        best.profile.as_str(),
        best_sc,
        best.met,
        oracle,
        gap_pct,
        per_phase.join(",")
    );
    Ok(())
}

fn phase_starts(trace: &Trace) -> Vec<u64> {
    let mut starts = Vec::with_capacity(trace.phases.len());
    let mut t = 0;
    for p in &trace.phases {
        starts.push(t);
        t += p.duration_us;
    }
    starts
}

fn phase_of(arrival_us: u64, starts: &[u64]) -> usize {
    starts
        .partition_point(|&s| s <= arrival_us)
        .saturating_sub(1)
}
