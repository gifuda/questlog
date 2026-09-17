"""QuestLog 租户适配器（待办租户 GX）。

纯函数模块，零 I/O、零平台 import（build_task_spec 惰性 import 契约）——
验证逻辑可在任何机器单测；TaskSpec 构造仅在采集机平台侧使用。

落位：collector_node/tenants/questlog.py（executor 施工项）。
"""

from __future__ import annotations

import re

HEX64 = re.compile(r"^[0-9a-f]{64}$")
SHA1 = re.compile(r"^[0-9a-f]{40}$")
TENANT = "questlog"

MAX_OBJECTIVE_BYTES = 32 * 1024
MAX_TIMEOUT_SECONDS = 900
MIN_TIMEOUT_SECONDS = 60

REQUIRED_FIELDS = (
    "version",
    "task_key",
    "tenant_id",
    "todo_id",
    "package_id",
    "package_hash",
    "objective",
    "repo_root",
    "base_sha",
    "timeout_seconds",
)


class DeliveryError(ValueError):
    """投递消息不合法——该消息被跳过并记录，不产生任何任务。"""


def validate_delivery(msg: dict) -> dict:
    """校验经理机 todo-outbox 投递消息；通过则原样返回，否则 DeliveryError。"""
    if not isinstance(msg, dict):
        raise DeliveryError("delivery message is not an object")
    for field in REQUIRED_FIELDS:
        if field not in msg:
            raise DeliveryError(f"missing field: {field}")
    if msg["version"] != 1:
        raise DeliveryError(f"unsupported version: {msg['version']!r}")
    if not HEX64.match(str(msg["task_key"])):
        raise DeliveryError("task_key must be 64 lowercase hex chars")
    if not HEX64.match(str(msg["package_hash"])):
        raise DeliveryError("package_hash must be 64 lowercase hex chars")
    if not SHA1.match(str(msg["base_sha"])):
        raise DeliveryError("base_sha must be 40 lowercase hex chars")
    if str(msg["tenant_id"]) != TENANT:
        raise DeliveryError(f"tenant_id must be {TENANT!r}")
    for field in ("todo_id", "package_id", "repo_root"):
        value = str(msg[field])
        if not value or "\x00" in value:
            raise DeliveryError(f"{field} must be a non-empty safe string")
    objective = str(msg["objective"])
    if not objective.strip():
        raise DeliveryError("objective is empty")
    if len(objective.encode("utf-8")) > MAX_OBJECTIVE_BYTES:
        raise DeliveryError(f"objective exceeds {MAX_OBJECTIVE_BYTES} bytes")
    try:
        timeout = int(msg["timeout_seconds"])
    except (TypeError, ValueError):
        raise DeliveryError("timeout_seconds must be an integer") from None
    if not (MIN_TIMEOUT_SECONDS <= timeout <= MAX_TIMEOUT_SECONDS):
        raise DeliveryError(
            f"timeout_seconds out of bounds [{MIN_TIMEOUT_SECONDS},{MAX_TIMEOUT_SECONDS}]"
        )
    return msg


def clamp_timeout(raw) -> int:
    try:
        return max(MIN_TIMEOUT_SECONDS, min(MAX_TIMEOUT_SECONDS, int(raw)))
    except (TypeError, ValueError):
        return MIN_TIMEOUT_SECONDS


def sanitize_segment(value: str) -> str:
    """路径段消毒：只留字母数字-_，其余换 '_'（task_key/todo_id/package_id 均安全）。"""
    return re.sub(r"[^A-Za-z0-9._-]", "_", value)[:80]


def build_args(msg: dict) -> list[str]:
    """research 脚本的 argv（执行器参数白名单只认 {script, args}）。"""
    reports = "/Users/Shared/CollectorData/tenants/questlog/reports"
    repo_dir = (
        "/Users/Shared/CollectorData/tenants/questlog/repos/"
        f"{sanitize_segment(str(msg['repo_root']))}/{str(msg['base_sha'])[:12]}"
    )
    return [f"{reports}/{msg['task_key']}.delivery.json", repo_dir, reports]


def build_task_spec(msg: dict, *, task_id: str, args: list[str]) -> "object":
    """构造平台 TaskSpec（惰性 import 平台契约——仅采集机侧可调用）。"""
    from collector_node.contracts import (
        Acceptance,
        Approval,
        ExecutorRef,
        InputRef,
        OutputRef,
        ResourceBudget,
        RetryPolicy,
        TaskSpec,
    )

    msg = validate_delivery(msg)
    return TaskSpec(
        task_id=task_id,
        tenant=TENANT,
        task_type="todo_research",
        executor=ExecutorRef(kind="python", name="questlog-research"),
        inputs=[
            InputRef(logical_uri=f"questlog://delivery/{msg['task_key']}", sha256=str(msg["package_hash"])),
            InputRef(
                logical_uri=f"questlog://snapshot/{sanitize_segment(str(msg['repo_root']))}@{msg['base_sha']}",
                sha256=None,
            ),
        ],
        outputs=[
            OutputRef(logical_uri=f"questlog://report/{msg['task_key']}", content_type="text/markdown"),
        ],
        parameters={"args": args},
        resource_budget=ResourceBudget(
            max_wall_seconds=clamp_timeout(msg["timeout_seconds"]),
            max_cpu_seconds=None,
            max_memory_mb=None,
            max_disk_mb=None,
            max_output_bytes=2 * 1024 * 1024,
        ),
        retry_policy=RetryPolicy(max_attempts=1, backoff_seconds=5, backoff_multiplier=1.0),
        idempotency_key=str(msg["task_key"]),
        side_effect_class="read_only",
        approval=Approval(required=False, approver=None, rationale=None),
        acceptance=Acceptance(kind="exit_code_zero", expect=None),
    )
