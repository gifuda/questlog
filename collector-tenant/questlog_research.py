"""QuestLog 研究执行器（待办租户 GX）——平台白名单脚本候选（allowlist 名 questlog-research）。

只读研究任务：读投递消息 → 构造只读研究信封 → 调 zcode headless（或 shadow
桩）→ 报告 stdout 落 report_dir/<task_key>/report.md。

调用形态：TaskSpec parameters.args = [delivery_path, repo_dir, report_dir]
（逐字 execve，无 shell）。

配置注入面（D3 裁定：租户数据目录配置文件，环境变量仅作手动覆盖）：
  /Users/Shared/CollectorData/tenants/questlog/zcode.bin   —— zcode 可执行文件绝对路径
  /Users/Shared/CollectorData/tenants/questlog/stub.mode   —— 存在即桩模式
                                                             （固定 NEEDS_REVIEW 报告，shadow 联调用）
两者缺省时：无 zcode.bin 且无 stub.mode → 干净失败（exit 5）。

平台产物契约：spec.outputs 的 logical_uri basename（=<task_key>）必须出现在
平台注入的 COLLECTER_OUTPUT_DIR——那是产物登记与 sha256 复算的管辖面；
report_dir/<task_key>/report.md 是租户侧持久副本（回执取证用）。

红线：不写 repo_dir 内任何文件；写面仅上述两处。
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

STUB_REPORT = """```json
{{
  "schema_version": "questlog-todo-report",
  "task_key": "{task_key}",
  "verdict": "NEEDS_REVIEW",
  "summary": "shadow 桩执行器产出：全链联调报告，等待 zcode 实接。",
  "findings": ["stub 模式未执行真实研究"],
  "evidence_refs": [],
  "unresolved": ["zcode headless 未接入（手册 F-2）"],
  "limitations": ["stub"]
}}
```
"""

ENVELOPE = """{objective}

【执行纪律（硬约束）】
1. 本任务为只读研究：不得创建、修改、删除仓库内任何文件。
2. 完成后，你的 stdout 末尾必须且只能包含一个 ```json 围栏块，内容为结构化报告：
   schema_version="questlog-todo-report"，task_key="{task_key}"，
   verdict ∈ DONE | CANNOT_AUTO | NEEDS_REVIEW，
   字段：summary（结论摘要）、findings（要点列表）、evidence_refs（证据引用，
   格式 "相对路径:行号"，必须指向仓库中真实存在的文件）、unresolved（未决项，
   DONE 时必须为空）、limitations（局限）、cannot_auto_reason（仅 CANNOT_AUTO）。
3. DONE 只是声明：宿主验证器会核对结构、任务绑定与证据引用，伪造会被降级。
"""


def write_report(content: str, task_key: str, report_file: Path) -> None:
    """双落点：平台输出目录（basename=task_key，产物登记面）+ 租户持久副本。"""
    output_dir = os.environ.get("COLLECTOR_OUTPUT_DIR", "")
    if output_dir:
        od = Path(output_dir)
        od.mkdir(parents=True, exist_ok=True)
        (od / task_key).write_text(content, encoding="utf-8")
    report_file.parent.mkdir(parents=True, exist_ok=True)
    report_file.write_text(content, encoding="utf-8")


def run_stub(task_key: str, report_file: Path) -> None:
    write_report(STUB_REPORT.format(task_key=task_key), task_key, report_file)


def run_zcode(zcode_bin: str, prompt: str, repo_dir: str, task_key: str, report_file: Path, timeout_secs: int = 840) -> int:
    # 探针：CLI 必须可用（只读 --version）
    probe = subprocess.run([zcode_bin, "--version"], capture_output=True, text=True, check=False)
    if probe.returncode != 0:
        print(f"zcode probe failed: {probe.stderr.strip()}", file=sys.stderr)
        return 5
    try:
        proc = subprocess.run(
            [
                zcode_bin,
                "--prompt", prompt,
                "--cwd", repo_dir,
                "--mode", "yolo",
                # 只读研究在 CLI 层的硬约束：拒绝一切写工具（双保险，
                # 与平台 read_only side_effect_class 互补）
                "--disallowedTools", "Bash Edit Write MultiEdit NotebookEdit WebFetch",
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=840,
        )
    except subprocess.TimeoutExpired:
        print("zcode timed out", file=sys.stderr)
        return 124
    write_report(proc.stdout or "", task_key, report_file)
    return proc.returncode


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: questlog_research.py <delivery_path> <repo_dir> <report_dir>", file=sys.stderr)
        return 2
    delivery_path, repo_dir, report_dir = (Path(a) for a in argv)
    try:
        msg = json.loads(delivery_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        print(f"delivery unreadable: {e}", file=sys.stderr)
        return 2
    task_key = str(msg.get("task_key", ""))
    if not task_key:
        print("delivery missing task_key", file=sys.stderr)
        return 2
    report_file = Path(report_dir) / task_key / "report.md"

    tenant_cfg = Path("/Users/Shared/CollectorData/tenants/questlog")
    stub = os.environ.get("QUESTLOG_RESEARCH_STUB") == "1" or (tenant_cfg / "stub.mode").exists()
    zcode_bin = os.environ.get("QUESTLOG_ZCODE_BIN", "").strip()
    if not zcode_bin:
        bin_file = tenant_cfg / "zcode.bin"
        if bin_file.exists():
            zcode_bin = bin_file.read_text(encoding="utf-8").strip()
    if zcode_bin and not Path(zcode_bin).exists():
        zcode_bin = ""
    if stub:
        # Explicit stub intent (env or marker file) ALWAYS wins — shadow
        # drill/联调 must be deterministic even when zcode is configured.
        run_stub(task_key, report_file)
        return 0
    if not zcode_bin:
        print("zcode unavailable (tenants/questlog/zcode.bin) and stub mode off", file=sys.stderr)
        return 5

    prompt = ENVELOPE.format(objective=str(msg.get("objective", "")), task_key=task_key)
    # Inner timeout tracks the task's own budget (delivery timeout_seconds),
    # leaving 60s of headroom for report writing — the platform's wall-clock
    # kill remains the outer authority.
    try:
        budget = int(msg.get("timeout_seconds", 900))
    except (TypeError, ValueError):
        budget = 900
    budget = max(120, min(3600, budget))
    rc = run_zcode(zcode_bin, prompt, repo_dir, task_key, report_file, budget - 60)
    if rc != 0:
        print(f"zcode exited {rc}", file=sys.stderr)
        return rc
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
