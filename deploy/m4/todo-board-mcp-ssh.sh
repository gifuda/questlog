#!/bin/sh
# todo-board MCP client wrapper (M4 remote mode).
# Routes the MCP server to the m5p authority over ssh stdio and forwards the
# spawn cwd via MCP_CLIENT_CWD so the server can map M4 project paths onto
# its own home prefix (see mcp-server/src/store.ts mapClientPath).
# pwd -P resolves the REAL process cwd (the inherited $PWD env can be stale
# when spawned via Command::cwd). A cwd containing a single quote would break
# out of the remote quoting — refuse it and start without mapping (auto-detect
# degrades to the old fail-visible behavior) rather than risk injection.
cwd=$(pwd -P)
case "$cwd" in
  *\'*) unset cwd ;;
esac
if [ -n "${cwd:-}" ]; then
  exec /usr/bin/ssh m5p "MCP_CLIENT_CWD='$cwd' /opt/homebrew/bin/node '/Users/power/编程架构包/桌面清单贴/mcp-server/dist/index.js'"
else
  exec /usr/bin/ssh m5p "/opt/homebrew/bin/node '/Users/power/编程架构包/桌面清单贴/mcp-server/dist/index.js'"
fi
