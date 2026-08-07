#!/bin/bash
# QuestLog — one-shot MCP server setup for any MCP client
#
# Usage:
#   bash setup-mcp.sh                       → auto-detect installed clients, register
#   bash setup-mcp.sh --client claude,cursor→ register only the named clients
#   bash setup-mcp.sh --all                 → attempt all 5 clients (ignore detection)
#   bash setup-mcp.sh --help                → show this help
#   bash setup-mcp.sh <project-dir> [...]   → legacy per-project .mcp.json writer
#
# Supported clients: Claude Code · Cursor · ZCode · Codex · OpenCode

set -u
export LC_ALL="${LC_ALL:-en_US.UTF-8}"
export LANG="${LANG:-en_US.UTF-8}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MCP_SERVER="$SCRIPT_DIR/mcp-server/dist/index.js"
NODE_BIN="$(command -v node || true)"

# ---------- helpers ----------

print_help() {
  sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
}

# Resolve an absolute, symlink-stripped path. Falls back to the input if realpath
# is unavailable or the target doesn't exist yet.
norm_path() {
  if command -v realpath >/dev/null 2>&1; then
    realpath "$1" 2>/dev/null || printf '%s' "$1"
  else
    printf '%s' "$1"
  fi
}

# Idempotent JSON merge via a throwaway .mjs file (never node -e string concat,
# so paths containing spaces / unicode / quotes survive the bash→node boundary).
# Args: <configFile> <keyPath...> <serverJson>
#   keyPath is a dotted path like "mcpServers" or "mcp.servers".
# Writes a timestamped .bak before mutating. Aborts (no overwrite) on corrupt JSON.
merge_json() {
  local file="$1"; shift
  local keypath="$1"; shift
  local server_json="$1"; shift
  local tmp_mjs
  tmp_mjs="$(mktemp -t questlog-merge.XXXXXX)"
  tmp_mjs="$tmp_mjs.mjs"
  # mktemp -t on macOS gives TMPDIR/questlog-merge.XXXXXX; rename to .mjs for ESM.
  cat > "$tmp_mjs" <<'MJS'
import { readFileSync, writeFileSync, existsSync } from "node:fs";
const [file, keypath, serverJsonRaw] = process.argv.slice(2);
const incoming = JSON.parse(serverJsonRaw);
const keys = keypath.split(".");
function nodeOf(root) {
  let cur = root;
  for (const k of keys) {
    if (typeof cur !== "object" || cur === null || Array.isArray(cur)) {
      console.error(`merge: path segment "${k}" hits non-object — aborting`);
      process.exit(3);
    }
    if (cur[k] === undefined) cur[k] = {};
    cur = cur[k];
  }
  return cur;
}
function transportEq(a, b) {
  if (!a || typeof a !== "object") return false;
  const norm = (s) => s && s.replace(/\/+$/, "");
  return norm(a.command) === norm(b.command)
    && JSON.stringify(a.args || []) === JSON.stringify(b.args || []);
}
let raw = "";
if (existsSync(file)) raw = readFileSync(file, "utf8");
let root;
if (raw.trim() === "") {
  root = {};
} else {
  try { root = JSON.parse(raw); }
  catch (e) {
    console.error(`merge: ${file} is not valid JSON — left untouched (fix manually)`);
    process.exit(4);
  }
}
if (typeof root !== "object" || root === null || Array.isArray(root)) {
  console.error(`merge: ${file} root is not an object — aborting`);
  process.exit(5);
}
const leaf = nodeOf(root);
if (leaf["todo-board"] === undefined) {
  leaf["todo-board"] = incoming;
  writeFileSync(file, JSON.stringify(root, null, 2) + "\n");
  console.log("registered");
} else if (transportEq(leaf["todo-board"], incoming)) {
  console.log("already-registered");
} else {
  const existing = leaf["todo-board"];
  const merged = { ...existing, command: incoming.command, args: incoming.args };
  leaf["todo-board"] = merged;
  writeFileSync(file, JSON.stringify(root, null, 2) + "\n");
  console.log("updated-transport (preserved user fields)");
}
MJS
  if [ ! -f "$file" ]; then
    mkdir -p "$(dirname "$file")"
    printf '{}\n' > "$file"
  else
    # timestamped backup of any pre-existing content
    cp -p "$file" "$file.bak.$(date +%Y%m%d%H%M%S)" 2>/dev/null || true
  fi
  node "$tmp_mjs" "$file" "$keypath" "$server_json"
  local rc=$?
  rm -f "$tmp_mjs"
  return $rc
}

