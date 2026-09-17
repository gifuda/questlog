"""QuestLog 租户适配器测试（pytest；亦可用 `python3 test_questlog_adapter.py` 裸跑）。

覆盖：投递消息校验矩阵（合法/版本/键形/租户/边界值）、路径段消毒、
research 桩模式产出。TaskSpec 构造依赖平台契约，仅在采集机侧由 executor
补测（施工验收项）。
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from questlog import (  # noqa: E402
    DeliveryError,
    clamp_timeout,
    sanitize_segment,
    validate_delivery,
)
import questlog_research  # noqa: E402


def valid_delivery() -> dict:
    return {
        "version": 1,
        "task_key": "a" * 64,
        "tenant_id": "questlog",
        "todo_id": "td-1",
        "package_id": "pkg-1",
        "package_hash": "b" * 64,
        "objective": "研究目标与验收标准。",
        "repo_root": "/home/manager/work/proj",
        "base_sha": "c" * 40,
        "timeout_seconds": 900,
    }


def test_valid_passes():
    assert validate_delivery(valid_delivery()) is not None


def test_rejects_bad_version():
    m = valid_delivery(); m["version"] = 2
    assert raises(m)


def test_rejects_bad_task_key():
    m = valid_delivery(); m["task_key"] = "XYZ"
    assert raises(m)
    m = valid_delivery(); m["task_key"] = "a" * 63
    assert raises(m)


def test_tenant_id_is_informational():
    # 投递消息的 tenant_id 是经理机项目名——任意安全字符串（平台租户恒为
    # questlog，由 TaskSpec 构造决定），不参与校验。
    m = valid_delivery(); m["tenant_id"] = "任意项目名"
    assert validate_delivery(m) is not None


def test_rejects_bad_sha():
    m = valid_delivery(); m["base_sha"] = "zz"
    assert raises(m)
    m = valid_delivery(); m["package_hash"] = ""
    assert raises(m)


def test_rejects_empty_objective():
    m = valid_delivery(); m["objective"] = "   "
    assert raises(m)


def test_rejects_timeout_out_of_bounds():
    m = valid_delivery(); m["timeout_seconds"] = 10
    assert raises(m)
    m = valid_delivery(); m["timeout_seconds"] = 9999
    assert raises(m)


def test_sanitize_segment():
    assert sanitize_segment("/home/manager/x y") == "_home_manager_x_y"
    assert sanitize_segment("a" * 200).startswith("aaaa")
    assert len(sanitize_segment("a" * 200)) <= 80


def test_clamp_timeout():
    assert clamp_timeout(10) == 60
    assert clamp_timeout(99999) == 900
    assert clamp_timeout(300) == 300
    assert clamp_timeout("x") == 60


def test_research_stub_produces_report(tmp_path):
    import os

    os.environ["QUESTLOG_RESEARCH_STUB"] = "1"
    delivery = valid_delivery()
    d = Path(tmp_path or ".");
    delivery_file = d / "delivery.json"
    delivery_file.write_text(json.dumps(delivery), encoding="utf-8")
    report_dir = d / "reports"
    rc = questlog_research.main([
        str(delivery_file), str(d / "repo"), str(report_dir),
    ])
    assert rc == 0, "stub mode must succeed without zcode"
    report = (report_dir / delivery["task_key"] / "report.md").read_text(encoding="utf-8")
    assert "questlog-todo-report" in report
    assert delivery["task_key"] in report
    assert "NEEDS_REVIEW" in report


def raises(msg: dict) -> bool:
    try:
        validate_delivery(msg)
    except DeliveryError:
        return True
    return False


if __name__ == "__main__":
    import tempfile

    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                with tempfile.TemporaryDirectory() as tmp:
                    fn(Path(tmp)) if fn.__code__.co_argcount else fn()
                print(f"PASS {name}")
            except AssertionError as e:
                failures += 1
                print(f"FAIL {name}: {e}")
    sys.exit(1 if failures else 0)
