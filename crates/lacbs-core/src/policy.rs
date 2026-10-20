//! 两种准入策略。FIFO 是基线，Deadline 是"延迟感知"的最小形态：
//! 按首字截止期排序 + 切块 prefill（长 prefill 不许独吞整步）。

use crate::model::{Config, Request};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Fifo,
    Deadline,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fifo" => Some(Self::Fifo),
            "deadline" => Some(Self::Deadline),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::Deadline => "deadline",
        }
    }

    /// 在已到达到的候选里挑下一个准入者，返回其在 `candidates` 里的下标。
    pub fn pick(self, candidates: &[&Request]) -> Option<usize> {
        match self {
            Self::Fifo => (!candidates.is_empty()).then_some(0),
            Self::Deadline => candidates
                .iter()
                .enumerate()
                .min_by_key(|(_, r)| (r.ttft_deadline(), r.arrival_us, r.id))
                .map(|(i, _)| i),
        }
    }

    /// 单次 prefill 允许吃掉多少 token；0 表示"剩下多少吃多少"。
    pub fn chunk_limit(self, cfg: &Config) -> u32 {
        match self {
            Self::Fifo => 0,
            Self::Deadline => cfg.chunk_tokens,
        }
    }
}