# ---------- build guard ----------
if [ ! -f "$MCP_SERVER" ]; then
  echo "MCP server not built — installing deps and compiling..."
  (cd "$SCRIPT_DIR/mcp-server" && npm install && npm run build)
  if [ ! -f "$MCP_SERVER" ]; then
    echo "Build failed. Run manually: cd mcp-server && npm install && npm run build"
    exit 1
  fi
  echo "MCP server built."
fi

if [ -z "$NODE_BIN" ]; then
  echo "error: 'node' not found on PATH — install Node.js 18+ first." >&2
  exit 1
fi

# Canonical server entry. NODE_BIN & MCP_SERVER are absolute; transport fields only.
SERVER_JSON=$(node -e 'process.stdout.write(JSON.stringify({command:process.argv[1],args:[process.argv[2]]}))' "$NODE_BIN" "$MCP_SERVER")

# ---------- client registrations ----------

# Each register_* function echoes one line: <status>\t<detail>
# status ∈ {ok, skip, fail}. Caller aggregates into the summary table.

reg_claude() {
  if ! command -v claude >/dev/null 2>&1; then echo "skip	(not installed)"; return; fi
  if claude mcp list 2>/dev/null | grep -q "todo-board"; then
    echo "ok	already registered"
    return
  fi
  if claude mcp add --scope user todo-board -- "$NODE_BIN" "$MCP_SERVER" >/dev/null 2>&1; then
    echo "ok	registered"
  else
    echo "fail	(claude mcp add returned $?)"
  fi
}

reg_cursor() {
  local f="$HOME/.cursor/mcp.json"
  if ! command -v cursor >/dev/null 2>&1 && [ ! -f "$f" ]; then
    echo "skip	(not installed, no config)"; return
  fi
  local out
  if out=$(merge_json "$f" "mcpServers" "$SERVER_JSON" 2>&1); then
    echo "ok	$out"
  else
    echo "fail	($out)"
  fi
}

reg_zcode() {
  local f="$HOME/.zcode/cli/config.json"
  if ! command -v zcode >/dev/null 2>&1 && [ ! -f "$f" ]; then
    echo "skip	(not installed, no config)"; return
  fi
  local out
  if out=$(merge_json "$f" "mcp.servers" "$SERVER_JSON" 2>&1); then
    echo "ok	$out"
  else
    echo "fail	($out)"
  fi
}

reg_codex() {
  local f="$HOME/.codex/config.toml"
  if ! command -v codex >/dev/null 2>&1; then
    if [ ! -f "$f" ]; then echo "skip	(not installed, no config)"; return; fi
    echo "fail	(codex CLI missing; edit $f manually)"
    return
  fi
  if codex mcp add todo-board -- "$NODE_BIN" "$MCP_SERVER" >/dev/null 2>&1; then
    echo "ok	registered"
  else
    echo "fail	(codex mcp add returned $?; edit $f manually)"
  fi
}

reg_opencode() {
  local f="$HOME/.config/opencode/config.json"
  if ! command -v opencode >/dev/null 2>&1; then
    if [ ! -f "$f" ]; then echo "skip	(not installed, no config)"; return; fi
    echo "skip	(CLI missing but config exists — verify manually at $f)"
    return
  fi
  if opencode mcp add todo-board -- "$NODE_BIN" "$MCP_SERVER" >/dev/null 2>&1; then
    echo "ok	registered"
  else
    echo "fail	(opencode mcp add returned $?)"
  fi
}

# ---------- arg parsing ----------

MODE="auto"          # auto | explicit | all | perproject
EXPLICIT_CLIENTS=""

while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) print_help; exit 0 ;;
    --all) MODE="all"; shift ;;
    --client)
      if [ -z "${2:-}" ]; then echo "error: --client needs a value" >&2; exit 2; fi
      MODE="explicit"
      EXPLICIT_CLIENTS="$2"
      shift 2
      ;;
    --client=*) MODE="explicit"; EXPLICIT_CLIENTS="${1#--client=}"; shift ;;
    --*) echo "error: unknown flag '$1' (try --help)" >&2; exit 2 ;;
    *) MODE="perproject"; break ;;   # positional → legacy per-project mode
  esac
done

