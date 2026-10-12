#!/usr/bin/env bash
# target-eviction.sh — src-tauri/target 构建缓存淘汰机制
#
# 根因：cargo 对 target/ 没有淘汰——每次构建生成新哈希产物，旧的永不删除，
# 只涨不缩（2026-09 实测涨到 9GB）。本脚本按阶梯策略回收，由
# com.questlog.target-eviction LaunchAgent 每日调用，也可手动执行。
#
# 阶梯（默认模式，仅在总量超 WARN 时启动）：
#   1. 删 target/{debug,release}/incremental（零风险，代价仅下次增量编译变全量）
#   2. 清扫 deps/build/.fingerprint/bundle 中 mtime 超 KEEP_DAYS 的陈旧产物
#      （被误删的仍会被 cargo 按需重编，最坏代价是一次慢构建，不会损坏状态）
#   3. 清扫后总量仍超 HARD 时整删 target/debug（release 交付产物不动）
#   4. 仍超 HARD：记 CRITICAL 并发 macOS 通知；不自动 cargo clean——
#      抹掉 release 会让下次交付构建全量重编，这一步留给人工决策
#
# 用法：target-eviction.sh [--dry-run] [--deep]
#   --dry-run  只报告将执行的动作，不删除
#   --deep     无视阈值强制执行全阶梯，KEEP_DAYS 收紧到 7 天（首轮清理用）
#
# 日志：~/questlog-runtime/logs/target-eviction.log
# 文档：docs/target-eviction.md

set -euo pipefail

REPO_ROOT="${HOME:?HOME 未设置，拒绝执行}/编程架构包/桌面清单贴"
TARGET_DIR="$REPO_ROOT/desktop-app/src-tauri/target"
LOG_DIR="$HOME/questlog-runtime/logs"
LOG_FILE="$LOG_DIR/target-eviction.log"
LOCK_DIR="$HOME/questlog-runtime/target-eviction.lock"

WARN_KIB=$((2560 * 1024))   # 2.5 GiB：触发淘汰的软阈值
HARD_KIB=$((3584 * 1024))   # 3.5 GiB：允许整删 debug 的硬阈值
KEEP_DAYS=30                # 陈旧产物保留天数
DEEP_DAYS=7                 # --deep 模式收紧到 7 天

DRY_RUN=false
FORCE=false
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=true ;;
    --deep)    FORCE=true ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done
if [ "$FORCE" = true ]; then KEEP_DAYS=$DEEP_DAYS; fi

mkdir -p "$LOG_DIR"

log() {
  local line
  line="$(date '+%Y-%m-%d %H:%M:%S') $*"
  # dry-run 只上屏不入档：操作日志只记真实动作，观察期日检按此日志判定
  if [ "$DRY_RUN" != true ]; then
    echo "$line" >> "$LOG_FILE"
  fi
  if [ "$DRY_RUN" = true ]; then line="[dry] $line"; fi
  echo "$line"
  return 0
}

human() { # $1=KiB → 人类可读
  local kib=$1
  if [ "$kib" -ge $((1024 * 1024)) ]; then
    awk -v k="$kib" 'BEGIN{printf "%.1fG", k/1024/1024}'
  elif [ "$kib" -ge 1024 ]; then
    awk -v k="$kib" 'BEGIN{printf "%.0fM", k/1024}'
  else
    echo "${kib}K"
  fi
}

measure() { # $1=dir → KiB
  if [ -d "$1" ]; then du -sk "$1" 2>/dev/null | cut -f1; else echo 0; fi
}

run_rm() {
  if [ "$DRY_RUN" = true ]; then
    log "  [dry] rm -rf $*"
  else
    rm -rf "$@"
  fi
}

sweep_stale_files() { # $1=要清扫的目录（按文件 mtime）
  local dir=$1 count freed
  [ -d "$dir" ] || return 0
  count=$(find "$dir" -type f -mtime +"$KEEP_DAYS" 2>/dev/null | wc -l | tr -d ' ')
  [ "$count" -eq 0 ] && return 0
  freed=$(find "$dir" -type f -mtime +"$KEEP_DAYS" -print0 2>/dev/null \
    | xargs -0 du -sk 2>/dev/null | awk '{s+=$1} END{print s+0}')
  log "  sweep $1: $count 个文件超 ${KEEP_DAYS} 天 ($(human "$freed"))"
  if [ "$DRY_RUN" != true ]; then
    find "$dir" -type f -mtime +"$KEEP_DAYS" -delete 2>/dev/null || true
    find "$dir" -type d -empty -delete 2>/dev/null || true
  fi
}

