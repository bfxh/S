//! 三值准入决策表的门禁。
//!
//! fixture 由 `scripts/gen_ternary_fixture.py` 调用 tla 生成（K3/B3 各一份），不是手写；
//! 这里只断言语义性质，不重实现三值求值器。

const K3: &str = include_str!("fixtures/ternary_admission_k3.csv");
const B3: &str = include_str!("fixtures/ternary_admission_b3.csv");

type Row = (String, String, String, String);

fn parse(src: &str) -> Vec<Row> {
    let rows: Vec<Row> = src
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut it = l.split(',');
            (
                it.next().unwrap().trim().to_string(),
                it.next().unwrap().trim().to_string(),
                it.next().unwrap().trim().to_string(),
                it.next().unwrap().trim().to_string(),
            )
        })
        .collect();
    assert_eq!(rows.len(), 27, "fixture 必须是 3^3=27 行");
    rows
}

fn counts(rows: &[Row]) -> (usize, usize, usize) {
    let known = || rows.iter().filter(|r| r.0 != "X");
    (
        known().filter(|r| r.3 == "1").count(),
        known().filter(|r| r.3 == "0").count(),
        known().filter(|r| r.3 == "X").count(),
    )
}

#[test]
fn k3_hard_constraint_survives_unknown_hopelessness() {
    let rows = parse(K3);
    let mut infeasible_rows = 0;
    for (i, h, c, reject) in &rows {
        if i == "1" {
            assert_eq!(*reject, "1", "KV 装不下必须确定拒绝（h={h}, c={c}）");
            infeasible_rows += 1;
        }
    }
    assert_eq!(infeasible_rows, 9, "i=1 的行数不对");
    assert_eq!(
        rows.iter()
            .find(|r| r.0 == "X" && r.1 == "1" && r.2 == "1")
            .map(|r| r.3.as_str()),
        Some("1"),
        "K3 下“确定无望+竞争”应压过未知的 infeasible"
    );
}

#[test]
fn k3_never_rejects_without_contention_or_proof() {
    let rows = parse(K3);
    for (i, h, c, reject) in &rows {
        if i == "0" && c == "0" {
            assert_eq!(*reject, "0", "有空槽且装得下时不得拒（h={h}）");
        }
        if i == "0" && reject == "1" {
            assert_eq!(
                (h.as_str(), c.as_str()),
                ("1", "1"),
                "拒绝必须是“确定无望 + 有竞争”"
            );
        }
    }
    assert_eq!(counts(&rows), (10, 5, 3), "K3 的 18 行计数变了");
}

#[test]
fn b3_poisons_the_hard_constraint() {
    let rows = parse(B3);
    let poisoned = rows
        .iter()
        .find(|r| r.0 == "1" && r.1 == "X" && r.2 == "0")
        .expect("B3 fixture 缺 i=1,h=X,c=0 行");
    assert_eq!(
        poisoned.3, "X",
        "B3 应把“KV 装不下 + 未知无望”毒成未知（这正是不能用 B3 的反证）"
    );
    assert_eq!(counts(&rows), (5, 3, 10), "B3 的 18 行计数变了");
}
