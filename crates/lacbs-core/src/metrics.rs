//! 评估用纯函数：逐阶段 oracle 与最好固定策略的差距。
//! 单独拆出来是为了能脱离模拟器单测——"gap=0"必须证明尺子能报出非零值。

pub struct OracleGap {
    pub best_fixed: usize,
    pub oracle: usize,
    pub gap_pct: f64,
    pub per_phase_best: Vec<usize>,
}

/// `rows`：每个 profile 一行，列为各阶段的达标数。
pub fn oracle_gap(rows: &[Vec<usize>]) -> OracleGap {
    let phases = rows.first().map_or(0, |r| r.len());
    let best_fixed = rows.iter().map(|r| r.iter().sum()).max().unwrap_or(0);
    let mut per_phase_best = vec![0usize; phases];
    for pi in 0..phases {
        per_phase_best[pi] = rows.iter().map(|r| r[pi]).max().unwrap_or(0);
    }
    let oracle: usize = per_phase_best.iter().sum();
    let gap_pct = if oracle == 0 {
        0.0
    } else {
        (oracle - best_fixed) as f64 / oracle as f64 * 100.0
    };
    OracleGap {
        best_fixed,
        oracle,
        gap_pct,
        per_phase_best,
    }
}

#[cfg(test)]
mod tests {
    use super::oracle_gap;

    #[test]
    fn reports_nonzero_gap_when_phases_favor_different_profiles() {
        let g = oracle_gap(&[vec![10, 0], vec![0, 10]]);
        assert_eq!(g.best_fixed, 10);
        assert_eq!(g.oracle, 20);
        assert!((g.gap_pct - 50.0).abs() < 1e-9, "gap={}", g.gap_pct);
        assert_eq!(g.per_phase_best, vec![10, 10]);
    }

    #[test]
    fn reports_zero_when_one_profile_dominates_every_phase() {
        let g = oracle_gap(&[vec![1, 2], vec![5, 6]]);
        assert_eq!(g.best_fixed, 11);
        assert_eq!(g.oracle, 11);
        assert_eq!(g.gap_pct, 0.0);
    }

    #[test]
    fn empty_input_is_zero_not_nan() {
        let g = oracle_gap(&[]);
        assert_eq!(g.oracle, 0);
        assert_eq!(g.gap_pct, 0.0);
    }
}
