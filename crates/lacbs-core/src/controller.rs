//! M2：窗口级规则控制器 + 安全层。
//! 只选 profile（P0–P4），不碰成本模型；切换受冷却与滞回约束。

use crate::profile::ProfileId;

#[derive(Debug, Clone)]
pub struct WindowStats {
    pub index: u64,
    pub t_start: u64,
    pub t_end: u64,
    pub arrivals: usize,
    pub finished: usize,
    pub met: usize,
    pub rejected: usize,
    pub recompute_delta: u64,
    pub max_wait_us: u64,
    pub queue_len: usize,
}

impl WindowStats {
    pub fn reject_rate(&self) -> f64 {
        let total = self.finished + self.rejected;
        if total == 0 {
            0.0
        } else {
            self.rejected as f64 / total as f64
        }
    }
}

pub trait Controller {
    fn choose(&mut self, w: &WindowStats) -> ProfileId;
}

/// 规则表：按"最大等待 + 拒绝率"选档，带滞回（连续 2 窗一致）与冷却（至少隔 1 窗）。
pub struct RuleController {
    current: ProfileId,
    pending: Option<(ProfileId, u32)>,
    last_switch: Option<u64>,
    pub switches: u64,
    pub cooldown_windows: u64,
    /// 超过它视为过载，用 P4（拒绝 + 老化）
    pub overload_wait_us: u64,
    /// 低于它视为空闲，用 P0（不拒绝、晚服务）
    pub calm_wait_us: u64,
    /// 切换所需连续窗口数
    pub confirm_windows: u32,
    /// 见过的最大"本窗处理量"，用来估计容量（自适应，不假设已知）
    capacity_est: f64,
}

impl Default for RuleController {
    fn default() -> Self {
        Self::new(ProfileId::P1)
    }
}

impl RuleController {
    pub fn new(initial: ProfileId) -> Self {
        Self {
            current: initial,
            pending: None,
            last_switch: None,
            switches: 0,
            cooldown_windows: 2,
            overload_wait_us: 1_000_000,
            calm_wait_us: 200_000,
            confirm_windows: 3,
            capacity_est: 0.0,
        }
    }

    pub fn current(&self) -> ProfileId {
        self.current
    }

    fn target(&mut self, w: &WindowStats) -> ProfileId {
        if w.arrivals == 0 && w.finished == 0 && w.rejected == 0 {
            return self.current;
        }
        // 只走"拒绝"与"不拒绝"两端：ServeGen 六条轨迹里逐阶段最优只在这两端出现。
        // 用"历史最大处理量"估容量，再算利用率；队列长度会被"待拒绝的无望请求"污染，不能用。
        let handled = (w.finished + w.rejected) as f64;
        if handled > self.capacity_est {
            self.capacity_est = handled;
        }
        let util = if self.capacity_est <= 1.0 {
            1.0
        } else {
            w.arrivals as f64 / self.capacity_est
        };
        if util > 0.95 || w.max_wait_us > self.overload_wait_us {
            return ProfileId::P4;
        }
        if util < 0.5 {
            return ProfileId::P0;
        }
        self.current
    }
}

impl Controller for RuleController {
    fn choose(&mut self, w: &WindowStats) -> ProfileId {
        let target = self.target(w);
        if std::env::var_os("LACBS_CTL_DEBUG").is_some() {
            eprintln!(
                "win={} arr={} fin={} rej={} q={} wait_ms={:.0} rate={:.2} target={:?} cur={:?}",
                w.index,
                w.arrivals,
                w.finished,
                w.rejected,
                w.queue_len,
                w.max_wait_us as f64 / 1000.0,
                if w.finished + w.rejected == 0 {
                    0.0
                } else {
                    w.arrivals as f64 / (w.finished + w.rejected) as f64
                },
                target,
                self.current
            );
        }
        if target == self.current {
            self.pending = None;
            return self.current;
        }
        let n = match self.pending {
            Some((p, n)) if p == target => n + 1,
            _ => 1,
        };
        self.pending = Some((target, n));

        let cooled = self
            .last_switch
            .is_none_or(|last| w.index.saturating_sub(last) >= self.cooldown_windows);
        if n >= self.confirm_windows && cooled {
            self.current = target;
            self.pending = None;
            self.last_switch = Some(w.index);
            self.switches += 1;
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(index: u64, arrivals: usize, finished: usize, rejected: usize) -> WindowStats {
        WindowStats {
            index,
            t_start: 0,
            t_end: 0,
            arrivals,
            finished,
            met: finished,
            rejected,
            recompute_delta: 0,
            max_wait_us: 0,
            queue_len: 0,
        }
    }

    #[test]
    fn switches_to_p4_after_sustained_overload() {
        let mut c = RuleController::new(ProfileId::P0);
        assert_eq!(c.choose(&win(0, 100, 10, 0)), ProfileId::P0);
        assert_eq!(c.choose(&win(1, 100, 10, 0)), ProfileId::P0);
        assert_eq!(c.choose(&win(2, 100, 10, 0)), ProfileId::P4);
        assert_eq!(c.switches, 1);
    }

    #[test]
    fn switches_to_p0_when_clearly_underloaded() {
        let mut c = RuleController::new(ProfileId::P4);
        assert_eq!(c.choose(&win(0, 1, 10, 0)), ProfileId::P4);
        assert_eq!(c.choose(&win(1, 1, 10, 0)), ProfileId::P4);
        assert_eq!(c.choose(&win(2, 1, 10, 0)), ProfileId::P0);
    }

    #[test]
    fn stays_put_in_the_middle_band() {
        let mut c = RuleController::new(ProfileId::P0);
        for i in 0..5 {
            assert_eq!(c.choose(&win(i, 5, 10, 0)), ProfileId::P0);
        }
        assert_eq!(c.switches, 0);
    }
}
