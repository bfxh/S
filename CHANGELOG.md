# Changelog

所有值得记录的变更都写在这里。格式参考 Keep a Changelog；版本遵循 SemVer。

## [0.1.0] - 2026-10-21

首个公开版本：观测面 + 调度器内核 + 真实/生成 trace 评估 + tokio 执行层。

### Added

- **观测面**：tokio-console（含无头探针 `console-probe`）、dial9、hotpath、
  async-liveliness-monitor；一个工具一个 feature，门禁真跑。
  hud 在 WSL2 上 PID 过滤全落空，记录为环境限制（`docs/TOOLING.md`）。
- **内核**（`lacbs-core`）：确定性整数步进模拟；FIFO/EDF、拒绝、抢占、老化、KV 上限、
  客户端重试；profile P0–P4；逐阶段 oracle 指标（含合成矩阵单测）。
- **真实 trace**：BurstGPT 窗口抽取与回放（`--replay-csv`）、ServeGen 生成器负载；
  预注册批次 6 条轨迹，4 条 gap ≥5%（`docs/REPLAY-TRACES.md`）。
- **M2 控制器**：窗口级规则控制器 + 安全层；实测 5/6 劣于静态 P4，判定不上线
  （`docs/M2-RESULT.md`）。逐请求"竞争门槛拒绝"作为默认关闭的旋钮保留。
- **执行层**（`lacbs-exec`）：tokio 任务上跑 EDF/FIFO 准入、无望即拒、交互式抢占批处理、
  换出/恢复；A/B 三次采样中位数：换出把 p99 从 671 ms 降到 394 ms、重算 token 归零。
- **CI**：fmt / clippy(-D warnings) / test / gate / cargo audit；`scripts/gate.sh` 跨平台。

### Known limitations

- 内核成本模型是合成的；执行层的计算用 `spin` 模拟，不是真实模型。
- 执行层只支持"交互式挤批处理"一个方向；换出是"保留进度 + 固定代价"，没有真实 KV 拷贝。
- 真实 GPU、多实例迁移、多租户均未立项。
