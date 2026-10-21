## 这个 PR 做了什么

（一句话；如果是修复，先写现象）

## 判据（必填）

- 命令：
- 改前 → 改后：
- 变异/反证（新增断言时必填）：

## 门禁

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `bash scripts/gate.sh --skip-dial9 --skip-hotpath`

## 风险与回滚

- 回滚点（提交/标签）：
- 有没有改动基线或放宽判据（有则单独说明）：
