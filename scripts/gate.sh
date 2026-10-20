#!/usr/bin/env bash
# 工具面门禁：逐个 feature 真跑一遍，判据不达标就红。
# 用法：bash scripts/gate.sh [--skip-hotpath]
#
# 每个 feature 构建后把二进制快照到 tmp/，避免后一步的构建覆盖前一步的产物；
# 之前出现过"跑到的其实是上一个 feature 的二进制"导致的偶发假绿/假红。
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

tmp=${TMPDIR:-/tmp}/lacbs-gate
mkdir -p "$tmp"
fail=0
step() { printf '\n== %s ==\n' "$1"; }
red() { echo "RED: $1"; fail=1; }

# build_harness <name> [features...] → 快照路径
build_harness() {
  local name=$1
  shift
  if ! cargo build -q -p lacbs-harness "$@"; then
    echo "RED: $name 构建失败"
    fail=1
    return 1
  fi
  cp target/debug/harness.exe "$tmp/harness-$name.exe" || return 1
  echo "$tmp/harness-$name.exe"
}

step "0. 基线（无工具）"
base=$(build_harness base) || exit 1
out=$(timeout 60 "$base" --secs 2 --tasks 8 --mode mixed)
echo "$out" | grep -q '"iterations":[1-9]' || red "基线没有迭代: $out"

step "1. tokio-console：无头探针必须看到任务与 poll"
cons=$(build_harness console --features lacbs-obs/console) || exit 1
cargo build -q -p lacbs-console-probe || red "探针构建失败"
timeout 60 "$cons" --secs 10 --tasks 16 --mode mixed > /dev/null 2>&1 &
hp=$!
sleep 2
timeout 60 ./target/debug/console-probe.exe --secs 3 > "$tmp/probe.out" 2>&1
rc=$?
cat "$tmp/probe.out"
[ "$rc" -eq 0 ] || red "console 探针退出码 $rc"
wait "$hp" 2>/dev/null

step "2. liveliness：停滞必须抓到（且真的卡住），正常场景必须不报"
live=$(build_harness live --features lacbs-obs/liveliness) || exit 1
stall=$(timeout 60 "$live" --secs 3 --workers 4 --tasks 4 --mode stall 2>&1)
echo "$stall"
echo "$stall" | tail -1 | grep -q '"stalls":1' || red "停滞没被抓到"
echo "$stall" | tail -1 | grep -q '"iterations":0' || red "阻塞没真正生效（迭代数不为 0），这次 stalls=0 不能算数"
echo "$stall" | grep -q '进入阻塞的 worker 数 = 4/4' || red "没有 4/4 worker 进入阻塞"
mixed=$(timeout 60 "$live" --secs 3 --workers 4 --tasks 8 --mode mixed 2>/dev/null | tail -1)
echo "$mixed"
echo "$mixed" | grep -q '"stalls":0' || red "正常场景误报"

step "3. dial9：必须产出可解码的 trace，且 poll 事件有量"
d9=$(build_harness dial9 --features lacbs-obs/dial9) || exit 1
rm -rf traces
timeout 60 "$d9" --secs 3 --tasks 8 --mode mixed > /dev/null 2>&1 || red "dial9 运行失败"
dial9 trace-shape extract traces/trace.0.bin traces/shape.json > /dev/null 2>&1 \
  || red "trace 解析失败"
polls=$(grep -o '"PollStartEvent": [0-9]*' traces/shape.json | grep -o '[0-9]*' | head -1)
workers=$(grep -o '"worker_cardinality": [0-9]*' traces/shape.json | grep -o '[0-9]*' | head -1)
[ -n "$polls" ] && [ "$polls" -gt 100 ] || red "poll 事件太少: ${polls:-无}"
[ -n "$workers" ] && [ "$workers" -eq 4 ] || red "worker 数不对: ${workers:-无}"
echo "PollStartEvent=$polls worker_cardinality=$workers"

