# lacbs — 延迟感知的连续批处理调度器

Rust 实现。**当前阶段：观测面就绪，调度器本体尚未开始。**

本仓先解决"怎么看见调度行为"，再做调度算法。工具矩阵与实测证据见 [docs/TOOLING.md](docs/TOOLING.md)。

## 布局

```
crates/lacbs-obs          # 观测装配，一个工具一个 feature
crates/lacbs-harness      # 合成负载（decode / prefill / mixed / stall）
crates/lacbs-console-probe# tokio-console 的无头探针（可判红）
scripts/gate.sh           # 工具面门禁：真跑 + 断言
scripts/hud-smoke.sh      # WSL 里让 hud 抓一次执行器停滞
```

## 快速开始

```bash
# 基线
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

判据是双向的：停滞场景必须报 `stalls:1`，正常场景必须 `stalls:0`；探针连不上必须退出码 1。
反证已做：阈值改到 5s ⇒ `stalls:0`；探针打空端口 ⇒ rc=1。

## 构建约定

- `-j 1` 写在 `.cargo/config.toml`：本机内存紧，并发 rustc 会拖死桌面。
- `tokio_unstable` 全局开启（console 与 dial9 都依赖 tokio 的 runtime hooks）。
- Windows 上 `sleep` 粒度与定时器精度**未标定**；做延迟数字前先用基准校准，别拿裸读数下结论。
