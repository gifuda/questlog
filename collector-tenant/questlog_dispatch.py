"""QuestLog 租户派发器（待办租户 GX）——平台白名单脚本候选（allowlist 名 questlog-dispatch）。

一轮 = 拉取经理机 outbox → 校验 → 提交 TaskSpec（幂等键=task_key）→
检查终态任务 → 写回执到经理机 inbox → 归档已回执的 outbox 条目。

调度：平台 scheduler 注册（禁自建 launchd）。派发仅在夜间窗口
（19:00–21:00 UTC = 03:00–06:00 +08:00，与经理机 sweep 同窗）；回执半程
不受窗口限制（对账恢复语义，与经理机 G4 一致）。

安全边界：
- SSH 全部 BatchMode + 固定路径 + 已校验 64-hex task_key；自由文本只经
  scp 文件传输，永不进 shell 参数。
- 只读面：经理机 outbox 列表/取文件、git fetch 输入仓库。
- 写入面（仅两处）：经理机 todo-receipts/<task_key>.json、
  todo-outbox/archived/ 归档移动。
"""

from __future__ import annotations

import json
import sqlite3
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from collector_node import db as platform_db  # noqa: E402
from collector_node import queue as platform_queue  # noqa: E402
from collector_node.contracts import TaskSpec  # noqa: E402
from collector_node.tenants import questlog as adapter  # noqa: E402

MANAGER = "manager"
OUTBOX_DIR = "~/questlog-runtime/todo-outbox"
RECEIPTS_DIR = "~/questlog-runtime/todo-receipts"
TENANT_DATA = Path("/Users/Shared/CollectorData/tenants/questlog")
REPOS_ROOT = TENANT_DATA / "repos"
REPORTS_ROOT = TENANT_DATA / "reports"

TERMINAL_STATUSES = {"succeeded", "degraded", "failed", "cancelled", "orphaned"}


def _ssh(args: list[str], *, check: bool = True) -> subprocess.CompletedProcess:
    cmd = ["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new", MANAGER, *args]
    return subprocess.run(cmd, capture_output=True, text=True, check=check)


def in_dispatch_window(now: datetime | None = None) -> bool:
    """03:00–06:00 +08:00 == 19:00–21:00 UTC（派发窗口与经理机 sweep 同窗）。

    QUESTLOG_DISPATCH_FORCE=1 仅限手动运维/影子联调旁路窗口——launchd 调度
    环境不携带该变量，窗口语义不受影响。"""
    import os

    if os.environ.get("QUESTLOG_DISPATCH_FORCE") == "1":
        return True
    utc_hour = (now or datetime.now(timezone.utc)).hour
    return 19 <= utc_hour < 21


def list_outbox() -> list[str]:
    proc = _ssh([f"ls -1 {OUTBOX_DIR} 2>/dev/null"], check=False)
    if proc.returncode != 0:
        print(f"[questlog-dispatch] outbox listing failed: {proc.stderr.strip()}", file=sys.stderr)
        return []
    return [line.strip() for line in proc.stdout.splitlines() if line.strip().endswith(".json")]


