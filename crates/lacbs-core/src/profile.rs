//! Profile：把"策略 + 参数"打包成可命名、可回归、可切换的整数档位。
//! 自适应控制器只在 `ProfileId::ALL` 上做选择，不碰连续参数。

use crate::model::Config;
use crate::policy::Policy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProfileId {
    P0,
    P1,
    P2,
    P3,
    P4,
}

impl ProfileId {
    pub const ALL: [ProfileId; 5] = [
        ProfileId::P0,
        ProfileId::P1,
        ProfileId::P2,
        ProfileId::P3,
        ProfileId::P4,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::P0 => "P0-fifo",
            Self::P1 => "P1-edf",
            Self::P2 => "P2-preempt",
            Self::P3 => "P3-reject",
            Self::P4 => "P4-aging",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "P0" | "p0" | "fifo" => Some(Self::P0),
            "P1" | "p1" | "edf" => Some(Self::P1),
            "P2" | "p2" | "preempt" => Some(Self::P2),
            "P3" | "p3" | "reject" => Some(Self::P3),
            "P4" | "p4" | "aging" => Some(Self::P4),
            _ => None,
        }
    }

    /// 以 `base` 的成本/容量为底，叠加该 profile 的策略开关。
    pub fn apply(self, base: Config) -> (Policy, Config) {
        match self {
            Self::P0 => (
                Policy::Fifo,
                Config {
                    chunk_tokens: 0,
                    reject_hopeless: false,
                    preempt_for_interactive: false,
                    aging_factor: None,
                    ..base
                },
            ),
            Self::P1 => (
                Policy::Deadline,
                Config {
                    chunk_tokens: 128,
                    reject_hopeless: false,
                    preempt_for_interactive: false,
                    aging_factor: None,
                    ..base
                },
            ),
            Self::P2 => (
                Policy::Deadline,
                Config {
                    chunk_tokens: 128,
                    reject_hopeless: false,
                    preempt_for_interactive: true,
                    aging_factor: None,
                    ..base
                },
            ),
            Self::P3 => (
                Policy::Deadline,
                Config {
                    chunk_tokens: 128,
                    reject_hopeless: true,
                    preempt_for_interactive: true,
                    aging_factor: None,
                    ..base
                },
            ),
            Self::P4 => (
                Policy::Deadline,
                Config {
                    chunk_tokens: 128,
                    reject_hopeless: true,
                    preempt_for_interactive: true,
                    aging_factor: Some(3),
                    ..base
                },
            ),
        }
    }
}