step "4. 调度器内核：异质 SLO 下截止期策略必须减少交互式违约"
cargo test -q -p lacbs-core > "$tmp/core-test.out" 2>&1 || red "内核测试失败，见 $tmp/core-test.out"
cargo build -q -p lacbs-sim || red "sim 构建失败"
fifo=$(./target/debug/sim.exe --policy fifo)
edf=$(./target/debug/sim.exe --policy deadline)
echo "$fifo"
echo "$edf"
f=$(echo "$fifo" | grep -o '"interactive":{[^}]*}' | grep -o '"ttft_misses":[0-9]*' | cut -d: -f2)
e=$(echo "$edf" | grep -o '"interactive":{[^}]*}' | grep -o '"ttft_misses":[0-9]*' | cut -d: -f2)
if [ -z "$f" ] || [ -z "$e" ]; then
  red "解析不到交互式违约数"
elif [ "$e" -ge "$f" ]; then
  red "截止期策略没赢: fifo=$f deadline=$e"
fi

# 拒绝：无望的直接丢，被服务的人必须全部达标
rej=$(./target/debug/sim.exe --policy deadline --reject)
echo "$rej"
r=$(echo "$rej" | grep -o '"rejected":[0-9]*' | cut -d: -f2)
m=$(echo "$rej" | grep -o '"ttft_misses":[0-9]*' | head -1 | cut -d: -f2)
[ "${r:-0}" -gt 0 ] || red "拒绝没生效"
[ "${m:-1}" -eq 0 ] || red "拒绝后被服务的仍有首字违约: $m"

# 抢占：批处理占满槽位后才到的交互式，必须从"等"变成"挤"
base=$(./target/debug/sim.exe --policy deadline --scenario flood --late-interactive 4)
pre=$(./target/debug/sim.exe --policy deadline --scenario flood --late-interactive 4 --preempt)
echo "$base"
echo "$pre"
bm=$(echo "$base" | grep -o '"interactive":{[^}]*}' | grep -o '"ttft_misses":[0-9]*' | cut -d: -f2)
pm=$(echo "$pre" | grep -o '"interactive":{[^}]*}' | grep -o '"ttft_misses":[0-9]*' | cut -d: -f2)
pc=$(echo "$pre" | grep -o '"preemptions":[0-9]*' | cut -d: -f2)
[ "${bm:-0}" -gt 0 ] || red "flood 基线没违约，测不出抢占"
[ "${pm:-1}" -eq 0 ] || red "抢占后交互式仍违约: $pm"
[ "${pc:-0}" -gt 0 ] || red "没有发生抢占"

step "5. M1 扫描：oracle 必须不低于最好固定策略，且 5 条轨迹都要跑出来"
cargo build -q -p lacbs-sim || red "sim 构建失败"
if ./target/debug/sim.exe --sweep --kv-tokens 6000 --retry-pct 80 --retry-delay-us 50000 --max-attempts 3 --reject-penalty 1 > "$tmp/sweep.out" 2>&1; then
  n=$(grep -c '^SUMMARY' "$tmp/sweep.out")
  [ "$n" -eq 5 ] || red "SUMMARY 条数 $n != 5"
  if ! awk '/^SUMMARY/ {
        b=-1; o=-1;
        for (i=1;i<=NF;i++) {
          if ($i ~ /^best_score=/) { split($i,a,"="); b=a[2]+0 }
          if ($i ~ /^oracle_score=/) { split($i,a,"="); o=a[2]+0 }
        }
        if (b < 0 || o < 0 || o < b) { print "BAD: " $0; bad=1 }
      } END { exit bad }' "$tmp/sweep.out"; then
    red "oracle 自检失败（oracle 低于最好固定策略，或解析失败）"
  fi
  grep -q 'error=' "$tmp/sweep.out" && red "有 profile 在扫描中报错"
