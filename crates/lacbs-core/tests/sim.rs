use lacbs_core::model::{Class, Config};
use lacbs_core::policy::Policy;
use lacbs_core::sim::run;
use lacbs_core::workload::{self, Spec};

fn cfg() -> Config {
    Config {
        max_seqs: 16,
        max_step_tokens: 2048,
        prefill_us_per_token: 1,
        decode_us_per_seq: 20,
        step_overhead_us: 50,
        chunk_tokens: 128,
        reject_hopeless: false,
        preempt_for_interactive: false,
    }
}

fn workload() -> Vec<lacbs_core::Request> {
    workload::generate(&Spec::default())
}

#[test]
fn same_input_bitwise_same_report() {
    let a = run(cfg(), Policy::Deadline, workload()).unwrap();
    let b = run(cfg(), Policy::Deadline, workload()).unwrap();
    assert_eq!(a, b, "同 seed 同配置必须逐位一致");
}

#[test]
fn capacity_never_exceeded() {
    for policy in [Policy::Fifo, Policy::Deadline] {
        let r = run(cfg(), policy, workload()).unwrap();
        assert!(
            r.max_step_tokens_seen <= cfg().max_step_tokens,
            "{policy:?} 突破了 token 预算: {} > {}",
            r.max_step_tokens_seen,
            cfg().max_step_tokens
        );
    }
}

#[test]
fn every_request_finishes_once() {
    let reqs = workload();
    for policy in [Policy::Fifo, Policy::Deadline] {
        let r = run(cfg(), policy, reqs.clone()).unwrap();
        assert_eq!(r.finished.len(), reqs.len(), "{policy:?} 有请求没跑完");
        assert!(r.steps > 0);
        let mut ids: Vec<u32> = r.finished.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), reqs.len(), "{policy:?} 有重复或缺失的 id");
    }
}

#[test]
fn latency_terms_have_floor() {
    let r = run(cfg(), Policy::Deadline, workload()).unwrap();
    for f in &r.finished {
        assert!(f.ttft_us > 0, "id={} ttft 为 0，说明没算计算成本", f.id);
        assert!(f.finish_us >= f.arrival_us, "id={} 完成早于到达", f.id);
        if f.tpot_us > 0 {
            assert!(
                f.tpot_us >= cfg().decode_us_per_seq,
                "id={} tpot {} 低于单个 decode step 成本",
                f.id,
                f.tpot_us
            );
        }
    }
}

#[test]
fn invalid_config_is_rejected() {
    let mut bad = cfg();
    bad.max_step_tokens = 4;
    bad.max_seqs = 16;
    let err = run(bad, Policy::Fifo, workload()).unwrap_err();
    assert!(
        err.contains("max_step_tokens"),
        "错误信息应点名容量矛盾: {err}"
    );
}

/// 异质 SLO 下，按截止期排序必须减少交互式首字违约，且不许把批处理拖下水。
/// 4 个 seed 都要成立；参数一改就重测，别拿单点读数当结论。
#[test]
fn deadline_policy_cuts_interactive_misses() {
    for seed in [7, 11, 23, 42] {
        let spec = Spec {
            seed,
            ..Spec::default()
        };
        let reqs = workload::generate(&spec);
        let fifo = run(cfg(), Policy::Fifo, reqs.clone()).unwrap();
        let edf = run(cfg(), Policy::Deadline, reqs).unwrap();

        let f = fifo.class_ttft_misses(Class::Interactive);
        let e = edf.class_ttft_misses(Class::Interactive);
        assert!(
            f > 0,
            "seed {seed}: FIFO 都没违约，这个负载测不出差别（先加负载）"
        );
        assert!(e < f, "seed {seed}: 截止期策略没赢，fifo={f} deadline={e}");
        assert_eq!(
            edf.class_ttft_misses(Class::Batch),
            0,
            "seed {seed}: 批处理被排序牺牲了"
        );
    }
}

fn flood_cfg() -> Config {
    Config {
        decode_us_per_seq: 500,
        ..cfg()
    }
}

fn flood_spec(late: u32) -> Spec {
    Spec {
        steady: 16,
        steady_class: Some(Class::Batch),
        burst: 0,
        interval_us: 1_000,
        burst_at_us: 100_000,
        late_interactive: late,
        b_output: (128, 256),
        i_ttft_us: 25_000,
        i_tpot_us: 20_000,
        ..Spec::default()
    }
}

#[test]
fn rejection_only_drops_requests_that_would_have_missed() {
    let reqs = workload();
    let mut c = cfg();
    c.reject_hopeless = true;
    let r = run(c, Policy::Deadline, reqs.clone()).unwrap();

    assert!(!r.rejected.is_empty(), "过载场景应当有人被拒");
    assert_eq!(
        r.finished.len() + r.rejected.len(),
        reqs.len(),
        "有请求既没完成也没被拒"
    );
    let mut ids: Vec<u32> = r.finished.iter().map(|f| f.id).collect();
    ids.extend(r.rejected.iter().map(|x| x.id));
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), reqs.len(), "同一请求同时出现在完成与拒绝里");
    assert_eq!(r.ttft_misses(), 0, "拒绝之后仍有人首字超时，拒绝规则没对齐");
}

#[test]
fn preemption_protects_late_interactive() {
    let reqs = workload::generate(&flood_spec(4));
    let base = run(flood_cfg(), Policy::Deadline, reqs.clone()).unwrap();
    let mut c = flood_cfg();
    c.preempt_for_interactive = true;
    let pre = run(c, Policy::Deadline, reqs).unwrap();

    assert_eq!(base.class_ttft_misses(Class::Interactive), 4);
    assert_eq!(pre.class_ttft_misses(Class::Interactive), 0);
    assert!(pre.preemptions > 0, "应该发生抢占");
    assert!(pre.recomputed_tokens > 0, "抢占必须付出重算代价");
    assert_eq!(
        pre.class_ttft_misses(Class::Batch),
        0,
        "批处理为抢占买单到超时了"
    );
}

#[test]
fn preemption_is_a_noop_when_nobody_waits() {
    let spec = Spec {
        steady: 4,
        burst: 0,
        late_interactive: 0,
        ..Spec::default()
    };
    let reqs = workload::generate(&spec);
    let mut c = cfg();
    c.preempt_for_interactive = true;
    let r = run(c, Policy::Fifo, reqs).unwrap();
    assert_eq!(r.preemptions, 0, "闲置时不该有抢占");
}