sweep_stale_fingerprints() { # $1=.fingerprint 目录（按子目录 mtime）
  local dir=$1 d
  [ -d "$dir" ] || return 0
  while IFS= read -r d; do
    log "  sweep fingerprint: $(basename "$d")"
    if [ "$DRY_RUN" != true ]; then rm -rf "$d"; fi
  done < <(find "$dir" -mindepth 1 -maxdepth 1 -type d -mtime +"$KEEP_DAYS" 2>/dev/null)
}

sweep_stale_bundles() { # release/bundle 顶层条目（.app 目录与 .dmg）
  local dir=$1 e
  [ -d "$dir" ] || return 0
  while IFS= read -r e; do
    log "  sweep bundle: $(basename "$e")"
    if [ "$DRY_RUN" != true ]; then rm -rf "$e"; fi
  done < <(find "$dir" -mindepth 1 -maxdepth 1 -mtime +"$KEEP_DAYS" 2>/dev/null)
}

# ── 防误配护栏：只接受 src-tauri/target 路径，避免 rm -rf 打到别处 ──
case "$TARGET_DIR" in
  */src-tauri/target) ;;
  *) echo "refusing suspicious TARGET_DIR: $TARGET_DIR" >&2; exit 1 ;;
esac

if [ ! -d "$TARGET_DIR" ]; then
  log "target 不存在，跳过"
  exit 0
fi

# ── 并发锁：另一个实例在跑就退出；超 2 小时的残留锁视为陈旧并接管 ──
if [ -d "$LOCK_DIR" ]; then
  if [ -n "$(find "$LOCK_DIR" -maxdepth 0 -mmin +120 2>/dev/null)" ]; then
    rm -rf "$LOCK_DIR"
  else
    log "已有实例在跑，退出"
    exit 0
  fi
fi
mkdir "$LOCK_DIR"
trap 'rm -rf "$LOCK_DIR"' EXIT

# ── 阶梯执行 ──
mode="default"
[ "$FORCE" = true ] && mode="deep"
size_before=$(measure "$TARGET_DIR")
log "start mode=$mode size=$(human "$size_before") warn=$(human "$WARN_KIB") hard=$(human "$HARD_KIB") keep=${KEEP_DAYS}d"

if [ "$FORCE" != true ] && [ "$size_before" -le "$WARN_KIB" ]; then
  log "healthy：未超阈值，无事可做"
  exit 0
fi

# 1) 增量编译缓存：随时可删
for prof in debug release; do
  run_rm "$TARGET_DIR/$prof/incremental"
done

# 2) 陈旧产物清扫（默认模式仅在删 incremental 后仍超 WARN 时进行）
size_mid=$(measure "$TARGET_DIR")
if [ "$FORCE" = true ] || [ "$size_mid" -gt "$WARN_KIB" ]; then
  for prof in debug release; do
    sweep_stale_files "$TARGET_DIR/$prof/deps"
    sweep_stale_files "$TARGET_DIR/$prof/build"
    sweep_stale_fingerprints "$TARGET_DIR/$prof/.fingerprint"
  done
  sweep_stale_bundles "$TARGET_DIR/release/bundle"
fi

# 3) 硬阈值兜底：整删 debug（release 不动，交付构建不受影响）
size_mid=$(measure "$TARGET_DIR")
if [ "$size_mid" -gt "$HARD_KIB" ]; then
  log "清扫后仍 $(human "$size_mid") 超硬阈值，整删 debug"
  run_rm "$TARGET_DIR/debug"
fi

# 4) 终态汇报；仍超标则通知人工
size_after=$(measure "$TARGET_DIR")
if [ "$size_after" -gt "$HARD_KIB" ]; then
  log "CRITICAL: 清理后仍 $(human "$size_after")，建议人工 cargo clean（下次构建将全量重编）"
  osascript -e "display notification \"target 清理后仍 $(human "$size_after")，建议手动 cargo clean\" with title \"QuestLog target 淘汰\"" >/dev/null 2>&1 || true
else
  log "done: $(human "$size_before") → $(human "$size_after")"
fi
