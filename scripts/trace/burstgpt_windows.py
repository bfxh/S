#!/usr/bin/env python3
"""从 BurstGPT 原始 CSV 抽窗口，输出回放用的精简 CSV。

输出列：arrival_s,prompt_tokens,output_tokens,class
- class: 按响应长度中位数切成 interactive（短）/ batch（长）。这是建模假设，不是 trace 自带标签。
- 时间按窗口起点归零，单位秒（Rust 侧再做缩放与微秒转换）。

用法：
  python scripts/trace/burstgpt_windows.py --src vendor/BurstGPT/data/BurstGPT_1.csv \
      --centers 235,350,1236 --hours 4 --out-dir vendor/burstgpt
"""

import argparse
import csv
import os
import statistics


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--centers", required=True, help="窗口中心（小时序号，逗号分隔）")
    ap.add_argument("--hours", type=int, default=4)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--stride", type=int, default=1, help="抽样步长（1=全量）")
    args = ap.parse_args()

    centers = [int(x) for x in args.centers.split(",")]
    spans = [(c - args.hours // 2, c + args.hours // 2) for c in centers]
    buckets = {s: [] for s in spans}

    with open(args.src, newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            t = int(row["Timestamp"])
            h = t // 3600
            for (lo, hi) in spans:
                if lo <= h < hi:
                    buckets[(lo, hi)].append(
                        (t, int(row["Request tokens"]), int(row["Response tokens"]))
                    )
                    break

    os.makedirs(args.out_dir, exist_ok=True)
    for (lo, hi) in spans:
        rows = buckets[(lo, hi)]
        if not rows:
            print(f"WARN 窗口 {lo}-{hi}h 没有数据")
            continue
        rows.sort()
        if args.stride > 1:
            rows = rows[:: args.stride]
        outs = [o for _, _, o in rows]
        med = statistics.median(outs)
        t0 = rows[0][0]
        path = os.path.join(args.out_dir, f"window_{lo}_{hi}.csv")
        with open(path, "w", newline="", encoding="utf-8") as g:
            w = csv.writer(g)
            w.writerow(["arrival_s", "prompt_tokens", "output_tokens", "class"])
            for t, p, o in rows:
                cls = "interactive" if o <= med else "batch"
                w.writerow([t - t0, p, o, cls])
        rate = len(rows) / ((hi - lo) * 3600)
        print(
            f"{path}: rows={len(rows)} span_s={rows[-1][0]-t0} rate={rate:.2f}/s "
            f"median_out={med} avg_out={sum(outs)/len(outs):.0f}"
        )


if __name__ == "__main__":
    main()