# ---------- mode: per-project .mcp.json (legacy, unchanged) ----------
if [ "$MODE" = "perproject" ]; then
  for dir in "$@"; do
    abs=$(cd "$dir" 2>/dev/null && pwd) || { echo "skipping (no such dir): $dir"; continue; }
    target="$abs/.mcp.json"
    if [ -f "$target" ]; then
      echo "skip (already has .mcp.json): $abs"
    else
      printf '{"mcpServers":{"todo-board":{"command":"%s","args":["%s"]}}}\n' "$NODE_BIN" "$MCP_SERVER" > "$target"
      echo "wrote $target"
    fi
  done
  exit 0
fi

# ---------- mode dispatch: which clients to try ----------
ALL_CLIENTS="claude cursor zcode codex opencode"
valid_client() { case "$1" in claude|cursor|zcode|codex|opencode) return 0;; *) return 1;; esac; }

# Bash 3.2 (macOS default) has no associative arrays / herestrings — use plain
# parallel strings indexed by position.
RESULTS=""
DETAILS=""
ok_count=0; fail_count=0; skip_count=0

record() {
  # record <client> <statusLine>  — statusLine = "<status>\t<detail>"
  local client="$1" line="$2"
  local status="${line%%	*}"
  local detail="${line#*	}"
  RESULTS="$RESULTS $client=$status"
  DETAILS="$DETAILS
$client	$status	$detail"
  case "$status" in
    ok)   ok_count=$((ok_count+1)) ;;
    fail) fail_count=$((fail_count+1)) ;;
    skip) skip_count=$((skip_count+1)) ;;
  esac
}

if [ "$MODE" = "explicit" ]; then
  TARGETS=""
  # Bash 3.2 safe comma-split (no herestring): use IFS in a loop
  _saved_IFS="$IFS"
  IFS=','
  for c in $EXPLICIT_CLIENTS; do
    IFS="$_saved_IFS"
    c="$(printf '%s' "$c" | tr -d ' ')"
    [ -z "$c" ] && continue
    if ! valid_client "$c"; then
      echo "error: unknown client '$c'. Valid: $ALL_CLIENTS" >&2
      exit 1
    fi
    TARGETS="$TARGETS $c"
  done
  IFS="$_saved_IFS"
else
  TARGETS="$ALL_CLIENTS"   # auto & all share the full set; per-client funcs gate themselves
fi

# ---------- run ----------
for c in $TARGETS; do
  case "$c" in
    claude)   record claude "$(reg_claude)" ;;
    cursor)   record cursor "$(reg_cursor)" ;;
    zcode)    record zcode "$(reg_zcode)" ;;
    codex)    record codex "$(reg_codex)" ;;
    opencode) record opencode "$(reg_opencode)" ;;
  esac
done

# ---------- summarize ----------
echo ""
echo "=== QuestLog MCP setup summary ==="
printf '%-10s %-8s %s\n' "CLIENT" "STATUS" "DETAIL"
echo "$DETAILS" | tail -n +2 | while IFS='	' read -r c s d; do
  [ -n "$c" ] && printf '%-10s %-8s %s\n' "$c" "$s" "$d"
done
echo ""

# ---------- pre-commit guard install (fulfils README/CONTRIBUTING/SECURITY promise) ----------
GIT_DIR="$SCRIPT_DIR/.git"
if [ -d "$GIT_DIR" ]; then
  HOOK="$GIT_DIR/hooks/pre-commit"
  GUARD="$SCRIPT_DIR/scripts/pre-commit-guard.sh"
  if [ -x "$GUARD" ]; then
    if [ ! -e "$HOOK" ] || [ "$(readlink "$HOOK" 2>/dev/null)" != "$GUARD" ]; then
      ln -sf "$GUARD" "$HOOK" && echo "pre-commit guard: linked → $HOOK"
    else
      echo "pre-commit guard: already linked"
    fi
  fi
fi

# ---------- exit code ----------
if [ $fail_count -gt 0 ]; then
  exit 1
elif [ $ok_count -gt 0 ]; then
  exit 0
else
  echo "No MCP client detected. Install Claude Code / Cursor / ZCode / Codex / OpenCode, or use:" >&2
  echo "  bash setup-mcp.sh --client <name>   # force a specific client" >&2
  echo "  bash setup-mcp.sh <project-dir>    # write a per-project .mcp.json" >&2
  exit 2
fi
