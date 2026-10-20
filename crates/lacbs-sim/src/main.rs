mod sweep;

use anyhow::{Context, bail};
use lacbs_core::model::{Class, Config};
use lacbs_core::policy::Policy;
use lacbs_core::workload::{self, Spec};

fn main() -> anyhow::Result<()> {
    let mut policy = Policy::Fifo;
    let mut spec = Spec::default();
    let mut cfg = Config {
        max_seqs: 16,
        max_step_tokens: 2048,
        prefill_us_per_token: 1,
        decode_us_per_seq: 20,
        step_overhead_us: 50,
        chunk_tokens: 128,
        reject_hopeless: false,
        preempt_for_interactive: false,
        aging_factor: None,
        kv_capacity_tokens: None,
        retry_prob_pct: 0,
        retry_delay_us: 0,
        max_attempts: 1,
        retry_seed: 1,
    };

    let mut sweep = false;
    let mut reject_penalty = 0.0f64;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().context(format!("{flag} 缺值"));
        match flag.as_str() {
            "--policy" => {
                let v = val()?;
                policy = Policy::parse(&v).with_context(|| format!("未知策略 {v}"))?;
            }
            "--scenario" => {
                let v = val()?;
                match v.as_str() {
                    // 默认：平稳 + 齐射，两类混跑
                    "bursty" => {}
                    // 批处理先占满所有槽位，交互式后才陆续到达
                    "flood" => {
                        spec.steady = 16;
                        spec.steady_class = Some(Class::Batch);
                        spec.burst = 0;
                        spec.interval_us = 1_000;
                        spec.burst_at_us = 100_000;
                        spec.late_interactive = 16;
                        spec.b_output = (128, 256);
                        // 8ms 一步 ⇒ 首字地板约 16–19ms，SLO 取 25ms 才分得出"挤"与"等"
                        spec.i_ttft_us = 25_000;
                        // decode 一步 8ms，tpot SLO 必须高于它，否则每条交互式都会在第二项上失败
                        spec.i_tpot_us = 20_000;
                        cfg.decode_us_per_seq = 500;
                    }
                    other => bail!("未知场景 {other}"),
                }
            }
            "--seed" => spec.seed = val()?.parse()?,
            "--steady" => spec.steady = val()?.parse()?,
            "--burst" => spec.burst = val()?.parse()?,
            "--interval-us" => spec.interval_us = val()?.parse()?,
            "--burst-at-us" => spec.burst_at_us = val()?.parse()?,
            "--late-interactive" => spec.late_interactive = val()?.parse()?,
            "--interactive-pct" => spec.interactive_pct = val()?.parse()?,
            "--i-ttft-us" => spec.i_ttft_us = val()?.parse()?,
            "--b-ttft-us" => spec.b_ttft_us = val()?.parse()?,
            "--max-seqs" => cfg.max_seqs = val()?.parse()?,
            "--max-step-tokens" => cfg.max_step_tokens = val()?.parse()?,
            "--chunk" => cfg.chunk_tokens = val()?.parse()?,
            "--decode-us" => cfg.decode_us_per_seq = val()?.parse()?,
            "--prefill-us" => cfg.prefill_us_per_token = val()?.parse()?,
            "--sweep" => sweep = true,
            "--reject-penalty" => reject_penalty = val()?.parse()?,
            "--kv-tokens" => cfg.kv_capacity_tokens = Some(val()?.parse()?),
            "--retry-pct" => cfg.retry_prob_pct = val()?.parse()?,
            "--retry-delay-us" => cfg.retry_delay_us = val()?.parse()?,
            "--max-attempts" => cfg.max_attempts = val()?.parse()?,
            "--reject" => cfg.reject_hopeless = true,
            "--preempt" => cfg.preempt_for_interactive = true,
            other => bail!("未知参数: {other}"),
        }
    }

    if sweep {
        return sweep::run(spec.seed, cfg, reject_penalty);
    }

    let reqs = workload::generate(&spec);
    let report = lacbs_core::sim::run(cfg, policy, reqs.clone()).map_err(|e| anyhow::anyhow!(e))?;

    println!(
        concat!(
            "{{\"policy\":\"{}\",\"requests\":{},\"steps\":{},\"makespan_us\":{},",
            "\"ttft_p50\":{},\"ttft_p99\":{},\"tpot_p50\":{},\"tpot_p99\":{},",
            "\"ttft_misses\":{},\"tpot_misses\":{},\"met_both\":{},",
            "\"max_step_tokens_seen\":{},\"total_step_tokens\":{},",
            "\"rejected\":{},\"rejected_interactive\":{},\"rejected_batch\":{},",
            "\"preemptions\":{},\"recomputed_tokens\":{},",
            "\"interactive\":{{\"n\":{},\"ttft_p99\":{},\"ttft_misses\":{},\"met_both\":{}}},",
            "\"batch\":{{\"n\":{},\"ttft_p99\":{},\"ttft_misses\":{},\"met_both\":{}}}}}"
        ),
        policy.as_str(),
        report.finished.len() + report.rejected.len(),
        report.steps,
        report.makespan_us,
        report.ttft_p(50.0),
        report.ttft_p(99.0),
        report.tpot_p(50.0),
        report.tpot_p(99.0),
        report.ttft_misses(),
        report.tpot_misses(),
        report.met_both(),
        report.max_step_tokens_seen,
        report.total_step_tokens,
        report.rejected.len(),
        report.rejected_of(Class::Interactive),
        report.rejected_of(Class::Batch),
        report.preemptions,
        report.recomputed_tokens,
        report.of_class(Class::Interactive).len(),
        report.class_ttft_p(Class::Interactive, 99.0),
        report.class_ttft_misses(Class::Interactive),
        report.class_met_both(Class::Interactive),
        report.of_class(Class::Batch).len(),
        report.class_ttft_p(Class::Batch, 99.0),
        report.class_ttft_misses(Class::Batch),
        report.class_met_both(Class::Batch),
    );
    Ok(())
}
