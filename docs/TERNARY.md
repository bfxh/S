# 三值准入：把拒绝规则放进 K3 / Ł3 / B3 里看

> 结论先行：内核现在的拒绝规则等价于 **K3 的"只在确定为真时拒绝"**；
> 引入预测后第三态（`X → defer`）才有意义，而且**只能用 K3/Ł3，B3 会把硬约束毒成未知**。
> 本文是分析，不改运行时。

## 1. 规则形式化

内核的拒绝（`crates/lacbs-core/src/sim.rs:221-252`，`hopeless()` 在 `:529`）可写成：

```text
reject = infeasible ∨ (reject_hopeless ∧ contended ∧ hopeless)
```

| 输入 | 含义 | 在线可得性 |
|---|---|---|
| `infeasible` | prompt+output 超过 KV 上限，永远装不下 | **已知** |
| `hopeless` | 现在开始 prefill 也赶不上首字截止期 | 仿真里精确；在线需要预测，**可能是 X** |
| `contended` | 还能达标的等待者多于空余槽位（`reject_only_when_contended` 打开时） | 当前 tick 已知，但依赖 `hopeless` 的计数 |

三值化只发生在 `hopeless`：当预测样本不足、或预测区间跨过截止期时取 `X`（不知道）。
`X` 在拒绝门上的语义是"本 tick 不拒"——请求留在池里，下一个 tick 再判。

## 2. 全枚举（18 组有效输入）

固定 `infeasible` 为已知量 `i∈{0,1}`，`h,c∈{0,1,X}`。K3 下：

| h | c | reject（i=0） | reject（i=1） |
|---|---|---|---|
| 0 | 0 | 0 | 1 |
| 1 | 0 | 0 | 1 |
| X | 0 | 0 | 1 |
| 0 | 1 | 0 | 1 |
| 1 | 1 | 1 | 1 |
| X | 1 | X | 1 |
| 0 | X | 0 | 1 |
| 1 | X | X | 1 |
| X | X | X | 1 |

计数（18 组）：

| 语义 | 确定拒绝（1） | 确定不拒（0） | 未知（X/defer） |
|---|---|---|---|
| Kleene（K3） | 10 | 5 | 3 |
| Łukasiewicz（Ł3） | 10 | 5 | 3 |
| Bochvar（B3） | 5 | 3 | 10 |

K3 与 Ł3 在这条规则上相同（合取/析取都是 min/max）；差别只在蕴含与强运算，本规则用不到。

## 3. 为什么 B3 不能用

B3 把 `X` 当"毒值"，一沾就吸收：

| 情况 | K3 / Ł3 | B3 |
|---|---|---|
| `i=0, h=X, c=0`（有空槽，预测未知） | 0（不拒，可以服务） | X（defer，白让槽位空着） |
| `i=0, h=0, c=X`（确定能达标，竞争未知） | 0（不拒） | X（defer） |
| `i=1, h=X, c=0`（**KV 根本装不下**） | 1（拒） | X（defer，硬约束丢失） |
| `i=1, h=1, c=X`（确定无望且装不下） | 1（拒） | X（defer） |

最后两行是致命的：`infeasible` 是**确定事实**，B3 却因为另一个输入未知而拒绝给出"拒"。
准入这种"或"结构的规则必须用 K3：**确定真可以压过未知，确定假可以短路未知**。

## 4. 对现有内核的含义

- 内核的 `hopeless()` 用当前 `prefilled` 与步成本精确计算，不存在 `X`。
- 因此现有规则已经等价于 K3 表里"只拒确定 1"的那一列，**不需要加第三态**。
- 第三态只在把 `hopeless` 换成在线预测时才有价值：预测不足时 `defer`，避免"样本少就误杀"。
  代价是请求多留一 tick，队列压力上升；收益要用"误拒减少"来量，而不是拍脑袋。

## 5. 如果"三进制"指 base-3 编码

本地工具（`D:\KF\trit`）的**枚举**已经是三进制：第 i 个变量的取值由赋值编号的
第 i 个三进制位决定（`crates/trit-core/src/bits.rs`、`eval.rs` 的解码）。
但**求值**用双位平面（`ones` / `unknowns`，2 bit/trit），不是 base-3 打包。

