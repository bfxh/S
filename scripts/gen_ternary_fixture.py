#!/usr/bin/env python3
"""从本地 trit 生成三值准入 fixture（K3 与 B3 各一份）。

用法：
  py -3.11 scripts/gen_ternary_fixture.py --trit D:/KF/trit/target/release/trit.exe

产物：crates/lacbs-core/tests/fixtures/ternary_admission_{k3,b3}.csv
fixture 是 trit 的输出，不是手写的；语义改动时重跑本脚本。
"""

import argparse
import csv
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXPR = "i | (h & c)"
OUT = ROOT / "crates/lacbs-core/tests/fixtures"
ALGS = {"kleene": "k3", "bochvar": "b3"}


def gen(tla: str, alg: str, path: Path) -> None:
    out = subprocess.run(
        [tla, "-a", alg, "table", EXPR, "-f", "json"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=True,
    )
    data = json.loads(out.stdout)
    with path.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow([*data["variables"], "reject"])
        w.writerows(data["rows"])


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--trit", default="trit", help="trit 可执行文件路径")
    ns = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    for alg, tag in ALGS.items():
        path = OUT / f"ternary_admission_{tag}.csv"
        gen(ns.trit, alg, path)
        print(path)


if __name__ == "__main__":
    main()
