#!/bin/sh
# pre-commit-guard.sh — QuestLog 个人信息泄露守卫
#
# 安装：软链到 .git/hooks/pre-commit（setup-mcp.sh 自动完成，现已被兑现；或手动）
#   ln -sf ../../scripts/pre-commit-guard.sh .git/hooks/pre-commit
#   chmod +x scripts/pre-commit-guard.sh
#
# 作用：git commit 时扫描暂存区的新增/修改行，发现下列敏感模式就阻止提交。
# 误报时用 `git commit --no-verify` 跳过（慎用）。

set -e

# ─── 白名单：守卫自身、文档、README、.gitignore、LICENSE ───
# 这些文件里的敏感字符串是"说明用例"，合法
WHITELIST='^scripts/pre-commit-guard\.sh$|\.md$|README|\.gitignore$|^LICENSE$|^docs/|^deploy/'

# 只看暂存区里"新增"的行（+ 开头，排除 +++ 文件头）
PATCH=$(git -c core.quotepath=false diff --cached --no-color --unified=0 --name-only)
# 收集新增行内容（仅非白名单文件）
ADDED=""
for f in $PATCH; do
  # 文件名匹配白名单 → 跳过
  if printf '%s' "$f" | grep -qE "$WHITELIST"; then continue; fi
  # 取该文件的新增行
  # "|| true": a brand-new or binary file can yield no added lines — an
  # empty grep match must not kill the whole hook under `set -e`.
  LINES=$(git -c core.quotepath=false diff --cached --no-color --unified=0 -- "$f" | grep -E '^\+' | grep -vE '^\+\+\+' || true)
  if [ -n "$LINES" ]; then
    ADDED="$ADDED
$LINES"
  fi
done

# 去掉前导空行
ADDED=$(printf '%s' "$ADDED" | sed '/^$/d')

if [ -z "$ADDED" ]; then
  exit 0  # 没有需要扫描的新增内容，放行
fi

# ─── 敏感模式（命中即阻止）───
# 1. 硬编码绝对路径：/Users/xxx /home/xxx /Volumes/xxx（合法的是 ~/ 相对路径）
# 2. 内网 IP 段：192.168. 10.0. 172.16-31.
# 3. 凭证：OpenAI sk- key、Bearer token、api_key=xxx
# 4. 数据文件被强加：.todos.json .epics.json .todo-board.json .codebase-memory
#
# 自定义（可选）：把你的主机名、私有项目名、业务上下文词填到 CUSTOM_PATTERNS，
# 用 | 分隔，例如 CUSTOM_PATTERNS='my-secret-project|my-hostname'。
# 默认为空——每个用户的环境不同，本文件不预置任何具体词。
CUSTOM_PATTERNS="${QUESTLOG_GUARD_PATTERNS:-}"
LEAKS=$(printf '%s\n' "$ADDED" | grep -nE \
  '/Users/[a-zA-Z]|/home/[a-zA-Z]|/Volumes/[a-zA-Z]|'\
'192\.168\.|10\.0\.[0-9]|172\.(1[6-9]|2[0-9]|3[01])\.|'\
"${CUSTOM_PATTERNS:+$CUSTOM_PATTERNS|}"\
'sk-[A-Za-z0-9]{20,}|[Bb]earer [A-Za-z0-9._-]{30,}|api[_-]?key["'"'"' ]*[:=]["'"'"' ]*[A-Za-z0-9]{16,}' \
  | head -20)

if [ -n "$LEAKS" ]; then
  cat <<EOF >&2

❌  pre-commit-guard: 检测到潜在个人信息/业务数据泄露，提交已阻止。

命中行（暂存区新增）：
$LEAKS

排查方向：
  - 个人路径 → 改用 ~/ 或 os.homedir()
  - 项目名/业务上下文 → 用通用示例（my-project / example）替代
  - 数据文件 → 检查 .gitignore 是否覆盖
  - 凭证 → 移到 .env（已被 .gitignore 排除）

确认是误报（如示例数据/文档引用）时，可强制跳过：
  git commit --no-verify

EOF
  exit 1
fi

# ─── 二次确认：暂存区里不应出现被 .gitignore 排除的文件被 -f 强加 ───
STAGED=$(git diff --cached --name-only)
FORBIDDEN=$(printf '%s\n' "$STAGED" | grep -E \
  '^\.todos\.json$|^\.epics\.json$|^\.todo-board\.json$|^\.codebase-memory/|^.questlog-hook/' \
  | head -5)
if [ -n "$FORBIDDEN" ]; then
  cat <<EOF >&2

❌  pre-commit-guard: 暂存区含个人数据文件，提交已阻止。

$FORBIDDEN

这些文件应在 .gitignore 中排除。如需移除：git reset HEAD <文件>
EOF
  exit 1
fi

exit 0
