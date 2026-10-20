# 观测工具面：装了什么、在哪跑、怎么判红

所有"实测"都是本机 2026-10-21 跑出来的；没跑过的写"未测"，不拿文档口径冒充。

## 矩阵

| 工具 | 版本 | Windows | WSL2 | 采集到数据 |
|---|---|---|---|---|
| tokio-console | console-subscriber 0.5.0 / CLI 0.1.14 | ✅ | 未测 | ✅ 69 任务 / 24.2 万 poll |
| dial9 | 0.5.5（含 `dial9` CLI） | ✅ | 未测 | ✅ 37,337 poll，p99 3.01 ms |
| hotpath | 0.28.6 | ✅（基础档） | 未测 | ✅ `burn` 19,626 次 / p95 3 ms |
| async-liveliness-monitor | 0.1.1 | ✅ | 未测 | ✅ 停滞 401 ms 被抓到 |
| hud | v0.5.2（release v0.5.3） | ❌ 仅 Linux | ⚠️ 能跑但 0 样本 | ❌ PID 过滤全落空 |

## 逐项

### tokio-console
- 接线：`lacbs-obs` 的 `console` feature；`init_tracing()` 必须在 runtime 之前调用。
- 前提：`--cfg tokio_unstable`（已写进 `.cargo/config.toml`）。
- 人看：`tokio-console` 连 `127.0.0.1:6669`。
- 机器判：`console-probe`（`console-api` 的 gRPC 客户端）抓 3 秒更新，断言 `distinct_tasks > 0 && polls > 0`。
- 实测：`{"updates":6,"spawned":138,"distinct_tasks":69,"polls":242337,"wakes":242049}`。
- 开销：官方口径 5–10%，只在开发期开。

### dial9
- 接线：`dial9` feature；runtime 必须由 `lacbs_obs::build_runtime` 建（recorder 要抢在 runtime 前挂钩子）。
- 产物：`traces/trace.0.bin`（5 s / 756 KB；`max_total_size` 64 MiB、单文件 8 MiB 滚动）。
- 判据：`dial9 trace-shape extract` 出 JSON，`PollStartEvent > 100` 且 `worker_cardinality == 4`。
- 实测摘要：75,151 事件、37,337 次 poll、p50 50 µs、p90 3 ms、p99 3.01 ms、max 3.4 ms。
- 平台：核心 Tokio 事件在 Windows 可用；`cpu-profiling` / `linux-socket` / sched-switch 采样是 Linux 专有，未启用。

### hotpath
- 接线：`hotpath` feature；`#[cfg_attr(feature = "hotpath", hotpath::measure)]` 标在 `burn` 上，`init_hotpath("harness")` 的 guard 活到进程退出才出报告。
- 判据：报告里必须出现 `harness::burn`。
- 实测：19,626 次调用、avg 608 µs、p95 3 ms；4 个 tokio worker 各 ~99% CPU。
- 注意：`hotpath-cpu`（真 CPU 采样）**只支持 macOS/Linux**，Windows 只能拿计时 + 线程档。

### async-liveliness-monitor
- 接线：`liveliness` feature；监控任务在 runtime 里自唤醒打点，看门狗独占一个 OS 线程。
- 判据（双向）：`--mode stall`（每 worker 一个 1.2 s 同步阻塞）必须 `stalls:1`；`--mode mixed` 必须 `stalls:0`。
- 实测：阈值 400 ms 时在 401 ms 报 STALL，阻塞 worker 数 4/4。
- 反证：把阈值改成 5000 ms，同样的停滞场景 `stalls:0` ⇒ 断言确实由信号驱动，不是恒绿。

### hud
- 安装：release 二进制放 `/opt/hud/hud`（WSL root）。release 叫 v0.5.3，二进制自报 v0.5.2。
- 能做的：识别出 4 个 tokio worker、附着成功、能导 trace 文件。
- 卡住的：`Handler called: 18,943 / Passed PID filter: 0 (ALL FILTERED OUT)`，8 秒 0 事件，导出文件 50 字节。
- 结论：WSL2 内核（6.18.54.1-microsoft-standard-WSL2，有 `/sys/kernel/btf/vmlinux`）上 perf 事件到了用户态回调，但按 PID 过滤一个都不匹配。**要真采栈得上裸机 Linux 或云 VM**；在那之前 hud 只能算"装好了"。
- 复跑：`MSYS_NO_PATHCONV=1 wsl.exe -u root -- bash /mnt/d/KF/lacbs/scripts/hud-smoke.sh`

## 平台坑（都踩过）

- Git Bash 会把 `/mnt/...` 改写成 `C:/Program Files/Git/mnt/...`，传给 `wsl.exe` 必须加 `MSYS_NO_PATHCONV=1`。
- WSL 里 `index.crates.io` 连不上（`static.crates.io` 通）。离线构建靠：
  `CARGO_HOME=/mnt/c/Users/lbx13/.cargo CARGO_TARGET_DIR=/home/lbx13/lacbs-target RUSTFLAGS="" cargo build --offline`
  —— `RUSTFLAGS=""` 用来压掉 `.cargo/config.toml` 里的 `tokio_unstable`（纯基线用不上）。
- 管道会吞退出码：`cmd | tail` 之后再取 `$?` 拿到的是 `tail` 的。判 rc 时先落文件。

## 未完成

- hud 端到端（缺一台能跑 eBPF 的 Linux）。
- tokio-console 的 TUI 只验证到"Cli 能起、数据能取"，没做人眼走查。
- Windows 定时器/sleep 粒度未标定：任何延迟结论之前先跑一次基准，别拿裸数字比。
