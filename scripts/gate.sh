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

if [ "${1:-}" != "--skip-hotpath" ]; then
  step "5. hotpath：报告里必须出现被插桩的函数"
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
