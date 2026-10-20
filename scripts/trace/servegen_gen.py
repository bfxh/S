#!/usr/bin/env python3
"""用 ServeGen 生成带到达时间的在线负载（速率随时间变化），输出回放 CSV。

与 BurstGPT 的抽取脚本保持同一输出格式：arrival_s,prompt_tokens,output_tokens,class
class 仍按响应长度中位数切，便于横向比较。

用法：
  python scripts/trace/servegen_gen.py --root vendor/ServeGen \
      --category language --model m-large --duration 480 --out vendor/servegen/language.csv
"""

import argparse
import csv
import os
import statistics
import sys


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True)
    ap.add_argument("--category", default="language", choices=["language", "reason"])
    ap.add_argument("--model", default="m-large")
    ap.add_argument("--duration", type=float, default=3600.0)
    ap.add_argument("--out", required=True)
    ap.add_argument("--stride", type=int, default=1)
    ap.add_argument("--seed", type=int, default=42, help="固定种子，保证可复现")
    ap.add_argument("--shape", default="standard", choices=["standard", "alt"])
    args = ap.parse_args()

    out_path = os.path.abspath(args.out)  # 必须在 chdir 之前解析
    root = os.path.abspath(args.root)
    os.chdir(root)  # ServeGen 的数据路径是相对 cwd 的
    sys.path.insert(0, root)
    from servegen import Category, ClientPool
    from servegen.construct import generate_workload

    cat = Category.LANGUAGE if args.category == "language" else Category.REASON
    pool = ClientPool(cat, args.model)
    d = args.duration
    # ServeGen 的内部窗口是固定的（这里 600s 一片），rate_fn 的键必须与之一一对应
    windows = pool.span(0, d).get()
    stamps = sorted(set(w.timestamp for w in windows))
    shape = (
        [1.0, 3.5, 0.8, 2.5, 1.2, 4.0]
        if args.shape == "standard"
        else [0.5, 4.0, 1.5, 0.5, 4.0, 1.0]
    )
    rate_fn = {ts: 2.0 * shape[i % len(shape)] for i, ts in enumerate(stamps)}
    reqs = generate_workload(pool, rate_fn, duration=d, seed=args.seed)
    if not reqs:
        raise SystemExit("ServeGen 没有生成请求")

    def field(r, name):
        return int(r.data.get(name + "_tokens", r.data.get(name + "_length", 0)))

    outs = [field(r, "output") for r in reqs]
    med = statistics.median(outs)
    rows = []
    for r in reqs:
        p = field(r, "input")
        o = field(r, "output")
        rows.append((float(r.timestamp), p, o, "interactive" if o <= med else "batch"))
    rows.sort()
    if args.stride > 1:
        rows = rows[:: args.stride]

    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["arrival_s", "prompt_tokens", "output_tokens", "class"])
        w.writerows(rows)

    span = rows[-1][0] - rows[0][0]
    print(
        f"{out_path}: rows={len(rows)} span_s={span:.0f} rate={len(rows)/max(span,1):.2f}/s "
        f"median_out={med} avg_in={sum(r[1] for r in rows)/len(rows):.0f} "
        f"avg_out={sum(r[2] for r in rows)/len(rows):.0f}"
    )


if __name__ == "__main__":
    main()