def fetch_delivery(name: str) -> dict | None:
    with tempfile.TemporaryDirectory() as tmp:
        local = Path(tmp) / name
        proc = subprocess.run(
            ["scp", "-q", "-o", "BatchMode=yes", f"{MANAGER}:{OUTBOX_DIR}/{name}", str(local)],
            capture_output=True, text=True, check=False,
        )
        if proc.returncode != 0:
            print(f"[questlog-dispatch] scp {name} failed: {proc.stderr.strip()}", file=sys.stderr)
            return None
        try:
            return json.loads(local.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as e:
            print(f"[questlog-dispatch] delivery {name} unreadable: {e}", file=sys.stderr)
            return None


def ensure_snapshot(repo_root: str, base_sha: str) -> Path | None:
    """输入快照：经理机仓库按 base_sha 的只读分离检出（已存在则跳过）。"""
    repo_dir = REPOS_ROOT / adapter.sanitize_segment(repo_root) / base_sha[:12]
    if (repo_dir / ".git").exists():
        return repo_dir
    repo_dir.parent.mkdir(parents=True, exist_ok=True)
    url = f"{MANAGER}:{repo_root}"
    for cmd in (
        ["git", "clone", "--quiet", "--no-checkout", url, str(repo_dir)],
        ["git", "-C", str(repo_dir), "fetch", "--quiet", "origin", base_sha],
        ["git", "-C", str(repo_dir), "checkout", "--quiet", "--detach", base_sha],
    ):
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        if proc.returncode != 0:
            print(f"[questlog-dispatch] snapshot failed ({cmd[1]}): {proc.stderr.strip()}", file=sys.stderr)
            return None
    return repo_dir


def submit_delivery(conn: sqlite3.Connection, msg: dict) -> str | None:
    task_id = f"questlog-{adapter.sanitize_segment(str(msg['task_key']))[:40]}"
    spec: TaskSpec = adapter.build_task_spec(msg, task_id=task_id, args=adapter.build_args(msg))
    result = platform_queue.submit(conn, spec)
    print(f"[questlog-dispatch] submit {task_id}: created={result.created} status={result.status}")
    return result.task_id


def terminal_questlog_tasks(conn: sqlite3.Connection) -> list[sqlite3.Row]:
    placeholders = ",".join("?" for _ in TERMINAL_STATUSES)
    return conn.execute(
        f"SELECT task_id, idempotency_key, status FROM tasks "
        f"WHERE tenant = 'questlog' AND status IN ({placeholders}) ORDER BY created_at",
        tuple(TERMINAL_STATUSES),
    ).fetchall()


def write_receipt(row: sqlite3.Row, outbox_name: str | None) -> bool:
    task_key = row["idempotency_key"]
    report_path = REPORTS_ROOT / task_key / "report.md"
    # Repo cleanliness: the delivery's OWN snapshot must be pristine — any
    # dirt is a read-only violation (the only write surface is report_dir).
    changes_detected = False
    delivery_file = REPORTS_ROOT / f"{task_key}.delivery.json"
    if delivery_file.exists():
        try:
            delivery = json.loads(delivery_file.read_text(encoding="utf-8"))
            snapshot = (
                REPOS_ROOT
                / adapter.sanitize_segment(str(delivery.get("repo_root", "")))
                / str(delivery.get("base_sha", ""))[:12]
            )
            dirty = subprocess.run(
                ["git", "-C", str(snapshot), "status", "--porcelain"],
                capture_output=True, text=True, check=False,
            )
            changes_detected = bool(dirty.returncode == 0 and dirty.stdout.strip())
        except (OSError, json.JSONDecodeError):
            changes_detected = False
    report_stdout = ""
    if report_path.exists():
        report_stdout = report_path.read_text(encoding="utf-8", errors="replace")[: 2 * 1024 * 1024]
    receipt = {
        "version": 1,
        "task_key": task_key,
        "attempt_id": f"{row['task_id']}-001",
        "platform_task_id": row["task_id"],
        "exit_ok": row["status"] == "succeeded",
        "timed_out": row["status"] == "failed",
        "budget_exceeded": False,
        "changes_detected": changes_detected,
        "report_stdout": report_stdout,
        "received_at": datetime.now(timezone.utc).isoformat(),
    }
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as tmp:
        json.dump(receipt, tmp, ensure_ascii=False)
        tmp_path = tmp.name
    proc = subprocess.run(
        ["scp", "-q", "-o", "BatchMode=yes", tmp_path, f"{MANAGER}:{RECEIPTS_DIR}/{task_key}.json"],
        capture_output=True, text=True, check=False,
    )
    if proc.returncode != 0:
        print(f"[questlog-dispatch] receipt scp failed: {proc.stderr.strip()}", file=sys.stderr)
        return False
    if outbox_name:
        _ssh([f"mkdir -p {OUTBOX_DIR}/archived && mv {OUTBOX_DIR}/{outbox_name} {OUTBOX_DIR}/archived/"], check=False)
    return True


def main() -> int:
    conn = platform_db.connect("/Users/Shared/CollectorPlatform/state/control.db")

    dispatched: set[str] = set()
    if in_dispatch_window():
        for name in list_outbox():
            msg = fetch_delivery(name)
            if msg is None:
                continue
            try:
                msg = adapter.validate_delivery(msg)
            except adapter.DeliveryError as e:
                print(f"[questlog-dispatch] invalid delivery {name}: {e}", file=sys.stderr)
                continue
            # 持久化投递消息（research 脚本与回执取证都要用）
            REPORTS_ROOT.mkdir(parents=True, exist_ok=True)
            delivery_local = REPORTS_ROOT / f"{msg['task_key']}.delivery.json"
            if not delivery_local.exists():
                delivery_local.write_text(json.dumps(msg, ensure_ascii=False, indent=2), encoding="utf-8")
            if ensure_snapshot(str(msg["repo_root"]), str(msg["base_sha"])) is None:
                print(f"[questlog-dispatch] snapshot failed for {name}; retrying next pass", file=sys.stderr)
                continue
            submit_delivery(conn, msg)
            dispatched.add(str(msg["task_key"]))

    # 回执半程（不受窗口限制）
    outbox_now = set(list_outbox())
    for row in terminal_questlog_tasks(conn):
        task_key = row["idempotency_key"]
        outbox_name = f"{task_key}.json"
        # 只有「回执尚未写过」的任务才写：outbox 条目还在（未归档）即视为待回执
        if outbox_name not in outbox_now and task_key not in dispatched:
            continue
        write_receipt(row, outbox_name if outbox_name in outbox_now else None)
    return 0


if __name__ == "__main__":
    sys.exit(main())
