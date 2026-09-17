"""QuestLog 研究执行器（待办租户 GX）——平台白名单脚本候选（allowlist 名 questlog-research）。

只读研究任务：读投递消息 → 构造只读研究信封 → 调 zcode headless（或 shadow
桩）→ 报告 stdout 落 report_dir/<task_key>/report.md。

调用形态：TaskSpec parameters.args = [delivery_path, repo_dir, report_dir]
（逐字 execve，无 shell）。zcode 二进制路径取环境变量 QUESTLOG_ZCODE_BIN；
缺失时若 QUESTLOG_RESEARCH_STUB=1 走桩模式（产出固定 NEEDS_REVIEW 报告，
用于 shadow 全链联调），否则干净失败（exit 5）。

红线：不写 repo_dir 内任何文件；报告只落 report_dir。
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
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


def run_stub(task_key: str, report_file: Path) -> None:
    report_file.parent.mkdir(parents=True, exist_ok=True)
    report_file.write_text(STUB_REPORT.format(task_key=task_key), encoding="utf-8")


def run_zcode(zcode_bin: str, prompt: str, repo_dir: str, task_key: str, report_file: Path) -> int:
    # 探针：CLI 必须可用（只读 --version）
    probe = subprocess.run([zcode_bin, "--version"], capture_output=True, text=True, check=False)
    if probe.returncode != 0:
        print(f"zcode probe failed: {probe.stderr.strip()}", file=sys.stderr)
        return 5
    with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False) as tmp:
        tmp.write(prompt)
        prompt_file = tmp.name
    try:
        proc = subprocess.run(
            [zcode_bin, "--prompt", prompt_file, "--cwd", repo_dir, "--mode", "yolo"],
            capture_output=True,
            text=True,
            check=False,
            timeout=840,
        )
    except subprocess.TimeoutExpired:
        print("zcode timed out", file=sys.stderr)
        return 124
    finally:
        os.unlink(prompt_file)
    report_file.parent.mkdir(parents=True, exist_ok=True)
    report_file.write_text(proc.stdout or "", encoding="utf-8")
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

    zcode_bin = os.environ.get("QUESTLOG_ZCODE_BIN", "")
    if zcode_bin and shutil.which(zcode_bin) is None and not Path(zcode_bin).exists():
        zcode_bin = ""
    if not zcode_bin:
        if os.environ.get("QUESTLOG_RESEARCH_STUB") == "1":
            run_stub(task_key, report_file)
            return 0
        print("zcode unavailable and stub mode off (QUESTLOG_ZCODE_BIN / QUESTLOG_RESEARCH_STUB)", file=sys.stderr)
        return 5

    prompt = ENVELOPE.format(objective=str(msg.get("objective", "")), task_key=task_key)
    rc = run_zcode(zcode_bin, prompt, repo_dir, task_key, report_file)
    if rc != 0:
        print(f"zcode exited {rc}", file=sys.stderr)
        return rc
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
