#!/bin/bash
# board-git-backup.sh — 板数据 git 备份腿（S5 后置补强 2026-10-14）
# 与 NAS tar 快照腿（board-snapshot-nas.sh）互补：
#   - 本地 git 仓库（~/questlog-board-backup）：每日一次 commit，无限历史、可 diff/可回滚单文件
#   - NAS 异地：git bundle 单文件推 /volume2/git/（NAS 无 git 服务，bundle 是已验证通道），30 天轮转
# 覆盖：注册表 + 全部项目板文件 + questlog-runtime（执行台账/attempts——TM 之外的唯一异地副本）
# 排除：logs/、*.lock（pid 跨机无意义）、*.tmp-*（半写态）、*.bak（内部备份）、.DS_Store
# 调度：com.questlog.board-git-backup 每日 21:12（紧随 21:10 tar 快照，同窗错峰）
set -uo pipefail
: "${HOME:?HOME 未设置，拒绝执行}"

REPO="$HOME/questlog-board-backup"
REG="$HOME/.todo-board.json"
RUNTIME="$HOME/questlog-runtime"
LOG_DIR="$HOME/questlog-runtime/logs"
DAY=$(date +%Y%m%d)

log() { echo "$(date '+%F %T') $*" | tee -a "$LOG_DIR/board-git-backup.log"; }
mkdir -p "$LOG_DIR"

# 1) 本地仓库初始化（幂等）
if [ ! -d "$REPO/.git" ]; then
  git init -q "$REPO" || { log "FAIL git init"; exit 1; }
  git -C "$REPO" config user.name "questlog-backup"
  git -C "$REPO" config user.email "backup@questlog.local"
fi

# 2) 同步内容（cp 而非 rsync：保持 git 工作树简单；删旧文件保证检出态=当前态）
python3 - "$REPO" <<'PY'
import json, os, shutil, sys
repo = sys.argv[1]
reg = json.load(open(os.path.expanduser("~/.todo-board.json")))
# 注册表
shutil.copy2(os.path.expanduser("~/.todo-board.json"), os.path.join(repo, ".todo-board.json"))
# 各项目板文件（按注册表路径，<sanitized>/<file> 两级结构避免跨项目同名）
def sanitize(name): return name.replace("/", "_")
copied = 0
for p in reg.get("projects", []):
    d = p["path"]
    for f in (".todos.json", ".epics.json"):
        src = os.path.join(d, f)
        if os.path.isfile(src):
            dst_dir = os.path.join(repo, "boards", sanitize(p["name"]))
            os.makedirs(dst_dir, exist_ok=True)
            shutil.copy2(src, os.path.join(dst_dir, f))
            copied += 1
# runtime（浅结构文件；排除目录型数据中体积大且可再生者不在此列——attempts 保留）
rt_src = os.path.expanduser("~/questlog-runtime")
rt_dst = os.path.join(repo, "runtime")
os.makedirs(rt_dst, exist_ok=True)
rt_copied = 0
for root, dirs, files in os.walk(rt_src):
    dirs[:] = [x for x in dirs if x not in ("logs",)]
    rel = os.path.relpath(root, rt_src)
    for f in files:
        if f.endswith((".lock", ".bak", ".DS_Store")) or ".tmp-" in f or f.endswith(".bak-20260818-scopefix") or f.endswith(".bak-20260818-unobserve") or f.endswith(".bak-20260817-refix") or f.endswith(".pre-reanchor") or f.endswith(".s4-cleanup-bak") or f.endswith(".quarantine-bak"):
            continue
        src = os.path.join(root, f)
        dst = os.path.join(rt_dst, rel, f)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy2(src, dst)
        rt_copied += 1
# 清掉仓库里已消失的文件（保持检出态=当前态）
for base, sub in (("boards", None), ("runtime", None)):
    tgt = os.path.join(repo, base)
    if not os.path.isdir(tgt):
        continue
    for root, dirs, files in os.walk(tgt):
        rel = os.path.relpath(root, repo)
        src_base = os.path.expanduser("~") if base == "runtime" else None
        for f in files:
            repo_f = os.path.join(root, f)
            if base == "runtime":
                orig = os.path.join(rt_src, os.path.relpath(repo_f, rt_dst))
                if not os.path.isfile(orig):
                    os.remove(repo_f)
print(f"boards copied={copied} runtime copied={rt_copied}")
PY

# 3) 提交（有变化才 commit）
git -C "$REPO" add -A
if git -C "$REPO" diff --cached --quiet; then
  log "OK 无变化（幂等跳过）"
else
  git -C "$REPO" commit -q -m "board backup $DAY" && log "OK commit $DAY"
fi

# 4) NAS bundle（失败只 WARN——本地 git 历史已成，异地腿由 21:10 tar 兜底）
BUNDLE="/tmp/board-data-$DAY.bundle"
if git -C "$REPO" bundle create "$BUNDLE" --all >/dev/null 2>&1; then
  if cat "$BUNDLE" | ssh -o BatchMode=yes -o ConnectTimeout=10 nas \
       "cat > /volume2/git/board-data-$DAY.bundle.part && mv /volume2/git/board-data-$DAY.bundle.part /volume2/git/board-data-$DAY.bundle"; then
    log "OK bundle $(du -h "$BUNDLE" | cut -f1) -> nas"
  else
    log "FAIL bundle 传输（本地 git 已提交，仅异地缺失）"
  fi
  rm -f "$BUNDLE"
else
  log "WARN bundle create 失败"
fi
ssh -o BatchMode=yes -o ConnectTimeout=10 nas 'find /volume2/git -maxdepth 1 -name "board-data-*.bundle" -mtime +30 -delete' 2>/dev/null || true
