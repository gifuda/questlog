#!/usr/bin/env python3
"""mcp-jsonrpc-smoke.py — S4 切流验收/切流后板写驱动（MCP over ssh stdio）

通过 `ssh m5p node …/mcp-server/dist/index.js` 直连 m5p 权威 MCP server，
按 MCP stdio 协议（换行分隔 JSON-RPC）执行一次工具调用并打印结果。

用途：
  1. 切流验收：list_epics / push_todo 读写往返（写入落 m5p 权威副本）
  2. 切流后本会话的 todo-board MCP 已是旧 M4 stdio 进程（禁再用）——
     对权威板的少量登记动作用本驱动经 m5p MCP 完成（锁协议在 m5p 本机原样生效）

用法：
  mcp-jsonrpc-smoke.py list
  mcp-jsonrpc-smoke.py call <tool> '<json-args>'
示例：
  mcp-jsonrpc-smoke.py call list_epics '{"project_dir": "/Users/power/编程架构包/桌面清单贴"}'
"""

import json
import subprocess
import sys
import threading

REMOTE_CMD = [
    "ssh", "-o", "BatchMode=yes", "m5p",
    "/opt/homebrew/bin/node",
    "/Users/power/编程架构包/桌面清单贴/mcp-server/dist/index.js",
]

_next_id = [0]


def msg(method, params):
    _next_id[0] += 1
    return {"jsonrpc": "2.0", "id": _next_id[0], "method": method, "params": params}


def notify(method, params):
    return {"jsonrpc": "2.0", "method": method, "params": params}


def read_until_id(proc, want_id, timeout=30):
    result = {}

    def pump():
        for raw in proc.stdout:
            line = raw.decode("utf-8", "replace").strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            if obj.get("id") == want_id:
                result.update(obj)
                return

    t = threading.Thread(target=pump, daemon=True)
    t.start()
    t.join(timeout)
    return result or None


def send(proc, obj):
    proc.stdin.write((json.dumps(obj) + "\n").encode())
    proc.stdin.flush()


def run(method_name, tool_args=None):
    proc = subprocess.Popen(
        REMOTE_CMD, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
    )
    try:
        init = msg("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "questlog-s4-smoke", "version": "1.0"},
        })
        send(proc, init)
        if not read_until_id(proc, init["id"]):
            print("FAIL: no initialize response", file=sys.stderr)
            sys.exit(1)
        send(proc, notify("notifications/initialized", {}))
        if method_name == "list":
            req = msg("tools/list", {})
            send(proc, req)
            resp = read_until_id(proc, req["id"]) or {}
            tools = (resp.get("result") or {}).get("tools", [])
            for t in tools:
                print(t.get("name"))
        else:
            req = msg("tools/call", {"name": method_name, "arguments": tool_args or {}})
            send(proc, req)
            resp = read_until_id(proc, req["id"], timeout=60) or {}
            if resp.get("error"):
                print("ERROR:", json.dumps(resp["error"], ensure_ascii=False))
                sys.exit(1)
            content = (resp.get("result") or {}).get("content", [])
            for c in content:
                print(c.get("text", ""))
    finally:
        proc.terminate()


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in ("list", "call"):
        print(__doc__)
        sys.exit(2)
    if sys.argv[1] == "list":
        run("list")
    else:
        if len(sys.argv) < 3:
            print("call needs <tool> '<json>'", file=sys.stderr)
            sys.exit(2)
        args = json.loads(sys.argv[3]) if len(sys.argv) > 3 else {}
        run(sys.argv[2], args)


if __name__ == "__main__":
    main()
