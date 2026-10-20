# BurstGPT 真实 trace 回放：是否重启自适应方向

> 判据（预注册）：`docs/ADAPTIVE-DESIGN.md` §11.1 第 1 条——
> **真实 trace 上至少 3 条轨迹的逐阶段 oracle gap ≥ 5%**，否则方向维持结案。

## 数据与方法

- **来源**：BurstGPT（1,429,737 条、61 天，Azure GPT 服务真实 trace；字段 = 时间戳 / 模型 / prompt / response tokens）。
- **窗口**：按小时序号取 4 小时窗口，覆盖高峰与低谷；`scripts/trace/burstgpt_windows.py` 抽取。
  为控制仿真规模，部分窗口按 2 或 4 抽稀（表里注明）。
- **类别映射**（建模假设，trace 本身没有类别）：响应长度 ≤ 窗口中位数 ⇒ `interactive`，否则 `batch`。
- **SLO**：沿用合成轨迹常数——interactive 首字 25 ms / TPOT 20 ms，batch 首字 300 ms / TPOT 100 ms。
  这些是**合成值**，只保证与 8 ms/step 的成本模型自洽，不代表真实 SLA。
- **时间缩放**：`arrival_us = 真实秒 × k`，`k = 观测速率 / (target_load × 当前容量)`；
  target_load 见各次运行（0.6–0.7），保持原 trace 的突发形状。
- **其他开关**：队列上限 1024/2048；**KV 上限关闭、客户端重试关闭**——
  这两者已在 M1.1 的合成轨迹上单独测过，真实 prompt 长度（p90 1796、max 29665）下开启会让系统进入
  "挤出→重算→再挤出"的抖动，且与"策略排序是否有阶段差"不是同一个问题。
- **阶段**：把窗口等分成 4 段，作为逐阶段 oracle 的单位。

## 结果

| 窗口 | 抽稀 | 行数 | avg prompt/out | penalty 0 gap | penalty 1 gap | 状态 |
|---|---|---|---|---|---|---|
| 231–235h | 2 | 11,500 | 447/70 | 0.1% | 0.0% | 健康 |
| 348–352h | 4 | 14,027 | 226/46 | 0.0% | 0.0% | 健康 |
| 520–522h（2h，长 prompt） | 4 | 5,656 | 607/95 | 0.1% | 0.1% | 健康 |
| 1234–1238h | 2 | 9,602 | 447/123 | 0.0% | 200%（退化） | 见下 |
| 519–523h | 4 | 8,390 | 671/137 | — | — | 超时，未计入 |

- **四个健康窗口的 gap 全部 ≤0.1%**（含一个 avg prompt 607 的长 prompt 窗口）：
  最好固定 profile 恰好达到逐阶段 oracle。
  逐阶段最优 profile 并不总是同一个（231 窗口内 P4 在前三段、P2 在第四段），但 **P2 全程总分与 oracle 打平**——
  这再次说明"阶段最优不同"不等于"切换能赚"。
- **1234 窗口的 200% 是退化值**：该窗口是"长时间稀疏 + 分钟级尖峰"（p50 3 条/分、峰值 1259 条/分），
  在 penalty=1 下几乎所有 profile 的得分都 ≤0，最好固定为 −28、oracle 为 +28，
  分母接近零导致比例失真。实际服务量只有 268/9,602，属于系统被压垮的区间，不作为证据。
- **519 窗口**：prompt 均值 671、单档 30 万步以上，单次全量扫描超过 7 分钟，超出本轮时间预算；
  仅跑完 P0/P1（5192 vs 5123，FIFO 略优）。**未计入结论**，但提示"长 prompt + 强突发"是 EDF 的弱区。

## ServeGen：为什么不能直接做在线回放

拉取了 `alibaba/ServeGen`（SOSP'26 ACDC 论文配套）的离线 trace。结论：**不能当在线负载用**。

- `data/offline/trace_d/e/f.jsonl` 是**整批提交**的离线任务，每个文件是一次批作业的全部请求，
  只有 `request_id`（提交顺序）、`input_length`、`output_length`、`duration`——**没有到达时间**。
  没有到达过程，就没有"非平稳到达"可言，回放会退化成我们自己造时间轴。
- `duration` 是生产端到端时延（含排队与批处理），不是单请求服务时间。用它拟合成本模型会把排队混进计算。
  实测：`trace_d`（生成型）`duration ≈ 1821 + 14.9×output`，R²=0.46；
  `trace_e`（评分型，input≈707 恒定）`duration ≈ 229 + 0.45×input`，R²=0.00（输入没有变化，拟合不识别）。
- **它能提供的唯一直接价值是尺度校验**：生成型任务约 15 ms/输出 token（端到端、含批处理），
  与我们选的 8 ms/step 同一量级——说明 M1/M1.1 的"慢 regime"不是拍脑袋选的。
- 要用 ServeGen 做在线实验，需要它的 `servegen` 生成器造到达过程，或使用其在线 trace（本仓库只有离线批任务）。

## 结论

**重启条件未满足**（0/4 健康窗口达到 5%），自适应控制器方向维持结案。

可复算：
```bash
python scripts/trace/burstgpt_windows.py --src vendor/BurstGPT/data/BurstGPT_1.csv \
    --centers 231,348,519 --hours 4 --out-dir vendor/burstgpt/s2 --stride 2
./target/debug/sim.exe --replay-csv vendor/burstgpt/s2/window_231_235.csv \
    --retry-pct 0 --max-queue 1024 --target-load 0.6 --phases 4 --reject-penalty 0
```
（fixture 版在门禁第 6 步：`tests/fixtures/burstgpt_sample.csv`，300 行，1 秒内跑完。）

## 这一轮证明了什么、没证明什么

**证明了的**：
- 回放管线可用：真实到达/长度分布能直接喂进内核，且同输入可复算。
- 在真实窗口上，静态 profile 的差距依然极小；最优档会随窗口在 P4/P2 之间移动，但不需要在线切换。

**没证明的**：
- 类别映射与 SLO 是合成的；换一套 SLO（例如按模型/租户区分）可能改变排序。
- KV 与重试在真实 trace 上是关闭的；开启后长 prompt 的行为未纳入本轮比较。
- 519 这类重窗口没有跑完，样本里缺"长 prompt + 强突发"这一类最难的情形。

## 若还要继续，优先级

1. **解锁重窗口**：把 EDF/FIFO 的候选选择从每步 O(队列) 改成堆（当前是回放吞吐的主要瓶颈），
   让 519 这类 4 小时大窗口能完整跑完（本轮用 2 小时切片绕过了）。
2. **换一个更贴真实的 SLO 口径**：按模型（ChatGPT vs GPT-4）或按 log 类型分层，而不是按响应长度切。
3. **接 ServeGen 的在线形态**：本仓库的离线 trace 没有到达时间；要覆盖多租户场景，
   需要用 `servegen` 生成器造到达过程，或拿到其在线 trace——那是 H-MAS 声称收益的地方。
   本轮的 BurstGPT 是单租户 API 流量，本来就不利于"自适应"命题。
