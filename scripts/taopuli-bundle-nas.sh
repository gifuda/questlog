#!/usr/bin/env bash
# taopuli-bundle-nas.sh — TopLEe 仓每日 git bundle 腿（S4 从 board-snapshot-nas.sh 拆出留 M4）
# 原因：S4 后板快照推送源迁 m5p，而 TopLEe 源仓留 M4——bundle 腿跟源仓走。
# 覆盖：git bundle create --all → /tmp → NAS 原子落位（.part + mv）；30 天轮转。
# 调度：com.questlog.taopuli-bundle 每日 21:20（与 m5p 板快照 21:10 错峰）。
# 日志：~/questlog-runtime/logs/taopuli-bundle.log
set -uo pipefail
: "${HOME:?HOME 未设置，拒绝执行}"

TL="$HOME/韬普利TopLEe"
LOG_DIR="$HOME/questlog-runtime/logs"
DAYB="taopuli-$(date +%Y%m%d).bundle"
mkdir -p "$LOG_DIR"
log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" | tee -a "$LOG_DIR/taopuli-bundle.log"; }

if git -C "$TL" bundle create "/tmp/$DAYB" --all >/dev/null 2>&1; then
  if cat "/tmp/$DAYB" | ssh -o BatchMode=yes -o ConnectTimeout=10 nas \
       "cat > /volume2/git/$DAYB.part && mv /volume2/git/$DAYB.part /volume2/git/$DAYB"; then
    log "OK TopLEe bundle $(du -h "/tmp/$DAYB" | cut -f1)"
  else
    log "FAIL TopLEe bundle 传输"
  fi
  rm -f "/tmp/$DAYB"
else
  log "WARN TopLEe bundle create 失败"
fi
ssh -o BatchMode=yes -o ConnectTimeout=10 nas 'find /volume2/git -maxdepth 1 -name "taopuli-*.bundle" -mtime +30 -delete' 2>/dev/null || true
