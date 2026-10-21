# Contributing

S（`lacbs`）是一个**证据优先**的调度器仓库：任何结论都必须能被一条命令复算，
任何优化都必须给出门禁或对照。先读 [docs/PROCESS.md](docs/PROCESS.md)。

## 环境

```bash
rustup toolchain install stable   # rust-toolchain.toml 已锁定 stable
cargo build --workspace
```

本机 `.cargo/config.toml` 里 `jobs = 1` 是为了照顾内存紧的开发机；CI 用 `CARGO_BUILD_JOBS=4` 覆盖。

## 提交前必须跑

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/gate.sh --skip-dial9 --skip-hotpath   # 与 CI 一致
```

`scripts/gate.sh` 全量版还需要 `dial9` CLI 与 hotpath 采样环境：

```bash
cargo install dial9 --features cli
bash scripts/gate.sh
```

## 规则

1. **先写判据，再改代码**。PR 描述里写清楚"哪条命令、哪个数字、改前 → 改后"。
2. **新增断言要能判红**。至少手动打一次变异，确认目标测试真的失败；没做过就在 PR 里说明。
3. **基线改动单独提交**，并在描述里双向披露（哪些变绿、哪些变红）。
4. **不要为了绿而放松判据**。判据要放宽，必须单独 PR 并说明理由。
5. **数字要有出处**：合成负载 / BurstGPT / ServeGen 分开写，不要混着比。
6. 不要提交 `vendor/`、`traces/`、任何凭据；`.gitignore` 已经挡住，但仍要检查 `git status`。

## 分支与提交

- 分支：`feat/<topic>`、`fix/<topic>`、`docs/<topic>`、`gate/<topic>`。
- 提交信息：第一行 ≤ 72 字符，说明**为什么**；正文给判据与数字。
- 合并：squash merge；`main` 上禁止 force push。
