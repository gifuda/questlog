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


OFF_LEVELS = ("off", "nothink", "disabled", "none")


def _budget_tokens(level: str) -> int:
    return {"max": 32000, "high": 16000, "low": 8000}.get(level, 16000)


def _native_thinking_options(level: str) -> dict:
    if level in OFF_LEVELS:
        return {"anthropic": {"thinking": {"type": "disabled"}}}
    return {
        "anthropic": {
            "effort": level,
            "thinking": {"budgetTokens": _budget_tokens(level), "type": "enabled"},
        }
    }


def _thinking_levels(model_name: str, model_def: dict) -> list[str]:
    import copy

    r = (model_def or {}).get("reasoning") or {}
    for key in ("levels", "variants"):
        lv = r.get(key)
        if isinstance(lv, list) and lv:
            return [str(x) for x in lv]
    lower = model_name.lower()
    if "deepseek-v4" in lower:
        return ["high", "max"]
    if lower.startswith("glm-5.2"):
        return ["max", "high", "nothink"]
    return ["high", "max"]


def _resolve_provider_def(provider_name: str) -> dict | None:
    """v2 桌面配置优先，cli 兜底（与 manager runner_git 同序）。"""
    import json as _json

    home = Path.home()
    for rel in (".zcode/v2/config.json", ".zcode/cli/config.json"):
        try:
            cfg = _json.loads((home / rel).read_text(encoding="utf-8"))
            def_ = (cfg.get("provider") or {}).get(provider_name)
            if def_:
                return def_
        except (OSError, ValueError):
            continue
    return None


def provision_model_config(repo_dir: str, model: str, thinking: str | None) -> None:
    """项目级 .zcode/config.json 注入（manager Runner 已验证手法的 Python 复刻）：
    - provider def 从 ~/.zcode/{v2,cli}/config.json 复制（项目配置不合并全局
      provider 表，必须整份携带）
    - models 为旧 list 形态时升级为 dict（模型名→{}），使思考档可注入
    - thinking 归一化：模型 reasoning 的全部档位都改写为所选档参数（对齐
      manager runner_git.rs 语义：effort + budgetTokens native 表，off 档
      disabled；用户声明的 providerOptionsByLevel[level] 优先逐字复制）
    - .git/info/exclude 隐身，不污染快照 git status
    失败不致命：打日志回退 CLI 默认。"""
    import copy
    import json as _json

    provider_name, _, model_name = model.partition("/")
    if not provider_name or not model_name:
        print(f"[questlog-research] model {model!r} not provider/model form; skip", file=sys.stderr)
        return
    try:
        prov_def = _resolve_provider_def(provider_name)
        if prov_def is None:
            print(f"[questlog-research] provider {provider_name!r} not in zcode configs; default model", file=sys.stderr)
            return
        prov_def = copy.deepcopy(prov_def)
        models = prov_def.get("models")
        if isinstance(models, list):
            # 旧形态升级：list → dict（保留全部模型名）
            prov_def["models"] = {m if isinstance(m, str) else str(m): {} for m in models}
        entry = prov_def["models"].setdefault(model_name, {})
        if not isinstance(entry, dict):
            prov_def["models"][model_name] = entry = {}
        if thinking:
            levels = _thinking_levels(model_name, entry)
            user_povl = ((entry.get("reasoning") or {}).get("providerOptionsByLevel") or {})
            chosen = (
                user_povl.get(thinking)
                if isinstance(user_povl, dict) and isinstance(user_povl.get(thinking), dict)
                else _native_thinking_options(thinking)
            )
            povl = {lv: chosen for lv in levels}
            entry["reasoning"] = {
                "enabled": True,
                "levels": levels,
                "defaultLevel": thinking,
                "providerOptionsByLevel": povl,
            }
        proj = {"provider": {provider_name: prov_def}, "model": {"main": model}}
        zdir = Path(repo_dir) / ".zcode"
        zdir.mkdir(parents=True, exist_ok=True)
        (zdir / "config.json").write_text(_json.dumps(proj, ensure_ascii=False, indent=2), encoding="utf-8")
        git_info = Path(repo_dir) / ".git" / "info"
        git_info.mkdir(parents=True, exist_ok=True)
        excl = git_info / "exclude"
        existing = excl.read_text() if excl.exists() else ""
        if ".zcode/" not in existing:
            excl.write_text(existing + "\n.zcode/\n")
    except Exception as e:  # noqa: BLE001 — 注入失败回退默认，不挡任务
        print(f"[questlog-research] model config injection skipped: {e}", file=sys.stderr)


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

    model_pref = str(msg.get("model") or "")
    thinking_pref = str(msg.get("thinking") or "") or None
    if model_pref:
        provision_model_config(repo_dir, model_pref, thinking_pref)
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
