#!/usr/bin/env bash
# board-snapshot-nas.sh — 板数据 NAS 只读快照腿（施工手册 W02.6-2.7 / v3.4 板镜像腿落地）
# 语义：只读单向快照，非双写。板写者仍只有 M4 桌面 app/Rust 路径与 ZCode MCP；
#   本脚本只读复制（读时 JSON 校验，撕裂文件跳过并告警，绝不写板文件）。
# 覆盖：~/.todo-board.json 注册表列出的全部项目目录的 .todos.json/.epics.json
#   ＋注册表本体；每文件带 sha256 清单（W02.7：JSON/revision/hash 三验）。
# 通道：tar-over-ssh（W00 实测约束：NAS 无 git、rsync 路径被 rsyncd 拦、sftp 被拒）。
# 落点：nas:/volume2/db-backup/board-snapshots/<YYYYMMDD>/（同日重跑幂等覆盖）
# 保留：NAS 侧 30 天轮转；静态板不变属正常，不按 mtime 判故障（手册 W02.7）。
# 调度：com.questlog.board-snapshot 每日 21:10；launchd 对睡眠错过的日历任务
#   唤醒后补跑（macOS 语义），无需 RunAtLoad。
#   S4 后推送源= m5p（plist 迁移到 power 用户下，本脚本 $HOME 相对零改动）；
#   TopLEe bundle 腿已拆出 scripts/taopuli-bundle-nas.sh 留 M4（源仓不动）。
# 日志：~/questlog-runtime/logs/board-snapshot.log
set -euo pipefail
: "${HOME:?HOME 未设置，拒绝执行}"

REG="$HOME/.todo-board.json"
LOG_DIR="$HOME/questlog-runtime/logs"
DAY=$(date +%Y%m%d)
STAGE=$(mktemp -d /tmp/board-snap.XXXXXX)
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$LOG_DIR"
log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" | tee -a "$LOG_DIR/board-snapshot.log"; }

[ -f "$REG" ] || { log "FAIL 注册表不存在：$REG"; exit 1; }

# 1. 读注册表逐项目复制（JSON 完整性校验，坏文件跳过计数）
COPIED=$(python3 - "$REG" "$STAGE" <<'PY'
import json, sys, shutil, pathlib
reg_path, stage = sys.argv[1], pathlib.Path(sys.argv[2])
reg = json.load(open(reg_path))
copied, skipped = 0, []
for p in reg.get("projects", []):
    d = pathlib.Path(p["path"]).expanduser()
    for name in (".todos.json", ".epics.json"):
        f = d / name
        if not f.is_file():
            continue
        try:
            json.loads(f.read_text())
        except Exception as e:
            skipped.append(f"{f}: {e}")
            continue
        shutil.copy2(f, stage / f"{p['name']}__{name}")
        copied += 1
shutil.copy2(reg_path, stage / "_registry.todo-board.json")
print(f"{copied+1} {len(skipped)}")
for s in skipped:
    print(f"WARN {s}", file=sys.stderr)
PY
) || { log "FAIL 快照 staged 失败"; exit 1; }
SKIPPED=$(echo "$COPIED" | awk '{print $2}')
TOTAL=$(echo "$COPIED" | awk '{print $1}')
[ "$SKIPPED" = "0" ] || log "WARN $SKIPPED 个板文件 JSON 校验失败被跳过"

# 2. sha256 清单（排除 manifest 自身，避免自引用）
( cd "$STAGE" && find . -maxdepth 1 -type f ! -name '_manifest.sha256' -exec shasum -a 256 {} + > _manifest.sha256 )

# 3. tar-over-ssh 推送（幂等：同日重跑覆盖同名）
if { cd "$STAGE" && tar --no-xattrs -cf - .; } | ssh -o BatchMode=yes -o ConnectTimeout=10 \
     nas "mkdir -p /volume2/db-backup/board-snapshots/$DAY && tar xf - -C /volume2/db-backup/board-snapshots/$DAY && chmod -R go-rwx /volume2/db-backup/board-snapshots"; then
  log "OK $DAY files=$TOTAL skipped=$SKIPPED"
else
  log "FAIL $DAY 推送失败（本地暂存已弃，不造成功戳）"
  exit 1
fi

# 4. 30 天轮转
ssh -o BatchMode=yes -o ConnectTimeout=10 nas \
  'find /volume2/db-backup/board-snapshots -maxdepth 1 -mindepth 1 -type d -name "20*" -mtime +30 -exec rm -rf {} +' 2>/dev/null || true
