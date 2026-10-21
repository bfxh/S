#!/usr/bin/env python3
"""SLO 灵敏度扫描：固定工作负载，扫多个批处理 SLO × 多个 victim，输出中位数。

用法：
  py -3.11 scripts/slo_sweep.py --samples 3 \
      --slos 150000,250000,400000 --victims longest,hybrid \
      -- --arrivals-csv vendor/servegen_traces/A1_language_m-large.csv \
         --slice-start-s 3060 --slice-s 60 --target-load 1.0 --swap --swap-slots 8 --hybrid-alpha 0.25

输出列：slo_ms victim met_median met_min met_max preempt_median
"""

import argparse
import json
import statistics
import subprocess
import sys
from pathlib import Path


def run_once(bin_path: str, extra: list[str]) -> dict:
    out = subprocess.run([bin_path, *extra], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=180)
    lines = [l for l in out.stdout.splitlines() if l.startswith("{")]
    if not lines:
        raise RuntimeError(f"无输出 rc={out.returncode}: {out.stderr[:300]}")
    return json.loads(lines[-1])


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", type=int, default=3)
    ap.add_argument("--slos", default="100000,200000,400000",
                    help="批处理 SLO，单位微秒，逗号分隔")
    ap.add_argument("--victims", default="longest,hybrid")
    ap.add_argument("--bin", default=None)
    ap.add_argument("args", nargs=argparse.REMAINDER)
    ns = ap.parse_args()

    extra = [a for a in ns.args if a != "--"]
    bin_path = ns.bin
    if bin_path is None:
        exe = "exec.exe" if sys.platform.startswith("win") else "exec"
        bin_path = str(Path(__file__).resolve().parent.parent / "target" / "debug" / exe)

    slos = [int(x) for x in ns.slos.split(",") if x]
    victims = [v for v in ns.victims.split(",") if v]

    print("slo_ms victim met_median met_min met_max preempt_median")
    for slo in slos:
        for victim in victims:
            mets, pres = [], []
            for _ in range(ns.samples):
                rec = run_once(
                    bin_path,
                    [*extra, "--batch-slo-us", str(slo), "--victim", victim],
                )
                mets.append(rec["met_ttft"])
                pres.append(rec["preemptions"])
            mets.sort()
            print(
                f"{slo // 1000:6d} {victim:10s} "
                f"{statistics.median(mets):10.1f} {mets[0]:7d} {mets[-1]:7d} "
                f"{statistics.median(pres):14.1f}"
            )
            sys.stdout.flush()


if __name__ == "__main__":
    main()
