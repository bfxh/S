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
    };

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().context(format!("{flag} 缺值"));
        match flag.as_str() {
            "--policy" => {
                let v = val()?;
                policy = Policy::parse(&v).with_context(|| format!("未知策略 {v}"))?;
            }
            "--seed" => spec.seed = val()?.parse()?,
            "--steady" => spec.steady = val()?.parse()?,
            "--burst" => spec.burst = val()?.parse()?,
            "--interval-us" => spec.interval_us = val()?.parse()?,
            "--burst-at-us" => spec.burst_at_us = val()?.parse()?,
            "--interactive-pct" => spec.interactive_pct = val()?.parse()?,
            "--i-ttft-us" => spec.i_ttft_us = val()?.parse()?,
            "--b-ttft-us" => spec.b_ttft_us = val()?.parse()?,
            "--max-seqs" => cfg.max_seqs = val()?.parse()?,
            "--max-step-tokens" => cfg.max_step_tokens = val()?.parse()?,
            "--chunk" => cfg.chunk_tokens = val()?.parse()?,
            other => bail!("未知参数: {other}"),
        }
    }

    let reqs = workload::generate(&spec);
    let report = lacbs_core::sim::run(cfg, policy, reqs.clone()).map_err(|e| anyhow::anyhow!(e))?;

    println!(
        concat!(
            "{{\"policy\":\"{}\",\"requests\":{},\"steps\":{},\"makespan_us\":{},",
            "\"ttft_p50\":{},\"ttft_p99\":{},\"tpot_p50\":{},\"tpot_p99\":{},",
            "\"ttft_misses\":{},\"tpot_misses\":{},\"met_both\":{},",
            "\"max_step_tokens_seen\":{},\"total_step_tokens\":{},",
            "\"interactive\":{{\"n\":{},\"ttft_p99\":{},\"ttft_misses\":{},\"met_both\":{}}},",
            "\"batch\":{{\"n\":{},\"ttft_p99\":{},\"ttft_misses\":{},\"met_both\":{}}}}}"
        ),
        policy.as_str(),
        report.finished.len(),
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
