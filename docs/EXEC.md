# 执行层 v0：内核策略跑在 tokio 任务上

> 目的：把模拟器里的策略（EDF 准入 / 无望即拒）落到真实异步任务上，
> 让 `tokio-console` / `dial9` / liveliness 这些工具第一次照到真实实现，而不是只照合成 harness。

## 结构

```
到达发生器（tokio task）──▶ 调度器循环（每 step_ms 一个 tick）
                              │  1. 收新请求，spawn 一个 job task（等准入）
                              │  2. 拒绝无望（now ≥ deadline）
                              │  3. 按 EDF / FIFO 排序，填满 max_seqs
                              │  4. 给每个在跑 job 发一个 Step 许可
                              └▶ job task：收到 Admit → 做一次 prefill 计算；
                                          每收一个 Step → 做一次 decode 计算并出词
```

- 一个 tick = 一次引擎迭代；`--step-ms` 默认 8 ms。
- 槽位回收靠 job task 退出后关闭通道（`Sender::is_closed`）。
- 无望判据与内核一致：当前时刻已过首字截止期即拒绝。

## 用法

```bash
cargo run -p lacbs-exec -- --secs 3 --tasks 256 --max-seqs 16
# 观测（另开终端）
cargo build -p lacbs-exec --features console
cargo run -p lacbs-exec --features console -- --secs 8 --tasks 192
tokio-console
```

## 实测

| 场景 | 结果 |
|---|---|
| 48 任务 / 3 s（轻载） | 48 完成、0 拒绝、p99 TTFT 0.8 ms、max_running 5 |
| 256 任务 / 3 s（突发） | 247 完成、9 拒绝、246 达标、p99 119 ms（SLO 120 ms）、max_running 16、0 停滞 |
| console 探针（192 任务） | 87 个不同任务、2,552 次 poll、1,720 次 wake 被观测到 |

## 与内核的关系

- 复用 `lacbs-core` 的请求类型与 SLO 定义；**调度决策是重写的 8 行逻辑**，没有直接调用 `sim::run`
  （后者是确定性模拟器，时间由成本模型推进）。
- 因此这里的数字是**墙上时间**，会和模拟器的模型时间不一致；两边不要混着比。

## 已知边界

- **抢占未接入**：v0 只做准入与拒绝；抢占需要取消任务并保序重排，留给下一版。
- 只有一个类别（全部 interactive），没有类别混合与优先级。
- 计算用 `spin` 模拟，不是真模型；`WORK_US=200` 是常数。
- 到达发生器是均匀投放，不带突发形状；突发场景靠把 `--tasks` 调大来构造。
- 门的判据只有"全部交代 + 无停滞"，不涉及延迟优劣。
