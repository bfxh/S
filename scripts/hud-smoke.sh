#!/usr/bin/env bash
# 在 WSL 里让 hud 抓一次"执行器停滞"：harness 用同步阻塞占满所有 worker。
# 用法：wsl -u root -- bash /mnt/d/KF/lacbs/scripts/hud-smoke.sh [harness可执行文件]
set -uo pipefail

BIN=${1:-/home/lbx13/lacbs-target/debug/harness}
HUD=${HUD_BIN:-/opt/hud/hud}
OUT=${HUD_TRACE:-/tmp/hud.trace}
MODE=${MODE:-stall}
THRESHOLD=${THRESHOLD:-5}

"$BIN" --secs 15 --workers 4 --tasks 8 --mode "$MODE" > /tmp/harness-stall.out 2>&1 &
HP=$!
sleep 1

"$HUD" --pid "$HP" --duration 8 --headless --export "$OUT" --threshold "$THRESHOLD"
rc=$?

wait "$HP" || true
echo "=== harness 输出 ==="
cat /tmp/harness-stall.out
echo "=== hud 导出 ==="
ls -l "$OUT" 2>&1
exit "$rc"
