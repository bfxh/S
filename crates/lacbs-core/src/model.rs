//! 请求与成本模型：全部整数微秒，保证同一输入逐位可复算。

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// 交互式：短输出、紧首字预算。
    Interactive,
    /// 批处理：长输出、松预算。
    Batch,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Batch => "batch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub id: u32,
    pub class: Class,
    pub arrival_us: u64,
    pub prompt_tokens: u32,
    pub output_tokens: u32,
    /// 首字时间上限（从到达到第一个输出 token）。
    pub ttft_slo_us: u64,
    /// 平均每 token 间隔上限。
    pub tpot_slo_us: u64,
}

impl Request {
    pub fn ttft_deadline(&self) -> u64 {
        self.arrival_us + self.ttft_slo_us
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub max_seqs: usize,
    pub max_step_tokens: u32,
    /// 每个 prefill token 的计算成本。
    pub prefill_us_per_token: u64,
    /// 每个 decode 序列每步的计算成本（每步产出 1 token）。
    pub decode_us_per_seq: u64,
    /// 每步固定开销（kernel launch 一类）。
    pub step_overhead_us: u64,
    /// 单次 prefill 的切块上限；0 表示不切块。
    pub chunk_tokens: u32,
    /// 到服务时已赶不上首字截止期的请求，直接拒绝而不是硬跑。
    pub reject_hopeless: bool,
    /// 交互式请求可挤掉批处理序列（KV 丢弃、恢复时重算 prefill）。
    pub preempt_for_interactive: bool,
}

impl Config {
    /// 每步至少要能容纳"所有槽位同时 decode"，否则容量约束自相矛盾。
    pub fn validate(&self) -> Result<(), String> {
        if self.max_seqs == 0 {
            return Err("max_seqs 不能为 0".into());
        }
        if self.max_step_tokens < self.max_seqs as u32 {
            return Err(format!(
                "max_step_tokens({}) < max_seqs({})，decode 挤不进一个 step",
                self.max_step_tokens, self.max_seqs
            ));
        }
        if self.prefill_us_per_token == 0 || self.decode_us_per_seq == 0 {
            return Err("成本必须为正，否则时间不前进".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub id: u32,
    pub class: Class,
    pub arrival_us: u64,
    pub ttft_us: u64,
    pub finish_us: u64,
    pub tpot_us: u64,
    pub met_ttft: bool,
    pub met_tpot: bool,
}

impl Finished {
    pub fn met_both(&self) -> bool {
        self.met_ttft && self.met_tpot
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub id: u32,
    pub class: Class,
    pub arrival_us: u64,
    pub rejected_at_us: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub finished: Vec<Finished>,
    pub rejected: Vec<Rejected>,
    pub makespan_us: u64,
    pub max_step_tokens_seen: u32,
    pub steps: u64,
    /// 把 step 成本里的 token 数加起来，用来核对容量没被突破。
    pub total_step_tokens: u64,
    pub preemptions: u64,
    /// 被抢占导致要重算的 token 数（prompt + 已生成长度）。
    pub recomputed_tokens: u64,
}

impl Report {
    pub fn ttft_misses(&self) -> usize {
        self.finished.iter().filter(|f| !f.met_ttft).count()
    }

    pub fn tpot_misses(&self) -> usize {
        self.finished.iter().filter(|f| !f.met_tpot).count()
    }

    pub fn met_both(&self) -> usize {
        self.finished.iter().filter(|f| f.met_both()).count()
    }

    pub fn ttft_p(&self, p: f64) -> u64 {
        percentile(self.finished.iter().map(|f| f.ttft_us).collect(), p)
    }

    pub fn tpot_p(&self, p: f64) -> u64 {
        percentile(self.finished.iter().map(|f| f.tpot_us).collect(), p)
    }

    pub fn of_class(&self, class: Class) -> Vec<&Finished> {
        self.finished.iter().filter(|f| f.class == class).collect()
    }

    pub fn class_ttft_p(&self, class: Class, p: f64) -> u64 {
        percentile(self.of_class(class).iter().map(|f| f.ttft_us).collect(), p)
    }

    pub fn class_ttft_misses(&self, class: Class) -> usize {
        self.of_class(class).iter().filter(|f| !f.met_ttft).count()
    }

    pub fn class_met_both(&self, class: Class) -> usize {
        self.of_class(class).iter().filter(|f| f.met_both()).count()
    }

    pub fn rejected_of(&self, class: Class) -> usize {
        self.rejected.iter().filter(|r| r.class == class).count()
    }

    /// 已接受且两项 SLO 都满足的比例；被拒绝的不算服务成功。
    pub fn met_ratio(&self, total_requests: usize) -> f64 {
        if total_requests == 0 {
            return 0.0;
        }
        self.met_both() as f64 / total_requests as f64
    }
}

pub fn percentile(mut xs: Vec<u64>, p: f64) -> u64 {
    if xs.is_empty() {
        return 0;
    }
    xs.sort_unstable();
    let idx = ((p / 100.0) * (xs.len() - 1) as f64).round() as usize;
    xs[idx.min(xs.len() - 1)]
}
