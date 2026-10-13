#!/bin/bash
# questlog-bridge-mirror.sh — S4 过渡镜像桥（m5p → M4 单向只读 rsync）
#
# 职责：把 m5p 权威副本（注册表 + questlog-runtime + 各项目板文件）拉到本机
# 独立镜像根 ~/questlog-mirror/，并维护 REMOTE_ACTIVE 标记（GUI 据此进入
# 远程只读模式：读镜像、写命令 Rust 层拒绝）。镜像生命周期 = S4 切流 → S5
# 客户端化交付，S5 后撤桥删根。
#
# 纪律：
#  - 单向只读，绝无 M4 → m5p 回写（单写者纪律）
#  - 不用 --delete（桥只服务数天，旧文件残留无害；绝不复用 M4 旧权威路径）
#  - 排除 *.lock/*.tmp-*/*.bak/logs（锁 pid 跨机无意义、半写态与滚动日志无价值）
#  - 标记最后写（tmp+mv）：同步失败不翻转标记语义，仅内容陈旧（滞后可见）

set -uo pipefail

REMOTE="${QUESTLOG_BRIDGE_REMOTE:-m5p}"
MIRROR="$HOME/questlog-mirror"
PREFIX_FROM="/Users/power"

log() { echo "[bridge $(date '+%F %T')] $*"; }

mkdir -p "$MIRROR/questlog-runtime"

# 1) 注册表（镜像注册表内 projects[].path 是 m5p 权威路径，GUI 经
#    data_root::map_read 前缀映射读到镜像副本）
if ! rsync -a "$REMOTE:.todo-board.json" "$MIRROR/.todo-board.json.tmp"; then
  log "FAIL registry pull"; exit 1
fi
mv "$MIRROR/.todo-board.json.tmp" "$MIRROR/.todo-board.json"

# 2) runtime 快照（执行状态/DR/台账——GUI 桥接期只读展示）
rsync -a \
  --exclude '*.lock' --exclude '*.tmp-*' --exclude '*.bak' --exclude 'logs/' \
  "$REMOTE:questlog-runtime/" "$MIRROR/questlog-runtime/" \
  || log "WARN runtime sync partial (continuing — stale view, not a failure)"

# 3) 各项目板文件（按镜像注册表枚举；单文件缺失只 WARN——项目可能暂无某文件）
python3 - "$MIRROR/.todo-board.json" "$MIRROR" "$PREFIX_FROM" <<'PY' > /tmp/ql-bridge-projects.$$
import json, sys
reg_path, mirror, prefix_from = sys.argv[1:4]
reg = json.load(open(reg_path))
for p in reg.get("projects", []):
    path = p.get("path", "")
    if path.startswith(prefix_from):
        suffix = path[len(prefix_from):].lstrip("/")
        for f in (".todos.json", ".epics.json"):
            print(f"{suffix}/{f}")
PY
while IFS= read -r rel; do
  [ -z "$rel" ] && continue
  dir="${rel%/*}"
  mkdir -p "$MIRROR/$dir"
  rsync -a "$REMOTE:/$PREFIX_FROM/$rel" "$MIRROR/$rel" 2>/dev/null \
    || log "WARN board file not on authority yet: $rel"
done < /tmp/ql-bridge-projects.$$
rm -f /tmp/ql-bridge-projects.$$

# 4) 标记最后写（内容 = data_root::RemoteConfig JSON）
printf '%s' "{\"prefix_from\":\"$PREFIX_FROM\",\"mirror_root\":\"$MIRROR\"}" \
  > "$MIRROR/REMOTE_ACTIVE.tmp"
mv "$MIRROR/REMOTE_ACTIVE.tmp" "$MIRROR/REMOTE_ACTIVE"

log "OK mirror synced (registry + runtime + board files)"
