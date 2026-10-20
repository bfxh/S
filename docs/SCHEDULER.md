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

## 过载机制：拒绝与抢占

两个机制是**独立开关**（`Config::reject_hopeless` / `preempt_for_interactive`），默认关，方便归因。

### 拒绝（reject_hopeless）

每到一步，先检查等待池：**按当前时刻算，连自己的 prefill + 一个 decode step 都赶不上首字截止期**的请求，直接拒绝，不再占算力。

bursty 场景（seed 7，deadline 策略）：

| 指标 | 不拒绝 | 拒绝 |
|---|---|---|
| 交互式首字违约 | 11 | **0**（11 个被拒，全部是交互式） |
| 被拒数 | 0 | 11 / 120 |
| 被服务者达标数 | 109 | 109 |
| makespan | 431.7 ms | 422.9 ms |

**这不是"质量提升"**：达标总数没变（109），拒绝只是把注定失败的请求提前失败，并省下约 2% 的算力。
好处是显式的、可计量的失败，而不是悄悄拖到超时。

### 抢占（preempt_for_interactive）

交互式在等而槽位满时，一次腾出所有需要的槽位（不是每步腾一个），被挤掉的批处理序列丢弃 KV、
回到等待队列，恢复时重算 `prompt + 已生成` 个 token。

flood 场景（16 个批处理先占满槽位，8 ms/step，交互式 SLO 25 ms）：

| 迟到的交互式 | 指标 | 不抢占 | 抢占 |
|---|---|---|---|
| 4 个 | 交互式首字 p99 | 1.01 s | **18.8 ms** |
| 4 个 | 交互式首字违约 | 4 / 4 | **0 / 4** |
| 16 个 | 交互式首字 p99 | 1.33 s | **17.7 ms** |
| 16 个 | 交互式首字违约 | 16 / 16 | **0 / 16** |
| 4 / 16 个 | 批处理首字违约 | 0 | **0** |
| 4 / 16 个 | 重算 token | 0 | 1,501 / 5,468 |

代价是重算算力，不是批处理超时（批处理 SLO 300 ms，有足够余量吸收）。

### 一个反直觉的实测

默认成本模型一步只要 ~0.37 ms 时，**抢占一次都没触发**（0 次，延迟无变化）——
槽位几乎立刻自然腾出，挤掉别人毫无收益。把 `decode_us_per_seq` 提到 500 µs（一步约 8 ms）后，
抢占才从 1 s 降到 ~18 ms。

**结论：抢占的价值只在"step 成本与首字 SLO 同量级"时成立。** 先量 step 成本，再谈要不要抢占。

## 限制（别把这里的数字当产品结论）

- 抢占只支持"交互式挤批处理"一种方向，没有多级优先级、没有换出让位（swap）、没有部分 prefill 保全。
- 抢占代价按"prompt + 已生成 token 全部重算"计，真实系统的 prefix cache 会便宜很多。
- 拒绝规则只有一条（到服务时已无望），没有基于队列长度或预算的主动限流。
- SLO 是合成常数；真实流量里 p99 与突发形状完全不同。
- 成本模型是线性的，没有 attention 的平方项、没有 KV cache 上限、没有显存约束。
- Windows 定时器粒度与真实服务无关：这里的时间是模型时间，不是墙上时间。

## 复跑

```bash
cargo test -p lacbs-core                                  # 不变量 + 跨 seed 策略对比 + 拒绝/抢占
cargo run -p lacbs-sim -- --policy fifo
cargo run -p lacbs-sim -- --policy deadline
cargo run -p lacbs-sim -- --policy deadline --reject      # 拒绝无望请求
cargo run -p lacbs-sim -- --policy deadline --scenario flood --late-interactive 4            # 抢占基线
cargo run -p lacbs-sim -- --policy deadline --scenario flood --late-interactive 4 --preempt  # 抢占
cargo run -p lacbs-sim -- --policy deadline --chunk 0     # 归因：去掉切块
```

判红的断言在 `crates/lacbs-core/tests/sim.rs::deadline_policy_cuts_interactive_misses`：
4 个 seed 全部要求 `deadline < fifo`，且批处理违约必须为 0。
