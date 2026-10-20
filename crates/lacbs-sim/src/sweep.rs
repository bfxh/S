//! M1 扫描：固定 profile 跑完整轨迹，按"到达阶段"归属达标数，逐阶段取最优得到 oracle。
//! 判据：最好的单一 profile 距离 oracle 的差距 <3% ⇒ 自适应层不值得做（见 docs/ADAPTIVE-DESIGN.md）。

use lacbs_core::model::Config;
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
        // 只跑 8ms/step 这一档：0.37ms 档下所有 profile 都被容量淹没，测不出差别
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
    let secs = trace.duration_us() as f64 / 1e6;

    let mut rows = Vec::new();
    for pid in ProfileId::ALL {
        let (policy, c) = pid.apply(cfg);
        let r = match sim::run(c, policy, reqs.clone()) {
            Ok(r) => r,
            Err(e) => {
                // 抖动/颠簸到步数预算耗尽也是一种结果：记 0 达标，不中断整轮扫描
                println!(
                    "ROW trace={} profile={} error={}",
                    trace.name,
                    pid.as_str(),
                    e
                );
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
                met_by_phase[phase_of(f.arrival_us, &starts)] += 1;
            }
        }
        let mut rejected_by_phase = vec![0i64; starts.len()];
        for x in &r.rejected {
            rejected_by_phase[phase_of(x.arrival_us, &starts)] += 1;
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
            "ROW trace={} profile={} met={} goodput={:.2}/s rejected={} retries={} recompute={} kv_peak={} kv_over={} max_wait_ms={:.1} by_phase={}",
            trace.name,
            pid.as_str(),
            row.met,
            row.met as f64 / secs,
            row.rejected,
            row.retries,
            row.recompute,
            row.kv_peak,
            row.kv_over,
            row.max_wait_us as f64 / 1000.0,
            row.met_by_phase
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("/")
        );
        rows.push(row);
    }

    // 目标：达标数 − penalty × 拒绝数（penalty=0 即 M1 的纯达标口径）
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
    for pi in 0..starts.len() {
        let (pid, sc) = rows
            .iter()
            .map(|r| (r.profile, score(r, pi)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).expect("分数非 NaN"))
            .expect("profile 列表非空");
        oracle += sc;
        per_phase.push(format!(
            "{}={}:{:.0}",
            trace.phases[pi].name,
            pid.as_str(),
            sc
        ));
    }
    let best_sc = total(best);
    let gap_pct = if oracle.abs() < 1e-9 {
        0.0
    } else {
        (oracle - best_sc) / oracle.abs() * 100.0
    };
    println!(
        "SUMMARY trace={} penalty={} best_fixed={} best_score={:.0} best_met={} oracle_score={:.0} gap_pct={:.1} per_phase={}",
        trace.name,
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