算术对比（12 变量，3^12 = 531,441 组，估算非实测）：

| 表示 | 体积 | 单次 AND |
|---|---|---|
| 双位平面（现状） | 2 × ⌈531441/64⌉ × 8 B = 132,864 B | 2 次按位运算 / 64 组 |
| base-3 打包（5 trits/byte） | 531,441 × log2(3) / 8 ≈ 105,280 B（−20.8%） | 每 trit 查表/divmod，约慢一个量级 |

结论：省 21% 内存换掉位并行，不划算；双位平面是正确选择。base-3 只适合做真值表的存储/序列化格式。

## 6. 下一步 B（预测式三值准入）的预注册草案

不是现在的默认路径，先写判据再动代码：

- **开关**：`exec --forecast-admit`（opt-in），内核默认不变。
- **X 的来源**：用最近 1 s 的交互式到达率估计 `hopeless`；样本 < N 或预测区间跨截止期时取 X。
- **决策**：只拒 K3 值为 1 的请求；X → defer 一 tick。
- **主判据**：真实切片（A1 m-large 最忙 60 s，138 条）上"误拒"（被拒但在不早拒口径下本可首字达标）
  相对现行 deadline 规则下降 ≥30%。
- **护栏**：`met` 不低于现行规则；队列长度不超过 2×。
- **Kill**：误拒不降或 `met` 下降 ⇒ 放弃，保留现行精确规则。

## 7. 复现

```bash
# 工具：D:\KF\trit（公开仓 bfxh/trit）
CARGO_BUILD_JOBS=1 cargo build -q -p trit-cli --release

trit table "i | (h & c)" -f markdown      # K3，27 行（含 i=X）
trit -a bochvar table "i | (h & c)"       # B3 对照
trit eval "h & c" -s "h=X,c=0"            # K3/Ł3 => 0；B3 => X
```

## 8. 门禁

`crates/lacbs-core` 里有两层断言，`cargo test -p lacbs-core` 即可离线跑：

| 断言 | 位置 | 钉住什么 |
|---|---|---|
| `reject_decision_table` | `src/sim.rs` 单元测试 | 运行时精确决策表的 16 种布尔组合：装不下必拒、开关关闭不拒、无竞争不拒、其余恰好等于 `hopeless` |
| `k3_*` / `b3_*` 共 3 个 | `tests/ternary_admission.rs` | K3 的硬约束与空槽性质、18 行计数 `10/5/3`；B3 把硬约束毒成 `X` 的反证、计数 `5/3/10` |
| `policy (trit verify)` | CI 独立 job + `policy/admission.json` | 用发布版 trit 的 `verify` 在 SAT/BDD 两引擎上检查同一份准入规格 |

fixture（`tests/fixtures/ternary_admission_{k3,b3}.csv`）是 trit 的输出，不是手写。重算：

```bash
py -3.11 scripts/gen_ternary_fixture.py --trit D:/KF/trit/target/release/trit.exe
```

CI **不把 trit 引入 cargo 依赖图**：`deny.toml` 仍是 `unknown-git = "deny"`，而 `trit-core` 未发布到
crates.io。fixture 把工具的判定固化下来，测试只断言语义性质，不重实现三值求值器。

独立策略门（`policy (trit verify)`）走另一条线：下载固定版本 `trit v0.3.1` 的 release 资产，
用 SHA256 钉住后对 `policy/admission.json` 跑 SAT/BDD 两遍。它不进依赖图、不需要 git 源，
失败会让 CI 判红；fixture 测运行时布尔表，策略门测规格本身。
变异复算：删掉 `contended` 条件 ⇒ `reject_decision_table` 红；把 K3 的 `1,X,0,1` 改成 `X` ⇒
两个 K3 测试红。

## 9. 已知边界

- 表中把 `contended` 当单值输入；真实竞争计数在 `hopeless` 有 X 时是一个区间 `[lo,hi]`，
  完整处理需要区间计数，而不只是三值。
- "defer" 不等于"不占资源"：请求仍在等待池里，后续仍可能被准入。
- 三值语义只约束**决策表**，不改变延迟模型；收益必须在执行层实测。
