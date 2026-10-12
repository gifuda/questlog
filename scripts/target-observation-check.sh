#!/usr/bin/env bash
# target-observation-check.sh — 观察期每日自动核验（epic_521748a0 · S5）
#
# 由 com.questlog.target-observation LaunchAgent 每日 12:10 调用，后台自动追踪
# 淘汰机制的观察期完成情况。三项验收（与方案 acceptance 一致）：
#   ① 淘汰调度 26h 内有运行记录（宽限覆盖 Mac 睡眠导致的延迟触发）
#   ② 26h 内无 CRITICAL
#   ③ target 总量 ≤ 2.5GiB
# 结果追加到 ~/questlog-runtime/logs/target-eviction-observation.md；
# FAIL 或观察期满（≥2026-10-16 的当日 PASS）发 macOS 通知；期满后自动退役
# （删 plist + bootout，防重启复活）。只读检查 + 追加记录，绝不改面板状态——
# S1-S5 推进 observed 仍由用户在面板确认。
#
# 用法：target-observation-check.sh [--check-only]
#   --check-only  只跑检查打印结果，不记录不通知（测试用）

set -euo pipefail

REPO_ROOT="$HOME/编程架构包/桌面清单贴"
TARGET_DIR="$REPO_ROOT/desktop-app/src-tauri/target"
RUNTIME_LOG_DIR="$HOME/questlog-runtime/logs"
EVICT_LOG="$RUNTIME_LOG_DIR/target-eviction.log"
OBS_FILE="$RUNTIME_LOG_DIR/target-eviction-observation.md"
LABEL="com.questlog.target-observation"
WARN_KIB=$((2560 * 1024))   # 2.5 GiB
WINDOW_END="2026-10-16"
GRACE_HOURS=26

CHECK_ONLY=false
if [ "${1:-}" = "--check-only" ]; then CHECK_ONLY=true; fi

today=$(date +%F)
now_s=$(date +%s)

notify() {
  osascript -e "display notification \"$1\" with title \"QuestLog 观察期日检\"" >/dev/null 2>&1 || true
  return 0
}

mkdir -p "$RUNTIME_LOG_DIR"
touch "$OBS_FILE"

# 幂等：当日已有记录则退出（launchd 重复触发不双记）
if [ "$CHECK_ONLY" != true ] && grep -q "^| $today " "$OBS_FILE"; then
  exit 0
fi

# ① 淘汰调度最近 26h 内有运行
fail_reason=""
last_start=$(grep " start mode=" "$EVICT_LOG" 2>/dev/null | tail -1 | cut -d" " -f1-2)
if [ -z "$last_start" ]; then
  fail_reason="淘汰日志无任何运行记录"
else
  last_epoch=$(date -j -f "%Y-%m-%d %H:%M:%S" "$last_start" +%s 2>/dev/null || echo 0)
  if [ "$last_epoch" -eq 0 ]; then
    fail_reason="淘汰日志时间戳无法解析（$last_start）"
  else
    age_h=$(( (now_s - last_epoch) / 3600 ))
    if [ "$age_h" -gt "$GRACE_HOURS" ]; then
      fail_reason="淘汰调度超 ${GRACE_HOURS}h 未运行（最后：$last_start）"
    fi
  fi
fi

# ② 最近 26h 内无 CRITICAL
while IFS= read -r line; do
  ts=$(echo "$line" | cut -d" " -f1-2)
  ep=$(date -j -f "%Y-%m-%d %H:%M:%S" "$ts" +%s 2>/dev/null || echo 0)
  if [ "$ep" -gt 0 ] && [ $(( (now_s - ep) / 3600 )) -le "$GRACE_HOURS" ]; then
    if [ -n "$fail_reason" ]; then fail_reason="$fail_reason；"; fi
    fail_reason="${fail_reason}出现 CRITICAL：$line"
  fi
done < <(grep "CRITICAL" "$EVICT_LOG" 2>/dev/null || true)

# ③ 水位 ≤ 2.5GiB
size_kib=$(du -sk "$TARGET_DIR" 2>/dev/null | cut -f1)
if [ -z "$size_kib" ]; then size_kib=0; fi
size_h=$(awk -v k="$size_kib" 'BEGIN{printf "%.1fG", k/1024/1024}')
if [ "$size_kib" -gt "$WARN_KIB" ]; then
  if [ -n "$fail_reason" ]; then fail_reason="$fail_reason；"; fi
  fail_reason="${fail_reason}target $size_h 超 2.5G 阈值"
fi

result="PASS"
if [ -n "$fail_reason" ]; then result="FAIL"; fi

if [ "$CHECK_ONLY" = true ]; then
  echo "$today $result $size_h ${fail_reason:-三项全过}"
  exit 0
fi

echo "| $today | $result | $size_h | ${fail_reason:-三项全过} |" >> "$OBS_FILE"

if [ "$result" = "FAIL" ]; then
  notify "FAIL：$fail_reason（详见 target-eviction-observation.md）"
  exit 0
fi

# 观察期满：通知汇总并真正退役——先删 plist 再 bootout（只 bootout 会被下次
# 重启从 plist 复活，2026-10-08 重启的教训）
if [[ "$today" == "$WINDOW_END" || "$today" > "$WINDOW_END" ]]; then
  fail_count=$(awk '/窗口二/,0' "$OBS_FILE" | grep -c "| FAIL" || true)
  if [ -z "$fail_count" ]; then fail_count=0; fi
  notify "观察期 7 天完成（FAIL ${fail_count} 次）。面板确认后可推进 S1-S5 observed。"
  rm -f "$HOME/Library/LaunchAgents/${LABEL}.plist"
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
fi
exit 0
