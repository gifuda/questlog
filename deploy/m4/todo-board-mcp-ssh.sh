#!/bin/sh
# todo-board MCP client wrapper (M4 remote mode).
# Routes the MCP server to the m5p authority over ssh stdio and forwards the
# spawn cwd via MCP_CLIENT_CWD so the server can map M4 project paths onto
# its own home prefix (see mcp-server/src/store.ts mapClientPath).
# $PWD expands locally (spawn cwd = session project dir); the single quotes
# keep it one word through the remote shell. Paths on this fleet contain no
# single quotes — if one ever appears this wrapper must be reworked.
exec /usr/bin/ssh m5p "MCP_CLIENT_CWD='$PWD' /opt/homebrew/bin/node '/Users/power/编程架构包/桌面清单贴/mcp-server/dist/index.js'"
