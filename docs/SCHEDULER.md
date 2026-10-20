# 调度器内核 v0：模型与实测

纯确定性模拟，不碰 tokio、不碰 GPU。所有时间都是整数微秒，同一 seed 逐位可复算。

## 模型

一个 step = 一次批处理迭代：

```
cost = step_overhead + prefill_tokens × p_cost + decode_seqs × d_cost
```

- 每步**先保底 decode**（每个在解码的序列占 1 个 token 预算），剩余预算才给 prefill。
- prefill 可以切块跨 step 续跑；切块上限由策略决定。
- 序列完成 prefill 的那一步产出首字（计 1 个 token）；之后每步 1 个 token。
- 容量约束：`max_seqs`（并发槽位）与 `max_step_tokens`（每步 token 预算）。

默认成本：`p_cost=1µs/token`、`d_cost=20µs/seq`、`overhead=50µs`、`max_seqs=16`、`max_step_tokens=2048`。
这组数是**合成值**，只保证内部自洽，不对应任何真实硬件。

## 两种策略

| 策略 | 准入顺序 | prefill |
|---|---|---|
| `fifo` | 先到先服务 | 不切块，能吃多少吃多少 |
| `deadline` | 首字截止期最早者优先（EDF） | 按 `chunk_tokens` 切块 |

负载：24 个平稳到达 + 96 个在 200 ms 处齐射；约 1/3 是交互式（首字预算 15 ms、输出短），
其余是批处理（首字预算 300 ms、输出长）。

## 实测（seed 7，默认配置）

| 指标 | fifo | deadline |
|---|---|---|
| 交互式首字违约 | 30 / 45 | **11 / 45** |
| 交互式首字 p99 | 198 ms | **22.8 ms** |
| 批处理首字违约 | 0 / 75 | 0 / 75 |
| 批处理首字 p99 | 197.6 ms | 193.1 ms |
| makespan | 431.9 ms | 431.7 ms |
| 每步 token 峰值 | 2048 | 1816 |

跨 seed 稳定（交互式违约 fifo→deadline）：seed 11 = 33→13，seed 23 = 23→5，seed 42 = 19→0。

## 归因：赢在排序，不在切块

`--policy deadline --chunk 0`（只排序、不切块）得到 **11 / 45、p99 22.865 ms**，与切块版（11 / 45、22.835 ms）
在统计上无差别。**当前负载下切块没有贡献**——它的价值要等 prefill 长到能独占整步时才会显现，现在没有证据。

## 限制（别把这里的数字当产品结论）

- 没有抢占、没有拒绝/降级，超载时只能排队。
- SLO 是合成常数；真实流量里 p99 与突发形状完全不同。
- 成本模型是线性的，没有 attention 的平方项、没有 KV cache 上限、没有显存约束。
- Windows 定时器粒度与真实服务无关：这里的时间是模型时间，不是墙上时间。

## 复跑

```bash
cargo test -p lacbs-core                                  # 不变量 + 跨 seed 策略对比
cargo run -p lacbs-sim -- --policy fifo
cargo run -p lacbs-sim -- --policy deadline
cargo run -p lacbs-sim -- --policy deadline --chunk 0     # 归因：去掉切块
```

判红的断言在 `crates/lacbs-core/tests/sim.rs::deadline_policy_cuts_interactive_misses`：
4 个 seed 全部要求 `deadline < fifo`，且批处理违约必须为 0。
