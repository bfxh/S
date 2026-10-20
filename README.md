# lacbs — 延迟感知的连续批处理调度器

Rust 实现。**结论：非平稳负载下，单一静态 profile 在合成负载与 BurstGPT 真实窗口上都接近逐阶段 oracle
（聚合 ~1% / ≤0.1%）；ServeGen 生成负载 3 条里 2 条 ≥5%（合计 2/6，未达重启门槛），
自适应控制器方向维持结案**。仓内保留调度器内核、profile 评估母线、真实/生成 trace 回放与观测工具。
详见 [docs/M1-ORACLE.md](docs/M1-ORACLE.md)、[docs/REPLAY-TRACES.md](docs/REPLAY-TRACES.md)。

| 文档 | 内容 |
|---|---|
| [docs/RESEARCH-ADAPTIVE.md](docs/RESEARCH-ADAPTIVE.md) | 文献与项目分析：vLLM/Sarathi/Llumnix/DistServe/H-MAS，证据分级与空白 |
| [docs/ADAPTIVE-DESIGN.md](docs/ADAPTIVE-DESIGN.md) | 自适应设计：信号、profile 空间、控制器、安全不变量、评估与 kill criteria |
| [docs/M1-ORACLE.md](docs/M1-ORACLE.md) | M1/M1.1 实测与结案依据：纯达标口径 gap 0%，可用性口径聚合 ~1% |
| [docs/REPLAY-TRACES.md](docs/REPLAY-TRACES.md) | 真实/生成 trace 回放：BurstGPT 4 窗口 ≤0.1%，ServeGen 2/3 ≥5%，合计 2/6 |
| [docs/SCHEDULER.md](docs/SCHEDULER.md) | 现有内核模型与实测读数 |
| [docs/TOOLING.md](docs/TOOLING.md) | 观测工具矩阵与实测证据 |

## 布局

```
crates/lacbs-core          # 调度器内核：请求/成本模型、FIFO 与 EDF 策略、确定性模拟器
crates/lacbs-sim           # 场景跑分二进制（--policy fifo|deadline）
crates/lacbs-obs           # 观测装配，一个工具一个 feature
crates/lacbs-harness       # 合成负载（decode / prefill / mixed / stall）
crates/lacbs-console-probe # tokio-console 的无头探针（可判红）
scripts/gate.sh            # 全量门禁
scripts/hud-smoke.sh       # WSL 里让 hud 抓一次执行器停滞
```

## 快速开始

```bash
# 调度器内核：策略对比 / 拒绝 / 抢占
cargo run -p lacbs-sim -- --policy fifo
cargo run -p lacbs-sim -- --policy deadline
cargo run -p lacbs-sim -- --policy deadline --reject
cargo run -p lacbs-sim -- --policy deadline --scenario flood --late-interactive 4 --preempt
cargo test -p lacbs-core

# 基线负载
cargo run -p lacbs-harness -- --secs 5 --tasks 64 --mode mixed

# tokio-console（另开终端跑 TUI）
cargo build -p lacbs-harness --features lacbs-obs/console
./target/debug/harness.exe --secs 30 --mode mixed &
tokio-console

# dial9 飞行记录仪
cargo build -p lacbs-harness --features lacbs-obs/dial9
./target/debug/harness.exe --secs 5 --mode mixed
dial9 trace-shape extract traces/trace.0.bin traces/shape.json

# 执行器停滞看门狗
cargo build -p lacbs-harness --features lacbs-obs/liveliness
./target/debug/harness.exe --secs 5 --workers 4 --tasks 8 --mode stall

# hotpath
cargo build -p lacbs-harness --features hotpath
./target/debug/harness.exe --secs 3 --mode mixed
```

## 门禁

```bash
bash scripts/gate.sh              # 全量
bash scripts/gate.sh --skip-hotpath
```

判据是双向的：停滞场景必须报 `stalls:1`，正常场景必须 `stalls:0`；探针连不上必须退出码 1；
调度器内核在 4 个 seed 上都要求 `deadline` 的交互式违约严格少于 `fifo`，且批处理违约为 0；
拒绝开启后被服务者必须零首字违约；flood 场景下开启抢占后交互式违约必须归零。
反证已做：停滞阈值改到 5s ⇒ `stalls:0`；探针打空端口 ⇒ rc=1；把 EDF 退化成 FIFO ⇒ 内核测试判红。

## 构建约定

- `-j 1` 写在 `.cargo/config.toml`：本机内存紧，并发 rustc 会拖死桌面。
- `tokio_unstable` 全局开启（console 与 dial9 都依赖 tokio 的 runtime hooks）。
- Windows 上 `sleep` 粒度与定时器精度**未标定**；做延迟数字前先用基准校准，别拿裸读数下结论。
