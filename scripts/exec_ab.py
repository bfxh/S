#!/usr/bin/env python3
"""执行层 A/B 采样器：跑 N 次，输出中位数/最小/最大。

用法：
  py -3.11 scripts/exec_ab.py --samples 10 -- --secs 4 --tasks 256 \
      --interactive-pct 30 --swap --swap-slots 8 --victim slack --batch-slo-us 200000

只依赖标准库；退出码恒 0（统计工具，不做判定），判定放在调用方。
"""

import argparse
import json
import statistics
import subprocess
import sys
from pathlib import Path


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", type=int, default=10)
    ap.add_argument("--bin", default=None)
    ap.add_argument("args", nargs=argparse.REMAINDER)
    ns = ap.parse_args()

    extra = [a for a in ns.args if a != "--"]
    bin_path = ns.bin
    if bin_path is None:
        exe = "exec.exe" if sys.platform.startswith("win") else "exec"
        bin_path = str(Path(__file__).resolve().parent.parent / "target" / "debug" / exe)

    rows = []
    for i in range(ns.samples):
        out = subprocess.run(
            [bin_path, *extra], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=120
        )
        line = [l for l in out.stdout.splitlines() if l.startswith("{")]
        if not line:
            print(f"sample {i}: 无输出（rc={out.returncode}）", file=sys.stderr)
            continue
        rows.append(json.loads(line[-1]))

    if not rows:
        print("没有有效样本", file=sys.stderr)
        sys.exit(1)

    keys = ["completed", "rejected", "met_ttft", "ttft_p50_ms", "ttft_p99_ms",
            "preemptions", "swaps", "recompute_tokens"]
    print(f"samples={len(rows)} victim={rows[0].get('victim')} cfg={' '.join(extra)}")
    for k in keys:
        xs = sorted(float(r.get(k, 0)) for r in rows)
        med = statistics.median(xs)
        print(f"{k:18s} median={med:8.1f}  min={xs[0]:8.1f}  max={xs[-1]:8.1f}")


if __name__ == "__main__":
    main()