else
  red "sweep 运行失败"
fi

step "6. 真实 trace 回放（fixture）：5 个 profile 都要跑完且 oracle ≥ best"
if ./target/debug/sim.exe --replay-csv tests/fixtures/burstgpt_sample.csv --max-queue 1024 --target-load 0.6 --phases 3 --reject-penalty 1 > "$tmp/replay.out" 2>&1; then
  n=$(grep -c '^ROW' "$tmp/replay.out")
  [ "$n" -eq 5 ] || red "回放 ROW 条数 $n != 5"
  [ "$(grep -c '^SUMMARY' "$tmp/replay.out")" -eq 1 ] || red "回放 SUMMARY 条数不对"
  grep -q 'error=' "$tmp/replay.out" && red "回放中有 profile 报错"
  if ! awk '/^SUMMARY/ {
        b=-1; o=-1;
        for (i=1;i<=NF;i++) {
          if ($i ~ /^best_score=/) { split($i,a,"="); b=a[2]+0 }
          if ($i ~ /^oracle_score=/) { split($i,a,"="); o=a[2]+0 }
        }
        if (b < 0 || o < 0 || o < b) { print "BAD: " $0; bad=1 }
      } END { exit bad }' "$tmp/replay.out"; then
    red "回放 oracle 自检失败"
  fi
else
  red "真实 trace 回放失败"
fi

step "7. M2 控制器（fixture）：必须跑完且切换次数 ≤ 窗口数"
if ./target/debug/sim.exe --replay-csv tests/fixtures/burstgpt_sample.csv --adaptive --window-s 2     --decode-us 2000 --max-queue 512 --target-load 0.8 --phases 3 --reject-penalty 1 > "$tmp/adaptive.out" 2>&1; then
  grep -q '^ADAPTIVE' "$tmp/adaptive.out" || red "没有 ADAPTIVE 输出"
  sw=$(grep -o 'switches=[0-9]*' "$tmp/adaptive.out" | cut -d: -f2 | cut -d= -f2)
  win=$(grep -o 'windows=[0-9]*' "$tmp/adaptive.out" | cut -d: -f2 | cut -d= -f2)
  [ -n "$sw" ] && [ -n "$win" ] || red "解析不到 switches/windows"
  [ "${sw:-1}" -le "${win:-0}" ] || red "切换次数 $sw 超过窗口数 $win"
else
  red "控制器运行失败"
fi

step "8. 执行层：内核策略跑在 tokio 任务上，必须全部交代且无停滞"
cargo build -q -p lacbs-exec || red "exec 构建失败"
out=$(timeout 60 ./target/debug/exec.exe --secs 2 --tasks 64 2>/dev/null | tail -1)
echo "$out"
c=$(echo "$out" | grep -o '"completed":[0-9]*' | cut -d: -f2)
r=$(echo "$out" | grep -o '"rejected":[0-9]*' | cut -d: -f2)
t=$(echo "$out" | grep -o '"tasks":[0-9]*' | cut -d: -f2)
if [ -z "$c" ] || [ -z "$r" ] || [ -z "$t" ]; then
  red "执行层输出解析失败"
elif [ "$((c + r))" -ne "$t" ]; then
  red "完成($c)+拒绝($r) != 任务数($t)"
fi
echo "$out" | grep -q '"stalls":0' || red "执行层出现停滞"

if [ "${1:-}" != "--skip-hotpath" ]; then
  step "9. hotpath：报告里必须出现被插桩的函数"
  hp_bin=$(build_harness hotpath --features hotpath) || exit 1
  timeout 60 "$hp_bin" --secs 2 --tasks 8 --mode mixed 2>&1 | grep -q "harness::burn" \
    || red "hotpath 报告缺 harness::burn"
fi

printf '\n'
if [ "$fail" -ne 0 ]; then
  echo "GATE: RED"
  exit 1
fi
echo "GATE: GREEN"
