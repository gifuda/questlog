#!/usr/bin/env python3
"""questlog-reanchor.py — S4 m5p 侧权威副本重锚（幂等）

把从 M4 拷来的权威副本中"权威机路径"改写为本机（m5p/power）路径：
  1. ~/.todo-board.json            projects[].path 前缀互换（arb_board.board_path
                                   保留 M4 路径——该字段语义=GUI 所在机的本地 gros 导出）
  2. ~/questlog-runtime/runner-config.json
                                   projects 键前缀互换；todo_schedule 三绝对路径
                                   （node_bin/producer_cli/verify_cli）改写为本机路径；
                                   global_enabled 强制 false（默认；--keep-switches 保留）
  3. ~/questlog-runtime/packages.json
                                   project_path 在本机不存在的 sealed 包 → status=cancelled
                                   （status 不参与 seal 哈希，task-package.ts canonicalize
                                   显式排除——改 status 不破 seal；重封走对话侧正常流程）

每次写前备份 *.pre-reanchor（已存在则跳过备份）。重复运行安全（已互换的前缀不再匹配）。
用法：python3 questlog-reanchor.py [--from /Users/abo] [--to /Users/power] [--keep-switches]
"""

import json
import os
import shutil
import sys

HOME = os.path.expanduser("~")
REGISTRY = os.path.join(HOME, ".todo-board.json")
RUNTIME = os.environ.get("QUESTLOG_RUNTIME", os.path.join(HOME, "questlog-runtime"))
RUNNER_CONFIG = os.path.join(RUNTIME, "runner-config.json")
PACKAGES = os.path.join(RUNTIME, "packages.json")

NODE_BIN = "/opt/homebrew/bin/node"
REPO = None  # filled in main from argv


def backup(path):
    if not os.path.exists(path):
        return
    bak = path + ".pre-reanchor"
    if not os.path.exists(bak):
        shutil.copy2(path, bak)


def swap_prefix(path_str, frm, to):
    if path_str == frm:
        return to, True
    if path_str.startswith(frm + "/"):
        return to + path_str[len(frm):], True
    return path_str, False


def reanchor_registry(frm, to):
    if not os.path.exists(REGISTRY):
        return ["registry missing (skip)"]
    report = []
    with open(REGISTRY) as f:
        reg = json.load(f)
    changed = 0
    for p in reg.get("projects", []):
        new, hit = swap_prefix(p.get("path", ""), frm, to)
        if hit:
            p["path"] = new
            changed += 1
            report.append(f"project {p.get('name')}: {new}")
    if changed:
        backup(REGISTRY)
        with open(REGISTRY, "w") as f:
            json.dump(reg, f, ensure_ascii=False, indent=2)
        report.append(f"registry: {changed} paths re-anchored")
    else:
        report.append("registry: nothing to re-anchor (idempotent skip)")
    return report


def reanchor_runner_config(frm, to, keep_switches):
    if not os.path.exists(RUNNER_CONFIG):
        return ["runner-config missing (skip)"]
    report = []
    with open(RUNNER_CONFIG) as f:
        cfg = json.load(f)
    changed = 0
    projects = cfg.get("projects")
    if isinstance(projects, dict):
        new_map = {}
        for k, v in projects.items():
            nk, hit = swap_prefix(k, frm, to)
            new_map[nk] = v
            if hit:
                changed += 1
        cfg["projects"] = new_map
    ts = cfg.get("todo_schedule")
    if isinstance(ts, dict):
        ts["node_bin"] = NODE_BIN
        for field in ("producer_cli", "verify_cli"):
            if ts.get(field):
                new, _hit = swap_prefix(ts[field], frm, to)
                ts[field] = new
        changed += 1
        report.append(f"todo_schedule: node_bin={NODE_BIN}")
    if not keep_switches and cfg.get("global_enabled") is not False:
        cfg["global_enabled"] = False
        report.append("global_enabled -> False (pre-deploy safety; "
                      "re-enable via API/开关 when the cutover is accepted)")
        changed += 1
    if changed:
        backup(RUNNER_CONFIG)
        with open(RUNNER_CONFIG, "w") as f:
            json.dump(cfg, f, ensure_ascii=False, indent=2)
    report.append(f"runner-config: {changed} field(s) changed")
    return report


def cancel_stale_sealed():
    if not os.path.exists(PACKAGES):
        return ["packages.json missing (skip)"]
    report = []
    with open(PACKAGES) as f:
        data = json.load(f)
    pkgs = data.get("packages", [])
    cancelled = 0
    for p in pkgs:
        if p.get("status") != "sealed":
            continue
        pp = p.get("project_path", "")
        if pp and not os.path.isdir(pp):
            # status is excluded from the seal hash — safe to flip.
            p["status"] = "cancelled"
            cancelled += 1
            report.append(f"cancel stale sealed: {p.get('package_id')} ({pp})")
    if cancelled:
        backup(PACKAGES)
        with open(PACKAGES, "w") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
    report.append(f"packages: {cancelled} stale sealed cancelled (status-only, "
                  "seal hash untouched; re-seal via conversation if still wanted)")
    return report


def main():
    global HOME
    frm, to = "/Users/abo", "/Users/power"
    keep_switches = "--keep-switches" in sys.argv
    args = sys.argv[1:]
    if "--from" in args:
        frm = args[args.index("--from") + 1]
    if "--to" in args:
        to = args[args.index("--to") + 1]

    print(f"re-anchor {frm} -> {to} (home={HOME})")
    for line in reanchor_registry(frm, to):
        print(" ", line)
    for line in reanchor_runner_config(frm, to, keep_switches):
        print(" ", line)
    for line in cancel_stale_sealed():
        print(" ", line)
    print("done")


if __name__ == "__main__":
    main()
