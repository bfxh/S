# 流程（Process）

> 这个仓的规则只有一条主线：**结论 → 判据 → 证据 → 可回滚**。
> 下面的流程是为了让这条主线在多人/多会话下不被稀释。

## 1. 变更类型与入口

| 类型 | 入口 | 必须带 |
|---|---|---|
| 缺陷 | Issue（bug_report） | 复现命令 |
| 新能力 / 新评估 | Issue（feature_request） | "怎么判它有用" |
| 纯文档 | 直接 PR | 说明读者与用途 |
| 门禁/基线变更 | 单独 PR | 双向披露：哪些变绿、哪些变红 |

没有判据的提案不进实现；先讨论，再落 Issue。

## 2. PR 生命周期

```
Issue → 分支 → 本地四件套 → PR → CI（fmt/clippy/test/gate/audit）→ 评审 → squash 合并
```

本地四件套：

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/gate.sh --skip-dial9 --skip-hotpath
```

CI 与本地用同一条 gate 命令（见 `.github/workflows/ci.yml`），避免"本地绿、CI 红"。

## 3. 判据分级

| 级别 | 含义 | 例子 |
|---|---|---|
| G1 正确性 | 不变量/回归，必须常绿 | 请求守恒、EOQ 自检、切换次数上限 |
| G2 行为对比 | 策略 A 必须优于 B | EDF vs FIFO 的交互式违约、抢占 vs 不抢占 |
| G3 研究结论 | 允许为负，但必须可复算 | M1 oracle gap、M2 控制器失败、换出 A/B |

G1 进 CI；G2 尽量进 CI；G3 不进 CI（耗时/依赖 trace），但必须留命令与文档，
结论变化时更新对应 `docs/*.md` 并在 CHANGELOG 记录。

## 4. 基线纪律

- 基线（`scripts/gate.sh` 里的期望值、`docs/*` 的读数）只能由**记录它们的那次提交**修改。
- 同一提交两轮跑出的数字不一致时，先查环境（负载、计时、随机种子），不要直接改基线。
- 涉及随机的地方必须固定 seed；ServeGen 生成器已固定为 42。

## 5. 发布与回滚

- 版本：`Cargo.toml` 的 workspace version；`CHANGELOG.md` 按 Keep a Changelog。
- 发布：先更新 CHANGELOG，再 `git tag vX.Y.Z && git push origin vX.Y.Z`；
  `Release` workflow 会跑一遍门禁并生成 release notes（门禁红则不出 release）。
- 回滚：优先 `git revert <commit>`；不重写 `main` 历史。
- 回滚点写在 CHANGELOG 条目里（"回滚到哪个提交、需要还原哪些配置"）。

## 6. 不在本流程内的事

- 真实 GPU 后端接入、多实例迁移（Llumnix 方向）、多租户计费：未立项。
- 任何"只在一个 seed 上成立"的性能结论：不算证据。
